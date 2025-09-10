pub mod addressables;
pub mod utils;

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
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

pub use rabex;

pub mod game_files;
pub mod handle;
pub mod resolver;
pub mod unity;

mod typetree_generator_cache;

pub use resolver::EnvResolver;
use typetree_generator_api::{GeneratorBackend, TypeTreeGenerator};

use crate::addressables::AddressablesSettings;
use crate::handle::SerializedFileHandle;
use crate::resolver::BasedirEnvResolver;
use crate::typetree_generator_cache::TypeTreeGeneratorCache;
use crate::unity::types::{BuildSettings, MonoBehaviour, MonoScript};
use game_files::GameFiles;

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

struct AddressablesData {
    settings: AddressablesSettings,
}

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

    pub fn load_addressables_bundle(
        &self,
        bundle: &str,
    ) -> Result<BundleFileReader<Cursor<memmap2::Mmap>>> {
        let addressables = self
            .addressables_build_folder()?
            .context("no addressables settings found")?;
        let costs = addressables.join(bundle);
        let data = unsafe { memmap2::Mmap::map(&File::open(costs)?)? };

        let bundle = BundleFileReader::from_reader(
            Cursor::new(data),
            &ExtractionConfig::new(None, Some(self.unity_version()?)),
        )?;

        Ok(bundle)
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
        Ok(self.addressables_cache()?.map(|x| &x.settings))
    }

    fn addressables_cache(&self) -> Result<Option<&AddressablesData>> {
        match self.addressables.get() {
            Some(addressables) => Ok(addressables.as_ref()),
            None => {
                let path = self
                    .resolver
                    .base_dir()
                    .join("StreamingAssets/aa/settings.json");
                if !path.exists() {
                    return Ok(None);
                }
                let reader = BufReader::new(File::open(path)?);
                let settings = serde_json::from_reader(reader)?;

                let cache = AddressablesData { settings };
                Ok(self.addressables.get_or_init(|| Some(cache)).as_ref())
            }
        }
    }
}

impl<R: EnvResolver, P: TypeTreeProvider> Environment<R, P> {
    pub fn unity_version(&self) -> Result<UnityVersion> {
        match self.unity_version.get() {
            Some(unity_version) => Ok(*unity_version),
            None => {
                let ggm = self.load_cached("globalgamemanagers")?;
                let unity_version = ggm.file.m_UnityVersion.expect("missing unity version");
                let _ = self.unity_version.set(unity_version);
                Ok(unity_version)
            }
        }
    }

    pub fn build_settings(&self) -> Result<BuildSettings> {
        let ggm = self.load_cached("globalgamemanagers")?;
        ggm.find_object_of::<BuildSettings>().transpose().unwrap()
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

    fn load_external_file(&self, path_name: &Path) -> Result<SerializedFileHandle<'_, R, P>> {
        Ok(match self.serialized_files.get(path_name) {
            Some((file, data)) => SerializedFileHandle {
                file,
                data: data.as_ref(),
                env: self,
            },
            None => {
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
                SerializedFileHandle {
                    file: &file.0,
                    data: file.1.as_ref(),
                    env: self,
                }
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
        Ok(match pptr.m_FileID {
            0 => pptr.deref_local(file, &self.tpk)?.read(reader)?,
            file_id => {
                let external_info = &file.m_Externals[file_id as usize - 1];
                let external = self.load_external_file(Path::new(&external_info.pathName))?;
                let object = pptr
                    .make_local()
                    .deref_local(external.file, &self.tpk)
                    .with_context(|| {
                        format!("In external {} {}", file_id, external_info.pathName)
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
