// HeightToNormal — convert a scalar height field to a tangent-space normal (vec3).
// Used for Master Material Normal / bump workflows without an external normal map.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Height to Normal";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::float("Height", 0.0),
        Socket::float("Strength", 1.0),
        // Sample step in UV/body-local units (bake domain ~[-1,1]).
        Socket::float("Epsilon", 0.02),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![Socket::vec3("Normal", [0.0, 0.0, 1.0])]
}
