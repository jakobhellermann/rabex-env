#![allow(dead_code)]
mod utils;

use anyhow::{Context as _, Result};
use indexmap::IndexMap;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex_env::Environment;
use rabex_env::typetree_merge::MergedTypeTree;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;

type TypeName = String;
type RustCode = String;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config<'a> {
    /// One or more game installs to merge into a single definition. Fields that are not
    /// present in every game are generated as `Option<_>` with `#[serde(default)]`.
    game_paths: Vec<String>,
    #[serde(borrow)]
    #[serde(default)]
    field_ignores: Vec<&'a str>,
    scripts: IndexMap<String, Vec<String>>,

    #[serde(default)]
    class_ids: Vec<&'a str>,
    #[serde(default)]
    untyped_pptr_fields: Vec<String>,
}

fn main() -> Result<()> {
    let config = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or("examples/unity2rust.toml".into());
    let config = std::fs::read_to_string(&config).context("Failed to read config")?;
    let config = toml::from_str::<Config>(&config)?;

    let home = std::env::home_dir().unwrap();
    let envs = config
        .game_paths
        .iter()
        .map(|game_path| {
            let game_path = game_path.replace("~", home.to_str().unwrap());
            let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
            Environment::new_in(game_path, tpk)
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!envs.is_empty(), "`game_paths` must list at least one game");

    // human-readable source label per game (the install folder name), used to annotate
    // fields that only exist in a subset of the games
    let labels: Vec<String> = config
        .game_paths
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(p)
                .to_string()
        })
        .collect();

    let settings = Settings {
        derives: Some("Debug, serde::Deserialize"),
        field_ignores: &config.field_ignores,
        untyped_pptr_fields: &config.untyped_pptr_fields,
        additional_fields: HashMap::from_iter([(
            "SavedItem",
            [("displayName", "Option<LocalisedString>")].as_slice(),
        )]),
    };
    let mut cx = Context::new(&envs, &labels, settings);

    for class_id in config.class_ids {
        let class_id = ClassId::from_str(class_id)?;
        cx.generate_classid(class_id)?;
    }
    for (assembly, scripts) in &config.scripts {
        for script in scripts {
            cx.generate_script(assembly, script)?;
        }
    }
    cx.finish(std::io::stdout().lock())?;

    Ok(())
}

struct Context<'a> {
    /// the games to merge into one definition; non-shared fields become `Option<_>`
    envs: &'a [Environment],
    /// human-readable label per env (same order), for annotating subset-only fields
    labels: &'a [String],
    settings: Settings<'a>,

    generated: FxHashSet<String>,
    generated_code: Vec<(TypeName, RustCode)>,
    class_id_types: BTreeMap<TypeName, ClassId>,
    hash_key_types: FxHashSet<TypeName>,
    script_pptr_types: FxHashMap<(String, String), Option<TypeName>>,
    queued: VecDeque<(String, MergedTypeTree)>,
    /// assembly the type currently being generated belongs to
    current_assembly: String,
    /// all loaded assembly names per env, lazily populated
    all_assemblies: Vec<Option<Vec<String>>>,
}

struct Settings<'a> {
    derives: Option<&'a str>,
    field_ignores: &'a [&'a str],
    untyped_pptr_fields: &'a [String],
    additional_fields: HashMap<&'a str, &'a [(&'a str, &'a str)]>,
}

impl<'a> Context<'a> {
    pub fn new(envs: &'a [Environment], labels: &'a [String], settings: Settings<'a>) -> Self {
        Context {
            envs,
            labels,
            settings,
            generated: FxHashSet::default(),
            generated_code: Vec::new(),
            class_id_types: BTreeMap::new(),
            hash_key_types: FxHashSet::default(),
            script_pptr_types: FxHashMap::default(),
            queued: VecDeque::new(),
            current_assembly: String::new(),
            all_assemblies: vec![None; envs.len()],
        }
    }
    pub fn finish<W: Write>(&mut self, mut writer: W) -> Result<()> {
        self.handle_queue()?;
        writeln!(
            writer,
            "#![allow(dead_code, unused_imports, non_snake_case, nonstandard_style)]"
        )?;
        writeln!(
            writer,
            "use rabex_env::rabex::objects::{{ClassId, ClassIdType, PPtr, TypedPPtr}};"
        )?;
        writeln!(writer, "use rabex_env::unity::types::*;")?;
        if self
            .generated_code
            .iter()
            .any(|(_, code)| code.contains("HashMap<"))
        {
            writeln!(writer, "use std::collections::HashMap;")?;
        }
        writeln!(writer)?;
        // each block already ends with a newline from generate_inner; separate blocks
        // with a blank line but don't emit a trailing one (to match rustfmt)
        for (i, (type_name, code)) in self.generated_code.iter().enumerate() {
            if i > 0 {
                writeln!(writer)?;
            }
            write!(writer, "{code}")?;
            if let Some(class_id) = self.class_id_types.get(type_name) {
                let class_id_name = class_id.name().context("class ID has no name")?;
                writeln!(writer)?;
                writeln!(writer, "impl ClassIdType for {type_name} {{")?;
                writeln!(
                    writer,
                    "    const CLASS_ID: ClassId = ClassId::{class_id_name};"
                )?;
                writeln!(writer, "}}")?;
            }
        }
        Ok(())
    }
    pub fn generate(&mut self, assembly: &str, tt: MergedTypeTree) -> Result<()> {
        self.queue(assembly, tt);
        Ok(())
    }
    pub fn generate_classid(&mut self, class_id: ClassId) -> Result<()> {
        let mut nodes = Vec::new();
        for env in self.envs {
            if let Some(tt) = env.tpk.get_typetree_node(class_id, env.unity_version()?) {
                nodes.push(tt);
            }
        }
        let merged = MergedTypeTree::merge(nodes.iter().map(|tt| tt.as_ref()))?
            .context("typetree not found")?;
        let type_name = self.escape_typename(&merged);
        self.class_id_types.insert(type_name, class_id);
        self.generate("Assembly-CSharp", merged)
    }
    pub fn generate_script(&mut self, assembly: &str, script: &str) -> Result<()> {
        let merged = self
            .merged_typetree(assembly, script)?
            .with_context(|| format!("type tree not found for {script} ({assembly})"))?;
        self.generate(assembly, merged)
    }

    /// Resolve `script` in `assembly` across every env and merge the results into one tree.
    fn merged_typetree(&self, assembly: &str, script: &str) -> Result<Option<MergedTypeTree>> {
        let mut nodes = Vec::new();
        for env in self.envs {
            if let Some(tt) = env.generate_typetree(assembly, script)? {
                nodes.push(tt);
            }
        }
        Ok(MergedTypeTree::merge(nodes)?)
    }

    fn queue(&mut self, assembly: &str, tt: MergedTypeTree) {
        if self.generated.contains(&tt.m_Type) {
            return;
        }
        assert!(self.generated.insert(tt.m_Type.clone()));

        self.queued.push_back((assembly.to_owned(), tt));
    }

    fn discover_type_dependencies(&mut self, tt: &MergedTypeTree) -> Result<()> {
        for field in &tt.children {
            if self.ignore_field(field) {
                continue;
            }
            let untyped_pptr = self.settings.untyped_pptr_fields.iter().any(|entry| {
                entry.split_once('.') == Some((tt.m_Type.as_str(), field.m_Name.as_str()))
            });
            self.discover_field_dependencies(field, untyped_pptr)?;
        }
        Ok(())
    }

    fn discover_field_dependencies(
        &mut self,
        field: &MergedTypeTree,
        untyped_pptr: bool,
    ) -> Result<()> {
        match self.classify(field) {
            Classify::Primitive(_) => {}
            Classify::PPtr(_) if untyped_pptr => {}
            Classify::PPtr(pptr) => {
                if let Some(asm_ty) = pptr.strip_prefix('$') {
                    let key = (self.current_assembly.clone(), asm_ty.to_owned());
                    if !self.script_pptr_types.contains_key(&key) {
                        let resolved = self.resolve_script_type(asm_ty)?;
                        let type_name = match resolved {
                            Some((assembly, ty))
                                if !(ty.m_Name == "Base" && ty.m_Type == "MonoBehaviour") =>
                            {
                                let type_name = self.escape_typename(&ty);
                                self.queue(&assembly, ty);
                                Some(type_name)
                            }
                            _ => None,
                        };
                        self.script_pptr_types.insert(key, type_name);
                    }
                }
            }
            Classify::Other(other) => {
                let assembly = self.current_assembly.clone();
                self.queue(&assembly, other.clone());
            }
            Classify::Array(item) => {
                self.discover_field_dependencies(item, untyped_pptr)?;
            }
            Classify::Map { key, value } => {
                let mut key_types = FxHashSet::default();
                self.collect_hash_key_dependencies(key, &mut key_types);
                self.hash_key_types.extend(key_types);
                self.discover_field_dependencies(key, untyped_pptr)?;
                self.discover_field_dependencies(value, untyped_pptr)?;
            }
        }
        Ok(())
    }

    fn collect_hash_key_dependencies(
        &self,
        field: &MergedTypeTree,
        dependencies: &mut FxHashSet<TypeName>,
    ) {
        match self.classify(field) {
            Classify::Other(other) => {
                dependencies.insert(other.m_Type.clone());
            }
            Classify::Array(item) => self.collect_hash_key_dependencies(item, dependencies),
            Classify::Map { key, value } => {
                self.collect_hash_key_dependencies(key, dependencies);
                self.collect_hash_key_dependencies(value, dependencies);
            }
            Classify::Primitive(_) | Classify::PPtr(_) => {}
        }
    }

    fn propagate_hash_key_derives(&mut self, discovered: &[(String, MergedTypeTree)]) {
        loop {
            let mut dependencies = FxHashSet::default();
            for (_, tt) in discovered {
                if !self.hash_key_types.contains(&tt.m_Type) {
                    continue;
                }
                for field in &tt.children {
                    if !self.ignore_field(field) {
                        self.collect_hash_key_dependencies(field, &mut dependencies);
                    }
                }
            }

            let old_len = self.hash_key_types.len();
            self.hash_key_types.extend(dependencies);
            if self.hash_key_types.len() == old_len {
                break;
            }
        }
    }

    fn handle_queue(&mut self) -> Result<()> {
        let mut discovered = Vec::new();
        while let Some((assembly, item)) = self.queued.pop_front() {
            self.current_assembly = assembly.clone();
            self.discover_type_dependencies(&item)?;
            discovered.push((assembly, item));
        }

        self.propagate_hash_key_derives(&discovered);

        for (assembly, item) in discovered {
            self.current_assembly = assembly;
            let code = self.generate_inner(&item)?;
            let type_name = self.escape_typename(&item);
            self.generated_code.push((type_name, code));
        }

        Ok(())
    }

    /// All loaded assembly names for env `env_index`, lazily computed and cached.
    fn assemblies(&mut self, env_index: usize) -> Result<&[String]> {
        if self.all_assemblies[env_index].is_none() {
            let envs = self.envs;
            let env = &envs[env_index];
            let keys = env
                .typetree_generator
                .backend(env)?
                .monobehaviour_definitions()?
                .into_keys()
                .collect();
            self.all_assemblies[env_index] = Some(keys);
        }
        Ok(self.all_assemblies[env_index].as_deref().unwrap())
    }

    /// Look up a MonoScript type by name in every env (preferring the current assembly,
    /// then any other loaded assembly) and merge the results. Returns the assembly it was
    /// first found in and the merged type tree.
    fn resolve_script_type(&mut self, full_name: &str) -> Result<Option<(String, MergedTypeTree)>> {
        let current = self.current_assembly.clone();
        let envs = self.envs;
        let mut found_assembly: Option<String> = None;
        let mut variants: Vec<&TypeTreeNode> = Vec::new();

        for (i, env) in envs.iter().enumerate() {
            let resolved = if let Some(ty) = env.generate_typetree(&current, full_name)? {
                Some((current.clone(), ty))
            } else {
                let mut found = None;
                for assembly in self.assemblies(i)?.to_vec() {
                    if assembly == current {
                        continue;
                    }
                    if let Some(ty) = env.generate_typetree(&assembly, full_name)? {
                        found = Some((assembly, ty));
                        break;
                    }
                }
                found
            };
            if let Some((assembly, ty)) = resolved {
                found_assembly.get_or_insert(assembly);
                variants.push(ty);
            }
        }

        match found_assembly {
            Some(assembly) => {
                let merged = MergedTypeTree::merge(variants)?.expect("variants is non-empty");
                Ok(Some((assembly, merged)))
            }
            None => Ok(None),
        }
    }

    fn ignore_field(&self, field: &MergedTypeTree) -> bool {
        self.settings.field_ignores.iter().any(|ignore| {
            field
                .m_Name
                .to_ascii_lowercase()
                .contains(&ignore.to_ascii_lowercase())
        })
    }

    fn generate_inner(&mut self, tt: &MergedTypeTree) -> Result<RustCode> {
        // eprintln!("Generating {} {}", tt.type_name, tt.name);
        let mut f = String::new();
        let type_name = self.escape_typename(tt);
        let derives = self.derives_for_type(&tt.m_Type);
        if !derives.is_empty() {
            writeln!(&mut f, "#[derive({derives})]")?;
        }
        writeln!(&mut f, "pub struct {type_name} {{")?;
        for field in &tt.children {
            // eprintln!("Field {} {}", field.type_name, field.name);
            if self.ignore_field(field) {
                continue;
            }
            let untyped_pptr = self.settings.untyped_pptr_fields.iter().any(|entry| {
                entry.split_once('.') == Some((tt.m_Type.as_str(), field.m_Name.as_str()))
            });
            let field_ty = self.field_type(field, untyped_pptr)?;
            let (field_ty, comment) = split_trailing_comment(&field_ty);
            let field_name = self.escape_identifier(&field.m_Name);
            if field_name.as_ref() != field.m_Name.as_str()
                && self.settings.derives.is_some_and(|derives| {
                    derives.contains("serde::")
                        || derives.contains("Deserialize")
                        || derives.contains("Serialize")
                })
            {
                writeln!(&mut f, "    #[serde(rename = {:?})]", field.m_Name)?;
            }
            // a field missing from some game that has the parent struct becomes optional, so a
            // single struct deserializes every version; annotate which games actually have it
            let field_ty = if field.present_in.len() == tt.present_in.len() {
                Cow::Borrowed(field_ty)
            } else {
                let games: Vec<&str> = field
                    .present_in
                    .iter()
                    .map(|&i| self.labels[i].as_str())
                    .collect();
                writeln!(&mut f, "    // only in: {}", games.join(", "))?;
                writeln!(&mut f, "    #[serde(default)]")?;
                Cow::Owned(format!("Option<{field_ty}>"))
            };
            writeln!(&mut f, "    pub {}: {},{}", field_name, field_ty, comment,)?;
        }
        if let Some(additional_fields) = self.settings.additional_fields.get(tt.m_Type.as_str()) {
            for (field_name, field_ty) in *additional_fields {
                writeln!(
                    &mut f,
                    "    pub {}: {},",
                    self.escape_identifier(field_name),
                    field_ty,
                )?;
            }
        }
        writeln!(&mut f, "}}")?;
        Ok(f)
    }

    fn field_type(&mut self, field: &MergedTypeTree, untyped_pptr: bool) -> Result<String> {
        let field_ty = match self.classify(field) {
            Classify::Primitive(ty) => ty.to_owned(),
            Classify::PPtr(pptr) => {
                if untyped_pptr {
                    "PPtr".to_owned()
                } else if let Some(asm_ty) = pptr.strip_prefix('$') {
                    let key = (self.current_assembly.clone(), asm_ty.to_owned());
                    match self
                        .script_pptr_types
                        .get(&key)
                        .expect("script pointer dependencies were discovered")
                    {
                        Some(name) => format!("TypedPPtr<{name}>"),
                        None => format!("PPtr /* {asm_ty} */"),
                    }
                } else {
                    format!("TypedPPtr<{}>", pptr)
                }
            }
            Classify::Other(other) => self.escape_typename(other),
            Classify::Array(item) => {
                format!("Vec<{}>", self.field_type(item, untyped_pptr)?)
            }
            Classify::Map { key, value } => {
                format!(
                    "HashMap<{}, {}>",
                    self.field_type(key, untyped_pptr)?,
                    self.field_type(value, untyped_pptr)?
                )
            }
        };
        Ok(field_ty)
    }

    fn classify<'tt>(&self, tt: &'tt MergedTypeTree) -> Classify<'tt> {
        if let Some(rest) = tt.m_Type.strip_prefix("PPtr<")
            && let Some(pptr) = rest.strip_suffix('>')
        {
            return Classify::PPtr(pptr.to_owned());
        }
        match tt.m_Type.as_str() {
            "UInt8" => Classify::Primitive("u8"),
            "UInt16" | "unsigned short" => Classify::Primitive("u16"),
            "UInt32" | "unsigned int" | "Type*" => Classify::Primitive("u32"),
            "UInt64" | "unsigned long long" | "FileSize" => Classify::Primitive("u64"),
            "SInt8" => Classify::Primitive("i8"),
            "SInt16" | "short" => Classify::Primitive("i16"),
            "SInt32" | "int" => Classify::Primitive("i32"),
            "SInt64" | "long long" => Classify::Primitive("i64"),
            "float" => Classify::Primitive("f32"),
            "double" => Classify::Primitive("f64"),
            "char" => Classify::Primitive("char"),
            "string" => Classify::Primitive("String"),
            "bool" => Classify::Primitive("bool"),
            "TypelessData" => Classify::Primitive("Vec<u8>"),
            "map" => {
                let pair = &tt.children[0].children[1];
                let key = &pair.children[0];
                let value = &pair.children[1];
                Classify::Map { key, value }
            }
            _ if tt.children.len() == 1 && tt.children[0].m_Type == "Array" => {
                let item = &tt.children[0].children[1];
                Classify::Array(item)
            }
            _ => Classify::Other(tt),
        }
    }

    fn escape_typename(&self, tt: &MergedTypeTree) -> String {
        tt.m_Type.replace('`', "")
    }

    fn derives_for_type(&self, source_type_name: &str) -> String {
        let mut derives = self.settings.derives.unwrap_or_default().to_owned();
        if self.hash_key_types.contains(source_type_name) {
            for (derive, short_name) in [
                ("PartialEq", "PartialEq"),
                ("Eq", "Eq"),
                ("std::hash::Hash", "Hash"),
            ] {
                let already_present = derives.split(',').map(str::trim).any(|existing| {
                    existing == derive || existing.rsplit("::").next() == Some(short_name)
                });
                if !already_present {
                    if !derives.is_empty() {
                        derives.push_str(", ");
                    }
                    derives.push_str(derive);
                }
            }
        }
        derives
    }

    fn escape_identifier<'tt>(&self, identifier: &'tt str) -> Cow<'tt, str> {
        if ["type"].contains(&identifier) {
            Cow::Owned(format!("r#{identifier}"))
        } else if identifier.contains(|c| matches!(c, '[' | ' ')) {
            // Unity type trees can use array element names such as `data[0]`
            Cow::Owned(
                identifier
                    .replace(']', "")
                    .replace(|c| matches!(c, '[' | ' '), "_")
                    .replace("__", "_"),
            )
        } else {
            Cow::Borrowed(identifier)
        }
    }
}

/// Split a generated type string into its type and an optional trailing `/* ... */` comment,
/// so the field comma can be placed before the comment instead of after it.
fn split_trailing_comment(field_ty: &str) -> (&str, String) {
    if let Some(rest) = field_ty.strip_suffix("*/")
        && let Some(start) = rest.rfind("/*")
    {
        let ty = field_ty[..start].trim_end();
        let comment = &field_ty[start..];
        (ty, format!(" {comment}"))
    } else {
        (field_ty, String::new())
    }
}

#[derive(Debug)]
enum Classify<'a> {
    Primitive(&'static str),
    PPtr(String),
    Other(&'a MergedTypeTree),
    Array(&'a MergedTypeTree),
    Map {
        key: &'a MergedTypeTree,
        value: &'a MergedTypeTree,
    },
}
