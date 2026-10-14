// BrightnessContrast node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Bright/Contrast";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
                Socket::float("Brightness", 0.0),
                Socket::float("Contrast", 0.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.5, 0.5, 0.5]),
            ]
}
