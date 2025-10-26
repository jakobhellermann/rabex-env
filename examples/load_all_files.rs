mod utils;

use anyhow::Result;
fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let files = env.load_all_serialized_files()?;
    dbg!(files.len());

    std::mem::forget(env);

    Ok(())
}
