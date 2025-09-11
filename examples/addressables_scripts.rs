mod utils;

use anyhow::Result;
use indexmap::IndexMap;
use rabex_env::unity::types::MonoBehaviour;
use rustc_hash::FxHashMap;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let mut results = IndexMap::new();

    let data_assets = env.addressables_build_folder()?.unwrap().join("");

    for bundle in env.addressables_bundles() {
        let name = bundle.strip_prefix(&data_assets).unwrap().to_str().unwrap();
        if !name.contains("") {
            continue;
        }

        let file = env.load_addressables_bundle_content(&bundle)?;

        let mut counts = FxHashMap::<String, usize>::default();
        for mb in file.objects_of::<MonoBehaviour>() {
            let Some(script) = mb.mono_script()? else {
                continue;
            };
            let script = script.full_name();
            *counts.entry(script.into_owned()).or_default() += 1;
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
