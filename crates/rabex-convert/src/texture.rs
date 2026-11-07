//! Textures across the field changes around the mipmap limit: the master texture
//! limit flag the old form carries renamed in Unity 2022.2, and the fallback fields
//! the forms above their removal want no longer.

use anyhow::{Context, Result};
use rabex::typetree::TypeTreeNode;
use rabex_shader::types::Texture2D;

/// Convert one texture to the form `target`'s tree describes. Which limit flag the
/// texture carries is in its own fields — the mipmap limit of the rename of Unity
/// 2022.2 against the master texture limit before it — so no version decides the
/// direction. The fallback format and downscale flag default to what the old builds
/// write; which textures overrode them is not in the data of the new forms.
pub fn convert(mut texture: Texture2D, target: &TypeTreeNode) -> Result<Texture2D> {
    let old_form = crate::value::has_field(target, "m_IgnoreMasterTextureLimit");
    match (texture.m_IgnoreMipmapLimit.is_some(), old_form) {
        // downwards: the flag arrives under its old name, the fallback fields fill
        // with the old builds' defaults, the limit group name has no equivalent
        (true, true) => {
            if texture.m_ForcedFallbackFormat.is_none() {
                texture.m_ForcedFallbackFormat = Some(4);
            }
            if texture.m_DownscaleFallback.is_none() {
                texture.m_DownscaleFallback = Some(false);
            }
            if texture.m_IgnoreMasterTextureLimit.is_none() {
                texture.m_IgnoreMasterTextureLimit =
                    Some(texture.m_IgnoreMipmapLimit.take().unwrap_or(false));
            }
            texture.m_MipmapLimitGroupName = None;
        }
        // upwards: the flag renames back
        (false, false) => {
            if texture.m_IgnoreMipmapLimit.is_none() {
                texture.m_IgnoreMipmapLimit = Some(
                    texture
                        .m_IgnoreMasterTextureLimit
                        .take()
                        .context("texture without either mipmap limit flag")?,
                );
            }
            texture.m_IgnoreMasterTextureLimit = None;
        }
        // already the target's form
        _ => {}
    }
    Ok(texture)
}
