// TextureSample node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Texture Sample";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
                Socket::float("Pattern", 0.0),   // 0=checker, 1=gradient, 2=brick
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5; 3]),
                Socket::float("Alpha", 1.0),
            ]
}
