//! Corner caps + M_ADJ grid fill (BEVEL.md §4 STEP 5 / §3.7).
//!
//! Pure relocation: M_ADJ Catmull-Clark corner grid, rim helpers (append_chained, Newell, simple-loop).

use glam::Vec3;
use std::collections::{HashMap, HashSet};
use super::{WeldMap, circular_arc};

// =====================================================================================================
// M_ADJ CORNER GRID (§3.7) — Blender's converging quad-grid fill for a fully-beveled corner, verified
// vertex-exact against the Blender 4.5 oracle (cube ±1, offset 0.3: seg3 worst 5e-6, seg4 9e-6, seg5
// 4.5e-4 in the Python pilot `madj_sim.py`). Clean-room from the published algorithm (Levin 1999
// "Filling an N-sided hole using combined subdivision schemes" + Catmull-Clark/Sabin), studied via
// bmesh_bevel.cc. Pipeline: coarse control mesh (BoundVerts + arc mids + a fullness-placed center) →
// Levin cubic subdivision to ≥seg → arc-length-frac bilinear resample to exactly seg → (circular
// profile, 3-sector corner) snap to the fitted fillet sphere. The KEY non-obvious constant: for a
// 3-sector circular corner with seg>2 the coarse center starts at 1.4× the pole height (Blender's
// offline-optimized "closest fit to sphere" seed) — without it the inner ring lands ~1.7° too low.
// =====================================================================================================

/// Grid of corner vertices keyed by CANONICAL (sector, j, k).
pub(crate) type MadjGrid = HashMap<(usize, usize, usize), Vec3>;

/// Canonical (sector, j, k) for Blender's M_ADJ vertex grid. Sector `i` owns a (ns2+1)×(ns+1) patch —
/// j rows inward from its arc, k along the arc — but rows overlap neighbouring sectors:
///   • k > ns/2                      → the point belongs to the NEXT sector:   (i+1, ns−k, j)
///   • even ns, j == k == ns/2       → the single shared centre vertex:        (0, ns/2, ns/2)
///   • j past the sector's own rows  → the PREVIOUS sector's k-row:            (i−1, k, ns−j)
pub(crate) fn madj_canon(n: usize, ns: usize, i: usize, j: usize, k: usize) -> (usize, usize, usize) {
    let ns2 = ns / 2;
    let odd = ns % 2;
    let (mut i, mut j, mut k) = (i, j, k);
    loop {
        if k > ns2 {
            let (i2, j2, k2) = ((i + 1) % n, ns - k, j);
            i = i2; j = j2; k = k2;
            continue;
        }
        if odd == 0 && j == ns2 && k == ns2 {
            return (0, ns2, ns2);
        }
        if j <= ns2 - 1 + odd {
            return (i, j, k);
        }
        let (i2, j2, k2) = ((i + n - 1) % n, k, ns - j);
        i = i2; j = j2; k = k2;
    }
}

/// Sabin's gamma for the Catmull-Clark centre vertex of valence `n` (bounded-curvature modification).
/// Table for common n, closed-form root of x³+(4k²−3)x−2k = 0 beyond (Blender-identical constants).
pub(crate) fn sabin_gamma(n: usize) -> f32 {
    match n {
        0..=2 => 0.0,
        3 => 0.065247584,
        4 => 0.25,
        5 => 0.401983447,
        6 => 0.523423277,
        _ => {
            let k = (std::f64::consts::PI / n as f64).cos();
            let (k2, k4) = (k * k, k * k * k * k);
            let k6 = k4 * k2;
            let y = (3f64.sqrt() * (64.0 * k6 - 144.0 * k4 + 135.0 * k2 - 27.0).sqrt() + 9.0 * k)
                .powf(1.0 / 3.0);
            let x = 0.480749856769136 * y - (0.231120424783545 * (12.0 * k2 - 9.0)) / y;
            ((k * x + 2.0 * k2 - 1.0) / (x * x * (k * x + 1.0))) as f32
        }
    }
}

/// Fraction of the way from the BoundVert centroid to the original vertex where the coarse M_ADJ centre
/// starts (Blender's profile "fullness"): offline-optimized circle table for seg ≤ 11, linear regression
/// (by seg parity) for everything else.
pub(crate) fn madj_fullness(ns: usize, shape: f32) -> f32 {
    const CIRCLE: [f32; 11] = [0.0, 0.559, 0.642, 0.551, 0.646, 0.624, 0.646, 0.619, 0.647, 0.639, 0.647];
    if (shape - 0.5).abs() < 5e-3 && (1..=11).contains(&ns) {
        return CIRCLE[ns - 1];
    }
    if ns % 2 == 0 { 2.4506 * shape - 0.6266 } else { 2.3635 * shape - 0.606 }
}

/// Point at parameter `t` on the circular arc a→b whose circle also passes through `m` (circumcircle of
/// the three). The corner grid needs the rim's fillet arc at arbitrary subdivision parameters, while the
/// rim only samples it at k/n_seg — the circumcircle through (BV, an interior arc point, BV) is the same
/// circle `circular_arc` produced (symmetric corner ⇒ equal end radii). Fillet arcs are < π so the
/// signed short-way rotation is always the right branch. Degenerate/collinear → chord lerp.
/// searched for existing: `circular_arc` (tangent-APEX form — the apex is not available at cap time);
/// this is the 3-point form of the same circle, not a second evaluation path.
pub(crate) fn arc_lerp_3pt(a: Vec3, m: Vec3, b: Vec3, t: f32) -> Vec3 {
    let u = b - a;
    let v = m - a;
    let w = u.cross(v);
    let w2 = w.length_squared();
    if w2 < 1e-12 { return a.lerp(b, t); }
    let c = a + (v.length_squared() * w.cross(u) + u.length_squared() * v.cross(w)) / (2.0 * w2);
    let ra = a - c;
    let rb = b - c;
    let wn = w / w2.sqrt();
    let ang = ra.cross(rb).dot(wn).atan2(ra.dot(rb));
    let q = glam::Quat::from_axis_angle(wn, ang * t);
    c + q * ra
}

/// One Levin cubic-subdivision step of the corner grid: level `lvl` (even) → `2·lvl`. Boundary rows ride
/// the exact arc curves; interior uses the verified masks — face = cell centroid, edge = (ends + the two
/// adjacent face points)/4, vertex = E_avg − ¼·F_avg + ¼·old (Levin, NOT textbook CC), centre =
/// E_avg − γ·F_avg + γ·old with γ = `sabin_gamma(n)`.
pub(crate) fn madj_cubic_subdiv(n: usize, lvl: usize, cur: &MadjGrid, arc_pt: &dyn Fn(usize, f32) -> Vec3) -> MadjGrid {
    let lo = lvl * 2;
    let h2 = lvl / 2;
    let mut out: MadjGrid = HashMap::new();
    let gi = |g: &MadjGrid, l: usize, i: usize, j: usize, k: usize| -> Vec3 {
        *g.get(&madj_canon(n, l, i, j, k)).expect("madj grid slot")
    };
    // Boundary: exact curve samples (our profile curve is known analytically — Blender's polyline
    // smoothing + power-of-2 chord-lerp is an approximation of this same curve; ours is the intended
    // limit and stays within 2e-4 of Blender's on the cube oracle).
    for i in 0..n {
        for m in 0..lo {
            out.insert(madj_canon(n, lo, i, 0, m), arc_pt(i, m as f32 / lo as f32));
        }
    }
    // Face points: input-cell centroids.
    for i in 0..n {
        for j in 0..h2 {
            for k in 0..h2 {
                let f = (gi(cur, lvl, i, j, k) + gi(cur, lvl, i, j, k + 1)
                    + gi(cur, lvl, i, j + 1, k) + gi(cur, lvl, i, j + 1, k + 1)) / 4.0;
                out.insert(madj_canon(n, lo, i, 2 * j + 1, 2 * k + 1), f);
            }
        }
    }
    // Edge points (vertical then horizontal); the j==h2 horizontal row aliases the previous sector's
    // vertical edges through `madj_canon`, so it is skipped — one writer per canonical slot.
    for i in 0..n {
        for j in 0..h2 {
            for k in 1..=h2 {
                let e = (gi(cur, lvl, i, j, k) + gi(cur, lvl, i, j + 1, k)
                    + gi(&out, lo, i, 2 * j + 1, 2 * k - 1) + gi(&out, lo, i, 2 * j + 1, 2 * k + 1)) / 4.0;
                out.insert(madj_canon(n, lo, i, 2 * j + 1, 2 * k), e);
            }
        }
        for j in 1..h2 {
            for k in 0..h2 {
                let e = (gi(cur, lvl, i, j, k) + gi(cur, lvl, i, j, k + 1)
                    + gi(&out, lo, i, 2 * j - 1, 2 * k + 1) + gi(&out, lo, i, 2 * j + 1, 2 * k + 1)) / 4.0;
                out.insert(madj_canon(n, lo, i, 2 * j, 2 * k + 1), e);
            }
        }
    }
    // Interior vertex points (Levin α=1, β=−γ, γ=¼).
    for i in 0..n {
        for j in 1..h2 {
            for k in 1..=h2 {
                let e_avg = (gi(&out, lo, i, 2 * j, 2 * k - 1) + gi(&out, lo, i, 2 * j, 2 * k + 1)
                    + gi(&out, lo, i, 2 * j - 1, 2 * k) + gi(&out, lo, i, 2 * j + 1, 2 * k)) / 4.0;
                let f_avg = (gi(&out, lo, i, 2 * j - 1, 2 * k - 1) + gi(&out, lo, i, 2 * j + 1, 2 * k - 1)
                    + gi(&out, lo, i, 2 * j - 1, 2 * k + 1) + gi(&out, lo, i, 2 * j + 1, 2 * k + 1)) / 4.0;
                let p = e_avg - 0.25 * f_avg + 0.25 * gi(cur, lvl, i, j, k);
                out.insert(madj_canon(n, lo, i, 2 * j, 2 * k), p);
            }
        }
    }
    // Centre vertex (valence n — Sabin gamma).
    let gam = sabin_gamma(n);
    let mut e_sum = Vec3::ZERO;
    let mut f_sum = Vec3::ZERO;
    for i in 0..n {
        e_sum += gi(&out, lo, i, lvl, lvl - 1);
        f_sum += gi(&out, lo, i, lvl - 1, lvl - 1) + gi(&out, lo, i, lvl - 1, lvl + 1);
    }
    let center = e_sum / n as f32 - gam * f_sum / (2.0 * n as f32) + gam * gi(cur, lvl, 0, h2, h2);
    out.insert((0, lvl, lvl), center);
    out
}

/// Fill a CLEAN fully-beveled corner with Blender's M_ADJ converging quad grid: the pinned rim (n
/// profile arcs × `ns` segments — SHARED with the strips, so edge cross-sections stay exact) is bridged
/// by n sector grids of quads; even `ns` → single shared centre vertex, odd `ns` → centre n-gon.
/// Interior = Levin subdivision (+ fillet-sphere snap for the circular 3-sector corner — Blender's
/// superellipsoid snap). Returns false (emitting nothing) when the rim is not the clean n×ns arc chain —
/// the caller falls back to the fillet-sphere fan.
/// searched for existing: `subdivide_catmull_clark` (half_edge.rs — whole-mesh CC, frozen file, no
/// pinned-rim boundary curves / sector grid / sphere snap) — not reusable for a bounded patch fill.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_corner_grid_madj(
    rim: &[usize],
    ns: usize,
    orig_v: Vec3,
    pole: Vec3,
    sphere: Option<(Vec3, f32)>,
    shape: f32,
    bv_set: &HashSet<usize>,
    weld: &mut WeldMap,
    faces_out: &mut Vec<Vec<usize>>,
) -> bool {
    let nb = rim.len();
    if ns < 2 || nb < 6 { return false; }
    // The rim must be the clean chain of n arcs: BoundVerts exactly every `ns` slots.
    let bvp: Vec<usize> = (0..nb).filter(|&p| bv_set.contains(&rim[p])).collect();
    let n = bvp.len();
    if n < 3 || nb != n * ns || !bvp.windows(2).all(|w2| w2[1] - w2[0] == ns) { return false; }
    let bv0 = bvp[0];
    let arcs: Vec<Vec<Vec3>> = (0..n)
        .map(|i| (0..=ns).map(|m| weld.positions[rim[(bv0 + i * ns + m) % nb]]).collect())
        .collect();
    let arc_pt = |i: usize, t: f32| arc_lerp_3pt(arcs[i][0], arcs[i][ns / 2], arcs[i][ns], t);

    // Blender path split: a 3-sector corner with the circular profile is the `tri_corner` special case
    // (sphere-seeded centre, ≥0 subdivisions, superellipsoid snap); everything else is `adj_vmesh`
    // (fullness-seeded centre, ≥1 subdivision, no snap).
    // Tri-corner (sphere-snap) path ONLY when the rim actually LIES on the fitted fillet sphere (a
    // true cube-style corner). A MIXED 3-sector corner (meet + on-edge BoundVerts at different radii —
    // `mixed_k3d4_ref.json`) is NOT spherical: Blender routes it through adj_vmesh (fullness, no snap);
    // snapping would distort it. Mirrors Blender's `tri_corner_test` angle gate, geometrically.
    let rim_on_sphere = sphere.map_or(false, |(c, r)| {
        let scale = r.max(1e-6);
        (0..n).all(|i| (0..=ns).all(|m| {
            ((weld.positions[rim[(bv0 + i * ns + m) % nb]] - c).length() - r).abs() < 2e-2 * scale
        }))
    });
    let circle3 = n == 3 && (shape - 0.5).abs() < 5e-3 && sphere.is_some() && rim_on_sphere;
    let center0 = if circle3 {
        let (c, _) = sphere.unwrap();
        // Offline-optimized seed: the coarse centre starts 1.4× the pole height for seg>2 (the missing
        // constant that cost 1.7° of inner-ring placement), exactly the pole for seg 2.
        if ns > 2 { c + (pole - c) * 1.4 } else { pole }
    } else {
        let bvc: Vec3 = (0..n).map(|i| arcs[i][0]).sum::<Vec3>() / n as f32;
        bvc + (orig_v - bvc) * madj_fullness(ns, shape)
    };

    // Coarse control mesh (level 2): BoundVerts + true arc midpoints + the seeded centre.
    let mut cur: MadjGrid = HashMap::new();
    let mut lvl = 2usize;
    for i in 0..n {
        cur.insert(madj_canon(n, 2, i, 0, 0), arcs[i][0]);
        cur.insert(madj_canon(n, 2, i, 0, 1), arc_pt(i, 0.5));
    }
    cur.insert((0, 1, 1), center0);

    if circle3 {
        while lvl < ns {
            cur = madj_cubic_subdiv(n, lvl, &cur, &arc_pt);
            lvl *= 2;
        }
    } else {
        loop {
            cur = madj_cubic_subdiv(n, lvl, &cur, &arc_pt);
            lvl *= 2;
            if lvl >= ns { break; }
        }
    }

    // Target interior canonical coords.
    let ns2t = ns / 2;
    let oddt = ns % 2;
    let mut tcoords: Vec<(usize, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in 1..=(ns2t - 1 + oddt) {
            for k in 1..=ns2t { tcoords.push((i, j, k)); }
        }
    }
    if oddt == 0 { tcoords.push((0, ns2t, ns2t)); }

    let gi = |g: &MadjGrid, l: usize, i: usize, j: usize, k: usize| -> Vec3 {
        *g.get(&madj_canon(n, l, i, j, k)).expect("madj grid slot")
    };
    // Resample to the exact target level (arc-length-frac bilinear — Blender's `interp_vmesh`). The
    // even-ns centre copies straight across (`vmesh_center`), not bilinear.
    let interior: Vec<((usize, usize, usize), Vec3)> = if lvl == ns {
        tcoords.iter().map(|&c| (c, gi(&cur, ns, c.0, c.1, c.2))).collect()
    } else {
        let fracs = |pts: &[Vec3]| -> Vec<f32> {
            let mut f = vec![0.0f32];
            for q in pts.windows(2) { f.push(f.last().unwrap() + (q[1] - q[0]).length()); }
            let tot = *f.last().unwrap();
            if tot > 0.0 { for x in f.iter_mut() { *x /= tot; } } else { *f.last_mut().unwrap() = 1.0; }
            f
        };
        let interp_range = |fr: &[f32], f: f32| -> (usize, f32) {
            let m = fr.len() - 1;
            for x in 0..m {
                if f <= fr[x + 1] {
                    let rest = f - fr[x];
                    let r = if rest == 0.0 { 0.0 } else { rest / (fr[x + 1] - fr[x]) };
                    if x == m - 1 && r == 1.0 { return (m, 0.0); }
                    return (x, r);
                }
            }
            (m, 0.0)
        };
        let fine_frac: Vec<Vec<f32>> = (0..n)
            .map(|i| fracs(&(0..=lvl).map(|m| arc_pt(i, m as f32 / lvl as f32)).collect::<Vec<_>>()))
            .collect();
        let targ_frac: Vec<Vec<f32>> = (0..n)
            .map(|i| fracs(&(0..=ns).map(|k| arc_pt(i, k as f32 / ns as f32)).collect::<Vec<_>>()))
            .collect();
        tcoords.iter().map(|&(i, j, k)| {
            if oddt == 0 && (i, j, k) == (0, ns2t, ns2t) {
                return ((i, j, k), gi(&cur, lvl, 0, lvl / 2, lvl / 2));
            }
            let (k_in, restk) = interp_range(&fine_frac[i], targ_frac[i][k]);
            let iprev = (i + n - 1) % n;
            let (kp, restkp) = interp_range(&fine_frac[iprev], targ_frac[iprev][ns - j]);
            let (j_in, restj) = if restkp <= 1e-6 { (lvl - kp, 0.0) } else { (lvl - kp - 1, 1.0 - restkp) };
            let p = if restj < 1e-6 && restk < 1e-6 {
                gi(&cur, lvl, i, j_in, k_in)
            } else {
                let j1 = if restj < 1e-6 || j_in == lvl { 0 } else { 1 };
                let k1 = if restk < 1e-6 || k_in == lvl { 0 } else { 1 };
                let q0 = gi(&cur, lvl, i, j_in, k_in);
                let q1 = gi(&cur, lvl, i, j_in, k_in + k1);
                let q2 = gi(&cur, lvl, i, j_in + j1, k_in + k1);
                let q3 = gi(&cur, lvl, i, j_in + j1, k_in);
                q0 * (1.0 - restk) * (1.0 - restj) + q1 * restk * (1.0 - restj)
                    + q2 * restk * restj + q3 * (1.0 - restk) * restj
            };
            ((i, j, k), p)
        }).collect()
    };

    // Snap to the fitted fillet sphere (circular 3-sector corner only — Blender's superellipsoid snap),
    // then weld the interior.
    let mut idx_of: HashMap<(usize, usize, usize), usize> = HashMap::new();
    for (coord, mut p) in interior {
        if circle3 {
            let (c, r) = sphere.unwrap();
            let d = p - c;
            let dl = d.length();
            if dl > 1e-6 { p = c + d * (r / dl); }
        }
        idx_of.insert(coord, weld.push(p));
    }

    // Emit: per sector ns2·(ns2+odd) quads + (odd) the centre n-gon. Winding [rim, rim+1, in, in-prev]
    // matches the strips (the proven seg-2 quad orientation, generalized).
    let vert_at = |i: usize, j: usize, k: usize| -> usize {
        let (ci, cj, ck) = madj_canon(n, ns, i, j, k);
        if cj == 0 {
            rim[(bv0 + ci * ns + ck) % nb]
        } else if ck == 0 {
            rim[(bv0 + ((ci + n - 1) % n) * ns + (ns - cj)) % nb]
        } else {
            idx_of[&(ci, cj, ck)]
        }
    };
    for i in 0..n {
        for j in 0..ns2t {
            for k in 0..(ns2t + oddt) {
                let mut q = vec![vert_at(i, j, k), vert_at(i, j, k + 1),
                    vert_at(i, j + 1, k + 1), vert_at(i, j + 1, k)];
                dedup_ring(&mut q);
                if q.len() >= 3 { faces_out.push(q); }
            }
        }
    }
    if oddt == 1 {
        faces_out.push((0..n).map(|i| vert_at(i, ns2t, ns2t)).collect());
    }
    true
}

/// Remove consecutive duplicate indices in a face ring (wrap-around aware). A welded corner can map two
/// adjacent ring slots to one index; leaving the duplicate would make `from_polygons` build a zero-length
/// half-edge.
pub(crate) fn dedup_ring(ring: &mut Vec<usize>) {
    if ring.len() < 2 { return; }
    let mut out: Vec<usize> = Vec::with_capacity(ring.len());
    for &i in ring.iter() {
        if out.last() != Some(&i) { out.push(i); }
    }
    while out.len() >= 2 && out.first() == out.last() { out.pop(); }
    *ring = out;
}

/// Append arc `a` to the corner rim, chaining by SHARED WELDED endpoint so the arc's storage direction
/// (L→R) is irrelevant: if `a` starts (or ends) at the rim's current tail, append the remainder in the
/// matching direction; otherwise start a fresh run. This makes the fan-order cap rim robust to per-edge
/// arc orientation (the risk the design panel flagged as the main implementation hazard).
pub(crate) fn append_chained(a: &[usize], loop_pts: &mut Vec<usize>) {
    if a.is_empty() { return; }
    if loop_pts.is_empty() { loop_pts.extend_from_slice(a); return; }
    let last = *loop_pts.last().unwrap();
    let first = *loop_pts.first().unwrap();
    if a[0] == last {
        for &ix in &a[1..] { loop_pts.push(ix); }
    } else if *a.last().unwrap() == last {
        for &ix in a[..a.len() - 1].iter().rev() { loop_pts.push(ix); }
    } else if a[0] == first {
        // `a` shares the loop's HEAD, not its tail — the first arc landed in the opposite orientation.
        // Reverse the loop so the shared point is the tail, then append. (Without this, full-ring corners
        // whose fan orientation opposes the first arc's storage direction append as a duplicate run.)
        loop_pts.reverse();
        for &ix in &a[1..] { loop_pts.push(ix); }
    } else if *a.last().unwrap() == first {
        loop_pts.reverse();
        for &ix in a[..a.len() - 1].iter().rev() { loop_pts.push(ix); }
    } else {
        loop_pts.extend_from_slice(a);
    }
}

/// Newell area-weighted normal of a polygon given by welded indices into `positions`. Sign-correct for any
/// non-planar / non-convex loop — used to orient the corner cap deterministically (CONV-C).
pub(crate) fn newell_idx(idxs: &[usize], positions: &[Vec3]) -> Vec3 {
    let mut n = Vec3::ZERO;
    let c = idxs.len();
    for i in 0..c {
        let a = positions[idxs[i]];
        let b = positions[idxs[(i + 1) % c]];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// True if the loop is SIMPLE (no two non-adjacent edges cross) when projected to its Newell plane. A
/// simple rim caps cleanly as one n-gon; a non-simple (self-crossing when flattened) rim needs an apex fan.
pub(crate) fn is_simple_loop(idxs: &[usize], positions: &[Vec3]) -> bool {
    let n = idxs.len();
    if n < 4 { return true; }
    let nrm = newell_idx(idxs, positions).normalize_or_zero();
    if nrm.length_squared() < 1e-12 { return true; }
    let up = if nrm.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u = nrm.cross(up).normalize_or_zero();
    let vv = nrm.cross(u);
    let p2: Vec<(f32, f32)> = idxs.iter().map(|&i| (positions[i].dot(u), positions[i].dot(vv))).collect();
    fn seg_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
        let o = |p: (f32, f32), q: (f32, f32), r: (f32, f32)|
            (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        let (x1, x2, x3, x4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
        ((x1 > 0.0) != (x2 > 0.0)) && ((x3 > 0.0) != (x4 > 0.0))
    }
    for i in 0..n {
        let (a, b) = (p2[i], p2[(i + 1) % n]);
        for j in (i + 1)..n {
            if (i + 1) % n == j || (j + 1) % n == i { continue; }
            if seg_cross(a, b, p2[j], p2[(j + 1) % n]) { return false; }
        }
    }
    true
}
