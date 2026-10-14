//! Action Center (Modo-style) — resolve transform **center + axes** for G/R/S and the gizmo.
//!
//! Chosen via the **T** pie. Orthogonal to body pivot placement (D pie).
//! All G/R/S paths should use [`AzraApp::resolve_action_frame`] instead of ad-hoc medians.

use eframe::egui;
use glam::{Mat3, Quat, Vec3};
use crate::{ActionCenter, AzraApp, TransformSpace};

/// Resolved action frame for the current selection + Action Center mode.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActionFrame {
    /// World-space pivot for G/R/S / gizmo.
    pub center: Vec3,
    /// World orientation of tool handles (local +X/+Y/+Z → world). Identity = world axes.
    pub orient: Quat,
    /// Whether axes should follow `orient` as Local (true) or pure Global world axes (false).
    pub use_orient: bool,
}

impl AzraApp {
    /// Single source of truth for gizmo pivot + axis orientation under the active Action Center.
    pub(crate) fn resolve_action_frame(&self) -> ActionFrame {
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
        let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
        let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize_or_zero();
        let obj_orient = self.active_rotation_quat();
        let sel_center = self.selection_median();
        let mode = self.tool.action_center;

        match mode {
            ActionCenter::Origin => ActionFrame {
                center: Vec3::ZERO,
                orient: Quat::IDENTITY,
                use_orient: false,
            },
            ActionCenter::Screen => {
                // Axes follow camera every frame (MMB orbit / angle changes update gizmo).
                // Center stays on selection (or Automatic place), NOT frozen to orbit target.
                let m = Mat3::from_cols(cam_right, cam_up, -cam_fwd);
                let orient = Quat::from_mat3(&m).normalize();
                let center = self
                    .tool
                    .action_automatic_center
                    .unwrap_or(sel_center);
                ActionFrame {
                    center,
                    orient,
                    use_orient: true,
                }
            }
            ActionCenter::Pivot => ActionFrame {
                center: self.selection_median_pivot(),
                orient: obj_orient,
                use_orient: self.tool.transform_space == TransformSpace::Local,
            },
            ActionCenter::Element => {
                // Live selection / hover first so the gizmo tracks the chosen face/edge/vert
                // (owner: "Element orientation broken — must follow the chosen element").
                if let Some((c, o)) = self.poly_element_action_frame() {
                    ActionFrame {
                        center: c,
                        orient: o,
                        use_orient: true,
                    }
                } else if let Some((c, o)) = self.tool.action_element {
                    ActionFrame {
                        center: c,
                        orient: o,
                        use_orient: true,
                    }
                } else {
                    self.resolve_action_frame_selection(sel_center, obj_orient)
                }
            }
            ActionCenter::Workplane => {
                let wp = &self.viewport.wp_resolved;
                let y = wp.normal.normalize_or_zero();
                let mut x = wp.tangent_u.normalize_or_zero();
                if y.length_squared() < 0.25 {
                    ActionFrame {
                        center: wp.origin,
                        orient: Quat::IDENTITY,
                        use_orient: false,
                    }
                } else {
                    // Re-orthonormalize: Y = WP normal, X = U on plane, Z = X×Y.
                    x = (x - y * x.dot(y)).normalize_or_zero();
                    if x.length_squared() < 0.25 {
                        let a = if y.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
                        x = (a - y * a.dot(y)).normalize_or_zero();
                    }
                    let z = x.cross(y).normalize_or_zero();
                    let m = Mat3::from_cols(x, y, z);
                    ActionFrame {
                        center: wp.origin,
                        orient: Quat::from_mat3(&m).normalize(),
                        use_orient: true,
                    }
                }
            }
            ActionCenter::Selection => self.resolve_action_frame_selection(sel_center, obj_orient),
            ActionCenter::SelectionCenterAutoAxis => {
                let axis = match self.tool.action_auto_axis {
                    0 => Vec3::X,
                    2 => Vec3::Z,
                    _ => Vec3::Y,
                };
                // Build a frame with +Y = chosen major axis (or +X if axis is X).
                let orient = if axis.abs_diff_eq(Vec3::Y, 1e-4) {
                    Quat::IDENTITY
                } else {
                    Quat::from_rotation_arc(Vec3::Y, axis)
                };
                ActionFrame {
                    center: sel_center,
                    orient,
                    use_orient: true,
                }
            }
            ActionCenter::Automatic => {
                // Center: temporary Shift+RMB place while GRS is armed, else selection center.
                // Place is cleared when GRS is deactivated (`set_gizmo_armed(false)`).
                // Axes: Global / Local / Work Plane (same as None).
                let center = self.tool.action_automatic_center.unwrap_or(sel_center);
                let (orient, use_orient) = self.automatic_none_axes(obj_orient);
                ActionFrame {
                    center,
                    orient,
                    use_orient,
                }
            }
            ActionCenter::None => {
                // Center always tracks selection; no click-to-redefine.
                let (orient, use_orient) = self.automatic_none_axes(obj_orient);
                ActionFrame {
                    center: sel_center,
                    orient,
                    use_orient,
                }
            }
        }
    }

    /// Shared Global / Local / Work-Plane axis policy for Automatic and None.
    fn automatic_none_axes(&self, obj_orient: Quat) -> (Quat, bool) {
        match self.tool.transform_space {
            TransformSpace::Global => {
                // Prefer workplane axes when WP is locked (Modo Automatic + WP).
                if self.viewport.wp_mode == crate::WorkPlaneMode::Locked {
                    let n = self.viewport.wp_resolved.normal.normalize_or_zero();
                    if n.length_squared() > 0.25 {
                        (Quat::from_rotation_arc(Vec3::Y, n), true)
                    } else {
                        (Quat::IDENTITY, false)
                    }
                } else {
                    (Quat::IDENTITY, false)
                }
            }
            TransformSpace::Local => (obj_orient, true),
        }
    }

    fn resolve_action_frame_selection(&self, fallback_center: Vec3, obj_orient: Quat) -> ActionFrame {
        // Poly edit: center + average normal from selection.
        if let Some((c, n)) = self.poly_selection_place_frame() {
            let orient = if n.length_squared() > 0.25 {
                Quat::from_rotation_arc(Vec3::Y, n.normalize())
            } else {
                obj_orient
            };
            return ActionFrame {
                center: c,
                orient,
                use_orient: true,
            };
        }
        ActionFrame {
            center: fallback_center,
            orient: obj_orient,
            use_orient: self.tool.transform_space == TransformSpace::Local,
        }
    }

    /// Item pivot center (body pivot / custom / bounding) for ActionCenter::Pivot.
    fn selection_median_pivot(&self) -> Vec3 {
        // Prefer active node's pivot, else fall back to geometry median.
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(p) = self.node_pivot_world(nid) {
                return p;
            }
        }
        self.selection_median()
    }

    /// Apply T-pie selection (inner slice index).
    pub(crate) fn apply_action_center_from_pie(&mut self, idx: usize) {
        use ActionCenter::*;
        let mode = match idx {
            0 => Automatic,
            1 => Selection,
            2 => Workplane,
            3 => Element,
            4 => Screen,
            5 => Origin,
            6 => Pivot,
            7 => None,
            _ => return,
        };
        self.tool.action_center = mode;
        // Automatic place: keep while GRS is armed (Shift+RMB → G → R still uses the place).
        // Cleared when GRS deactivates (`set_gizmo_armed(false)`) so default is selection center.
        // Also clear on explicit Action Center "None".
        // Note: `None` here is ActionCenter::None (use ActionCenter::*).
        if mode == None {
            self.tool.action_automatic_center = Option::None;
        }
        // Element: seed frame from current selection immediately so axes match the face/edge.
        if mode == Element {
            if let Some((c, o)) = self.poly_element_action_frame() {
                self.tool.action_element = Some((c, o));
            }
        }
        let placed = self.tool.action_automatic_center.is_some();
        let extra = if mode == Automatic {
            if placed {
                " · placed pivot kept · Shift+RMB re-places (face/edge snap)"
            } else {
                " · Shift+RMB place pivot · snaps face/edge centers"
            }
        } else if mode == Element {
            " · follows face/edge/vert · Shift+RMB also picks"
        } else if mode == Workplane {
            " · WP origin + axes"
        } else {
            ""
        };
        self.toast(&format!("Action Center: {}{extra}", mode.label()));
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Right-handed frame: +Y = primary (face normal / edge-up), +X = secondary (edge dir).
    fn orient_from_y_and_x_hint(y_axis: Vec3, x_hint: Vec3) -> Quat {
        let y = y_axis.normalize_or_zero();
        if y.length_squared() < 0.25 {
            return Quat::IDENTITY;
        }
        let mut x = x_hint - y * x_hint.dot(y);
        if x.length_squared() < 1e-8 {
            let a = if y.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
            x = a - y * a.dot(y);
        }
        let x = x.normalize_or_zero();
        if x.length_squared() < 0.25 {
            return Quat::from_rotation_arc(Vec3::Y, y);
        }
        let z = x.cross(y).normalize_or_zero();
        let x = y.cross(z).normalize_or_zero(); // re-ortho
        Quat::from_mat3(&Mat3::from_cols(x, y, z)).normalize()
    }

    /// Live Element frame from poly selection or hover: proper axes, not only a free Y-up arc.
    /// Face: +Y = normal, +X = longest edge on the face. Edge: +Y ≈ avg normal, +X = edge dir.
    fn poly_element_action_frame(&self) -> Option<(Vec3, Quat)> {
        use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE, NO_FACE, NO_TWIN};
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform_p = |p: Vec3| world.transform_point3(p);
        let xform_v = |v: Vec3| world.transform_vector3(v).normalize_or_zero();

        // Prefer explicit selection over hover (user chose it).
        match self.poly_work_mode {
            crate::PolyWorkMode::Face if !self.poly_sel.selected_faces.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut edge_hint = Vec3::ZERO;
                let mut count = 0u32;
                for &fid in &self.poly_sel.selected_faces {
                    let f = FaceId(fid);
                    if (fid as usize) >= mesh.faces.len() || mesh.face(f).half_edge == NO_EDGE {
                        continue;
                    }
                    let vs = mesh.face_vertices(f);
                    if vs.len() < 3 {
                        continue;
                    }
                    let c: Vec3 =
                        vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                    let n = mesh.compute_face_normal(f);
                    if n.length_squared() < 1e-10 {
                        continue;
                    }
                    c_sum += c;
                    n_sum += n.normalize();
                    // Longest edge on this face → X hint in face plane.
                    let mut best_len = 0.0f32;
                    let mut best_e = Vec3::ZERO;
                    for i in 0..vs.len() {
                        let a = mesh.vert(vs[i]).position;
                        let b = mesh.vert(vs[(i + 1) % vs.len()]).position;
                        let e = b - a;
                        let l = e.length();
                        if l > best_len {
                            best_len = l;
                            best_e = e;
                        }
                    }
                    edge_hint += best_e;
                    count += 1;
                }
                if count == 0 {
                    return None;
                }
                let c = xform_p(c_sum / count as f32);
                let n = xform_v(n_sum);
                let xh = xform_v(edge_hint);
                let orient = Self::orient_from_y_and_x_hint(n, xh);
                return Some((c, orient));
            }
            crate::PolyWorkMode::Edge if !self.poly_sel.selected_edges.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut dir_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut count = 0u32;
                for &eid in &self.poly_sel.selected_edges {
                    let he = HalfEdgeId(eid);
                    if !mesh.is_edge_live(he) {
                        continue;
                    }
                    let a = mesh.he_source(he);
                    let b = mesh.he(he).vertex;
                    let pa = mesh.vert(a).position;
                    let pb = mesh.vert(b).position;
                    c_sum += (pa + pb) * 0.5;
                    let d = pb - pa;
                    // Align edge directions so they don't cancel.
                    let d = if count > 0 && d.dot(dir_sum) < 0.0 { -d } else { d };
                    dir_sum += d;
                    let f0 = mesh.he(he).face;
                    if f0 != NO_FACE && (f0.0 as usize) < mesh.faces.len() {
                        let n = mesh.compute_face_normal(f0);
                        if n.length_squared() > 1e-10 {
                            n_sum += n.normalize();
                        }
                    }
                    let tw = mesh.he(he).twin;
                    if tw != NO_TWIN {
                        let f1 = mesh.he(tw).face;
                        if f1 != NO_FACE && (f1.0 as usize) < mesh.faces.len() {
                            let n = mesh.compute_face_normal(f1);
                            if n.length_squared() > 1e-10 {
                                n_sum += n.normalize();
                            }
                        }
                    }
                    count += 1;
                }
                if count == 0 {
                    return None;
                }
                let c = xform_p(c_sum / count as f32);
                let edge_dir = xform_v(dir_sum);
                let n = xform_v(n_sum);
                // +X along edge, +Y = face-average normal (perp to edge when possible).
                let y = if n.length_squared() > 0.25 {
                    (n - edge_dir * n.dot(edge_dir)).normalize_or_zero()
                } else {
                    Vec3::Y
                };
                let y = if y.length_squared() > 0.25 { y } else { Vec3::Y };
                let orient = Self::orient_from_y_and_x_hint(y, edge_dir);
                return Some((c, orient));
            }
            crate::PolyWorkMode::Vertex if !self.poly_sel.selected_vertices.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut count = 0u32;
                for &vid in &self.poly_sel.selected_vertices {
                    if (vid as usize) >= mesh.vertices.len() {
                        continue;
                    }
                    let v = VertexId(vid);
                    c_sum += mesh.vert(v).position;
                    // Average incident face normals.
                    for i in 0..mesh.faces.len() as u32 {
                        if mesh.face(FaceId(i)).half_edge == NO_EDGE {
                            continue;
                        }
                        if mesh.face_vertices(FaceId(i)).contains(&v) {
                            let n = mesh.compute_face_normal(FaceId(i));
                            if n.length_squared() > 1e-10 {
                                n_sum += n.normalize();
                            }
                        }
                    }
                    count += 1;
                }
                if count == 0 {
                    return None;
                }
                let c = xform_p(c_sum / count as f32);
                let n = xform_v(n_sum);
                let orient = Self::orient_from_y_and_x_hint(n, Vec3::X);
                return Some((c, orient));
            }
            _ => {}
        }
        // Hover fallback (same rules, single element).
        if let Some((c, n)) = self.poly_hover_place_frame() {
            // When hovering an edge, poly_hover only returns avg normal — re-fetch edge dir if possible.
            if let Some(eid) = self.poly_sel.hovered_edge {
                let he = HalfEdgeId(eid);
                if mesh.is_edge_live(he) {
                    let a = mesh.vert(mesh.he_source(he)).position;
                    let b = mesh.vert(mesh.he(he).vertex).position;
                    let edge_dir = xform_v(b - a);
                    let y = (n - edge_dir * n.dot(edge_dir)).normalize_or_zero();
                    let y = if y.length_squared() > 0.25 { y } else { n };
                    return Some((c, Self::orient_from_y_and_x_hint(y, edge_dir)));
                }
            }
            if let Some(fid) = self.poly_sel.hovered_face {
                let f = FaceId(fid);
                if (fid as usize) < mesh.faces.len() && mesh.face(f).half_edge != NO_EDGE {
                    let vs = mesh.face_vertices(f);
                    let mut best_e = Vec3::X;
                    let mut best_l = 0.0f32;
                    for i in 0..vs.len() {
                        let a = mesh.vert(vs[i]).position;
                        let b = mesh.vert(vs[(i + 1) % vs.len()]).position;
                        let e = b - a;
                        if e.length() > best_l {
                            best_l = e.length();
                            best_e = e;
                        }
                    }
                    return Some((c, Self::orient_from_y_and_x_hint(n, xform_v(best_e))));
                }
            }
            return Some((c, Self::orient_from_y_and_x_hint(n, Vec3::X)));
        }
        None
    }

    /// Automatic Action Center: temporarily place the G/R/S pivot at the click hit.
    ///
    /// Bound to **Shift+RMB** (contextual). Prefer face-center / edge-center when the cursor is
    /// near them (always — not only when the magnet is on). Then smart-snap when magnet/Ctrl.
    /// Returns true if the pivot was set.
    pub(crate) fn action_center_place_automatic(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
    ) -> bool {
        if self.tool.action_center != ActionCenter::Automatic {
            return false;
        }

        // 1) Face center / edge mid / vert when cursor is close (primary UX for Automatic place).
        if let Some((p, _n, label)) = self.pick_poly_element_near_mouse(mouse, rect) {
            self.tool.action_automatic_center = Some(p);
            self.toast(&format!("Action Center pivot → {label}"));
            self.mark_dirty(crate::DirtyFlags::RENDER);
            return true;
        }

        // 2) Smart snap (magnet / Enable Snap / Ctrl) — SDF box centers, grid, etc.
        if self.is_snap_active(ctrl) {
            if let Some(hit) = self.find_smart_snap(mouse, rect, None) {
                self.tool.action_automatic_center = Some(hit.world_pos);
                self.toast(&format!("Action Center pivot → {}", hit.label));
                self.mark_dirty(crate::DirtyFlags::RENDER);
                return true;
            }
        }

        // 3) Surface hit; if near a poly element center, snap there too.
        let (origin, dir) = self.cam_ray(mouse, rect);
        let (p, how) = if let Some((hit, _n)) = self.raycast_scene_surface(origin, dir) {
            if let Some((sp, _, label)) = self.snap_world_point_to_nearby_poly_element(hit, mouse, rect) {
                self.tool.action_automatic_center = Some(sp);
                self.toast(&format!("Action Center pivot → {label}"));
                self.mark_dirty(crate::DirtyFlags::RENDER);
                return true;
            }
            (hit, "surface")
        } else if let Some(wp) = self.ray_workplane_hit(mouse, rect) {
            (wp, "workplane")
        } else {
            let plane_pt = self.selection_median();
            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
            match self.ray_plane_hit(mouse, rect, plane_pt, cam_fwd) {
                Some(hit) => (hit, "view plane"),
                None => return false,
            }
        };

        self.tool.action_automatic_center = Some(p);
        self.toast(&format!("Action Center pivot → {how}"));
        self.mark_dirty(crate::DirtyFlags::RENDER);
        true
    }

    /// Element Action Center: Shift+RMB picks the target face/edge/vert under the cursor
    /// (does not change mesh selection — sets action frame only).
    pub(crate) fn action_center_place_element(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> bool {
        if self.tool.action_center != ActionCenter::Element {
            return false;
        }
        // Prefer live hover (edit mode), else nearest poly element under cursor.
        if let Some((c, n)) = self.poly_hover_place_frame() {
            self.action_center_set_element(c, n);
            self.toast("Action Center: Element (hover)");
            return true;
        }
        if let Some((c, n, label)) = self.pick_poly_element_near_mouse(mouse, rect) {
            self.action_center_set_element(c, n);
            self.toast(&format!("Action Center: Element → {label}"));
            return true;
        }
        // Surface hit as last resort (SDF / no poly topology under cursor).
        let (origin, dir) = self.cam_ray(mouse, rect);
        if let Some((hit, n)) = self.raycast_scene_surface(origin, dir) {
            self.action_center_set_element(hit, n);
            self.toast("Action Center: Element → surface");
            return true;
        }
        self.toast("Element: hover a face/edge/vert, then Shift+RMB");
        false
    }

    /// Screen-space pick of nearest poly face center / edge mid / vertex (active poly body).
    /// Threshold uses smart-snap pixel radius (slightly larger) so "close to them" feels natural.
    /// Shared by Action Center Automatic place **and** workplane Shift/Home lock (edge mid).
    pub(crate) fn pick_poly_element_near_mouse(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<(Vec3, Vec3, &'static str)> {
        use core_math::half_edge::{FaceId, HalfEdgeId, NO_FACE, NO_TWIN};
        use core_math::scene_graph::NodePayload;
        use crate::world_to_screen_ex;

        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform_p = |p: Vec3| world.transform_point3(p);
        let xform_n = |n: Vec3| world.transform_vector3(n).normalize_or_zero();

        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        // Slightly roomier than transform snap so face/edge centers catch easily.
        let threshold = self.viewport.smart_snap_threshold.max(14.0) * 1.35;

        let mut best: Option<(f32, Vec3, Vec3, &'static str)> = None;
        let mut consider = |world_p: Vec3, world_n: Vec3, label: &'static str| {
            let Some(sp) = world_to_screen_ex(
                world_p,
                cam_pos,
                self.viewport.camera.orbit,
                rect,
                aspect,
                ortho_hw,
                self.render.fov_deg,
            ) else {
                return;
            };
            let d = ((sp.x - mouse.x).powi(2) + (sp.y - mouse.y).powi(2)).sqrt();
            if d > threshold {
                return;
            }
            if best.as_ref().map_or(true, |b| d < b.0) {
                best = Some((d, world_p, world_n, label));
            }
        };

        // Face centers (skip free face slots — half_edge == NO_EDGE)
        use core_math::half_edge::NO_EDGE;
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
            if mesh.faces[fi].half_edge == NO_EDGE {
                continue;
            }
            let vs = mesh.face_vertices(f);
            if vs.len() < 3 {
                continue;
            }
            let c: Vec3 =
                vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            let n = mesh.compute_face_normal(f);
            if n.length_squared() > 1e-10 {
                consider(xform_p(c), xform_n(n), "face center");
            }
        }
        // Edge midpoints (unique undirected)
        for i in 0..mesh.half_edges.len() {
            let he = HalfEdgeId(i as u32);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let twin = mesh.he(he).twin;
            if twin != NO_TWIN && twin.0 < i as u32 {
                continue;
            }
            let a = mesh.he_source(he);
            let b = mesh.he(he).vertex;
            let mid = (mesh.vert(a).position + mesh.vert(b).position) * 0.5;
            let mut n_sum = Vec3::ZERO;
            let f0 = mesh.he(he).face;
            if f0 != NO_FACE && (f0.0 as usize) < mesh.faces.len() {
                let n = mesh.compute_face_normal(f0);
                if n.length_squared() > 1e-10 {
                    n_sum += n.normalize();
                }
            }
            if twin != NO_TWIN && (twin.0 as usize) < mesh.half_edges.len() {
                let f1 = mesh.he(twin).face;
                if f1 != NO_FACE && (f1.0 as usize) < mesh.faces.len() {
                    let n = mesh.compute_face_normal(f1);
                    if n.length_squared() > 1e-10 {
                        n_sum += n.normalize();
                    }
                }
            }
            let n = xform_n(n_sum);
            consider(
                xform_p(mid),
                if n.length_squared() > 0.25 {
                    n
                } else {
                    Vec3::Y
                },
                "edge center",
            );
        }
        // Vertices (lower priority unless closer)
        for vi in 0..mesh.vertices.len() {
            let p = mesh.vertices[vi].position;
            consider(xform_p(p), Vec3::Y, "vertex");
        }

        best.map(|(_, p, n, lab)| (p, n, lab))
    }

    /// If a world hit is near a poly face/edge center in screen space, snap to it.
    pub(crate) fn snap_world_point_to_nearby_poly_element(
        &self,
        _hit: Vec3,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<(Vec3, Vec3, &'static str)> {
        // Same screen-space pick — distance is to mouse, not world hit.
        self.pick_poly_element_near_mouse(mouse, rect)
    }

    /// Cycle auto-axis X/Y/Z when Action Center is SelectionCenterAutoAxis (optional hotkey helper).
    pub(crate) fn action_center_cycle_auto_axis(&mut self) {
        self.tool.action_auto_axis = (self.tool.action_auto_axis + 1) % 3;
        let name = match self.tool.action_auto_axis {
            0 => "X",
            2 => "Z",
            _ => "Y",
        };
        self.toast(&format!("Auto Axis: {name}"));
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Set Element action frame from a world point + normal (face/edge/vert pick).
    /// Does not change mesh selection — only the transform frame.
    /// Uses a stable X hint so axes don't free-spin around the normal.
    pub(crate) fn action_center_set_element(&mut self, point: Vec3, normal: Vec3) {
        let orient = Self::orient_from_y_and_x_hint(normal, Vec3::X);
        self.tool.action_element = Some((point, orient));
        self.tool.action_center = ActionCenter::Element;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Element pick with explicit edge direction (world) for +X axis.
    pub(crate) fn action_center_set_element_with_x(
        &mut self,
        point: Vec3,
        y_axis: Vec3,
        x_hint: Vec3,
    ) {
        let orient = Self::orient_from_y_and_x_hint(y_axis, x_hint);
        self.tool.action_element = Some((point, orient));
        self.tool.action_center = ActionCenter::Element;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// If Action Center is Element, try to set the frame from current poly hover
    /// (does not change selection — Modo Element behavior).
    pub(crate) fn action_center_try_element_pick(&mut self) -> bool {
        if self.tool.action_center != ActionCenter::Element {
            return false;
        }
        if self.modeling_context != crate::ModelingContext::Polygonal
            || self.poly_work_mode == crate::PolyWorkMode::Object
        {
            return false;
        }
        if let Some((c, n)) = self.poly_selection_place_frame() {
            // Prefer hover when available for "click different element than selection".
            if let Some((c2, n2)) = self.poly_hover_place_frame() {
                self.action_center_set_element(c2, n2);
            } else {
                self.action_center_set_element(c, n);
            }
            return true;
        }
        if let Some((c, n)) = self.poly_hover_place_frame() {
            self.action_center_set_element(c, n);
            return true;
        }
        false
    }

    /// World center + normal from hovered poly element (vert/edge/face), if any.
    fn poly_hover_place_frame(&self) -> Option<(Vec3, Vec3)> {
        use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_FACE, NO_TWIN};
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform_p = |p: Vec3| world.transform_point3(p);
        let xform_n = |n: Vec3| world.transform_vector3(n).normalize_or_zero();

        if let Some(fid) = self.poly_sel.hovered_face {
            let f = FaceId(fid);
            if (fid as usize) < mesh.faces.len() {
                let vs = mesh.face_vertices(f);
                if vs.len() >= 3 {
                    let c: Vec3 =
                        vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                    let n = mesh.compute_face_normal(f);
                    if n.length_squared() > 1e-10 {
                        return Some((xform_p(c), xform_n(n)));
                    }
                }
            }
        }
        if let Some(eid) = self.poly_sel.hovered_edge {
            let he = HalfEdgeId(eid);
            if mesh.is_edge_live(he) {
                let a = mesh.he_source(he);
                let b = mesh.he(he).vertex;
                let mid = (mesh.vert(a).position + mesh.vert(b).position) * 0.5;
                let mut n_sum = Vec3::ZERO;
                let f0 = mesh.he(he).face;
                if f0 != NO_FACE && (f0.0 as usize) < mesh.faces.len() {
                    let n = mesh.compute_face_normal(f0);
                    if n.length_squared() > 1e-10 {
                        n_sum += n.normalize();
                    }
                }
                let tw = mesh.he(he).twin;
                if tw != NO_TWIN && (tw.0 as usize) < mesh.half_edges.len() {
                    let f1 = mesh.he(tw).face;
                    if f1 != NO_FACE && (f1.0 as usize) < mesh.faces.len() {
                        let n = mesh.compute_face_normal(f1);
                        if n.length_squared() > 1e-10 {
                            n_sum += n.normalize();
                        }
                    }
                }
                let n = xform_n(n_sum);
                return Some((
                    xform_p(mid),
                    if n.length_squared() > 0.25 { n } else { Vec3::Y },
                ));
            }
        }
        if let Some(vid) = self.poly_sel.hovered_vertex {
            if (vid as usize) < mesh.vertices.len() {
                let p = mesh.vert(VertexId(vid)).position;
                return Some((xform_p(p), Vec3::Y));
            }
        }
        None
    }

    /// Orientation quat for gizmo draw / Local transform under Action Center.
    pub(crate) fn action_frame_orient(&self) -> Quat {
        let f = self.resolve_action_frame();
        if f.use_orient {
            f.orient
        } else if self.tool.transform_space == TransformSpace::Local {
            self.active_rotation_quat()
        } else {
            // Global: world axes — do not stick to object rotation.
            Quat::IDENTITY
        }
    }

    /// Transform space effective under Action Center (Local when custom orient is active).
    pub(crate) fn action_frame_space(&self) -> TransformSpace {
        let f = self.resolve_action_frame();
        if f.use_orient {
            TransformSpace::Local
        } else {
            // Still honour Global/Local toggle for None/Automatic world axes.
            self.tool.transform_space
        }
    }
}
