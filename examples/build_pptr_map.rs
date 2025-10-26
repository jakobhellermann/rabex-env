mod utils;

use std::path::Path;

use anyhow::{Context as _, Result};
use dashmap::DashMap;
use rabex::objects::pptr::PathId;
use rabex_env::addressables::ArchivePath;
use rabex_env::handle::SerializedFileHandle;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rustc_hash::FxHashMap;

// #[global_allocator]
// static ALLOC: dhat::Alloc = dhat::Alloc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
struct GlobalPPtr {
    global_file_id: u32,
    path_id: PathId,
}

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let files = env.load_all_serialized_files()?;

    // let _profiler = dhat::Profiler::new_heap();

    let global_file_map: FxHashMap<_, _> = ["Library/unity default resources"]
        .into_iter()
        .chain(files.keys().map(AsRef::as_ref))
        .enumerate()
        .map(|(i, file)| (file, i as u32))
        .collect();

    let pptr_references = DashMap::<GlobalPPtr, Vec<GlobalPPtr>>::new();

    files.par_iter().try_for_each(|(path, file)| {
        find_pptr_references(&pptr_references, &global_file_map, file, path)
    })?;

    let db_path = "pptrs.db";
    std::fs::remove_file(db_path)?;
    let mut db = rusqlite::Connection::open(db_path)?;
    db.pragma_update(None, "journal_mode", "off")?;
    db.execute_batch(
        r#"
CREATE TABLE files (
    global_file_id INTEGER PRIMARY KEY,
    filename TEXT NOT NULL UNIQUE,
    bundlename TEXT
);

CREATE TABLE IF NOT EXISTS pptr_references (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_global_file_id INTEGER NOT NULL,
    source_path_id INTEGER NOT NULL,
    reference_global_file_id INTEGER NOT NULL,
    reference_path_id INTEGER NOT NULL
);
"#,
    )?;

    {
        let tx = db.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO files (global_file_id, filename, bundlename) VALUES (?, ?, ?)",
        )?;
        for (filename, id) in &global_file_map {
            let addressables = env.addressables()?.unwrap();

            let bundle_name = match ArchivePath::try_parse(Path::new(filename))? {
                Some(archive_path) => Some(
                    addressables.cab_to_bundle[archive_path.bundle]
                        .display()
                        .to_string(),
                ),
                None => None,
            };

            stmt.execute((id, filename, bundle_name.unwrap_or_default()))?;
        }
        stmt.finalize()?;
        tx.commit()?;
    }
    {
        let tx = db.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO pptr_references (source_global_file_id, source_path_id, reference_global_file_id, reference_path_id)
             VALUES (?, ?, ?, ?)",
        )?;

        for item in pptr_references.iter() {
            let (source, references) = item.pair();
            for reference in references {
                stmt.execute((
                    source.global_file_id,
                    source.path_id,
                    reference.global_file_id,
                    reference.path_id,
                ))?;
            }
        }
        stmt.finalize()?;

        tx.commit()?;
    }

    std::mem::forget(env);
    Ok(())
}

/// Go through all objects, and for each referenced PPtr record the object as one of its references
fn find_pptr_references(
    out: &DashMap<GlobalPPtr, Vec<GlobalPPtr>>,
    global_file_map: &FxHashMap<&str, u32>,
    file: &SerializedFileHandle,
    archive_path: &str,
) -> Result<()> {
    let global_file_id = global_file_map[archive_path];

    for object in file.objects::<()>() {
        let reference_pptr = GlobalPPtr {
            global_file_id,
            path_id: object.path_id(),
        };

        let pptrs = object.reachable_one()?;

        for pptr in pptrs {
            let global_file_id = match pptr.is_local() {
                true => global_file_id,
                false => {
                    let external = &pptr.file_identifier(&file.file).unwrap();
                    *global_file_map
                        .get(external.pathName.as_str())
                        .context(external.pathName.clone())
                        .unwrap()
                }
            };

            let source_pptr = GlobalPPtr {
                global_file_id,
                path_id: pptr.m_PathID,
            };
            out.entry(source_pptr).or_default().push(reference_pptr);
        }
    }

    Ok(())
}
