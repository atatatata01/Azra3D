// Noise node — full suite (Value/FBM/Perlin/Simplex/White/Cellular/Ridged/Billow/
// DomainWarp/Musgrave) + built-in ProjectionStyle (Triplanar/Planar/Box/Cyl/Sph/None).
// Type + projection are Node params (see inspector ComboBoxes), not separate nodes.
//
// UI fields are free-form (no hard range). Eval applies only safety floors/ceilings
// (scale≠0, octaves≥1, runaway cost cap) so extreme typed values still work.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Noise";

/// Safety ceiling for CPU/GPU loops (not a UI limit — type higher if you want; eval clamps).
pub const OCTAVES_SAFETY_MAX: u32 = 64;

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Offset", [0.0, 0.0, 0.0]),
        Socket::float("Scale", 4.0),
        Socket::float("Octaves", 4.0),
        Socket::float("Lacunarity", 2.0),
        Socket::float("Gain", 0.5),
        Socket::float("Distortion", 0.0),
        Socket::float("Seed", 0.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::float("Value", 0.0)]
}
