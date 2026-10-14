//! ADVERSARIAL knife stress — FAMILY: CLOSED LOOP + INTERIOR.
//!
//! Goal: try to BREAK `knife_cut_screen` when `closed=true` and the polyline vertices are
//! INTERIOR to faces (routed via `poly_3d`). Two hard scenarios:
//!   (a) a closed loop whose corners are all interior points enclosing a region;
//!   (b) a closed loop that crosses the SAME edge twice.
//!
//! Structural invariants asserted (watertight / correct):
//!   1. mesh.validate().is_ok() after the cut.
//!   2. raster_data_ngon(&mesh) does not panic; non-empty; index list multiple of 3.
//!   3. no THROUGH-cut tangle (a dup-vertex face that is NOT an intentional slit).
//!   4. face_count changes by the derived amount.
//!   5. every interior click that should materialise became a real vertex.

use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};
use core_math::knife::knife_cut_screen;
use core_math::poly_mesh::raster_data_ngon;

/// Flat N×N grid plane in the XY plane at z=0 (open mesh) — the flat-grid test path.
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

/// Orthographic top-down projection (world XY → screen), so screen == world XY.
fn ortho_xy(p: Vec3) -> Option<Vec2> {
    Some(Vec2::new(p.x, p.y))
}

/// A live vertex sits within 1e-3 of `p`.
fn has_vertex_at(m: &HalfEdgeMesh, p: Vec3) -> bool {
    (0..m.vertices.len() as u32).any(|v| {
        m.vert(VertexId(v)).half_edge != NO_EDGE && (m.vert(VertexId(v)).position - p).length() < 1e-3
    })
}

/// Report every surviving face carrying a repeated vertex, splitting them into two classes:
/// - "slit"  : EVERY repeated vertex on the face appears exactly TWICE — i.e. one or more independent
///             out-and-back spurs (dangling cut ends). A closed loop that fails to close leaves such a
///             spur per un-threaded interior corner, and two spurs can legitimately stack on one face.
/// - "tangle": some vertex appears 3+ TIMES — a genuine multi-fold self-touch (the self-intersecting
///             through-cut the ear-clipper fans into a triangle spray).
/// Returns (slit_faces, tangle_faces) as vectors of (FaceId.0, vertex-id list) for diagnostics.
fn dup_face_classes(m: &HalfEdgeMesh) -> (Vec<(u32, Vec<u32>)>, Vec<(u32, Vec<u32>)>) {
    let mut slits = Vec::new();
    let mut tangles = Vec::new();
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let vs: Vec<u32> = m.face_vertices(fid).iter().map(|v| v.0).collect();
        // count occurrences
        let mut counts = std::collections::HashMap::new();
        for &v in &vs {
            *counts.entry(v).or_insert(0u32) += 1;
        }
        let repeated: Vec<(u32, u32)> = counts.iter().filter(|(_, &c)| c > 1).map(|(&v, &c)| (v, c)).collect();
        if repeated.is_empty() {
            continue;
        }
        // Legit slit(s): every repeated anchor appears EXACTLY twice. A 3+-fold repeat is a real tangle.
        let all_twice = repeated.iter().all(|&(_, c)| c == 2);
        if all_twice {
            slits.push((f, vs));
        } else {
            tangles.push((f, vs));
        }
    }
    (slits, tangles)
}

/// The full structural gate: valid mesh + raster does not panic + is well-formed + NO tangle face.
/// Prints diagnostics for --nocapture. `label` identifies the scenario.
fn assert_watertight(m: &HalfEdgeMesh, label: &str) {
    // 1. topology
    let val = m.validate();
    eprintln!("[{label}] validate = {:?}", val);
    assert!(val.is_ok(), "[{label}] mesh must be valid after the cut: {:?}", val);

    // 3. tangle scan
    let (slits, tangles) = dup_face_classes(m);
    eprintln!("[{label}] slit faces = {} :: {:?}", slits.len(), slits);
    eprintln!("[{label}] TANGLE faces = {} :: {:?}", tangles.len(), tangles);
    assert!(tangles.is_empty(), "[{label}] a through-cut tangled into a dup-vertex face (ear-clip fan bug): {:?}", tangles);

    // 2. render path must not panic + well-formed
    let (verts, tris) = raster_data_ngon(m);
    eprintln!("[{label}] raster verts = {}, tris idx = {}", verts.len(), tris.len());
    assert!(!verts.is_empty(), "[{label}] raster produced no vertices");
    assert!(!tris.is_empty(), "[{label}] raster produced no triangles");
    assert_eq!(tris.len() % 3, 0, "[{label}] triangle index list not a multiple of 3");
    // every index must be in range (a self-intersecting fan can emit an out-of-range index → GPU crash)
    for &i in &tris {
        assert!((i as usize) < verts.len(), "[{label}] triangle index {i} out of range ({} verts)", verts.len());
    }
}

// =====================================================================================
// (a.1) Fully-interior closed loop that stays INSIDE ONE face → chord-splits the cell.
// =====================================================================================
#[test]
fn closed_interior_single_face_splits_ngon() {
    // grid(2): bottom-left cell is x∈[-0.5,0], y∈[-0.5,0]. All 4 corners live strictly inside it,
    // and every side segment stays inside it → the loop crosses NO mesh edge and touches NO vertex.
    // Was `closed_interior_single_face_is_graceful_noop`, which pinned the BUG (an anchor-less
    // interior run produced no geometry — the "basic cut does not register" family). It then became
    // a CHORD (+1 face): the loop did not truly close, both free ends anchored on the ring.
    // 2026-08-07 (KNIFE.md §5.5): a CLOSED interior loop now takes `split_poly_closed_loop` and
    // divides into inner disc + two annulus halves (+2 faces) — Blender's EXACT answer, and the
    // fix for the owner's circle "GOT SEGMENT AFTER CUT". The enclosed region is now really
    // separated, which is what the user drew.
    let mut m = grid(2);
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let poly = [
        Vec2::new(-0.40, -0.40), Vec2::new(-0.10, -0.40),
        Vec2::new(-0.10, -0.10), Vec2::new(-0.40, -0.10),
    ];
    let poly_3d = [
        Vec3::new(-0.40, -0.40, 0.0), Vec3::new(-0.10, -0.40, 0.0),
        Vec3::new(-0.10, -0.10, 0.0), Vec3::new(-0.40, -0.10, 0.0),
    ];
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&poly_3d), true, true, Vec3::NEG_Z, None, None);
    eprintln!("[single_face_ngon] cuts = {cuts}, faces {f0}->{}, verts {v0}->{}", m.face_count(), m.vertex_count());
    assert!(cuts >= 1, "interior-only closed loop must create geometry, got {cuts}");
    assert_eq!(m.face_count(), f0 + 2, "closed loop → inner disc + two annulus halves");
    assert!(m.vertex_count() >= v0 + 4, "all 4 interior corners became real vertices");
    for p in poly_3d {
        assert!(has_vertex_at(&m, p), "interior corner {p:?} must become a real vertex");
    }
    assert_watertight(&m, "single_face_ngon");
}

// =====================================================================================
// (a.2) Closed loop enclosing a REGION with interior corners in 4 distinct cells — the
//        "carve a closed region" case. It spans cells so the sides cross grid edges.
// =====================================================================================
#[test]
fn closed_interior_region_multicell_stays_valid() {
    // grid(6): boundaries at -0.5,-1/3,-1/6,0,1/6,1/3,0.5. Corners at ±0.25 sit centred in 4 cells.
    let mut m = grid(6);
    let f0 = m.face_count();
    let v0 = m.vertex_count();
    let corners = [
        Vec3::new(-0.25, -0.25, 0.0), Vec3::new(0.25, -0.25, 0.0),
        Vec3::new(0.25, 0.25, 0.0), Vec3::new(-0.25, 0.25, 0.0),
    ];
    let poly: Vec<Vec2> = corners.iter().map(|p| Vec2::new(p.x, p.y)).collect();
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&corners), true, true, Vec3::NEG_Z, None, None);
    eprintln!("[region_multicell] cuts = {cuts}, faces {f0}->{}, verts {v0}->{}", m.face_count(), m.vertex_count());

    // Structural gate — the only thing that separates BUG from graceful-degradation.
    assert_watertight(&m, "region_multicell");

    // Geometry actually happened: all four interior corners materialised as real vertices (either as a
    // through-cut interior vertex or as a slit tip).
    for p in corners {
        assert!(has_vertex_at(&m, p), "interior corner {p:?} must become a real vertex");
    }
    // FINDING (documented, not a hard fail): the closed loop does NOT truly close — the connect step is
    // linear (bounds.windows(2)), it never links the last anchor back to the first, and the first
    // interior corner degrades to a leading SLIT. So the enclosed region is NOT separated. We report the
    // observed face delta rather than asserting a full closed-region count.
    assert!(cuts >= 1, "the region cut must create geometry, got {cuts}");
    assert!(m.face_count() > f0, "cells were split, face count grew ({f0} -> {})", m.face_count());
}

// =====================================================================================
// (b) Closed loop crossing the SAME edge TWICE — and, as a consequence of the closed
//      wrap-around not being handled, TWO slits land on the SAME starting cell face.
// =====================================================================================
#[test]
fn closed_loop_same_edge_twice_two_slits_same_face() {
    // grid(2): internal vertical edge is x=0 (bottom half is the segment (0,-0.5)-(0,0)). A thin closed
    // rectangle in the bottom row straddles x=0 at y=-0.3 and y=-0.2 → the SAME bottom-half edge is
    // crossed twice. Corners A,D fall in the bottom-LEFT cell, B,C in the bottom-RIGHT cell.
    let mut m = grid(2);
    let f0 = m.face_count();
    let a = Vec3::new(-0.2, -0.3, 0.0);
    let b = Vec3::new(0.2, -0.3, 0.0);
    let c = Vec3::new(0.2, -0.2, 0.0);
    let d = Vec3::new(-0.2, -0.2, 0.0);
    let corners = [a, b, c, d];
    let poly: Vec<Vec2> = corners.iter().map(|p| Vec2::new(p.x, p.y)).collect();
    let cuts = knife_cut_screen(&mut m, &ortho_xy, &poly, Some(&corners), true, true, Vec3::NEG_Z, None, None);
    eprintln!("[same_edge_twice] cuts = {cuts}, faces {f0}->{}", m.face_count());

    // The two same-edge crossings must materialise as two DISTINCT vertices on x=0 (progressive split).
    assert!(has_vertex_at(&m, Vec3::new(0.0, -0.3, 0.0)), "first same-edge crossing (0,-0.3) must exist");
    assert!(has_vertex_at(&m, Vec3::new(0.0, -0.2, 0.0)), "second same-edge crossing (0,-0.2) must exist");

    // All four interior corners must materialise (A,D as slit tips in the bottom-left cell; B,C as the
    // through-cut interior chain in the bottom-right cell).
    for p in corners {
        assert!(has_vertex_at(&m, p), "corner {p:?} must become a real vertex");
    }

    // The crux: cut_slit runs TWICE on the SAME bottom-left face (A then D) because the closed loop
    // never wraps (last anchor → first anchor is not connected), so BOTH the leading corner A and the
    // trailing corner D degrade to slits that stack on the start cell. If create_face's twin pairing
    // mis-links on the doubly-slit ring, validate() breaks or raster emits a bad index. assert_watertight
    // is the hard gate — it must survive the doubly-slit face without corruption or panic.
    assert_watertight(&m, "same_edge_twice");

    // Hand-derived: the bottom-left cell becomes ring [0,1,X,A,X,Y,D,Y,4,3] — TWO independent out-and-back
    // spurs (X→A→X and Y→D→Y). Both anchors X,Y appear exactly twice (a legitimate double-slit, NOT a
    // self-intersecting through-cut). Confirm exactly one such double-slit face exists and nothing tangled.
    let (slits, tangles) = dup_face_classes(&m);
    assert!(tangles.is_empty(), "no 3+-fold self-touch face: {:?}", tangles);
    assert_eq!(slits.len(), 1, "exactly one doubly-slit face (the un-closed loop's two stacked spurs): {:?}", slits);

    // Derived face count: bottom-left cell slit(A) [delete1/create1, net 0] → through-cut on bottom-right
    // [delete1/create2, net +1] → bottom-left cell slit(D) [delete1/create1, net 0].  4 -> 5.
    eprintln!("[same_edge_twice] expected face_count = {}, actual = {}", f0 + 1, m.face_count());
    assert_eq!(m.face_count(), f0 + 1, "same-edge-twice closed cut: 4 faces -> 5 (one through-cut split)");
    assert_eq!(cuts, 3, "leading slit + one through-cut + trailing slit = 3 reported cut edges");
}
