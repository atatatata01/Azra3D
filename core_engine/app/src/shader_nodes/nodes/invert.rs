// Invert node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Invert";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::color("Value", [0.5, 0.5, 0.5]),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
            ]
}
