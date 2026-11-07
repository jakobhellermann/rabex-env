//! Hold a program's declared binding numbers against the sets and bindings its SPIR-V
//! decorates, to read off how the index in the metadata is built.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Binding, Platform, Shader, SubProgramView};

fn main() -> Result<()> {
    let dir = std::env::args().nth(1).context("game data directory")?;
    let _version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let limit: usize = std::env::args()
        .nth(3)
        .map_or(Ok(6), |n| n.parse())
        .context("limit")?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut shown = 0;
    let mut formula_holds = true;
    let mut checked = 0;

    for file in std::fs::read_dir(Path::new(&dir))? {
        let path = file?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.ends_with(".assets") || (name.starts_with("level") && !name.contains('.'))) {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let Ok(file) = SerializedFile::from_reader(&mut Cursor::new(&bytes)) else {
            continue;
        };
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let Ok(shader) = file.read::<Shader>(info, &tpk, &mut Cursor::new(bytes.as_slice()))
            else {
                continue;
            };
            for blob in shader.platform_blobs()? {
                if blob.platform != Platform::Vulkan {
                    continue;
                }
                for location in blob.entries()? {
                    let Ok(view) = SubProgramView::parse(blob.entry(location)?) else {
                        continue;
                    };
                    let program = VulkanProgram::parse(view.code)?;
                    let mut spirv: BTreeSet<(u32, u32, String, &'static str)> = BTreeSet::new();
                    for (slot, stage) in program.stages.iter().enumerate() {
                        let Some(stage) = stage else { continue };
                        let words = smolv::decode(&stage.smolv)?;
                        for resource in rabex_shader_convert::spirv::resources(&words)? {
                            let kind = match resource.kind {
                                rabex_shader_convert::spirv::ResourceKind::Sampler => "sampler",
                                rabex_shader_convert::spirv::ResourceKind::Texture { .. } => {
                                    "texture"
                                }
                                _ => "buffer",
                            };
                            spirv.insert((
                                slot as u32,
                                resource.set << 16 | resource.binding,
                                resource.name.clone(),
                                kind,
                            ));
                        }
                    }
                    let declared: Vec<(String, i32)> = match &view.layout {
                        rabex_shader::SubProgramViewLayout::V201806140 { parameters, .. } => {
                            parameters
                                .bindings
                                .iter()
                                .map(|b| (b.name().to_owned(), index_of(b)))
                                .collect()
                        }
                        _ => continue,
                    };
                    checked += 1;
                    // the guess: stage in the high byte, then the set, then the binding
                    for (name, index) in &declared {
                        let low = (*index as u32) & 0x00ff_ffff;
                        let matches = spirv
                            .iter()
                            .any(|(_, packed, spirv_name, _)| spirv_name == name && *packed == low);
                        if !matches {
                            formula_holds = false;
                        }
                    }
                    if shown < limit {
                        shown += 1;
                        println!("{}:", shader.m_ParsedForm.m_Name);
                        for (name, index) in &declared {
                            println!(
                                "  declared {name}: {index:#010x} -> stage {:#04x} set {} binding {}",
                                (*index as u32) >> 24,
                                ((*index as u32) >> 16) & 0xff,
                                (*index as u32) & 0xffff
                            );
                        }
                        for (slot, packed, name, kind) in &spirv {
                            println!(
                                "  spirv slot {slot} {kind} {name}: set {} binding {}",
                                packed >> 16,
                                packed & 0xffff
                            );
                        }
                    }
                }
            }
        }
    }
    println!(
        "{checked} programs checked, the set and binding of the index {} the SPIR-V",
        if formula_holds {
            "match"
        } else {
            "DO NOT match"
        }
    );
    Ok(())
}

fn index_of(binding: &Binding) -> i32 {
    match binding {
        Binding::Texture { index, .. }
        | Binding::ConstantBuffer { index, .. }
        | Binding::Buffer { index, .. }
        | Binding::Uav { index, .. }
        | Binding::Sampler { index, .. } => *index,
    }
}
