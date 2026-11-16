use std::sync::Arc;

use indexmap::IndexMap;

use super::{
    AddressablesCatalog, AssemblyClass, AssetBundleRequestOptions, CatalogKey, CatalogString,
    ObjectInitializationData, ResourceLocation, SerializedObject, assemblies, resource_providers,
};

/// Builds an [`AddressablesCatalog`] of asset bundles and the assets in them
#[derive(Default)]
pub struct CatalogBuilder {
    locations: Vec<ResourceLocation>,
    resources: IndexMap<CatalogKey, Vec<usize>>,
}

impl CatalogBuilder {
    pub fn new() -> Self {
        CatalogBuilder::default()
    }

    /// Add the bundle loaded from the path `internal_id`, returning its location
    pub fn add_bundle(
        &mut self,
        primary_key: &str,
        internal_id: &str,
        options: AssetBundleRequestOptions,
    ) -> usize {
        self.add(ResourceLocation {
            internal_id: Arc::new(internal_id.to_owned()),
            provider_id: Arc::new(resource_providers::ASSET_BUNDLE.to_owned()),
            dependencies: Vec::new(),
            data: Some(SerializedObject {
                type_: Some(AssemblyClass::new(
                    assemblies::RESOURCE_MANAGER,
                    "UnityEngine.ResourceManagement.ResourceProviders.AssetBundleRequestOptions",
                )),
                object: Some(options),
            }),
            dependency_hash_code: 0,
            primary_key: Arc::new(primary_key.to_owned()),
            type_: AssemblyClass::new(
                assemblies::RESOURCE_MANAGER,
                "UnityEngine.ResourceManagement.ResourceProviders.IAssetBundleResource",
            ),
        })
    }

    /// Add the asset named `internal_id` in its bundle's `AssetBundle.m_Container`, returning its location.
    ///
    /// `dependencies` lists every bundle location the asset needs, its own first.
    pub fn add_asset(
        &mut self,
        primary_key: &str,
        internal_id: &str,
        type_: AssemblyClass,
        dependencies: Vec<usize>,
    ) -> usize {
        self.add(ResourceLocation {
            internal_id: Arc::new(internal_id.to_owned()),
            provider_id: Arc::new(resource_providers::BUNDLED_ASSET.to_owned()),
            dependencies,
            data: None,
            dependency_hash_code: 0,
            primary_key: Arc::new(primary_key.to_owned()),
            type_,
        })
    }

    pub fn add_key(&mut self, key: &str, location: usize) {
        let key = CatalogKey {
            type_: Some(AssemblyClass::new(assemblies::MSCORLIB, "System.String")),
            object: Some(CatalogString {
                value: Arc::new(key.to_owned()),
                separator: None,
            }),
        };
        self.resources.entry(key).or_default().push(location);
    }

    pub fn build(self, locator_id: &str, build_result_hash: &str) -> AddressablesCatalog {
        AddressablesCatalog {
            locator_id: Arc::new(locator_id.to_owned()),
            build_result_hash: Some(Arc::new(build_result_hash.to_owned())),
            instance_provider_data: provider(
                "UnityEngine.ResourceManagement.ResourceProviders.InstanceProvider",
            ),
            scene_provider_data: provider(
                "UnityEngine.ResourceManagement.ResourceProviders.SceneProvider",
            ),
            resource_provider_data: vec![
                provider(resource_providers::ASSET_BUNDLE),
                provider(resource_providers::BUNDLED_ASSET),
            ],
            locations: self.locations,
            resources: self.resources,
        }
    }

    fn add(&mut self, location: ResourceLocation) -> usize {
        let index = self.locations.len();
        let key = location.primary_key.clone();
        self.locations.push(location);
        self.add_key(&key, index);
        index
    }
}

fn provider(class: &str) -> ObjectInitializationData {
    ObjectInitializationData {
        id: Arc::new(class.to_owned()),
        object_type: AssemblyClass::new(assemblies::RESOURCE_MANAGER, class),
        data: None,
    }
}
