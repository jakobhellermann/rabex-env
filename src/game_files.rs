use std::ffi::OsStr;
use std::fs::File;
use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use memmap2::Mmap;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};

use crate::env::Data;
use crate::resolver::EnvResolver;

pub struct GameFiles {
    pub game_dir: PathBuf,
    pub level_files: LevelFiles,
}

pub enum LevelFiles {
    Unpacked,
    Packed(Box<BundleFileReader<Cursor<Mmap>>>),
}

fn find_unity_data_dir(install_dir: &Path) -> Result<Option<PathBuf>> {
    Ok(std::fs::read_dir(install_dir)?
        .filter_map(Result::ok)
        .find_map(|entry| is_unity_data_dir(&entry.path()).then(|| entry.path())))
}

fn is_unity_data_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with("_Data"))
        && dir.is_dir()
}

impl GameFiles {
    pub fn probe(game_dir: impl AsRef<Path>) -> Result<GameFiles> {
        GameFiles::probe_inner(game_dir.as_ref())
    }

    pub fn probe_dir(game_dir: &Path) -> Result<PathBuf> {
        ensure!(
            game_dir.exists(),
            "Game Directory '{}' does not exist",
            game_dir.display()
        );

        Ok(match is_unity_data_dir(game_dir) {
            true => game_dir.to_owned(),
            false => match find_unity_data_dir(game_dir)? {
                Some(dir) => dir,
                None => {
                    bail!(
                        "Game Directory '{}' is not a unity game. It should have a gamename_Data folder.",
                        game_dir.display()
                    )
                }
            },
        })
    }
    fn probe_inner(game_dir: &Path) -> Result<GameFiles> {
        let game_dir = GameFiles::probe_dir(game_dir)?;

        let bundle_path = game_dir.join("data.unity3d");
        let level_files = if bundle_path.exists() {
            let reader = unsafe { Mmap::map(&File::open(&bundle_path)?)? };
            let bundle =
                BundleFileReader::from_reader(Cursor::new(reader), &ExtractionConfig::default())?;

            LevelFiles::Packed(Box::new(bundle))
        } else {
            LevelFiles::Unpacked
        };

        Ok(GameFiles {
            game_dir: game_dir.to_owned(),
            level_files,
        })
    }

    pub fn read(&self, filename: &str) -> Result<Data, std::io::Error> {
        match &self.level_files {
            LevelFiles::Unpacked => {
                let path = self.game_dir.join(filename);
                let file = File::open(path)?;
                let mmap = unsafe { Mmap::map(&file)? };
                Ok(Data::Mmap(mmap))
            }
            LevelFiles::Packed(bundle) => {
                let data = bundle.read_at(filename)?.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "File not found in bundle")
                })?;
                Ok(Data::InMemory(data))
            }
        }
    }
}

impl EnvResolver for GameFiles {
    fn base_dir(&self) -> &Path {
        &self.game_dir
    }

    fn read_path(&self, path: &Path) -> Result<Data, std::io::Error> {
        if let Ok(suffix) = path.strip_prefix("Library") {
            let resource_path = self.game_dir.join("Resources").join(suffix);

            match File::open(resource_path) {
                Ok(val) => {
                    let mmap = unsafe { memmap2::Mmap::map(&val)? };
                    return Ok(Data::Mmap(mmap));
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }

        match &self.level_files {
            LevelFiles::Unpacked => {
                let file = File::open(self.game_dir.join(path))?;
                let mmap = unsafe { memmap2::Mmap::map(&file)? };
                Ok(Data::Mmap(mmap))
            }
            LevelFiles::Packed(bundle) => {
                let path = path
                    .to_str()
                    .ok_or_else(|| std::io::Error::other("non-utf8 string"))?;
                let data = bundle.read_at(path)?.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("File '{path}' does not exist in bundle"),
                    )
                })?;

                Ok(Data::InMemory(data))
            }
        }
    }

    fn all_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        match &self.level_files {
            LevelFiles::Unpacked => {
                let mut all = Vec::new();
                for entry in std::fs::read_dir(&self.game_dir)? {
                    let entry = entry?;

                    if entry.file_type()?.is_dir() {
                        continue;
                    }

                    all.push(
                        entry
                            .path()
                            .strip_prefix(&self.game_dir)
                            .unwrap()
                            .to_owned(),
                    );
                }
                Ok(all)
            }
            LevelFiles::Packed(bundle) => {
                // TODO: non-unity3d files as well
                Ok(bundle
                    .files()
                    .iter()
                    .map(|file| file.path.clone().into())
                    .collect())
            }
        }
    }
}
