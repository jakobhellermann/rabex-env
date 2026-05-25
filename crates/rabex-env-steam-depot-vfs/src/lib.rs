use std::path::{Path, PathBuf};
use std::sync::Arc;

use rabex_env::resolver::EnvResolver;
use steam_depot_vfs::chunk_store::{ChunkStore, FsCacheStore};
use steam_depot_vfs::fs::DepotManifestStore;
use tokio::runtime::Handle;

pub struct SteamDepotGameFiles<C: ChunkStore = FsCacheStore> {
    data_dir: PathBuf,
    manifest_store: Arc<DepotManifestStore<C>>,
    handle: Handle,
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
            handle: Handle::current(),
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
    fn base_dir(&self) -> &Path {
        todo!()
    }

    #[track_caller]
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

        self.handle.block_on(f).unwrap();

        Ok(rabex_env::env::Data::InMemory(out))
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
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
}
