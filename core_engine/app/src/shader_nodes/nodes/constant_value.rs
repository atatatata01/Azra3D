// ConstantValue node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Value";

pub fn inputs() -> Vec<Socket> {
    vec![]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Value", 0.5),
            ]
}
