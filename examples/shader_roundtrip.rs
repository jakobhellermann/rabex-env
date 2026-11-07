//! Rewrite every program, parameter blob and platform blob of a game's shaders and compare with the original.
//! The chunk size is the game's `m_DefaultShaderChunkSizeInMB`, which isn't stored in the build.

mod utils;

use anyhow::{Context, Result, bail, ensure};
use rabex::objects::ClassId;
use rabex_shader::{EntryKind, PlatformBlob, ProgramParameters, Shader, SubProgram};

fn main() -> Result<()> {
    let game = std::env::args().nth(1).context("missing game")?;
    let chunk_mb: usize = std::env::args()
        .nth(2)
        .context("missing chunk size in MB")?
        .parse()?;
    let env = utils::find_game(&game)?.context("game not found")?;
    let mut files = env.load_all_serialized_files()?;
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }

    let (mut shaders, mut entries_checked, mut multi_segment) = (0, 0, 0);
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            let mut shader: Shader = file.object_at(info.m_PathID)?.read()?;
            let name = shader.m_ParsedForm.m_Name.clone();
            let blobs = shader.platform_blobs()?;
            for (i, blob) in blobs.iter().enumerate() {
                let entries = blob.entry_data()?;
                let kinds = shader.entry_kinds(blob.platform, entries.len())?;
                for (index, (data, kind)) in entries.iter().zip(&kinds).enumerate() {
                    let rewritten = match kind {
                        Some(EntryKind::Program) => SubProgram::parse(data)?.to_bytes()?,
                        Some(EntryKind::Parameters) => ProgramParameters::parse(data)?.to_blob()?,
                        None => bail!("{name} {:?}: entry {index} is unreferenced", blob.platform),
                    };
                    ensure!(
                        rewritten == *data,
                        "{name} {:?}: entry {index} ({kind:?}) differs after rewriting",
                        blob.platform
                    );
                    entries_checked += 1;
                }
                let rebuilt = PlatformBlob::from_entries(blob.platform, &entries, chunk_mb << 20)?;
                ensure!(
                    rebuilt.segments == blob.segments,
                    "{name} {:?}: segments differ after rebuilding",
                    blob.platform
                );
                if blob.segments.len() > 1 {
                    multi_segment += 1;
                }
                // a stripped platform has an empty blob but keeps its stale stage count
                if let (false, Some(stage_counts)) =
                    (entries.is_empty(), shader.stageCounts.as_ref())
                {
                    ensure!(
                        shader.stage_count(blob.platform)? == stage_counts[i],
                        "{name}: stage count differs"
                    );
                }
            }
            shader.set_platform_blobs(&blobs)?;
            let reread: Vec<_> = shader
                .platform_blobs()?
                .into_iter()
                .map(|b| b.segments)
                .collect();
            let original: Vec<_> = blobs.into_iter().map(|b| b.segments).collect();
            ensure!(
                reread == original,
                "{name}: blobs differ after recompressing"
            );
            shaders += 1;
        }
    }
    println!(
        "{shaders} shaders, {entries_checked} entries, {multi_segment} multi-segment blobs roundtrip"
    );
    Ok(())
}
