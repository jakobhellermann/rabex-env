//! Materials across the keyword rework of Unity 2021.2: the keyword list the old form
//! carries as one space separated string became a vector of valid and a vector of invalid
//! keywords, and integer properties moved into their own saved-properties map.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rabex::typetree::TypeTreeNode;
use rabex_shader::types::Material;

/// Downport to the pre-2021.2 form. The invalid keywords have no equivalent and drop
/// with a `dropped` entry when the material carries any; integer properties become the
/// float properties the old form has.
fn downport(mut material: Material, dropped: &mut BTreeMap<String, usize>) -> Result<Material> {
    let valid = material
        .m_ValidKeywords
        .take()
        .context("material without the 2021.2+ keyword vectors")?;
    if material
        .m_InvalidKeywords
        .as_ref()
        .is_some_and(|invalid| !invalid.is_empty())
    {
        *dropped
            .entry("Material.m_InvalidKeywords".to_owned())
            .or_insert(0) += 1;
    }
    material.m_InvalidKeywords = None;
    material.m_ShaderKeywords = Some(valid.join(" "));

    if let Some(ints) = material.m_SavedProperties.m_Ints.take() {
        for (name, value) in ints {
            material
                .m_SavedProperties
                .m_Floats
                .insert(name, value as f32);
        }
    }
    Ok(material)
}

/// The 2021.2+ form of one material. The invalid keywords start empty; float properties
/// stay floats — which of them the source declared as integers is not in the data.
fn upport(mut material: Material) -> Result<Material> {
    let keywords = material
        .m_ShaderKeywords
        .take()
        .context("material without the pre-2021.2 keyword string")?;
    material.m_ValidKeywords = Some(
        keywords
            .split(' ')
            .filter(|keyword| !keyword.is_empty())
            .map(|keyword| keyword.to_owned())
            .collect(),
    );
    material.m_InvalidKeywords = Some(Vec::new());
    Ok(material)
}

/// Convert one material to the form `target`'s tree describes. The form the material
/// carries is in its own fields — the keyword vectors of the rework of Unity 2021.2
/// against the space separated string before it — so no version decides the direction.
pub fn convert(
    material: Material,
    target: &TypeTreeNode,
    dropped: &mut BTreeMap<String, usize>,
) -> Result<Material> {
    let old_form = crate::value::has_field(target, "m_ShaderKeywords");
    match (material.m_ValidKeywords.is_some(), old_form) {
        // the vectors against the string
        (true, true) => downport(material, dropped),
        // the string against the vectors
        (false, false) => upport(material),
        // already the target's form
        _ => Ok(material),
    }
}
