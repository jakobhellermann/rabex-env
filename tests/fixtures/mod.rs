//! In-memory Unity SerializedFile fixtures for tests.
//!
//! `SerializedFileBuilder` assembles tiny scenes (GameObjects + Transforms)
//! into a `Vec<u8>` without touching disk. `MemResolver` then re-opens those
//! bytes through a real rabex `Environment`, so the production load/read code
//! runs against a genuine `SerializedFileHandle`.
#![allow(dead_code)]

use std::io::Cursor;

use rabex_env::Environment;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::rabex::UnityVersion;
use rabex_env::rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex_env::rabex::files::serializedfile::{
    LocalSerializedObjectIdentifier, SerializedType, build_common_offset_map,
};
use rabex_env::rabex::objects::pptr::{FileId, PathId};
use rabex_env::rabex::objects::{ClassId, PPtr, TypedPPtr};
use rabex_env::rabex::tpk::TpkTypeTreeBlob;
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::resolver::MemResolver;
use rabex_env::unity::types::{
    ComponentPair, GameObject, MonoBehaviour, MonoScript, PreloadData, Transform,
};

/// Unity version every fixture is built with. The embedded TPK has full
/// coverage for it.
pub const TEST_UNITY_VERSION: &str = "2022.3.0f1";

/// Open scene bytes via a fresh `Environment` and hand the resulting handle to
/// `f`. Closure-shaped so the env outlives the handle.
pub fn with_handle<R>(
    path: &str,
    bytes: Vec<u8>,
    f: impl FnOnce(&SerializedFileHandle<'_, MemResolver, TypeTreeCache<TpkTypeTreeBlob>>) -> R,
) -> R {
    let resolver = MemResolver::single(path, bytes);
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new(resolver, tpk);
    let handle = env.load_serialized(path).unwrap();
    f(&handle)
}

/// A flat scene: one GameObject + Transform per name, written in order.
/// Returns the bytes plus the path ids assigned to each GameObject so tests
/// can assert against known ids.
pub struct Flat {
    names: Vec<&'static str>,
}

impl Flat {
    pub fn new(names: &[&'static str]) -> Self {
        Flat {
            names: names.to_vec(),
        }
    }

    /// Build the file. Returns `(bytes, gameobject_path_ids)`.
    pub fn write(&self) -> (Vec<u8>, Vec<PathId>) {
        let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
        let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
        let common = build_common_offset_map(&tpk.inner, &unity_version);
        let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);

        let mut go_ids = Vec::new();
        for name in &self.names {
            let go_id = sfb.get_next_path_id();
            let transform_id = sfb.get_next_path_id();

            let go = GameObject {
                m_Component: vec![ComponentPair {
                    component: PPtr::local(transform_id),
                }],
                m_Layer: 0,
                m_Name: (*name).to_owned(),
                m_Tag: 0,
                m_IsActive: true,
            };
            sfb.add_object_at(go_id, &go).unwrap();

            let transform = Transform {
                m_GameObject: TypedPPtr::local(go_id),
                m_LocalRotation: (0.0, 0.0, 0.0, 1.0),
                m_LocalPosition: (0.0, 0.0, 0.0),
                m_LocalScale: (1.0, 1.0, 1.0),
                m_Children: Vec::new(),
                m_Father: TypedPPtr::null(),
            };
            sfb.add_object_at(transform_id, &transform).unwrap();

            go_ids.push(go_id);
        }

        (sfb.write_vec().unwrap(), go_ids)
    }
}

/// A flat scene (one GameObject + Transform) plus a `PreloadData` whose
/// `m_Assets` references that GameObject. Exercises preload-reference filtering:
/// the GameObject is referenced both by its Transform (a real user) and by the
/// PreloadData (a load-time dependency). Returns `(bytes, gameobject_path_id)`.
pub fn scene_with_preload(name: &str) -> (Vec<u8>, PathId) {
    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &unity_version);
    let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);

    let go_id = sfb.get_next_path_id();
    let transform_id = sfb.get_next_path_id();
    let go = GameObject {
        m_Component: vec![ComponentPair {
            component: PPtr::local(transform_id),
        }],
        m_Layer: 0,
        m_Name: name.to_owned(),
        m_Tag: 0,
        m_IsActive: true,
    };
    sfb.add_object_at(go_id, &go).unwrap();
    let transform = Transform {
        m_GameObject: TypedPPtr::local(go_id),
        m_LocalRotation: (0.0, 0.0, 0.0, 1.0),
        m_LocalPosition: (0.0, 0.0, 0.0),
        m_LocalScale: (1.0, 1.0, 1.0),
        m_Children: Vec::new(),
        m_Father: TypedPPtr::null(),
    };
    sfb.add_object_at(transform_id, &transform).unwrap();

    let preload_id = sfb.get_next_path_id();
    let preload = PreloadData {
        m_Name: "preload".to_owned(),
        m_Assets: vec![PPtr::local(go_id)],
        m_Dependencies: Vec::new(),
        m_ExplicitDataLayout: false,
    };
    sfb.add_object_at(preload_id, &preload).unwrap();

    (sfb.write_vec().unwrap(), go_id)
}

/// A serialized file containing one `MonoScript` per class name (no namespace,
/// so each script's `full_name` is just the class name).
pub fn scripts_file(class_names: &[&str]) -> Vec<u8> {
    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &unity_version);
    let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);

    for class_name in class_names {
        let id = sfb.get_next_path_id();
        let script = MonoScript {
            m_Name: (*class_name).to_owned(),
            m_ExecutionOrder: 0,
            m_PropertiesHash: [0; 16],
            m_ClassName: (*class_name).to_owned(),
            m_Namespace: String::new(),
            m_AssemblyName: "Assembly-CSharp.dll".to_owned(),
        };
        sfb.add_object_at(id, &script).unwrap();
    }

    sfb.write_vec().unwrap()
}

/// A GameObject (`go_name`) with a Transform and a MonoBehaviour whose script
/// class is `script_class`, plus the `MonoScript` it points at. Wires the
/// script-type metadata (a `SerializedType` with `m_ScriptTypeIndex` into
/// `m_ScriptTypes`) so `mono_script()` resolves the behaviour to its script.
/// Returns `(bytes, gameobject_path_id, monobehaviour_path_id)`.
pub fn scene_with_script_component(go_name: &str, script_class: &str) -> (Vec<u8>, PathId, PathId) {
    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &unity_version);
    let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);

    let script_id = sfb.get_next_path_id();
    let script = MonoScript {
        m_Name: script_class.to_owned(),
        m_ExecutionOrder: 0,
        m_PropertiesHash: [0; 16],
        m_ClassName: script_class.to_owned(),
        m_Namespace: String::new(),
        m_AssemblyName: "Assembly-CSharp.dll".to_owned(),
    };
    sfb.add_object_at(script_id, &script).unwrap();

    let go_id = sfb.get_next_path_id();
    let transform_id = sfb.get_next_path_id();
    let mb_id = sfb.get_next_path_id();

    let go = GameObject {
        m_Component: vec![
            ComponentPair {
                component: PPtr::local(transform_id),
            },
            ComponentPair {
                component: PPtr::local(mb_id),
            },
        ],
        m_Layer: 0,
        m_Name: go_name.to_owned(),
        m_Tag: 0,
        m_IsActive: true,
    };
    sfb.add_object_at(go_id, &go).unwrap();

    let transform = Transform {
        m_GameObject: TypedPPtr::local(go_id),
        m_LocalRotation: (0.0, 0.0, 0.0, 1.0),
        m_LocalPosition: (0.0, 0.0, 0.0),
        m_LocalScale: (1.0, 1.0, 1.0),
        m_Children: Vec::new(),
        m_Father: TypedPPtr::null(),
    };
    sfb.add_object_at(transform_id, &transform).unwrap();

    // The MonoBehaviour can't go through `add_object_at` (its `get_or_insert_type`
    // refuses MonoBehaviour). Add a dedicated `SerializedType` whose
    // `m_ScriptTypeIndex` selects an `m_ScriptTypes` entry pointing at the script,
    // then attach the object to that type.
    let mut mb_tt = sfb
        .typetree_provider
        .get_typetree_node(ClassId::MonoBehaviour, &unity_version)
        .expect("MonoBehaviour typetree")
        .into_owned();
    // name the root after the script class so readers use this embedded typetree instead of
    // regenerating it from the script assembly (which an in-memory fixture has no access to)
    mb_tt.m_Type = script_class.to_owned();
    let mut mb_type = SerializedType::simple(ClassId::MonoBehaviour, Some(mb_tt));
    mb_type.m_ScriptTypeIndex = 0;
    let type_id = sfb.add_type_uncached(mb_type);
    sfb.serialized
        .m_ScriptTypes
        .as_mut()
        .unwrap()
        .push(LocalSerializedObjectIdentifier {
            m_LocalSerializedFileIndex: FileId::LOCAL,
            m_LocalIdentifierInFile: script_id,
        });

    let mb = MonoBehaviour {
        m_GameObject: TypedPPtr::local(go_id),
        m_Enabled: 1,
        m_Script: TypedPPtr::local(script_id),
        m_Name: String::new(),
    };
    sfb.add_object_with(&mb, mb_id, ClassId::MonoBehaviour, type_id)
        .unwrap();

    (sfb.write_vec().unwrap(), go_id, mb_id)
}

/// Path ids of the nodes built by [`tree`].
pub struct Tree {
    pub root: PathId,
    pub child: PathId,
    pub leaf: PathId,
    /// MonoBehaviour (`script_class`) attached to `leaf`
    pub leaf_mb: PathId,
    /// two sibling GameObjects both named "Dup" under `root`, for `:index` disambiguation
    pub dup0: PathId,
    pub dup1: PathId,
    /// the MonoScript asset (no GameObject) — a loose object that resolves to no path
    pub script: PathId,
}

/// A nested scene exercising hierarchy paths and `:index` disambiguation:
/// ```text
/// Root
///  ├─ Child
///  │   └─ Leaf      (+ a MonoBehaviour `script_class`)
///  ├─ Dup
///  └─ Dup
/// ```
pub fn tree(script_class: &str) -> (Vec<u8>, Tree) {
    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &unity_version);
    let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);

    // pre-allocate ids so transforms can reference each other
    let script_id = sfb.get_next_path_id();
    let (root_go, root_tf) = (sfb.get_next_path_id(), sfb.get_next_path_id());
    let (child_go, child_tf) = (sfb.get_next_path_id(), sfb.get_next_path_id());
    let (leaf_go, leaf_tf, leaf_mb) = (
        sfb.get_next_path_id(),
        sfb.get_next_path_id(),
        sfb.get_next_path_id(),
    );
    let (dup0_go, dup0_tf) = (sfb.get_next_path_id(), sfb.get_next_path_id());
    let (dup1_go, dup1_tf) = (sfb.get_next_path_id(), sfb.get_next_path_id());

    let script = MonoScript {
        m_Name: script_class.to_owned(),
        m_ExecutionOrder: 0,
        m_PropertiesHash: [0; 16],
        m_ClassName: script_class.to_owned(),
        m_Namespace: String::new(),
        m_AssemblyName: "Assembly-CSharp.dll".to_owned(),
    };
    sfb.add_object_at(script_id, &script).unwrap();

    let mut add_go = |id: PathId, name: &str, components: Vec<PathId>| {
        let go = GameObject {
            m_Component: components
                .into_iter()
                .map(|c| ComponentPair {
                    component: PPtr::local(c),
                })
                .collect(),
            m_Layer: 0,
            m_Name: name.to_owned(),
            m_Tag: 0,
            m_IsActive: true,
        };
        sfb.add_object_at(id, &go).unwrap();
    };
    add_go(root_go, "Root", vec![root_tf]);
    add_go(child_go, "Child", vec![child_tf]);
    add_go(leaf_go, "Leaf", vec![leaf_tf, leaf_mb]);
    add_go(dup0_go, "Dup", vec![dup0_tf]);
    add_go(dup1_go, "Dup", vec![dup1_tf]);

    let mut add_transform = |id: PathId, go: PathId, father: PathId, children: Vec<PathId>| {
        let transform = Transform {
            m_GameObject: TypedPPtr::local(go),
            m_LocalRotation: (0.0, 0.0, 0.0, 1.0),
            m_LocalPosition: (0.0, 0.0, 0.0),
            m_LocalScale: (1.0, 1.0, 1.0),
            m_Children: children.into_iter().map(TypedPPtr::local).collect(),
            m_Father: if father == 0 {
                TypedPPtr::null()
            } else {
                TypedPPtr::local(father)
            },
        };
        sfb.add_object_at(id, &transform).unwrap();
    };
    add_transform(root_tf, root_go, 0, vec![child_tf, dup0_tf, dup1_tf]);
    add_transform(child_tf, child_go, root_tf, vec![leaf_tf]);
    add_transform(leaf_tf, leaf_go, child_tf, vec![]);
    add_transform(dup0_tf, dup0_go, root_tf, vec![]);
    add_transform(dup1_tf, dup1_go, root_tf, vec![]);

    // MonoBehaviour on Leaf, wired to its script (same dance as scene_with_script_component)
    let mut mb_tt = sfb
        .typetree_provider
        .get_typetree_node(ClassId::MonoBehaviour, &unity_version)
        .expect("MonoBehaviour typetree")
        .into_owned();
    // name the root after the script class so readers use this embedded typetree instead of
    // regenerating it from the script assembly (which an in-memory fixture has no access to)
    mb_tt.m_Type = script_class.to_owned();
    let mut mb_type = SerializedType::simple(ClassId::MonoBehaviour, Some(mb_tt));
    mb_type.m_ScriptTypeIndex = 0;
    let type_id = sfb.add_type_uncached(mb_type);
    sfb.serialized
        .m_ScriptTypes
        .as_mut()
        .unwrap()
        .push(LocalSerializedObjectIdentifier {
            m_LocalSerializedFileIndex: FileId::LOCAL,
            m_LocalIdentifierInFile: script_id,
        });
    let mb = MonoBehaviour {
        m_GameObject: TypedPPtr::local(leaf_go),
        m_Enabled: 1,
        m_Script: TypedPPtr::local(script_id),
        m_Name: String::new(),
    };
    sfb.add_object_with(&mb, leaf_mb, ClassId::MonoBehaviour, type_id)
        .unwrap();

    let tree = Tree {
        root: root_go,
        child: child_go,
        leaf: leaf_go,
        leaf_mb,
        dup0: dup0_go,
        dup1: dup1_go,
        script: script_id,
    };
    (sfb.write_vec().unwrap(), tree)
}

/// Wrap raw serialized-file bytes into a minimal uncompressed UnityFS bundle
/// holding a single serialized entry named `entry_name`.
pub fn bundle_with_serialized(entry_name: &str, serialized: &[u8]) -> Vec<u8> {
    use rabex_env::rabex::files::bundlefile::CompressionType;
    use rabex_env::rabex::files::bundlefile::builder::BundleFileBuilder;

    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let mut builder = BundleFileBuilder::unityfs(7, &unity_version);
    builder.add_file(entry_name, serialized).unwrap();

    let mut out = Cursor::new(Vec::new());
    builder.write(&mut out, CompressionType::None).unwrap();
    out.into_inner()
}
