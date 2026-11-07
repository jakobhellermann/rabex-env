//! Downport a Unity bundle through the retarget: shaders semantically, every other
//! engine class against the target's type trees. The result must parse as the target
//! version's data.

use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::files::serializedfile::build_common_offset_map;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::retarget::Retarget;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle path")?;
    let to: UnityVersion = std::env::args().nth(2).context("target version")?.parse()?;
    let chunk: usize = std::env::args()
        .nth(3)
        .context("shader chunk size in MB")?
        .parse()?;

    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &to);
    let retarget = Retarget {
        to: to.clone(),
        typetrees: &tpk,
        shader_chunk_size: chunk * 1024 * 1024,
        common_strings: common.iter().map(|(s, o)| (*s, *o)).collect(),
        source_version: None,
        scripts: None,
    };

    let data = std::fs::read(&path).with_context(|| path.clone())?;
    let converted = retarget
        .bundle(&data)?
        .with_context(|| format!("{path}: nothing to retarget"))?;

    // the result must parse as a bundle and every serialized file as the target version
    let bundle = BundleFileReader::from_reader(
        Cursor::new(converted.as_slice()),
        &ExtractionConfig::default(),
    )
    .context("the retargeted bundle doesn't parse")?;
    let (mut objects, mut shaders) = (0, 0);
    for entry in bundle.serialized_files() {
        let bytes = bundle.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(bytes.as_slice()))
            .with_context(|| entry.path.clone())?;
        ensure!(
            file.m_UnityVersion.as_ref().is_some_and(|v| *v == to),
            "{}: unity version is {}, not {to}",
            entry.path,
            file.m_UnityVersion
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
        for info in file.objects() {
            if info.m_ClassID == rabex::objects::ClassId::Shader {
                shaders += 1;
            }
        }
        objects += file.objects().count();
    }
    println!("{path}: {objects} objects parse as {to}, {shaders} of them shaders");
    Ok(())
}
