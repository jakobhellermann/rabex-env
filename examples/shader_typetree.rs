//! Dump the full Shader typetree of a Unity version as an indented field list.

use rabex::UnityVersion;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeNode;

fn print_tree(node: &TypeTreeNode, depth: usize) {
    println!("{}{} {}", "  ".repeat(depth), node.m_Type, node.m_Name);
    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

fn main() {
    let version: UnityVersion = std::env::args()
        .nth(1)
        .expect("usage: shader_typetree <version>")
        .parse()
        .unwrap();
    let tpk = TpkTypeTreeBlob::embedded();
    let Some(node) = tpk.get_typetree_node(ClassId::Shader, &version) else {
        eprintln!("no Shader typetree for {version}");
        return;
    };
    print_tree(&node, 0);
}
