//! Unity's Vulkan programs.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail, ensure};
use md5::{Digest, Md5};
use rabex_shader::dxbc::{Container, PLACEHOLDER_PREFIX, rdef};
use rabex_shader::unity::shader_param_type::ShaderParamType;
use rabex_shader::unity::texture_dimension;
use rabex_shader::unity::vertex_attribute::{self, VertexAttribute};
use rabex_shader::vulkan::{
    SLOT_FRAGMENT, SLOT_GEOMETRY, SLOT_VERTEX, STAGE_SLOTS, VulkanProgram,
    VulkanStage as VulkanStageCode, flags,
};
use rabex_shader::{
    Binding, Constant, ConstantBufferLayout, ProgramParameters, ProgramVariant, Stage,
    StructConstant, SubProgram, SubProgramLayout, SubProgramViewLayout,
};

use crate::backend::{Glslang, Hlslcc};
use crate::{Program, Translator, spirv};

pub mod compute;
pub mod shader;

/// One stage of a D3D11 program variant translated to SPIR-V for Vulkan, with what HLSLcc reports
/// about it.
#[derive(Debug)]
pub struct VulkanStage {
    pub stage: Stage,
    pub glsl: String,
    pub spirv: Vec<u32>,
    pub reflection: Vec<hlslcc::Reflection>,
}

/// The order Unity translates a program's stages in, which decides the bindings HLSLcc assigns.
// TODO: Unity numbers textures before samplers, HLSLcc samplers first; reproduce Unity's order
pub const TRANSLATION_ORDER: [Stage; 5] = [
    Stage::Fragment,
    Stage::Vertex,
    Stage::Geometry,
    Stage::Hull,
    Stage::Domain,
];

/// The stage variants of a pass making up its Vulkan variant with `keywords`: per stage the
/// variant with the most keywords, all of them in `keywords`. Stages can use fewer keywords than
/// the pass, like with `shader_feature_local_fragment`.
pub fn select_stages<'a, 'b, I>(
    keywords: &[String],
    variants: I,
) -> Result<Vec<&'a ProgramVariant<'b>>>
where
    I: IntoIterator<Item = &'a ProgramVariant<'b>>,
{
    let variants: Vec<&ProgramVariant> = variants.into_iter().collect();
    let mut stages: Vec<Stage> = variants.iter().map(|v| v.stage).collect();
    stages.sort_by_key(|&s| TRANSLATION_ORDER.iter().position(|&o| o == s));
    stages.dedup();
    stages
        .into_iter()
        .map(|stage| {
            let candidates: Vec<&ProgramVariant> = variants
                .iter()
                .copied()
                .filter(|v| v.stage == stage && v.keywords.iter().all(|k| keywords.contains(k)))
                .collect();
            let most = candidates
                .iter()
                .map(|v| v.keywords.len())
                .max()
                .with_context(|| format!("no {stage:?} variant within {keywords:?}"))?;
            let mut best = candidates.into_iter().filter(|v| v.keywords.len() == most);
            let chosen = best.next().unwrap();
            ensure!(
                best.next().is_none(),
                "several {stage:?} variants fit {keywords:?} equally well"
            );
            Ok(chosen)
        })
        .collect()
}

/// The semantic of an input fed by a Unity vertex channel (`VertexAttribute`).
fn channel_semantic(channel: VertexAttribute) -> Result<(&'static str, u32)> {
    Ok(match channel {
        VertexAttribute::Position => ("POSITION", 0),
        VertexAttribute::Normal => ("NORMAL", 0),
        VertexAttribute::Tangent => ("TANGENT", 0),
        VertexAttribute::Color => ("COLOR", 0),
        VertexAttribute::TexCoord0
        | VertexAttribute::TexCoord1
        | VertexAttribute::TexCoord2
        | VertexAttribute::TexCoord3
        | VertexAttribute::TexCoord4
        | VertexAttribute::TexCoord5
        | VertexAttribute::TexCoord6
        | VertexAttribute::TexCoord7 => ("TEXCOORD", channel.value() - vertex_attribute::TexCoord0),
        // HLSLcc gives these inputs no semantic; nothing could feed them
        VertexAttribute::BlendWeight | VertexAttribute::BlendIndices => {
            bail!("no semantic for vertex channel {channel}")
        }
    })
}

/// The vertex channels of the Vulkan program built from a D3D11 vertex stage: each channel goes
/// to 13 plus the location HLSLcc gave its input. Channels whose input the code doesn't read have
/// no location and are left out, like Unity does.
pub fn channels<'a>(
    vertex: &ProgramVariant<'a>,
    translated: &VulkanStage,
) -> Result<Vec<(u32, u32)>> {
    ensure!(
        vertex.stage == Stage::Vertex && translated.stage == Stage::Vertex,
        "channels of a {:?} stage",
        vertex.stage
    );
    // TODO: Unity sometimes keeps a channel whose input the D3D11 code doesn't read
    let mut channels = Vec::new();
    for reflection in &translated.reflection {
        let hlslcc::Reflection::Input { name, bind_index } = reflection else {
            continue;
        };
        let mut source = None;
        let SubProgramViewLayout::V202012090 {
            channels: program_channels,
            ..
        } = &vertex.program.layout
        else {
            bail!("vertex program not in the 202012090 layout")
        };
        for &(channel, _) in program_channels {
            let channel = VertexAttribute::try_from(i64::from(channel))
                .with_context(|| format!("vertex channel {channel}"))?;
            let (semantic, index) = channel_semantic(channel)?;
            if name.eq_ignore_ascii_case(&format!("in_{semantic}{index}")) {
                source = Some(channel);
            }
        }
        let source = source.with_context(|| format!("input {name} is fed by no vertex channel"))?;
        channels.push((source.value(), 13 + u32::try_from(*bind_index)?));
    }
    Ok(channels)
}

/// Translate the D3D11 stages of one pass variant to SPIR-V the way Unity builds its Vulkan
/// programs: each stage's DXBC with its rebuilt `RDEF` through HLSLcc to Vulkan GLSL, sharing
/// bindings and varying locations between the stages, compiled by glslang.
/// Negate `gl_Position.y` in the last stage before rasterization, like Unity's Vulkan programs:
/// around every vertex a geometry stage emits, otherwise after `main`.
fn flip_clip_space_y(stage: Stage, glsl: &str) -> Result<String> {
    const FLIP: &str = "gl_Position.y = -gl_Position.y;";
    if stage == Stage::Geometry {
        // TODO: multiple streams, once data with EmitStreamVertex turns up
        ensure!(
            !glsl.contains("EmitStreamVertex("),
            "geometry stage with multiple streams"
        );
        ensure!(
            glsl.contains("EmitVertex();"),
            "geometry stage without EmitVertex"
        );
        return Ok(glsl.replace("EmitVertex();", &format!("{FLIP} EmitVertex(); {FLIP}")));
    }
    const MAIN: &str = "void main()\n{";
    ensure!(
        glsl.matches(MAIN).count() == 1,
        "{stage:?} stage without exactly one main"
    );
    Ok(format!(
        "{}\nvoid main()\n{{\n    rabex_main();\n    {FLIP}\n}}\n",
        glsl.replace(MAIN, "void rabex_main()\n{")
    ))
}

pub fn from_d3d11<'a>(stages: &[&ProgramVariant<'a>]) -> Result<Vec<VulkanStage>> {
    let mut ordered = stages.to_vec();
    ordered.sort_by_key(|v| TRANSLATION_ORDER.iter().position(|&s| s == v.stage));
    for pair in ordered.windows(2) {
        ensure!(
            pair[0].stage != pair[1].stage,
            "two {:?} stages",
            pair[0].stage
        );
    }
    let rasterized = [Stage::Geometry, Stage::Domain, Stage::Vertex]
        .into_iter()
        .find(|s| ordered.iter().any(|v| v.stage == *s));
    let mut dependencies = hlslcc::Dependencies::new();
    ordered
        .into_iter()
        .map(|variant| {
            ensure!(
                TRANSLATION_ORDER.contains(&variant.stage),
                "no Vulkan stage for {:?}",
                variant.stage
            );
            let mut container = Container::parse(variant.program.dxbc()?)?;
            container.set_rdef(rdef(&variant.parameters, &container)?);
            let mut translation = Hlslcc::vulkan()
                .translate_with_reflection(&container.to_bytes()?, &mut dependencies)
                .with_context(|| format!("HLSLcc {:?}", variant.stage))?;
            if Some(variant.stage) == rasterized {
                translation.source = flip_clip_space_y(variant.stage, &translation.source)?;
            }
            let spirv = match Glslang::vulkan()
                .translate(variant.stage, Program::Glsl(translation.source.clone()))?
            {
                Program::Spirv(words) => words,
                other => bail!("glslang produced {other:?}"),
            };
            Ok(VulkanStage {
                stage: variant.stage,
                glsl: translation.source,
                spirv,
                reflection: translation.reflection,
            })
        })
        .collect()
}

fn stage_bit(stage: Stage) -> Result<u32> {
    Ok(match stage {
        Stage::Vertex => 0x04,
        Stage::Fragment => 0x08,
        Stage::Hull => 0x10,
        Stage::Domain => 0x20,
        Stage::Geometry => 0x40,
        Stage::RayTracing => bail!("no Vulkan stage bit for ray tracing"),
        Stage::Compute => bail!("compute kernels bind without stage bits"),
    })
}

/// Unity's Vulkan binding index: the stages using the resource, its descriptor set and binding.
fn binding_index(mask: u32, set: u32, binding: u32) -> Result<i32> {
    ensure!(
        set < 0x100 && binding < 0x1_0000,
        "set {set} or binding {binding} out of range"
    );
    Ok((mask << 24 | set << 16 | binding) as i32)
}

/// `GpuProgramType` of Vulkan programs.
const VULKAN_PROGRAM_TYPE: u32 = 25;

/// Blocks the engine sizes above what the members the program accesses end at: the draw
/// call info holds a second field the parameters don't list, so Unity's own Vulkan
/// programs measure eight bytes for the one int the shaders read.
const ENGINE_MINIMUM_BLOCK_SIZES: &[(&str, u32)] = &[("UnityDrawCallInfo", 8)];

// TODO: confirm with more data that Unity marks UAV buffers with this mask bit
const UAV_BUFFER_MASK_BIT: u32 = 0x02;

fn data_type(scalar: spirv::Scalar) -> ShaderParamType {
    match scalar {
        spirv::Scalar::Float => ShaderParamType::Float,
        spirv::Scalar::Int => ShaderParamType::Int,
        spirv::Scalar::Bool => ShaderParamType::Bool,
        spirv::Scalar::Uint => ShaderParamType::UInt,
    }
}

fn constant(member: &spirv::Member) -> Result<Constant> {
    let (ty, array_size) = match &member.ty {
        spirv::Type::Array {
            element, length, ..
        } => (element.as_ref(), *length),
        other => (other, 0),
    };
    let (scalar, rows, columns, is_matrix) = match ty {
        spirv::Type::Scalar(s) => (*s, 1, 1, false),
        spirv::Type::Vector(s, n) => (*s, 1, *n, false),
        spirv::Type::Matrix {
            scalar,
            columns,
            rows,
            ..
        } => (*scalar, *rows, *columns, true),
        other => bail!("{}: no Unity constant for {other:?}", member.name),
    };
    Ok(Constant {
        name: member.name.clone(),
        data_type: data_type(scalar).value(),
        rows,
        columns: Some(columns),
        is_matrix,
        array_size,
        offset: member.offset,
    })
}

/// The layout of a uniform buffer's members, up to the end of the last one the code accesses.
fn constant_buffer(
    name: &str,
    members: &[spirv::Member],
    accessed: &BTreeSet<String>,
) -> Result<ConstantBufferLayout> {
    let mut constants = Vec::new();
    let mut structs = Vec::new();
    let described = members
        .iter()
        .filter(|m| !m.name.starts_with(PLACEHOLDER_PREFIX));
    let size = members
        .iter()
        .filter(|m| accessed.contains(&m.name))
        .map(|m| m.offset + spirv::size(&m.ty))
        .max()
        .unwrap_or(0);
    for member in described {
        let (fields, array_size, struct_size) = match &member.ty {
            spirv::Type::Struct(fields) => (Some(fields), 0, spirv::size(&member.ty)),
            spirv::Type::Array {
                element,
                length,
                stride,
            } => match element.as_ref() {
                spirv::Type::Struct(fields) => (Some(fields), *length, *stride),
                _ => (None, 0, 0),
            },
            _ => (None, 0, 0),
        };
        match fields {
            Some(fields) => structs.push(StructConstant {
                name: member.name.clone(),
                offset: member.offset,
                array_size,
                struct_size,
                members: fields.iter().map(constant).collect::<Result<_>>()?,
            }),
            None => constants.push(constant(member)?),
        }
    }
    Ok(ConstantBufferLayout {
        name: name.to_owned(),
        size,
        constants,
        structs,
    })
}

fn texture_dimension(image: &spirv::Image) -> Result<u32> {
    Ok(match (image.dim, image.arrayed) {
        (spirv::Dim::D2, false) => texture_dimension::Tex2D,
        (spirv::Dim::D3, false) => texture_dimension::Tex3D,
        (spirv::Dim::Cube, false) => texture_dimension::Cube,
        (spirv::Dim::D2, true) => texture_dimension::Tex2DArray,
        (spirv::Dim::Cube, true) => texture_dimension::CubeArray,
        other => bail!("no Unity texture dimension for {other:?}"),
    })
}

/// The register of a resource HLSLcc named after its D3D11 register, like `u2_origX2X` or `samplers4`.
fn register_in_name(name: &str, prefix: &str) -> Option<i32> {
    let digits: String = name
        .strip_prefix(prefix)?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Stages declare the members of a shared uniform buffer they use; the program's layout has them all.
fn merge_kinds(
    name: &str,
    existing: &mut spirv::ResourceKind,
    other: spirv::ResourceKind,
) -> Result<()> {
    match (existing, other) {
        (
            spirv::ResourceKind::UniformBuffer {
                size,
                members,
                accessed,
            },
            spirv::ResourceKind::UniformBuffer {
                size: other_size,
                members: other_members,
                accessed: other_accessed,
            },
        ) => {
            for member in other_members {
                match members.iter().find(|m| m.name == member.name) {
                    Some(m) => ensure!(
                        *m == member,
                        "{name}.{}: differs between stages, {m:?} and {member:?}",
                        member.name
                    ),
                    None => members.push(member),
                }
            }
            members.sort_by_key(|m| m.offset);
            *size = (*size).max(other_size);
            accessed.extend(other_accessed);
            Ok(())
        }
        (existing, other) => {
            ensure!(*existing == other, "{name}: differs between stages");
            Ok(())
        }
    }
}

/// The parameters of the Vulkan program built from `stages`, translated from the D3D11 stage
/// variants `d3d11`, describing the translated SPIR-V's resources the way Unity's do.
pub fn parameters<'a>(
    stages: &[VulkanStage],
    d3d11: &[&ProgramVariant<'a>],
) -> Result<ProgramParameters> {
    let mut resources: Vec<(spirv::Resource, u32)> = Vec::new();
    for stage in stages {
        let bit = stage_bit(stage.stage)?;
        for resource in spirv::resources(&stage.spirv)? {
            match resources
                .iter_mut()
                .find(|(r, _)| (r.set, r.binding) == (resource.set, resource.binding))
            {
                Some((existing, mask)) => {
                    ensure!(
                        existing.name == resource.name,
                        "set {} binding {} is {} in one stage and {} in another",
                        resource.set,
                        resource.binding,
                        existing.name,
                        resource.name
                    );
                    merge_kinds(&existing.name, &mut existing.kind, resource.kind)?;
                    *mask |= bit;
                }
                None => resources.push((resource, bit)),
            }
        }
    }
    let d3d11_bindings = || d3d11.iter().flat_map(|v| &v.parameters.bindings);
    let index_of = |name: &str| {
        resources
            .iter()
            .find(|(r, _)| r.name == name)
            .map(|(r, mask)| binding_index(*mask, r.set, r.binding))
    };

    // specialization constants, with the stages using them and their SpecId as offset, like Unity's
    let mut spec_constants: Vec<(spirv::SpecConstant, u32)> = Vec::new();
    for stage in stages {
        let bit = stage_bit(stage.stage)?;
        for constant in spirv::spec_constants(&stage.spirv)? {
            match spec_constants
                .iter_mut()
                .find(|(c, _)| c.spec_id == constant.spec_id)
            {
                Some((existing, mask)) => {
                    ensure!(
                        *existing == constant,
                        "{}: differs between stages",
                        constant.name
                    );
                    *mask |= bit;
                }
                None => spec_constants.push((constant, bit)),
            }
        }
    }
    let mut constant_buffers = vec![ConstantBufferLayout {
        name: String::new(),
        size: 0,
        constants: spec_constants
            .iter()
            .map(|(c, mask)| Constant {
                name: c.name.clone(),
                data_type: data_type(c.scalar).value(),
                rows: 1,
                columns: Some(1),
                is_matrix: false,
                array_size: 0,
                offset: mask << 24 | c.spec_id,
            })
            .collect(),
        structs: Vec::new(),
    }];
    let mut bindings = Vec::new();
    // Every uniform block is announced at the binding its module reads it from. Unity's own
    // translator folds the engine's builtin buffers (UnityPerDraw, UnityPerFrame) into the
    // stage's Globals block, in the module as well; announcing them folded while the
    // module reads them on their own bindings leaves those unbound, and the reads fault.
    // Announced on their own, by name, they are what the D3D11 programs of the same
    // shader announce, which the player fills.
    for (resource, mask) in &resources {
        let index = binding_index(*mask, resource.set, resource.binding)?;
        let name = resource.name.clone();
        match &resource.kind {
            spirv::ResourceKind::UniformBuffer {
                members, accessed, ..
            } => {
                constant_buffers.push(constant_buffer(&name, members, accessed)?);
                bindings.push(Binding::ConstantBuffer {
                    name,
                    index,
                    array_size: 0,
                });
            }
            spirv::ResourceKind::Texture { image, combined } => {
                let sampler_index = match combined {
                    true => -1,
                    false => index_of(&format!("sampler{name}"))
                        .transpose()?
                        .unwrap_or(-1),
                };
                bindings.push(Binding::Texture {
                    dimension: texture_dimension(image)?,
                    multisampled: image.multisampled,
                    name,
                    index,
                    sampler_index,
                });
            }
            spirv::ResourceKind::Sampler => {
                let paired = resources.iter().any(|(r, _)| {
                    matches!(r.kind, spirv::ResourceKind::Texture { .. })
                        && format!("sampler{}", r.name) == name
                });
                if paired {
                    continue;
                }
                let register = register_in_name(&name, "samplers").with_context(|| {
                    format!("sampler {name} pairs with no texture and names no D3D11 register")
                })?;
                let state = d3d11_bindings()
                    .find_map(|b| match b {
                        Binding::Sampler { index, state, .. } if *index == register => Some(*state),
                        _ => None,
                    })
                    .with_context(|| format!("no D3D11 inline sampler at s{register}"))?;
                bindings.push(Binding::Sampler {
                    name: String::new(),
                    index,
                    state,
                });
            }
            spirv::ResourceKind::StorageBuffer { writable: false } => {
                bindings.push(Binding::Buffer {
                    name,
                    index,
                    array_size: 1,
                });
            }
            spirv::ResourceKind::StorageBuffer { writable: true }
            | spirv::ResourceKind::StorageImage(_) => {
                let original_index = match register_in_name(&name, "u") {
                    Some(register) => register,
                    None => d3d11_bindings()
                        .find_map(|b| match b {
                            Binding::Uav {
                                name: n,
                                original_index,
                                ..
                            } if *n == name => Some(*original_index as i32),
                            _ => None,
                        })
                        .with_context(|| format!("UAV {name} has no D3D11 register"))?,
                };
                let index = match resource.kind {
                    spirv::ResourceKind::StorageBuffer { .. } => {
                        binding_index(*mask | UAV_BUFFER_MASK_BIT, resource.set, resource.binding)?
                    }
                    _ => index,
                };
                bindings.push(Binding::Uav {
                    name,
                    index,
                    original_index: u32::try_from(original_index)?,
                });
            }
        }
    }
    for cb in &mut constant_buffers {
        if let Some(&(_, minimum)) = ENGINE_MINIMUM_BLOCK_SIZES
            .iter()
            .find(|(name, _)| *name == cb.name)
        {
            cb.size = cb.size.max(minimum);
        }
    }
    Ok(ProgramParameters {
        constant_buffers,
        bindings,
    })
}

fn slot(stage: Stage) -> Result<usize> {
    Ok(match stage {
        Stage::Vertex => SLOT_VERTEX,
        Stage::Fragment => SLOT_FRAGMENT,
        Stage::Geometry => SLOT_GEOMETRY,
        other => bail!("no Vulkan program slot known for {other:?}"),
    })
}

/// The Vulkan program of a pass variant: Unity's container around the SMOL-V encoded stages, with
/// the keywords and vertex channels of the D3D11 stage variants it was translated from.
pub fn program<'a>(stages: &[VulkanStage], d3d11: &[&ProgramVariant<'a>]) -> Result<SubProgram> {
    let resources = stages
        .iter()
        .map(|s| spirv::resources(&s.spirv))
        .collect::<Result<Vec<_>>>()?;
    let any = |f: fn(&spirv::ResourceKind) -> bool| resources.iter().flatten().any(|r| f(&r.kind));
    let mut program_flags = flags::HIGH_SEEN | flags::ALWAYS_SEEN;
    if !any(|k| matches!(k, spirv::ResourceKind::Sampler)) {
        program_flags |= flags::COMBINED_SAMPLERS_ONLY;
    }
    // TODO: confirm with more data that UAVs set this bit
    if any(|k| {
        matches!(
            k,
            spirv::ResourceKind::StorageBuffer { writable: true }
                | spirv::ResourceKind::StorageImage(_)
        )
    }) {
        program_flags |= flags::UAV_SEEN;
    }
    for stage in stages {
        if stage.stage == Stage::Fragment && spirv::has_kill(&stage.spirv)? {
            program_flags |= flags::DISCARD;
        }
    }

    let mut slots: [Option<VulkanStageCode>; STAGE_SLOTS] = Default::default();
    for stage in stages {
        let slot = slot(stage.stage)?;
        ensure!(slots[slot].is_none(), "two {:?} stages", stage.stage);
        let bytes: Vec<u8> = stage.spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
        slots[slot] = Some(VulkanStageCode {
            // TODO: Unity's hash algorithm is unknown; MD5 of the SPIR-V stands in
            hash: Md5::digest(&bytes).into(),
            smolv: smolv::encode(&stage.spirv)?,
        });
    }
    let container = VulkanProgram {
        flags: program_flags,
        // the translation targets the form the 2022.3 and 6000 players read
        form: rabex_shader::vulkan::Form::Hashed,
        // TODO: find what the last word, 1 or 15 in Unity's programs, means
        unknown: [0, 0, 0, 0, 0, 0, 0, 0, 1],
        stages: slots,
        trailer: [0; 4],
    };

    let mut keywords: Vec<String> = d3d11
        .iter()
        .flat_map(|v| v.keywords.iter().cloned())
        .collect();
    keywords.sort();
    keywords.dedup();
    let (source_map, channels) = match d3d11.iter().find(|v| v.stage == Stage::Vertex) {
        Some(vertex) => {
            let translated = stages
                .iter()
                .find(|s| s.stage == Stage::Vertex)
                .context("vertex stage wasn't translated")?;
            let SubProgramViewLayout::V202012090 { source_map, .. } = vertex.program.layout else {
                bail!("vertex program not in the 202012090 layout")
            };
            (source_map, channels(vertex, translated)?)
        }
        None => (0, Vec::new()),
    };
    Ok(SubProgram {
        program_type: VULKAN_PROGRAM_TYPE,
        stats: vec![0; 4],
        keywords,
        code: container.to_bytes()?,
        layout: SubProgramLayout::V202012090 {
            source_map,
            channels,
        },
    })
}
