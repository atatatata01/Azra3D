//! Face hinge rotate — sequential modal tool.
//!
//! Workflow: select poly (one face or many) → activate → click hinge segment → rotate.
//!
//! **Shear-plane hinge (owner 2026-07-30):** free verts of the hinge patch do **not** free-orbit
//! on spheres (that twists side edges). Instead each free vert slides on the **ray of its
//! original side edge** from a fixed neighbour into the **half-plane it rotates into** — the
//! plane through the hinge line containing the vert's rigidly-rotated position. Side edges keep
//! their original direction → the panel shears open like a fold / hatch.
//!
//! For a single planar face that half-plane IS the rotated face plane, so the panel stays planar
//! and ends as a parallelogram — identical to the original single-face math. Stated per vert it
//! also generalises to a **multi-face patch** with no coplanarity assumption.
//!
//! Multi-select: every selected face folds together as one patch. With no face selection the tool
//! resolves a single face from the picked edge (larger panel = area × free-edge span).
//!
//! **Gizmo:** the standard rotate ring (same geometry/colour as the R gizmo), centred on the hinge
//! midpoint, around the hinge axis. **Alt** = free drag (no ring grab required).

use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui;
use glam::{Quat, Vec3};

use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN};
use core_math::scene_graph::NodePayload;

use crate::{
    ActiveTool, AzraApp, DirtyFlags, GIZMO_ARROW_PX, GIZMO_HIT_RADIUS, GIZMO_HOVER_WIDTH,
    GIZMO_LINE_WIDTH, GizmoComponent, ModelingContext, PolyFaceHingePhase, PolyFaceHingeRotate,
    PolyTool, PolyWorkMode, dist_point_to_ring, gizmo_color, world_to_screen,
};

/// Ring radius as a fraction of the R gizmo's arrow length — matches `draw_gizmo`'s rotate ring.
const GIZMO_RING_SCALE: f32 = 0.85;

impl AzraApp {
    /// Activate hinge rotate on the active poly body.
    pub(crate) fn poly_activate_face_hinge(&mut self) {
        if self.tool.current.is_poly_face_hinge() {
            if self
                .tool
                .current
                .as_poly_face_hinge()
                .map(|d| d.phase == PolyFaceHingePhase::Rotate)
                .unwrap_or(false)
            {
                self.poly_confirm_face_hinge();
            }
            return;
        }
        if self.modeling_context != ModelingContext::Polygonal {
            self.toast("Hinge Rotate: select a poly mesh first");
            return;
        }
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Hinge Rotate: select a poly object first");
                return;
            }
        };
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Hinge Rotate: active object is not a poly mesh");
            return;
        }

        // Multi-face: the whole face selection is the swinging patch. Sorted = deterministic.
        let mut preferred_faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        preferred_faces.sort_unstable();

        if self.poly_work_mode == PolyWorkMode::Object {
            self.poly_work_mode = if preferred_faces.is_empty() {
                PolyWorkMode::Edge
            } else {
                PolyWorkMode::Face
            };
        }

        let n_faces = preferred_faces.len();
        let anchor_face = preferred_faces.first().copied().unwrap_or(0);
        self.poly_tool = PolyTool::Idle;
        self.start_operation(ActiveTool::PolyFaceHingeRotate(PolyFaceHingeRotate {
            node_id: nid,
            phase: PolyFaceHingePhase::PickEdge,
            preferred_faces,
            hover_edge: None,
            original_mesh: None,
            faces: Vec::new(),
            anchor_face,
            hinge_edge: 0,
            axis_origin: Vec3::ZERO,
            axis_dir: Vec3::Y,
            axis_mid: Vec3::ZERO,
            moving_verts: Vec::new(),
            orig_positions: Vec::new(),
            start_mouse: egui::pos2(0.0, 0.0),
            angle_deg: 0.0,
            gizmo_dragging: false,
            gizmo_hover: false,
        }));
        self.ui2.active_tool = "Hinge Rotate".into();
        self.ui2.active_pillar = "poly".into();
        if n_faces > 1 {
            self.toast(&format!(
                "Hinge ({n_faces} faces): click edge · drag ring (or Alt+drag free) · Enter=confirm · Esc=cancel"
            ));
        } else {
            self.toast(
                "Hinge: click edge · then drag ring (or Alt+drag free) · Enter=confirm · Esc=cancel",
            );
        }
    }

    pub(crate) fn poly_face_hinge_click(&mut self, mouse: egui::Pos2, alt: bool) {
        let phase = match self.tool.current.as_poly_face_hinge() {
            Some(d) => d.phase,
            None => return,
        };
        match phase {
            PolyFaceHingePhase::PickEdge => self.poly_face_hinge_pick_edge(mouse),
            PolyFaceHingePhase::Rotate => {
                // LMB on gizmo starts drag; LMB elsewhere confirms unless Alt free-mode just used.
                if self.poly_face_hinge_hit_gizmo(mouse) {
                    if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
                        d.gizmo_dragging = true;
                        d.start_mouse = mouse;
                        // Keep current angle as base: store by freezing start at current pose.
                        // Re-base: set start so angle delta starts at 0 from this click.
                        d.start_mouse = mouse;
                        // angle_deg stays; we measure delta from start each frame relative to
                        // click — reset by storing angle_at_press in start... simpler: set
                        // start_mouse and zero the *delta* by remembering base_angle.
                    }
                    // Use angle_deg as absolute from gizmo geometry each frame while dragging.
                } else if !alt {
                    self.poly_confirm_face_hinge();
                }
            }
        }
    }

    pub(crate) fn poly_face_hinge_release(&mut self) {
        if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
            d.gizmo_dragging = false;
        }
    }

    fn poly_face_hinge_pick_edge(&mut self, mouse: egui::Pos2) {
        let nid = match self.tool.current.as_poly_face_hinge() {
            Some(d) => d.node_id,
            None => return,
        };
        let preferred: Vec<u32> = self
            .tool
            .current
            .as_poly_face_hinge()
            .map(|d| d.preferred_faces.clone())
            .unwrap_or_default();

        let edge = self
            .poly_sel
            .hovered_edge
            .or_else(|| self.poly_pick_nearest_edge(nid, mouse));

        let Some(hinge) = edge else {
            self.toast("Hinge: click a mesh edge (segment)");
            return;
        };

        let (original_mesh, faces, anchor, built) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            let world = self.scene.scene_graph.world_transform(nid);
            let Some((faces, anchor)) =
                resolve_hinge_patch(mesh, hinge, &preferred, world, &self.viewport.camera)
            else {
                self.toast("Hinge: could not resolve a face for that edge");
                return;
            };
            let Some(built) = build_hinge_panel(mesh, &faces, anchor, hinge, world) else {
                self.toast("Hinge: could not build swing for that edge");
                return;
            };
            (mesh.clone(), faces, anchor, built)
        };

        if built.moving_verts.is_empty() {
            self.toast("Hinge: no free verts on that selection to swing");
            return;
        }

        self.begin_episode("Face Hinge Rotate");

        let n_faces = faces.len();
        if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
            d.phase = PolyFaceHingePhase::Rotate;
            d.original_mesh = Some(original_mesh);
            d.faces = faces.clone();
            d.anchor_face = anchor;
            d.hinge_edge = hinge;
            d.axis_origin = built.axis_origin;
            d.axis_dir = built.axis_dir;
            d.axis_mid = built.axis_mid;
            d.moving_verts = built.moving_verts;
            d.orig_positions = built.orig_positions;
            d.start_mouse = mouse;
            d.angle_deg = 0.0;
            d.hover_edge = Some(hinge);
            d.gizmo_dragging = false;
        }
        // Select hinge edge + patch faces only — do NOT seed selected_vertices (that draws the
        // vertex cage overlay the hinge tool should own itself).
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_edges.insert(hinge);
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_faces.extend(faces);
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.hovered_vertex = None;

        self.toast(&format!(
            "Hinge locked ({n_faces} face{}) — drag ring · Alt+drag free · Shift=snap ° · Enter=confirm · Esc=cancel",
            if n_faces == 1 { "" } else { "s" }
        ));
    }

    fn poly_pick_nearest_edge(
        &self,
        nid: core_math::scene_graph::NodeId,
        mouse: egui::Pos2,
    ) -> Option<u32> {
        use crate::dist_point_to_segment;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let fov = self.render.fov_deg;
        let orbit = self.viewport.camera.orbit;

        let mut best: Option<(f32, u32)> = None;
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE {
                continue;
            }
            if he.twin != NO_TWIN && i > he.twin.0 {
                continue;
            }
            let a = world.transform_point3(mesh.vert(mesh.he_source(HalfEdgeId(i))).position);
            let b = world.transform_point3(mesh.vert(he.vertex).position);
            let Some(sa) = world_to_screen(a, cam_pos, orbit, rect, aspect, ortho_hw, fov) else {
                continue;
            };
            let Some(sb) = world_to_screen(b, cam_pos, orbit, rect, aspect, ortho_hw, fov) else {
                continue;
            };
            let d = dist_point_to_segment(mouse, sa, sb);
            if d < 14.0 && best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        best.map(|(_, e)| e)
    }

    /// Rotate-ring frame: `(centre, axis, world radius)`. Centred on the hinge midpoint and sized
    /// exactly like `draw_gizmo`'s rotate ring so the two gizmos read as one system.
    fn poly_face_hinge_gizmo_frame(&self) -> Option<(Vec3, Vec3, f32)> {
        let d = self.tool.current.as_poly_face_hinge()?;
        if d.phase != PolyFaceHingePhase::Rotate {
            return None;
        }
        let centre = d.axis_mid;
        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let dist = (centre - cam_pos).length();
        let r = dist * GIZMO_ARROW_PX / (rect.height().max(1.0) * 0.5) * GIZMO_RING_SCALE;
        Some((centre, d.axis_dir, r))
    }

    /// Axis colour for the hinge ring: the world axis the hinge runs along, in the same palette as
    /// the R gizmo. A hinge that is not axis-aligned gets the neutral (Center) colour rather than
    /// a misleading red/green/blue.
    fn poly_face_hinge_axis_color(&self, axis: Vec3, hot: bool) -> egui::Color32 {
        const AXIS_SNAP: f32 = 0.95; // ≈18° — an edge this close to an axis reads as that axis
        let mut best = (0.0f32, GizmoComponent::Center);
        for (dir, comp) in [
            (Vec3::X, GizmoComponent::AxisX),
            (Vec3::Y, GizmoComponent::AxisY),
            (Vec3::Z, GizmoComponent::AxisZ),
        ] {
            let d = axis.dot(dir).abs();
            if d > best.0 {
                best = (d, comp);
            }
        }
        let comp = if best.0 >= AXIS_SNAP {
            best.1
        } else {
            GizmoComponent::Center
        };
        gizmo_color(comp, hot)
    }

    fn poly_face_hinge_ref_dir(&self, d: &PolyFaceHingeRotate) -> Option<Vec3> {
        if d.orig_positions.is_empty() {
            return None;
        }
        let world = self.scene.scene_graph.world_transform(d.node_id);
        let mut c = Vec3::ZERO;
        for p in &d.orig_positions {
            c += world.transform_point3(*p);
        }
        c /= d.orig_positions.len() as f32;
        let v = c - d.axis_origin;
        let axis = d.axis_dir;
        let perp = v - axis * v.dot(axis);
        let n = perp.normalize_or_zero();
        if n.length_squared() < 0.25 {
            // Degenerate: pick camera-facing perpendicular.
            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
            let mut p = axis.cross(cam_fwd);
            if p.length_squared() < 1e-6 {
                p = axis.cross(Vec3::Y);
            }
            if p.length_squared() < 1e-6 {
                p = axis.cross(Vec3::X);
            }
            Some(p.normalize_or_zero())
        } else {
            Some(n)
        }
    }

    /// Grab anywhere on the ring — same hit model (and radius) as the R gizmo's rotate rings.
    fn poly_face_hinge_hit_gizmo(&self, mouse: egui::Pos2) -> bool {
        let Some((centre, axis, r)) = self.poly_face_hinge_gizmo_frame() else {
            return false;
        };
        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        dist_point_to_ring(
            mouse,
            centre,
            axis,
            r,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        ) <= GIZMO_HIT_RADIUS
    }

    pub(crate) fn poly_update_face_hinge(&mut self, mouse: egui::Pos2, shift: bool, alt: bool) {
        let phase = match self.tool.current.as_poly_face_hinge() {
            Some(d) => d.phase,
            None => return,
        };

        if phase == PolyFaceHingePhase::PickEdge {
            // Always screen-pick nearest edge — poly_sel.hovered_edge only updates in Edge mode
            // and is empty in Face/Object pick, so pre-highlight would never appear.
            let nid = self
                .tool
                .current
                .as_poly_face_hinge()
                .map(|d| d.node_id);
            let hover = nid.and_then(|nid| self.poly_pick_nearest_edge(nid, mouse));
            if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
                d.hover_edge = hover;
            }
            // Mirror into poly_sel so any shared hover consumers stay in sync.
            self.poly_sel.hovered_edge = hover;
            self.poly_sel.hovered_vertex = None;
            return;
        }

        // Hover gizmo
        let hit = self.poly_face_hinge_hit_gizmo(mouse);
        if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
            d.gizmo_hover = hit;
        }

        let dragging = self
            .tool
            .current
            .as_poly_face_hinge()
            .map(|d| d.gizmo_dragging)
            .unwrap_or(false);

        // Free mode (Alt+LMB) or gizmo drag: update angle.
        if !alt && !dragging {
            return;
        }

        // Alt free-drag measures the SAME absolute angle around the hinge as a ring drag — there is
        // no separate re-basing path, so both gestures agree frame-for-frame.
        let (start, pivot_w) = {
            let d = match self.tool.current.as_poly_face_hinge() {
                Some(d) if d.phase == PolyFaceHingePhase::Rotate => d,
                _ => return,
            };
            (d.start_mouse, d.axis_mid)
        };

        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let fov = self.render.fov_deg;
        let orbit = self.viewport.camera.orbit;

        // Absolute angle around the hinge in screen space vs rest pose (ref_dir at 0°), measured
        // about the ring centre so the cursor tracks the ring it is dragging.
        let mut angle = {
            let mut deg = if let Some(pivot_s) =
                world_to_screen(pivot_w, cam_pos, orbit, rect, aspect, ortho_hw, fov)
            {
                let a0 = (start - pivot_s).angle();
                let a1 = (mouse - pivot_s).angle();
                (a1 - a0).to_degrees()
            } else {
                (mouse.x - start.x) * 0.5
            };
            // Prefer absolute vs ref when available (stable, Blender-like).
            if let Some(ref_dir) = self
                .tool
                .current
                .as_poly_face_hinge()
                .and_then(|d| self.poly_face_hinge_ref_dir(d))
            {
                if let (Some(os), Some(rs)) = (
                    world_to_screen(pivot_w, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                    world_to_screen(
                        pivot_w + ref_dir,
                        cam_pos,
                        orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        fov,
                    ),
                ) {
                    let zero = (rs - os).angle();
                    let cur = (mouse - os).angle();
                    deg = (cur - zero).to_degrees();
                }
            }
            while deg > 180.0 {
                deg -= 360.0;
            }
            while deg <= -180.0 {
                deg += 360.0;
            }
            deg
        };

        if shift {
            let step = self.viewport.snap_rotation_deg.max(1.0);
            angle = (angle / step).round() * step;
        }

        if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
            if (d.angle_deg - angle).abs() < 0.05 {
                return;
            }
            d.angle_deg = angle;
        }
        self.poly_face_hinge_regenerate();
    }

    pub(crate) fn poly_face_hinge_regenerate(&mut self) {
        let (nid, original, axis_o, axis_d, rails, angle_deg) = {
            let d = match self.tool.current.as_poly_face_hinge() {
                Some(d) if d.phase == PolyFaceHingePhase::Rotate => d,
                _ => return,
            };
            let orig = match d.original_mesh.as_ref() {
                Some(m) => m.clone(),
                None => return,
            };
            // Rebuild rails from snapshot + stored free verts (world-space at rest).
            let world = self.scene.scene_graph.world_transform(d.node_id);
            let rails = build_shear_rails(
                orig.as_ref(),
                d.hinge_edge,
                &d.moving_verts,
                &d.orig_positions,
                world,
            );
            (
                d.node_id,
                orig,
                d.axis_origin,
                d.axis_dir,
                rails,
                d.angle_deg,
            )
        };

        let world = self.scene.scene_graph.world_transform(nid);
        let inv = world.inverse();
        let q = Quat::from_axis_angle(axis_d, angle_deg.to_radians());

        let mut new_mesh = (*original).clone();
        // Shear-plane hinge: each free vert slides on its side-edge ray into the half-plane it
        // rotates into. Side edges keep their original direction → parallelogram / shear fold.
        for rail in &rails {
            let p_world = shear_free_vert_on_plane(rail, axis_o, axis_d, q);
            let back = inv.transform_point3(p_world);
            if (rail.vid as usize) < new_mesh.vertices.len() {
                new_mesh.vert_mut(VertexId(rail.vid)).position = back;
            }
        }

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = Arc::new(new_mesh);
            }
        }
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_confirm_face_hinge(&mut self) {
        let Some(d) = self.tool.current.as_poly_face_hinge() else {
            return;
        };
        if d.phase != PolyFaceHingePhase::Rotate {
            return;
        }
        let angle = d.angle_deg;
        if angle.abs() < 0.05 {
            self.poly_cancel_face_hinge();
            self.toast("Hinge Rotate cancelled (zero angle)");
            return;
        }
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.toast(&format!("Hinge Rotate applied ({angle:.1}°)"));
    }

    pub(crate) fn poly_cancel_face_hinge(&mut self) {
        let (nid, original_mesh, phase) = match self.tool.current.as_poly_face_hinge() {
            Some(d) => (d.node_id, d.original_mesh.clone(), d.phase),
            None => {
                self.tool.current = ActiveTool::Idle;
                self.poly_tool = PolyTool::Idle;
                return;
            }
        };
        if phase == PolyFaceHingePhase::Rotate {
            if let Some(orig) = original_mesh {
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *mesh = orig;
                    }
                }
                self.cancel_episode();
                self.poly_request_display_sync(nid);
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Draw the hinge overlay.
    ///
    /// **Geometry overlay is the yellow hinge line only** (owner 2026-08-09) — no face fills, no
    /// free-edge rails, no vertex ticks. The mesh reads through; the line says where it folds.
    /// The rotate ring is the only other viewport furniture.
    pub(crate) fn draw_face_hinge_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(d) = self.tool.current.as_poly_face_hinge() else {
            return;
        };
        let nid = d.node_id;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let fov = self.render.fov_deg;
        let orbit = self.viewport.camera.orbit;
        let world = self.scene.scene_graph.world_transform(nid);
        let to_s = |p: Vec3| world_to_screen(p, cam_pos, orbit, rect, aspect, ortho_hw, fov);

        let Some(node) = self.scene.scene_graph.get(nid) else {
            return;
        };
        let Some(mesh) = (match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh),
            _ => None,
        }) else {
            return;
        };

        let yellow = egui::Color32::from_rgb(255, 200, 40);

        // ── Pick phase: the hover edge as the yellow line, nothing else ──
        if d.phase == PolyFaceHingePhase::PickEdge {
            let edge = d.hover_edge.or(self.poly_sel.hovered_edge);
            if let Some(e) = edge {
                // Outer glow + solid stroke so "click to set hinge" is obvious
                self.draw_edge_line(
                    painter,
                    mesh,
                    &world,
                    e,
                    egui::Color32::from_rgba_unmultiplied(255, 200, 40, 90),
                    9.0,
                    &to_s,
                );
                self.draw_edge_line(painter, mesh, &world, e, yellow, 4.5, &to_s);
                painter.text(
                    egui::pos2(rect.center().x, rect.max.y - 28.0),
                    egui::Align2::CENTER_BOTTOM,
                    "Hinge: click highlighted edge to set  ·  Esc=cancel",
                    egui::FontId::proportional(13.0),
                    yellow,
                );
            } else {
                painter.text(
                    egui::pos2(rect.center().x, rect.max.y - 28.0),
                    egui::Align2::CENTER_BOTTOM,
                    "Hinge: hover an edge, then click to set  ·  Esc=cancel",
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(200, 200, 180),
                );
            }
            return;
        }

        // ── Rotate phase: the yellow hinge line + the rotate ring ──
        self.draw_edge_line(painter, mesh, &world, d.hinge_edge, yellow, 4.5, &to_s);

        // Rotate ring — same geometry as `draw_gizmo`'s R rings, coloured by the hinge axis.
        if let Some((centre, axis, r)) = self.poly_face_hinge_gizmo_frame() {
            let hot = d.gizmo_hover || d.gizmo_dragging;
            let col = self.poly_face_hinge_axis_color(axis, hot);
            let w = if hot {
                GIZMO_HOVER_WIDTH
            } else {
                GIZMO_LINE_WIDTH
            };
            // Ring basis, identical to draw_gizmo / dist_point_to_ring so draw and hit agree.
            let p1 = if axis.y.abs() > 0.9 { Vec3::X } else { Vec3::Y }
                .cross(axis)
                .normalize_or_zero();
            let p2 = axis.cross(p1).normalize_or_zero();
            if p1.length_squared() > 0.25 && p2.length_squared() > 0.25 {
                let mut prev: Option<egui::Pos2> = None;
                for i in 0..=48 {
                    let a = (i as f32 / 48.0) * std::f32::consts::TAU;
                    let p = centre + (p1 * a.cos() + p2 * a.sin()) * r;
                    match to_s(p) {
                        Some(sp) => {
                            if let Some(pv) = prev {
                                painter.line_segment([pv, sp], egui::Stroke::new(w, col));
                            }
                            prev = Some(sp);
                        }
                        None => prev = None,
                    }
                }
                // Swept sector: rest direction → current angle, on the same ring.
                if let Some(ref_dir) = self.poly_face_hinge_ref_dir(d) {
                    let sweep = d.angle_deg.to_radians();
                    if sweep.abs() > 1e-3 {
                        let steps = 48;
                        let mut prev: Option<egui::Pos2> = None;
                        for i in 0..=steps {
                            let t = sweep * (i as f32) / (steps as f32);
                            let p = centre
                                + Quat::from_axis_angle(axis, t) * (ref_dir * r);
                            match to_s(p) {
                                Some(sp) => {
                                    if let Some(pv) = prev {
                                        painter.line_segment(
                                            [pv, sp],
                                            egui::Stroke::new(GIZMO_HOVER_WIDTH, col),
                                        );
                                    }
                                    prev = Some(sp);
                                }
                                None => prev = None,
                            }
                        }
                    }
                }
            }
        }

        let n_faces = d.faces.len().max(1);
        painter.text(
            egui::pos2(rect.center().x, rect.max.y - 28.0),
            egui::Align2::CENTER_BOTTOM,
            format!(
                "Hinge  {:.1}°  ·  {n_faces} face{}  ·  drag ring  ·  Alt=free  ·  Shift=snap  ·  Enter=confirm",
                d.angle_deg,
                if n_faces == 1 { "" } else { "s" }
            ),
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(220, 220, 220),
        );
    }

    fn draw_edge_line(
        &self,
        painter: &egui::Painter,
        mesh: &HalfEdgeMesh,
        world: &glam::Mat4,
        edge: u32,
        color: egui::Color32,
        width: f32,
        to_s: &dyn Fn(Vec3) -> Option<egui::Pos2>,
    ) {
        if edge as usize >= mesh.half_edges.len() {
            return;
        }
        let he = mesh.he(HalfEdgeId(edge));
        let a = world.transform_point3(mesh.vert(mesh.he_source(HalfEdgeId(edge))).position);
        let b = world.transform_point3(mesh.vert(he.vertex).position);
        if let (Some(sa), Some(sb)) = (to_s(a), to_s(b)) {
            painter.line_segment([sa, sb], egui::Stroke::new(width, color));
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Rotate `p` around the infinite line through `origin` with direction `axis`, using prebuilt `q`
/// = `Quat::from_axis_angle(axis, angle)`. Parallel component along the line is fixed.
fn rotate_around_axis_line(p: Vec3, origin: Vec3, axis: Vec3, q: Quat) -> Vec3 {
    let axis = axis.normalize_or_zero();
    if axis.length_squared() < 0.25 {
        return p;
    }
    let v = p - origin;
    let parallel = axis * v.dot(axis);
    let perp = v - parallel;
    origin + parallel + q * perp
}

fn hinge_vert_ids(mesh: &HalfEdgeMesh, hinge: u32) -> HashSet<u32> {
    let mut s = HashSet::new();
    if hinge as usize >= mesh.half_edges.len() {
        return s;
    }
    s.insert(mesh.he_source(HalfEdgeId(hinge)).0);
    s.insert(mesh.he(HalfEdgeId(hinge)).vertex.0);
    s
}

fn face_area_world(mesh: &HalfEdgeMesh, face: u32, world: glam::Mat4) -> f32 {
    let verts = mesh.face_vertices(FaceId(face));
    if verts.len() < 3 {
        return 0.0;
    }
    let p0 = world.transform_point3(mesh.vert(verts[0]).position);
    let mut area = 0.0;
    for i in 1..verts.len() - 1 {
        let p1 = world.transform_point3(mesh.vert(verts[i]).position);
        let p2 = world.transform_point3(mesh.vert(verts[i + 1]).position);
        area += (p1 - p0).cross(p2 - p0).length() * 0.5;
    }
    area
}

/// Length of the free edge(s): max distance between free verts (proxy for panel span).
fn free_span_world(
    mesh: &HalfEdgeMesh,
    face: u32,
    hinge_verts: &HashSet<u32>,
    world: glam::Mat4,
) -> f32 {
    let free: Vec<Vec3> = mesh
        .face_vertices(FaceId(face))
        .into_iter()
        .filter(|v| !hinge_verts.contains(&v.0))
        .map(|v| world.transform_point3(mesh.vert(v).position))
        .collect();
    if free.len() < 2 {
        return free.first().map(|_| 0.01).unwrap_or(0.0);
    }
    let mut best = 0.0f32;
    for i in 0..free.len() {
        for j in (i + 1)..free.len() {
            best = best.max((free[i] - free[j]).length());
        }
    }
    best
}

/// Resolve the swinging patch for `hinge`: `(faces, anchor)`.
///
/// Every **selected** face folds together (multi-poly hinge). `anchor` is a patch face that bounds
/// the hinge — it supplies the rest normal that orients the swing. With no usable selection the
/// patch is the single auto-resolved face (larger panel = area × free-edge span).
fn resolve_hinge_patch(
    mesh: &HalfEdgeMesh,
    hinge: u32,
    preferred: &[u32],
    world: glam::Mat4,
    camera: &crate::types::CameraState,
) -> Option<(Vec<u32>, u32)> {
    if hinge as usize >= mesh.half_edges.len() {
        return None;
    }
    let he = mesh.he(HalfEdgeId(hinge));
    let mut faces = Vec::new();
    if he.face != NO_FACE {
        faces.push(he.face.0);
    }
    if he.twin != NO_TWIN {
        let tf = mesh.he(he.twin).face;
        if tf != NO_FACE {
            faces.push(tf.0);
        }
    }
    if faces.is_empty() {
        return None;
    }
    // Face selection wins whenever ANY selected face bounds the hinge: the whole selection swings,
    // anchored on the selected face that touches the hinge (largest, if both do).
    if !preferred.is_empty() {
        let anchor = preferred
            .iter()
            .copied()
            .filter(|f| faces.contains(f))
            .max_by(|a, b| {
                face_area_world(mesh, *a, world)
                    .total_cmp(&face_area_world(mesh, *b, world))
            });
        if let Some(anchor) = anchor {
            let mut patch: Vec<u32> = preferred
                .iter()
                .copied()
                .filter(|&f| {
                    (f as usize) < mesh.faces.len() && mesh.face(FaceId(f)).half_edge != NO_EDGE
                })
                .collect();
            patch.sort_unstable();
            patch.dedup();
            return Some((patch, anchor));
        }
        // Selection exists but none of it touches the picked edge — fall through to auto-resolve
        // rather than folding a patch that is not attached to the hinge.
    }

    let hinge_verts = hinge_vert_ids(mesh, hinge);
    let cam_pos = camera.target + camera.orbit * Vec3::new(0.0, 0.0, camera.distance);
    // Prefer the **larger door panel**: area × free-edge span, then camera-facing.
    // Avoids picking a tiny end triangle over the main plate when the shared edge is short.
    let mut best: Option<(f32, u32)> = None;
    for &f in &faces {
        let verts = mesh.face_vertices(FaceId(f));
        if verts.len() < 3 {
            continue;
        }
        let area = face_area_world(mesh, f, world);
        let span = free_span_world(mesh, f, &hinge_verts, world);
        let p0 = world.transform_point3(mesh.vert(verts[0]).position);
        let p1 = world.transform_point3(mesh.vert(verts[1]).position);
        let p2 = world.transform_point3(mesh.vert(verts[2]).position);
        let n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        let c = {
            let mut s = Vec3::ZERO;
            for v in &verts {
                s += world.transform_point3(mesh.vert(*v).position);
            }
            s / verts.len() as f32
        };
        let toward = (cam_pos - c).normalize_or_zero();
        let facing = n.dot(toward).max(0.0);
        // Primary: panel size. Secondary: facing (0..1).
        let score = area * (span + 1e-4) * (0.35 + 0.65 * facing);
        if best.map_or(true, |(s, _)| score > s) {
            best = Some((score, f));
        }
    }
    best.map(|(_, f)| (vec![f], f))
}

struct HingePanel {
    axis_origin: Vec3,
    axis_dir: Vec3,
    axis_mid: Vec3,
    moving_verts: Vec<u32>,
    orig_positions: Vec<Vec3>,
}

/// Per free vert: slide on side-edge ray (fixed origin → original free) into rotated plane.
struct ShearRail {
    vid: u32,
    /// Original free vert in world.
    orig_w: Vec3,
    /// Ray origin (hinge endpoint or other fixed neighbour) in world.
    rail_origin_w: Vec3,
    /// Unit direction along original side edge (orig_w - rail_origin).
    rail_dir_w: Vec3,
    /// Rest length along the rail (|orig - origin|).
    rail_len: f32,
}

fn find_hinge_on_face(mesh: &HalfEdgeMesh, face: u32, hinge_he: u32) -> Option<HalfEdgeId> {
    let he = mesh.he(HalfEdgeId(hinge_he));
    if he.face == FaceId(face) {
        return Some(HalfEdgeId(hinge_he));
    }
    if he.twin != NO_TWIN && mesh.he(he.twin).face == FaceId(face) {
        return Some(he.twin);
    }
    let a = mesh.he_source(HalfEdgeId(hinge_he)).0;
    let b = he.vertex.0;
    let start = mesh.face(FaceId(face)).half_edge;
    if start == NO_EDGE {
        return None;
    }
    let mut cur = start;
    loop {
        let h = mesh.he(cur);
        let sa = mesh.he_source(cur).0;
        let sb = h.vertex.0;
        if (sa == a && sb == b) || (sa == b && sb == a) {
            return Some(cur);
        }
        cur = h.next;
        if cur == start {
            break;
        }
    }
    None
}

/// Ray–plane: origin + t·dir, plane (n, p0). Returns t if finite.
fn ray_plane_t(origin: Vec3, dir: Vec3, n: Vec3, p0: Vec3) -> Option<f32> {
    let denom = n.dot(dir);
    if denom.abs() < 1e-8 {
        return None;
    }
    Some(n.dot(p0 - origin) / denom)
}

/// Shear free vert: slide on its body side-edge ray into the **half-plane it rotates into** — the
/// plane spanned by the hinge axis and the vert's rigidly-rotated position.
///
/// For a single planar panel every vert's half-plane is the same rotated face plane, so this is
/// exactly the original shear (panel stays planar, side edges keep direction). Stated per vert it
/// needs no face normal, so a **multi-face patch** shears without any coplanarity assumption.
/// Falls back to pure orbit when there is no usable rail (open shell / no fixed neighbour) or the
/// vert sits on the axis (no radial direction → no half-plane).
fn shear_free_vert_on_plane(rail: &ShearRail, axis_o: Vec3, axis_d: Vec3, q: Quat) -> Vec3 {
    let p_rot = rotate_around_axis_line(rail.orig_w, axis_o, axis_d, q);
    if rail.rail_len > 1e-8 && rail.rail_dir_w.length_squared() > 0.25 {
        let v = p_rot - axis_o;
        let radial = v - axis_d * v.dot(axis_d);
        let plane_n = axis_d.cross(radial).normalize_or_zero();
        if plane_n.length_squared() > 0.25 {
            // t can be any real — free vert may move past the rest length (shear stretch).
            if let Some(t) = ray_plane_t(rail.rail_origin_w, rail.rail_dir_w, plane_n, axis_o) {
                if t.is_finite() && t.abs() < 1e5 {
                    return rail.rail_origin_w + rail.rail_dir_w * t;
                }
            }
        }
    }
    p_rot
}

/// Build side-edge rails for the patch's free verts (no coplanar grow).
///
/// Prefer rails to **fixed body verts that are NOT in the patch and NOT on the hinge** (e.g.
/// vertical box edges). Those rays are not coplanar with the rotated plane in a degenerate way, so
/// free verts can slide while **keeping side-edge direction** (shear parallelograms). Hinge-endpoint
/// rails only meet the rotated plane at t=0 — never use them as shear rails.
fn build_shear_rails(
    mesh: &HalfEdgeMesh,
    hinge_he: u32,
    moving: &[u32],
    orig_local: &[Vec3],
    world: glam::Mat4,
) -> Vec<ShearRail> {
    let hinge_verts = hinge_vert_ids(mesh, hinge_he);
    let free_set: HashSet<u32> = moving.iter().copied().collect();
    let mut rails = Vec::with_capacity(moving.len());

    for (i, &vid) in moving.iter().enumerate() {
        let orig_w = world.transform_point3(orig_local[i]);
        let mut best: Option<(Vec3, Vec3, f32)> = None; // origin, dir, score

        // 1) Preferred: edge to a fixed body vert NOT on the hinge (keeps wall-edge alignment).
        for &h in &mesh.vertex_fan(VertexId(vid)) {
            let nb = mesh.he(h).vertex.0;
            if free_set.contains(&nb) || hinge_verts.contains(&nb) {
                continue;
            }
            let fo = world.transform_point3(mesh.vert(VertexId(nb)).position);
            let delta = orig_w - fo;
            let len = delta.length();
            if len < 1e-8 {
                continue;
            }
            let dir = delta / len;
            // Prefer longer rails and those more perpendicular to the free edge span.
            let score = len;
            if best.map_or(true, |(_, _, s)| score > s) {
                best = Some((fo, dir, score));
            }
        }

        let (rail_origin_w, rail_dir_w, rail_len) = match best {
            Some((o, d, len)) => (o, d, len),
            None => {
                // No body rail (open surface / thin shell): pure orbit fallback.
                (orig_w, Vec3::ZERO, 0.0)
            }
        };
        rails.push(ShearRail {
            vid,
            orig_w,
            rail_origin_w,
            rail_dir_w,
            rail_len,
        });
    }
    rails
}

/// Free verts of the hinge PATCH (every selected face); side edges become shear rails at rest.
///
/// `anchor` must bound `hinge_he` — it supplies the rest normal that orients the swing direction.
fn build_hinge_panel(
    mesh: &HalfEdgeMesh,
    faces: &[u32],
    anchor: u32,
    hinge_he: u32,
    world: glam::Mat4,
) -> Option<HingePanel> {
    if anchor as usize >= mesh.faces.len() || mesh.face(FaceId(anchor)).half_edge == NO_EDGE {
        return None;
    }
    if hinge_he as usize >= mesh.half_edges.len() {
        return None;
    }

    let hinge_on_face = find_hinge_on_face(mesh, anchor, hinge_he)?;
    let v0 = mesh.he_source(hinge_on_face).0;
    let v1 = mesh.he(hinge_on_face).vertex.0;
    let p0 = world.transform_point3(mesh.vert(VertexId(v0)).position);
    let p1 = world.transform_point3(mesh.vert(VertexId(v1)).position);
    let mut axis_dir = (p1 - p0).normalize_or_zero();
    if axis_dir.length_squared() < 0.25 {
        return None;
    }
    let axis_origin = p0;
    let axis_mid = (p0 + p1) * 0.5;

    let hinge_verts: HashSet<u32> = [v0, v1].into_iter().collect();
    // Only the selected faces — no coplanar grow (that dragged half the body along).
    let mut moving_verts: Vec<u32> = faces
        .iter()
        .filter(|&&f| {
            (f as usize) < mesh.faces.len() && mesh.face(FaceId(f)).half_edge != NO_EDGE
        })
        .flat_map(|&f| mesh.face_vertices(FaceId(f)))
        .map(|v| v.0)
        .filter(|v| !hinge_verts.contains(v))
        .collect();
    moving_verts.sort_unstable();
    moving_verts.dedup();
    if moving_verts.is_empty() {
        return None;
    }

    let n = mesh.compute_face_normal(FaceId(anchor)).normalize_or_zero();
    let n_w = {
        let t0 = world.transform_point3(Vec3::ZERO);
        (world.transform_point3(n) - t0).normalize_or_zero()
    };
    let free_centroid = {
        let mut s = Vec3::ZERO;
        for &v in &moving_verts {
            s += world.transform_point3(mesh.vert(VertexId(v)).position);
        }
        s / moving_verts.len() as f32
    };
    let from_hinge = free_centroid - axis_origin;
    let radial = from_hinge - axis_dir * from_hinge.dot(axis_dir);
    if axis_dir.cross(radial).dot(n_w) < 0.0 {
        axis_dir = -axis_dir;
    }

    let orig_positions: Vec<Vec3> = moving_verts
        .iter()
        .map(|&v| mesh.vert(VertexId(v)).position)
        .collect();

    Some(HingePanel {
        axis_origin,
        axis_dir,
        axis_mid,
        moving_verts,
        orig_positions,
    })
}

#[cfg(test)]
mod hinge_tests {
    use super::*;
    use core_math::half_edge::HalfEdgeMesh;
    use glam::Vec3;

    /// Shear hinge: free verts stay on side-edge rays → edges keep alignment.
    #[test]
    fn hinge_shear_keeps_side_edge_direction() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut top_face = None;
        for fi in 0..mesh.faces.len() as u32 {
            if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                continue;
            }
            let n = mesh.compute_face_normal(FaceId(fi)).normalize_or_zero();
            if n.y > 0.9 {
                top_face = Some(fi);
                break;
            }
        }
        let face = top_face.expect("top face");
        let hes = mesh.face_half_edges(FaceId(face));
        let hinge = hes[0].0;
        let world = glam::Mat4::IDENTITY;
        let panel = build_hinge_panel(&mesh, &[face], face, hinge, world).expect("panel");
        assert!(panel.moving_verts.len() >= 2);

        let rails = build_shear_rails(
            &mesh,
            hinge,
            &panel.moving_verts,
            &panel.orig_positions,
            world,
        );
        let q = Quat::from_axis_angle(panel.axis_dir, 35f32.to_radians());

        for rail in &rails {
            if rail.rail_len < 1e-6 {
                continue;
            }
            let p = shear_free_vert_on_plane(rail, panel.axis_origin, panel.axis_dir, q);
            // Side edge direction from rail origin to new free vert ≈ original rail dir.
            let dir = (p - rail.rail_origin_w).normalize_or_zero();
            let align = dir.dot(rail.rail_dir_w).abs();
            assert!(
                align > 0.995,
                "side edge lost alignment: dot={align} vid={}",
                rail.vid
            );
        }
    }

    /// Per-vert half-plane shear reproduces the old whole-face plane result for a planar panel:
    /// the swung panel stays planar (all free verts land in one plane through the hinge).
    #[test]
    fn hinge_shear_planar_panel_stays_planar() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let face = (0..mesh.faces.len() as u32)
            .find(|&fi| {
                mesh.face(FaceId(fi)).half_edge != NO_EDGE
                    && mesh.compute_face_normal(FaceId(fi)).normalize_or_zero().y > 0.9
            })
            .expect("top face");
        let hinge = mesh.face_half_edges(FaceId(face))[0].0;
        let world = glam::Mat4::IDENTITY;
        let panel = build_hinge_panel(&mesh, &[face], face, hinge, world).expect("panel");
        let rails = build_shear_rails(
            &mesh,
            hinge,
            &panel.moving_verts,
            &panel.orig_positions,
            world,
        );
        let q = Quat::from_axis_angle(panel.axis_dir, 35f32.to_radians());
        // Plane through the hinge line with the rotated face normal — the old construction.
        let n1 = (q * mesh.compute_face_normal(FaceId(face)).normalize_or_zero())
            .normalize_or_zero();
        for rail in &rails {
            let p = shear_free_vert_on_plane(rail, panel.axis_origin, panel.axis_dir, q);
            let off = n1.dot(p - panel.axis_origin).abs();
            assert!(off < 1e-3, "free vert left the rotated panel plane: {off}");
        }
    }

    /// Multi-face patch: the swinging vert set is the UNION of every selected face's free verts,
    /// on the anchor face's hinge axis — one fold for the whole selection.
    #[test]
    fn hinge_multi_face_patch_folds_together() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let top = (0..mesh.faces.len() as u32)
            .find(|&fi| {
                mesh.face(FaceId(fi)).half_edge != NO_EDGE
                    && mesh.compute_face_normal(FaceId(fi)).normalize_or_zero().y > 0.9
            })
            .expect("top face");
        let hinge = mesh.face_half_edges(FaceId(top))[0].0;
        // The neighbour across the hinge — a second selected face, not coplanar with the top.
        let neighbour = mesh.he(mesh.he(HalfEdgeId(hinge)).twin).face.0;
        let world = glam::Mat4::IDENTITY;

        let single = build_hinge_panel(&mesh, &[top], top, hinge, world).expect("single");
        let patch = build_hinge_panel(&mesh, &[top, neighbour], top, hinge, world).expect("patch");

        for v in &single.moving_verts {
            assert!(patch.moving_verts.contains(v), "patch dropped vert {v}");
        }
        assert!(
            patch.moving_verts.len() > single.moving_verts.len(),
            "patch must move a superset: {} vs {}",
            patch.moving_verts.len(),
            single.moving_verts.len()
        );
        // Same picked edge → same hinge axis and ring centre regardless of patch size.
        assert!((patch.axis_dir - single.axis_dir).length() < 1e-5);
        assert!((patch.axis_mid - single.axis_mid).length() < 1e-5);
        // Hinge verts never move.
        let hv = hinge_vert_ids(&mesh, hinge);
        for v in &patch.moving_verts {
            assert!(!hv.contains(v), "hinge vert {v} must stay fixed");
        }
    }
}
