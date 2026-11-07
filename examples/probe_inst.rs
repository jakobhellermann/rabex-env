//! Dump the inline parameters of a game's or fixture's Vulkan shader entries.

mod utils;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{ProgramParameters, Shader, SubProgramView, SubProgramViewLayout};

fn inspect(shader: &Shader, want: &Option<String>) -> Result<()> {
    let name = &shader.m_ParsedForm.m_Name;
    if let Some(w) = want
        && name != w
    {
        return Ok(());
    }
    for blob in shader.platform_blobs()? {
        if blob.platform != rabex_shader::Platform::Vulkan {
            continue;
        }
        println!("{name} Vulkan:");
        for (i, loc) in blob.entries()?.iter().cloned().take(2).enumerate() {
            let bytes = blob.entry(loc)?;
            let scopes: Vec<String> = match SubProgramView::parse(bytes) {
                Ok(view) => match &view.layout {
                    SubProgramViewLayout::V201806140 { parameters, .. } => parameters
                        .constant_buffers
                        .iter()
                        .map(|cb| {
                            format!(
                                "    scope {:?} size={} consts={:?}",
                                cb.name,
                                cb.size,
                                cb.constants
                                    .iter()
                                    .map(|c| format!(
                                        "{},{},{}",
                                        c.name,
                                        c.rows,
                                        c.columns.unwrap_or(0)
                                    ))
                                    .collect::<Vec<_>>()
                            )
                        })
                        .collect(),
                    _ => vec!["    (202012090 program)".to_owned()],
                },
                Err(_) => match ProgramParameters::parse(bytes) {
                    Ok(params) => params
                        .constant_buffers
                        .iter()
                        .map(|cb| {
                            format!(
                                "    scope {:?} size={} consts={:?}",
                                cb.name,
                                cb.size,
                                cb.constants
                                    .iter()
                                    .map(|c| format!(
                                        "{},{},{}",
                                        c.name,
                                        c.rows,
                                        c.columns.unwrap_or(0)
                                    ))
                                    .collect::<Vec<_>>()
                            )
                        })
                        .collect(),
                    Err(_) => vec![format!("    entry {i}: neither program nor parameter blob")],
                },
            };
            println!("  entry {i} ({} B):", bytes.len());
            for line in scopes {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let path = std::path::PathBuf::from(std::env::args().nth(1).context("dir or bundle")?);
    let want = std::env::args().nth(2);

    let bundle_path = if path.is_dir() {
        path.join("shaders")
    } else {
        path.clone()
    };
    if bundle_path.exists() {
        let bundle = BundleFileReader::from_reader(
            std::io::Cursor::new(std::fs::read(&bundle_path)?),
            &ExtractionConfig::default(),
        )?;
        let entry = bundle.main_serializedfile().context("no serialized file")?;
        let data = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut std::io::Cursor::new(data.as_slice()))?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(
                info,
                NullTypeTreeProvider,
                &mut std::io::Cursor::new(data.as_slice()),
            )?;
            inspect(&shader, &want)?;
        }
        return Ok(());
    }

    // a game's raw data files: walk them through the environment
    let env = utils::find_game(&path.to_string_lossy())?.context("game not found")?;
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
            inspect(&shader, &want)?;
        }
    }
    Ok(())
}
