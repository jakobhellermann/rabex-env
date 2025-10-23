pub mod addressables;
pub mod utils;

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use elsa::sync::FrozenMap;
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::files::serializedfile::ObjectRef;
use rabex::objects::{ClassId, PPtr, TypedPPtr};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;

pub use rabex;

pub mod game_files;
pub mod handle;
pub mod resolver;
pub mod scene_lookup;
pub mod trace_pptr;
pub mod typetree_generator_cache;
pub mod unity;

pub use resolver::EnvResolver;
use rustc_hash::FxHashMap;
use typetree_generator_api::{GeneratorBackend, TypeTreeGenerator};
use walkdir::WalkDir;

use crate::addressables::AddressablesSettings;
use crate::handle::SerializedFileHandle;
use crate::resolver::BasedirEnvResolver;
use crate::typetree_generator_cache::TypeTreeGeneratorCache;
use crate::unity::types::{BuildSettings, MonoBehaviour, MonoScript, ResourceManager};
use game_files::GameFiles;

pub use typetree_generator_api;

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

pub struct Environment<R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>> {
    pub resolver: R,
    pub tpk: P,
    pub serialized_files: FrozenMap<PathBuf, Box<(SerializedFile, Data)>>,
    pub typetree_generator: TypeTreeGeneratorCache,
    unity_version: OnceLock<UnityVersion>,
    addressables: OnceLock<Option<AddressablesData>>,
}

#[derive(Debug)]
pub struct AddressablesData {
    pub settings: AddressablesSettings,
    pub cab_to_bundle: FxHashMap<String, PathBuf>,
    pub bundle_to_cab: FxHashMap<PathBuf, Vec<String>>,
}
impl AddressablesData {}

impl<R, P> Environment<R, P> {
    pub fn new(resolver: R, tpk: P) -> Self {
        Environment {
            resolver,
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
            resolver: GameFiles::probe(path.as_ref())?,
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
impl<R: BasedirEnvResolver, P: TypeTreeProvider> Environment<R, P> {
    /// Initializes [`Environment::typetree_generator`] from the `Managed` DLLs.
    /// Requires `libTypeTreeGenerator.so`/`TypeTreeGenerator.dll` next to the executing binary.
    pub fn load_typetree_generator(&mut self, backend: GeneratorBackend) -> Result<()> {
        let unity_version = self.unity_version()?;
        let generator = TypeTreeGenerator::new_lib_next_to_exe(unity_version, backend)?;
        generator.load_all_dll_in_dir(self.resolver.base_dir().join("Managed"))?;
        let base_node = self
            .tpk
            .get_typetree_node(ClassId::MonoBehaviour, unity_version)
            .expect("missing MonoBehaviour class");
        self.typetree_generator = TypeTreeGeneratorCache::new(generator, base_node.into_owned());

        Ok(())
    }

    pub fn app_info(&self) -> Result<AppInfo> {
        let path = self.resolver.base_dir().join("app.info");
        let contents = std::fs::read_to_string(path).context("could not find app.info")?;
        let (developer, name) = contents.split_once('\n').context("app.info is malformed")?;

        Ok(AppInfo {
            developer: developer.to_owned(),
            name: name.to_owned(),
        })
    }

    /// bundle is relative to the adressables build folder (or absolute)
    pub fn load_addressables_bundle(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<BundleFileReader<Cursor<memmap2::Mmap>>> {
        let aa_build = self
            .addressables_build_folder()?
            .context("no addressables settings found")?;

        let bundle = aa_build.join(bundle);
        load_addressables_bundle_inner(&bundle, self.unity_version()?)
    }

    pub fn load_addressables_bundle_content(
        &self,
        bundle: impl AsRef<Path>,
    ) -> Result<SerializedFileHandle<'_, R, P>> {
        let (archive_name, file, data) = self.load_addressables_bundle_content_leaf(bundle)?;
        Ok(self.insert_cache(
            addressables::wrap_archive(&archive_name).into(),
            file,
            Data::InMemory(data),
        ))
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

    pub fn addressables_build_folder(&self) -> Result<Option<PathBuf>> {
        let Some(settings) = self.addressables_settings()? else {
            return Ok(None);
        };

        let path = self
            .resolver
            .base_dir()
            .join("StreamingAssets/aa")
            .join(&settings.m_buildTarget);

        Ok(Some(path))
    }

    pub fn addressables_settings(&self) -> Result<Option<&AddressablesSettings>> {
        Ok(self.addressables()?.map(|x| &x.settings))
    }

    pub fn addressables_bundles(&self) -> impl Iterator<Item = PathBuf> {
        let build_folder = self.addressables_build_folder().ok().flatten();
        build_folder
            .map(|aa| {
                WalkDir::new(aa)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter_map(|x| {
                        if x.file_type().is_dir() {
                            return None;
                        }
                        if !x.path().extension().is_some_and(|ext| ext == "bundle") {
                            return None;
                        }
                        Some(x.into_path())
                    })
            })
            .into_iter()
            .flatten()
    }

    pub fn addressables(&self) -> Result<Option<&AddressablesData>> {
        match self.addressables.get() {
            Some(addressables) => Ok(addressables.as_ref()),
            None => {
                let base_dir = self.resolver.base_dir();
                let aa = base_dir.join("StreamingAssets/aa");
                if !aa.exists() {
                    return Ok(None);
                }
                let reader = BufReader::new(
                    File::open(aa.join("settings.json")).context("no settings.json")?,
                );
                let settings: AddressablesSettings = serde_json::from_reader(reader)?;

                let lookup = addressables_bundle_lookup(
                    &aa.join(&settings.m_buildTarget),
                    self.unity_version()?,
                )
                .context("could not determine CAB locations")?;
                let cache = AddressablesData {
                    settings,
                    cab_to_bundle: lookup.0,
                    bundle_to_cab: lookup.1,
                };
                let cache = self.addressables.get_or_init(|| Some(cache)).as_ref();
                Ok(cache)
            }
        }
    }
}

impl<R: BasedirEnvResolver, P: TypeTreeProvider> Environment<R, P> {
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
        let data = self.resolver.read_path(relative_path.as_ref())?;
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

    fn load_external_file(&self, path_name: &Path) -> Result<SerializedFileHandle<'_, R, P>> {
        Ok(match self.serialized_files.get(path_name) {
            Some((file, data)) => SerializedFileHandle {
                file,
                data: data.as_ref(),
                env: self,
            },
            None => {
                if let Some(cab) = addressables::unwrap_archive(path_name) {
                    let aa = self.addressables()?.context(
                        "Can't use archive:/ external files without addressables in the game",
                    )?;
                    let cab_bundle = aa
                        .cab_to_bundle
                        .get(cab)
                        .with_context(|| format!("CAB {} doesn't exist", cab))?;
                    let bundle = self.load_addressables_bundle(cab_bundle)?;
                    let cab_data = bundle.read_at(cab)?.expect("cab unexpectedly not present");

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
                    .resolver
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

fn addressables_bundle_lookup(
    aa_build: &Path,
    unity_version: &UnityVersion,
) -> Result<(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)> {
    use rayon::prelude::*;

    WalkDir::new(aa_build)
        .into_iter()
        .par_bridge()
        .try_fold(
            <(FxHashMap<String, PathBuf>, FxHashMap<PathBuf, Vec<String>>)>::default,
            |mut acc, path| -> Result<_> {
                let path = path?;
                if path.file_type().is_dir() {
                    return Ok(acc);
                }
                let relative = path.path().strip_prefix(aa_build).unwrap();

                let bundle_to_cab_files: &mut Vec<_> =
                    acc.1.entry(relative.to_owned()).or_default();

                // PERF: (tested with 2k bundles on silksong)
                // seq + mmap: 27ms
                // seq + read: 18ms
                // par + mmap: 20ms
                // par + read: 7ms
                let bundle = BundleFileReader::from_reader(
                    BufReader::new(File::open(path.path())?),
                    &ExtractionConfig::new(None, Some(unity_version.clone())),
                )?;

                for file in bundle.files() {
                    acc.0.insert(file.path.clone(), relative.to_owned());
                    bundle_to_cab_files.push(file.path.clone());
                }

                Ok(acc)
            },
        )
        .try_reduce(Default::default, |mut acc, item| {
            acc.0.extend(item.0);
            acc.1.extend(item.1);
            Ok(acc)
        })
}

fn load_addressables_bundle_inner(
    bundle: &Path,
    unity_version: &UnityVersion,
) -> Result<BundleFileReader<Cursor<memmap2::Mmap>>> {
    let file = File::open(bundle)?;
    if file.metadata()?.is_dir() {
        bail!(
            "Attempted to load directory '{}' as assetbundle",
            bundle.display()
        );
    }
    let data = unsafe { memmap2::Mmap::map(&file)? };

    let bundle = BundleFileReader::from_reader(
        Cursor::new(data),
        &ExtractionConfig::new(None, Some(unity_version.clone())),
    )?;

    Ok(bundle)
}

fn bundle_main_serializedfile<T: AsRef<[u8]>>(
    bundle: &BundleFileReader<Cursor<T>>,
) -> Result<(String, SerializedFile, Vec<u8>)> {
    let entry = bundle
        .files()
        .iter()
        .find(|file| (file.flags & 4) != 0 && !file.path.ends_with(".sharedAssets"))
        .context("no non-resource serializedfile in bundle")?;
    let data = bundle.read_at(&entry.path)?.unwrap();
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    Ok((entry.path.clone(), file, data))
}
