//! Resolve a [`PPtr`] to a human-readable, version-stable [`ComponentPath`] — the reverse of
//! [`SceneLookup::lookup_path`](crate::scene_lookup::SceneLookup::lookup_path).
//!
//! A pointer into the scene hierarchy (a GameObject, or a component on one) resolves to its
//! Transform-hierarchy path with a `@Component` selector and disambiguating `:index` only where
//! names repeat (`Root/Child@PlayMakerFSM:15`). External pointers additionally report the external
//! file they live in, and their path is built in that file's own hierarchy. Loose objects (assets
//! with no GameObject) resolve to no path. Best-effort: anything unreadable yields `None`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rabex::objects::pptr::PathId;
use rabex::objects::{ClassId, PPtr, TypedPPtr};
use rabex::typetree::TypeTreeProvider;

use crate::Environment;
use crate::addressables::ArchivePath;
use crate::component_path::{Component, ComponentId, ComponentPath, PathSegment};
use crate::handle::SerializedFileHandle;
use crate::resolver::EnvResolver;
use crate::unity::types::{Component as UnityComponent, GameObject, Transform};

/// A resolved object pointer.
#[derive(Debug, Clone, Default)]
pub struct QualifiedPPtr {
    /// External file name; set only for external pointers (`m_FileID != 0`).
    pub file: Option<String>,
    /// Component path within the target file; set only if the pointer reaches a scene-hierarchy
    /// object (a GameObject or a component on one).
    pub path: Option<ComponentPath>,
    /// The target's `m_Name`, read only for loose (non-hierarchy) objects as a display fallback.
    pub name: Option<String>,
}

/// Resolves pointers to their hierarchy [`ComponentPath`], following external files and memoising
/// each touched file's root scan and per-target results.
pub struct Qualifier<'a, R, P> {
    local: FileCtx<'a, R, P>,
    /// External files keyed by `m_FileID`; `None` = couldn't be loaded.
    externals: HashMap<i32, Option<FileCtx<'a, R, P>>>,
}

impl<'a, R: EnvResolver, P: TypeTreeProvider> Qualifier<'a, R, P> {
    pub fn new(file: &SerializedFileHandle<'a, R, P>) -> Self {
        Qualifier {
            local: FileCtx::new(file.reborrow()),
            externals: HashMap::new(),
        }
    }

    /// Resolve `pptr` to its external file (if external) and component path (if it reaches the
    /// hierarchy). A null pointer or loose asset yields an empty / path-less result.
    pub fn qualify(&mut self, pptr: PPtr) -> QualifiedPPtr {
        let Some(pptr) = pptr.optional() else {
            return QualifiedPPtr::default();
        };
        if pptr.is_local() {
            let (path, name) = resolve_in(&mut self.local, pptr.m_PathID);
            return QualifiedPPtr {
                file: None,
                path,
                name,
            };
        }

        let file = pptr
            .file_identifier(self.local.file.file)
            .map(|external| external_label(self.local.file.env, &external.pathName));

        let key = pptr.m_FileID.value();
        if !self.externals.contains_key(&key) {
            let ctx = external_ctx(&self.local, pptr);
            self.externals.insert(key, ctx);
        }
        let (path, name) = match self.externals.get_mut(&key).unwrap() {
            Some(ctx) => resolve_in(ctx, pptr.m_PathID),
            None => (None, None),
        };
        QualifiedPPtr { file, path, name }
    }

    /// Local-only convenience: the component path of a local path id, or `None`.
    pub fn qualify_local(&mut self, target: PathId) -> Option<ComponentPath> {
        path_in(&mut self.local, target)
    }
}

/// One serialized file's roots and a per-target memo.
struct FileCtx<'a, R, P> {
    file: SerializedFileHandle<'a, R, P>,
    /// All root transforms as (transform path id, owning GameObject name).
    roots: Vec<(PathId, String)>,
    cache: HashMap<PathId, Option<ComponentPath>>,
}

impl<'a, R: EnvResolver, P: TypeTreeProvider> FileCtx<'a, R, P> {
    fn new(file: SerializedFileHandle<'a, R, P>) -> Self {
        let roots = roots(&file);
        FileCtx {
            file,
            roots,
            cache: HashMap::new(),
        }
    }
}

/// Human-readable label for an external file: its addressables bundle path if the name is a parseable
/// archive path that maps to a known bundle, otherwise the raw file name.
fn external_label<R: EnvResolver, P: TypeTreeProvider>(
    env: &Environment<R, P>,
    path_name: &str,
) -> String {
    if let Ok(Some(archive)) = ArchivePath::try_parse(Path::new(path_name))
        && let Ok(Some(addressables)) = env.addressables()
        && let Some(bundle) = addressables.cab_to_bundle.get(archive.bundle)
    {
        return bundle.display().to_string();
    }
    path_name.to_owned()
}

/// Load the external file `pptr` points into as a [`FileCtx`], or `None` if it can't be resolved.
fn external_ctx<'a, R: EnvResolver, P: TypeTreeProvider>(
    local: &FileCtx<'a, R, P>,
    pptr: PPtr,
) -> Option<FileCtx<'a, R, P>> {
    let handle = local.file.deref(pptr.typed::<()>()).ok()?.file;
    Some(FileCtx::new(handle))
}

/// Memoised component path for a target path id within `cx`'s file.
fn path_in<R: EnvResolver, P: TypeTreeProvider>(
    cx: &mut FileCtx<'_, R, P>,
    target: PathId,
) -> Option<ComponentPath> {
    if let Some(cached) = cx.cache.get(&target) {
        return cached.clone();
    }
    let path = build_path(cx, target).unwrap_or(None);
    cx.cache.insert(target, path.clone());
    path
}

/// The component path of `target`, plus its `m_Name` as a fallback when it has no path (loose).
fn resolve_in<R: EnvResolver, P: TypeTreeProvider>(
    cx: &mut FileCtx<'_, R, P>,
    target: PathId,
) -> (Option<ComponentPath>, Option<String>) {
    let path = path_in(cx, target);
    let name = path.is_none().then(|| name_in(cx, target)).flatten();
    (path, name)
}

/// A minimal view reading just `m_Name`.
#[derive(serde_derive::Deserialize)]
#[allow(non_snake_case)]
struct Named {
    m_Name: String,
}

/// The non-empty `m_Name` of `target`, if it has one (best-effort).
fn name_in<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    target: PathId,
) -> Option<String> {
    cx.file
        .deref_read(TypedPPtr::<Named>::local(target))
        .ok()
        .map(|named| named.m_Name)
        .filter(|name| !name.is_empty())
}

/// Build the [`ComponentPath`] addressing `target`, or `None` if it is not a GameObject or a
/// component on one (e.g. a loose asset).
fn build_path<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    target: PathId,
) -> Result<Option<ComponentPath>> {
    let class = cx.file.object_at::<()>(target)?.class_id();

    let (go_id, component) = if class == ClassId::GameObject {
        (target, None)
    } else {
        // Components carry an `m_GameObject`; assets without one aren't in the hierarchy.
        let Ok(component) = cx
            .file
            .deref_read(TypedPPtr::<UnityComponent>::local(target))
        else {
            return Ok(None);
        };
        let Some(go) = component.m_GameObject.optional() else {
            return Ok(None);
        };
        let id = component_id(&cx.file, PPtr::local(target))?;
        let index = component_index(cx, go.m_PathID, target, &id)?;
        (go.m_PathID, Some(Component { id, index }))
    };

    let Some(segments) = go_segments(cx, go_id)? else {
        return Ok(None);
    };
    Ok(Some(ComponentPath {
        segments,
        component,
    }))
}

/// A component's type id: the script class name for a MonoBehaviour, else its Unity class.
fn component_id<R: EnvResolver, P: TypeTreeProvider>(
    file: &SerializedFileHandle<'_, R, P>,
    component: PPtr,
) -> Result<ComponentId> {
    let handle = file.deref(component.typed::<()>())?;
    let class_id = handle.class_id();
    if class_id == ClassId::MonoBehaviour
        && let Some(script) = handle.mono_script()?
    {
        return Ok(ComponentId::Script(script.m_ClassName));
    }
    Ok(ComponentId::Class(class_id))
}

/// Index of `target` among `go`'s components of the same type, or `None` if it is the only one.
fn component_index<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    go_id: PathId,
    target: PathId,
    id: &ComponentId,
) -> Result<Option<usize>> {
    let go = cx.file.deref_read(TypedPPtr::<GameObject>::local(go_id))?;
    let mut same = Vec::new();
    for pair in &go.m_Component {
        if &component_id(&cx.file, pair.component)? == id {
            same.push(pair.component.m_PathID);
        }
    }
    Ok(disambiguate(&same, target))
}

/// The hierarchy segments (root first) addressing the GameObject `go_id`, or `None` if it has no
/// Transform (not part of the scene hierarchy).
fn go_segments<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    go_id: PathId,
) -> Result<Option<Vec<PathSegment>>> {
    let Some(transform_id) = gameobject_transform(cx, go_id)? else {
        return Ok(None);
    };

    let mut chain = Vec::new();
    let mut id = transform_id;
    loop {
        let transform = cx.file.deref_read(TypedPPtr::<Transform>::local(id))?;
        let father = transform.m_Father.optional();
        chain.push((id, transform));
        match father {
            Some(father) => id = father.m_PathID,
            None => break,
        }
    }
    chain.reverse();

    let mut segments = Vec::with_capacity(chain.len());
    for (i, (id, transform)) in chain.iter().enumerate() {
        let name = transform_go_name(cx, transform)?;
        let siblings = if i == 0 {
            self_named_roots(cx, &name)
        } else {
            named_children(cx, &chain[i - 1].1, &name)?
        };
        segments.push(PathSegment {
            name,
            index: disambiguate(&siblings, *id),
        });
    }
    Ok(Some(segments))
}

/// The path id of a GameObject's (Rect)Transform component, if any.
fn gameobject_transform<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    go_id: PathId,
) -> Result<Option<PathId>> {
    let go = cx.file.deref_read(TypedPPtr::<GameObject>::local(go_id))?;
    for pair in &go.m_Component {
        let class = cx.file.deref(pair.component.typed::<()>())?.class_id();
        if class == ClassId::Transform || class == ClassId::RectTransform {
            return Ok(Some(pair.component.m_PathID));
        }
    }
    Ok(None)
}

fn transform_go_name<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    transform: &Transform,
) -> Result<String> {
    Ok(cx.file.deref_read(transform.m_GameObject)?.m_Name)
}

/// Root transform ids whose GameObject is named `name`.
fn self_named_roots<R, P>(cx: &FileCtx<'_, R, P>, name: &str) -> Vec<PathId> {
    cx.roots
        .iter()
        .filter(|(_, n)| n == name)
        .map(|(id, _)| *id)
        .collect()
}

/// Child transform ids of `parent` whose GameObject is named `name`.
fn named_children<R: EnvResolver, P: TypeTreeProvider>(
    cx: &FileCtx<'_, R, P>,
    parent: &Transform,
    name: &str,
) -> Result<Vec<PathId>> {
    let mut matches = Vec::new();
    for child in &parent.m_Children {
        let transform = cx.file.deref_read(*child)?;
        if transform_go_name(cx, &transform)? == name {
            matches.push(child.m_PathID);
        }
    }
    Ok(matches)
}

/// `Some(position)` of `target` among `matches` when there is more than one, else `None`.
fn disambiguate(matches: &[PathId], target: PathId) -> Option<usize> {
    if matches.len() <= 1 {
        return None;
    }
    matches.iter().position(|id| *id == target)
}

/// All root transforms as (path id, owning GameObject name).
fn roots<R: EnvResolver, P: TypeTreeProvider>(
    file: &SerializedFileHandle<'_, R, P>,
) -> Vec<(PathId, String)> {
    let mut roots = Vec::new();
    for handle in file.transforms() {
        let path_id = handle.path_id();
        let Ok(transform) = handle.read() else {
            continue;
        };
        if transform.m_Father.optional().is_some() {
            continue;
        }
        if let Ok(go) = file.deref_read(transform.m_GameObject) {
            roots.push((path_id, go.m_Name));
        }
    }
    roots
}
