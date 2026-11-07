//! Map every shader variant to a checksum of its compiled program, per platform.
//!
//! Two builds of the same shader must agree here: the keywords a variant is selected by
//! and the code the player then runs. A mapping that slipped shows as the same keyword
//! set pointing at different code — the player would pick the variant and get another
//! one's program, which no amount of correct keywords repairs.

use std::collections::BTreeMap;
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
    let path = std::env::args().nth(1).context("bundle")?;
    let version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let want = std::env::args().nth(3).context("shader name")?;

    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let raw = std::fs::read(&path)?;
    // a bundle, or a serialized file of a game's own data
    let files: Vec<Vec<u8>> = if raw.starts_with(b"UnityFS") {
        let config = ExtractionConfig {
            fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
            ..Default::default()
        };
        let reader = BundleFileReader::from_reader(Cursor::new(raw), &config)?;
        reader
            .serialized_files()
            .map(|entry| reader.read_at_entry(entry))
            .collect::<Result<_, _>>()?
    } else {
        vec![raw]
    };
    for data in files {
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            // a game's own files ship no type trees; the package has them per version
            let shader: Shader = file.read(info, &tpk, &mut Cursor::new(data.as_slice()))?;
            if shader.m_ParsedForm.m_Name != want {
                continue;
            }
            if std::env::var("ENTRY").is_ok() {
                for blob in shader.platform_blobs()? {
                    if blob.platform != rabex_shader::Platform::Vulkan {
                        continue;
                    }
                    for (index, location) in blob.entries()?.iter().enumerate() {
                        let bytes = blob.entry(*location)?;
                        let view = rabex_shader::SubProgramView::parse(bytes)?;
                        println!(
                            "entry {index}: {} bytes, version {}, program type {}, code {} bytes, keywords {:?}",
                            bytes.len(),
                            view.version(),
                            view.program_type,
                            view.code.len(),
                            view.keywords
                        );
                        println!("    layout {:?}", view.layout);
                    }
                }
                return Ok(());
            }
            if std::env::var("MASKS").is_ok() {
                for subshader in &shader.m_ParsedForm.m_SubShaders {
                    for (index, pass) in subshader.m_Passes.iter().enumerate() {
                        let slots: Vec<_> = pass
                            .m_NameIndices
                            .iter()
                            .filter(|(name, _)| {
                                name.chars().all(|c| {
                                    c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()
                                })
                            })
                            .map(|(name, slot)| format!("{name}={slot}"))
                            .collect();
                        println!(
                            "pass{index}: globalMask {:?} localMask {:?}\n  keyword slots {slots:?}",
                            pass.m_GlobalKeywordMask, pass.m_LocalKeywordMask
                        );
                    }
                }
                return Ok(());
            }
            let mut rows: BTreeMap<String, String> = BTreeMap::new();
            for blob in shader.platform_blobs()? {
                let entries = blob.entries()?;
                for subshader in &shader.m_ParsedForm.m_SubShaders {
                    for (index, pass) in subshader.m_Passes.iter().enumerate() {
                        // the newer form names a variant's keywords, the older one
                        // indexes them into the pass' name table
                        if shader.m_ParsedForm.m_KeywordNames.is_some() {
                            for variant in shader.pass_variants(pass, &blob)? {
                                let mut keywords = variant.keywords.clone();
                                keywords.sort();
                                if std::env::var("UNIFORMS").is_ok() {
                                    println!(
                                        "--- {:?} {:?} {keywords:?}",
                                        blob.platform, variant.stage
                                    );
                                    for buffer in &variant.parameters.constant_buffers {
                                        let names: Vec<_> = buffer
                                            .constants
                                            .iter()
                                            .map(|c| format!("{}@{}", c.name, c.offset))
                                            .collect();
                                        println!(
                                            "  buffer {} size {}: {names:?}",
                                            buffer.name, buffer.size
                                        );
                                    }
                                    for binding in &variant.parameters.bindings {
                                        println!("  binding {binding:?}");
                                    }
                                }
                                rows.insert(
                                    format!(
                                        "{:?} pass{index} {:?} {keywords:?}",
                                        blob.platform, variant.stage
                                    ),
                                    format!("{:016x}", checksum(variant.program.code)),
                                );
                            }
                            continue;
                        }
                        for (stage, program) in pass.programs() {
                            for sub in &program.m_SubPrograms {
                                let mut keywords: Vec<String> = sub
                                    .m_GlobalKeywordIndices
                                    .iter()
                                    .flatten()
                                    .chain(sub.m_LocalKeywordIndices.iter().flatten())
                                    .filter_map(|i| {
                                        pass.m_NameIndices
                                            .iter()
                                            .find(|(_, slot)| **slot == *i as i32)
                                            .map(|(name, _)| name.clone())
                                    })
                                    .collect();
                                keywords.sort();
                                let location = *entries
                                    .get(sub.m_BlobIndex as usize)
                                    .with_context(|| format!("blob {} missing", sub.m_BlobIndex))?;
                                let code =
                                    rabex_shader::SubProgramView::parse(blob.entry(location)?)?;
                                if std::env::var("UNIFORMS").is_ok() {
                                    println!(
                                        "--- {:?} {stage:?} gpu{} {keywords:?}",
                                        blob.platform, sub.m_GpuProgramType
                                    );
                                    for b in sub
                                        .m_ConstantBuffers
                                        .iter()
                                        .flatten()
                                        .flat_map(|c| c.m_StructParams.iter())
                                    {
                                        println!(
                                            "  struct param nameIndex {} arraySize {} size {}",
                                            b.m_NameIndex, b.m_ArraySize, b.m_StructSize
                                        );
                                    }
                                    if let rabex_shader::SubProgramViewLayout::V201806140 {
                                        parameters,
                                        ..
                                    } = &code.layout
                                    {
                                        for b in &parameters.bindings {
                                            println!("  binding {b:?}");
                                        }
                                    }
                                    for buffer in sub.m_ConstantBuffers.iter().flatten() {
                                        let name = pass
                                            .m_NameIndices
                                            .iter()
                                            .find(|(_, slot)| **slot == buffer.m_NameIndex)
                                            .map(|(n, _)| n.clone())
                                            .unwrap_or_default();
                                        let name_of = |index: i32| {
                                            pass.m_NameIndices
                                                .iter()
                                                .find(|(_, slot)| **slot == index)
                                                .map(|(n, _)| n.clone())
                                                .unwrap_or_default()
                                        };
                                        let mut members: Vec<String> = buffer
                                            .m_VectorParams
                                            .iter()
                                            .map(|p| {
                                                format!("{}@{}", name_of(p.m_NameIndex), p.m_Index)
                                            })
                                            .chain(buffer.m_MatrixParams.iter().map(|p| {
                                                format!("{}@{}", name_of(p.m_NameIndex), p.m_Index)
                                            }))
                                            .collect();
                                        members.sort();
                                        println!(
                                            "  buffer {name} size {}: {members:?}",
                                            buffer.m_Size
                                        );
                                    }
                                }
                                rows.insert(
                                    format!(
                                        "{:?} pass{index} {stage:?} {keywords:?}",
                                        blob.platform
                                    ),
                                    format!("{:016x}", checksum(code.code)),
                                );
                            }
                        }
                    }
                }
            }
            for (key, code) in rows {
                println!("{key} -> {code}");
            }
            return Ok(());
        }
    }
    anyhow::bail!("no shader {want} in {path}")
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
