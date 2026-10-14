// PrimSdfMask — use a scene primitive's SDF as a 0–1 mask (alpha) to place effects.
// Pick prim via node.sdf_prim_uid (UI combo of scene prims). Grow/Shrink offsets the
// isosurface; Softness is the band width; Contrast/Strength shape the mask.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Prim SDF Mask";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        // Positive = grow (expand mask), negative = shrink
        Socket::float("GrowShrink", 0.0),
        // Soft edge band width (distance units)
        Socket::float("Softness", 0.08),
        Socket::float("Contrast", 1.0),
        Socket::float("Strength", 1.0),
        // >0.5 inverts (outside becomes mask)
        Socket::float("Invert", 0.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
        Socket::float("Mask", 0.0),     // 0–1 alpha
        Socket::float("Distance", 0.0), // raw SDF (after grow offset)
        Socket::float("Inside", 0.0),   // 1 inside surface, 0 outside
    ]
}
