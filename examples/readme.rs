use anyhow::{Context, Result};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::Environment;
use rabex_env::unity::types::BuildSettings;

fn main() -> Result<()> {
    let game_path = std::env::args().nth(1).context("missing game path")?;

    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new_in(game_path, tpk)?;

    let version = env.unity_version()?;
    println!("Unity Version: {}", version);

    let ggm = env.globalgamemanagers()?;
    let build_settings = ggm.find_object_of::<BuildSettings>()?.unwrap();
    println!(
        "Scenes: {:?}",
        build_settings.scene_names().collect::<Vec<_>>()
    );

    // load and read components of a serialized file
    let level0 = env.load_serialized("level0")?;
    for transform in level0.transforms() {
        let transform = transform.read()?;
        if transform.m_Father.is_null() {
            let game_object = level0.deref_read(transform.m_GameObject)?;
            println!("- {}", game_object.m_Name)
        }
    }

    // load addressables bundles
    let addressables = env.addressables()?;
    if let Some(addressables) = addressables {
        for bundle in addressables.bundle_paths().take(10) {
            let file = env.load_addressables_bundle_content(bundle)?;
            println!(
                "{} contains {} objects",
                bundle.display(),
                file.objects::<()>().len()
            );
        }
    }

    Ok(())
}
