// CylindricalMapping node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Cylindrical";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::float("Axis", 1.0),   // 0=X, 1=Y, 2=Z
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("UV", [0.0; 3]),
            ]
}
