mod utils;

use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};
use dashmap::DashMap;
use rabex::objects::pptr::PathId;
use rabex_env::Environment;
use rabex_env::addressables::{AddressablesData, ArchivePath};
use rabex_env::handle::SerializedFileHandle;
use rabex_env::resolver::EnvResolver;
use rayon::iter::{IntoParallelRefIterator, ParallelBridge as _, ParallelIterator};
use rustc_hash::FxHashMap;

// #[global_allocator]
// static ALLOC: dhat::Alloc = dhat::Alloc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
struct GlobalPPtr {
    global_file_id: u32,
    path_id: PathId,
}

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let addressables = env.addressables()?.unwrap();

    let (global_files, global_file_map) = global_external_numbering(addressables);

    let start = Instant::now();
    env.load_all_serialized_files()?;
    println!("loaded all files in {:?}", start.elapsed());

    let bundles = [
        "dataassets_assets_assets/dataassets/questsystem/quests.bundle",
        "fsmtemplates_assets_shared.bundle",
        "localpoolprefabs_assets_areapeak.bundle",
        "scenes_scenes_scenes/mosstown_03.bundle",
        "scenes_scenes_scenes/song_01.bundle",
        "sfxstatic_assets_areacogarealibrary.bundle",
        "tk2danimations_assets_water.bundle",
    ];
    let bundles = env.addressables_bundles();
    for bundle in bundles {
        // let bundle = Path::new(bundle);
        let bundle = bundle
            .strip_prefix(env.game_files.base_dir().join(addressables.build_folder()))
            .unwrap();
        let target_archive_path = addressables
            .bundle_main_archive_path(bundle)
            .unwrap()
            .to_string();

        let target_global_file_id = global_file_map
            .iter()
            .find_map(|(key, val)| (*key == target_archive_path).then_some(*val))
            .unwrap();

        let start = Instant::now();
        let referencing_files = find_referencing_files(&env, bundle)?;

        let references = DashMap::new();
        referencing_files.par_iter().try_for_each(|(path, file)| {
            find_pptr_references(
                &references,
                &global_file_map,
                &file,
                path,
                target_global_file_id,
            )
        })?;

        println!("{}", bundle.display());
        println!("- {} object's references computed", references.len());
        println!(
            "- {} total references",
            references
                .iter()
                .map(|item| item.value().len())
                .sum::<usize>()
        );
        println!("- {} files referencing the bundle", referencing_files.len());
        for (path, _) in referencing_files {
            let archive_path = ArchivePath::try_parse(Path::new(&path))?.unwrap();
            let _bundle_path = &addressables.cab_to_bundle[archive_path.bundle];
            // println!("  - {}", _bundle_path.display());
        }
        println!("  Scan took {:?}", start.elapsed());

        if false {
            for (source, references) in references {
                let global_file = Path::new(&global_files[source.global_file_id as usize]);
                let bundle_path = ArchivePath::try_parse(global_file)?
                    .map(|external| addressables.cab_to_bundle[external.bundle].as_path())
                    .unwrap_or(global_file);
                println!("- {} @ {}", source.path_id, bundle_path.display());

                for reference in references {
                    let global_file = Path::new(&global_files[reference.global_file_id as usize]);
                    let bundle_path = ArchivePath::try_parse(global_file)?
                        .map(|external| addressables.cab_to_bundle[external.bundle].as_path())
                        .unwrap_or(global_file);
                    println!("  by {} @ {}", reference.path_id, bundle_path.display());
                }
            }
        }

        println!();
    }

    std::mem::forget(env);
    Ok(())
}

fn global_external_numbering(
    addressables: &AddressablesData,
) -> (Vec<String>, FxHashMap<String, u32>) {
    let files = addressables.cab_to_bundle.keys().map(|cab| {
        let base = Path::new(cab).file_stem().unwrap().to_str().unwrap();
        ArchivePath::new(base, cab).to_string()
    });
    let global_files: Vec<_> = ["Library/unity default resources".to_owned()]
        .into_iter()
        .chain(files)
        .collect();
    let global_file_map: FxHashMap<_, _> = global_files
        .iter()
        .enumerate()
        .map(|(i, file)| (file.clone(), i as u32))
        .collect();

    (global_files, global_file_map)
}

/// Find all files ('archive:/a/b' and content) that reference the given target bundle
fn find_referencing_files<'a>(
    env: &'a Environment,
    target_bundle: &Path,
) -> Result<Vec<(String, SerializedFileHandle<'a>)>, anyhow::Error> {
    let Some(addressables) = env.addressables()? else {
        return Ok(Vec::new());
    };

    let target_archive_path = addressables
        .bundle_main_archive_path(target_bundle)
        .unwrap();

    let mut referencing_files = rabex_env::utils::par_fold_reduce(
        env.addressables_bundles().par_bridge(),
        |acc: &mut Vec<_>, bundle_path| {
            let bundle_path = bundle_path
                .strip_prefix(env.game_files.game_dir.join(addressables.build_folder()))
                .unwrap();
            let archive_path = addressables
                .bundle_main_archive_path(bundle_path)
                .unwrap()
                .to_string();
            let file = env.load_addressables_bundle_content(&bundle_path)?;
            if file
                .file
                .externals_paths()
                .any(|path| path == target_archive_path.to_string())
            {
                acc.push((archive_path, file));
            }
            Ok(())
        },
    )?;
    referencing_files.push({
        let archive_path = addressables
            .bundle_main_archive_path(target_bundle)
            .unwrap();
        let file = env.load_addressables_bundle_content(&target_bundle)?;

        (archive_path.to_string(), file)
    });
    Ok(referencing_files)
}

/// Go through all objects, and for each referenced PPtr record the object as one of its references
fn find_pptr_references(
    out: &DashMap<GlobalPPtr, Vec<GlobalPPtr>>,
    global_file_map: &FxHashMap<String, u32>,
    file: &SerializedFileHandle,
    archive_path: &str,
    target_global_file_id: u32,
) -> Result<()> {
    let global_file_id = global_file_map[archive_path];

    for object in file.objects::<()>() {
        let reference_pptr = GlobalPPtr {
            global_file_id,
            path_id: object.path_id(),
        };

        let pptrs = object.reachable_one()?;

        for pptr in pptrs {
            let global_file_id = match pptr.is_local() {
                true => global_file_id,
                false => {
                    let external = &pptr.file_identifier(&file.file).unwrap();
                    *global_file_map
                        .get(external.pathName.as_str())
                        .context(external.pathName.clone())
                        .unwrap()
                }
            };

            if global_file_id != target_global_file_id {
                continue;
            }

            let source_pptr = GlobalPPtr {
                global_file_id,
                path_id: pptr.m_PathID,
            };
            out.entry(source_pptr).or_default().push(reference_pptr);
        }
    }

    Ok(())
}
