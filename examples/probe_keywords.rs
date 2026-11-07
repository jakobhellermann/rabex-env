//! Dump how each Unity version represents a variant's keywords: the 2020.2 masks and
//! indices against the 2022+ keyword name lists.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
mod utils;

use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{Shader, SubProgramView, SubProgramViewLayout};

fn main() -> Result<()> {
    let want = std::env::args().nth(2);
    let arg = std::env::args()
        .nth(1)
        .context("fixture version directory or game directory")?;
    let path = Path::new(&arg);
    if path.join("shaders").exists() || path.join("StandaloneWindows64").exists() {
        let shaders = if path.join("shaders").exists() {
            path.join("shaders")
        } else {
            path.join("StandaloneWindows64/shaders")
        };
        let bundle = BundleFileReader::from_reader(
            Cursor::new(std::fs::read(&shaders).with_context(|| shaders.display().to_string())?),
            &ExtractionConfig::default(),
        )?;
        let entry = bundle.main_serializedfile().context("no serialized file")?;
        let data = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?;
            if want.is_none() || want.as_deref() == Some(shader.m_ParsedForm.m_Name.as_str()) {
                inspect(&shader)?;
            }
        }
        return Ok(());
    }

    // a game directory: walk its serialized files
    let env = utils::find_game(&arg)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            let shader: Shader = file.object_at(info.m_PathID)?.read()?;
            if want.is_none() || want.as_deref() == Some(shader.m_ParsedForm.m_Name.as_str()) {
                inspect(&shader)?;
            }
        }
    }
    Ok(())
}

fn inspect(shader: &Shader) -> Result<()> {
    let name = &shader.m_ParsedForm.m_Name;
    let requirements: std::collections::BTreeSet<i64> = shader
        .m_ParsedForm
        .m_SubShaders
        .iter()
        .flat_map(|s| &s.m_Passes)
        .flat_map(|pass| pass.programs())
        .flat_map(|(_, program)| &program.m_SubPrograms)
        .map(|sub| sub.m_ShaderRequirements)
        .collect();
    if std::env::var("RABEX_REQ").is_ok() {
        println!(
            "REQ {name}: {requirements:?} platforms {:?} isBaked {}",
            shader.platforms, shader.m_ShaderIsBaked
        );
        return Ok(());
    }
    println!("== {name}");
    if let Some(names) = &shader.m_ParsedForm.m_KeywordNames {
        println!(
            "  m_KeywordNames {names:?} flags {:?}",
            shader.m_ParsedForm.m_KeywordFlags.as_deref().unwrap_or(&[])
        );
    } else {
        println!("  (no m_KeywordNames)");
    }
    for blob in shader.platform_blobs()? {
        println!("  platform {:?}", blob.platform);
        for (s, sub_shader) in shader.m_ParsedForm.m_SubShaders.iter().enumerate() {
            for (p, pass) in sub_shader.m_Passes.iter().enumerate() {
                println!(
                    "  pass {s}/{p} type {} programMask {}: local mask {:?} global mask {:?}",
                    pass.m_Type,
                    pass.m_ProgramMask,
                    pass.m_LocalKeywordMask,
                    pass.m_GlobalKeywordMask
                );
                println!("    nameIndices {:?}", pass.m_NameIndices);
                for (stage, program) in pass.programs() {
                    println!(
                        "    {stage:?} tiers {:?}",
                        program
                            .m_PlayerSubPrograms
                            .as_ref()
                            .map(|t| t.iter().map(|v| v.len()).collect::<Vec<_>>())
                    );
                }
                println!(
                    "    editorDataHash {:?} platforms {:?} hasInstancing {} useName {:?}",
                    pass.m_EditorDataHash,
                    pass.m_Platforms,
                    pass.m_HasInstancingVariant,
                    pass.m_UseName
                );
                for (stage, program) in [
                    ("vertex", &pass.progVertex),
                    ("fragment", &pass.progFragment),
                ] {
                    for sub in &program.m_SubPrograms {
                        println!(
                            "    {stage} sub tier {} gpu {} blob {} req {}: gk {:?} lk {:?} kws {:?} vec {} tex {} cbs {}",
                            sub.m_ShaderHardwareTier,
                            sub.m_GpuProgramType,
                            sub.m_BlobIndex,
                            sub.m_ShaderRequirements,
                            sub.m_GlobalKeywordIndices,
                            sub.m_LocalKeywordIndices,
                            sub.m_KeywordIndices,
                            sub.m_VectorParams.as_ref().map_or(0, |v| v.len()),
                            sub.m_TextureParams.as_ref().map_or(0, |v| v.len()),
                            sub.m_ConstantBuffers.as_ref().map_or(0, |v| v.len()),
                        );
                    }
                    for (t, tier) in program
                        .m_PlayerSubPrograms
                        .iter()
                        .flatten()
                        .flatten()
                        .enumerate()
                    {
                        println!(
                            "    {stage} player[{t}] blob {}: keywords {:?} req {}",
                            tier.m_BlobIndex, tier.m_KeywordIndices, tier.m_ShaderRequirements
                        );
                    }
                    for (t, (subs, params)) in program
                        .m_PlayerSubPrograms
                        .iter()
                        .flatten()
                        .zip(program.m_ParameterBlobIndices.iter().flatten())
                        .enumerate()
                    {
                        println!(
                            "    {stage} tier {t}: {} subs, {} param blobs",
                            subs.len(),
                            params.len()
                        );
                        for (v, (sub, &pi)) in subs.iter().zip(params.iter()).enumerate() {
                            println!("      {stage} v{v} blob {} params {}", sub.m_BlobIndex, pi);
                        }
                    }
                    println!(
                        "    {stage} paramBlobIndices {:?} keywordStateMask {:?} commonParams {}",
                        program.m_ParameterBlobIndices.is_some(),
                        program.m_SerializedKeywordStateMask,
                        program.m_CommonParameters.is_some()
                    );
                }
            }
        }
        // the keyword lists each program entry carries before its code
        for (index, loc) in blob.entries()?.iter().cloned().enumerate() {
            if let Ok(view) = SubProgramView::parse(blob.entry(loc)?) {
                let local = match &view.layout {
                    SubProgramViewLayout::V201806140 { local_keywords, .. } => {
                        format!("{local_keywords:?}")
                    }
                    _ => "-".to_string(),
                };
                println!(
                    "  entry {index} type {} keywords {:?} local {local}",
                    view.program_type, view.keywords
                );
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn unused() {}
