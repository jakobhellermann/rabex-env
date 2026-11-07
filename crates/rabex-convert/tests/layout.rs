//! The layout rules against rabex's own reader and writer, on every object of the fixture
//! bundles: reading past an object lands where the reader ends, and copying it to a
//! position that is off by one to three bytes gives what the writer writes there.

use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::serde_typetree;
use rabex::typetree::{NullTypeTreeProvider, TypeTreeNode, TypetreeNodeKind};
use rabex_convert::layout::{Layout, Reader, copy};
use rabex_convert::value::Value;

const FIXTURES: &str = "../rabex-shader/tests/fixtures";

/// Whether `node` holds anything the reader cannot read.
fn unreadable(node: &TypeTreeNode) -> bool {
    matches!(node.classify(), TypetreeNodeKind::TodoReferenced)
        || node.children.iter().any(unreadable)
}

/// Every object of every fixture bundle, with the tree it reads with.
fn for_each_object(
    mut check: impl FnMut(&str, &TypeTreeNode, &[u8]) -> Result<()>,
) -> Result<usize> {
    let mut count = 0;
    for version in std::fs::read_dir(FIXTURES)? {
        let version = version?.path();
        for name in ["shaders", "textures", "sprites", "compute"] {
            let path = version.join(name);
            if !path.is_file() {
                continue;
            }
            let bundle = BundleFileReader::from_reader(
                Cursor::new(std::fs::read(&path)?),
                &ExtractionConfig::default(),
            )?;
            for entry in bundle.serialized_files() {
                let data = bundle.read_at_entry(entry)?;
                let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
                for info in file.objects() {
                    let tree = file.get_typetree_for(info, &NullTypeTreeProvider)?;
                    if unreadable(&tree) {
                        continue;
                    }
                    let start = usize::try_from(info.m_Offset)?;
                    let bytes = &data[start..start + info.m_Size as usize];
                    let label =
                        format!("{} {:?} {}", path.display(), info.m_ClassID, info.m_PathID);
                    check(&label, &tree, bytes).with_context(|| label.clone())?;
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

#[test]
fn skipping_an_object_reads_exactly_its_bytes() -> Result<()> {
    let count = for_each_object(|_, tree, bytes| {
        let mut reader = Reader::new(bytes);
        reader.skip(&Layout::of(tree))?;
        ensure!(
            reader.is_at_end(),
            "stopped at {} of {}",
            reader.pos(),
            bytes.len()
        );
        Ok(())
    })?;
    ensure!(count > 100, "only {count} objects in the fixtures");
    Ok(())
}

/// Shifted by `shift` bytes: the object as a field after that many single bytes.
fn shifted(tree: &TypeTreeNode, shift: u8) -> TypeTreeNode {
    let pads = (0..shift).map(|i| TypeTreeNode {
        m_Type: "UInt8".to_owned(),
        m_Name: format!("pad{i}"),
        ..Default::default()
    });
    TypeTreeNode {
        m_Type: "Shifted".to_owned(),
        m_Name: "Base".to_owned(),
        children: pads.chain([tree.clone()]).collect(),
        ..Default::default()
    }
}

/// What the writer lays out for `value` as a field after `shift` bytes.
fn written(tree: &TypeTreeNode, value: &Value, shift: u8) -> Result<Vec<u8>> {
    let fields: Vec<(Value, Value)> = (0..shift)
        .map(|i| (Value::String(format!("pad{i}")), Value::U8(0)))
        .chain([(Value::String(tree.m_Name.clone()), value.clone())])
        .collect();
    Ok(serde_typetree::to_vec::<_, byteorder::LittleEndian>(
        &Value::Map(fields.into_iter().collect()),
        &shifted(tree, shift),
    )?)
}

#[test]
fn copying_to_a_shifted_position_writes_what_the_writer_writes_there() -> Result<()> {
    let (mut checked, mut lossy) = (0, Vec::new());
    for_each_object(|label, tree, bytes| {
        let value: Value = serde_typetree::from_reader::<_, byteorder::LittleEndian>(
            &mut Cursor::new(bytes),
            tree,
        )?;
        // the writer is the reference only where the value carries everything: a map
        // value keeps one entry per key, and maps with a key twice lose the rest
        // TODO: the value path drops those entries on every re-encode; a map value that
        // keeps its pairs fixes that, and this check then covers every object.
        if written(tree, &value, 0)? != bytes {
            lossy.push(label.to_owned());
            return Ok(());
        }
        for shift in 1..4 {
            let mut out = vec![0; usize::from(shift)];
            copy(&Layout::of(tree), &mut Reader::new(bytes), &mut out)?;
            ensure!(
                out == written(tree, &value, shift)?,
                "differs shifted by {shift}"
            );
        }
        checked += 1;
        Ok(())
    })?;
    eprintln!(
        "{checked} objects checked, {} not representable as a value: {lossy:#?}",
        lossy.len()
    );
    ensure!(checked > 100, "only {checked} objects checked");
    Ok(())
}
