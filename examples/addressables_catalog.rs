mod utils;

use std::collections::BTreeMap;
use std::ops::AddAssign;
use std::path::Path;

use anyhow::Result;
use rabex_env::addressables::binary_catalog::resource_providers;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let addressables = env.addressables()?.unwrap();

    let mut bundle_names = BTreeMap::default();
    let mut bundled_asset_counts = BTreeMap::default();

    for mut catalog in addressables.catalogs(&env.game_files)? {
        let catalog = catalog.read()?;
        for loc in catalog.locations() {
            match loc.provider_id.as_str() {
                resource_providers::ASSET_BUNDLE => {
                    let abro = loc.data.as_ref().unwrap();
                    let path = addressables.evaluate_string(&loc.internal_id);
                    let path = Path::new(&path)
                        .strip_prefix(addressables.build_folder())
                        .unwrap();
                    bundle_names.insert((*abro.bundle_name).clone(), path.to_owned());
                }
                resource_providers::BUNDLED_ASSET => {
                    let class_name = loc.type_.m_ClassName.as_str();
                    let type_name = class_name.rsplit_once('.').map_or(class_name, |n| n.1);
                    bundled_asset_counts
                        .entry(type_name.to_owned())
                        .or_insert(0)
                        .add_assign(1);
                }
                _ => {
                    todo!()
                }
            }
        }
    }

    dbg!(bundled_asset_counts);
    dbg!(bundle_names);

    Ok(())
}
