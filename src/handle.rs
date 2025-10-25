use std::collections::{BTreeSet, VecDeque};
use std::fmt::Display;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use rabex::files::serializedfile::ObjectRef;
use rabex::files::{SerializedFile, serializedfile};
use rabex::objects::pptr::PathId;
use rabex::objects::{ClassId, ClassIdType, PPtr, TypedPPtr};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use serde::Deserialize;

use crate::Environment;
use crate::game_files::GameFiles;
use crate::resolver::EnvResolver;
use crate::unity::types::{GameObject, MonoBehaviour, MonoScript, Transform};

pub struct SerializedFileHandle<'a, R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>> {
    pub file: &'a SerializedFile,
    pub data: &'a [u8],
    pub env: &'a Environment<R, P>,
}
pub struct ObjectRefHandle<'a, T, R = GameFiles, P = TypeTreeCache<TpkTypeTreeBlob>> {
    pub object: ObjectRef<'a, T>,
    pub file: SerializedFileHandle<'a, R, P>,
}

impl<'a, T, R, P> std::fmt::Debug for ObjectRefHandle<'a, T, R, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectRefHandle")
            .field("object", &self.object.info)
            .finish()
    }
}
impl<'a, R, P> SerializedFileHandle<'a, R, P> {
    pub fn reborrow(&self) -> SerializedFileHandle<'a, R, P> {
        SerializedFileHandle {
            file: self.file,
            data: self.data,
            env: self.env,
        }
    }

    pub fn new(env: &'a Environment<R, P>, file: &'a SerializedFile, data: &'a [u8]) -> Self {
        SerializedFileHandle { file, data, env }
    }

    pub fn reader(&self) -> Cursor<&'a [u8]> {
        Cursor::new(self.data)
    }
}

impl<'a, R: EnvResolver, P: TypeTreeProvider> SerializedFileHandle<'a, R, P> {
    pub fn object_at<T>(&self, path_id: PathId) -> Result<ObjectRefHandle<'a, T, R, P>> {
        let object = self.file.get_object(path_id, &self.env.tpk)?;
        Ok(ObjectRefHandle::new(object, self.reborrow()))
    }

    pub fn find_object_of<T: ClassIdType + for<'de> Deserialize<'de>>(&self) -> Result<Option<T>> {
        let Some(data) = self.file.find_object_of::<T>(&self.env.tpk) else {
            return Ok(None);
        };
        Ok(Some(data.read(&mut self.reader())?))
    }

    pub fn objects<T>(&self) -> impl ExactSizeIterator<Item = ObjectRefHandle<'a, T, R, P>> {
        self.file.objects().map(|o| {
            let tt = self.file.get_typetree_for(o, &self.env.tpk);
            ObjectRefHandle::new(ObjectRef::new(self.file, o, tt), self.reborrow())
        })
    }

    pub fn objects_of<T>(&self) -> impl Iterator<Item = ObjectRefHandle<'a, T, R, P>>
    where
        T: ClassIdType,
    {
        let iter = self.file.objects_of::<T>(&self.env.tpk);
        iter.map(|o| ObjectRefHandle::new(o, self.reborrow()))
    }

    /// Returns `Transform`s and `RectTransform`s
    pub fn transforms(&self) -> impl Iterator<Item = ObjectRefHandle<'a, Transform, R, P>> {
        self.file
            .objects()
            .filter(|obj| {
                obj.m_ClassID == ClassId::Transform || obj.m_ClassID == ClassId::RectTransform
            })
            .map(move |o| {
                let tt = self.file.get_typetree_for(o, &self.env.tpk);
                ObjectRefHandle::new(ObjectRef::new(self.file, o, tt), self.reborrow())
            })
    }

    pub fn scripts<T>(
        &self,
        filter: impl ScriptFilter,
    ) -> Result<impl Iterator<Item = ObjectRefHandle<'a, T, R, P>>> {
        let mut script = None;
        for &script_type in self.file.m_ScriptTypes.as_deref().unwrap_or_default() {
            let script_data = self.env.deref_read(
                PPtr::from(script_type).typed::<MonoScript>(),
                self.file,
                &mut self.reader(),
            )?;
            if filter.matches(&script_data) {
                script = Some(PPtr::from(script_type));
            }
        }
        let script = match script {
            Some(script) => script,
            None => {
                let found = self
                    .file
                    .m_ScriptTypes
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|&script_type| -> Result<_> {
                        let script_data = self.env.deref_read(
                            PPtr::from(script_type).typed::<MonoScript>(),
                            self.file,
                            &mut self.reader(),
                        )?;
                        Ok(script_data.m_ClassName)
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");

                bail!("Script {filter} was not found in serialized file.\nFound {found}",)
            }
        };

        Ok(self
            .objects_of::<MonoBehaviour>()
            .filter(move |mb| self.file.script_type(mb.object.info) == Some(script))
            .map(|mb| mb.cast_owned::<T>()))
    }

    pub fn deref_optional<T: for<'de> Deserialize<'de>>(
        &self,
        pptr: TypedPPtr<T>,
    ) -> Result<Option<ObjectRefHandle<'a, T, R, P>>> {
        match pptr.optional() {
            Some(pptr) => self.deref(pptr).map(Some),
            None => Ok(None),
        }
    }

    pub fn deref_read_optional<T: for<'de> Deserialize<'de>>(
        &self,
        pptr: TypedPPtr<T>,
    ) -> Result<Option<T>> {
        self.deref_optional(pptr)?.map(|obj| obj.read()).transpose()
    }

    pub fn deref<T: for<'de> Deserialize<'de>>(
        &self,
        pptr: TypedPPtr<T>,
    ) -> Result<ObjectRefHandle<'a, T, R, P>> {
        Ok(match pptr.m_FileID.get_external(self.file) {
            None => {
                let object = pptr.deref_local(self.file, &self.env.tpk)?;
                ObjectRefHandle::new(object, self.reborrow())
            }
            Some(external_path) => {
                let external = self
                    .env
                    .load_external_file(Path::new(&external_path))
                    .with_context(|| format!("failed to load external file '{}'", external_path))?;
                let object = pptr
                    .make_local()
                    .deref_local(external.file, &self.env.tpk)
                    .with_context(|| format!("In external {} {}", pptr.m_FileID, external_path))?;
                ObjectRefHandle::new(object, external)
            }
        })
    }

    pub fn deref_read<T: for<'de> Deserialize<'de>>(&self, pptr: TypedPPtr<T>) -> Result<T> {
        self.deref(pptr)?.read()
    }
}

impl<'a, T, R: EnvResolver, P: TypeTreeProvider> ObjectRefHandle<'a, T, R, P> {
    pub fn new(object: ObjectRef<'a, T>, file: SerializedFileHandle<'a, R, P>) -> Self {
        ObjectRefHandle { object, file }
    }

    pub fn read(&self) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        if self.object.info.m_ClassID == ClassId::MonoBehaviour
            && self.file.env.typetree_generator.can_generate()
            && let Ok(tt) = &self.object.tt
            && tt.m_Type == "MonoBehaviour"
        {
            let with_tt = self.load_typetree()?;
            return Ok(with_tt.object.read(&mut self.file.reader())?);
        }

        let data = self.object.read(&mut self.file.reader())?;
        Ok(data)
    }

    pub fn typetree(&self) -> Result<&TypeTreeNode, serializedfile::Error> {
        self.object.typetree()
    }

    pub fn reachable(&self) -> Result<(BTreeSet<PathId>, BTreeSet<PPtr>)> {
        let reachable = crate::reachable::reachable(
            self.file.env,
            self.file.file,
            &mut self.file.reader(),
            VecDeque::from_iter(std::iter::once(self.path_id())),
        )?;
        Ok(reachable)
    }

    pub fn reachable_one(&self) -> Result<Vec<PPtr>> {
        let reachable = crate::reachable::reachable_one(
            self.file.env,
            self.file.file,
            self.path_id(),
            &mut self.file.reader(),
        )?;
        Ok(reachable)
    }
}

impl<'a, T, R, P> ObjectRefHandle<'a, T, R, P> {
    pub fn path_id(&self) -> PathId {
        self.object.info.m_PathID
    }

    pub fn class_id(&self) -> ClassId {
        self.object.info.m_ClassID
    }

    pub fn data(&self) -> &'a [u8] {
        &self.file.data[self.object.info.m_Offset as usize
            ..(self.object.info.m_Offset as usize + self.object.info.m_Size as usize)]
    }

    pub fn object_reader(&self) -> Cursor<&'a [u8]> {
        Cursor::new(self.data())
    }
}
impl<'a, R: EnvResolver, P: TypeTreeProvider> ObjectRefHandle<'a, GameObject, R, P> {
    pub fn path(&self) -> Result<String> {
        let path =
            self.read()?
                .path(self.file.file, &mut self.file.reader(), &self.file.env.tpk)?;
        Ok(path)
    }
}

impl<'a, T, R: EnvResolver, P: TypeTreeProvider> ObjectRefHandle<'a, T, R, P> {
    pub fn cast<U>(&'a self) -> ObjectRefHandle<'a, U, R, P> {
        ObjectRefHandle {
            object: self.object.cast(),
            file: self.file.reborrow(),
        }
    }

    pub fn cast_owned<U>(self) -> ObjectRefHandle<'a, U, R, P> {
        ObjectRefHandle {
            object: self.object.cast_owned(),
            file: self.file.reborrow(),
        }
    }

    fn load_typetree(&'a self) -> Result<ObjectRefHandle<'a, T, R, P>>
    where
        for<'de> T: Deserialize<'de>,
    {
        let script = self
            .mono_script()?
            .with_context(|| format!("MonoBehaviour {} has no MonoScript", self.path_id()))?;
        self.load_typetree_as(&script)
    }

    fn load_typetree_as<U>(&'a self, script: &MonoScript) -> Result<ObjectRefHandle<'a, U, R, P>>
    where
        U: for<'de> Deserialize<'de>,
    {
        let data = self
            .file
            .env
            .load_typetree_as(&self.object.cast(), script)?;

        Ok(ObjectRefHandle {
            object: data,
            file: self.file.reborrow(),
        })
    }

    pub fn mono_script(&self) -> Result<Option<MonoScript>> {
        let Some(script_type) = self.file.file.script_type(self.object.info) else {
            return Ok(None);
        };

        self.file
            .env
            .deref_read(script_type.typed(), self.file.file, &mut self.file.reader())
    }
}

pub trait ScriptFilter: Display {
    fn matches(&self, script: &MonoScript) -> bool;
}
impl ScriptFilter for &dyn ScriptFilter {
    fn matches(&self, script: &MonoScript) -> bool {
        (**self).matches(script)
    }
}
impl<T: ScriptFilter> ScriptFilter for &T {
    fn matches(&self, script: &MonoScript) -> bool {
        (**self).matches(script)
    }
}
impl ScriptFilter for &'_ str {
    fn matches(&self, script: &MonoScript) -> bool {
        script.m_ClassName == *self
    }
}

pub struct ScriptFilterContains<'a>(pub &'a str);
impl Display for ScriptFilterContains<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "containing {}", self.0)
    }
}
impl ScriptFilter for ScriptFilterContains<'_> {
    fn matches(&self, script: &MonoScript) -> bool {
        script.m_ClassName.contains(self.0)
    }
}
