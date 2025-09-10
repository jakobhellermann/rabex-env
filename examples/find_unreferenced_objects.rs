mod utils;

use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use rabex::objects::pptr::PathId;
use rabex_env::{
    handle::SerializedFileHandle, scene_lookup::SceneLookup, unity::types::Transform,
    utils::par_fold_reduce,
};
use rayon::iter::ParallelBridge;
use rustc_hash::FxHashSet;

fn main() -> Result<()> {
    let mut env = utils::find_game("silksong")?.unwrap();
    env.load_typetree_generator(typetree_generator_api::GeneratorBackend::AssetsTools)?;

    let aa = env.addressables_build_folder()?.unwrap();
    par_fold_reduce::<(), _>(
        std::fs::read_dir(aa.join("scenes_scenes_scenes"))?.par_bridge(),
        |_, scene| {
            let scene = scene?;

            let file = env.load_addressables_bundle_content(scene.path())?;
            let file = SerializedFileHandle::new(&env, &file.0, file.1.as_slice());
            let lookup = SceneLookup::new(&file.file, &mut file.reader(), &env.tpk)?;

            let mut cx = Cx {
                lookup,
                file,
                reachable: HashSet::default(),
            };
            let roots: Vec<_> = cx
                .lookup
                .roots()
                .map(|(path_id, transform)| (path_id, transform.clone()))
                .collect();
            for &(path_id, ref transform) in &roots {
                cx.reachable.insert(path_id);
                cx.visit(&transform)?;
            }

            let mut unreachable = BTreeMap::<_, usize>::default();
            for obj in cx.file.objects::<()>() {
                let obj = obj?;
                if !cx.reachable.contains(&obj.path_id()) {
                    *unreachable.entry(obj.class_id()).or_default() += 1;
                }
            }
            println!("{}: {:#?}", scene.file_name().display(), unreachable);

            Ok(())
        },
    )?;

    Ok(())
}

struct Cx<'a, P> {
    file: SerializedFileHandle<'a>,
    lookup: SceneLookup<'a, P>,

    reachable: FxHashSet<PathId>,
}
impl<'a, P> Cx<'a, P> {
    fn visit(&mut self, transform: &Transform) -> Result<()> {
        for &child in &transform.m_Children {
            let child = self.file.deref(child)?;
            self.reachable.insert(child.path_id());

            let child = child.read()?;
            self.visit(&child)?;
        }

        let go = self.file.deref(transform.m_GameObject)?;
        self.reachable.insert(go.path_id());
        for component in go.read()?.components(self.file.file, &self.file.env.tpk) {
            let component = component?;
            self.reachable.insert(component.info.m_PathID);
        }

        Ok(())
    }
}
