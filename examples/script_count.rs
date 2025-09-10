mod utils;

use std::{cmp::Reverse, collections::BTreeMap};

use anyhow::Result;
use rabex_env::{EnvResolver, unity::types::MonoBehaviour, utils::par_fold_reduce};

fn main() -> Result<()> {
    utils::for_each_steam_game(|env| {
        let name = env.app_info()?.name;
        println!("-- {name} --");

        let scripts = par_fold_reduce::<BTreeMap<String, usize>, _>(
            env.resolver.serialized_files()?,
            |scripts, path| {
                let file = env.load_cached(path)?;
                for mb in file.objects_of::<MonoBehaviour>()? {
                    let Some(script) = mb.mono_script()? else {
                        continue;
                    };
                    *scripts.entry(script.full_name().into_owned()).or_default() += 1;
                }
                Ok(())
            },
        )?;

        let mut scripts = scripts.into_iter().collect::<Vec<_>>();
        scripts.sort_by_key(|(_, count)| Reverse(*count));

        for (name, count) in scripts.iter().take(10) {
            println!("{} - {}", name, count);
        }
        println!();

        Ok(())
    })
}
