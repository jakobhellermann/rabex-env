mod utils;

use anyhow::Result;
use rayon::iter::ParallelBridge;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let aa = env.addressables()?.unwrap();

    let files = rabex_env::utils::par_fold_reduce::<usize, _>(
        aa.bundle_to_cab
            .iter()
            .filter(|(bundle, _)| bundle.display().to_string().contains("dataassets"))
            .par_bridge(),
        |acc, (bundle, files)| {
            let bundle = env.load_addressables_bundle(bundle)?;
            for file in files {
                if file.ends_with(".resS") || file.ends_with(".resource") {
                    continue;
                }
                let data = bundle.read_at(&file)?.unwrap();
                // let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
                *acc += data.len();
                // acc.push((data, file));
            }
            Ok(())
        },
    )?;
    dbg!(files as f32 / 1024. / 1024.);

    Ok(())
}
