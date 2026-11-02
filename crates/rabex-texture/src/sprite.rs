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
#[derive(Debug, Deserialize, Clone)]
#[allow(non_snake_case)]
pub struct SpriteRenderData {
    pub texture: TypedPPtr<Texture2D>,
    /// The sprite's trimmed opaque part within `texture`, y-up.
    pub textureRect: Rect,
    /// Position of that trimmed part within the sprite's full rect, y-up.
    pub textureRectOffset: (f32, f32),
    pub settingsRaw: u32,
    /// The sprite's mesh. Atlas entries leave it empty; the sprite's own render data has it.
    #[serde(default)]
    pub m_SubMeshes: Vec<SubMesh>,
    #[serde(default)]
    pub m_IndexBuffer: Vec<u8>,
    #[serde(default)]
    pub m_VertexData: VertexData,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(non_snake_case)]
pub struct SubMesh {
    pub firstByte: u32,
    pub indexCount: u32,
    pub topology: i32,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(non_snake_case)]
pub struct VertexData {
    pub m_VertexCount: u32,
    pub m_Channels: Vec<ChannelInfo>,
    pub m_DataSize: Vec<u8>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChannelInfo {
    pub stream: u8,
    pub offset: u8,
    pub format: u8,
    pub dimension: u8,
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

    // Unity draws exactly the mesh; with tight packing, the textureRect also covers parts of atlas
    // neighbours.
    let mask = mesh_mask(sprite)?;
    for (pixel, inside) in full.pixels_mut().zip(mask) {
        if !inside {
            pixel.0 = [0; 4];
        }
    }
    Ok(full)
}

/// Which pixels of the sprite's full rect (top-down, row-major) its mesh covers.
fn mesh_mask(sprite: &Sprite) -> Result<Vec<bool>> {
    let (w, h) = (
        sprite.m_Rect.width.round() as usize,
        sprite.m_Rect.height.round() as usize,
    );
    let mut mask = vec![false; w * h];
    for [a, b, c] in mesh_triangles(sprite)? {
        fill_triangle(&mut mask, w, h, a, b, c);
    }
    Ok(mask)
}

/// The triangles of the sprite's mesh, in pixels of its full rect, top-down.
pub fn mesh_triangles(sprite: &Sprite) -> Result<Vec<[(f64, f64); 3]>> {
    let rd = &sprite.m_RD;
    let vd = &rd.m_VertexData;
    ensure!(!rd.m_SubMeshes.is_empty(), "sprite without a mesh");
    let position = vd
        .m_Channels
        .first()
        .context("mesh without vertex channels")?;
    ensure!(
        position.format == 0,
        "vertex positions in format {}, expected float32",
        position.format
    );
    ensure!(
        position.stream == 0,
        "vertex positions in stream {}",
        position.stream
    );
    let format_size = |format: u8| -> Result<usize> {
        Ok(match format {
            0 | 10 | 11 => 4,       // Float32, UInt32, SInt32
            1 | 4 | 5 | 8 | 9 => 2, // Float16, UNorm16, SNorm16, UInt16, SInt16
            2 | 3 | 6 | 7 => 1,     // UNorm8, SNorm8, UInt8, SInt8
            other => bail!("unknown vertex format {other}"),
        })
    };
    // A stream's channels are interleaved per vertex.
    let mut stride = 0;
    for channel in vd
        .m_Channels
        .iter()
        .filter(|c| c.stream == 0 && c.dimension & 0xf > 0)
    {
        stride += format_size(channel.format)? * (channel.dimension & 0xf) as usize;
    }

    let (w, h) = (sprite.m_Rect.width as f64, sprite.m_Rect.height as f64);
    let ppu = sprite.m_PixelsToUnits as f64;
    let (pivot_x, pivot_y) = (sprite.m_Pivot.0 as f64, sprite.m_Pivot.1 as f64);
    let vertex = |i: usize| -> Result<(f64, f64)> {
        ensure!(
            i < vd.m_VertexCount as usize,
            "index {i} beyond {} vertices",
            vd.m_VertexCount
        );
        let at = i * stride + position.offset as usize;
        let float = |o: usize| -> Result<f64> {
            let bytes = vd
                .m_DataSize
                .get(at + o..at + o + 4)
                .context("vertex data too short")?;
            Ok(f32::from_le_bytes(bytes.try_into().expect("4 bytes")) as f64)
        };
        // Pivot-relative units to pixels of the full rect, flipped to top-down.
        Ok((
            float(0)? * ppu + w * pivot_x,
            h - (float(4)? * ppu + h * pivot_y),
        ))
    };

    let mut triangles = Vec::new();
    for sub in &rd.m_SubMeshes {
        ensure!(
            sub.topology == 0,
            "submesh topology {}, expected triangles",
            sub.topology
        );
        let start = sub.firstByte as usize;
        let indices = rd
            .m_IndexBuffer
            .get(start..start + sub.indexCount as usize * 2)
            .context("index buffer too short")?;
        let indices: Vec<usize> = indices
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&bytes| u16::from_le_bytes(bytes) as usize)
            .collect();
        for &[i, j, k] in indices.as_chunks::<3>().0 {
            triangles.push([vertex(i)?, vertex(j)?, vertex(k)?]);
        }
    }
    Ok(triangles)
}

/// Marks the pixels whose center lies inside (or on the edge of) the triangle.
fn fill_triangle(
    mask: &mut [bool],
    w: usize,
    h: usize,
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
) {
    let edge = |p: (f64, f64), q: (f64, f64), r: (f64, f64)| {
        (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
    };
    let area = edge(a, b, c);
    if area == 0.0 {
        return;
    }
    let x0 = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
    let x1 = (a.0.max(b.0).max(c.0).ceil() as usize).min(w);
    let y0 = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
    let y1 = (a.1.max(b.1).max(c.1).ceil() as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = (x as f64 + 0.5, y as f64 + 0.5);
            let inside = [edge(a, b, p), edge(b, c, p), edge(c, a, p)]
                .into_iter()
                .all(|e| e / area >= -1e-9);
            if inside {
                mask[y * w + x] = true;
            }
        }
    }
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
            // packed, rectangle packing
            settingsRaw: 1 | 2 | (rotation << 2),
            m_SubMeshes: vec![SubMesh {
                firstByte: 0,
                indexCount: 6,
                topology: 0,
            }],
            m_IndexBuffer: [0u16, 1, 2, 2, 1, 3]
                .iter()
                .flat_map(|i| i.to_le_bytes())
                .collect(),
            // a quad over the full 3x3 rect, in units around its center pivot
            m_VertexData: VertexData {
                m_VertexCount: 4,
                m_Channels: vec![ChannelInfo {
                    stream: 0,
                    offset: 0,
                    format: 0,
                    dimension: 3,
                }],
                m_DataSize: [
                    [-0.015f32, -0.015, 0.0],
                    [0.015, -0.015, 0.0],
                    [-0.015, 0.015, 0.0],
                    [0.015, 0.015, 0.0],
                ]
                .iter()
                .flatten()
                .flat_map(|f| f.to_le_bytes())
                .collect(),
            },
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
            m_RD: rd.clone(),
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
            m_RenderDataMap: vec![(key, rd.clone())],
        };
        assert_eq!(
            sprite.render_data(Some(&atlas)).unwrap().textureRect,
            rd.textureRect
        );
        assert!(sprite.render_data(None).is_err());
    }
}

#[cfg(test)]
mod mesh_tests {
    use rabex::objects::PPtr;

    use super::*;

    #[test]
    fn tight_packing_masks_pixels_outside_the_mesh() {
        // A 2x2 sprite (1 px per unit, pivot at its center) whose mesh is the triangle over its
        // top-left half: y-up (-1, -1), (-1, 1), (1, 1).
        let positions: Vec<u8> = [[-1.0f32, -1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, 1.0, 0.0]]
            .iter()
            .flatten()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let rd = SpriteRenderData {
            texture: PPtr {
                m_FileID: 0.into(),
                m_PathID: 1,
            }
            .typed(),
            textureRect: Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            textureRectOffset: (0.0, 0.0),
            // packed, tight packing
            settingsRaw: 1,
            m_SubMeshes: vec![SubMesh {
                firstByte: 0,
                indexCount: 3,
                topology: 0,
            }],
            m_IndexBuffer: [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect(),
            m_VertexData: VertexData {
                m_VertexCount: 3,
                m_Channels: vec![ChannelInfo {
                    stream: 0,
                    offset: 0,
                    format: 0,
                    dimension: 3,
                }],
                m_DataSize: positions,
            },
        };
        let sprite = Sprite {
            m_Name: String::new(),
            m_Rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            m_Pivot: (0.5, 0.5),
            m_PixelsToUnits: 1.0,
            m_RD: rd.clone(),
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
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255]));
        let alpha: Vec<u8> = render_sprite(&sprite, &rd, &texture)
            .unwrap()
            .pixels()
            .map(|p| p.0[3])
            .collect();
        // top-down: top-left, top-right, bottom-left are inside, bottom-right is not
        assert_eq!(alpha, [255, 255, 255, 0]);
    }
}
