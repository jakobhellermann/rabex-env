use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::env::Data;

/// A trait abstracting where the game files are read from. All paths
/// are interpreted as relative to the resolver's root — for the
/// `GameFiles` backend that's the `_Data/` directory (mirrors what
/// `probe_dir` lands on); depot-backed resolvers map the same shape
/// onto their manifest.
pub trait EnvResolver {
    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error>;
    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error>;

    /// List every file under `prefix` (paths relative to the resolver
    /// root). The default impl filters [`all_files`] in O(N), which is
    /// fine for in-memory backings (e.g. depot manifests). Filesystem-
    /// backed resolvers should override with a directory walk so the
    /// addressables-bundle scan doesn't read the whole game tree.
    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        Ok(self
            .all_files()?
            .into_iter()
            .filter(|p| p.starts_with(prefix))
            .collect())
    }

    fn serialized_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        Ok(self
            .all_files()?
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                let is_level = name
                    .strip_prefix("level")
                    .and_then(|x| x.parse::<usize>().ok())
                    .is_some();

                let is_serialized = is_level
                    || path.extension().is_some_and(|e| e == "assets")
                    || name == "globalgamemanagers";

                is_serialized.then_some(path)
            })
            .collect())
    }

    fn level_files(&self) -> Result<Vec<usize>, std::io::Error> {
        Ok(self
            .all_files()?
            .iter()
            .filter_map(|path| path.file_name()?.to_str())
            .filter_map(|path| {
                let index = path.strip_prefix("level")?;
                index.parse::<usize>().ok()
            })
            .collect())
    }
}

impl<T: EnvResolver> EnvResolver for &T {
    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error> {
        (**self).read_path(path)
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        (**self).all_files()
    }

    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        (**self).list_under(prefix)
    }
}
