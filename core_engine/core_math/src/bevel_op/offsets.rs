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

/// 3D line–line meet (Blender `isect_line_line_v3`): the point where infinite lines a0→a1 and b0→b1
/// intersect, or the MIDPOINT of their closest approach when SKEW. `a0` if parallel. Unlike
/// `solve_corner_lines` (2D in-plane) this works when the two offset lines are NOT coplanar (degree>3 miter).
pub(crate) fn isect_lines_3d(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> Vec3 {
    let da = a1 - a0;
    let db = b1 - b0;
    let n = da.cross(db);
    let denom = n.length_squared();
    if denom < 1e-12 { return a0; }
    let dc = b0 - a0;
    let t = dc.cross(db).dot(n) / denom;
    let s = dc.cross(da).dot(n) / denom;
    0.5 * ((a0 + da * t) + (b0 + db * s))
}

/// Closest point on the infinite line `p0 + t·pd` to the infinite line `q0 + s·qd`
/// (Blender `project_to_edge` via `isect_line_line_v3`: the point ON the FIRST line nearest the second).
/// Falls back to `p0` when the two directions are parallel. Used to place the miter-weld profile's
/// middle control point — the closest point on the beveled edge to the segment between the two BoundVerts.
pub(crate) fn closest_point_line_line(p0: Vec3, pd: Vec3, q0: Vec3, qd: Vec3) -> Vec3 {
    let w0 = p0 - q0;
    let a = pd.dot(pd);
    let b = pd.dot(qd);
    let c = qd.dot(qd);
    let d = pd.dot(w0);
    let e = qd.dot(w0);
    let denom = a * c - b * b;
    if denom.abs() < 1e-12 { return p0; }
    let t = (b * e - c * d) / denom;
    p0 + pd * t
}

/// Intersect the line `p0 + t·pd` with the plane through `plane_co` with normal `plane_no`
/// (Blender `isect_line_plane_v3`). Returns `p0` when the line is parallel to the plane.
pub(crate) fn isect_line_plane(p0: Vec3, pd: Vec3, plane_co: Vec3, plane_no: Vec3) -> Vec3 {
    let denom = pd.dot(plane_no);
    if denom.abs() < 1e-12 { return p0; }
    p0 + pd * ((plane_co - p0).dot(plane_no) / denom)
}

/// Inward OFFSET LINE of a beveled directed half-edge `he` at corner `v`: rail point `v + inward*w` plus
/// the edge direction, `inward` = STEP-1's centroid-sign-fixed in-plane perpendicular. Returns
/// `(rail_point_at_v, edge_dir)` for `isect_lines_3d`; `None` on a boundary/degenerate half-edge.
pub(crate) fn inward_offset_line(mesh: &HalfEdgeMesh, orig_pos: &[Vec3], he: HalfEdgeId, v: u32, w: f32) -> Option<(Vec3, Vec3)> {
    let f = mesh.he(he).face;
    if f == NO_FACE { return None; }
    let s = orig_pos[mesh.he_source(he).0 as usize];
    let d = orig_pos[mesh.he(he).vertex.0 as usize];
    let edge_dir = (d - s).normalize_or_zero();
    if edge_dir.length_squared() < 1e-12 { return None; }
    let normal = mesh.compute_face_normal(f);
    let inward = normal.cross(edge_dir).normalize_or_zero();
    if inward.length_squared() < 1e-12 { return None; }
    // `normal × edge_dir` already points into the face (interior = LEFT of the directed ring
    // boundary; Newell normal is winding-consistent) — valid for CONCAVE faces too, where the old
    // centroid flip picked the wrong side (see the STEP-1 note; the 90° staircase bug).
    Some((orig_pos[v as usize] + inward * w, edge_dir))
}

/// Blender LOOP SLIDE (`offset_on_edge_between`): pin the miter BoundVert ONTO the single non-beveled
/// between-edge `emid` (slide `0.5·(w/sin a1 + w/sin a2)` along it) instead of the floating 3D rail meet.
/// The slide distance is CLAMPED to the emid edge length (Blender `slide_dist`) — w/sin is unbounded for an
/// acute corner and would overshoot past the far vertex. `None` on a degenerate (parallel/straight) corner.
pub(crate) fn loop_slide_meet(v: Vec3, e1_far: Vec3, e2_far: Vec3, emid_far: Vec3, w: f32, v_no: Vec3) -> Option<Vec3> {
    loop_slide_meet2(v, e1_far, e2_far, emid_far, w, w, v_no)
}

/// Blender `offset_meet(..., edges_between = true)` (BEVEL.md §3.2, last line): take the two beveled
/// edges' RAIL MEET — the point they would share if the between-edge were not there — and PROJECT it
/// onto that between-edge. One BoundVert, shared by both rails.
///
/// This is the rule, measured (BEVEL.md §11 2026-08-15): the projection reproduces Blender exactly on
/// a box stretched to sx ∈ {0.5, 1, 2, 3} with a flat diagonal across its top face — (0.44,−0.88,1),
/// (0.9,−0.9,1), (1.88,−0.94,1), (2.88,−0.96,1) — while [`loop_slide_meet2`]'s averaged
/// `0.5·(w/sin a1 + w/sin a2)` matches ONLY at the symmetric sx=1, where every rule coincides.
/// Sliding each rail independently (`rail_endpoint_pos`'s terminal slide) yields TWO points instead
/// of one, which is what broke the corner rim into a non-chained ring.
///
/// The slide is clamped to the between-edge length (Blender `slide_dist`) and to `t >= 0`, so an
/// obtuse corner whose meet projects BEHIND `v` still lands on the edge rather than off the far end.
pub(crate) fn project_meet_onto_edge(v: Vec3, meet: Vec3, emid_far: Vec3) -> Option<Vec3> {
    let emid_vec = emid_far - v;
    let emid_len = emid_vec.length();
    if emid_len < 1e-6 { return None; }
    let emid_dir = emid_vec / emid_len;
    let t = (meet - v).dot(emid_dir).clamp(0.0, (emid_len - 50.0 * 1e-6).max(0.0));
    Some(v + emid_dir * t)
}

/// Per-edge-width variant (adjust_offsets): slide = 0.5·(w1/sin a1 + w2/sin a2).
pub(crate) fn loop_slide_meet2(v: Vec3, e1_far: Vec3, e2_far: Vec3, emid_far: Vec3, w1: f32, w2: f32, v_no: Vec3) -> Option<Vec3> {
    let e1_dir = (e1_far - v).normalize_or_zero();
    let e2_dir = (e2_far - v).normalize_or_zero();
    let emid_vec = emid_far - v;
    let emid_len = emid_vec.length();
    let emid_dir = emid_vec.normalize_or_zero();
    let _ = v_no; // Blender's reflex guard is outside-CCW; our vertex_fan is inside-CCW → sign inverts, drop it.
    let corner_sin = |d1: Vec3, d2: Vec3| -> Option<f32> {
        let ang = d1.dot(d2).clamp(-1.0, 1.0).acos();
        const GOOD: f32 = 0.01;
        if ang < GOOD || (ang - std::f32::consts::PI).abs() < GOOD { return None; }
        let s = ang.sin();
        if s < 1e-4 { None } else { Some(s) }
    };
    let s1 = corner_sin(e1_dir, emid_dir)?;
    let s2 = corner_sin(emid_dir, e2_dir)?;
    let d = (0.5 * (w1 / s1 + w2 / s2)).min((emid_len - 50.0 * 1e-6).max(0.0));
    Some(v + emid_dir * d)
}

/// Blender `point_between_edges`: is `co` inside the angular sector between edges (far ends `e1_far`,
/// `e2_far`) seen from `v` on a face with outward normal `f_no`? SIGNED wedge test (angles promoted to
/// [0,2π) via `cross(·)·f_no`), 2° slack. Gates the drop-onto-face. All dirs point TOWARD `v`.
pub(crate) fn point_between_edges(co: Vec3, v: Vec3, f_no: Vec3, e1_far: Vec3, e2_far: Vec3) -> bool {
    let dir1 = (v - e1_far).normalize_or_zero();
    let dir2 = (v - e2_far).normalize_or_zero();
    let dirco = (v - co).normalize_or_zero();
    let two_pi = std::f32::consts::TAU;
    let mut ang11 = dir1.dot(dir2).clamp(-1.0, 1.0).acos();
    let mut ang1co = dir1.dot(dirco).clamp(-1.0, 1.0).acos();
    if dir1.cross(dir2).dot(f_no) < 0.0 { ang11 = two_pi - ang11; }
    if dir1.cross(dirco).dot(f_no) < 0.0 { ang1co = two_pi - ang1co; }
    (ang11 - ang1co) > -0.0349
}

/// Blender `offset_meet(edges_between=true)` DROP-ONTO-FACE: for ≥2 between-edges the floating skew rail
/// meet hangs off the surface; re-pin it by PERPENDICULARLY projecting onto each intermediate face plane
/// (through `v`) and accepting the first whose wedge (`point_between_edges`) contains it. Keeps the float
/// if none accept. The wedge face of fan-consecutive `fan[k]`/`fan[k+1]` is resolved DIRECTLY as their
/// shared face — the old `he(fan[k+1]).face` shortcut assumed a fan winding that inverts on some meshes
/// (the octahedron k=2-adjacent corner: every drop got wedge-rejected against the WRONG face and the
/// float survived at 1.33·|v| off-surface; Blender's drop lands (−0.0544,−0.0544,0.8911), oracle-exact).
pub(crate) fn drop_onto_faces(mesh: &HalfEdgeMesh, orig_pos: &[Vec3], v: u32, fan: &[HalfEdgeId], i: usize, j: usize, meetco: Vec3) -> Vec3 {
    let n = fan.len();
    if n == 0 { return meetco; }
    let vpos = orig_pos[v as usize];
    let far = |h: HalfEdgeId| orig_pos[mesh.he(h).vertex.0 as usize];
    // Shared face of two consecutive fan edges — winding-free (checks both sides of each half-edge).
    let faces_of = |h: HalfEdgeId| -> [FaceId; 2] {
        let t = mesh.he(h).twin;
        [mesh.he(h).face, if t != NO_TWIN { mesh.he(t).face } else { NO_FACE }]
    };
    let shared_face = |a: HalfEdgeId, b: HalfEdgeId| -> Option<FaceId> {
        let fb = faces_of(b);
        faces_of(a).into_iter()
            .find(|&f| f != NO_FACE && (f == fb[0] || f == fb[1]))
    };
    let mut k = i;
    while k != j {
        let e = fan[k];
        let enext = fan[(k + 1) % n];
        if let Some(f) = shared_face(e, enext) {
            let f_no = mesh.compute_face_normal(f);
            if f_no.length_squared() > 1e-12 {
                let dropco = meetco - f_no * (meetco - vpos).dot(f_no);
                // Orient the wedge so it contains the FACE INTERIOR (centroid): the signed wedge
                // promotion in `point_between_edges` depends on fan handedness (inside- vs outside-CCW
                // — the §11 reflex-guard trap), which inverted a 60° face corner into a 300° wedge that
                // swallowed the un-dropped float. The face centroid defines the interior, winding-free.
                let fv = mesh.face_vertices(f);
                let cen: Vec3 = fv.iter().map(|x| orig_pos[x.0 as usize]).sum::<Vec3>() / fv.len().max(1) as f32;
                let (a_far, b_far) = if point_between_edges(cen, vpos, f_no, far(e), far(enext)) {
                    (far(e), far(enext))
                } else {
                    (far(enext), far(e))
                };
                if point_between_edges(dropco, vpos, f_no, a_far, b_far) {
                    return dropco;
                }
            }
        }
        k = (k + 1) % n;
    }
    meetco
}

/// Quadratic Bézier point: `a`=start, `c`=control, `b`=end, at `t ∈ [0,1]`. The rounded-bevel profile
/// pulls the control toward the ORIGINAL edge vertex, bulging the flat chord into a convex fillet.
#[inline]
pub(crate) fn quad_bezier(a: Vec3, c: Vec3, b: Vec3, t: f32) -> Vec3 {
    let u = 1.0 - t;
    a * (u * u) + c * (2.0 * u * t) + b * (t * t)
}

/// CIRCULAR bevel profile (Blender's Superellipse profile=0.5): the quarter ELLIPSE through `a_l` (t=0)
/// and `a_r` (t=1) in the parallelogram frame (centre = a_l + a_r − apex; p(θ) = c + cosθ·(a_l−c) +
/// sinθ·(a_r−c)) — Blender's `make_unit_square_map` profile, the same form the chain-weld oracles decoded.
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
