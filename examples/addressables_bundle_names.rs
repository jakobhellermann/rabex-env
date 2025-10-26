mod utils;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

const ASSET_BUNDLE_PROVIDER: &str =
    "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleProvider";

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let addressables = env.addressables()?.unwrap();

    let mut bundle_names = BTreeMap::default();

    for mut catalog in addressables.catalogs()? {
        let catalog = catalog.read()?;
        for location in catalog.locations_of_provider(ASSET_BUNDLE_PROVIDER) {
            let abro = location.data.as_ref().unwrap();
            let path = addressables.evaluate_string(&location.internal_id);
            let path = Path::new(&path)
                .strip_prefix(&addressables.build_folder())
                .unwrap();
            bundle_names.insert((*abro.bundle_name).clone(), path.to_owned());
        }
    }

    dbg!(bundle_names);

    Ok(())
}
