// draw_workplane.rs — Work plane overlay drawing
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use crate::*;

impl AzraApp {
    // ---- Work Plane overlay (thinner, more transparent than grid) ----
    /// The workplane sits at its DEFAULT placement when it is auto-oriented (never Shift-click
    /// "set") and its origin is still at the world floor. In that state the depth-correct GPU floor
    /// grid already represents the ground, so the always-on-top egui overlay is redundant clutter
    /// (owner: "renders on top of geo, looks bad"). Only a custom placement — Locked (Shift-click)
    /// or a moved origin — earns the on-top overlay so the user can see where they put it.
    pub(crate) fn workplane_is_at_default(&self) -> bool {
        self.viewport.wp_mode == WorkPlaneMode::Auto
            && self.viewport.wp_settings.origin.abs_diff_eq(Vec3::ZERO, 1e-4)
    }

    pub(crate) fn draw_workplane_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.viewport.wp_settings.opacity < 0.01 { return; }
        // At the default placement the overlay would just re-draw the floor grid, on top of every
        // object. Skip it there; render on top only once the plane has been explicitly placed.
        if self.workplane_is_at_default() { return; }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let wp = self.viewport.wp_resolved;
        let spacing = self.viewport.wp_settings.spacing.max(0.1);
        let alpha = (self.viewport.wp_settings.opacity * 255.0).round() as u8;
        let [cr, cg, cb] = self.viewport.wp_settings.color;
        let line_col = egui::Color32::from_rgba_unmultiplied(
            (cr * 255.0) as u8, (cg * 255.0) as u8, (cb * 255.0) as u8, alpha,
        );

        let extent = self.viewport.camera.distance * 3.0;
        let max_lines = 150i32;
        let n = ((extent / spacing).ceil() as i32).min(max_lines);

        // Helper: draw a line with distance-based fade + near-plane clipping
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();

        // EDGE-ON FADE. Viewed edge-on, EVERY grid line projects onto the same screen line, so the
        // whole plane renders as one hairline streak across the viewport — read as a stray-geometry
        // artifact (owner 2026-08-02, reported on polygroups and on a selected area, which is where
        // the auto workplane aligns itself to the selection and goes edge-on). Blender fades its
        // grid the same way. Below ~7° the plane carries no information at all: skip it.
        let edge_on = {
            let nrm = wp.normal.normalize_or_zero();
            if nrm.length_squared() < 0.5 {
                1.0
            } else {
                // |n·view| = 1 face-on, 0 edge-on.
                let facing = nrm.dot(cam_fwd).abs();
                if facing < 0.12 {
                    return;
                }
                ((facing - 0.12) / 0.18).clamp(0.0, 1.0)
            }
        };
        let line_col = egui::Color32::from_rgba_unmultiplied(
            line_col.r(),
            line_col.g(),
            line_col.b(),
            ((line_col.a() as f32) * edge_on).round() as u8,
        );
        let draw_line = |p: &egui::Painter, w0: Vec3, w1: Vec3, col: egui::Color32, width: f32| {
            // Near-plane clip: instead of dropping lines with endpoints behind camera,
            // clip them to the near plane so grid lines don't vanish at steep angles.
            let d0 = (w0 - cam_pos).dot(cam_fwd);
            let d1 = (w1 - cam_pos).dot(cam_fwd);
            let near = 0.15;
            let (cw0, cw1) = if d0 > near && d1 > near {
                (w0, w1)
            } else if d0 <= near && d1 <= near {
                return; // both behind camera
            } else {
                let t = (near - d0) / (d1 - d0);
                let mid = w0 + (w1 - w0) * t;
                if d0 <= near { (mid, w1) } else { (w0, mid) }
            };
            let s0 = world_to_screen(cw0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
            let s1 = world_to_screen(cw1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
            if let (Some(a), Some(b)) = (s0, s1) {
                let mid = (w0 + w1) * 0.5;
                let dist = (mid - cam_pos).length();
                let fade = (1.0 - dist / (extent * 1.5)).clamp(0.0, 1.0);
                if fade < 0.02 { return; }
                let faded_alpha = ((col.a() as f32) * fade * fade).round() as u8;
                let fc = egui::Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), faded_alpha);
                p.line_segment([a, b], egui::Stroke::new(width, fc));
            }
        };

        // Grid lines along tangent_u (constant v offset)
        for i in -n..=n {
            let offset = wp.origin + wp.tangent_v * (i as f32 * spacing);
            let a = offset - wp.tangent_u * extent;
            let b = offset + wp.tangent_u * extent;
            draw_line(painter, a, b, line_col, 0.5);
        }
        // Grid lines along tangent_v (constant u offset)
        for i in -n..=n {
            let offset = wp.origin + wp.tangent_u * (i as f32 * spacing);
            let a = offset - wp.tangent_v * extent;
            let b = offset + wp.tangent_v * extent;
            draw_line(painter, a, b, line_col, 0.5);
        }

        // Origin axis indicator (short colored lines)
        let axis_len = spacing * 1.5;
        let u_col = egui::Color32::from_rgba_unmultiplied(240, 80, 80, alpha.min(200));   // red = U
        let v_col = egui::Color32::from_rgba_unmultiplied(80, 220, 80, alpha.min(200));   // green = V
        let n_col = egui::Color32::from_rgba_unmultiplied(80, 140, 240, alpha.min(200));  // blue = normal
        draw_line(painter, wp.origin, wp.origin + wp.tangent_u * axis_len, u_col, 2.0);
        draw_line(painter, wp.origin, wp.origin + wp.tangent_v * axis_len, v_col, 2.0);
        draw_line(painter, wp.origin, wp.origin + wp.normal * (axis_len * 0.7), n_col, 1.5);

        // Locked mode: draw a small diamond at origin
        if self.viewport.wp_mode == WorkPlaneMode::Locked {
            if let Some(sp) = world_to_screen(wp.origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let d = 5.0;
                let pts = [
                    egui::pos2(sp.x, sp.y - d),
                    egui::pos2(sp.x + d, sp.y),
                    egui::pos2(sp.x, sp.y + d),
                    egui::pos2(sp.x - d, sp.y),
                ];
                let locked_col = egui::Color32::from_rgb(100, 200, 255);
                painter.line_segment([pts[0], pts[1]], egui::Stroke::new(1.5, locked_col));
                painter.line_segment([pts[1], pts[2]], egui::Stroke::new(1.5, locked_col));
                painter.line_segment([pts[2], pts[3]], egui::Stroke::new(1.5, locked_col));
                painter.line_segment([pts[3], pts[0]], egui::Stroke::new(1.5, locked_col));
            }
        }
    }
}
