use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_TWIN};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Blender **Select Similar** (Shift+G) — expand selection by attribute of the current mode.
    ///
    /// | Mode   | Criterion |
    /// |--------|-----------|
    /// | Face   | Normal within 10° of selection average (or area within 15% of avg) — **Normal** first |
    /// | Edge   | Length within 10% of selection average |
    /// | Vertex | Same valence as any selected vertex |
    ///
    /// Requires a non-empty selection (or hover seed for edge). Selection is **extended** (union).
    pub(crate) fn poly_select_similar(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        match self.poly_work_mode {
            PolyWorkMode::Face | PolyWorkMode::Group => self.poly_select_similar_faces(nid),
            PolyWorkMode::Edge => self.poly_select_similar_edges(nid),
            PolyWorkMode::Vertex => self.poly_select_similar_verts(nid),
            PolyWorkMode::Object => {
                self.toast("Select Similar: enter Vertex / Edge / Face mode");
            }
        }
    }

    fn poly_select_similar_faces(&mut self, nid: core_math::scene_graph::NodeId) {
        let seeds: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        if seeds.is_empty() {
            self.toast("Select Similar: select at least one face");
            return;
        }
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            self.toast("Select Similar: not a poly mesh");
            return;
        };

        // Average normal of seeds (unit).
        let mut n_sum = Vec3::ZERO;
        let mut area_sum = 0.0f32;
        let mut n_area = 0u32;
        for &fid in &seeds {
            let n = mesh.compute_face_normal(FaceId(fid));
            let len = n.length();
            if len > 1e-8 {
                n_sum += n / len;
                // |Newell| ≈ 2×area; use length of unnormalized normal via recompute.
                // compute_face_normal returns unit — use face_vertices polygon area.
                let pts: Vec<Vec3> = mesh
                    .face_vertices(FaceId(fid))
                    .iter()
                    .map(|&v| mesh.vert(v).position)
                    .collect();
                let a = poly_area(&pts);
                area_sum += a;
                n_area += 1;
            }
        }
        let ref_n = n_sum.normalize_or_zero();
        if ref_n.length_squared() < 0.25 {
            self.toast("Select Similar: invalid face normals");
            return;
        }
        let avg_area = if n_area > 0 {
            area_sum / n_area as f32
        } else {
            0.0
        };
        // 10° normal threshold; 15% area band (secondary — face must match normal OR we only do normal).
        // Blender separates modes; we do **Normal** (primary).
        let cos_min = 10.0f32.to_radians().cos();
        let mut added = 0u32;
        for i in 0..mesh.face_count() as u32 {
            if mesh.face(FaceId(i)).half_edge == core_math::half_edge::NO_EDGE {
                continue;
            }
            // Skip free faces — face_count is live; use free_faces check via half_edge sentinel already.
            let n = mesh.compute_face_normal(FaceId(i)).normalize_or_zero();
            if n.dot(ref_n) >= cos_min {
                if self.poly_sel.selected_faces.insert(i) {
                    added += 1;
                }
            }
        }
        let _ = avg_area; // reserved if we add Area mode toggle later
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Select Similar Normal: +{added} faces"));
    }

    fn poly_select_similar_edges(&mut self, nid: core_math::scene_graph::NodeId) {
        let mut seeds: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        if seeds.is_empty() {
            if let Some(h) = self.poly_sel.hovered_edge {
                seeds.push(h);
            }
        }
        if seeds.is_empty() {
            self.toast("Select Similar: select or hover an edge");
            return;
        }
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            self.toast("Select Similar: not a poly mesh");
            return;
        };

        let edge_len = |he: HalfEdgeId| -> f32 {
            let a = mesh.he_source(he);
            let b = mesh.he(he).vertex;
            mesh.vert(a).position.distance(mesh.vert(b).position)
        };
        let mut sum = 0.0f32;
        let mut cnt = 0u32;
        for &e in &seeds {
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                continue;
            }
            sum += edge_len(he);
            cnt += 1;
        }
        if cnt == 0 {
            return;
        }
        let avg = sum / cnt as f32;
        let lo = avg * 0.90;
        let hi = avg * 1.10;
        let mut added = 0u32;
        let he_n = mesh.half_edges.len();
        for i in 0..he_n {
            let he = HalfEdgeId(i as u32);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let tw = mesh.he(he).twin;
            if tw != NO_TWIN && tw.0 < i as u32 {
                continue; // undirected once
            }
            let len = edge_len(he);
            if len >= lo && len <= hi {
                if self.poly_sel.selected_edges.insert(he.0) {
                    added += 1;
                }
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Select Similar Length: +{added} edges"));
    }

    fn poly_select_similar_verts(&mut self, nid: core_math::scene_graph::NodeId) {
        let seeds: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if seeds.is_empty() {
            self.toast("Select Similar: select at least one vertex");
            return;
        }
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            self.toast("Select Similar: not a poly mesh");
            return;
        };

        let mut target_valences = std::collections::HashSet::new();
        for &v in &seeds {
            target_valences.insert(mesh.vertex_valence(VertexId(v)));
        }
        let mut added = 0u32;
        for i in 0..mesh.vertices.len() as u32 {
            let v = VertexId(i);
            // Skip freed verts (half_edge sentinel).
            if mesh.vert(v).half_edge == core_math::half_edge::NO_EDGE {
                continue;
            }
            let val = mesh.vertex_valence(v);
            if target_valences.contains(&val) && self.poly_sel.selected_vertices.insert(i) {
                added += 1;
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Select Similar Valence: +{added} verts"));
    }

    /// Active Loop Select params (MESHmachine panel → kernel).
    pub(crate) fn poly_loop_select_params(&self) -> core_math::edge_select::LoopSelectParams {
        core_math::edge_select::LoopSelectParams {
            min_angle_deg: self.loop_sel_min_angle.clamp(0.0, 179.0),
            max_steps: if self.loop_sel_use_edge_limit {
                Some(self.loop_sel_steps.max(1))
            } else {
                None
            },
        }
    }

    /// True when every currently selected edge is Mark Sharp (MESHmachine "all-sharp selection"
    /// branch of the Select wrapper). Empty selection → false.
    fn poly_selection_is_all_sharp(&self, nid: core_math::scene_graph::NodeId) -> bool {
        if self.poly_sel.selected_edges.is_empty() {
            return false;
        }
        let Some(cd) = self.cache.poly_custom_data.get(&nid) else {
            return false;
        };
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return false;
        };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return false;
        };
        for &e in &self.poly_sel.selected_edges {
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                return false;
            }
            let twin = mesh.he(he).twin;
            let sharp = cd.sharp.get(he)
                || (twin != NO_TWIN && cd.sharp.get(twin));
            if !sharp {
                return false;
            }
        }
        true
    }

    /// Grow selection along connected Mark Sharp edges (MESHmachine SSelect-lite when
    /// Always Loop Select is off and the seed selection is all sharp).
    fn poly_select_connected_sharp(&mut self, nid: core_math::scene_graph::NodeId, seed: u32) {
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
        let Some(cd) = self.cache.poly_custom_data.get(&nid) else { return };
        let is_sharp = |he: HalfEdgeId| -> bool {
            if !mesh.is_edge_live(he) {
                return false;
            }
            let twin = mesh.he(he).twin;
            cd.sharp.get(he) || (twin != NO_TWIN && cd.sharp.get(twin))
        };
        let und = |he: HalfEdgeId| -> u32 {
            let t = mesh.he(he).twin;
            if t != NO_TWIN {
                he.0.min(t.0)
            } else {
                he.0
            }
        };
        let seed_he = HalfEdgeId(seed);
        if !is_sharp(seed_he) {
            return;
        }
        let mut stack = vec![und(seed_he)];
        let mut seen = std::collections::HashSet::new();
        seen.insert(und(seed_he));
        while let Some(e) = stack.pop() {
            self.poly_sel.selected_edges.insert(e);
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                continue;
            }
            for end in [mesh.he_source(he).0, mesh.he(he).vertex.0] {
                for h in mesh.vertex_fan(VertexId(end)) {
                    if !is_sharp(h) {
                        continue;
                    }
                    let k = und(h);
                    if seen.insert(k) {
                        stack.push(k);
                    }
                }
            }
        }
    }

    /// Canonical undirected id for a half-edge (lower of twin pair).
    fn poly_edge_canon(mesh: &core_math::half_edge::HalfEdgeMesh, he: HalfEdgeId) -> u32 {
        let t = mesh.he(he).twin;
        if t != NO_TWIN {
            he.0.min(t.0)
        } else {
            he.0
        }
    }

    /// True when undirected edges `a` and `b` share a vertex.
    fn poly_edges_share_vertex(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        a: HalfEdgeId,
        b: HalfEdgeId,
    ) -> bool {
        let (as_, ad) = (mesh.he_source(a).0, mesh.he(a).vertex.0);
        let (bs, bd) = (mesh.he_source(b).0, mesh.he(b).vertex.0);
        as_ == bs || as_ == bd || ad == bs || ad == bd
    }

    /// Is undirected edge `he` in the current edge selection (either half id).
    fn poly_edge_is_selected(
        &self,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        he: HalfEdgeId,
    ) -> bool {
        let c = Self::poly_edge_canon(mesh, he);
        if self.poly_sel.selected_edges.contains(&c) || self.poly_sel.selected_edges.contains(&he.0)
        {
            return true;
        }
        let t = mesh.he(he).twin;
        t != NO_TWIN && self.poly_sel.selected_edges.contains(&t.0)
    }

    /// Resolve a directed-loop driver for `seed` (Alt+Shift / two-edge orientation).
    ///
    /// Priority (Blender Select Next Active / MeshMachine directed loop):
    /// 1. Selection-history: most recent still-selected edge that shares a vertex with the seed
    /// 2. Any currently selected edge (not the seed) that shares a vertex with the seed
    fn poly_resolve_loop_driver(
        &self,
        mesh: &core_math::half_edge::HalfEdgeMesh,
        seed: HalfEdgeId,
    ) -> Option<HalfEdgeId> {
        let seed_c = Self::poly_edge_canon(mesh, seed);
        // 1) History: most recent still-selected edge other than seed that touches seed.
        for &e in self.poly_edge_history.iter().rev() {
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                continue;
            }
            if Self::poly_edge_canon(mesh, he) == seed_c {
                continue;
            }
            if !self.poly_edge_is_selected(mesh, he) {
                continue;
            }
            if Self::poly_edges_share_vertex(mesh, he, seed) {
                return Some(he);
            }
        }
        // 2) Any selected edge sharing a vertex with the seed.
        for &e in &self.poly_sel.selected_edges {
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                continue;
            }
            if Self::poly_edge_canon(mesh, he) == seed_c {
                continue;
            }
            if Self::poly_edges_share_vertex(mesh, he, seed) {
                return Some(he);
            }
        }
        None
    }

    /// Canonize loop half-edges for selection (no mut self — call then insert).
    fn poly_loop_canons(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        loop_edges: &[HalfEdgeId],
    ) -> Vec<u32> {
        let mut out = Vec::with_capacity(loop_edges.len());
        let mut seen = std::collections::HashSet::new();
        for he in loop_edges {
            if !mesh.is_edge_live(*he) {
                continue;
            }
            let c = Self::poly_edge_canon(mesh, *he);
            if seen.insert(c) {
                out.push(c);
            }
        }
        out
    }

    /// Select an edge loop from the currently selected/hovered edge (Alt+click).
    ///
    /// MESHmachine Select / LSelect parity:
    /// * **Always Loop Select ON** (default) → angle+topology hybrid walk (`edge_loop_with_params`)
    /// * **Always Loop Select OFF** + all-sharp selection → connected sharp edges
    /// * directed walk when a second selected edge shares a vertex with the seed
    /// * Min Angle / Use Edge Limit / Steps from the Loop Select tool panel
    ///
    /// `prefer_directed` = **Alt+Shift**: force the two-edge oriented walk when a driver exists
    /// (Blender "use two edges to pick which loop continues through a pole").
    pub(crate) fn poly_select_edge_loop(&mut self) {
        self.poly_select_edge_loop_ex(false);
    }

    /// Alt+Shift directed edge loop (two selected edges orient the walk).
    pub(crate) fn poly_select_edge_loop_directed(&mut self) {
        self.poly_select_edge_loop_ex(true);
    }

    pub(crate) fn poly_select_edge_loop_ex(&mut self, prefer_directed: bool) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let he_id = match self
            .poly_sel
            .hovered_edge
            .or_else(|| self.poly_sel.selected_edges.iter().next().copied())
        {
            Some(e) => e,
            None => return,
        };

        // Always Loop Select OFF + all-sharp selection → SSelect-style connected sharps.
        // Directed (Alt+Shift) always wants geometry loop orientation, not sharp flood.
        if !prefer_directed && !self.loop_sel_always_loop && self.poly_selection_is_all_sharp(nid) {
            self.poly_select_connected_sharp(nid, he_id);
            self.poly_push_edge_history(he_id);
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        let params = self.poly_loop_select_params();
        // Collect canonical edge ids first (immutable mesh borrow), then mutate selection.
        let (canons, last_canon): (Vec<u32>, Option<u32>) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return;
            };
            let clicked = HalfEdgeId(he_id);
            if !mesh.is_edge_live(clicked) {
                return;
            }
            let driver = self.poly_resolve_loop_driver(mesh, clicked);
            // Directed whenever a driver exists (Alt+Shift requires two-edge orientation;
            // plain Alt also uses it when two adjacent edges are already selected — Blender
            // second-edge orientation through poles / hard junctions).
            let loop_edges = if let Some(d) = driver {
                core_math::edge_select::edge_loop_directed_with_params(mesh, d, clicked, params)
            } else if prefer_directed {
                // Alt+Shift with no adjacent driver: grow from history pair (prev → active).
                if let Some((prev, active)) = self.poly_edge_history_pair_live(mesh) {
                    core_math::edge_select::edge_loop_directed_with_params(
                        mesh,
                        HalfEdgeId(prev),
                        HalfEdgeId(active),
                        params,
                    )
                } else {
                    core_math::edge_select::edge_loop_with_params(mesh, clicked, params)
                }
            } else {
                core_math::edge_select::edge_loop_with_params(mesh, clicked, params)
            };
            let canons = Self::poly_loop_canons(mesh, &loop_edges);
            let last = canons.last().copied();
            (canons, last)
        };
        for c in canons {
            self.poly_sel.selected_edges.insert(c);
        }
        self.poly_push_edge_history(he_id);
        if let Some(last) = last_canon {
            self.poly_push_edge_history(last);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Last two live edges from selection history (prev, active), for directed loop without hover.
    fn poly_edge_history_pair_live(
        &self,
        mesh: &core_math::half_edge::HalfEdgeMesh,
    ) -> Option<(u32, u32)> {
        let mut pair: Vec<u32> = Vec::with_capacity(2);
        for &e in self.poly_edge_history.iter().rev() {
            let he = HalfEdgeId(e);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let c = Self::poly_edge_canon(mesh, he);
            if pair.contains(&c) {
                continue;
            }
            // Prefer edges still selected, but accept history for direction even if cleared.
            pair.push(c);
            if pair.len() == 2 {
                // history back = most recent = active; second = prev
                return Some((pair[1], pair[0]));
            }
        }
        None
    }

    const SEL_HISTORY_CAP: usize = 32;

    /// Push a face onto the selection history (Blender "active" chain). Dedups consecutive repeats.
    pub(crate) fn poly_push_face_history(&mut self, f: u32) {
        if self.poly_face_history.last() == Some(&f) {
            return;
        }
        self.poly_face_history.push(f);
        if self.poly_face_history.len() > Self::SEL_HISTORY_CAP {
            let n = self.poly_face_history.len() - Self::SEL_HISTORY_CAP;
            self.poly_face_history.drain(0..n);
        }
    }

    /// Push an edge onto the selection history.
    pub(crate) fn poly_push_edge_history(&mut self, e: u32) {
        if self.poly_edge_history.last() == Some(&e) {
            return;
        }
        self.poly_edge_history.push(e);
        if self.poly_edge_history.len() > Self::SEL_HISTORY_CAP {
            let n = self.poly_edge_history.len() - Self::SEL_HISTORY_CAP;
            self.poly_edge_history.drain(0..n);
        }
    }

    /// Push a vertex onto the selection history (Blender "active" chain for Next Active).
    pub(crate) fn poly_push_vert_history(&mut self, v: u32) {
        if self.poly_vert_history.last() == Some(&v) {
            return;
        }
        self.poly_vert_history.push(v);
        if self.poly_vert_history.len() > Self::SEL_HISTORY_CAP {
            let n = self.poly_vert_history.len() - Self::SEL_HISTORY_CAP;
            self.poly_vert_history.drain(0..n);
        }
    }

    /// Last two *still-selected* faces from history (prev, active). Needs ≥2 for direction.
    fn poly_face_history_pair(&self) -> Option<(u32, u32)> {
        let mut pair = Vec::with_capacity(2);
        for &f in self.poly_face_history.iter().rev() {
            if self.poly_sel.selected_faces.contains(&f) && !pair.contains(&f) {
                pair.push(f);
                if pair.len() == 2 {
                    // history back = most recent = active; second = prev
                    return Some((pair[1], pair[0]));
                }
            }
        }
        None
    }

    /// Last two *still-selected* edges from history (prev, active).
    /// Twin-aware: history may hold either half of an undirected edge.
    fn poly_edge_history_pair(&self) -> Option<(u32, u32)> {
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let edge_still_selected = |e: u32| -> bool {
            if self.poly_sel.selected_edges.contains(&e) {
                return true;
            }
            if (e as usize) < mesh.half_edges.len() {
                let t = mesh.he(HalfEdgeId(e)).twin;
                if t != core_math::half_edge::NO_TWIN
                    && self.poly_sel.selected_edges.contains(&t.0)
                {
                    return true;
                }
            }
            false
        };
        let same_undirected = |a: u32, b: u32| -> bool {
            if a == b {
                return true;
            }
            if (a as usize) >= mesh.half_edges.len() || (b as usize) >= mesh.half_edges.len() {
                return false;
            }
            let ta = mesh.he(HalfEdgeId(a)).twin;
            let tb = mesh.he(HalfEdgeId(b)).twin;
            (ta != core_math::half_edge::NO_TWIN && ta.0 == b)
                || (tb != core_math::half_edge::NO_TWIN && tb.0 == a)
        };
        let mut pair = Vec::with_capacity(2);
        for &e in self.poly_edge_history.iter().rev() {
            if edge_still_selected(e) && !pair.iter().any(|&p| same_undirected(p, e)) {
                pair.push(e);
                if pair.len() == 2 {
                    return Some((pair[1], pair[0]));
                }
            }
        }
        None
    }

    /// Last two *still-selected* verts from history (prev, active).
    fn poly_vert_history_pair(&self) -> Option<(u32, u32)> {
        let mut pair = Vec::with_capacity(2);
        for &v in self.poly_vert_history.iter().rev() {
            if self.poly_sel.selected_vertices.contains(&v) && !pair.contains(&v) {
                pair.push(v);
                if pair.len() == 2 {
                    return Some((pair[1], pair[0]));
                }
            }
        }
        None
    }

    /// Face LOOP (Blender-style). Owner 2026-08-06:
    /// - Plain double-click → island (handled elsewhere).
    /// - **Shift+double-click** → full face loop.
    /// - Direction from selection history (last two selected faces). Needs ≥2 faces —
    ///   select A, then B, then Shift+dbl to walk A→B→…
    pub(crate) fn poly_select_face_loop(&mut self) {
        use core_math::half_edge::FaceId;
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return;
        };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return;
        };

        let hover = self.poly_sel.hovered_face;
        // Prefer history pair (true selection order), then any two selected, then toast.
        let loop_faces: Vec<FaceId> = if let Some((f0, f1)) = self.poly_face_history_pair() {
            core_math::edge_select::face_loop_from_two(mesh, FaceId(f0), FaceId(f1))
        } else if self.poly_sel.selected_faces.len() >= 2 {
            let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
            let (f0, f1) = if let Some(h) = hover {
                if faces.contains(&h) {
                    let other = faces.iter().copied().find(|&f| f != h).unwrap_or(h);
                    (other, h)
                } else {
                    (faces[0], faces[1])
                }
            } else {
                (faces[0], faces[1])
            };
            core_math::edge_select::face_loop_from_two(mesh, FaceId(f0), FaceId(f1))
        } else {
            // One face only — no reliable direction (owner: need 2 polys for loop direction).
            self.toast("Face loop: select two faces first (A then B), then Shift+double-click");
            return;
        };

        for f in &loop_faces {
            self.poly_sel.selected_faces.insert(f.0);
            self.poly_push_face_history(f.0);
        }
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Blender **Select Next/Previous Active** (Shift+Ctrl+Numpad±), exposed as Shift+Scroll.
    /// Uses selection history: the two most recently selected elements define a direction;
    /// grow selects the next logical neighbour, shrink undoes the last.
    ///
    /// Works in Vertex mode (along a vertex chain), Edge mode (along edge loop), and Face mode
    /// (along face loop). Oracle: Blender Manual → Select → More/Less → Next/Previous Active.
    pub(crate) fn poly_loop_extend(&mut self, grow: bool) {
        match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_vert_next_active(grow),
            PolyWorkMode::Face | PolyWorkMode::Group => self.poly_face_next_active(grow),
            PolyWorkMode::Edge => self.poly_edge_next_active(grow),
            _ => {}
        }
    }

    /// Vertex-mode Next/Prev Active (Blender `mesh.select_next_item` / `select_prev_item`).
    /// History pair prev→active orients the step; grow selects the next neighbour, shrink
    /// deselects the most recent history vert still selected.
    fn poly_vert_next_active(&mut self, grow: bool) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };

        if !grow {
            if let Some(last) = self
                .poly_vert_history
                .iter()
                .rev()
                .copied()
                .find(|v| self.poly_sel.selected_vertices.contains(v))
            {
                self.poly_sel.selected_vertices.remove(&last);
                self.poly_vert_history.retain(|&x| x != last);
                if self.poly_active_vertex == Some(last) {
                    self.poly_active_vertex = self.poly_vert_history.iter().rev().copied().next()
                        .or_else(|| self.poly_sel.selected_vertices.iter().copied().next());
                }
                self.mark_dirty(crate::DirtyFlags::RENDER);
            }
            return;
        }

        let Some((prev, active)) = self.poly_vert_history_pair() else {
            if self.poly_sel.selected_vertices.len() < 2 {
                self.toast("Next vert: select two verts (A then B) to set direction");
            }
            return;
        };

        let next = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            core_math::edge_select::vert_next_active(mesh, prev, active)
        };

        if let Some(nv) = next {
            // Skip already-selected verts by walking until an unselected one (or dead end).
            let mut walk_act = active;
            let mut candidate = Some(nv);
            for _ in 0..64 {
                let Some(c) = candidate else { break; };
                if self.poly_sel.selected_vertices.insert(c) {
                    self.poly_push_vert_history(c);
                    self.poly_active_vertex = Some(c);
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                    return;
                }
                // Already selected — step past it along the same direction.
                let nxt = {
                    let node = match self.scene.scene_graph.get(nid) {
                        Some(n) => n,
                        None => return,
                    };
                    let mesh = match &node.payload {
                        NodePayload::PolygonalBody { mesh, .. } => mesh,
                        _ => return,
                    };
                    core_math::edge_select::vert_next_active(mesh, walk_act, c)
                };
                walk_act = c;
                candidate = nxt;
            }
        }
    }

    /// Face-mode Next/Prev Active from history (Blender: two most recent faces → next in strip).
    fn poly_face_next_active(&mut self, grow: bool) {
        use core_math::half_edge::{FaceId, NO_FACE, NO_TWIN};
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };

        if !grow {
            // Previous Active: deselect the most recent history face that is still selected.
            if let Some(last) = self
                .poly_face_history
                .iter()
                .rev()
                .copied()
                .find(|f| self.poly_sel.selected_faces.contains(f))
            {
                self.poly_sel.selected_faces.remove(&last);
                self.poly_face_history.retain(|&x| x != last);
                self.mark_dirty(crate::DirtyFlags::RENDER);
            }
            return;
        }

        let Some((prev, active)) = self.poly_face_history_pair() else {
            // Need two faces in history to establish direction.
            if self.poly_sel.selected_faces.len() < 2 {
                self.toast("Next face: select two faces (A then B) to set direction");
            }
            return;
        };

        let next: Option<u32> = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            // Shared edge prev↔active, then step from active across the opposite side.
            let mut shared: Option<HalfEdgeId> = None;
            for h in mesh.face_half_edges(FaceId(active)) {
                let t = mesh.he(h).twin;
                if t != NO_TWIN && mesh.he(t).face.0 == prev {
                    shared = Some(h);
                    break;
                }
            }
            let Some(entry) = shared else {
                return;
            };
            // Opposite half-edge on the active face (quad strip step).
            let ring = mesh.face_half_edges(FaceId(active));
            let n = ring.len();
            if n < 3 {
                None
            } else if let Some(idx) = ring.iter().position(|&h| {
                h == entry || {
                    let t = mesh.he(h).twin;
                    t != NO_TWIN && t == entry
                }
            }) {
                let opp = ring[(idx + n / 2) % n]; // for quads: opposite edge
                let tw = mesh.he(opp).twin;
                if tw == NO_TWIN {
                    None
                } else {
                    let nf = mesh.he(tw).face;
                    if nf == NO_FACE {
                        None
                    } else {
                        Some(nf.0)
                    }
                }
            } else {
                None
            }
        };

        if let Some(nf) = next {
            if self.poly_sel.selected_faces.insert(nf) {
                self.poly_push_face_history(nf);
                self.mark_dirty(crate::DirtyFlags::RENDER);
            }
        }
    }

    /// Edge-mode Next/Prev Active: history pair orients a directed edge-loop step.
    fn poly_edge_next_active(&mut self, grow: bool) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };

        if !grow {
            if let Some(last) = self
                .poly_edge_history
                .iter()
                .rev()
                .copied()
                .find(|e| self.poly_sel.selected_edges.contains(e))
            {
                // Remove last + twin
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                        if (last as usize) < mesh.half_edges.len() {
                            let t = mesh.he(HalfEdgeId(last)).twin;
                            self.poly_sel.selected_edges.remove(&last);
                            if t != core_math::half_edge::NO_TWIN {
                                self.poly_sel.selected_edges.remove(&t.0);
                            }
                        }
                    }
                }
                self.poly_sel.selected_edges.remove(&last);
                self.poly_edge_history.retain(|&x| x != last);
                self.mark_dirty(crate::DirtyFlags::RENDER);
            }
            return;
        }

        // Prefer history-directed step (Blender Next Active); fall back to frontier walk.
        if let Some((prev, active)) = self.poly_edge_history_pair() {
            let next: Option<u32> = {
                let node = match self.scene.scene_graph.get(nid) {
                    Some(n) => n,
                    None => return,
                };
                let mesh = match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => mesh,
                    _ => return,
                };
                let is_sel = |h: HalfEdgeId| -> bool {
                    let t = mesh.he(h).twin;
                    self.poly_sel.selected_edges.contains(&h.0)
                        || (t != core_math::half_edge::NO_TWIN
                            && self.poly_sel.selected_edges.contains(&t.0))
                };
                core_math::edge_select::edge_next_active(
                    mesh,
                    HalfEdgeId(prev),
                    HalfEdgeId(active),
                    &is_sel,
                )
                .map(|h| h.0)
            };
            if let Some(h) = next {
                let canon = {
                    let node = self.scene.scene_graph.get(nid);
                    if let Some(NodePayload::PolygonalBody { mesh, .. }) =
                        node.map(|n| &n.payload)
                    {
                        if (h as usize) < mesh.half_edges.len() {
                            let t = mesh.he(HalfEdgeId(h)).twin;
                            if t != core_math::half_edge::NO_TWIN {
                                h.min(t.0)
                            } else {
                                h
                            }
                        } else {
                            h
                        }
                    } else {
                        h
                    }
                };
                if self.poly_sel.selected_edges.insert(canon) {
                    self.poly_push_edge_history(canon);
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                }
            }
            return;
        }

        // Fallback: frontier walk (legacy) when history has <2 edges.
        let seed = match self
            .poly_sel
            .hovered_edge
            .or_else(|| self.poly_sel.selected_edges.iter().next().copied())
        {
            Some(e) => e,
            None => return,
        };
        let action: Option<(u32, Option<u32>)> = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let is_sel = |h: HalfEdgeId| -> bool {
                let t = mesh.he(h).twin;
                self.poly_sel.selected_edges.contains(&h.0)
                    || (t != core_math::half_edge::NO_TWIN
                        && self.poly_sel.selected_edges.contains(&t.0))
            };
            let lp = core_math::edge_select::edge_loop(mesh, HalfEdgeId(seed));
            let twin_of = |h: HalfEdgeId| {
                let t = mesh.he(h).twin;
                if t != core_math::half_edge::NO_TWIN {
                    Some(t.0)
                } else {
                    None
                }
            };
            let touches_selection = |h: HalfEdgeId| -> bool {
                let (s, d) = (mesh.he_source(h).0, mesh.he(h).vertex.0);
                lp.iter().any(|&o| {
                    if o == h || !is_sel(o) {
                        return false;
                    }
                    let (os, od) = (mesh.he_source(o).0, mesh.he(o).vertex.0);
                    s == os || s == od || d == os || d == od
                })
            };
            let any_selected = lp.iter().any(|&h| is_sel(h));
            lp.iter()
                .find(|&&h| !is_sel(h) && (!any_selected || touches_selection(h)))
                .or_else(|| lp.iter().find(|&&h| !is_sel(h)))
                .map(|&h| (h.0, twin_of(h)))
        };
        if let Some((h, t)) = action {
            let canon = match t {
                Some(t) => h.min(t),
                None => h,
            };
            self.poly_sel.selected_edges.insert(canon);
            self.poly_push_edge_history(canon);
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// Select an edge ring from the currently selected/hovered edge (Alt+Ctrl / Ctrl+Alt click).
    ///
    /// Uses opposite-on-face (quad + n-gon) so hard-surface strips walk further than the old
    /// quad-only `select_edge_ring`. Honours Loop Select **Use Edge Limit / Steps**.
    pub(crate) fn poly_select_edge_ring(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let he_id = match self.poly_sel.hovered_edge.or_else(|| self.poly_sel.selected_edges.iter().next().copied()) {
            Some(e) => e,
            None => return,
        };

        let max_steps = if self.loop_sel_use_edge_limit {
            Some(self.loop_sel_steps.max(1))
        } else {
            None
        };
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                let ring_edges = core_math::edge_select::edge_ring_with_params(
                    mesh,
                    HalfEdgeId(he_id),
                    max_steps,
                );
                for he in ring_edges {
                    let t = mesh.he(he).twin;
                    let canon = if t != NO_TWIN { he.0.min(t.0) } else { he.0 };
                    self.poly_sel.selected_edges.insert(canon);
                }
                self.poly_push_edge_history(he_id);
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }
}

/// Planar polygon area from Newell magnitude / 2.
fn poly_area(pts: &[Vec3]) -> f32 {
    if pts.len() < 3 {
        return 0.0;
    }
    let n = core_math::half_edge::newell_normal(pts).length() * 0.5;
    n
}
