// TODO: check string caching
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use rustc_hash::FxHashMap;

#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssemblyClass {
    pub m_AssemblyName: Arc<String>,
    pub m_ClassName: Arc<String>,
}
impl AssemblyClass {
    pub fn class_name(&self) -> &str {
        self.m_ClassName
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(&*self.m_ClassName)
    }
}

impl AssemblyClass {
    fn from_reader<R: Read + Seek>(
        mut reader: R,
        cache: &mut Cache,
    ) -> Result<Self, std::io::Error> {
        let reader = &mut reader;
        let assembly_name = read_u32(reader)?;
        let class_name = read_u32(reader)?;
        let assembly_name = read_encoded_string_sep(reader, cache, assembly_name, '.')?;
        let class_name = read_encoded_string_sep(reader, cache, class_name, '.')?;
        Ok(AssemblyClass {
            m_AssemblyName: assembly_name,
            m_ClassName: class_name,
        })
    }

    fn get_match_name(&self) -> String {
        format!("{}; {}", self.get_assembly_short_name(), self.m_ClassName)
    }
    fn get_assembly_short_name(&self) -> &str {
        assert!(self.m_AssemblyName.contains(','));
        self.m_AssemblyName.split(',').next().unwrap()
    }
}

#[derive(Debug)]
pub struct ObjectInitializationData {
    pub id: Arc<String>,
    pub object_type: AssemblyClass,
    pub data: Arc<String>,
}
impl ObjectInitializationData {
    fn from_reader<R: Read + Seek>(
        reader: &mut R,
        cache: &mut Cache,
    ) -> Result<Self, std::io::Error> {
        let id_offset = read_u32(reader)?;
        let object_type_offset = read_u32(reader)?;
        let data_offset = read_u32(reader)?;

        let id = read_encoded_string(reader, cache, id_offset)?;
        reader.seek(SeekFrom::Start(object_type_offset as u64))?;
        let object_type = AssemblyClass::from_reader(&mut *reader, cache)?;
        let data = read_encoded_string(reader, cache, data_offset)?;

        Ok(ObjectInitializationData {
            id,
            object_type,
            data,
        })
    }
}

#[derive(Debug)]
pub struct ResourceLocationHeader {
    pub primary_key_offset: u32,
    pub internal_id_offset: u32,
    pub provider_id_offset: u32,
    pub dependencies_offset: u32,
    pub dependency_hash_code: i32,
    pub data_offset: u32,
    pub type_offset: u32,
}
impl ResourceLocationHeader {
    fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self, std::io::Error> {
        let primary_key_offset = read_u32(reader)?;
        let internal_id_offset = read_u32(reader)?;
        let provider_id_offset = read_u32(reader)?;
        let dependencies_offset = read_u32(reader)?;
        let dependency_hash_code = read_i32(reader)?;
        let data_offset = read_u32(reader)?;
        let type_offset = read_u32(reader)?;
        Ok(ResourceLocationHeader {
            primary_key_offset,
            internal_id_offset,
            provider_id_offset,
            dependencies_offset,
            dependency_hash_code,
            data_offset,
            type_offset,
        })
    }

    pub fn primary_key<R: Read + Seek>(
        &self,
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<Arc<String>, std::io::Error> {
        catalog.read_encoded_string_sep(self.primary_key_offset, '/')
    }

    pub fn provider_id<R: Read + Seek>(
        &self,
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<Arc<String>, std::io::Error> {
        catalog.read_encoded_string_sep(self.provider_id_offset, '.')
    }

    pub fn internal_id<R: Read + Seek>(
        &self,
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<Arc<String>, std::io::Error> {
        catalog.read_encoded_string_sep(self.internal_id_offset, '/')
    }

    pub fn data<R: Read + Seek>(
        &self,
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<Option<Value>, std::io::Error> {
        catalog.decode_v2(self.data_offset)
    }

    pub fn r#type<R: Read + Seek>(
        &self,
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<AssemblyClass, std::io::Error> {
        catalog
            .reader
            .seek(SeekFrom::Start(self.type_offset as u64))?;
        AssemblyClass::from_reader(&mut catalog.reader, &mut catalog.cache)
    }
}

/// `provider_id` is usually one of:
/// - `UnityEngine.ResourceManagement.ResourceProviders.AssetBundleProvider`
///   - `internal_id` is the runtime path
///   - `data` is of type `AssetBundleRequestOptions` (`.bundle_name` is AssetBundle.m_Name, *not* the archive filename)
///   - `primary_key` is the mixture of path and unknown
/// - `UnityEngine.ResourceManagement.ResourceProviders.BundledAssetProvider`
///   - `internal_id` is editor path
///   - `primary_key` is the path from the editor I think
///   - `data` seems to be None
///   - `type_` varies
#[derive(PartialEq)]
pub struct ResourceLocation {
    pub internal_id: Arc<String>,
    /// - `AssetBundleProvider`
    /// - `AtlasSpriteProvider`
    /// - `BinaryAssetProvider<TAdapter>`
    /// - `BinaryDataProvider`
    /// - `BundledAssetProvider`
    /// - `ContentCatalogProvider`
    /// - `JsonAssetProvider`
    /// - `TextDataProvider`
    pub provider_id: Arc<String>,
    pub dependencies: Vec<Arc<ResourceLocation>>,
    pub data: Option<AssetBundleRequestOptions>,
    pub dependency_hash_code: i32,
    /// Used in `Addressables.LoadAssetAsync<GameObject>(primaryKey)`
    pub primary_key: Arc<String>,
    pub type_: AssemblyClass,
}

impl std::fmt::Debug for ResourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceLocation")
            .field("internal_id", &self.internal_id)
            .field("provider_id", &self.provider_id)
            // .field("dependencies", &self.dependencies)
            .field(
                "dependencies",
                &format!("{} dependencies", self.dependencies.len()),
            )
            // .field("data", &self.data)
            // .field("dependency_hash_code", &self.dependency_hash_code)
            .field("primary_key", &self.primary_key)
            .field("type_", &self.type_)
            .finish()
    }
}
impl ResourceLocation {
    pub fn provider_name(&self) -> &str {
        self.provider_id
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(self.provider_id.as_ref())
    }
    fn from_catalog_here<R: Read + Seek>(
        catalog: &mut BinaryCatalogReader<R>,
    ) -> Result<Self, std::io::Error> {
        let header = ResourceLocationHeader::from_reader(&mut catalog.reader)?;
        let primary_key = header.primary_key(catalog)?;
        let internal_id = header.internal_id(catalog)?;
        let provider_id = header.provider_id(catalog)?;

        let dependency_offsets = catalog.read_offset_array(header.dependencies_offset)?;
        let dependencies = dependency_offsets
            .iter()
            .map(|&offset| catalog.get_ot_read_resource(offset))
            .collect::<Result<Vec<_>, _>>()?;

        let data = header.data(catalog)?.map(|ty| ty.into_abro().unwrap());
        let r#type = header.r#type(catalog)?;

        Ok(ResourceLocation {
            internal_id,
            provider_id,
            dependencies,
            data,
            dependency_hash_code: header.dependency_hash_code,
            primary_key,
            type_: r#type,
        })
    }
}
#[derive(Debug)]
pub struct BinaryCatalog {
    pub locator_id: Arc<String>,
    pub build_result_hash: Arc<String>,
    pub instance_provider_data: ObjectInitializationData,
    pub scene_provider_data: ObjectInitializationData,
    pub resource_provider_data: Vec<ObjectInitializationData>,
    pub resources: HashMap<Arc<String>, Vec<Arc<ResourceLocation>>>,
}

struct BinaryCatalogHeader {
    keys_offset: u32,
    id_offset: u32,
    instance_provider_offset: u32,
    scene_provider_offset: u32,
    init_objects_array_offset: u32,
    build_result_hash_offset: u32,
}
impl BinaryCatalogHeader {
    fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self, std::io::Error> {
        let magic = read_i32(reader)?;
        if magic != 0xde38942 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unexpected magic for addressables catalog: {}", magic),
            ));
        }
        let version = read_i32(reader)?;
        if version != 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unsuppported version {}", version),
            ));
        }

        let keys_offset = read_u32(reader)?;
        let id_offset = read_u32(reader)?;
        let instance_provider_offset = read_u32(reader)?;
        let scene_provider_offset = read_u32(reader)?;
        let init_objects_array_offset = read_u32(reader)?;
        let build_result_hash_offset = read_u32(reader)?; // v2 only

        Ok(BinaryCatalogHeader {
            keys_offset,
            id_offset,
            instance_provider_offset,
            scene_provider_offset,
            init_objects_array_offset,
            build_result_hash_offset,
        })
    }
}

struct Cache {
    strings: FxHashMap<u32, Arc<String>>,
    locations: FxHashMap<u32, Arc<ResourceLocation>>,
}

pub struct BinaryCatalogReader<R> {
    reader: R,
    header: BinaryCatalogHeader,
    cache: Cache,
}

impl BinaryCatalog {
    pub fn from_reader<R: Read + Seek>(reader: R) -> Result<Self, std::io::Error> {
        BinaryCatalogReader::new(reader)?.read()
    }

    pub fn locations(&self) -> impl Iterator<Item = &ResourceLocation> {
        self.resources
            .iter()
            .flat_map(|(_, locations)| locations.iter().map(Arc::as_ref))
    }

    pub fn locations_of_provider(
        &self,
        provider_id: &str,
    ) -> impl Iterator<Item = &ResourceLocation> {
        self.locations()
            .filter(move |item| *item.provider_id == provider_id)
    }
}

impl<R: Read + Seek> BinaryCatalogReader<R> {
    pub fn new(mut reader: R) -> Result<Self, std::io::Error> {
        let header = BinaryCatalogHeader::from_reader(&mut reader)?;
        let cache = Cache {
            strings: HashMap::default(),
            locations: HashMap::default(),
        };
        Ok(BinaryCatalogReader {
            reader,
            header,
            cache,
        })
    }
    fn read_encoded_string(&mut self, encoded_offset: u32) -> Result<Arc<String>, std::io::Error> {
        read_encoded_string(&mut self.reader, &mut self.cache, encoded_offset)
    }
    fn read_encoded_string_sep(
        &mut self,
        encoded_offset: u32,
        dynamic_string_separator: char,
    ) -> Result<Arc<String>, std::io::Error> {
        read_encoded_string_sep(
            &mut self.reader,
            &mut self.cache,
            encoded_offset,
            dynamic_string_separator,
        )
    }
    fn decode_v2(&mut self, offset: u32) -> Result<Option<Value>, std::io::Error> {
        decode_v2(&mut self.reader, &mut self.cache, offset)
    }

    pub fn location_headers(&mut self) -> Result<Vec<ResourceLocationHeader>, std::io::Error> {
        let key_location_offsets = self.read_offset_array(self.header.keys_offset)?;

        let mut resources = Vec::new();
        for i in (0..key_location_offsets.len()).step_by(2) {
            let _key_offset = key_location_offsets[i];
            let location_list_offset = key_location_offsets[i + 1];

            let location_offsets = self.read_offset_array(location_list_offset)?;
            for &location in location_offsets.as_slice() {
                self.reader.seek(SeekFrom::Start(location as u64))?;
                let location = ResourceLocationHeader::from_reader(&mut self.reader)?;
                resources.push(location);
            }
        }
        Ok(resources)
    }

    pub fn assetbundle_names(&mut self) -> Result<Vec<(String, String)>, std::io::Error> {
        let key_location_offsets = self.read_offset_array(self.header.keys_offset)?;

        let mut resources = Vec::new();
        for i in (0..key_location_offsets.len()).step_by(2) {
            let _key_offset = key_location_offsets[i];
            let location_list_offset = key_location_offsets[i + 1];

            let location_offsets = self.read_offset_array(location_list_offset)?;
            for &location in location_offsets.as_slice() {
                self.reader.seek(SeekFrom::Start(location as u64))?;
                let location = ResourceLocationHeader::from_reader(&mut self.reader)?;
                let provider_id = location.provider_id(self)?;
                if *provider_id
                    != "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleProvider"
                {
                    continue;
                }
                let internal_id = location.internal_id(self)?;
                let abro = location
                    .data(self)?
                    .expect("no data for AssetBundleProvider")
                    .into_abro()
                    .unwrap();
                let path = internal_id
                    .strip_prefix("{UnityEngine.AddressableAssets.Addressables.RuntimePath}")
                    .expect("expected RuntimePath placeholder in provider ID")
                    .trim_start_matches(['/', '\\']);

                resources.push((path.to_owned(), (*abro.bundle_name).clone()));
            }
        }
        Ok(resources)
    }

    pub fn read(&mut self) -> Result<BinaryCatalog, std::io::Error> {
        let locator_id = self.read_encoded_string(self.header.id_offset)?;
        let build_result_hash = self.read_encoded_string(self.header.build_result_hash_offset)?;

        self.reader
            .seek(SeekFrom::Start(self.header.instance_provider_offset as u64))?;
        let instance_provider_data =
            ObjectInitializationData::from_reader(&mut self.reader, &mut self.cache)?;

        self.reader
            .seek(SeekFrom::Start(self.header.scene_provider_offset as u64))?;
        let scene_provider_data =
            ObjectInitializationData::from_reader(&mut self.reader, &mut self.cache)?;

        let resource_provider_data_offsets =
            self.read_offset_array(self.header.init_objects_array_offset)?;
        let resource_provider_data = resource_provider_data_offsets
            .iter()
            .map(|&offset| {
                self.reader.seek(SeekFrom::Start(offset as u64))?;
                ObjectInitializationData::from_reader(&mut self.reader, &mut self.cache)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let resources = {
            let key_location_offsets = self.read_offset_array(self.header.keys_offset)?;

            let mut resources = HashMap::default();
            for i in (0..key_location_offsets.len()).step_by(2) {
                let key_offset = key_location_offsets[i];
                let location_list_offset = key_location_offsets[i + 1];

                let key = self
                    .decode_v2(key_offset)?
                    .expect("expected null resource key")
                    .into_string()
                    .expect("unexpected non-string resource key");
                let location_offsets = self.read_offset_array(location_list_offset)?;
                let locations = location_offsets
                    .iter()
                    .map(|&offset| self.get_ot_read_resource(offset))
                    .collect::<Result<Vec<_>, _>>()?;
                resources.insert(key, locations);
            }
            resources
        };

        Ok(BinaryCatalog {
            locator_id,
            build_result_hash,
            instance_provider_data,
            scene_provider_data,
            resource_provider_data,
            resources,
        })
    }

    fn get_ot_read_resource(
        &mut self,
        offset: u32,
    ) -> Result<Arc<ResourceLocation>, std::io::Error> {
        if let Some(cached) = self.cache.locations.get(&offset) {
            return Ok(Arc::clone(cached));
        }
        self.reader.seek(SeekFrom::Start(offset as u64))?;
        let location = Arc::new(ResourceLocation::from_catalog_here(self)?);

        self.cache.locations.insert(offset, Arc::clone(&location));
        Ok(location)
    }

    fn read_offset_array(&mut self, encoded_offset: u32) -> Result<Vec<u32>, std::io::Error> {
        read_offset_array(&mut self.reader, encoded_offset)
    }
}

fn read_i16<R: Read>(reader: &mut R) -> Result<i16, std::io::Error> {
    let mut buf = [0; _];
    reader.read_exact(&mut buf)?;
    Ok(i16::from_le_bytes(buf))
}

fn read_i32<R: Read>(reader: &mut R) -> Result<i32, std::io::Error> {
    let mut buf = [0; _];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, std::io::Error> {
    let mut buf = [0; _];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}
fn read_i64<R: Read>(reader: &mut R) -> std::io::Result<i64> {
    let mut buf = [0; 8];
    reader.read_exact(&mut buf)?;
    Ok(i64::from_le_bytes(buf))
}
fn read_bool<R: Read>(reader: &mut R) -> std::io::Result<bool> {
    let mut buf = [0; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0] != 0)
}
fn read_u8<R: Read>(reader: &mut R) -> std::io::Result<u8> {
    let mut buf = [0; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}
fn read_char<R: Read>(reader: &mut R) -> std::io::Result<char> {
    let mut buf = [0; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0] as char)
}

fn read_encoded_string<R: Read + Seek>(
    reader: &mut R,
    cache: &mut Cache,
    encoded_offset: u32,
) -> Result<Arc<String>, std::io::Error> {
    read_encoded_string_sep(reader, cache, encoded_offset, '\0')
}

fn read_encoded_string_sep<R: Read + Seek>(
    reader: &mut R,
    cache: &mut Cache,
    encoded_offset: u32,
    dynamic_string_separator: char,
) -> Result<Arc<String>, std::io::Error> {
    if let Some(cached) = cache.strings.get(&encoded_offset) {
        return Ok(Arc::clone(cached));
    }

    if encoded_offset == u32::MAX {
        return Ok(Arc::new(String::new()));
    }

    let unicode = (encoded_offset & 0x80000000) != 0;
    let dynamic_string = (encoded_offset & 0x40000000) != 0 && dynamic_string_separator != '\0';
    let offset = encoded_offset & 0x3fffffff;

    let result = if dynamic_string {
        read_segmented_string(reader, cache, offset, unicode, dynamic_string_separator)?
    } else {
        read_basic_string(reader, offset, unicode)?
    };

    let result = Arc::new(result);
    cache.strings.insert(encoded_offset, Arc::clone(&result));

    Ok(result)
}

fn read_basic_string<R: Read + Seek>(
    reader: &mut R,
    offset: u32,
    _unicode: bool,
) -> Result<String, std::io::Error> {
    reader.seek(SeekFrom::Start(offset as u64 - 4))?;
    let length = read_i32(reader)?;
    let mut buf = vec![0; length as usize];
    reader.read_exact(&mut buf)?;

    let str = String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(str)
}

fn read_segmented_string<R: Read + Seek>(
    reader: &mut R,
    cache: &mut Cache,
    offset: u32,
    _unicode: bool,
    separator: char,
) -> Result<String, std::io::Error> {
    reader.seek(SeekFrom::Start(offset as u64))?;

    let mut parts = Vec::new();
    loop {
        let part_offset = read_u32(reader)?;
        let next_part_offset = read_u32(reader)?;
        parts.push(read_encoded_string(reader, cache, part_offset)?);

        if next_part_offset == u32::MAX {
            break;
        }

        reader.seek(SeekFrom::Start(next_part_offset as u64))?;
    }
    parts.reverse();
    let parts = parts.iter().map(|part| part.as_str()).collect::<Vec<_>>(); // TODO:perf
    Ok(parts.join(&separator.to_string()))
}

fn read_offset_array<R: Read + Seek>(
    reader: &mut R,
    encoded_offset: u32,
) -> Result<Vec<u32>, std::io::Error> {
    if encoded_offset == u32::MAX {
        return Ok(vec![]);
    }
    reader.seek(SeekFrom::Start(encoded_offset as u64 - 4))?;
    let byte_size = read_i32(reader)?;
    if byte_size % 4 != 0 {
        unreachable!();
    }
    let elem_count = byte_size / 4;
    // PERF: read + reinterpret
    let mut results = vec![0; elem_count as usize];
    for i in 0..elem_count {
        results[i as usize] = read_u32(reader)?;
    }

    Ok(results)
}

//
//

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash128(u32, u32, u32, u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetBundleRequestOptions {
    pub hash: Hash128,
    pub crc: u32,
    pub common_info: CommonInfo,
    pub bundle_name: Arc<String>,
    pub bundle_size: u32,
}
impl AssetBundleRequestOptions {
    fn from_reader<R: Read + Seek>(
        reader: &mut R,
        cache: &mut Cache,
    ) -> Result<Self, std::io::Error> {
        let hash_offset = read_u32(reader)?;
        let bundle_name_offset = read_u32(reader)?;
        let crc = read_u32(reader)?;
        let bundle_size = read_u32(reader)?;
        let common_info_offset = read_u32(reader)?;

        reader.seek(SeekFrom::Start(hash_offset as u64))?;
        let hash_v0 = read_u32(reader)?;
        let hash_v1 = read_u32(reader)?;
        let hash_v2 = read_u32(reader)?;
        let hash_v3 = read_u32(reader)?;
        let hash = Hash128(hash_v0, hash_v1, hash_v2, hash_v3);

        let bundle_name = read_encoded_string_sep(reader, cache, bundle_name_offset, '_')?;
        // Crc = crc;
        // BundleSize = bundleSize;

        reader.seek(SeekFrom::Start(common_info_offset as u64))?;
        let common_info = CommonInfo::from_reader(reader)?;

        Ok(AssetBundleRequestOptions {
            hash,
            crc,
            common_info,
            bundle_name,
            bundle_size,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommonInfo {
    pub timeout: i16,
    pub redirect_limit: u8,
    pub retry_count: u8,
    pub flags: i32,
}
impl CommonInfo {
    fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self, std::io::Error> {
        let timeout = read_i16(reader)?;
        let redirect_limit = read_u8(reader)?;
        let retry_count = read_u8(reader)?;
        let flags = read_i32(reader)?;
        Ok(CommonInfo {
            timeout,
            redirect_limit,
            retry_count,
            flags,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Int(i32),
    Long(i64),
    Bool(bool),
    String(Arc<String>),
    Hash128(Hash128),
    Abro(AssemblyClass, AssetBundleRequestOptions),
}

impl Value {
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(str) => Some(&**str),
            _ => None,
        }
    }
    pub fn into_string(self) -> Option<Arc<String>> {
        match self {
            Value::String(str) => Some(str),
            _ => None,
        }
    }
    pub fn into_abro(self) -> Option<AssetBundleRequestOptions> {
        match self {
            Value::Abro(_, abro) => Some(abro),
            _ => None,
        }
    }
}

fn decode_v2<R: Read + Seek>(
    reader: &mut R,
    cache: &mut Cache,
    offset: u32,
) -> Result<Option<Value>, std::io::Error> {
    const INT_MATCHNAME: &str = "mscorlib; System.Int32";
    const LONG_MATCHNAME: &str = "mscorlib; System.Int64";
    const BOOL_MATCHNAME: &str = "mscorlib; System.Boolean";
    const STRING_MATCHNAME: &str = "mscorlib; System.String";
    const HASH128_MATCHNAME: &str = "UnityEngine.CoreModule; UnityEngine.Hash128";
    const ABRO_MATCHNAME: &str = "Unity.ResourceManager; UnityEngine.ResourceManagement.ResourceProviders.AssetBundleRequestOptions";

    if offset == u32::MAX {
        return Ok(None);
    }

    reader.seek(SeekFrom::Start(offset as u64))?;
    let type_name_offset = read_u32(reader)?;
    let object_offset = read_u32(reader)?;

    let is_default_object = object_offset == u32::MAX;

    reader.seek(SeekFrom::Start(type_name_offset as u64))?;
    let serialized_type = AssemblyClass::from_reader(&mut *reader, cache)?;
    let match_name = serialized_type.get_match_name();

    match match_name.as_str() {
        INT_MATCHNAME => {
            if is_default_object {
                return Ok(Some(Value::Int(0i32)));
            }
            reader.seek(SeekFrom::Start(object_offset as u64))?;
            let value = read_i32(reader)?;
            Ok(Some(Value::Int(value)))
        }
        LONG_MATCHNAME => {
            if is_default_object {
                return Ok(Some(Value::Long(0i64)));
            }
            reader.seek(SeekFrom::Start(object_offset as u64))?;
            let value = read_i64(reader)?;
            Ok(Some(Value::Long(value)))
        }
        BOOL_MATCHNAME => {
            if is_default_object {
                return Ok(Some(Value::Bool(false)));
            }
            reader.seek(SeekFrom::Start(object_offset as u64))?;
            let value = read_bool(reader)?;
            Ok(Some(Value::Bool(value)))
        }
        STRING_MATCHNAME => {
            if is_default_object {
                return Ok(Some(Value::String(Arc::new(String::new()))));
            }
            reader.seek(SeekFrom::Start(object_offset as u64))?;
            let string_offset = read_u32(reader)?;
            let separator = read_char(reader)?;
            let value = read_encoded_string_sep(reader, cache, string_offset, separator)?;
            Ok(Some(Value::String(value)))
        }
        HASH128_MATCHNAME => {
            if is_default_object {
                return Ok(Some(Value::Hash128(Hash128(0, 0, 0, 0))));
            }
            reader.seek(SeekFrom::Start(object_offset as u64))?;
            let v0 = read_u32(reader)?;
            let v1 = read_u32(reader)?;
            let v2 = read_u32(reader)?;
            let v3 = read_u32(reader)?;
            Ok(Some(Value::Hash128(Hash128(v0, v1, v2, v3))))
        }
        ABRO_MATCHNAME => {
            if is_default_object {
                return Ok(None); // loses type info, can't do much
            }

            let value = {
                reader.seek(SeekFrom::Start(object_offset as u64))?;
                let obj = AssetBundleRequestOptions::from_reader(reader, cache)?;
                Value::Abro(serialized_type, obj)
            };

            Ok(Some(value))
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Unsupported type for deserialization {}", match_name),
        )),
    }
}
