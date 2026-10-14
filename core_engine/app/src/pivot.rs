// core_engine/app/src/pivot.rs
// Custom pivot point editing — D key to enter, drag to reposition
// Ctrl = snap, Shift = orient to surface, Alt = orient only (keep position)
use eframe::egui;
use glam::{Vec3, Quat, EulerRot};
use core_math::scene_graph::BodyTransform;
use crate::{AzraApp, ActiveTool, DirtyFlags, PivotEditState, TransformSpace, world_to_screen};

/// When true, prim.position in the flat cache is body-local (relative to body.origin).
/// Pivot drag reads world-space from ray/camera and must convert back to local before writing.
use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    /// Enter pivot edit mode: D key / Edit Pivot menu.
    /// Poly mesh or selected node → moves `SceneNode.pivot` (geometry stays).
    /// SDF prim → moves prim position (legacy shape-center pivot).
    /// While live: drag the yellow marker, or G (grab) then move the mouse; Enter confirms, Esc cancels.
    pub(crate) fn enter_pivot_mode(&mut self) {
        // Prefer poly / scene node pivot when a PolygonalBody (or any node) is selected.
        if let Some(nid) = self.resolve_pivot_target_nid().or_else(|| self.resolve_poly_body_nid()) {
            let orig = self
                .selection_pivot_world()
                .or_else(|| self.node_pivot_world(nid))
                .unwrap_or(Vec3::ZERO);
            self.begin_episode("Move pivot");
            self.start_operation(ActiveTool::PivotEdit(PivotEditState {
                original_pivot: orig,
                original_rot: Vec3::ZERO,
                dragging: false,
                drag_start_mouse: egui::pos2(0.0, 0.0),
                node_id: Some(nid),
            }));
            self.ui.expose_selection_pivot = true;
            // Arm Translate so G is already live; user can also press G/R/S.
            self.tool.gizmo_mode = crate::GizmoMode::Translate;
            self.tool.gizmo_armed = true;
            self.toast(
                "Edit Pivot — drag yellow marker or G-grab · Ctrl=snap · Enter=confirm · Esc=cancel",
            );
            return;
        }
        let idx = match self.active_prim_index_unified() {
            Some(i) if i < self.scene.primitives.len() => i,
            _ => {
                self.toast("Select an object first");
                return;
            }
        };
        let orig_pos = self.scene.primitives[idx].position;
        let orig_rot = self.scene.primitives[idx].rotation_deg;
        // Undo BEFORE any mutation — captures pre-pivot state
        self.begin_episode("Move pivot");
        self.start_operation(ActiveTool::PivotEdit(PivotEditState {
            original_pivot: orig_pos,
            original_rot: orig_rot,
            dragging: false,
            drag_start_mouse: egui::pos2(0.0, 0.0),
            node_id: None,
        }));
        self.ui.expose_selection_pivot = true;
        self.tool.gizmo_mode = crate::GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.toast(
            "Edit Pivot — drag yellow marker or G-grab · Ctrl=snap · Enter=confirm · Esc=cancel",
        );
    }

    /// World-space pivot for one scene node from its `PivotConfig`.
    ///
    /// **BoundingCenter** = live geometry center (poly mesh AABB, SDF body bounds/centroid) —
    /// NOT `local_position` / object origin. That was the Object-mode R-still-uses-origin bug.
    /// **Custom** is stored in node-local space (`pivot.local`) so it follows G/R/S.
    pub(crate) fn node_pivot_world(&self, nid: core_math::scene_graph::NodeId) -> Option<Vec3> {
        use core_math::scene_graph::PivotMode;
        let node = self.scene.scene_graph.get(nid)?;
        Some(match node.pivot.mode {
            PivotMode::Custom => {
                if node.pivot.local {
                    self.scene
                        .scene_graph
                        .world_transform(nid)
                        .transform_point3(node.pivot.position)
                } else {
                    node.pivot.position
                }
            }
            PivotMode::WorldOrigin => Vec3::ZERO,
            PivotMode::BoundingCenter => self
                .node_geometry_center_world(nid)
                .unwrap_or(node.local_position),
        })
    }

    /// Write a Custom pivot from a world-space point (stored node-local so it follows the object).
    pub(crate) fn set_node_custom_pivot_world(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        world: Vec3,
    ) {
        use core_math::scene_graph::PivotMode;
        let local = self
            .scene
            .scene_graph
            .world_transform(nid)
            .inverse()
            .transform_point3(world);
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            node.pivot.position = local;
            node.pivot.mode = PivotMode::Custom;
            node.pivot.local = true;
        }
    }

    /// World-space center of the node's **geometry** (mesh AABB / body bounds), not the transform origin.
    pub(crate) fn node_geometry_center_world(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> Option<Vec3> {
        use core_math::scene_graph::NodePayload;
        // SDF volume bounds (already world).
        if let Some(&(bmin, bmax)) = self.cache.body_vol_cache.body_bounds.get(&nid) {
            return Some((bmin + bmax) * 0.5);
        }
        let node = self.scene.scene_graph.get(nid)?;
        match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                let local = Self::poly_mesh_local_center(mesh);
                let world = self
                    .scene
                    .scene_graph
                    .world_transform(nid)
                    .transform_point3(local);
                Some(world)
            }
            NodePayload::SdfBody { origin, .. } => Some(*origin),
            NodePayload::ImportedMesh { .. } | NodePayload::NurbsBody { .. } => {
                // Fall back to node origin until we have a mesh AABB cache for these.
                Some(node.local_position)
            }
            _ => Some(node.local_position),
        }
    }

    /// World-space pivot for the current selection (node pivot config, else prim/body origin).
    pub(crate) fn selection_pivot_world(&self) -> Option<Vec3> {
        // Prefer explicit node pivot when set
        if let Some(nid) = self.resolve_pivot_target_nid() {
            return self.node_pivot_world(nid);
        }
        // Active prim position (world)
        if let Some(idx) = self.active_prim_index_unified() {
            if idx < self.scene.primitives.len() {
                let p = &self.scene.primitives[idx];
                if BODY_LOCAL_PRIMS {
                    if let Some(&(nid, _)) = self.scene.prim_body_map.get(idx) {
                        if let Some(bt) = self.scene.scene_graph.body_world_transform(nid) {
                            return Some(bt.local_to_world(p.position));
                        }
                    }
                }
                return Some(p.position);
            }
        }
        None
    }

    /// Draw the selection pivot as a yellow disc + cross in the viewport.
    pub(crate) fn draw_selection_pivot_marker(&self, painter: &egui::Painter, rect: egui::Rect) {
        let show = self.ui.expose_selection_pivot
            || self.ui.pivot_center_menu_open
            || self.tool.current.is_pivot_edit();
        if !show {
            return;
        }
        let Some(world) = self.selection_pivot_world() else {
            return;
        };
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let Some(sp) = world_to_screen(
            world,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        ) else {
            return;
        };
        let col = egui::Color32::from_rgb(0xFF, 0xC1, 0x07);
        let r = 6.0;
        painter.circle_filled(sp, r + 3.0, egui::Color32::from_black_alpha(90));
        painter.circle_filled(sp, r, col);
        painter.circle_stroke(sp, r, egui::Stroke::new(1.0, egui::Color32::from_rgb(0x40, 0x30, 0x00)));
        let arm = 9.0;
        let stroke = egui::Stroke::new(1.5, col);
        painter.line_segment(
            [egui::pos2(sp.x - arm, sp.y), egui::pos2(sp.x + arm, sp.y)],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(sp.x, sp.y - arm), egui::pos2(sp.x, sp.y + arm)],
            stroke,
        );
    }

    /// Cancel pivot edit: discard undo entry and restore original position.
    pub(crate) fn cancel_pivot_mode(&mut self) {
        let pe = self.tool.current.as_pivot_edit().cloned();
        if let Some(pe) = pe {
            if let Some(nid) = pe.node_id {
                self.pop_last_undo();
                self.set_node_custom_pivot_world(nid, pe.original_pivot);
                self.tool.gizmo_armed = false;
                self.tool.current = ActiveTool::Idle;
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast("Pivot edit cancelled");
                return;
            }
            self.pop_last_undo();
            if let Some(idx) = self.active_prim_index_unified() {
                if idx < self.scene.primitives.len() {
                    let (orig_p, orig_r) = (pe.original_pivot, pe.original_rot);
                    self.write_prim(idx, |p| {
                        p.position = orig_p;
                        p.rotation_deg = orig_r;
                    });
                    self.rebake_no_undo();
                }
            }
        }
        self.tool.gizmo_armed = false;
        self.tool.current = ActiveTool::Idle;
    }

    /// Confirm pivot edit: keep the new position.
    /// Node pivot: geometry already stayed put. SDF prim: shape center moves with pivot.
    pub(crate) fn confirm_pivot_mode(&mut self) {
        if let Some(pe) = self.tool.current.as_pivot_edit().cloned() {
            if pe.node_id.is_some() {
                // Live edits already wrote `node.pivot`; undo episode captures the change.
                self.tool.gizmo_armed = false;
                self.tool.current = ActiveTool::Idle;
                self.ui.expose_selection_pivot = true;
                self.toast("Pivot moved");
                self.mark_dirty(DirtyFlags::RENDER);
                return;
            }
            if let Some(idx) = self.active_prim_index_unified() {
                if idx < self.scene.primitives.len() {
                    let prim = &self.scene.primitives[idx];
                    let old_pos = pe.original_pivot;
                    let new_pos = prim.position;
                    let delta = new_pos - old_pos;

                    // Undo was saved at enter_pivot_mode. If pivot didn't move, discard it.
                    if delta.length_squared() > 1e-8 {
                        self.rebake_no_undo();
                        self.toast("Pivot moved");
                    } else {
                        self.pop_last_undo();
                    }
                }
            }
        }
        self.tool.gizmo_armed = false;
        self.tool.current = ActiveTool::Idle;
    }

    /// Update pivot drag: move the pivot to follow the mouse.
    /// `ctrl` = snap to smart snap candidates.
    /// `shift` = orient (SDF prim only).
    pub(crate) fn update_pivot_drag(&mut self, mouse: egui::Pos2, rect: egui::Rect, ctrl: bool, shift: bool) {
        let pe = match self.tool.current.as_pivot_edit() {
            Some(pe) => pe.clone(),
            None => return,
        };

        // ── Node pivot (poly mesh / body): pe.original_pivot is WORLD ──
        if let Some(nid) = pe.node_id {
            let cam_pos = self.viewport.camera.target
                + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
            let pivot_world = pe.original_pivot;
            let (ray_o, ray_d) = self.cam_ray(mouse, rect);
            let plane_n = cam_fwd;
            let denom = ray_d.dot(plane_n);
            if denom.abs() < 1e-6 {
                return;
            }
            // Plane through original pivot facing camera
            let t = (pivot_world - ray_o).dot(plane_n) / denom;
            if t < 0.0 {
                return;
            }
            let mut new_pos_world = ray_o + ray_d * t;
            if ctrl {
                if let Some(hit) = self.find_smart_snap(mouse, rect, None) {
                    new_pos_world = hit.world_pos;
                }
            }
            self.set_node_custom_pivot_world(nid, new_pos_world);
            self.tool.action_automatic_center = Some(new_pos_world);
            self.mark_dirty(DirtyFlags::RENDER);
            let _ = cam_pos; // silence if unused on some builds
            return;
        }

        let idx = match self.active_prim_index_unified() {
            Some(i) if i < self.scene.primitives.len() => i,
            _ => return,
        };
        // Ray-plane intersection: move pivot on the plane through original_pivot, facing camera
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();

        // With BODY_LOCAL_PRIMS, pe.original_pivot is body-local. Convert to world-space
        // so the drag plane is positioned correctly relative to the camera.
        let body_transform: Option<BodyTransform> = if BODY_LOCAL_PRIMS {
            self.scene
                .prim_body_map
                .get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
        } else {
            None
        };
        let pivot_world = if BODY_LOCAL_PRIMS {
            if let Some(bt) = body_transform {
                bt.local_to_world(pe.original_pivot)
            } else {
                pe.original_pivot
            }
        } else {
            pe.original_pivot
        };

        let (ray_o, ray_d) = self.cam_ray(mouse, rect);
        let plane_n = cam_fwd;
        let plane_d = cam_fwd.dot(pivot_world - cam_pos);
        let denom = ray_d.dot(plane_n);
        if denom.abs() < 1e-6 {
            return;
        }
        let t = (cam_pos + plane_n * plane_d - ray_o).dot(plane_n) / denom;
        if t < 0.0 {
            return;
        }
        let mut new_pos_world = ray_o + ray_d * t;

        // Ctrl = snap to smart snap candidates (result is world-space)
        if ctrl {
            if let Some(hit) = self.find_smart_snap(mouse, rect, Some(idx)) {
                new_pos_world = hit.world_pos;
            }
        }

        // With BODY_LOCAL_PRIMS, convert world-space result back to body-local before writing.
        let new_pos = if BODY_LOCAL_PRIMS {
            if let Some(bt) = body_transform {
                bt.world_to_local(new_pos_world)
            } else {
                new_pos_world
            }
        } else {
            new_pos_world
        };

        self.write_prim(idx, |p| p.position = new_pos);

        // Shift = orient gizmo to match surface normal under mouse
        if shift {
            let (origin, dir) = self.cam_ray(mouse, rect);
            if let Some((_hit_p, hit_n)) = self.raycast_scene(origin, dir) {
                // Orient the primitive so its local Y aligns with the surface normal
                let up = hit_n.normalize();
                let fwd = if up.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
                let right = fwd.cross(up).normalize();
                let fwd = up.cross(right).normalize();
                let mat = glam::Mat3::from_cols(right, up, fwd);
                let quat = Quat::from_mat3(&mat);
                let (yaw, pitch, roll) = quat.to_euler(EulerRot::YXZ);
                let rot = Vec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees());
                self.write_prim(idx, |p| p.rotation_deg = rot);
            }
        }

        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Live label for the ctx-bar pivot pill (from the active node's pivot mode).
    pub(crate) fn ui2_pivot_pill_label(&self) -> &'static str {
        use core_math::scene_graph::PivotMode;
        let nid = self
            .resolve_pivot_target_nid()
            .or_else(|| self.resolve_poly_body_nid());
        let mode = nid
            .and_then(|n| self.scene.scene_graph.get(n))
            .map(|node| node.pivot.mode);
        match mode {
            Some(PivotMode::BoundingCenter) => "Bounding Center",
            Some(PivotMode::WorldOrigin) => "World Origin",
            Some(PivotMode::Custom) => "Custom",
            None => "Bounding Center",
        }
    }

    /// Live label for the ctx-bar space pill.
    pub(crate) fn ui2_space_pill_label(&self) -> &'static str {
        match self.tool.transform_space {
            TransformSpace::Global => "Global",
            TransformSpace::Local => "Local",
        }
    }

    // ================================================================
    // D-key pie menu pivot placement (SceneNode.pivot)
    // ================================================================

    /// Get whether the active node's pivot is persistent.
    pub(crate) fn get_active_pivot_persistent(&self) -> bool {
        let nid = if self.sel.usel.has_nodes() {
            self.sel.usel.selected_nodes().next()
        } else {
            // Prim selected → get its body
            self.selected_prim_indices_unified().first()
                .and_then(|&i| self.scene.prim_body_map.get(i).map(|&(n, _)| n))
        };
        nid.and_then(|n| self.scene.scene_graph.get(n))
            .map(|node| node.pivot.persistent)
            .unwrap_or(true)
    }

    /// Toggle persistent/temporary for active node's pivot.
    pub(crate) fn toggle_pivot_persistent(&mut self) {
        let nid = self.resolve_pivot_target_nid();
        if let Some(nid) = nid {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.pivot.persistent = !node.pivot.persistent;
                let label = if node.pivot.persistent { "Persistent" } else { "Temporary" };
                self.toast(&format!("Pivot: {}", label));
            }
        }
    }

    /// Apply a pivot placement from the D-key / yellow-dot center pie.
    ///
    /// Slots (center_pie_def / d_pie_def):
    /// 0 Mass · 1 World Origin · 2 Workplane · 3 Object · 4 Surface · 5 Drag · 6 Local/World
    pub(crate) fn apply_pivot_from_pie(&mut self, idx: usize, mouse: egui::Pos2, rect: egui::Rect) {
        use core_math::scene_graph::PivotMode;

        // Local / Global toggle — no nid required for gizmo space
        if idx == 6 {
            self.tool.transform_space = match self.tool.transform_space {
                TransformSpace::Global => TransformSpace::Local,
                TransformSpace::Local => TransformSpace::Global,
            };
            let label = match self.tool.transform_space {
                TransformSpace::Global => "Global",
                TransformSpace::Local => "Local",
            };
            self.toast(&format!("Gizmo space: {label}"));
            self.ui.expose_selection_pivot = true;
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }

        // Free drag — enter pivot edit, pick up the yellow point and move it
        if idx == 5 {
            self.enter_pivot_mode();
            return;
        }

        let nid = match self.resolve_pivot_target_nid() {
            Some(n) => n,
            None => {
                self.toast("Select an object first");
                return;
            }
        };

        let (new_pos, new_mode, label) = match idx {
            0 => {
                // Mass / bounds center
                let center = if let Some(&(bmin, bmax)) =
                    self.cache.body_vol_cache.body_bounds.get(&nid)
                {
                    (bmin + bmax) * 0.5
                } else {
                    self.scene
                        .scene_graph
                        .body_origin(nid)
                        .unwrap_or(Vec3::ZERO)
                };
                (center, PivotMode::BoundingCenter, "Mass / bounds")
            }
            1 => (Vec3::ZERO, PivotMode::WorldOrigin, "World origin"),
            2 => {
                let wp = self.viewport.wp_resolved.origin;
                (wp, PivotMode::Custom, "Workplane")
            }
            3 => {
                // Active object / prim origin
                let pos = self
                    .selection_pivot_world()
                    .or_else(|| self.scene.scene_graph.body_origin(nid))
                    .unwrap_or(Vec3::ZERO);
                (pos, PivotMode::Custom, "Object")
            }
            4 => {
                if let Some(pos) = self.raycast_pivot_surface(mouse, rect) {
                    (pos, PivotMode::Custom, "Surface")
                } else {
                    self.toast("No surface under cursor");
                    return;
                }
            }
            _ => return,
        };

        self.transaction("Set Pivot", |s| {
            if new_mode == PivotMode::Custom {
                s.set_node_custom_pivot_world(nid, new_pos);
            } else if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                node.pivot.position = new_pos;
                node.pivot.mode = new_mode;
            }
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.ui.expose_selection_pivot = true;
        self.toast(&format!("Pivot: {label}"));
    }

    /// Resolve which NodeId the pivot action should target.
    pub(crate) fn resolve_pivot_target_nid(&self) -> Option<core_math::scene_graph::NodeId> {
        if self.sel.usel.has_nodes() {
            self.sel.usel.selected_nodes().next()
        } else {
            self.selected_prim_indices_unified().first()
                .and_then(|&i| self.scene.prim_body_map.get(i).map(|&(n, _)| n))
        }
    }

    /// Active / first selected PolygonalBody node (object-mode pivot ops).
    pub(crate) fn resolve_poly_body_nid(&self) -> Option<core_math::scene_graph::NodeId> {
        use core_math::scene_graph::NodePayload;
        let is_poly = |nid: core_math::scene_graph::NodeId| {
            self.scene
                .scene_graph
                .get(nid)
                .map(|n| matches!(n.payload, NodePayload::PolygonalBody { .. }))
                .unwrap_or(false)
        };
        if let Some(nid) = self.sel.usel.active_node().filter(|&n| is_poly(n)) {
            return Some(nid);
        }
        self.sel.usel.selected_nodes().find(|&n| is_poly(n))
    }

    /// Local-space AABB center of a poly body's base mesh.
    fn poly_mesh_local_center(mesh: &core_math::half_edge::HalfEdgeMesh) -> Vec3 {
        use core_math::half_edge::VertexId;
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        let mut any = false;
        for (i, v) in mesh.vertices.iter().enumerate() {
            if !mesh.is_vert_live(VertexId(i as u32)) {
                continue;
            }
            any = true;
            min = min.min(v.position);
            max = max.max(v.position);
        }
        if !any {
            return Vec3::ZERO;
        }
        (min + max) * 0.5
    }

    /// Pivot → center of mesh (geometry stays put in world; pivot moves).
    pub(crate) fn poly_pivot_to_center_mesh(&mut self) {
        use core_math::scene_graph::NodePayload;
        let Some(nid) = self.resolve_poly_body_nid() else {
            self.toast("Select a poly object first");
            return;
        };
        let center_local = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => Self::poly_mesh_local_center(mesh),
                _ => return,
            }
        };
        let world = self
            .scene
            .scene_graph
            .world_transform(nid)
            .transform_point3(center_local);
        self.transaction("Pivot → Center Mesh", |s| {
            s.set_node_custom_pivot_world(nid, world);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.ui.expose_selection_pivot = true;
        self.toast("Pivot → center of mesh");
    }

    /// Pivot → current face / edge / vertex selection (world centroid).
    ///
    /// Geometry stays put; only the object's pivot moves. Used from the Ctrl+RMB
    /// viewport context menu in Vertex / Edge / Face (and Group) mode.
    pub(crate) fn poly_pivot_to_selected(&mut self) {
        let Some(nid) = self.resolve_poly_body_nid() else {
            self.toast("Select a poly object first");
            return;
        };
        // Prefer the active component domain; if empty (e.g. Object mode with leftover
        // selection sets), fall back to any non-empty domain.
        let (world, domain) = if let Some(c) = self.poly_selection_center() {
            let d = match self.poly_work_mode {
                crate::PolyWorkMode::Vertex => "vertex",
                crate::PolyWorkMode::Edge => "edge",
                crate::PolyWorkMode::Face => "face",
                crate::PolyWorkMode::Group => "group",
                _ => "selection",
            };
            (c, d)
        } else if let Some((c, d)) = self.poly_selection_center_any_domain() {
            (c, d)
        } else {
            let what = match self.poly_work_mode {
                crate::PolyWorkMode::Vertex => "vertices",
                crate::PolyWorkMode::Edge => "edges",
                crate::PolyWorkMode::Face | crate::PolyWorkMode::Group => "faces",
                _ => "faces, edges, or vertices",
            };
            self.toast(&format!("Pivot to Selected: select one or more {what} first"));
            return;
        };
        self.transaction("Pivot → Selected", |s| {
            s.set_node_custom_pivot_world(nid, world);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        // Park Action Center Automatic place so G/R/S can use this point immediately
        // when Action Center is Automatic (Shift+RMB place slot).
        self.tool.action_automatic_center = Some(world);
        self.ui.expose_selection_pivot = true;
        self.toast(&format!("Pivot → selected {domain}"));
    }

    /// World centroid of any non-empty poly selection set (verts / edges / faces), ignoring
    /// the current work mode. Used when Object-mode menu offers Pivot to Selected.
    fn poly_selection_center_any_domain(&self) -> Option<(glam::Vec3, &'static str)> {
        use core_math::half_edge::{FaceId, HalfEdgeId, VertexId};
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node().or_else(|| self.resolve_poly_body_nid())?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform = |p: glam::Vec3| world.transform_point3(p);

        if !self.poly_sel.selected_vertices.is_empty() {
            let mut sum = glam::Vec3::ZERO;
            let mut n = 0u32;
            for &vid in &self.poly_sel.selected_vertices {
                if (vid as usize) >= mesh.vertices.len() {
                    continue;
                }
                sum += xform(mesh.vert(VertexId(vid)).position);
                n += 1;
            }
            if n > 0 {
                return Some((sum / n as f32, "vertex"));
            }
        }
        if !self.poly_sel.selected_edges.is_empty() {
            let mut sum = glam::Vec3::ZERO;
            let mut n = 0u32;
            for &eid in &self.poly_sel.selected_edges {
                if (eid as usize) >= mesh.half_edges.len() {
                    continue;
                }
                let he = mesh.he(HalfEdgeId(eid));
                let src = mesh.he_source(HalfEdgeId(eid));
                if (src.0 as usize) >= mesh.vertices.len()
                    || (he.vertex.0 as usize) >= mesh.vertices.len()
                {
                    continue;
                }
                sum += xform(mesh.vert(src).position);
                sum += xform(mesh.vert(he.vertex).position);
                n += 2;
            }
            if n > 0 {
                return Some((sum / n as f32, "edge"));
            }
        }
        if !self.poly_sel.selected_faces.is_empty() {
            let mut sum = glam::Vec3::ZERO;
            let mut n = 0u32;
            for &fid in &self.poly_sel.selected_faces {
                if (fid as usize) >= mesh.faces.len() {
                    continue;
                }
                for vid in mesh.face_vertices(FaceId(fid)) {
                    if (vid.0 as usize) >= mesh.vertices.len() {
                        continue;
                    }
                    sum += xform(mesh.vert(vid).position);
                    n += 1;
                }
            }
            if n > 0 {
                return Some((sum / n as f32, "face"));
            }
        }
        None
    }

    /// Mesh → pivot (verts shift so mesh center sits on the current pivot; pivot stays).
    pub(crate) fn poly_mesh_to_pivot(&mut self) {
        use core_math::scene_graph::NodePayload;
        let Some(nid) = self.resolve_poly_body_nid() else {
            self.toast("Select a poly object first");
            return;
        };
        let pivot_world = self
            .selection_pivot_world()
            .or_else(|| self.node_pivot_world(nid))
            .unwrap_or(Vec3::ZERO);
        let inv = self.scene.scene_graph.world_transform(nid).inverse();
        let pivot_local = inv.transform_point3(pivot_world);
        self.transaction("Mesh → Pivot", |s| {
            let center = {
                let Some(node) = s.scene.scene_graph.get(nid) else {
                    return;
                };
                match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Self::poly_mesh_local_center(mesh),
                    _ => return,
                }
            };
            let delta = pivot_local - center;
            if delta.length_squared() < 1e-12 {
                return;
            }
            s.poly_with_base_mesh_mut(nid, |mesh| {
                use core_math::half_edge::VertexId;
                for i in 0..mesh.vertices.len() {
                    if mesh.is_vert_live(VertexId(i as u32)) {
                        mesh.vertices[i].position += delta;
                    }
                }
            });
            s.poly_sync_after_position_edit(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
        });
        self.toast("Mesh → pivot");
    }

    /// Mesh → world origin: center verts at local 0, node position → world (0,0,0).
    pub(crate) fn poly_mesh_to_origin(&mut self) {
        use core_math::scene_graph::{NodePayload, PivotMode};
        let Some(nid) = self.resolve_poly_body_nid() else {
            self.toast("Select a poly object first");
            return;
        };
        self.transaction("Mesh → Origin", |s| {
            let center = {
                let Some(node) = s.scene.scene_graph.get(nid) else {
                    return;
                };
                match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Self::poly_mesh_local_center(mesh),
                    _ => return,
                }
            };
            if center.length_squared() > 1e-12 {
                s.poly_with_base_mesh_mut(nid, |mesh| {
                    use core_math::half_edge::VertexId;
                    for i in 0..mesh.vertices.len() {
                        if mesh.is_vert_live(VertexId(i as u32)) {
                            mesh.vertices[i].position -= center;
                        }
                    }
                });
                s.poly_sync_after_position_edit(nid);
            }
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                node.local_position = Vec3::ZERO;
                node.pivot.position = Vec3::ZERO;
                node.pivot.mode = PivotMode::WorldOrigin;
            }
            s.scene.scene_graph.invalidate_world_xform_cache();
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
        });
        self.ui.expose_selection_pivot = true;
        self.toast("Mesh → origin (0, 0, 0)");
    }

    /// Raycast to find SDF surface hit for pivot placement.
    fn raycast_pivot_surface(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<Vec3> {
        let (origin, dir) = self.cam_ray(mouse, rect);
        self.raycast_scene(origin, dir).map(|(hit_p, _)| hit_p)
    }

    /// Compute AABB half-extents for a body node (used for corner/edge pivot placement).
    fn compute_node_aabb_half_extents(&self, nid: core_math::scene_graph::NodeId) -> Vec3 {
        // Use cached body_bounds if available, otherwise estimate from prims
        if let Some(&(bmin, bmax)) = self.cache.body_vol_cache.body_bounds.get(&nid) {
            let origin = self.scene.scene_graph.body_origin(nid).unwrap_or(Vec3::ZERO);
            return (bmax - bmin) * 0.5;
        }
        // Fallback: estimate from prim bounding radii
        let prims = self.scene.scene_graph.body_primitives(nid).unwrap_or(&[]);
        if prims.is_empty() { return Vec3::splat(0.5); }
        let mut aabb_min = Vec3::splat(f32::MAX);
        let mut aabb_max = Vec3::splat(f32::MIN);
        for p in prims {
            let he = p.bounding_aabb_half_extents();
            aabb_min = aabb_min.min(p.position - he);
            aabb_max = aabb_max.max(p.position + he);
        }
        (aabb_max - aabb_min) * 0.5
    }
}

#[cfg(test)]
mod custom_pivot_follow_tests {
    use super::*;
    use crate::{ActionCenter, AzraApp, SceneItemId};
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::scene_graph::{NodePayload, PivotMode};
    use glam::Vec3;
    use std::sync::Arc;

    fn app_with_box() -> (AzraApp, core_math::scene_graph::NodeId) {
        let mut app = AzraApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "box".into(),
                mesh: Arc::new(HalfEdgeMesh::new_box(Vec3::ONE)),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(SceneItemId::Node(nid));
        app.tool.action_center = ActionCenter::Pivot;
        (app, nid)
    }

    #[test]
    fn custom_face_pivot_follows_object_move() {
        let (mut app, nid) = app_with_box();
        let face_world = Vec3::new(1.0, 0.0, 0.0);
        app.set_node_custom_pivot_world(nid, face_world);
        let before = app.node_pivot_world(nid).unwrap();
        assert!((before - face_world).length() < 1e-4);

        // Move the object +2 on X (object-mode grab).
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            n.local_position.x += 2.0;
        }
        app.scene.scene_graph.invalidate_world_xform_cache();

        let after = app.node_pivot_world(nid).unwrap();
        assert!(
            (after - (face_world + Vec3::X * 2.0)).length() < 1e-4,
            "custom pivot must follow the object, got {after:?} want {:?}",
            face_world + Vec3::X * 2.0
        );
        // Action Center Pivot must report the same point (T-pie gizmo).
        assert!((app.resolve_action_frame().center - after).length() < 1e-4);
    }

    #[test]
    fn legacy_world_custom_pivot_migrates_and_follows() {
        let (mut app, nid) = app_with_box();
        // Old file: Custom position stored as world, local=false.
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            n.pivot.position = Vec3::new(0.0, 1.0, 0.0);
            n.pivot.mode = PivotMode::Custom;
            n.pivot.local = false;
        }
        app.scene.scene_graph.migrate_custom_pivots_to_local();
        if let Some(n) = app.scene.scene_graph.get(nid) {
            assert!(n.pivot.local);
        }
        if let Some(n) = app.scene.scene_graph.get_mut(nid) {
            n.local_position.z += 3.0;
        }
        app.scene.scene_graph.invalidate_world_xform_cache();
        let p = app.node_pivot_world(nid).unwrap();
        assert!(
            (p - Vec3::new(0.0, 1.0, 3.0)).length() < 1e-4,
            "migrated pivot must follow, got {p:?}"
        );
    }
}
