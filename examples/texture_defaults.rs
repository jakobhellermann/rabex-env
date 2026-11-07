//! Print one Texture2D of a game to see the fields the old form carries.

mod utils;

use anyhow::{Context, Result, bail};
use rabex::objects::ClassId;
use rabex_shader::types::Texture2D;

fn main() -> Result<()> {
    let game = std::env::args().nth(1).context("game")?;
    let env = utils::find_game(&game)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }
    for (name, file) in &files {
        if let Some(info) = file
            .file
            .objects()
            .find(|o| o.m_ClassID == ClassId::Texture2D)
        {
            let texture: Texture2D = file.object_at(info.m_PathID)?.read()?;
            println!(
                "{name}: m_ForcedFallbackFormat {:?} m_DownscaleFallback {:?} m_IgnoreMasterTextureLimit {:?} m_IgnoreMipmapLimit {:?}",
                texture.m_ForcedFallbackFormat,
                texture.m_DownscaleFallback,
                texture.m_IgnoreMasterTextureLimit,
                texture.m_IgnoreMipmapLimit
            );
            return Ok(());
        }
    }
    bail!("no Texture2D found")
}
