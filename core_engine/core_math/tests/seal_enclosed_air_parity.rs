//! Correctness gate for `VoxelGrid::seal_enclosed_air`.
//!
//! `seal_enclosed_air` had **no test of any kind** before this file, despite guarding the one-way
//! `had_enclosed_void` lockout (red line #1 of `docs/planning/poly/REMESH_ADAPTIVE_OCTREE_PLAN.md`).
//!
//! It was originally written here as a *bit-identical* gate against a verbatim copy of the shipped
//! span flood. That gate then **failed the shipped flood itself**: replacing it with run-based
//! connected components (Gate 1b) changed the output, and inspection showed the OLD code was the
//! wrong one — it emitted FALSE SEALS, filling air that is genuinely reachable from the border.
//! See `does_not_false_seal_reachable_air` for the mechanism.
//!
//! So the oracle here is no longer "whatever the old code did". It is `seal_reference` below: a
//! plain 6-connected BFS with a SEPARATE visited array, written to be obviously correct rather
//! than fast. Aliasing "is air" with "has been visited" is precisely what broke the old flood, so
//! the oracle must not do it.

use core_math::voxel_sculpt::VoxelGrid;

/// OBVIOUSLY-CORRECT oracle: 6-connected BFS from every border air cell, with a **separate**
/// `seen` array so "is air" and "has been visited" can never alias. Deliberately slow and dumb —
/// its only job is to be trustworthy. Returns the number of cells filled.
fn seal_reference(dens: &mut [f32], n: usize, iso: f32) -> usize {
    let idx = |x: usize, y: usize, z: usize| x + y * n + z * n * n;
    let air: Vec<bool> = dens.iter().map(|&v| v < iso).collect();
    let mut seen = vec![false; n * n * n];
    let mut q: std::collections::VecDeque<(usize, usize, usize)> = std::collections::VecDeque::new();
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                if x == 0 || y == 0 || z == 0 || x == n - 1 || y == n - 1 || z == n - 1 {
                    let i = idx(x, y, z);
                    if air[i] && !seen[i] {
                        seen[i] = true;
                        q.push_back((x, y, z));
                    }
                }
            }
        }
    }
    while let Some((x, y, z)) = q.pop_front() {
        let nbrs = [
            (x.wrapping_sub(1), y, z, x > 0),
            (x + 1, y, z, x + 1 < n),
            (x, y.wrapping_sub(1), z, y > 0),
            (x, y + 1, z, y + 1 < n),
            (x, y, z.wrapping_sub(1), z > 0),
            (x, y, z + 1, z + 1 < n),
        ];
        for (nx, ny, nz, ok) in nbrs {
            if !ok {
                continue;
            }
            let i = idx(nx, ny, nz);
            if air[i] && !seen[i] {
                seen[i] = true;
                q.push_back((nx, ny, nz));
            }
        }
    }
    let mut filled = 0usize;
    for i in 0..air.len() {
        if air[i] && !seen[i] {
            dens[i] = 1.0;
            filled += 1;
        }
    }
    filled
}

/// Deterministic LCG — no `rand` dependency, and the same field every run so a failure is
/// reproducible from the fixture name alone.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32) / ((1u64 << 31) as f32)
    }
}

/// Build a density field for a named fixture. Coordinates are cell indices.
fn fixture(name: &str, n: usize) -> Vec<f32> {
    let mut d = vec![0.0f32; n * n * n];
    let c = (n as f32 - 1.0) * 0.5;
    let mut rng = Lcg(0x5EA1_5EED);
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let i = x + y * n + z * n * n;
                let (fx, fy, fz) = (x as f32 - c, y as f32 - c, z as f32 - c);
                let r = (fx * fx + fy * fy + fz * fz).sqrt();
                d[i] = match name {
                    // Hollow shell => a genuine ENCLOSED cavity at the centre. The case the
                    // whole function exists for.
                    "hollow_shell" => {
                        if r > c * 0.45 && r < c * 0.70 { 1.0 } else { 0.0 }
                    }
                    // Solid ball => air is all border-connected, nothing to fill.
                    "solid_ball" => {
                        if r < c * 0.70 { 1.0 } else { 0.0 }
                    }
                    // Two nested shells => two separate enclosed cavities.
                    "nested_shells" => {
                        let a = r > c * 0.60 && r < c * 0.75;
                        let b = r > c * 0.25 && r < c * 0.35;
                        if a || b { 1.0 } else { 0.0 }
                    }
                    // Shell with a hole punched through +x => cavity is NOT enclosed.
                    "shell_with_vent" => {
                        let shell = r > c * 0.45 && r < c * 0.70;
                        let vent = fy.abs() < 1.5 && fz.abs() < 1.5 && fx > 0.0;
                        if shell && !vent { 1.0 } else { 0.0 }
                    }
                    "all_air" => 0.0,
                    "all_solid" => 1.0,
                    // The strongest fixture: random solid/air produces many tiny enclosed
                    // pockets and an irregular border, exercising the span flood's row scans.
                    "random" => {
                        if rng.next_f32() < 0.5 { 1.0 } else { 0.0 }
                    }
                    // Random but with a guaranteed air border (the remesh lattice's contract).
                    "random_air_border" => {
                        let border = x < 2 || y < 2 || z < 2 || x + 2 >= n || y + 2 >= n || z + 2 >= n;
                        if border {
                            0.0
                        } else if rng.next_f32() < 0.55 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    _ => unreachable!("unknown fixture {name}"),
                };
            }
        }
    }
    d
}

fn grid_from(d: &[f32], n: u32) -> VoxelGrid {
    let mut g = VoxelGrid::new_empty_dense(n, 1.0);
    for (i, &v) in d.iter().enumerate() {
        g.density.set_idx(i, v);
    }
    g
}

#[test]
fn seal_enclosed_air_is_bit_identical_to_reference() {
    const FIXTURES: &[&str] = &[
        "hollow_shell",
        "solid_ball",
        "nested_shells",
        "shell_with_vent",
        "all_air",
        "all_solid",
        "random",
        "random_air_border",
    ];
    // 17³ and 33³ are deliberately not multiples of 64 — they caught the bitset packing's
    // off-by-one when the mask was a bitset, and they still exercise ragged run/row edges now.
    // 8 is REMESH_RES_MIN.
    for &n in &[8u32, 17, 32, 33, 64] {
        for &name in FIXTURES {
            let base = fixture(name, n as usize);

            let mut ref_dens = base.clone();
            let ref_filled = seal_reference(&mut ref_dens, n as usize, grid_from(&base, n).iso);

            let mut g = grid_from(&base, n);
            let got_filled = g.seal_enclosed_air();

            assert_eq!(
                got_filled, ref_filled,
                "{name} @ res {n}: filled count differs (impl {got_filled} vs BFS oracle {ref_filled})"
            );
            for i in 0..base.len() {
                let got = g.density.get_idx(i);
                assert_eq!(
                    got.to_bits(),
                    ref_dens[i].to_bits(),
                    "{name} @ res {n}: density differs at flat index {i} \
                     (impl {got}, BFS oracle {})",
                    ref_dens[i]
                );
            }
        }
    }
}

/// The property the remesh actually depends on, asserted directly rather than via the oracle:
/// a sealed cavity is filled, an open one is not, and sealing is idempotent (a second call must
/// fill nothing — the incremental path re-seals an already-sealed field every drag frame, and
/// `had_enclosed_void` would latch permanently if that second call reported a fill).
#[test]
fn seal_fills_only_enclosed_cavities_and_is_idempotent() {
    for &n in &[17u32, 32] {
        let mut sealed = grid_from(&fixture("hollow_shell", n as usize), n);
        let first = sealed.seal_enclosed_air();
        assert!(first > 0, "hollow shell @ {n} must have an enclosed cavity to fill");
        assert_eq!(
            sealed.seal_enclosed_air(),
            0,
            "re-sealing an already-sealed field must fill nothing @ res {n} — a non-zero \
             second call would latch had_enclosed_void and banish the body to the full path"
        );

        // A vent to the border must leave the CENTRAL CAVITY unfilled. It is not exactly zero:
        // discretising a spherical shell traps a handful of single air cells inside the shell
        // WALL itself, and those really are enclosed, so filling them is correct. The property
        // that matters is that the cavity — two orders of magnitude larger — survives.
        let mut vented = grid_from(&fixture("shell_with_vent", n as usize), n);
        let vented_filled = vented.seal_enclosed_air();
        assert!(
            vented_filled * 10 < first,
            "venting a shell to the border must leave its cavity OPEN @ res {n}: \
             vented filled {vented_filled}, sealed filled {first} — expected the vented case to \
             be a tiny fraction (stray cells trapped in the shell wall), not the whole cavity"
        );
    }
}

/// REGRESSION — the shipped span flood emitted **FALSE SEALS**: it filled air that is genuinely
/// reachable from the border, permanently, and then latched `had_enclosed_void` on the strength of
/// it.
///
/// MECHANISM: the flood's `air` array aliased "is air" with "not yet visited". `visit_row` pushed
/// only the FIRST cell of a run it found, then cleared the rest of that run **without pushing it**,
/// and bounded the clear to the parent span's `x1`. On popping the pushed cell, its right-expansion
/// immediately saw the next cell already cleared and stopped. So (a) the cleared tail never seeded
/// any neighbour-row scan, and (b) whatever lay beyond `x1` in that run was never visited at all —
/// reported unreachable, then filled solid.
///
/// Found 2026-08-09 when run-based connected components disagreed with it; `project_seal_enclosed_air_false_seals`
/// records the same aliased-mask class biting once before elsewhere.
#[test]
fn does_not_false_seal_reachable_air() {
    let n = 8usize;
    let base = fixture("random_air_border", n);
    let mut g = grid_from(&base, n as u32);
    let iso = g.iso;
    g.seal_enclosed_air();
    // Both cells are air, and each has an air neighbour one step further out that sits against an
    // all-air border plane — so both are reachable in two steps. The old flood filled both.
    for &(x, y, z) in &[(4usize, 2usize, 3usize), (3usize, 5usize, 3usize)] {
        let i = x + y * n + z * n * n;
        assert!(base[i] < iso, "fixture precondition: ({x},{y},{z}) must start as air");
        assert!(
            g.density.get_idx(i) < iso,
            "({x},{y},{z}) is reachable from the border and must NOT be sealed —              this is the false-seal regression"
        );
    }
}
