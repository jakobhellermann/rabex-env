//! Rewriting Unity serialized files and asset bundles.
//!
//! [`bundle`] maps the serialized files of bundles, [`objects`] the objects of a serialized
//! file, and [`retarget`] re-encodes them for another Unity version.

pub mod bundle;
pub mod compat;
pub mod fields;
pub mod layout;
#[cfg(feature = "shaders")]
pub mod material;
pub mod objects;
pub mod retarget;
pub mod rewriter;
#[cfg(feature = "shaders")]
pub mod shader;
#[cfg(feature = "shaders")]
pub mod texture;
pub mod transcode;
pub mod value;
