//! Adversarial verification — FAMILY: MANY CROSSINGS ON ONE EDGE.
//!
//! A zig-zag polyline authored as a SAWTOOTH against ONE edge of a single quad, crossing that
//! edge 3–4 times. Every crossing must materialise its OWN distinct vertex (progressive
//! multi-split, knife.rs step 2, lines 222-278) and the mesh must stay watertight (validate ok,
//! no self-intersecting/dup-vertex face, raster tessellation cannot panic). Plus the
//! near-coincident case (two crossings within ~1e-4 on the same edge) — must MERGE, not spawn a
//! zero-length edge that breaks validate.
//!
//! Base mesh for every case: a single CCW quad in the z=0 plane, [-1,1]^2, normal +Z. The BOTTOM
//! edge is v0(-1,-1) -> v1(1,-1); an ortho top-down projection makes screen == world XY, so the
//! 3D split param s = (x+1)/2 along the bottom edge.

use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};
use core_math::knife::knife_cut_screen;
use core_math::poly_mesh::raster_data_ngon;

/// Orthographic top-down projection: screen == world XY.
fn ortho_xy(p: Vec3) -> Option<Vec2> {
    Some(Vec2::new(p.x, p.y))
}

/// A single CCW quad, [-1,1]^2 in the z=0 plane. Bottom edge = v0(-1,-1) -> v1(1,-1).
fn quad() -> HalfEdgeMesh {
    let pos = vec![
        Vec3::new(-1.0, -1.0, 0.0), // v0
        Vec3::new( 1.0, -1.0, 0.0), // v1
        Vec3::new( 1.0,  1.0, 0.0), // v2
        Vec3::new(-1.0,  1.0, 0.0), // v3
    ];
    let faces = vec![vec![0usize, 1, 2, 3]];
    HalfEdgeMesh::from_polygons(&pos, &faces)
}

/// Sorted x-coords of ACTIVE vertices lying strictly ON the bottom edge (y=-1, z=0, x in (-1,1)).
/// These are exactly the new vertices the knife progressively split into the bottom edge — the
/// two original endpoints (x = ±1) are excluded by the strict interior bound.
fn bottom_edge_split_xs(m: &HalfEdgeMesh) -> Vec<f32> {
    let mut xs: Vec<f32> = Vec::new();
    for i in 0..m.vertices.len() as u32 {
        let v = m.vert(VertexId(i));
        if v.half_edge == NO_EDGE {
            continue; // freed slot
        }
        let p = v.position;
        if (p.y + 1.0).abs() < 1e-4 && p.z.abs() < 1e-4 && p.x > -0.9999 && p.x < 0.9999 {
            xs.push(p.x);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs
}

/// No surviving face may carry a repeated vertex — the tell-tale of a self-intersecting tangle
/// that the ear-clipper would fan into a triangle spray. (No slits are ever produced in this
/// family, so ANY dup-vertex face here is a genuine tangle.)
fn no_dup_vertex_face(m: &HalfEdgeMesh) -> bool {
    use std::collections::HashSet;
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(fid);
        let mut seen = HashSet::new();
        if vs.iter().any(|v| !seen.insert(v.0)) {
            return false;
        }
    }
    true
}

/// Is there an active vertex within 1e-3 of `p`?
fn has_vert_near(m: &HalfEdgeMesh, p: Vec3) -> bool {
    (0..m.vertices.len() as u32).any(|i| {
        let v = m.vert(VertexId(i));
        v.half_edge != NO_EDGE && (v.position - p).length() < 1e-3
    })
}

/// Render tessellation MUST NOT panic, and must return a well-formed, non-empty index list.
fn assert_raster_ok(m: &HalfEdgeMesh) {
    let (verts, tris) = raster_data_ngon(m);
    assert!(!verts.is_empty(), "raster_data_ngon vert list must be non-empty");
    assert!(!tris.is_empty(), "raster_data_ngon index list must be non-empty");
    assert_eq!(tris.len() % 3, 0, "raster_data_ngon index list must be a multiple of 3");
}

/// Assert a strictly-increasing set of x-coords is well separated (genuinely distinct vertices,
/// NOT a merged/near-degenerate pair). `min_sep` in world units.
fn assert_distinct(xs: &[f32], min_sep: f32) {
    for w in xs.windows(2) {
        assert!(
            (w[1] - w[0]) > min_sep,
            "adjacent split vertices too close ({} vs {}) — merged or degenerate, sep {}",
            w[0], w[1], w[1] - w[0]
        );
    }
}

// =====================================================================================
// TEST 1 — the faithful sawtooth: 4 crossings on the bottom edge WITH interior apexes.
//
// polyline (out->in->out->in->out) so every INSIDE excursion has its apex recorded as an
// interior cut point (poly_3d supplied). The two interior dips carve two triangular notches
// out of the bottom edge; the two OUTSIDE dips (between an in-apex and the next) connect two
// edge-adjacent crossings with no interior -> correctly rejected (a straight 2-gon).
//   crossings on the bottom edge at x = -0.7, -0.5, -0.3, -0.1  (s = 0.15,0.25,0.35,0.45)
//   interior apexes p1=(-0.6,-0.5), p3=(-0.2,-0.5)
// Expected: 1 quad -> 3 faces (two notch triangles + the remaining n-gon), 2 cut edges,
// 4 distinct edge vertices, both apexes materialised, watertight, no tangle.
// =====================================================================================
#[test]
fn sawtooth_four_crossings_with_apexes_watertight() {
    let mut m = quad();
    let f0 = m.face_count();

    let poly = [
        Vec2::new(-0.8, -1.5), // p0 OUT
        Vec2::new(-0.6, -0.5), // p1 IN  (apex)
        Vec2::new(-0.4, -1.5), // p2 OUT
        Vec2::new(-0.2, -0.5), // p3 IN  (apex)
        Vec2::new( 0.0, -1.5), // p4 OUT
    ];
    let poly_3d = [
        Vec3::new(-0.8, -1.5, 0.0),
        Vec3::new(-0.6, -0.5, 0.0),
        Vec3::new(-0.4, -1.5, 0.0),
        Vec3::new(-0.2, -0.5, 0.0),
        Vec3::new( 0.0, -1.5, 0.0),
    ];

    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
    eprintln!("[T1] cuts={cuts} face_count={} bottom_xs={:?}", m.face_count(), bottom_edge_split_xs(&m));
    if let Err(e) = m.validate() {
        eprintln!("[T1] VALIDATE ERROR: {e}");
        m.debug_cube_topology();
    }

    assert!(m.validate().is_ok(), "[T1] mesh must be valid after the sawtooth cut");
    assert!(no_dup_vertex_face(&m), "[T1] no self-intersecting (dup-vertex) face");

    // Progressive multi-split: 4 crossings on ONE edge -> 4 distinct, correctly ordered vertices.
    let xs = bottom_edge_split_xs(&m);
    assert_eq!(xs.len(), 4, "[T1] the crossed edge must gain exactly 4 new vertices, got {xs:?}");
    assert_distinct(&xs, 0.05);
    for (got, want) in xs.iter().zip([-0.7, -0.5, -0.3, -0.1]) {
        assert!((got - want).abs() < 1e-3, "[T1] edge vertex {got} != expected {want}");
    }

    // The two interior apexes became real vertices.
    assert!(has_vert_near(&m, Vec3::new(-0.6, -0.5, 0.0)), "[T1] apex p1 must be a real vertex");
    assert!(has_vert_near(&m, Vec3::new(-0.2, -0.5, 0.0)), "[T1] apex p3 must be a real vertex");

    // Two interior dips -> two through-cuts -> two extra faces. The two outside dips split nothing.
    assert_eq!(cuts, 2, "[T1] exactly the two interior dips should cut (no dropped/spurious cut)");
    assert_eq!(m.face_count(), f0 + 2, "[T1] quad -> 3 faces (two notch triangles + remainder)");

    assert_raster_ok(&m);
}

// =====================================================================================
// TEST 2 — near-coincident crossings on the same edge MUST MERGE (not spawn a zero-length edge).
//
// Two segments cross the bottom edge at x=0.0 (s=0.5) and x=0.0002 (s=0.5001) — a 1e-4 gap in
// the edge param, well under VEND=1e-3. The progressive split must reuse the first split vertex
// for the second crossing (knife.rs:264), so the edge gains exactly ONE vertex, and validate
// stays green (no degenerate zero-length edge).
// =====================================================================================
#[test]
fn near_coincident_crossings_merge_to_one_vertex() {
    let mut m = quad();
    let f0 = m.face_count();

    // out->in->out is NOT needed; a simple in->out->in produces two crossings ~1e-4 apart.
    let poly = [
        Vec2::new(-0.0001, -0.5), // IN
        Vec2::new( 0.0001, -1.5), // OUT  (seg0 crosses at x=0.0)
        Vec2::new( 0.0003, -0.5), // IN   (seg1 crosses at x=0.0002)
    ];
    // poly_3d = None on purpose: isolate the edge-crossing/merge path (no interior points).
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
    let xs = bottom_edge_split_xs(&m);
    eprintln!("[T2] cuts={cuts} face_count={} bottom_xs={:?}", m.face_count(), xs);

    assert!(m.validate().is_ok(), "[T2] merge must NOT create a zero-length edge that breaks validate");
    assert!(no_dup_vertex_face(&m), "[T2] no tangled face");
    assert_eq!(
        xs.len(), 1,
        "[T2] two crossings 1e-4 apart must MERGE into ONE edge vertex, got {xs:?}"
    );
    assert!((xs[0] - 0.0).abs() < 1e-2, "[T2] the merged vertex sits at the first split (~x=0.0), got {}", xs[0]);
    assert_eq!(m.face_count(), f0, "[T2] a grazing merge splits no face");
    assert_raster_ok(&m);
}

// =====================================================================================
// TEST 3 — pure progressive multi-split, MONOTONIC path order, poly_3d = None.
//
// The same 4-crossing sawtooth as T1 but WITHOUT interior apexes. This isolates the
// crossing-only progressive split: 4 crossings on ONE edge must still create 4 distinct,
// correctly-ordered vertices. With no interior apex to break collinearity, each pair of
// edge-adjacent crossings connects as a straight 2-gon -> correctly rejected -> the face is
// untouched (documented crossing-only behaviour), but the 4 vertices are real.
// =====================================================================================
#[test]
fn pure_four_crossings_monotonic_makes_four_vertices() {
    let mut m = quad();
    let f0 = m.face_count();

    let poly = [
        Vec2::new(-0.8, -1.5),
        Vec2::new(-0.6, -0.5),
        Vec2::new(-0.4, -1.5),
        Vec2::new(-0.2, -0.5),
        Vec2::new( 0.0, -1.5),
    ];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
    let xs = bottom_edge_split_xs(&m);
    eprintln!("[T3] cuts={cuts} face_count={} bottom_xs={:?}", m.face_count(), xs);

    assert!(m.validate().is_ok(), "[T3] valid after 4-crossing progressive split");
    assert!(no_dup_vertex_face(&m), "[T3] no tangled face");
    assert_eq!(xs.len(), 4, "[T3] the crossed edge must gain exactly 4 new vertices, got {xs:?}");
    assert_distinct(&xs, 0.05);
    for (got, want) in xs.iter().zip([-0.7, -0.5, -0.3, -0.1]) {
        assert!((got - want).abs() < 1e-3, "[T3] edge vertex {got} != expected {want}");
    }
    // No apex recorded -> the grazing sawtooth carves nothing; face count is unchanged.
    assert_eq!(m.face_count(), f0, "[T3] crossing-only grazing sawtooth splits no face");
    assert_raster_ok(&m);
}

// =====================================================================================
// TEST 4 — SCRAMBLED path order (adversarial): 4 crossings on ONE edge whose PATH order differs
// from their EDGE order, poly_3d = None. This stresses the decoupling of step-2 (group-by-edge,
// split in ascending edge param) from step-3 (connect in path order). The split parameters must
// be computed against the ORIGINAL edge before any split invalidates endpoints — if that were
// wrong, positions would drift or validate would fail.
//   crossings (path order): x = 0.4, 0.0, -0.4, -0.1   (edge params 0.7,0.5,0.3,0.45)
//   -> 4 distinct vertices at x = -0.4,-0.1,0.0,0.4 in edge order.
// Invariants that MUST hold regardless of the connect topology: validate ok, no dup-vertex
// tangle, raster no-panic, and the 4 distinct correctly-placed vertices.
// =====================================================================================
#[test]
fn scrambled_order_four_crossings_progressive_split() {
    let mut m = quad();

    let poly = [
        Vec2::new( 0.4, -0.5), // IN
        Vec2::new( 0.4, -1.5), // OUT (seg0 crosses x=0.4)
        Vec2::new(-0.4, -0.5), // IN  (seg1 crosses x=0.0)
        Vec2::new(-0.4, -1.5), // OUT (seg2 crosses x=-0.4)
        Vec2::new( 0.2, -0.5), // IN  (seg3 crosses x=-0.1)
    ];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, None, false, true, Vec3::NEG_Z, None, None);
    let xs = bottom_edge_split_xs(&m);
    eprintln!("[T4] cuts={cuts} face_count={} bottom_xs={:?}", m.face_count(), xs);

    assert!(m.validate().is_ok(), "[T4] valid after scrambled-order progressive split");
    assert!(no_dup_vertex_face(&m), "[T4] no self-intersecting (dup-vertex) tangle");

    // Split params were computed against the ORIGINAL edge, so despite scrambled path order the
    // 4 vertices land in correct EDGE order at the expected positions.
    assert_eq!(xs.len(), 4, "[T4] the crossed edge must gain exactly 4 new vertices, got {xs:?}");
    assert_distinct(&xs, 0.05);
    for (got, want) in xs.iter().zip([-0.4, -0.1, 0.0, 0.4]) {
        assert!((got - want).abs() < 1e-3, "[T4] edge vertex {got} != expected {want}");
    }
    assert_raster_ok(&m);
}

// =====================================================================================
// TEST 5 — ROOT-CAUSE ISOLATION (not multi-crossing): two SEPARATE through-cuts where the
// SECOND lands on a face produced by the FIRST. No sawtooth, no collinear chain on one edge —
// each cut spans left-edge -> interior apex -> right-edge on a different height. If this ALSO
// corrupts the twin linkage, the T1 failure is NOT about "many crossings on one edge" per se; it
// is `cut_through_interior`'s delete_face + create_face rebuild leaving broken twins whenever a
// second cut is applied to a face the first cut created.
//
// Baseline: a SINGLE such cut on this quad must be valid (existing engine behaviour). The second
// cut is the regression. This test documents the engine defect (it FAILS on the corrupt engine).
// =====================================================================================
#[test]
fn sequential_cuts_on_child_face_stay_watertight() {
    let mut m = quad();

    // Cut A: left edge -> apex(0,0.4) -> right edge. Splits the quad into a top and bottom face.
    let poly_a = [Vec2::new(-1.2, 0.3), Vec2::new(0.0, 0.4), Vec2::new(1.2, 0.3)];
    let poly_a3 = [Vec3::new(-1.0, 0.3, 0.0), Vec3::new(0.0, 0.4, 0.0), Vec3::new(1.0, 0.3, 0.0)];
    let cuts_a = knife_cut_screen(&mut m, &ortho_xy, &poly_a, Some(&poly_a3), false, true, Vec3::NEG_Z, None, None);
    eprintln!("[T5] after cut A: cuts={cuts_a} faces={} valid={:?}", m.face_count(), m.validate());
    assert!(m.validate().is_ok(), "[T5] a SINGLE apex cut must be valid (engine baseline)");
    assert_eq!(m.face_count(), 2, "[T5] cut A splits the quad into 2 faces");

    // Cut B: left edge -> apex(0,-0.2) -> right edge, landing inside the BOTTOM child face.
    let poly_b = [Vec2::new(-1.2, -0.3), Vec2::new(0.0, -0.2), Vec2::new(1.2, -0.3)];
    let poly_b3 = [Vec3::new(-1.0, -0.3, 0.0), Vec3::new(0.0, -0.2, 0.0), Vec3::new(1.0, -0.3, 0.0)];
    let cuts_b = knife_cut_screen(&mut m, &ortho_xy, &poly_b, Some(&poly_b3), false, true, Vec3::NEG_Z, None, None);
    eprintln!("[T5] after cut B: cuts={cuts_b} faces={} valid={:?}", m.face_count(), m.validate());
    if let Err(e) = m.validate() {
        eprintln!("[T5] VALIDATE ERROR (second cut on child face): {e}");
    }
    if !no_dup_vertex_face(&m) {
        eprintln!("[T5] TANGLE: a surviving face carries a duplicated vertex");
        m.debug_cube_topology();
    }

    assert!(m.validate().is_ok(), "[T5] a second cut on the first cut's child face must stay valid");
    assert!(no_dup_vertex_face(&m), "[T5] no tangled face after sequential cuts");
    assert_eq!(m.face_count(), 3, "[T5] cut B splits the bottom child -> 3 faces");
    assert_raster_ok(&m);
}
