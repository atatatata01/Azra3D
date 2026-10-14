// draw_light.rs — Light viewport gizmos, volumetric/range handles, hit test
// Extracted from drawing.rs — Unreal-style spatial light controls.
use eframe::egui;
use glam::Vec3;
use app_types::LightHandleKind;
use crate::*;

impl AzraApp {
    /// True when a light's gizmo is hidden behind scene geometry. Used for draw + pick so icons
    /// don't float on top of faces.
    ///
    /// This is a DEPTH TEST, resolved on the GPU: `poll_light_depth_probe` samples the raster
    /// depth buffer at each light's own texel and leaves the verdict in `light_occluded`. It used
    /// to be `raycast_scene_surface` — a full-scene CPU raycast per light per frame, which cost
    /// **31 ms of a 33 ms frame** with ONE light on a 323k-tri body, because it scaled with
    /// triangles rather than lights (VIEWPORT_PERF_2026-08-12 item A).
    ///
    /// Absent verdict ⇒ NOT occluded. The probe fails open on purpose: no depth buffer yet, light
    /// off-screen, first frame, failed map. A stray icon is recoverable; a light the user cannot
    /// find is not.
    fn light_gizmo_occluded(&self, light_id: u32) -> bool {
        self.light_occluded.get(&light_id).copied().unwrap_or(false)
    }

    // ---- Light viewport gizmos ----
    pub(crate) fn draw_light_gizmos(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        // Resolve BOTH selection forms once: a light picked in the viewport arrives as
        // SceneItemId::Light, one picked in the outliner as SceneItemId::Node. Testing only
        // the former made the range sphere + handles vanish for outliner-selected lights.
        let selected_light_ids = self.selected_light_ids();
        for (nid, light) in self.scene.scene_graph.lights() {
            if !light.visible { continue; }
            let sel = selected_light_ids.contains(&light.id)
                || self.sel.usel.selected.contains(&SceneItemId::Node(nid));
            let gp = light.gizmo_position();
            // Depth occlusion — skip icon + handles when a face is in front of the light.
            if self.light_gizmo_occluded(light.id) {
                continue;
            }
            let Some(center) = world_to_screen(gp, cam_pos, orbit, rect, aspect, ortho_hw, fov) else { continue };

            let base_col = if sel { egui::Color32::from_rgb(255, 210, 70) } else { egui::Color32::from_rgb(220, 190, 90) };
            let fill_alpha = if sel { 200u8 } else { 110 };
            // Larger discs so lights stay readable at typical orbit distances.
            let disc_r = if sel { 18.0 } else { 13.0 };
            let line_w = if sel { 2.8 } else { 1.8 };
            // Soft halo behind the icon (selected lights especially).
            if sel {
                painter.circle_filled(center, disc_r + 8.0,
                    egui::Color32::from_rgba_unmultiplied(255, 200, 60, 40));
            }
            let hover = self.tool.light_handle_hovered.filter(|(id, _)| *id == light.id).map(|(_, k)| k);
            let dragging = self.tool.light_handle_drag.as_ref().filter(|d| d.light_id == light.id).map(|d| d.kind);

            match light.light_type {
                LightType::Directional => {
                    painter.circle_filled(center, disc_r,
                        egui::Color32::from_rgba_unmultiplied(255, 220, 100, fill_alpha));
                    painter.circle_stroke(center, disc_r, egui::Stroke::new(line_w, base_col));
                    for i in 0..8 {
                        let a = (i as f32 / 8.0) * std::f32::consts::TAU;
                        let inner = disc_r + 3.0;
                        let outer = disc_r + 9.0;
                        let p0 = egui::pos2(center.x + a.cos() * inner, center.y + a.sin() * inner);
                        let p1 = egui::pos2(center.x + a.cos() * outer, center.y + a.sin() * outer);
                        painter.line_segment([p0, p1], egui::Stroke::new(line_w * 0.8, base_col));
                    }
                    if sel {
                        // Aim arrow from fixed sun position along direction (Blender sun:
                        // rotate aims only — icon stays put).
                        let d = light.direction();
                        let dir = Vec3::new(d[0], d[1], d[2]);
                        let arrow_len = 5.0;
                        let end_w = gp + dir * arrow_len;
                        if let Some(end_s) = world_to_screen(end_w, cam_pos, orbit, rect, aspect, ortho_hw, fov) {
                            painter.line_segment([center, end_s], egui::Stroke::new(line_w, base_col));
                            let arrow_vec = end_s - center;
                            if arrow_vec.length() > 10.0 {
                                let dir2d = arrow_vec / arrow_vec.length();
                                let perp = egui::vec2(-dir2d.y, dir2d.x);
                                let wing = 8.0;
                                painter.line_segment([end_s, end_s - dir2d * wing + perp * wing * 0.4],
                                    egui::Stroke::new(line_w, base_col));
                                painter.line_segment([end_s, end_s - dir2d * wing - perp * wing * 0.4],
                                    egui::Stroke::new(line_w, base_col));
                            }
                        }
                    }
                }
                LightType::Point => {
                    painter.circle_filled(center, disc_r,
                        egui::Color32::from_rgba_unmultiplied(255, 220, 100, fill_alpha));
                    painter.circle_stroke(center, disc_r, egui::Stroke::new(line_w, base_col));
                    let cr = disc_r * 0.5;
                    painter.line_segment(
                        [egui::pos2(center.x - cr, center.y), egui::pos2(center.x + cr, center.y)],
                        egui::Stroke::new(1.5, base_col));
                    painter.line_segment(
                        [egui::pos2(center.x, center.y - cr), egui::pos2(center.x, center.y + cr)],
                        egui::Stroke::new(1.5, base_col));

                    if sel {
                        // Amber illumination RANGE — 3-plane wire sphere (Unreal-style).
                        let range_hot = hover == Some(LightHandleKind::Range)
                            || dragging == Some(LightHandleKind::Range);
                        let range_col = if range_hot {
                            egui::Color32::from_rgb(255, 230, 120)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(255, 200, 60, 90)
                        };
                        self.draw_wire_sphere(painter, rect, cam_pos, aspect, ortho_hw,
                            light.position, light.range, range_col, 1.2);
                        // Drag handle on +X equator of range
                        self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw,
                            light.position + Vec3::X * light.range, range_hot, range_col);
                    }
                }
                LightType::Spot => {
                    painter.circle_filled(center, disc_r,
                        egui::Color32::from_rgba_unmultiplied(255, 200, 100, fill_alpha));
                    painter.circle_stroke(center, disc_r, egui::Stroke::new(line_w, base_col));
                    let d = light.direction();
                    let dir = Vec3::new(d[0], d[1], d[2]);
                    let cone_len = 3.0_f32.min(light.range * 0.5).max(0.5);
                    let end_w = light.position + dir * cone_len;
                    if let Some(end_s) = world_to_screen(end_w, cam_pos, orbit, rect, aspect, ortho_hw, fov) {
                        painter.line_segment([center, end_s], egui::Stroke::new(line_w, base_col));
                        let arrow_vec = end_s - center;
                        if arrow_vec.length() > 5.0 {
                            let dir2d = arrow_vec / arrow_vec.length();
                            let perp = egui::vec2(-dir2d.y, dir2d.x);
                            let wing = 6.0;
                            painter.line_segment([end_s, end_s - dir2d * wing + perp * wing * 0.4],
                                egui::Stroke::new(line_w, base_col));
                            painter.line_segment([end_s, end_s - dir2d * wing - perp * wing * 0.4],
                                egui::Stroke::new(line_w, base_col));
                        }
                    }
                    if sel {
                        let outer_rad = cone_len * light.spot_outer_deg.to_radians().tan();
                        let up = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
                        let right = dir.cross(up).normalize_or_zero();
                        let up2 = right.cross(dir).normalize_or_zero();
                        let cone_col = egui::Color32::from_rgba_unmultiplied(255, 200, 60, 100);
                        for offset in [right, -right, up2, -up2] {
                            let tip = light.position + dir * cone_len + offset * outer_rad;
                            if let (Some(s0), Some(s1)) = (
                                world_to_screen(light.position, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                                world_to_screen(tip, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                            ) {
                                painter.line_segment([s0, s1], egui::Stroke::new(1.0, cone_col));
                            }
                        }
                        // Range handle along aim axis
                        let range_hot = hover == Some(LightHandleKind::Range)
                            || dragging == Some(LightHandleKind::Range);
                        let range_col = if range_hot {
                            egui::Color32::from_rgb(255, 230, 120)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(255, 200, 60, 120)
                        };
                        let range_end = light.position + dir * light.range;
                        if let Some(re) = world_to_screen(range_end, cam_pos, orbit, rect, aspect, ortho_hw, fov) {
                            painter.line_segment([center, re], egui::Stroke::new(1.0, range_col));
                        }
                        self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw, range_end, range_hot, range_col);
                    }
                }
            }

            // --- Volumetric zone / softness (interactive handles when selected) ---
            if light.volumetric.enabled {
                let vcol = egui::Color32::from_rgb(123, 97, 255);
                painter.text(
                    egui::pos2(center.x + disc_r + 4.0, center.y - disc_r),
                    egui::Align2::LEFT_BOTTOM, "VOL",
                    egui::FontId::proportional(9.0),
                    egui::Color32::from_rgba_unmultiplied(123, 97, 255, if sel { 230 } else { 130 }));
                if sel {
                    let zone = if light.volumetric.zone > 0.0 { light.volumetric.zone } else { light.range };
                    let feather = light.volumetric.softness.clamp(0.0, 1.0);
                    let zone_hot = hover == Some(LightHandleKind::Zone)
                        || dragging == Some(LightHandleKind::Zone);
                    let soft_hot = hover == Some(LightHandleKind::Softness)
                        || dragging == Some(LightHandleKind::Softness);
                    self.draw_volumetric_zone(
                        painter, rect, cam_pos, aspect, ortho_hw, light, zone, feather, vcol,
                        zone_hot, soft_hot,
                    );
                }
            }

            // Name + short stats when selected
            let label_y = center.y + disc_r + (if sel { 16.0 } else { 8.0 });
            painter.text(
                egui::pos2(center.x, label_y),
                egui::Align2::CENTER_TOP, &light.name,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_unmultiplied(255, 200, 60, if sel { 220 } else { 120 }));
            if sel {
                let mut stats = format!("I:{:.1}  R:{:.1}", light.intensity, light.range);
                if light.volumetric.enabled {
                    let z = if light.volumetric.zone > 0.0 { light.volumetric.zone } else { light.range };
                    stats.push_str(&format!("  Z:{:.1}  S:{:.0}%", z, light.volumetric.softness * 100.0));
                }
                painter.text(
                    egui::pos2(center.x, label_y + 12.0),
                    egui::Align2::CENTER_TOP, stats,
                    egui::FontId::proportional(9.0),
                    egui::Color32::from_rgba_unmultiplied(200, 200, 210, 180));
            }
        }
    }

    /// 3-plane wire sphere (XZ / XY / YZ) — Unreal-style volume/range visualization.
    #[allow(clippy::too_many_arguments)]
    fn draw_wire_sphere(
        &self, painter: &egui::Painter, rect: egui::Rect, cam_pos: Vec3,
        aspect: f32, ortho_hw: f32, c: Vec3, radius: f32, col: egui::Color32, stroke: f32,
    ) {
        if radius <= 1e-3 { return; }
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let planes = [
            (Vec3::X, Vec3::Z),
            (Vec3::X, Vec3::Y),
            (Vec3::Y, Vec3::Z),
        ];
        let segs = 48;
        for (u, v) in planes {
            for i in 0..segs {
                let a0 = (i as f32 / segs as f32) * std::f32::consts::TAU;
                let a1 = ((i + 1) as f32 / segs as f32) * std::f32::consts::TAU;
                let p0 = c + (u * a0.cos() + v * a0.sin()) * radius;
                let p1 = c + (u * a1.cos() + v * a1.sin()) * radius;
                if let (Some(s0), Some(s1)) = (
                    world_to_screen(p0, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                    world_to_screen(p1, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                ) {
                    painter.line_segment([s0, s1], egui::Stroke::new(stroke, col));
                }
            }
        }
    }

    fn draw_handle_knob(
        &self, painter: &egui::Painter, rect: egui::Rect, cam_pos: Vec3,
        aspect: f32, ortho_hw: f32, world: Vec3, hot: bool, col: egui::Color32,
    ) {
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let Some(sp) = world_to_screen(world, cam_pos, orbit, rect, aspect, ortho_hw, fov) else { return; };
        let r = if hot { 7.0 } else { 5.0 };
        painter.circle_filled(sp, r, egui::Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), if hot { 230 } else { 180 }));
        painter.circle_stroke(sp, r, egui::Stroke::new(1.5, egui::Color32::WHITE));
    }

    /// Draw volumetric Zone + Softness for a selected volumetric light.
    /// Point → 3-plane wire spheres + knobs. Spot → cone-end rings + knobs. Dir → badge only.
    #[allow(clippy::too_many_arguments)]
    fn draw_volumetric_zone(
        &self, painter: &egui::Painter, rect: egui::Rect, cam_pos: Vec3,
        aspect: f32, ortho_hw: f32, light: &core_math::scene_graph::LightData,
        zone: f32, feather: f32, vcol: egui::Color32,
        zone_hot: bool, soft_hot: bool,
    ) {
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let zone_col = if zone_hot {
            egui::Color32::from_rgb(180, 150, 255)
        } else {
            egui::Color32::from_rgba_unmultiplied(vcol.r(), vcol.g(), vcol.b(), 180)
        };
        let soft_col = if soft_hot {
            egui::Color32::from_rgb(200, 180, 255)
        } else {
            egui::Color32::from_rgba_unmultiplied(vcol.r(), vcol.g(), vcol.b(), 90)
        };
        let inner_r = (zone * (1.0 - feather)).max(0.05);

        match light.light_type {
            LightType::Point => {
                self.draw_wire_sphere(painter, rect, cam_pos, aspect, ortho_hw,
                    light.position, zone, zone_col, 1.6);
                self.draw_wire_sphere(painter, rect, cam_pos, aspect, ortho_hw,
                    light.position, inner_r, soft_col, 1.1);
                // Zone knob +X, Softness knob +Z (distinct so both are pickable)
                self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw,
                    light.position + Vec3::X * zone, zone_hot, zone_col);
                self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw,
                    light.position + Vec3::Z * inner_r, soft_hot, soft_col);
            }
            LightType::Spot => {
                let d = light.direction();
                let dir = Vec3::new(d[0], d[1], d[2]);
                let up = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
                let right = dir.cross(up).normalize_or_zero();
                let up2 = right.cross(dir).normalize_or_zero();
                let end = light.position + dir * zone;
                let outer_r = zone * light.spot_outer_deg.to_radians().tan();
                let soft_r = outer_r * (1.0 + feather * 0.4);
                // Cone-end rings
                let ring = |painter: &egui::Painter, c: Vec3, radius: f32, col: egui::Color32, sw: f32| {
                    if radius <= 1e-3 { return; }
                    let segs = 40;
                    for i in 0..segs {
                        let a0 = (i as f32 / segs as f32) * std::f32::consts::TAU;
                        let a1 = ((i + 1) as f32 / segs as f32) * std::f32::consts::TAU;
                        let p0 = c + (right * a0.cos() + up2 * a0.sin()) * radius;
                        let p1 = c + (right * a1.cos() + up2 * a1.sin()) * radius;
                        if let (Some(s0), Some(s1)) = (
                            world_to_screen(p0, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                            world_to_screen(p1, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                        ) {
                            painter.line_segment([s0, s1], egui::Stroke::new(sw, col));
                        }
                    }
                };
                ring(painter, end, outer_r, zone_col, 1.6);
                ring(painter, end, soft_r, soft_col, 1.1);
                let rail_col = egui::Color32::from_rgba_unmultiplied(vcol.r(), vcol.g(), vcol.b(), 110);
                for off in [right, -right, up2, -up2] {
                    let tip = end + off * outer_r;
                    if let (Some(s0), Some(s1)) = (
                        world_to_screen(light.position, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                        world_to_screen(tip, cam_pos, orbit, rect, aspect, ortho_hw, fov),
                    ) {
                        painter.line_segment([s0, s1], egui::Stroke::new(1.0, rail_col));
                    }
                }
                // Zone knob at cone tip; Softness on ring rim
                self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw, end, zone_hot, zone_col);
                self.draw_handle_knob(painter, rect, cam_pos, aspect, ortho_hw,
                    end + right * soft_r, soft_hot, soft_col);
            }
            LightType::Directional => { /* shafts are non-spatial → badge only */ }
        }
    }

    /// Hit-test light icon (selection). Returns light id.
    /// Occluded lights (behind geometry) are not pickable — same rule as draw.
    pub(crate) fn light_gizmo_hit_test(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<u32> {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let mut best: Option<(f32, u32)> = None;
        for (_, light) in self.scene.scene_graph.lights() {
            if !light.visible { continue; }
            let gp = light.gizmo_position();
            if self.light_gizmo_occluded(light.id) {
                continue;
            }
            if let Some(sp) = world_to_screen(gp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let d = (mouse - sp).length();
                if d < 18.0 {
                    if best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, light.id));
                    }
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Hit-test interactive handles on the *selected* light only (zone / soft / range).
    /// Checked before icon pick so handles win when overlapping.
    pub(crate) fn light_handle_hit_test(
        &self, mouse: egui::Pos2, rect: egui::Rect,
    ) -> Option<(u32, LightHandleKind)> {
        let lid = self.active_light_id_any()?;
        let light = self.scene.light_by_id(lid)?;
        if !light.visible { return None; }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        let mut candidates: Vec<(f32, LightHandleKind, Vec3)> = Vec::new();

        // Range handle (point/spot)
        match light.light_type {
            LightType::Point => {
                candidates.push((light.range, LightHandleKind::Range, light.position + Vec3::X * light.range));
            }
            LightType::Spot => {
                let d = light.direction();
                let dir = Vec3::new(d[0], d[1], d[2]);
                candidates.push((light.range, LightHandleKind::Range, light.position + dir * light.range));
            }
            LightType::Directional => {}
        }

        // Volumetric zone / soft
        if light.volumetric.enabled {
            let zone = if light.volumetric.zone > 0.0 { light.volumetric.zone } else { light.range };
            let feather = light.volumetric.softness.clamp(0.0, 1.0);
            let inner_r = (zone * (1.0 - feather)).max(0.05);
            match light.light_type {
                LightType::Point => {
                    candidates.push((zone, LightHandleKind::Zone, light.position + Vec3::X * zone));
                    candidates.push((inner_r, LightHandleKind::Softness, light.position + Vec3::Z * inner_r));
                }
                LightType::Spot => {
                    let d = light.direction();
                    let dir = Vec3::new(d[0], d[1], d[2]);
                    let up = if dir.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
                    let right = dir.cross(up).normalize_or_zero();
                    let end = light.position + dir * zone;
                    let outer_r = zone * light.spot_outer_deg.to_radians().tan();
                    let soft_r = outer_r * (1.0 + feather * 0.4);
                    candidates.push((zone, LightHandleKind::Zone, end));
                    candidates.push((inner_r, LightHandleKind::Softness, end + right * soft_r));
                }
                LightType::Directional => {}
            }
        }

        let mut best: Option<(f32, LightHandleKind)> = None;
        for (_val, kind, world) in candidates {
            if let Some(sp) = world_to_screen(world, cam_pos, orbit, rect, aspect, ortho_hw, fov) {
                let d = (mouse - sp).length();
                if d < 14.0 {
                    if best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, kind));
                    }
                }
            }
        }
        best.map(|(_, k)| (lid, k))
    }

    /// World-space distance from light origin to mouse ray hit on a plane through the light,
    /// used to set range/zone from a handle drag.
    pub(crate) fn light_handle_world_radius(
        &self, mouse: egui::Pos2, rect: egui::Rect, light_pos: Vec3,
    ) -> Option<f32> {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        // Prefer camera-facing plane through light (stable orbiting feel).
        let plane_n = (cam_pos - light_pos).normalize_or_zero();
        let n = if plane_n.length_squared() < 1e-6 { Vec3::Y } else { plane_n };
        let hit = self.ray_plane_hit(mouse, rect, light_pos, n)?;
        Some((hit - light_pos).length().max(0.05))
    }
}
