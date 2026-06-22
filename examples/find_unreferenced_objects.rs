mod utils;

use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use rabex::objects::ClassId;
use rabex::objects::pptr::PathId;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::resolver::EnvResolver;
use rabex_env::scene_lookup::SceneLookup;
use rabex_env::unity::types::{MeshFilter, Transform};
use rabex_env::utils::par_fold_reduce;
use rustc_hash::FxHashSet;

const SCENE_SINGLETON_OBJECTS: &[ClassId] = &[
    ClassId::RenderSettings,
    ClassId::LightmapSettings,
    ClassId::NavMeshSettings,
];

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let aa = env.addressables_build_folder()?.unwrap();
    let scenes = env
        .game_files
        .list_under(&aa.join("scenes_scenes_scenes"))?;
    par_fold_reduce::<(), _>(scenes, |_, scene| {
        let scene = scene.strip_prefix(&aa).unwrap();
        let file = env.load_addressables_bundle_content(scene)?;
        let lookup = SceneLookup::new(file.file, &mut file.reader(), &env.tpk)?;

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
            cx.visit(transform)?;
        }

        let mut unreachable = BTreeMap::<_, usize>::default();
        for obj in cx.file.objects::<()>() {
            if !cx.reachable.contains(&obj.path_id()) {
                if SCENE_SINGLETON_OBJECTS.contains(&obj.class_id()) {
                    continue;
                }
                println!(
                    "{} {:?} {}",
                    scene.file_name().unwrap().display(),
                    obj.class_id(),
                    obj.path_id()
                );

                *unreachable.entry(obj.class_id()).or_default() += 1;
            }
        }
        if !unreachable.is_empty() {
            println!("{}: {:#?}", scene.display(), unreachable);
        }

        Ok(())
    })?;

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

            if component.info.m_ClassID == ClassId::MeshFilter {
                let data = component
                    .cast::<MeshFilter>()
                    .read(&mut self.file.reader())?;
                if data.m_Mesh.is_local() {
                    self.reachable.insert(data.m_Mesh.m_PathID);
                }
            }
        }

        Ok(())
    }
}
