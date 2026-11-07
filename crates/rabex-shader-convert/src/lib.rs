//! Convert compiled Unity shader programs between graphics APIs.
//!
//! [`backend`]s translate between [`Program`] formats; [`target`]s chain them into the programs and
//! parameters a Unity graphics API expects.

use anyhow::Result;
use rabex_shader::Stage;

pub mod backend;
pub mod spirv;
pub mod split_samplers;
pub mod target;
pub mod vulkan_bindings;

/// A shader program in one of the formats translators exchange.
#[derive(Clone, Debug)]
pub enum Program {
    /// A DXBC container, with an `RDEF` chunk describing its resources.
    Dxbc(Vec<u8>),
    Glsl(String),
    Spirv(Vec<u32>),
}

/// Converts programs of one format to another.
pub trait Translator {
    fn translate(&self, stage: Stage, program: Program) -> Result<Program>;
}
