// ConstantColor node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Color";

pub fn inputs() -> Vec<Socket> {
    vec![]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::color("Color", [0.8, 0.8, 0.8]),
            ]
}
