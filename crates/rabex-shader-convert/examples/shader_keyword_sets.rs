//! List the keyword set of every Vulkan variant of every shader, one line per set:
//! `shader name<TAB>keyword keyword…`, the input of the render harness's `-rabexVariants`.
//!
//! Only shaders in the 2022+ form: the 2020.2 form stores keyword masks whose names are
//! not in the shader.
//! TODO: the 2020.2 form's sets would come from the shader's source form before the
//! downport; until then the harness covers a 2020.2 bundle with the keyword-less variant.
//!
//! usage: shader_keyword_sets <version for bundles without one> <bundle or serialized file>...
use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::{Platform, Shader};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let version: UnityVersion = args.next().context("version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut lines = BTreeSet::new();
    let mut skipped = 0;
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
                let name = shader.m_ParsedForm.m_Name.clone();
                for blob in shader.platform_blobs()? {
                    if blob.platform != Platform::Vulkan {
                        continue;
                    }
                    for pass in shader
                        .m_ParsedForm
                        .m_SubShaders
                        .iter()
                        .flat_map(|s| &s.m_Passes)
                    {
                        if pass.progVertex.m_PlayerSubPrograms.is_none() {
                            skipped += 1;
                            continue;
                        }
                        for variant in shader.pass_variants(pass, &blob)? {
                            let mut keywords = variant.keywords.clone();
                            keywords.sort();
                            lines.insert(format!("{name}\t{}", keywords.join(" ")));
                        }
                    }
                }
            }
        }
    }
    for line in &lines {
        println!("{line}");
    }
    eprintln!(
        "{} keyword sets, {skipped} passes in the 2020.2 form skipped",
        lines.len()
    );
    Ok(())
}
