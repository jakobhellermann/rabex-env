//! The engine class rules against the fixture pair: retargeted 6000 builds must match
//! Unity's own 2020.2 builds of the same content, and the typed rules round trip on the
//! model level.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::files::serializedfile::build_common_offset_map;
use rabex::objects::ClassId;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::retarget::Retarget;
use rabex_shader::types::{Material, Texture2D, UnityPropertySheet};

const NEW: &str = "../rabex-shader/tests/fixtures/6000.3.27f2";
const OLD: &str = "../rabex-shader/tests/fixtures/2020.2.2f1";

/// A class tree of `fields`, the top level the rules read.
fn tree_of(fields: &[&str]) -> rabex::typetree::TypeTreeNode {
    rabex::typetree::TypeTreeNode {
        m_Type: "class".to_owned(),
        m_Name: "Base".to_owned(),
        children: fields
            .iter()
            .map(|field| rabex::typetree::TypeTreeNode {
                m_Type: "int".to_owned(),
                m_Name: (*field).to_owned(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// The Texture2D objects of a fixture bundle's serialized file, by name.
fn textures(bundle: &[u8]) -> Result<BTreeMap<String, Texture2D>> {
    let bundle = BundleFileReader::from_reader(Cursor::new(bundle), &ExtractionConfig::default())?;
    let entry = bundle.main_serializedfile().context("no serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    let mut textures = BTreeMap::new();
    for info in file.objects().filter(|o| o.m_ClassID == ClassId::Texture2D) {
        let texture: Texture2D = file
            .read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )
            .with_context(|| format!("Texture2D {}", info.m_PathID))?;
        textures.insert(texture.m_Name.clone(), texture);
    }
    Ok(textures)
}

use rabex::typetree::NullTypeTreeProvider;

/// Retarget a fixture bundle's bytes to `to`.
fn retarget(bundle: &[u8], to: &rabex::UnityVersion) -> Result<Vec<u8>> {
    let tpk = TypeTreeCache::new(rabex::tpk::TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, to);
    let retarget = Retarget {
        to: to.clone(),
        typetrees: &tpk,
        shader_chunk_size: 2 * 1024 * 1024,
        common_strings: common.iter().map(|(s, o)| (*s, *o)).collect(),
        source_version: None,
        scripts: None,
    };
    retarget
        .bundle(bundle)?
        .context("the retarget changed nothing")
}

#[test]
fn retargeted_textures_match_the_2020_2_build() -> Result<()> {
    let source = std::fs::read(format!("{NEW}/textures"))?;
    let expected = std::fs::read(format!("{OLD}/textures"))?;
    let retargeted = retarget(
        &source,
        &"2020.2.2f1".parse::<rabex::UnityVersion>().unwrap(),
    )?;
    let (mine, theirs, input) = (
        textures(&retargeted)?,
        textures(&expected)?,
        textures(&source)?,
    );

    ensure!(
        mine.len() == theirs.len(),
        "{} textures retargeted, the 2020.2 build has {}",
        mine.len(),
        theirs.len()
    );
    for (name, texture) in &mine {
        let expected = theirs
            .get(name)
            .with_context(|| format!("the 2020.2 build lacks {name}"))?;
        // everything the two forms share carries, pixel data included
        for (field, mine, theirs) in [
            (
                "width",
                texture.m_Width.to_string(),
                expected.m_Width.to_string(),
            ),
            (
                "height",
                texture.m_Height.to_string(),
                expected.m_Height.to_string(),
            ),
            (
                "format",
                texture.m_TextureFormat.to_string(),
                expected.m_TextureFormat.to_string(),
            ),
            (
                "mip count",
                texture.m_MipCount.to_string(),
                expected.m_MipCount.to_string(),
            ),
            (
                "image data",
                texture.image_data.len().to_string(),
                expected.image_data.len().to_string(),
            ),
        ] {
            ensure!(
                mine == theirs,
                "{name}: {field} {mine} vs the 2020.2 build's {theirs}"
            );
        }
        // pixel data carries verbatim; what the editors' compressors produce from the
        // same PNG differs (the 6000 and 2020.2 BC7 encoders disagree), so the
        // comparison runs against the source, not the other build
        ensure!(
            texture.image_data == input[name].image_data,
            "{name}: pixel data doesn't carry"
        );
        // the settings and streaming info carry verbatim: the two editors' import
        // defaults differ (the 6000 imports wrap as repeat, 2020.2 as clamp), so the
        // 2020.2 build of the same content disagrees with the source here
        ensure!(
            format!("{:?}", texture.m_TextureSettings)
                == format!("{:?}", input[name].m_TextureSettings),
            "{name}: texture settings don't carry"
        );
        ensure!(
            format!("{:?}", texture.m_StreamData) == format!("{:?}", input[name].m_StreamData),
            "{name}: streaming info doesn't carry"
        );
        // the fields the rule fills: what Unity's own 2020.2 build writes
        ensure!(
            texture.m_ForcedFallbackFormat == expected.m_ForcedFallbackFormat,
            "{name}: fallback format {:?} vs the build's {:?}",
            texture.m_ForcedFallbackFormat,
            expected.m_ForcedFallbackFormat
        );
        ensure!(
            texture.m_DownscaleFallback == expected.m_DownscaleFallback,
            "{name}: downscale fallback differs"
        );
        ensure!(
            texture.m_IgnoreMasterTextureLimit == expected.m_IgnoreMasterTextureLimit,
            "{name}: master texture limit differs"
        );
    }
    Ok(())
}

#[test]
fn texture_fields_round_trip() -> Result<()> {
    let old_form = tree_of(&["m_IgnoreMasterTextureLimit"]);
    let new_form = tree_of(&["m_IgnoreMipmapLimit"]);
    let texture = Texture2D {
        m_Name: "round trip".to_owned(),
        m_ForcedFallbackFormat: None,
        m_DownscaleFallback: None,
        m_IsAlphaChannelOptional: false,
        m_Width: 4,
        m_Height: 4,
        m_CompleteImageSize: 64,
        m_MipsStripped: 0,
        m_TextureFormat: 4,
        m_MipCount: 1,
        m_IsReadable: false,
        m_IsPreProcessed: false,
        m_IgnoreMasterTextureLimit: None,
        m_StreamingMipmaps: false,
        m_StreamingMipmapsPriority: 0,
        m_ImageCount: 1,
        m_TextureDimension: 2,
        m_TextureSettings: rabex_shader::types::GLTextureSettings {
            m_FilterMode: 1,
            m_Aniso: 1,
            m_MipBias: 0.0,
            m_WrapU: 1,
            m_WrapV: 1,
            m_WrapW: 1,
        },
        m_LightmapFormat: 0,
        m_ColorSpace: 0,
        m_PlatformBlob: Vec::new(),
        image_data: vec![7; 64],
        m_StreamData: rabex_shader::types::StreamingInfo {
            offset: 0,
            size: 0,
            path: String::new(),
        },
        m_IgnoreMipmapLimit: Some(true),
        m_MipmapLimitGroupName: Some("group".to_owned()),
    };
    let old = rabex_convert::texture::convert(texture.clone(), &old_form)?;
    // the new form's mipmap limit flag arrives under the old name, the group name has
    // no old equivalent, and the fallback fields fill with the builds' defaults
    ensure!(old.m_IgnoreMasterTextureLimit == Some(true));
    ensure!(old.m_IgnoreMipmapLimit.is_none());
    ensure!(old.m_MipmapLimitGroupName.is_none());
    ensure!(old.m_ForcedFallbackFormat == Some(4));
    ensure!(old.m_DownscaleFallback == Some(false));

    let new = rabex_convert::texture::convert(old, &new_form)?;
    ensure!(new.m_IgnoreMipmapLimit == Some(true));
    ensure!(new.m_IgnoreMasterTextureLimit.is_none());
    Ok(())
}

#[test]
fn material_keywords_round_trip() -> Result<()> {
    let old_form = tree_of(&["m_ShaderKeywords"]);
    let new_form = tree_of(&["m_ValidKeywords", "m_InvalidKeywords"]);
    let mut properties = UnityPropertySheet {
        m_TexEnvs: Default::default(),
        m_Floats: Default::default(),
        m_Colors: Default::default(),
        m_Ints: Some([("count".to_owned(), 7)].into_iter().collect()),
    };
    let material = Material {
        m_Name: "round trip".to_owned(),
        m_Shader: Default::default(),
        m_ShaderKeywords: None,
        m_LightmapFlags: 0,
        m_EnableInstancingVariants: false,
        m_DoubleSidedGI: false,
        m_CustomRenderQueue: 0,
        stringTagMap: Default::default(),
        disabledShaderPasses: Vec::new(),
        m_SavedProperties: properties.clone(),
        m_BuildTextureStacks: Vec::new(),
        m_ValidKeywords: Some(vec!["KEY_A".to_owned(), "KEY_B".to_owned()]),
        m_InvalidKeywords: Some(Vec::new()),
    };
    let mut dropped = BTreeMap::new();
    let old = rabex_convert::material::convert(material, &old_form, &mut dropped)?;
    // the keyword vector joins into the space separated string, the integer property
    // moves in with the floats
    ensure!(old.m_ShaderKeywords.as_deref() == Some("KEY_A KEY_B"));
    ensure!(old.m_SavedProperties.m_Floats.get("count") == Some(&7.0));
    ensure!(old.m_SavedProperties.m_Ints.is_none());
    ensure!(dropped.is_empty(), "nothing dropped: {dropped:?}");
    properties.m_Ints = None;

    let mut dropped = BTreeMap::new();
    let new = rabex_convert::material::convert(old, &new_form, &mut dropped)?;
    ensure!(
        new.m_ValidKeywords.as_deref() == Some(["KEY_A".to_owned(), "KEY_B".to_owned()].as_slice())
    );
    ensure!(new.m_InvalidKeywords.as_deref() == Some([].as_slice()));
    Ok(())
}
