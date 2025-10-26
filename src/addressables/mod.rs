mod archive_path;
pub mod binary_catalog;
pub mod settings;

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use memmap2::Mmap;
use rabex::UnityVersion;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::typetree::TypeTreeProvider;
use rustc_hash::FxHashMap;
use walkdir::WalkDir;

use crate::Environment;
use crate::addressables::binary_catalog::{BinaryCatalogReader, ResourceLocation};
use crate::addressables::settings::AddressablesSettings;
use crate::resolver::EnvResolver;

pub use archive_path::ArchivePath;

pub struct AddressablesData {
    base_dir: PathBuf,
    pub settings: AddressablesSettings,
    pub cab_to_bundle: FxHashMap<String, PathBuf>,
    pub bundle_to_cab: FxHashMap<PathBuf, Vec<String>>,
}
impl AddressablesData {
    pub fn build_folder(&self) -> PathBuf {
        self.settings.build_folder()
    }

    pub fn bundle_paths(&self) -> impl Iterator<Item = &Path> {
        self.bundle_to_cab.keys().map(AsRef::as_ref)
    }

    pub fn catalogs(&self) -> Result<Vec<BinaryCatalogReader<impl Read + Seek>>, std::io::Error> {
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
                    let data = unsafe { Mmap::map(&File::open(self.base_dir.join(path))?)? };
                    BinaryCatalogReader::new(Cursor::new(data))
                })())
            })
            .collect()
    }

    pub fn resource_locations(&self) -> Result<Vec<Arc<ResourceLocation>>> {
        let mut all = Vec::new();
        for mut catalog in self.catalogs()? {
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
        let base_dir = env.game_files.base_dir();
        let aa = base_dir.join("StreamingAssets/aa");
        if !aa.exists() {
            return Ok(None);
        }
        let reader =
            BufReader::new(File::open(aa.join("settings.json")).context("no settings.json")?);
        let settings: AddressablesSettings = serde_json::from_reader(reader)?;

        let lookup =
            addressables_bundle_lookup(&aa.join(&settings.m_buildTarget), env.unity_version()?)
                .context("could not determine CAB locations")?;
        let data = AddressablesData {
            base_dir: base_dir.to_owned(),
            settings,
            cab_to_bundle: lookup.0,
            bundle_to_cab: lookup.1,
        };
        Ok(Some(data))
    }
}

#[allow(clippy::type_complexity)]
fn addressables_bundle_lookup(
    aa_build: &Path,
    unity_version: &UnityVersion,
) -> Result<(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)> {
    use rayon::prelude::*;

    WalkDir::new(aa_build)
        .into_iter()
        .par_bridge()
        .try_fold(
            <(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)>::default,
            |mut acc, path| -> Result<_> {
                let path = path?;
                if path.file_type().is_dir() {
                    return Ok(acc);
                }
                let relative = path.path().strip_prefix(aa_build).unwrap();

                let bundle_to_cab_files: &mut Vec<_> =
                    acc.1.entry(relative.to_owned()).or_default();

                // PERF: (tested with 2k bundles on silksong)
                // seq + mmap: 27ms
                // seq + read: 18ms
                // par + mmap: 20ms
                // par + read: 7ms
                let bundle = BundleFileReader::from_reader(
                    BufReader::new(File::open(path.path())?),
                    &ExtractionConfig::new(None, Some(unity_version.clone())),
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
