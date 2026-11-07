//! Whether a bundle's serialized files carry their own type trees, and for which classes.

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle path")?;
    let source: UnityVersion = std::env::args()
        .nth(2)
        .context("the version the bundle was built with")?
        .parse()?;
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(source.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;
    for entry in reader.files() {
        let data = reader.read_at_entry(entry)?;
        println!(
            "  file {} flags {} {} bytes",
            entry.path,
            entry.flags,
            data.len()
        );
    }
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &source)?;
        let with_tree = file.m_Types.iter().filter(|t| t.m_Type.is_some()).count();
        println!(
            "{}: version {:?} type trees {} ({}/{} types carry one), {} objects, {} bytes uncompressed",
            entry.path,
            file.m_UnityVersion,
            file.m_EnableTypeTree,
            with_tree,
            file.m_Types.len(),
            file.objects().count(),
            data.len()
        );
    }
    Ok(())
}
