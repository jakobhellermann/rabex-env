//! Check how the binding indices of a game's Vulkan program parameters relate to the descriptor
//! set and binding decorations of the programs' SPIR-V stages.
//!
//! A Vulkan binding index is assumed to pack a stage mask in bits 24..32, the descriptor set in
//! bits 16..24 and the binding in bits 0..16; every mismatch with the SPIR-V is reported.

mod utils;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Binding, Platform, Shader};
use spirv::{Decoration, Op};

/// `(set, binding)` of every decorated variable in a SPIR-V module.
fn descriptor_bindings(words: &[u32]) -> BTreeSet<(u32, u32)> {
    let (mut sets, mut bindings) = (BTreeMap::new(), BTreeMap::new());
    let mut pos = 5;
    while pos < words.len() {
        let len = ((words[pos] >> 16) as usize).max(1);
        if words[pos] & 0xffff == Op::Decorate as u32 && len >= 4 {
            let (target, decoration, value) = (words[pos + 1], words[pos + 2], words[pos + 3]);
            match Decoration::from_u32(decoration) {
                Some(Decoration::DescriptorSet) => {
                    sets.insert(target, value);
                }
                Some(Decoration::Binding) => {
                    bindings.insert(target, value);
                }
                _ => {}
            }
        }
        pos += len;
    }
    bindings
        .iter()
        .map(|(target, &binding)| (sets.get(target).copied().unwrap_or(0), binding))
        .collect()
}

fn main() -> Result<()> {
    let game = std::env::args().nth(1).context("missing game")?;
    let env = utils::find_game(&game)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for name in ["globalgamemanagers.assets", "resources.assets"] {
        if let Ok(file) = env.load_serialized(name) {
            files.insert(name.to_owned(), file);
        }
    }
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }

    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: BTreeMap<String, String> = BTreeMap::new();
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            let shader: Shader = file.object_at(info.m_PathID)?.read()?;
            let name = &shader.m_ParsedForm.m_Name;
            for blob in shader
                .platform_blobs()?
                .into_iter()
                .filter(|b| b.platform == Platform::Vulkan)
            {
                for pass in shader
                    .m_ParsedForm
                    .m_SubShaders
                    .iter()
                    .flat_map(|s| &s.m_Passes)
                {
                    for variant in shader.pass_variants(pass, &blob)? {
                        let program = VulkanProgram::parse(variant.program.code)?;
                        let stages: Vec<(usize, BTreeSet<(u32, u32)>)> = program
                            .stages
                            .iter()
                            .enumerate()
                            .filter_map(|(slot, s)| s.as_ref().map(|s| (slot, s)))
                            .map(|(slot, s)| {
                                Ok((slot, descriptor_bindings(&smolv::decode(&s.smolv)?)))
                            })
                            .collect::<Result<_>>()?;
                        let mut check = |kind: &str, binding_name: &str, index: i32| {
                            let index = index as u32;
                            let (mask, set, binding) =
                                (index >> 24, (index >> 16) & 0xff, index & 0xffff);
                            let found: Vec<usize> = stages
                                .iter()
                                .filter(|(_, b)| b.contains(&(set, binding)))
                                .map(|&(slot, _)| slot)
                                .collect();
                            let found_mask: u32 = found.iter().map(|&slot| 1 << (slot + 2)).sum();
                            let outcome = if found.is_empty() {
                                format!("{kind}: set {set} binding {binding} in no stage")
                            } else if found_mask == mask {
                                format!("{kind}: set {set}, mask = stages with it")
                            } else {
                                format!("{kind}: set {set}, mask {mask:#x} but stages {found:?}")
                            };
                            examples.entry(outcome.clone()).or_insert_with(|| {
                                format!("{name} {:?} {binding_name} {index:#010x}", pass.m_Name)
                            });
                            *outcomes.entry(outcome).or_default() += 1;
                        };
                        for b in &variant.parameters.bindings {
                            match b {
                                Binding::Texture {
                                    name,
                                    index,
                                    sampler_index,
                                    ..
                                } => {
                                    check("texture", name, *index);
                                    if *sampler_index >= 0 {
                                        check("texture sampler", name, *sampler_index);
                                    }
                                }
                                Binding::ConstantBuffer { name, index, .. } => {
                                    check("cbuffer", name, *index)
                                }
                                Binding::Buffer { name, index, .. } => {
                                    check("buffer", name, *index)
                                }
                                Binding::Uav { name, index, .. } => check("uav", name, *index),
                                Binding::Sampler { index, .. } => {
                                    check("inline sampler", "", *index)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for (outcome, n) in &outcomes {
        println!("{n:6} {outcome}    e.g. {}", examples[outcome]);
    }
    Ok(())
}
