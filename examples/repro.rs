mod utils;

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Result;

fn main() -> Result<()> {
    let mut env = utils::find_game("silksong")?.unwrap();
    // env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let start = Instant::now();
    let catalog = env.addressables_catalog()?.unwrap();
    dbg!(start.elapsed());

    for (key, locations) in catalog.resources {
        for loc in locations {
            let provider_id = loc.provider_id.rsplit_once('.').unwrap().1;
            match provider_id {
                "AssetBundleProvider" => {
                    let path = loc
                        .internal_id
                        .strip_prefix(
                            "{UnityEngine.AddressableAssets.Addressables.RuntimePath}/StandaloneLinux64/",
                        )
                        .unwrap();
                    let bundle_name = loc.data.unwrap().bundle_name;
                    println!("  {}={}", bundle_name, path);
                }
                "BundledAssetProvider" => continue,
                _ => todo!("{}", provider_id),
            }
        }
    }

    Ok(())
}
