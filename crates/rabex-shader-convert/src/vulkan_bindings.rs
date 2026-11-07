//! Every binding a Vulkan program entry announces has to be one its SPIR-V decorates.
//!
//! The entry tells the player where to bind a texture, a sampler and a constant buffer; the
//! module says where it reads them. If the two disagree, the player binds nothing to a
//! descriptor the shader reads, and that read is not a wrong colour but an access to
//! memory the process does not own: a GPU page fault, which can take the desktop with it.
//! Cheap enough to check every converted program.
//!
//! The index of a binding packs the stage, the descriptor set and the binding:
//! `(stage << 24) | (set << 16) | binding`, read off Hollow Knight's own 2020.2 builds.

use std::collections::BTreeSet;

use crate::spirv::{ResourceKind, resources};
use anyhow::{Context, Result, ensure};
use rabex_shader::vulkan::VulkanProgram;
use rabex_shader::{Binding, Platform, Shader, SubProgramView, SubProgramViewLayout};

/// The stage slot a binding's index names.
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

/// What one program's modules decorate, per stage slot, as (set, binding, is a sampler).
fn declared(code: &[u8]) -> Result<Vec<BTreeSet<(u32, u32, bool)>>> {
    let program = VulkanProgram::parse(code)?;
    let mut out = Vec::new();
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
        out.push(slots);
    }
    Ok(out)
}

fn check_program(name: &str, code: &[u8], bindings: &[Binding]) -> Result<usize> {
    let declared = declared(code)?;

    let mut checked = 0;
    let mut check = |index: i32, sampler: bool, what: &str| -> Result<()> {
        let Some(stage) = stage_of(index) else {
            return Ok(());
        };
        let (set, binding) = set_and_binding(index);
        checked += 1;
        let slots = declared
            .get(stage)
            .with_context(|| format!("{name}: no stage {stage}"))?;
        ensure!(
            slots.contains(&(set, binding, sampler)),
            "{name}: {what} at set {set} binding {binding} of stage {stage} is not in the module, \
             which has {slots:?}"
        );
        Ok(())
    };
    // and the other way: whatever a module reads has to be announced, or the player's
    // pipeline layout lacks it and the read goes nowhere
    let mut announced = BTreeSet::new();
    for binding in bindings {
        let indices: &[i32] = match binding {
            Binding::Texture {
                index,
                sampler_index,
                ..
            } => &[*index, *sampler_index],
            Binding::ConstantBuffer { index, .. }
            | Binding::Buffer { index, .. }
            | Binding::Uav { index, .. }
            | Binding::Sampler { index, .. } => std::slice::from_ref(index),
        };
        announced.extend(
            indices
                .iter()
                .filter(|i| **i >= 0)
                .map(|i| set_and_binding(*i)),
        );
    }
    for (stage, slots) in declared.iter().enumerate() {
        for &(set, binding, _) in slots {
            ensure!(
                announced.contains(&(set, binding)),
                "{name}: the module of stage {stage} reads set {set} binding {binding}, which the \
                 entry does not announce (it announces {announced:?})"
            );
        }
    }
    for binding in bindings {
        match binding {
            Binding::Texture {
                name: texture,
                index,
                sampler_index,
                ..
            } => {
                check(*index, false, &format!("texture {texture}"))?;
                if *sampler_index >= 0 {
                    check(*sampler_index, true, &format!("the sampler of {texture}"))?;
                }
            }
            Binding::ConstantBuffer { name, index, .. } => {
                check(*index, false, &format!("constant buffer {name}"))?
            }
            _ => {}
        }
    }
    Ok(checked)
}

/// Check every Vulkan program of `shader`, in either serialized form; returns the number
/// of programs and bindings checked.
pub fn verify(shader: &Shader) -> Result<(usize, usize)> {
    let name = &shader.m_ParsedForm.m_Name;
    let (mut programs, mut bindings) = (0, 0);
    for blob in shader.platform_blobs()? {
        if blob.platform != Platform::Vulkan {
            continue;
        }
        // the 2020.2 form: the parameters inline after each program
        let mut inline = false;
        for location in blob.entries()? {
            let Ok(view) = SubProgramView::parse(blob.entry(location)?) else {
                continue;
            };
            if let SubProgramViewLayout::V201806140 { parameters, .. } = &view.layout {
                inline = true;
                bindings += check_program(name, view.code, &parameters.bindings)?;
                programs += 1;
            }
        }
        if inline {
            continue;
        }
        // the 2022+ form: parameters in their own blob entries, merged per variant
        for pass in shader
            .m_ParsedForm
            .m_SubShaders
            .iter()
            .flat_map(|s| &s.m_Passes)
        {
            if pass.progVertex.m_PlayerSubPrograms.is_none() {
                continue;
            }
            for variant in shader.pass_variants(pass, &blob)? {
                bindings +=
                    check_program(name, variant.program.code, &variant.parameters.bindings)?;
                programs += 1;
            }
        }
    }
    Ok((programs, bindings))
}
