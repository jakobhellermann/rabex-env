//! The transcoder against the writer: what it makes of an object's bytes is what the
//! writer writes for the expected value against the target tree. The field rules are
//! the ones of `fields.rs`, each kind once.

use std::collections::BTreeMap;

use byteorder::LittleEndian;
use rabex::typetree::TypeTreeNode;
use rabex::{UnityVersion, serde_typetree};
use rabex_convert::transcode::Transcoder;
use rabex_convert::value::Value;

fn to() -> UnityVersion {
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

fn leaf(ty: &str, name: &str) -> TypeTreeNode {
    node(ty, name, vec![])
}

/// `node` with the data after it padded to four bytes.
fn aligned(mut node: TypeTreeNode) -> TypeTreeNode {
    node.m_MetaFlag = Some(0x4000);
    node
}

fn string(name: &str) -> TypeTreeNode {
    node(
        "string",
        name,
        vec![aligned(node(
            "Array",
            "Array",
            vec![leaf("int", "size"), leaf("char", "data")],
        ))],
    )
}

fn vector(name: &str, element: TypeTreeNode) -> TypeTreeNode {
    node(
        "vector",
        name,
        vec![node("Array", "Array", vec![leaf("int", "size"), element])],
    )
}

fn class(ty: &str, fields: Vec<TypeTreeNode>) -> TypeTreeNode {
    node(ty, "Base", fields)
}

fn map(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Value::String(name.to_owned()), value))
            .collect(),
    )
}

fn encode(value: &Value, tree: &TypeTreeNode) -> Vec<u8> {
    serde_typetree::to_vec::<_, LittleEndian>(value, tree).unwrap()
}

/// Transcode `value`, encoded against `source`, to `target`: the bytes must be what the
/// writer writes for `expected`. Returns the dropped fields by path.
fn check(
    source: &TypeTreeNode,
    target: &TypeTreeNode,
    value: &Value,
    expected: &Value,
) -> BTreeMap<String, usize> {
    let out = transcode(source, target, value);
    assert_eq!(out.0, encode(expected, target), "against the writer");
    out.1
}

/// Transcode `value`, encoded against `source`, to `target`: the bytes and the dropped
/// fields by path.
fn transcode(
    source: &TypeTreeNode,
    target: &TypeTreeNode,
    value: &Value,
) -> (Vec<u8>, BTreeMap<String, usize>) {
    let transcoder = Transcoder::new(source, target, &source.m_Type).expect("a plan");
    let mut counts = vec![0; transcoder.dropped().len()];
    let out = transcoder
        .run(
            &encode(value, source),
            &to(),
            &mut counts,
            &mut BTreeMap::new(),
        )
        .unwrap();
    let dropped = transcoder
        .dropped()
        .iter()
        .cloned()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .collect();
    (out, dropped)
}

#[test]
fn fields_take_the_targets_order() {
    let source = class(
        "Thing",
        vec![leaf("SInt32", "m_A"), string("m_B"), leaf("UInt8", "m_C")],
    );
    let target = class(
        "Thing",
        vec![leaf("UInt8", "m_C"), leaf("SInt32", "m_A"), string("m_B")],
    );
    let value = map([
        ("m_A", Value::I32(-7)),
        ("m_B", Value::String("seven".to_owned())),
        ("m_C", Value::U8(3)),
    ]);
    let expected = map([
        ("m_C", Value::U8(3)),
        ("m_A", Value::I32(-7)),
        ("m_B", Value::String("seven".to_owned())),
    ]);
    check(&source, &target, &value, &expected);
}

#[test]
fn reordered_fields_around_a_dropped_one() {
    let source = class(
        "Thing",
        vec![
            leaf("SInt32", "m_A"),
            string("m_Gone"),
            leaf("UInt8", "m_B"),
        ],
    );
    let target = class("Thing", vec![leaf("UInt8", "m_B"), leaf("SInt32", "m_A")]);
    let value = map([
        ("m_A", Value::I32(1)),
        ("m_Gone", Value::String("x".to_owned())),
        ("m_B", Value::U8(2)),
    ]);
    let expected = map([("m_B", Value::U8(2)), ("m_A", Value::I32(1))]);
    let dropped = check(&source, &target, &value, &expected);
    assert_eq!(dropped, [("Thing.m_Gone".to_owned(), 1)].into());
}

#[test]
fn integers_change_width_and_keep_their_value() {
    let source = class(
        "Thing",
        vec![leaf("SInt16", "m_Signed"), leaf("UInt8", "m_Unsigned")],
    );
    let target = class(
        "Thing",
        vec![leaf("SInt64", "m_Signed"), leaf("UInt32", "m_Unsigned")],
    );
    let value = map([
        ("m_Signed", Value::I16(-300)),
        ("m_Unsigned", Value::U8(200)),
    ]);
    let expected = map([
        ("m_Signed", Value::I64(-300)),
        ("m_Unsigned", Value::U32(200)),
    ]);
    check(&source, &target, &value, &expected);
}

#[test]
fn an_integer_the_target_cannot_hold_fails() {
    let source = class("Thing", vec![leaf("SInt32", "m_N")]);
    let target = class("Thing", vec![leaf("UInt8", "m_N")]);
    let transcoder = Transcoder::new(&source, &target, "Thing").unwrap();
    let errors = [300, -1].map(|n| {
        let bytes = encode(&map([("m_N", Value::I32(n))]), &source);
        let error = transcoder
            .run(&bytes, &to(), &mut [], &mut BTreeMap::new())
            .unwrap_err();
        format!("{error:#}")
    });
    assert_eq!(
        errors,
        [
            "300 does not fit the target's U8",
            "-1 does not fit the target's U8"
        ]
    );
}

/// A dropped byte moves everything after it off the four byte grid until the next
/// padding: the target lays its own padding out where its fields land.
#[test]
fn padding_follows_where_fields_land() {
    let inner = |extra: bool| {
        let mut fields = vec![aligned(leaf("bool", "m_Flag")), leaf("SInt32", "m_N")];
        if extra {
            fields.push(leaf("UInt8", "m_Extra"));
        }
        node("Inner", "m_Inner", fields)
    };
    let source = class(
        "Thing",
        vec![
            leaf("UInt8", "m_Gone"),
            leaf("UInt8", "m_Kept"),
            inner(true),
            string("m_Name"),
            vector("m_Bytes", leaf("UInt8", "data")),
        ],
    );
    let target = class(
        "Thing",
        vec![
            leaf("UInt8", "m_Kept"),
            inner(false),
            string("m_Name"),
            vector("m_Bytes", leaf("UInt8", "data")),
        ],
    );
    let inner_value = |extra: bool| {
        let mut fields = vec![("m_Flag", Value::Bool(true)), ("m_N", Value::I32(5))];
        if extra {
            fields.push(("m_Extra", Value::U8(9)));
        }
        map(fields)
    };
    let bytes = Value::Seq((1..=5).map(Value::U8).collect());
    let value = map([
        ("m_Gone", Value::U8(1)),
        ("m_Kept", Value::U8(2)),
        ("m_Inner", inner_value(true)),
        ("m_Name", Value::String("abc".to_owned())),
        ("m_Bytes", bytes.clone()),
    ]);
    let expected = map([
        ("m_Kept", Value::U8(2)),
        ("m_Inner", inner_value(false)),
        ("m_Name", Value::String("abc".to_owned())),
        ("m_Bytes", bytes),
    ]);
    let dropped = check(&source, &target, &value, &expected);
    assert_eq!(
        dropped,
        [
            ("Thing.m_Gone".to_owned(), 1),
            ("Thing.m_Inner.m_Extra".to_owned(), 1)
        ]
        .into()
    );
}

/// Every element of an array drops the field, counted under one path.
#[test]
fn array_elements_drop_fields_under_one_path() {
    let element = |extra: bool| {
        let mut fields = vec![leaf("SInt32", "x")];
        if extra {
            fields.push(leaf("float", "gone"));
        }
        node("Element", "data", fields)
    };
    let source = class("Thing", vec![vector("m_List", element(true))]);
    let target = class("Thing", vec![vector("m_List", element(false))]);
    let elements = |extra: bool| {
        Value::Seq(
            (0..3)
                .map(|i| {
                    let mut fields = vec![("x", Value::I32(i))];
                    if extra {
                        fields.push(("gone", Value::F32(0.5)));
                    }
                    map(fields)
                })
                .collect(),
        )
    };
    let dropped = check(
        &source,
        &target,
        &map([("m_List", elements(true))]),
        &map([("m_List", elements(false))]),
    );
    assert_eq!(dropped, [("Thing.m_List[].gone".to_owned(), 3)].into());
}

/// Renames from `fields.rs`, both directions: a float under another name, and a flag
/// that became an enum.
#[test]
fn renamed_fields_go_through_their_rule() {
    let new = class(
        "Collider2D",
        vec![
            leaf("float", "m_LinearDamping"),
            leaf("int", "m_CompositeOperation"),
            leaf("SInt32", "m_After"),
        ],
    );
    let old = class(
        "Collider2D",
        vec![
            leaf("float", "m_LinearDrag"),
            aligned(leaf("bool", "m_UsedByComposite")),
            leaf("SInt32", "m_After"),
        ],
    );
    // down: the merge operation is the flag
    check(
        &new,
        &old,
        &map([
            ("m_LinearDamping", Value::F32(0.25)),
            ("m_CompositeOperation", Value::I32(1)),
            ("m_After", Value::I32(7)),
        ]),
        &map([
            ("m_LinearDrag", Value::F32(0.25)),
            ("m_UsedByComposite", Value::Bool(true)),
            ("m_After", Value::I32(7)),
        ]),
    );
    // up: the flag is the merge operation
    check(
        &old,
        &new,
        &map([
            ("m_LinearDrag", Value::F32(0.5)),
            ("m_UsedByComposite", Value::Bool(true)),
            ("m_After", Value::I32(7)),
        ]),
        &map([
            ("m_LinearDamping", Value::F32(0.5)),
            ("m_CompositeOperation", Value::I32(1)),
            ("m_After", Value::I32(7)),
        ]),
    );
}

/// A composite operation the older flag has no spelling for fails rather than pass as
/// "not used by a composite": Intersect is neither.
#[test]
fn an_enum_variant_the_flag_cannot_say_fails() {
    let new = class("Collider2D", vec![leaf("int", "m_CompositeOperation")]);
    let old = class("Collider2D", vec![leaf("bool", "m_UsedByComposite")]);
    let transcoder = Transcoder::new(&new, &old, "Collider2D").unwrap();
    let errors = [2, 3].map(|mode| {
        let bytes = encode(&map([("m_CompositeOperation", Value::I32(mode))]), &new);
        let error = transcoder
            .run(&bytes, &to(), &mut [], &mut BTreeMap::new())
            .unwrap_err();
        format!("{error:#}")
    });
    assert_eq!(
        errors,
        [
            "the rule for m_UsedByComposite: m_CompositeOperation is 2, which m_UsedByComposite cannot say",
            "the rule for m_UsedByComposite: m_CompositeOperation is 3, which m_UsedByComposite cannot say"
        ]
    );
    for (mode, flag) in [(0, false), (1, true)] {
        check(
            &new,
            &old,
            &map([("m_CompositeOperation", Value::I32(mode))]),
            &map([("m_UsedByComposite", Value::Bool(flag))]),
        );
    }
}

/// A mode the older flag has no spelling for fails rather than pick a side: stepping
/// physics in Update has no automatic-stepping flag.
#[test]
fn a_value_the_rule_cannot_say_fails() {
    let new = class("PhysicsManager", vec![leaf("int", "m_SimulationMode")]);
    let old = class("PhysicsManager", vec![leaf("bool", "m_AutoSimulation")]);
    let transcoder = Transcoder::new(&new, &old, "PhysicsManager").unwrap();
    let bytes = encode(&map([("m_SimulationMode", Value::I32(1))]), &new);
    let error = transcoder
        .run(&bytes, &to(), &mut [], &mut BTreeMap::new())
        .unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "the rule for m_AutoSimulation: m_SimulationMode is 1, which m_AutoSimulation cannot say"
    );
    // the two it has
    for (mode, flag) in [(0, true), (2, false)] {
        check(
            &new,
            &old,
            &map([("m_SimulationMode", Value::I32(mode))]),
            &map([("m_AutoSimulation", Value::Bool(flag))]),
        );
    }
}

/// What neither the trees nor a rule say fails when the plan is made, naming the field.
#[test]
fn a_plan_fails_where_nothing_says_how() {
    let thing = |fields| class("Thing", fields);
    let fails = |source: TypeTreeNode, target: TypeTreeNode, says: &str| {
        let error = Transcoder::new(&source, &target, "Thing")
            .err()
            .expect("no plan");
        assert_eq!(format!("{error:#}"), says);
    };
    // a field the target wants that the source lacks
    fails(
        thing(vec![leaf("SInt32", "m_A")]),
        thing(vec![leaf("SInt32", "m_A"), leaf("SInt32", "m_New")]),
        "Thing.m_New: the target wants it, the source lacks it, and no rule provides it",
    );
    // a struct where the target keeps a plain value
    fails(
        thing(vec![node(
            "Pair",
            "m_A",
            vec![leaf("SInt32", "x"), leaf("SInt32", "y")],
        )]),
        thing(vec![leaf("SInt32", "m_A")]),
        "Thing.m_A: a Pair (Struct) in the source, a SInt32 (I32) in the target, and no rule converting between them",
    );
    // a number that becomes a string
    fails(
        thing(vec![leaf("SInt32", "m_A")]),
        thing(vec![string("m_A")]),
        "Thing.m_A: a SInt32 (I32) in the source, a string (String) in the target, and no rule converting between them",
    );
    // a float that would become an integer
    fails(
        thing(vec![leaf("float", "m_A")]),
        thing(vec![leaf("SInt32", "m_A")]),
        "Thing.m_A: a float (Float) in the source, a SInt32 (I32) in the target, and no rule converting between them",
    );
}

/// A field Unity 6 dropped gets the value the older version's builds write.
#[test]
fn a_dropped_field_comes_back_with_its_value() {
    let new = class(
        "LightingSettings",
        vec![leaf("SInt32", "m_Before"), leaf("SInt32", "m_After")],
    );
    let old = class(
        "LightingSettings",
        vec![
            leaf("SInt32", "m_Before"),
            leaf("int", "m_GIWorkflowMode"),
            leaf("SInt32", "m_After"),
        ],
    );
    check(
        &new,
        &old,
        &map([("m_Before", Value::I32(3)), ("m_After", Value::I32(4))]),
        &map([
            ("m_Before", Value::I32(3)),
            ("m_GIWorkflowMode", Value::I32(1)),
            ("m_After", Value::I32(4)),
        ]),
    );
}

/// BuildSettings: the build's identity made from its token, a release build, and the
/// version it is written for.
#[test]
fn build_settings_follow_the_target() {
    let guid = node(
        "GUID",
        "buildGUID",
        (0..4)
            .map(|i| leaf("unsigned int", &format!("data[{i}]")))
            .collect(),
    );
    let new = class(
        "BuildSettings",
        vec![string("m_Version"), string("m_AuthToken")],
    );
    let old = class(
        "BuildSettings",
        vec![
            string("m_Version"),
            guid,
            string("m_AuthToken"),
            leaf("bool", "isDebugBuild"),
        ],
    );
    let settings = |token: &str| {
        map([
            ("m_Version", Value::String("6000.0.50f1".to_owned())),
            ("m_AuthToken", Value::String(token.to_owned())),
        ])
    };
    let (out, _) = transcode(&new, &old, &settings("a token"));
    let read: Value = serde_typetree::from_slice::<_, LittleEndian>(&out, &old).unwrap();
    let field = |name| rabex_convert::value::field(&read, name).cloned();
    assert_eq!(field("m_Version"), Some(Value::String(to().to_string())));
    assert_eq!(field("isDebugBuild"), Some(Value::Bool(false)));
    assert_eq!(
        field("m_AuthToken"),
        Some(Value::String("a token".to_owned()))
    );
    // the same build, the same GUID; another build, another
    assert_eq!(out, transcode(&new, &old, &settings("a token")).0);
    assert_ne!(field("buildGUID"), {
        let (other, _) = transcode(&new, &old, &settings("another token"));
        let read: Value = serde_typetree::from_slice::<_, LittleEndian>(&other, &old).unwrap();
        rabex_convert::value::field(&read, "buildGUID").cloned()
    });
}

/// TimeManager's fixed step: a rational time in Unity 6, seconds before.
#[test]
fn the_fixed_step_becomes_seconds() {
    let rate = node(
        "RationalRate",
        "m_Rate",
        vec![
            leaf("unsigned int", "m_Denominator"),
            leaf("unsigned int", "m_Numerator"),
        ],
    );
    let new = class(
        "TimeManager",
        vec![node(
            "RationalTime",
            "Fixed Timestep",
            vec![leaf("SInt64", "m_Count"), rate],
        )],
    );
    let old = class("TimeManager", vec![leaf("float", "Fixed Timestep")]);
    let (out, dropped) = transcode(
        &new,
        &old,
        &map([(
            "Fixed Timestep",
            map([
                ("m_Count", Value::I64(2_822_399)),
                (
                    "m_Rate",
                    map([
                        ("m_Denominator", Value::U32(1)),
                        ("m_Numerator", Value::U32(141_120_000)),
                    ]),
                ),
            ]),
        )]),
    );
    let seconds = f32::from_le_bytes(out.try_into().unwrap());
    assert!((seconds - 0.02).abs() < 1e-6, "{seconds}");
    assert!(dropped.is_empty(), "{dropped:?}");
}

/// And back: Hollow Knight's 0.02 s of 2020.2 as its upgraded 6000.0 build stores it.
#[test]
fn seconds_become_the_fixed_step_hollow_knight_upgraded_to() {
    let rate = node(
        "RationalRate",
        "m_Rate",
        vec![
            leaf("unsigned int", "m_Denominator"),
            leaf("unsigned int", "m_Numerator"),
        ],
    );
    let new = class(
        "TimeManager",
        vec![node(
            "RationalTime",
            "Fixed Timestep",
            vec![leaf("SInt64", "m_Count"), rate],
        )],
    );
    let old = class("TimeManager", vec![leaf("float", "Fixed Timestep")]);
    check(
        &old,
        &new,
        &map([("Fixed Timestep", Value::F32(0.02))]),
        &map([(
            "Fixed Timestep",
            map([
                ("m_Count", Value::I64(2_822_399)),
                (
                    "m_Rate",
                    map([
                        ("m_Denominator", Value::U32(1)),
                        ("m_Numerator", Value::U32(141_120_000)),
                    ]),
                ),
            ]),
        )]),
    );
}

/// A streamed clip's discrete curves fold into its curve count where the target counts
/// them all in one, deep inside the clip; where it counts them apart they carry over.
#[test]
fn discrete_curves_fold_into_the_curve_count() {
    let streamed = |split: bool| {
        let mut fields = vec![leaf(
            if split { "UInt16" } else { "unsigned int" },
            "curveCount",
        )];
        if split {
            fields.push(leaf("UInt16", "discreteCurveCount"));
        }
        let clip = node("StreamedClip", "m_StreamedClip", fields);
        class("AnimationClip", vec![node("Clip", "m_Clip", vec![clip])])
    };
    let clip = |fields: Vec<(&'static str, Value)>| {
        map([("m_Clip", map([("m_StreamedClip", map(fields))]))])
    };
    let dropped = check(
        &streamed(true),
        &streamed(false),
        &clip(vec![
            ("curveCount", Value::U16(5)),
            ("discreteCurveCount", Value::U16(2)),
        ]),
        &clip(vec![("curveCount", Value::U32(7))]),
    );
    // the discrete count lives on in the folded one: nothing is dropped
    assert!(dropped.is_empty(), "{dropped:?}");
    let split = clip(vec![
        ("curveCount", Value::U16(5)),
        ("discreteCurveCount", Value::U16(2)),
    ]);
    check(&streamed(true), &streamed(true), &split, &split);
}

/// A mixer's groups get send slots past the slots the snapshots hold, which grow by one
/// silent value per group.
#[test]
fn mixer_groups_get_send_slots_at_the_end() {
    let mixer = |sends: bool| {
        let mut group = vec![leaf("unsigned int", "volumeIndex")];
        if sends {
            group.push(leaf("unsigned int", "sendIndex"));
        }
        let snapshot = node(
            "SnapshotConstant",
            "data",
            vec![
                leaf("unsigned int", "nameHash"),
                vector("values", leaf("float", "data")),
            ],
        );
        class(
            "AudioMixerController",
            vec![node(
                "AudioMixerConstant",
                "m_MixerConstant",
                vec![
                    vector("groups", node("GroupConstant", "data", group)),
                    vector("snapshots", snapshot),
                ],
            )],
        )
    };
    let constant = |groups: Vec<Value>, snapshot_values: Vec<f32>| {
        let snapshot = |hash| {
            map([
                ("nameHash", Value::U32(hash)),
                (
                    "values",
                    Value::Seq(snapshot_values.iter().map(|v| Value::F32(*v)).collect()),
                ),
            ])
        };
        map([(
            "m_MixerConstant",
            map([
                ("groups", Value::Seq(groups)),
                ("snapshots", Value::Seq(vec![snapshot(1), snapshot(2)])),
            ]),
        )])
    };
    let group = |volume: u32, send: Option<u32>| {
        let mut fields = vec![("volumeIndex", Value::U32(volume))];
        fields.extend(send.map(|send| ("sendIndex", Value::U32(send))));
        map(fields)
    };
    check(
        &mixer(false),
        &mixer(true),
        &constant(vec![group(0, None), group(1, None)], vec![0.0, 0.5, 1.0]),
        &constant(
            vec![group(0, Some(3)), group(1, Some(4))],
            vec![0.0, 0.5, 1.0, -80.0, -80.0],
        ),
    );
}

/// Map entries keep every pair, a key repeated included, with their values rebuilt.
#[test]
fn map_values_change_and_every_pair_stays() {
    let entry = |extra: bool| {
        let mut fields = vec![leaf("SInt32", "x")];
        if extra {
            fields.push(leaf("SInt32", "gone"));
        }
        node("Entry", "second", fields)
    };
    let table = |extra: bool| {
        let pair = node("pair", "data", vec![string("first"), entry(extra)]);
        class(
            "Thing",
            vec![node(
                "map",
                "m_Table",
                vec![node("Array", "Array", vec![leaf("int", "size"), pair])],
            )],
        )
    };
    // the bytes of a map whose key repeats: written by hand, a map value cannot hold it
    let pair = |key: &str, x: i32, gone: Option<i32>| {
        let mut bytes = (key.len() as i32).to_le_bytes().to_vec();
        bytes.extend(key.as_bytes());
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        bytes.extend(x.to_le_bytes());
        bytes.extend(gone.map(i32::to_le_bytes).into_iter().flatten());
        bytes
    };
    let source = [
        2i32.to_le_bytes().to_vec(),
        pair("k", 1, Some(9)),
        pair("k", 2, Some(9)),
    ]
    .concat();
    let expected = [
        2i32.to_le_bytes().to_vec(),
        pair("k", 1, None),
        pair("k", 2, None),
    ]
    .concat();
    let (from, to_tree) = (table(true), table(false));
    let transcoder = Transcoder::new(&from, &to_tree, "Thing").unwrap();
    let mut counts = vec![0; transcoder.dropped().len()];
    assert_eq!(
        transcoder
            .run(&source, &to(), &mut counts, &mut BTreeMap::new())
            .unwrap(),
        expected
    );
    assert_eq!(transcoder.dropped(), ["Thing.m_Table[].second.gone"]);
    assert_eq!(counts, [2]);
}

#[test]
fn bytes_the_tree_does_not_account_for_fail() {
    let source = class(
        "Thing",
        vec![leaf("SInt32", "m_A"), leaf("UInt8", "m_Gone")],
    );
    let target = class("Thing", vec![leaf("SInt32", "m_A")]);
    let transcoder = Transcoder::new(&source, &target, "Thing").unwrap();
    let mut bytes = encode(
        &map([("m_A", Value::I32(1)), ("m_Gone", Value::U8(2))]),
        &source,
    );
    let mut counts = vec![0; transcoder.dropped().len()];
    // one short and one over
    let short = transcoder
        .run(&bytes[..4], &to(), &mut counts, &mut BTreeMap::new())
        .unwrap_err();
    assert_eq!(format!("{short:#}"), "the object ends inside a field");
    bytes.push(0);
    let over = transcoder
        .run(&bytes, &to(), &mut counts, &mut BTreeMap::new())
        .unwrap_err();
    assert_eq!(
        format!("{over:#}"),
        "the tree accounts for 5 of the object's 6 bytes"
    );
}

/// Unity 6 split AnimatePhysics into the mode Fixed and m_AnimatePhysics. Going down, the
/// mode stays; where the two disagree the old form cannot say it, and the retarget warns
/// and goes on.
#[test]
fn an_animator_the_old_mode_cannot_say_warns() {
    let new = class(
        "Animator",
        vec![
            leaf("int", "m_UpdateMode"),
            aligned(leaf("bool", "m_AnimatePhysics")),
        ],
    );
    let old = class("Animator", vec![leaf("int", "m_UpdateMode")]);
    let transcoder = Transcoder::new(&new, &old, "Animator").unwrap();
    let run = |mode: i32, sync: bool| {
        let bytes = encode(
            &map([
                ("m_UpdateMode", Value::I32(mode)),
                ("m_AnimatePhysics", Value::Bool(sync)),
            ]),
            &new,
        );
        let mut warnings = BTreeMap::new();
        let mut counts = vec![0; transcoder.dropped().len()];
        let out = transcoder
            .run(&bytes, &to(), &mut counts, &mut warnings)
            .unwrap();
        (i32::from_le_bytes(out.try_into().unwrap()), warnings.len())
    };
    assert_eq!(run(1, true), (1, 0));
    assert_eq!(run(0, false), (0, 0));
    assert_eq!(run(1, false), (1, 1), "Fixed without the sync");
    assert_eq!(run(0, true), (0, 1), "the sync without Fixed");

    // and up: the sync is what mode 1 did
    let up = Transcoder::new(&old, &new, "Animator").unwrap();
    for (mode, sync) in [(0, false), (1, true), (2, false)] {
        let bytes = encode(&map([("m_UpdateMode", Value::I32(mode))]), &old);
        let out = up
            .run(&bytes, &to(), &mut [], &mut BTreeMap::new())
            .unwrap();
        assert_eq!(
            out,
            encode(
                &map([
                    ("m_UpdateMode", Value::I32(mode)),
                    ("m_AnimatePhysics", Value::Bool(sync))
                ]),
                &new
            )
        );
    }
}

/// A spot light's shape and its type, folded together in Unity 6, both ways.
#[test]
fn a_spot_lights_shape_becomes_its_type() {
    let new = class("Light", vec![leaf("int", "m_Type")]);
    let old = class("Light", vec![leaf("int", "m_Type"), leaf("int", "m_Shape")]);
    for (kind, shape, folded) in [(0, 0, 0), (0, 1, 5), (0, 2, 6), (2, 0, 2), (1, 0, 1)] {
        check(
            &old,
            &new,
            &map([("m_Type", Value::I32(kind)), ("m_Shape", Value::I32(shape))]),
            &map([("m_Type", Value::I32(folded))]),
        );
        check(
            &new,
            &old,
            &map([("m_Type", Value::I32(folded))]),
            &map([("m_Type", Value::I32(kind)), ("m_Shape", Value::I32(shape))]),
        );
    }
}
