// validate / is_manifold / counts / content_signature (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    /// Debug: validate topology invariants.
    pub fn validate(&self) -> Result<(), String> {
        // Linear `Vec::contains` per half-edge → O(E x free-list). Hoisted; see `recompute_normals`.
        // This one matters most: `validate()` gates every poly op's result (`mesh_ops::compute`), so
        // it runs on the interactive edit path, on the mesh that has just been edited.
        let free_e: std::collections::HashSet<u32> = self.free_edges.iter().copied().collect();
        for (i, he) in self.half_edges.iter().enumerate() {
            if free_e.contains(&(i as u32)) { continue; }
            let id = HalfEdgeId(i as u32);
            // next/prev consistency
            if self.he(he.next).prev != id {
                return Err(format!("HalfEdge {}: next.prev != self", i));
            }
            if self.he(he.prev).next != id {
                return Err(format!("HalfEdge {}: prev.next != self", i));
            }
            // twin consistency
            if he.twin != NO_TWIN {
                if self.he(he.twin).twin != id {
                    return Err(format!("HalfEdge {}: twin.twin != self", i));
                }
            }
        }
        Ok(())
    }

    /// Manifold / degeneracy classifier — the checks `validate()` is BLIND to.
    ///
    /// `validate()` only proves twin symmetry + next/prev cycles; a mesh can pass it and still be
    /// non-manifold, degenerate, or carry a stale `vertex.half_edge` (the 2026-07-07 audit's #1 root
    /// cause — boolean mesh-break + delete corruption both slipped through the twin-consistent guard).
    /// This is the geometric-integrity gate the boolean install + edit-op undo snapshots must run so
    /// broken output is REJECTED (source mesh left intact) instead of installed. Detects:
    ///   (a) an undirected edge with >2 incident faces, or a directed edge used by 2 faces with the SAME
    ///       winding (the coplanar-boolean non-manifold pair that twins cleanly);
    ///   (b) a live face with <3 vertices, a repeated vertex, or a self-loop half-edge (`dst == src`);
    ///   (c) a live `vertex.half_edge` that points at a FREED / out-of-range edge, or a live edge not
    ///       sourced at that vertex (the `delete_face` dangling-pointer bug);
    ///   (d) a bowtie / fan-junction vertex whose incident faces form >1 disjoint fan (edge-manifold but
    ///       vertex-non-manifold — the case a complex boolean produces and vertex_fan silently mis-walks).
    /// Returns `Ok(())` for a clean manifold mesh (open or closed); `Err(reason)` on the first violation.
    /// CORRUPTION-ONLY subset of `is_manifold()` — checks (b) face degeneracy and (c) stale
    /// vertex disk pointers. These states are never legitimate: they are broken data structures.
    ///
    /// Free every live face whose ring is not a real polygon: fewer than 3 verts, or a
    /// repeated vertex (self-loop / digon). Returns how many faces were retired.
    ///
    /// Owner 2026-08-06: a free-knife **slice** can leave a digon (`degenerate face N: only
    /// 2 vertices`). That poison then made Ctrl+X refuse EVERY face delete with
    /// `op produced invalid mesh: degenerate face …` even on a good selection. Knife
    /// commit and the edit-op gate both call this before `integrity()` so a single digon
    /// cannot lock the mesh.
    pub fn purge_degenerate_faces(&mut self) -> usize {
        use std::collections::HashSet;
        let mut purged = 0usize;
        // Collect first — `delete_face` mutates free_faces while we walk.
        let mut bad: Vec<FaceId> = Vec::new();
        for i in 0..self.faces.len() as u32 {
            if self.free_faces.contains(&i) {
                continue;
            }
            let f = FaceId(i);
            let vs = self.face_vertices(f);
            if vs.len() < 3 {
                bad.push(f);
                continue;
            }
            // Degenerate = a ring that is not a real polygon: fewer than 3 DISTINCT
            // corners (digon / self-loop) or a zero-length edge (adjacent repeat).
            // A NON-adjacent repeated vertex is NOT degenerate: a knife slit ring
            // `[.., a, i0..ik, i_{k-1}..i0, a, ..]` revisits its anchor through
            // distinct spur edges by design (KNIFE.md §1, Blender dangling-cut
            // parity), and `validate()` accepts it. The old any-repeat rule silently
            // DELETED every slit-carrying face on the classic knife commit — the
            // owner "classic S deletes faces" P1 (2026-08-07).
            let mut seen = HashSet::new();
            let unique = vs.iter().filter(|v| seen.insert(v.0)).count();
            let zero_len_edge = (0..vs.len()).any(|k| vs[k] == vs[(k + 1) % vs.len()]);
            if unique < 3 || zero_len_edge {
                bad.push(f);
            }
        }
        for f in bad {
            // Already free → no-op; delete_face frees the face's half-edges so a digon
            // does not leave a 2-cycle in the twin graph.
            self.delete_face(f);
            purged += 1;
        }
        purged
    }

    /// Re-point any live vertex whose `half_edge` is freed, OOB, or not sourced at that
    /// vertex — same repair `delete_face` does for corners, applied to the whole mesh.
    /// Returns how many vertices were fixed.
    pub fn repair_stale_vertex_disks(&mut self) -> usize {
        let mut fixed = 0usize;
        for i in 0..self.vertices.len() as u32 {
            if self.free_verts.contains(&i) {
                continue;
            }
            let v = VertexId(i);
            let h = self.vert(v).half_edge;
            let stale = h == NO_EDGE
                || h.0 as usize >= self.half_edges.len()
                || self.free_edges.contains(&h.0)
                || self.he_source(h) != v;
            // NO_EDGE on a live vert is allowed (loose); only fix when pointer is wrong.
            if h != NO_EDGE
                && (h.0 as usize >= self.half_edges.len()
                    || self.free_edges.contains(&h.0)
                    || self.he_source(h) != v)
            {
                let mut repl = NO_EDGE;
                for j in 0..self.half_edges.len() as u32 {
                    if self.free_edges.contains(&j) {
                        continue;
                    }
                    if self.he_source(HalfEdgeId(j)) == v {
                        repl = HalfEdgeId(j);
                        break;
                    }
                }
                self.vert_mut(v).half_edge = repl;
                fixed += 1;
            } else if stale && h != NO_EDGE {
                // unreachable branch kept for clarity — covered above
                let _ = stale;
            }
        }
        fixed
    }

    /// Heal poison that blocks edit ops on dirty bodies (owner 2026-08-06: "Ctrl+X should
    /// ALWAYS work even if mesh not clean — usually you delete faces to clean up bad geo").
    ///
    /// Order: purge digons → repair vertex disks → purge again (repairs can expose empty rings).
    /// Returns total faces purged + verts repaired (informational).
    pub fn heal_for_edit(&mut self) -> usize {
        let mut n = self.purge_degenerate_faces();
        n += self.repair_stale_vertex_disks();
        n += self.purge_degenerate_faces();
        n
    }

    /// Split out 2026-08-03 (owner directive: "all tools must work even on open / non-manifold
    /// shapes — I merged vertices on an open sheet and got a manifold error"). Checks (a)
    /// edge multiplicity and (d) bowtie vertices describe LEGAL Blender-representable geometry
    /// (a merge of two boundary verts routinely creates a bowtie), so EDIT-op result gates now
    /// require only THIS; full `is_manifold()` remains the bar for operations that genuinely
    /// need solids (boolean install / BSP preflight).
    pub fn integrity(&self) -> Result<(), String> {
        // FLAT, allocation-light (2026-08-09 perf pass — this is the first half of every
        // `is_manifold()` / `is_edge_manifold()` call, i.e. of every boolean-install and edit-op
        // result gate). Three structures went away, none of the four checks did:
        //   * the hoisted `HashSet`s of `free_edges`/`free_verts`/`free_faces` — `is_face_live` /
        //     `is_vert_live` / `is_edge_live` have been O(1) (`FreeBitset`) since 2026-08-09, so
        //     building three hash sets to answer what a bit test answers was pure loss;
        //   * one `Vec<VertexId>` allocation PER FACE — `face_vertices_into` (the existing
        //     buffer-taking form of `face_vertices`) fills one reused ring buffer instead;
        //   * one `HashSet` PER FACE for the repeated-corner test — replaced by a stamped
        //     `Vec` indexed by vertex id, which is what a dense id space wants.
        // Accept/reject and every message are unchanged, including WHICH violation is reported
        // first (both loops still run in ascending id order, rings still in ring order).
        let mut ring: Vec<VertexId> = Vec::new();
        // Repeated-corner detector: `seen_epoch[v] == epoch` ⇔ v already appeared in THIS ring.
        // Indexed by vertex id, which is a dense index into `self.vertices` by kernel contract.
        // A corner id outside that range — which no kernel-built mesh can produce — falls back to
        // `seen_oob`, so the array is always sized by the MESH and never by an id value.
        let mut seen_epoch: Vec<u32> = vec![0; self.vertices.len()];
        let mut seen_oob: Vec<u32> = Vec::new();

        // (b) Face degeneracy — <3 verts, repeated vertex, or a self-loop half-edge.
        for i in 0..self.faces.len() as u32 {
            let f = FaceId(i);
            if !self.is_face_live(f) { continue; }
            self.face_vertices_into(f, &mut ring);
            if ring.len() < 3 {
                return Err(format!("degenerate face {}: only {} vertices", i, ring.len()));
            }
            let epoch = i + 1; // 0 = "never stamped"; face ids ascend, so epochs never collide
            seen_oob.clear();
            for v in &ring {
                let repeated = if (v.0 as usize) < seen_epoch.len() {
                    let slot = &mut seen_epoch[v.0 as usize];
                    let hit = *slot == epoch;
                    *slot = epoch;
                    hit
                } else {
                    let hit = seen_oob.contains(&v.0);
                    if !hit { seen_oob.push(v.0); }
                    hit
                };
                if repeated {
                    return Err(format!("degenerate face {}: repeated vertex {} (self-loop / pinch)", i, v.0));
                }
            }
        }

        // (c) Vertex disk pointer — no dangling / mislinked vertex.half_edge.
        for i in 0..self.vertices.len() as u32 {
            if !self.is_vert_live(VertexId(i)) { continue; }
            let h = self.vert(VertexId(i)).half_edge;
            if h == NO_EDGE { continue; } // loose vertex (no incident edge) — allowed, not corruption
            // `is_edge_live` is exactly the old `h.0 >= half_edges.len() || free_edges.contains(&h.0)`
            // test negated (the `h == NO_EDGE` arm it also carries is already handled above).
            if !self.is_edge_live(h) {
                return Err(format!("stale vertex.half_edge: vertex {} -> freed/OOB edge {}", i, h.0));
            }
            if self.he_source(h).0 != i {
                return Err(format!("mislinked vertex.half_edge: vertex {} -> edge {} sourced at {}", i, h.0, self.he_source(h).0));
            }
        }
        Ok(())
    }

    pub fn is_manifold(&self) -> Result<(), String> {
        self.manifold_check(true)
    }

    /// EDGE-manifold check: integrity + (a) edge multiplicity, but bowtie vertices (d) are
    /// PERMITTED. A vertex-touch boolean legitimately produces a bowtie — Blender ships
    /// those (contact-matrix vertex_touch cell) — and a boolean result that is watertight,
    /// fold-free and volume-exact must not be discarded for one (owner test-02, 2026-08-04:
    /// a perfect weld-collapse candidate was rejected on "2 disjoint fans (bowtie)").
    pub fn is_edge_manifold(&self) -> Result<(), String> {
        self.manifold_check(false)
    }

    /// FLAT-INDEXED since 2026-08-09. Vertex, half-edge and face ids are DENSE `u32` indices into
    /// `self.vertices` / `self.half_edges` / `self.faces`, so every structure here is a `Vec`
    /// indexed by an id. The three `HashMap`s it replaced (`directed` + `undirected` keyed by a
    /// `(u32,u32)` tuple, `by_source` keyed by a vertex with one `Vec` allocated per vertex) put
    /// ~3 x E tuple keys through SipHash, and the vertex-fan union-find allocated a FRESH
    /// `HashMap` INSIDE the per-vertex loop — one map per vertex. Measured on a 9216-face grid:
    /// 8.44 ms -> see §perf in the commit; `validate()` (the twin/cycle check) costs 0.025 ms, and
    /// this runs 6-15 times per boolean (`post_boolean_polish` gates all 5 of its steps on it).
    ///
    /// Nothing about WHAT is accepted or rejected changed. What did change, deliberately: the
    /// reported violation is now DETERMINISTIC (lowest id first). It used to be whichever key
    /// `HashMap` iteration reached first, i.e. it could differ between two runs on the same mesh.
    fn manifold_check(&self, reject_bowtie: bool) -> Result<(), String> {
        // (b) + (c) first — corruption is corruption regardless of manifoldness.
        self.integrity()?;

        const SKIP: u32 = u32::MAX; // "this half-edge carries no face incidence"
        let nhe = self.half_edges.len();

        // ---------------------------------------------------------------------------------
        // Pass 1 — endpoints of every LIVE, FACE-BEARING half-edge, in a dense vertex domain.
        // ---------------------------------------------------------------------------------
        // `dense_vid` is the IDENTITY for every mesh the kernel can build (`he.vertex` is always
        // an index into `self.vertices`). It exists so the id-indexed arrays below are sized by
        // the MESH and never by an id VALUE: a corrupted out-of-range endpoint — which the old
        // `HashMap` simply counted — is interned into a fresh dense slot instead of forcing a
        // `max_id`-sized allocation. `alias` stays empty in every real case.
        fn dense_vid(x: u32, n_base: u32, alias: &mut Vec<u32>) -> u32 {
            if x < n_base { return x; }
            if let Some(k) = alias.iter().position(|&a| a == x) { return n_base + k as u32; }
            alias.push(x);
            n_base + alias.len() as u32 - 1
        }
        fn orig_vid(x: u32, n_base: u32, alias: &[u32]) -> u32 {
            if x < n_base { x } else { alias[(x - n_base) as usize] }
        }
        let n_base = self.vertices.len() as u32;
        let mut alias: Vec<u32> = Vec::new();
        let mut src: Vec<u32> = vec![SKIP; nhe];
        let mut dst: Vec<u32> = vec![0u32; nhe];
        let mut n_items = 0usize;
        for i in 0..nhe as u32 {
            let h = HalfEdgeId(i);
            if !self.is_edge_live(h) { continue; }
            if self.he(h).face == NO_FACE { continue; } // boundary loop half-edge — not a face incidence
            src[i as usize] = dense_vid(self.he_source(h).0, n_base, &mut alias);
            dst[i as usize] = dense_vid(self.he(h).vertex.0, n_base, &mut alias);
            n_items += 1;
        }

        // ---------------------------------------------------------------------------------
        // CSR by source vertex (counting sort) — the flat replacement for `by_source`.
        // `items[start[v] .. start[v+1]]` = the outgoing face-bearing half-edges at v, packed as
        // (dense destination << 32 | half-edge id) and sorted, so a directed edge's incidences are
        // one contiguous run and a neighbour lookup is a short scan with an early break.
        // ---------------------------------------------------------------------------------
        let n_v = n_base as usize + alias.len();
        let mut start: Vec<u32> = vec![0; n_v + 1];
        for i in 0..nhe {
            if src[i] != SKIP { start[src[i] as usize + 1] += 1; }
        }
        for v in 0..n_v { start[v + 1] += start[v]; }
        let mut items: Vec<u64> = vec![0; n_items];
        {
            let mut cursor = start.clone();
            for i in 0..nhe {
                let s = src[i];
                if s == SKIP { continue; }
                let p = cursor[s as usize] as usize;
                items[p] = ((dst[i] as u64) << 32) | i as u64;
                cursor[s as usize] += 1;
            }
        }
        for v in 0..n_v {
            let g = &mut items[start[v] as usize..start[v + 1] as usize];
            // A group is one vertex fan — 3..8 entries on real geometry, where an insertion sort
            // beats the generic sort's setup outright. The threshold hands pathological fans
            // (a cone apex with thousands of spokes) back to pdqsort so this stays O(k log k).
            if g.len() > 24 {
                g.sort_unstable();
            } else {
                for x in 1..g.len() {
                    let cur = g[x];
                    let mut y = x;
                    while y > 0 && g[y - 1] > cur { g[y] = g[y - 1]; y -= 1; }
                    g[y] = cur;
                }
            }
        }
        /// Incidences of the DIRECTED edge v->w (0 when v names no group).
        fn dir_count(start: &[u32], items: &[u64], v: u32, w: u32) -> u32 {
            let vi = v as usize;
            if vi + 1 >= start.len() { return 0; }
            let mut c = 0u32;
            for &it in &items[start[vi] as usize..start[vi + 1] as usize] {
                let d = (it >> 32) as u32;
                if d == w { c += 1; } else if d > w { break; }
            }
            c
        }

        // (a) Edge manifoldness — a DIRECTED edge used by 2 faces means two faces with the SAME
        // winding (the coplanar-boolean pair that twins cleanly); an UNDIRECTED edge with >2
        // incident faces is the classic "book spine". Ascending source, then ascending
        // destination, so the FIRST violation reported is the lowest-id one.
        for v in 0..n_v as u32 {
            let g = &items[start[v as usize] as usize..start[v as usize + 1] as usize];
            let mut k = 0usize;
            while k < g.len() {
                let d = (g[k] >> 32) as u32;
                let mut j = k + 1;
                while j < g.len() && (g[j] >> 32) as u32 == d { j += 1; }
                if j - k > 1 {
                    return Err(format!("non-manifold: directed edge {}->{} used by {} faces (same winding)",
                        orig_vid(v, n_base, &alias), orig_vid(d, n_base, &alias), j - k));
                }
                k = j;
            }
        }
        // NOTE (2026-08-09): this second check is UNREACHABLE given the first, and is kept anyway
        // rather than silently dropped. undirected{u,v} == directed(u->v) + directed(v->u), and the
        // loop above already returned unless BOTH are <= 1, so the total can never exceed 2. It is
        // retained because it states the invariant the caller actually depends on ("no edge with
        // three faces"), it costs one short neighbour scan per edge, and dropping a documented
        // manifoldness check to save that is not a trade this gate should make.
        for v in 0..n_v as u32 {
            let g = &items[start[v as usize] as usize..start[v as usize + 1] as usize];
            let mut k = 0usize;
            while k < g.len() {
                let w = (g[k] >> 32) as u32;
                let mut j = k + 1;
                while j < g.len() && (g[j] >> 32) as u32 == w { j += 1; }
                let c_fwd = (j - k) as u32;
                k = j;
                let total = if w == v {
                    c_fwd // self-loop: the forward run IS every incidence of {v,v}
                } else {
                    let c_rev = dir_count(&start, &items, w, v);
                    if w < v && c_rev > 0 { continue; } // already evaluated at the lower endpoint
                    c_fwd + c_rev
                };
                if total > 2 {
                    let (ov, ow) = (orig_vid(v, n_base, &alias), orig_vid(w, n_base, &alias));
                    let (a, b) = if ov <= ow { (ov, ow) } else { (ow, ov) };
                    return Err(format!("non-manifold: undirected edge {{{},{}}} incident to {} faces", a, b, total));
                }
            }
        }

        // (d) Vertex (disk) manifoldness — the faces incident to a vertex must form ONE connected fan.
        // A bowtie / fan-junction vertex (two disjoint fans meeting at a single point) is edge-manifold
        // (every check above passes) but vertex-NON-manifold; vertex_fan()/find_half_edge() then silently
        // traverse only one fan, so downstream ops (boolean install, extrude, dissolve) corrupt. Union the
        // incident faces via the shared edges radiating from the vertex; >1 component ⇒ non-manifold vertex.
        if !reject_bowtie { return Ok(()); }

        // ONE union-find for the whole check instead of one `HashMap` per vertex. `fslot` maps a
        // face id to a LOCAL node index valid only for the current vertex; `fstamp` carries the
        // epoch that made it valid, so "reset" is a counter bump, not an O(F) clear. `parent` is
        // the local forest (a few nodes), cleared and refilled per vertex.
        // `fslot`/`fstamp` are sized by `self.faces`; an out-of-range face id — again impossible
        // for a kernel-built mesh — takes the tiny `oob_face` side list for the same reason
        // `dense_vid` exists.
        fn face_node(
            f: u32, epoch: u32, nf: usize,
            fstamp: &mut [u32], fslot: &mut [u32], oob_face: &mut Vec<(u32, u32)>, parent: &mut Vec<u32>,
        ) -> u32 {
            if (f as usize) < nf {
                let i = f as usize;
                if fstamp[i] == epoch { return fslot[i]; }
                let k = parent.len() as u32;
                parent.push(k);
                fstamp[i] = epoch;
                fslot[i] = k;
                k
            } else {
                if let Some(&(_, k)) = oob_face.iter().find(|&&(id, _)| id == f) { return k; }
                let k = parent.len() as u32;
                parent.push(k);
                oob_face.push((f, k));
                k
            }
        }
        fn uf_find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                parent[x as usize] = parent[parent[x as usize] as usize]; // path halving
                x = parent[x as usize];
            }
            x
        }
        let nf = self.faces.len();
        let mut fstamp: Vec<u32> = vec![0; nf];
        let mut fslot: Vec<u32> = vec![0; nf];
        let mut oob_face: Vec<(u32, u32)> = Vec::new();
        let mut parent: Vec<u32> = Vec::new();
        let mut root_seen: Vec<bool> = Vec::new();
        let mut epoch = 0u32; // 0 = "never stamped"; the first vertex uses 1
        for v in 0..n_v as u32 {
            let g = &items[start[v as usize] as usize..start[v as usize + 1] as usize];
            if g.len() < 2 { continue; }
            let ov = orig_vid(v, n_base, &alias);
            // Exactly the old `free_verts.contains(&v)`: skip a FREED vertex, but not an
            // out-of-range one (the free-list never holds an out-of-range id).
            if (ov as usize) < self.vertices.len() && !self.is_vert_live(VertexId(ov)) { continue; }

            epoch += 1;
            parent.clear();
            oob_face.clear();
            for &it in g {
                let f = self.he(HalfEdgeId(it as u32)).face.0;
                face_node(f, epoch, nf, &mut fstamp, &mut fslot, &mut oob_face, &mut parent);
            }
            for &it in g {
                let h = HalfEdgeId(it as u32);
                let f = self.he(h).face.0;
                let t = self.he(h).twin;
                if t != NO_TWIN {
                    let tf = self.he(t).face;
                    if tf != NO_FACE {
                        let a = face_node(f, epoch, nf, &mut fstamp, &mut fslot, &mut oob_face, &mut parent);
                        let b = face_node(tf.0, epoch, nf, &mut fstamp, &mut fslot, &mut oob_face, &mut parent);
                        let (ra, rb) = (uf_find(&mut parent, a), uf_find(&mut parent, b));
                        if ra != rb { parent[ra as usize] = rb; }
                    }
                }
            }
            root_seen.clear();
            root_seen.resize(parent.len(), false);
            let mut fans = 0usize;
            for &it in g {
                let f = self.he(HalfEdgeId(it as u32)).face.0;
                let a = face_node(f, epoch, nf, &mut fstamp, &mut fslot, &mut oob_face, &mut parent);
                let r = uf_find(&mut parent, a) as usize;
                if !root_seen[r] { root_seen[r] = true; fans += 1; }
            }
            if fans > 1 {
                return Err(format!("non-manifold vertex {}: incident faces form {} disjoint fans (bowtie)", ov, fans));
            }
        }

        Ok(())
    }

    /// Debug-only mesh integrity check. Panics with context on any invariant violation.
    #[cfg(debug_assertions)]
    pub fn validate_mesh(&self, context: &str) {
        // Hoisted free-list tests; see `recompute_normals`. Debug-only, but it runs on every
        // debug-build edit, so an O(E x free-list) scan here is felt directly while modelling.
        let free_e: std::collections::HashSet<u32> = self.free_edges.iter().copied().collect();
        let free_f: std::collections::HashSet<u32> = self.free_faces.iter().copied().collect();
        for (i, he) in self.half_edges.iter().enumerate() {
            if free_e.contains(&(i as u32)) { continue; }
            // Twin symmetry
            if he.twin != NO_TWIN {
                let twin = &self.half_edges[he.twin.0 as usize];
                assert_eq!(twin.twin, HalfEdgeId(i as u32),
                    "[{}] HE{} twin symmetry broken", context, i);
            }
            // No NaN
            let v = &self.vertices[he.vertex.0 as usize];
            assert!(!v.position.is_nan(),
                "[{}] HE{} points to NaN vertex {}", context, i, he.vertex.0);
            // Valid next/prev cycle
            assert!(he.next != NO_EDGE,
                "[{}] HE{} has no next", context, i);
            assert!(he.prev != NO_EDGE,
                "[{}] HE{} has no prev", context, i);
        }
        // Face validation
        for i in 0..self.faces.len() {
            if free_f.contains(&(i as u32)) { continue; }
            let verts = self.face_vertices(FaceId(i as u32));
            assert!(verts.len() >= 3,
                "[{}] Face {} has {} verts", context, i, verts.len());
        }
    }

    /// Number of active vertices.
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() - self.free_verts.len()
    }

    /// Number of active faces.
    pub fn face_count(&self) -> usize {
        self.faces.len() - self.free_faces.len()
    }

    /// Number of active edges (half_edges / 2, approximately).
    pub fn edge_count(&self) -> usize {
        let active = self.half_edges.len() - self.free_edges.len();
        active / 2
    }

    /// A cheap O(V+E+F) fingerprint of everything the DISPLAY (rasterised) mesh depends on:
    /// array sizes, vertex positions + stored NORMALS, connectivity, per-face normals + material.
    /// Two meshes with the same signature retessellate to the same triangles, so a display cache
    /// can safely skip a (potentially expensive) modifier re-evaluation when the signature is
    /// unchanged.
    ///
    /// It is a pure read over the existing arrays — NOT a serialized field, so the on-disk shape
    /// is untouched. It deliberately covers BOTH in-place edits (`Arc::make_mut` moves a vertex
    /// without changing any count) and topological edits, so a pointer/identity check alone (which
    /// misses in-place moves) is not enough — this is. Hashing the small base cage, not the
    /// subdivided output, keeps the cost negligible next to what it guards.
    ///
    /// STORED NORMALS are part of the fingerprint on purpose: the `Solidify` modifier reads stored
    /// FACE normals (`compute_vertex_normal` → `face.normal`), so a "Recalculate Normals" op that
    /// rewrites normals WITHOUT moving any vertex/topology/material must still invalidate the cache
    /// — otherwise the corrected shell never reaches the display (a real latent bug this closes).
    pub fn content_signature(&self) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write_usize(self.vertices.len());
        h.write_usize(self.half_edges.len());
        h.write_usize(self.faces.len());
        for v in &self.vertices {
            h.write_u32(v.position.x.to_bits());
            h.write_u32(v.position.y.to_bits());
            h.write_u32(v.position.z.to_bits());
            h.write_u32(v.normal.x.to_bits());
            h.write_u32(v.normal.y.to_bits());
            h.write_u32(v.normal.z.to_bits());
        }
        for e in &self.half_edges {
            h.write_u32(e.vertex.0);
            h.write_u32(e.next.0);
        }
        for f in &self.faces {
            h.write_u32(f.half_edge.0);
            h.write_u32(f.material_id);
            h.write_u32(f.normal.x.to_bits());
            h.write_u32(f.normal.y.to_bits());
            h.write_u32(f.normal.z.to_bits());
        }
        h.finish()
    }

    /// Connectivity-only fingerprint (POLY_VIEWPORT_PERF P3): ignores positions and normals.
    /// Used to detect topology-stable edits (grab/scale verts) so the display path can
    /// skip full modifier-stack re-evaluation when the cage connectivity is unchanged.
    pub fn topology_signature(&self) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write_usize(self.vertices.len());
        h.write_usize(self.half_edges.len());
        h.write_usize(self.faces.len());
        for e in &self.half_edges {
            h.write_u32(e.vertex.0);
            h.write_u32(e.next.0);
            h.write_u32(e.twin.0);
            h.write_u32(e.face.0);
        }
        for f in &self.faces {
            h.write_u32(f.half_edge.0);
            h.write_u32(f.material_id);
        }
        h.finish()
    }

}
