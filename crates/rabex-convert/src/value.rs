//! Untyped values of typetree data, and how type trees compare.
//!
//! The carrier is [`serde_value::Value`], built with order-preserving maps: the fields
//! keep the order they were put in, which is the order the bytes are read in. Rules read
//! and make values of single fields; class models stay for rules that change semantics,
//! like the shader blob conversion.
//!
//! A map value keeps one entry per key: a map with a key twice loses the rest. The
//! re-encode reads values only of fields a rule decides.

use rabex::typetree::TypeTreeNode;

pub use serde_value::Value;

/// The value of a struct's field.
pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Map(fields) => fields.get(&Value::String(name.to_owned())),
        _ => None,
    }
}

/// [`field`], to change in place.
pub fn field_mut<'a>(value: &'a mut Value, name: &str) -> Option<&'a mut Value> {
    match value {
        Value::Map(fields) => fields.get_mut(&Value::String(name.to_owned())),
        _ => None,
    }
}

/// Whether two type trees describe the same byte layout: same types and names in the same
/// order, aligned alike. Other node metadata like sizes does not change the layout of
/// the data.
pub fn layout_equal(a: &TypeTreeNode, b: &TypeTreeNode) -> bool {
    a.m_Type == b.m_Type
        && a.m_Name == b.m_Name
        && a.requires_align() == b.requires_align()
        && a.children.len() == b.children.len()
        && a.children
            .iter()
            .zip(&b.children)
            .all(|(a, b)| layout_equal(a, b))
}

/// Whether the tree carries a field of this name at its top level: how a rule reads
/// which form the target version's class has.
pub fn has_field(tree: &TypeTreeNode, name: &str) -> bool {
    tree.children.iter().any(|child| child.m_Name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabex::serde_typetree;

    /// A struct value of the given fields, in the given order.
    fn map(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::Map(
            fields
                .into_iter()
                .map(|(name, value)| (Value::String(name.to_owned()), value))
                .collect(),
        )
    }

    fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.to_owned(),
            m_Name: name.to_owned(),
            children,
            ..Default::default()
        }
    }

    /// A `bool` aligned after it moves every following field; the same fields unaligned
    /// are another layout, even with the same names and types.
    #[test]
    fn alignment_is_part_of_the_layout() {
        let tree = |align: bool| {
            let mut flag = node("bool", "m_Flag", vec![]);
            flag.m_MetaFlag = Some(if align { 0x4000 } else { 0 });
            node("Base", "Base", vec![flag, node("SInt32", "m_Next", vec![])])
        };
        assert!(layout_equal(&tree(true), &tree(true)));
        assert!(!layout_equal(&tree(true), &tree(false)));
        // the bytes say so too: three bytes of padding apart
        let value = map([("m_Flag", Value::Bool(true)), ("m_Next", Value::I32(1))]);
        let aligned =
            serde_typetree::to_vec::<_, byteorder::LittleEndian>(&value, &tree(true)).unwrap();
        let packed =
            serde_typetree::to_vec::<_, byteorder::LittleEndian>(&value, &tree(false)).unwrap();
        assert_eq!((aligned.len(), packed.len()), (8, 5));
    }

    #[test]
    fn values_roundtrip_through_a_tree() {
        let tree = node(
            "Sample",
            "",
            vec![
                node("SInt32", "count", vec![]),
                node("string", "name", vec![]),
                node(
                    "TypelessData",
                    "blob",
                    vec![node("int", "size", vec![]), node("UInt8", "data", vec![])],
                ),
            ],
        );
        let value = map([
            ("count", Value::I32(3)),
            ("name", Value::String("three".to_owned())),
            // raw data reads as a sequence of bytes, which serde_json::Value takes
            ("blob", Value::Seq((1..=3).map(Value::U8).collect())),
        ]);
        let bytes = serde_typetree::to_vec::<_, byteorder::LittleEndian>(&value, &tree).unwrap();
        let back: Value =
            serde_typetree::from_slice::<_, byteorder::LittleEndian>(bytes.as_slice(), &tree)
                .unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn layout_equality_follows_types_and_names() {
        let same = node("A", "x", vec![node("SInt32", "y", vec![])]);
        assert!(layout_equal(
            &same,
            &node("A", "x", vec![node("SInt32", "y", vec![])])
        ));
        assert!(!layout_equal(
            &same,
            &node("A", "x", vec![node("SInt64", "y", vec![])])
        ));
        assert!(!layout_equal(
            &same,
            &node("A", "x", vec![node("SInt32", "z", vec![])])
        ));
    }

    #[test]
    fn a_struct_cannot_stand_in_for_a_number() {
        let float = node("float", "x", vec![]);
        let empty = Value::Map(Default::default());
        let error =
            serde_typetree::to_vec::<_, byteorder::LittleEndian>(&empty, &float).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid type: a struct of 0 fields, expected float x"
        );
    }
}
