//! Prints the in-place compatibility patches of every bundle in a directory, as
//! `bundle<TAB>offset<TAB>hex bytes`, and on stderr the rules that couldn't be applied.
//!
//! `cargo run --release -p rabex-convert --example compat_patches -- <bundle dir> <source version> <target version>`
use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::compat::bundle_patches;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: compat_patches <bundle dir> <source version> <target version>";
    let dir = args.next().context(usage)?;
    let source: UnityVersion = args.next().context(usage)?.parse()?;
    let target: UnityVersion = args.next().context(usage)?.parse()?;
    let typetrees = TypeTreeCache::embedded();

    let mut paths: Vec<_> = std::fs::read_dir(&dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "bundle"));
    paths.sort();

    let start = Instant::now();
    let mut unfixable: BTreeMap<String, usize> = BTreeMap::new();
    let (mut patches, mut patched_bundles) = (0, 0);
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy();
        let bundle = unsafe { memmap2::Mmap::map(&std::fs::File::open(path)?)? };
        let result = bundle_patches(&bundle, &source, &target, &typetrees)
            .with_context(|| name.to_string())?;
        for patch in &result.patches {
            let hex: String = patch.bytes.iter().map(|b| format!("{b:02x}")).collect();
            println!("{name}\t{}\t{hex}", patch.offset);
        }
        patches += result.patches.len();
        patched_bundles += !result.patches.is_empty() as usize;
        for rule in result.unfixable {
            *unfixable.entry(rule).or_default() += 1;
        }
    }
    eprintln!(
        "{} bundles, {patched_bundles} patched, {patches} patches, {:?}",
        paths.len(),
        start.elapsed()
    );
    for (rule, bundles) in unfixable {
        eprintln!("unfixable in {bundles} bundles: {rule}");
    }
    Ok(())
}
