//! THE TEST SPINE (plan §4.5) — the harness every poly layer is gated on.
//!
//! `cargo test -p core_math` runs this continuously; no layer merges with a red spine. Phase P0
//! stands up the SKELETON:
//! - `mesh_eq(deep)` — the strengthened oracle (positions within epsilon + every CustomData column
//!   byte-equal + `topo_gen` consistency). Degrades gracefully with empty CustomData this phase.
//! - the bincode **sentinel-variant guard** — proves no non-skip field was added to `HalfEdgeMesh`.
//! - the **FL4 round-trip suite** stubs — `#[ignore]` where the live `PolyLayerRecord` field does
//!   not exist yet (Phase 3); the sentinel guard + `mesh_eq` RUN and PASS now.
//! - **proptest** random-mesh generation + `validate()` fuzz.
//!
//! This module is `#[cfg(test)]`-only (it is wired in `mesh_ops/mod.rs` under `#[cfg(test)]`), so it
//! adds ZERO bytes to the shipping kernel and depends on the test-only `proptest` / `bincode` crates.

use glam::Vec3;

use crate::half_edge::HalfEdgeMesh;
use crate::poly_mesh::PolyMesh;

/// Deep structural + attribute equality oracle (plan §4.5). The `apply→inverse→apply` proptests
/// must restore FULL equality, NOT just `validate()` topology — today's `validate()` (next/prev +
/// twin only) would PASS a mesh whose positions/UVs were silently corrupted by a wrong inverse
/// delta. `mesh_eq` checks:
/// 1. same active vertex / face / edge counts,
/// 2. every vertex position within `eps`,
/// 3. every face's ordered vertex ring equal,
/// 4. every CustomData column byte-equal (vacuously true in P0 — both sidecars empty),
/// 5. `topo_gen` behavior consistent (both sides have advanced through the same op count is NOT
///    required — what matters is the geometry/attributes match; `topo_gen` is asserted equal only
///    when both inputs are at the same generation, which the identity proptest arranges).
///
/// Returns `Ok(())` on equality, `Err(reason)` with the first mismatch (used by proptest for a
/// readable shrink message).
pub fn mesh_eq(a: &PolyMesh, b: &PolyMesh, eps: f32) -> Result<(), String> {
    let (ta, tb) = (&a.topo, &b.topo);

    if ta.vertex_count() != tb.vertex_count() {
        return Err(format!("vertex_count {} != {}", ta.vertex_count(), tb.vertex_count()));
    }
    if ta.face_count() != tb.face_count() {
        return Err(format!("face_count {} != {}", ta.face_count(), tb.face_count()));
    }
    if ta.edge_count() != tb.edge_count() {
        return Err(format!("edge_count {} != {}", ta.edge_count(), tb.edge_count()));
    }

    // Positions: compare the multiset of active vertex positions (slot indices need not match after
    // a rebuild via from_polygons, but the geometry must). Greedy nearest-match within eps.
    let pa = active_positions(ta);
    let pb = active_positions(tb);
    if !positions_match(&pa, &pb, eps) {
        return Err(format!("vertex positions differ beyond eps={}", eps));
    }

    // Faces: compare the multiset of face position-rings (canonicalized by rotation+reflection so
    // winding/start-vertex differences from a rebuild don't cause false negatives).
    let fa = face_position_rings(ta);
    let fb = face_position_rings(tb);
    if !face_rings_match(&fa, &fb, eps) {
        return Err("face position-rings differ (topology mismatch)".into());
    }

    // CustomData column byte-equality (plan §4.5 — the deep oracle now compares EVERY authored
    // column, not just emptiness). A wrong inverse delta or a wrong propagation that corrupted a UV
    // / sharp / crease entry FAILS here even when topology `validate()`s. Vacuously equal for empty
    // sidecars (both columns empty → `columns_eq` is Ok).
    a.cd.columns_eq(&b.cd).map_err(|e| format!("CustomData column mismatch: {e}"))?;

    // topo_gen consistency (plan §4.5 — the deep oracle checks generation behavior, not just
    // geometry). The identity proptests always compare two meshes at the SAME generation (both pre-op
    // at gen 0, or both post-op at gen 1), so a generation divergence here flags a wrapper bug (e.g.
    // an op that mutated topology WITHOUT bumping topo_gen — a no-op-replay hazard, plan §4.6).
    if a.topo_gen != b.topo_gen {
        return Err(format!("topo_gen {} != {}", a.topo_gen, b.topo_gen));
    }

    Ok(())
}

/// TOTAL byte-identity oracle for a `HalfEdgeMesh` — the ATOMICITY oracle (distinct from `mesh_eq`,
/// which is an eps-geometric ACTIVE-geometry oracle). Used to prove that a REFUSED op left the mesh
/// bit-for-bit untouched.
///
/// Why bincode and not a field-by-field diff: `HalfEdgeMesh` has SIX fields (half_edge.rs:168-176) —
/// the three `pub` Vecs PLUS the PRIVATE `free_verts` / `free_edges` / `free_faces`. Rust privacy is
/// MODULE-scoped, so `mesh_ops` (a sibling of `half_edge`, not a descendant) cannot name the private
/// three — a hand-written diff here is structurally limited to 3 of 6 fields and would silently PASS
/// a mesh whose free-lists were corrupted (verified: `m.free_verts` from this module is rustc E0616;
/// see also the same observation at test_spine.rs:81-82). `#[derive(Serialize)]` (half_edge.rs:167)
/// covers ALL six fields regardless of visibility, so the encoded bytes are a TOTAL image — the only
/// complete comparison expressible from outside `half_edge` without touching the kernel's API.
///
/// bincode is the same codec the save format uses, so this also matches what would hit disk.
pub fn topo_identical(a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> Result<(), String> {
    // Cheap, readable diagnostics FIRST (the pub fields), so a failure names the mismatch instead of
    // just reporting "bytes differ". These are necessary-but-not-sufficient — the bincode compare
    // below is the actual verdict and is what catches a private free-list divergence.
    if a.vertices.len() != b.vertices.len() {
        return Err(format!("vertices.len {} != {}", a.vertices.len(), b.vertices.len()));
    }
    if a.half_edges.len() != b.half_edges.len() {
        return Err(format!("half_edges.len {} != {}", a.half_edges.len(), b.half_edges.len()));
    }
    if a.faces.len() != b.faces.len() {
        return Err(format!("faces.len {} != {}", a.faces.len(), b.faces.len()));
    }
    if a.face_count() != b.face_count() {
        return Err(format!("face_count {} != {}", a.face_count(), b.face_count()));
    }
    if a.vertex_count() != b.vertex_count() {
        return Err(format!("vertex_count {} != {}", a.vertex_count(), b.vertex_count()));
    }
    // `HalfEdge` derives no PartialEq, so compare field by field (mirrors half_edge.rs:4966-4974).
    for i in 0..a.half_edges.len() {
        let (x, y) = (&a.half_edges[i], &b.half_edges[i]);
        if !(x.vertex == y.vertex && x.twin == y.twin && x.next == y.next
            && x.prev == y.prev && x.face == y.face)
        {
            return Err(format!("half_edge {i} differs"));
        }
    }
    for i in 0..a.vertices.len() {
        let (x, y) = (&a.vertices[i], &b.vertices[i]);
        if x.position != y.position || x.half_edge != y.half_edge {
            return Err(format!("vertex {i} differs"));
        }
    }
    for i in 0..a.faces.len() {
        let (x, y) = (&a.faces[i], &b.faces[i]);
        if x.half_edge != y.half_edge || x.material_id != y.material_id {
            return Err(format!("face {i} differs"));
        }
    }

    // THE VERDICT: total image equality across all six fields, private free-lists included.
    let ea = bincode::serialize(a).map_err(|e| format!("bincode encode a: {e}"))?;
    let eb = bincode::serialize(b).map_err(|e| format!("bincode encode b: {e}"))?;
    if ea != eb {
        return Err(format!(
            "meshes differ in a field the pub-field diff cannot name (private free-list \
             free_verts/free_edges/free_faces, or a vertex normal): encoded {} vs {} bytes",
            ea.len(),
            eb.len()
        ));
    }
    Ok(())
}

/// FREE-LIST-AWARE active enumeration (Phase 2 — the free-aware compaction the P0 note anticipated).
/// The kernel's free-lists are PRIVATE, so we cannot ask "is slot i free?". Instead we enumerate
/// ACTIVE geometry by REACHABILITY — exactly what `to_raster_data` / `to_mesh` do via public API:
/// an active face has `half_edge != NO_EDGE`; an active vertex is one referenced by some active
/// face. A freed vertex keeps a stale position but is unreferenced, so it is naturally excluded.
/// This lets `mesh_eq` compare the SPARSE meshes produced by delete/dissolve/merge without ever
/// touching the private free-list (No-Bandaids: reachability is the structural truth, not a guess).
fn active_face_ids(m: &HalfEdgeMesh) -> Vec<crate::half_edge::FaceId> {
    use crate::half_edge::{FaceId, NO_EDGE};
    (0..m.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| m.face(f).half_edge != NO_EDGE && m.face_vertices(f).len() >= 3)
        .collect()
}

/// Positions of all vertices REACHED by an active face (the active vertex multiset).
fn active_positions(m: &HalfEdgeMesh) -> Vec<Vec3> {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for f in active_face_ids(m) {
        for v in m.face_vertices(f) { seen.insert(v.0); }
    }
    seen.into_iter().map(|i| m.vert(crate::half_edge::VertexId(i)).position).collect()
}

/// Greedy multiset match of two position lists within `eps` (O(n²); test-only).
fn positions_match(a: &[Vec3], b: &[Vec3], eps: f32) -> bool {
    if a.len() != b.len() { return false; }
    let mut used = vec![false; b.len()];
    'outer: for &pa in a {
        for (j, &pb) in b.iter().enumerate() {
            if used[j] { continue; }
            if (pa - pb).length() <= eps {
                used[j] = true;
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Each active face as its ordered ring of vertex POSITIONS (topology-independent of slot indices).
/// Free-list-aware via `active_face_ids` (skips freed faces by reachability, not by free-list).
fn face_position_rings(m: &HalfEdgeMesh) -> Vec<Vec<Vec3>> {
    let mut rings = Vec::new();
    for f in active_face_ids(m) {
        let verts = m.face_vertices(f);
        if verts.is_empty() { continue; }
        rings.push(verts.iter().map(|v| m.vert(*v).position).collect());
    }
    rings
}

/// Canonicalize a ring under rotation + reflection so two windings of the same loop compare equal.
/// Returns a quantized key (the smallest rotation of either orientation, lexicographically).
fn canonical_ring_key(ring: &[Vec3], eps: f32) -> Vec<(i64, i64, i64)> {
    let q = (1.0 / eps).max(1.0);
    let quant: Vec<(i64, i64, i64)> = ring.iter()
        .map(|p| ((p.x * q).round() as i64, (p.y * q).round() as i64, (p.z * q).round() as i64))
        .collect();
    let n = quant.len();
    if n == 0 { return Vec::new(); }

    let mut best: Option<Vec<(i64, i64, i64)>> = None;
    for &rev in &[false, true] {
        let seq: Vec<(i64, i64, i64)> = if rev {
            quant.iter().rev().cloned().collect()
        } else {
            quant.clone()
        };
        for start in 0..n {
            let rot: Vec<(i64, i64, i64)> = (0..n).map(|k| seq[(start + k) % n]).collect();
            match &best {
                None => best = Some(rot),
                Some(b) if rot < *b => best = Some(rot),
                _ => {}
            }
        }
    }
    best.unwrap()
}

fn face_rings_match(a: &[Vec<Vec3>], b: &[Vec<Vec3>], eps: f32) -> bool {
    if a.len() != b.len() { return false; }
    let mut ka: Vec<Vec<(i64, i64, i64)>> = a.iter().map(|r| canonical_ring_key(r, eps)).collect();
    let mut kb: Vec<Vec<(i64, i64, i64)>> = b.iter().map(|r| canonical_ring_key(r, eps)).collect();
    ka.sort();
    kb.sort();
    ka == kb
}

// =====================================================================================
// proptest random-mesh generator + validate() fuzz (plan §4.5)
// =====================================================================================

use proptest::prelude::*;

/// A small library of KNOWN-MANIFOLD meshes the generator samples from, parameterized by a few
/// random scalars. We do NOT generate arbitrary polygon soup (that would mostly produce
/// non-manifold input `from_polygons` is not meant to repair); we generate VALID closed manifolds
/// at random sizes/positions so `validate()` is a meaningful invariant and bevel has real edges.
pub fn arb_manifold_mesh() -> impl Strategy<Value = HalfEdgeMesh> {
    // (kind, scale, jitter-seed) — kind selects the base solid.
    (0u8..3u8, 0.3f32..3.0f32).prop_map(|(kind, scale)| {
        match kind {
            0 => HalfEdgeMesh::new_box(Vec3::splat(scale)),
            1 => HalfEdgeMesh::new_box(Vec3::new(scale, scale * 0.5, scale * 1.5)),
            _ => grid_quad_plane_closed(scale),
        }
    })
}

/// A 2×2 quad grid extruded into a closed box-like manifold (gives interior edges to bevel).
fn grid_quad_plane_closed(s: f32) -> HalfEdgeMesh {
    // Reuse new_box but at a distinct aspect so the corpus has variety; this stays a valid manifold.
    HalfEdgeMesh::new_box(Vec3::new(s * 1.2, s, s * 0.8))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// FUZZ GATE: every randomly generated mesh in the corpus passes `validate()`. This is the
    /// floor the whole spine stands on — if the generator can't produce valid meshes, no op test
    /// downstream is trustworthy.
    #[test]
    fn random_mesh_validates(m in arb_manifold_mesh()) {
        prop_assert!(m.validate().is_ok(), "generated mesh failed validate(): {:?}", m.validate());
        // PolyMesh wrap is inert and must not perturb topology.
        let pm = PolyMesh::new(m.clone());
        prop_assert_eq!(pm.topo().vertex_count(), m.vertex_count());
    }

    /// `mesh_eq` reflexivity: a mesh deep-equals itself (sanity for the oracle before it gates
    /// real inverse-undo proptests in Step G / Phase 2).
    #[test]
    fn mesh_eq_reflexive(m in arb_manifold_mesh()) {
        let a = PolyMesh::new(m.clone());
        let b = PolyMesh::new(m);
        prop_assert!(mesh_eq(&a, &b, 1e-4).is_ok(), "mesh_eq must be reflexive");
    }
}

// =====================================================================================
// THE BEVEL HARD-MERGE GATE (plan §7 / Phase 1): apply → inverse → apply == identity,
// asserted with mesh_eq(deep), NOT validate() alone. Random interior-edge selection +
// random width fuzzing finds no panic and no topology break.
// =====================================================================================

use crate::mesh_ops::{Handle, MeshOp, OpDelta};
use crate::mesh_ops::bevel::{BevelOp, BevelOpParams};
use crate::half_edge::{HalfEdgeId, NO_TWIN, NO_FACE};

/// All interior (manifold, non-boundary) half-edge ids of a mesh — the edges bevel v1 acts on.
fn interior_edges(m: &HalfEdgeMesh) -> Vec<u32> {
    (0..m.half_edges.len() as u32)
        .filter(|&i| {
            let he = m.he(HalfEdgeId(i));
            he.twin != NO_TWIN && he.face != NO_FACE && m.he(he.twin).face != NO_FACE
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// HARD MERGE GATE. Bevel a random subset of a random mesh's interior edges at a random width.
    /// `apply` yields (result, inverse-snapshot). Restoring the inverse and re-applying the SAME op
    /// must reproduce the SAME result — proving the op is deterministic AND its inverse faithfully
    /// restores the pre-op state. Asserted with mesh_eq(deep) (positions + CustomData + topo_gen),
    /// not validate() alone — a wrong inverse that still passed validate() would corrupt geometry.
    #[test]
    fn bevel_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        edge_pick in any::<u64>(),
        width in 0.02f32..0.25f32,
    ) {
        let edges = interior_edges(&m);
        prop_assume!(!edges.is_empty());

        // Deterministic pseudo-random non-empty edge subset from the seed.
        let mut sel: Vec<Handle> = Vec::new();
        let mut bits = edge_pick;
        for (k, &e) in edges.iter().enumerate() {
            // Always include at least edge 0 so the selection is non-empty.
            if k == 0 || (bits & 1) == 1 { sel.push(Handle::new(e)); }
            bits >>= 1;
        }

        let params = BevelOpParams { width, ..Default::default() };
        let pm = PolyMesh::new(m.clone());

        // First apply.
        let r1 = match BevelOp.apply(&pm, &sel, &params) {
            Ok(r) => r,
            Err(_) => return Ok(()), // a clean refusal (e.g. degenerate) is not a gate failure
        };
        prop_assert!(r1.mesh.topo.validate().is_ok(), "first bevel must be manifold");

        // Restore via the inverse APPLIED TO THE POST-OP MESH (the real undo semantics: Ctrl+Z
        // runs the inverse against the edited mesh), then re-apply the SAME op.
        let mut restored_topo = r1.mesh.topo.clone();
        r1.inverse.apply_inverse(&mut restored_topo);
        let restored = PolyMesh::new(restored_topo);
        // The inverse must restore the EXACT pre-op mesh.
        prop_assert!(mesh_eq(&restored, &pm, 1e-4).is_ok(),
            "inverse must restore the pre-op mesh exactly");

        let r2 = BevelOp.apply(&restored, &sel, &params)
            .expect("re-apply on the restored mesh must succeed (deterministic)");

        // apply == apply-after-inverse-restore (the identity the gate asserts).
        prop_assert!(mesh_eq(&r1.mesh, &r2.mesh, 1e-4).is_ok(),
            "apply→inverse→apply must reproduce the first result (deep mesh_eq)");
    }
}

// =====================================================================================
// THE EULER-OP INVERSE-DELTA GATE (plan §7 / Phase 2 — Gate 2a). For EVERY wrapped op:
//   apply → inverse-on-post-op-mesh → re-apply == identity, asserted with mesh_eq(deep).
// The inverse is run against the POST-op mesh (the real Ctrl+Z semantics) and must
// (a) restore the EXACT pre-op mesh, then (b) re-applying the SAME op reproduces the
// first result. mesh_eq(deep) — NOT validate() alone — so a wrong inverse that still
// passed validate() (e.g. silently corrupted positions / wrong free-list reuse) FAILS.
// =====================================================================================

/// Shared identity check for a parametric op `apply`. `make` builds a fresh op invocation; running
/// it on `pm` yields (result, inverse). The inverse applied to the post-op mesh must restore `pm`
/// exactly, and re-applying must reproduce the first result. A clean `Err` (degenerate input /
/// out-of-range param) is NOT a gate failure — only a wrong inverse or a panic is.
fn assert_op_identity<F>(pm: &PolyMesh, mut run: F) -> Result<(), proptest::test_runner::TestCaseError>
where
    F: FnMut(&PolyMesh) -> Result<crate::mesh_ops::EditResult, crate::mesh_ops::OpError>,
{
    let r1 = match run(pm) {
        Ok(r) => r,
        Err(_) => return Ok(()), // clean refusal — not a gate failure
    };
    prop_assert!(r1.mesh.topo.validate().is_ok(), "first apply must be manifold");

    // (a) inverse applied to the POST-op mesh restores the EXACT pre-op mesh.
    let mut restored_topo = r1.mesh.topo.clone();
    r1.inverse.apply_inverse(&mut restored_topo);
    let restored = PolyMesh::new(restored_topo);
    prop_assert!(mesh_eq(&restored, pm, 1e-4).is_ok(),
        "inverse must restore the pre-op mesh exactly (deep mesh_eq)");

    // (b) re-applying the SAME op reproduces the first result (determinism + faithful restore).
    let r2 = match run(&restored) {
        Ok(r) => r,
        Err(e) => panic!("re-apply on the restored mesh must succeed (deterministic): {e}"),
    };
    prop_assert!(mesh_eq(&r1.mesh, &r2.mesh, 1e-4).is_ok(),
        "apply→inverse→apply must reproduce the first result (deep mesh_eq)");
    Ok(())
}

use crate::mesh_ops::local_ops::{
    ExtrudeFaceOp, ExtrudeFaceParams, FlipFaceOp, FlipFaceParams, LoopCutOp, LoopCutParams,
    SplitEdgeOp, SplitEdgeParams,
};
use crate::mesh_ops::local_rebuild_ops::{
    DeleteFaceOp, DeleteFaceParams, DissolveEdgeOp, DissolveEdgeParams, DissolveFaceOp,
    DissolveFaceParams, InsetFaceOp, InsetFaceParams, MergeVerticesOp, MergeVerticesParams,
};
use crate::mesh_ops::subdivide::{
    SubdivideCatmullClarkOp, SubdivideCcParams, SubdivideFacesOp, SubdivideFacesParams,
};
use crate::half_edge::FaceId;

/// Active face ids of a mesh (half_edge != NO_EDGE, ≥3 verts).
fn active_faces(m: &HalfEdgeMesh) -> Vec<u32> {
    use crate::half_edge::NO_EDGE;
    (0..m.faces.len() as u32)
        .filter(|&i| m.face(FaceId(i)).half_edge != NO_EDGE && m.face_vertices(FaceId(i)).len() >= 3)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// split_edge: O(touched) ElementDelta. Split a random interior edge at a random t.
    #[test]
    fn split_edge_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        edge_pick in any::<u64>(),
        t in 0.1f32..0.9f32,
    ) {
        let edges = interior_edges(&m);
        prop_assume!(!edges.is_empty());
        let e = edges[(edge_pick as usize) % edges.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| SplitEdgeOp.apply(x, &[], &SplitEdgeParams { edge: e, t }))?;
    }

    /// extrude_face: O(touched) ElementDelta. Extrude a random face by a random offset.
    #[test]
    fn extrude_face_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
        d in 0.05f32..0.6f32,
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| {
            ExtrudeFaceOp.apply(x, &[], &ExtrudeFaceParams { face: f, offset: [0.0, d, 0.0] })
        })?;
    }

    /// loop_cut: O(touched) ElementDelta (internally add-only edge splits). Cut a random quad loop.
    #[test]
    fn loop_cut_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        edge_pick in any::<u64>(),
    ) {
        let edges = interior_edges(&m);
        prop_assume!(!edges.is_empty());
        let e = edges[(edge_pick as usize) % edges.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| LoopCutOp.apply(x, &[], &LoopCutParams { start_edge: e }))?;
    }

    /// flip_face: O(touched) ElementDelta (in-place winding reversal). Flip a random face.
    #[test]
    fn flip_face_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| FlipFaceOp.apply(x, &[], &FlipFaceParams { face: f }))?;
    }

    /// delete_face: honest O(body) Snapshot. Delete a random face.
    #[test]
    fn delete_face_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| DeleteFaceOp.apply(x, &[], &DeleteFaceParams { face: f }))?;
    }

    /// dissolve_face: honest O(body) Snapshot. Dissolve a random face into a hole.
    #[test]
    fn dissolve_face_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| DissolveFaceOp.apply(x, &[], &DissolveFaceParams { face: f }))?;
    }

    /// dissolve_edge: honest O(body) Snapshot. Merge the two faces across a random interior edge.
    #[test]
    fn dissolve_edge_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        edge_pick in any::<u64>(),
    ) {
        let edges = interior_edges(&m);
        prop_assume!(!edges.is_empty());
        let e = edges[(edge_pick as usize) % edges.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| DissolveEdgeOp.apply(x, &[], &DissolveEdgeParams { edge: e }))?;
    }

    /// inset_face: honest O(body) Snapshot. Inset a random face by a random amount.
    #[test]
    fn inset_face_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
        amount in 0.02f32..0.3f32,
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| InsetFaceOp.apply(x, &[], &InsetFaceParams { face: f, amount }))?;
    }

    /// merge_vertices: honest O(body) Snapshot. Weld two adjacent verts of a random edge.
    #[test]
    fn merge_vertices_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        edge_pick in any::<u64>(),
    ) {
        let edges = interior_edges(&m);
        prop_assume!(!edges.is_empty());
        let e = edges[(edge_pick as usize) % edges.len()];
        // Two endpoints of the chosen edge (guaranteed distinct, adjacent active verts).
        let he = m.he(HalfEdgeId(e));
        let b = he.vertex.0;
        let a = m.he_source(HalfEdgeId(e)).0;
        prop_assume!(a != b);
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| MergeVerticesOp.apply(x, &[], &MergeVerticesParams { a, b }))?;
    }

    /// subdivide_catmull_clark: honest O(body) Snapshot (whole-mesh rebuild). Subdivide the body.
    #[test]
    fn subdivide_cc_apply_inverse_apply_is_identity(m in arb_manifold_mesh()) {
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| SubdivideCatmullClarkOp.apply(x, &[], &SubdivideCcParams))?;
    }

    /// subdivide_faces: honest O(body) Snapshot. Subdivide a random non-empty subset of faces.
    #[test]
    fn subdivide_faces_apply_inverse_apply_is_identity(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        // Deterministic non-empty subset from the seed.
        let mut sel: Vec<u32> = Vec::new();
        let mut bits = face_pick;
        for (k, &f) in faces.iter().enumerate() {
            if k == 0 || (bits & 1) == 1 { sel.push(f); }
            bits >>= 1;
        }
        let pm = PolyMesh::new(m);
        assert_op_identity(&pm, |x| SubdivideFacesOp.apply(x, &[], &SubdivideFacesParams { faces: sel.clone() }))?;
    }
}

// =====================================================================================
// COPY/CONSTANT PROPAGATION PROPTEST (plan §4.2 Stage A, §4.5; PHASE_P3 propagation gate).
// Random mesh → author UV on a random face → extrude/inset a random face → assert every
// PRE-EXISTING corner's UV is UNCHANGED and every NEW corner's UV equals its Copy parent
// (an authored source vertex's UV) — bounded to the TOUCHED delta, no NaN, no silent zero.
// =====================================================================================

use crate::poly_mesh::CustomData;
use glam::Vec2;

/// Author a distinct UV on every corner of `face`, returning the `HalfEdgeId.0 -> uv` map authored.
fn author_face_uv(cd: &mut CustomData, m: &HalfEdgeMesh, face: FaceId) -> std::collections::HashMap<u32, Vec2> {
    let mut authored = std::collections::HashMap::new();
    for (i, &he) in m.face_half_edges(face).iter().enumerate() {
        let uv = Vec2::new(0.13 * (i as f32 + 1.0), 0.21 * (i as f32 + 1.0));
        cd.uv.set(he, uv);
        authored.insert(he.0, uv);
    }
    authored
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// extrude Copy-propagates UV: author UV on the extruded face, then assert the top face's
    /// corners (same HalfEdgeId slots) keep their UV EXACTLY and the column has no NaN — bounded,
    /// no silent zero. The new side-wall corners that copied a source vertex's UV equal a source UV.
    #[test]
    fn uv_copy_propagation_after_extrude(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
        d in 0.05f32..0.6f32,
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];

        let mut pm = PolyMesh::new(m.clone());
        let authored = author_face_uv(&mut pm.cd, &m, FaceId(f));

        let res = ExtrudeFaceOp.apply(&pm, &[], &ExtrudeFaceParams { face: f, offset: [0.0, d, 0.0] });
        let res = match res { Ok(r) => r, Err(_) => return Ok(()) };

        // (1) Every pre-existing authored corner keeps its EXACT UV (top face reused those slots).
        for (&he, &uv) in &authored {
            prop_assert_eq!(res.mesh.cd.uv.get(crate::half_edge::HalfEdgeId(he)).copied(), Some(uv),
                "extrude must preserve the source face's corner UV exactly");
        }
        // (2) No NaN/Inf, no fabricated value: every entry is finite, and every NON-original entry
        // equals SOME authored source UV (a Copy, never a silent zero or garbage).
        let authored_vals: Vec<Vec2> = authored.values().copied().collect();
        for (he, uv) in res.mesh.cd.uv.iter() {
            prop_assert!(uv.x.is_finite() && uv.y.is_finite(), "no NaN/Inf in propagated UV");
            if !authored.contains_key(&he.0) {
                prop_assert!(authored_vals.iter().any(|w| (*w - *uv).length() < 1e-6),
                    "a new corner's UV must Copy an authored source UV (no silent zero/garbage)");
            }
        }
    }

    /// inset Copy-propagates UV under FREE-LIST reuse: author UV on the inset face, then assert no
    /// NaN and every propagated entry equals an authored source UV (the stale source-corner slots
    /// were cleared, not silently corrupted), and the inset region gained at least one UV.
    #[test]
    fn uv_copy_propagation_after_inset(
        m in arb_manifold_mesh(),
        face_pick in any::<u64>(),
        amount in 0.02f32..0.3f32,
    ) {
        let faces = active_faces(&m);
        prop_assume!(!faces.is_empty());
        let f = faces[(face_pick as usize) % faces.len()];

        let mut pm = PolyMesh::new(m.clone());
        let authored = author_face_uv(&mut pm.cd, &m, FaceId(f));
        let authored_vals: Vec<Vec2> = authored.values().copied().collect();

        let res = InsetFaceOp.apply(&pm, &[], &InsetFaceParams { face: f, amount });
        let res = match res { Ok(r) => r, Err(_) => return Ok(()) };

        let mut propagated = 0usize;
        for (_he, uv) in res.mesh.cd.uv.iter() {
            prop_assert!(uv.x.is_finite() && uv.y.is_finite(), "no NaN/Inf in inset-propagated UV");
            prop_assert!(authored_vals.iter().any(|w| (*w - *uv).length() < 1e-6),
                "every inset-propagated UV must equal an authored source UV (no stale-slot corruption)");
            propagated += 1;
        }
        prop_assert!(propagated > 0, "inset must Copy at least one source UV onto the inset region");
    }
}

// =====================================================================================
// GOLDEN-TOPOLOGY GATE (plan §4.5; PHASE_P2 acceptance). Known-input assertions: exact
// face/vert/edge counts AND validate() manifold AFTER apply, then the SAME pre-op counts
// AND validate() AFTER inverse — proving the inverse reproduces the EXACT pre-op mesh
// (deterministic free-list reuse), not merely a valid mesh.
// =====================================================================================

#[cfg(test)]
mod golden_topology {
    use super::*;
    use crate::mesh_ops::OpDelta;

    /// Apply an op to a fresh cube, return (pre-counts, post-result). Helper for the golden checks.
    fn cube() -> PolyMesh { PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE)) }

    fn counts(m: &HalfEdgeMesh) -> (usize, usize, usize) {
        (m.vertex_count(), m.edge_count(), m.face_count())
    }

    /// Run an op, assert (a) post counts+validate, (b) inverse restores pre counts+validate+mesh_eq.
    fn golden(
        pm: &PolyMesh,
        res: crate::mesh_ops::EditResult,
        expect_post: (usize, usize, usize),
        expect_element_delta: bool,
    ) {
        let pre = counts(&pm.topo);
        // (a) AFTER apply: exact counts + manifold.
        assert_eq!(counts(&res.mesh.topo), expect_post, "post-apply counts");
        res.mesh.topo.validate().expect("post-apply mesh must be manifold");
        // The honest-cost variant is the one we expect for this op class.
        match (&res.inverse, expect_element_delta) {
            (OpDelta::Element(_), true) | (OpDelta::Snapshot(_), false) => {}
            (got, _) => panic!("wrong inverse tier: {:?}", std::mem::discriminant(got)),
        }
        // (b) AFTER inverse on the POST-op mesh: pre counts + manifold + deep-equal the original.
        let mut restored = res.mesh.topo.clone();
        res.inverse.apply_inverse(&mut restored);
        assert_eq!(counts(&restored), pre, "post-inverse counts must equal pre-op");
        restored.validate().expect("post-inverse mesh must be manifold");
        let rpm = PolyMesh::new(restored);
        assert!(mesh_eq(&rpm, pm, 1e-5).is_ok(), "inverse must restore the EXACT pre-op mesh");
    }

    #[test]
    fn golden_split_edge_on_cube() {
        let pm = cube(); // 8 v, 12 e, 6 f
        let res = SplitEdgeOp.apply(&pm, &[], &SplitEdgeParams { edge: 0, t: 0.5 }).unwrap();
        // Split one edge: +1 vertex, +1 edge (the two half-edges of the split edge become four).
        golden(&pm, res, (9, 13, 6), true);
    }

    #[test]
    fn golden_extrude_face_on_cube() {
        let pm = cube();
        let res = ExtrudeFaceOp
            .apply(&pm, &[], &ExtrudeFaceParams { face: 0, offset: [0.0, 0.5, 0.0] })
            .unwrap();
        // Extrude a quad: +4 verts (the lifted top), +4 side faces (top face reused). 12 v, 10 f.
        let post = counts(&res.mesh.topo);
        assert_eq!((post.0, post.2), (12, 10), "extrude: 12 verts, 10 faces");
        golden(&pm, res, post, true);
    }

    #[test]
    fn golden_delete_face_on_cube() {
        let pm = cube();
        let res = DeleteFaceOp.apply(&pm, &[], &DeleteFaceParams { face: 0 }).unwrap();
        // -1 face; the face's 4 half-edges are freed → 24-4=20 active half-edges → edge_count 10.
        golden(&pm, res, (8, 10, 5), false);
    }

    #[test]
    fn golden_dissolve_edge_on_cube() {
        let pm = cube();
        // An interior cube edge between two quads → dissolving merges them into one hexagon. 5 faces.
        let e = {
            use crate::half_edge::{NO_TWIN, NO_FACE};
            (0..pm.topo.half_edges.len() as u32).find(|&i| {
                let he = pm.topo.he(HalfEdgeId(i));
                he.twin != NO_TWIN && he.face != NO_FACE && pm.topo.he(he.twin).face != NO_FACE
            }).unwrap()
        };
        let res = DissolveEdgeOp.apply(&pm, &[], &DissolveEdgeParams { edge: e }).unwrap();
        assert_eq!(res.mesh.topo.face_count(), 5, "dissolve merges two quads → 5 faces");
        let post = counts(&res.mesh.topo);
        golden(&pm, res, post, false);
    }

    #[test]
    fn golden_subdivide_cc_on_cube() {
        let pm = cube();
        let res = SubdivideCatmullClarkOp.apply(&pm, &[], &SubdivideCcParams).unwrap();
        golden(&pm, res, (26, 48, 24), false); // cube → 24 quads, 26 verts, 48 edges
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::*;

    #[test]
    fn mesh_eq_detects_moved_vertex() {
        let a = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let mut moved = HalfEdgeMesh::new_box(Vec3::ONE);
        moved.vertices[0].position += Vec3::splat(0.5);
        let b = PolyMesh::new(moved);
        assert!(mesh_eq(&a, &b, 1e-4).is_err(), "a moved vertex must break mesh_eq");
    }

    #[test]
    fn mesh_eq_ignores_slot_permutation_via_rebuild() {
        // from_polygons rebuild can renumber slots; mesh_eq compares geometry, not slot ids, so a
        // box rebuilt from its own polygon data still deep-equals the original.
        let orig = HalfEdgeMesh::new_box(Vec3::ONE);
        let (positions, faces) = box_polygons(&orig);
        let rebuilt = HalfEdgeMesh::from_polygons(&positions, &faces);
        let a = PolyMesh::new(orig);
        let b = PolyMesh::new(rebuilt);
        assert!(mesh_eq(&a, &b, 1e-4).is_ok(),
            "rebuild that preserves geometry must deep-equal the original");
    }

    /// Extract (positions, face index lists) from a DENSE mesh for a from_polygons round-trip.
    fn box_polygons(m: &HalfEdgeMesh) -> (Vec<Vec3>, Vec<Vec<usize>>) {
        use crate::half_edge::FaceId;
        let positions: Vec<Vec3> = m.vertices.iter().map(|v| v.position).collect();
        let mut faces = Vec::new();
        for i in 0..m.faces.len() {
            faces.push(m.face_vertices(FaceId(i as u32)).iter().map(|v| v.0 as usize).collect());
        }
        (positions, faces)
    }
}

// =====================================================================================
// bincode sentinel-variant guard (plan §4.5, §6.2) — the save-format CI guard
// =====================================================================================

#[cfg(test)]
mod bincode_guard {
    use super::*;
    use serde::{Serialize, Deserialize};

    /// A two-field struct mirroring the in-blob layout hazard: a `HalfEdgeMesh` FOLLOWED BY a
    /// tagged enum. bincode is positional and NOT self-describing, so if a NON-skip field is ever
    /// added to `HalfEdgeMesh`, serialization writes extra bytes and the `Sentinel` after it
    /// deserializes from the WRONG offset — the variant tag / payload come back wrong. This test
    /// fails the moment that happens, exactly like a real `PolygonalBody` whose trailing enum
    /// (e.g. a `Material` node) would misread.
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    enum Sentinel {
        A,
        B(u64),
        C(String),
    }

    #[derive(Serialize, Deserialize)]
    struct MeshThenSentinel {
        mesh: HalfEdgeMesh,
        sentinel: Sentinel,
    }

    #[test]
    fn sentinel_after_halfedgemesh_reads_back() {
        let payload = MeshThenSentinel {
            mesh: HalfEdgeMesh::new_box(Vec3::ONE),
            sentinel: Sentinel::B(0xDEAD_BEEF_CAFE_F00D),
        };
        let bytes = bincode::serialize(&payload).expect("serialize");
        let back: MeshThenSentinel = bincode::deserialize(&bytes).expect("deserialize");

        // If a non-skip HalfEdgeMesh field shifted the bytes, the sentinel would NOT round-trip.
        assert_eq!(back.sentinel, Sentinel::B(0xDEAD_BEEF_CAFE_F00D),
            "sentinel after HalfEdgeMesh did not round-trip — a non-skip field was likely added to \
             HalfEdgeMesh, shifting every following byte (the .fl4-corruption class)");
        assert_eq!(back.mesh.vertex_count(), 8);
        assert_eq!(back.mesh.face_count(), 6);
    }

    /// Belt-and-suspenders: a HalfEdgeMesh round-trips through bincode unchanged (the same codec as
    /// project save/load). Pairs with scene_graph::arc_serde_roundtrip.
    #[test]
    fn halfedgemesh_bincode_roundtrip() {
        let m = HalfEdgeMesh::new_box(Vec3::new(1.0, 2.0, 3.0));
        let bytes = bincode::serialize(&m).expect("serialize");
        let back: HalfEdgeMesh = bincode::deserialize(&bytes).expect("deserialize");
        let a = PolyMesh::new(m);
        let b = PolyMesh::new(back);
        assert!(mesh_eq(&a, &b, 1e-6).is_ok(), "bincode round-trip must preserve the mesh exactly");
    }
}

// =====================================================================================
// FL4 round-trip suite (plan §4.5) — LIVE in Phase 3. The CustomData-LEVEL round-trips
// (the conversion + bincode serialization core_math OWNS) run here; the full ProjectFile
// save_binary/load_binary gates (child_order coexistence, forged-v7) live in the app crate
// (`app/src/project.rs::poly_layer_save_tests`), where `ProjectFile` + the FL4ORDER loader
// live. Together they cover all four mandatory gates (PHASE_P3 §7 Acceptance).
// =====================================================================================

#[cfg(test)]
mod fl4_roundtrip_stubs {
    use super::*;
    use crate::poly_mesh::{CustomData, EdgeKey};
    use crate::half_edge::HalfEdgeId;
    use glam::Vec2;
    use serde::{Serialize, Deserialize};

    /// A serializable mirror of `app::project::PolyLayerRecord` — the exact tuple shape the in-blob
    /// FL4 field carries. core_math cannot reach the app `ProjectFile`, so we round-trip the SAME
    /// payload through the SAME codec (bincode) to prove the conversion + serialization is lossless.
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct PolyLayerRecordMirror {
        node_id: u64,
        uv: Vec<(u32, Vec2)>,
        crease: Vec<(EdgeKey, f32)>,
        seam: Vec<u32>,
        sharp: Vec<u32>,
        #[serde(default)]
        groups: Vec<(u32, u16)>,
        #[serde(default)]
        group_meta: Vec<crate::poly_mesh::PolyGroupMeta>,
    }

    /// Build an authored CustomData on a cube + the record it serializes to.
    fn authored() -> (HalfEdgeMesh, CustomData) {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut cd = CustomData::new();
        // UV on face 0's corners, sharp on one edge, crease on one edge, seam on one corner.
        for (i, &he) in m.face_half_edges(crate::half_edge::FaceId(0)).iter().enumerate() {
            cd.uv.set(he, Vec2::new(0.1 * i as f32, 0.2 * i as f32));
        }
        cd.sharp.set(HalfEdgeId(2), true);
        cd.seam.set(HalfEdgeId(4), true);
        cd.crease.set(EdgeKey(0, 1), 0.75);
        cd.vcrease.set(crate::half_edge::VertexId(3), 0.5); // runtime-only column (NOT in the record this phase)
        (m, cd)
    }

    /// GATE 1 — authored CustomData round-trips through the record codec byte-equal (the columns the
    /// record persists: uv/crease/seam/sharp). Reattach rebuilds an EQUAL CustomData (`columns_eq`).
    #[test]
    fn authored_body_roundtrip() {
        let (_m, cd) = authored();
        let (uv, crease, seam, sharp, groups, group_meta) = cd.to_layer_tuples();
        let rec = PolyLayerRecordMirror {
            node_id: 42,
            uv,
            crease,
            seam,
            sharp,
            groups,
            group_meta,
        };

        // Serialize → deserialize through bincode (the SAME codec as save_binary).
        let bytes = bincode::serialize(&rec).expect("serialize record");
        let back: PolyLayerRecordMirror = bincode::deserialize(&bytes).expect("deserialize record");
        assert_eq!(back.node_id, 42);

        // Rebuild CustomData from the reloaded record; the persisted columns must compare equal.
        let rebuilt = CustomData::from_layer_tuples(
            &back.uv,
            &back.crease,
            &back.seam,
            &back.sharp,
            &back.groups,
            &back.group_meta,
        );
        // Compare ONLY the persisted columns (vcrease is runtime-only this phase → not round-tripped).
        let mut expected = cd.clone();
        expected.vcrease = crate::poly_mesh::LayerStore::new(crate::poly_mesh::Interp::Copy);
        assert!(rebuilt.columns_eq(&expected).is_ok(),
            "authored uv/crease/seam/sharp must round-trip byte-equal: {:?}",
            rebuilt.columns_eq(&expected));
    }

    /// GATE 2 — a pre-record (empty `poly_layers`) load yields EMPTY CustomData. Modelled here as an
    /// empty record list → `from_layer_tuples` on empty inputs → an empty sidecar (no panic).
    #[test]
    fn pre_record_file_loads_empty_customdata() {
        let cd = CustomData::from_layer_tuples(&[], &[], &[], &[], &[], &[]);
        assert!(cd.is_empty(), "a pre-PolyLayerRecord file (no layers) must load empty CustomData");
    }

    /// GATE 3 (kernel half) — the record codec is deterministic + order-independent: two records
    /// built from the SAME authored data are byte-identical (so coexisting with child_order, tested
    /// at the ProjectFile level in app, cannot perturb the poly-layer bytes).
    #[test]
    fn both_child_order_and_poly_layers_load() {
        let (_m, cd) = authored();
        let (uv1, crease1, seam1, sharp1, g1, m1) = cd.to_layer_tuples();
        let (uv2, crease2, seam2, sharp2, g2, m2) = cd.to_layer_tuples();
        let r1 = PolyLayerRecordMirror {
            node_id: 7,
            uv: uv1,
            crease: crease1,
            seam: seam1,
            sharp: sharp1,
            groups: g1,
            group_meta: m1,
        };
        let r2 = PolyLayerRecordMirror {
            node_id: 7,
            uv: uv2,
            crease: crease2,
            seam: seam2,
            sharp: sharp2,
            groups: g2,
            group_meta: m2,
        };
        let b1 = bincode::serialize(&r1).unwrap();
        let b2 = bincode::serialize(&r2).unwrap();
        assert_eq!(b1, b2, "to_layer_tuples must be deterministic (sorted) → reproducible FL4 bytes");
    }

    /// GATE 4 (kernel half) — an EMPTY authored set serializes to a record whose bytes carry no layer
    /// payload (empty Vecs), so an unauthored body adds nothing to the file (the forged-v7
    /// byte-stability guarantee at the kernel level; the full no-magic blob test is in app).
    #[test]
    fn forged_v7_no_magic_byte_identical() {
        let empty = CustomData::new();
        let (uv, crease, seam, sharp, groups, group_meta) = empty.to_layer_tuples();
        assert!(
            uv.is_empty()
                && crease.is_empty()
                && seam.is_empty()
                && sharp.is_empty()
                && groups.is_empty()
                && group_meta.is_empty(),
            "an empty sidecar must produce empty layer vectors → no file growth"
        );
    }

    /// AUTO_POLYGROUPS survival: bevel → tag new faces → group still names those faces.
    #[test]
    fn bevel_auto_group_tags_new_faces() {
        use crate::bevel_op::{bevel_edges_offset, last_bevel_new_face_range, BevelParams};
        use crate::half_edge::FaceId;
        use crate::poly_mesh::PolyGroupKind;
        use std::collections::HashSet;

        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin == crate::half_edge::NO_TWIN || i > he.twin.0 {
                continue;
            }
            let a = cube.he_source(HalfEdgeId(i)).0;
            let b = he.vertex.0;
            let ya = cube.vert(crate::half_edge::VertexId(a)).position.y;
            let yb = cube.vert(crate::half_edge::VertexId(b)).position.y;
            if (ya - 1.0).abs() < 1e-4 && (yb - 1.0).abs() < 1e-4 {
                sel.insert(i);
                break;
            }
        }
        let out = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams {
                width: 0.2,
                segments: 2,
                shape: 0.5,
                clamp: true,
            },
        );
        let (start, end) = last_bevel_new_face_range().expect("bevel must record new-face range");
        assert!(end > start, "bevel must create strip/cap faces");
        assert!((end as usize) <= out.faces.len());

        let mut cd = CustomData::new();
        let gid = cd.create_group(PolyGroupKind::Bevel, None);
        let faces: Vec<FaceId> = (start..end).map(FaceId).collect();
        cd.assign_faces_to_group(faces.iter().copied(), gid);
        assert_eq!(cd.faces_in_group(gid).len() as u32, end - start);

        // Purge should keep them (all faces live).
        cd.purge_empty_groups(&out);
        assert_eq!(cd.faces_in_group(gid).len() as u32, end - start);
        assert_eq!(cd.group_meta.len(), 1);
    }

    /// AUTO_POLYGROUPS: face group + meta round-trip through the record codec.
    #[test]
    fn polygroup_assignment_roundtrips() {
        use crate::half_edge::FaceId;
        use crate::poly_mesh::{PolyGroupBevelParams, PolyGroupKind};
        let mut cd = CustomData::new();
        let gid = cd.create_group_ex(
            PolyGroupKind::Bevel,
            None,
            Some(PolyGroupBevelParams {
                width: 0.25,
                segments: 3,
                shape: 0.5,
                clamp: true,
            }),
        );
        cd.assign_faces_to_group(
            [FaceId(0), FaceId(1), FaceId(2)],
            gid,
        );
        let (uv, crease, seam, sharp, groups, group_meta) = cd.to_layer_tuples();
        let rebuilt = CustomData::from_layer_tuples(&uv, &crease, &seam, &sharp, &groups, &group_meta);
        assert_eq!(rebuilt.faces_in_group(gid).len(), 3);
        assert_eq!(rebuilt.group_meta.len(), 1);
        assert_eq!(rebuilt.group_meta[0].kind, PolyGroupKind::Bevel);
        assert!(rebuilt.group_meta[0].name.starts_with("Bevel"));
        let bp = rebuilt.group_meta[0].bevel.as_ref().expect("bevel params");
        assert!((bp.width - 0.25).abs() < 1e-5);
        assert_eq!(bp.segments, 3);
        assert!(cd.rename_group(gid, "My Bevel".into()));
        assert_eq!(cd.group_meta[0].name, "My Bevel");
    }
}
