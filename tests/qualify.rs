//! Tests for [`rabex_env::qualify::Qualifier`] (PPtr -> ComponentPath).
//!
//! Scenes are built in memory (see `fixtures`) and re-opened through a real `Environment`, so the
//! resolver runs against a genuine `SerializedFileHandle`.

mod fixtures;

use fixtures::{
    Flat, file_referencing_external, named_asset_file, scene_with_script_component, tree,
    with_handle,
};
use rabex_env::Environment;
use rabex_env::qualify::Qualifier;
use rabex_env::rabex::objects::PPtr;
use rabex_env::rabex::objects::pptr::PathId;
use rabex_env::rabex::tpk::TpkTypeTreeBlob;
use rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::resolver::MemResolver;

fn path_of(bytes: Vec<u8>, id: PathId) -> Option<String> {
    with_handle("scene", bytes, |handle| {
        Qualifier::new(handle)
            .qualify_local(id)
            .map(|p| p.to_string())
    })
}

#[test]
fn gameobject_resolves_to_its_name() {
    let (bytes, go_ids) = Flat::new(&["Player", "Camera"]).write();
    assert_eq!(path_of(bytes.clone(), go_ids[0]).as_deref(), Some("Player"));
    assert_eq!(path_of(bytes, go_ids[1]).as_deref(), Some("Camera"));
}

#[test]
fn component_resolves_with_script_label() {
    let (bytes, go_id, mb_id) = scene_with_script_component("Enemy", "PlayMakerFSM");
    // the GameObject itself: just the name
    assert_eq!(path_of(bytes.clone(), go_id).as_deref(), Some("Enemy"));
    // the component: name + @ScriptClass
    assert_eq!(path_of(bytes, mb_id).as_deref(), Some("Enemy@PlayMakerFSM"));
}

#[test]
fn nested_hierarchy_path() {
    let (bytes, t) = tree("PlayMakerFSM");
    assert_eq!(path_of(bytes.clone(), t.root).as_deref(), Some("Root"));
    assert_eq!(
        path_of(bytes.clone(), t.leaf).as_deref(),
        Some("Root/Child/Leaf")
    );
    assert_eq!(
        path_of(bytes, t.leaf_mb).as_deref(),
        Some("Root/Child/Leaf@PlayMakerFSM")
    );
}

#[test]
fn sibling_name_collision_gets_index() {
    let (bytes, t) = tree("PlayMakerFSM");
    assert_eq!(
        path_of(bytes.clone(), t.dup0).as_deref(),
        Some("Root/Dup:0")
    );
    assert_eq!(path_of(bytes, t.dup1).as_deref(), Some("Root/Dup:1"));
}

#[test]
fn loose_object_resolves_to_no_path() {
    let (bytes, t) = tree("PlayMakerFSM");
    // the MonoScript asset has no GameObject -> not in the hierarchy
    assert_eq!(path_of(bytes, t.script), None);
}

#[test]
fn unknown_path_id_resolves_to_no_path() {
    let (bytes, _) = Flat::new(&["Player"]).write();
    assert_eq!(path_of(bytes, 999_999), None);
}

#[test]
fn external_name_survives_a_dangling_sibling_ref() {
    // an external file with one named asset, and a local file that references it
    let (ext_bytes, asset_id) = named_asset_file("dust_roach_clip");
    let (scene_bytes, ext_fid) = file_referencing_external("ext.assets");

    let mut resolver = MemResolver::new();
    resolver.insert("scene", scene_bytes);
    resolver.insert("ext.assets", ext_bytes);
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new(resolver, tpk);
    let handle = env.load_serialized("scene").unwrap();

    let mut q = Qualifier::new(&handle);

    // a dangling pointer into the external file (no such object) — must not poison the file for
    // later, valid lookups into the SAME external
    let dangling = q.qualify(PPtr::new(ext_fid, 999_999));
    assert_eq!(dangling.name, None);

    // a valid pointer into the same external must still resolve to the asset's name
    let valid = q.qualify(PPtr::new(ext_fid, asset_id));
    assert_eq!(valid.name.as_deref(), Some("dust_roach_clip"));
}

#[test]
fn resolve_reports_local_and_pathless() {
    let (bytes, t) = tree("PlayMakerFSM");
    with_handle("scene", bytes, |handle| {
        let mut r = Qualifier::new(handle);
        let leaf = r.qualify(PPtr::local(t.leaf));
        assert_eq!(leaf.file, None);
        assert_eq!(
            leaf.path.map(|p| p.to_string()).as_deref(),
            Some("Root/Child/Leaf")
        );

        let script = r.qualify(PPtr::local(t.script));
        assert_eq!(script.file, None);
        assert!(script.path.is_none());
    });
}
