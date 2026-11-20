//! Field sizes and padding of a type tree, to skip fields without reading their values.

use anyhow::{Context, Result, bail};
use rabex::typetree::{TypeTreeNode, TypetreeNodeKind as Kind};

/// The element node of a `vector`, past its size node.
pub fn element(node: &TypeTreeNode) -> Option<&TypeTreeNode> {
    match node.classify() {
        Kind::ArrayWrapper => node.children.first()?.children.get(1),
        _ => None,
    }
}

fn pair(node: &TypeTreeNode) -> Option<(&TypeTreeNode, &TypeTreeNode)> {
    match node.classify() {
        Kind::Map => {
            let pair = node.children.first()?.children.get(1)?;
            Some((pair.children.first()?, pair.children.get(1)?))
        }
        _ => None,
    }
}

/// A vector also pads by its Array's flag, a string always.
fn aligns_after(node: &TypeTreeNode) -> bool {
    match node.classify() {
        Kind::String => true,
        Kind::ArrayWrapper => {
            node.requires_align()
                || node
                    .children
                    .first()
                    .is_some_and(TypeTreeNode::requires_align)
        }
        // no pair or Array of a map carries the flag in any class of the type tree package
        _ => node.requires_align(),
    }
}

#[derive(Debug, Clone)]
pub struct Layout {
    shape: Shape,
    /// Padding to four bytes from the object start follows the field.
    aligns: bool,
    /// Without its trailing padding; only for fields without length prefixes or inner padding.
    fixed: Option<usize>,
}

#[derive(Debug, Clone)]
enum Shape {
    Scalar(usize),
    /// Length prefixed bytes: a string or raw data.
    Blob,
    Array(Box<Layout>),
    Map(Box<Layout>, Box<Layout>),
    Struct(Vec<Layout>),
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
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(object: &'a [u8]) -> Self {
        Self {
            bytes: object,
            pos: 0,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

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
        self.pad_after(layout);
        Ok(())
    }

    /// Past the padding following the field of `layout`, once at its end.
    pub fn pad_after(&mut self, layout: &Layout) {
        if layout.aligns {
            self.pos = self.pos.next_multiple_of(4);
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self
            .bytes
            .get(self.pos..self.pos + n)
            .context("the object ends inside a field")?;
        self.pos += n;
        Ok(bytes)
    }

    pub fn count(&mut self) -> Result<usize> {
        let len = i32::from_le_bytes(self.take(4)?.try_into()?);
        usize::try_from(len).with_context(|| format!("a length of {len}"))
    }
}
