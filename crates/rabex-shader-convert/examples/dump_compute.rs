//! Print the GLSL HLSLcc makes of a compute shader's D3D11 kernels:
//! `dump_compute <bundle> <compute shader name>`.

use std::io::Cursor;

use anyhow::{Context, Result};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::compute_types::ComputeShader;
use rabex_shader::dxbc::{Container, rdef};
use rabex_shader_convert::backend::Hlslcc;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(bundle), Some(name)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: dump_compute <bundle> <compute shader name>");
    };
    let data = std::fs::read(&bundle).context(bundle)?;
    let reader =
        BundleFileReader::from_reader(Cursor::new(data.as_slice()), &ExtractionConfig::default())?;
    for entry in reader.serialized_files() {
        let file_data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader(&mut Cursor::new(file_data.as_slice()))?;
        for info in file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::ComputeShader)
        {
            let shader: ComputeShader = file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(file_data.as_slice()),
            )?;
            if shader.m_Name != name {
                continue;
            }
            for variant in &shader.variants {
                for kernel in &variant.kernels {
                    for unique in &kernel.uniqueVariants {
                        let parameters =
                            rabex_shader::compute::parameters(&variant.constantBuffers, unique)?;
                        let mut container = Container::parse(&unique.code)?;
                        container.set_rdef(rdef(&parameters, &container)?);
                        let translation = Hlslcc::vulkan().translate_with_reflection(
                            &container.to_bytes()?,
                            &mut hlslcc::Dependencies::new(),
                        )?;
                        println!("==== {}\n{}", kernel.name, translation.source);
                    }
                }
            }
        }
    }
    Ok(())
}
