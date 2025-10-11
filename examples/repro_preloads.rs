mod utils;

use std::io::Cursor;

use anyhow::Result;
use rabex::files::SerializedFile;
use rabex::objects::TypedPPtr;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::unity::types::{AssetBundle, GameObject};

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let bundle = env.load_addressables_bundle("enemycorpses_assets_areacoral.bundle")?;

    let _unity_version = env.unity_version()?;

    for entry in bundle.files() {
        let data = bundle.read_at_entry(entry)?;

        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        let file = SerializedFileHandle::new(&env, &file, &data);

        let ab = file.find_object_of::<AssetBundle>()?.unwrap();
        for (i, (name, info)) in ab.m_Container.iter().enumerate() {
            println!("--- {i} {name} ---");
            for (j, preload_pptr) in info.preload_range().map(|i| (i, ab.m_PreloadTable[i])) {
                let preload = file.deref(preload_pptr.typed::<InfoBase>())?;

                let path = preload_pptr
                    .as_external()
                    .map(|pptr| &file.file.get_external(pptr.m_FileID).unwrap().pathName)
                    .map(|x| format!(" @ {x}"))
                    .unwrap_or_default();

                let info = preload.read()?;

                /*info.m_GameObject
                .and_then(|go| file.deref_optional(go).transpose())
                .transpose()
                .context(format!("{:?}", info))?,*/
                println!(
                    "  {j} {:?} '{}'{path}",
                    preload.class_id(),
                    info.m_Name.unwrap_or_default()
                );
            }
        }
    }

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code, non_snake_case)]
struct InfoBase {
    m_Name: Option<String>,
    m_GameObject: Option<TypedPPtr<GameObject>>,
}
