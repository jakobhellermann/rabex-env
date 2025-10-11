mod utils;

use anyhow::Result;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let addressables = env.addressables()?.unwrap();

    dbg!(env.build_settings()?.scene_names().collect::<Vec<_>>());

    /*
    let item = addressables
        .cab_to_bundle
        .iter()
        // .find(|(key, path)| key.contains("9f1c78"))
        .find(|(key, path)| path.to_str().unwrap().contains("under_13"))
        .unwrap();

    let mut dependencies = Vec::new();

    dbg!(item);
    let file = env.load_addressables_bundle_content(&item.1)?;
    for path in file.file.externals_paths() {
        let archive = ArchivePath::try_parse(Path::new(path))?;
        match archive {
            Some(archive) => {
                let bundle = &addressables.cab_to_bundle[archive.bundle];
                let name = bundle.to_str().unwrap().replace("\\", "/");
                dbg!(archive.bundle, &name);
                dependencies.push(name);
            }
            None => {
                // dbg!(path);
            }
        }
    }

    for dep in dependencies {
        println!("\"{}\",", dep);
    }*/

    Ok(())
}
