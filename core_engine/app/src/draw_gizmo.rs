// draw_gizmo.rs — Gizmo drawing, falloff handles, mask gizmo, falloff graph
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use core_math::primitives::MaskShape;
use crate::*;

use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    /// Compute the 3 falloff handle world positions: (origin, end, radius_handle).
    /// Radius handle sits at the midpoint, offset perpendicular to the axis.
    pub(crate) fn falloff_handle_positions(&self, idx: usize) -> Option<(Vec3, Vec3, Vec3)> {
        let prim = self.scene.primitives.get(idx)?;
        if !prim.blend_falloff.is_active() { return None; }
        let fo = &prim.blend_falloff;
        let axis_dir = fo.direction.normalize_or_zero();
        let prim_world_pos = if BODY_LOCAL_PRIMS {
            self.scene.prim_body_map.get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .unwrap_or_default()
                .local_to_world(prim.position)
        } else { prim.position };
        let origin = prim_world_pos + axis_dir * fo.origin_offset;
        let end = origin + axis_dir * fo.length;
        let mid = (origin + end) * 0.5;
        // Radius handle: perpendicular to axis, offset by current blend k
        let k_val = prim.operation.blend_k();
        // Pick a perpendicular direction (camera-aware for better visibility)
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let perp = axis_dir.cross(cam_fwd).normalize_or_zero();
        let perp = if perp.length() > 0.1 { perp } else {
            // axis aligned with camera forward, use camera right instead
            let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize();
            axis_dir.cross(cam_right).normalize_or_zero()
        };
        let radius_pt = mid + perp * k_val;
        Some((origin, end, radius_pt))
    }

    /// Detect which falloff handle the mouse is hovering (within 10px).
    pub(crate) fn detect_falloff_hover(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<FalloffHandle> {
        let idx = self.active_prim_index_unified()?;
        let (origin, end, radius_pt) = self.falloff_handle_positions(idx)?;
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let tol = self.tweak.falloff_hover_px;
        let mut best_d = tol;
        let mut best: Option<FalloffHandle> = None;
        for (wp, handle) in [(origin, FalloffHandle::Origin), (end, FalloffHandle::End), (radius_pt, FalloffHandle::Radius)] {
            if let Some(sp) = world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let d = (mouse - sp).length();
                if d < best_d { best_d = d; best = Some(handle); }
            }
        }
        best
    }

    pub(crate) fn draw_falloff_gizmo(&self, painter: &egui::Painter, rect: egui::Rect) {
        let idx = match self.active_prim_index_unified() { Some(i) if i < self.scene.primitives.len() => i, _ => return };
        let (origin, end, radius_pt) = match self.falloff_handle_positions(idx) { Some(v) => v, None => return };
        let prim = &self.scene.primitives[idx];
        let fo = &prim.blend_falloff;
        let axis_dir = fo.direction.normalize_or_zero();
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        let base_col = egui::Color32::from_rgb(100, 200, 255);
        let bright_col = egui::Color32::from_rgb(180, 240, 255);
        let dragging = self.tool.current.is_falloff_adjust();

        // Gradient line (16 segments, fading from dim to bright)
        let segs = 16;
        let mut prev: Option<egui::Pos2> = None;
        for i in 0..=segs {
            let t = i as f32 / segs as f32;
            let wp = origin + axis_dir * fo.length * t;
            if let Some(sp) = world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                if let Some(ps) = prev {
                    let a = 40 + (t * 215.0) as u8;
                    painter.line_segment([ps, sp], egui::Stroke::new(2.5,
                        egui::Color32::from_rgba_unmultiplied(100, 200, 255, a)));
                }
                prev = Some(sp);
            } else { prev = None; }
        }

        // Dashed line from midpoint to radius handle
        let mid = (origin + end) * 0.5;
        if let (Some(mid_s), Some(rad_s)) = (
            world_to_screen(mid, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            world_to_screen(radius_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
        ) {
            let rad_hov = self.tool.falloff_hovered == Some(FalloffHandle::Radius) || matches!(self.tool.current, ActiveTool::FalloffAdjust(ref fd) if fd.handle == FalloffHandle::Radius);
            let col = if rad_hov { bright_col } else { egui::Color32::from_rgba_unmultiplied(100, 200, 255, 100) };
            painter.line_segment([mid_s, rad_s], egui::Stroke::new(1.5, col));
        }

        // End handle (arrow tip)
        let end_hov = self.tool.falloff_hovered == Some(FalloffHandle::End) || matches!(self.tool.current, ActiveTool::FalloffAdjust(ref fd) if fd.handle == FalloffHandle::End);
        if let (Some(end_s), Some(org_s)) = (
            world_to_screen(end, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            world_to_screen(origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
        ) {
            let dir = egui::vec2(end_s.x - org_s.x, end_s.y - org_s.y);
            let l = dir.length();
            if l > 1.0 {
                let d = dir / l;
                let perp = egui::vec2(-d.y, d.x);
                let col = if end_hov { bright_col } else { base_col };
                let sz = if end_hov { 14.0 } else { 12.0 };
                let hw = if end_hov { 7.0 } else { 6.0 };
                painter.add(egui::Shape::convex_polygon(
                    vec![end_s,
                         egui::pos2(end_s.x - d.x*sz + perp.x*hw, end_s.y - d.y*sz + perp.y*hw),
                         egui::pos2(end_s.x - d.x*sz - perp.x*hw, end_s.y - d.y*sz - perp.y*hw)],
                    col, egui::Stroke::NONE));
            }
        }

        // Origin handle (filled circle)
        let org_hov = self.tool.falloff_hovered == Some(FalloffHandle::Origin) || matches!(self.tool.current, ActiveTool::FalloffAdjust(ref fd) if fd.handle == FalloffHandle::Origin);
        if let Some(org_s) = world_to_screen(origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let (r, col) = if org_hov { (6.0, bright_col) } else { (4.0, base_col) };
            painter.circle_filled(org_s, r, col);
            painter.circle_stroke(org_s, r, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }

        // Radius handle (diamond)
        let rad_hov = self.tool.falloff_hovered == Some(FalloffHandle::Radius) || matches!(self.tool.current, ActiveTool::FalloffAdjust(ref fd) if fd.handle == FalloffHandle::Radius);
        if let Some(rad_s) = world_to_screen(radius_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let s = if rad_hov { 7.0 } else { 5.0 };
            let col = if rad_hov { bright_col } else { base_col };
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(rad_s.x, rad_s.y - s), egui::pos2(rad_s.x + s, rad_s.y),
                     egui::pos2(rad_s.x, rad_s.y + s), egui::pos2(rad_s.x - s, rad_s.y)],
                col, egui::Stroke::new(1.0, egui::Color32::WHITE)));
        }

        // Tooltip when hovering
        if !dragging {
            if let Some(handle) = self.tool.falloff_hovered {
                if let Some(mp) = world_to_screen(match handle {
                    FalloffHandle::Origin => origin,
                    FalloffHandle::End => end,
                    FalloffHandle::Radius => radius_pt,
                }, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    let txt = match handle {
                        FalloffHandle::Origin => "Drag: move origin (rotate + offset)",
                        FalloffHandle::End => "Drag: rotate direction + length",
                        FalloffHandle::Radius => "Drag: adjust blend radius (k)",
                    };
                    painter.text(mp + egui::vec2(14.0, -14.0), egui::Align2::LEFT_CENTER, txt,
                        egui::FontId::proportional(11.0), egui::Color32::from_rgb(180, 230, 255));
                }
            }
        }
    }

    // ================================================================
    // Volume Mask Gizmo: wireframe sphere/box/gradient arrow
    // ================================================================
    pub(crate) fn draw_mask_gizmo(&self, painter: &egui::Painter, rect: egui::Rect) {
        let idx = match self.active_prim_index_unified() { Some(i) if i < self.scene.primitives.len() => i, _ => return };
        let prim = &self.scene.primitives[idx];
        if !prim.modifier.is_active() || prim.modifier.mask.shape == MaskShape::None { return; }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let quat = prim.rotation_quat();
        let mask = &prim.modifier.mask;
        // Transform mask center from local-space to world-space
        let prim_world_pos = if BODY_LOCAL_PRIMS {
            self.scene.prim_body_map.get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .unwrap_or_default()
                .local_to_world(prim.position)
        } else { prim.position };
        let mask_world_center = prim_world_pos + quat * (mask.center * prim.scale);
        let col = egui::Color32::from_rgba_unmultiplied(255, 220, 60, 160); // yellow
        let stroke = egui::Stroke::new(1.5, col);

        match mask.shape {
            MaskShape::Sphere => {
                let radius_world = mask.extents.x * prim.scale.x.min(prim.scale.y).min(prim.scale.z);
                // Draw 3 orthogonal circles (XY, XZ, YZ planes)
                let circle_segs = 32;
                for plane in 0..3 {
                    let mut prev_pt: Option<egui::Pos2> = None;
                    for i in 0..=circle_segs {
                        let a = (i as f32 / circle_segs as f32) * std::f32::consts::TAU;
                        let (s, c) = a.sin_cos();
                        let local_offset = match plane {
                            0 => Vec3::new(c, s, 0.0), // XY
                            1 => Vec3::new(c, 0.0, s), // XZ
                            _ => Vec3::new(0.0, c, s), // YZ
                        } * radius_world;
                        let wp = mask_world_center + quat * local_offset;
                        if let Some(sp) = world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                            if let Some(pp) = prev_pt {
                                painter.line_segment([pp, sp], stroke);
                            }
                            prev_pt = Some(sp);
                        } else { prev_pt = None; }
                    }
                }
            }
            MaskShape::Box => {
                let he = mask.extents * prim.scale;
                let corners: [Vec3; 8] = [
                    Vec3::new(-he.x, -he.y, -he.z), Vec3::new(he.x, -he.y, -he.z),
                    Vec3::new(he.x, -he.y, he.z),   Vec3::new(-he.x, -he.y, he.z),
                    Vec3::new(-he.x, he.y, -he.z),  Vec3::new(he.x, he.y, -he.z),
                    Vec3::new(he.x, he.y, he.z),    Vec3::new(-he.x, he.y, he.z),
                ];
                let edges = [(0,1),(1,2),(2,3),(3,0),(4,5),(5,6),(6,7),(7,4),(0,4),(1,5),(2,6),(3,7)];
                for (a, b) in edges {
                    let wa = mask_world_center + quat * corners[a];
                    let wb = mask_world_center + quat * corners[b];
                    if let (Some(sa), Some(sb)) = (
                        world_to_screen(wa, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                        world_to_screen(wb, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                    ) {
                        painter.line_segment([sa, sb], stroke);
                    }
                }
            }
            MaskShape::Gradient => {
                let dir_world = quat * (mask.extents * prim.scale);
                let start = mask_world_center;
                let end = mask_world_center + dir_world;
                if let (Some(ss), Some(se)) = (
                    world_to_screen(start, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                    world_to_screen(end, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                ) {
                    painter.line_segment([ss, se], egui::Stroke::new(2.0, col));
                    // Arrow head
                    let dir = egui::vec2(se.x - ss.x, se.y - ss.y);
                    let l = dir.length();
                    if l > 4.0 {
                        let d = dir / l;
                        let perp = egui::vec2(-d.y, d.x);
                        painter.add(egui::Shape::convex_polygon(
                            vec![se,
                                 egui::pos2(se.x - d.x*10.0 + perp.x*5.0, se.y - d.y*10.0 + perp.y*5.0),
                                 egui::pos2(se.x - d.x*10.0 - perp.x*5.0, se.y - d.y*10.0 - perp.y*5.0)],
                            col, egui::Stroke::NONE));
                    }
                }
            }
            _ => {}
        }
        // Label at mask center
        if let Some(cs) = world_to_screen(mask_world_center, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let label = match mask.shape {
                MaskShape::Sphere => "Sphere Mask",
                MaskShape::Box => "Box Mask",
                MaskShape::Gradient => "Gradient Mask",
                _ => "",
            };
            painter.text(cs + egui::vec2(0.0, -14.0), egui::Align2::CENTER_BOTTOM, label,
                egui::FontId::proportional(11.0), egui::Color32::from_rgba_unmultiplied(255, 220, 60, 200));
        }
    }

    // ================================================================
    // PRO GIZMO: Draw (rings for Rotate, arrows for Translate, squares for Scale)
    // ================================================================
    pub(crate) fn draw_gizmo(&self, painter: &egui::Painter, rect: egui::Rect) {
        // Show gizmo when armed (G/S/R pressed, waiting for axis click) or during active gizmo drag
        let gizmo_visible = self.tool.gizmo_armed
            || self.tool.current.is_gizmo()
            || (self.tool.current.is_transform() && !self.tool.current.as_transform().map_or(false, |t| t.free_mode))
            || (self.tool.current.is_poly_transform() && !self.tool.current.as_poly_transform().map_or(false, |t| t.free_mode))
            || (self.tool.current.is_nurbs_cv_move() && !self.tool.current.as_nurbs_cv_move().map_or(false, |t| t.free_mode));
        if !gizmo_visible { return; }
        let in_poly_edit = self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object;
        let has_poly_sel = in_poly_edit && self.poly_selection_center().is_some();
        let has_nurbs_sel = self.modeling_context == ModelingContext::Nurbs
            && self.nurbs_selection_center().is_some();
        if !has_poly_sel && !has_nurbs_sel {
            let has_sel = self.sel.usel.has_prims() || self.sel.usel.has_nodes() || self.sel.usel.has_lights();
            if !has_sel { return; }
            // Gizmo allowed in Object mode (bodies, lights) and Vertex mode (prims, bodies)
            if self.tool.work_mode != WorkMode::Object && self.tool.work_mode != WorkMode::Vertex { return; }
        }
        // Hide gizmo while creating a primitive or during active mesh operations
        if self.tool.current.is_create() || self.tool.current.is_busy() { return; }
        // Action Center (T pie) supplies pivot center + axes when idle; freezes mid-drag.
        let frame = self.resolve_action_frame();
        let gp = if self.tool.current.is_gizmo() {
            self.tool.current.as_gizmo().map(|g| g.median_start_pos).unwrap_or(frame.center)
        } else if let Some(ts) = self.tool.current.as_poly_transform() {
            ts.center
        } else if let Some(d) = self.tool.current.as_nurbs_cv_move() {
            d.center
        } else {
            frame.center
        };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit*Vec3::new(0.0,0.0,self.viewport.camera.distance);
        let cam_fwd = (self.viewport.camera.orbit*Vec3::NEG_Z).normalize();
        let aspect = rect.width()/rect.height();
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let Some(center) = world_to_screen(gp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { return };
        let dist = (gp-cam_pos).length();
        let gwl = dist * GIZMO_ARROW_PX / (rect.height()*0.5);
        // Frozen during active gizmo / modal G/R/S so arrows match Local drag math after rotation.
        let obj_rot = self.gizmo_draw_orient();
        let draw_space = self.gizmo_draw_space();

        // Smart rotation: detect perpendicular axis for center hover/drag highlight
        let perp = perpendicular_axis(cam_fwd);
        let center_active = self.tool.gizmo_hovered == Some(GizmoComponent::Center)
            || self.tool.current.as_gizmo().map_or(false, |g| g.component == GizmoComponent::Center);

        for comp in GizmoComponent::all_axes() {
            let axis_dir = get_gizmo_axis(comp, draw_space, obj_rot, cam_fwd);
            let end_world = gp + axis_dir * gwl;
            let Some(end_s) = world_to_screen(end_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
            let hov = self.tool.gizmo_hovered==Some(comp);
            let drag = self.tool.current.as_gizmo().map_or(false,|g|g.component==comp);
            // In Rotate mode: highlight perpendicular axis when center is hovered/dragged
            let smart_highlight = self.tool.gizmo_mode == GizmoMode::Rotate && center_active && comp == perp;
            let col = gizmo_color(comp, hov||drag||smart_highlight);
            let w = if hov||drag||smart_highlight{GIZMO_HOVER_WIDTH}else{GIZMO_LINE_WIDTH};

            match self.tool.gizmo_mode {
                GizmoMode::Translate => {
                    painter.line_segment([center,end_s], egui::Stroke::new(w,col));
                    let dir=egui::vec2(end_s.x-center.x, end_s.y-center.y);
                    let l=(dir.x*dir.x+dir.y*dir.y).sqrt();
                    if l>1.0 { let d=egui::vec2(dir.x/l,dir.y/l); let p=egui::vec2(-d.y,d.x);
                        painter.add(egui::Shape::convex_polygon(vec![end_s,egui::pos2(end_s.x-d.x*10.0+p.x*5.0,end_s.y-d.y*10.0+p.y*5.0),egui::pos2(end_s.x-d.x*10.0-p.x*5.0,end_s.y-d.y*10.0-p.y*5.0)],col,egui::Stroke::NONE));
                    }
                }
                GizmoMode::Scale => {
                    painter.line_segment([center,end_s], egui::Stroke::new(w,col));
                    painter.rect_filled(egui::Rect::from_center_size(end_s,egui::vec2(10.0,10.0)),0.0,col);
                }
                GizmoMode::Elongate => {
                    // Elongate mode draws face dots separately below — skip per-axis here
                }
                GizmoMode::Rotate => {
                    // 3D Ring
                    let rr = gwl * 0.85;
                    let p1=if axis_dir.y.abs()>0.9{Vec3::X}else{Vec3::Y}.cross(axis_dir).normalize();
                    let p2=axis_dir.cross(p1).normalize();
                    let mut prev_s:Option<egui::Pos2>=None;
                    for i in 0..=48 {
                        let a=(i as f32/48.0)*std::f32::consts::TAU;
                        let pt=gp+(p1*a.cos()+p2*a.sin())*rr;
                        if let Some(sp)=world_to_screen(pt,cam_pos,self.viewport.camera.orbit,rect,aspect,ortho_hw,self.render.fov_deg) {
                            if let Some(pv)=prev_s { painter.line_segment([pv,sp],egui::Stroke::new(w,col)); }
                            prev_s=Some(sp);
                        } else { prev_s=None; }
                    }
                }
            }
            painter.text(egui::pos2(end_s.x+8.0,end_s.y-8.0),egui::Align2::LEFT_BOTTOM,comp.label(),egui::FontId::proportional(11.0),col);
        }

        // Center dot — in Rotate mode, tint with perpendicular axis color when hovered
        let ch=self.tool.gizmo_hovered==Some(GizmoComponent::Center);
        let cd=self.tool.current.as_gizmo().map_or(false,|g|g.component==GizmoComponent::Center);
        let cc = if self.tool.gizmo_mode == GizmoMode::Rotate && (ch || cd) {
            gizmo_color(perp, true) // tint center with perpendicular axis color
        } else {
            gizmo_color(GizmoComponent::Center, ch || cd)
        };
        let cr=if ch||cd{GIZMO_CENTER_RADIUS+2.0}else{GIZMO_CENTER_RADIUS};
        painter.circle_filled(center,cr,egui::Color32::from_rgba_unmultiplied(40,40,40,120));
        painter.circle_stroke(center,cr,egui::Stroke::new(2.0,cc));
        painter.circle_filled(center,3.0,cc);

        // Reset handle — small filled dot at center, visible in Rotate/Scale modes
        if self.viewport.show_reset_handle && self.tool.gizmo_mode != GizmoMode::Translate {
            let reset_hov = self.tool.gizmo_hovered == Some(GizmoComponent::Center)
                && !self.tool.current.is_gizmo(); // only show reset style when not already dragging
            let reset_r = if reset_hov { 6.0 } else { 5.0 };
            let reset_col = if reset_hov {
                egui::Color32::from_rgb(255, 200, 60) // amber on hover
            } else {
                egui::Color32::from_rgba_unmultiplied(255, 200, 60, 140)
            };
            painter.circle_filled(center, reset_r, reset_col);
            if reset_hov {
                painter.circle_stroke(center, reset_r, egui::Stroke::new(1.0, egui::Color32::WHITE));
                let label = match self.tool.gizmo_mode {
                    GizmoMode::Rotate => "Click: Reset Rotation",
                    GizmoMode::Scale    => "Click: Reset Scale",
                    GizmoMode::Elongate => "Click: Reset Elongation",
                    _ => "",
                };
                painter.text(egui::pos2(center.x, center.y - cr - 18.0), egui::Align2::CENTER_BOTTOM,
                    label, egui::FontId::proportional(10.0), reset_col);
            }
        }

        // Elongate mode: draw 6 face dots on bounding box + center scale dot
        if self.tool.gizmo_mode == GizmoMode::Elongate {
            if let Some(idx) = self.active_prim_index_unified() {
                if idx < self.scene.primitives.len() {
                    let prim = &self.scene.primitives[idx];
                    let he = shape_half_extent(prim) + prim.elongation;
                    let prim_rot = prim.rotation_quat();
                    // Get prim world position
                    let prim_world = if BODY_LOCAL_PRIMS {
                        self.scene.prim_body_map.get(idx)
                            .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                            .unwrap_or_default()
                            .local_to_world(prim.position)
                    } else { prim.position };
                    let faces: [(GizmoComponent, bool, Vec3); 6] = [
                        (GizmoComponent::AxisX, false, Vec3::new( he.x * prim.scale.x, 0.0, 0.0)),
                        (GizmoComponent::AxisX, true,  Vec3::new(-he.x * prim.scale.x, 0.0, 0.0)),
                        (GizmoComponent::AxisY, false, Vec3::new(0.0,  he.y * prim.scale.y, 0.0)),
                        (GizmoComponent::AxisY, true,  Vec3::new(0.0, -he.y * prim.scale.y, 0.0)),
                        (GizmoComponent::AxisZ, false, Vec3::new(0.0, 0.0,  he.z * prim.scale.z)),
                        (GizmoComponent::AxisZ, true,  Vec3::new(0.0, 0.0, -he.z * prim.scale.z)),
                    ];
                    for &(comp, neg, local_offset) in &faces {
                        let world_pt = prim_world + prim_rot * local_offset;
                        let Some(sp) = world_to_screen(world_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
                        let hov = self.tool.gizmo_hovered == Some(comp) && self.tool.elongate_face_negative == neg;
                        let drag = self.tool.current.as_gizmo().map_or(false, |g| g.component == comp && g.elongate_face_negative == neg);
                        let col = gizmo_color(comp, hov || drag);
                        let r = if hov || drag { 8.0 } else { 6.0 };
                        painter.circle_filled(sp, r, col);
                        if hov || drag {
                            painter.circle_stroke(sp, r, egui::Stroke::new(1.5, egui::Color32::WHITE));
                        }
                    }
                }
            }
        }

        // Space label + smart axis hint
        let sl = self.tool.action_center.label();
        painter.text(egui::pos2(center.x,center.y+cr+14.0),egui::Align2::CENTER_TOP,sl,egui::FontId::proportional(10.0),egui::Color32::from_gray(140));
        if self.tool.gizmo_mode == GizmoMode::Rotate && center_active {
            let axis_lbl = perp.label();
            let axis_col = gizmo_color(perp, true);
            painter.text(egui::pos2(center.x, center.y - cr - 6.0), egui::Align2::CENTER_BOTTOM,
                format!("Smart {}", axis_lbl), egui::FontId::proportional(10.0), axis_col);
        }
    }

    // ================================================================
    // PRO GIZMO: Hit Test (rings for Rotate, center dot, line segments)
    // ================================================================
    pub(crate) fn gizmo_hit_test(&self, mouse:egui::Pos2, rect:egui::Rect) -> Option<GizmoComponent> {
        // Only allow gizmo hit when armed or during active (non-free) transform
        let gizmo_active = self.tool.gizmo_armed
            || self.tool.current.is_gizmo()
            || (self.tool.current.is_transform() && !self.tool.current.as_transform().map_or(false, |t| t.free_mode))
            || (self.tool.current.is_poly_transform() && !self.tool.current.as_poly_transform().map_or(false, |t| t.free_mode));
        if !gizmo_active { return None; }
        let in_poly_edit = self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object;
        let has_poly_sel = in_poly_edit && self.poly_selection_center().is_some();
        if !has_poly_sel {
            if (self.tool.work_mode!=WorkMode::Object && self.tool.work_mode!=WorkMode::Vertex) || (!self.sel.usel.has_prims() && !self.sel.usel.has_nodes() && !self.sel.usel.has_lights()) { return None; }
        }
        // Must match draw + G/R/S: Action Center (incl. Automatic Shift+LMB pivot), not raw median.
        let gp = self.resolve_action_frame().center;
        let cam_pos=self.viewport.camera.target+self.viewport.camera.orbit*Vec3::new(0.0,0.0,self.viewport.camera.distance);
        let cam_fwd=(self.viewport.camera.orbit*Vec3::NEG_Z).normalize();
        let aspect=rect.width()/rect.height();
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let Some(center)=world_to_screen(gp,cam_pos,self.viewport.camera.orbit,rect,aspect,ortho_hw,self.render.fov_deg) else{return None};
        let dist=(gp-cam_pos).length();
        let gwl=dist*GIZMO_ARROW_PX/(rect.height()*0.5);
        let obj_rot=self.gizmo_draw_orient();
        let draw_space=self.gizmo_draw_space();

        // Center first (highest priority)
        if (mouse-center).length() < GIZMO_CENTER_RADIUS+4.0 { return Some(GizmoComponent::Center); }

        // Elongate mode: hit test 6 face dots instead of axis lines
        if self.tool.gizmo_mode == GizmoMode::Elongate {
            return self.elongate_face_hit_test(mouse, rect, cam_pos, aspect, ortho_hw);
        }

        let mut best:Option<(GizmoComponent,f32)>=None;
        for comp in GizmoComponent::all_axes() {
            let axis_dir=get_gizmo_axis(comp,draw_space,obj_rot,cam_fwd);
            let d = if self.tool.gizmo_mode==GizmoMode::Rotate {
                dist_point_to_ring(mouse,gp,axis_dir,gwl*0.85,cam_pos,self.viewport.camera.orbit,rect,aspect,ortho_hw,self.render.fov_deg)
            } else {
                let end=gp+axis_dir*gwl;
                let Some(es)=world_to_screen(end,cam_pos,self.viewport.camera.orbit,rect,aspect,ortho_hw,self.render.fov_deg) else{continue};
                dist_point_to_segment(mouse,center,es)
            };
            if d<GIZMO_HIT_RADIUS { if best.is_none()||d<best.unwrap().1 { best=Some((comp,d)); } }
        }
        best.map(|(c,_)|c)
    }

    /// Hit test the 6 bounding-box face dots + center for Elongate mode.
    /// Returns (component, is_negative_face). Caller must set `elongate_face_negative`.
    fn elongate_face_hit_test(&self, mouse: egui::Pos2, rect: egui::Rect, cam_pos: Vec3, aspect: f32, ortho_hw: f32) -> Option<GizmoComponent> {
        let idx = self.active_prim_index_unified()?;
        if idx >= self.scene.primitives.len() { return None; }
        let prim = &self.scene.primitives[idx];
        let he = shape_half_extent(prim) + prim.elongation;
        let prim_rot = prim.rotation_quat();
        let prim_world = if BODY_LOCAL_PRIMS {
            self.scene.prim_body_map.get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .unwrap_or_default()
                .local_to_world(prim.position)
        } else { prim.position };

        // Center dot first (highest priority)
        if let Some(cp) = world_to_screen(prim_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            if (mouse - cp).length() < GIZMO_CENTER_RADIUS + 4.0 {
                return Some(GizmoComponent::Center);
            }
        }

        let faces: [(GizmoComponent, bool, Vec3); 6] = [
            (GizmoComponent::AxisX, false, Vec3::new( he.x * prim.scale.x, 0.0, 0.0)),
            (GizmoComponent::AxisX, true,  Vec3::new(-he.x * prim.scale.x, 0.0, 0.0)),
            (GizmoComponent::AxisY, false, Vec3::new(0.0,  he.y * prim.scale.y, 0.0)),
            (GizmoComponent::AxisY, true,  Vec3::new(0.0, -he.y * prim.scale.y, 0.0)),
            (GizmoComponent::AxisZ, false, Vec3::new(0.0, 0.0,  he.z * prim.scale.z)),
            (GizmoComponent::AxisZ, true,  Vec3::new(0.0, 0.0, -he.z * prim.scale.z)),
        ];
        let tol = GIZMO_HIT_RADIUS + 2.0;
        let mut best: Option<(GizmoComponent, bool, f32)> = None;
        for &(comp, neg, local_offset) in &faces {
            let world_pt = prim_world + prim_rot * local_offset;
            let Some(sp) = world_to_screen(world_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
            let d = (mouse - sp).length();
            if d < tol {
                if best.is_none() || d < best.unwrap().2 { best = Some((comp, neg, d)); }
            }
        }
        // Note: caller must read `best_neg` from a side channel. We use a workaround:
        // store the result in self via interior mutability pattern. But since we're &self,
        // we return the component and the caller queries elongate_face_negative separately.
        // The actual face_negative is set by the mutable caller in input_viewport.
        best.map(|(c, _, _)| c)
    }

    /// Compute which face (positive/negative) the mouse is hovering in Elongate mode.
    /// Returns (component, is_negative). Called from mutable context to set tool state.
    pub(crate) fn elongate_face_hover(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<(GizmoComponent, bool)> {
        let idx = self.active_prim_index_unified()?;
        if idx >= self.scene.primitives.len() { return None; }
        let prim = &self.scene.primitives[idx];
        let he = shape_half_extent(prim) + prim.elongation;
        let prim_rot = prim.rotation_quat();
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height();
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let prim_world = if BODY_LOCAL_PRIMS {
            self.scene.prim_body_map.get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .unwrap_or_default()
                .local_to_world(prim.position)
        } else { prim.position };

        // Center
        if let Some(cp) = world_to_screen(prim_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            if (mouse - cp).length() < GIZMO_CENTER_RADIUS + 4.0 {
                return Some((GizmoComponent::Center, false));
            }
        }

        let faces: [(GizmoComponent, bool, Vec3); 6] = [
            (GizmoComponent::AxisX, false, Vec3::new( he.x * prim.scale.x, 0.0, 0.0)),
            (GizmoComponent::AxisX, true,  Vec3::new(-he.x * prim.scale.x, 0.0, 0.0)),
            (GizmoComponent::AxisY, false, Vec3::new(0.0,  he.y * prim.scale.y, 0.0)),
            (GizmoComponent::AxisY, true,  Vec3::new(0.0, -he.y * prim.scale.y, 0.0)),
            (GizmoComponent::AxisZ, false, Vec3::new(0.0, 0.0,  he.z * prim.scale.z)),
            (GizmoComponent::AxisZ, true,  Vec3::new(0.0, 0.0, -he.z * prim.scale.z)),
        ];
        let tol = GIZMO_HIT_RADIUS + 2.0;
        let mut best: Option<(GizmoComponent, bool, f32)> = None;
        for &(comp, neg, local_offset) in &faces {
            let world_pt = prim_world + prim_rot * local_offset;
            let Some(sp) = world_to_screen(world_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
            let d = (mouse - sp).length();
            if d < tol {
                if best.is_none() || d < best.unwrap().2 { best = Some((comp, neg, d)); }
            }
        }
        best.map(|(c, neg, _)| (c, neg))
    }

    // ---- Falloff graph ----
    pub(crate) fn draw_falloff_graph(&self, ui:&mut egui::Ui) {
        let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width().min(180.0),50.0),egui::Sense::hover());
        let p=ui.painter_at(rect);
        p.rect_filled(rect,2.0,egui::Color32::from_gray(35)); p.rect_stroke(rect,2.0,egui::Stroke::new(1.0,egui::Color32::from_gray(60)), egui::StrokeKind::Middle);
        let mut prev:Option<egui::Pos2>=None;
        for i in 0..=40 { let t=i as f32/40.0; let v=self.brush.falloff.evaluate(t);
            let pt=egui::pos2(rect.min.x+t*rect.width(),rect.max.y-v*rect.height());
            if let Some(pv)=prev{p.line_segment([pv,pt],egui::Stroke::new(1.5,egui::Color32::from_rgb(200,160,60)));}prev=Some(pt);
        }
        p.text(egui::pos2(rect.min.x+3.0,rect.min.y+2.0),egui::Align2::LEFT_TOP,self.brush.falloff.label(),egui::FontId::proportional(10.0),egui::Color32::from_gray(150));
    }
}
