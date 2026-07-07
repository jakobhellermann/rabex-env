//! Rewriting raw `{m_FileID, m_PathID}` PPtrs into the qualified `{file, path_id, class_id}`
//! shape that the `deref` builtin (and human readers) consume.

use anyhow::{Context as _, Result, anyhow};
use jaq_json::{Rc, Val};
use jaq_std::ValT as _;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::rabex::objects::PPtr;
use rabex_env::rabex::objects::pptr::{FileId, PathId};
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::resolver::EnvResolver;

/// A PPtr resolved to the file it lives in (an external name, or the local file) plus its path id —
/// extracted back out of the `{file, path_id, ..}` object [`qualify_pptrs`] produces.
pub struct QualifiedPPtr {
    pub file: String,
    pub path_id: PathId,
}

impl QualifiedPPtr {
    pub fn from_val(v: &Val) -> Result<Self> {
        let Val::Obj(map) = v else {
            return Err(anyhow!("expected a PPtr object, found {v}"));
        };
        let field = |name: &[u8]| map.iter().find(|(k, _)| k.as_utf8_bytes() == Some(name));
        let file = field(b"file")
            .and_then(|(_, v)| v.as_utf8_bytes())
            .context("PPtr missing string field `file`")?;
        let path_id = field(b"path_id")
            .and_then(|(_, v)| v.as_isize())
            .context("PPtr missing integer field `path_id`")?;
        Ok(QualifiedPPtr {
            file: String::from_utf8_lossy(file).into_owned(),
            path_id: path_id as PathId,
        })
    }
}

/// Recursively rewrite every `{m_FileID, m_PathID}` in `value`:
///
/// - a null pptr (`m_PathID == 0`) becomes `null`;
/// - otherwise it becomes `{file, path_id, class_id?}` where `file` is the local file path for a
///   local ref or the resolved external name (addressables `archive:/…` included), and `class_id`
///   is the target's engine class. `class_id` is best-effort: a dangling ref still yields
///   `{file, path_id}` rather than failing the whole object.
pub fn qualify_pptrs<R: EnvResolver, P: TypeTreeProvider>(
    file_path: &str,
    file: &SerializedFileHandle<'_, R, P>,
    value: &mut Val,
) -> Result<()> {
    *value = match value {
        Val::Arr(values) => {
            let values = Rc::get_mut(values).unwrap();
            return values
                .iter_mut()
                .try_for_each(|x| qualify_pptrs(file_path, file, x));
        }
        Val::Obj(map) => {
            let map = Rc::get_mut(map).unwrap();

            if map.len() == 2
                && let Some(file_id) = map
                    .iter()
                    .find(|x| x.0.as_utf8_bytes() == Some(b"m_FileID"))
                    .and_then(|(_, x)| x.as_isize())
                && let Some(path_id) = map
                    .iter()
                    .find(|x| x.0.as_utf8_bytes() == Some(b"m_PathID"))
                    .and_then(|(_, x)| x.as_isize())
            {
                match PPtr::new(FileId::new(file_id as i32), path_id as PathId).optional() {
                    Some(pptr) => {
                        let pptr_file = if pptr.is_local() {
                            file_path.to_owned()
                        } else {
                            let external = pptr
                                .file_identifier(file.file)
                                .with_context(|| format!("invalid PPtr: {pptr:?}"))?;
                            external.pathName.clone()
                        };

                        let mut obj = jaq_json::Map::default();
                        obj.insert("file".to_string().into(), pptr_file.into());
                        obj.insert("path_id".to_string().into(), path_id.into());
                        // Best-effort: reading the target's class is what makes
                        // `select(.m_Father.class_id == "Transform")` work, but a broken ref must
                        // not sink the whole enrichment — leave `class_id` off when it can't resolve.
                        if let Ok(target) = file.deref(pptr.typed::<()>()) {
                            let class_id = target.object.info.m_ClassID;
                            obj.insert(
                                "class_id".to_string().into(),
                                format!("{class_id:?}").into(),
                            );
                        }
                        Val::Obj(Rc::new(obj))
                    }
                    None => Val::Null,
                }
            } else {
                return map
                    .values_mut()
                    .try_for_each(|x| qualify_pptrs(file_path, file, x));
            }
        }
        _ => return Ok(()),
    };
    Ok(())
}
