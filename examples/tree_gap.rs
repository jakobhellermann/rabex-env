//! Find where a tree misses a field: insert a 4 byte field — an `int`, or a `bool`
//! aligned to four — at every position of every struct in the tree and report the
//! positions at which every given file's objects of the class read exactly.
//!
//! usage: tree_gap <class> <tree version> <file>...

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex_convert::value::Value;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let class: ClassId = args
        .next()
        .context("class")?
        .parse()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let version: UnityVersion = args.next().context("tree version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let tree = tpk
        .get_typetree_node(class, &version)
        .context("no tree")?
        .into_owned();

    let mut objects = Vec::new();
    for path in args {
        let data = std::fs::read(&path)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(&data))?;
        for info in file.objects().filter(|info| info.m_ClassID == class) {
            let start = info.m_Offset as usize;
            objects.push((
                path.clone(),
                data[start..start + info.m_Size as usize].to_vec(),
            ));
        }
    }

    // every struct node, as the path of child indices to it
    fn structs(node: &TypeTreeNode, path: Vec<usize>, out: &mut Vec<Vec<usize>>) {
        use rabex::typetree::TypetreeNodeKind;
        match node.classify() {
            TypetreeNodeKind::Struct => out.push(path.clone()),
            TypetreeNodeKind::ArrayWrapper | TypetreeNodeKind::Array | TypetreeNodeKind::Map => {}
            _ => return,
        }
        for (i, child) in node.children.iter().enumerate() {
            let mut next = path.clone();
            next.push(i);
            structs(child, next, out);
        }
    }
    let mut paths = Vec::new();
    structs(&tree, Vec::new(), &mut paths);
    eprintln!("{} objects, {} structs to try", objects.len(), paths.len());

    let reads_exactly = |candidate: &TypeTreeNode| {
        objects.iter().all(|(_, bytes)| {
            let mut cursor = Cursor::new(bytes.as_slice());
            rabex::serde_typetree::from_reader::<Value, byteorder::LittleEndian>(
                &mut cursor,
                candidate,
            )
            .is_ok()
                && cursor.position() == bytes.len() as u64
        })
    };
    let gap = |ty: &str, align: bool| TypeTreeNode {
        m_Type: ty.into(),
        m_Name: "_gap".into(),
        m_ByteSize: if ty == "int" { 4 } else { 1 },
        m_MetaFlag: Some(if align { 0x4000 } else { 0 }),
        ..Default::default()
    };
    for path in &paths {
        let mut node = &tree;
        let mut name = String::new();
        for i in path {
            node = &node.children[*i];
            name = format!("{name}.{}", node.m_Name);
        }
        for at in 0..=node.children.len() {
            for (ty, align) in [("int", false), ("bool", true)] {
                let mut candidate = tree.clone();
                let mut target = &mut candidate;
                for i in path {
                    target = &mut target.children[*i];
                }
                target.children.insert(at, gap(ty, align));
                if reads_exactly(&candidate) {
                    let before = at.checked_sub(1).map(|i| node.children[i].m_Name.as_str());
                    let after = node.children.get(at).map(|c| c.m_Name.as_str());
                    println!(
                        "{ty}{} in {name:?}: after {before:?}, before {after:?}",
                        if align { " aligned" } else { "" }
                    );
                }
            }
        }
    }
    Ok(())
}
