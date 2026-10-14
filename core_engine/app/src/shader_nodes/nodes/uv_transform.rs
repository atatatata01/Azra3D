// UVTransform node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "UV Transform";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("UV", [0.0; 3]),
                Socket::float("Offset X", 0.0),
                Socket::float("Offset Y", 0.0),
                Socket::float("Rotation", 0.0),
                Socket::float("Scale X", 1.0),
                Socket::float("Scale Y", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("UV", [0.0; 3]),
            ]
}
