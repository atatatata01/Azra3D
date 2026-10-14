// Allocation / free chokepoints for mesh element slots (split from half_edge.rs — pure
// relocation). ONLY this file may touch the free_verts/free_edges/free_faces lists.

use super::*;

impl HalfEdgeMesh {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            half_edges: Vec::new(),
            faces: Vec::new(),
            free_verts: Vec::new(),
            free_edges: Vec::new(),
            free_faces: Vec::new(),
            free_index: FreeSlotIndex::default(),
        }
    }

    /// Re-derive the O(1) free-slot mirror from the authoritative `Vec` free-lists.
    ///
    /// The mirror (`free_index`) is `#[serde(skip)]`, so a DESERIALIZED mesh arrives with
    /// populated `free_*` Vecs and an empty mirror; `Deserialize` (types.rs) calls this so
    /// that state never escapes the decoder. Every other producer of a `HalfEdgeMesh`
    /// maintains the mirror incrementally through the `alloc_*` / `free_*` chokepoints
    /// below, and `Clone` copies it verbatim. The remaining hole — a struct literal or
    /// `Default` that hands over populated Vecs with an empty mirror — cannot produce a
    /// WRONG answer either: `FreeBitset::contains` compares cardinality in O(1) and falls
    /// back to the linear `Vec` scan, and the first `&mut` entry point calls `ensure`,
    /// which heals it. Correctness never depends on remembering to call this.
    pub(super) fn rebuild_free_index(&mut self) {
        self.free_index.verts.rebuild(&self.free_verts);
        self.free_index.edges.rebuild(&self.free_edges);
        self.free_index.faces.rebuild(&self.free_faces);
    }

    // ============================================================
    // Allocation helpers (with free-list reuse)
    // ============================================================

    /// THE single chokepoint for retiring a VERTEX slot. Idempotent by construction — the vertex twin of
    /// [`free_face`], with the identical rationale (read it). `alloc_vertex` pops `free_verts` with NO
    /// dedup, so a slot pushed TWICE is handed out by two separate `alloc_vertex()` calls: two live
    /// vertices, one VertexId, silently. Neither `validate()` nor `is_manifold()` can see it (both skip
    /// freed slots and walk only live topology). EVERY `free_verts` push in this file therefore goes
    /// through here. Do NOT push `free_verts` directly. (Cherry-picked from orphaned ad6393e; master
    /// had already grown the edge-side chokepoint independently.)
    ///
    /// Returns true if this call retired the slot, false if it was already free.
    /// Public for locality installs (bevel region) that must drop replaced corners without O(mesh) scans.
    pub fn free_vert(&mut self, v: VertexId) -> bool {
        debug_assert!(
            (v.0 as usize) < self.vertices.len(),
            "free_vert: VertexId({}) out of bounds (vertices.len() = {}) — caller bug",
            v.0, self.vertices.len()
        );
        if (v.0 as usize) >= self.vertices.len() { return false; }
        // O(1) via the free-slot mirror instead of `free_verts.contains` (linear). This guard
        // runs on EVERY free, so the linear form made retiring N vertices O(N^2).
        self.free_index.verts.ensure(&self.free_verts);
        if !self.is_vert_live(v) { return false; }
        self.vertices[v.0 as usize].half_edge = NO_EDGE;
        self.free_verts.push(v.0);
        self.free_index.verts.insert(v.0);
        true
    }

    pub fn alloc_vertex(&mut self, v: HVertex) -> VertexId {
        // LIFO `pop()` order is UNCHANGED — the mirror only records which slots are free, it
        // never reorders the list (id assignment, and every op's output ids, stay identical).
        self.free_index.verts.ensure(&self.free_verts);
        if let Some(idx) = self.free_verts.pop() {
            self.free_index.verts.remove(idx);
            self.vertices[idx as usize] = v;
            VertexId(idx)
        } else {
            let id = VertexId(self.vertices.len() as u32);
            self.vertices.push(v);
            id
        }
    }

    pub fn alloc_half_edge(&mut self, he: HalfEdge) -> HalfEdgeId {
        self.free_index.edges.ensure(&self.free_edges);
        if let Some(idx) = self.free_edges.pop() {
            self.free_index.edges.remove(idx);
            self.half_edges[idx as usize] = he;
            HalfEdgeId(idx)
        } else {
            let id = HalfEdgeId(self.half_edges.len() as u32);
            self.half_edges.push(he);
            id
        }
    }

    pub(super) fn alloc_face(&mut self, f: HFace) -> FaceId {
        self.free_index.faces.ensure(&self.free_faces);
        if let Some(idx) = self.free_faces.pop() {
            self.free_index.faces.remove(idx);
            self.faces[idx as usize] = f;
            FaceId(idx)
        } else {
            let id = FaceId(self.faces.len() as u32);
            self.faces.push(f);
            id
        }
    }

    /// Pre-reserve element Vec capacity for an in-place edit. Builders pack Vecs to exact
    /// length; the first `push` then reallocates **the whole mesh** (O(mesh) memcpy) — that
    /// alone was ~40 ms of extrude_face XL and masked true ring-local cost. Call once with
    /// a tight upper bound before bulk allocs so cost stays ∝ EDIT size.
    pub fn reserve_elements(&mut self, verts: usize, half_edges: usize, faces: usize) {
        self.vertices.reserve(verts);
        self.half_edges.reserve(half_edges);
        self.faces.reserve(faces);
    }

    /// THE single chokepoint for retiring a face slot. Idempotent by construction.
    ///
    /// `alloc_face` above pops `free_faces` with no dedup, so a slot pushed TWICE is handed out by two
    /// separate `create_face()` calls: two live faces, one FaceId, silently. Neither `validate()` nor
    /// `is_manifold()` can see this (both walk next/prev/twin round-trips and live-face invariants — a
    /// duplicated free slot violates neither), so no gate anywhere downstream catches the corruption.
    ///
    /// Guarding the CALLERS was the previous contract and it does not hold: it is unenforceable tribal
    /// knowledge that every present and future op must independently re-derive (CLAUDE.md "No Bandaids"
    /// Q3 — "enforce correctness structurally"). EVERY `free_faces` push in this file therefore goes
    /// through here. That is what makes double-freeing a face impossible rather than merely
    /// currently-absent. Do NOT push `free_faces` directly.
    ///
    /// Returns true if this call retired the slot, false if it was already free.
    pub(super) fn free_face(&mut self, f: FaceId) -> bool {
        // Two DIFFERENT conditions, deliberately not merged. Out-of-bounds is a CALLER BUG and must not
        // be laundered into the idempotent path: before this fn existed, `delete_face(oob)` panicked at
        // `face()`'s direct index, and silently swallowing that would trade a loud caller bug for a quiet
        // one (CLAUDE.md: "Wrapping in Option and silently returning None where a value should always
        // exist — assert instead"). debug_assert keeps the dev-build signal; the release branch still
        // returns false rather than panicking, because this fn's whole contract is to be safely callable.
        debug_assert!(
            (f.0 as usize) < self.faces.len(),
            "free_face: FaceId({}) out of bounds (faces.len() = {}) — caller bug",
            f.0, self.faces.len()
        );
        if (f.0 as usize) >= self.faces.len() { return false; }
        // The legitimate idempotent case: already retired. NOT an error — that is the whole point.
        // O(1) via the free-slot mirror; `free_faces.contains` here was linear, so deleting N
        // faces cost O(N^2) in the guard alone.
        self.free_index.faces.ensure(&self.free_faces);
        if !self.is_face_live(f) { return false; }
        self.faces[f.0 as usize].half_edge = NO_EDGE;
        self.free_faces.push(f.0);
        self.free_index.faces.insert(f.0);
        true
    }

    /// THE single chokepoint for retiring a half-edge slot — the edge-side mirror of `free_face`.
    ///
    /// Before this existed every op open-coded its own `free_edges.push`, and the freed-slot STATE was
    /// inconsistent: `dissolve_edge` zeroed next/prev/twin to a canonical dead state while `delete_face`,
    /// `delete_edge`, and `collapse_self_loops` left next/prev DANGLING at other (also-freed) ring
    /// members. A dangling freed slot is still walkable, so any consumer that iterates
    /// `0..half_edges.len()` and dereferences a slot (the viewport wire overlay, `he_source`) reads a
    /// live-looking-but-dead half-edge — the source of the repeating `[WARN] he_source: no valid prev or
    /// twin` spam the owner sees during dissolve/delete.
    ///
    /// Blender never hits this class: `BM_edge_kill`/`BM_loop_kill` unlink the element from every cycle
    /// (loop, radial, disk) and return it to the mempool, so a freed element is unreachable by
    /// construction and iteration walks only LIVE pools. This is AZRA's structural equivalent — EVERY
    /// `free_edges` push goes through here, which zeroes the slot to ONE canonical dead state so
    /// `is_edge_live` (and every consumer built on it) recognizes a dead slot uniformly. Do NOT push
    /// `free_edges` directly.
    ///
    /// Returns true if this call retired the slot, false if it was already free.
    pub(super) fn free_edge(&mut self, he: HalfEdgeId) -> bool {
        // Mirrors `free_face`: out-of-bounds is a CALLER BUG (debug_assert keeps the dev signal); the
        // release branch returns false rather than panicking because this fn's whole contract is to be
        // safely callable. NOT folded into the idempotent path — that would launder a loud caller bug.
        debug_assert!(
            (he.0 as usize) < self.half_edges.len(),
            "free_edge: HalfEdgeId({}) out of bounds (half_edges.len() = {}) — caller bug",
            he.0, self.half_edges.len()
        );
        if (he.0 as usize) >= self.half_edges.len() { return false; }
        // The legitimate idempotent case: already retired. NOT an error — that is the whole point.
        // O(1) via the free-slot mirror; `free_edges.contains` here was linear and it runs on
        // EVERY free, so `delete_face` over N faces was O(N^2) in this guard alone.
        self.free_index.edges.ensure(&self.free_edges);
        if !self.is_edge_live(he) { return false; }
        // Canonical dead state — what a freed slot must look like EVERYWHERE so no consumer mistakes it
        // for a live half-edge. `alloc_half_edge` fully overwrites on reuse, so reuse stays sound.
        self.half_edges[he.0 as usize] = HalfEdge {
            vertex: VertexId(0), twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: NO_FACE,
        };
        self.free_edges.push(he.0);
        self.free_index.edges.insert(he.0);
        true
    }

    /// Is `he` a LIVE half-edge (in range and not on the free-list)? The canonical predicate every
    /// consumer that iterates half-edges MUST use before dereferencing a slot — a dead/OOB slot carries
    /// no valid source, twin, or endpoints. Replaces open-coded `!free_edges.contains(&i)` (which
    /// consumers outside this module cannot even write — `free_edges` is private) and lets the viewport
    /// overlays skip freed slots instead of drawing garbage lines from them.
    ///
    /// **O(1)** since 2026-08-09: the free-list membership test goes through the
    /// [`FreeBitset`] mirror instead of `Vec::contains`. It used to be O(free-list), which
    /// made every `for i in 0..half_edges.len() { if is_edge_live(..) }` sweep in the kernel
    /// O(HE x free_len) — quadratic on a heavily edited mesh.
    #[inline]
    pub fn is_edge_live(&self, he: HalfEdgeId) -> bool {
        he != NO_EDGE
            && (he.0 as usize) < self.half_edges.len()
            && !self.free_index.edges.contains(&self.free_edges, he.0)
    }

    /// Is `f` a LIVE face (in range and not on the free-list)? The face-side mirror of
    /// [`is_edge_live`], and the canonical predicate for any consumer that iterates
    /// `0..faces.len()` — a dead slot's `half_edge` is `NO_EDGE` and walking it yields nothing.
    ///
    /// Exists because `free_faces` is PRIVATE, so out-of-module consumers (the polygroup purge in
    /// `poly_mesh.rs`, viewport overlays) cannot write the open-coded `!free_faces.contains(&i)`
    /// check at all. Added rather than making the field public: the free-lists are an allocator
    /// detail, and `alloc_face`'s LIFO `pop()` reuse depends on their exact order — exposing them
    /// invites a consumer to mutate or reorder that.
    ///
    /// **O(1)** since 2026-08-09 — see [`HalfEdgeMesh::is_edge_live`].
    #[inline]
    pub fn is_face_live(&self, f: FaceId) -> bool {
        (f.0 as usize) < self.faces.len() && !self.free_index.faces.contains(&self.free_faces, f.0)
    }

    /// Is `v` a LIVE vertex (in range and not on the free-list)? The vertex-side mirror of
    /// [`is_edge_live`] / [`is_face_live`], closing the gap where edges and faces had a liveness
    /// predicate but vertices did not — so consumers holding a possibly-stale `VertexId` (after a
    /// dissolve/delete) literally could not check it (`free_verts` is private).
    ///
    /// **O(1)** since 2026-08-09 — the follow-up this doc-comment used to promise ("a
    /// bitset/generation upgrade for all three predicates") has LANDED: all three now go
    /// through the [`FreeBitset`] mirror. Hot loops that hoist their own `HashSet`
    /// (`bounding_box`, `recompute_normals`, `validate`) are no longer obliged to — the
    /// predicate itself is now cheaper than a hash lookup — but hoisting is still harmless.
    #[inline]
    pub fn is_vert_live(&self, v: VertexId) -> bool {
        (v.0 as usize) < self.vertices.len() && !self.free_index.verts.contains(&self.free_verts, v.0)
    }

}
