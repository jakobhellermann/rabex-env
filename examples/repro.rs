mod utils;

use anyhow::Result;

fn main() -> Result<()> {
    let mut env = utils::find_game("silksong")?.unwrap();
    env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let file = env.load_cached("resources.assets")?;
    let obj = file.object_at::<serde_json::Value>(448)?;
    let obj = obj.read()?;

    dbg!(obj);

    Ok(())
}
