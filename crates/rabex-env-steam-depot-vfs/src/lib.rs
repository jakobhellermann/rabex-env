use std::path::{Path, PathBuf};
use std::sync::Arc;

use rabex_env::resolver::EnvResolver;
use steam_depot_vfs::chunk_store::ChunkStore;
use steam_depot_vfs::fs::DepotManifestStore;
use tokio::runtime::Handle;

pub struct SteamDepotGameFiles<C: ChunkStore> {
    data_dir: PathBuf,
    manifest_store: Arc<DepotManifestStore<C>>,
}

impl<C: ChunkStore> SteamDepotGameFiles<C> {
    pub fn new(manifest_store: Arc<DepotManifestStore<C>>) -> Result<Self, std::io::Error> {
        let root_files = manifest_store
            .list_dir("/")
            .map_err(|e| std::io::Error::other(e))?;
        let data_dir = root_files
            .iter()
            .find(|x| x.name.ends_with("_Data"))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "couldnt find unity data dir, found {:?}",
                        root_files.iter().map(|x| &x.name).collect::<Vec<_>>()
                    ),
                )
            })?;
        Ok(Self {
            data_dir: PathBuf::from(data_dir.name.clone()),
            manifest_store,
        })
    }

    /// Directory the unity build keeps its assets in, relative to the
    /// manifest root (e.g. `hollow_knight_Data`). Useful for callers
    /// that hold a manifest-relative path and need to strip the prefix
    /// before handing it to `Environment::load_*`, which works in
    /// data-dir-relative paths.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

impl<C: ChunkStore> EnvResolver for SteamDepotGameFiles<C> {
    #[track_caller]
    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(skip_all, fields(path = %path.display()))
    )]
    fn read_path(&self, path: &Path) -> Result<rabex_env::env::Data, std::io::Error> {
        // PERF: reduce allocation
        let path = if let Ok(suffix) = path.strip_prefix("Library") {
            self.data_dir.join("Resources").join(suffix)
        } else {
            self.data_dir.join(path)
        };

        // TODO: O(n)
        let Some(path) = path.to_str() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidFilename,
                format!("'{}' is not a valid utf8 filename", path.display()),
            ));
        };
        let metadata = self.manifest_store.metadata(path)?;

        let mut out = Vec::with_capacity(metadata.size as usize);
        let f = self
            .manifest_store
            .read_into(path, 0, metadata.size, &mut out);

        tokio::task::block_in_place(|| Handle::current().block_on(f)).unwrap();

        Ok(rabex_env::env::Data::InMemory(out))
    }

    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        // Resolver paths are data-dir-relative (mirror of `read_path`,
        // which joins with `self.data_dir`). Strip the prefix on the
        // way out so callers like `addressables::list_under` see the
        // same shape they pass back into `read_path`.
        Ok(self
            .manifest_store
            .manifest()
            .normal_paths()
            .filter_map(|p| {
                Path::new(p)
                    .strip_prefix(&self.data_dir)
                    .ok()
                    .map(PathBuf::from)
            })
            .collect())
    }

    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(skip_all, fields(prefix = %prefix.display()))
    )]
    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        // Walk the manifest directly instead of `all_files() + filter`
        // — same complexity in theory (manifest paths are in-memory),
        // but skips two allocations per entry and gives a cheaper
        // happy path for the common "addressables bundle scan" case.
        Ok(self
            .manifest_store
            .manifest()
            .normal_paths()
            .filter_map(|p| {
                let rel = Path::new(p).strip_prefix(&self.data_dir).ok()?;
                rel.starts_with(prefix).then(|| rel.to_owned())
            })
            .collect())
    }
}
