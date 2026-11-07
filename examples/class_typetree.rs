//! Print a class's type tree for a Unity version, as field paths.

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;

fn main() -> Result<()> {
    let class: ClassId = std::env::args().nth(1).context("class name")?.parse()?;
    let version: UnityVersion = std::env::args().nth(2).context("version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let tree = tpk
        .get_typetree_node(class, &version)
        .with_context(|| format!("no type tree of {class:?} for {version}"))?;
    let mut path = Vec::new();
    walk(&tree, &mut path);
    Ok(())
}

fn walk(node: &rabex::typetree::TypeTreeNode, path: &mut Vec<String>) {
    path.push(node.m_Name.clone());
    println!(
        "{} {}{}",
        path.join("."),
        node.m_Type,
        if node.requires_align() {
            " [align]"
        } else {
            ""
        }
    );
    for child in &node.children {
        walk(child, path);
    }
    path.pop();
}
