//! End-to-end checks against a real game, which is the only way to exercise the addressables
//! catalog path of [`SceneIndex`]. Point `RABEX_JQ_GAME_DIR` at a game dir (e.g. a Silksong
//! install) to run them; without it they no-op so `cargo test` stays hermetic.
//!
//!   RABEX_JQ_GAME_DIR="$HOME/.local/share/Steam/steamapps/common/Hollow Knight Silksong" \
//!     cargo test -p rabex-jq --test game -- --nocapture

use rabex_env::Environment;
use rabex_env::rabex::tpk::TpkTypeTreeBlob;
use rabex_env::rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_jq::SceneIndex;

fn game_env() -> Option<Environment> {
    let dir = std::env::var_os("RABEX_JQ_GAME_DIR")?;
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    Some(Environment::new_in(dir, tpk).expect("RABEX_JQ_GAME_DIR is not a unity game dir"))
}

#[test]
fn scene_index_enumerates_addressable_scenes() {
    let Some(env) = game_env() else {
        eprintln!("skipping: set RABEX_JQ_GAME_DIR to run");
        return;
    };

    let index = SceneIndex::build(&env).unwrap();
    let names: Vec<_> = index.scene_names().collect();
    assert!(!names.is_empty(), "no scenes resolved");
    // Silksong ships its rooms as addressables scenes; Abyss_01 is one of them. Adjust the
    // expected name if pointing at a different game.
    assert!(
        names.contains(&"Abyss_01"),
        "expected an addressables scene 'Abyss_01' among {} resolved scenes",
        names.len(),
    );
}
