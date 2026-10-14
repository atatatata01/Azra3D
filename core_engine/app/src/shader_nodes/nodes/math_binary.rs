// MathBinary node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Math";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("A", 0.0),
                Socket::float("B", 0.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
