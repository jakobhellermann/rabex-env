use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::env::Data;

/// A trait abstracting where the game files are read from.
pub trait EnvResolver {
    fn base_dir(&self) -> &Path;

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error>;
    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error>;

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
    fn base_dir(&self) -> &Path {
        (**self).base_dir()
    }

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error> {
        (**self).read_path(path)
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        (**self).all_files()
    }
}
