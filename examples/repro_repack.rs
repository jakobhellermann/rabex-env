mod utils;

use std::collections::BTreeSet;
use std::io::Cursor;

use anyhow::Result;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileBuilder, CompressionType};
use rabex::files::serializedfile::build_common_offset_map;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::objects::PPtr;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::unity::types::AssetBundle;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();

    let bundle = env.load_addressables_bundle("enemycorpses_assets_areacoral.bundle")?;

    let unity_version = env.unity_version()?;

    let com = build_common_offset_map(&env.tpk.inner, unity_version);

    let mut builder = BundleFileBuilder::unityfs(8, unity_version);
    for entry in bundle.files() {
        let data = bundle.read_at_entry(entry)?;

        let file = SerializedFile::from_reader(&mut Cursor::new(data.as_slice()))?;
        let file = SerializedFileHandle::new(&env, &file, &data);
        let mut file_builder = SerializedFileBuilder::from_serialized(
            unity_version,
            &file.file,
            &file.data,
            &env.tpk,
            &com,
            file.file.objects().cloned(),
        );

        let mut ab = file.find_object_of::<AssetBundle>()?.unwrap();

        let old_preloads = std::mem::take(&mut ab.m_PreloadTable);

        for (_name, container) in &mut ab.m_Container {
            println!("{_name}");
            let x = file.deref(container.asset.typed::<()>())?;
            let reachable = x.reachable()?;

            let (serialized_local, serialized_remote) = old_preloads[container.preload_range()]
                .iter()
                .partition::<Vec<PPtr>, _>(|x| x.is_local());
            let serialized_local = serialized_local
                .into_iter()
                .map(|x| x.m_PathID)
                .collect::<BTreeSet<_>>();
            let serialized_remote = serialized_remote.into_iter().collect::<BTreeSet<_>>();

            let only_reachable_local = reachable
                .0
                .difference(&serialized_local)
                .collect::<Vec<_>>();
            let only_serialized_local = serialized_local
                .difference(&reachable.0)
                .collect::<Vec<_>>();
            let only_reachable_external = reachable
                .1
                .difference(&serialized_remote)
                .collect::<Vec<_>>();
            let only_serialized_external = serialized_remote
                .difference(&reachable.1)
                .collect::<Vec<_>>();
            dbg!(only_reachable_local.len());
            dbg!(only_serialized_local.len());
            dbg!(only_reachable_external.len());
            dbg!(only_serialized_external.len());

            let preloads = reachable.0.into_iter().map(PPtr::local).chain(reachable.1);

            let range = {
                ab.m_PreloadTable.extend(preloads);
                let preload_index_end = ab.m_PreloadTable.len();
                ab.m_PreloadTable.len()..preload_index_end
            };
            container.preloadIndex = range.start as i32;
            container.preloadSize = range.len() as i32;
        }

        ab.m_Container.values_mut().for_each(|val| {
            val.preloadIndex = 0;
            val.preloadSize = ab.m_PreloadTable.len() as i32;
        });

        file_builder.serialized.m_ScriptTypes = Some(vec![]);
        file_builder.serialized.m_Types.iter_mut().for_each(|x| {
            x.m_ScriptTypeIndex = -1;
        });

        file_builder.objects.remove(&1);
        file_builder.add_object_at(1, &ab)?;

        let data = file_builder.write_vec()?;
        builder.add_file(&entry.path, data.as_slice())?;
    }
    builder.write_to_file("out.bundle", CompressionType::Lzma)?;

    Ok(())
}
