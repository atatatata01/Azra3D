//! Corner / miter construction (BEVEL.md §4 STEP 2b / STEP 5b, §3.7 M_NONE weld).
//!
//! Pure relocation: `build_boundary_flat`, `miter_weld_arc`, `partial_back_edge_slide`.

use glam::Vec3;
use std::collections::{HashMap, HashSet};
use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, NO_TWIN};
use super::{
    drop_onto_faces, inward_offset_line, isect_line_plane, isect_lines_3d, loop_slide_meet2,
    project_meet_onto_edge,
};

/// ROUNDED 2-EDGE MITER arc — the **M_NONE weld** profile of Blender's `bmesh_bevel.cc` (§3.7). At a
/// degree>3 vertex where exactly two beveled edges meet, the flat miter is a single shared edge between two
/// BoundVerts (`a`,`b`); rounding it (segments>1) bridges those two BoundVerts with a circular profile arc
/// welded from BOTH edges' profiles (co-planarized onto ONE plane, so the two arcs coincide — zero corner
/// grid, `ns−1` shared interior verts). This is a **clean-room reimplementation** of Blender's exact
/// construction (`set_profile_params` + `move_weld_profile_planes` + `calculate_profile_segments`), verified
/// vertex-for-vertex against the oracle:
///  1. `middle` = closest point on the beveled edge line (`orig`→`edge_far`) to the segment `a`–`b`
///     (`project_to_edge`).
///  2. Map each superellipse sample `(x,y)` (profile 0.5 ⇒ even-angle circle) through the unit-square map
///     `co = x·(mid−a) + y·(mid−b) + (a+b−mid)` (`make_unit_square_map`, `z=0`).
///  3. Project `co` ALONG the edge direction `proj_dir` onto the WELD plane (`plane_co=a`,
///     `plane_no = (orig−a)×(orig−b)` from `move_weld_profile_planes`) — this is the step that turns the
///     unit-square (parabola-like) point into the true circular fillet point.
/// Returns `ns+1` points from `a` (i=0) to `b` (i=ns). `shape` blends flat(0)↔circular(0.5)↔convex(1) like
/// the single-edge STEP-2.5 profile so non-default shapes degrade the same way.
pub(crate) fn miter_weld_arc(a: Vec3, b: Vec3, orig: Vec3, edge_far: Vec3, ns: usize, shape: f32) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(ns + 1);
    // FLAT-miter degenerate (Blender parity): when V lies ON the a–b line the miter site is planar —
    // a knife-chain bend across a FLAT face (dihedral π): the two offset intersections straddle V
    // collinearly. Blender's profile has no curvature to round there and emits the straight segment
    // (oracle knife_chain_seg3: interior verts are exact thirds of a–b). The superellipse below would
    // bow it (~0.01 off oracle).
    {
        let ab = b - a;
        let ab_len2 = ab.length_squared();
        if ab_len2 > 1e-12 {
            let t = (orig - a).dot(ab) / ab_len2;
            let on_line = orig - (a + ab * t);
            if on_line.length_squared() < 1e-8 * ab_len2 {
                for i in 0..=ns { out.push(a.lerp(b, i as f32 / ns as f32)); }
                return out;
            }
        }
    }
    let proj_dir = (edge_far - orig).normalize_or_zero();
    let edge_dir = proj_dir;
    // Profile CONTROL POINT = V itself. Inverting the unit-square map on BOTH oracles
    // (owner_ring gentle bend AND the pyramid rounded 2-edge miter) recovers M = orig to
    // 3-4 decimals. The old `closest_point_line_line(edge, chord)` only COINCIDED with V on
    // the pyramid (chord far from the edge line); at a gentle pass-through bend it slid the
    // anchor onto the chord → zero bow → the owner's "segments too straight" (2026-07-23).
    let middle = orig;
    let mut plane_no = (orig - a).cross(orig - b);
    if plane_no.length_squared() < 1e-14 { plane_no = Vec3::ZERO; } else { plane_no = plane_no.normalize(); }
    // In-plane bend: the projection direction is (nearly) PARALLEL to the weld plane —
    // isect_line_plane is degenerate there; the unmapped unit-square point is already exact
    // (oracle-verified at the owner bend). Disable the projection instead of amplifying noise.
    if proj_dir.dot(plane_no).abs() < 1e-3 { plane_no = Vec3::ZERO; }
    let plane_co = a;
    let d_am = middle - a;
    let d_bm = middle - b;
    let cst = a + b - middle;
    let half = std::f32::consts::FRAC_PI_2;
    for i in 0..=ns {
        if i == 0 { out.push(a); continue; }
        if i == ns { out.push(b); continue; }
        let ang = (i as f32 / ns as f32) * half;
        // Superellipse profile 0.5 (r=2, circle): even-angle x=sin, y=cos so i=0→(0,1)→a, i=ns→(1,0)→b.
        let (x, y) = (ang.sin(), ang.cos());
        let co2d = d_am * x + d_bm * y + cst;
        // Project along the edge onto the weld plane → the circular fillet point (Blender's key step).
        let circ = if plane_no.length_squared() > 0.5 && proj_dir.length_squared() > 0.5 {
            isect_line_plane(co2d, proj_dir, plane_co, plane_no)
        } else {
            co2d
        };
        // Shape blend, matching STEP 2.5: 0=flat chord, 0.5=circular fillet, 1=convex.
        let t = i as f32 / ns as f32;
        let flat = a.lerp(b, t);
        let p = if shape <= 0.5 {
            flat.lerp(circ, shape * 2.0)
        } else {
            circ + (circ - flat) * ((shape - 0.5) * 2.0)
        };
        out.push(p);
    }
    out
}

/// INNER MITER ARC (Blender "Miter Inner = Arc") — the closed form, derived from the oracle in
/// BEVEL.md §3.8. Replaces the SHARP corner's single meet point `m` with a circular arc of radius
/// `spread`, whose ends sit back along each beveled edge:
///
/// ```text
/// S0 = m + spread·d_a          S1 = m + spread·d_b
/// C  = m + spread·(d_a + d_b)  →  |S0 − C| = |S1 − C| = spread
/// ```
///
/// so the arc is `circle(C, spread)` from `S0` to `S1`. Because `S0 − C = −spread·d_b` and
/// `S1 − C = −spread·d_a`, the radius holds for NON-perpendicular edges too (`spread·|d_b|`), which
/// is why this is a slerp and not a special-cased quarter turn.
///
/// `d_a` / `d_b` are the two beveled edges' UNIT directions at the corner vertex, pointing AWAY from
/// it. Returns `ns+1` points from `S0` (i=0) to `S1` (i=ns).
///
/// ⚠ **Blender is exact only at power-of-two segment counts** (§3.8 table): `ns ∈ {2,4}` land on this
/// circle to 5 decimals, while `ns ∈ {3,5,6}` are pulled IN by up to 1.7% by Blender's own
/// subdivision resample — at `ns=6` its 45° midpoint is still exact while the neighbours are not.
/// That is an artifact of Blender's sampling, NOT a different intended curve. Gate vertex-exact at
/// `ns ∈ {1,2,4}` and allow a tolerance elsewhere; do not "fix" this toward Blender's artifact.
pub(crate) fn inner_miter_arc(
    m: Vec3, d_a: Vec3, d_b: Vec3, spread: f32, ns: usize,
) -> (Vec3, Vec3, Vec<Vec3>) {
    let ns = ns.max(1);
    let spread = spread.max(0.0);
    let (da, db) = (d_a.normalize_or_zero(), d_b.normalize_or_zero());
    let s0 = m + da * spread;
    let s1 = m + db * spread;
    // spread == 0 (or a degenerate direction) collapses both ends onto `m` — i.e. exactly Sharp.
    if spread <= 1e-9 || da.length_squared() < 0.5 || db.length_squared() < 0.5 {
        return (m, m, vec![m; ns + 1]);
    }
    let c = m + (da + db) * spread;
    let (u, v) = (s0 - c, s1 - c); // == (−spread·db, −spread·da)
    let (ul, vl) = (u.length(), v.length());
    let mut out = Vec::with_capacity(ns + 1);
    let cos = if ul > 1e-9 && vl > 1e-9 { (u.dot(v) / (ul * vl)).clamp(-1.0, 1.0) } else { 1.0 };
    let theta = cos.acos();
    // Collinear edges (θ→0 or θ→π) leave the arc plane undefined; the straight chord is the only
    // defensible answer there and matches the Sharp degenerate that `miter_weld_arc` already takes.
    if !theta.is_finite() || theta < 1e-5 || (std::f32::consts::PI - theta) < 1e-5 {
        for i in 0..=ns {
            out.push(s0.lerp(s1, i as f32 / ns as f32));
        }
        return (s0, s1, out);
    }
    let sin_t = theta.sin();
    for i in 0..=ns {
        if i == 0 {
            out.push(s0);
            continue;
        }
        if i == ns {
            out.push(s1);
            continue;
        }
        // Slerp on the radius-`spread` circle: exact for any opening angle, unlike a lerp+renormalise.
        let t = i as f32 / ns as f32;
        let w0 = ((1.0 - t) * theta).sin() / sin_t;
        let w1 = (t * theta).sin() / sin_t;
        out.push(c + u * w0 + v * w1);
    }
    (s0, s1, out)
}

/// ARC INNER MITER corner patch (BEVEL.md §3.8 + §3.8a). Takes the SHARP corner chain oriented
/// `O` (the shared-face rail meet, `i=0`) → `M` (the slide onto the unbeveled edge, `i=ns`) and
/// spreads it into a 3-sided patch, returning `ns+1` LEVELS where level `i` holds `i+1` points
/// running from the `d_a` side to the `d_b` side:
///
/// * level `0` = `[O]` — the outer end is NOT spread, so both edges' strips still share it (§3.8).
/// * level `ns` = the full top arc `S0 … S1`, the closed form of [`inner_miter_arc`].
/// * `levels[i][0]` / `levels[i][i]` are the two SIDE chains the two beveled edges' strips consume
///   in place of the shared sharp chain.
///
/// **The ramp is `t²`, and it is ours, not Blender's.** §3.8a measured Blender's side chain at
/// `ns=3` sitting 0.0066 off the sharp chain at level 1 where a LINEAR `spread·i/ns` ramp puts it at
/// 0.0333 — 5× too far — because Blender's ramp falls out of the `M_ADJ` VMesh solve (§3.7), which
/// this does not implement. `t²` is the minimal ramp whose first derivative vanishes at the outer
/// end, so the patch leaves `O` tangent to the sharp chain instead of creasing there; measured
/// against the oracle it lands 0.111 vs 0.066 at `t=1/3` and 0.444 vs 0.428 at `t=2/3`. The patch is
/// therefore vertex-exact at `O`, `S0`, `S1` and on the whole top arc, and approximate on the two
/// side chains — do NOT write a gate that demands side-chain parity without porting `M_ADJ` first.
pub(crate) fn inner_miter_patch(
    chain_om: &[Vec3], d_a: Vec3, d_b: Vec3, spread: f32,
) -> Vec<Vec<Vec3>> {
    let ns = chain_om.len().saturating_sub(1);
    let mut levels: Vec<Vec<Vec3>> = Vec::with_capacity(ns + 1);
    if ns == 0 { return levels; }
    levels.push(vec![chain_om[0]]);
    for i in 1..=ns {
        let t = i as f32 / ns as f32;
        let (_, _, pts) = inner_miter_arc(chain_om[i], d_a, d_b, spread * t * t, i);
        levels.push(pts);
    }
    levels
}

/// Index on the TOP arc (`levels[ns]`, `0 = S0` … `ns = S1`) where the two adjoining WALL faces meet
/// — i.e. where the unbeveled edge's foot lands once the arc has replaced the sharp meet point.
///
/// Measured over `ns` 1→6 in `miter_inner_wall_split.py` (§3.8a): Blender splits at `ceil(ns/2)`
/// counted from the `S1` end, which is `floor(ns/2)` from the `S0` end — the exact middle for even
/// `ns`, biased one step for odd. The odd-`ns` bias is a tie-break from Blender's internal edge
/// order, so a port only has to be DETERMINISTIC; what it must NOT do is split mid-segment, which
/// leaves no vertex to be the foot and tears the corner open.
pub(crate) fn inner_miter_wall_split(ns: usize) -> usize {
    ns / 2
}

/// Blender `build_boundary` (clean-room, FLAT/SHARP) for a MITER vertex `v`: walk the CCW fan, and for each
/// consecutive beveled pair place a BoundVert per Blender's decision tree — 0 between-edges → coplanar rail
/// meet; exactly 1 → loop slide onto it; ≥2 → floating rail meet then drop-onto-face. `v` is deleted and
/// replaced by the BoundVert ring. Returns `(boundverts, rail_override[(he,v)]→pos, corner_bv[ce]→pos,
/// miter_arc)`: STEP 2 welds each rail to its BoundVert; STEP 3 replaces the corner with `corner_bv[ce]`
/// (ce.leftv). `miter_arc` is `Some(chain)` ONLY for a ROUNDED (`ns>1`) 2-BoundVert miter — the `ns+1`
/// welded profile points bridging the two BoundVerts (the M_NONE weld, §3.7); `None` otherwise (flat or
/// N-way, which stays a bare edge / M_POLY cap). The chain runs from `boundverts[0]` to `boundverts[1]`.
pub(crate) fn build_boundary_flat(
    mesh: &HalfEdgeMesh, v: u32, beveled_he: &HashSet<u32>, orig_pos: &[Vec3],
    w_of: &dyn Fn(u32) -> f32,
    ns: usize, shape: f32,
) -> (Vec<Vec3>, HashMap<(u32, u32), Vec3>, HashMap<u32, Vec3>, Option<Vec<Vec3>>) {
    let mut boundverts: Vec<Vec3> = Vec::new();
    let mut rail_override: HashMap<(u32, u32), Vec3> = HashMap::new();
    let mut corner_bv: HashMap<u32, Vec3> = HashMap::new();
    let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
    let n = fan.len();
    if n == 0 { return (boundverts, rail_override, corner_bv, None); }
    let bev_idx: Vec<usize> = (0..n).filter(|&i| beveled_he.contains(&fan[i].0)).collect();
    if bev_idx.len() < 2 { return (boundverts, rail_override, corner_bv, None); }

    let vpos = orig_pos[v as usize];
    let v_no = mesh.compute_vertex_normal(crate::half_edge::VertexId(v));
    let far = |h: HalfEdgeId| orig_pos[mesh.he(h).vertex.0 as usize];
    for a in 0..bev_idx.len() {
        let i = bev_idx[a];
        let j = bev_idx[(a + 1) % bev_idx.len()];
        let e1 = fan[i];
        let e2 = fan[j];
        let twin_e1 = mesh.he(e1).twin;
        if twin_e1 == NO_TWIN { continue; }
        let rail_meet = || match (
            inward_offset_line(mesh, orig_pos, twin_e1, v, w_of(twin_e1.0)),
            inward_offset_line(mesh, orig_pos, e2, v, w_of(e2.0)),
        ) {
            (Some((a0, adir)), Some((b0, bdir))) => Some(isect_lines_3d(a0, a0 + adir, b0, b0 + bdir)),
            _ => None,
        };
        let between = (j + n - i) % n; // fan steps e1→e2; between-edges = between-1
        let bv = if between == 1 {
            rail_meet()
        } else if between == 2 {
            // ONE unbeveled edge between the two beveled ones. Blender takes the rail MEET and
            // PROJECTS it onto that edge (§3.2) — both rails then share ONE BoundVert. Measured
            // vertex-exact against the oracle at sx ∈ {0.5,1,2,3} (§11 2026-08-15); the older
            // averaged `loop_slide_meet2` agrees only in the symmetric case and is kept as the
            // fallback for when the rails are too degenerate to meet at all.
            let emid = fan[(i + 1) % n];
            rail_meet()
                .and_then(|m| project_meet_onto_edge(vpos, m, far(emid)))
                .or_else(|| loop_slide_meet2(vpos, far(e1), far(e2), far(emid),
                                             w_of(e1.0), w_of(e2.0), v_no))
        } else {
            rail_meet().map(|m| drop_onto_faces(mesh, orig_pos, v, &fan, i, j, m))
        };
        let bv = match bv { Some(p) => p, None => continue };
        boundverts.push(bv);
        rail_override.insert((twin_e1.0, v), bv);
        rail_override.insert((e2.0, v), bv);
        let sector_len = (j + n - i) % n;
        for step in 1..=sector_len {
            corner_bv.insert(fan[(i + step) % n].0, bv);
        }
    }
    // ROUNDED 2-EDGE MITER (M_NONE weld, §3.7): with exactly two BoundVerts and segments>1, bridge them
    // with the co-planarized profile arc. `boundverts[0]` and `[1]` are the two miter corners; the beveled
    // edge whose direction defines the weld profile is `fan[bev_idx[0]]` (either edge gives the SAME welded
    // chain — Blender co-planarizes both onto one plane). N-way (>2 BoundVerts) stays flat here (its M_POLY
    // cap is STEP 5b); the M_ADJ rounded grid is a separate future task (§9).
    let miter_arc = if ns > 1 && boundverts.len() == 2 {
        let edge_far = far(fan[bev_idx[0]]);
        Some(miter_weld_arc(boundverts[0], boundverts[1], vpos, edge_far, ns, shape))
    } else {
        None
    };
    (boundverts, rail_override, corner_bv, miter_arc)
}

/// Recessed CENTER for a k=1 PARTIAL corner at a degree>3 far-endpoint (Blender `offset_on_edge_between`,
/// the deleted-vertex merged point). ONE beveled edge lands on V; its chamfer contributes two rim points on
/// the two FLANKING fan edges (both computed elsewhere). The remaining "back" region collapses to a single
/// point that SLIDES off V by exactly the offset `w` along the back fan edge — the non-beveled edge whose
/// BOTH incident faces are un-beveled (i.e. neither touches the single beveled edge). At the degree-4
/// octahedron base this is unique (X↔−Y) and `V + w·normalize(far − V)` lands vertex-for-vertex on the
/// Blender oracle. `None` when no unique back edge exists (e.g. some round bevel-on-beveled corners), so the
/// caller keeps its arc-centroid fallback. A fan edge is "beveled" if it OR its twin is in `beveled_he`.
pub(crate) fn partial_back_edge_slide(
    mesh: &HalfEdgeMesh, beveled_he: &HashSet<u32>, orig_pos: &[Vec3], v: u32, fan: &[HalfEdgeId], w: f32,
) -> Option<Vec3> {
    let is_bev = |h: HalfEdgeId| {
        if beveled_he.contains(&h.0) { return true; }
        let t = mesh.he(h).twin;
        t != NO_TWIN && beveled_he.contains(&t.0)
    };
    // The back fan edge: non-beveled, and BOTH of its incident faces are also non-beveled at this corner.
    // In the CCW fan the face between fan[k] and fan[k+1] is he(fan[k+1]).face; an edge's two faces are the
    // one it opens (he(edge).face) and the one on its other side (he(edge.twin).face). An edge is a FLANKING
    // edge iff it is adjacent (fan-consecutive) to the single beveled edge — so require BOTH the previous and
    // next fan edges to be non-beveled too.
    let n = fan.len();
    let mut candidates: Vec<HalfEdgeId> = Vec::new();
    for i in 0..n {
        let e = fan[i];
        if is_bev(e) { continue; }
        let prev_e = fan[(i + n - 1) % n];
        let next_e = fan[(i + 1) % n];
        if is_bev(prev_e) || is_bev(next_e) { continue; } // flanking a beveled edge → not a back edge
        candidates.push(e);
    }
    if candidates.len() != 1 { return None; } // no unique back edge → let the caller fall back
    let e = candidates[0];
    let far = orig_pos[mesh.he(e).vertex.0 as usize];
    let vpos = orig_pos[v as usize];
    let dir = (far - vpos).normalize_or_zero();
    if dir.length_squared() < 1e-12 { return None; }
    Some(vpos + dir * w)
}
