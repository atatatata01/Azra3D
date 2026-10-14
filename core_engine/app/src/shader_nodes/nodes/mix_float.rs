// MixFloat node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Mix";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::float("A", 0.0),
                Socket::float("B", 1.0),
                Socket::float("Factor", 0.5),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Result", 0.0),
            ]
}
