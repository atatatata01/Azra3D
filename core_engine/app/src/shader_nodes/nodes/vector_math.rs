// VectorMath node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Vector Math";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("A", [0.0; 3]),
                Socket::vec3("B", [0.0; 3]),
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Vector", [0.0; 3]),
                Socket::float("Value", 0.0),
            ]
}
