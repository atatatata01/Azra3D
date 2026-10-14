// Reroute — wire passthrough (Blender parity, G8). Sockets are Float, but
// `SocketType::compatible` is total (every pair connects) and the CPU evaluator
// passes the RAW upstream `Val` through, so colours survive a reroute unchanged.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Reroute";

pub fn inputs() -> Vec<Socket> {
    vec![Socket::float("In", 0.0)]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::float("Out", 0.0)]
}
