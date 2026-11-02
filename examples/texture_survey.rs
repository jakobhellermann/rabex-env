//! Count which Texture2D / Sprite / SpriteAtlas features a game uses.

mod utils;

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rabex::objects::{ClassId, ClassIdType, PPtr};
use rayon::prelude::*;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_derive::Deserialize;

/// Length of a byte array, without keeping its contents.
#[derive(Debug, Default)]
struct Len(usize);
impl<'de> serde::Deserialize<'de> for Len {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Len;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a byte array")
            }
            fn visit_bytes<E>(self, v: &[u8]) -> Result<Len, E> {
                Ok(Len(v.len()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Len, A::Error> {
                let mut n = 0;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    n += 1;
                }
                Ok(Len(n))
            }
        }
        d.deserialize_seq(V)
    }
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct Texture2D {
    m_Width: i32,
    m_Height: i32,
    m_CompleteImageSize: u32,
    m_MipsStripped: i32,
    m_TextureFormat: i32,
    m_MipCount: i32,
    m_IsReadable: bool,
    m_IsPreProcessed: bool,
    m_StreamingMipmaps: bool,
    m_ImageCount: i32,
    m_TextureDimension: i32,
    m_TextureSettings: TextureSettings,
    m_LightmapFormat: i32,
    m_ColorSpace: i32,
    m_IsAlphaChannelOptional: bool,
    m_PlatformBlob: Len,
    #[serde(rename = "image data")]
    image_data: Len,
    m_StreamData: StreamingInfo,
}
impl ClassIdType for Texture2D {
    const CLASS_ID: ClassId = ClassId::Texture2D;
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct TextureSettings {
    m_FilterMode: i32,
    m_WrapU: i32,
    m_WrapV: i32,
}

#[derive(Debug, Deserialize)]
struct StreamingInfo {
    size: u32,
    path: String,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Vec2 {
    x: f32,
    y: f32,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Vec4 {
    x: f32,
    y: f32,
    z: f32,
    w: f32,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct Sprite {
    m_Rect: Rect,
    m_Offset: Vec2,
    m_Border: Vec4,
    m_PixelsToUnits: f32,
    m_Pivot: Vec2,
    m_Extrude: u32,
    m_IsPolygon: bool,
    m_AtlasTags: Vec<String>,
    m_SpriteAtlas: PPtr,
    m_RD: RenderData,
    m_PhysicsShape: Vec<IgnoredAny>,
    m_Bones: Vec<IgnoredAny>,
}
impl ClassIdType for Sprite {
    const CLASS_ID: ClassId = ClassId::Sprite;
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct RenderData {
    texture: PPtr,
    alphaTexture: PPtr,
    secondaryTextures: Vec<IgnoredAny>,
    #[serde(default)]
    m_SubMeshes: Vec<SubMesh>,
    #[serde(default)]
    m_IndexBuffer: Len,
    #[serde(default)]
    m_VertexData: Option<VertexData>,
    textureRect: Rect,
    textureRectOffset: Vec2,
    atlasRectOffset: Vec2,
    settingsRaw: u32,
    downscaleMultiplier: f32,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct SubMesh {
    indexCount: u32,
    topology: i32,
    vertexCount: u32,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct VertexData {
    m_VertexCount: u32,
    m_Channels: Vec<Channel>,
}

#[derive(Debug, Deserialize)]
struct Channel {
    stream: u8,
    offset: u8,
    format: u8,
    dimension: u8,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
struct SpriteAtlas {
    m_PackedSprites: Vec<IgnoredAny>,
    m_RenderDataMap: Vec<(IgnoredAny, RenderData)>,
    m_IsVariant: bool,
}
impl ClassIdType for SpriteAtlas {
    const CLASS_ID: ClassId = ClassId::SpriteAtlas;
}

/// Feature -> (count, first example).
#[derive(Default)]
struct Survey(Mutex<BTreeMap<String, (usize, String)>>);
impl Survey {
    fn add(&self, feature: impl Into<String>, example: impl FnOnce() -> String) {
        let mut map = self.0.lock().unwrap();
        let entry = map.entry(feature.into()).or_insert_with(|| (0, example()));
        entry.0 += 1;
    }
}

fn frac(v: f32) -> bool {
    v.fract() != 0.0
}

fn texture_features(t: &Texture2D) -> Vec<String> {
    let mut f = vec![
        format!("texture.format={}", t.m_TextureFormat),
        format!("texture.dimension={}", t.m_TextureDimension),
        format!("texture.image_count={}", t.m_ImageCount),
        format!("texture.colorspace={}", t.m_ColorSpace),
        format!("texture.filter={}", t.m_TextureSettings.m_FilterMode),
        format!(
            "texture.wrap={}/{}",
            t.m_TextureSettings.m_WrapU, t.m_TextureSettings.m_WrapV
        ),
        format!("texture.lightmap_format={}", t.m_LightmapFormat),
        format!("texture.readable={}", t.m_IsReadable),
        format!("texture.preprocessed={}", t.m_IsPreProcessed),
        format!("texture.streaming_mipmaps={}", t.m_StreamingMipmaps),
        format!("texture.alpha_optional={}", t.m_IsAlphaChannelOptional),
        format!("texture.mips_stripped={}", t.m_MipsStripped),
        format!(
            "texture.mipmaps={}",
            if t.m_MipCount > 1 { "yes" } else { "no" }
        ),
        format!(
            "texture.format={}.size_multiple_of_4={}",
            t.m_TextureFormat,
            t.m_Width % 4 == 0 && t.m_Height % 4 == 0
        ),
        format!(
            "texture.pot={}",
            (t.m_Width as u32).is_power_of_two() && (t.m_Height as u32).is_power_of_two()
        ),
        format!("texture.square={}", t.m_Width == t.m_Height),
        format!("texture.platform_blob={}", t.m_PlatformBlob.0 > 0),
    ];
    let storage = match (t.image_data.0 > 0, t.m_StreamData.path.as_str()) {
        (true, "") => "inline".to_owned(),
        (true, _) => "inline+stream".to_owned(),
        (false, "") => "empty".to_owned(),
        (false, p) if p.starts_with("archive:/") => "stream:archive".to_owned(),
        (false, _) => "stream:file".to_owned(),
    };
    f.push(format!("texture.storage={storage}"));
    let len = t.image_data.0.max(t.m_StreamData.size as usize);
    f.push(format!(
        "texture.data_len_vs_complete_image_size={}",
        match len.cmp(&(t.m_CompleteImageSize as usize * t.m_ImageCount.max(1) as usize)) {
            std::cmp::Ordering::Equal => "equal",
            std::cmp::Ordering::Less => "less",
            std::cmp::Ordering::Greater => "greater",
        }
    ));
    f
}

fn render_data_features(prefix: &str, rd: &RenderData) -> Vec<String> {
    let s = rd.settingsRaw;
    let mut f = vec![
        format!(
            "{prefix}.settings packed={} packingMode={} rotation={} meshType={}",
            s & 1,
            (s >> 1) & 1,
            (s >> 2) & 0xf,
            (s >> 6) & 1
        ),
        format!("{prefix}.settings.high_bits={:#x}", s >> 7),
        format!("{prefix}.texture_null={}", rd.texture.m_PathID == 0),
        format!(
            "{prefix}.texture_external={}",
            rd.texture.m_FileID != rabex::objects::pptr::FileId::LOCAL
        ),
        format!("{prefix}.alpha_texture={}", rd.alphaTexture.m_PathID != 0),
        format!(
            "{prefix}.secondary_textures={}",
            !rd.secondaryTextures.is_empty()
        ),
        format!(
            "{prefix}.texture_rect_fractional={}",
            frac(rd.textureRect.x)
                || frac(rd.textureRect.y)
                || frac(rd.textureRect.width)
                || frac(rd.textureRect.height)
        ),
        format!(
            "{prefix}.texture_rect_offset_fractional={}",
            frac(rd.textureRectOffset.x) || frac(rd.textureRectOffset.y)
        ),
        format!(
            "{prefix}.atlas_rect_offset={},{}",
            rd.atlasRectOffset.x, rd.atlasRectOffset.y
        ),
        format!("{prefix}.downscale_multiplier={}", rd.downscaleMultiplier),
        format!("{prefix}.submesh_count={}", rd.m_SubMeshes.len()),
    ];
    for sub in &rd.m_SubMeshes {
        f.push(format!("{prefix}.submesh.topology={}", sub.topology));
        let bytes_per_index = match sub.indexCount {
            0 => 0,
            n => rd.m_IndexBuffer.0 / n as usize,
        };
        f.push(format!(
            "{prefix}.submesh.index_bytes={bytes_per_index} (u32 indices needed={})",
            sub.vertexCount > u16::MAX as u32
        ));
    }
    if let Some(vd) = &rd.m_VertexData {
        let layout: Vec<String> = vd
            .m_Channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.dimension & 0xf > 0)
            .map(|(i, c)| {
                format!(
                    "ch{i}:s{}o{}f{}d{}",
                    c.stream, c.offset, c.format, c.dimension
                )
            })
            .collect();
        f.push(format!("{prefix}.vertex_layout={}", layout.join(" ")));
        f.push(format!("{prefix}.has_vertices={}", vd.m_VertexCount > 0));
    }
    f
}

fn sprite_features(s: &Sprite) -> Vec<String> {
    let mut f = vec![
        format!(
            "sprite.rect_fractional={}",
            frac(s.m_Rect.x) || frac(s.m_Rect.y) || frac(s.m_Rect.width) || frac(s.m_Rect.height)
        ),
        format!(
            "sprite.rect_origin_nonzero(own texture)={}",
            s.m_RD.texture.m_PathID != 0 && (s.m_Rect.x != 0.0 || s.m_Rect.y != 0.0)
        ),
        format!(
            "sprite.offset_nonzero={}",
            s.m_Offset.x != 0.0 || s.m_Offset.y != 0.0
        ),
        format!(
            "sprite.border={}",
            s.m_Border.x != 0.0
                || s.m_Border.y != 0.0
                || s.m_Border.z != 0.0
                || s.m_Border.w != 0.0
        ),
        format!("sprite.pixels_to_units={}", s.m_PixelsToUnits),
        format!(
            "sprite.pivot={}",
            match (s.m_Pivot.x, s.m_Pivot.y) {
                (0.5, 0.5) => "center".to_owned(),
                (x, y) if [0.0, 0.5, 1.0].contains(&x) && [0.0, 0.5, 1.0].contains(&y) =>
                    format!("{x},{y}"),
                _ => "custom".to_owned(),
            }
        ),
        format!("sprite.extrude={}", s.m_Extrude),
        format!("sprite.is_polygon={}", s.m_IsPolygon),
        format!("sprite.atlas_tags={}", !s.m_AtlasTags.is_empty()),
        format!("sprite.sprite_atlas_set={}", s.m_SpriteAtlas.m_PathID != 0),
        format!(
            "sprite.atlas_packed(texture null)={} sprite_atlas_set={}",
            s.m_RD.texture.m_PathID == 0,
            s.m_SpriteAtlas.m_PathID != 0
        ),
        format!("sprite.physics_shape={}", !s.m_PhysicsShape.is_empty()),
        format!("sprite.bones={}", !s.m_Bones.is_empty()),
    ];
    f.extend(render_data_features("sprite.rd", &s.m_RD));
    f
}

fn atlas_features(a: &SpriteAtlas) -> Vec<String> {
    let mut f = vec![
        format!("atlas.is_variant={}", a.m_IsVariant),
        format!(
            "atlas.packed_sprites_eq_render_data={}",
            a.m_PackedSprites.len() == a.m_RenderDataMap.len()
        ),
    ];
    for (_, rd) in &a.m_RenderDataMap {
        f.extend(render_data_features("atlas.rd", rd));
    }
    f
}

fn main() -> Result<()> {
    let game = std::env::args().nth(1).unwrap_or_else(|| "silksong".into());
    let env = utils::find_game(&game)?.context("game not found")?;

    let mut files = env.load_all_serialized_files()?;
    for name in ["globalgamemanagers.assets", "resources.assets"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            (0..)
                .map(|i| format!("sharedassets{i}.assets"))
                .take_while(|n| env.load_serialized(n).is_ok()),
        )
        .chain(
            (0..)
                .map(|i| format!("level{i}"))
                .take_while(|n| env.load_serialized(n).is_ok()),
        )
    {
        files.insert(name.clone(), env.load_serialized(&name)?);
    }
    eprintln!("{} serialized files", files.len());

    let survey = Survey::default();
    let errors = Survey::default();
    files
        .par_iter()
        .try_for_each(|(path, file)| -> Result<()> {
            let class_counts = file
                .file
                .objects()
                .fold(BTreeMap::<_, usize>::new(), |mut m, o| {
                    *m.entry(o.m_ClassID).or_default() += 1;
                    m
                });
            for (class, n) in class_counts {
                let name = format!("{class:?}");
                if name.contains("Texture") || name.contains("Sprite") || name.contains("Cubemap") {
                    for _ in 0..n {
                        survey.add(format!("class={name}"), || path.clone());
                    }
                }
            }

            macro_rules! scan {
                ($ty:ty, $features:expr) => {
                    for obj in file.objects_of::<$ty>() {
                        let example = || format!("{path} @ {}", obj.path_id());
                        match obj.read() {
                            Ok(value) => {
                                for feature in $features(&value) {
                                    survey.add(feature, example);
                                }
                            }
                            Err(e) => errors.add(format!("{}: {e}", stringify!($ty)), example),
                        }
                    }
                };
            }
            scan!(Texture2D, texture_features);
            scan!(Sprite, sprite_features);
            scan!(SpriteAtlas, atlas_features);
            Ok(())
        })?;

    for (feature, (count, example)) in survey.0.into_inner().unwrap() {
        println!("{count:>8}  {feature:<70}  {example}");
    }
    let errors = errors.0.into_inner().unwrap();
    if !errors.is_empty() {
        println!("\nread errors:");
        for (error, (count, example)) in errors {
            println!("{count:>8}  {error}  {example}");
        }
    }
    Ok(())
}
