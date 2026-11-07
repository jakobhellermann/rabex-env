//! Print the program fields of the fixtures' D3D11 stages next to Unity's Vulkan program of the
//! same pass variant: statistics, keywords, vertex channels and container words.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan;

fn shaders(path: &Path) -> Result<Vec<Shader>> {
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(path)?),
        &ExtractionConfig::default(),
    )?;
    let entry = bundle
        .main_serializedfile()
        .context("bundle without serialized file")?;
    let data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    file.objects()
        .filter(|o| o.m_ClassID == ClassId::Shader)
        .map(|info| {
            Ok(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(data.as_slice()),
            )?)
        })
        .collect()
}

fn main() -> Result<()> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rabex-shader/tests/fixtures/6000.3.27f2");
    let linux = shaders(&dir.join("shaders"))?;
    for shader in shaders(&dir.join("StandaloneWindows64/shaders"))? {
        let name = &shader.m_ParsedForm.m_Name;
        let unity = linux
            .iter()
            .find(|s| &s.m_ParsedForm.m_Name == name)
            .context("no Linux shader")?;
        let d3d = shader
            .platform_blobs()?
            .into_iter()
            .next()
            .context("no D3D11 blob")?;
        let vk = unity
            .platform_blobs()?
            .into_iter()
            .find(|b| b.platform == Platform::Vulkan)
            .context("no Vulkan")?;
        let passes = shader.m_ParsedForm.m_SubShaders[0].m_Passes.iter();
        for (p, (pass, unity_pass)) in passes
            .zip(&unity.m_ParsedForm.m_SubShaders[0].m_Passes)
            .enumerate()
        {
            let variants = shader.pass_variants(pass, &d3d)?;
            for unity_variant in unity.pass_variants(unity_pass, &vk)? {
                println!("{name} pass {p} {:?}", unity_variant.keywords);
                for stage in vulkan::select_stages(&unity_variant.keywords, &variants)? {
                    let program = &stage.program;
                    let (source_map, channels) = match &program.layout {
                        rabex_shader::SubProgramViewLayout::V202012090 {
                            source_map,
                            channels,
                            ..
                        } => (*source_map, channels.clone()),
                        _ => (0, Vec::new()),
                    };
                    println!(
                        "    d3d11 {:?}: type {} stats {:?} keywords {:?} source_map {source_map:#x} channels {channels:?}",
                        stage.stage, program.program_type, program.stats, program.keywords
                    );
                }
                let program = &unity_variant.program;
                let container = VulkanProgram::parse(program.code)?;
                let (source_map, channels) = match &program.layout {
                    rabex_shader::SubProgramViewLayout::V202012090 {
                        source_map,
                        channels,
                        ..
                    } => (*source_map, channels.clone()),
                    _ => (0, Vec::new()),
                };
                println!(
                    "    vulkan: type {} stats {:?} keywords {:?} source_map {source_map:#x} channels {channels:?} flags {:#x} unknown {:?}",
                    program.program_type,
                    program.stats,
                    program.keywords,
                    container.flags,
                    container.unknown
                );
            }
        }
    }
    Ok(())
}
