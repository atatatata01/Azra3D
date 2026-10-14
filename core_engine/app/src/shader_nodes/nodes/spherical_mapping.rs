// SphericalMapping node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Spherical";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("UV", [0.0; 3]),
            ]
}
