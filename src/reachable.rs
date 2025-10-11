use anyhow::Result;
use rabex::files::SerializedFile;
use rabex::objects::PPtr;
use rabex::objects::pptr::PathId;
use rabex::typetree::TypeTreeProvider;
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

use crate::resolver::EnvResolver;
use crate::{Environment, trace_pptr};

/// Returns all reachable local objects from the starting point,
/// only going down the transform hierarchy.
pub fn reachable<R: EnvResolver, P: TypeTreeProvider>(
    env: &Environment<R, P>,
    file: &SerializedFile,
    reader: &mut (impl Read + Seek),
    from: VecDeque<PathId>,
) -> Result<(BTreeSet<PathId>, BTreeSet<PPtr>)> {
    let mut queue = from;

    let mut locals = BTreeSet::new();
    let mut externals = BTreeSet::new();

    while let Some(node) = queue.pop_front() {
        locals.insert(node);

        let reachable = reachable_one(env, file, node, reader)?;
        for reachable in reachable {
            if !reachable.is_local() {
                externals.insert(reachable);
                continue;
            }

            if locals.insert(reachable.m_PathID) {
                queue.push_back(reachable.m_PathID);
            }
        }
    }

    Ok((locals, externals))
}

pub fn reachable_one<R: EnvResolver, P: TypeTreeProvider>(
    env: &Environment<R, P>,
    file: &SerializedFile,
    from: PathId,
    reader: &mut (impl Read + Seek),
) -> Result<Vec<PPtr>> {
    let info = file.get_object_info(from).unwrap();

    /*let tt = env
    .tpk
    .get_typetree_node(info.m_ClassID, unity_version)
    .with_context(|| format!("No typetree available for {:?}", info.m_ClassID))?;*/
    let tt = file.get_typetree_for(info, &env.tpk)?;

    // TODO: use serialized typetree
    reader.seek(std::io::SeekFrom::Start(info.m_Offset as u64))?;
    trace_pptr::trace_pptrs_endianned(&tt, reader, file.m_Header.m_Endianess)
}
