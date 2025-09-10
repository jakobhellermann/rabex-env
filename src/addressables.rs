use std::path::Path;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_derive::Deserialize;

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct AddressablesSettings {
    pub m_buildTarget: String,
    pub m_SettingsHash: String,
    pub m_CatalogLocations: Vec<CatalogLocation>,
    pub m_LogResourceManagerExceptions: bool,
    pub m_ExtraInitializationData: Vec<()>,
    pub m_DisableCatalogUpdateOnStart: bool,
    pub m_IsLocalCatalogInBundle: bool,
    // pub m_CertificateHandlerType: AssemblyClass,
    pub m_AddressablesVersion: String,
    pub m_maxConcurrentWebRequests: u32,
    pub m_CatalogRequestsTimeout: u32,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct CatalogLocation {
    pub m_Keys: Vec<String>,
    pub m_InternalId: String,
    pub m_Provider: String,
    pub m_Dependencies: Vec<()>,
    // pub m_ResourceType: AssemblyClass, TODO
}

#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssemblyClass {
    pub m_AssemblyName: Arc<String>,
    pub m_ClassName: Arc<String>,
}

// archive:/CAB-asdf/CAB-asdf

pub fn wrap_archive(cab: &str) -> String {
    format!("archive:/{cab}/{cab}")
}
pub fn unwrap_archive(path: &Path) -> Option<&str> {
    let path = path.strip_prefix("archive:").ok()?;

    let mut parts = path.iter();
    let first = parts.next()?.to_str()?;
    let second = parts.next()?.to_str()?.trim_end_matches(".sharedAssets"); // maybe?
    if first != second {
        return None;
    }
    Some(second)
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
pub struct ResourceLocation {
    pub internal_id: Arc<String>,
    pub provider_id: Arc<String>,
    pub dependencies: Vec<ResourceLocation>,
    pub data: Option<Value>,
    pub dependency_hash_code: i32,
    pub primary_key: Arc<String>,
    pub type_: AssemblyClass,
}
impl ResourceLocation {
    fn from_reader<R: Read + Seek>(
        reader: &mut R,
        cache: &mut Cache,
    ) -> Result<Self, std::io::Error> {
        let primary_key_offset = read_u32(reader)?;
        let internal_id_offset = read_u32(reader)?;
        let provider_id_offset = read_u32(reader)?;
        let dependencies_offset = read_u32(reader)?;
        let dependency_hash_code = read_i32(reader)?;
        let data_offset = read_u32(reader)?;
        let type_offset = read_u32(reader)?;
        let primary_key = read_encoded_string_sep(reader, cache, primary_key_offset, '/')?;
        let internal_id = read_encoded_string_sep(reader, cache, internal_id_offset, '/')?;
        let provider_id = read_encoded_string_sep(reader, cache, provider_id_offset, '.')?;

        let dependency_offsets = read_offset_array(reader, cache, dependencies_offset)?;
        let dependencies = dependency_offsets
            .into_iter()
            .map(|offset| {
                reader.seek(SeekFrom::Start(offset as u64))?;
                ResourceLocation::from_reader(reader, cache)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let data = decode_v2(reader, cache, data_offset)?;
        reader.seek(SeekFrom::Start(type_offset as u64))?;
        let type_ = AssemblyClass::from_reader(reader, cache)?;

        Ok(ResourceLocation {
            internal_id,
            provider_id,
            dependencies,
            data,
            dependency_hash_code,
            primary_key,
            type_,
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
    pub resources: HashMap<Value, Vec<ResourceLocation>>,
}

impl BinaryCatalog {
    pub fn from_reader<R: Read + Seek>(reader: R) -> Result<Self, std::io::Error> {
        let mut cache = Cache {
            strings: HashMap::default(),
            offsets: HashMap::default(),
        };
        BinaryCatalog::from_reader_inner(reader, &mut cache)
    }
    fn from_reader_inner<R: Read + Seek>(
        mut reader: R,
        cache: &mut Cache,
    ) -> Result<Self, std::io::Error> {
        let reader = &mut reader;

        let _magic = read_i32(reader)?;
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

        let locator_id = read_encoded_string(reader, cache, id_offset)?;
        let build_result_hash = read_encoded_string(reader, cache, build_result_hash_offset)?;

        reader.seek(SeekFrom::Start(instance_provider_offset as u64))?;
        let instance_provider_data = ObjectInitializationData::from_reader(reader, cache)?;

        reader.seek(SeekFrom::Start(scene_provider_offset as u64))?;
        let scene_provider_data = ObjectInitializationData::from_reader(reader, cache)?;

        let resource_provider_data_offsets =
            read_offset_array(reader, cache, init_objects_array_offset)?;
        let resource_provider_data = resource_provider_data_offsets
            .into_iter()
            .map(|offset| {
                reader.seek(SeekFrom::Start(offset as u64))?;
                ObjectInitializationData::from_reader(reader, cache)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let resources = {
            let key_location_offsets = read_offset_array(reader, cache, keys_offset)?;

            let mut resources = HashMap::default();
            for i in (0..key_location_offsets.len()).step_by(2) {
                let key_offset = key_location_offsets[i as usize];
                let location_list_offset = key_location_offsets[i as usize + 1];

                let key = decode_v2(reader, cache, key_offset)?.expect("todo");
                let location_offsets = read_offset_array(reader, cache, location_list_offset)?;
                let locations = location_offsets
                    .into_iter()
                    .map(|offset| {
                        reader.seek(SeekFrom::Start(offset as u64))?;
                        ResourceLocation::from_reader(reader, cache)
                    })
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

struct Cache {
    strings: FxHashMap<u32, Arc<String>>,
    offsets: FxHashMap<u32, Vec<u32>>,
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
        read_dynamic_string(reader, cache, offset, unicode, dynamic_string_separator)?
    } else {
        read_basic_string(reader, offset, unicode)?
    };

    let result = Arc::new(result);
    // TODO: should this be encoded_offset?
    cache.strings.insert(offset, Arc::clone(&result));

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

fn read_dynamic_string<R: Read + Seek>(
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
    cache: &mut Cache,
    encoded_offset: u32,
) -> Result<Vec<u32>, std::io::Error> {
    if let Some(cached) = cache.offsets.get(&encoded_offset) {
        return Ok(cached.clone());
    }

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

    cache.offsets.insert(encoded_offset, results.clone());

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
