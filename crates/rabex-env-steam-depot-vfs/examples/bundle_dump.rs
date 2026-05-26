// TODO(ai-review): review for style and correctness
//! Minimal repro of the viewer's `/file/structured` + `/file/structured/node`
//! routes for a single bundle, so the underlying rabex-env work can be
//! profiled / debugged without the HTTP layer in the way.
//!
//! Hardcoded to Hollow Knight Silksong's `costs.bundle` per the
//! example URL on issue tracker; edit the consts at the top for any
//! other depot.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::typetree_cache::sync::TypeTreeCache;
use rabex_env::env::Data;
use rabex_env::resolver::EnvResolver;
use rabex_env::{Environment, rabex};
use rabex_env_steam_depot_vfs::SteamDepotGameFiles;
use steam_depot_vfs::DepotStore;
use steam_depot_vfs::session::LazyCachedAuth;

/// Local store path; same convention the viewer uses by default.
const STORE_ROOT: &str = "/Users/sipgatejj/.personal/steam/steam-depot-vfs/target/steam-vfs-store";

const APP_ID: u32 = 1030300;
const DEPOT_ID: u32 = 1030301;
const MANIFEST_ID: u64 = 4421626056705534276;
const BRANCH: &str = "public";

/// Manifest-relative path of the bundle file.
const BUNDLE_PATH: &str = "Hollow Knight Silksong_Data/StreamingAssets/aa/StandaloneWindows64/dataassets_assets_assets/dataassets/costs.bundle";

/// One specific archive entry inside the bundle + an object inside it.
/// `obj:` is implicit — the literal i64 path_id.
const ARCHIVE_ENTRY: &str = "CAB-1e29d7e3d94b56f1c2801e198547d035";
const OBJECT_PATH_ID: i64 = -5298865543675552381;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rabex_env=info,steam_depot_vfs=warn".into()),
        )
        .init();

    let auth = LazyCachedAuth::prepare(
        LazyCachedAuth::default_refresh_token_cache(),
        std::env::var("STEAM_USERNAME").expect("missing STEAM_USERNAME"),
        std::env::var("STEAM_PASSWORD").expect("missing STEAM_PASSWORD"),
    )
    .await?;

    let store = DepotStore::new(STORE_ROOT.into());
    let manifest_store = Arc::new(
        store
            .open_depot_manifest(Arc::new(auth), APP_ID, DEPOT_ID, MANIFEST_ID, BRANCH)
            .await?,
    );

    let game_files = SteamDepotGameFiles::new(manifest_store.clone())?;
    let data_dir = game_files.data_dir().display().to_string();
    let tpk = TypeTreeCache::new(TpkTypeTreeBlob::embedded());
    let env = Environment::new(game_files, &tpk);
    // Warm the unity-version OnceLock — done once on first bundle open
    // in the route, charge it to setup time so the per-route numbers
    // below are apples to apples.
    let _ = env.unity_version()?;

    // ---- /file/structured (build_tree on the bundle) -------------------

    let bundle_relative = BUNDLE_PATH
        .strip_prefix(&format!("{data_dir}/"))
        .unwrap_or(BUNDLE_PATH);

    let started = Instant::now();
    let bundle_bytes = env.game_files.read_path(Path::new(bundle_relative))?;
    let read_bundle_ms = started.elapsed();
    let unity_version = env.unity_version()?.clone();
    let bundle = BundleFileReader::from_reader(
        Cursor::new(bundle_bytes.as_ref()),
        &ExtractionConfig::default().with_fallback_unity_version(unity_version),
    )?;
    let open_bundle_ms = started.elapsed();
    println!(
        "open bundle: read={:?} parse={:?} entries={}",
        read_bundle_ms,
        open_bundle_ms - read_bundle_ms,
        bundle.files().len()
    );

    // Iterate every SerializedFile entry the way `unity::bundle::build_tree`
    // does — insert into the env cache + walk objects/transforms to
    // mirror the class-stats / hierarchy / loose-section work.
    let sf_paths: std::collections::HashSet<&str> =
        bundle.serialized_files().map(|e| e.path.as_str()).collect();
    let started = Instant::now();
    let mut total_objects = 0usize;
    let mut total_transforms = 0usize;
    for entry in bundle.files() {
        if !sf_paths.contains(entry.path.as_str()) {
            continue;
        }
        let per_entry = Instant::now();
        let bytes = bundle
            .read_at(&entry.path)?
            .expect("entry listed in serialized_files but read_at returned None");
        let sf = SerializedFile::from_reader(&mut Cursor::new(bytes.as_slice()))?;
        let handle = env.insert_cache(entry.path.clone().into(), sf, Data::InMemory(bytes));
        let mut object_count = 0usize;
        for _ in handle.file.objects() {
            object_count += 1;
        }
        let mut transform_count = 0usize;
        for transform in handle.transforms() {
            let _ = transform.read()?;
            transform_count += 1;
        }
        total_objects += object_count;
        total_transforms += transform_count;
        println!(
            "  archive {}: parse+walk={:?} objects={} transforms={}",
            entry.path,
            per_entry.elapsed(),
            object_count,
            transform_count,
        );
    }
    println!(
        "build_tree-equivalent: {:?} (objects={} transforms={})",
        started.elapsed(),
        total_objects,
        total_transforms
    );

    // ---- /file/structured/node (dump_bundle_object_json) ---------------

    let started = Instant::now();
    let handle = env.load_external_file(Path::new(ARCHIVE_ENTRY))?;
    let object = handle.object_at::<serde_value::Value>(OBJECT_PATH_ID)?;
    let read_obj_ms = started.elapsed();
    let value = object.read()?;
    let read_value_ms = started.elapsed();
    let json = serde_json::to_string_pretty(&value)?;
    println!(
        "node dump: object_at={:?} read_value={:?} serialize={:?} json_len={}",
        read_obj_ms,
        read_value_ms - read_obj_ms,
        started.elapsed() - read_value_ms,
        json.len()
    );

    Ok(())
}
