// MathUnary node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Math (Unary)";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
