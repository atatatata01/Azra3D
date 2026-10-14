// GradientTexture node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Gradient";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0; 3]),
                Socket::float("Mode", 0.0),       // 0=linear, 1=radial, 2=spherical
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
                Socket::color("Color", [0.5; 3]),
            ]
}
