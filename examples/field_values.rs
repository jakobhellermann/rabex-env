//! Tally the values of fields across a game's objects: what a version's builds write, the
//! evidence for the value a field the older form lacks takes.
//!
//! usage: field_values <version> <Class.field>,... <serialized file, bundle or dir>...
//! `Class.a+b` tallies the fields side by side, `Class.a.b` reads into a field, through
//! every element of the arrays on the way.
//! Bundles without a version in their header are read as `<version>`.
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::{Value, field};
use rayon::prelude::*;

/// The values at a dotted path, every element of the arrays on the way.
fn at<'v>(value: &'v Value, path: &[&str]) -> Vec<&'v Value> {
    let Some((first, rest)) = path.split_first() else {
        return vec![value];
    };
    match value {
        Value::Seq(elements) => elements.iter().flat_map(|e| at(e, path)).collect(),
        _ => field(value, first).map_or(Vec::new(), |v| at(v, rest)),
    }
}

type Tally = BTreeMap<String, BTreeMap<String, usize>>;

fn files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.is_dir() {
        out.push(path.to_owned());
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        files(&entry?.path(), out)?;
    }
    Ok(())
}

fn tally_file(
    data: &[u8],
    version: &UnityVersion,
    in_bundle: bool,
    wanted: &[(String, String)],
    tpk: &TypeTreeCache<TpkTypeTreeBlob>,
    tally: &mut Tally,
) -> Result<()> {
    let mut cursor = Cursor::new(data);
    let file = match in_bundle {
        true => SerializedFile::from_reader_with_version(&mut cursor, version)?,
        false => SerializedFile::from_reader(&mut cursor)?,
    };
    for info in file.objects() {
        let class = format!("{:?}", info.m_ClassID);
        let fields: Vec<&str> = wanted
            .iter()
            .filter(|(c, _)| *c == class)
            .map(|(_, f)| f.as_str())
            .collect();
        if fields.is_empty() {
            continue;
        }
        let Ok(value) = file.read::<Value>(info, tpk, &mut Cursor::new(data)) else {
            *tally
                .entry(format!("{class}: unreadable"))
                .or_default()
                .entry(String::new())
                .or_default() += 1;
            continue;
        };
        for name in fields {
            // `a+b`: the fields side by side, to see how they relate
            let shown = name
                .split('+')
                .map(|name| {
                    let found = at(&value, &name.split('.').collect::<Vec<_>>());
                    match found.as_slice() {
                        [] => "<absent>".to_owned(),
                        [one] => format!("{one:?}"),
                        many => format!("{many:?}"),
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ");
            *tally
                .entry(format!("{class}.{name}"))
                .or_default()
                .entry(shown.chars().take(300).collect())
                .or_default() += 1;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let version: UnityVersion = args.next().context("version")?.parse()?;
    let wanted: Vec<(String, String)> = args
        .next()
        .context("Class.field,...")?
        .split(',')
        .map(|s| {
            let (c, f) = s.split_once('.').context("Class.field")?;
            Ok((c.to_owned(), f.to_owned()))
        })
        .collect::<Result<_>>()?;
    let mut paths = Vec::new();
    for arg in args {
        files(Path::new(&arg), &mut paths)?;
    }
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let tallies: Vec<Tally> = paths
        .par_iter()
        .map(|path| {
            let mut tally = Tally::new();
            let Ok(data) = std::fs::read(path) else {
                return tally;
            };
            if data.starts_with(b"UnityFS") {
                let config = ExtractionConfig {
                    fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
                    ..Default::default()
                };
                let Ok(reader) =
                    BundleFileReader::from_reader(Cursor::new(data.as_slice()), &config)
                else {
                    return tally;
                };
                for entry in reader.serialized_files() {
                    if let Ok(bytes) = reader.read_entry(entry) {
                        let _ = tally_file(&bytes, &version, true, &wanted, &tpk, &mut tally);
                    }
                }
            } else {
                let _ = tally_file(&data, &version, false, &wanted, &tpk, &mut tally);
            }
            tally
        })
        .collect();
    let mut total = Tally::new();
    for tally in tallies {
        for (key, values) in tally {
            for (value, n) in values {
                *total
                    .entry(key.clone())
                    .or_default()
                    .entry(value)
                    .or_default() += n;
            }
        }
    }
    for (key, values) in &total {
        println!("{key}");
        for (value, n) in values {
            println!("  {n:7} × {value}");
        }
    }
    Ok(())
}
