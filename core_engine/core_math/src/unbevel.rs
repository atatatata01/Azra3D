//! Reverse-math Unbevel — reconstruct the sharp edge from the BAKED mesh, no history needed.
//!
//! Owner 2026-08-05: "make the un-bevel [work on the] bake, reverse math — Modo has it." This must
//! work on the mesh AS IT IS, so modeling done after the bevel survives (the session-local history
//! snapshot cannot promise that).
//!
//! Approach:
//! 1. Connected face component from the seed selection.
//! 2. Boundary loops (1 = open strip, 2 = closed band).
//! 3. Pair rails with **exclusive** arc-length matching (no many-to-one unite collapse).
//! 4. **Reverse math:** each pair's sharp point = least-squares intersection of the OUTSIDE
//!    surface planes adjacent to the pair (extend the faces back to where they met — Modo
//!    Unbevel). Midpoint sew (the old guess) shaved every corner by ~w/√2 and was why this
//!    path was second-class. Open-strip end arcs collapse onto the recovered end points, so a
//!    single-edge bevel restores the original mesh EXACTLY.
//! 5. Drop strip faces; rebuild via `from_polygons`; fix outward winding for re-bevel.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::{DMat3, DVec3, Vec3};

use crate::half_edge::{
    FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN,
};

/// Result of a successful topology unbevel.
#[derive(Clone, Debug)]
pub struct UnbevelResult {
    pub mesh: HalfEdgeMesh,
    /// Canonical half-edge ids (min of twin pair) of the recovered sharp edges.
    pub sharp_edges: HashSet<u32>,
    /// Approx prior bevel width (face offset), for initial live re-edit.
    pub estimated_width: f32,
    /// Approx prior segment count (face hops across strip).
    pub estimated_segments: u32,
}

/// Unbevel from face ids (the cyan strip selection). Empty seed → `None`.
pub fn unbevel_from_faces(mesh: &HalfEdgeMesh, seed_faces: &HashSet<u32>) -> Option<UnbevelResult> {
    if seed_faces.is_empty() {
        return None;
    }
    let selected = grow_face_component(mesh, seed_faces);
    if selected.is_empty() {
        return None;
    }
    unbevel_selected_faces(mesh, &selected)
}

/// Unbevel from edge ids. Three seeds are tried in order; the first that survives the
/// reverse-math gate wins.
///
/// 1. **Incident** — every face touching a selected edge. Right when the selection lies along
///    the middle of a strip.
/// 2. **Crossed** — only faces the selection runs ACROSS: at least two selected edges that are
///    NOT adjacent in the face's ring. Touching a face is not the same as being part of it, and
///    seed (1) cannot tell the difference — selecting the strip's own cross edges also drags in
///    the flanking walls (each shares one rail), so the "strip" grew sideways and the collapse
///    refused. A strip face is *spanned* by the selection; a neighbour merely borders it.
///
/// 3. **Rail run** — the selection lies ALONG the strip (a lengthwise / rail edge, which is what
///    a user picks when they click the rounded corner they can see). Neither seed above can work
///    from one: it borders the strip and the flat wall equally. [`rail_strip`] walks ACROSS
///    instead, and that walk is also the scoping — only the strip column at the selected edge.
///
/// Owner 2026-08-08, unbevelling a knife-rect corner round by picking its edges: seed (1) alone
/// pulled in both hole walls and refused.
pub fn unbevel_from_edges(mesh: &HalfEdgeMesh, seed_edges: &HashSet<u32>) -> Option<UnbevelResult> {
    if seed_edges.is_empty() {
        return None;
    }
    // Canonicalize to undirected edges so "is this edge selected" is twin-independent — the
    // caller may hand us either half, or both.
    let sel_key: HashSet<(u32, u32)> = seed_edges
        .iter()
        .filter(|&&e| (e as usize) < mesh.half_edges.len())
        .map(|&e| {
            let h = HalfEdgeId(e);
            edge_key(mesh.he_source(h).0, mesh.he(h).vertex.0)
        })
        .collect();

    // Seed 3 is tried FIRST when the selection is unambiguously a RAIL — one side of every
    // seed edge is a wall, so seed 1's "every incident face" set is strip+wall by
    // construction. It does not merely refuse there: on a cube it happily collapses the
    // 2-face component into a plausible-looking 7-face mesh (one strip face orphaned).
    if let Some(strip) = rail_strip(mesh, seed_edges) {
        if let Some(r) = unbevel_from_faces(mesh, &strip) {
            return Some(r);
        }
    }

    let mut incident = HashSet::new();
    for &e in seed_edges {
        if e as usize >= mesh.half_edges.len() {
            continue;
        }
        let he = mesh.he(HalfEdgeId(e));
        if he.face != NO_FACE {
            incident.insert(he.face.0);
        }
        if he.twin != NO_TWIN {
            let tf = mesh.he(he.twin).face;
            if tf != NO_FACE {
                incident.insert(tf.0);
            }
        }
    }
    if let Some(r) = unbevel_from_faces(mesh, &incident) {
        return Some(r);
    }

    // Seed 2: keep only faces the selection SPANS.
    let mut crossed = HashSet::new();
    for &fid in &incident {
        let ring = mesh.face_half_edges(FaceId(fid));
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let hit: Vec<bool> = ring
            .iter()
            .map(|&h| sel_key.contains(&edge_key(mesh.he_source(h).0, mesh.he(h).vertex.0)))
            .collect();
        // Two selected edges with at least one unselected edge between them, both ways round.
        let spanned = (0..n).any(|i| {
            hit[i] && (0..n).any(|j| hit[j] && j != i && (j + 1) % n != i && (i + 1) % n != j)
        });
        if spanned {
            crossed.insert(fid);
        }
    }
    if crossed.is_empty() || crossed.len() == incident.len() {
        return None;
    }
    unbevel_from_faces(mesh, &crossed)
}

/// A strip face may be at most this many times wider (measured across the walk) than the
/// narrowest face at the seed edge before the walk calls it a flanking wall and stops.
const RAIL_WIDTH_RATIO: f32 = 3.0;
/// Hard cap on segments crossed from one seed edge (a bevel is never 32 segments wide in
/// practice, and the cap keeps a smooth curved surface from being walked end to end).
const RAIL_MAX_HOPS: usize = 32;
/// Upper bound on single-strip-face boundary vertices before the exhaustive 4-subset corner
/// search is skipped for the turn-angle fallback. C(20,4) = 4845 — trivial; beyond that the
/// selection is not a bevel strip and the search would be answering the wrong question.
const MAX_CORNER_CANDIDATES: usize = 20;
/// How far the two end arcs of one strip may disagree in length before the selection is judged
/// not to be a single two-railed strip. Generous on purpose — a bevel end can be truncated by a
/// corner patch, and variable-width bevels differ end to end — while still an order of
/// magnitude below the mismatch an N-way corner fan produces (0.235 vs 1.38 on the owner's).
const MAX_ARC_LENGTH_RATIO: f32 = 4.0;

/// Distance across `face` from undirected edge `entry` to the ring-opposite edge, plus that
/// opposite half-edge (the exit for the next hop). `None` when the face has no opposite edge.
fn face_cross(mesh: &HalfEdgeMesh, face: FaceId, entry: HalfEdgeId) -> Option<(f32, HalfEdgeId)> {
    let on = crate::edge_select::he_on_face(mesh, face, entry)?;
    let opp = crate::edge_select::opposite_on_face(mesh, face, on)?;
    let mid = |h: HalfEdgeId| {
        (mesh.vert(mesh.he_source(h)).position + mesh.vert(mesh.he(h).vertex).position) * 0.5
    };
    Some(((mid(on) - mid(opp)).length(), opp))
}

/// Faces of the bevel strip a RAIL (lengthwise) edge selection lies on.
///
/// Owner 2026-08-09, one rounded corner edge picked in edge mode: seeds 1 and 2 both refuse
/// here — a rail edge borders the strip AND the flat wall equally, so "incident" drags the
/// wall in and "crossed" finds nothing to span. This walks ACROSS the strip instead, which is
/// also what scopes the operation: only the column of strip faces at the SELECTED edge is
/// collected, so unbevelling one segment of a long beveled edge leaves the rest alone.
///
/// The strip/wall discriminator is geometric: a bevel's faces are narrow across the walk and
/// similar to each other, while the surfaces it joins are wider. The walk starts at the
/// narrower of the two faces at the seed edge and stops at the first face more than
/// [`RAIL_WIDTH_RATIO`]x that width. When the seed edge is INTERIOR to a multi-segment strip
/// both sides qualify and both are walked.
///
/// KNOWN LIMIT (documented, not hidden): on a mesh whose flanking faces are as narrow as the
/// bevel itself the walk can overshoot into them. The result still has to clear
/// `unbevel_selected_faces` and the caller's manifold gate, and the whole thing runs inside an
/// undo episode — but a plausible-looking over-collapse is possible there. Select the strip
/// faces instead when the surrounding topology is that dense.
fn rail_strip(mesh: &HalfEdgeMesh, seed_edges: &HashSet<u32>) -> Option<HashSet<u32>> {
    let mut strip: HashSet<u32> = HashSet::new();
    // Only a selection where a WALL side was actually rejected is a rail selection. Without
    // that asymmetry (both faces at the seed edge equally wide — e.g. the strip's own cross
    // edges) this returns None and the older seeds keep their behaviour and their gates.
    let mut saw_wall = false;
    for &e in seed_edges {
        if e as usize >= mesh.half_edges.len() {
            continue;
        }
        let h = HalfEdgeId(e);
        let he = mesh.he(h);
        // The two starts: this half-edge's face, and the twin's face.
        let mut starts: Vec<(FaceId, HalfEdgeId, f32)> = Vec::new();
        if he.face != NO_FACE {
            if let Some((w, _)) = face_cross(mesh, he.face, h) {
                starts.push((he.face, h, w));
            }
        }
        if he.twin != NO_TWIN {
            let tf = mesh.he(he.twin).face;
            if tf != NO_FACE {
                if let Some((w, _)) = face_cross(mesh, tf, he.twin) {
                    starts.push((tf, he.twin, w));
                }
            }
        }
        let Some(base) = starts
            .iter()
            .map(|&(_, _, w)| w)
            .fold(None::<f32>, |acc, w| Some(acc.map_or(w, |a: f32| a.min(w))))
        else {
            continue;
        };
        if base <= 0.0 {
            continue;
        }
        let limit = base * RAIL_WIDTH_RATIO;

        for (face0, entry0, w0) in starts {
            if w0 > limit {
                saw_wall = true;
                continue; // that side is the flanking wall, not the strip
            }
            let mut face = face0;
            let mut entry = entry0;
            for _ in 0..RAIL_MAX_HOPS {
                let Some((w, opp)) = face_cross(mesh, face, entry) else {
                    break;
                };
                if w > limit {
                    break; // walked off the strip onto the surface it joins
                }
                strip.insert(face.0);
                let tw = mesh.he(opp).twin;
                if tw == NO_TWIN {
                    break;
                }
                let next = mesh.he(tw).face;
                if next == NO_FACE || strip.contains(&next.0) {
                    break;
                }
                face = next;
                entry = tw;
            }
        }
    }
    if strip.is_empty() || !saw_wall {
        None
    } else {
        Some(strip)
    }
}

// ── grow ────────────────────────────────────────────────────────────────────

fn grow_face_component(mesh: &HalfEdgeMesh, seed: &HashSet<u32>) -> HashSet<u32> {
    let seed_alive: HashSet<u32> = seed
        .iter()
        .copied()
        .filter(|&f| f < mesh.faces.len() as u32 && mesh.face(FaceId(f)).half_edge != NO_EDGE)
        .collect();
    if seed_alive.is_empty() {
        return HashSet::new();
    }
    let mut comp = HashSet::new();
    for &s in &seed_alive {
        if comp.contains(&s) {
            continue;
        }
        let mut q = VecDeque::new();
        q.push_back(s);
        comp.insert(s);
        while let Some(f) = q.pop_front() {
            let hs = mesh.face(FaceId(f)).half_edge;
            if hs == NO_EDGE {
                continue;
            }
            let mut he = hs;
            loop {
                let h = mesh.he(he);
                if h.twin != NO_TWIN {
                    let tf = mesh.he(h.twin).face;
                    if tf != NO_FACE && seed_alive.contains(&tf.0) && comp.insert(tf.0) {
                        q.push_back(tf.0);
                    }
                }
                he = h.next;
                if he == hs {
                    break;
                }
            }
        }
    }
    comp
}

// ── core sew ────────────────────────────────────────────────────────────────

fn unbevel_selected_faces(mesh: &HalfEdgeMesh, selected: &HashSet<u32>) -> Option<UnbevelResult> {
    // Boundary undirected edges of the selection.
    let mut boundary: Vec<(u32, u32)> = Vec::new();
    let mut boundary_set: HashSet<(u32, u32)> = HashSet::new();
    for &fid in selected {
        let start = mesh.face(FaceId(fid)).half_edge;
        if start == NO_EDGE {
            continue;
        }
        let mut he = start;
        loop {
            let h = mesh.he(he);
            let other_sel = if h.twin == NO_TWIN {
                false
            } else {
                let tf = mesh.he(h.twin).face;
                tf != NO_FACE && selected.contains(&tf.0)
            };
            if !other_sel {
                let a = mesh.he_source(he).0;
                let b = h.vertex.0;
                let key = edge_key(a, b);
                if boundary_set.insert(key) {
                    boundary.push(key);
                }
            }
            he = h.next;
            if he == start {
                break;
            }
        }
    }
    if boundary.len() < 2 {
        return None;
    }

    // DETERMINISM: `boundary` was accumulated by iterating `selected`, a HashSet, so its order
    // — and therefore each vertex's neighbour order, the loop-walk direction, and every index
    // derived from them downstream — varied run to run with Rust's per-process hash seed. The
    // same strip could unbevel cleanly once and refuse the next time on identical input, which
    // makes the result unreproducible and any refusal impossible to bisect. Sorting costs
    // nothing here and makes the walk a function of the mesh alone.
    boundary.sort_unstable();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(a, b) in &boundary {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    for n in adj.values_mut() {
        n.sort_unstable();
    }
    if adj.values().any(|n| n.len() < 2) {
        return None;
    }

    let loops = extract_boundary_loops(&adj);
    if loops.is_empty() {
        return None;
    }

    // Exclusive sew pairs: each vert appears in at most one pair (no transitive collapse).
    // Open strips also keep their two short end chains (the profile arcs) — those verts
    // collapse onto the recovered end points below.
    let mut end_arcs: Option<(Vec<u32>, Vec<u32>)> = None;
    let (pairs, closed) = if loops.len() >= 2 {
        let mut loops_sorted = loops;
        loops_sorted.sort_by_key(|l| std::cmp::Reverse(l.len()));
        let a = &loops_sorted[0];
        let b = &loops_sorted[1];
        if a.len() < 2 || b.len() < 2 {
            return None;
        }
        (pair_loops_exclusive(mesh, a, b), true)
    } else {
        let loop_v = &loops[0];
        if loop_v.len() < 4 {
            return None;
        }
        let Some((rail0, rail1, arc_a, arc_b)) = split_loop_into_rails(mesh, selected, loop_v)
        else {
            return None;
        };
        if rail0.len() < 2 || rail1.len() < 2 {
            return None;
        }
        end_arcs = Some((arc_a, arc_b));
        (pair_rails_exclusive(mesh, &rail0, &rail1), false)
    };

    if pairs.len() < 2 {
        return None;
    }

    // Sharp target per exclusive pair — REVERSE MATH: least-squares intersection of the
    // OUTSIDE surface planes adjacent to the pair (extend the neighbouring faces back to
    // where they met — Modo Unbevel). The old midpoint sew shaved every corner by ~w/√2.
    // Straight run → 2 planes (their line, pinned nearest the pair); mesh corner → 3+
    // planes (the exact corner point). Near-parallel/ill-conditioned → midpoint fallback.
    let mut target_of: HashMap<u32, Vec3> = HashMap::new(); // old vert → sharp target
    let mut pair_widths: Vec<f32> = Vec::new();
    let mut offset_widths: Vec<f32> = Vec::new();
    let mut targets_ordered: Vec<Vec3> = Vec::with_capacity(pairs.len());
    for &(va, vb) in &pairs {
        let pa = mesh.vert(VertexId(va)).position;
        let pb = mesh.vert(VertexId(vb)).position;
        let mid = (pa + pb) * 0.5;
        let sep = pa.distance(pb);
        let mut planes: Vec<(Vec3, f32)> = Vec::new();
        outside_planes_at(mesh, selected, va, &mut planes);
        outside_planes_at(mesh, selected, vb, &mut planes);
        let mut target = if planes.is_empty() {
            mid
        } else {
            solve_sharp_point(&planes, mid)
        };
        // Spike guard: a wildly distant solution means near-parallel planes (shallow
        // bevel on an almost-flat edge) or bad plane gathering — the midpoint is the
        // honest answer there (the sharp shape barely differs from the strip).
        if target.distance(mid) > sep * 10.0 + 1e-4 {
            target = mid;
        } else if !planes.is_empty() {
            // Recovered offset = rail→sharp distance along the surface (Blender's width).
            offset_widths.push((pa.distance(target) + pb.distance(target)) * 0.5);
        }
        pair_widths.push(sep);
        target_of.insert(va, target);
        target_of.insert(vb, target);
        targets_ordered.push(target);
    }

    // Open strip: the end-arc (profile) verts collapse onto the sharp point recovered at
    // their end of the strip — this is what restores a single-edge bevel EXACTLY (the end
    // caps re-trace through the original corner instead of keeping the arc dent).
    if let Some((arc_a, arc_b)) = &end_arcs {
        if let (Some(&first), Some(&last)) = (targets_ordered.first(), targets_ordered.last()) {
            for arc in [arc_a, arc_b] {
                for &v in arc {
                    if target_of.contains_key(&v) {
                        continue;
                    }
                    let p = mesh.vert(VertexId(v)).position;
                    let t = if p.distance(first) <= p.distance(last) {
                        first
                    } else {
                        last
                    };
                    target_of.insert(v, t);
                }
            }
        }
    }
    // Rebuild: keep non-selected faces; map rail verts to sharp targets; drop strip faces.
    let mut positions: Vec<Vec3> = Vec::new();
    let mut vert_to_out: HashMap<u32, usize> = HashMap::new();

    let mut ensure = |v: u32| -> Option<usize> {
        // Prefer the recovered sharp target if this vert is on a rail pair / end arc.
        if let Some(&mid) = target_of.get(&v) {
            // One output index per unique target position (shared by both rails).
            // Key targets by quantized position so va and vb share one index.
            let q = quant_key(mid);
            // Search existing
            for (i, p) in positions.iter().enumerate() {
                if quant_key(*p) == q {
                    vert_to_out.insert(v, i);
                    return Some(i);
                }
            }
            let idx = positions.len();
            positions.push(mid);
            vert_to_out.insert(v, idx);
            return Some(idx);
        }
        if let Some(&idx) = vert_to_out.get(&v) {
            return Some(idx);
        }
        let pos = mesh.vert(VertexId(v)).position;
        let idx = positions.len();
        positions.push(pos);
        vert_to_out.insert(v, idx);
        Some(idx)
    };

    let mut faces_out: Vec<Vec<usize>> = Vec::new();
    for fi in 0..mesh.faces.len() as u32 {
        if selected.contains(&fi) {
            continue;
        }
        if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
            continue;
        }
        let ring = mesh.face_vertices(FaceId(fi));
        let mut out_ring: Vec<usize> = Vec::with_capacity(ring.len());
        for v in ring.iter() {
            let idx = ensure(v.0)?;
            if out_ring.last().copied() != Some(idx) {
                out_ring.push(idx);
            }
        }
        let final_r = clean_sewn_ring(out_ring);
        // A vertex repeated at NON-adjacent ring positions is a PINCH: the sew pulled two
        // separated corners of this face onto one point, folding it into two lobes joined at
        // that point. Splitting there is the honest resolution — the lobes are the real
        // surface and the collapsed side is a sliver with no area. Dropping the whole face
        // would open a hole; keeping the ring intact makes `from_polygons` emit a self-loop
        // face and `is_manifold()` refuse the entire unbevel (owner's 16-face yellow strip:
        // "degenerate face 7: repeated vertex 20"), which is how a correct collapse was
        // being thrown away over one 9-gon.
        split_pinched_ring(final_r, &mut faces_out);
    }

    if faces_out.is_empty() || positions.len() < 3 {
        return None;
    }

    // WINDING IS INHERITED, NEVER RE-DERIVED.
    //
    // Every ring in `faces_out` was copied straight off the input mesh by `face_vertices`
    // above, so it already carries that mesh's consistent orientation; merging vertices
    // cannot reverse a loop. There is nothing to fix, and guessing makes it worse.
    //
    // This USED to run a "flip any face whose normal points away from the mesh centroid"
    // pass. That is only valid on a star-shaped solid. On anything concave — and a knife
    // rect cut is the canonical case, since the body centroid lands INSIDE the hole — every
    // wall of the hole has its normal pointing back toward the centroid, so the whole hole
    // got reversed. `from_polygons` then saw two faces walking the same directed edge and
    // `is_manifold()` rejected it ("directed edge a->b used by 2 faces (same winding)"),
    // which the gate below turned into a flat "Unbevel: reverse math refused" — on a
    // collapse whose vertex and face counts were already exactly right (owner 2026-08-08:
    // "i still cant unbevel the bevel from rect knife cut"). Same family as BEVEL.md §11
    // 2026-07-18: the centroid-based inward sign-fix is WRONG on concave faces.
    let new_mesh = HalfEdgeMesh::from_polygons(&positions, &faces_out);

    // Refuse garbage: topology-guess must not open holes or go non-manifold (owner image:
    // collapsed pyramid after Unbevel on multi-seg corner strip). Clean refuse → history path.
    if new_mesh.is_manifold().is_err() {
        return None;
    }
    let in_loops = crate::bevel_op::boundary_loop_count(mesh);
    let out_loops = crate::bevel_op::boundary_loop_count(&new_mesh);
    if out_loops > in_loops {
        return None;
    }
    // Face count should drop (strip removed) but stay plausible — not a near-empty wreck.
    let in_faces = mesh.face_count();
    let out_faces = new_mesh.face_count();
    if out_faces == 0 || out_faces > in_faces {
        return None;
    }
    // Closed input: refuse total collapse (owner: pyramid/hole after Unbevel on multi-seg strip).
    // Strip removal may drop many faces; still keep a solid-ish shell (at least a tet/cube-ish).
    if in_loops == 0 && out_faces < 4 {
        return None;
    }
    // WRECK GATE. A face only ever disappears here by collapsing to fewer than three DISTINCT
    // points, i.e. to zero area, so no face COUNT can separate a good collapse from a bad one.
    // That is why the old test — "refuse if more than 2/3 of the faces are gone" — threw away
    // correct answers: a 4-segment bevel of a cube's top ring legitimately IS 16 of the mesh's
    // 22 faces, so removing it correctly (back to the exact 6-face cube, proven manifold and
    // hole-free right above) tripped the rule every time, and the rounder the bevel the worse
    // it got.
    //
    // The wreck it was written for has its own signature: the strip sewn down to a POINT
    // instead of to an edge. Guard that. With distinct recovered sharp points the result is an
    // edge, and the manifold + boundary-loop gates above already prove the surface closed up
    // around it.
    let mut unique_targets: Vec<(i64, i64, i64)> =
        targets_ordered.iter().map(|&t| quant_key(t)).collect();
    unique_targets.sort_unstable();
    unique_targets.dedup();
    if unique_targets.len() < 2 {
        return None;
    }

    // Sharp edges: consecutive recovered targets along the chain (wrap if closed band).
    let mut sharp = HashSet::new();
    let n_m = targets_ordered.len();
    let steps = if closed { n_m } else { n_m.saturating_sub(1) };
    for i in 0..steps {
        let j = (i + 1) % n_m;
        if !closed && j == 0 {
            break;
        }
        let p0 = targets_ordered[i];
        let p1 = targets_ordered[j];
        // Skip zero-length (duplicate targets from sampling)
        if p0.distance(p1) < 1e-6 {
            continue;
        }
        if let Some(e) = find_edge_by_positions(&new_mesh, p0, p1) {
            sharp.insert(e);
        }
    }

    if sharp.is_empty() {
        return None;
    }

    // Bevel width: with plane recovery it is MEASURED — rail→sharp distance along the
    // surface is exactly Blender's offset, at any dihedral. Fallback (no planes solved):
    // rail separation / √2, right only for 90° corners.
    let estimated_width = if !offset_widths.is_empty() {
        (offset_widths.iter().sum::<f32>() / offset_widths.len() as f32).max(0.001)
    } else {
        let avg_sep = if pair_widths.is_empty() {
            0.2
        } else {
            pair_widths.iter().sum::<f32>() / pair_widths.len() as f32
        };
        (avg_sep / std::f32::consts::SQRT_2).max(0.001)
    };
    let estimated_segments = estimate_segments(mesh, selected).max(1);

    Some(UnbevelResult {
        mesh: new_mesh,
        sharp_edges: sharp,
        estimated_width,
        estimated_segments,
    })
}

// ── reverse math: adjacent-surface plane recovery ───────────────────────────

/// Collect the surface planes OUTSIDE the strip that touch vertex `v` — the faces the
/// bevel offset away from. Each plane is `(unit normal, d)` with `n·x = d`, through the
/// face centroid (robust on slightly non-planar faces). Near-duplicate planes (coplanar
/// neighbours) are deduped so they don't over-weight the least-squares solve.
fn outside_planes_at(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    v: u32,
    out: &mut Vec<(Vec3, f32)>,
) {
    for he in mesh.vertex_fan(VertexId(v)) {
        let f = mesh.he(he).face;
        if f == NO_FACE || selected.contains(&f.0) {
            continue;
        }
        let n = mesh.compute_face_normal(f);
        if n.length_squared() < 1e-12 {
            continue;
        }
        let ring = mesh.face_vertices(f);
        if ring.is_empty() {
            continue;
        }
        let c = ring
            .iter()
            .map(|&vv| mesh.vert(vv).position)
            .sum::<Vec3>()
            / ring.len() as f32;
        let d = n.dot(c);
        let dup = out
            .iter()
            .any(|&(en, ed)| en.dot(n) > 0.999 && (ed - d).abs() < 1e-4 * (1.0 + d.abs()));
        if !dup {
            out.push((n, d));
        }
    }
}

/// Least-squares intersection of `planes`, regularized toward `mid`: minimizes
/// Σ(nᵢ·P − dᵢ)² + λ|P − mid|². Two planes → their intersection LINE, pinned to the point
/// nearest `mid` (λ fills the null direction); three independent planes → their exact
/// corner point; degenerate sets degrade gracefully toward `mid`. Solved in f64 with a
/// tiny λ so well-conditioned recoveries are exact to float precision.
fn solve_sharp_point(planes: &[(Vec3, f32)], mid: Vec3) -> Vec3 {
    const LAMBDA: f64 = 1e-6;
    let mut a = DMat3::from_diagonal(DVec3::splat(LAMBDA));
    let mut b = mid.as_dvec3() * LAMBDA;
    for &(n, d) in planes {
        let n = n.as_dvec3();
        a += DMat3::from_cols(n * n.x, n * n.y, n * n.z);
        b += n * d as f64;
    }
    let det = a.determinant();
    if !det.is_finite() || det.abs() < 1e-18 {
        return mid;
    }
    let p = a.inverse() * b;
    if p.is_finite() {
        p.as_vec3()
    } else {
        mid
    }
}

/// Normalise one rebuilt face ring after the sew merged vertices, to a fixpoint.
///
/// Two degeneracies appear, and BOTH must be removed before the ring is handed to
/// `from_polygons`, because each of them makes `is_manifold()` refuse the whole unbevel:
///
/// * **Repeated neighbours** (`… A A …`) — two of the face's corners merged onto one point.
/// * **Hairline flaps** (`… A B A …`) — the ring walks out to B and straight back. Removing
///   only the repeated neighbours never catches these: B's neighbours are equal but B itself
///   differs from both, so the old single pass walked straight past `[20, 6, 20]` and emitted
///   a ring with vertex 20 twice. That is the "degenerate face: repeated vertex (self-loop /
///   pinch)" the owner's 16-face strip refused on — one 9-gon with a zero-area flap, killing
///   an otherwise correct collapse.
///
/// Removing a flap can expose another (`A B A B A` peels twice), hence the loop.
fn clean_sewn_ring(mut ring: Vec<usize>) -> Vec<usize> {
    loop {
        // Cyclic consecutive duplicates.
        let mut dedup: Vec<usize> = Vec::with_capacity(ring.len());
        for &i in &ring {
            if dedup.last().copied() != Some(i) {
                dedup.push(i);
            }
        }
        while dedup.len() >= 2 && dedup.first() == dedup.last() {
            dedup.pop();
        }
        if dedup.len() < 3 {
            return Vec::new();
        }
        // Flap tips: a vertex whose two ring neighbours are the SAME point encloses no area.
        let n = dedup.len();
        let Some(tip) = (0..n).find(|&i| dedup[(i + n - 1) % n] == dedup[(i + 1) % n]) else {
            return dedup;
        };
        dedup.remove(tip);
        ring = dedup;
    }
}

/// Emit `ring`, splitting it at any vertex that appears twice (a pinch) into the lobes that
/// meet there. Recurses so a ring pinched more than once still resolves. Lobes with fewer than
/// three distinct vertices are slivers with no area and are dropped.
fn split_pinched_ring(ring: Vec<usize>, out: &mut Vec<Vec<usize>>) {
    if ring.len() < 3 {
        return;
    }
    let mut first_at: HashMap<usize, usize> = HashMap::new();
    for (k, &v) in ring.iter().enumerate() {
        if let Some(&p) = first_at.get(&v) {
            // Lobe A: p..k. Lobe B: k..end + start..p. Each keeps ONE copy of the pinch vertex.
            let a = ring[p..k].to_vec();
            let mut b = ring[k..].to_vec();
            b.extend_from_slice(&ring[..p]);
            split_pinched_ring(a, out);
            split_pinched_ring(b, out);
            return;
        }
        first_at.insert(v, k);
    }
    out.push(ring);
}

fn quant_key(p: Vec3) -> (i64, i64, i64) {
    const S: f32 = 1e5;
    (
        (p.x * S).round() as i64,
        (p.y * S).round() as i64,
        (p.z * S).round() as i64,
    )
}

// ── boundary loops ──────────────────────────────────────────────────────────

fn extract_boundary_loops(adj: &HashMap<u32, Vec<u32>>) -> Vec<Vec<u32>> {
    let mut visited_edge: HashSet<(u32, u32)> = HashSet::new();
    let mut loops = Vec::new();
    // Sorted, not `adj.keys()`: which vertex a loop starts at decides the index of every
    // corner the caller later picks, so a hash-ordered start made the whole split unstable.
    let mut starts: Vec<u32> = adj.keys().copied().collect();
    starts.sort_unstable();
    for start in starts {
        for &n0 in adj.get(&start).into_iter().flatten() {
            let e0 = edge_key(start, n0);
            if visited_edge.contains(&e0) {
                continue;
            }
            let mut loop_v = vec![start];
            let mut prev = start;
            let mut cur = n0;
            visited_edge.insert(e0);
            let mut guard = 0;
            while cur != start {
                loop_v.push(cur);
                let nbrs = adj.get(&cur).cloned().unwrap_or_default();
                let next = nbrs.into_iter().find(|&n| n != prev).unwrap_or(prev);
                let ek = edge_key(cur, next);
                if !visited_edge.insert(ek) && next != start {
                    break;
                }
                prev = cur;
                cur = next;
                guard += 1;
                if guard > 100_000 {
                    break;
                }
            }
            if loop_v.len() >= 3 {
                loops.push(loop_v);
            }
        }
    }
    loops
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

// ── exclusive pairing (no many-to-one collapse) ─────────────────────────────

/// Sample both loops to the same count along arc length; try reverse + phase for best fit.
/// Each sample produces one exclusive pair — never unites three verts into one midpoint.
fn pair_loops_exclusive(mesh: &HalfEdgeMesh, a: &[u32], b: &[u32]) -> Vec<(u32, u32)> {
    let pa = loop_positions(mesh, a);
    let pb = loop_positions(mesh, b);
    if pa.len() < 2 || pb.len() < 2 {
        return Vec::new();
    }
    let n = pa.len().min(pb.len()).max(2);
    let sa = resample_closed(&pa, n);
    let mut best: Option<(f32, bool, usize, Vec<Vec3>, Vec<Vec3>)> = None;
    for rev in [false, true] {
        let mut sb = resample_closed(&pb, n);
        if rev {
            sb.reverse();
            // keep cyclic order: reverse then rotate so start is consistent
        }
        for off in 0..n {
            let mut cost = 0.0f32;
            for i in 0..n {
                let j = (i + off) % n;
                cost += sa[i].distance_squared(sb[j]);
            }
            if best.as_ref().map_or(true, |(c, ..)| cost < *c) {
                best = Some((cost, rev, off, sa.clone(), sb.clone()));
            }
        }
    }
    let (_, _, off, sa, sb) = best.unwrap();
    // Map samples back to nearest original loop verts for pairing ids.
    let mut pairs = Vec::with_capacity(n);
    let mut used_a = HashSet::new();
    let mut used_b = HashSet::new();
    for i in 0..n {
        let j = (i + off) % n;
        let ia = nearest_vert(mesh, a, sa[i], &used_a);
        let ib = nearest_vert(mesh, b, sb[j], &used_b);
        if let (Some(va), Some(vb)) = (ia, ib) {
            used_a.insert(va);
            used_b.insert(vb);
            pairs.push((va, vb));
        }
    }
    pairs
}

fn pair_rails_exclusive(mesh: &HalfEdgeMesh, rail0: &[u32], rail1: &[u32]) -> Vec<(u32, u32)> {
    let p0 = loop_positions(mesh, rail0);
    let p1 = loop_positions(mesh, rail1);
    if p0.len() < 2 || p1.len() < 2 {
        return Vec::new();
    }
    let n = p0.len().min(p1.len()).max(2);
    let s0 = resample_open(&p0, n);
    let try_pair = |s1: &[Vec3]| -> (f32, Vec<(u32, u32)>) {
        let mut cost = 0.0f32;
        let mut pairs = Vec::new();
        let mut used0 = HashSet::new();
        let mut used1 = HashSet::new();
        for i in 0..n {
            cost += s0[i].distance_squared(s1[i]);
            let va = nearest_vert(mesh, rail0, s0[i], &used0);
            let vb = nearest_vert(mesh, rail1, s1[i], &used1);
            if let (Some(a), Some(b)) = (va, vb) {
                used0.insert(a);
                used1.insert(b);
                pairs.push((a, b));
            }
        }
        (cost, pairs)
    };
    let s1 = resample_open(&p1, n);
    let (c1, p_fwd) = try_pair(&s1);
    let mut s1r = s1.clone();
    s1r.reverse();
    let (c2, p_rev) = try_pair(&s1r);
    if c1 <= c2 {
        p_fwd
    } else {
        p_rev
    }
}

fn loop_positions(mesh: &HalfEdgeMesh, ids: &[u32]) -> Vec<Vec3> {
    ids.iter()
        .map(|&v| mesh.vert(VertexId(v)).position)
        .collect()
}

fn resample_closed(pts: &[Vec3], n: usize) -> Vec<Vec3> {
    if pts.is_empty() || n == 0 {
        return Vec::new();
    }
    if pts.len() == 1 {
        return vec![pts[0]; n];
    }
    // Cumulative arc length including closing segment.
    let mut cum = vec![0.0f32];
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        cum.push(cum[i] + pts[i].distance(pts[j]));
    }
    let total = *cum.last().unwrap();
    if total < 1e-8 {
        return vec![pts[0]; n];
    }
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let t = total * (k as f32) / (n as f32);
        out.push(sample_polyline_closed(pts, &cum, t));
    }
    out
}

fn resample_open(pts: &[Vec3], n: usize) -> Vec<Vec3> {
    if pts.is_empty() || n == 0 {
        return Vec::new();
    }
    if pts.len() == 1 {
        return vec![pts[0]; n];
    }
    let mut cum = vec![0.0f32];
    for i in 0..pts.len() - 1 {
        cum.push(cum[i] + pts[i].distance(pts[i + 1]));
    }
    let total = *cum.last().unwrap();
    if total < 1e-8 {
        return vec![pts[0]; n];
    }
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let t = if n == 1 {
            0.0
        } else {
            total * (k as f32) / ((n - 1) as f32)
        };
        out.push(sample_polyline_open(pts, &cum, t));
    }
    out
}

fn sample_polyline_closed(pts: &[Vec3], cum: &[f32], t: f32) -> Vec3 {
    let total = *cum.last().unwrap();
    let t = t.clamp(0.0, total - 1e-6);
    for i in 0..pts.len() {
        if t >= cum[i] && t <= cum[i + 1] {
            let seg = (cum[i + 1] - cum[i]).max(1e-8);
            let u = (t - cum[i]) / seg;
            let j = (i + 1) % pts.len();
            return pts[i].lerp(pts[j], u);
        }
    }
    pts[0]
}

fn sample_polyline_open(pts: &[Vec3], cum: &[f32], t: f32) -> Vec3 {
    let total = *cum.last().unwrap();
    let t = t.clamp(0.0, total);
    for i in 0..pts.len() - 1 {
        if t >= cum[i] && t <= cum[i + 1] {
            let seg = (cum[i + 1] - cum[i]).max(1e-8);
            let u = (t - cum[i]) / seg;
            return pts[i].lerp(pts[i + 1], u);
        }
    }
    *pts.last().unwrap()
}

fn nearest_vert(
    mesh: &HalfEdgeMesh,
    ids: &[u32],
    p: Vec3,
    used: &HashSet<u32>,
) -> Option<u32> {
    let mut best: Option<(f32, u32)> = None;
    for &v in ids {
        if used.contains(&v) {
            continue;
        }
        let d = mesh.vert(VertexId(v)).position.distance_squared(p);
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, v));
        }
    }
    best.map(|(_, v)| v)
}

/// Split a single closed boundary loop into two rails (long sides of the strip) plus the
/// two short end chains (the profile arcs at each strip end — they collapse onto the
/// recovered sharp end points).
fn split_loop_into_rails(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    loop_v: &[u32],
) -> Option<(Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>)> {
    let n = loop_v.len();
    if n < 4 {
        return None;
    }
    // CORNERS ARE TOPOLOGICAL, NOT THE SHARPEST TURNS.
    //
    // A strip corner is a boundary vertex touched by exactly ONE strip face: the quad at the
    // end of the run contributes both of the boundary edges that meet there. Every other
    // boundary vertex sits between two strip faces — mid-rail (adjacent columns) and mid-arc
    // (adjacent segments) alike — and so does the vertex where the run BENDS.
    //
    // The old rule took the four largest turn angles. That is only true for a bevel of ONE
    // straight edge. The moment the beveled run turns a corner — the normal case on any real
    // model — the rail's bend is as sharp as the end caps and outranks them, so two of the
    // four "corners" landed mid-rail. The loop then split lopsided (measured on a cube:
    // rails 2/6, 7/2, 2/7 where both rails must be equal), `pair_rails_exclusive` sampled
    // `min(len)` pairs, and every unsewn rail vertex stayed behind as a hole — surfacing as
    // "opened holes" / "lost >2/3 of faces" / a self-loop pinch, i.e. the flat
    // "Unbevel: reverse math refused" the owner hit on every strip that was not a lone edge.
    let mut corners: Vec<usize> = (0..n)
        .filter(|&i| {
            let mut faces: HashSet<u32> = HashSet::new();
            for he in mesh.vertex_fan(VertexId(loop_v[i])) {
                let f = mesh.he(he).face;
                if f != NO_FACE && selected.contains(&f.0) {
                    faces.insert(f.0);
                }
            }
            faces.len() == 1
        })
        .collect();
    // A corner patch at a BEND (the triangle/n-gon a multi-edge bevel drops between two
    // columns) also touches the boundary alone, so the rule above yields a SUPERSET of the
    // four real corners — 6 on the owner's 16-face strip, 9 on their 3-face one. Pick the
    // four that satisfy the invariant every bevel strip's boundary obeys: it is a grid, so
    // OPPOSITE chains have equal length. The true corners minimise that imbalance; a bend
    // pretending to be a corner maximises it. Candidate sets are tiny (a strip has a handful
    // of these), so the exhaustive 4-subset search is cheaper than any heuristic.
    if corners.len() > 4 && corners.len() <= MAX_CORNER_CANDIDATES {
        let c = corners.clone();
        let mut best: Option<(usize, usize, [usize; 4])> = None;
        for a in 0..c.len() {
            for b in (a + 1)..c.len() {
                for d in (b + 1)..c.len() {
                    for e in (d + 1)..c.len() {
                        let pick = [c[a], c[b], c[d], c[e]];
                        // Chain lengths in edges, walking the loop in order.
                        let l = [
                            (pick[1] + n - pick[0]) % n,
                            (pick[2] + n - pick[1]) % n,
                            (pick[3] + n - pick[2]) % n,
                            (pick[0] + n - pick[3]) % n,
                        ];
                        if l.iter().any(|&x| x == 0) {
                            continue;
                        }
                        let imbalance = l[0].abs_diff(l[2]) + l[1].abs_diff(l[3]);
                        // Tie-break on the SHORTEST chain: a split that shaves one edge off
                        // an end cap ties on imbalance with the true split, and taking the
                        // larger minimum keeps whole chains.
                        let shortest = *l.iter().min().unwrap_or(&0);
                        if best.map_or(true, |(bi, bs, _)| {
                            imbalance < bi || (imbalance == bi && shortest > bs)
                        }) {
                            best = Some((imbalance, shortest, pick));
                        }
                    }
                }
            }
        }
        if let Some((_, _, pick)) = best {
            corners = pick.to_vec();
        }
    }
    if corners.len() != 4 {
        // Not a four-cornered strip (a branching selection, or a closed band that reached
        // here). Fall back to the turn-angle guess rather than inventing a split —
        // downstream gates still refuse anything it gets wrong.
        let mut turns: Vec<(usize, f32)> = Vec::with_capacity(n);
        for i in 0..n {
            let prev = mesh.vert(VertexId(loop_v[(i + n - 1) % n])).position;
            let cur = mesh.vert(VertexId(loop_v[i])).position;
            let next = mesh.vert(VertexId(loop_v[(i + 1) % n])).position;
            let e0 = (cur - prev).normalize_or_zero();
            let e1 = (next - cur).normalize_or_zero();
            let cross = e0.cross(e1).length();
            let dot = e0.dot(e1).clamp(-1.0, 1.0);
            let ang = cross.atan2(dot).abs();
            turns.push((i, ang));
        }
        turns.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if turns.len() < 4 {
            return None;
        }
        corners = turns.iter().take(4).map(|t| t.0).collect();
    }
    corners.sort_unstable();
    let mut chains: Vec<Vec<u32>> = Vec::new();
    for ci in 0..4 {
        let a = corners[ci];
        let b = corners[(ci + 1) % 4];
        let mut chain = Vec::new();
        let mut i = a;
        loop {
            chain.push(loop_v[i]);
            if i == b {
                break;
            }
            i = (i + 1) % n;
            if chain.len() > n + 2 {
                break;
            }
        }
        chains.push(chain);
    }
    // Opposite chains (0↔2 vs 1↔3): the RAILS are the pair the strip is NARROW between —
    // that is the bevel's profile, and collapsing it is the whole operation.
    //
    // Separation is measured as the mean NEAREST-vertex distance between the two chains, not
    // the mean of all pairwise distances. Mean-pairwise answers a different question: for two
    // parallel rails of run-length L a bevel-width w apart it returns O(L), because most pairs
    // it averages are far apart ALONG the strip, and it only stays below the end caps' own
    // separation while L is short. On the owner's 5-column strip it inverted — the caps
    // measured closer than the rails — so the sew ran lengthwise down the strip and tore it
    // open. Nearest-vertex distance returns w for the rails and L for the caps, which is the
    // narrow-direction question actually being asked, at any strip length.
    let score = |i: usize, j: usize| -> (usize, f32) {
        let len = chains[i].len() + chains[j].len();
        let mut sep = 0.0f32;
        let mut c = 0u32;
        for (x, y) in [(i, j), (j, i)] {
            for &va in &chains[x] {
                let pa = mesh.vert(VertexId(va)).position;
                let nearest = chains[y]
                    .iter()
                    .map(|&vb| pa.distance(mesh.vert(VertexId(vb)).position))
                    .fold(f32::INFINITY, f32::min);
                if nearest.is_finite() {
                    sep += nearest;
                    c += 1;
                }
            }
        }
        (len, if c > 0 { sep / c as f32 } else { 0.0 })
    };
    let s02 = score(0, 2);
    let s13 = score(1, 3);
    // Chain length only breaks separation ties.
    let use_02 = s02.1 < s13.1 || (s02.1 == s13.1 && s02.0 >= s13.0);
    let [c0, c1, c2, c3] = match <[Vec<u32>; 4]>::try_from(chains) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let (r0, r1, arc_a, arc_b) = if use_02 {
        (c0, c2, c1, c3)
    } else {
        (c1, c3, c0, c2)
    };
    if r0.len() < 2 || r1.len() < 2 {
        return None;
    }
    // The two end arcs are the SAME bevel profile, measured once at each end of the run, so
    // their arc lengths must be comparable. When they are not, the selection is not a single
    // two-railed strip and this four-corner model is describing something that isn't there.
    //
    // The case in hand is an N-WAY CORNER bevel — several beveled edges meeting at one vertex
    // (the owner's 16-face strip is three, at a box corner: three real 3-chord profile arcs
    // joined by rails through junction vertices, 6 corners, not 4). Forced through this model
    // it yields a mesh that is manifold, hole-free and the right bounding box, yet sews four
    // of its six rail pairs across ~1.5 units when the bevel chord is 0.235 — a sheared lie
    // that passes every downstream gate. Refuse instead: N-way corner unbevel needs its own
    // path (recover one sharp edge per beveled edge, all meeting at one recovered corner),
    // and a wrong result here is worse than none.
    let arc_len = |c: &[u32]| -> f32 {
        c.windows(2)
            .map(|w| {
                mesh.vert(VertexId(w[0]))
                    .position
                    .distance(mesh.vert(VertexId(w[1])).position)
            })
            .sum()
    };
    let (la, lb) = (arc_len(&arc_a), arc_len(&arc_b));
    let (lo, hi) = if la <= lb { (la, lb) } else { (lb, la) };
    if hi > lo * MAX_ARC_LENGTH_RATIO + 1e-6 {
        return None;
    }
    Some((r0, r1, arc_a, arc_b))
}

fn estimate_segments(mesh: &HalfEdgeMesh, selected: &HashSet<u32>) -> u32 {
    let mut is_boundary_face = HashSet::new();
    for &fid in selected {
        let start = mesh.face(FaceId(fid)).half_edge;
        if start == NO_EDGE {
            continue;
        }
        let mut he = start;
        loop {
            let h = mesh.he(he);
            let other_sel = if h.twin == NO_TWIN {
                false
            } else {
                let tf = mesh.he(h.twin).face;
                tf != NO_FACE && selected.contains(&tf.0)
            };
            if !other_sel {
                is_boundary_face.insert(fid);
                break;
            }
            he = h.next;
            if he == start {
                break;
            }
        }
    }
    if is_boundary_face.is_empty() {
        return 1;
    }
    let mut dist: HashMap<u32, u32> = HashMap::new();
    let mut q = VecDeque::new();
    for &f in &is_boundary_face {
        dist.insert(f, 0);
        q.push_back(f);
    }
    let mut max_d = 0u32;
    while let Some(f) = q.pop_front() {
        let d = dist[&f];
        max_d = max_d.max(d);
        let hs = mesh.face(FaceId(f)).half_edge;
        if hs == NO_EDGE {
            continue;
        }
        let mut he = hs;
        loop {
            let h = mesh.he(he);
            if h.twin != NO_TWIN {
                let tf = mesh.he(h.twin).face;
                if tf != NO_FACE && selected.contains(&tf.0) {
                    let nd = d + 1;
                    let e = dist.entry(tf.0).or_insert(u32::MAX);
                    if nd < *e {
                        *e = nd;
                        q.push_back(tf.0);
                    }
                }
            }
            he = h.next;
            if he == hs {
                break;
            }
        }
    }
    max_d.max(1)
}

fn find_edge_by_positions(mesh: &HalfEdgeMesh, p0: Vec3, p1: Vec3) -> Option<u32> {
    let mut best: Option<(f32, u32)> = None;
    for i in 0..mesh.half_edges.len() as u32 {
        let he = mesh.he(HalfEdgeId(i));
        if he.face == NO_FACE {
            continue;
        }
        if he.twin != NO_TWIN && i > he.twin.0 {
            continue;
        }
        let a = mesh.vert(mesh.he_source(HalfEdgeId(i))).position;
        let b = mesh.vert(he.vertex).position;
        let d1 = a.distance(p0) + b.distance(p1);
        let d2 = a.distance(p1) + b.distance(p0);
        let d = d1.min(d2);
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.and_then(|(d, e)| {
        let len = p0.distance(p1).max(1e-4);
        // Strict: both endpoints must nearly match (avoids picking diagonal / side edges).
        if d < len * 0.08 + 1e-4 {
            Some(e)
        } else {
            None
        }
    })
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bevel_op::{bevel_edges_offset, BevelParams};

    fn cube() -> HalfEdgeMesh {
        HalfEdgeMesh::new_box(Vec3::ONE)
    }

    fn first_interior_edge(m: &HalfEdgeMesh) -> u32 {
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.face == NO_FACE {
                continue;
            }
            if he.twin != NO_TWIN && i > he.twin.0 {
                continue;
            }
            if he.twin != NO_TWIN && m.he(he.twin).face != NO_FACE {
                return i;
            }
        }
        0
    }

    /// Undirected interior edges of `m`, canonical (min of each twin pair).
    fn interior_edges(m: &HalfEdgeMesh) -> Vec<u32> {
        (0..m.half_edges.len() as u32)
            .filter(|&i| {
                let he = m.he(HalfEdgeId(i));
                he.face != NO_FACE
                    && he.twin != NO_TWIN
                    && i < he.twin.0
                    && m.he(he.twin).face != NO_FACE
            })
            .collect()
    }

    /// Bevel `sel` at `segments`, then unbevel the exact faces the bevel created.
    fn round_trip(sel: &HashSet<u32>, segments: u32) -> Option<UnbevelResult> {
        let m = cube();
        let beveled = bevel_edges_offset(
            &m,
            sel,
            &BevelParams { width: 0.2, segments, shape: 0.5, clamp: true },
        );
        let (s, e) = crate::bevel_op::last_bevel_new_face_range().expect("bevel face range");
        let strip: HashSet<u32> = (s..e)
            .filter(|&f| {
                (f as usize) < beveled.faces.len() && beveled.face(FaceId(f)).half_edge != NO_EDGE
            })
            .collect();
        unbevel_from_faces(&beveled, &strip)
    }

    fn top_ring(m: &HalfEdgeMesh) -> Vec<u32> {
        let mut v: Vec<u32> = interior_edges(m)
            .into_iter()
            .filter(|&e| {
                let a = m.vert(m.he_source(HalfEdgeId(e))).position.y;
                let b = m.vert(m.he(HalfEdgeId(e)).vertex).position.y;
                (a - 1.0).abs() < 1e-4 && (b - 1.0).abs() < 1e-4
            })
            .collect();
        v.sort_unstable();
        v
    }

    /// A beveled edge RUN that turns a corner must unbevel — the case that made the owner
    /// report "Unbevel does not work" (2026-08-11), and the case every real model produces.
    ///
    /// The strip's four corners used to be taken as the four SHARPEST TURNS in its boundary
    /// loop, which is only correct for a bevel of one straight edge. As soon as the run bends,
    /// the rail's own bend is as sharp as an end cap and outranks it, so the loop split
    /// lopsided (rails 2/6 where both rails must be equal), `pair_rails_exclusive` sewed only
    /// `min(len)` pairs, and every unsewn rail vertex stayed put as a hole. Corners are found
    /// topologically now, so this sweep — straight, bent, open runs and closed rings, at 1..5
    /// segments — must restore the exact cube every time.
    #[test]
    fn unbevel_restores_the_cube_for_every_strip_shape() {
        let m = cube();
        let edges = interior_edges(&m);
        let ring = top_ring(&m);

        let a = edges[0];
        let av1 = m.he(HalfEdgeId(a)).vertex.0;
        let bent: HashSet<u32> = HashSet::from([
            a,
            edges
                .iter()
                .copied()
                .find(|&e| {
                    e != a
                        && (m.he_source(HalfEdgeId(e)).0 == av1
                            || m.he(HalfEdgeId(e)).vertex.0 == av1)
                })
                .expect("an edge sharing a vertex with the first"),
        ]);

        let cases: Vec<(&str, HashSet<u32>, usize)> = vec![
            ("one straight edge", HashSet::from([a]), 1),
            ("run bending through a corner", bent, 2),
            ("open run of three ring edges", ring.iter().copied().take(3).collect(), 3),
            ("closed top ring", ring.iter().copied().collect(), 4),
        ];

        for (name, sel, want_sharp) in cases {
            for segments in [1u32, 2, 3, 4, 5] {
                let r = round_trip(&sel, segments)
                    .unwrap_or_else(|| panic!("{name}, seg={segments}: unbevel refused"));
                assert_eq!(
                    r.mesh.face_count(),
                    6,
                    "{name}, seg={segments}: want the 6-face cube back"
                );
                assert_eq!(
                    r.mesh.vertices.len(),
                    8,
                    "{name}, seg={segments}: want the 8-vertex cube back"
                );
                assert_eq!(
                    r.sharp_edges.len(),
                    want_sharp,
                    "{name}, seg={segments}: recovered sharp edge count"
                );
                assert!(
                    r.mesh.is_manifold().is_ok(),
                    "{name}, seg={segments}: must stay manifold"
                );
                for v in &r.mesh.vertices {
                    let p = v.position;
                    for c in [p.x, p.y, p.z] {
                        assert!(
                            (c.abs() - 1.0).abs() < 1e-3,
                            "{name}, seg={segments}: corner {p:?} is off the ±1 cube"
                        );
                    }
                }
            }
        }
    }

    /// The result must not depend on Rust's per-process hash seed. The boundary walk was fed
    /// by HashSet iteration, so the same strip could unbevel cleanly in one process and refuse
    /// in the next — unreproducible, and impossible to bisect a refusal against. Within one
    /// process the seed is fixed, so this asserts the weaker but still load-bearing property
    /// that the walk is a pure function of the mesh: repeated calls agree exactly.
    #[test]
    fn unbevel_is_deterministic() {
        let m = cube();
        let sel: HashSet<u32> = top_ring(&m).into_iter().take(3).collect();
        let first = round_trip(&sel, 3).expect("unbevel");
        for _ in 0..8 {
            let again = round_trip(&sel, 3).expect("unbevel");
            assert_eq!(first.mesh.vertices.len(), again.mesh.vertices.len());
            assert_eq!(first.mesh.face_count(), again.mesh.face_count());
            assert_eq!(first.sharp_edges.len(), again.sharp_edges.len());
            let (mn0, mx0) = first.mesh.bounding_box();
            let (mn1, mx1) = again.mesh.bounding_box();
            assert!(
                mn0.abs_diff_eq(mn1, 1e-6) && mx0.abs_diff_eq(mx1, 1e-6),
                "same input must give the same mesh"
            );
        }
    }

    /// An N-WAY CORNER strip — several beveled edges meeting at one vertex — is NOT a
    /// two-railed strip: its boundary carries one profile arc per edge (three, on a box
    /// corner), not two. It must REFUSE rather than be forced through the four-corner model,
    /// which yields a mesh that is manifold, hole-free and the right size while sewing rail
    /// pairs across ~6x the bevel chord — a sheared lie that clears every downstream gate.
    /// Owner file `UNBEVEL TEST.fl4` (2026-08-11), 16-face strip. Per-edge unbevel from a rail
    /// edge still works and is the route until the N-way path exists.
    #[test]
    fn unbevel_refuses_an_n_way_corner_strip_instead_of_shearing_it() {
        let m = cube();
        let edges = interior_edges(&m);
        // The three edges meeting at one cube corner.
        let corner_v = m.he(HalfEdgeId(edges[0])).vertex.0;
        let three: HashSet<u32> = edges
            .iter()
            .copied()
            .filter(|&e| {
                m.he_source(HalfEdgeId(e)).0 == corner_v || m.he(HalfEdgeId(e)).vertex.0 == corner_v
            })
            .collect();
        assert_eq!(three.len(), 3, "a cube corner joins exactly three edges");

        let beveled = bevel_edges_offset(
            &m,
            &three,
            &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true },
        );
        let (s, e) = crate::bevel_op::last_bevel_new_face_range().expect("bevel face range");
        let strip: HashSet<u32> = (s..e)
            .filter(|&f| {
                (f as usize) < beveled.faces.len() && beveled.face(FaceId(f)).half_edge != NO_EDGE
            })
            .collect();

        if let Some(r) = unbevel_from_faces(&beveled, &strip) {
            // If a future N-way path lands, it must produce the real corner back — never the
            // sheared mesh the four-corner model produced. Bevelling three edges of a cube
            // corner and undoing it can only mean the cube.
            assert_eq!(r.mesh.face_count(), 6, "an N-way unbevel must restore the cube");
            assert_eq!(r.mesh.vertices.len(), 8, "an N-way unbevel must restore the cube");
        }
    }

    /// THE reverse-math gate: bevel one cube edge (seg=3), unbevel from the strip faces
    /// alone (no history), and the ORIGINAL cube must come back EXACTLY — 8 corners at ±1,
    /// 6 quads, measured width ≈ the bevel's width. The old midpoint sew can never pass
    /// this: it left every recovered corner shaved inward by ~w/√2 and kept the end-cap
    /// profile arcs.
    #[test]
    fn unbevel_reverse_math_restores_exact_cube() {
        let m = cube();
        let e = first_interior_edge(&m);
        let beveled = bevel_edges_offset(
            &m,
            &HashSet::from([e]),
            &BevelParams {
                width: 0.2,
                segments: 3,
                shape: 0.5,
                clamp: true,
            },
        );
        let (s, end) =
            crate::bevel_op::last_bevel_new_face_range().expect("bevel face range");
        let seed: HashSet<u32> = (s..end).collect();
        let r = unbevel_from_faces(&beveled, &seed).expect("reverse-math unbevel");
        assert_eq!(
            r.mesh.face_count(),
            6,
            "exact cube restore: 6 faces, got {}",
            r.mesh.face_count()
        );
        assert_eq!(
            r.mesh.vertices.len(),
            8,
            "exact cube restore: 8 verts, got {}",
            r.mesh.vertices.len()
        );
        for v in &r.mesh.vertices {
            let p = v.position;
            for c in [p.x, p.y, p.z] {
                assert!(
                    (c.abs() - 1.0).abs() < 1e-3,
                    "recovered corner {p:?} not on the ±1 cube"
                );
            }
        }
        assert!(
            (r.estimated_width - 0.2).abs() < 0.03,
            "measured width {} should recover the bevel's 0.2",
            r.estimated_width
        );
        assert!(!r.sharp_edges.is_empty());
    }

    /// Owner 2026-08-09: in edge mode the user clicks the rounded corner they can SEE — one
    /// RAIL (lengthwise) edge of the strip, not its cross edges and not its faces. Seeds 1+2
    /// both refuse that; the rail run must recover the same exact cube the face seed does.
    #[test]
    fn unbevel_from_one_rail_edge_restores_exact_cube() {
        let m = cube();
        let e = first_interior_edge(&m);
        let beveled = bevel_edges_offset(
            &m,
            &HashSet::from([e]),
            &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true },
        );
        let (s, end) = crate::bevel_op::last_bevel_new_face_range().expect("bevel face range");
        let strip: HashSet<u32> = (s..end).collect();

        // A rail: strip boundary edge running the length of the beveled cube edge (long),
        // as opposed to the short end-cap edges.
        let mut rail = None;
        for &f in &strip {
            for h in beveled.face_half_edges(FaceId(f)) {
                let he = beveled.he(h);
                let outside = he.twin == NO_TWIN
                    || beveled.he(he.twin).face == NO_FACE
                    || !strip.contains(&beveled.he(he.twin).face.0);
                if !outside {
                    continue;
                }
                let a = beveled.vert(beveled.he_source(h)).position;
                let b = beveled.vert(he.vertex).position;
                if (a - b).length() > 1.0 {
                    rail = Some(h.0);
                }
            }
        }
        let rail = rail.expect("strip must have a long rail edge");

        let r = unbevel_from_edges(&beveled, &HashSet::from([rail]))
            .expect("rail-edge unbevel must find the strip");
        assert_eq!(r.mesh.face_count(), 6, "rail unbevel: 6 faces");
        assert_eq!(r.mesh.vertices.len(), 8, "rail unbevel: 8 verts");
        for v in &r.mesh.vertices {
            let p = v.position;
            for c in [p.x, p.y, p.z] {
                assert!((c.abs() - 1.0).abs() < 1e-3, "recovered corner {p:?} off the ±1 cube");
            }
        }
        assert!(r.mesh.is_manifold().is_ok(), "rail unbevel must stay manifold");
    }

    #[test]
    fn unbevel_single_edge_bevel_restores_and_rebevels() {
        let m = cube();
        let e = first_interior_edge(&m);
        let mut sel = HashSet::new();
        sel.insert(e);
        let beveled = bevel_edges_offset(
            &m,
            &sel,
            &BevelParams {
                width: 0.2,
                segments: 1,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(beveled.faces.len() > m.faces.len());

        let he = m.he(HalfEdgeId(e));
        let a = m.vert(m.he_source(HalfEdgeId(e))).position;
        let b = m.vert(he.vertex).position;
        let mid = (a + b) * 0.5;
        let edge_dir = (b - a).normalize_or_zero();
        let mut seed = HashSet::new();
        for fi in 0..beveled.faces.len() as u32 {
            if beveled.face(FaceId(fi)).half_edge == NO_EDGE {
                continue;
            }
            let verts = beveled.face_vertices(FaceId(fi));
            if verts.len() != 4 {
                continue;
            }
            let c: Vec3 = verts
                .iter()
                .map(|v| beveled.vert(*v).position)
                .sum::<Vec3>()
                / verts.len().max(1) as f32;
            let to = c - mid;
            let lateral = (to - edge_dir * to.dot(edge_dir)).length();
            if lateral < 0.35 && c.distance(mid) < 1.2 {
                seed.insert(fi);
            }
        }
        assert!(!seed.is_empty());

        let result = unbevel_from_faces(&beveled, &seed).expect("unbevel should succeed");
        assert!(!result.sharp_edges.is_empty());

        let again = bevel_edges_offset(
            &result.mesh,
            &result.sharp_edges,
            &BevelParams {
                width: result.estimated_width.max(0.05).min(0.25),
                segments: 1,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(again.is_manifold().is_ok());
        assert!(again.faces.len() >= 6);
    }

    /// Stacked bevel: single edge then multi-edge on the result — face-range topology
    /// guess must either restore a clean closed mesh or refuse (never install a pyramid wreck).
    #[test]
    fn unbevel_stacked_multi_seg_refuses_or_stays_closed() {
        let cube = cube();
        let e0 = first_interior_edge(&cube);
        let p = BevelParams {
            width: 0.28,
            segments: 3,
            shape: 0.5,
            clamp: true,
        };
        let m1 = bevel_edges_offset(&cube, &HashSet::from([e0]), &p);
        assert!(m1.is_manifold().is_ok());
        // Five undirected interior edges on m1.
        let mut edges = HashSet::new();
        for i in 0..m1.half_edges.len() as u32 {
            if edges.len() >= 5 {
                break;
            }
            let he = m1.he(HalfEdgeId(i));
            if he.face == NO_FACE || he.twin == NO_TWIN || i > he.twin.0 {
                continue;
            }
            edges.insert(i);
        }
        assert_eq!(edges.len(), 5);
        let m2 = bevel_edges_offset(
            &m1,
            &edges,
            &BevelParams {
                width: 0.2,
                segments: 3,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(m2.is_manifold().is_ok(), "precondition: stacked bevel manifold");
        let Some((s, e)) = crate::bevel_op::last_bevel_new_face_range() else {
            panic!("expected new face range from second bevel");
        };
        let seed: HashSet<u32> = (s..e).collect();
        assert!(!seed.is_empty());
        match unbevel_from_faces(&m2, &seed) {
            None => {
                // Honest refuse is OK — app falls back to history.
            }
            Some(r) => {
                assert!(
                    r.mesh.is_manifold().is_ok(),
                    "installed unbevel must stay manifold: {:?}",
                    r.mesh.is_manifold()
                );
                assert_eq!(
                    crate::bevel_op::boundary_loop_count(&r.mesh),
                    0,
                    "must not open holes"
                );
                assert!(
                    r.mesh.face_count() >= 4,
                    "must not collapse to a pyramid wreck (faces={})",
                    r.mesh.face_count()
                );
            }
        }
    }

    #[test]
    fn unbevel_top_loop_multi_seg_stays_boxy() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || i > he.twin.0 {
                continue;
            }
            let a = cube.he_source(HalfEdgeId(i)).0;
            let b = he.vertex.0;
            let ya = cube.vert(VertexId(a)).position.y;
            let yb = cube.vert(VertexId(b)).position.y;
            if (ya - 1.0).abs() < 1e-4 && (yb - 1.0).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert!(!sel.is_empty());
        let beveled = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams {
                width: 0.2,
                segments: 3,
                shape: 0.5,
                clamp: true,
            },
        );
        // Seed: faces near y=0.8..1.0 (the top rim strip)
        let mut seed = HashSet::new();
        for fi in 0..beveled.faces.len() as u32 {
            if beveled.face(FaceId(fi)).half_edge == NO_EDGE {
                continue;
            }
            let verts = beveled.face_vertices(FaceId(fi));
            let c: Vec3 = verts
                .iter()
                .map(|v| beveled.vert(*v).position)
                .sum::<Vec3>()
                / verts.len().max(1) as f32;
            if c.y > 0.55 && c.y < 1.05 {
                // exclude pure top face (centroid y near 1 and |x|,|z| small)
                let on_top_cap = c.y > 0.95 && c.x.abs() < 0.7 && c.z.abs() < 0.7;
                if !on_top_cap {
                    seed.insert(fi);
                }
            }
        }
        assert!(seed.len() >= 4, "expected strip faces, got {}", seed.len());
        let result = unbevel_from_faces(&beveled, &seed).expect("top-loop unbevel");
        // Recovered mesh should be roughly a box: bbox near original
        let (mn, mx) = result.mesh.bounding_box();
        let size = mx - mn;
        assert!(
            size.x > 1.5 && size.x < 2.5 && size.y > 1.5 && size.y < 2.5 && size.z > 1.5 && size.z < 2.5,
            "bbox size {:?}",
            size
        );
        assert!(!result.sharp_edges.is_empty());
        // Re-bevel at modest width must stay manifold and not explode vertex count
        let again = bevel_edges_offset(
            &result.mesh,
            &result.sharp_edges,
            &BevelParams {
                width: result.estimated_width.min(0.15).max(0.05),
                segments: 1,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(again.is_manifold().is_ok());
        assert!(
            again.vertices.len() < beveled.vertices.len() * 3,
            "re-bevel exploded verts"
        );
    }
}
