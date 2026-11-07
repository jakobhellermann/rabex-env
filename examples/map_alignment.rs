//! How Unity flags alignment around maps: for every `map` node in the embedded type tree
//! package (every class of every Unity version it holds), which of the map, its Array
//! and its pair carry the align flag.
use std::collections::BTreeMap;

use rabex::tpk::TpkTypeTreeBlob;

fn main() {
    let tpk = TpkTypeTreeBlob::embedded();
    let name = |i: u16| tpk.string_buffer[i as usize].as_str();
    let aligns = |i: u16| tpk.node_buffer[i as usize].meta_flag & 0x4000 != 0;
    let mut combos: BTreeMap<(bool, bool, bool), Vec<String>> = BTreeMap::new();
    for node in &tpk.node_buffer {
        if name(node.type_name) != "map" {
            continue;
        }
        let array = node.sub_nodes[0];
        let pair = tpk.node_buffer[array as usize].sub_nodes[1];
        let pair_node = &tpk.node_buffer[pair as usize];
        let key = (node.meta_flag & 0x4000 != 0, aligns(array), aligns(pair));
        let first = pair_node
            .sub_nodes
            .first()
            .map(|&k| name(tpk.node_buffer[k as usize].type_name))
            .unwrap_or("?");
        let second = pair_node
            .sub_nodes
            .get(1)
            .map(|&v| name(tpk.node_buffer[v as usize].type_name))
            .unwrap_or("?");
        combos
            .entry(key)
            .or_default()
            .push(format!("{} map<{first}, {second}>", name(node.name)));
    }
    for ((map, array, pair), maps) in &combos {
        println!(
            "map {map:5} Array {array:5} pair {pair:5}: {} maps, e.g. {:?}",
            maps.len(),
            &maps[..maps.len().min(4)]
        );
    }
}
