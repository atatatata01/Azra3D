// LAB SPIKE — poly-mesh-as-SDF-operand feasibility.
// Log: docs/planning/lab/poly-mesh-sdf-operand.md
//
// Measures the real cost of mesh→SDF conversion on THIS machine, through the
// production entry point `io_bridge::mesh_to_sdf::region_mesh_to_sdf` (the same
// function the voxel-sculpt MOVE tool already ships in the release path).
//
// Two scenarios:
//   1. FULL-GRID conversion — the one-shot cost when a poly mesh is first assigned
//      as an SDF operand (or destructively stamped). Paid once per mesh edit, NOT
//      per frame — moving/rotating the operand never re-converts (sampled through
//      the inverse transform).
//   2. REGION update — the incremental cost of a debounced partial re-convert
//      while live-editing the poly mesh's vertices (dirty-AABB style).
//
// Run:
//   cargo test -p io_bridge --release --test mesh_sdf_perf -- --ignored --nocapture

mod common;

use common::uv_sphere;
use glam::Vec3;
use io_bridge::mesh_to_sdf::{region_mesh_to_sdf, region_mesh_to_sdf_band};
use std::time::Instant;

const BOUNDS: f32 = 2.0; // matches cpu_volume default: SdfVolume::new_empty(128, 2.0)

fn bench_case(label: &str, verts: &[Vec3], indices: &[u32], res: u32, aabb: [u32; 6]) {
    let [x0, y0, z0, x1, y1, z1] = aabb;
    let (nx, ny, nz) = (
        (x1 - x0 + 1) as usize,
        (y1 - y0 + 1) as usize,
        (z1 - z0 + 1) as usize,
    );
    let t0 = Instant::now();
    let out = region_mesh_to_sdf(verts, indices, BOUNDS, res, aabb).expect("conversion failed");
    let dt = t0.elapsed();
    let cells = nx * ny * nz;
    assert_eq!(out.len(), cells);

    // Sign sanity: when the region covers the sphere centre, the centre cell must be inside (< 0).
    let mid = res / 2;
    let sign_note = if (x0..=x1).contains(&mid) && (y0..=y1).contains(&mid) && (z0..=z1).contains(&mid) {
        let c = ((mid - x0) as usize)
            + ((mid - y0) as usize) * nx
            + ((mid - z0) as usize) * nx * ny;
        assert!(out[c] < 0.0, "{label}: centre sample not inside (sdf={})", out[c]);
        format!("sdf(centre)={:+.3}", out[c])
    } else {
        String::from("(off-centre region)")
    };

    println!(
        "{label:<34} tris={:>7}  region={nx}x{ny}x{nz} ({:>9} cells)  time={:>9.1?}  mem={:>6.1} MB  {sign_note}",
        indices.len() / 3,
        cells,
        dt,
        (cells * 4) as f32 / 1.0e6,
    );
}

#[test]
#[ignore]
fn bench_mesh_to_sdf() {
    println!("\n=== LAB SPIKE: mesh→SDF conversion cost (release, this machine) ===\n");

    let low = uv_sphere(50, 52, 1.5); //   ~5k tris
    let mid = uv_sphere(100, 102, 1.5); // ~20k tris
    let high = uv_sphere(200, 202, 1.5); // ~80k tris

    println!("-- Scenario 1: FULL-GRID conversion (one-shot, on mesh assign / mesh edit commit) --");
    let full = |res: u32| [0, 0, 0, res - 1, res - 1, res - 1];
    bench_case("full  5k tris @  64^3", &low.0, &low.1, 64, full(64));
    bench_case("full  5k tris @ 128^3", &low.0, &low.1, 128, full(128));
    bench_case("full 20k tris @  64^3", &mid.0, &mid.1, 64, full(64));
    bench_case("full 20k tris @ 128^3", &mid.0, &mid.1, 128, full(128));
    bench_case("full 20k tris @ 256^3", &mid.0, &mid.1, 256, full(256));
    bench_case("full 80k tris @ 128^3", &high.0, &high.1, 128, full(128));

    println!("\n-- Scenario 1b: NARROW-BAND conversion (Remesh modifier live path) --");
    let band_case = |label: &str, verts: &[Vec3], indices: &[u32], res: u32| {
        let cell = (BOUNDS * 2.0) / (res as f32 - 1.0);
        let band = 2.0 * cell; // w in write_region_from_sdf — the remesh wiring
        let t0 = Instant::now();
        let out = region_mesh_to_sdf_band(
            verts, indices, BOUNDS, res,
            [0, 0, 0, res - 1, res - 1, res - 1], band,
        )
        .expect("band conversion failed");
        let dt = t0.elapsed();
        let mid = (res / 2) as usize;
        let c = mid + mid * res as usize + mid * (res as usize).pow(2);
        assert!(out[c] < 0.0, "{label}: centre sample not inside (sdf={})", out[c]);
        println!(
            "{label:<34} tris={:>7}  region={res}x{res}x{res}  time={:>9.1?}",
            indices.len() / 3, dt,
        );
    };
    band_case("band  5k tris @  64^3", &low.0, &low.1, 64);
    band_case("band  5k tris @ 128^3", &low.0, &low.1, 128);
    band_case("band 20k tris @  64^3", &mid.0, &mid.1, 64);
    band_case("band 20k tris @ 128^3", &mid.0, &mid.1, 128);
    band_case("band 20k tris @ 256^3", &mid.0, &mid.1, 256);
    band_case("band 80k tris @ 128^3", &high.0, &high.1, 128);

    println!("\n-- Scenario 2: REGION update (debounced incremental, during live poly edit) --");
    // Centred sub-regions of a 128^3 lattice.
    let centered = |n: u32| {
        let lo = 64 - n / 2;
        let hi = lo + n - 1;
        [lo, lo, lo, hi, hi, hi]
    };
    bench_case("region 24^3 of 128, 20k tris", &mid.0, &mid.1, 128, centered(24));
    bench_case("region 48^3 of 128, 20k tris", &mid.0, &mid.1, 128, centered(48));
    bench_case("region 32^3 of 128, 80k tris", &high.0, &high.1, 128, centered(32));

    println!();
}
