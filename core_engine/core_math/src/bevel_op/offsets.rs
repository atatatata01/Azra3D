//! Offset / meet / profile primitives (BEVEL.md §4 STEP 1–2, §3.2 offset_meet).
//!
//! Pure relocation of geometry helpers used by rails, corners, and the pipeline body.

use glam::Vec3;
use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, FaceId, NO_TWIN, NO_FACE};

/// Order a small set of welded ring points into the arc that runs from the pe-side (`pe_src`) to the
/// ce-side (`ce_dst`) about corner `v_pos`, so the un-beveled-but-adjacent face (`(false,false)` corner)
/// inserts V's rail points WITHOUT self-intersecting the re-traced ring.
///
/// The sweep axis is `(pe_src - V) × (ce_dst - V)` — derived from the corner itself, NOT from an external
/// face/vertex normal. Sorting by angle from the pe-direction around that axis puts the pe-side point
/// first and the ce-side point last, monotonically. This is handedness-free: passing a face or vertex
/// normal as the axis (the old bug) could flip the in-plane orientation and REVERSE the pair, twisting
/// the face into a self-intersecting ring (the "edges slide along the wrong face" bevel defect).
pub(crate) fn order_arc_about_vertex(idxs: &mut Vec<usize>, positions: &[Vec3], v_pos: Vec3, pe_src: Vec3, ce_dst: Vec3) {
    if idxs.len() < 2 { return; }
    let ux = (pe_src - v_pos).normalize_or_zero();
    let to_ce = (ce_dst - v_pos).normalize_or_zero();
    let axis = ux.cross(to_ce);
    if axis.length_squared() < 1e-12 { return; } // collinear corner — no meaningful arc to order
    let axis = axis.normalize();
    // CRASH FIX (owner 2026-08-05, panic during a bevel drag: "user-provided comparison function
    // does not correctly implement a total order").
    //
    // The comparator was `partial_cmp(..).unwrap_or(Equal)`. That is NOT a total order: a NaN
    // angle compares Equal to EVERY other value, so `a == b` and `a == c` while `b < c` —
    // transitivity is broken and Rust's sort detects it and PANICS. It is wrong independently of
    // whether a NaN ever shows up; `unwrap_or(Equal)` on floats is never a valid ordering.
    //
    // `total_cmp` IS a total order over all f32 including NaN, so the sort is now well-defined by
    // construction. That is a fix to the COMPARATOR, not a mask over bad data — hence the
    // assertion below, which still fails loudly in dev if a non-finite angle ever reaches here,
    // so the arithmetic that produced it can be found rather than silently sorted.
    let angle = |i: usize| -> f32 {
        let p = positions[i] - v_pos;
        let a = axis.dot(ux.cross(p)).atan2(ux.dot(p));
        debug_assert!(
            a.is_finite(),
            "non-finite arc angle at bevel corner: idx={i} pos={:?} v_pos={v_pos:?}",
            positions[i]
        );
        a
    };
    idxs.sort_by(|&a, &b| angle(a).total_cmp(&angle(b)));
}

/// Intersect two coplanar in-plane offset lines at a shared corner vertex V. Each beveled edge's rail is
/// its edge translated inward by its face `dir`; the two rails of the two edges meeting at V cross at the
/// corner vertex. Solves the 2×2 system in the face plane (face normal as up). Falls back to the averaged
/// offset for near-parallel rails (collinear edges). `(pe_*)` = incoming edge to V, `(ce_*)` = outgoing.
pub(crate) fn solve_corner_lines(
    v_pos: Vec3, n_face: Vec3,
    pe_dir: Vec3, pe_edge: Vec3,
    ce_dir: Vec3, ce_edge: Vec3,
    w: f32,
) -> Vec3 {
    solve_corner_lines2(v_pos, n_face, pe_dir, pe_edge, w, ce_dir, ce_edge, w)
}

/// Per-edge-width variant (adjust_offsets): each rail is displaced by ITS edge's adjusted width.
pub(crate) fn solve_corner_lines2(
    v_pos: Vec3, n_face: Vec3,
    pe_dir: Vec3, pe_edge: Vec3, pe_w: f32,
    ce_dir: Vec3, ce_edge: Vec3, ce_w: f32,
) -> Vec3 {
    let a0 = v_pos + pe_dir * pe_w;
    let b0 = v_pos + ce_dir * ce_w;
    let ux = if pe_edge.length_squared() > 1e-12 { pe_edge.normalize() } else { ce_edge.normalize_or_zero() };
    let uy = n_face.cross(ux).normalize_or_zero();
    let to2 = |p: Vec3| -> (f32, f32) { ((p - v_pos).dot(ux), (p - v_pos).dot(uy)) };
    let (a0x, a0y) = to2(a0);
    let (b0x, b0y) = to2(b0);
    let d1 = (pe_edge.dot(ux), pe_edge.dot(uy));
    let d2 = (ce_edge.dot(ux), ce_edge.dot(uy));
    let denom = d1.0 * (-d2.1) - d1.1 * (-d2.0);
    if denom.abs() < 1e-9 {
        return v_pos + 0.5 * (pe_dir * pe_w + ce_dir * ce_w);
    }
    let rhs = (b0x - a0x, b0y - a0y);
    let t = (rhs.0 * (-d2.1) - rhs.1 * (-d2.0)) / denom;
    v_pos + ux * (a0x + t * d1.0) + uy * (a0y + t * d1.1)
}

/// Inward OFFSET LINE of a directed half-edge `he` seen from vertex `v` (one of its endpoints): the
/// edge's own line shifted by `w` inside the face to its left, in that face's plane. Independent
/// construction: with the face normal `n` (CCW winding) the interior lies to the LEFT of travel,
/// i.e. along `n × dir`. Returns `(point_on_line_nearest_v, unit_edge_dir)`; `None` on a boundary or
/// degenerate half-edge.
pub(crate) fn inward_offset_line(mesh: &HalfEdgeMesh, orig_pos: &[Vec3], he: HalfEdgeId, v: u32, w: f32) -> Option<(Vec3, Vec3)> {
    let h = mesh.he(he);
    if h.face == NO_FACE { return None; }
    let a = orig_pos[mesh.he_source(he).0 as usize];
    let b = orig_pos[h.vertex.0 as usize];
    let dir = (b - a).normalize_or_zero();
    let n = mesh.compute_face_normal(h.face).normalize_or_zero();
    if dir == Vec3::ZERO || n == Vec3::ZERO { return None; }
    let inward = n.cross(dir).normalize_or_zero();
    if inward == Vec3::ZERO { return None; }
    Some((orig_pos[v as usize] + inward * w, dir))
}

/// Meeting point of two 3D lines `p0 + s·pd` and `q0 + t·qd`: the intersection when they cross, else
/// the midpoint of their closest approach (standard two-line closest-point solve). For parallel lines
/// (no unique meet) the midpoint of the two anchor points is returned.
pub(crate) fn lines_meet_3d(p0: Vec3, pd: Vec3, q0: Vec3, qd: Vec3) -> Vec3 {
    let r = p0 - q0;
    let (a, b, c) = (pd.dot(pd), pd.dot(qd), qd.dot(qd));
    let (d, e) = (pd.dot(r), qd.dot(r));
    let den = a * c - b * b;
    if den.abs() < 1e-12 * (a * c).max(1e-30) { return (p0 + q0) * 0.5; }
    let s = (b * e - c * d) / den;
    let t = (a * e - b * d) / den;
    ((p0 + pd * s) + (q0 + qd * t)) * 0.5
}

/// Orthogonal projection of `meet` onto the segment `v → far` (parameter clamped to the segment).
/// This pins a boundary vertex ONTO the single un-bevelled edge lying between two bevelled ones, so
/// it stays on the original surface. `None` for a zero-length edge.
pub(crate) fn project_meet_onto_edge(v: Vec3, meet: Vec3, far: Vec3) -> Option<Vec3> {
    let e = far - v;
    let len2 = e.length_squared();
    if len2 <= 1e-20 { return None; }
    let t = ((meet - v).dot(e) / len2).clamp(0.0, 1.0);
    Some(v + e * t)
}

/// Is `p` inside the angular wedge at `v` of a face with normal `n` (CCW), bounded by the direction
/// to `next_far` (where the wedge starts) rotating counter-clockwise about `n` to the direction to
/// `prev_far`? 2° slack at both edges. All directions are taken from `v`.
pub(crate) fn point_in_wedge(p: Vec3, v: Vec3, n: Vec3, next_far: Vec3, prev_far: Vec3) -> bool {
    let d0 = (next_far - v).normalize_or_zero();
    let d1 = (prev_far - v).normalize_or_zero();
    let x = (p - v).normalize_or_zero();
    if d0 == Vec3::ZERO || d1 == Vec3::ZERO || x == Vec3::ZERO { return false; }
    let ang = |u: Vec3| -> f32 {
        let mut a = n.dot(d0.cross(u)).atan2(d0.dot(u));
        if a < 0.0 { a += std::f32::consts::TAU; }
        a
    };
    let slack = 2.0f32.to_radians();
    let (ax, a1) = (ang(x), ang(d1));
    ax <= a1 + slack || ax >= std::f32::consts::TAU - slack
}

/// Re-pin a floating boundary point onto the surface when 2+ un-bevelled edges lie between two
/// bevelled ones: orthogonally project `meet` onto the plane (through `v`) of each intermediate face
/// of the span, in order, and accept the first projection that falls inside that face's wedge at `v`.
/// `fan` is the vertex fan, `i`/`j` the fan indices of the bracketing bevelled edges (fan order); the
/// intermediate faces are those between consecutive between-edges. Keeps `meet` when none accepts.
pub(crate) fn drop_onto_faces(mesh: &HalfEdgeMesh, orig_pos: &[Vec3], v: u32, fan: &[HalfEdgeId], i: usize, j: usize, meet: Vec3) -> Vec3 {
    let n_fan = fan.len();
    let vp = orig_pos[v as usize];
    let span = (j + n_fan - i) % n_fan;
    for s in (i + 1)..(i + span.saturating_sub(1)) {
        let (e_a, e_b) = (fan[s % n_fan], fan[(s + 1) % n_fan]);
        let face = mesh.he(e_b).face;      // the face shared by consecutive fan edges
        if face == NO_FACE { continue; }
        let n = mesh.compute_face_normal(face).normalize_or_zero();
        if n == Vec3::ZERO { continue; }
        let p = meet - n * n.dot(meet - vp);
        let far = |h: HalfEdgeId| orig_pos[mesh.he(h).vertex.0 as usize];
        if point_in_wedge(p, vp, n, far(e_b), far(e_a)) { return p; }
    }
    meet
}
/// Quadratic Bézier point: `a`=start, `c`=control, `b`=end, at `t ∈ [0,1]`. The rounded-bevel profile
/// pulls the control toward the ORIGINAL edge vertex, bulging the flat chord into a convex fillet.
#[inline]
pub(crate) fn quad_bezier(a: Vec3, c: Vec3, b: Vec3, t: f32) -> Vec3 {
    let u = 1.0 - t;
    a * (u * u) + c * (2.0 * u * t) + b * (t * t)
}

/// CIRCULAR bevel profile (superellipse exponent 2): the quarter ELLIPSE through `a_l` (t=0)
/// and `a_r` (t=1) in the parallelogram frame (centre = a_l + a_r − apex; p(θ) = c + cosθ·(a_l−c) +
/// sinθ·(a_r−c)) — the affine image of the unit circle in the corner parallelogram (independent derivation).
/// For symmetric perpendicular legs (a plain cube edge) this IS the circular arc (all golden oracles
/// unchanged). Crucially it NEVER leaves the (ua, ub) parallelogram: the previous slerp + radius-lerp
/// construction swept OUTSIDE it for strongly ASYMMETRIC legs (one rail loop-slid 10× the other along a
/// near-collinear edge) — the arc-rim re-bevel SPIKE (bbox +0.47, self-intersecting face; gate
/// `bevel_on_rounded_bevel_arc_rim_spike`). `apex` carries the `bulge` shaping (=orig vertex at 0.5 →
/// profile; pulled toward the chord for flatter, past it for convex).
pub(crate) fn circular_arc(a_l: Vec3, a_r: Vec3, apex: Vec3, t: f32) -> Vec3 {
    let center = a_l + a_r - apex;
    let ua = a_l - center;
    let ub = a_r - center;
    // DEGENERATE FRAME → chord. When the legs are (anti)parallel — the apex sits on/near the a_l→a_r
    // line, e.g. a rail loop-slid past the vertex — the ellipse plane is undefined and the quarter
    // ellipse collapses THROUGH the centre (cosθ·ua + sinθ·ub with ub ≈ −ua sweeps to zero and back →
    // a self-crossing "arc" → twisted strip quads). sin²(angle between legs) < (~3°)² → flat chord.
    let denom = ua.length_squared() * ub.length_squared();
    if denom < 1e-24 || ua.cross(ub).length_squared() / denom < 2.5e-3 {
        return a_l.lerp(a_r, t);
    }
    let ang = t * std::f32::consts::FRAC_PI_2;
    center + ua * ang.cos() + ub * ang.sin()
}

/// True when the edge is a **concave / reentrant** crease (interior dihedral > π).
///
/// Uses outward face normals: `n1 × n2 · edge_dir` is **negative** on a concave L-step
/// vertical edge and **positive** on a convex cube edge (verified both ways). Twin
/// orientation flips both cross and edge, so the product is stable.
///
/// Owner 2026-08-08: a single edge ending at a 3-spoke vertex was filleted convex on one
/// end and concave on the other — callers force concave treatment when this is true.
pub(crate) fn edge_is_concave(mesh: &HalfEdgeMesh, h: HalfEdgeId) -> bool {
    if !mesh.is_edge_live(h) {
        return false;
    }
    let e = mesh.he(h);
    if e.twin == NO_TWIN || e.face == NO_FACE {
        return false;
    }
    let tf = mesh.he(e.twin).face;
    if tf == NO_FACE || tf == e.face {
        return false;
    }
    let n1 = mesh.compute_face_normal(e.face).normalize_or_zero();
    let n2 = mesh.compute_face_normal(tf).normalize_or_zero();
    if n1.length_squared() < 0.25 || n2.length_squared() < 0.25 {
        return false;
    }
    let src = mesh.he_source(h);
    let dst = e.vertex;
    let edir = (mesh.vert(dst).position - mesh.vert(src).position).normalize_or_zero();
    if edir.length_squared() < 0.25 {
        return false;
    }
    n1.cross(n2).dot(edir) < 0.0
}

/// Ensure the profile arc bulges toward `apex_hint` (the original edge vertex), never away
/// from it. When rails / apex projection put the mid-arc on the wrong side of the L–R chord
/// (owner: one end of a concave crease filleted convex), reflect the apex through the chord
/// so the quarter-ellipse flips to the concave side.
pub(crate) fn apex_bulge_toward_edge(a_l: Vec3, a_r: Vec3, apex: Vec3) -> Vec3 {
    let chord = a_l.lerp(a_r, 0.5);
    let mid = circular_arc(a_l, a_r, apex, 0.5);
    // Arc must be closer to the original edge vertex than the flat chord is.
    if (mid - apex).length_squared() <= (chord - apex).length_squared() + 1e-10 {
        return apex;
    }
    // Reflect apex through the L–R midpoint → flips the circular_arc centre to the other side.
    a_l + a_r - apex
}
