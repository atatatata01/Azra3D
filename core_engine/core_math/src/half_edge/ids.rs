// Typed index ids + sentinels for the half-edge mesh (split from half_edge.rs — pure relocation).

use serde::{Serialize, Deserialize};

// ============================================================
// Typed index types
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct VertexId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct HalfEdgeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FaceId(pub u32);

/// Sentinel: no twin (boundary edge).
pub const NO_TWIN: HalfEdgeId = HalfEdgeId(u32::MAX);
/// Sentinel: no face (boundary half-edge).
pub const NO_FACE: FaceId = FaceId(u32::MAX);
/// Sentinel: no half-edge.
pub const NO_EDGE: HalfEdgeId = HalfEdgeId(u32::MAX);
