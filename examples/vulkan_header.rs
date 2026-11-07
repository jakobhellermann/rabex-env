//! Check what a game's Vulkan program headers look like: the slot table is self
//! describing, so the header size follows from the data instead of from a version.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::{Platform, Shader};

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .context("game data directory or bundle")?;
    let want = std::env::args().nth(2).unwrap_or_default();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    let mut mismatches = 0;

    let paths: Vec<std::path::PathBuf> = if Path::new(&dir).is_dir() {
        std::fs::read_dir(Path::new(&dir))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(".assets") || (name.starts_with("level") && !name.contains('.'))
            })
            .collect()
    } else {
        vec![Path::new(&dir).to_path_buf()]
    };
    for path in paths {
        let raw = std::fs::read(&path)?;
        let bytes = if raw.starts_with(b"UnityFS") {
            let config = rabex::files::bundlefile::ExtractionConfig {
                fallback_unity_version: rabex::files::bundlefile::FallbackUnityVersion::Current,
                ..Default::default()
            };
            let reader = rabex::files::bundlefile::BundleFileReader::from_reader(
                std::io::Cursor::new(raw),
                &config,
            )?;
            let entry = reader.serialized_files().next().context("no file")?;
            reader.read_at_entry(entry)?
        } else {
            raw
        };
        let Ok(file) = SerializedFile::from_reader(&mut std::io::Cursor::new(&bytes)) else {
            continue;
        };
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(info, &tpk, &mut std::io::Cursor::new(&bytes))?;
            if !want.is_empty() && shader.m_ParsedForm.m_Name != want {
                continue;
            }
            for blob in shader.platform_blobs()? {
                if blob.platform != Platform::Vulkan {
                    continue;
                }
                for location in blob.entries()? {
                    let Ok(view) = rabex_shader::SubProgramView::parse(blob.entry(location)?)
                    else {
                        continue;
                    };
                    let code = view.code;
                    let word = |i: usize| {
                        u32::from_le_bytes([
                            code[i * 4],
                            code[i * 4 + 1],
                            code[i * 4 + 2],
                            code[i * 4 + 3],
                        ]) as usize
                    };
                    if code.len() < 44 {
                        *shapes
                            .entry("too short for a slot table".into())
                            .or_default() += 1;
                        continue;
                    }
                    let slots: Vec<(usize, usize)> =
                        (0..5).map(|s| (word(1 + s * 2), word(2 + s * 2))).collect();
                    // the stages lie back to back after the header, so the first one
                    // starts where the header ends
                    let header = slots
                        .iter()
                        .filter(|(_, size)| *size != 0)
                        .map(|(offset, _)| *offset)
                        .min()
                        .unwrap_or(0);
                    let end = slots.iter().map(|(o, s)| o + s).max().unwrap_or(0);
                    let filled: Vec<usize> = slots
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, size))| *size != 0)
                        .map(|(slot, _)| slot)
                        .collect();
                    if end != code.len() {
                        mismatches += 1;
                    }
                    // the words between the slot table and the first stage, for short
                    // headers: the hashed form's are hashes, not worth listing
                    let rest: Vec<usize> = match header {
                        44..=76 => (11..header / 4).map(word).collect(),
                        _ => Vec::new(),
                    };
                    *shapes
                        .entry(format!(
                            "flags {:#x}, header {header}, slots {filled:?}, covers all bytes {}, rest {rest:?}",
                            word(0),
                            end == code.len()
                        ))
                        .or_default() += 1;
                }
            }
        }
    }
    for (shape, count) in &shapes {
        println!("{count:5} × {shape}");
    }
    println!("{mismatches} programs whose slots do not cover their bytes");
    Ok(())
}
