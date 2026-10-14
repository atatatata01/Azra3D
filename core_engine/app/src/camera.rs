//! Camera, raycasting, and viewport geometry — moved from main.rs.

use eframe::egui;
use glam::{Quat, EulerRot, Vec3};
use core_math::primitives::{scene_sdf, SdfShape};
use core_math::scene_graph::BodyTransform;
use crate::*;

/// When true, prim.position in the flat cache is body-local (relative to body.origin).
/// Box-face picking must convert to world-space before ray intersection.
use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    /// Orientation used for Local-space gizmo axes (and Local transform math).
    /// Includes **body** rotation when a body is selected — prim-only euler ignored body
    /// transform, so Local axes looked broken after rotating a body.
    pub(crate) fn active_rotation_quat(&self) -> Quat {
        if let Some(i) = self.active_prim_index_unified() {
            if i < self.scene.primitives.len() && self.sel.usel.has_prims() {
                return self.prim_world_orient(i);
            }
        }
        use core_math::scene_graph::NodePayload;
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                match &node.payload {
                    NodePayload::SdfBody { body_rotation, .. } => return *body_rotation,
                    _ => {
                        let r = node.local_rotation * (std::f32::consts::PI / 180.0);
                        return Quat::from_euler(EulerRot::YXZ, r.y, r.x, r.z);
                    }
                }
            }
        }
        Quat::IDENTITY
    }

    /// World orientation of a prim: body_rotation * prim.rotation_quat (or prim alone).
    pub(crate) fn prim_world_orient(&self, flat_idx: usize) -> Quat {
        if flat_idx >= self.scene.primitives.len() {
            return Quat::IDENTITY;
        }
        let prim_q = self.scene.primitives[flat_idx].rotation_quat();
        if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(flat_idx) {
            if let Some(bt) = self.scene.scene_graph.body_world_transform(body_nid) {
                return (bt.rotation * prim_q).normalize();
            }
        }
        prim_q
    }

    /// Orientation to draw/hit-test the gizmo: frozen during an active gizmo or modal G/R/S
    /// drag so arrows stay locked to the drag math (Blender-style), live otherwise.
    /// When idle, follows Action Center (T pie) via [`AzraApp::action_frame_orient`].
    pub(crate) fn gizmo_draw_orient(&self) -> Quat {
        if let Some(gi) = self.tool.current.as_gizmo() {
            return gi.orient;
        }
        if let Some(ts) = self.tool.current.as_transform() {
            return ts.object_orient;
        }
        if let Some(ts) = self.tool.current.as_poly_transform() {
            return ts.frozen_orient;
        }
        self.action_frame_orient()
    }

    /// Transform space for gizmo draw/hit during drag (frozen), else Action Center space.
    pub(crate) fn gizmo_draw_space(&self) -> TransformSpace {
        if let Some(gi) = self.tool.current.as_gizmo() {
            return gi.space;
        }
        if let Some(ts) = self.tool.current.as_transform() {
            return ts.space;
        }
        if let Some(ts) = self.tool.current.as_poly_transform() {
            return ts.frozen_space;
        }
        self.action_frame_space()
    }

    pub(crate) fn cam_ray(&self, mouse:egui::Pos2, rect:egui::Rect) -> (Vec3, Vec3) {
        let w=rect.width().max(64.0); let h=rect.height().max(64.0);
        let uv_x=((mouse.x-rect.min.x)/w)*2.0-1.0;
        let uv_y=-(((mouse.y-rect.min.y)/h)*2.0-1.0);
        let cam_pos=self.viewport.camera.target+self.viewport.camera.orbit*Vec3::new(0.0,0.0,self.viewport.camera.distance);
        let fwd=(self.viewport.camera.orbit*Vec3::NEG_Z).normalize();
        let right=(self.viewport.camera.orbit*Vec3::X).normalize();
        let up=(self.viewport.camera.orbit*Vec3::Y).normalize();
        let aspect = (w / h).clamp(0.1, 10.0);
        if self.render.ortho {
            let ortho_hw = self.viewport.camera.distance;
            let origin = cam_pos + right * (uv_x * ortho_hw * aspect) + up * (uv_y * ortho_hw);
            (origin, fwd)
        } else {
            // tan(fov/2) matches the GPU perspective (Mat4::perspective_rh) + world_to_screen; hardcoding
            // 90° here skewed object-mode picking / pivot / surface-snap off the cursor at other FOVs.
            let tan_half = (self.render.fov_deg.to_radians() * 0.5).tan();
            let rd=(fwd+right*(uv_x*aspect*tan_half)+up*(uv_y*tan_half)).normalize();
            (cam_pos, rd)
        }
    }

    /// CPU raymarch against the analytical SDF scene — returns (hit_point, normal)
    pub(crate) fn raycast_scene(&self, origin:Vec3, dir:Vec3) -> Option<(Vec3, Vec3)> {
        let max_dist = (self.viewport.camera.distance * self.tweak.ray_dist_mult).max(self.tweak.ray_max_dist);
        let mut t = 0.0f32;
        for _ in 0..self.tweak.ray_max_steps {
            let p = origin + dir * t;
            let d = self.scene_distance(p);
            if d < self.tweak.ray_hit_threshold {
                let e = self.tweak.ray_normal_eps;
                let n = Vec3::new(
                    self.scene_distance(p+Vec3::new(e,0.0,0.0))-self.scene_distance(p-Vec3::new(e,0.0,0.0)),
                    self.scene_distance(p+Vec3::new(0.0,e,0.0))-self.scene_distance(p-Vec3::new(0.0,e,0.0)),
                    self.scene_distance(p+Vec3::new(0.0,0.0,e))-self.scene_distance(p-Vec3::new(0.0,0.0,e)),
                ).normalize();
                return Some((p, n));
            }
            t += d.max(0.01);
            if t > max_dist { break; }
        }
        None
    }

    /// Raycast against ALL cached triangle meshes (poly, imported, voxel, NURBS, and SDF
    /// DC meshes) in world space. Returns the nearest hit point + surface normal (facing the
    /// ray). Complements `raycast_scene`, which only marches the analytical SDF prim field —
    /// so mesh-only bodies (a polygon box, an imported/voxel object) are invisible to it.
    /// Reuses `poly_selection::ray_triangle` — no new intersection math.
    /// Returns `(hit_point, world_normal, node)` — the NodeId lets object-mode click-select
    /// resolve a mesh body on the SAME frame instead of waiting for the async GPU pick.
    pub(crate) fn raycast_scene_meshes(&self, origin: Vec3, dir: Vec3) -> Option<(Vec3, Vec3, core_math::scene_graph::NodeId)> {
        let mut best: Option<(f32, Vec3, core_math::scene_graph::NodeId)> = None; // (t, world_normal, node)
        for (nid, mid) in self.scene.scene_graph.renderable_meshes() {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => continue };
            if !node.visible { continue; }
            let cached = match self.cache.mesh_cache.get(mid) { Some(c) => c, None => continue };
            let m = &cached.cpu_mesh;
            if m.vertices.is_empty() || m.quads.is_empty() { continue; }
            let world = self.scene.scene_graph.world_transform(nid);
            let xf = |v: Vec3| -> Vec3 {
                let w = world * glam::Vec4::new(v.x, v.y, v.z, 1.0);
                Vec3::new(w.x, w.y, w.z)
            };
            let nv = m.vertices.len();
            for q in &m.quads {
                for [a, b, c] in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                    let (a, b, c) = (a as usize, b as usize, c as usize);
                    if a >= nv || b >= nv || c >= nv { continue; }
                    let (v0, v1, v2) = (xf(m.vertices[a]), xf(m.vertices[b]), xf(m.vertices[c]));
                    if let Some(t) = crate::poly_selection::ray_triangle(origin, dir, v0, v1, v2) {
                        if best.map_or(true, |(bt, _, _)| t < bt) {
                            let mut n = (v1 - v0).cross(v2 - v0).normalize_or_zero();
                            if n.dot(dir) > 0.0 { n = -n; } // face the camera
                            best = Some((t, n, nid));
                        }
                    }
                }
            }
        }
        // Live half-edge poly bodies (authoring mesh). mesh_cache can lag edits / be empty
        // right after create, so workplane placement missed poly surfaces (owner report).
        // Always test the half-edge; nearest-hit wins if cache also hit the same body.
        for (nid, node) in self.scene.scene_graph.poly_bodies() {
            if !node.visible { continue; }
            let mesh = match &node.payload {
                core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => continue,
            };
            let world = self.scene.scene_graph.world_transform(nid);
            // Ear-clipped, concave-safe (a fan put triangles OUTSIDE a concave n-gon, so a ray
            // through a cap's notch reported a bogus surface hit).
            let no_hidden = std::collections::HashSet::new();
            if let Some((t, fid)) =
                crate::poly_selection::ray_pick_mesh_face(mesh, world, origin, dir, &no_hidden)
            {
                if best.map_or(true, |(bt, _, _)| t < bt) {
                    let ln = mesh.compute_face_normal(core_math::half_edge::FaceId(fid));
                    let mut n = world.transform_vector3(ln).normalize_or_zero();
                    if n.dot(dir) > 0.0 { n = -n; }
                    best = Some((t, n, nid));
                }
            }
        }
        best.map(|(t, n, nid)| (origin + dir * t, n, nid))
    }

    /// Nearest surface hit across BOTH the analytical SDF field and cached meshes.
    /// Used for surface-aware placement (workplane, 3D cursor) so it recognizes every
    /// representation the user sees, not just SDF prims.
    pub(crate) fn raycast_scene_surface(&self, origin: Vec3, dir: Vec3) -> Option<(Vec3, Vec3)> {
        let sdf = self.raycast_scene(origin, dir);
        let mesh = self.raycast_scene_meshes(origin, dir).map(|(p, n, _)| (p, n));
        match (sdf, mesh) {
            (Some(s), Some(m)) => Some(
                if (s.0 - origin).length_squared() <= (m.0 - origin).length_squared() { s } else { m }
            ),
            (Some(s), None) => Some(s),
            (None, Some(m)) => Some(m),
            (None, None) => None,
        }
    }

    /// Evaluate full scene SDF at a point (analytical primitives)
    pub(crate) fn scene_distance(&self, p:Vec3) -> f32 {
        scene_sdf(p, &self.scene.primitives)
    }

    /// Detect which box face the mouse is hovering over. Returns (prim_idx, local_face_normal).
    pub(crate) fn detect_hovered_face(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<(usize, Vec3)> {
        let (origin, dir) = self.cam_ray(mouse, rect);
        let mut closest: Option<(usize, Vec3, f32)> = None;
        for (i, p) in self.scene.primitives.iter().enumerate() {
            if !p.visible { continue; }
            if let SdfShape::Box { half_extents, .. } = &p.shape {
                let he = *half_extents * p.scale;
                // With BODY_LOCAL_PRIMS the flat cache stores body-local positions.
                // raycast_box_face expects a world-space box centre — convert here.
                let world_pos = if BODY_LOCAL_PRIMS {
                    let bt: Option<BodyTransform> = self.scene.prim_body_map.get(i)
                        .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid));
                    if let Some(bt) = bt { bt.local_to_world(p.position) } else { p.position }
                } else {
                    p.position
                };
                if let Some((t, local_n)) = raycast_box_face(origin, dir, world_pos, p.rotation_quat(), he) {
                    if closest.as_ref().map_or(true, |c| t < c.2) {
                        closest = Some((i, local_n, t));
                    }
                }
            }
        }
        closest.map(|(i, n, _)| (i, n))
    }

    /// Detect face hover on a single primitive (for edit mode isolation).
    pub(crate) fn detect_hovered_face_single(&self, mouse: egui::Pos2, rect: egui::Rect, target: usize) -> Option<(usize, Vec3)> {
        if target >= self.scene.primitives.len() { return None; }
        let (origin, dir) = self.cam_ray(mouse, rect);
        let p = &self.scene.primitives[target];
        if let SdfShape::Box { half_extents, .. } = &p.shape {
            let he = *half_extents * p.scale;
            // With BODY_LOCAL_PRIMS the flat cache stores body-local positions.
            // raycast_box_face expects a world-space box centre — convert here.
            let world_pos = if BODY_LOCAL_PRIMS {
                let bt: Option<BodyTransform> = self.scene.prim_body_map.get(target)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid));
                if let Some(bt) = bt { bt.local_to_world(p.position) } else { p.position }
            } else {
                p.position
            };
            if let Some((_t, local_n)) = raycast_box_face(origin, dir, world_pos, p.rotation_quat(), he) {
                return Some((target, local_n));
            }
        }
        None
    }

    pub(crate) fn surface_tangent_frame(normal: Vec3, cam_orbit: Quat) -> (Vec3, Vec3) {
        // Horizontal surfaces (floor/ceiling) use world-aligned axes so footprint
        // stays axis-aligned regardless of camera angle — matches grid behavior.
        if normal.y.abs() > 0.7 {
            let tu = Vec3::X;
            let tv = tu.cross(normal).normalize(); // right-handed
            return (tu, tv);
        }
        // Vertical / angled surfaces use camera-projected axes
        let cam_right = (cam_orbit * Vec3::X).normalize();
        // Project camera right onto the surface plane (reject the normal component)
        let mut tu = cam_right - normal * cam_right.dot(normal);
        if tu.length_squared() < 0.001 {
            // Camera right is nearly parallel to normal — use camera up instead
            let cam_up = (cam_orbit * Vec3::Y).normalize();
            tu = cam_up - normal * cam_up.dot(normal);
        }
        tu = tu.normalize();
        let tv = tu.cross(normal).normalize(); // right-handed: Z = X × Y
        (tu, tv)
    }

    /// Convert a tangent frame (tangent_u=local X, normal=local Y, tangent_v=local Z)
    /// into rotation_deg (YXZ Euler angles in degrees).
    pub(crate) fn frame_to_rotation_deg(tu: Vec3, normal: Vec3, tv: Vec3) -> Vec3 {
        let mat = glam::Mat3::from_cols(tu, normal, tv);
        let quat = Quat::from_mat3(&mat);
        let (yaw, pitch, roll) = quat.to_euler(EulerRot::YXZ);
        Vec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees())
    }

    /// Ray-plane intersection: cast camera ray through `mouse` and intersect with the plane
    /// defined by (plane_point, plane_normal). Returns the world-space intersection point.
    pub(crate) fn ray_plane_hit(&self, mouse: egui::Pos2, rect: egui::Rect, plane_point: Vec3, plane_normal: Vec3) -> Option<Vec3> {
        let (origin, dir) = self.cam_ray(mouse, rect);
        let denom = dir.dot(plane_normal);
        if denom.abs() < 1e-6 { return None; } // ray parallel to plane
        let t = (plane_point - origin).dot(plane_normal) / denom;
        if t < 0.0 { return None; } // behind camera
        Some(origin + dir * t)
    }

    /// Snap orbit to the nearest axis view (Front/Right/Bottom/Back/Left/Top).
    /// One-shot only — caller must edge-trigger (not call every frame while Alt is held).
    ///
    /// **Perspective mode (Blender-style):** forces temporary orthographic for the
    /// straight view. Free-orbit that leaves the axis restores perspective
    /// (`ortho_from_view_snap`). **User Orthographic mode:** only snaps rotation —
    /// stays ortho until the user switches Perspective in settings.
    pub(crate) fn snap_orbit_to_nearest_view(&mut self) {
        let hp = std::f32::consts::FRAC_PI_2;
        let views: [(Quat, &str); 6] = [
            (Quat::from_euler(EulerRot::YXZ, 0.0, 0.0, 0.0), "Front"),
            (Quat::from_euler(EulerRot::YXZ, -hp, 0.0, 0.0), "Right"),
            (Quat::from_euler(EulerRot::YXZ, 0.0, hp, 0.0), "Bottom"),
            (Quat::from_euler(EulerRot::YXZ, std::f32::consts::PI, 0.0, 0.0), "Back"),
            (Quat::from_euler(EulerRot::YXZ, hp, 0.0, 0.0), "Left"),
            (Quat::from_euler(EulerRot::YXZ, 0.0, -hp, 0.0), "Top"),
        ];
        let cur_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
        let mut best_i = 0usize;
        let mut best_dot = -2.0_f32;
        for (i, (q, _)) in views.iter().enumerate() {
            let f = (*q * Vec3::NEG_Z).normalize_or_zero();
            let d = cur_fwd.dot(f);
            if d > best_dot {
                best_dot = d;
                best_i = i;
            }
        }
        // Skip no-op if already on this view (avoids toast/dirty spam on re-entry).
        let already = self.ui.last_view_idx == Some(best_i)
            && self.render.ortho
            && cur_fwd.dot((views[best_i].0 * Vec3::NEG_Z).normalize_or_zero()) > 0.999;
        if already {
            return;
        }
        self.viewport.camera.orbit = views[best_i].0;
        // User-chosen Orthographic stays permanent. Perspective → temporary ortho for the snap.
        if self.render.ortho && !self.viewport.ortho_from_view_snap {
            // Permanent ortho: rotation only.
            self.toast(&format!("View: {} (ortho)", views[best_i].1));
        } else {
            self.render.ortho = true;
            self.viewport.ortho_from_view_snap = true;
            self.toast(&format!("View: {} (temp ortho)", views[best_i].1));
        }
        self.ui.last_view_idx = Some(best_i);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// True when camera looks nearly along ±X/±Y/±Z (axis-aligned view).
    pub(crate) fn orbit_is_axis_aligned(orbit: Quat) -> bool {
        let fwd = (orbit * Vec3::NEG_Z).normalize_or_zero();
        if !fwd.is_finite() || fwd.length_squared() < 1e-8 {
            return false;
        }
        // ~5.7° from an axis — free orbit past this restores perspective after temp snap.
        const DOT_THRESH: f32 = 0.995;
        let ax = fwd.x.abs();
        let ay = fwd.y.abs();
        let az = fwd.z.abs();
        ax.max(ay).max(az) > DOT_THRESH
    }

    /// If temp ortho came from view-snap and the orbit left the axis view, restore Perspective.
    pub(crate) fn maybe_restore_perspective_after_orbit(&mut self) {
        if !self.viewport.ortho_from_view_snap {
            return;
        }
        if Self::orbit_is_axis_aligned(self.viewport.camera.orbit) {
            return;
        }
        self.render.ortho = false;
        self.viewport.ortho_from_view_snap = false;
        self.ui.last_view_idx = None;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// User explicitly chose Perspective or Orthographic in settings / hotkey — drop temp flag.
    pub(crate) fn set_user_projection_ortho(&mut self, ortho: bool) {
        self.render.ortho = ortho;
        self.viewport.ortho_from_view_snap = false;
        self.mark_dirty(DirtyFlags::RENDER);
    }
}
