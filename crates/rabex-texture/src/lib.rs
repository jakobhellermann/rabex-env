//! Decode Unity `Texture2D` pixel data into RGBA images, and render `Sprite`s from them. IO-free.
//! Supported formats: Alpha8, RGB24, RGBA32, ARGB4444, DXT1/BC1, DXT5/BC3, BC7, DXT1Crunched.

pub mod sprite;

use std::borrow::Cow;

use anyhow::{Result, bail, ensure};
use serde::Deserialize;

pub use image::RgbaImage;

/// `Texture2D.m_TextureFormat` values this crate can decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Alpha8,
    RGB24,
    RGBA32,
    Argb4444,
    Dxt1,
    Dxt5,
    Bc7,
    Dxt1Crunched,
}

impl TextureFormat {
    /// Map a raw `m_TextureFormat` id, or `None` for one we can't decode.
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => Self::Alpha8,
            2 => Self::Argb4444,
            3 => Self::RGB24,
            4 => Self::RGBA32,
            10 => Self::Dxt1,
            12 => Self::Dxt5,
            25 => Self::Bc7,
            28 => Self::Dxt1Crunched,
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
    /// The texture's pixel data: inline, or fetched by `read_stream`, which must return exactly the
    /// `size` bytes at `offset` of the stream's `path`.
    pub fn pixels(
        &self,
        read_stream: impl FnOnce(&StreamingInfo) -> Result<Vec<u8>>,
    ) -> Result<Cow<'_, [u8]>> {
        if !self.image_data.is_empty() {
            return Ok(Cow::Borrowed(&self.image_data));
        }
        let stream = &self.m_StreamData;
        ensure!(
            !stream.path.is_empty(),
            "texture {} has no pixel data",
            self.m_Name
        );
        let data = read_stream(stream)?;
        ensure!(
            data.len() == stream.size as usize,
            "stream of texture {} returned {} bytes, expected {}",
            self.m_Name,
            data.len(),
            stream.size
        );
        Ok(Cow::Owned(data))
    }

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
        TextureFormat::Alpha8 => uncompressed(width, height, data, 1, |p| [0, 0, 0, p[0]]),
        TextureFormat::RGB24 => uncompressed(width, height, data, 3, |p| [p[0], p[1], p[2], 255]),
        TextureFormat::RGBA32 => uncompressed(width, height, data, 4, |p| [p[0], p[1], p[2], p[3]]),
        TextureFormat::Argb4444 => uncompressed(width, height, data, 2, |p| {
            let v = u16::from_le_bytes([p[0], p[1]]);
            let nibble = |shift: u32| (((v >> shift) & 0xF) * 0x11) as u8;
            [nibble(8), nibble(4), nibble(0), nibble(12)]
        }),
        TextureFormat::Dxt1 => block(width, height, data, texture2ddecoder::decode_bc1),
        TextureFormat::Dxt5 => block(width, height, data, texture2ddecoder::decode_bc3),
        TextureFormat::Bc7 => block(width, height, data, texture2ddecoder::decode_bc7),
        TextureFormat::Dxt1Crunched => {
            block(width, height, data, texture2ddecoder::decode_unity_crunch)
        }
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
            img.put_pixel(
                x as u32,
                flip_y(h, y),
                image::Rgba(to_rgba(&data[i..i + bpp])),
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(image_data: Vec<u8>, path: &str, size: u32) -> Texture2D {
        Texture2D {
            m_Name: "t".into(),
            m_Width: 1,
            m_Height: 1,
            m_TextureFormat: 4,
            image_data,
            m_StreamData: StreamingInfo {
                offset: 8,
                size,
                path: path.into(),
            },
        }
    }

    #[test]
    fn pixels_are_inline_or_streamed() {
        let inline = texture(vec![1, 2, 3, 4], "", 0);
        let pixels = inline
            .pixels(|_| panic!("inline pixels must not be streamed"))
            .unwrap();
        assert!(matches!(pixels, Cow::Borrowed([1, 2, 3, 4])));

        let streamed = texture(Vec::new(), "archive:/CAB-a/CAB-a.resS", 4);
        let pixels = streamed
            .pixels(|stream| {
                assert_eq!(
                    (stream.path.as_str(), stream.offset, stream.size),
                    ("archive:/CAB-a/CAB-a.resS", 8, 4)
                );
                Ok(vec![5, 6, 7, 8])
            })
            .unwrap();
        assert_eq!(pixels.as_ref(), [5, 6, 7, 8]);

        assert!(
            texture(Vec::new(), "", 0)
                .pixels(|_| Ok(Vec::new()))
                .is_err()
        );
        assert!(streamed.pixels(|_| Ok(vec![0; 3])).is_err());
    }

    #[test]
    fn decode_argb4444_expands_nibbles_to_bytes() {
        // Pixel 0: A=F R=F G=0 B=0 -> opaque red. Pixel 1: A=8 R=4 G=2 B=1.
        let data = [0xFF00u16.to_le_bytes(), 0x8421u16.to_le_bytes()].concat();
        let img = decode(2, 2, 1, &data).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [68, 34, 17, 136]);
    }
}
