//! WHOLE-MESH subdivide ops wrapped as `MeshOp`s with the honest O(body) `Snapshot` inverse
//! (plan §4.6 honest cost model, Risk #3; §7 Phase 2 — Gate 2a: inverse deltas ONLY, NO parent-map).
//!
//! `subdivide_catmull_clark` and `subdivide_faces` REBUILD the whole body (`*self =
//! from_polygons(...)`), touching ~all elements. Per plan §4.6 their inverse is honestly the
//! per-BODY Arc snapshot (O(body)) — NOT a faked element delta. This is among the MOST COMMON
//! destructive poly ops, and O(body) is still strictly cheaper than Blender's O(scene) state-copy
//! undo (T88044). The cost tier is encoded in the `OpDelta::Snapshot` variant and documented here,
//! never hidden behind a false "all O(touched)" claim.
//!
//! SCOPE GUARD: NO n-gon topology fix here (that is Phase 5 — the T31537 audit). These wrappers
//! apply `subdivide_*` AS-IS. ZERO `half_edge.rs` edits.

use std::collections::HashSet;

use serde::Serialize;

use crate::mesh_ops::{EditResult, Handle, MeshOp, OpDelta, OpError, TopoDelta};
use crate::poly_mesh::PolyMesh;

/// `subdivide_catmull_clark` takes no params (whole-mesh). The struct is empty but `Serialize` so a
/// past op can be re-opened in the op-record history (§4.6).
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SubdivideCcParams;

pub struct SubdivideCatmullClarkOp;

impl MeshOp for SubdivideCatmullClarkOp {
    type Params = SubdivideCcParams;
    const PROPAGATES: bool = false; // Gate 2a — no parent-map (crease/uv flow is Phase 4/5).

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], _p: &Self::Params) -> Result<EditResult, OpError> {
        if m.topo.face_count() == 0 {
            return Err(OpError::EmptySelection);
        }
        let before = m.topo.clone(); // honest per-BODY snapshot (O(body)).
        let mut topo = m.topo.clone();
        topo.subdivide_catmull_clark();
        topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
        let mut out = m.clone();
        out.set_topo(topo); // bumps topo_gen
        Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Snapshot(before) })
    }
}

/// `subdivide_faces` subdivides a SELECTED set of faces (each n-gon → n quads). The set is the param.
#[derive(Clone, Debug, Serialize)]
pub struct SubdivideFacesParams {
    pub faces: Vec<u32>,
}

pub struct SubdivideFacesOp;

impl MeshOp for SubdivideFacesOp {
    type Params = SubdivideFacesParams;
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, _sel: &[Handle], p: &Self::Params) -> Result<EditResult, OpError> {
        if p.faces.is_empty() {
            return Err(OpError::EmptySelection);
        }
        let before = m.topo.clone(); // honest per-BODY snapshot (O(body)) — subdivide_faces rebuilds.
        let mut topo = m.topo.clone();
        let set: HashSet<u32> = p.faces.iter().copied().collect();
        topo.subdivide_faces(&set);
        topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;
        let mut out = m.clone();
        out.set_topo(topo);
        Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse: OpDelta::Snapshot(before) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use crate::half_edge::HalfEdgeMesh;

    #[test]
    fn subdivide_cc_emits_snapshot_and_multiplies_faces() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = SubdivideCatmullClarkOp.apply(&pm, &[], &SubdivideCcParams).unwrap();
        // A cube (6 quads) → 24 quads after one Catmull-Clark level.
        assert_eq!(res.mesh.topo.face_count(), 24, "cc subdivide of a cube = 24 quads");
        assert!(matches!(res.inverse, OpDelta::Snapshot(_)),
            "whole-mesh subdivide uses the honest O(body) snapshot tier");
        assert_eq!(res.mesh.topo_gen, 1);
    }

    #[test]
    fn subdivide_faces_emits_snapshot() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = SubdivideFacesOp.apply(&pm, &[], &SubdivideFacesParams { faces: vec![0] }).unwrap();
        // Face 0 (a quad) → 4 quads; the other 5 faces kept → 9 total.
        assert_eq!(res.mesh.topo.face_count(), 9, "one quad → 4, plus 5 kept = 9");
        assert!(matches!(res.inverse, OpDelta::Snapshot(_)));
    }

    #[test]
    fn subdivide_faces_rejects_empty() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let err = SubdivideFacesOp.apply(&pm, &[], &SubdivideFacesParams { faces: vec![] }).unwrap_err();
        assert_eq!(err, OpError::EmptySelection);
    }

    /// Upgraded whole-mesh CC snapshot check: the op must ALSO produce a MANIFOLD result (Bug #4).
    /// The old `subdivide_cc_emits_snapshot_and_multiplies_faces` only checked face_count/validate()
    /// and stayed green while the app path (is_manifold gate) refused a bowtie CC. Pin manifold here.
    #[test]
    fn subdivide_cc_is_manifold() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let res = SubdivideCatmullClarkOp.apply(&pm, &[], &SubdivideCcParams).unwrap();
        assert_eq!(res.mesh.topo.face_count(), 24);
        assert!(res.mesh.topo.is_manifold().is_ok(),
            "CC of a cube must be manifold (26 verts, all single fans): {:?}",
            res.mesh.topo.is_manifold());
    }
}

/// Regression tests for user report #4 ("face subdivide does nothing"). These go through the REAL app
/// dispatch — `PolyEdit::…apply_in_place(&mut mesh)` — which runs the full `is_manifold()` integrity
/// gate (mesh_ops/mod.rs:564) that the op-level `apply()` above and the pre-existing unit tests do NOT.
/// Both subdivide ops used to produce NON-MANIFOLD meshes, so `apply_in_place` returned
/// `Err(InvalidResult{"…bowtie"})` and the app silently refused the whole op — "tests pass, app refuses".
///
/// GATE = VERTEX IDENTITY against a headless Blender oracle (`bmesh.ops.subdivide_edges(use_grid_fill
/// =True)` — what Mesh→Subdivide calls internally) run on a ±1 cube, comparing the POSITION MULTISET.
/// Blender's linear-subdivide vertex set = 8 corners + midpoints of every edge of every selected face
/// + one center per selected face; every coordinate lands in {-1,0,1}. AZRA matches that set exactly.
/// Blender additionally TRIANGULATES neighbours that receive 2 adjacent cuts; AZRA keeps them as
/// manifold n-gons — SAME vertices, different internal tessellation, both closed+manifold.
#[cfg(test)]
mod apply_path_tests {
    use crate::half_edge::HalfEdgeMesh;
    use crate::mesh_ops::PolyEdit;
    use glam::Vec3;

    /// AZRA `new_box` faces (half_edge.rs:2884): 0=-Y, 1=+Y, 2=-Z, 3=+Z, 4=-X, 5=+X. The Blender
    /// oracle selects the SAME geometric faces by normal, so the multisets are directly comparable.

    /// Sorted integer position multiset (coords ×1e4, rounded) of the LIVE vertices. The fixtures
    /// are dense and the id-keeping rebuild only frees slots that were dead before the op (or that
    /// it consumed — subdivide consumes no vertex), so the result is dense, which we assert first.
    fn pos_ms(m: &HalfEdgeMesh) -> Vec<[i64; 3]> {
        assert_eq!(m.vertices.len(), m.vertex_count(),
            "subdivide of a dense mesh stays dense — no free vertex slots expected");
        let mut v: Vec<[i64; 3]> = m.vertices.iter().map(|vert| {
            let p = vert.position;
            [(p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64]
        }).collect();
        v.sort();
        v
    }

    /// Build the expected sorted multiset from a list of exact coordinate triples (Blender oracle).
    fn expect_ms(coords: &[[f32; 3]]) -> Vec<[i64; 3]> {
        let mut v: Vec<[i64; 3]> = coords.iter().map(|c| {
            [(c[0] * 1e4).round() as i64, (c[1] * 1e4).round() as i64, (c[2] * 1e4).round() as i64]
        }).collect();
        v.sort();
        v
    }

    // ---- Blender oracle vertex sets (bmesh.ops.subdivide_edges, use_grid_fill=True, ±1 cube) ----

    // -Y face alone → 13 verts, 9 faces {4:5,5:4}
    const BLENDER_MINUS_Y: [[f32; 3]; 13] = [
        [-1.,-1.,-1.],[-1.,-1.,0.],[-1.,-1.,1.],[-1.,1.,-1.],[-1.,1.,1.],
        [0.,-1.,-1.],[0.,-1.,0.],[0.,-1.,1.],[1.,-1.,-1.],[1.,-1.,0.],
        [1.,-1.,1.],[1.,1.,-1.],[1.,1.,1.],
    ];
    // -Y and -Z (2 adjacent) → 17 verts
    const BLENDER_MINUS_Y_MINUS_Z: [[f32; 3]; 17] = [
        [-1.,-1.,-1.],[-1.,-1.,0.],[-1.,-1.,1.],[-1.,0.,-1.],[-1.,1.,-1.],[-1.,1.,1.],
        [0.,-1.,-1.],[0.,-1.,0.],[0.,-1.,1.],[0.,0.,-1.],[0.,1.,-1.],
        [1.,-1.,-1.],[1.,-1.,0.],[1.,-1.,1.],[1.,0.,-1.],[1.,1.,-1.],[1.,1.,1.],
    ];
    // -Z, +X, +Y (corner triad / L) → 20 verts
    const BLENDER_L: [[f32; 3]; 20] = [
        [-1.,-1.,-1.],[-1.,-1.,1.],[-1.,0.,-1.],[-1.,1.,-1.],[-1.,1.,0.],[-1.,1.,1.],
        [0.,-1.,-1.],[0.,0.,-1.],[0.,1.,-1.],[0.,1.,0.],[0.,1.,1.],
        [1.,-1.,-1.],[1.,-1.,0.],[1.,-1.,1.],[1.,0.,-1.],[1.,0.,0.],[1.,0.,1.],
        [1.,1.,-1.],[1.,1.,0.],[1.,1.,1.],
    ];
    // ALL faces → 26 verts, 24 quads
    const BLENDER_ALL: [[f32; 3]; 26] = [
        [-1.,-1.,-1.],[-1.,-1.,0.],[-1.,-1.,1.],[-1.,0.,-1.],[-1.,0.,0.],[-1.,0.,1.],
        [-1.,1.,-1.],[-1.,1.,0.],[-1.,1.,1.],[0.,-1.,-1.],[0.,-1.,0.],[0.,-1.,1.],
        [0.,0.,-1.],[0.,0.,1.],[0.,1.,-1.],[0.,1.,0.],[0.,1.,1.],
        [1.,-1.,-1.],[1.,-1.,0.],[1.,-1.,1.],[1.,0.,-1.],[1.,0.,0.],[1.,0.,1.],
        [1.,1.,-1.],[1.,1.,0.],[1.,1.,1.],
    ];

    fn subdivide_faces(faces: Vec<u32>) -> HalfEdgeMesh {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        PolyEdit::SubdivideFaces { faces }
            .apply_in_place(&mut m)
            .expect("SubdivideFaces must be ACCEPTED by the is_manifold app gate (Bug #4 was a refusal)");
        assert!(m.is_manifold().is_ok(), "result must be manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        m
    }

    #[test]
    fn linear_single_face_matches_blender_identity() {
        let m = subdivide_faces(vec![0]); // -Y
        assert_eq!(m.vertex_count(), 13, "8 corners + 4 edge mids + 1 center");
        assert_eq!(pos_ms(&m), expect_ms(&BLENDER_MINUS_Y), "vertex multiset must equal Blender -Y");
    }

    #[test]
    fn linear_two_adjacent_faces_matches_blender_identity() {
        let m = subdivide_faces(vec![0, 2]); // -Y, -Z (share the y=-1,z=-1 edge)
        assert_eq!(m.vertex_count(), 17, "8 + 7 union edge mids + 2 centers");
        assert_eq!(pos_ms(&m), expect_ms(&BLENDER_MINUS_Y_MINUS_Z));
    }

    #[test]
    fn linear_l_shape_shared_neighbor_two_midpoints() {
        // -Z(2), +X(5), +Y(1): each unselected neighbour (-Y,+Z,-X) borders TWO selected faces →
        // receives TWO midpoints in-ring (n-gon). Proves multi-midpoint ring insertion stays manifold.
        let m = subdivide_faces(vec![2, 5, 1]);
        assert_eq!(m.vertex_count(), 20, "8 + 9 union edge mids + 3 centers");
        assert_eq!(pos_ms(&m), expect_ms(&BLENDER_L));
    }

    #[test]
    fn linear_all_faces_is_24_quads_matches_blender() {
        let m = subdivide_faces(vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(m.vertex_count(), 26);
        assert_eq!(m.face_count(), 24, "every face → 4 quads, no n-gon neighbours");
        assert_eq!(pos_ms(&m), expect_ms(&BLENDER_ALL));
    }

    #[test]
    fn cc_cube_manifold_and_positions() {
        // Catmull-Clark of a cube (nothing selected → smooth). Must be MANIFOLD (Bug #4-B: the quad
        // assembly paired each new vertex with the WRONG two edge-points → bowtie). 26 verts = 8 corner
        // + 12 edge + 6 face points; corners valence 3, edge points valence 4.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        PolyEdit::SubdivideCatmullClark
            .apply_in_place(&mut m)
            .expect("CC must be ACCEPTED by the is_manifold app gate");
        assert!(m.is_manifold().is_ok(), "CC cube must be manifold: {:?}", m.is_manifold());
        assert_eq!(m.vertex_count(), 26);
        assert_eq!(m.face_count(), 24);

        // AZRA uses the TEXTBOOK Catmull-Clark stencils: corner (F+2R+(n-3)P)/n with R=edge MIDPOINTS
        // → corners at ±5/9, edge points at ±3/4, face points at ±1. (Blender/OpenSubdiv pulls these
        // further in — ±0.5 / ±0.6096 / ±0.8395 — a different, limit-converging weight set. Both are
        // valid manifold CC surfaces; the OpenSubdiv-exact weights are a separate follow-up, tracked.)
        let classic: [[f32; 3]; 26] = [
            [-1.,0.,0.],[-0.75,-0.75,0.],[-0.75,0.,-0.75],[-0.75,0.,0.75],[-0.75,0.75,0.],
            [-0.555556,-0.555556,-0.555556],[-0.555556,-0.555556,0.555556],
            [-0.555556,0.555556,-0.555556],[-0.555556,0.555556,0.555556],
            [0.,-1.,0.],[0.,-0.75,-0.75],[0.,-0.75,0.75],[0.,0.,-1.],[0.,0.,1.],
            [0.,0.75,-0.75],[0.,0.75,0.75],[0.,1.,0.],
            [0.555556,-0.555556,-0.555556],[0.555556,-0.555556,0.555556],
            [0.555556,0.555556,-0.555556],[0.555556,0.555556,0.555556],
            [0.75,-0.75,0.],[0.75,0.,-0.75],[0.75,0.,0.75],[0.75,0.75,0.],[1.,0.,0.],
        ];
        assert_eq!(pos_ms(&m), expect_ms(&classic),
            "CC positions must equal the textbook Catmull-Clark stencil result");
    }

    #[test]
    fn cc_noncube_irregular_is_manifold() {
        // Prove CC is NOT cube-special-cased: linear-subdivide one face first (introduces irregular
        // valences + n-gons), THEN Catmull-Clark the whole thing. Must still be manifold.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        PolyEdit::SubdivideFaces { faces: vec![0] }.apply_in_place(&mut m).unwrap();
        assert!(m.is_manifold().is_ok(), "linear pre-pass manifold: {:?}", m.is_manifold());
        PolyEdit::SubdivideCatmullClark.apply_in_place(&mut m)
            .expect("CC of an irregular (already-subdivided) mesh must be accepted");
        assert!(m.is_manifold().is_ok(), "CC of irregular mesh must be manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
    }
}
