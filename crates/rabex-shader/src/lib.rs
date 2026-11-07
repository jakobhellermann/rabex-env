//! Unity `Shader` objects: their raw serialized form, the compiled program blobs of
//! their platforms, and what those programs mean.
//!
//! Three layers, each usable without the next:
//!
//! - **Raw data**: [`types`] holds the generated model of the serialized `Shader` —
//!   the bytes as Unity wrote them, fields that only some Unity versions carry are
//!   `Option`. `Shader` roundtrips byte-exactly; nothing here interprets content.
//! - **Program blobs**: [`Shader::platform_blobs`] decompresses `Shader.compressedBlob` into
//!   one [`PlatformBlob`] per platform, whose segments hold the program entries:
//!   [`SubProgram`] (one compiled program, whose layout follows its serialization
//!   version, [`SubProgramLayout`]) and [`ProgramParameters`] (a variant's parameter
//!   tables, Unity 2022+). Reading and writing these is format logic; entries with
//!   unknown layout parts stay verbatim.
//! - **Interpretation**: [`variants`] derives what the blobs contain — the compiled
//!   [`ProgramVariant`]s of every pass ([`Shader::pass_variants`]), which entry holds
//!   what ([`Shader::entry_kinds`], [`EntryKind`]), how many stages a platform has
//!   ([`Shader::stage_count`]). Unity 2020 shaders carry keyword masks instead of the
//!   keyword names this needs and fail loudly.
//!
//! [`unity`] holds Unity's own enum definitions as constants and types,
//! [`dxbc`]/[`vulkan`]/[`d3d`] the compiled code formats of the entries.

pub mod compute;
pub mod compute_types;
pub mod d3d;
pub mod dxbc;
pub mod params;
pub mod types;
pub mod unity;
pub mod variants;
pub mod vulkan;

use anyhow::{Context, Result, bail, ensure};

pub use params::{
    Binding, Constant, ConstantBufferLayout, InlineParameters, ProgramParameters, StructConstant,
};
pub use types::Shader;
pub use variants::{EntryKind, ProgramVariant, Stage};

/// `Shader.platforms` values (`ShaderCompilerPlatform`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Gl,
    D3D9,
    Xbox360,
    Ps3,
    D3D11,
    Gles20,
    NaCl,
    Flash,
    D3D11_9x,
    Gles3Plus,
    Psp2,
    Ps4,
    XboxOne,
    Psm,
    Metal,
    OpenGLCore,
    N3ds,
    WiiU,
    Vulkan,
    Switch,
    XboxOneD3D12,
    GameCoreXboxOne,
    GameCoreXboxSeries,
    Ps5,
    Ps5Nggc,
    GameCore,
    WebGpu,
    Switch2,
    Unknown(u32),
}

impl Platform {
    const KNOWN: [Platform; 28] = {
        use Platform::*;
        [
            Gl,
            D3D9,
            Xbox360,
            Ps3,
            D3D11,
            Gles20,
            NaCl,
            Flash,
            D3D11_9x,
            Gles3Plus,
            Psp2,
            Ps4,
            XboxOne,
            Psm,
            Metal,
            OpenGLCore,
            N3ds,
            WiiU,
            Vulkan,
            Switch,
            XboxOneD3D12,
            GameCoreXboxOne,
            GameCoreXboxSeries,
            Ps5,
            Ps5Nggc,
            GameCore,
            WebGpu,
            Switch2,
        ]
    };

    pub fn from_id(id: u32) -> Self {
        Platform::KNOWN
            .into_iter()
            .find(|p| p.id() == id)
            .unwrap_or(Platform::Unknown(id))
    }
}

/// One platform's decompressed program data, one buffer per segment.
///
/// `Shader.compressedBlob` decompresses into the platforms' data, concatenated in chunks
/// of the build's `m_DefaultShaderChunkSizeInMB`; the first segment of every platform
/// holds the entry table (offsets, lengths, kinds), the rest the entry bytes. Entries are
/// [`SubProgram`]s and, Unity 2022+, [`ProgramParameters`].
#[derive(Debug)]
pub struct PlatformBlob {
    pub platform: Platform,
    pub segments: Vec<Vec<u8>>,
}

impl Shader {
    /// Decompress the program data of every platform in `platforms`.
    pub fn platform_blobs(&self) -> Result<Vec<PlatformBlob>> {
        let n = self.platforms.len();
        ensure!(
            self.offsets.len() == n
                && self.compressedLengths.len() == n
                && self.decompressedLengths.len() == n,
            "{}: per-platform arrays disagree with {n} platforms",
            self.m_Name
        );
        (0..n)
            .map(|i| {
                let segments = (self.offsets[i].iter())
                    .zip(&self.compressedLengths[i])
                    .zip(&self.decompressedLengths[i])
                    .map(|((&offset, &compressed), &decompressed)| {
                        let range = offset as usize..offset as usize + compressed as usize;
                        let input = self
                            .compressedBlob
                            .get(range)
                            .context("compressed segment out of range")?;
                        Ok(lz4_flex::block::decompress(input, decompressed as usize)?)
                    })
                    .collect::<Result<_>>()?;
                Ok(PlatformBlob {
                    platform: Platform::from_id(self.platforms[i]),
                    segments,
                })
            })
            .collect()
    }

    /// Replace the program data of every platform, compressing each segment.
    pub fn set_platform_blobs(&mut self, blobs: &[PlatformBlob]) -> Result<()> {
        let mut compressed_blob = Vec::new();
        let (mut offsets, mut compressed_lengths, mut decompressed_lengths) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut stage_counts = Vec::new();
        for blob in blobs {
            let (mut o, mut c, mut d) = (Vec::new(), Vec::new(), Vec::new());
            for segment in &blob.segments {
                let compressed = lz4_flex::block::compress(segment);
                o.push(u32::try_from(compressed_blob.len())?);
                c.push(u32::try_from(compressed.len())?);
                d.push(u32::try_from(segment.len())?);
                compressed_blob.extend_from_slice(&compressed);
            }
            offsets.push(o);
            compressed_lengths.push(c);
            decompressed_lengths.push(d);
            stage_counts.push(self.stage_count(blob.platform)?);
        }
        self.platforms = blobs.iter().map(|b| b.platform.id()).collect();
        self.offsets = offsets;
        self.compressedLengths = compressed_lengths;
        self.decompressedLengths = decompressed_lengths;
        self.compressedBlob = compressed_blob;
        // Unity 2020 shaders have no stageCounts field to fill
        if self.stageCounts.is_some() {
            self.stageCounts = Some(stage_counts);
        }
        Ok(())
    }

    /// The number of stages with programs for `platform`, as stored in `stageCounts`.
    pub fn stage_count(&self, platform: Platform) -> Result<u32> {
        let mut stages = Vec::new();
        for pass in self
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .flat_map(|s| &s.m_Passes)
        {
            for (stage, program) in pass.programs() {
                let Some(player_tiers) = &program.m_PlayerSubPrograms else {
                    continue;
                };
                for sub in player_tiers.iter().flatten() {
                    if platform.runs(u32::try_from(sub.m_GpuProgramType)?)?
                        && !stages.contains(&stage)
                    {
                        stages.push(stage);
                    }
                }
            }
        }
        Ok(u32::try_from(stages.len())?)
    }
}

/// Location of one entry in a platform's segments.
#[derive(Clone, Copy, Debug)]
pub struct EntryLocation {
    pub offset: u32,
    pub length: u32,
    pub segment: u32,
}

impl PlatformBlob {
    /// The entry table at the start of segment 0, indexed by `m_BlobIndex` and `m_ParameterBlobIndices`.
    pub fn entries(&self) -> Result<Vec<EntryLocation>> {
        let mut r = Reader::new(self.segments.first().context("platform without segments")?);
        let count = r.u32()?;
        (0..count)
            .map(|_| {
                Ok(EntryLocation {
                    offset: r.u32()?,
                    length: r.u32()?,
                    segment: r.u32()?,
                })
            })
            .collect()
    }

    pub fn entry(&self, location: EntryLocation) -> Result<&[u8]> {
        let segment = self
            .segments
            .get(location.segment as usize)
            .with_context(|| format!("entry in missing segment {}", location.segment))?;
        let range = location.offset as usize..(location.offset + location.length) as usize;
        segment.get(range).with_context(|| {
            format!(
                "entry {location:?} exceeds segment of {} bytes",
                segment.len()
            )
        })
    }

    /// Every entry's data, in table order.
    pub fn entry_data(&self) -> Result<Vec<&[u8]>> {
        self.entries()?
            .into_iter()
            .map(|location| self.entry(location))
            .collect()
    }

    /// Lay out `entries` like Unity: the table and entries in one segment if they fit `chunk_size`,
    /// otherwise the table alone, followed by segments of entries no larger than `chunk_size`.
    pub fn from_entries(
        platform: Platform,
        entries: &[impl AsRef<[u8]>],
        chunk_size: usize,
    ) -> Result<Self> {
        let table_size = 4 + 12 * entries.len();
        let total = table_size + entries.iter().map(|e| e.as_ref().len()).sum::<usize>();
        let mut data_segments: Vec<Vec<u8>> = vec![Vec::new()];
        let mut locations = Vec::with_capacity(entries.len());
        let single = total <= chunk_size;
        for entry in entries {
            let entry = entry.as_ref();
            let current = data_segments.last_mut().unwrap();
            if !single && !current.is_empty() && current.len() + entry.len() > chunk_size {
                data_segments.push(Vec::new());
            }
            let index = data_segments.len() - 1;
            let current = data_segments.last_mut().unwrap();
            let (offset, segment) = if single {
                (table_size + current.len(), 0)
            } else {
                (current.len(), index + 1)
            };
            locations.push(EntryLocation {
                offset: u32::try_from(offset)?,
                length: u32::try_from(entry.len())?,
                segment: u32::try_from(segment)?,
            });
            current.extend_from_slice(entry);
        }

        let mut table = Writer::default();
        table.u32(u32::try_from(entries.len())?);
        for location in &locations {
            table.u32(location.offset);
            table.u32(location.length);
            table.u32(location.segment);
        }
        let mut table = table.finish();
        let segments = if single {
            table.extend(data_segments.pop().unwrap());
            vec![table]
        } else {
            std::iter::once(table).chain(data_segments).collect()
        };
        Ok(PlatformBlob { platform, segments })
    }
}

/// The serialization version Unity 2021.2 and later write program entries with: keyword
/// list, code, then source map and vertex channel bindings. Parameter entries carry the
/// same version number.
pub const PROGRAM_VERSION_202012090: u32 = 202012090;
/// The serialization version Unity 2019.1–2020.1 write program entries with: a second,
/// shader-local keyword list before the code and the parameter tables inline after it.
pub const PROGRAM_VERSION_201806140: u32 = 201806140;

/// One compiled program entry of a platform blob, as a view over its bytes: the program
/// code borrows instead of copying. [`SubProgram`] is the owning form.
#[derive(Debug)]
pub struct SubProgramView<'a> {
    pub program_type: u32,
    /// Instruction statistics of the variant.
    pub stats: Vec<u32>,
    pub keywords: Vec<String>,
    /// The compiled code, borrowed from the entry.
    pub code: &'a [u8],
    pub layout: SubProgramViewLayout,
}

/// The parts of a program entry that differ between serialization layouts.
#[derive(Debug)]
pub enum SubProgramViewLayout {
    /// Unity 2020: a second keyword list, then parameter tables inline after the program.
    V201806140 {
        local_keywords: Vec<String>,
        parameters: InlineParameters,
    },
    /// Unity 2022+: the source map and vertex channel bindings follow the code.
    V202012090 {
        source_map: u32,
        /// `(source, target)` vertex channel bindings.
        channels: Vec<(u32, u32)>,
    },
}

impl SubProgramViewLayout {
    /// The source map and vertex channel bindings of a `V202012090` entry; other layouts
    /// carry none.
    pub fn source_map_and_channels(&self) -> Option<(u32, &[(u32, u32)])> {
        match self {
            Self::V202012090 {
                source_map,
                channels,
                ..
            } => Some((*source_map, channels)),
            Self::V201806140 { .. } => None,
        }
    }
}

impl SubProgramLayout {
    /// The source map and vertex channel bindings of a `V202012090` entry; other layouts
    /// carry none.
    pub fn source_map_and_channels(&self) -> Option<(u32, &[(u32, u32)])> {
        match self {
            Self::V202012090 {
                source_map,
                channels,
                ..
            } => Some((*source_map, channels)),
            Self::V201806140 { .. } => None,
        }
    }
}

impl SubProgramView<'_> {
    /// The serialization version the entry was read from.
    pub fn version(&self) -> u32 {
        match self.layout {
            SubProgramViewLayout::V201806140 { .. } => PROGRAM_VERSION_201806140,
            SubProgramViewLayout::V202012090 { .. } => PROGRAM_VERSION_202012090,
        }
    }

    /// The DXBC container of a D3D11 program, after Unity's 38 byte prefix. The prefix
    /// stays part of the carried program code; only the container access skips it.
    pub fn dxbc(&self) -> Result<&[u8]> {
        dxbc_after_prefix(self.code)
    }

    /// The owning form: the borrowed parts copy.
    pub fn to_sub_program(&self) -> SubProgram {
        SubProgram {
            program_type: self.program_type,
            stats: self.stats.clone(),
            keywords: self.keywords.clone(),
            code: self.code.to_vec(),
            layout: match &self.layout {
                SubProgramViewLayout::V201806140 {
                    local_keywords,
                    parameters,
                } => SubProgramLayout::V201806140 {
                    local_keywords: local_keywords.clone(),
                    parameters: parameters.clone(),
                },
                SubProgramViewLayout::V202012090 {
                    source_map,
                    channels,
                } => SubProgramLayout::V202012090 {
                    source_map: *source_map,
                    channels: channels.clone(),
                },
            },
        }
    }

    pub fn parse(data: &[u8]) -> Result<SubProgramView<'_>> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        let stat_count = SubProgram::stat_count(version)?;
        let program_type = r.u32()?;
        let stats = (0..stat_count).map(|_| r.u32()).collect::<Result<_>>()?;
        let keywords = (0..r.u32()?).map(|_| r.string()).collect::<Result<_>>()?;
        // the second keyword list sits before the program code
        let local_keywords = if version == PROGRAM_VERSION_201806140 {
            Some((0..r.u32()?).map(|_| r.string()).collect::<Result<_>>()?)
        } else {
            None
        };
        let code = r.bytes()?;
        r.align4();
        let layout = match version {
            PROGRAM_VERSION_201806140 => SubProgramViewLayout::V201806140 {
                local_keywords: local_keywords.expect("checked above"),
                parameters: InlineParameters::parse(r.rest()?)?,
            },
            PROGRAM_VERSION_202012090 => {
                let source_map = r.u32()?;
                let channels = (0..r.u32()?)
                    .map(|_| Ok((r.u32()?, r.u32()?)))
                    .collect::<Result<_>>()?;
                r.finish()?;
                SubProgramViewLayout::V202012090 {
                    source_map,
                    channels,
                }
            }
            _ => bail!("unsupported program version {version}"),
        };
        Ok(SubProgramView {
            program_type,
            stats,
            keywords,
            code,
            layout,
        })
    }
}

/// One compiled program entry of a platform blob, with the parts that differ between
/// serialization layouts split into [`SubProgramLayout`].
#[derive(Debug)]
pub struct SubProgram {
    pub program_type: u32,
    /// Instruction statistics of the variant.
    pub stats: Vec<u32>,
    pub keywords: Vec<String>,
    pub code: Vec<u8>,
    pub layout: SubProgramLayout,
}

/// The parts of a program entry that differ between serialization layouts.
#[derive(Debug)]
pub enum SubProgramLayout {
    /// Unity 2020: a second keyword list, then parameter tables inline after the program.
    V201806140 {
        local_keywords: Vec<String>,
        parameters: InlineParameters,
    },
    /// Unity 2022+: the source map and vertex channel bindings follow the code.
    V202012090 {
        source_map: u32,
        /// `(source, target)` vertex channel bindings.
        channels: Vec<(u32, u32)>,
    },
}

impl SubProgram {
    /// The serialization version the entry was read from; `to_bytes` writes it back.
    pub fn version(&self) -> u32 {
        match self.layout {
            SubProgramLayout::V201806140 { .. } => PROGRAM_VERSION_201806140,
            SubProgramLayout::V202012090 { .. } => PROGRAM_VERSION_202012090,
        }
    }
}

impl SubProgram {
    fn stat_count(version: u32) -> Result<usize> {
        Ok(match version {
            PROGRAM_VERSION_201806140 | PROGRAM_VERSION_202012090 => 4,
            _ => bail!("unsupported program version {version}"),
        })
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(SubProgramView::parse(data)?.to_sub_program())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut w = Writer::default();
        w.u32(self.version());
        w.u32(self.program_type);
        for stat in &self.stats {
            w.u32(*stat);
        }
        w.u32(u32::try_from(self.keywords.len())?);
        for keyword in &self.keywords {
            w.string(keyword)?;
        }
        // the second keyword list sits before the program code
        if let SubProgramLayout::V201806140 { local_keywords, .. } = &self.layout {
            w.u32(u32::try_from(local_keywords.len())?);
            for keyword in local_keywords {
                w.string(keyword)?;
            }
        }
        w.bytes(&self.code)?;
        w.align4();
        match &self.layout {
            SubProgramLayout::V201806140 { parameters, .. } => {
                w.data.extend_from_slice(&parameters.to_inline()?);
            }
            SubProgramLayout::V202012090 {
                source_map,
                channels,
            } => {
                w.u32(*source_map);
                w.u32(u32::try_from(channels.len())?);
                for &(source, target) in channels {
                    w.u32(source);
                    w.u32(target);
                }
            }
        }
        Ok(w.finish())
    }

    /// The DXBC container of a D3D11 program, after Unity's 38 byte prefix. The prefix
    /// stays part of the carried program code; only the container access skips it.
    pub fn dxbc(&self) -> Result<&[u8]> {
        dxbc_after_prefix(&self.code)
    }
}

/// The DXBC container of a D3D11 program, after Unity's 38 byte prefix.
fn dxbc_after_prefix(code: &[u8]) -> Result<&[u8]> {
    const PREFIX: usize = 38;
    let dxbc = code
        .get(PREFIX..)
        .context("program code shorter than its prefix")?;
    ensure!(
        dxbc.starts_with(b"DXBC"),
        "no DXBC container after the prefix"
    );
    Ok(dxbc)
}

impl Platform {
    pub fn id(self) -> u32 {
        use crate::unity::shader_compiler_platform as unity;
        use Platform::*;
        match self {
            // Legacy platforms, which Unity's public enum no longer lists; 0 is `None` there.
            // TODO: verify the legacy ids with data of old Unity versions
            Gl => 0,
            D3D9 => 1,
            Xbox360 => 2,
            Ps3 => 3,
            NaCl => 6,
            Flash => 7,
            D3D11_9x => 8,
            Psp2 => 10,
            Psm => 13,
            N3ds => 16,
            WiiU => 17,
            D3D11 => unity::D3D,
            Gles20 => unity::GLES20,
            Gles3Plus => unity::GLES3x,
            Ps4 => unity::PS4,
            XboxOne => unity::XboxOneD3D11,
            Metal => unity::Metal,
            OpenGLCore => unity::OpenGLCore,
            Vulkan => unity::Vulkan,
            Switch => unity::Switch,
            XboxOneD3D12 => unity::XboxOneD3D12,
            GameCoreXboxOne => unity::GameCoreXboxOne,
            GameCoreXboxSeries => unity::GameCoreXboxSeries,
            Ps5 => unity::PS5,
            Ps5Nggc => unity::PS5NGGC,
            GameCore => unity::GameCore,
            WebGpu => unity::WebGPU,
            Switch2 => unity::Switch2,
            Unknown(id) => id,
        }
    }

    /// Whether programs of `GpuProgramType` `program_type` belong to this platform.
    pub fn runs(self, program_type: u32) -> Result<bool> {
        Ok(match self {
            Platform::D3D11 => (15..=22).contains(&program_type),
            Platform::OpenGLCore => (6..=8).contains(&program_type),
            Platform::Vulkan => program_type == 25,
            // every entry of A Short Hike's macOS builds (2019.4.29f1, 2021.3.45f2) names 23
            // or 24
            Platform::Metal => (23..=24).contains(&program_type),
            other => bail!("program types of {other:?} are unknown"),
        })
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self.data.get(self.pos..self.pos + n).with_context(|| {
            format!(
                "read of {n} bytes at {} past end {}",
                self.pos,
                self.data.len()
            )
        })?;
        self.pos += n;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn bool32(&mut self) -> Result<bool> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            other => bail!("expected a boolean, got {other}"),
        }
    }

    fn align4(&mut self) {
        self.pos += (4 - self.pos % 4) % 4;
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn string(&mut self) -> Result<String> {
        let s = String::from_utf8(self.bytes()?.to_vec())?;
        self.align4();
        Ok(s)
    }

    fn rest(&mut self) -> Result<&'a [u8]> {
        self.take(self.data.len() - self.pos)
    }

    fn finish(&self) -> Result<()> {
        ensure!(
            self.pos == self.data.len(),
            "{} trailing bytes",
            self.data.len() - self.pos
        );
        Ok(())
    }
}

#[derive(Default)]
struct Writer {
    data: Vec<u8>,
}

impl Writer {
    fn u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    fn bool32(&mut self, value: bool) {
        self.u32(value.into());
    }

    fn align4(&mut self) {
        let padding = (4 - self.data.len() % 4) % 4;
        self.data.resize(self.data.len() + padding, 0);
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.u32(u32::try_from(bytes.len())?);
        self.data.extend_from_slice(bytes);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<()> {
        self.bytes(value.as_bytes())?;
        self.align4();
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.data
    }
}
