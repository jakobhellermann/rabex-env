//! Check that every reference a game's data makes into the engine's built-in files
//! (`unity default resources`, `unity_builtin_extra`) finds the same object — same
//! class, same name — in another Unity version's built-in files. A player of another
//! version ships its own; a reference whose path names something else there, or
//! nothing, reads as the wrong object or as null.
//!
//! usage: builtin_refs <source version> <source Resources dir> <target version>
//!        <target Resources dir> <file, bundle or dir>...

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::{Value, field};
use rayon::prelude::*;

/// The built-in files, by the name the externals list gives them.
const BUILTIN: &[(&str, &str)] = &[
    ("Library/unity default resources", "unity default resources"),
    ("Resources/unity_builtin_extra", "unity_builtin_extra"),
];

type Tpk = TypeTreeCache<TpkTypeTreeBlob>;

/// (class, name) of every object of a built-in file, by path id.
fn index(path: &Path, tpk: &Tpk) -> Result<HashMap<i64, (ClassId, String)>> {
    let data = std::fs::read(path).with_context(|| path.display().to_string())?;
    let file = SerializedFile::from_reader(&mut Cursor::new(&data))?;
    let mut out = HashMap::new();
    for info in file.objects() {
        let name = file
            .read::<Value>(info, tpk, &mut Cursor::new(data.as_slice()))
            .ok()
            .and_then(|value| {
                let named = field(&value, "m_ParsedForm").unwrap_or(&value);
                match field(named, "m_Name") {
                    Some(Value::String(name)) => Some(name.clone()),
                    _ => None,
                }
            })
            .unwrap_or_default();
        out.insert(info.m_PathID, (info.m_ClassID, name));
    }
    Ok(out)
}

/// Every (field path, file id, path id) a value references.
fn pptrs(value: &Value, path: &str, out: &mut Vec<(String, i32, i64)>) {
    match value {
        Value::Map(entries) => {
            if let (Some(file), Some(id)) = (field(value, "m_FileID"), field(value, "m_PathID")) {
                let file = match file {
                    Value::I32(n) => Some(*n),
                    _ => None,
                };
                let id = match id {
                    Value::I64(n) => Some(*n),
                    Value::I32(n) => Some(*n as i64),
                    _ => None,
                };
                if let (Some(file), Some(id)) = (file, id) {
                    out.push((path.to_owned(), file, id));
                    return;
                }
            }
            for (key, value) in entries.iter() {
                let key = match key {
                    Value::String(name) => name.clone(),
                    other => format!("{other:?}"),
                };
                pptrs(value, &format!("{path}.{key}"), out);
            }
        }
        Value::Seq(values) => values
            .iter()
            .enumerate()
            .for_each(|(i, v)| pptrs(v, &format!("{path}[{i}]"), out)),
        _ => {}
    }
}

/// References into the built-in files, as (built-in file, path id) → who refers.
type Refs = BTreeMap<(&'static str, i64), Vec<String>>;

fn scan(
    name: &str,
    data: &[u8],
    file: &SerializedFile,
    tpk: &Tpk,
    refs: &mut Refs,
    unread: &mut usize,
) {
    // the external index of each built-in file in this file's list; file id 0 is the
    // file itself, n is m_Externals[n - 1]
    let builtin: HashMap<i32, &'static str> = file
        .m_Externals
        .iter()
        .enumerate()
        .filter_map(|(i, external)| {
            BUILTIN
                .iter()
                .find(|(path, _)| external.pathName.ends_with(path))
                .map(|(_, name)| (i as i32 + 1, *name))
        })
        .collect();
    if builtin.is_empty() {
        return;
    }
    for info in file.objects() {
        let Ok(value) = file.read::<Value>(info, tpk, &mut Cursor::new(data)) else {
            *unread += 1;
            continue;
        };
        let mut found = Vec::new();
        pptrs(&value, "", &mut found);
        for (field, file_id, path_id) in found {
            if let Some(builtin) = builtin.get(&file_id) {
                refs.entry((builtin, path_id)).or_default().push(format!(
                    "{name}: {:?} {}{field}",
                    info.m_ClassID, info.m_PathID
                ));
            }
        }
    }
}

fn inputs(path: PathBuf, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(&path)? {
            inputs(entry?.path(), out)?;
        }
    } else {
        out.push(path);
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let source: UnityVersion = args.next().context("source version")?.parse()?;
    let source_dir = PathBuf::from(args.next().context("source Resources dir")?);
    let target: UnityVersion = args.next().context("target version")?.parse()?;
    let target_dir = PathBuf::from(args.next().context("target Resources dir")?);
    let mut files = Vec::new();
    for arg in args {
        inputs(PathBuf::from(arg), &mut files)?;
    }

    let tpk = Tpk::new(TpkTypeTreeBlob::embedded());
    let refs = Mutex::new(Refs::new());
    let unread = Mutex::new(0usize);
    files.par_iter().try_for_each(|path| -> Result<()> {
        let data = std::fs::read(path)?;
        let (mut local, mut local_unread) = (Refs::new(), 0);
        if data.starts_with(b"UnityFS") {
            let config = ExtractionConfig {
                fallback_unity_version: FallbackUnityVersion::Exact(source.clone()),
                ..Default::default()
            };
            let reader = BundleFileReader::from_reader(Cursor::new(&data), &config)?;
            for entry in reader.serialized_files() {
                let bytes = reader.read_at_entry(entry)?;
                let file =
                    SerializedFile::from_reader_with_version(&mut Cursor::new(&bytes), &source)?;
                scan(
                    &entry.path,
                    &bytes,
                    &file,
                    &tpk,
                    &mut local,
                    &mut local_unread,
                );
            }
        } else if let Ok(file) = SerializedFile::from_reader(&mut Cursor::new(&data)) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            scan(&name, &data, &file, &tpk, &mut local, &mut local_unread);
        }
        let mut refs = refs.lock().unwrap();
        for (key, from) in local {
            refs.entry(key).or_default().extend(from);
        }
        *unread.lock().unwrap() += local_unread;
        Ok(())
    })?;
    let refs = refs.into_inner().unwrap();

    let mut indices = HashMap::new();
    for (_, name) in BUILTIN {
        indices.insert(
            *name,
            (
                index(&source_dir.join(name), &tpk)?,
                index(&target_dir.join(name), &tpk)?,
            ),
        );
    }
    let (mut same, mut problems) = (0, Vec::new());
    for ((name, path_id), from) in &refs {
        let (in_source, in_target) = &indices[name];
        let wanted = in_source.get(path_id);
        let found = in_target.get(path_id);
        match (wanted, found) {
            (Some(a), Some(b)) if a == b => same += 1,
            _ => problems.push(format!(
                "{name} {path_id}: {source} has {wanted:?}, {target} has {found:?}\n      from {}",
                from.iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n      from ")
            )),
        }
    }
    println!(
        "{} distinct references into the built-in files, {same} find the same object in {target}, \
         {} do not; {} objects unreadable without a tree",
        refs.len(),
        problems.len(),
        unread.into_inner().unwrap()
    );
    for line in problems {
        println!("  {line}");
    }
    Ok(())
}
