// SdfThickness — interior distance along the outward normal (artistic thickness mask).
// CPU: march −n into the body SDF; WGSL live: abs(scene_sdf(p - n * step)) style samples.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Thickness";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        Socket::vec3("Normal", [0.0, 1.0, 0.0]),
        // Max search distance (world units); larger = thicker shells read higher.
        Socket::float("MaxDistance", 0.25),
        Socket::float("Contrast", 1.0),
        Socket::float("Strength", 1.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::float("Value", 0.0)]
}
