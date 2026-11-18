//! Rewriting fixed size fields of an object's bytes in place, by its type tree.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use rabex::typetree::{TypeTreeNode, TypetreeNodeKind as Kind};

use crate::layout::{Layout, Reader};

/// Field names from the root, written `fsm.preprocessed`.
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

/// Encoded by the type of the field it is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValue {
    Bool(bool),
    Int(i64),
}

pub struct FieldRewriter {
    steps: Vec<Step>,
}

/// One field of a struct, in order, up to the last one the rewrite needs.
enum Step {
    Skip(Layout),
    Write(Layout, Vec<u8>),
    /// A struct with rewritten fields; its steps cover all its fields where more steps follow.
    Enter(Layout, Vec<Step>),
}

/// A rewritten field, by its path below the struct being planned.
struct Field<'a> {
    rest: &'a [String],
    path: &'a FieldPath,
    value: FieldValue,
}

impl FieldRewriter {
    pub fn new(tree: &TypeTreeNode, fields: &[(FieldPath, FieldValue)]) -> Result<Self> {
        let fields: Vec<Field> = fields
            .iter()
            .map(|(path, value)| Field {
                rest: &path.0,
                path,
                value: *value,
            })
            .collect();
        Ok(FieldRewriter {
            steps: plan(tree, &fields, false)?,
        })
    }

    pub fn apply(&self, object: &mut Cow<'_, [u8]>) -> Result<()> {
        let mut writes = Vec::new();
        collect(&self.steps, &mut Reader::new(object), &mut writes)?;
        for (start, bytes) in writes {
            let end = start + bytes.len();
            if object[start..end] != *bytes {
                object.to_mut()[start..end].copy_from_slice(bytes);
            }
        }
        Ok(())
    }
}

/// `whole`: the steps cover all fields of `node`, as more steps follow it.
fn plan(node: &TypeTreeNode, fields: &[Field], whole: bool) -> Result<Vec<Step>> {
    let mut by_child: BTreeMap<usize, Vec<Field>> = BTreeMap::new();
    for field in fields {
        let (name, rest) = field.rest.split_first().expect("field paths are not empty");
        ensure!(
            matches!(node.classify(), Kind::Struct),
            "{}: {} {} has no fields",
            field.path,
            node.m_Type,
            node.m_Name
        );
        let index = node
            .children
            .iter()
            .position(|child| &child.m_Name == name)
            .with_context(|| format!("{}: {} has no field {name}", field.path, node.m_Type))?;
        by_child
            .entry(index)
            .or_default()
            .push(Field { rest, ..*field });
    }

    let last = by_child.keys().last().copied();
    let end = if whole {
        node.children.len()
    } else {
        last.map_or(0, |last| last + 1)
    };
    let mut steps = Vec::with_capacity(end);
    for (index, child) in node.children[..end].iter().enumerate() {
        let layout = Layout::of(child);
        let step = match by_child.remove(&index) {
            None => Step::Skip(layout),
            Some(fields) => match fields.as_slice() {
                [field] if field.rest.is_empty() => {
                    let bytes =
                        encode(child, field.value).with_context(|| field.path.to_string())?;
                    Step::Write(layout, bytes)
                }
                _ => {
                    if let Some(field) = fields.iter().find(|field| field.rest.is_empty()) {
                        bail!("{} overlaps another rewritten field", field.path);
                    }
                    let whole = whole || Some(index) != last;
                    Step::Enter(layout, plan(child, &fields, whole)?)
                }
            },
        };
        steps.push(step);
    }
    Ok(steps)
}

fn collect<'s>(
    steps: &'s [Step],
    reader: &mut Reader,
    writes: &mut Vec<(usize, &'s [u8])>,
) -> Result<()> {
    for step in steps {
        match step {
            Step::Skip(layout) => reader.skip(layout)?,
            Step::Write(layout, bytes) => {
                writes.push((reader.pos(), bytes));
                reader.skip(layout)?;
            }
            Step::Enter(layout, steps) => {
                collect(steps, reader, writes)?;
                reader.pad_after(layout);
            }
        }
    }
    Ok(())
}

fn encode(node: &TypeTreeNode, value: FieldValue) -> Result<Vec<u8>> {
    let field = || format!("{} {}", node.m_Type, node.m_Name);
    let int = match value {
        FieldValue::Int(int) => int,
        FieldValue::Bool(bool) => bool as i64,
    };
    let out_of_range = |_| anyhow::anyhow!("{value:?} is out of range for {}", field());
    Ok(match node.classify() {
        Kind::Bool => match value {
            FieldValue::Bool(bool) => vec![bool as u8],
            FieldValue::Int(int @ (0 | 1)) => vec![int as u8],
            FieldValue::Int(_) => bail!("{value:?} for the bool field {}", field()),
        },
        Kind::U8 => vec![u8::try_from(int).map_err(out_of_range)?],
        Kind::I8 => i8::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::U16 => u16::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::I16 => i16::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::U32 => u32::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::I32 => i32::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::U64 => u64::try_from(int)
            .map_err(out_of_range)?
            .to_le_bytes()
            .to_vec(),
        Kind::I64 => int.to_le_bytes().to_vec(),
        _ => bail!(
            "only integer and bool fields can be rewritten, not {}",
            field()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.into(),
            m_Name: name.into(),
            children,
            ..Default::default()
        }
    }
    fn leaf(ty: &str, name: &str) -> TypeTreeNode {
        node(ty, name, vec![])
    }

    /// `int a; string s; Fsm fsm { bool preprocessed; int x; } int z;`
    fn tree() -> TypeTreeNode {
        let array = node(
            "Array",
            "Array",
            vec![leaf("int", "size"), leaf("char", "data")],
        );
        let string = node("string", "s", vec![array]);
        let fsm = node(
            "Fsm",
            "fsm",
            vec![leaf("bool", "preprocessed"), leaf("int", "x")],
        );
        node(
            "Base",
            "Base",
            vec![leaf("int", "a"), string, fsm, leaf("int", "z")],
        )
    }

    /// a = 5, s = "abc" and its padding, preprocessed = true, x = 7, z = 9
    const OBJECT: &[u8] = &[
        5, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 1, 7, 0, 0, 0, 9, 0, 0, 0,
    ];

    fn rewriter(fields: &[(&str, FieldValue)]) -> Result<FieldRewriter> {
        let fields: Vec<_> = fields
            .iter()
            .map(|(path, value)| (path.parse().unwrap(), *value))
            .collect();
        FieldRewriter::new(&tree(), &fields)
    }

    fn rewritten(fields: &[(&str, FieldValue)]) -> Vec<u8> {
        let mut object = Cow::Borrowed(OBJECT);
        rewriter(fields).unwrap().apply(&mut object).unwrap();
        object.into_owned()
    }

    #[test]
    fn rewrites_fields_past_variable_length_ones() {
        assert_eq!(
            rewritten(&[
                ("fsm.preprocessed", FieldValue::Bool(false)),
                ("fsm.x", FieldValue::Int(-2)),
            ]),
            [
                5, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 0, 0xfe, 0xff, 0xff, 0xff, 9, 0, 0, 0
            ]
        );
    }

    #[test]
    fn rewrites_a_field_after_a_rewritten_struct() {
        assert_eq!(
            rewritten(&[
                ("z", FieldValue::Int(3)),
                ("fsm.preprocessed", FieldValue::Bool(false)),
            ]),
            [
                5, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 0, 7, 0, 0, 0, 3, 0, 0, 0
            ]
        );
    }

    #[test]
    fn keeps_the_object_borrowed_when_nothing_changes() {
        let rewriter = rewriter(&[("fsm.preprocessed", FieldValue::Bool(true))]).unwrap();
        let mut object = Cow::Borrowed(OBJECT);
        rewriter.apply(&mut object).unwrap();
        assert!(matches!(object, Cow::Borrowed(_)));
    }

    #[test]
    fn rejects_a_missing_field() {
        let error = rewriter(&[("fsm.nope", FieldValue::Int(0))]).err().unwrap();
        assert_eq!(format!("{error:#}"), "fsm.nope: Fsm has no field nope");
    }

    #[test]
    fn rejects_a_path_into_a_field_without_fields() {
        let error = rewriter(&[("a.b", FieldValue::Int(0))]).err().unwrap();
        assert_eq!(format!("{error:#}"), "a.b: int a has no fields");
    }

    #[test]
    fn rejects_a_field_of_variable_length() {
        let error = rewriter(&[("s", FieldValue::Int(0))]).err().unwrap();
        assert_eq!(
            format!("{error:#}"),
            "s: only integer and bool fields can be rewritten, not string s"
        );
    }

    #[test]
    fn rejects_overlapping_fields() {
        let error = rewriter(&[("fsm.x", FieldValue::Int(0)), ("fsm.x", FieldValue::Int(1))])
            .err()
            .unwrap();
        assert_eq!(
            format!("{error:#}"),
            "fsm.x overlaps another rewritten field"
        );
    }

    #[test]
    fn rejects_a_value_out_of_range() {
        let tree = node("Base", "Base", vec![leaf("UInt8", "small")]);
        let fields = [("small".parse().unwrap(), FieldValue::Int(300))];
        let error = FieldRewriter::new(&tree, &fields).err().unwrap();
        assert_eq!(
            format!("{error:#}"),
            "small: Int(300) is out of range for UInt8 small"
        );
    }

    #[test]
    fn rejects_an_empty_path_segment() {
        let error = "fsm..x".parse::<FieldPath>().err().unwrap();
        assert_eq!(error.to_string(), "'fsm..x' is not a field path");
    }
}
