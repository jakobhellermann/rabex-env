//! The shader downport: Unity 2022+/6000 serialized form to Unity 2020.2.
//!
//! Unity 2021.2 changed the serialized shader grammar: keyword names moved from the
//! passes to the shader (`m_KeywordNames`, `m_KeywordFlags`), the sub programs became
//! tiered player sub programs whose parameters live in their own blob entries, and the
//! program entries dropped their inline parameter tables. The 2020.2 form that the
//! downport rebuilds keeps the whole mapping pass-local: a pass name table
//! (`m_NameIndices`) gives every keyword, constant and binding a slot, the sub programs
//! reference keywords by global and local index lists, and the parameters travel both as
//! flat fields on the sub program and as the inline tables of its 201806140 program
//! entry.
//!
//! Unity's own slot numbering in `m_NameIndices` follows the editor's import order and
//! is not reproducible from the serialized data; the downport assigns slots in first
//! encounter order instead. The numbering is an internal detail — the player resolves
//! names through the table — so any consistent assignment loads and binds the same.

use std::collections::HashMap;

use anyhow::{Context, Result, bail, ensure};
use indexmap::IndexMap;
use rabex_shader::types::{
    BufferBinding, ConstantBuffer, Hash128, MatrixParameter, ParserBindChannels, SamplerParameter,
    SerializedPass, SerializedPlayerSubProgram, SerializedProgram, SerializedProgramParameters,
    SerializedSubProgram, ShaderBindChannel, StructParameter, TextureParameter, UAVParameter,
    VectorParameter,
};
use rabex_shader::vulkan::Form;
use rabex_shader::{
    Binding, Constant, InlineParameters, Platform, PlatformBlob, ProgramParameters, Shader,
    StructConstant, SubProgram, SubProgramLayout,
};

/// The pre-2021.2 serialized form of one shader, from the 2021.2+ form.
fn downport(shader: &Shader, chunk_size: usize) -> Result<Shader> {
    let mut out = shader.clone();
    // shader-level fields the 2020.2 form lacks
    out.stageCounts = None;
    out.m_AssetGUID = None;
    out.m_AssetLocalIdentifierInFile = None;
    out.m_ParsedForm.m_KeywordNames = None;
    out.m_ParsedForm.m_KeywordFlags = None;
    out.m_ParsedForm.m_CustomEditorForRenderPipelines = None;

    let keyword_names = shader.m_ParsedForm.m_KeywordNames.as_deref().unwrap_or(&[]);
    let keyword_flags = shader.m_ParsedForm.m_KeywordFlags.as_deref().unwrap_or(&[]);
    ensure!(
        keyword_names.len() == keyword_flags.len(),
        "{}: keyword names and flags disagree in count",
        shader.m_ParsedForm.m_Name
    );
    let keyword = |index: u16| -> Result<(String, bool)> {
        let name = keyword_names
            .get(index as usize)
            .with_context(|| format!("keyword index {index} out of range"))?;
        // flag 1 marks shader-local keywords; 0 the global ones
        let flag = keyword_flags[index as usize];
        ensure!(
            flag <= 1,
            "{}: unknown keyword flag {flag}",
            name.escape_debug()
        );
        Ok((name.clone(), flag == 1))
    };

    let blobs = shader.platform_blobs()?;
    ensure!(!blobs.is_empty(), "{}: no platform blobs", out.m_Name);
    let mut state = PassState::default();
    for (si, sub_shader) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
        for (pi, pass) in sub_shader.m_Passes.iter().enumerate() {
            let label = format!("{}: sub shader {si} pass {pi}", out.m_Name);
            let converted = ConvertedPass::convert(pass, &blobs, &keyword, &mut state)
                .with_context(|| label.clone())?;
            let out_pass = &mut out.m_ParsedForm.m_SubShaders[si].m_Passes[pi];
            out_pass.m_NameIndices = converted.name_indices;
            out_pass.m_GlobalKeywordMask = Some(converted.global_mask);
            out_pass.m_LocalKeywordMask = Some(converted.local_mask);
            // the hash is an editor cache key of the version that built the shader;
            // the downport has none to offer, so the entries stay zeroed
            out_pass.m_EditorDataHash = Some(
                (0..blobs.len())
                    .map(|_| Hash128 {
                        bytes_0: 0,
                        bytes_1: 0,
                        bytes_2: 0,
                        bytes_3: 0,
                        bytes_4: 0,
                        bytes_5: 0,
                        bytes_6: 0,
                        bytes_7: 0,
                        bytes_8: 0,
                        bytes_9: 0,
                        bytes_10: 0,
                        bytes_11: 0,
                        bytes_12: 0,
                        bytes_13: 0,
                        bytes_14: 0,
                        bytes_15: 0,
                    })
                    .collect(),
            );
            out_pass.m_Platforms = Some(blobs.iter().map(|b| b.platform.id() as u8).collect());
            let mut programs = [
                &mut out_pass.progVertex,
                &mut out_pass.progFragment,
                &mut out_pass.progGeometry,
                &mut out_pass.progHull,
                &mut out_pass.progDomain,
                &mut out_pass.progRayTracing,
            ];
            for (program, subs) in programs.iter_mut().zip(&converted.subs) {
                write_program(program, subs);
            }
        }
    }
    // shaders without programs (everything falls back) keep their empty blobs
    let mut platforms = Vec::new();
    if state.entries.is_empty() {
        for blob in &blobs {
            platforms.push(PlatformBlob::from_entries(
                blob.platform,
                &[] as &[Vec<u8>],
                chunk_size,
            )?);
        }
    }
    for (platform, entries) in state.entries {
        platforms.push(PlatformBlob::from_entries(platform, &entries, chunk_size)?);
    }
    out.set_platform_blobs(&platforms)?;
    Ok(out)
}

/// The blob entries the passes rebuild, accumulated across them.
#[derive(Default)]
struct PassState {
    entries: Vec<(Platform, Vec<Vec<u8>>)>,
    /// The downport's rebuilt program entries by source entry.
    converted_indices: HashMap<(u32, u32), u32>,
    /// The upport's program and parameter entries by source entry.
    upported_indices: HashMap<(u32, u32), (u32, u32)>,
    /// The upport's keyword registry: name to scope flag.
    keywords: IndexMap<String, u8>,
}

/// The sub programs of the six stage programs, by position in `SerializedPass::programs`.
type StageSubs = [Vec<SerializedSubProgram>; 6];

/// One pass's conversion result.
struct ConvertedPass {
    name_indices: IndexMap<String, i32>,
    global_mask: Vec<u16>,
    local_mask: Vec<u16>,
    subs: StageSubs,
}

impl ConvertedPass {
    fn convert(
        pass: &SerializedPass,
        blobs: &[PlatformBlob],
        keyword: &impl Fn(u16) -> Result<(String, bool)>,
        state: &mut PassState,
    ) -> Result<Self> {
        let mut table = NameTable::default();
        let (mut global_mask, mut local_mask) = (Vec::new(), Vec::new());
        let entries = &mut state.entries;
        let converted_indices = &mut state.converted_indices;
        let mut subs: StageSubs = Default::default();

        for (position, (stage, program)) in pass.programs().iter().enumerate() {
            let Some(tiers) = &program.m_PlayerSubPrograms else {
                if program.m_SubPrograms.is_empty() {
                    continue;
                }
                bail!("{stage:?}: already carries 2020.2 style sub programs");
            };
            let parameter_indices = program.m_ParameterBlobIndices.as_ref().with_context(|| {
                format!("{stage:?}: player sub programs without parameter blob indices")
            })?;
            ensure!(
                tiers.len() == parameter_indices.len(),
                "{stage:?}: sub program and parameter tiers disagree"
            );
            // Unity 2020.2 writes a single sub program per variant. The 2022+ editors
            // list every hardware tier, all but one empty; the filled one is the
            // highest, which is what the 2020.2 build carries.
            let filled: Vec<usize> = (0..tiers.len()).filter(|&t| !tiers[t].is_empty()).collect();
            let tier = match filled.as_slice() {
                [] => continue,
                [only] => *only,
                many => bail!("{stage:?}: {} filled hardware tiers {many:?}", many.len()),
            };
            let common = ProgramParameters::from_common(
                program.m_CommonParameters.as_ref().with_context(|| {
                    format!("{stage:?}: player sub programs without common parameters")
                })?,
                &pass.m_NameIndices,
            )
            .with_context(|| format!("{stage:?}: common parameters"))?;
            let subs_tier = &tiers[tier];
            let parameter_indices = &parameter_indices[tier];
            ensure!(
                subs_tier.len() == parameter_indices.len(),
                "{stage:?}: sub programs and parameter blobs disagree in count"
            );
            let mut stage_subs = Vec::new();
            for (sub, &parameter_index) in subs_tier.iter().zip(parameter_indices) {
                let program_type = u32::try_from(sub.m_GpuProgramType)?;
                let label = || {
                    format!(
                        "{stage:?} blob {} keywords {:?}",
                        sub.m_BlobIndex, sub.m_KeywordIndices
                    )
                };
                let mut running = Vec::new();
                for blob in blobs {
                    if blob.platform.runs(program_type)? {
                        running.push(blob);
                    }
                }
                let blob = running.first().with_context(|| {
                    format!("{}: no blob runs program type {program_type}", label())
                })?;
                ensure!(
                    running.len() == 1,
                    "{}: program type {program_type} runs on {} platforms",
                    label(),
                    running.len()
                );
                let entry = |index: u32| -> Result<Vec<u8>> {
                    let location = *blob
                        .entries()?
                        .get(index as usize)
                        .with_context(|| format!("{}: blob index {index}", label()))?;
                    Ok(blob.entry(location)?.to_vec())
                };
                let source = SubProgram::parse(&entry(sub.m_BlobIndex)?).with_context(label)?;
                let (source_map, channels) = match &source.layout {
                    SubProgramLayout::V202012090 {
                        source_map,
                        channels,
                    } => (*source_map, channels.clone()),
                    SubProgramLayout::V201806140 { .. } => {
                        bail!("{}: program entry already has the 2020.2 layout", label())
                    }
                };
                let own = ProgramParameters::parse(&entry(parameter_index)?)
                    .with_context(|| format!("{}: parameters {parameter_index}", label()))?;
                let mut parameters =
                    ProgramParameters::merge(common.clone(), own).with_context(label)?;
                // the common parameters store matrices with their row count only; the
                // inline tables need both, and every matrix the serialized form carries
                // has the four columns of Unity's column-major float4 columns
                for scope in &mut parameters.constant_buffers {
                    for constant in &mut scope.constants {
                        if constant.is_matrix && constant.columns.is_none() {
                            constant.columns = Some(4);
                        }
                    }
                }

                // the variant's keywords split by scope into the two lists the 2020.2
                // entry and the sub program carry
                let (mut globals, mut locals) = (Vec::new(), Vec::new());
                for &index in &sub.m_KeywordIndices {
                    let (name, is_local) = keyword(index).with_context(label)?;
                    if is_local {
                        locals.push(name.to_string());
                    } else {
                        globals.push(name.to_string());
                    }
                }
                let global_indices: Vec<u16> = globals
                    .iter()
                    .map(|name| u16::try_from(table.slot(name)).with_context(label))
                    .collect::<Result<_>>()?;
                let local_indices: Vec<u16> = locals
                    .iter()
                    .map(|name| u16::try_from(table.slot(name)).with_context(label))
                    .collect::<Result<_>>()?;
                global_mask.extend(global_indices.iter().copied());
                local_mask.extend(local_indices.iter().copied());
                // one entry per source entry, shared by every reference to it
                let key = (blob.platform.id(), sub.m_BlobIndex);
                let new_index = match converted_indices.get(&key) {
                    Some(&index) => index,
                    None => {
                        // Vulkan programs carry code the older player cannot feed:
                        // combined image samplers, and a container with stage hashes
                        let (code, pairs) = match blob.platform {
                            Platform::Vulkan => downport_vulkan_program(&source.code)
                                .with_context(label)?
                                .unwrap_or_else(|| (source.code.clone(), HashMap::new())),
                            _ => (source.code.clone(), HashMap::new()),
                        };
                        let bindings = match pairs.is_empty() {
                            true => inline_bindings(&parameters.bindings, blob.platform),
                            false => rebound_textures(
                                &inline_bindings(&parameters.bindings, blob.platform),
                                &pairs,
                            ),
                        };
                        let inline = InlineParameters {
                            source_map: source_map as i32,
                            channels: channels
                                .iter()
                                .map(|&(source, target)| (source as i32, target as i32))
                                .collect(),
                            constant_buffers: parameters.constant_buffers.clone(),
                            bindings,
                        };
                        let wrapped = SubProgram {
                            program_type,
                            stats: source.stats.clone(),
                            keywords: globals.clone(),
                            code,
                            layout: SubProgramLayout::V201806140 {
                                local_keywords: locals.clone(),
                                parameters: inline,
                            },
                        };
                        let bytes = wrapped.to_bytes()?;
                        let position = entries
                            .iter()
                            .position(|(platform, _)| *platform == blob.platform)
                            .unwrap_or_else(|| {
                                entries.push((blob.platform, Vec::new()));
                                entries.len() - 1
                            });
                        let list = &mut entries[position].1;
                        list.push(bytes);
                        let index = u32::try_from(list.len() - 1)?;
                        converted_indices.insert(key, index);
                        index
                    }
                };

                let flat = flat_parameters(&parameters, &mut table)?;
                stage_subs.push(SerializedSubProgram {
                    m_BlobIndex: new_index,
                    m_Channels: ParserBindChannels {
                        m_Channels: channels
                            .iter()
                            .map(|&(source, target)| ShaderBindChannel {
                                source: source as i8,
                                target: target as i8,
                            })
                            .collect(),
                        m_SourceMap: source_map as i32,
                    },
                    m_GlobalKeywordIndices: Some(global_indices),
                    m_LocalKeywordIndices: Some(local_indices),
                    // Unity 2020.2 writes its highest hardware tier
                    m_ShaderHardwareTier: 3,
                    m_GpuProgramType: sub.m_GpuProgramType,
                    m_VectorParams: Some(flat.vector),
                    m_MatrixParams: Some(flat.matrix),
                    m_TextureParams: Some(flat.texture),
                    m_BufferParams: Some(flat.buffer),
                    m_ConstantBuffers: Some(flat.constant_buffers),
                    m_ConstantBufferBindings: Some(flat.constant_buffer_bindings),
                    m_UAVParams: Some(flat.uav),
                    m_Samplers: Some(flat.samplers),
                    m_ShaderRequirements: sub.m_ShaderRequirements,
                    m_KeywordIndices: None,
                    m_Parameters: None,
                });
            }
            subs[position] = stage_subs;
        }
        Ok(ConvertedPass {
            name_indices: table.finish(),
            global_mask: dedup(global_mask),
            local_mask: dedup(local_mask),
            subs,
        })
    }
}

/// A rewritten Vulkan program and the `(texture, sampler)` bindings its split handed
/// out, keyed by the texture's `(stage slot, descriptor set, binding)` — the texture
/// keeps the binding it had, so that triple is what the entry already names it by.
type DownportedProgram = (Vec<u8>, HashMap<(usize, u32, u32), u32>);

/// Rewrite a Vulkan program into the older form: its stages lose the combined image
/// samplers that only the newer players bind, and the container drops the per stage
/// hashes the older one does not carry. The textures' new bindings come back so the
/// entry can announce them.
///
/// Returns `None` for a program that already is in that form.
fn downport_vulkan_program(code: &[u8]) -> Result<Option<DownportedProgram>> {
    let mut program = rabex_shader::vulkan::VulkanProgram::parse(code)?;
    let mut pairs: HashMap<(usize, u32, u32), u32> = HashMap::new();
    for (slot, stage) in program.stages.iter_mut().enumerate() {
        let Some(stage) = stage else { continue };
        let mut words = smolv::decode(&stage.smolv)?;
        let split = rabex_shader_convert::split_samplers::split_combined_samplers(&mut words)?;
        if split.is_empty() {
            continue;
        }
        // the bindings the split handed out, read back off the module so the entry and
        // the code cannot drift apart
        let resources = rabex_shader_convert::spirv::resources(&words)?;
        for (image, sampler) in &split {
            let at = |id: u32| {
                resources
                    .iter()
                    .find(|resource| resource.id == id)
                    .map(|resource| (resource.set, resource.binding))
            };
            let (Some((set, texture)), Some((_, sampler))) = (at(*image), at(*sampler)) else {
                bail!("a split half without a binding");
            };
            pairs.insert((slot, set, texture), sampler);
        }
        stage.smolv = smolv::encode(&words)?;
    }
    // the flags of the older form: it has no bit for a program whose samplers are all
    // combined (none are, after the split), and never sets the high one
    use rabex_shader::vulkan::flags;
    let downported = program.flags & !(flags::HIGH_SEEN | flags::COMBINED_SAMPLERS_ONLY);
    if pairs.is_empty() && program.form == Form::Plain && downported == program.flags {
        return Ok(None);
    }
    program.flags = downported;
    program.form = Form::Plain;
    Ok(Some((program.to_bytes()?, pairs)))
}

/// Give each texture the sampler its module got, found by the stage, set and binding
/// its index already names. Going by the order the splits happened in would mix the
/// stages up, and the entry would then point the player at a descriptor the shader
/// does not read.
fn rebound_textures(bindings: &[Binding], pairs: &HashMap<(usize, u32, u32), u32>) -> Vec<Binding> {
    bindings
        .iter()
        .map(|binding| match binding {
            Binding::Texture {
                name,
                index,
                dimension,
                multisampled,
                ..
            } => {
                let stage = match (*index as u32) >> 24 {
                    0x04 => rabex_shader::vulkan::SLOT_VERTEX,
                    0x08 => rabex_shader::vulkan::SLOT_FRAGMENT,
                    _ => return binding.clone(),
                };
                let (set, texture) = (((*index as u32) >> 16) & 0xff, (*index as u32) & 0xffff);
                match pairs.get(&(stage, set, texture)) {
                    // the texture keeps its place; the sampler takes the one the split
                    // found free, in the same stage and set
                    Some(sampler) => Binding::Texture {
                        name: name.clone(),
                        index: *index,
                        sampler_index: (*index & !0xffff) | *sampler as i32,
                        dimension: *dimension,
                        multisampled: *multisampled,
                    },
                    None => binding.clone(),
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// The binding list the 2020.2 entry carries: OpenGLCore leaves the unnamed globals
/// block to its implicit binding, every other platform keeps the list as is.
fn inline_bindings(bindings: &[Binding], platform: Platform) -> Vec<Binding> {
    match platform {
        Platform::OpenGLCore => bindings
            .iter()
            .filter(|b| !matches!(b, Binding::ConstantBuffer { name, .. } if name == "$Globals"))
            .cloned()
            .collect(),
        _ => bindings.to_vec(),
    }
}

/// The flat parameter fields of the 2020.2 sub program, mirroring the inline tables.
struct FlatParameters {
    vector: Vec<VectorParameter>,
    matrix: Vec<MatrixParameter>,
    texture: Vec<TextureParameter>,
    buffer: Vec<BufferBinding>,
    constant_buffers: Vec<ConstantBuffer>,
    constant_buffer_bindings: Vec<BufferBinding>,
    uav: Vec<UAVParameter>,
    samplers: Vec<SamplerParameter>,
}

fn flat_parameters(
    parameters: &ProgramParameters,
    table: &mut NameTable,
) -> Result<FlatParameters> {
    let mut flat = FlatParameters {
        vector: Vec::new(),
        matrix: Vec::new(),
        texture: Vec::new(),
        buffer: Vec::new(),
        constant_buffers: Vec::new(),
        constant_buffer_bindings: Vec::new(),
        uav: Vec::new(),
        samplers: Vec::new(),
    };
    for scope in &parameters.constant_buffers {
        let vectors = scope
            .constants
            .iter()
            .filter(|c| !c.is_matrix)
            .map(|c| vector_parameter(c, table))
            .collect::<Result<_>>()?;
        let matrices = scope
            .constants
            .iter()
            .filter(|c| c.is_matrix)
            .map(|c| matrix_parameter(c, table))
            .collect::<Result<_>>()?;
        let structs = scope
            .structs
            .iter()
            .map(|s| struct_parameter(s, table))
            .collect::<Result<_>>()?;
        if scope.name.is_empty() {
            // the unnamed scope holds constants outside any buffer, like the Vulkan
            // specialization constants
            flat.vector.extend(vectors);
            flat.matrix.extend(matrices);
            continue;
        }
        flat.constant_buffers.push(ConstantBuffer {
            m_NameIndex: table.slot(&scope.name),
            m_MatrixParams: matrices,
            m_VectorParams: vectors,
            m_StructParams: structs,
            m_Size: scope.size as i32,
            m_IsPartialCB: None,
        });
    }
    for binding in &parameters.bindings {
        match binding {
            Binding::Texture {
                name,
                index,
                sampler_index,
                dimension,
                multisampled,
            } => flat.texture.push(TextureParameter {
                m_NameIndex: table.slot(name),
                m_Index: *index,
                m_SamplerIndex: *sampler_index,
                m_MultiSampled: *multisampled,
                m_Dim: *dimension as i8,
            }),
            Binding::ConstantBuffer {
                name,
                index,
                array_size,
            } => flat.constant_buffer_bindings.push(BufferBinding {
                m_NameIndex: table.slot(name),
                m_Index: *index,
                m_ArraySize: Some(*array_size as i32),
            }),
            Binding::Buffer {
                name,
                index,
                array_size,
            } => flat.buffer.push(BufferBinding {
                m_NameIndex: table.slot(name),
                m_Index: *index,
                m_ArraySize: Some(*array_size as i32),
            }),
            Binding::Uav {
                name,
                index,
                original_index,
            } => flat.uav.push(UAVParameter {
                m_NameIndex: table.slot(name),
                m_Index: *index,
                m_OriginalIndex: *original_index as i32,
            }),
            Binding::Sampler { index, state, .. } => flat.samplers.push(SamplerParameter {
                sampler: *state,
                bindPoint: *index,
            }),
        }
    }
    Ok(flat)
}

fn vector_parameter(c: &Constant, table: &mut NameTable) -> Result<VectorParameter> {
    Ok(VectorParameter {
        m_NameIndex: table.slot(&c.name),
        m_Index: c.offset as i32,
        m_ArraySize: c.array_size as i32,
        m_Type: c.data_type as i8,
        m_Dim: c.columns.unwrap_or(1) as i8,
    })
}

fn matrix_parameter(c: &Constant, table: &mut NameTable) -> Result<MatrixParameter> {
    Ok(MatrixParameter {
        m_NameIndex: table.slot(&c.name),
        m_Index: c.offset as i32,
        m_ArraySize: c.array_size as i32,
        m_Type: c.data_type as i8,
        m_RowCount: c.rows as i8,
    })
}

fn struct_parameter(s: &StructConstant, table: &mut NameTable) -> Result<StructParameter> {
    Ok(StructParameter {
        m_NameIndex: table.slot(&s.name),
        m_Index: s.offset as i32,
        m_ArraySize: s.array_size as i32,
        m_StructSize: s.struct_size as i32,
        m_VectorMembers: s
            .members
            .iter()
            .filter(|c| !c.is_matrix)
            .map(|c| vector_parameter(c, table))
            .collect::<Result<_>>()?,
        m_MatrixMembers: s
            .members
            .iter()
            .filter(|c| c.is_matrix)
            .map(|c| matrix_parameter(c, table))
            .collect::<Result<_>>()?,
    })
}

/// Assigns first-encounter slots for the pass's name table.
#[derive(Default)]
struct NameTable {
    map: IndexMap<String, i32>,
}

impl NameTable {
    fn slot(&mut self, name: &str) -> i32 {
        let next = self.map.len() as i32;
        *self.map.entry(name.to_string()).or_insert(next)
    }

    fn finish(self) -> IndexMap<String, i32> {
        self.map
    }
}

fn dedup(mut indices: Vec<u16>) -> Vec<u16> {
    let mut seen = std::collections::BTreeSet::new();
    indices.retain(|i| seen.insert(*i));
    indices
}

fn write_program(program: &mut SerializedProgram, subs: &[SerializedSubProgram]) {
    program.m_SubPrograms = subs.to_vec();
    program.m_PlayerSubPrograms = None;
    program.m_ParameterBlobIndices = None;
    program.m_CommonParameters = None;
    program.m_SerializedKeywordStateMask = None;
}

/// The 2021.2+ serialized form of one shader, from the pre-2021.2 form: the inverse of
/// [`downport`] for everything the forms carry. The keyword name list is rebuilt from
/// the variants' own lists, so the padding keywords the 2022+ editors mix into theirs
/// (the unused stereo set) drop out, the pass name table stays empty — the 2022+
/// editors leave theirs in an import order serialized data doesn't decide — and the
/// `m_AssetGUID` of a 6000 build has no source in the 2020.2 form. Variant keyword sets,
/// program code, parameter tables and bindings round trip.
fn upport(shader: &Shader, chunk_size: usize) -> Result<Shader> {
    let mut out = shader.clone();
    ensure!(
        out.m_ParsedForm.m_KeywordNames.is_none(),
        "{}: already carries 2022+ style keyword names",
        out.m_Name
    );
    out.stageCounts = Some(Vec::new());
    out.m_ParsedForm.m_CustomEditorForRenderPipelines = None;

    let blobs = shader.platform_blobs()?;
    ensure!(!blobs.is_empty(), "{}: no platform blobs", out.m_Name);
    let mut state = PassState::default();
    for (si, sub_shader) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
        for (pi, pass) in sub_shader.m_Passes.iter().enumerate() {
            let label = format!("{}: sub shader {si} pass {pi}", out.m_Name);
            let converted = upport_pass(pass, &blobs, &mut state).with_context(|| label.clone())?;
            let out_pass = &mut out.m_ParsedForm.m_SubShaders[si].m_Passes[pi];
            out_pass.m_NameIndices = IndexMap::new();
            out_pass.m_GlobalKeywordMask = None;
            out_pass.m_LocalKeywordMask = None;
            out_pass.m_EditorDataHash = None;
            out_pass.m_Platforms = None;
            let mut programs = [
                &mut out_pass.progVertex,
                &mut out_pass.progFragment,
                &mut out_pass.progGeometry,
                &mut out_pass.progHull,
                &mut out_pass.progDomain,
                &mut out_pass.progRayTracing,
            ];
            for (program, players) in programs.iter_mut().zip(&converted) {
                write_upgraded(program, players);
            }
        }
    }
    let names: Vec<String> = state
        .keywords
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let flags: Vec<u8> = state.keywords.iter().map(|(_, flag)| *flag).collect();
    let count = u16::try_from(names.len())?;
    for pass in out
        .m_ParsedForm
        .m_SubShaders
        .iter_mut()
        .flat_map(|s| &mut s.m_Passes)
    {
        for (_, program) in pass.programs_mut() {
            program.m_SerializedKeywordStateMask = Some((0..count).collect());
        }
    }
    out.m_ParsedForm.m_KeywordNames = Some(names);
    out.m_ParsedForm.m_KeywordFlags = Some(flags);

    let mut platforms = Vec::new();
    if state.entries.is_empty() {
        for blob in &blobs {
            platforms.push(PlatformBlob::from_entries(
                blob.platform,
                &[] as &[Vec<u8>],
                chunk_size,
            )?);
        }
    }
    for (platform, entries) in state.entries {
        platforms.push(PlatformBlob::from_entries(platform, &entries, chunk_size)?);
    }
    out.set_platform_blobs(&platforms)?;
    Ok(out)
}

/// The player sub programs and parameter blob indices per stage program.
#[derive(Default)]
struct UpPlayers {
    tiers: Vec<Vec<SerializedPlayerSubProgram>>,
    parameters: Vec<Vec<u32>>,
}

fn upport_pass(
    pass: &SerializedPass,
    blobs: &[PlatformBlob],
    state: &mut PassState,
) -> Result<[UpPlayers; 6]> {
    let mut programs: [Option<UpPlayers>; 6] = Default::default();
    for (position, (_stage, program)) in pass.programs().iter().enumerate() {
        if program.m_SubPrograms.is_empty() {
            continue;
        }
        ensure!(
            program.m_PlayerSubPrograms.is_none(),
            "sub programs of both forms at once"
        );
        // Unity lists every hardware tier slot, all but the one the 2020.2 build
        // carries filled; stage programs without variants carry no tiers at all
        let tier_slots = usize::try_from(
            program
                .m_SubPrograms
                .first()
                .context("checked by the caller")?
                .m_ShaderHardwareTier
                + 1,
        )?;
        let mut players = UpPlayers {
            tiers: vec![Vec::new(); tier_slots],
            parameters: vec![Vec::new(); tier_slots],
        };
        for sub in &program.m_SubPrograms {
            let program_type = u32::try_from(sub.m_GpuProgramType)?;
            let label = || {
                format!(
                    "blob {} of tier {}",
                    sub.m_BlobIndex, sub.m_ShaderHardwareTier
                )
            };
            let mut running = Vec::new();
            for blob in blobs {
                if blob.platform.runs(program_type)? {
                    running.push(blob);
                }
            }
            let blob = running.first().with_context(|| {
                format!("{}: no blob runs program type {program_type}", label())
            })?;
            ensure!(
                running.len() == 1,
                "{}: program type {program_type} runs on {} platforms",
                label(),
                running.len()
            );
            let location = *blob
                .entries()?
                .get(sub.m_BlobIndex as usize)
                .with_context(|| format!("{}: out of range", label()))?;
            let entry = SubProgram::parse(blob.entry(location)?).with_context(label)?;
            let inline = match &entry.layout {
                SubProgramLayout::V201806140 { parameters, .. } => parameters,
                SubProgramLayout::V202012090 { .. } => {
                    bail!("{}: already has the 2022+ layout", label())
                }
            };
            let locals = match &entry.layout {
                SubProgramLayout::V201806140 { local_keywords, .. } => local_keywords.clone(),
                _ => bail!("{}: already has the 2022+ layout", label()),
            };
            let keywords =
                keyword_indices(&entry.keywords, &locals, &mut state.keywords, &label())?;

            let wrapped = SubProgram {
                program_type,
                stats: entry.stats.clone(),
                keywords: entry.keywords.clone(),
                code: entry.code.clone(),
                layout: SubProgramLayout::V202012090 {
                    source_map: u32::try_from(inline.source_map)?,
                    channels: inline
                        .channels
                        .iter()
                        .map(|&(source, target)| {
                            Ok((u32::try_from(source)?, u32::try_from(target)?))
                        })
                        .collect::<Result<Vec<_>>>()?,
                },
            };

            // the GL entries of the 2020.2 build leave the unnamed globals block to
            // its implicit binding; the 2022+ parameter blobs list it
            let mut bindings = inline.bindings.clone();
            if blob.platform == Platform::OpenGLCore
                && !bindings.iter().any(|b| b.name() == "$Globals")
                && inline
                    .constant_buffers
                    .iter()
                    .any(|scope| scope.name == "$Globals")
            {
                bindings.insert(
                    0,
                    Binding::ConstantBuffer {
                        name: "$Globals".to_string(),
                        index: 0,
                        array_size: 0,
                    },
                );
            }
            let parameters = ProgramParameters {
                constant_buffers: inline.constant_buffers.clone(),
                bindings,
            };

            let key = (blob.platform.id(), sub.m_BlobIndex);
            let (program_index, parameter_index) = match state.upported_indices.get(&key) {
                Some(&pair) => pair,
                None => {
                    let program_bytes = wrapped.to_bytes()?;
                    let parameter_bytes = parameters.to_blob()?;
                    let pair = push_pair(
                        &mut state.entries,
                        blob.platform,
                        program_bytes,
                        parameter_bytes,
                    )?;
                    state.upported_indices.insert(key, pair);
                    pair
                }
            };
            let tier = usize::try_from(sub.m_ShaderHardwareTier)
                .with_context(|| format!("{}: tier {}", label(), sub.m_ShaderHardwareTier))?;
            players.tiers[tier].push(SerializedPlayerSubProgram {
                m_BlobIndex: program_index,
                m_KeywordIndices: keywords,
                m_ShaderRequirements: sub.m_ShaderRequirements,
                m_GpuProgramType: sub.m_GpuProgramType,
            });
            players.parameters[tier].push(parameter_index);
        }
        programs[position] = Some(players);
    }
    Ok(programs.map(|program| program.unwrap_or_default()))
}

/// The variant's keywords, registered as globals and locals in first encounter order.
fn keyword_indices(
    globals: &[String],
    locals: &[String],
    registry: &mut IndexMap<String, u8>,
    shader: &str,
) -> Result<Vec<u16>> {
    let mut indices = Vec::new();
    for (name, local) in globals
        .iter()
        .map(|name| (name, false))
        .chain(locals.iter().map(|name| (name, true)))
    {
        let flag = *registry.entry(name.clone()).or_insert(u8::from(local));
        ensure!(
            flag == u8::from(local),
            "{shader}: keyword {} is both global and local",
            name.escape_debug()
        );
        indices.push(u16::try_from(
            registry.get_index_of(name).context("just inserted")?,
        )?);
    }
    Ok(indices)
}

/// Append a program entry and its parameter entry, returning both indices.
fn push_pair(
    entries: &mut Vec<(Platform, Vec<Vec<u8>>)>,
    platform: Platform,
    program: Vec<u8>,
    parameters: Vec<u8>,
) -> Result<(u32, u32)> {
    let position = entries
        .iter()
        .position(|(p, _)| *p == platform)
        .unwrap_or_else(|| {
            entries.push((platform, Vec::new()));
            entries.len() - 1
        });
    let list = &mut entries[position].1;
    list.push(program);
    let program_index = u32::try_from(list.len() - 1)?;
    list.push(parameters);
    let parameter_index = u32::try_from(list.len() - 1)?;
    Ok((program_index, parameter_index))
}

fn write_upgraded(program: &mut SerializedProgram, players: &UpPlayers) {
    program.m_SubPrograms = Vec::new();
    program.m_PlayerSubPrograms = Some(players.tiers.clone());
    program.m_ParameterBlobIndices = Some(players.parameters.clone());
    // the variants carry their parameters whole; the split into shared common
    // parameters is an editor optimization the 2020.2 form doesn't decide
    program.m_CommonParameters = Some(SerializedProgramParameters {
        m_VectorParams: Vec::new(),
        m_MatrixParams: Vec::new(),
        m_TextureParams: Vec::new(),
        m_BufferParams: Vec::new(),
        m_ConstantBuffers: Vec::new(),
        m_ConstantBufferBindings: Vec::new(),
        m_UAVParams: Vec::new(),
        m_Samplers: Vec::new(),
    });
}

/// Convert one shader to the form `target`'s tree describes. Which grammar the shader
/// carries is in its own parsed form — the keyword name list of the rework of Unity
/// 2021.2 against the pass tables before it — so no version decides the direction.
pub fn convert(
    shader: &Shader,
    target: &rabex::typetree::TypeTreeNode,
    chunk_size: usize,
) -> Result<Shader> {
    // the keyword names live in the parsed form, not at the shader's top level
    let parsed_form = target
        .children
        .iter()
        .find(|child| child.m_Name == "m_ParsedForm")
        .context("not a shader tree")?;
    let new_form = crate::value::has_field(parsed_form, "m_KeywordNames");
    let converted = match (shader.m_ParsedForm.m_KeywordNames.is_some(), new_form) {
        (true, false) => downport(shader, chunk_size)?,
        (false, true) => upport(shader, chunk_size)?,
        // already the target's grammar
        _ => return Ok(shader.clone()),
    };
    // every Vulkan program announces what its modules read, and nothing else: a
    // disagreement is a GPU page fault in the player
    rabex_shader_convert::vulkan_bindings::verify(&converted)?;
    Ok(converted)
}
