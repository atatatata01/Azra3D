// Free-standing geometry helpers (split from half_edge.rs — pure relocation).

use glam::{Vec2, Vec3};

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

/// Best-fit circle `(centre, radius)` of 2D points in the geometric least-squares sense:
/// minimises `sum_i (|p_i - c| - r)^2` over the centre `c` and radius `r`.
///
/// Independent implementation from the standard literature on circle fitting (Coope 1993;
/// Chernov, "Circular and Linear Regression"): the points are centred on their centroid, an
/// algebraic (Kasa) fit supplies the initial guess — a 2x2 linear solve of
/// `x^2 + y^2 + D x + E y + F = 0` in least squares — and Levenberg-Marquardt-damped
/// Gauss-Newton then refines it on the true geometric residuals. All arithmetic is `f64`.
///
/// NOTE this is the GEOMETRIC fit, not the algebraic one (they differ for noisy / asymmetric
/// input), and it is NOT the mean distance to the centroid: those coincide only when the
/// points are symmetric about their centroid (see `circle_region`'s doc comment).
///
/// Fewer than 3 points returns `(ZERO, 1.0)`. Collinear / coincident input has no finite
/// circle; it returns the centroid with the mean distance to it.
pub(super) fn best_fit_circle_2d(pts: &[Vec2]) -> (Vec2, f32) {
    if pts.len() < 3 { return (Vec2::ZERO, 1.0); }
    let n = pts.len() as f64;
    let (mut mx, mut my) = (0.0f64, 0.0f64);
    for p in pts { mx += p.x as f64; my += p.y as f64; }
    mx /= n; my /= n;
    let q: Vec<(f64, f64)> = pts.iter().map(|p| (p.x as f64 - mx, p.y as f64 - my)).collect();

    let mean_dist = |cx: f64, cy: f64| -> f64 {
        q.iter().map(|&(x, y)| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()).sum::<f64>() / n
    };

    // Kasa: minimise sum (x^2+y^2 + D x + E y + F)^2. With centred data sum x = sum y = 0, so the
    // normal equations decouple: D = -Szx/Sxx-style 2x2 system, centre = (-D/2, -E/2).
    let (mut sxx, mut syy, mut sxy, mut sxz, mut syz) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for &(x, y) in &q {
        let z = x * x + y * y;
        sxx += x * x; syy += y * y; sxy += x * y; sxz += x * z; syz += y * z;
    }
    let det = sxx * syy - sxy * sxy;
    let scale2 = sxx + syy;
    if !(det.abs() > 1e-12 * scale2 * scale2) || !det.is_finite() {
        let r = mean_dist(0.0, 0.0);
        return (Vec2::new(mx as f32, my as f32), r as f32);
    }
    // [sxx sxy; sxy syy] [D; E] = -[sxz; syz]
    let d = (-sxz * syy + syz * sxy) / det;
    let e = (-syz * sxx + sxz * sxy) / det;
    let (mut cx, mut cy) = (-0.5 * d, -0.5 * e);
    let mut r = mean_dist(cx, cy);

    // Geometric refinement. Residual f_i = |q_i - c| - r; Jacobian row = (-ux, -uy, -1) with
    // u = unit(q_i - c). Solve (J^T J + lambda I) delta = -J^T f, accept only if cost drops.
    let cost = |cx: f64, cy: f64, r: f64| -> f64 {
        q.iter().map(|&(x, y)| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).powi(2)).sum()
    };
    let mut cur = cost(cx, cy, r);
    let mut lambda = 1e-3f64;
    for _ in 0..100 {
        let mut a = [[0.0f64; 3]; 3];
        let mut g = [0.0f64; 3];
        for &(x, y) in &q {
            let (dx, dy) = (x - cx, y - cy);
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < 1e-300 { continue; }
            let row = [-dx / dist, -dy / dist, -1.0];
            let f = dist - r;
            for i in 0..3 {
                g[i] += row[i] * f;
                for j in 0..3 { a[i][j] += row[i] * row[j]; }
            }
        }
        let mut improved = false;
        for _ in 0..30 {
            let mut m = a;
            for i in 0..3 { m[i][i] += lambda * (a[i][i].max(1e-12)); }
            let Some(step) = solve3(&m, [-g[0], -g[1], -g[2]]) else { lambda *= 10.0; continue; };
            let (ncx, ncy, nr) = (cx + step[0], cy + step[1], r + step[2]);
            let nc = cost(ncx, ncy, nr);
            if nc.is_finite() && nc <= cur {
                let tiny = step[0].abs().max(step[1].abs()).max(step[2].abs()) <= 1e-12 * (r.abs() + 1.0);
                cx = ncx; cy = ncy; r = nr; cur = nc;
                lambda = (lambda * 0.3).max(1e-12);
                improved = !tiny;
                break;
            }
            lambda *= 10.0;
        }
        if !improved { break; }
    }
    (Vec2::new((cx + mx) as f32, (cy + my) as f32), r.abs() as f32)
}

/// Solve a 3x3 linear system by Cramer's rule; `None` when (near-)singular.
fn solve3(m: &[[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det3 = |a: &[[f64; 3]; 3]| -> f64 {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det3(m);
    if !d.is_finite() || d.abs() < 1e-300 { return None; }
    let mut out = [0.0f64; 3];
    for k in 0..3 {
        let mut t = *m;
        for i in 0..3 { t[i][k] = b[i]; }
        out[k] = det3(&t) / d;
    }
    Some(out)
}

