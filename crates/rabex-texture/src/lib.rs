//! Decode Unity `Texture2D` pixel data into RGBA images. IO-free.
//! Supported formats: Alpha8, RGB24, RGBA32, DXT1/BC1, DXT5/BC3, BC7.

use anyhow::{Result, bail, ensure};
use image::RgbaImage;
use serde::Deserialize;

/// `Texture2D.m_TextureFormat` values this crate can decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Alpha8,
    RGB24,
    RGBA32,
    Dxt1,
    Dxt5,
    Bc7,
}

impl TextureFormat {
    /// Map a raw `m_TextureFormat` id, or `None` for one we can't decode.
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => Self::Alpha8,
            3 => Self::RGB24,
            4 => Self::RGBA32,
            10 => Self::Dxt1,
            12 => Self::Dxt5,
            25 => Self::Bc7,
            _ => return None,
        })
    }
}

/// Unity `Texture2D`, trimmed to the fields decoding needs.
#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct Texture2D {
    #[serde(default)]
    pub m_Name: String,
    pub m_Width: i32,
    pub m_Height: i32,
    pub m_TextureFormat: i32,
    #[serde(rename = "image data")]
    pub image_data: Vec<u8>,
    pub m_StreamData: StreamingInfo,
}

/// Location of a texture's pixels in a resource (`.resS`) stream.
#[derive(Debug, Deserialize)]
pub struct StreamingInfo {
    pub offset: u64,
    pub size: u32,
    pub path: String,
}

impl Texture2D {
    /// Decode `pixels` into a top-down RGBA image.
    pub fn decode(&self, pixels: &[u8]) -> Result<RgbaImage> {
        decode(
            self.m_TextureFormat,
            self.m_Width as u32,
            self.m_Height as u32,
            pixels,
        )
    }
}

/// Decode a pixel buffer into a top-down RGBA image.
pub fn decode(format: i32, width: u32, height: u32, data: &[u8]) -> Result<RgbaImage> {
    let Some(format) = TextureFormat::from_id(format) else {
        bail!("unsupported Texture2D format {format}");
    };
    match format {
        TextureFormat::Alpha8 => uncompressed(width, height, data, 1, |p| [p[0], p[0], p[0], 255]),
        TextureFormat::RGB24 => uncompressed(width, height, data, 3, |p| [p[0], p[1], p[2], 255]),
        TextureFormat::RGBA32 => uncompressed(width, height, data, 4, |p| [p[0], p[1], p[2], p[3]]),
        TextureFormat::Dxt1 => block(width, height, data, texture2ddecoder::decode_bc1),
        TextureFormat::Dxt5 => block(width, height, data, texture2ddecoder::decode_bc3),
        TextureFormat::Bc7 => block(width, height, data, texture2ddecoder::decode_bc7),
    }
}

/// Encode an RGBA image as PNG bytes.
pub fn to_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Unity stores textures bottom row first, so every path flips vertically.
fn flip_y(height: usize, y: usize) -> u32 {
    (height - 1 - y) as u32
}

/// Uncompressed layouts: `bpp` bytes per pixel, `to_rgba` maps one pixel.
fn uncompressed(
    width: u32,
    height: u32,
    data: &[u8],
    bpp: usize,
    to_rgba: impl Fn(&[u8]) -> [u8; 4],
) -> Result<RgbaImage> {
    let (w, h) = (width as usize, height as usize);
    ensure!(
        data.len() >= w * h * bpp,
        "texture data too short: {} < {}",
        data.len(),
        w * h * bpp
    );
    let mut img = RgbaImage::new(width, height);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * bpp;
            img.put_pixel(x as u32, flip_y(h, y), image::Rgba(to_rgba(&data[i..i + bpp])));
        }
    }
    Ok(img)
}

/// Block-compressed layouts decoded via `texture2ddecoder`, whose output
/// is little-endian BGRA.
fn block(
    width: u32,
    height: u32,
    data: &[u8],
    decoder: fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>,
) -> Result<RgbaImage> {
    let (w, h) = (width as usize, height as usize);
    let mut buf = vec![0u32; w * h];
    decoder(data, w, h, &mut buf).map_err(anyhow::Error::msg)?;
    let mut img = RgbaImage::new(width, height);
    for y in 0..h {
        for x in 0..w {
            let [b, g, r, a] = buf[y * w + x].to_le_bytes();
            img.put_pixel(x as u32, flip_y(h, y), image::Rgba([r, g, b, a]));
        }
    }
    Ok(img)
}
