//! Mapping a loaded file's path to the scene it belongs to, for the `_scene` enrichment key.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use rabex_env::Environment;
use rabex_env::addressables::ArchivePath;
use rabex_env::addressables::binary_catalog::resource_providers;
use rabex_env::rabex::typetree::TypeTreeProvider;
use rabex_env::resolver::EnvResolver;

/// A scene resource location's `type_` is this class.
const SCENE_INSTANCE_CLASS: &str = "UnityEngine.ResourceManagement.ResourceProviders.SceneInstance";

/// Resolves a serialized-file / bundle path to its scene name. Built once per [`Environment`] and
/// then queried per object, so the addressables catalog is parsed a single time.
///
/// Two sources, mirroring how the engine loads scenes:
/// - built-in scenes, indexed by their `levelN` file (`level3` → `scene_names()[3]`);
/// - addressables scenes, resolved from the catalog. Each scene's `SceneInstance` location names
///   the scene (its `primary_key`'s file stem) and depends on one asset-bundle location; that
///   bundle's CABs are the files scene objects actually load from. Catalog bundle paths and the
///   on-disk `cab_to_bundle` map are joined by bundle **file name** (stable across the path-prefix
///   normalisation the two sources apply differently).
#[derive(Default)]
pub struct SceneIndex {
    levels: Vec<String>,
    cab_to_scene: HashMap<String, String>,
}

impl SceneIndex {
    pub fn build<R: EnvResolver, P: TypeTreeProvider>(env: &Environment<R, P>) -> Result<Self> {
        // A game without `BuildSettings` (e.g. a bare serialized file) simply has no built-in
        // scenes — that's not an error, it just means no `levelN` names to resolve.
        let levels = match env.build_settings() {
            Ok(bs) => bs.scene_names().map(ToOwned::to_owned).collect(),
            Err(_) => Vec::new(),
        };

        let mut cab_to_scene = HashMap::new();
        if let Some(aa) = env.addressables()? {
            // bundle file name -> its CABs (the archive entries objects load under).
            let mut file_to_cabs: HashMap<OsString, Vec<String>> = HashMap::new();
            for (bundle, cabs) in &aa.bundle_to_cab {
                if let Some(name) = bundle.file_name() {
                    file_to_cabs
                        .entry(name.to_owned())
                        .or_default()
                        .extend(cabs.iter().cloned());
                }
            }

            for mut catalog in aa.catalogs(&env.game_files)? {
                let catalog = catalog.read()?;
                for loc in catalog.locations() {
                    if loc.provider_id.as_str() != resource_providers::BUNDLED_ASSET
                        || loc.type_.m_ClassName.as_str() != SCENE_INSTANCE_CLASS
                    {
                        continue;
                    }
                    let Some(name) = Path::new(loc.primary_key.as_str())
                        .file_stem()
                        .and_then(|s| s.to_str())
                    else {
                        continue;
                    };
                    let Some(dep) = loc
                        .dependencies
                        .iter()
                        .find(|dep| dep.provider_id.as_str() == resource_providers::ASSET_BUNDLE)
                    else {
                        continue;
                    };
                    let bundle = aa.evaluate_string(&dep.internal_id);
                    let Some(file_name) = Path::new(&bundle).file_name() else {
                        continue;
                    };
                    if let Some(cabs) = file_to_cabs.get(file_name) {
                        for cab in cabs {
                            cab_to_scene
                                .entry(cab.clone())
                                .or_insert_with(|| name.to_owned());
                        }
                    }
                }
            }
        }

        Ok(SceneIndex {
            levels,
            cab_to_scene,
        })
    }

    /// Every scene name known to the index (built-in first, then addressables; addressables names
    /// may repeat, once per CAB). Mainly for tests / listing.
    pub fn scene_names(&self) -> impl Iterator<Item = &str> {
        self.levels
            .iter()
            .map(String::as_str)
            .chain(self.cab_to_scene.values().map(String::as_str))
    }

    /// The scene `path` belongs to, or `None` if it isn't a scene file. `path` is either a `levelN`
    /// name or an `archive:/<cab>/<file>` path (as PPtr externals / loaded scene bundles carry).
    pub fn scene_of(&self, path: &str) -> Option<&str> {
        if let Some(index) = path
            .strip_prefix("level")
            .and_then(|n| n.parse::<usize>().ok())
        {
            return self.levels.get(index).map(String::as_str);
        }
        let archive = ArchivePath::try_parse(Path::new(path)).ok().flatten()?;
        self.cab_to_scene.get(archive.bundle).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::SceneIndex;
    use std::collections::HashMap;

    #[test]
    fn scene_of_resolves_levels_and_archive_cabs() {
        let index = SceneIndex {
            levels: vec!["Boot".to_owned(), "Menu".to_owned()],
            cab_to_scene: HashMap::from([("CAB-abyss".to_owned(), "Abyss_01".to_owned())]),
        };

        assert_eq!(index.scene_of("level0"), Some("Boot"));
        assert_eq!(index.scene_of("level1"), Some("Menu"));
        assert_eq!(index.scene_of("level2"), None); // out of range
        assert_eq!(
            index.scene_of("archive:/CAB-abyss/CAB-abyss"),
            Some("Abyss_01")
        );
        assert_eq!(index.scene_of("archive:/CAB-other/CAB-other"), None);
        assert_eq!(index.scene_of("sharedassets0.assets"), None);
    }
}
