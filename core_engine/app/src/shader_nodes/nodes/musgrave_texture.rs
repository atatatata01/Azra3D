// MusgraveTexture node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Musgrave";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
                Socket::float("Scale", 5.0),
                Socket::float("Detail", 3.0),
                Socket::float("Dimension", 2.0),
                Socket::float("Lacunarity", 2.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
            ]
}
