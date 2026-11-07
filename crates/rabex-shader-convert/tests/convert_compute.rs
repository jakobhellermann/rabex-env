//! D3D11 compute shaders of the fixtures converted to Vulkan match Unity's own Vulkan kernels of
//! the same shaders in everything but their SPIR-V.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use byteorder::LittleEndian;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::serde_typetree;
use rabex::typetree::{NullTypeTreeProvider, TypeTreeNode};
use rabex_shader::compute_types::ComputeShader;
use rabex_shader_convert::target::vulkan::compute::convert_inplace;

mod common;
use common::fixture_dirs;

/// The compute shaders of a bundle, and the type tree they're serialized with.
fn compute_shaders(path: &Path) -> Result<(Vec<ComputeShader>, TypeTreeNode)> {
    let data = std::fs::read(path).with_context(|| path.display().to_string())?;
    let bundle =
        BundleFileReader::from_reader(Cursor::new(data.as_slice()), &ExtractionConfig::default())?;
    let entry = bundle
        .main_serializedfile()
        .context("bundle without serialized file")?;
    let file_data = bundle.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(file_data.as_slice()))?;
    let infos: Vec<_> = file
        .objects()
        .filter(|o| o.m_ClassID == ClassId::ComputeShader)
        .collect();
    let tree = file
        .get_typetree_for(
            infos.first().context("no compute shaders")?,
            &NullTypeTreeProvider,
        )?
        .into_owned();
    let shaders = infos
        .into_iter()
        .map(|info| {
            Ok(file.read(
                info,
                NullTypeTreeProvider,
                &mut Cursor::new(file_data.as_slice()),
            )?)
        })
        .collect::<Result<_>>()?;
    Ok((shaders, tree))
}

/// Everything about a compute shader but its kernels' code.
fn described(shader: &mut ComputeShader) -> String {
    for variant in &mut shader.variants {
        for kernel in &mut variant.kernels {
            for unique in &mut kernel.uniqueVariants {
                unique.code.clear();
            }
        }
    }
    format!("{shader:#?}")
}

#[test]
fn converted_kernels_match_unity() -> Result<()> {
    let mut checked = 0;
    for dir in fixture_dirs()? {
        let version = dir.file_name().unwrap().to_string_lossy().into_owned();
        // the 2020.2 fixtures carry no compute shaders
        if !dir.join("compute").exists() {
            continue;
        }
        let (mut linux, tree) = compute_shaders(&dir.join("compute"))?;
        for mut shader in compute_shaders(&dir.join("StandaloneWindows64/compute"))?.0 {
            let label = format!("{version} {}", shader.m_Name);
            convert_inplace(&mut shader, &version.parse()?).with_context(|| label.clone())?;
            serde_typetree::to_vec::<_, LittleEndian>(&shader, &tree)
                .with_context(|| format!("{label}: serialize like Unity's Linux build"))?;
            for kernel in shader
                .variants
                .iter()
                .flat_map(|v| &v.kernels)
                .flat_map(|k| &k.uniqueVariants)
            {
                assert_eq!(
                    &kernel.code[..4],
                    &[0x03, 0x02, 0x23, 0x07],
                    "{label}: not SPIR-V"
                );
            }
            let index = linux
                .iter()
                .position(|s| s.m_Name == shader.m_Name)
                .context("no Linux compute shader")?;
            let unity = &mut linux[index];
            assert_eq!(described(&mut shader), described(unity), "{label}");
            checked += 1;
        }
    }
    assert_eq!(checked, 8);
    Ok(())
}
