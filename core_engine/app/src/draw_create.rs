// draw_create.rs — Create tool overlay, HUD, dimensions, active tool HUD
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_create_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let color = cs.op.color();
        let fill_alpha = 30u8;
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let stroke = egui::Stroke::new(2.0, color);
        if cs.phase == CreatePhase::Footprint {
            let (a, n) = (cs.anchor, cs.hit_normal);
            let (lu, lv) = (cs.local_u, cs.local_v);
            let offset = n * 0.002;
            // Apply rotation to tangent frame
            let rot = glam::Quat::from_axis_angle(n, cs.rotation_angle.to_radians());
            let tu = rot * cs.tangent_u;
            let tv = rot * cs.tangent_v;

            // ALL shapes use rectangular footprint (consistent drag behavior)
            {
                let c0 = a + offset;
                let c1 = a + tu * lu + offset;
                let c2 = a + tu * lu + tv * lv + offset;
                let c3 = a + tv * lv + offset;
                let pts: Vec<Option<egui::Pos2>> = [c0, c1, c2, c3].iter()
                    .map(|&wp| world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg))
                    .collect();
                let edges = [(0,1),(1,2),(2,3),(3,0)];
                for (a, b) in edges {
                    if let (Some(sa), Some(sb)) = (pts[a], pts[b]) {
                        painter.line_segment([sa, sb], stroke);
                    }
                }
                let fill_pts: Vec<egui::Pos2> = pts.iter().filter_map(|p| *p).collect();
                if fill_pts.len() == 4 {
                    painter.add(egui::Shape::convex_polygon(
                        fill_pts,
                        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), fill_alpha),
                        egui::Stroke::NONE,
                    ));
                }
            }
            // Anchor dot
            if let Some(sp) = world_to_screen(a + offset, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                painter.circle_filled(sp, 4.0, color);
            }
        } else {
            // Extrude phase: no wireframe overlay — let the user see the true
            // SDF volume so there's no perceptual "shrink" when they commit.
        }
    }

    pub(crate) fn draw_create_hud(&self, painter: &egui::Painter, mouse: egui::Pos2) {
        let cs = match self.tool.current.as_create() {
            Some(cs) => cs,
            None => return,
        };

        let op_label = cs.op.label();
        let op_color = cs.op.color();
        let phase_text = match cs.phase {
            CreatePhase::Footprint => "[Shift] Square  [Ctrl+Shift] Center  [Alt] Rotate  [Scroll] Bevel  [B] Chamfer  [C] Infinite",
            CreatePhase::Extrude => "[Shift] Symmetric  [Alt] Rotate  [X] Cycle Op  [J] Join  [Alt+Scroll] Blend  [Click] Confirm",
        };
        let angle_text = if cs.rotation_angle.abs() > 0.1 {
            format!("  angle: {:.1}\u{00b0}", cs.rotation_angle)
        } else { String::new() };
        let extra = if cs.phase == CreatePhase::Extrude {
            format!("  depth: {:.3}  k: {:.2}  rv: {:.2}  rtb: {:.2}{}", cs.depth, cs.blend_k, cs.corner_radius, cs.corner_radius_tb, angle_text)
        } else {
            let w = cs.local_u.abs();
            let d = cs.local_v.abs();
            format!("  W: {:.2}  D: {:.2}  r: {:.2}{}", w, d, cs.corner_radius, angle_text)
        };

        let y_off = 28.0;
        painter.text(egui::pos2(mouse.x, mouse.y + y_off), egui::Align2::CENTER_TOP,
            op_label, egui::FontId::proportional(14.0), op_color);
        let hint_y = y_off + 18.0;
        painter.text(egui::pos2(mouse.x, mouse.y + hint_y), egui::Align2::CENTER_TOP,
            &format!("{}{}", phase_text, extra),
            egui::FontId::proportional(11.0), egui::Color32::from_white_alpha(160));

        if !cs.num_buf.is_empty() {
            painter.text(egui::pos2(mouse.x, mouse.y + hint_y + 16.0), egui::Align2::CENTER_TOP,
                &format!("Depth: {}_", cs.num_buf),
                egui::FontId::proportional(13.0), egui::Color32::from_rgb(255, 220, 100));
        }
    }

    /// Draw dimension lines near the footprint rectangle or extrude height.
    pub(crate) fn draw_create_dimensions(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let dim_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180);
        let dim_font = egui::FontId::proportional(11.0);
        let dim_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120));

        let rot = glam::Quat::from_axis_angle(cs.hit_normal, cs.rotation_angle.to_radians());
        let tu = rot * cs.tangent_u;
        let tv = rot * cs.tangent_v;
        let (a, n) = (cs.anchor, cs.hit_normal);
        let offset = n * 0.002;

        if cs.phase == CreatePhase::Footprint {
            let (lu, lv) = (cs.local_u, cs.local_v);
            if lu.abs() < 0.01 && lv.abs() < 0.01 { return; }
            // Width dimension along tu (bottom edge, offset outward along -tv)
            let margin = tv * (-lv.signum() * 0.05);
            let w0 = a + offset + margin;
            let w1 = a + tu * lu + offset + margin;
            if let (Some(s0), Some(s1)) = (
                world_to_screen(w0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                world_to_screen(w1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            ) {
                painter.line_segment([s0, s1], dim_stroke);
                let mid = egui::pos2((s0.x + s1.x) * 0.5, (s0.y + s1.y) * 0.5);
                painter.text(mid + egui::vec2(0.0, -10.0), egui::Align2::CENTER_BOTTOM,
                    format!("{:.3}", lu.abs()), dim_font.clone(), dim_color);
            }
            // Depth dimension along tv (left edge, offset outward along -tu)
            let margin_u = tu * (-lu.signum() * 0.05);
            let d0 = a + offset + margin_u;
            let d1 = a + tv * lv + offset + margin_u;
            if let (Some(s0), Some(s1)) = (
                world_to_screen(d0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                world_to_screen(d1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            ) {
                painter.line_segment([s0, s1], dim_stroke);
                let mid = egui::pos2((s0.x + s1.x) * 0.5, (s0.y + s1.y) * 0.5);
                painter.text(mid + egui::vec2(-10.0, 0.0), egui::Align2::RIGHT_CENTER,
                    format!("{:.3}", lv.abs()), dim_font.clone(), dim_color);
            }
        } else {
            // Extrude phase: show height dimension along extrude axis
            let (lu, lv) = (cs.local_u, cs.local_v);
            let draw_center = a + cs.tangent_u * (lu * 0.5) + cs.tangent_v * (lv * 0.5);
            let base = draw_center + offset;
            let top = base + n * cs.depth;
            // Offset the dimension line to the side
            let side_offset = tu * (lu.abs() * 0.5 + 0.05);
            let h0 = base + side_offset;
            let h1 = top + side_offset;
            if let (Some(s0), Some(s1)) = (
                world_to_screen(h0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                world_to_screen(h1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            ) {
                painter.line_segment([s0, s1], dim_stroke);
                let mid = egui::pos2((s0.x + s1.x) * 0.5, (s0.y + s1.y) * 0.5);
                painter.text(mid + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER,
                    format!("{:.3}", cs.depth.abs()), dim_font.clone(), dim_color);
            }
        }
    }

    /// Draw HUD when a tool is armed but not actively drawing.
    pub(crate) fn draw_active_tool_hud(&self, painter: &egui::Painter, rect: egui::Rect) {
        let si = match self.tool.mode.armed_shape_idx() { Some(si) => si, None => return };
        if self.tool.current.is_create() { return; } // drawing HUD handles this instead

        let shape = Self::shape_preset(si);
        let shape_name = shape.label();
        let op_label = self.tool.create_op.label();
        let op_color = self.tool.create_op.color();

        let cx = rect.center().x;
        let y = rect.min.y + 38.0;

        painter.text(egui::pos2(cx, y), egui::Align2::CENTER_TOP,
            &format!("{} — {}", shape_name, op_label),
            egui::FontId::proportional(14.0), op_color);
        painter.text(egui::pos2(cx, y + 18.0), egui::Align2::CENTER_TOP,
            "Click+drag to draw | X = cycle op | J = join | Q = drop tool",
            egui::FontId::proportional(11.0), egui::Color32::from_white_alpha(130));
    }
}
