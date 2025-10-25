mod utils;

use std::time::Instant;

use anyhow::{Context, Result, bail};
use byteorder::LE;
use rabex::objects::ClassId;
use rabex::serde_typetree;
use rayon::iter::ParallelBridge as _;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let start = Instant::now();
    let result = rabex_env::utils::par_fold_reduce(
        env.addressables_bundles().par_bridge(),
        |acc: &mut (usize, usize), bundle| {
            let file = env.load_addressables_bundle_content(&bundle)?;
            for item in file.objects::<serde_value::Value>() {
                acc.0 += 1;

                if item.class_id() == ClassId::AssetBundle {
                    return Ok(());
                }

                (|| -> Result<()> {
                    let value = item.read().context("Failed to deserialize")?;

                    let tt = item.object.typetree()?;
                    let data = item.object_reader().into_inner();
                    let written = serde_typetree::to_vec::<_, LE>(&value, tt)
                        .inspect_err(|_| {
                            println!("Deserialized: {:#?}\nTypetree: {}", (), tt.dump_pretty())
                        })
                        .context("Failed to serialize")?;

                    if data != written {
                        acc.1 += 1;
                        bail!(
                            "Roundtrip failed. Deserialized: {:#?}\nTypetree: {}",
                            (),
                            tt.dump_pretty()
                        );
                    }

                    Ok(())
                })()
                .with_context(|| format!("At {:?} {}", item.class_id(), item.path_id()))
                .with_context(|| {
                    format!(
                        "At bundle {}",
                        bundle
                            // .strip_prefix(env.game_files.game_dir.join(aa.build_folder()))
                            // .unwrap()
                            .to_string_lossy()
                            .to_owned()
                    )
                })?;
            }
            Ok(())
        },
    )?;
    dbg!(result);
    dbg!(start.elapsed());

    Ok(())
}
