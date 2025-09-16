use crate::unity::types::Transform;
use anyhow::Result;
use rabex::files::SerializedFile;
use rabex::objects::pptr::PathId;
use rabex::typetree::TypeTreeProvider;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{Read, Seek};

enum RootLookup {
    Ambiguous(Vec<usize>),
    Root(usize),
}

pub struct SceneLookup<'a, P> {
    roots: Vec<(PathId, Transform)>,
    roots_lookup: HashMap<String, RootLookup>,
    file: &'a SerializedFile,
    tpk: P,
}

impl<'a, P: TypeTreeProvider> SceneLookup<'a, P> {
    pub fn new(file: &'a SerializedFile, reader: &mut (impl Read + Seek), tpk: P) -> Result<Self> {
        let mut roots = Vec::new();
        let mut roots_lookup = HashMap::new();

        for transform_obj in file.objects_of::<Transform>(&tpk)? {
            let transform = transform_obj.read(reader)?;
            if transform.m_Father.optional().is_some() {
                continue;
            }

            let go = transform
                .m_GameObject
                .deref_local(file, &tpk)?
                .read(reader)?;

            let index = roots.len();
            roots.push((transform_obj.info.m_PathID, transform));

            match roots_lookup.entry(go.m_Name) {
                Entry::Occupied(mut occupied_entry) => match occupied_entry.get_mut() {
                    RootLookup::Ambiguous(items) => items.push(index),
                    other => *other = RootLookup::Ambiguous(vec![index]),
                },
                Entry::Vacant(entry) => drop(entry.insert(RootLookup::Root(index))),
            }
        }

        Ok(SceneLookup {
            roots,
            roots_lookup,
            file,
            tpk,
        })
    }

    pub fn roots(&self) -> impl ExactSizeIterator<Item = (PathId, &Transform)> {
        self.roots
            .iter()
            .map(|(path_id, transform)| (*path_id, transform))
    }

    pub fn lookup_path(
        &self,
        reader: &mut (impl Read + Seek),
        path: &str,
    ) -> Result<Option<(PathId, Transform)>> {
        let mut segments = path.split('/');
        let Some(root_name) = segments.next() else {
            return Ok(None);
        };
        let root = match self.roots_lookup.get(root_name) {
            Some(RootLookup::Ambiguous(_)) => todo!(),
            Some(RootLookup::Root(index)) => &self.roots[*index],
            None => return Ok(None),
        };
        let mut current = vec![root.clone()];

        for segment in segments {
            let mut found = Vec::new();
            for current in &current {
                for child_pptr in &current.1.m_Children {
                    let child = child_pptr.deref_local(self.file, &self.tpk)?.read(reader)?;
                    let go = child
                        .m_GameObject
                        .deref_local(self.file, &self.tpk)?
                        .read(reader)?;

                    if go.m_Name == segment {
                        found.push((child_pptr.m_PathID, child));
                    }
                }
            }

            current = found;
            if current.is_empty() {
                return Ok(None);
            }
        }

        if current.len() > 1 {
            // TODO
            eprintln!("Found {} matches for path '{path}'", current.len());
        }

        Ok(current.pop())
    }
}
