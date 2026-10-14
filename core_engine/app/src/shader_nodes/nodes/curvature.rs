// Curvature node — analytic height-field fallback.
// Prefer SdfCurvature when you need real SDF convex/concave with grow/shrink.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Curvature";
pub const CONTEXT_STUB: &str = "fn sn_mesh_curvature(position: vec3<f32>, radius: f32) -> f32 { return 0.0; }";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        Socket::float("Epsilon", 0.02),
        Socket::float("Contrast", 1.0),
        Socket::float("Strength", 1.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::float("Value", 0.0)]
}
