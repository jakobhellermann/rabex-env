#![allow(dead_code)]
mod utils;

use anyhow::{Context as _, Result};
use rabex::objects::ClassId;
use rabex::typetree::{TypeTreeNode, TypeTreeProvider};
use rabex_env::Environment;
use rustc_hash::FxHashSet;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write;

fn main() -> Result<()> {
    let mut env = utils::find_game("silksong")?.unwrap();
    env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let settings = Settings {
        derives: Some("Debug, serde::Deserialize"),
        field_ignores: &[
            "audio",
            "color",
            "colour",
            "font",
            "icon",
            "prefab",
            "sound",
            "sprite",
            "tint",
            // "ui",
            "vibration",
            "clipref",
            "effect",
        ],
        additional_fields: HashMap::from_iter([(
            "SavedItem",
            [("displayName", "Option<LocalisedString>")].as_slice(),
        )]),
    };
    let mut cx = Context::new(&env, settings);

    for script in [
        /*"CollectableItemRelicType",
        "EnemyJournalRecord",
        "IntReference",
        "DamageTag",
        "ShopItem",
        "Quest",
        "ToolItemBasic",*/
    ] {
        cx.generate_script("Assembly-CSharp", script)?;
    }
    cx.generate_script("PlayMaker", "PlayMakerFSM")?;
    // cx.generate_classid(ClassId::RectTransform)?;

    cx.finish(std::io::stdout().lock())?;

    Ok(())
}

struct Context<'a> {
    env: &'a Environment,
    settings: Settings<'a>,

    generated: FxHashSet<String>,
    generated_code: Vec<String>,
    queued: VecDeque<TypeTreeNode>,
}

struct Settings<'a> {
    derives: Option<&'a str>,
    field_ignores: &'a [&'a str],
    additional_fields: HashMap<&'a str, &'a [(&'a str, &'a str)]>,
}

impl<'a> Context<'a> {
    pub fn new(env: &'a Environment, settings: Settings<'a>) -> Self {
        Context {
            env,
            settings,
            generated: FxHashSet::default(),
            generated_code: Vec::new(),
            queued: VecDeque::new(),
        }
    }
    pub fn finish<W: Write>(&self, mut writer: W) -> Result<()> {
        writeln!(
            writer,
            "#![allow(dead_code, unused_imports, non_snake_case, nonstandard_style)]"
        )?;
        writeln!(writer, "use rabex_env::unity::types::*;")?;
        writeln!(
            writer,
            "use rabex_env::rabex::objects::{{PPtr, TypedPPtr}};"
        )?;
        writeln!(writer, "")?;
        for code in &self.generated_code {
            writeln!(writer, "{code}")?;
        }
        Ok(())
    }
    pub fn generate(&mut self, tt: &TypeTreeNode) -> Result<()> {
        self.queue(tt);
        self.handle_queue()
    }
    pub fn generate_classid(&mut self, class_id: ClassId) -> Result<()> {
        let tt = self
            .env
            .tpk
            .get_typetree_node(class_id, self.env.unity_version()?)
            .context("typetree not found")?;
        self.generate(&tt)
    }
    pub fn generate_script(&mut self, assembly: &str, script: &str) -> Result<()> {
        let tt = self.env.typetree_generator.generate(assembly, script)?;
        self.generate(tt)
    }

    fn queue(&mut self, tt: &TypeTreeNode) {
        if self.generated.contains(&tt.m_Type) {
            return;
        }
        assert!(self.generated.insert(tt.m_Type.clone()));

        self.queued.push_back(tt.clone());
    }

    fn handle_queue(&mut self) -> Result<()> {
        while let Some(item) = self.queued.pop_front() {
            let code = self.generate_inner(&item)?;
            self.generated_code.push(code);
        }

        Ok(())
    }

    fn ignore_field(&self, field: &TypeTreeNode) -> bool {
        self.settings
            .field_ignores
            .iter()
            .any(|ignore| field.m_Name.to_ascii_lowercase().contains(ignore))
    }

    fn generate_inner(&mut self, tt: &TypeTreeNode) -> Result<String> {
        // eprintln!("Generating {} {}", tt.m_Type, tt.m_Name);
        let mut f = String::new();
        if let Some(derives) = &self.settings.derives {
            writeln!(&mut f, "#[derive({})]", derives)?;
        }
        writeln!(&mut f, "pub struct {} {{", self.escape_typename(tt))?;
        for field in &tt.children {
            // eprintln!("Field {} {}", field.m_Type, field.m_Name);
            if self.ignore_field(field) {
                continue;
            }
            let field_ty = self.field_type(field)?;
            writeln!(
                &mut f,
                "    pub {}: {},",
                self.escape_identifier(&field.m_Name),
                field_ty,
            )?;
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

    fn field_type(&mut self, field: &TypeTreeNode) -> Result<String> {
        let field_ty = match self.classify(field) {
            Classify::Primitive(ty) => ty.to_owned(),
            Classify::PPtr(pptr) => {
                if let Some(asm_ty) = pptr.strip_prefix('$') {
                    let ty = self
                        .env
                        .typetree_generator
                        .generate("Assembly-CSharp", asm_ty)?;
                    if ty.m_Name == "Base" && ty.m_Type == "MonoBehaviour" {
                        format!("PPtr /* {asm_ty} */")
                    } else {
                        self.queue(ty);
                        format!("TypedPPtr<{}>", self.escape_typename(ty))
                    }
                } else {
                    format!("TypedPPtr<{}>", pptr)
                    // format!("PPtr /* {} */", pptr)
                }
            }
            Classify::Other(other) => {
                self.queue(field);
                self.escape_typename(other)
            }
            Classify::Array(item) => {
                format!("Vec<{}>", self.field_type(item)?)
            }
        };
        Ok(field_ty)
    }

    fn classify<'tt>(&self, tt: &'tt TypeTreeNode) -> Classify<'tt> {
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
            "map" => todo!(),
            _ if tt.children.len() == 1 && tt.children[0].m_Type == "Array" => {
                let item = &tt.children[0].children[1];
                Classify::Array(item)
            }
            _ => Classify::Other(tt),
        }
    }

    fn escape_typename(&self, tt: &TypeTreeNode) -> String {
        tt.m_Type.replace('`', "")
    }

    fn escape_identifier<'tt>(&self, identifier: &'tt str) -> Cow<'tt, str> {
        if ["type"].contains(&identifier) {
            Cow::Owned(format!("r#{identifier}"))
        } else {
            Cow::Borrowed(identifier)
        }
    }
}

#[derive(Debug)]
enum Classify<'a> {
    Primitive(&'static str),
    PPtr(String),
    Other(&'a TypeTreeNode),
    Array(&'a TypeTreeNode),
}
