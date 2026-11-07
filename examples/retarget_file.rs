//! Retarget loose serialized files (a player's `globalgamemanagers`, `levelN`,
//! `*.assets`) and report per file what the retarget did, or how it failed. Every file
//! is attempted: the point is the full list of what breaks.
//!
//! usage: retarget_file <target version> [--out <dir>] <file>...
//!
//! With `--out`, every retargeted file is written there under its own name.

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::serializedfile::build_common_offset_map;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_convert::retarget::Retarget;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let to: UnityVersion = args.next().context("target version")?.parse()?;
    let mut args = args.peekable();
    let out = match args.peek().map(String::as_str) {
        Some("--out") => {
            args.next();
            Some(std::path::PathBuf::from(
                args.next().context("--out <dir>")?,
            ))
        }
        _ => None,
    };
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let common = build_common_offset_map(&tpk.inner, &to);
    let retarget = Retarget {
        to: to.clone(),
        typetrees: &tpk,
        shader_chunk_size: 8 * 1024 * 1024,
        common_strings: common.iter().map(|(s, o)| (*s, *o)).collect(),
        source_version: None,
        scripts: None,
    };
    let mut failed = 0;
    for path in args {
        let data = std::fs::read(&path).with_context(|| path.clone())?;
        match retarget.serialized_file_reported(&data) {
            Ok((written, report)) => {
                if let (Some(dir), Some(bytes)) = (&out, &written) {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .context("file name")?;
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(dir.join(name), bytes)?;
                }
                println!(
                    "{path}: {} re-encoded, {} carried, {}",
                    report.reencoded,
                    report.carried,
                    match written {
                        Some(bytes) => format!("{} bytes written", bytes.len()),
                        None => "unchanged".into(),
                    }
                );
                for (class, objects) in &report.removed {
                    println!("    removed {objects} {class}, a class the target lacks");
                }
                for (field, objects) in &report.dropped {
                    println!("    dropped {field} ({objects} objects)");
                }
                for (warning, objects) in &report.warnings {
                    println!("    warning: {warning} ({objects} objects)");
                }
            }
            Err(error) => {
                failed += 1;
                println!("{path}: FAILED {error:#}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} files failed");
    Ok(())
}
