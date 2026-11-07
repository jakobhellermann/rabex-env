//! Where a type tree puts an object's data in its bytes: how long each field is and
//! where padding goes. Reading past fields and copying them needs no values.
//!
//! A tree is worked out once into a [`Layout`]: what each node is, whether it pads
//! after itself, and its size where that is fixed. Reading and copying go by that.
//!
//! Alignment counts from the start of the object: a field that aligns is followed by
//! padding up to the next multiple of four bytes from there.

use anyhow::{Context, Result, bail};
use rabex::typetree::{TypeTreeNode, TypetreeNodeKind as Kind};

/// The element of a `vector`, past its size field.
pub fn element(node: &TypeTreeNode) -> Option<&TypeTreeNode> {
    match node.classify() {
        Kind::ArrayWrapper => node.children.first()?.children.get(1),
        _ => None,
    }
}

/// The key and value of a `map`.
pub fn pair(node: &TypeTreeNode) -> Option<(&TypeTreeNode, &TypeTreeNode)> {
    match node.classify() {
        Kind::Map => {
            let pair = node.children.first()?.children.get(1)?;
            Some((pair.children.first()?, pair.children.get(1)?))
        }
        _ => None,
    }
}

/// Whether the data after `node` is padded to four bytes: its own flag; for a vector
/// also its Array's; after a string always.
pub fn aligns_after(node: &TypeTreeNode) -> bool {
    match node.classify() {
        Kind::String => true,
        Kind::ArrayWrapper => {
            node.requires_align()
                || node
                    .children
                    .first()
                    .is_some_and(TypeTreeNode::requires_align)
        }
        // a map pads after itself by its own flag: across every class of every version
        // in the type tree package, no pair and no Array of a map carries one
        _ => node.requires_align(),
    }
}

/// One node of a tree, as its data lies in the bytes.
#[derive(Debug, Clone)]
pub struct Layout {
    shape: Shape,
    aligns: bool,
    /// The size of the data, its own trailing padding not included, where it has no
    /// variable length part and no padding inside: then it is the same wherever it sits.
    fixed: Option<usize>,
}

#[derive(Debug, Clone)]
enum Shape {
    /// A number, bool or char of this many bytes, or nothing.
    Scalar(usize),
    /// A length and that many bytes: a string, raw data.
    Blob,
    Array(Box<Layout>),
    Map(Box<Layout>, Box<Layout>),
    Struct(Vec<Layout>),
    /// A node the bytes cannot carry, named for the error reading it raises.
    Unreadable(String),
}

impl Layout {
    pub fn of(node: &TypeTreeNode) -> Layout {
        let aligns = aligns_after(node);
        let unreadable = || Shape::Unreadable(format!("{} {}", node.m_Type, node.m_Name));
        let shape = match node.classify() {
            Kind::Bool | Kind::Char | Kind::U8 | Kind::I8 => Shape::Scalar(1),
            Kind::U16 | Kind::I16 => Shape::Scalar(2),
            Kind::U32 | Kind::I32 | Kind::Float => Shape::Scalar(4),
            Kind::U64 | Kind::I64 | Kind::Double => Shape::Scalar(8),
            Kind::Empty | Kind::TodoType => Shape::Scalar(0),
            Kind::String | Kind::Untyped => Shape::Blob,
            Kind::ArrayWrapper => match element(node) {
                Some(element) => Shape::Array(Box::new(Layout::of(element))),
                None => unreadable(),
            },
            Kind::Map => match pair(node) {
                Some((key, value)) => {
                    Shape::Map(Box::new(Layout::of(key)), Box::new(Layout::of(value)))
                }
                None => unreadable(),
            },
            Kind::Struct => Shape::Struct(node.children.iter().map(Layout::of).collect()),
            Kind::Array | Kind::TodoReferenced => unreadable(),
        };
        let fixed = match &shape {
            Shape::Scalar(size) => Some(*size),
            Shape::Struct(fields) => fields
                .iter()
                .map(|field| field.fixed.filter(|_| !field.aligns))
                .sum(),
            _ => None,
        };
        Layout {
            shape,
            aligns,
            fixed,
        }
    }

    /// Whether the data after this node is padded to four bytes.
    pub fn aligns_after(&self) -> bool {
        self.aligns
    }

    /// The size of the data, if it is the same wherever it sits; its own trailing
    /// padding not included.
    pub fn fixed_size(&self) -> Option<usize> {
        self.fixed
    }
}

/// A position in one object's bytes.
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// At the start of an object's bytes.
    pub fn new(object: &'a [u8]) -> Self {
        Self {
            bytes: object,
            pos: 0,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn is_at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }

    /// Move past one field.
    pub fn skip(&mut self, layout: &Layout) -> Result<()> {
        match (layout.fixed, &layout.shape) {
            (Some(size), _) => {
                self.take(size)?;
            }
            (None, Shape::Blob) => {
                let n = self.count()?;
                self.take(n)?;
            }
            (None, Shape::Array(element)) => {
                let n = self.count()?;
                match element.fixed.filter(|_| !element.aligns) {
                    Some(size) => {
                        self.take(n.checked_mul(size).context("an array past any size")?)?;
                    }
                    None => {
                        for _ in 0..n {
                            self.skip(element)?;
                        }
                    }
                }
            }
            (None, Shape::Map(key, value)) => {
                let n = self.count()?;
                for _ in 0..n {
                    self.skip(key)?;
                    self.skip(value)?;
                }
            }
            (None, Shape::Struct(fields)) => {
                for field in fields {
                    self.skip(field)?;
                }
            }
            (None, Shape::Unreadable(node)) => bail!("{node}: not a field the bytes can carry"),
            (None, Shape::Scalar(_)) => unreachable!("a scalar's size is fixed"),
        }
        self.align(layout.aligns);
        Ok(())
    }

    /// Move past one field and return its bytes, its padding included.
    pub fn field(&mut self, layout: &Layout) -> Result<&'a [u8]> {
        let start = self.pos;
        self.skip(layout)?;
        Ok(&self.bytes[start..self.pos])
    }

    /// Move past the padding to four bytes, if `aligns`.
    pub fn align(&mut self, aligns: bool) {
        if aligns {
            self.pos = self.pos.next_multiple_of(4);
        }
    }

    /// The next `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self
            .bytes
            .get(self.pos..self.pos + n)
            .context("the object ends inside a field")?;
        self.pos += n;
        Ok(bytes)
    }

    /// An array, map or string length.
    pub fn count(&mut self) -> Result<usize> {
        let len = i32::from_le_bytes(self.take(4)?.try_into()?);
        usize::try_from(len).with_context(|| format!("a length of {len}"))
    }
}

/// Copy one field from `from` to the end of `out`, which holds one object from its
/// start, padded for where it lands. Its bytes go as they are where both sides are as
/// far from a four byte boundary, or where nothing in it pads; otherwise only the parts
/// that pad are laid out anew.
pub fn copy(layout: &Layout, from: &mut Reader, out: &mut Vec<u8>) -> Result<()> {
    if from.pos % 4 == out.len() % 4 || layout.fixed.is_some() && !layout.aligns {
        out.extend_from_slice(from.field(layout)?);
        return Ok(());
    }
    match &layout.shape {
        Shape::Scalar(size) => out.extend_from_slice(from.take(*size)?),
        Shape::Blob => {
            let n = from.count()?;
            out.extend_from_slice(&(n as i32).to_le_bytes());
            out.extend_from_slice(from.take(n)?);
        }
        Shape::Array(element) => {
            let n = from.count()?;
            out.extend_from_slice(&(n as i32).to_le_bytes());
            for _ in 0..n {
                copy(element, from, out)?;
            }
        }
        Shape::Map(key, value) => {
            let n = from.count()?;
            out.extend_from_slice(&(n as i32).to_le_bytes());
            for _ in 0..n {
                copy(key, from, out)?;
                copy(value, from, out)?;
            }
        }
        Shape::Struct(fields) => {
            for field in fields {
                copy(field, from, out)?;
            }
        }
        Shape::Unreadable(node) => bail!("{node}: not a field the bytes can carry"),
    }
    if layout.aligns {
        from.align(true);
        align(out);
    }
    Ok(())
}

/// Pad `out`, which holds one object from its start, to four bytes.
pub fn align(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}
