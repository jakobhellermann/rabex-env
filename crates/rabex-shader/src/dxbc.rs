//! DXBC containers, and rebuilding the `RDEF` reflection chunk Unity strips from its
//! D3D11 programs.
//!
//! Unity's D3D11 program entries hold DXBC containers (see [`crate::SubProgram::dxbc`]) with the
//! `RDEF` reflection chunk stripped, because the player reads the parameters from the
//! serialized form instead. Translating a program to another backend needs the code, and
//! the translator (`rabex-shader-convert`'s HLSLcc) refuses a container without `RDEF`:
//!
//! 1. [`Container::parse`] the DXBC of the D3D11 entry,
//! 2. [`rdef`] rebuilds an `RDEF` from the variant's [`ProgramParameters`] — resources
//!    the parameters don't list get placeholder constants
//!    ([`PLACEHOLDER_PREFIX`]) to fill the register gaps,
//! 3. [`Container::set_rdef`] splices it in, and the translator receives a complete
//!    container.
//!
//! The Vulkan target reads the container's [`Container::input_signature`] (vertex
//! inputs, to derive the channel bindings) and [`Container::declarations`] (resource
//! registers, to map HLSLcc's register-named inputs back to Unity's bindings).

use anyhow::{Context, Result, bail, ensure};

use crate::d3d::*;
use crate::unity::{shader_param_type, texture_dimension};
use crate::{Binding, Constant, ProgramParameters, Reader, StructConstant, Writer};

/// A DXBC container's chunks, in order.
#[derive(Clone, Debug)]
pub struct Container {
    pub chunks: Vec<([u8; 4], Vec<u8>)>,
}

impl Container {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        ensure!(r.take(4)? == b"DXBC", "not a DXBC container");
        r.take(16)?;
        ensure!(r.u32()? == 1, "unknown DXBC container version");
        let size = r.u32()? as usize;
        ensure!(
            size == data.len(),
            "container size {size} disagrees with {} bytes",
            data.len()
        );
        let offsets = (0..r.u32()?).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        let chunks = offsets
            .into_iter()
            .map(|offset| {
                let mut r = Reader::new(data);
                r.take(offset as usize)?;
                let fourcc: [u8; 4] = r.take(4)?.try_into().unwrap();
                let len = r.u32()? as usize;
                Ok((fourcc, r.take(len)?.to_vec()))
            })
            .collect::<Result<_>>()?;
        Ok(Container { chunks })
    }

    /// Serialize with a zero checksum, which translators like HLSLcc don't verify.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let header_size = 32 + 4 * self.chunks.len();
        let mut offsets = Vec::with_capacity(self.chunks.len());
        let mut offset = header_size;
        for (_, data) in &self.chunks {
            offsets.push(u32::try_from(offset)?);
            offset += 8 + data.len();
        }
        let mut w = Writer::default();
        w.data.extend_from_slice(b"DXBC");
        w.data.extend_from_slice(&[0; 16]);
        w.u32(1);
        w.u32(u32::try_from(offset)?);
        w.u32(u32::try_from(self.chunks.len())?);
        for offset in offsets {
            w.u32(offset);
        }
        for (fourcc, data) in &self.chunks {
            w.data.extend_from_slice(fourcc);
            w.u32(u32::try_from(data.len())?);
            w.data.extend_from_slice(data);
        }
        Ok(w.finish())
    }

    pub fn chunk(&self, fourcc: &[u8; 4]) -> Option<&[u8]> {
        self.chunks
            .iter()
            .find(|(f, _)| f == fourcc)
            .map(|(_, d)| d.as_slice())
    }

    /// The program's version token: `(program type, major, minor)`.
    pub fn shader_version(&self) -> Result<(u32, u32, u32)> {
        let code = self
            .chunk(b"SHEX")
            .or_else(|| self.chunk(b"SHDR"))
            .context("container without SHDR or SHEX chunk")?;
        let token = Reader::new(code).u32()?;
        Ok((
            (token & D3D10_SB_TOKENIZED_PROGRAM_TYPE_MASK) >> D3D10_SB_TOKENIZED_PROGRAM_TYPE_SHIFT,
            (token & D3D10_SB_TOKENIZED_PROGRAM_MAJOR_VERSION_MASK)
                >> D3D10_SB_TOKENIZED_PROGRAM_MAJOR_VERSION_SHIFT,
            token & D3D10_SB_TOKENIZED_PROGRAM_MINOR_VERSION_MASK,
        ))
    }

    /// Insert or replace the `RDEF` chunk, as the first chunk like fxc does.
    pub fn set_rdef(&mut self, rdef: Vec<u8>) {
        self.chunks.retain(|(fourcc, _)| fourcc != b"RDEF");
        self.chunks.insert(0, (*b"RDEF", rdef));
    }
}

/// An element of a program's input or output signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureElement {
    pub semantic: String,
    pub semantic_index: u32,
    /// `D3D_NAME`, like 1 for `SV_Position`.
    pub system_value: u32,
    /// `D3D_REGISTER_COMPONENT_TYPE`
    pub component_type: u32,
    pub register: u32,
    pub mask: u8,
}

/// A shader resource, UAV or sampler declared in the program's bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub space: RegisterSpace,
    pub register: u32,
    pub kind: DeclarationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterSpace {
    /// `t#`
    Resource,
    /// `u#`
    Uav,
    /// `s#`
    Sampler,
}

impl RegisterSpace {
    fn prefix(self) -> char {
        match self {
            RegisterSpace::Resource => 't',
            RegisterSpace::Uav => 'u',
            RegisterSpace::Sampler => 's',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    /// `RESOURCE_DIMENSION` of the bytecode and the first component's return type.
    Typed {
        dimension: u32,
        return_type: u32,
    },
    Raw,
    Structured {
        stride: u32,
        counter: bool,
    },
    Sampler,
}

/// `DECODE_D3D10_SB_OPERAND_INDEX_REPRESENTATION`, a function-like macro bindgen skips.
const fn index_representation(dimension: u32, operand: u32) -> u32 {
    let shift = 22 + 3 * (dimension & 3);
    (operand & (0x3 << shift)) >> shift
}

impl Container {
    /// The resource and UAV declarations of the program's bytecode.
    /// The elements of the input signature (`ISGN`).
    pub fn input_signature(&self) -> Result<Vec<SignatureElement>> {
        let chunk = self
            .chunk(b"ISGN")
            .context("container without ISGN chunk")?;
        let mut r = Reader::new(chunk);
        let count = r.u32()?;
        ensure!(r.u32()? == 8, "unexpected ISGN element table offset");
        (0..count)
            .map(|_| {
                let name_offset = r.u32()? as usize;
                let semantic_index = r.u32()?;
                let system_value = r.u32()?;
                let component_type = r.u32()?;
                let register = r.u32()?;
                let masks = r.u32()?;
                let name = chunk
                    .get(name_offset..)
                    .and_then(|rest| rest.split(|&b| b == 0).next())
                    .context("signature element name out of range")?;
                Ok(SignatureElement {
                    semantic: String::from_utf8(name.to_vec())?,
                    semantic_index,
                    system_value,
                    component_type,
                    register,
                    mask: masks as u8,
                })
            })
            .collect()
    }

    pub fn declarations(&self) -> Result<Vec<Declaration>> {
        let code = self
            .chunk(b"SHEX")
            .or_else(|| self.chunk(b"SHDR"))
            .context("container without SHDR or SHEX chunk")?;
        let tokens: Vec<u32> = code
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let length = *tokens.get(1).context("bytecode without length token")? as usize;
        ensure!(
            length <= tokens.len(),
            "bytecode length {length} exceeds the chunk"
        );
        let mut declarations = Vec::new();
        let mut pos = 2;
        while pos < length {
            let token = tokens[pos];
            let opcode = token & D3D10_SB_OPCODE_TYPE_MASK;
            let instruction_length = if opcode == D3D10_SB_OPCODE_CUSTOMDATA {
                *tokens.get(pos + 1).context("truncated custom data")? as usize
            } else {
                ((token & D3D10_SB_TOKENIZED_INSTRUCTION_LENGTH_MASK)
                    >> D3D10_SB_TOKENIZED_INSTRUCTION_LENGTH_SHIFT) as usize
            };
            ensure!(
                instruction_length > 0,
                "zero length instruction at token {pos}"
            );
            let instruction = tokens
                .get(pos..pos + instruction_length)
                .context("instruction exceeds the bytecode")?;
            let kind = match opcode {
                D3D10_SB_OPCODE_DCL_RESOURCE | D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_TYPED => {
                    Some(DeclarationKind::Typed {
                        dimension: (token & D3D10_SB_RESOURCE_DIMENSION_MASK)
                            >> D3D10_SB_RESOURCE_DIMENSION_SHIFT,
                        return_type: *instruction.last().unwrap()
                            & D3D10_SB_RESOURCE_RETURN_TYPE_MASK,
                    })
                }
                D3D11_SB_OPCODE_DCL_RESOURCE_RAW
                | D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_RAW => Some(DeclarationKind::Raw),
                D3D11_SB_OPCODE_DCL_RESOURCE_STRUCTURED
                | D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED => {
                    Some(DeclarationKind::Structured {
                        stride: *instruction.last().unwrap(),
                        counter: opcode == D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED
                            && token & D3D11_SB_UAV_HAS_ORDER_PRESERVING_COUNTER != 0,
                    })
                }
                D3D10_SB_OPCODE_DCL_SAMPLER => Some(DeclarationKind::Sampler),
                _ => None,
            };
            if let Some(kind) = kind {
                let operand = *instruction.get(1).context("declaration without operand")?;
                ensure!(
                    operand & D3D10_SB_OPERAND_EXTENDED_MASK == 0,
                    "extended operand in a declaration"
                );
                // TODO: shader model 5.1 declarations with register ranges
                ensure!(
                    (operand & D3D10_SB_OPERAND_INDEX_DIMENSION_MASK)
                        >> D3D10_SB_OPERAND_INDEX_DIMENSION_SHIFT
                        == D3D10_SB_OPERAND_INDEX_1D
                        && index_representation(0, operand) == D3D10_SB_OPERAND_INDEX_IMMEDIATE32,
                    "declaration operand without a single immediate index (shader model 5.1?)"
                );
                declarations.push(Declaration {
                    space: match opcode {
                        D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_TYPED
                        | D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_RAW
                        | D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED => {
                            RegisterSpace::Uav
                        }
                        D3D10_SB_OPCODE_DCL_SAMPLER => RegisterSpace::Sampler,
                        _ => RegisterSpace::Resource,
                    },
                    register: *instruction.get(2).context("declaration without index")?,
                    kind,
                });
            }
            pos += instruction_length;
        }
        Ok(declarations)
    }
}

struct ConstantBufferDesc {
    name: String,
    size: u32,
    kind: u32,
    variables: Vec<Variable>,
}

/// Builds the `RDEF` chunk of `container`'s program from its Unity parameters. Buffer kinds and
/// strides come from the bytecode's declarations.
pub fn rdef(parameters: &ProgramParameters, container: &Container) -> Result<Vec<u8>> {
    let (program_type, major, minor) = container.shader_version()?;
    let declarations = container.declarations()?;
    let declaration = |space: RegisterSpace, index: i32| -> Result<DeclarationKind> {
        let register = u32::try_from(index)?;
        declarations
            .iter()
            .find(|d| d.space == space && d.register == register)
            .map(|d| d.kind)
            .with_context(|| format!("no declaration for {}{register}", space.prefix()))
    };
    let (unnamed, named) = parameters
        .constant_buffers
        .split_first()
        .context("parameters without the unnamed scope")?;
    ensure!(
        unnamed.constants.is_empty() && unnamed.structs.is_empty(),
        "D3D11 parameters with constants outside constant buffers"
    );

    let mut resources = Vec::new();
    let mut constant_buffers = Vec::new();
    for cb in named {
        let index = parameters
            .bindings
            .iter()
            .find_map(|b| match b {
                Binding::ConstantBuffer { name, index, .. } if *name == cb.name => Some(*index),
                _ => None,
            })
            .with_context(|| format!("constant buffer {} has no binding", cb.name))?;
        resources.push(Resource {
            name: cb.name.clone(),
            kind: D3D_SIT_CBUFFER,
            return_type: 0,
            dimension: 0,
            samples: 0,
            bind_point: u32::try_from(index)?,
            flags: 0,
        });
        let mut variables = cb
            .constants
            .iter()
            .map(|c| {
                Ok(Variable {
                    name: c.name.clone(),
                    offset: c.offset,
                    ty: constant_type(c)?,
                })
            })
            .chain(cb.structs.iter().map(|s| {
                Ok(Variable {
                    name: s.name.clone(),
                    offset: s.offset,
                    ty: struct_type(s)?,
                })
            }))
            .collect::<Result<Vec<_>>>()?;
        fill_gaps(&mut variables, cb.size, constant_buffers.len())?;
        constant_buffers.push(ConstantBufferDesc {
            name: cb.name.clone(),
            size: cb.size,
            kind: D3D_CT_CBUFFER,
            variables,
        });
    }

    // structured buffers describe their stride through a constant buffer named like them
    let mut structured = |name: &str, stride: u32| -> Result<()> {
        ensure!(
            stride % 4 == 0,
            "{name}: stride {stride} isn't a multiple of 4"
        );
        let members = (0..stride / 4)
            .map(|i| (format!("u{i}"), 4 * i, scalar_uint()))
            .collect();
        constant_buffers.push(ConstantBufferDesc {
            name: name.to_owned(),
            size: stride,
            kind: D3D_CT_RESOURCE_BIND_INFO,
            variables: vec![Variable {
                name: "$Element".to_owned(),
                offset: 0,
                ty: Type {
                    class: D3D_SVC_STRUCT,
                    ty: D3D_SVT_VOID,
                    rows: 1,
                    columns: u16::try_from(stride / 4)?,
                    elements: 0,
                    members,
                    element_size: stride,
                },
            }],
        });
        Ok(())
    };

    let mut samplers: Vec<(i32, String)> = Vec::new();
    for binding in &parameters.bindings {
        match binding {
            Binding::Texture {
                name,
                index,
                sampler_index,
                dimension,
                multisampled,
            } => {
                resources.push(Resource {
                    name: name.clone(),
                    kind: D3D_SIT_TEXTURE,
                    return_type: D3D_RETURN_TYPE_FLOAT,
                    dimension: srv_dimension(*dimension, *multisampled)?,
                    samples: if *multisampled { 0 } else { u32::MAX },
                    bind_point: u32::try_from(*index)?,
                    flags: D3D_SIF_TEXTURE_COMPONENTS,
                });
                if *sampler_index >= 0 && !samplers.iter().any(|(i, _)| i == sampler_index) {
                    samplers.push((*sampler_index, format!("sampler{name}")));
                }
            }
            Binding::ConstantBuffer { .. } => {}
            Binding::Buffer { name, index, .. } => {
                let (kind, return_type, dimension, samples) =
                    match declaration(RegisterSpace::Resource, *index)
                        .with_context(|| name.clone())?
                    {
                        DeclarationKind::Structured { stride, .. } => {
                            structured(name, stride)?;
                            (
                                D3D_SIT_STRUCTURED,
                                D3D_RETURN_TYPE_MIXED,
                                D3D_SRV_DIMENSION_BUFFEREX,
                                stride,
                            )
                        }
                        DeclarationKind::Raw => (
                            D3D_SIT_BYTEADDRESS,
                            D3D_RETURN_TYPE_MIXED,
                            D3D_SRV_DIMENSION_BUFFEREX,
                            0,
                        ),
                        DeclarationKind::Typed {
                            dimension,
                            return_type,
                        } => {
                            ensure!(
                                dimension == 1,
                                "{name}: typed buffer with resource dimension {dimension}"
                            );
                            (
                                D3D_SIT_TEXTURE,
                                return_type,
                                D3D_SRV_DIMENSION_BUFFER,
                                u32::MAX,
                            )
                        }
                        DeclarationKind::Sampler => bail!("{name}: sampler declared as t register"),
                    };
                resources.push(Resource {
                    name: name.clone(),
                    kind,
                    return_type,
                    dimension,
                    samples,
                    bind_point: u32::try_from(*index)?,
                    flags: 0,
                });
            }
            Binding::Uav { name, index, .. } => {
                let (kind, return_type, dimension, samples) =
                    match declaration(RegisterSpace::Uav, *index).with_context(|| name.clone())? {
                        DeclarationKind::Structured { stride, counter } => {
                            structured(name, stride)?;
                            let kind = if counter {
                                D3D_SIT_UAV_RWSTRUCTURED_WITH_COUNTER
                            } else {
                                D3D_SIT_UAV_RWSTRUCTURED
                            };
                            (
                                kind,
                                D3D_RETURN_TYPE_MIXED,
                                D3D_SRV_DIMENSION_BUFFEREX,
                                stride,
                            )
                        }
                        DeclarationKind::Raw => (
                            D3D_SIT_UAV_RWBYTEADDRESS,
                            D3D_RETURN_TYPE_MIXED,
                            D3D_SRV_DIMENSION_BUFFEREX,
                            0,
                        ),
                        DeclarationKind::Typed {
                            dimension,
                            return_type,
                        } => (
                            D3D_SIT_UAV_RWTYPED,
                            return_type,
                            declared_dimension(dimension)?,
                            0,
                        ),
                        DeclarationKind::Sampler => bail!("{name}: sampler declared as u register"),
                    };
                resources.push(Resource {
                    name: name.clone(),
                    kind,
                    return_type,
                    dimension,
                    samples,
                    bind_point: u32::try_from(*index)?,
                    flags: if kind == D3D_SIT_UAV_RWTYPED {
                        D3D_SIF_TEXTURE_COMPONENTS
                    } else {
                        0
                    },
                });
            }
            // inline samplers are unnamed; their register is covered by its declaration below
            Binding::Sampler { index, .. } => {
                declaration(RegisterSpace::Sampler, *index)?;
            }
        }
    }
    for (index, name) in samplers {
        resources.push(Resource {
            name,
            kind: D3D_SIT_SAMPLER,
            return_type: 0,
            dimension: 0,
            samples: 0,
            bind_point: u32::try_from(index)?,
            flags: 0,
        });
    }

    // Unity leaves some declared registers out of its D3D11 parameters, like UAVs, which it binds
    // by register: name them after the register so translators see every declaration
    for d in &declarations {
        let kinds: &[u32] = match d.space {
            RegisterSpace::Resource => &[D3D_SIT_TEXTURE, D3D_SIT_STRUCTURED, D3D_SIT_BYTEADDRESS],
            RegisterSpace::Uav => &[
                D3D_SIT_UAV_RWTYPED,
                D3D_SIT_UAV_RWSTRUCTURED,
                D3D_SIT_UAV_RWBYTEADDRESS,
                D3D_SIT_UAV_RWSTRUCTURED_WITH_COUNTER,
            ],
            RegisterSpace::Sampler => &[D3D_SIT_SAMPLER],
        };
        if resources
            .iter()
            .any(|r| r.bind_point == d.register && kinds.contains(&r.kind))
        {
            continue;
        }
        let name = format!("{}{}", d.space.prefix(), d.register);
        let (kind, return_type, dimension, samples, flags) = match (d.space, d.kind) {
            (RegisterSpace::Sampler, DeclarationKind::Sampler) => (D3D_SIT_SAMPLER, 0, 0, 0, 0),
            (space, DeclarationKind::Structured { stride, counter }) => {
                structured(&name, stride)?;
                let kind = match (space, counter) {
                    (RegisterSpace::Uav, true) => D3D_SIT_UAV_RWSTRUCTURED_WITH_COUNTER,
                    (RegisterSpace::Uav, false) => D3D_SIT_UAV_RWSTRUCTURED,
                    _ => D3D_SIT_STRUCTURED,
                };
                (
                    kind,
                    D3D_RETURN_TYPE_MIXED,
                    D3D_SRV_DIMENSION_BUFFEREX,
                    stride,
                    0,
                )
            }
            (space, DeclarationKind::Raw) => {
                let kind = if space == RegisterSpace::Uav {
                    D3D_SIT_UAV_RWBYTEADDRESS
                } else {
                    D3D_SIT_BYTEADDRESS
                };
                (
                    kind,
                    D3D_RETURN_TYPE_MIXED,
                    D3D_SRV_DIMENSION_BUFFEREX,
                    0,
                    0,
                )
            }
            (
                space,
                DeclarationKind::Typed {
                    dimension,
                    return_type,
                },
            ) => {
                let kind = if space == RegisterSpace::Uav {
                    D3D_SIT_UAV_RWTYPED
                } else {
                    D3D_SIT_TEXTURE
                };
                let samples = if space == RegisterSpace::Uav {
                    0
                } else {
                    u32::MAX
                };
                (
                    kind,
                    return_type,
                    declared_dimension(dimension)?,
                    samples,
                    D3D_SIF_TEXTURE_COMPONENTS,
                )
            }
            (space, kind) => bail!("{name}: {kind:?} declared in {space:?} space"),
        };
        resources.push(Resource {
            name,
            kind,
            return_type,
            dimension,
            samples,
            bind_point: d.register,
            flags,
        });
    }

    let sm5 = major >= 5;
    let mut layout = Layout::default();
    // without the SM5 `RD11` sub-header, which consumers derive from the program version instead
    let header_size = 28;
    let resource_size = 32;
    let cbuffer_size = 24;
    let variable_size = if sm5 { 40 } else { 24 };
    let resource_offset = header_size;
    let cbuffer_offset = resource_offset + resource_size * resources.len();
    let mut variable_offset = cbuffer_offset + cbuffer_size * constant_buffers.len();
    layout.cursor = variable_offset
        + variable_size
            * constant_buffers
                .iter()
                .map(|cb| cb.variables.len())
                .sum::<usize>();

    let mut body = Writer::default();
    let mut cbuffers = Writer::default();
    for cb in &constant_buffers {
        cbuffers.u32(layout.string(&cb.name));
        cbuffers.u32(u32::try_from(cb.variables.len())?);
        cbuffers.u32(u32::try_from(variable_offset)?);
        cbuffers.u32(cb.size);
        cbuffers.u32(0);
        cbuffers.u32(cb.kind);
        for variable in &cb.variables {
            body.u32(layout.string(&variable.name));
            body.u32(variable.offset);
            body.u32(variable.ty.size());
            body.u32(D3D_SVF_USED);
            body.u32(layout.ty(&variable.ty));
            body.u32(0);
            if sm5 {
                body.u32(u32::MAX);
                body.u32(0);
                body.u32(u32::MAX);
                body.u32(0);
            }
            variable_offset += variable_size;
        }
    }

    let mut w = Writer::default();
    w.u32(u32::try_from(constant_buffers.len())?);
    w.u32(u32::try_from(cbuffer_offset)?);
    w.u32(u32::try_from(resources.len())?);
    w.u32(u32::try_from(resource_offset)?);
    // TODO: fxc's RDEF program type tags aren't in the DirectX headers, and HLSLcc ignores them
    let rdef_program_type = match program_type {
        D3D10_SB_PIXEL_SHADER => 0xffff,
        D3D10_SB_VERTEX_SHADER => 0xfffe,
        D3D10_SB_GEOMETRY_SHADER => 0x4753,
        D3D11_SB_HULL_SHADER => 0x4853,
        D3D11_SB_DOMAIN_SHADER => 0x4453,
        D3D11_SB_COMPUTE_SHADER => 0x4353,
        other => bail!("unknown program type {other}"),
    };
    w.u32(minor | major << 8 | rdef_program_type << 16);
    w.u32(0);
    w.u32(layout.string("rabex-shader"));
    ensure!(
        w.data.len() == header_size,
        "RDEF header has {} bytes",
        w.data.len()
    );
    for resource in &resources {
        w.u32(layout.string(&resource.name));
        w.u32(resource.kind);
        w.u32(resource.return_type);
        w.u32(resource.dimension);
        w.u32(resource.samples);
        w.u32(resource.bind_point);
        w.u32(1);
        w.u32(resource.flags);
    }
    w.data.extend_from_slice(&cbuffers.finish());
    w.data.extend_from_slice(&body.finish());
    ensure!(w.data.len() == layout.base(), "RDEF body layout is off");
    w.data.extend_from_slice(&layout.finish());
    Ok(w.finish())
}

/// Names of the placeholders [`rdef`] fills constant buffer gaps with start with this.
pub const PLACEHOLDER_PREFIX: &str = "_rabex_unused";

/// Cover the parts of a constant buffer Unity doesn't describe, like unsupported matrix shapes,
/// with float placeholders named uniquely across buffers: translators resolve every accessed offset
/// to a variable, and Unity never uploads values there. Placeholders keep their offset under std140
/// too. Sorts `variables` by offset.
fn fill_gaps(variables: &mut Vec<Variable>, size: u32, buffer_index: usize) -> Result<()> {
    let mut covered = vec![false; size.div_ceil(4) as usize];
    for v in variables.iter() {
        let start = (v.offset / 4) as usize;
        let end = (v.offset + v.ty.size()).div_ceil(4) as usize;
        let slots = covered
            .get_mut(start..end)
            .with_context(|| format!("{} exceeds the constant buffer size {size}", v.name))?;
        slots.fill(true);
    }
    for register in 0..covered.len().div_ceil(4) {
        let slots = register * 4..(register * 4 + 4).min(covered.len());
        let mut slot = slots.start;
        while slot < slots.end {
            if covered[slot] {
                slot += 1;
                continue;
            }
            let mut free = 0;
            while slot + free < slots.end && !covered[slot + free] {
                free += 1;
            }
            // std140 aligns vec2 to 8 and vec3 and vec4 to 16 bytes, D3D packs them anywhere in a register
            let columns = match slot % 4 {
                0 => free,
                2 => free.min(2),
                _ => 1,
            } as u16;
            let start = slot;
            slot += usize::from(columns);
            variables.push(Variable {
                name: format!("{PLACEHOLDER_PREFIX}{buffer_index}_{}", start * 4),
                offset: (start * 4) as u32,
                ty: Type {
                    class: if columns == 1 {
                        D3D_SVC_SCALAR
                    } else {
                        D3D_SVC_VECTOR
                    },
                    ty: D3D_SVT_FLOAT,
                    rows: 1,
                    columns,
                    elements: 0,
                    members: Vec::new(),
                    element_size: 4 * u32::from(columns),
                },
            });
        }
    }
    variables.sort_by_key(|v| v.offset);
    Ok(())
}

fn scalar_uint() -> Type {
    Type {
        class: D3D_SVC_SCALAR,
        ty: D3D_SVT_UINT,
        rows: 1,
        columns: 1,
        elements: 0,
        members: Vec::new(),
        element_size: 4,
    }
}

fn declared_dimension(dimension: D3D10_SB_RESOURCE_DIMENSION) -> Result<D3D_SRV_DIMENSION> {
    Ok(match dimension {
        D3D10_SB_RESOURCE_DIMENSION_BUFFER => D3D_SRV_DIMENSION_BUFFER,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE1D => D3D_SRV_DIMENSION_TEXTURE1D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2D => D3D_SRV_DIMENSION_TEXTURE2D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DMS => D3D_SRV_DIMENSION_TEXTURE2DMS,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE3D => D3D_SRV_DIMENSION_TEXTURE3D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURECUBE => D3D_SRV_DIMENSION_TEXTURECUBE,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE1DARRAY => D3D_SRV_DIMENSION_TEXTURE1DARRAY,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DARRAY => D3D_SRV_DIMENSION_TEXTURE2DARRAY,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DMSARRAY => D3D_SRV_DIMENSION_TEXTURE2DMSARRAY,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURECUBEARRAY => D3D_SRV_DIMENSION_TEXTURECUBEARRAY,
        D3D11_SB_RESOURCE_DIMENSION_RAW_BUFFER | D3D11_SB_RESOURCE_DIMENSION_STRUCTURED_BUFFER => {
            D3D_SRV_DIMENSION_BUFFEREX
        }
        other => bail!("unknown resource dimension {other}"),
    })
}

struct Resource {
    name: String,
    kind: u32,
    return_type: u32,
    dimension: u32,
    samples: u32,
    bind_point: u32,
    flags: u32,
}

struct Variable {
    name: String,
    offset: u32,
    ty: Type,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Type {
    class: _D3D_SHADER_VARIABLE_CLASS,
    ty: _D3D_SHADER_VARIABLE_TYPE,
    rows: u16,
    columns: u16,
    elements: u16,
    /// `(name, offset in the struct, type)`.
    members: Vec<(String, u32, Type)>,
    /// Size of one element.
    element_size: u32,
}

impl Type {
    /// Like fxc: array elements start at 16 byte register boundaries, the last isn't padded.
    fn size(&self) -> u32 {
        match self.elements {
            0 | 1 => self.element_size,
            n => self.element_size.next_multiple_of(16) * (u32::from(n) - 1) + self.element_size,
        }
    }
}

fn constant_type(c: &Constant) -> Result<Type> {
    let ty = match c.data_type {
        shader_param_type::Float => D3D_SVT_FLOAT,
        shader_param_type::Int => D3D_SVT_INT,
        shader_param_type::Bool => D3D_SVT_BOOL,
        shader_param_type::UInt => D3D_SVT_UINT,
        // TODO: half and short constants, once data with them turns up
        other => bail!(
            "{}: ShaderParamType {other} has no D3D11 equivalent here",
            c.name
        ),
    };
    let elements = u16::try_from(c.array_size)?;
    if c.is_matrix {
        let columns = match c.columns {
            Some(columns) => columns,
            None if c.rows == 4 => 4,
            // TODO: column count of non-4x4 matrices from m_CommonParameters
            None => bail!(
                "{}: matrix with {} rows has no known column count",
                c.name,
                c.rows
            ),
        };
        Ok(Type {
            class: D3D_SVC_MATRIX_COLUMNS,
            ty,
            rows: u16::try_from(c.rows)?,
            columns: u16::try_from(columns)?,
            elements,
            members: Vec::new(),
            // column-major: one 16 byte register per column, the last one unpadded
            element_size: 16 * (columns - 1) + 4 * c.rows,
        })
    } else {
        ensure!(c.rows == 1, "{}: vector with {} rows", c.name, c.rows);
        let columns = c.columns.context("vector without column count")?;
        Ok(Type {
            class: if columns == 1 {
                D3D_SVC_SCALAR
            } else {
                D3D_SVC_VECTOR
            },
            ty,
            rows: 1,
            columns: u16::try_from(columns)?,
            elements,
            members: Vec::new(),
            element_size: 4 * columns,
        })
    }
}

fn struct_type(s: &StructConstant) -> Result<Type> {
    let mut members = s
        .members
        .iter()
        .map(|m| Ok((m.name.clone(), m.offset, constant_type(m)?)))
        .collect::<Result<Vec<_>>>()?;
    members.sort_by_key(|(_, offset, _)| *offset);
    Ok(Type {
        class: D3D_SVC_STRUCT,
        ty: D3D_SVT_VOID,
        rows: 1,
        columns: u16::try_from(s.struct_size / 4)?,
        elements: u16::try_from(s.array_size)?,
        members,
        element_size: s.struct_size,
    })
}

/// The SRV dimension of a Unity `TextureDimension`.
fn srv_dimension(dimension: u32, multisampled: bool) -> Result<D3D_SRV_DIMENSION> {
    Ok(match (dimension, multisampled) {
        (texture_dimension::Tex2D, false) => D3D_SRV_DIMENSION_TEXTURE2D,
        (texture_dimension::Tex2D, true) => D3D_SRV_DIMENSION_TEXTURE2DMS,
        (texture_dimension::Tex3D, false) => D3D_SRV_DIMENSION_TEXTURE3D,
        (texture_dimension::Cube, false) => D3D_SRV_DIMENSION_TEXTURECUBE,
        (texture_dimension::Tex2DArray, false) => D3D_SRV_DIMENSION_TEXTURE2DARRAY,
        (texture_dimension::Tex2DArray, true) => D3D_SRV_DIMENSION_TEXTURE2DMSARRAY,
        (texture_dimension::CubeArray, false) => D3D_SRV_DIMENSION_TEXTURECUBEARRAY,
        other => bail!("no D3D11 resource dimension for TextureDimension {other:?}"),
    })
}

/// The part of the chunk after the fixed tables: strings, types and struct members.
#[derive(Default)]
struct Layout {
    cursor: usize,
    start: Option<usize>,
    data: Vec<u8>,
    strings: Vec<(String, u32)>,
    types: Vec<(Type, u32)>,
}

impl Layout {
    fn base(&mut self) -> usize {
        *self.start.get_or_insert(self.cursor)
    }

    fn offset(&mut self) -> u32 {
        (self.base() + self.data.len()) as u32
    }

    fn string(&mut self, value: &str) -> u32 {
        if let Some((_, offset)) = self.strings.iter().find(|(s, _)| s == value) {
            return *offset;
        }
        let offset = self.offset();
        self.data.extend_from_slice(value.as_bytes());
        self.data.push(0);
        while self.data.len() % 4 != 0 {
            self.data.push(0xab);
        }
        self.strings.push((value.to_owned(), offset));
        offset
    }

    fn ty(&mut self, ty: &Type) -> u32 {
        if let Some((_, offset)) = self.types.iter().find(|(t, _)| t == ty) {
            return *offset;
        }
        let members: Vec<(u32, u32, u32)> = ty
            .members
            .iter()
            .map(|(name, offset, member)| (self.string(name), self.ty(member), *offset))
            .collect();
        let member_offset = if members.is_empty() { 0 } else { self.offset() };
        for (name, member_type, offset) in members {
            for value in [name, member_type, offset] {
                self.data.extend_from_slice(&value.to_le_bytes());
            }
        }
        let offset = self.offset();
        let narrow =
            |value: u32| u16::try_from(value).expect("D3D enum value exceeds its 16 bit field");
        for value in [
            narrow(ty.class),
            narrow(ty.ty),
            ty.rows,
            ty.columns,
            ty.elements,
            ty.members.len() as u16,
        ] {
            self.data.extend_from_slice(&value.to_le_bytes());
        }
        self.data.extend_from_slice(&member_offset.to_le_bytes());
        // SM5 type descriptors: four unused words and a type name, which consumers don't need
        self.data.extend_from_slice(&[0; 20]);
        self.types.push((ty.clone(), offset));
        offset
    }

    fn finish(self) -> Vec<u8> {
        self.data
    }
}
