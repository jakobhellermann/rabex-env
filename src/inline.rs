//! Copying the objects a serialized file references in other files into it.
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;

use anyhow::{Context, Result, bail};
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::objects::ClassId;
use rabex::objects::pptr::{FileId, PPtr, PathId};
use rabex::typetree::TypeTreeProvider;

use crate::Environment;
use crate::resolver::EnvResolver;
use crate::trace_pptr::{map_pptrs_inplace, trace_pptrs_endianned};

/// Copy all objects of `classes` from external files that are referenced in `builder` into it,
/// and point references to the copies.
/// Returns where each copy ended up at.
pub fn inline_external<R: EnvResolver, P: TypeTreeProvider, B: TypeTreeProvider>(
    env: &Environment<R, P>,
    builder: &mut SerializedFileBuilder<'_, B>,
    classes: &[ClassId],
) -> Result<HashMap<PPtr, PathId>> {
    let endianness = builder.serialized.m_Header.m_Endianess;

    let mut referencing = Vec::new();
    let mut targets = BTreeSet::new(); // the external pptrs

    for (&path_id, (info, data)) in &builder.objects {
        let tt = builder
            .serialized
            .get_typetree_for(info, builder.typetree_provider)?;

        let mut externals: Vec<PPtr> =
            trace_pptrs_endianned(&tt, &mut Cursor::new(data.as_ref()), endianness, env)
                .with_context(|| format!("object {path_id} ({})", tt.m_Type))?;
        externals.retain(|pptr| !pptr.m_FileID.is_local());

        if !externals.is_empty() {
            referencing.push(path_id);
            targets.extend(externals);
        }
    }

    for script in builder.serialized.m_ScriptTypes.iter().flatten() {
        if !script.m_LocalSerializedFileIndex.is_local() {
            targets.insert(PPtr::new(
                script.m_LocalSerializedFileIndex,
                script.m_LocalIdentifierInFile,
            ));
        }
    }

    let mut copies = HashMap::new();
    // types copied in, by their file and type id there: their type id here, and whether they hold pptrs
    let mut types: HashMap<(FileId, i32), (i32, bool)> = HashMap::new();

    for target in targets {
        let PPtr {
            m_FileID: file_id,
            m_PathID: path_id,
        } = target;

        let path = file_id
            .get_externals_index()
            .and_then(|index| builder.serialized.m_Externals.get(index))
            .with_context(|| format!("no external file {file_id:?}"))?
            .pathName
            .clone();

        let source = env.load_serialized(&path)?;
        let info = source
            .file
            .get_object_info(path_id)
            .with_context(|| format!("{path} has no object {path_id}"))?;

        if !classes.contains(&info.m_ClassID) {
            continue;
        }

        let tt = source.file.get_typetree_for(info, &env.tpk)?;
        let start = info.m_Offset as usize;
        let data = &source.data[start..start + info.m_Size as usize];

        let (type_id, holds_pptrs) = match types.get(&(file_id, info.m_TypeID)) {
            Some(&known) => known,
            None => {
                let mut ty = source.file.m_Types[info.m_TypeID as usize].clone();
                if builder.serialized.m_EnableTypeTree {
                    ty.m_Type = Some(tt.clone().into_owned());
                }
                let holds_pptrs = tt.contains_type_with(|ty| ty.starts_with("PPtr<"));
                let known = (builder.add_type_uncached(ty), holds_pptrs);
                types.insert((file_id, info.m_TypeID), known);
                known
            }
        };

        if holds_pptrs
            && !trace_pptrs_endianned(&tt, &mut Cursor::new(data), endianness, env)?.is_empty()
        {
            bail!(
                "{path} object {path_id} references other objects, which inlining doesn't follow"
            );
        }

        let copy = builder.get_next_path_id();
        builder.add_object_untyped_with(
            copy,
            info.m_ClassID,
            type_id,
            Cow::Owned(data.to_vec()),
        )?;
        copies.insert(target, copy);
    }

    for script in builder.serialized.m_ScriptTypes.iter_mut().flatten() {
        let target = PPtr::new(
            script.m_LocalSerializedFileIndex,
            script.m_LocalIdentifierInFile,
        );
        if let Some(&copy) = copies.get(&target) {
            script.m_LocalSerializedFileIndex = FileId::LOCAL;
            script.m_LocalIdentifierInFile = copy;
        }
    }

    for path_id in referencing {
        let (info, data) = builder.objects.get_mut(&path_id).expect("collected above");
        let tt = builder
            .serialized
            .get_typetree_for(info, builder.typetree_provider)?;

        map_pptrs_inplace(data.to_mut(), &tt, endianness, env, |_, pptr| {
            Ok(match copies.get(&pptr) {
                Some(&copy) => PPtr::local(copy),
                None => pptr,
            })
        })?;
    }

    Ok(copies)
}
