//! Find and replace PPtr references in arbitrary objects
use anyhow::Result;
use byteorder::{BigEndian, ByteOrder, LittleEndian, ReadBytesExt};
use rabex::files::serializedfile::Endianness;
use rabex::objects::PPtr;
use rabex::objects::pptr::{FileId, PathId};
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rustc_hash::FxHashMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

use crate::Environment;
use crate::resolver::EnvResolver;

pub fn trace_pptrs_endianned(
    tt: &TypeTreeNode,
    reader: &mut (impl Read + Seek),
    endianness: Endianness,
    ref_types: &impl RefTypes,
) -> Result<Vec<PPtr>> {
    match endianness {
        Endianness::Little => trace_pptrs::<LittleEndian>(tt, reader, ref_types),
        Endianness::Big => trace_pptrs::<BigEndian>(tt, reader, ref_types),
    }
}

#[inline(never)]
pub fn trace_pptrs<B: ByteOrder>(
    tt: &TypeTreeNode,
    reader: &mut (impl Read + Seek),
    ref_types: &impl RefTypes,
) -> Result<Vec<PPtr>> {
    let mut pptrs = Vec::new();
    visit::<_, B>(reader, tt, ref_types, &mut |tt, reader| {
        if tt.m_Type.starts_with("PPtr<") && tt.m_Name != "m_Father" {
            let file_id = reader.read_i32::<B>()?;
            let path_id = reader.read_i64::<B>()?;
            if tt.requires_align() {
                reader.align4()?;
            }

            let pptr = PPtr {
                m_FileID: file_id.into(),
                m_PathID: path_id,
            };
            if !pptr.is_null() {
                pptrs.push(pptr);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    })?;

    Ok(pptrs)
}

#[inline(never)]
pub fn replace_pptrs_inplace_endianed(
    value: &mut [u8],
    ty: &TypeTreeNode,
    path_id_remap: &FxHashMap<PathId, PathId>,
    file_id_remap: &FxHashMap<FileId, FileId>,
    endianness: Endianness,
    ref_types: &impl RefTypes,
) -> Result<()> {
    match endianness {
        Endianness::Little => replace_pptrs_inplace::<LittleEndian>(
            value,
            ty,
            path_id_remap,
            file_id_remap,
            ref_types,
        ),
        Endianness::Big => {
            replace_pptrs_inplace::<BigEndian>(value, ty, path_id_remap, file_id_remap, ref_types)
        }
    }
}

/// Replace every PPtr in `value` using `f`
pub fn map_pptrs_inplace(
    value: &mut [u8],
    ty: &TypeTreeNode,
    endianness: Endianness,
    ref_types: &impl RefTypes,
    f: impl FnMut(&TypeTreeNode, PPtr) -> Result<PPtr>,
) -> Result<()> {
    match endianness {
        Endianness::Little => map_pptrs_inplace_inner::<LittleEndian>(value, ty, ref_types, f),
        Endianness::Big => map_pptrs_inplace_inner::<BigEndian>(value, ty, ref_types, f),
    }
}

fn map_pptrs_inplace_inner<B: ByteOrder>(
    value: &mut [u8],
    ty: &TypeTreeNode,
    ref_types: &impl RefTypes,
    mut f: impl FnMut(&TypeTreeNode, PPtr) -> Result<PPtr>,
) -> Result<()> {
    let len = value.len();
    let mut cursor = Cursor::new(value);
    let mut error = None;
    let visited = visit::<_, B>(&mut cursor, ty, ref_types, &mut |tt, reader| {
        if !tt.m_Type.starts_with("PPtr<") {
            return Ok(false);
        }
        let pos = reader.position() as usize;
        let pptr = PPtr {
            m_FileID: reader.read_i32::<B>()?.into(),
            m_PathID: reader.read_i64::<B>()?,
        };
        if tt.requires_align() {
            reader.align4()?;
        }

        let new = f(tt, pptr).map_err(|e| std::io::Error::other(error.insert(e).to_string()))?;
        B::write_i32(&mut reader.get_mut()[pos..], new.m_FileID.value());
        B::write_i64(&mut reader.get_mut()[pos + 4..], new.m_PathID);
        Ok(true)
    });
    if let Some(error) = error {
        return Err(error);
    }
    visited?;
    anyhow::ensure!(
        cursor.position() as usize == len,
        "type tree covers {} of {len} bytes",
        cursor.position()
    );
    Ok(())
}

pub fn replace_pptrs_inplace<B: ByteOrder>(
    value: &mut [u8],
    ty: &TypeTreeNode,
    path_id_remap: &FxHashMap<PathId, PathId>,
    file_id_remap: &FxHashMap<FileId, FileId>,
    ref_types: &impl RefTypes,
) -> Result<()> {
    visit::<_, B>(&mut Cursor::new(value), ty, ref_types, &mut |tt, reader| {
        if tt.m_Type.starts_with("PPtr<") {
            let pos = reader.position() as usize;

            let file_id = FileId::from(reader.read_i32::<B>()?);
            let path_id = reader.read_i64::<B>()?;
            if tt.requires_align() {
                reader.align4()?;
            }

            if file_id.is_local() {
                if let Some(&replacement) = path_id_remap.get(&path_id) {
                    B::write_i64(&mut reader.get_mut()[pos + 4..], replacement);
                }
            } else if let Some(&replacement) = file_id_remap.get(&file_id) {
                B::write_i32(&mut reader.get_mut()[pos..], replacement.value());
            }

            Ok(true)
        } else {
            Ok(false)
        }
    })?;

    Ok(())
}

/// A managed type a `[SerializeReference]` field holds an instance of.
#[derive(Debug)]
pub struct ManagedType {
    pub class: String,
    pub namespace: String,
    pub assembly: String,
}

/// The type trees of the managed types `[SerializeReference]` fields hold instances of.
pub trait RefTypes {
    fn type_tree(&self, ty: &ManagedType) -> Result<&TypeTreeNode, std::io::Error>;
}

/// Knows no managed types, so walking `[SerializeReference]` data fails.
pub struct NoRefTypes;

impl RefTypes for NoRefTypes {
    fn type_tree(&self, ty: &ManagedType) -> Result<&TypeTreeNode, std::io::Error> {
        Err(std::io::Error::other(format!(
            "no type tree for the [SerializeReference] data of {ty:?}"
        )))
    }
}

pub fn visit<R, B>(
    reader: &mut R,
    tt: &TypeTreeNode,
    ref_types: &impl RefTypes,
    f: &mut impl FnMut(&TypeTreeNode, &mut R) -> Result<bool, std::io::Error>,
) -> Result<(), std::io::Error>
where
    R: Read + Seek,
    B: ByteOrder,
{
    let size = match tt.m_Type.as_str() {
        "bool" => 1,
        "UInt8" => 1,
        "UInt16" | "unsigned short" => 2,
        "UInt32" | "unsigned int" | "Type*" => 4,
        "UInt64" | "unsigned long long" | "FileSize" => 8,
        "SInt8" => 1,
        "SInt16" | "short" => 2,
        "SInt32" | "int" => 4,
        "SInt64" | "long long" => 8,
        "float" => 4,
        "double" => 8,
        "char" => 1,
        "string" => {
            let length = reader.read_u32::<B>()?;
            reader.seek_relative(length as i64)?;
            reader.align4()?;
            return Ok(());
        }
        "map" => {
            let length = reader.read_u32::<B>()?;

            let pair = &tt.children[0].children[1];
            let key_type = &pair.children[0];
            let value_type = &pair.children[1];

            if tt.requires_align() || pair.requires_align() {
                reader.align4()?;
            }

            for _ in 0..length {
                visit::<_, B>(reader, key_type, ref_types, f)?;
                visit::<_, B>(reader, value_type, ref_types, f)?;
            }

            return Ok(());
        }
        "TypelessData" => {
            let len = reader.read_u32::<B>()?;
            reader.seek(SeekFrom::Current(len as i64))?;
            return Ok(());
        }
        // `rid`, then the managed type, then the data laid out by that type
        "ReferencedObject" => {
            let [_, _, data] = tt.children.as_slice() else {
                return Err(std::io::Error::other(
                    "ReferencedObject without rid, type and data",
                ));
            };
            reader.seek_relative(8)?;
            let ty = ManagedType {
                class: read_string::<_, B>(reader)?,
                namespace: read_string::<_, B>(reader)?,
                assembly: read_string::<_, B>(reader)?,
            };
            // an empty type marks a null reference, which has no data
            if !ty.class.is_empty() {
                visit::<_, B>(reader, ref_types.type_tree(&ty)?, ref_types, f)?;
            }
            if data.requires_align() {
                reader.align4()?;
            }
            return Ok(());
        }
        "ManagedReferencesRegistry"
            if !matches!(tt.children.as_slice(), [version, ids]
                if version.m_Name == "version" && ids.m_Name == "RefIds") =>
        {
            return Err(std::io::Error::other(
                "ManagedReferencesRegistry without RefIds not implemented",
            ));
        }
        _ => {
            if let [child] = tt.children.as_slice()
                && child.m_Type == "Array"
            {
                let item_type = &child.children[1];
                let length = reader.read_u32::<B>()?;
                for _ in 0..length {
                    visit::<_, B>(reader, item_type, ref_types, f)?;
                }
                if tt.requires_align() || child.requires_align() {
                    reader.align4()?;
                }

                return Ok(());
            }

            if f(tt, reader)? {
                return Ok(());
            }

            for child in &tt.children {
                visit::<_, B>(reader, child, ref_types, f)?;
            }

            if tt.requires_align() {
                reader.align4()?;
            }

            return Ok(());
        }
    };

    reader.seek_relative(size)?;
    if tt.requires_align() {
        reader.align4()?;
    }

    Ok(())
}

trait SeekExt: Seek {
    fn align(&mut self, align: usize) -> Result<(), std::io::Error> {
        let pos = self.stream_position()?;
        let new_pos = (pos + align as u64 - 1) & !(align as u64 - 1);
        let diff = new_pos - pos;
        if diff > 0 {
            self.seek(std::io::SeekFrom::Current(diff as i64))?;
        }
        Ok(())
    }

    fn align4(&mut self) -> Result<(), std::io::Error> {
        self.align(4)
    }
}

impl<R: Read + Seek + ?Sized> SeekExt for R {}

fn read_string<R: Read + Seek, B: ByteOrder>(reader: &mut R) -> Result<String, std::io::Error> {
    let length = reader.read_u32::<B>()? as usize;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    reader.align4()?;
    String::from_utf8(bytes).map_err(std::io::Error::other)
}

/// Generates the type trees of `[SerializeReference]` data from the game's assemblies.
impl<R: EnvResolver, P: TypeTreeProvider> RefTypes for Environment<R, P> {
    fn type_tree(&self, ty: &ManagedType) -> Result<&TypeTreeNode, std::io::Error> {
        let full_name = match ty.namespace.is_empty() {
            true => ty.class.clone(),
            false => format!("{}.{}", ty.namespace, ty.class),
        };
        let assembly = format!("{}.dll", ty.assembly.trim_end_matches(".dll"));
        self.generate_typetree(&assembly, &full_name)
            .map_err(std::io::Error::other)?
            .ok_or_else(|| std::io::Error::other(format!("no type tree for {ty:?}")))
    }
}
