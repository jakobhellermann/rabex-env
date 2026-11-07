//! Compare two directories of bundles by content: the header's versions, the entries
//! with their flags, and every entry's bytes, decompressed. Bundles written with another
//! compression compare equal when they hold the same.
//!
//! usage: bundle_content_diff <dir a> <dir b> [version for headers without one]
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rayon::prelude::*;

fn bundles(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        match path.is_dir() {
            true => bundles(&path, out)?,
            false if path.extension().is_some_and(|e| e == "bundle") => out.push(path),
            false => {}
        }
    }
    Ok(())
}

/// Why two bundles differ, `None` if they hold the same: header versions, entries and
/// their flags, and every entry's bytes, one pair of entries in memory at a time.
fn differ(a: &Path, b: &Path, config: &ExtractionConfig) -> Result<Option<String>> {
    let a = BundleFileReader::from_reader(Cursor::new(std::fs::read(a)?), config)?;
    let b = BundleFileReader::from_reader(Cursor::new(std::fs::read(b)?), config)?;
    let head = |reader: &BundleFileReader<Cursor<Vec<u8>>>| {
        let header = reader.header();
        format!(
            "v{} {} {:?}",
            header.version, header.unity_version, header.unity_revision
        )
    };
    if head(&a) != head(&b) {
        return Ok(Some(format!("header {} vs {}", head(&a), head(&b))));
    }
    if a.files().len() != b.files().len() {
        return Ok(Some("entry counts differ".to_owned()));
    }
    for (x, y) in a.files().iter().zip(b.files()) {
        if (&x.path, x.flags) != (&y.path, y.flags) {
            return Ok(Some(format!("entry {} vs {}", x.path, y.path)));
        }
        if a.read_entry(x)? != b.read_entry(y)? {
            return Ok(Some(format!("{} differs", x.path)));
        }
    }
    Ok(None)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let a = PathBuf::from(args.next().context("dir a")?);
    let b = PathBuf::from(args.next().context("dir b")?);
    let mut config = ExtractionConfig::default();
    if let Some(version) = args.next() {
        config.fallback_unity_version = FallbackUnityVersion::Exact(version.parse()?);
    }
    let mut paths = Vec::new();
    bundles(&a, &mut paths)?;
    let differ: Vec<String> = paths
        .par_iter()
        .filter_map(|path| {
            let other = b.join(path.strip_prefix(&a).unwrap());
            let result = differ(path, &other, &config);
            match result {
                Ok(None) => None,
                Ok(Some(why)) => Some(format!("{}: {why}", path.display())),
                Err(e) => Some(format!("{}: {e:#}", path.display())),
            }
        })
        .collect();
    println!("{} bundles, {} differ", paths.len(), differ.len());
    for line in differ.iter().take(10) {
        println!("  {line}");
    }
    Ok(())
}
