//! Translate D3D11 pass variants to Vulkan and compare the result with Unity's own Vulkan programs
//! of the same pass variants: bindings, discard flag, stage count and vertex channels.
//!
//! Usage: `compare_vulkan` for the rabex-shader fixtures, or
//! `compare_vulkan <windows game dir> <linux game dir>` for two builds of a game.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::NullTypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::Environment;
use rabex_shader::vulkan::{VulkanProgram, flags};
use rabex_shader::{Binding, Platform, ProgramVariant, Shader, Stage};
use rabex_shader_convert::spirv;
use rabex_shader_convert::target::vulkan;

fn bundle_shaders(path: &Path) -> Result<Vec<Shader>> {
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(path)?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle
        .main_serializedfile()
        .context("bundle without serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    file.objects()
        .filter(|o| o.m_ClassID == ClassId::Shader)
        .map(|info| {
            Ok(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?)
        })
        .collect()
}

fn game_shaders(dir: &Path) -> Result<Vec<Shader>> {
    let env = Environment::new_in(dir, TypeTreeCache::new(TpkTypeTreeBlob::embedded()))?;
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
    let mut shaders = Vec::new();
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            shaders.push(file.object_at(info.m_PathID)?.read()?);
        }
    }
    Ok(shaders)
}

struct Spirv {
    /// Variable or block name to `(set, binding)`.
    bindings: BTreeMap<String, (u32, u32)>,
    kills: bool,
}

fn inspect(words: &[u32]) -> Spirv {
    let bindings = spirv::resources(words)
        .unwrap()
        .into_iter()
        .map(|r| (r.name, (r.set, r.binding)))
        .collect();
    Spirv {
        bindings,
        kills: spirv::has_kill(words).unwrap(),
    }
}

fn stage_bit(stage: Stage) -> u32 {
    match stage {
        Stage::Vertex => 0x04,
        Stage::Fragment => 0x08,
        Stage::Hull => 0x10,
        Stage::Domain => 0x20,
        Stage::Geometry => 0x40,
        Stage::RayTracing | Stage::Compute => 0,
    }
}

/// Unity names a stage's globals like `VGlobals2840191130`; HLSLcc without a suffix.
fn normalize(name: &str) -> String {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.len() < name.len() && trimmed.len() == 8 && trimmed.ends_with("Globals") {
        trimmed.to_owned()
    } else {
        name.to_owned()
    }
}

#[derive(Default)]
struct Report {
    outcomes: BTreeMap<String, (usize, String)>,
}

impl Report {
    fn add(&mut self, outcome: impl Into<String>, example: impl FnOnce() -> String) {
        let entry = self
            .outcomes
            .entry(outcome.into())
            .or_insert_with(|| (0, example()));
        entry.0 += 1;
    }
}

fn unpack(index: i32) -> (u32, u32, u32) {
    let index = index as u32;
    (index >> 24, (index >> 16) & 0xff, index & 0xffff)
}

fn compare(
    label: &str,
    stages: &[&ProgramVariant],
    unity: &ProgramVariant,
    report: &mut Report,
) -> Result<()> {
    let translated = match vulkan::from_d3d11(stages) {
        Ok(translated) => translated,
        Err(e) => {
            report.add(
                format!(
                    "translation failed: {}",
                    format!("{e:#}").lines().next().unwrap_or("")
                ),
                || label.to_owned(),
            );
            return Ok(());
        }
    };
    let program = VulkanProgram::parse(unity.program.code)?;

    let mut ours: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
    let mut kills = false;
    for stage in &translated {
        let spirv = inspect(&stage.spirv);
        kills |= stage.stage == Stage::Fragment && spirv.kills;
        for (name, (set, binding)) in spirv.bindings {
            ours.entry(name).or_insert((0, set, binding)).0 |= stage_bit(stage.stage);
        }
    }

    if std::env::var("COMPARE_DUMP").is_ok_and(|filter| label.contains(&filter)) {
        println!("{label}\n    ours: {ours:?}");
        for stage in &translated {
            println!("    {:?} GLSL:\n{}", stage.stage, stage.glsl);
        }
        println!("    unity: {:?}", unity.parameters.bindings);
        if let Ok(dir) = std::env::var("COMPARE_DUMP_DIR") {
            let dir = std::path::Path::new(&dir);
            std::fs::create_dir_all(dir).unwrap();
            let file = label.replace(['/', ' ', '[', ']', '"', ','], "_");
            for stage in &translated {
                let bytes: Vec<u8> = stage.spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
                std::fs::write(
                    dir.join(format!("{file}.ours.{:?}.spv", stage.stage)),
                    bytes,
                )
                .unwrap();
            }
            for (slot, stage) in program.stages.iter().enumerate() {
                if let Some(stage) = stage {
                    let words = smolv::decode(&stage.smolv).unwrap();
                    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                    std::fs::write(dir.join(format!("{file}.unity.slot{slot}.spv")), bytes)
                        .unwrap();
                }
            }
        }
    }
    let unity_stages = program.stages.iter().filter(|s| s.is_some()).count();
    let outcome = if unity_stages == translated.len() {
        "stage count matches"
    } else {
        "stage count differs"
    };
    report.add(outcome, || {
        format!("{label}: unity {unity_stages}, ours {}", translated.len())
    });
    let unity_discard = program.flags & flags::DISCARD != 0;
    let outcome = if unity_discard == kills {
        "discard flag matches OpKill"
    } else {
        "discard flag differs"
    };
    report.add(outcome, || {
        format!("{label}: unity {unity_discard}, ours {kills}")
    });

    for binding in &unity.parameters.bindings {
        let (kind, key, index) = match binding {
            Binding::ConstantBuffer { name, index, .. } => ("cbuffer", normalize(name), *index),
            Binding::Texture { name, index, .. } => ("texture", name.clone(), *index),
            Binding::Buffer { name, index, .. } => ("buffer", name.clone(), *index),
            Binding::Uav {
                original_index,
                index,
                ..
            } => {
                let key = ours
                    .keys()
                    .find(|k| k.starts_with(&format!("u{original_index}_")))
                    .cloned();
                ("uav", key.unwrap_or_default(), *index)
            }
            Binding::Sampler { index, state, .. } => {
                let register = stages
                    .iter()
                    .flat_map(|s| &s.parameters.bindings)
                    .find_map(|b| match b {
                        Binding::Sampler {
                            index,
                            state: d3d_state,
                            ..
                        } if d3d_state == state => Some(*index),
                        _ => None,
                    });
                let key = register.map(|r| format!("samplers{r}")).unwrap_or_default();
                ("inline sampler", key, *index)
            }
        };
        let theirs = unpack(index);
        let outcome = match ours.get(&key) {
            Some(&found) if found == theirs => format!("{kind}: same mask, set and binding"),
            Some(&(mask, set, _)) if (mask, set) == (theirs.0, theirs.1) => {
                format!("{kind}: same mask and set, binding differs")
            }
            Some(&(mask, _, _)) if mask != theirs.0 => format!("{kind}: mask differs"),
            Some(_) => format!("{kind}: set differs"),
            None => format!("{kind}: missing in ours"),
        };
        report.add(outcome, || {
            format!(
                "{label} {key:?}: unity {theirs:?}, ours {:?}",
                ours.get(&key)
            )
        });
        if let Binding::Texture {
            name,
            sampler_index,
            ..
        } = binding
            && *sampler_index >= 0
        {
            let ours = ours.get(&format!("sampler{name}"));
            let theirs = unpack(*sampler_index);
            let outcome = match ours {
                Some(&found) if found == theirs => "texture sampler: same mask, set and binding",
                Some(&(mask, set, _)) if (mask, set) == (theirs.0, theirs.1) => {
                    "texture sampler: same mask and set, binding differs"
                }
                Some(_) => "texture sampler: differs",
                None => "texture sampler: missing in ours",
            };
            report.add(outcome, || {
                format!("{label} {name}: unity {theirs:?}, ours {ours:?}")
            });
        }
    }

    if let Some(vertex) = stages.iter().find(|s| s.stage == Stage::Vertex) {
        let mut actual = match &unity.program.layout {
            rabex_shader::SubProgramViewLayout::V202012090 { channels, .. } => channels.clone(),
            _ => Vec::new(),
        };
        actual.sort();
        let translated_vertex = translated
            .iter()
            .find(|t| t.stage == Stage::Vertex)
            .context("no translated vertex stage")?;
        match vulkan::channels(vertex, translated_vertex) {
            Ok(mut derived) => {
                derived.sort();
                let outcome = if derived == actual {
                    "channels: 13 + HLSLcc input location"
                } else {
                    "channels differ"
                };
                report.add(outcome, || {
                    format!("{label}: unity {actual:?}, derived {derived:?}")
                });
            }
            Err(e) => report.add(format!("channels failed: {e}"), || label.to_owned()),
        }
    }
    Ok(())
}

fn keyed<'a, 'b>(
    variants: &'a [ProgramVariant<'b>],
) -> BTreeMap<Vec<String>, Vec<&'a ProgramVariant<'b>>> {
    let mut by_keywords: BTreeMap<Vec<String>, Vec<&ProgramVariant>> = BTreeMap::new();
    for v in variants {
        let mut k = v.keywords.clone();
        k.sort();
        by_keywords.entry(k).or_default().push(v);
    }
    by_keywords
}

fn compare_shaders(windows: &[Shader], linux: &[Shader], report: &mut Report) -> Result<()> {
    for shader in windows {
        let name = &shader.m_ParsedForm.m_Name;
        let Some(unity) = linux.iter().find(|s| &s.m_ParsedForm.m_Name == name) else {
            report.add("shader missing on Linux", || name.clone());
            continue;
        };
        let Some(d3d) = shader
            .platform_blobs()?
            .into_iter()
            .find(|b| b.platform == Platform::D3D11)
        else {
            continue;
        };
        let Some(vk) = unity
            .platform_blobs()?
            .into_iter()
            .find(|b| b.platform == Platform::Vulkan)
        else {
            continue;
        };
        let subs = shader
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .zip(&unity.m_ParsedForm.m_SubShaders);
        for (s, (sub, unity_sub)) in subs.enumerate() {
            for (p, (pass, unity_pass)) in sub.m_Passes.iter().zip(&unity_sub.m_Passes).enumerate()
            {
                let variants = shader.pass_variants(pass, &d3d)?;
                let unity_variants = unity.pass_variants(unity_pass, &vk)?;
                for (keywords, unity) in keyed(&unity_variants) {
                    let label = format!("{name} {s}.{p} {keywords:?}");
                    let stages = match vulkan::select_stages(&keywords, &variants) {
                        Ok(stages) => stages,
                        Err(e) => {
                            report.add(format!("stage selection failed: {e}"), || label.clone());
                            continue;
                        }
                    };
                    compare(&label, &stages, unity[0], report)?;
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let mut report = Report::default();
    match args.as_slice() {
        [] => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rabex-shader/tests/fixtures");
            for version in std::fs::read_dir(&root)? {
                let dir = version?.path();
                compare_shaders(
                    &bundle_shaders(&dir.join("StandaloneWindows64/shaders"))?,
                    &bundle_shaders(&dir.join("shaders"))?,
                    &mut report,
                )?;
            }
        }
        [windows, linux] => {
            compare_shaders(&game_shaders(windows)?, &game_shaders(linux)?, &mut report)?
        }
        _ => anyhow::bail!("usage: compare_vulkan [<windows game dir> <linux game dir>]"),
    }
    for (outcome, (n, example)) in &report.outcomes {
        println!("{n:7} {outcome}\n          e.g. {example}");
    }
    Ok(())
}
