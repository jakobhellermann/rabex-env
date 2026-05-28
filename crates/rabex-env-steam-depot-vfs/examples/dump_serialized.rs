use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use rabex::objects::ClassId;
use rabex::objects::pptr::PathId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeProvider;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::handle::SerializedFileHandle;
use rabex_env::resolver::EnvResolver;
use rabex_env::scene_lookup::SceneLookup;
use rabex_env::unity::types::Transform;
use rabex_env::{Environment, rabex};
use rabex_env_steam_depot_vfs::SteamDepotGameFiles;
use steam_depot_vfs::DepotStore;
use steam_depot_vfs::session::LazyCachedAuth;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let relative_path = std::env::args()
        .nth(1)
        .context("expected relative path to serialized file as first argument")?;

    let auth = LazyCachedAuth::prepare(
        LazyCachedAuth::default_refresh_token_cache(),
        std::env::var("STEAM_USERNAME").expect("missing STEAM_USERNAME"),
        std::env::var("STEAM_PASSWORD").expect("missing STEAM_PASSWORD"),
    )
    .await?;

    let store = "/home/jakob/.local/share/steam-multiversion-viewer/store";
    let store = DepotStore::new(store.into());

    let manifest_store = store
        .open_depot_manifest(Arc::new(auth), 367520, 367523, 708613018541602983, "public")
        .await?;

    let game_files = SteamDepotGameFiles::new(Arc::new(manifest_store))?;

    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new(game_files, &tpk);

    let file = env.load_serialized(&relative_path)?;
    print_class_stats(&file);
    println!();
    print_hierarchy(&file)?;

    Ok(())
}

fn print_class_stats<R: EnvResolver, P: TypeTreeProvider>(file: &SerializedFileHandle<'_, R, P>) {
    let mut counts: HashMap<ClassId, usize> = HashMap::new();
    for obj in file.file.objects() {
        *counts.entry(obj.m_ClassID).or_default() += 1;
    }

    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|(class_id, _)| *class_id);

    let total: usize = sorted.iter().map(|(_, n)| n).sum();
    println!("Class stats ({total} objects):");
    for (class_id, count) in sorted {
        println!("  {count:>6}  {class_id:?}");
    }
}

fn print_hierarchy<R: EnvResolver, P: TypeTreeProvider>(
    file: &SerializedFileHandle<'_, R, P>,
) -> Result<()> {
    let scene = SceneLookup::new(file.file, &mut file.reader(), &file.env.tpk)?;

    for (path_id, transform) in scene.roots() {
        print_node(file, path_id, transform, 0)?;
    }

    Ok(())
}

fn print_node<R: EnvResolver, P: TypeTreeProvider>(
    file: &SerializedFileHandle<'_, R, P>,
    transform_path_id: PathId,
    transform: &Transform,
    depth: usize,
) -> Result<()> {
    let go = transform
        .m_GameObject
        .deref_local(file.file, &file.env.tpk)?
        .read(&mut file.reader())?;

    let indent = "  ".repeat(depth);
    println!(
        "{indent}{} [{}]",
        go.m_Name, transform.m_GameObject.m_PathID
    );

    let child_indent = "  ".repeat(depth + 1);
    for component in &go.m_Component {
        let component_ref = component
            .component
            .deref_local::<()>(file.file, &file.env.tpk)?;
        let class_id = component_ref.info.m_ClassID;
        let path_id = component_ref.info.m_PathID;

        if path_id == transform_path_id
            && matches!(class_id, ClassId::Transform | ClassId::RectTransform)
        {
            continue;
        }

        println!("{child_indent}- {class_id:?} ({path_id})");
    }

    for child_pptr in &transform.m_Children {
        let child = child_pptr
            .deref_local(file.file, &file.env.tpk)?
            .read(&mut file.reader())?;
        print_node(file, child_pptr.m_PathID, &child, depth + 1)?;
    }

    Ok(())
}
