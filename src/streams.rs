//! Find and relocate `StreamingInfo`/`StreamedResource` data references in objects
use std::io::Cursor;
use std::ops::Range;

use anyhow::Result;
use byteorder::LittleEndian;
use rabex::files::serializedfile::ObjectInfo;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::serde_typetree;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use serde_derive::{Deserialize, Serialize};

use crate::trace_pptr::{self, NoRefTypes};

/// Target for a `StreamingInfo` or `StreamedResource`
pub struct StreamLocation<'a> {
    pub path: &'a str,
    pub offset: u64,
    pub size: u64,
}

/// Rewrite every present stream location in `object_data` to the location specified by `relocate`
pub fn relocate_streams(
    object_data: &mut Vec<u8>,
    tt: &TypeTreeNode,
    mut relocate: impl FnMut(StreamLocation) -> Result<(String, u64)>,
) -> Result<()> {
    // PERF: there might be more efficient ways to do this, but there probably aren't many streams per object usually
    for (range, node, mut stream) in find_streams(object_data, tt)?.into_iter().rev() {
        let location = stream.location();
        if location.path.is_empty() {
            continue;
        }

        let (path, offset) = relocate(location)?;
        stream.set_location(path, offset);

        // Alignment is relative to the object start, so write at the node's original position.
        let mut writer = Cursor::new(vec![0; range.start]);
        writer.set_position(range.start as u64);
        stream.write(&mut writer, &node)?;
        object_data.splice(range.clone(), writer.into_inner().split_off(range.start));
    }
    Ok(())
}

pub trait MapStreamLocations {
    /// [`relocate_streams`] in every object
    fn map_stream_locations(
        &mut self,
        relocate: impl FnMut(&ObjectInfo, StreamLocation) -> Result<(String, u64)>,
    ) -> Result<()>;
}

impl<P: TypeTreeProvider> MapStreamLocations for SerializedFileBuilder<'_, P> {
    fn map_stream_locations(
        &mut self,
        mut relocate: impl FnMut(&ObjectInfo, StreamLocation) -> Result<(String, u64)>,
    ) -> Result<()> {
        for (info, data) in self.objects.values_mut() {
            let tt = self
                .serialized
                .get_typetree_for(info, self.typetree_provider)?;
            if !tt.contains_type("StreamingInfo") && !tt.contains_type("StreamedResource") {
                continue;
            }

            let mut bytes = data.to_vec();
            relocate_streams(&mut bytes, &tt, |location| relocate(info, location))?;
            *data = bytes.into();
        }
        Ok(())
    }
}

enum Stream {
    Info(StreamingInfo),
    Resource(StreamedResource),
}

#[derive(Serialize, Deserialize)]
struct StreamingInfo {
    offset: u64,
    size: u32,
    path: String,
}

#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct StreamedResource {
    m_Source: String,
    m_Offset: u64,
    m_Size: u64,
}

impl Stream {
    fn location(&self) -> StreamLocation<'_> {
        match self {
            Stream::Info(info) => StreamLocation {
                path: &info.path,
                offset: info.offset,
                size: info.size.into(),
            },
            Stream::Resource(res) => StreamLocation {
                path: &res.m_Source,
                offset: res.m_Offset,
                size: res.m_Size,
            },
        }
    }

    fn set_location(&mut self, path: String, offset: u64) {
        match self {
            Stream::Info(info) => (info.path, info.offset) = (path, offset),
            Stream::Resource(res) => (res.m_Source, res.m_Offset) = (path, offset),
        }
    }

    fn write<W: std::io::Write + std::io::Seek>(
        &self,
        writer: &mut W,
        node: &TypeTreeNode,
    ) -> Result<()> {
        match self {
            Stream::Info(info) => {
                serde_typetree::to_writer::<_, _, LittleEndian>(writer, info, node)?
            }
            Stream::Resource(res) => {
                serde_typetree::to_writer::<_, _, LittleEndian>(writer, res, node)?
            }
        }
        Ok(())
    }
}

fn find_streams(
    data: &[u8],
    tt: &TypeTreeNode,
) -> Result<Vec<(Range<usize>, TypeTreeNode, Stream)>> {
    let mut streams = Vec::new();
    trace_pptr::visit::<_, LittleEndian>(
        &mut Cursor::new(data),
        tt,
        &NoRefTypes,
        &mut |node, reader| {
            let start = reader.position() as usize;
            let stream = match node.m_Type.as_str() {
                "StreamingInfo" => Stream::Info(read_typed(reader, node)?),
                "StreamedResource" => Stream::Resource(read_typed(reader, node)?),
                _ => return Ok(false),
            };
            streams.push((start..reader.position() as usize, node.clone(), stream));
            Ok(true)
        },
    )?;

    Ok(streams)
}

fn read_typed<T: serde::de::DeserializeOwned>(
    reader: &mut Cursor<&[u8]>,
    node: &TypeTreeNode,
) -> std::io::Result<T> {
    serde_typetree::from_reader::<_, LittleEndian>(reader, node).map_err(std::io::Error::other)
}
