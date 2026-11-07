//! The games on this machine, swept at the program level.
//!
//! The fixtures cover what the editors build on demand; the games cover the versions and
//! platforms that actually ship. Shipped data is copyrighted, so the paths are a local
//! list — a machine without them runs the test as a no-op rather than failing.
//!
//! What this checks is deliberately the layer the container round trip does not reach:
//! it opens each platform program and reads the stages inside, which is the only way a
//! version whose program container we cannot read shows up as a failure.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::{Platform, Shader};

/// `(data directory, the Unity version that built it)`.
const GAMES: &[(&str, &str)] = &[
    (
        "/home/jakob/.steamapps/Hollow Knight-1.5.78.11833/hollow_knight_Data",
        "2020.2.2f1",
    ),
    (
        "/home/jakob/.steamapps/Hollow Knight/hollow_knight_Data",
        "6000.0.61f1",
    ),
    (
        "/home/jakob/.steamapps/Hollow Knight Silksong/Hollow Knight Silksong_Data",
        "6000.0.50f1",
    ),
    (
        "/home/jakob/.steamapps/Nine Sols/NineSols_Data",
        "2022.3.62f3",
    ),
];

#[test]
fn the_vulkan_programs_of_every_game_parse() -> Result<()> {
    // deliberately one cache for every game: it has to answer per version, and a cache
    // that does not would hand the second game the first one's trees
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut swept = 0;
    for (data_dir, version) in GAMES {
        let dir = Path::new(data_dir);
        if !dir.exists() {
            continue;
        }
        let (mut programs, mut failures, mut unreadable) = (0, Vec::new(), Vec::new());
        for file in serialized_files(dir)? {
            let bytes = std::fs::read(&file)?;
            let Ok(serialized) = SerializedFile::from_reader(&mut std::io::Cursor::new(&bytes))
            else {
                continue;
            };
            for info in serialized
                .objects()
                .filter(|info| info.m_ClassID == ClassId::Shader)
            {
                // the model not matching a version is its own failure, told apart from
                // a program container we cannot open
                let shader: Shader = match serialized.read(
                    info,
                    &tpk,
                    &mut std::io::Cursor::new(bytes.as_slice()),
                ) {
                    Ok(shader) => shader,
                    Err(error) => {
                        unreadable.push(format!(
                            "{} object {} (class {:?}, type index {}): {error}",
                            file.file_name().unwrap_or_default().to_string_lossy(),
                            info.m_PathID,
                            info.m_ClassID,
                            info.m_TypeID
                        ));
                        continue;
                    }
                };
                for blob in shader.platform_blobs()? {
                    if blob.platform != Platform::Vulkan {
                        continue;
                    }
                    for location in blob.entries()? {
                        let entry = blob.entry(location)?;
                        let Ok(view) = rabex_shader::SubProgramView::parse(entry) else {
                            continue;
                        };
                        programs += 1;
                        if let Err(error) = rabex_shader::vulkan::VulkanProgram::parse(view.code) {
                            failures.push(format!("{}: {error}", shader.m_ParsedForm.m_Name));
                        }
                    }
                }
            }
        }
        if !failures.is_empty() || !unreadable.is_empty() {
            bail!(
                "{data_dir} ({version}): {} of {programs} Vulkan programs do not parse \
                 (e.g. {:?}), and {} shaders do not read as the model at all (e.g. {:?})",
                failures.len(),
                &failures[..failures.len().min(2)],
                unreadable.len(),
                &unreadable[..unreadable.len().min(2)]
            );
        }
        swept += programs;
    }
    println!("{swept} Vulkan programs parsed");
    Ok(())
}

/// A game's serialized files: the levels and the asset files beside them.
fn serialized_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("level") && !name.contains('.') || name.ends_with(".assets") {
            files.push(path);
        }
    }
    Ok(files)
}
