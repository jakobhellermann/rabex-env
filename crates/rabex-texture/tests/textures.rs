//! Decode textures Unity imported in every format and compare against the pixels Unity reports.

mod common;

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rabex::objects::ClassId;
use rabex_texture::Texture2D;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Manifest {
    unity_version: String,
    bundle: String,
    textures: Vec<TextureEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TextureEntry {
    name: String,
    requested_format: String,
    format: String,
    format_id: i32,
    width: u32,
    height: u32,
    mip_count: i32,
    readable: bool,
    expected: String,
    raw_data_length: usize,
}

#[test]
fn decoded_textures_match_unity() -> Result<()> {
    let mut failures = Vec::new();
    for group in common::groups::<Manifest>("textures")? {
        let version = &group.manifest.unity_version;
        let textures: BTreeMap<String, Texture2D> = group
            .objects::<Texture2D>(ClassId::Texture2D)?
            .into_iter()
            .map(|(_, t)| (t.m_Name.clone(), t))
            .collect();
        let mut inline_pixels = BTreeMap::new();
        for entry in &group.manifest.textures {
            let texture = textures
                .get(&entry.name)
                .with_context(|| format!("{} missing from bundle", entry.name))?;
            ensure!(
                (
                    texture.m_Width as u32,
                    texture.m_Height as u32,
                    texture.m_TextureFormat
                ) == (entry.width, entry.height, entry.format_id),
                "{version} {}: serialized fields disagree with Unity",
                entry.name
            );
            let pixels = group.pixels(texture)?;
            if let Some(base) = entry.name.strip_suffix("_inline") {
                inline_pixels.insert(base.to_owned(), pixels.clone());
            } else if let Some(base) = entry.name.strip_suffix("_stream") {
                ensure!(
                    inline_pixels.get(base) == Some(&pixels),
                    "{version} {}: streamed pixels differ from the inline variant",
                    entry.name
                );
            }

            match texture.decode(&pixels) {
                Err(e) => failures.push(format!("{version} {}: {e}", entry.name)),
                Ok(img) => {
                    let (differing, max) = common::diff(&img, &group.image(&entry.expected)?);
                    if differing > 0 {
                        failures.push(format!(
                            "{version} {}: {differing} px differ, max per channel {max:?}",
                            entry.name
                        ));
                    }
                }
            }
        }
    }
    assert_eq!(failures, Vec::<String>::new());
    Ok(())
}
