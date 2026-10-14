//! ADD-ONLY / IN-PLACE Euler ops wrapped as `MeshOp`s emitting an O(touched) `ElementDelta`
//! (plan §4.6, §7 Phase 2 — Gate 2a: inverse deltas ONLY, NO parent-map).
//!
//! These four ops — `split_edge`, `extrude_face`, `loop_cut`, `flip_face` — APPEND new elements at
//! the tail and overwrite only a BOUNDED set of pre-existing slots; they NEVER push to / pop from
//! the kernel's private free-list. That is exactly the precondition `ElementDelta` needs, so these
//! are the ops where the dense single-body O(touched) local-edit win is REAL (plan Risk #3). The
//! free-list-mutating ops (inset / merge / delete / dissolve) and the whole-mesh subdivide ops live
//! in `local_rebuild_ops.rs` / `subdivide.rs` and honestly use the O(body) `Snapshot` tier.
//!
//! ZERO `half_edge.rs` edits: the wrappers read the EXISTING public mutating API and capture the
//! inverse by EXTERNAL OBSERVATION of changed slots (`capture_element_delta`), exactly as plan §7
//! prescribes for the wrapper layer.

use serde::Serialize;

use crate::half_edge::{FaceId, HalfEdgeId};
use crate::mesh_ops::{
    capture_element_delta, EditResult, Handle, MeshOp, OpDelta, OpError, TopoDelta,
};
use crate::poly_mesh::PolyMesh;

/// The add-only ops keep the half-edge Vec dense iff the INPUT is dense (no free slots to reuse).
/// Vert+face density is observable through the public count API; on a dense closed manifold the
/// half-edge Vec is dense too (every interior edge has its twin, no boundary half-edge holes), so
/// truncation in `ElementDelta::apply_inverse` is byte-exact. A sparse input falls back to Snapshot
/// — declared loudly here, never silently mis-handled (No-Bandaids).
fn is_vert_face_dense(m: &crate::half_edge::HalfEdgeMesh) -> bool {
    m.vertices.len() == m.vertex_count() && m.faces.len() == m.face_count()
}

/// Run an in-place add-only op on a clone of `m.topo`, capturing the O(touched) inverse by external
/// observation. Returns the new `PolyMesh` + the `ElementDelta` inverse. The ONE clone here is the
/// "return a new mesh" contract of `MeshOp::apply` (the app hot path runs the op on the already-
/// mutable `Arc::make_mut(mesh)` and captures the delta with NO extra clone — see `mesh_actions`).
///
/// The before-image of the pre-op element Vecs is captured for the diff (external observation cost,
/// O(body) transient); the STORED delta and the undo-apply are O(touched) — the honest win.
fn run_add_only<F: FnOnce(&mut crate::half_edge::HalfEdgeMesh)>(
    m: &PolyMesh,
    run: F,
) -> Result<EditResult, OpError> {
    let mut topo = m.topo.clone();

    if !is_vert_face_dense(&topo) {
        // Sparse input → the truncate-based element delta is unsafe. Fall back to the honest
        // O(body) Snapshot tier rather than emit a wrong delta (No-Bandaids: declare the limit).
        let before = topo.clone();
        run(&mut topo);
        topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
        let mut out = m.clone();
        out.set_topo(topo);
        return Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Snapshot(before) });
    }

    // Dense path: snapshot the pre-op element Vecs (the before-image the diff reads), run in place,
    // then capture only the slots that changed + the appended-tail truncation point.
    let before_verts = topo.vertices.clone();
    let before_edges = topo.half_edges.clone();
    let before_faces = topo.faces.clone();

    run(&mut topo);

    // Guard the result — a topology violation is a real bug, never silenced (No-Bandaids).
    // validate() = twin/cycle consistency; is_manifold() = the geometric-integrity checks it is blind to
    // (non-manifold edge, degenerate/self-loop face, stale vertex.half_edge — 2026-07-07 audit #1).
    // RELATIVE: only require manifold result when the input was manifold (already-broken stays editable).
    topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
    // Owner directive 2026-08-03: edit ops gate on CORRUPTION only (integrity = degenerate
    // faces + stale disk pointers). Manifoldness (bowties, multi-fan edges) is a LEGAL
    // Blender-representable state — merging boundary verts on an open sheet creates it.
    // Full is_manifold() remains the bar only where solids are required (boolean install).
    topo.integrity()
        .map_err(|reason| OpError::InvalidResult { reason })?;

    let delta = capture_element_delta(&before_verts, &before_edges, &before_faces, &topo);

    let mut out = m.clone();
    out.set_topo(topo); // bumps topo_gen

    Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Element(delta) })
}

/// Run a NON-add-only op (rebuilds the mesh / mutates the free list) and capture the honest O(body)
/// `Snapshot` inverse. `capture_element_delta`'s truncate-based diff is unsafe for these — use this.
fn run_snapshot<F: FnOnce(&mut crate::half_edge::HalfEdgeMesh)>(
    m: &PolyMesh,
    run: F,
) -> Result<EditResult, OpError> {
    let mut topo = m.topo.clone();
    let before = topo.clone();
    run(&mut topo);
    topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
    // RELATIVE: already-broken meshes stay editable (same policy as PolyEdit::compute).
    // Owner directive 2026-08-03: edit ops gate on CORRUPTION only (integrity = degenerate
    // faces + stale disk pointers). Manifoldness (bowties, multi-fan edges) is a LEGAL
    // Blender-representable state — merging boundary verts on an open sheet creates it.
    // Full is_manifold() remains the bar only where solids are required (boolean install).
    topo.integrity()
        .map_err(|reason| OpError::InvalidResult { reason })?;
    let mut out = m.clone();
    out.set_topo(topo);
    Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Snapshot(before) })
}

// =====================================================================================
// split_edge — insert a vertex at parameter t on an edge. Pure add-only.
// =====================================================================================

/// Params for `split_edge`: the half-edge to split (by id) and the split parameter t ∈ [0,1].
#[derive(Clone, Copy, Debug, Serialize)]
pub struct SplitEdgeParams {
    pub edge: u32,
    pub t: f32,
}

pub struct SplitEdgeOp;

impl MeshOp for SplitEdgeOp {
    type Params = SplitEdgeParams;
    const PROPAGATES: bool = false; // Gate 2a — no parent-map yet.

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if !(0.0..=1.0).contains(&p.t) {
            return Err(OpError::InvalidParams { reason: "split t must be in [0,1]".into() });
        }
        if p.edge as usize >= m.topo.half_edges.len() {
            return Err(OpError::EmptySelection);
        }
        let edge = HalfEdgeId(p.edge);
        let t = p.t;
        run_add_only(m, move |topo| { topo.split_edge(edge, t); })
    }
}

// =====================================================================================
// extrude_face — push a face along an offset, building side walls. Pure add-only.
// =====================================================================================

/// Params for `extrude_face`: the face to extrude and the world-space offset vector.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ExtrudeFaceParams {
    pub face: u32,
    pub offset: [f32; 3],
}

pub struct ExtrudeFaceOp;

impl MeshOp for ExtrudeFaceOp {
    type Params = ExtrudeFaceParams;
    /// Stage A: extrude TRIVIALLY preserves UV — the top face reuses the source face's half-edge
    /// slots (their UV stays valid automatically), and the new side-wall corners Copy the source
    /// corner's UV via the op's own append-tail parent-map (plan §4.2). PROPAGATES = true.
    const PROPAGATES: bool = true;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.face as usize >= m.topo.faces.len() {
            return Err(OpError::EmptySelection);
        }
        let face = FaceId(p.face);
        let offset = glam::Vec3::from(p.offset);

        // Pre-op provenance for Copy-propagation (the op emits its OWN parent-map, §4.2): the source
        // ring + the dense tail lengths the appended verts/corners start at.
        let prev_v = m.topo.vertices.len();
        let prev_e = m.topo.half_edges.len();
        let src_ring = m.topo.face_vertices(face);

        let mut res = run_add_only(m, move |topo| { topo.extrude_face(face, offset); })?;

        // Copy the source face's authored per-corner UV onto the new side-wall corners, bounded to
        // the touched (append-tail) set — never a full-column sweep (Frame Budget §6.3). The top
        // face's corners are the same HalfEdgeId slots and keep their UV automatically.
        if !res.mesh.cd.uv.is_empty() {
            let parents = crate::mesh_ops::propagate::append_tail_parent_map(
                &res.mesh.topo, prev_v, prev_e, &src_ring);
            crate::mesh_ops::propagate::copy_propagate_corner_uv(
                &mut res.mesh.cd, &m.topo, &res.mesh.topo, face, &parents);
        }
        Ok(res)
    }
}

// =====================================================================================
// loop_cut — split a quad loop across the mesh. Internally splits edges (add-only).
// =====================================================================================

/// Params for `loop_cut`: the seed half-edge whose quad loop is cut.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct LoopCutParams {
    pub start_edge: u32,
}

pub struct LoopCutOp;

impl MeshOp for LoopCutOp {
    type Params = LoopCutParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.start_edge as usize >= m.topo.half_edges.len() {
            return Err(OpError::EmptySelection);
        }
        let start = HalfEdgeId(p.start_edge);
        // In-place ID-stable loop cut + first real TopoDelta (rail verts / sub-quad faces).
        // Capture parents outside run_add_only so ElementDelta inverse stays O(touched).
        let mut topo = m.topo.clone();
        if !is_vert_face_dense(&topo) {
            let before = topo.clone();
            let (_v, parents) = topo.loop_cut_multi_with_delta(start, &[0.5], None);
            topo.validate()
                .map_err(|reason| OpError::InvalidResult { reason })?;
            // Owner directive 2026-08-03: edit ops gate on CORRUPTION only (integrity = degenerate
            // faces + stale disk pointers). Manifoldness (bowties, multi-fan edges) is a LEGAL
            // Blender-representable state — merging boundary verts on an open sheet creates it.
            // Full is_manifold() remains the bar only where solids are required (boolean install).
            topo.integrity()
                .map_err(|reason| OpError::InvalidResult { reason })?;
            let mut out = m.clone();
            out.set_topo(topo);
            return Ok(EditResult {
                mesh: out,
                parents,
                inverse: OpDelta::Snapshot(before),
            });
        }
        let before_verts = topo.vertices.clone();
        let before_edges = topo.half_edges.clone();
        let before_faces = topo.faces.clone();
        let (_v, parents) = topo.loop_cut_multi_with_delta(start, &[0.5], None);
        topo.validate()
            .map_err(|reason| OpError::InvalidResult { reason })?;
        // Owner directive 2026-08-03: edit ops gate on CORRUPTION only (integrity = degenerate
        // faces + stale disk pointers). Manifoldness (bowties, multi-fan edges) is a LEGAL
        // Blender-representable state — merging boundary verts on an open sheet creates it.
        // Full is_manifold() remains the bar only where solids are required (boolean install).
        topo.integrity()
            .map_err(|reason| OpError::InvalidResult { reason })?;
        let delta = capture_element_delta(&before_verts, &before_edges, &before_faces, &topo);
        let mut out = m.clone();
        out.set_topo(topo);
        Ok(EditResult {
            mesh: out,
            parents,
            inverse: OpDelta::Element(delta),
        })
    }
}

// =====================================================================================
// offset_edge_loop — a parallel loop beside a selected edge loop, original KEPT. The
// selection may be a CLOSED loop or an OPEN chain with two free endpoints (plan §3.3).
// Add-only: split_edge + split_face_between only ALLOCATE, so the O(touched) tier is real.
// =====================================================================================

/// Params for `offset_edge_loop` (plan `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` §4 P1 + P2).
#[derive(Clone, Debug, Serialize)]
pub struct OffsetEdgeLoopParams {
    /// The selected loop or chain as `HalfEdgeId` u32s — the same currency the app's
    /// `poly_sel.selected_edges` stores (see `ElemDomain::HalfEdge`'s note in `mesh_ops/mod.rs`).
    pub edges: Vec<u32>,
    /// `|distance|` = absolute world offset; its SIGN picks which adjacent face the new loop lands in.
    pub distance: f32,
}

pub struct OffsetEdgeLoopOp;

impl MeshOp for OffsetEdgeLoopOp {
    type Params = OffsetEdgeLoopParams;
    /// **Declared loudly, not silently dropped** (plan §4.2 / OFFSET_EDGE_LOOP_PLAN §4 P5): the op
    /// emits no parent-map yet, so authored CustomData on the split faces/rungs does not flow to the
    /// new corners. P5 owns that, together with the `poly_export_raster` parity gate.
    const PROPAGATES: bool = false;

    /// **Inverse tier: `OpDelta::Element`** (O(touched)) on a dense input — the honest tier, not a
    /// claim. [`HalfEdgeMesh::offset_edge_loop`] mutates only through `split_edge` and
    /// `split_face_between`, both of which ALLOCATE and never free, so the kernel's private
    /// free-lists are untouched and `ElementDelta`'s truncate-the-tail inverse is byte-exact. A
    /// SPARSE input cannot use that inverse, and `run_add_only` falls back to the honest O(body)
    /// `Snapshot` there — the same declared limit every sibling op in this file carries.
    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.edges.is_empty() {
            return Err(OpError::EmptySelection);
        }
        if !p.distance.is_finite() || p.distance == 0.0 {
            return Err(OpError::InvalidParams {
                reason: "offset distance must be non-zero and finite".into(),
            });
        }
        let sel: std::collections::HashSet<HalfEdgeId> =
            p.edges.iter().map(|&e| HalfEdgeId(e)).collect();
        let distance = p.distance;

        // The kernel op is ATOMIC on refusal (every check runs before the first mutation), so its
        // reason can simply be carried out of the closure and the untouched result discarded. This
        // keeps `run_add_only` the ONE place the element-vs-snapshot tier is decided (reuse rule)
        // instead of forking a second, near-identical runner for a fallible op.
        let mut refusal: Option<String> = None;
        let res = run_add_only(m, |topo| {
            if let Err(why) = topo.offset_edge_loop(&sel, distance) {
                refusal = Some(why);
            }
        })?;
        if let Some(reason) = refusal {
            return Err(OpError::InvalidParams { reason });
        }
        Ok(res)
    }
}

// =====================================================================================
// flip_face — reverse a face's winding. In-place only (no add, no free).
// =====================================================================================

/// Params for `flip_face`: the face whose winding is reversed.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct FlipFaceParams {
    pub face: u32,
}

pub struct FlipFaceOp;

impl MeshOp for FlipFaceOp {
    type Params = FlipFaceParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.face as usize >= m.topo.faces.len() {
            return Err(OpError::EmptySelection);
        }
        let face = FaceId(p.face);
        run_add_only(m, move |topo| { topo.flip_face(face); })
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
    fn split_edge_emits_element_delta_and_bumps_gen() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let e = interior_edge(&pm.topo);
        let res = SplitEdgeOp.apply(&pm, &[], &SplitEdgeParams { edge: e, t: 0.5 }).unwrap();
        assert_eq!(res.mesh.topo.vertex_count(), 9, "split adds exactly one vertex");
        assert_eq!(res.mesh.topo_gen, 1);
        assert!(matches!(res.inverse, OpDelta::Element(_)), "split must emit an O(touched) element delta");
    }

    #[test]
    fn extrude_face_emits_element_delta() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = ExtrudeFaceOp
            .apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.5, 0.0] })
            .unwrap();
        assert!(matches!(res.inverse, OpDelta::Element(_)), "extrude must emit an element delta");
        assert!(res.mesh.topo.validate().is_ok());
    }

    /// The honest-cost gate, in code: split's inverse is an ElementDelta whose stored payload is
    /// O(touched) — NOT a whole-mesh clone. We assert the delta records only a handful of changed
    /// slots, far fewer than the full element count (so no full-mesh copy hides in the inverse).
    /// LAYER-FLOW GATE (plan §4.2 Stage A, §4.5): extrude Copy-preserves UV. Author UV on the face
    /// being extruded; after the op (1) the top face's corners keep their UV (same HalfEdgeId slots),
    /// and (2) the new side-wall corners that point at the source-ring vertices inherit those UVs —
    /// no NaN, no silent zero, bounded to the touched set.
    #[test]
    fn uv_preserved_after_extrude() {
        use glam::Vec2;
        let mut pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        // Author a distinct UV on each corner of face 0 (keyed by HalfEdgeId).
        let f0 = pm.topo.face_half_edges(FaceId(0));
        let authored: Vec<(HalfEdgeId, Vec2)> = f0.iter().enumerate()
            .map(|(i, &he)| (he, Vec2::new(0.1 * i as f32, 0.2 * i as f32))).collect();
        for &(he, uv) in &authored { pm.cd.uv.set(he, uv); }

        let res = ExtrudeFaceOp.apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.5, 0.0] }).unwrap();

        // (1) The top face reuses face 0's half-edge slots → its UV is preserved verbatim.
        for &(he, uv) in &authored {
            assert_eq!(res.mesh.cd.uv.get(he).copied(), Some(uv), "top-face corner UV must survive extrude");
        }
        // (2) New side-wall corners exist and each authored value flows to at least one new corner.
        assert!(res.mesh.cd.uv.len() > authored.len(), "extrude must Copy UV onto new side-wall corners");
        // No NaN crept in anywhere.
        for (_he, uv) in res.mesh.cd.uv.iter() {
            assert!(uv.x.is_finite() && uv.y.is_finite(), "no NaN/Inf in propagated UV");
        }
    }

    /// Empty-CustomData extrude must leave the column empty (no spurious entries) — the empty fast
    /// path that guarantees byte-identical bake to the old `to_raster_data`.
    #[test]
    fn extrude_with_empty_uv_stays_empty() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = ExtrudeFaceOp.apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.5, 0.0] }).unwrap();
        assert!(res.mesh.cd.uv.is_empty(), "extrude on an unauthored mesh must not author any UV");
    }

    // ---------------------------------------------------------------------------------
    // Offset Edge Loop (plan docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md §4 P1)
    // ---------------------------------------------------------------------------------

    /// Deep mesh equality over every element slot, built from the SAME three field comparators
    /// `capture_element_delta` diffs with (`mesh_ops/mod.rs`) — so "the inverse restored it" is
    /// judged by exactly the predicate that decided what the inverse had to record.
    fn topo_eq(a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> bool {
        use crate::mesh_ops::{halfedge_eq, hface_eq, hvertex_eq};
        a.vertices.len() == b.vertices.len()
            && a.half_edges.len() == b.half_edges.len()
            && a.faces.len() == b.faces.len()
            && a.vertices.iter().zip(&b.vertices).all(|(x, y)| hvertex_eq(x, y))
            && a.half_edges.iter().zip(&b.half_edges).all(|(x, y)| halfedge_eq(x, y))
            && a.faces.iter().zip(&b.faces).all(|(x, y)| hface_eq(x, y))
    }

    /// A cube face's boundary loop + the sign that actually has off-loop rails on the far side.
    fn cube_loop_params(pm: &PolyMesh, d: f32) -> OffsetEdgeLoopParams {
        let edges: Vec<u32> = pm.topo.face_half_edges(FaceId(0)).iter().map(|h| h.0).collect();
        for sign in [d, -d] {
            let p = OffsetEdgeLoopParams { edges: edges.clone(), distance: sign };
            if OffsetEdgeLoopOp.apply(pm, &[], &p).is_ok() {
                return p;
            }
        }
        panic!("neither side of the cube's face loop is offsettable");
    }

    /// **THE P1 WRAPPER GATE** — the `test_spine::assert_op_identity` shape, spelled out for a plain
    /// `#[test]`: (1) apply is manifold, (2) the inverse restores the pre-op mesh EXACTLY, (3)
    /// re-applying on the restored mesh reproduces the first result.
    #[test]
    fn offset_edge_loop_apply_inverse_apply_is_identity() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let p = cube_loop_params(&pm, 0.5);

        let r1 = OffsetEdgeLoopOp.apply(&pm, &[], &p).expect("offset must apply");
        assert!(r1.mesh.topo.validate().is_ok(), "first apply must be valid");
        assert!(r1.mesh.topo.is_manifold().is_ok(), "first apply must be manifold");
        assert_eq!(r1.mesh.topo.vertex_count(), 12, "4 new verts on a cube face loop");
        assert_eq!(r1.mesh.topo.face_count(), 10, "4 new quads on a cube face loop");
        assert_eq!(r1.mesh.topo_gen, 1, "a mutating op bumps topo_gen");

        // (a) the inverse restores the EXACT pre-op mesh.
        let mut restored = r1.mesh.topo.clone();
        r1.inverse.apply_inverse(&mut restored);
        assert!(topo_eq(&restored, &pm.topo), "inverse must restore the pre-op mesh exactly");

        // (b) re-applying the SAME op to the restored mesh reproduces the first result.
        let r2 = OffsetEdgeLoopOp
            .apply(&PolyMesh::new(restored), &[], &p)
            .expect("re-apply on the restored mesh must succeed (deterministic)");
        assert!(topo_eq(&r2.mesh.topo, &r1.mesh.topo),
            "apply -> inverse -> apply must reproduce the first result");
    }

    /// The inverse tier is `Element`, and it is HONEST: the stored payload is O(touched), not a
    /// whole-mesh clone hiding behind the variant name (plan §4.6 Risk #3).
    #[test]
    fn offset_edge_loop_inverse_is_element_tier_and_o_touched() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let p = cube_loop_params(&pm, 0.5);
        let res = OffsetEdgeLoopOp.apply(&pm, &[], &p).unwrap();
        match res.inverse {
            OpDelta::Element(d) => {
                assert_eq!(d.prev_v, 8, "truncation target = pre-op vertex count");
                assert_eq!(d.prev_f, 6, "truncation target = pre-op face count");
                let touched = d.changed_verts.len() + d.changed_edges.len() + d.changed_faces.len();
                let total = pm.topo.vertices.len() + pm.topo.half_edges.len() + pm.topo.faces.len();
                assert!(touched < total,
                    "element inverse must be O(touched), got {touched} of {total} pre-op slots");
            }
            OpDelta::Snapshot(_) => {
                panic!("offset_edge_loop is add-only — a dense input must take the element tier")
            }
        }
    }

    /// **THE HONEST-COST GATE.** `OpDelta::Element` claims O(touched) — bounded by the LOOP, not by
    /// the mesh. A cube cannot distinguish the two (every slot on a cube is "local"), so this runs
    /// the SAME 16-edge loop on two torus sizes (256 vs 768 faces) and pins that the stored inverse
    /// records the SAME number of slots. A delta that secretly scaled with the body would diverge.
    /// `local_rebuild_ops.rs:9` — "This is NOT a fake delta" — is the standard this has to meet.
    #[test]
    fn offset_edge_loop_inverse_scales_with_the_loop_not_the_mesh() {
        /// The live half-edge a -> b, without `find_half_edge` (which is kernel-private).
        fn he_between(m: &HalfEdgeMesh, a: u32, b: u32) -> u32 {
            (0..m.half_edges.len() as u32)
                .find(|&i| {
                    let h = HalfEdgeId(i);
                    m.is_edge_live(h) && m.he_source(h).0 == a && m.he(h).vertex.0 == b
                })
                .unwrap_or_else(|| panic!("no half-edge {a} -> {b}"))
        }
        // `new_torus` lays vertices out as `major * minor_segs + minor`, so the MINOR ring at major
        // segment 0 is simply verts 0..ns — a closed, all-valence-4 loop with real rails on BOTH
        // sides (unlike a cube face's boundary, whose inner side has no off-loop edge).
        const NS: u32 = 16;
        let run = |ms: u32| -> (usize, usize, usize) {
            let topo = HalfEdgeMesh::new_torus(1.0, 0.3, ms, NS);
            let edges: Vec<u32> = (0..NS).map(|j| he_between(&topo, j, (j + 1) % NS)).collect();
            let total = topo.vertices.len() + topo.half_edges.len() + topo.faces.len();
            let pm = PolyMesh::new(topo);
            let res = OffsetEdgeLoopOp
                .apply(&pm, &[], &OffsetEdgeLoopParams { edges, distance: 0.05 })
                .expect("a torus minor ring is offsettable on both sides");
            assert!(res.mesh.topo.validate().is_ok(), "validate: {:?}", res.mesh.topo.validate());
            assert!(res.mesh.topo.is_manifold().is_ok(), "manifold: {:?}", res.mesh.topo.is_manifold());
            assert_eq!(res.mesh.topo.vertex_count(), (ms * NS + NS) as usize, "N new verts");
            assert_eq!(res.mesh.topo.face_count(), (ms * NS + NS) as usize, "N new faces");
            let touched = match res.inverse {
                OpDelta::Element(d) => {
                    d.changed_verts.len() + d.changed_edges.len() + d.changed_faces.len()
                }
                OpDelta::Snapshot(_) => {
                    panic!("add-only op on a dense mesh must take the element tier")
                }
            };
            (touched, total, res.mesh.topo.face_count())
        };
        let (t_small, total_small, f_small) = run(16);
        let (t_big, total_big, f_big) = run(48);
        assert!(f_big > f_small * 2, "the second fixture must actually be bigger ({f_small} vs {f_big})");
        assert_eq!(t_small, t_big,
            "the element inverse must not grow with the mesh: {t_small} slots on {total_small} \
             vs {t_big} on {total_big}");
        assert!(t_big * 4 < total_big,
            "and it must stay a small fraction of the body: {t_big} of {total_big}");
    }

    /// A refusal is a clean `OpError`, and the caller's mesh is never handed a half-built loop.
    #[test]
    fn offset_edge_loop_refusals_are_clean() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let edges: Vec<u32> = pm.topo.face_half_edges(FaceId(0)).iter().map(|h| h.0).collect();

        assert!(matches!(
            OffsetEdgeLoopOp.apply(&pm, &[], &OffsetEdgeLoopParams { edges: vec![], distance: 0.5 }),
            Err(OpError::EmptySelection)
        ));
        assert!(matches!(
            OffsetEdgeLoopOp.apply(&pm, &[], &OffsetEdgeLoopParams { edges: edges.clone(), distance: 0.0 }),
            Err(OpError::InvalidParams { .. })
        ));
        // An open chain (2 of the 4 loop edges) is P2 — refused, with a reason.
        assert!(matches!(
            OffsetEdgeLoopOp.apply(&pm, &[], &OffsetEdgeLoopParams {
                edges: edges[..2].to_vec(), distance: 0.5 }),
            Err(OpError::InvalidParams { .. })
        ));
    }

    #[test]
    fn split_inverse_is_o_touched_not_whole_mesh() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let e = interior_edge(&pm.topo);
        let res = SplitEdgeOp.apply(&pm, &[], &SplitEdgeParams { edge: e, t: 0.5 }).unwrap();
        match res.inverse {
            OpDelta::Element(d) => {
                // split touches its edge + twin chain + the dst-vertex back-pointer: a small set.
                let touched = d.changed_verts.len() + d.changed_edges.len() + d.changed_faces.len();
                assert!(touched <= 8, "split inverse must be O(touched) (got {touched} changed slots)");
                // The appended tail is reclaimed by truncation, not stored as a clone.
                assert_eq!(d.prev_v, 8, "truncation target = pre-op vertex count");
            }
            OpDelta::Snapshot(_) => panic!("split on a dense mesh must use the element-delta path"),
        }
    }
}
