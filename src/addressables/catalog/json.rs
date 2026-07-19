//! Parser for the JSON addressables catalog (`catalog.json`), the old alternative to `catalog.bin`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_derive::Deserialize;

use crate::addressables::catalog::{
    AddressablesCatalog, AssemblyClass, AssetBundleRequestOptions, CommonInfo, Hash128,
    ObjectInitializationData, ResourceLocation,
};

#[allow(non_snake_case)]
#[derive(Deserialize)]
struct JsonCatalog {
    m_LocatorId: String,
    #[serde(default)]
    m_BuildResultHash: String,
    m_InstanceProviderData: JsonObjectInitData,
    m_SceneProviderData: JsonObjectInitData,
    m_ResourceProviderData: Vec<JsonObjectInitData>,
    m_ProviderIds: Vec<String>,
    m_InternalIds: Vec<String>,
    m_KeyDataString: String,
    m_BucketDataString: String,
    m_EntryDataString: String,
    m_ExtraDataString: String,
    m_resourceTypes: Vec<JsonSerializedType>,
    #[serde(default)]
    m_InternalIdPrefixes: Vec<String>,
}

#[allow(non_snake_case)]
#[derive(Deserialize)]
struct JsonObjectInitData {
    m_Id: String,
    m_ObjectType: JsonSerializedType,
    m_Data: String,
}

#[allow(non_snake_case)]
#[derive(Deserialize)]
struct JsonSerializedType {
    m_AssemblyName: String,
    m_ClassName: String,
}

impl JsonSerializedType {
    fn into_assembly_class(self) -> AssemblyClass {
        AssemblyClass {
            m_AssemblyName: Arc::new(self.m_AssemblyName),
            m_ClassName: Arc::new(self.m_ClassName),
        }
    }
}

impl JsonObjectInitData {
    fn into_init_data(self) -> ObjectInitializationData {
        ObjectInitializationData {
            id: Arc::new(self.m_Id),
            object_type: self.m_ObjectType.into_assembly_class(),
            data: Arc::new(self.m_Data),
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<AddressablesCatalog> {
    let cat: JsonCatalog =
        serde_json::from_slice(bytes).context("parsing JSON addressables catalog")?;

    let key_data = decode_b64(&cat.m_KeyDataString, "m_KeyDataString")?;
    let bucket_data = decode_b64(&cat.m_BucketDataString, "m_BucketDataString")?;
    let entry_data = decode_b64(&cat.m_EntryDataString, "m_EntryDataString")?;
    let extra_data = decode_b64(&cat.m_ExtraDataString, "m_ExtraDataString")?;

    // Buckets: a key-data offset plus the entry indices grouped under that key.
    let bucket_count = read_i32(&bucket_data, 0)? as usize;
    let mut buckets: Vec<(usize, Vec<usize>)> = Vec::with_capacity(bucket_count);
    let mut off = 4;
    for _ in 0..bucket_count {
        let data_offset = read_i32(&bucket_data, off)? as usize;
        off += 4;
        let entry_count = read_i32(&bucket_data, off)? as usize;
        off += 4;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            entries.push(read_i32(&bucket_data, off)? as usize);
            off += 4;
        }
        buckets.push((data_offset, entries));
    }

    // One key per bucket, read from key data at the bucket's offset.
    let keys: Vec<String> = buckets
        .iter()
        .map(|(offset, _)| Ok(read_object(&key_data, *offset)?.into_key_string()))
        .collect::<Result<_>>()?;

    let entry_count = read_i32(&entry_data, 0)? as usize;
    let raw: Vec<RawEntry> = (0..entry_count)
        .map(|i| RawEntry::read(&entry_data, 4 + i * RawEntry::SIZE))
        .collect::<Result<_>>()?;

    let ctx = BuildCtx {
        raw: &raw,
        buckets: &buckets,
        keys: &keys,
        internal_ids: &cat.m_InternalIds,
        provider_ids: &cat.m_ProviderIds,
        resource_types: &cat.m_resourceTypes,
        prefixes: &cat.m_InternalIdPrefixes,
        extra_data: &extra_data,
    };
    let mut memo: Vec<Option<Arc<ResourceLocation>>> = vec![None; entry_count];
    for i in 0..entry_count {
        ctx.build_location(i, &mut memo)?;
    }

    // Each bucket maps its key to the locations of its entries.
    let mut resources = HashMap::default();
    for (i, (_, entries)) in buckets.iter().enumerate() {
        let locations = entries
            .iter()
            .map(|&e| Arc::clone(memo[e].as_ref().expect("all entries built")))
            .collect();
        resources.insert(Arc::new(keys[i].clone()), locations);
    }

    Ok(AddressablesCatalog {
        locator_id: Arc::new(cat.m_LocatorId),
        build_result_hash: Arc::new(cat.m_BuildResultHash),
        instance_provider_data: cat.m_InstanceProviderData.into_init_data(),
        scene_provider_data: cat.m_SceneProviderData.into_init_data(),
        resource_provider_data: cat
            .m_ResourceProviderData
            .into_iter()
            .map(JsonObjectInitData::into_init_data)
            .collect(),
        resources,
    })
}

/// A location entry: indexes into the catalog's side tables.
/// A negative index means "none".
struct RawEntry {
    internal_id: i32,
    provider: i32,
    dependency_key: i32,
    dependency_hash: i32,
    data: i32,
    primary_key: i32,
    resource_type: i32,
}
impl RawEntry {
    const SIZE: usize = 7 * 4;

    fn read(data: &[u8], off: usize) -> Result<Self> {
        Ok(RawEntry {
            internal_id: read_i32(data, off)?,
            provider: read_i32(data, off + 4)?,
            dependency_key: read_i32(data, off + 8)?,
            dependency_hash: read_i32(data, off + 12)?,
            data: read_i32(data, off + 16)?,
            primary_key: read_i32(data, off + 20)?,
            resource_type: read_i32(data, off + 24)?,
        })
    }
}

struct BuildCtx<'a> {
    raw: &'a [RawEntry],
    buckets: &'a [(usize, Vec<usize>)],
    keys: &'a [String],
    internal_ids: &'a [String],
    provider_ids: &'a [String],
    resource_types: &'a [JsonSerializedType],
    prefixes: &'a [String],
    extra_data: &'a [u8],
}

impl BuildCtx<'_> {
    fn build_location(
        &self,
        idx: usize,
        memo: &mut Vec<Option<Arc<ResourceLocation>>>,
    ) -> Result<Arc<ResourceLocation>> {
        if let Some(cached) = &memo[idx] {
            return Ok(Arc::clone(cached));
        }
        let entry = &self.raw[idx];

        // Dependencies are all locations grouped under the dependency key's
        // bucket (assumed acyclic, as bundle dependencies are).
        let dependencies = if entry.dependency_key >= 0 {
            let dep_entries = self.buckets[entry.dependency_key as usize].1.clone();
            dep_entries
                .iter()
                .map(|&e| self.build_location(e, memo))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        // The binary catalog joins path segments with `/`; the JSON form keeps
        // the build machine's separator (`\` for Windows bundles). Normalize so
        // downstream path handling (`build_folder` stripping) behaves the same.
        let internal_id = expand_internal_id(
            self.prefixes,
            &self.internal_ids[entry.internal_id as usize],
        )
        .replace('\\', "/");
        let primary_key = if entry.primary_key >= 0 {
            self.keys[entry.primary_key as usize].clone()
        } else {
            String::new()
        };
        let type_ = match self.resource_types.get(entry.resource_type as usize) {
            Some(ty) if entry.resource_type >= 0 => AssemblyClass {
                m_AssemblyName: Arc::new(ty.m_AssemblyName.clone()),
                m_ClassName: Arc::new(ty.m_ClassName.clone()),
            },
            _ => AssemblyClass {
                m_AssemblyName: Arc::new(String::new()),
                m_ClassName: Arc::new(String::new()),
            },
        };
        let data = if entry.data >= 0 {
            read_abro(self.extra_data, entry.data as usize)?
        } else {
            None
        };

        let location = Arc::new(ResourceLocation {
            internal_id: Arc::new(internal_id),
            provider_id: Arc::new(self.provider_ids[entry.provider as usize].clone()),
            dependencies,
            data,
            dependency_hash_code: entry.dependency_hash,
            primary_key: Arc::new(primary_key),
            type_,
        });
        memo[idx] = Some(Arc::clone(&location));
        Ok(location)
    }
}

/// `AssetBundleRequestOptions`, JSON-serialized inside `m_ExtraDataString`.
#[allow(non_snake_case)]
#[derive(Deserialize)]
struct AbroJson {
    #[serde(default)]
    m_Hash: String,
    #[serde(default)]
    m_Crc: u32,
    #[serde(default)]
    m_Timeout: i16,
    #[serde(default)]
    m_RedirectLimit: i32,
    #[serde(default)]
    m_RetryCount: i32,
    #[serde(default)]
    m_BundleName: String,
    #[serde(default)]
    m_BundleSize: u64,
}

/// Read an `AssetBundleRequestOptions` from extra data (a JSON-object blob).
///
/// The binary catalog packs several booleans (`ChunkedTransfer`, `AssetLoadMode`,
/// …) into `CommonInfo::flags`; the JSON form keeps them as separate fields that
/// no consumer here reads, so `flags` is left `0`.
fn read_abro(extra_data: &[u8], offset: usize) -> Result<Option<AssetBundleRequestOptions>> {
    let CatalogObject::Json { json, .. } = read_object(extra_data, offset)? else {
        // Only AssetBundleProvider locations carry ABRO data; anything else has
        // no bundle request options to expose.
        return Ok(None);
    };
    let abro: AbroJson =
        serde_json::from_str(&json).context("parsing AssetBundleRequestOptions")?;
    Ok(Some(AssetBundleRequestOptions {
        hash: Hash128::from_u32s(parse_hash128(&abro.m_Hash)),
        crc: abro.m_Crc,
        common_info: CommonInfo {
            timeout: abro.m_Timeout,
            redirect_limit: abro.m_RedirectLimit as u8,
            retry_count: abro.m_RetryCount as u8,
            flags: 0,
        },
        bundle_name: Arc::new(abro.m_BundleName),
        bundle_size: abro.m_BundleSize as u32,
    }))
}

/// Expand an internal id that references a shared prefix (`"<index>#<suffix>"`).
/// A no-op when the catalog has no `m_InternalIdPrefixes`.
fn expand_internal_id(prefixes: &[String], input: &str) -> String {
    if prefixes.is_empty() {
        return input.to_owned();
    }
    match input.split_once('#') {
        Some((idx, rest)) => match idx.parse::<usize>().ok().and_then(|i| prefixes.get(i)) {
            Some(prefix) => format!("{prefix}{rest}"),
            None => input.to_owned(),
        },
        None => input.to_owned(),
    }
}

/// A value serialized by Unity's `SerializationUtilities.WriteObjectToByteArray`.
/// Only the variants that occur as catalog keys / extra data are represented.
enum CatalogObject {
    String(String),
    Int(i64),
    Hash128([u32; 4]),
    Json {
        #[allow(dead_code)]
        assembly: String,
        #[allow(dead_code)]
        class: String,
        json: String,
    },
}
impl CatalogObject {
    fn into_key_string(self) -> String {
        match self {
            CatalogObject::String(s) => s,
            CatalogObject::Int(i) => i.to_string(),
            CatalogObject::Hash128([a, b, c, d]) => format!("{a:08x}{b:08x}{c:08x}{d:08x}"),
            CatalogObject::Json { json, .. } => json,
        }
    }
}

/// Mirror of `SerializationUtilities.ObjectType`.
mod object_type {
    pub const ASCII_STRING: u8 = 0;
    pub const UNICODE_STRING: u8 = 1;
    pub const U16: u8 = 2;
    pub const U32: u8 = 3;
    pub const I32: u8 = 4;
    pub const HASH128: u8 = 5;
    pub const JSON_OBJECT: u8 = 7;
}

fn read_object(data: &[u8], offset: usize) -> Result<CatalogObject> {
    let ty = *data
        .get(offset)
        .context("catalog object type out of bounds")?;
    let mut off = offset + 1;
    Ok(match ty {
        object_type::ASCII_STRING => {
            let len = read_i32(data, off)? as usize;
            off += 4;
            CatalogObject::String(read_ascii(data, off, len)?)
        }
        object_type::UNICODE_STRING => {
            let len = read_i32(data, off)? as usize;
            off += 4;
            CatalogObject::String(read_utf16(data, off, len)?)
        }
        object_type::U16 => CatalogObject::Int(read_u16(data, off)? as i64),
        object_type::U32 => CatalogObject::Int(read_u32(data, off)? as i64),
        object_type::I32 => CatalogObject::Int(read_i32(data, off)? as i64),
        object_type::HASH128 => {
            let mut v = [0u32; 4];
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = read_u32(data, off + i * 4)?;
            }
            CatalogObject::Hash128(v)
        }
        object_type::JSON_OBJECT => {
            let assembly_len = *data.get(off).context("json object assembly len")? as usize;
            off += 1;
            let assembly = read_ascii(data, off, assembly_len)?;
            off += assembly_len;
            let class_len = *data.get(off).context("json object class len")? as usize;
            off += 1;
            let class = read_ascii(data, off, class_len)?;
            off += class_len;
            let json_len = read_i32(data, off)? as usize;
            off += 4;
            let json = read_utf16(data, off, json_len)?;
            CatalogObject::Json {
                assembly,
                class,
                json,
            }
        }
        other => bail!("unsupported catalog object type {other}"),
    })
}

fn parse_hash128(hex: &str) -> [u32; 4] {
    if hex.len() < 32 {
        return [0; 4];
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    let mut out = [0u32; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    }
    out
}

fn decode_b64(s: &str, field: &str) -> Result<Vec<u8>> {
    BASE64
        .decode(s)
        .with_context(|| format!("base64-decoding {field}"))
}

fn slice(data: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    data.get(off..off + len)
        .with_context(|| format!("catalog read out of bounds at {off}..{}", off + len))
}

fn read_i32(data: &[u8], off: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(slice(data, off, 4)?.try_into().unwrap()))
}
fn read_u32(data: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(data, off, 4)?.try_into().unwrap()))
}
fn read_u16(data: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(slice(data, off, 2)?.try_into().unwrap()))
}

fn read_ascii(data: &[u8], off: usize, len: usize) -> Result<String> {
    Ok(String::from_utf8_lossy(slice(data, off, len)?).into_owned())
}

fn read_utf16(data: &[u8], off: usize, byte_len: usize) -> Result<String> {
    let bytes = slice(data, off, byte_len)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}
