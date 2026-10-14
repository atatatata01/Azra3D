// SurfaceDirection — angle-based mask from the SURFACE NORMAL against a chosen world axis.
//
// `dot(n, axis)` remapped −1..1 → 0..1, then shaped by Contrast/Strength exactly like
// SdfCurvature's tail. This is the "snow on top / dirt on the bottom / wear on one side"
// generator: axis +Y gives 1 on up-facing surfaces, 0 on down-facing, 0.5 on vertical walls.
//
// WHY THIS IS NOT `Slope`: `Slope` reads input socket 0 (a vec3 default) and, on the planar
// bake domain, that default is a CONSTANT — every pixel gets the same normal, so `Slope`
// bakes a flat gray card. This node instead recovers the REAL surface normal from the body
// SDF gradient (the same `project_to_surface` + `sdf_grad` machinery `SdfCurvature` uses), so
// it is correct on the flat bake domain the way curvature is. In the live WGSL shader the
// per-pixel geometric normal `n` is already an argument to `eval_node_material`, so the WGSL
// arm is EXACT — not the synthetic-height-field placeholder the other surface analyzers use.
//
// `Node.sdf_curv_region` selects the axis (0=+X 1=−X 2=+Y 3=−Y 4=+Z 5=−Z) — reusing the
// existing per-node u32 rather than adding a serialized field.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "Direction";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        // FD step for the SDF gradient (CPU arm); auto-floored against body size.
        Socket::float("Epsilon", 0.02),
        // Offset surface before measuring (positive = expanded shell). Matches SdfCurvature.
        Socket::float("GrowShrink", 0.0),
        // Max distance allowed when projecting a seed onto the surface (world units).
        Socket::float("Band", 0.2),
        // Falloff shaping of the 0..1 dot: 1 = linear, >1 tightens toward the axis.
        Socket::float("Contrast", 1.0),
        Socket::float("Strength", 1.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
        Socket::float("Mask", 0.0),  // shaped 0–1 against the selected axis
        Socket::float("Dot", 0.0),   // raw dot(n, axis) in −1..1
    ]
}

/// The unit axis for a `sdf_curv_region` code. Any out-of-range code falls back to +Y
/// (the "top" case), which is the overwhelmingly common authoring intent.
pub fn axis_vec(code: u32) -> [f32; 3] {
    match code {
        0 => [1.0, 0.0, 0.0],
        1 => [-1.0, 0.0, 0.0],
        3 => [0.0, -1.0, 0.0],
        4 => [0.0, 0.0, 1.0],
        5 => [0.0, 0.0, -1.0],
        _ => [0.0, 1.0, 0.0],
    }
}

/// Human label for an axis code (UI + round-trip tags).
pub fn axis_label(code: u32) -> &'static str {
    match code {
        0 => "+X",
        1 => "-X",
        3 => "-Y",
        4 => "+Z",
        5 => "-Z",
        _ => "+Y",
    }
}
