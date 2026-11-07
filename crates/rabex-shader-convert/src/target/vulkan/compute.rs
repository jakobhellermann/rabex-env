//! Replacing a compute shader's D3D11 kernels with Vulkan kernels, described like Unity's Linux
//! builds describe them.

use anyhow::{Context, Result, bail, ensure};
use rabex::UnityVersion;
use rabex_shader::Stage;
use rabex_shader::compute_types::{
    ComputeShader, ComputeShaderBuiltinSampler, ComputeShaderCB, ComputeShaderKernel,
    ComputeShaderKernelParent, ComputeShaderParam, ComputeShaderPlatformVariant,
    ComputeShaderResource,
};
use rabex_shader::dxbc::{Container, PLACEHOLDER_PREFIX, rdef};
use rabex_shader::unity::graphics_device_type;
use rabex_shader::unity::shader_param_type::{self, ShaderParamType};

use super::{data_type, texture_dimension};
use crate::backend::{Glslang, Hlslcc};
use crate::{Program, Translator, spirv};

/// Flags of Unity's Vulkan `bindPoint`s besides set and binding, as observed in its Linux builds.
// TODO: what the bits mean individually; only their combinations below were observed
#[derive(Clone, Copy)]
struct BindFlags {
    texture: u32,
    writable_buffer: u32,
    storage_image: u32,
}

impl BindFlags {
    // TODO: which version between 2022.3 and 6000.3 introduced the texture flags
    fn of(version: &UnityVersion) -> Self {
        const WRITABLE: u32 = 0x0200_0000;
        const TEXTURE: u32 = 0x0080_0000;
        match version.major >= 6000 {
            true => BindFlags {
                texture: TEXTURE,
                writable_buffer: WRITABLE,
                storage_image: 0x8000_0000 | WRITABLE | TEXTURE,
            },
            false => BindFlags {
                texture: 0,
                writable_buffer: WRITABLE,
                storage_image: WRITABLE,
            },
        }
    }
}

fn bind_point(flags: u32, set: u32, binding: u32) -> Result<i32> {
    ensure!(
        set < 0x80 && binding < 0x1_0000,
        "set {set} binding {binding} out of range"
    );
    Ok((flags | set << 16 | binding) as i32)
}

/// The D3D11 name of a resource HLSLcc named after its register, like `_Out_origX0X`.
fn original_name(name: &str) -> &str {
    name.find("_origX").map_or(name, |end| &name[..end])
}

/// A kernel variant translated to Vulkan, and the layouts of its constant buffers.
struct Translated {
    kernel: ComputeShaderKernel,
    layouts: Vec<ComputeShaderCB>,
}

fn translate(
    layouts: &[ComputeShaderCB],
    named: &[ComputeShaderCB],
    d3d11: &ComputeShaderKernel,
    flags: BindFlags,
) -> Result<Translated> {
    let parameters = rabex_shader::compute::parameters(layouts, d3d11)?;
    let mut container = Container::parse(&d3d11.code)?;
    container.set_rdef(rdef(&parameters, &container)?);
    let translation = Hlslcc::vulkan()
        .translate_with_reflection(&container.to_bytes()?, &mut hlslcc::Dependencies::new())
        .context("HLSLcc")?;
    let mut words = match Glslang::vulkan()
        .translate(Stage::Compute, Program::Glsl(translation.source.clone()))
        .with_context(|| translation.source.clone())?
    {
        Program::Spirv(words) => words,
        other => bail!("glslang produced {other:?}"),
    };

    rebind_like_unity(&mut words, d3d11)?;

    let mut kernel = ComputeShaderKernel {
        cbVariantIndices: Vec::new(),
        cbs: Vec::new(),
        textures: Vec::new(),
        builtinSamplers: Vec::new(),
        inBuffers: Vec::new(),
        outBuffers: Vec::new(),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        threadGroupSize: d3d11.threadGroupSize.clone(),
        requirements: d3d11.requirements,
    };
    let resource = |name: &str, bind_point: i32| ComputeShaderResource {
        name: name.to_owned(),
        generatedName: String::new(),
        bindPoint: bind_point,
        samplerBindPoint: -1,
        texDimension: -1,
        resType: None,
    };
    let resources = spirv::resources(&words)?;
    let mut layouts_out = Vec::new();
    for r in &resources {
        match &r.kind {
            spirv::ResourceKind::UniformBuffer {
                members, accessed, ..
            } => {
                if let Some(layout) = named.iter().find(|c| c.name == r.name) {
                    layouts_out.push(clone_layout(layout));
                    kernel
                        .cbs
                        .push(resource(&r.name, bind_point(0, r.set, r.binding)?));
                    continue;
                }
                let described: Vec<_> = members
                    .iter()
                    .filter(|m| !m.name.starts_with(PLACEHOLDER_PREFIX))
                    .collect();
                let size = members
                    .iter()
                    .filter(|m| accessed.contains(&m.name))
                    .map(|m| m.offset + spirv::size(&m.ty))
                    .max()
                    .unwrap_or(0);
                let mut params = described
                    .iter()
                    .map(|m| param(m))
                    .collect::<Result<Vec<_>>>()?;
                params.sort_by(|a, b| a.name.cmp(&b.name));
                layouts_out.push(ComputeShaderCB {
                    name: r.name.clone(),
                    byteSize: i32::try_from(size)?,
                    params,
                });
                kernel
                    .cbs
                    .push(resource(&r.name, bind_point(0, r.set, r.binding)?));
            }
            spirv::ResourceKind::Texture { image, combined } => {
                ensure!(!combined, "{}: combined image sampler", r.name);
                let sampler = resources
                    .iter()
                    .find(|s| s.name == format!("sampler{}", r.name))
                    .map_or(Ok(-1), |s| i32::try_from(s.binding))?;
                kernel.textures.push(ComputeShaderResource {
                    samplerBindPoint: sampler,
                    texDimension: i32::try_from(texture_dimension(image)?)?,
                    ..resource(&r.name, bind_point(flags.texture, r.set, r.binding)?)
                });
            }
            spirv::ResourceKind::StorageBuffer { writable: false } => kernel
                .inBuffers
                .push(resource(&r.name, bind_point(0, r.set, r.binding)?)),
            spirv::ResourceKind::StorageBuffer { writable: true } => {
                kernel.outBuffers.push(resource(
                    original_name(&r.name),
                    bind_point(flags.writable_buffer, r.set, r.binding)?,
                ))
            }
            spirv::ResourceKind::StorageImage(image) => {
                kernel.outBuffers.push(ComputeShaderResource {
                    texDimension: i32::try_from(texture_dimension(image)?)?,
                    ..resource(
                        original_name(&r.name),
                        bind_point(flags.storage_image, r.set, r.binding)?,
                    )
                })
            }
            spirv::ResourceKind::Sampler => {
                let texture_sampler = resources
                    .iter()
                    .any(|t| r.name == format!("sampler{}", t.name));
                if !texture_sampler {
                    kernel.builtinSamplers.push(builtin_sampler(d3d11, r)?);
                }
            }
        }
    }
    for (list, d3d11) in [
        (&mut kernel.cbs, &d3d11.cbs),
        (&mut kernel.textures, &d3d11.textures),
        (&mut kernel.inBuffers, &d3d11.inBuffers),
        (&mut kernel.outBuffers, &d3d11.outBuffers),
    ] {
        list.sort_by(|a, b| a.name.cmp(&b.name));
        // TODO: what resType means; Unity 6 writes 0 for every resource of the fixtures
        for resource in list.iter_mut() {
            let d3d11_name = match resource.name.as_str() {
                "CGlobals" => "$Globals",
                name => name,
            };
            let source = d3d11
                .iter()
                .find(|r| r.name == d3d11_name)
                .with_context(|| format!("{}: no D3D11 resource", resource.name))?;
            resource.resType = source.resType;
        }
    }
    Ok(Translated {
        kernel,
        layouts: layouts_out,
    })
}

/// Renumber the descriptor set 0 bindings HLSLcc gives in declaration order, samplers first, like
/// Unity: shader resources, then UAVs, then samplers, each by D3D11 register.
// TODO: whether textures and buffers interleave by register or come by kind; the fixtures can't tell
fn rebind_like_unity(words: &mut [u32], d3d11: &ComputeShaderKernel) -> Result<()> {
    let register = |list: &[ComputeShaderResource], name: &str| {
        list.iter()
            .find(|r| r.name == name)
            .map(|r| r.bindPoint)
            .with_context(|| format!("{name}: not in the D3D11 kernel"))
    };
    let resources = spirv::resources(words)?;
    let mut order = Vec::new();
    for r in resources.iter().filter(|r| r.set == 0) {
        let key = match &r.kind {
            spirv::ResourceKind::Texture { .. } => (0, register(&d3d11.textures, &r.name)?),
            spirv::ResourceKind::StorageBuffer { writable: false } => {
                (0, register(&d3d11.inBuffers, &r.name)?)
            }
            spirv::ResourceKind::StorageBuffer { writable: true }
            | spirv::ResourceKind::StorageImage(_) => {
                (1, register(&d3d11.outBuffers, original_name(&r.name))?)
            }
            spirv::ResourceKind::Sampler => {
                let texture = r.name.strip_prefix("sampler").and_then(|t| {
                    d3d11
                        .textures
                        .iter()
                        .find(|x| x.name == t && x.samplerBindPoint >= 0)
                });
                match texture {
                    Some(texture) => (2, texture.samplerBindPoint),
                    None => (2, inline_sampler_register(&r.name)?),
                }
            }
            spirv::ResourceKind::UniformBuffer { .. } => {
                bail!("{}: uniform buffer in descriptor set 0", r.name)
            }
        };
        order.push((key, r.id));
    }
    order.sort();
    let bindings = order
        .iter()
        .enumerate()
        .map(|(binding, &(_, id))| Ok((id, u32::try_from(binding)?)))
        .collect::<Result<_>>()?;
    spirv::rebind(words, &bindings)
}

fn param(member: &spirv::Member) -> Result<ComputeShaderParam> {
    let (scalar, rows, columns, array_size) = match &member.ty {
        spirv::Type::Scalar(s) => (*s, 1, 1, 0),
        spirv::Type::Vector(s, n) => (*s, 1, *n, 0),
        spirv::Type::Matrix {
            scalar,
            columns,
            rows,
            ..
        } => (*scalar, *rows, *columns, 0),
        spirv::Type::Array {
            element, length, ..
        } => match element.as_ref() {
            spirv::Type::Scalar(s) => (*s, 1, 1, *length),
            spirv::Type::Vector(s, n) => (*s, 1, *n, *length),
            spirv::Type::Matrix {
                scalar,
                columns,
                rows,
                ..
            } => (*scalar, *rows, *columns, *length),
            other => bail!("{}: array of {other:?}", member.name),
        },
        spirv::Type::Struct(_) => bail!("{}: struct in a compute constant buffer", member.name),
    };
    Ok(ComputeShaderParam {
        name: member.name.clone(),
        r#type: vulkan_param_type(data_type(scalar))?,
        offset: member.offset,
        arraySize: array_size,
        rowCount: rows,
        colCount: columns,
    })
}

/// The D3D11 register of an inline sampler HLSLcc named after it, like `s1` or `samplers1`.
fn inline_sampler_register(name: &str) -> Result<i32> {
    super::register_in_name(name, "s")
        .or_else(|| super::register_in_name(name, "samplers"))
        .with_context(|| format!("{name}: no register in the sampler's name"))
}

/// The state of an inline sampler, from the D3D11 kernel's sampler at the same register.
fn builtin_sampler(
    d3d11: &ComputeShaderKernel,
    sampler: &spirv::Resource,
) -> Result<ComputeShaderBuiltinSampler> {
    let register = inline_sampler_register(&sampler.name)?;
    let state = d3d11
        .builtinSamplers
        .iter()
        .find(|s| s.bindPoint == register)
        .with_context(|| format!("{}: no D3D11 inline sampler at s{register}", sampler.name))?;
    Ok(ComputeShaderBuiltinSampler {
        sampler: state.sampler,
        bindPoint: i32::try_from(sampler.binding)?,
    })
}

fn clone_layout(layout: &ComputeShaderCB) -> ComputeShaderCB {
    ComputeShaderCB {
        name: layout.name.clone(),
        byteSize: layout.byteSize,
        params: layout
            .params
            .iter()
            .map(|p| ComputeShaderParam {
                name: p.name.clone(),
                r#type: p.r#type,
                offset: p.offset,
                arraySize: p.arraySize,
                rowCount: p.rowCount,
                colCount: p.colCount,
            })
            .collect(),
    }
}

/// The Vulkan layouts of the constant buffers besides `$Globals`: every member any kernel's
/// D3D11 layout of the buffer has, like Unity, which describes the whole buffer for each kernel.
// TODO: members no kernel uses, which no D3D11 layout lists
fn named_layouts(d3d11: &ComputeShaderPlatformVariant) -> Result<Vec<ComputeShaderCB>> {
    let mut layouts: Vec<ComputeShaderCB> = Vec::new();
    for cb in d3d11
        .constantBuffers
        .iter()
        .filter(|c| c.name != "$Globals")
    {
        let layout = match layouts.iter_mut().find(|l| l.name == cb.name) {
            Some(layout) => layout,
            None => {
                layouts.push(ComputeShaderCB {
                    name: cb.name.clone(),
                    byteSize: cb.byteSize,
                    params: Vec::new(),
                });
                layouts.last_mut().unwrap()
            }
        };
        ensure!(
            layout.byteSize == cb.byteSize,
            "{}: sizes {} and {} in different kernels",
            cb.name,
            layout.byteSize,
            cb.byteSize
        );
        for p in &cb.params {
            let r#type = ShaderParamType::try_from(i64::from(p.r#type))
                .with_context(|| format!("{}: constant {}", cb.name, p.name))?;
            let param = ComputeShaderParam {
                name: p.name.clone(),
                r#type: vulkan_param_type(r#type)?,
                offset: p.offset,
                arraySize: p.arraySize,
                rowCount: p.rowCount,
                colCount: p.colCount,
            };
            match layout.params.iter().find(|q| q.name == p.name) {
                Some(q) => ensure!(
                    (q.r#type, q.offset, q.arraySize, q.rowCount, q.colCount)
                        == (
                            param.r#type,
                            param.offset,
                            param.arraySize,
                            param.rowCount,
                            param.colCount
                        ),
                    "{}.{}: differs between kernels",
                    cb.name,
                    p.name
                ),
                None => layout.params.push(param),
            }
        }
    }
    for layout in &mut layouts {
        layout.params.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(layouts)
}

/// Unity's Vulkan kernels describe uint constants as int.
// TODO: whether graphics programs do too, once a fixture has a uint constant
fn vulkan_param_type(d3d11: ShaderParamType) -> Result<i32> {
    Ok(match d3d11 {
        ShaderParamType::UInt => shader_param_type::Int as i32,
        ShaderParamType::Float | ShaderParamType::Int | ShaderParamType::Bool => {
            d3d11.value() as i32
        }
        // TODO: half and short constants, once data with them turns up
        other => bail!("{other} in a compute shader"),
    })
}

/// Replace the D3D11 kernels of a compute shader of Unity `version` with Vulkan kernels.
/// Convert a copy of the compute shader, leaving the original untouched.
pub fn convert(shader: &ComputeShader, version: &UnityVersion) -> Result<ComputeShader> {
    let mut shader = shader.clone();
    convert_inplace(&mut shader, version)?;
    Ok(shader)
}

/// Convert the compute shader in place: its D3D11 kernels become Vulkan kernels.
pub fn convert_inplace(shader: &mut ComputeShader, version: &UnityVersion) -> Result<()> {
    let flags = BindFlags::of(version);
    let named = match shader.variants.as_slice() {
        [d3d11] => named_layouts(d3d11)?,
        _ => Vec::new(),
    };
    let [d3d11] = shader.variants.as_slice() else {
        bail!(
            "{} platform variants, not D3D11 alone",
            shader.variants.len()
        );
    };
    ensure!(
        d3d11.targetRenderer == graphics_device_type::Direct3D11 as i32,
        "renderer {}, not D3D11",
        d3d11.targetRenderer
    );
    let mut constant_buffers: Vec<ComputeShaderCB> = Vec::new();
    let mut kernels = Vec::new();
    for parent in &d3d11.kernels {
        let mut variants = Vec::new();
        for (i, variant) in parent.uniqueVariants.iter().enumerate() {
            let translated = translate(&d3d11.constantBuffers, &named, variant, flags)
                .with_context(|| format!("{} variant {i}", parent.name))?;
            let mut kernel = translated.kernel;
            for layout in translated.layouts {
                let index = match constant_buffers
                    .iter()
                    .position(|c| same_layout(c, &layout))
                {
                    Some(index) => index,
                    None => {
                        constant_buffers.push(layout);
                        constant_buffers.len() - 1
                    }
                };
                kernel.cbVariantIndices.push(u32::try_from(index)?);
            }
            variants.push(kernel);
        }
        kernels.push(ComputeShaderKernelParent {
            name: parent.name.clone(),
            uniqueVariants: variants,
            variantIndices: parent
                .variantIndices
                .iter()
                .map(|p| rabex_shader::compute_types::pair {
                    first: p.first.clone(),
                    second: p.second,
                })
                .collect(),
            globalKeywords: parent.globalKeywords.clone(),
            localKeywords: parent.localKeywords.clone(),
            dynamicKeywords: parent.dynamicKeywords.clone(),
        });
    }
    shader.variants = vec![ComputeShaderPlatformVariant {
        targetRenderer: graphics_device_type::Vulkan as i32,
        targetLevel: d3d11.targetLevel,
        kernels,
        constantBuffers: constant_buffers,
        resourcesResolved: d3d11.resourcesResolved,
    }];
    Ok(())
}

fn same_layout(a: &ComputeShaderCB, b: &ComputeShaderCB) -> bool {
    let params = |c: &ComputeShaderCB| {
        c.params
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    p.r#type,
                    p.offset,
                    p.arraySize,
                    p.rowCount,
                    p.colCount,
                )
            })
            .collect::<Vec<_>>()
    };
    a.name == b.name && a.byteSize == b.byteSize && params(a) == params(b)
}
