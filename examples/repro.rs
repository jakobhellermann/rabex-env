mod utils;

use anyhow::Result;

fn main() -> Result<()> {
    let mut env = utils::find_game("silksong")?.unwrap();
    env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let file = env.load_addressables_bundle_content("heroloading_assets_all.bundle")?;
    dbg!(
        file.scripts::<serde_json::Value>("PlayMakerFSM")?
            .map(|x| x.read())
            .collect::<Result<Vec<_>>>()?
    );
    // let obj = file.object_at::<serde_json::Value>(448)?;
    // let obj = obj.read()?;

    Ok(())
}
