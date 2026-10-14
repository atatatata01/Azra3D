// Extrude family (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    /// ONE `O(half_edges)` index of `source vertex -> lowest-id live outgoing half-edge`.
    ///
    /// Replaces the "scan every half-edge in the mesh to find a replacement disk pointer" loops
    /// that every extrude tail ran **once per touched vertex** — and because `free_edges` is a
    /// `Vec`, each `contains` inside them was itself a linear scan, making the repair
    /// `O(touched x half_edges x free_edges)`. `or_insert` over ascending ids answers exactly what
    /// the scan's `break`-on-first-match answered, so repaired disks are bit-identical.
    ///
    /// DUPLICATION, FLAGGED NOT FIXED (CLAUDE.md reuse rule): `euler.rs::delete_faces` builds the
    /// byte-identical index inline as `first_out`, and `repoint_vert_disk_if_stale` runs the
    /// unbatched scan. All three want one shared `HalfEdgeMesh` helper; that lives in `euler.rs`,
    /// outside this task's file scope.
    fn extrude_first_outgoing_map(&self) -> std::collections::HashMap<u32, HalfEdgeId> {
        let free_e: std::collections::HashSet<u32> = self.free_edges.iter().copied().collect();
        let mut out = std::collections::HashMap::with_capacity(self.vertices.len());
        for i in 0..self.half_edges.len() as u32 {
            if free_e.contains(&i) { continue; }
            out.entry(self.he_source(HalfEdgeId(i)).0).or_insert(HalfEdgeId(i));
        }
        out
    }

    /// ONE `O(half_edges)` index of `(src, dst) -> lowest-id live FACE-BEARING half-edge`.
    ///
    /// The directed-edge lookup `extrude_region_volume`'s rim bridge needs. Same lowest-id-wins
    /// semantics as the linear scan it replaces (see [`Self::extrude_first_outgoing_map`]).
    /// Face-bearing only: a wire half-edge is never a valid bridge target.
    fn extrude_faced_dir_map(&self) -> std::collections::HashMap<(u32, u32), HalfEdgeId> {
        let free_e: std::collections::HashSet<u32> = self.free_edges.iter().copied().collect();
        let mut out = std::collections::HashMap::with_capacity(self.half_edges.len());
        for i in 0..self.half_edges.len() as u32 {
            if free_e.contains(&i) { continue; }
            let h = HalfEdgeId(i);
            if self.he(h).face == NO_FACE { continue; }
            out.entry((self.he_source(h).0, self.he(h).vertex.0)).or_insert(h);
        }
        out
    }

    /// Extrude a single face along an offset vector.
    /// Returns the new top face ID.
    pub fn extrude_face(&mut self, f: FaceId, offset: Vec3) -> FaceId {
        // `verts[k]` = DEST of `edges[k]`; therefore `edges[k]` runs verts[k-1] -> verts[k].
        let verts = self.face_vertices(f);
        let edges = self.face_half_edges(f);
        let n = verts.len();
        if n < 3 { return f; }

        // Avoid O(mesh) Vec reallocation on exact-capacity builders (locality spike).
        // Cap: n new verts, 4n new half-edges (walls), n new faces.
        self.reserve_elements(n, 4 * n, n);

        // Capture each boundary half-edge's neighbour twin BEFORE we repurpose `edges[k]` into the lifted
        // cap — the new side wall re-pairs with that neighbour so the base seam stays watertight.
        let base_twins: Vec<HalfEdgeId> = edges.iter().map(|&he| self.he(he).twin).collect();

        // Lifted (cap) vertices — copies of the ring, translated by `offset`.
        let new_verts: Vec<VertexId> = verts.iter().map(|&v| {
            let pos = self.vert(v).position + offset;
            let normal = self.vert(v).normal;
            self.alloc_vertex(HVertex { position: pos, normal, half_edge: NO_EDGE })
        }).collect();

        // Repurpose the face's own half-edges into the lifted CAP loop: edges[k] : new_verts[k-1] ->
        // new_verts[k]. Detach the old neighbour twin (moves to the wall bottom); re-paired to the wall
        // top below. (Setting .vertex = new_verts[k] lifts the dest; the source lifts via prev's dest.)
        for (k, &he) in edges.iter().enumerate() {
            self.he_mut(he).vertex = new_verts[k];
            self.he_mut(he).twin = NO_TWIN;
        }

        // Build the n side walls. Wall k (for edge edges[k], base sv->dv) is wound
        // sv -> dv -> new_dv -> new_sv (outward, opposite the cap edge). sv=verts[k-1], dv=verts[k].
        // Walls inherit the extruded face's material_id (owner: extrude uses assigned material).
        let wall_mat = self.face(f).material_id;
        let mut w_bottom = vec![NO_EDGE; n]; // sv -> dv        (twins neighbour base_twins[k] / boundary)
        let mut w_right  = vec![NO_EDGE; n]; // dv -> new_dv    (vertical; twins wall[k+1].left)
        let mut w_top    = vec![NO_EDGE; n]; // new_dv -> new_sv(twins cap edge edges[k])
        let mut w_left   = vec![NO_EDGE; n]; // new_sv -> sv    (vertical; twins wall[k-1].right)
        for k in 0..n {
            let kp = (k + n - 1) % n;
            let (sv, dv, nsv, ndv) = (verts[kp], verts[k], new_verts[kp], new_verts[k]);
            let sf = self.alloc_face(HFace {
                half_edge: NO_EDGE,
                normal: Vec3::ZERO,
                material_id: wall_mat,
            });
            let he0 = self.alloc_half_edge(HalfEdge { vertex: dv,  twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: sf });
            let he1 = self.alloc_half_edge(HalfEdge { vertex: ndv, twin: NO_TWIN, next: NO_EDGE, prev: he0,    face: sf });
            let he2 = self.alloc_half_edge(HalfEdge { vertex: nsv, twin: NO_TWIN, next: NO_EDGE, prev: he1,    face: sf });
            let he3 = self.alloc_half_edge(HalfEdge { vertex: sv,  twin: NO_TWIN, next: he0,     prev: he2,    face: sf });
            self.he_mut(he0).next = he1;
            self.he_mut(he0).prev = he3; // close the cyclic prev (he3.next already = he0)
            self.he_mut(he1).next = he2;
            self.he_mut(he2).next = he3;
            self.face_mut(sf).half_edge = he0;
            w_bottom[k] = he0; w_right[k] = he1; w_top[k] = he2; w_left[k] = he3;
        }

        // Re-link every twin so the region is watertight + manifold.
        for k in 0..n {
            let kn = (k + 1) % n;
            // wall top <-> cap edge (new_dv->new_sv  vs  new_sv->new_dv)
            let cap = edges[k];
            self.he_mut(w_top[k]).twin = cap;
            self.he_mut(cap).twin = w_top[k];
            // wall bottom <-> original neighbour across the base edge (else stays a boundary edge)
            let nb = base_twins[k];
            if nb != NO_TWIN {
                self.he_mut(w_bottom[k]).twin = nb;
                self.he_mut(nb).twin = w_bottom[k];
            }
            // vertical: wall[k].right (dv->new_dv) <-> wall[k+1].left (new_dv->dv)
            self.he_mut(w_right[k]).twin = w_left[kn];
            self.he_mut(w_left[kn]).twin = w_right[k];
        }

        // Point every touched vertex's disk at a genuine OUTGOING half-edge (the old code stored an
        // incoming one — a stale/mislinked vertex.half_edge that is_manifold() now rejects).
        for k in 0..n {
            self.vert_mut(new_verts[k]).half_edge = edges[(k + 1) % n]; // cap edge sourced at new_verts[k]
            self.vert_mut(verts[k]).half_edge = w_bottom[(k + 1) % n];  // wall bottom sourced at verts[k]
        }

        // Locality: only recompute normals for the extruded island (base ring + cap verts).
        let mut seed = verts.clone();
        seed.extend_from_slice(&new_verts);
        self.recompute_normals_region(seed);
        f // the original face is now the lifted cap
    }

    /// Create a faceless **wire edge** between two live vertices.
    ///
    /// Both half-edges have `face == NO_FACE`, are mutual twins, and form a 2-cycle on
    /// `next`/`prev` — the same representation as vertex-extrude wires and delete-faces-only
    /// loose edges. Accepted by `validate()` / `is_manifold()`.
    ///
    /// Returns the canonical half-edge id (`min` of the pair) so undirected edge selection can
    /// store a single id. Returns `None` if either vertex is dead or `a == b`.
    ///
    /// Used by poly Shift+D edge duplicate (Blender: independent wire island, not a digon face).
    pub fn create_wire_edge(&mut self, a: VertexId, b: VertexId) -> Option<HalfEdgeId> {
        if a == b || !self.is_vert_live(a) || !self.is_vert_live(b) {
            return None;
        }
        // he_ab: a -> b (dest = b); he_ba: b -> a (dest = a).
        let he_ab = self.alloc_half_edge(HalfEdge {
            vertex: b,
            twin: NO_TWIN,
            next: NO_EDGE,
            prev: NO_EDGE,
            face: NO_FACE,
        });
        let he_ba = self.alloc_half_edge(HalfEdge {
            vertex: a,
            twin: he_ab,
            next: he_ab,
            prev: he_ab,
            face: NO_FACE,
        });
        self.he_mut(he_ab).twin = he_ba;
        self.he_mut(he_ab).next = he_ba;
        self.he_mut(he_ab).prev = he_ba;
        // Disk pointers: only set when the vertex has no live outgoing HE yet (new loose verts
        // start with NO_EDGE; existing mesh verts keep their face-bound disk).
        if self.vert(a).half_edge == NO_EDGE || !self.is_edge_live(self.vert(a).half_edge) {
            self.vert_mut(a).half_edge = he_ab; // sourced at a
        }
        if self.vert(b).half_edge == NO_EDGE || !self.is_edge_live(self.vert(b).half_edge) {
            self.vert_mut(b).half_edge = he_ba; // sourced at b
        }
        Some(HalfEdgeId(he_ab.0.min(he_ba.0)))
    }

    /// Blender `mesh.extrude_vertices_move`: for each selected live vertex, create a NEW vertex at
    /// `pos + offset` and a wire edge connecting them. Returns the new vertex ids (in ascending
    /// selection order). Shared selection verts are extruded once.
    ///
    /// Wire edges: both half-edges faceless, twinned, 2-cycle next/prev — accepted by `validate()` /
    /// `is_manifold()` (same representation as delete-faces-only loose edges).
    pub fn extrude_vertices(
        &mut self,
        selected: &std::collections::HashSet<u32>,
        offset: Vec3,
    ) -> Vec<VertexId> {
        let mut ordered: Vec<u32> = selected.iter().copied()
            .filter(|&v| {
                self.is_vert_live(VertexId(v))
            })
            .collect();
        ordered.sort_unstable();
        if ordered.is_empty() { return Vec::new(); }

        let mut new_verts = Vec::with_capacity(ordered.len());
        for &vid in &ordered {
            let v = VertexId(vid);
            let pos = self.vert(v).position + offset;
            let normal = self.vert(v).normal;
            let nv = self.alloc_vertex(HVertex {
                position: pos,
                normal,
                half_edge: NO_EDGE,
            });
            let _ = self.create_wire_edge(v, nv);
            new_verts.push(nv);
        }
        // Locality: only the selected verts' own disks changed (no face moved, no face was
        // created), so a region recompute seeded on them is the whole of this op's normal work.
        //
        // ONE deliberate behaviour delta vs. the old full-mesh pass: a WIRE vertex has no
        // incident face, so `recompute_normals` used to overwrite every loose vertex in the mesh
        // with `Vec3::Y`; the region pass never visits them and the new tips therefore keep the
        // source vertex's normal that `alloc_vertex` copied two lines above (which is what the
        // copy was for — the full pass was discarding it). A loose vertex has no shaded surface,
        // so neither value reaches the screen.
        let mut seed: Vec<VertexId> = ordered.iter().map(|&v| VertexId(v)).collect();
        seed.extend_from_slice(&new_verts);
        self.recompute_normals_region(seed);
        new_verts
    }

    /// Blender `mesh.extrude_edges_move`: for each selected undirected edge that borders at least one
    /// face, create a new edge and a connecting quad face. Shared endpoints weld (one new vert per
    /// old vert). Returns the new (lifted) vertex ids. Edges with no face on either side are skipped.
    ///
    /// Returns empty if nothing was extruded.
    pub fn extrude_edges(
        &mut self,
        selected: &std::collections::HashSet<u32>,
        offset: Vec3,
    ) -> Vec<VertexId> {
        use std::collections::{HashMap, HashSet};
        // Canonical undirected primary half-edges.
        let mut primary: Vec<u32> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut sel_sorted: Vec<u32> = selected.iter().copied().collect();
        sel_sorted.sort_unstable();
        for &eid in &sel_sorted {
            if eid as usize >= self.half_edges.len() { continue; }
            if self.free_edges.contains(&eid) { continue; }
            let he = self.he(HalfEdgeId(eid));
            if he.face == NO_FACE && (he.twin == NO_TWIN || self.he(he.twin).face == NO_FACE) {
                continue; // pure wire — nothing to extrude a face from
            }
            // Prefer the directed he that has a face (for winding).
            // INTERIOR EDGES ARE ALLOWED (owner 2026-08-04: "for the edge it is ok to have
            // non-manifold, the user expects it"). Blender extrudes an interior edge into a
            // fin, producing an edge with THREE incident faces. A twin-based half-edge can
            // pair only two, so the fin's base half-edge stays UNPAIRED (NO_TWIN) while the
            // original edge keeps its two faces and its pairing. The fin is still attached:
            // it shares the two real endpoint VERTICES, so it moves with them and welds on
            // merge. That is this kernel's faithful spelling of Blender's non-manifold
            // result, not an approximation of it -- the only difference is bookkeeping
            // (two coincident edge records instead of one edge with three faces).
            let prim = if he.face != NO_FACE {
                eid
            } else if he.twin != NO_TWIN && self.he(he.twin).face != NO_FACE {
                he.twin.0
            } else {
                continue;
            };
            if seen.contains(&prim) { continue; }
            seen.insert(prim);
            primary.push(prim);
        }
        if primary.is_empty() { return Vec::new(); }

        // One new vert per unique endpoint on the selection.
        let mut lift: HashMap<u32, VertexId> = HashMap::new();
        let mut new_verts: Vec<VertexId> = Vec::new();
        for &eid in &primary {
            let he = HalfEdgeId(eid);
            let dst = self.he(he).vertex.0;
            let src = self.he_source(he).0;
            for vid in [src, dst] {
                if lift.contains_key(&vid) { continue; }
                let v = VertexId(vid);
                let pos = self.vert(v).position + offset;
                let normal = self.vert(v).normal;
                let nv = self.alloc_vertex(HVertex {
                    position: pos,
                    normal,
                    half_edge: NO_EDGE,
                });
                lift.insert(vid, nv);
                new_verts.push(nv);
            }
        }

        // Quad per primary edge. Order [src, new_src, new_dst, dst] so create_face's last edge
        // is dst→src and twins the existing face he src→dst (boundary free twin slot).
        // New fin faces inherit the source edge's face material.
        for &eid in &primary {
            let he = HalfEdgeId(eid);
            let dst = self.he(he).vertex.0;
            let src = self.he_source(he).0;
            let ns = lift[&src];
            let nd = lift[&dst];
            let vids = [VertexId(src), ns, nd, VertexId(dst)];
            let mat = {
                let f = self.he(he).face;
                if f != NO_FACE {
                    self.face(f).material_id
                } else {
                    0
                }
            };
            if let Some(fid) = self.create_face(&vids) {
                self.face_mut(fid).material_id = mat;
            }
        }
        // Point lifted verts at an outgoing half-edge if still unset. ONE O(half-edges) index,
        // built only if some vert actually needs it, instead of a full rescan per vert (E3).
        let mut first_out: Option<HashMap<u32, HalfEdgeId>> = None;
        for &nv in &new_verts {
            if self.vert(nv).half_edge == NO_EDGE || !self.is_edge_live(self.vert(nv).half_edge) {
                if let Some(&h) = first_out
                    .get_or_insert_with(|| self.extrude_first_outgoing_map())
                    .get(&nv.0)
                {
                    self.vert_mut(nv).half_edge = h;
                }
            }
        }
        self.repair_unpaired_twins();
        // Locality: only the new fin quads and their endpoints changed. Seed = each lifted vert
        // plus the base endpoint it came from (deterministic — driven by `primary`, not a map).
        let mut seed: Vec<VertexId> = new_verts.clone();
        for &eid in &primary {
            let he = HalfEdgeId(eid);
            seed.push(self.he(he).vertex);
            seed.push(self.he_source(he));
        }
        self.recompute_normals_region(seed);
        new_verts
    }

    /// Average unit normal for edge extrude direction: mean of face normals incident to the edges.
    pub fn edge_extrude_direction(&self, selected: &std::collections::HashSet<u32>) -> Vec3 {
        let mut sum = Vec3::ZERO;
        let mut n = 0u32;
        for &e in selected {
            if e as usize >= self.half_edges.len() {
                continue;
            }
            let he = self.he(HalfEdgeId(e));
            if he.face != NO_FACE {
                sum += self.compute_face_normal(he.face);
                n += 1;
            }
            if he.twin != NO_TWIN {
                let tf = self.he(he.twin).face;
                if tf != NO_FACE {
                    sum += self.compute_face_normal(tf);
                    n += 1;
                }
            }
        }
        if n == 0 {
            return Vec3::Y;
        }
        let d = sum.normalize_or_zero();
        if d.length_squared() < 0.25 {
            Vec3::Y
        } else {
            d
        }
    }

    /// Region extrude (Blender default E): lift a SET of faces as one shell. Interior shared edges stay
    /// welded (both region faces re-point to the same lifted verts); walls are built only on the region's
    /// OUTER boundary. Boundary verts are DUPLICATED (original stays for the wall bottom, lifted for the
    /// cap); true interior verts (all incident faces in the region AND not on a region-boundary edge)
    /// are MOVED in place. Open-mesh hole rims count as region-boundary even when no non-region face
    /// remains (e.g. open box after one face deleted) — otherwise walls self-loop. Contrast
    /// `extrude_face` (per-face) which duplicates every shared edge into a doubled wall.
    ///
    /// Returns the **cap** face ids the caller should slide/select after extrude:
    /// - solid (neighboured) region: the same region face ids (they were re-pointed onto the lift)
    /// - fully-open volume path: the **new** top shell faces (originals become the bottom shell)
    pub fn extrude_region(&mut self, faces: &std::collections::HashSet<FaceId>, offset: Vec3) -> Vec<FaceId> {
        use std::collections::{HashMap, HashSet};
        // DETERMINISM (root cause of run-to-run boolean divergence, 2026-08-04, owner
        // test-05: v=53/55/57 across three identical runs): HashSet iteration order is
        // RANDOM per process. Iterating the raw set created cap topology in random order,
        // so every downstream consumer — including the exact boolean, whose tessellation
        // is input-order-sensitive — produced run-dependent results. Iterate faces in
        // sorted id order; identical input must yield identical topology, always.
        let faces_sorted: Vec<FaceId> = {
            let mut v: Vec<FaceId> = faces.iter().copied().collect();
            v.sort_unstable_by_key(|f| f.0);
            v
        };
        // ITERATE `faces_sorted` (determinism, above) but TEST MEMBERSHIP on `faces` — the
        // caller's own `HashSet`. Shadowing the parameter with the sorted `Vec` silently turned
        // every `faces.contains(..)` below into a LINEAR scan of the selection, which is half of
        // the O(selection^2) region-extrude blowup (4096-face selection: 2.2 s). The determinism
        // contract above needs a deterministic ITERATION order; it never needed a Vec for lookups.
        let mut region_verts_set: HashSet<u32> = HashSet::new();
        let mut ring: Vec<VertexId> = Vec::new(); // reused ring buffer — see `face_vertices_into`
        for &f in &faces_sorted {
            if self.face(f).half_edge == NO_EDGE { continue; }
            self.face_vertices_into(f, &mut ring);
            for v in &ring { region_verts_set.insert(v.0); }
        }
        if region_verts_set.is_empty() { return Vec::new(); }
        // SORTED — vertex ALLOCATION order must be deterministic (see faces_sorted above;
        // iterating the set allocated lifted verts in random per-run order, randomizing
        // every downstream id and the boolean's order-sensitive tessellation).
        let region_verts: Vec<u32> = {
            let mut v: Vec<u32> = region_verts_set.iter().copied().collect();
            v.sort_unstable();
            v
        };

        // FULLY-OPEN region (no boundary edge twins a non-region face = a standalone open surface): Blender
        // extrudes it into a closed VOLUME — keep the original surface as one shell, add a translated copy
        // as the other, bridge the rim. (A neighboured region — e.g. a cube face — instead MOVES, below.)
        let mut he_ring: Vec<HalfEdgeId> = Vec::new(); // reused — see `face_half_edges_into`
        let mut fully_open = true;
        for &f in &faces_sorted {
            if self.face(f).half_edge == NO_EDGE { continue; }
            self.face_half_edges_into(f, &mut he_ring);
            let open = he_ring.iter().all(|&he| {
                let t = self.he(he).twin;
                t == NO_TWIN || faces.contains(&self.he(t).face)
            });
            if !open { fully_open = false; break; }
        }
        if fully_open {
            // SORTED slice, not a HashSet: `extrude_region_volume` iterates it to allocate the
            // lifted cap faces and the rim walls, so a random-order set randomized THEIR ids too
            // — the same run-to-run divergence the determinism note above fixed for this path
            // (measured: v/h/f COUNTS stable, id ORDER different on every process run).
            return self.extrude_region_volume(&faces_sorted, &region_verts_set, offset);
        }

        // Snapshot each region face half-edge (BEFORE mutation): orig src/dst, boundary?, orig twin.
        // A half-edge is a boundary edge iff its twin's face is NOT in the region (or it has no twin).
        // `mat` = source region face material — walls inherit it by default.
        struct Info {
            he: HalfEdgeId,
            src: u32,
            dst: u32,
            boundary: bool,
            twin: HalfEdgeId,
            mat: u32,
        }
        let mut infos: Vec<Info> = Vec::new();
        let mut on_region_boundary: HashSet<u32> = HashSet::new();
        for &f in &faces_sorted {
            if self.face(f).half_edge == NO_EDGE { continue; }
            let mat = self.face(f).material_id;
            self.face_half_edges_into(f, &mut he_ring);
            for &he in &he_ring {
                let src = self.he_source(he).0;
                let dst = self.he(he).vertex.0;
                let twin = self.he(he).twin;
                let boundary = twin == NO_TWIN || !faces.contains(&self.he(twin).face);
                if boundary {
                    // Open-mesh hole rims and selection borders — these verts MUST be
                    // duplicated for walls. Classifying them as interior (all faces in
                    // region, but an open boundary edge) builds degenerate wall quads
                    // with repeated verts (open box, 4 walls after delete one face).
                    on_region_boundary.insert(src);
                    on_region_boundary.insert(dst);
                }
                infos.push(Info { he, src, dst, boundary, twin, mat });
            }
        }

        // INTERIOR vert → move in place (no dup). Must be strictly inside the region:
        // every face using it is in the region AND it is not on a region-boundary edge.
        // The boundary-edge clause is required for open meshes: hole-rim verts often have
        // no non-region face (deleted face) so the faces-only test would mark them interior,
        // then wall src→dst→lift(dst)→lift(src) collapses to a self-loop pinch.
        //
        // ONE pass over the mesh's faces builds the fact this needs — "does vertex v have an
        // incident live face OUTSIDE the region?" — instead of re-deciding it per region vertex.
        // The old form was `for v in region_verts { for f in all_faces { face_vertices(f) } }`:
        // O(region_verts x mesh_faces x ring_len) with a fresh `Vec` allocated on every inner
        // step, which is why a 64-face extrude cost 14x sixty-four single-face extrudes and why
        // the cost tracked the WHOLE mesh even when the selection never changed.
        let free_f: HashSet<u32> = self.free_faces.iter().copied().collect();
        let mut has_outside_face: Vec<bool> = vec![false; self.vertices.len()];
        for i in 0..self.faces.len() as u32 {
            if free_f.contains(&i) { continue; }
            let f = FaceId(i);
            if faces.contains(&f) { continue; } // in-region faces never disqualify a vertex
            self.face_vertices_into(f, &mut ring);
            for v in &ring {
                if let Some(slot) = has_outside_face.get_mut(v.0 as usize) { *slot = true; }
            }
        }
        let mut interior: HashMap<u32, bool> = HashMap::with_capacity(region_verts.len());
        for &vid in &region_verts {
            // Exactly the old predicate, negated: "no live face outside the region uses vid".
            let only_region_faces = !has_outside_face.get(vid as usize).copied().unwrap_or(false);
            let is_int = only_region_faces && !on_region_boundary.contains(&vid);
            interior.insert(vid, is_int);
        }

        // Lifted copies for boundary verts; interior verts move in place.
        let mut lifted: HashMap<u32, VertexId> = HashMap::new();
        for &vid in &region_verts {
            if !interior[&vid] {
                let n = self.vert(VertexId(vid)).normal;
                let p = self.vert(VertexId(vid)).position + offset;
                lifted.insert(vid, self.alloc_vertex(HVertex { position: p, normal: n, half_edge: NO_EDGE }));
            }
        }
        for &vid in &region_verts {
            if interior[&vid] {
                let p = self.vert(VertexId(vid)).position + offset;
                self.vert_mut(VertexId(vid)).position = p;
            }
        }
        let map_v = |vid: u32, interior: &HashMap<u32, bool>, lifted: &HashMap<u32, VertexId>| {
            if interior[&vid] { VertexId(vid) } else { lifted[&vid] }
        };

        // Re-point every region face half-edge's dest to the lifted/moved vertex (lifts the region).
        for info in &infos {
            self.he_mut(info.he).vertex = map_v(info.dst, &interior, &lifted);
        }
        // Detach the boundary edges' twins — they'll re-pair to the new wall (interior edges keep twins).
        for info in &infos {
            if info.boundary {
                self.he_mut(info.he).twin = NO_TWIN;
                if info.twin != NO_TWIN { self.he_mut(info.twin).twin = NO_TWIN; }
            }
        }
        // Walls on boundary edges, twinned EXPLICITLY (create_face's find_half_edge is unreliable here —
        // every boundary vertex has multiple detached edges, so vertex_fan can't locate the neighbour).
        // Wall wound src→dst→lifted_dst→lifted_src. up_at[v]/down_at[v] collect the two verticals at each
        // boundary vertex (the wall ending at v contributes the up-vertical v→lift; the wall starting at v
        // the down-vertical lift→v) so adjacent walls pair around the boundary loop.
        let mut up_at: HashMap<u32, HalfEdgeId> = HashMap::new();
        let mut down_at: HashMap<u32, HalfEdgeId> = HashMap::new();
        for info in &infos {
            if !info.boundary { continue; }
            let (os, od) = (VertexId(info.src), VertexId(info.dst));
            let ls = map_v(info.src, &interior, &lifted);
            let ld = map_v(info.dst, &interior, &lifted);
            let sf = self.alloc_face(HFace {
                half_edge: NO_EDGE,
                normal: Vec3::ZERO,
                material_id: info.mat,
            });
            let he0 = self.alloc_half_edge(HalfEdge { vertex: od, twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: sf }); // os->od
            let he1 = self.alloc_half_edge(HalfEdge { vertex: ld, twin: NO_TWIN, next: NO_EDGE, prev: he0,    face: sf }); // od->ld
            let he2 = self.alloc_half_edge(HalfEdge { vertex: ls, twin: NO_TWIN, next: NO_EDGE, prev: he1,    face: sf }); // ld->ls
            let he3 = self.alloc_half_edge(HalfEdge { vertex: os, twin: NO_TWIN, next: he0,     prev: he2,    face: sf }); // ls->os
            self.he_mut(he0).next = he1;
            self.he_mut(he0).prev = he3;
            self.he_mut(he1).next = he2;
            self.he_mut(he2).next = he3;
            self.face_mut(sf).half_edge = he0;
            // wall top ↔ the re-pointed cap edge (info.he is ls->ld; he2 is ld->ls)
            self.he_mut(he2).twin = info.he;
            self.he_mut(info.he).twin = he2;
            // wall bottom ↔ the outside neighbour across the base edge (else stays boundary)
            if info.twin != NO_TWIN {
                self.he_mut(he0).twin = info.twin;
                self.he_mut(info.twin).twin = he0;
            }
            up_at.insert(info.dst, he1);   // vertical od->ld  (the "up" at dst)
            down_at.insert(info.src, he3); // vertical ls->os  (the "down" at src)
        }
        // Pair the two verticals meeting at each boundary vertex.
        let mut up_keys: Vec<u32> = up_at.keys().copied().collect();
        up_keys.sort_unstable();
        for &v in &up_keys {
            let up = up_at[&v];
            if let Some(&down) = down_at.get(&v) {
                self.he_mut(up).twin = down;
                self.he_mut(down).twin = up;
            }
        }
        // Repair vertex.half_edge on the duplicated boundary verts + their lifts (re-pointing may have
        // left them pointing at a now-lifted edge — the same class the delete_face #6 fix handles).
        let mut fixups: Vec<VertexId> = Vec::new();
        for &vid in &region_verts {
            if !interior[&vid] { fixups.push(VertexId(vid)); fixups.push(lifted[&vid]); }
        }
        // ONE O(half-edges) index instead of one full rescan PER stale vertex (E3).
        let mut first_out: Option<HashMap<u32, HalfEdgeId>> = None;
        for &v in &fixups {
            let h = self.vert(v).half_edge;
            let stale = h == NO_EDGE || h.0 as usize >= self.half_edges.len()
                || self.free_edges.contains(&h.0) || self.he_source(h) != v;
            if stale {
                let repl = first_out
                    .get_or_insert_with(|| self.extrude_first_outgoing_map())
                    .get(&v.0).copied().unwrap_or(NO_EDGE);
                self.vert_mut(v).half_edge = repl;
            }
        }
        // Locality: only the lifted/moved island's normals changed (same treatment as
        // `extrude_face`). Seed = every region vertex plus its lifted copy, in sorted order.
        let mut seed: Vec<VertexId> = Vec::with_capacity(region_verts.len() * 2);
        for &vid in &region_verts {
            seed.push(VertexId(vid));
            if let Some(&l) = lifted.get(&vid) { seed.push(l); }
        }
        self.recompute_normals_region(seed);
        // Cap faces are the region faces (re-pointed onto lifted/moved verts), in the same sorted
        // order the op allocated them — see the determinism note at the top.
        faces_sorted.iter().copied().filter(|f| self.face(*f).half_edge != NO_EDGE).collect()
    }

    /// Extrude a fully-OPEN region into a closed volume (Blender open-surface extrude): keep the original
    /// surface (flipped to face outward as one shell), add a translated copy of every region vertex as the
    /// other shell, and bridge the open rim with wall quads. Explicit twin-linking (create_face's fan search
    /// is unreliable across the many detached boundary verts — same lesson as extrude_region).
    ///
    /// Returns the **new top-shell** face ids (originals are the bottom shell after flip).
    ///
    /// `faces` is a **sorted** slice, not a set: every loop below allocates topology in this
    /// order (cap faces, rim walls), so the caller's ordering IS the output's id assignment.
    fn extrude_region_volume(&mut self, faces: &[FaceId],
                             region_verts: &std::collections::HashSet<u32>, offset: Vec3) -> Vec<FaceId> {
        use std::collections::HashMap;
        // Capture face rings (pre-flip) + open rim edges with source material.
        // rings: (vertex ring, source material_id) — caps inherit source face material.
        let mut rings: Vec<(Vec<u32>, u32)> = Vec::new();
        let mut rim: Vec<(u32, u32, u32)> = Vec::new(); // (src, dst, mat) pre-flip
        let mut ring: Vec<VertexId> = Vec::new();
        let mut he_ring: Vec<HalfEdgeId> = Vec::new();
        for &f in faces {
            if self.face(f).half_edge == NO_EDGE { continue; }
            let mat = self.face(f).material_id;
            self.face_vertices_into(f, &mut ring);
            rings.push((ring.iter().map(|v| v.0).collect(), mat));
            self.face_half_edges_into(f, &mut he_ring);
            for &he in &he_ring {
                if self.he(he).twin == NO_TWIN {
                    rim.push((self.he_source(he).0, self.he(he).vertex.0, mat));
                }
            }
        }
        // Translated copy of every region vertex.
        let mut lifted: HashMap<u32, VertexId> = HashMap::new();
        let mut rv_sorted: Vec<u32> = region_verts.iter().copied().collect();
        rv_sorted.sort_unstable();
        for &vid in &rv_sorted {
            let p = self.vert(VertexId(vid)).position + offset;
            let n = self.vert(VertexId(vid)).normal;
            lifted.insert(vid, self.alloc_vertex(HVertex { position: p, normal: n, half_edge: NO_EDGE }));
        }
        // Flip the originals (outward-facing bottom shell) and build lifted cap faces (top shell).
        for &f in faces { if self.face(f).half_edge != NO_EDGE { self.flip_face(f); } }
        // `create_face` runs a FULL-MESH `recompute_normals()` per call (euler.rs) — N cap faces
        // meant N whole-mesh normal passes. The `_no_normals_` variant is exactly the batch-install
        // form; the single `recompute_normals_region` at the tail covers every face created here.
        let mut cap_faces: Vec<FaceId> = Vec::new();
        for (r, mat) in &rings {
            let lr: Vec<VertexId> = r.iter().map(|v| lifted[v]).collect();
            if let Some(cf) = self.create_face_no_normals_with_material(&lr, *mat) {
                cap_faces.push(cf);
            }
        }
        // Re-pair twins EXPLICITLY. For each rim edge a->b (pre-flip): the flipped bottom now has b->a; the
        // lifted cap has a'->b'. Build wall a->b->b'->a' — a->b twins bottom's b->a, b'->a' twins cap's a'->b',
        // and adjacent walls' verticals pair at each rim vertex.
        // Built ONCE (E3). The old `find_dir` closure rescanned every half-edge in the mesh per rim
        // edge — and `free_edges.contains` is itself linear, so the walk was O(rim x HE x free).
        // `or_insert` over ascending ids reproduces the scan's lowest-id-wins answer exactly, and
        // each new wall half-edge is registered as it is created so a later query still sees it.
        let mut dir_map = self.extrude_faced_dir_map();
        let mut up_at: HashMap<u32, HalfEdgeId> = HashMap::new();   // vertical b -> b' (up at dst)
        let mut down_at: HashMap<u32, HalfEdgeId> = HashMap::new(); // vertical a' -> a (down at src)
        for &(a, b, mat) in &rim {
            let (la, lb) = (lifted[&a], lifted[&b]);
            // flipped original edge b->a, and the lifted cap edge a'->b'
            let bottom_ba = dir_map.get(&(b, a)).copied().unwrap_or(NO_EDGE);
            let cap_ab = dir_map.get(&(la.0, lb.0)).copied().unwrap_or(NO_EDGE);
            let sf = self.alloc_face(HFace {
                half_edge: NO_EDGE,
                normal: Vec3::ZERO,
                material_id: mat,
            });
            let he0 = self.alloc_half_edge(HalfEdge { vertex: VertexId(b), twin: NO_TWIN, next: NO_EDGE, prev: NO_EDGE, face: sf }); // a->b
            let he1 = self.alloc_half_edge(HalfEdge { vertex: lb,          twin: NO_TWIN, next: NO_EDGE, prev: he0,    face: sf });  // b->b'
            let he2 = self.alloc_half_edge(HalfEdge { vertex: la,          twin: NO_TWIN, next: NO_EDGE, prev: he1,    face: sf });  // b'->a'
            let he3 = self.alloc_half_edge(HalfEdge { vertex: VertexId(a), twin: NO_TWIN, next: he0,     prev: he2,    face: sf });  // a'->a
            self.he_mut(he0).next = he1; self.he_mut(he0).prev = he3;
            self.he_mut(he1).next = he2; self.he_mut(he2).next = he3;
            self.face_mut(sf).half_edge = he0;
            if bottom_ba != NO_EDGE { self.he_mut(he0).twin = bottom_ba; self.he_mut(bottom_ba).twin = he0; }
            if cap_ab != NO_EDGE { self.he_mut(he2).twin = cap_ab; self.he_mut(cap_ab).twin = he2; }
            // Keep the index current: the old per-rim-edge scan could see a wall built by an
            // EARLIER iteration (only reachable on a degenerate rim carrying an edge in both
            // directions). New ids are always higher, so `or_insert` still yields lowest-id-wins.
            dir_map.entry((a, b)).or_insert(he0);
            dir_map.entry((b, lb.0)).or_insert(he1);
            dir_map.entry((lb.0, la.0)).or_insert(he2);
            dir_map.entry((la.0, a)).or_insert(he3);
            up_at.insert(b, he1);
            down_at.insert(a, he3);
        }
        let mut up_keys: Vec<u32> = up_at.keys().copied().collect();
        up_keys.sort_unstable();
        for &v in &up_keys {
            let up = up_at[&v];
            if let Some(&down) = down_at.get(&v) {
                self.he_mut(up).twin = down;
                self.he_mut(down).twin = up;
            }
        }
        // Heal any face-bearing boundary half-edges that should twin but didn't — create_face's incremental
        // fan-search can miss a lifted-cap INTERIOR edge (e.g. around a shared center vertex). Pair remaining
        // untwined face half-edges by exact reverse direction (a closed volume has none left over).
        let free_e: std::collections::HashSet<u32> = self.free_edges.iter().copied().collect();
        let mut by_dir: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
        for i in 0..self.half_edges.len() as u32 {
            if free_e.contains(&i) { continue; } // hoisted — `free_edges` is a Vec (linear contains)
            let h = HalfEdgeId(i);
            if self.he(h).face == NO_FACE || self.he(h).twin != NO_TWIN { continue; }
            let (s, d) = (self.he_source(h).0, self.he(h).vertex.0);
            if let Some(g) = by_dir.remove(&(d, s)) {
                if self.he(g).twin == NO_TWIN {
                    self.he_mut(h).twin = g;
                    self.he_mut(g).twin = h;
                    continue;
                }
            }
            by_dir.insert((s, d), h);
        }
        // Repair disk pointers on every touched vertex. Sorted-order build (same SET as before)
        // so the list is also the normal-recompute seed below.
        let mut fixups: Vec<VertexId> = Vec::with_capacity(rv_sorted.len() * 2);
        for &vid in &rv_sorted {
            fixups.push(VertexId(vid));
            if let Some(&l) = lifted.get(&vid) { fixups.push(l); }
        }
        // ONE O(half-edges) index instead of one full rescan PER stale vertex (E3).
        let mut first_out: Option<HashMap<u32, HalfEdgeId>> = None;
        for &v in &fixups {
            let h = self.vert(v).half_edge;
            let stale = h == NO_EDGE || h.0 as usize >= self.half_edges.len()
                || self.free_edges.contains(&h.0) || self.he_source(h) != v;
            if stale {
                let repl = first_out
                    .get_or_insert_with(|| self.extrude_first_outgoing_map())
                    .get(&v.0).copied().unwrap_or(NO_EDGE);
                self.vert_mut(v).half_edge = repl;
            }
        }
        // Locality: the volume is exactly the original shell + its lifted copy, so `fixups` is
        // already the complete touched-vertex seed (and covers the cap faces built without
        // normals above).
        self.recompute_normals_region(fixups);
        cap_faces
    }

}
