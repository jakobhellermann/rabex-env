//! Home for the [`Environment`] abstraction and associated types.
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io::{Cursor, Read, Seek};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use elsa::sync::FrozenMap;
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::TypedPPtr;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};

use crate::addressables::settings::AddressablesSettings;
use crate::addressables::{AddressablesData, ArchivePath};
use crate::handle::SerializedFileHandle;
use crate::resolver::{EnvResolver, GameFiles};
use crate::typetree_generator_cache::TypeTreeGeneratorCache;
use crate::unity::types::{BuildSettings, MonoManager, MonoScript, ResourceManager};

/// Owned or mmap-backed bytes
pub enum Data {
    InMemory(Vec<u8>),
    Mmap(memmap2::Mmap),
}
impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        match self {
            Data::InMemory(data) => data.as_slice(),
            Data::Mmap(mmap) => mmap.as_ref(),
        }
    }
}
impl From<Vec<u8>> for Data {
    fn from(data: Vec<u8>) -> Self {
        Data::InMemory(data)
    }
}

/// The central entrypoint to a untiy game.
/// ```no_run
/// use anyhow::{Context, Result};
/// use rabex::tpk::TpkTypeTreeBlob;
/// use rabex::typetree::typetree_cache::sync::TypeTreeCache;
/// use rabex_env::Environment;
/// use rabex_env::unity::types::BuildSettings;
///
/// fn main() -> Result<()> {
///     let game_path = std::env::args().nth(1).context("missing game path")?;
///
///     let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
///     let env = Environment::new_in(game_path, tpk)?;
///
///     let version = env.unity_version()?;
///     println!("Unity Version: {}", version);
///
///     let ggm = env.globalgamemanagers()?;
///     let build_settings = ggm.find_object_of::<BuildSettings>()?.unwrap();
///     println!(
///         "Scenes: {:?}",
///         build_settings.scene_names().collect::<Vec<_>>()
///     );
///
///     // load and read components of a serialized file
///     let level0 = env.load_serialized("level0")?;
///     for transform in level0.transforms() {
///         let transform = transform.read()?;
///         if transform.m_Father.is_null() {
///             let game_object = level0.deref_read(transform.m_GameObject)?;
///             println!("- {}", game_object.m_Name)
///         }
///     }
///
///     // load addressables bundles
///     let addressables = env.addressables()?;
///     if let Some(addressables) = addressables {
///         for bundle in addressables.bundle_paths().take(10) {
///             let file = env.load_addressables_bundle_content(&bundle)?;
///             println!(
///                 "{} contains {} objects",
///                 bundle.display(),
///                 file.objects::<()>().len()
///             );
///         }
///     }
///
///     Ok(())
/// }
/// ```
pub struct Environment<R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>> {
    pub game_files: R,
    pub tpk: P,
    pub typetree_generator: TypeTreeGeneratorCache,
    serialized_files: FrozenMap<PathBuf, Box<(SerializedFile, Data)>>,
    unity_version: OnceLock<UnityVersion>,
    addressables: OnceLock<Option<AddressablesData>>,
}

impl<R: Debug, P> std::fmt::Debug for Environment<R, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment")
            .field("game_files", &self.game_files)
            .field(
                "loaded_serialized_files",
                &self.serialized_files.keys_cloned(),
            )
            .field("unity_version", &self.unity_version)
            .finish_non_exhaustive()
    }
}

impl<R, P> Environment<R, P> {
    /// Construct a new `Environment` with the specified resolver and typetree provider.
    pub fn new(resolver: R, tpk: P) -> Self {
        Environment {
            game_files: resolver,
            tpk,
            serialized_files: Default::default(),
            typetree_generator: TypeTreeGeneratorCache::empty(),
            unity_version: OnceLock::new(),
            addressables: OnceLock::new(),
        }
    }
}

impl<P: TypeTreeProvider> Environment<GameFiles, P> {
    /// Construct a new `Environment` from the path to a unity game.
    ///
    /// Accepts both the `unitygame_Data` directory and its parent.
    /// ```no_run
    /// # use rabex_env::Environment;
    /// # use rabex::tpk::TpkTypeTreeBlob;
    /// # use rabex::typetree::typetree_cache::TypeTreeCache;
    /// let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    /// let env = Environment::new("/path/to/game", tpk);
    /// ```
    pub fn new_in(path: impl AsRef<Path>, tpk: P) -> Result<Self> {
        Ok(Environment {
            game_files: GameFiles::probe(path.as_ref())?,
            tpk,
            serialized_files: Default::default(),
            typetree_generator: TypeTreeGeneratorCache::empty(),
            unity_version: OnceLock::new(),
            addressables: OnceLock::new(),
        })
    }
}

#[derive(Debug)]
pub struct AppInfo {
    pub developer: String,
    pub name: String,
}

impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    /// Reads the data from the `app.info` file
    pub fn app_info(&self) -> Result<AppInfo> {
        let data = self
            .game_files
            .read_path(Path::new("app.info"))
            .context("could not find app.info")?;
        let contents = std::str::from_utf8(data.as_ref()).context("app.info is not valid UTF-8")?;
        let (developer, name) = contents.split_once('\n').context("app.info is malformed")?;

        Ok(AppInfo {
            developer: developer.to_owned(),
            name: name.to_owned(),
        })
    }

    /// Returns the unity version of the game.
    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
    pub fn unity_version(&self) -> Result<&UnityVersion> {
        match self.unity_version.get() {
            Some(unity_version) => Ok(unity_version),
            None => {
                let ggm = self.globalgamemanagers()?;
                let unity_version = ggm
                    .file
                    .m_UnityVersion
                    .clone()
                    .context("missing unity version in globalgamemanagers")?;
                let _ = self.unity_version.set(unity_version);
                Ok(self.unity_version.get().unwrap())
            }
        }
    }

    /// Reads the `globalgamemanagers` serialized file, which contains
    /// global singletons like `MonoManager`, `PlayerSettings`, `BuildSettings` etc.
    pub fn globalgamemanagers(&self) -> Result<SerializedFileHandle<'_, R, P>> {
        self.load_serialized("globalgamemanagers")
    }

    /// Reads the [`BuildSettings`] singleton from `globalgamemanagers`
    pub fn build_settings(&self) -> Result<BuildSettings> {
        let ggm = self.globalgamemanagers()?;
        ggm.find_object_of::<BuildSettings>()
            .transpose()
            .context("no BuildSettings found in globalgamemanagers")
            .flatten()
    }

    /// Reads the [`ResourceManager`] singleton from `globalgamemanagers`
    pub fn resource_manager(&self) -> Result<ResourceManager> {
        let ggm = self.globalgamemanagers()?;
        ggm.find_object_of::<ResourceManager>()
            .transpose()
            .context("no ResourceManager found in globalgamemanagers")
            .flatten()
    }

    /// Reads the [`MonoManager`] singleton from `globalgamemanagers`
    pub fn mono_manager(&self) -> Result<MonoManager> {
        let ggm = self.globalgamemanagers()?;
        ggm.find_object_of::<MonoManager>()
            .transpose()
            .context("no MonoManager found in globalgamemanagers")
            .flatten()
    }

    /// Reads all [`MonoScript`]s from `globalgamemanagers`
    pub fn mono_scripts(&self) -> Result<Vec<MonoScript>> {
        let ggm = self.globalgamemanagers()?;
        let mono_manager = ggm.find_object_of::<MonoManager>()?.unwrap();
        mono_manager
            .m_Scripts
            .iter()
            .map(|script| ggm.deref_read(*script))
            .collect::<Result<Vec<_>, _>>()
    }

    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(skip_all, fields(path = %relative_path.as_ref().display()))
    )]
    pub fn load_serialized(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        self.load_external_file(relative_path.as_ref())
    }

    pub fn load_serialized_uncached(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<(SerializedFile, Data)> {
        let data = self.game_files.read_path(relative_path.as_ref())?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_ref()))?;
        Ok((file, data))
    }

    pub fn insert_cache(
        &self,
        path: PathBuf,
        file: SerializedFile,
        data: Data,
    ) -> SerializedFileHandle<'_, R, P> {
        let file = self.serialized_files.insert(path, Box::new((file, data)));
        SerializedFileHandle::new(self, &file.0, file.1.as_ref())
    }

    #[cfg_attr(
        feature = "tracing-instrument",
        tracing::instrument(level = "trace", skip_all, fields(path = %path_name.display()))
    )]
    pub(crate) fn load_external_file(
        &self,
        path_name: &Path,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        Ok(match self.serialized_files.get(path_name) {
            Some((file, data)) => SerializedFileHandle {
                file,
                data: data.as_ref(),
                env: self,
            },
            None => {
                if let Some(cab) = ArchivePath::try_parse(path_name)? {
                    let aa = self.addressables()?.context(
                        "Can't use archive:/ external files without addressables in the game",
                    )?;
                    let cab_bundle = aa
                        .cab_to_bundle
                        .get(cab.bundle)
                        .with_context(|| format!("CAB {} doesn't exist", cab))?;
                    let bundle = self.load_addressables_bundle(cab_bundle)?;
                    let cab_data = bundle
                        .read_at(cab.file)?
                        .expect("cab unexpectedly not present");

                    let mut serialized =
                        SerializedFile::from_reader(&mut Cursor::new(cab_data.as_slice()))?;
                    serialized
                        .m_UnityVersion
                        .get_or_insert(self.unity_version()?.clone());
                    let file = self.serialized_files.insert(
                        path_name.to_owned(),
                        Box::new((serialized, Data::InMemory(cab_data))),
                    );
                    return Ok(SerializedFileHandle::new(self, &file.0, file.1.as_ref()));
                }

                let data = self
                    .game_files
                    .read_path(Path::new(path_name))
                    .with_context(|| {
                        format!("Cannot read external file {}", path_name.display())
                    })?;
                let serialized = SerializedFile::from_reader(&mut Cursor::new(data.as_ref()))?;
                let file = self
                    .serialized_files
                    .insert(path_name.to_owned(), Box::new((serialized, data)));
                SerializedFileHandle::new(self, &file.0, file.1.as_ref())
            }
        })
    }

    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(level = "trace", skip_all))]
    pub fn deref_read<'de, T>(
        &self,
        pptr: TypedPPtr<T>,
        file: &SerializedFile,
        reader: &mut (impl Read + Seek),
    ) -> Result<T>
    where
        T: serde::Deserialize<'de>,
    {
        Ok(match pptr.m_FileID.get_externals_index() {
            None => pptr.deref_local(file, &self.tpk)?.read(reader)?,
            Some(external_index) => {
                let external_info = &file.m_Externals[external_index];
                let external = self
                    .load_external_file(Path::new(&external_info.pathName))
                    .with_context(|| {
                        format!("Failed to load external file {}", external_info.pathName)
                    })?;
                let object = pptr
                    .make_local()
                    .deref_local(external.file, &self.tpk)
                    .with_context(|| {
                        format!("In external {} {}", pptr.m_FileID, external_info.pathName)
                    })?;
                object.read(&mut Cursor::new(external.data))?
            }
        })
    }

    pub fn loaded_files(&mut self) -> impl Iterator<Item = &Path> {
        self.serialized_files.as_mut().keys().map(Deref::deref)
    }
}

impl<R: EnvResolver, P: TypeTreeProvider + Sync> Environment<R, P> {
    // TODO: non-addressables
    pub fn load_all_serialized_files(
        &self,
    ) -> Result<BTreeMap<String, SerializedFileHandle<'_, R, P>>> {
        let Some(addressables) = self.addressables()? else {
            return Ok(Default::default());
        };

        use rayon::iter::{ParallelBridge as _, ParallelIterator as _};

        addressables
            .bundle_paths()
            .par_bridge()
            .try_fold(BTreeMap::default, |mut acc, bundle_path| {
                let bundle = self.load_addressables_bundle(bundle_path)?;
                let bundle_identifier = bundle
                    .serialized_files()
                    .find_map(|file| match Path::new(&file.path).extension().is_some() {
                        true => None,
                        false => Some(&file.path),
                    })
                    .unwrap();

                for entry in bundle.serialized_files() {
                    let archive_path = ArchivePath::new(bundle_identifier, &entry.path);
                    let data = bundle.read_at_entry(entry)?;
                    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
                    let file = self.insert_cache(archive_path.into(), file, data.into());

                    acc.insert(archive_path.to_string(), file);
                }
                Ok(acc)
            })
            .try_reduce(Default::default, |mut acc, item| {
                for other in item {
                    acc.insert(other.0, other.1);
                }
                Ok(acc)
            })
    }
}

/// # Addressables
impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    /// Load an assetbundle from the [Addressables](https://docs.unity3d.com/Packages/com.unity.addressables@3.1/manual/index.html) system.
    ///
    /// The path is relative to the addressables build folder `StreamingAssets/aa/StandaloneLinux64`.
    pub fn load_addressables_bundle(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<BundleFileReader<Cursor<Data>>> {
        let aa_build = self
            .addressables_build_folder()?
            .context("no addressables settings found")?;
        let bundle_path = aa_build.join(bundle);
        let data = self
            .game_files
            .read_path(&bundle_path)
            .with_context(|| format!("read bundle {}", bundle_path.display()))?;
        let reader = BundleFileReader::from_reader(
            Cursor::new(data),
            &ExtractionConfig::default().with_fallback_unity_version(self.unity_version()?.clone()),
        )?;
        Ok(reader)
    }

    /// Loads the main `SerializedFile` from an assetbundle.
    pub fn load_addressables_bundle_content(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        let bundle = bundle.as_ref();

        let archive_path = self
            .addressables()?
            .context("can't load addressables bundle content without addressables in the game")?
            .bundle_main_archive_path(bundle)
            .with_context(|| {
                format!("'{}' is not a known addressables bundle", bundle.display())
            })?;

        if let Some(cached) = self
            .serialized_files
            .get(Path::new(&archive_path.to_string()))
        {
            return Ok(SerializedFileHandle::new(
                self,
                &cached.0,
                cached.1.as_ref(),
            ));
        }

        let (archive_name, file, data) = self.load_addressables_bundle_content_leaf(bundle)?;
        debug_assert_eq!(archive_path, ArchivePath::same(&archive_name));

        Ok(self.insert_cache(archive_path.to_string().into(), file, Data::InMemory(data)))
    }

    /// Loads the main `SerializedFile` from an assetbundle without internally caching it.
    pub fn load_addressables_bundle_content_leaf(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<(String, SerializedFile, Vec<u8>)> {
        let bundle = self
            .load_addressables_bundle(bundle.as_ref())
            .with_context(|| format!("Failed to load bundle '{}'", bundle.as_ref().display()))?;

        let entry = bundle
            .main_serializedfile()
            .context("no non-resource serializedfile in bundle")?;
        let data = bundle.read_at(&entry.path)?.unwrap();
        let mut file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        file.m_UnityVersion
            .get_or_insert(self.unity_version()?.clone());
        Ok((entry.path.clone(), file, data))
    }

    /// Relative path to the addressables build folder.
    pub fn addressables_build_folder(&self) -> Result<Option<PathBuf>> {
        let Some(settings) = self.addressables_settings()? else {
            return Ok(None);
        };
        let path = Path::new("StreamingAssets/aa").join(&settings.m_buildTarget);
        Ok(Some(path))
    }

    /// Returns the contents of the addressables `settings.json`
    pub fn addressables_settings(&self) -> Result<Option<&AddressablesSettings>> {
        Ok(self.addressables()?.map(|x| &x.settings))
    }

    /// All `.bundle` files living under, and relative to the addressables build folder.
    /// Can be passsed to [`Self::load_addressables_bundle`]).
    pub fn addressables_bundles(&self) -> Result<Vec<PathBuf>> {
        let Some(build) = self.addressables_build_folder()? else {
            return Ok(Vec::new());
        };
        Ok(self
            .game_files
            .list_under(&build)?
            .into_iter()
            .filter(|p| p.extension().is_some_and(|ext| ext == "bundle"))
            .filter_map(|p| p.strip_prefix(&build).ok().map(PathBuf::from))
            .collect())
    }

    #[cfg_attr(feature = "tracing-instrument", tracing::instrument(skip_all))]
    pub fn addressables(&self) -> Result<Option<&AddressablesData>> {
        match self.addressables.get() {
            Some(addressables) => Ok(addressables.as_ref()),
            None => {
                let data = AddressablesData::read(self)?;
                let cache = self.addressables.get_or_init(|| data).as_ref();
                Ok(cache)
            }
        }
    }
}

impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    /// Generate the MonoBehaviour type tree for `full_name` (e.g.
    /// `Some.Namespace.Foo`) defined in `assembly` (e.g. `Assembly-CSharp.dll`),
    /// May return `None` if the type can't be resolved (e.g. editor-only)
    pub fn generate_typetree(
        &self,
        assembly: &str,
        full_name: &str,
    ) -> Result<Option<&TypeTreeNode>> {
        self.typetree_generator
            .backend(self)?
            .generate(assembly, full_name)
    }
}
