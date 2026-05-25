use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::{Environment, rabex};
use rabex_env_steam_depot_vfs::SteamDepotGameFiles;
use steam_depot_vfs::DepotStore;
use steam_depot_vfs::session::LazyCachedAuth;

async fn steam_game_files(
    app_id: u32,
    depot_id: u32,
    manifest_id: u64,
    branch: &str,
) -> Result<SteamDepotGameFiles> {
    let auth = LazyCachedAuth::prepare(
        LazyCachedAuth::default_refresh_token_cache(),
        std::env::var("STEAM_USERNAME").expect("missing STEAM_USERNAME"),
        std::env::var("STEAM_PASSWORD").expect("missing STEAM_PASSWORD"),
    )
    .await?;

    let store = "/tmp/steam-vfs-store";
    let store = DepotStore::new(store.into());

    let manifest_store = store
        .open_depot_manifest(Arc::new(auth), app_id, depot_id, manifest_id, branch)
        .await?;

    let game_files = SteamDepotGameFiles::new(Arc::new(manifest_store))?;
    Ok(game_files)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let app_id = 367520;
    let depot_id = 367523;
    let manifest_id = 708613018541602983;
    let game_files = steam_game_files(app_id, depot_id, manifest_id, "public").await?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new(game_files, &tpk);
    env.unity_version()?;

    let file = env.load_cached("level0")?;
    let mut counts = BTreeMap::<_, usize>::default();
    for obj in file.objects::<()>() {
        *counts.entry(obj.class_id()).or_default() += 1;
    }
    dbg!(counts);

    Ok(())
}
