mod utils;

use anyhow::Result;
use rayon::iter::ParallelBridge;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let addressables = env.addressables()?.unwrap();

    let item = addressables
        .cab_to_bundle
        .keys()
        .find(|key| key.contains("9f1c78"))
        .unwrap();
    dbg!(item);

    rabex_env::utils::par_fold_reduce::<(), _>(
        env.addressables_bundles().par_bridge(),
        |(), path| {
            let bundle = env.load_addressables_bundle_content(&path)?;

            if bundle
                .file
                .externals_paths()
                .any(|path| path.contains(item))
            {
                let name = path.file_name().unwrap().to_str().unwrap();
                dbg!(name);
            }
            Ok(())
        },
    )?;

    Ok(())
}
