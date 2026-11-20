//! Reading and writing scalar fields directly in an object's bytes, without deserializing it.
//!
//! ```no_run
//! # use std::borrow::Cow;
//! # use rabex::typetree::TypeTreeNode;
//! # use rabex_env::rewrite::{Fields, Scalar};
//! # fn f(tree: &TypeTreeNode, object: &mut Cow<'_, [u8]>) -> anyhow::Result<()> {
//! // fold `discreteCurveCount` into `curveCount` in every StreamedClip of an AnimationClip
//! let names = ["curveCount", "discreteCurveCount"];
//! if let Some(fields) = Fields::of_type(tree, "StreamedClip", &names)? {
//!     fields.rewrite(object, |values| {
//!         let [Scalar::Int(curves), Scalar::Int(discrete)] = values else {
//!             anyhow::bail!("curve counts that aren't integers");
//!         };
//!         *curves += *discrete;
//!         *discrete = 0;
//!         Ok(())
//!     })?;
//! }
//! # Ok(())
//! # }
//! ```
use std::borrow::Cow;
use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use rabex::typetree::{TypeTreeNode, TypetreeNodeKind as Kind};

use crate::layout::{self, Layout, Reader};

/// Field names from the root of an object, written `fsm.preprocessed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPath(Vec<String>);

impl FromStr for FieldPath {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let names: Vec<String> = s.split('.').map(str::to_owned).collect();
        ensure!(
            names.iter().all(|name| !name.is_empty()),
            "'{s}' is not a field path"
        );
        Ok(FieldPath(names))
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("."))
    }
}

/// The value of a scalar field: bools, integers of any width, floats and doubles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
}

/// Where some scalar fields lie in the objects of one type tree.
pub struct Fields {
    steps: Vec<Step>,
    /// The fields of each occurrence, as asked for.
    names: Vec<String>,
}

impl Fields {
    /// The fields at `paths`, one occurrence per object.
    pub fn at(tree: &TypeTreeNode, paths: &[FieldPath]) -> Result<Fields> {
        let targets: Vec<(&[String], usize)> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.0.as_slice(), index))
            .collect();
        let mut steps = plan_paths(tree, &targets).with_context(|| {
            let paths: Vec<String> = paths.iter().map(ToString::to_string).collect();
            format!("fields {}", paths.join(", "))
        })?;
        trim_trailing_skips(&mut steps);
        Ok(Fields {
            steps: vec![Step::Occurrence(Layout::of(tree), steps)],
            names: paths.iter().map(ToString::to_string).collect(),
        })
    }

    /// The fields `names` of every struct of type `ty`, wherever in the tree it is, one
    /// occurrence per struct. `None` if the tree has no such struct.
    pub fn of_type(tree: &TypeTreeNode, ty: &str, names: &[&str]) -> Result<Option<Fields>> {
        if !contains(tree, ty) {
            return Ok(None);
        }
        let mut steps =
            plan_type(tree, ty, names).with_context(|| format!("{} of {ty}", names.join(", ")))?;
        trim_trailing_skips(&mut steps);
        Ok(Some(Fields {
            steps,
            names: names.iter().map(|name| name.to_string()).collect(),
        }))
    }

    /// Calls `f` with the fields of each occurrence in `object`, in the order they were
    /// asked for.
    pub fn find(&self, object: &[u8], mut f: impl FnMut(&[Field]) -> Result<()>) -> Result<()> {
        let unset = Field {
            offset: 0,
            kind: ScalarKind::Bool,
        };
        let mut found = vec![unset; self.names.len()];

        let mut reader = Reader::new(object);
        walk(&self.steps, &mut reader, &mut found, &mut f)
    }

    /// Sets the fields of each occurrence in `object` to what `f` makes of their current
    /// values. `object` is copied only once a value changes.
    pub fn rewrite(
        &self,
        object: &mut Cow<'_, [u8]>,
        mut f: impl FnMut(&mut [Scalar]) -> Result<()>,
    ) -> Result<()> {
        let bytes: &[u8] = object;
        let mut writes = Vec::new();
        let mut values = Vec::with_capacity(self.names.len());

        self.find(bytes, |fields| {
            values.clear();
            for field in fields {
                values.push(field.read(bytes)?);
            }

            f(&mut values)?;

            writes.extend(
                fields
                    .iter()
                    .copied()
                    .zip(values.iter().copied())
                    .enumerate(),
            );
            Ok(())
        })?;

        let mut buf = [0u8; 8];
        for (index, (field, value)) in writes {
            // compare the bytes: a UInt8 field reads as an Int, while `f` may give it a Bool
            let encoded = field
                .encode(value, &mut buf)
                .with_context(|| self.names[index].clone())?;
            if object[field.range()] != *encoded {
                object.to_mut()[field.range()].copy_from_slice(encoded);
            }
        }
        Ok(())
    }
}

/// A scalar field found in an object.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    offset: usize,
    kind: ScalarKind,
}

impl Field {
    /// Where the field starts in the object's bytes.
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn size(&self) -> usize {
        self.kind.size()
    }

    fn range(&self) -> std::ops::Range<usize> {
        self.offset..self.offset + self.size()
    }

    pub fn read(&self, object: &[u8]) -> Result<Scalar> {
        let bytes = object
            .get(self.range())
            .context("the object ends inside the field")?;
        let int = |value: i128| {
            i64::try_from(value)
                .map(Scalar::Int)
                .with_context(|| format!("{value} doesn't fit a Scalar"))
        };
        Ok(match self.kind {
            ScalarKind::Bool => Scalar::Bool(bytes[0] != 0),
            ScalarKind::U8 => Scalar::Int(bytes[0].into()),
            ScalarKind::I8 => Scalar::Int(i8::from_le_bytes([bytes[0]]).into()),
            ScalarKind::U16 => Scalar::Int(u16::from_le_bytes(bytes.try_into()?).into()),
            ScalarKind::I16 => Scalar::Int(i16::from_le_bytes(bytes.try_into()?).into()),
            ScalarKind::U32 => Scalar::Int(u32::from_le_bytes(bytes.try_into()?).into()),
            ScalarKind::I32 => Scalar::Int(i32::from_le_bytes(bytes.try_into()?).into()),
            ScalarKind::U64 => int(u64::from_le_bytes(bytes.try_into()?).into())?,
            ScalarKind::I64 => Scalar::Int(i64::from_le_bytes(bytes.try_into()?)),
            ScalarKind::F32 => Scalar::Float(f32::from_le_bytes(bytes.try_into()?).into()),
            ScalarKind::F64 => Scalar::Float(f64::from_le_bytes(bytes.try_into()?)),
        })
    }

    fn encode<'b>(&self, value: Scalar, buf: &'b mut [u8; 8]) -> Result<&'b [u8]> {
        match (self.kind, value) {
            // scripts' bools are UInt8 in type trees
            (ScalarKind::Bool | ScalarKind::U8, Scalar::Bool(b)) => buf[0] = b.into(),
            (ScalarKind::F32, Scalar::Float(x)) => {
                buf[..4].copy_from_slice(&(x as f32).to_le_bytes())
            }
            (ScalarKind::F64, Scalar::Float(x)) => buf.copy_from_slice(&x.to_le_bytes()),
            (kind, Scalar::Int(n)) => {
                let range = kind
                    .int_range()
                    .with_context(|| format!("{value:?} for a {kind:?} field"))?;
                ensure!(
                    range.contains(&n.into()),
                    "{value:?} is out of range for a {kind:?}"
                );
                // little endian two's complement: an integer in range is the low bytes
                buf.copy_from_slice(&n.to_le_bytes());
            }
            (kind, value) => bail!("{value:?} for a {kind:?} field"),
        }
        Ok(&buf[..self.size()])
    }
}

#[derive(Debug, Clone, Copy)]
enum ScalarKind {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl ScalarKind {
    fn of(node: &TypeTreeNode) -> Option<ScalarKind> {
        Some(match node.classify() {
            Kind::Bool => ScalarKind::Bool,
            Kind::U8 | Kind::Char => ScalarKind::U8,
            Kind::I8 => ScalarKind::I8,
            Kind::U16 => ScalarKind::U16,
            Kind::I16 => ScalarKind::I16,
            Kind::U32 => ScalarKind::U32,
            Kind::I32 => ScalarKind::I32,
            Kind::U64 => ScalarKind::U64,
            Kind::I64 => ScalarKind::I64,
            Kind::Float => ScalarKind::F32,
            Kind::Double => ScalarKind::F64,
            _ => return None,
        })
    }

    fn int_range(self) -> Option<RangeInclusive<i128>> {
        Some(match self {
            ScalarKind::U8 => u8::MIN.into()..=u8::MAX.into(),
            ScalarKind::I8 => i8::MIN.into()..=i8::MAX.into(),
            ScalarKind::U16 => u16::MIN.into()..=u16::MAX.into(),
            ScalarKind::I16 => i16::MIN.into()..=i16::MAX.into(),
            ScalarKind::U32 => u32::MIN.into()..=u32::MAX.into(),
            ScalarKind::I32 => i32::MIN.into()..=i32::MAX.into(),
            ScalarKind::U64 => u64::MIN.into()..=u64::MAX.into(),
            ScalarKind::I64 => i64::MIN.into()..=i64::MAX.into(),
            ScalarKind::Bool | ScalarKind::F32 | ScalarKind::F64 => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            ScalarKind::Bool | ScalarKind::U8 | ScalarKind::I8 => 1,
            ScalarKind::U16 | ScalarKind::I16 => 2,
            ScalarKind::U32 | ScalarKind::I32 | ScalarKind::F32 => 4,
            ScalarKind::U64 | ScalarKind::I64 | ScalarKind::F64 => 8,
        }
    }
}

enum Step {
    Skip(Layout),
    /// The field asked for at this index.
    Field(usize, ScalarKind, Layout),
    /// A struct, with steps for all of its fields.
    Enter(Layout, Vec<Step>),
    /// A vector, with the step for each element.
    Each(Layout, Box<Step>),
    /// The object, or a struct of the type asked for: its fields are one occurrence.
    Occurrence(Layout, Vec<Step>),
}

fn plan_paths(node: &TypeTreeNode, targets: &[(&[String], usize)]) -> Result<Vec<Step>> {
    ensure!(
        matches!(node.classify(), Kind::Struct),
        "{} {} has no fields",
        node.m_Type,
        node.m_Name
    );
    let mut steps = Vec::with_capacity(node.children.len());
    for child in &node.children {
        let here: Vec<(&[String], usize)> = targets
            .iter()
            .filter(|(path, _)| path[0] == child.m_Name)
            .map(|&(path, index)| (&path[1..], index))
            .collect();
        steps.push(match here.as_slice() {
            [] => Step::Skip(Layout::of(child)),
            [([], index)] => {
                let kind = ScalarKind::of(child).with_context(|| {
                    format!("{} {} is not a scalar", child.m_Type, child.m_Name)
                })?;
                Step::Field(*index, kind, Layout::of(child))
            }
            _ => {
                ensure!(
                    here.iter().all(|(rest, _)| !rest.is_empty()),
                    "{} is asked for together with fields inside it",
                    child.m_Name
                );
                Step::Enter(Layout::of(child), plan_paths(child, &here)?)
            }
        });
    }
    for (path, _) in targets {
        ensure!(
            node.children.iter().any(|child| child.m_Name == path[0]),
            "{} has no field {}",
            node.m_Type,
            path[0]
        );
    }
    Ok(steps)
}

fn plan_type(node: &TypeTreeNode, ty: &str, names: &[&str]) -> Result<Vec<Step>> {
    let mut steps = Vec::with_capacity(node.children.len());
    for child in &node.children {
        steps.push(plan_type_node(child, ty, names)?);
    }
    Ok(steps)
}

fn plan_type_node(node: &TypeTreeNode, ty: &str, names: &[&str]) -> Result<Step> {
    let layout = Layout::of(node);
    if !contains(node, ty) {
        return Ok(Step::Skip(layout));
    }
    if node.m_Type == ty {
        let paths: Vec<Vec<String>> = names.iter().map(|name| vec![name.to_string()]).collect();
        let targets: Vec<(&[String], usize)> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.as_slice(), index))
            .collect();
        return Ok(Step::Occurrence(layout, plan_paths(node, &targets)?));
    }
    match node.classify() {
        Kind::Struct => Ok(Step::Enter(layout, plan_type(node, ty, names)?)),
        Kind::ArrayWrapper => {
            let element = layout::element(node).context("a vector without element")?;
            Ok(Step::Each(
                layout,
                Box::new(plan_type_node(element, ty, names)?),
            ))
        }
        _ => bail!(
            "a {ty} inside {} {}, which isn't a struct or vector",
            node.m_Type,
            node.m_Name
        ),
    }
}

/// Whether `node` is or holds a struct of type `ty`.
fn contains(node: &TypeTreeNode, ty: &str) -> bool {
    node.m_Type == ty || node.children.iter().any(|child| contains(child, ty))
}

/// Past the last field asked for, the rest of the object needn't be read.
fn trim_trailing_skips(steps: &mut Vec<Step>) {
    while matches!(steps.last(), Some(Step::Skip(_))) {
        steps.pop();
    }
}

fn walk(
    steps: &[Step],
    reader: &mut Reader,
    found: &mut [Field],
    f: &mut impl FnMut(&[Field]) -> Result<()>,
) -> Result<()> {
    for step in steps {
        match step {
            Step::Skip(layout) => reader.skip(layout)?,
            Step::Field(index, kind, layout) => {
                found[*index] = Field {
                    offset: reader.pos(),
                    kind: *kind,
                };
                reader.skip(layout)?;
            }
            Step::Enter(layout, steps) => {
                walk(steps, reader, found, f)?;
                reader.pad_after(layout);
            }
            Step::Each(layout, element) => {
                for _ in 0..reader.count()? {
                    walk(std::slice::from_ref(element), reader, found, f)?;
                }
                reader.pad_after(layout);
            }
            Step::Occurrence(layout, steps) => {
                walk(steps, reader, found, f)?;
                reader.pad_after(layout);
                f(found)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.to_owned(),
            m_Name: name.to_owned(),
            m_MetaFlag: Some(0),
            children,
            ..Default::default()
        }
    }

    fn leaf(ty: &str, name: &str) -> TypeTreeNode {
        node(ty, name, vec![])
    }

    fn aligned(mut node: TypeTreeNode) -> TypeTreeNode {
        node.m_MetaFlag = Some(0x4000);
        node
    }

    fn vector(name: &str, element: TypeTreeNode) -> TypeTreeNode {
        node(
            "vector",
            name,
            vec![node("Array", "Array", vec![leaf("int", "size"), element])],
        )
    }

    fn rewritten(
        fields: &Fields,
        object: &[u8],
        f: impl FnMut(&mut [Scalar]) -> Result<()>,
    ) -> Result<Cow<'static, [u8]>> {
        let mut data = Cow::Borrowed(object);
        fields.rewrite(&mut data, f)?;
        Ok(Cow::Owned(data.into_owned()))
    }

    #[test]
    fn fields_at_paths_are_written_in_place() -> Result<()> {
        // bool a (padded), int b, struct inner { bool c }
        let tree = node(
            "Script",
            "Base",
            vec![
                aligned(leaf("bool", "a")),
                leaf("int", "b"),
                node("Inner", "inner", vec![leaf("bool", "c")]),
            ],
        );
        let fields = Fields::at(&tree, &["inner.c".parse()?, "a".parse()?])?;
        let object = [0, 0, 0, 0, 7, 0, 0, 0, 0];
        let out = rewritten(&fields, &object, |values| {
            assert_eq!(values, [Scalar::Bool(false), Scalar::Bool(false)]);
            values.copy_from_slice(&[Scalar::Bool(true), Scalar::Bool(true)]);
            Ok(())
        })?;
        assert_eq!(*out, [1, 0, 0, 0, 7, 0, 0, 0, 1]);
        Ok(())
    }

    #[test]
    fn fields_past_variable_length_ones_are_found() -> Result<()> {
        // int a; string s; Fsm fsm { bool preprocessed; int x; } int z;
        let string = node(
            "string",
            "s",
            vec![node(
                "Array",
                "Array",
                vec![leaf("int", "size"), leaf("char", "data")],
            )],
        );
        let fsm = node(
            "Fsm",
            "fsm",
            vec![leaf("bool", "preprocessed"), leaf("int", "x")],
        );
        let tree = node(
            "Base",
            "Base",
            vec![leaf("int", "a"), string, fsm, leaf("int", "z")],
        );
        let fields = Fields::at(&tree, &["fsm.preprocessed".parse()?, "fsm.x".parse()?])?;
        // a = 5, s = "abc" and its padding, preprocessed = true, x = 7, z = 9
        let object = [
            5, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 1, 7, 0, 0, 0, 9, 0, 0, 0,
        ];
        let out = rewritten(&fields, &object, |values| {
            values.copy_from_slice(&[Scalar::Bool(false), Scalar::Int(-2)]);
            Ok(())
        })?;
        assert_eq!(
            *out,
            [
                5, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 0, 0xfe, 0xff, 0xff, 0xff, 9, 0, 0, 0
            ]
        );
        Ok(())
    }

    #[test]
    fn unchanged_values_leave_the_object_borrowed() -> Result<()> {
        let tree = node("Script", "Base", vec![leaf("int", "b")]);
        let fields = Fields::at(&tree, &["b".parse()?])?;
        let object = [7, 0, 0, 0];
        let mut data = Cow::Borrowed(&object[..]);
        fields.rewrite(&mut data, |values| {
            values[0] = Scalar::Int(7);
            Ok(())
        })?;
        assert!(matches!(data, Cow::Borrowed(_)));
        Ok(())
    }

    #[test]
    fn fields_of_a_type_are_found_in_every_struct_of_it() -> Result<()> {
        let pair = || {
            node(
                "Pair",
                "pair",
                vec![leaf("UInt16", "x"), leaf("UInt16", "y")],
            )
        };
        // Pair first, vector<Pair> items, string-free tail int
        let tree = node(
            "Clip",
            "Base",
            vec![pair(), vector("items", pair()), leaf("int", "tail")],
        );
        let fields = Fields::of_type(&tree, "Pair", &["x", "y"])?.expect("the tree has Pairs");
        #[rustfmt::skip]
        let object = [
            1, 0, 2, 0,             // pair
            2, 0, 0, 0,             // items: 2
            3, 0, 4, 0, 5, 0, 6, 0, // items[0], items[1]
            9, 0, 0, 0,             // tail
        ];
        let out = rewritten(&fields, &object, |values| {
            let [Scalar::Int(x), Scalar::Int(y)] = values else {
                bail!("not integers")
            };
            *x += *y;
            *y = 0;
            Ok(())
        })?;
        #[rustfmt::skip]
        assert_eq!(*out, [
            3, 0, 0, 0,
            2, 0, 0, 0,
            7, 0, 0, 0, 11, 0, 0, 0,
            9, 0, 0, 0,
        ]);
        Ok(())
    }

    #[test]
    fn a_tree_without_the_type_has_no_fields_of_it() -> Result<()> {
        let tree = node("Clip", "Base", vec![leaf("int", "tail")]);
        assert!(Fields::of_type(&tree, "Pair", &["x"])?.is_none());
        Ok(())
    }

    #[test]
    fn find_reports_where_fields_are() -> Result<()> {
        let tree = node(
            "Script",
            "Base",
            vec![aligned(leaf("bool", "a")), leaf("float", "f")],
        );
        let fields = Fields::at(&tree, &["f".parse()?])?;
        let object = [0, 0, 0, 0, 0, 0, 0x80, 0x3f];
        let mut seen = Vec::new();
        fields.find(&object, |found| {
            seen.push((found[0].offset(), found[0].size(), found[0].read(&object)?));
            Ok(())
        })?;
        assert_eq!(seen, [(4, 4, Scalar::Float(1.0))]);
        Ok(())
    }

    #[test]
    fn bad_requests_fail_when_planned() -> Result<()> {
        let tree = node(
            "Script",
            "Base",
            vec![node("Inner", "inner", vec![leaf("int", "c")])],
        );
        let error = |paths: &[&str]| -> String {
            let paths: Vec<FieldPath> = paths.iter().map(|p| p.parse().unwrap()).collect();
            format!("{:#}", Fields::at(&tree, &paths).err().expect("an error"))
        };
        assert_eq!(
            error(&["missing"]),
            "fields missing: Script has no field missing"
        );
        assert_eq!(
            error(&["inner"]),
            "fields inner: Inner inner is not a scalar"
        );
        assert_eq!(
            error(&["inner", "inner.c"]),
            "fields inner, inner.c: inner is asked for together with fields inside it"
        );
        Ok(())
    }

    #[test]
    fn values_must_fit_the_field() -> Result<()> {
        let tree = node(
            "Script",
            "Base",
            vec![leaf("UInt8", "n"), leaf("bool", "b")],
        );
        let fields = Fields::at(&tree, &["n".parse()?, "b".parse()?])?;
        let object = [0, 0];
        let error = |values: [Scalar; 2]| -> String {
            let result = rewritten(&fields, &object, |v| {
                v.copy_from_slice(&values);
                Ok(())
            });
            format!("{:#}", result.expect_err("an error"))
        };
        assert_eq!(
            error([Scalar::Int(256), Scalar::Bool(false)]),
            "n: Int(256) is out of range for a U8"
        );
        assert_eq!(
            error([Scalar::Int(0), Scalar::Int(1)]),
            "b: Int(1) for a Bool field"
        );
        Ok(())
    }

    #[test]
    fn writing_a_script_bool_it_already_has_leaves_the_object_borrowed() -> Result<()> {
        let tree = node("Script", "Base", vec![leaf("UInt8", "flag")]);
        let fields = Fields::at(&tree, &["flag".parse()?])?;
        let object = [0];
        let mut data = Cow::Borrowed(&object[..]);
        fields.rewrite(&mut data, |values| {
            values[0] = Scalar::Bool(false);
            Ok(())
        })?;
        assert!(matches!(data, Cow::Borrowed(_)));
        Ok(())
    }

    #[test]
    fn a_bool_goes_into_a_script_bool() -> Result<()> {
        let tree = node("Script", "Base", vec![leaf("UInt8", "flag")]);
        let fields = Fields::at(&tree, &["flag".parse()?])?;
        let out = rewritten(&fields, &[0], |values| {
            values[0] = Scalar::Bool(true);
            Ok(())
        })?;
        assert_eq!(*out, [1]);
        Ok(())
    }
}
