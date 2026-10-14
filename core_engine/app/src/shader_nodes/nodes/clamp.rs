// Clamp node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Clamp";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.5),
                Socket::float("Min", 0.0),
                Socket::float("Max", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
