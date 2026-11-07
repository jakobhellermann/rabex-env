//! Every D3D11 program variant of the Unity-built fixtures translates to Vulkan SPIR-V.
//! The fixtures and their generator live in a separate Unity project (`ShaderFixtures.cs`).

use std::io::Cursor;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::objects::ClassId;
use rabex::typetree::NullTypeTreeProvider;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan;

fn windows_shaders(version_dir: &Path) -> Result<Vec<Shader>> {
    let path = version_dir.join("StandaloneWindows64/shaders");
    let bundle = BundleFileReader::from_reader(
        Cursor::new(std::fs::read(&path).with_context(|| path.display().to_string())?),
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

#[test]
fn d3d11_variants_translate_to_vulkan() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rabex-shader/tests/fixtures");
    let mut failures = Vec::new();
    let mut translated_stages = 0;
    for dir in std::fs::read_dir(&root)? {
        let dir = dir?.path();
        let version = dir.file_name().unwrap().to_string_lossy().into_owned();
        // Variant enumeration needs keyword names, which Unity 2020 shaders don't carry
        if rabex::UnityVersion::from_str(&version).unwrap() < rabex::UnityVersion::new(2022, 3, 0) {
            continue;
        }
        for shader in windows_shaders(&dir)? {
            let name = &shader.m_ParsedForm.m_Name;
            for blob in shader.platform_blobs()? {
                ensure!(
                    blob.platform == Platform::D3D11,
                    "{name}: {:?} in the Windows bundle",
                    blob.platform
                );
                for pass in shader
                    .m_ParsedForm
                    .m_SubShaders
                    .iter()
                    .flat_map(|s| &s.m_Passes)
                {
                    let variants = shader.pass_variants(pass, &blob)?;
                    let mut by_keywords: std::collections::BTreeMap<_, Vec<_>> = Default::default();
                    for variant in &variants {
                        by_keywords
                            .entry(variant.keywords.clone())
                            .or_default()
                            .push(variant);
                    }
                    for (keywords, stages) in by_keywords {
                        let label = format!("{version} {name} {} {keywords:?}", pass.m_Name);
                        match vulkan::from_d3d11(&stages) {
                            Ok(translated) => {
                                for stage in translated {
                                    if stage.spirv.first() == Some(&spirv::MAGIC_NUMBER) {
                                        translated_stages += 1;
                                    } else {
                                        failures.push(format!(
                                            "{label} {:?}: output isn't SPIR-V",
                                            stage.stage
                                        ));
                                    }
                                }
                            }
                            Err(e) => failures.push(format!("{label}: {e:#}")),
                        }
                    }
                }
            }
        }
    }
    ensure!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    ensure!(
        translated_stages == 62,
        "translated {translated_stages} stages, the fixtures have 31 per Unity version"
    );
    Ok(())
}
