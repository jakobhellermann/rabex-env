//! Provider and cache for generated typetrees from assemblies
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Result;
use elsa::sync::FrozenMap;
use rabex::UnityVersion;
use rabex::objects::ClassId;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};

use crate::Environment;
use crate::resolver::EnvResolver;

pub struct AssemblyTypeTreeGenerator<'a, R, P> {
    env: &'a Environment<R, P>,
    generator: &'a unity_typetree_gen::AssemblyTypeTreeGenerator,
    base_node: &'a TypeTreeNode,
    cache: &'a FrozenMap<(String, String), Box<TypeTreeNode>>,
}

impl<'a, R: EnvResolver, P: TypeTreeProvider> AssemblyTypeTreeGenerator<'a, R, P> {
    pub fn generate(
        &self,
        assembly_name: &str,
        full_name: &str,
    ) -> Result<Option<&'a TypeTreeNode>> {
        let key = (assembly_name.to_owned(), full_name.to_owned());
        if let Some(value) = self.cache.get(&key) {
            return Ok(Some(value));
        }

        let (namespace, type_name) = full_name.rsplit_once('.').unwrap_or(("", full_name));
        let value = self
            .generator
            .generate(
                &|name| self.load_managed_assembly(name),
                assembly_name,
                namespace,
                type_name,
            )?
            .map(|mut node| {
                // prepend MonoBehaviour header
                node.children.splice(0..0, self.base_node.children.clone());
                node
            });
        let key = (assembly_name.to_owned(), full_name.to_owned());

        Ok(value.map(|value| self.cache.insert(key, Box::new(value))))
    }

    pub fn monobehaviour_definitions(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let definitions = self
            .generator
            .monobehaviour_definitions(&|name| self.load_managed_assembly(name))?;
        Ok(definitions)
    }

    pub fn load_assembly(&self, assembly: &str) -> Result<bool> {
        let added = self
            .generator
            .load_assembly(&|name| self.load_managed_assembly(name), assembly)?;
        Ok(added)
    }

    fn load_managed_assembly(&self, name: &str) -> Result<Vec<u8>, std::io::Error> {
        let data = self
            .env
            .game_files
            .read_path(&Path::new("Managed").join(name))?;
        // PERF: remove copy
        Ok(data.as_ref().to_vec())
    }
}

struct Backend {
    generator: unity_typetree_gen::AssemblyTypeTreeGenerator,
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
            generator: unity_typetree_gen::AssemblyTypeTreeGenerator::new(unity_version),
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

    pub fn backend<'a, R: EnvResolver, P: TypeTreeProvider>(
        &'a self,
        env: &'a Environment<R, P>,
    ) -> Result<AssemblyTypeTreeGenerator<'a, R, P>> {
        let backend = self.get_or_init(|| {
            let unity_version = env.unity_version()?;
            let base_node = env
                .tpk
                .get_typetree_node(ClassId::MonoBehaviour, unity_version)
                .expect("missing MonoBehaviour class")
                .into_owned();
            Ok((unity_version.clone(), base_node))
        })?;
        Ok(AssemblyTypeTreeGenerator {
            env,
            generator: &backend.generator,
            base_node: &backend.base_node,
            cache: &self.cache,
        })
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

    fn get_or_init(
        &self,
        init: impl FnOnce() -> Result<(UnityVersion, TypeTreeNode)>,
    ) -> Result<&Backend> {
        if let Some(backend) = self.backend.get() {
            return Ok(backend);
        }
        let (unity_version, base_node) = init()?;
        let generator = unity_typetree_gen::AssemblyTypeTreeGenerator::new(unity_version);
        // A racing initializer may win; either backend is equivalent.
        Ok(self.backend.get_or_init(|| Backend {
            generator,
            base_node,
        }))
    }
}
