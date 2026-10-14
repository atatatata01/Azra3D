// BrickTexture node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Brick";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
                Socket::float("Scale", 5.0),
                Socket::float("Mortar", 0.02),
                Socket::float("Row Offset", 0.5),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5; 3]),
                Socket::float("Factor", 0.0),
            ]
}
