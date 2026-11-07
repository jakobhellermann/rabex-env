//! Mapping the serialized files of asset bundles.

use std::borrow::Cow;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::bundlefile::builder::BundleFileBuilder;
use rabex::files::bundlefile::{
    BundleFileReader, CompressionType, ExtractionConfig, FallbackUnityVersion,
};
use rabex::files::serializedfile::CommonOffsetMap;
use rabex::typetree::TypeTreeProvider;

use crate::rewriter::SerializedFileRewriter;

/// What a bundle rewrite cannot read off the bundle itself, and what it writes
/// differently. Addressables builds leave the header's revision empty and write `0.0.0`
/// into their serialized files, so both ends of the version have to be told.
#[derive(Default, Clone)]
pub struct Options {
    /// What to read the bundle as when its header names no revision.
    pub source: Option<UnityVersion>,
    /// What revision the rewritten bundle's header names. A rewrite that retargets the
    /// files has to say so here: the player refuses an archive whose header names a
    /// version it does not know, whatever the files inside say.
    pub written: Option<UnityVersion>,
    /// The compression of the rewritten bundle's data; without, the source's.
    pub compression: Option<CompressionType>,
}

/// The newest UnityFS container version a build of `unity` reads.
///
/// Measured on built games: Blasphemous (2017.4) ships v6, Hollow Knight (2020.2) v7,
/// A Short Hike (2021.3), Nine Sols (2022.3) and Silksong (6000.0) v8 — but Hollow
/// Knight's own 6000.0 build ships v7, so what a build *writes* is not the engine
/// version alone. What matters here is only what the target still reads, and a player
/// reads the containers older than its own.
///
/// TODO: 7 for everything below 2021.3 is the cautious edge of the measurements, not a
/// measurement: no build between 2020.3 and 2021.2 was at hand to say where v8 starts.
/// Reading a 2020.2 bundle with a 2021.x player would settle it.
fn container_version(unity: &UnityVersion) -> u32 {
    match unity >= &UnityVersion::new(2021, 3, 0) {
        true => 8,
        false => 7,
    }
}

/// The compression a rewritten archive takes where its source used `source`: the same,
/// except that LZ4HC, LZ4's format compressed harder, is written as LZ4 — a player
/// decodes both alike, and the hard compressor would take most of a rewrite's time.
pub fn written_compression(source: CompressionType) -> CompressionType {
    match source {
        CompressionType::Lz4hc => CompressionType::Lz4,
        other => other,
    }
}

/// Parse `bundle`, hand every serialized file of it to `rule` as a
/// [`SerializedFileRewriter`], and write the bundle again, `None` if no rule changed
/// anything. Files other than serialized files carry over unchanged.
pub fn rewrite_bundle<P: TypeTreeProvider>(
    bundle: &[u8],
    typetrees: &P,
    common_strings: &CommonOffsetMap,
    versions: &Options,
    mut rule: impl FnMut(&str, &mut SerializedFileRewriter<'_, P>) -> Result<()>,
) -> Result<Option<Vec<u8>>> {
    let mut config = ExtractionConfig::default();
    if let Some(version) = &versions.source {
        config.fallback_unity_version = FallbackUnityVersion::Exact(version.clone());
    }
    let reader = BundleFileReader::from_reader(Cursor::new(bundle), &config)?;
    let mut files = Vec::new();
    let mut changed = false;
    for entry in reader.files() {
        let data = reader.read_entry(entry)?;
        let serialized = reader.serialized_files().any(|s| s.path == entry.path);
        let rewritten = match serialized {
            true => {
                let mut rewriter = SerializedFileRewriter::new_in_bundle(
                    &data,
                    typetrees,
                    common_strings,
                    versions.source.as_ref(),
                )?;
                rule(&entry.path, &mut rewriter).with_context(|| entry.path.clone())?;
                rewriter.finish()?
            }
            false => None,
        };
        changed |= rewritten.is_some();
        files.push((entry, rewritten.map(Cow::Owned).unwrap_or(data)));
    }
    if !changed {
        return Ok(None);
    }

    let header = reader.header();
    // the engine the archive is written for decides its layout; a header without revision
    // (Addressables write `0.0.0`) stays without one unless a retarget names the version
    let named = versions
        .written
        .clone()
        .or_else(|| header.unity_revision.clone());
    let engine = named
        .clone()
        .or_else(|| versions.source.clone())
        .context("bundle without Unity revision, and none given to read or write it as")?;
    let container = match &versions.written {
        Some(version) => container_version(version).min(header.version),
        None => header.version,
    };
    let mut out = BundleFileBuilder::unityfs(container, &engine);
    if named.is_none() {
        out = out.without_revision();
    }
    for (entry, data) in files {
        out.add_file_with_flags(&entry.path, data.as_ref(), entry.flags)?;
    }
    let mut written = Cursor::new(Vec::new());
    let compression = versions
        .compression
        .unwrap_or_else(|| written_compression(reader.compression()));
    out.write(&mut written, compression)?;
    Ok(Some(written.into_inner()))
}
