// Voronoi node
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Voronoi";

pub fn inputs() -> Vec<Socket> {
    vec![
                Socket::vec3("Offset", [0.0, 0.0, 0.0]),
                Socket::float("Scale", 5.0),
                Socket::float("Randomness", 1.0),
            ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
                Socket::float("Distance", 0.0),
                Socket::float("Cell ID", 0.0),
                Socket::color("Cell Color", [0.5, 0.5, 0.5]),
            ]
}
