//! What a game's Vulkan programs declare in their SPIR-V: the resources, their
//! descriptor sets and bindings, and whether a texture carries its sampler with it.
//!
//! The two forms differ here, and the difference is what a conversion has to produce.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Platform, Shader, SubProgramView};
use rabex_shader_convert::spirv::ResourceKind;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("bundle, file or game directory")?;
    let version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let want = std::env::args().nth(3).unwrap_or_default();

    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    for data in files(Path::new(&path), &version)? {
        let Ok(file) = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)
        else {
            continue;
        };
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let Ok(shader) = file.read::<Shader>(info, &tpk, &mut Cursor::new(data.as_slice()))
            else {
                continue;
            };
            if !want.is_empty() && shader.m_ParsedForm.m_Name != want {
                continue;
            }
            for blob in shader.platform_blobs()? {
                if blob.platform != Platform::Vulkan {
                    continue;
                }
                for location in blob.entries()? {
                    let Ok(view) = SubProgramView::parse(blob.entry(location)?) else {
                        continue;
                    };
                    let program = VulkanProgram::parse(view.code)?;
                    for slot in program.stages.iter().flatten() {
                        let words = smolv::decode(&slot.smolv)?;
                        for resource in rabex_shader_convert::spirv::resources(&words)? {
                            let kind = match &resource.kind {
                                ResourceKind::Texture { combined, .. } => {
                                    format!("texture combined={combined}")
                                }
                                ResourceKind::Sampler => "sampler".to_owned(),
                                ResourceKind::UniformBuffer { .. } => "uniform buffer".to_owned(),
                                other => format!("{other:?}"),
                            };
                            *shapes
                                .entry(format!(
                                    "set {} binding {:2} {kind}",
                                    resource.set, resource.binding
                                ))
                                .or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    for (shape, count) in &shapes {
        println!("{count:6} × {shape}");
    }
    Ok(())
}

/// The serialized files of a bundle, a single file, or a game's data directory.
fn files(path: &Path, version: &UnityVersion) -> Result<Vec<Vec<u8>>> {
    if path.is_dir() {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".assets") || (name.starts_with("level") && !name.contains('.')) {
                files.push(std::fs::read(&path)?);
            }
        }
        return Ok(files);
    }
    let raw = std::fs::read(path)?;
    if !raw.starts_with(b"UnityFS") {
        return Ok(vec![raw]);
    }
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(raw), &config)?;
    Ok(reader
        .serialized_files()
        .map(|entry| reader.read_at_entry(entry))
        .collect::<Result<_, _>>()?)
}
