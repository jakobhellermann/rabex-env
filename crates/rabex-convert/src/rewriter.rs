//! One serialized file, parsed, with its original bytes: the unit rules transform.
//!
//! A rewriter is two parts: the [`Source`], the file as it was read, which objects and
//! references resolve against and which does not change; and the [`Output`], what is
//! written. Rules read from the one and write to the other, so what they read can stay
//! borrowed while they write. Objects no rule replaces carry their original bytes, and
//! the file's tables move from the source into the output when it is written.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use byteorder::LittleEndian;
use rabex::files::SerializedFile;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::files::serializedfile::{CommonOffsetMap, Endianness, ObjectInfo};
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex::{UnityVersion, serde_typetree};
use serde::Serialize;

pub struct SerializedFileRewriter<'a, P: TypeTreeProvider> {
    pub source: Source<'a, P>,
    pub output: Output<'a, P>,
}

/// The file as it was read.
pub struct Source<'a, P> {
    file: SerializedFile,
    data: &'a [u8],
    provider: &'a P,
    unity_version: UnityVersion,
}

/// What the file is written as.
pub struct Output<'a, P> {
    /// The objects; the tables come from the source when the file is written.
    builder: SerializedFileBuilder<'a, P>,
    unity_version: Option<UnityVersion>,
    /// The trees type entries were given, by the source's type index: what their objects
    /// are encoded against.
    retyped: HashMap<usize, TypeTreeNode>,
    /// Top-level fields of type entries' embedded trees renamed, by the source's index.
    renamed: HashMap<usize, Vec<(String, String)>>,
    /// Type entries objects were removed from, by the source's type index; the ones left
    /// without objects go when the file is written.
    emptied: BTreeSet<usize>,
    dirty: bool,
}

impl<'a, P: TypeTreeProvider> SerializedFileRewriter<'a, P> {
    /// Parse `data` as a serialized file; its objects are addressable until
    /// [`Self::finish`].
    pub fn new(
        data: &'a [u8],
        typetrees: &'a P,
        common_strings: &'a CommonOffsetMap<'a>,
    ) -> Result<Self> {
        Self::new_in_bundle(data, typetrees, common_strings, None)
    }

    /// [`Self::new`] for a file out of a bundle: files built that way carry `0.0.0` in
    /// their own header and read as the version their bundle knows.
    pub fn new_in_bundle(
        data: &'a [u8],
        typetrees: &'a P,
        common_strings: &'a CommonOffsetMap<'a>,
        bundle_version: Option<&UnityVersion>,
    ) -> Result<Self> {
        let mut cursor = Cursor::new(data);
        let file = match bundle_version {
            Some(version) => SerializedFile::from_reader_with_version(&mut cursor, version)?,
            None => SerializedFile::from_reader(&mut cursor)?,
        };
        ensure!(
            file.m_Header.m_Endianess == Endianness::Little,
            "big endian serialized file"
        );
        let unity_version = file
            .m_UnityVersion
            .clone()
            .or_else(|| bundle_version.cloned())
            .context("serialized file without Unity version")?;
        let mut builder = SerializedFileBuilder::from_serialized_meta(
            &unity_version,
            &file,
            typetrees,
            common_strings,
        );
        for info in file.objects() {
            let start = usize::try_from(info.m_Offset)?;
            let bytes = data
                .get(start..start + info.m_Size as usize)
                .context("object outside its file")?;
            builder
                .objects
                .insert(info.m_PathID, (info.clone(), Cow::Borrowed(bytes)));
        }
        Ok(Self {
            source: Source {
                file,
                data,
                provider: typetrees,
                unity_version,
            },
            output: Output {
                builder,
                unity_version: None,
                retyped: HashMap::new(),
                renamed: HashMap::new(),
                emptied: BTreeSet::new(),
                dirty: false,
            },
        })
    }

    /// The file's Unity version as read.
    pub fn unity_version(&self) -> &UnityVersion {
        &self.source.unity_version
    }

    /// Write the file's Unity version, for files whose objects were re-encoded for it.
    pub fn set_unity_version(&mut self, version: UnityVersion) {
        if self.source.unity_version != version {
            self.output.dirty = true;
        }
        self.output.unity_version = Some(version);
    }

    pub fn objects(&self) -> impl Iterator<Item = &ObjectInfo> + '_ {
        self.source.objects()
    }

    pub fn read<T: serde::de::DeserializeOwned>(&self, info: &ObjectInfo) -> Result<T> {
        self.source.read(info)
    }

    /// Replace one object, encoded against the tree of its type entry: the one
    /// [`Output::retype`] gave it, else the one it reads with.
    pub fn replace(&mut self, info: &ObjectInfo, value: &impl Serialize) -> Result<()> {
        let context = || format!("{:?} {}", info.m_ClassID, info.m_PathID);
        let bytes = match self.output.retyped.get(&type_index(info)?) {
            Some(tree) => serde_typetree::to_vec::<_, LittleEndian>(value, tree),
            None => serde_typetree::to_vec::<_, LittleEndian>(value, &*self.source.tree_of(info)?),
        }
        .with_context(context)?;
        self.output.replace_encoded(info, bytes)
    }

    /// Rename top-level fields of the type tree embedded for type entry `index`, `(from,
    /// to)`. The objects' bytes stay: a reader that reads by name (Unity's `SafeBinaryRead`
    /// does whenever a file embeds trees) finds the value under the new name. Returns the
    /// renames that applied.
    pub fn rename_type_fields(
        &mut self,
        index: usize,
        renames: &[(String, String)],
    ) -> Result<Vec<(String, String)>> {
        let ty = self.source.file.m_Types.get(index).context("type index")?;
        let Some(tree) = ty.m_Type.as_ref() else {
            return Ok(Vec::new());
        };
        let mut applied = Vec::new();
        for (from, to) in renames {
            ensure!(
                !tree.children.iter().any(|c| &c.m_Name == to),
                "{}: renaming {from} to {to}, which it already has",
                tree.m_Type
            );
            if tree.children.iter().any(|c| &c.m_Name == from) {
                applied.push((from.clone(), to.clone()));
            }
        }
        if !applied.is_empty() {
            self.output
                .renamed
                .entry(index)
                .or_default()
                .extend(applied.iter().cloned());
            self.output.dirty = true;
        }
        Ok(applied)
    }

    /// The rewritten file, `None` if no rule replaced anything or set the version.
    pub fn finish(self) -> Result<Option<Vec<u8>>> {
        let Self { source, output } = self;
        let Output {
            mut builder,
            unity_version,
            mut retyped,
            mut renamed,
            emptied,
            dirty,
        } = output;
        if !dirty {
            return Ok(None);
        }
        let file = source.file;
        let serialized = &mut builder.serialized;
        if let Some(version) = unity_version {
            serialized.m_UnityVersion = Some(version);
        }
        serialized.m_Externals = file.m_Externals;
        serialized.m_ScriptTypes = file.m_ScriptTypes;
        serialized.m_RefTypes = file.m_RefTypes;
        serialized.m_Types = file.m_Types;
        for (index, ty) in serialized.m_Types.iter_mut().enumerate() {
            if let Some(tree) = retyped.remove(&index).filter(|_| file.m_EnableTypeTree) {
                ty.m_OldTypeHash = tree.hash();
                ty.m_Type = Some(tree);
            }
            if let (Some(renames), Some(tree)) = (renamed.remove(&index), ty.m_Type.as_mut()) {
                for (from, to) in renames {
                    if let Some(child) = tree.children.iter_mut().find(|c| c.m_Name == from) {
                        child.m_Name = to;
                    }
                }
                ty.m_OldTypeHash = tree.hash();
            }
        }
        // last to first, so the indices still to look at stay the source's
        for &ty in emptied.iter().rev() {
            let ty_id = i32::try_from(ty)?;
            let objects = &mut builder.objects;
            if objects.values().any(|(other, _)| other.m_TypeID == ty_id) {
                continue;
            }
            builder.serialized.m_Types.remove(ty);
            for (other, _) in objects.values_mut() {
                if other.m_TypeID > ty_id {
                    other.m_TypeID -= 1;
                }
            }
        }
        Ok(Some(builder.write_vec()?))
    }
}

impl<'a, P: TypeTreeProvider> Source<'a, P> {
    /// The file as it was read, which references in it resolve against.
    pub fn file(&self) -> &SerializedFile {
        &self.file
    }

    /// The file's bytes.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn objects(&self) -> impl Iterator<Item = &ObjectInfo> + '_ {
        self.file.objects()
    }

    /// The type tree `info` reads with: the file's own or the provider's.
    pub fn tree_of(&self, info: &ObjectInfo) -> Result<Cow<'_, TypeTreeNode>> {
        self.file
            .get_typetree_for(info, self.provider)
            .with_context(|| format!("{:?} {}", info.m_ClassID, info.m_PathID))
    }

    /// The bytes of one object as the file has them.
    pub fn object_bytes(&self, info: &ObjectInfo) -> Result<&'a [u8]> {
        let start = usize::try_from(info.m_Offset)?;
        self.data
            .get(start..start + info.m_Size as usize)
            .context("object outside its file")
    }

    /// Read one object as `T` or as an untyped [`crate::value::Value`].
    ///
    /// The tree has to account for the object's every byte: a tree short of a field
    /// reads every field after it from the wrong place, and only the data can show it.
    pub fn read<T: serde::de::DeserializeOwned>(&self, info: &ObjectInfo) -> Result<T> {
        let tree = self.tree_of(info)?;
        let bytes = self.object_bytes(info)?;
        let exact = |tree: &TypeTreeNode| -> Result<Option<T>> {
            let mut cursor = Cursor::new(bytes);
            let value = serde_typetree::from_reader::<T, LittleEndian>(&mut cursor, tree)?;
            Ok((cursor.position() == bytes.len() as u64).then_some(value))
        };
        // a build writes fields no tree names. Which form the bytes are in shows only in
        // which tree reads them exactly; where both do, the values say different things
        // and nothing here can tell which is meant
        let plain = exact(&tree)?;
        let built = match crate::fields::build_tree(&tree) {
            Some(build) => exact(&build)?,
            None => None,
        };
        match (plain, built) {
            (Some(value), None) | (None, Some(value)) => Ok(value),
            (Some(_), Some(_)) => bail!(
                "{:?} {}: reads exactly both with and without the fields a build adds",
                info.m_ClassID,
                info.m_PathID
            ),
            (None, None) => bail!(
                "{:?} {}: the tree does not account for the object's {} bytes",
                info.m_ClassID,
                info.m_PathID,
                bytes.len()
            ),
        }
    }
}

impl<P> Output<'_, P> {
    /// Replace one object with `bytes`, encoded against the tree of its type entry.
    pub fn replace_encoded(&mut self, info: &ObjectInfo, bytes: Vec<u8>) -> Result<()> {
        let entry = self
            .builder
            .objects
            .get_mut(&info.m_PathID)
            .context("object vanished")?;
        entry.1 = Cow::Owned(bytes);
        self.dirty = true;
        Ok(())
    }

    /// Give type entry `index` (the source's) `tree`, which its objects are encoded
    /// against: the retarget shape, for a class that changed between Unity versions.
    /// Files that embed type trees get `tree` embedded, with its hash.
    pub fn retype(&mut self, index: usize, tree: TypeTreeNode) {
        self.retyped.insert(index, tree);
        self.dirty = true;
    }

    /// Take one object out of the file. A type entry no remaining object uses goes too
    /// when the file is written, and the type indices behind it move up, so the written
    /// file names no class it has no object of; until then every index is the source's.
    pub fn remove(&mut self, info: &ObjectInfo) -> Result<()> {
        self.builder
            .objects
            .remove(&info.m_PathID)
            .with_context(|| format!("{:?} {} vanished", info.m_ClassID, info.m_PathID))?;
        self.emptied.insert(type_index(info)?);
        self.dirty = true;
        Ok(())
    }
}

/// The source's type entry `info` belongs to.
fn type_index(info: &ObjectInfo) -> Result<usize> {
    usize::try_from(info.m_TypeID).context("type index")
}
