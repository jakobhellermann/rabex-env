use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::env::Data;
use crate::resolver::EnvResolver;

/// An [`EnvResolver`] backed by in-memory files.
pub struct MemResolver {
    files: HashMap<PathBuf, Vec<u8>>,
}

impl MemResolver {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    pub fn single(path: &str, bytes: Vec<u8>) -> Self {
        let mut files = HashMap::new();
        files.insert(PathBuf::from(path), bytes);
        Self { files }
    }

    pub fn insert(&mut self, path: &str, bytes: Vec<u8>) -> &mut Self {
        self.files.insert(PathBuf::from(path), bytes);
        self
    }
}

impl Default for MemResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvResolver for MemResolver {
    type Reader<'a>
        = Cursor<&'a [u8]>
    where
        Self: 'a;

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error> {
        self.files
            .get(path)
            .map(|v| Data::InMemory(v.clone()))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, path.display().to_string())
            })
    }

    fn open_path(&self, path: &Path) -> Result<Self::Reader<'_>, std::io::Error> {
        self.files
            .get(path)
            .map(|v| Cursor::new(v.as_slice()))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, path.display().to_string())
            })
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        Ok(self.files.keys().cloned().collect())
    }
}
