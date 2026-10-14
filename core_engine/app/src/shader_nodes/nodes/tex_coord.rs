// TexCoord node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Texture Coordinate";

pub fn inputs() -> Vec<Socket> {
    vec![]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Position", [0.0; 3]),
                Socket::vec3("Normal", [0.0, 1.0, 0.0]),
            ]
}
