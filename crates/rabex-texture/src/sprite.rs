//! Cut Unity `Sprite`s out of their texture, which is often a shared atlas.

use anyhow::{Context, Result, bail, ensure};
use rabex::objects::TypedPPtr;
use serde::Deserialize;

use crate::{RgbaImage, Texture2D};

/// Unity `Sprite`, trimmed to the fields rendering needs.
#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct Sprite {
    #[serde(default)]
    pub m_Name: String,
    pub m_Rect: Rect,
    pub m_Pivot: (f32, f32),
    pub m_PixelsToUnits: f32,
    pub m_RD: SpriteRenderData,
    pub m_SpriteAtlas: TypedPPtr<SpriteAtlas>,
    pub m_RenderDataKey: (Guid, i64),
}

/// Where a sprite's pixels live. Atlas-packed sprites leave `texture` empty in their own render
/// data; their [`SpriteAtlas`] holds the real one under the sprite's render data key.
#[derive(Debug, Deserialize, Clone, Copy)]
#[allow(non_snake_case)]
pub struct SpriteRenderData {
    pub texture: TypedPPtr<Texture2D>,
    /// The sprite's trimmed opaque part within `texture`, y-up.
    pub textureRect: Rect,
    /// Position of that trimmed part within the sprite's full rect, y-up.
    pub textureRectOffset: (f32, f32),
    pub settingsRaw: u32,
}

impl SpriteRenderData {
    pub fn settings(&self) -> SpriteSettings {
        SpriteSettings::from_raw(self.settingsRaw)
    }
}

/// Unity `SpriteAtlas`, trimmed to the render data of its sprites.
#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct SpriteAtlas {
    pub m_RenderDataMap: Vec<((Guid, i64), SpriteRenderData)>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    #[serde(rename = "data[0]")]
    pub a: u32,
    #[serde(rename = "data[1]")]
    pub b: u32,
    #[serde(rename = "data[2]")]
    pub c: u32,
    #[serde(rename = "data[3]")]
    pub d: u32,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Unity's packed `SpriteSettings` bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteSettings {
    pub packed: bool,
    /// Tight packing fits sprites by their mesh, so a sprite's textureRect may overlap neighbours.
    pub tight: bool,
    pub rotation: PackingRotation,
}

impl SpriteSettings {
    pub fn from_raw(raw: u32) -> Self {
        SpriteSettings {
            packed: raw & 1 == 1,
            tight: (raw >> 1) & 1 == 0,
            rotation: PackingRotation::from_raw((raw >> 2) & 0b1111),
        }
    }
}

/// How a sprite was turned to fit into its atlas (`SpritePackingRotation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackingRotation {
    None,
    FlipHorizontal,
    FlipVertical,
    Rotate180,
    Rotate90,
    Unknown(u32),
}

impl PackingRotation {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::None,
            1 => Self::FlipHorizontal,
            2 => Self::FlipVertical,
            3 => Self::Rotate180,
            4 => Self::Rotate90,
            other => Self::Unknown(other),
        }
    }
}

impl Sprite {
    /// Render data locating the sprite's pixels: its own, or its entry in `atlas` for atlas-packed
    /// sprites, which then must be the sprite's [`m_SpriteAtlas`](Self::m_SpriteAtlas).
    pub fn render_data<'a>(
        &'a self,
        atlas: Option<&'a SpriteAtlas>,
    ) -> Result<&'a SpriteRenderData> {
        if !self.is_atlas_packed() {
            return Ok(&self.m_RD);
        }
        let atlas = atlas.context("sprite is atlas-packed, but no atlas was given")?;
        atlas
            .m_RenderDataMap
            .iter()
            .find(|(key, _)| *key == self.m_RenderDataKey)
            .map(|(_, rd)| rd)
            .context("sprite missing from its atlas' render data")
    }

    /// Whether the pixel location lives in the sprite's atlas rather than its own render data.
    pub fn is_atlas_packed(&self) -> bool {
        self.m_RD.texture.m_PathID == 0
    }
}

/// Render `sprite` over its full rect as a top-down image, from its (decoded) `texture` and the
/// `render_data` from [`Sprite::render_data`].
pub fn render_sprite(
    sprite: &Sprite,
    render_data: &SpriteRenderData,
    texture: &RgbaImage,
) -> Result<RgbaImage> {
    let settings = render_data.settings();

    let tr = render_data.textureRect;
    let (crop_w, crop_h) = (tr.width.round() as u32, tr.height.round() as u32);
    // The texture is decoded top-down, textureRect is y-up.
    let crop_x = tr.x.round() as u32;
    let crop_y = (texture.height() as f32 - tr.y - tr.height).round() as u32;
    ensure!(
        crop_x + crop_w <= texture.width() && crop_y + crop_h <= texture.height(),
        "textureRect outside the {}x{} texture",
        texture.width(),
        texture.height()
    );
    let crop = image::imageops::crop_imm(texture, crop_x, crop_y, crop_w, crop_h).to_image();

    // Verified against UnityPy's output on Hollow Knight: Silksong's map atlases.
    let trimmed = match (settings.packed, settings.rotation) {
        (false, _) | (true, PackingRotation::None) => crop,
        (true, PackingRotation::FlipHorizontal) => image::imageops::flip_horizontal(&crop),
        (true, PackingRotation::FlipVertical) => image::imageops::flip_vertical(&crop),
        (true, PackingRotation::Rotate180) => image::imageops::rotate180(&crop),
        (true, rotation) => bail!("unsupported packing rotation {rotation:?}"),
    };

    let rect = sprite.m_Rect;
    let mut full = RgbaImage::new(rect.width.round() as u32, rect.height.round() as u32);
    let (offset_x, offset_y) = render_data.textureRectOffset;
    let paste_x = offset_x.round() as i64;
    let paste_y = (rect.height - offset_y - trimmed.height() as f32).round() as i64;
    image::imageops::replace(&mut full, &trimmed, paste_x, paste_y);
    Ok(full)
}

#[cfg(test)]
mod tests {
    use rabex::objects::PPtr;

    use super::*;

    /// A 4x2 texture whose pixels encode their own top-down position as `[x, y, 0, 255]`.
    fn texture() -> RgbaImage {
        RgbaImage::from_fn(4, 2, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
    }

    fn sprite(rotation: u32) -> (Sprite, SpriteRenderData) {
        let rd = SpriteRenderData {
            texture: PPtr {
                m_FileID: 0.into(),
                m_PathID: 1,
            }
            .typed(),
            // the left 2x2 of the texture
            textureRect: Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            // placed at the top-left of a 3x3 sprite (y-up offset from the bottom)
            textureRectOffset: (0.0, 1.0),
            settingsRaw: 1 | (rotation << 2),
        };
        let sprite = Sprite {
            m_Name: String::new(),
            m_Rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 3.0,
                height: 3.0,
            },
            m_Pivot: (0.5, 0.5),
            m_PixelsToUnits: 100.0,
            m_RD: rd,
            m_SpriteAtlas: PPtr::null().typed(),
            m_RenderDataKey: (
                Guid {
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                },
                0,
            ),
        };
        (sprite, rd)
    }

    fn rendered(rotation: u32) -> Vec<[u8; 4]> {
        let (sprite, rd) = sprite(rotation);
        render_sprite(&sprite, &rd, &texture())
            .unwrap()
            .pixels()
            .map(|p| p.0)
            .collect()
    }

    const EMPTY: [u8; 4] = [0, 0, 0, 0];

    #[test]
    fn trimmed_part_is_placed_at_its_offset_in_the_full_rect() {
        #[rustfmt::skip]
        assert_eq!(rendered(0), [
            [0, 0, 0, 255], [1, 0, 0, 255], EMPTY,
            [0, 1, 0, 255], [1, 1, 0, 255], EMPTY,
            EMPTY,          EMPTY,          EMPTY,
        ]);
    }

    #[test]
    fn packing_rotation_is_undone() {
        #[rustfmt::skip]
        assert_eq!(rendered(1), [
            [1, 0, 0, 255], [0, 0, 0, 255], EMPTY,
            [1, 1, 0, 255], [0, 1, 0, 255], EMPTY,
            EMPTY,          EMPTY,          EMPTY,
        ]);
        #[rustfmt::skip]
        assert_eq!(rendered(2), [
            [0, 1, 0, 255], [1, 1, 0, 255], EMPTY,
            [0, 0, 0, 255], [1, 0, 0, 255], EMPTY,
            EMPTY,          EMPTY,          EMPTY,
        ]);
        #[rustfmt::skip]
        assert_eq!(rendered(3), [
            [1, 1, 0, 255], [0, 1, 0, 255], EMPTY,
            [1, 0, 0, 255], [0, 0, 0, 255], EMPTY,
            EMPTY,          EMPTY,          EMPTY,
        ]);
    }

    #[test]
    fn atlas_packed_sprites_use_their_atlas_entry() {
        let (mut sprite, rd) = sprite(0);
        sprite.m_RD.texture = PPtr::null().typed();
        let key = (
            Guid {
                a: 1,
                b: 2,
                c: 3,
                d: 4,
            },
            7,
        );
        sprite.m_RenderDataKey = key;
        let atlas = SpriteAtlas {
            m_RenderDataMap: vec![(key, rd)],
        };
        assert_eq!(
            sprite.render_data(Some(&atlas)).unwrap().textureRect,
            rd.textureRect
        );
        assert!(sprite.render_data(None).is_err());
    }
}
