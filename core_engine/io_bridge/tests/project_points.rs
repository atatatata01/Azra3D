//! Gates for `project_points_to_mesh` — the surface reprojection behind the poly Remesh
//! modifier's "Project to surface" slider.
//!
//! The function's whole safety argument is the `max_dist` bound: it must pull quantisation error
//! out of a remeshed vertex while REFUSING to move anything that has no source surface nearby
//! (a sealed void's wall, a bridged gap). Both halves are gated here — "it moves points closer"
//! alone would be satisfied by an unbounded snap, which is the dangerous version.

use glam::Vec3;
use io_bridge::mesh_to_sdf::{project_points_to_mesh, project_points_to_mesh_filtered};

/// Icosphere-ish UV sphere as a triangle soup.
fn uv_sphere(r: f32, rings: usize, segs: usize) -> (Vec<Vec3>, Vec<u32>) {
    let mut v: Vec<Vec3> = Vec::new();
    for i in 0..=rings {
        let phi = (i as f32 / rings as f32) * std::f32::consts::PI;
        for j in 0..segs {
            let th = (j as f32 / segs as f32) * std::f32::consts::TAU;
            v.push(Vec3::new(
                r * phi.sin() * th.cos(),
                r * phi.cos(),
                r * phi.sin() * th.sin(),
            ));
        }
    }
    let mut idx: Vec<u32> = Vec::new();
    for i in 0..rings {
        for j in 0..segs {
            let j1 = (j + 1) % segs;
            let a = (i * segs + j) as u32;
            let b = (i * segs + j1) as u32;
            let c = ((i + 1) * segs + j1) as u32;
            let d = ((i + 1) * segs + j) as u32;
            idx.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    (v, idx)
}

/// Distance from `p` to the sphere of radius `r` centred at the origin.
fn dev(p: Vec3, r: f32) -> f32 {
    (p.length() - r).abs()
}

#[test]
fn projection_pulls_points_onto_the_surface() {
    let r = 1.0f32;
    let (v, idx) = uv_sphere(r, 48, 64);

    // Points scattered just off the surface, like voxel-quantised vertices.
    let mut pts: Vec<Vec3> = Vec::new();
    let mut state = 0x1234_5678u64;
    for _ in 0..2000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = ((state >> 16) & 0xFFFF) as f32 / 65535.0 * std::f32::consts::TAU;
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let b = ((state >> 16) & 0xFFFF) as f32 / 65535.0 * std::f32::consts::PI;
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let off = (((state >> 16) & 0xFFFF) as f32 / 65535.0 - 0.5) * 0.08; // ±0.04
        let dir = Vec3::new(b.sin() * a.cos(), b.cos(), b.sin() * a.sin());
        pts.push(dir * (r + off));
    }

    let before: f32 = pts.iter().map(|&p| dev(p, r)).sum::<f32>() / pts.len() as f32;
    let out = project_points_to_mesh(&pts, &v, &idx, 0.1).expect("sphere is a valid mesh");
    assert_eq!(out.len(), pts.len(), "one output per input");
    let after: f32 = out.iter().map(|&p| dev(p, r)).sum::<f32>() / out.len() as f32;

    eprintln!("mean deviation from surface: {before:.5} -> {after:.5}");
    assert!(
        after < before * 0.25,
        "projection must substantially reduce deviation ({before:.5} -> {after:.5})"
    );
}

#[test]
fn points_beyond_max_dist_are_left_untouched() {
    let r = 1.0f32;
    let (v, idx) = uv_sphere(r, 32, 48);

    // Far inside the sphere (the "sealed void wall" case) and far outside — both are further
    // than max_dist from the surface, so both must survive unchanged.
    let far = vec![
        Vec3::ZERO,
        Vec3::new(0.1, 0.0, 0.0),
        Vec3::new(0.0, 3.0, 0.0),
        Vec3::new(-2.5, -2.5, 2.5),
    ];
    let out = project_points_to_mesh(&far, &v, &idx, 0.05).expect("valid mesh");
    for (i, (a, b)) in out.iter().zip(far.iter()).enumerate() {
        assert_eq!(
            a.to_array(),
            b.to_array(),
            "point {i} at {b:?} is beyond max_dist and must not move"
        );
    }
}

/// The bound is a HARD limit, not a suggestion: no point may travel further than `max_dist`.
/// A vertex that jumped further would be landing on unrelated geometry.
#[test]
fn no_point_travels_further_than_max_dist() {
    let r = 1.0f32;
    let (v, idx) = uv_sphere(r, 32, 48);
    let max_dist = 0.07f32;

    let mut pts: Vec<Vec3> = Vec::new();
    let mut state = 0xDEAD_BEEFu64;
    for _ in 0..1500 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = ((state >> 16) & 0xFFFF) as f32 / 65535.0 * std::f32::consts::TAU;
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let b = ((state >> 16) & 0xFFFF) as f32 / 65535.0 * std::f32::consts::PI;
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        // Span both sides of the bound so the clamp is genuinely exercised.
        let off = (((state >> 16) & 0xFFFF) as f32 / 65535.0 - 0.5) * 0.6;
        let dir = Vec3::new(b.sin() * a.cos(), b.cos(), b.sin() * a.sin());
        pts.push(dir * (r + off));
    }

    let out = project_points_to_mesh(&pts, &v, &idx, max_dist).expect("valid mesh");
    for (i, (o, p)) in out.iter().zip(pts.iter()).enumerate() {
        let moved = (*o - *p).length();
        assert!(
            moved <= max_dist + 1e-4,
            "point {i} travelled {moved:.5}, over the {max_dist:.5} bound"
        );
    }
}

#[test]
fn degenerate_input_fails_closed() {
    let (v, idx) = uv_sphere(1.0, 8, 8);
    // No triangles at all → None, so the caller keeps the unprojected mesh.
    assert!(project_points_to_mesh(&[Vec3::ZERO], &v, &[], 0.1).is_none());
    // Zero / negative strength short-circuits to the input, untouched.
    let pts = vec![Vec3::new(0.5, 0.5, 0.5)];
    let out = project_points_to_mesh(&pts, &v, &idx, 0.0).expect("valid mesh");
    assert_eq!(out, pts);
}

/// A clip box around a point must match the unfiltered project for that point,
/// and leave a far-away point untouched (no triangle in range).
#[test]
fn filtered_clip_matches_full_inside_the_box() {
    let r = 1.0f32;
    let (v, idx) = uv_sphere(r, 24, 32);
    let on_eq = Vec3::new(r, 0.0, 0.0);
    let near = on_eq * 1.02;
    let far = Vec3::new(-r, 0.0, 0.0) * 1.02;
    let pts = vec![near, far];
    let full = project_points_to_mesh(&pts, &v, &idx, 0.08).expect("full");
    let clip = (on_eq - Vec3::splat(0.15), on_eq + Vec3::splat(0.15));
    let clipped = project_points_to_mesh_filtered(&pts, &v, &idx, 0.08, Some(clip))
        .expect("clipped");
    assert_eq!(
        clipped[0].to_array(),
        full[0].to_array(),
        "point inside the clip box must match the unfiltered project"
    );
    assert_eq!(
        clipped[1].to_array(),
        far.to_array(),
        "point outside the clip box must stay put (no source triangle in range)"
    );
}
