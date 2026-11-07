//! Parse every program variant of a game's shaders, with its merged parameters, and summarize them.

mod utils;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex_shader::{Binding, Shader};

fn main() -> Result<()> {
    let game = std::env::args().nth(1).context("missing game")?;
    let env = utils::find_game(&game)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }

    let mut shaders = 0;
    let mut program_types = BTreeMap::<u32, usize>::new();
    let mut bindings = BTreeMap::<&str, usize>::new();
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            shaders += 1;
            let shader: Shader = file.object_at(info.m_PathID)?.read()?;
            let name = &shader.m_ParsedForm.m_Name;
            for blob in shader.platform_blobs()? {
                for pass in shader
                    .m_ParsedForm
                    .m_SubShaders
                    .iter()
                    .flat_map(|s| &s.m_Passes)
                {
                    let variants = shader.pass_variants(pass, &blob).with_context(|| {
                        format!("{name} {:?} pass {}", blob.platform, pass.m_Name)
                    })?;
                    for variant in variants {
                        if (15..=22).contains(&variant.program.program_type) {
                            variant.program.dxbc().with_context(|| name.clone())?;
                        }
                        for binding in &variant.parameters.bindings {
                            let kind = match binding {
                                Binding::Texture { .. } => "texture",
                                Binding::ConstantBuffer { .. } => "cbuffer",
                                Binding::Buffer { .. } => "buffer",
                                Binding::Uav { .. } => "uav",
                                Binding::Sampler { .. } => "sampler",
                            };
                            *bindings.entry(kind).or_default() += 1;
                        }
                        *program_types
                            .entry(variant.program.program_type)
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    println!("{shaders} shaders, program types: {program_types:?}, bindings: {bindings:?}");
    Ok(())
}
