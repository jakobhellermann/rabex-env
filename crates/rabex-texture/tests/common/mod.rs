//! Bundles built by Unity, with what Unity reports about their contents. The fixtures and their
//! generator live in a separate Unity project (`RabexFixtures.cs`), one bundle and manifest per group.

#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::objects::pptr::PathId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_texture::{RgbaImage, Texture2D};
use serde::de::DeserializeOwned;

/// One fixture group of one Unity version.
pub struct Group<M> {
    pub dir: PathBuf,
    pub manifest: M,
    bundle: BundleFileReader<Cursor<Vec<u8>>>,
    file: SerializedFile,
    data: Vec<u8>,
}

/// The fixture groups named `name`, one per Unity version.
pub fn groups<M: DeserializeOwned>(name: &str) -> Result<Vec<Group<M>>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<_>>()?;
    dirs.sort();
    dirs.iter().map(|dir| Group::load(dir, name)).collect()
}

impl<M: DeserializeOwned> Group<M> {
    fn load(dir: &Path, name: &str) -> Result<Self> {
        let manifest_path = dir.join(format!("{name}.json"));
        let manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        let bundle = BundleFileReader::from_reader(
            Cursor::new(std::fs::read(dir.join(name))?),
            &ExtractionConfig::default(),
        )?;
        let entry = bundle
            .main_serializedfile()
            .context("bundle without serialized file")?;
        let data = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        Ok(Group {
            dir: dir.to_owned(),
            manifest,
            bundle,
            file,
            data,
        })
    }
}

impl<M> Group<M> {
    /// Every object of `class`, by path id.
    pub fn objects<T: DeserializeOwned>(&self, class: ClassId) -> Result<Vec<(PathId, T)>> {
        self.file
            .objects()
            .filter(|o| o.m_ClassID == class)
            .map(|info| Ok((info.m_PathID, self.read(info.m_PathID)?)))
            .collect()
    }

    pub fn read<T: DeserializeOwned>(&self, path_id: PathId) -> Result<T> {
        let info = self
            .file
            .get_object_info(path_id)
            .with_context(|| format!("no object {path_id}"))?;
        Ok(self.file.read(
            info,
            NullTypeTreeProvider,
            &mut Cursor::new(self.data.as_slice()),
        )?)
    }

    /// The texture's pixel data, inline or from the bundle's `.resS`.
    pub fn pixels(&self, texture: &Texture2D) -> Result<Vec<u8>> {
        let pixels = texture.pixels(|stream| {
            let file = stream
                .path
                .strip_prefix("archive:/")
                .and_then(|p| p.split_once('/'))
                .map(|(_, file)| file)
                .with_context(|| format!("stream outside the bundle: {}", stream.path))?;
            let data = self
                .bundle
                .read_at(file)?
                .with_context(|| format!("{file} not in bundle"))?;
            let range = stream.offset as usize..stream.offset as usize + stream.size as usize;
            Ok(data.get(range).context("stream out of range")?.to_vec())
        })?;
        Ok(pixels.into_owned())
    }

    /// An image Unity wrote, relative to the manifest.
    pub fn image(&self, path: &str) -> Result<RgbaImage> {
        Ok(image::open(self.dir.join(path))?.to_rgba8())
    }
}

/// Number of differing pixels and the largest difference per channel.
pub fn diff(actual: &RgbaImage, expected: &RgbaImage) -> (usize, [u8; 4]) {
    assert_eq!(actual.dimensions(), expected.dimensions());
    let mut differing = 0;
    let mut max = [0u8; 4];
    for (a, e) in actual.pixels().zip(expected.pixels()) {
        if a != e {
            differing += 1;
        }
        for c in 0..4 {
            max[c] = max[c].max(a.0[c].abs_diff(e.0[c]));
        }
    }
    (differing, max)
}
