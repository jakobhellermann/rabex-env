//! Convert the D3D11 shaders and compute shaders of a bundle to Vulkan:
//! `convert_bundle <bundle> <output>`.

use anyhow::{Context, Result};
use rabex_convert::bundle::rewrite_bundle;
use rabex_convert::objects::map_class;
use rabex_convert::rewriter::SerializedFileRewriter;
use rabex_shader::Shader;
use rabex_shader::compute_types::ComputeShader;
use rabex_shader_convert::target::vulkan::{compute, shader};

fn convert(
    rewriter: &mut SerializedFileRewriter<'_, rabex::typetree::NullTypeTreeProvider>,
) -> Result<()> {
    let version = rewriter.unity_version().clone();
    map_class(rewriter, &mut |s: Shader| {
        shader::convert(&s, 8 << 20)?;
        Ok(s)
    })?;
    map_class(rewriter, &mut |s: ComputeShader| {
        compute::convert(&s, &version)?;
        Ok(s)
    })
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: convert_bundle <bundle> <output>");
    };
    let common = rabex::files::serializedfile::CommonOffsetMap::new();
    let converted = rewrite_bundle(
        &std::fs::read(&input).context(input)?,
        &rabex::typetree::NullTypeTreeProvider,
        &common,
        &Default::default(),
        |_, rewriter| convert(rewriter),
    )?
    .context("nothing to convert")?;
    std::fs::write(&output, converted).context(output)?;
    Ok(())
}
