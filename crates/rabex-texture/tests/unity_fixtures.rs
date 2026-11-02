//! Decode textures from bundles built by Unity and compare against the pixels Unity reports for
//! them. The fixtures and their generator live in a separate Unity project (`RabexFixtures.cs`).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_texture::{RgbaImage, Texture2D};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Manifest {
    unity_version: String,
    bundle: String,
    textures: Vec<TextureEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TextureEntry {
    name: String,
    requested_format: String,
    format: String,
    format_id: i32,
    width: u32,
    height: u32,
    mip_count: i32,
    readable: bool,
    expected: String,
    raw_data_length: usize,
}

struct Fixture {
    dir: PathBuf,
    manifest: Manifest,
    bundle: BundleFileReader<Cursor<Vec<u8>>>,
    textures: BTreeMap<String, Texture2D>,
}

impl Fixture {
    fn load(dir: &Path) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(dir.join("textures.json"))?)?;
        let bundle_data = std::fs::read(dir.join(&manifest.bundle))?;
        let bundle =
            BundleFileReader::from_reader(Cursor::new(bundle_data), &ExtractionConfig::default())?;
        let entry = bundle
            .main_serializedfile()
            .context("bundle without serialized file")?;
        let data = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        let mut textures = BTreeMap::new();
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Texture2D) {
            let texture: Texture2D = file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?;
            textures.insert(texture.m_Name.clone(), texture);
        }
        Ok(Fixture {
            dir: dir.to_owned(),
            manifest,
            bundle,
            textures,
        })
    }

    fn pixels(&self, texture: &Texture2D) -> Result<Vec<u8>> {
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

    fn expected(&self, entry: &TextureEntry) -> Result<RgbaImage> {
        Ok(image::open(self.dir.join(&entry.expected))?.to_rgba8())
    }
}

fn fixture_dirs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    dirs
}

/// Number of differing pixels and the largest difference per channel.
fn diff(actual: &RgbaImage, expected: &RgbaImage) -> (usize, [u8; 4]) {
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

/// What decoding a texture of this (actual) format is expected to yield.
enum Expect {
    Exact,
    /// Alpha off by at most this much: texture2ddecoder truncates interpolated BC3 alpha, Unity rounds.
    AlphaWithin(u8),
    Unsupported,
}

fn expectation(format: &str) -> Expect {
    match format {
        "DXT5" => Expect::AlphaWithin(1),
        "DXT1Crunched" => Expect::Unsupported,
        _ => Expect::Exact,
    }
}

/// `Ok(Err(..))` describes how `decoded` misses the expectation for the entry's format.
fn check(
    entry: &TextureEntry,
    decoded: Result<RgbaImage>,
    expected: impl FnOnce() -> Result<RgbaImage>,
) -> Result<Result<(), String>> {
    let expect = expectation(&entry.format);
    let img = match (&expect, decoded) {
        (Expect::Unsupported, Err(e)) => {
            let message = format!("unsupported Texture2D format {}", entry.format_id);
            return Ok(match e.to_string() == message {
                true => Ok(()),
                false => Err(format!("unexpected error: {e}")),
            });
        }
        (Expect::Unsupported, Ok(_)) => return Ok(Err("expected to be unsupported".into())),
        (_, Err(e)) => return Ok(Err(e.to_string())),
        (_, Ok(img)) => img,
    };
    let (differing, max) = diff(&img, &expected()?);
    let ok = match expect {
        Expect::Exact => differing == 0,
        Expect::AlphaWithin(tolerance) => max[..3] == [0; 3] && max[3] <= tolerance,
        Expect::Unsupported => unreachable!(),
    };
    Ok(match ok {
        true => Ok(()),
        false => Err(format!("{differing} px differ, max per channel {max:?}")),
    })
}

#[test]
fn decoded_textures_match_unity() -> Result<()> {
    let mut failures = Vec::new();
    for dir in fixture_dirs() {
        let fixture = Fixture::load(&dir)?;
        let version = &fixture.manifest.unity_version;
        let mut inline_pixels = BTreeMap::new();
        for entry in &fixture.manifest.textures {
            let texture = fixture
                .textures
                .get(&entry.name)
                .with_context(|| format!("{} missing from bundle", entry.name))?;
            ensure!(
                (
                    texture.m_Width as u32,
                    texture.m_Height as u32,
                    texture.m_TextureFormat
                ) == (entry.width, entry.height, entry.format_id),
                "{version} {}: serialized fields disagree with Unity",
                entry.name
            );
            let pixels = fixture.pixels(texture)?;
            if let Some(base) = entry.name.strip_suffix("_inline") {
                inline_pixels.insert(base.to_owned(), pixels.clone());
            } else if let Some(base) = entry.name.strip_suffix("_stream") {
                ensure!(
                    inline_pixels.get(base) == Some(&pixels),
                    "{version} {}: streamed pixels differ from the inline variant",
                    entry.name
                );
            }

            if let Err(failure) = check(entry, texture.decode(&pixels), || fixture.expected(entry))?
            {
                failures.push(format!("{version} {}: {failure}", entry.name));
            }
        }
    }
    assert_eq!(failures, Vec::<String>::new());
    Ok(())
}
