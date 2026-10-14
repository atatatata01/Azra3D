// Smoothstep node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Smoothstep";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Edge0", 0.0),
                Socket::float("Edge1", 1.0),
                Socket::float("Value", 0.5),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
