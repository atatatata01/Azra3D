//! Shared test meshes for the mesh→SDF test suite (perf harness + band golden).
#![allow(dead_code)] // each integration test binary uses a subset

use glam::Vec3;

/// Axis-aligned cube (12 tris, outward winding) — dispatches to the BRUTE path (≤128 tris).
pub fn cube(h: f32) -> (Vec<Vec3>, Vec<u32>) {
    let v = vec![
        Vec3::new(-h, -h, -h), Vec3::new(h, -h, -h), Vec3::new(h, h, -h), Vec3::new(-h, h, -h),
        Vec3::new(-h, -h, h), Vec3::new(h, -h, h), Vec3::new(h, h, h), Vec3::new(-h, h, h),
    ];
    let i = vec![
        0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4,
        1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7, 6, 3, 0, 4, 3, 4, 7,
    ];
    (v, i)
}

/// UV sphere, outward winding enforced via signed volume (mesh_to_sdf's
/// SignMethod::Normal reads nearest-triangle normals, so winding must be
/// consistently outward for inside = negative).
pub fn uv_sphere(rings: u32, segs: u32, radius: f32) -> (Vec<Vec3>, Vec<u32>) {
    let mut verts: Vec<Vec3> = Vec::new();
    verts.push(Vec3::new(0.0, radius, 0.0));
    for r in 1..rings {
        let phi = std::f32::consts::PI * r as f32 / rings as f32;
        for s in 0..segs {
            let theta = std::f32::consts::TAU * s as f32 / segs as f32;
            verts.push(Vec3::new(
                radius * phi.sin() * theta.cos(),
                radius * phi.cos(),
                radius * phi.sin() * theta.sin(),
            ));
        }
    }
    verts.push(Vec3::new(0.0, -radius, 0.0));
    let south = (verts.len() - 1) as u32;
    let idx = |r: u32, s: u32| 1 + (r - 1) * segs + (s % segs);

    let mut tris: Vec<u32> = Vec::new();
    for s in 0..segs {
        tris.extend([0, idx(1, s + 1), idx(1, s)]);
    }
    for r in 1..rings - 1 {
        for s in 0..segs {
            let (a, b) = (idx(r, s), idx(r, s + 1));
            let (c, d) = (idx(r + 1, s), idx(r + 1, s + 1));
            tris.extend([a, b, d, a, d, c]);
        }
    }
    for s in 0..segs {
        tris.extend([south, idx(rings - 1, s), idx(rings - 1, s + 1)]);
    }

    // Signed volume (divergence theorem); negative = inward winding → flip.
    let vol: f32 = tris
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (verts[t[0] as usize], verts[t[1] as usize], verts[t[2] as usize]);
            a.dot(b.cross(c)) / 6.0
        })
        .sum();
    if vol < 0.0 {
        for t in tris.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
    (verts, tris)
}
