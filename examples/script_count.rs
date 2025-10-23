#![feature(super_let)]
mod utils;

use std::cmp::Reverse;
use std::collections::BTreeMap;

use anyhow::Result;
use rabex_env::resolver::EnvResolver as _;
use rabex_env::unity::types::MonoBehaviour;
use rabex_env::utils::par_fold_reduce;

enum UnityFile<T> {
    Bundle(T),
    SerializedFile(T),
}

fn main() -> Result<()> {
    let game_filter = "";

    utils::for_each_steam_game(|env| {
        let name = env.app_info()?.name;
        if !name.to_ascii_lowercase().contains(game_filter) {
            return Ok(());
        }

        println!("-- {name} --");

        let mut files = env
            .resolver
            .serialized_files()?
            .into_iter()
            .map(UnityFile::SerializedFile)
            .collect::<Vec<_>>();
        files.extend(env.addressables_bundles().map(UnityFile::Bundle));

        let scripts = par_fold_reduce::<BTreeMap<String, usize>, _>(files, |scripts, path| {
            let file = match path {
                UnityFile::Bundle(bundle) => env.load_addressables_bundle_content(bundle)?,
                UnityFile::SerializedFile(path) => env.load_cached(&path)?,
            };
            for mb in file.objects_of::<MonoBehaviour>() {
                let Some(script) = mb.mono_script()? else {
                    continue;
                };
                *scripts.entry(script.full_name().into_owned()).or_default() += 1;
            }
            Ok(())
        })?;

        let mut scripts = scripts.into_iter().collect::<Vec<_>>();
        scripts.sort_by_key(|(_, count)| Reverse(*count));

        for (name, count) in scripts.iter().take(10) {
            println!("{} - {}", name, count);
        }
        println!();

        Ok(())
    })
}
