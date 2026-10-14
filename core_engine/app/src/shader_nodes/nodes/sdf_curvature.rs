// SdfCurvature — 3D surface mean-curvature from body/prim SDF (not a UV-plane silhouette).
// CPU bake: project UV seeds onto the isosurface (triplanar through body AABB), evaluate
// level-set mean curvature. Region: Both / Convex (edges, line-art) / Concave (cavities).
// GrowShrink offsets the surface; Band = projection accept radius; Contrast + Strength shape mask.
use crate::shader_nodes::types::Socket;

pub const LABEL: &str = "SDF Curvature";

pub fn inputs() -> Vec<Socket> {
    vec![
        Socket::vec3("Position", [0.0; 3]),
        // FD step for Hessian (auto-scales with body size if small)
        Socket::float("Epsilon", 0.02),
        // Offset surface before measuring (positive = expanded shell)
        Socket::float("GrowShrink", 0.0),
        // Max distance allowed when projecting seed → surface (world units)
        Socket::float("Band", 0.2),
        Socket::float("Contrast", 1.5),
        Socket::float("Strength", 1.0),
    ]
}

pub fn outputs() -> Vec<Socket> {
    vec![
        Socket::float("Mask", 0.0),      // shaped 0–1 (region-filtered)
        Socket::float("Curvature", 0.0), // signed κ (convex +, concave −) in ~−1..1
        Socket::float("Convex", 0.0),    // edge wear / line art
        Socket::float("Concave", 0.0),   // cavity / dirt
    ]
}
