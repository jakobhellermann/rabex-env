//! Print the platforms of a bundle's shaders, and a game's.

mod utils;

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::{Platform, Shader};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: probe_platforms <bundle>")?;
    let data = std::fs::read(&path)?;
    let reader =
        BundleFileReader::from_reader(Cursor::new(data.as_slice()), &ExtractionConfig::default());
    let files: Vec<Vec<u8>> = match reader {
        Ok(reader) => reader
            .serialized_files()
            .map(|entry| reader.read_at_entry(entry).unwrap())
            .collect(),
        // not a bundle: the file itself
        Err(_) => vec![data],
    };
    {
        for data in files {
            let data = &data;
            let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
            for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
                let typetrees = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
                let shader: Shader = file
                    .read(info, &typetrees, &mut Cursor::new(data.as_slice()))
                    .with_context(|| format!("{:?}", info.m_PathID))?;
                let platforms: Vec<_> = shader
                    .platforms
                    .iter()
                    .map(|&p| format!("{:?}", Platform::from_id(p)))
                    .collect();
                println!("{}: {:?}", shader.m_ParsedForm.m_Name, platforms);
            }
        }
    }
    Ok(())
}
