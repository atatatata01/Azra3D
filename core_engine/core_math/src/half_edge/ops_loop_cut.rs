// Loop cut family (split from half_edge.rs — pure relocation).

use super::*;

impl HalfEdgeMesh {
    /// Exit half-edge of `face` for a loop-cut strip entering on `entry`.
    ///
    /// * **Quads:** classic opposite edge (entry + 2).
    /// * **N-gons (n≥5):** pick a non-adjacent edge most **parallel** to the entry rail and
    ///   nearest to the angular opposite — owner 2026-08-08: after sphere−cylinder boolean the
    ///   cylindrical wall becomes 5–6-gons; pure-quad walk found **zero** circumferential rings.
    /// * **Tris / digons:** no sensible parallel rail → None.
    fn loop_cut_exit_he(&self, face: FaceId, entry: HalfEdgeId) -> Option<HalfEdgeId> {
        let hes = self.face_half_edges(face);
        let n = hes.len();
        if n < 4 {
            return None;
        }
        let pos = hes.iter().position(|&h| h == entry)?;
        if n == 4 {
            return Some(hes[(pos + 2) % 4]);
        }
        let e0 = self.vert(self.he_source(entry)).position;
        let e1 = self.vert(self.he(entry).vertex).position;
        let entry_dir = (e1 - e0).normalize_or_zero();
        if entry_dir.length_squared() < 1e-12 {
            return None;
        }
        let mut best: Option<(f32, HalfEdgeId)> = None;
        for k in 0..n {
            if k == pos {
                continue;
            }
            // Skip edges that share a vertex with the entry (adjacent rails).
            let steps = (k + n - pos) % n;
            if steps == 1 || steps == n - 1 {
                continue;
            }
            let h = hes[k];
            let a = self.vert(self.he_source(h)).position;
            let b = self.vert(self.he(h).vertex).position;
            let dir = (b - a).normalize_or_zero();
            let parallel = entry_dir.dot(dir).abs();
            if parallel < 0.55 {
                continue; // not a parallel rail
            }
            // Prefer near-opposite around the ring (steps ≈ n/2).
            let opp = 1.0 - ((steps as f32) - (n as f32) * 0.5).abs() / (n as f32 * 0.5).max(1.0);
            let score = parallel * 2.0 + opp;
            if best.map_or(true, |(bs, _)| score > bs) {
                best = Some((score, h));
            }
        }
        best.map(|(_, h)| h)
    }

    /// One direction of the strip walk from `seed`. Each record = (face, [q0,q1,q2,q3]) with the
    /// loop's entry rail q0→q1 and exit rail q3→q2 (same "up" orientation). Walks quads and
    /// n-gons with a parallel exit rail. `visited` is the strip set + closed-loop guard.
    /// `limit` (partial loop cut): stop when the far rail isn't in the selection set.
    fn loop_ring_walk(&self, seed: HalfEdgeId, visited: &mut std::collections::HashSet<u32>,
                      limit: Option<&std::collections::HashSet<u32>>) -> Vec<(FaceId, [VertexId; 4])> {
        let mut out = Vec::new();
        let mut cur = seed;
        loop {
            let face = self.he(cur).face;
            if face == NO_FACE { break; }
            let Some(exit_he) = self.loop_cut_exit_he(face, cur) else { break; };
            // Partial-cut limit: don't cross a face whose FAR rail isn't selected.
            if let Some(lim) = limit {
                let twin_e = self.he(exit_he).twin;
                let exit_sel = lim.contains(&exit_he.0) || (twin_e != NO_TWIN && lim.contains(&twin_e.0));
                if !exit_sel { break; }
            }
            if !visited.insert(face.0) { break; }     // closed loop / already crossed
            // Entry rail q0→q1, exit rail oriented to the same "up" as entry.
            let q0 = self.he_source(cur);
            let q1 = self.he(cur).vertex;
            let mut x0 = self.he_source(exit_he);
            let mut x1 = self.he(exit_he).vertex;
            let ed = self.vert(q1).position - self.vert(q0).position;
            let xd = self.vert(x1).position - self.vert(x0).position;
            if ed.dot(xd) < 0.0 {
                std::mem::swap(&mut x0, &mut x1);
            }
            // [q0,q1,q2,q3] with exit rail (q3,q2)=(x0,x1) so phase-2 keys match quads.
            let q2 = x1;
            let q3 = x0;
            out.push((face, [q0, q1, q2, q3]));
            let twin = self.he(exit_he).twin;
            if twin == NO_TWIN { break; }
            cur = twin;
        }
        out
    }

    /// The full bidirectional strip of quads a loop cut from `start_he` crosses (reverse(backward)
    /// ++ forward), plus the set of crossed face ids. Shared by the kernel cut and the preview.
    fn loop_ring_quads(&self, start_he: HalfEdgeId, limit: Option<&std::collections::HashSet<u32>>)
        -> (Vec<(FaceId, [VertexId; 4])>, std::collections::HashSet<u32>) {
        let mut visited = std::collections::HashSet::new();
        let forward = self.loop_ring_walk(start_he, &mut visited, limit);
        let twin_start = self.he(start_he).twin;
        let backward = if twin_start != NO_TWIN { self.loop_ring_walk(twin_start, &mut visited, limit) } else { Vec::new() };
        let mut quads = Vec::with_capacity(forward.len() + backward.len());
        for q in backward.iter().rev() { quads.push(*q); }
        for q in forward.iter() { quads.push(*q); }
        (quads, visited)
    }

    /// Per-quad rail VERTICES for the loop-cut strip: for each crossed quad,
    /// `(entry_rail=(bottom,top), exit_rail=(bottom,top))`.
    ///
    /// Ids rather than positions, because a curvature-following cut ([`rail_curve_point`]) needs the
    /// endpoints' normals, not just their positions. `limit` gives partial-cut parity with the
    /// actual cut (selection limits the span).
    ///
    /// [`rail_curve_point`]: Self::rail_curve_point
    pub fn loop_cut_preview_rail_verts(
        &self,
        start_he: HalfEdgeId,
        limit: Option<&std::collections::HashSet<u32>>,
    ) -> Vec<((VertexId, VertexId), (VertexId, VertexId))> {
        if start_he.0 as usize >= self.half_edges.len() {
            return Vec::new();
        }
        let (quads, _) = self.loop_ring_quads(start_he, limit);
        quads
            .iter()
            .map(|&(_, [q0, q1, q2, q3])| ((q0, q1), (q3, q2)))
            .collect()
    }

    /// Per-quad rail segments (LOCAL space) for the loop-cut viewport preview. The viewport draws the
    /// cut line by joining `lerp(entry)` to `lerp(exit)` at each cut fraction — so the preview matches
    /// the actual cut. Positional view of [`loop_cut_preview_rail_verts`] — ONE strip walk, so the two
    /// can never disagree about which quads the ring crosses.
    pub fn loop_cut_preview_rails(&self, start_he: HalfEdgeId, limit: Option<&std::collections::HashSet<u32>>)
        -> Vec<((Vec3, Vec3), (Vec3, Vec3))> {
        self.loop_cut_preview_rail_verts(start_he, limit)
            .into_iter()
            .map(|((a, b), (c, d))| {
                ((self.vert(a).position, self.vert(b).position),
                 (self.vert(c).position, self.vert(d).position))
            })
            .collect()
    }

    /// Point on the rail `a`→`b` at `t`, FOLLOWING THE SURFACE CURVATURE instead of taking the
    /// straight chord — blended back toward the chord by `1 - amount` (`amount` 0 = plain lerp).
    ///
    /// Construction: a cubic Bézier whose interior control points sit on each endpoint's tangent
    /// plane, at the offset that reproduces a CIRCULAR arc of the angle between the two vertex
    /// normals (`k = 4/3·tan(α/4)·R`). So a rail spanning a rounded corner returns a point on that
    /// round, not inside it — which is what "the inserted loop should follow the curvature" means.
    ///
    /// Degenerate inputs fail closed to the straight lerp: equal normals (flat surface) collapse the
    /// scale to the classic 1/3 and the control points land evenly on the segment, giving EXACTLY
    /// the lerp — `amount` can never dent a flat face. A rail parallel to its endpoint normal has no
    /// tangent, and a dead/zero-normal endpoint has no curvature to follow; both return the chord.
    pub fn rail_curve_point(&self, a: VertexId, b: VertexId, t: f32, amount: f32) -> Vec3 {
        let p0 = self.vert(a).position;
        let p1 = self.vert(b).position;
        let linear = p0.lerp(p1, t);
        if amount.abs() < 1e-6 {
            return linear;
        }
        let n0 = self.vert(a).normal.normalize_or_zero();
        let n1 = self.vert(b).normal.normalize_or_zero();
        if n0.length_squared() < 0.5 || n1.length_squared() < 0.5 {
            return linear;
        }
        let d = p1 - p0;
        let len = d.length();
        if len < 1e-9 {
            return linear;
        }
        // Arc angle between the endpoint normals → Bézier tangent offset for that arc.
        let alpha = n0.dot(n1).clamp(-1.0, 1.0).acos();
        let scale = if alpha < 1e-4 {
            1.0 / 3.0
        } else {
            (4.0 / 3.0) * (alpha * 0.25).tan() / (2.0 * (alpha * 0.5).sin())
        };
        // Tangent at each endpoint, pointing along the rail inside that endpoint's tangent plane.
        let t0 = (d - d.dot(n0) * n0).normalize_or_zero();
        let t1 = (-d - (-d).dot(n1) * n1).normalize_or_zero();
        let b1 = p0 + t0 * (scale * len);
        let b2 = p1 + t1 * (scale * len);
        let u = 1.0 - t;
        let curved =
            u * u * u * p0 + 3.0 * u * u * t * b1 + 3.0 * u * t * t * b2 + t * t * t * p1;
        linear.lerp(curved, amount)
    }

    /// Move the vertices a loop cut just inserted off the straight rail chord and onto the
    /// curvature-following rail curve (see [`rail_curve_point`](Self::rail_curve_point)).
    ///
    /// `delta` must be the one [`loop_cut_multi_with_delta`](Self::loop_cut_multi_with_delta)
    /// returned for THIS mesh: its `vert_parents` carry each inserted vertex's two rail endpoints
    /// and its position along them, which is exactly what the curve needs and what cannot be
    /// recovered afterwards (in a multi-cut, a new vertex's rail neighbours are the OTHER new
    /// vertices, not the original rail ends).
    ///
    /// No-op at `amount == 0`, so this is safe to call unconditionally.
    pub fn apply_loop_cut_curvature(&mut self, delta: &crate::mesh_ops::TopoDelta, amount: f32) {
        if amount.abs() < 1e-6 {
            return;
        }
        let mut moved: Vec<VertexId> = Vec::with_capacity(delta.vert_parents.len());
        for (nv, parents) in &delta.vert_parents {
            if parents.len() != 2 {
                continue;
            }
            let v = VertexId(*nv);
            let (bottom, top) = (VertexId(parents[0].0), VertexId(parents[1].0));
            if !self.is_vert_live(v) || !self.is_vert_live(bottom) || !self.is_vert_live(top) {
                continue;
            }
            let t = parents[1].1;
            self.vert_mut(v).position = self.rail_curve_point(bottom, top, t, amount);
            moved.push(v);
        }
        if !moved.is_empty() {
            self.recompute_normals_region(moved.iter().copied());
        }
    }


    /// Loop cut: insert ONE edge loop perpendicular to `start_he` at its center (Blender Ctrl+R,
    /// single cut). Returns the inserted loop vertices. See [`loop_cut_multi`](Self::loop_cut_multi).
    pub fn loop_cut(&mut self, start_he: HalfEdgeId) -> Vec<VertexId> {
        self.loop_cut_multi(start_he, &[0.5], None)
    }

    /// Insert `ts.len()` parallel edge loops PERPENDICULAR to `start_he`, each at its fraction in
    /// `ts` (each strictly inside 0..1, measured along the ring's rails from the same strip side).
    ///
    /// **In-place / ID-stable (locality spike):** splits only strip rails via [`split_edge`] and
    /// subdivides strip faces via [`split_face_between`]. Untouched element IDs are preserved
    /// (no `from_polygons` rebuild). Adjacent non-strip faces weld automatically because
    /// `split_edge` inserts into both sides of a shared rail. Returns inserted loop VertexIds.
    ///
    /// Open boundaries terminate the ring. Quads use opposite-edge walk; n-gons (n≥5) use a
    /// parallel-rail exit so boolean walls (cylinder hole, etc.) still form a strip. Tris stop.
    /// Per-face `material_id` is preserved on reused strip faces. Asserts `validate()` +
    /// `is_manifold()` (relative: only if input was manifold).
    ///
    /// No-op (returns empty, mesh unchanged) when the ring crosses no strip face or `ts` is empty.
    pub fn loop_cut_multi(&mut self, start_he: HalfEdgeId, ts: &[f32],
                          limit: Option<&std::collections::HashSet<u32>>) -> Vec<VertexId> {
        let (verts, _delta) = self.loop_cut_multi_with_delta(start_he, ts, limit);
        verts
    }

    /// Like [`loop_cut_multi`] but also returns the first real [`TopoDelta`] parent map
    /// (rail split verts + new sub-quad faces → strip parents).
    pub fn loop_cut_multi_with_delta(
        &mut self,
        start_he: HalfEdgeId,
        ts: &[f32],
        limit: Option<&std::collections::HashSet<u32>>,
    ) -> (Vec<VertexId>, crate::mesh_ops::TopoDelta) {
        use std::collections::HashMap;
        let mut delta = crate::mesh_ops::TopoDelta::new();
        if ts.is_empty() || start_he.0 as usize >= self.half_edges.len() {
            return (Vec::new(), delta);
        }
        // Relative manifold check ONLY in debug — `is_manifold()` is O(mesh) and would
        // re-introduce measurement-C scaling on the hot path in release.
        #[cfg(debug_assertions)]
        let input_manifold = self.is_manifold().is_ok();

        // --- Phase 1: bidirectional quad-ring walk ---
        let (quads, _visited) = self.loop_ring_quads(start_he, limit);
        if quads.is_empty() {
            return (Vec::new(), delta);
        }

        // Normalize / sort cut fractions in (0,1).
        let mut ts_sorted: Vec<f32> = ts
            .iter()
            .map(|&t| t.clamp(1e-6, 1.0 - 1e-6))
            .collect();
        ts_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ts_sorted.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        if ts_sorted.is_empty() {
            return (Vec::new(), delta);
        }
        let n = ts_sorted.len();

        // Pre-reserve: ≤ n verts per unique rail (≤ 2·|quads| rails open-strip worst case),
        // split_edge adds 1 HE (+1 twin) per insert, split_face_between adds 2 HE + 1 face per cut.
        // Tight upper bounds keep first push off the O(mesh) reallocation cliff.
        let n_quads = quads.len();
        let max_rails = n_quads.saturating_mul(2).saturating_add(1);
        let max_verts = max_rails.saturating_mul(n);
        let max_he = max_verts.saturating_mul(2) // rail splits (he + twin)
            .saturating_add(n_quads.saturating_mul(n).saturating_mul(2)); // diagonals
        let max_faces = n_quads.saturating_mul(n); // one new face per cut per quad
        self.reserve_elements(max_verts, max_he, max_faces);

        // --- Phase 2: split each unique rail once (shared across adjacent quads) ---
        // Key = (bottom, top) VertexId. Value = inserted verts ordered by ascending t (bottom→top).
        //
        // CRITICAL (open strips / boundary): do NOT use `find_half_edge` / `vertex_fan` here.
        // `vertex_fan` stops at the first boundary twin, so reverse-directed rails on open meshes
        // are invisible (plane 4×1 only saw the seed rail → loose verts, no face growth). Resolve
        // the rail HE from the strip face's own half-edges (O(face), both directions).
        //
        // CRITICAL (orthogonal multi on open plane): adjacent strip faces often see the SAME
        // undirected rail with opposite (bottom,top) orientation. Store BOTH directions after
        // the first split (reverse key → reversed vert list). Otherwise phase 3 looks up the
        // flipped key, misses, skips that face → rails split but face not subdivided (owner
        // 2026-08-07: multi U then multi V on a square plane left n-gons / wrong face count).
        let mut rail_verts: HashMap<(u32, u32), Vec<VertexId>> = HashMap::new();
        let mut inserted_order: Vec<VertexId> = Vec::new();

        for &(fid, [q0, q1, q2, q3]) in &quads {
            for &(bottom, top) in &[(q0, q1), (q3, q2)] {
                let key = (bottom.0, top.0);
                let rev_key = (top.0, bottom.0);
                if rail_verts.contains_key(&key) {
                    continue;
                }
                // Same undirected rail already split from the opposite face orientation.
                if let Some(verts_rev) = rail_verts.get(&rev_key).cloned() {
                    let verts: Vec<VertexId> = verts_rev.into_iter().rev().collect();
                    rail_verts.insert(key, verts);
                    continue;
                }
                let Some((mut he, reversed)) = self.face_rail_he(fid, bottom, top) else {
                    continue;
                };
                // Sequential split along `he`. If he runs top→bottom, map t_bottom→top to 1−t
                // and walk ascending along he, then reverse the stored list back to bottom→top.
                let ts_along: Vec<f32> = if reversed {
                    ts_sorted.iter().rev().map(|&t| 1.0 - t).collect()
                } else {
                    ts_sorted.clone()
                };
                let mut prev_t = 0.0f32;
                let mut verts_along = Vec::with_capacity(ts_along.len());
                for &t_he in &ts_along {
                    let denom = (1.0 - prev_t).max(1e-9);
                    let t_local = ((t_he - prev_t) / denom).clamp(1e-6, 1.0 - 1e-6);
                    let nv = self.split_edge(he, t_local);
                    verts_along.push(nv);
                    // After split: `he` is prev → nv; next is nv → remaining end.
                    he = self.he(he).next;
                    prev_t = t_he;
                }
                let verts: Vec<VertexId> = if reversed {
                    verts_along.into_iter().rev().collect()
                } else {
                    verts_along
                };
                for (i, &nv) in verts.iter().enumerate() {
                    let t = ts_sorted[i];
                    delta.vert_parents.push((
                        nv.0,
                        vec![(bottom.0, 1.0 - t), (top.0, t)],
                    ));
                    inserted_order.push(nv);
                }
                // Mirror reverse mapping so adjacent quads that flip the rail still resolve.
                let rev_verts: Vec<VertexId> = verts.iter().rev().copied().collect();
                rail_verts.insert(key, verts);
                rail_verts.insert(rev_key, rev_verts);
            }
        }

        if inserted_order.is_empty() {
            return (Vec::new(), delta);
        }

        // --- Phase 3: subdivide each strip face by connecting matching rail points ---
        // After rail splits, each strip face is a (4+2n)-gon. Connecting e[i]–x[i] with
        // split_face_between yields n+1 sub-quads; original FaceId is reused for one half.
        for &(fid, [q0, q1, q2, q3]) in &quads {
            if self.free_faces.contains(&fid.0) || self.face(fid).half_edge == NO_EDGE {
                continue;
            }
            // Resolve rail insert lists; tolerate flipped keys (see phase-2 dual insert).
            let e = rail_verts
                .get(&(q0.0, q1.0))
                .cloned()
                .or_else(|| {
                    rail_verts
                        .get(&(q1.0, q0.0))
                        .map(|v| v.iter().rev().copied().collect())
                });
            let x = rail_verts
                .get(&(q3.0, q2.0))
                .cloned()
                .or_else(|| {
                    rail_verts
                        .get(&(q2.0, q3.0))
                        .map(|v| v.iter().rev().copied().collect())
                });
            let (Some(e), Some(x)) = (e, x) else {
                continue;
            };
            if e.len() != n || x.len() != n {
                continue;
            }
            let mut face = fid;
            for i in 0..n {
                let ei = e[i];
                let xi = x[i];
                // Region-local: after rail splits the strip face holds all e[i]/x[i]; after each
                // diagonal we keep the half that still contains the next cut (no mesh scan).
                let ring = self.face_vertices(face);
                if !(ring.contains(&ei) && ring.contains(&xi)) {
                    // Rare: face id drifted — recover from either endpoint's live incident faces
                    // without a full mesh walk (vertex_fan is boundary-incomplete on open strips).
                    if let Some(f) = self.face_containing_verts(ei, xi) {
                        face = f;
                    } else {
                        continue;
                    }
                }
                let f_use = face;
                if let Some(f_new) = self.split_face_between(f_use, ei, xi) {
                    delta.face_parents.push((f_new.0, fid.0));
                    // Continue subdividing the half that still holds remaining cut points.
                    if i + 1 < n {
                        let next_e = e[i + 1];
                        face = if self.face_vertices(f_use).contains(&next_e) {
                            f_use
                        } else {
                            f_new
                        };
                    }
                }
            }
        }

        // Region normals — only faces incident to inserted + rail endpoints.
        let mut seed: Vec<VertexId> = inserted_order.clone();
        for &(_fid, [q0, q1, q2, q3]) in &quads {
            seed.push(q0);
            seed.push(q1);
            seed.push(q2);
            seed.push(q3);
        }
        self.recompute_normals_region(seed.iter().copied());

        debug_assert!(
            self.validate().is_ok(),
            "loop_cut_multi in-place invalid: {:?}",
            self.validate()
        );
        #[cfg(debug_assertions)]
        if input_manifold {
            debug_assert!(
                self.is_manifold().is_ok(),
                "loop_cut_multi in-place non-manifold: {:?}",
                self.is_manifold()
            );
        }

        (inserted_order, delta)
    }

    /// Fallback when no quad loop is found: split each **selected edge** at the fractions in `ts`
    /// (default mid = 0.5), then **connect** cut verts that share a face so a real cut segment is
    /// created (e.g. two opposite edges of a face → one new edge through the face).
    ///
    /// Undirected edges are twin-deduped. Returns all newly inserted (split) vertices.
    pub fn edge_series_cut(
        &mut self,
        edge_hes: &std::collections::HashSet<u32>,
        ts: &[f32],
    ) -> Vec<VertexId> {
        if ts.is_empty() || edge_hes.is_empty() {
            return Vec::new();
        }
        let mut ts_sorted: Vec<f32> = ts
            .iter()
            .map(|&t| t.clamp(1e-6, 1.0 - 1e-6))
            .collect();
        ts_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ts_sorted.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        if ts_sorted.is_empty() {
            return Vec::new();
        }
        let n_ts = ts_sorted.len();

        // Undirected edge pairs (src, dst) from the selection, twin-deduped.
        let mut pairs: Vec<(VertexId, VertexId)> = Vec::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &eid in edge_hes {
            let he = HalfEdgeId(eid);
            if !self.is_edge_live(he) {
                continue;
            }
            let twin = self.he(he).twin;
            let canon = if twin != NO_TWIN {
                he.0.min(twin.0)
            } else {
                he.0
            };
            if !seen.insert(canon) {
                continue;
            }
            let src = self.he_source(he);
            let dst = self.he(he).vertex;
            pairs.push((src, dst));
        }
        if pairs.is_empty() {
            return Vec::new();
        }

        // inserts[pair_idx][t_idx] = new VertexId
        let mut inserts: Vec<Vec<VertexId>> = vec![Vec::new(); pairs.len()];
        let mut inserted: Vec<VertexId> = Vec::new();

        for (pi, &(src0, dst0)) in pairs.iter().enumerate() {
            let Some(mut he) = self
                .find_half_edge(src0, dst0)
                .or_else(|| self.find_half_edge(dst0, src0))
            else {
                continue;
            };
            let he_src = self.he_source(he);
            let reversed = he_src == dst0;
            let ts_along: Vec<f32> = if reversed {
                ts_sorted.iter().rev().map(|&t| 1.0 - t).collect()
            } else {
                ts_sorted.clone()
            };
            let mut prev_t = 0.0f32;
            let mut verts_along = Vec::with_capacity(ts_along.len());
            for &t_he in &ts_along {
                let denom = (1.0 - prev_t).max(1e-9);
                let t_local = ((t_he - prev_t) / denom).clamp(1e-6, 1.0 - 1e-6);
                let nv = self.split_edge(he, t_local);
                verts_along.push(nv);
                he = self.he(he).next;
                prev_t = t_he;
            }
            let verts: Vec<VertexId> = if reversed {
                verts_along.into_iter().rev().collect()
            } else {
                verts_along
            };
            inserted.extend(verts.iter().copied());
            inserts[pi] = verts;
        }

        // Connect cut verts that share a face → real cut segment(s).
        // For each cut fraction, on each face that has exactly two cut verts, join them.
        for ti in 0..n_ts {
            let face_ids: Vec<u32> = (0..self.faces.len() as u32)
                .filter(|fi| {
                    !self.free_faces.contains(fi) && self.face(FaceId(*fi)).half_edge != NO_EDGE
                })
                .collect();
            for fi in face_ids {
                let fid = FaceId(fi);
                if self.free_faces.contains(&fi) || self.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                let ring = self.face_vertices(fid);
                let mut cuts: Vec<VertexId> = Vec::new();
                for (pi, &(src0, dst0)) in pairs.iter().enumerate() {
                    // Original edge was on this face if both endpoints are still in the ring
                    // (split only inserts between them).
                    if !ring.contains(&src0) || !ring.contains(&dst0) {
                        continue;
                    }
                    if let Some(&vid) = inserts[pi].get(ti) {
                        if ring.contains(&vid) {
                            cuts.push(vid);
                        }
                    }
                }
                // Dedup (same vert shouldn't appear twice).
                cuts.sort_by_key(|v| v.0);
                cuts.dedup();
                if cuts.len() == 2 {
                    let _ = self.split_face_between(fid, cuts[0], cuts[1]);
                }
            }
        }

        if !inserted.is_empty() {
            self.recompute_normals_region(inserted.iter().copied());
        }
        inserted
    }

    /// Canonical half-edge ids of every RAIL the loop-cut ring from `start_he` crosses — the
    /// entry rail of each strip face plus the strip's final exit rail. These are exactly the
    /// edges the inserted loop passes through.
    ///
    /// A rail belongs to exactly ONE ring (the ring seeded at that rail), which is what makes
    /// [`loop_cut_ring_groups`](Self::loop_cut_ring_groups) a true partition.
    pub fn loop_cut_ring_rail_edges(
        &self,
        start_he: HalfEdgeId,
        limit: Option<&std::collections::HashSet<u32>>,
    ) -> std::collections::HashSet<u32> {
        let mut out = std::collections::HashSet::new();
        if start_he.0 as usize >= self.half_edges.len() {
            return out;
        }
        let (quads, _) = self.loop_ring_quads(start_he, limit);
        for &(fid, [q0, q1, q2, q3]) in &quads {
            for &(bottom, top) in &[(q0, q1), (q3, q2)] {
                if let Some((he, _)) = self.face_rail_he(fid, bottom, top) {
                    out.insert(self.canonical_edge(he).0);
                }
            }
        }
        out
    }

    /// Partition an edge selection into independent loop-cut RINGS: one entry per ring, as
    /// `(seed, the selected edges lying on that ring)` — all canonical ids.
    ///
    /// Multi-selection Ctrl+R needs this. The session used to collapse ANY multi-edge selection
    /// into one seed plus one span limit, so a selection touching several rings could never
    /// produce more than one cut (owner 2026-08-10: "CTRL R DOES NOT SUPPORT MULTI SELECTION").
    /// Grouping is exact rather than heuristic because a rail belongs to one ring only — no edge
    /// can land in two groups.
    ///
    /// Ordered by ascending seed id, NOT by selection-set order: `HashSet` iteration is
    /// nondeterministic and the cuts are applied in sequence, so an unstable order would let one
    /// selection produce different meshes run to run.
    ///
    /// An edge whose ring crosses no strip face (tri fan, lone boundary edge) comes back as its
    /// own singleton group — callers fall back to segment mode for those.
    pub fn loop_cut_ring_groups(
        &self,
        edges: &std::collections::HashSet<u32>,
    ) -> Vec<(u32, std::collections::HashSet<u32>)> {
        let mut selected: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut seeds: Vec<u32> = Vec::with_capacity(edges.len());
        for &e in edges {
            let he = HalfEdgeId(e);
            if e as usize >= self.half_edges.len() || !self.is_edge_live(he) {
                continue;
            }
            let canon = self.canonical_edge(he).0;
            if selected.insert(canon) {
                seeds.push(canon);
            }
        }
        seeds.sort_unstable();

        let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut out: Vec<(u32, std::collections::HashSet<u32>)> = Vec::new();
        for &seed in &seeds {
            if claimed.contains(&seed) {
                continue;
            }
            let mut members = std::collections::HashSet::new();
            members.insert(seed);
            claimed.insert(seed);
            // Walk this seed's ring ONCE and claim every selected edge on it: O(Σ strip lengths)
            // overall, where a pairwise same-ring test would be O(selection²).
            for rail in self.loop_cut_ring_rail_edges(HalfEdgeId(seed), None) {
                if selected.contains(&rail) && claimed.insert(rail) {
                    members.insert(rail);
                }
            }
            out.push((seed, members));
        }
        out
    }

    /// True when a loop-cut ring from `start_he` crosses at least one quad
    /// (optionally limited to a partial selection).
    pub fn loop_cut_ring_has_quads(
        &self,
        start_he: HalfEdgeId,
        limit: Option<&std::collections::HashSet<u32>>,
    ) -> bool {
        if start_he.0 as usize >= self.half_edges.len() {
            return false;
        }
        let (quads, _) = self.loop_ring_quads(start_he, limit);
        !quads.is_empty()
    }

    /// First live face that has both `a` and `b` on its boundary (for multi-step strip splits).
    ///
    /// Does not use `vertex_fan` (boundary-incomplete on open meshes): scans live faces' rings.
    /// Callers pass strip faces first via `unwrap_or(face)` fallback; this is the rare miss path.
    fn face_containing_verts(&self, a: VertexId, b: VertexId) -> Option<FaceId> {
        // Prefer faces incident to `a` via its stored half_edge + next-walk on that face only,
        // then a full scan. vertex_fan is wrong on open boundaries (stops at first NO_TWIN).
        for fi in 0..self.faces.len() as u32 {
            if self.free_faces.contains(&fi) {
                continue;
            }
            let fid = FaceId(fi);
            if self.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let ring = self.face_vertices(fid);
            if ring.contains(&a) && ring.contains(&b) {
                return Some(fid);
            }
        }
        None
    }

    /// Half-edge on `face` whose destination is `dst` and source is `src` (face-local, O(n)).
    fn face_he_between(&self, face: FaceId, src: VertexId, dst: VertexId) -> Option<HalfEdgeId> {
        let hes = self.face_half_edges(face);
        let verts = self.face_vertices(face); // verts[k] = dest of hes[k]
        if hes.len() != verts.len() {
            return None;
        }
        for (i, &v) in verts.iter().enumerate() {
            if v == dst {
                let n = verts.len();
                let prev = verts[(i + n - 1) % n];
                if prev == src {
                    return Some(hes[i]);
                }
            }
        }
        None
    }

    /// Rail half-edge on `face` for undirected edge bottom—top.
    /// Returns `(he, reversed)` where `reversed` means `he` runs top→bottom (map t → 1−t).
    fn face_rail_he(
        &self,
        face: FaceId,
        bottom: VertexId,
        top: VertexId,
    ) -> Option<(HalfEdgeId, bool)> {
        if let Some(h) = self.face_he_between(face, bottom, top) {
            return Some((h, false));
        }
        if let Some(h) = self.face_he_between(face, top, bottom) {
            return Some((h, true));
        }
        None
    }

}
