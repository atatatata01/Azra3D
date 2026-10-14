// AmbientOcclusion node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Ambient Occlusion";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::vec3("Normal", [0.0, 1.0, 0.0]),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
            ]
}
