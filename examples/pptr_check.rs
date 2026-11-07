//! Compare the object references of two versions of a bundle: every PPtr of every
//! object, by path id, so a conversion that quietly repointed something shows up.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::typetree::{NullTypeTreeProvider, TypeTreeNode};
use rabex_convert::value::Value;

fn main() -> Result<()> {
    let (a, a_version, b, b_version) = (
        std::env::args().nth(1).context("bundle")?,
        std::env::args().nth(2).context("version")?,
        std::env::args().nth(3).context("other bundle")?,
        std::env::args().nth(4).context("other version")?,
    );
    let left = pointers(&a, &a_version.parse()?)?;
    let right = pointers(&b, &b_version.parse()?)?;

    let mut differences = 0;
    for (object, left_pointers) in &left {
        match right.get(object) {
            None => {
                println!("{object}: gone");
                differences += 1;
            }
            Some(right_pointers) if right_pointers != left_pointers => {
                println!("{object}:\n  {left_pointers:?}\n  {right_pointers:?}");
                differences += 1;
            }
            Some(_) => {}
        }
    }
    for object in right.keys() {
        if !left.contains_key(object) {
            println!("{object}: new");
            differences += 1;
        }
    }
    println!(
        "{} objects compared, {differences} with different references",
        left.len()
    );
    std::process::exit(if differences > 0 { 1 } else { 0 });
}

/// Every object's references, keyed by `class pathid`.
fn pointers(path: &str, version: &UnityVersion) -> Result<BTreeMap<String, Vec<(i32, i64)>>> {
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(version.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(path)?), &config)?;
    let mut all = BTreeMap::new();
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), version)?;
        for info in file.objects() {
            let tree = file.get_typetree_for(info, &NullTypeTreeProvider)?;
            let mut cursor = Cursor::new(data.as_slice());
            cursor.set_position(info.m_Offset as u64);
            let Ok(value) = rabex::serde_typetree::from_reader_endianed::<Value>(
                &mut cursor,
                &tree,
                file.m_Header.m_Endianess,
            ) else {
                continue;
            };
            let mut found = Vec::new();
            collect(&tree, &value, &mut found);
            all.insert(format!("{:?} {}", info.m_ClassID, info.m_PathID), found);
        }
    }
    Ok(all)
}

/// Walk value and tree together, picking up the PPtr structs.
fn collect(tree: &TypeTreeNode, value: &Value, found: &mut Vec<(i32, i64)>) {
    if tree.m_Type.starts_with("PPtr<") {
        let number = |name: &str| match rabex_convert::value::field(value, name) {
            Some(Value::I32(v)) => *v as i64,
            Some(Value::I64(v)) => *v,
            _ => 0,
        };
        found.push((number("m_FileID") as i32, number("m_PathID")));
        return;
    }
    match value {
        Value::Map(fields) => {
            for (key, child_value) in fields.iter() {
                let Value::String(name) = key else { continue };
                if let Some(child) = tree.children.iter().find(|c| c.m_Name == *name) {
                    collect(child, child_value, found);
                }
            }
        }
        Value::Seq(values) => {
            if let Some(element) = rabex_convert::layout::element(tree) {
                for value in values {
                    collect(element, value, found);
                }
            }
        }
        _ => {}
    }
}
