//! Every binding a converted Vulkan entry announces must be one the SPIR-V decorates.
//!
//! The entry tells the player where to bind a texture, a sampler and a constant buffer;
//! the module says where it reads them. If the two disagree, the player binds nothing
//! to a descriptor the shader uses, and sampling it is not a wrong colour but a read
//! into memory the process does not own — a GPU page fault.
//!
//! The index of a binding packs the stage, the descriptor set and the binding:
//! `(stage << 24) | (set << 16) | binding`, read off Hollow Knight's own 2020.2 builds.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use rabex::UnityVersion;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig, FallbackUnityVersion};
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Binding, Platform, Shader, SubProgramView, SubProgramViewLayout};
use rabex_shader_convert::spirv::{ResourceKind, resources};

/// Silksong's shaders: every texture of them is a combined sampler, so every program
/// goes through the split. A machine without the game skips the test.
const SHADERS: &str = "/home/jakob/.steamapps/Hollow Knight Silksong/Hollow Knight Silksong_Data/StreamingAssets/aa/StandaloneLinux64/shaders_assets_all.bundle";

/// The stage a binding's index names, as the bit in its high byte.
fn stage_of(index: i32) -> Option<usize> {
    match (index as u32) >> 24 {
        0x04 => Some(rabex_shader::vulkan::SLOT_VERTEX),
        0x08 => Some(rabex_shader::vulkan::SLOT_FRAGMENT),
        _ => None,
    }
}

fn set_and_binding(index: i32) -> (u32, u32) {
    let index = index as u32;
    ((index >> 16) & 0xff, index & 0xffff)
}

#[test]
fn a_converted_entry_announces_the_bindings_its_module_decorates() -> Result<()> {
    let path = Path::new(SHADERS);
    if !path.exists() {
        return Ok(());
    }
    let source: UnityVersion = "6000.0.50f1".parse()?;
    let target: UnityVersion = "2020.2.2f1".parse()?;
    let config = ExtractionConfig {
        fallback_unity_version: FallbackUnityVersion::Exact(source.clone()),
        ..Default::default()
    };
    let reader = BundleFileReader::from_reader(Cursor::new(std::fs::read(path)?), &config)?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let tree = tpk
        .get_typetree_node(ClassId::Shader, &target)
        .context("no Shader tree for the target")?
        .into_owned();

    let (mut entries, mut bindings) = (0, 0);
    for entry in reader.serialized_files() {
        let data = reader.read_at_entry(entry)?;
        let file = SerializedFile::from_reader_with_version(&mut Cursor::new(&data), &source)?;
        for info in file.objects().filter(|o| o.m_ClassID == ClassId::Shader) {
            let shader: Shader = file.read(info, &tpk, &mut Cursor::new(data.as_slice()))?;
            let name = shader.m_ParsedForm.m_Name.clone();
            let converted = rabex_convert::shader::convert(&shader, &tree, 2 << 20)
                .with_context(|| name.clone())?;

            for blob in converted.platform_blobs()? {
                if blob.platform != Platform::Vulkan {
                    continue;
                }
                for location in blob.entries()? {
                    let view = SubProgramView::parse(blob.entry(location)?)?;
                    let SubProgramViewLayout::V201806140 { parameters, .. } = &view.layout else {
                        continue;
                    };
                    let program = VulkanProgram::parse(view.code)?;
                    // what each stage's module decorates, as (set, binding, is a sampler)
                    let mut declared: Vec<BTreeSet<(u32, u32, bool)>> = Vec::new();
                    for stage in &program.stages {
                        let mut slots = BTreeSet::new();
                        if let Some(stage) = stage {
                            for resource in resources(&smolv::decode(&stage.smolv)?)? {
                                slots.insert((
                                    resource.set,
                                    resource.binding,
                                    matches!(resource.kind, ResourceKind::Sampler),
                                ));
                            }
                        }
                        declared.push(slots);
                    }
                    entries += 1;

                    let mut check = |index: i32, sampler: bool, what: &str| -> Result<()> {
                        let Some(stage) = stage_of(index) else {
                            return Ok(());
                        };
                        let (set, binding) = set_and_binding(index);
                        bindings += 1;
                        anyhow::ensure!(
                            declared[stage].contains(&(set, binding, sampler)),
                            "{name}: {what} at set {set} binding {binding} of stage {stage} \
                             is not in the module, which has {:?}",
                            declared[stage]
                        );
                        Ok(())
                    };
                    for binding in &parameters.bindings {
                        match binding {
                            Binding::Texture {
                                name: texture,
                                index,
                                sampler_index,
                                ..
                            } => {
                                check(*index, false, &format!("texture {texture}"))?;
                                if *sampler_index >= 0 {
                                    check(
                                        *sampler_index,
                                        true,
                                        &format!("the sampler of {texture}"),
                                    )?;
                                }
                            }
                            Binding::ConstantBuffer { name, index, .. } => {
                                check(*index, false, &format!("constant buffer {name}"))?
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    assert!(entries > 0, "no converted Vulkan entry");
    println!("{entries} entries checked, {bindings} bindings against their modules");
    Ok(())
}
