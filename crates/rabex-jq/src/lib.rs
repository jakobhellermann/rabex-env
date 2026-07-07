//! Run [jq](https://jqlang.org) queries over Unity serialized objects.
//!
//! The engine is [`QueryRunner`]: compile a query once, then [`exec`](QueryRunner::exec) it over
//! object values. On top of stock jq (via [jaq](https://github.com/01mf02/jaq)) it adds a native
//! **`deref`** filter that follows a PPtr to its target object through an [`Environment`], plus the
//! convenience definitions in `defs.jq` (`go`, `transform`, `parent`, `path`, `components`, …).
//!
//! Values fed to a query should first be [`enrich`]ed: that rewrites raw `{m_FileID, m_PathID}`
//! PPtrs into the `{file, path_id, class_id}` shape `deref` consumes, and tags the object with
//! `_file` / `_scene` / `_type`. See [`enrich`] for exactly what it adds.
//!
//! ```no_run
//! # use rabex_env::Environment;
//! # fn f(env: &Environment, mut value: rabex_jq::jaq_json::Val, file: &rabex_env::handle::SerializedFileHandle<'_>, path: &str) -> anyhow::Result<()> {
//! use rabex_jq::{QueryRunner, SceneIndex, enrich, Enrich};
//!
//! let scenes = SceneIndex::build(env)?;
//! enrich(&mut value, path, file, Enrich { scenes: Some(&scenes), script: None })?;
//!
//! let runner = QueryRunner::new("go | path")?;
//! for out in runner.exec(env, value)? {
//!     println!("{out}");
//! }
//! # Ok(()) }
//! ```
//!
//! [`Environment`]: rabex_env::Environment

mod enrich;
mod pptr;
mod query;
mod scenes;

// Re-exported so downstream crates name the exact same `Val` type (incl. the `sync` feature).
pub use jaq_json;

pub use enrich::{Enrich, enrich};
pub use pptr::{QualifiedPPtr, qualify_pptrs};
pub use query::{HasEnv, QueryRunner};
pub use scenes::SceneIndex;
