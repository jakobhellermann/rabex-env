//! Re-encoding objects from one type tree to another directly on their bytes.
//!
//! What a class's re-encode does depends only on the two trees, so it is worked out once
//! per pair: a field the target names carries over from the source field of that name —
//! as it is, at another width, or rebuilt where its parts changed — a source field the
//! target does not name is dropped, and a field a rule in [`crate::fields`] decides gets
//! the rule's value, the one part that becomes a value. Each object then runs through
//! that plan.
//!
//! A pair the trees and the rules do not say how to map fails when the plan is made: a
//! field the target wants that nothing provides, a field whose kind changes.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use byteorder::LittleEndian;
use rabex::typetree::{TypeTreeNode, TypetreeNodeKind as Kind};
use rabex::{UnityVersion, serde_typetree};

use crate::fields::{Rule, Source};
use crate::layout::{self, Layout, Reader, aligns_after};
use crate::value::{Value, layout_equal};

/// The plan for one pair of trees, borrowing them.
pub struct Transcoder<'t> {
    source: &'t TypeTreeNode,
    root: Node<'t>,
    dropped: Vec<String>,
}

enum Node<'t> {
    /// The same layout on both sides.
    Same(Layout),
    /// A number of fixed width whose width, signedness or padding changes.
    Scalar {
        from: Scalar,
        from_aligns: bool,
        to: Scalar,
        to_aligns: bool,
    },
    /// A length and that many bytes (a string, raw data) whose padding changes.
    Blob { from_aligns: bool, to_aligns: bool },
    /// A struct whose target fields come from the source's in their order, with no
    /// rule reading them: each source field in turn, kept or dropped.
    Streamed {
        fields: Vec<Stream<'t>>,
        from_aligns: bool,
        to_aligns: bool,
    },
    /// Any other struct: where each source field starts is noted first, then the
    /// target's fields are written in its order.
    Struct {
        /// The source's fields, as read past, and their trees, which rules read them by.
        source: Vec<Layout>,
        trees: &'t [TypeTreeNode],
        /// What becomes of each source field.
        slots: Vec<Slot>,
        /// Each target field, in the target's order.
        order: Vec<Target<'t>>,
        from_aligns: bool,
        to_aligns: bool,
    },
    Array {
        element: Box<Node<'t>>,
        from_aligns: bool,
        to_aligns: bool,
    },
    Map {
        key: Box<Node<'t>>,
        value: Box<Node<'t>>,
        from_aligns: bool,
        to_aligns: bool,
    },
}

/// A source field of a streamed struct.
enum Stream<'t> {
    Kept(Node<'t>),
    /// Read past, counted as the plan's dropped path at this index.
    Dropped(Layout, usize),
}

/// What becomes of a source field of a struct that is not streamed.
#[derive(Clone, Copy)]
enum Slot {
    Kept,
    /// Dropped, counted as the plan's dropped path at this index.
    Dropped(usize),
    /// Read by a rule only.
    Read,
}

/// Where a target field comes from.
enum Target<'t> {
    /// The source field at this index, through the node.
    Field(usize, Node<'t>),
    /// A rule's value, encoded against the target's field.
    Rule(Rule, &'t TypeTreeNode),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scalar {
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}

impl Scalar {
    fn of(node: &TypeTreeNode) -> Option<Scalar> {
        Some(match node.classify() {
            Kind::Bool => Scalar::Bool,
            Kind::Char => Scalar::Char,
            Kind::I8 => Scalar::I8,
            Kind::I16 => Scalar::I16,
            Kind::I32 => Scalar::I32,
            Kind::I64 => Scalar::I64,
            Kind::U8 => Scalar::U8,
            Kind::U16 => Scalar::U16,
            Kind::U32 => Scalar::U32,
            Kind::U64 => Scalar::U64,
            Kind::Float => Scalar::F32,
            Kind::Double => Scalar::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::Bool | Scalar::Char | Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::I64 | Scalar::U64 | Scalar::F64 => 8,
        }
    }

    /// The range of an integer, `None` for the others.
    fn range(self) -> Option<(i128, i128)> {
        Some(match self {
            Scalar::I8 => (i8::MIN.into(), i8::MAX.into()),
            Scalar::I16 => (i16::MIN.into(), i16::MAX.into()),
            Scalar::I32 => (i32::MIN.into(), i32::MAX.into()),
            Scalar::I64 => (i64::MIN.into(), i64::MAX.into()),
            Scalar::U8 => (0, u8::MAX.into()),
            Scalar::U16 => (0, u16::MAX.into()),
            Scalar::U32 => (0, u32::MAX.into()),
            Scalar::U64 => (0, u64::MAX.into()),
            _ => return None,
        })
    }

    fn signed(self) -> bool {
        matches!(self, Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64)
    }

    /// Whether a value of `self` turns into one of `to`: the same kind, or integers.
    fn converts_to(self, to: Scalar) -> bool {
        self == to || self.range().is_some() && to.range().is_some()
    }

    /// Write `bytes`, a value of `self`, as `to`.
    fn convert(self, bytes: &[u8], to: Scalar, out: &mut Vec<u8>) -> Result<()> {
        if self == to {
            out.extend_from_slice(bytes);
            return Ok(());
        }
        let mut wide = [0; 8];
        wide[..bytes.len()].copy_from_slice(bytes);
        let raw = u64::from_le_bytes(wide);
        let bits = 8 * bytes.len() as u32;
        let number = match self.signed() {
            // sign-extend from the source's width
            true => ((raw << (64 - bits)) as i64 >> (64 - bits)) as i128,
            false => raw as i128,
        };
        let (min, max) = to.range().expect("integers only");
        ensure!(
            (min..=max).contains(&number),
            "{number} does not fit the target's {to:?}"
        );
        out.extend_from_slice(&(number as u64).to_le_bytes()[..to.size()]);
        Ok(())
    }
}

impl<'t> Transcoder<'t> {
    /// The plan from `source` to `target`. `class` names the class in the dropped fields'
    /// paths and the errors.
    pub fn new(source: &'t TypeTreeNode, target: &'t TypeTreeNode, class: &str) -> Result<Self> {
        let mut dropped = Vec::new();
        let root = plan(source, target, class, &mut dropped)?;
        Ok(Self {
            source,
            root,
            dropped,
        })
    }

    /// The paths of the fields the plan drops; [`Self::run`] counts by their index.
    pub fn dropped(&self) -> &[String] {
        &self.dropped
    }

    /// Re-encode one object for `target`, counting its dropped fields into `counts`,
    /// one per [`Self::dropped`]. The source tree has to account for every byte.
    pub fn run(
        &self,
        object: &[u8],
        target: &UnityVersion,
        counts: &mut [usize],
        warnings: &mut BTreeMap<String, usize>,
    ) -> Result<Vec<u8>> {
        let mut run = Run {
            object,
            root: self.source,
            target,
            from: Reader::new(object),
            out: Vec::with_capacity(object.len()),
            counts,
            starts: Vec::new(),
            indices: Vec::new(),
            whole: OnceCell::new(),
            warnings: RefCell::new(BTreeMap::new()),
        };
        run.node(&self.root)?;
        for (warning, n) in run.warnings.take() {
            *warnings.entry(warning).or_default() += n;
        }
        ensure!(
            run.from.is_at_end(),
            "the tree accounts for {} of the object's {} bytes",
            run.from.pos(),
            object.len()
        );
        Ok(run.out)
    }
}

fn plan<'t>(
    source: &'t TypeTreeNode,
    target: &'t TypeTreeNode,
    path: &str,
    dropped: &mut Vec<String>,
) -> Result<Node<'t>> {
    if layout_equal(source, target) {
        return Ok(Node::Same(Layout::of(target)));
    }
    let (from_aligns, to_aligns) = (aligns_after(source), aligns_after(target));
    Ok(match (source.classify(), target.classify()) {
        (Kind::Struct, Kind::Struct) => plan_struct(source, target, path, dropped)?,
        (Kind::ArrayWrapper, Kind::ArrayWrapper) => Node::Array {
            element: Box::new(plan(
                layout::element(source).context("a vector without element")?,
                layout::element(target).context("a vector without element")?,
                &format!("{path}[]"),
                dropped,
            )?),
            from_aligns,
            to_aligns,
        },
        (Kind::Map, Kind::Map) => {
            let (from_key, from_value) = layout::pair(source).context("a map without pair")?;
            let (to_key, to_value) = layout::pair(target).context("a map without pair")?;
            Node::Map {
                key: Box::new(plan(from_key, to_key, &format!("{path}[].first"), dropped)?),
                value: Box::new(plan(
                    from_value,
                    to_value,
                    &format!("{path}[].second"),
                    dropped,
                )?),
                from_aligns,
                to_aligns,
            }
        }
        (Kind::String, Kind::String) | (Kind::Untyped, Kind::Untyped) => Node::Blob {
            from_aligns,
            to_aligns,
        },
        (from, to) => match (Scalar::of(source), Scalar::of(target)) {
            (Some(from), Some(to)) if from.converts_to(to) => Node::Scalar {
                from,
                from_aligns,
                to,
                to_aligns,
            },
            _ => bail!(
                "{path}: a {} ({from:?}) in the source, a {} ({to:?}) in the target, and no \
                 rule converting between them",
                source.m_Type,
                target.m_Type
            ),
        },
    })
}

fn plan_struct<'t>(
    source: &'t TypeTreeNode,
    target: &'t TypeTreeNode,
    path: &str,
    dropped: &mut Vec<String>,
) -> Result<Node<'t>> {
    // where each target field comes from: a rule, or the source field of its name
    let mut slots: Vec<Option<Slot>> = vec![None; source.children.len()];
    let mut origins = Vec::with_capacity(target.children.len());
    for to in &target.children {
        let name = to.m_Name.as_str();
        if let Some(rule) = crate::fields::rule(source, target, name) {
            // the source field the rule takes the place of, under the target's name or
            // the one it was renamed from, is not dropped
            for replaced in [name].into_iter().chain(rule.replaces().iter().copied()) {
                if let Some(at) = source.children.iter().position(|c| c.m_Name == replaced) {
                    slots[at] = Some(Slot::Read);
                }
            }
            origins.push(Err(rule));
            continue;
        }
        let Some(at) = source.children.iter().position(|c| c.m_Name == name) else {
            bail!(
                "{path}.{name}: the target wants it, the source lacks it, and no rule provides it"
            );
        };
        slots[at] = Some(Slot::Kept);
        origins.push(Ok(at));
    }
    let slots: Vec<Slot> = slots
        .into_iter()
        .zip(&source.children)
        .map(|(slot, field)| {
            slot.unwrap_or_else(|| {
                dropped.push(format!("{path}.{}", field.m_Name));
                Slot::Dropped(dropped.len() - 1)
            })
        })
        .collect();
    let mut plan_field = |at: usize, to: &'t TypeTreeNode| {
        plan(
            &source.children[at],
            to,
            &format!("{path}.{}", to.m_Name),
            dropped,
        )
    };
    let (from_aligns, to_aligns) = (aligns_after(source), aligns_after(target));

    // the fields stream through when the target takes the source's fields in their
    // order and no rule reads them, which would need where they start
    let fields_only: Option<Vec<usize>> = origins
        .iter()
        .map(|origin| origin.as_ref().ok().copied())
        .collect();
    if let Some(ats) = fields_only.filter(|ats| ats.is_sorted()) {
        let mut kept = ats.into_iter().zip(&target.children);
        let mut fields = Vec::with_capacity(source.children.len());
        for (field, slot) in source.children.iter().zip(&slots) {
            fields.push(match *slot {
                Slot::Dropped(n) => Stream::Dropped(Layout::of(field), n),
                Slot::Kept | Slot::Read => {
                    let (at, to) = kept.next().expect("one target field per kept source field");
                    Stream::Kept(plan_field(at, to)?)
                }
            });
        }
        return Ok(Node::Streamed {
            fields,
            from_aligns,
            to_aligns,
        });
    }
    let order = origins
        .into_iter()
        .zip(&target.children)
        .map(|(origin, to)| {
            Ok(match origin {
                Ok(at) => Target::Field(at, plan_field(at, to)?),
                Err(rule) => Target::Rule(rule, to),
            })
        })
        .collect::<Result<_>>()?;
    Ok(Node::Struct {
        source: source.children.iter().map(Layout::of).collect(),
        trees: &source.children,
        slots,
        order,
        from_aligns,
        to_aligns,
    })
}

struct Run<'a, 'c> {
    object: &'a [u8],
    root: &'a TypeTreeNode,
    target: &'a UnityVersion,
    from: Reader<'a>,
    out: Vec<u8>,
    counts: &'c mut [usize],
    /// Where the source's fields start, for structs that are not streamed.
    starts: Vec<usize>,
    /// The element indices of the arrays around the current position.
    indices: Vec<usize>,
    /// The whole source object, for rules that read it.
    whole: OnceCell<Value>,
    /// What rules wrote that the target cannot say exactly, by message.
    warnings: RefCell<BTreeMap<String, usize>>,
}

impl Run<'_, '_> {
    fn node(&mut self, node: &Node) -> Result<()> {
        match node {
            Node::Same(layout) => layout::copy(layout, &mut self.from, &mut self.out),
            Node::Scalar {
                from,
                from_aligns,
                to,
                to_aligns,
            } => {
                let bytes = self.from.take(from.size())?;
                from.convert(bytes, *to, &mut self.out)?;
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
            Node::Blob {
                from_aligns,
                to_aligns,
            } => {
                let count = self.from.count()?;
                self.out
                    .extend_from_slice(&i32::try_from(count)?.to_le_bytes());
                self.out.extend_from_slice(self.from.take(count)?);
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
            Node::Array {
                element,
                from_aligns,
                to_aligns,
            } => {
                let count = self.from.count()?;
                self.out
                    .extend_from_slice(&i32::try_from(count)?.to_le_bytes());
                for index in 0..count {
                    self.indices.push(index);
                    self.node(element)?;
                    self.indices.pop();
                }
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
            Node::Map {
                key,
                value,
                from_aligns,
                to_aligns,
            } => {
                let count = self.from.count()?;
                self.out
                    .extend_from_slice(&i32::try_from(count)?.to_le_bytes());
                for index in 0..count {
                    self.indices.push(index);
                    self.node(key)?;
                    self.node(value)?;
                    self.indices.pop();
                }
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
            Node::Streamed {
                fields,
                from_aligns,
                to_aligns,
            } => {
                for field in fields {
                    match field {
                        Stream::Kept(node) => self.node(node)?,
                        Stream::Dropped(layout, n) => {
                            self.from.skip(layout)?;
                            self.counts[*n] += 1;
                        }
                    }
                }
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
            Node::Struct {
                source,
                trees,
                slots,
                order,
                from_aligns,
                to_aligns,
            } => {
                self.noted(source, trees, slots, order)?;
                self.end(*from_aligns, *to_aligns);
                Ok(())
            }
        }
    }

    /// A struct whose fields change order or which rules read: where each source field
    /// starts is noted first.
    fn noted(
        &mut self,
        source: &[Layout],
        trees: &[TypeTreeNode],
        slots: &[Slot],
        order: &[Target],
    ) -> Result<()> {
        let base = self.starts.len();
        for (field, slot) in source.iter().zip(slots) {
            self.starts.push(self.from.pos());
            self.from.skip(field)?;
            if let Slot::Dropped(n) = *slot {
                self.counts[n] += 1;
            }
        }
        let end = self.from.pos();
        for target in order {
            match target {
                Target::Field(at, node) => {
                    self.from.seek(self.starts[base + at]);
                    self.node(node)?;
                }
                Target::Rule(rule, to) => {
                    let value = rule
                        .value(&Fields {
                            run: self,
                            source: trees,
                            starts: &self.starts[base..],
                        })
                        .with_context(|| format!("the rule for {}", to.m_Name))?;
                    let mut out = Cursor::new(&mut self.out);
                    out.set_position(out.get_ref().len() as u64);
                    serde_typetree::to_writer::<_, _, LittleEndian>(&mut out, &value, to)
                        .with_context(|| format!("{} as the rule made it", to.m_Name))?;
                }
            }
        }
        self.from.seek(end);
        self.starts.truncate(base);
        Ok(())
    }

    fn end(&mut self, from_aligns: bool, to_aligns: bool) {
        self.from.align(from_aligns);
        if to_aligns {
            layout::align(&mut self.out);
        }
    }

    /// The source object as a value, read once.
    fn whole(&self) -> Result<&Value> {
        if let Some(value) = self.whole.get() {
            return Ok(value);
        }
        let value: Value = serde_typetree::from_reader::<_, LittleEndian>(
            &mut Cursor::new(self.object),
            self.root,
        )?;
        Ok(self.whole.get_or_init(|| value))
    }
}

/// What a rule reads: the fields of the source struct, noted where they start.
struct Fields<'r, 'a, 'c> {
    run: &'r Run<'a, 'c>,
    source: &'r [TypeTreeNode],
    starts: &'r [usize],
}

impl Source for Fields<'_, '_, '_> {
    fn field(&self, name: &str) -> Result<Option<Value>> {
        let Some(at) = self.source.iter().position(|c| c.m_Name == name) else {
            return Ok(None);
        };
        // read in place: padding inside the field counts from the object's start
        let mut cursor = Cursor::new(self.run.object);
        cursor.set_position(self.starts[at] as u64);
        let value = serde_typetree::from_reader::<_, LittleEndian>(&mut cursor, &self.source[at])
            .with_context(|| format!("reading {name}"))?;
        Ok(Some(value))
    }

    fn object(&self) -> Result<&Value> {
        self.run.whole()
    }

    fn index(&self) -> Result<usize> {
        self.run.indices.last().copied().context("not in an array")
    }

    fn target(&self) -> &UnityVersion {
        self.run.target
    }

    fn warn(&self, message: String) {
        *self.run.warnings.borrow_mut().entry(message).or_default() += 1;
    }
}
