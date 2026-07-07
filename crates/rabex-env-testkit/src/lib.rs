//! Shared in-memory Unity `SerializedFile` fixtures for the rabex-env / rabex-cli / scene-repacker
//! test suites.
//!
//! Fixtures assemble tiny scenes (GameObjects + Transforms, MonoBehaviours, …) into a `Vec<u8>`
//! without touching disk, via rabex's [`SerializedFileBuilder`]. [`with_handle`] then re-opens those
//! bytes through a real [`Environment`], so production load/read code runs against a genuine
//! `SerializedFileHandle`.

use std::io::Cursor;

use rabex_env::Environment;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::rabex::UnityVersion;
use rabex_env::rabex::files::serializedfile::build_common_offset_map;
use rabex_env::rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex_env::rabex::objects::pptr::{FileId, PathId};
use rabex_env::rabex::objects::{ClassId, PPtr, TypedPPtr};
use rabex_env::rabex::tpk::TpkTypeTreeBlob;
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::resolver::MemResolver;
use rabex_env::unity::types::{
    ComponentPair, GameObject, MonoBehaviour, MonoScript, PreloadData, Transform,
};

/// Unity version every fixture is built with. The embedded TPK has full coverage for it.
pub const TEST_UNITY_VERSION: &str = "6000.0.0f1";

/// The concrete builder type fixtures work with.
pub type Builder<'a> = SerializedFileBuilder<'a, TypeTreeCache<TpkTypeTreeBlob>>;

/// Assemble a `SerializedFile` through `f` and return its bytes. Owns the version / TPK / offset-map
/// prelude for the duration of the callback so fixtures don't each repeat it.
pub fn build_file(f: impl FnOnce(&mut Builder<'_>)) -> Vec<u8> {
    let unity_version: UnityVersion = TEST_UNITY_VERSION.parse().unwrap();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &unity_version);
    let mut sfb = SerializedFileBuilder::new(&unity_version, &tpk, &common, true);
    f(&mut sfb);
    sfb.write_vec().unwrap()
}

/// Open scene bytes via a fresh [`Environment`] and hand the resulting handle to `f`. Closure-shaped
/// so the env outlives the handle.
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

fn monoscript(class_name: &str) -> MonoScript {
    MonoScript {
        m_Name: class_name.to_owned(),
        m_ExecutionOrder: 0,
        m_PropertiesHash: [0; 16],
        m_ClassName: class_name.to_owned(),
        m_Namespace: String::new(),
        m_AssemblyName: "Assembly-CSharp.dll".to_owned(),
    }
}

/// Add a GameObject at `id` named `name`, listing `components` (component path ids) in
/// `m_Component`.
pub fn add_go(sfb: &mut Builder<'_>, id: PathId, name: &str, components: &[PathId]) {
    let go = GameObject {
        m_Component: components
            .iter()
            .map(|&c| ComponentPair {
                component: PPtr::local(c),
            })
            .collect(),
        m_Layer: 0,
        m_Name: name.to_owned(),
        m_Tag: 0,
        m_IsActive: true,
    };
    sfb.add_object_at(id, &go).unwrap();
}

/// Add a Transform at `id` owned by GameObject `go`, with an optional `father` and `children`
/// (transform path ids).
pub fn add_transform(
    sfb: &mut Builder<'_>,
    id: PathId,
    go: PathId,
    father: Option<PathId>,
    children: &[PathId],
) {
    let transform = Transform {
        m_GameObject: TypedPPtr::local(go),
        m_LocalRotation: (0.0, 0.0, 0.0, 1.0),
        m_LocalPosition: (0.0, 0.0, 0.0),
        m_LocalScale: (1.0, 1.0, 1.0),
        m_Children: children.iter().map(|&c| TypedPPtr::local(c)).collect(),
        m_Father: father.map_or_else(TypedPPtr::null, TypedPPtr::local),
    };
    sfb.add_object_at(id, &transform).unwrap();
}

/// Add a MonoBehaviour on `go_id` instantiating `script_class` — creating the backing `MonoScript`
/// and wiring the script type — at the next free path id. Returns `(monobehaviour, monoscript)`.
pub fn add_scripted_mb(
    sfb: &mut Builder<'_>,
    go_id: PathId,
    script_class: &str,
) -> (PathId, PathId) {
    let script_id = sfb.add_object(&monoscript(script_class)).unwrap();

    // Stamp the embedded MonoBehaviour type tree's root with the script class name so
    // `SerializedFileHandle::read` deserializes straight off it instead of regenerating it from the
    // script assembly (an in-memory fixture has none).
    let mut mb_tt = sfb
        .typetree_provider
        .get_typetree_node(ClassId::MonoBehaviour, sfb.unity_version())
        .expect("embedded TPK has MonoBehaviour")
        .into_owned();
    mb_tt.m_Type = script_class.to_owned();

    let mb_type = sfb.add_monobehaviour_type(PPtr::local(script_id), Some(mb_tt));
    let mb_id = sfb
        .add_monobehaviour_with_type(
            &MonoBehaviour {
                m_GameObject: TypedPPtr::local(go_id),
                m_Enabled: 1,
                m_Script: TypedPPtr::local(script_id),
                m_Name: String::new(),
            },
            mb_type,
        )
        .unwrap();
    (mb_id, script_id)
}

/// A flat scene: one GameObject + Transform per name, written in order. Returns the bytes plus the
/// GameObject path ids so tests can assert against known ids.
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
        let mut go_ids = Vec::new();
        let bytes = build_file(|sfb| {
            for name in &self.names {
                let go_id = sfb.get_next_path_id();
                let transform_id = sfb.get_next_path_id();
                add_go(sfb, go_id, name, &[transform_id]);
                add_transform(sfb, transform_id, go_id, None, &[]);
                go_ids.push(go_id);
            }
        });
        (bytes, go_ids)
    }
}

/// A serialized file containing one `MonoScript` per class name (no namespace, so each script's
/// `full_name` is just the class name).
pub fn scripts_file(class_names: &[&str]) -> Vec<u8> {
    build_file(|sfb| {
        for class_name in class_names {
            sfb.add_object(&monoscript(class_name)).unwrap();
        }
    })
}

/// A flat scene (one GameObject + Transform) plus a `PreloadData` whose `m_Assets` references that
/// GameObject. Returns `(bytes, gameobject_path_id)`.
pub fn scene_with_preload(name: &str) -> (Vec<u8>, PathId) {
    let mut go_id = 0;
    let bytes = build_file(|sfb| {
        let g = sfb.get_next_path_id();
        let transform_id = sfb.get_next_path_id();
        add_go(sfb, g, name, &[transform_id]);
        add_transform(sfb, transform_id, g, None, &[]);
        sfb.add_object(&PreloadData {
            m_Name: "preload".to_owned(),
            m_Assets: vec![PPtr::local(g)],
            m_Dependencies: Vec::new(),
            m_ExplicitDataLayout: false,
        })
        .unwrap();
        go_id = g;
    });
    (bytes, go_id)
}

/// A GameObject (`go_name`) with a Transform and a MonoBehaviour whose script class is
/// `script_class`, plus the `MonoScript` it points at. Returns `(bytes, gameobject, monobehaviour)`.
pub fn scene_with_script_component(go_name: &str, script_class: &str) -> (Vec<u8>, PathId, PathId) {
    let (mut go_id, mut mb_id) = (0, 0);
    let bytes = build_file(|sfb| {
        let g = sfb.get_next_path_id();
        let transform_id = sfb.get_next_path_id();
        // Create the behaviour first (it only needs the reserved GameObject id), then the
        // GameObject can list it in m_Component.
        let (mb, _script) = add_scripted_mb(sfb, g, script_class);
        add_go(sfb, g, go_name, &[transform_id, mb]);
        add_transform(sfb, transform_id, g, None, &[]);
        go_id = g;
        mb_id = mb;
    });
    (bytes, go_id, mb_id)
}

/// A serialized file holding a single named loose asset a(a `MonoScript`, which has an `m_Name` but
/// no GameObject — so it resolves to a name, not a path). Returns `(bytes, asset_path_id)`. Used as
/// the *external* file in cross-file resolution tests.
pub fn named_asset_file(name: &str) -> (Vec<u8>, PathId) {
    let mut id = 0;
    let bytes = build_file(|sfb| {
        id = sfb.add_object(&monoscript(name)).unwrap();
    });
    (bytes, id)
}

/// A minimal local file whose `m_Externals` references `external_path`. Returns `(bytes, file_id)`
/// where `file_id` is the `m_FileID` to use in a `PPtr` pointing into that external file.
pub fn file_referencing_external(external_path: &str) -> (Vec<u8>, FileId) {
    let mut file_id = FileId::LOCAL;
    let bytes = build_file(|sfb| {
        file_id = sfb.get_or_insert_external(external_path);
        // one trivial GameObject so the file isn't degenerate
        let go_id = sfb.get_next_path_id();
        add_go(sfb, go_id, "Root", &[]);
    });
    (bytes, file_id)
}

/// Wrap raw serialized-file bytes into a minimal uncompressed UnityFS bundle holding a single
/// serialized entry named `entry_name`.
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
