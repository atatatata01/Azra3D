// SeparateXyz node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Separate XYZ";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Vector", [0.0, 0.0, 0.0]),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("X", 0.0),
                Socket::float("Y", 0.0),
                Socket::float("Z", 0.0),
            ]
}
