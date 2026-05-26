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
use rabex::files::serializedfile::ObjectRef;
use rabex::objects::{ClassId, PPtr, TypedPPtr};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use typetree_generator_api::{GeneratorBackend, TypeTreeGenerator};

use crate::addressables::settings::AddressablesSettings;
use crate::addressables::{AddressablesData, ArchivePath};
use crate::game_files::GameFiles;
use crate::handle::SerializedFileHandle;
use crate::resolver::EnvResolver;
use crate::typetree_generator_cache::TypeTreeGeneratorCache;
use crate::unity::types::{BuildSettings, MonoBehaviour, MonoScript, ResourceManager};

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

    /// Initializes [`Environment::typetree_generator`] from the `Managed` DLLs.
    /// Requires `libTypeTreeGenerator.so`/`TypeTreeGenerator.dll` next to the executing binary.
    ///
    /// Only available with the `GameFiles` resolver — the underlying
    /// [`TypeTreeGenerator::load_all_dll_in_dir`] does its own
    /// filesystem walk + per-DLL `File::open`, which can't be
    /// expressed via the resolver trait. Depot-backed environments
    /// that want script typetrees will need a separate API path.
    pub fn load_typetree_generator(&mut self, backend: GeneratorBackend) -> Result<()> {
        let unity_version = self.unity_version()?;
        let generator = TypeTreeGenerator::new_lib_next_to_exe(unity_version, backend)?;
        generator.load_all_dll_in_dir(self.game_files.game_dir.join("Managed"))?;
        let base_node = self
            .tpk
            .get_typetree_node(ClassId::MonoBehaviour, unity_version)
            .expect("missing MonoBehaviour class");
        self.typetree_generator = TypeTreeGeneratorCache::new(generator, base_node.into_owned());

        Ok(())
    }
}

#[derive(Debug)]
pub struct AppInfo {
    pub developer: String,
    pub name: String,
}
impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    pub fn app_info(&self) -> Result<AppInfo> {
        let data = self
            .game_files
            .read_path(Path::new("app.info"))
            .context("could not find app.info")?;
        let contents = std::str::from_utf8(data.as_ref())
            .context("app.info is not valid UTF-8")?;
        let (developer, name) = contents.split_once('\n').context("app.info is malformed")?;

        Ok(AppInfo {
            developer: developer.to_owned(),
            name: name.to_owned(),
        })
    }

    /// bundle is relative to the addressables build folder.
    /// Bytes come through the resolver — same code path for
    /// filesystem-backed `GameFiles` and depot-backed resolvers.
    pub fn load_addressables_bundle(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<BundleFileReader<Cursor<Data>>> {
        let aa_build = self
            .addressables_build_folder()?
            .context("no addressables settings found")?;
        let bundle_rel = aa_build.join(bundle);
        let data = self
            .game_files
            .read_path(&bundle_rel)
            .with_context(|| format!("read bundle {}", bundle_rel.display()))?;
        // `Data` is `AsRef<[u8]>` (Mmap or owned Vec), so `Cursor<Data>`
        // is `Read + Seek` directly — no per-bundle copy.
        let reader = BundleFileReader::from_reader(
            Cursor::new(data),
            &ExtractionConfig::default().with_fallback_unity_version(self.unity_version()?.clone()),
        )?;
        Ok(reader)
    }

    pub fn load_addressables_bundle_content(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        let (archive_name, file, data) = self.load_addressables_bundle_content_leaf(bundle)?;
        let archive_path = ArchivePath::same(&archive_name);
        Ok(self.insert_cache(archive_path.to_string().into(), file, Data::InMemory(data)))
    }

    pub fn load_addressables_bundle_content_leaf(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<(String, SerializedFile, Vec<u8>)> {
        let bundle = self
            .load_addressables_bundle(bundle.as_ref())
            .with_context(|| format!("Failed to load bundle '{}'", bundle.as_ref().display()))?;
        let mut file = bundle_main_serializedfile(&bundle)?;
        file.1
            .m_UnityVersion
            .get_or_insert(self.unity_version()?.clone());
        Ok(file)
    }

    /// Path to the addressables build folder. Returned **relative to
    /// the resolver root** so callers can pass it back into
    /// `env.game_files.read_path` / `list_under` without caring about
    /// the actual filesystem layout (which doesn't exist for depot-
    /// backed environments anyway).
    pub fn addressables_build_folder(&self) -> Result<Option<PathBuf>> {
        let Some(settings) = self.addressables_settings()? else {
            return Ok(None);
        };
        Ok(Some(
            Path::new("StreamingAssets/aa").join(&settings.m_buildTarget),
        ))
    }

    pub fn addressables_settings(&self) -> Result<Option<&AddressablesSettings>> {
        Ok(self.addressables()?.map(|x| &x.settings))
    }

    /// All `.bundle` files under the addressables build folder.
    /// Paths are **relative to the build folder** (passable straight
    /// to [`load_addressables_bundle`]).
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

impl<R: EnvResolver + Send + Sync, P: TypeTreeProvider + Send + Sync> Environment<R, P> {
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

impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    pub fn unity_version(&self) -> Result<&UnityVersion> {
        match self.unity_version.get() {
            Some(unity_version) => Ok(unity_version),
            None => {
                let ggm = self.load_cached("globalgamemanagers")?;
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

    pub fn build_settings(&self) -> Result<BuildSettings> {
        let ggm = self.load_cached("globalgamemanagers")?;
        ggm.find_object_of::<BuildSettings>()
            .transpose()
            .context("no BuildSettings found in globalgamemanagers")
            .flatten()
    }

    pub fn resource_manager(&self) -> Result<ResourceManager> {
        let ggm = self.load_cached("globalgamemanagers")?;
        ggm.find_object_of::<ResourceManager>()
            .transpose()
            .context("no ResourceManager found in globalgamemanagers")
            .flatten()
    }

    pub fn load_leaf(&self, relative_path: impl AsRef<Path>) -> Result<(SerializedFile, Data)> {
        let data = self.game_files.read_path(relative_path.as_ref())?;
        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_ref()))?;
        Ok((file, data))
    }

    pub fn load_cached(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        self.load_external_file(relative_path.as_ref())
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

    pub fn load_external_file(&self, path_name: &Path) -> Result<SerializedFileHandle<'_, R, P>> {
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

    pub fn deref_read_untyped<'de, T>(
        &self,
        pptr: PPtr,
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

    pub fn deref_read<'de, T>(
        &self,
        pptr: TypedPPtr<T>,
        file: &SerializedFile,
        reader: &mut (impl Read + Seek),
    ) -> Result<T>
    where
        T: serde::Deserialize<'de>,
    {
        self.deref_read_untyped(pptr.untyped(), file, reader)
    }

    pub fn load_typetree_as<'a, T>(
        &'a self,
        mb_obj: &ObjectRef<'a, MonoBehaviour>,
        script: &MonoScript,
    ) -> Result<ObjectRef<'a, T>> {
        let tt = self
            .typetree_generator
            .generate(&script.assembly_name(), &script.full_name());
        let tt = tt?;
        let data = mb_obj.with_typetree::<T>(tt);
        Ok(data)
    }

    pub fn loaded_files(&mut self) -> impl Iterator<Item = &Path> {
        self.serialized_files.as_mut().keys().map(Deref::deref)
    }
}


pub fn bundle_main_serializedfile<T: AsRef<[u8]>>(
    bundle: &BundleFileReader<Cursor<T>>,
) -> Result<(String, SerializedFile, Vec<u8>)> {
    let entry = bundle
        .serialized_files()
        .find(|file| !file.path.ends_with(".sharedAssets"))
        .context("no non-resource serializedfile in bundle")?;
    let data = bundle.read_at(&entry.path)?.unwrap();
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    Ok((entry.path.clone(), file, data))
}
