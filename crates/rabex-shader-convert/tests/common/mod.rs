#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::Shader;

/// The fixture directory of every Unity version.
pub fn fixture_dirs() -> Result<Vec<PathBuf>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rabex-shader/tests/fixtures");
    let mut dirs = std::fs::read_dir(&root)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<Vec<_>>>()?;
    dirs.sort();
    Ok(dirs)
}

pub fn shaders(path: &Path) -> Result<Vec<Shader>> {
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(path).with_context(|| path.display().to_string())?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle
        .main_serializedfile()
        .context("bundle without serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    file.objects()
        .filter(|o| o.m_ClassID == ClassId::Shader)
        .map(|info| {
            Ok(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?)
        })
        .collect()
}
