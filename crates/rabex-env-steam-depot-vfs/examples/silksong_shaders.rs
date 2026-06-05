//! Validate shader decoding across all three Silksong platform depots.
//!
//! Per depot we open the addressables `shaders_assets_all.bundle`,
//! iterate every `Shader`, and per compiler platform:
//!   - slice + LZ4-decompress the `compressedBlob` (concatenating its
//!     segments) into the `ShaderSubProgramBlob`,
//!   - split that into its `count` index entries,
//!   - classify each entry via `m_ParsedForm`: sub-program code lives at
//!     the `m_BlobIndex` of a `m_PlayerSubPrograms` variant (filtered to
//!     this platform by the variant's `m_GpuProgramType`), parameter
//!     blobs at the paired `m_ParameterBlobIndices` slot,
//!   - parse each sub-program header to its length-prefixed program
//!     code (the real layout, the way AssetRipper / USCSandbox read it).
//!
//! The code is shader source for text backends (GLSL), bytecode for
//! binary ones (DXBC / SPIR-V), and a `0xF00DCAFE`-wrapped container for
//! Metal. Cross-checked against a framing-independent marker scan; every
//! entry classifies (`unref = 0`) and every sub-program parses.

#![allow(non_snake_case)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::rabex::objects::ClassIdType;
use rabex_env::resolver::EnvResolver;
use rabex_env::{Environment, rabex};
use rabex_env_steam_depot_vfs::SteamDepotGameFiles;
use serde::Deserialize;
use serde_value::Value;
use steam_depot_vfs::DepotStore;
use steam_depot_vfs::session::LazyCachedAuth;

const STORE: &str = "/Users/sipgatejj/.personal/steam/steam-depot-vfs/target/steam-vfs-store";

const DEPOTS: &[(&str, u32, u32, u64)] = &[
    ("windows", 1030300, 1030301, 4421626056705534276),
    ("macos", 1030300, 1030302, 4136280015582261500),
    ("linux", 1030300, 1030303, 7921642076658611197),
];

type Version = (u16, u16, u16);

#[tokio::main]
async fn main() -> Result<()> {
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());

    for &(os, app, depot, manifest) in DEPOTS {
        println!("\n========== {os} (depot {depot}) ==========");
        let game_files = open_env_files(app, depot, manifest).await?;

        // Synchronous env reads block on async internally — keep them
        // off the async worker.
        let (build, version, stats, shader_count) =
            tokio::task::block_in_place(|| process_depot(Environment::new(game_files, &tpk)))?;

        println!("buildTarget: {}  unity {version:?}", build.display());
        println!("Shader objects: {shader_count}");
        for (platform, s) in &stats {
            println!(
                "  platform {:>2} {:<11} subprograms={:<6} text={:<6} binary={:<6} metal_msl={:<6} params={:<6} unref={:<4} markers={}",
                platform,
                platform_name(*platform),
                s.subprograms,
                s.text_sources,
                s.binary,
                s.metal_msl,
                s.param_blobs,
                s.unreferenced,
                s.markers,
            );
            println!("      programTypes: {:?}", s.program_types);
        }
    }

    Ok(())
}

async fn open_env_files(app: u32, depot: u32, manifest: u64) -> Result<SteamDepotGameFiles> {
    let auth = LazyCachedAuth::prepare(
        LazyCachedAuth::default_refresh_token_cache(),
        std::env::var("STEAM_USERNAME").unwrap_or_default(),
        std::env::var("STEAM_PASSWORD").unwrap_or_default(),
    )
    .await?;
    let store = DepotStore::new(STORE.into());
    let manifest_store = store
        .open_depot_manifest(Arc::new(auth), app, depot, manifest, "public")
        .await?;
    Ok(SteamDepotGameFiles::new(Arc::new(manifest_store))?)
}

fn process_depot<R: EnvResolver, P: TypeTreeProvider>(
    env: Environment<R, P>,
) -> Result<(PathBuf, Version, BTreeMap<u32, PlatformStats>, usize)> {
    let version = env.unity_version()?.version_tuple();
    let build = env
        .addressables_build_folder()?
        .context("no addressables build folder")?;
    let file = env
        .load_addressables_bundle_content("shaders_assets_all.bundle")
        .context("load shaders_assets_all.bundle")?;

    let mut stats: BTreeMap<u32, PlatformStats> = BTreeMap::new();
    let mut shader_count = 0usize;

    for obj in file.objects_of::<Shader>() {
        shader_count += 1;
        let shader = obj
            .read()
            .with_context(|| format!("read shader {}", obj.path_id()))?;

        for (p, &platform) in shader.platforms.iter().enumerate() {
            let entry = stats.entry(platform).or_default();
            let Ok(blob) = decompress_platform(&shader, p) else {
                continue;
            };
            entry.markers += count_markers(&blob);
            let Ok(entries) = blob_entries(&blob, version) else {
                continue;
            };
            let (sub_indices, param_indices) = referenced_indices(&shader.m_ParsedForm, platform);

            for (i, bytes) in entries.iter().enumerate() {
                let i = i as u32;
                if param_indices.contains(&i) {
                    entry.param_blobs += 1;
                } else if !sub_indices.contains(&i) {
                    entry.unreferenced += 1;
                } else {
                    classify_subprogram(entry, bytes, version)?;
                }
            }
        }
    }

    Ok((build, version, stats, shader_count))
}

fn classify_subprogram(entry: &mut PlatformStats, bytes: &[u8], version: Version) -> Result<()> {
    entry.subprograms += 1;
    let sp = parse_subprogram(bytes, version)?;
    *entry.program_types.entry(sp.program_type).or_default() += 1;
    if is_text_source(sp.program_data) {
        entry.text_sources += 1;
    } else if unwrap_metal(sp.program_data).is_some_and(is_text_source) {
        entry.metal_msl += 1;
    } else {
        entry.binary += 1;
    }
    Ok(())
}

/// Unwrap a Metal `0xF00DCAFE` container to its embedded MSL source:
/// after the magic an `i32` offset points past the header to a
/// null-terminated entry-point name, and the rest is the UTF-8 source.
/// (Mirrors AssetStudio's `ShaderConverter` Metal branch.)
fn unwrap_metal(data: &[u8]) -> Option<&[u8]> {
    if !data.starts_with(&METAL_MAGIC) {
        return None;
    }
    let offset = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let body = data.get(offset..)?;
    let entry_end = body.iter().position(|&b| b == 0)?;
    body.get(entry_end + 1..)
}

fn decompress_platform(shader: &Shader, p: usize) -> Result<Vec<u8>> {
    let segments = shader.offsets[p]
        .iter()
        .zip(&shader.compressedLengths[p])
        .zip(&shader.decompressedLengths[p]);

    let mut out = Vec::new();
    for ((&off, &clen), &dlen) in segments {
        let (off, clen, dlen) = (off as usize, clen as usize, dlen as usize);
        let slice = shader
            .compressedBlob
            .get(off..off + clen)
            .context("blob slice out of range")?;
        let chunk = lz4_flex::block::decompress(slice, dlen)?;
        if chunk.len() != dlen {
            bail!("decompressed length mismatch");
        }
        out.extend(chunk);
    }
    Ok(out)
}

/// Split a decompressed platform blob into its raw entry byte slices,
/// indexed by blob index. The entries are heterogeneous (sub-programs
/// and parameter blobs); the caller classifies them via `m_ParsedForm`.
fn blob_entries(blob: &[u8], ver: Version) -> Result<Vec<&[u8]>> {
    let mut r = Reader::new(blob);
    let count = r.i32_as_usize()?;
    let mut spans = Vec::with_capacity(count);
    for _ in 0..count {
        let off = r.i32_as_usize()?;
        let len = r.i32_as_usize()?;
        if ver >= (2019, 3, 0) {
            let _segment = r.i32()?;
        }
        spans.push((off, len));
    }
    spans
        .into_iter()
        .map(|(off, len)| blob.get(off..off + len).context("entry past blob end"))
        .collect()
}

/// Parse a `ShaderSubProgram` far enough to extract its program code.
/// Field order per AssetRipper / USCSandbox; version gates the optional
/// fields. We stop at the program code (channels / params follow).
fn parse_subprogram<'a>(sub: &'a [u8], ver: Version) -> Result<SubProgram<'a>> {
    let mut r = Reader::new(sub);
    let _blob_version = r.i32()?;
    let program_type = r.i32()?;
    let (_alu, _tex, _flow) = (r.i32()?, r.i32()?, r.i32()?);
    if ver >= (5, 5, 0) {
        let _temp_register = r.i32()?;
    }

    let global_keywords = r.i32_as_usize()?;
    for _ in 0..global_keywords {
        r.skip_string()?;
    }
    if (2019, 1, 0) <= ver && ver < (2021, 2, 0) {
        let local_keywords = r.i32_as_usize()?;
        for _ in 0..local_keywords {
            r.skip_string()?;
        }
    }

    let program_data = r.byte_array()?;
    Ok(SubProgram {
        program_type,
        program_data,
    })
}

const STAGES: &[&str] = &[
    "progVertex",
    "progFragment",
    "progGeometry",
    "progHull",
    "progDomain",
    "progRayTracing",
];

/// Collect the blob indices referenced for `platform`, as sub-programs
/// (`m_BlobIndex`) and parameter blobs (`m_ParameterBlobIndices`).
///
/// `m_PlayerSubPrograms` is `[hardware-tier][variant]`; a multi-platform
/// shader interleaves both platforms' variants in one tier, told apart
/// by each variant's `m_GpuProgramType`. The parallel
/// `m_ParameterBlobIndices[tier]` pairs 1:1 with the variants, so a
/// variant's parameter blob sits at the same slot.
fn referenced_indices(parsed: &Value, platform: u32) -> (BTreeSet<u32>, BTreeSet<u32>) {
    let mut subs = BTreeSet::new();
    let mut params = BTreeSet::new();
    for subshader in seq(get(Some(parsed), "m_SubShaders")) {
        for pass in seq(get(Some(subshader), "m_Passes")) {
            for &stage in STAGES {
                let prog = get(Some(pass), stage);
                let param_tiers = get(prog, "m_ParameterBlobIndices");
                for (tier, variants) in seq(get(prog, "m_PlayerSubPrograms")).iter().enumerate() {
                    let param_tier = nth(param_tiers, tier);
                    for (slot, variant) in seq(Some(variants)).iter().enumerate() {
                        let gpu = as_i64(get(Some(variant), "m_GpuProgramType")).unwrap_or(-1);
                        if !gpu_matches_platform(gpu, platform) {
                            continue;
                        }
                        subs.extend(as_u32(get(Some(variant), "m_BlobIndex")));
                        params.extend(as_u32(nth(param_tier, slot)));
                    }
                }
            }
        }
    }
    (subs, params)
}

/// Does a `ShaderGpuProgramType` belong to `ShaderCompilerPlatform`?
fn gpu_matches_platform(gpu: i64, platform: u32) -> bool {
    match platform {
        4 => (9..=22).contains(&gpu),   // D3D11 — DX9/DX10/DX11 program types
        14 => (23..=24).contains(&gpu), // Metal — MetalVS / MetalFS
        15 => (1..=8).contains(&gpu),   // OpenGLCore — GLES* / GLCore*
        18 => gpu == 25,                // Vulkan — SPIRV
        _ => false,
    }
}

fn get<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match v? {
        Value::Map(m) => m.get(&Value::String(key.to_string())),
        _ => None,
    }
}

fn seq(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Seq(items)) => items,
        _ => &[],
    }
}

fn nth(v: Option<&Value>, i: usize) -> Option<&Value> {
    seq(v).get(i)
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::U32(n) => Some(*n),
        Value::U64(n) => u32::try_from(*n).ok(),
        Value::I32(n) => u32::try_from(*n).ok(),
        Value::I64(n) => u32::try_from(*n).ok(),
        _ => None,
    }
}

fn as_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::I8(n) => Some(i64::from(*n)),
        Value::I16(n) => Some(i64::from(*n)),
        Value::I32(n) => Some(i64::from(*n)),
        Value::I64(n) => Some(*n),
        Value::U8(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        _ => None,
    }
}

/// A program-code blob is text source when it decodes as UTF-8 and opens
/// with `#` (GLSL `#ifdef`/`#version`).
fn is_text_source(data: &[u8]) -> bool {
    std::str::from_utf8(data).is_ok_and(|s| s.trim_start().starts_with('#'))
}

/// Framing-independent ground truth: count source-start markers anywhere.
fn count_markers(blob: &[u8]) -> usize {
    const MARKERS: &[&[u8]] = &[b"#version ", b"#include <metal", b"#ifdef VERTEX"];
    (0..blob.len())
        .filter(|&i| MARKERS.iter().any(|m| blob[i..].starts_with(m)))
        .count()
}

fn platform_name(p: u32) -> &'static str {
    match p {
        4 => "D3D11",
        14 => "Metal",
        15 => "OpenGLCore",
        18 => "Vulkan",
        _ => "?",
    }
}

/// Metal program code is wrapped in a `0xF00DCAFE`-tagged container with
/// the MSL embedded inside, rather than stored as raw text.
const METAL_MAGIC: [u8; 4] = [0xfe, 0xca, 0x0d, 0xf0];

/// Minimal slice of the typetree `Shader`. The blob fields are typed;
/// `m_ParsedForm` is read dynamically because partially typing its deep
/// tree desyncs the typetree deserializer.
#[derive(Deserialize)]
struct Shader {
    m_ParsedForm: Value,
    platforms: Vec<u32>,
    offsets: Vec<Vec<u32>>,
    compressedLengths: Vec<Vec<u32>>,
    decompressedLengths: Vec<Vec<u32>>,
    compressedBlob: Vec<u8>,
}
impl ClassIdType for Shader {
    const CLASS_ID: ClassId = ClassId::Shader;
}

struct SubProgram<'a> {
    program_type: i32,
    program_data: &'a [u8],
}

#[derive(Default)]
struct PlatformStats {
    subprograms: usize,
    text_sources: usize,
    binary: usize,
    metal_msl: usize,
    param_blobs: usize,
    unreferenced: usize,
    markers: usize,
    program_types: BTreeMap<i32, usize>,
}

/// Little-endian reader over one entry's bytes. Unity 4-byte aligns
/// after each variable-length field, relative to the slice start.
struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }
    fn i32(&mut self) -> Result<i32> {
        let s = self.b.get(self.p..self.p + 4).context("read past end")?;
        self.p += 4;
        Ok(i32::from_le_bytes(s.try_into().unwrap()))
    }
    fn i32_as_usize(&mut self) -> Result<usize> {
        Ok(usize::try_from(self.i32()?)?)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n).context("read past end")?;
        self.p += n;
        Ok(s)
    }
    fn align(&mut self) {
        self.p = self.p.next_multiple_of(4);
    }
    fn skip_string(&mut self) -> Result<()> {
        let n = self.i32_as_usize()?;
        self.bytes(n)?;
        self.align();
        Ok(())
    }
    fn byte_array(&mut self) -> Result<&'a [u8]> {
        let n = self.i32_as_usize()?;
        let v = self.bytes(n)?;
        self.align();
        Ok(v)
    }
}
