// Cavity node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Cavity";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::float("Epsilon", 0.02),
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
            ]
}
