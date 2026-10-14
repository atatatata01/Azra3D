// PlanarMapping node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Planar";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::float("Axis", 1.0),   // 0=XY, 1=XZ, 2=YZ
                Socket::float("Scale", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("UV", [0.0; 3]),
            ]
}
