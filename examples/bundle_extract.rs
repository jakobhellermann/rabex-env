//! Write a bundle's entries to a directory, all or those whose path a pattern contains.
//!
//! usage: bundle_extract <bundle> <out dir> [<substring>...]
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().context("bundle")?;
    let out = std::path::PathBuf::from(args.next().context("out dir")?);
    let wanted: Vec<String> = args.collect();
    let file = std::fs::File::open(&path)?;
    let map = unsafe { memmap2::Mmap::map(&file)? };
    let reader =
        BundleFileReader::from_reader(Cursor::new(&map[..]), &ExtractionConfig::default())?;
    std::fs::create_dir_all(&out)?;
    for entry in reader.files() {
        if !wanted.is_empty() && !wanted.iter().any(|w| entry.path.contains(w.as_str())) {
            continue;
        }
        let data = reader.read_at_entry(entry)?;
        std::fs::write(out.join(&entry.path), &data)?;
        println!("{} {}", entry.path, data.len());
    }
    Ok(())
}
