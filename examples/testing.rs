mod utils;

use anyhow::Result;
use rayon::iter::ParallelBridge;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let aa = env.addressables()?.unwrap();
    dbg!(&aa.cab_to_bundle);

    Ok(())
}
