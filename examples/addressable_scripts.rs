mod utils;

use std::io::Cursor;

use anyhow::Result;
use indexmap::IndexMap;
use rabex::files::SerializedFile;
use rabex_env::{handle::SerializedFileHandle, unity::types::MonoBehaviour};
use rustc_hash::FxHashMap;
use walkdir::WalkDir;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let mut results = IndexMap::new();

    let data_assets = env.addressables_build_folder()?.unwrap().join("");
    for item in WalkDir::new(&data_assets) {
        let item = item?;
        if item.file_type().is_dir() {
            continue;
        }
        let name = item
            .path()
            .strip_prefix(&data_assets)
            .unwrap()
            .to_str()
            .unwrap();
        if !name.contains("dataasset") {
            continue;
        }

        eprintln!("{}", name);
        let mut bundle = env.load_addressables_bundle(item.path())?;

        let mut counts = FxHashMap::<String, usize>::default();
        while let Some(mut file) = bundle.next() {
            if file.path.ends_with("resS") || file.path.ends_with("resource") {
                continue;
            }

            let data = file.read()?;
            let mut file = SerializedFile::from_reader(&mut Cursor::new(data))?;
            file.m_UnityVersion.get_or_insert(env.unity_version()?);
            let file = SerializedFileHandle::new(&env, &file, data);

            for mb in file.objects_of::<MonoBehaviour>()? {
                let Some(script) = mb.mono_script()? else {
                    continue;
                };
                let script = script.full_name();
                *counts.entry(script.into_owned()).or_default() += 1;
            }
        }

        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by(|a, b| a.1.cmp(&b.1).reverse());

        if sorted.len() > 0 {
            results.insert(
                name.to_owned(),
                sorted.into_iter().collect::<IndexMap<_, _>>(),
            );
        }
    }

    println!("{}", serde_json::to_string_pretty(&results)?);

    Ok(())
}
