//! Interpret sprites Unity imported and packed into atlases, and compare against Unity's public
//! `Sprite` API and the sprite's mesh drawn with its uvs.

mod common;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex::objects::pptr::PathId;
use rabex_texture::Texture2D;
use rabex_texture::sprite::{self, Sprite, SpriteAtlas, SpriteRenderData};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    unity_version: String,
    sprites: Vec<SpriteEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpriteEntry {
    name: String,
    mesh_type: String,
    rect: Rect,
    pivot: Vec2,
    pixels_per_unit: f32,
    packed: bool,
    packing_mode: String,
    packing_rotation: String,
    texture: String,
    texture_rect: Rect,
    texture_rect_offset: Vec2,
    expected: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Vec2 {
    x: f32,
    y: f32,
}

impl From<sprite::Rect> for Rect {
    fn from(r: sprite::Rect) -> Self {
        Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

impl From<(f32, f32)> for Vec2 {
    fn from((x, y): (f32, f32)) -> Self {
        Vec2 { x, y }
    }
}

/// Every manifest entry with its sprite and the render data locating its pixels.
fn for_each_sprite(
    mut f: impl FnMut(
        &common::Group<Manifest>,
        &SpriteEntry,
        &Sprite,
        &SpriteRenderData,
    ) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for group in common::groups::<Manifest>("sprites")? {
        let sprites: BTreeMap<String, Sprite> = group
            .objects::<Sprite>(ClassId::Sprite)?
            .into_iter()
            .map(|(_, s)| (s.m_Name.clone(), s))
            .collect();
        let atlases: BTreeMap<PathId, SpriteAtlas> = group
            .objects::<SpriteAtlas>(ClassId::SpriteAtlas)?
            .into_iter()
            .collect();
        for entry in &group.manifest.sprites {
            let sprite = sprites
                .get(&entry.name)
                .with_context(|| format!("{} missing from bundle", entry.name))?;
            let atlas = atlases.get(&sprite.m_SpriteAtlas.m_PathID);
            let rd = sprite.render_data(atlas)?;
            for failure in f(&group, entry, sprite, rd)? {
                let version = &group.manifest.unity_version;
                failures.push(format!("{version} {}: {failure}", entry.name));
            }
        }
    }
    Ok(failures)
}

#[test]
fn sprite_fields_match_unity() -> Result<()> {
    let failures = for_each_sprite(|group, entry, sprite, rd| {
        let mut failures = Vec::new();
        let mut check = |what: &str, ok: bool, actual: String, expected: String| {
            if !ok {
                failures.push(format!("{what}: {actual} != {expected}"));
            }
        };
        let rect = Rect::from(sprite.m_Rect);
        check(
            "rect",
            rect == entry.rect,
            format!("{rect:?}"),
            format!("{:?}", entry.rect),
        );
        let pivot = Vec2 {
            x: sprite.m_Pivot.0 * sprite.m_Rect.width,
            y: sprite.m_Pivot.1 * sprite.m_Rect.height,
        };
        check(
            "pivot (px)",
            (pivot.x - entry.pivot.x).abs() < 1e-3 && (pivot.y - entry.pivot.y).abs() < 1e-3,
            format!("{pivot:?}"),
            format!("{:?}", entry.pivot),
        );
        check(
            "pixels per unit",
            sprite.m_PixelsToUnits == entry.pixels_per_unit,
            sprite.m_PixelsToUnits.to_string(),
            entry.pixels_per_unit.to_string(),
        );
        let texture_rect = Rect::from(rd.textureRect);
        check(
            "textureRect",
            texture_rect == entry.texture_rect,
            format!("{texture_rect:?}"),
            format!("{:?}", entry.texture_rect),
        );
        let offset = Vec2::from(rd.textureRectOffset);
        check(
            "textureRectOffset",
            offset == entry.texture_rect_offset,
            format!("{offset:?}"),
            format!("{:?}", entry.texture_rect_offset),
        );

        let settings = rd.settings();
        check(
            "packed",
            settings.packed == entry.packed,
            settings.packed.to_string(),
            entry.packed.to_string(),
        );
        if entry.packed {
            let mode = if settings.tight { "Tight" } else { "Rectangle" };
            check(
                "packingMode",
                mode == entry.packing_mode,
                mode.into(),
                entry.packing_mode.clone(),
            );
            let rotation = format!("{:?}", settings.rotation);
            check(
                "packingRotation",
                rotation == entry.packing_rotation,
                rotation,
                entry.packing_rotation.clone(),
            );
        }
        let mesh_type = if (rd.settingsRaw >> 6) & 1 == 1 {
            "Tight"
        } else {
            "FullRect"
        };
        check(
            "meshType (bit 6)",
            mesh_type == entry.mesh_type,
            mesh_type.into(),
            entry.mesh_type.clone(),
        );

        let texture: Texture2D = group.read(rd.texture.m_PathID)?;
        check(
            "texture",
            texture.m_Name == entry.texture,
            texture.m_Name,
            entry.texture.clone(),
        );
        Ok(failures)
    })?;
    assert_eq!(failures, Vec::<String>::new());
    Ok(())
}

#[test]
fn rendered_sprites_match_unity() -> Result<()> {
    let failures = for_each_sprite(|group, entry, sprite, rd| {
        let texture: Texture2D = group.read(rd.texture.m_PathID)?;
        let decoded = texture.decode(&group.pixels(&texture)?)?;
        let rendered = match sprite::render_sprite(sprite, rd, &decoded) {
            Ok(img) => img,
            Err(e) => return Ok(vec![e.to_string()]),
        };
        let expected = group.image(&entry.expected)?;
        let triangles = sprite::mesh_triangles(sprite)?;
        let mut unambiguous = Vec::new();
        for (x, y, actual) in rendered.enumerate_pixels() {
            let wanted = expected.get_pixel(x, y);
            if actual != wanted && !on_edge(&triangles, (x as f64 + 0.5, y as f64 + 0.5)) {
                unambiguous.push(format!("({x}, {y}): {:?} != {:?}", actual.0, wanted.0));
            }
        }
        Ok(match unambiguous.len() {
            0 => vec![],
            n => vec![format!("{n} px differ, first {}", unambiguous[0])],
        })
    })?;
    assert_eq!(failures, Vec::<String>::new());
    Ok(())
}

/// Whether `p` lies on an edge of the mesh, where whether a pixel center counts as inside comes down
/// to float precision and the rasterizer's fill rule.
fn on_edge(triangles: &[[(f64, f64); 3]], p: (f64, f64)) -> bool {
    let distance = |a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((a.0 + t * dx - p.0).powi(2) + (a.1 + t * dy - p.1).powi(2)).sqrt()
    };
    triangles
        .iter()
        .flat_map(|&[a, b, c]| [(a, b), (b, c), (c, a)])
        .any(|(a, b)| distance(a, b) < 1e-3)
}
