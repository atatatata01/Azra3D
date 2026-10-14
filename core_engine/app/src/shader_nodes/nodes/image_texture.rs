// ImageTexture node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Image Texture";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [1.0, 1.0, 1.0]),
                Socket::float("Alpha", 1.0),
            ]
}
