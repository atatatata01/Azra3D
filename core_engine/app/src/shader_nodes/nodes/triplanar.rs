// Triplanar node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Triplanar";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::vec3("Normal", [0.0, 1.0, 0.0]),
                Socket::float("Sharpness", 4.0),
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Weights", [0.0; 3]),   // blend weights per axis
                Socket::vec3("UV X", [0.0; 3]),
                Socket::vec3("UV Y", [0.0; 3]),
                Socket::vec3("UV Z", [0.0; 3]),
            ]
}
