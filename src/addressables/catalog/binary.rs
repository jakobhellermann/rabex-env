mod write;

use std::io::{Read, Seek};
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssemblyClass {
    pub m_AssemblyName: Arc<String>,
    pub m_ClassName: Arc<String>,
}
impl AssemblyClass {
    pub fn new(assembly: &str, class: &str) -> Self {
        AssemblyClass {
            m_AssemblyName: Arc::new(assembly.to_owned()),
            m_ClassName: Arc::new(class.to_owned()),
        }
    }

    pub fn class_name(&self) -> &str {
        self.m_ClassName
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(&*self.m_ClassName)
    }

    fn is(&self, (assembly, class): (&str, &str)) -> bool {
        self.m_AssemblyName.split(',').next() == Some(assembly) && *self.m_ClassName == class
    }
}

#[derive(Debug)]
pub struct ObjectInitializationData {
    pub id: Arc<String>,
    pub object_type: AssemblyClass,
    pub data: Option<Arc<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SerializedObject<T> {
    /// `None` in JSON catalogs, which don't record it
    pub type_: Option<AssemblyClass>,
    /// `None` for a default-constructed object
    pub object: Option<T>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogString {
    pub value: Arc<String>,
    /// Binary catalogs store strings containing this separator as deduplicated segments
    pub separator: Option<char>,
}

pub type CatalogKey = SerializedObject<CatalogString>;

impl CatalogKey {
    pub fn as_str(&self) -> &str {
        self.object.as_ref().map_or("", |s| s.value.as_str())
    }
}

pub mod resource_providers {
    pub const ASSET_BUNDLE: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleProvider";
    pub const ATLAS_SPRITE: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.AtlasSpriteProvider";
    /// - `BinaryAssetProvider<TAdapter>`
    pub const BINARY_DATA: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.BinaryDataProvider";
    pub const BUNDLED_ASSET: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.BundledAssetProvider";
    pub const CONTENT_CATALOG: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.ContentCatalogProvider";
    pub const JSON_ASSET: &str =
        "UnityEngine.ResourceManagement.ResourceProviders.JsonAssetProvider";
    pub const SCENE: &str = "UnityEngine.ResourceManagement.ResourceProviders.SceneProvider";
    pub const TEXT_DATA: &str = "UnityEngine.ResourceManagement.ResourceProviders.TextDataProvider";
}

/// Fully qualified names of the assemblies catalog types live in
pub mod assemblies {
    pub const RESOURCE_MANAGER: &str =
        "Unity.ResourceManager, Version=0.0.0.0, Culture=neutral, PublicKeyToken=null";
    pub const CORE_MODULE: &str =
        "UnityEngine.CoreModule, Version=0.0.0.0, Culture=neutral, PublicKeyToken=null";
    pub const MSCORLIB: &str =
        "mscorlib, Version=4.0.0.0, Culture=neutral, PublicKeyToken=b77a5c561934e089";
}

/// Descriptor of a catalog resource location
///
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
#[derive(Debug, PartialEq)]
pub struct ResourceLocation {
    /// ID of the resource location
    ///
    /// - `AssetBundleProvider`: `{...RuntimePath}/OS/path/to/name.bundle`
    /// - `BundledAssetProvider`: `Assets/Scenes/Name.unity`, Àssets/Audio/audio.asset`
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
    /// Indices into [`AddressablesCatalog::locations`]
    pub dependencies: Vec<usize>,
    pub data: Option<SerializedObject<AssetBundleRequestOptions>>,
    pub dependency_hash_code: i32,
    /// Used in `Addressables.LoadAssetAsync<GameObject>(primaryKey)`
    pub primary_key: Arc<String>,
    pub type_: AssemblyClass,
}

impl ResourceLocation {
    pub fn provider_name(&self) -> &str {
        self.provider_id
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(self.provider_id.as_ref())
    }

    pub fn abro(&self) -> Option<&AssetBundleRequestOptions> {
        self.data.as_ref()?.object.as_ref()
    }
}

#[derive(Debug)]
pub struct AddressablesCatalog {
    pub locator_id: Arc<String>,
    pub build_result_hash: Option<Arc<String>>,
    pub instance_provider_data: ObjectInitializationData,
    pub scene_provider_data: ObjectInitializationData,
    pub resource_provider_data: Vec<ObjectInitializationData>,
    pub locations: Vec<ResourceLocation>,
    /// Indices into [`AddressablesCatalog::locations`]
    pub resources: IndexMap<CatalogKey, Vec<usize>>,
}

impl AddressablesCatalog {
    pub fn from_reader<R: Read + Seek>(mut reader: R) -> Result<Self, std::io::Error> {
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        Self::from_bytes(&data)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, std::io::Error> {
        BinaryCatalogReader::new(data).read()
    }

    pub fn locations(&self) -> impl Iterator<Item = &ResourceLocation> {
        self.locations.iter()
    }

    pub fn locations_at<'a>(
        &'a self,
        indices: &'a [usize],
    ) -> impl Iterator<Item = &'a ResourceLocation> + 'a {
        indices.iter().map(|&i| &self.locations[i])
    }

    pub fn dependencies<'a>(
        &'a self,
        location: &'a ResourceLocation,
    ) -> impl Iterator<Item = &'a ResourceLocation> + 'a {
        self.locations_at(&location.dependencies)
    }

    pub fn locations_of_key(&self, key: &str) -> Option<impl Iterator<Item = &ResourceLocation>> {
        let (_, indices) = self.resources.iter().find(|(k, _)| k.as_str() == key)?;
        Some(self.locations_at(indices))
    }

    pub fn locations_of_provider(
        &self,
        provider_id: &str,
    ) -> impl Iterator<Item = &ResourceLocation> {
        self.locations()
            .filter(move |item| *item.provider_id == provider_id)
    }
}

const MAGIC: i32 = 0x0de38942;
const VERSION: i32 = 2;
const NONE: u32 = u32::MAX;
const UNICODE_FLAG: u32 = 0x8000_0000;
const DYNAMIC_FLAG: u32 = 0x4000_0000;
const OFFSET_MASK: u32 = 0x3fff_ffff;

const STRING_TYPE: (&str, &str) = ("mscorlib", "System.String");
const ABRO_TYPE: (&str, &str) = (
    "Unity.ResourceManager",
    "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleRequestOptions",
);

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

struct BinaryCatalogReader<'a> {
    data: &'a [u8],
    strings: FxHashMap<(u32, Option<char>), Arc<String>>,
}

impl<'a> BinaryCatalogReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BinaryCatalogReader {
            data,
            strings: FxHashMap::default(),
        }
    }

    fn bytes(&self, offset: u32, len: usize) -> Result<&'a [u8], std::io::Error> {
        let start = offset as usize;
        self.data
            .get(start..start + len)
            .ok_or_else(|| invalid(format!("catalog read out of bounds at {start}+{len}")))
    }
    fn array<const N: usize>(&self, offset: u32) -> Result<[u8; N], std::io::Error> {
        Ok(self.bytes(offset, N)?.try_into().unwrap())
    }
    fn u8(&self, offset: u32) -> Result<u8, std::io::Error> {
        Ok(self.array::<1>(offset)?[0])
    }
    fn i16(&self, offset: u32) -> Result<i16, std::io::Error> {
        Ok(i16::from_le_bytes(self.array(offset)?))
    }
    fn u32(&self, offset: u32) -> Result<u32, std::io::Error> {
        Ok(u32::from_le_bytes(self.array(offset)?))
    }
    fn i32(&self, offset: u32) -> Result<i32, std::io::Error> {
        Ok(i32::from_le_bytes(self.array(offset)?))
    }
    fn u32s<const N: usize>(&self, offset: u32) -> Result<[u32; N], std::io::Error> {
        let mut values = [0; N];
        for (i, value) in values.iter_mut().enumerate() {
            *value = self.u32(offset + 4 * i as u32)?;
        }
        Ok(values)
    }
    /// Payload of a blob whose `i32` byte length precedes `offset`
    fn length_prefixed(&self, offset: u32) -> Result<&'a [u8], std::io::Error> {
        let len_offset = offset
            .checked_sub(4)
            .ok_or_else(|| invalid(format!("length prefix before {offset:#x} out of bounds")))?;
        let len = self.i32(len_offset)?;
        let len =
            usize::try_from(len).map_err(|_| invalid(format!("negative length at {offset:#x}")))?;
        self.bytes(offset, len)
    }

    fn read(&mut self) -> Result<AddressablesCatalog, std::io::Error> {
        let magic = self.i32(0)?;
        if magic != MAGIC {
            return Err(invalid(format!(
                "Unexpected magic for addressables catalog: {magic}"
            )));
        }
        let version = self.i32(4)?;
        if version != VERSION {
            return Err(invalid(format!("Unsupported version {version}")));
        }
        let [
            keys_offset,
            id_offset,
            instance_provider_offset,
            scene_provider_offset,
            init_objects_array_offset,
            build_result_hash_offset,
        ] = self.u32s(8)?;

        let locator_id = self.string(id_offset, None)?;
        let build_result_hash = self.optional_string(build_result_hash_offset, None)?;
        let instance_provider_data = self.object_init(instance_provider_offset)?;
        let scene_provider_data = self.object_init(scene_provider_offset)?;
        let resource_provider_data = self
            .offset_array(init_objects_array_offset)?
            .unwrap_or_default()
            .into_iter()
            .map(|offset| self.object_init(offset))
            .collect::<Result<_, _>>()?;

        let key_entries = self
            .offset_array(keys_offset)?
            .ok_or_else(|| invalid("catalog has no keys array"))?;
        let (pairs, rest) = key_entries.as_chunks::<2>();
        if !rest.is_empty() {
            return Err(invalid("odd number of entries in keys array"));
        }
        let key_entries = pairs
            .iter()
            .map(|&[key, locations]| Ok((key, self.offset_array(locations)?.unwrap_or_default())))
            .collect::<Result<Vec<_>, std::io::Error>>()?;

        let location_offsets = self.location_offsets(key_entries.iter().flat_map(|(_, l)| l))?;
        let location_index: FxHashMap<u32, usize> = location_offsets
            .iter()
            .enumerate()
            .map(|(i, &offset)| (offset, i))
            .collect();
        let locations = location_offsets
            .iter()
            .map(|&offset| self.location(offset, &location_index))
            .collect::<Result<_, _>>()?;

        let mut resources = IndexMap::with_capacity(key_entries.len());
        for (key_offset, location_offsets) in key_entries {
            let key = self
                .serialized(key_offset, STRING_TYPE, Self::string_object)?
                .ok_or_else(|| invalid("null resource key"))?;
            let indices = location_offsets
                .iter()
                .map(|offset| location_index[offset])
                .collect();
            if resources.contains_key(&key) {
                return Err(invalid(format!("duplicate resource key {key:?}")));
            }
            resources.insert(key, indices);
        }

        Ok(AddressablesCatalog {
            locator_id,
            build_result_hash,
            instance_provider_data,
            scene_provider_data,
            resource_provider_data,
            locations,
            resources,
        })
    }

    /// All locations reachable from `roots`, sorted by offset
    fn location_offsets<'r>(
        &self,
        roots: impl Iterator<Item = &'r u32>,
    ) -> Result<Vec<u32>, std::io::Error> {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack: Vec<u32> = roots.copied().collect();
        while let Some(offset) = stack.pop() {
            if !seen.insert(offset) {
                continue;
            }
            let dependencies_offset = self.u32(offset + 12)?;
            stack.extend(self.offset_array(dependencies_offset)?.unwrap_or_default());
        }
        let mut offsets: Vec<u32> = seen.into_iter().collect();
        offsets.sort_unstable();
        Ok(offsets)
    }

    fn location(
        &mut self,
        offset: u32,
        location_index: &FxHashMap<u32, usize>,
    ) -> Result<ResourceLocation, std::io::Error> {
        let [
            primary_key,
            internal_id,
            provider_id,
            dependencies,
            dependency_hash_code,
            data,
            type_,
        ] = self.u32s(offset)?;

        let dependencies = match self.offset_array(dependencies)? {
            None => Vec::new(),
            Some(offsets) if offsets.is_empty() => {
                return Err(invalid(format!(
                    "empty dependency array of location at {offset:#x}"
                )));
            }
            Some(offsets) => offsets
                .iter()
                .map(|offset| location_index[offset])
                .collect(),
        };
        Ok(ResourceLocation {
            primary_key: self.string(primary_key, Some('/'))?,
            internal_id: self.string(internal_id, Some('/'))?,
            provider_id: self.string(provider_id, Some('.'))?,
            dependencies,
            dependency_hash_code: dependency_hash_code as i32,
            data: self.serialized(data, ABRO_TYPE, Self::abro)?,
            type_: self.assembly_class(type_)?,
        })
    }

    fn assembly_class(&mut self, offset: u32) -> Result<AssemblyClass, std::io::Error> {
        Ok(AssemblyClass {
            m_AssemblyName: self.string(self.u32(offset)?, Some('.'))?,
            m_ClassName: self.string(self.u32(offset + 4)?, Some('.'))?,
        })
    }

    fn object_init(&mut self, offset: u32) -> Result<ObjectInitializationData, std::io::Error> {
        Ok(ObjectInitializationData {
            id: self.string(self.u32(offset)?, None)?,
            object_type: self.assembly_class(self.u32(offset + 4)?)?,
            data: self.optional_string(self.u32(offset + 8)?, None)?,
        })
    }

    fn serialized<T>(
        &mut self,
        offset: u32,
        expected_type: (&str, &str),
        read_object: impl FnOnce(&mut Self, u32) -> Result<T, std::io::Error>,
    ) -> Result<Option<SerializedObject<T>>, std::io::Error> {
        if offset == NONE {
            return Ok(None);
        }
        let type_ = self.assembly_class(self.u32(offset)?)?;
        if !type_.is(expected_type) {
            return Err(invalid(format!(
                "expected serialized {expected_type:?}, got {type_:?}"
            )));
        }
        let object_offset = self.u32(offset + 4)?;
        let object = match object_offset {
            NONE => None,
            offset => Some(read_object(self, offset)?),
        };
        Ok(Some(SerializedObject {
            type_: Some(type_),
            object,
        }))
    }

    fn string_object(&mut self, offset: u32) -> Result<CatalogString, std::io::Error> {
        let separator = match self.u8(offset + 4)? {
            0 => None,
            c => Some(c as char),
        };
        Ok(CatalogString {
            value: self.string(self.u32(offset)?, separator)?,
            separator,
        })
    }

    fn abro(&mut self, offset: u32) -> Result<AssetBundleRequestOptions, std::io::Error> {
        let [hash, bundle_name, crc, bundle_size, common_info] = self.u32s(offset)?;
        Ok(AssetBundleRequestOptions {
            hash: Hash128::from_u32s(self.u32s(hash)?),
            crc,
            common_info: CommonInfo {
                timeout: self.i16(common_info)?,
                redirect_limit: self.u8(common_info + 2)?,
                retry_count: self.u8(common_info + 3)?,
                flags: CommonInfoFlags::from_bits_retain(self.i32(common_info + 4)?),
            },
            bundle_name: self.string(bundle_name, Some('_'))?,
            bundle_size,
        })
    }

    fn string(
        &mut self,
        encoded_offset: u32,
        separator: Option<char>,
    ) -> Result<Arc<String>, std::io::Error> {
        self.optional_string(encoded_offset, separator)?
            .ok_or_else(|| invalid("unexpected null string"))
    }

    fn optional_string(
        &mut self,
        encoded_offset: u32,
        separator: Option<char>,
    ) -> Result<Option<Arc<String>>, std::io::Error> {
        if encoded_offset == NONE {
            return Ok(None);
        }
        if let Some(cached) = self.strings.get(&(encoded_offset, separator)) {
            return Ok(Some(Arc::clone(cached)));
        }
        if encoded_offset & UNICODE_FLAG != 0 {
            return Err(invalid(format!(
                "unsupported unicode string at {:#x}",
                encoded_offset & OFFSET_MASK
            )));
        }
        let offset = encoded_offset & OFFSET_MASK;
        let value = if encoded_offset & DYNAMIC_FLAG != 0 {
            let separator = separator.ok_or_else(|| {
                invalid(format!("segmented string without separator at {offset:#x}"))
            })?;
            self.segmented_string(offset, separator)?
        } else {
            let bytes = self.length_prefixed(offset)?;
            String::from_utf8(bytes.to_vec())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        };
        let value = Arc::new(value);
        self.strings
            .insert((encoded_offset, separator), Arc::clone(&value));
        Ok(Some(value))
    }

    fn segmented_string(
        &mut self,
        mut offset: u32,
        separator: char,
    ) -> Result<String, std::io::Error> {
        let mut parts = Vec::new();
        loop {
            let part = self.string(self.u32(offset)?, None)?;
            parts.push(part);
            let next = self.u32(offset + 4)?;
            if next == NONE {
                break;
            }
            offset = next;
        }
        parts.reverse();
        Ok(parts
            .iter()
            .map(|part| part.as_str())
            .collect::<Vec<_>>()
            .join(&separator.to_string()))
    }

    fn offset_array(&self, offset: u32) -> Result<Option<Vec<u32>>, std::io::Error> {
        if offset == NONE {
            return Ok(None);
        }
        let (chunks, rest) = self.length_prefixed(offset)?.as_chunks::<4>();
        if !rest.is_empty() {
            return Err(invalid(format!(
                "offset array at {offset:#x} has unaligned length"
            )));
        }
        Ok(Some(
            chunks
                .iter()
                .map(|chunk| u32::from_le_bytes(*chunk))
                .collect(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash128(u32, u32, u32, u32);
impl Hash128 {
    pub fn from_u32s([a, b, c, d]: [u32; 4]) -> Self {
        Hash128(a, b, c, d)
    }
    pub fn to_u32s(&self) -> [u32; 4] {
        [self.0, self.1, self.2, self.3]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetBundleRequestOptions {
    pub hash: Hash128,
    pub crc: u32,
    pub common_info: CommonInfo,
    pub bundle_name: Arc<String>,
    pub bundle_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommonInfo {
    pub timeout: i16,
    pub redirect_limit: u8,
    pub retry_count: u8,
    pub flags: CommonInfoFlags,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CommonInfoFlags: i32 {
        const ALL_PACKED_ASSETS_AND_DEPENDENCIES = 1 << 0;
        const CHUNKED_TRANSFER = 1 << 1;
        const USE_CRC_FOR_CACHED_BUNDLE = 1 << 2;
        const USE_UNITY_WEB_REQUEST_FOR_LOCAL_BUNDLES = 1 << 3;
        const CLEAR_OTHER_CACHED_VERSIONS_WHEN_LOADED = 1 << 4;
    }
}

impl AssetBundleRequestOptions {
    /// Options for a bundle loaded from a local path
    pub fn new(bundle_name: &str) -> Self {
        AssetBundleRequestOptions {
            hash: Hash128::from_u32s([0; 4]),
            crc: 0,
            common_info: CommonInfo {
                timeout: 0,
                redirect_limit: 32,
                retry_count: 0,
                flags: CommonInfoFlags::USE_CRC_FOR_CACHED_BUNDLE,
            },
            bundle_name: Arc::new(bundle_name.to_owned()),
            // only used for the download size of remote bundles
            bundle_size: 0,
        }
    }
}
