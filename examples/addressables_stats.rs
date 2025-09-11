mod utils;

use std::collections::BTreeMap;

use anyhow::Result;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let addressables = env.addressables()?.unwrap();
    let resources = addressables.resource_locations()?;

    let mut provider_counts = BTreeMap::default();

    let mut bundles = Vec::new();
    let mut assets = Vec::new();

    for res in resources {
        match res.provider_name() {
            "AssetBundleProvider" => bundles.push(addressables.evaluate_string(&res.internal_id)),
            "BundledAssetProvider" => assets.push(res.internal_id.clone()),
            _ => {}
        }

        *provider_counts
            .entry(res.provider_name().to_owned())
            .or_insert(0) += 1;
    }

    dbg!(assets);
    dbg!(bundles);
    dbg!(provider_counts);

    Ok(())
}
