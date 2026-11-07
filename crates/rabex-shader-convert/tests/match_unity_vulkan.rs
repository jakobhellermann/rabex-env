//! D3D11 pass variants of the fixtures translated to Vulkan match Unity's own Vulkan programs of
//! the same pass variants: stage count, discard flag, vertex channels and constant buffer
//! bindings exactly; textures, buffers and UAVs in stage mask and descriptor set.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use rabex_shader::vulkan::{SLOT_FRAGMENT, SLOT_GEOMETRY, SLOT_VERTEX, VulkanProgram, flags};
use rabex_shader::{
    Binding, Constant, ConstantBufferLayout, Platform, ProgramParameters, ProgramVariant, Stage,
};
use rabex_shader_convert::spirv;
use rabex_shader_convert::target::vulkan;

mod common;
use common::{fixture_dirs, shaders};

/// Variable or block name to `(set, binding)`, and whether the module contains `OpKill`.
fn inspect(words: &[u32]) -> (BTreeMap<String, (u32, u32)>, bool) {
    let bindings = spirv::resources(words)
        .unwrap()
        .into_iter()
        .map(|r| (r.name, (r.set, r.binding)))
        .collect();
    (bindings, spirv::has_kill(words).unwrap())
}

fn negates(words: &[u32]) -> usize {
    let mut count = 0;
    let mut pos = 5;
    while pos < words.len() {
        count += usize::from(words[pos] & 0xffff == ::spirv::Op::FNegate as u32);
        pos += ((words[pos] >> 16) as usize).max(1);
    }
    count
}

fn stage_bit(stage: Stage) -> u32 {
    match stage {
        Stage::Vertex => 0x04,
        Stage::Fragment => 0x08,
        Stage::Geometry => 0x40,
        other => panic!("no {other:?} stages in the fixtures"),
    }
}

/// Unity suffixes a stage's globals with a hash, like `VGlobals2840191130`.
fn normalize(name: &str) -> String {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.len() == 8 && trimmed.ends_with("Globals") {
        trimmed.to_owned()
    } else {
        name.to_owned()
    }
}

fn unpack(index: i32) -> (u32, u32, u32) {
    let index = index as u32;
    (index >> 24, (index >> 16) & 0xff, index & 0xffff)
}

fn check(
    label: &str,
    stages: &[&ProgramVariant],
    unity: &ProgramVariant,
    failures: &mut Vec<String>,
) -> Result<()> {
    let translated = vulkan::from_d3d11(stages)?;
    let program = VulkanProgram::parse(unity.program.code)?;
    let mut ours: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
    let mut kills = false;
    for stage in &translated {
        let (bindings, stage_kills) = inspect(&stage.spirv);
        kills |= stage.stage == Stage::Fragment && stage_kills;
        for (name, (set, binding)) in bindings {
            ours.entry(name).or_insert((0, set, binding)).0 |= stage_bit(stage.stage);
        }
    }

    // pins the clip space y flip of the last stage before rasterization
    for stage in &translated {
        let slot = match stage.stage {
            Stage::Vertex => SLOT_VERTEX,
            Stage::Fragment => SLOT_FRAGMENT,
            Stage::Geometry => SLOT_GEOMETRY,
            other => panic!("no {other:?} stages in the fixtures"),
        };
        let Some(unity_stage) = &program.stages[slot] else {
            continue;
        };
        let unity_negates = negates(&smolv::decode(&unity_stage.smolv)?);
        if negates(&stage.spirv) != unity_negates {
            failures.push(format!(
                "{label}: {:?} has {} OpFNegate, Unity {unity_negates}",
                stage.stage,
                negates(&stage.spirv)
            ));
        }
    }

    let unity_stages = program.stages.iter().filter(|s| s.is_some()).count();
    if unity_stages != translated.len() {
        failures.push(format!(
            "{label}: {} stages, Unity {unity_stages}",
            translated.len()
        ));
    }
    if (program.flags & flags::DISCARD != 0) != kills {
        failures.push(format!(
            "{label}: discard flag {:#x} but OpKill {kills}",
            program.flags
        ));
    }
    let vertex = stages
        .iter()
        .find(|s| s.stage == Stage::Vertex)
        .context("no vertex stage")?;
    let translated_vertex = translated
        .iter()
        .find(|t| t.stage == Stage::Vertex)
        .context("no vertex stage")?;
    // TODO: Unity orders channels differently in one fixture, maybe by first use in the code
    let mut channels = vulkan::channels(vertex, translated_vertex)?;
    channels.sort();
    let mut unity_channels = unity
        .program
        .layout
        .source_map_and_channels()
        .expect("Unity's Vulkan programs are in the 202012090 layout")
        .1
        .to_vec();
    unity_channels.sort();
    if channels != unity_channels {
        failures.push(format!(
            "{label}: channels {channels:?}, Unity {unity_channels:?}"
        ));
    }

    let ours_program = vulkan::program(&translated, stages)?;
    let ours_fields = (
        ours_program.program_type,
        ours_program.stats.clone(),
        ours_program.keywords.clone(),
        ours_program
            .layout
            .source_map_and_channels()
            .expect("the Vulkan target builds 202012090 entries")
            .0,
    );
    let theirs_fields = (
        unity.program.program_type,
        unity.program.stats.clone(),
        unity.program.keywords.clone(),
        unity
            .program
            .layout
            .source_map_and_channels()
            .expect("Unity's Vulkan programs are in the 202012090 layout")
            .0,
    );
    if ours_fields != theirs_fields {
        failures.push(format!(
            "{label}: program {ours_fields:?}, Unity {theirs_fields:?}"
        ));
    }
    let ours_container = VulkanProgram::parse(&ours_program.code)?;
    let slots = |c: &VulkanProgram| c.stages.iter().map(Option::is_some).collect::<Vec<_>>();
    let layout = |c: &VulkanProgram| (slots(c), c.unknown, c.trailer);
    if layout(&ours_container) != layout(&program) {
        failures.push(format!(
            "{label}: container {:?}, Unity {:?}",
            layout(&ours_container),
            layout(&program)
        ));
    }
    // ours samples through separate samplers where Unity combines, so that bit follows our SPIR-V
    let separate = translated.iter().any(|t| {
        rabex_shader_convert::spirv::resources(&t.spirv).is_ok_and(|r| {
            r.iter()
                .any(|r| r.kind == rabex_shader_convert::spirv::ResourceKind::Sampler)
        })
    });
    let combined_bit = flags::COMBINED_SAMPLERS_ONLY;
    if (ours_container.flags & combined_bit == 0) != separate
        || ours_container.flags & !combined_bit != program.flags & !combined_bit
    {
        failures.push(format!(
            "{label}: flags {:#x}, Unity {:#x}, our separate samplers {separate}",
            ours_container.flags, program.flags
        ));
    }
    for stage in ours_container.stages.iter().flatten() {
        let decoded = smolv::decode(&stage.smolv)?;
        if !translated.iter().any(|t| t.spirv == decoded) {
            failures.push(format!("{label}: a stage doesn't decode to its SPIR-V"));
        }
    }

    let ours_parameters = vulkan::parameters(&translated, stages)?;
    compare_parameters(label, &ours_parameters, &unity.parameters, failures);

    for binding in &unity.parameters.bindings {
        let (key, index, exact) = match binding {
            Binding::ConstantBuffer { name, index, .. } => (normalize(name), *index, true),
            Binding::Texture { name, index, .. } | Binding::Buffer { name, index, .. } => {
                (name.clone(), *index, false)
            }
            Binding::Uav {
                original_index,
                index,
                ..
            } => {
                let key = ours
                    .keys()
                    .find(|k| k.starts_with(&format!("u{original_index}_")))
                    .cloned();
                (key.unwrap_or_default(), *index, false)
            }
            Binding::Sampler { .. } => continue,
        };
        let theirs = unpack(index);
        // FOLDED_BUILTINS: a Globals block Unity folded the builtin buffers into has no
        // binding of its own in ours when it held nothing else; the builtin buffers ours
        // announces instead are where their modules read them, which the bindings check
        // of the conversion holds them to
        if matches!(key.as_str(), "VGlobals" | "PGlobals")
            && !ours.contains_key(&key)
            && ours.keys().any(|k| BUILTIN_BUFFERS.contains(&k.as_str()))
        {
            continue;
        }
        let ours = ours.get(&key).copied();
        let matches = match ours {
            Some(found) if exact => found == theirs,
            // TODO: confirm with more data that Unity marks UAV buffers with mask bit 0x02
            Some((mask, set, _)) => (mask, set) == (theirs.0 & !0x02, theirs.1),
            None => false,
        };
        if !matches {
            failures.push(format!("{label} {key:?}: ours {ours:?}, Unity {theirs:?}"));
        }
    }
    Ok(())
}

type ConstantKey = (String, u32, u32, Option<u32>, bool, u32, u32);

fn constants(constants: &[Constant]) -> Vec<ConstantKey> {
    // matrices from m_CommonParameters have no column count, so it isn't compared
    let mut keys: Vec<ConstantKey> = constants
        .iter()
        .map(|c| {
            let columns = if c.is_matrix { None } else { c.columns };
            (
                c.name.clone(),
                c.data_type,
                c.rows,
                columns,
                c.is_matrix,
                c.array_size,
                c.offset,
            )
        })
        .collect();
    keys.sort();
    keys
}

/// A constant buffer's comparable form: its size, its constants and its structs.
type Layout = (
    u32,
    Vec<ConstantKey>,
    Vec<(String, u32, u32, u32, Vec<ConstantKey>)>,
);

fn layout(cb: &ConstantBufferLayout) -> Layout {
    let mut structs: Vec<_> = cb
        .structs
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.offset,
                s.array_size,
                s.struct_size,
                constants(&s.members),
            )
        })
        .collect();
    structs.sort();
    (cb.size, constants(&cb.constants), structs)
}

/// The engine's builtin constant buffers. A named difference to Unity's translation
/// (FOLDED_BUILTINS): Unity folds them into the stage's `VGlobals`/`PGlobals` block, in the
/// module as in the parameters; ours keeps each as the block the D3D11 program has, at its
/// own binding, announced by name the way the D3D11 programs of the same shader announce
/// them. Folding them in the parameters alone, as ours once did, left the module reading
/// bindings the player never bound: a GPU page fault.
const BUILTIN_BUFFERS: &[&str] = &["UnityPerDraw", "UnityPerFrame"];

/// The constants of a Globals block without their offsets, which folding relayouts.
fn members(constants: &[Constant]) -> Vec<ConstantKey> {
    let mut keys: Vec<ConstantKey> = self::constants(constants)
        .into_iter()
        .map(|mut k| {
            k.6 = 0;
            k
        })
        .collect();
    keys.sort();
    keys
}

/// Compare what both describe, with names normalized and binding indices left to the binding check.
fn compare_parameters(
    label: &str,
    ours: &ProgramParameters,
    unity: &ProgramParameters,
    failures: &mut Vec<String>,
) {
    for cb in unity.constant_buffers.iter().skip(1) {
        let name = normalize(&cb.name);
        if matches!(name.as_str(), "VGlobals" | "PGlobals") {
            // FOLDED_BUILTINS: every constant of Unity's block is in ours' block of that
            // name or in one of the builtin buffers, with its type
            let ours_members: Vec<ConstantKey> = ours
                .constant_buffers
                .iter()
                .filter(|c| c.name == name || BUILTIN_BUFFERS.contains(&c.name.as_str()))
                .flat_map(|c| members(&c.constants))
                .collect();
            for constant in members(&cb.constants) {
                if !ours_members.contains(&constant) {
                    failures.push(format!("{label} {name}: {constant:?} in no block of ours"));
                }
            }
            continue;
        }
        match ours.constant_buffers.iter().find(|c| c.name == name) {
            Some(found) if layout(found) == layout(cb) => {}
            Some(found) => failures.push(format!(
                "{label} {name}: layout\n      ours  {:?}\n      unity {:?}",
                layout(found),
                layout(cb)
            )),
            None => failures.push(format!("{label} {name}: constant buffer missing in ours")),
        }
    }
    let textures = |p: &ProgramParameters| {
        let mut t: Vec<(String, u32, bool)> = p
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Texture {
                    name,
                    dimension,
                    multisampled,
                    ..
                } => Some((name.clone(), *dimension, *multisampled)),
                _ => None,
            })
            .collect();
        t.sort();
        t
    };
    if textures(ours) != textures(unity) {
        failures.push(format!(
            "{label}: textures {:?}, Unity {:?}",
            textures(ours),
            textures(unity)
        ));
    }
    let uavs = |p: &ProgramParameters| {
        let mut u: Vec<u32> = p
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Uav { original_index, .. } => Some(*original_index),
                _ => None,
            })
            .collect();
        u.sort();
        u
    };
    if uavs(ours) != uavs(unity) {
        failures.push(format!(
            "{label}: UAV registers {:?}, Unity {:?}",
            uavs(ours),
            uavs(unity)
        ));
    }
    let inline_samplers = |p: &ProgramParameters| {
        let mut s: Vec<u32> = p
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Sampler { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        s.sort();
        s
    };
    let ours_samplers = inline_samplers(ours);
    if ours_samplers != inline_samplers(unity) {
        failures.push(format!(
            "{label}: inline sampler states {:?}, Unity {:?}",
            ours_samplers,
            inline_samplers(unity)
        ));
    }
}

#[test]
fn translated_variants_match_unity_vulkan() -> Result<()> {
    let mut failures = Vec::new();
    let mut checked = 0;
    for dir in fixture_dirs()? {
        let label_version = dir.file_name().unwrap().to_string_lossy().into_owned();
        // Variant enumeration needs keyword names, which Unity 2020 shaders don't carry
        if rabex::UnityVersion::from_str(&label_version).unwrap()
            < rabex::UnityVersion::new(2022, 3, 0)
        {
            continue;
        }
        let linux = shaders(&dir.join("shaders"))?;
        for shader in shaders(&dir.join("StandaloneWindows64/shaders"))? {
            let name = &shader.m_ParsedForm.m_Name;
            let unity = linux
                .iter()
                .find(|s| &s.m_ParsedForm.m_Name == name)
                .context("no Linux shader")?;
            let d3d = shader
                .platform_blobs()?
                .into_iter()
                .next()
                .context("no D3D11 blob")?;
            let vk = unity
                .platform_blobs()?
                .into_iter()
                .find(|b| b.platform == Platform::Vulkan)
                .context("no Vulkan blob")?;
            let passes = shader.m_ParsedForm.m_SubShaders[0].m_Passes.iter();
            for (p, (pass, unity_pass)) in passes
                .zip(&unity.m_ParsedForm.m_SubShaders[0].m_Passes)
                .enumerate()
            {
                let variants = shader.pass_variants(pass, &d3d)?;
                for unity_variant in unity.pass_variants(unity_pass, &vk)? {
                    let label = format!(
                        "{label_version} {name} pass {p} {:?}",
                        unity_variant.keywords
                    );
                    let stages = vulkan::select_stages(&unity_variant.keywords, &variants)?;
                    check(&label, &stages, &unity_variant, &mut failures)?;
                    checked += 1;
                }
            }
        }
    }
    ensure!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // the instancing shader adds its two variants
    ensure!(
        checked == 34,
        "checked {checked} pass variants, the fixtures have 17 per Unity version"
    );
    Ok(())
}
