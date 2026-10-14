//! PLACEMENT AUDIT — corner-turning consume fuse (`consume_fuse_bevel`, bevel_absorb.rs).
//!
//! Unlike `consume_corner_adversarial.rs` (which PINS fail-closed refusal), this suite
//! asserts the corner turn actually WORKS and lands geometry where the design says it
//! must (BEVEL.md §11 corner design: each straight run lofts its bands as today; at a
//! corner vertex the two runs share ONE profile column placed on the corner miter —
//! the cap-offset math generalized to a quad-ring side):
//!
//!   1. validate() + is_manifold() + no folded faces (beveled cube stays convex —
//!      every face's Newell normal points away from the origin);
//!   2. the MITER POINT exists as a live vertex at exactly (width, width) from the
//!      corner on the shared (top) face;
//!   3. the corner profile column carries `segments-1` intermediate vertices bulging
//!      OFF the P–Q chord (P = miter on the shared face, Q = the point `width` down
//!      the vertical corner edge) TOWARD the old sharp corner — roundness, not a
//!      flat chamfer collapsed onto the chord;
//!   4. every live vertex is either untouched original lattice (on the 2/n grid, on
//!      the cube surface) or inside the fillet band around a selected edge line —
//!      including the corner square (ball around the corner);
//!   5. boundary loops carry ONE vertex per profile column — top-face boundary at
//!      exact in-face distance `width` from the selected polyline, side boundary at
//!      y = 1 - width; counts match the rim column count, all pairwise distinct.
//!
//! If the fuse REFUSES either scenario this suite FAILS LOUDLY (panic with an
//! explicit "REFUSED" message) — refusal means corner turning is not implemented
//! and placement cannot be audited.

use core_math::bevel_absorb::consume_fuse_bevel;
use core_math::bevel_op::BevelParams;
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_TWIN};
use glam::Vec3;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Fixture (same construction as the bevel_absorb unit-test / adversarial fixture)
// ---------------------------------------------------------------------------

/// Cube subdivided n x n on every side, [-1,1]^3, welded lattice, outward winding.
fn subdivided_cube(n: usize) -> HalfEdgeMesh {
    let mut map: HashMap<(usize, usize, usize), usize> = HashMap::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let c = |i: usize| -1.0 + 2.0 * i as f32 / n as f32;
    let mut faces: Vec<Vec<usize>> = Vec::new();
    {
        let mut vid = |ijk: (usize, usize, usize)| -> usize {
            *map.entry(ijk).or_insert_with(|| {
                positions.push(Vec3::new(c(ijk.0), c(ijk.1), c(ijk.2)));
                positions.len() - 1
            })
        };
        let sides: [Box<dyn Fn(usize, usize) -> (usize, usize, usize)>; 6] = [
            Box::new(move |u, v| (u, n, v)), // top    y=+1
            Box::new(move |u, v| (v, 0, u)), // bottom y=-1
            Box::new(move |u, v| (u, v, 0)), // front  z=-1
            Box::new(move |u, v| (v, u, n)), // back   z=+1
            Box::new(move |u, v| (0, u, v)), // left   x=-1
            Box::new(move |u, v| (n, v, u)), // right  x=+1
        ];
        for side in &sides {
            for u in 0..n {
                for v in 0..n {
                    faces.push(vec![
                        vid(side(u, v)),
                        vid(side(u, v + 1)),
                        vid(side(u + 1, v + 1)),
                        vid(side(u + 1, v)),
                    ]);
                }
            }
        }
    }
    let mut vol6 = 0.0f32;
    for f in &faces {
        for k in 1..f.len() - 1 {
            let (a, b, cc) = (positions[f[0]], positions[f[k]], positions[f[k + 1]]);
            vol6 += a.dot(b.cross(cc));
        }
    }
    if vol6 < 0.0 {
        for f in &mut faces {
            f.reverse();
        }
    }
    let m = HalfEdgeMesh::from_polygons(&positions, &faces);
    m.validate().expect("subdivided cube valid");
    assert!(m.is_manifold().is_ok(), "subdivided cube watertight");
    m
}

/// Undirected selection of every live edge whose BOTH endpoints satisfy `pred`.
fn select_edges(mesh: &HalfEdgeMesh, pred: impl Fn(Vec3, Vec3) -> bool) -> HashSet<u32> {
    let mut sel = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !mesh.is_edge_live(he) {
            continue;
        }
        let t = mesh.he(he).twin;
        if t != NO_TWIN && i > t.0 {
            continue;
        }
        let a = mesh.vert(mesh.he_source(he)).position;
        let b = mesh.vert(mesh.he(he).vertex).position;
        if pred(a, b) {
            sel.insert(i);
        }
    }
    sel
}

fn live_positions(m: &HalfEdgeMesh) -> Vec<Vec3> {
    (0..m.vertices.len() as u32)
        .map(VertexId)
        .filter(|&v| m.is_vert_live(v))
        .map(|v| m.vert(v).position)
        .collect()
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

struct EdgeLine {
    p: Vec3,
    d: Vec3,
}

fn line(p: Vec3, d: Vec3) -> EdgeLine {
    EdgeLine { p, d: d.normalize() }
}

fn dist_to_line(p: Vec3, l: &EdgeLine) -> f32 {
    let v = p - l.p;
    (v - l.d * v.dot(l.d)).length()
}

/// 2D point-to-segment distance (used for the in-face top boundary polyline).
fn seg_dist2(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 <= 1e-12 {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0)
    };
    let cx = a[0] + ab[0] * t - p[0];
    let cy = a[1] + ab[1] * t - p[1];
    (cx * cx + cy * cy).sqrt()
}

/// A turned corner: sharp corner C, expected miter point M on the shared (top) face
/// at exactly (width, width) from C, and Q = `width` down the vertical corner edge.
struct Corner {
    c: Vec3,
    m: Vec3,
    q: Vec3,
}

// ---------------------------------------------------------------------------
// The audit proper
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn audit_placement(
    label: &str,
    mesh: &HalfEdgeMesh,
    sel: &HashSet<u32>,
    width: f32,
    segments: u32,
    lines: &[EdgeLine],
    corners: &[Corner],
    // top-face selected polyline as 2D (x,z) segments, for the boundary check
    top_polyline: &[([f32; 2], [f32; 2])],
    // face predicate: is this live vertex on one of the SIDE faces the band covers?
    on_side_face: &dyn Fn(Vec3) -> bool,
    expected_columns: usize,
    n: usize,
) {
    let params = BevelParams {
        width,
        segments,
        shape: 0.5,
        clamp: true,
    };

    // ---- 0. The fuse must ACCEPT the corner scenario. Refusal = loud FAIL. ----
    let out = match consume_fuse_bevel(mesh, sel, &params) {
        Some(o) => o,
        None => panic!(
            "{label}: FAIL — corner-turning consume fuse REFUSED this scenario \
             (returned None, fell back to absorb+bevel). Corner turning is not \
             functional for this selection; placement cannot be audited."
        ),
    };

    // ---- 1. validate + manifold + no folds ----
    out.validate()
        .unwrap_or_else(|e| panic!("{label}: fused mesh invalid: {e}"));
    assert!(out.is_manifold().is_ok(), "{label}: fused mesh not watertight");
    // Fold check: a round bevel of a convex solid centered at the origin stays convex,
    // so EVERY live face's Newell normal must point away from the origin. A folded
    // face (the concave-cap class from BEVEL.md §11h fix 1) flips this sign.
    for fi in 0..out.faces.len() as u32 {
        let f = FaceId(fi);
        if !out.is_face_live(f) {
            continue;
        }
        let vs = out.face_vertices(f);
        assert!(vs.len() >= 3, "{label}: face {fi} degenerate ({} verts)", vs.len());
        let pts: Vec<Vec3> = vs.iter().map(|&v| out.vert(v).position).collect();
        let mut normal = Vec3::ZERO;
        let mut centroid = Vec3::ZERO;
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            normal += a.cross(b);
            centroid += a;
        }
        centroid /= pts.len() as f32;
        assert!(
            normal.length() > 1e-8,
            "{label}: face {fi} has zero-area Newell normal (collapsed/folded)"
        );
        assert!(
            normal.dot(centroid) > 1e-6,
            "{label}: FOLDED face {fi} — Newell normal points inward \
             (centroid ({:.3},{:.3},{:.3}), n·c = {:.5})",
            centroid.x,
            centroid.y,
            centroid.z,
            normal.normalize().dot(centroid)
        );
    }

    let pos = live_positions(&out);
    let tol = 1e-3f32;
    let find_at = |p: Vec3| pos.iter().any(|&v| (v - p).length() < tol);

    // ---- 2. Miter point at exactly (width, width) from the corner, shared face ----
    for (k, co) in corners.iter().enumerate() {
        assert!(
            find_at(co.m),
            "{label}: corner {k} MITER POINT missing — no live vertex at \
             ({:.3},{:.3},{:.3}) = (width,width) from corner ({:.3},{:.3},{:.3}) on the shared face",
            co.m.x, co.m.y, co.m.z, co.c.x, co.c.y, co.c.z
        );
        assert!(
            find_at(co.q),
            "{label}: corner {k} profile column base missing — no live vertex at \
             ({:.3},{:.3},{:.3}) = width down the vertical corner edge",
            co.q.x, co.q.y, co.q.z
        );
    }

    // ---- 3. Corner profile column: segments-1 intermediates bulging toward C ----
    for (k, co) in corners.iter().enumerate() {
        // The corner column lives in the diagonal plane through C, M and Q.
        let plane_n = (co.m - co.c).cross(co.q - co.c);
        assert!(plane_n.length() > 1e-6, "corner {k}: degenerate C/M/Q frame");
        let plane_n = plane_n.normalize();
        let chord = co.q - co.m;
        let chord_len2 = chord.length_squared();
        let mut intermediates: Vec<Vec3> = Vec::new();
        for &v in &pos {
            if (v - co.c).length() > width * 2.0 {
                continue; // outside the corner region
            }
            if (v - co.c).dot(plane_n).abs() > tol {
                continue; // off the corner symmetry plane
            }
            let t = (v - co.m).dot(chord) / chord_len2;
            if !(0.02..=0.98).contains(&t) {
                continue; // M / Q themselves (or beyond)
            }
            intermediates.push(v);
        }
        assert_eq!(
            intermediates.len(),
            (segments - 1) as usize,
            "{label}: corner {k} profile column has {} intermediate verts on the \
             C/M/Q plane between P and Q, expected segments-1 = {}",
            intermediates.len(),
            segments - 1
        );
        for v in &intermediates {
            let t = (*v - co.m).dot(chord) / chord_len2;
            let cp = co.m + chord * t; // closest point on the P–Q chord
            let d = *v - cp;
            // direction from the chord toward the old sharp corner, in-plane
            let u = chord.normalize();
            let mut toward = co.c - cp;
            toward -= u * toward.dot(u);
            assert!(
                toward.length() > 1e-6,
                "{label}: corner {k} chord passes through C — bad frame"
            );
            let toward = toward.normalize();
            assert!(
                d.length() > 1e-4,
                "{label}: corner {k} profile vert ({:.4},{:.4},{:.4}) sits ON the \
                 P–Q chord — flat chamfer, no roundness (t={t:.3})",
                v.x, v.y, v.z
            );
            assert!(
                d.dot(toward) > 0.0,
                "{label}: corner {k} profile vert ({:.4},{:.4},{:.4}) bulges AWAY \
                 from the old corner (d·toward = {:.5}) — inverted profile",
                v.x, v.y, v.z,
                d.dot(toward)
            );
        }
    }

    // ---- 4. Every vertex: original lattice OR band (incl. corner square) ----
    let step = 2.0 / n as f32;
    let lattice = |x: f32| ((x / step).round() * step - x).abs() < 1e-3;
    let on_surface =
        |p: Vec3| p.x.abs() > 1.0 - 1e-3 || p.y.abs() > 1.0 - 1e-3 || p.z.abs() > 1.0 - 1e-3;
    let mut strays = 0usize;
    for &p in &pos {
        let is_lattice = lattice(p.x) && lattice(p.y) && lattice(p.z) && on_surface(p);
        let band = lines
            .iter()
            .map(|l| dist_to_line(p, l))
            .fold(f32::MAX, f32::min);
        let corner_d = corners
            .iter()
            .map(|co| (p - co.c).length())
            .fold(f32::MAX, f32::min);
        let in_band = band <= width * 1.6 + 1e-3 || corner_d <= width * 1.8;
        if !is_lattice && !in_band {
            strays += 1;
            if strays <= 8 {
                eprintln!(
                    "{label}: STRAY vert ({:.4},{:.4},{:.4}) — neither lattice nor \
                     band/corner region (line d={band:.4}, corner d={corner_d:.4})",
                    p.x, p.y, p.z
                );
            }
        }
    }
    assert_eq!(strays, 0, "{label}: {strays} strayed vertices outside band+corner region");

    // ---- 5. Boundary loops: one vert per column ----
    // Top-face boundary: live verts on y=1 at in-face distance exactly `width`
    // from the selected polyline. One per profile column (corner column shared).
    let top_boundary: Vec<Vec3> = pos
        .iter()
        .copied()
        .filter(|p| (p.y - 1.0).abs() < tol)
        .filter(|p| {
            let d = top_polyline
                .iter()
                .map(|(a, b)| seg_dist2([p.x, p.z], *a, *b))
                .fold(f32::MAX, f32::min);
            (d - width).abs() < 1.5e-3
        })
        .collect();
    for (i, a) in top_boundary.iter().enumerate() {
        for b in top_boundary.iter().skip(i + 1) {
            assert!(
                (*a - *b).length() > 1e-4,
                "{label}: duplicated top boundary vert at ({:.4},{:.4},{:.4}) — \
                 two columns collapsed onto one boundary point",
                a.x, a.y, a.z
            );
        }
    }
    // The corner FAN shares one miter point P across the whole influence stretch (Maya
    // radial-spoke model, BEVEL.md §11 2026-08-08i): the top loop carries the
    // non-influenced columns' cuts + ONE shared P per corner — fewer verts than columns.
    let expected_top = {
        let influenced = |p: Vec3| corners.iter().any(|c| (p - c.c).length() <= width + 2e-3);
        // Structural checks instead of a closed-form count: every miter present exactly
        // once, and every non-miter boundary vert farther than width from every corner.
        for c in corners {
            assert_eq!(
                top_boundary.iter().filter(|p| (**p - c.m).length() < 1.5e-3).count(),
                1,
                "{label}: miter P for corner at ({:.2},{:.2},{:.2}) must appear exactly once",
                c.c.x, c.c.y, c.c.z
            );
        }
        for p in &top_boundary {
            let is_miter = corners.iter().any(|c| (*p - c.m).length() < 1.5e-3);
            assert!(
                is_miter || !influenced(*p),
                "{label}: non-miter boundary vert ({:.3},{:.3},{:.3}) inside a corner's \
                 influence zone — a fan column leaked a private cut",
                p.x, p.y, p.z
            );
        }
        top_boundary.len()
    };
    assert!(
        expected_top >= corners.len() + 1,
        "{label}: top boundary degenerate ({} verts)",
        expected_top
    );
    // Side boundary: live verts on the covered side faces at y = 1 - width.
    let side_boundary: Vec<Vec3> = pos
        .iter()
        .copied()
        .filter(|&p| on_side_face(p) && (p.y - (1.0 - width)).abs() < 1.5e-3)
        .collect();
    for (i, a) in side_boundary.iter().enumerate() {
        for b in side_boundary.iter().skip(i + 1) {
            assert!(
                (*a - *b).length() > 1e-4,
                "{label}: duplicated side boundary vert at ({:.4},{:.4},{:.4})",
                a.x, a.y, a.z
            );
        }
    }
    assert_eq!(
        side_boundary.len(),
        expected_columns,
        "{label}: side boundary loop has {} verts at y = 1-width, expected one per \
         profile column = {}",
        side_boundary.len(),
        expected_columns
    );

    eprintln!(
        "{label}: PASS — fused v={} f={}, {} corners mitered, {} columns",
        out.vertex_count(),
        out.face_count(),
        corners.len(),
        expected_columns
    );
}

// ---------------------------------------------------------------------------
// Scenario 1: L-chain, n=8, w=0.5, segments=4 — top-front + top-right edges
// turning at cube corner (1,1,-1). Shared face = top (y=1).
// Miter point = (1-w, 1, -1+w); Q = (1, 1-w, -1) on the vertical corner edge.
// Columns: 2n+1 rim verts, corner column shared => 2n+1 = 17.
// ---------------------------------------------------------------------------
#[test]
fn l_chain_n8_corner_placement() {
    let n = 8usize;
    let w = 0.5f32;
    let mesh = subdivided_cube(n);
    let on = |p: Vec3, q: Vec3| {
        let top = (p.y - 1.0).abs() < 1e-4 && (q.y - 1.0).abs() < 1e-4;
        let front = (p.z + 1.0).abs() < 1e-4 && (q.z + 1.0).abs() < 1e-4;
        let right = (p.x - 1.0).abs() < 1e-4 && (q.x - 1.0).abs() < 1e-4;
        top && (front || right)
    };
    let sel = select_edges(&mesh, on);
    assert_eq!(sel.len(), 2 * n, "L-chain of two full top edges");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X),
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),
    ];
    let corners = [Corner {
        c: Vec3::new(1.0, 1.0, -1.0),
        m: Vec3::new(1.0 - w, 1.0, -1.0 + w),
        q: Vec3::new(1.0, 1.0 - w, -1.0),
    }];
    // (x,z) polyline of the selected edges on the top face
    let top_polyline = [([-1.0, -1.0], [1.0, -1.0]), ([1.0, -1.0], [1.0, 1.0])];
    let on_side = |p: Vec3| (p.z + 1.0).abs() < 1e-3 || (p.x - 1.0).abs() < 1e-3;
    audit_placement(
        "L-chain n8 w=0.5 seg=4",
        &mesh,
        &sel,
        w,
        4,
        &lines,
        &corners,
        &top_polyline,
        &on_side,
        2 * n + 1,
        n,
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: FULL top-perimeter ring, n=8, w=0.75, segments=3 — closed loop
// turning at all four top corners. Miters at (±(1-w), 1, ±(1-w)); Q at
// (±1, 1-w, ±1). Columns: 4n rim verts, each corner column shared => 4n = 32.
// ---------------------------------------------------------------------------
#[test]
fn full_ring_n8_corner_placement() {
    let n = 8usize;
    let w = 0.75f32;
    let mesh = subdivided_cube(n);
    let on_border = |p: Vec3| {
        (p.y - 1.0).abs() < 1e-4
            && ((p.x.abs() - 1.0).abs() < 1e-4 || (p.z.abs() - 1.0).abs() < 1e-4)
    };
    let sel = select_edges(&mesh, |a, b| on_border(a) && on_border(b));
    assert_eq!(sel.len(), 4 * n, "full perimeter ring selected");
    let lines = [
        line(Vec3::new(0.0, 1.0, -1.0), Vec3::X),
        line(Vec3::new(0.0, 1.0, 1.0), Vec3::X),
        line(Vec3::new(-1.0, 1.0, 0.0), Vec3::Z),
        line(Vec3::new(1.0, 1.0, 0.0), Vec3::Z),
    ];
    let s = 1.0 - w; // 0.25
    let mk = |sx: f32, sz: f32| Corner {
        c: Vec3::new(sx, 1.0, sz),
        m: Vec3::new(sx * s, 1.0, sz * s),
        q: Vec3::new(sx, 1.0 - w, sz),
    };
    let corners = [mk(1.0, -1.0), mk(1.0, 1.0), mk(-1.0, 1.0), mk(-1.0, -1.0)];
    let top_polyline = [
        ([-1.0, -1.0], [1.0, -1.0]),
        ([1.0, -1.0], [1.0, 1.0]),
        ([1.0, 1.0], [-1.0, 1.0]),
        ([-1.0, 1.0], [-1.0, -1.0]),
    ];
    let on_side = |p: Vec3| (p.x.abs() - 1.0).abs() < 1e-3 || (p.z.abs() - 1.0).abs() < 1e-3;
    audit_placement(
        "full ring n8 w=0.75 seg=3",
        &mesh,
        &sel,
        w,
        3,
        &lines,
        &corners,
        &top_polyline,
        &on_side,
        4 * n,
        n,
    );
}
