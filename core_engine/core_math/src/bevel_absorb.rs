//! Bevel CONSUME GEOMETRY — carve a clean stripe for the bevel to fill (BEVEL.md §11).
//!
//! CAD-style fillet behaviour: when the requested bevel width exceeds the distance to the
//! next parallel edge loop, the geometry in the bevel's path is consumed instead of the
//! bevel sticking at the clamp. Runs on a working copy only — the caller's mesh is never
//! touched (the modifier re-derives per evaluation; the interactive drag per frame).
//!
//! v2 — THE STRIPE RULE (owner 2026-08-07, informed by Maya polyBevel / Blender Loop Slide):
//! Maya's bevel places the strip rim by sliding ALONG the adjacent edges (width = a
//! *Fraction* of the edge), so the strip always lies on the original surface. v1 instead
//! dissolved every blocking ring and let the bevel offset perpendicular across the merged
//! chord — on a curved wall the rim left the surface and the result visibly followed the
//! eaten rings. v2 carves a bounded stripe:
//!   * a ring is DISSOLVED only when its whole row fits inside the stripe (2×width — the
//!     clamp allows width w when the incident row is ≥ 2w);
//!   * the FIRST ring that would overshoot is not eaten — it is SLID down its rails so the
//!     stripe height is exactly 2×width. The slide moves each boundary vertex along its own
//!     existing rail edge, so the stripe boundary is a clean uniform polygon loop ON the
//!     original surface, and geometry beyond it has zero influence on the bevel.
//!
//! v2 scope (deliberate): QUAD-GRID rows only. Anything else — n-gons, triangles, a loop
//! that touches the beveled selection, boundary edges — is a barrier: fail closed to the
//! ordinary clamped bevel. Never a folded surface.
//!
//! The oracle is not Blender/Maya (neither consumes): absorb-then-bevel must equal
//! `bevel_edges_offset` on a mesh authored with the stripe already carved (see tests).

use crate::half_edge::{HalfEdgeId, HalfEdgeMesh, VertexId, NO_FACE, NO_TWIN};
use glam::Vec3;
use std::collections::{HashMap, HashSet};

/// `AZRA_CONSUME_DEBUG=1` — print why the fuse / carve refused (owner repro triage).
macro_rules! cdbg {
    ($($t:tt)*) => {
        if std::env::var_os("AZRA_CONSUME_DEBUG").is_some() { eprintln!($($t)*); }
    };
}

#[cfg(test)]
use crate::half_edge::FaceId;

/// Max carve rounds — each round eats at most one ring per side. Backstop only.
const MAX_ROUNDS: usize = 16;

/// One blocking rim half-edge and what lies below it (one quad row).
struct Occurrence {
    /// The selected rim half-edge whose quad row is too short for the stripe.
    rim_he: HalfEdgeId,
    /// Canonical (min-id) undirected key of the row's opposite edge — the blocking loop.
    opposite_key: u32,
    /// Row height = min rail length of this quad.
    row_height: f32,
    /// Height of the NEXT row below (None when the face below is missing / not a quad).
    row2_height: Option<f32>,
}

/// Carve the stripe that lets `selected` reach bevel `width`, on a copy.
///
/// Returns `Some(mesh)` when at least one ring was dissolved or slid and `validate()` stayed
/// green. Returns `None` when nothing blocked OR a gate refused — the caller keeps the
/// original mesh either way (fail closed).
///
/// `selected` holds DIRECTED half-edge ids (either direction names the undirected edge),
/// the same convention `bevel_edges_offset` takes. The carve never removes a selected edge,
/// and surviving ids stay valid (kernel ops are ID-preserving; slides only move positions);
/// the caller must still re-filter its set to live edges afterwards.
pub fn absorb_for_bevel(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    width: f32,
) -> Option<HalfEdgeMesh> {
    if width <= 0.0 || selected.is_empty() {
        return None;
    }
    // The stripe is 2×width: `bevel_edges_offset` clamps width to ½ the shortest incident
    // edge (bevel_op/mod.rs STEP 1), so a row of exactly 2w lets the bevel reach w with the
    // strip rim landing mid-row — bounded by the slid boundary loop, uniform along the edge.
    let stripe = 2.0 * width;
    let eps = (stripe * 1e-4).max(1e-6);

    let mut work = mesh.clone();
    let mut changed = false;
    let mut slid: HashSet<u32> = HashSet::new();

    for _round in 0..MAX_ROUNDS {
        let occs = blocking_occurrences(&work, selected, stripe, eps);
        if occs.is_empty() {
            break;
        }
        // Per blocking loop: DISSOLVE when every occurrence's merged row would still fit the
        // stripe (no overshoot); otherwise the loop is the stripe BOUNDARY — slide it.
        let mut fits: HashMap<u32, bool> = HashMap::new();
        for o in &occs {
            let f = match o.row2_height {
                Some(h2) => o.row_height + h2 <= stripe + eps,
                None => false, // nothing below to merge with — never dissolve into a barrier
            };
            fits.entry(o.opposite_key).and_modify(|v| *v &= f).or_insert(f);
        }
        let dissolve_keys: HashSet<u32> =
            fits.iter().filter(|&(_, &f)| f).map(|(&k, _)| k).collect();

        if !dissolve_keys.is_empty() {
            let Some(vetted) = check_absorbable(&work, selected, &dissolve_keys) else {
                break; // barrier — keep what we have (fail closed)
            };
            if !absorb_loop_edges(&mut work, &vetted) {
                return None; // half-eaten state is not installable
            }
            if work.validate().is_err() {
                return None;
            }
            changed = true;
            continue;
        }

        // Slide round: every remaining blocking loop is a stripe boundary. Final placement.
        for o in &occs {
            changed |= slide_boundary(&mut work, selected, o.rim_he, stripe, eps, &mut slid);
        }
        break;
    }

    if !changed {
        return None;
    }
    work.recompute_normals();
    if work.validate().is_err() {
        return None;
    }
    Some(work)
}

/// Collect every selected rim half-edge whose quad row is shorter than the stripe.
fn blocking_occurrences(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    stripe: f32,
    eps: f32,
) -> Vec<Occurrence> {
    let is_selected = |he: HalfEdgeId| -> bool {
        if selected.contains(&he.0) {
            return true;
        }
        let t = mesh.he(he).twin;
        t != NO_TWIN && selected.contains(&t.0)
    };
    let edge_len = |he: HalfEdgeId| -> f32 {
        let a = mesh.vert(mesh.he_source(he)).position;
        let b = mesh.vert(mesh.he(he).vertex).position;
        (b - a).length()
    };
    let mut out = Vec::new();
    for &e in selected {
        let he = HalfEdgeId(e);
        if (e as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(he) {
            continue;
        }
        // Both directions = both sides of the beveled loop.
        let twin = mesh.he(he).twin;
        let mut sides = vec![he];
        if twin != NO_TWIN {
            sides.push(twin);
        }
        for h in sides {
            let f = mesh.he(h).face;
            if f == NO_FACE || mesh.face_vertices(f).len() != 4 {
                continue; // non-quad side (e.g. a cap n-gon): nothing to carve
            }
            // CORNER QUAD (borders two selected edges — the loop turns here): never a
            // dissolvable row; its "opposite" is another run's territory. The corner fuse
            // claims it explicitly (consume_fuse_bevel_corners).
            if is_selected(mesh.he(h).next) || is_selected(mesh.he(h).prev) {
                continue;
            }
            let side_a = mesh.he(h).next;
            let side_b = mesh.he(h).prev;
            let d = edge_len(side_a).min(edge_len(side_b));
            if d >= stripe - eps {
                continue; // row already fits the stripe
            }
            let opposite = mesh.he(side_a).next;
            debug_assert_eq!(mesh.he(opposite).next, side_b, "quad ring arithmetic");
            if is_selected(opposite) {
                continue; // the next loop is itself beveled — a real wall, never food
            }
            let t = mesh.he(opposite).twin;
            let opposite_key = if t == NO_TWIN { opposite.0 } else { opposite.0.min(t.0) };
            // The row below (for the dissolve-vs-slide decision).
            let row2_height = if t != NO_TWIN {
                let f2 = mesh.he(t).face;
                if f2 != NO_FACE && mesh.face_vertices(f2).len() == 4 {
                    Some(edge_len(mesh.he(t).next).min(edge_len(mesh.he(t).prev)))
                } else {
                    None
                }
            } else {
                None
            };
            out.push(Occurrence { rim_he: h, opposite_key, row_height: d, row2_height });
        }
    }
    out
}

/// Slide the stripe-boundary vertices of `rim_he`'s row down their own rail edges so the row
/// height becomes exactly `stripe`. Each vertex moves ALONG the existing edge to the loop
/// below (Maya Fraction / Blender Loop Slide placement) — it stays on the original surface,
/// so the bevel is bounded by a clean uniform loop and the geometry below has no influence.
/// Returns true if any vertex moved. Skips (never distorts) anything ambiguous.
fn slide_boundary(
    mesh: &mut HalfEdgeMesh,
    selected: &HashSet<u32>,
    rim_he: HalfEdgeId,
    stripe: f32,
    eps: f32,
    slid: &mut HashSet<u32>,
) -> bool {
    if !mesh.is_edge_live(rim_he) {
        return false;
    }
    let side_a = mesh.he(rim_he).next; // v2 → u2
    let side_b = mesh.he(rim_he).prev; // u1 → v1
    let opposite = mesh.he(side_a).next; // u2 → u1
    let t_op = mesh.he(opposite).twin;
    if t_op == NO_TWIN {
        return false;
    }
    let f2 = mesh.he(t_op).face;
    if f2 == NO_FACE || mesh.face_vertices(f2).len() != 4 {
        return false; // no clean row below to slide into
    }
    let u2 = mesh.he(side_a).vertex;
    let u1 = mesh.he_source(side_b);
    let w2 = mesh.he(mesh.he(t_op).next).vertex; // below u2
    let w1 = mesh.he_source(mesh.he(t_op).prev); // below u1

    let sel_touches = |v: VertexId, m: &HalfEdgeMesh| -> bool {
        m.vertex_fan(v).iter().any(|&h| {
            let t = m.he(h).twin;
            selected.contains(&h.0) || (t != NO_TWIN && selected.contains(&t.0))
        })
    };

    let mut moved = false;
    let pairs: [(VertexId, VertexId, f32); 2] = [
        (u2, w2, {
            let a = mesh.vert(mesh.he_source(side_a)).position;
            let b = mesh.vert(u2).position;
            (b - a).length()
        }),
        (u1, w1, {
            let a = mesh.vert(mesh.he(side_b).vertex).position;
            let b = mesh.vert(u1).position;
            (b - a).length()
        }),
    ];
    for (u, w, d_v) in pairs {
        if slid.contains(&u.0) || !mesh.is_vert_live(u) || !mesh.is_vert_live(w) {
            continue;
        }
        if d_v >= stripe - eps {
            continue; // this rail already reaches the stripe
        }
        if sel_touches(u, mesh) {
            continue; // never move a vertex of the beveled selection itself
        }
        let pu: Vec3 = mesh.vert(u).position;
        let pw: Vec3 = mesh.vert(w).position;
        let h_v = (pw - pu).length();
        if h_v < 1e-9 {
            continue;
        }
        // Move u toward w so rail length d_v grows to `stripe`. Cap the fraction well below
        // 1 so the row below can never collapse or invert.
        let t = ((stripe - d_v) / h_v).clamp(0.0, 0.9);
        if t <= 0.0 {
            continue;
        }
        mesh.vert_mut(u).position = pu + (pw - pu) * t;
        slid.insert(u.0);
        moved = true;
    }
    moved
}

/// Gate: every loop edge in `keys` must be a clean interior quad-grid edge whose removal is
/// a pure row merge. Returns the vetted edges, or None to stop the carve entirely.
///
/// Conditions per edge: twinned (interior), neither face degenerate, and — the load-bearing
/// one — NO edge incident to its endpoints is in the beveled selection (the endpoint
/// vertices get collapsed by `join_edge_kill_vert`, which would invalidate a selected id).
fn check_absorbable(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    keys: &HashSet<u32>,
) -> Option<Vec<HalfEdgeId>> {
    let sel_undirected: HashSet<u32> = selected
        .iter()
        .filter(|&&e| (e as usize) < mesh.half_edges.len() && mesh.is_edge_live(HalfEdgeId(e)))
        .map(|&e| {
            let t = mesh.he(HalfEdgeId(e)).twin;
            if t == NO_TWIN { e } else { e.min(t.0) }
        })
        .collect();
    let mut out = Vec::new();
    for &e in keys {
        let he = HalfEdgeId(e);
        if !mesh.is_edge_live(he) {
            return None;
        }
        let twin = mesh.he(he).twin;
        if twin == NO_TWIN {
            return None; // boundary — not an interior row merge
        }
        if mesh.he(he).face == NO_FACE || mesh.he(twin).face == NO_FACE {
            return None;
        }
        for v in [mesh.he_source(he), mesh.he(he).vertex] {
            for out_he in mesh.vertex_fan(v) {
                let t = mesh.he(out_he).twin;
                let key = if t == NO_TWIN { out_he.0 } else { out_he.0.min(t.0) };
                if sel_undirected.contains(&key) {
                    return None; // absorbing would touch the beveled selection itself
                }
            }
        }
        out.push(he);
    }
    out.sort_by_key(|h| h.0); // HashSet order is random — deterministic dissolve order
    Some(out)
}

/// Dissolve the vetted loop edges, then collapse the valence-2 vertices they leave behind
/// (merging each pair of rail edges into one longer edge — which is what raises the clamp
/// limit). Returns false if any kernel op refuses mid-way.
fn absorb_loop_edges(mesh: &mut HalfEdgeMesh, loop_edges: &[HalfEdgeId]) -> bool {
    let mut touched: Vec<VertexId> = Vec::new();
    for &he in loop_edges {
        if !mesh.is_edge_live(he) {
            continue; // already gone via an earlier merge this round
        }
        touched.push(mesh.he_source(he));
        touched.push(mesh.he(he).vertex);
        if mesh.dissolve_edge(he).is_none() {
            return false;
        }
    }
    touched.sort_by_key(|v| v.0);
    touched.dedup();
    for v in touched {
        if !mesh.is_vert_live(v) {
            continue;
        }
        if mesh.vertex_fan(v).len() == 2 {
            // Interior valence-2 leftover of the row merge. A refusal here (boundary
            // configuration) leaves a valid mesh — the vertex just stays.
            let _ = mesh.join_edge_kill_vert(v);
        }
    }
    true
}

/// Clear the 4 end rails of every open chain (both sides × both ends). True = all clean.
fn clear_open_chain_end_rails(
    work: &mut HalfEdgeMesh,
    loops: &[(Vec<HalfEdgeId>, bool)],
    width: f32,
    eps: f32,
) -> bool {
    for (hes, closed) in loops {
        if *closed {
            continue;
        }
        let m = hes.len();
        let h_first = hes[0];
        let h_last = hes[m - 1];
        let t_first = work.he(h_first).twin;
        let t_last = work.he(h_last).twin;
        if t_first == NO_TWIN || t_last == NO_TWIN {
            return false;
        }
        let rails = [
            (h_first, work.he(h_first).prev), // side A, start: rail = twin(prev)
            (t_first, HalfEdgeId(u32::MAX)),  // side B, start: rail = next(t)
            (h_last, HalfEdgeId(u32::MAX)),   // side A, end: rail = next(h)
            (t_last, work.he(t_last).prev),   // side B, end: rail = twin(prev)
        ];
        for (k, &(ring_he, prev_he)) in rails.iter().enumerate() {
            let rail = match k {
                0 | 3 => {
                    let t = work.he(prev_he).twin;
                    if t == NO_TWIN {
                        return false;
                    }
                    t
                }
                _ => work.he(ring_he).next,
            };
            let rf = work.he(ring_he).face;
            if !clear_end_rail(work, rf, rail, width, eps) {
                cdbg!("[CONSUME]   end rail k={k} not clearable");
                return false;
            }
        }
    }
    true
}

/// Clear one open-chain end rail: while the first rail edge from the end vertex is shorter
/// than `width`, the vertex it reaches must be a valence-3 stub (rail back + rail on + one
/// end-face interior edge). Dissolve the interior edge (merging two end-face faces) and
/// collapse the stub, so the rail becomes a single edge the cut can land on. Returns false
/// (mesh possibly partially cleared but VALID) when a vertex is not a clean stub.
fn clear_end_rail(
    mesh: &mut HalfEdgeMesh,
    ring_face: crate::half_edge::FaceId,
    rail_start: HalfEdgeId,
    width: f32,
    eps: f32,
) -> bool {
    let mut rail = rail_start;
    for _round in 0..MAX_ROUNDS {
        if !mesh.is_edge_live(rail) {
            return false;
        }
        let a = mesh.vert(mesh.he_source(rail)).position;
        let w_v = mesh.he(rail).vertex;
        let b = mesh.vert(w_v).position;
        if (b - a).length() >= width - eps {
            return true; // rail long enough — the cut lands on it
        }
        // KEEP REPEATING THE FIRST SET (owner 2026-08-08): a stub carries the back rail,
        // the rail continuation, and any number of leftovers — the end-face interior edge
        // AND, from carve round 2+, the surviving end segment of a ring the pentagon face
        // kept out of the dissolve. Keep the continuation (the fan edge most parallel to
        // the rail), dissolve every other edge, collapse the stub — each pass leaves the
        // end column in exactly the state the interior columns reached, so the loop
        // repeats cleanly however many rows the fillet spans.
        let fan = mesh.vertex_fan(w_v);
        let back = mesh.he(rail).twin;
        let rail_dir = (b - a).normalize_or_zero();
        if rail_dir.length_squared() < 0.5 {
            return false;
        }
        let mut cont: Option<(f32, HalfEdgeId)> = None;
        for &g in &fan {
            if g == back {
                continue;
            }
            let d = (mesh.vert(mesh.he(g).vertex).position - b).normalize_or_zero();
            let score = d.dot(rail_dir);
            if cont.map_or(true, |(sc, _)| score > sc) {
                cont = Some((score, g));
            }
        }
        let Some((cont_score, cont_he)) = cont else { return false };
        if cont_score < 0.5 {
            cdbg!("[CONSUME]   clear_end_rail: stub {} has no straight continuation", w_v.0);
            return false; // rail bends away — not a clean grid rail
        }
        // STUB TEST: every extra edge must either border the ring face (a leftover ring
        // segment from the dissolve) or be the SINGLE end-face interior edge. More than
        // one non-ring extra means this vertex is PRISTINE lattice the carve has not
        // reached — STOP (harmlessly; the next carve round gets there). Eating pristine
        // rows here was the 2026-08-08 regression.
        let extras: Vec<HalfEdgeId> = fan
            .iter()
            .copied()
            .filter(|&g| g != back && g != cont_he && mesh.is_edge_live(g))
            .collect();
        let non_ring = extras
            .iter()
            .filter(|&&g| {
                let gf = mesh.he(g).face;
                let gt = mesh.he(g).twin;
                let gtf = if gt == NO_TWIN { NO_FACE } else { mesh.he(gt).face };
                gf != ring_face && gtf != ring_face
            })
            .count();
        if non_ring > 1 {
            return true; // pristine vertex — stop here, carve continues next round
        }
        for &g in &extras {
            if !mesh.is_edge_live(g) {
                continue;
            }
            let far = mesh.he(g).vertex;
            if mesh.dissolve_edge(g).is_none() {
                cdbg!("[CONSUME]   clear_end_rail: stub {} edge refused dissolve", w_v.0);
                return false;
            }
            // The dissolved edge's FAR endpoint can drop to valence 2 (e.g. the leftover
            // ring segment's other corner on the neighbouring rail) — collapse it too, or
            // it re-pentagons the neighbouring column's face.
            if mesh.is_vert_live(far) && mesh.vertex_fan(far).len() == 2 {
                let _ = mesh.join_edge_kill_vert(far);
            }
        }
        if !mesh.join_edge_kill_vert(w_v) {
            cdbg!("[CONSUME]   clear_end_rail: stub {} refused collapse", w_v.0);
            return false;
        }
        // `rail` survives the collapse retargeted to the next vertex (join keeps a→v).
    }
    false
}

// ============================================================================
// v3 — FUSE: the stripe IS the bevel (owner 2026-08-07, after Maya polyBevel /
// Blender "Offset Edge Loop Cut" + Offset Edges / MESHmachine Chamfer→Fuse research).
//
// Principle shared by all three references: the strip rim is placed at an EVEN
// distance from the edge ALONG the surface (a cut through existing topology), and
// the rounded surface is lofted BETWEEN the two rim loops — never offset from the
// original edge across whatever geometry happens to be there. Concretely:
//   1. dissolve every ring the fillet fully covers (closer than `width`);
//   2. CUT the stripe boundary at exactly `width` along each rail (new vertex on
//      the existing rail edge — an Offset Edge Loop Cut, nothing moves);
//   3. delete the sharp edge loop + stripe rows, loft `segments` profile rows
//      between the two boundary loops (control point = the original sharp edge).
// The boundary loops ARE the bevel rims: no clamp interaction, no leftover band,
// zero influence from consumed geometry.
//
// v3 scope gate (falls back to v2 absorb + offset bevel when not met): the
// selection forms CLOSED corner-free loops; each side of a loop is either a clean
// quad ring or one flat cap n-gon whose boundary is exactly the loop.
// ============================================================================

/// One side of a beveled loop after carving.
enum SidePlan {
    /// A ring of quads — one per rim segment. `outer[i]` is the ring vertex below rim
    /// vertex `i`; the stripe boundary is CUT at `width` along each rail rim→outer.
    Quad { ring_faces: Vec<crate::half_edge::FaceId>, outer: Vec<VertexId> },
    /// One flat n-gon whose boundary is exactly the rim loop (a cap). The boundary is an
    /// in-plane miter offset of the loop; the cap is re-traced on the offset loop.
    Cap { face: crate::half_edge::FaceId, offsets: Vec<Vec3> },
}

/// THE consume fillet profile — ONE definition shared by both consume paths.
///
/// Point at `t ∈ [0,1]` on the profile spanning `a` (side-A rim) → `b` (side-B rim), bulging
/// toward the original sharp rim vertex `rim` by `shape`: 0 = flat chord (chamfer), 0.5 =
/// through the corner (≈ circular fillet, Blender's default), 1 = convex bulge that reaches
/// the rim vertex itself.
///
/// The FUSE lofts it between its two boundary loops; the WEDGE uses it as the cutter's
/// cross-section, so the boolean carves the same surface instead of a flat chord. Both paths
/// therefore produce the same roundness for the same `(segments, shape)` — which is the whole
/// point of sharing it: the user cannot tell which consume path ran, so the two must not
/// disagree about the profile.
pub(crate) fn consume_profile_point(a: Vec3, b: Vec3, rim: Vec3, shape: f32, t: f32) -> Vec3 {
    let shape_f = (shape.clamp(0.0, 1.0) * 2.0).clamp(0.0, 2.0);
    let mid = (a + b) * 0.5;
    let ctrl = mid + (rim - mid) * shape_f;
    crate::bevel_op::quad_bezier(a, ctrl, b, t)
}

/// Full consume bevel: carve + cut + loft. Returns the FINISHED beveled mesh, or None when
/// the scope gate refuses — the caller then falls back to `absorb_for_bevel` + the ordinary
/// offset bevel (fail closed, never a broken surface).
pub fn consume_fuse_bevel(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &crate::bevel_op::BevelParams,
) -> Option<HalfEdgeMesh> {
    let width = params.width;
    if width <= 0.0 || selected.is_empty() {
        return None;
    }
    let segments = params.segments.clamp(1, 32) as usize;
    let eps = (width * 1e-4).max(1e-6);

    let mut work = mesh.clone();
    // 1. Ordered loops / chains FIRST (selection ids survive the carve — it only removes
    //    unselected ring/rail edges).
    let Some(mut loops) = ordered_loops(&work, selected) else {
        cdbg!("[CONSUME] fuse refused: selection has corners or leftovers");
        return None;
    };

    // 1b. CORNER TURNING (owner "L-chain / full edge ring", BEVEL.md §11 2026-08-08i):
    // consecutive loop half-edges sharing a side face turn a corner at their shared vertex.
    // Orient every loop so sharing lies on side A; per corner record the miter point
    // P = C + w·(d1+d2) (distance w from BOTH rim lines inside the shared face) and the
    // corner rail (the one unbeveled fan edge — v1 scope: valence-3 corners, ~90°,
    // lattice-aligned widths so P lands on an existing vertex; anything else refuses).
    struct Corner {
        col: usize,        // column index of the corner vertex in the loop
        c_pos: Vec3,       // corner vertex position
        p_pos: Vec3,       // miter point on the shared side
        d1: Vec3,          // unit dir from C back along run 1's rim
        d2: Vec3,          // unit dir from C forward along run 2's rim
        rail: HalfEdgeId,  // perpendicular unbeveled edge out of the corner vertex
        flank_a: HalfEdgeId, // the two rim half-edges flanking the corner (shared face)
        flank_b: HalfEdgeId,
    }
    let mut loop_corners: Vec<Vec<Corner>> = Vec::with_capacity(loops.len());
    for (hes, closed) in &mut loops {
        let m = hes.len();
        let pairs = if *closed { m } else { m.saturating_sub(1) };
        let mut share_a = false;
        let mut share_b = false;
        for i in 0..pairs {
            let j = (i + 1) % m;
            if work.he(hes[i]).face == work.he(hes[j]).face {
                share_a = true;
            }
            let (ti, tj) = (work.he(hes[i]).twin, work.he(hes[j]).twin);
            if ti != NO_TWIN && tj != NO_TWIN && work.he(ti).face == work.he(tj).face {
                share_b = true;
            }
        }
        if share_a && share_b {
            cdbg!("[CONSUME] fuse refused: corner shares faces on BOTH sides");
            return None;
        }
        if share_b {
            // Flip the loop so the shared side becomes side A.
            hes.reverse();
            for h in hes.iter_mut() {
                let t = work.he(*h).twin;
                if t == NO_TWIN {
                    return None;
                }
                *h = t;
            }
        }
        let mut cs: Vec<Corner> = Vec::new();
        // A loop whose EVERY half-edge shares one single face is a CAP (flat n-gon side),
        // not a chain of corners — the cap classification owns that case.
        let first_f = work.he(hes[0]).face;
        if *closed && hes.iter().all(|&h| work.he(h).face == first_f) {
            loop_corners.push(cs);
            continue;
        }
        for i in 0..pairs {
            let j = (i + 1) % m;
            if work.he(hes[i]).face != work.he(hes[j]).face {
                continue;
            }
            let vc = work.he(hes[i]).vertex; // dst(h_i) == src(h_j)
            let c = work.vert(vc).position;
            let d1 = (work.vert(work.he_source(hes[i])).position - c).normalize_or_zero();
            let d2 = (work.vert(work.he(hes[j]).vertex).position - c).normalize_or_zero();
            if d1.length_squared() < 0.5 || d2.length_squared() < 0.5 || d1.dot(d2).abs() > 0.3 {
                cdbg!("[CONSUME] fuse refused: corner at v{} not ~90 degrees", vc.0);
                return None;
            }
            // The corner rail: the single non-selected edge in the corner vertex's fan.
            let mut rail: Option<HalfEdgeId> = None;
            let mut ambiguous = false;
            for g in work.vertex_fan(vc) {
                let t = work.he(g).twin;
                let sel = selected.contains(&g.0) || (t != NO_TWIN && selected.contains(&t.0));
                if sel {
                    continue;
                }
                if rail.is_some() {
                    ambiguous = true;
                }
                rail = Some(g);
            }
            let (Some(rail), false) = (rail, ambiguous) else {
                cdbg!("[CONSUME] fuse refused: corner v{} is not a clean valence-3 corner", vc.0);
                return None;
            };
            cs.push(Corner {
                col: j,
                c_pos: c,
                p_pos: c + (d1 + d2) * width,
                d1,
                d2,
                rail,
                flank_a: hes[i],
                flank_b: hes[j],
            });
        }
        loop_corners.push(cs);
    }
    let has_corners = loop_corners.iter().any(|c| !c.is_empty());

    // 2. Carve: dissolve every ring the fillet covers (rows whose height stays < width),
    //    and after EVERY round clear the open-chain END RAILS (owner 2026-08-08: "keep
    //    repeating the first set"). A dissolved ring leaves stubs on the end faces that
    //    turn the end columns into pentagons; clearing them each round restores the same
    //    uniform state the interior columns are in, so the next round's dissolve covers
    //    the end columns again — however many rows the fillet spans.
    for round in 0..MAX_ROUNDS {
        let occs = blocking_occurrences(&work, selected, width, eps);
        if occs.is_empty() {
            break;
        }
        let keys: HashSet<u32> = occs.iter().map(|o| o.opposite_key).collect();
        let Some(vetted) = check_absorbable(&work, selected, &keys) else {
            break; // barrier — the cut_t gate below decides if we can still fuse
        };
        if !absorb_loop_edges(&mut work, &vetted) {
            return None;
        }
        // Clear the end rails AFTER the dissolve — only the stubs this round just created
        // (a pristine vertex halts the walk harmlessly; the next round reaches it). It
        // must NEVER run before a dissolve: that ate live lattice rows ahead of the carve
        // (owner regression 2026-08-08, "not doing the job").
        if !clear_open_chain_end_rails(&mut work, &loops, width, eps) {
            cdbg!("[CONSUME] fuse refused: cannot clear end rails (round {round})");
            return None;
        }
        // Corner rails: same treatment, once per flanking wall (the wall ring faces are
        // the stubs' "leftover ring segment" reference).
        for cs in &loop_corners {
            for c in cs {
                if !work.is_edge_live(c.rail) {
                    cdbg!("[CONSUME] fuse refused: corner rail died (round {round})");
                    return None;
                }
                for flank in [c.flank_a, c.flank_b] {
                    let t = work.he(flank).twin;
                    if t == NO_TWIN {
                        return None;
                    }
                    let rf = work.he(t).face;
                    if !clear_end_rail(&mut work, rf, c.rail, width, eps) {
                        cdbg!("[CONSUME] fuse refused: cannot clear corner rail (round {round})");
                        return None;
                    }
                }
            }
        }
        if work.validate().is_err() {
            return None;
        }
    }

    // 3. Per loop: classify both sides and gather boundary data.
    struct LoopPlan {
        rim: Vec<VertexId>, // cols entries (m for closed, m+1 for open)
        closed: bool,
        side_a: SidePlan,
        side_b: SidePlan,
        /// Per column: Some(corner index in `corners`) when the column lies within `width`
        /// of that corner (the fan stretch, the corner vertex itself, and the two adjacent
        /// kept columns whose rail cut coincides with P).
        corner_of: Vec<Option<usize>>,
        corners: Vec<Corner>,
    }
    let mut plans: Vec<LoopPlan> = Vec::new();
    let mut removed_faces: HashSet<u32> = HashSet::new();
    let mut rim_all: HashSet<u32> = HashSet::new();
    for (li, (hes, closed)) in loops.iter().enumerate() {
        let closed = *closed;
        let m = hes.len();
        if m < if closed { 3 } else { 1 } {
            return None;
        }
        let mut rim: Vec<VertexId> = hes.iter().map(|&h| work.he_source(h)).collect();
        if !closed {
            rim.push(work.he(*hes.last()?).vertex); // far endpoint
        }
        let twins: Vec<HalfEdgeId> = hes
            .iter()
            .map(|&h| work.he(h).twin)
            .collect();
        if twins.iter().any(|&t| t == NO_TWIN) {
            return None;
        }
        let corners = std::mem::take(&mut loop_corners[li]);
        let corner_quads: HashSet<u32> = corners
            .iter()
            .map(|c| work.he(c.flank_a).face.0)
            .collect();
        // Column → owning corner (within width of the corner vertex; the strictly-inside
        // stretch AND the boundary columns at exactly width, whose cut IS P).
        let corner_of: Vec<Option<usize>> = rim
            .iter()
            .map(|&v| {
                let p = work.vert(v).position;
                corners
                    .iter()
                    .position(|c| (p - c.c_pos).length() <= width + eps)
            })
            .collect();
        // Per-he corner influence: a he is inside the zone when BOTH endpoints' columns
        // are (the transition he from the last kept column into the stretch stays strict —
        // wait, no: a he with EITHER endpoint influenced borders the merged corner face).
        let corner_hes: Vec<bool> = (0..m)
            .map(|i| {
                let j = if closed { (i + 1) % m } else { i + 1 };
                corner_of[i].is_some() || corner_of.get(j).map_or(false, |o| o.is_some())
            })
            .collect();
        let no_hes: Vec<bool> = vec![false; m];
        // Side A from the loop's own half-edges; side B from their twins. For side A,
        // rim vertex i's ring neighbour is src(prev(h_i)); for side B it's dst(next(t_i)).
        let Some(side_a) =
            classify_side(&work, selected, hes, &rim, false, closed, width, &corner_hes)
        else {
            cdbg!("[CONSUME] fuse refused: side A not a clean quad ring / cap (loop len {})", m);
            return None;
        };
        let Some(side_b) =
            classify_side(&work, selected, &twins, &rim, true, closed, width, &no_hes)
        else {
            cdbg!("[CONSUME] fuse refused: side B not a clean quad ring / cap (loop len {})", m);
            return None;
        };
        // Track removed faces; overlap is out of scope EXCEPT the merged corner-region
        // face, which every corner-influenced half-edge legitimately references (claimed
        // once). Alignment: ring_faces[i] is he i's face.
        let _ = &corner_quads;
        for (sp, hes_mask) in [(&side_a, &corner_hes), (&side_b, &no_hes)] {
            match sp {
                SidePlan::Quad { ring_faces, .. } => {
                    for (i, f) in ring_faces.iter().enumerate() {
                        let relaxed = hes_mask.get(i).copied().unwrap_or(false);
                        if !removed_faces.insert(f.0) && !relaxed {
                            cdbg!("[CONSUME] fuse refused: ring face {} owned twice", f.0);
                            return None;
                        }
                    }
                }
                SidePlan::Cap { face, .. } => {
                    if !removed_faces.insert(face.0) {
                        return None;
                    }
                }
            }
        }
        for v in &rim {
            rim_all.insert(v.0);
        }
        plans.push(LoopPlan { rim, closed, side_a, side_b, corner_of, corners });
    }
    let _ = has_corners;
    // 3b. CORNER FLOOD: remove every face inside a corner's L-band (all vertices strictly
    // within `width` of EITHER rim line, capped near the corner). The fan bands re-skin
    // this region; faces touching the boundary lines stay (their verts sit AT width).
    for plan in &plans {
        for c in &plan.corners {
            let start = work.he(c.flank_a).face;
            if start == NO_FACE {
                return None;
            }
            let dist_line = |p: Vec3, d: Vec3| -> f32 {
                let r = p - c.c_pos;
                (r - d * r.dot(d)).length()
            };
            let inside = |p: Vec3| -> bool {
                (p - c.c_pos).length() <= width * 1.75 + eps
                    && (dist_line(p, c.d1) < width - eps || dist_line(p, c.d2) < width - eps)
            };
            let mut queue = vec![start];
            let mut seen_f: HashSet<u32> = HashSet::new();
            seen_f.insert(start.0);
            while let Some(f) = queue.pop() {
                let ring = work.face_vertices(f);
                if ring
                    .iter()
                    .any(|&v| !inside(work.vert(v).position))
                {
                    continue; // face reaches the boundary — stays (or is a run's own)
                }
                removed_faces.insert(f.0);
                if seen_f.len() > 256 {
                    cdbg!("[CONSUME] fuse refused: corner flood exceeded budget");
                    return None;
                }
                for h in work.face_half_edges(f) {
                    let t = work.he(h).twin;
                    if t == NO_TWIN {
                        continue;
                    }
                    let nf = work.he(t).face;
                    if nf != NO_FACE && seen_f.insert(nf.0) {
                        queue.push(nf);
                    }
                }
            }
        }
    }
    // Rim isolation: an INTERIOR rim vertex must be fully surrounded by removed faces —
    // else deleting it tears. An open chain's END vertex is allowed exactly ONE unremoved
    // incident face: the END FACE, which is retraced with the profile arc replacing the
    // vertex (the visible arc where the fillet dies into the perpendicular face).
    let mut end_faces: HashMap<u32, (usize, usize)> = HashMap::new(); // face → (plan, col)
    for (pi, plan) in plans.iter().enumerate() {
        let cols = plan.rim.len();
        for (ci, &v) in plan.rim.iter().enumerate() {
            let is_end = !plan.closed && (ci == 0 || ci == cols - 1);
            let mut extra: Vec<u32> = Vec::new();
            for h in work.vertex_fan(v) {
                let f = work.he(h).face;
                if f != NO_FACE && !removed_faces.contains(&f.0) {
                    extra.push(f.0);
                }
            }
            extra.sort_unstable();
            extra.dedup();
            if is_end {
                if extra.len() != 1 {
                    cdbg!("[CONSUME] fuse refused: chain end vertex {} borders {} unremoved faces (need exactly 1)", v.0, extra.len());
                    return None;
                }
                if end_faces.insert(extra[0], (pi, ci)).is_some() {
                    cdbg!("[CONSUME] fuse refused: end face {} shared by two chain ends", extra[0]);
                    return None;
                }
            } else if !extra.is_empty() {
                cdbg!("[CONSUME] fuse refused: rim vertex {} touches unremoved face {} (support edge?)", v.0, extra[0]);
                return None;
            }
        }
    }

    // 4. Rebuild: kept faces verbatim, then per loop the cut/offset boundaries, profile
    //    loft, residual rows and re-traced caps. Parallel material vec restored after
    //    from_polygons (same discipline as bevel_op).
    let mut positions: Vec<Vec3> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let mut mats: Vec<u32> = Vec::new();
    let mut vmap: HashMap<u32, usize> = HashMap::new();
    for i in 0..work.vertices.len() as u32 {
        let v = VertexId(i);
        if work.is_vert_live(v) && !rim_all.contains(&i) {
            vmap.insert(i, positions.len());
            positions.push(work.vert(v).position);
        }
    }
    for i in 0..work.faces.len() as u32 {
        let f = crate::half_edge::FaceId(i);
        if !work.is_face_live(f) || removed_faces.contains(&i) || end_faces.contains_key(&i) {
            continue;
        }
        let ring = work.face_vertices(f);
        let mut out = Vec::with_capacity(ring.len());
        for v in &ring {
            out.push(*vmap.get(&v.0)?); // an unmapped (rim) vid in a kept face = gate bug
        }
        faces.push(out);
        mats.push(work.faces[i as usize].material_id);
    }

    // Per-plan profile columns, saved for the end-face retrace below.
    let mut plan_columns: Vec<Vec<Vec<usize>>> = Vec::with_capacity(plans.len());
    for plan in &plans {
        let m = plan.rim.len(); // = columns (rim verts), m-1 bands when open, m when closed
        let rim_pos: Vec<Vec3> = plan.rim.iter().map(|&v| work.vert(v).position).collect();
        // Resolve each corner's miter point P to an EXISTING vertex (v1 scope: lattice-
        // aligned widths; P is the shared rail-end of the two adjacent kept columns).
        // Unaligned widths find no vertex → refuse (falls back to absorb+bevel).
        let mut p_idx: Vec<usize> = Vec::with_capacity(plan.corners.len());
        for c in &plan.corners {
            let mut found: Option<usize> = None;
            for i in 0..work.vertices.len() as u32 {
                let v = VertexId(i);
                if !work.is_vert_live(v) || rim_all.contains(&i) {
                    continue;
                }
                if (work.vert(v).position - c.p_pos).length() <= (width * 1e-3).max(eps) {
                    found = vmap.get(&i).copied();
                    break;
                }
            }
            let Some(fi) = found else {
                cdbg!("[CONSUME] fuse refused: no vertex at miter point ({:.3},{:.3},{:.3}) — width not lattice-aligned at the corner", c.p_pos.x, c.p_pos.y, c.p_pos.z);
                return None;
            };
            p_idx.push(fi);
        }
        // Boundary index loop per side; also emit residual rows for quad sides.
        // `use_corners` = side A only (the shared side): corner-influenced columns take P.
        let mut boundary = |side: &SidePlan,
                            positions: &mut Vec<Vec3>,
                            reuse_eps: f32,
                            use_corners: bool|
         -> Option<(Vec<usize>, Vec<Vec3>, u32)> {
            match side {
                SidePlan::Quad { ring_faces, outer } => {
                    let mat = work.faces[ring_faces[0].0 as usize].material_id;
                    let mut idx = Vec::with_capacity(m);
                    let mut pos = Vec::with_capacity(m);
                    for i in 0..m {
                        if use_corners {
                            if let Some(k) = plan.corner_of[i] {
                                idx.push(p_idx[k]);
                                pos.push(plan.corners[k].p_pos);
                                continue;
                            }
                        }
                        let a = work.vert(outer[i]).position;
                        let d = (a - rim_pos[i]).length();
                        if d < width - reuse_eps {
                            cdbg!("[CONSUME] fuse refused: rail {:.4} shorter than width (carve blocked)", d);
                            return None;
                        }
                        let t = (width / d).min(1.0);
                        if t > 0.999 {
                            idx.push(*vmap.get(&outer[i].0)?);
                            pos.push(a);
                        } else {
                            let p = rim_pos[i] + (a - rim_pos[i]) * t;
                            idx.push(positions.len());
                            positions.push(p);
                            pos.push(p);
                        }
                    }
                    Some((idx, pos, mat))
                }
                SidePlan::Cap { face, offsets } => {
                    let mat = work.faces[face.0 as usize].material_id;
                    let mut idx = Vec::with_capacity(m);
                    let mut pos = Vec::with_capacity(m);
                    for &p in offsets {
                        idx.push(positions.len());
                        positions.push(p);
                        pos.push(p);
                    }
                    Some((idx, pos, mat))
                }
            }
        };
        let (la_idx, la_pos, mat_a) = boundary(&plan.side_a, &mut positions, eps, true)?;
        let (lb_idx, lb_pos, mat_b) = boundary(&plan.side_b, &mut positions, eps, false)?;

        // Profile loft L0=side A .. Ls=side B, via THE shared consume profile
        // (`consume_profile_point`) — the same curve the wedge cutter carves.
        let mut loops_idx: Vec<Vec<usize>> = Vec::with_capacity(segments + 1);
        loops_idx.push(la_idx.clone());
        for k in 1..segments {
            let t = k as f32 / segments as f32;
            let mut lk = Vec::with_capacity(m);
            for i in 0..m {
                let p = consume_profile_point(la_pos[i], lb_pos[i], rim_pos[i], params.shape, t);
                lk.push(positions.len());
                positions.push(p);
            }
            loops_idx.push(lk);
        }
        loops_idx.push(lb_idx.clone());

        // Bands: (L_{k+1}[i], L_{k+1}[i+1], L_k[i+1], L_k[i]) — derived from the removed
        // ring faces' winding so every shared edge twins in reverse. Open chains have one
        // band fewer than columns (no wraparound).
        let bands = if plan.closed { m } else { m - 1 };
        for k in 0..segments {
            for i in 0..bands {
                let j = (i + 1) % m;
                // push_dedup: corner-fan segments repeat P on the A side — the k=0 band
                // row degenerates to triangles there (the Maya radial fan).
                push_dedup_face(
                    &mut faces,
                    &mut mats,
                    vec![
                        loops_idx[k + 1][i],
                        loops_idx[k + 1][j],
                        loops_idx[k][j],
                        loops_idx[k][i],
                    ],
                    // Nearest-side material: bands in the A half take side A's, the B half
                    // side B's (matches the offset-bevel fallback + Blender on two-material
                    // loops; middle band of an odd count ties to A).
                    if 2 * k < segments { mat_a } else { mat_b },
                );
            }
        }
        // Residual rows where the cut landed mid-rail (quad sides only). Corner-influenced
        // segments have no A-side residual: their surface is the corner flood, re-skinned
        // by the fan bands (their `outer` rails point into removed territory).
        if let SidePlan::Quad { outer, ring_faces } = &plan.side_a {
            let mat = work.faces[ring_faces[0].0 as usize].material_id;
            for i in 0..bands {
                let j = (i + 1) % m;
                if plan.corner_of[i].is_some() && plan.corner_of[j].is_some() {
                    continue; // fan interior — surface is the flood, re-skinned by bands
                }
                let ring = vec![la_idx[i], la_idx[j], *vmap.get(&outer[j].0)?, *vmap.get(&outer[i].0)?];
                push_dedup_face(&mut faces, &mut mats, ring, mat);
            }
        }
        if let SidePlan::Quad { outer, ring_faces } = &plan.side_b {
            let mat = work.faces[ring_faces[0].0 as usize].material_id;
            for i in 0..bands {
                let j = (i + 1) % m;
                let ring = vec![lb_idx[j], lb_idx[i], *vmap.get(&outer[i].0)?, *vmap.get(&outer[j].0)?];
                push_dedup_face(&mut faces, &mut mats, ring, mat);
            }
        }
        // Re-traced caps: the old cap ring with each rim vertex replaced by its offset —
        // same winding as the old face, so every twin direction is preserved.
        let mut retrace_cap = |side: &SidePlan, bnd: &[usize], faces: &mut Vec<Vec<usize>>, mats: &mut Vec<u32>| -> Option<()> {
            if let SidePlan::Cap { face, .. } = side {
                let mat = work.faces[face.0 as usize].material_id;
                let old_ring = work.face_vertices(*face);
                let by_vid: HashMap<u32, usize> = plan
                    .rim
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (v.0, i))
                    .collect();
                let mut ring = Vec::with_capacity(old_ring.len());
                for v in &old_ring {
                    ring.push(bnd[*by_vid.get(&v.0)?]);
                }
                faces.push(ring);
                mats.push(mat);
            }
            Some(())
        };
        retrace_cap(&plan.side_a, &la_idx, &mut faces, &mut mats)?;
        retrace_cap(&plan.side_b, &lb_idx, &mut faces, &mut mats)?;
        plan_columns.push(loops_idx);
    }

    // END FACES (open chains): retrace the perpendicular face with the profile ARC in
    // place of the sharp corner vertex — the visible curve where the fillet dies into
    // that face. The arc's ends lie ON the face's two edges bordering the rails, so the
    // retraced ring stays a simple polygon and every shared edge still twins.
    // Sorted — HashMap iteration order is random and this loop pushes faces (face-id
    // determinism across evaluations of identical input).
    let mut end_list: Vec<(u32, (usize, usize))> =
        end_faces.iter().map(|(&f, &pc)| (f, pc)).collect();
    end_list.sort_unstable_by_key(|&(f, _)| f);
    for (fid, (pi, ci)) in end_list {
        let plan = &plans[pi];
        let v_e = plan.rim[ci];
        let f = crate::half_edge::FaceId(fid);
        let old_ring = work.face_vertices(f);
        let k_pos = old_ring.iter().position(|&v| v == v_e)?;
        // Only the end vertex itself may be a rim vertex of this ring.
        if old_ring.iter().any(|v| *v != v_e && rim_all.contains(&v.0)) {
            cdbg!("[CONSUME] fuse refused: end face {} touches other rim vertices", fid);
            return None;
        }
        let prev_v = old_ring[(k_pos + old_ring.len() - 1) % old_ring.len()];
        let next_v = old_ring[(k_pos + 1) % old_ring.len()];
        let outer_at = |side: &SidePlan| -> Option<VertexId> {
            match side {
                SidePlan::Quad { outer, .. } => Some(outer[ci]),
                SidePlan::Cap { .. } => None,
            }
        };
        let oa = outer_at(&plan.side_a)?;
        let ob = outer_at(&plan.side_b)?;
        // Arc column at this end, a-side first (k = 0..=segments).
        let col: Vec<usize> = plan_columns[pi].iter().map(|l| l[ci]).collect();
        let seq: Vec<usize> = if prev_v == oa && next_v == ob {
            col
        } else if prev_v == ob && next_v == oa {
            col.into_iter().rev().collect()
        } else {
            cdbg!("[CONSUME] fuse refused: end face {} ring does not border the rails", fid);
            return None;
        };
        let mut ring: Vec<usize> = Vec::with_capacity(old_ring.len() + seq.len());
        for (idx, v) in old_ring.iter().enumerate() {
            if idx == k_pos {
                ring.extend_from_slice(&seq);
            } else {
                ring.push(*vmap.get(&v.0)?);
            }
        }
        push_dedup_face(&mut faces, &mut mats, ring, work.faces[fid as usize].material_id);
    }

    // Compact to referenced positions only: the corner flood strands its interior stub
    // vertices (kept in vmap but referenced by no surviving face) — from_polygons would
    // materialize them as orphans.
    let mut remap: Vec<Option<usize>> = vec![None; positions.len()];
    let mut compact: Vec<Vec3> = Vec::with_capacity(positions.len());
    let mut faces = faces;
    for ring in &mut faces {
        for v in ring.iter_mut() {
            let idx = *remap.get(*v)?;
            *v = match idx {
                Some(i) => i,
                None => {
                    let i = compact.len();
                    compact.push(positions[*v]);
                    remap[*v] = Some(i);
                    i
                }
            };
        }
    }
    let positions = compact;

    let out = HalfEdgeMesh::from_polygons(&positions, &faces);
    if out.faces.len() == mats.len() {
        let mut o = out;
        for (i, &mmat) in mats.iter().enumerate() {
            o.faces[i].material_id = mmat;
        }
        if !crate::bevel_op::bevel_result_installable(mesh, &o) {
            return None;
        }
        Some(o)
    } else {
        // from_polygons dropped a degenerate face — indices no longer line up with mats;
        // materials would smear across the mesh. Refuse.
        None
    }
}

/// Emit `ring` with consecutive duplicates removed; skip if fewer than 3 distinct corners.
fn push_dedup_face(faces: &mut Vec<Vec<usize>>, mats: &mut Vec<u32>, ring: Vec<usize>, mat: u32) {
    let mut out: Vec<usize> = Vec::with_capacity(ring.len());
    for &v in &ring {
        if out.last() != Some(&v) {
            out.push(v);
        }
    }
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    if out.len() >= 3 {
        faces.push(out);
        mats.push(mat);
    }
}

/// Classify one side of an oriented rim loop. `hes[i]` runs v_i→v_{i+1} for the A side and
/// v_{i+1}→v_i (the twins) for the B side; `outer_from_next` picks the matching ring-vertex
/// extraction (`dst(next)` on the twin side, `src(prev)` on the own side).
fn classify_side(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    hes: &[HalfEdgeId],
    rim: &[VertexId],
    outer_from_next: bool,
    closed: bool,
    width: f32,
    corner_hes: &[bool], // per-he: touches a corner's influence zone (side A relaxations)
) -> Option<SidePlan> {
    let is_selected = |he: HalfEdgeId| -> bool {
        if selected.contains(&he.0) {
            return true;
        }
        let t = mesh.he(he).twin;
        t != NO_TWIN && selected.contains(&t.0)
    };
    let m = hes.len();
    let first_face = mesh.he(hes[0]).face;
    if first_face == NO_FACE {
        return None;
    }
    let all_same_face = hes.iter().all(|&h| mesh.he(h).face == first_face);
    if closed && all_same_face && mesh.face_vertices(first_face).len() == m {
        // Cap: one flat n-gon whose boundary is exactly the rim loop. Boundary = in-plane
        // miter offset toward the interior, distance `width` from each rim edge — the same
        // placement the ordinary bevel uses on a flat face.
        let ring = mesh.face_vertices(first_face);
        let rim_set: HashSet<u32> = rim.iter().map(|v| v.0).collect();
        if ring.iter().any(|v| !rim_set.contains(&v.0)) {
            return None;
        }
        let pos: Vec<Vec3> = rim.iter().map(|&v| mesh.vert(v).position).collect();
        // Newell normal of the cap.
        let mut n = Vec3::ZERO;
        for i in 0..m {
            n += pos[i].cross(pos[(i + 1) % m]);
        }
        let n = n.normalize_or_zero();
        if n.length_squared() < 0.5 {
            return None;
        }
        let mut offsets = Vec::with_capacity(m);
        for i in 0..m {
            let e_prev = pos[i] - pos[(i + m - 1) % m];
            let e_next = pos[(i + 1) % m] - pos[i];
            // Inward perp = n × e, PERIOD. For winding-consistent loops this points into
            // the face at every vertex including reflex ones (on the B side both n and e
            // negate together, so the product stays inward). NEVER orient with a centroid
            // test — a concave cap puts the centroid outside some edge's inward half-plane
            // and flips the miter out of the face (project rule: inward offset =
            // normal×edge_dir, never centroid tests).
            let perp = |e: Vec3| -> Vec3 { n.cross(e).normalize_or_zero() };
            let (pp, pn) = (perp(e_prev), perp(e_next));
            let msum = (pp + pn).normalize_or_zero();
            if msum.length_squared() < 0.5 {
                return None; // degenerate corner (reversal) — out of scope
            }
            let cos_half = msum.dot(pn);
            if cos_half < 0.2 {
                // Needle corner (interior angle ≲ 23°): the miter point would land at
                // ≥5× width, and silently shortening it breaks the fuse's even-distance
                // invariant. Fail closed — absorb+bevel (which clamps properly) takes over.
                cdbg!("[CONSUME] fuse refused: cap needle corner (cos_half {cos_half:.3})");
                return None;
            }
            offsets.push(pos[i] + msum * (width / cos_half));
        }
        return Some(SidePlan::Cap { face: first_face, offsets });
    }
    // Quad ring: every segment face is a quad and its side edges are not beveled.
    // Boundary columns: m for a closed loop, m+1 for an open chain (the far end's outer
    // vertex comes from the last half-edge's OTHER extraction rule).
    let cols = if closed { m } else { m + 1 };
    let mut ring_faces = Vec::with_capacity(m);
    let mut outer = vec![VertexId(u32::MAX); cols];
    let mut seen: HashSet<u32> = HashSet::new();
    for (i, &h) in hes.iter().enumerate() {
        // CORNER RELAXATION (side A of an oriented corner loop only): a half-edge inside
        // a corner's influence zone references the merged corner region face — any polygon
        // (the carve grows it), shared across the whole fan stretch, its "next/prev" being
        // the other rim. Its outer extraction is unused (fan columns override to P).
        let at_corner = corner_hes.get(i).copied().unwrap_or(false);
        let f = mesh.he(h).face;
        if f == NO_FACE || (!at_corner && mesh.face_vertices(f).len() != 4) {
            if f != NO_FACE {
                cdbg!("[CONSUME]   side seg {i}: face {} has {} verts (need quad)", f.0, mesh.face_vertices(f).len());
                if std::env::var_os("AZRA_CONSUME_DEBUG").is_some() {
                    for v in mesh.face_vertices(f) {
                        let p = mesh.vert(v).position;
                        eprintln!("[CONSUME]     ring v{} ({:.3},{:.3},{:.3})", v.0, p.x, p.y, p.z);
                    }
                }
            }
            return None;
        }
        if !seen.insert(f.0) && !at_corner {
            cdbg!("[CONSUME]   side seg {i}: face {} repeated along ring", f.0);
            return None; // a face repeated along the ring — not a clean quad strip
        }
        if !at_corner && (is_selected(mesh.he(h).next) || is_selected(mesh.he(h).prev)) {
            return None;
        }
        ring_faces.push(f);
        // Per half-edge, the two extraction rules name the outer ring vertices under the
        // chain's OWN index i and i+1 (side A ring (v_i, v_{i+1}, a_{i+1}, a_i); the twin
        // side ring (v_{i+1}, v_i, b_i, b_{i+1})).
        let (o_here, o_next) = if outer_from_next {
            (mesh.he(mesh.he(h).next).vertex, mesh.he_source(mesh.he(h).prev))
        } else {
            (mesh.he_source(mesh.he(h).prev), mesh.he(mesh.he(h).next).vertex)
        };
        if !mesh.is_vert_live(o_here) || !mesh.is_vert_live(o_next) {
            return None;
        }
        outer[i] = o_here;
        if !closed && i + 1 < cols {
            outer[i + 1] = o_next;
        }
    }
    Some(SidePlan::Quad { ring_faces, outer })
}

/// Order the selected edges into directed CLOSED LOOPS and OPEN CHAINS (bool = closed).
/// Every selected vertex must carry exactly one or two selected edges — a vertex with 3+
/// (a corner) refuses, as do leftovers. This is the fuse scope gate.
fn ordered_loops(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
) -> Option<Vec<(Vec<HalfEdgeId>, bool)>> {
    // Canonical undirected set of live selected edges.
    let mut undirected: HashSet<u32> = HashSet::new();
    for &e in selected {
        let he = HalfEdgeId(e);
        if (e as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(he) {
            continue;
        }
        let t = mesh.he(he).twin;
        if t == NO_TWIN {
            return None; // boundary rim edge — cannot fuse
        }
        undirected.insert(e.min(t.0));
    }
    if undirected.is_empty() {
        return None;
    }
    // vid → outgoing directed selected half-edges.
    let mut out: HashMap<u32, Vec<HalfEdgeId>> = HashMap::new();
    for &e in &undirected {
        let he = HalfEdgeId(e);
        let t = mesh.he(he).twin;
        out.entry(mesh.he_source(he).0).or_default().push(he);
        out.entry(mesh.he(he).vertex.0).or_default().push(t);
    }
    if out.values().any(|v| v.len() > 2) {
        return None; // a corner (3+ beveled edges at a vertex)
    }
    let canon = |h: HalfEdgeId| -> u32 {
        let t = mesh.he(h).twin;
        h.0.min(t.0)
    };
    let mut visited: HashSet<u32> = HashSet::new();
    let mut loops = Vec::new();
    // OPEN CHAINS first: walk from each degree-1 endpoint. Sorted — HashMap iteration
    // order is random per instance, and chain order decides output vertex/face id
    // assignment (id churn across evaluations of identical input = per-frame GPU
    // re-upload + broken id-keyed downstream data).
    let mut endpoints: Vec<u32> = out
        .iter()
        .filter(|(_, hs)| hs.len() == 1)
        .map(|(&v, _)| v)
        .collect();
    endpoints.sort_unstable();
    for &vstart in &endpoints {
        let start = out[&vstart][0];
        if visited.contains(&canon(start)) {
            continue; // the other end of an already-walked chain
        }
        let mut chain = Vec::new();
        let mut cur = start;
        for _guard in 0..undirected.len() + 1 {
            visited.insert(canon(cur));
            chain.push(cur);
            let v = mesh.he(cur).vertex;
            let twin_cur = mesh.he(cur).twin;
            let nexts = out.get(&v.0)?;
            let next = nexts.iter().copied().find(|&h| h != twin_cur);
            let Some(next) = next else { break }; // reached the far endpoint
            cur = next;
        }
        loops.push((chain, false));
    }
    // CLOSED CYCLES from whatever remains. Sorted seeds — same determinism rule as the
    // endpoints above (HashSet order is random; the seed picks column 0 of the loft).
    let mut cycle_seeds: Vec<u32> = undirected.iter().copied().collect();
    cycle_seeds.sort_unstable();
    for &start_e in &cycle_seeds {
        if visited.contains(&start_e) {
            continue;
        }
        let start = HalfEdgeId(start_e);
        let mut cycle = Vec::new();
        let mut cur = start;
        for _guard in 0..undirected.len() + 1 {
            visited.insert(canon(cur));
            cycle.push(cur);
            let v = mesh.he(cur).vertex;
            let twin_cur = mesh.he(cur).twin;
            let nexts = out.get(&v.0)?;
            let next = if nexts[0] == twin_cur { nexts[1] } else { nexts[0] };
            if next == start {
                break;
            }
            cur = next;
        }
        // Closed check: the last edge must feed back into the start.
        let last = *cycle.last()?;
        let v_end = mesh.he(last).vertex;
        if v_end != mesh.he_source(start) {
            return None;
        }
        loops.push((cycle, true));
    }
    if visited.len() != undirected.len() {
        return None;
    }
    Some(loops)
}

// ============================================================================
// v4 — WEDGE CUT: consume by BOOLEAN (owner EAT.fl4 2026-08-08: "vertices do not get
// consumed — first step is just a 45 strip we can make large; I boolean a cylinder in
// a sphere").
//
// On a boolean rim, the sides are NOT clean quad rings — they are retraced n-gons,
// support edges, micro-edges. No topology-walking carve survives that (the fuse gate
// refuses, the plain bevel's clamp is crushed by the shortest stray edge). But the rim
// CAME from a boolean — so the chamfer is built the same way: sweep a wedge along the rim
// (apex pushed outside the corner, the two base corners at `width` along each side surface)
// and SUBTRACT it. The boolean consumes every vertex, edge and face in the wedge's path,
// whatever the topology, and the wedge's inner band becomes the chamfer strip.
//
// SEGMENTS / ROUNDNESS (2026-08-09, owner: "geometry consume does not support segment and
// roundness"): the inner band is no longer a single flat chord. The cross-section between
// the two base corners follows `consume_profile_point` — the SAME profile the fuse lofts —
// sampled at `segments` bands, so the subtracted region shrinks toward the corner and the
// boolean leaves a rounded fillet. segments=1 reproduces the historical triangular wedge
// exactly (one band, chord = the flat 45-class strip).
// ============================================================================

/// Consume-by-boolean: sweep a wedge along each closed selected loop and subtract it.
/// Returns the finished chamfered mesh, or None when out of scope / any gate refuses.
///
/// Takes the full `BevelParams` (not just width) so `segments`/`shape` reach the sweep's
/// cross-section — a wedge built from `width` alone can only ever cut a flat chamfer.
pub fn consume_wedge_cut(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &crate::bevel_op::BevelParams,
) -> Option<HalfEdgeMesh> {
    let width = params.width;
    let segments = params.segments.clamp(1, 32) as usize;
    if width <= 0.0 || selected.is_empty() {
        return None;
    }
    // Strict closed loops when the selection is clean; else interpret a ragged boolean-rim
    // selection the way loop-select reads it: longest-edge seed, straightest-continuation
    // walk (junk micro-clusters hang off the walked cycle and are simply not part of the
    // sweep — the wedge consumes them geometrically anyway).
    let loops: Vec<(Vec<HalfEdgeId>, bool)> = match ordered_loops(mesh, selected) {
        Some(l) => l,
        None => {
            // Ragged boolean-rim selection: tolerant straightest-walk (closed cycles only).
            let l = walk_loops(mesh, selected);
            if l.is_empty() {
                cdbg!("[CONSUME] wedge refused: no closed rim walk in selection");
                return None;
            }
            l.into_iter().map(|hes| (hes, true)).collect()
        }
    };
    let mut work = mesh.clone();
    for (hes, closed) in &loops {
        let cutter = build_rim_wedge(mesh, hes, width, *closed, segments, params.shape)?;
        let out = crate::mesh_boolean::mesh_boolean(
            &work,
            &cutter,
            crate::mesh_boolean::BoolOp::Difference,
        )?;
        cdbg!("[CONSUME] wedge boolean: {}v/{}f -> {}v/{}f (cutter {}v)",
            work.vertex_count(), work.face_count(), out.vertex_count(), out.face_count(),
            cutter.vertex_count());
        if std::env::var_os("AZRA_CONSUME_DEBUG").is_some() {
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            for i in 0..out.vertices.len() as u32 {
                let v = VertexId(i);
                if !out.is_vert_live(v) { continue; }
                let p = out.vert(v).position;
                lo = lo.min(p); hi = hi.max(p);
            }
            eprintln!("[CONSUME]   result bbox ({:.2},{:.2},{:.2})..({:.2},{:.2},{:.2})", lo.x, lo.y, lo.z, hi.x, hi.y, hi.z);
            let ib = crate::mesh_boolean::mesh_boolean(&work, &cutter, crate::mesh_boolean::BoolOp::Intersect);
            if let Some(ib) = ib { eprintln!("[CONSUME]   intersect would be {}v/{}f", ib.vertex_count(), ib.face_count()); }
        }
        if out.validate().is_err() {
            cdbg!("[CONSUME] wedge refused: boolean result invalid");
            return None;
        }
        work = out;
    }
    if !crate::bevel_op::bevel_result_installable(mesh, &work) {
        cdbg!("[CONSUME] wedge refused: result not installable (non-manifold / new hole)");
        return None;
    }
    // LOCALITY GATE (owner 2026-08-08: "this needs to be localised — breaks all geo,
    // removes all edges"): the boolean kernel MERGES COPLANAR faces, so on a subdivided
    // flat mesh a wedge cut can collapse the entire lattice into a handful of n-gons far
    // beyond the strip. A chamfer only ever ADDS detail near the rim — losing half the
    // vertices means the kernel rewrote unrelated geometry. Refuse; the absorb/slide path
    // (which is strictly local) takes over.
    if work.vertex_count() * 2 < mesh.vertex_count() {
        cdbg!(
            "[CONSUME] wedge refused: non-local result ({} of {} verts survive)",
            work.vertex_count(),
            mesh.vertex_count()
        );
        return None;
    }
    Some(work)
}

/// Tolerant rim extraction for ragged (boolean-rim) selections: seed each walk on the
/// LONGEST unvisited selected edge, continue on the STRAIGHTEST selected edge (max direction
/// dot) at each vertex, accept walks that close into cycles of ≥3. Selected edges that no
/// walk uses (micro-cluster junk) are ignored — the wedge sweep consumes their geometry
/// anyway. Directed half-edges returned run around each cycle like `ordered_loops`'.
fn walk_loops(mesh: &HalfEdgeMesh, selected: &HashSet<u32>) -> Vec<Vec<HalfEdgeId>> {
    let live: Vec<u32> = selected
        .iter()
        .copied()
        .filter(|&e| (e as usize) < mesh.half_edges.len() && mesh.is_edge_live(HalfEdgeId(e)))
        .collect();
    let canon = |e: u32| -> u32 {
        let t = mesh.he(HalfEdgeId(e)).twin;
        if t == NO_TWIN { e } else { e.min(t.0) }
    };
    let in_sel = |key: u32| -> bool {
        let he = HalfEdgeId(key);
        let t = mesh.he(he).twin;
        selected.contains(&key) || (t != NO_TWIN && selected.contains(&t.0))
    };
    let elen = |e: u32| -> f32 {
        let he = HalfEdgeId(e);
        (mesh.vert(mesh.he(he).vertex).position - mesh.vert(mesh.he_source(he)).position).length()
    };
    let mut seeds = live.clone();
    // Tie-break equal lengths by edge id: `live` comes from HashSet iteration (random
    // order), and equal-length edges are ubiquitous on lattices/symmetric rims — without
    // the tie-break the walk seeds (hence the accepted cycle set) vary run to run.
    seeds.sort_by(|&a, &b| {
        elen(b)
            .partial_cmp(&elen(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let mut used: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    for &seed in &seeds {
        if used.contains(&canon(seed)) {
            continue;
        }
        let start = HalfEdgeId(seed);
        let start_v = mesh.he_source(start);
        let mut cycle = vec![start];
        let mut cur_v = mesh.he(start).vertex;
        let mut cur_dir =
            (mesh.vert(cur_v).position - mesh.vert(start_v).position).normalize_or_zero();
        let mut prev_key = canon(seed);
        let mut closed = false;
        for _step in 0..live.len() * 2 + 4 {
            if cur_v == start_v {
                closed = true;
                break;
            }
            let mut best: Option<(f32, HalfEdgeId)> = None;
            for h in mesh.vertex_fan(cur_v) {
                let key = canon(h.0);
                if key == prev_key || !in_sel(key) || used.contains(&key) {
                    continue;
                }
                let d = (mesh.vert(mesh.he(h).vertex).position - mesh.vert(cur_v).position)
                    .normalize_or_zero();
                let straight = d.dot(cur_dir);
                if best.map_or(true, |(bs, _)| straight > bs) {
                    best = Some((straight, h));
                }
            }
            let Some((_, h)) = best else { break };
            prev_key = canon(h.0);
            cur_dir = (mesh.vert(mesh.he(h).vertex).position - mesh.vert(cur_v).position)
                .normalize_or_zero();
            cur_v = mesh.he(h).vertex;
            cycle.push(h);
        }
        if closed && cycle.len() >= 3 {
            for h in &cycle {
                used.insert(canon(h.0));
            }
            out.push(cycle);
        }
    }
    out
}

/// Build the closed sweep solid for one rim loop: cross-section at rim vertex i =
/// (apex_i just outside the corner, a_i at `width` along side A's surface, the `segments`−1
/// interior profile points, b_i at `width` along side B's surface). The a→b run is the future
/// fillet surface — a flat chord at `segments`=1, the `shape`-bulged profile above that; the
/// apex bands stay outside the mesh.
fn build_rim_wedge(
    mesh: &HalfEdgeMesh,
    hes: &[HalfEdgeId],
    width: f32,
    closed: bool,
    segments: usize,
    shape: f32,
) -> Option<HalfEdgeMesh> {
    let m = hes.len();
    if m < if closed { 3 } else { 1 } {
        return None;
    }
    let mut rim: Vec<VertexId> = hes.iter().map(|&h| mesh.he_source(h)).collect();
    if !closed {
        rim.push(mesh.he(*hes.last()?).vertex);
    }
    let p: Vec<Vec3> = rim.iter().map(|&v| mesh.vert(v).position).collect();

    // RESAMPLE while building: a boolean rim carries micro-edge zigzag clusters (owner
    // EAT.fl4: 0.008–0.045 edges at wild dihedrals where the cut ran near tangency). The
    // wedge is pure geometry — it only needs to CONTAIN the corner — so points that sit
    // closer than a fraction of the width to the last kept point, or whose local frame is
    // degenerate (spike normals), are simply skipped: the sweep bridges over them and the
    // boolean consumes the whole junk cluster.
    let min_step = width * 0.25;
    let mut a_pts = Vec::with_capacity(m);
    let mut b_pts = Vec::with_capacity(m);
    let mut x_pts = Vec::with_capacity(m);
    // Per-section profile apex: the rim vertex, LIFTED off the surface by the same hair as
    // a/b below. At shape=1 the profile passes exactly through its apex, so an unlifted rim
    // point would make the cutter TANGENT to the original surface along the whole rim — the
    // boolean degeneracy the a/b lift already exists to avoid. Lifted, it crosses cleanly.
    let mut r_pts = Vec::with_capacity(m);
    let mut last_kept: Option<Vec3> = None;
    let sections = p.len(); // m for closed, m+1 for open
    for i in 0..sections {
        let is_end = !closed && (i == 0 || i == sections - 1);
        if let Some(lp) = last_kept {
            if !is_end && (p[i] - lp).length() < min_step {
                continue; // micro cluster — bridge over it
            }
        }
        // Adjacent rim half-edges (clamped at open ends — the end section reuses its only
        // neighbour's frame).
        let h_prev = if closed { hes[(i + m - 1) % m] } else { hes[i.saturating_sub(1).min(m - 1)] };
        let h_cur = if closed { hes[i % m] } else { hes[i.min(m - 1)] };
        let edge_dir = if closed {
            (p[(i + 1) % m] - p[(i + m - 1) % m]).normalize_or_zero()
        } else {
            (p[(i + 1).min(sections - 1)] - p[i.saturating_sub(1)]).normalize_or_zero()
        };
        if edge_dir.length_squared() < 0.5 {
            continue;
        }
        // Side frame from the faces' OWN winding: a face's interior lies to the LEFT of
        // its directed half-edge (n × he_dir). Purely local — face centroids are unusable
        // here (an annulus face around a hole has its centroid across the void, which
        // flipped the offset INTO the hole and made the cutter miss the mesh entirely).
        let mut side = |twin: bool| -> Option<(Vec3, Vec3)> {
            let mut n_sum = Vec3::ZERO;
            let mut into_sum = Vec3::ZERO;
            for h in [h_prev, h_cur] {
                let hh = if twin {
                    let t = mesh.he(h).twin;
                    if t == NO_TWIN {
                        return None;
                    }
                    t
                } else {
                    h
                };
                let f = mesh.he(hh).face;
                if f == NO_FACE {
                    return None;
                }
                let n = mesh.compute_face_normal(f);
                let hd = (mesh.vert(mesh.he(hh).vertex).position
                    - mesh.vert(mesh.he_source(hh)).position)
                    .normalize_or_zero();
                n_sum += n;
                into_sum += n.cross(hd);
            }
            let n = n_sum.normalize_or_zero();
            let into = into_sum.normalize_or_zero();
            if n.length_squared() < 0.5 || into.length_squared() < 0.5 {
                return None; // opposing frames — degenerate corner
            }
            Some((n, into))
        };
        let (Some((na, da)), Some((nb, db))) = (side(false), side(true)) else {
            continue; // spike frame — resample over it
        };
        let outward = (na + nb).normalize_or_zero();
        if outward.length_squared() < 0.5 {
            continue;
        }
        // Lift a/b a hair OFF the surface along the side normals: an exactly-coplanar
        // apex band is a classic boolean degeneracy (open-chain sweep collapsed the whole
        // mesh to the sliver, 2026-08-08). 2% of width — invisible, decisive.
        let lift = width * 0.02;
        a_pts.push(p[i] + da * width + na * lift);
        b_pts.push(p[i] + db * width + nb * lift);
        r_pts.push(p[i] + outward * lift);
        // Apex clear of the corner: past the surface by 1.5×width so the two apex bands
        // never graze the original surface tangentially (boolean robustness).
        x_pts.push(p[i] + outward * (width * 1.5));
        last_kept = Some(p[i]);
    }
    let mut s_count = x_pts.len();
    if s_count < if closed { 3 } else { 2 } {
        cdbg!("[CONSUME] wedge refused: fewer than {} usable rim points after resample",
            if closed { 3 } else { 2 });
        return None;
    }
    if !closed {
        // Extend the sweep PAST both chain ends so the boolean carves the profile cleanly
        // through the perpendicular end faces (the visible arc where the fillet dies).
        let ext = width * 1.2;
        let d0 = (p[0] - p[1]).normalize_or_zero();
        let dn = (p[sections - 1] - p[sections - 2]).normalize_or_zero();
        if d0.length_squared() < 0.5 || dn.length_squared() < 0.5 {
            cdbg!("[CONSUME] wedge refused: degenerate chain end direction");
            return None;
        }
        for arr in [&mut x_pts, &mut a_pts, &mut b_pts, &mut r_pts] {
            let first = arr[0] + d0 * ext;
            let last = *arr.last()? + dn * ext;
            arr.insert(0, first);
            arr.push(last);
        }
        s_count += 2;
    }

    // Sweep rings: X (apex, outside the corner) then the PROFILE columns P_0 = A … P_segs = B.
    // Bands: apex→A, `segments` profile bands, B→apex (+ 2 end caps when open). At segments=1
    // this is EXACTLY the historical triangular wedge (one A→B chord band); above it the
    // profile bulges toward the rim, so the subtracted region shrinks and the cut is rounded.
    // Winding fixed afterwards by signed volume so the boolean sees an outward solid.
    let m = s_count;
    let mut positions: Vec<Vec3> = Vec::with_capacity((segments + 2) * m);
    positions.extend_from_slice(&x_pts);
    positions.extend_from_slice(&a_pts);
    for k in 1..segments {
        let t = k as f32 / segments as f32;
        for i in 0..m {
            positions.push(consume_profile_point(a_pts[i], b_pts[i], r_pts[i], shape, t));
        }
    }
    positions.extend_from_slice(&b_pts);
    let xo = 0usize;
    // Profile ring k (0 = A … segments = B) starts at this index.
    let ring = |k: usize| m + k * m;
    let mut faces: Vec<Vec<usize>> = Vec::with_capacity((segments + 2) * m + 2);
    let bands = if closed { m } else { m - 1 };
    for i in 0..bands {
        let j = (i + 1) % m;
        faces.push(vec![xo + i, xo + j, ring(0) + j, ring(0) + i]); // apex → A band
        for k in 0..segments {
            let (lo, hi) = (ring(k), ring(k + 1));
            faces.push(vec![lo + i, lo + j, hi + j, hi + i]); // profile band k
        }
        let bo = ring(segments);
        faces.push(vec![bo + i, bo + j, xo + j, xo + i]); // B → apex band
    }
    if !closed {
        // Caps = the whole cross-section polygon (apex, A, profile…, B), twinning every band's
        // end edge. Mirrored at the far end so both caps wind consistently with the bands.
        let mut near = Vec::with_capacity(segments + 2);
        near.push(xo);
        near.extend((0..=segments).map(ring));
        faces.push(near);
        let mut far = Vec::with_capacity(segments + 2);
        far.push(xo + m - 1);
        far.extend((0..=segments).rev().map(|k| ring(k) + m - 1));
        faces.push(far);
    }
    // Signed volume (sum of tetrahedra to the origin); flip if inward.
    let mut vol6 = 0.0f32;
    for f in &faces {
        for k in 1..f.len() - 1 {
            let (a, b, c) = (positions[f[0]], positions[f[k]], positions[f[k + 1]]);
            vol6 += a.dot(b.cross(c));
        }
    }
    cdbg!("[CONSUME] wedge cutter: sections={} vol6={:.6} (flip={})", m, vol6, vol6 < 0.0);
    if vol6 < 0.0 {
        for f in &mut faces {
            f.reverse();
        }
    }
    let cutter = HalfEdgeMesh::from_polygons(&positions, &faces);
    if cutter.validate().is_err() || cutter.is_manifold().is_err() {
        cdbg!("[CONSUME] wedge refused: cutter degenerate (self-folded sweep?)");
        return None;
    }
    Some(cutter)
}

/// Live quad faces (test helper).
#[cfg(test)]
fn quad_count(mesh: &HalfEdgeMesh) -> usize {
    (0..mesh.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| mesh.is_face_live(f) && mesh.face_vertices(f).len() == 4)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bevel_op::{bevel_edges_offset, BevelParams};

    /// A closed box subdivided into a 1×N column of quad rows per side face — the minimal
    /// "bowl rim" analogue: bevel the top rim; rows of height 2/n block a wide bevel.
    fn ring_column_box_at(ys: &[f32]) -> HalfEdgeMesh {
        let mut positions: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        let mut rings: Vec<[usize; 4]> = Vec::new();
        for &y in ys {
            let base = positions.len();
            positions.extend_from_slice(&[
                Vec3::new(-1.0, y, -1.0),
                Vec3::new(1.0, y, -1.0),
                Vec3::new(1.0, y, 1.0),
                Vec3::new(-1.0, y, 1.0),
            ]);
            rings.push([base, base + 1, base + 2, base + 3]);
        }
        for r in 0..rings.len() - 1 {
            let (lo, hi) = (rings[r], rings[r + 1]);
            for k in 0..4 {
                let k2 = (k + 1) % 4;
                faces.push(vec![lo[k], lo[k2], hi[k2], hi[k]]);
            }
        }
        let b = rings[0];
        faces.push(vec![b[3], b[2], b[1], b[0]]);
        let t = rings[rings.len() - 1];
        faces.push(vec![t[0], t[1], t[2], t[3]]);
        HalfEdgeMesh::from_polygons(&positions, &faces)
    }

    fn ring_column_box(rows: u32) -> HalfEdgeMesh {
        let ys: Vec<f32> = (0..=rows).map(|r| -1.0 + 2.0 * (r as f32) / (rows as f32)).collect();
        ring_column_box_at(&ys)
    }

    fn top_rim_selection(mesh: &HalfEdgeMesh) -> HashSet<u32> {
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let ya = mesh.vert(mesh.he_source(he)).position.y;
            let yb = mesh.vert(mesh.he(he).vertex).position.y;
            if (ya - 1.0).abs() < 1e-4 && (yb - 1.0).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 4, "box rim = 4 undirected edges");
        sel
    }

    /// Vertices whose y sits within tol of `y`.
    fn verts_at_y(mesh: &HalfEdgeMesh, y: f32, tol: f32) -> usize {
        (0..mesh.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| mesh.is_vert_live(v) && (mesh.vert(v).position.y - y).abs() < tol)
            .count()
    }

    /// Narrow bevel fits inside the first row → nothing to carve → None.
    #[test]
    fn no_absorb_when_width_fits() {
        let mesh = ring_column_box(4); // rows of height 0.5 → stripe fits for w ≤ 0.25
        let sel = top_rim_selection(&mesh);
        assert!(absorb_for_bevel(&mesh, &sel, 0.2).is_none());
    }

    /// THE STRIPE RULE: an overshooting ring is SLID to exactly 2×width, never dissolved.
    /// rows=4 (height 0.5), w=0.4 → stripe 0.8: dissolving would merge to 1.0 (> 0.8), so
    /// the ring below the rim slides from y=0.5 to y = 1 − 0.8 = 0.2. Topology unchanged.
    #[test]
    fn overshooting_ring_slides_to_stripe_boundary() {
        let mesh = ring_column_box(4);
        let sel = top_rim_selection(&mesh);
        let before_quads = quad_count(&mesh);
        let out = absorb_for_bevel(&mesh, &sel, 0.4).expect("0.8 stripe > 0.5 row → carve");
        out.validate().expect("carved mesh valid");
        assert_eq!(quad_count(&out), before_quads, "slide keeps topology");
        assert_eq!(verts_at_y(&out, 0.2, 1e-4), 4, "boundary loop slid to y=0.2 (stripe=0.8)");
        assert_eq!(verts_at_y(&out, 0.5, 1e-4), 0, "no vertex left at the old ring height");
        for &e in &sel {
            assert!(out.is_edge_live(HalfEdgeId(e)), "rim edge {e} must survive");
        }
    }

    /// Deep stripe: rings fully inside 2×width are dissolved, the last one slides.
    /// rows=8 (height 0.25), w=0.6 → stripe 1.2: rows merge 0.25→0.5→0.75→1.0 (3 rings
    /// eaten), the next ring (at y=−0.25) would overshoot to 1.25 → slides to y = 1−1.2.
    #[test]
    fn absorbs_full_rings_then_slides_the_boundary() {
        let mesh = ring_column_box(8);
        let sel = top_rim_selection(&mesh);
        let before_quads = quad_count(&mesh);
        let out = absorb_for_bevel(&mesh, &sel, 0.6).expect("carves");
        out.validate().expect("valid");
        assert_eq!(quad_count(&out), before_quads - 12, "3 rings of 4 quads dissolved");
        assert_eq!(verts_at_y(&out, -0.2, 1e-3), 4, "boundary slid to y=1−1.2=−0.2");
    }

    /// THE ORACLE: carve-then-bevel == bevel on a mesh AUTHORED with the stripe in place.
    /// rows=4, w=0.4 → carve slides the y=0.5 ring to y=0.2; the reference is a box built
    /// with rings at exactly those heights. Identical bevel output, vertex-for-vertex.
    #[test]
    fn carve_then_bevel_matches_authored_stripe_mesh() {
        let mesh = ring_column_box(4);
        let sel = top_rim_selection(&mesh);
        let width = 0.4;
        let params = BevelParams { width, segments: 1, shape: 0.5, clamp: true };

        let carved = absorb_for_bevel(&mesh, &sel, width).expect("carves");
        let ours = bevel_edges_offset(&carved, &sel, &params);
        ours.validate().expect("beveled carved mesh valid");

        let reference = ring_column_box_at(&[-1.0, -0.5, 0.0, 0.2, 1.0]);
        let ref_sel = top_rim_selection(&reference);
        let theirs = bevel_edges_offset(&reference, &ref_sel, &params);

        assert_eq!(ours.vertex_count(), theirs.vertex_count(), "vertex count");
        assert_eq!(ours.face_count(), theirs.face_count(), "face count");
        let cloud = |m: &HalfEdgeMesh| -> Vec<[i32; 3]> {
            let q = |x: f32| (x * 10_000.0).round() as i32;
            let mut c: Vec<[i32; 3]> = (0..m.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| m.is_vert_live(v))
                .map(|v| {
                    let p = m.vert(v).position;
                    [q(p.x), q(p.y), q(p.z)]
                })
                .collect();
            c.sort_unstable();
            c
        };
        assert_eq!(cloud(&ours), cloud(&theirs), "bevel must match the authored-stripe reference");
    }

    // ---- v3 fuse ----

    /// FUSE ORACLE vs OUR OWN BEVEL: on a straight prism rim with tall rows and seg=1, the
    /// fuse (cut at w + chamfer band) must equal `bevel_edges_offset` (unclamped case)
    /// vertex-for-vertex: wall rim at w down the rail, cap rim at w in-plane.
    #[test]
    fn fuse_seg1_matches_plain_bevel_when_unclamped() {
        let mesh = ring_column_box(4); // rows 0.5; w=0.2 < clamp 0.25 → plain bevel unclamped
        let sel = top_rim_selection(&mesh);
        let params = BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true };
        let fused = consume_fuse_bevel(&mesh, &sel, &params).expect("fuse in scope");
        fused.validate().expect("fused valid");
        let plain = bevel_edges_offset(&mesh, &sel, &params);
        let cloud = |m: &HalfEdgeMesh| -> Vec<[i32; 3]> {
            let q = |x: f32| (x * 10_000.0).round() as i32;
            let mut c: Vec<[i32; 3]> = (0..m.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| m.is_vert_live(v))
                .map(|v| {
                    let p = m.vert(v).position;
                    [q(p.x), q(p.y), q(p.z)]
                })
                .collect();
            c.sort_unstable();
            c
        };
        assert_eq!(cloud(&fused), cloud(&plain), "seg=1 fuse == plain unclamped bevel");
    }

    /// FUSE past the clamp: w=0.4 on 0.5-rows — plain bevel clamps at 0.25; fuse cuts the
    /// wall boundary at exactly 0.4 (y=0.6) and offsets the cap by 0.4, watertight.
    #[test]
    fn fuse_reaches_full_width_past_clamp() {
        let mesh = ring_column_box(4);
        let sel = top_rim_selection(&mesh);
        let params = BevelParams { width: 0.4, segments: 1, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params).expect("fuse in scope");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        // Wall-side rim: 4 cut verts at y = 1 − 0.4.
        assert_eq!(verts_at_y(&out, 0.6, 1e-3), 4, "wall boundary cut at width");
        // Cap-side rim: 4 offset verts still at y = 1, inset to ±0.6 (square miter).
        let cap_rim = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .filter(|&v| {
                let p = out.vert(v).position;
                (p.y - 1.0).abs() < 1e-3
                    && (p.x.abs() - 0.6).abs() < 1e-3
                    && (p.z.abs() - 0.6).abs() < 1e-3
            })
            .count();
        assert_eq!(cap_rim, 4, "cap boundary offset by width");
        // The sharp rim vertices (±1, 1, ±1) are gone.
        let old_rim = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .filter(|&v| {
                let p = out.vert(v).position;
                (p.y - 1.0).abs() < 1e-3 && (p.x.abs() - 1.0).abs() < 1e-3
            })
            .count();
        assert_eq!(old_rim, 0, "sharp rim replaced by the fillet");
    }

    /// FUSE eats rings it covers: dense rows (0.25) with w=0.6 → rings at y=0.5 and 0.25
    /// dissolve (inside the fillet), the boundary is CUT at y=0.4 inside the surviving row.
    #[test]
    fn fuse_dissolves_covered_rings_and_cuts_midrow() {
        let mesh = ring_column_box(8);
        let sel = top_rim_selection(&mesh);
        let params = BevelParams { width: 0.6, segments: 3, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params).expect("fuse in scope");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        assert_eq!(verts_at_y(&out, 0.4, 1e-3), 4, "boundary cut at y=1−0.6 inside the row");
        assert_eq!(verts_at_y(&out, 0.5, 1e-3), 0, "covered ring y=0.5 dissolved");
        assert_eq!(verts_at_y(&out, 0.75, 1e-3), 0, "covered ring y=0.75 dissolved");
        // seg=3 → 2 intermediate profile loops of 4 verts between the two rims.
        assert!(out.vertex_count() > mesh.vertex_count() - 8, "profile loops exist");
    }

    /// Two parallel beveled loops share their ring faces → face-conflict gate → None
    /// (falls back to the v2 path; never double-owns a face).
    #[test]
    fn fuse_refuses_adjacent_parallel_loops() {
        let mesh = ring_column_box(4);
        let mut sel = top_rim_selection(&mesh);
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let ya = mesh.vert(mesh.he_source(he)).position.y;
            let yb = mesh.vert(mesh.he(he).vertex).position.y;
            if (ya - 0.5).abs() < 1e-4 && (yb - 0.5).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        let params = BevelParams { width: 0.1, segments: 1, shape: 0.5, clamp: true };
        assert!(consume_fuse_bevel(&mesh, &sel, &params).is_none());
    }

    /// Cube subdivided n×n on every side, [-1,1]³, welded lattice, outward winding.
    fn subdivided_cube(n: usize) -> HalfEdgeMesh {
        let mut map: HashMap<(usize, usize, usize), usize> = HashMap::new();
        let mut positions: Vec<Vec3> = Vec::new();
        let c = |i: usize| -1.0 + 2.0 * i as f32 / n as f32;
        let mut faces: Vec<Vec<usize>> = Vec::new();
        {
            let mut vid = |ijk: (usize, usize, usize)| -> usize {
                *map.entry(ijk).or_insert_with(|| {
                    positions.push(Vec3::new(c(ijk.0), c(ijk.1), c(ijk.2)));
                    positions.len() - 1
                })
            };
            // Each side: map (u,v) → lattice point; quad ring (u,v),(u,v+1),(u+1,v+1),(u+1,v).
            // Ring (u,v)→(u,v+1)→(u+1,v+1)→(u+1,v) has normal dP/dv × dP/du — each side's
            // parameterisation is chosen so that cross product points OUTWARD.
            let sides: [Box<dyn Fn(usize, usize) -> (usize, usize, usize)>; 6] = [
                Box::new(move |u, v| (u, n, v)), // top    y=+1: ẑ×x̂ = +ŷ
                Box::new(move |u, v| (v, 0, u)), // bottom y=-1: x̂×ẑ = −ŷ
                Box::new(move |u, v| (u, v, 0)), // front  z=-1: ŷ×x̂ = −ẑ
                Box::new(move |u, v| (v, u, n)), // back   z=+1: x̂×ŷ = +ẑ
                Box::new(move |u, v| (0, u, v)), // left   x=-1: ẑ×ŷ = −x̂
                Box::new(move |u, v| (n, v, u)), // right  x=+1: ŷ×ẑ = +x̂
            ];
            for side in &sides {
                for u in 0..n {
                    for v in 0..n {
                        faces.push(vec![
                            vid(side(u, v)),
                            vid(side(u, v + 1)),
                            vid(side(u + 1, v + 1)),
                            vid(side(u + 1, v)),
                        ]);
                    }
                }
            }
        }
        // Orient outward via signed volume.
        let mut vol6 = 0.0f32;
        for f in &faces {
            for k in 1..f.len() - 1 {
                let (a, b, cc) = (positions[f[0]], positions[f[k]], positions[f[k + 1]]);
                vol6 += a.dot(b.cross(cc));
            }
        }
        if vol6 < 0.0 {
            for f in &mut faces {
                f.reverse();
            }
        }
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        m.validate().expect("subdivided cube valid");
        assert!(m.is_manifold().is_ok(), "subdivided cube watertight");
        m
    }

    /// THE OWNER'S SIMPLE SITUATION (2026-08-08): subdivided cube, ONE full top edge
    /// selected (open chain ending on cube corners). The fuse must run the fillet the
    /// whole length and terminate it by carving the profile ARC into each end face —
    /// not a smeared fan.
    #[test]
    fn open_chain_fuse_on_subdivided_cube_ends_with_arc() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        // Chain: every edge along y=+1, z=-1.
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            if (a.y - 1.0).abs() < 1e-4
                && (b.y - 1.0).abs() < 1e-4
                && (a.z + 1.0).abs() < 1e-4
                && (b.z + 1.0).abs() < 1e-4
            {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), n, "full top-front edge chain selected");
        // v1 scope: width inside the first row (0.25 rows) — the carve-free clean case.
        // Wider widths take the open-chain WEDGE path (test below).
        let width = 0.2;
        let segments = 4u32;
        let params = BevelParams { width, segments, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params).expect("open-chain fuse in scope");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds");
        // The sharp edge is gone: no vertex remains on the old corner line (y=1, z=-1).
        let on_edge = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .filter(|&v| {
                let p = out.vert(v).position;
                (p.y - 1.0).abs() < 1e-3 && (p.z + 1.0).abs() < 1e-3
            })
            .count();
        assert_eq!(on_edge, 0, "sharp edge fully consumed");
        // Each end face (x = ±1) carries the profile arc: its two rim points at exactly
        // `width` from the old edge, plus segments−1 interior arc verts strictly between.
        for xs in [-1.0f32, 1.0f32] {
            let arc: Vec<Vec3> = (0..out.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| out.is_vert_live(v))
                .map(|v| out.vert(v).position)
                .filter(|p| (p.x - xs).abs() < 1e-3)
                .filter(|p| p.y > 1.0 - width - 1e-3 && p.z < -1.0 + width + 1e-3)
                .filter(|p| p.y < 1.0 + 1e-3 && p.z > -1.0 - 1e-3)
                .filter(|p| {
                    // strictly inside the rounded corner region, not on the flat lattice
                    let dy = 1.0 - p.y;
                    let dz = p.z + 1.0;
                    dy > 1e-3 || dz > 1e-3
                })
                .collect();
            // a (on top boundary), b (on front boundary) + segments−1 interior points.
            assert!(
                arc.len() >= (segments - 1) as usize + 2,
                "end face x={xs} carries the arc ({} pts)",
                arc.len()
            );
        }
    }

    /// WIDE width on the same open chain (crosses a lattice row): the carve dissolves the
    /// covered ring, the END-RAIL CLEAR removes the valence-3 stubs the dissolve leaves on
    /// the end faces (merging two end-face faces — consume extends into the end face), and
    /// the fuse then cuts + lofts as usual. This is the owner's cube scenario at full size.
    /// (The WEDGE is wrong here: its boolean merges the cube's coplanar lattice into big
    /// n-gons — 386v→10v — so clean meshes must fuse; the chain order guarantees it.)
    #[test]
    fn open_chain_fuse_wide_clears_end_rails() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            if (a.y - 1.0).abs() < 1e-4
                && (b.y - 1.0).abs() < 1e-4
                && (a.z + 1.0).abs() < 1e-4
                && (b.z + 1.0).abs() < 1e-4
            {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), n);
        let width = 0.45; // crosses the 0.25 lattice row on both sides
        let params = BevelParams { width, segments: 4, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params).expect("wide open-chain fuse in scope");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds");
        // The lattice BULK survives (fuse touches only the strip + end faces).
        assert!(
            out.vertex_count() > mesh.vertex_count() / 2,
            "bulk survives: {} of {} verts",
            out.vertex_count(),
            mesh.vertex_count()
        );
        // The sharp edge line (y=1, z=-1) is fully consumed.
        let on_edge = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .filter(|&v| {
                let p = out.vert(v).position;
                (p.y - 1.0).abs() < 1e-3 && (p.z + 1.0).abs() < 1e-3
            })
            .count();
        assert_eq!(on_edge, 0, "sharp edge fully consumed");
    }

    /// OWNER 2026-08-08: "push the width — your tool breaks all geo, removes all edges."
    /// Sweep the width across the whole range on the cube chain and require EVERY width to
    /// leave a sane mesh whichever consume path takes it (or cleanly falls through).
    #[test]
    fn width_sweep_never_destroys_the_mesh() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            if (a.y - 1.0).abs() < 1e-4
                && (b.y - 1.0).abs() < 1e-4
                && (a.z + 1.0).abs() < 1e-4
                && (b.z + 1.0).abs() < 1e-4
            {
                sel.insert(i);
            }
        }
        for step in 1..=14 {
            let width = step as f32 * 0.1;
            let params = BevelParams { width, segments: 3, shape: 0.5, clamp: true };
            // The exact chain the app runs: fuse → wedge → absorb+bevel → plain bevel.
            let (out, path) = match consume_fuse_bevel(&mesh, &sel, &params) {
                Some(m) => (m, "fuse"),
                None => match consume_wedge_cut(&mesh, &sel, &params) {
                    Some(m) => (m, "wedge"),
                    None => match absorb_for_bevel(&mesh, &sel, width) {
                        Some(a) => {
                            let live: HashSet<u32> = sel
                                .iter()
                                .copied()
                                .filter(|&e| {
                                    (e as usize) < a.half_edges.len()
                                        && a.is_edge_live(HalfEdgeId(e))
                                })
                                .collect();
                            (bevel_edges_offset(&a, &live, &params), "absorb+bevel")
                        }
                        None => (bevel_edges_offset(&mesh, &sel, &params), "plain"),
                    },
                },
            };
            out.validate().unwrap_or_else(|e| panic!("w={width}: invalid ({path}): {e}"));
            assert!(out.is_manifold().is_ok(), "w={width}: not watertight ({path})");
            assert!(
                out.vertex_count() * 2 >= mesh.vertex_count(),
                "w={width}: mesh destroyed ({path}): {} of {} verts",
                out.vertex_count(),
                mesh.vertex_count()
            );
            // PLACEMENT (owner 2026-08-08: "each time you ship, check vertex count AND
            // placement — it needs to make sense"): every vertex is either untouched
            // original lattice (all coords on the 0.25 grid, on the cube surface) or lives
            // inside the fillet band around the beveled edge (y=1,z=-1 line, within
            // ~width). A vertex that is neither means the op moved geometry it had no
            // business touching.
            let lattice = |x: f32| ((x / 0.25).round() * 0.25 - x).abs() < 1e-3;
            let on_surface = |p: Vec3| {
                p.x.abs() > 1.0 - 1e-3 || p.y.abs() > 1.0 - 1e-3 || p.z.abs() > 1.0 - 1e-3
            };
            let mut strays = 0usize;
            for vi in 0..out.vertices.len() as u32 {
                let v = VertexId(vi);
                if !out.is_vert_live(v) {
                    continue;
                }
                let p = out.vert(v).position;
                let is_lattice =
                    lattice(p.x) && lattice(p.y) && lattice(p.z) && on_surface(p);
                // Distance to the beveled edge line (y=1, z=-1).
                let dy = 1.0 - p.y;
                let dz = p.z + 1.0;
                let in_band = (dy * dy + dz * dz).sqrt() <= width * 1.6 + 1e-3;
                if !is_lattice && !in_band {
                    strays += 1;
                    if strays <= 4 {
                        eprintln!(
                            "w={width:.1} STRAY vert ({:.3},{:.3},{:.3}) — neither lattice nor band",
                            p.x, p.y, p.z
                        );
                    }
                }
            }
            assert_eq!(strays, 0, "w={width}: {strays} strayed vertices ({path})");
            // STRUCTURE (fuse only): each side's strip boundary is ONE clean loop — exactly
            // one vertex per column (n+1 = 9) at exactly `width` from the edge, on the
            // surface it belongs to. This is the check that catches a mangled band even
            // when counts and manifoldness look fine.
            if path == "fuse" {
                let count_at = |pred: &dyn Fn(Vec3) -> bool| -> usize {
                    (0..out.vertices.len() as u32)
                        .map(VertexId)
                        .filter(|&v| out.is_vert_live(v))
                        .map(|v| out.vert(v).position)
                        .filter(|&p| pred(p))
                        .count()
                };
                let top = count_at(&|p: Vec3| {
                    (p.y - 1.0).abs() < 1e-3 && (p.z - (-1.0 + width)).abs() < 2e-3
                });
                let wall = count_at(&|p: Vec3| {
                    (p.z + 1.0).abs() < 1e-3 && (p.y - (1.0 - width)).abs() < 2e-3
                });
                assert_eq!(top, n + 1, "w={width}: top boundary loop malformed ({top} verts)");
                assert_eq!(wall, n + 1, "w={width}: wall boundary loop malformed ({wall} verts)");
            }
            eprintln!("w={width:.1} path={path} v={} f={}", out.vertex_count(), out.face_count());
        }
    }

    /// OWNER 2026-08-08: "more edge cases — like FULL EDGE RING." The whole top-face
    /// perimeter selected: a closed loop whose 4 corner vertices TURN — side A is the top
    /// face's border ring turning at corner quads (each corner quad borders TWO selected
    /// edges → the ring-face-owned-twice gate refuses), side B changes wall face per side
    /// and needs a miter/corner fill at each vertical corner edge. This is the classic
    /// bevel CORNER problem (BEVEL.md §3 build_boundary / §9) surfacing inside consume.
    /// Ignored until the corner-turning fuse lands; the assert documents today's refusal
    /// so the wire-up is honest when it flips.
    #[test]
    fn full_top_perimeter_ring_fuse() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            // Any top-face (y=+1) BORDER edge: both endpoints on y=1 and on the cube's rim.
            let on_border = |p: Vec3| {
                (p.y - 1.0).abs() < 1e-4
                    && ((p.x.abs() - 1.0).abs() < 1e-4 || (p.z.abs() - 1.0).abs() < 1e-4)
            };
            if on_border(a) && on_border(b) {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 4 * n, "full perimeter ring selected");
        // v1 corner scope: lattice-aligned width (0.75 = 3 rows of 0.25); unaligned
        // widths refuse cleanly and fall back to absorb+bevel.
        let params = BevelParams { width: 0.75, segments: 3, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params);
        assert!(out.is_some(), "full-ring fuse (corner turning) supported");
    }

    /// Owner variation 1 (2026-08-08): TWO separate vertical edges (opposite cube corners),
    /// both full chains, both eating ≥4 rows (w=1.0 on the 0.25 lattice).
    #[test]
    fn two_vertical_chains_eat_four_rows() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            let vertical = (a.x - b.x).abs() < 1e-4 && (a.z - b.z).abs() < 1e-4;
            // Front-left (x=-1,z=-1) and back-right (x=1,z=1) vertical cube edges.
            let on_fl = (a.x + 1.0).abs() < 1e-4 && (a.z + 1.0).abs() < 1e-4;
            let on_br = (a.x - 1.0).abs() < 1e-4 && (a.z - 1.0).abs() < 1e-4;
            if vertical && (on_fl || on_br) {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 2 * n, "two full vertical chains");
        let width = 1.0; // 4 rows of 0.25
        let params = BevelParams { width, segments: 3, shape: 0.5, clamp: true };
        let out = consume_fuse_bevel(&mesh, &sel, &params).expect("two-chain fuse in scope");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds");
        assert!(out.vertex_count() * 2 >= mesh.vertex_count(), "bulk survives");
        // Both sharp edge lines fully consumed.
        for (xs, zs) in [(-1.0f32, -1.0f32), (1.0, 1.0)] {
            let on_edge = (0..out.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| out.is_vert_live(v))
                .filter(|&v| {
                    let p = out.vert(v).position;
                    (p.x - xs).abs() < 1e-3 && (p.z - zs).abs() < 1e-3
                })
                .count();
            assert_eq!(on_edge, 0, "edge ({xs},{zs}) consumed");
        }
        // Boundary loops at exactly width on both wall faces of each chain.
        let count_plane = |pred: &dyn Fn(Vec3) -> bool| -> usize {
            (0..out.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| out.is_vert_live(v))
                .map(|v| out.vert(v).position)
                .filter(|&p| pred(p))
                .count()
        };
        let fl_front = count_plane(&|p: Vec3| {
            (p.z + 1.0).abs() < 1e-3 && (p.x - (-1.0 + width)).abs() < 2e-3
        });
        assert_eq!(fl_front, n + 1, "front-left chain: front boundary loop ({fl_front})");
    }

    /// Owner variation 2 (2026-08-08): L-CHAIN — two adjacent top edges turning at the cube
    /// corner, eating ≥4 rows. Requires the corner-turning fuse (the corner quad borders
    /// both selected edges). Ignored until that lands — same corner class as the full ring.
    #[test]
    fn l_chain_around_corner_eats_four_rows() {
        let n = 8usize;
        let mesh = subdivided_cube(n);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = mesh.vert(mesh.he_source(he)).position;
            let b = mesh.vert(mesh.he(he).vertex).position;
            let top = (a.y - 1.0).abs() < 1e-4 && (b.y - 1.0).abs() < 1e-4;
            let on_front = (a.z + 1.0).abs() < 1e-4 && (b.z + 1.0).abs() < 1e-4;
            let on_right = (a.x - 1.0).abs() < 1e-4 && (b.x - 1.0).abs() < 1e-4;
            if top && (on_front || on_right) {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 2 * n, "L-chain of two top edges");
        let params = BevelParams { width: 1.0, segments: 3, shape: 0.5, clamp: true };
        assert!(
            consume_fuse_bevel(&mesh, &sel, &params).is_some(),
            "L-chain corner-turning fuse supported"
        );
    }

    // ---- v4 wedge cut ----

    /// The owner's EAT class in miniature: a cylinder hole booleaned into a box, with the
    /// sharp rim around the hole on the top face selected. The rim's outer side is a retraced
    /// n-gon ring, so the fuse refuses and the wedge owns this shape.
    fn boolean_rim_fixture() -> (HalfEdgeMesh, HashSet<u32>) {
        let box_mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let cyl = HalfEdgeMesh::new_cylinder(0.45, 3.0, 12);
        let cut = crate::mesh_boolean::mesh_boolean(
            &box_mesh,
            &cyl,
            crate::mesh_boolean::BoolOp::Difference,
        )
        .expect("boolean fixture");
        cut.validate().expect("fixture valid");
        let mut sel = HashSet::new();
        for i in 0..cut.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !cut.is_edge_live(he) {
                continue;
            }
            let t = cut.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let a = cut.vert(cut.he_source(he)).position;
            let b = cut.vert(cut.he(he).vertex).position;
            let on_top = (a.y - 1.0).abs() < 1e-4 && (b.y - 1.0).abs() < 1e-4;
            let r = |p: Vec3| (p.x * p.x + p.z * p.z).sqrt();
            let on_rim = r(a) < 0.5 && r(b) < 0.5;
            if on_top && on_rim {
                sel.insert(i);
            }
        }
        (cut, sel)
    }

    /// Distance from `p` to the fixture's sharp rim locus — the circle (radius 0.45, y = 1).
    /// The chamfer's closest approach to this circle is what separates a FLAT chord from a
    /// ROUND profile: the profile bulges toward the original corner, the chord cannot.
    fn dist_to_rim_circle(p: Vec3) -> f32 {
        let r = (p.x * p.x + p.z * p.z).sqrt();
        ((r - 0.45).powi(2) + (p.y - 1.0).powi(2)).sqrt()
    }

    /// SEGMENTS + ROUNDNESS ON THE WEDGE PATH (owner 2026-08-09: "geometry consume does not
    /// support segment and roundness"). The wedge used to cut a flat chord whatever the
    /// params. Gate is GEOMETRIC, not a vertex count: with segments>1 the cut surface must
    /// come strictly closer to the original sharp rim than the flat chord's closest possible
    /// approach — that is what "round" means here — and shape must move it monotonically.
    #[test]
    fn wedge_cut_rounds_with_segments_and_shape() {
        let (cut, sel) = boolean_rim_fixture();
        let w = 0.15;
        let closest = |segments: u32, shape: f32| -> f32 {
            let params = BevelParams { width: w, segments, shape, clamp: true };
            let out = consume_wedge_cut(&cut, &sel, &params)
                .unwrap_or_else(|| panic!("wedge cut succeeds at segments={segments} shape={shape}"));
            out.validate().expect("valid");
            assert!(out.is_manifold().is_ok(), "watertight at segments={segments}");
            assert_eq!(
                out.count_folded_coplanar_pairs(),
                0,
                "no folds at segments={segments}"
            );
            (0..out.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| out.is_vert_live(v))
                .map(|v| dist_to_rim_circle(out.vert(v).position))
                .fold(f32::MAX, f32::min)
        };
        // Flat chord: its perpendicular foot from the corner is w/√2 ≈ 0.106. Nothing the
        // seg=1 cutter produces can sit nearer the rim than that.
        let flat = closest(1, 0.5);
        let chord_foot = w / std::f32::consts::SQRT_2;
        assert!(
            flat > chord_foot * 0.9,
            "flat chamfer should not approach the rim closer than the chord foot ({chord_foot:.4}); got {flat:.4}"
        );
        // Round (shape 0.5 = circular): the profile mid-point sits at half the chord foot's
        // distance from the corner (0.25a + 0.5·rim + 0.25b), so the surface bulges IN.
        let round = closest(4, 0.5);
        assert!(
            round < flat * 0.8,
            "segments>1 must round the cut: closest approach {round:.4} vs flat {flat:.4}"
        );
        // Shape is live too: 0 = flat chord even at segments>1; 1 = bulge through the corner.
        let flat_shape = closest(4, 0.0);
        let convex = closest(4, 1.0);
        assert!(
            flat_shape > round,
            "shape=0 must flatten the profile back toward the chord ({flat_shape:.4} vs {round:.4})"
        );
        assert!(
            convex < round,
            "shape=1 must bulge further toward the rim ({convex:.4} vs {round:.4})"
        );
    }

    /// The wedge sweep must produce a manifold chamfer, consuming the rim vertices.
    #[test]
    fn wedge_cut_chamfers_a_boolean_rim() {
        let (cut, sel) = boolean_rim_fixture();
        assert!(sel.len() >= 10, "rim selection found ({} edges)", sel.len());
        let flat = BevelParams { width: 0.15, segments: 1, shape: 0.5, clamp: true };
        let out = consume_wedge_cut(&cut, &sel, &flat).expect("wedge cut succeeds");
        out.validate().expect("valid");
        assert!(out.is_manifold().is_ok(), "watertight");
        assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds");
        // The sharp rim vertices (top plane, radius 0.45) must be CONSUMED.
        let rim_left = (0..out.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| out.is_vert_live(v))
            .filter(|&v| {
                let p = out.vert(v).position;
                (p.y - 1.0).abs() < 1e-3 && ((p.x * p.x + p.z * p.z).sqrt() - 0.45).abs() < 1e-3
            })
            .count();
        assert_eq!(rim_left, 0, "rim vertices consumed by the wedge");
    }

    /// A selected loop is a WALL, never food or a slide target. Selecting every horizontal
    /// ring means each blocking "next loop" is itself beveled → nothing may be carved.
    #[test]
    fn refuses_when_next_loop_is_beveled_too() {
        let mesh = ring_column_box(4);
        let mut sel = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 {
                continue;
            }
            let ya = mesh.vert(mesh.he_source(he)).position.y;
            let yb = mesh.vert(mesh.he(he).vertex).position.y;
            if (ya - yb).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert!(sel.len() >= 20, "all 5 rings selected");
        assert!(absorb_for_bevel(&mesh, &sel, 0.4).is_none(), "beveled loops are walls");
    }
}
