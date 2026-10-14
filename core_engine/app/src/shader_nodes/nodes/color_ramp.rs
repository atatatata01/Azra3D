// ColorRamp node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Color Ramp";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Factor", 0.5),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
            ]
}
