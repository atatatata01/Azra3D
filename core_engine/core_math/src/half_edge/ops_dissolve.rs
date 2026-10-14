// Dissolve / merge family (split from half_edge.rs — pure relocation).
//
// PERF NOTE (2026-08-09) — free-list membership.
// Every liveness test in this file used to be an open-coded `free_*.contains(&id)`, a LINEAR
// scan of the free-list `Vec`. Sweeps shaped `for i in 0..half_edges.len() { if free.. }` were
// therefore O(elements x free_len) — quadratic on a heavily edited mesh, which is exactly the
// state dissolve/merge operate on. They now route through the canonical
// [`HalfEdgeMesh::is_edge_live`] / [`HalfEdgeMesh::is_face_live`] / [`HalfEdgeMesh::is_vert_live`]
// predicates, which are O(1) via the `FreeBitset` mirror in `alloc.rs`.
//
// The swap is EXACT, not merely equivalent. `is_edge_live(h)` is
// `h != NO_EDGE && in_range(h) && !free_edges.contains(&h.0)`:
//   * where the id is a loop index over `0..len`, the two extra conjuncts are true by
//     construction, so the predicate reduces to the old `!contains`;
//   * where the old code already spelled `h == NO_EDGE || oob || contains` as an OR-chain
//     (the disk-pointer staleness checks), `!is_edge_live(h)` IS that chain, same terms in the
//     same short-circuit order.
// Face sites follow the same argument with `is_face_live(f)` = `in_range(f) && !free.contains`.
// Where the file deliberately treats "on the free list" and "half_edge == NO_EDGE" as TWO
// DIFFERENT conditions (`collapse_self_loops`, `dissolve_faces_region_gated`), only the
// free-list half is converted and the OR is preserved verbatim.

use super::*;

impl HalfEdgeMesh {
    /// Merge two vertices into one (at their midpoint).
    /// Returns the surviving vertex.
    pub fn merge_vertices(&mut self, a: VertexId, b: VertexId) -> VertexId {
        let mid = (self.vert(a).position + self.vert(b).position) * 0.5;
        self.vert_mut(a).position = mid;

        // Redirect all half-edges pointing to b to point to a
        for i in 0..self.half_edges.len() {
            if !self.is_edge_live(HalfEdgeId(i as u32)) { continue; }
            if self.half_edges[i].vertex == b {
                self.half_edges[i].vertex = a;
            }
        }

        // Update vertex b's outgoing edges to come from a
        self.vert_mut(b).half_edge = NO_EDGE;
        self.free_vert(b);

        self.recompute_normals();
        a
    }

    /// Weld a SET of vertices to a single survivor (verts[0]) placed at `target`, deterministically
    /// (audit #13 — chaining `merge_vertices` midpoints landed the survivor at an iteration-order-dependent
    /// running average, matching no Blender mode). Blender "Merge at Center" → `target` = the centroid of
    /// the selection; "At First" → verts[0]'s position; etc. Returns the survivor.
    /// Adjacent selected verts (sharing an edge) COLLAPSE that edge cleanly — the zero-length self-loop
    /// half-edges are spliced out and any face reduced below 3 sides is dropped (Blender edge-collapse
    /// on Merge; audit #8). The result is manifold, so the app gate accepts it.
    pub fn merge_vertices_to(&mut self, verts: &[VertexId], target: Vec3) -> Option<VertexId> {
        self.merge_vertices_to_ex(verts, target, true)
    }

    /// [`Self::merge_vertices_to`] with the trailing full-mesh `recompute_normals` optional.
    /// The dyntopo refine kernel (`ops_dyntopo.rs`) collapses MANY edges per call and does ONE
    /// `recompute_normals_region` pass over its touched set at the end — paying a full-mesh
    /// normals pass per collapse there was O(mesh × collapses). Every other caller keeps
    /// `recompute_normals: true` (the historical contract).
    pub fn merge_vertices_to_ex(
        &mut self,
        verts: &[VertexId],
        target: Vec3,
        recompute_normals: bool,
    ) -> Option<VertexId> {
        if verts.is_empty() { return None; }
        let survivor = verts[0];
        for &b in &verts[1..] {
            if b == survivor || !self.is_vert_live(b) { continue; }
            for i in 0..self.half_edges.len() {
                if !self.is_edge_live(HalfEdgeId(i as u32)) { continue; }
                if self.half_edges[i].vertex == b { self.half_edges[i].vertex = survivor; }
            }
            self.vert_mut(b).half_edge = NO_EDGE;
            self.free_vert(b);
        }
        self.vert_mut(survivor).position = target;
        self.collapse_self_loops();
        // Repair the survivor's disk pointer if it referenced a collapsed edge.
        let h = self.vert(survivor).half_edge;
        let stale = !self.is_edge_live(h) || self.he_source(h) != survivor;
        if stale {
            let mut repl = NO_EDGE;
            for i in 0..self.half_edges.len() as u32 {
                if !self.is_edge_live(HalfEdgeId(i)) { continue; }
                if self.he_source(HalfEdgeId(i)) == survivor { repl = HalfEdgeId(i); break; }
            }
            self.vert_mut(survivor).half_edge = repl;
        }
        if recompute_normals {
            self.recompute_normals();
        }
        Some(survivor)
    }

    /// **Merge by Distance** (Blender "Merge by Distance" / Remove Doubles) for the whole mesh.
    ///
    /// Clusters live vertices whose positions are within `threshold` (world/local units) of each
    /// other, then welds each cluster to its **average** position (centroid). Returns the number
    /// of vertices removed. Threshold ≤ 0 is a no-op.
    ///
    /// Implementation rebuilds via face soup + `from_polygons` so self-loops and degenerate rings
    /// are handled consistently with the boolean cleanup path (not iterative in-place merges that
    /// can leave open rims).
    pub fn merge_by_distance(&mut self, threshold: f32) -> usize {
        self.merge_by_distance_ex(threshold, &[])
    }

    /// [`Self::merge_by_distance`] that REFUSES any weld which would collapse both endpoints of a
    /// protected edge into a single vertex.
    ///
    /// `protected` is a list of undirected endpoint-POSITION pairs (positions, not ids — Mesh
    /// Cleanup chains several rebuilds, and its coplanar merge still renumbers), the same currency
    /// `mesh_boolean::CleanupOptions::protected_edges` speaks. Mesh Cleanup fills it from the
    /// sidecar's authored layers (Smart Select `op_created`, Mark Sharp, UV seam, crease), so a
    /// Merge-by-Distance pass inside Cleanup can never silently erase an authored edge.
    ///
    /// The guard is per-cluster, not global: unrelated doubles elsewhere still weld. Only the
    /// specific union that would fuse a protected edge's two ends is declined.
    pub fn merge_by_distance_ex(&mut self, threshold: f32, protected: &[(Vec3, Vec3)]) -> usize {
        let thr = threshold.max(0.0);
        if thr <= 0.0 || self.vertex_count() < 2 {
            return 0;
        }
        let before = self.vertex_count();
        let welded = weld_half_edge_by_distance(self, thr, protected);
        if welded.vertex_count() < before && welded.validate().is_ok() {
            *self = welded;
            before.saturating_sub(self.vertex_count())
        } else {
            0
        }
    }

    /// Splice out every zero-length self-loop half-edge (`src == dst`, e.g. left by an adjacent-vertex
    /// merge) from its face loop, freeing the edge; a face reduced below 3 sides is deleted. Keeps the
    /// mesh manifold after an edge collapse.
    fn collapse_self_loops(&mut self) {
        use std::collections::HashSet;
        let loops: Vec<HalfEdgeId> = (0..self.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| self.is_edge_live(h))
            .filter(|&h| self.he(h).face != NO_FACE && self.he_source(h) == self.he(h).vertex)
            .collect();
        let mut touched: HashSet<u32> = HashSet::new();
        for h in loops {
            if !self.is_edge_live(h) || self.he(h).face == NO_FACE { continue; }
            let f = self.he(h).face;
            let p = self.he(h).prev;
            let n = self.he(h).next;
            // Splice h out of its face cycle.
            self.he_mut(p).next = n;
            self.he_mut(n).prev = p;
            if f != NO_FACE && self.face(f).half_edge == h { self.face_mut(f).half_edge = n; }
            // Free h (its twin is the other collapsed half-edge — detach so it frees independently).
            let tw = self.he(h).twin;
            if tw != NO_TWIN { self.he_mut(tw).twin = NO_TWIN; }
            self.free_edge(h);
            if f != NO_FACE { touched.insert(f.0); }
        }
        // Any face reduced below a triangle is degenerate — remove it.
        for fi in touched {
            let f = FaceId(fi);
            // TWO DIFFERENT conditions, deliberately kept as an OR (see the module header): a
            // freed slot, and a live slot whose ring was emptied. Only the free-list half moved
            // to the O(1) predicate. `fi` came from a live half-edge's `face` that this fn
            // already indexed through `self.face(f)` above, so it is in range and
            // `!is_face_live(f)` is exactly the old `free_faces.contains(&fi)`.
            if !self.is_face_live(f) || self.face(f).half_edge == NO_EDGE { continue; }
            if self.face_half_edges(f).len() < 3 { self.remove_degenerate_face(f); }
        }
    }

    /// Remove a face that an edge collapse reduced below a triangle, KEEPING THE SURFACE CLOSED.
    ///
    /// A 2-gon left by a collapse is a zero-area wedge: its two half-edges `a→c` and `c→a` are the
    /// same physical edge seen twice. Its outer neighbours (the twins) are therefore each other's
    /// true twin once the wedge is gone. `delete_face` cannot be used here — it clears every
    /// neighbour's `twin` to `NO_TWIN`, which turns them into boundary edges and rips a hole in the
    /// mesh. That is exactly why `collapse_epsilon_debris` / merge-by-distance could never be
    /// accepted before (mesh_boolean_manifold.rs: "cross-rim welds break closedness because
    /// merge_vertices_to cannot re-twin the doubled edges"). Re-twinning is the missing step.
    ///
    /// Falls back to `delete_face` when the wedge is not re-twinnable (a neighbour already on the
    /// boundary, or not a clean 2-gon) — fail closed, the caller's gates then reject the result.
    fn remove_degenerate_face(&mut self, f: FaceId) {
        let hs = self.face_half_edges(f);
        if hs.len() != 2 {
            self.delete_face(f);
            return;
        }
        let (h1, h2) = (hs[0], hs[1]);
        let (t1, t2) = (self.he(h1).twin, self.he(h2).twin);
        let re_twinnable = t1 != NO_TWIN
            && t2 != NO_TWIN
            && t1 != h2
            && t2 != h1
            && self.is_edge_live(t1)
            && self.is_edge_live(t2)
            // The survivors must still be an opposite-direction pair, or twinning them
            // would violate the half-edge invariant.
            && self.he_source(t1) == self.he(t2).vertex
            && self.he(t1).vertex == self.he_source(t2);
        if !re_twinnable {
            self.delete_face(f);
            return;
        }
        let verts = self.face_vertices(f);
        self.he_mut(t1).twin = t2;
        self.he_mut(t2).twin = t1;
        // Detach before freeing so `free_edge` cannot disturb the pairing just made.
        self.he_mut(h1).twin = NO_TWIN;
        self.he_mut(h2).twin = NO_TWIN;
        self.free_edge(h1);
        self.free_edge(h2);
        self.free_face(f);
        // Repoint any corner vertex whose disk pointer named one of the freed half-edges
        // (same repair delete_face performs — a stale pointer mis-pairs the next create_face).
        for &v in &verts {
            if !self.is_vert_live(v) { continue; }
            let h = self.vert(v).half_edge;
            let stale = h == NO_EDGE
                || h.0 as usize >= self.half_edges.len()
                || !self.is_edge_live(h)
                || self.he_source(h) != v;
            if stale {
                let mut repl = NO_EDGE;
                for i in 0..self.half_edges.len() as u32 {
                    let g = HalfEdgeId(i);
                    if !self.is_edge_live(g) { continue; }
                    if self.he_source(g) == v { repl = g; break; }
                }
                self.vert_mut(v).half_edge = repl;
            }
        }
    }


    /// Dissolve an edge: merge the two faces on either side into one face.
    /// Removes the edge and its twin, merges faces. Returns the surviving face.
    pub fn dissolve_edge(&mut self, he_id: HalfEdgeId) -> Option<FaceId> {
        let he = self.he(he_id);
        if he.twin == NO_TWIN { return None; }
        let twin_id = he.twin;
        let face_a = he.face;
        let face_b = self.he(twin_id).face;
        if face_a == NO_FACE || face_b == NO_FACE { return None; }
        // SLIT EDGE: he and its twin belong to the SAME face (the face touches itself along this edge).
        // There are not "two faces on either side" to merge — the documented precondition above is
        // violated exactly like the two guards before this one, so refuse rather than mangle the mesh.
        //
        // DO NOT REMOVE — this is not a redundant check. Without it the body below corrupts the mesh:
        // it sets face_a's rep to he_next, then (face_b BEING face_a) immediately nulls that same rep
        // and pushes the slot onto `free_faces` — freeing a face that a whole live ring of half-edges
        // still names. The next create_face() reuses that FaceId => two logical faces, one id, silently.
        // `validate()` is blind to this (it only checks next/prev/twin round-trips), so nothing
        // downstream catches it. The relabel loop is also unsound here: the splice that precedes it cuts
        // the single ring in two, so the loop never meets its `cur == he_next` terminator and exits only
        // via the `safety < 10000` cap.
        //
        // Blender-confirmed refusal, not a split: bmesh.ops.dissolve_edges leaves this configuration
        // bit-identical under all four use_verts × use_face_split combinations (4.5 and 5.0), and the
        // Ctrl+X UI path returns FINISHED while changing nothing. Never split, delete, or "repair" here.
        //
        // Refusal (not an assert): a slit is a reachable input state — dissolve_edge itself produces one
        // when it merges two faces that already share more than one edge — not an internal invariant
        // violation. Callers honor None as a clean refusal (mesh_ops/mod.rs:409 -> OpError::InvalidParams,
        // mesh_ops/local_rebuild_ops.rs:194-197). This must stay ABOVE the snapshot: an early return is
        // the only refusal that leaves the mesh byte-identical.
        if face_a == face_b { return None; }

        // Snapshot connectivity before we start modifying
        let he_prev = he.prev;
        let he_next = he.next;
        let he_vertex = he.vertex; // dst vertex of he
        let twin_prev = self.he(twin_id).prev;
        let twin_next = self.he(twin_id).next;
        // src vertex of he = dst vertex of he.prev
        let he_src_vertex = self.he(he_prev).vertex;

        // Connect he's prev→twin's next, twin's prev→he's next
        self.half_edges[he_prev.0 as usize].next = twin_next;
        self.half_edges[twin_next.0 as usize].prev = he_prev;
        self.half_edges[twin_prev.0 as usize].next = he_next;
        self.half_edges[he_next.0 as usize].prev = twin_prev;

        // All half-edges of face_b now belong to face_a
        let mut cur = twin_next;
        let mut safety = 0;
        while cur != he_next && safety < 10000 {
            self.half_edges[cur.0 as usize].face = face_a;
            cur = self.he(cur).next;
            safety += 1;
        }

        // Update face_a's half_edge pointer (in case it pointed to he_id)
        self.faces[face_a.0 as usize].half_edge = he_next;

        // Mark face_b as free (face_a != face_b is guaranteed by the slit refusal above).
        self.free_face(face_b);

        // Mark the dissolved half-edges as free (canonical dead state via the chokepoint).
        self.free_edge(he_id);
        self.free_edge(twin_id);

        // Update vertex half-edge refs if they pointed to dissolved edges
        if self.vert(he_src_vertex).half_edge == he_id {
            self.vertices[he_src_vertex.0 as usize].half_edge = twin_next;
        }
        if self.vert(he_vertex).half_edge == twin_id {
            self.vertices[he_vertex.0 as usize].half_edge = he_next;
        }

        self.recompute_normals();
        Some(face_a)
    }

    /// Dissolve a face: remove the face, turning its interior into a hole.
    /// The half-edges become boundary edges (face = NO_FACE).
    pub fn dissolve_face(&mut self, fid: FaceId) {
        if self.face(fid).half_edge == NO_EDGE { return; }
        let hes = self.face_half_edges(fid);
        for &he in &hes {
            self.half_edges[he.0 as usize].face = NO_FACE;
        }
        self.free_face(fid);
        self.recompute_normals();
    }

    /// Blender "Dissolve Faces": merge each connected component of the selected faces into ONE n-gon
    /// (audit #10 — the old per-face `dissolve_face` hole-punched and left non-manifold wire edges on a
    /// multi-face selection). Returns the merged face(s). Traces each component's outer boundary loop,
    /// frees the interior, and rebuilds one face (whose edges auto-twin the outside neighbours).
    pub fn dissolve_faces_region(&mut self, faces: &std::collections::HashSet<FaceId>) -> Vec<FaceId> {
        self.dissolve_faces_region_gated(faces, None)
    }

    /// Region merge with an OPTIONAL edge gate. When `only_cross` is `Some(set)`, two faces join the same
    /// component ONLY across a shared edge whose half-edge (either direction) is in `set`; a shared edge
    /// outside the set does NOT connect them. When `None`, any shared edge connects (Dissolve Faces /
    /// Dissolve Vertex semantics).
    ///
    /// This is the over-merge guard for `dissolve_edges_region`: Blender `bmo_dissolve_edges` removes ONLY
    /// the selected edges, so a selection that SKIPS an intervening edge must stay two independent regions
    /// (the flat-strip B01+B23-skip-B12 case). The all-shared-edge BFS silently merged across the
    /// unselected middle edge; gating on the selected-edge subgraph keeps the components apart. Interior
    /// edges of each resulting component (which for a genuine merge are exactly the selected edges plus any
    /// edge that topology forces interior-to-one-face) are still removed by `dissolve_one_region` —
    /// matching Blender, which also drops the vertical edges of a collapsed cube cap.
    fn dissolve_faces_region_gated(
        &mut self,
        faces: &std::collections::HashSet<FaceId>,
        only_cross: Option<&std::collections::HashSet<HalfEdgeId>>,
    ) -> Vec<FaceId> {
        use std::collections::HashSet;
        // `is_face_live` IS `(f.0 as usize) < self.faces.len() && !free_faces.contains(&f.0)` —
        // same two conjuncts, same short-circuit order — so the `half_edge != NO_EDGE` half of
        // the OR-of-conditions distinction is preserved untouched (module header).
        let mut remaining: HashSet<FaceId> = faces.iter().copied()
            .filter(|&f| self.is_face_live(f) && self.face(f).half_edge != NO_EDGE)
            .collect();
        let mut merged = Vec::new();
        while let Some(&seed) = remaining.iter().next() {
            // BFS the edge-connected component within the selection.
            let mut comp: HashSet<FaceId> = HashSet::new();
            let mut stack = vec![seed];
            while let Some(f) = stack.pop() {
                if !comp.insert(f) { continue; }
                for he in self.face_half_edges(f) {
                    let tw = self.he(he).twin;
                    if tw != NO_TWIN {
                        // Gate: cross an edge only if it (either half-edge) is in the allowed set.
                        if let Some(allow) = only_cross {
                            if !allow.contains(&he) && !allow.contains(&tw) { continue; }
                        }
                        let nf = self.he(tw).face;
                        if nf != NO_FACE && remaining.contains(&nf) && !comp.contains(&nf) {
                            stack.push(nf);
                        }
                    }
                }
            }
            for f in &comp { remaining.remove(f); }
            if let Some(m) = self.dissolve_one_region(&comp) { merged.push(m); }
        }
        merged
    }

    /// Dissolve ONE edge-connected face region into a single n-gon. Returns None (no-op) if the region's
    /// boundary is not a simple loop (non-disk: a pinch vertex / annulus) — the caller keeps the mesh.
    fn dissolve_one_region(&mut self, comp: &std::collections::HashSet<FaceId>) -> Option<FaceId> {
        use std::collections::HashMap;
        // 1. Boundary half-edges: in-region face, twin's face NOT in region (or no twin).
        let mut boundary: Vec<HalfEdgeId> = Vec::new();
        for &f in comp {
            if self.face(f).half_edge == NO_EDGE { continue; }
            for he in self.face_half_edges(f) {
                let tw = self.he(he).twin;
                let outside = tw == NO_TWIN
                    || { let nf = self.he(tw).face; nf == NO_FACE || !comp.contains(&nf) };
                if outside { boundary.push(he); }
            }
        }
        if boundary.len() < 3 { return None; }
        // 2. Chain into a single loop by source vertex (one out-edge per boundary vertex = a disk).
        let mut by_src: HashMap<u32, HalfEdgeId> = HashMap::new();
        for &he in &boundary { by_src.insert(self.he_source(he).0, he); }
        if by_src.len() != boundary.len() { return None; }
        let start = boundary[0];
        let mut ring: Vec<VertexId> = Vec::new();
        let mut cur = start;
        loop {
            let dst = self.he(cur).vertex;
            ring.push(dst);
            match by_src.get(&dst.0) {
                Some(&nxt) => { if nxt == start { break; } cur = nxt; }
                None => return None,
            }
            if ring.len() > boundary.len() { return None; }
        }
        if ring.len() != boundary.len() { return None; }
        // Region verts that will NOT be on the merged boundary ring = interior verts to free after merge
        // (Blender removes the isolated interior vertex of a dissolved region — verification #6).
        let ring_set: std::collections::HashSet<u32> = ring.iter().map(|v| v.0).collect();
        let mut region_verts: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &f in comp {
            if self.face(f).half_edge == NO_EDGE { continue; }
            for v in self.face_vertices(f) { region_verts.insert(v.0); }
        }
        // MATERIALS: the merged n-gon replaces the region's faces, and `create_face` allocates with
        // `material_id: 0` — so without this the whole dissolved region drops to the default
        // material and the user's assignment disappears (owner: "Ctrl+X removes material").
        // Blender's `BM_faces_join` takes the merged face's attributes from an EXAMPLE face; the
        // deterministic, id-stable choice here is the lowest live FaceId in the region (`comp` is a
        // HashSet, so iteration order is NOT deterministic — min is).
        let region_material = comp
            .iter()
            .filter(|&&f| self.face(f).half_edge != NO_EDGE)
            .min_by_key(|f| f.0)
            .map(|&f| self.face(f).material_id)
            .unwrap_or(0);
        // 3. Free the region faces (delete_face repairs surviving pointers), then build the merged n-gon.
        let comp_faces: Vec<FaceId> = comp.iter().copied().collect();
        for f in comp_faces {
            if self.face(f).half_edge != NO_EDGE { self.delete_face(f); }
        }
        let merged = self.create_face_with_material(&ring, region_material);
        // Free interior verts (not on the ring) that became loose — no live face uses them anymore.
        for vid in region_verts {
            if ring_set.contains(&vid) || !self.is_vert_live(VertexId(vid)) { continue; }
            let loose = (0..self.faces.len() as u32).filter(|&fi| self.is_face_live(FaceId(fi)))
                .all(|fi| !self.face_vertices(FaceId(fi)).iter().any(|x| x.0 == vid));
            if loose {
                self.vert_mut(VertexId(vid)).half_edge = NO_EDGE;
                self.free_vert(VertexId(vid));
            }
        }
        merged
    }

    /// Blender "Dissolve Vertices": remove `v` and merge its fan of incident faces into ONE n-gon
    /// (distinct from `delete_vertex`, which removes the faces and leaves a hole). Reuses the region
    /// merge — the fan around `v` is exactly one face region whose interior vertex is `v`. Returns the
    /// merged face, or None for a boundary / <2-face vertex (nothing to merge).
    pub fn dissolve_vertex(&mut self, v: VertexId) -> Option<FaceId> {
        let incident: std::collections::HashSet<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .filter(|&f| self.face_vertices(f).contains(&v))
            .collect();
        if incident.len() < 2 { return None; }
        let merged = self.dissolve_faces_region(&incident);
        // v is interior to the merged region → now loose. Free it if no live face still uses it.
        let still_used = (0..self.faces.len() as u32)
            .filter(|&fi| self.is_face_live(FaceId(fi)))
            .any(|fi| self.face_vertices(FaceId(fi)).contains(&v));
        if !still_used {
            self.vert_mut(v).half_edge = NO_EDGE;
            self.free_vert(v);
        }
        merged.into_iter().next()
    }

    /// Blender `bmesh_kernel_join_edge_kill_vert` (the operator-level `use_verts` cleanup AZRA lacked):
    /// collapse an INTERIOR VALENCE-2 vertex `v` by removing it and one of its two edges, merging the two
    /// collinear edges `a-v` and `v-b` into a single edge `a-b`. Both adjacent faces are KEPT — each
    /// simply drops `v` from its loop (`len -= 1`). This is NOT `dissolve_vertex` (which MERGES the fan of
    /// incident faces into one n-gon and reduces the face count); here the face count is UNCHANGED. It is
    /// exactly what turns single-edge dissolve from 8v/11e/5f into Blender's 6v/9e/5f (sweep the two
    /// valence-2 endpoints).
    ///
    /// ID-preserving (POLY plan: prefer relink over rebuild): keeps `he_av` (a→v, retargeted a→b) and
    /// `he_bv` (b→v, retargeted b→a) as the surviving twinned pair; frees `he_vb` and `he_va` through the
    /// `free_edge` chokepoint and marks `v` loose (the `dissolve_vertex` loose-free pattern).
    ///
    /// Returns `true` if it collapsed, `false` (mesh byte-untouched) if `v` is not a clean interior
    /// valence-2 vertex — a boundary valence-2 vertex (an edge with a missing twin) or any other valence
    /// is left in place for the caller to keep. Refusing (not asserting) is correct: an open-mesh
    /// selection can legitimately present a boundary valence-2 endpoint; leaving it is a VALID mesh, not a
    /// corruption. The interior branch is the only one the closed-mesh oracle cases exercise.
    pub fn join_edge_kill_vert(&mut self, v: VertexId) -> bool {
        if !self.is_vert_live(v) { return false; }
        let fan = self.vertex_fan(v); // outgoing half-edges v→x
        if fan.len() != 2 { return false; }
        let (he_va, he_vb) = (fan[0], fan[1]); // v→a, v→b
        let he_av = self.he(he_va).twin;       // a→v
        let he_bv = self.he(he_vb).twin;       // b→v
        // Interior-only clean configuration: both edges twinned, both adjacent half-edges bear a face.
        if he_av == NO_TWIN || he_bv == NO_TWIN { return false; }
        let f1 = self.he(he_av).face; // the face carrying ...a→v→b...
        let f2 = self.he(he_bv).face; // the face carrying ...b→v→a...
        if f1 == NO_FACE || f2 == NO_FACE { return false; }
        // Structural preconditions of a valence-2 interior vertex (study PART 1): the two adjacent
        // half-edges continue THROUGH v into the OTHER edge. If not, the disk is malformed — refuse.
        if self.he(he_av).next != he_vb || self.he(he_bv).next != he_va { return false; }
        let a = self.he(he_va).vertex; // v→a dst
        let b = self.he(he_vb).vertex; // v→b dst
        if a == b { return false; }    // would create a 2-gon / self-loop — refuse
        let vb_next = self.he(he_vb).next; // in f1, the half-edge after v→b
        let va_next = self.he(he_va).next; // in f2, the half-edge after v→a
        // F1: a→v→b… ==> a→b… (he_av absorbs v→b's continuation)
        self.he_mut(he_av).vertex = b;
        self.he_mut(he_av).next = vb_next;
        self.he_mut(vb_next).prev = he_av;
        // F2: b→v→a… ==> b→a… (he_bv absorbs v→a's continuation)
        self.he_mut(he_bv).vertex = a;
        self.he_mut(he_bv).next = va_next;
        self.he_mut(va_next).prev = he_bv;
        // The surviving a→b and b→a are each other's twins now.
        self.he_mut(he_av).twin = he_bv;
        self.he_mut(he_bv).twin = he_av;
        // Face reps that named a killed half-edge move to the survivor in the same face.
        if self.face(f1).half_edge == he_vb { self.face_mut(f1).half_edge = he_av; }
        if self.face(f2).half_edge == he_va { self.face_mut(f2).half_edge = he_bv; }
        // Free the two killed half-edges + mark v loose (canonical dead state via the chokepoint).
        self.free_edge(he_vb);
        self.free_edge(he_va);
        self.vert_mut(v).half_edge = NO_EDGE;
        self.free_vert(v);
        // Repair a's/b's disk pointer if it named a freed slot (e.g. it pointed at he_va/he_vb).
        for x in [a, b] {
            let h = self.vert(x).half_edge;
            let stale = !self.is_edge_live(h) || self.he_source(h) != x;
            if stale {
                let mut repl = NO_EDGE;
                for i in 0..self.half_edges.len() as u32 {
                    if !self.is_edge_live(HalfEdgeId(i)) { continue; }
                    if self.he_source(HalfEdgeId(i)) == x { repl = HalfEdgeId(i); break; }
                }
                self.vert_mut(x).half_edge = repl;
            }
        }
        true
    }

    /// Dissolve a valence-2 vertex on the OPEN BOUNDARY: its two incident edges are BOTH boundary (no
    /// twin) and it belongs to exactly ONE face — a straight point on an open edge (`a→v→b`). Merge it to
    /// `a→b`, dropping `v` from the face ring. `join_edge_kill_vert` only handles INTERIOR valence-2 verts
    /// (it requires both edges twinned); Blender's `use_verts` dissolves boundary ones too, so the multi-
    /// edge valence sweep needs this for open-surface endpoints (the flat-strip B01/B23 endpoints, which
    /// `vertex_fan` even reports as valence-1 because the twin-walk stops at the boundary). Refuses unless
    /// the exact boundary-valence-2 configuration holds, and never shrinks a face below a triangle.
    fn dissolve_boundary_valence2_vert(&mut self, v: VertexId) -> bool {
        if !self.is_vert_live(v) { return false; }
        // Every LIVE half-edge touching v, split into outgoing (v→x) and incoming (x→v).
        let mut inc_out: Vec<HalfEdgeId> = Vec::new();
        let mut inc_in: Vec<HalfEdgeId> = Vec::new();
        for i in 0..self.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !self.is_edge_live(h) { continue; }
            if self.he_source(h) == v { inc_out.push(h); }
            if self.he(h).vertex == v { inc_in.push(h); }
        }
        // Exactly one outgoing + one incoming, both boundary, sharing one face, consecutive `a→v→b`.
        if inc_out.len() != 1 || inc_in.len() != 1 { return false; }
        let hout = inc_out[0]; // v→b
        let hin = inc_in[0];   // a→v
        if self.he(hout).twin != NO_TWIN || self.he(hin).twin != NO_TWIN { return false; }
        let f = self.he(hout).face;
        if f == NO_FACE || self.he(hin).face != f { return false; }
        if self.he(hin).next != hout { return false; } // a→v→b must be consecutive in f
        if self.face_half_edges(f).len() < 4 { return false; } // removing v must leave >=3 verts
        let a = self.he_source(hin);
        let b = self.he(hout).vertex;
        if a == b || a == v || b == v { return false; } // guard 2-gon / self-loop
        let hout_next = self.he(hout).next;
        // a→v (hin) absorbs v→b's continuation: becomes a→b, still a boundary half-edge.
        self.he_mut(hin).vertex = b;
        self.he_mut(hin).next = hout_next;
        self.he_mut(hout_next).prev = hin;
        if self.face(f).half_edge == hout { self.face_mut(f).half_edge = hin; }
        self.free_edge(hout);
        self.vert_mut(v).half_edge = NO_EDGE;
        self.free_vert(v);
        // Repair b's disk pointer if it named the freed slot.
        let bh = self.vert(b).half_edge;
        let stale = !self.is_edge_live(bh) || self.he_source(bh) != b;
        if stale {
            let mut repl = NO_EDGE;
            for i in 0..self.half_edges.len() as u32 {
                if !self.is_edge_live(HalfEdgeId(i)) { continue; }
                if self.he_source(HalfEdgeId(i)) == b { repl = HalfEdgeId(i); break; }
            }
            self.vert_mut(b).half_edge = repl;
        }
        true
    }

    /// Blender multi-edge "Dissolve Edges" (Ctrl+X, `use_verts=True`) — the REGION collapse, run as ONE
    /// atomic pass, matching bmesh `bmo_dissolve_edges_exec` BY IDENTITY. Replaces the old per-edge
    /// sequential batch (which refused on the first slit and so "did nothing" on any shared selection).
    ///
    ///   1. **Face-region merge**, gated on the SELECTED-edge subgraph (`dissolve_faces_region_gated` with
    ///      `Some(sel_set)`). Two faces join the same component ONLY across a SELECTED edge — an unselected
    ///      shared edge never links them, so a selection that skips an intervening edge (flat strip:
    ///      B01+B23, not B12) stays two independent regions instead of over-merging the whole strip into
    ///      one face. Each component collapses to one n-gon; its interior verts are freed there. (The plain
    ///      `dissolve_faces_region` all-shared-edge BFS is WRONG here — it silently merged across B12.)
    ///   2. **Duplicate-face cull** (`kill_duplicate_faces`). A component that wrapped a closed cup — the
    ///      4 top edges of a cube — merges to a face whose whole boundary coincides with an EXISTING face
    ///      (the opposite quad). Drop that duplicate so the cap collapses onto the opposite face (4-top →
    ///      a single quad, 4v/4e/1f) instead of stranding a doubled face that `is_manifold` would reject.
    ///   3. **Valence sweep** (`use_verts` / `join_edge_kill_vert`). Every SELECTED-edge endpoint that is
    ///      now an interior valence-2 vertex is collapsed — this is what makes single-edge give 6/9/5.
    ///      Genuinely loose endpoints (all edges gone) are freed. Iterated to a fixpoint (bounded by the
    ///      endpoint count).
    ///
    /// Returns `false` (mesh untouched) if the selection bounds no live face region — an honest refusal
    /// the app surfaces as a toast, never a silent no-op.
    pub fn dissolve_edges_region(&mut self, edges: &std::collections::HashSet<HalfEdgeId>) -> bool {
        use std::collections::HashSet;
        let sel: Vec<HalfEdgeId> = edges.iter().copied().filter(|&h| self.is_edge_live(h)).collect();
        if sel.is_empty() { return false; }
        // Endpoints to sweep after the merge (both ends of every selected edge), + the face set the
        // selection bounds. Captured as ids BEFORE any mutation (all-or-nothing renumbers nothing).
        let mut endpoints: HashSet<u32> = HashSet::new();
        let mut face_set: HashSet<FaceId> = HashSet::new();
        // The SELECTED-edge subgraph (both half-edges of each selected edge) — the gate the region merge
        // may cross. Faces joined only by an UNSELECTED edge stay independent (the flat-strip case).
        let mut sel_set: HashSet<HalfEdgeId> = HashSet::new();
        for &h in &sel {
            sel_set.insert(h);
            let tw = self.he(h).twin;
            if tw != NO_TWIN { sel_set.insert(tw); }
            endpoints.insert(self.he_source(h).0);
            endpoints.insert(self.he(h).vertex.0);
            let (f1, f2) = self.edge_faces(h);
            if f1 != NO_FACE && self.face(f1).half_edge != NO_EDGE { face_set.insert(f1); }
            if f2 != NO_FACE && self.face(f2).half_edge != NO_EDGE { face_set.insert(f2); }
        }
        if face_set.is_empty() { return false; }
        // (1) Region merge — one n-gon per SELECTED-edge-connected component (frees interior verts). The
        // gate is what stops the over-merge: a selection that skips an intervening edge (B01+B23, not B12)
        // stays two components, matching Blender bmo_dissolve_edges (removes only the selected edges).
        let merged = self.dissolve_faces_region_gated(&face_set, Some(&sel_set));
        if merged.is_empty() { return false; }
        // (2) Duplicate-face cull: a merged face whose EVERY edge twins to ONE single other face is
        // coincident with it (the cup-collapse) — remove the merged duplicate, leaving the survivor as an
        // open cap. A genuine merged n-gon twins to SEVERAL distinct faces and is kept.
        for &m in &merged {
            if self.face(m).half_edge == NO_EDGE { continue; }
            let hes = self.face_half_edges(m);
            let mut common: Option<FaceId> = None;
            let mut dup = true;
            for &he in &hes {
                let tw = self.he(he).twin;
                if tw == NO_TWIN { dup = false; break; }
                let nf = self.he(tw).face;
                if nf == NO_FACE || nf == m { dup = false; break; }
                match common {
                    None => common = Some(nf),
                    Some(c) => if c != nf { dup = false; break; },
                }
            }
            if dup && common.is_some() { self.delete_face(m); }
        }
        // (3) Valence sweep of the selected-edge endpoints — to fixpoint. Each primitive self-guards on
        // its exact precondition (so we try both rather than trust `vertex_valence`, which under-reports a
        // boundary vertex as valence-1 because the twin-walk stops at the open edge):
        //   • interior valence-2 → join_edge_kill_vert (what makes single-edge give 6/9/5);
        //   • open-boundary valence-2 → dissolve_boundary_valence2_vert (the flat-strip endpoints);
        //   • genuinely loose (no live half-edge touches it) → free.
        loop {
            let mut changed = false;
            for &vid in &endpoints {
                if !self.is_vert_live(VertexId(vid)) { continue; }
                let v = VertexId(vid);
                if self.join_edge_kill_vert(v) { changed = true; continue; }
                if self.dissolve_boundary_valence2_vert(v) { changed = true; continue; }
                let used = (0..self.half_edges.len() as u32).any(|i| {
                    let h = HalfEdgeId(i);
                    self.is_edge_live(h) && (self.he(h).vertex == v || self.he_source(h) == v)
                });
                if !used {
                    self.vert_mut(v).half_edge = NO_EDGE;
                    self.free_vert(VertexId(vid));
                    changed = true;
                }
            }
            if !changed { break; }
        }
        self.recompute_normals();
        true
    }

    /// SketchUp push/pull parity — merge extrude walls that landed EXACTLY coplanar with the
    /// pre-existing face across their base edge (owner 2026-08-03). Push = the wall covers a
    /// strip of the neighbor (stacked double plane, z-fight), pull = the wall extends the
    /// neighbor's plane; both merge into ONE face by dissolving the base edge, and
    /// [`Self::dissolve_edges_region`] collapses each connected stack to a single n-gon and
    /// sweeps the seam verts (old rim corners become valence-2 and are joined through).
    ///
    /// Guard: four probes per wall (cap-edge verts + strip midpoints) classify against the
    /// neighbor as strictly inside / ON its rim / strictly outside. A genuine strict-inside +
    /// strict-outside mix = the strip CROSSES the neighbor's boundary (overshoot / partial
    /// overlap) — merging would self-intersect, so that wall is left alone (fail closed).
    /// On-rim probes are NEUTRAL: a cap vertex landing exactly on the neighbor's boundary
    /// (the pushed region touching the solid's corner — the common case) must not veto the
    /// merge, else the wall is skipped and a stacked sliver + unwelded seam vertex remains.
    ///
    /// `caps` = the extrude's cap faces. Returns how many walls were merged.
    ///
    /// A hole-rim that was already knifed (`close wall.fl4`) makes the new top/bottom
    /// wall an n-gon sitting on TWO+ coplanar rim faces. Every coplanar base is
    /// dissolved; non-coplanar neighbours (outer side walls) are ignored, not a veto.
    /// The inside/outside mix test classifies probes against the UNION of those
    /// coplanar pieces so a strip that covers the whole split rim is not refused.
    pub fn merge_coplanar_extrude_walls(
        &mut self,
        caps: &std::collections::HashSet<FaceId>,
    ) -> (usize, usize) {
        use std::collections::HashSet;
        // Same plane through a shared edge ⇔ |n·n| ≈ 1 (walls flip winding on push — sign is noise).
        const STACKED_DOT: f32 = 0.999;
        // Walls = faces across the caps' boundary half-edges.
        let mut walls: HashSet<FaceId> = HashSet::new();
        for &c in caps {
            if self.face(c).half_edge == NO_EDGE { continue; }
            for he in self.face_half_edges(c) {
                let tw = self.he(he).twin;
                if tw == NO_TWIN { continue; }
                let f = self.he(tw).face;
                if f != NO_FACE && !caps.contains(&f) { walls.insert(f); }
            }
        }
        let mut dissolve: HashSet<HalfEdgeId> = HashSet::new();
        let mut count = 0usize;
        for &w in &walls {
            let n_w = self.compute_face_normal(w);
            // EVERY wall edge whose far side is a pre-existing face (neither cap nor wall).
            // A hole-rim that was already split (owner `close wall.fl4`) makes the new
            // top/bottom wall an n-gon sitting on TWO+ coplanar rim faces — the old
            // "exactly one base or skip" rule left those stacked slivers in place.
            // Non-coplanar bases (outer side walls) are ignored, not a veto.
            let mut bases: Vec<(HalfEdgeId, FaceId)> = Vec::new();
            for he in self.face_half_edges(w) {
                let tw = self.he(he).twin;
                if tw == NO_TWIN { continue; }
                let f = self.he(tw).face;
                if f == NO_FACE || caps.contains(&f) || walls.contains(&f) { continue; }
                bases.push((he, f));
            }
            if bases.is_empty() { continue; }
            // Cap-side points, shared across every base of this wall.
            let mut cap_pts: Vec<glam::Vec3> = Vec::new();
            for he in self.face_half_edges(w) {
                let tw = self.he(he).twin;
                if tw == NO_TWIN { continue; }
                let f = self.he(tw).face;
                if f == NO_FACE || !caps.contains(&f) { continue; }
                cap_pts.push(self.vert(self.he_source(he)).position);
                cap_pts.push(self.vert(self.he(he).vertex).position);
            }
            if cap_pts.is_empty() {
                if let Some((base_he, _)) = bases.first() {
                    let n1 = self.he(*base_he).next;
                    let vc1 = self.he(n1).vertex;
                    let vc0 = self.he(self.he(n1).next).vertex;
                    cap_pts.push(self.vert(vc0).position);
                    cap_pts.push(self.vert(vc1).position);
                }
            }
            // Only coplanar neighbours are merge candidates. A non-coplanar neighbour
            // (the outer side wall on close wall.fl4) is ignored, not a veto.
            let mut coplanar: Vec<(HalfEdgeId, FaceId)> = Vec::new();
            for (he, f) in bases {
                let n_b = self.compute_face_normal(f);
                if n_w.dot(n_b).abs() >= STACKED_DOT {
                    coplanar.push((he, f));
                }
            }
            if coplanar.is_empty() { continue; }
            // Mix-test against the UNION of coplanar bases. Per-base inside/outside
            // vetoes a split rim: a probe over the right half is "outside" the left
            // piece even though it sits on the same plane. Genuine overshoot still
            // mixes against the whole union and is refused.
            //
            // Midpoints pair each base endpoint to the NEAREST cap point. Index-pairing
            // a hexagon wall's front base with the back cap shoots a diagonal through
            // the hole (owner close wall.fl4) and the mix test false-vetoes the merge.
            let mut probes: Vec<glam::Vec3> = cap_pts.clone();
            let mut diag = 0.0f32;
            for &(base_he, b_face) in &coplanar {
                if !cap_pts.is_empty() {
                    for p in [
                        self.vert(self.he_source(base_he)).position,
                        self.vert(self.he(base_he).vertex).position,
                    ] {
                        let mut best = cap_pts[0];
                        let mut best_d = (best - p).length_squared();
                        for &c in &cap_pts[1..] {
                            let d = (c - p).length_squared();
                            if d < best_d {
                                best = c;
                                best_d = d;
                            }
                        }
                        probes.push((p + best) * 0.5);
                    }
                }
                let bverts = self.face_vertices(b_face);
                for k in 1..bverts.len() {
                    diag = diag.max((self.vert(bverts[k]).position - self.vert(bverts[0]).position).length());
                }
            }
            let eps = (diag * 1e-4).max(1e-5);
            let (mut strict_in, mut strict_out) = (0usize, 0usize);
            for p in probes {
                let mut on_rim = false;
                let mut inside = false;
                for &(_, b_face) in &coplanar {
                    let bverts = self.face_vertices(b_face);
                    let m = bverts.len();
                    let mut rim = f32::MAX;
                    for k in 0..m {
                        let a = self.vert(bverts[k]).position;
                        let b = self.vert(bverts[(k + 1) % m]).position;
                        rim = rim.min(crate::knife::closest_on_seg_3d(p, a, b).1);
                    }
                    if rim <= eps { on_rim = true; }
                    if crate::knife::point_in_face_3d(self, b_face, p) { inside = true; }
                }
                if on_rim { continue; }
                if inside { strict_in += 1; } else { strict_out += 1; }
            }
            if strict_in > 0 && strict_out > 0 { continue; }
            for (base_he, _) in coplanar {
                dissolve.insert(base_he);
                count += 1;
            }
        }
        if !dissolve.is_empty() && !self.dissolve_edges_region(&dissolve) {
            return (0, self.count_folded_coplanar_pairs());
        }
        (count, self.count_folded_coplanar_pairs())
    }

    /// FOLD DETECTOR: adjacent faces that are coplanar with OPPOSITE normals (dot <= -0.999).
    /// A watertight solid can never contain such a pair — it means a face folded back over
    /// its neighbor as a zero-thickness flap (owner extrude.fl4 2026-08-03: the far wall
    /// came out as f0+f3-f5 with f5 inverted). Edge pairing stays consistent through a
    /// fold, so `is_manifold` cannot see it — callers use this count to reject the result.
    pub fn count_folded_coplanar_pairs(&self) -> usize {
        self.folded_coplanar_pairs_dbg().len()
    }

    /// Debug companion of [`count_folded_coplanar_pairs`]: the actual pairs.
    ///
    /// PERF (2026-08-09): normals are computed **once per face**, into a flat cache indexed by
    /// `FaceId`, with ONE pair of reused scratch buffers. The old loop called the ALLOCATING
    /// [`Self::compute_face_normal`] twice per undirected edge — two `Vec`s per call
    /// (`normals.rs`), i.e. ~4 heap allocations per edge, and it re-derived each face's normal
    /// once per INCIDENT edge (4x over on a quad mesh). `mesh_boolean_manifold` calls this 6-10
    /// times per boolean (fold-count gates before and after every polish candidate), so it was
    /// worth 10-20 ms per boolean on a 9216-face target.
    ///
    /// BIT-IDENTICAL, not merely equivalent: `compute_face_normal` is a pure function of
    /// (`&self`, `FaceId`) — the mesh cannot change inside this `&self` method — so reading a
    /// cached value is the same `f32` the second call would have produced. Newell's summation
    /// order is fixed by the face ring, which the cache does not touch. The cached value is the
    /// FULLY PROCESSED `compute_face_normal(f).normalize_or_zero()`, because normalising an
    /// already-unit `Vec3` a second time is NOT the identity in `f32` — the composite is what
    /// the comparison consumes, so the composite is what is cached.
    pub fn folded_coplanar_pairs_dbg(&self) -> Vec<(FaceId, FaceId)> {
        let mut folds: Vec<(FaceId, FaceId)> = Vec::new();
        let mut fnormal: Vec<Option<Vec3>> = vec![None; self.faces.len()];
        let (mut ring, mut pts) = (Vec::new(), Vec::new());
        for i in 0..self.half_edges.len() {
            let he = HalfEdgeId(i as u32);
            if !self.is_edge_live(he) {
                continue;
            }
            let tw = self.he(he).twin;
            if tw == NO_TWIN || tw.0 < i as u32 {
                continue;
            }
            let (fa, fb) = (self.he(he).face, self.he(tw).face);
            if fa == NO_FACE || fb == NO_FACE || fa == fb {
                continue;
            }
            let (ia, ib) = (fa.0 as usize, fb.0 as usize);
            if fnormal[ia].is_none() {
                fnormal[ia] =
                    Some(self.compute_face_normal_into(fa, &mut ring, &mut pts).normalize_or_zero());
            }
            if fnormal[ib].is_none() {
                fnormal[ib] =
                    Some(self.compute_face_normal_into(fb, &mut ring, &mut pts).normalize_or_zero());
            }
            let (na, nb) = (fnormal[ia].unwrap(), fnormal[ib].unwrap());
            if na != glam::Vec3::ZERO && nb != glam::Vec3::ZERO && na.dot(nb) <= -0.999 {
                folds.push((fa, fb));
            }
        }
        folds
    }

    /// Blender **Limited Dissolve**: dissolve undirected edges whose two adjacent face normals form
    /// an angle ≤ `angle_deg` (default Blender is 5°). Then the same valence-2 / boundary cleanup as
    /// [`dissolve_edges_region`].
    ///
    /// When `only_faces` is `Some`, only edges whose **both** faces are in the set are candidates
    /// (operate on a selection). When `None`, every interior edge of the mesh is considered.
    ///
    /// When `protect` is `Some`, the two faces of every protected edge are excluded from the
    /// dissolve, which is what actually keeps the edge (see the note at the face-set build —
    /// merely dropping it from the candidate list does NOT). Owner 2026-08-05: "mesh clean up
    /// should never touch [tagged edges]" — an edge carrying authored sidecar data (Smart Select
    /// `op_created`, Mark Sharp, UV seam, crease) holds intent that dies with the edge, so
    /// flatness alone must not remove it. Either half-edge of a pair may be passed.
    ///
    /// Returns how many undirected edges were marked for dissolve (0 = refuse / nothing to do).
    /// On 0 the mesh is untouched.
    pub fn limited_dissolve(
        &mut self,
        angle_deg: f32,
        only_faces: Option<&std::collections::HashSet<FaceId>>,
        protect: Option<&std::collections::HashSet<HalfEdgeId>>,
    ) -> u32 {
        use std::collections::HashSet;
        let angle = angle_deg.clamp(0.0, 180.0).to_radians();
        // Face normals within `angle` ⇔ n1·n2 ≥ cos(angle) (both unit).
        let cos_min = angle.cos();
        let mut to_dissolve: HashSet<HalfEdgeId> = HashSet::new();
        let he_n = self.half_edges.len();
        // Per-FACE normal cache + reused Newell scratch — the same fix, and for the same reason,
        // as [`Self::folded_coplanar_pairs_dbg`] (read its note for why this is bit-identical).
        // The candidate scan below is `&self`-only, so a face's normal cannot change inside it.
        // This scan is on the boolean path (`mesh_boolean/mod.rs`, `mesh_boolean/rebuild.rs`).
        let mut fnormal: Vec<Option<Vec3>> = vec![None; self.faces.len()];
        let (mut nring, mut npts) = (Vec::new(), Vec::new());
        // Protection has to hold the FACES, not just drop the edge from the candidate list.
        // `dissolve_edges_region` merges each selected-edge-connected COMPONENT of faces into
        // ONE n-gon, and an n-gon has no interior edges — so a protected edge whose two faces
        // reach each other by any OTHER dissolvable path is swallowed regardless (on a flat
        // grid every face is one component, which is how this was caught). Excluding the two
        // faces keeps the edge and its neighbours intact; everything further away still merges.
        let mut protected_faces: HashSet<FaceId> = HashSet::new();
        if let Some(p) = protect {
            for &h in p {
                if !self.is_edge_live(h) {
                    continue;
                }
                let (f1, f2) = self.edge_faces(h);
                if f1 != NO_FACE {
                    protected_faces.insert(f1);
                }
                if f2 != NO_FACE {
                    protected_faces.insert(f2);
                }
            }
        }
        for i in 0..he_n {
            let he = HalfEdgeId(i as u32);
            if !self.is_edge_live(he) {
                continue;
            }
            let tw = self.he(he).twin;
            // Undirected: process once (lower id).
            if tw == NO_TWIN || tw.0 < i as u32 {
                continue;
            }
            let fa = self.he(he).face;
            let fb = self.he(tw).face;
            if fa == NO_FACE || fb == NO_FACE || fa == fb {
                continue;
            }
            if protected_faces.contains(&fa) || protected_faces.contains(&fb) {
                continue;
            }
            if let Some(sel) = only_faces {
                if !sel.contains(&fa) || !sel.contains(&fb) {
                    continue;
                }
            }
            let (ia, ib) = (fa.0 as usize, fb.0 as usize);
            if fnormal[ia].is_none() {
                fnormal[ia] = Some(
                    self.compute_face_normal_into(fa, &mut nring, &mut npts).normalize_or_zero(),
                );
            }
            if fnormal[ib].is_none() {
                fnormal[ib] = Some(
                    self.compute_face_normal_into(fb, &mut nring, &mut npts).normalize_or_zero(),
                );
            }
            let n1 = fnormal[ia].unwrap();
            let n2 = fnormal[ib].unwrap();
            if n1.length_squared() < 0.25 || n2.length_squared() < 0.25 {
                continue;
            }
            if n1.dot(n2) >= cos_min {
                to_dissolve.insert(he);
            }
        }
        if to_dissolve.is_empty() {
            return 0;
        }
        let n = to_dissolve.len() as u32;
        if self.dissolve_edges_region(&to_dissolve) {
            n
        } else {
            0
        }
    }

}

/// Live-vertex census shared by every weld: `(live ids, positions, id → live index)`.
/// Split out so the proximity weld and [`HalfEdgeMesh::dissolve_degenerate`]'s edge-length weld
/// build their clusters over the SAME index space and hand it to the SAME rebuild.
pub(super) fn live_vertex_census(
    src: &HalfEdgeMesh,
) -> (Vec<u32>, Vec<Vec3>, std::collections::HashMap<u32, usize>) {
    let mut live: Vec<u32> = Vec::new();
    let mut pos: Vec<Vec3> = Vec::new();
    let mut id_to_idx: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for i in 0..src.vertices.len() as u32 {
        if !src.is_vert_live(VertexId(i)) {
            continue;
        }
        id_to_idx.insert(i, live.len());
        live.push(i);
        pos.push(src.vert(VertexId(i)).position);
    }
    (live, pos, id_to_idx)
}

/// Union-find over live vertex indices that can be told which pairs must NEVER share a cluster.
///
/// The guard exists because every weld in this kernel is destructive-by-rebuild: once two
/// vertices land in one cluster the edge between them is gone for good. Mesh Cleanup protects
/// authored edges (Smart Select tags / sharp / seam / crease) by handing their endpoints here,
/// so the ONE union that would fuse such an edge is declined while every other weld proceeds.
pub(super) struct WeldUf {
    parent: Vec<usize>,
    rank: Vec<u8>,
    /// Protected pairs as live-index pairs.
    pairs: Vec<(usize, usize)>,
    /// Cluster root → indices into `pairs` with an endpoint in that cluster (small-to-large merged).
    at: std::collections::HashMap<usize, Vec<usize>>,
}

impl WeldUf {
    pub(super) fn new(n: usize, pairs: Vec<(usize, usize)>) -> Self {
        let mut at: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
        for (k, &(a, b)) in pairs.iter().enumerate() {
            at.entry(a).or_default().push(k);
            at.entry(b).or_default().push(k);
        }
        Self { parent: (0..n).collect(), rank: vec![0; n], pairs, at }
    }

    pub(super) fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    /// Merge `a`'s and `b`'s clusters unless that would collapse a protected pair.
    /// Returns true when the merge happened.
    pub(super) fn unite(&mut self, a: usize, b: usize) -> bool {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        if !self.pairs.is_empty() {
            // Probe the smaller side so a big cluster absorbing singletons stays cheap.
            let (probe_root, other_root) =
                if self.at.get(&ra).map_or(0, |v| v.len()) <= self.at.get(&rb).map_or(0, |v| v.len())
                { (ra, rb) } else { (rb, ra) };
            if let Some(list) = self.at.get(&probe_root) {
                let candidates: Vec<usize> = list.clone();
                for k in candidates {
                    let (pa, pb) = self.pairs[k];
                    let (rpa, rpb) = (self.find(pa), self.find(pb));
                    let spans = (rpa == probe_root && rpb == other_root)
                        || (rpa == other_root && rpb == probe_root);
                    if spans {
                        return false;
                    }
                }
            }
        }
        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] = self.rank[ra].saturating_add(1);
        }
        if let Some(list) = self.at.remove(&rb) {
            self.at.entry(ra).or_default().extend(list);
        }
        true
    }
}

/// Resolve protected endpoint POSITIONS to live-vertex index pairs, quantised at the same 1e-5
/// the boolean's `protected_half_edges` uses (one protection currency across the kernel).
/// Positions with no live vertex are dropped — a protected edge that no longer exists cannot
/// constrain a weld.
pub(super) fn protected_index_pairs(
    pos: &[Vec3],
    protected: &[(Vec3, Vec3)],
) -> Vec<(usize, usize)> {
    if protected.is_empty() {
        return Vec::new();
    }
    let q = |p: Vec3| -> (i64, i64, i64) {
        ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)
    };
    let mut by_key: std::collections::HashMap<(i64, i64, i64), usize> =
        std::collections::HashMap::with_capacity(pos.len());
    for (i, &p) in pos.iter().enumerate() {
        by_key.entry(q(p)).or_insert(i);
    }
    protected
        .iter()
        .filter_map(|&(a, b)| {
            let ia = *by_key.get(&q(a))?;
            let ib = *by_key.get(&q(b))?;
            if ia == ib { None } else { Some((ia, ib)) }
        })
        .collect()
}

/// Spatial weld for half-edge meshes: cluster verts within `thr`, average each cluster, rebuild.
/// Pure — does not mutate `src`. Used by [`HalfEdgeMesh::merge_by_distance_ex`].
fn weld_half_edge_by_distance(
    src: &HalfEdgeMesh,
    thr: f32,
    protected: &[(Vec3, Vec3)],
) -> HalfEdgeMesh {
    use std::collections::HashMap;
    let thr = thr.max(1e-12);
    let thr2 = thr * thr;
    let inv = 1.0 / thr;

    // Live vertex ids + positions.
    let (live, pos, id_to_idx) = live_vertex_census(src);
    let n = live.len();
    if n == 0 {
        return HalfEdgeMesh::new();
    }

    let mut uf = WeldUf::new(n, protected_index_pairs(&pos, protected));

    let cell_of = |p: Vec3| -> (i32, i32, i32) {
        (
            (p.x * inv).floor() as i32,
            (p.y * inv).floor() as i32,
            (p.z * inv).floor() as i32,
        )
    };
    let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::with_capacity(n);
    for i in 0..n {
        let c = cell_of(pos[i]);
        // Union with any already-placed point within thr in the 3×3×3 neighbourhood.
        for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                for dz in -1i32..=1 {
                    let key = (c.0 + dx, c.1 + dy, c.2 + dz);
                    if let Some(list) = grid.get(&key) {
                        for &j in list {
                            if (pos[i] - pos[j]).length_squared() <= thr2 {
                                uf.unite(i, j);
                            }
                        }
                    }
                }
            }
        }
        grid.entry(c).or_default().push(i);
    }

    weld_rebuild(src, &pos, &id_to_idx, &mut uf, 0.0)
}

/// Rebuild `src` with every vertex collapsed onto its [`WeldUf`] cluster centroid.
///
/// Shared tail of every weld in this kernel (proximity merge-by-distance and degenerate
/// dissolve): cluster → average position, compact, re-map each face ring, drop rings that the
/// weld reduced below a real polygon, then `from_polygons_keeping_ids` so per-face materials AND
/// ids survive: every unwelded vertex, every surviving face and edge keeps its id, and a welded
/// cluster keeps its lowest member's. Pure — does not mutate `src`.
///
/// `min_area` > 0 additionally drops rings whose Newell area falls at or below it — the
/// zero-area half of Blender's Degenerate Dissolve. Pass 0.0 for a plain weld.
pub(super) fn weld_rebuild(
    src: &HalfEdgeMesh,
    pos: &[Vec3],
    id_to_idx: &std::collections::HashMap<u32, usize>,
    uf: &mut WeldUf,
    min_area: f32,
) -> HalfEdgeMesh {
    use std::collections::HashMap;
    let n = pos.len();
    // Live index → vertex id (the census numbers live vertices in ascending id order).
    let mut idx_to_id = vec![0u32; n];
    for (&id, &li) in id_to_idx {
        idx_to_id[li] = id;
    }
    // Cluster → average position (Blender Merge by Distance uses average of the cluster), and its
    // LOWEST member — the vertex whose id the welded vertex keeps (ascending `i` → first seen).
    let mut sum: HashMap<usize, (Vec3, u32, usize)> = HashMap::new();
    for i in 0..n {
        let r = uf.find(i);
        let e = sum.entry(r).or_insert((Vec3::ZERO, 0, i));
        e.0 += pos[i];
        e.1 += 1;
    }
    let mut rep: HashMap<usize, (Vec3, usize)> = HashMap::with_capacity(sum.len());
    for (r, (s, c, first)) in sum {
        rep.insert(r, (s / c as f32, first));
    }

    // Compact cluster ids → new vertex indices (stable order by first live index).
    let mut cluster_order: Vec<usize> = rep.keys().copied().collect();
    cluster_order.sort_unstable();
    let mut cluster_to_new: HashMap<usize, usize> = HashMap::new();
    let mut new_positions: Vec<Vec3> = Vec::with_capacity(cluster_order.len());
    // ID-STABLE (POLY-STALE-IDS): a cluster IS its members welded together, so the welded vertex
    // keeps one member's id (the lowest — deterministic); the others' ids become dead slots.
    let mut pos_origin: Vec<Option<VertexId>> = Vec::with_capacity(cluster_order.len());
    for r in cluster_order {
        cluster_to_new.insert(r, new_positions.len());
        let (p, first) = rep[&r];
        new_positions.push(p);
        pos_origin.push(Some(VertexId(idx_to_id[first])));
    }

    // Rebuild faces: map each ring through weld, drop degenerates.
    let mut faces: Vec<Vec<usize>> = Vec::new();
    let mut face_mats: Vec<u32> = Vec::new();
    let mut face_parent: Vec<Option<FaceId>> = Vec::new();
    for i in 0..src.faces.len() as u32 {
        let fid = FaceId(i);
        if src.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let mat = src.face(fid).material_id;
        let mut ring: Vec<usize> = Vec::new();
        for v in src.face_vertices(fid) {
            let Some(&li) = id_to_idx.get(&v.0) else {
                continue;
            };
            let r = uf.find(li);
            let ni = cluster_to_new[&r];
            if ring.last().copied() != Some(ni) {
                ring.push(ni);
            }
        }
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        // Collapse runs of the same welded index that wrap.
        if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
            ring.pop();
        }
        // Remove consecutive duplicates again after wrap clean.
        let mut cleaned = Vec::with_capacity(ring.len());
        for &idx in &ring {
            if cleaned.last().copied() != Some(idx) {
                cleaned.push(idx);
            }
        }
        if cleaned.len() >= 2 && cleaned[0] == *cleaned.last().unwrap() {
            cleaned.pop();
        }
        if cleaned.len() < 3 {
            continue;
        }
        if min_area > 0.0 {
            // Newell area — sign-correct for any non-planar / non-convex ring, so a fold or a
            // needle sliver reads as ~0 exactly like a collapsed one.
            let pts: Vec<Vec3> = cleaned.iter().map(|&i| new_positions[i]).collect();
            if super::newell_normal(&pts).length() * 0.5 <= min_area {
                continue;
            }
        }
        faces.push(cleaned);
        face_mats.push(mat);
        face_parent.push(Some(fid));
    }
    if faces.is_empty() {
        return src.clone();
    }
    HalfEdgeMesh::from_polygons_keeping_ids(src, &new_positions, &pos_origin, &faces, &face_parent, &face_mats)
}
