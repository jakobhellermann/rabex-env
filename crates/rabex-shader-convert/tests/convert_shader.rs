//! D3D11-only fixture shaders converted to Vulkan are laid out like Unity's own Linux builds of
//! the same shaders: same pass variants, requirements, keyword masks and stage counts.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use rabex_shader::types::SerializedPass;
use rabex_shader::{Platform, Shader};
use rabex_shader_convert::target::vulkan::shader::convert_inplace;

mod common;
use common::{fixture_dirs, shaders};

/// `(tier, keyword names, requirements)` of a pass's Vulkan programs.
fn vulkan_programs(
    shader: &Shader,
    pass: &SerializedPass,
) -> Result<BTreeSet<(usize, Vec<String>, i64)>> {
    let names = shader
        .m_ParsedForm
        .m_KeywordNames
        .as_deref()
        .context("no keyword names")?;
    let mut programs = BTreeSet::new();
    for (stage, program) in pass.programs() {
        let tiers = program.m_PlayerSubPrograms.as_deref().unwrap_or(&[]);
        for (tier, subs) in tiers.iter().enumerate() {
            for sub in subs.iter().filter(|s| s.m_GpuProgramType == 25) {
                ensure!(
                    stage == rabex_shader::Stage::Vertex,
                    "Vulkan program under {stage:?}"
                );
                let mut keywords = sub
                    .m_KeywordIndices
                    .iter()
                    .map(|&i| names.get(usize::from(i)).cloned().context("keyword index"))
                    .collect::<Result<Vec<_>>>()?;
                keywords.sort();
                programs.insert((tier, keywords, sub.m_ShaderRequirements));
            }
        }
    }
    Ok(programs)
}

#[test]
fn converted_shaders_match_unity_layout() -> Result<()> {
    let mut checked = 0;
    for dir in fixture_dirs()? {
        let version = dir.file_name().unwrap().to_string_lossy().into_owned();
        // Variant enumeration needs keyword names, which Unity 2020 shaders don't carry
        if rabex::UnityVersion::from_str(&version).unwrap() < rabex::UnityVersion::new(2022, 3, 0) {
            continue;
        }
        let linux = shaders(&dir.join("shaders"))?;
        for mut shader in shaders(&dir.join("StandaloneWindows64/shaders"))? {
            let name = shader.m_ParsedForm.m_Name.clone();
            let label = format!("{version} {name}");
            convert_inplace(&mut shader, 8 << 20).with_context(|| label.clone())?;
            let unity = linux
                .iter()
                .find(|s| s.m_ParsedForm.m_Name == name)
                .context("no Linux shader")?;
            let vulkan_index = unity
                .platforms
                .iter()
                .position(|&p| p == Platform::Vulkan.id())
                .context("no Vulkan in the Linux shader")?;
            assert_eq!(shader.platforms, vec![Platform::Vulkan.id()], "{label}");
            assert_eq!(
                shader.stageCounts,
                Some(vec![
                    unity.stageCounts.as_ref().context("no stage counts")?[vulkan_index]
                ]),
                "{label}"
            );

            let blobs = shader.platform_blobs()?;
            let passes = shader
                .m_ParsedForm
                .m_SubShaders
                .iter()
                .flat_map(|s| &s.m_Passes);
            let unity_passes = unity
                .m_ParsedForm
                .m_SubShaders
                .iter()
                .flat_map(|s| &s.m_Passes);
            for (p, (pass, unity_pass)) in passes.zip(unity_passes).enumerate() {
                let label = format!("{label} pass {p}");
                assert_eq!(
                    vulkan_programs(&shader, pass)?,
                    vulkan_programs(unity, unity_pass)?,
                    "{label}"
                );
                for ((stage, ours), (_, theirs)) in
                    pass.programs().into_iter().zip(unity_pass.programs())
                {
                    assert_eq!(
                        ours.m_SerializedKeywordStateMask, theirs.m_SerializedKeywordStateMask,
                        "{label} {stage:?}"
                    );
                }
                assert_eq!(pass.m_ProgramMask, unity_pass.m_ProgramMask, "{label}");
                assert_eq!(
                    pass.m_Platforms
                        .as_ref()
                        .map(|_| vec![Platform::Vulkan.id() as u8]),
                    pass.m_Platforms,
                    "{label}"
                );
                // the converted programs read back
                let variants = shader.pass_variants(pass, &blobs[0])?;
                ensure!(!variants.is_empty(), "{label}: no variants");
                checked += variants.len();
            }
        }
    }
    // the instancing shader adds its two variants
    assert_eq!(checked, 34);
    Ok(())
}
