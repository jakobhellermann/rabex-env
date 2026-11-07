//! The retarget re-encodes objects whose class tree changed, carries the rest, and reports
//! dropped fields.

use std::borrow::Cow;
use std::collections::HashMap;

use rabex::files::SerializedFile;
use rabex::files::serializedfile::SerializedType;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::objects::ClassId;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex::{UnityVersion, serde_typetree};
use rabex_convert::retarget::{Report, Retarget};
use rabex_convert::value::Value;

/// A struct value of the given fields, in the given order.
fn map(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Value::String(name.to_owned()), value))
            .collect(),
    )
}

fn source() -> UnityVersion {
    "2022.3.62f3".parse().unwrap()
}

fn target() -> UnityVersion {
    "2020.2.2f1".parse().unwrap()
}

fn node(ty: &str, name: &str, children: Vec<TypeTreeNode>) -> TypeTreeNode {
    TypeTreeNode {
        m_Type: ty.to_owned(),
        m_Name: name.to_owned(),
        m_MetaFlag: Some(0),
        children,
        ..Default::default()
    }
}

/// Font of 2022.3 has `m_OnlyInSource`; Font of 2020.2 has `m_OnlyInTarget` instead.
struct TwoTrees {
    with_source_field: TypeTreeNode,
    with_target_field: TypeTreeNode,
}

impl TypeTreeProvider for TwoTrees {
    fn get_typetree_node(
        &self,
        class_id: ClassId,
        target_version: &UnityVersion,
    ) -> Option<Cow<'_, TypeTreeNode>> {
        if class_id != ClassId::Font {
            return None;
        }
        Some(Cow::Borrowed(if *target_version == target() {
            &self.with_target_field
        } else {
            &self.with_source_field
        }))
    }
}

fn two_trees() -> TwoTrees {
    let common = vec![node("SInt32", "m_A", vec![]), node("string", "m_B", vec![])];
    TwoTrees {
        with_source_field: node(
            "Font",
            "Font",
            [
                common.clone(),
                vec![node("SInt32", "m_OnlyInSource", vec![])],
            ]
            .concat(),
        ),
        // the target version dropped the field instead of gaining one
        with_target_field: node("Font", "Font", common),
    }
}

/// A serialized file with one Font object encoded against `tree`.
fn serialized_file(tree: &TypeTreeNode, value: &Value) -> Vec<u8> {
    let trees = TwoTrees {
        with_source_field: tree.clone(),
        with_target_field: tree.clone(),
    };
    let bytes = serde_typetree::to_vec::<_, byteorder::LittleEndian>(value, tree).unwrap();
    let common_offsets = HashMap::new();
    let mut builder = SerializedFileBuilder::new(&source(), &trees, &common_offsets, true);
    let type_index = builder.add_type_uncached(SerializedType {
        m_ClassID: ClassId::Font,
        m_OldTypeHash: tree.hash(),
        m_Type: Some(tree.clone()),
        ..Default::default()
    });
    builder
        .add_object_untyped_with(1, ClassId::Font, type_index, bytes.into())
        .unwrap();
    builder.write_vec().unwrap()
}

#[test]
fn retarget_reencodes_changed_classes_and_reports_dropped_fields() {
    let trees = two_trees();
    let value = map([
        ("m_A", Value::I32(7)),
        ("m_B", Value::String("seven".to_owned())),
        ("m_OnlyInSource", Value::I32(9)),
    ]);
    let data = serialized_file(&trees.with_source_field, &value);

    let retarget = Retarget {
        to: target(),
        typetrees: &trees,
        common_strings: HashMap::new(),
        shader_chunk_size: 8 << 20,
        source_version: None,
        scripts: None,
    };
    let (retargeted, report) = retarget.serialized_file_reported(&data).unwrap();
    assert_eq!(report.reencoded, 1);
    assert_eq!(
        report.dropped,
        [("Font.m_OnlyInSource".to_owned(), 1usize)]
            .into_iter()
            .collect()
    );

    // the retargeted file reads with the target tree, the carried fields intact
    let data = retargeted.unwrap();
    let file = SerializedFile::from_reader(&mut std::io::Cursor::new(data.as_slice())).unwrap();
    assert_eq!(file.m_UnityVersion, Some(target()));
    let info = file.objects().next().unwrap();
    assert_eq!(info.m_ClassID, ClassId::Font);
    let read: Value = file
        .read(info, &trees, &mut std::io::Cursor::new(data.as_slice()))
        .unwrap();
    assert_eq!(
        read,
        map([
            ("m_A", Value::I32(7)),
            ("m_B", Value::String("seven".to_owned())),
        ])
    );
}

#[test]
fn retarget_carries_files_without_tree_changes() {
    let trees = two_trees();
    let value = map([
        ("m_A", Value::I32(7)),
        ("m_B", Value::String("seven".to_owned())),
        ("m_OnlyInSource", Value::I32(9)),
    ]);
    // the file's tree matches what the provider serves for the file's own version
    let data = serialized_file(&trees.with_source_field, &value);
    // but the target version's tree is the same tree: nothing to do
    let same_source_and_target = TwoTrees {
        with_source_field: trees.with_source_field.clone(),
        with_target_field: trees.with_source_field.clone(),
    };
    let retarget = Retarget {
        to: target(),
        typetrees: &same_source_and_target,
        common_strings: HashMap::new(),
        shader_chunk_size: 8 << 20,
        source_version: None,
        scripts: None,
    };
    let (retargeted, report) = retarget.serialized_file_reported(&data).unwrap();
    // the objects carry over, but the file's version string becomes the target's
    let retargeted = retargeted.expect("the version string changes");
    let file =
        SerializedFile::from_reader(&mut std::io::Cursor::new(retargeted.as_slice())).unwrap();
    assert_eq!(file.m_UnityVersion.as_ref().unwrap(), &target());
    assert_eq!(
        report,
        Report {
            reencoded: 0,
            carried: 1,
            dropped: Default::default(),
            removed: Default::default(),
            renamed_fields: Default::default(),
            warnings: Default::default(),
        }
    );
}

#[test]
fn retarget_fails_loudly_when_the_target_wants_unknown_fields() {
    let trees = two_trees();
    let value = map([
        ("m_A", Value::I32(7)),
        ("m_B", Value::String("seven".to_owned())),
    ]);
    let data = serialized_file(&trees.with_target_field, &value);
    // retargeting to a file whose tree wants m_OnlyInSource that the data lacks
    let inverse = Retarget {
        to: target(),
        common_strings: HashMap::new(),
        shader_chunk_size: 8 << 20,
        typetrees: &TwoTrees {
            with_source_field: trees.with_target_field.clone(),
            with_target_field: trees.with_source_field.clone(),
        },
        source_version: None,
        scripts: None,
    };
    let err = format!("{:#}", inverse.serialized_file_reported(&data).unwrap_err());
    assert_eq!(
        err,
        "Font 1: Font.m_OnlyInSource: the target wants it, the source lacks it, and no rule provides it"
    );
}

#[test]
fn objects_of_a_class_the_target_lacks_leave_with_their_type() {
    let trees = two_trees();
    let font = map([
        ("m_A", Value::I32(7)),
        ("m_B", Value::String("seven".to_owned())),
    ]);
    // a class the provider has no tree of for any version, written first so its type
    // entry comes before the one that stays
    let gone_tree = node("Mesh", "Base", vec![node("SInt32", "m_X", vec![])]);
    let gone = map([("m_X", Value::I32(1))]);

    let common_offsets = HashMap::new();
    let mut builder = SerializedFileBuilder::new(&source(), &trees, &common_offsets, true);
    let mut add = |path: i64, class: ClassId, tree: &TypeTreeNode, value: &Value| {
        let ty = builder.add_type_uncached(SerializedType {
            m_ClassID: class,
            m_OldTypeHash: tree.hash(),
            m_Type: Some(tree.clone()),
            ..Default::default()
        });
        let bytes = serde_typetree::to_vec::<_, byteorder::LittleEndian>(value, tree).unwrap();
        builder
            .add_object_untyped_with(path, class, ty, bytes.into())
            .unwrap();
    };
    add(1, ClassId::Mesh, &gone_tree, &gone);
    add(2, ClassId::Font, &trees.with_target_field, &font);
    let data = builder.write_vec().unwrap();

    let retarget = Retarget {
        to: target(),
        typetrees: &trees,
        common_strings: HashMap::new(),
        shader_chunk_size: 8 << 20,
        source_version: None,
        scripts: None,
    };
    let (retargeted, report) = retarget.serialized_file_reported(&data).unwrap();
    assert_eq!(
        report.removed,
        [("Mesh".to_owned(), 1usize)].into_iter().collect()
    );

    let data = retargeted.unwrap();
    let file = SerializedFile::from_reader(&mut std::io::Cursor::new(data.as_slice())).unwrap();
    assert_eq!(
        file.m_Types.len(),
        1,
        "the removed class's type stays behind"
    );
    let objects: Vec<_> = file.objects().collect();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].m_ClassID, ClassId::Font);
    assert_eq!(objects[0].m_TypeID, 0, "the type index did not move up");
    let read: Value = file
        .read(
            objects[0],
            &trees,
            &mut std::io::Cursor::new(data.as_slice()),
        )
        .unwrap();
    assert_eq!(read, font);
}

/// Type entries removed ahead of one that is re-encoded: the re-encoded objects get the
/// target tree in their own entry, and a removed class whose objects are spread over the
/// file goes whole.
#[test]
fn removed_types_ahead_of_a_reencoded_one_leave_its_entry_alone() {
    let trees = two_trees();
    let common_offsets = HashMap::new();
    let mut builder = SerializedFileBuilder::new(&source(), &trees, &common_offsets, true);
    let mut types = HashMap::new();
    let mut add = |path: i64, class: ClassId, tree: &TypeTreeNode, value: &Value| {
        let ty = *types.entry(class).or_insert_with(|| {
            builder.add_type_uncached(SerializedType {
                m_ClassID: class,
                m_OldTypeHash: tree.hash(),
                m_Type: Some(tree.clone()),
                ..Default::default()
            })
        });
        let bytes = serde_typetree::to_vec::<_, byteorder::LittleEndian>(value, tree).unwrap();
        builder
            .add_object_untyped_with(path, class, ty, bytes.into())
            .unwrap();
    };
    // two classes the provider has no tree of, their entries before Font's
    let mesh = node("Mesh", "Base", vec![node("SInt32", "m_X", vec![])]);
    let clip = node("AudioClip", "Base", vec![node("SInt32", "m_Y", vec![])]);
    add(1, ClassId::Mesh, &mesh, &map([("m_X", Value::I32(1))]));
    add(2, ClassId::AudioClip, &clip, &map([("m_Y", Value::I32(2))]));
    // Font in the source's form, which the target's tree reads differently
    let font = map([
        ("m_A", Value::I32(7)),
        ("m_B", Value::String("seven".to_owned())),
        ("m_OnlyInSource", Value::I32(9)),
    ]);
    add(3, ClassId::Font, &trees.with_source_field, &font);
    // a Mesh after it, so its entry is not empty after the first removal
    add(4, ClassId::Mesh, &mesh, &map([("m_X", Value::I32(3))]));
    let data = builder.write_vec().unwrap();

    let retarget = Retarget {
        to: target(),
        typetrees: &trees,
        common_strings: HashMap::new(),
        shader_chunk_size: 8 << 20,
        source_version: None,
        scripts: None,
    };
    let (retargeted, report) = retarget.serialized_file_reported(&data).unwrap();
    assert_eq!(
        report.removed,
        [("AudioClip".to_owned(), 1usize), ("Mesh".to_owned(), 2)]
            .into_iter()
            .collect()
    );
    assert_eq!(report.reencoded, 1);

    let data = retargeted.unwrap();
    let file = SerializedFile::from_reader(&mut std::io::Cursor::new(data.as_slice())).unwrap();
    let [ty] = file.m_Types.as_slice() else {
        panic!(
            "{} type entries, only Font's should be left",
            file.m_Types.len()
        );
    };
    assert_eq!(ty.m_ClassID, ClassId::Font);
    // the target's fields (reading the blob back fills in `m_RefTypeHash`)
    let fields: Vec<_> = ty
        .m_Type
        .as_ref()
        .unwrap()
        .children
        .iter()
        .map(|c| c.m_Name.as_str())
        .collect();
    assert_eq!(fields, ["m_A", "m_B"]);
    assert_eq!(ty.m_OldTypeHash, trees.with_target_field.hash());
    let objects: Vec<_> = file.objects().collect();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].m_TypeID, 0);
    let read: Value = file
        .read(
            objects[0],
            &trees,
            &mut std::io::Cursor::new(data.as_slice()),
        )
        .unwrap();
    assert_eq!(
        read,
        map([
            ("m_A", Value::I32(7)),
            ("m_B", Value::String("seven".to_owned())),
        ])
    );
}
