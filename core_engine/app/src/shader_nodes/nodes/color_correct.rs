// ColorCorrect node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Color Correct";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
                Socket::float("Hue", 0.0),
                Socket::float("Saturation", 1.0),
                Socket::float("Value", 1.0),
                Socket::float("Gamma", 1.0),
                Socket::float("Exposure", 0.0),
                Socket::float("Contrast", 0.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
            ]
}
