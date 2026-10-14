//! Clamp-overlap (BEVEL.md §4 STEP 1, §5 invariant 9).
//!
//! Blender's `geometry_collide_offset` walk lives **inside** `bevel_edges_offset_mut`
//! (STEP 1 body). Pure relocation forbids splitting a function body across files, so that
//! walk stays with the pipeline in `mod.rs` until a future task extracts
//! the whole STEP-1 clamp block as a single item.
//!
//! This module owns the one clamp rule that is a standalone item: the OPPOSING-PAIR bound that
//! variable width needs (see [`opposing_pair_caps`]).

use glam::Vec3;
use std::collections::{HashMap, HashSet};

use crate::half_edge::{HalfEdgeId, HalfEdgeMesh, NO_FACE};

/// Cosine threshold for "these two edges offset straight at each other". −0.95 ≈ within 18° of
/// exactly antiparallel.
///
/// Deliberately strict. A CONVEX face's non-adjacent edges are NOT in each other's way even when
/// their inward normals broadly oppose — their offset lines meet off to the side and the corner
/// solve handles it, which is why a full-rim cylinder-cap bevel legitimately reaches ~0.97·r
/// (`mod.rs` STEP 1 records that case). Loosening this to −0.5 measurably eats into that: on a
/// 32-gon cap it would cut 0.97·r down to ~0.82·r for no geometric reason. Only a genuine SLAB —
/// two near-parallel walls facing each other — shares one corridor.
const OPPOSING_COS: f32 = -0.95;

/// Faces with more beveled edges than this skip the pair scan (it is O(k²)). A face needs hundreds
/// of simultaneously-beveled sides to reach this, and the per-edge capacity bound still applies.
const MAX_PAIR_SCAN: usize = 256;

/// Per-edge width ceilings from NON-ADJACENT beveled edges that offset straight at each other
/// inside one face — a "how thick is this wall" bound. Returns `primary edge id → ceiling`; an
/// absent edge is unconstrained by this rule.
///
/// **Why this exists, and why only for variable width.** The uniform path takes the GLOBAL minimum
/// of every per-edge capacity, and on a thin wall that minimum is usually contributed by the wall's
/// own short edges — so it accidentally protects the long edges too. Going per-edge removes that
/// accident: on a 0.1-thick plate the long edges' local capacity is 1.0 (their in-face neighbours
/// are 2 units away), so a 0.3 target would sail past the opposite wall and fold. Vertex-local
/// terms cannot see this: the two colliding edges are two steps apart in the face ring, never
/// adjacent.
///
/// The constraint for a facing pair is `w_a + w_b ≤ separation`, resolved PROPORTIONALLY
/// (`share_a = sep · w_a/(w_a+w_b)`) so the ratio variable width just established survives — an
/// even split would flatten a deliberately-small edge and a deliberately-large one back together.
///
/// ADJACENT pairs are skipped on purpose: a shared vertex is exactly what the collapse and slide
/// terms in STEP 1 already bound, and an acute corner would report a separation of ~0 here.
pub(crate) fn opposing_pair_caps(
    mesh: &HalfEdgeMesh,
    orig_pos: &[Vec3],
    beveled_he: &HashSet<u32>,
    und2prim: &HashMap<u32, u32>,
    target: &HashMap<u32, f32>,
) -> HashMap<u32, f32> {
    // Group the beveled half-edges by the face they offset into. Both twins of an edge are in
    // `beveled_he` and land in DIFFERENT faces, so each (face, side) pair is visited once.
    let mut by_face: HashMap<u32, Vec<u32>> = HashMap::new();
    for &he_id in beveled_he {
        let f = mesh.he(HalfEdgeId(he_id)).face;
        if f == NO_FACE {
            continue;
        }
        by_face.entry(f.0).or_default().push(he_id);
    }

    let mut out: HashMap<u32, f32> = HashMap::new();
    let mut faces: Vec<u32> = by_face.keys().copied().collect();
    faces.sort_unstable(); // determinism (§5 invariant 7)
    for fid in faces {
        let hes = by_face.get_mut(&fid).expect("key came from this map");
        if hes.len() < 2 || hes.len() > MAX_PAIR_SCAN {
            continue;
        }
        hes.sort_unstable();

        // Geometry per beveled side: endpoints, edge direction, and the in-plane inward
        // perpendicular. `normal × edge_dir` already points INTO the face for a winding-consistent
        // Newell normal, convex or concave — the same fact STEP 1's rail build relies on, so no
        // centroid sign fix here either.
        let normal = mesh.compute_face_normal(crate::half_edge::FaceId(fid));
        struct Side {
            prim: u32,
            src: Vec3,
            dst: Vec3,
            dir: Vec3,
            v0: u32,
            v1: u32,
            w: f32,
        }
        let mut sides: Vec<Side> = Vec::with_capacity(hes.len());
        for &he_id in hes.iter() {
            let he = HalfEdgeId(he_id);
            let v0 = mesh.he_source(he).0;
            let v1 = mesh.he(he).vertex.0;
            let (src, dst) = (orig_pos[v0 as usize], orig_pos[v1 as usize]);
            let edge_dir = (dst - src).normalize_or_zero();
            let dir = normal.cross(edge_dir).normalize_or_zero();
            if dir.length_squared() < 1e-12 {
                continue;
            }
            let Some(&prim) = und2prim.get(&he_id) else { continue };
            let Some(&w) = target.get(&prim) else { continue };
            sides.push(Side { prim, src, dst, dir, v0, v1, w });
        }

        for i in 0..sides.len() {
            for j in (i + 1)..sides.len() {
                let (a, b) = (&sides[i], &sides[j]);
                if a.prim == b.prim {
                    continue; // same undirected edge twice (cannot happen in one face, but cheap)
                }
                if a.dir.dot(b.dir) > OPPOSING_COS {
                    continue; // not facing each other down one corridor
                }
                if a.v0 == b.v0 || a.v0 == b.v1 || a.v1 == b.v0 || a.v1 == b.v1 {
                    continue; // adjacent — vertex-local terms own this case
                }
                let sum = a.w + b.w;
                if sum <= 1e-9 {
                    continue;
                }
                // Each side's own room: how far its rail can advance before reaching the nearer of
                // the other side's endpoints. Measured separately per side because a skewed pair
                // genuinely has two different corridors.
                let room = |from: &Side, to: &Side| -> f32 {
                    let d0 = (to.src - from.src).dot(from.dir);
                    let d1 = (to.dst - from.src).dot(from.dir);
                    d0.min(d1)
                };
                for (me, other) in [(a, b), (b, a)] {
                    let sep = room(me, other);
                    if sep <= 1e-7 {
                        continue; // behind, or touching — nothing this rule can say
                    }
                    if sum > sep {
                        let share = sep * (me.w / sum);
                        let slot = out.entry(me.prim).or_insert(f32::INFINITY);
                        if share < *slot {
                            *slot = share;
                        }
                    }
                }
            }
        }
    }
    out
}

/// CORNER SPAN — the size-of-detail measure behind [`BevelWidthDriver::CornerSpan`]
/// (owner-specified 2026-08-11; BEVEL.md §11).
///
/// For each beveled edge, at EACH of its two endpoint vertices:
///  1. take the two in-face neighbour edges (one per incident face — the same neighbours
///     `geometry_collide_offset` uses in STEP 1, so the two measures agree about what "next to"
///     means),
///  2. slide along BOTH by the SAME distance `s = min(len)` — the shorter neighbour caps it,
///  3. the chord between the two slid points is that end's span.
///
/// The edge's size is the **min over its two ends** (the tighter end governs: a bevel wide enough
/// for the roomy end would overrun the tight one).
///
/// 🔑 **Why the shared, min-capped slide is the whole point.** Sliding each neighbour to its own far
/// end would let one long neighbour inflate the span — the owner's exact counter-example is a 2 m
/// edge meeting a 1 cm edge, which must still read SMALL. Capping the slide at the shorter neighbour
/// is what makes the long one unable to lift the result.
///
/// ⚠ **NOT fully subdivision-invariant, and that was over-claimed when this was scoped.** The span is
/// stable while the SHORTEST neighbour is untouched (cutting the 2 m side changes nothing, since the
/// 1 cm side still caps `s`). But subdividing the shortest neighbour itself does shrink `s` and
/// therefore the span. That is the same sensitivity the existing clamp capacity already has — the two
/// measures behave consistently — but do not write a gate asserting invariance under arbitrary
/// subdivision, because it does not hold.
pub(crate) fn corner_span_sizes(
    mesh: &HalfEdgeMesh,
    orig_pos: &[Vec3],
    beveled_he: &HashSet<u32>,
    und2prim: &HashMap<u32, u32>,
) -> HashMap<u32, f32> {
    // (primary edge, endpoint vertex) → the incident faces' neighbour rays (unit dir, length).
    let mut per_end: HashMap<(u32, u32), Vec<(Vec3, f32)>> = HashMap::new();
    for &he_id in beveled_he {
        let he = mesh.he(HalfEdgeId(he_id));
        if he.face == NO_FACE {
            continue;
        }
        let Some(&prim) = und2prim.get(&he_id) else { continue };
        let hes = mesh.face_half_edges(he.face);
        let m = hes.len();
        if m < 3 {
            continue;
        }
        let Some(j) = hes.iter().position(|h| h.0 == he_id) else { continue };
        let vb = mesh.he_source(HalfEdgeId(he_id)).0;
        let vc = he.vertex.0;
        // Neighbour at the SOURCE runs back along the face's previous edge; at the DEST, forward
        // along the next edge. Identical selection to the STEP-1 collide walk.
        let va = mesh.he_source(hes[(j + m - 1) % m]).0;
        let vd = mesh.he(hes[(j + 1) % m]).vertex.0;
        for (v, nb) in [(vb, va), (vc, vd)] {
            let d = orig_pos[nb as usize] - orig_pos[v as usize];
            let l = d.length();
            if l > 1e-9 {
                per_end.entry((prim, v)).or_default().push((d / l, l));
            }
        }
    }

    let mut out: HashMap<u32, f32> = HashMap::new();
    for ((prim, v), nbrs) in per_end {
        // One entry means the edge is only manifold on one side here — no corner to span.
        if nbrs.len() < 2 {
            continue;
        }
        let s = nbrs.iter().map(|(_, l)| *l).fold(f32::INFINITY, f32::min);
        if !s.is_finite() || s <= 1e-9 {
            continue;
        }
        let vp = orig_pos[v as usize];
        // Min over pairs: with exactly two neighbours this is the single chord. A non-manifold fan
        // would give more, and the tightest pair is the one that limits the bevel.
        let mut span = f32::INFINITY;
        for i in 0..nbrs.len() {
            for k in (i + 1)..nbrs.len() {
                let p1 = vp + nbrs[i].0 * s;
                let p2 = vp + nbrs[k].0 * s;
                span = span.min((p1 - p2).length());
            }
        }
        if span.is_finite() {
            let slot = out.entry(prim).or_insert(f32::INFINITY);
            if span < *slot {
                *slot = span;
            }
        }
    }
    out
}
