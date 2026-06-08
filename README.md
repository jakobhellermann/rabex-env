# rabex-env

[![Latest Version]][crates.io] [![Docs]][docs.rs] [![License_MIT]][license_mit] [![License_APACHE]][license_apache] 

[Latest Version]: https://img.shields.io/crates/v/rabex-env.svg
[crates.io]: https://crates.io/crates/rabex-env
[Docs]: https://docs.rs/rabex-env/badge.svg
[docs.rs]: https://docs.rs/crate/rabex-env/
[License_MIT]: https://img.shields.io/badge/License-MIT-yellow.svg
[license_mit]: https://raw.githubusercontent.com/UniversalGameExtraction/RustyAssetBundleEXtractor/main/LICENSE-MIT
[License_APACHE]: https://img.shields.io/badge/License-Apache%202.0-blue.svg
[license_apache]: https://raw.githubusercontent.com/UniversalGameExtraction/RustyAssetBundleEXtractor/main/LICENSE-APACHE

A crate for working with Unity Engine asset files. It supports reading and writing bundle files and serialized files, as well as reading typetrees with serde integration.

## Examples

```rust
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
            let file = env.load_addressables_bundle_content(&bundle)?;
            println!(
                "{} contains {} objects",
                bundle.display(),
                file.objects::<()>().len()
            );
        }
    }

    Ok(())
}
```

**Collect which C# Scripts are used in addressables**

[examples/addressables_scripts.rs](./examples/addressables_scripts.rs)

**Generate serde rust types for unity objects/MonoBehaviours**

[examples/unity2rust.rs](./examples/unity2rust.rs)

## Related Projects
- [RustyAssetBundleEXtractor](https://github.com/jakobhellermann/RustyAssetBundleEXtractor): Lower-level crate used for serializedfile and typetree handling
- [unity-scene-repacker](https://github.com/jakobhellermann/unity-scene-repacker/): Command line tool for repacking unity scenes and asset bundles into distilled versions suitable for loading certain objects in mods
- [steam-multiversion-viewer](https://github.com/jakobhellermann/steam-multiversion-viewer): [wip] Visual exploration tool for steam game versions, with support for reading and structurally diffing unity game files.
