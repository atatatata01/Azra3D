// shader_nodes/types.rs
use serde::{Serialize, Deserialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SocketType {
    Float,
    Vec3,
    Color,  // same as Vec3 but clamped 0-1 in UI
}

impl SocketType {
    pub fn wgsl_type(&self) -> &'static str {
        match self {
            Self::Float => "f32",
            Self::Vec3 | Self::Color => "vec3<f32>",
        }
    }
    pub fn compatible(&self, other: &Self) -> bool {
        // Floatâ†”Color/Vec3 allowed (broadcast / luminance) so Noiseâ†’albedo and ColorRampâ†’fac
        // connections work like Blender â€” required for MaterialPresets + PR-VIS bake visibility.
        matches!(
            (self, other),
            (Self::Float, Self::Float)
            | (Self::Vec3, Self::Vec3)
            | (Self::Color, Self::Color)
            | (Self::Vec3, Self::Color)
            | (Self::Color, Self::Vec3)
            | (Self::Float, Self::Vec3)
            | (Self::Float, Self::Color)
            | (Self::Vec3, Self::Float)
            | (Self::Color, Self::Float)
        )
    }
}

// ====================================================================
// Socket definition (input or output of a node)
// ====================================================================
#[derive(Clone, Debug)]
pub struct Socket {
    pub name: &'static str,
    pub socket_type: SocketType,
    pub default_f32: f32,
    pub default_vec3: [f32; 3],
}

impl Socket {
    pub fn float(name: &'static str, default: f32) -> Self {
        Self { name, socket_type: SocketType::Float, default_f32: default, default_vec3: [0.0; 3] }
    }
    pub fn vec3(name: &'static str, default: [f32; 3]) -> Self {
        Self { name, socket_type: SocketType::Vec3, default_f32: 0.0, default_vec3: default }
    }
    pub fn color(name: &'static str, default: [f32; 3]) -> Self {
        Self { name, socket_type: SocketType::Color, default_f32: 0.0, default_vec3: default }
    }
}

// ====================================================================
// Node types â€” each defines inputs, outputs, and WGSL body
// ====================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputDefault {
    pub value_f32: f32,
    pub value_vec3: [f32; 3],
}
