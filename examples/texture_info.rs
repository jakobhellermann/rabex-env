//! Print every texture of a bundle with the fields that decide what the player uploads,
//! and a checksum of the pixel data it points at — so a converted bundle can be held
//! against its source.

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_convert::value::{Value, field};

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle")?;
    let version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;

    // the streamed pixels live beside the serialized file, in the bundle's resource file
    let resource = reader
        .files()
        .iter()
        .find(|entry| entry.path.ends_with(".resS"))
        .map(|entry| reader.read_at_entry(entry))
        .transpose()?;

    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Texture2D) {
            let texture: Value = file.read(info, &NullTypeTreeProvider, &mut Cursor::new(&data))?;
            let get = |name: &str| field(&texture, name).cloned();
            let number = |name: &str| match get(name) {
                Some(Value::I32(v)) => v as i64,
                Some(Value::U32(v)) => v as i64,
                Some(Value::I64(v)) => v,
                Some(Value::U64(v)) => v as i64,
                _ => -1,
            };
            let name = match get("m_Name") {
                Some(Value::String(name)) => name,
                _ => String::new(),
            };
            let stream = get("m_StreamData");
            let (offset, size) = match &stream {
                Some(value) => (
                    match field(value, "offset") {
                        Some(Value::U64(v)) => *v,
                        Some(Value::U32(v)) => *v as u64,
                        _ => 0,
                    },
                    match field(value, "size") {
                        Some(Value::U32(v)) => *v as u64,
                        Some(Value::U64(v)) => *v,
                        _ => 0,
                    },
                ),
                None => (0, 0),
            };
            let pixels = match (&resource, size) {
                (Some(resource), size) if size > 0 => {
                    let end = (offset + size) as usize;
                    match resource.get(offset as usize..end) {
                        Some(bytes) => format!("{:016x}", checksum(bytes)),
                        None => "PAST THE END OF THE RESOURCE".to_owned(),
                    }
                }
                _ => "inline".to_owned(),
            };
            println!(
                "{name}: {}x{} format {} mips {} complete {} stream {offset}+{size} pixels {pixels}",
                number("m_Width"),
                number("m_Height"),
                number("m_TextureFormat"),
                number("m_MipCount"),
                number("m_CompleteImageSize"),
            );
        }
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
