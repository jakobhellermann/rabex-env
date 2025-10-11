mod utils;

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Cursor};

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::BundleFileBuilder;
use rabex::files::serializedfile::build_common_offset_map;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::objects::pptr::FileId;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::env::bundle_main_serializedfile;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::prune::prune_scene_handle;
use rabex_env::unity::types::{AssetBundle, AssetInfo, PreloadData};
use rustc_hash::FxHashMap;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let tpk = TypeTreeCache::embedded();
    let unity_version = env.unity_version()?;

    let load = [
        (
            "scenes_scenes_scenes/song_03.bundle",
            ["Song Pilgrim 03", "Song Reed", "Pilgrim 01 Song"].as_slice(),
        ),
        /*(
            "scenes_scenes_scenes/shellwood_01.bundle",
            ["Pond Skater (2)", "Shellwood Goomba"].as_slice(),
        ),*/
    ];

    let scenes = load
        .into_iter()
        .map(|(bundle_path, object_paths)| {
            let bundle = env.load_addressables_bundle(bundle_path)?;

            let (name, mut main, data) = bundle_main_serializedfile(&bundle)?;
            let shared_data = bundle
                .read_at(&format!("{name}.sharedAssets"))?
                .with_context(|| format!("expected {}.sharedAssets in bundle", name))?;
            let shared = SerializedFile::from_reader(&mut Cursor::new(&shared_data))?;
            ensure!(
                shared.objects().len() == 2,
                "expected exactly 2 objects in scene asset bundle"
            );
            let mut preload_data = shared
                .find_object_of::<PreloadData>(&env.tpk)
                .context("expected PreloadData in scene assetbundle")?
                .read(&mut Cursor::new(shared_data.as_slice()))?;

            let asset_bundle = shared
                .find_object_of::<AssetBundle>(&env.tpk)
                .context("expected AssetBundle in scene assetbundle")?
                .read(&mut Cursor::new(shared_data.as_slice()))?;

            // make preloads reference the main file externals order, not the shared one
            let remap_externals = remap_externals(&shared, &main);
            preload_data
                .m_Assets
                .iter_mut()
                .for_each(|x| x.m_FileID = remap_externals[&x.m_FileID]);

            main.m_UnityVersion.get_or_insert(unity_version.clone());

            let mut replacements = FxHashMap::default();
            let prune = prune_scene_handle(
                SerializedFileHandle::new(&env, &main, &data),
                object_paths.iter().copied(),
                &mut replacements,
                false,
            )?;

            Ok((
                name,
                (main, data),
                (preload_data, asset_bundle),
                object_paths,
                prune,
                replacements,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut bundle_builder = BundleFileBuilder::unityfs(8, unity_version);
    let com = build_common_offset_map(&tpk.inner, unity_version);

    // Build `AssetBundle`
    let mut asset_bundle = AssetBundle::asset_base("repack_bundle");
    for (_, _, (scene_preloads, existing_bundle), _, prune, ..) in &scenes {
        asset_bundle
            .m_Dependencies
            .extend_from_slice(&existing_bundle.m_Dependencies);

        let all_preloads = asset_bundle.add_preloads(scene_preloads.m_Assets.iter().cloned());

        for (path, root) in &prune.roots {
            let go = root.m_GameObject;
            // let reachable = main.deref(go)?.reachable()?;
            // let preloads = reachable.0.into_iter().map(PPtr::local).chain(reachable.1);
            // let range = asset_bundle.add_preloads(preloads);

            asset_bundle.m_Container.insert(
                format!("Assets/Prefabs/{}.prefab", path),
                AssetInfo::with_preloads(all_preloads.clone(), go.untyped()),
            );
        }
    }
    let mut asset_bundle = Some(asset_bundle);

    for (name, main, _, _, prune, replacements) in scenes {
        let main = SerializedFileHandle::new(&env, &main.0, &main.1);

        let mut file_builder = SerializedFileBuilder::from_serialized(
            unity_version,
            &main.file,
            &main.data,
            &tpk,
            &com,
            main.file
                .objects()
                .filter(|obj| prune.reachable.contains(&obj.m_PathID))
                .cloned(),
        );

        for (path_id, replacement) in replacements {
            file_builder.objects.get_mut(&path_id).unwrap().1 = Cow::Owned(replacement);
        }

        file_builder.objects.remove(&1); // TODO
        file_builder.add_object_at(1, &asset_bundle.take().unwrap())?;

        let out = file_builder.write_vec()?;
        bundle_builder.add_file(&name, out.as_slice())?;
    }

    bundle_builder.write(
        BufWriter::new(File::create("out.bundle")?),
        rabex::files::bundlefile::CompressionType::Lzma,
    )?;
    println!("Done");

    Ok(())
}

fn remap_externals(from: &SerializedFile, to: &SerializedFile) -> FxHashMap<FileId, FileId> {
    let index_in_main: FxHashMap<_, _> = to
        .externals_paths()
        .enumerate()
        .map(|(main_i, path)| (path, FileId::from_externals_index(main_i + 1)))
        .collect();
    from.externals_paths()
        .enumerate()
        .map(|(meta_i, path)| {
            (
                FileId::from_externals_index(meta_i + 1),
                index_in_main[path],
            )
        })
        .collect()
}
