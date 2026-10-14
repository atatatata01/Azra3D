//! Adversarial knife test — FAMILY: CUT THROUGH EXISTING VERTICES + INTERIOR RUNS.
//!
//! A single stroke that mixes ON-VERTEX boundary nodes (the cut passes exactly through an existing
//! mesh vertex — must SNAP, no new vertex, no zero-length edge) with genuine face-INTERIOR clicked
//! points (must become real cut vertices). Also: a cut whose ENDPOINT lands exactly on a vertex
//! (must anchor as a clean through-cut, never degrade to a dangling slit).
//!
//! Projection is orthographic top-down (screen == world XY), so the polyline screen coords equal the
//! mesh-local XY. `poly_3d` mirrors the screen path at z=0 so interior clicks are recognised.

use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};
use core_math::knife::knife_cut_screen;

/// N×N flat grid in the XY plane, [-0.5,0.5]^2, z=0. Same construction the in-crate tests use.
/// For n=2 the center grid vertex is id 4 at (0,0), shared by all four quads.
fn grid(n: usize) -> HalfEdgeMesh {
    let mut p = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            p.push(Vec3::new(i as f32 / n as f32 - 0.5, j as f32 / n as f32 - 0.5, 0.0));
        }
    }
    let idx = |i: usize, j: usize| j * (n + 1) + i;
    let mut f = Vec::new();
    for j in 0..n {
        for i in 0..n {
            f.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
        }
    }
    HalfEdgeMesh::from_polygons(&p, &f)
}

fn ortho_xy(p: Vec3) -> Option<Vec2> {
    Some(Vec2::new(p.x, p.y))
}

/// Count of ACTIVE vertices whose position is within `tol` of `p`.
fn active_vert_at(m: &HalfEdgeMesh, p: Vec3, tol: f32) -> usize {
    (0..m.vertices.len() as u32)
        .filter(|&v| {
            m.vert(VertexId(v)).half_edge != NO_EDGE && (m.vert(VertexId(v)).position - p).length() < tol
        })
        .count()
}

/// True if NO surviving face carries a repeated vertex id (a dup-vertex face = the self-intersecting
/// tangle the ear-clipper fans into a spray). Through-cuts must be tangle-free.
fn no_tangled_faces(m: &HalfEdgeMesh) -> bool {
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(fid);
        let mut seen = std::collections::HashSet::new();
        if vs.iter().any(|v| !seen.insert(v.0)) {
            return false;
        }
    }
    true
}

/// Count of zero-length boundary edges over all surviving faces (two consecutive ring vertices at the
/// same position). A through-vertex snap that duplicated the vertex would show up here.
fn zero_len_edges(m: &HalfEdgeMesh) -> usize {
    let mut count = 0;
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(fid);
        let n = vs.len();
        for i in 0..n {
            let a = m.vert(vs[i]).position;
            let b = m.vert(vs[(i + 1) % n]).position;
            if (a - b).length() < 1e-6 {
                count += 1;
            }
        }
    }
    count
}

/// Render-path smoke: raster tessellation must not panic and must yield a well-formed index list.
fn assert_renders(m: &HalfEdgeMesh) {
    let (verts, tris) = core_math::poly_mesh::raster_data_ngon(m);
    assert!(!verts.is_empty(), "tessellation produced no vertices");
    assert!(!tris.is_empty(), "tessellation produced no triangles");
    assert_eq!(tris.len() % 3, 0, "triangle index list not a multiple of 3");
    let vmax = verts.len() as u32;
    assert!(tris.iter().all(|&i| i < vmax), "triangle index out of range");
}

// =====================================================================================================
// TEST 1 — V-dip through the CENTER vertex, interior click in the quad on EACH side (TL then TR).
// Path (all in world XY): outside-left -> interior(TL) -> center vertex(0,0) -> interior(TR) -> outside-right.
// Expected: the two boundary entries split their edges (2 new verts), the center is SNAPPED (still exactly
// ONE vertex at (0,0)), each interior click becomes a real cut vertex, TL and TR each split -> +2 faces.
// =====================================================================================================
#[test]
fn thruvert_center_with_interior_runs_both_sides() {
    let mut m = grid(2);
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    assert_eq!(active_vert_at(&m, Vec3::new(0.0, 0.0, 0.0), 1e-4), 1, "sanity: one center vertex pre-cut");

    let p1 = Vec3::new(-0.2, 0.25, 0.0); // interior TL
    let p3 = Vec3::new(0.2, 0.25, 0.0);  // interior TR
    let poly = [
        Vec2::new(-0.7, 0.25),
        Vec2::new(p1.x, p1.y),
        Vec2::new(0.0, 0.0), // exactly the center vertex
        Vec2::new(p3.x, p3.y),
        Vec2::new(0.7, 0.25),
    ];
    let poly_3d = [
        Vec3::new(-0.7, 0.25, 0.0),
        p1,
        Vec3::new(0.0, 0.0, 0.0),
        p3,
        Vec3::new(0.7, 0.25, 0.0),
    ];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
    println!(
        "[T1] cuts={cuts} faces {}->{} verts {}->{} zero_len={} tangle_free={}",
        f0, m.face_count(), v0, m.vertex_count(), zero_len_edges(&m), no_tangled_faces(&m)
    );

    assert!(cuts >= 1, "mixed through-vertex cut must create geometry, got {cuts}");
    assert!(m.validate().is_ok(), "mesh valid after mixed through-vertex cut");
    assert!(no_tangled_faces(&m), "no self-intersecting (dup-vertex) face");
    assert_eq!(zero_len_edges(&m), 0, "through-vertex snap must not create a zero-length edge");
    // Center snapped, NOT duplicated:
    assert_eq!(active_vert_at(&m, Vec3::new(0.0, 0.0, 0.0), 1e-4), 1, "center vertex snapped, not duplicated");
    // Interior clicks materialised as real vertices:
    assert_eq!(active_vert_at(&m, p1, 1e-3), 1, "TL interior click became exactly one real vertex");
    assert_eq!(active_vert_at(&m, p3, 1e-3), 1, "TR interior click became exactly one real vertex");
    // TL + TR each split -> +2 faces. (The center touch and the two boundary entries add NO faces.)
    assert_eq!(m.face_count(), f0 + 2, "TL and TR each split into two -> +2 faces");
    assert_renders(&m);
}

// =====================================================================================================
// TEST 2 — cut whose ENDPOINT lands EXACTLY on a vertex, with an interior click before it.
// Path: outside-left -> interior(TL) -> vertex 7 (0,0.5) (the endpoint, a real grid vertex).
// Expected: the endpoint anchors as a clean through-cut Bound (NOT a dangling slit). TL splits -> +1 face,
// no dup-vertex face, vertex 7 not duplicated. A regression here would degrade to a slit (face_count
// unchanged, a dup-vertex spur face) — both of which the asserts below reject.
// =====================================================================================================
#[test]
fn thruvert_endpoint_lands_on_vertex() {
    let mut m = grid(2);
    let f0 = m.face_count();
    let v7 = Vec3::new(0.0, 0.5, 0.0); // grid vertex id 7 (top-middle)
    assert_eq!(active_vert_at(&m, v7, 1e-4), 1, "sanity: one vertex at (0,0.5) pre-cut");

    let p1 = Vec3::new(-0.2, 0.25, 0.0); // interior TL
    let poly = [
        Vec2::new(-0.7, 0.25),
        Vec2::new(p1.x, p1.y),
        Vec2::new(v7.x, v7.y), // endpoint == existing vertex
    ];
    let poly_3d = [Vec3::new(-0.7, 0.25, 0.0), p1, v7];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
    println!(
        "[T2] cuts={cuts} faces {}->{} zero_len={} tangle_free={} v_at_endpoint={}",
        f0, m.face_count(), zero_len_edges(&m), no_tangled_faces(&m), active_vert_at(&m, v7, 1e-4)
    );

    assert!(cuts >= 1, "endpoint-on-vertex cut must create geometry, got {cuts}");
    assert!(m.validate().is_ok(), "mesh valid after endpoint-on-vertex cut");
    assert!(no_tangled_faces(&m), "endpoint-on-vertex must NOT degrade to a dup-vertex slit face");
    assert_eq!(zero_len_edges(&m), 0, "endpoint snap must not create a zero-length edge");
    assert_eq!(active_vert_at(&m, v7, 1e-4), 1, "endpoint vertex snapped, not duplicated");
    assert_eq!(active_vert_at(&m, p1, 1e-3), 1, "interior click became a real vertex");
    assert_eq!(m.face_count(), f0 + 1, "clean through-cut: TL splits into two (a slit would leave it at f0)");
    assert_renders(&m);
}

// =====================================================================================================
// TEST 3 — through the CENTER vertex DIAGONALLY, interior clicks in DIFFERENT quadrants (TL then BR).
// Stresses face disambiguation: the center vertex borders all four quads, so each interior run must be
// routed (via its probe point) to the correct face. Path: outside-left -> interior(TL) -> center ->
// interior(BR) -> outside-right. Expected: TL and BR each split -> +2 faces, center snapped once.
// =====================================================================================================
#[test]
fn thruvert_center_diagonal_disambiguates_faces() {
    let mut m = grid(2);
    let f0 = m.face_count();
    let p1 = Vec3::new(-0.2, 0.2, 0.0);  // interior TL
    let p3 = Vec3::new(0.2, -0.2, 0.0);  // interior BR
    let poly = [
        Vec2::new(-0.7, 0.3),
        Vec2::new(p1.x, p1.y),
        Vec2::new(0.0, 0.0), // center vertex
        Vec2::new(p3.x, p3.y),
        Vec2::new(0.7, -0.3),
    ];
    let poly_3d = [
        Vec3::new(-0.7, 0.3, 0.0),
        p1,
        Vec3::new(0.0, 0.0, 0.0),
        p3,
        Vec3::new(0.7, -0.3, 0.0),
    ];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
    println!(
        "[T3] cuts={cuts} faces {}->{} zero_len={} tangle_free={} center_ct={}",
        f0, m.face_count(), zero_len_edges(&m), no_tangled_faces(&m),
        active_vert_at(&m, Vec3::new(0.0, 0.0, 0.0), 1e-4)
    );

    assert!(cuts >= 1, "diagonal through-vertex cut must create geometry, got {cuts}");
    assert!(m.validate().is_ok(), "mesh valid after diagonal through-vertex cut");
    assert!(no_tangled_faces(&m), "no self-intersecting face across the diagonal span");
    assert_eq!(zero_len_edges(&m), 0, "center snap must not create a zero-length edge");
    assert_eq!(active_vert_at(&m, Vec3::new(0.0, 0.0, 0.0), 1e-4), 1, "center vertex snapped once");
    assert_eq!(active_vert_at(&m, p1, 1e-3), 1, "TL interior click became a real vertex");
    assert_eq!(active_vert_at(&m, p3, 1e-3), 1, "BR interior click became a real vertex");
    assert_eq!(m.face_count(), f0 + 2, "TL and BR each split -> +2 faces (correct disambiguation)");
    assert_renders(&m);
}
