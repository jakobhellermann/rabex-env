//! Write `target`'s bundle with its shaders replaced by the D3D11 shaders of the same name from
//! `source`, converted to Vulkan: `replace_shaders <target bundle> <D3D11 bundle> <output>`.

use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_convert::bundle::rewrite_bundle;
use rabex_convert::objects::map_class;
use rabex_shader::Shader;
use rabex_shader_convert::target::vulkan::shader::convert;

fn shaders(bundle: &[u8]) -> Result<Vec<Shader>> {
    let reader = BundleFileReader::from_reader(Cursor::new(bundle), &ExtractionConfig::default())?;
    let entry = reader.main_serializedfile().context("no serialized file")?;
    let data = reader.read_at_entry(entry)?;
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
    let mut args = std::env::args().skip(1);
    let (Some(target), Some(source), Some(output)) = (args.next(), args.next(), args.next()) else {
        anyhow::bail!("usage: replace_shaders <target bundle> <D3D11 bundle> <output>");
    };
    let mut sources = shaders(&std::fs::read(&source).context(source)?)?;
    let common = rabex::files::serializedfile::CommonOffsetMap::new();
    let rewritten = rewrite_bundle(
        &std::fs::read(&target).context(target)?,
        &NullTypeTreeProvider,
        &common,
        &Default::default(),
        |_, rewriter| {
            map_class(rewriter, &mut |shader: Shader| {
                let name = &shader.m_ParsedForm.m_Name;
                let index = sources
                    .iter()
                    .position(|s| &s.m_ParsedForm.m_Name == name)
                    .context("not in the D3D11 bundle")?;
                let replacement = sources.swap_remove(index);
                ensure!(
                    replacement.m_Dependencies.is_empty()
                        && replacement.m_NonModifiableTextures.is_empty(),
                    "shader references other objects"
                );
                convert(&replacement, 8 << 20)?;
                Ok(replacement)
            })
        },
    )?
    .context("no shaders")?;
    ensure!(
        sources.is_empty(),
        "{} D3D11 shaders not in the target bundle",
        sources.len()
    );
    std::fs::write(&output, rewritten).context(output)?;
    Ok(())
}
