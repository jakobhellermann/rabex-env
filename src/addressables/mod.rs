mod archive_path;
pub mod binary_catalog;
pub mod settings;

use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use rabex::UnityVersion;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::typetree::TypeTreeProvider;
use rustc_hash::FxHashMap;

use crate::Environment;
use crate::addressables::binary_catalog::{BinaryCatalogReader, ResourceLocation};
use crate::addressables::settings::AddressablesSettings;
use crate::resolver::EnvResolver;

pub use archive_path::ArchivePath;

pub struct AddressablesData {
    pub settings: AddressablesSettings,
    pub cab_to_bundle: FxHashMap<String, PathBuf>,
    pub bundle_to_cab: FxHashMap<PathBuf, Vec<String>>,
}
impl AddressablesData {
    pub fn bundle_main_archive_path(&self, bundle_path: &Path) -> Option<ArchivePath<'_>> {
        let bundle_contents = self.bundle_to_cab.get(bundle_path)?;
        let first = bundle_contents.first()?;
        let archive_path = ArchivePath::same(first);
        Some(archive_path)
    }

    pub fn build_folder(&self) -> PathBuf {
        self.settings.build_folder()
    }

    pub fn bundle_paths(&self) -> impl Iterator<Item = &Path> {
        self.bundle_to_cab.keys().map(AsRef::as_ref)
    }

    /// Open each registered catalog. The bytes come through the
    /// passed-in resolver — this used to mmap from `base_dir.join(...)`
    /// directly, which prevented depot-backed environments from
    /// reading catalogs.
    pub fn catalogs(
        &self,
        env: &impl EnvResolver,
    ) -> Result<Vec<BinaryCatalogReader<impl Read + Seek>>, std::io::Error> {
        self.settings
            .m_CatalogLocations
            .iter()
            .filter_map(|catalog| {
                if catalog.m_Provider
                    != "UnityEngine.AddressableAssets.ResourceProviders.ContentCatalogProvider"
                {
                    tracing::warn!("Unsupported catalog provider: '{}'", catalog.m_Provider);
                    return None;
                }
                let path = self.evaluate_string(&catalog.m_InternalId);

                Some((|| {
                    let data = env.read_path(Path::new(&path))?;
                    // `Data` impls `AsRef<[u8]>`, so `Cursor<Data>` is
                    // already `Read + Seek` — no need to copy the bytes
                    // (catalogs can be megabytes).
                    BinaryCatalogReader::new(Cursor::new(data))
                })())
            })
            .collect()
    }

    pub fn resource_locations(&self, env: &impl EnvResolver) -> Result<Vec<Arc<ResourceLocation>>> {
        let mut all = Vec::new();
        for mut catalog in self.catalogs(env)? {
            let catalog = catalog.read()?;

            for (_key, locations) in catalog.resources {
                for loc in locations {
                    all.push(loc);
                }
            }
        }
        Ok(all)
    }

    pub fn evaluate_string(&self, str: &str) -> String {
        str.replace(
            "{UnityEngine.AddressableAssets.Addressables.RuntimePath}",
            "StreamingAssets/aa",
        )
    }

    pub(crate) fn read<R: EnvResolver, P: TypeTreeProvider>(
        env: &Environment<R, P>,
    ) -> Result<Option<AddressablesData>> {
        // Settings drives whether the game ships addressables at all —
        // absent file = no addressables, just return None. We go
        // through the resolver so depot-backed environments work the
        // same as filesystem-backed ones.
        let settings_bytes = match env
            .game_files
            .read_path(Path::new("StreamingAssets/aa/settings.json"))
        {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let settings: AddressablesSettings = serde_json::from_slice(settings_bytes.as_ref())?;

        let aa_build_rel = Path::new("StreamingAssets/aa").join(&settings.m_buildTarget);
        let mut lookup =
            addressables_bundle_lookup(&env.game_files, &aa_build_rel, env.unity_version()?)
                .context("could not determine CAB locations")?;
        lookup.1.values_mut().for_each(|files| files.sort());
        let data = AddressablesData {
            settings,
            cab_to_bundle: lookup.0,
            bundle_to_cab: lookup.1,
        };
        Ok(Some(data))
    }
}

#[allow(clippy::type_complexity)]
fn addressables_bundle_lookup<R: EnvResolver>(
    env: &R,
    aa_build_rel: &Path,
    unity_version: &UnityVersion,
) -> Result<(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)> {
    // Sequential. The previous rayon impl saved ~20ms on silksong's
    // 2k bundles by parallelising the file reads — but the resolver
    // abstraction doesn't make Send+Sync guarantees about every
    // backend (the depot resolver uses `block_in_place` which only
    // works on tokio worker threads). If perf becomes a bottleneck we
    // can reintroduce rayon behind a `Send + Sync` bound on R.
    let mut acc: (FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>) = Default::default();
    for path in env.list_under(aa_build_rel)? {
        let relative = match path.strip_prefix(aa_build_rel) {
            Ok(rel) => rel.to_owned(),
            Err(_) => continue,
        };
        let bundle_bytes = env.read_path(&path)?;
        let bundle = BundleFileReader::from_reader(
            Cursor::new(bundle_bytes.as_ref()),
            &ExtractionConfig::default().with_fallback_unity_version(unity_version.clone()),
        )?;
        let bundle_to_cab_files: &mut Vec<_> = acc.1.entry(relative.clone()).or_default();
        for file in bundle.files() {
            acc.0.insert(file.path.clone(), relative.clone());
            bundle_to_cab_files.push(file.path.clone());
        }
    }
    Ok(acc)
}
