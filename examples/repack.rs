mod utils;

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Cursor};

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileBuilder, BundleFileReader};
use rabex::files::serializedfile::build_common_offset_map;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::objects::pptr::FileId;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::env::bundle_main_serializedfile;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::prune::prune_scene_handle;
use rabex_env::unity::types::{AssetBundle, PreloadData};
use rustc_hash::FxHashMap;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let tpk = TypeTreeCache::embedded();
    let unity_version = env.unity_version()?;

    let load = [
        /*(
            "scenes_scenes_scenes/song_03.bundle",
            ["Song Pilgrim 03", "Song Reed", "Pilgrim 01 Song"].as_slice(),
        ),
        (
            "scenes_scenes_scenes/shellwood_01.bundle",
            ["Pond Skater (2)", "Shellwood Goomba"].as_slice(),
        ),*/
        (
            "scenes_scenes_scenes/under_13.bundle",
            [
                // "Pilgrim 03 Understore",
                // "Understore Automaton",
                "Understore Automaton EX",
            ]
            .as_slice(),
        ),
    ];

    let scenes = load
        .into_iter()
        .map(|(bundle_path, object_paths)| {
            let bundle = env.load_addressables_bundle(bundle_path)?;

            let (name, main, _) = bundle_main_serializedfile(&bundle)?;
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

            let _asset_bundle = shared
                .find_object_of::<AssetBundle>(&env.tpk)
                .context("expected AssetBundle in scene assetbundle")?
                .read(&mut Cursor::new(shared_data.as_slice()))?;

            #[cfg(debug_assertions)]
            {
                use rustc_hash::FxHashSet;
                let externals_main = main.externals_paths().collect::<FxHashSet<_>>();
                let externals_shared = shared.externals_paths().collect::<FxHashSet<_>>();
                assert_eq!(externals_main, externals_shared);
            }

            let remap_externals = remap_externals(&shared, &main);
            preload_data
                .m_Assets
                .iter_mut()
                .for_each(|x| x.m_FileID = remap_externals[&x.m_FileID]);

            let (name, mut main, data, preload_data) =
                extract_assetbundle_main_and_preloads(&bundle, &env.tpk)?;
            main.m_UnityVersion.get_or_insert(unity_version.clone());

            Ok((name, (main, data), preload_data, object_paths))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut bundle_builder = BundleFileBuilder::unityfs(8, unity_version);
    let com = build_common_offset_map(&tpk.inner, unity_version);

    let mut asset_bundle = AssetBundle::scene_base("repack_bundle");
    for (scene_hash, _, preloads, _) in &scenes {
        asset_bundle
            .m_Dependencies
            .extend_from_slice(&preloads.m_Dependencies);
        asset_bundle.add_scene(
            &format!("Assets/RepackScenes/thescene{scene_hash}.unity"),
            &scene_hash,
        );
    }
    let mut asset_bundle = Some(asset_bundle);

    for (scene_hash, main, preload_data, object_paths) in scenes {
        let main = SerializedFileHandle::new(&env, &main.0, &main.1);

        {
            let mut file_builder =
                SerializedFileBuilder::from_serialized_meta(unity_version, &main.file, &tpk, &com);
            file_builder.copy_externals(&main.file);

            file_builder.add_object_at(1, &preload_data)?;
            if let Some(asset_bundle) = asset_bundle.take() {
                file_builder.add_object_at(2, &asset_bundle)?;
            }

            let out = file_builder.write_vec()?;
            bundle_builder.add_file(&format!("{scene_hash}.sharedAssets"), out.as_slice())?;
        }

        {
            let mut replacements = FxHashMap::default();
            let prune = prune_scene_handle(
                main.reborrow(),
                object_paths.iter().copied(),
                &mut replacements,
                false,
            )?;

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

            let out = file_builder.write_vec()?;
            bundle_builder.add_file(&scene_hash, out.as_slice())?;
        }
    }

    bundle_builder.write(
        BufWriter::new(File::create(
            "C:/Users/Jakob/Documents/dev/unity/unity-scene-repacker/out/silksong.bundle",
        )?),
        rabex::files::bundlefile::CompressionType::Lzma,
    )?;
    println!("Done");

    Ok(())
}

fn extract_assetbundle_main_and_preloads<'a, P, T>(
    bundle: &BundleFileReader<Cursor<T>>,
    tpk: &P,
) -> Result<(String, SerializedFile, Vec<u8>, PreloadData)>
where
    P: TypeTreeProvider,
    T: AsRef<[u8]>,
{
    let (name, main, data) = rabex_env::env::bundle_main_serializedfile(&bundle)?;

    let shared_data = bundle
        .read_at(&format!("{name}.sharedAssets"))?
        .with_context(|| format!("expected {}.sharedAssets in bundle", name))?;
    let shared = SerializedFile::from_reader(&mut Cursor::new(&shared_data))?;
    ensure!(
        shared.objects().len() == 2,
        "expected exactly 2 objects in scene asset bundle"
    );
    let mut preload_data = shared
        .find_object_of::<PreloadData>(tpk)
        .context("expected PreloadData in scene assetbundle")?
        .read(&mut Cursor::new(shared_data.as_slice()))?;

    let asset_bundle = shared
        .find_object_of::<AssetBundle>(tpk)
        .context("expected AssetBundle in scene assetbundle")?
        .read(&mut Cursor::new(shared_data.as_slice()))?;

    #[cfg(debug_assertions)]
    {
        use rustc_hash::FxHashSet;
        let externals_main = main.externals_paths().collect::<FxHashSet<_>>();
        let externals_shared = shared.externals_paths().collect::<FxHashSet<_>>();
        assert_eq!(externals_main, externals_shared);

        assert_eq!(preload_data.m_Dependencies, asset_bundle.m_Dependencies);
    }

    let remap_externals = remap_externals(&shared, &main);
    preload_data
        .m_Assets
        .iter_mut()
        .for_each(|x| x.m_FileID = remap_externals[&x.m_FileID]);

    Ok((name, main, data, preload_data))
}

fn remap_externals(from: &SerializedFile, to: &SerializedFile) -> FxHashMap<FileId, FileId> {
    let index_in_main: FxHashMap<_, _> = to
        .externals_paths()
        .enumerate()
        .map(|(main_i, path)| (path, FileId::from_externals_index(main_i)))
        .collect();
    from.externals_paths()
        .enumerate()
        .map(|(meta_i, path)| (FileId::from_externals_index(meta_i), index_in_main[path]))
        .collect()
}
