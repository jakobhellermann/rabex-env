//! List a bundle's shaders with the keywords and variants they carry, so a converted
//! bundle can be held against its source: a variant that did not survive is a variant
//! the player silently replaces with the one without the keyword.

use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::Shader;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle")?;
    let version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let want = std::env::args().nth(3);

    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?;
            let name = &shader.m_ParsedForm.m_Name;
            if want.as_ref().is_some_and(|want| !name.contains(want)) {
                continue;
            }
            let mut keywords = BTreeSet::new();
            // the newer form lists a shader's keywords once, next to the parsed form
            if let Some(names) = &shader.m_ParsedForm.m_KeywordNames {
                for name in names {
                    keywords.insert(name.clone());
                }
            }
            let mut variants = 0;
            let mut combos: BTreeSet<String> = BTreeSet::new();
            let mut passes = 0;
            for subshader in &shader.m_ParsedForm.m_SubShaders {
                for pass in &subshader.m_Passes {
                    passes += 1;
                    // the older form keeps every name the pass uses in one table the
                    // variants index into; the keywords are the shouting ones
                    for name in pass.m_NameIndices.keys() {
                        if name
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
                        {
                            keywords.insert(name.clone());
                        }
                    }
                    for (stage, program) in pass.programs() {
                        // the older form lists the variants flat, the newer one per
                        // hardware tier
                        for sub in &program.m_SubPrograms {
                            variants += 1;
                            let names: Vec<_> = sub
                                .m_GlobalKeywordIndices
                                .iter()
                                .flatten()
                                .filter_map(|i| {
                                    pass.m_NameIndices
                                        .iter()
                                        .find(|(_, slot)| **slot == *i as i32)
                                        .map(|(name, _)| name.clone())
                                })
                                .collect();
                            combos.insert(format!("{stage:?} {names:?}"));
                        }
                        for tier in program.m_PlayerSubPrograms.iter().flatten() {
                            for sub in tier {
                                variants += 1;
                                let mut names = sub.m_KeywordIndices.clone();
                                names.sort();
                                let named: Vec<_> = names
                                    .iter()
                                    .filter_map(|i| {
                                        shader
                                            .m_ParsedForm
                                            .m_KeywordNames
                                            .as_ref()
                                            .and_then(|all| all.get(*i as usize))
                                            .cloned()
                                    })
                                    .collect();
                                combos.insert(format!("{stage:?} {named:?}"));
                            }
                        }
                    }
                }
            }
            println!(
                "{name}: {passes} passes, {variants} subprograms, {} variant keyword sets",
                combos.len()
            );
            if std::env::var("PARAMS").is_ok() {
                for subshader in &shader.m_ParsedForm.m_SubShaders {
                    for pass in &subshader.m_Passes {
                        for (stage, program) in pass.programs() {
                            for sub in &program.m_SubPrograms {
                                let names: Vec<_> = sub
                                    .m_GlobalKeywordIndices
                                    .iter()
                                    .flatten()
                                    .filter_map(|i| {
                                        pass.m_NameIndices
                                            .iter()
                                            .find(|(_, slot)| **slot == *i as i32)
                                            .map(|(name, _)| name.clone())
                                    })
                                    .collect();
                                if !names.iter().any(|n| n == "IS_CHARACTER") {
                                    continue;
                                }
                                let vectors: Vec<_> = sub
                                    .m_VectorParams
                                    .iter()
                                    .flatten()
                                    .map(|p| p.m_NameIndex)
                                    .collect();
                                let name_of = |index: i32| {
                                    pass.m_NameIndices
                                        .iter()
                                        .find(|(_, slot)| **slot == index)
                                        .map(|(name, _)| name.clone())
                                        .unwrap_or_else(|| format!("#{index}"))
                                };
                                println!(
                                    "   {stage:?} {names:?} gpu {} vectors {:?}",
                                    sub.m_GpuProgramType,
                                    vectors.iter().map(|i| name_of(*i)).collect::<Vec<_>>()
                                );
                            }
                        }
                    }
                }
            }
            if std::env::var("COMBOS").is_ok() {
                for combo in &combos {
                    println!("   {combo}");
                }
            }
        }
    }
    Ok(())
}
