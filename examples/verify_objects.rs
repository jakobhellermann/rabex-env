//! Read every object of a bundle against the type tree the file ships, and report the
//! ones that do not consume exactly their own bytes.
//!
//! A player reads objects strictly: it starts at the object's offset and expects the
//! tree to describe exactly the bytes up to the next one. Reading short leaves garbage
//! for the following field, reading long runs off the end of the file — which is the
//! "is corrupted! Remove it and launch unity again" the player reports.

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::typetree::NullTypeTreeProvider;
use rabex_convert::value::Value;

fn count(node: &rabex::typetree::TypeTreeNode) -> usize {
    1 + node.children.iter().map(count).sum::<usize>()
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle path")?;
    let version: UnityVersion = std::env::args()
        .nth(2)
        .context("the version the bundle is built with")?
        .parse()?;
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;

    let mut checked = 0;
    let mut bad = 0;
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)?;
        for info in file.objects() {
            let tree = file.get_typetree_for(info, &NullTypeTreeProvider)?;
            if std::env::var("TPK").is_ok()
                && info.m_ClassID == rabex::objects::ClassId::ParticleSystemRenderer
            {
                use rabex::typetree::TypeTreeProvider;
                let tpk = rabex::typetree::typetree_cache::sync::TypeTreeCache::new(
                    rabex::tpk::TpkTypeTreeBlob::embedded(),
                );
                let reference = tpk.get_typetree_node(info.m_ClassID, &version).unwrap();
                let mut c = Cursor::new(data.as_slice());
                c.set_position(info.m_Offset as u64);
                let r: Result<Value, _> = rabex::serde_typetree::from_reader_endianed(
                    &mut c,
                    &reference,
                    file.m_Header.m_Endianess,
                );
                println!(
                    "  with the tpk tree: {} (read {} of {})",
                    if r.is_ok() { "ok" } else { "error" },
                    c.position() - info.m_Offset as u64,
                    info.m_Size
                );
                let embedded = count(&tree);
                println!(
                    "  embedded tree {embedded} nodes, tpk tree {} nodes",
                    count(&reference)
                );
                break;
            }
            if std::env::var("DUMP").ok().as_deref() == Some(&format!("{}", info.m_PathID)) {
                let start = info.m_Offset as usize;
                let end = start + info.m_Size as usize;
                println!(
                    "{:?} {} bytes {:02x?}",
                    info.m_ClassID,
                    info.m_Size,
                    &data[start..end.min(start + 48)]
                );
            }
            checked += 1;
            // the object offsets are already relative to the file's data
            let start = info.m_Offset as u64;
            let mut cursor = Cursor::new(data.as_slice());
            cursor.set_position(start);
            let read: Result<Value, _> = rabex::serde_typetree::from_reader_endianed(
                &mut cursor,
                &tree,
                file.m_Header.m_Endianess,
            );
            match read {
                Err(error) => {
                    bad += 1;
                    println!("{:?} {}: {error}", info.m_ClassID, info.m_PathID);
                }
                Ok(_) => {
                    let used = cursor.position() - start;
                    if used != info.m_Size as u64 {
                        bad += 1;
                        println!(
                            "{:?} {} (type entry {}): read {used} bytes of {}",
                            info.m_ClassID, info.m_PathID, info.m_TypeID, info.m_Size
                        );
                    }
                }
            }
        }
    }
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &version)?;
        let mut per_class: std::collections::BTreeMap<_, std::collections::BTreeSet<_>> =
            Default::default();
        for info in file.objects() {
            per_class
                .entry(info.m_ClassID)
                .or_default()
                .insert(info.m_TypeID);
        }
        for (class, types) in per_class.iter().filter(|(_, types)| types.len() > 1) {
            println!("{class:?} spread over type entries {types:?}");
        }
        if std::env::var("TYPES").is_ok() {
            for (index, ty) in file.m_Types.iter().enumerate() {
                let fields = ty.m_Type.as_ref().map_or(0, count);
                println!("  type {index}: {:?} {} fields", ty.m_ClassID, fields);
            }
        }
    }
    println!("{checked} objects checked, {bad} that do not fit their bytes");
    let _ = NullTypeTreeProvider;
    std::process::exit(if bad > 0 { 1 } else { 0 });
}
