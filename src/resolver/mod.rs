//! Filesystem abstraction for game files
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::env::Data;

pub mod game_files;
mod mem;

pub use game_files::GameFiles;
pub use mem::MemResolver;

/// A trait abstracting where the game files are read from.
/// All paths are interpreted as relative to the `Game_Data/` directory.
pub trait EnvResolver: Sync {
    /// Reader returned by [`EnvResolver::open_path`].
    type Reader<'a>: Read + Seek
    where
        Self: 'a;

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error>;

    /// Prefer this over [`EnvResolver::read_path`] when only a small prefix of the file is needed
    fn open_path(&self, path: &Path) -> Result<Self::Reader<'_>, std::io::Error>;

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error>;

    /// List every file under `prefix`
    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        // PERF: O(n) default impl
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
    type Reader<'a>
        = T::Reader<'a>
    where
        Self: 'a;

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error> {
        (**self).read_path(path)
    }

    fn open_path(&self, path: &Path) -> Result<Self::Reader<'_>, std::io::Error> {
        (**self).open_path(path)
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        (**self).all_files()
    }

    fn list_under(&self, prefix: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
        (**self).list_under(prefix)
    }
}

pub enum DataOrFile {
    Data(Cursor<Data>),
    File(BufReader<File>),
}

impl Read for DataOrFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            DataOrFile::Data(c) => c.read(buf),
            DataOrFile::File(f) => f.read(buf),
        }
    }
}

impl Seek for DataOrFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            DataOrFile::Data(c) => c.seek(pos),
            DataOrFile::File(f) => f.seek(pos),
        }
    }
}
