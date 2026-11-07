//! Decode every object of Unity-written files with its type tree and check the decoder
//! ends on the object's last byte. Unity's writer is the authority on the layout, so a
//! decoder that skips or adds an alignment — a mistake the encoder might share, and
//! that reading back our own output can then never show — ends elsewhere here.
//!
//! usage: decode_exact <version for bundles without one> <file or bundle>...

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::Value;

#[derive(Default)]
struct Tally {
    exact: usize,
    off: Vec<String>,
    failed: Vec<String>,
}

fn check(
    name: &str,
    data: &[u8],
    file: &SerializedFile,
    tpk: &TypeTreeCache<TpkTypeTreeBlob>,
    tally: &mut Tally,
) {
    for info in file.objects() {
        // script data has no tree without the game's assemblies
        if info.m_ClassID == ClassId::MonoBehaviour && !file.m_EnableTypeTree {
            continue;
        }
        let Ok(tree) = file.get_typetree_for(info, tpk) else {
            continue;
        };
        let start = info.m_Offset as usize;
        let bytes = &data[start..start + info.m_Size as usize];
        let mut cursor = Cursor::new(bytes);
        match rabex::serde_typetree::from_reader::<Value, byteorder::LittleEndian>(
            &mut cursor,
            &tree,
        ) {
            Ok(_) if cursor.position() == bytes.len() as u64 => tally.exact += 1,
            Ok(_) => tally.off.push(format!(
                "{name}: {:?} {} read {} of {} bytes",
                info.m_ClassID,
                info.m_PathID,
                cursor.position(),
                bytes.len()
            )),
            Err(error) => tally.failed.push(format!(
                "{name}: {:?} {}: {error}",
                info.m_ClassID, info.m_PathID
            )),
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let fallback: UnityVersion = args.next().context("version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut tally = Tally::default();
    for path in args {
        let data = std::fs::read(&path).with_context(|| path.clone())?;
        if data.starts_with(b"UnityFS") {
            let config = ExtractionConfig {
                fallback_unity_version: FallbackUnityVersion::Exact(fallback.clone()),
                ..Default::default()
            };
            let reader = BundleFileReader::from_reader(Cursor::new(&data), &config)?;
            for entry in reader.serialized_files() {
                let bytes = reader.read_at_entry(entry)?;
                let file =
                    SerializedFile::from_reader_with_version(&mut Cursor::new(&bytes), &fallback)?;
                check(&entry.path, &bytes, &file, &tpk, &mut tally);
            }
        } else {
            let file = SerializedFile::from_reader(&mut Cursor::new(&data))?;
            check(&path, &data, &file, &tpk, &mut tally);
        }
    }
    println!(
        "{} exact, {} ended elsewhere, {} failed",
        tally.exact,
        tally.off.len(),
        tally.failed.len()
    );
    for line in tally.off.iter().chain(&tally.failed).take(30) {
        println!("  {line}");
    }
    Ok(())
}
