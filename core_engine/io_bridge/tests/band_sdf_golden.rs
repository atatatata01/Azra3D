//! GOLDEN GATE — `region_mesh_to_sdf_band` vs the full sampler `region_mesh_to_sdf`.
//!
//! The narrow-band sampler must be indistinguishable from the full sampler at the level
//! its consumers observe:
//!   * exact (bit-identical) signed distance for every cell within the band,
//!   * correct sign + saturated magnitude (±band) for every far cell,
//!   * bit-identical DENSITY after the sculpt mapping `clamp(0.5 − d/w)` with `w ≤ band`
//!     — this is the value the Remesh modifier's Surface Nets actually reads, so equality
//!     here proves the remeshed output mesh is IDENTICAL through either sampler.
//!
//! Cube (12 tris) exercises the full sampler's BRUTE dispatch (bit-exact tie-break parity);
//! the 5k-tri sphere exercises its spatial-HASH dispatch (tie-break may differ in the last
//! ulp → tolerance compare there).

mod common;

use io_bridge::mesh_to_sdf::{
    region_mesh_to_sdf, region_mesh_to_sdf_band, region_mesh_to_sdf_band_seeded,
};

/// Sculpt density mapping — mirrors `VoxelGrid::write_region_from_sdf` (ops.rs) EXACTLY,
/// including the multiply-by-reciprocal form: `0.5 - d * inv_w` is not IEEE-identical to
/// `0.5 - d / w`, and this test's claim is bit-identity through the PRODUCTION formula.
fn density(d: f32, w: f32) -> f32 {
    let inv_w = 1.0 / w.max(1e-6);
    (0.5 - d * inv_w).clamp(0.0, 1.0)
}

/// Run both samplers over the full lattice and compare.
/// `exact` = bit-identical in-band compare (brute-dispatch meshes only).
fn compare(verts: &[glam::Vec3], indices: &[u32], bounds: f32, res: u32, exact: bool) {
    let full_aabb = [0, 0, 0, res - 1, res - 1, res - 1];
    let cell = (bounds * 2.0) / (res as f32 - 1.0);
    let w = 2.0 * cell; // remesh band width (write_region_from_sdf convention)
    let band = w;

    let full = region_mesh_to_sdf(verts, indices, bounds, res, full_aabb).expect("full");
    let band_sdf =
        region_mesh_to_sdf_band(verts, indices, bounds, res, full_aabb, band).expect("band");
    assert_eq!(full.len(), band_sdf.len());

    let mut checked_in = 0usize;
    let mut checked_far = 0usize;
    for i in 0..full.len() {
        let (f, b) = (full[i], band_sdf[i]);
        if f.abs() <= band * 0.98 {
            // In-band: same closest_on_tri, same tie-break → identical.
            if exact {
                assert_eq!(f.to_bits(), b.to_bits(), "in-band mismatch at {i}: full={f} band={b}");
            } else {
                assert!((f - b).abs() <= 1e-5, "in-band mismatch at {i}: full={f} band={b}");
            }
            checked_in += 1;
        } else if f.abs() >= band * 1.02 {
            // Far: saturated magnitude, matching sign.
            assert_eq!(b.abs(), band, "far cell {i} not saturated: band={b}");
            assert_eq!(
                f.signum(), b.signum(),
                "far sign mismatch at {i}: full={f} band={b}"
            );
            checked_far += 1;
        }
        // Cells within 2% of the band boundary: classification may differ — density
        // equality below still covers them (both sides saturate identically at |d| ≥ w/2).
        let (df, db) = (density(f, w), density(b, w));
        if exact {
            assert_eq!(df.to_bits(), db.to_bits(), "density mismatch at {i}: {df} vs {db}");
        } else {
            assert!((df - db).abs() <= 1e-5, "density mismatch at {i}: {df} vs {db}");
        }
    }
    // Sanity: the compare actually exercised both regimes.
    assert!(checked_in > 100, "too few in-band cells checked ({checked_in})");
    assert!(checked_far > 1000, "too few far cells checked ({checked_far})");
    // Interior must exist and read inside (negative density > 0.5 ⇒ solid).
    let mid = ((res / 2) as usize)
        + ((res / 2) as usize) * res as usize
        + ((res / 2) as usize) * (res as usize).pow(2);
    assert!(band_sdf[mid] < 0.0, "solid centre not negative: {}", band_sdf[mid]);
}

#[test]
fn cube_brute_dispatch_bit_exact() {
    let (v, i) = common::cube(0.9);
    compare(&v, &i, 1.0, 48, true);
    compare(&v, &i, 1.0, 64, true);
}

#[test]
fn sphere_hash_dispatch_tolerance() {
    let (v, i) = common::uv_sphere(50, 52, 1.5); // ~5k tris → hash dispatch in the full sampler
    compare(&v, &i, 2.0, 64, false);
}

#[test]
fn band_region_subrange_matches_full_lattice_layout() {
    // Off-centre sub-region: same lattice contract as region_mesh_to_sdf (world coords from
    // GLOBAL indices, output in region order). The region border must lie in AIR — that is
    // the band sampler's documented contract (remesh guarantees it via AABB padding); a
    // border cutting the solid would seed the sign flood inside it. Cube half=0.9 spans
    // indices ≈2.4..44.6 of the 48 lattice, so [1..46] keeps every border cell outside.
    let (v, i) = common::cube(0.9);
    let res = 48u32;
    let aabb = [1, 1, 1, 46, 46, 45];
    let cell = 2.0 / (res as f32 - 1.0);
    let band = 2.0 * cell;
    let full = region_mesh_to_sdf(&v, &i, 1.0, res, aabb).expect("full");
    let b = region_mesh_to_sdf_band(&v, &i, 1.0, res, aabb, band).expect("band");
    assert_eq!(full.len(), b.len());
    for k in 0..full.len() {
        let (df, db) = (density(full[k], band), density(b[k], band));
        assert_eq!(df.to_bits(), db.to_bits(), "sub-region density mismatch at {k}");
    }
}

/// Cube-in-cube (inner shell wound inward = sealed air pocket) sampled through the band
/// sampler over the full lattice.
fn hollow_cube_band(inner_half: f32, res: u32) -> (Vec<f32>, f32) {
    let (ov, oi) = common::cube(0.9);
    let (iv, ii) = common::cube(inner_half);
    let mut v = ov.clone();
    let mut idx = oi.clone();
    let off = v.len() as u32;
    v.extend(iv.iter().copied());
    // Flip inner cube winding → its normals face the pocket (hollow interior).
    for t in ii.chunks(3) {
        idx.extend([off + t[0], off + t[2], off + t[1]]);
    }
    let cell = 2.0 / (res as f32 - 1.0);
    let band = 2.0 * cell;
    let b = region_mesh_to_sdf_band(&v, &idx, 1.0, res, [0, 0, 0, res - 1, res - 1, res - 1], band)
        .expect("band");
    (b, band)
}

#[test]
fn enclosed_pocket_fills_solid_with_no_inner_crossing() {
    // Sealed voids FILL — the industry voxel-remesh semantic (Blender / 3D-Coat) and this
    // sampler's documented contract. This deliberately DIVERGES from the full sampler,
    // whose pseudo-normal sign keeps the pocket as air (remeshing the inner shell).
    // The assertion is a FULL SCAN of the pocket: every cell strictly inside the inner
    // cube must be solid (≤ 0). A single kept-positive cell would put TWO iso crossings
    // inside the void — Surface Nets would extract the inner wall plus a phantom floating
    // core (the double-crossing bug the air-walk flood + unreached-air flip eliminate).
    let res = 48u32;
    let cell = 2.0 / (res as f32 - 1.0);
    let (b, _) = hollow_cube_band(0.45, res);
    let at = |x: u32, y: u32, z: u32| b[(x + y * res + z * res * res) as usize];
    let gidx = |w: f32| ((w + 1.0) / cell).round() as u32;
    let (p0, p1) = (gidx(-0.42), gidx(0.42)); // strictly inside the 0.45 inner cube
    for z in p0..=p1 {
        for y in p0..=p1 {
            for x in p0..=p1 {
                assert!(
                    at(x, y, z) <= 0.0,
                    "pocket cell ({x},{y},{z}) kept air value {} — phantom inner crossing",
                    at(x, y, z)
                );
            }
        }
    }
    // Between shells (r ≈ 0.675): solid → negative. Outside: air → positive.
    let mid = res / 2;
    let shell_mid = gidx(0.675);
    assert!(at(shell_mid, mid, mid) < 0.0, "shell interior must be negative");
    assert!(at(1, 1, 1) > 0.0, "outer air must be positive");
}

/// V1 INCREMENTAL GATE — the seeded-border region resample spliced into an existing field
/// must equal a full-lattice resample of the edited mesh, BIT-FOR-BIT.
///
/// Protocol (exactly what the Remesh modifier's incremental path does):
///   1. full band field of mesh A (subdivided-face cube),
///   2. edit: move one vertex (all its incident triangles change),
///   3. dirty region = union of old+new AABBs of every face touching the moved vertex,
///      dilated by band + 1 cell (the caller contract),
///   4. seeded resample of ONLY that region, `prev_air` answered from field A,
///   5. splice into a copy of A → must equal the full-lattice band field of mesh B.
#[test]
fn seeded_region_splice_matches_full_resample() {
    let res = 48u32;
    let bounds = 1.0f32;
    let cell = (bounds * 2.0) / (res as f32 - 1.0);
    let band = 2.0 * cell;

    // Mesh A: cube with the +X face split 2×2 (so an interior-of-face vertex exists whose
    // move keeps the mesh watertight). Build: cube faces as quads → split one.
    let (mut v, i) = common::cube(0.7);
    // The +X face of common::cube uses verts 1,2,6,5 (two tris: 1,2,6 / 1,6,5). Replace
    // with a fan around a new centre vertex so we get a movable non-corner vertex.
    let mut idx: Vec<u32> = i
        .chunks(3)
        .filter(|t| {
            // Drop the two +X-face triangles (all three verts have x == +0.7).
            !t.iter().all(|&k| (v[k as usize].x - 0.7).abs() < 1e-6)
        })
        .flatten()
        .copied()
        .collect();
    let c = v.len() as u32;
    v.push(glam::Vec3::new(0.7, 0.0, 0.0)); // +X face centre
    // Outward fan (+X): ring 1 → 2 → 6 → 5 (CCW seen from +X).
    for w in [[1u32, 2], [2, 6], [6, 5], [5, 1]] {
        idx.extend([c, w[0], w[1]]);
    }
    let full_a = region_mesh_to_sdf_band(&v, &idx, bounds, res, [0, 0, 0, res - 1, res - 1, res - 1], band)
        .expect("field A");

    // Mesh B: pull the face-centre vertex outward (+0.12 along X) — a bulge.
    let mut vb = v.clone();
    vb[c as usize].x += 0.12;

    // Dirty region: union of old+new AABBs of the 4 fan faces, dilated band + 1 cell.
    let mut mn = glam::Vec3::splat(f32::INFINITY);
    let mut mx = glam::Vec3::splat(f32::NEG_INFINITY);
    for t in idx.chunks(3) {
        if t.contains(&c) {
            for &k in t {
                mn = mn.min(v[k as usize]).min(vb[k as usize]);
                mx = mx.max(v[k as usize]).max(vb[k as usize]);
            }
        }
    }
    let dil = band + cell;
    let lo = |w: f32| (((w - dil + bounds) / cell).floor() as i64).clamp(0, res as i64 - 1) as u32;
    let hi = |w: f32| (((w + dil + bounds) / cell).ceil() as i64).clamp(0, res as i64 - 1) as u32;
    let region = [lo(mn.x), lo(mn.y), lo(mn.z), hi(mx.x), hi(mx.y), hi(mx.z)];

    let at_a = |x: u32, y: u32, z: u32| full_a[(x + y * res + z * res * res) as usize];
    let patch = region_mesh_to_sdf_band_seeded(
        &vb, &idx, bounds, res, region, band,
        &|gx, gy, gz| at_a(gx, gy, gz) > 0.0,
    )
    .expect("seeded patch");

    // Splice patch into a copy of A.
    let mut spliced = full_a.clone();
    let (nx, ny) = (
        (region[3] - region[0] + 1) as usize,
        (region[4] - region[1] + 1) as usize,
    );
    for z in region[2]..=region[5] {
        for y in region[1]..=region[4] {
            for x in region[0]..=region[3] {
                let pi = (x - region[0]) as usize
                    + (y - region[1]) as usize * nx
                    + (z - region[2]) as usize * nx * ny;
                spliced[(x + y * res + z * res * res) as usize] = patch[pi];
            }
        }
    }

    // Reference: full-lattice band field of B. Must match the splice EXACTLY.
    let full_b = region_mesh_to_sdf_band(&vb, &idx, bounds, res, [0, 0, 0, res - 1, res - 1, res - 1], band)
        .expect("field B");
    let mut mismatches = 0usize;
    for k in 0..full_b.len() {
        if spliced[k].to_bits() != full_b[k].to_bits() {
            mismatches += 1;
            if mismatches <= 5 {
                let x = k as u32 % res;
                let y = (k as u32 / res) % res;
                let z = k as u32 / (res * res);
                eprintln!(
                    "mismatch at ({x},{y},{z}): spliced={} full_b={}",
                    spliced[k], full_b[k]
                );
            }
        }
    }
    assert_eq!(mismatches, 0, "seeded splice diverged from full resample in {mismatches} cells");
}

#[test]
fn fully_in_band_pocket_also_fills() {
    // Inner cube at 0.08 half-extent < band (2·cell ≈ 0.085 at res 48): the entire sealed
    // void is within band of its wall, so there are NO far cells to flood-fill — the fill
    // must come entirely from the unreached-in-band-air flip. Guards that path in isolation.
    let res = 48u32;
    let cell = 2.0 / (res as f32 - 1.0);
    let (b, _) = hollow_cube_band(0.08, res);
    let at = |x: u32, y: u32, z: u32| b[(x + y * res + z * res * res) as usize];
    let gidx = |w: f32| ((w + 1.0) / cell).round() as u32;
    // Scan the whole tiny void: every cell strictly inside the inner cube must be solid.
    let (p0, p1) = (gidx(-0.05), gidx(0.05));
    for z in p0..=p1 {
        for y in p0..=p1 {
            for x in p0..=p1 {
                assert!(
                    at(x, y, z) <= 0.0,
                    "in-band void cell ({x},{y},{z}) kept air value {}",
                    at(x, y, z)
                );
            }
        }
    }
    assert!(at(1, 1, 1) > 0.0, "outer air must be positive");
}
