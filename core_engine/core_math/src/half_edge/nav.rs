// Navigation queries: fans, rings, loops, boundaries, slide rails (split from half_edge.rs —
// pure relocation).

use super::*;

impl HalfEdgeMesh {
    // ============================================================
    // Navigation queries
    // ============================================================

    /// All outgoing half-edges from a vertex (fan traversal).
    pub fn vertex_fan(&self, v: VertexId) -> Vec<HalfEdgeId> {
        let mut result = Vec::new();
        self.vertex_fan_into(v, &mut result);
        result
    }

    /// [`Self::vertex_fan`] into a caller-owned buffer — same walk, same guards, but zero
    /// allocation when the buffer is reused across vertices.
    ///
    /// The allocating form is a thin wrapper over this one, so there is exactly ONE fan walk
    /// (reuse rule). Hot loops that circulate every vertex — corner-normal bakes, weighted
    /// normals — allocated one `Vec` per vertex, per pass; that allocation, not the walk,
    /// was the cost.
    pub fn vertex_fan_into(&self, v: VertexId, result: &mut Vec<HalfEdgeId>) {
        result.clear();
        let start = self.vert(v).half_edge;
        if start == NO_EDGE { return; }
        if start.0 as usize >= self.half_edges.len() { return; }
        let mut cur = start;
        let max_iter = self.half_edges.len();
        loop {
            result.push(cur);
            let twin = self.he(cur).twin;
            if twin == NO_TWIN { break; } // boundary
            if twin.0 as usize >= self.half_edges.len() { break; }
            cur = self.he(twin).next;
            if cur == start || cur == NO_EDGE || cur.0 as usize >= self.half_edges.len() { break; }
            if result.len() >= max_iter { break; } // corrupted topology guard
        }
    }

    /// Edge-slide RAILS for a loop half-edge `h` (ending at V = `h.vertex`). Returns the two off-loop
    /// neighbour vertices V rides during an edge slide: `rail_pos` on `h`'s face side, `rail_neg` on the
    /// twin's face side. The slide tool then moves V to `V + t·(rail_pos − V)` for t ≥ 0 and
    /// `V + |t|·(rail_neg − V)` for t < 0 (Blender MESH_OT_edge_slide, factor −1..1). Returns None on a
    /// boundary edge (no twin → only one rail) or malformed topology. Single home for the rail math
    /// (reuse rule) — the vertex-slide tool needs no kernel helper (it just lerps toward a picked
    /// `vertex_fan` neighbour).
    pub fn edge_slide_rails(&self, h: HalfEdgeId) -> Option<(VertexId, VertexId)> {
        if h == NO_EDGE || h.0 as usize >= self.half_edges.len() { return None; }
        let he = self.he(h);
        let next = he.next;
        if next == NO_EDGE || next.0 as usize >= self.half_edges.len() { return None; }
        let rail_pos = self.he(next).vertex; // h.next.vertex — off-loop vertex in h's face
        let twin = he.twin;
        if twin == NO_TWIN || twin.0 as usize >= self.half_edges.len() { return None; } // boundary: one rail
        let tprev = self.he(twin).prev;
        if tprev == NO_EDGE || tprev.0 as usize >= self.half_edges.len() { return None; }
        let rail_neg = self.he_source(tprev); // source of twin.prev — off-loop vertex in the twin's face
        Some((rail_pos, rail_neg))
    }

    /// Coherently-oriented edge-slide rails for a SET of edges (an edge loop/ring selection). Walks the
    /// selection into ONE consistently-directed sequence so every vertex's +rail lands on the SAME side of
    /// the loop — fixes an edge slide where "one side goes the wrong direction" because the raw selection's
    /// half-edges were inconsistently oriented (some h, some twin). Returns `(vertex, +rail, −rail)` per
    /// covered loop vertex. Disjoint / non-walkable leftovers fall back to their raw per-edge rails but are
    /// geometrically aligned to the first vertex's +rail direction so a parallel RING still slides uniformly.
    pub fn edge_slide_rails_loop(&self, edges: &[HalfEdgeId]) -> Vec<(VertexId, VertexId, VertexId)> {
        use std::collections::{HashMap, HashSet};
        // Directed half-edge lookup + undirected adjacency over the selection.
        let mut dir: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
        let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
        for &e in edges {
            if e.0 as usize >= self.half_edges.len() { continue; }
            let d = self.he(e).vertex;
            let s = self.he_source(e);
            if s == d { continue; }
            dir.insert((s.0, d.0), e);
            let t = self.he(e).twin;
            if t != NO_TWIN { dir.insert((d.0, s.0), t); }
            adj.entry(s.0).or_default().push(d.0);
            adj.entry(d.0).or_default().push(s.0);
        }
        let mut out: Vec<(VertexId, VertexId, VertexId)> = Vec::new();
        let mut covered: HashSet<u32> = HashSet::new();
        let mut used: HashSet<(u32, u32)> = HashSet::new();
        // Walk each connected chain: from a start vertex, follow unused incident edges. Each step's
        // directed half-edge yields coherent rails for its DESTINATION vertex.
        let starts: Vec<u32> = {
            let mut s: Vec<u32> = adj.keys().copied().collect();
            s.sort_unstable(); // determinism
            s
        };
        let mut walked: Vec<HalfEdgeId> = Vec::new();
        for &s0 in &starts {
            let mut cur = s0;
            loop {
                let next = adj.get(&cur).and_then(|ns| {
                    ns.iter().copied().find(|&n| !used.contains(&(cur.min(n), cur.max(n))))
                });
                let n = match next { Some(n) => n, None => break };
                used.insert((cur.min(n), cur.max(n)));
                if let Some(&h) = dir.get(&(cur, n)) {
                    walked.push(h);
                    if let Some((rp, rn)) = self.edge_slide_rails(h) {
                        if covered.insert(n) { out.push((VertexId(n), rp, rn)); }
                    }
                }
                cur = n;
                if cur == s0 { break; }
            }
        }
        // Cover the SOURCE endpoint of any walked edge (the START of an open chain / a single edge, whose
        // walk only covered its destination) with TOPOLOGICALLY coherent rails — its +rail in the SAME face
        // as the walked edge's +rail. Using the twin's rails + a geometric world-direction test flipped the
        // wrong way at a corner, where the two ends' natural +rails point in OPPOSITE world directions (the
        // "triple corner won't slide both ways" bug). +rail = the vertex before the source in h's face;
        // −rail = the vertex after it in the twin face. A closed loop already covers every source (no-op).
        for &h in &walked {
            let b = self.he_source(h);
            if covered.contains(&b.0) { continue; }
            let hp = self.he(h).prev;
            let twin = self.he(h).twin;
            if hp == NO_EDGE || hp.0 as usize >= self.half_edges.len() || twin == NO_TWIN { continue; }
            let tn = self.he(twin).next;
            if tn == NO_EDGE || tn.0 as usize >= self.half_edges.len() { continue; }
            let rail_pos = self.he_source(hp); // before B in h's face — same side as A's +rail
            let rail_neg = self.he(tn).vertex; // after B in the twin face
            if covered.insert(b.0) { out.push((b, rail_pos, rail_neg)); }
        }
        // Disjoint edges the walk never reached (a parallel RING of unconnected edges): raw per-edge rails,
        // geometrically aligned to the first covered vertex's +rail so the ring slides uniformly.
        let ref_dir = out.first().map(|&(v, rp, _)| {
            (self.vert(rp).position - self.vert(v).position).normalize_or_zero()
        });
        for &e in edges {
            if e.0 as usize >= self.half_edges.len() { continue; }
            let twin = self.he(e).twin;
            let halves = if twin != NO_TWIN { [Some(e), Some(twin)] } else { [Some(e), None] };
            for h in halves.into_iter().flatten() {
                let d = self.he(h).vertex;
                if covered.contains(&d.0) { continue; }
                if let Some((rp, rn)) = self.edge_slide_rails(h) {
                    let (mut a, mut b) = (rp, rn);
                    if let Some(rd) = ref_dir {
                        let dp = (self.vert(rp).position - self.vert(d).position).normalize_or_zero();
                        if dp.dot(rd) < 0.0 { std::mem::swap(&mut a, &mut b); } // flip so +rail ∥ reference
                    }
                    if covered.insert(d.0) { out.push((d, a, b)); }
                }
            }
        }
        out
    }

    /// Ordered vertices around a face.
    pub fn face_vertices(&self, f: FaceId) -> Vec<VertexId> {
        let mut result = Vec::new();
        self.face_vertices_into(f, &mut result);
        result
    }

    /// [`Self::face_vertices`] into a caller-owned buffer — same walk, same guards, zero
    /// allocation when the buffer is reused across faces. The allocating form wraps this one
    /// so there is exactly ONE face-ring walk (reuse rule).
    pub fn face_vertices_into(&self, f: FaceId, result: &mut Vec<VertexId>) {
        result.clear();
        let start = self.face(f).half_edge;
        if start == NO_EDGE { return; }
        if start.0 as usize >= self.half_edges.len() { return; }
        let mut cur = start;
        let max_iter = self.half_edges.len(); // safety: prevent infinite loops
        loop {
            result.push(self.he(cur).vertex);
            cur = self.he(cur).next;
            if cur == start || cur == NO_EDGE || cur.0 as usize >= self.half_edges.len() { break; }
            if result.len() >= max_iter { break; } // corrupted topology guard
        }
    }

    /// Ordered half-edges around a face.
    pub fn face_half_edges(&self, f: FaceId) -> Vec<HalfEdgeId> {
        let mut result = Vec::new();
        self.face_half_edges_into(f, &mut result);
        result
    }

    /// [`Self::face_half_edges`] into a caller-owned buffer (see [`Self::face_vertices_into`]).
    pub fn face_half_edges_into(&self, f: FaceId, result: &mut Vec<HalfEdgeId>) {
        result.clear();
        let start = self.face(f).half_edge;
        if start == NO_EDGE { return; }
        if start.0 as usize >= self.half_edges.len() { return; }
        let mut cur = start;
        let max_iter = self.half_edges.len(); // safety: prevent infinite loops
        loop {
            result.push(cur);
            cur = self.he(cur).next;
            if cur == start || cur == NO_EDGE || cur.0 as usize >= self.half_edges.len() { break; }
            if result.len() >= max_iter { break; } // corrupted topology guard
        }
    }

    /// The two faces adjacent to an edge (via half-edge and its twin).
    pub fn edge_faces(&self, he: HalfEdgeId) -> (FaceId, FaceId) {
        let f1 = self.he(he).face;
        let twin = self.he(he).twin;
        let f2 = if twin == NO_TWIN { NO_FACE } else { self.he(twin).face };
        (f1, f2)
    }

    /// Every UNDIRECTED edge of the mesh exactly once, as its **canonical** half-edge id:
    /// the LOWER id of a twin pair, or the id itself when there is no twin.
    ///
    /// This is the ONE definition of "walk the edges" for selection, picking and overlays.
    /// Those systems must agree on which half of a pair carries an edge's identity — an edge
    /// that one path enumerates and another does not is an edge the user can see and hover but
    /// cannot select (or vice-versa).
    ///
    /// **Liveness is the only filter — never `face == NO_FACE`.** A live faceless half-edge is
    /// a real, selectable segment: `dissolve_face` clears the face back-pointer and KEEPS the
    /// half-edge, and Delete-Only-Faces / a knife cut that stops inside a face produce the same
    /// thing. The open-coded rule this replaces (`if he.face == NO_FACE { continue }` followed
    /// by a min-of-pair test) DROPPED such an edge outright whenever the faceless half held the
    /// LOWER id: the faceless half failed the face test, and its faced twin then failed the
    /// min-of-pair test. On any OPEN mesh that silently made the rim edges unselectable by box /
    /// lasso / paint / select-all / invert / select-linked, while they stayed hoverable,
    /// clickable and drawn. Callers that genuinely need face context must ask for it per edge
    /// (see [`Self::edge_faces`]) instead of filtering the walk.
    /// The **canonical** id of the undirected edge `he` belongs to: the LOWER id of the twin
    /// pair, or `he` itself when it has no twin. Per-edge form of [`Self::canonical_edges`] —
    /// the two MUST agree, or an edge gets enumerated under one id and tested under another.
    ///
    /// Caller must pass a live, in-range half-edge.
    pub fn canonical_edge(&self, he: HalfEdgeId) -> HalfEdgeId {
        let twin = self.he(he).twin;
        if twin != NO_TWIN && twin.0 < he.0 {
            twin
        } else {
            he
        }
    }

    pub fn canonical_edges(&self) -> impl Iterator<Item = HalfEdgeId> + '_ {
        (0..self.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(move |&he| {
                if !self.is_edge_live(he) {
                    return false;
                }
                let twin = self.he(he).twin;
                twin == NO_TWIN || he.0 < twin.0
            })
    }

    /// Number of edges emanating from a vertex.
    pub fn vertex_valence(&self, v: VertexId) -> u32 {
        self.vertex_fan(v).len() as u32
    }

    /// True if this vertex is on a boundary (has at least one boundary edge).
    pub fn is_boundary_vertex(&self, v: VertexId) -> bool {
        self.vertex_fan(v).iter().any(|&h| self.he(h).twin == NO_TWIN)
    }

    /// True if this half-edge is on a boundary.
    pub fn is_boundary_edge(&self, he: HalfEdgeId) -> bool {
        self.he(he).twin == NO_TWIN
    }

    /// Follow quad-dominant edge loops: traverse next→twin→next→twin...
    pub fn select_edge_loop(&self, start: HalfEdgeId) -> Vec<HalfEdgeId> {
        let mut result = Vec::new();
        let mut cur = start;
        loop {
            result.push(cur);
            // Advance: go to opposite face via twin, then skip one edge
            let twin = self.he(cur).twin;
            if twin == NO_TWIN { break; }
            let face_edges = self.face_half_edges(self.he(twin).face);
            if face_edges.len() != 4 { break; } // only works for quads
            // Find twin in the face's edge list, advance by 2
            let pos = face_edges.iter().position(|&h| h == twin).unwrap_or(0);
            cur = face_edges[(pos + 2) % 4];
            if cur == start { break; }
        }
        result
    }

    /// Follow edge rings: traverse twin→prev→twin→prev...
    pub fn select_edge_ring(&self, start: HalfEdgeId) -> Vec<HalfEdgeId> {
        let mut result = Vec::new();
        let mut cur = start;
        loop {
            result.push(cur);
            let next_he = self.he(cur).next;
            let face_edges = self.face_half_edges(self.he(cur).face);
            if face_edges.len() != 4 { break; }
            // Cross to opposite edge in the quad
            let pos = face_edges.iter().position(|&h| h == cur).unwrap_or(0);
            let opposite = face_edges[(pos + 2) % 4];
            let twin = self.he(opposite).twin;
            if twin == NO_TWIN { break; }
            cur = twin;
            if cur == start { break; }
            let _ = next_he; // suppress warning
        }
        result
    }

    /// Every half-edge on the OUTER BOUNDARY of a face region: one that lies in a selected face but
    /// whose twin is absent (a mesh boundary) or belongs to a face OUTSIDE the selection. Shared
    /// interior edges of the selection are therefore excluded.
    ///
    /// THE single region-boundary rule — [`inset_region_offset_dirs`](Self::inset_region_offset_dirs),
    /// [`inset_region`](Self::inset_region) and [`circle_region`](Self::circle_region) all read it, so
    /// "what is the boundary of this selection" is defined in exactly ONE place (CLAUDE.md: never two
    /// code paths for one operation).
    ///
    /// It is also exactly Blender's own definition: LoopTools' `circle_get_input` keeps the selected
    /// faces' edge-keys that are used by exactly ONE selected face.
    ///
    /// Deterministic: faces are sorted before the walk (HashSet iteration order is random —
    /// POLY_POLISH_PLAN §1).
    pub fn region_boundary_half_edges(&self, faces: &std::collections::HashSet<FaceId>) -> Vec<HalfEdgeId> {
        let mut sorted: Vec<FaceId> = faces.iter().copied()
            .filter(|&f| (f.0 as usize) < self.faces.len() && self.face(f).half_edge != NO_EDGE)
            .collect();
        sorted.sort_by_key(|f| f.0);
        let mut bnd: Vec<HalfEdgeId> = Vec::new();
        for &f in &sorted {
            for he in self.face_half_edges(f) {
                let t = self.he(he).twin;
                let outside = t == NO_TWIN || !faces.contains(&self.he(t).face);
                if outside { bnd.push(he); }
            }
        }
        bnd
    }

    /// The region's boundary half-edges traced into ORDERED, closed vertex loops (one per connected
    /// boundary component: 1 for a disk-like selection, 2+ for an annulus).
    ///
    /// Returns EMPTY if the boundary is not a set of simple closed loops — i.e. some vertex has more
    /// than one outgoing boundary half-edge (a PINCH, where the region touches itself at a vertex) or
    /// a walk dead-ends. Such a region has no well-defined loop to fit a circle to, so the caller must
    /// treat it as a safe no-op rather than emit garbage — the same policy `dissolve_faces_region`
    /// uses for non-disk regions.
    ///
    /// The loops are traced in the region's own winding direction. Deterministic (the walk starts from
    /// [`region_boundary_half_edges`](Self::region_boundary_half_edges), which is sorted).
    pub fn region_boundary_loops(&self, faces: &std::collections::HashSet<FaceId>) -> Vec<Vec<VertexId>> {
        use std::collections::{HashMap, HashSet};
        let bnd = self.region_boundary_half_edges(faces);
        if bnd.is_empty() { return Vec::new(); }
        // src vertex -> the boundary half-edge LEAVING it (continues the loop). A duplicate source =
        // a pinch vertex: the loop is ambiguous, so refuse the whole region.
        let mut out_of: HashMap<u32, HalfEdgeId> = HashMap::new();
        for &he in &bnd {
            if out_of.insert(self.he_source(he).0, he).is_some() { return Vec::new(); }
        }
        let mut visited: HashSet<u32> = HashSet::new();
        let mut loops: Vec<Vec<VertexId>> = Vec::new();
        for &start in &bnd {
            if visited.contains(&start.0) { continue; }
            let mut ring: Vec<VertexId> = Vec::new();
            let mut cur = start;
            loop {
                if !visited.insert(cur.0) { break; }
                ring.push(self.he(cur).vertex);
                match out_of.get(&self.he(cur).vertex.0) {
                    Some(&nx) if nx == start => break, // closed
                    Some(&nx) => cur = nx,
                    None => return Vec::new(), // dead end — not a closed loop
                }
            }
            if ring.len() < 3 { return Vec::new(); }
            loops.push(ring);
        }
        loops
    }


    /// The SELECTED EDGES traced into ORDERED, closed vertex loops — the edge-mode counterpart of
    /// [`region_boundary_loops`](Self::region_boundary_loops), mirroring its safety EXACTLY so the two
    /// selection domains hand the SAME shape of input to [`circle_reshape_loops`](Self::circle_reshape_loops).
    ///
    /// `edges` holds [`HalfEdgeId`] u32s (as the app's `selected_edges` does). Each geometric edge is a
    /// half-edge + its twin; both may be present, so we dedup by the UNDIRECTED `{min,max}` endpoint
    /// pair before building adjacency.
    ///
    /// Refuses (returns EMPTY, caller treats as a no-op) exactly when the selection is not a set of
    /// simple closed loops:
    /// * any vertex touched by `!= 2` selected edges — an open end (1), a fork/pinch (>2) — the loop is
    ///   ambiguous, same policy as `region_boundary_loops`' duplicate-source refusal;
    /// * a walk that dead-ends before closing, or any ring shorter than 3.
    ///
    /// Deterministic: dead/out-of-range half-edge ids are dropped, endpoint pairs are canonicalised,
    /// and the per-component walk starts from the lowest-id endpoint.
    pub fn edge_loop_rings(&self, edges: &std::collections::HashSet<HalfEdgeId>) -> Vec<Vec<VertexId>> {
        use std::collections::{HashMap, HashSet};

        // Dedup he+twin to undirected geometric edges keyed by {min,max} endpoint vertex.
        //
        // DETERMINISM (fixed 2026-08-07 — this was a real, reproducible corruption source):
        // `edges` is a HashSet, so iterating it directly gave `geom.entry().or_insert()` a RANDOM
        // winner between a half-edge and its twin. That decided each ring's ORIENTATION, which in
        // turn decided the winding `bridge_loops` picks — so the SAME selection bridged differently
        // from run to run. Measured via AZRA_BRIDGEDIAG=edges2: 5 identical runs, 4 refused and 1
        // committed a NON-MANIFOLD mesh, because the app previews on a clone and then re-runs the
        // op on the real mesh (two rolls of the same dice). CLAUDE.md: "HashMap iteration = random
        // order. Sort when order matters." It orders here.
        let mut ids: Vec<HalfEdgeId> = edges.iter().copied().collect();
        ids.sort_unstable_by_key(|h| h.0);
        let mut geom: HashMap<(u32, u32), (VertexId, VertexId)> = HashMap::new();
        for &he in &ids {
            if !self.is_edge_live(he) { continue; }
            let a = self.he_source(he);
            let b = self.he(he).vertex;
            if a == b { continue; } // degenerate — no endpoints to walk
            let key = (a.0.min(b.0), a.0.max(b.0));
            geom.entry(key).or_insert((a, b));
        }
        if geom.is_empty() { return Vec::new(); }

        // Undirected adjacency: each vertex -> its neighbours across selected edges. A vertex with a
        // degree other than 2 makes the loop ambiguous => refuse the WHOLE selection.
        // `geom.values()` is ALSO HashMap-ordered, so build adjacency from a sorted key list and
        // sort each neighbour pair — otherwise the walk below could still leave a vertex in either
        // direction depending on hash layout.
        let mut keys: Vec<(u32, u32)> = geom.keys().copied().collect();
        keys.sort_unstable();
        let mut adj: HashMap<u32, Vec<VertexId>> = HashMap::new();
        for k in &keys {
            let (a, b) = geom[k];
            adj.entry(a.0).or_default().push(b);
            adj.entry(b.0).or_default().push(a);
        }
        for ns in adj.values_mut() {
            if ns.len() != 2 { return Vec::new(); }
            ns.sort_unstable_by_key(|v| v.0);
        }

        // Walk each connected component into a closed ring. Deterministic start = lowest vertex id.
        let mut starts: Vec<u32> = adj.keys().copied().collect();
        starts.sort();
        let mut visited: HashSet<u32> = HashSet::new();
        let mut loops: Vec<Vec<VertexId>> = Vec::new();
        for &s in &starts {
            if visited.contains(&s) { continue; }
            let mut ring: Vec<VertexId> = Vec::new();
            let mut cur = s;
            let mut prev: Option<u32> = None;
            loop {
                if !visited.insert(cur) {
                    // Closed the loop when we returned to the start; any other revisit = tangle.
                    if cur == s { break; }
                    return Vec::new();
                }
                ring.push(VertexId(cur));
                let ns = match adj.get(&cur) { Some(n) => n, None => return Vec::new() };
                // Degree is exactly 2 here; step to the neighbour we did not come from.
                let nxt = if Some(ns[0].0) == prev { ns[1].0 } else { ns[0].0 };
                prev = Some(cur);
                cur = nxt;
                if cur == s { break; } // closed
            }
            if ring.len() < 3 { return Vec::new(); }
            loops.push(ring);
        }
        loops
    }


    /// Boundary flag for EVERY vertex at once — the batch form of
    /// [`vertex_is_boundary`](Self::vertex_is_boundary), indexed by `VertexId.0`.
    ///
    /// Per-vertex results are IDENTICAL to calling `vertex_is_boundary` on each vertex, because that
    /// function's exhaustive scan is a superset of its own fan checks: a fan half-edge with
    /// `twin == NO_TWIN` is a live half-edge *sourced* at `v` with no twin, and an asymmetric-twin
    /// case (`he(tw).twin == NO_TWIN`) has `tw` *incoming* to `v` — the scan's two endpoint clauses
    /// cover both. So "any live half-edge with no twin, marked at both endpoints" is the whole
    /// predicate, and one pass computes it for the entire mesh.
    ///
    /// **Use this whenever you need the flag for more than one vertex.** `vertex_is_boundary` is
    /// O(half_edges) per call (its scan cannot be skipped without changing the answer on a bowtie
    /// vertex, whose second fan the disk walk never reaches), so a per-vertex loop is O(V × HE):
    /// `loop_relax` on a 9216-face mesh measured 319 ms with `lock_boundary = true` versus 1.76 ms
    /// without — 317 ms of that was this predicate.
    pub fn boundary_vertex_mask(&self) -> Vec<bool> {
        let mut mask = vec![false; self.vertices.len()];
        for i in 0..self.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !self.is_edge_live(h) || self.he(h).twin != NO_TWIN {
                continue;
            }
            for v in [self.he(h).vertex, self.he_source(h)] {
                if let Some(slot) = mask.get_mut(v.0 as usize) {
                    *slot = true;
                }
            }
        }
        mask
    }

    /// True if any live half-edge incident to `v` is a mesh boundary (`twin == NO_TWIN`).
    ///
    /// O(half_edges) — see [`boundary_vertex_mask`](Self::boundary_vertex_mask) for the batch form,
    /// which every multi-vertex caller should prefer.
    pub fn vertex_is_boundary(&self, v: VertexId) -> bool {
        for h in self.vertex_fan(v) {
            if self.he(h).twin == NO_TWIN {
                return true;
            }
            // Also check the reverse half of each edge.
            let tw = self.he(h).twin;
            if tw != NO_TWIN && self.he(tw).twin == NO_TWIN {
                return true;
            }
        }
        // Fan may miss pure boundary when only reverse is boundary-marked — scan free-safe.
        for i in 0..self.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !self.is_edge_live(h) {
                continue;
            }
            if self.he(h).twin != NO_TWIN {
                continue;
            }
            if self.he(h).vertex == v || self.he_source(h) == v {
                return true;
            }
        }
        false
    }

    // ============================================================
    // Open-border (rim) navigation — the two directions an open boundary leaves a vertex in.
    //
    // `vertex_fan` CANNOT answer this. It walks `twin → next` and BREAKS the moment it meets
    // `twin == NO_TWIN` (line 35, commented `// boundary`), so on a boundary vertex it returns
    // only the edges on ONE side of the gap and stops exactly AT the outgoing boundary edge.
    // The INCOMING boundary edge lies on the FAR side of that gap and is structurally
    // unreachable from that walk. Everything below exists because of that one fact.
    // ============================================================

    /// Walk the vertex disk FORWARD from the outgoing half-edge `h` and return the OUTGOING
    /// boundary half-edge of the fan sector `h` sits in — `None` when that sector closes (an
    /// interior vertex) or the topology is malformed.
    ///
    /// Same `twin → next` step as [`Self::vertex_fan_into`], but **seeded**: it starts at a
    /// caller-chosen half-edge instead of `vertex.half_edge`. The seed is the whole point. At a
    /// pinch vertex (two rims meeting at one point) the disk has more than one sector and only
    /// the sector containing the half-edge we ARRIVED on continues the rim being walked;
    /// `vertex.half_edge` names an arbitrary sector. `vertex_fan_into` takes no seed and its
    /// callers depend on the disk-pointer start, so the step is re-walked here rather than
    /// shared.
    fn sector_boundary_out(&self, h: HalfEdgeId) -> Option<HalfEdgeId> {
        if !self.is_edge_live(h) { return None; }
        let start = h;
        let mut cur = h;
        for _ in 0..self.half_edges.len() {
            let t = self.he(cur).twin;
            if t == NO_TWIN { return Some(cur); } // the gap — this is the outgoing rim edge
            if !self.is_edge_live(t) { return None; }
            cur = self.he(t).next;
            if !self.is_edge_live(cur) || cur == start { return None; } // closed disk = interior
        }
        None
    }

    /// Walk the vertex disk BACKWARD from the outgoing half-edge `h` and return the INCOMING
    /// boundary half-edge of the fan sector `h` sits in — `None` when that sector closes.
    ///
    /// The step is `prev` (the half-edge ENDING at the vertex inside the current face) then
    /// `twin` (the next outgoing half-edge the other way round the disk). This is the half no
    /// fan walk can reach — see the section banner.
    fn sector_boundary_in(&self, h: HalfEdgeId) -> Option<HalfEdgeId> {
        if !self.is_edge_live(h) { return None; }
        let start = h;
        let mut cur = h;
        for _ in 0..self.half_edges.len() {
            let p = self.he(cur).prev;
            if !self.is_edge_live(p) { return None; }
            let t = self.he(p).twin;
            if t == NO_TWIN { return Some(p); } // the gap — this is the incoming rim edge
            if !self.is_edge_live(t) { return None; }
            cur = t;
            if cur == start { return None; } // closed disk = interior
        }
        None
    }

    /// The two BOUNDARY half-edges at `v`, as `(outgoing, incoming)`:
    ///
    /// * `.0` — the live half-edge SOURCED at `v` with no twin (`he_source(h) == v`).
    /// * `.1` — the live half-edge ENDING at `v` with no twin (`he(h).vertex == v`).
    ///
    /// Together they are the two directions the open border leaves `v` in, which is exactly
    /// what a rim walk (and, later, a mouse-direction test) has to choose between. `(None,
    /// None)` = an interior vertex, a dead/loose vertex, or an out-of-range id. Exactly one
    /// `Some` = a rim that dead-ends at `v` (a torn / wire-edge chain end).
    ///
    /// **Pinch vertices:** answered for the ONE sector `vertex.half_edge` happens to name. A
    /// vertex where two rims meet has more than one such pair and this returns a single one of
    /// them — callers that must distinguish them should walk from a known rim half-edge with
    /// [`Self::boundary_walk_from`] instead of re-entering here.
    pub fn boundary_edges_at(&self, v: VertexId) -> (Option<HalfEdgeId>, Option<HalfEdgeId>) {
        if !self.is_vert_live(v) { return (None, None); }
        let seed = self.vert(v).half_edge;
        if !self.is_edge_live(seed) { return (None, None); }
        (self.sector_boundary_out(seed), self.sector_boundary_in(seed))
    }

    /// The next `k` boundary vertices along the open border, starting at `v` and leaving it via
    /// the boundary half-edge `start` (one of [`Self::boundary_edges_at`]'s two answers).
    ///
    /// `start` also picks the DIRECTION, so one function serves both ways round the rim:
    /// * `start` sourced at `v` (the outgoing rim edge) → walk with the border's own direction;
    /// * `start` ending at `v` (the incoming rim edge) → walk against it.
    ///
    /// The two results are exact reverses of each other on a clean rim. The walk stops early
    /// when it comes back to `v` — and `v` IS pushed as the final element in that case, so the
    /// caller can tell "the rim closed in n steps" from "the rim is longer than k". It also
    /// stops at a dead end (a rim that terminates) rather than guessing a continuation.
    ///
    /// Each step is seeded from the half-edge we arrived on, so a pinch vertex continues THIS
    /// rim instead of jumping to whichever sector the pinch's disk pointer names.
    pub fn boundary_walk_from(&self, v: VertexId, start: HalfEdgeId, k: usize) -> Vec<VertexId> {
        let mut out: Vec<VertexId> = Vec::new();
        if k == 0 || !self.is_vert_live(v) || !self.is_edge_live(start) { return out; }
        if self.he(start).twin != NO_TWIN { return out; } // not a boundary half-edge
        let forward = self.he_source(start) == v;
        if !forward && self.he(start).vertex != v { return out; } // `start` is not incident to `v`
        let mut cur = start;
        let mut at = v;
        for _ in 0..k {
            let nxt = if forward { self.he(cur).vertex } else { self.he_source(cur) };
            if nxt == at || !self.is_vert_live(nxt) { break; } // self-loop / dead slot
            out.push(nxt);
            if nxt == v { break; } // the rim closed back onto the seed
            at = nxt;
            let step = if forward {
                // `cur.next` is outgoing at `nxt` inside `cur`'s own face — the same sector.
                self.sector_boundary_out(self.he(cur).next)
            } else {
                // `cur` is itself outgoing at `nxt`.
                self.sector_boundary_in(cur)
            };
            match step {
                Some(h) if h != cur => cur = h,
                _ => break, // the rim dead-ends here
            }
        }
        out
    }

    /// The ring of EXISTING corners that closes the open border at `v`, wound so a face built
    /// from it TWINS with the border it welds to — or `None` when the rim cannot supply at
    /// least three distinct corners.
    ///
    /// `start` selects the side of `v` to close (see [`Self::boundary_walk_from`]);
    /// `max_corners` caps the result, so `4` yields one quad and leaves a longer hole open
    /// (never an n-gon — whole-loop filling is Grid Fill's job).
    ///
    /// **The winding is topological, not geometric, and that is load-bearing.** A rim
    /// half-edge runs in its adjacent face's direction, so the face that fills the hole must
    /// run OPPOSITE it — `create_face` twins `a→b` by looking up the existing `b→a`
    /// ([`Self::create_face_no_normals`]), and a ring wound the same way as the rim would
    /// instead duplicate every directed edge and leave the new face unattached. (Same rule
    /// `cap_boundary_loops` states as "a cap filling the hole must be wound in REVERSE".)
    /// Every ring edge but the last is therefore backed by an existing rim half-edge; the last
    /// one closes `ring[n-1] → ring[0]` and is a NEW chord unless the rim closed exactly.
    ///
    /// Pure topology — no geometry is inspected, so a caller that cares about area or
    /// self-overlap must test the returned ring itself.
    pub fn boundary_closing_ring(
        &self,
        v: VertexId,
        start: HalfEdgeId,
        max_corners: usize,
    ) -> Option<Vec<VertexId>> {
        if max_corners < 3 { return None; }
        let mut corners = self.boundary_walk_from(v, start, max_corners - 1);
        if corners.last() == Some(&v) { corners.pop(); } // drop the "rim closed" marker
        if corners.len() < 2 { return None; } // fewer than 3 corners total — no face to make
        // A corner repeated (or equal to `v`) means the rim revisits a vertex within the span
        // we are about to weld — the face would be degenerate. Refuse rather than emit it.
        let mut seen = std::collections::HashSet::with_capacity(corners.len() + 1);
        seen.insert(v.0);
        if !corners.iter().all(|c| seen.insert(c.0)) { return None; }

        // Reverse the border's own direction; keep the reverse walk as-is (it already runs
        // against it).
        let forward = self.he_source(start) == v;
        let mut ring = Vec::with_capacity(corners.len() + 1);
        if forward {
            corners.reverse();
            ring.extend(corners);
            ring.push(v);
        } else {
            ring.push(v);
            ring.extend(corners);
        }
        Some(ring)
    }

}

/// Open-border navigation gates — Quad Fill P1
/// (`docs/planning/poly/QUAD_FILL_SINGLE_VERTEX_PLAN.md` §3).
#[cfg(test)]
mod tests {
    use super::*;

    /// A 5x5 quad plane with the CENTRE quad deleted: a 4-corner hole with plenty of grid left
    /// over, so vertices that are neither hole corners nor outer rim still exist. Returns the
    /// mesh and the hole's 4 corners in the deleted face's own order.
    fn plane_with_square_hole() -> (HalfEdgeMesh, Vec<VertexId>) {
        let mut m = HalfEdgeMesh::new_plane(5.0, 5.0, 5, 5);
        let centre = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                m.is_face_live(f)
                    && m.face_vertices(f).iter().all(|&v| {
                        let p = m.vert(v).position;
                        p.x.abs() < 1.0 && p.z.abs() < 1.0
                    })
            })
            .expect("centre quad");
        let corners = m.face_vertices(centre);
        assert_eq!(corners.len(), 4);
        m.delete_face(centre);
        (m, corners)
    }

    /// The same plane with TWO adjacent quads deleted: a 6-vertex hole, every rim vertex an
    /// interior grid vertex (so the hole never touches the outer rim).
    fn plane_with_six_vertex_hole() -> HalfEdgeMesh {
        let mut m = HalfEdgeMesh::new_plane(5.0, 5.0, 5, 5);
        // Grid lines sit at -2.5, -1.5, -0.5, 0.5, 1.5, 2.5. This window selects exactly the two
        // centre-row quads spanning x = -1.5..-0.5 and -0.5..0.5.
        let doomed: Vec<FaceId> = (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| {
                m.is_face_live(f)
                    && m.face_vertices(f).iter().all(|&v| {
                        let p = m.vert(v).position;
                        (-1.6..=0.6).contains(&p.x) && p.z.abs() < 1.0
                    })
            })
            .collect();
        assert_eq!(doomed.len(), 2, "expected exactly two centre-row quads");
        for f in doomed {
            m.delete_face(f);
        }
        m
    }

    /// A CLOSED sphere with one pole triangle deleted: a 3-vertex hole whose three rim edges are
    /// all interior and whose three corners stay edge-manifold.
    ///
    /// A subdivided triangle with its middle face removed does NOT work as a fixture — its
    /// corners become bowties (`is_manifold`: "incident faces form 2 disjoint fans"), the hole
    /// merges with the outer rim, and a correct rim walk therefore walks straight off the hole.
    fn sphere_with_triangular_hole() -> (HalfEdgeMesh, Vec<VertexId>) {
        let mut m = HalfEdgeMesh::new_uv_sphere(1.0, 8, 4);
        let tri = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| m.is_face_live(f) && m.face_vertices(f).len() == 3)
            .expect("a pole triangle");
        let corners = m.face_vertices(tri);
        assert_eq!(corners.len(), 3);
        m.delete_face(tri);
        assert!(m.is_manifold().is_ok(), "fixture must stay manifold: {:?}", m.is_manifold());
        (m, corners)
    }

    /// The count of live rim half-edges reachable from a vertex pair — used to prove a fill
    /// actually consumed boundary.
    fn boundary_half_edges(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).twin == NO_TWIN)
            .count()
    }

    // ---------- P1 gate: boundary_edges_at ----------

    #[test]
    fn boundary_edges_at_finds_both_rim_edges_at_a_hole_corner() {
        let (m, corners) = plane_with_square_hole();
        for &c in &corners {
            let (out, inc) = m.boundary_edges_at(c);
            let out = out.unwrap_or_else(|| panic!("corner {:?}: no outgoing rim edge", c));
            let inc = inc.unwrap_or_else(|| panic!("corner {:?}: no incoming rim edge", c));
            assert_ne!(out, inc, "the two rim edges must be distinct half-edges");
            assert_eq!(m.he(out).twin, NO_TWIN);
            assert_eq!(m.he(inc).twin, NO_TWIN);
            assert_eq!(m.he_source(out), c, "`.0` must be SOURCED at the vertex");
            assert_eq!(m.he(inc).vertex, c, "`.1` must END at the vertex");
        }
    }

    #[test]
    fn boundary_edges_at_returns_none_for_an_interior_vertex() {
        let (m, corners) = plane_with_square_hole();
        // An interior grid vertex that is neither a hole corner nor on the outer rim.
        let interior = (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                m.is_vert_live(v) && !corners.contains(&v) && {
                    let p = m.vert(v).position;
                    p.x.abs() < 2.4 && p.z.abs() < 2.4
                }
            })
            .expect("an interior non-corner vertex");
        assert_eq!(
            m.boundary_edges_at(interior),
            (None, None),
            "interior vertex {:?} must report no rim edges",
            interior
        );
        assert!(!m.vertex_is_boundary(interior), "sanity: really interior");
    }

    #[test]
    fn boundary_edges_at_is_none_for_dead_and_out_of_range_ids() {
        let (m, _) = plane_with_square_hole();
        assert_eq!(m.boundary_edges_at(VertexId(u32::MAX)), (None, None));
    }

    // ---------- P1 gate: the walk ----------

    #[test]
    fn walk_from_a_hole_corner_returns_the_other_three_in_both_directions() {
        let (m, corners) = plane_with_square_hole();
        for &c in &corners {
            let (out, inc) = m.boundary_edges_at(c);
            let fwd = m.boundary_walk_from(c, out.unwrap(), 3);
            let bwd = m.boundary_walk_from(c, inc.unwrap(), 3);
            assert_eq!(fwd.len(), 3, "forward walk from {:?}", c);
            assert_eq!(bwd.len(), 3, "backward walk from {:?}", c);

            let others: std::collections::HashSet<u32> =
                corners.iter().filter(|&&x| x != c).map(|v| v.0).collect();
            let f_set: std::collections::HashSet<u32> = fwd.iter().map(|v| v.0).collect();
            let b_set: std::collections::HashSet<u32> = bwd.iter().map(|v| v.0).collect();
            assert_eq!(f_set, others, "forward walk must yield the other 3 corners");
            assert_eq!(b_set, others, "backward walk must yield the other 3 corners");

            let mut reversed = fwd.clone();
            reversed.reverse();
            assert_eq!(bwd, reversed, "the two directions must be exact reverses");

            // Rim ORDER, not just membership: each consecutive pair is joined by a rim edge.
            let mut chain = vec![c];
            chain.extend(fwd.iter().copied());
            for w in chain.windows(2) {
                let (a, b) = (w[0], w[1]);
                assert!(
                    (0..m.half_edges.len() as u32).map(HalfEdgeId).any(|h| {
                        m.is_edge_live(h)
                            && m.he(h).twin == NO_TWIN
                            && m.he_source(h) == a
                            && m.he(h).vertex == b
                    }),
                    "{:?} -> {:?} is not a rim edge — the walk left the boundary",
                    a,
                    b
                );
            }
        }
    }

    #[test]
    fn walk_marks_rim_closure_by_returning_the_seed() {
        let (m, corners) = plane_with_square_hole();
        let c = corners[0];
        let (out, _) = m.boundary_edges_at(c);
        // One step past the 4-vertex rim: the 4th entry is the seed itself.
        let w = m.boundary_walk_from(c, out.unwrap(), 8);
        assert_eq!(w.len(), 4, "a 4-vertex rim closes after 4 steps");
        assert_eq!(w[3], c, "closure is reported by pushing the seed back");
    }

    #[test]
    fn walk_on_a_six_vertex_hole_does_not_close_within_three_steps() {
        let m = plane_with_six_vertex_hole();
        // Any hole-rim vertex: an interior grid vertex that became a boundary vertex.
        let v = (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                m.is_vert_live(v) && {
                    let p = m.vert(v).position;
                    p.x.abs() < 2.4 && p.z.abs() < 2.4 && m.boundary_edges_at(v).0.is_some()
                }
            })
            .expect("a hole-rim vertex");
        let (out, _) = m.boundary_edges_at(v);
        let w = m.boundary_walk_from(v, out.unwrap(), 3);
        assert_eq!(w.len(), 3);
        assert!(!w.contains(&v), "a 6-vertex rim must not close within 3 steps");
        let full = m.boundary_walk_from(v, out.unwrap(), 8);
        assert_eq!(full.len(), 6, "the rim is 6 vertices round");
        assert_eq!(full[5], v);
    }

    #[test]
    fn walk_refuses_a_non_boundary_or_unrelated_start() {
        let (m, corners) = plane_with_square_hole();
        let c = corners[0];
        // Scanned, not fanned: `delete_face` repoints the corner's disk pointer AT the rim edge,
        // so `vertex_fan` here returns that single half-edge and never reaches the interior ones.
        let interior_he = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&h| m.is_edge_live(h) && m.he(h).twin != NO_TWIN && m.he_source(h) == c)
            .expect("an interior edge at a hole corner");
        assert!(m.boundary_walk_from(c, interior_he, 3).is_empty());
        // A rim edge that is not incident to `c` at all.
        let other = corners[2];
        let (far, _) = m.boundary_edges_at(other);
        assert!(m.boundary_walk_from(c, far.unwrap(), 3).is_empty());
    }

    // ---------- closing ring: winding is what makes the fill weld ----------

    #[test]
    fn closing_ring_fills_a_square_hole_with_a_welded_quad() {
        let (base, corners) = plane_with_square_hole();
        let rim_before = boundary_half_edges(&base);
        let loops_before = base.mesh_boundary_loops().len();
        assert_eq!(loops_before, 2, "outer rim + the hole");

        for &c in &corners {
            for dir in 0..2 {
                let mut m = base.clone();
                let (out, inc) = m.boundary_edges_at(c);
                let start = if dir == 0 { out.unwrap() } else { inc.unwrap() };
                let ring = m.boundary_closing_ring(c, start, 4).expect("ring");
                assert_eq!(ring.len(), 4);
                let got: std::collections::HashSet<u32> = ring.iter().map(|v| v.0).collect();
                let want: std::collections::HashSet<u32> = corners.iter().map(|v| v.0).collect();
                assert_eq!(got, want, "the ring must be the hole's own 4 corners");

                let fid = m.create_face(&ring).expect("create_face");
                assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
                assert!(m.is_manifold().is_ok(), "is_manifold: {:?}", m.is_manifold());
                assert_eq!(m.face_vertices(fid).len(), 4);
                // WELDED, not merely created: all 4 rim half-edges gained a twin.
                assert_eq!(
                    boundary_half_edges(&m),
                    rim_before - 4,
                    "corner {:?} dir {}: the hole's 4 rim edges must all have twinned",
                    c,
                    dir
                );
                assert_eq!(m.mesh_boundary_loops().len(), 1, "only the outer rim is left");
            }
        }
    }

    #[test]
    fn closing_ring_on_a_six_vertex_hole_fills_only_the_nearest_quad() {
        let base = plane_with_six_vertex_hole();
        let rim_before = boundary_half_edges(&base);
        let v = (0..base.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                base.is_vert_live(v) && {
                    let p = base.vert(v).position;
                    p.x.abs() < 2.4 && p.z.abs() < 2.4 && base.boundary_edges_at(v).0.is_some()
                }
            })
            .expect("a hole-rim vertex");

        for dir in 0..2 {
            let mut m = base.clone();
            let (out, inc) = m.boundary_edges_at(v);
            let start = if dir == 0 { out.unwrap() } else { inc.unwrap() };
            let ring = m.boundary_closing_ring(v, start, 4).expect("ring");
            assert_eq!(ring.len(), 4, "capped at one quad — never an n-gon");
            assert!(ring.contains(&v));
            let fid = m.create_face(&ring).expect("create_face");
            assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
            assert!(m.is_manifold().is_ok(), "is_manifold: {:?}", m.is_manifold());
            assert_eq!(m.face_vertices(fid).len(), 4);
            // 3 of the quad's edges weld to the rim; the 4th is a new chord across the hole,
            // which stays boundary. Net rim change: -3 welded pairs (6 half-edges) +1 chord.
            assert_eq!(boundary_half_edges(&m), rim_before - 3 + 1);
            assert_eq!(m.mesh_boundary_loops().len(), 2, "the rest of the hole stays open");
        }
    }

    #[test]
    fn closing_ring_fills_a_three_vertex_hole_with_a_triangle() {
        let (base, corners) = sphere_with_triangular_hole();
        for &c in &corners {
            for dir in 0..2 {
                let mut m = base.clone();
                let (out, inc) = m.boundary_edges_at(c);
                let start = if dir == 0 { out.unwrap() } else { inc.unwrap() };
                let ring = m
                    .boundary_closing_ring(c, start, 4)
                    .expect("a 3-vertex gap is a legitimate hole, not a decline");
                assert_eq!(ring.len(), 3, "a 3-step closure fills a TRIANGLE");
                let fid = m.create_face(&ring).expect("create_face");
                assert_eq!(m.face_vertices(fid).len(), 3);
                assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
                assert!(m.is_manifold().is_ok(), "is_manifold: {:?}", m.is_manifold());
                assert!(
                    m.mesh_boundary_loops().is_empty(),
                    "the sphere is closed again"
                );
            }
        }
    }

    #[test]
    fn closing_ring_declines_an_interior_vertex_and_a_too_small_cap() {
        let (m, corners) = plane_with_square_hole();
        let c = corners[0];
        let (out, _) = m.boundary_edges_at(c);
        assert!(
            m.boundary_closing_ring(c, out.unwrap(), 2).is_none(),
            "a cap below 3 corners cannot describe a face"
        );
        let interior = (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                m.is_vert_live(v) && !corners.contains(&v) && {
                    let p = m.vert(v).position;
                    p.x.abs() < 2.4 && p.z.abs() < 2.4
                }
            })
            .expect("interior vertex");
        // No rim edge to start from at all — the caller cannot even form the call.
        assert_eq!(m.boundary_edges_at(interior), (None, None));
    }

    #[test]
    fn closing_ring_leaves_the_mesh_untouched() {
        // The whole helper family is read-only: the app's decline paths depend on it, because a
        // refused fill must not eat a Ctrl+Z.
        let (m, corners) = plane_with_square_hole();
        let before = m.content_signature();
        for &c in &corners {
            let (out, inc) = m.boundary_edges_at(c);
            let _ = m.boundary_walk_from(c, out.unwrap(), 8);
            let _ = m.boundary_closing_ring(c, inc.unwrap(), 4);
        }
        assert_eq!(m.content_signature(), before);
    }
}
