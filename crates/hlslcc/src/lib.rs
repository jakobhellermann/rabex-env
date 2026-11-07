//! Translate DXBC shader bytecode to GLSL, GLSL ES, Vulkan GLSL or Metal with Unity's
//! [HLSLcc](https://github.com/Unity-Technologies/HLSLcc).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fmt;

/// `HLSLCC_FLAG_*` translation flags.
pub mod flags {
    pub const UNIFORM_BUFFER_OBJECT: u32 = 0x1;
    pub const ORIGIN_UPPER_LEFT: u32 = 0x2;
    pub const PIXEL_CENTER_INTEGER: u32 = 0x4;
    pub const GLOBAL_CONSTS_NEVER_IN_UBO: u32 = 0x8;
    pub const GS_ENABLED: u32 = 0x10;
    pub const TESS_ENABLED: u32 = 0x20;
    pub const DUAL_SOURCE_BLENDING: u32 = 0x40;
    pub const INOUT_SEMANTIC_NAMES: u32 = 0x80;
    pub const INOUT_APPEND_SEMANTIC_NAMES: u32 = 0x100;
    pub const COMBINE_TEXTURE_SAMPLERS: u32 = 0x200;
    pub const DISABLE_EXPLICIT_LOCATIONS: u32 = 0x400;
    pub const DISABLE_GLOBALS_STRUCT: u32 = 0x800;
    pub const SAMPLER_PRECISION_ENCODED_IN_NAME: u32 = 0x2000;
    pub const WRAP_UBO: u32 = 0x8000;
    pub const REMOVE_UNUSED_GLOBALS: u32 = 0x10000;
    pub const TRANSLATE_MATRICES: u32 = 0x20000;
    pub const VULKAN_BINDINGS: u32 = 0x40000;
    pub const METAL_SHADOW_SAMPLER_LINEAR: u32 = 0x80000;
    pub const AVOID_SHADER_ATOMIC_COUNTERS: u32 = 0x100000;
    pub const SHADER_FRAMEBUFFER_FETCH: u32 = 0x400000;
    pub const NVN_TARGET: u32 = 0x800000;
    pub const UNIFORM_BUFFER_OBJECT_WITH_INSTANCE_NAME: u32 = 0x1000000;
    pub const METAL_TESSELLATION: u32 = 0x2000000;
    pub const DISABLE_FASTMATH: u32 = 0x4000000;
    pub const FORCE_EXPLICIT_LOCATIONS: u32 = 0x8000000;
    pub const INCLUDE_INSTRUCTIONS_COMMENTS: u32 = 0x10000000;
    pub const KEEP_VARYING_LOCATIONS: u32 = 0x20000000;
    pub const MOBILE_TARGET: u32 = 0x40000000;
}

/// `GLLang`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Language {
    /// Depends on the shader model.
    Default,
    Es100,
    Es300,
    Es310,
    Glsl120,
    Glsl130,
    Glsl140,
    Glsl150,
    Glsl330,
    Glsl400,
    Glsl410,
    Glsl420,
    Glsl430,
    Glsl440,
    Metal,
}

/// `GlExtensions` the generated code may use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extensions {
    pub explicit_attrib_location: bool,
    pub explicit_uniform_location: bool,
    pub shading_language_420pack: bool,
    pub ovr_multiview: bool,
    pub shader_framebuffer_fetch: bool,
}

impl Extensions {
    fn bits(self) -> u32 {
        u32::from(self.explicit_attrib_location)
            | u32::from(self.explicit_uniform_location) << 1
            | u32::from(self.shading_language_420pack) << 2
            | u32::from(self.ovr_multiview) << 3
            | u32::from(self.shader_framebuffer_fetch) << 4
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub flags: u32,
    pub language: Language,
    pub extensions: Extensions,
}

/// `HLSLCC_TEX_DIMENSION`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureDimension {
    Float,
    Int,
    Tex2D,
    Tex3D,
    Cube,
    Tex2DShadow,
    Tex2DArray,
    CubeArray,
    Unknown(i32),
}

impl TextureDimension {
    fn from_raw(raw: c_int) -> Self {
        use TextureDimension::*;
        match raw {
            0 => Float,
            1 => Int,
            2 => Tex2D,
            3 => Tex3D,
            4 => Cube,
            5 => Tex2DShadow,
            6 => Tex2DArray,
            7 => CubeArray,
            other => Unknown(other),
        }
    }
}

/// What HLSLcc reports through `HLSLccReflection`, in callback order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reflection {
    Input {
        name: String,
        bind_index: i32,
    },
    ConstantBuffer {
        name: String,
        size: usize,
        member_count: usize,
    },
    Constant {
        name: String,
        offset: i32,
        /// `SHADER_VARIABLE_TYPE`
        variable_type: i32,
        rows: i32,
        columns: i32,
        is_matrix: bool,
        array_size: i32,
        is_used: bool,
    },
    ConstantBufferBinding {
        name: String,
        bind_index: i32,
    },
    TextureBinding {
        name: String,
        bind_index: i32,
        sampler_index: i32,
        multisampled: bool,
        dimension: TextureDimension,
        is_uav: bool,
    },
    BufferBinding {
        name: String,
        bind_index: i32,
        is_uav: bool,
    },
}

#[derive(Debug)]
pub struct Translation {
    pub source: String,
    pub reflection: Vec<Reflection>,
    /// Warnings, as `(line, message)`.
    pub warnings: Vec<(i32, String)>,
}

#[derive(Debug)]
pub enum Error {
    /// The DXBC container header is malformed or larger than the input.
    InvalidContainer,
    /// HLSLcc failed, with its error diagnostics as `(line, message)`.
    Failed(Vec<(i32, String)>),
    /// HLSLcc produced a name or source that isn't UTF-8.
    NotUtf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidContainer => write!(f, "invalid DXBC container"),
            Error::Failed(diagnostics) if diagnostics.is_empty() => {
                write!(f, "HLSLcc translation failed")
            }
            Error::Failed(diagnostics) => {
                write!(f, "HLSLcc translation failed:")?;
                for (line, message) in diagnostics {
                    write!(f, "\n  line {line}: {message}")?;
                }
                Ok(())
            }
            Error::NotUtf8 => write!(f, "HLSLcc produced non-UTF-8 text"),
        }
    }
}

impl std::error::Error for Error {}

#[repr(C)]
struct Callbacks {
    user: *mut c_void,
    diagnostic: extern "C" fn(*mut c_void, *const c_char, c_int, c_int),
    input: extern "C" fn(*mut c_void, *const c_char, c_int),
    constant_buffer: extern "C" fn(*mut c_void, *const c_char, usize, usize),
    constant:
        extern "C" fn(*mut c_void, *const c_char, c_int, c_int, c_int, c_int, c_int, c_int, c_int),
    constant_buffer_binding: extern "C" fn(*mut c_void, *const c_char, c_int),
    texture_binding: extern "C" fn(*mut c_void, *const c_char, c_int, c_int, c_int, c_int, c_int),
    buffer_binding: extern "C" fn(*mut c_void, *const c_char, c_int, c_int),
    source: extern "C" fn(*mut c_void, *const c_char, usize),
}

unsafe extern "C" {
    fn hlslcc_dependencies_new() -> *mut c_void;
    fn hlslcc_dependencies_free(dependencies: *mut c_void);
    fn hlslcc_translate(
        dxbc: *const u8,
        flags: u32,
        language: c_int,
        extensions: u32,
        dependencies: *mut c_void,
        callbacks: *const Callbacks,
    ) -> c_int;
}

/// `GLSLCrossDependencyData`: what translations of the stages of one program share, like
/// varying locations and Vulkan bindings. Translate a program's stages with the same instance.
pub struct Dependencies(*mut c_void);

impl Dependencies {
    pub fn new() -> Self {
        Dependencies(unsafe { hlslcc_dependencies_new() })
    }
}

impl Default for Dependencies {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Dependencies {
    fn drop(&mut self) {
        unsafe { hlslcc_dependencies_free(self.0) }
    }
}

#[derive(Default)]
struct State {
    reflection: Vec<Reflection>,
    errors: Vec<(i32, String)>,
    warnings: Vec<(i32, String)>,
    source: Option<String>,
    not_utf8: bool,
}

/// # Safety
/// `user` is the `State` passed to `hlslcc_translate` and `name` a C string, for the duration of a callback.
unsafe fn with_name(user: *mut c_void, name: *const c_char, f: impl FnOnce(&mut State, String)) {
    let state = unsafe { &mut *(user as *mut State) };
    match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(name) => f(state, name.to_owned()),
        Err(_) => state.not_utf8 = true,
    }
}

extern "C" fn on_diagnostic(
    user: *mut c_void,
    message: *const c_char,
    line: c_int,
    is_error: c_int,
) {
    unsafe {
        with_name(user, message, |s, message| {
            if is_error != 0 {
                s.errors.push((line, message));
            } else {
                s.warnings.push((line, message));
            }
        })
    }
}

extern "C" fn on_input(user: *mut c_void, name: *const c_char, bind_index: c_int) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection.push(Reflection::Input { name, bind_index })
        })
    }
}

extern "C" fn on_constant_buffer(
    user: *mut c_void,
    name: *const c_char,
    size: usize,
    member_count: usize,
) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection.push(Reflection::ConstantBuffer {
                name,
                size,
                member_count,
            })
        })
    }
}

#[allow(clippy::too_many_arguments)]
extern "C" fn on_constant(
    user: *mut c_void,
    name: *const c_char,
    offset: c_int,
    variable_type: c_int,
    rows: c_int,
    columns: c_int,
    is_matrix: c_int,
    array_size: c_int,
    is_used: c_int,
) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection.push(Reflection::Constant {
                name,
                offset,
                variable_type,
                rows,
                columns,
                is_matrix: is_matrix != 0,
                array_size,
                is_used: is_used != 0,
            })
        })
    }
}

extern "C" fn on_constant_buffer_binding(
    user: *mut c_void,
    name: *const c_char,
    bind_index: c_int,
) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection
                .push(Reflection::ConstantBufferBinding { name, bind_index })
        })
    }
}

extern "C" fn on_texture_binding(
    user: *mut c_void,
    name: *const c_char,
    bind_index: c_int,
    sampler_index: c_int,
    multisampled: c_int,
    dimension: c_int,
    is_uav: c_int,
) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection.push(Reflection::TextureBinding {
                name,
                bind_index,
                sampler_index,
                multisampled: multisampled != 0,
                dimension: TextureDimension::from_raw(dimension),
                is_uav: is_uav != 0,
            })
        })
    }
}

extern "C" fn on_buffer_binding(
    user: *mut c_void,
    name: *const c_char,
    bind_index: c_int,
    is_uav: c_int,
) {
    unsafe {
        with_name(user, name, |s, name| {
            s.reflection.push(Reflection::BufferBinding {
                name,
                bind_index,
                is_uav: is_uav != 0,
            })
        })
    }
}

extern "C" fn on_source(user: *mut c_void, source: *const c_char, length: usize) {
    let state = unsafe { &mut *(user as *mut State) };
    let bytes = unsafe { std::slice::from_raw_parts(source as *const u8, length) };
    match std::str::from_utf8(bytes) {
        Ok(source) => state.source = Some(source.to_owned()),
        Err(_) => state.not_utf8 = true,
    }
}

/// Translate a DXBC container.
///
/// # Safety
/// HLSLcc trusts its input: `dxbc` must be a well-formed container whose `RDEF` chunk describes
/// every constant buffer offset and resource the bytecode accesses, otherwise HLSLcc reads out of
/// bounds or dereferences null pointers.
pub unsafe fn translate(
    dxbc: &[u8],
    options: &Options,
    dependencies: &mut Dependencies,
) -> Result<Translation, Error> {
    if dxbc.len() < 32 || &dxbc[..4] != b"DXBC" {
        return Err(Error::InvalidContainer);
    }
    let size = u32::from_le_bytes(dxbc[24..28].try_into().unwrap()) as usize;
    if size > dxbc.len() {
        return Err(Error::InvalidContainer);
    }

    let mut state = State::default();
    let callbacks = Callbacks {
        user: &mut state as *mut State as *mut c_void,
        diagnostic: on_diagnostic,
        input: on_input,
        constant_buffer: on_constant_buffer,
        constant: on_constant,
        constant_buffer_binding: on_constant_buffer_binding,
        texture_binding: on_texture_binding,
        buffer_binding: on_buffer_binding,
        source: on_source,
    };
    let ok = unsafe {
        hlslcc_translate(
            dxbc.as_ptr(),
            options.flags,
            options.language as c_int,
            options.extensions.bits(),
            dependencies.0,
            &callbacks,
        )
    };
    if state.not_utf8 {
        return Err(Error::NotUtf8);
    }
    match (ok, state.source) {
        (0, _) | (_, None) => Err(Error::Failed(state.errors)),
        (_, Some(_)) if !state.errors.is_empty() => Err(Error::Failed(state.errors)),
        (_, Some(source)) => Ok(Translation {
            source,
            reflection: state.reflection,
            warnings: state.warnings,
        }),
    }
}
