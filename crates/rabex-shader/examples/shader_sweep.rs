//! Read every shader of serialized files and round trip what rabex-shader models of it:
//! the model itself, each platform's blob (entries out, laid out again), each program entry
//! (parsed, written again), each parameter blob, and Vulkan's program container. Reports
//! per file what holds and the first failures of each kind.
//!
//! usage: shader_sweep <serialized file>... — the version comes from the files, the trees
//! from the files or, for builds without, the type tree package.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::params::ProgramParameters;
use rabex_shader::{Platform, PlatformBlob, Shader, SubProgram};

#[derive(Default)]
struct Tally {
    shaders: usize,
    platforms: BTreeMap<String, usize>,
    programs: usize,
    parameters: usize,
    vulkan: usize,
    /// Program entries without code, per platform.
    empty: BTreeMap<String, usize>,
    /// Per platform, the program types its entries name and how many: the evidence for
    /// which types a platform runs.
    types: BTreeMap<(String, u32), (usize, usize)>,
    /// Failures by kind, the first few of each.
    failures: BTreeMap<&'static str, (usize, Vec<String>)>,
}

impl Tally {
    fn fail(&mut self, kind: &'static str, what: String) {
        let (count, first) = self.failures.entry(kind).or_default();
        *count += 1;
        if first.len() < 3 {
            first.push(what);
        }
    }
}

/// The chunk size Unity laid `blob` out with: the smallest whole number of MiB that lays its
/// entries out the same, or the whole blob where it is one segment.
fn relaid(blob: &PlatformBlob) -> Result<bool> {
    let entries = blob.entry_data()?;
    let candidates: Vec<usize> = match blob.segments.len() {
        1 => vec![blob.segments[0].len()],
        _ => (1..=64).map(|mb| mb << 20).collect(),
    };
    for chunk in candidates {
        let again = PlatformBlob::from_entries(blob.platform, &entries, chunk)?;
        if again.segments == blob.segments {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sweep(shader: &Shader, tally: &mut Tally) -> Result<()> {
    let name = &shader.m_ParsedForm.m_Name;
    // which entries are programs and which parameter blobs, from the parsed form: each
    // sub program names its program type, which says the platform; a player sub program's
    // parameter blob index sits at its place in m_ParameterBlobIndices
    let mut subs: Vec<(u32, u32, Option<u32>)> = Vec::new();
    for pass in shader
        .m_ParsedForm
        .m_SubShaders
        .iter()
        .flat_map(|s| &s.m_Passes)
    {
        for program in [
            &pass.progVertex,
            &pass.progFragment,
            &pass.progGeometry,
            &pass.progHull,
            &pass.progDomain,
            &pass.progRayTracing,
        ] {
            for sub in &program.m_SubPrograms {
                subs.push((sub.m_GpuProgramType as u32, sub.m_BlobIndex, None));
            }
            let parameters = program.m_ParameterBlobIndices.as_deref().unwrap_or(&[]);
            for (tier, list) in program.m_PlayerSubPrograms.iter().flatten().enumerate() {
                for (i, sub) in list.iter().enumerate() {
                    let parameter = parameters.get(tier).and_then(|p| p.get(i)).copied();
                    subs.push((sub.m_GpuProgramType as u32, sub.m_BlobIndex, parameter));
                }
            }
        }
    }

    for blob in shader.platform_blobs()? {
        *tally
            .platforms
            .entry(format!("{:?}", blob.platform))
            .or_default() += 1;
        let at = |index: u32| format!("{name} {:?} entry {index}", blob.platform);
        if !relaid(&blob)? {
            tally.fail("blob layout", format!("{name} {:?}", blob.platform));
        }
        let entries = blob.entry_data()?;
        // every entry that reads as a program, by the type it names; with the shortest code
        for entry in &entries {
            if let Ok(program) = SubProgram::parse(entry) {
                let key = (format!("{:?}", blob.platform), program.program_type);
                let (count, shortest) = tally.types.entry(key).or_insert((0, usize::MAX));
                *count += 1;
                *shortest = (*shortest).min(program.code.len());
            }
        }
        let mut programs = Vec::new();
        let mut parameters = Vec::new();
        for &(program_type, index, parameter) in &subs {
            match blob.platform.runs(program_type) {
                Ok(true) => {
                    programs.push(index);
                    parameters.extend(parameter);
                }
                Ok(false) => {}
                Err(error) => {
                    tally.fail("program type", format!("{name}: {error:#}"));
                    break;
                }
            }
        }
        programs.sort_unstable();
        programs.dedup();
        parameters.sort_unstable();
        parameters.dedup();
        for &index in &programs {
            let Some(&entry) = entries.get(index as usize) else {
                tally.fail("program index", at(index));
                continue;
            };
            tally.programs += 1;
            let program = match SubProgram::parse(entry) {
                Ok(program) => program,
                Err(error) => {
                    tally.fail("program parse", format!("{}: {error:#}", at(index)));
                    continue;
                }
            };
            match program.to_bytes() {
                Ok(bytes) if bytes == entry => {}
                Ok(_) => tally.fail("program bytes", at(index)),
                Err(error) => tally.fail("program write", format!("{}: {error:#}", at(index))),
            }
            // an entry without code: a variant the build left empty
            if program.code.is_empty() {
                *tally
                    .empty
                    .entry(format!("{:?}", blob.platform))
                    .or_default() += 1;
                continue;
            }
            if blob.platform == Platform::Vulkan {
                tally.vulkan += 1;
                match rabex_shader::vulkan::VulkanProgram::parse(&program.code)
                    .and_then(|vulkan| vulkan.to_bytes())
                {
                    Ok(bytes) if bytes == program.code => {}
                    Ok(_) => tally.fail("vulkan bytes", at(index)),
                    Err(error) => tally.fail("vulkan", format!("{}: {error:#}", at(index))),
                }
            }
        }
        for &index in &parameters {
            let Some(&entry) = entries.get(index as usize) else {
                tally.fail("parameter index", at(index));
                continue;
            };
            tally.parameters += 1;
            match ProgramParameters::parse(entry).and_then(|p| p.to_blob()) {
                Ok(bytes) if bytes == entry => {}
                Ok(_) => tally.fail("parameter bytes", at(index)),
                Err(error) => tally.fail("parameters", format!("{}: {error:#}", at(index))),
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).with_context(|| path.clone())?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        let version = file
            .m_UnityVersion
            .as_ref()
            .map_or("?".to_owned(), |v| v.to_string());
        let mut tally = Tally::default();
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            tally.shaders += 1;
            let shader: Shader = match file.read(info, &tpk, &mut Cursor::new(data.as_slice())) {
                Ok(shader) => shader,
                Err(error) => {
                    tally.fail("model", format!("object {}: {error:#}", info.m_PathID));
                    continue;
                }
            };
            if let Err(error) = sweep(&shader, &mut tally) {
                tally.fail(
                    "blobs",
                    format!("{}: {error:#}", shader.m_ParsedForm.m_Name),
                );
            }
        }
        println!(
            "{path} ({version}): {} shaders, platforms {:?}, {} programs ({} Vulkan), {} parameter blobs",
            tally.shaders, tally.platforms, tally.programs, tally.vulkan, tally.parameters
        );
        if !tally.empty.is_empty() {
            println!("  programs without code: {:?}", tally.empty);
        }
        for ((platform, program_type), (count, shortest)) in &tally.types {
            println!(
                "  {platform} entries of program type {program_type}: {count}, code from {shortest} bytes"
            );
        }
        for (kind, (count, first)) in &tally.failures {
            println!("  {kind}: {count} failed, e.g.");
            for what in first {
                println!("    {}", what.chars().take(240).collect::<String>());
            }
        }
    }
    Ok(())
}
