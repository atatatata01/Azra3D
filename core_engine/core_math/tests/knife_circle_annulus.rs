//! Legality proof for the PROPOSED fix to the circle-knife "segment after cut" defect
//! (owner 2026-08-07, `KNIFE CIRCLE CUT FAIL.fl4`; see `docs/planning/poly/KNIFE.md` §5.5).
//!
//! Today a CLOSED interior loop (the C-shape knife drawn wholly inside one face) falls into
//! `knife_cut_screen`'s anchor-less branch, both free ends resolve to the SAME nearest ring
//! vertex, and the `(Some(a), _)` arm calls `split_poly_slit` — a dangling spur. The owner's
//! file proves it: face 0 is a 47-ring with 28 unique vertices (the circle walked out and
//! back), the face was never divided, and `is_manifold()` reports the pinch.
//!
//! Blender's EXACT oracle (`docs/planning/poly/circle_knife_oracle.py`, cylinder DIFFERENCE
//! box) never emits a pinched face: it splits the resulting ANNULUS into two SIMPLE polygons
//! (arity 7 + 17 on a 16-gon hole, `pinched_faces=0`). This test builds exactly that topology
//! by hand on a unit box and proves our `from_polygons` rebuild accepts it — so the proposed
//! `split_poly_closed_loop` has a legal target shape before anyone writes it.
//!
//! It is a SPEC test: it does not call the knife. It fails only if the kernel could not
//! represent the correct answer.

use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_FACE, NO_TWIN};
use core_math::knife::knife_cut_screen;
use glam::{Vec2, Vec3};
use std::collections::HashSet;

const N: usize = 16;
const R: f32 = 0.35;

/// Cube ±1 with outward winding, then the −X face replaced by
/// `[inner disc] + [two annulus halves]` — the Blender-parity answer for a closed loop
/// drawn wholly inside that face.
fn box_with_circle_cut() -> HalfEdgeMesh {
    let mut p: Vec<Vec3> = vec![
        Vec3::new(-1.0, -1.0, -1.0), // 0
        Vec3::new(1.0, -1.0, -1.0),  // 1
        Vec3::new(1.0, 1.0, -1.0),   // 2
        Vec3::new(-1.0, 1.0, -1.0),  // 3
        Vec3::new(-1.0, -1.0, 1.0),  // 4
        Vec3::new(1.0, -1.0, 1.0),   // 5
        Vec3::new(1.0, 1.0, 1.0),    // 6
        Vec3::new(-1.0, 1.0, 1.0),   // 7
    ];
    // Loop on the −X face, wound to MATCH that face's ring order (v0 → v4 → v7 → v3).
    let l0 = p.len();
    for k in 0..N {
        let t = std::f32::consts::TAU * (k as f32) / (N as f32);
        p.push(Vec3::new(-1.0, R * t.sin(), R * t.cos()));
    }
    let l = |k: usize| l0 + (k % N);

    let mut polys: Vec<Vec<usize>> = vec![
        vec![0, 3, 2, 1], // −Z
        vec![4, 5, 6, 7], // +Z
        vec![0, 1, 5, 4], // −Y
        vec![3, 7, 6, 2], // +Y
        vec![1, 2, 6, 5], // +X
    ];
    // Inner disc (same winding as the parent face → same outward normal).
    polys.push((0..N).map(l).collect());
    // Annulus half A: ring arc v0→v4→v7, bridge to L8, loop REVERSE down to L0, bridge home.
    let mut a = vec![0usize, 4, 7];
    for k in (0..=8).rev() {
        a.push(l(k));
    }
    polys.push(a);
    // Annulus half B: ring arc v7→v3→v0, bridge to L0, loop REVERSE down to L8, bridge home.
    let mut b = vec![7usize, 3, 0];
    for k in (8..=16).rev() {
        b.push(l(k)); // l() wraps 16 → 0
    }
    polys.push(b);

    HalfEdgeMesh::from_polygons(&p, &polys)
}

#[test]
fn closed_interior_loop_annulus_split_is_a_legal_mesh() {
    let m = box_with_circle_cut();
    assert!(m.validate().is_ok(), "validate: {:?}", m.validate().err());
    assert!(
        m.is_manifold().is_ok(),
        "is_manifold: {:?}",
        m.is_manifold().err()
    );

    // Blender-oracle invariants (circle_knife_oracle.py, SUBTRACT census):
    // no pinched ring, no boundary edge, no wire edge.
    let mut pinched = 0;
    let mut live_faces = 0;
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        if !m.is_face_live(f) {
            continue;
        }
        live_faces += 1;
        let vs = m.face_vertices(f);
        let uniq: HashSet<u32> = vs.iter().map(|v| v.0).collect();
        if uniq.len() < vs.len() {
            pinched += 1;
        }
    }
    assert_eq!(pinched, 0, "annulus split must not pinch any ring");
    // 5 untouched box faces + disc + 2 annulus halves.
    assert_eq!(live_faces, 8, "face count");

    let mut boundary = 0;
    for i in 0..m.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN {
            boundary += 1;
        }
    }
    assert_eq!(boundary, 0, "solid must stay closed");
}

/// The defect's signature, asserted as a NEGATIVE: the slit form of the same cut (loop walked
/// out and back off one bridge vertex) is what the owner's file contains, and it is NOT
/// manifold. Locks the class so a future "fix" cannot quietly re-emit a spur.
#[test]
fn closed_interior_loop_as_a_slit_spur_is_non_manifold() {
    let mut p: Vec<Vec3> = vec![
        Vec3::new(-1.0, -1.0, -1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(1.0, -1.0, 1.0),
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(-1.0, 1.0, 1.0),
    ];
    let l0 = p.len();
    for k in 0..N {
        let t = std::f32::consts::TAU * (k as f32) / (N as f32);
        p.push(Vec3::new(-1.0, R * t.sin(), R * t.cos()));
    }
    let l = |k: usize| l0 + (k % N);
    // −X face as a SLIT: v0 → v4 → v7 → v3 → spur out around the loop → back → close.
    let mut slit = vec![0usize, 4, 7, 3];
    for k in 0..N {
        slit.push(l(k));
    }
    for k in (1..N).rev() {
        slit.push(l(k));
    }
    slit.push(l(0));
    let polys: Vec<Vec<usize>> = vec![
        vec![0, 3, 2, 1],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![3, 7, 6, 2],
        vec![1, 2, 6, 5],
        slit,
    ];
    let m = HalfEdgeMesh::from_polygons(&p, &polys);
    assert!(
        m.is_manifold().is_err(),
        "a slit spur ring must NOT read as manifold — it is the reported defect"
    );
}

// ---------------------------------------------------------------------------
// REAL CODE PATH: drive `knife_cut_screen` with a closed circle drawn wholly
// inside one face. Before the §5.5 fix this produced a slit spur (face never
// divided, `is_manifold()` = "degenerate face: repeated vertex"). After it, the
// face divides into inner disc + two annulus halves — Blender's answer.
// ---------------------------------------------------------------------------

fn ortho_xy(p: Vec3) -> Option<Vec2> {
    Some(Vec2::new(p.x, p.y))
}

/// One flat quad in the XY plane (z=0), CCW.
fn quad() -> HalfEdgeMesh {
    let p = vec![
        Vec3::new(-1.0, -1.0, 0.0),
        Vec3::new(1.0, -1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(-1.0, 1.0, 0.0),
    ];
    HalfEdgeMesh::from_polygons(&p, &vec![vec![0usize, 1, 2, 3]])
}

/// Closed circle path (screen + 3D), with the repeated closing sample the app's
/// `poly_knife_shape_rebuild` emits so `knife_free_is_closed()` reads closure (§5.6).
fn circle_path() -> (Vec<Vec2>, Vec<Vec3>) {
    let mut s = Vec::new();
    let mut w = Vec::new();
    for k in 0..=N {
        let t = std::f32::consts::TAU * (k % N) as f32 / N as f32;
        let (x, y) = (R * t.cos(), R * t.sin());
        s.push(Vec2::new(x, y));
        w.push(Vec3::new(x, y, 0.0));
    }
    (s, w)
}

fn census(m: &HalfEdgeMesh) -> (usize, usize) {
    let mut pinched = 0;
    let mut live = 0;
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        if !m.is_face_live(f) {
            continue;
        }
        live += 1;
        let vs = m.face_vertices(f);
        let uniq: HashSet<u32> = vs.iter().map(|v| v.0).collect();
        if uniq.len() < vs.len() {
            pinched += 1;
        }
    }
    (live, pinched)
}

#[test]
fn circle_inside_one_face_divides_it_through_the_real_knife() {
    let mut m = quad();
    let (s, w) = circle_path();
    let cuts = knife_cut_screen(
        &mut m,
        &ortho_xy,
        &s,
        Some(&w),
        true,  // closed
        false, // through
        Vec3::new(0.0, 0.0, -1.0),
        None,
        None,
    );
    assert!(cuts > 0, "circle must cut");
    assert!(m.validate().is_ok(), "validate: {:?}", m.validate().err());
    let (live, pinched) = census(&m);
    assert_eq!(pinched, 0, "closed loop must not leave a pinched/spur ring");
    assert_eq!(live, 3, "1 quad -> inner disc + 2 annulus halves, got {live}");
}

#[test]
fn circle_inside_a_box_face_stays_manifold() {
    // Solid box; the circle is drawn on the +Z face only (through=false, view from +Z).
    let p = vec![
        Vec3::new(-1.0, -1.0, -1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(1.0, -1.0, 1.0),
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(-1.0, 1.0, 1.0),
    ];
    let polys: Vec<Vec<usize>> = vec![
        vec![0, 3, 2, 1],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![3, 7, 6, 2],
        vec![1, 2, 6, 5],
        vec![0, 4, 7, 3],
    ];
    let mut m = HalfEdgeMesh::from_polygons(&p, &polys);
    let mut s = Vec::new();
    let mut w = Vec::new();
    for k in 0..=N {
        let t = std::f32::consts::TAU * (k % N) as f32 / N as f32;
        let (x, y) = (R * t.cos(), R * t.sin());
        s.push(Vec2::new(x, y));
        w.push(Vec3::new(x, y, 1.0)); // on the +Z face
    }
    let cuts = knife_cut_screen(
        &mut m,
        &ortho_xy,
        &s,
        Some(&w),
        true,
        false,
        Vec3::new(0.0, 0.0, -1.0),
        Some(Vec3::new(0.0, 0.0, 10.0)),
        None,
    );
    assert!(cuts > 0, "circle must cut");
    assert!(m.validate().is_ok(), "validate: {:?}", m.validate().err());
    assert!(
        m.is_manifold().is_ok(),
        "is_manifold: {:?}",
        m.is_manifold().err()
    );
    let (live, pinched) = census(&m);
    assert_eq!(pinched, 0, "no pinched ring");
    assert_eq!(live, 8, "6 box faces -> 5 + disc + 2 halves, got {live}");

    let mut boundary = 0;
    for i in 0..m.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if m.he(h).face != NO_FACE && m.he(h).twin == NO_TWIN {
            boundary += 1;
        }
    }
    assert_eq!(boundary, 0, "solid must stay closed");
}
