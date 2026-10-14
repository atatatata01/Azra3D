// Checker Texture (Blender parity, G9): parity of floor(domain * scale) summed
// over the three axes. Formula must stay IDENTICAL in eval_nodes.rs (CPU) and
// graph.rs gen_node_wgsl (WGSL) — two's-complement `& 1` odd-tests match for
// negative cells on both sides, which is why parity uses ints, never fract().
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Checker";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Vector", [0.0; 3]),
        Socket::color("Color1", [0.8, 0.8, 0.8]),
        Socket::color("Color2", [0.2, 0.2, 0.2]),
        Socket::float("Scale", 5.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
        Socket::color("Color", [0.5, 0.5, 0.5]),
        Socket::float("Fac", 0.0),
    ]
}
