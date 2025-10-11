mod utils;

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::BundleFileReader;
use rabex::objects::{PPtr, TypedPPtr};
use rabex::typetree::TypeTreeProvider;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::unity::types::{AssetBundle, GameObject, PreloadData};

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let bundle = env.load_addressables_bundle("scenes_scenes_scenes/under_13.bundle")?;
    let (name, file, data, preload) = extract_assetbundle_main_and_preloads(&bundle, &env.tpk)?;
    let file = SerializedFileHandle::new(&env, &file, &data);

    for (i, preload_relative) in preload.preload_assets_relative.iter().enumerate() {
        let path = &preload.externals[preload_relative.m_FileID.get_externals_index().unwrap()];
        let external = env.load_external_file(Path::new(path))?;

        println!("--- {i} {name} ---");
        let preload = external.object_at::<InfoBase>(preload_relative.m_PathID)?;

        let info = preload.read()?;

        println!(
            "  {i} {:?} '{}' @ {path}",
            preload.class_id(),
            info.m_Name.unwrap_or_default()
        );
    }

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code, non_snake_case)]
struct InfoBase {
    m_Name: Option<String>,
    m_GameObject: Option<TypedPPtr<GameObject>>,
}

pub struct PreloadInfo {
    // these two are from the `PreloadData` object in the scene .sharedAssets
    preload_assets_relative: Vec<PPtr>, // m_Assets from PreloadData. !! file id is relative to externals !!
    preload_dependencies: Vec<String>,  // m_Dependencies from PreloadData
    // the externals of the .sharedAssets (which are usually the same as the main file (in a different order), but not always
    externals: Vec<String>,
}

fn extract_assetbundle_main_and_preloads<'a, P, T>(
    bundle: &BundleFileReader<Cursor<T>>,
    tpk: &P,
) -> Result<(String, SerializedFile, Vec<u8>, PreloadInfo)>
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
    let preload_data = shared
        .find_object_of::<PreloadData>(tpk)
        .context("expected PreloadData in scene assetbundle")?
        .read(&mut Cursor::new(shared_data.as_slice()))?;

    let asset_bundle = shared
        .find_object_of::<AssetBundle>(tpk)
        .context("expected AssetBundle in scene assetbundle")?
        .read(&mut Cursor::new(shared_data.as_slice()))?;

    #[cfg(debug_assertions)]
    {
        assert_eq!(preload_data.m_Dependencies, asset_bundle.m_Dependencies);
        assert_eq!(preload_data.m_Dependencies, asset_bundle.m_Dependencies);
    }

    let preload_info = PreloadInfo {
        preload_assets_relative: preload_data.m_Assets,
        preload_dependencies: preload_data.m_Dependencies,
        externals: shared.m_Externals.into_iter().map(|x| x.pathName).collect(),
    };

    Ok((name, main, data, preload_info))
}
