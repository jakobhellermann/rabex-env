mod utils;

use anyhow::Result;
use rabex::objects::pptr::PathId;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    // env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let bundle = "scenes_scenes_scenes/song_17.bundle";
    let path_id: PathId = -7545636390849209228;

    let file = env.load_addressables_bundle_content(bundle)?;
    let object = file.object_at::<serde_value::Value>(path_id)?;
    let item = object.read()?;

    println!("{:?}", object.class_id());
    println!("{}", object.object.typetree()?.dump());
    println!("{}", serde_json::to_string_pretty(&item)?);

    Ok(())
}
