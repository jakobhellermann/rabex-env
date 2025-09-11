mod utils;

use std::time::Instant;

use anyhow::Result;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let start = Instant::now();
    let catalog = env.addressables_bundle_locations()?.unwrap();

    dbg!(catalog);
    dbg!(start.elapsed());

    Ok(())
}
