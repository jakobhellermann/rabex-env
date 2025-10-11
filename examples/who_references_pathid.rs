mod utils;

use anyhow::Result;
use rabex::objects::{
    PPtr,
    pptr::{FileId, PathId},
};

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let file = env.load_addressables_bundle_content("enemycorpses_assets_areacoral.bundle")?;

    let external =
        "archive:/CAB-bf39d1dee67f853703ba8ebaa4288cf4/CAB-bf39d1dee67f853703ba8ebaa4288cf4";
    let path_id: PathId = -1483748356446411615;

    let file_id = file
        .file
        .externals_paths()
        .enumerate()
        .find_map(|(i, path)| (path == external).then_some(FileId::from_externals_index(i)))
        .unwrap();

    let pptr_to_find = PPtr::new(file_id, path_id);

    for obj in file.objects::<()>() {
        let pptrs = obj.reachable_one()?;
        if pptrs.contains(&pptr_to_find) {
            dbg!(obj.class_id());
            println!("Found!");
        }
    }

    Ok(())
}
