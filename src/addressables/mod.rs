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

impl std::fmt::Debug for AddressablesData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressablesData")
            .field("settings", &self.settings)
            .field("bundle_to_cab", &self.bundle_to_cab)
            .finish()
    }
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

    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
    pub(crate) fn read<R: EnvResolver, P: TypeTreeProvider>(
        env: &Environment<R, P>,
    ) -> Result<Option<AddressablesData>> {
        let settings = Path::new("StreamingAssets/aa/settings.json");
        let settings_bytes = match env.game_files.read_path(settings) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let settings: AddressablesSettings = serde_json::from_slice(settings_bytes.as_ref())?;

        let aa_build = Path::new("StreamingAssets/aa").join(&settings.m_buildTarget);
        let mut lookup =
            addressables_bundle_lookup(&env.game_files, &aa_build, env.unity_version()?)
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
#[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
fn addressables_bundle_lookup<R: EnvResolver>(
    env: &R,
    aa_build: &Path,
    unity_version: &UnityVersion,
) -> Result<(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)> {
    use rayon::prelude::*;

    #[cfg(feature = "tracing-instrument")]
    let parent_span = tracing::Span::current();

    let files = env.list_under(aa_build)?;
    files
        .into_par_iter()
        .try_fold(
            <(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)>::default,
            |mut acc, path| -> Result<_> {
                #[cfg(feature = "tracing-instrument")]
                let _parent = parent_span.enter();

                let relative = path.strip_prefix(aa_build).unwrap();

                let bundle_to_cab_files: &mut Vec<_> =
                    acc.1.entry(relative.to_owned()).or_default();

                #[cfg(feature = "tracing-instrument")]
                let _span = tracing::info_span!("read_bundle").entered();

                // PERF: (tested with 2k bundles on silksong)

                // Just doing BufReader<File> takes 2.5ms.
                // Doing `read_path` which does mmap internally takes 15ms.
                // TODO: figure out how to architect this better

                let data = env.read_path(&path)?;
                let mut data = data.as_ref();
                let data = Cursor::new(&mut data);
                let bundle = BundleFileReader::from_reader(
                    data,
                    &ExtractionConfig::default().with_fallback_unity_version(unity_version.clone()),
                )?;

                for file in bundle.files() {
                    acc.0.insert(file.path.clone(), relative.to_owned());
                    bundle_to_cab_files.push(file.path.clone());
                }

                Ok(acc)
            },
        )
        .try_reduce(Default::default, |mut acc, item| {
            acc.0.extend(item.0);
            acc.1.extend(item.1);
            Ok(acc)
        })
}
