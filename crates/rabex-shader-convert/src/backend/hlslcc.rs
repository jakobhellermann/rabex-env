//! DXBC to GLSL with Unity's HLSLcc.

use anyhow::{Result, bail, ensure};
use hlslcc::{Extensions, Language, Options, flags};
use rabex_shader::Stage;
use rabex_shader::dxbc::Container;

use crate::{Program, Translator};

pub struct Hlslcc {
    pub options: Options,
}

impl Hlslcc {
    /// GLSL for glslang to compile to Vulkan SPIR-V, with Vulkan `(set, binding)` layouts.
    pub fn vulkan() -> Self {
        Hlslcc {
            options: Options {
                flags: flags::VULKAN_BINDINGS
                    | flags::REMOVE_UNUSED_GLOBALS
                    | flags::UNIFORM_BUFFER_OBJECT
                    // TODO: Unity combines a texture with its own sampler; HLSLcc still emits separate ones
                    | flags::COMBINE_TEXTURE_SAMPLERS,
                language: Language::Glsl440,
                extensions: Extensions {
                    explicit_attrib_location: true,
                    shading_language_420pack: true,
                    ..Default::default()
                },
            },
        }
    }

    /// Translate one stage; translate all stages of a program with the same `dependencies`.
    pub fn translate_with_reflection(
        &self,
        dxbc: &[u8],
        dependencies: &mut hlslcc::Dependencies,
    ) -> Result<hlslcc::Translation> {
        let container = Container::parse(dxbc)?;
        if container.chunk(b"RDEF").is_none() {
            bail!(
                "HLSLcc needs an RDEF chunk, which Unity strips; rebuild it with rabex_shader::dxbc::rdef"
            );
        }
        // SAFETY: the container parses and carries an RDEF chunk. Whether that chunk describes
        // every accessed resource is up to the caller, who builds it from Unity's parameters.
        let mut translation = unsafe { hlslcc::translate(dxbc, &self.options, dependencies)? };
        translation.source =
            enable_required_extensions(&remove_dummy_sampler(&translation.source)?);
        Ok(translation)
    }
}

impl Translator for Hlslcc {
    fn translate(&self, _stage: Stage, program: Program) -> Result<Program> {
        match program {
            Program::Dxbc(dxbc) => Ok(Program::Glsl(
                self.translate_with_reflection(&dxbc, &mut hlslcc::Dependencies::new())?
                    .source,
            )),
            other => bail!("HLSLcc translates DXBC, not {other:?}"),
        }
    }
}

/// HLSLcc pairs textures read without a sampler, like by `Load`, with a sampler of its own;
/// read them sampler-less instead, like Unity's Vulkan programs, which bind no such sampler.
fn remove_dummy_sampler(source: &str) -> Result<String> {
    const DUMMY: &str = "hlslcc_dummyPointClamp";
    if !source.contains(DUMMY) {
        return Ok(source.to_owned());
    }
    let pairing = regex::Regex::new(r"\b[iu]?sampler\w*\((\w+), hlslcc_dummyPointClamp\)").unwrap();
    let source = pairing.replace_all(source, "$1");
    let mut lines: Vec<&str> = source
        .lines()
        .filter(|line| {
            !(line.contains(" uniform ") && line.trim_end().ends_with(&format!("sampler {DUMMY};")))
        })
        .collect();
    ensure!(
        !lines.iter().any(|line| line.contains(DUMMY)),
        "{DUMMY} used other than to pair a texture"
    );
    lines.insert(
        1,
        "#extension GL_EXT_samplerless_texture_functions : require",
    );
    Ok(lines.join("\n") + "\n")
}

/// HLSLcc emits `textureLod` on shadow samplers for `SampleCmpLevelZero` without enabling the
/// extension GLSL requires for it; SPIR-V needs no extension for the resulting instruction.
fn enable_required_extensions(source: &str) -> String {
    let uses_shadow_samplers = source.contains("Shadow ") || source.contains("Shadow;");
    if !uses_shadow_samplers {
        return source.to_owned();
    }
    let mut lines = source.splitn(2, '\n');
    let version = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();
    format!("{version}\n#extension GL_EXT_texture_shadow_lod : require\n{rest}")
}
