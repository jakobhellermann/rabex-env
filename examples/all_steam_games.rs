use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use rabex_env::{
    EnvResolver, handle::SerializedFileHandle, unity::types::MonoScript, utils::par_fold_reduce,
};

mod utils;

fn main() -> Result<()> {
    utils::for_each_steam_game(|env| {
        let name = env.app_info()?.name;
        println!("-- {name} --");

        let scripts = par_fold_reduce::<BTreeMap<String, HashSet<String>>, _>(
            env.resolver.serialized_files()?,
            |scripts, path| {
                let path = path.to_str().unwrap();
                let (file, data) = env.load_leaf(&path)?;
                let file = SerializedFileHandle::new(&env, &file, data.as_ref());

                let entry = scripts.entry(path.to_owned()).or_default();
                for script in file.objects_of::<MonoScript>()? {
                    let script = script.read()?;
                    entry.insert(script.full_name().into_owned());
                }
                Ok(())
            },
        )?;

        for (file, scripts) in scripts {
            if scripts.len() > 0 {
                println!("- {}: {}", file, scripts.len());
            }
        }
        Ok(())
    })
}
