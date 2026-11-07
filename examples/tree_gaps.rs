//! Every field a retarget from one version to another has no source for: the fields the
//! target's trees want that the source's lack, or hold in another shape, with no rule in
//! `fields.rs`, for the classes
//! of the given bundles' serialized files. All of them at once, where a retarget stops at the
//! first.
//!
//! usage: tree_gaps <from> <to> <bundle>...
use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider, TypetreeNodeKind as Kind};
use rabex_convert::layout;
use rabex_convert::value::layout_equal;

fn gaps(source: &TypeTreeNode, target: &TypeTreeNode, path: &str, out: &mut Vec<String>) {
    if layout_equal(source, target) {
        return;
    }
    match (source.classify(), target.classify()) {
        (Kind::Struct, Kind::Struct) => {
            for to in &target.children {
                let field = format!("{path}.{}", to.m_Name);
                if rabex_convert::fields::rule(source, target, &to.m_Name).is_some() {
                    continue;
                }
                match source.children.iter().find(|c| c.m_Name == to.m_Name) {
                    Some(from) => gaps(from, to, &field, out),
                    None => out.push(format!("{field} ({})", to.m_Type)),
                }
            }
        }
        (Kind::ArrayWrapper, Kind::ArrayWrapper) => {
            if let (Some(a), Some(b)) = (layout::element(source), layout::element(target)) {
                gaps(a, b, &format!("{path}[]"), out);
            }
        }
        (Kind::Map, Kind::Map) => {
            if let (Some((ka, va)), Some((kb, vb))) = (layout::pair(source), layout::pair(target)) {
                gaps(ka, kb, &format!("{path}[].first"), out);
                gaps(va, vb, &format!("{path}[].second"), out);
            }
        }
        (from, to) if shape(&from) != shape(&to) => {
            out.push(format!(
                "{path}: {} in the source, {} in the target",
                source.m_Type, target.m_Type
            ));
        }
        _ => {}
    }
}

/// What a node is as far as the plan converts between them: numbers convert into numbers.
fn shape(kind: &Kind) -> &'static str {
    match kind {
        Kind::Struct => "struct",
        Kind::ArrayWrapper | Kind::Array => "array",
        Kind::Map => "map",
        Kind::String => "string",
        Kind::Untyped => "bytes",
        Kind::Float | Kind::Double => "float",
        _ => "number",
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let from: UnityVersion = args.next().context("from")?.parse()?;
    let to: UnityVersion = args.next().context("to")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut classes = BTreeSet::new();
    for path in args {
        let config = ExtractionConfig {
            fallback_unity_version: FallbackUnityVersion::Exact(from.clone()),
            ..Default::default()
        };
        let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(&path)?), &config)?;
        for entry in reader.serialized_files() {
            let bytes = reader.read_entry(entry)?;
            let file = SerializedFile::from_reader(&mut Cursor::new(bytes.as_ref()))?;
            classes.extend(file.m_Types.iter().map(|t| t.m_ClassID));
        }
    }
    for class in classes {
        let (Some(source), Some(target)) = (
            tpk.get_typetree_node(class, &from),
            tpk.get_typetree_node(class, &to),
        ) else {
            continue;
        };
        let mut out = Vec::new();
        gaps(&source, &target, &format!("{class:?}"), &mut out);
        for gap in out {
            println!("{gap}");
        }
    }
    Ok(())
}
