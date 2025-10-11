use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::objects::pptr::PathId;
use rabex::objects::{ClassId, ClassIdType};
use rabex::serde_typetree;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rustc_hash::FxHashMap;
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

use crate::handle::SerializedFileHandle;
use crate::scene_lookup::SceneLookup;
use crate::unity::types::Transform;
use crate::{Environment, reachable};

// TODO:
// don't include ancestors.
// needs to be able to deal with retain_paths that are ancestors of each other

pub struct PruneSceneResult {
    pub reachable: BTreeSet<PathId>,
    pub roots: Vec<(String, Transform)>,
}

pub fn prune_scene_handle<'a>(
    file: SerializedFileHandle,
    retain_paths: impl IntoIterator<Item = &'a str>,
    replacements: &mut FxHashMap<PathId, Vec<u8>>,
    disable_roots: bool,
) -> Result<PruneSceneResult> {
    prune_scene(
        file.env,
        file.file,
        &mut file.reader(),
        retain_paths.into_iter(),
        replacements,
        disable_roots,
    )
}

pub fn prune_scene<'a>(
    env: &Environment,
    file: &SerializedFile,
    reader: &mut (impl Read + Seek),
    retain_paths: impl Iterator<Item = &'a str>,
    replacements: &mut FxHashMap<PathId, Vec<u8>>,
    disable_roots: bool,
) -> Result<PruneSceneResult> {
    let scene_lookup = SceneLookup::new(file, &mut *reader, &env.tpk)?;

    let mut retain_ids = VecDeque::with_capacity(retain_paths.size_hint().0);
    let mut retain_objects = Vec::with_capacity(retain_paths.size_hint().0);
    for path in retain_paths {
        match scene_lookup.lookup_path(&mut *reader, path)? {
            Some((path_id, transform)) => {
                retain_ids.push_back(path_id);
                retain_objects.push((path.to_owned(), transform));
            }
            None => {
                // TODO
                eprintln!("Could not find path '{path}'")
            }
        }
    }

    prune_scene_inner(
        env,
        file,
        reader,
        retain_ids,
        retain_objects,
        replacements,
        disable_roots,
    )
}

pub fn prune_scene_inner(
    env: &Environment,
    file: &SerializedFile,
    reader: &mut (impl Read + Seek),
    retain_ids: VecDeque<PathId>,
    retain_objects: Vec<(String, Transform)>,
    replacements: &mut FxHashMap<PathId, Vec<u8>>,
    disable_roots: bool,
) -> Result<PruneSceneResult> {
    let (mut all_reachable, _) = reachable::reachable(env, file, reader, retain_ids)
        .context("Could not determine reachable nodes")?;

    let mut ancestors = Vec::new();
    for (_, transform) in &retain_objects {
        for ancestor in transform.ancestors(file, reader, &env.tpk)? {
            let (id, transform) = ancestor?;
            if !all_reachable.insert(id) {
                break;
            }

            ancestors.push((id, transform));
        }
    }

    let transform_typetree = file.get_typetree_for_class(Transform::CLASS_ID, &env.tpk)?;

    for (id, transform) in ancestors {
        adjust_ancestor(
            replacements,
            file,
            &mut all_reachable,
            &transform_typetree,
            id,
            transform,
        )?;
    }

    for settings in file
        .objects()
        .filter(|info| [ClassId::RenderSettings].contains(&info.m_ClassID))
    {
        all_reachable.insert(settings.m_PathID);
    }

    for (_, root_transform) in &retain_objects {
        adjust_kept(
            replacements,
            file,
            reader.by_ref(),
            &env.tpk,
            root_transform,
            disable_roots,
        )?;
    }

    Ok(PruneSceneResult {
        reachable: all_reachable,
        roots: retain_objects,
    })
}

fn adjust_ancestor(
    replacements: &mut FxHashMap<PathId, Vec<u8>>,
    file: &SerializedFile,
    all_reachable: &mut BTreeSet<i64>,
    transform_typetree: &TypeTreeNode,
    id: PathId,
    mut transform: Transform,
) -> Result<()> {
    transform
        .m_Children
        .retain(|child| all_reachable.contains(&child.m_PathID));
    all_reachable.insert(transform.m_GameObject.m_PathID);
    let transform_modified =
        serde_typetree::to_vec_endianed(&transform, transform_typetree, file.m_Header.m_Endianess)?;
    assert!(replacements.insert(id, transform_modified).is_none());
    Ok(())
}

fn adjust_kept(
    replacements: &mut FxHashMap<PathId, Vec<u8>>,
    file: &SerializedFile,
    data: &mut (impl Read + Seek),
    tpk: &impl TypeTreeProvider,
    transform: &Transform,
    disable: bool,
) -> Result<(), anyhow::Error> {
    if disable {
        let go = transform.m_GameObject.deref_local(file, tpk)?;
        let mut go_data = go.read(data)?;
        go_data.m_IsActive = false;
        let go_modified =
            serde_typetree::to_vec_endianed(&go_data, go.typetree()?, file.m_Header.m_Endianess)?;
        assert!(replacements.insert(go.info.m_PathID, go_modified).is_none());
    }

    Ok(())
}
