// Misc ops: bridge, grid fill, loop utils, spin, rip, mirror/symmetry, smooth, solidify,
// bevel, subdivision, poke, connect, to_sphere (split from half_edge.rs — pure relocation).

use super::*;

/// ONE Jacobi Laplacian pass over a flat CSR 1-ring table: `out[v] = lerp(cur[v], avg(1-ring), f)`.
///
/// `mask`, when present, scales `f` per vertex (`0` = pinned, `1` = full strength) so a modifier
/// can restrict where it deforms. It gathers from the FULL 1-ring either way — a pinned vertex
/// still contributes to its neighbours' averages, which is what makes the masked boundary
/// continuous instead of a crack.
///
/// Split out of [`HalfEdgeMesh::smooth_laplacian`] so the per-vertex gather — which is pure,
/// disjoint-write and reads only the PREVIOUS pass — can be fanned out with rayon on meshes big
/// enough to pay for the fork. Each vertex sums its own neighbours in CSR order regardless of
/// which thread runs it, so the parallel and serial results are bit-identical (not merely close).
fn smooth_jacobi_pass(
    cur: &[Vec3],
    out: &mut [Vec3],
    offs: &[u32],
    deg: &[u32],
    nbr: &[u32],
    f: f32,
    mask: Option<&[f32]>,
) {
    // Below this the rayon fork costs more than the pass. Measured on the 2.5k-quad sphere:
    // parallel was slower until ~10k vertices.
    const PAR_MIN_VERTS: usize = 8192;
    let kernel = |v: usize, o: &mut Vec3| {
        let d = deg[v] as usize;
        if d == 0 {
            *o = cur[v];
            return;
        }
        let s = offs[v] as usize;
        let mut sum = Vec3::ZERO;
        for &nb in &nbr[s..s + d] {
            sum += cur[nb as usize];
        }
        let avg = sum / d as f32;
        let c = cur[v];
        // UNMASKED USES `f` UNTOUCHED — not `f * 1.0`. `poly_smooth_parity.rs` asserts exact
        // `f32` equality against the pre-optimisation reference, and the masked path must not
        // put so much as a redundant multiply in the unmasked one.
        let fv = match mask {
            Some(w) => f * w[v],
            None => f,
        };
        *o = c + (avg - c) * fv;
    };
    if cur.len() >= PAR_MIN_VERTS {
        use rayon::prelude::*;
        out.par_iter_mut()
            .enumerate()
            .for_each(|(v, o)| kernel(v, o));
    } else {
        for (v, o) in out.iter_mut().enumerate() {
            kernel(v, o);
        }
    }
}

impl HalfEdgeMesh {












    /// Bridge two closed vertex loops of equal length with a strip of quads — Blender
    /// `bpy.ops.mesh.bridge_edge_loops` at default settings (no twist, 1 cut, no merge).
    ///
    /// `loop_a` / `loop_b` are ORDERED closed rings (last connects to first). Length must match and
    /// be ≥ 3. The second loop is optimally rotated and optionally reversed to minimise total
    /// endpoint distance (same alignment idea as Blender's pairing). Each pair of corresponding
    /// edges becomes one quad via [`create_face`](Self::create_face) (twins existing boundary HEs
    /// when present — holes seal into a tube; freestanding wire loops become an open strip).
    ///
    /// Returns the new face ids, or empty on refuse (length mismatch / dead verts). Does NOT
    /// mutate on refuse. Oracle: two 8-vert open circles stacked on Z → nv=16 / nf=8 side quads
    /// (`bridge_two_circles_matches_blender_oracle`).
    pub fn bridge_loops(&mut self, loop_a: &[VertexId], loop_b: &[VertexId]) -> Vec<FaceId> {
        let n = loop_a.len();
        if n < 3 || n != loop_b.len() { return Vec::new(); }
        for &v in loop_a.iter().chain(loop_b.iter()) {
            if !self.is_vert_live(v) {
                return Vec::new();
            }
        }
        // Refuse if the two loops share a vertex (would create degenerate zero-area faces).
        {
            use std::collections::HashSet;
            let sa: HashSet<u32> = loop_a.iter().map(|v| v.0).collect();
            if loop_b.iter().any(|v| sa.contains(&v.0)) { return Vec::new(); }
        }

        // Best (rotation, reverse) of loop_b vs fixed loop_a — minimise sum of squared distances.
        let mut best_cost = f32::MAX;
        let mut best_rot = 0usize;
        let mut best_rev = false;
        for rev in [false, true] {
            let b: Vec<VertexId> = if rev {
                loop_b.iter().copied().rev().collect()
            } else {
                loop_b.to_vec()
            };
            for rot in 0..n {
                let mut cost = 0.0f32;
                for i in 0..n {
                    let bi = b[(i + rot) % n];
                    cost += (self.vert(loop_a[i]).position - self.vert(bi).position).length_squared();
                }
                if cost < best_cost {
                    best_cost = cost;
                    best_rot = rot;
                    best_rev = rev;
                }
            }
        }
        let mut b_aligned: Vec<VertexId> = if best_rev {
            loop_b.iter().copied().rev().collect()
        } else {
            loop_b.to_vec()
        };
        b_aligned.rotate_left(best_rot);

        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let a0 = loop_a[i];
            let a1 = loop_a[(i + 1) % n];
            let b0 = b_aligned[i];
            let b1 = b_aligned[(i + 1) % n];
            // create_face twins a NEW he src→dst with an existing boundary he dst→src.
            // If loop edge a0→a1 already exists as a boundary HE (e.g. a filled cap), the new face
            // must walk a1→a0 so it twins instead of double-using the same directed edge
            // (is_manifold "same winding" failure). Same for the b-side. Freestanding wire loops
            // have no existing HE → default [a0,a1,b1,b0].
            let a_bound = self.find_half_edge(a0, a1)
                .map(|h| self.he(h).twin == NO_TWIN).unwrap_or(false);
            let b_bound = self.find_half_edge(b0, b1)
                .map(|h| self.he(h).twin == NO_TWIN).unwrap_or(false);
            let ring = match (a_bound, b_bound) {
                (false, false) => [a0, a1, b1, b0],
                (true, true)   => [a1, a0, b1, b0],
                (true, false)  => [a1, a0, b0, b1],
                (false, true)  => [a0, a1, b1, b0],
            };
            // `create_face_no_normals` — this loop ends in ONE full `recompute_normals()` below,
            // and `create_face` is that same call plus a WHOLE-MESH normal pass PER FACE
            // (`euler.rs:440`). Every one of those passes is overwritten by the trailing one, so
            // they were pure waste that made an n-face bridge cost n full normal passes. Same
            // substitution in every batch builder in this file; the trailing pass is what the
            // output normals actually come from, so the result is bit-identical.
            if let Some(fid) = self.create_face_no_normals(&ring) {
                out.push(fid);
            }
        }
        if out.is_empty() { return out; }
        self.recompute_normals();
        out
    }

    /// Bridge ONE ordered pair of closed rings — the single place that decides between the
    /// equal-length quad strip ([`bridge_loops`]) and the unequal-count fan
    /// ([`bridge_unequal_loops`]). Every bridge caller routes through here so the equal/unequal
    /// choice is never duplicated (CLAUDE.md: never two code paths for one operation).
    pub fn bridge_ring_pair(&mut self, a: &[VertexId], b: &[VertexId]) -> Vec<FaceId> {
        if a.len() == b.len() {
            self.bridge_loops(a, b)
        } else {
            self.bridge_unequal_loops(a, b)
        }
    }

    /// Centroid of a ring's vertex positions (used to order loops for chaining).
    fn ring_centroid(&self, ring: &[VertexId]) -> Vec3 {
        if ring.is_empty() { return Vec3::ZERO; }
        let mut c = Vec3::ZERO;
        for &v in ring { c += self.vert(v).position; }
        c / ring.len() as f32
    }

    /// Order N rings into a single CHAIN by proximity — Blender's `BM_mesh_edgeloops_calc_order`.
    ///
    /// Greedy nearest-neighbour that may extend at EITHER end of the growing chain, seeded from the
    /// ring holding the lowest vertex id (determinism — `edge_loop_rings` already starts each walk
    /// at its lowest-id vertex). Island membership deliberately does NOT constrain pairing: the
    /// Blender oracle (`docs/planning/poly/bridge_multi_ref.py`, `two_islands_wire`) produces byte
    /// -identical census to the single-island four-ring case, i.e. it chains straight across islands.
    ///
    /// Returns indices into `rings`, length == `rings.len()`.
    pub fn order_loop_chain(&self, rings: &[Vec<VertexId>]) -> Vec<usize> {
        let n = rings.len();
        if n <= 2 { return (0..n).collect(); }
        let cents: Vec<Vec3> = rings.iter().map(|r| self.ring_centroid(r)).collect();
        let key = |i: usize| rings[i].iter().map(|v| v.0).min().unwrap_or(u32::MAX);
        let seed = (0..n).min_by_key(|&i| key(i)).unwrap_or(0);

        let mut used = vec![false; n];
        used[seed] = true;
        let mut chain = std::collections::VecDeque::new();
        chain.push_back(seed);
        for _ in 1..n {
            let (mut best, mut best_d, mut at_front) = (usize::MAX, f32::MAX, false);
            let (front, back) = (*chain.front().unwrap(), *chain.back().unwrap());
            for i in 0..n {
                if used[i] { continue; }
                let df = cents[i].distance_squared(cents[front]);
                let db = cents[i].distance_squared(cents[back]);
                // Ties resolve on the lowest vertex id so the result never depends on iteration luck.
                for (d, f) in [(db, false), (df, true)] {
                    if d < best_d - 1e-9 || ((d - best_d).abs() <= 1e-9 && best != usize::MAX && key(i) < key(best)) {
                        best_d = d; best = i; at_front = f;
                    }
                }
            }
            if best == usize::MAX { break; }
            used[best] = true;
            if at_front { chain.push_front(best); } else { chain.push_back(best); }
        }
        chain.into_iter().collect()
    }

    /// Bridge **N ≥ 2** closed rings in ONE call — Blender `MESH_OT_bridge_edge_loops` on a
    /// multi-loop selection.
    ///
    /// The rings are ordered into a chain by [`order_loop_chain`] and consecutive pairs are bridged
    /// via [`bridge_ring_pair`], yielding exactly **N − 1** bridges. Oracle-confirmed against
    /// Blender 4.5 (`bridge_multi_ref.py`): 3 wire rings → 16 quads, 4 → 24 quads, no new
    /// vertices, and unequal counts fan rather than resample.
    ///
    /// Returns every new face id (empty if fewer than 2 rings, or nothing could be built).
    pub fn bridge_ring_chain(&mut self, rings: &[Vec<VertexId>]) -> Vec<FaceId> {
        if rings.len() < 2 { return Vec::new(); }
        let order = self.order_loop_chain(rings);
        let mut out = Vec::new();
        for w in order.windows(2) {
            let (a, b) = (rings[w[0]].clone(), rings[w[1]].clone());
            out.extend(self.bridge_ring_pair(&a, &b));
        }
        out
    }

    /// Bridge a selection of edges forming **two or more** closed loops
    /// ([`edge_loop_rings`](Self::edge_loop_rings)), chained by [`bridge_ring_chain`].
    ///
    /// Empty when the selection is not at least two simple rings. Unequal vertex counts are fanned
    /// by [`bridge_unequal_loops`]; N > 2 loops produce N − 1 bridges in ONE invocation.
    pub fn bridge_edge_selection(&mut self, edges: &std::collections::HashSet<HalfEdgeId>) -> Vec<FaceId> {
        let rings = self.edge_loop_rings(edges);
        self.bridge_ring_chain(&rings)
    }

    /// Bridge two faces into a tunnel (Blender **Bridge Edge Loops** on a two-face selection).
    ///
    /// Captures each face's ordered vertex ring, deletes both faces (opening boundary holes), then
    /// strips quads/tri-fans between the rings via [`bridge_loops`] / [`bridge_unequal_loops`].
    ///
    /// **Already-connected case (closed box opposite faces):** if the strip would double existing
    /// side walls (non-manifold), the op still **deletes both faces** and returns `Some(empty)` —
    /// that alone opens a clean through-tunnel using the surviving walls.
    ///
    /// Returns `None` (mesh untouched) when either face is dead, a ring is too short, the faces
    /// share a vertex, or neither a manifold strip nor a clean delete-only open is possible.
    pub fn bridge_faces(&mut self, face_a: FaceId, face_b: FaceId) -> Option<Vec<FaceId>> {
        self.bridge_faces_multi(&[face_a, face_b])
    }

    /// Bridge **N ≥ 2** selected faces into a tunnel — the multi-selection generalisation of
    /// [`bridge_faces`], which is now a two-face wrapper over this (ONE implementation).
    ///
    /// Blender's Bridge Edge Loops on a face selection removes the selected faces and bridges the
    /// resulting boundary rings; with N faces that is a chain of N − 1 bridges
    /// ([`bridge_ring_chain`]). Same honesty contract as before: the mesh is only mutated when the
    /// result validates AND is manifold, and the delete-only fallback (opening a through-tunnel via
    /// surviving side walls) still applies when the strip itself would be non-manifold.
    ///
    /// Returns `None` (mesh untouched) if fewer than 2 live faces, any ring is shorter than 3, two
    /// selected faces share a vertex, or neither a manifold chain nor a clean delete-only is possible.
    pub fn bridge_faces_multi(&mut self, faces: &[FaceId]) -> Option<Vec<FaceId>> {
        use std::collections::HashSet;
        let mut ids: Vec<FaceId> = faces.to_vec();
        ids.sort_unstable_by_key(|f| f.0);
        ids.dedup();
        if ids.len() < 2 { return None; }
        if ids.iter().any(|&f| !self.is_face_live(f)) { return None; }

        let rings: Vec<Vec<VertexId>> = ids.iter().map(|&f| self.face_vertices(f)).collect();
        if rings.iter().any(|r| r.len() < 3) { return None; }
        // Any shared vertex between two selected faces would make degenerate zero-area bridges.
        let mut seen: HashSet<u32> = HashSet::new();
        for r in &rings {
            for v in r {
                if !seen.insert(v.0) { return None; }
            }
        }

        // Prefer the full chain when it stays manifold (open caps with empty space between).
        {
            let mut scratch = self.clone();
            for &f in &ids { scratch.delete_face(f); }
            let strip = scratch.bridge_ring_chain(&rings);
            if !strip.is_empty() && scratch.validate().is_ok() && scratch.is_manifold().is_ok() {
                for &f in &ids { self.delete_face(f); }
                return Some(self.bridge_ring_chain(&rings));
            }
        }

        // Chain failed or non-manifold (typical: opposite faces of a closed solid — side walls
        // already connect the rings). Fall back to delete-only if that alone is clean.
        {
            let mut open_only = self.clone();
            for &f in &ids { open_only.delete_face(f); }
            if open_only.validate().is_ok() && open_only.is_manifold().is_ok() {
                for &f in &ids { self.delete_face(f); }
                self.recompute_normals();
                return Some(Vec::new()); // tunnel opened via existing walls; no new faces
            }
        }
        None
    }

    /// Bridge two closed loops of **different** vertex counts (Blender parity).
    ///
    /// Owner 2026-07-30: "bridge did not connect vertex properly." The kernel refused outright when
    /// `loop_a.len() != loop_b.len()` (and the app toasted "need exactly two closed equal-length
    /// loops"), so bridging a 4-vert loop to an 8-vert loop — an ordinary box-modelling move that
    /// Blender handles — did nothing.
    ///
    /// Strategy: **resample, do not reimplement.** The shorter ring is subdivided by splitting its
    /// longest edges until both rings have the same count, then the existing [`bridge_loops`] runs
    /// unchanged. That keeps ONE bridge implementation — the rotation/reverse alignment, the
    /// boundary-aware winding rule, and the Blender-oracle test all still cover the real work
    /// (CLAUDE.md: never two code paths for one operation).
    ///
    /// Splitting the LONGEST edge each round distributes the new vertices evenly in arc length
    /// rather than bunching them at the start of the ring, which is what keeps the resulting quads
    /// from degenerating into slivers.
    ///
    /// Returns the new faces, or empty if either ring is too small or a split fails.
    pub fn bridge_unequal_loops(&mut self, loop_a: &[VertexId], loop_b: &[VertexId]) -> Vec<FaceId> {
        let (na, nb) = (loop_a.len(), loop_b.len());
        if na < 3 || nb < 3 { return Vec::new(); }
        for &v in loop_a.iter().chain(loop_b.iter()) {
            if !self.is_vert_live(v) {
                return Vec::new();
            }
        }
        {
            use std::collections::HashSet;
            let sa: HashSet<u32> = loop_a.iter().map(|v| v.0).collect();
            if loop_b.iter().any(|v| sa.contains(&v.0)) { return Vec::new(); }
        }

        // Orient the LONG ring against the short one (same rotation+reverse search bridge_loops
        // uses), so spans pair up by proximity instead of by arbitrary index.
        let (short, long_raw) = if na <= nb { (loop_a, loop_b) } else { (loop_b, loop_a) };
        let (ns, nl) = (short.len(), long_raw.len());

        // Which way must the SHORT ring's edges be walked? If s0->s1 already exists as a boundary
        // half-edge, the new faces must walk s1->s0 so they TWIN it. That single bit orients the
        // whole fan (short backward implies long forward — the (true,true) row of bridge_loops).
        let flip = (0..ns).any(|i| {
            let (s0, s1) = (short[i], short[(i + 1) % ns]);
            self.find_half_edge(s0, s1).map(|h| self.he(h).twin == NO_TWIN).unwrap_or(false)
        });
        // With `flip` fixed, the LONG ring is no longer free to be reversed at will: a chained
        // bridge (`bridge_ring_chain`) hands us a long ring whose edges were ALREADY created as
        // boundary half-edges by the previous bridge in the chain. Reversing the ring is exactly
        // what flips those edge directions, so the reverse choice must satisfy the twin constraint
        // FIRST and minimise distance only among the orientations that survive. Without this the
        // second bridge of a 6/8/4 chain reused directed edges and left the mesh non-manifold.
        let long_ok = |me: &Self, l: &[VertexId]| -> bool {
            (0..nl).all(|k| {
                // Face walks l[k+1]->l[k] by default, l[k]->l[k+1] when flipped.
                let (p, q) = (l[k], l[(k + 1) % nl]);
                let used = if flip { (p, q) } else { (q, p) };
                me.find_half_edge(used.0, used.1).is_none()
            })
        };
        let fwd: Vec<VertexId> = long_raw.to_vec();
        let rev_v: Vec<VertexId> = long_raw.iter().copied().rev().collect();
        let allowed: Vec<bool> = vec![long_ok(self, &fwd), long_ok(self, &rev_v)];
        // If neither orientation is clean (free-standing rings satisfy both; a pathological mesh
        // may satisfy none), fall back to searching both rather than refusing outright.
        let any_allowed = allowed[0] || allowed[1];

        let mut best_cost = f32::MAX;
        let mut best_rot = 0usize;
        let mut best_rev = !allowed[0] && allowed[1];
        for rev in [false, true] {
            if any_allowed && !allowed[rev as usize] { continue; }
            let l: &Vec<VertexId> = if rev { &rev_v } else { &fwd };
            for rot in 0..nl {
                // Compare each SHORT vert against the long vert it would map to.
                let mut cost = 0.0f32;
                for i in 0..ns {
                    let li = (i * nl) / ns;
                    let lv = l[(li + rot) % nl];
                    cost += (self.vert(short[i]).position - self.vert(lv).position).length_squared();
                }
                if cost < best_cost { best_cost = cost; best_rot = rot; best_rev = rev; }
            }
        }
        let mut long: Vec<VertexId> = if best_rev {
            long_raw.iter().copied().rev().collect()
        } else {
            long_raw.to_vec()
        };
        long.rotate_left(best_rot);

        // FAN, don't split. Walk the long ring once; each short vertex owns a contiguous ARC of long
        // vertices. Every long edge in that arc becomes a triangle apexed on the short vertex, and
        // one more triangle closes the arc onto the NEXT short vertex — exactly how Blender's Bridge
        // Edge Loops absorbs a vertex-count mismatch. Total faces = nl + ns, matching the Blender
        // oracle (`bridge_multi_ref.py` `three_wire_unequal`: 6→8 gives 14, 8→4 gives 12, sum 26).
        //
        // Fixed 2026-08-07: the previous version emitted a quad SPANNING the whole arc
        // (`[s0,s1,l_to,l_from]`, skipping the interior long verts) AND then fanned triangles over
        // that same arc, so multi-vertex spans produced OVERLAPPING faces. The mesh validated but
        // was wildly non-manifold — `bridge_ring_chain_unequal_counts_three_loops` measured 40
        // boundary half-edges where Blender has 10. A single tiling has no spanning quad at all.
        //
        // Winding rule from bridge_loops: if the short ring's edges already exist as boundary
        // half-edges, the new faces must walk them the other way so they TWIN instead of reusing the
        // same directed edge (the is_manifold "same winding" failure). For a fan that means
        // reversing the WHOLE span — short backward implies long forward, which is precisely the
        // (true,true) row of bridge_loops' quad table.
        let emit = |me: &mut Self, tri: [VertexId; 3]| -> Option<FaceId> {
            let t = if flip { [tri[2], tri[1], tri[0]] } else { tri };
            // No per-face normal pass — one trailing `recompute_normals()` below (see `bridge_loops`).
            me.create_face_no_normals(&t)
        };

        let mut out = Vec::new();
        // start[i] = index into `long` where short[i]'s arc begins.
        let start: Vec<usize> = (0..ns).map(|i| (i * nl) / ns).collect();
        for i in 0..ns {
            let s0 = short[i];
            let s1 = short[(i + 1) % ns];
            let from = start[i];
            let to = start[(i + 1) % ns] + if i + 1 == ns { nl } else { 0 };
            // Fan every long edge in this arc back to s0.
            for k in from..to {
                let p = long[k % nl];
                let q = long[(k + 1) % nl];
                if p == q { continue; }
                if let Some(f) = emit(self, [s0, q, p]) { out.push(f); }
            }
            // Close the arc onto the next short vertex.
            let l_to = long[to % nl];
            if s0 != s1 && s1 != l_to && s0 != l_to {
                if let Some(f) = emit(self, [s0, s1, l_to]) { out.push(f); }
            }
        }
        if out.is_empty() { return out; }
        self.recompute_normals();
        out
    }

    /// Blender **Grid Fill** — fill a single closed edge loop with a regular grid of quads.
    ///
    /// `span` = number of columns (edges along one pair of opposite sides). When `None`, picks the
    /// factor of `L/2` closest to square by side arc-length. `offset` rotates which loop vertex is
    /// the bottom-left corner (mod L).
    ///
    /// Requires: exactly **one** simple closed ring from the edge set, even length `L ≥ 4`.
    /// Interior verts are bilinearly lerped (Blender simple interp). Returns new face ids, or
    /// empty on refuse (mesh untouched).
    pub fn grid_fill(
        &mut self,
        edges: &std::collections::HashSet<HalfEdgeId>,
        span: Option<usize>,
        offset: usize,
    ) -> Vec<FaceId> {
        let rings = self.edge_loop_rings(edges);
        if rings.len() != 1 {
            return Vec::new();
        }
        self.grid_fill_loop(&rings[0], span, offset)
    }

    /// Grid-fill an ordered closed vertex loop (last connects to first). See [`grid_fill`].
    pub fn grid_fill_loop(
        &mut self,
        loop_v: &[VertexId],
        span: Option<usize>,
        offset: usize,
    ) -> Vec<FaceId> {
        let l = loop_v.len();
        if l < 4 || l % 2 != 0 {
            return Vec::new();
        }
        for &v in loop_v {
            if !self.is_vert_live(v) {
                return Vec::new();
            }
        }
        // Half-perimeter must split into two positive side lengths.
        let half = l / 2;
        let cols = match span {
            Some(s) if s >= 1 && s < half => s,
            Some(_) => return Vec::new(),
            None => self.grid_fill_auto_span(loop_v),
        };
        let rows = half - cols;
        if rows < 1 {
            return Vec::new();
        }

        let off = offset % l;
        // Rotated ring so index 0 is bottom-left corner.
        let ring: Vec<VertexId> = (0..l).map(|i| loop_v[(i + off) % l]).collect();

        // Grid of (rows+1) × (cols+1) vertex ids. Boundary from the loop; interior allocated.
        // Layout (CCW from bottom-left):
        //   bottom  i=0,     j=0..cols     → ring[0 ..= cols]
        //   right   j=cols,  i=0..rows     → ring[cols ..= cols+rows]
        //   top     i=rows,  j=cols..0     → ring[cols+rows ..= cols+rows+cols] (j decreasing)
        //   left    j=0,     i=rows..0     → ring[cols+rows+cols ..= L] then ring[0]
        let mut grid: Vec<Vec<VertexId>> = vec![vec![VertexId(0); cols + 1]; rows + 1];

        for j in 0..=cols {
            grid[0][j] = ring[j];
        }
        for i in 0..=rows {
            grid[i][cols] = ring[cols + i];
        }
        for j in 0..=cols {
            // top walks opposite to bottom: j=cols → ring[cols+rows], j=0 → ring[cols+rows+cols]
            grid[rows][j] = ring[cols + rows + (cols - j)];
        }
        for i in 0..=rows {
            // left: i=rows → top-left, i=0 → bottom-left (= ring[0])
            let k = rows - i;
            let idx = cols + rows + cols + k; // k=rows → L → ring[0]
            grid[i][0] = ring[idx % l];
        }

        // Interior: Coons patch from boundary rails (Blender simple interp spirit).
        let c00 = self.vert(grid[0][0]).position;
        let c10 = self.vert(grid[0][cols]).position;
        let c01 = self.vert(grid[rows][0]).position;
        let c11 = self.vert(grid[rows][cols]).position;
        for i in 1..rows {
            let v = i as f32 / rows as f32;
            for j in 1..cols {
                let u = j as f32 / cols as f32;
                let p_corners = c00 * ((1.0 - u) * (1.0 - v))
                    + c10 * (u * (1.0 - v))
                    + c01 * ((1.0 - u) * v)
                    + c11 * (u * v);
                let bot = self.vert(grid[0][j]).position;
                let top = self.vert(grid[rows][j]).position;
                let left = self.vert(grid[i][0]).position;
                let right = self.vert(grid[i][cols]).position;
                let p = bot.lerp(top, v) + left.lerp(right, u) - p_corners;
                grid[i][j] = self.alloc_vertex(HVertex {
                    position: p,
                    normal: Vec3::Y,
                    half_edge: NO_EDGE,
                });
            }
        }

        // If a boundary HE already walks a→b on the bottom edge, create_face must use reverse
        // winding so it twins instead of double-using the directed edge.
        let flip = self
            .find_half_edge(grid[0][0], grid[0][1.min(cols)])
            .map(|h| self.he(h).twin == NO_TWIN)
            .unwrap_or(false);

        let mut out = Vec::with_capacity(rows * cols);
        for i in 0..rows {
            for j in 0..cols {
                let a = grid[i][j];
                let b = grid[i][j + 1];
                let c = grid[i + 1][j + 1];
                let d = grid[i + 1][j];
                let ring4 = if flip {
                    [b, a, d, c]
                } else {
                    [a, b, c, d]
                };
                // No per-face normal pass — one trailing `recompute_normals()` (see `bridge_loops`).
                if let Some(fid) = self.create_face_no_normals(&ring4) {
                    out.push(fid);
                }
            }
        }
        if out.is_empty() {
            return out;
        }
        self.recompute_normals();
        out
    }

    /// LoopTools **Relax** — Jacobi Laplacian smooth on selected verts.
    ///
    /// Each iteration moves every selected vertex toward the average of its 1-ring neighbours
    /// (full mesh fan — unselected neighbours act as fixed anchors). `factor` ∈ [0, 1] blends
    /// `old → target` (1 = full Laplacian step). Boundary verts of the **mesh** (any incident
    /// half-edge with `NO_TWIN`) are pinned when `lock_boundary` is true.
    ///
    /// Pure reshape: topology unchanged. Returns how many verts were written (0 = refuse /
    /// nothing movable). On 0 the mesh is untouched.
    pub fn loop_relax(
        &mut self,
        verts: &std::collections::HashSet<VertexId>,
        iterations: u32,
        factor: f32,
        lock_boundary: bool,
    ) -> u32 {
        use std::collections::HashSet;
        if verts.is_empty() || iterations == 0 {
            return 0;
        }
        let factor = factor.clamp(0.0, 1.0);
        if factor < 1e-8 {
            return 0;
        }

        // Movable set: selected, live, and (optionally) not on mesh boundary.
        //
        // `vertex_is_boundary` is O(half_edges) per call (its exhaustive scan cannot be skipped
        // without changing the answer on a bowtie vertex), so asking it per vertex made this loop
        // O(V x HE): 319 ms vs 1.76 ms on a 9216-face mesh, i.e. 317 ms of the 319 was this one
        // predicate. `boundary_vertex_mask` computes the identical flag for every vertex in ONE
        // pass — see its doc comment for why the two agree exactly.
        let boundary = if lock_boundary {
            Some(self.boundary_vertex_mask())
        } else {
            None
        };
        let mut movable: Vec<VertexId> = Vec::new();
        for &v in verts {
            if !self.is_vert_live(v) {
                continue;
            }
            if self.vert(v).half_edge == NO_EDGE {
                continue;
            }
            if let Some(mask) = &boundary {
                if mask.get(v.0 as usize).copied().unwrap_or(false) {
                    continue;
                }
            }
            movable.push(v);
        }
        if movable.is_empty() {
            return 0;
        }
        movable.sort_by_key(|v| v.0);

        // Precompute 1-ring neighbour lists (destinations of outgoing half-edges).
        let mut nbrs: Vec<Vec<VertexId>> = Vec::with_capacity(movable.len());
        for &v in &movable {
            let fan = self.vertex_fan(v);
            let mut ns = Vec::with_capacity(fan.len());
            for h in fan {
                let d = self.he(h).vertex;
                if d != v {
                    ns.push(d);
                }
            }
            ns.sort_by_key(|x| x.0);
            ns.dedup();
            nbrs.push(ns);
        }

        for _ in 0..iterations {
            // Jacobi: all targets from previous positions.
            let next: Vec<(VertexId, Vec3)> = movable
                .iter()
                .zip(nbrs.iter())
                .filter_map(|(&v, ns)| {
                    if ns.is_empty() {
                        return None;
                    }
                    let mut sum = Vec3::ZERO;
                    for &nb in ns {
                        sum += self.vert(nb).position;
                    }
                    let target = sum / ns.len() as f32;
                    let old = self.vert(v).position;
                    Some((v, old.lerp(target, factor)))
                })
                .collect();
            for (v, p) in next {
                self.vert_mut(v).position = p;
            }
        }
        self.recompute_normals();
        movable.len() as u32
    }


    /// LoopTools **Flatten** — project selected verts onto the best-fit plane (Newell).
    /// Returns how many verts moved (0 = refuse: <3 verts or degenerate plane).
    pub fn loop_flatten(&mut self, verts: &std::collections::HashSet<VertexId>) -> u32 {
        if verts.len() < 3 {
            return 0;
        }
        let mut pts = Vec::with_capacity(verts.len());
        let mut ids = Vec::with_capacity(verts.len());
        for &v in verts {
            if !self.is_vert_live(v) {
                continue;
            }
            if self.vert(v).half_edge == NO_EDGE {
                continue;
            }
            ids.push(v);
            pts.push(self.vert(v).position);
        }
        if pts.len() < 3 {
            return 0;
        }
        let n = newell_normal(&pts).normalize_or_zero();
        if n.length_squared() < 0.25 {
            return 0;
        }
        let com = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
        let mut moved = 0u32;
        for &v in &ids {
            let p = self.vert(v).position;
            let d = (p - com).dot(n);
            if d.abs() > 1e-8 {
                self.vert_mut(v).position = p - n * d;
                moved += 1;
            }
        }
        if moved > 0 {
            self.recompute_normals();
        }
        // Report count of verts considered when any projection ran; if all already planar, still
        // success for dry-run (ids.len() > 0).
        if moved == 0 && !ids.is_empty() {
            // Already flat — treat as success so app can toast "already planar" vs refuse.
            return ids.len() as u32;
        }
        moved
    }

    /// LoopTools **Space** — equalize arc spacing along edge loops / chains in `edges`.
    ///
    /// Simple path or cycle components only (vertex degree ≤ 2 in the selection graph).
    /// Open chains: endpoints fixed, interiors re-spaced. Closed rings: phase locked at the
    /// lowest-id vertex, others re-spaced. Returns verts moved (0 = refuse).
    pub fn loop_space(&mut self, edges: &std::collections::HashSet<HalfEdgeId>) -> u32 {
        use std::collections::{HashMap, HashSet};

        let mut geom: HashMap<(u32, u32), (VertexId, VertexId)> = HashMap::new();
        for &he in edges {
            if !self.is_edge_live(he) {
                continue;
            }
            let a = self.he_source(he);
            let b = self.he(he).vertex;
            if a == b {
                continue;
            }
            let key = (a.0.min(b.0), a.0.max(b.0));
            geom.entry(key).or_insert((a, b));
        }
        if geom.is_empty() {
            return 0;
        }
        let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(a, b) in geom.values() {
            adj.entry(a.0).or_default().push(b.0);
            adj.entry(b.0).or_default().push(a.0);
        }
        for ns in adj.values_mut() {
            ns.sort();
            ns.dedup();
            if ns.len() > 2 {
                return 0; // branch — refuse whole selection
            }
        }

        // Prefer open-chain starts (degree 1); else closed-ring seeds (all verts).
        let mut seeds: Vec<u32> = adj
            .iter()
            .filter(|(_, ns)| ns.len() == 1)
            .map(|(&k, _)| k)
            .collect();
        if seeds.is_empty() {
            seeds = adj.keys().copied().collect();
        }
        seeds.sort();

        let mut visited: HashSet<u32> = HashSet::new();
        let mut moved = 0u32;

        for &s in &seeds {
            if visited.contains(&s) {
                continue;
            }
            let mut path: Vec<u32> = Vec::new();
            let mut cur = s;
            let mut prev: Option<u32> = None;
            loop {
                if !visited.insert(cur) {
                    break;
                }
                path.push(cur);
                let ns = match adj.get(&cur) {
                    Some(n) => n.as_slice(),
                    None => break,
                };
                let nxt = match ns.len() {
                    1 => {
                        if Some(ns[0]) == prev {
                            break;
                        }
                        ns[0]
                    }
                    2 => {
                        if Some(ns[0]) == prev {
                            ns[1]
                        } else if Some(ns[1]) == prev {
                            ns[0]
                        } else {
                            ns[0] // cycle start: deterministic lower neighbour (sorted)
                        }
                    }
                    _ => break,
                };
                if nxt == s {
                    break; // closed
                }
                if visited.contains(&nxt) {
                    break;
                }
                prev = Some(cur);
                cur = nxt;
            }

            let n = path.len();
            if n < 3 {
                continue;
            }
            let closed = adj.get(&path[0]).map(|ns| ns.len() == 2).unwrap_or(false)
                && {
                    let a = path[0].min(path[n - 1]);
                    let b = path[0].max(path[n - 1]);
                    geom.contains_key(&(a, b))
                };

            // Polyline samples (closed: append start).
            let mut pts: Vec<Vec3> = path
                .iter()
                .map(|&id| self.vert(VertexId(id)).position)
                .collect();
            if closed {
                pts.push(pts[0]);
            }
            let m = pts.len();
            let mut cum = vec![0.0f32; m];
            for i in 1..m {
                cum[i] = cum[i - 1] + (pts[i] - pts[i - 1]).length();
            }
            let total = cum[m - 1];
            if total < 1e-8 {
                continue;
            }

            let point_at = |dist: f32| -> Vec3 {
                let d = dist.clamp(0.0, total);
                for i in 0..m - 1 {
                    if d <= cum[i + 1] + 1e-6 {
                        let len = cum[i + 1] - cum[i];
                        let t = if len < 1e-12 {
                            0.0
                        } else {
                            ((d - cum[i]) / len).clamp(0.0, 1.0)
                        };
                        return pts[i].lerp(pts[i + 1], t);
                    }
                }
                pts[m - 1]
            };

            if closed {
                // Equal spacing around the ring; keep path[0] as phase origin.
                let seg = total / n as f32;
                for i in 1..n {
                    let target = point_at(seg * i as f32);
                    let v = VertexId(path[i]);
                    if (self.vert(v).position - target).length_squared() > 1e-12 {
                        self.vert_mut(v).position = target;
                        moved += 1;
                    }
                }
            } else {
                let seg = total / (n - 1) as f32;
                for i in 1..n - 1 {
                    let target = point_at(seg * i as f32);
                    let v = VertexId(path[i]);
                    if (self.vert(v).position - target).length_squared() > 1e-12 {
                        self.vert_mut(v).position = target;
                        moved += 1;
                    }
                }
            }
        }

        if moved > 0 {
            self.recompute_normals();
        }
        moved
    }

    /// Blender **Spin** — rotate selected verts around `axis` through `center` in `steps`
    /// segments totaling `angle_rad`, connecting selected edges with side quads each step.
    ///
    /// - `verts`: profile vertices (must include both ends of every edge to spin).  
    /// - `merge`: when true and `|angle| ≈ 2π`, the last ring welds to the first (closed lathe).  
    /// - Positions are mesh-local (caller converts world pivot/axis).  
    ///
    /// Returns how many side quads were created (0 = refuse, mesh untouched on empty verts /
    /// zero steps / degenerate axis). New interior verts are left in place; original verts stay
    /// as ring 0 (extrude-style, not duplicate-and-leave-gap).
    pub fn spin(
        &mut self,
        verts: &std::collections::HashSet<VertexId>,
        center: Vec3,
        axis: Vec3,
        angle_rad: f32,
        steps: u32,
        merge: bool,
    ) -> u32 {
        use glam::Quat;
        use std::collections::{HashMap, HashSet};

        if verts.is_empty() || steps == 0 {
            return 0;
        }
        let axis = axis.normalize_or_zero();
        if axis.length_squared() < 0.25 {
            return 0;
        }
        for &v in verts {
            if !self.is_vert_live(v) {
                return 0;
            }
        }

        // Undirected edges whose both endpoints are in the selection.
        let mut edge_pairs: Vec<(u32, u32)> = Vec::new();
        let mut seen_e: HashSet<(u32, u32)> = HashSet::new();
        for i in 0..self.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !self.is_edge_live(he) {
                continue;
            }
            let tw = self.he(he).twin;
            if tw != NO_TWIN && i > tw.0 {
                continue;
            }
            let a = self.he_source(he).0;
            let b = self.he(he).vertex.0;
            if a == b {
                continue;
            }
            if !verts.contains(&VertexId(a)) || !verts.contains(&VertexId(b)) {
                continue;
            }
            let key = (a.min(b), a.max(b));
            if seen_e.insert(key) {
                edge_pairs.push((a, b));
            }
        }
        if edge_pairs.is_empty() {
            return 0; // nothing to sweep
        }

        let full_circle = merge
            && (angle_rad.abs() - std::f32::consts::TAU).abs() < 1e-2;
        if full_circle && steps < 3 {
            return 0; // need ≥3 segments to close
        }

        let dtheta = angle_rad / steps as f32;

        // rings[s]: original_vid → VertexId at step s
        let mut rings: Vec<HashMap<u32, VertexId>> = Vec::with_capacity(steps as usize + 1);
        let mut r0 = HashMap::with_capacity(verts.len());
        for &v in verts {
            r0.insert(v.0, v);
        }
        rings.push(r0);

        let n_extra = if full_circle { steps - 1 } else { steps };
        for s in 1..=n_extra {
            let ang = dtheta * s as f32;
            let q = Quat::from_axis_angle(axis, ang);
            let mut rm = HashMap::with_capacity(verts.len());
            for &v in verts {
                let p = self.vert(v).position;
                let np = center + q * (p - center);
                let nrm = q * self.vert(v).normal;
                let nv = self.alloc_vertex(HVertex {
                    position: np,
                    normal: nrm,
                    half_edge: NO_EDGE,
                });
                rm.insert(v.0, nv);
            }
            rings.push(rm);
        }
        if full_circle {
            // Segment steps-1 → 0 uses a clone of ring 0 as the "next" map.
            rings.push(rings[0].clone());
        }

        let mut n_faces = 0u32;
        for s in 0..steps as usize {
            let ra = &rings[s];
            let rb = &rings[s + 1];
            for &(a, b) in &edge_pairs {
                let a0 = ra[&a];
                let b0 = ra[&b];
                let a1 = rb[&a];
                let b1 = rb[&b];
                // Prefer twinning existing boundary HE a0→b0 when present.
                let flip = self
                    .find_half_edge(a0, b0)
                    .map(|h| self.he(h).twin == NO_TWIN)
                    .unwrap_or(false);
                let ring4 = if flip {
                    [b0, a0, a1, b1]
                } else {
                    [a0, b0, b1, a1]
                };
                // No per-face normal pass — one trailing `recompute_normals()` (see `bridge_loops`).
                if self.create_face_no_normals(&ring4).is_some() {
                    n_faces += 1;
                }
            }
        }
        if n_faces > 0 {
            self.recompute_normals();
        }
        n_faces
    }

    /// Pick span ∈ [1, half-1] that makes the two side arc-lengths as equal as possible.
    fn grid_fill_auto_span(&self, loop_v: &[VertexId]) -> usize {
        let l = loop_v.len();
        let half = l / 2;
        let mut best_s = 1usize;
        let mut best_score = f32::MAX;
        for s in 1..half {
            let r = half - s;
            // Arc length of first side (s edges) vs second (r edges), starting at 0.
            let mut len_s = 0.0f32;
            for i in 0..s {
                let a = self.vert(loop_v[i]).position;
                let b = self.vert(loop_v[i + 1]).position;
                len_s += (a - b).length();
            }
            let mut len_r = 0.0f32;
            for i in 0..r {
                let a = self.vert(loop_v[s + i]).position;
                let b = self.vert(loop_v[s + i + 1]).position;
                len_r += (a - b).length();
            }
            let score = (len_s - len_r).abs();
            if score < best_score {
                best_score = score;
                best_s = s;
            }
        }
        best_s
    }

    /// Local-space V/E/F snap targets for edit-mode geometry snap (Blender-style).
    ///
    /// - **V** (`kind=0`): live vertex positions  
    /// - **E** (`kind=1`): undirected edge midpoints  
    /// - **F** (`kind=2`): face centroids (average of ring verts)  
    ///
    /// `exclude_verts` drops those vertices and any edge/face that touches them (so a poly G/R/S
    /// selection does not snap to itself mid-drag). Caller applies the body world transform.
    pub fn geometry_snap_points_local(
        &self,
        exclude_verts: &std::collections::HashSet<u32>,
    ) -> Vec<(Vec3, u8)> {
        let mut out = Vec::new();
        // Vertices
        for i in 0..self.vertices.len() as u32 {
            if exclude_verts.contains(&i) {
                continue;
            }
            if self.vert(VertexId(i)).half_edge == NO_EDGE {
                continue;
            }
            out.push((self.vert(VertexId(i)).position, 0));
        }
        // Undirected edge midpoints
        for i in 0..self.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !self.is_edge_live(he) {
                continue;
            }
            let tw = self.he(he).twin;
            // Dedup undirected: keep lower id, or boundary (NO_TWIN).
            if tw != NO_TWIN && i > tw.0 {
                continue;
            }
            let a = self.he_source(he).0;
            let b = self.he(he).vertex.0;
            if exclude_verts.contains(&a) || exclude_verts.contains(&b) {
                continue;
            }
            let pa = self.vert(VertexId(a)).position;
            let pb = self.vert(VertexId(b)).position;
            out.push(((pa + pb) * 0.5, 1));
        }
        // Face centroids
        for i in 0..self.faces.len() as u32 {
            if self.face(FaceId(i)).half_edge == NO_EDGE {
                continue;
            }
            let vs = self.face_vertices(FaceId(i));
            if vs.len() < 3 {
                continue;
            }
            if vs.iter().any(|v| exclude_verts.contains(&v.0)) {
                continue;
            }
            let mut c = Vec3::ZERO;
            for &v in &vs {
                c += self.vert(v).position;
            }
            out.push((c / vs.len() as f32, 2));
        }
        out
    }

    /// Blender **Rip** along one interior edge: duplicate the edge's endpoints for the **twin** face
    /// and clear the twin link so the edge becomes a boundary slit (can pull apart).
    ///
    /// Returns `Some((a2, b2))` — duplicates of `(source, dest)` remounted on the twin face —
    /// or `None` (mesh untouched) if the edge is already a boundary, dead, or a same-face slit.
    pub fn rip_edge(&mut self, he: HalfEdgeId) -> Option<(VertexId, VertexId)> {
        if !self.is_edge_live(he) {
            return None;
        }
        let tw = self.he(he).twin;
        if tw == NO_TWIN || !self.is_edge_live(tw) {
            return None; // already open
        }
        let fa = self.he(he).face;
        let fb = self.he(tw).face;
        if fa == NO_FACE || fb == NO_FACE || fa == fb {
            return None;
        }
        let a = self.he_source(he);
        let b = self.he(he).vertex;
        // Duplicate endpoints for the twin-side face only.
        let a2 = self.alloc_vertex(HVertex {
            position: self.vert(a).position,
            normal: self.vert(a).normal,
            half_edge: NO_EDGE,
        });
        let b2 = self.alloc_vertex(HVertex {
            position: self.vert(b).position,
            normal: self.vert(b).normal,
            half_edge: NO_EDGE,
        });
        // Remount destinations on the twin face: a→a2, b→b2.
        for h in self.face_half_edges(fb) {
            let v = self.he(h).vertex;
            if v == a {
                self.he_mut(h).vertex = a2;
            } else if v == b {
                self.he_mut(h).vertex = b2;
            }
        }
        // Open the edge.
        self.he_mut(he).twin = NO_TWIN;
        self.he_mut(tw).twin = NO_TWIN;
        // Outgoing half-edges for new verts (any HE that points to them as destination, walk fan via twin).
        // Set a crude outgoing: find HE on fb with source a2.
        for h in self.face_half_edges(fb) {
            if self.he_source(h) == a2 {
                self.vert_mut(a2).half_edge = h;
            }
            if self.he_source(h) == b2 {
                self.vert_mut(b2).half_edge = h;
            }
        }
        if self.vert(a2).half_edge == NO_EDGE {
            self.vert_mut(a2).half_edge = tw;
        }
        if self.vert(b2).half_edge == NO_EDGE {
            // next of tw on face should leave b2
            let nxt = self.he(tw).next;
            self.vert_mut(b2).half_edge = nxt;
        }
        self.recompute_normals();
        Some((a2, b2))
    }

    /// Rip every undirected interior edge in `edges` (Blender V with edge selection).
    /// Returns how many edges were ripped. 0 = nothing done (caller should refuse undo).
    pub fn rip_edges(&mut self, edges: &std::collections::HashSet<HalfEdgeId>) -> u32 {
        use std::collections::HashSet;
        // Collect unique undirected interior edges.
        let mut uniq: Vec<HalfEdgeId> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for &h in edges {
            if !self.is_edge_live(h) {
                continue;
            }
            let tw = self.he(h).twin;
            if tw == NO_TWIN {
                continue;
            }
            let key = h.0.min(tw.0);
            if seen.insert(key) {
                uniq.push(HalfEdgeId(key));
            }
        }
        let mut n = 0u32;
        for h in uniq {
            // Re-check live each time (prior rips may have affected topology only locally).
            if self.is_edge_live(h) && self.he(h).twin != NO_TWIN && self.rip_edge(h).is_some() {
                n += 1;
            }
        }
        n
    }

    /// Blender **Rip Fill** (Alt+V): rip one interior edge, then seal the slit with a zero-area
    /// quad so pulling the sides apart stretches the new face (no open hole).
    ///
    /// Fill winding `[a, a2, b2, b]` twins the existing boundary half-edges `a→b` and `b2→a2`.
    /// Returns the new face id, or `None` if rip refused / create_face failed.
    pub fn rip_fill_edge(&mut self, he: HalfEdgeId) -> Option<FaceId> {
        let a = self.he_source(he);
        let b = self.he(he).vertex;
        let (a2, b2) = self.rip_edge(he)?;
        self.create_face(&[a, a2, b2, b])
    }

    /// Rip-fill every undirected interior edge in `edges`. Returns how many fill faces were created.
    /// 0 = refuse (caller should skip undo).
    pub fn rip_fill_edges(&mut self, edges: &std::collections::HashSet<HalfEdgeId>) -> u32 {
        use std::collections::HashSet;
        let mut uniq: Vec<HalfEdgeId> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for &h in edges {
            if !self.is_edge_live(h) {
                continue;
            }
            let tw = self.he(h).twin;
            if tw == NO_TWIN {
                continue;
            }
            let key = h.0.min(tw.0);
            if seen.insert(key) {
                uniq.push(HalfEdgeId(key));
            }
        }
        let mut n = 0u32;
        for h in uniq {
            if self.is_edge_live(h) && self.he(h).twin != NO_TWIN && self.rip_fill_edge(h).is_some() {
                n += 1;
            }
        }
        n
    }


    /// Laplacian smooth (Blender Smooth modifier): move every vertex toward the average of its
    /// 1-ring neighbours, `iterations` times, blended by `factor` — `new = lerp(v, avg(neighbours), factor)`
    /// (`factor=0` no-op, `factor=1` full averaging). JACOBI update (every new position derived from the
    /// PREVIOUS pass, then written) so the result is order-independent — the SAME scheme as the Circle
    /// tool's interior relax (`circle_reshape_loops`), lifted here as a whole-mesh op with no pinning.
    /// Boundary/all vertices are smoothed uniformly (matches Blender's Smooth modifier, which — unlike
    /// its Laplacian-Smooth cousin — does not pin the boundary). Normals recomputed at the end.
    pub fn smooth_laplacian(&mut self, iterations: u32, factor: f32) {
        self.smooth_laplacian_masked(iterations, factor, None);
    }

    /// [`Self::smooth_laplacian`] with an optional per-vertex strength mask (`0` = pinned, `1` =
    /// full `factor`), indexed by `VertexId`. Build one from a
    /// [`crate::geometry_channel::GeometryChannel`] with `vertex_weights`.
    ///
    /// A mask shorter than the vertex array is IGNORED rather than partially applied: a mask that
    /// silently pinned the tail of the mesh because it was built against a stale topology would be
    /// a wrong-geometry bug with no error, which is worse than an unmasked smooth.
    ///
    /// `mask = None` is the exact pre-existing code path (see `smooth_jacobi_pass`), so an
    /// unmasked call stays bit-identical to what `poly_smooth_parity.rs` pins.
    pub fn smooth_laplacian_masked(
        &mut self,
        iterations: u32,
        factor: f32,
        mask: Option<&[f32]>,
    ) {
        if iterations == 0 || factor.abs() < 1e-9 { return; }
        let mask = match mask {
            Some(w) if w.len() == self.vertices.len() => Some(w),
            Some(w) => {
                debug_assert!(
                    false,
                    "smooth mask len {} != vertex count {} — mask ignored",
                    w.len(),
                    self.vertices.len()
                );
                None
            }
            None => None,
        };
        let nv = self.vertices.len();
        if nv == 0 { return; }

        // 1-ring adjacency over every LIVE face (undirected edges of each face ring), stored as a
        // flat CSR (one offset array + one neighbour array) instead of a `HashMap<u32, Vec<_>>`.
        //
        // BIT-IDENTICAL, not merely equivalent: neighbours end up sorted ascending and deduped —
        // the same set in the same order the HashMap version produced — so every float sum below
        // adds the same values in the same sequence. Only the table's BUILD cost changes, and that
        // build was 38% of a Smooth-modifier evaluation (40k quads: 9.5 ms of 25 ms; see
        // `tests/bench_poly_smooth.rs`), because a HashMap entry + a per-vertex `Vec` + a
        // per-face `face_vertices` allocation ran for every face corner.
        // MEASURED DEAD END (2026-08-09): this build was rewritten to walk each face ring ONCE
        // into a flat corner buffer (the counting and scatter passes reading that buffer instead
        // of re-walking) and to fan the per-vertex sort/dedup out over rayon. Both were REVERTED:
        // measured 2.47 -> 2.6-2.9 ms at 40k quads and 9.19 -> 9.8-11.0 ms at 160k, i.e. neutral
        // to slightly worse for materially more code. The double traversal is not what this costs;
        // it is bound by the allocation + memory traffic of the six arrays below, so the way to
        // win here is to stop REBUILDING the table on every frame of a drag (topology does not
        // change while a slider moves), not to make the build cleverer. Measure before retrying.
        let mut ring: Vec<VertexId> = Vec::new();
        let mut counts = vec![0u32; nv];
        for i in 0..self.faces.len() as u32 {
            let fid = FaceId(i);
            if self.face(fid).half_edge == NO_EDGE { continue; }
            self.face_vertices_into(fid, &mut ring);
            let n = ring.len();
            for k in 0..n {
                let (a, b) = (ring[k], ring[(k + 1) % n]);
                counts[a.0 as usize] += 1;
                counts[b.0 as usize] += 1;
            }
        }
        let mut offs = vec![0u32; nv + 1];
        let mut acc = 0u32;
        for v in 0..nv {
            offs[v] = acc;
            acc += counts[v];
        }
        offs[nv] = acc;
        let mut nbr = vec![0u32; acc as usize];
        let mut cursor = offs.clone();
        for i in 0..self.faces.len() as u32 {
            let fid = FaceId(i);
            if self.face(fid).half_edge == NO_EDGE { continue; }
            self.face_vertices_into(fid, &mut ring);
            let n = ring.len();
            for k in 0..n {
                let (a, b) = (ring[k], ring[(k + 1) % n]);
                nbr[cursor[a.0 as usize] as usize] = b.0;
                cursor[a.0 as usize] += 1;
                nbr[cursor[b.0 as usize] as usize] = a.0;
                cursor[b.0 as usize] += 1;
            }
        }
        // Sort + dedup each vertex's slice in place; `counts` becomes the DEDUPED degree, so the
        // live neighbours of `v` are `nbr[offs[v] .. offs[v] + counts[v]]`.
        for v in 0..nv {
            let (s, e) = (offs[v] as usize, offs[v + 1] as usize);
            if s == e { counts[v] = 0; continue; }
            let slice = &mut nbr[s..e];
            slice.sort_unstable();
            let mut w = 1usize;
            for r in 1..slice.len() {
                if slice[r] != slice[w - 1] {
                    slice[w] = slice[r];
                    w += 1;
                }
            }
            counts[v] = w as u32;
        }

        // JACOBI passes over flat position buffers. Vertices with no live face keep their
        // position (degree 0 => copied through untouched), exactly as the HashMap version left
        // any vertex absent from `adj` alone.
        let f = factor.clamp(0.0, 1.0);
        let mut cur: Vec<Vec3> = self.vertices.iter().map(|v| v.position).collect();
        let mut next = cur.clone();
        for _ in 0..iterations {
            smooth_jacobi_pass(&cur, &mut next, &offs, &counts, &nbr, f, mask);
            std::mem::swap(&mut cur, &mut next);
        }
        for (i, p) in cur.iter().enumerate() {
            self.vertices[i].position = *p;
        }
        self.recompute_normals();
    }

    /// BEND (Blender Simple Deform "Bend"): wrap the mesh around an arc of `angle_rad`, curling it
    /// out of the `along` axis toward `up`. Positions only — no topology change, no vertex/edge/face
    /// ID churn, so every authored sidecar layer (seam / sharp / crease / `op_created`) survives.
    ///
    /// The deform frame is `(along, up_perp, binormal)` about `pivot`, where `up_perp` is `up` made
    /// perpendicular to `along`. Writing a vertex as `pivot + a·along + u·up_perp + (binormal part)`:
    ///
    /// ```text
    /// θ  = k · clamp(a, lo, hi)          k = angle_rad / (hi - lo)   R = 1/k
    /// a' = (R - u)·sin θ + over·cos θ    over = a - clamp(a, lo, hi)
    /// u' = R - (R - u)·cos θ + over·sin θ
    /// ```
    ///
    /// The binormal component is carried through untouched, so a bend is a plane-strain deformation:
    /// it never changes the mesh's width across the bend.
    ///
    /// **The fixed point is `pivot`, NOT the lower limit** — a vertex at `a = 0, u = 0` does not
    /// move, and geometry that straddles `a = 0` curls symmetrically about it. This is Blender's
    /// behaviour (its bend pivots on the object origin), and it is why a mesh that sits entirely off
    /// to one side of `pivot` swings around it rather than bending in place; move `pivot` to bend in
    /// place.
    ///
    /// `over` is the signed distance a vertex lies OUTSIDE `[lo, hi]`. Adding it back along the
    /// arc's end tangent `(cos θ, sin θ)` is what makes the unbent tails rigid and the join C1
    /// continuous — the geometry past a limit rides off straight, with no kink at the boundary.
    ///
    /// `limit_lo` / `limit_hi` are NORMALIZED 0..1 over the mesh's own extent along `along` (Blender's
    /// convention), resolved here rather than by the caller because only the mesh knows that extent.
    /// They are sorted, so an inverted pair is not an error.
    ///
    /// Fails CLOSED (leaves the mesh untouched) on every degenerate input: a non-finite or ~zero
    /// angle, a zero-length or non-finite axis, an `up` parallel to `along` (no curl plane exists),
    /// and a limit span too thin to divide by — never a NaN position.
    pub fn bend_deform(
        &mut self,
        along: Vec3,
        up: Vec3,
        pivot: Vec3,
        angle_rad: f32,
        limit_lo: f32,
        limit_hi: f32,
    ) {
        if !angle_rad.is_finite() || angle_rad.abs() < 1e-6 { return; }
        if !along.is_finite() || !up.is_finite() || !pivot.is_finite() { return; }
        let a_dir = along.normalize_or_zero();
        if a_dir.length_squared() < 0.5 { return; }
        // Gram-Schmidt: only the part of `up` perpendicular to `along` defines a curl plane. An `up`
        // parallel to `along` leaves nothing, and bending in an undefined plane is not a fallback —
        // it is a wrong result, so this no-ops instead.
        let u_dir = (up - a_dir * up.dot(a_dir)).normalize_or_zero();
        if u_dir.length_squared() < 0.5 { return; }

        // Mesh extent along the bend axis, over LIVE vertices only — a freed vertex still occupies a
        // slot with a stale position, and letting one widen the extent would silently rescale every
        // limit the user set.
        let mut lo_a = f32::INFINITY;
        let mut hi_a = f32::NEG_INFINITY;
        for i in 0..self.vertices.len() as u32 {
            if !self.is_vert_live(VertexId(i)) { continue; }
            let a = (self.vertices[i as usize].position - pivot).dot(a_dir);
            lo_a = lo_a.min(a);
            hi_a = hi_a.max(a);
        }
        if !lo_a.is_finite() || !hi_a.is_finite() { return; }

        let (t0, t1) = if limit_lo <= limit_hi { (limit_lo, limit_hi) } else { (limit_hi, limit_lo) };
        let extent = hi_a - lo_a;
        let lo = lo_a + extent * t0.clamp(0.0, 1.0);
        let hi = lo_a + extent * t1.clamp(0.0, 1.0);
        let span = hi - lo;
        // A zero-thickness band (degenerate mesh, or limits dragged onto each other) would make the
        // curvature infinite. No-op rather than fold the mesh into a point.
        if span < 1e-9 { return; }

        let k = angle_rad / span;
        let r = 1.0 / k;
        for i in 0..self.vertices.len() as u32 {
            if !self.is_vert_live(VertexId(i)) { continue; }
            let d = self.vertices[i as usize].position - pivot;
            let a = d.dot(a_dir);
            let u = d.dot(u_dir);
            // Everything the deform frame does not span rides through unchanged.
            let rest = d - a_dir * a - u_dir * u;
            let ac = a.clamp(lo, hi);
            let over = a - ac;
            let (sin, cos) = (k * ac).sin_cos();
            let arm = r - u;
            let na = arm * sin + over * cos;
            let nu = r - arm * cos + over * sin;
            self.vertices[i as usize].position = pivot + rest + a_dir * na + u_dir * nu;
        }
        self.recompute_normals();
    }

    /// Solidify (Blender Solidify modifier): give the surface `thickness` by adding an INNER shell offset
    /// along the vertex normals, a reversed-winding inner copy of every face, and rim quads bridging any
    /// open boundary. `offset` ∈ [-1,1] biases the two shells relative to the original surface:
    /// -1 = original stays outer (Blender default), +1 = original becomes inner, 0 = centered.
    /// A closed input → a hollow shell (no rim); an open input → a watertight slab.
    pub fn solidify(&mut self, thickness: f32, offset: f32) {
        use std::collections::HashMap;
        if thickness.abs() < 1e-9 { return; }
        let out_f = thickness * (1.0 + offset) * 0.5;
        let in_f = thickness * (1.0 - offset) * 0.5;

        let live_verts: Vec<VertexId> = (0..self.vertices.len() as u32)
            .filter(|&i| self.is_vert_live(VertexId(i))).map(VertexId).collect();
        // Per-vertex normals; fall back to any incident FACE normal if the smoothed normal cancels
        // (opposite fans) — a zero normal would collapse the inner vertex onto the outer → degenerate
        // zero-area faces (verification #10).
        let mut fan: Vec<HalfEdgeId> = Vec::new();
        let normals: HashMap<u32, Vec3> = live_verts.iter().map(|&v| {
            let mut n = self.compute_vertex_normal(v);
            if n.length_squared() < 1e-12 {
                // `vertex_fan_into` — the fallback allocated a whole fan `Vec` just to read its
                // first element, once per live vertex.
                self.vertex_fan_into(v, &mut fan);
                if let Some(&h) = fan.first() {
                    let f = self.he(h).face;
                    if f != NO_FACE { n = self.compute_face_normal(f); }
                }
                if n.length_squared() < 1e-12 { n = Vec3::Y; }
            }
            (v.0, n)
        }).collect();

        // Inner-shell vertices (created before the originals move to the outer shell).
        let mut inner: HashMap<u32, VertexId> = HashMap::new();
        for &v in &live_verts {
            let n = normals[&v.0];
            let ipos = self.vert(v).position - n * in_f;
            let iv = self.alloc_vertex(HVertex { position: ipos, normal: -n, half_edge: NO_EDGE });
            inner.insert(v.0, iv);
        }
        // Move the originals out to the outer shell.
        for &v in &live_verts {
            let n = normals[&v.0];
            let p = self.vert(v).position + n * out_f;
            self.vert_mut(v).position = p;
        }
        // Open-boundary edges (captured BEFORE the inner faces exist).
        let boundary: Vec<HalfEdgeId> = (0..self.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| self.is_edge_live(h))
            .filter(|&h| self.he(h).twin == NO_TWIN && self.he(h).face != NO_FACE)
            .collect();
        // Inner faces = every original face, reversed winding (faces inward), on the inner verts.
        let orig_faces: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId).filter(|&f| self.is_face_live(f)).collect();
        let mut ring: Vec<VertexId> = Vec::new();
        let mut iring: Vec<VertexId> = Vec::new();
        for f in &orig_faces {
            self.face_vertices_into(*f, &mut ring);
            iring.clear();
            iring.extend(ring.iter().rev().map(|v| inner[&v.0]));
            // No per-face normal pass — one trailing `recompute_normals()` (see `bridge_loops`).
            self.create_face_no_normals(&iring);
        }
        // Rim quads bridge each open boundary edge outer↔inner. The outer boundary edge is a→b; the rim
        // wound b→a→ia→ib puts b→a opposite a→b (twins it) and ia→ib opposite the inner face's ib→ia.
        for &h in &boundary {
            let a = self.he_source(h);
            let b = self.he(h).vertex;
            let (ia, ib) = (inner[&a.0], inner[&b.0]);
            self.create_face_no_normals(&[b, a, ia, ib]);
        }
        self.recompute_normals();
    }

    /// Flip a face's winding order (reverses its normal).
    /// Swaps next/prev for all half-edges in the face and updates vertex references.
    ///
    /// The normal update is REGION-bounded ([`recompute_normals_region`](Self::recompute_normals_region)
    /// seeded with this face's own corners), not a full-mesh pass. A flip moves exactly one face
    /// normal, so the only vertex normals that can change are that face's corners — everything the
    /// region pass writes is what a full `recompute_normals()` writes, so the result is unchanged
    /// while the cost stops scaling with the mesh. It used to end in a full `recompute_normals()`,
    /// which cost as much as one whole normal pass PER CALL (measured, `fast-release`, 4096-face
    /// plane: 0.166 ms per flip vs 0.170 ms for the entire full pass) — and
    /// `extrude_region_volume` calls it once per region face, so a 4096-face selection paid 4096
    /// full-mesh normal passes.
    pub fn flip_face(&mut self, fid: FaceId) {
        if let Some(seed) = self.flip_face_winding(fid) {
            self.recompute_normals_region(seed);
        }
    }

    /// The winding half of [`flip_face`], WITHOUT the normal update: swap next/prev, rotate the
    /// destination vertices, negate the stored face normal. Returns the face's corner vertices
    /// (the [`recompute_normals_region`] seed), or `None` for a degenerate ring.
    ///
    /// Split out for [`flip_across`], which flips EVERY face and then runs one full
    /// [`recompute_normals`](Self::recompute_normals) — so each per-face region pass was written
    /// and then immediately overwritten. Worse than redundant: a partially-flipped mesh has
    /// inconsistent winding, so `vertex_fan`'s twin→next walk stops finding its start and runs to
    /// its `half_edges.len()` corruption guard, making each region pass O(mesh). Measured
    /// (`fast-release`, closed quad sphere): flip_across 71 ms at 1024 faces, 887 ms at 4096,
    /// 4198 ms at 9216 — i.e. quadratic in a "flip the winding" op. `flip_face` itself is
    /// UNCHANGED (same swap, same seed, same region pass).
    pub(super) fn flip_face_winding(&mut self, fid: FaceId) -> Option<Vec<VertexId>> {
        let hes = self.face_half_edges(fid);
        if hes.len() < 3 { return None; }

        // Collect current state
        let he_data: Vec<(HalfEdgeId, VertexId, HalfEdgeId, HalfEdgeId)> = hes.iter()
            .map(|&h| (h, self.he(h).vertex, self.he(h).next, self.he(h).prev))
            .collect();

        // Swap next/prev for each half-edge
        for &(h, _, next, prev) in &he_data {
            self.half_edges[h.0 as usize].next = prev;
            self.half_edges[h.0 as usize].prev = next;
        }

        // Rotate vertex assignments: each half-edge now points to the vertex
        // that its original prev pointed to
        for i in 0..he_data.len() {
            let (h, _, _, _) = he_data[i];
            // After swap, "prev" is now "next", so the vertex this HE points to
            // should be the source vertex of the original edge (= prev's vertex)
            let prev_idx = if i == 0 { he_data.len() - 1 } else { i - 1 };
            self.half_edges[h.0 as usize].vertex = he_data[prev_idx].1;
        }

        // Negate the face normal
        self.faces[fid.0 as usize].normal = -self.face(fid).normal;
        // The face's own ring (`he_data.1` = each half-edge's destination vertex, i.e. the corners
        // captured before the rewind — the ring is reversed by the flip, never re-membered).
        // Those are the only vertices whose normal a winding flip can move.
        Some(he_data.iter().map(|&(_, v, _, _)| v).collect())
    }

    // ============================================================
    // Mirror (non-destructive modifier)
    // ============================================================

    /// Return a new mesh that is this mesh + mirrored copies across the planes selected by
    /// [`MirrorAxes`] (bitmask: X=1, Y=2, Z=4 — **not** an axis index 0/1/2).
    ///
    /// Multi-axis applies successive single-axis mirrors in X→Y→Z order (Blender Mirror
    /// multi-axis parity: N bits → up to 2^N copies). Empty axes → clone (identity).
    /// If `merge` is true, vertices within `threshold` of each active mirror plane are welded.
    /// Mirrored faces reverse winding so normals point outward.
    pub fn mirrored(&self, axes: MirrorAxes, merge: bool, threshold: f32) -> Self {
        if axes.is_empty() {
            return self.clone();
        }
        let mut out = self.clone();
        // Successive single-axis mirrors: X then Y then Z → deterministic 2^n expansion.
        if axes.contains_x() {
            out = out.mirrored_single_axis(0, merge, threshold);
        }
        if axes.contains_y() {
            out = out.mirrored_single_axis(1, merge, threshold);
        }
        if axes.contains_z() {
            out = out.mirrored_single_axis(2, merge, threshold);
        }
        out
    }

    /// Single-axis mirror: `axis_index` is 0=X, 1=Y, 2=Z (internal / legacy call-site encoding).
    /// Prefer [`Self::mirrored`] with [`MirrorAxes`] at all external boundaries.
    fn mirrored_single_axis(&self, axis_index: u8, merge: bool, threshold: f32) -> Self {
        let axis = axis_index.min(2);
        // Collect live vertices and faces
        let mut live_verts: Vec<(u32, Vec3, Vec3)> = Vec::new(); // (old_idx, pos, normal)
        for (i, v) in self.vertices.iter().enumerate() {
            if !self.is_vert_live(VertexId(i as u32)) { continue; }
            live_verts.push((i as u32, v.position, v.normal));
        }

        let mut live_faces: Vec<Vec<u32>> = Vec::new(); // each face as list of vertex indices
        // Parallel to `live_faces` — the mirror rebuilds every face through `create_face`, which
        // allocates `material_id: 0`. Mirroring materials by default is the owner-specified
        // behaviour ("it should symmetrize material by default").
        let mut live_mats: Vec<u32> = Vec::new();
        let mut ring: Vec<VertexId> = Vec::new();
        for i in 0..self.faces.len() {
            if !self.is_face_live(FaceId(i as u32)) { continue; }
            self.face_vertices_into(FaceId(i as u32), &mut ring);
            live_faces.push(ring.iter().map(|v| v.0).collect());
            live_mats.push(self.faces[i].material_id);
        }

        // Build output positions (original + mirrored)
        let mut positions: Vec<Vec3> = Vec::new();
        let mut old_to_new: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();

        // Original vertices
        for &(old_idx, pos, _) in &live_verts {
            old_to_new.insert(old_idx, positions.len());
            positions.push(pos);
        }

        // Reflect function (axis_index only — never a bitmask)
        let reflect = |p: Vec3| -> Vec3 {
            match axis {
                0 => Vec3::new(-p.x, p.y, p.z),
                1 => Vec3::new(p.x, -p.y, p.z),
                _ => Vec3::new(p.x, p.y, -p.z),
            }
        };

        // Mirrored vertices — map old_idx -> new index for the mirror side
        let mut mirror_map: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for &(old_idx, pos, _) in &live_verts {
            let rp = reflect(pos);
            if merge {
                // Check if this vertex is on the mirror plane (within threshold)
                let plane_dist = match axis { 0 => pos.x.abs(), 1 => pos.y.abs(), _ => pos.z.abs() };
                if plane_dist <= threshold {
                    // Seam vertex: reuse the original
                    mirror_map.insert(old_idx, *old_to_new.get(&old_idx).unwrap());
                    // Snap original to plane
                    let idx = *old_to_new.get(&old_idx).unwrap();
                    match axis {
                        0 => positions[idx].x = 0.0,
                        1 => positions[idx].y = 0.0,
                        _ => positions[idx].z = 0.0,
                    }
                    continue;
                }
            }
            mirror_map.insert(old_idx, positions.len());
            positions.push(rp);
        }

        // Build face index lists: original faces + mirrored (reversed winding).
        // `quad_mats` stays index-parallel to `quads` through BOTH halves.
        let mut quads: Vec<Vec<usize>> = Vec::new();
        let mut quad_mats: Vec<u32> = Vec::new();

        // Original faces
        for (fi, face_verts) in live_faces.iter().enumerate() {
            let mapped: Vec<usize> = face_verts.iter()
                .map(|&v| *old_to_new.get(&v).unwrap_or(&0))
                .collect();
            quads.push(mapped);
            quad_mats.push(live_mats[fi]);
        }

        // Mirrored faces (reversed winding) — same material as the source face they mirror.
        for (fi, face_verts) in live_faces.iter().enumerate() {
            let mut mapped: Vec<usize> = face_verts.iter()
                .map(|&v| *mirror_map.get(&v).unwrap_or(&0))
                .collect();
            mapped.reverse(); // flip winding for correct normals
            quads.push(mapped);
            quad_mats.push(live_mats[fi]);
        }

        // Build new mesh using from_polygons
        let mut mesh = Self::new();
        // Allocate all vertices
        for &p in &positions {
            mesh.alloc_vertex(HVertex {
                position: p,
                normal: Vec3::Y,
                half_edge: NO_EDGE,
            });
        }
        // Create faces. `create_face_no_normals_with_material` — the trailing `recompute_normals()`
        // below is where every normal in the output comes from, and `create_face_with_material`
        // would additionally run a WHOLE-MESH normal pass for each of the 2N faces
        // (`euler.rs:453`). That made the Mirror MODIFIER — re-evaluated on every frame of a
        // slider drag (`app/src/poly/mesh_sync.rs:1809`) — quadratic: measured 5.8 s on a
        // 9216-face cage in `fast-release`.
        let mut vids: Vec<VertexId> = Vec::new();
        for (fi, face) in quads.iter().enumerate() {
            vids.clear();
            vids.extend(face.iter().map(|&i| VertexId(i as u32)));
            mesh.create_face_no_normals_with_material(&vids, quad_mats[fi]);
        }
        mesh.repair_unpaired_twins(); // create_face pairing is order-sensitive — see repair_unpaired_twins
        mesh.recompute_normals();
        mesh
    }

    /// Mirror across an ARBITRARY plane {origin, normal} (not just an axis-0 plane like `mirrored`).
    /// Returns original geometry + its reflection with reversed winding; seam vertices within
    /// `threshold` of the plane are shared (and snapped onto the plane) when `merge`. Used by
    /// `symmetrize` so a symmetry plane through the geometry center (or any rotated normal) is exact.
    pub fn mirrored_across(&self, origin: Vec3, normal: Vec3, merge: bool, threshold: f32) -> Self {
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.5 { return self.clone(); }

        let mut live_verts: Vec<(u32, Vec3)> = Vec::new();
        for (i, v) in self.vertices.iter().enumerate() {
            if !self.is_vert_live(VertexId(i as u32)) { continue; }
            live_verts.push((i as u32, v.position));
        }
        let mut live_faces: Vec<Vec<u32>> = Vec::new();
        // Parallel to `live_faces` — symmetrize mirrors materials by default (owner 2026-08-07).
        let mut live_mats: Vec<u32> = Vec::new();
        let mut ring: Vec<VertexId> = Vec::new();
        for i in 0..self.faces.len() {
            if !self.is_face_live(FaceId(i as u32)) { continue; }
            self.face_vertices_into(FaceId(i as u32), &mut ring);
            live_faces.push(ring.iter().map(|v| v.0).collect());
            live_mats.push(self.faces[i].material_id);
        }

        let mut positions: Vec<Vec3> = Vec::new();
        let mut old_to_new: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for &(old_idx, pos) in &live_verts {
            old_to_new.insert(old_idx, positions.len());
            positions.push(pos);
        }

        // Signed distance to the plane and the mirror reflection about it.
        let signed = |p: Vec3| (p - origin).dot(n);
        let reflect = |p: Vec3| p - 2.0 * signed(p) * n;

        let mut mirror_map: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for &(old_idx, pos) in &live_verts {
            if merge && signed(pos).abs() <= threshold {
                // Seam vertex: share the original, snapped exactly onto the plane.
                let idx = *old_to_new.get(&old_idx).unwrap();
                positions[idx] = pos - signed(pos) * n;
                mirror_map.insert(old_idx, idx);
                continue;
            }
            mirror_map.insert(old_idx, positions.len());
            positions.push(reflect(pos));
        }

        // Drop faces that lie entirely on the plane (all verts within threshold). After a correct
        // delete_half they should already be gone, but if they remain, emitting original + reverse-
        // winding mirror on the same seam verts creates a coplanar double-face "middle poly" band
        // (Alt+X face-selection plane: selected faces sit on the cut). Exclude from both halves.
        let thr = threshold.max(0.0);
        let face_all_on_plane = |fv: &[u32]| -> bool {
            !fv.is_empty() && fv.iter().all(|&vid| {
                let pos = self.vertices.get(vid as usize).map(|v| v.position).unwrap_or(Vec3::ZERO);
                signed(pos).abs() <= thr
            })
        };

        // `quad_mats` is index-parallel to `quads` — the on-plane skip must drop the material with
        // the face, or every later face is painted with its neighbour's material (index drift).
        let mut quads: Vec<Vec<usize>> = Vec::new();
        let mut quad_mats: Vec<u32> = Vec::new();
        for (fi, fv) in live_faces.iter().enumerate() {
            if face_all_on_plane(fv) { continue; }
            quads.push(fv.iter().map(|&v| *old_to_new.get(&v).unwrap_or(&0)).collect());
            quad_mats.push(live_mats[fi]);
        }
        for (fi, fv) in live_faces.iter().enumerate() {
            if face_all_on_plane(fv) { continue; }
            let mut mapped: Vec<usize> = fv.iter().map(|&v| *mirror_map.get(&v).unwrap_or(&0)).collect();
            mapped.reverse(); // reversed winding keeps outward normals on the mirror
            quads.push(mapped);
            quad_mats.push(live_mats[fi]);
        }

        let mut mesh = Self::new();
        for &p in &positions {
            mesh.alloc_vertex(HVertex { position: p, normal: Vec3::Y, half_edge: NO_EDGE });
        }
        // No per-face normal pass — one trailing `recompute_normals()` (see `mirrored_single_axis`).
        let mut vids: Vec<VertexId> = Vec::new();
        for (fi, face) in quads.iter().enumerate() {
            vids.clear();
            vids.extend(face.iter().map(|&i| VertexId(i as u32)));
            mesh.create_face_no_normals_with_material(&vids, quad_mats[fi]);
        }
        mesh.repair_unpaired_twins();
        mesh.recompute_normals();
        mesh
    }

    /// Link every pair of live half-edges (a→b, b→a) that both have `twin == NO_TWIN`.
    ///
    /// `create_face`'s incremental twin auto-pairing is ORDER-SENSITIVE: while a mesh is being
    /// assembled face-by-face, a vertex's outgoing fan can be temporarily disconnected, and the
    /// dst→src search misses an edge that exists — leaving two exact twin candidates both unpaired
    /// (an open "slit" in a closed surface). Proven live by the symmetrize-after-bevel open-seam bug:
    /// after `mirrored_across` rebuilt the mesh, half-edges 57→73 and 73→57 both had NO_TWIN.
    /// This is a deterministic global completion pass keyed by (src,dst) — order-independent, so it
    /// finds every such pair. Run it after any bulk from-faces mesh assembly. Skips would-be
    /// non-manifold matches (two candidates for the same key keep NO_TWIN — a real boundary stays one).
    pub fn repair_unpaired_twins(&mut self) {
        use std::collections::HashMap;
        // (src,dst) -> half-edge id, unpaired only. If a key appears twice the surface is non-manifold
        // there; leave those alone (pairing one arbitrarily would hide the defect).
        let mut open: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for i in 0..self.half_edges.len() as u32 {
            if !self.is_edge_live(HalfEdgeId(i)) { continue; }
            if self.he(HalfEdgeId(i)).twin != NO_TWIN { continue; }
            let src = self.he_source(HalfEdgeId(i)).0;
            let dst = self.he(HalfEdgeId(i)).vertex.0;
            open.entry((src, dst)).or_default().push(i);
        }
        let keys: Vec<(u32, u32)> = open.keys().copied().collect();
        for (src, dst) in keys {
            let fwd = match open.get(&(src, dst)) { Some(v) if v.len() == 1 => v[0], _ => continue };
            let rev = match open.get(&(dst, src)) { Some(v) if v.len() == 1 => v[0], _ => continue };
            if fwd == rev { continue; }
            self.he_mut(HalfEdgeId(fwd)).twin = HalfEdgeId(rev);
            self.he_mut(HalfEdgeId(rev)).twin = HalfEdgeId(fwd);
        }
    }

    /// Delete every face that has **no vertex strictly on the +normal side** of the plane
    /// `{origin, normal}`. Equivalent to keeping only faces with at least one vert at
    /// signed-distance `> eps` (default `1e-5` via [`Self::delete_half`]).
    ///
    /// This drops:
    /// - entirely-negative faces (the discarded half);
    /// - **on-plane-only faces** (all verts within `eps` of the plane) — the Alt+X face-selection
    ///   "middle poly" bug: selected faces define the cut plane so `centroid · n ≈ 0` and the old
    ///   centroid `< 0` test kept them; they then survived into the mirror as a coplanar strip.
    ///
    /// Prefer running [`Self::bisect_plane`] first so straddling faces are split into pure
    /// ± halves before this filter. Vertices that become orphaned are freed by `delete_face`.
    pub fn delete_half(&mut self, origin: Vec3, normal: Vec3) {
        self.delete_half_eps(origin, normal, 1e-5);
    }

    /// [`Self::delete_half`] with an explicit on-plane epsilon (pass the same value used for bisect /
    /// mirror weld so seam faces are classified consistently).
    pub fn delete_half_eps(&mut self, origin: Vec3, normal: Vec3, eps: f32) {
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.5 { return; }
        let eps = eps.max(0.0);

        let mut to_delete: std::collections::HashSet<FaceId> = std::collections::HashSet::new();
        let mut verts: Vec<VertexId> = Vec::new();
        for i in 0..self.faces.len() {
            if !self.is_face_live(FaceId(i as u32)) { continue; }
            self.face_vertices_into(FaceId(i as u32), &mut verts);
            if verts.is_empty() {
                to_delete.insert(FaceId(i as u32));
                continue;
            }
            // Keep only if some vertex is strictly on the +normal side.
            let mut max_sd = f32::NEG_INFINITY;
            for v in &verts {
                let d = (self.vert(*v).position - origin).dot(n);
                if d > max_sd { max_sd = d; }
            }
            if max_sd <= eps {
                to_delete.insert(FaceId(i as u32));
            }
        }
        // ONE batched delete, never a `delete_face` loop. Each `delete_face` call ends
        // with a vertex-disk repair scan that walks the FULL half-edge array whenever a
        // corner went loose — and deleting a half interior makes a corner loose on a
        // large fraction of the calls, i.e. O(faces × half-edges) total. That is the
        // exact class `delete_faces` was built for (2026-08-07 "Del on a UV sphere hung
        // the app"): batch the frees, then ONE repair pass. On a sculpt-sized symmetrize
        // (Alt+X) this is the difference between minutes and milliseconds.
        self.delete_faces(&to_delete);
        self.recompute_normals();
    }

    /// Cut the mesh along the plane {origin, normal}: every edge that STRICTLY crosses the plane gets
    /// a new vertex exactly on it (`split_edge` at the crossing parameter), and every convex face that
    /// straddles the plane is split by a seam edge joining its two on-plane vertices
    /// (`split_face_between`). Vertices already within `eps` of the plane count as on-plane and are not
    /// re-split. After this, no face straddles the plane, so `delete_half` cuts cleanly and the seam
    /// lies exactly on the plane (so a subsequent mirror welds). Returns the seam vertex ids.
    ///
    /// This is what makes `symmetrize` produce clean welded topology instead of the ragged
    /// centroid-deleted boundary that never welded.
    pub fn bisect_plane(&mut self, origin: Vec3, normal: Vec3, eps: f32) -> Vec<VertexId> {
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.5 { return Vec::new(); }
        let sd = |p: Vec3| (p - origin).dot(n);

        // Existing on-plane vertices are already seam verts.
        let mut seam: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for i in 0..self.vertices.len() {
            if !self.is_vert_live(VertexId(i as u32)) { continue; }
            if sd(self.vert(VertexId(i as u32)).position).abs() <= eps { seam.insert(i as u32); }
        }

        // 1. Split every strictly-crossing edge once (process the lower half-edge of each undirected
        //    edge; `split_edge` splits the twin too). Snapshot the count so new half-edges are skipped.
        let he_count = self.half_edges.len();
        for i in 0..he_count {
            let he = HalfEdgeId(i as u32);
            if !self.is_edge_live(he) { continue; }
            let twin = self.he(he).twin;
            if twin != NO_TWIN && twin.0 < i as u32 { continue; }
            let src = self.he_source(he);
            let dst = self.he(he).vertex;
            let ds = sd(self.vert(src).position);
            let dd = sd(self.vert(dst).position);
            let crosses = (ds > eps && dd < -eps) || (ds < -eps && dd > eps);
            if crosses {
                let t = ds / (ds - dd); // exact crossing parameter in (0,1)
                let nv = self.split_edge(he, t.clamp(0.0, 1.0));
                seam.insert(nv.0);
            }
        }

        // 2. Split every face that STILL straddles the plane (vertices strictly on both sides) with
        //    chords joining on-plane vertices, until no live face straddles.
        //
        //    After step 1 a sign change around a ring can only happen THROUGH an on-plane vertex (every
        //    strictly-crossing edge was split), so a ring reads as runs of + and − separated by zeros.
        //    A face that crosses ONCE has one + run and one − run and needs ONE chord. But a face can
        //    cross the plane TWICE — any non-planar / saddle n-gon, whose ring reads +,0,−,0,+,0,−,0 —
        //    and then NO single chord separates the sides. The old one-chord-per-face pass found no
        //    "separating" pair for those and left the face straddling; `delete_half` then KEPT it (it
        //    owns + verts), so the kept half's seam sat far off the plane — outside the mirror weld
        //    threshold — leaving an OPEN hole plus a mirrored copy crossing back through the plane.
        //    That is the Alt+X "bad geo" with a face-selected mirror plane: the plane is derived FROM a
        //    face, and a non-planar face always straddles its own plane in exactly this pattern.
        //
        //    So: split off ONE sign-pure arc at a time and re-queue both halves. Both arcs are required
        //    non-empty, so each split strictly shrinks both pieces (k+2 < n and n−k < n) — this
        //    terminates. A straddling face always HAS such a chord: take a non-empty + run and the
        //    zeros bounding it — that arc is pure, and the opposite arc holds the − run.
        let mut work: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .collect();
        while let Some(f) = work.pop() {
            if !self.is_face_live(f) { continue; }
            let ring = self.face_vertices(f);
            let n = ring.len();
            if n < 4 { continue; } // a straddling tri was already edge-split into a bigger n-gon
            let signs: Vec<i8> = ring.iter().map(|v| {
                if seam.contains(&v.0) { 0 }
                else { let d = sd(self.vert(*v).position); if d > eps { 1 } else if d < -eps { -1 } else { 0 } }
            }).collect();
            if !(signs.contains(&1) && signs.contains(&-1)) { continue; } // not straddling
            let zeros: Vec<usize> = (0..n).filter(|&i| signs[i] == 0).collect();
            // Signs on the open arc strictly between two ring positions, plus its length.
            let arc = |from: usize, to: usize| -> (bool, bool, usize) {
                let (mut pos, mut neg, mut len) = (false, false, 0usize);
                let mut i = (from + 1) % n;
                while i != to {
                    match signs[i] { 1 => pos = true, -1 => neg = true, _ => {} }
                    len += 1;
                    i = (i + 1) % n;
                }
                (pos, neg, len)
            };
            // Chord between two on-plane verts with BOTH arcs non-empty (else it duplicates a ring
            // edge) and at least one arc SIGN-PURE — that arc becomes a piece fully on one side, and
            // whatever still straddles comes back through the work list. Faces are small; brute-force.
            let mut pick: Option<(VertexId, VertexId)> = None;
            'pairs: for (ai, &ia) in zeros.iter().enumerate() {
                for &ib in zeros.iter().skip(ai + 1) {
                    let (p1, n1, l1) = arc(ia, ib);
                    let (p2, n2, l2) = arc(ib, ia);
                    if l1 == 0 || l2 == 0 { continue; }
                    // `p != n` = the arc holds exactly one sign (zeros may be interspersed — they are
                    // ON the plane, so the piece is still pure). An all-zero arc is NOT accepted: it
                    // would split off an on-plane sliver that `delete_half` then removes as a hole.
                    if (p1 != n1) || (p2 != n2) {
                        pick = Some((ring[ia], ring[ib]));
                        break 'pairs;
                    }
                }
            }
            if let Some((va, vb)) = pick {
                if let Some(f2) = self.split_face_between(f, va, vb) {
                    work.push(f);
                    work.push(f2);
                }
            }
        }
        debug_assert!(
            (0..self.faces.len() as u32).map(FaceId).filter(|&f| self.is_face_live(f)).all(|f| {
                let vs = self.face_vertices(f);
                !(vs.iter().any(|&v| sd(self.vert(v).position) > eps)
                    && vs.iter().any(|&v| sd(self.vert(v).position) < -eps))
            }),
            "bisect_plane left a face straddling the plane — delete_half would keep it and the seam \
             could not weld"
        );

        seam.into_iter().map(VertexId).collect()
    }

    /// Symmetrize: bisect at the plane, delete the negative half (and any on-plane-only faces), then
    /// mirror the positive half back across the plane (welding the seam). Uses the ARBITRARY-plane
    /// reflection so the plane may sit anywhere (e.g. through the geometry center) and carry any
    /// normal (e.g. a rotated body's axis), not only an axis-0 world plane. The bisect step is what
    /// gives a clean welded seam; the on-plane face drop stops the Alt+X face-plane "middle poly".
    pub fn symmetrize(&mut self, origin: Vec3, normal: Vec3, merge: bool, threshold: f32) {
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.5 { return; }

        let eps = threshold.max(1e-5);
        self.bisect_plane(origin, n, eps);
        self.delete_half_eps(origin, n, eps);
        *self = self.mirrored_across(origin, n, merge, threshold);
    }

    /// Flip (reflect) all vertices across the given plane.
    pub fn flip_across(&mut self, origin: Vec3, normal: Vec3) {
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.5 { return; }
        for i in 0..self.vertices.len() {
            if !self.is_vert_live(VertexId(i as u32)) { continue; }
            let p = self.vertices[i].position;
            let d = (p - origin).dot(n);
            self.vertices[i].position = p - 2.0 * d * n;
        }
        // Flip all face windings to maintain outward normals. Winding only — the single full
        // `recompute_normals()` below is what every normal in the result comes from, so the
        // per-face region passes `flip_face` would run are written and then overwritten (and are
        // O(mesh) each while the mesh is half-flipped — see `flip_face_winding`).
        let face_ids: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .collect();
        for fid in face_ids {
            self.flip_face_winding(fid);
        }
        self.recompute_normals();
    }


    /// Single-segment chamfer bevel on selected edges. Replaces each selected edge with a
    /// quad strip face, sliding adjacent face vertices inward by `width`.
    /// Corner triangles fill gaps at edge endpoints.
    /// Always a flat 1-segment chamfer — multi-segment / shaped bevels live in
    /// `bevel_op::bevel_edges_offset` (the production path). The old `segments`/`shape`
    /// params here were `_`-ignored and never read (findings §10); removed 2026-08-02.
    pub fn bevel_edges(&mut self, selected: &std::collections::HashSet<u32>, width: f32) {
        self.bevel_edges_ex(selected, width);
    }

    pub fn debug_cube_topology(&self) {
        eprintln!("=== MESH TOPOLOGY DUMP ===");
        eprintln!("Vertices: {} total", self.vertices.len());
        for i in 0..self.vertices.len() {
            let v = &self.vertices[i];
            eprintln!("  V{}: pos=({:.4}, {:.4}, {:.4}) he={}", i, v.position.x, v.position.y, v.position.z, v.half_edge.0);
        }
        eprintln!("HalfEdges: {} total", self.half_edges.len());
        for i in 0..self.half_edges.len() {
            let he = &self.half_edges[i];
            let src = self.he_source(HalfEdgeId(i as u32)).0;
            eprintln!("  HE{}: v{}→v{} twin={} next={} prev={} face={}",
                i, src, he.vertex.0,
                if he.twin == NO_TWIN { "NONE".to_string() } else { format!("{}", he.twin.0) },
                he.next.0, he.prev.0,
                if he.face == NO_FACE { "NONE".to_string() } else { format!("{}", he.face.0) });
        }
        eprintln!("Faces: {} total, free: {:?}", self.faces.len(), self.free_faces);
        for i in 0..self.faces.len() {
            if !self.is_face_live(FaceId(i as u32)) { continue; }
            let fid = FaceId(i as u32);
            let hes = self.face_half_edges(fid);
            let verts = self.face_vertices(fid);
            eprint!("  F{}: verts=[", i);
            for (j, v) in verts.iter().enumerate() {
                if j > 0 { eprint!(", "); }
                eprint!("v{}", v.0);
            }
            eprint!("] hes=[");
            for (j, &he) in hes.iter().enumerate() {
                if j > 0 { eprint!(", "); }
                let s = self.he_source(he).0;
                let d = self.he(he).vertex.0;
                eprint!("he{}(v{}→v{})", he.0, s, d);
            }
            eprintln!("]");
        }
        eprintln!("=== END TOPOLOGY DUMP ===");
    }

    /// Legacy single-segment endpoint-slide chamfer (superseded by `bevel_op::bevel_edges_offset`).
    /// Formerly took `_segments`/`_shape` params it never read — a caller asking for a multi-segment
    /// bevel silently got a 1-segment chamfer. Grep proved no caller ever depended on them (only the
    /// defaults-passing `bevel_edges` wrapper + in-crate tests asserting validity, not segment count),
    /// so the dead params were removed rather than wired (findings §10).
    pub fn bevel_edges_ex(&mut self, selected: &std::collections::HashSet<u32>, width: f32) {
        if selected.is_empty() || width <= 0.0 { return; }
        use std::collections::{HashMap, HashSet};

        // =================================================================
        // STEP 1: Collect bevel data — edge-slide offsets.
        // =================================================================
        let mut positions: Vec<Vec3> = self.vertices.iter().map(|v| v.position).collect();

        struct BevelEdge {
            src_offset: usize,
            dst_offset: usize,
        }

        let mut bevel_data: HashMap<u32, BevelEdge> = HashMap::new();
        let mut corner_offsets: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut affected_faces: HashSet<u32> = HashSet::new();
        let mut processed_edges: HashSet<u32> = HashSet::new();
        let mut primary_edges: Vec<u32> = Vec::new();

        for &eid in selected {
            if eid as usize >= self.half_edges.len() { continue; }
            let he = self.half_edges[eid as usize].clone();
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if processed_edges.contains(&he.twin.0) { continue; }
            processed_edges.insert(eid);
            processed_edges.insert(he.twin.0);
            primary_edges.push(eid);
            let twin = self.half_edges[he.twin.0 as usize].clone();
            if twin.face == NO_FACE { continue; }

            let src = self.he_source(HalfEdgeId(eid)).0 as usize;
            let dst = he.vertex.0 as usize;
            let src_pos = positions[src];
            let dst_pos = positions[dst];
            let edge_len = (dst_pos - src_pos).length();
            if edge_len < 1e-7 { continue; }

            let w = width.min(edge_len * 0.45).max(0.0001);

            // LEFT FACE (he.face): slide src toward prev, dst toward next
            let prev_vert = self.he_source(he.prev).0 as usize;
            let next_vert = self.half_edges[he.next.0 as usize].vertex.0 as usize;
            let src_left_dir = (positions[prev_vert] - src_pos).normalize_or_zero();
            let dst_left_dir = (positions[next_vert] - dst_pos).normalize_or_zero();
            let w_src_left = w.min((positions[prev_vert] - src_pos).length() * 0.49);
            let w_dst_left = w.min((positions[next_vert] - dst_pos).length() * 0.49);

            let src_left = positions.len();
            positions.push(src_pos + src_left_dir * w_src_left);
            let dst_left = positions.len();
            positions.push(dst_pos + dst_left_dir * w_dst_left);

            // RIGHT FACE (twin.face): twin goes dst→src
            let prev_twin_vert = self.he_source(twin.prev).0 as usize;
            let next_twin_vert = self.half_edges[twin.next.0 as usize].vertex.0 as usize;
            let dst_right_dir = (positions[prev_twin_vert] - dst_pos).normalize_or_zero();
            let src_right_dir = (positions[next_twin_vert] - src_pos).normalize_or_zero();
            let w_dst_right = w.min((positions[prev_twin_vert] - dst_pos).length() * 0.49);
            let w_src_right = w.min((positions[next_twin_vert] - src_pos).length() * 0.49);

            let src_right = positions.len();
            positions.push(src_pos + src_right_dir * w_src_right);
            let dst_right = positions.len();
            positions.push(dst_pos + dst_right_dir * w_dst_right);

            bevel_data.insert(eid, BevelEdge { src_offset: src_left, dst_offset: dst_left });
            bevel_data.insert(he.twin.0, BevelEdge { src_offset: dst_right, dst_offset: src_right });

            affected_faces.insert(he.face.0);
            affected_faces.insert(twin.face.0);
            corner_offsets.entry(src).or_default().extend_from_slice(&[src_left, src_right]);
            corner_offsets.entry(dst).or_default().extend_from_slice(&[dst_left, dst_right]);
        }

        if bevel_data.is_empty() { return; }

        // =================================================================
        // STEP 1b: Add ALL faces touching beveled VERTICES to affected_faces.
        // Without this, faces like F4/F5 on a cube keep the original vertex
        // while the bevel strip creates new vertices, causing from_polygons
        // to produce non-manifold edges.
        // =================================================================
        for (&vert, _) in &corner_offsets {
            let vid = VertexId(vert as u32);
            for &he_id in &self.vertex_fan(vid) {
                let fid = self.he(he_id).face;
                if fid != NO_FACE {
                    affected_faces.insert(fid.0);
                }
            }
        }

        // =================================================================
        // STEP 2: Collect all face vertex lists for final rebuild.
        // =================================================================
        let mut face_lists: Vec<Vec<usize>> = Vec::new();
        // Index-parallel: the face each output polygon descends from (strip quads are new).
        let mut face_parent: Vec<Option<FaceId>> = Vec::new();
        let mut fid_to_idx: Vec<Option<usize>> = vec![None; self.faces.len()];
        let mut ring: Vec<VertexId> = Vec::new();
        for i in 0..self.faces.len() {
            if !self.is_face_live(FaceId(i as u32)) { continue; }
            let idx = face_lists.len();
            fid_to_idx[i] = Some(idx);
            self.face_vertices_into(FaceId(i as u32), &mut ring);
            face_lists.push(ring.iter().map(|v| v.0 as usize).collect());
            face_parent.push(Some(FaceId(i as u32)));
        }

        // =================================================================
        // STEP 3: Rebuild affected faces via half-edge traversal.
        // =================================================================
        for &fid_raw in &affected_faces {
            let fid = FaceId(fid_raw);
            let face_hes = self.face_half_edges(fid);
            if face_hes.is_empty() { continue; }

            let mut new_verts: Vec<usize> = Vec::new();

            for (i, &he_id) in face_hes.iter().enumerate() {
                let prev_he_id = face_hes[if i == 0 { face_hes.len() - 1 } else { i - 1 }];
                let src = self.he_source(he_id).0 as usize;

                let cur_beveled = bevel_data.contains_key(&he_id.0);
                let prev_beveled = bevel_data.contains_key(&prev_he_id.0);

                match (prev_beveled, cur_beveled) {
                    (false, false) => {
                        if let Some(offsets) = corner_offsets.get(&src) {
                            let prev_src = self.he_source(prev_he_id).0 as usize;
                            let prev_dir = (positions[prev_src] - positions[src]).normalize_or_zero();

                            let mut sorted: Vec<usize> = offsets.clone();
                            let eps = 1e-5_f32;
                            sorted.dedup_by(|a, b| {
                                (positions[*a] - positions[*b]).length_squared() < eps * eps
                            });
                            sorted.sort_by(|&a, &b| {
                                let da = (positions[a] - positions[src]).normalize_or_zero();
                                let db = (positions[b] - positions[src]).normalize_or_zero();
                                db.dot(prev_dir).partial_cmp(&da.dot(prev_dir))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                            for &o in &sorted {
                                new_verts.push(o);
                            }
                        } else {
                            new_verts.push(src);
                        }
                    }
                    (false, true) => {
                        new_verts.push(bevel_data[&he_id.0].src_offset);
                    }
                    (true, false) => {
                        new_verts.push(bevel_data[&prev_he_id.0].dst_offset);
                    }
                    (true, true) => {
                        new_verts.push(bevel_data[&prev_he_id.0].dst_offset);
                        new_verts.push(bevel_data[&he_id.0].src_offset);
                    }
                }
            }

            if let Some(idx) = fid_to_idx[fid_raw as usize] {
                if new_verts.len() >= 3 {
                    // Preserve original face winding
                    let orig_n = {
                        let ov = &face_lists[idx];
                        if ov.len() >= 3 {
                            (positions[ov[1]] - positions[ov[0]]).cross(positions[ov[2]] - positions[ov[0]]).normalize_or_zero()
                        } else { Vec3::Y }
                    };
                    let new_n = (positions[new_verts[1]] - positions[new_verts[0]])
                        .cross(positions[new_verts[2]] - positions[new_verts[0]]);
                    if new_n.dot(orig_n) < 0.0 {
                        new_verts.reverse();
                    }
                    face_lists[idx] = new_verts;
                }
            }
        }

        // =================================================================
        // STEP 4: Create bevel strip quad.
        // =================================================================
        for &eid in &primary_edges {
            let bd = &bevel_data[&eid];
            let twin_id = self.half_edges[eid as usize].twin.0;
            let bd_twin = &bevel_data[&twin_id];

            let quad = vec![bd.src_offset, bd_twin.dst_offset, bd_twin.src_offset, bd.dst_offset];
            face_lists.push(quad);
            face_parent.push(None);
        }

        // =================================================================
        // NO CORNER FILLS. All faces touching beveled vertices are rebuilt
        // in STEP 3 with the offset vertices, so there are no gaps.
        // =================================================================

        // =================================================================
        // STEP 5: Rebuild mesh from polygon data. ID-STABLE (POLY-STALE-IDS): soup index =
        // vertex id, offsets appended; every face keeps its id, strip quads are new. Materials
        // stay as before (`&[]` → 0) — this legacy chamfer has no production caller.
        // =================================================================
        let pos_origin = self.id_indexed_origin(positions.len());
        *self = Self::from_polygons_keeping_ids(self, &positions, &pos_origin, &face_lists, &face_parent, &[]);
        #[cfg(debug_assertions)]
        self.validate_mesh("bevel_edges_ex");
    }

    // ============================================================
    // Subdivision
    // ============================================================

    /// Catmull-Clark subdivision. Replaces the mesh in-place.
    /// Every N-gon becomes N quads. Preserves boundary edges.
    pub fn subdivide_catmull_clark(&mut self) {
        use std::collections::HashMap;

        // Collect active face/edge/vert indices (skip free-list slots)
        let active_faces: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .collect();
        let active_verts: Vec<VertexId> = (0..self.vertices.len() as u32)
            .filter(|&i| self.is_vert_live(VertexId(i)))
            .map(VertexId)
            .collect();

        // 1. Face points: average of face vertex positions
        let mut face_points: HashMap<u32, Vec3> = HashMap::new();
        for &fid in &active_faces {
            let verts = self.face_vertices(fid);
            let avg = verts.iter().map(|v| self.vert(*v).position).sum::<Vec3>() / verts.len() as f32;
            face_points.insert(fid.0, avg);
        }

        // 2. Edge points: collect unique edges (use sorted (min,max) vertex pair as key)
        //    Edge point = avg of two endpoints + two adjacent face points, divided by 4
        //    For boundary edges: midpoint of endpoints only
        let mut edge_map: HashMap<(u32, u32), Vec3> = HashMap::new();
        let mut he_to_edge_key: HashMap<u32, (u32, u32)> = HashMap::new();
        for (i, he) in self.half_edges.iter().enumerate() {
            if !self.is_edge_live(HalfEdgeId(i as u32)) { continue; }
            let src = self.he_source(HalfEdgeId(i as u32));
            let dst = he.vertex;
            let key = if src.0 < dst.0 { (src.0, dst.0) } else { (dst.0, src.0) };
            he_to_edge_key.insert(i as u32, key);
            if edge_map.contains_key(&key) { continue; }

            let p0 = self.vert(src).position;
            let p1 = self.vert(dst).position;

            if he.twin == NO_TWIN {
                // Boundary edge: midpoint
                edge_map.insert(key, (p0 + p1) * 0.5);
            } else {
                let f0 = he.face;
                let f1 = self.he(he.twin).face;
                let fp0 = face_points.get(&f0.0).copied().unwrap_or(Vec3::ZERO);
                let fp1 = face_points.get(&f1.0).copied().unwrap_or(Vec3::ZERO);
                edge_map.insert(key, (p0 + p1 + fp0 + fp1) * 0.25);
            }
        }

        // 3. New vertex positions
        let mut new_vert_pos: HashMap<u32, Vec3> = HashMap::new();
        for &vid in &active_verts {
            let p = self.vert(vid).position;
            let fan = self.vertex_fan(vid);
            let n = fan.len() as f32;
            if n < 1.0 { new_vert_pos.insert(vid.0, p); continue; }

            let is_boundary = fan.iter().any(|&h| self.he(h).twin == NO_TWIN);
            if is_boundary {
                // Boundary vertex: average of adjacent boundary edge midpoints + self
                let mut boundary_mid_sum = Vec3::ZERO;
                let mut boundary_count = 0u32;
                for &h in &fan {
                    if self.he(h).twin == NO_TWIN {
                        let dst = self.he(h).vertex;
                        boundary_mid_sum += (p + self.vert(dst).position) * 0.5;
                        boundary_count += 1;
                    }
                }
                if boundary_count >= 2 {
                    new_vert_pos.insert(vid.0, (boundary_mid_sum + p * (boundary_count as f32 - 2.0)) / boundary_count as f32);
                } else {
                    new_vert_pos.insert(vid.0, p);
                }
            } else {
                // Interior vertex: (F + 2R + (n-3)P) / n
                let mut f_avg = Vec3::ZERO;
                let mut r_avg = Vec3::ZERO;
                for &h in &fan {
                    let fid = self.he(h).face;
                    if fid != NO_FACE {
                        f_avg += face_points.get(&fid.0).copied().unwrap_or(Vec3::ZERO);
                    }
                    let dst = self.he(h).vertex;
                    r_avg += (p + self.vert(dst).position) * 0.5;
                }
                f_avg /= n;
                r_avg /= n;
                new_vert_pos.insert(vid.0, (f_avg + r_avg * 2.0 + p * (n - 3.0)) / n);
            }
        }

        // 4. Build new mesh: for each face with N verts, create N quads
        //    Quad corners: face_point, edge_point(prev_edge), new_vert_pos(v), edge_point(cur_edge)
        let mut positions: Vec<Vec3> = Vec::new();
        let mut quads: Vec<[usize; 4]> = Vec::new();
        // Index-parallel to `quads` — each sub-quad inherits its parent face's material. Without it
        // the whole subdivided body returns as material 0 (the Ctrl+X / Alt+X material-loss class).
        let mut quad_mats: Vec<u32> = Vec::new();
        let mut pos_map: HashMap<u64, usize> = HashMap::new(); // key → index in positions

        // Key encoding: face points = 0x1_XXXXXXXX, edge points = 0x2 + sorted pair, vert points = 0x3 + vert_id
        let face_key = |fid: u32| -> u64 { 0x1_0000_0000 | fid as u64 };
        let edge_key = |a: u32, b: u32| -> u64 {
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            0x2_0000_0000 | ((lo as u64) << 20) | (hi as u64)
        };
        let vert_key = |vid: u32| -> u64 { 0x3_0000_0000 | vid as u64 };

        let get_or_insert = |key: u64, pos: Vec3, positions: &mut Vec<Vec3>, pos_map: &mut HashMap<u64, usize>| -> usize {
            if let Some(&idx) = pos_map.get(&key) { return idx; }
            let idx = positions.len();
            positions.push(pos);
            pos_map.insert(key, idx);
            idx
        };

        for &fid in &active_faces {
            let face_mat = self.face(fid).material_id;
            let face_hes = self.face_half_edges(fid);
            let fp = face_points[&fid.0];
            let fp_idx = get_or_insert(face_key(fid.0), fp, &mut positions, &mut pos_map);

            for (i, &he_id) in face_hes.iter().enumerate() {
                let prev_he_id = face_hes[(i + face_hes.len() - 1) % face_hes.len()];
                // The corner vertex of this sub-quad is the SOURCE of `he_id` (v_i), NOT its destination.
                // Both edge points below — prev edge (v_{i-1},v_i) and cur edge (v_i,v_{i+1}) — are the
                // two edges INCIDENT to v_i. Using the destination (the prior bug) paired v_{i+1} with an
                // edge point that does not touch it, so the shared-edge half-edges across faces did not
                // line up (v_i→E on one side, but no E→v_i twin on the other) → bowtie / non-manifold.
                let v = self.he_source(he_id);

                // Edge point for prev edge
                let prev_src = self.he_source(prev_he_id);
                let prev_dst = self.he(prev_he_id).vertex;
                let prev_ek = if prev_src.0 < prev_dst.0 { (prev_src.0, prev_dst.0) } else { (prev_dst.0, prev_src.0) };
                let prev_ep = edge_map[&prev_ek];
                let prev_ep_idx = get_or_insert(edge_key(prev_ek.0, prev_ek.1), prev_ep, &mut positions, &mut pos_map);

                // Vertex point
                let vp = new_vert_pos[&v.0];
                let vp_idx = get_or_insert(vert_key(v.0), vp, &mut positions, &mut pos_map);

                // Edge point for current edge
                let cur_src = self.he_source(he_id);
                let cur_dst = self.he(he_id).vertex;
                let cur_ek = if cur_src.0 < cur_dst.0 { (cur_src.0, cur_dst.0) } else { (cur_dst.0, cur_src.0) };
                let cur_ep = edge_map[&cur_ek];
                let cur_ep_idx = get_or_insert(edge_key(cur_ek.0, cur_ek.1), cur_ep, &mut positions, &mut pos_map);

                // Quad: face_point → prev_edge_point → vertex → cur_edge_point
                quads.push([fp_idx, prev_ep_idx, vp_idx, cur_ep_idx]);
                quad_mats.push(face_mat);
            }
        }

        // Replace self with new mesh (materials carried — plain `from_quads` would zero them).
        *self = Self::from_quads_with_materials(&positions, &quads, &quad_mats);
    }

    /// Subdivide only the specified faces (linear / non-smoothing — the cube stays a cube, just denser).
    /// Each selected N-gon becomes N quads using its face center + edge midpoints.
    ///
    /// MANIFOLD-CORRECT PARTIAL SELECTION (the T31537 "Phase 5" fix; user report #4). A selected face's
    /// boundary edge that it shares with an UNSELECTED neighbour is split on the selected side only — the
    /// neighbour would still span the full original edge, leaving the new midpoint with faces on one side
    /// only (a T-junction = bowtie = non-manifold, which the `apply_in_place` `is_manifold()` gate then
    /// REFUSES → "subdivide does nothing"). Fix: insert that midpoint into the neighbour's vertex ring
    /// too, so the neighbour becomes an n-gon and the mesh stays closed + manifold. A neighbour bordering
    /// several selected faces receives several midpoints, in ring order.
    ///
    /// This matches Blender's Mesh→Subdivide by VERTEX IDENTITY (position multiset = 8 corners + every
    /// selected-face edge midpoint + one center per selected face). Blender additionally TRIANGULATES a
    /// neighbour that gets ≥2 adjacent cuts; AZRA keeps it as a manifold n-gon — same vertices, different
    /// internal tessellation. ID-preserving: a whole-body `from_polygons_keeping_ids` rebuild (the
    /// Snapshot inverse tier accounts for that honest O(body) cost) in which every vertex, edge and
    /// face the op does not consume keeps its id; each selected face's first child quad keeps the
    /// parent's id.
    pub fn subdivide_faces(&mut self, face_ids: &std::collections::HashSet<u32>) {
        if face_ids.is_empty() { return; }

        // Base positions = current vertices (indices preserved for the pass below).
        let mut positions: Vec<Vec3> = self.vertices.iter().map(|v| v.position).collect();

        use std::collections::HashMap;
        // (min_v, max_v) -> midpoint position index. Shared by BOTH the selected split and the
        // neighbour ring insertion so a split edge yields exactly ONE midpoint used by every incident face.
        let mut edge_mid_map: HashMap<(u32, u32), usize> = HashMap::new();

        let active_faces: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .collect();

        let edge_key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };

        // PASS 1 — create the midpoint for every edge of every SELECTED face. Doing this before ANY face
        // is emitted removes the iteration-order hazard: an unselected neighbour visited before its
        // selected partner still finds the shared edge's midpoint already present.
        for &fid in &active_faces {
            if !face_ids.contains(&fid.0) { continue; }
            let verts = self.face_vertices(fid);
            let n = verts.len();
            if n < 3 { continue; }
            for i in 0..n {
                let va = verts[i].0;
                let vb = verts[(i + 1) % n].0;
                let key = edge_key(va, vb);
                edge_mid_map.entry(key).or_insert_with(|| {
                    let mid = (positions[va as usize] + positions[vb as usize]) * 0.5;
                    let idx = positions.len();
                    positions.push(mid);
                    idx
                });
            }
        }

        // PASS 2 — emit faces.
        let mut new_face_lists: Vec<Vec<usize>> = Vec::new();
        // Index-parallel to `new_face_lists` — every child quad inherits its parent face's material
        // (and every untouched neighbour keeps its own). Without this the rebuild returns the whole
        // mesh as material 0, the same class as the Ctrl+X / Alt+X material loss.
        let mut new_face_mats: Vec<u32> = Vec::new();
        // Index-parallel too: the face each output polygon descends from (the first child quad
        // keeps the parent's id). Filled lazily — each iteration stamps the PREVIOUS face's
        // polygons, because the branches below `continue` from more than one place.
        let mut new_face_parent: Vec<Option<FaceId>> = Vec::new();
        let mut parent: Option<FaceId> = None;
        for &fid in &active_faces {
            new_face_parent.resize(new_face_lists.len(), parent);
            parent = Some(fid);
            let mat = self.face(fid).material_id;
            let verts = self.face_vertices(fid);
            let vert_indices: Vec<usize> = verts.iter().map(|v| v.0 as usize).collect();
            let n = vert_indices.len();

            if face_ids.contains(&fid.0) {
                if n < 3 { new_face_lists.push(vert_indices); new_face_mats.push(mat); continue; }
                // Face center.
                let center: Vec3 = vert_indices.iter().map(|&vi| positions[vi]).sum::<Vec3>() / n as f32;
                let center_idx = positions.len();
                positions.push(center);
                // Midpoints (all created in pass 1).
                let mid_indices: Vec<usize> = (0..n).map(|i| {
                    edge_mid_map[&edge_key(verts[i].0, verts[(i + 1) % n].0)]
                }).collect();
                // N quads: center, mid_prev, vert_i, mid_cur.
                for i in 0..n {
                    let prev_mid = mid_indices[(i + n - 1) % n];
                    let cur_mid = mid_indices[i];
                    new_face_lists.push(vec![center_idx, prev_mid, vert_indices[i], cur_mid]);
                    new_face_mats.push(mat);
                }
            } else {
                // Unselected neighbour: walk the ring, inserting the midpoint of any split edge right
                // after its source vertex (→ n-gon). Multiple split edges each contribute in ring order.
                let mut ring: Vec<usize> = Vec::with_capacity(n + 2);
                for i in 0..n {
                    ring.push(vert_indices[i]);
                    if let Some(&mid) = edge_mid_map.get(&edge_key(verts[i].0, verts[(i + 1) % n].0)) {
                        ring.push(mid);
                    }
                }
                new_face_lists.push(ring);
                new_face_mats.push(mat);
            }
        }

        // ID-STABLE (POLY-STALE-IDS): soup index = vertex id, midpoints / centers appended.
        new_face_parent.resize(new_face_lists.len(), parent);
        let pos_origin = self.id_indexed_origin(positions.len());
        *self = Self::from_polygons_keeping_ids(
            self,
            &positions,
            &pos_origin,
            &new_face_lists,
            &new_face_parent,
            &new_face_mats,
        );
        #[cfg(debug_assertions)]
        self.validate_mesh("subdivide_faces");
    }

    /// POKE each selected face into a triangle fan from a NEW center vertex (Blender `bpy.ops.mesh.poke`).
    ///
    /// Center = Blender's DEFAULT `center_mode=MEDIAN_WEIGHTED` (`face_center_median_weighted` — each
    /// vertex weighted by its two incident edge lengths, matching Blender EXACTLY incl. irregular faces;
    /// the plain centroid used by `subdivide_faces` does NOT match on a triangle/irregular quad, verified
    /// vs `poke_ref.json`). `offset` pushes the center OUT along the (unit) face normal (Blender
    /// `use_relative_offset=False`): the cube at offset 0.5 lands its +Y center at `(0,1.5,0)`.
    ///
    /// Works on tris/quads/ngons; a tri → 3 tris, a quad → 4, an n-gon → n. UNSELECTED faces are copied
    /// unchanged (poke only fans INSIDE a face — it never touches a shared boundary edge, so neighbours
    /// stay watertight with no T-junction, unlike `subdivide_faces`). Rebuilt via
    /// `from_polygons_keeping_ids`: the ORIGINAL vertices, every untouched edge and face, and each poked
    /// face (on its first fan triangle) keep their ids; the new centers are appended. Empty selection / a
    /// <3-vertex "face" is a no-op (the caller refuses).
    pub fn poke_faces(&mut self, face_ids: &std::collections::HashSet<u32>, offset: f32) {
        if face_ids.is_empty() { return; }
        let mut positions: Vec<Vec3> = self.vertices.iter().map(|v| v.position).collect();
        let active_faces: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .collect();
        let mut new_face_lists: Vec<Vec<usize>> = Vec::new();
        // Index-parallel to `new_face_lists`: every fan triangle inherits the poked face's material,
        // every copied face keeps its own. Without this the whole mesh returns as material 0.
        let mut new_face_mats: Vec<u32> = Vec::new();
        // And the face each output polygon descends from: the first fan triangle keeps the
        // poked face's id, a copied face keeps its own.
        let mut new_face_parent: Vec<Option<FaceId>> = Vec::new();
        for &fid in &active_faces {
            let mat = self.face(fid).material_id;
            let verts = self.face_vertices(fid);
            let vi: Vec<usize> = verts.iter().map(|v| v.0 as usize).collect();
            let n = vi.len();
            if face_ids.contains(&fid.0) && n >= 3 {
                let pts: Vec<Vec3> = vi.iter().map(|&k| positions[k]).collect();
                let mut center = face_center_median_weighted(&pts);
                if offset != 0.0 {
                    center += newell_normal(&pts).normalize_or_zero() * offset;
                }
                let cidx = positions.len();
                positions.push(center);
                // Triangle fan: one tri per boundary edge, KEEPING the edge direction vi[i] -> vi[i+1] so
                // the fan winds like the original face (outward normal) and the boundary edge still twins
                // its neighbour across the seam.
                for i in 0..n {
                    new_face_lists.push(vec![vi[i], vi[(i + 1) % n], cidx]);
                    new_face_mats.push(mat);
                }
            } else {
                new_face_lists.push(vi);
                new_face_mats.push(mat);
            }
            new_face_parent.resize(new_face_lists.len(), Some(fid));
        }
        // ID-STABLE (POLY-STALE-IDS): soup index = vertex id, centers appended.
        let pos_origin = self.id_indexed_origin(positions.len());
        *self = Self::from_polygons_keeping_ids(
            self,
            &positions,
            &pos_origin,
            &new_face_lists,
            &new_face_parent,
            &new_face_mats,
        );
        #[cfg(debug_assertions)]
        self.validate_mesh("poke_faces");
    }

    /// The shared face of two vertices that `split_face_between` could split: a LIVE face whose ring
    /// contains BOTH `v_a` and `v_b` and in which they are NON-ADJACENT (adjacent = already joined by an
    /// edge, which Blender's vert-connect no-ops). Returns `None` if they share no such face.
    fn find_splittable_shared_face(&self, v_a: VertexId, v_b: VertexId) -> Option<FaceId> {
        // ONE ring buffer for the whole scan: `face_vertices` allocated a `Vec` per face, and
        // `connect_path_order` calls this O(k^2) times over a k-vertex selection — so a 12-vertex
        // Connect on a 4096-face cage did ~500k throwaway allocations (measured 104 ms).
        let mut ring: Vec<VertexId> = Vec::new();
        for i in 0..self.faces.len() as u32 {
            let fid = FaceId(i);
            if !self.is_face_live(fid) { continue; }
            self.face_vertices_into(fid, &mut ring);
            let n = ring.len();
            if n < 4 { continue; } // a triangle has no non-adjacent vertex pair.
            let ia = ring.iter().position(|&v| v == v_a);
            let ib = ring.iter().position(|&v| v == v_b);
            if let (Some(ia), Some(ib)) = (ia, ib) {
                // non-adjacent in the ring → split_face_between will accept it.
                if (ia + 1) % n != ib && (ib + 1) % n != ia {
                    return Some(fid);
                }
            }
        }
        None
    }

    /// CONNECT VERTEX PATH (Blender `bpy.ops.mesh.vert_connect_path`, key `J`): walk the ORDERED vertex
    /// list and connect each consecutive pair `verts[i] -> verts[i+1]` with a new edge that splits their
    /// shared face (via the ID-preserving `split_face_between`). A pair sharing no splittable face is
    /// SKIPPED (Blender no-ops a pair already joined by an edge, and cannot connect a pair with no common
    /// face). The shared face is re-found on the CURRENT mesh before each split, because a split re-keys
    /// face ids. Returns the number of edges actually created — `0` means nothing connected (the caller
    /// refuses cleanly). Verified vs `connect_ref.json`: quad diag → 2 tris, hex 0-3 → 2 quads, the
    /// 3-vert path 0-4-2 across two faces → 4 tris.
    pub fn connect_vertices(&mut self, verts: &[VertexId]) -> usize {
        let mut made = 0usize;
        for pair in verts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a == b { continue; }
            if let Some(f) = self.find_splittable_shared_face(a, b) {
                if self.split_face_between(f, a, b).is_some() {
                    made += 1;
                }
            }
        }
        made
    }

    /// Order an UNORDERED selected-vertex set into a connect PATH by chaining vertices that share a
    /// splittable face (the app has no selection-order history — `PolySelection` is a `HashSet` — so a
    /// valid path is RECONSTRUCTED from shared-face adjacency).
    ///
    /// For a simple chain (each interior vertex shares a face with exactly its two path neighbours, the
    /// endpoints with one) this recovers the exact path. The key is to START at an ENDPOINT (degree-1 in
    /// the shared-face graph): starting at an interior HUB instead would walk to one leaf and STRAND the
    /// other. `start` (the active vertex) is honoured only if it is itself an endpoint; otherwise the
    /// lowest-id endpoint is chosen, falling back to `start` / lowest id when no endpoint exists (a cycle
    /// or an isolated set). Leftovers with no chain are appended in id order — `connect_vertices` simply
    /// skips any consecutive pair that shares no face. A ≤2-vertex selection is returned as-is (the SIMPLE
    /// case is order-independent).
    pub fn connect_path_order(&self, sel: &std::collections::HashSet<u32>, start: Option<u32>) -> Vec<VertexId> {
        let ids: Vec<u32> = {
            let mut v: Vec<u32> = sel.iter().copied().filter(|&v| (v as usize) < self.vertices.len()).collect();
            v.sort_unstable();
            v
        };
        if ids.len() <= 2 { return ids.into_iter().map(VertexId).collect(); }

        // Shared-face adjacency among the selected vertices (degree = # of selected verts this one can be
        // connected to). Small selections, so an O(k²) build is fine.
        let deg = |v: u32| ids.iter().filter(|&&o| o != v
            && self.find_splittable_shared_face(VertexId(v), VertexId(o)).is_some()).count();

        // Start at an endpoint (degree 1): honour `start` if it is one, else the lowest-id endpoint.
        let start_v = start
            .filter(|&s| ids.contains(&s) && deg(s) == 1)
            .or_else(|| ids.iter().copied().find(|&v| deg(v) == 1))
            .or(start.filter(|s| ids.contains(s)))
            .unwrap_or(ids[0]);

        let mut remaining: std::collections::BTreeSet<u32> = ids.into_iter().collect();
        let mut path: Vec<VertexId> = Vec::with_capacity(remaining.len());
        let mut cur = start_v;
        remaining.remove(&cur);
        path.push(VertexId(cur));
        while !remaining.is_empty() {
            let next = remaining.iter().copied()
                .find(|&v| self.find_splittable_shared_face(VertexId(cur), VertexId(v)).is_some());
            match next {
                Some(v) => { remaining.remove(&v); path.push(VertexId(v)); cur = v; }
                None => break,
            }
        }
        for v in remaining { path.push(VertexId(v)); }
        path
    }

    /// TO SPHERE (Blender `bpy.ops.transform.tosphere`): push each selected vertex toward a sphere by
    /// `factor` in `[0,1]`. Center = MEAN of the selected vertex positions; radius = MEAN distance of the
    /// selected vertices from that center; each vertex `new = lerp(old, center + normalize(old-center) *
    /// radius, factor)`. A vertex AT the center stays put. Pure position edit — NO topology change, so
    /// all ids are preserved. Returns `false` on empty / all-out-of-range selection (caller refuses).
    /// Verified vs `tosphere_ref.json`: a flat 3x3 grid at factor 1.0 lands corner `(-1,0,-1)` on
    /// `(-0.758714,0,-0.758714)` and edge `(0,0,-1)` on `(0,0,-1.072984)`; factor 0.5 is the exact
    /// midpoint; a cube's already-equidistant top face is unchanged at factor 1.0.
    pub fn to_sphere(&mut self, verts: &std::collections::HashSet<u32>, factor: f32) -> bool {
        let ids: Vec<u32> = verts.iter().copied()
            .filter(|&v| (v as usize) < self.vertices.len())
            .collect();
        if ids.is_empty() { return false; }
        let center = ids.iter()
            .map(|&v| self.vertices[v as usize].position)
            .fold(Vec3::ZERO, |a, b| a + b) / ids.len() as f32;
        let radius = ids.iter()
            .map(|&v| (self.vertices[v as usize].position - center).length())
            .sum::<f32>() / ids.len() as f32;
        for &v in &ids {
            let p = self.vertices[v as usize].position;
            let d = p - center;
            let len = d.length();
            if len < 1e-9 { continue; } // a vertex at the center stays.
            let target = center + d / len * radius;
            self.vertices[v as usize].position = p.lerp(target, factor);
        }
        true
    }

}
