// ObjectInfo node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Object Info";

pub fn inputs() -> Vec<Socket> {
    vec![]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Index", 0.0),
            ]
}
