mod utils;

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Result;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    // env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let start = Instant::now();
    let catalog = env.addressables_catalog()?.unwrap();
    dbg!(start.elapsed());

    for (_key, locations) in &catalog.resources {
        for loc in locations {
            let provider_id = loc.provider_id.rsplit_once('.').unwrap().1;
            match provider_id {
                "AssetBundleProvider" => {
                    let full_path = Path::new(&*loc.internal_id);

                    assert!(
                        full_path.starts_with(
                            "{UnityEngine.AddressableAssets.Addressables.RuntimePath}"
                        )
                    );

                    let _path = full_path.components().skip(2).collect::<PathBuf>();
                    let _bundle_name = &loc.data.as_ref().unwrap().bundle_name;
                    // println!("  {}={}", bundle_name, path.display());
                }
                "BundledAssetProvider" => {
                    let _full_path = Path::new(&*loc.internal_id);
                    dbg!(loc.dependencies.iter().count());
                    for dep in &loc.dependencies {
                        let x = catalog
                            .resources
                            .iter()
                            .flat_map(|x| x.1.as_slice())
                            .find(|x| x.internal_id == dep.internal_id)
                            .unwrap();

                        assert_eq!(x, dep);
                        // dbg!(&dep.internal_id);
                    }
                    break;
                }
                _ => todo!("{}", provider_id),
            }
        }
    }

    Ok(())
}
