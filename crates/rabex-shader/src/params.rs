//! The constants and resources a program variant uses: a parameter blob entry of
//! Unity 2022+ shader blobs, with the constant buffer layouts and resource bindings.
//! Unity 2020 keeps these tables inline in each program entry instead.

use std::collections::HashMap;

use indexmap::IndexMap;

use anyhow::{Context, Result, bail, ensure};

use crate::types::{
    BufferBinding, MatrixParameter, SerializedProgramParameters, StructParameter, VectorParameter,
};
use crate::unity::shader_param_type::ShaderParamType;
use crate::{PROGRAM_VERSION_202012090, Reader, Writer};

/// Parameters of a program variant, either from its parameter blob, from the program's
/// `m_CommonParameters`, or both merged.
#[derive(Clone, Debug)]
pub struct ProgramParameters {
    /// The first scope is the unnamed one holding constants outside any constant buffer, like Vulkan
    /// specialization constants.
    pub constant_buffers: Vec<ConstantBufferLayout>,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Debug)]
pub struct ConstantBufferLayout {
    pub name: String,
    pub size: u32,
    pub constants: Vec<Constant>,
    pub structs: Vec<StructConstant>,
}

/// A struct-typed constant, as used by instancing constant buffers.
#[derive(Clone, Debug)]
pub struct StructConstant {
    pub name: String,
    pub offset: u32,
    pub array_size: u32,
    pub struct_size: u32,
    /// Offsets are relative to the struct.
    pub members: Vec<Constant>,
}

#[derive(Clone, Debug)]
pub struct Constant {
    pub name: String,
    /// `ShaderParamType`: float, int, bool, half, short, uint.
    pub data_type: u32,
    pub rows: u32,
    /// Unknown for matrices from `m_CommonParameters`, which only store their row count.
    pub columns: Option<u32>,
    pub is_matrix: bool,
    pub array_size: u32,
    /// Byte offset in the constant buffer.
    pub offset: u32,
}

#[derive(Clone, Debug)]
pub enum Binding {
    Texture {
        name: String,
        index: i32,
        /// `-1` without a sampler.
        sampler_index: i32,
        /// `TextureDimension`.
        dimension: u32,
        multisampled: bool,
    },
    ConstantBuffer {
        name: String,
        index: i32,
        array_size: u32,
    },
    Buffer {
        name: String,
        index: i32,
        array_size: u32,
    },
    /// A writable resource like `RWTexture2D` or `RWStructuredBuffer`.
    Uav {
        name: String,
        index: i32,
        /// The `register(uN)` slot from the source.
        original_index: u32,
    },
    /// An inline sampler state like `sampler_linear_clamp`.
    Sampler {
        name: String,
        index: i32,
        state: u32,
    },
}

impl Binding {
    pub fn name(&self) -> &str {
        match self {
            Binding::Texture { name, .. }
            | Binding::ConstantBuffer { name, .. }
            | Binding::Buffer { name, .. }
            | Binding::Uav { name, .. }
            | Binding::Sampler { name, .. } => name,
        }
    }

    fn kind(&self) -> u32 {
        match self {
            Binding::Texture { .. } => 0,
            Binding::ConstantBuffer { .. } => 1,
            Binding::Buffer { .. } => 2,
            Binding::Uav { .. } => 3,
            Binding::Sampler { .. } => 4,
        }
    }
}

impl ProgramParameters {
    /// Parse a variant's parameter blob, which holds what the variant doesn't share with
    /// `m_CommonParameters`.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        ensure!(
            version == PROGRAM_VERSION_202012090,
            "unsupported parameter version {version}"
        );
        let constant_buffers = (0..r.u32()?)
            .map(|_| r.scope())
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            constant_buffers
                .first()
                .is_some_and(|scope| scope.name.is_empty()),
            "parameter blob doesn't start with the unnamed scope"
        );
        let bindings = r.bindings()?;
        r.finish()?;
        Ok(ProgramParameters {
            constant_buffers,
            bindings,
        })
    }

    /// Serialize as a variant's parameter blob.
    pub fn to_blob(&self) -> Result<Vec<u8>> {
        let mut w = Writer::default();
        w.u32(PROGRAM_VERSION_202012090);
        w.u32(u32::try_from(self.constant_buffers.len())?);
        for cb in &self.constant_buffers {
            w.scope(cb)?;
        }
        w.bindings(&self.bindings)?;
        Ok(w.finish())
    }
}

/// The parameters a 201806140 program entry carries inline after its code: the vertex
/// channel bindings, then the constant buffer scopes and resource bindings. Like the
/// 202012090 parameter blobs, the scope list starts with the unnamed scope holding
/// constants outside any constant buffer, like Vulkan specialization constants.
#[derive(Clone, Debug)]
pub struct InlineParameters {
    pub source_map: i32,
    /// `(source, target)` vertex channel bindings, one per set bit of `source_map`.
    pub channels: Vec<(i32, i32)>,
    /// The first scope is the unnamed one, like in [`ProgramParameters`].
    pub constant_buffers: Vec<ConstantBufferLayout>,
    pub bindings: Vec<Binding>,
}

impl InlineParameters {
    /// Parse the inline parameters of a 201806140 program entry, everything after its code.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let source_map = r.i32()?;
        let channels = (0..r.u32()?)
            .map(|_| Ok((r.i32()?, r.i32()?)))
            .collect::<Result<_>>()?;
        let scope_count = r.u32()?;
        let constant_buffers = (0..scope_count)
            .map(|_| r.scope())
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            constant_buffers
                .first()
                .is_some_and(|scope| scope.name.is_empty()),
            "the inline parameters don't start with the unnamed scope"
        );
        let bindings = r.bindings()?;
        r.finish()?;
        Ok(Self {
            source_map,
            channels,
            constant_buffers,
            bindings,
        })
    }

    /// Serialize as the inline parameters of a 201806140 program entry.
    pub fn to_inline(&self) -> Result<Vec<u8>> {
        let mut w = Writer::default();
        w.i32(self.source_map);
        w.u32(u32::try_from(self.channels.len())?);
        for &(source, target) in &self.channels {
            w.i32(source);
            w.i32(target);
        }
        w.u32(u32::try_from(self.constant_buffers.len())?);
        for cb in &self.constant_buffers {
            w.scope(cb)?;
        }
        w.bindings(&self.bindings)?;
        Ok(w.finish())
    }
}

impl ProgramParameters {
    /// Convert a program's `m_CommonParameters`, naming entries through the pass's `m_NameIndices`.
    pub fn from_common(
        common: &SerializedProgramParameters,
        name_indices: &IndexMap<String, i32>,
    ) -> Result<Self> {
        let names: HashMap<i32, &str> = name_indices
            .iter()
            .map(|(name, &index)| (index, name.as_str()))
            .collect();
        let name = |index: i32| -> Result<String> {
            names
                .get(&index)
                .map(|name| name.to_string())
                .with_context(|| format!("no name for index {index}"))
        };
        let constants = |vectors: &[VectorParameter], matrices: &[MatrixParameter]| {
            let vectors = vectors.iter().map(|v| {
                Ok(Constant {
                    name: name(v.m_NameIndex)?,
                    data_type: ShaderParamType::try_from(i64::from(v.m_Type))?.value(),
                    rows: 1,
                    columns: Some(u32::try_from(v.m_Dim)?),
                    is_matrix: false,
                    array_size: u32::try_from(v.m_ArraySize)?,
                    offset: u32::try_from(v.m_Index)?,
                })
            });
            let matrices = matrices.iter().map(|m| {
                Ok(Constant {
                    name: name(m.m_NameIndex)?,
                    data_type: ShaderParamType::try_from(i64::from(m.m_Type))?.value(),
                    rows: u32::try_from(m.m_RowCount)?,
                    columns: None,
                    is_matrix: true,
                    array_size: u32::try_from(m.m_ArraySize)?,
                    offset: u32::try_from(m.m_Index)?,
                })
            });
            vectors.chain(matrices).collect::<Result<Vec<_>>>()
        };
        let structs = |structs: &[StructParameter]| {
            structs
                .iter()
                .map(|s| {
                    Ok(StructConstant {
                        name: name(s.m_NameIndex)?,
                        offset: u32::try_from(s.m_Index)?,
                        array_size: u32::try_from(s.m_ArraySize)?,
                        struct_size: u32::try_from(s.m_StructSize)?,
                        members: constants(&s.m_VectorMembers, &s.m_MatrixMembers)?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        };

        let mut constant_buffers = vec![ConstantBufferLayout {
            name: String::new(),
            size: 0,
            constants: constants(&common.m_VectorParams, &common.m_MatrixParams)?,
            structs: Vec::new(),
        }];
        for cb in &common.m_ConstantBuffers {
            constant_buffers.push(ConstantBufferLayout {
                name: name(cb.m_NameIndex)?,
                size: u32::try_from(cb.m_Size)?,
                constants: constants(&cb.m_VectorParams, &cb.m_MatrixParams)?,
                structs: structs(&cb.m_StructParams)?,
            });
        }

        let buffer = |b: &BufferBinding| -> Result<(String, i32, u32)> {
            Ok((
                name(b.m_NameIndex)?,
                b.m_Index,
                // Unity 2020+ stores it; common parameters are 2022+
                u32::try_from(b.m_ArraySize.with_context(|| {
                    format!("buffer binding {} without array size", b.m_NameIndex)
                })?)?,
            ))
        };
        let mut bindings = Vec::new();
        for t in &common.m_TextureParams {
            bindings.push(Binding::Texture {
                name: name(t.m_NameIndex)?,
                index: t.m_Index,
                sampler_index: t.m_SamplerIndex,
                dimension: u32::try_from(t.m_Dim)?,
                multisampled: t.m_MultiSampled,
            });
        }
        for b in &common.m_ConstantBufferBindings {
            let (name, index, array_size) = buffer(b)?;
            bindings.push(Binding::ConstantBuffer {
                name,
                index,
                array_size,
            });
        }
        for b in &common.m_BufferParams {
            let (name, index, array_size) = buffer(b)?;
            bindings.push(Binding::Buffer {
                name,
                index,
                array_size,
            });
        }
        for u in &common.m_UAVParams {
            bindings.push(Binding::Uav {
                name: name(u.m_NameIndex)?,
                index: u.m_Index,
                original_index: u32::try_from(u.m_OriginalIndex)?,
            });
        }
        for s in &common.m_Samplers {
            bindings.push(Binding::Sampler {
                name: String::new(),
                index: s.bindPoint,
                state: s.sampler,
            });
        }
        Ok(ProgramParameters {
            constant_buffers,
            bindings,
        })
    }

    /// The parameters of a variant: the program's common ones plus the variant's own.
    pub fn merge(mut common: ProgramParameters, variant: ProgramParameters) -> Result<Self> {
        for cb in variant.constant_buffers {
            match common
                .constant_buffers
                .iter_mut()
                .find(|c| c.name == cb.name)
            {
                Some(existing) => {
                    // common parameters are shared across platforms, whose layouts can differ in size
                    existing.size = cb.size;
                    for constant in cb.constants {
                        ensure!(
                            !existing.constants.iter().any(|c| c.name == constant.name),
                            "{}.{} is both common and per variant",
                            cb.name,
                            constant.name
                        );
                        existing.constants.push(constant);
                    }
                    for s in cb.structs {
                        ensure!(
                            !existing.structs.iter().any(|c| c.name == s.name),
                            "{}.{} is both common and per variant",
                            cb.name,
                            s.name
                        );
                        existing.structs.push(s);
                    }
                }
                None => common.constant_buffers.push(cb),
            }
        }
        for binding in variant.bindings {
            ensure!(
                binding.name().is_empty()
                    || !common
                        .bindings
                        .iter()
                        .any(|b| b.kind() == binding.kind() && b.name() == binding.name()),
                "binding {} is both common and per variant",
                binding.name()
            );
            common.bindings.push(binding);
        }
        Ok(common)
    }
}

impl Reader<'_> {
    fn constant(&mut self) -> Result<Constant> {
        Ok(Constant {
            name: self.string()?,
            data_type: self.u32()?,
            rows: self.u32()?,
            columns: Some(self.u32()?),
            is_matrix: self.bool32()?,
            array_size: self.u32()?,
            offset: self.u32()?,
        })
    }

    fn scope(&mut self) -> Result<ConstantBufferLayout> {
        let name = self.string()?;
        let size = self.u32()?;
        let constants = (0..self.u32()?)
            .map(|_| self.constant())
            .collect::<Result<_>>()?;
        let structs = (0..self.u32()?)
            .map(|_| {
                Ok(StructConstant {
                    name: self.string()?,
                    offset: self.u32()?,
                    array_size: self.u32()?,
                    struct_size: self.u32()?,
                    members: (0..self.u32()?)
                        .map(|_| self.constant())
                        .collect::<Result<_>>()?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(ConstantBufferLayout {
            name,
            size,
            constants,
            structs,
        })
    }

    fn bindings(&mut self) -> Result<Vec<Binding>> {
        (0..self.u32()?)
            .map(|_| {
                let name = self.string()?;
                Ok(match self.u32()? {
                    0 => {
                        let index = self.i32()?;
                        let sampler_index = self.i32()?;
                        let packed = self.u32()?;
                        Binding::Texture {
                            name,
                            index,
                            sampler_index,
                            dimension: packed >> 1,
                            multisampled: packed & 1 == 1,
                        }
                    }
                    1 => Binding::ConstantBuffer {
                        name,
                        index: self.i32()?,
                        array_size: self.u32()?,
                    },
                    2 => Binding::Buffer {
                        name,
                        index: self.i32()?,
                        array_size: self.u32()?,
                    },
                    3 => Binding::Uav {
                        name,
                        index: self.i32()?,
                        original_index: self.u32()?,
                    },
                    4 => Binding::Sampler {
                        name,
                        index: self.i32()?,
                        state: self.u32()?,
                    },
                    kind => bail!("{name}: unknown binding kind {kind}"),
                })
            })
            .collect()
    }
}

impl Writer {
    fn scope(&mut self, cb: &ConstantBufferLayout) -> Result<()> {
        self.string(&cb.name)?;
        self.u32(cb.size);
        self.u32(u32::try_from(cb.constants.len())?);
        for constant in &cb.constants {
            self.constant(constant)?;
        }
        self.u32(u32::try_from(cb.structs.len())?);
        for s in &cb.structs {
            self.string(&s.name)?;
            self.u32(s.offset);
            self.u32(s.array_size);
            self.u32(s.struct_size);
            self.u32(u32::try_from(s.members.len())?);
            for member in &s.members {
                self.constant(member)?;
            }
        }
        Ok(())
    }

    fn bindings(&mut self, bindings: &[Binding]) -> Result<()> {
        self.u32(u32::try_from(bindings.len())?);
        for binding in bindings {
            self.string(binding.name())?;
            self.u32(binding.kind());
            match *binding {
                Binding::Texture {
                    index,
                    sampler_index,
                    dimension,
                    multisampled,
                    ..
                } => {
                    self.i32(index);
                    self.i32(sampler_index);
                    self.u32(dimension << 1 | u32::from(multisampled));
                }
                Binding::ConstantBuffer {
                    index, array_size, ..
                }
                | Binding::Buffer {
                    index, array_size, ..
                } => {
                    self.i32(index);
                    self.u32(array_size);
                }
                Binding::Uav {
                    index,
                    original_index,
                    ..
                } => {
                    self.i32(index);
                    self.u32(original_index);
                }
                Binding::Sampler { index, state, .. } => {
                    self.i32(index);
                    self.u32(state);
                }
            }
        }
        Ok(())
    }

    fn constant(&mut self, constant: &Constant) -> Result<()> {
        self.string(&constant.name)?;
        self.u32(constant.data_type);
        self.u32(constant.rows);
        self.u32(
            constant
                .columns
                .with_context(|| format!("{}: column count unknown", constant.name))?,
        );
        self.bool32(constant.is_matrix);
        self.u32(constant.array_size);
        self.u32(constant.offset);
        Ok(())
    }
}
