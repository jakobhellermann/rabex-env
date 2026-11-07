//! Check the Vulkan programs of every shader in bundles against their modules.
//!
//! usage: verify_vulkan_bindings <version for bundles without one> <bundle or serialized file>...
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::Shader;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let version: UnityVersion = args.next().context("version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let (mut shaders, mut programs, mut bindings, mut failed) = (0, 0, 0, 0);
    for path in args {
        let raw = std::fs::read(&path)?;
        let files: Vec<Vec<u8>> = if raw.starts_with(b"UnityFS") {
            let config = ExtractionConfig {
                fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
                ..Default::default()
            };
            let reader = BundleFileReader::from_reader(Cursor::new(&raw), &config)?;
            reader
                .serialized_files()
                .map(|entry| reader.read_at_entry(entry).map_err(Into::into))
                .collect::<Result<_>>()?
        } else {
            vec![raw]
        };
        for data in files {
            let Ok(file) =
                SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)
            else {
                continue;
            };
            for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
                let shader: Shader = file.read(info, &tpk, &mut Cursor::new(data.as_slice()))?;
                shaders += 1;
                match rabex_shader_convert::vulkan_bindings::verify(&shader) {
                    Ok((p, b)) => {
                        programs += p;
                        bindings += b;
                    }
                    Err(error) => {
                        failed += 1;
                        if failed <= 10 {
                            println!("FAIL {error:#}");
                        }
                    }
                }
            }
        }
    }
    println!(
        "{shaders} shaders, {programs} Vulkan programs, {bindings} bindings checked, {failed} shaders failing"
    );
    anyhow::ensure!(failed == 0, "{failed} shaders disagree with their modules");
    Ok(())
}
