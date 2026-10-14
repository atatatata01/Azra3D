//! Viewport overlays (selection circles, grid, wireframe, etc.) — moved from viewport.rs.

use crate::ui_menus::DefaultMenu;
use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use glam::Vec3;
use core_math::primitives::SdfShape;
use crate::*;

impl AzraApp {
    /// Shift+O overlay pie: toggle individual viewport overlay layers.
    /// Center hub (via `apply_overlay_pie_center`) toggles master `show_overlays`.
    pub(crate) fn apply_overlay_pie(&mut self, idx: usize) {
        let (name, on) = match idx {
            0 => {
                self.viewport.show_light_gizmos = !self.viewport.show_light_gizmos;
                ("Light gizmos", self.viewport.show_light_gizmos)
            }
            1 => {
                self.viewport.show_workplane = !self.viewport.show_workplane;
                ("Workplane", self.viewport.show_workplane)
            }
            2 => {
                self.viewport.grid_visible = !self.viewport.grid_visible;
                ("Grid", self.viewport.grid_visible)
            }
            3 => {
                self.viewport.show_gizmo = !self.viewport.show_gizmo;
                ("Gizmo", self.viewport.show_gizmo)
            }
            4 => {
                self.viewport.show_sel_rim = !self.viewport.show_sel_rim;
                ("Selection rim", self.viewport.show_sel_rim)
            }
            5 => {
                self.viewport.show_poly_overlays = !self.viewport.show_poly_overlays;
                ("Poly overlays", self.viewport.show_poly_overlays)
            }
            6 => {
                self.render.wire_overlay_enabled = !self.render.wire_overlay_enabled;
                ("Wire overlay", self.render.wire_overlay_enabled)
            }
            7 => {
                self.viewport.show_probe_gizmo = !self.viewport.show_probe_gizmo;
                ("Probe gizmo", self.viewport.show_probe_gizmo)
            }
            _ => return,
        };
        self.toast(&format!(
            "{name}: {}",
            if on { "ON" } else { "OFF" }
        ));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Center of Shift+O pie: master overlays toggle.
    pub(crate) fn apply_overlay_pie_center(&mut self) {
        self.viewport.show_overlays = !self.viewport.show_overlays;
        let on = self.viewport.show_overlays;
        self.toast(if on {
            "Overlays: ON (master)"
        } else {
            "Overlays: OFF (master)"
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn draw_viewport_overlays(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, rect: egui::Rect) {
        let shift=ctx.input(|i|i.modifiers.shift); let ctrl=ctx.input(|i|i.modifiers.command);
        // Voxel resolution-rebuild progress bar (the old surface keeps rendering until the swap).
        if let Some((done, total)) = self.sculpt.voxel.as_ref().and_then(|v| v.pending_rebuild.as_ref())
            .map(|p| (p.done, p.total)) {
            // `total` is None during the worker's resample phase (before it reports the chunk
            // count) → show an indeterminate "Remeshing…"; once known, show the fill %.
            let (frac, label) = match total {
                Some(t) if t > 0 => { let f = (done as f32 / t as f32).clamp(0.0, 1.0); (f, format!("Remeshing  {:.0}%", f * 100.0)) }
                _ => (0.0, "Remeshing\u{2026}".to_string()),
            };
            let (w, h) = (240.0_f32, 22.0_f32);
            let bar = egui::Rect::from_min_size(egui::pos2(rect.center().x - w * 0.5, rect.bottom() - 56.0), egui::vec2(w, h));
            let p = ui.painter();
            p.rect_filled(bar.expand(6.0), 6.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            p.rect_filled(bar, 4.0, egui::Color32::from_gray(48));
            p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(w * frac, h)), 4.0, egui::Color32::from_rgb(90, 170, 255));
            p.text(bar.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(13.0), egui::Color32::WHITE);
            ctx.request_repaint(); // keep draining until the swap completes
        }
        // Profiler overlay
        if self.ui.show_profiler_overlay && !self.ui.prof_frame_times.is_empty() {
            let p = ui.painter();
            let n = self.ui.prof_frame_times.len();
            let avg = self.ui.prof_frame_times.iter().sum::<f32>() / n as f32;
            let fps = if avg > 0.001 { 1000.0 / avg } else { 0.0 };
            let x = rect.left() + 8.0;
            let mut y = rect.top() + 8.0;
            let font = egui::FontId::monospace(11.0);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
            let _white = egui::Color32::from_gray(220);
            let dim = egui::Color32::from_gray(140);
            let gpu_only_count = if self.ui.prof_gpu_timestamps_available {
                [10usize, 11, 12, 13, 14, 15].iter().filter(|&&i| self.ui.prof_gpu_pass_times[i] > 0.001).count()
            } else { 0 };
            let lines = 3 + self.ui.prof_pass_times.iter().filter(|&&t| t > 0.001).count() + gpu_only_count;
            let hud_rect = egui::Rect::from_min_size(egui::pos2(x - 4.0, y - 4.0), egui::vec2(190.0, lines as f32 * 14.0 + 12.0));
            p.rect_filled(hud_rect, 4.0, bg);
            let fps_col = if fps >= 55.0 { egui::Color32::from_rgb(80, 220, 80) } else if fps >= 30.0 { egui::Color32::from_rgb(220, 200, 60) } else { egui::Color32::from_rgb(220, 80, 80) };
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("{:.0} FPS | {:.2}ms | {}x{}", fps, avg, self.ui.prof_render_res.0, self.ui.prof_render_res.1), font.clone(), fps_col);
            y += 14.0;
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("GPU: {:.1} MB (tex {:.1} | buf {:.2} | shd {:.1})",
                self.ui.prof_gpu_mem_total_mb, self.ui.prof_gpu_mem_textures_mb, self.ui.prof_gpu_mem_buffers_mb, self.ui.prof_gpu_mem_shadow_mb), font.clone(), dim);
            y += 14.0;
            // P3.1: shade list occupancy (top-K cull)
            if let Some(ref fl) = self.cache.cached_frame_lights {
                if fl.visible_count > 0 {
                    let shade_n = fl.shade_ids.len() as u32;
                    let col = if fl.culled_count > 0 {
                        egui::Color32::from_rgb(255, 200, 100)
                    } else {
                        dim
                    };
                    p.text(
                        egui::pos2(x, y),
                        egui::Align2::LEFT_TOP,
                        format!("Lights: {}/{} active{}", shade_n, fl.visible_count,
                            if fl.culled_count > 0 { " (culled)" } else { "" }),
                        font.clone(),
                        col,
                    );
                    y += 14.0;
                }
            }
            let colors = [egui::Color32::from_rgb(255,120,120), egui::Color32::from_rgb(120,200,255),
                egui::Color32::from_rgb(200,150,255), egui::Color32::from_rgb(150,200,150),
                egui::Color32::from_rgb(255,200,100), egui::Color32::from_rgb(200,200,200), egui::Color32::from_rgb(100,255,200)];
            for i in 0..7 {
                let cpu_t = self.ui.prof_pass_times[i];
                // Show GPU time alongside CPU time if available
                let gpu_idx = match i { 0 => 0, 1 => 1, 2 => 3, 3 => 4, 4 => 5, 5 => 8, _ => 9 };
                let gpu_t = if self.ui.prof_gpu_timestamps_available { self.ui.prof_gpu_pass_times[gpu_idx] } else { 0.0 };
                if cpu_t > 0.001 {
                    let label = if gpu_t > 0.001 {
                        format!("{}: {:.2}ms (GPU {:.3}ms)", self.ui.prof_pass_names[i], cpu_t, gpu_t)
                    } else {
                        format!("{}: {:.2}ms", self.ui.prof_pass_names[i], cpu_t)
                    };
                    p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, label, font.clone(), colors[i]);
                    y += 14.0;
                }
            }
            // GPU-only passes (no CPU-side equivalent)
            if self.ui.prof_gpu_timestamps_available {
                let gpu_col = egui::Color32::from_rgb(100, 220, 160);
                let gpu_only = [
                    (10, "FXAA"),
                    (11, "Shadow"),
                    (12, "Raster GBuf"),
                    (13, "SSGI"),
                    (14, "SSAO"),
                    (15, "RT Shadow"),
                ];
                for &(idx, name) in &gpu_only {
                    let t = self.ui.prof_gpu_pass_times[idx];
                    if t > 0.001 {
                        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
                            format!("{}: GPU {:.3}ms", name, t), font.clone(), gpu_col);
                        y += 14.0;
                    }
                }
            }
        }
        // CPU Frame Profiler overlay
        if self.ui.show_cpu_frame_profiler && !self.ui.cpu_frame_times.is_empty() {
            let p = ui.painter();
            let x = rect.left() + 8.0;
            let section_count = self.ui.cpu_section_ms.iter().filter(|&&t| t > 0.01).count();
            let graph_h = 40.0;
            let hud_h = (3 + section_count) as f32 * 14.0 + graph_h + 16.0;
            let y_start = rect.bottom() - hud_h - 30.0;
            let font = egui::FontId::monospace(11.0);
            let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
            let white = egui::Color32::from_gray(220);
            let dim = egui::Color32::from_gray(140);
            let n = self.ui.cpu_frame_times.len();
            let avg = self.ui.cpu_frame_times.iter().sum::<f32>() / n as f32;
            let max_t = self.ui.cpu_frame_times.iter().cloned().fold(0.0f32, f32::max);
            // Background
            let hud_rect = egui::Rect::from_min_size(egui::pos2(x - 4.0, y_start - 4.0), egui::vec2(206.0, hud_h));
            p.rect_filled(hud_rect, 4.0, bg);
            let mut y = y_start;
            // Header
            let col = if avg < 2.0 { egui::Color32::from_rgb(80, 220, 80) } else if avg < 8.0 { egui::Color32::from_rgb(220, 200, 60) } else { egui::Color32::from_rgb(220, 80, 80) };
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("CPU: {:.2}ms avg | {:.1}ms max", avg, max_t), font.clone(), col);
            y += 14.0;
            // Section breakdown (all cpu_section_ms slots, including Gate 0 poll/setup/toast)
            let colors = [
                egui::Color32::from_rgb(255,120,120), egui::Color32::from_rgb(120,200,255),
                egui::Color32::from_rgb(200,150,255), egui::Color32::from_rgb(255,200,100),
                egui::Color32::from_rgb(150,200,150), egui::Color32::from_rgb(200,200,200),
                egui::Color32::from_rgb(100,255,200), egui::Color32::from_rgb(255,160,80),
                egui::Color32::from_rgb(180,180,255), egui::Color32::from_rgb(255,100,180),
                egui::Color32::from_rgb(100,220,220),
            ];
            let n_sec = self.ui.cpu_section_ms.len().min(self.ui.cpu_section_names.len());
            for i in 0..n_sec {
                let t = self.ui.cpu_section_ms[i];
                if t > 0.01 {
                    p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
                        format!("  {:<10} {:.2}ms", self.ui.cpu_section_names[i], t), font.clone(), colors[i % colors.len()]);
                    y += 14.0;
                }
            }
            // Stacked bar (one row showing proportions)
            let total = self.ui.cpu_section_ms.iter().sum::<f32>().max(0.01);
            let bar_w = 198.0;
            let bar_y = y + 2.0;
            let mut bx = x;
            for i in 0..n_sec {
                let frac = self.ui.cpu_section_ms[i] / total;
                let w = frac * bar_w;
                if w > 0.5 {
                    p.rect_filled(egui::Rect::from_min_size(egui::pos2(bx, bar_y), egui::vec2(w, 6.0)), 0.0, colors[i % colors.len()]);
                    bx += w;
                }
            }
            y += 12.0;
            // Rolling graph
            let graph_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(bar_w, graph_h));
            p.rect_filled(graph_rect, 2.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 80));
            let graph_max = max_t.max(2.0);
            let dx = bar_w / 120.0;
            let points: Vec<egui::Pos2> = self.ui.cpu_frame_times.iter().enumerate().map(|(i, &t)| {
                egui::pos2(graph_rect.left() + i as f32 * dx, graph_rect.bottom() - (t / graph_max) * graph_h)
            }).collect();
            if points.len() >= 2 {
                p.add(egui::Shape::line(points, egui::Stroke::new(1.0, white)));
            }
            // 1ms reference line
            let ref_y = graph_rect.bottom() - (1.0 / graph_max) * graph_h;
            p.line_segment([egui::pos2(graph_rect.left(), ref_y), egui::pos2(graph_rect.right(), ref_y)],
                egui::Stroke::new(1.0, dim));
            p.text(egui::pos2(graph_rect.right() + 2.0, ref_y - 5.0), egui::Align2::LEFT_TOP, "1ms", egui::FontId::monospace(8.0), dim);
        }
        // Perf HUD — shared with ui2_draw_scene_overlays (v2 default shell).
        self.draw_perf_hud_overlay(ui, rect);

        // Viewport Bottom Toolbar
        {
            let bar_h = 26.0;
            let bar_w = 420.0;
            let bar_x = rect.center().x - bar_w * 0.5;
            let bar_y = rect.max.y - bar_h - 6.0;
            let bar_rect = egui::Rect::from_min_size(egui::pos2(bar_x, bar_y), egui::vec2(bar_w, bar_h));
            let p = ui.painter();
            p.rect_filled(bar_rect, 6.0, egui::Color32::from_rgba_unmultiplied(20, 20, 25, 210));
            p.rect_stroke(bar_rect, 6.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(60, 60, 80, 120)), egui::StrokeKind::Middle);

            let toggle_btn = |ui: &mut egui::Ui, label: &str, active: bool, hint: &str| -> bool {
                let col = if active { egui::Color32::from_rgb(30, 136, 229) } else { egui::Color32::from_gray(120) };
                let resp = ui.add(egui::Button::new(egui::RichText::new(label).size(11.0).color(col))
                    .fill(egui::Color32::TRANSPARENT)
                    .min_size(egui::vec2(0.0, 22.0)));
                resp.on_hover_text(hint).clicked()
            };

            let inner = bar_rect.shrink2(egui::vec2(8.0, 2.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.spacing_mut().item_spacing.x = 2.0;
            if toggle_btn(&mut child, "Grid", self.viewport.grid_visible, "Toggle grid (G)
Right-click: settings") {
                self.viewport.grid_visible = !self.viewport.grid_visible;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if child.ui_contains_pointer() && ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary)) {
                self.viewport.show_grid_settings = !self.viewport.show_grid_settings;
            }
            // Old snap UI removed — smart snap bar at top-center handles all snapping
            child.separator();
            // ONE X-Ray concept: `sel.visible_only` (header + render panel + poly wire).
            // Never write the dead `viewport.xray_mode` field (zero readers — Phase 0 honesty).
            let xray_on = !self.sel.visible_only;
            if toggle_btn(&mut child, "X-Ray", xray_on,
                "X-Ray: show all edges + select through surface
(same as header ◉Visible / ◎X-Ray)") {
                self.sel.visible_only = !self.sel.visible_only;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            child.separator();
            let shading_lbl = match self.render.shading_mode { ShadingMode::Pbr=>"PBR", ShadingMode::MatCap=>"MatCap", ShadingMode::Lit=>"Lit", ShadingMode::Wireframe=>"Wire", ShadingMode::SolidHdr=>"Solid", ShadingMode::Raytrace=>"RT", ShadingMode::PathTrace=>"PT" };
            if toggle_btn(&mut child, shading_lbl, true, "Shading (Z-pie)") {
                self.ui.z_pie.open(egui::pos2(rect.center().x, rect.center().y), 7);
            }
            child.separator();
            let hybrid_on = self.render.viewport_mode == ViewportRenderMode::Hybrid;
            if toggle_btn(&mut child, if hybrid_on {"Hybrid"} else {"SDF"}, hybrid_on, "Toggle SDF / Hybrid raster viewport") {
                self.render.viewport_mode = if hybrid_on { ViewportRenderMode::FullSdf } else { ViewportRenderMode::Hybrid };
                self.mark_dirty(DirtyFlags::RENDER);
            }
            child.separator();
            if toggle_btn(&mut child, "SSR", self.render.ssr_enabled, "Screen-Space Reflections") {
                self.render.ssr_enabled = !self.render.ssr_enabled;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if toggle_btn(&mut child, "Bloom", self.render.bloom_enabled, "Bloom post-process") {
                self.render.bloom_enabled = !self.render.bloom_enabled;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if toggle_btn(&mut child, "Fog", self.render.fog_enabled, "Volumetric fog") {
                self.render.fog_enabled = !self.render.fog_enabled;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if toggle_btn(&mut child, "BE", self.ui.show_bool_edges, "Boolean edge detection overlay") {
                self.ui.show_bool_edges = !self.ui.show_bool_edges;
                if self.ui.show_bool_edges { self.bool_edge_dirty = true; }
            }
            child.separator();
            if toggle_btn(&mut child, "\u{1F4CA}", self.ui.show_profiler_overlay, "Profiler overlay") {
                self.ui.show_profiler_overlay = !self.ui.show_profiler_overlay;
            }
            if toggle_btn(&mut child, "\u{23F1}", self.ui.show_perf_hud, "Perf HUD (CPU timings)") {
                self.ui.show_perf_hud = !self.ui.show_perf_hud;
            }
        }

        // ---- Status line hints (context-sensitive shortcuts, above bottom toolbar) ----
        {
            let hint = self.status_line_hint();
            if !hint.is_empty() {
                let hint_y = rect.max.y - 26.0 - 6.0 - 16.0;
                let hint_x = rect.center().x;
                ui.painter().text(
                    egui::pos2(hint_x, hint_y),
                    egui::Align2::CENTER_TOP,
                    &hint,
                    egui::FontId::monospace(10.5),
                    egui::Color32::from_rgba_unmultiplied(200, 200, 210, 200),
                );
            }
        }

        // Smart snap pre-highlight: when magnet / Enable Snap / Ctrl is active, always query the
        // nearest candidate under the cursor (not only mid-drag). Owner: "when snapping is
        // activated we need to prehighlight the snap effect". Mid-drag still updates live.
        let snap_active = self.is_snap_active(ctrl);
        if snap_active {
            // latest_pos survives mid-drag when hover_pos is None (GRS D1 / poly G/R/S).
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos())) {
                let exclude = self.tool.current.as_create().map(|cs| cs.prim_idx);
                self.viewport.smart_snap_result = self.find_smart_snap(mp, rect, exclude);
            }
        } else {
            self.viewport.smart_snap_result = None;
        }

        // Boolean edge detection + projection
        if self.ui.show_bool_edges {
            if self.bool_edge_dirty {
                self.bool_edges = crate::edge_detect::detect_bool_edges(&self.scene.primitives);
                self.bool_edge_dirty = false;
            }
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
            let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
            crate::edge_detect::project_edges(&mut self.bool_edges, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
            // Hover detection
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.bool_edge_hovered = crate::edge_detect::detect_edge_hover(&self.bool_edges, mp, 10.0);
            } else {
                self.bool_edge_hovered = None;
            }
        }

        let overlays_on = self.viewport.show_overlays;

        // Wire grid overlay (egui lines, same style as workplane)
        if overlays_on {
            self.draw_grid_overlay(ui.painter(), rect);
        }
        // Work Plane overlay: egui wire grid in all modes (Auto + Locked)
        if overlays_on && self.viewport.show_workplane {
            self.draw_workplane_overlay(ui.painter(), rect);
        }
        // Hover ring overlay (legacy "Selection UI", OFF by default). SELECTED-prim
        // feedback is the GPU prim outline (viewport_render MaskOutline pass) — the CPU
        // shape wireframe was deleted 2026-07-09. Hover has no GPU pass, so the ring
        // stays here, in both Object and SDF Edit (Vertex) modes.
        let prim_overlay_mode = self.tool.work_mode == WorkMode::Object
            || (self.tool.work_mode == WorkMode::Vertex && self.modeling_context == ModelingContext::Sdf);
        if overlays_on && self.viewport.show_selection_ui && prim_overlay_mode {
            self.draw_selection_highlights(ui.painter(), rect);
        }
        if self.tool.work_mode == WorkMode::Object {
            // Body/node selection feedback: Selection Outline (show_sel_rim) is the overall
            // setting — used in Hybrid and Path Trace (GPU mesh rim may be absent in PT).
            // Selection UI remains the optional hover/legacy rings toggle.
            if overlays_on
                && (self.viewport.show_sel_rim || self.viewport.show_selection_ui)
            {
                self.draw_node_selection_outlines(ui.painter(), rect);
            }
            self.draw_billboard_camera_icon(ctx, ui.painter(), rect);
            // Always show light icons when a light is selected (findable after create);
            // otherwise respect overlay + Light Gizmos toggles.
            // `selected_light_ids` (not `has_lights`) — an outliner-selected light is a
            // `SceneItemId::Node`, so with overlays off its icon/handles disappeared entirely.
            if (overlays_on && self.viewport.show_light_gizmos)
                || !self.selected_light_ids().is_empty()
            {
                self.draw_light_gizmos(ui.painter(), rect);
            }
        }
        // Live transform values during gizmo drag / modal transform
        self.draw_live_transform_values(ui.painter(), rect);
        // NOTE: the transform gizmo is drawn LAST (see the end of this function), not here.
        // Owner 2026-07-30: "gizmo render under polygroup" — the polygroup fills paint through this
        // SAME egui painter (`draw_poly.rs:317`), and they used to run AFTER this call, so the
        // opaque group colour covered the gizmo arrows.
        // Blend-k handle (diamond on selected prim/body with boolean op)
        if overlays_on && (self.tool.work_mode == WorkMode::Object || self.tool.work_mode == WorkMode::Vertex) {
            let m = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_blend_k_handle(ui.painter(), rect, m);
        }
        // Blend-k popup (right-click on diamond — works for both prims and bodies)
        self.draw_blend_k_popup(ctx);
        self.draw_depth_pick_overlay(ui.painter(),rect);
        self.draw_symmetry_plane(ui.painter(), rect);
        if self.ui.sym_pie.open {
            let def = crate::pie_menu::sym_pie_def(self.sym_center_label());
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.sym_pie, &def, None);
        }
        self.draw_dirty_regions_overlay(ui.painter(), rect);
        // Create tool overlay + HUD
        self.draw_create_overlay(ui.painter(),rect);
        self.draw_clip_curve_overlay(ui.painter(), rect);
        self.draw_curve_point_overlay(ui.painter(), rect);
        self.draw_spine_point_overlay(ui.painter(), rect);
        self.draw_curve_edge_toolbar(ui, rect);
        { let m = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
          self.draw_create_hud(ui.painter(), m); }
        self.draw_create_dimensions(ui.painter(), rect);
        // Active tool HUD (armed but not drawing)
        self.draw_active_tool_hud(ui.painter(), rect);
        let bevel_indices = self.selected_prim_indices_unified();
        // Handles + reset chrome only while Bevel mode (B) is active — not sticky after tool switch.
        if self.tool.mode.is_bevel()
            && self.tool.work_mode == WorkMode::Object
            && bevel_indices.len() == 1
        {
            let idx = bevel_indices[0];
            if matches!(
                self.scene.primitives.get(idx).map(|p| &p.shape),
                Some(SdfShape::Box { .. } | SdfShape::Cylinder { .. })
            ) {
                self.draw_bevel_handles(ui.painter(), rect, ctx);
                // Bevel reset button — small curving arrow in viewport corner
                let has_bevel = self.scene.primitives.get(idx).map_or(false, |p| match &p.shape {
                    SdfShape::Box { bevel, .. } => bevel.radii.iter().any(|r| *r > 0.001),
                    SdfShape::Cylinder { .. } => p.cyl_bevel.has_any(),
                    _ => false,
                });
                if has_bevel {
                    let btn_size = egui::vec2(22.0, 22.0);
                    let btn_pos = egui::pos2(rect.right() - 34.0, rect.top() + 56.0);
                    let btn_rect = egui::Rect::from_min_size(btn_pos, btn_size);
                    let btn_id = ui.id().with("bevel_reset_btn");
                    let resp = ui.interact(btn_rect, btn_id, egui::Sense::click());
                    let bg = if resp.hovered() {
                        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 200)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(40, 40, 40, 160)
                    };
                    ui.painter().rect_filled(btn_rect, 4.0, bg);
                    ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0,
                        if resp.hovered() { egui::Color32::from_rgb(255, 170, 40) }
                        else { egui::Color32::from_rgb(120, 120, 120) }), egui::StrokeKind::Middle);
                    // Draw curving arrow (↺) as arc + arrowhead
                    let c = btn_rect.center();
                    let r = 6.0f32;
                    let pts: Vec<egui::Pos2> = (0..=10).map(|i| {
                        let t = std::f32::consts::PI * 1.5 * (i as f32 / 10.0) - std::f32::consts::FRAC_PI_2;
                        egui::pos2(c.x + r * t.cos(), c.y - r * t.sin())
                    }).collect();
                    let arrow_col = if resp.hovered() { egui::Color32::from_rgb(255, 200, 80) }
                        else { egui::Color32::from_rgb(180, 180, 180) };
                    ui.painter().add(egui::Shape::line(pts.clone(), egui::Stroke::new(1.5, arrow_col)));
                    // Arrowhead at end of arc
                    if let Some(&tip) = pts.last() {
                        let prev = pts[pts.len().saturating_sub(2)];
                        let dir = egui::vec2(tip.x - prev.x, tip.y - prev.y);
                        let len = (dir.x * dir.x + dir.y * dir.y).sqrt().max(0.01);
                        let d = egui::vec2(dir.x / len, dir.y / len);
                        let perp = egui::vec2(-d.y, d.x);
                        let a1 = tip + (-d * 4.0 + perp * 2.5);
                        let a2 = tip + (-d * 4.0 - perp * 2.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![tip, a1, a2], arrow_col, egui::Stroke::NONE));
                    }
                    resp.clone().on_hover_text("Reset all bevels");
                    if resp.clicked() {
                        // Skip empty undo when corner bevels are already at defaults.
                        if self.reset_prim_corner_bevel_undoable(idx) {
                            self.toast("Bevel reset");
                        }
                    }
                }
            }
        }
        // RMB region-select shape (Box / Circle / Lasso) — drawn UNGATED, on purpose.
        //
        // Owner 2026-07-30: "RMB I don't see the shape draw when I drag." The drag WAS working and
        // the selection WAS being applied — only the rubber-band was invisible, because this call
        // used to sit inside `if overlays_on && self.viewport.show_poly_overlays`. Both of those are
        // user-facing DISPLAY toggles ("Overlays" / "Poly Overlays"), so turning off wireframe
        // decoration also silently blinded an INPUT GESTURE.
        //
        // A live drag's feedback is not decoration: without it the user cannot see what they are
        // about to select, and the tool reads as broken rather than as "overlays are off". It only
        // paints while `ActiveTool::PolyRegionSelect` is active, so an ungated call costs nothing
        // when no drag is in flight.
        self.draw_poly_region_select_overlay(ui.painter());
        // Knife R/C rubber-band — same ungated path as region select (must paint even if
        // poly overlays are off). Never shares state with drag-select tools.
        self.draw_knife_shape_select_overlay(ui.painter(), rect);
        // Viewport frame status (record / broken geometry / render progress / unsaved / VRAM).
        // Drawn from BOTH overlay paths for the same reason the region-select band is - a single
        // call site means the v1 and v2 shells silently disagree about what the user can see.
        self.draw_viewport_frame_indicator(ui.painter(), rect);
        // Bevel debug overlay (gated by Developer Tweaks checkbox)
        if self.ui.show_bevel_debug {
            self.draw_bevel_debug_overlay(ui.painter(), rect);
        }
        // Wireframe overlays (object + poly edit) are drawn by the depth-tested GPU line pass
        // in viewport_render.rs, not by the egui painter — they must be occluded by the mesh.
        //
        // Poly selection/hover + live tool feedback — UNGATED (same class as region-select).
        //
        // Owner: "selection highlight missing half the time." Component highlights lived inside
        // `if overlays_on && show_poly_overlays`, so Shift+O / Poly Overlays off (or master
        // Overlays off) hid verts/edges/faces while selection still worked — the tool looked
        // broken. Selection is not decoration; GPU edit wire + polygroups keep their own gates.
        // Each draw_* early-returns when idle, so this is free outside poly edit / tool use.
        self.draw_poly_overlays(ui.painter(), rect);
        self.draw_loop_cut_preview(ui.painter(), rect);
        self.draw_bevel_hud(ctx, rect);
        self.draw_extrude_hud(ui.painter(), rect);
        self.draw_extrude_sweep_overlay(ui.painter(), rect);
        self.draw_inset_hud(ui.painter(), rect);
        self.draw_poly_circle_radius_handle(ui.painter(), rect);
        self.draw_knife_overlay(ui.painter(), rect);
        self.draw_poly_box_select_overlay(ui.painter());
        {
            let mp = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(0.0, 0.0)));
            self.draw_poly_circle_select_overlay(ui.painter(), mp);
            self.draw_poly_paint_select_overlay(ui.painter(), mp, self.poly_paint_radius);
        }
        // NURBS cage/control decoration still follows Poly Overlays (not selection-critical).
        if overlays_on && self.viewport.show_poly_overlays {
            self.draw_nurbs_overlays(ui.painter(), rect);
        }
        // TRANSFORM GIZMO — drawn AFTER the poly overlays, on purpose.
        //
        // Owner 2026-07-30: "gizmo render under polygroup." The polygroup fills
        // (`draw_poly.rs:317`) paint through this SAME egui painter, which has no depth buffer —
        // later calls simply cover earlier ones. The gizmo used to be drawn ~110 lines ABOVE this
        // block, so an opaque group colour painted straight over the arrows and the user could not
        // grab a handle they could not see.
        //
        // The gizmo is a CONTROL, not decoration: it must sit above every mesh-space overlay
        // (polygroups, selection fills, wire) while still staying below the screen-space HUD strips
        // drawn further down. This position is that layer. Keep it here — moving it back above the
        // poly block reintroduces the bug, and moving it to the very end would let it cover the
        // toolbars.
        if overlays_on && self.viewport.show_gizmo {
            self.draw_gizmo(ui.painter(), rect);
            self.draw_falloff_gizmo(ui.painter(), rect);
            self.draw_mask_gizmo(ui.painter(), rect);
        }
        // Boolean edge overlays
        self.draw_bool_edges(ui.painter());
        if self.tool.work_mode==WorkMode::Face {
            self.draw_selected_faces(ui.painter(), rect);
            self.draw_face_feedback(ui.painter(), rect);
            // Edit mode HUD hints
            let hp = egui::pos2(rect.min.x + 10.0, rect.max.y - 70.0);
            let hints: &[&str] = if self.tool.current.is_face_push_pull() {
                &["Drag: Push/Pull", "Shift: Snap to grid", "Esc: Cancel"]
            } else {
                &["Click face to select", "Shift+Click: multi-select", "G: Push/Pull selected", "Tab: exit edit mode"]
            };
            for (i, h) in hints.iter().enumerate() {
                ui.painter().text(hp + egui::vec2(0.0, i as f32 * 14.0), egui::Align2::LEFT_TOP, *h,
                    egui::FontId::monospace(10.0), egui::Color32::from_rgba_unmultiplied(200,200,200,180));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.viewport.snap_indicator.is_some() {
            self.draw_snap_indicator(ui.painter(), rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Smart snap indicator + bar (viewport modeling only — hide over Shader Node Editor)
        if !self.ui.show_node_editor {
            if self.viewport.smart_snap_result.is_some() {
                self.draw_smart_snap_indicator(ui.painter(), rect);
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
            // Smart snap bar at top-center of the 3D viewport
            self.draw_smart_snap_bar(ctx, rect);
        }
        // Object-mode long-press link / match rubber-band
        if self.ui.object_link_gesture.as_ref().is_some_and(|g| g.armed) {
            self.object_link_draw_overlay(ctx, rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Symmetry tool overlay
        if self.tool.current.is_smart_mirror() {
            self.draw_smart_mirror(ui.painter(), rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.tool.current.is_selection_box(){let m=ctx.input(|i|i.pointer.hover_pos().unwrap_or(egui::pos2(0.0,0.0)));self.draw_selection_overlay(ui.painter(),m);}
        if self.tool.work_mode==WorkMode::Sculpt{let m=ctx.input(|i|i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0,-100.0)));self.draw_brush_preview(ui.painter(),rect,m,shift,ctrl);ctx.request_repaint_after(std::time::Duration::from_millis(16));
            // Same sculpt header as the v2 path — Add/Sub, Size, Strength, Falloff, Brush/Texture/Stroke.
            self.ui2_sculpt_header_overlay(ctx, ui, rect);
        }
        // Proportional Edit falloff ring + origin handle (self-gated on prop being ON in a
        // poly sub-object mode). No repaint timer: it is static geometry, and egui already
        // repaints on pointer motion, which is all the hover highlight needs.
        {
            let m = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_poly_prop_overlay(ui.painter(), rect, m);
        }
        // SDF sculpt brush cursor (Object mode, modifier-based)
        if self.sdf_sculpt_active && (self.tool.work_mode == WorkMode::Object || self.tool.work_mode == WorkMode::Sculpt) {
            let m = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_sdf_brush_preview(ui.painter(), rect, m);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        // Shortcut Discovery Overlay
        if !self.tool.current.is_transform() && !self.tool.current.is_gizmo()
           && self.tool.work_mode==WorkMode::Object && self.ui.z_pie.open==false && self.ui.tab_pie.open==false {
            let (ctrl_h, shift_h) = ctx.input(|i| (i.modifiers.command && !i.modifiers.shift, i.modifiers.shift && !i.modifiers.command));
            if ctrl_h || shift_h {
                let kb = &self.keybindings;
                let pairs: Vec<(&str, String)> = if ctrl_h {
                    vec![
                        ("Undo", kb.label(ACT_UNDO)), ("Redo", kb.label(ACT_REDO)),
                        ("Select All", kb.label(ACT_SELECT_ALL)),
                        ("Export", kb.label(ACT_EXPORT)),
                    ]
                } else {
                    vec![("Duplicate", kb.label(ACT_DUPLICATE)), ("Box Select", "Shift+Drag".into())]
                };
                let base = egui::pos2(rect.min.x + 12.0, rect.max.y - 12.0 - (pairs.len() as f32)*18.0);
                let p = ui.painter();
                let bg_h = pairs.len() as f32 * 18.0 + 8.0;
                p.rect_filled(egui::Rect::from_min_size(egui::pos2(base.x-4.0, base.y-4.0), egui::vec2(200.0, bg_h)),
                    4.0, egui::Color32::from_rgba_unmultiplied(10,10,10,170));
                for (i,(name,key)) in pairs.iter().enumerate() {
                    let y = base.y + i as f32 * 18.0;
                    p.text(egui::pos2(base.x, y), egui::Align2::LEFT_TOP, format!("{:<18}  {}", name, key),
                        egui::FontId::monospace(11.0), egui::Color32::from_rgba_unmultiplied(220,220,220,220));
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }

        // ---- Viewport overlay strip (top-right, Blender-style) ----
        {
            let icon_size = egui::vec2(22.0, 20.0);
            let strip_h = 24.0;
            let strip_y = rect.min.y + 4.0;
            let margin_r = 8.0;

            // Count buttons to calculate width
            let n_buttons = 10u32;
            let strip_w = n_buttons as f32 * (icon_size.x + 2.0) + 12.0;
            let strip_x = rect.max.x - strip_w - margin_r;
            let strip_rect = egui::Rect::from_min_size(egui::pos2(strip_x, strip_y), egui::vec2(strip_w, strip_h));

            // Background
            let p = ui.painter();
            p.rect_filled(strip_rect, 4.0, egui::Color32::from_rgba_unmultiplied(20, 20, 25, 200));
            p.rect_stroke(strip_rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(60, 60, 80, 100)), egui::StrokeKind::Middle);

            let inner = strip_rect.shrink2(egui::vec2(6.0, 2.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.spacing_mut().item_spacing.x = 2.0;

            let icon_btn = |ui: &mut egui::Ui, label: &str, active: bool, hint: &str| -> bool {
                let col = if active { egui::Color32::from_rgb(30, 136, 229) } else { egui::Color32::from_gray(100) };
                ui.add(egui::Button::new(egui::RichText::new(label).size(12.0).color(col))
                    .fill(egui::Color32::TRANSPARENT)
                    .min_size(icon_size))
                    .on_hover_text(hint).clicked()
            };

            // Overlays master toggle (eye icon)
            if icon_btn(&mut child, "\u{1F441}", self.viewport.show_overlays, "Toggle All Overlays") {
                self.viewport.show_overlays = !self.viewport.show_overlays;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            // Overlay settings popover (down arrow)
            if icon_btn(&mut child, "\u{25BE}", self.ui.show_overlay_popover, "Overlay Settings") {
                self.ui.show_overlay_popover = !self.ui.show_overlay_popover;
            }

            child.separator();

            // Gizmo toggle
            if icon_btn(&mut child, "\u{2316}", self.viewport.show_gizmo, "Gizmo") {
                self.viewport.show_gizmo = !self.viewport.show_gizmo;
            }
            // Selection UI
            if icon_btn(&mut child, "\u{25CB}", self.viewport.show_selection_ui, "Selection UI") {
                self.viewport.show_selection_ui = !self.viewport.show_selection_ui;
                self.mark_dirty(DirtyFlags::RENDER);
            }

            child.separator();

            // Shading mode cycle
            let shading_lbl = match self.render.shading_mode {
                ShadingMode::Pbr=>"PBR", ShadingMode::MatCap=>"MC", ShadingMode::Lit=>"Lit", ShadingMode::Wireframe=>"W",
                ShadingMode::SolidHdr=>"Sol", ShadingMode::Raytrace=>"RT", ShadingMode::PathTrace=>"PT"
            };
            if icon_btn(&mut child, shading_lbl, true, "Shading Mode (Z pie)") {
                self.ui.z_pie.open(egui::pos2(rect.center().x, rect.center().y), 7);
            }

            // Dither (left-click toggle, right-click settings popup)
            {
                let col = if self.render.dither_enabled { egui::Color32::from_rgb(30, 136, 229) } else { egui::Color32::from_gray(100) };
                let resp = child.add(egui::Button::new(egui::RichText::new("Di").size(12.0).color(col))
                    .fill(egui::Color32::TRANSPARENT)
                    .min_size(icon_size));
                resp.clone().on_hover_text("Output Dither (anti-banding)
Right-click: settings");
                if resp.clicked() {
                    self.render.dither_enabled = !self.render.dither_enabled;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                resp.azra_context_menu(|ui| {
                    ui.label("Dither Settings");
                    ui.separator();
                    if ui.checkbox(&mut self.render.dither_enabled, "Enabled")
.default_menu("render.dither_enabled", serde_json::json!(self.render.dither_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                    if ui.add(egui::Slider::new(&mut self.render.dither_strength, 0.0..=2.0).text("Strength").step_by(0.05)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                });
            }

            // Denoise lives under Render → Path Trace / Path Tracer Settings (not viewport overlays).

            child.separator();

            // Ortho / Persp toggle
            let proj_lbl = if self.render.ortho { "O" } else { "P" };
            if icon_btn(&mut child, proj_lbl, true, "Ortho / Perspective") {
                self.set_user_projection_ortho(!self.render.ortho);
            }

            // FOV display (only in perspective)
            if !self.render.ortho {
                let fov_resp = child.add(egui::DragValue::new(&mut self.render.fov_deg)
                    .range(10.0..=170.0).speed(0.5).suffix("°")
                    .custom_formatter(|v, _| format!("{:.0}°", v)));
                if fov_resp.changed() { self.mark_dirty(DirtyFlags::RENDER); }
                fov_resp.on_hover_text("Field of View");
            }
        }

        // ---- Overlay popover (below the strip) ----
        if self.ui.show_overlay_popover {
            let popup_w = 220.0;
            let popup_x = rect.max.x - popup_w - 16.0;
            let popup_y = rect.min.y + 32.0;
            let popup_rect = egui::Rect::from_min_size(egui::pos2(popup_x, popup_y), egui::vec2(popup_w, 640.0));
            let resp = egui::Area::new(egui::Id::new("overlay_popover"))
                .fixed_pos(popup_rect.min)
                .constrain(true)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_width(popup_w - 16.0);
                        ui.label(egui::RichText::new("Viewport Overlays").strong().size(12.0));
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(600.0).show(ui, |ui| {
                        if ui.checkbox(&mut self.viewport.show_overlays, "Show Overlays")
.default_menu("viewport.show_overlays", serde_json::json!(self.viewport.show_overlays)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        ui.separator();
                        // (Debug render-mode moved to Developer Tweaks panel — win_tweaks.rs)
                        ui.label(egui::RichText::new("Display").size(11.0).color(egui::Color32::from_gray(140)));
                        if ui.checkbox(&mut self.viewport.show_sdf, "SDF Geometry")
.default_menu("viewport.show_sdf", serde_json::json!(self.viewport.show_sdf)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.viewport.show_poly_overlays, "Poly Overlays")
.default_menu("viewport.show_poly_overlays", serde_json::json!(self.viewport.show_poly_overlays)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.viewport.grid_visible, "Grid")
.default_menu("viewport.grid_visible", serde_json::json!(self.viewport.grid_visible)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.viewport.show_workplane, "Work Plane")
.default_menu("viewport.show_workplane", serde_json::json!(self.viewport.show_workplane)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.viewport.show_light_gizmos, "Light Gizmos")
.default_menu("viewport.show_light_gizmos", serde_json::json!(self.viewport.show_light_gizmos)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        // ---- Wire look (object + poly edit) ----
                        ui.label(egui::RichText::new("Wire").size(11.0).color(egui::Color32::from_gray(140)));
                        {
                            let mut wire = self.render.wire_overlay_enabled;
                            if ui.checkbox(&mut wire, "Object Wire (Shift+Z)")
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
                                .on_hover_text("Object-mode mesh wireframe (poly + SDF DC edges).")
                                .changed()
                            {
                                self.render.wire_overlay_enabled = wire;
                                self.overlay_lines_fp = u64::MAX;
                                if wire { self.wire_local_cache.clear(); }
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        }
                        if self.render.wire_overlay_enabled {
                            ui.indent("obj_wire_look", |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Color");
                                    let mut c = self.render.wire_overlay_color;
                                    if ui.color_edit_button_rgb(&mut c).changed() {
                                        self.render.wire_overlay_color = c;
                                        self.overlay_lines_fp = u64::MAX;
                                        self.mark_dirty(DirtyFlags::RENDER);
                                    }
                                });
                                if ui.add(egui::Slider::new(&mut self.render.wire_overlay_opacity, 0.05..=1.0).text("Opacity")).changed() {
                                    self.overlay_lines_fp = u64::MAX;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                                if ui.add(egui::Slider::new(&mut self.render.wire_overlay_thickness, 0.5..=5.0).text("Size")).changed() {
                                    self.overlay_lines_fp = u64::MAX;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                                if ui.checkbox(&mut self.render.wire_overlay_backface, "Show backfaces (X-Ray)")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface)).changed() {
                                    self.overlay_lines_fp = u64::MAX;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            });
                        }
                        ui.label(egui::RichText::new("Poly Edit Wire").size(10.0).weak());
                        ui.indent("poly_wire_look", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Color");
                                let mut c = self.render.poly_wire_color;
                                if ui.color_edit_button_rgb(&mut c).changed() {
                                    self.render.poly_wire_color = c;
                                    self.overlay_lines_fp = u64::MAX;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            });
                            if ui.add(egui::Slider::new(&mut self.render.poly_wire_opacity, 0.05..=1.0).text("Opacity")).changed() {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.add(egui::Slider::new(&mut self.render.poly_wire_thickness, 0.5..=5.0).text("Size")).changed() {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            // X-Ray is shared with visible_only selection (header ◉Visible / ◎X-Ray).
                            let mut xray = !self.sel.visible_only;
                            if ui.checkbox(&mut xray, "X-Ray (all edges + select through)")
                                .on_hover_text("On: draw every edge and pick through the surface. Off: front-facing wire + visible-only pick.")
                                .changed()
                            {
                                self.sel.visible_only = !xray;
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                        // ---- UV Editor: a VIEW, not an overlay ----
                        // Owner 2026-08-11: *"THE UV VIEW SHOULD BE PART OF THE VIEWPORT LIST
                        // PART OF THE UI"*. Alt+U already toggled it, but a keybind is not
                        // discoverable — it belongs in the list of things the viewport can show.
                        ui.label(
                            egui::RichText::new("Views")
                                .size(11.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                        if ui
                            .checkbox(&mut self.ui.show_uv_layout, "UV Editor  (Alt+U)")
                            .on_hover_text(
                                "Split the viewport and show the UV layout on the left. Drag the                                  divider to resize.",
                            )
                            .changed()
                        {
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                        ui.separator();
                        // ---- Mesh edge marks (UV seam / sharp / edge weight) ----
                        ui.label(egui::RichText::new("Mesh Edge Marks").size(11.0).color(egui::Color32::from_gray(140)));
                        ui.add_enabled_ui(self.viewport.show_overlays, |ui| {
                            if ui.checkbox(&mut self.viewport.show_uv_seams, "UV Seams")
.default_menu("viewport.show_uv_seams", serde_json::json!(self.viewport.show_uv_seams))
                                .on_hover_text("Highlight authored UV seam edges (red). Mark seams via UV unwrap / seam tools.")
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.checkbox(&mut self.viewport.show_sharp_edges, "Sharp Edges")
.default_menu("viewport.show_sharp_edges", serde_json::json!(self.viewport.show_sharp_edges))
                                .on_hover_text("Highlight Mark Sharp edges (cyan). Edge mode: Ctrl+Shift+E / Clear Sharp.")
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.checkbox(&mut self.viewport.show_edge_weights, "Edge Weights")
.default_menu("viewport.show_edge_weights", serde_json::json!(self.viewport.show_edge_weights))
                                .on_hover_text("Highlight per-edge weights / creases (magenta; brighter = heavier). Poly menu → Set Edge Weight.")
                                .changed()
                            {
                                self.overlay_lines_fp = u64::MAX;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                        ui.separator();

                        // ---- Selection highlight ----
                        ui.label(egui::RichText::new("Selection").size(11.0).color(egui::Color32::from_gray(140)));
                        if ui.checkbox(&mut self.viewport.show_selection_ui, "Selection UI (hover rings)")
.default_menu("viewport.show_selection_ui", serde_json::json!(self.viewport.show_selection_ui)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.viewport.show_sel_rim, "Object Outline")
.default_menu("viewport.show_sel_rim", serde_json::json!(self.viewport.show_sel_rim)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if self.viewport.show_sel_rim {
                            ui.indent("sel_rim_settings", |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Color");
                                    let mut c = self.viewport.sel_rim_color;
                                    if ui.color_edit_button_rgb(&mut c).changed() { self.viewport.sel_rim_color = c; self.mark_dirty(DirtyFlags::RENDER); }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Width");
                                    if ui.add(egui::DragValue::new(&mut self.viewport.sel_outline_width).speed(0.1).range(0.5..=f32::MAX).suffix(" px")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                });
                            });
                        }
                        if self.viewport.show_selection_ui {
                            ui.indent("sel_ui_settings", |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Color");
                                    let mut c = self.viewport.sel_color;
                                    if ui.color_edit_button_rgb(&mut c).changed() { self.viewport.sel_color = c; self.mark_dirty(DirtyFlags::RENDER); }
                                });
                                if ui.add(egui::Slider::new(&mut self.viewport.sel_opacity, 0.0..=1.0).text("Opacity").clamping(egui::SliderClamping::Never)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                ui.horizontal(|ui| {
                                    ui.label("Radius");
                                    if ui.add(egui::DragValue::new(&mut self.viewport.sel_radius).speed(0.5).range(0.0..=f32::MAX).suffix(" px")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Stroke");
                                    if ui.add(egui::DragValue::new(&mut self.viewport.sel_thickness).speed(0.1).range(0.0..=f32::MAX).suffix(" px")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                                });
                            });
                        }
                        ui.label(egui::RichText::new("Poly Edit Selection").size(10.0).weak());
                        ui.indent("poly_sel_look", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Color");
                                let mut c = self.viewport.poly_sel_color;
                                if ui.color_edit_button_rgb(&mut c).changed() {
                                    self.viewport.poly_sel_color = c;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            });
                            if ui.add(egui::Slider::new(&mut self.viewport.poly_sel_opacity, 0.05..=1.0).text("Opacity")).changed() {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.add(egui::Slider::new(&mut self.viewport.poly_sel_face_opacity, 0.0..=1.0).text("Face fill")).changed() {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.add(egui::Slider::new(&mut self.viewport.poly_sel_edge_width, 0.5..=8.0).text("Edge size")).changed() {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.add(egui::Slider::new(&mut self.viewport.poly_sel_vert_size, 1.0..=12.0).text("Vert size")).changed() {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.checkbox(&mut self.viewport.poly_sel_show_backface, "Show backfaces")
.default_menu("viewport.poly_sel_show_backface", serde_json::json!(self.viewport.poly_sel_show_backface))
                                .on_hover_text("On: selection always visible (including back-facing). Off: hide selection on faces pointing away from the camera.")
                                .changed()
                            {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                        ui.separator();
                        ui.checkbox(&mut self.viewport.show_tool_viewport_ui, "Tool UI")
.default_menu("viewport.show_tool_viewport_ui", serde_json::json!(self.viewport.show_tool_viewport_ui));
                        if ui.checkbox(&mut self.viewport.show_gizmo, "Gizmo")
.default_menu("viewport.show_gizmo", serde_json::json!(self.viewport.show_gizmo)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if self.viewport.show_gizmo {
                            ui.indent("reset_handle_toggle", |ui| {
                                ui.checkbox(&mut self.viewport.show_reset_handle, "Reset Handle")
.default_menu("viewport.show_reset_handle", serde_json::json!(self.viewport.show_reset_handle));
                            });
                        }
                        ui.separator();
                        // Viewport display effects (not PT denoise — that is Render → Denoise only).
                        ui.label(egui::RichText::new("Rendering").size(11.0).color(egui::Color32::from_gray(140)));
                        if ui.checkbox(&mut self.render.ssr_enabled, "SSR")
.default_menu("render.ssr_enabled", serde_json::json!(self.render.ssr_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.render.bloom_enabled, "Bloom")
.default_menu("render.bloom_enabled", serde_json::json!(self.render.bloom_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.checkbox(&mut self.render.fog_enabled, "Fog")
.default_menu("render.fog_enabled", serde_json::json!(self.render.fog_enabled)).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        ui.separator();
                        ui.label(egui::RichText::new("Camera").size(11.0).color(egui::Color32::from_gray(140)));
                        if ui.add(egui::Slider::new(&mut self.render.fov_deg, 10.0..=170.0).text("FOV")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.add(egui::Slider::new(&mut self.render.exposure, 0.0..=5.0).text("Exposure")).changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        }); // end ScrollArea
                    });
                });
            // Close popover when clicking outside
            if ctx.input(|i| i.pointer.any_click()) && !resp.response.rect.contains(ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0,-100.0)))) {
                // Check if click was on the popover button itself (don't close immediately)
                let strip_area = egui::Rect::from_min_size(egui::pos2(rect.max.x - 260.0, rect.min.y + 4.0), egui::vec2(260.0, 24.0));
                if !strip_area.contains(ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0,-100.0)))) {
                    self.ui.show_overlay_popover = false;
                }
            }
        }

        // Viewport shading indicator
        if self.render.shading_mode != ShadingMode::Pbr {
            let lbl = match self.render.shading_mode { ShadingMode::MatCap=>"MatCap", ShadingMode::Lit=>"Lit", ShadingMode::Wireframe=>"Wireframe", ShadingMode::SolidHdr=>"Solid+HDR", ShadingMode::Pbr=>"PBR", ShadingMode::Raytrace=>"Raytrace", ShadingMode::PathTrace=>"Path Trace" };
            ui.painter().text(egui::pos2(rect.min.x+8.0, rect.min.y+8.0), egui::Align2::LEFT_TOP, lbl,
                egui::FontId::proportional(11.0), egui::Color32::from_rgba_unmultiplied(255,200,80,200));
        }

        // PT SPP counter overlay (Octane-style progressive status)
        if self.render.shading_mode == ShadingMode::PathTrace && self.render.pt_show_spp {
            let spp = self.render.pt_accumulated_frames * self.render.pt_samples_per_frame.max(1);
            let spp_lbl = if self.render.pt_converged {
                format!("{spp} SPP  OK")
            } else {
                format!("{spp} SPP  \u{2026}")
            };
            let col = if self.render.pt_converged {
                egui::Color32::from_rgba_unmultiplied(100, 220, 160, 220)
            } else if spp < 64 {
                egui::Color32::from_rgba_unmultiplied(255, 160, 80, 220)
            } else if spp < 256 {
                egui::Color32::from_rgba_unmultiplied(180, 220, 100, 220)
            } else {
                egui::Color32::from_rgba_unmultiplied(100, 220, 180, 220)
            };
            ui.painter().text(egui::pos2(rect.min.x+8.0, rect.min.y+22.0), egui::Align2::LEFT_TOP, spp_lbl,
                egui::FontId::monospace(11.0), col);
        }

        // Z-Pie shading overlay — uses shared draw_pie_menu
        {
            let z_def = crate::pie_menu::z_pie_def();
            let active_shading = match self.render.shading_mode {
                ShadingMode::Pbr => Some(0), ShadingMode::Raytrace => Some(1),
                ShadingMode::PathTrace => Some(2), ShadingMode::MatCap => Some(3),
                ShadingMode::Lit => Some(4), ShadingMode::SolidHdr => Some(5),
                ShadingMode::Wireframe => Some(6),
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.z_pie, &z_def, active_shading);
        }

        // View-Pie overlay (backtick key) — uses shared draw_pie_menu
        {
            let view_def = crate::pie_menu::view_pie_def();
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.view_pie, &view_def, self.ui.last_view_idx);
        }

        // Tab-Pie selection mode overlay (inner ring only, no tools)
        {
            let tab_def = crate::pie_menu::tab_pie_def_ex(
                self.ui.pie_mode,
                self.poly_pie_shows_group(),
            );
            let active_inner = match self.ui.pie_mode {
                crate::pie_menu::PieMode::Sdf => {
                    // SDF: 0=Object(top), 1=Prim(right), 3=Cycle(left)  (idx 2 hidden)
                    if self.ui.sdf_cycle_mode {
                        Some(3) // Cycle active
                    } else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0), // Object = top
                            WorkMode::Vertex => Some(1), // Prim = right
                            _ => None,
                        }
                    }
                }
                crate::pie_menu::PieMode::Poly => {
                    // Poly (8 sectors, Blender): 0=Object(top), 2=Face(right), 3=Group(bottom-right),
                    // 4=Edge(bottom), 5=Sculpt(diag), 6=Vertex(left)
                    if self.modeling_context == ModelingContext::Polygonal {
                        match self.poly_work_mode {
                            PolyWorkMode::Object => Some(0),
                            PolyWorkMode::Face => Some(2),
                            PolyWorkMode::Group => Some(3),
                            PolyWorkMode::Edge   => Some(4), PolyWorkMode::Vertex => Some(6),
                        }
                    } else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0), WorkMode::Face   => Some(2),
                            WorkMode::Edge   => Some(4), WorkMode::Sculpt => Some(5),
                            WorkMode::Vertex => Some(6),
                        }
                    }
                }
                crate::pie_menu::PieMode::Nurbs => {
                    // Same as poly: 0 Object, 2 Face, 4 Edge, 6 Vertex
                    match self.nurbs_work_mode {
                        NurbsWorkMode::Object => Some(0),
                        NurbsWorkMode::Face => Some(2),
                        NurbsWorkMode::Edge => Some(4),
                        NurbsWorkMode::Vertex => Some(6),
                    }
                }
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.tab_pie, &tab_def, active_inner);
        }

        // Reset-Pie overlay — transform reset options
        {
            let reset_def = crate::pie_menu::reset_pie_def();
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.reset_pie, &reset_def, None);
        }
        // Spine corner type pie (B while editing SpinePipe points)
        if self.ui.spine_corner_pie.open {
            let def = crate::pie_menu::spine_corner_pie_def();
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.spine_corner_pie, &def, None);
        }

        // F-Pie overlay — Focus Camera / Isolate
        {
            let f_def = crate::pie_menu::f_pie_def(self.scene.isolated_body_nid.is_some(), self.isolate_context_ghost);
            // Highlight the Isolate item (idx 1) when isolation is active
            let active = if self.scene.isolated_body_nid.is_some() { Some(1) } else { None };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.f_pie, &f_def, active);
        }
        // Shift+O overlay pie
        {
            let o_def = crate::pie_menu::overlay_pie_def(self.viewport.show_overlays);
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.overlay_pie, &o_def, None);
        }
        // D-key pivot pie
        {
            let persistent = self.get_active_pivot_persistent();
            let d_def = crate::pie_menu::d_pie_def(persistent);
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.d_pie, &d_def, None);
        }
        // T-key Action Center pie
        if self.ui.t_pie.open {
            let t_def = crate::pie_menu::t_pie_def(self.tool.action_center.label());
            let active = match self.tool.action_center {
                crate::ActionCenter::Automatic => Some(0),
                crate::ActionCenter::Selection => Some(1),
                crate::ActionCenter::Workplane => Some(2),
                crate::ActionCenter::Element => Some(3),
                crate::ActionCenter::Screen => Some(4),
                crate::ActionCenter::Origin => Some(5),
                crate::ActionCenter::Pivot => Some(6),
                crate::ActionCenter::None => Some(7),
                crate::ActionCenter::SelectionCenterAutoAxis => None,
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.t_pie, &t_def, active);
        }
        // X-key poly boolean pie (Union / Difference / Intersect, cutter → targets)
        {
            let bool_def = crate::pie_menu::boolean_pie_def(self.ui.bool_pie_keep_cutter);
            // 8-sector pie: Keep Cutter is slot 6 (was 3 on the old 4-item pie).
            let active = if self.ui.bool_pie_keep_cutter { Some(6) } else { None };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.bool_pie, &bool_def, active);
        }
        // X-key SDF boolean pie (op type for selected prims / sub-bodies)
        if self.ui.sdf_bool_pie.open {
            let sdf_bool_def = crate::pie_menu::sdf_boolean_pie_def();
            // Highlight the current op of the active target (active prim, else first
            // selected SdfBody) — same "active item" convention as the tab/F pies.
            let active = if let Some(idx) = self.active_prim_index_unified() {
                // Prim: handle-mode priority (Seam op > Emboss/Deboss modifier > op).
                self.scene.primitives.get(idx)
                    .map(crate::pie_menu::sdf_bool_pie_active_slot)
            } else {
                self.sel.usel.selected_nodes()
                    .find_map(|nid| self.scene.scene_graph.body_op(nid))
                    .map(crate::pie_menu::sdf_bool_op_slot)
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.sdf_bool_pie, &sdf_bool_def, active);
        }
        // Ctrl+R SDF resolution pie
        self.draw_resolution_pie_overlay(ui.painter());
        // X boolean pie CENTER → special commands (Extract Intersection, …)
        if self.ui.sdf_special_pie.open {
            let def = crate::pie_menu::sdf_special_pie_def();
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.sdf_special_pie, &def, None);
        }

    }

    /// Ctrl+R SDF resolution pie — ONE draw path shared by the v1 overlays, the ui2
    /// scene overlays, and `ui2_draw_pies`. Current resolution slice highlighted;
    /// slices above the selectable ceiling draw disabled (see `resolution_pie_def`).
    pub(crate) fn draw_resolution_pie_overlay(&self, painter: &egui::Painter) {
        if !self.ui.res_pie.open {
            return;
        }
        let (cur, sel_max) = match self.resolution_target_body() {
            Some(b) => {
                let (c, m) = self.body_grid_res_state(b);
                (Some(c), m)
            }
            None => (None, crate::gpu_jobs::max_gridbacked_resolution()),
        };
        let def = crate::pie_menu::resolution_pie_def(sel_max);
        let active = cur.and_then(|c| {
            crate::properties_render::RES_LADDER.iter().position(|&r| r == c)
        });
        crate::pie_menu::draw_pie_menu(painter, &self.ui.res_pie, &def, active);
    }

    /// Perf HUD (CPU timings + terrain gauges).
    ///
    /// Drawn from **both** `draw_viewport_overlays` (legacy) and `ui2_draw_scene_overlays`
    /// (default shell). Before this shared call, gauges latched in poll but never painted
    /// under ui2.
    pub(crate) fn draw_perf_hud_overlay(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        if !self.ui.show_perf_hud {
            return;
        }
        let p = ui.painter();
        let x = rect.left() + 8.0;
        let mut y = if self.ui.show_profiler_overlay && !self.ui.prof_frame_times.is_empty() {
            // Position below existing profiler overlay
            let pass_lines = self.ui.prof_pass_times.iter().filter(|&&t| t > 0.001).count();
            rect.top() + 8.0 + (3 + pass_lines) as f32 * 14.0 + 20.0
        } else {
            rect.top() + 8.0
        };
        let font = egui::FontId::monospace(10.0);
        let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
        let dim = egui::Color32::from_gray(150);
        let cyan = egui::Color32::from_rgb(80, 220, 220);
        let yellow = egui::Color32::from_rgb(220, 200, 80);
        let white = egui::Color32::from_gray(220);
        let line_h = 13.0;
        let worker_stats: Vec<_> = self.job_dispatcher.as_ref()
            .map(|d| d.worker_stats().iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let active_count = worker_stats.iter().filter(|s| s.healthy && {
            self.worker_gpus.get(s.worker_idx).map_or(false, |w| w.enabled)
        }).count();
        let total_workers = worker_stats.len();
        let has_worker = total_workers > 0;
        let worker_lines = if has_worker { 1 + total_workers } else { 0 };
        let brick_lines = if self.cache.body_vol_cache.use_brick_pool { 1 } else { 0 };
        let sched_lines = if self.scheduler.work_log.is_empty() { 0 } else { 1 };
        let terrain_lines = if self.perf.terrain_tiles > 0 { 1 } else { 0 };
        let num_lines = 21 + if self.cache.use_body_vol_cache { 1 } else { 0 } + brick_lines + worker_lines + sched_lines + terrain_lines;
        let hud_rect = egui::Rect::from_min_size(
            egui::pos2(x - 4.0, y - 4.0),
            egui::vec2(310.0, num_lines as f32 * line_h + 12.0),
        );
        p.rect_filled(hud_rect, 4.0, bg);
        let fps = if self.perf.frame_time_ms > 0.01 { 1000.0 / self.perf.frame_time_ms } else { 0.0 };
        let fps_col = if fps >= 55.0 { egui::Color32::from_rgb(80, 220, 80) } else if fps >= 30.0 { yellow } else { egui::Color32::from_rgb(220, 80, 80) };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("{:.0} FPS | Frame {:.2}ms", fps, self.perf.frame_time_ms), font.clone(), fps_col);
        y += line_h;
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("GPU dispatch: {:.2}ms", self.perf.dispatch_gpu_ms), font.clone(), cyan);
        y += line_h;
        // PLAN_FOR_GROK_VIEWPORT_PERF P2: name the poll bottleneck (not a black rest(top)).
        let poll_col = if self.perf.poll_tasks_ms > 16.0 {
            egui::Color32::from_rgb(220, 80, 80)
        } else if self.perf.poll_tasks_ms > 8.0 {
            yellow
        } else {
            dim
        };
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Poll {:.1}ms | top {} {:.1} | 2nd {} {:.1} | rest {:.1} | maps {}",
                self.perf.poll_tasks_ms,
                self.perf.poll_top_name,
                self.perf.poll_top_ms,
                self.perf.poll_2nd_name,
                self.perf.poll_2nd_ms,
                self.perf.poll_rest_ms,
                self.perf.poll_map_uploads,
            ),
            font.clone(),
            poll_col,
        );
        y += line_h;
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Serialize:    {:.2}ms", self.perf.serialize_graph_ms), font.clone(), dim);
        y += line_h;
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Sel resolve:  {:.3}ms", self.perf.selected_resolve_ms), font.clone(), dim);
        y += line_h;
        // POLY_VIEWPORT_PERF P0/P1a — orbit goal: mod_evals = 0
        let evals = self.poly_modifier_evals_last_frame;
        let eval_col = if evals == 0 {
            egui::Color32::from_rgb(80, 220, 80)
        } else {
            egui::Color32::from_rgb(220, 80, 80)
        };
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Poly mod evals: {} | sel outline {:.2}ms",
                evals, self.poly_sel_outline_ms
            ),
            font.clone(),
            eval_col,
        );
        y += line_h;
        let prefix_mb = self.poly_prefix_total_bytes as f32 / (1024.0 * 1024.0);
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Poly prefix: {} bodies | {:.1} MB",
                self.poly_prefix_cache.len(),
                prefix_mb
            ),
            font.clone(),
            dim,
        );
        y += line_h;
        // P6.2 wire budget honesty
        let wire_col = if self.overlay_wire_capped {
            egui::Color32::from_rgb(220, 80, 80)
        } else {
            dim
        };
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Wire segs: {}{} | frustum {}/{} | occ {}/{} | inst +{}",
                self.overlay_wire_seg_count,
                if self.overlay_wire_capped { " CAP" } else { "" },
                self.render.last_frustum_culled,
                self.render.last_frustum_total,
                self.render.last_occlusion_culled,
                self.render.last_occlusion_tested,
                self.render.last_instance_merged,
            ),
            font.clone(),
            wire_col,
        );
        y += line_h;
        // PLAN_FOR_GROK_VIEWPORT_PERF: mark-then-drain coalescing proof
        let sync_col = if self.poly_sync_marks_last_frame > self.poly_sync_calls_last_frame.max(1)
        {
            egui::Color32::from_rgb(80, 220, 80) // coalesced
        } else {
            dim
        };
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Poly sync: {} calls | {} marks",
                self.poly_sync_calls_last_frame, self.poly_sync_marks_last_frame
            ),
            font.clone(),
            sync_col,
        );
        y += line_h;
        // PLAN_FOR_GROK_VIEWPORT_PERF P3/P5 gate: LOD levels + full vs drawn tris.
        // Cluster LOD (P5) only if drawn tris stay the bottleneck after P1–P4.
        let (pl0, pl1, pl2, full_t, draw_t) = self.poly_lod_hud_stats();
        let lod_bodies = pl0 + pl1 + pl2;
        let lod_col = if !self.cache.poly_lod_auto {
            dim
        } else if draw_t + 1 < full_t {
            egui::Color32::from_rgb(80, 220, 80) // actually reduced
        } else if lod_bodies > 0 {
            yellow
        } else {
            dim
        };
        p.text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            format!(
                "Poly LOD: L0={} L1={} L2={} | tris {}→{} | shared {}",
                pl0,
                pl1,
                pl2,
                full_t,
                draw_t,
                self.poly_lod_shared.len(),
            ),
            font.clone(),
            lod_col,
        );
        y += line_h;
        let sync_col = if self.perf.sync_flat_cache_count > 0 { yellow } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Sync flat:    {:.2}ms x{}", self.perf.sync_flat_cache_ms, self.perf.sync_flat_cache_count), font.clone(), sync_col);
        y += line_h;
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Prims: {} | Lights: {} | Nodes: {}", self.perf.prim_count, self.perf.light_count, self.perf.node_count), font.clone(), white);
        y += line_h;
        let dirty_str = if self.perf.dirty_flags_this_frame == 0 { "none".to_string() } else {
            let mut parts = Vec::new();
            if self.perf.dirty_flags_this_frame & DirtyFlags::SCENE != 0 { parts.push("SCN"); }
            if self.perf.dirty_flags_this_frame & DirtyFlags::RENDER != 0 { parts.push("RND"); }
            if self.perf.dirty_flags_this_frame & DirtyFlags::MESH != 0 { parts.push("MSH"); }
            if self.perf.dirty_flags_this_frame & DirtyFlags::LIGHT != 0 { parts.push("LIT"); }
            if self.perf.dirty_flags_this_frame & DirtyFlags::SVO != 0 { parts.push("SVO"); }
            parts.join("|")
        };
        let dirty_col = if self.perf.dirty_flags_this_frame != 0 { yellow } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Dirty: {}", dirty_str), font.clone(), dirty_col);
        y += line_h;
        let realloc_col = if self.perf.gpu_buffer_reallocs > 0 { egui::Color32::from_rgb(220, 80, 80) } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("GPU reallocs: {}", self.perf.gpu_buffer_reallocs), font.clone(), realloc_col);
        y += line_h;
        // --- Frame pacing metrics ---
        let poll_col = if self.perf.poll_tasks_ms > 5.0 { yellow } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
            format!("Poll: {:.1}ms | Bake: {:.1}ms x{}",
                self.perf.poll_tasks_ms, self.perf.bake_readback_ms,
                self.perf.bake_readback_count),
            font.clone(), poll_col);
        y += line_h;
        let switch_col = if self.perf.path_switch_count > 0 || self.perf.halfres_transition_count > 0 { yellow } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
            format!("Switches: {}/s | HalfRes: {}/s | Pipe: {}",
                self.perf.path_switch_count, self.perf.halfres_transition_count,
                self.perf.prev_pipeline_label),
            font.clone(), switch_col);
        y += line_h;
        let spike_col = if self.perf.consecutive_spike_count >= 3 { egui::Color32::from_rgb(220, 80, 80) }
            else if self.perf.consecutive_spike_count > 0 { yellow } else { dim };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
            format!("Spikes: {} consecutive | Warm: {}/{}",
                self.perf.consecutive_spike_count,
                self.perf.pipelines_warm, self.perf.pipelines_total),
            font.clone(), spike_col);
        y += line_h;
        // Bodies count
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
            format!("Bodies: {}", self.perf.body_count), font.clone(), dim);
        y += line_h;
        // Gate 0 — DC-consolidation baseline metrics: per-body DC populate ms, full/partial
        // rebakes + %incremental, live resident-mesh pool VRAM + hole ratio, drag frame p95
        // (percentile over the existing frame-time ring). These were dead/stderr-only before.
        let g0_total = self.perf.vol_full_rebakes + self.perf.vol_partial_rebakes;
        let g0_inc = if g0_total > 0 { self.perf.vol_partial_rebakes as f32 * 100.0 / g0_total as f32 } else { 0.0 };
        p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
            format!("DC {:.1}ms reB f{}/p{} {:.0}%inc pool {:.0}MB h{:.0}% p95 {:.1}ms",
                self.perf.vol_dc_ms, self.perf.vol_full_rebakes, self.perf.vol_partial_rebakes, g0_inc,
                self.perf.sdf_pool_body_bytes as f64 / (1024.0 * 1024.0),
                self.perf.sdf_pool_hole_ratio * 100.0, self.perf.frame_percentile(0.95)),
            font.clone(), cyan);
        y += line_h;
        // Mesh Terrain (plan Phase 1 table: bake / DC / tris / dirty) — only when a world exists.
        if self.perf.terrain_tiles > 0 {
            let t = &self.perf;
            p.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_TOP,
                format!(
                    "Terrain tiles {}/{} | bake {:.0}ms | dc {:.1}ms | remesh {:.0}ms | tris {} | dirty b{}/r{}{}",
                    t.terrain_vis,
                    t.terrain_tiles,
                    t.terrain_last_bake_ms,
                    t.terrain_last_dc_ms,
                    t.terrain_last_remesh_ms,
                    t.terrain_tris,
                    t.terrain_dirty_bodies,
                    t.terrain_dirty_regions,
                    if t.terrain_pending_remesh > 0 {
                        format!(" | remeshQ{}", t.terrain_pending_remesh)
                    } else {
                        String::new()
                    },
                ),
                font.clone(),
                yellow,
            );
            y += line_h;
        }
        // Per-body volume cache stats
        if self.cache.use_body_vol_cache {
            let hits = self.cache.body_vol_cache.cache_hits;
            let total = self.cache.body_vol_cache.cache_total;
            let pending = if self.cache.body_vol_cache.has_pending() { " ⏳" } else { "" };
            let bvc_col = if hits == total && total > 0 { egui::Color32::from_rgb(80, 220, 80) } else { yellow };
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, format!("Vol cache: {}/{}{}", hits, total, pending), font.clone(), bvc_col);
            y += line_h;
        }
        if self.cache.body_vol_cache.use_brick_pool {
            // used/capacity only — free field reads (always updated). Per-body active count
            // was removed: active_bricks() is O(indirection) and HUD was the only consumer.
            let bp = &self.perf;
            p.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_TOP,
                format!("Bricks: {}/{}", bp.brick_used, bp.brick_capacity),
                font.clone(),
                dim,
            );
            y += line_h;
        }
        if has_worker {
            let worker_blue = egui::Color32::from_rgb(120, 180, 255);
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP,
                format!("Workers: {}/{} active", active_count, total_workers),
                font.clone(), worker_blue);
            y += line_h;
            for s in &worker_stats {
                let enabled = self.worker_gpus.get(s.worker_idx).map_or(false, |w| w.enabled);
                let col = if !s.healthy {
                    egui::Color32::from_rgb(220, 80, 80)
                } else if s.jobs_failed > 0 {
                    yellow
                } else {
                    egui::Color32::from_rgb(80, 220, 80)
                };
                let name = if s.adapter_name.len() > 24 { &s.adapter_name[..24] } else { &s.adapter_name };
                let status = if !s.healthy {
                    format!("{}: LOST", name)
                } else if !enabled {
                    format!("{}: disabled", name)
                } else if s.jobs_completed > 0 || s.jobs_failed > 0 {
                    format!("{}: {} jobs, last {:.1}ms ({})", name, s.jobs_completed, s.last_duration_ms, s.last_job_kind)
                } else {
                    format!("{}: idle", name)
                };
                p.text(egui::pos2(x + 8.0, y), egui::Align2::LEFT_TOP, status, font.clone(), col);
                y += line_h;
            }
            // Undo the extra y increment from the loop — the last line_h was already accounted
            // Actually no, we need to leave y where it is for any further content
        }
        // Scheduler work log
        if !self.scheduler.work_log.is_empty() {
            let mut parts = Vec::new();
            for entry in &self.scheduler.work_log {
                if entry.ran {
                    parts.push(format!("{}: {:.1}ms", entry.name, entry.actual_ms));
                } else {
                    parts.push(format!("{}: SKIP", entry.name));
                }
            }
            let sched_str = format!("[Sched] {}", parts.join(" | "));
            let sched_col = if parts.iter().any(|s| s.contains("SKIP")) { yellow } else { dim };
            p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, sched_str, font.clone(), sched_col);
            // y += line_h; // not needed — last line in HUD
        }

    }

    /// ui2 scene overlays — gizmo, create HUD, selection, pies — without the legacy
    /// floating bottom toolbar / top-right strip (those are ui2 chrome now).
    ///
    /// **Clip:** everything paints only inside `rect` (scene above the library). Without
    /// this, workplane/grid lines project into the library band and draw on top of it.
    pub(crate) fn ui2_draw_scene_overlays(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: egui::Rect,
    ) {
        if rect.width() < 40.0 || rect.height() < 40.0 {
            return;
        }
        let _t_scene_t0 = std::time::Instant::now();
        let (mut _t_workplane, mut _t_selhl, mut _t_nodeoutl, mut _t_lights, mut _t_polyov) =
            (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        // Checkpoints that BISECT the ungated tail. With all seven named timers reading 0.0 and
        // `rest` holding 40ms, the spender is one of ~15 ungated draws; two cumulative marks split
        // them into thirds so one run names the third instead of another guess per call.
        let (mut _ck1, mut _ck2) = (0.0f32, 0.0f32);
        let prev_clip = ui.clip_rect();
        ui.set_clip_rect(rect.intersect(prev_clip));

        let shift = ctx.input(|i| i.modifiers.shift);
        let ctrl = ctx.input(|i| i.modifiers.command);
        let overlays_on = self.viewport.show_overlays;

        // Smart snap pre-highlight whenever magnet / Ctrl is on (same as full overlay path).
        let snap_active = self.is_snap_active(ctrl);
        if snap_active {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos())) {
                // Only snap-query while pointer is over the scene (not the library).
                if rect.contains(mp) {
                    let exclude = self.tool.current.as_create().map(|cs| cs.prim_idx);
                    self.viewport.smart_snap_result = self.find_smart_snap(mp, rect, exclude);
                } else {
                    self.viewport.smart_snap_result = None;
                }
            }
        } else {
            self.viewport.smart_snap_result = None;
        }

        if overlays_on {
            self.draw_grid_overlay(ui.painter(), rect);
        }
        if overlays_on && self.viewport.show_workplane {
            let _t = std::time::Instant::now();
            self.draw_workplane_overlay(ui.painter(), rect);
            _t_workplane = _t.elapsed().as_secs_f32() * 1000.0;
        }

        let prim_overlay_mode = self.tool.work_mode == WorkMode::Object
            || (self.tool.work_mode == WorkMode::Vertex
                && self.modeling_context == ModelingContext::Sdf);
        if overlays_on && self.viewport.show_selection_ui && prim_overlay_mode {
            let _t = std::time::Instant::now();
            self.draw_selection_highlights(ui.painter(), rect);
            _t_selhl = _t.elapsed().as_secs_f32() * 1000.0;
        }
        if self.tool.work_mode == WorkMode::Object {
            if overlays_on && self.viewport.show_selection_ui {
                let _t = std::time::Instant::now();
                self.draw_node_selection_outlines(ui.painter(), rect);
                _t_nodeoutl = _t.elapsed().as_secs_f32() * 1000.0;
            }
            self.draw_billboard_camera_icon(ctx, ui.painter(), rect);
            if overlays_on && self.viewport.show_light_gizmos {
                let _t = std::time::Instant::now();
                self.draw_light_gizmos(ui.painter(), rect);
                _t_lights = _t.elapsed().as_secs_f32() * 1000.0;
            }
        }

        // Live transform badge. The GIZMO is drawn after the poly overlays below — see the note
        // there ("gizmo render under polygroup"): this painter has no depth buffer, so anything
        // drawn later simply covers it.
        self.draw_live_transform_values(ui.painter(), rect);
        // Selection pivot / center (yellow) — shown during center-menu / pivot-edit flow.
        self.draw_selection_pivot_marker(ui.painter(), rect);

        if overlays_on
            && (self.tool.work_mode == WorkMode::Object || self.tool.work_mode == WorkMode::Vertex)
        {
            let m = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_blend_k_handle(ui.painter(), rect, m);
        }
        self.draw_blend_k_popup(ctx);
        self.draw_depth_pick_overlay(ui.painter(), rect);
        self.draw_symmetry_plane(ui.painter(), rect);
        self.draw_dirty_regions_overlay(ui.painter(), rect);
        _ck1 = _t_scene_t0.elapsed().as_secs_f32() * 1000.0; // cumulative mark A

        // Create / curve tools.
        //
        // SUB-TIMERS for the ckA..ckB middle third. Owner run 2026-08-12 (`tool=Poly Transform`,
        // orbiting) reads `[SceneOv] rest 35.8 || ckA 0.0 ckB 35.8` — every named sub-timer is 0.0
        // and the whole overlay cost is in HERE, unattributed. Twelve calls, so one macro per call
        // rather than another guess; same 8 ms threshold gate as `[SceneOv]`, so healthy frames
        // pay nothing and the steady state shows rather than only spikes.
        macro_rules! mid {
            ($slot:expr, $call:expr) => {{
                let _t = std::time::Instant::now();
                $call;
                $slot = _t.elapsed().as_secs_f32() * 1000.0;
            }};
        }
        let (mut _m_create, mut _m_clip, mut _m_cpt, mut _m_spine, mut _m_ctool) =
            (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut _m_chud, mut _m_cdim, mut _m_thud, mut _m_bevel) =
            (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut _m_region, mut _m_knife, mut _m_frame) = (0.0f32, 0.0f32, 0.0f32);
        mid!(_m_create, self.draw_create_overlay(ui.painter(), rect));
        mid!(_m_clip, self.draw_clip_curve_overlay(ui.painter(), rect));
        mid!(_m_cpt, self.draw_curve_point_overlay(ui.painter(), rect));
        mid!(_m_spine, self.draw_spine_point_overlay(ui.painter(), rect));
        mid!(_m_ctool, self.draw_curve_edge_toolbar(ui, rect));
        {
            let m = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            mid!(_m_chud, self.draw_create_hud(ui.painter(), m));
        }
        mid!(_m_cdim, self.draw_create_dimensions(ui.painter(), rect));
        mid!(_m_thud, self.draw_active_tool_hud(ui.painter(), rect));

        // Bevel handles (selected box/cyl) — ONLY while ToolMode::Bevel is on.
        // Do not sticky-show from radii (that left B chrome + yellow square over the blend-k
        // diamond after tool switch — same class as the input-side fix).
        mid!(_m_bevel, {
            let bevel_indices = self.selected_prim_indices_unified();
            if self.tool.mode.is_bevel()
                && self.tool.work_mode == WorkMode::Object
                && bevel_indices.len() == 1
            {
                let idx = bevel_indices[0];
                if matches!(
                    self.scene.primitives.get(idx).map(|p| &p.shape),
                    Some(SdfShape::Box { .. } | SdfShape::Cylinder { .. })
                ) {
                    self.draw_bevel_handles(ui.painter(), rect, ctx);
                }
            }
        });

        // Shape / region select rubber-band — UNGATED (must paint even if "Poly Overlays" is off).
        // Owner 2026-08-06: three passes failed because v2 never called this (only the legacy
        // draw_viewport_overlays path did). Alt+LMB PolyRegionSelect was live; the stroke was invisible.
        mid!(_m_region, self.draw_poly_region_select_overlay(ui.painter()));
        // Knife R/C rubber-band — ungated, same as region select (v2 path).
        mid!(_m_knife, self.draw_knife_shape_select_overlay(ui.painter(), rect));
        // Viewport frame status (record / broken geometry / render progress / unsaved / VRAM).
        // Drawn from BOTH overlay paths for the same reason the region-select band is - a single
        // call site means the v1 and v2 shells silently disagree about what the user can see.
        mid!(_m_frame, self.draw_viewport_frame_indicator(ui.painter(), rect));
        _ck2 = _t_scene_t0.elapsed().as_secs_f32() * 1000.0; // cumulative mark B
        // Name the ckA..ckB middle third. Gated on the SAME 8 ms threshold as `[SceneOv]`.
        if _ck2 - _ck1 > 8.0 || crate::perf_spam_on() {
            eprintln!(
                "[MidOv] {:.1}ms = create {:.1} clip {:.1} curvept {:.1} spine {:.1} ctoolbar {:.1}                  chud {:.1} cdim {:.1} toolhud {:.1} bevelh {:.1} region {:.1} knife {:.1} frame {:.1}",
                _ck2 - _ck1, _m_create, _m_clip, _m_cpt, _m_spine, _m_ctool, _m_chud, _m_cdim,
                _m_thud, _m_bevel, _m_region, _m_knife, _m_frame,
            );
        }
        // Poly selection/hover + live tool feedback — UNGATED (v2 path).
        // Same reason as the legacy path above: Overlays / Poly Overlays are display toggles
        // and must not hide component selection or active-tool chrome.
        {
            let _t = std::time::Instant::now();
            self.draw_poly_overlays(ui.painter(), rect);
            _t_polyov = _t.elapsed().as_secs_f32() * 1000.0;
        }
        self.draw_loop_cut_preview(ui.painter(), rect);
        self.draw_bevel_hud(ctx, rect);
        self.draw_extrude_hud(ui.painter(), rect);
        self.draw_extrude_sweep_overlay(ui.painter(), rect);
        self.draw_inset_hud(ui.painter(), rect);
        self.draw_knife_overlay(ui.painter(), rect);
        self.draw_poly_box_select_overlay(ui.painter());
        {
            let mp = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(0.0, 0.0)));
            self.draw_poly_circle_select_overlay(ui.painter(), mp);
            self.draw_poly_paint_select_overlay(ui.painter(), mp, self.poly_paint_radius);
        }
        let mut _t_nurbs = 0.0f32;
        if overlays_on && self.viewport.show_poly_overlays {
            let _t = std::time::Instant::now();
            self.draw_nurbs_overlays(ui.painter(), rect);
            _t_nurbs = _t.elapsed().as_secs_f32() * 1000.0;
        }
        // TRANSFORM GIZMO — after the poly overlays, same reason as the other draw path in this
        // file: the polygroup fills share this painter and would otherwise cover the arrows.
        let mut _t_gizmo = 0.0f32;
        if overlays_on && self.viewport.show_gizmo {
            let _t = std::time::Instant::now();
            self.draw_gizmo(ui.painter(), rect);
            self.draw_falloff_gizmo(ui.painter(), rect);
            self.draw_mask_gizmo(ui.painter(), rect);
            _t_gizmo = _t.elapsed().as_secs_f32() * 1000.0;
        }
        // SUB-TIMERS for the `[Overlays] scene` bucket. The coarse bucket named this function as
        // the frame's whole cost (39 of 39.4ms) but not WHICH draw — and the two obvious suspects
        // were both wrong: selection outlines (cost is selection-INdependent) and the grid (a
        // no-op). Same threshold gate as `[Overlays]` so the steady state shows, not just spikes.
        let _scene_ms = _t_scene_t0.elapsed().as_secs_f32() * 1000.0;
        if _scene_ms > 8.0 || crate::perf_spam_on() {
            eprintln!(
                "[SceneOv] {:.1}ms = workplane {:.1} + selhl {:.1} + nodeoutl {:.1} + lights {:.1} \
                 + nurbs {:.1} + gizmo {:.1} + polyov {:.1} | rest {:.1} \
                 || ckA {:.1} ckB {:.1} (A=start..dirty_regions, B=..frame_indicator)",
                _scene_ms, _t_workplane, _t_selhl, _t_nodeoutl, _t_lights, _t_nurbs, _t_gizmo,
                _t_polyov,
                _scene_ms - _t_workplane - _t_selhl - _t_nodeoutl - _t_lights - _t_nurbs
                    - _t_gizmo - _t_polyov,
                _ck1,
                _ck2,
            );
        }

        // (draw_wireframe_overlay DELETED on master — the object-mode wire is now a
        // depth-tested GPU line pass: overlay_lines.rs gather + viewport_render draw.)
        self.draw_bool_edges(ui.painter());

        if self.viewport.snap_indicator.is_some() {
            self.draw_snap_indicator(ui.painter(), rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.viewport.smart_snap_result.is_some() {
            self.draw_smart_snap_indicator(ui.painter(), rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Smart-snap bar controls live in the ctxbar Snap menu (ui2) — no floating bar.
        if self.ui.object_link_gesture.as_ref().is_some_and(|g| g.armed) {
            self.object_link_draw_overlay(ctx, rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        if self.tool.current.is_smart_mirror() {
            self.draw_smart_mirror(ui.painter(), rect);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.tool.current.is_selection_box() {
            let m = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(0.0, 0.0)));
            self.draw_selection_overlay(ui.painter(), m);
        }
        if self.tool.work_mode == WorkMode::Sculpt {
            let m = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_brush_preview(ui.painter(), rect, m, shift, ctrl);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Proportional Edit falloff ring + origin handle — see the twin call in the legacy
        // overlay path above; both draw sites stay in lock-step.
        {
            let m = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_poly_prop_overlay(ui.painter(), rect, m);
        }
        if self.sdf_sculpt_active
            && (self.tool.work_mode == WorkMode::Object || self.tool.work_mode == WorkMode::Sculpt)
        {
            let m = ctx
                .input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(-100.0, -100.0)));
            self.draw_sdf_brush_preview(ui.painter(), rect, m);
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        // Pie menus (Z / View / Tab / Reset / F / D / boolean) — same as legacy path
        {
            let z_def = crate::pie_menu::z_pie_def();
            let active_shading = match self.render.shading_mode {
                ShadingMode::Pbr => Some(0),
                ShadingMode::Raytrace => Some(1),
                ShadingMode::PathTrace => Some(2),
                ShadingMode::MatCap => Some(3),
                ShadingMode::Lit => Some(4),
                ShadingMode::SolidHdr => Some(5),
                ShadingMode::Wireframe => Some(6),
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.z_pie, &z_def, active_shading);
        }
        {
            let view_def = crate::pie_menu::view_pie_def();
            crate::pie_menu::draw_pie_menu(
                ui.painter(),
                &self.ui.view_pie,
                &view_def,
                self.ui.last_view_idx,
            );
        }
        {
            let tab_def = crate::pie_menu::tab_pie_def_ex(
                self.ui.pie_mode,
                self.poly_pie_shows_group(),
            );
            let active_inner = match self.ui.pie_mode {
                crate::pie_menu::PieMode::Sdf => {
                    if self.ui.sdf_cycle_mode {
                        Some(3)
                    } else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0),
                            WorkMode::Vertex => Some(1),
                            _ => None,
                        }
                    }
                }
                crate::pie_menu::PieMode::Poly => {
                    if self.modeling_context == ModelingContext::Polygonal {
                        match self.poly_work_mode {
                            PolyWorkMode::Object => Some(0),
                            PolyWorkMode::Face => Some(2),
                            PolyWorkMode::Group => Some(3),
                            PolyWorkMode::Edge => Some(4),
                            PolyWorkMode::Vertex => Some(6),
                        }
                    } else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0),
                            WorkMode::Face => Some(2),
                            WorkMode::Edge => Some(4),
                            WorkMode::Sculpt => Some(5),
                            WorkMode::Vertex => Some(6),
                        }
                    }
                }
                crate::pie_menu::PieMode::Nurbs => match self.nurbs_work_mode {
                    NurbsWorkMode::Object => Some(0),
                    NurbsWorkMode::Face => Some(2),
                    NurbsWorkMode::Edge => Some(4),
                    NurbsWorkMode::Vertex => Some(6),
                },
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.tab_pie, &tab_def, active_inner);
        }
        {
            let reset_def = crate::pie_menu::reset_pie_def();
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.reset_pie, &reset_def, None);
        }
        {
            let f_def = crate::pie_menu::f_pie_def(
                self.scene.isolated_body_nid.is_some(),
                self.isolate_context_ghost,
            );
            let active = if self.scene.isolated_body_nid.is_some() {
                Some(1)
            } else {
                None
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.f_pie, &f_def, active);
        }
        {
            let o_def = crate::pie_menu::overlay_pie_def(self.viewport.show_overlays);
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.overlay_pie, &o_def, None);
        }
        {
            let persistent = self.get_active_pivot_persistent();
            let d_def = crate::pie_menu::d_pie_def(persistent);
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.d_pie, &d_def, None);
        }
        {
            let bool_def = crate::pie_menu::boolean_pie_def(self.ui.bool_pie_keep_cutter);
            // 8-sector pie: Keep Cutter is slot 6 (was 3 on the old 4-item pie).
            let active = if self.ui.bool_pie_keep_cutter {
                Some(6)
            } else {
                None
            };
            crate::pie_menu::draw_pie_menu(ui.painter(), &self.ui.bool_pie, &bool_def, active);
        }
        if self.ui.sdf_bool_pie.open {
            let sdf_bool_def = crate::pie_menu::sdf_boolean_pie_def();
            let active = if let Some(idx) = self.active_prim_index_unified() {
                self.scene
                    .primitives
                    .get(idx)
                    .map(crate::pie_menu::sdf_bool_pie_active_slot)
            } else {
                self.sel
                    .usel
                    .selected_nodes()
                    .find_map(|nid| self.scene.scene_graph.body_op(nid))
                    .map(crate::pie_menu::sdf_bool_op_slot)
            };
            crate::pie_menu::draw_pie_menu(
                ui.painter(),
                &self.ui.sdf_bool_pie,
                &sdf_bool_def,
                active,
            );
        }
        // Ctrl+R SDF resolution pie
        self.draw_resolution_pie_overlay(ui.painter());

        // ── Sculpt tool header (top of the 3D view, Blender VIEW3D header) ──
        // Owner: Add/Subtract, Size, Strength, Falloff, Brush / Texture / Stroke
        // live ON the viewport — the empty strip above the mesh — not the ctx bar.
        if self.tool.work_mode == WorkMode::Sculpt {
            self.ui2_sculpt_header_overlay(ctx, ui, rect);
        }

        // ── Shading / render-mode bar (top-right, in-viewport) ────────────────
        // Owner: the shading strip lives here, not in the ctx bar. It only
        // interacts within its own button rects + popover Area — every other
        // pixel falls through to camera/tool input (orbit over empty viewport
        // must still work). Draw last so it sits above the scene overlays.
        self.ui2_shading_overlay(ctx, ui, rect);

        // Perf HUD must paint on the default ui2 shell (legacy-only path is dead for gauges).
        // Still inside scene clip so the panel does not spill into the library band.
        self.draw_perf_hud_overlay(ui, rect);

        // Restore clip so later CentralPanel widgets (library, docks) are not clipped
        // to the scene rect.
        ui.set_clip_rect(prev_clip);
    }

    /// Bevel debug overlay: corner points, edge midpoints, radius arcs, info text.
    /// Only draws for a single selected Box prim with any bevel radius > 0.
    fn draw_bevel_debug_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let idx = match self.selected_prim_indices_unified().first() { Some(&i) => i, None => return };
        let p = match self.scene.primitives.get(idx) { Some(p) => p, None => return };
        let (he, bevel, up_axis) = match &p.shape {
            SdfShape::Box { half_extents, bevel } => (*half_extents, bevel, bevel.up_axis),
            _ => return,
        };
        if !bevel.radii.iter().any(|r| *r > 0.001) { return; }

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let rot = p.rotation_quat();
        let scale = p.scale;
        let pos = p.position;
        // Body transform: prim positions are body-local, convert to world for screen projection.
        let bt = self.scene.prim_body_map.get(idx)
            .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
            .unwrap_or_default();
        let c = |x: f32, y: f32, z: f32| -> Vec3 {
            let body_local = rot * (Vec3::new(x, y, z) * scale) + pos;
            bt.local_to_world(body_local)
        };
        let w2s = |wp: Vec3| -> Option<egui::Pos2> { world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) };

        // 8 box vertices in local coords
        let verts_local: [(f32,f32,f32); 8] = [
            (-he.x,-he.y,-he.z), ( he.x,-he.y,-he.z),
            (-he.x,-he.y, he.z), ( he.x,-he.y, he.z),
            (-he.x, he.y,-he.z), ( he.x, he.y,-he.z),
            (-he.x, he.y, he.z), ( he.x, he.y, he.z),
        ];
        let verts: Vec<Vec3> = verts_local.iter().map(|&(x,y,z)| c(x,y,z)).collect();

        // Edge index tuples (same as box_bevel_edges)
        let edge_idx: [(usize,usize); 12] = match up_axis {
            1 => [(0,4),(2,6),(0,2),(4,6),(1,5),(3,7),(1,3),(5,7),(0,1),(4,5),(2,3),(6,7)],
            2 => [(0,1),(4,5),(0,4),(1,5),(2,3),(6,7),(2,6),(3,7),(0,2),(1,3),(4,6),(5,7)],
            _ => [(0,1),(2,3),(0,2),(1,3),(4,5),(6,7),(4,6),(5,7),(0,4),(1,5),(2,6),(3,7)],
        };

        // Which edge group each edge belongs to: 0-3=bot, 4-7=top, 8-11=vert
        // Count how many active groups touch each vertex
        let mut corner_active_groups: [u8; 8] = [0; 8];
        for (ei, &(va, vb)) in edge_idx.iter().enumerate() {
            if bevel.radii[ei] > 0.001 {
                let group = (ei / 4) as u8; // 0=bot, 1=top, 2=vert
                // Track unique groups per corner using bit flags
                corner_active_groups[va] |= 1 << group;
                corner_active_groups[vb] |= 1 << group;
            }
        }

        let green = egui::Color32::from_rgb(80, 220, 80);
        let red = egui::Color32::from_rgb(255, 60, 60);
        let magenta = egui::Color32::from_rgb(255, 80, 255);
        let gray = egui::Color32::from_rgb(100, 100, 100);
        let font_sm = egui::FontId::monospace(9.0);
        let font_info = egui::FontId::monospace(10.0);

        // 1. CORNER POINTS — 8 circles at box vertices
        for (vi, vert) in verts.iter().enumerate() {
            if let Some(sp) = w2s(*vert) {
                let bits = corner_active_groups[vi];
                let group_count = bits.count_ones();
                let color = if group_count >= 2 { red } else { green };
                painter.circle_filled(sp, 4.0, color);
                painter.circle_stroke(sp, 4.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
            }
        }

        // 2. EDGE MIDPOINTS — 12 dots at edge midpoints + radius labels
        for (ei, &(va, vb)) in edge_idx.iter().enumerate() {
            let mid = (verts[va] + verts[vb]) * 0.5;
            if let Some(sp) = w2s(mid) {
                let r = bevel.radii[ei];
                let active = r > 0.001;
                let color = if active { magenta } else { gray };
                painter.circle_filled(sp, 3.0, color);
                if active {
                    painter.text(
                        sp + egui::vec2(6.0, -4.0), egui::Align2::LEFT_CENTER,
                        format!("{:.3}", r), font_sm.clone(), magenta,
                    );
                }
            }
        }

        // 3. BEVEL RADII ARCS — two short lines from edge midpoint toward adjacent faces
        for (ei, &(va, vb)) in edge_idx.iter().enumerate() {
            let r = bevel.radii[ei];
            if r < 0.001 { continue; }
            let mid_world = (verts[va] + verts[vb]) * 0.5;
            let edge_dir = (verts[vb] - verts[va]).normalize_or_zero();
            // Compute two face normals perpendicular to this edge
            // The edge lies on two faces; approximate face normals from the box center
            let center = pos;
            let to_mid = (mid_world - center).normalize_or_zero();
            // face_n1 and face_n2: project to_mid perpendicular to edge_dir, then find two orthogonal dirs
            let proj = to_mid - edge_dir * to_mid.dot(edge_dir);
            let face_n1 = proj.normalize_or_zero();
            // Second face normal: use cross product for the other face
            let face_n2_raw = edge_dir.cross(face_n1);
            let face_n2 = face_n2_raw.normalize_or_zero();

            let arm_len = r.min(he.x.min(he.y).min(he.z) * 0.8);
            let tip1 = mid_world + face_n1 * arm_len;
            let tip2 = mid_world + face_n2 * arm_len;

            if let (Some(sp_mid), Some(sp_t1), Some(sp_t2)) = (w2s(mid_world), w2s(tip1), w2s(tip2)) {
                let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 200, 80));
                painter.line_segment([sp_mid, sp_t1], stroke);
                painter.line_segment([sp_mid, sp_t2], stroke);
                // Small dot at tips
                painter.circle_filled(sp_t1, 2.0, egui::Color32::from_rgb(255, 200, 80));
                painter.circle_filled(sp_t2, 2.0, egui::Color32::from_rgb(255, 200, 80));
            }
        }

        // 4. INFO TEXT — bottom-left corner
        let avg_r = |start: usize, end: usize| -> f32 {
            let mut sum = 0.0f32;
            let mut cnt = 0;
            for i in start..end {
                if bevel.radii[i] > 0.001 { sum += bevel.radii[i]; cnt += 1; }
            }
            if cnt > 0 { sum / cnt as f32 } else { 0.0 }
        };
        let r_bot = avg_r(0, 4);
        let r_top = avg_r(4, 8);
        let r_vert = avg_r(8, 12);
        let problem_corners = corner_active_groups.iter().filter(|&&b| b.count_ones() >= 2).count();

        let info_lines = [
            format!("Bot:  r={:.3} p={:.2} a={:.0}\u{00B0}", r_bot, p.bevel_profile_bot, p.chamfer_angle_bot),
            format!("Top:  r={:.3} p={:.2} a={:.0}\u{00B0}", r_top, p.bevel_profile_top, p.chamfer_angle_top),
            format!("Vert: r={:.3} p={:.2} a={:.0}\u{00B0}", r_vert, p.bevel_profile_vert, p.chamfer_angle_vert),
            format!("Active corners: {}/8 (2+ groups)", problem_corners),
        ];
        let x = rect.left() + 10.0;
        let mut y = rect.bottom() - 20.0 - (info_lines.len() as f32 * 14.0);
        let bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
        let info_rect = egui::Rect::from_min_size(
            egui::pos2(x - 4.0, y - 4.0),
            egui::vec2(240.0, info_lines.len() as f32 * 14.0 + 8.0),
        );
        painter.rect_filled(info_rect, 4.0, bg);
        for line in &info_lines {
            let col = if line.contains("Active corners") && problem_corners > 0 { red } else { egui::Color32::from_gray(200) };
            painter.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, line, font_info.clone(), col);
            y += 14.0;
        }
    }

    /// Debug overlay: draw wireframe boxes for dirty bake regions.
    /// Green = partial bake, Red = full grid. Alpha fades over 60 frames.
    fn draw_dirty_regions_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        if !self.show_dirty_regions || self.cache.body_vol_cache.debug_last_regions.is_empty() {
            return;
        }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let age = self.cache.body_vol_cache.debug_region_age;
        let alpha = ((1.0 - age as f32 / 60.0).max(0.0) * 200.0) as u8;
        if alpha == 0 { return; }

        let edges: [(usize, usize); 12] = [
            (0,1),(1,2),(2,3),(3,0), // bottom
            (4,5),(5,6),(6,7),(7,4), // top
            (0,4),(1,5),(2,6),(3,7), // verticals
        ];

        for &(body_nid, local_min, local_max, is_partial) in &self.cache.body_vol_cache.debug_last_regions {
            let bt = self.scene.scene_graph.body_world_transform(body_nid);

            let corners = [
                Vec3::new(local_min.x, local_min.y, local_min.z),
                Vec3::new(local_max.x, local_min.y, local_min.z),
                Vec3::new(local_max.x, local_max.y, local_min.z),
                Vec3::new(local_min.x, local_max.y, local_min.z),
                Vec3::new(local_min.x, local_min.y, local_max.z),
                Vec3::new(local_max.x, local_min.y, local_max.z),
                Vec3::new(local_max.x, local_max.y, local_max.z),
                Vec3::new(local_min.x, local_max.y, local_max.z),
            ];

            // Transform body-local corners to world space
            let world_corners: Vec<Vec3> = corners.iter().map(|&c| {
                if let Some(ref bt) = bt { bt.local_to_world(c) } else { c }
            }).collect();

            // Project to screen and draw edges
            let col = if is_partial {
                egui::Color32::from_rgba_unmultiplied(60, 220, 60, alpha)
            } else {
                egui::Color32::from_rgba_unmultiplied(220, 60, 60, alpha)
            };
            let stroke = egui::Stroke::new(1.5, col);

            for &(a, b) in &edges {
                if let (Some(sa), Some(sb)) = (
                    world_to_screen(world_corners[a], cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                    world_to_screen(world_corners[b], cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                ) {
                    painter.line_segment([sa, sb], stroke);
                }
            }
        }
    }
}
