//! Id survival across mesh-REBUILDING ops (POLY-STALE-IDS, 2026-10-13) — the id-side twin of
//! `tests_materials.rs`.
//!
//! # The class
//! A soup rebuild through a plain `from_polygons` renumbers every vertex / half-edge / face and
//! compacts dead slots, so every id held OUTSIDE the mesh (poly selection, hidden set, hover,
//! active vertex, the id-keyed `CustomData` sidecar) silently names OTHER geometry afterwards.
//! Each op here rebuilds through `from_polygons_keeping_ids` instead; each test selects geometry
//! the op does not touch, runs the op on a SPARSE mesh (dead slots before the op — the owner's
//! "delete a vertex, then …" sequence), and asserts the held ids name the same geometry.

use super::rebuild_ids::test_support::{assert_ids_name_same_geometry, grid};
use super::*;
use std::collections::HashSet;

/// 3×3 grid (vertex `j*4+i` at `(i, j)`, face `j*3+i`) with corner vertex 15 deleted, which kills
/// face 8. Faces 4 (5,6,10,9) and 7 (9,10,14,13) share no edge with face 0 (0,1,5,4).
fn sparse_grid() -> HalfEdgeMesh {
    let mut m = grid(3);
    m.delete_vertex(VertexId(15));
    assert!(!m.is_face_live(FaceId(8)));
    m
}

/// What an outside holder would have selected, all away from face 0.
fn held(m: &HalfEdgeMesh) -> (Vec<VertexId>, Vec<HalfEdgeId>, Vec<FaceId>) {
    (vec![VertexId(10), VertexId(14)], m.face_half_edges(FaceId(4)), vec![FaceId(4), FaceId(7)])
}

#[test]
fn subdivide_faces_keeps_ids_of_untouched_geometry() {
    let mut m = sparse_grid();
    let before = m.clone();
    m.subdivide_faces(&HashSet::from([0u32]));
    assert!(m.validate().is_ok(), "{:?}", m.validate());
    assert_eq!(m.face_count(), before.face_count() + 3, "face 0 → 4 quads");
    let (v, e, f) = held(&before);
    assert_ids_name_same_geometry(&before, &m, &v, &e, &f);
    assert!(m.is_face_live(FaceId(0)), "the first child quad keeps the parent's id");
    assert!(!m.is_vert_live(VertexId(15)), "a dead slot must stay dead");
}

#[test]
fn poke_faces_keeps_ids_of_untouched_geometry() {
    let mut m = sparse_grid();
    let before = m.clone();
    m.poke_faces(&HashSet::from([0u32]), 0.0);
    assert!(m.validate().is_ok(), "{:?}", m.validate());
    assert_eq!(m.face_count(), before.face_count() + 3, "face 0 → 4-tri fan");
    let (v, e, mut f) = held(&before);
    // Poke never touches a shared edge, so face 0's neighbours are untouched too.
    f.extend([FaceId(1), FaceId(3)]);
    assert_ids_name_same_geometry(&before, &m, &v, &e, &f);
    assert!(m.is_face_live(FaceId(0)), "the first fan triangle keeps the poked face's id");
    assert!(!m.is_vert_live(VertexId(15)), "a dead slot must stay dead");
}

/// The legacy endpoint-slide chamfer (`bevel_edges_ex`). Beveling the edge 1–5 shared by faces 0
/// and 1 rebuilds every face around vertices 1 and 5 (0, 1, 3, 4) — so the held set here is the
/// far side of the grid.
#[test]
fn legacy_bevel_edges_keeps_ids_of_untouched_geometry() {
    let mut m = sparse_grid();
    let before = m.clone();
    let e15 = (0..m.half_edges.len() as u32)
        .map(HalfEdgeId)
        .find(|&h| m.is_edge_live(h) && m.he_source(h) == VertexId(1) && m.he(h).vertex == VertexId(5))
        .expect("edge 1→5");
    m.bevel_edges_ex(&HashSet::from([e15.0]), 0.1);
    assert!(m.validate().is_ok(), "{:?}", m.validate());
    assert_eq!(m.face_count(), before.face_count() + 1, "one strip quad");
    assert_ids_name_same_geometry(
        &before,
        &m,
        &[VertexId(10), VertexId(14)],
        &before.face_half_edges(FaceId(7)),
        &[FaceId(2), FaceId(5), FaceId(6), FaceId(7)],
    );
    assert!(!m.is_vert_live(VertexId(15)), "a dead slot must stay dead");
}

/// Merge by Distance / Degenerate Dissolve share one weld tail (`weld_rebuild`). Vertex 3 is
/// nudged onto vertex 2, so the weld turns face 2 (2,3,7,6) into a triangle; the rest of the
/// grid is untouched. The welded vertex keeps the LOWER id (2); 3 becomes a dead slot.
#[test]
fn merge_by_distance_keeps_ids_of_untouched_geometry() {
    let mut m = sparse_grid();
    m.vert_mut(VertexId(3)).position = Vec3::new(2.0005, 0.0, 0.0);
    let before = m.clone();
    assert_eq!(m.merge_by_distance(1e-3), 1, "one weld");
    assert!(m.validate().is_ok(), "{:?}", m.validate());
    let (v, e, f) = held(&before);
    assert_ids_name_same_geometry(&before, &m, &v, &e, &f);
    assert!(m.is_vert_live(VertexId(2)) && !m.is_vert_live(VertexId(3)), "the cluster keeps its lowest id");
    assert_eq!(m.face_vertices(FaceId(2)).len(), 3, "face 2 keeps its id as the welded triangle");
    assert!(!m.is_vert_live(VertexId(15)), "a dead slot must stay dead");
}
