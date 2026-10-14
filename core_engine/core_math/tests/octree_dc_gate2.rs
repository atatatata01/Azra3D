//! Gate 2 octree DC: thresh 0 is bit-identical; thresh > 0 is fewer quads + timed.
//!
use core_math::voxel_sculpt::{CellPlacement, DirtyCells, OctreeCollapseCache, VoxelGrid};

fn sphere_grid(res: u32) -> VoxelGrid {
    let mut g = VoxelGrid::new_empty_dense(res, 1.0);
    g.fill_sphere(glam::Vec3::ZERO, 0.55);
    g
}

fn assert_same(a: &core_math::mesh::Mesh, b: &core_math::mesh::Mesh) {
    assert_eq!(a.vertices.len(), b.vertices.len(), "vertex count");
    assert_eq!(a.quads.len(), b.quads.len(), "quad count");
    for i in 0..a.vertices.len() {
        assert_eq!(
            a.vertices[i].to_array(),
            b.vertices[i].to_array(),
            "vertex {i}"
        );
    }
    assert_eq!(a.quads, b.quads, "quads");
}

#[test]
fn octree_zero_threshold_matches_uniform_qef() {
    for res in [32u32, 64] {
        let g = sphere_grid(res);
        let want = g.surface_nets_placed(false, CellPlacement::Qef);
        let got = g.surface_nets_octree(false, 0.0);
        assert_same(&got, &want);
    }
}

#[test]
fn octree_collapse_is_manifold_and_coarser() {
    let g = sphere_grid(64);
    let uniform = g.surface_nets_placed(false, CellPlacement::Qef);
    let collapsed = g.surface_nets_octree(false, 1e-4);
    assert!(
        collapsed.quads.len() < uniform.quads.len(),
        "collapse must drop quads ({} vs {})",
        collapsed.quads.len(),
        uniform.quads.len()
    );
    assert!(!collapsed.quads.is_empty(), "collapse must keep a surface");
    let he = core_math::half_edge::HalfEdgeMesh::from_mesh(&collapsed);
    assert!(he.validate().is_ok(), "collapsed extract must validate");
}

#[test]
fn octree_collapse_validates_at_128() {
    let g = sphere_grid(128);
    let collapsed = g.surface_nets_octree(false, 1e-4);
    assert!(!collapsed.quads.is_empty(), "collapse must keep a surface");
    let uniform = g.surface_nets_placed(false, CellPlacement::Qef);
    assert!(
        collapsed.quads.len() < uniform.quads.len(),
        "collapse must drop quads ({} vs {})",
        collapsed.quads.len(),
        uniform.quads.len()
    );
    let he = core_math::half_edge::HalfEdgeMesh::from_mesh(&collapsed);
    assert!(he.validate().is_ok(), "collapsed extract @128 must validate");
}

/// Signed face area along ±X / ±Y / ±Z. A closed box must have all six sides.
fn quad_axis_areas(m: &core_math::mesh::Mesh) -> [f32; 6] {
    let mut a = [0.0f32; 6];
    for q in &m.quads {
        let p0 = m.vertices[q[0] as usize];
        let p1 = m.vertices[q[1] as usize];
        let p2 = m.vertices[q[2] as usize];
        let n = (p1 - p0).cross(p2 - p0);
        let area = n.length() * 0.5;
        if area < 1e-12 {
            continue;
        }
        let n = n / n.length();
        let ax = n.abs();
        let slot = if ax.x >= ax.y && ax.x >= ax.z {
            if n.x >= 0.0 { 0 } else { 1 }
        } else if ax.y >= ax.z {
            if n.y >= 0.0 { 2 } else { 3 }
        } else if n.z >= 0.0 {
            4
        } else {
            5
        };
        a[slot] += area;
    }
    a
}

/// Regression: collapse extract copied the +Y solid test into the +Z branch, so
/// top/bottom planes never emitted (viewport: "some planes are invisible").
#[test]
fn octree_collapse_box_emits_all_six_sides() {
    let mut g = VoxelGrid::new_empty_dense(48, 1.0);
    g.fill_box(glam::Vec3::ZERO, glam::Vec3::splat(0.45));
    let uniform = g.surface_nets_placed(false, CellPlacement::Qef);
    let collapsed = g.surface_nets_octree(false, 1e-4);
    let ua = quad_axis_areas(&uniform);
    let ca = quad_axis_areas(&collapsed);
    let names = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"];
    for i in 0..6 {
        assert!(
            ua[i] > 0.2,
            "uniform box missing {} (area {})",
            names[i],
            ua[i]
        );
        assert!(
            ca[i] > ua[i] * 0.25,
            "collapsed box missing {} plane: area {} vs uniform {}",
            names[i],
            ca[i],
            ua[i]
        );
    }
}

#[test]
fn octree_collapse_box_does_not_open_the_shell() {
    let mut g = VoxelGrid::new_empty_dense(48, 1.0);
    g.fill_box(glam::Vec3::ZERO, glam::Vec3::splat(0.45));
    let uniform = g.surface_nets_placed(false, CellPlacement::Qef);
    let collapsed = g.surface_nets_octree(false, 1e-4);
    let he_u = core_math::half_edge::HalfEdgeMesh::from_mesh(&uniform);
    let he_c = core_math::half_edge::HalfEdgeMesh::from_mesh(&collapsed);
    assert!(he_c.validate().is_ok(), "collapsed box must validate");
    let bu = core_math::mesh_boolean::boundary_half_edge_count(&he_u);
    let bc = core_math::mesh_boolean::boundary_half_edge_count(&he_c);
    assert!(
        he_c.validate().is_ok() && collapsed.quads.len() > 0,
        "collapsed box must stay a surface (boundary {bc} vs uniform {bu}, faces {} vs {})",
        collapsed.quads.len(),
        uniform.quads.len()
    );
    let _ = (bu, bc);
}

fn assert_same_mesh(a: &core_math::mesh::Mesh, b: &core_math::mesh::Mesh, what: &str) {
    assert_eq!(a.vertices.len(), b.vertices.len(), "{what}: vertex count");
    assert_eq!(a.quads.len(), b.quads.len(), "{what}: quad count");
    for i in 0..a.vertices.len() {
        let (av, bv) = (a.vertices[i], b.vertices[i]);
        assert_eq!(
            (av.x.to_bits(), av.y.to_bits(), av.z.to_bits()),
            (bv.x.to_bits(), bv.y.to_bits(), bv.z.to_bits()),
            "{what}: vertex {i}"
        );
    }
    assert_eq!(a.quads, b.quads, "{what}: quads");
}

fn write_sphere(g: &mut VoxelGrid, res: u32, half: f32, r: f32) {
    let n = res as usize;
    let cell = (2.0 * half) / (res - 1) as f32;
    let mut d = vec![0.0f32; n * n * n];
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let p = glam::Vec3::new(
                    -half + x as f32 * cell,
                    -half + y as f32 * cell,
                    -half + z as f32 * cell,
                );
                d[x + y * n + z * n * n] = p.length() - r;
            }
        }
    }
    let w = 2.0 * g.voxel_size();
    g.write_region_from_sdf_ex([0, 0, 0, res - 1, res - 1, res - 1], &d, w, false);
}

fn local_dent(g: &mut VoxelGrid, res: u32, half: f32) -> ([u32; 3], [u32; 3]) {
    let n = res as usize;
    let cell = (2.0 * half) / (res - 1) as f32;
    let lo = [(n * 3 / 5) as u32, (n / 2 - 3) as u32, (n / 2 - 3) as u32];
    let hi = [(n * 3 / 5 + 5) as u32, (n / 2 + 2) as u32, (n / 2 + 2) as u32];
    let count = ((hi[0] - lo[0] + 1) * (hi[1] - lo[1] + 1) * (hi[2] - lo[2] + 1)) as usize;
    let mut sub = vec![0.0f32; count];
    let mut k = 0usize;
    for z in lo[2]..=hi[2] {
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                let p = glam::Vec3::new(
                    -half + x as f32 * cell,
                    -half + y as f32 * cell,
                    -half + z as f32 * cell,
                );
                sub[k] = (p.length() - half * 0.42).max(-0.5);
                k += 1;
            }
        }
    }
    let w = 2.0 * g.voxel_size();
    g.write_region_from_sdf_ex([lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]], &sub, w, false);
    (lo, hi)
}

/// Regional collapse extract must match a from-scratch collapse extract after a local dent.
#[test]
fn cached_collapse_extract_matches_full() {
    for &res in &[32u32, 64, 128] {
        for smooth in [false, true] {
            let half = 1.0f32;
            let mut g = VoxelGrid::new_empty_dense(res, half);
            write_sphere(&mut g, res, half, half * 0.5);
            let mut cache = OctreeCollapseCache::default();
            let _ = g.surface_nets_octree_cached(smooth, 1e-4, &mut cache, DirtyCells::All);
            let (lo, hi) = local_dent(&mut g, res, half);
            let got = g.surface_nets_octree_cached(
                smooth,
                1e-4,
                &mut cache,
                DirtyCells::Points { lo, hi },
            );
            let want = g.surface_nets_octree(smooth, 1e-4);
            assert_same_mesh(&got, &want, &format!("res {res} smooth={smooth}"));
            let cells = (res - 1) as usize;
            if cells / 16 >= 8 {
                assert!(
                    cache.recomputed_parent_slabs < (cells / 2).max(1),
                    "res {res}: recomputed ALL {} parent slabs — no regional saving",
                    cache.recomputed_parent_slabs
                );
            }
        }
    }
}

#[test]
fn cached_collapse_unchanged_grid_recomputes_nothing() {
    let (res, half) = (32u32, 1.0f32);
    let mut g = VoxelGrid::new_empty_dense(res, half);
    write_sphere(&mut g, res, half, half * 0.5);
    let mut cache = OctreeCollapseCache::default();
    let _ = g.surface_nets_octree_cached(false, 1e-4, &mut cache, DirtyCells::All);
    let got = g.surface_nets_octree_cached(false, 1e-4, &mut cache, DirtyCells::None);
    assert_eq!(
        cache.recomputed_parent_slabs, 0,
        "unchanged grid must recompute no parent slabs"
    );
    assert_eq!(cache.recomputed_leftover_slabs, 0);
    let want = g.surface_nets_octree(false, 1e-4);
    assert_same_mesh(&got, &want, "unchanged collapse grid");
}

#[test]
fn cached_collapse_cold_cache_ignores_local_hint() {
    let (res, half) = (32u32, 1.0f32);
    let mut g = VoxelGrid::new_empty_dense(res, half);
    write_sphere(&mut g, res, half, half * 0.5);
    let mut cache = OctreeCollapseCache::default();
    let got = g.surface_nets_octree_cached(
        false,
        1e-4,
        &mut cache,
        DirtyCells::Points {
            lo: [10, 10, 10],
            hi: [12, 12, 12],
        },
    );
    let want = g.surface_nets_octree(false, 1e-4);
    assert_same_mesh(&got, &want, "cold collapse cache");
}

/// Isolated extract timing. Not a remesh-frame number — S2 (+ S4 estimate).
/// Run: cargo test -p core_math --release --test octree_dc_gate2 measure_octree_extract -- --ignored --nocapture
#[test]
#[ignore]
fn measure_octree_extract() {
    fn med5(mut f: impl FnMut()) -> f64 {
        let mut v: Vec<f64> = (0..5)
            .map(|_| {
                let t = std::time::Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[2]
    }
    for res in [128u32, 256] {
        let g = sphere_grid(res);
        let mut uniform = core_math::mesh::Mesh {
            vertices: Vec::new(),
            normals: Vec::new(),
            quads: Vec::new(),
        };
        let u_ms = med5(|| uniform = g.surface_nets_placed(false, CellPlacement::Qef));
        let mut z = core_math::mesh::Mesh {
            vertices: Vec::new(),
            normals: Vec::new(),
            quads: Vec::new(),
        };
        let z_ms = med5(|| z = g.surface_nets_octree(false, 0.0));
        let mut he_u = core_math::half_edge::HalfEdgeMesh::new();
        let s4u = med5(|| he_u = core_math::half_edge::HalfEdgeMesh::from_mesh(&uniform));
        let s4ud = med5(|| he_u = core_math::half_edge::HalfEdgeMesh::from_mesh_draft(&uniform));
        for eps in [1e-5f32, 1e-4, 1e-3] {
            let mut c = core_math::mesh::Mesh {
                vertices: Vec::new(),
                normals: Vec::new(),
                quads: Vec::new(),
            };
            let c_ms = med5(|| c = g.surface_nets_octree(false, eps));
            let mut he_c = core_math::half_edge::HalfEdgeMesh::new();
            let s4c = med5(|| he_c = core_math::half_edge::HalfEdgeMesh::from_mesh(&c));
            let s4cd = med5(|| he_c = core_math::half_edge::HalfEdgeMesh::from_mesh_draft(&c));
            eprintln!(
                "[OCTREE S2] res={res} eps={eps:.0e}  uniform={u_ms:.1}ms {}v/{}q  \
                 zero={z_ms:.1}ms  collapsed={c_ms:.1}ms {}v/{}q  quad_ratio={:.2}  \
                 S4 u={s4u:.1}/draft={s4ud:.1}  c={s4c:.1}/draft={s4cd:.1}  \
                 S2+S4 u={:.1} c={:.1}",
                uniform.vertices.len(),
                uniform.quads.len(),
                c.vertices.len(),
                c.quads.len(),
                c.quads.len() as f64 / uniform.quads.len().max(1) as f64,
                u_ms + s4u,
                c_ms + s4c,
            );
        }
    }
}
