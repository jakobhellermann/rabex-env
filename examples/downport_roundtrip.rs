//! Downport every shader of a game to the Unity 2020.2 form and roundtrip each rebuilt
//! program entry through the 2020.2 reader and writer.

mod utils;

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex_shader::{Shader, SubProgram};

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

    let (mut shaders, mut entries, mut variants) = (0, 0, 0);
    for file in files.values() {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Shader)
        {
            let shader: Shader = file.object_at(info.m_PathID)?.read()?;
            let name = shader.m_ParsedForm.m_Name.clone();
            let chunk = chunk_mb * 1024 * 1024;
            // the trees the rules read the target form from, straight from the package
            let tree = |version: &str| -> Result<_> {
                use rabex::typetree::TypeTreeProvider;
                let tpk = rabex::typetree::typetree_cache::sync::TypeTreeCache::new(
                    rabex::tpk::TpkTypeTreeBlob::embedded(),
                );
                Ok(tpk
                    .get_typetree_node(ClassId::Shader, &version.parse()?)
                    .context("the package carries the Shader tree")?
                    .into_owned())
            };
            let downported = rabex_convert::shader::convert(&shader, &tree("2020.2.2f1")?, chunk)
                .with_context(|| name.clone())?;
            for blob in downported.platform_blobs()? {
                for location in blob.entries()? {
                    let entry = blob.entry(location)?;
                    let rewritten = SubProgram::parse(entry)?.to_bytes()?;
                    anyhow::ensure!(
                        rewritten == entry,
                        "{name}: entry {location:?} differs after rewriting"
                    );
                    entries += 1;
                }
            }
            let upported =
                rabex_convert::shader::convert(&downported, &tree("6000.0.50f1")?, chunk)
                    .with_context(|| name.clone())?;

            // every variant must come back: platform, stage, code, parameters
            let variants_of = |shader: &Shader| -> Result<Vec<_>> {
                let mut all = Vec::new();
                for pass in shader
                    .m_ParsedForm
                    .m_SubShaders
                    .iter()
                    .flat_map(|s| &s.m_Passes)
                {
                    for blob in shader.platform_blobs()? {
                        for variant in shader.pass_variants(pass, &blob)? {
                            all.push((
                                blob.platform,
                                variant.stage,
                                variant.program.code.to_vec(),
                                format!("{:?}", variant.parameters),
                            ));
                        }
                    }
                }
                Ok(all)
            };
            let (mine, theirs) = (variants_of(&upported)?, variants_of(&shader)?);
            anyhow::ensure!(
                mine.len() == theirs.len(),
                "{name}: {} variants upported, {} in the source",
                mine.len(),
                theirs.len()
            );
            for (mine, theirs) in mine.iter().zip(&theirs) {
                anyhow::ensure!(mine.0 == theirs.0, "{name}: variant platform differs");
                anyhow::ensure!(mine.1 == theirs.1, "{name}: variant stage differs");
                anyhow::ensure!(mine.2 == theirs.2, "{name}: variant code differs");
            }
            variants += mine.len();
            shaders += 1;
        }
    }
    println!(
        "{shaders} shaders downported, {entries} entries roundtrip, \
         {variants} variants round trip through the upport"
    );
    Ok(())
}
