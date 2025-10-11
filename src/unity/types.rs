#![allow(non_snake_case, dead_code)]

mod utils;

use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;

use indexmap::IndexMap;
use rabex::objects::pptr::{PPtr, TypedPPtr};
use rabex::objects::{ClassId, ClassIdType};
use rustc_hash::FxHashMap;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct BuildSettings {
    pub scenes: Vec<String>,
}
impl ClassIdType for BuildSettings {
    const CLASS_ID: ClassId = ClassId::BuildSettings;
}
impl BuildSettings {
    pub fn scene_name_lookup(&self) -> FxHashMap<String, usize> {
        self.scene_names()
            .enumerate()
            .map(|(i, name)| (name.to_owned(), i))
            .collect()
    }

    pub fn scene_names(&self) -> impl Iterator<Item = &str> {
        self.scenes
            .iter()
            .map(|scene_path| Path::new(scene_path).file_stem().unwrap().to_str().unwrap())
    }
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct PreloadData {
    pub m_Name: String,
    // order is irrelevant
    pub m_Assets: Vec<PPtr>,
    // order is irrelevant
    pub m_Dependencies: Vec<String>,
    pub m_ExplicitDataLayout: bool,
}
impl ClassIdType for PreloadData {
    const CLASS_ID: ClassId = ClassId::PreloadData;
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AssetBundle {
    pub m_Name: String,
    pub m_PreloadTable: Vec<PPtr>,
    pub m_Container: IndexMap<String, AssetInfo>,
    pub m_MainAsset: AssetInfo,
    pub m_RuntimeCompatibility: u32,
    pub m_AssetBundleName: String,
    // order irrelevant
    pub m_Dependencies: Vec<String>,
    pub m_IsStreamedSceneAssetBundle: bool,
    pub m_ExplicitDataLayout: i32,
    pub m_PathFlags: i32,
    // needs to be specified, value is flexible
    pub m_SceneHashes: IndexMap<String, String>,
}
impl ClassIdType for AssetBundle {
    const CLASS_ID: ClassId = ClassId::AssetBundle;
}

impl AssetBundle {
    /// Create a `AssetBundle` describing a scene asset bundle.
    /// The iterator specifies the list of scenes (and their scene hash).
    /// The path must begin with `Assets/` in order to load the scene.
    pub fn scene<'a>(
        name: &str,
        scenes: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> AssetBundle {
        let iter = scenes.into_iter();

        let mut bundle = AssetBundle::scene_base(name);
        for (path, scene_hash) in iter {
            bundle.add_scene(path, scene_hash);
        }

        bundle
    }

    /// Add a scene to this assetbundle.
    /// The path must begin with `Assets/` in order to load the scene.
    /// The scene hash must the same as the `hash` and `hash.sharedAssets` filenames in the bundle
    #[track_caller]
    pub fn add_scene(&mut self, path: &str, scene_hash: &str) {
        debug_assert!(self.m_IsStreamedSceneAssetBundle);
        self.m_Container
            .insert(path.to_owned(), AssetInfo::default());
        self.m_SceneHashes
            .insert(path.to_owned(), scene_hash.to_owned());
    }

    pub fn add_preloads<I: IntoIterator<Item = PPtr>>(&mut self, preloads: I) -> Range<usize> {
        let preload_index = self.m_PreloadTable.len();
        self.m_PreloadTable.extend(preloads);
        let preload_index_end = self.m_PreloadTable.len();
        preload_index..preload_index_end
    }

    pub fn scene_base(name: &str) -> AssetBundle {
        AssetBundle {
            m_Name: name.to_owned(),
            m_AssetBundleName: name.to_owned(),
            m_Container: IndexMap::default(),
            m_IsStreamedSceneAssetBundle: true,
            // TODO: investigate these
            m_RuntimeCompatibility: 1,
            m_ExplicitDataLayout: 1,
            m_SceneHashes: IndexMap::default(),
            ..Default::default()
        }
    }

    pub fn asset_base(name: &str) -> AssetBundle {
        AssetBundle {
            m_Name: name.to_owned(),
            m_AssetBundleName: name.to_owned(),
            m_IsStreamedSceneAssetBundle: false,
            // TODO: investigate these
            m_RuntimeCompatibility: 1,
            m_ExplicitDataLayout: 1,
            ..Default::default()
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct AssetInfo {
    pub preloadIndex: i32,
    pub preloadSize: i32,
    pub asset: PPtr,
}
impl AssetInfo {
    pub fn preload_range(&self) -> Range<usize> {
        self.preloadIndex as usize..(self.preloadIndex + self.preloadSize) as usize
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Transform {
    pub m_GameObject: TypedPPtr<GameObject>,
    pub m_LocalRotation: (f32, f32, f32, f32),
    pub m_LocalPosition: (f32, f32, f32),
    pub m_LocalScale: (f32, f32, f32),
    pub m_Children: Vec<TypedPPtr<Transform>>, // TODO recttransform
    pub m_Father: TypedPPtr<Transform>,
}
impl ClassIdType for Transform {
    const CLASS_ID: ClassId = ClassId::Transform;
}

#[derive(Debug, Deserialize)]
pub struct RectTransform {
    pub m_GameObject: TypedPPtr<GameObject>,
    pub m_LocalRotation: (f32, f32, f32, f32),
    pub m_LocalPosition: (f32, f32, f32),
    pub m_LocalScale: (f32, f32, f32),
    pub m_Children: Vec<TypedPPtr<Transform>>,
    pub m_Father: TypedPPtr<Transform>,
    pub m_AnchorMin: (f32, f32),
    pub m_AnchorMax: (f32, f32),
    pub m_AnchoredPosition: (f32, f32),
    pub m_SizeDelta: (f32, f32),
    pub m_Pivot: (f32, f32),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GameObject {
    pub m_Component: Vec<ComponentPair>,
    pub m_Layer: u32,
    pub m_Name: String,
    pub m_Tag: u16,
    pub m_IsActive: bool,
}
impl ClassIdType for GameObject {
    const CLASS_ID: ClassId = ClassId::GameObject;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComponentPair {
    pub component: PPtr,
}

#[derive(Debug, Deserialize)]
pub struct Component {
    pub m_GameObject: TypedPPtr<GameObject>,
}

impl ClassIdType for Component {
    const CLASS_ID: ClassId = ClassId::Component;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MonoBehaviour {
    pub m_GameObject: TypedPPtr<GameObject>,
    pub m_Enabled: u8,
    pub m_Script: TypedPPtr<MonoScript>,
    pub m_Name: String,
}
impl ClassIdType for MonoBehaviour {
    const CLASS_ID: ClassId = ClassId::MonoBehaviour;
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct MonoScript {
    pub m_Name: String,
    pub m_ExecutionOrder: i32,
    pub m_PropertiesHash: [u8; 16],
    pub m_ClassName: String,
    pub m_Namespace: String,
    pub m_AssemblyName: String,
}
impl MonoScript {
    pub fn assembly_name_base(&self) -> &str {
        self.m_AssemblyName.trim_end_matches(".dll")
    }

    pub fn assembly_name(&self) -> Cow<'_, str> {
        match self.m_AssemblyName.ends_with(".dll") {
            true => Cow::Borrowed(&self.m_AssemblyName),
            false => Cow::Owned(format!("{}.dll", self.m_AssemblyName)),
        }
    }

    pub fn full_name(&self) -> Cow<'_, str> {
        match self.m_Namespace.is_empty() {
            true => Cow::Borrowed(&self.m_ClassName),
            false => Cow::Owned(format!("{}.{}", self.m_Namespace, self.m_ClassName)),
        }
    }
}

impl ClassIdType for MonoScript {
    const CLASS_ID: ClassId = ClassId::MonoScript;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResourceManager {
    pub m_Container: IndexMap<String, PPtr>,
    pub m_DependentAssets: Vec<(PPtr, Vec<PPtr>)>,
}
impl ClassIdType for ResourceManager {
    const CLASS_ID: ClassId = ClassId::ResourceManager;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextAsset {
    pub m_Name: String,
    pub m_Script: String,
}
impl ClassIdType for TextAsset {
    const CLASS_ID: ClassId = ClassId::TextAsset;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MeshFilter {
    pub m_GameObject: TypedPPtr<GameObject>,
    pub m_Mesh: TypedPPtr<()>,
}
