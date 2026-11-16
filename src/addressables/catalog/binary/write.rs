use rustc_hash::FxHashMap;

use super::{
    AddressablesCatalog, AssemblyClass, AssetBundleRequestOptions, CatalogString, DYNAMIC_FLAG,
    MAGIC, NONE, OFFSET_MASK, ObjectInitializationData, SerializedObject, VERSION,
};

const HEADER_SIZE: usize = 32;
/// Strings up to this length are stored whole, and shorter segments merge into their left neighbour
const MIN_SEGMENT_LEN: usize = 8;

fn unsupported(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg.into())
}

impl AddressablesCatalog {
    pub fn to_binary(&self) -> Result<Vec<u8>, std::io::Error> {
        BinaryCatalogWriter::default().write(self)
    }
}

#[derive(Default)]
struct BinaryCatalogWriter {
    buf: Vec<u8>,
    blobs: FxHashMap<Vec<u8>, u32>,
}

impl BinaryCatalogWriter {
    fn write(mut self, catalog: &AddressablesCatalog) -> Result<Vec<u8>, std::io::Error> {
        self.buf.resize(HEADER_SIZE, 0);

        let key_count = catalog.resources.len();
        self.buf
            .extend_from_slice(&(8 * key_count as i32).to_le_bytes());
        let keys_offset = self.buf.len();
        self.buf.resize(keys_offset + 8 * key_count, 0);

        let locator_id = self.plain_string(&catalog.locator_id)?;
        let instance_provider = self.object_init(&catalog.instance_provider_data)?;
        let scene_provider = self.object_init(&catalog.scene_provider_data)?;
        let resource_providers = catalog
            .resource_provider_data
            .iter()
            .map(|data| self.object_init(data))
            .collect::<Result<Vec<_>, _>>()?;
        let resource_providers = self.offset_array(&resource_providers)?;
        let build_result_hash = catalog
            .build_result_hash
            .as_ref()
            .ok_or_else(|| unsupported("catalog without build result hash"))?;
        let build_result_hash = self.plain_string(build_result_hash)?;

        let mut location_offsets = vec![None; catalog.locations.len()];
        for i in 0..catalog.locations.len() {
            self.location(catalog, i, &mut location_offsets)?;
        }
        let location_offsets: Vec<u32> = location_offsets.into_iter().map(Option::unwrap).collect();

        let mut key_entries = Vec::with_capacity(2 * key_count);
        for (key, indices) in &catalog.resources {
            key_entries.push(self.serialized(key, Self::string_object)?);
            let locations: Vec<u32> = indices.iter().map(|&i| location_offsets[i]).collect();
            key_entries.push(self.offset_array(&locations)?);
        }
        for (i, entry) in key_entries.iter().enumerate() {
            let at = keys_offset + 4 * i;
            self.buf[at..at + 4].copy_from_slice(&entry.to_le_bytes());
        }

        let header = [
            MAGIC as u32,
            VERSION as u32,
            keys_offset as u32,
            locator_id,
            instance_provider,
            scene_provider,
            resource_providers,
            build_result_hash,
        ];
        for (i, value) in header.iter().enumerate() {
            self.buf[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
        }
        Ok(self.buf)
    }

    /// Appends `bytes` unless an identical blob was already written, returning its offset
    fn blob(&mut self, bytes: &[u8]) -> Result<u32, std::io::Error> {
        if let Some(&offset) = self.blobs.get(bytes) {
            return Ok(offset);
        }
        let offset = u32::try_from(self.buf.len())
            .ok()
            .filter(|&offset| offset <= OFFSET_MASK)
            .ok_or_else(|| unsupported("catalog exceeds the maximum offset"))?;
        self.buf.extend_from_slice(bytes);
        self.blobs.insert(bytes.to_vec(), offset);
        Ok(offset)
    }

    fn length_prefixed(&mut self, payload: &[u8]) -> Result<u32, std::io::Error> {
        let mut bytes = Vec::with_capacity(4 + payload.len());
        bytes.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        bytes.extend_from_slice(payload);
        Ok(self.blob(&bytes)? + 4)
    }

    fn offset_array(&mut self, offsets: &[u32]) -> Result<u32, std::io::Error> {
        let payload: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
        self.length_prefixed(&payload)
    }

    fn plain_string(&mut self, value: &str) -> Result<u32, std::io::Error> {
        if !value.is_ascii() {
            return Err(unsupported(format!("non-ascii string {value:?}")));
        }
        self.length_prefixed(value.as_bytes())
    }

    fn string(&mut self, value: &str, separator: Option<char>) -> Result<u32, std::io::Error> {
        match separator {
            Some(separator) if value.len() > MIN_SEGMENT_LEN && value.contains(separator) => {
                let segments = segments(value, separator);
                Ok(self.segment_chain(&segments)? | DYNAMIC_FLAG)
            }
            _ => self.plain_string(value),
        }
    }

    fn segment_chain(&mut self, segments: &[&str]) -> Result<u32, std::io::Error> {
        let (last, prefix) = segments.split_last().unwrap();
        let last = self.plain_string(last)?;
        let prefix = match prefix {
            [] => NONE,
            prefix => self.segment_chain(prefix)?,
        };
        self.blob(&u32_bytes(&[last, prefix]))
    }

    fn assembly_class(&mut self, class: &AssemblyClass) -> Result<u32, std::io::Error> {
        let assembly = self.string(&class.m_AssemblyName, Some('.'))?;
        let class = self.string(&class.m_ClassName, Some('.'))?;
        self.blob(&u32_bytes(&[assembly, class]))
    }

    fn object_init(&mut self, data: &ObjectInitializationData) -> Result<u32, std::io::Error> {
        if data.data.is_some() {
            return Err(unsupported(format!(
                "object initialization data with data for {}",
                data.id
            )));
        }
        let id = self.plain_string(&data.id)?;
        let object_type = self.assembly_class(&data.object_type)?;
        self.blob(&u32_bytes(&[id, object_type, NONE]))
    }

    fn serialized<T>(
        &mut self,
        value: &SerializedObject<T>,
        write_object: impl FnOnce(&mut Self, &T) -> Result<u32, std::io::Error>,
    ) -> Result<u32, std::io::Error> {
        let type_ = value
            .type_
            .as_ref()
            .ok_or_else(|| unsupported("serialized object without type"))?;
        let object = value
            .object
            .as_ref()
            .ok_or_else(|| unsupported(format!("default object of type {type_:?}")))?;
        let object = write_object(self, object)?;
        let type_ = self.assembly_class(type_)?;
        self.blob(&u32_bytes(&[type_, object]))
    }

    fn string_object(&mut self, value: &CatalogString) -> Result<u32, std::io::Error> {
        let separator = match value.separator {
            None => 0,
            Some(c) if c.is_ascii() && c != '\0' => c as u8,
            Some(c) => return Err(unsupported(format!("separator {c:?}"))),
        };
        let string = self.string(&value.value, value.separator)?;
        let mut bytes = string.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[separator, 0, 0, 0]);
        self.blob(&bytes)
    }

    fn abro(&mut self, abro: &AssetBundleRequestOptions) -> Result<u32, std::io::Error> {
        let hash = self.blob(&u32_bytes(&abro.hash.to_u32s()))?;
        let bundle_name = self.string(&abro.bundle_name, Some('_'))?;
        let info = &abro.common_info;
        let mut common_info = Vec::with_capacity(8);
        common_info.extend_from_slice(&info.timeout.to_le_bytes());
        common_info.extend_from_slice(&[info.redirect_limit, info.retry_count]);
        common_info.extend_from_slice(&info.flags.to_le_bytes());
        let common_info = self.blob(&common_info)?;
        self.blob(&u32_bytes(&[
            hash,
            bundle_name,
            abro.crc,
            abro.bundle_size,
            common_info,
        ]))
    }

    fn location(
        &mut self,
        catalog: &AddressablesCatalog,
        index: usize,
        offsets: &mut [Option<u32>],
    ) -> Result<u32, std::io::Error> {
        if let Some(offset) = offsets[index] {
            return Ok(offset);
        }
        let location = &catalog.locations[index];
        let dependencies = match location.dependencies.as_slice() {
            [] => NONE,
            dependencies => {
                let dependencies = dependencies
                    .iter()
                    .map(|&i| self.location(catalog, i, offsets))
                    .collect::<Result<Vec<_>, _>>()?;
                self.offset_array(&dependencies)?
            }
        };
        let primary_key = self.string(&location.primary_key, Some('/'))?;
        let internal_id = self.string(&location.internal_id, Some('/'))?;
        let provider_id = self.string(&location.provider_id, Some('.'))?;
        let data = match &location.data {
            None => NONE,
            Some(data) => self.serialized(data, Self::abro)?,
        };
        let type_ = self.assembly_class(&location.type_)?;
        let offset = self.blob(&u32_bytes(&[
            primary_key,
            internal_id,
            provider_id,
            dependencies,
            location.dependency_hash_code as u32,
            data,
            type_,
        ]))?;
        offsets[index] = Some(offset);
        Ok(offset)
    }
}

fn u32_bytes(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Splits on `separator`, merging segments shorter than [`MIN_SEGMENT_LEN`] into their left neighbour
fn segments(value: &str, separator: char) -> Vec<&str> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            value
                .match_indices(separator)
                .map(|(i, _)| i + separator.len_utf8()),
        )
        .collect();
    let mut segments = Vec::new();
    let mut end = value.len();
    let mut i = starts.len();
    while i > 0 {
        i -= 1;
        while end - starts[i] < MIN_SEGMENT_LEN && i > 0 {
            i -= 1;
        }
        segments.push(&value[starts[i]..end]);
        end = starts[i].saturating_sub(separator.len_utf8());
    }
    segments.reverse();
    segments
}

#[cfg(test)]
mod tests {
    use super::segments;

    #[test]
    fn segments_merge_short_parts_leftwards() {
        assert_eq!(
            segments(
                "Unity.ResourceManager, Version=0.0.0.0, Culture=neutral",
                '.'
            ),
            [
                "Unity",
                "ResourceManager, Version=0.0.0",
                "0, Culture=neutral"
            ]
        );
        assert_eq!(
            segments("animations_assets_areaant_d071fb294e817a04.bundle", '_'),
            ["animations", "assets_areaant", "d071fb294e817a04.bundle"]
        );
        assert_eq!(
            segments("animations_assets__92cd1f53.bundle", '_'),
            ["animations_assets_", "92cd1f53.bundle"]
        );
        assert_eq!(segments("System.String", '.'), ["System.String"]);
    }
}
