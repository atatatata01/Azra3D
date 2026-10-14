// Remap node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Remap";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.0),
                Socket::float("From Min", 0.0),
                Socket::float("From Max", 1.0),
                Socket::float("To Min", 0.0),
                Socket::float("To Max", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
