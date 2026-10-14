// Free-standing geometry helpers (split from half_edge.rs — pure relocation).

use glam::{Mat3, Vec2, Vec3};

// ============================================================
// Geometry helpers (shared, no mesh required)
// ============================================================

/// Newell's method over a CLOSED polygon's points, in ring order.
///
/// The result is **not normalized**: its magnitude is twice the polygon's area, so it doubles as an
/// area weight when summing several faces' normals. Newell is area-weighted and sign-correct for any
/// non-convex / non-planar ring, unlike a first-3-vertex cross product.
///
/// THE single Newell rule for `core_math`. Callers: [`HalfEdgeMesh::compute_face_normal`] and
/// [`HalfEdgeMesh::circle_region`]'s plane fit.
///
/// TODO(dedup): two more copies of this math exist and should fold into this fn when their files are
/// next touched — `bevel_op::newell_idx` (indices + a positions slice) and the app's
/// `poly/fill.rs::best_fit_plane_normal` (adds a collinear-triple fallback). Not folded here to keep
/// this change scoped (CLAUDE.md refactor budget); `bevel_op` is additionally governed by BEVEL.md.
pub fn newell_normal(pts: &[Vec3]) -> Vec3 {
    let n = pts.len();
    if n < 3 { return Vec3::ZERO; }
    let mut acc = Vec3::ZERO;
    for i in 0..n {
        let cur = pts[i];
        let nxt = pts[(i + 1) % n];
        acc.x += (cur.y - nxt.y) * (cur.z + nxt.z);
        acc.y += (cur.z - nxt.z) * (cur.x + nxt.x);
        acc.z += (cur.x - nxt.x) * (cur.y + nxt.y);
    }
    acc
}

/// Blender's `BM_face_calc_center_median_weighted` — the DEFAULT poke center (`center_mode=
/// MEDIAN_WEIGHTED`). Each vertex is weighted by the SUM of its two incident edge lengths, so a
/// regular face collapses to the plain centroid but an irregular one does NOT (the plain centroid is
/// a DIFFERENT point). Verified against a headless Blender 4.5 oracle (`poke_ref.json`): an isoceles
/// triangle `[(-1,0,-1),(1,0,-1),(0,0,1)]` centres at `(0,0,-0.309017)` — NOT the centroid
/// `(0,0,-0.333)` — and an irregular quad at `(2.154508,0,0.523545)` — NOT the centroid `(2.125,0,
/// 0.5)`. `pts` is the face vertex ring in winding order.
pub fn face_center_median_weighted(pts: &[Vec3]) -> Vec3 {
    let n = pts.len();
    if n == 0 { return Vec3::ZERO; }
    if n == 1 { return pts[0]; }
    // edge_len[i] = |pts[i] -> pts[(i+1)%n]| — the edge leaving vertex i.
    let edge_len: Vec<f32> = (0..n).map(|i| (pts[(i + 1) % n] - pts[i]).length()).collect();
    let mut cent = Vec3::ZERO;
    let mut totw = 0.0f32;
    for i in 0..n {
        // vertex i weighted by its two incident edges: the one ARRIVING (i-1 -> i) and LEAVING (i -> i+1).
        let w = edge_len[(i + n - 1) % n] + edge_len[i];
        cent += pts[i] * w;
        totw += w;
    }
    if totw != 0.0 { cent / totw } else { pts[0] }
}

/// GEOMETRIC (least-squares) best-fit circle of 2D points: minimizes `Σ(‖v − c‖ − r)²` over the
/// centre `c` and radius `r` by Gauss-Newton from the start `(0, 0, 1)`.
///
/// This is a faithful port of LoopTools' `circle_calculate_best_fit` (the `fit='best'` default of
/// `bpy.ops.mesh.looptools_circle`), including its start point and its `1e-6` convergence test, so
/// [`HalfEdgeMesh::circle_region`] reproduces Blender's radius exactly.
///
/// NOTE this is the GEOMETRIC fit, not the algebraic (Kåsa) one — they give different answers, and
/// Blender uses the geometric one. It is also NOT the mean distance: mean and best-fit coincide only
/// when the points are symmetric about their centroid (see `circle_region`'s doc comment).
pub(super) fn best_fit_circle_2d(pts: &[Vec2]) -> (Vec2, f32) {
    let mut c = Vec2::ZERO;
    let mut r = 1.0f32;
    if pts.len() < 3 { return (c, r); }
    for _ in 0..500 {
        let mut ata = Mat3::ZERO; // JᵀJ (symmetric)
        let mut atk = Vec3::ZERO; // Jᵀk
        for &v in pts {
            let d = (v - c).length();
            if d < 1e-12 { continue; } // a point AT the centre has no gradient — skip it
            let j = Vec3::new((c.x - v.x) / d, (c.y - v.y) / d, -1.0);
            atk += j * (r - d);
            // JᵀJ += j ⊗ j (column-major outer product; symmetric, so the layout convention is moot)
            ata += Mat3::from_cols(j * j.x, j * j.y, j * j.z);
        }
        if ata.determinant().abs() < 1e-12 { break; } // degenerate (e.g. collinear points)
        let delta = ata.inverse() * atk;
        c.x += delta.x;
        c.y += delta.y;
        r += delta.z;
        if delta.abs().max_element() < 1e-6 { break; }
    }
    (c, r.abs())
}

