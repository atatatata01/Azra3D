//! Edge-loop selection — a PURE query over the public half-edge API.
//!
//! Independent implementation from the topological definition of an edge loop: starting at a seed
//! edge, keep crossing vertices along the edge that continues "straight through" them.
//!   - valence 2: the only other edge;
//!   - valence 4 (interior): the edge that shares no face with the incoming one;
//!   - valence 3 on the rim of a large n-gon (a cap): the next edge of that n-gon (the cap is
//!     latched once, from the seed, so a stray n-gon cannot redirect an ordinary quad loop);
//!   - seed on a boundary: follow the boundary rim.
//! The walk stops at poles, wrong valences, already-visited edges, and when the loop closes.
//! Results are compared against Blender's loop select as a behavioural oracle only.

use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, FaceId, NO_TWIN, NO_FACE};
use std::collections::HashSet;

/// Number of incident faces around vertex `v`, counting each distinct face once.
fn vert_face_count(mesh: &HalfEdgeMesh, v: u32) -> usize {
    let mut faces: HashSet<u32> = HashSet::new();
    for h in loop_neighbors(mesh, crate::half_edge::VertexId(v)) {
        for f in edge_faces(mesh, h) {
            if f != NO_FACE {
                faces.insert(f.0);
            }
        }
    }
    faces.len()
}

#[inline]
fn valence(mesh: &HalfEdgeMesh, v: u32) -> usize {
    mesh.vertex_fan(crate::half_edge::VertexId(v)).len()
}

/// Cap-face latch for a seed edge. A seed on the rim of a large polygon (the polygon has more than
/// four sides, the edge is interior, and at least one end is a closed valence-3 corner) walks that
/// polygon's boundary; any other seed gets no latch. Fixing this per seed (never per vertex) stops a
/// stray n-gon beside an ordinary quad loop from redirecting it.
fn seed_hub(mesh: &HalfEdgeMesh, start: HalfEdgeId) -> Option<FaceId> {
    let [fa, fb] = edge_faces(mesh, start);
    if fa == NO_FACE || fb == NO_FACE {
        return None;
    }
    let sides = |f: FaceId| mesh.face_half_edges(f).len();
    let big = if sides(fa) >= sides(fb) { fa } else { fb };
    if sides(big) <= 4 {
        return None;
    }
    let closed_corner = |v: u32| valence(mesh, v) == 3 && vert_face_count(mesh, v) == 3;
    let a = mesh.he_source(start).0;
    let b = mesh.he(start).vertex.0;
    (closed_corner(a) || closed_corner(b)).then_some(big)
}


/// MESHmachine LSelect / Loop Select tool parameters (see machin3.io LSelect + Select).
///
/// * `min_angle_deg` — maximum turn from straight (degrees) allowed when continuing.
///   Higher values grow the selection (MESHmachine 0.8+ UX). Default ~41–60°.
/// * `max_steps` — when set, cap the walk length **per direction** (Use Edge Limit + Steps).
#[derive(Clone, Copy, Debug)]
pub struct LoopSelectParams {
    pub min_angle_deg: f32,
    pub max_steps: Option<u32>,
}

impl Default for LoopSelectParams {
    fn default() -> Self {
        Self {
            min_angle_deg: 41.0,
            max_steps: None,
        }
    }
}

/// Cosine of the max turn from straight allowed by `min_angle_deg`.
#[inline]
fn min_angle_cos(min_angle_deg: f32) -> f32 {
    min_angle_deg.clamp(0.0, 179.0).to_radians().cos()
}

/// Straightest outgoing half-edge at the head of `cur` (for hard-surface junctions where
/// valence ≠ 4). Candidate must stay within `min_angle_deg` of the incoming direction.
/// Used by directed loop, plain `edge_loop`, and LSelect.
fn straightest_out_angle(mesh: &HalfEdgeMesh, cur: HalfEdgeId, min_angle_deg: f32) -> Option<HalfEdgeId> {
    let v = mesh.he(cur).vertex;
    let pv = mesh.vert(v).position;
    let pin = mesh.vert(mesh.he_source(cur)).position;
    let din = (pv - pin).normalize_or_zero();
    if din.length_squared() < 1e-12 {
        return None;
    }
    let floor = min_angle_cos(min_angle_deg);
    let tw = mesh.he(cur).twin;
    let cur_k = {
        let t = tw;
        if t != NO_TWIN {
            cur.0.min(t.0)
        } else {
            cur.0
        }
    };
    let mut best: Option<(f32, HalfEdgeId)> = None;
    for h in loop_neighbors(mesh, v) {
        // Prefer edges leaving `v` (source = v). Fan may return either orientation.
        let out = if mesh.he_source(h).0 == v.0 {
            h
        } else if mesh.he(h).vertex.0 == v.0 {
            let t = mesh.he(h).twin;
            if t == NO_TWIN {
                continue;
            }
            t
        } else {
            continue;
        };
        let ht = mesh.he(out).twin;
        let hk = if ht != NO_TWIN { out.0.min(ht.0) } else { out.0 };
        if hk == cur_k {
            continue;
        }
        let pout = mesh.vert(mesh.he(out).vertex).position;
        let dout = (pout - pv).normalize_or_zero();
        if dout.length_squared() < 1e-12 {
            continue;
        }
        let dot = din.dot(dout);
        // Require turn ≤ min_angle (dot ≥ cos(min_angle)); pick straightest among those.
        if dot >= floor && best.map_or(true, |(bd, _)| dot > bd) {
            best = Some((dot, out));
        }
    }
    best.map(|(_, h)| h)
}

/// Straightest out with the historical floor (`dot > 0.2` ≈ 78°) for Blender-parity walks.
///
/// Uses neighbour half-edges **as returned by the fan/scan** (no reorientation). That matches the
/// pre-LSelect behaviour: at a high-valence pole the fan often hands edges that point at the hub,
/// so the score fails and the plain loop STOPS — which is what Blender does and what
/// `directed_loop_crosses_pole_straight` pins. Directed walk + LSelect use
/// [`straightest_out_angle`], which orients candidates correctly so hard-surface belts continue.
fn straightest_out(mesh: &HalfEdgeMesh, cur: HalfEdgeId) -> Option<HalfEdgeId> {
    let v = mesh.he(cur).vertex;
    let pv = mesh.vert(v).position;
    let pin = mesh.vert(mesh.he_source(cur)).position;
    let din = (pv - pin).normalize_or_zero();
    if din.length_squared() < 1e-12 {
        return None;
    }
    let tw = mesh.he(cur).twin;
    let cur_k = {
        let t = tw;
        if t != NO_TWIN {
            cur.0.min(t.0)
        } else {
            cur.0
        }
    };
    let mut best: Option<(f32, HalfEdgeId)> = None;
    for h in loop_neighbors(mesh, v) {
        let ht = mesh.he(h).twin;
        let hk = if ht != NO_TWIN { h.0.min(ht.0) } else { h.0 };
        if hk == cur_k {
            continue;
        }
        let pout = mesh.vert(mesh.he(h).vertex).position;
        let dout = (pout - pv).normalize_or_zero();
        let dot = din.dot(dout);
        if dot > 0.2 && best.map_or(true, |(bd, _)| dot > bd) {
            best = Some((dot, h));
        }
    }
    best.map(|(_, h)| h)
}

/// Undirected edge key (lower half-edge id of a twin pair).
#[inline]
fn undirected_key(mesh: &HalfEdgeMesh, h: HalfEdgeId) -> u32 {
    let t = mesh.he(h).twin;
    if t != NO_TWIN {
        h.0.min(t.0)
    } else {
        h.0
    }
}

/// Select the edge LOOP through `start` (both directions). Returns representative half-edge ids, one per
/// undirected edge, including `start`.
///
/// **Topology-first** (valence rules first, straightest fallback at ~78°). For MESHmachine-style
/// Min Angle / Steps control use [`edge_loop_with_params`] or [`edge_loop_lselect`].
///
/// At poles / non-valence-4 junctions (bevel corners, hard-surface T-junctions), continues along
/// the straightest edge so a mid-belt double-click selects the full closed loop rather than
/// stopping after one segment (owner 1234.fl4).
pub fn edge_loop(mesh: &HalfEdgeMesh, start: HalfEdgeId) -> Vec<HalfEdgeId> {
    if (start.0 as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(start) {
        return Vec::new();
    }
    let key = |h: HalfEdgeId| undirected_key(mesh, h);
    let is_boundary = mesh.he(start).twin == NO_TWIN;
    let f_hub = seed_hub(mesh, start);

    let mut result: Vec<HalfEdgeId> = vec![start];
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(key(start));

    for &dir in &[start, mesh.he(start).twin] {
        if dir == NO_TWIN {
            continue;
        }
        let mut cur = dir;
        let mut hub = f_hub;
        for _ in 0..mesh.half_edges.len().max(1) {
            let step = next_loop_edge(mesh, cur, is_boundary, hub)
                .map(|(n, h)| (n, h))
                .or_else(|| straightest_out(mesh, cur).map(|n| (n, hub)));
            match step {
                Some((nxt, new_hub)) => {
                    let k = key(nxt);
                    if seen.contains(&k) {
                        break;
                    }
                    seen.insert(k);
                    result.push(nxt);
                    cur = nxt;
                    hub = new_hub;
                }
                None => break,
            }
        }
    }
    result
}

/// Topological loop + MESHmachine LSelect angle/step limits.
///
/// Topology step is preferred when it continues within `min_angle_deg` of straight; otherwise
/// (or when topology stops) the angle-based straightest candidate is used. Hard-surface /
/// non-all-quad meshes match MeshMachine better than the pure topological walk.
///
/// Prefer this for **Alt+click** loop select.
pub fn edge_loop_with_params(
    mesh: &HalfEdgeMesh,
    start: HalfEdgeId,
    params: LoopSelectParams,
) -> Vec<HalfEdgeId> {
    if (start.0 as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(start) {
        return Vec::new();
    }
    let key = |h: HalfEdgeId| undirected_key(mesh, h);
    let is_boundary = mesh.he(start).twin == NO_TWIN;
    let f_hub = seed_hub(mesh, start);
    // Slightly more permissive than min_angle for pure topology: accept the opposite-edge
    // step up to max(min_angle, 50°) so mild curved belts still complete, then fall back to
    // angle walk. MeshMachine LSelect alone uses pure angle (see edge_loop_lselect).
    let topo_floor = min_angle_cos(params.min_angle_deg.max(50.0));

    let mut result: Vec<HalfEdgeId> = vec![start];
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(key(start));

    for &dir in &[start, mesh.he(start).twin] {
        if dir == NO_TWIN {
            continue;
        }
        let mut cur = dir;
        let mut hub = f_hub;
        let mut steps_taken = 0u32;
        let max_iter = mesh.half_edges.len().max(1);
        for _ in 0..max_iter {
            if let Some(max) = params.max_steps {
                if steps_taken >= max {
                    break;
                }
            }
            let topo = next_loop_edge(mesh, cur, is_boundary, hub);
            let step = match topo {
                Some((n, h)) if turn_dot(mesh, cur, n) >= topo_floor => Some((n, h)),
                Some(_) | None => {
                    straightest_out_angle(mesh, cur, params.min_angle_deg).map(|n| (n, hub))
                }
            };
            match step {
                Some((nxt, new_hub)) => {
                    let k = key(nxt);
                    if seen.contains(&k) {
                        break;
                    }
                    seen.insert(k);
                    result.push(nxt);
                    cur = nxt;
                    hub = new_hub;
                    steps_taken += 1;
                }
                None => break,
            }
        }
    }
    result
}

/// Dot product of incoming direction along `cur` with outgoing direction of `nxt`
/// (both leaving the shared vertex). 1 = collinear continuation, 0 = 90° turn.
fn turn_dot(mesh: &HalfEdgeMesh, cur: HalfEdgeId, nxt: HalfEdgeId) -> f32 {
    let hub = mesh.he(cur).vertex;
    let pin = mesh.vert(mesh.he_source(cur)).position;
    let pv = mesh.vert(hub).position;
    let din = (pv - pin).normalize_or_zero();
    // Orient nxt so it leaves hub.
    let (a, b) = if mesh.he_source(nxt).0 == hub.0 {
        (hub, mesh.he(nxt).vertex)
    } else if mesh.he(nxt).vertex.0 == hub.0 {
        (hub, mesh.he_source(nxt))
    } else {
        return -1.0;
    };
    let dout = (mesh.vert(b).position - mesh.vert(a).position).normalize_or_zero();
    if din.length_squared() < 1e-12 || dout.length_squared() < 1e-12 {
        return -1.0;
    }
    din.dot(dout)
}

/// MESHmachine **LSelect**: pure angle-based chain walk (no valence-4 requirement).
/// Better than the pure topological walk on non-all-quad hard-surface meshes.
///
/// Walks both directions from `start`. At each vertex picks the unvisited neighbour whose
/// direction is closest to the incoming rail and within `min_angle_deg` of straight.
pub fn edge_loop_lselect(
    mesh: &HalfEdgeMesh,
    start: HalfEdgeId,
    params: LoopSelectParams,
) -> Vec<HalfEdgeId> {
    if (start.0 as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(start) {
        return Vec::new();
    }
    let key = |h: HalfEdgeId| undirected_key(mesh, h);
    let mut result: Vec<HalfEdgeId> = vec![start];
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(key(start));

    for &dir in &[start, mesh.he(start).twin] {
        if dir == NO_TWIN {
            continue;
        }
        let mut cur = dir;
        let mut steps_taken = 0u32;
        let max_iter = mesh.half_edges.len().max(1);
        for _ in 0..max_iter {
            if let Some(max) = params.max_steps {
                if steps_taken >= max {
                    break;
                }
            }
            let Some(nxt) = straightest_out_angle(mesh, cur, params.min_angle_deg) else {
                break;
            };
            let k = key(nxt);
            if !seen.insert(k) {
                break;
            }
            result.push(nxt);
            cur = nxt;
            steps_taken += 1;
        }
    }
    result
}

/// Edge RING through `start` (both directions) — Blender Alt+Ctrl / Ctrl+Alt click.
///
/// Uses opposite-on-face (quad + n-gon floor(n/2)) so boolean / hard-surface strips still walk.
/// Optional `max_steps` caps each direction (shared Loop Select "Use Edge Limit" / Steps).
pub fn edge_ring_with_params(
    mesh: &HalfEdgeMesh,
    start: HalfEdgeId,
    max_steps: Option<u32>,
) -> Vec<HalfEdgeId> {
    if (start.0 as usize) >= mesh.half_edges.len() || !mesh.is_edge_live(start) {
        return Vec::new();
    }
    let key = |h: HalfEdgeId| undirected_key(mesh, h);
    let mut result: Vec<HalfEdgeId> = vec![start];
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(key(start));

    // Both orientations of the seed rail so open rings walk both ways.
    for &seed in &[start, mesh.he(start).twin] {
        if seed == NO_TWIN || !mesh.is_edge_live(seed) {
            continue;
        }
        let mut cur = seed;
        let mut steps_taken = 0u32;
        let max_iter = mesh.half_edges.len().max(1);
        for _ in 0..max_iter {
            if let Some(max) = max_steps {
                if steps_taken >= max {
                    break;
                }
            }
            let face = mesh.he(cur).face;
            if face == NO_FACE {
                break;
            }
            let Some(opp) = opposite_on_face(mesh, face, cur) else {
                break;
            };
            let tw = mesh.he(opp).twin;
            if tw == NO_TWIN {
                break;
            }
            let k = key(tw);
            if !seen.insert(k) {
                break;
            }
            result.push(tw);
            cur = tw;
            steps_taken += 1;
        }
    }
    result
}

/// Blender **Select Next Active** for vertices (`mesh.select_next_item`).
///
/// Oracle (Blender Manual → Select Menu → More/Less → Next Active / Previous Active):
/// "Selects the element that logically follows the two most recently selected ones."
/// Direction is the ordered pair `prev → active` from selection history. From `active`, the
/// next vertex is the unselected neighbour that best continues that direction (highest
/// forward alignment). On a clean path (exactly one remaining neighbour after excluding
/// `prev`) that neighbour is taken even if the turn is sharp — Blender walks the path, not
/// only straight belts.
///
/// Returns `None` when there is no topological neighbour to step to (dead end / isolated).
pub fn vert_next_active(mesh: &HalfEdgeMesh, prev: u32, active: u32) -> Option<u32> {
    use crate::half_edge::VertexId;
    if prev == active {
        return None;
    }
    if (prev as usize) >= mesh.vertices.len() || (active as usize) >= mesh.vertices.len() {
        return None;
    }
    let p_prev = mesh.vert(VertexId(prev)).position;
    let p_act = mesh.vert(VertexId(active)).position;
    let din = (p_act - p_prev).normalize_or_zero();

    // Neighbours of `active`. Union the radial fan AND a full outgoing scan: the fan alone
    // undercounts at boundaries (breaks on NO_TWIN), which made Next Active jump the wrong
    // way on strip / hard-surface rims (owner Shift+scroll "next in line").
    //
    // Half-edges may be outgoing (source = active) or fan-style ending at active — resolve the
    // far endpoint relative to `active` so both conventions work.
    let mut candidates: Vec<u32> = Vec::new();
    let mut seen_he: HashSet<u32> = HashSet::new();
    let mut push_he = |h: HalfEdgeId| {
        if !seen_he.insert(h.0) {
            return;
        }
        let src = mesh.he_source(h).0;
        let dst = mesh.he(h).vertex.0;
        let n = if src == active {
            dst
        } else if dst == active {
            src
        } else {
            let t = mesh.he(h).twin;
            if t != NO_TWIN {
                mesh.he(t).vertex.0
            } else {
                return;
            }
        };
        if n == prev || n == active {
            return;
        }
        if !candidates.contains(&n) {
            candidates.push(n);
        }
    };
    for h in mesh.vertex_fan(VertexId(active)) {
        push_he(h);
    }
    for h in outgoing_at_vertex(mesh, active) {
        push_he(h);
    }
    if candidates.is_empty() {
        return None;
    }
    // Single remaining path neighbour → take it (Blender walks the chain through corners).
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    // Branch: prefer the neighbour most aligned with prev→active.
    // If the incoming direction is degenerate (coincident verts), fall back to any neighbour.
    if din.length_squared() < 1e-12 {
        return candidates.into_iter().next();
    }
    let mut best: Option<(f32, u32)> = None;
    for n in candidates {
        let pn = mesh.vert(VertexId(n)).position;
        let dout = (pn - p_act).normalize_or_zero();
        if dout.length_squared() < 1e-12 {
            continue;
        }
        let dot = din.dot(dout);
        if best.map_or(true, |(bd, _)| dot > bd) {
            best = Some((dot, n));
        }
    }
    best.map(|(_, n)| n)
}

/// Blender **Select Next Active** for edges: ordered history pair `prev → active` orients a
/// directed edge-loop walk; return the first still-unselected undirected edge past `active`.
///
/// `is_selected` must treat an edge and its twin as the same undirected selection (caller
/// supplies twin-aware membership). Returns a representative half-edge id of the next edge.
pub fn edge_next_active(
    mesh: &HalfEdgeMesh,
    prev: HalfEdgeId,
    active: HalfEdgeId,
    is_selected: &dyn Fn(HalfEdgeId) -> bool,
) -> Option<HalfEdgeId> {
    let key = |h: HalfEdgeId| -> u32 {
        let t = mesh.he(h).twin;
        if t != NO_TWIN {
            h.0.min(t.0)
        } else {
            h.0
        }
    };
    let lp = edge_loop_directed(mesh, prev, active);
    let act_k = key(active);
    let pos = lp.iter().position(|&h| key(h) == act_k);
    if let Some(i) = pos {
        lp.iter()
            .skip(i + 1)
            .find(|&&h| !is_selected(h))
            .copied()
    } else {
        lp.iter().find(|&&h| !is_selected(h)).copied()
    }
}

/// DIRECTED edge loop (Blender-parity request, owner 2026-07-23): when two loop directions
/// exist at a junction, a SECOND selected edge drives the direction. `prev` = the already-
/// selected driver edge, `seed` = the loop-clicked edge; they must share a vertex (else this
/// falls back to the plain undirected loop). The walk runs one way — out of the shared vertex,
/// through `seed`, onward — applying the standard loop rules first and, ONLY where they stop
/// (pole / non-valence-4 junction / ambiguous corner), continuing along the straightest
/// candidate (max alignment with the incoming direction, floor 0.2 so it never turns a hard
/// corner uninvited). Returns representative half-edges including `prev` and `seed`.
///
/// **Alt+Shift+click** uses this: two edges orient the walk through poles / hard junctions.
pub fn edge_loop_directed(mesh: &HalfEdgeMesh, prev: HalfEdgeId, seed: HalfEdgeId) -> Vec<HalfEdgeId> {
    edge_loop_directed_with_params(mesh, prev, seed, LoopSelectParams {
        min_angle_deg: 78.0,
        max_steps: None,
    })
}

/// Directed edge loop with MESHmachine Min Angle / Steps applied after the seed.
pub fn edge_loop_directed_with_params(
    mesh: &HalfEdgeMesh,
    prev: HalfEdgeId,
    seed: HalfEdgeId,
    params: LoopSelectParams,
) -> Vec<HalfEdgeId> {
    if (prev.0 as usize) >= mesh.half_edges.len()
        || (seed.0 as usize) >= mesh.half_edges.len()
        || !mesh.is_edge_live(prev)
        || !mesh.is_edge_live(seed)
    {
        return Vec::new();
    }
    let key = |h: HalfEdgeId| undirected_key(mesh, h);
    let (ps, pd) = (mesh.he_source(prev).0, mesh.he(prev).vertex.0);
    let (ss, sd) = (mesh.he_source(seed).0, mesh.he(seed).vertex.0);
    // The shared vertex orients the walk; non-adjacent driver → plain loop (with params).
    let shared = if ss == ps || ss == pd {
        ss
    } else if sd == ps || sd == pd {
        sd
    } else {
        return edge_loop_with_params(mesh, seed, params);
    };
    // `cur` must point INTO the far vertex (away from the shared one).
    let mut cur = if ss == shared {
        seed
    } else {
        let t = mesh.he(seed).twin;
        if t == NO_TWIN {
            return edge_loop_with_params(mesh, seed, params);
        }
        t
    };
    let is_boundary = mesh.he(seed).twin == NO_TWIN;
    let mut hub = seed_hub(mesh, seed);
    let mut result: Vec<HalfEdgeId> = vec![prev, seed];
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(key(prev));
    seen.insert(key(seed));

    let min_cos = min_angle_cos(params.min_angle_deg.max(50.0));
    let mut steps_taken = 0u32;
    let max_iter = mesh.half_edges.len().max(1);
    for _ in 0..max_iter {
        if let Some(max) = params.max_steps {
            if steps_taken >= max {
                break;
            }
        }
        // Standard topological step first — the direction hint only decides where it stops.
        let step = next_loop_edge(mesh, cur, is_boundary, hub);
        let nxt = match step {
            Some((n, h)) if turn_dot(mesh, cur, n) >= min_cos => {
                hub = h;
                n
            }
            Some(_) | None => {
                // Junction / hard turn: oriented LSelect under min_angle.
                match straightest_out_angle(mesh, cur, params.min_angle_deg) {
                    Some(h) => h,
                    None => break,
                }
            }
        };
        let k = key(nxt);
        if seen.contains(&k) {
            break;
        } // closed / rejoined
        seen.insert(k);
        result.push(nxt);
        cur = nxt;
        steps_taken += 1;
    }
    result
}

/// Outgoing live half-edges at `v` via a linear scan — robust when the radial
/// `vertex_fan` walk undercounts (knife densify can leave a broken twin→next cycle
/// so fan returns 1 while two real edges still touch the vert).
fn outgoing_at_vertex(mesh: &HalfEdgeMesh, v: u32) -> Vec<HalfEdgeId> {
    let mut out = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if mesh.he(h).face == NO_FACE {
            continue;
        }
        if mesh.he_source(h).0 != v {
            continue;
        }
        let t = mesh.he(h).twin;
        let key = if t != NO_TWIN { i.min(t.0) } else { i };
        if seen.insert(key) {
            out.push(h);
        }
    }
    out
}

/// Neighbour set for loop stepping: prefer the radial fan when it looks complete,
/// else fall back to a full scan (owner 22.fl4 knife slice).
fn loop_neighbors(mesh: &HalfEdgeMesh, v: crate::half_edge::VertexId) -> Vec<HalfEdgeId> {
    let fan = mesh.vertex_fan(v);
    let scanned = outgoing_at_vertex(mesh, v.0);
    if scanned.len() > fan.len() {
        scanned
    } else {
        fan
    }
}

/// Faces on either side of the undirected edge `h` (a missing side is `NO_FACE`).
#[inline]
fn edge_faces(mesh: &HalfEdgeMesh, h: HalfEdgeId) -> [FaceId; 2] {
    let t = mesh.he(h).twin;
    let other = if t == NO_TWIN { NO_FACE } else { mesh.he(t).face };
    [mesh.he(h).face, other]
}

#[inline]
fn edge_touches_face(mesh: &HalfEdgeMesh, h: HalfEdgeId, f: FaceId) -> bool {
    edge_faces(mesh, h).contains(&f)
}

#[inline]
fn edges_share_face(mesh: &HalfEdgeMesh, a: HalfEdgeId, b: HalfEdgeId) -> bool {
    let fb = edge_faces(mesh, b);
    edge_faces(mesh, a).iter().any(|f| *f != NO_FACE && fb.contains(f))
}

/// Edge-loop step. `cur` arrives INTO its head vertex `v`; returns the half-edge leaving `v` that
/// continues the loop, plus the cap-face latch to carry forward. The rule depends on the loop class
/// fixed at the seed:
///
/// * boundary class: the single boundary half-edge leaving `v`;
/// * valence 2: the only other edge;
/// * valence 4 with every incident edge interior: the edge sharing no face with `cur` (the edge
///   "across" the vertex). When a cap face is latched, the latch moves to the face that holds both
///   the exit edge and the cap's second edge at `v` (a chord splitting the cap);
/// * valence 3 with a latched cap face: the other edge of that face at `v` (walk the cap's rim);
/// * anything else (poles, non-manifold, boundary vertices on an interior walk): `None`.
fn next_loop_edge(mesh: &HalfEdgeMesh, cur: HalfEdgeId, is_boundary: bool, f_hub: Option<FaceId>)
    -> Option<(HalfEdgeId, Option<FaceId>)>
{
    let v = mesh.he(cur).vertex;
    let cur_key = undirected_key(mesh, cur);
    // Outgoing edges plus boundary half-edges that arrive at `v` (they have no outgoing twin, so the
    // outgoing scan alone would miss them and under-count the vertex degree).
    let mut around = loop_neighbors(mesh, v);
    around.extend((0..mesh.half_edges.len() as u32).map(HalfEdgeId).filter(|&h| {
        let he = mesh.he(h);
        he.vertex == v && he.twin == NO_TWIN && he.face != NO_FACE
    }));
    let others: Vec<HalfEdgeId> =
        around.iter().copied().filter(|&h| undirected_key(mesh, h) != cur_key).collect();

    if is_boundary {
        let mut rim = others.iter().copied().filter(|&h| mesh.he(h).twin == NO_TWIN && mesh.he_source(h) == v);
        return match (rim.next(), rim.next()) {
            (Some(h), None) => Some((h, None)),
            _ => None,
        };
    }

    // Degree at v counts `cur` itself (a boundary `cur` is absent from the outgoing list).
    let degree = others.len() + 1;
    let interior = mesh.he(cur).twin != NO_TWIN && others.iter().all(|&h| mesh.he(h).twin != NO_TWIN);
    match degree {
        2 => Some((others[0], f_hub)),
        3 if interior => {
            let cap = f_hub?;
            let mut on_cap = others.iter().copied().filter(|&h| edge_touches_face(mesh, h, cap));
            match (on_cap.next(), on_cap.next()) {
                (Some(h), None) => Some((h, Some(cap))),
                _ => None,
            }
        }
        4 if interior => {
            let mut across = others.iter().copied().filter(|&h| !edges_share_face(mesh, cur, h));
            let exit = match (across.next(), across.next()) {
                (Some(h), None) => h,
                _ => return None,
            };
            let next_hub = f_hub.and_then(|cap| {
                let chord = others
                    .iter()
                    .copied()
                    .find(|&h| h != exit && undirected_key(mesh, h) != cur_key && edge_touches_face(mesh, h, cap))?;
                let exit_faces = edge_faces(mesh, exit);
                edge_faces(mesh, chord)
                    .into_iter()
                    .find(|&f| f != cap && f != NO_FACE && exit_faces.contains(&f))
            });
            Some((exit, next_hub))
        }
        _ => None,
    }
}


// ── Face loop (Blender Alt-click / Shift+double-click face mode) ─────────────

/// Opposite half-edge on a face ring relative to `he` (must lie on `face`).
/// Quads: +2 around the ring. N-gons: furthest (floor(n/2)) step so a belt of
/// n-gons still walks a consistent loop direction.
pub fn opposite_on_face(mesh: &HalfEdgeMesh, face: FaceId, he: HalfEdgeId) -> Option<HalfEdgeId> {
    let ring = mesh.face_half_edges(face);
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let pos = ring.iter().position(|&h| h == he)?;
    let step = if n == 4 { 2 } else { n / 2 };
    Some(ring[(pos + step) % n])
}

/// Half-edge of `face` that is the twin-side of undirected edge `he` (or `he` itself if on face).
pub fn he_on_face(mesh: &HalfEdgeMesh, face: FaceId, he: HalfEdgeId) -> Option<HalfEdgeId> {
    if mesh.he(he).face == face {
        return Some(he);
    }
    let t = mesh.he(he).twin;
    if t != NO_TWIN && mesh.he(t).face == face {
        return Some(t);
    }
    None
}

/// Walk a face LOOP starting at `seed`, entering across undirected edge `across`
/// (the shared edge that orients the loop — like Blender's face under the cursor edge).
/// Walks both directions from the seed. Returns unique FaceIds including `seed`.
pub fn face_loop(mesh: &HalfEdgeMesh, seed: FaceId, across: HalfEdgeId) -> Vec<FaceId> {
    let mut result = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let push = |f: FaceId, result: &mut Vec<FaceId>, seen: &mut HashSet<u32>| {
        if seen.insert(f.0) {
            result.push(f);
        }
    };
    push(seed, &mut result, &mut seen);

    // Orient: step on seed from `across` to opposite, cross to neighbour, continue.
    let walk = |start_face: FaceId, entry_he: HalfEdgeId, result: &mut Vec<FaceId>, seen: &mut HashSet<u32>| {
        let mut face = start_face;
        let mut entry = entry_he;
        for _ in 0..mesh.faces.len() {
            let Some(on) = he_on_face(mesh, face, entry) else {
                break;
            };
            let Some(opp) = opposite_on_face(mesh, face, on) else {
                break;
            };
            let tw = mesh.he(opp).twin;
            if tw == NO_TWIN {
                break;
            }
            let next_f = mesh.he(tw).face;
            if next_f == NO_FACE || !seen.insert(next_f.0) {
                break; // closed or dead
            }
            result.push(next_f);
            // Enter next face across twin of opp
            face = next_f;
            entry = tw;
        }
    };

    // Direction A: from seed across `across` toward neighbour
    if let Some(on) = he_on_face(mesh, seed, across) {
        let tw = mesh.he(on).twin;
        if tw != NO_TWIN {
            let nf = mesh.he(tw).face;
            if nf != NO_FACE && seen.insert(nf.0) {
                result.push(nf);
                walk(nf, tw, &mut result, &mut seen);
            }
        }
        // Direction B: opposite side of seed (other way around the loop)
        if let Some(opp) = opposite_on_face(mesh, seed, on) {
            let tw = mesh.he(opp).twin;
            if tw != NO_TWIN {
                let nf = mesh.he(tw).face;
                if nf != NO_FACE && seen.insert(nf.0) {
                    result.push(nf);
                    walk(nf, tw, &mut result, &mut seen);
                }
            }
        }
    }
    result
}

/// Face loop oriented by two already-selected faces (owner: select 1 then 2, then
/// Shift+double-click → continue 3,4,… in that direction).
/// Finds a shared edge between `f0` and `f1`; walks the loop through `f1` away from `f0`,
/// and also the opposite way from `f0`. Returns all faces on the loop including both seeds.
pub fn face_loop_from_two(mesh: &HalfEdgeMesh, f0: FaceId, f1: FaceId) -> Vec<FaceId> {
    if f0 == f1 {
        // No direction — pick first edge of f0 as across.
        let ring = mesh.face_half_edges(f0);
        if ring.is_empty() {
            return vec![f0];
        }
        return face_loop(mesh, f0, ring[0]);
    }
    // Shared undirected edge between f0 and f1.
    let mut shared: Option<HalfEdgeId> = None;
    for h in mesh.face_half_edges(f0) {
        let t = mesh.he(h).twin;
        if t != NO_TWIN && mesh.he(t).face == f1 {
            shared = Some(h);
            break;
        }
    }
    let Some(across) = shared else {
        // Not adjacent — face loop from f1 alone with first edge.
        let ring = mesh.face_half_edges(f1);
        if ring.is_empty() {
            return vec![f0, f1];
        }
        let mut out = face_loop(mesh, f1, ring[0]);
        if !out.iter().any(|f| *f == f0) {
            out.push(f0);
        }
        return out;
    };
    // Full bidirectional loop through the shared edge (includes f0 and f1).
    face_loop(mesh, f0, across)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::half_edge::HalfEdgeMesh;
    use glam::Vec3;

    fn undirected_count(mesh: &HalfEdgeMesh, edges: &[HalfEdgeId]) -> usize {
        let mut keys = HashSet::new();
        for &h in edges {
            let t = mesh.he(h).twin;
            keys.insert(if t != NO_TWIN { h.0.min(t.0) } else { h.0 });
        }
        keys.len()
    }

    /// Find the half-edge between vertex indices (a, b) in either direction.
    fn he_between(mesh: &HalfEdgeMesh, a: u32, b: u32) -> HalfEdgeId {
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if mesh.he(h).face == NO_FACE { continue; }
            let (s, d) = (mesh.he_source(h).0, mesh.he(h).vertex.0);
            if (s == a && d == b) || (s == b && d == a) { return h; }
        }
        panic!("no half-edge between v{a} and v{b}");
    }

    fn contains_undirected(mesh: &HalfEdgeMesh, edges: &[HalfEdgeId], a: u32, b: u32) -> bool {
        let want = he_between(mesh, a, b);
        let t = mesh.he(want).twin;
        let wk = if t != NO_TWIN { want.0.min(t.0) } else { want.0 };
        edges.iter().any(|&h| {
            let ht = mesh.he(h).twin;
            let hk = if ht != NO_TWIN { h.0.min(ht.0) } else { h.0 };
            hk == wk
        })
    }

    /// 4×3 vertex grid (x=0..3, y=0..2), quads except the (2,1)-(3,1)-(3,2)-(2,2) cell which is
    /// split diagonally → vertex v6=(2,1) has valence 5: a POLE where the plain loop stops.
    fn grid_with_pole() -> HalfEdgeMesh {
        let mut pos = Vec::new();
        for j in 0..3 {
            for i in 0..4 {
                pos.push(Vec3::new(i as f32, j as f32, 0.0));
            }
        }
        let q = |i: usize, j: usize| vec![j * 4 + i, j * 4 + i + 1, (j + 1) * 4 + i + 1, (j + 1) * 4 + i];
        let mut faces = vec![q(0, 0), q(1, 0), q(2, 0), q(0, 1), q(1, 1)];
        faces.push(vec![6, 7, 11]); // split cell → diagonal 6-11 → v6 valence 5
        faces.push(vec![6, 11, 10]);
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// Owner Desktop `22.fl4` topology (Poly.box after free-knife through-slice):
    /// densified cut path with valence-2 mid verts. Double-click on any cut segment must
    /// select the full closed slice belt (not 1–3 edges).
    #[test]
    fn edge_loop_owner_22_fl4_slice_belt() {
        // Positions from diag_fl4_dump on 22.fl4 (2026-08-05).
        let pos = [
            Vec3::new(-1.0, -1.0, 1.0),   // 0
            Vec3::new(-1.0, -1.0, -1.0),  // 1
            Vec3::new(1.0, -1.0, -1.0),   // 2
            Vec3::new(1.0, -1.0, 1.0),    // 3
            Vec3::new(-1.0, 1.0, -1.0),   // 4
            Vec3::new(-1.0, 1.0, 1.0),    // 5
            Vec3::new(1.0, 1.0, 1.0),     // 6
            Vec3::new(1.0, 1.0, -1.0),    // 7
            Vec3::new(1.0, -0.5166557, -1.0),           // 8
            Vec3::new(-0.2738211, -0.51665556, -1.0),   // 9  was v17
            Vec3::new(-0.5930399, -0.19743681, -1.0),   // 10 was v20
            Vec3::new(-0.9799963, -0.19743681, -1.0),   // 11 was v22
            Vec3::new(-1.0, -0.19743678, -1.0),         // 12 was v23
            Vec3::new(1.0, -0.5166557, 1.0),            // 13 was v24
            Vec3::new(-0.2738211, -0.51665556, 1.0),    // 14 was v33
            Vec3::new(-0.5930399, -0.19743681, 1.0),    // 15 was v36
            Vec3::new(-1.0, -0.19743681, 1.0),          // 16 was v39
        ];
        // Face rings from the dump (reindexed free slots 17→9 … 39→16).
        let faces = [
            vec![1usize, 2, 3, 0],                                 // bottom
            vec![5, 6, 7, 4],                                      // top
            vec![12, 4, 7, 8, 9, 10, 11],                          // back upper (z=-1)
            vec![13, 14, 15, 16, 0, 3],                            // front lower-ish
            vec![1, 0, 16, 12],                                    // left lower
            vec![7, 6, 13, 8],                                     // right
            vec![16, 15, 14, 13, 6, 5],                            // front upper
            vec![1, 12, 11, 10, 9, 8, 2],                          // back lower
            vec![5, 4, 12, 16],                                    // left upper
            vec![3, 2, 8, 13],                                     // right lower
        ];
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        // Mid-path cut verts are valence-2 (the bug class).
        assert_eq!(valence(&m, 9), 2, "slice mid v9");
        assert_eq!(valence(&m, 14), 2, "slice mid v14");
        // Closed slice belt (through-cut):
        //  8-9-10-11-12-16-15-14-13-8
        let belt = [
            (8u32, 9u32),
            (9, 10),
            (10, 11),
            (11, 12),
            (12, 16),
            (16, 15),
            (15, 14),
            (14, 13),
            (13, 8),
        ];
        // Seed on a mid path segment that previously returned loop size 1.
        let seed = he_between(&m, 9, 10);
        let lp = edge_loop(&m, seed);
        let n = undirected_count(&m, &lp);
        for &(a, b) in &belt {
            assert!(
                contains_undirected(&m, &lp, a, b),
                "slice belt missing edge v{a}-v{b}; loop size={n}"
            );
        }
        assert!(
            n >= belt.len(),
            "full slice belt expected ≥{} edges, got {n}",
            belt.len()
        );
    }

    /// Knife-slice mid-face verts are valence-2 (two coplanar n-gons share the cut path).
    /// Double-click must walk the whole path, not stop after one segment (owner 22.fl4).
    #[test]
    fn edge_loop_walks_valence2_knife_path() {
        // Two faces sharing the chain v0—v1—v2 (v1 is pure path / valence-2):
        //   F1: 0,1,2,3,4
        //   F2: 2,1,0,5,6
        let pos = [
            Vec3::new(0.0, 0.0, 0.0), // 0
            Vec3::new(1.0, 0.0, 0.0), // 1  mid path
            Vec3::new(2.0, 0.0, 0.0), // 2
            Vec3::new(2.0, 1.0, 0.0), // 3
            Vec3::new(0.0, 1.0, 0.0), // 4
            Vec3::new(0.0, -1.0, 0.0), // 5
            Vec3::new(2.0, -1.0, 0.0), // 6
        ];
        let faces = [vec![0usize, 1, 2, 3, 4], vec![2, 1, 0, 5, 6]];
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        assert_eq!(valence(&m, 1), 2, "mid-path vert must be valence-2");
        let seed = he_between(&m, 0, 1);
        let lp = edge_loop(&m, seed);
        assert!(
            contains_undirected(&m, &lp, 0, 1),
            "seed edge stays in the loop"
        );
        assert!(
            contains_undirected(&m, &lp, 1, 2),
            "must continue past valence-2 mid-path vert onto 1-2"
        );
        assert!(
            undirected_count(&m, &lp) >= 2,
            "knife path loop must include both path edges; got {}",
            undirected_count(&m, &lp)
        );
    }

    /// Closed knife-style belt: square rim densified with mid-edge verts (all valence-2 on a
    /// two-face "strip" is hard; use a flat annulus of two n-gons sharing a 4-edge hole...
    /// Simpler closed chain: two n-gons that share a full closed? Not planar-disk.
    /// Instead: open path of 3 segments (2 mid verts) — walker must get all 3.
    #[test]
    fn edge_loop_walks_multi_segment_valence2_path() {
        // F1/F2 share v0-v1-v2-v3 (two mid verts v1,v2 valence-2).
        let pos = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(3.0, -1.0, 0.0),
        ];
        let faces = [vec![0usize, 1, 2, 3, 4, 5], vec![3, 2, 1, 0, 6, 7]];
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        assert_eq!(valence(&m, 1), 2);
        assert_eq!(valence(&m, 2), 2);
        let seed = he_between(&m, 1, 2); // middle segment
        let lp = edge_loop(&m, seed);
        assert!(contains_undirected(&m, &lp, 0, 1), "left segment");
        assert!(contains_undirected(&m, &lp, 1, 2), "middle segment");
        assert!(contains_undirected(&m, &lp, 2, 3), "right segment");
        assert_eq!(
            undirected_count(&m, &lp),
            3,
            "full 3-segment knife path; got {}",
            undirected_count(&m, &lp)
        );
    }

    /// Face loop on a 2×2 quad grid: four faces in a belt around the shared vertex.
    #[test]
    fn face_loop_walks_quad_strip() {
        // 3×2 verts → 2 quads side by side (one strip).
        let pos = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
        ];
        let faces = [vec![0usize, 1, 4, 3], vec![1, 2, 5, 4]];
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        // Across the shared vertical edge between the two quads → both faces selected.
        let across = he_between(&m, 1, 4);
        let lp = face_loop(&m, FaceId(0), across);
        assert_eq!(lp.len(), 2, "two-quad strip is a 2-face loop; got {lp:?}");
        assert!(lp.contains(&FaceId(0)) && lp.contains(&FaceId(1)));

        // From two faces: same result.
        let lp2 = face_loop_from_two(&m, FaceId(0), FaceId(1));
        assert_eq!(lp2.len(), 2);
    }

    /// 2×2 face grid (4 quads): face loop along a row selects both faces of that row.
    #[test]
    fn face_loop_row_on_2x2_grid() {
        let mut pos = Vec::new();
        for j in 0..3 {
            for i in 0..3 {
                pos.push(Vec3::new(i as f32, j as f32, 0.0));
            }
        }
        let q = |i: usize, j: usize| {
            let a = j * 3 + i;
            vec![a, a + 1, a + 4, a + 3]
        };
        let faces = [q(0, 0), q(1, 0), q(0, 1), q(1, 1)];
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        // Bottom row: face 0 and face 1 share edge between verts 1-4.
        let across = he_between(&m, 1, 4);
        let lp = face_loop(&m, FaceId(0), across);
        assert!(
            lp.contains(&FaceId(0)) && lp.contains(&FaceId(1)),
            "bottom row loop must include f0+f1; got {lp:?}"
        );
        assert!(
            !lp.contains(&FaceId(2)) && !lp.contains(&FaceId(3)),
            "bottom row must not jump to top row; got {lp:?}"
        );
    }

    /// MESHmachine LSelect: Use Edge Limit + Steps caps walk length per direction.
    #[test]
    fn lselect_edge_limit_caps_steps() {
        // Open tube rim is a long closed boundary loop (N edges).
        let m = open_tube(16);
        let seed = {
            // First boundary half-edge.
            let mut s = HalfEdgeId(0);
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if m.is_edge_live(h) && m.he(h).twin == NO_TWIN {
                    s = h;
                    break;
                }
            }
            s
        };
        let unlimited = edge_loop_with_params(
            &m,
            seed,
            LoopSelectParams {
                min_angle_deg: 90.0,
                max_steps: None,
            },
        );
        let limited = edge_loop_with_params(
            &m,
            seed,
            LoopSelectParams {
                min_angle_deg: 90.0,
                max_steps: Some(2),
            },
        );
        assert!(
            undirected_count(&m, &unlimited) >= 8,
            "full rim should be long, got {}",
            undirected_count(&m, &unlimited)
        );
        assert!(
            undirected_count(&m, &unlimited) > undirected_count(&m, &limited),
            "unlimited ({}) must grow past limited steps=2 ({})",
            undirected_count(&m, &unlimited),
            undirected_count(&m, &limited)
        );
        // Seed + 2 steps each direction max → at most 1 + 2 + 2 = 5 undirected edges.
        assert!(
            undirected_count(&m, &limited) <= 5,
            "steps=2 both dirs → ≤5 edges, got {}",
            undirected_count(&m, &limited)
        );
    }

    /// Edge ring walks a plane's parallel rails (n-gon-safe opposite-on-face).
    #[test]
    fn edge_ring_plane_row() {
        let m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let hes = m.face_half_edges(FaceId(0));
        let seed = hes[0];
        let ring = edge_ring_with_params(&m, seed, None);
        assert!(
            undirected_count(&m, &ring) >= 2,
            "ring across a plane strip must span ≥2 edges; got {}",
            undirected_count(&m, &ring)
        );
        let limited = edge_ring_with_params(&m, seed, Some(1));
        assert!(
            undirected_count(&m, &limited) <= undirected_count(&m, &ring),
            "step-limited ring must not exceed full ring"
        );
    }

    /// Owner request 2026-07-23 (Blender parity): at a junction with two loop directions, a
    /// second selected edge drives the direction. Plain loop stops AT the pole; the directed
    /// walk crosses it along the straightest edge — and never onto the diagonal.
    #[test]
    fn directed_loop_crosses_pole_straight() {
        let m = grid_with_pole();
        let prev = he_between(&m, 4, 5); // driver: row-1 edge left of the seed
        let seed = he_between(&m, 5, 6); // clicked: points at the valence-5 pole v6

        // Plain loop must stop at the pole (Blender behavior): no edge 6-7, no diagonal.
        let plain = edge_loop(&m, seed);
        assert!(!contains_undirected(&m, &plain, 6, 7), "plain loop must stop at the pole");

        // Directed: crosses v6 straight onto 6-7, skips the diagonal 6-11.
        let dir = edge_loop_directed(&m, prev, seed);
        assert!(contains_undirected(&m, &dir, 4, 5), "directed loop keeps the driver");
        assert!(contains_undirected(&m, &dir, 5, 6), "directed loop keeps the seed");
        assert!(contains_undirected(&m, &dir, 6, 7), "directed loop must cross the pole straight");
        assert!(!contains_undirected(&m, &dir, 6, 11), "directed loop must not take the diagonal");
        assert_eq!(undirected_count(&m, &dir), 3, "row edges only: 4-5, 5-6, 6-7");
    }

    /// Capless tube (side quads only → rim is a BOUNDARY loop): the top rim edge's loop is all N rim edges
    /// (seed edge is boundary → boundary class). Built explicitly since `new_cylinder` is now capped.
    fn open_tube(segs: usize) -> HalfEdgeMesh {
        let mut pos: Vec<Vec3> = Vec::new();
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(), -1.0, t.sin())); }
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(),  1.0, t.sin())); }
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for s in 0..segs { let n = (s + 1) % segs; faces.push(vec![s, segs + s, segs + n, n]); } // side quads only
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    #[test]
    fn edge_loop_open_tube_rim_is_boundary() {
        let segs = 8usize;
        let m = open_tube(segs);
        let start = (0..m.half_edges.len() as u32).find(|&i| {
            let h = HalfEdgeId(i);
            if m.he(h).face == NO_FACE { return false; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            m.he(h).twin == NO_TWIN && (s.y - 1.0).abs() < 1e-4 && (d.y - 1.0).abs() < 1e-4
        }).map(HalfEdgeId).expect("top boundary rim edge");
        let loop_edges = edge_loop(&m, start);
        assert_eq!(undirected_count(&m, &loop_edges), segs,
            "open-tube rim loop must select all {segs} boundary edges; got {}", undirected_count(&m, &loop_edges));
    }

    /// CAPPED cylinder (n-gon top+bottom caps + side quads): a top rim edge's seed hub = the cap n-gon →
    /// the loop walks around the cap = all N rim edges. This is the user's actual case.
    fn capped_cylinder(segs: usize) -> HalfEdgeMesh {
        let mut pos: Vec<Vec3> = Vec::new();
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(), -1.0, t.sin())); }
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(),  1.0, t.sin())); }
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for s in 0..segs { let n = (s + 1) % segs; faces.push(vec![s, n, segs + n, segs + s]); } // side quads
        faces.push((0..segs).map(|s| segs + s).collect());  // top cap n-gon (twins the wall)
        faces.push((0..segs).rev().collect());               // bottom cap n-gon
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    #[test]
    fn edge_loop_capped_cylinder_rim_is_ngon() {
        let segs = 6usize;
        let m = capped_cylinder(segs);
        let start = (0..m.half_edges.len() as u32).find(|&i| {
            let h = HalfEdgeId(i);
            if m.he(h).twin == NO_TWIN || m.he(h).face == NO_FACE { return false; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            (s.y - 1.0).abs() < 1e-4 && (d.y - 1.0).abs() < 1e-4
        }).map(HalfEdgeId).expect("top rim edge");
        let loop_edges = edge_loop(&m, start);
        assert_eq!(undirected_count(&m, &loop_edges), segs,
            "capped cylinder rim loop must select all {segs} rim edges; got {}", undirected_count(&m, &loop_edges));
    }

    /// `new_cylinder` now builds a CAPPED cylinder (n-gon caps + side quads), watertight, all normals
    /// OUTWARD (the reported "poly seems flipped"), and its rim is a single n-gon → edge_loop selects all
    /// the way around. Guards the cylinder fix.
    #[test]
    fn new_cylinder_is_capped_and_outward() {
        let segs = 12u32;
        let m = HalfEdgeMesh::new_cylinder(0.6, 2.0, segs);
        let boundary = (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(boundary, 0, "capped cylinder must be watertight; boundary={boundary}");
        assert_eq!(m.faces.len(), segs as usize + 2, "segs side quads + 2 caps");
        let centroid = m.vertices.iter().map(|v| v.position).sum::<Vec3>() / m.vertices.len() as f32;
        for f in 0..m.faces.len() as u32 {
            let n = m.compute_face_normal(FaceId(f));
            let vs = m.face_vertices(FaceId(f));
            let fc = vs.iter().map(|v| m.vertices[v.0 as usize].position).sum::<Vec3>() / vs.len() as f32;
            assert!(n.dot(fc - centroid) > 0.0, "face {f} normal points inward (flipped)");
        }
        let start = (0..m.half_edges.len() as u32).find(|&i| {
            let h = HalfEdgeId(i);
            if m.he(h).twin == NO_TWIN || m.he(h).face == NO_FACE { return false; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            (s.y - 1.0).abs() < 1e-4 && (d.y - 1.0).abs() < 1e-4
        }).map(HalfEdgeId).expect("top rim edge");
        assert_eq!(undirected_count(&m, &edge_loop(&m, start)), segs as usize, "rim loop selects all rim edges");
    }

    /// DEBUG: run edge_loop on EVERY rim half-edge (both directed orientations) of the capped cylinder;
    /// report any seed that does NOT return the full rim. Catches a one-direction / orientation bug.
    #[test]
    fn dbg_edge_loop_all_rim_seeds() {
        let segs = 8u32;
        let m = HalfEdgeMesh::new_cylinder(0.6, 2.0, segs);
        let mut fails = 0;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if m.he(h).face == NO_FACE { continue; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            if (s.y - 1.0).abs() < 1e-4 && (d.y - 1.0).abs() < 1e-4 {
                let n = undirected_count(&m, &edge_loop(&m, h));
                if n != segs as usize {
                    fails += 1;
                    eprintln!("SEED he{i} (v{}->v{}) → loop size {n} (expected {segs})", m.he_source(h).0, m.he(h).vertex.0);
                }
            }
        }
        eprintln!("rim seeds failing: {fails}");
        assert_eq!(fails, 0, "{fails} rim seeds did not select the full loop");
    }

    /// SEED-LATCH: a VERTICAL wall edge of a capped cylinder is NOT a hub seed (its endpoints are valence-3
    /// rim verts but its own faces are two quad walls, len 4 → no hub) — so it must NOT get hijacked into
    /// walking a cap rim. It should select just itself (both endpoints are valence-3 poles for the interior
    /// rule). Guards Blender divergence A (per-seed, not per-vertex).
    #[test]
    fn edge_loop_vertical_edge_not_hijacked_by_cap() {
        let segs = 6usize;
        let m = capped_cylinder(segs);
        // vertical edge: endpoints differ only in y (top vs bottom, same x,z)
        let start = (0..m.half_edges.len() as u32).find(|&i| {
            let h = HalfEdgeId(i);
            if m.he(h).twin == NO_TWIN || m.he(h).face == NO_FACE { return false; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            (s.x - d.x).abs() < 1e-4 && (s.z - d.z).abs() < 1e-4 && (s.y - d.y).abs() > 1.5
        }).map(HalfEdgeId).expect("vertical wall edge");
        let loop_edges = edge_loop(&m, start);
        // Must NOT wrap a cap rim (would be `segs`); the vertical edge is a valence-3 pole → just itself.
        assert_eq!(undirected_count(&m, &loop_edges), 1,
            "vertical edge must not be hijacked into a cap rim; got {}", undirected_count(&m, &loop_edges));
    }

    /// Capped cylinder whose TOP cap n-gon is SPLIT into two faces by a chord (top vert 0 → top vert
    /// segs/2). This is the user's "adding this segment broke the loop" case: the two chord endpoints
    /// become valence-4, so a hub walk latched to one cap piece would stop there and select only ONE
    /// SIDE of the rim. The full rim must still be selectable.
    fn split_cap_cylinder(segs: usize) -> HalfEdgeMesh {
        let mut pos: Vec<Vec3> = Vec::new();
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(), -1.0, t.sin())); }
        for s in 0..segs { let t = std::f32::consts::TAU * s as f32 / segs as f32; pos.push(Vec3::new(t.cos(),  1.0, t.sin())); }
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for s in 0..segs { let n = (s + 1) % segs; faces.push(vec![s, n, segs + n, segs + s]); } // side quads
        let half = segs / 2;
        faces.push((0..=half).map(|s| segs + s).collect());                       // top cap piece A (chord 0→half)
        let mut b: Vec<usize> = (half..segs).map(|s| segs + s).collect(); b.push(segs); // piece B ...→(segs-1)→0
        faces.push(b);
        faces.push((0..segs).rev().collect());                                    // bottom cap n-gon
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    #[test]
    fn edge_loop_split_cap_selects_full_rim() {
        let segs = 8usize;
        let m = split_cap_cylinder(segs);
        // sanity: the two chord vertices (top 0 and top segs/2) are valence-4 (2 rim + 1 wall + 1 chord).
        assert_eq!(valence(&m, segs as u32), 4, "chord vertex top-0 should be valence-4");
        assert_eq!(valence(&m, (segs + segs / 2) as u32), 4, "chord vertex top-half should be valence-4");
        // EVERY top rim seed (both orientations) must select all `segs` rim edges — never one side, never
        // the chord (that would be segs+1).
        let mut checked = 0;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if m.he(h).twin == NO_TWIN || m.he(h).face == NO_FACE { continue; }
            let s = m.vertices[m.he_source(h).0 as usize].position;
            let d = m.vertices[m.he(h).vertex.0 as usize].position;
            // rim edge: both endpoints on the top ring AND not the vertical/chord (chord skips a step in x/z,
            // but easiest: adjacency by index is implied since only rim+chord live on the top ring; exclude
            // the chord by requiring the loop to be exactly `segs`).
            if (s.y - 1.0).abs() < 1e-4 && (d.y - 1.0).abs() < 1e-4 {
                let n = undirected_count(&m, &edge_loop(&m, h));
                // The chord (top0↔top-half) also has both endpoints on the top ring; its loop is NOT the rim.
                // Skip it: a chord seed is not a rim edge. Detect the chord as the top-ring edge whose
                // endpoints are segs apart in index → but we just assert rim seeds hit `segs`.
                let src = m.he_source(h).0 as usize; let dst = m.he(h).vertex.0 as usize;
                let a = src - segs; let bb = dst - segs;
                let adjacent = (a + 1) % segs == bb || (bb + 1) % segs == a;
                if !adjacent { continue; } // the chord, not a rim edge
                assert_eq!(n, segs, "split-cap rim seed he{i} must select all {segs} rim edges, got {n} (one side / chord bug)");
                checked += 1;
            }
        }
        assert!(checked > 0, "no rim seeds were checked");
    }

    /// GATE (owner 2026-07-30) — the Shift+Wheel loop walk must be a ROUND TRIP.
    ///
    /// Owner: "the selection next shift scroll wheel do not behave like Blender." Two defects in
    /// `poly_loop_extend` (app/src/poly/selection.rs), both reproduced by walking the same ring here:
    ///
    ///  1. GROW inserted the raw half-edge id while SHRINK removed the id AND its twin, so
    ///     scroll-up-then-down was not an inverse — the set could retain a twin id, and the next
    ///     shrink then removed two entries for one visual edge.
    ///  2. SHRINK scanned forward and `break`-ed at the first UNSELECTED edge, so it retracted from
    ///     the wrong end whenever the selected run did not start at index 0 of the loop order.
    ///
    /// This pins the invariant those fixes restore, on the kernel side where it is testable:
    /// canonicalising each half-edge to `min(id, twin)` makes "is this edge selected" independent of
    /// which half was stored, which is what lets grow and shrink be exact inverses.
    #[test]
    fn edge_loop_halves_canonicalise_to_a_stable_id() {
        let m = HalfEdgeMesh::new_cylinder(1.0, 2.0, 12);
        let mut best = (HalfEdgeId(0), 0usize);
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let n = edge_loop(&m, h).len();
            if n > best.1 { best = (h, n); }
        }
        let lp = edge_loop(&m, best.0);
        assert!(lp.len() >= 4, "need a real ring to test the walk (got {})", lp.len());

        let canon = |h: HalfEdgeId| -> u32 {
            let t = m.he(h).twin;
            if t != NO_TWIN { h.0.min(t.0) } else { h.0 }
        };

        // Every edge in the loop must canonicalise to ONE id regardless of which half is examined.
        for &h in &lp {
            let t = m.he(h).twin;
            if t == NO_TWIN { continue; }
            assert_eq!(
                canon(h),
                canon(t),
                "half-edge {} and its twin {} must share a canonical id — otherwise grow (which                  stores one half) and shrink (which removes both) are not inverses",
                h.0, t.0
            );
        }

        // And the loop order is spatially contiguous, which is what makes "extend at the frontier"
        // a meaningful rule rather than an arbitrary jump.
        let mut breaks = 0;
        for w in lp.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (a_s, a_d) = (m.he_source(a).0, m.he(a).vertex.0);
            let (b_s, b_d) = (m.he_source(b).0, m.he(b).vertex.0);
            if !(a_s == b_s || a_s == b_d || a_d == b_s || a_d == b_d) {
                breaks += 1;
            }
        }
        assert_eq!(breaks, 0, "edge_loop order must be spatially contiguous ({breaks} breaks)");
    }

    /// GATE (owner 2026-08-08) — Shift+Scroll "select next in line" on a VERTEX chain.
    ///
    /// Blender oracle: Select → More/Less → Next Active (Shift-Ctrl-NumpadPlus).
    /// History pair prev→active defines direction; each step selects the next neighbour.
    /// Round-trip: walking a path of N verts needs exactly N-2 next-steps from the seed pair.
    #[test]
    fn vert_next_active_walks_grid_row() {
        // Explicit 1×4 quad strip (5×2 verts). Built with from_polygons so connectivity is
        // unambiguous: bottom row 0-1-2-3-4 is a straight path; vertical rungs go to 5-6-7-8-9.
        let mut pos = Vec::new();
        for z in 0..2 {
            for x in 0..5 {
                pos.push(Vec3::new(x as f32, 0.0, z as f32));
            }
        }
        let mut faces = Vec::new();
        for x in 0..4 {
            let a = x;
            let b = x + 1;
            let c = x + 1 + 5;
            let d = x + 5;
            faces.push(vec![a, b, c, d]);
        }
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);

        // Single step: 0→1 continues to 2 (+X), NOT up the rung to 6.
        let step = vert_next_active(&m, 0, 1);
        assert_eq!(
            step,
            Some(2),
            "strip: Next Active from 0→1 must be vert 2 (+X), got {step:?}"
        );
        // Full bottom-row walk.
        let mut chain = vec![0u32, 1u32];
        let mut prev = 0u32;
        let mut active = 1u32;
        for _ in 0..8 {
            match vert_next_active(&m, prev, active) {
                Some(n) if !chain.contains(&n) => {
                    chain.push(n);
                    prev = active;
                    active = n;
                }
                _ => break,
            }
        }
        assert_eq!(
            &chain[..5],
            &[0, 1, 2, 3, 4],
            "must walk the +X row in order (got {chain:?})"
        );
    }

    /// Path that turns a corner (L-shape): Next Active must follow the chain, not require
    /// a straight belt — Blender walks the selected path through hard corners.
    #[test]
    fn vert_next_active_follows_corner_path() {
        // Unit square: verts 0-1-2-3 around the boundary.
        let m = HalfEdgeMesh::from_polygons(
            &[
                glam::Vec3::new(0.0, 0.0, 0.0),
                glam::Vec3::new(1.0, 0.0, 0.0),
                glam::Vec3::new(1.0, 0.0, 1.0),
                glam::Vec3::new(0.0, 0.0, 1.0),
            ],
            &[vec![0, 1, 2, 3]],
        );
        // Path 0 → 1 → 2 turns 90° at vert 1.
        let n = vert_next_active(&m, 0, 1).expect("corner step 0→1 must yield 2");
        assert_eq!(n, 2, "from 0→1 the only remaining neighbour of 1 is 2 (got {n})");
        let n2 = vert_next_active(&m, 1, 2).expect("corner step 1→2 must yield 3");
        assert_eq!(n2, 3, "from 1→2 next is 3 (got {n2})");
    }

    /// Edge Next Active: grow along a cylinder rim must step one undirected edge at a time,
    /// and the selected-set membership must be twin-canonical (grow/shrink round-trip).
    #[test]
    fn edge_next_active_rim_steps_one_and_round_trips() {
        let m = HalfEdgeMesh::new_cylinder(1.0, 2.0, 12);
        // Longest loop = a rim.
        let mut best = (HalfEdgeId(0), 0usize);
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let n = edge_loop(&m, h).len();
            if n > best.1 {
                best = (h, n);
            }
        }
        let lp = edge_loop(&m, best.0);
        assert!(lp.len() >= 4, "need a rim (got {})", lp.len());
        let canon = |h: HalfEdgeId| -> u32 {
            let t = m.he(h).twin;
            if t != NO_TWIN {
                h.0.min(t.0)
            } else {
                h.0
            }
        };
        let mut selected: HashSet<u32> = HashSet::new();
        selected.insert(canon(lp[0]));
        selected.insert(canon(lp[1]));
        // Walk the rest of the rim with edge_next_active.
        let mut prev = lp[0];
        let mut active = lp[1];
        let mut steps = 0;
        while steps < lp.len() + 2 {
            // Rebuild the twin-aware predicate each step so `selected` is free to mutate.
            let sel_snap = selected.clone();
            let is_sel = |h: HalfEdgeId| -> bool {
                let t = m.he(h).twin;
                sel_snap.contains(&h.0)
                    || (t != NO_TWIN && sel_snap.contains(&t.0))
                    || sel_snap.contains(&canon(h))
            };
            match edge_next_active(&m, prev, active, &is_sel) {
                Some(n) => {
                    let c = canon(n);
                    assert!(
                        selected.insert(c),
                        "next edge must be unselected (duplicate step)"
                    );
                    prev = active;
                    active = n;
                    steps += 1;
                }
                None => break,
            }
        }
        assert!(
            steps >= lp.len().saturating_sub(2),
            "rim Next Active must cover the loop (steps={steps}, loop={})",
            lp.len()
        );
    }

}
