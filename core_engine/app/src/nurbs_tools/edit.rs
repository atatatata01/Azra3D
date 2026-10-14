// core_engine/app/src/nurbs_tools/edit.rs
// Hover/pick/select, translate, CV manip, knot insert, keyboard, CV drag/transform.

use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, NurbsWorkMode, NurbsTool,
    ActiveTool, NurbsCvDrag, NurbsKeyFlags, DirtyFlags,
    SceneItemId, world_to_screen,
};

impl AzraApp {
    // ============================================================
    // Hover / click pick (Face / Edge / Vertex)
    // ============================================================

    /// Hover pick for NURBS sub-elements.
    ///
    /// **Modeless** when `work_mode == Object`: priority **vertex > edge > face**
    /// (small target wins). Locked Face/Edge/Vertex modes only test that domain.
    /// Always front-face only (no X-ray).
    pub(crate) fn nurbs_hover_pick(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        self.nurbs_sel.hovered_cv = None;
        self.nurbs_sel.hovered_face = None;
        self.nurbs_sel.hovered_edge = None;

        if self.modeling_context != ModelingContext::Nurbs {
            return;
        }
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => return,
        };
        let n_faces = self.nurbs_face_count(solid);
        if n_faces == 0 {
            return;
        }

        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        let modeless = self.nurbs_work_mode == NurbsWorkMode::Object;
        let want_vert = modeless || self.nurbs_work_mode == NurbsWorkMode::Vertex;
        let want_edge = modeless || self.nurbs_work_mode == NurbsWorkMode::Edge;
        let want_face = modeless || self.nurbs_work_mode == NurbsWorkMode::Face;

        // ── Vertex (priority 1) ──────────────────────────────────────────
        if want_vert {
            let mut best = 12.0f32;
            let mut best_cv = None;
            let mut best_fi = None;
            for fi in 0..n_faces {
                let fi = fi as u32;
                if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                    continue;
                }
                let xform = self.nurbs_face_world_xform(solid, fi);
                self.nurbs_with_face_surface(solid, fi, |surface| {
                    for (row, cp_row) in surface.control_points.iter().enumerate() {
                        for (col, cp) in cp_row.iter().enumerate() {
                            let p = xform.transform_point3(cp.position);
                            if let Some(sp) =
                                world_to_screen(p, cam_pos, orbit, rect, aspect, ortho_hw, fov)
                            {
                                let d = (sp - mouse).length();
                                if d < best {
                                    best = d;
                                    best_cv = Some((row as u32, col as u32));
                                    best_fi = Some(fi);
                                }
                            }
                        }
                    }
                });
            }
            if best_cv.is_some() {
                self.nurbs_sel.hovered_cv = best_cv;
                // Remember face for CV selection context (stable id)
                if let Some(fi) = best_fi {
                    self.nurbs_sel.hovered_face = self.nurbs_face_id_at(solid, fi);
                }
                if modeless {
                    return; // vertex wins — stop
                }
            }
        }

        // ── Edge (priority 2) ────────────────────────────────────────────
        // Sample CURRENT face geometry every frame (no cached polylines).
        if want_edge && self.nurbs_sel.hovered_cv.is_none() {
            let mut best = 10.0f32;
            let mut best_e = None;
            for fi in 0..n_faces {
                let fi = fi as u32;
                if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                    continue;
                }
                let Some(fid) = self.nurbs_face_id_at(solid, fi) else {
                    continue;
                };
                let xform = self.nurbs_face_world_xform(solid, fi);
                let hit: Option<(f32, u8)> = self
                    .nurbs_with_face_surface(solid, fi, |surface| {
                        // Sample along UV boundary (works for closed cylinder rims too).
                        let (u0, u1) = surface.domain_u();
                        let (v0, v1) = surface.domain_v();
                        let edge_uv = [
                            (u0, v0, u0, v1),
                            (u0, v1, u1, v1),
                            (u1, v1, u1, v0),
                            (u1, v0, u0, v0),
                        ];
                        let mut local: Option<(f32, u8)> = None;
                        for (ei, (ua, va, ub, vb)) in edge_uv.iter().enumerate() {
                            let mut prev: Option<egui::Pos2> = None;
                            let mut dmin = f32::MAX;
                            for i in 0..=16 {
                                let t = i as f32 / 16.0;
                                let p = xform.transform_point3(surface.evaluate(
                                    ua + (ub - ua) * t,
                                    va + (vb - va) * t,
                                ));
                                if let Some(sp) =
                                    world_to_screen(p, cam_pos, orbit, rect, aspect, ortho_hw, fov)
                                {
                                    if let Some(pp) = prev {
                                        dmin = dmin.min(crate::dist_point_to_segment(mouse, pp, sp));
                                    }
                                    prev = Some(sp);
                                } else {
                                    prev = None;
                                }
                            }
                            if local.map_or(true, |(bd, _)| dmin < bd) {
                                local = Some((dmin, ei as u8));
                            }
                        }
                        local
                    })
                    .flatten();
                if let Some((d, ei)) = hit {
                    if d < best {
                        best = d;
                        best_e = Some((fid, ei));
                    }
                }
            }
            if best_e.is_some() {
                self.nurbs_sel.hovered_edge = best_e;
                if modeless {
                    return;
                }
            }
        }

        // ── Face (priority 3) ────────────────────────────────────────────
        if want_face && self.nurbs_sel.hovered_cv.is_none() && self.nurbs_sel.hovered_edge.is_none()
        {
            let mut best = 28.0f32;
            let mut best_id = None;
            for fi in 0..n_faces {
                let fi = fi as u32;
                if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                    continue;
                }
                let Some(fid) = self.nurbs_face_id_at(solid, fi) else {
                    continue;
                };
                let xform = self.nurbs_face_world_xform(solid, fi);
                // T4: sample UV grid, skip points outside face trims (notch must not pick)
                let hit = self.nurbs_with_face(solid, fi, |face| {
                    let surface = face.surface.as_ref();
                    let (u0, u1) = surface.domain_u();
                    let (v0, v1) = surface.domain_v();
                    let mut local_best = f32::MAX;
                    let mut any = false;
                    for iu in 0..=6 {
                        for iv in 0..=6 {
                            let u = u0 + (u1 - u0) * iu as f32 / 6.0;
                            let v = v0 + (v1 - v0) * iv as f32 / 6.0;
                            if !face.uv_inside(u, v) {
                                continue;
                            }
                            let p = xform.transform_point3(surface.evaluate(u, v));
                            if let Some(sp) =
                                world_to_screen(p, cam_pos, orbit, rect, aspect, ortho_hw, fov)
                            {
                                local_best = local_best.min((sp - mouse).length());
                                any = true;
                            }
                        }
                    }
                    if any {
                        Some(local_best)
                    } else {
                        None
                    }
                });
                if let Some(Some(d)) = hit {
                    if d < best {
                        best = d;
                        best_id = Some(fid);
                    }
                }
            }
            self.nurbs_sel.hovered_face = best_id;
        }
    }

    /// Four corners of a NURBS patch in local space (00, 01, 11, 10) for edge hull.
    fn nurbs_face_corners_local(surface: &core_math::nurbs::NurbsSurface) -> [Vec3; 4] {
        let (u0, u1) = surface.domain_u();
        let (v0, v1) = surface.domain_v();
        [
            surface.evaluate(u0, v0),
            surface.evaluate(u0, v1),
            surface.evaluate(u1, v1),
            surface.evaluate(u1, v0),
        ]
    }

    /// Click select — modeless uses hover priority; locked modes only that domain.
    pub(crate) fn nurbs_click_select(&mut self, shift: bool) {
        // Modeless (Object): commit whichever hover wins (vert > edge > face).
        if self.nurbs_work_mode == NurbsWorkMode::Object {
            if self.nurbs_sel.hovered_cv.is_some() {
                self.nurbs_work_mode = NurbsWorkMode::Vertex;
                self.nurbs_click_select_vertex(shift);
                return;
            }
            if self.nurbs_sel.hovered_edge.is_some() {
                self.nurbs_work_mode = NurbsWorkMode::Edge;
                self.nurbs_click_select_edge(shift);
                return;
            }
            if self.nurbs_sel.hovered_face.is_some() {
                self.nurbs_work_mode = NurbsWorkMode::Face;
                self.nurbs_click_select_face(shift);
                return;
            }
            if !shift {
                self.nurbs_clear_selection();
            }
            return;
        }
        match self.nurbs_work_mode {
            NurbsWorkMode::Object => {}
            NurbsWorkMode::Face => self.nurbs_click_select_face(shift),
            NurbsWorkMode::Edge => self.nurbs_click_select_edge(shift),
            NurbsWorkMode::Vertex => self.nurbs_click_select_vertex(shift),
        }
    }

    fn nurbs_click_select_face(&mut self, shift: bool) {
        if let Some(f) = self.nurbs_sel.hovered_face {
            if shift {
                if !self.nurbs_sel.selected_faces.remove(&f) {
                    self.nurbs_sel.selected_faces.insert(f);
                }
            } else {
                self.nurbs_sel.selected_faces.clear();
                self.nurbs_sel.selected_faces.insert(f);
                self.nurbs_sel.selected_edges.clear();
                self.nurbs_sel.selected_cvs.clear();
            }
        } else if !shift {
            self.nurbs_sel.selected_faces.clear();
        }
    }

    fn nurbs_click_select_edge(&mut self, shift: bool) {
        if let Some(e) = self.nurbs_sel.hovered_edge {
            if shift {
                if !self.nurbs_sel.selected_edges.remove(&e) {
                    self.nurbs_sel.selected_edges.insert(e);
                }
            } else {
                self.nurbs_sel.selected_edges.clear();
                self.nurbs_sel.selected_edges.insert(e);
                self.nurbs_sel.selected_faces.clear();
                self.nurbs_sel.selected_cvs.clear();
            }
        } else if !shift {
            self.nurbs_sel.selected_edges.clear();
        }
    }

    fn nurbs_click_select_vertex(&mut self, shift: bool) {
        if let Some(cv) = self.nurbs_sel.hovered_cv {
            if shift {
                if !self.nurbs_sel.selected_cvs.remove(&cv) {
                    self.nurbs_sel.selected_cvs.insert(cv);
                }
            } else {
                self.nurbs_sel.selected_cvs.clear();
                self.nurbs_sel.selected_cvs.insert(cv);
            }
            // Keep face context for multi-face solids
            if let Some(f) = self.nurbs_sel.hovered_face {
                if !shift {
                    self.nurbs_sel.selected_faces.clear();
                    self.nurbs_sel.selected_faces.insert(f);
                }
            }
        } else if !shift {
            self.nurbs_sel.selected_cvs.clear();
        }
    }

    /// Clear all NURBS sub-element selections.
    pub(crate) fn nurbs_clear_selection(&mut self) {
        self.nurbs_sel.clear_sub();
    }

    /// Pivot for G/R/S gizmo in NURBS edit modes (world space).
    pub(crate) fn nurbs_selection_center(&self) -> Option<Vec3> {
        let solid = self.nurbs_solid_root()?;
        if self.nurbs_face_count(solid) == 0 {
            return None;
        }
        let mut acc = Vec3::ZERO;
        let mut n = 0u32;

        // Face selection works in Face mode and after modeless pick
        if !self.nurbs_sel.selected_faces.is_empty()
            && (self.nurbs_work_mode == NurbsWorkMode::Face
                || self.nurbs_work_mode == NurbsWorkMode::Object)
        {
            for &fid in &self.nurbs_sel.selected_faces {
                let Some(fi) = self.nurbs_face_index(solid, fid) else {
                    continue;
                };
                let xform = self.nurbs_face_world_xform(solid, fi);
                let c = self.nurbs_with_face_surface(solid, fi, |surface| {
                    let (u0, u1) = surface.domain_u();
                    let (v0, v1) = surface.domain_v();
                    xform.transform_point3(surface.evaluate((u0 + u1) * 0.5, (v0 + v1) * 0.5))
                })?;
                acc += c;
                n += 1;
            }
            if n > 0 {
                return Some(acc / n as f32);
            }
        }
        if !self.nurbs_sel.selected_edges.is_empty()
            && (self.nurbs_work_mode == NurbsWorkMode::Edge
                || self.nurbs_work_mode == NurbsWorkMode::Object)
        {
            for &(fid, ei) in &self.nurbs_sel.selected_edges {
                let Some(fi) = self.nurbs_face_index(solid, fid) else {
                    continue;
                };
                let xform = self.nurbs_face_world_xform(solid, fi);
                // Fresh evaluate on CURRENT surface (never cached pre-regenerate polyline).
                let mid = self.nurbs_with_face_surface(solid, fi, |surface| {
                    let (u0, u1) = surface.domain_u();
                    let (v0, v1) = surface.domain_v();
                    let (ua, va, ub, vb) = match ei {
                        0 => (u0, v0, u0, v1),
                        1 => (u0, v1, u1, v1),
                        2 => (u1, v1, u1, v0),
                        _ => (u1, v0, u0, v0),
                    };
                    let p = surface.evaluate((ua + ub) * 0.5, (va + vb) * 0.5);
                    xform.transform_point3(p)
                })?;
                acc += mid;
                n += 1;
            }
            if n > 0 {
                return Some(acc / n as f32);
            }
        }
        if !self.nurbs_sel.selected_cvs.is_empty() {
            let fi = self.nurbs_active_face_index()?;
            let xform = self.nurbs_face_world_xform(solid, fi);
            let cvs: Vec<_> = self.nurbs_sel.selected_cvs.iter().copied().collect();
            let (sum, count) = self
                .nurbs_with_face_surface(solid, fi, |surface| {
                    let mut s = Vec3::ZERO;
                    let mut c = 0u32;
                    for (row, col) in cvs {
                        if let Some(cp) = surface
                            .control_points
                            .get(row as usize)
                            .and_then(|r| r.get(col as usize))
                        {
                            s += xform.transform_point3(cp.position);
                            c += 1;
                        }
                    }
                    (s, c)
                })
                .unwrap_or((Vec3::ZERO, 0));
            if count > 0 {
                return Some(sum / count as f32);
            }
        }
        None
    }

    /// Translate selected NURBS sub-elements by world delta (G gizmo / modal).
    /// Face moves **weld** shared CPs on neighbour faces so the solid stays closed (N3).
    pub(crate) fn nurbs_apply_translate(&mut self, delta: Vec3) {
        if delta.length_squared() < 1e-16 {
            return;
        }
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => return,
        };
        let n_faces = self.nurbs_face_count(solid);
        if n_faces == 0 {
            return;
        }

        // World-space delta → local (assume solid xform is affine with uniform scale-ish).
        let xform = self.scene.scene_graph.world_transform(solid);
        let inv = xform.inverse();
        let local_delta = inv.transform_vector3(delta);

        match self.nurbs_work_mode {
            NurbsWorkMode::Face | NurbsWorkMode::Object
                if !self.nurbs_sel.selected_faces.is_empty() =>
            {
                let sel: Vec<u32> = self
                    .nurbs_sel
                    .selected_faces
                    .iter()
                    .filter_map(|&id| self.nurbs_face_index(solid, id))
                    .collect();
                if sel.is_empty() {
                    return;
                }
                self.transaction("Move NURBS Face", |s| {
                    s.nurbs_translate_faces_closed(solid, &sel, local_delta);
                    s.nurbs_sync_mesh_cache(solid);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }
            NurbsWorkMode::Edge if !self.nurbs_sel.selected_edges.is_empty() => {
                let sel: Vec<(u32, u8)> = self
                    .nurbs_sel
                    .selected_edges
                    .iter()
                    .filter_map(|&(id, ei)| {
                        self.nurbs_face_index(solid, id).map(|fi| (fi, ei))
                    })
                    .collect();
                if sel.is_empty() {
                    return;
                }
                self.transaction("Move NURBS Edge", |s| {
                    s.nurbs_translate_edges_closed(solid, &sel, local_delta);
                    s.nurbs_sync_mesh_cache(solid);
                    s.mark_dirty(DirtyFlags::RENDER);
                });
            }
            NurbsWorkMode::Vertex => {
                self.nurbs_move_cvs(delta);
            }
            _ => {}
        }
    }

    /// Move selected faces and any CPs on other faces that shared their positions (weld).
    pub(crate) fn nurbs_translate_faces_closed(
        &mut self,
        solid: NodeId,
        selected: &[u32],
        local_delta: Vec3,
    ) {
        const EPS2: f32 = 1e-8;
        // 1) Snapshot seed positions from selected faces
        let mut seeds: Vec<Vec3> = Vec::new();
        for &fi in selected {
            self.nurbs_with_face_surface(solid, fi, |surface| {
                for row in &surface.control_points {
                    for cp in row {
                        seeds.push(cp.position);
                    }
                }
            });
        }
        if seeds.is_empty() {
            return;
        }
        // 2) Move every CP on every face that matches a seed (keeps seams closed)
        let n_faces = self.nurbs_face_count(solid);
        for fi in 0..n_faces {
            let fi = fi as u32;
            self.nurbs_with_face_surface_mut(solid, fi, |surface| {
                for row in &mut surface.control_points {
                    for cp in row.iter_mut() {
                        if seeds.iter().any(|s| (cp.position - *s).length_squared() < EPS2) {
                            cp.position += local_delta;
                        }
                    }
                }
            });
        }
    }

    /// Move edge CVs + weld to neighbour faces that share those positions.
    pub(crate) fn nurbs_translate_edges_closed(
        &mut self,
        solid: NodeId,
        selected: &[(u32, u8)],
        local_delta: Vec3,
    ) {
        const EPS2: f32 = 1e-8;
        let mut seeds: Vec<Vec3> = Vec::new();
        for &(fi, ei) in selected {
            self.nurbs_with_face_surface(solid, fi, |surface| {
                let (rows, cols) = surface.grid_size();
                if rows == 0 || cols == 0 {
                    return;
                }
                match ei {
                    0 => {
                        for c in 0..cols {
                            seeds.push(surface.control_points[0][c].position);
                        }
                    }
                    1 => {
                        for r in 0..rows {
                            seeds.push(surface.control_points[r][cols - 1].position);
                        }
                    }
                    2 => {
                        for c in 0..cols {
                            seeds.push(surface.control_points[rows - 1][c].position);
                        }
                    }
                    _ => {
                        for r in 0..rows {
                            seeds.push(surface.control_points[r][0].position);
                        }
                    }
                }
            });
        }
        if seeds.is_empty() {
            return;
        }
        let n_faces = self.nurbs_face_count(solid);
        for fi in 0..n_faces {
            let fi = fi as u32;
            self.nurbs_with_face_surface_mut(solid, fi, |surface| {
                for row in &mut surface.control_points {
                    for cp in row.iter_mut() {
                        if seeds.iter().any(|s| (cp.position - *s).length_squared() < EPS2) {
                            cp.position += local_delta;
                        }
                    }
                }
            });
        }
    }

    // ============================================================
    // CV manipulation
    // ============================================================

    /// Move selected CVs by a world-space delta (G gizmo / free drag).
    /// Welds matching CPs on neighbour faces so corners stay closed.
    pub(crate) fn nurbs_move_cvs(&mut self, delta: Vec3) {
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => return,
        };
        let fi = match self.nurbs_active_face_index() {
            Some(f) => f,
            None => return,
        };
        let cvs: Vec<(u32, u32)> = self.nurbs_sel.selected_cvs.iter().copied().collect();
        if cvs.is_empty() {
            return;
        }
        if delta.length_squared() < 1e-16 {
            return;
        }
        let xform = self.scene.scene_graph.world_transform(solid);
        let local_delta = xform.inverse().transform_vector3(delta);
        self.transaction("Move CVs", |s| {
            // Seeds = selected CV positions before move
            let mut seeds = Vec::new();
            s.nurbs_with_face_surface(solid, fi, |surface| {
                for &(row, col) in &cvs {
                    if let Some(cp) = surface
                        .control_points
                        .get(row as usize)
                        .and_then(|r| r.get(col as usize))
                    {
                        seeds.push(cp.position);
                    }
                }
            });
            const EPS2: f32 = 1e-8;
            let n_faces = s.nurbs_face_count(solid);
            for f in 0..n_faces {
                let f = f as u32;
                s.nurbs_with_face_surface_mut(solid, f, |surface| {
                    for row in &mut surface.control_points {
                        for cp in row.iter_mut() {
                            if seeds.iter().any(|p| (cp.position - *p).length_squared() < EPS2) {
                                cp.position += local_delta;
                            }
                        }
                    }
                });
            }
            s.nurbs_sync_mesh_cache(solid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }

    /// Adjust weight of selected CVs by a delta amount.
    pub(crate) fn nurbs_adjust_weight(&mut self, delta: f32) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let cvs: Vec<(u32, u32)> = self.nurbs_sel.selected_cvs.iter().copied().collect();
        if cvs.is_empty() { return; }
        if delta.abs() < 1e-12 { return; }
        // Decreasing weight when all selected CVs are already at the floor (0.01) is a no-op.
        let would_change = {
            match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::NurbsBody { surface, .. }) => {
                    cvs.iter().any(|&(row, col)| {
                        let r = row as usize;
                        let c = col as usize;
                        if r >= surface.control_points.len() || c >= surface.control_points[r].len() {
                            return false;
                        }
                        let w = surface.control_points[r][c].weight;
                        let new_w = (w + delta).max(0.01);
                        (new_w - w).abs() > 1e-12
                    })
                }
                _ => false,
            }
        };
        if !would_change { return; }
        self.transaction("Adjust CV Weight", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::NurbsBody { surface, .. } = &mut node.payload {
                    let surface = Arc::make_mut(surface);
                    for &(row, col) in &cvs {
                        let r = row as usize;
                        let c = col as usize;
                        if r < surface.control_points.len() && c < surface.control_points[r].len() {
                            let new_w = (surface.control_points[r][c].weight + delta).max(0.01);
                            surface.control_points[r][c].weight = new_w;
                        }
                    }
                }
            }

            s.nurbs_sync_mesh_cache(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }
    // ============================================================
    // Knot insertion
    // ============================================================

    /// Insert a knot in the U direction at the midpoint of the domain.
    /// Refuse-before-transaction when active node is not a NURBS body.
    pub(crate) fn nurbs_insert_knot_u(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let is_nurbs = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::NurbsBody { .. })
        );
        if !is_nurbs {
            self.toast("Insert Knot: select a NURBS surface");
            return;
        }
        self.transaction("Insert Knot U", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::NurbsBody { surface, .. } = &mut node.payload {
                    let surface = Arc::make_mut(surface);
                    let (u0, u1) = surface.domain_u();
                    let mid = (u0 + u1) * 0.5;
                    surface.insert_knot_u(mid);
                }
            }

            s.nurbs_sync_mesh_cache(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Knot inserted (U)");
        });
    }

    /// Insert a knot in the V direction at the midpoint of the domain.
    /// Refuse-before-transaction when active node is not a NURBS body.
    pub(crate) fn nurbs_insert_knot_v(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let is_nurbs = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::NurbsBody { .. })
        );
        if !is_nurbs {
            self.toast("Insert Knot: select a NURBS surface");
            return;
        }
        self.transaction("Insert Knot V", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::NurbsBody { surface, .. } = &mut node.payload {
                    let surface = Arc::make_mut(surface);
                    let (v0, v1) = surface.domain_v();
                    let mid = (v0 + v1) * 0.5;
                    surface.insert_knot_v(mid);
                }
            }

            s.nurbs_sync_mesh_cache(nid);
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast("Knot inserted (V)");
        });
    }
    // ============================================================
    // Keyboard-driven tool dispatch
    // ============================================================

    /// Handle a NURBS tool keyboard shortcut. Returns true if consumed.
    pub(crate) fn nurbs_handle_key(&mut self, key: egui::Key, ctrl: bool, shift: bool) -> bool {
        if self.modeling_context != ModelingContext::Nurbs { return false; }

        match key {
            // Tab — cycle work mode (Object → Vertex → Edge → Face)
            egui::Key::Tab => {
                let next = match self.nurbs_work_mode {
                    NurbsWorkMode::Object => NurbsWorkMode::Vertex,
                    NurbsWorkMode::Vertex => NurbsWorkMode::Edge,
                    NurbsWorkMode::Edge => NurbsWorkMode::Face,
                    NurbsWorkMode::Face => NurbsWorkMode::Object,
                };
                self.nurbs_switch_work_mode(next);
                true
            }
            // U — insert knot U
            egui::Key::U if !ctrl => {
                self.nurbs_insert_knot_u();
                true
            }
            // V — insert knot V (only in NURBS context, not grab/view conflicts)
            egui::Key::V if ctrl => {
                // Ctrl+V would be paste, skip
                false
            }
            egui::Key::V if !ctrl => {
                self.nurbs_insert_knot_v();
                true
            }
            // W — adjust weight up on selected CVs
            egui::Key::W if !ctrl && !shift => {
                self.nurbs_adjust_weight(0.1);
                true
            }
            // Shift+W — adjust weight down
            egui::Key::W if shift && !ctrl => {
                self.nurbs_adjust_weight(-0.1);
                true
            }
            // 1–4 quick modes (same spirit as poly). G/R/S are handled by the unified
            // gizmo arm path (like poly) — not a separate free CV grab tool.
            egui::Key::Num1 if !ctrl => { self.nurbs_switch_work_mode(NurbsWorkMode::Object); true }
            egui::Key::Num2 if !ctrl => { self.nurbs_switch_work_mode(NurbsWorkMode::Vertex); true }
            egui::Key::Num3 if !ctrl => { self.nurbs_switch_work_mode(NurbsWorkMode::Edge); true }
            egui::Key::Num4 if !ctrl => { self.nurbs_switch_work_mode(NurbsWorkMode::Face); true }
            _ => false,
        }
    }

    /// Sticky work-mode switch (Tab pie / 1–4). Sub-selection clears on mode change.
    pub(crate) fn nurbs_switch_work_mode(&mut self, mode: NurbsWorkMode) {
        if self.nurbs_work_mode == mode {
            return;
        }
        self.nurbs_work_mode = mode;
        self.nurbs_clear_selection();
        // Prefer selecting the solid root in object mode so G/R/S move the whole cube.
        if mode == NurbsWorkMode::Object {
            if let Some(root) = self.nurbs_solid_root() {
                self.select_after_create(&[SceneItemId::Node(root)]);
            }
        }
        let label = match mode {
            NurbsWorkMode::Object => "Object — whole solid · G/R/S",
            NurbsWorkMode::Vertex => "Vertex — CVs · G/R/S gizmo",
            NurbsWorkMode::Edge => "Edge — boundary · G/R/S · Ctrl+B bevel",
            NurbsWorkMode::Face => "Face — whole surface · G/R/S",
        };
        self.toast(label);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Dispatch NURBS tool keys from keybind flags (Phase 3 contextual keybinds).
    pub(crate) fn nurbs_handle_keys(&mut self, kf: &NurbsKeyFlags) {
        if self.modeling_context != ModelingContext::Nurbs { return; }
        if kf.knot_u { self.nurbs_insert_knot_u(); }
        if kf.knot_v { self.nurbs_insert_knot_v(); }
        if kf.weight_up { self.nurbs_adjust_weight(0.1); }
        if kf.weight_down { self.nurbs_adjust_weight(-0.1); }
        // grab_cv: arm translate gizmo (parity with poly G) when sub-selection exists
        if kf.grab_cv && self.nurbs_work_mode != NurbsWorkMode::Object
            && self.nurbs_sel.has_sub_selection()
        {
            self.tool.gizmo_mode = crate::GizmoMode::Translate;
            self.tool.gizmo_armed = true;
        }
    }
    // ============================================================
    // CV drag interactions
    // ============================================================

    /// Snapshot CVs for solid drag: `(face_idx, list of ((row,col), pos))` including weld.
    fn nurbs_snapshot_solid_weld(&self) -> Vec<(u32, Vec<((u32, u32), Vec3)>)> {
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let n_faces = self.nurbs_face_count(solid);
        if n_faces == 0 {
            return Vec::new();
        }
        const EPS2: f32 = 1e-8;
        // Seed positions from selection
        let mut seeds: Vec<Vec3> = Vec::new();
        match self.nurbs_work_mode {
            NurbsWorkMode::Face | NurbsWorkMode::Object => {
                for &fid in &self.nurbs_sel.selected_faces {
                    let Some(fi) = self.nurbs_face_index(solid, fid) else {
                        continue;
                    };
                    self.nurbs_with_face_surface(solid, fi, |surface| {
                        for row in &surface.control_points {
                            for cp in row {
                                seeds.push(cp.position);
                            }
                        }
                    });
                }
            }
            NurbsWorkMode::Edge => {
                for &(fid, ei) in &self.nurbs_sel.selected_edges {
                    let Some(fi) = self.nurbs_face_index(solid, fid) else {
                        continue;
                    };
                    self.nurbs_with_face_surface(solid, fi, |surface| {
                        let (rows, cols) = surface.grid_size();
                        if rows == 0 || cols == 0 {
                            return;
                        }
                        match ei {
                            0 => {
                                for c in 0..cols {
                                    seeds.push(surface.control_points[0][c].position);
                                }
                            }
                            1 => {
                                for r in 0..rows {
                                    seeds.push(surface.control_points[r][cols - 1].position);
                                }
                            }
                            2 => {
                                for c in 0..cols {
                                    seeds.push(surface.control_points[rows - 1][c].position);
                                }
                            }
                            _ => {
                                for r in 0..rows {
                                    seeds.push(surface.control_points[r][0].position);
                                }
                            }
                        }
                    });
                }
            }
            NurbsWorkMode::Vertex => {
                let fi = match self.nurbs_active_face_index() {
                    Some(f) => f,
                    None => return Vec::new(),
                };
                let cvs: Vec<_> = self.nurbs_sel.selected_cvs.iter().copied().collect();
                self.nurbs_with_face_surface(solid, fi, |surface| {
                    for (row, col) in cvs {
                        if let Some(cp) = surface
                            .control_points
                            .get(row as usize)
                            .and_then(|r| r.get(col as usize))
                        {
                            seeds.push(cp.position);
                        }
                    }
                });
            }
        }
        if seeds.is_empty() {
            return Vec::new();
        }
        // Every face: CPs matching seeds
        let mut out = Vec::new();
        for fi in 0..n_faces {
            let fi = fi as u32;
            let mut originals = Vec::new();
            self.nurbs_with_face_surface(solid, fi, |surface| {
                for (r, row) in surface.control_points.iter().enumerate() {
                    for (c, cp) in row.iter().enumerate() {
                        if seeds.iter().any(|s| (cp.position - *s).length_squared() < EPS2) {
                            originals.push(((r as u32, c as u32), cp.position));
                        }
                    }
                }
            });
            if !originals.is_empty() {
                out.push((fi, originals));
            }
        }
        out
    }

    /// Start NURBS G/R/S — same entry contract as `poly_start_transform`.
    pub(crate) fn nurbs_start_cv_drag(&mut self, mouse: egui::Pos2) {
        self.nurbs_start_transform(crate::TransformKind::Grab, mouse);
    }

    /// Gizmo axis drag entry (axis set by caller after start).
    pub(crate) fn nurbs_start_sub_drag(&mut self, mouse: egui::Pos2, _axis_lock: u8) {
        let kind = match self.tool.gizmo_mode {
            crate::GizmoMode::Translate => crate::TransformKind::Grab,
            crate::GizmoMode::Rotate => crate::TransformKind::Rotate,
            crate::GizmoMode::Scale => crate::TransformKind::Scale,
            crate::GizmoMode::Elongate => crate::TransformKind::Scale,
        };
        self.nurbs_start_transform(kind, mouse);
    }

    pub(crate) fn nurbs_start_transform(&mut self, kind: crate::TransformKind, mouse: egui::Pos2) {
        use crate::{AxisConstraint, TransformKind, TransformSpace};
        if matches!(kind, TransformKind::Elongate) {
            self.toast("Elongate is SDF-only — use Scale (S) on NURBS");
            return;
        }
        let solid = match self.nurbs_solid_root() {
            Some(s) => s,
            None => {
                self.toast("Select a NURBS solid first");
                return;
            }
        };
        let multi_face_idx = self.nurbs_snapshot_solid_weld();
        if multi_face_idx.is_empty() {
            self.toast("Select a face / edge / vertex first");
            return;
        }
        let center = self
            .nurbs_selection_center()
            .unwrap_or_else(|| self.resolve_action_frame().center);
        let frame = self.resolve_action_frame();
        let frozen_orient = if frame.use_orient {
            frame.orient
        } else {
            self.active_rotation_quat()
        };
        let frozen_space = if frame.use_orient {
            TransformSpace::Local
        } else {
            self.tool.transform_space
        };
        let label = match kind {
            TransformKind::Grab => "NURBS Grab",
            TransformKind::Rotate => "NURBS Rotate",
            TransformKind::Scale => "NURBS Scale",
            TransformKind::Elongate => "NURBS Scale",
        };
        self.begin_episode(label);
        self.start_operation(ActiveTool::NurbsCvMove(NurbsCvDrag {
            node_id: solid,
            multi_face_idx,
            start_mouse: mouse,
            kind,
            axis: AxisConstraint::None,
            center,
            free_mode: false,
            frozen_orient,
            frozen_space,
            confirm_on_release: false,
            num_buf: String::new(),
        }));
    }

    pub(crate) fn nurbs_transform_set_axis(&mut self, axis: crate::AxisConstraint) {
        if let Some(d) = self.tool.current.as_nurbs_cv_move_mut() {
            d.axis = axis;
        }
    }

    pub(crate) fn nurbs_transform_num_input(&mut self, ch: char) {
        if let Some(d) = self.tool.current.as_nurbs_cv_move_mut() {
            if ch == '\x08' {
                d.num_buf.pop();
            } else {
                d.num_buf.push(ch);
            }
        }
    }

    /// Update NURBS G/R/S via the **same** `compute_transform` as poly/SDF gizmo.
    pub(crate) fn nurbs_update_cv_drag(&mut self, cur: egui::Pos2, shift: bool, ctrl: bool) {
        use crate::{AxisConstraint, TransformKind};
        let (solid, start, multi_idx, kind, axis, center, free_mode, frozen_orient, frozen_space, num_buf) = {
            let d = match self.tool.current.as_nurbs_cv_move() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.start_mouse,
                d.multi_face_idx.clone(),
                d.kind,
                d.axis,
                d.center,
                d.free_mode,
                d.frozen_orient,
                d.frozen_space,
                d.num_buf.clone(),
            )
        };
        if multi_idx.is_empty() {
            return;
        }

        let world_xform = self.scene.scene_graph.world_transform(solid);
        let inv_xform = world_xform.inverse();
        let to_world = |p: Vec3| world_xform.transform_point3(p);
        let to_local = |p: Vec3| inv_xform.transform_point3(p);

        let cam = self.transform_camera();
        let settings = self.transform_settings();
        let num_val: Option<f32> = if num_buf.is_empty() {
            None
        } else {
            num_buf.parse().ok()
        };
        let snap_active = self.is_snap_active(ctrl);
        let snap_grid = self.effective_snap_grid();
        let snap_rot = self.viewport.snap_rotation_deg.max(0.0);
        let (snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize, snap_angle_lock) =
            self.snap_input_flags();
        let dx = cur.x - start.x;
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();

        let snap_pos = if snap_active && matches!(kind, TransformKind::Grab) && snap_affect_move {
            let rect = cam.viewport_rect;
            if rect.width() > 1.0 {
                let hit = self.find_smart_snap(cur, rect, None);
                self.viewport.smart_snap_result = hit.clone();
                hit.map(|h| h.world_pos)
            } else {
                None
            }
        } else {
            None
        };

        let grab_input = crate::transform::TransformInput {
            kind: TransformKind::Grab,
            axis,
            original_pos: center,
            original_rot: Vec3::ZERO,
            original_scale: Vec3::ONE,
            original_elongation: Vec3::ZERO,
            mouse_start: start,
            mouse_current: cur,
            numeric_value: num_val,
            snap_active,
            snap_grid,
            snap_rotation_deg: snap_rot,
            snap_rotation_active: shift,
            snap_pos,
            snap_affect_move,
            snap_affect_rotate,
            snap_affect_scale,
            snap_grid_quantize,
            snap_angle_lock,
            free_mode,
            elongate_face_negative: false,
            elongate_symmetric: false,
            space: frozen_space,
            object_rot: frozen_orient,
            precision_scale: 1.0,
        };
        let grab_result = crate::transform::compute_transform(&grab_input, &cam, &settings);
        let mut grab_delta = grab_result.position - center;
        if free_mode && shift && axis == AxisConstraint::None {
            let (ax, ay, az) = (grab_delta.x.abs(), grab_delta.y.abs(), grab_delta.z.abs());
            grab_delta = if ax >= ay && ax >= az {
                Vec3::new(grab_delta.x, 0.0, 0.0)
            } else if ay >= az {
                Vec3::new(0.0, grab_delta.y, 0.0)
            } else {
                Vec3::new(0.0, 0.0, grab_delta.z)
            };
        }

        let rot_quat = {
            let mut angle_deg = num_val.unwrap_or(dx * settings.rotate_speed);
            if (snap_active || shift) && snap_rot > 0.0 && num_val.is_none() {
                angle_deg = (angle_deg / snap_rot).round() * snap_rot;
            }
            let angle_rad = angle_deg.to_radians();
            let rot_axis = match axis {
                AxisConstraint::X
                | AxisConstraint::Y
                | AxisConstraint::Z
                | AxisConstraint::PlaneX
                | AxisConstraint::PlaneY
                | AxisConstraint::PlaneZ => {
                    crate::transform::constraint_axis_world(axis, frozen_space, frozen_orient)
                        .unwrap_or(Vec3::Y)
                }
                AxisConstraint::None => {
                    let ax = cam_fwd.x.abs();
                    let ay = cam_fwd.y.abs();
                    let az = cam_fwd.z.abs();
                    if ax >= ay && ax >= az {
                        Vec3::X * cam_fwd.x.signum()
                    } else if ay >= ax && ay >= az {
                        Vec3::Y * cam_fwd.y.signum()
                    } else {
                        Vec3::Z * cam_fwd.z.signum()
                    }
                }
            };
            glam::Quat::from_axis_angle(rot_axis, angle_rad)
        };

        let mut scale_factor =
            num_val.unwrap_or_else(|| (1.0 + dx * settings.scale_speed).max(0.01));
        if snap_active && num_val.is_none() {
            scale_factor = (scale_factor * 10.0).round() / 10.0;
            scale_factor = scale_factor.max(0.01);
        }
        // Shift + free axis: mouse-direction single-axis lock (poly/SDF scale parity).
        let scale_axis_world: Option<Vec3> = match axis {
            AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z => {
                crate::transform::constraint_axis_world(axis, frozen_space, frozen_orient)
            }
            AxisConstraint::None
                if shift
                    && matches!(kind, TransformKind::Scale | TransformKind::Elongate)
                    && num_val.is_none() =>
            {
                let lock = crate::transform::dominant_axis_constraint_from_mouse(
                    center,
                    start,
                    cur,
                    &cam,
                    frozen_space,
                    frozen_orient,
                );
                crate::transform::constraint_axis_world(lock, frozen_space, frozen_orient)
            }
            _ => None,
        };

        // WORLD map of one original LOCAL CP → new LOCAL (identical to poly map_vert).
        let map_cp = |orig_local: Vec3| -> Vec3 {
            let w0 = to_world(orig_local);
            let w1 = match kind {
                TransformKind::Grab => w0 + grab_delta,
                TransformKind::Rotate => center + rot_quat * (w0 - center),
                TransformKind::Scale | TransformKind::Elongate => {
                    let rel = w0 - center;
                    let scaled = if let Some(aw) = scale_axis_world {
                        let aw = aw.normalize_or_zero();
                        let along = rel.dot(aw) * aw;
                        let perp = rel - along;
                        along * scale_factor + perp
                    } else {
                        rel * scale_factor
                    };
                    center + scaled
                }
            };
            to_local(w1)
        };

        for (fi, origs) in &multi_idx {
            self.nurbs_with_face_surface_mut(solid, *fi, |surface| {
                for &((row, col), orig_pos) in origs {
                    let r = row as usize;
                    let c = col as usize;
                    if r < surface.control_points.len() && c < surface.control_points[r].len() {
                        surface.control_points[r][c].position = map_cp(orig_pos);
                    }
                }
            });
        }
        self.nurbs_sync_mesh_cache(solid);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
    }

    /// Confirm NURBS G/R/S (Space / Enter / LMB release with confirm_on_release).
    pub(crate) fn nurbs_confirm_cv_drag(&mut self) {
        let (solid, multi_idx) = match self.tool.current.as_nurbs_cv_move() {
            Some(d) => (d.node_id, d.multi_face_idx.clone()),
            None => return,
        };
        let mut moved = false;
        for (fi, origs) in &multi_idx {
            let hit = self.nurbs_with_face_surface(solid, *fi, |surface| {
                origs.iter().any(|&((row, col), orig)| {
                    surface
                        .control_points
                        .get(row as usize)
                        .and_then(|r| r.get(col as usize))
                        .map(|cp| (cp.position - orig).length_squared() > 1e-12)
                        .unwrap_or(false)
                })
            });
            if hit == Some(true) {
                moved = true;
                break;
            }
        }
        if !moved {
            self.cancel_episode();
            self.tool.current = ActiveTool::Idle;
            self.nurbs_tool = NurbsTool::Idle;
            self.set_gizmo_armed(false);
            return;
        }
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.nurbs_tool = NurbsTool::Idle;
        // Keep gizmo armed like poly after a handle drag so user can continue
        // (confirm_on_release free drag clears arm via caller).
    }

    /// Cancel NURBS G/R/S: restore originals.
    pub(crate) fn nurbs_cancel_cv_drag(&mut self) {
        if let Some(d) = self.tool.current.as_nurbs_cv_move() {
            let solid = d.node_id;
            for (fi, originals) in d.multi_face_idx.clone() {
                self.nurbs_with_face_surface_mut(solid, fi, |surface| {
                    for &((row, col), orig) in &originals {
                        let r = row as usize;
                        let c = col as usize;
                        if r < surface.control_points.len() && c < surface.control_points[r].len()
                        {
                            surface.control_points[r][c].position = orig;
                        }
                    }
                });
            }
            self.nurbs_sync_mesh_cache(solid);
            self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
        }
        if self.has_active_episode() {
            self.cancel_episode();
        } else if let Some(entry) = self.history.undo_stack.pop_back() {
            if let Some(sg) = entry.payload.scene_graph_snapshot() {
                self.scene.scene_graph = sg;
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.nurbs_tool = NurbsTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
