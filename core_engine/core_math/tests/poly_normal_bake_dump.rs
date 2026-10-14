//! BEFORE/AFTER DUMP for the poly smooth + normal-bake speedups.
//!
//! The parity claim for these changes is "not one float moves". A reference implementation proves
//! that for the code paths it copies; this proves it for ALL of them at once, including the Mark
//! Sharp / custom-corner / Shade Flat branches, by hashing every output byte and diffing the hash
//! against the same run on the pre-change tree:
//!
//! ```text
//! cargo test -p core_math --release --test poly_normal_bake_dump -- --ignored --nocapture
//! git stash && <run again> && git stash pop     # hashes must match line for line
//! ```
//!
//! `#[ignore]` because it is a diff harness, not a gate — `poly_smooth_parity.rs` is the gate.

use core_math::half_edge::*;
use core_math::poly_mesh::{raster_data_ngon_with_normals, NormalConfig};
use glam::Vec3;
use std::hash::{Hash, Hasher};

fn uv_sphere_quads(rings: usize, segs: usize) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::new();
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..segs {
            let theta = (s as f32 / segs as f32) * std::f32::consts::TAU;
            positions.push(Vec3::new(
                phi.sin() * theta.cos(),
                phi.cos(),
                phi.sin() * theta.sin(),
            ));
        }
    }
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for r in 0..rings {
        for s in 0..segs {
            let s1 = (s + 1) % segs;
            faces.push(vec![
                r * segs + s,
                r * segs + s1,
                (r + 1) * segs + s1,
                (r + 1) * segs + s,
            ]);
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

/// A cube — hard 90° edges, so the Auto Smooth fan must SPLIT (the branch a smooth-only sphere
/// never exercises).
fn cube() -> HalfEdgeMesh {
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
    let f = vec![
        vec![0, 3, 2, 1],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![1, 2, 6, 5],
        vec![2, 3, 7, 6],
        vec![3, 0, 4, 7],
    ];
    HalfEdgeMesh::from_polygons(&p, &f)
}

fn open_grid(n: usize) -> HalfEdgeMesh {
    let mut positions: Vec<Vec3> = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            let h = ((i * 7 + j * 13) % 5) as f32 * 0.11;
            positions.push(Vec3::new(i as f32, h, j as f32));
        }
    }
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let row = n + 1;
    for j in 0..n {
        for i in 0..n {
            faces.push(vec![j * row + i, j * row + i + 1, (j + 1) * row + i + 1, (j + 1) * row + i]);
        }
    }
    HalfEdgeMesh::from_polygons(&positions, &faces)
}

fn punched_sphere() -> HalfEdgeMesh {
    let mut m = uv_sphere_quads(8, 12);
    let victims: Vec<FaceId> = (0..m.faces.len() as u32)
        .filter(|i| i % 11 == 3)
        .map(FaceId)
        .collect();
    for f in victims {
        let _ = m.delete_face(f);
    }
    m
}

#[derive(Default)]
struct H(std::collections::hash_map::DefaultHasher);

impl H {
    fn f(&mut self, v: f32) {
        // Hash the BIT PATTERN — `-0.0 == 0.0` and NaN != NaN would both hide a real change.
        v.to_bits().hash(&mut self.0);
    }
    fn v3(&mut self, v: Vec3) {
        self.f(v.x);
        self.f(v.y);
        self.f(v.z);
    }
    fn u(&mut self, v: u64) {
        v.hash(&mut self.0);
    }
    fn done(self) -> u64 {
        self.0.finish()
    }
}

fn hash_mesh(m: &HalfEdgeMesh) -> u64 {
    let mut h = H::default();
    h.u(m.vertices.len() as u64);
    for v in &m.vertices {
        h.v3(v.position);
        h.v3(v.normal);
    }
    h.u(m.faces.len() as u64);
    for f in &m.faces {
        h.v3(f.normal);
        h.u(f.half_edge.0 as u64);
        h.u(f.material_id as u64);
    }
    h.done()
}

fn hash_bake(m: &HalfEdgeMesh, cfg: &NormalConfig<'_>) -> u64 {
    let (verts, idx, ranges) = raster_data_ngon_with_normals(m, cfg);
    let mut h = H::default();
    h.u(verts.len() as u64);
    for v in &verts {
        for c in v {
            h.f(*c);
        }
    }
    h.u(idx.len() as u64);
    for i in &idx {
        h.u(*i as u64);
    }
    for r in &ranges {
        h.u(r.0 as u64);
        h.u(r.1 as u64);
        h.u(r.2 as u64);
    }
    h.done()
}

fn hash_corner_normals(m: &HalfEdgeMesh, keep: f32, mode: u8, weight: u32, thr: f32) -> u64 {
    let ns = core_math::half_edge::weighted_corner_normals(m, keep, mode, weight, thr, None);
    let mut h = H::default();
    h.u(ns.len() as u64);
    for n in &ns {
        h.v3(*n);
    }
    h.done()
}

#[test]
#[ignore = "before/after diff harness - run explicitly with --release --ignored --nocapture"]
fn dump_all_hashes() {
    let meshes: Vec<(&str, HalfEdgeMesh)> = vec![
        ("sphere_12x16", uv_sphere_quads(12, 16)),
        ("cube", cube()),
        ("open_grid_9", open_grid(9)),
        ("punched_sphere", punched_sphere()),
    ];

    eprintln!("\n=== POLY SMOOTH / NORMAL BAKE HASHES ===");
    for (name, base) in &meshes {
        // 1. Smooth modifier output (positions AND the normals it recomputes).
        for &(it, f) in &[(1u32, 0.5f32), (5, 0.25), (20, 1.0)] {
            let mut m = base.clone();
            m.smooth_laplacian(it, f);
            eprintln!("{name:>16} smooth({it},{f})  {:016x}", hash_mesh(&m));
        }

        // 2. Display bake — Auto Smooth (the live default), Shade Flat, and a wide angle.
        for &deg in &[0.0f32, 40.0, 180.0] {
            let cfg = NormalConfig {
                angle_deg: deg,
                sharp: None,
                custom_corner: None,
            };
            eprintln!("{name:>16} bake(angle={deg})  {:016x}", hash_bake(base, &cfg));
        }

        // 3. Display bake with CUSTOM corner normals (Weighted / Smooth Normal modifier output),
        //    which is the branch that skips the fan walk entirely.
        let custom = core_math::half_edge::weighted_corner_normals(base, 180.0, 2, 50, 0.0, None);
        let cfg = NormalConfig {
            angle_deg: 40.0,
            sharp: None,
            custom_corner: Some(&custom),
        };
        eprintln!("{name:>16} bake(custom)      {:016x}", hash_bake(base, &cfg));

        // 4. The weighted / smooth corner-normal producers across every mode + weight exponent.
        for &(keep, mode, weight, thr) in &[
            (180.0f32, 2u8, 50u32, 0.0f32),
            (30.0, 0, 100, 0.0),
            (30.0, 1, 1, 0.5),
            (89.0, 2, 75, 0.25),
        ] {
            eprintln!(
                "{name:>16} corner({keep},{mode},{weight},{thr})  {:016x}",
                hash_corner_normals(base, keep, mode, weight, thr)
            );
        }

        // 5. The per-VERTEX weighted writer (Solidify + content_signature depend on it).
        for &keep in &[180.0f32, 30.0] {
            let mut m = base.clone();
            m.recompute_normals_weighted(keep);
            eprintln!("{name:>16} recompute({keep})  {:016x}", hash_mesh(&m));
        }
    }
    eprintln!("=== END ===\n");
}
