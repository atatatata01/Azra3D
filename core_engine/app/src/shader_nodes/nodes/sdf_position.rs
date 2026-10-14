// SdfPosition — body-local position as 0..1 colour (AABB-normalized).
// Used for gradients, dirt falloff by height, object-space masks.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Position";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        // 0=RGB all axes, 1=X only, 2=Y only, 3=Z only (replicated to RGB).
        Socket::float("Axis", 0.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
        Socket::color("Color", [0.5, 0.5, 0.5]),
        Socket::float("X", 0.5),
        Socket::float("Y", 0.5),
        Socket::float("Z", 0.5),
    ]
}
