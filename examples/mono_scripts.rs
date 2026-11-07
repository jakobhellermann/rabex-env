//! List the MonoScripts of bundles: CAB, path id, assembly and class.
//!
//! usage: mono_scripts <version> <bundle>...
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::unity::types::MonoScript;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let version: rabex::UnityVersion = args.next().context("version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    for path in args {
        let config = ExtractionConfig {
            fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
            ..Default::default()
        };
        let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;
        for entry in reader.serialized_files() {
            let bytes = reader.read_at_entry(entry)?;
            let file =
                SerializedFile::from_reader_with_version(&mut Cursor::new(&bytes), &version)?;
            for info in file
                .objects()
                .filter(|o| o.m_ClassID == ClassId::MonoScript)
            {
                let s: MonoScript = file.read(info, &tpk, &mut Cursor::new(bytes.as_slice()))?;
                println!(
                    "{} {} [{}]{}.{}",
                    entry.path, info.m_PathID, s.m_AssemblyName, s.m_Namespace, s.m_ClassName
                );
            }
        }
    }
    Ok(())
}
