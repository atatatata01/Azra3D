// Inset / circle family (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    /// Inset a face by `amount`. Returns the new inner face.
    /// Creates connecting quads between the original boundary and the inset face (Blender-style).
    pub fn inset_face(&mut self, f: FaceId, amount: f32) -> FaceId {
        self.inset_face_depth(f, amount, 0.0)
    }

    /// Per-vertex **EVEN-OFFSET displacement per unit of thickness** for a face's boundary ring, in
    /// `face_vertices(f)` order: `v_inset = v + inset_offset_dirs(f)[i] * thickness`.
    ///
    /// THE single even-offset rule. `inset_face_depth` AND the app's interactive inset drag both read
    /// it, so the geometry exists in exactly one place (CLAUDE.md No-Bandaids: never two code paths for
    /// one operation — the drag used to re-implement a naive move-toward-centroid of its own, so the I
    /// key stayed wrong even when the kernel was right).
    ///
    /// The returned vector is NOT normalized: it carries the `1/(1 + dot(n1,n2))` scale, so scaling it
    /// by `thickness` lands every edge exactly `thickness` inward of its original line.
    pub fn inset_offset_dirs(&self, f: FaceId) -> Vec<Vec3> {
        let verts = self.face_vertices(f);
        let n = verts.len();
        if n < 3 { return vec![Vec3::ZERO; n]; }
        let normal = self.compute_face_normal(f);
        let pos: Vec<Vec3> = verts.iter().map(|&v| self.vert(v).position).collect();
        // Inward in-plane normal of edge i (pos[i] -> pos[i+1]); winding-invariant.
        let edge_inward = |i: usize| -> Vec3 {
            normal.cross(pos[(i + 1) % n] - pos[i]).normalize_or_zero()
        };
        (0..n).map(|i| {
            let n_in = edge_inward((i + n - 1) % n); // edge arriving at vertex i
            let n_out = edge_inward(i);              // edge leaving vertex i
            let denom = 1.0 + n_in.dot(n_out);
            // denom -> 0 = the two offset lines are (anti)parallel (a ~180° spike vertex). Fall back to
            // a plain single-edge offset instead of exploding to infinity.
            if denom.abs() < 1e-4 { n_out } else { (n_in + n_out) / denom }
        }).collect()
    }

    /// Boundary-vertex EVEN-OFFSET displacements (PER UNIT thickness) for a REGION inset of `faces` —
    /// Blender `inset_region` / the `I` default. Only the region's OUTER boundary moves; shared INTERIOR
    /// edges and interior vertices stay put. Returns `(boundary vertex, displacement-per-thickness)`.
    ///
    /// A boundary half-edge = one inside a selected face whose twin is absent or lies OUTSIDE the
    /// selection. Each boundary vertex has one incoming + one outgoing boundary edge; the displacement
    /// reuses the same even-offset intersection as [`inset_offset_dirs`](Self::inset_offset_dirs), with
    /// each edge's inward normal taken in ITS OWN face's plane (so a non-planar region degrades sanely).
    /// Two COLLINEAR boundary edges give `denom = 2` -> a plain perpendicular offset, which is exactly
    /// what Blender does at a mid-edge boundary vertex.
    pub fn inset_region_offset_dirs(&self, faces: &std::collections::HashSet<FaceId>) -> Vec<(VertexId, Vec3)> {
        use std::collections::HashMap;
        let bnd = self.region_boundary_half_edges(faces);
        // src vertex -> the boundary edge LEAVING it (continues the loop)
        let mut out_of: HashMap<u32, HalfEdgeId> = HashMap::new();
        for &he in &bnd { out_of.insert(self.he_source(he).0, he); }
        let inward = |he: HalfEdgeId| -> Vec3 {
            let n = self.compute_face_normal(self.he(he).face);
            let a = self.vert(self.he_source(he)).position;
            let b = self.vert(self.he(he).vertex).position;
            n.cross(b - a).normalize_or_zero()
        };
        bnd.iter().map(|&he| {
            let v = self.he(he).vertex;              // the vertex at the END of this boundary edge
            let n_in = inward(he);
            let n_out = out_of.get(&v.0).map(|&nx| inward(nx)).unwrap_or(n_in);
            let denom = 1.0 + n_in.dot(n_out);
            let d = if denom.abs() < 1e-4 { n_out } else { (n_in + n_out) / denom };
            (v, d)
        }).collect()
    }

    /// REGION inset (Blender `bpy.ops.mesh.inset(use_individual=False)` — the `I` DEFAULT): the whole
    /// selection is ONE region, so only its outer boundary is inset and shared interior edges survive.
    /// Returns the created faces (rim quads + the re-created region faces).
    ///
    /// Contrast [`inset_face`](Self::inset_face) applied per-face = Blender's **Individual** (`I,I`),
    /// where every face gets its own full ring.
    /// Oracle: `docs/planning/poly/inset_region_ref.py` -> `inset_region_ref.json`.
    ///
    /// Returns [`InsetRegionResult`] — the created faces, plus each NEW ring vertex paired with its
    /// even-offset displacement per unit thickness (`pos = orig + dir * thickness`, dirs point inward).
    ///
    /// **Signed thickness (body corners never move):**
    /// * `thickness > 0` — classic **inset**: new ring moves **inward**.
    /// * `thickness < 0` — **outset / outer segment**: new ring moves **outside** the original
    ///   boundary; original (shared) body verts stay fixed so adjacent faces are not flared. Rim
    ///   faces are the outer segment ready to thicken. Rim winding is flipped so normals stay valid.
    /// * `thickness == 0` — topology only (drag start).
    pub fn inset_region(&mut self, faces: &std::collections::HashSet<FaceId>, thickness: f32, depth: f32) -> InsetRegionResult {
        use std::collections::HashMap;
        let mut live: Vec<FaceId> = faces.iter().copied()
            .filter(|&f| (f.0 as usize) < self.faces.len() && self.face(f).half_edge != NO_EDGE)
            .collect();
        live.sort_by_key(|f| f.0);
        if live.is_empty() { return InsetRegionResult::default(); }
        let live_set: std::collections::HashSet<FaceId> = live.iter().copied().collect();

        let dirs = self.inset_region_offset_dirs(&live_set);
        if dirs.is_empty() { return InsetRegionResult::default(); }

        // Average region normal drives `depth`.
        let mut navg = Vec3::ZERO;
        for &f in &live { navg += self.compute_face_normal(f); }
        let navg = navg.normalize_or_zero();

        // Boundary edges, directed with the region's winding — the rim strip.
        let bnd: Vec<(VertexId, VertexId)> = self.region_boundary_half_edges(&live_set)
            .into_iter()
            .map(|he| (self.he_source(he), self.he(he).vertex))
            .collect();

        let outward = thickness < 0.0;

        // NEW ring verts only move. Original boundary verts stay put (body not deformed).
        // dirs point inward → thickness < 0 places the ring outside.
        // `seed` = every vertex this op can give a new normal to = every vertex it ALLOCATES plus
        // every corner of every face it CREATES. It replaces the full-mesh `recompute_normals()`
        // this op used to end with (see `inset_recompute_normals_local`). Allocated verts must be
        // in it even when nothing ends up referencing them: on a PINCHED region a boundary vertex
        // is the end of two boundary edges, so `dirs` names it twice, the second `map.insert`
        // discards the first allocation, and that vertex is left loose. The full recompute wrote
        // `Vec3::Y` there (no faces => no terms); the region pass never visits it at all.
        let mut map: HashMap<u32, VertexId> = HashMap::new();
        let mut seed: Vec<VertexId> = Vec::with_capacity(dirs.len() * 5);
        for &(v, d) in &dirs {
            let p = self.vert(v).position + d * thickness + navg * depth;
            let nrm = self.vert(v).normal;
            let nv = self.alloc_vertex(HVertex { position: p, normal: nrm, half_edge: NO_EDGE });
            seed.push(nv);
            map.insert(v.0, nv);
        }

        // Island always on the NEW ring (Blender). Read rings BEFORE deleting.
        let island_rings: Vec<Vec<VertexId>> = live.iter()
            .map(|&f| self.face_vertices(f).iter().map(|&v| *map.get(&v.0).unwrap_or(&v)).collect())
            .collect();

        // BATCH DELETE — one free/repoint pass for the whole region, not one per face.
        // `delete_face` ends in a vertex-disk repair whose scan is bounded by the WHOLE mesh, so a
        // 64-face region ran 64 of them and the delete alone scaled with the mesh: at 9216 f it was
        // 5.67 ms of a 5.90 ms `inset_region`, i.e. ~90 % of the op (1024 f: 0.573 ms; 4096 f:
        // 2.419 ms). `delete_faces` frees the same slots and then answers every touched vertex in
        // ONE ascending half-edge pass — 0.228 ms at 9216 f, 24.8x.
        //
        // The result is BIT-IDENTICAL to the loop it replaces, not merely equivalent: `live` is
        // sorted, `delete_faces` now frees in that same ascending id order (euler.rs — this switch
        // is exactly what that determinism fix unblocked), free-list push order is therefore
        // identical, and so is every id `create_face_no_normals` pops back off below. The deferred
        // disk repair lands on the same answer too — a replacement half-edge sourced at `v` belongs
        // to a face that has `v` as a corner, so if the per-face loop's pick was itself deleted a
        // later face's repair re-answered it; batching just skips the intermediate answers.
        // Verified: `delete_faces(&set)` and the sorted `delete_face` loop digest identically at
        // 1024 / 4096 / 9216 f, and the whole `inset_region` digest is unchanged.
        self.delete_faces(&live_set);

        // BATCH INSTALL — no per-face normal pass. `create_face` ends in a FULL-MESH
        // `recompute_normals()` (euler.rs:374), so a 64-face region ran ~100 of them and the op
        // scaled with the WHOLE mesh even though the selection was constant (9216 f: 83 ms, of
        // which one full recompute is 0.38 ms). `create_face_no_normals` is the documented batch
        // form and allocates `material_id: 0` exactly as `create_face` does, so the installed
        // faces are byte-identical; one region pass at the end replaces all ~100 recomputes.
        let mut out = Vec::new();
        let mut rim = Vec::new();
        for (a, b) in bnd {
            if let (Some(&na), Some(&nb)) = (map.get(&a.0), map.get(&b.0)) {
                // Classic rim winding [a,b,nb,na]: edge a→b twins the adjacent side's boundary
                // HE b→a. Do NOT reverse for outward — that re-uses b→a same-winding as the side
                // and goes non-manifold. Body corners (a,b) never move; only na/nb slide.
                if let Some(f) = self.create_face_no_normals(&[a, b, nb, na]) {
                    out.push(f);
                    rim.push(f);
                    seed.extend_from_slice(&[a, b, nb, na]);
                }
            }
        }
        let mut inner = Vec::new();
        for ring in island_rings {
            if ring.len() >= 3 {
                if let Some(f) = self.create_face_no_normals(&ring) {
                    out.push(f);
                    inner.push(f);
                    seed.extend_from_slice(&ring);
                }
            }
        }

        self.inset_recompute_normals_local(&out, &seed);
        let boundary = dirs.iter()
            .filter_map(|&(v, d)| map.get(&v.0).map(|&nv| (nv, d)))
            .collect();
        InsetRegionResult { faces: out, inner, rim, boundary, outward }
    }

    /// ONE region normal pass that gives every face in `faces` and every vertex in `seed` exactly
    /// the normal a full `recompute_normals()` would, and leaves every other element untouched —
    /// the exact replacement for the full recompute [`inset_region`](Self::inset_region) used to
    /// end with. Two DISCOVERY gaps stand between `recompute_normals_region` and that guarantee,
    /// and each one was measured, not assumed:
    ///
    /// **1 — created faces the fan walk cannot see.** `recompute_normals_region` discovers its
    /// dirty FACE set by circulating `vertex_fan` from each seed vertex, and a fan walk truncates
    /// at a non-manifold vertex (it can only cover one umbrella). On a pinched selection a
    /// just-created face is therefore reachable from none of its own corners and keeps
    /// `create_face_no_normals`' placeholder `Vec3::Y` (measured: checkerboard region on an 8x8
    /// plane). We know the created set exactly, so stamp it through the canonical Newell helper
    /// first; the region pass re-derives the identical value for whatever it does reach, and every
    /// vertex term then reads a correct face normal.
    ///
    /// **2 — a seed vertex with no incident live face.** It is a corner of no dirty face, so the
    /// region pass never visits it and it keeps the normal it was allocated with. The full weighted
    /// pass reaches every live vertex and writes `Vec3::Y` for exactly that "no terms" case. That
    /// is the loose ring vertex a pinched region leaks (see `inset_region`'s `seed` comment).
    ///
    /// **A third item — the one-ring COLLAR snapshot — was RETIRED 2026-08-09 because its root
    /// cause was fixed.** The region pass expands one ring past its seed and recomputes those
    /// neighbours too, and it used to weight any fan face outside its own dirty set by a hardcoded
    /// fallback `area = 1.0` instead of that face's real Newell area — so wherever the mesh is not
    /// locally coplanar the collar came back DIFFERENT from the full recompute this replaces
    /// (10 of 12 probe configurations moved). This function worked around that by snapshotting the
    /// collar and restoring it afterwards, which is only sound because inset never moves an
    /// existing vertex. `normals.rs` now derives each collar face's real `(Newell normal, area)`
    /// on the spot and feeds the full pass's own kernel, so the pass writes the collar the same
    /// value the snapshot held. Re-measured before deleting the restore: the snapshot covered 4–22
    /// collar vertices on plane / box / uv-sphere / torus / cylinder / cone and **not one of them
    /// differed** (max |restored − recomputed| = 0.000000 in every configuration), i.e. the restore
    /// had become a no-op. Keeping it would now be the bandaid — it would pin the collar to a stale
    /// value the moment `recompute_normals_region` legitimately needs to move one.
    fn inset_recompute_normals_local(&mut self, faces: &[FaceId], seed: &[VertexId]) {
        // (1) Every face we created, through the ONE Newell definition the full pass also uses.
        let (mut fring, mut fpts) = (Vec::new(), Vec::new());
        for &f in faces {
            let n = self.compute_face_normal_into(f, &mut fring, &mut fpts);
            self.faces[f.0 as usize].normal = n;
        }
        // (2) Seed vertices with no live incident face, collected BEFORE the pass (it is the pass
        // that would otherwise leave them untouched). One live face is enough to disqualify a
        // vertex, so this stops at the first hit instead of walking the whole fan — the full
        // corner walk it used to do existed only to collect the collar snapshot, now retired.
        let mut loose: Vec<usize> = Vec::new();
        let (mut fan, mut ring) = (Vec::new(), Vec::new());
        for &v in seed {
            if !self.is_vert_live(v) { continue; }
            self.vertex_fan_into(v, &mut fan);
            let mut has_face = false;
            for i in 0..fan.len() {
                let f = self.he(fan[i]).face;
                if f == NO_FACE || (f.0 as usize) >= self.faces.len() { continue; }
                // Empty ring = retired face slot; `recompute_normals_region` skips those too.
                self.face_vertices_into(f, &mut ring);
                if ring.is_empty() { continue; }
                has_face = true;
                break;
            }
            if !has_face { loose.push(v.0 as usize); }
        }
        self.recompute_normals_region(seed.iter().copied());
        for i in loose {
            self.vertices[i].normal = Vec3::Y;
        }
    }

    /// Reshape a face region's BOUNDARY LOOP onto a CIRCLE — Blender's LoopTools **Circle**
    /// (`bpy.ops.mesh.looptools_circle`). PURE RESHAPE: it only moves existing vertices, so vertex /
    /// edge / face counts, every id, and watertightness are all preserved (no `from_polygons` rebuild —
    /// feedback_half_edge_hard_frozen).
    ///
    /// It is NOT an inset mode: it reshapes the CURRENT selection in place, so it composes as
    /// `select region -> circle_region -> inset_region -> extrude_region` to build a round boss/hole
    /// out of a grid.
    ///
    /// * `radius` — `None` = Blender's default auto radius (the least-squares best fit);
    ///   `Some(r)` = Blender's `custom_radius` override (the fitted CENTRE is still used).
    /// * `flatten` — Blender's default `true`: the loop lands exactly in its best-fit plane. `false`
    ///   keeps each vertex's original offset ALONG the plane normal (see the divergence note below).
    /// * `regular` — Blender's default `true`: vertices are spread at equal angles. `false` = each
    ///   vertex keeps its own angle.
    /// * `relax_passes` — an AZRA EXTENSION (Blender's Circle never touches the interior); see below.
    ///
    /// # The algorithm (verified against the real addon — `docs/planning/poly/circle_ref.py`)
    /// Per boundary loop: centroid `com` + best-fit plane; project the loop into that plane;
    /// least-squares best-fit circle -> `(fitted_centre, r)`; each vertex's DIRECTION is taken about
    /// `fitted_centre`; the vertex is then placed at that direction × `r` **from `com`**.
    ///
    /// **The circle is centred on the loop CENTROID, not on the fitted centre** — LoopTools computes
    /// `(x0, y0, r)` and then only ever uses `x0, y0` for the angular reference: `circle_project_regular`
    /// writes back `[cos(t)*r, sin(t)*r]` with no `+x0, +y0`, and those 2D coords are relative to `com`.
    /// This is faithfully reproduced. It also means the radius is the best fit about a centre that is
    /// then discarded — so `r` is NOT the mean distance from `com`, though the two COINCIDE whenever the
    /// loop is symmetric about its centroid (a grid region on a box face, the common case). On the
    /// asymmetric oracle case they differ: Blender/AZRA give 1.138111, the mean gives 1.137707.
    ///
    /// With `regular`, the ANCHOR (the loop vertex closest to `com`) keeps its own angle and the rest
    /// are spread at `2π/n` from it, in the loop's traversal direction. The result is independent of
    /// the in-plane basis, of the plane normal's sign, and of the traversal direction; it depends ONLY
    /// on the anchor. When several vertices tie for closest, the choice is implementation-defined (it
    /// is bmesh-order-dependent in Blender too) and rotates the circle by a sub-step angle at most.
    ///
    /// # Deliberate divergences from LoopTools (all documented in POLY_POLISH_PLAN §7)
    /// * **Plane fit**: we use the loop's Newell normal. LoopTools does a PCA of the loop, whose
    ///   covariance is SINGULAR for the planar loops this tool actually targets — it then falls back to
    ///   the nearest AXIS-ALIGNED normal, which is only correct for axis-aligned planes. Newell is exact
    ///   for any planar loop (so: identical on every planar region) and sane for a non-planar one.
    /// * **`flatten = false`**: we preserve each vertex's original along-normal offset. LoopTools
    ///   ray-casts the new in-plane point back onto the adjacent faces. Non-default on both sides, and
    ///   identical to `flatten = true` on a planar region.
    /// * **`relax_passes`**: Blender's Circle leaves the interior grid untouched (verified: only the 16
    ///   loop verts move). `relax_passes > 0` additionally relaxes the region's INTERIOR vertices so the
    ///   grid does not shear against the new circular boundary — pinned-boundary Laplacian, which is not
    ///   a fudge: its fixed point is the harmonic (Laplace) solution of the region with the circle as
    ///   Dirichlet boundary, i.e. the classic evenly-distributed grid-in-a-disk. Passes are JACOBI (every
    ///   vertex reads the previous pass), so the result never depends on iteration order.
    ///
    /// Returns [`CircleRegionResult`]; `loops` is EMPTY and the mesh is UNCHANGED when the region's
    /// boundary is not a set of simple closed loops (pinch / dead-end) or its plane is degenerate.
    /// Each loop pairs `(VertexId, unit radial dir)` by IDENTITY so a future radius handle is just
    /// `pos = center + dir * radius` (POLY_POLISH_PLAN §8 — never re-derive the pairing from ring order).
    pub fn circle_region(&mut self, faces: &std::collections::HashSet<FaceId>, radius: Option<f32>,
                         flatten: bool, regular: bool, relax_passes: u32) -> CircleRegionResult {
        let loops = self.region_boundary_loops(faces);
        if loops.is_empty() { return CircleRegionResult::default(); }

        let mut live: Vec<FaceId> = faces.iter().copied()
            .filter(|&f| (f.0 as usize) < self.faces.len() && self.face(f).half_edge != NO_EDGE)
            .collect();
        live.sort_by_key(|f| f.0);
        self.circle_reshape_loops(&loops, &live, radius, flatten, regular, relax_passes)
    }

    /// THE shared Circle reshape core — the fit + layout + relax math that BOTH [`circle_region`]
    /// (face selection) and [`circle_edge_loop`](Self::circle_edge_loop) (edge-loop selection) come
    /// through, so the geometry lives in exactly ONE place (CLAUDE.md: never two code paths for one
    /// operation — the circle op already ate a duplicated-kernel round; see the `apply_circle` app-layer
    /// doc comment).
    ///
    /// [`circle_region`]: Self::circle_region
    ///
    /// * `loops` — ORDERED, closed vertex loops: from [`region_boundary_loops`](Self::region_boundary_loops)
    ///   for a face selection, or [`edge_loop_rings`](Self::edge_loop_rings) for an edge selection.
    /// * `live` — the region's faces. Used ONLY for the degenerate-normal fallback and the interior
    ///   RELAX pass (both are face concepts). Pass `&[]` for a pure edge loop: it has no enclosed
    ///   interior to relax, so relax becomes a correct no-op and the Newell loop normal drives the fit.
    ///
    /// The fit/layout/relax math is IDENTICAL regardless of caller. See [`circle_region`] for the full
    /// algorithm and its deliberate divergences from LoopTools.
    fn circle_reshape_loops(&mut self, loops: &[Vec<VertexId>], live: &[FaceId], radius: Option<f32>,
                            flatten: bool, regular: bool, relax_passes: u32) -> CircleRegionResult {
        use std::collections::{HashMap, HashSet};

        // ---- fit + reshape every boundary loop --------------------------------------------------
        let mut fits: Vec<CircleLoopFit> = Vec::new();
        let mut moves: Vec<(VertexId, Vec3)> = Vec::new();
        for ring in loops {
            let pts: Vec<Vec3> = ring.iter().map(|&v| self.vert(v).position).collect();
            let com = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;

            // Best-fit plane normal: the loop's own Newell normal, falling back to the region's
            // area-weighted face normal for a degenerate (collinear / figure-eight) ring.
            let mut normal = newell_normal(&pts).normalize_or_zero();
            if normal == Vec3::ZERO {
                let mut acc = Vec3::ZERO;
                for &f in live {
                    let fp: Vec<Vec3> = self.face_vertices(f).iter().map(|&v| self.vert(v).position).collect();
                    acc += newell_normal(&fp); // unnormalized => area-weighted
                }
                normal = acc.normalize_or_zero();
            }
            if normal == Vec3::ZERO { return CircleRegionResult::default(); } // no plane — refuse

            // Orthonormal in-plane basis. Any basis gives the SAME result (see the doc comment), so
            // unlike LoopTools we do not need its unnormalized p/q and their 1/|p| bookkeeping.
            let p = normal.any_orthonormal_vector();
            let q = normal.cross(p);
            let to_2d = |w: Vec3| -> Vec2 { let l = w - com; Vec2::new(p.dot(l), q.dot(l)) };
            let locs: Vec<Vec2> = pts.iter().map(|&w| to_2d(w)).collect();

            // ANCHOR = the loop vertex closest to com, measured in 3D BEFORE projection (LoopTools'
            // `circle_shift_loop`). Ties resolve to the lowest loop index, as they do in Blender.
            let anchor = (0..pts.len())
                .min_by(|&a, &b| {
                    let da = (pts[a] - com).length_squared();
                    let db = (pts[b] - com).length_squared();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(0);

            let (fc, fit_r) = best_fit_circle_2d(&locs);
            let r = radius.unwrap_or(fit_r);
            if !r.is_finite() || r <= 1e-9 { return CircleRegionResult::default(); }

            let n = locs.len();
            // Directions are taken about the FITTED centre; positions are laid out from `com`.
            let dir_2d = |i: usize| -> Vec2 { (locs[i] - fc).normalize_or_zero() };
            let mut new_2d: Vec<Vec2> = vec![Vec2::ZERO; n];
            if regular {
                let a0 = dir_2d(anchor);
                if a0 == Vec2::ZERO { return CircleRegionResult::default(); } // anchor AT the centre
                let offset = a0.y.atan2(a0.x);
                // Traversal direction of the ring about the fitted centre.
                let a1 = dir_2d((anchor + 1) % n);
                let ccw = if a0.x * a1.y - a0.y * a1.x >= 0.0 { 1.0f32 } else { -1.0f32 };
                for step in 0..n {
                    let t = offset + ccw * (step as f32 / n as f32) * std::f32::consts::TAU;
                    new_2d[(anchor + step) % n] = Vec2::new(t.cos(), t.sin()) * r;
                }
            } else {
                for i in 0..n {
                    let d = dir_2d(i);
                    if d == Vec2::ZERO { return CircleRegionResult::default(); }
                    new_2d[i] = d * r;
                }
            }

            let mut boundary: Vec<(VertexId, Vec3)> = Vec::with_capacity(n);
            for i in 0..n {
                let dir3 = (p * new_2d[i].x + q * new_2d[i].y).normalize_or_zero();
                // `flatten` = land in the plane. Otherwise keep the vertex's own along-normal offset.
                let off = if flatten { 0.0 } else { (pts[i] - com).dot(normal) };
                moves.push((ring[i], com + dir3 * r + normal * off));
                boundary.push((ring[i], dir3));
            }
            fits.push(CircleLoopFit { center: com, normal, radius: r, boundary });
        }
        for (v, pos) in moves { self.vert_mut(v).position = pos; }

        // ---- relax the region INTERIOR (AZRA extension; Blender leaves it alone) -----------------
        let mut relaxed: Vec<VertexId> = Vec::new();
        if relax_passes > 0 {
            // 1-ring adjacency WITHIN the region. An interior vertex has every incident face in the
            // region, so its whole 1-ring is a region vertex => this adjacency is complete for it.
            let mut adj: HashMap<u32, Vec<VertexId>> = HashMap::new();
            for &f in live {
                let ring = self.face_vertices(f);
                for i in 0..ring.len() {
                    let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                    adj.entry(a.0).or_default().push(b);
                    adj.entry(b.0).or_default().push(a);
                }
            }
            for ns in adj.values_mut() { ns.sort_by_key(|v| v.0); ns.dedup_by_key(|v| v.0); }

            let pinned: HashSet<u32> = loops.iter().flatten().map(|v| v.0).collect();
            let mut interior: Vec<VertexId> = adj.keys().copied()
                .filter(|v| !pinned.contains(v))
                .map(VertexId)
                .collect();
            interior.sort_by_key(|v| v.0);

            // Constrain to the fit plane only when there IS a single plane to constrain to.
            let plane = if flatten && fits.len() == 1 { Some((fits[0].center, fits[0].normal)) } else { None };
            if let Some((com, normal)) = plane {
                // Flatten the interior too, so the whole region is planar (and the Laplacian of
                // coplanar points then stays exactly coplanar).
                for &v in &interior {
                    let pos = self.vert(v).position;
                    self.vert_mut(v).position = pos - normal * (pos - com).dot(normal);
                }
            }
            for _ in 0..relax_passes {
                // JACOBI: compute every new position from the PREVIOUS pass, then write. A
                // Gauss-Seidel in-place update would make the result depend on iteration order.
                let next: Vec<(VertexId, Vec3)> = interior.iter().filter_map(|&v| {
                    let ns = adj.get(&v.0)?;
                    if ns.is_empty() { return None; }
                    let mut sum = Vec3::ZERO;
                    for &nb in ns { sum += self.vert(nb).position; }
                    let mut pos = sum / ns.len() as f32;
                    if let Some((com, normal)) = plane { pos -= normal * (pos - com).dot(normal); }
                    Some((v, pos))
                }).collect();
                for (v, pos) in next { self.vert_mut(v).position = pos; }
            }
            relaxed = interior;
        }

        self.recompute_normals();
        CircleRegionResult { loops: fits, relaxed }
    }

    /// Reshape a SELECTED EDGE LOOP onto a circle — Blender LoopTools **Circle** in EDGE select mode
    /// (Alt+C with an edge loop selected). Verified vertex-for-vertex against
    /// `bpy.ops.mesh.looptools_circle` on the boundary edge loop (oracle: the edge case matches the
    /// face-mode oracle by IDENTITY — same loop, same fit, max per-vertex delta 0.0).
    ///
    /// Shares the ONE reshape producer with [`circle_region`](Self::circle_region) via
    /// [`circle_reshape_loops`](Self::circle_reshape_loops): `edge_loop_rings` supplies the loops, then
    /// the identical fit/layout runs. `live = &[]` — a pure edge loop encloses no interior, so `relax`
    /// (a face concept) is correctly a no-op; the loop's own Newell normal drives the fit.
    ///
    /// Returns [`CircleRegionResult`] with `loops` EMPTY and the mesh UNCHANGED when the selection is
    /// not a set of simple closed loops (see [`edge_loop_rings`](Self::edge_loop_rings)).
    pub fn circle_edge_loop(&mut self, edges: &std::collections::HashSet<HalfEdgeId>, radius: Option<f32>,
                            flatten: bool, regular: bool, relax_passes: u32) -> CircleRegionResult {
        let loops = self.edge_loop_rings(edges);
        if loops.is_empty() { return CircleRegionResult::default(); }
        self.circle_reshape_loops(&loops, &[], radius, flatten, regular, relax_passes)
    }

    /// Inset a face by `thickness` using Blender's **EVEN OFFSET** (`bpy.ops.mesh.inset`, the UI default
    /// `use_even_offset=True`), then raise/lower the inset ring by `depth` along the face normal.
    /// Returns the new inner face.
    ///
    /// Even offset = push every edge inward along its **in-plane** normal by `thickness` and intersect
    /// the two offset lines meeting at each vertex, so every edge keeps a UNIFORM gap. Closed form at a
    /// vertex whose incoming/outgoing edges have inward in-plane normals `n1`,`n2`:
    /// ```text
    ///     v' = v + thickness * (n1 + n2) / (1 + dot(n1, n2))
    /// ```
    /// (Solves dot(v'-v, n1) = dot(v'-v, n2) = thickness.) Inward normal of edge a->b on a CCW /
    /// outward-wound face is `normal × (b - a)`.
    ///
    /// The old code slid each vertex toward the face CENTROID, which only coincides with Blender on a
    /// REGULAR polygon: on the 4x2 oracle face it produced uneven 0.268/0.134 edge gaps instead of a
    /// uniform 0.3 (`inner ring ±1.732/±0.866` vs Blender's `±1.7/±0.7`). The topology was already right,
    /// so watertight+manifold gates passed while the geometry was wrong — Blender audit #15.
    /// Oracle: `docs/planning/poly/inset_ref.py` -> `inset_ref.json`.
    pub fn inset_face_depth(&mut self, f: FaceId, thickness: f32, depth: f32) -> FaceId {
        // Single-face path shares region inset (signed thickness / outset topology).
        let set: std::collections::HashSet<FaceId> = std::iter::once(f).collect();
        let res = self.inset_region(&set, thickness, depth);
        res.inner.first().copied().unwrap_or(f)
    }

    /// Max **positive** (inward) thickness before consecutive ring verts would meet/cross.
    ///
    /// Used by interactive inset to clamp the drag and then **merge** verts at the clamp point
    /// (owner 2026-08-06 / EDGE CASE INSET). Bevel deliberately does NOT merge at clamp so the
    /// profile can still be adjusted — inset wants clean topology once the ring collapses.
    ///
    /// `orig` / `dirs` are the live ring at thickness=0 (identity-paired), same order as returned
    /// by [`inset_region`](Self::inset_region).dirs point inward (even-offset scale included).
    pub fn inset_max_inward_thickness(orig: &[(u32, Vec3)], dirs: &[Vec3]) -> f32 {
        let n = orig.len().min(dirs.len());
        if n < 3 {
            return f32::MAX;
        }
        let mut tmax = f32::MAX;
        for i in 0..n {
            let j = (i + 1) % n;
            let oi = orig[i].1;
            let oj = orig[j].1;
            let di = dirs[i];
            let dj = dirs[j];
            let edge = oj - oi;
            let elen = edge.length();
            if elen < 1e-8 {
                continue;
            }
            let ehat = edge / elen;
            // Edge-closing rate under even-offset motion (positive = edge shrinks).
            let rate = di.dot(ehat) - dj.dot(ehat);
            if rate > 1e-6 {
                tmax = tmax.min(elen / rate);
            }
            // Ray meeting at acute corners (dirs converge).
            let rd = di - dj;
            let rd2 = rd.length_squared();
            if rd2 > 1e-10 {
                let t_meet = (oj - oi).dot(rd) / rd2;
                if t_meet > 1e-5 {
                    let pi = oi + di * t_meet;
                    let pj = oj + dj * t_meet;
                    if (pi - pj).length() < elen * 0.08 + 1e-4 {
                        tmax = tmax.min(t_meet);
                    }
                }
            }
        }
        // Collapse toward region centroid: never let the ring pass the center.
        let centroid: Vec3 = orig.iter().map(|(_, p)| *p).sum::<Vec3>() / n as f32;
        for i in 0..n {
            let di = dirs[i];
            let d2 = di.length_squared();
            if d2 < 1e-12 {
                continue;
            }
            let t_c = (centroid - orig[i].1).dot(di) / d2;
            if t_c > 1e-5 {
                tmax = tmax.min(t_c * 0.98);
            }
        }
        if !tmax.is_finite() || tmax < 0.0 {
            0.0
        } else {
            tmax
        }
    }

    // ==========================================================================================
    // Offset Edge Loop — plan `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md`
    // ==========================================================================================

    /// The selected edges as ONE ordered vertex chain, plus whether that chain CLOSES.
    ///
    /// **Closed selections go through [`edge_loop_rings`](Self::edge_loop_rings) and nothing else**,
    /// so a closed loop takes byte-for-byte the path P1 shipped — same walk, same orientation, same
    /// ids. Only its EMPTY return (which it uses for "some vertex has degree != 2", nav.rs:428)
    /// falls through to the open walk below. That degree test is exactly what refused the owner's
    /// silhouette chain — a run over a bevel with two free endpoints — with "not a simple CLOSED
    /// edge loop"; open chains are legal input per plan §3.3.
    ///
    /// Refuses a fork (degree > 2), several disjoint chains, and a walk that fails to cover the
    /// selection — fail closed, never a partial answer (plan §3.2).
    fn offset_loop_chain(
        &self,
        edges: &std::collections::HashSet<HalfEdgeId>,
    ) -> Result<(Vec<VertexId>, bool), String> {
        use std::collections::{BTreeMap, BTreeSet};

        let mut rings = self.edge_loop_rings(edges);
        if rings.len() == 1 {
            return Ok((rings.pop().expect("len checked == 1"), true));
        }
        if rings.len() > 1 {
            return Err(format!(
                "this selection walked into {} disjoint CLOSED loops. Each ring's side is derived \
                 from its OWN walk orientation, so offsetting several at once could silently send \
                 them to opposite geometric sides — refused rather than guessed.",
                rings.len()
            ));
        }

        // ---- open chain (plan §3.3) --------------------------------------------------------
        // Dedup half-edge + twin to UNDIRECTED geometric edges. Ordered containers throughout: a
        // raw `HashSet` iteration picks a random winner between a half-edge and its twin, which
        // decides the walk orientation — nav.rs:393 records that as a measured, live corruption
        // source ("4 refused and 1 committed a NON-MANIFOLD mesh" across 5 identical runs).
        let mut und: BTreeSet<(u32, u32)> = BTreeSet::new();
        for &he in edges {
            if !self.is_edge_live(he) {
                continue;
            }
            let a = self.he_source(he);
            let b = self.he(he).vertex;
            if a == b {
                continue; // degenerate — no endpoints to walk
            }
            und.insert((a.0.min(b.0), a.0.max(b.0)));
        }
        if und.is_empty() {
            return Err("the selection holds no live edges".to_string());
        }

        let mut adj: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for &(a, b) in &und {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
        for (&v, ns) in adj.iter_mut() {
            ns.sort_unstable();
            if ns.len() > 2 {
                return Err(format!(
                    "vertex {} carries {} selected edges — the chain forks there and the offset \
                     side is ambiguous. Select a single loop or a single chain.",
                    v,
                    ns.len()
                ));
            }
        }

        let ends: Vec<u32> = adj.iter().filter(|(_, ns)| ns.len() == 1).map(|(&v, _)| v).collect();
        if ends.len() != 2 {
            return Err(format!(
                "an open chain must have EXACTLY two free endpoints; this selection has {}. \
                 (A closed loop is handled separately; {} endpoints means several disjoint chains, \
                 or a walk that never closed.)",
                ends.len(),
                ends.len()
            ));
        }

        // Deterministic start = the LOWER-id endpoint (`ends` comes out of a BTreeMap, so it is
        // already sorted). The sign then flips the side by reversing this chain, exactly as it does
        // for a closed ring.
        let start = ends[0];
        let mut chain: Vec<VertexId> = vec![VertexId(start)];
        let (mut prev, mut cur) = (None::<u32>, start);
        loop {
            let ns = adj.get(&cur).ok_or_else(|| format!("vertex {cur} left the adjacency"))?;
            let nxt = match ns.iter().copied().find(|&x| Some(x) != prev) {
                Some(x) => x,
                None => break, // walked back into the endpoint we came from
            };
            prev = Some(cur);
            cur = nxt;
            chain.push(VertexId(cur));
            if adj.get(&cur).map(|n| n.len()) == Some(1) {
                break; // reached the other free endpoint
            }
            if chain.len() > adj.len() {
                return Err("the chain walk revisited a vertex (tangled selection)".to_string());
            }
        }
        if chain.len() != adj.len() {
            return Err(format!(
                "the selection is not ONE connected chain: the walk covered {} of {} vertices.",
                chain.len(),
                adj.len()
            ));
        }
        if chain.len() < 2 {
            return Err("a chain needs at least one edge".to_string());
        }
        Ok((chain, false))
    }

    /// Rotate around `v` on the CHOSEN side, from the rung `cuts[from]` rides to the rung that ends
    /// at `to_rail`, planning a cut on every rung crossed and a split of every face between them.
    /// Returns the cut index on the FINAL rung (the one face `to_rail` belongs to expects).
    ///
    /// **This is plan §3.2's "a `None` mid-chain must not be skipped" case, answered topologically.**
    /// A chain vertex whose two chosen-side faces do NOT share a rung (valence > 4 on that side) has
    /// several off-loop edges there, with one or more faces between them. The offset loop has to
    /// travel THROUGH those faces — it cannot be one vertex, because a single vertex cannot lie on
    /// two different edges. Emitting one point (per the plan's averaged direction) would put it
    /// inside no edge at all, so the strip could not be closed by
    /// [`split_face_between`](Self::split_face_between) and the loop would pinch. Riding each rung
    /// in turn keeps the loop CONTINUOUS and every new vertex exactly on its own rail.
    ///
    /// Fails closed when the fan runs into a mesh boundary before reaching `to_rail` — there is then
    /// no path across this vertex on this side at all (plan §3.2's "decline the whole op").
    #[allow(clippy::too_many_arguments)]
    fn offset_loop_fan(
        &self,
        v: VertexId,
        from: usize,
        to_rail: VertexId,
        chain_edges: &std::collections::HashSet<(u32, u32)>,
        cuts: &mut Vec<OffsetCut>,
        index: &mut std::collections::HashMap<(u32, u32), usize>,
        splits: &mut Vec<OffsetSplit>,
    ) -> Result<usize, String> {
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut o = cuts[from].he; // sourced at `v` by construction (see the caller)
        let mut prev_cut = from;
        for _ in 0..=self.half_edges.len() {
            if !seen.insert(o.0) {
                return Err(format!(
                    "the edge fan around vertex {} loops without reaching its rail {} — refused \
                     rather than emitting a partial loop.",
                    v.0, to_rail.0
                ));
            }
            let tw = self.he(o).twin;
            if tw == NO_TWIN || !self.is_edge_live(tw) {
                return Err(format!(
                    "vertex {} has more than one off-chain edge on the chosen side and the fan \
                     between them is cut by a MESH BOUNDARY, so the offset loop cannot cross it. \
                     Refused before mutating (plan §3.2 fail-closed).",
                    v.0
                ));
            }
            let nxt = self.he(tw).next;
            if nxt == NO_EDGE || !self.is_edge_live(nxt) || nxt == tw {
                return Err(format!("malformed face ring at vertex {} while crossing its edge fan", v.0));
            }
            let g = self.he(nxt).face;
            if g == NO_FACE {
                return Err(format!(
                    "vertex {}'s edge fan crosses an open boundary region on the chosen side.",
                    v.0
                ));
            }
            debug_assert_eq!(self.he_source(nxt), v, "a fan step must stay sourced at the vertex");
            let rail = self.he(nxt).vertex;
            let ci = offset_loop_register_cut(
                cuts,
                index,
                chain_edges,
                OffsetCut { he: nxt, chain_v: v, rail_v: rail, from_source: true },
            )?;
            if ci == prev_cut {
                return Err(format!("degenerate edge fan at vertex {} (a face bounded twice by one edge)", v.0));
            }
            // The strip crosses `g` as the triangle [prev cut, v, this cut] — a corner cut, which
            // is exactly what an offset loop does at an irregular vertex.
            splits.push(OffsetSplit { face: g, from: prev_cut, a: v, b: None, to: ci });
            if rail == to_rail {
                return Ok(ci);
            }
            prev_cut = ci;
            o = nxt;
        }
        Err(format!("the edge fan around vertex {} did not terminate", v.0))
    }

    /// **Offset Edge Loop** — Maya's *Offset Edge Loop Tool* / Blender's *Offset Edge Loop Cut*
    /// (`Ctrl+Shift+R`). Given a selected edge loop — CLOSED, or an OPEN chain with two free
    /// endpoints — insert ONE parallel loop on ONE side at an ABSOLUTE world distance and **KEEP the
    /// original loop**. Returns the new vertices in the order the source chain was walked, or a
    /// refusal reason.
    ///
    /// Plan: `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` — **P1 + P2**. Proportional mode and the
    /// symmetric pair are P3 and are absent rather than approximated.
    ///
    /// NOT a bevel and must never be fused into one (plan §5): a 1-segment edge bevel CONSUMES the
    /// original edge, this keeps it. It is also not Loop Cut (different axis, seeded by a ring).
    ///
    /// # Sign = side
    /// `|distance|` is the world-space offset; its SIGN picks which adjacent face the new loop lands
    /// in (the plan's "crossing zero flips the loop to the other adjacent face"). The sign is applied
    /// by REVERSING the walk, so the "+" side of the reversed chain *is* the "−" side of the original
    /// and there is exactly ONE placement/split code path instead of two mirrored ones. This holds
    /// identically for an open chain (reversing swaps which face bounds each edge).
    ///
    /// # Geometry (plan §3.1)
    /// `V' = V + s · (rail − V)` with `s = min(d / |rail − V|, 1 − ε)`, i.e. `V + d · normalize(rail
    /// − V)` clamped **PER VERTEX**. The clamp is deliberately per-vertex: clamping the whole loop to
    /// the narrowest rail would let one tight quad dictate the band width and destroy the
    /// uniform-band property absolute mode exists for. Unclamped vertices land at EXACTLY `d`, and
    /// placement is a **pure scaling in `d`** below the clamp — the app's drag tool binds its
    /// drag→sign mapping to a tiny probe and depends on that property.
    ///
    /// # Where the rail comes from (plan §3.2)
    /// The rail is read ONE-SIDEDLY from the chosen-side face's own ring: for the chain half-edge
    /// `h` bounding face `f`, `h.prev`'s source is the rail at `h`'s start and `h.next`'s
    /// destination is the rail at `h`'s end. [`edge_slide_rails`](Self::edge_slide_rails) remains
    /// the single home for the TWO-sided rail query and this function's answer is pinned to agree
    /// with it by a `debug_assert` wherever it is defined — but it returns `None` for a BOUNDARY
    /// edge, and it does so because the *other* side is missing, not this one. Consuming that `None`
    /// as "no rail" is what made P1 refuse every chain touching an open rim. The face ring always
    /// knows the chosen side's rail.
    ///
    /// A vertex whose two chosen-side faces ride DIFFERENT rungs (valence > 4 there) is handled by
    /// [`offset_loop_fan`](Self::offset_loop_fan) — one new vertex per rung, with the faces between
    /// them split too, so the loop stays continuous instead of pinching.
    ///
    /// # Open-chain ends (plan §3.3)
    /// A chain of `k` edges yields `k + 1` new vertices. The terminal rung's far face gains a corner
    /// and becomes an **n-gon** — the correct, manifold result; it is deliberately NOT fanned into
    /// triangles (this kernel is n-gon-native).
    ///
    /// # Atomicity
    /// **Every check runs before the first mutation.** A refusal leaves the mesh byte-identical, so
    /// callers may probe with it. (The only post-mutation `Err` is a `debug_assert`-guarded kernel
    /// invariant break that the pre-checks prove unreachable — see the split loop.)
    ///
    /// # Add-only
    /// Both mutators used here — [`split_edge`](Self::split_edge) and
    /// [`split_face_between`](Self::split_face_between) — only ALLOCATE (verts / half-edges / faces);
    /// nothing is freed and the private free-lists are never touched. That is what lets the
    /// `mesh_ops` wrapper honestly claim the O(touched) `OpDelta::Element` tier on a dense input.
    pub fn offset_edge_loop(
        &mut self,
        edges: &std::collections::HashSet<HalfEdgeId>,
        distance: f32,
    ) -> Result<Vec<VertexId>, String> {
        use std::collections::{HashMap, HashSet};

        // Per-vertex rail clamp (plan §3.1). `s` must stay strictly inside the rail or the connecting
        // quad folds through its rail vertex.
        const RAIL_EPS: f32 = 1e-3;

        if !distance.is_finite() {
            return Err("offset distance must be finite".to_string());
        }
        let d = distance.abs();
        if d < 1e-9 {
            return Err("offset distance is zero — nothing to insert".to_string());
        }

        // ---- 1. ordered chain — closed ring OR open chain (plan §3.3) ------------------------
        let (mut ring, closed) = self.offset_loop_chain(edges)?;
        if distance < 0.0 {
            ring.reverse(); // sign = side, via ONE code path (see doc comment)
        }
        let n = ring.len();
        let ne = if closed { n } else { n - 1 }; // chain EDGES (a closed ring has one per vertex)

        // ---- 2. the chain's directed half-edges + the face each one bounds on the chosen side ---
        // Resolved from the SELECTION's own live half-edge ids rather than `find_half_edge`, whose
        // `vertex_fan` stops at a boundary (nav.rs:35) and therefore cannot see every outgoing
        // half-edge of a rim vertex — precisely the vertices an open chain ends on. Sorted +
        // `or_insert` so a non-manifold double edge resolves deterministically.
        let mut directed: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
        let mut ids: Vec<HalfEdgeId> = edges.iter().copied().collect();
        ids.sort_unstable_by_key(|e| e.0);
        for &e in &ids {
            if !self.is_edge_live(e) {
                continue;
            }
            let (a, b) = (self.he_source(e), self.he(e).vertex);
            if a == b {
                continue;
            }
            directed.entry((a.0, b.0)).or_insert(e);
            let tw = self.he(e).twin;
            if tw != NO_TWIN && self.is_edge_live(tw) {
                directed.entry((b.0, a.0)).or_insert(tw);
            }
        }

        let mut h: Vec<HalfEdgeId> = Vec::with_capacity(ne);
        for j in 0..ne {
            let (a, b) = (ring[j], ring[(j + 1) % n]);
            let he = *directed.get(&(a.0, b.0)).ok_or_else(|| {
                format!(
                    "chain edge {} -> {} has NO half-edge on the chosen side: it lies on the mesh \
                     BOUNDARY and this is the open side, so there is no face to carve the offset \
                     strip out of. Flip the sign to offset into the surface.",
                    a.0, b.0
                )
            })?;
            h.push(he);
        }

        let mut faces: Vec<FaceId> = Vec::with_capacity(ne);
        for j in 0..ne {
            let f = self.he(h[j]).face;
            if f == NO_FACE {
                return Err(format!(
                    "chain edge {} -> {} has no face on the chosen side (mesh boundary) — flip the \
                     sign to offset into the surface.",
                    ring[j].0,
                    ring[(j + 1) % n].0
                ));
            }
            if self.face_vertices(f).len() < 3 {
                return Err(format!("face {} on the chosen side is degenerate (< 3 corners)", f.0));
            }
            faces.push(f);
        }
        if faces.iter().map(|f| f.0).collect::<HashSet<u32>>().len() != ne {
            // Two chain edges bounding the SAME face on this side. Splitting one face twice would
            // invalidate the second split's ring. This is what "offset a face's own boundary loop
            // INTO that face" looks like from here — that is the Inset tool's job, not this one.
            return Err(
                "two loop edges bound the SAME face on the chosen side — the strip would have to \
                 split one face twice. Flip the sign to offset the other way (insetting a single \
                 face is the Inset tool)."
                    .to_string(),
            );
        }

        // ---- 3. plan every cut and every face split, BEFORE touching the mesh -----------------
        let mut chain_edges: HashSet<(u32, u32)> = HashSet::new();
        for j in 0..ne {
            let (a, b) = (ring[j], ring[(j + 1) % n]);
            chain_edges.insert((a.0.min(b.0), a.0.max(b.0)));
        }
        let mut cuts: Vec<OffsetCut> = Vec::with_capacity(n);
        let mut index: HashMap<(u32, u32), usize> = HashMap::new();
        let mut splits: Vec<OffsetSplit> = Vec::with_capacity(ne);
        let mut prev_right: Option<usize> = None;

        for j in 0..ne {
            let (vj, vk) = (ring[j], ring[(j + 1) % n]);
            let l_he = self.he(h[j]).prev; // rail_j -> vj, inside faces[j]
            let r_he = self.he(h[j]).next; // vk -> rail_k, inside faces[j]
            if l_he == NO_EDGE || r_he == NO_EDGE || l_he == r_he || l_he == h[j] || r_he == h[j] {
                return Err(format!("face {} has a malformed boundary ring", faces[j].0));
            }
            let y = self.he_source(l_he); // the rail at vj on THIS face
            let x = self.he(r_he).vertex; // the rail at vk on THIS face
            debug_assert_eq!(self.he(l_he).vertex, vj, "prev must arrive at the chain vertex");
            debug_assert_eq!(self.he_source(r_he), vk, "next must leave the chain vertex");
            // Pinned to the single home for the rail math wherever that home has an answer: the
            // chosen-side rail IS `edge_slide_rails`' `rail_pos` for this half-edge. It returns
            // None on a boundary edge (missing the OTHER rail), which is the only reason this is
            // read from the face ring instead of from there.
            debug_assert!(
                self.edge_slide_rails(h[j]).map_or(true, |(rp, _)| rp == x),
                "chosen-side rail disagrees with edge_slide_rails"
            );

            // -- the cut at vj: reuse the one the previous face already placed, or cross the fan --
            let left = match prev_right {
                None => {
                    // Chain start (or the closed ring's seed). Prefer the half-edge SOURCED at the
                    // chain vertex so `t` is measured forward; a rim rung has no twin, so fall back
                    // to the reversed half-edge and measure `t` from the far end.
                    let tw = self.he(l_he).twin;
                    let (he, from_source) = if tw != NO_TWIN && self.is_edge_live(tw) {
                        (tw, true)
                    } else {
                        (l_he, false)
                    };
                    offset_loop_register_cut(
                        &mut cuts,
                        &mut index,
                        &chain_edges,
                        OffsetCut { he, chain_v: vj, rail_v: y, from_source },
                    )?
                }
                Some(pi) => {
                    if cuts[pi].rail_v == y {
                        pi // valence-4: both faces ride the SAME rung, ONE new vertex
                    } else {
                        self.offset_loop_fan(
                            vj, pi, y, &chain_edges, &mut cuts, &mut index, &mut splits,
                        )?
                    }
                }
            };

            // -- the cut at vk, on THIS face's rail (the next face may need a fan to reach it) --
            let right = offset_loop_register_cut(
                &mut cuts,
                &mut index,
                &chain_edges,
                OffsetCut { he: r_he, chain_v: vk, rail_v: x, from_source: true },
            )?;
            if left == right {
                return Err(format!(
                    "face {} would be split by a single point (its two chain corners share one \
                     rung) — refused before mutating.",
                    faces[j].0
                ));
            }
            splits.push(OffsetSplit { face: faces[j], from: left, a: vj, b: Some(vk), to: right });
            prev_right = Some(right);
        }

        // A closed ring's seed vertex is visited twice: once as face 0's left corner and once as the
        // last face's right corner. If those two are different rungs, the fan between them closes
        // the loop — without this the ring would have a gap at exactly one vertex.
        if closed {
            let last = prev_right.expect("ne >= 3 for a closed ring");
            let first = splits[0].from;
            if last != first {
                let want = cuts[first].rail_v;
                let landed = self.offset_loop_fan(
                    ring[0], last, want, &chain_edges, &mut cuts, &mut index, &mut splits,
                )?;
                if landed != first {
                    return Err(format!(
                        "the edge fan at vertex {} did not close back onto the first cut — refused \
                         rather than emitting a loop with a gap.",
                        ring[0].0
                    ));
                }
            }
        }

        // Every face may be split AT MOST once: a second split would be planned against a stale
        // ring. (`faces` was already checked for duplicates; this also covers the fan faces.)
        let mut split_faces: HashSet<u32> = HashSet::new();
        for sp in &splits {
            if !split_faces.insert(sp.face.0) {
                return Err(format!(
                    "face {} would be split TWICE by this offset (the chain doubles back through \
                     it). Flip the sign to offset the other way.",
                    sp.face.0
                ));
            }
        }

        // ---- 4. placement: absolute, clamped PER VERTEX (plan §3.1) --------------------------
        let mut t: Vec<f32> = Vec::with_capacity(cuts.len());
        let mut newp: Vec<Vec3> = Vec::with_capacity(cuts.len());
        for c in &cuts {
            let v = self.vert(c.chain_v).position;
            let r = self.vert(c.rail_v).position;
            let len = (r - v).length();
            if !(len > 1e-9) {
                return Err(format!("rung {} -> {} has zero length", c.chain_v.0, c.rail_v.0));
            }
            let s = (d / len).min(1.0 - RAIL_EPS);
            // `split_edge` lerps from the half-edge's OWN source, so measure `t` from that end. Both
            // spellings land the point at `chain_v + s·(rail_v − chain_v)`, i.e. linear in `d`.
            let (src, dst, tj) = if c.from_source { (v, r, s) } else { (r, v, 1.0 - s) };
            t.push(tj);
            newp.push(src.lerp(dst, tj)); // EXACTLY what split_edge computes for the same t.
        }

        // ---- 5. inversion guard, BEFORE any mutation ------------------------------------------
        // `split_face_between(f, from, to)` keeps `f` for the run from..to in winding order — the
        // strip quad [from, vj, vk, to] on a chain face, or the corner triangle [from, v, to] on a
        // fan face. If its Newell normal opposes the face it is carved out of, the piece has folded
        // through a rail — refuse, never emit it (plan §3.1).
        for sp in &splits {
            let mut pts: Vec<Vec3> = Vec::with_capacity(4);
            pts.push(newp[sp.from]);
            pts.push(self.vert(sp.a).position);
            if let Some(b) = sp.b {
                pts.push(self.vert(b).position);
            }
            pts.push(newp[sp.to]);
            let nq = newell_normal(&pts);
            let nf = self.compute_face_normal(sp.face);
            if nq.dot(nf) <= 0.0 {
                return Err(format!(
                    "offset {} would INVERT the connecting face carved from face {} (its winding \
                     flips against that face). Refused before mutating.",
                    distance, sp.face.0
                ));
            }
        }

        // Debug-only before-image for the post-condition below. `split_face_between` keeps `f` for
        // the strip half, so each split face's normal must still agree with its pre-op normal
        // afterwards — that check is what proves the winding derivation above, not just its
        // prediction.
        #[cfg(debug_assertions)]
        let pre_normals: Vec<Vec3> =
            splits.iter().map(|sp| self.compute_face_normal(sp.face)).collect();

        // ---- 6. mutate: split every rung, then connect the new verts inside each face ---------
        // Rungs first. They are pairwise distinct UNDIRECTED edges (the cut index dedups by edge
        // key), so no rung is another rung's twin — the only slot whose endpoints `split_edge`
        // re-points in a way a later lookup could read. A rung that is another rung's `next` still
        // resolves correctly: `split_edge` re-points that slot's `prev` to the NEW half-edge, whose
        // `vertex` is the same source vertex as before. Face IDs are untouched by `split_edge`, so
        // `splits` stays valid across this loop.
        let new_verts: Vec<VertexId> =
            (0..cuts.len()).map(|i| self.split_edge(cuts[i].he, t[i])).collect();

        for sp in &splits {
            // Unreachable by construction: `from` and `to` sit on two DISTINCT edges of `sp.face`,
            // so after both splits the ring has >= 5 corners and the two new vertices are separated
            // by at least one original corner on each side — exactly what `split_face_between`
            // requires. A `None` here means one of those invariants broke, i.e. a kernel bug, not a
            // user-reachable state.
            let split = self.split_face_between(sp.face, new_verts[sp.from], new_verts[sp.to]);
            debug_assert!(
                split.is_some(),
                "offset_edge_loop: face {} refused the connecting diagonal {} -> {}",
                sp.face.0, new_verts[sp.from].0, new_verts[sp.to].0
            );
            if split.is_none() {
                return Err(format!("internal: face {} refused the connecting diagonal", sp.face.0));
            }
        }

        // POST-CONDITION (plan §3.1 "assert against inversion"). The pre-mutation guard refuses a
        // fold before it can be emitted; this catches the OTHER failure mode — the derivation of
        // which half `split_face_between` keeps being wrong, which the pre-check cannot see.
        #[cfg(debug_assertions)]
        for (i, sp) in splits.iter().enumerate() {
            let ring_now = self.face_vertices(sp.face);
            let want = if sp.b.is_some() { 4 } else { 3 };
            debug_assert_eq!(ring_now.len(), want,
                "offset_edge_loop: face {} should be the connecting piece ({} corners), got {}",
                sp.face.0, want, ring_now.len());
            let nrm = self.compute_face_normal(sp.face);
            debug_assert!(nrm.dot(pre_normals[i]) > 0.0,
                "offset_edge_loop: connecting piece on face {} came out INVERTED", sp.face.0);
        }

        // `split_edge` lerps each new vertex normal and `split_face_between` refreshes both halves'
        // face normals, but the chain / rail vertices' area-weighted normals moved when their fans
        // gained a corner. `recompute_normals_region` is the canonical O(region) pass (loop_cut uses
        // it) and is contracted to write exactly what a full `recompute_normals` would.
        let seeds: Vec<VertexId> = ring
            .iter()
            .copied()
            .chain(new_verts.iter().copied())
            .chain(cuts.iter().map(|c| c.rail_v))
            .collect();
        self.recompute_normals_region(seeds);

        Ok(new_verts)
    }

    /// Offset **both** adjacent faces of the selected chain — one new loop per plane the
    /// segment separates (plan §0 / §4 P3 "symmetric pair").
    ///
    /// The original loop is kept. Each side is the same absolute `|distance|` (sign is ignored).
    /// The two sides ride **different rungs**, so the first call only splits one set of faces
    /// and the second call still sees the opposite faces intact.
    ///
    /// If either side is illegal the whole op refuses. When the first side has already
    /// mutated, the caller MUST discard this mesh (the drag tool rebuilds from
    /// `original_mesh` every frame, so a refusal never lands).
    pub fn offset_edge_loop_both(
        &mut self,
        edges: &std::collections::HashSet<HalfEdgeId>,
        distance: f32,
    ) -> Result<Vec<VertexId>, String> {
        if !distance.is_finite() {
            return Err("offset distance must be finite".to_string());
        }
        let d = distance.abs();
        if d < 1e-9 {
            return Err("offset distance is zero — nothing to insert".to_string());
        }
        let mut verts = self.offset_edge_loop(edges, d)?;
        let more = self.offset_edge_loop(edges, -d)?;
        verts.extend(more);
        Ok(verts)
    }
}

/// ONE vertex the offset will create: the edge to cut, and the two ends it rides between.
///
/// `he` is split at a `t` measured from ITS OWN source, so `from_source` records which end of `he`
/// the chain vertex sits at. Either way the point lands at `chain_v + s·(rail_v − chain_v)` for the
/// same clamped `s`, so the placement stays a **pure scaling in the offset distance** — the app's
/// drag tool binds drag direction to sign with a tiny probe and depends on exactly that.
#[derive(Clone, Copy, Debug)]
struct OffsetCut {
    he: HalfEdgeId,
    chain_v: VertexId,
    rail_v: VertexId,
    from_source: bool,
}

/// ONE face the offset will split, and the piece the strip keeps from it.
///
/// `from`/`to` index into the cut list. `a` (and `b`, on a chain face) are the ORIGINAL chain
/// vertices the kept piece wraps — a chain face keeps the quad `[from, a, b, to]`, a fan face keeps
/// the corner triangle `[from, a, to]`.
#[derive(Clone, Copy, Debug)]
struct OffsetSplit {
    face: FaceId,
    from: usize,
    a: VertexId,
    b: Option<VertexId>,
    to: usize,
}

/// Record a cut, deduplicating by UNDIRECTED edge so a rung shared by two chosen-side faces is split
/// exactly ONCE and both faces reference the same new vertex. Returns its index in `cuts`.
///
/// Refuses a rung that is itself a chain edge: the new vertex would land ON the source loop, which
/// is not an offset at all. (P1 refused on the rail VERTEX being on the loop; that is stricter than
/// necessary and rejects legitimate chains whose rail happens to be another chain vertex reached by
/// an off-chain edge — a shape that appears the moment open chains are allowed.)
fn offset_loop_register_cut(
    cuts: &mut Vec<OffsetCut>,
    index: &mut std::collections::HashMap<(u32, u32), usize>,
    chain_edges: &std::collections::HashSet<(u32, u32)>,
    cut: OffsetCut,
) -> Result<usize, String> {
    let key = (cut.chain_v.0.min(cut.rail_v.0), cut.chain_v.0.max(cut.rail_v.0));
    if cut.chain_v == cut.rail_v {
        return Err(format!("vertex {} rides a self-loop edge", cut.chain_v.0));
    }
    if chain_edges.contains(&key) {
        return Err(format!(
            "vertex {}'s rail on the chosen side is {}, reached along an edge that is ITSELF on the \
             selected loop — this side has no off-loop edge to ride. Flip the sign to offset the \
             other way.",
            cut.chain_v.0, cut.rail_v.0
        ));
    }
    if let Some(&i) = index.get(&key) {
        if cuts[i].chain_v != cut.chain_v {
            return Err(format!(
                "edge {}-{} would carry the offset of two different chain vertices",
                key.0, key.1
            ));
        }
        return Ok(i);
    }
    let i = cuts.len();
    cuts.push(cut);
    index.insert(key, i);
    Ok(i)
}

/// Offset Edge Loop — plan `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` §4 **P1** gate.
///
/// Inline here (not in `half_edge/tests.rs`) so this op's gates live beside the op.
#[cfg(test)]
mod offset_edge_loop_tests {
    use super::*;
    use std::collections::HashSet;

    /// The 4 half-edges bounding `f`, as the app's `selected_edges` would hold them.
    fn face_loop_selection(m: &HalfEdgeMesh, f: FaceId) -> HashSet<HalfEdgeId> {
        m.face_half_edges(f).into_iter().collect()
    }

    /// Deep-ish equality used to prove a REFUSAL mutated nothing. Compares every element slot, not
    /// just the counts — a refusal that split one rung and bailed would pass a count check.
    fn same_mesh(a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> bool {
        a.vertices.len() == b.vertices.len()
            && a.half_edges.len() == b.half_edges.len()
            && a.faces.len() == b.faces.len()
            && a.vertices.iter().zip(&b.vertices).all(|(x, y)| {
                x.position == y.position && x.normal == y.normal && x.half_edge == y.half_edge
            })
            && a.half_edges.iter().zip(&b.half_edges).all(|(x, y)| {
                x.vertex == y.vertex && x.twin == y.twin && x.next == y.next
                    && x.prev == y.prev && x.face == y.face
            })
            && a.faces.iter().zip(&b.faces).all(|(x, y)| {
                x.half_edge == y.half_edge && x.normal == y.normal && x.material_id == y.material_id
            })
    }

    /// Every live face must still point AWAY from the box centre (origin). A folded connecting quad
    /// shows up here as a negative dot even when `validate()` is happy — validate is topological.
    fn no_inverted_winding(m: &HalfEdgeMesh) {
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if m.face(f).half_edge == NO_EDGE { continue; }
            let ring = m.face_vertices(f);
            if ring.len() < 3 { continue; }
            let pts: Vec<Vec3> = ring.iter().map(|&v| m.vert(v).position).collect();
            let c: Vec3 = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
            let nrm = newell_normal(&pts).normalize_or_zero();
            assert!(nrm.dot(c) > 0.0,
                "face {} is inverted (normal {:?} vs outward {:?})", i, nrm, c.normalize_or_zero());
        }
    }

    /// Run the op with whichever sign has off-loop rails. On a cube's face-boundary loop exactly ONE
    /// sign is offsettable (the other lies inside the seed face); returns `(mesh, new_verts, sign)`.
    fn offset_either_side(m: &HalfEdgeMesh, sel: &HashSet<HalfEdgeId>, d: f32)
        -> (HalfEdgeMesh, Vec<VertexId>, f32)
    {
        let mut plus = m.clone();
        let rp = plus.offset_edge_loop(sel, d);
        let mut minus = m.clone();
        let rm = minus.offset_edge_loop(sel, -d);
        match (rp, rm) {
            (Ok(v), Err(_)) => (plus, v, d),
            (Err(_), Ok(v)) => (minus, v, -d),
            (Ok(_), Ok(_)) => panic!("both sides offsettable — impossible for a cube face loop"),
            (Err(a), Err(b)) => panic!("neither side offsettable:\n  +: {a}\n  -: {b}"),
        }
    }

    /// **THE P1 GATE.** Cube + a face-ring edge loop: exactly one new loop of N verts and N new
    /// quads, `validate()` green, manifold, no inverted winding.
    #[test]
    fn offset_edge_loop_adds_one_loop_of_n_verts_and_n_quads() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let sel = face_loop_selection(&base, FaceId(0));
        assert_eq!(sel.len(), 4, "a cube face is bounded by 4 half-edges");
        assert_eq!(base.edge_loop_rings(&sel).len(), 1, "the selection is ONE closed loop");

        let (m, new_verts, _sign) = offset_either_side(&base, &sel, 0.5);

        assert_eq!(new_verts.len(), 4, "one new vertex per loop vertex");
        assert_eq!(m.vertex_count(), base.vertex_count() + 4, "exactly N new vertices");
        assert_eq!(m.face_count(), base.face_count() + 4, "exactly N new faces");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        no_inverted_winding(&m);

        // The original loop SURVIVES — this is not a bevel (plan §5).
        for &v in &base.face_vertices(FaceId(0)) {
            assert_eq!(m.vert(v).position, base.vert(v).position,
                "loop vertex {} must be kept, unmoved", v.0);
        }
        // Each new vertex sits at EXACTLY the requested absolute distance from a loop vertex
        // (a cube's rungs are 2.0 long, so 0.5 clears the clamp everywhere).
        for &nv in &new_verts {
            let p = m.vert(nv).position;
            let best = base.face_vertices(FaceId(0)).iter()
                .map(|&v| (base.vert(v).position - p).length())
                .fold(f32::MAX, f32::min);
            assert!((best - 0.5).abs() < 1e-5, "new vertex {} is {best} from its loop vertex, want 0.5", nv.0);
        }
        // The connecting quads exist: 4 faces contain exactly two new verts + two loop verts.
        let news: HashSet<u32> = new_verts.iter().map(|v| v.0).collect();
        let loops: HashSet<u32> = base.face_vertices(FaceId(0)).iter().map(|v| v.0).collect();
        let strips = (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.face(f).half_edge != NO_EDGE)
            .map(|f| m.face_vertices(f))
            .filter(|r| r.len() == 4
                && r.iter().filter(|v| news.contains(&v.0)).count() == 2
                && r.iter().filter(|v| loops.contains(&v.0)).count() == 2)
            .count();
        assert_eq!(strips, 4, "one connecting QUAD per loop edge");
    }

    /// Fail-closed: the side with no off-loop rail is REFUSED and the mesh is left byte-identical
    /// (plan §3.2 — never a partial loop that looks like success).
    #[test]
    fn offset_edge_loop_refuses_the_rail_less_side_without_mutating() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let sel = face_loop_selection(&base, FaceId(0));
        let (_ok_mesh, _v, good) = offset_either_side(&base, &sel, 0.5);

        let mut m = base.clone();
        let err = m.offset_edge_loop(&sel, -good).expect_err("the other side has no off-loop rail");
        assert!(!err.is_empty(), "a refusal must say why");
        assert!(same_mesh(&m, &base), "a refused offset must leave the mesh byte-identical");
    }

    /// **Plan §3.1**: the clamp is PER VERTEX, not per loop. One short rung must not shrink the whole
    /// band — that is the uniform-band property absolute mode exists for.
    #[test]
    fn offset_edge_loop_clamps_per_vertex_not_per_loop() {
        let mut base = HalfEdgeMesh::new_box(Vec3::ONE);
        // Vertex 4 is vertex 0's rail (the -X/-Z vertical edge). Shorten that ONE rung to 0.5 while
        // the other three stay 2.0.
        base.vert_mut(VertexId(4)).position = Vec3::new(-1.0, -0.5, -1.0);
        let sel = face_loop_selection(&base, FaceId(0));

        let (m, new_verts, _) = offset_either_side(&base, &sel, 1.0);
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());

        let loop_pts: Vec<Vec3> = base.face_vertices(FaceId(0)).iter()
            .map(|&v| base.vert(v).position).collect();
        let mut dists: Vec<f32> = new_verts.iter().map(|&nv| {
            let p = m.vert(nv).position;
            loop_pts.iter().map(|&q| (q - p).length()).fold(f32::MAX, f32::min)
        }).collect();
        dists.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // The short rung clamps to (1 - eps) * 0.5; the other three land at EXACTLY 1.0. A whole-loop
        // clamp would have pulled all four down to ~0.4995.
        assert!((dists[0] - 0.5 * (1.0 - 1e-3)).abs() < 1e-4,
            "the short rung must clamp to just inside its rail, got {}", dists[0]);
        for dd in &dists[1..] {
            assert!((dd - 1.0).abs() < 1e-5,
                "an unclamped vertex must land at EXACTLY the requested distance, got {dd}");
        }
    }

    /// **Sign = SIDE.** A torus minor ring has real rails on BOTH sides (every vertex is valence 4),
    /// so both signs must apply cleanly and must land the new loop on OPPOSITE sides of the source —
    /// the plan's "crossing zero flips the loop to the other adjacent face".
    #[test]
    fn offset_edge_loop_sign_picks_the_opposite_side() {
        const NS: u32 = 16;
        // `new_torus` indexes verts as major*NS + minor, so 0..NS is the minor ring at major seg 0.
        let base = HalfEdgeMesh::new_torus(1.0, 0.3, 24, NS);
        let sel: HashSet<HalfEdgeId> = (0..NS)
            .map(|j| base.find_half_edge(VertexId(j), VertexId((j + 1) % NS)).expect("ring edge"))
            .collect();
        assert_eq!(base.edge_loop_rings(&sel).len(), 1, "the minor ring is ONE closed loop");

        let mut plus = base.clone();
        let vp = plus.offset_edge_loop(&sel, 0.05).expect("+ side must apply");
        let mut minus = base.clone();
        let vm = minus.offset_edge_loop(&sel, -0.05).expect("- side must apply");
        assert!(plus.validate().is_ok(), "validate +: {:?}", plus.validate());
        assert!(minus.validate().is_ok(), "validate -: {:?}", minus.validate());
        assert!(plus.is_manifold().is_ok() && minus.is_manifold().is_ok());
        assert_eq!(vp.len(), NS as usize);
        assert_eq!(vm.len(), NS as usize);

        // The source ring lies entirely at z == 0 (major angle 0); its two rails are one major
        // segment away, at z > 0 and z < 0. So the two runs must straddle the plane.
        let mean_z = |m: &HalfEdgeMesh, vs: &[VertexId]| -> f32 {
            vs.iter().map(|&v| m.vert(v).position.z).sum::<f32>() / vs.len() as f32
        };
        let (zp, zm) = (mean_z(&plus, &vp), mean_z(&minus, &vm));
        assert!(zp * zm < 0.0, "the sign must flip the side (mean z {zp} vs {zm})");
        // Both are the SAME absolute distance from the source — this is absolute mode, not
        // proportional, so the two sides are symmetric on a symmetric fixture.
        assert!((zp.abs() - zm.abs()).abs() < 1e-5, "absolute mode: |{zp}| should equal |{zm}|");
    }

    // =====================================================================================
    // P2 — rail ambiguity + open chains (plan §3.2 / §3.3, gate list in §4 "P2")
    // =====================================================================================

    /// The selection an app would hold for a chain given as undirected vertex pairs: BOTH half-edges
    /// of every edge, found by scanning rather than through `find_half_edge` — whose `vertex_fan`
    /// stops at a boundary, which is exactly where an open chain ends.
    fn chain_selection(m: &HalfEdgeMesh, pairs: &[(u32, u32)]) -> HashSet<HalfEdgeId> {
        let want: HashSet<(u32, u32)> =
            pairs.iter().map(|&(a, b)| (a.min(b), a.max(b))).collect();
        let sel: HashSet<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h))
            .filter(|&h| {
                let (a, b) = (m.he_source(h).0, m.he(h).vertex.0);
                want.contains(&(a.min(b), a.max(b)))
            })
            .collect();
        assert!(!sel.is_empty(), "the fixture's chain edges must exist");
        sel
    }

    /// Is there a live half-edge joining these two vertices (either direction)?
    fn joined(m: &HalfEdgeMesh, a: VertexId, b: VertexId) -> bool {
        (0..m.half_edges.len() as u32).map(HalfEdgeId).any(|h| {
            m.is_edge_live(h)
                && ((m.he_source(h) == a && m.he(h).vertex == b)
                    || (m.he_source(h) == b && m.he(h).vertex == a))
        })
    }

    /// **CONTINUITY** — the whole point of plan §3.2. The returned vertices must form ONE unbroken
    /// path: every consecutive pair joined by a real edge, and no vertex repeated (a repeat is the
    /// "pinch" the averaged-point fallback would produce).
    fn assert_continuous(m: &HalfEdgeMesh, vs: &[VertexId], closed: bool) {
        let uniq: HashSet<u32> = vs.iter().map(|v| v.0).collect();
        assert_eq!(uniq.len(), vs.len(), "the new loop pinched — a vertex is reused: {vs:?}");
        let n = vs.len();
        let last = if closed { n } else { n - 1 };
        for i in 0..last {
            let (a, b) = (vs[i], vs[(i + 1) % n]);
            assert!(joined(m, a, b), "gap in the new loop between {} and {}", a.0, b.0);
        }
    }

    /// **THE OWNER'S REPRO** (plan §4 P2, and the bug that motivated the phase): a cube with ONE
    /// beveled edge, an OPEN chain running along the top face, over the bevel and down the front —
    /// two free endpoints. P1 answered "selection is not a simple CLOSED edge loop".
    ///
    /// Built from an explicit polygon soup rather than by calling the bevel op, so the fixture is
    /// self-contained and cannot drift with bevel work. `w` is the chamfer width.
    fn beveled_cube(w: f32) -> HalfEdgeMesh {
        // Same corner numbering as `new_box(Vec3::ONE)` for 0..5; the top-front edge 6-7 is replaced
        // by a chamfer, so its two corners each split into a front (6/7) and a top (8/9) vertex.
        let p = [
            Vec3::new(-1.0, -1.0, -1.0),     // 0 bottom-left-back
            Vec3::new(1.0, -1.0, -1.0),      // 1 bottom-right-back
            Vec3::new(1.0, -1.0, 1.0),       // 2 bottom-right-front
            Vec3::new(-1.0, -1.0, 1.0),      // 3 bottom-left-front
            Vec3::new(-1.0, 1.0, -1.0),      // 4 top-left-back
            Vec3::new(1.0, 1.0, -1.0),       // 5 top-right-back
            Vec3::new(1.0, 1.0 - w, 1.0),    // 6 chamfer, front side, right
            Vec3::new(-1.0, 1.0 - w, 1.0),   // 7 chamfer, front side, left
            Vec3::new(1.0, 1.0, 1.0 - w),    // 8 chamfer, top side, right
            Vec3::new(-1.0, 1.0, 1.0 - w),   // 9 chamfer, top side, left
        ];
        let faces: Vec<Vec<usize>> = vec![
            vec![0, 1, 2, 3],       // bottom  -Y
            vec![9, 8, 5, 4],       // top     +Y
            vec![4, 5, 1, 0],       // back    -Z
            vec![6, 7, 3, 2],       // front   +Z
            vec![7, 6, 8, 9],       // the chamfer itself
            vec![5, 8, 6, 2, 1],    // right   +X (pentagon — the bevel ate a corner)
            vec![9, 4, 0, 3, 7],    // left    -X (pentagon)
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.validate().is_ok(), "fixture validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "fixture manifold: {:?}", m.is_manifold());
        no_inverted_winding(&m);
        m
    }

    #[test]
    fn offset_edge_loop_owner_repro_open_chain_over_a_beveled_edge() {
        const W: f32 = 0.3;
        let base = beveled_cube(W);
        // Top-face right edge -> chamfer right edge -> front-face right edge. Two free endpoints,
        // exactly the silhouette chain in the plan's reference images.
        let sel = chain_selection(&base, &[(5, 8), (8, 6), (6, 2)]);

        let mut m = base.clone();
        let new_verts = m
            .offset_edge_loop(&sel, 0.4)
            .expect("an open chain over a bevel MUST offset — this is the P2 bug report");

        // k = 3 chain edges -> k + 1 = 4 new vertices, and one connecting face per chain edge.
        assert_eq!(new_verts.len(), 4, "an open chain of k edges yields k+1 new vertices");
        assert_eq!(m.vertex_count(), base.vertex_count() + 4);
        assert_eq!(m.face_count(), base.face_count() + 3, "one new face per chain edge");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        no_inverted_winding(&m);
        assert_continuous(&m, &new_verts, false);

        // Every rail on this side runs in -X, so the new loop is the plane x = 1 - 0.4, parallel to
        // the original chain right around the chamfer. That is "rail it on either side".
        for &v in &new_verts {
            assert!((m.vert(v).position.x - 0.6).abs() < 1e-5,
                "new vertex {} landed at x={}, want 0.6", v.0, m.vert(v).position.x);
        }
        // The original chain is KEPT and unmoved (this is not a bevel — plan §5).
        for v in [5u32, 8, 6, 2] {
            assert_eq!(m.vert(VertexId(v)).position, base.vert(VertexId(v)).position);
        }

        // §3.3: each terminal rung's FAR face gains one boundary vertex and becomes an n-gon. The
        // bottom quad (across the chain's front end) and the back quad (across its top end) each
        // gain exactly one corner; nothing is fanned into triangles.
        for f in [FaceId(0), FaceId(2)] {
            assert_eq!(m.face_vertices(f).len(), 5,
                "the terminal face {} must become an n-gon, not be split or triangulated", f.0);
        }
        // No triangle anywhere: an n-gon-native kernel must not fan the ends.
        assert!(
            (0..m.faces.len() as u32).map(FaceId)
                .filter(|&f| m.face(f).half_edge != NO_EDGE)
                .all(|f| m.face_vertices(f).len() >= 4),
            "the open-chain ends were triangulated — plan §3.3 forbids the fan"
        );

        // The other side is the right pentagon for ALL THREE chain edges, so it stays refused
        // (splitting one face three times) — and refusing leaves the mesh byte-identical.
        let mut other = base.clone();
        let err = other.offset_edge_loop(&sel, -0.4).expect_err("the far side is one single face");
        assert!(!err.is_empty());
        assert!(same_mesh(&other, &base), "a refused offset must leave the mesh byte-identical");
    }

    /// **Plan §3.2, the case P1 could not express.** A chain lying on a mesh RIM: `edge_slide_rails`
    /// returns `None` for every one of its edges (no twin), which P1 consumed as "no rail" and
    /// refused. The rail on the FACE side is perfectly well defined — the `None` only ever meant the
    /// OTHER side was missing — so the offset must succeed into the surface, and must decline off it.
    #[test]
    fn offset_edge_loop_rides_a_boundary_chain_and_declines_the_open_side() {
        let base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3); // 4x4 verts, id = iz*4 + ix
        let sel = chain_selection(&base, &[(0, 1), (1, 2)]); // the -Z rim, two edges

        // Sanity: this is precisely the input P1's rail query cannot answer.
        let rim = *sel.iter().next().expect("non-empty");
        assert!(
            sel.iter().any(|&h| base.edge_slide_rails(h).is_none()),
            "fixture must contain a rim edge whose two-sided rail query is None ({rim:?})"
        );

        let mut into = base.clone();
        let vs = into.offset_edge_loop(&sel, -0.4).expect("offsetting INTO the surface must work");
        assert_eq!(vs.len(), 3, "2 chain edges -> 3 new vertices");
        assert!(into.validate().is_ok(), "validate: {:?}", into.validate());
        assert_continuous(&into, &vs, false);
        for &v in &vs {
            assert!((into.vert(v).position.z - (-1.5 + 0.4)).abs() < 1e-5,
                "new vertex {} should sit 0.4 inside the rim, got z={}", v.0, into.vert(v).position.z);
        }

        // The other sign points off the sheet: there is no face to carve the strip from. Decline,
        // byte-identical (plan §3.2 "fail closed, never a partial loop that looks like success").
        let mut off = base.clone();
        let err = off.offset_edge_loop(&sel, 0.4).expect_err("there is no surface on that side");
        assert!(err.contains("BOUNDARY") || err.contains("boundary"), "refusal must name why: {err}");
        assert!(same_mesh(&off, &base), "a declined offset must leave the mesh byte-identical");
    }

    /// **Plan §4 P2 gate: a loop crossing a valence-5 vertex still produces a CONTINUOUS loop.**
    ///
    /// A grid row, with the quad above one chain vertex split along its diagonal — that vertex now
    /// has TWO off-chain edges on the offset side (valence 5), so its two chosen-side faces ride
    /// DIFFERENT rungs. The loop rides both and cuts the corner through the face between them; one
    /// averaged point could not lie on either edge and would leave a gap.
    #[test]
    fn offset_edge_loop_crosses_a_valence_5_vertex_without_a_gap() {
        let mut base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3); // id = iz*4 + ix
        // Quad (ix=2, iz=2) is [10, 14, 15, 11]; its diagonal 10-15 makes vertex 10 valence 5.
        base.split_face_between(FaceId(8), VertexId(10), VertexId(15)).expect("diagonal");
        base.recompute_normals();
        assert!(base.validate().is_ok(), "fixture validate: {:?}", base.validate());
        assert_eq!(base.vertex_fan(VertexId(10)).len(), 5, "vertex 10 must be valence 5");

        let sel = chain_selection(&base, &[(8, 9), (9, 10), (10, 11)]); // the iz=2 row
        // The chain walks from its lower-id endpoint (8), so +Z — the side carrying the split quad —
        // is the NEGATIVE sign. (Sign = side by reversing the walk; which sign is which is
        // deliberately not assumed anywhere, hence the assertion below.)
        let mut m = base.clone();
        let vs = m.offset_edge_loop(&sel, -0.4).expect("the irregular side must offset");

        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.integrity().is_ok(), "integrity: {:?}", m.integrity());
        no_inverted_winding_flat(&m);
        assert_continuous(&m, &vs, false);
        for &v in &vs {
            assert!(m.vert(v).position.z > 0.5 + 1e-4,
                "every new vertex must land on the +Z side, got {:?}", m.vert(v).position);
        }
        // 4 chain vertices, but the valence-5 one rides TWO rungs -> 5 new vertices and 4 new faces
        // (3 strip quads + 1 corner triangle in the fan face).
        assert_eq!(vs.len(), 5, "the fan vertex contributes a second cut: {vs:?}");
        assert_eq!(m.vertex_count(), base.vertex_count() + 5);
        assert_eq!(m.face_count(), base.face_count() + 4);
        // Both of vertex 10's cuts exist and are adjacent to each other AND to vertex 10 — that is
        // what "no pinch" means here: the loop turns the corner instead of collapsing onto it.
        let at_10: Vec<VertexId> =
            vs.iter().copied().filter(|&v| joined(&m, v, VertexId(10))).collect();
        assert_eq!(at_10.len(), 2, "the valence-5 vertex must carry two cuts, got {at_10:?}");
        assert!(joined(&m, at_10[0], at_10[1]), "the two cuts must be joined — else the loop has a gap");
    }

    /// Every live face of a FLAT +Y sheet must still wind +Y. `no_inverted_winding` is written for a
    /// closed solid around the origin and does not apply to a plane.
    fn no_inverted_winding_flat(m: &HalfEdgeMesh) {
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if m.face(f).half_edge == NO_EDGE { continue; }
            let ring = m.face_vertices(f);
            if ring.len() < 3 { continue; }
            let pts: Vec<Vec3> = ring.iter().map(|&v| m.vert(v).position).collect();
            let nrm = newell_normal(&pts).normalize_or_zero();
            assert!(nrm.y > 0.5, "face {} is inverted (normal {:?})", i, nrm);
        }
    }

    /// **Plan §4 P2 gate: an isolated undefined rail declines and mutates NOTHING.** The chain runs
    /// two interior edges and then turns onto the sheet's rim, where the chosen side has no surface
    /// at all — the first vertices have perfectly good rails, one does not. Fail closed, and prove
    /// it by byte-comparing the mesh (a partial run that split one rung first would pass a count
    /// check but fail this).
    #[test]
    fn offset_edge_loop_declines_an_isolated_undefined_rail_without_mutating() {
        let base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3);
        // 9 - 5 - 1 run down a column (interior edges), then 1 - 2 turns along the -Z rim.
        let sel = chain_selection(&base, &[(9, 5), (5, 1), (1, 2)]);

        for sign in [0.3f32, -0.3] {
            let mut m = base.clone();
            let err = m
                .offset_edge_loop(&sel, sign)
                .expect_err("no side of this chain has a continuous rail");
            assert!(!err.is_empty(), "a decline must say why");
            assert!(same_mesh(&m, &base),
                "a decline must leave the mesh byte-identical (sign {sign}): {err}");
        }
    }

    /// **The property the app's drag→sign binding rests on** (plan §7b): placement is a PURE SCALING
    /// in the offset distance below the rail clamp, so a tiny probe measures the same direction as a
    /// large one. `offset_loop_drag.rs`'s `probe_direction_is_independent_of_probe_magnitude` gates
    /// it on a CLOSED torus ring; this pins it for the OPEN chains P2 just unlocked, where the cut
    /// half-edge can point the other way and `t` is measured from the far end.
    #[test]
    fn offset_edge_loop_placement_is_a_pure_scaling_in_distance() {
        let base = beveled_cube(0.3);
        let sel = chain_selection(&base, &[(5, 8), (8, 6), (6, 2)]);
        let anchor = [5u32, 8, 6, 2]
            .iter()
            .fold(Vec3::ZERO, |a, &v| a + base.vert(VertexId(v)).position)
            / 4.0;
        let landing = |d: f32| -> Vec3 {
            let mut m = base.clone();
            let vs = m.offset_edge_loop(&sel, d).expect("legal side");
            vs.iter().fold(Vec3::ZERO, |a, &v| a + m.vert(v).position) / vs.len() as f32
        };
        let (da, db) = (landing(0.02) - anchor, landing(0.2) - anchor);
        assert!(da.length() > 1e-6 && db.length() > 1e-6, "both probes must move the loop");
        assert!(
            da.normalize_or_zero().dot(db.normalize_or_zero()) > 0.999_99,
            "the measured direction drifted with the probe magnitude: {da:?} vs {db:?}"
        );
        assert!(
            (db.length() / da.length() - 10.0).abs() < 1e-3,
            "placement is not LINEAR in d: |{db:?}| / |{da:?}|"
        );
    }

    /// A fork (a vertex carrying three selected edges) and several disjoint chains are refused, not
    /// silently half-offset — the same fail-closed policy the closed walk has always had.
    #[test]
    fn offset_edge_loop_refuses_forks_and_split_selections() {
        let base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3);

        // Vertex 5 carries three selected edges.
        let fork = chain_selection(&base, &[(4, 5), (5, 6), (5, 9)]);
        let mut m = base.clone();
        assert!(m.offset_edge_loop(&fork, 0.3).is_err(), "a fork is ambiguous");
        assert!(same_mesh(&m, &base));

        // Two disjoint chains.
        let split = chain_selection(&base, &[(4, 5), (5, 6), (12, 13), (13, 14)]);
        let mut m2 = base.clone();
        assert!(m2.offset_edge_loop(&split, 0.3).is_err(), "two chains have four endpoints");
        assert!(same_mesh(&m2, &base));
    }

    /// A one-edge selection is the smallest legal open chain: two new vertices, one connecting quad.
    #[test]
    fn offset_edge_loop_handles_a_single_edge_chain() {
        let base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3);
        let sel = chain_selection(&base, &[(5, 6)]);
        let (mut a, mut b) = (base.clone(), base.clone());
        let (ra, rb) = (a.offset_edge_loop(&sel, 0.3), b.offset_edge_loop(&sel, -0.3));
        assert!(ra.is_ok() && rb.is_ok(), "an interior edge has a face on BOTH sides: {ra:?} {rb:?}");
        let mean_z = |m: &HalfEdgeMesh, vs: &[VertexId]| -> f32 {
            vs.iter().map(|&v| m.vert(v).position.z).sum::<f32>() / vs.len() as f32
        };
        let mut zs = Vec::new();
        for (m, vs) in [(&a, ra.expect("+")), (&b, rb.expect("-"))] {
            assert_eq!(vs.len(), 2, "1 chain edge -> 2 new vertices");
            assert_eq!(m.face_count(), base.face_count() + 1, "one connecting quad");
            assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
            assert_continuous(m, &vs, false);
            zs.push(mean_z(m, &vs));
        }
        // ...and the sign still picks the SIDE, exactly as it does for a closed loop: the source edge
        // lies at z = -0.5, so the two runs must straddle it at the same absolute distance.
        assert!((zs[0] + 0.5) * (zs[1] + 0.5) < 0.0, "the sign must flip the side: {zs:?}");
        assert!(((zs[0] + 0.5).abs() - (zs[1] + 0.5).abs()).abs() < 1e-5,
            "absolute mode is symmetric on a uniform grid: {zs:?}");
    }

    /// P3 gate — both planes of a selected interior edge get an offset strip. The source
    /// edge is kept; each adjacent n-gon receives its own connecting quad.
    #[test]
    fn offset_edge_loop_both_rails_each_adjacent_face() {
        let base = HalfEdgeMesh::new_plane(3.0, 3.0, 3, 3);
        let sel = chain_selection(&base, &[(5, 6)]);
        let mut m = base.clone();
        let vs = m
            .offset_edge_loop_both(&sel, 0.3)
            .expect("interior edge has a face on BOTH sides");
        assert_eq!(vs.len(), 4, "2 sides × 2 new verts");
        assert_eq!(m.vertex_count(), base.vertex_count() + 4);
        assert_eq!(m.face_count(), base.face_count() + 2, "one connecting quad per side");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());

        // Source edge endpoints stay put.
        for &vid in &[VertexId(5), VertexId(6)] {
            assert_eq!(m.vert(vid).position, base.vert(vid).position);
        }
        // New verts sit at 0.3 from the source edge, two on each side of its z-plane.
        let z_edge = 0.5 * (base.vert(VertexId(5)).position.z + base.vert(VertexId(6)).position.z);
        let mut above = 0u32;
        let mut below = 0u32;
        for &nv in &vs {
            let p = m.vert(nv).position;
            let dist = [VertexId(5), VertexId(6)]
                .iter()
                .map(|&v| (base.vert(v).position - p).length())
                .fold(f32::MAX, f32::min);
            assert!((dist - 0.3).abs() < 1e-4, "new vert {nv:?} dist {dist}");
            if p.z > z_edge {
                above += 1;
            } else {
                below += 1;
            }
        }
        assert_eq!(above, 2, "one rail on the + side of the segment");
        assert_eq!(below, 2, "one rail on the − side of the segment");
    }

    /// P3 gate — torus minor ring, both signs at once: two loops, mirror-equal about the source.
    #[test]
    fn offset_edge_loop_both_is_mirror_equal_on_a_symmetric_ring() {
        const NS: u32 = 16;
        let base = HalfEdgeMesh::new_torus(1.0, 0.3, 24, NS);
        let sel: HashSet<HalfEdgeId> = (0..NS)
            .map(|j| base.find_half_edge(VertexId(j), VertexId((j + 1) % NS)).expect("ring edge"))
            .collect();
        let mut m = base.clone();
        let vs = m
            .offset_edge_loop_both(&sel, 0.05)
            .expect("torus ring has rails on both sides");
        assert_eq!(vs.len(), (NS * 2) as usize);
        assert_eq!(m.face_count(), base.face_count() + (NS * 2) as usize);
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());

        let mut zp = 0.0f32;
        let mut zm = 0.0f32;
        let mut np = 0u32;
        let mut nm = 0u32;
        for &v in &vs {
            let z = m.vert(v).position.z;
            if z >= 0.0 {
                zp += z;
                np += 1;
            } else {
                zm += z;
                nm += 1;
            }
        }
        assert_eq!(np, NS);
        assert_eq!(nm, NS);
        let (zp, zm) = (zp / np as f32, zm / nm as f32);
        assert!((zp.abs() - zm.abs()).abs() < 1e-5, "mirror-equal |z| {zp} vs {zm}");
    }

    /// A zero / non-finite distance is a clean refusal, not a degenerate loop welded onto the source.
    #[test]
    fn offset_edge_loop_refuses_a_zero_distance() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let sel = face_loop_selection(&base, FaceId(0));
        let mut m = base.clone();
        assert!(m.offset_edge_loop(&sel, 0.0).is_err());
        assert!(m.offset_edge_loop(&sel, f32::NAN).is_err());
        assert!(same_mesh(&m, &base));
    }

    /// Determinism: `edge_loop_rings` was a live nondeterminism source (nav.rs:393), and this op
    /// inherits its walk. The same selection must produce the same mesh whatever order the HashSet
    /// was built in — the app previews on a clone and re-runs on the real mesh.
    #[test]
    fn offset_edge_loop_is_insertion_order_independent() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let order = base.face_half_edges(FaceId(0));
        let (baseline, _, sign) = offset_either_side(&base, &order.iter().copied().collect(), 0.5);
        for perm in 0..4usize {
            let mut o = order.clone();
            o.rotate_left(perm);
            if perm % 2 == 1 { o.reverse(); }
            let sel: HashSet<HalfEdgeId> = o.into_iter().collect();
            let mut m = base.clone();
            m.offset_edge_loop(&sel, sign).expect("same selection, same side");
            assert!(same_mesh(&m, &baseline), "offset varied with set insertion order (perm {perm})");
        }
    }
}
