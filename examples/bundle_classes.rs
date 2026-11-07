//! List the engine classes a bundle carries, with the Unity version of its serialized files.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};

fn main() -> Result<()> {
    for path in std::env::args().skip(1) {
        let bundle = BundleFileReader::from_reader(
            Cursor::new(std::fs::read(&path).with_context(|| path.clone())?),
            &ExtractionConfig::default(),
        )?;
        println!("== {path}");
        for entry in bundle.serialized_files() {
            let data = bundle.read_at_entry(entry)?;
            let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
            println!(
                "  {}: unity version {}, format version {}, {} objects",
                entry.path,
                file.m_UnityVersion
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                file.m_Header.m_Version,
                file.objects().count()
            );
            let mut classes = BTreeMap::new();
            for info in file.objects() {
                *classes
                    .entry(info.m_ClassID.name().unwrap_or("unknown").to_string())
                    .or_insert(0) += 1;
            }
            for (class, count) in classes {
                println!("    {class} × {count}");
            }
        }
    }
    Ok(())
}
