//! Adversarial knife stress — FAMILY: BEVELED-CUBE WRAPPING CUT (the user-reported scenario).
//!
//! Vertex-bevel the (1,1,1) corner of `new_box(Vec3::ONE)` with width 0.4 so the three faces at that
//! corner become PENTAGONS. The top (+Y) and right (+X) faces are pentagons that share the fold edge
//! {(1,1,0.6)-(1,1,-1)}. We view the cube isometrically (both faces camera-facing) and author knife
//! polylines that wrap over that shared edge with interior clicks on BOTH faces.
//!
//! RESULT (see the three tests):
//!   * `bevelwrap_setup_makes_pentagons`   — sanity: the bevel really produces the pentagons.       PASS
//!   * `bevelwrap_plain_wrap_stays_valid`  — a plain wrap (dangling ends → two slits) is watertight. PASS
//!   * `bevelwrap_uturn_over_fold_edge_keeps_mesh_valid`
//!                                         — the U-turn variant (dip into one face, return over the
//!                                           SAME fold edge) CORRUPTS the mesh: validate() fails with
//!                                           "twin.twin != self". This test asserts the invariant that
//!                                           SHOULD hold and therefore fails RED — it pins the bug.

use glam::{Vec2, Vec3};
use std::collections::HashSet;
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};
use core_math::bevel_op::{bevel_vertices_offset, BevelParams};
use core_math::knife::knife_cut_screen;
use core_math::poly_mesh::raster_data_ngon;

// ---------------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------------

/// Vertex-bevel the (1,1,1) corner of the unit cube into pentagons (the reported n-gon setup).
fn beveled_corner_cube() -> HalfEdgeMesh {
    let cube = HalfEdgeMesh::new_box(Vec3::ONE);
    let mut vi = None;
    for i in 0..cube.vertex_count() as u32 {
        if (cube.vert(VertexId(i)).position - Vec3::ONE).length() < 1e-4 { vi = Some(i); break; }
    }
    let mut sel = HashSet::new();
    sel.insert(vi.expect("corner (1,1,1) vertex"));
    bevel_vertices_offset(&cube, &sel, &BevelParams { width: 0.4, segments: 1, shape: 0.5, clamp: true })
}

/// First live face whose normal aligns with `target_n` (dot > 0.99) and has `want_verts` sides.
fn find_face(m: &HalfEdgeMesh, target_n: Vec3, want_verts: usize) -> Option<FaceId> {
    for i in 0..m.faces.len() as u32 {
        let f = FaceId(i);
        if m.face(f).half_edge == NO_EDGE { continue; }
        let vs = m.face_vertices(f);
        if vs.len() == want_verts && m.compute_face_normal(f).dot(target_n) > 0.99 { return Some(f); }
    }
    None
}

/// Orthonormal isometric projection basis looking along `view_dir`; both the top and side faces project
/// with non-degenerate area, so a wrapping cut over their shared edge is expressible in screen space.
fn iso_basis(view_dir: Vec3) -> (Vec3, Vec3) {
    let f = view_dir.normalize();
    let world_up = if f.dot(Vec3::Y).abs() > 0.95 { Vec3::Z } else { Vec3::Y };
    let right = world_up.cross(f).normalize();
    let up = f.cross(right).normalize();
    (right, up)
}

fn vert_exists_near(m: &HalfEdgeMesh, p: Vec3) -> bool {
    (0..m.vertices.len() as u32).any(|v| {
        m.vert(VertexId(v)).half_edge != NO_EDGE && (m.vert(VertexId(v)).position - p).length() < 1e-3
    })
}

/// Raster invariant (#3): must not panic, returns a non-empty multiple-of-3 index list.
fn check_raster(m: &HalfEdgeMesh, tag: &str) {
    let (verts, tris) = raster_data_ngon(m);
    assert!(!verts.is_empty(), "{tag}: raster verts non-empty");
    assert!(!tris.is_empty(), "{tag}: raster tris non-empty");
    assert_eq!(tris.len() % 3, 0, "{tag}: triangle index list well-formed (multiple of 3)");
}

/// Compact face-ring dump for failure evidence.
fn dump_faces(m: &HalfEdgeMesh) -> String {
    let mut s = String::new();
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE { continue; }
        let ring: Vec<u32> = m.face_vertices(fid).iter().map(|v| v.0).collect();
        s.push_str(&format!("\n    face {f}: {ring:?}"));
    }
    s
}

// A cube viewed from the (1,1,1) octant: top +Y, right +X and front +Z are all camera-facing.
const VIEW_DIR: Vec3 = Vec3::new(-1.0, -1.0, -1.0);

// ---------------------------------------------------------------------------------------------------
// scenario 0 — sanity: the bevel really produced pentagons that share a fold edge.
// ---------------------------------------------------------------------------------------------------
#[test]
fn bevelwrap_setup_makes_pentagons() {
    let m = beveled_corner_cube();
    assert!(m.validate().is_ok(), "beveled cube must be valid: {:?}", m.validate().err());
    // vertex bevel of one corner: 3 corner faces → pentagons, 3 far faces stay quads, +1 triangular cap = 7.
    assert_eq!(m.face_count(), 7, "one-corner vertex bevel → 7 faces{}", dump_faces(&m));
    assert!(find_face(&m, Vec3::Y, 5).is_some(), "top (+Y) is a pentagon");
    assert!(find_face(&m, Vec3::X, 5).is_some(), "right (+X) is a pentagon");
    assert!(find_face(&m, Vec3::Z, 5).is_some(), "front (+Z) is a pentagon");
    check_raster(&m, "setup");
}

// ---------------------------------------------------------------------------------------------------
// scenario A — the plain wrap: start on the top pentagon interior, wrap over the shared top/right edge,
// end on the right pentagon interior. Both endpoints are dangling → two slits from the shared crossing.
// This is the baseline the engine handles WATERTIGHT (documents the harness is sound).
// ---------------------------------------------------------------------------------------------------
#[test]
fn bevelwrap_plain_wrap_stays_valid() {
    let mut m = beveled_corner_cube();
    let v_before = m.vertex_count();
    let (rb, ub) = iso_basis(VIEW_DIR);
    let proj = |p: Vec3| -> Option<Vec2> { Some(Vec2::new(p.dot(rb), p.dot(ub))) };

    // interior points hugging the shared fold edge (short, local stroke — no spurious far-face crossings).
    let t_in = Vec3::new(0.85, 1.0, 0.0); // inside the top pentagon
    let r_in = Vec3::new(1.0, 0.85, 0.0); // inside the right pentagon
    let poly = [proj(t_in).unwrap(), proj(r_in).unwrap()];
    let poly_3d = [t_in, r_in];
    let cuts = knife_cut_screen(&mut m, &proj, &poly, Some(&poly_3d), false, false, VIEW_DIR, None, None);

    assert!(cuts >= 1, "plain wrap over the shared edge must create geometry, got {cuts}");
    assert!(m.validate().is_ok(), "plain wrap must keep the mesh valid: {:?}{}", m.validate().err(), dump_faces(&m));
    check_raster(&m, "plain_wrap");
    assert!(m.vertex_count() > v_before, "the shared-edge crossing + slit tips added vertices");
    // invariant #5: both interior clicks are materialised as real vertices.
    assert!(vert_exists_near(&m, t_in), "top interior click became a real vertex");
    assert!(vert_exists_near(&m, r_in), "right interior click became a real vertex");
}

// ---------------------------------------------------------------------------------------------------
// scenario C — THE BUG (U-turn variant): dip into ONE face and return over the SAME fold edge.
// Start in the top pentagon, cross the shared fold edge into the right pentagon, dip to a right-interior
// point, cross the SAME fold edge back, end in the top pentagon. The shared edge is crossed TWICE.
//
// The stroke is deliberately TINY and hugs the shared edge, so it provably crosses ONLY that edge (no
// spurious far-face cuts). The identical U-turn over a *coplanar* edge is watertight (verified during
// investigation); it is the FOLD (two non-coplanar pentagons sharing the crossed edge) that breaks the
// engine: cut_through_interior splits the adjacent face while cut_slit rebuilds this face twice over the
// same split vertices, and HalfEdgeMesh::create_face's first-match twin re-pairing cannot re-establish
// twin symmetry across the overlapping rebuilds → the mesh ends up NON-MANIFOLD.
//
// This test asserts the invariant that SHOULD hold (validate() ok) and therefore fails RED, pinning the
// bug. (validate() failing == data corruption == CLAUDE.md priority-1.)
// ---------------------------------------------------------------------------------------------------
#[test]
fn bevelwrap_uturn_over_fold_edge_keeps_mesh_valid() {
    let mut m = beveled_corner_cube();
    let (rb, ub) = iso_basis(VIEW_DIR);
    let proj = |p: Vec3| -> Option<Vec2> { Some(Vec2::new(p.dot(rb), p.dot(ub))) };

    let a_top = Vec3::new(0.90, 1.0, -0.10); // top interior, hugging the shared fold edge
    let dip = Vec3::new(1.0, 0.90, -0.20);   // right interior, hugging the shared fold edge (the dip)
    let b_top = Vec3::new(0.90, 1.0, -0.30); // top interior, hugging the shared fold edge
    let poly_3d = [a_top, dip, b_top];
    let poly: Vec<Vec2> = poly_3d.iter().map(|&p| proj(p).unwrap()).collect();

    let cuts = knife_cut_screen(&mut m, &proj, &poly, Some(&poly_3d), false, false, VIEW_DIR, None, None);
    assert!(cuts >= 1, "the U-turn must attempt a cut, got {cuts}");

    // Raster still does not panic even though the topology is corrupt (invariant #3 holds).
    check_raster(&m, "uturn");

    // THE INVARIANT THAT FAILS: the knife must never leave the mesh non-manifold.
    let res = m.validate();
    assert!(
        res.is_ok(),
        "ENGINE BUG: U-turn wrap over the beveled fold edge left the half-edge mesh INVALID: {:?}\
         \n  (the same U-turn over a coplanar edge is fine — the fold triggers a twin-pairing corruption)\
         \n  post-cut faces:{}",
        res.err(),
        dump_faces(&m),
    );
}
