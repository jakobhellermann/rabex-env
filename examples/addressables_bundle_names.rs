mod utils;

use std::collections::BTreeMap;

use anyhow::Result;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let aa = env.addressables()?.unwrap();

    let mut bundle_names = BTreeMap::default();

    for mut catalog in aa.catalogs()? {
        let locations = catalog.location_headers()?;
        for location in locations {
            let provider_id = location.provider_id(&mut catalog)?;
            if *provider_id
                != "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleProvider"
            {
                continue;
            }
            let internal_id = location.internal_id(&mut catalog)?;
            let abro = location
                .data(&mut catalog)?
                .expect("no data for AssetBundleProvider")
                .into_abro()
                .unwrap();
            let path = internal_id
                .strip_prefix("{UnityEngine.AddressableAssets.Addressables.RuntimePath}")
                .expect("expected RuntimePath placeholder in provider ID")
                .trim_start_matches(|x| x == '/' || x == '\\');

            bundle_names.insert(abro.bundle_name.clone(), path.to_owned());
        }
    }

    dbg!(bundle_names);

    Ok(())
}
