//! NURBS invariant harness (PLAN_FOR_GROK_NURBS.md §0 / N0).
//!
//! Every constructor ships at least one of these properties. Sabotage-check:
//! flip a normal/corner in `new_box_faces` and confirm `outward_normal` FAILS.

use core_math::nurbs::{ControlPoint, NurbsCurve, NurbsSurface};
use glam::Vec3;

const EPS: f32 = 1e-5;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Cox–de Boor basis N_{i,p}(t) for i = 0..n_cps-1. Sum must be 1 on the domain.
fn basis_functions(degree: u32, knots: &[f32], t: f32, n_cps: usize) -> Vec<f32> {
    let p = degree as usize;
    let mut n = vec![0.0f32; n_cps];
    if n_cps == 0 {
        return n;
    }
    // Clamp t into the open domain [knots[p], knots[last-p]]
    let t0 = knots.get(p).copied().unwrap_or(0.0);
    let t1 = knots.get(knots.len().saturating_sub(p + 1)).copied().unwrap_or(1.0);
    let t = t.clamp(t0, t1 - 1e-7);

    // Find span
    let mut k = p;
    for i in p..(knots.len().saturating_sub(1)) {
        if t >= knots[i] && t < knots[i + 1] {
            k = i;
            break;
        }
        if i + 1 == knots.len() - 1 {
            k = i;
        }
    }

    // Degree-0
    let mut left = vec![0.0f32; p + 1];
    let mut right = vec![0.0f32; p + 1];
    let mut ndu = vec![vec![0.0f32; p + 1]; p + 1];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[k + 1 - j];
        right[j] = knots[k + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let tmp = ndu[r][j - 1] / (right[r + 1] + left[j - r]).max(1e-12);
            ndu[r][j] = saved + right[r + 1] * tmp;
            saved = left[j - r] * tmp;
        }
        ndu[j][j] = saved;
    }
    for j in 0..=p {
        let idx = k.saturating_sub(p) + j;
        if idx < n_cps {
            n[idx] = ndu[j][p];
        }
    }
    n
}

fn sample_params(n: usize) -> Vec<f32> {
    (0..n).map(|i| (i as f32 + 0.5) / n as f32).collect()
}

// ── Partition of unity ───────────────────────────────────────────────────

#[test]
fn partition_of_unity_curve_line() {
    let line = NurbsCurve::new_line(Vec3::ZERO, Vec3::X);
    let (t0, t1) = line.domain();
    for u in sample_params(100) {
        let t = t0 + (t1 - t0) * u;
        let b = basis_functions(line.degree, &line.knots, t, line.control_points.len());
        let s: f32 = b.iter().sum();
        assert!(
            (s - 1.0).abs() < 1e-5,
            "partition of unity failed at t={t}: sum={s}"
        );
    }
}

#[test]
fn partition_of_unity_curve_circle() {
    let c = NurbsCurve::new_circle(1.0);
    let (t0, t1) = c.domain();
    for u in sample_params(50) {
        let t = t0 + (t1 - t0) * u * 0.999; // stay inside open interval
        let b = basis_functions(c.degree, &c.knots, t, c.control_points.len());
        let s: f32 = b.iter().sum();
        // Rational circle uses weights; basis here is the non-rational N_i,p
        // which still partition to 1 — the rational form reweights after.
        assert!(
            (s - 1.0).abs() < 1e-4,
            "circle basis sum at t={t}: {s}"
        );
    }
}

#[test]
fn partition_of_unity_surface_plane() {
    // Tensor product: Σ_i Σ_j N_i(u) M_j(v) = 1
    let plane = NurbsSurface::new_plane(2.0, 2.0);
    let (u0, u1) = plane.domain_u();
    let (v0, v1) = plane.domain_v();
    let rows = plane.control_points.len();
    let cols = plane.control_points[0].len();
    for su in sample_params(20) {
        for sv in sample_params(20) {
            let u = u0 + (u1 - u0) * su;
            let v = v0 + (v1 - v0) * sv;
            let nu = basis_functions(plane.degree_u, &plane.knots_u, u, rows);
            let nv = basis_functions(plane.degree_v, &plane.knots_v, v, cols);
            let mut s = 0.0f32;
            for i in 0..rows {
                for j in 0..cols {
                    s += nu[i] * nv[j];
                }
            }
            assert!(
                (s - 1.0).abs() < 1e-5,
                "plane partition failed at ({u},{v}): sum={s}"
            );
        }
    }
}

// ── Planarity ────────────────────────────────────────────────────────────

#[test]
fn planarity_plane() {
    let plane = NurbsSurface::new_plane(3.0, 2.0);
    let n = plane.normal(0.5, 0.5).normalize_or_zero();
    let origin = plane.evaluate(0.5, 0.5);
    for su in sample_params(10) {
        for sv in sample_params(10) {
            let p = plane.evaluate(su, sv);
            let d = (p - origin).dot(n).abs();
            assert!(d < 1e-5, "plane point off plane by {d}");
        }
    }
}

#[test]
fn planarity_box_faces() {
    for (name, surf) in NurbsSurface::new_box_faces(0.5, 0.4, 0.3) {
        // Geometric plane from three corners (avoids finite-diff normal noise).
        let a = surf.evaluate(0.0, 0.0);
        let b = surf.evaluate(1.0, 0.0);
        let c = surf.evaluate(0.0, 1.0);
        let n = (b - a).cross(c - a).normalize_or_zero();
        for su in sample_params(8) {
            for sv in sample_params(8) {
                let p = surf.evaluate(su, sv);
                let d = (p - a).dot(n).abs();
                assert!(
                    d < 1e-4,
                    "box face {name} non-planar: off-plane dist {d} at ({su},{sv})"
                );
            }
        }
    }
}

// ── Outward normal ───────────────────────────────────────────────────────

#[test]
fn outward_normal_box_faces() {
    let faces = NurbsSurface::new_box_faces(0.5, 0.5, 0.5);
    assert_eq!(faces.len(), 6);
    for (name, surf) in &faces {
        let p = surf.evaluate(0.5, 0.5);
        let n = surf.normal(0.5, 0.5);
        let outward = n.dot(p);
        assert!(
            outward > 0.1,
            "face {name}: normal points inward (n={n:?} p={p:?} n·p={outward})"
        );
    }
}

// ── Corner interpolation (clamped knots) ─────────────────────────────────

#[test]
fn corner_interpolation_plane() {
    let plane = NurbsSurface::new_plane(2.0, 2.0);
    let cp00 = plane.control_points[0][0].position;
    let p00 = plane.evaluate(0.0, 0.0);
    assert!(
        (p00 - cp00).length() < 1e-4,
        "plane corner (0,0) not at CP[0][0]: {p00:?} vs {cp00:?}"
    );
}

#[test]
fn corner_interpolation_quad() {
    let p00 = Vec3::new(0.0, 0.0, 0.0);
    let p01 = Vec3::new(0.0, 0.0, 1.0);
    let p10 = Vec3::new(1.0, 0.0, 0.0);
    let p11 = Vec3::new(1.0, 0.0, 1.0);
    let q = NurbsSurface::new_quad(p00, p01, p10, p11, "q");
    assert!((q.evaluate(0.0, 0.0) - p00).length() < 1e-4);
    assert!((q.evaluate(0.0, 1.0) - p01).length() < 1e-4);
    assert!((q.evaluate(1.0, 0.0) - p10).length() < 1e-4);
    assert!((q.evaluate(1.0, 1.0) - p11).length() < 1e-4);
}

#[test]
fn corner_interpolation_box_faces() {
    for (name, surf) in NurbsSurface::new_box_faces(0.5, 0.5, 0.5) {
        let cp00 = surf.control_points[0][0].position;
        let p00 = surf.evaluate(0.0, 0.0);
        assert!(
            (p00 - cp00).length() < 1e-4,
            "box face {name}: corner not interpolated"
        );
    }
}

// ── Constant-CP evaluation (partition consequence for weight=1) ──────────

/// After translating one face's CPs (and welded neighbours), solid corners still meet.
#[test]
fn face_translate_keeps_box_corners_welded() {
    let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
    assert_eq!(faces.len(), 6);
    // Seed positions of Top face (index 0) CPs
    let seeds: Vec<Vec3> = faces[0]
        .surface
        .control_points
        .iter()
        .flat_map(|row| row.iter().map(|cp| cp.position))
        .collect();
    let delta = Vec3::new(0.0, 0.25, 0.0);
    const EPS2: f32 = 1e-8;
    for face in &mut faces {
        let surf = std::sync::Arc::make_mut(&mut face.surface);
        for row in &mut surf.control_points {
            for cp in row.iter_mut() {
                if seeds.iter().any(|s| (cp.position - *s).length_squared() < EPS2) {
                    cp.position += delta;
                }
            }
        }
    }
    // Every seed world position after move should appear on ≥2 faces (shared corner/edge)
    // Check: collect all CP positions; for each unique corner of the moved top, count matches.
    let mut all_pos: Vec<Vec3> = Vec::new();
    for face in &faces {
        for row in &face.surface.control_points {
            for cp in row {
                all_pos.push(cp.position);
            }
        }
    }
    for s in &seeds {
        let moved = *s + delta;
        let count = all_pos
            .iter()
            .filter(|p| (**p - moved).length_squared() < 1e-6)
            .count();
        assert!(
            count >= 2,
            "welded corner {moved:?} appears only {count} times (solid opened)"
        );
    }
}

#[test]
fn constant_control_points_evaluate_to_point() {
    let p = Vec3::new(1.5, -2.0, 0.25);
    let mut plane = NurbsSurface::new_plane(2.0, 2.0);
    for row in &mut plane.control_points {
        for cp in row.iter_mut() {
            *cp = ControlPoint::new(p, 1.0);
        }
    }
    for su in sample_params(10) {
        for sv in sample_params(10) {
            let e = plane.evaluate(su, sv);
            assert!(
                (e - p).length() < 1e-4,
                "constant CP surface drifted: {e:?} vs {p:?}"
            );
        }
    }
}
