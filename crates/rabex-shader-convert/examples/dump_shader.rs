//! Print the D3D11 parameters and the translated GLSL of a shader's pass variants:
//! `dump_shader <bundle> <shader name> [keyword]`.

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan;
use rabex_shader_convert::target::vulkan::shader::pass_variants;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(bundle), Some(name)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: dump_shader <bundle> <shader name> [keyword]");
    };
    let keyword = args.next();
    let data = std::fs::read(&bundle).context(bundle)?;
    let reader =
        BundleFileReader::from_reader(Cursor::new(data.as_slice()), &ExtractionConfig::default())?;
    for entry in reader.serialized_files() {
        let file_data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(file_data.as_slice()))?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(file_data.as_slice()),
            )?;
            if shader.m_ParsedForm.m_Name != name {
                continue;
            }
            let blob = shader
                .platform_blobs()?
                .into_iter()
                .find(|b| b.platform == Platform::D3D11)
                .context("no D3D11")?;
            for (p, pass) in shader
                .m_ParsedForm
                .m_SubShaders
                .iter()
                .flat_map(|s| &s.m_Passes)
                .enumerate()
            {
                let variants = shader.pass_variants(pass, &blob)?;
                for variant in pass_variants(&variants)? {
                    if keyword
                        .as_ref()
                        .is_some_and(|k| !variant.keywords.contains(k))
                    {
                        continue;
                    }
                    println!("==== pass {p} {:?}", variant.keywords);
                    for stage in &variant.stages {
                        println!("-- {:?} D3D11 constant buffers:", stage.stage);
                        for cb in &stage.parameters.constant_buffers {
                            println!("   {} size {}", cb.name, cb.size);
                            for c in &cb.constants {
                                println!(
                                    "     {} @{} {:?}",
                                    c.name,
                                    c.offset,
                                    (c.data_type, c.rows, c.columns, c.array_size)
                                );
                            }
                        }
                    }
                    for stage in vulkan::from_d3d11(&variant.stages)? {
                        println!("-- {:?} GLSL:\n{}", stage.stage, stage.glsl);
                    }
                }
            }
        }
    }
    Ok(())
}
