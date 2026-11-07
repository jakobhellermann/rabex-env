//! Unity's enums whose values appear in serialized shaders, named like Unity's C# definitions.
//! The fixture tests check them against the editor's own definitions of every fixture version.

/// A raw enum value outside the table; [`TryFrom`] reports it instead of guessing.
#[derive(Debug)]
pub struct UnknownEnumValue {
    pub enum_name: &'static str,
    pub value: i64,
}

impl std::fmt::Display for UnknownEnumValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} {}", self.enum_name, self.value)
    }
}
impl std::error::Error for UnknownEnumValue {}

macro_rules! unity_enum {
    ($(#[$meta:meta])* $module:ident $enum_name:ident = $unity:literal { $($(#[$item_meta:meta])* $name:ident = $value:expr),* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_upper_case_globals)]
        pub mod $module {
            /// The C# type's full name.
            pub const UNITY_NAME: &str = $unity;
            $($(#[$item_meta])* pub const $name: u32 = $value;)*
            pub const ALL: &[(&str, u32)] = &[$((stringify!($name), $value)),*];

            /// The table as a type: values parse into it and fail on anything else.
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub enum $enum_name {
                $(
                    $(#[$item_meta])*
                    $name
                ),*
            }

            impl $enum_name {
                pub fn value(self) -> u32 {
                    match self {
                        $(Self::$name => $value),*
                    }
                }
            }

            impl std::fmt::Display for $enum_name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "{}::{}", UNITY_NAME, self.name())
                }
            }

            impl $enum_name {
                pub fn name(self) -> &'static str {
                    match self {
                        $(Self::$name => stringify!($name)),*
                    }
                }
            }

            impl std::convert::TryFrom<i64> for $enum_name {
                type Error = super::UnknownEnumValue;
                fn try_from(value: i64) -> Result<Self, Self::Error> {
                    $(
                        if value == i64::from($value) {
                            return Ok(Self::$name);
                        }
                    )*
                    Err(super::UnknownEnumValue { enum_name: UNITY_NAME, value })
                }
            }
        }
    };
}

unity_enum! {
    /// The public part of the platform enum of `Shader.platforms`; Unity 6 added `WebGPU` and
    /// `Switch2`.
    shader_compiler_platform ShaderCompilerPlatform = "UnityEditor.Rendering.ShaderCompilerPlatform" {
        None = 0,
        D3D = 4,
        GLES20 = 5,
        GLES3x = 9,
        PS4 = 11,
        XboxOneD3D11 = 12,
        Metal = 14,
        OpenGLCore = 15,
        Vulkan = 18,
        Switch = 19,
        XboxOneD3D12 = 20,
        GameCoreXboxOne = 21,
        GameCoreXboxSeries = 22,
        PS5 = 23,
        PS5NGGC = 24,
        GameCore = 25,
        WebGPU = 26,
        Switch2 = 27,
    }
}

unity_enum! {
    texture_dimension TextureDimension = "UnityEngine.Rendering.TextureDimension" {
        None = 0,
        Any = 1,
        Tex2D = 2,
        Tex3D = 3,
        Cube = 4,
        Tex2DArray = 5,
        CubeArray = 6,
        /// -1 in C#.
        Unknown = u32::MAX,
    }
}

unity_enum! {
    /// The vertex channels of a program's channel table.
    vertex_attribute VertexAttribute = "UnityEngine.Rendering.VertexAttribute" {
        Position = 0,
        Normal = 1,
        Tangent = 2,
        Color = 3,
        TexCoord0 = 4,
        TexCoord1 = 5,
        TexCoord2 = 6,
        TexCoord3 = 7,
        TexCoord4 = 8,
        TexCoord5 = 9,
        TexCoord6 = 10,
        TexCoord7 = 11,
        BlendWeight = 12,
        BlendIndices = 13,
    }
}

unity_enum! {
    /// The data type of a constant in the program parameters.
    shader_param_type ShaderParamType = "UnityEngine.Rendering.ShaderParamType" {
        Float = 0,
        Int = 1,
        Bool = 2,
        Half = 3,
        Short = 4,
        UInt = 5,
    }
}

unity_enum! {
    /// The graphics APIs of `BuildSettings.m_GraphicsAPIs`.
    graphics_device_type GraphicsDeviceType = "UnityEngine.Rendering.GraphicsDeviceType" {
        OpenGL2 = 0,
        Direct3D9 = 1,
        Direct3D11 = 2,
        PlayStation3 = 3,
        Null = 4,
        Xbox360 = 6,
        OpenGLES2 = 8,
        OpenGLES3 = 11,
        PlayStationVita = 12,
        PlayStation4 = 13,
        XboxOne = 14,
        PlayStationMobile = 15,
        Metal = 16,
        OpenGLCore = 17,
        Direct3D12 = 18,
        N3DS = 19,
        Vulkan = 21,
        Switch = 22,
        XboxOneD3D12 = 23,
        GameCoreXboxOne = 24,
        GameCoreXboxSeries = 25,
        PlayStation5 = 26,
        PlayStation5NGGC = 27,
        WebGPU = 28,
        Switch2 = 29,
        /// -1 in C#.
        GameCoreScarlett = u32::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::graphics_device_type::GraphicsDeviceType;
    use super::shader_compiler_platform::ShaderCompilerPlatform;
    use super::shader_param_type::ShaderParamType;
    use super::texture_dimension::TextureDimension;
    use super::vertex_attribute::VertexAttribute;
    use std::convert::TryFrom;

    #[test]
    fn enums_parse_their_tables_and_fail_on_unknowns() {
        // every table entry parses back to its name and value
        for &(name, value) in super::vertex_attribute::ALL {
            let parsed = VertexAttribute::try_from(i64::from(value)).unwrap();
            assert_eq!(parsed.name(), name);
            assert_eq!(parsed.value(), value);
        }
        for &(name, value) in super::shader_param_type::ALL {
            let parsed = ShaderParamType::try_from(i64::from(value)).unwrap();
            assert_eq!(parsed.name(), name);
            assert_eq!(parsed.value(), value);
        }
        // C# writes -1 as its bit pattern
        let unknown = TextureDimension::try_from(i64::from(u32::MAX)).unwrap();
        assert_eq!(unknown, TextureDimension::Unknown);
        for &(name, value) in super::shader_compiler_platform::ALL {
            let parsed = ShaderCompilerPlatform::try_from(i64::from(value)).unwrap();
            assert_eq!(parsed.name(), name);
        }
        for &(name, value) in super::graphics_device_type::ALL {
            let parsed = GraphicsDeviceType::try_from(i64::from(value)).unwrap();
            assert_eq!(parsed.name(), name);
        }

        let error = VertexAttribute::try_from(99).unwrap_err().to_string();
        assert_eq!(error, "unknown UnityEngine.Rendering.VertexAttribute 99");
        assert_eq!(
            TextureDimension::Tex2D.to_string(),
            "UnityEngine.Rendering.TextureDimension::Tex2D"
        );
    }
}
