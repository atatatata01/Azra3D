// MixRgb node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Mix Color";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Factor", 0.5),
                Socket::color("Color1", [0.5, 0.5, 0.5]),
                Socket::color("Color2", [1.0, 1.0, 1.0]),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
            ]
}
