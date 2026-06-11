mod utils;

use std::io::Cursor;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};
use rabex::files::SerializedFile;
use rabex::objects::pptr::PathId;
use rabex_env::Environment;
use rabex_env::addressables::{AddressablesData, ArchivePath};
use rayon::iter::ParallelBridge as _;
use rustc_hash::FxHashMap;

/// Find every object that references a given target object, whether the reference is local
/// (same file) or external (from another bundle).
///
/// First builds an externals index (which file lists which others in its `m_Externals`), so that
/// only the files that can possibly reference the target have to be loaded and scanned.
///
/// Usage: `cargo run --example find_references [<bundle>] [<path_id>]`
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let target_bundle = args
        .next()
        .unwrap_or_else(|| "scenes_scenes_scenes/tut_01.bundle".to_owned());
    let target_path_id = match args.next() {
        Some(arg) => arg.parse().context("path_id must be an integer")?,
        None => 671,
    };

    let env = utils::find_game("silksong")?.unwrap();
    let addressables = env.addressables()?.context("game has no addressables")?;

    // The archive path under which other files refer to the target's main serialized file.
    let target_archive_path = addressables
        .bundle_main_archive_path(Path::new(&target_bundle))
        .with_context(|| format!("unknown bundle '{target_bundle}'"))?
        .to_string();

    let start = Instant::now();
    let index = build_externals_index(&env)?;
    println!(
        "built externals index ({} referenced files) in {:?}",
        index.len(),
        start.elapsed()
    );

    let candidates = candidate_files(&index, &target_archive_path);
    println!("scanning {} candidate file(s)", candidates.len());
    let start = Instant::now();
    let referrers = find_referrers(&env, candidates, &target_archive_path, target_path_id)?;
    println!("scanned candidates in {:?}", start.elapsed());

    println!(
        "\n{} reference(s) to {target_bundle}#{target_path_id}:",
        referrers.len()
    );
    for (archive_path, path_id) in &referrers {
        println!("- {} #{path_id}", bundle_of(addressables, archive_path)?);
    }

    std::mem::forget(env);
    Ok(())
}

/// Build `external archive path -> [files that list it in their m_Externals]`.
///
/// Only each serialized file's header is parsed; no objects are deserialized and nothing is kept
/// in the env cache, so this stays cheap and low-memory (and could be persisted for reuse).
fn build_externals_index(env: &Environment) -> Result<FxHashMap<String, Vec<String>>> {
    let pairs = rabex_env::utils::par_fold_reduce(
        env.addressables_bundles()?.into_iter().par_bridge(),
        |acc: &mut Vec<(String, String)>, bundle_path| {
            let bundle = env.load_addressables_bundle(&bundle_path)?;
            let bundle_id = bundle
                .serialized_files()
                .find_map(|f| {
                    Path::new(&f.path)
                        .extension()
                        .is_none()
                        .then(|| f.path.clone())
                })
                .context("bundle has no main serialized file")?;
            for entry in bundle.serialized_files() {
                let archive_path = ArchivePath::new(&bundle_id, &entry.path).to_string();
                let data = bundle.read_at_entry(entry)?;
                let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
                for external in file.externals_paths() {
                    acc.push((external.to_owned(), archive_path.clone()));
                }
            }
            Ok(())
        },
    )?;

    let mut index: FxHashMap<String, Vec<String>> = FxHashMap::default();
    for (external, referrer) in pairs {
        index.entry(external).or_default().push(referrer);
    }
    Ok(index)
}

/// Files that can possibly reference the target: those listing it in their externals (for external
/// references), plus the target file itself (for local references).
fn candidate_files(
    index: &FxHashMap<String, Vec<String>>,
    target_archive_path: &str,
) -> Vec<String> {
    // Each serialized file appears at most once per external (m_Externals has no duplicates), and a
    // file never lists itself, so these candidates are already unique.
    let mut candidates = index.get(target_archive_path).cloned().unwrap_or_default();
    candidates.push(target_archive_path.to_owned());
    candidates
}

/// Load and scan only the candidate files, collecting every object whose PPtrs reach the target.
fn find_referrers(
    env: &Environment,
    candidates: Vec<String>,
    target_archive_path: &str,
    target_path_id: PathId,
) -> Result<Vec<(String, PathId)>> {
    rabex_env::utils::par_fold_reduce(
        candidates.into_iter().par_bridge(),
        |acc: &mut Vec<(String, PathId)>, candidate| {
            let file = env.load_serialized(&candidate)?;
            for object in file.objects::<()>() {
                for pptr in object.reachable_one()? {
                    let referenced_file = match pptr.is_local() {
                        true => candidate.as_str(),
                        false => pptr.file_identifier(&file.file).unwrap().pathName.as_str(),
                    };
                    if referenced_file == target_archive_path && pptr.m_PathID == target_path_id {
                        acc.push((candidate.clone(), object.path_id()));
                    }
                }
            }
            Ok(())
        },
    )
}

/// Resolve an archive path back to its human-readable bundle name.
fn bundle_of(addressables: &AddressablesData, archive_path: &str) -> Result<String> {
    Ok(match ArchivePath::try_parse(Path::new(archive_path))? {
        Some(ap) => addressables.cab_to_bundle[ap.bundle].display().to_string(),
        None => archive_path.to_owned(),
    })
}
