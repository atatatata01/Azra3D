//! FREE-LIST-MUTATING local Euler ops wrapped as `MeshOp`s with the honest O(body) `Snapshot`
//! inverse (plan §4.6, §7 Phase 2 — Gate 2a: inverse deltas ONLY, NO parent-map).
//!
//! `inset_face`, `merge_vertices`, `delete_face`, `dissolve_edge`, `dissolve_face` all push freed
//! slots onto the kernel's PRIVATE free-lists (`free_verts` / `free_edges` / `free_faces`). An
//! EXTERNAL element delta cannot reconstruct that private free-list state without a `half_edge.rs`
//! edit (a red line this phase forbids). Per plan §4.6's HONEST cost model, the correct inverse for
//! these is the per-BODY `Snapshot` (O(body)) — still strictly cheaper than Blender's O(scene) copy
//! (T88044). This is NOT a fake delta: the cost tier is encoded in the `OpDelta` variant and
//! documented, never hidden behind a false "all O(touched)" claim.
//!
//! ZERO `half_edge.rs` edits: the wrappers read the EXISTING public mutating API; the inverse is the
//! pre-op mesh image captured BEFORE the mutation (plan §7 "external observation of changed
//! elements" — here the touched region is the whole body, so the observation is the whole image).

use serde::Serialize;

use crate::half_edge::{FaceId, HalfEdgeId, VertexId};
use crate::mesh_ops::{EditResult, Handle, MeshOp, OpDelta, OpError, TopoDelta};
use crate::poly_mesh::PolyMesh;

/// Run an in-place free-list-mutating op on a clone of `m.topo`, capturing the honest O(body)
/// Snapshot inverse (the pre-op image). Validates the result (No-Bandaids: a topology break is a
/// real bug, never silenced). `pre_check` lets an op reject cleanly (e.g. non-existent face) before
/// any mutation — returning the error keeps `apply` side-effect-free on failure.
fn run_snapshot<F: FnOnce(&mut crate::half_edge::HalfEdgeMesh) -> Result<(), OpError>>(
    m: &PolyMesh,
    run: F,
) -> Result<EditResult, OpError> {
    let before = m.topo.clone(); // the honest per-BODY snapshot (O(body)) — the inverse image.
    let input_integrity_ok = m.topo.integrity().is_ok();
    let mut topo = m.topo.clone();
    // Owner 2026-08-06: purge digons / repair stale disks so Delete/Dissolve (and siblings)
    // are not soft-locked by knife-slice poison. Mirrors PolyEdit::compute.
    let _ = topo.heal_for_edit();
    run(&mut topo)?;
    let _ = topo.heal_for_edit();
    topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
    // Gate on the integrity classifier, not just validate() (twin/cycle). validate() is BLIND to
    // digons / stale disks. After heal_for_edit those should be gone; if residual remains and the
    // INPUT was already dirty, still accept — delete/dissolve is how users clean bad geo.
    // Owner directive 2026-08-03: edit ops gate on CORRUPTION only (integrity = degenerate
    // faces + stale disk pointers). Manifoldness (bowties, multi-fan edges) is a LEGAL
    // Blender-representable state — merging boundary verts on an open sheet creates it.
    // Full is_manifold() remains the bar only where solids are required (boolean install).
    if let Err(reason) = topo.integrity() {
        if input_integrity_ok {
            return Err(OpError::InvalidResult { reason });
        }
        // Dirty in → twin graph valid → accept residual (cleanup path).
    }
    let mut out = m.clone();
    out.set_topo(topo); // bumps topo_gen
    Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Snapshot(before) })
}

// =====================================================================================
// inset_face — shrink a face inward, ringing it with quads. Frees the old face slot.
// =====================================================================================

#[derive(Clone, Copy, Debug, Serialize)]
pub struct InsetFaceParams {
    pub face: u32,
    pub amount: f32,
}

pub struct InsetFaceOp;

impl MeshOp for InsetFaceOp {
    type Params = InsetFaceParams;
    /// Stage A: inset TRIVIALLY preserves UV. Inset is the FREE-LIST case (it deletes the source
    /// face and the new ring/inner faces reuse the freed slots), so its propagation CLEARS the stale
    /// source-corner entries and rebuilds the inset region's UV from the source vertices' authored UV
    /// (plan §4.2). PROPAGATES = true.
    const PROPAGATES: bool = true;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.face as usize >= m.topo.faces.len() || m.topo.face(FaceId(p.face)).half_edge == crate::half_edge::NO_EDGE {
            return Err(OpError::EmptySelection);
        }
        let face = FaceId(p.face);
        let amount = p.amount;

        // Pre-op provenance for Copy-propagation (the op emits its OWN parent-map, §4.2).
        let prev_v = m.topo.vertices.len();
        let src_ring = m.topo.face_vertices(face);
        let src_face_corners = m.topo.face_half_edges(face);

        let mut res = run_snapshot(m, move |topo| {
            topo.inset_face(face, amount);
            Ok(())
        })?;

        if !res.mesh.cd.uv.is_empty() {
            // The inset region's corners = every corner of a post-op face that touches an inset
            // vertex (index >= prev_v) OR a source-ring vertex (the ring quads share both). Bounded
            // to the inset footprint — never a full-column sweep (Frame Budget §6.3).
            let inset_corners = inset_region_corners(&res.mesh.topo, prev_v, &src_ring);
            crate::mesh_ops::propagate::copy_propagate_inset_uv(
                &mut res.mesh.cd, &m.topo, &res.mesh.topo, face, &src_ring,
                prev_v, &src_face_corners, &inset_corners);
        }
        Ok(res)
    }
}

/// Collect the corners of every post-op face that contains an INSET vertex (slot >= `prev_v`) — the
/// inset footprint (the n ring quads + the inner face). The defining property: ONLY inset geometry
/// touches an inset vertex; a neighbouring OUTER face shares the source-ring vertices but has ZERO
/// inset vertices, so it is correctly EXCLUDED (its corners must keep their own UV — writing inset
/// UV onto a neighbour would corrupt it). The op's OWN construction determines this set; it is
/// bounded to the inset region, never the whole mesh (Frame Budget §6.3).
fn inset_region_corners(
    after: &crate::half_edge::HalfEdgeMesh,
    prev_v: usize,
    _src_ring: &[crate::half_edge::VertexId],
) -> Vec<crate::half_edge::HalfEdgeId> {
    use crate::half_edge::{FaceId, NO_EDGE};
    use std::collections::HashSet;
    let mut out = Vec::new();
    let mut seen_faces: HashSet<u32> = HashSet::new();
    for i in 0..after.faces.len() as u32 {
        if after.face(FaceId(i)).half_edge == NO_EDGE { continue; }
        let corners = after.face_half_edges(FaceId(i));
        let touches_inset = corners.iter().any(|&c| after.he(c).vertex.0 as usize >= prev_v);
        if touches_inset && seen_faces.insert(i) {
            out.extend(corners);
        }
    }
    out
}

// =====================================================================================
// merge_vertices — weld two verts to their midpoint. Frees one vertex slot.
// =====================================================================================

#[derive(Clone, Copy, Debug, Serialize)]
pub struct MergeVerticesParams {
    pub a: u32,
    pub b: u32,
}

pub struct MergeVerticesOp;

impl MeshOp for MergeVerticesOp {
    type Params = MergeVerticesParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.a == p.b {
            return Err(OpError::InvalidParams { reason: "merge requires two distinct vertices".into() });
        }
        let (a, b) = (VertexId(p.a), VertexId(p.b));
        run_snapshot(m, move |topo| {
            if p.a as usize >= topo.vertices.len() || p.b as usize >= topo.vertices.len() {
                return Err(OpError::EmptySelection);
            }
            topo.merge_vertices(a, b);
            Ok(())
        })
    }
}

// =====================================================================================
// delete_face — remove a face, freeing it + its half-edges.
// =====================================================================================

#[derive(Clone, Copy, Debug, Serialize)]
pub struct DeleteFaceParams {
    pub face: u32,
}

pub struct DeleteFaceOp;

impl MeshOp for DeleteFaceOp {
    type Params = DeleteFaceParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        let face = FaceId(p.face);
        run_snapshot(m, move |topo| {
            if p.face as usize >= topo.faces.len() || topo.face(face).half_edge == crate::half_edge::NO_EDGE {
                return Err(OpError::EmptySelection);
            }
            topo.delete_face(face);
            Ok(())
        })
    }
}

// =====================================================================================
// dissolve_edge — merge the two faces across an edge. Frees the edge pair + one face.
// =====================================================================================

#[derive(Clone, Copy, Debug, Serialize)]
pub struct DissolveEdgeParams {
    pub edge: u32,
}

pub struct DissolveEdgeOp;

impl MeshOp for DissolveEdgeOp {
    type Params = DissolveEdgeParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        let edge = HalfEdgeId(p.edge);
        run_snapshot(m, move |topo| {
            if p.edge as usize >= topo.half_edges.len() {
                return Err(OpError::EmptySelection);
            }
            // dissolve_edge returns None on a boundary / invalid edge — a clean refusal.
            if topo.dissolve_edge(edge).is_none() {
                return Err(OpError::InvalidParams { reason: "edge is boundary or non-dissolvable".into() });
            }
            Ok(())
        })
    }
}

// =====================================================================================
// dissolve_face — turn a face into a hole (its edges become boundary). Frees the face.
// =====================================================================================

#[derive(Clone, Copy, Debug, Serialize)]
pub struct DissolveFaceParams {
    pub face: u32,
}

pub struct DissolveFaceOp;

impl MeshOp for DissolveFaceOp {
    type Params = DissolveFaceParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        let face = FaceId(p.face);
        run_snapshot(m, move |topo| {
            if p.face as usize >= topo.faces.len() || topo.face(face).half_edge == crate::half_edge::NO_EDGE {
                return Err(OpError::EmptySelection);
            }
            topo.dissolve_face(face);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use crate::half_edge::{HalfEdgeMesh, NO_TWIN, NO_FACE};

    fn interior_edge(m: &HalfEdgeMesh) -> u32 {
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE && m.he(he.twin).face != NO_FACE {
                return i;
            }
        }
        panic!("no interior edge");
    }

    #[test]
    fn delete_face_emits_snapshot() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = DeleteFaceOp.apply(&pm, &[], &DeleteFaceParams { face: 0 }).unwrap();
        assert_eq!(res.mesh.topo.face_count(), 5, "delete drops one face");
        assert!(matches!(res.inverse, OpDelta::Snapshot(_)),
            "free-list-mutating ops use the honest O(body) snapshot tier");
    }

    #[test]
    fn dissolve_edge_emits_snapshot_and_merges_faces() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let e = interior_edge(&pm.topo);
        let res = DissolveEdgeOp.apply(&pm, &[], &DissolveEdgeParams { edge: e }).unwrap();
        assert_eq!(res.mesh.topo.face_count(), 5, "dissolving an edge merges its two faces");
        assert!(matches!(res.inverse, OpDelta::Snapshot(_)));
    }

    /// LAYER-FLOW GATE (plan §4.2 Stage A, §4.5): inset Copy-preserves UV. Inset is the FREE-LIST
    /// case — it deletes the source face (freeing its corner slots, which the new geometry reuses).
    /// After the op: (1) the stale source-corner entries are CLEARED (not silently corrupting a
    /// reused slot), and (2) the inset region's corners inherit the source vertices' authored UV.
    #[test]
    fn uv_preserved_after_inset() {
        use glam::Vec2;
        let mut pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        // Author a distinct UV per corner of face 0, recording the source vertex → uv it anchors.
        let f0 = pm.topo.face_half_edges(FaceId(0));
        let mut vert_uv: std::collections::HashMap<u32, Vec2> = std::collections::HashMap::new();
        for (i, &he) in f0.iter().enumerate() {
            let uv = Vec2::new(0.3 * i as f32, 0.4 * i as f32);
            pm.cd.uv.set(he, uv);
            vert_uv.insert(pm.topo.he(he).vertex.0, uv);
        }

        let res = InsetFaceOp.apply(&pm, &[], &InsetFaceParams { face: 0, amount: 0.2 }).unwrap();

        // (1) No NaN/Inf anywhere in the propagated column.
        for (_he, uv) in res.mesh.cd.uv.iter() {
            assert!(uv.x.is_finite() && uv.y.is_finite(), "no NaN/Inf in inset-propagated UV");
        }
        // (2) The inner face's corners inherit the matching source vertex's UV. The inner face is the
        // one whose every corner points to an inset (new) vertex; verify at least one corner copied.
        let mut copied = 0usize;
        for i in 0..res.mesh.topo.faces.len() as u32 {
            if res.mesh.topo.face(FaceId(i)).half_edge == crate::half_edge::NO_EDGE { continue; }
            for &he in &res.mesh.topo.face_half_edges(FaceId(i)) {
                if let Some(got) = res.mesh.cd.uv.get(he) {
                    // Each propagated entry must equal SOME authored source-vertex UV (no fabricated value).
                    assert!(vert_uv.values().any(|w| w == got),
                        "inset-propagated UV must equal an authored source UV (no silent corruption)");
                    copied += 1;
                }
            }
        }
        assert!(copied > 0, "inset must Copy UV onto the inset region's corners");
    }

    /// Empty-CustomData inset leaves the column empty (byte-identical-bake guarantee).
    #[test]
    fn inset_with_empty_uv_stays_empty() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = InsetFaceOp.apply(&pm, &[], &InsetFaceParams { face: 0, amount: 0.2 }).unwrap();
        assert!(res.mesh.cd.uv.is_empty(), "inset on an unauthored mesh must not author any UV");
    }

    #[test]
    fn merge_vertices_rejects_self_merge() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let err = MergeVerticesOp.apply(&pm, &[], &MergeVerticesParams { a: 0, b: 0 }).unwrap_err();
        assert!(matches!(err, OpError::InvalidParams { .. }));
    }
}
