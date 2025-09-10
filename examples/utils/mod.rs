#![allow(dead_code)]

use anyhow::Result;
use rabex::{tpk::TpkTypeTreeBlob, typetree::typetree_cache::sync::TypeTreeCache};
use rabex_env::{Environment, game_files::GameFiles};

pub fn for_each_steam_game(mut f: impl FnMut(Environment) -> Result<()>) -> Result<()> {
    let steam = steamlocate::SteamDir::locate()?;
    for lib in steam.libraries()? {
        let lib = lib?;
        for app in lib.apps() {
            let app = app?;
            let path = lib.resolve_app_dir(&app);

            let Ok(game_files) = GameFiles::probe(path) else {
                continue;
            };

            // PERF: reuse
            let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
            let env = Environment::new(game_files, tpk);
            f(env)?;
        }
    }
    Ok(())
}

pub fn find_game(name: &str) -> Result<Option<Environment>> {
    let name_filter = name.to_lowercase();

    let steam = steamlocate::SteamDir::locate()?;
    for lib in steam.libraries()? {
        let lib = lib?;
        for app in lib.apps() {
            let app = app?;
            let path = lib.resolve_app_dir(&app);

            let Ok(game_files) = GameFiles::probe(path) else {
                continue;
            };

            if app
                .name
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&name_filter)
            {
                let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
                let env = Environment::new(game_files, tpk);
                return Ok(Some(env));
            }
        }
    }
    Ok(None)
}
