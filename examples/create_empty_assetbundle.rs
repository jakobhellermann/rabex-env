mod utils;

use std::fs::File;
use std::io::BufWriter;
use std::str::FromStr;

use anyhow::Result;
use rabex::UnityVersion;
use rabex::files::bundlefile::BundleFileBuilder;
use rabex::files::serializedfile::build_common_offset_map;
use rabex::files::serializedfile::builder::SerializedFileBuilder;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::unity::types::AssetBundle;

fn main() -> Result<()> {
    let env = utils::find_game("silksong")?.unwrap();
    let tpk = TypeTreeCache::embedded();
    let unity_version = env.unity_version()?;

    let filename = "something";

    let version = UnityVersion::from_str("6000.0.50f1-uum-100966-branch1")?;

    let mut bundle = BundleFileBuilder::unityfs(8, &version);

    let com = build_common_offset_map(&tpk.inner, unity_version);
    let mut file = SerializedFileBuilder::new(unity_version, &tpk, &com, false);
    // file.serialized.m_TargetPlatform = Some(0);

    file.next_path_id = 2;
    // let path_id = file.add_object(&Transform::default())?;

    let asset_bundle = AssetBundle::asset_base("assetbundle");
    /*asset_bundle.m_Container.insert(
        "test/foo.prefab".into(),
        AssetInfo::new(PPtr::local(path_id)),
    );*/
    file.add_object_at(1, &asset_bundle)?;

    bundle.add_file(filename, file.write_vec()?.as_slice())?;
    bundle.write(
        BufWriter::new(File::create("out.bundle")?),
        rabex::files::bundlefile::CompressionType::Lzma,
    )?;
    println!("Done");

    Ok(())
}
