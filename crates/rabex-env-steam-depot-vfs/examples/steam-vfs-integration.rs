use std::sync::Arc;

use anyhow::Result;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::{Environment, rabex};
use rabex_env_steam_depot_vfs::SteamDepotGameFiles;
use steam_depot_vfs::DepotStore;
use steam_depot_vfs::session::LazyCachedAuth;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

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
    env.unity_version()?;

    let file = env.load_cached("level0")?;
    for obj in file.objects::<()>() {
        dbg!(obj.class_id());
    }

    Ok(())
}
