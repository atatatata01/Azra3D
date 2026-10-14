// Gamma (Blender parity, G9): per-channel pow(max(color, 0), max(gamma, 1e-4)).
// The gamma floor keeps pow() defined identically on CPU and WGSL.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Gamma";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::color("Color", [1.0, 1.0, 1.0]),
        Socket::float("Gamma", 1.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::color("Color", [1.0, 1.0, 1.0])]
}
