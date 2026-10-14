//! SPIKE E (runner) — per-bake CPU cost of `classify_grid_to_sparse` at the visible-512 resolutions.
//!
//! GATE for "visible-512 step 3" (feed the sparse mesher from a dense res³ field bake + a CPU classify
//! on every bake). The recon measured the classify at ~2.4s single-threaded @512³; it's since been
//! rayon-parallelised. If the parallel classify is <~30ms it can run per-bake and the dense-field interim
//! ships now (3A's brick-addressed GPU bake DEFERRED). If it's >~100ms it would hitch every drag-release
//! bake and 3A must come first.
//!
//! Lives as an example, not a #[test], because a frozen-file test in half_edge.rs (`validate_mesh`)
//! currently breaks the whole core_math test build. The canonical #[ignore] home is
//! `chunked_dc.rs::spike_e_classify_cost`. Run:
//!   cargo run -p core_math --example spike_e_classify --release

use core_math::chunked_dc::classify_grid_to_sparse;
use std::time::Instant;

/// Dense box SDF over [-1,1]³ at `res` cells (nverts = res+1). Mirrors chunked_dc.rs::make_box_grid.
fn make_box_grid(res: usize, half: f32) -> Vec<f32> {
    let nverts = res + 1;
    let mut grid = vec![0.0f32; nverts * nverts * nverts];
    for gz in 0..nverts {
        for gy in 0..nverts {
            for gx in 0..nverts {
                let p = [
                    (gx as f32 / res as f32) * 2.0 - 1.0,
                    (gy as f32 / res as f32) * 2.0 - 1.0,
                    (gz as f32 / res as f32) * 2.0 - 1.0,
                ];
                let q = [p[0].abs() - half, p[1].abs() - half, p[2].abs() - half];
                let outside =
                    (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
                let inside = q[0].max(q[1]).max(q[2]).min(0.0);
                grid[gx + gy * nverts + gz * nverts * nverts] = outside + inside;
            }
        }
    }
    grid
}

fn main() {
    println!("\n=== SPIKE E: classify_grid_to_sparse per-bake cost (tight band, 8\u{b3} bricks) ===");
    for &res in &[256u32, 384, 512] {
        let nverts = res + 1;
        let half = 0.6; // box filling most of [-1,1] → realistic surface-brick count
        let grid = make_box_grid(res as usize, half);
        let cell = 2.0 / res as f32;
        let band = 2.0 * cell; // tight band (apron, not band, guarantees correctness — SPIKE A/B finding)

        // Warm-up: excludes the one-time rayon pool spin-up + gives the footprint stats.
        let warm = classify_grid_to_sparse(&grid, nverts, 8, band);
        let dims = warm.dims_bricks;
        let total_bricks = (dims[0] * dims[1] * dims[2]) as usize;
        let alloc = warm.allocated_bricks();
        let pool_mb = (alloc as f64 * 512.0 * 4.0) / (1024.0 * 1024.0);
        let index_mb = (total_bricks as f64 * 4.0) / (1024.0 * 1024.0);
        let dense_mb = (nverts as f64).powi(3) * 4.0 / (1024.0 * 1024.0);

        // Best of 3 timed runs (steady-state per-bake cost).
        let mut best_ms = f64::INFINITY;
        for _ in 0..3 {
            let t = Instant::now();
            let s = classify_grid_to_sparse(&grid, nverts, 8, band);
            best_ms = best_ms.min(t.elapsed().as_secs_f64() * 1000.0);
            std::hint::black_box(&s);
        }

        println!(
            "  res={res:>4}\u{b3}  classify={best_ms:>7.2}ms   bricks={alloc}/{total_bricks} ({:.1}% surface)   \
             sparse_pool={pool_mb:.1}MB + index={index_mb:.2}MB   vs dense_field={dense_mb:.0}MB",
            100.0 * alloc as f64 / total_bricks as f64,
        );
    }
    println!("  GATE: classify <~30ms @512\u{b3} -> dense-field+classify interim viable NOW (defer 3A GPU sparse bake).");
    println!("        classify >~100ms       -> 3A brick-addressed GPU bake needed first.\n");
}
