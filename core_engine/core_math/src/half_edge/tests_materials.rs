//! Regression tests for the per-face material-loss CLASS.
//!
//! # The class
//! `HalfEdgeMesh::from_polygons` and `create_face` both allocate faces with `material_id: 0`.
//! Every op that DELETES faces and re-creates replacement geometry therefore repainted the whole
//! affected region with the default material. Owner-visible instances:
//!   * "Dissolving edge (Ctrl+X) removes material"  → `dissolve_one_region` → `create_face`
//!   * "Symmetrize (Alt+X): I lose my material"     → `mirrored_across` / `mirrored_single_axis`
//!
//! These assert EXACT ids, never merely "not zero" — a fix that paints every face with a single
//! surviving material would pass a non-zero check while being just as wrong.

use super::*;
use glam::Vec3;

/// 2x1 grid of quads in the XY plane, sharing the middle edge. Face 0 gets `m0`, face 1 gets `m1`.
///
/// ```text
///  3---4---5
///  | 0 | 1 |
///  0---1---2
/// ```
fn two_quads(m0: u32, m1: u32) -> HalfEdgeMesh {
    let p = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
    ];
    let f = vec![vec![0, 1, 4, 3], vec![1, 2, 5, 4]];
    HalfEdgeMesh::from_polygons_with_materials(&p, &f, &[m0, m1])
}

/// Every live face's `material_id`, in FaceId order.
fn live_materials(m: &HalfEdgeMesh) -> Vec<u32> {
    (0..m.faces.len() as u32)
        .map(FaceId)
        .filter(|f| m.face(*f).half_edge != NO_EDGE)
        .map(|f| m.face(f).material_id)
        .collect()
}

// ============================================================
// The additive constructor
// ============================================================

#[test]
fn from_polygons_with_materials_stamps_each_face() {
    let m = two_quads(7, 9);
    assert_eq!(live_materials(&m), vec![7, 9]);
}

#[test]
fn from_polygons_delegates_with_zero_materials() {
    // The no-material overload must stay bit-compatible with the old behaviour.
    let p = vec![
        Vec3::ZERO,
        Vec3::X,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::Y,
    ];
    let f = vec![vec![0, 1, 2, 3]];
    assert_eq!(live_materials(&HalfEdgeMesh::from_polygons(&p, &f)), vec![0]);
}

#[test]
fn short_rings_do_not_drift_materials() {
    // `from_polygons` SKIPS rings with < 3 verts. The material slice is indexed by INPUT index, so
    // the surviving faces must still get THEIR own ids — not their neighbours'. This is the exact
    // drift a naive post-pass over output FaceIds would introduce.
    let p = vec![
        Vec3::ZERO,
        Vec3::X,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::Y,
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(3.0, 0.0, 0.0),
        Vec3::new(3.0, 1.0, 0.0),
    ];
    let f = vec![
        vec![0, 1, 2, 3], // index 0 -> kept
        vec![4, 5],       // index 1 -> SKIPPED (2 verts)
        vec![4, 5, 6],    // index 2 -> kept
    ];
    let m = HalfEdgeMesh::from_polygons_with_materials(&p, &f, &[11, 22, 33]);
    assert_eq!(live_materials(&m), vec![11, 33], "skipped ring must not shift materials");
}

#[test]
fn faces_as_new_mesh_keeps_materials() {
    let m = two_quads(4, 5);
    let sep = m.faces_as_new_mesh(&[FaceId(1)]).expect("one face separates");
    assert_eq!(live_materials(&sep), vec![5]);
}

// ============================================================
// create_face_with_material
// ============================================================

#[test]
fn create_face_with_material_stamps_and_plain_does_not() {
    let mut m = HalfEdgeMesh::new();
    for p in [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y] {
        m.alloc_vertex(HVertex { position: p, normal: Vec3::Z, half_edge: NO_EDGE });
    }
    let ring: Vec<VertexId> = (0..4).map(VertexId).collect();
    let f = m.create_face_with_material(&ring, 42).expect("quad");
    assert_eq!(m.face(f).material_id, 42);
}

// ============================================================
// Dissolve (Ctrl+X) — the primary owner report
// ============================================================

#[test]
fn dissolve_edge_keeps_surviving_face_material() {
    // dissolve_edge merges face_b INTO face_a; the survivor keeps its own material.
    let mut m = two_quads(7, 9);
    let he = m
        .face_half_edges(FaceId(0))
        .into_iter()
        .find(|&h| {
            let tw = m.he(h).twin;
            tw != NO_TWIN && m.he(tw).face == FaceId(1)
        })
        .expect("shared edge");
    let survivor = m.dissolve_edge(he).expect("merge");
    assert_eq!(m.face(survivor).material_id, 7);
}

#[test]
fn dissolve_edges_region_keeps_material() {
    // The real Ctrl+X path: dissolve_edges_region -> dissolve_one_region -> create_face.
    // BEFORE the fix the merged n-gon came back with material 0 (the owner's "Ctrl+X removes
    // material"); it must now carry the region's material.
    let mut m = two_quads(7, 7);
    let he = m
        .face_half_edges(FaceId(0))
        .into_iter()
        .find(|&h| {
            let tw = m.he(h).twin;
            tw != NO_TWIN && m.he(tw).face == FaceId(1)
        })
        .expect("shared edge");
    let mut sel = std::collections::HashSet::new();
    sel.insert(he);
    assert!(m.dissolve_edges_region(&sel), "region dissolve must fire");
    let mats = live_materials(&m);
    assert_eq!(mats, vec![7], "merged n-gon must keep material 7, not fall back to 0");
}

#[test]
fn dissolve_region_material_is_deterministic_lowest_face() {
    // Mixed materials in one region: the merged face takes the LOWEST live FaceId's material —
    // deterministic (comp is a HashSet, so iteration order is not), and never 0.
    let mut m = two_quads(3, 8);
    let he = m
        .face_half_edges(FaceId(0))
        .into_iter()
        .find(|&h| {
            let tw = m.he(h).twin;
            tw != NO_TWIN && m.he(tw).face == FaceId(1)
        })
        .expect("shared edge");
    let mut sel = std::collections::HashSet::new();
    sel.insert(he);
    assert!(m.dissolve_edges_region(&sel));
    assert_eq!(live_materials(&m), vec![3]);
}

#[test]
fn dissolve_faces_region_keeps_material() {
    let mut m = two_quads(6, 6);
    let sel: std::collections::HashSet<FaceId> = [FaceId(0), FaceId(1)].into_iter().collect();
    let merged = m.dissolve_faces_region(&sel);
    assert_eq!(merged.len(), 1);
    assert_eq!(m.face(merged[0]).material_id, 6);
}

// ============================================================
// Symmetrize / mirror (Alt+X) — the second owner report
// ============================================================

#[test]
fn mirrored_across_mirrors_materials() {
    // Mirror across the YZ plane at x = 2 (outside the geometry, so nothing welds and the face
    // count exactly doubles). Each mirrored face must carry ITS SOURCE face's material.
    let m = two_quads(7, 9);
    let out = m.mirrored_across(Vec3::new(2.0, 0.0, 0.0), Vec3::X, false, 1e-5);
    assert_eq!(
        live_materials(&out),
        vec![7, 9, 7, 9],
        "mirror must carry materials by default"
    );
}

#[test]
fn mirrored_axis_mirrors_materials() {
    let m = two_quads(7, 9);
    let out = m.mirrored(MirrorAxes::X, false, 1e-5);
    assert_eq!(live_materials(&out), vec![7, 9, 7, 9]);
}

#[test]
fn symmetrize_keeps_and_mirrors_materials() {
    // Full Alt+X op: bisect + delete_half + mirror. The kept half sits at x >= 1 (material 9);
    // symmetrizing about x = 1 must produce a body made ENTIRELY of material 9 — never 0.
    let mut m = two_quads(7, 9);
    m.symmetrize(Vec3::new(1.0, 0.0, 0.0), Vec3::X, true, 1e-4);
    let mats = live_materials(&m);
    assert!(!mats.is_empty(), "symmetrize must leave geometry");
    assert!(
        mats.iter().all(|&x| x == 9),
        "symmetrize must mirror the kept half's material, got {mats:?}"
    );
}

#[test]
fn symmetrize_preserves_distinct_materials_per_side() {
    // Two quads on the KEPT side with DIFFERENT materials — proves the mirror maps each face to
    // its own source, rather than flooding one material over everything.
    let p = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
    ];
    let f = vec![vec![0, 1, 4, 3], vec![1, 2, 5, 4]];
    let m = HalfEdgeMesh::from_polygons_with_materials(&p, &f, &[7, 9]);
    // Mirror about x = 2 with no welding: 4 faces, materials [7, 9, 7, 9].
    let out = m.mirrored_across(Vec3::new(2.0, 0.0, 0.0), Vec3::X, false, 1e-6);
    let mats = live_materials(&out);
    assert_eq!(mats.len(), 4);
    assert_eq!(mats.iter().filter(|&&x| x == 7).count(), 2);
    assert_eq!(mats.iter().filter(|&&x| x == 9).count(), 2);
}

// ============================================================
// Same class, other rebuild-through-a-constructor ops
// ============================================================

#[test]
fn poke_faces_keeps_materials() {
    let mut m = two_quads(7, 9);
    let sel: std::collections::HashSet<u32> = [0u32].into_iter().collect();
    m.poke_faces(&sel, 0.0);
    let mats = live_materials(&m);
    // Face 0 poked into a 4-tri fan (all material 7); face 1 copied unchanged (material 9).
    assert_eq!(mats.iter().filter(|&&x| x == 7).count(), 4);
    assert_eq!(mats.iter().filter(|&&x| x == 9).count(), 1);
    assert_eq!(mats.iter().filter(|&&x| x == 0).count(), 0);
}

#[test]
fn subdivide_faces_keeps_materials() {
    let mut m = two_quads(7, 9);
    let sel: std::collections::HashSet<u32> = [0u32].into_iter().collect();
    m.subdivide_faces(&sel);
    let mats = live_materials(&m);
    assert_eq!(mats.iter().filter(|&&x| x == 7).count(), 4, "4 child quads keep 7");
    assert_eq!(mats.iter().filter(|&&x| x == 9).count(), 1, "neighbour n-gon keeps 9");
    assert_eq!(mats.iter().filter(|&&x| x == 0).count(), 0);
}

#[test]
fn catmull_clark_keeps_materials() {
    let mut m = two_quads(7, 9);
    m.subdivide_catmull_clark();
    let mats = live_materials(&m);
    assert_eq!(mats.iter().filter(|&&x| x == 7).count(), 4);
    assert_eq!(mats.iter().filter(|&&x| x == 9).count(), 4);
    assert_eq!(mats.iter().filter(|&&x| x == 0).count(), 0);
}

#[test]
fn from_quads_with_materials_stamps_each_face() {
    let p = vec![
        Vec3::ZERO,
        Vec3::X,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::Y,
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
    ];
    let q = [[0usize, 1, 2, 3], [1, 4, 5, 2]];
    let m = HalfEdgeMesh::from_quads_with_materials(&p, &q, &[3, 4]);
    assert_eq!(live_materials(&m), vec![3, 4]);
    // The plain overload must stay exactly as before.
    assert_eq!(live_materials(&HalfEdgeMesh::from_quads(&p, &q)), vec![0, 0]);
}
