//! Retarget every serialized file of a bundle on its own and list what fails, by message:
//! the gaps between two versions the rules do not cover yet. Each file stops at its first
//! failure, so a gap can hide another behind it in the same file.
//!
//! usage: retarget_gaps <bundle> <target version>
use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::files::serializedfile::{CommonOffsetMap, build_common_offset_map};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::retarget::{Report, Retarget};
use rabex_convert::rewriter::SerializedFileRewriter;
use rayon::prelude::*;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("bundle path")?;
    let to: UnityVersion = std::env::args().nth(2).context("target version")?.parse()?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common_map = build_common_offset_map(&tpk.inner, &to);
    let retarget = Retarget {
        to: to.clone(),
        typetrees: &tpk,
        shader_chunk_size: 8 << 20,
        common_strings: common_map.iter().map(|(s, o)| (*s, *o)).collect(),
        source_version: None,
        scripts: None,
    };
    let common: CommonOffsetMap = common_map.iter().map(|(s, o)| (*s, *o)).collect();
    let data = std::fs::read(&path).with_context(|| path.clone())?;
    let reader =
        BundleFileReader::from_reader(Cursor::new(data.as_slice()), &ExtractionConfig::default())?;
    let entries: Vec<_> = reader.serialized_files().collect();
    let results: Vec<(String, Result<Report>)> = entries
        .par_iter()
        .map(|entry| {
            let result = (|| -> Result<Report> {
                let bytes = reader.read_entry(entry)?;
                let mut rewriter = SerializedFileRewriter::new(&bytes, &tpk, &common)?;
                let report = retarget.apply(&mut rewriter)?;
                rewriter.finish()?;
                Ok(report)
            })();
            (entry.path.clone(), result)
        })
        .collect();
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = Report::default();
    let mut ok = 0;
    for (file, result) in results {
        match result {
            Ok(report) => {
                ok += 1;
                total.merge(report);
            }
            Err(error) => {
                // the object's path id varies, the class and the reason do not
                let message = format!("{error:#}");
                let key = message
                    .split_once(": ")
                    .map_or(message.clone(), |(head, rest)| {
                        let class = head.split(' ').next().unwrap_or(head);
                        format!("{class}: {rest}")
                    });
                gaps.entry(key).or_default().push(file);
            }
        }
    }
    println!(
        "{ok} of {} files retarget to {to}: {} objects re-encoded, {} carried, removed {:?}",
        entries.len(),
        total.reencoded,
        total.carried,
        total.removed
    );
    for (path, n) in &total.dropped {
        println!("  dropped {path} ({n})");
    }
    for (warning, n) in &total.warnings {
        println!("  warning: {warning} ({n})");
    }
    for (gap, files) in &gaps {
        println!(
            "gap in {} files: {}",
            files.len(),
            gap.chars().take(400).collect::<String>()
        );
    }
    Ok(())
}
