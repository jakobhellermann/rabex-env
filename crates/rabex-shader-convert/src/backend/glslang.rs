//! GLSL to SPIR-V with glslang.

use anyhow::{Context, Result, bail};
use glslang::{
    Compiler, CompilerOptions, ShaderInput, ShaderSource, ShaderStage, SpirvVersion, Target,
    VulkanVersion,
};
use rabex_shader::Stage;

use crate::{Program, Translator};

pub struct Glslang {
    pub target: Target,
}

impl Glslang {
    /// SPIR-V 1.0 for Vulkan 1.0, like Unity's own Vulkan programs.
    pub fn vulkan() -> Self {
        Glslang {
            target: Target::Vulkan {
                version: VulkanVersion::Vulkan1_0,
                spirv_version: SpirvVersion::SPIRV1_0,
            },
        }
    }
}

fn shader_stage(stage: Stage) -> Result<ShaderStage> {
    Ok(match stage {
        Stage::Vertex => ShaderStage::Vertex,
        Stage::Fragment => ShaderStage::Fragment,
        Stage::Geometry => ShaderStage::Geometry,
        Stage::Hull => ShaderStage::TesselationControl,
        Stage::Domain => ShaderStage::TesselationEvaluation,
        Stage::Compute => ShaderStage::Compute,
        Stage::RayTracing => bail!("glslang can't compile ray tracing programs from GLSL"),
    })
}

impl Translator for Glslang {
    fn translate(&self, stage: Stage, program: Program) -> Result<Program> {
        let Program::Glsl(source) = program else {
            bail!("glslang compiles GLSL, not {program:?}");
        };
        let compiler = Compiler::acquire().context("glslang failed to initialize")?;
        let source = ShaderSource::from(source);
        let options = CompilerOptions {
            target: self.target.clone(),
            ..CompilerOptions::default()
        };
        let input = ShaderInput::new(
            &source,
            shader_stage(stage)?,
            &options,
            None::<&[(&str, Option<&str>)]>,
            None,
        )?;
        let shader = compiler.create_shader(input)?;
        Ok(Program::Spirv(shader.compile()?))
    }
}
