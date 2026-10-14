//! Offset rails (BEVEL.md §4 STEP 1) — constant in-plane perpendicular per directed half-edge.
//!
//! Rail construction lives in the pipeline body; this module owns the `Rail` record.

use glam::Vec3;

/// In-plane offset rail for ONE directed half-edge (src→dst) on its face.
///
/// `dir` is the CONSTANT perpendicular within the face plane, pointing INTO the face (sign-fixed by the
/// face centroid). The rail is the original edge translated by `dir * width` — parallel to the edge for
/// ANY dihedral angle (the property the frozen endpoint-slide algorithm lacked). `src`/`dst` are the
/// welded rail-point indices in the order this half-edge runs.
pub(crate) struct Rail {
    pub(crate) src_idx: usize,
    pub(crate) dst_idx: usize,
    /// Base (un-offset) source position — used by the corner line-intersection solve.
    pub(crate) src_base: Vec3,
    /// In-plane inward perpendicular for this half-edge's face.
    pub(crate) dir: Vec3,
    /// Edge unit direction (src→dst).
    pub(crate) edge_dir: Vec3,
    /// This edge's ADJUSTED offset width (adjust_offsets sinratio solve) — NOT the raw spec.
    ///
    /// TWO sources feed it: the uniform width harmonised across chained edges, and (when the caller
    /// passes a `BevelVarWidth`) a per-edge target derived from that edge's own clamp capacity or
    /// authored weight. Both arrive through the same least-squares fit, so nothing downstream of
    /// here can tell them apart — which is why variable width needed no new corner math.
    pub(crate) w: f32,
}
