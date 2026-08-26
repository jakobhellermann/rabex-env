//! Decode Unity `Texture2D` pixel data into RGBA images. IO-free.
//! Supported formats: BC7.

use anyhow::{Result, bail};
use image::RgbaImage;
use serde::Deserialize;

/// `Texture2D.m_TextureFormat` value for BC7.
pub const BC7: i32 = 25;

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
    match format {
        BC7 => decode_bc7(width, height, data),
        other => bail!("unsupported Texture2D format {other} (supported: BC7={BC7})"),
    }
}

/// Encode an RGBA image as PNG bytes.
pub fn to_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn decode_bc7(width: u32, height: u32, data: &[u8]) -> Result<RgbaImage> {
    let (w, h) = (width as usize, height as usize);
    let mut buf = vec![0u32; w * h];
    texture2ddecoder::decode_bc7(data, w, h, &mut buf).map_err(anyhow::Error::msg)?;

    // Decoder output is little-endian BGRA, bottom row first; repack to RGBA and flip.
    let mut img = RgbaImage::new(width, height);
    for y in 0..h {
        for x in 0..w {
            let [b, g, r, a] = buf[y * w + x].to_le_bytes();
            img.put_pixel(x as u32, (h - 1 - y) as u32, image::Rgba([r, g, b, a]));
        }
    }
    Ok(img)
}
