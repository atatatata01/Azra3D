// Frame — visual grouping box (Blender parity, G8). No sockets: it can never
// join an evaluation chain, so the evaluator's no-op arm is unreachable-but-total.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Frame";

pub fn inputs() -> Vec<Socket> {
    vec![]
}

pub fn outputs() -> Vec<Socket> {
    vec![]
}
