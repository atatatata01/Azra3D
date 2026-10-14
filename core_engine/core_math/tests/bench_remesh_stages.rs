//! PERF HARNESS — poly Voxel Remesh modifier stage costs (core_math side).
//!
//! Measures each stage of the remesh pipeline in isolation so regressions are
//! attributable: density write → Surface Nets (rough/smooth) → chunked parallel
//! mesher (reference ceiling) → half-edge rebuild → retriangulation.
//! The mesh→SDF stage lives in io_bridge (`mesh_sdf_perf.rs`).
//!
//! Baseline (2026-08-08, this machine, release):
//!   res=128  density=5.4ms  SN_rough=37.6ms  SN_smooth=42.4ms  chunked_par=4.6ms
//!            from_mesh=21.0ms  to_raster=10.7ms
//!
//! Run: cargo test -p core_math --release --test bench_remesh_stages -- --ignored --nocapture

use core_math::voxel_sculpt::VoxelGrid;
use std::time::Instant;

/// Analytic sphere SDF on the grid lattice — isolates meshing cost from mesh→SDF cost.
fn sphere_sdf(res: u32, half: f32) -> Vec<f32> {
    let n = res as usize;
    let mut out = vec![0.0f32; n * n * n];
    let cell = (2.0 * half) / (res - 1) as f32;
    let r = half * 0.7;
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let p = glam::Vec3::new(
                    -half + x as f32 * cell,
                    -half + y as f32 * cell,
                    -half + z as f32 * cell,
                );
                out[x + y * n + z * n * n] = p.length() - r;
            }
        }
    }
    out
}

#[test]
#[ignore] // perf harness — run explicitly with --ignored --nocapture
fn bench_remesh_stages() {
    for res in [32u32, 64, 128, 256] {
        let half = 1.0f32;
        let sdf = sphere_sdf(res, half);

        let mut grid = VoxelGrid::new_empty_dense(res, half);
        let w = 2.0 * grid.voxel_size();
        let t1 = Instant::now();
        grid.write_region_from_sdf_ex([0, 0, 0, res - 1, res - 1, res - 1], &sdf, w, false);
        let t_density = t1.elapsed();

        let t2 = Instant::now();
        let m_rough = grid.surface_nets(false);
        let t_sn_rough = t2.elapsed();

        let t3 = Instant::now();
        let m = grid.surface_nets(true);
        let t_sn_smooth = t3.elapsed();

        // S2 AS THE REMESH ACTUALLY RUNS IT. `surface_nets(true)` is the CENTROID placement
        // (the sculpt display path); the poly Remesh modifier calls
        // `surface_nets_placed(smooth, CellPlacement::Qef)` (app/src/poly/remesh.rs:93), which
        // solves a QEF per cell and resamples a gradient per crossing. Timing only the centroid
        // path understates the stage this plan is trying to make regional.
        let t3q = Instant::now();
        let m_qef = grid.surface_nets_placed(true, core_math::voxel_sculpt::CellPlacement::Qef);
        let t_sn_qef = t3q.elapsed();

        let t4 = Instant::now();
        let (cv, _ci) = grid.object_display_mesh_chunked(16);
        let t_chunked = t4.elapsed();

        let t5 = Instant::now();
        let he = core_math::half_edge::HalfEdgeMesh::from_mesh(&m);
        let t_he = t5.elapsed();

        let t6 = Instant::now();
        let (rv, _ri) = he.to_raster_data();
        let t_raster = t6.elapsed();

        // Mandatory per-incremental-frame enclosure reconcile (see seal_enclosed_air docs) —
        // whole-field span flood, so it does NOT shrink with the dirty region. This is the
        // floor V1 cannot go below; only V2's incremental extract changes the shape.
        let t7 = Instant::now();
        let sealed = grid.seal_enclosed_air();
        let t_seal = t7.elapsed();

        println!(
            "res={res:3}  density={:>7.1}ms  SN_rough={:>7.1}ms  SN_smooth={:>7.1}ms  \
             SN_QEF={:>7.1}ms  chunked_par={:>7.1}ms  from_mesh={:>7.1}ms  to_raster={:>7.1}ms  \
             seal={:>7.1}ms(filled {})  | quads={} verts={} qef_q={} qef_v={} chunked_v={} raster_v={}",
            t_density.as_secs_f64() * 1e3,
            t_sn_rough.as_secs_f64() * 1e3,
            t_sn_smooth.as_secs_f64() * 1e3,
            t_sn_qef.as_secs_f64() * 1e3,
            t_chunked.as_secs_f64() * 1e3,
            t_he.as_secs_f64() * 1e3,
            t_raster.as_secs_f64() * 1e3,
            t_seal.as_secs_f64() * 1e3,
            sealed,
            m.quads.len(),
            m.vertices.len(),
            m_qef.quads.len(),
            m_qef.vertices.len(),
            cv.len(),
            rv.len(),
        );
        let _ = m_rough;
    }
}
