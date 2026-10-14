// Euler primitives: split / create / delete / install (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    // ============================================================
    // Edit operations
    // ============================================================

    /// Split an edge at parameter t ∈ [0,1], inserting a new vertex.
    /// Returns the new vertex ID.
    pub fn split_edge(&mut self, he: HalfEdgeId, t: f32) -> VertexId {
        let src = self.he_source(he);
        let dst = self.he(he).vertex;
        let pos = self.vert(src).position.lerp(self.vert(dst).position, t);
        let normal = self.vert(src).normal.lerp(self.vert(dst).normal, t).normalize_or_zero();

        // Create new vertex
        let new_v = self.alloc_vertex(HVertex { position: pos, normal, half_edge: NO_EDGE });

        // Split this half-edge: old [src→dst] becomes [src→new_v] + [new_v→dst]
        let old_next = self.he(he).next;
        let old_face = self.he(he).face;

        let new_he = self.alloc_half_edge(HalfEdge {
            vertex: dst,
            twin: NO_TWIN, // will be fixed below
            next: old_next,
            prev: he,
            face: old_face,
        });

        self.he_mut(he).vertex = new_v;
        self.he_mut(he).next = new_he;
        self.he_mut(old_next).prev = new_he;
        self.vert_mut(new_v).half_edge = new_he;

        // Split twin if it exists
        let twin = self.he(he).twin;
        if twin != NO_TWIN {
            let twin_next = self.he(twin).next;
            let twin_face = self.he(twin).face;

            let new_twin_he = self.alloc_half_edge(HalfEdge {
                vertex: src,
                twin: he,
                next: twin_next,
                prev: twin,
                face: twin_face,
            });

            self.he_mut(twin).vertex = new_v;
            self.he_mut(twin).next = new_twin_he;
            self.he_mut(twin_next).prev = new_twin_he;

            // Fix twin links
            self.he_mut(he).twin = new_twin_he;
            self.he_mut(new_he).twin = twin;
            self.he_mut(twin).twin = new_he;
            self.he_mut(new_twin_he).twin = he;
        }

        new_v
    }

    /// Split a face into two by connecting two of its boundary vertices with a NEW diagonal edge.
    /// Returns the new face (the half containing the vertices between `v_b` and `v_a` in winding order),
    /// or `None` if the vertices aren't both on the face, are the same, or are adjacent.
    ///
    /// ID-PRESERVING EULER SPLIT (fixed 2026-07-17). The old body did `delete_face(f) + create_face ×2`,
    /// which FREED all of `f`'s boundary half-edges and relied on `create_face`'s `vertex_fan`-based twin
    /// auto-pairing to re-link them to the neighbours. On an INTERIOR face (one sharing an edge with
    /// another face) that re-pairing silently failed — the shared edge's twin was dropped, producing a
    /// bowtie (`is_manifold` "2 disjoint fans"). Confirmed on a 1×2 quad grid: a single diagonal split of
    /// one quad bowtied the shared vertex. The knife (`knife.rs`) and vert-connect both split interior
    /// faces, so this was a live corruption path.
    ///
    /// The fix inserts ONE diagonal half-edge pair and REUSES every existing boundary half-edge in place,
    /// so their twins to neighbours are never touched. `f` is reused for one half (stable id) and a new
    /// face for the other; both inherit the original `material_id`. This is a true Euler op (no rebuild),
    /// which is also what "ID-preserving" in the vert-connect KB entry means.
    pub fn split_face_between(&mut self, f: FaceId, v_a: VertexId, v_b: VertexId) -> Option<FaceId> {
        let verts = self.face_vertices(f);
        let hes = self.face_half_edges(f);
        let n = verts.len();
        if n < 4 || hes.len() != n { return None; } // a tri has no non-adjacent pair; guard corruption.
        let a = verts.iter().position(|&v| v == v_a)?;
        let b = verts.iter().position(|&v| v == v_b)?;
        if a == b { return None; }
        // Adjacent → the diagonal would duplicate an existing edge / make a 2-gon. Blender no-ops this.
        if (a + 1) % n == b || (b + 1) % n == a { return None; }

        // Ring convention: hes[k] : v[k-1] -> v[k] (dest = verts[k]). So the edge LEAVING v_a is
        // hes[(a+1)%n] and the edge ARRIVING at v_a is hes[a] (same for v_b).
        let mat = self.face(f).material_id;
        let (ha1, hb) = (hes[(a + 1) % n], hes[b]);   // face-1 boundary run start / end (v_a..v_b)
        let (hb1, ha) = (hes[(b + 1) % n], hes[a]);   // face-2 boundary run start / end (v_b..v_a)

        // The diagonal: d_ab (v_a -> v_b) closes face 2, d_ba (v_b -> v_a) closes face 1; they twin.
        let d_ab = self.alloc_half_edge(HalfEdge { vertex: v_b, twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: NO_FACE });
        let d_ba = self.alloc_half_edge(HalfEdge { vertex: v_a, twin: d_ab, next: NO_EDGE, prev: NO_EDGE, face: f });
        self.he_mut(d_ab).twin = d_ba;

        // Face 1 (reuse f): ... -> hb -> d_ba -> ha1 -> ...  (loop v_a..v_b + diagonal back to v_a).
        self.he_mut(d_ba).next = ha1; self.he_mut(d_ba).prev = hb;
        self.he_mut(hb).next = d_ba;  self.he_mut(ha1).prev = d_ba;
        self.face_mut(f).half_edge = d_ba;

        // Face 2 (new): ... -> ha -> d_ab -> hb1 -> ...  (loop v_b..v_a + diagonal back to v_b).
        let f2 = self.alloc_face(HFace { half_edge: d_ab, normal: Vec3::ZERO, material_id: mat });
        self.he_mut(d_ab).next = hb1; self.he_mut(d_ab).prev = ha; self.he_mut(d_ab).face = f2;
        self.he_mut(ha).next = d_ab;  self.he_mut(hb1).prev = d_ab;
        // Reassign .face for face 2's whole boundary run (was f); the diagonal is already f2.
        let mut cur = hb1;
        while cur != d_ab {
            self.he_mut(cur).face = f2;
            cur = self.he(cur).next;
        }

        // Refresh the two faces' normals from their (now-planar-diagonal) loops — O(face), not O(mesh).
        self.face_mut(f).normal = self.compute_face_normal(f);
        self.face_mut(f2).normal = self.compute_face_normal(f2);
        Some(f2)
    }


    /// Delete a face (mark as free, detach half-edges). Redundant deletes are a no-op.
    pub fn delete_face(&mut self, f: FaceId) {
        // Already-free slot => nothing to detach and nothing to free. Without this the body below runs
        // its loops as no-ops (`face_half_edges`/`face_vertices` return empty for a freed face) but
        // still reaches the free push, retiring `f` a SECOND time. `free_face` refuses the duplicate,
        // so this early return is not what prevents the corruption — it is what keeps the redundant
        // call byte-identical instead of merely harmless. Mirrors `dissolve_face`'s self-guard.
        //
        // NOTE this deliberately does NOT bounds-check. An out-of-range FaceId is a CALLER BUG, and it
        // still panics on the next line at `face_half_edges` -> `face()`'s direct index, exactly as it
        // did before `free_face` existed. Folding a bounds check in here would silently swallow that
        // class of caller bug — trading a loud failure for a quiet one, which CLAUDE.md forbids.
        if self.face_slot_is_free(f) { return; }
        let edges = self.face_half_edges(f);
        let verts = self.face_vertices(f);
        for &he in &edges {
            let twin = self.he(he).twin;
            if twin != NO_TWIN {
                // Clear the twin's twin pointer so it becomes boundary
                // and can be re-paired by future create_face calls
                self.he_mut(twin).twin = NO_TWIN;
            }
            // Free this half-edge through the chokepoint (canonical dead state, no dangling next/prev).
            self.free_edge(he);
        }
        self.free_face(f);

        // Repoint any corner vertex whose disk pointer now references a freed / mislinked half-edge
        // (audit #6). A stale vertex.half_edge makes vertex_fan/find_half_edge truncate, which mis-pairs
        // twins in the very next create_face (inset/dissolve rebuild) and corrupts the mesh. Reassign to
        // a live outgoing half-edge, or NO_EDGE if the vertex became loose. Scan directly (not via the
        // fan — the fan is exactly what's untrustworthy here).
        //
        // PERF 2026-08-09: this used to be a per-corner `repoint_vert_disk_if_stale(v)`, i.e. up to one
        // FULL half-edge scan PER CORNER of every deleted face. On a 96x96 plane (9216 f / ~37k HE)
        // 64 deletes cost 65 ms — 97 % of the whole `inset_region(64)`, which deletes the region face by
        // face. One pass for the WHOLE corner ring resolves every stale corner at once and stops as soon
        // as the last one is answered.
        self.repoint_vert_disks_if_stale(&verts);
    }

    /// Batch [`delete_face`] over many faces in **one** free/repoint pass.
    ///
    /// Owner 2026-08-07: Del on a UV sphere (A → all faces) hung the app. App path issued one
    /// `PolyEdit::DeleteFace` per face; each `apply_in_place` clones + validates the whole mesh
    /// → O(N) full-body work for N faces. This kernel batches frees, then repairs vertex disks
    /// with a **single** O(HE) scan (not O(verts×HE) per-face scans).
    ///
    /// **DETERMINISM (fixed 2026-08-09).** Faces are freed in ASCENDING id order, never in the
    /// caller `HashSet`'s own order. std sets iterate in a per-process-random order (SipHash with
    /// a random seed), and `free_face`/`free_edge` PUSH in iteration order while `alloc_face`/
    /// `alloc_half_edge` reuse slots by LIFO `pop()` — so a random free order made every id this
    /// mesh hands out afterwards random too. Measured before the fix: the same `delete_faces` on
    /// the same plane digested to a different mesh in each of four processes. This is the exact
    /// defect class `mesh_boolean/mod.rs`'s DETERMINISM banner and `extrude_region`'s
    /// `faces_sorted` were written for; the reproducible order is what makes this a safe drop-in
    /// for a `delete_face` loop (`inset_region`), which frees in sorted order by construction.
    ///
    /// Sorted order also makes the freed SET the only thing the caller controls: a shared edge
    /// between two selected faces, a face listed twice, and a stale id all resolve identically
    /// whatever order the selection was built in.
    pub fn delete_faces(&mut self, faces: &std::collections::HashSet<FaceId>) {
        if faces.is_empty() {
            return;
        }
        let mut ordered: Vec<FaceId> = faces.iter().copied().collect();
        ordered.sort_unstable_by_key(|f| f.0);
        let mut verts_touch: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut free_hes: Vec<HalfEdgeId> = Vec::new();
        for &f in &ordered {
            if !self.is_face_live(f) {
                continue;
            }
            let edges = self.face_half_edges(f);
            let verts = self.face_vertices(f);
            for &v in &verts {
                verts_touch.insert(v.0);
            }
            for &he in &edges {
                let twin = self.he(he).twin;
                if twin != NO_TWIN {
                    self.he_mut(twin).twin = NO_TWIN;
                }
                free_hes.push(he);
            }
            self.free_face(f);
        }
        for he in free_hes {
            if !self.edge_slot_is_free(he) {
                self.free_edge(he);
            }
        }
        // Same single-pass repair as `delete_face` — ONE ascending half-edge pass answering every
        // touched vertex with its lowest-id live outgoing half-edge. This used to be an inline
        // `HashMap<src, first_out>` built over the WHOLE mesh (one hash insert per live half-edge,
        // ~37k on a 96x96 plane) plus a second per-vertex pass; the shared helper computes the same
        // per-vertex answer with no map and stops as soon as the last vertex is resolved. Order of
        // `verts_touch` never mattered — each vertex's answer is independent of the others.
        let touched: Vec<VertexId> = verts_touch.into_iter().map(VertexId).collect();
        self.repoint_vert_disks_if_stale(&touched);
    }

    /// `free_faces.contains(&f.0)` **without** the O(free-list) linear scan — routed through the
    /// [`HalfEdgeMesh::is_face_live`] predicate so it inherits that predicate's O(1) index.
    ///
    /// Deliberately in-range-guarded rather than written as `!is_face_live(f)`: an OUT-OF-RANGE
    /// `FaceId` is a CALLER BUG that must keep reaching `face()`'s direct index and panicking (see
    /// [`Self::delete_face`]'s note). `free_faces` only ever holds in-range slots (`free_face`
    /// bounds-checks before pushing), so an out-of-range id answered `false` from `Vec::contains`
    /// and answers `false` here too — the replacement is exact, not merely equivalent-in-practice.
    #[inline]
    fn face_slot_is_free(&self, f: FaceId) -> bool {
        (f.0 as usize) < self.faces.len() && !self.is_face_live(f)
    }

    /// `free_edges.contains(&he.0)` without the O(free-list) linear scan — the edge-side mirror of
    /// [`Self::face_slot_is_free`], with the identical in-range rationale (`free_edge` bounds-checks
    /// before pushing, so `NO_EDGE` / out-of-range ids answer `false` exactly as `Vec::contains` did).
    #[inline]
    fn edge_slot_is_free(&self, he: HalfEdgeId) -> bool {
        (he.0 as usize) < self.half_edges.len() && !self.is_edge_live(he)
    }

    /// Is `v.half_edge` free / out-of-range / mis-sourced — i.e. does the disk pointer need repair?
    /// The staleness rule, named once: `delete_face`, `delete_faces` and `delete_face_only` all
    /// open-coded this same four-clause test, and `is_edge_live` already folds three of them.
    /// Callers must have bounds-checked `v` (`vert()` indexes directly).
    #[inline]
    fn vert_disk_is_stale(&self, v: VertexId) -> bool {
        let h = self.vert(v).half_edge;
        !self.is_edge_live(h) || self.he_source(h) != v
    }

    /// Re-point the disk pointer of every vertex in `verts` that went stale, using **one**
    /// half-edge pass for the whole set instead of one FULL pass per vertex.
    ///
    /// **The choice is identical to the old per-vertex scan, by construction.** That scan walked
    /// half-edge ids ASCENDING and took the first live one whose `he_source` was `v` — i.e.
    /// `min { h : is_edge_live(h) && he_source(h) == v }`, or `NO_EDGE` when the vertex went loose.
    /// This pass walks the SAME ascending order and records the SAME first hit for every pending
    /// vertex simultaneously, so each one still receives its lowest-id live outgoing half-edge.
    /// That value is what `vertex_fan` starts from, so anything weaker would change downstream op
    /// output — hence "lowest live id sourced at v" is reproduced exactly rather than replaced by a
    /// cheaper local pick (a fan walk can only see ONE fan, and `is_manifold` check (d) exists
    /// precisely because a vertex may carry several disjoint ones).
    ///
    /// Repeated vertices in `verts` are deduped: the old code called the per-vertex repair once per
    /// occurrence, and the repair is idempotent (a repointed pointer is either live-and-sourced-at-v,
    /// so the second call early-returns, or `NO_EDGE`, so the second call re-derives `NO_EDGE`).
    ///
    /// Cost: O(corners log corners) to build the pending list, then ONE ascending half-edge pass that
    /// stops the moment the last pending vertex is answered. A vertex left LOOSE by the delete has no
    /// answer to find, so it alone still forces the pass to run to the end — that is the irreducible
    /// part of "prove no live half-edge is sourced at v", which is a global question.
    ///
    /// `pub(super)` on purpose: `ops_extrude.rs::extrude_first_outgoing_map` carries a
    /// "DUPLICATION, FLAGGED NOT FIXED … all three want one shared `HalfEdgeMesh` helper; that lives
    /// in `euler.rs`" note. This IS that helper (it already absorbed the other two — `delete_faces`'
    /// inline `first_out` map and `delete_face_only`'s copy of the unbatched scan), so the extrude
    /// tails can migrate to it without another edit here.
    pub(super) fn repoint_vert_disks_if_stale(&mut self, verts: &[VertexId]) {
        // (vertex, answer). `NO_EDGE` doubles as "not yet answered": the pass only ever stores a real
        // half-edge id, and a vertex still holding `NO_EDGE` at the end is exactly the loose case.
        let mut pending: Vec<(VertexId, HalfEdgeId)> = Vec::new();
        for &v in verts {
            if (v.0 as usize) >= self.vertices.len() {
                continue;
            }
            if self.vert_disk_is_stale(v) {
                pending.push((v, NO_EDGE));
            }
        }
        if pending.is_empty() {
            return;
        }
        // Sorted + deduped so the per-half-edge membership test is a binary search. Order does not
        // affect the outcome — each vertex's answer depends only on the half-edges, which this pass
        // never touches.
        pending.sort_unstable_by_key(|&(v, _)| v.0);
        pending.dedup_by_key(|&mut (v, _)| v);

        let mut unresolved = pending.len();
        for i in 0..self.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !self.is_edge_live(he) {
                continue;
            }
            let src = self.he_source(he);
            if let Ok(k) = pending.binary_search_by_key(&src.0, |&(v, _)| v.0) {
                if pending[k].1 == NO_EDGE {
                    pending[k].1 = he;
                    unresolved -= 1;
                    if unresolved == 0 {
                        break;
                    }
                }
            }
        }
        for (v, repl) in pending {
            self.vert_mut(v).half_edge = repl;
        }
    }

    /// Blender "Delete > Only Faces" (`bpy.ops.mesh.delete(type="ONLY_FACE")`): remove ONLY the face
    /// record and KEEP every one of its half-edges + vertices. An edge that loses BOTH its faces
    /// survives as a WIRE EDGE (both half-edges `face == NO_FACE`, still twinned / or a lone half-edge
    /// with `twin == NO_TWIN`); an edge still bordering a surviving face becomes a normal boundary edge
    /// of that face. This is the deliberate opposite of [`delete_face`], which FREES the half-edges so a
    /// now-loose edge disappears (Blender "Delete Faces").
    ///
    /// Representation: the kernel's boundary convention is `face == NO_FACE` (from_polygons / delete_face
    /// leave boundary half-edges faceless; the only pre-existing live faceless slots are those). This op
    /// creates the FIRST live faceless slots that also retain their `next`/`prev` cycle — the deleted
    /// face's own loop, unchanged, minus its `.face`. That cycle is already self-consistent, so `validate()`
    /// (twin symmetry + next/prev round-trips) passes with ZERO relinking, and `is_manifold()` already
    /// skips faceless half-edges in its edge-count/bowtie checks (:701) and tolerates wire vertices (their
    /// `vertex.half_edge` points at a live sourced half-edge → check (c) passes; `by_source` never indexes
    /// them → check (d) is a no-op). So a wire result is accepted by both gates with no loosening.
    ///
    /// Idempotent + self-guarding like [`delete_face`] (an already-free slot is a no-op).
    pub fn delete_face_only(&mut self, f: FaceId) {
        // Already-free slot ⇒ nothing to clear and nothing to retire (mirrors delete_face's self-guard).
        // Deliberately NOT bounds-checked — an out-of-range FaceId is a CALLER BUG and still panics on the
        // next line at `face_half_edges` → `face()`'s direct index, exactly as delete_face does.
        if self.face_slot_is_free(f) { return; }
        // Capture BEFORE freeing the face — `free_face` nulls `faces[f].half_edge`, after which
        // `face_half_edges`/`face_vertices` return empty.
        let edges = self.face_half_edges(f);
        let verts = self.face_vertices(f);

        // KEEP every half-edge (and its twin, and its next/prev cycle) — only clear the face back-pointer.
        // A half-edge whose twin still bears a face is now a boundary half-edge of that surviving face; one
        // whose twin is also faceless (or NO_TWIN) is a wire edge. Either way the `next`/`prev` cycle is the
        // deleted face's original loop, still valid — nothing to relink.
        for &he in &edges {
            self.he_mut(he).face = NO_FACE;
        }
        // Retire ONLY the face record (through the single chokepoint). Free NOTHING else.
        self.free_face(f);

        // Repair any corner vertex whose disk pointer now references a mislinked half-edge, mirroring
        // delete_face. Nothing was freed here, so in the common case every pointer is still live and
        // sourced at its vertex and this is a no-op; it stays for parity with delete_face and to heal
        // any pre-existing stale pointer rather than propagate it. Shares delete_face's ONE repair
        // implementation (this was a verbatim copy of it, full-mesh scan and all). That adopts
        // delete_face's already-shipped skip for an out-of-range corner id (this copy indexed
        // `vert()` directly and panicked) — the two siblings are now consistent instead of
        // divergently defensive about the same corrupt-ring case.
        self.repoint_vert_disks_if_stale(&verts);

        // The result must be topology-valid (twin symmetry + next/prev cycles). A wire loop is a legit
        // outcome, so this asserts against a RELINK bug, never against wire edges.
        debug_assert!(self.validate().is_ok(), "delete_face_only produced invalid topology: {:?}", self.validate());
    }

    /// Batch [`delete_face_only`] over a face SET in a stable (ascending id) order — Blender's
    /// "Only Faces" on a multi-face selection. Stable order makes a shared edge between two selected
    /// faces resolve identically regardless of selection insertion order (it ends a wire edge either way).
    pub fn delete_faces_only(&mut self, faces: &std::collections::HashSet<FaceId>) {
        let mut ordered: Vec<FaceId> = faces.iter().copied().collect();
        ordered.sort_unstable_by_key(|f| f.0);
        for f in ordered {
            if self.face_slot_is_free(f) { continue; }
            if self.face(f).half_edge == NO_EDGE { continue; }
            self.delete_face_only(f);
        }
    }

    /// Blender "Delete Vertices": remove the vertex and EVERY face/edge that uses it (leaves a hole).
    /// Delegates to `delete_face` for each incident face (which frees the shared edges + repairs the
    /// surviving neighbours' disk pointers), then frees the now-loose vertex.
    pub fn delete_vertex(&mut self, v: VertexId) {
        let incident: Vec<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.is_face_live(f))
            .filter(|&f| self.face_vertices(f).contains(&v))
            .collect();
        for f in incident {
            if self.face(f).half_edge != NO_EDGE { self.delete_face(f); }
        }
        // Every incident face is gone → every edge at v is freed → v is loose. Free it.
        self.vert_mut(v).half_edge = NO_EDGE;
        self.free_vert(v);
    }

    /// Blender "Delete Edges": remove the (up to two) faces adjacent to the edge plus the edge itself
    /// (leaves a hole), and drop either endpoint that becomes loose. Distinct from `dissolve_edge`
    /// (which MERGES the two faces and keeps the surface).
    pub fn delete_edge(&mut self, he: HalfEdgeId) {
        let twin = self.he(he).twin;
        let (a, b) = (self.he_source(he), self.he(he).vertex);
        let f1 = self.he(he).face;
        let f2 = if twin != NO_TWIN { self.he(twin).face } else { NO_FACE };
        if f1 != NO_FACE && self.face(f1).half_edge != NO_EDGE { self.delete_face(f1); }
        if f2 != NO_FACE && self.face(f2).half_edge != NO_EDGE { self.delete_face(f2); }
        // Free the edge's half-edges if the face deletes didn't (e.g. a wire edge with no incident face).
        for e in [he, twin] {
            if e != NO_TWIN && self.is_edge_live(e) { self.free_edge(e); }
        }
        // Drop endpoints that no longer bound any live edge (Blender removes loose verts).
        for vtx in [a, b] {
            let still_used = (0..self.half_edges.len() as u32)
                .any(|i| self.is_edge_live(HalfEdgeId(i)) && self.he_source(HalfEdgeId(i)) == vtx);
            if !still_used && self.is_vert_live(vtx) {
                self.vert_mut(vtx).half_edge = NO_EDGE;
                self.free_vert(vtx);
            }
        }
    }

    /// Create a face from an ordered list of existing vertex IDs.
    /// Vertices must form a cycle (each pair connected by a half-edge gap or boundary).
    /// Returns the new FaceId, or None if the face can't be created.
    pub fn create_face(&mut self, vertex_ids: &[VertexId]) -> Option<FaceId> {
        let fid = self.create_face_no_normals(vertex_ids)?;
        self.recompute_normals();
        Some(fid)
    }

    /// [`Self::create_face`] with an explicit `material_id`.
    ///
    /// `create_face` allocates every face with `material_id: 0`. Any op that DELETES faces and
    /// re-creates replacement geometry (dissolve region merge, mirror/symmetrize rebuild) must use
    /// this instead, or the rebuilt faces come back with the default material and the user's
    /// assignment vanishes — the same defect class as `from_polygons` (see
    /// [`HalfEdgeMesh::from_polygons_with_materials`]).
    pub fn create_face_with_material(
        &mut self,
        vertex_ids: &[VertexId],
        material_id: u32,
    ) -> Option<FaceId> {
        let fid = self.create_face_no_normals_with_material(vertex_ids, material_id)?;
        self.recompute_normals();
        Some(fid)
    }

    /// [`Self::create_face_no_normals`] with an explicit `material_id`.
    pub fn create_face_no_normals_with_material(
        &mut self,
        vertex_ids: &[VertexId],
        material_id: u32,
    ) -> Option<FaceId> {
        let fid = self.create_face_no_normals(vertex_ids)?;
        self.faces[fid.0 as usize].material_id = material_id;
        Some(fid)
    }

    /// Like [`create_face`] but **no** normal recompute — for batch installs (bevel region,
    /// multi-face soup) that finish with one [`recompute_normals_region`]. Twin pairing still
    /// uses `find_half_edge` (callers doing large patches should prefer
    /// [`install_face_rings_local`] which twins via an edge map, O(new edges)).
    pub fn create_face_no_normals(&mut self, vertex_ids: &[VertexId]) -> Option<FaceId> {
        if vertex_ids.len() < 3 { return None; }
        let n = vertex_ids.len();

        let fid = self.alloc_face(HFace { half_edge: NO_EDGE, normal: Vec3::Y, material_id: 0 });

        let mut new_hes: Vec<HalfEdgeId> = Vec::with_capacity(n);
        for _ in 0..n {
            new_hes.push(self.alloc_half_edge(HalfEdge {
                vertex: VertexId(0), twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: NO_FACE,
            }));
        }

        for i in 0..n {
            let he_id = new_hes[i];
            let next_id = new_hes[(i + 1) % n];
            let prev_id = new_hes[(i + n - 1) % n];
            let dst = vertex_ids[(i + 1) % n];

            let he = &mut self.half_edges[he_id.0 as usize];
            he.vertex = dst;
            he.face = fid;
            he.next = next_id;
            he.prev = prev_id;
            he.twin = NO_TWIN;
        }

        self.faces[fid.0 as usize].half_edge = new_hes[0];

        for i in 0..n {
            let dst = vertex_ids[(i + 1) % n];
            let src = vertex_ids[i];
            let twin = self.find_half_edge(dst, src);
            if let Some(twin_he) = twin {
                if self.he(twin_he).twin == NO_TWIN {
                    self.half_edges[new_hes[i].0 as usize].twin = twin_he;
                    self.half_edges[twin_he.0 as usize].twin = new_hes[i];
                }
            }
        }

        for i in 0..n {
            let src = vertex_ids[i];
            if self.vert(src).half_edge == NO_EDGE {
                self.vertices[src.0 as usize].half_edge = new_hes[i];
            }
        }

        Some(fid)
    }

    /// Locality: delete a set of faces and free their half-edges **without** an O(mesh) scan.
    /// Surviving twins on faces **outside** `dying` become boundary and are registered in
    /// `boundary_out` as `(src,dst) → he` for later re-pairing. Twins that also die (shared
    /// edge between two dirty faces) are NOT registered — both sides rebuild and twin via the
    /// install edge-map.
    ///
    /// **DETERMINISM (fixed 2026-08-09).** Faces are freed in ASCENDING id order regardless of
    /// what the caller's iterator yields, for the reason spelled out on [`Self::delete_faces`]:
    /// free-list push order is LIFO-reused id order. Enforced HERE rather than at the call site
    /// because the one production caller (`bevel_op`'s region install) derives its `Vec` from a
    /// `HashSet<u32>` — a contract every future caller would otherwise have to re-derive
    /// (CLAUDE.md "No Bandaids" Q3: enforce structurally, not by tribal knowledge). Sorting an
    /// already-sorted region costs one pass over the dirty faces, nothing next to the frees.
    pub fn delete_faces_region(
        &mut self,
        faces: impl IntoIterator<Item = FaceId>,
        dying: &std::collections::HashSet<u32>,
        boundary_out: &mut std::collections::HashMap<(u32, u32), HalfEdgeId>,
    ) {
        let mut ordered: Vec<FaceId> = faces.into_iter().collect();
        ordered.sort_unstable_by_key(|f| f.0);
        let mut free_hes: Vec<HalfEdgeId> = Vec::new();
        for f in ordered {
            if self.face_slot_is_free(f) {
                continue;
            }
            if (f.0 as usize) >= self.faces.len() {
                continue;
            }
            let edges = self.face_half_edges(f);
            for &he in &edges {
                let twin = self.he(he).twin;
                if twin != NO_TWIN && !self.edge_slot_is_free(twin) {
                    let tf = self.he(twin).face;
                    let twin_dies = tf != NO_FACE && dying.contains(&tf.0);
                    self.he_mut(twin).twin = NO_TWIN;
                    if !twin_dies {
                        // Clean neighbour → true boundary for re-pair.
                        let tsrc = self.he_source(twin);
                        let tdst = self.he(twin).vertex;
                        boundary_out.insert((tsrc.0, tdst.0), twin);
                    }
                }
                free_hes.push(he);
            }
            self.free_face(f);
        }
        for he in free_hes {
            if self.edge_slot_is_free(he) {
                continue;
            }
            let src = self.he_source(he);
            if self.vert(src).half_edge == he {
                // LOWEST-id live boundary half-edge sourced at `src`, not "whichever the map
                // happened to yield first". This scan used to `break` on the first hit of a
                // HashMap iteration, i.e. it picked a RANDOM candidate per process, and the pick
                // becomes `vertex_fan`'s start — so every downstream fan walk (twin re-pair,
                // normals, the next op) branched on it. Lowest live id is the same rule
                // `repoint_vert_disks_if_stale` already applies for the un-regioned deletes, so
                // the two disk-repair paths now answer identically instead of one being ordered
                // and the other not. Dropping the `break` costs a full pass over `boundary_out`,
                // which is the REGION's boundary (bevel-sized), and only for a freed half-edge
                // that actually owns its source's disk pointer.
                let mut repl = NO_EDGE;
                for (&(s, _), &bhe) in boundary_out.iter() {
                    if s == src.0
                        && !self.edge_slot_is_free(bhe)
                        && (repl == NO_EDGE || bhe.0 < repl.0)
                    {
                        repl = bhe;
                    }
                }
                self.vert_mut(src).half_edge = repl;
            }
            self.free_edge(he);
        }
    }

    /// Install face rings in-place with O(new) twin linking (edge map + pre-collected boundary).
    /// Sets `material_id` per ring. Does **not** recompute normals (caller uses region normals).
    /// Returns new face ids (one per successful ring).
    ///
    /// Twin pass is **global after all rings exist** (from_polygons style): pair every
    /// `(src→dst)` with `(dst→src)` among new HEs + surviving boundary HEs. Incremental
    /// pairing missed reverse edges that appear later in the ring list (strips/caps).
    pub fn install_face_rings_local(
        &mut self,
        rings: &[(Vec<VertexId>, u32)],
        boundary: std::collections::HashMap<(u32, u32), HalfEdgeId>,
    ) -> Vec<FaceId> {
        use std::collections::HashMap;
        let mut out_fids = Vec::with_capacity(rings.len());
        let mut new_hes_all: Vec<HalfEdgeId> = Vec::new();

        for (ring, mat) in rings {
            if ring.len() < 3 {
                continue;
            }
            let n = ring.len();
            let fid = self.alloc_face(HFace {
                half_edge: NO_EDGE,
                normal: Vec3::Y,
                material_id: *mat,
            });
            let mut new_hes: Vec<HalfEdgeId> = Vec::with_capacity(n);
            for _ in 0..n {
                new_hes.push(self.alloc_half_edge(HalfEdge {
                    vertex: VertexId(0),
                    twin: NO_TWIN,
                    next: NO_EDGE,
                    prev: NO_EDGE,
                    face: NO_FACE,
                }));
            }
            for i in 0..n {
                let he_id = new_hes[i];
                let next_id = new_hes[(i + 1) % n];
                let prev_id = new_hes[(i + n - 1) % n];
                let src = ring[i];
                let dst = ring[(i + 1) % n];
                {
                    let he = &mut self.half_edges[he_id.0 as usize];
                    he.vertex = dst;
                    he.face = fid;
                    he.next = next_id;
                    he.prev = prev_id;
                    he.twin = NO_TWIN;
                }
                if self.vert(src).half_edge == NO_EDGE
                    || self.edge_slot_is_free(self.vert(src).half_edge)
                {
                    self.vert_mut(src).half_edge = he_id;
                }
            }
            self.faces[fid.0 as usize].half_edge = new_hes[0];
            new_hes_all.extend_from_slice(&new_hes);
            out_fids.push(fid);
        }

        // Global twin: (src,dst) → he for new + boundary, then pair reverses.
        let mut by_dir: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
        for (&(s, d), &he) in &boundary {
            if self.is_edge_live(he) && self.he(he).twin == NO_TWIN {
                by_dir.insert((s, d), he);
            }
        }
        for &he in &new_hes_all {
            if !self.is_edge_live(he) {
                continue;
            }
            let src = self.he_source(he);
            let dst = self.he(he).vertex;
            by_dir.insert((src.0, dst.0), he);
        }
        // SORTED (2026-08-09). `by_dir`'s CONTENT is already order-independent (one entry per
        // direction; the `new_hes_all` pass deterministically overrides a boundary entry with the
        // same key), and pairing is symmetric — processing `(s,d)` or `(d,s)` first forms the same
        // pair — so this loop's OUTCOME did not depend on the random `keys()` order. It is sorted
        // anyway so that determinism here is a property of the code rather than of an argument the
        // next reader has to reconstruct, and so a future non-symmetric rule (a first-wins tiebreak
        // on a fan, say) cannot silently reintroduce the free-list-order class of bug this pass sits
        // next to. Cost is one sort of the region's directed edges.
        let mut keys: Vec<(u32, u32)> = by_dir.keys().copied().collect();
        keys.sort_unstable();
        for (s, d) in keys {
            let Some(&he) = by_dir.get(&(s, d)) else {
                continue;
            };
            if !self.is_edge_live(he) || self.he(he).twin != NO_TWIN {
                continue;
            }
            if let Some(&twin) = by_dir.get(&(d, s)) {
                if self.is_edge_live(twin) && self.he(twin).twin == NO_TWIN && twin != he {
                    self.he_mut(he).twin = twin;
                    self.he_mut(twin).twin = he;
                }
            }
        }
        out_fids
    }

    /// Find a half-edge from src to dst, or None.
    pub(super) fn find_half_edge(&self, src: VertexId, dst: VertexId) -> Option<HalfEdgeId> {
        let fan = self.vertex_fan(src);
        for he in fan {
            if self.he(he).vertex == dst { return Some(he); }
        }
        None
    }

}

/// Regression guard for the 2026-08-09 free-list determinism fix (see [`HalfEdgeMesh::delete_faces`]).
/// Lives here rather than in `half_edge/tests.rs` because it asserts on `free_verts` / `free_edges` /
/// `free_faces`, which are `pub(in crate::half_edge)` allocator internals — the ORDER of those three
/// Vecs is the entire bug, and asserting on a public-API proxy for it would let the defect return
/// under a different symptom.
#[cfg(test)]
mod delete_determinism_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// The three free-lists IN ORDER — what `alloc_*` pops from, i.e. what decides every id the
    /// mesh hands out next.
    fn free_state(m: &HalfEdgeMesh) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        (m.free_verts.clone(), m.free_edges.clone(), m.free_faces.clone())
    }

    /// Whole-mesh image (topology + ids + positions + free-lists). `HalfEdgeMesh: Debug` is total,
    /// so this cannot silently stop covering a field the way a hand-written digest would.
    fn image(m: &HalfEdgeMesh) -> String {
        format!("{m:?}")
    }

    /// An 8x8 block of a 16x16 plane, as a FRESH `HashSet` each call. Fresh matters: every
    /// `HashSet` gets its own `RandomState`, so two sets holding the same ids iterate differently
    /// — which is exactly the input variation the fix has to absorb.
    fn block(rot: u32) -> HashSet<FaceId> {
        let mut s = HashSet::new();
        for k in 0..64u32 {
            // Insertion order varies with `rot` as well, so neither the hash seed nor the
            // insertion sequence is held constant across the runs being compared.
            let i = (k * 7 + rot * 13) % 64;
            s.insert(FaceId((i / 8) * 16 + (i % 8)));
        }
        s
    }

    #[test]
    fn delete_faces_is_the_sorted_delete_face_loop() {
        // The property `inset_region` depends on: batching the deletes must not move a single id.
        let base = HalfEdgeMesh::new_plane(4.0, 4.0, 16, 16);
        let sel = block(0);
        let mut ordered: Vec<FaceId> = sel.iter().copied().collect();
        ordered.sort_unstable_by_key(|f| f.0);

        let mut looped = base.clone();
        for &f in &ordered {
            looped.delete_face(f);
        }
        let mut batched = base.clone();
        batched.delete_faces(&sel);

        assert_eq!(
            free_state(&looped),
            free_state(&batched),
            "delete_faces must free in the same order as the sorted delete_face loop — \
             free-list order is LIFO-reused id order, so any difference shifts every later id"
        );
        assert_eq!(image(&looped), image(&batched), "batched delete diverged from the loop");
        assert!(batched.validate().is_ok(), "validate: {:?}", batched.validate());
    }

    #[test]
    fn delete_faces_ignores_hashset_iteration_order() {
        let base = HalfEdgeMesh::new_plane(4.0, 4.0, 16, 16);
        let mut first: Option<String> = None;
        for rot in 0..8u32 {
            let mut m = base.clone();
            m.delete_faces(&block(rot));
            assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
            let img = image(&m);
            match &first {
                None => first = Some(img),
                Some(f) => assert_eq!(
                    *f, img,
                    "delete_faces output depended on the caller HashSet's iteration order (rot {rot})"
                ),
            }
        }
    }

    #[test]
    fn delete_faces_region_ignores_caller_order() {
        // `delete_faces_region` takes an ITERATOR, and its one production caller (bevel) builds it
        // from a `HashSet<u32>`. Feeding the same ids ascending vs descending must land the same
        // mesh — this covers both the face sort AND the `boundary_out` replacement-pointer pick
        // (two separate HashMap instances iterate differently even inside one process).
        let base = HalfEdgeMesh::new_plane(4.0, 4.0, 16, 16);
        let ids: Vec<u32> = (0..64u32).map(|i| (i / 8) * 16 + (i % 8)).collect();
        let dying: HashSet<u32> = ids.iter().copied().collect();

        let run = |order: Vec<u32>| {
            let mut m = base.clone();
            let mut boundary: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
            m.delete_faces_region(order.into_iter().map(FaceId), &dying, &mut boundary);
            assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
            image(&m)
        };

        let mut asc = ids.clone();
        asc.sort_unstable();
        let mut desc = asc.clone();
        desc.reverse();
        let mut shuffled = ids.clone();
        shuffled.rotate_left(23);

        let a = run(asc);
        assert_eq!(a, run(desc), "delete_faces_region depended on caller order (descending)");
        assert_eq!(a, run(shuffled), "delete_faces_region depended on caller order (rotated)");
    }
}
