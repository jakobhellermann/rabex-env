use std::path::{Path, PathBuf};
use std::sync::Arc;

use rabex_env::resolver::EnvResolver;
use steam_depot_vfs::chunk_store::{ChunkStore, FsCacheStore};
use steam_depot_vfs::fs::{DepotFileReader, DepotManifestStore};
use tokio::runtime::Handle;

pub use steam_depot_vfs;
pub use steam_vent_depot;

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

    fn depot_path(&self, path: &Path) -> PathBuf {
        // TODO: consolidate and move away from the trait implementations?
        let mut components = path.components();
        let first = components.next().and_then(|c| c.as_os_str().to_str());
        let is_resource_dir =
            first.is_some_and(|s| matches!(s, "library" | "Library" | "resources" | "Resources"));
        if is_resource_dir {
            return self.data_dir().join("Resources").join(&components);
        }

        self.data_dir.join(path)
    }
}

fn utf8_path(path: &Path) -> Result<String, std::io::Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidFilename,
            format!("'{}' is not a valid utf8 filename", path.display()),
        )
    })
}

impl<C: ChunkStore> EnvResolver for SteamDepotGameFiles<C> {
    type Reader<'a>
        = DepotFileReader<'a, C>
    where
        Self: 'a;

    fn open_path(&self, path: &Path) -> Result<Self::Reader<'_>, std::io::Error> {
        let path = self.depot_path(path);
        let path = utf8_path(&path)?;
        let reader = self
            .manifest_store
            .open_reader(&path, self.handle.clone())?;
        Ok(reader)
    }

    #[track_caller]
    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(skip_all, fields(path = %path.display()))
    )]
    fn read_path(&self, path: &Path) -> Result<rabex_env::env::Data, std::io::Error> {
        let path = self.depot_path(path);
        let path = utf8_path(&path)?;

        let metadata = self.manifest_store.metadata(&path)?;

        let mut out = Vec::with_capacity(metadata.size as usize);
        let f = self
            .manifest_store
            .read_into(&path, 0, metadata.size, &mut out);

        self.handle.block_on(f)?;

        Ok(rabex_env::env::Data::InMemory(out))
    }

    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
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

    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(skip_all, fields(prefix = %prefix.display()))
    )]
    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        // PERF: O(n)
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
