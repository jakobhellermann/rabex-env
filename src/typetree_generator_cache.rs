use std::sync::OnceLock;

use anyhow::Result;
use elsa::sync::FrozenMap;
use rabex::UnityVersion;
use rabex::typetree::TypeTreeNode;
use unity_typetree_gen::{AssemblyTypeTreeGenerator, Loader};

struct Backend {
    generator: AssemblyTypeTreeGenerator,
    base_node: TypeTreeNode,
}

pub struct TypeTreeGeneratorCache {
    backend: OnceLock<Backend>,
    cache: FrozenMap<(String, String), Box<TypeTreeNode>>,
}
impl TypeTreeGeneratorCache {
    pub fn new(unity_version: UnityVersion, base_node: TypeTreeNode) -> Self {
        let backend = OnceLock::new();
        let _ = backend.set(Backend {
            generator: AssemblyTypeTreeGenerator::new(unity_version),
            base_node,
        });
        TypeTreeGeneratorCache {
            backend,
            cache: FrozenMap::default(),
        }
    }
    pub fn prefilled(cache: FrozenMap<(String, String), Box<TypeTreeNode>>) -> Self {
        TypeTreeGeneratorCache {
            backend: OnceLock::new(),
            cache,
        }
    }
    pub fn empty() -> Self {
        TypeTreeGeneratorCache {
            backend: OnceLock::new(),
            cache: FrozenMap::default(),
        }
    }

    pub fn insert_cache<'a>(
        &'a self,
        assembly_name: &str,
        full_name: &str,
        ty: TypeTreeNode,
    ) -> &'a TypeTreeNode {
        let key = (assembly_name.to_owned(), full_name.to_owned());
        self.cache.insert(key, Box::new(ty))
    }

    pub(crate) fn generate(
        &self,
        init: impl FnOnce() -> Result<(UnityVersion, TypeTreeNode)>,
        loader: &Loader,
        assembly_name: &str,
        full_name: &str,
    ) -> Result<Option<&TypeTreeNode>> {
        let key = (assembly_name.to_owned(), full_name.to_owned());
        if let Some(value) = self.cache.get(&key) {
            return Ok(Some(value));
        }

        let backend = self.get_or_init(init)?;
        let (namespace, type_name) = full_name.rsplit_once('.').unwrap_or(("", full_name));
        let value = backend
            .generator
            .generate(loader, assembly_name, namespace, type_name)?
            .map(|mut node| {
                // prepend MonoBehaviour header
                node.children
                    .splice(0..0, backend.base_node.children.clone());
                node
            });
        Ok(value.map(|value| self.cache.insert(key, Box::new(value))))
    }

    fn get_or_init(
        &self,
        init: impl FnOnce() -> Result<(UnityVersion, TypeTreeNode)>,
    ) -> Result<&Backend> {
        if let Some(backend) = self.backend.get() {
            return Ok(backend);
        }
        let (unity_version, base_node) = init()?;
        let generator = AssemblyTypeTreeGenerator::new(unity_version);
        // A racing initializer may win; either backend is equivalent.
        Ok(self.backend.get_or_init(|| Backend {
            generator,
            base_node,
        }))
    }
}
