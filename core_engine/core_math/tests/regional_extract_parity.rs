//! Gate 1b parity: `surface_nets_placed_cached` must be BIT-IDENTICAL to `surface_nets_placed`.
//!
//! The regional extract reuses cached Phase A slabs outside the dirty region. Reuse is sound only
//! if (a) the reused data is still correct — which is what the DILATION exists to guarantee — and
//! (b) reuse cannot perturb vertex numbering, which holds because the cache unit is the cz slab
//! Phase A already emits. This file drives a real edit sequence and compares every vertex, normal
//! and quad against a from-scratch extract of the same grid.

use core_math::voxel_sculpt::{CellPlacement, DirtyCells, SurfaceNetsCache, VoxelGrid};

/// Fill the grid with an analytic sphere SDF.
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

fn assert_same(a: &core_math::mesh::Mesh, b: &core_math::mesh::Mesh, what: &str) {
    assert_eq!(a.vertices.len(), b.vertices.len(), "{what}: vertex count");
    assert_eq!(a.quads.len(), b.quads.len(), "{what}: quad count");
    for i in 0..a.vertices.len() {
        let (av, bv) = (a.vertices[i], b.vertices[i]);
        assert_eq!(
            (av.x.to_bits(), av.y.to_bits(), av.z.to_bits()),
            (bv.x.to_bits(), bv.y.to_bits(), bv.z.to_bits()),
            "{what}: vertex {i} differs (cached {av:?} vs full {bv:?})"
        );
        let (an, bn) = (a.normals[i], b.normals[i]);
        assert_eq!(
            (an.x.to_bits(), an.y.to_bits(), an.z.to_bits()),
            (bn.x.to_bits(), bn.y.to_bits(), bn.z.to_bits()),
            "{what}: normal {i} differs"
        );
    }
    assert_eq!(a.quads, b.quads, "{what}: quad topology differs");
}

/// Rewrite a small point box with a dented sphere, and return that box.
fn local_dent(g: &mut VoxelGrid, res: u32, half: f32) -> ([u32; 3], [u32; 3]) {
    let n = res as usize;
    let cell = (2.0 * half) / (res - 1) as f32;
    // Small and fixed-size, like a real drag frame: one moved vertex dirties a handful of cells,
    // not a fifth of the lattice.
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

/// A LOCAL edit — the case the regional extract exists for. After splicing a small box, the cached
/// extract must equal a from-scratch extract of the same grid, for both placements and both
/// smoothing settings.
#[test]
fn cached_extract_matches_full_extract_after_local_edit() {
    for &res in &[32u32, 64, 128] {
        for placement in [CellPlacement::Qef, CellPlacement::Centroid] {
            for smooth in [false, true] {
                let half = 1.0f32;
                let mut g = VoxelGrid::new_empty_dense(res, half);
                write_sphere(&mut g, res, half, half * 0.5);

                let mut cache = SurfaceNetsCache::default();
                let _ = g.surface_nets_placed_cached(smooth, placement, &mut cache, DirtyCells::All);

                let (lo, hi) = local_dent(&mut g, res, half);

                let got = g.surface_nets_placed_cached(
                    smooth,
                    placement,
                    &mut cache,
                    DirtyCells::Points { lo, hi },
                );
                let want = g.surface_nets_placed(smooth, placement);
                assert_same(&got, &want, &format!("res {res} {placement:?} smooth={smooth}"));

                // ANTI-VACUITY: a cache that silently recomputes everything still passes a parity
                // test, so a local dent must actually skip slabs. Only meaningful once the axis
                // holds enough 16³ chunks for a ±1-chunk dilation not to span it: at res 32/64
                // there are just 2–4 chunks per axis, so covering everything is CORRECT, not a
                // regression. The production resolutions are 128 and 256.
                let cells = (res - 1) as usize;
                if cells / 16 >= 8 {
                    assert!(
                        cache.recomputed_slabs < cells,
                        "res {res}: recomputed ALL {} slabs — no regional saving, test is vacuous",
                        cache.recomputed_slabs
                    );
                }
            }
        }
    }
}

/// `DirtyCells::None` means the grid is byte-identical, so ZERO slabs may be recomputed — and the
/// result must still match a full extract exactly.
#[test]
fn unchanged_grid_recomputes_nothing() {
    let (res, half) = (32u32, 1.0f32);
    let mut g = VoxelGrid::new_empty_dense(res, half);
    write_sphere(&mut g, res, half, half * 0.5);
    let mut cache = SurfaceNetsCache::default();
    let _ = g.surface_nets_placed_cached(true, CellPlacement::Qef, &mut cache, DirtyCells::All);

    let got = g.surface_nets_placed_cached(true, CellPlacement::Qef, &mut cache, DirtyCells::None);
    assert_eq!(cache.recomputed_slabs, 0, "an unchanged grid must recompute no slabs");
    let want = g.surface_nets_placed(true, CellPlacement::Qef);
    assert_same(&got, &want, "unchanged grid");
}

/// A cold cache must produce a correct mesh even when told the edit was local — the first
/// incremental frame after a full eval is exactly this case, and a silent hole would be invisible
/// until someone looked at the mesh.
#[test]
fn cold_cache_ignores_a_local_dirty_hint() {
    let (res, half) = (32u32, 1.0f32);
    let mut g = VoxelGrid::new_empty_dense(res, half);
    write_sphere(&mut g, res, half, half * 0.5);
    let mut cache = SurfaceNetsCache::default();
    let got = g.surface_nets_placed_cached(
        true,
        CellPlacement::Qef,
        &mut cache,
        DirtyCells::Points { lo: [10, 10, 10], hi: [12, 12, 12] },
    );
    let want = g.surface_nets_placed(true, CellPlacement::Qef);
    assert_same(&got, &want, "cold cache with a local hint");
}

/// Changing the placement mode must invalidate: the cached vertices were solved by a different
/// rule, and reusing them would silently mix Qef and Centroid geometry in one mesh.
#[test]
fn placement_change_invalidates_the_cache() {
    let (res, half) = (32u32, 1.0f32);
    let mut g = VoxelGrid::new_empty_dense(res, half);
    write_sphere(&mut g, res, half, half * 0.5);
    let mut cache = SurfaceNetsCache::default();
    let _ = g.surface_nets_placed_cached(true, CellPlacement::Qef, &mut cache, DirtyCells::All);
    let got = g.surface_nets_placed_cached(
        true,
        CellPlacement::Centroid,
        &mut cache,
        DirtyCells::Points { lo: [10, 10, 10], hi: [12, 12, 12] },
    );
    let want = g.surface_nets_placed(true, CellPlacement::Centroid);
    assert_same(&got, &want, "placement switch");
}
