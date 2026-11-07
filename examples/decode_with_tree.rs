//! Decode the objects of one class in a loose file with another version's tree, minus
//! named top-level fields, and report whether the decoder ends on the object's last byte
//! — how to find which tree a patch release really wrote when the type tree package
//! holds an older one.
//!
//! usage: decode_with_tree <file> <class> <tree version> [-<field to leave out>]...
//!        [<field to print>]...

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::value::{Value, field};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().context("file")?;
    let class: ClassId = args
        .next()
        .context("class")?
        .parse()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let version: UnityVersion = args.next().context("tree version")?.parse()?;
    let (without, print): (Vec<String>, Vec<String>) = args.partition(|a| a.starts_with('-'));

    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let mut tree = tpk
        .get_typetree_node(class, &version)
        .context("no tree")?
        .into_owned();
    for name in &without {
        let name = &name[1..];
        let before = tree.children.len();
        tree.children.retain(|child| child.m_Name != name);
        anyhow::ensure!(tree.children.len() < before, "{name} is not in the tree");
    }

    let data = std::fs::read(&path)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(&data))?;
    for info in file.objects().filter(|info| info.m_ClassID == class) {
        let start = info.m_Offset as usize;
        let bytes = &data[start..start + info.m_Size as usize];
        let mut cursor = Cursor::new(bytes);
        let value: Value =
            rabex::serde_typetree::from_reader::<_, byteorder::LittleEndian>(&mut cursor, &tree)?;
        println!(
            "{class:?} {}: read {} of {} bytes",
            info.m_PathID,
            cursor.position(),
            bytes.len()
        );
        // the bytes around where the tree ran out, to see what the decoder did not read
        let end = cursor.position() as usize;
        println!(
            "  last 40 read: {:02x?}\n  unread: {:02x?}",
            &bytes[end.saturating_sub(40)..end],
            &bytes[end..]
        );
        for name in &print {
            println!("  {name} = {:?}", field(&value, name));
        }
    }
    Ok(())
}
