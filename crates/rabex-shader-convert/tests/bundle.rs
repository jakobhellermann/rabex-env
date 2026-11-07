//! Rewriting the shaders of the fixture bundles.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_convert::objects::map_class;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan::shader::convert_inplace;

mod common;
use common::{fixture_dirs, shaders};

/// The raw data of every object of a bundle's main serialized file, by path id.
fn objects(bundle: &[u8]) -> Result<BTreeMap<i64, (ClassId, Vec<u8>)>> {
    let reader = BundleFileReader::from_reader(Cursor::new(bundle), &ExtractionConfig::default())?;
    let entry = reader.main_serializedfile().context("no serialized file")?;
    let data = reader.read_at_entry(entry)?;
    let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
    Ok(file
        .objects()
        .map(|o| {
            let range = o.m_Offset as usize..o.m_Offset as usize + o.m_Size as usize;
            (o.m_PathID, (o.m_ClassID, data[range].to_vec()))
        })
        .collect())
}

#[test]
fn unchanged_shaders_roundtrip() -> Result<()> {
    for dir in fixture_dirs()? {
        for bundle in ["shaders", "StandaloneWindows64/shaders"] {
            let original = std::fs::read(dir.join(bundle))?;
            let rewritten = rabex_convert::bundle::rewrite_bundle(
                &original,
                &NullTypeTreeProvider,
                &rabex::files::serializedfile::CommonOffsetMap::new(),
                &Default::default(),
                |_, rewriter| map_class(rewriter, &mut |shader: Shader| Ok(shader)),
            )?
            .context("no shaders")?;
            let (rewritten, original) = (objects(&rewritten)?, objects(&original)?);
            assert_eq!(
                rewritten.keys().collect::<Vec<_>>(),
                original.keys().collect::<Vec<_>>()
            );
            for (path_id, (class, data)) in &original {
                let (rewritten_class, rewritten_data) = &rewritten[path_id];
                let first_difference = data.iter().zip(rewritten_data).position(|(a, b)| a != b);
                if let Some(at) = first_difference {
                    eprintln!("original  {:?}", &data[at.saturating_sub(16)..at + 32]);
                    eprintln!(
                        "rewritten {:?}",
                        &rewritten_data[at.saturating_sub(16)..at + 32]
                    );
                }
                assert!(
                    class == rewritten_class && data == rewritten_data,
                    "{} {bundle} {class:?} {path_id}: {} bytes, rewritten {}, first difference at {first_difference:?}",
                    dir.display(),
                    data.len(),
                    rewritten_data.len(),
                );
            }
        }
    }
    Ok(())
}

/// Unity's Linux bundle with its shaders replaced by the Windows shaders converted to Vulkan.
#[test]
fn converted_shaders_replace_linux_shaders() -> Result<()> {
    for dir in fixture_dirs()? {
        // Variant enumeration needs keyword names, which Unity 2020 shaders don't carry
        let version = dir.file_name().unwrap().to_string_lossy().into_owned();
        if rabex::UnityVersion::from_str(&version).unwrap() < rabex::UnityVersion::new(2022, 3, 0) {
            continue;
        }
        let mut windows = shaders(&dir.join("StandaloneWindows64/shaders"))?;
        let original = std::fs::read(dir.join("shaders"))?;
        let rewritten = rabex_convert::bundle::rewrite_bundle(
            &original,
            &NullTypeTreeProvider,
            &rabex::files::serializedfile::CommonOffsetMap::new(),
            &Default::default(),
            |_, rewriter| {
                map_class(rewriter, &mut |linux: Shader| {
                    let index = windows
                        .iter()
                        .position(|s| s.m_ParsedForm.m_Name == linux.m_ParsedForm.m_Name)
                        .context("no Windows shader")?;
                    let mut shader = windows.swap_remove(index);
                    ensure!(
                        shader.m_Dependencies.is_empty()
                            && shader.m_NonModifiableTextures.is_empty(),
                        "shader references other objects"
                    );
                    convert_inplace(&mut shader, 8 << 20)?;
                    Ok(shader)
                })
            },
        )?
        .context("no shaders")?;
        assert!(windows.is_empty(), "{} Windows shaders left", windows.len());

        let path = std::env::temp_dir().join(format!(
            "rabex-shader-convert-{}",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&path, &rewritten)?;
        for shader in shaders(&path)? {
            assert_eq!(shader.platforms, vec![Platform::Vulkan.id()]);
            let blob = &shader.platform_blobs()?[0];
            for pass in shader
                .m_ParsedForm
                .m_SubShaders
                .iter()
                .flat_map(|s| &s.m_Passes)
            {
                ensure!(!shader.pass_variants(pass, blob)?.is_empty());
            }
        }
        std::fs::remove_file(&path)?;
    }
    Ok(())
}
