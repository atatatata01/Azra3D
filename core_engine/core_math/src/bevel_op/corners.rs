//! Corner / miter construction (BEVEL.md §4 STEP 2b / STEP 5b, §3.7 M_NONE weld).
//!
//! Pure relocation: `build_boundary_flat`, `miter_weld_arc`, `partial_back_edge_slide`.

use glam::Vec3;
use std::collections::{HashMap, HashSet};
use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, NO_FACE, NO_TWIN};
use super::{
    drop_onto_faces, inward_offset_line, lines_meet_3d,
    project_meet_onto_edge,
};

/// Rounded two-edge miter arc: the profile that bridges the two boundary points `a` and `b` left by a
/// vertex `orig` where exactly two bevelled edges meet. Independent implementation from the published
/// Lamé-curve (superellipse) definition: the profile is the affine image of the unit superellipse
/// `x^n + y^n = 1` (quarter, first quadrant) in the parallelogram spanned at the sharp corner `orig`
/// by its two legs `orig→a` and `orig→b`; the curve's centre is the opposite corner `a + b − orig`.
/// `shape = 0.5` gives `n = 2` (a circle in that frame, sampled at equal angles). The exponent is
/// derived from where the curve crosses the unit-square diagonal (`r = 2^(−1/n)`): `r = 0.5`
/// (straight chord, `n = 1`) at `shape = 0`, `r = 1/√2` at `0.5`, `r → 1` (square corner, `n → ∞`)
/// at `shape = 1`, piecewise linear in between.
/// Returns `ns+1` points from `a` (i=0) to `b` (i=ns). `edge_far` fixes the bevelled edge direction and
/// is only used to keep the signature stable for callers (the arc lies in the plane of `a`, `orig`, `b`).
pub(crate) fn miter_weld_arc(a: Vec3, b: Vec3, orig: Vec3, edge_far: Vec3, ns: usize, shape: f32) -> Vec<Vec3> {
    let _ = edge_far;
    let ns = ns.max(1);
    let straight = |out: &mut Vec<Vec3>| { for i in 0..=ns { out.push(a.lerp(b, i as f32 / ns as f32)); } };
    let mut out = Vec::with_capacity(ns + 1);
    // Corner on the chord: nothing to round (a flat bend) — equal thirds along a–b.
    let ab = b - a;
    let ab_len2 = ab.length_squared();
    if ab_len2 <= 1e-12 { straight(&mut out); return out; }
    let t = (orig - a).dot(ab) / ab_len2;
    if (orig - (a + ab * t)).length_squared() < 1e-8 * ab_len2 { straight(&mut out); return out; }
    // Exponent from the diagonal-crossing ratio.
    let s = shape.clamp(0.0, 1.0);
    let r = if s <= 0.5 { 0.5 + s * (std::f32::consts::FRAC_1_SQRT_2 - 0.5) / 0.5 }
            else { std::f32::consts::FRAC_1_SQRT_2 + (s - 0.5) / 0.5 * (1.0 - std::f32::consts::FRAC_1_SQRT_2) };
    let r = r.clamp(0.5, 0.999);
    let n_exp = (0.5f32).ln() / r.ln();          // Lamé exponent
    let centre = a + b - orig;
    let (ua, ub) = (a - centre, b - centre);
    for i in 0..=ns {
        let th = std::f32::consts::FRAC_PI_2 * i as f32 / ns as f32;
        let (sx, sy) = if i == 0 { (1.0, 0.0) } else if i == ns { (0.0, 1.0) }
            else { (th.cos().powf(2.0 / n_exp), th.sin().powf(2.0 / n_exp)) };
        out.push(centre + ua * sx + ub * sy);
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
/// Boundary construction at a bevelled vertex `v` (independent implementation from planar
/// offset-line geometry). Walk the closed vertex fan; each consecutive pair of bevelled edges gets
/// ONE boundary vertex that both of their rails end at, placed so it stays on the original surface:
///  * no un-bevelled edge between the pair: the pair's rails lie in one face plane and simply cross;
///  * exactly one between: their (generally skew) rail meet is projected onto that edge;
///  * two or more: the floating rail meet is dropped onto the first intermediate face plane whose
///    corner wedge contains it.
/// `v` is then removed and replaced by that ring. Returns `(boundverts, rail_override[(he, v)]→pos,
/// corner_bv[ce]→pos, miter_arc)`. Boundary vertex `k` belongs to the span from the k-th to the
/// (k+1)-th bevelled fan edge; every face corner `ce` (outgoing from `v`) inside that span maps to it,
/// and the two rails flanking a bevelled edge end at the boundary vertices of the spans on each side.
/// `miter_arc` is `Some` only for a ROUNDED (`ns>1`) two-boundary-vertex miter: the `ns+1` profile
/// points from `boundverts[0]` to `boundverts[1]` ([`miter_weld_arc`]). Fails closed (empty result)
/// on an open fan, fewer than two bevelled edges, or degenerate geometry.
pub(crate) fn build_boundary_flat(
    mesh: &HalfEdgeMesh, v: u32, beveled_he: &HashSet<u32>, orig_pos: &[Vec3],
    w_of: &dyn Fn(u32) -> f32,
    ns: usize, shape: f32,
) -> (Vec<Vec3>, HashMap<(u32, u32), Vec3>, HashMap<u32, Vec3>, Option<Vec<Vec3>>) {
    let empty = || (Vec::new(), HashMap::new(), HashMap::new(), None);
    let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
    let n = fan.len();
    if n < 2 { return empty(); }
    // Closed fan only: stepping past the last edge must come back to the first.
    let last_twin = mesh.he(fan[n - 1]).twin;
    if last_twin == NO_TWIN || mesh.he(last_twin).next != fan[0] { return empty(); }
    if fan.iter().any(|&h| mesh.he(h).twin == NO_TWIN || mesh.he(h).face == NO_FACE) { return empty(); }
    let bev_idx: Vec<usize> = (0..n).filter(|&i| beveled_he.contains(&fan[i].0)).collect();
    let m = bev_idx.len();
    if m < 2 { return empty(); }
    let vpos = orig_pos[v as usize];

    let mut boundverts: Vec<Vec3> = Vec::with_capacity(m);
    for k in 0..m {
        let (i1, i2) = (bev_idx[k], bev_idx[(k + 1) % m]);
        let between = (i2 + n - i1) % n - 1;
        let (e1, e2) = (fan[i1], fan[i2]);
        let tw1 = mesh.he(e1).twin;
        let (l1, l2) = match (
            inward_offset_line(mesh, orig_pos, tw1, v, w_of(tw1.0)),
            inward_offset_line(mesh, orig_pos, e2, v, w_of(e2.0)),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return empty(),
        };
        let meet = lines_meet_3d(l1.0, l1.1, l2.0, l2.1);
        let pos = match between {
            0 => meet,
            1 => {
                let emid = fan[(i1 + 1) % n];
                project_meet_onto_edge(vpos, meet, orig_pos[mesh.he(emid).vertex.0 as usize]).unwrap_or(meet)
            }
            _ => drop_onto_faces(mesh, orig_pos, v, &fan, i1, i2, meet),
        };
        boundverts.push(pos);
    }

    let mut rail_override: HashMap<(u32, u32), Vec3> = HashMap::new();
    let mut corner_bv: HashMap<u32, Vec3> = HashMap::new();
    for k in 0..m {
        let e = fan[bev_idx[k]];
        rail_override.insert((mesh.he(e).twin.0, v), boundverts[k]);                 // right face: span k
        rail_override.insert((e.0, v), boundverts[(k + m - 1) % m]);                // left face: span k-1
        let (i1, i2) = (bev_idx[k], bev_idx[(k + 1) % m]);
        for s in 0..((i2 + n - i1) % n) {
            corner_bv.insert(fan[(i1 + s + 1) % n].0, boundverts[k]);
        }
    }
    let arc = if m == 2 && ns > 1 {
        Some(miter_weld_arc(boundverts[0], boundverts[1], vpos, vpos, ns, shape))
    } else { None };
    (boundverts, rail_override, corner_bv, arc)
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
