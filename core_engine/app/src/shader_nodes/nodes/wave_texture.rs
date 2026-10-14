// WaveTexture node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Wave";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
                Socket::float("Scale", 5.0),
                Socket::float("Distortion", 0.0),
                Socket::float("Profile", 0.0),    // 0=sine, 1=saw, 2=triangle
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
                Socket::color("Color", [0.5; 3]),
            ]
}
