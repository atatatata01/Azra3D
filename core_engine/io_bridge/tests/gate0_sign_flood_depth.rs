//! GATE 0 (`docs/planning/poly/REMESH_GPU_PLAN.md` §4) — MEASUREMENT ONLY, no production code.
//!
//! Decides Gate 0 option 1 ("full-grid iterative label propagation on the GPU") by measuring the
//! only quantity that governs its cost: **the level-synchronous BFS depth of the sign flood**.
//! A naive GPU propagation kernel advances the air frontier one cell per dispatch, so
//! `passes ≈ depth`. Depth in the single digits ⇒ option 1 is cheap. Depth in the hundreds ⇒
//! option 1 is dead on arrival and the gate goes to option 2 (hierarchical) or 3 (hybrid).
//!
//! **Why this can be measured through the public API.** The flood traverses exactly the cells it
//! marks air, and `band_finalize` writes far cells as ±band — so `d > 0` in the returned field is
//! precisely the set the flood reached. Cells that are walkable but unreachable (sealed voids) are
//! sealed solid and by definition never participate in propagation. No access to the private
//! `band_walkable` / `flood_air_reach` is needed, and none is taken.
//!
//! Run: cargo test -p io_bridge --release gate0 -- --ignored --nocapture

mod common;

use glam::Vec3;
use io_bridge::mesh_to_sdf::region_mesh_to_sdf_band_stats;

/// Level-synchronous 6-connected BFS from every border air cell, over the air set `d > 0`.
/// Returns `(max_depth, air_cells, reached, per_level_counts)`.
fn flood_depth(field: &[f32], res: usize) -> (u32, usize, usize, Vec<u32>) {
    let n = res * res * res;
    assert_eq!(field.len(), n);
    let air = |i: usize| field[i] > 0.0;
    let idx = |x: usize, y: usize, z: usize| x + y * res + z * res * res;

    let air_cells = (0..n).filter(|&i| air(i)).count();
    let mut seen = vec![false; n];
    let mut frontier: Vec<u32> = Vec::new();

    // Seed the six border faces (same seeding the sampler does).
    let mut seed = |x: usize, y: usize, z: usize, f: &mut Vec<u32>, seen: &mut Vec<bool>| {
        let i = idx(x, y, z);
        if air(i) && !seen[i] {
            seen[i] = true;
            f.push(i as u32);
        }
    };
    for y in 0..res {
        for x in 0..res {
            seed(x, y, 0, &mut frontier, &mut seen);
            seed(x, y, res - 1, &mut frontier, &mut seen);
        }
    }
    for z in 0..res {
        for x in 0..res {
            seed(x, 0, z, &mut frontier, &mut seen);
            seed(x, res - 1, z, &mut frontier, &mut seen);
        }
        for y in 0..res {
            seed(0, y, z, &mut frontier, &mut seen);
            seed(res - 1, y, z, &mut frontier, &mut seen);
        }
    }

    let mut per_level = vec![frontier.len() as u32];
    let mut reached = frontier.len();
    let mut depth = 0u32;
    let mut next: Vec<u32> = Vec::new();
    while !frontier.is_empty() {
        next.clear();
        for &c in &frontier {
            let i = c as usize;
            let x = i % res;
            let y = (i / res) % res;
            let z = i / (res * res);
            let mut push = |j: usize, next: &mut Vec<u32>, seen: &mut Vec<bool>| {
                if air(j) && !seen[j] {
                    seen[j] = true;
                    next.push(j as u32);
                }
            };
            if x > 0 { push(i - 1, &mut next, &mut seen); }
            if x + 1 < res { push(i + 1, &mut next, &mut seen); }
            if y > 0 { push(i - res, &mut next, &mut seen); }
            if y + 1 < res { push(i + res, &mut next, &mut seen); }
            if z > 0 { push(i - res * res, &mut next, &mut seen); }
            if z + 1 < res { push(i + res * res, &mut next, &mut seen); }
        }
        if next.is_empty() {
            break;
        }
        depth += 1;
        reached += next.len();
        per_level.push(next.len() as u32);
        std::mem::swap(&mut frontier, &mut next);
    }
    (depth, air_cells, reached, per_level)
}

/// Cup: a closed, manifold solid occupying `r_in..r_out` for `z in floor..h`, plus a solid disc
/// `0..r_out` for `z in 0..floor`. The cavity is open at the top only, so its air is reachable from
/// the lattice border ONLY down through the mouth — the adversarial case for frontier propagation,
/// and the shape the sampler's own docs call out ("a cup whose rim lies on the border plane",
/// "mug-neck phantom"). Centered on the origin in Z.
fn cup(r_out: f32, r_in: f32, h: f32, floor: f32, segs: u32) -> (Vec<Vec3>, Vec<u32>) {
    let mut v: Vec<Vec3> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let z0 = -h * 0.5;
    let z1 = h * 0.5;
    let zf = z0 + floor;
    let ang = |s: u32| {
        let t = s as f32 / segs as f32 * std::f32::consts::TAU;
        (t.cos(), t.sin())
    };
    // Two triangles for quad (a,b,c,d), wound so the face normal follows a→b→c.
    let quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, v: &mut Vec<Vec3>, idx: &mut Vec<u32>| {
        let base = v.len() as u32;
        v.extend_from_slice(&[a, b, c, d]);
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    // Disc wedges are TRIANGLES, not quads with a doubled centre vertex: a doubled corner emits a
    // second, zero-area triangle whose face normal is degenerate, and `signed_from_best` takes the
    // sign from whichever triangle is closest — so a garbage normal flips the sign of every cell
    // that lands nearest the disc. That is what made the first run of this probe report a cup whose
    // solid fraction disagreed between res 128 and 256 (7.2% vs 1.6%).
    let tri = |a: Vec3, b: Vec3, c: Vec3, v: &mut Vec<Vec3>, idx: &mut Vec<u32>| {
        let base = v.len() as u32;
        v.extend_from_slice(&[a, b, c]);
        idx.extend_from_slice(&[base, base + 1, base + 2]);
    };
    for s in 0..segs {
        let (c0, s0) = ang(s);
        let (c1, s1) = ang(s + 1);
        let (ox0, oy0) = (c0 * r_out, s0 * r_out);
        let (ox1, oy1) = (c1 * r_out, s1 * r_out);
        let (ix0, iy0) = (c0 * r_in, s0 * r_in);
        let (ix1, iy1) = (c1 * r_in, s1 * r_in);

        // 1. Outer side wall, normal +radial (outward).
        quad(
            Vec3::new(ox0, oy0, z0), Vec3::new(ox1, oy1, z0),
            Vec3::new(ox1, oy1, z1), Vec3::new(ox0, oy0, z1),
            &mut v, &mut idx,
        );
        // 2. Bottom disc at z0, normal -Z (outward).
        tri(
            Vec3::new(0.0, 0.0, z0), Vec3::new(ox1, oy1, z0), Vec3::new(ox0, oy0, z0),
            &mut v, &mut idx,
        );
        // 3. Rim annulus at z1 between r_in and r_out, normal +Z (outward).
        quad(
            Vec3::new(ix0, iy0, z1), Vec3::new(ox0, oy0, z1),
            Vec3::new(ox1, oy1, z1), Vec3::new(ix1, iy1, z1),
            &mut v, &mut idx,
        );
        // 4. Inner side wall, normal -radial (out of the solid = toward the axis).
        quad(
            Vec3::new(ix0, iy0, zf), Vec3::new(ix0, iy0, z1),
            Vec3::new(ix1, iy1, z1), Vec3::new(ix1, iy1, zf),
            &mut v, &mut idx,
        );
        // 5. Inner floor at zf, normal +Z (out of the solid, into the cavity).
        tri(
            Vec3::new(0.0, 0.0, zf), Vec3::new(ix0, iy0, zf), Vec3::new(ix1, iy1, zf),
            &mut v, &mut idx,
        );
    }
    (v, idx)
}

fn report(name: &str, verts: &[Vec3], indices: &[u32], bounds: f32, res: u32) {
    let cell = (bounds * 2.0) / (res as f32 - 1.0);
    let w = 2.0 * cell; // remesh band convention (write_region_from_sdf)
    let t = std::time::Instant::now();
    let Some((field, stats)) =
        region_mesh_to_sdf_band_stats(verts, indices, bounds, res, [0, 0, 0, res - 1, res - 1, res - 1], w)
    else {
        eprintln!("[GATE0] {name:<18} res={res:<4} SAMPLER RETURNED NONE");
        return;
    };
    let sample_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    let (depth, air, reached, per_level) = flood_depth(&field, res as usize);
    let bfs_ms = t.elapsed().as_secs_f64() * 1e3;
    let total = (res as usize).pow(3);

    // How much of the air is still unreached after a capped number of GPU passes? This is what
    // decides whether "propagate N times, else fall back" is viable or is just a fork.
    let cum = |n: usize| -> f64 {
        let s: u64 = per_level.iter().take(n + 1).map(|&c| c as u64).sum();
        100.0 * s as f64 / reached.max(1) as f64
    };
    eprintln!(
        "[GATE0] {name:<18} res={res:<4} air={:>5.1}% far_solid={:<8} sealed_pocket={:<6} \
         DEPTH={depth:<4} | reached after 8/16/32/64 passes: {:>5.1}/{:>5.1}/{:>5.1}/{:>5.1}% \
         | sample={sample_ms:>6.1}ms bfs={bfs_ms:>6.1}ms",
        100.0 * air as f64 / total as f64,
        stats.far_solid,
        stats.sealed_pocket,
        cum(8), cum(16), cum(32), cum(64),
    );
}

#[test]
#[ignore]
fn gate0_sign_flood_depth() {
    eprintln!(
        "\n[GATE0] Sign-flood propagation depth. passes(naive GPU frontier) ~= DEPTH.\n\
         [GATE0] Option 1 (full-grid iterative propagation) is viable only if DEPTH stays small.\n"
    );
    for res in [128u32, 256] {
        let (cv, ci) = common::cube(0.5);
        report("cube", &cv, &ci, 0.75, res);

        let (sv, si) = common::uv_sphere(64, 128, 0.5);
        report("sphere", &sv, &si, 0.75, res);

        // Shallow cup — mouth is wide relative to depth.
        let (uv, ui) = cup(0.55, 0.42, 0.5, 0.10, 96);
        report("cup(shallow)", &uv, &ui, 0.75, res);

        // Deep narrow cup — the adversarial case: the cavity air can only be reached by
        // travelling the full bore length from the mouth.
        let (dv, di) = cup(0.30, 0.22, 1.30, 0.08, 96);
        report("cup(deep bore)", &dv, &di, 0.75, res);
        eprintln!();
    }
}
