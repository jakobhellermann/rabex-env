//! Print fields of the objects of one class in a loose serialized file — the evidence
//! for what a version's own build writes into a field another version lacks.
//!
//! usage: object_fields <file> <class> [<field>...] — without fields, all their names

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::Value;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().context("file")?;
    let class = args.next().context("class")?;
    let fields: Vec<String> = args.collect();

    let data = std::fs::read(&path).with_context(|| path.clone())?;
    let file = SerializedFile::from_reader(&mut Cursor::new(&data))?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    println!("{path}: Unity {:?}", file.m_UnityVersion);
    for info in file
        .objects()
        .filter(|info| format!("{:?}", info.m_ClassID) == class)
    {
        let value: Value = file.read(info, &tpk, &mut Cursor::new(data.as_slice()))?;
        let Value::Map(map) = &value else {
            anyhow::bail!("{class} {} is not a map", info.m_PathID);
        };
        println!("{class} {}", info.m_PathID);
        if fields.is_empty() {
            // no fields asked for: the names of all of them
            for (key, _) in map.iter() {
                if let Value::String(name) = key {
                    println!("  {name}");
                }
            }
        }
        for field in &fields {
            let found = map
                .iter()
                .find(|(key, _)| matches!(key, Value::String(name) if name == field));
            match found {
                Some((_, value)) => println!("  {field} = {value:?}"),
                None => println!("  {field}: not in this version's tree"),
            }
        }
    }
    Ok(())
}
