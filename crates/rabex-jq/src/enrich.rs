//! Turning a freshly-read Unity object into a jq-friendly value.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use jaq_json::{Rc, Val};
use rabex_env::addressables::ArchivePath;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::resolver::EnvResolver;
use rabex_env::unity::types::MonoScript;

use crate::pptr::qualify_pptrs;
use crate::scenes::SceneIndex;

/// Extra context for [`enrich`]. Both fields are optional; leave them unset for a bare object.
#[derive(Default, Clone, Copy)]
pub struct Enrich<'a> {
    /// Resolves `_scene`. Build one [`SceneIndex`] per env and reuse it across objects.
    pub scenes: Option<&'a SceneIndex>,
    /// The object's `MonoScript`, for `_type` / `_asm`. Only meaningful for MonoBehaviours.
    pub script: Option<&'a MonoScript>,
}

/// Enrich a freshly-read object `value` in place so jq queries can navigate it. Given `path` (the
/// file the object was read from) and its `file` handle, this does exactly:
///
/// - **PPtrs** — every `{m_FileID, m_PathID}` anywhere in the object becomes `{file, path_id,
///   class_id?}`, the shape the `deref` builtin follows. `m_FileID` is resolved against `file`'s
///   externals, so refs into other files — including addressables `archive:/…` bundles — qualify
///   too. A null pptr becomes `null`; `class_id` is best-effort (see [`qualify_pptrs`]).
/// - **`_file`** — the path the object was read from. For an addressables CAB it is rewritten to
///   the owning bundle's path (with the inner file in parens when it differs), matching what the
///   rest of the tooling shows; otherwise it is `path` verbatim.
/// - **`_scene`** — the scene name, when `opts.scenes` is set and `path` belongs to a scene
///   (built-in `levelN` or an addressables scene bundle). Absent otherwise.
/// - **`_type`, `_asm`** — the script's full type name and assembly, when `opts.script` is set
///   (i.e. for a MonoBehaviour). Absent otherwise.
///
/// Non-object values (rare at the top level) are still PPtr-qualified but gain no `_*` keys.
pub fn enrich<R: EnvResolver, P: TypeTreeProvider>(
    value: &mut Val,
    path: &str,
    file: &SerializedFileHandle<'_, R, P>,
    opts: Enrich<'_>,
) -> Result<()> {
    qualify_pptrs(path, file, value)?;

    let Val::Obj(obj) = value else {
        return Ok(());
    };
    let mut map = Rc::into_inner(std::mem::take(obj)).expect("no other references to the object");

    map.insert("_file".to_string().into(), file_label(path, file).into());

    if let Some(scene) = opts.scenes.and_then(|s| s.scene_of(path)) {
        map.insert("_scene".to_string().into(), scene.to_owned().into());
    }

    if let Some(script) = opts.script {
        map.insert(
            "_type".to_string().into(),
            script.full_name().into_owned().into(),
        );
        map.insert(
            "_asm".to_string().into(),
            script.assembly_name().into_owned().into(),
        );
    }

    *value = Val::obj(map);
    Ok(())
}

/// The `_file` value: an addressables CAB rewritten to its bundle path, else `path` unchanged.
fn file_label<R: EnvResolver, P: TypeTreeProvider>(
    path: &str,
    file: &SerializedFileHandle<'_, R, P>,
) -> String {
    if let Ok(Some(cab)) = ArchivePath::try_parse(Path::new(path))
        && let Ok(Some(aa)) = file.env.addressables()
        && let Some(bundle) = aa.cab_to_bundle.get(cab.bundle)
    {
        let mut label = bundle.display().to_string();
        if cab.bundle != cab.file {
            let _ = write!(&mut label, " ({})", cab.file);
        }
        return label;
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Enrich, enrich};
    use crate::{QueryRunner, qualify_pptrs};
    use jaq_json::Val;
    use rabex_env::unity::types::MonoBehaviour;
    use rabex_env_testkit::{scene_with_script_component, with_handle};

    fn val(s: &str) -> Val {
        jaq_json::read::parse_single(s.as_bytes()).unwrap()
    }

    /// Enriched values are asserted through the query engine, which is how they're actually used.
    fn query(
        env: &rabex_env::Environment<rabex_env::resolver::MemResolver>,
        q: &str,
        v: &Val,
    ) -> Vec<Val> {
        QueryRunner::new(q).unwrap().exec(env, v.clone()).unwrap()
    }

    #[test]
    fn qualifies_pptrs_and_tags_file_and_type() {
        let (bytes, go_id, mb_id) = scene_with_script_component("Hero", "HeroController");
        with_handle("level0", bytes, |file| {
            // GameObject: no script, so just `_file` + qualified component pptrs.
            let mut go = file.object_at::<Val>(go_id).unwrap().read().unwrap();
            enrich(&mut go, "level0", file, Enrich::default()).unwrap();
            assert_eq!(query(file.env, "._file", &go), vec![val(r#""level0""#)]);
            // m_Component[].component pptrs became {file, path_id, class_id}: first is the Transform.
            assert_eq!(
                query(file.env, ".m_Component[0].component.class_id", &go),
                vec![val(r#""Transform""#)],
            );
            assert_eq!(query(file.env, "._type", &go), vec![val("null")]);

            // MonoBehaviour: passing its script tags `_type` / `_asm`.
            let script = file
                .object_at::<MonoBehaviour>(mb_id)
                .unwrap()
                .mono_script()
                .unwrap();
            let mut mb = file.object_at::<Val>(mb_id).unwrap().read().unwrap();
            enrich(
                &mut mb,
                "level0",
                file,
                Enrich {
                    scenes: None,
                    script: script.as_ref(),
                },
            )
            .unwrap();
            assert_eq!(
                query(file.env, "._type", &mb),
                vec![val(r#""HeroController""#)]
            );
            assert_eq!(
                query(file.env, "._asm", &mb),
                vec![val(r#""Assembly-CSharp.dll""#)]
            );
        });
    }

    #[test]
    fn dangling_ref_qualifies_without_class_id_and_does_not_fail() {
        let (bytes, _) = rabex_env_testkit::Flat::new(&["Root"]).write();
        with_handle("level0", bytes, |file| {
            // A local pptr to a path id that doesn't exist.
            let mut v = val(r#"{ "ref": { "m_FileID": 0, "m_PathID": 999999 } }"#);
            qualify_pptrs("level0", file, &mut v).unwrap();
            assert_eq!(query(file.env, ".ref.file", &v), vec![val(r#""level0""#)]);
            assert_eq!(query(file.env, ".ref.path_id", &v), vec![val("999999")]);
            // class_id couldn't resolve, so the key is simply absent.
            assert_eq!(
                query(file.env, ".ref | has(\"class_id\")", &v),
                vec![val("false")]
            );
        });
    }
}
