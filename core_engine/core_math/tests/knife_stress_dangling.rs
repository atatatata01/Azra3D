//! Adversarial knife-cut verification — FAMILY: DANGLING BOTH ENDS + DEGENERATE.
//!
//! (a) A cut that starts INSIDE a face and ends INSIDE a (possibly different) face, crossing an edge in
//!     between → both ends dangle (leading + trailing slits) with a through-section in the middle.
//! (b) A cut ENTIRELY inside one face, only interior points, NO edge crossings (no boundary anchor) →
//!     must be a graceful no-op (no corruption, no free-floating garbage).
//! (c) A single interior point only → validate ok + raster no-panic.
//!
//! The concern under test: an interior run with no `Bound` anchor must NEVER corrupt the mesh.
//! We only touch the PUBLIC API + write ONLY this test file (knife.rs / src stay untouched).

use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};
use core_math::knife::knife_cut_screen;

/// Orthographic top-down projection (world XY == screen), same as the in-crate flat-grid test path.
fn ortho_xy(p: Vec3) -> Option<Vec2> {
    Some(Vec2::new(p.x, p.y))
}

/// N×N flat quad grid in the XY plane at z=0, spanning [-0.5, 0.5]^2. Open mesh (all border edges are
/// boundary). Mirrors the private `grid()` helper inside knife.rs so results are directly comparable.
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

/// Is there a LIVE (wired) vertex within 1e-3 of `pos`?  Interior click points must materialise into
/// real, wired mesh vertices.
fn find_vertex(m: &HalfEdgeMesh, pos: Vec3) -> bool {
    (0..m.vertices.len() as u32).any(|v| {
        m.vert(VertexId(v)).half_edge != NO_EDGE
            && (m.vert(VertexId(v)).position - pos).length() < 1e-3
    })
}

/// (# faces carrying a repeated vertex, max times any single vertex repeats within one face).
///
/// A slit spur legitimately repeats its anchor EXACTLY twice (`.. a, tip, a ..`). A self-intersecting
/// through-cut tangle (the ear-clip fan bug) shows up either as validate() failing, as extra dup-faces,
/// or as a vertex appearing 3+ times — so `max_repeat >= 3` (or an unexpected dup-face count) is the tell.
fn dup_stats(m: &HalfEdgeMesh) -> (usize, usize) {
    use std::collections::HashMap;
    let mut dup_faces = 0usize;
    let mut max_repeat = 0usize;
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(fid);
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for v in &vs {
            *counts.entry(v.0).or_insert(0) += 1;
        }
        let mx = counts.values().copied().max().unwrap_or(0);
        if mx > 1 {
            dup_faces += 1;
        }
        max_repeat = max_repeat.max(mx);
    }
    (dup_faces, max_repeat)
}

/// Display tessellation MUST NOT panic / hang and must yield a well-formed (non-empty, %3==0) index list.
fn assert_raster_ok(m: &HalfEdgeMesh) {
    let (verts, tris) = core_math::poly_mesh::raster_data_ngon(m);
    assert!(!verts.is_empty(), "raster vertex list non-empty");
    assert!(!tris.is_empty(), "raster index list non-empty");
    assert_eq!(tris.len() % 3, 0, "raster index list well-formed (multiple of 3)");
}

// =====================================================================================================
// (a) BOTH ENDS DANGLE
// =====================================================================================================

/// (a1) interior(face A) → cross ONE shared edge → interior(face B). Two slits SHARE the crossing vertex
/// but grow into two DIFFERENT faces. The trailing slit must still find its face after the leading slit
/// deleted+rebuilt the neighbour across the shared edge (twin re-pairing must survive).
#[test]
fn dangling_both_ends_single_crossing() {
    let mut m = grid(2); // 2x2 quads; shared vertical edge at x=0 between cells (0,0) and (1,0)
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let pa = Vec3::new(-0.25, -0.25, 0.0); // interior to cell (0,0)
    let pb = Vec3::new(0.25, -0.25, 0.0);  // interior to cell (1,0)
    let poly = [Vec2::new(pa.x, pa.y), Vec2::new(pb.x, pb.y)];
    let poly_3d = [pa, pb];

    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);

    assert!(m.validate().is_ok(), "INVARIANT-1 validate: {:?}", m.validate());
    assert_raster_ok(&m);
    // Two slits, each net-zero on faces (one polygon + spur), in two different faces → face count fixed.
    assert_eq!(m.face_count(), f0, "INVARIANT-4 two same-anchor slits do not change face count");
    // +1 crossing vertex (shared), +2 interior slit tips.
    assert_eq!(m.vertex_count(), v0 + 3, "INVARIANT-5 crossing + 2 tips materialised");
    assert!(find_vertex(&m, pa), "leading interior tip became a real vertex");
    assert!(find_vertex(&m, pb), "trailing interior tip became a real vertex");
    let (dup_faces, max_rep) = dup_stats(&m);
    assert_eq!(max_rep, 2, "INVARIANT-2 no 3+-repeat tangle face");
    assert_eq!(dup_faces, 2, "INVARIANT-2 exactly the two slit faces carry a (spur) dup");
    assert_eq!(cuts, 2, "leading + trailing slit each report one cut edge, got {cuts}");
}

/// (a2) interior(A) → cross → cross → interior(C): leading slit (into A) + a clean THROUGH cut (splits the
/// middle face B) + trailing slit (into C). Three distinct faces; the middle must split cleanly (no tangle).
#[test]
fn dangling_both_ends_through_middle() {
    let mut m = grid(3); // 3x3; middle row cells (0,1),(1,1),(2,1)
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let pa = Vec3::new(-0.333, 0.0, 0.0); // interior to cell (0,1)
    let pc = Vec3::new(0.333, 0.0, 0.0);  // interior to cell (2,1)
    let poly = [Vec2::new(pa.x, pa.y), Vec2::new(pc.x, pc.y)];
    let poly_3d = [pa, pc];

    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);

    assert!(m.validate().is_ok(), "INVARIANT-1 validate: {:?}", m.validate());
    assert_raster_ok(&m);
    // leading slit (net 0) + middle through cut (+1) + trailing slit (net 0).
    assert_eq!(m.face_count(), f0 + 1, "INVARIANT-4 exactly the middle face splits");
    // 2 edge crossings + 2 interior slit tips.
    assert_eq!(m.vertex_count(), v0 + 4, "INVARIANT-5 2 crossings + 2 tips");
    assert!(find_vertex(&m, pa), "leading tip materialised");
    assert!(find_vertex(&m, pc), "trailing tip materialised");
    let (dup_faces, max_rep) = dup_stats(&m);
    assert_eq!(max_rep, 2, "INVARIANT-2 through-cut faces clean; only slit anchors dup");
    assert_eq!(dup_faces, 2, "INVARIANT-2 the 2 through faces are clean; only the 2 slits dup");
    assert!(cuts >= 3, "2 slits + 1 through cut, got {cuts}");
}

/// (a3) interior → OUT over the top edge → back IN, ending interior — both ends dangle into the SAME quad,
/// re-crossing the SAME edge twice. The 2-gon "middle" between the two same-edge crossings must be dropped
/// (not fanned into a self-intersecting spray); the one quad ends up carrying BOTH slit spurs.
#[test]
fn dangling_both_ends_same_face_uturn() {
    let mut m = grid(1); // single quad [-0.5,0.5]^2
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let pa = Vec3::new(-0.2, 0.2, 0.0);  // interior
    let pmid = Vec3::new(0.0, 0.7, 0.0); // OUTSIDE (above top edge) — a bend, not an interior click
    let pb = Vec3::new(0.2, 0.2, 0.0);   // interior
    let poly = [Vec2::new(pa.x, pa.y), Vec2::new(pmid.x, pmid.y), Vec2::new(pb.x, pb.y)];
    let poly_3d = [pa, pmid, pb];

    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);

    assert!(m.validate().is_ok(), "INVARIANT-1 validate: {:?}", m.validate());
    assert_raster_ok(&m); // the double-spur face MUST still tessellate (stall-guard fan), no panic
    assert!(find_vertex(&m, pa), "leading tip materialised");
    assert!(find_vertex(&m, pb), "trailing tip materialised");
    let (_dup_faces, max_rep) = dup_stats(&m);
    assert!(max_rep <= 2, "INVARIANT-2 no vertex appears 3+ times in a face (no ear-clip tangle)");
    // Same-face double slit: two spurs on the one quad, degenerate 2-gon middle dropped.
    assert_eq!(m.face_count(), f0, "INVARIANT-4 same-face double slit: face count unchanged");
    assert_eq!(m.vertex_count(), v0 + 4, "INVARIANT-5 2 same-edge crossings + 2 tips");
    assert!(cuts >= 1, "at least one slit was made, got {cuts}");
}

// =====================================================================================================
// (b) ALL-INTERIOR, NO EDGE CROSSING — the basic 2-click cut inside one face. Was pinned as a no-op
//     (the "basic cut does not register" bug). Blender parity now: each free endpoint connects to ITS
//     OWN nearest boundary vertex → a chord that SPLITS the face into two n-gons (not a dangling slit —
//     a slit's wire edge later breaks the bevel). Corruption invariants unchanged and still hard.
// =====================================================================================================

#[test]
fn all_interior_no_crossing_splits_ngon() {
    let mut m = grid(1);
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let pa = Vec3::new(-0.1, 0.0, 0.0); // both interior to the single quad, segment crosses no edge
    let pb = Vec3::new(0.1, 0.0, 0.0);
    let poly = [Vec2::new(pa.x, pa.y), Vec2::new(pb.x, pb.y)];
    let poly_3d = [pa, pb];

    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);

    assert!(cuts >= 1, "interior-only cut must create geometry, got {cuts}");
    assert!(m.validate().is_ok(), "INVARIANT-1 validate after n-gon split: {:?}", m.validate());
    assert_eq!(m.face_count(), f0 + 1, "chord split: one quad → two faces");
    // Both clicked points materialise; the anchors are EXISTING ring vertices (no orphan allocations).
    assert_eq!(m.vertex_count(), v0 + 2, "exactly the 2 clicked points became vertices");
    assert!(find_vertex(&m, pa), "first interior click materialised");
    assert!(find_vertex(&m, pb), "second interior click materialised");
    assert_raster_ok(&m);
    let (dup_faces, max_rep) = dup_stats(&m);
    assert_eq!(dup_faces, 0, "clean chord split — no slit/tangle face");
    assert!(max_rep <= 2, "INVARIANT-2 no vertex appears 3+ times in a face (no ear-clip tangle)");
}

// =====================================================================================================
// (c) SINGLE INTERIOR POINT ONLY — degenerate; two encodings, both must be safe no-ops.
// =====================================================================================================

#[test]
fn single_interior_point_noop() {
    let p = Vec3::new(0.0, 0.0, 0.0); // dead-center interior of the quad

    // c1: polyline of length 1 → immediate reject (poly.len() < 2).
    {
        let mut m = grid(1);
        let (f0, v0) = (m.face_count(), m.vertex_count());
        let poly = [Vec2::new(p.x, p.y)];
        let poly_3d = [p];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert_eq!(cuts, 0, "1-point polyline rejected");
        assert!(m.validate().is_ok(), "INVARIANT-1 validate (c1)");
        assert_eq!((m.face_count(), m.vertex_count()), (f0, v0), "1-point: mesh untouched");
        assert_raster_ok(&m);
    }

    // c2: polyline = [p, p] (one interior click duplicated) → no crossings, no anchor → no-op.
    {
        let mut m = grid(1);
        let (f0, v0) = (m.face_count(), m.vertex_count());
        let poly = [Vec2::new(p.x, p.y), Vec2::new(p.x, p.y)];
        let poly_3d = [p, p];
        let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), false, true, Vec3::NEG_Z, None, None);
        assert_eq!(cuts, 0, "single interior point (duplicated) is a no-op");
        assert!(m.validate().is_ok(), "INVARIANT-1 validate (c2)");
        assert_eq!((m.face_count(), m.vertex_count()), (f0, v0), "1-point-dup: zero vertices allocated");
        assert_raster_ok(&m);
        let (dup_faces, _) = dup_stats(&m);
        assert_eq!(dup_faces, 0, "no-op: no faces mutated");
    }
}
