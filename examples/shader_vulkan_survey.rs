//! Survey the Vulkan program containers of a game's shaders: which SPIR-V features of a program's
//! stages go with which bits of its flag word and of the word before the stage hashes.

mod utils;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex_shader::{Binding, Platform, Shader};
use spirv::{BuiltIn, Capability, Decoration, ExecutionMode, Op};

fn word(program: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(program[i * 4..i * 4 + 4].try_into().unwrap())
}

/// Capabilities, builtins, execution modes and notable opcodes of a SPIR-V module.
fn spirv_features(words: &[u32]) -> Vec<String> {
    let mut features = Vec::new();
    let mut pos = 5;
    while pos < words.len() {
        let len = (words[pos] >> 16) as usize;
        let op = words[pos] & 0xffff;
        let operands = &words[pos + 1..pos + len.max(1)];
        let builtin = Decoration::BuiltIn as u32;
        match Op::from_u32(op) {
            Some(Op::Capability) => {
                features.push(format!("{:?}", Capability::from_u32(operands[0])))
            }
            Some(Op::ExecutionMode) => {
                features.push(format!("{:?}", ExecutionMode::from_u32(operands[1])))
            }
            Some(Op::Decorate) if operands.get(1) == Some(&builtin) => {
                features.push(format!("{:?}", BuiltIn::from_u32(operands[2])))
            }
            Some(Op::MemberDecorate) if operands.get(2) == Some(&builtin) => {
                features.push(format!("{:?}", BuiltIn::from_u32(operands[3])))
            }
            Some(Op::Kill) => features.push("Kill".to_owned()),
            _ => {}
        }
        pos += len.max(1);
    }
    features.sort();
    features.dedup();
    features
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

    // flags -> (count, feature combinations)
    let mut flags: BTreeMap<u32, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    let mut middle: BTreeMap<String, usize> = BTreeMap::new();
    let mut roundtrips = 0;
    let mut flags_vs_samplers: BTreeMap<String, usize> = BTreeMap::new();
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            let shader: Shader = file.object_at(info.m_PathID)?.read()?;
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
                        let program = &variant.program.code;
                        let parsed = rabex_shader::vulkan::VulkanProgram::parse(program)
                            .with_context(|| shader.m_ParsedForm.m_Name.clone())?;
                        anyhow::ensure!(
                            parsed.to_bytes()? == *program,
                            "{}: Vulkan program differs after rewriting",
                            shader.m_ParsedForm.m_Name
                        );
                        roundtrips += 1;
                        let flag = word(program, 0);
                        let slots: Vec<usize> =
                            (0..5).filter(|s| word(program, 2 + 2 * s) > 0).collect();
                        if std::env::var("SURVEY_NAMES").is_ok()
                            && (std::env::var("SURVEY_ALL").is_ok()
                                || flag & 0x40 == 0
                                || word(program, 19) != 1)
                        {
                            let (source_map, channels) = match &variant.program.layout {
                                rabex_shader::SubProgramViewLayout::V202012090 {
                                    source_map,
                                    channels,
                                    ..
                                } => (*source_map, channels.clone()),
                                _ => (0, Vec::new()),
                            };
                            println!(
                                "{flag:#x} word19 {} {} pass {:?} keywords {:?} source_map {source_map:#x} channels {channels:?} order {:?} bindings {}",
                                word(program, 19),
                                shader.m_ParsedForm.m_Name,
                                pass.m_Name,
                                variant.keywords,
                                {
                                    let mut order: Vec<usize> =
                                        (0..5).filter(|s| word(program, 2 + 2 * s) > 0).collect();
                                    order.sort_by_key(|s| word(program, 1 + 2 * s));
                                    order
                                },
                                variant
                                    .parameters
                                    .bindings
                                    .iter()
                                    .filter(|b| !matches!(b, Binding::ConstantBuffer { .. }))
                                    .count(),
                            );
                        }
                        let separate_samplers =
                            variant.parameters.bindings.iter().any(|b| match b {
                                Binding::Texture { sampler_index, .. } => *sampler_index >= 0,
                                Binding::Sampler { .. } => true,
                                _ => false,
                            });
                        *flags_vs_samplers
                            .entry(format!(
                                "bit 0x40 {} separate samplers {separate_samplers}",
                                flag & 0x40 != 0
                            ))
                            .or_default() += 1;
                        let mut stage_features = Vec::new();
                        for s in &slots {
                            let (offset, size) = (
                                word(program, 1 + 2 * s) as usize,
                                word(program, 2 + 2 * s) as usize,
                            );
                            let spirv = smolv::decode(&program[offset..offset + size])?;
                            stage_features
                                .push(format!("{s}:{}", spirv_features(&spirv).join(",")));
                        }
                        let features =
                            format!("word19 {} {}", word(program, 19), stage_features.join(" "));
                        let entry = flags.entry(flag).or_default();
                        entry.0 += 1;
                        *entry.1.entry(features).or_default() += 1;
                        let first = (0..5)
                            .filter(|s| word(program, 2 + 2 * s) > 0)
                            .map(|s| word(program, 1 + 2 * s) as usize)
                            .min()
                            .unwrap_or(program.len());
                        let unknown: Vec<u32> = (11..20).map(|i| word(program, i)).collect();
                        let trailer: Vec<u32> = (40..first / 4).map(|i| word(program, i)).collect();
                        *middle
                            .entry(format!("first stage at {first}, words 11..20 {unknown:?}, after hashes {trailer:?}"))
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    for (flag, (count, features)) in &flags {
        println!("{flag:#010x}: {count} programs");
        for (features, n) in features {
            println!("    {n:6} {features}");
        }
    }
    println!("{roundtrips} programs round trip");
    for (outcome, n) in &flags_vs_samplers {
        println!("{n:6} {outcome}");
    }
    println!();
    for (layout, n) in &middle {
        println!("{n:6} {layout}");
    }
    Ok(())
}
