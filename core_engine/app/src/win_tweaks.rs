// win_tweaks.rs — Developer tweak panel (live-adjustable tool settings)
// Extracted from ui_windows.rs
use crate::ui_menus::DefaultMenu;
use eframe::egui;
use crate::*;

impl AzraApp {
    // ================================================================
    // Developer Tweak Panel — live-adjustable tool settings
    // ================================================================
    pub(crate) fn draw_tweak_panel(&mut self, ctx: &egui::Context) {
        // Closed = free. This fn had NO early-return, and `lab_has_convertible_poly()`
        // below walks EVERY half-edge of every visible poly body (is_closed scan) — 6.5 ms
        // per frame at 3.1M tris, paid by every user on every frame for a button state in
        // a dev panel that is almost always closed (PERF_TARGETS `lay` residual, [LAY3]).
        if !self.ui.show_tweak_panel {
            return;
        }
        let mut open = self.ui.show_tweak_panel;
        let march_render_dirty = std::cell::Cell::new(false);
        // LAB poly→SDF: computed before the `&mut self.tweak` borrow; the button click is
        // deferred past the panel closure (same pattern as march_render_dirty) because
        // convert_selected_poly_to_sdf() needs `&mut self`. Only runs with the panel OPEN
        // now (guard above) — the O(half_edges) closed-mesh scan is a dev-panel cost.
        let lab_has_poly = self.lab_has_convertible_poly();
        let do_lab_convert = std::cell::Cell::new(false);
        // Foreground: drawn after ui2 shell (see app_update), but docks/tool-options
        // also use Foreground — order within the layer is creation order, so this window
        // must be created AFTER ui2_draw. Tooltip would sit above everything including
        // legit tooltips; Foreground is correct for a modeless tool window.
        egui::Window::new("Developer Tweaks")
            .open(&mut open)
            .default_width(280.0)
            .resizable(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Debug render-mode (moved here from the Viewport Overlays popover).
                    // Kept before the `&mut self.tweak` borrow so `self.ui`/mark_dirty stay free.
                    ui.strong("Debug Render");
                    let dbg = self.ui.debug_render_mode;
                    ui.horizontal(|ui| {
                        if ui.selectable_label(dbg == 0, "Off").clicked() { self.ui.debug_render_mode = 0; self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.selectable_label(dbg == 1, "March").on_hover_text("March-only: white=hit, black=miss (no shading)").clicked() { self.ui.debug_render_mode = 1; self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.selectable_label(dbg == 2, "Steps").on_hover_text("Step-count heatmap: blue=few, red=many").clicked() { self.ui.debug_render_mode = 2; self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.selectable_label(dbg == 3, "Bricks").on_hover_text("Brick validation: green=match, red=error, magenta=missing").clicked() { self.ui.debug_render_mode = 3; self.mark_dirty(DirtyFlags::RENDER); }
                    });
                    ui.separator();
                    // ── Phase-D Virtual Geometry kill switches ──────────────────
                    // Live bisection for render artifacts: flip one technique off
                    // and watch. Each toggle marks RENDER so the change is visible
                    // immediately. Order = pipeline order (cull → occlusion → LOD).
                    ui.strong("Virtual Geometry (Phase D)");
                    let mut vg_dirty = false;
                    vg_dirty |= ui
                        .checkbox(&mut self.render.dbg_gpu_cull, "D1 GPU instance cull")
                        .on_hover_text("Off → CPU frustum + occlusion path (the pre-D1 renderer).")
                        .changed();
                    vg_dirty |= ui
                        .checkbox(&mut self.render.dbg_hzb, "D2 HZB occlusion")
                        .on_hover_text(
                            "Off → frustum-only culling. If an artifact disappears here, \
                             the depth-pyramid occlusion test is over-culling.",
                        )
                        .changed();
                    ui.add_enabled_ui(self.render.dbg_hzb, |ui| {
                        vg_dirty |= ui
                            .checkbox(&mut self.render.dbg_hzb_late, "└ two-pass late recovery")
                            .on_hover_text(
                                "Off → early-only occlusion: disoccluded geometry pops in a \
                                 frame late (expected). On → recovered same frame.",
                            )
                            .changed();
                    });
                    // D4 is PARKED (owner 2026-08-05 visual: cut still islands on 1x9).
                    // Default OFF — opt-in only for resume work, not shipping path.
                    vg_dirty |= ui
                        .checkbox(
                            &mut self.render.dbg_clusters,
                            "D4 cluster geometry (PARKED)",
                        )
                        .on_hover_text(
                            "PARKED — coverage still islands on dense 1x9. Leave OFF for \
                             real work (source mesh). Opt-in only when resuming D4 cut work.",
                        )
                        .changed();
                    ui.add_enabled_ui(self.render.dbg_clusters, |ui| {
                        vg_dirty |= ui
                            .checkbox(
                                &mut self.render.dbg_cluster_passthrough,
                                "└ sabotage: draw every cluster",
                            )
                            .on_hover_text(
                                "Selects ALL levels at once (massive overdraw). Proves the \
                                 LOD cut is doing the work — never leave on.",
                            )
                            .changed();
                        vg_dirty |= ui
                            .checkbox(
                                &mut self.render.dbg_cluster_force_l0,
                                "└ diagnostic: pin to level 0",
                            )
                            .on_hover_text(
                                "Draws ONLY the finest clusters — a fixed set with no camera \
                                 dependence. If an artifact survives this, the LOD selection \
                                 is not the cause; the draw path is.",
                            )
                            .changed();
                        vg_dirty |= ui
                            .checkbox(&mut self.render.dbg_cluster_view, "└ cluster debug view")
                            .on_hover_text(
                                "LOD palette (needs Clusters ON + dense ≥50k poly). PARKED \
                                 with D4 — visual still shows fragment islands on 1x9.",
                            )
                            .changed();
                    });
                    ui.label(
                        egui::RichText::new(format!(
                            "occl {}/{}  late {}  draws {}",
                            self.render.last_occlusion_culled,
                            self.render.last_occlusion_tested,
                            self.render.hzb_late_recovered,
                            self.render.last_instance_draws,
                        ))
                        .small()
                        .weak(),
                    );
                    if vg_dirty {
                        // Draw-kind routing (clustered vs source) is decided during the
                        // structural gather — force one so a toggle takes effect now.
                        self.structural_draw_stale = true;
                        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
                    }
                    ui.separator();
                    let tw = &mut self.tweak;
                    let def = ToolSettings::default();

                    // Helper macro: section header with inline reset button
                    macro_rules! section {
                        ($ui:expr, $label:expr, $body:expr) => {
                            $ui.horizontal(|ui| {
                                ui.strong($label);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Reset").clicked() { $body }
                                });
                            });
                        };
                    }

                    let mut save_tweaks = false;
                    // Safety dot colors: green=safe, yellow=may cause artifacts, red=stability critical
                    let dot_green = egui::Color32::from_rgb(80, 200, 80);
                    let dot_yellow = egui::Color32::from_rgb(220, 200, 50);
                    let dot_red = egui::Color32::from_rgb(220, 60, 60);

                    // Helper macro: safety-colored slider + "x" reset + "S" save-as-default
                    macro_rules! tweak_slider {
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr, log) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label).logarithmic(true));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                        // Yellow safety level
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr, yellow) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("May cause visual artifacts if set too extreme");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr, yellow, log) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("May cause visual artifacts if set too extreme");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label).logarithmic(true));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                        // Red safety level — shows confirmation prompt
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr, red) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_red, "\u{25CF}").on_hover_text("Stability critical — may cause crashes or severe perf issues");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                        ($ui:expr, $val:expr, $def:expr, $range:expr, $label:expr, red, log) => {
                            $ui.horizontal(|ui| {
                                ui.colored_label(dot_red, "\u{25CF}").on_hover_text("Stability critical — may cause crashes or severe perf issues");
                                let changed = ($val - $def).abs() > f32::EPSILON;
                                ui.add(egui::Slider::new(&mut $val, $range).text($label).logarithmic(true));
                                if changed {
                                    if ui.small_button("x").on_hover_text("Reset to code default").clicked() { $val = $def; }
                                    if ui.small_button("S").on_hover_text("Save current value as your default").clicked() { save_tweaks = true; }
                                }
                            });
                        };
                    }

                    // Legend + Search
                    ui.horizontal(|ui| {
                        ui.colored_label(dot_green, "\u{25A1} Safe");
                        ui.colored_label(dot_yellow, "\u{25A1} Artifacts");
                        ui.colored_label(dot_red, "\u{25A1} Critical");
                    });
                    ui.horizontal(|ui| {
                        ui.label("\u{1F50D}");
                        ui.text_edit_singleline(&mut self.ui.tweak_search);
                    });
                    let search = self.ui.tweak_search.to_lowercase();
                    // Helper: does this section match the search filter?
                    let sec_match = |keywords: &[&str]| -> bool {
                        search.is_empty() || keywords.iter().any(|k| k.to_lowercase().contains(&search))
                    };
                    ui.separator();

                    // -- Transform (G/R/S) --
                    if sec_match(&["Transform", "Grab speed", "Rotate", "Scale", "Duplicate"]) {
                    section!(ui, "Transform", {
                        tw.grab_speed = def.grab_speed;
                        tw.rotate_speed = def.rotate_speed;
                        tw.scale_speed = def.scale_speed;
                        tw.duplicate_offset = def.duplicate_offset;
                    });
                    ui.indent("tw_transform", |ui| {
                        tweak_slider!(ui, tw.grab_speed, def.grab_speed, 0.0001..=0.05, "Grab speed", log);
                        tweak_slider!(ui, tw.rotate_speed, def.rotate_speed, 0.01..=5.0, "Rotate deg/px");
                        tweak_slider!(ui, tw.scale_speed, def.scale_speed, 0.0001..=0.1, "Scale /px", log);
                        tweak_slider!(ui, tw.duplicate_offset, def.duplicate_offset, 0.0..=5.0, "Duplicate offset");
                    });
                    }

                    ui.separator();

                    // -- Camera --
                    if sec_match(&["Camera", "Orbit", "Pan", "Zoom", "Pitch", "Distance"]) {
                    section!(ui, "Camera", {
                        tw.cam_orbit_speed = def.cam_orbit_speed;
                        tw.cam_pan_speed = def.cam_pan_speed;
                        tw.cam_zoom_speed = def.cam_zoom_speed;
                        tw.cam_zoom_min = def.cam_zoom_min;
                        tw.cam_zoom_max = def.cam_zoom_max;
                        tw.default_cam_dist = def.default_cam_dist;
                        tw.cam_pitch_clamp = def.cam_pitch_clamp;
                    });
                    ui.indent("tw_camera", |ui| {
                        tweak_slider!(ui, tw.cam_orbit_speed, def.cam_orbit_speed, 0.0005..=0.05, "Orbit rad/px", log);
                        tweak_slider!(ui, tw.cam_pan_speed, def.cam_pan_speed, 0.0001..=0.02, "Pan speed", log);
                        tweak_slider!(ui, tw.cam_zoom_speed, def.cam_zoom_speed, 0.0001..=0.02, "Zoom speed", log);
                        tweak_slider!(ui, tw.cam_zoom_min, def.cam_zoom_min, 0.01..=10.0, "Zoom min");
                        tweak_slider!(ui, tw.cam_zoom_max, def.cam_zoom_max, 10.0..=2000.0, "Zoom max");
                        tweak_slider!(ui, tw.default_cam_dist, def.default_cam_dist, 0.5..=50.0, "Default distance");
                        tweak_slider!(ui, tw.cam_pitch_clamp, def.cam_pitch_clamp, 0.5..=1.57, "Pitch clamp (rad)");
                    });
                    }

                    ui.separator();

                    // -- Face Push/Pull --
                    section!(ui, "Push/Pull", {
                        tw.face_drag_speed = def.face_drag_speed;
                    });
                    ui.indent("tw_pushpull", |ui| {
                        tweak_slider!(ui, tw.face_drag_speed, def.face_drag_speed, 0.0001..=0.05, "Drag speed", log);
                    });

                    ui.separator();

                    // -- Bevel --
                    section!(ui, "Bevel", {
                        tw.bevel_scroll_speed = def.bevel_scroll_speed;
                        tw.bevel_drag_speed = def.bevel_drag_speed;
                        tw.bevel_hover_px = def.bevel_hover_px;
                        tw.blend_k_speed = def.blend_k_speed;
                    });
                    ui.indent("tw_bevel", |ui| {
                        tweak_slider!(ui, tw.bevel_scroll_speed, def.bevel_scroll_speed, 0.001..=0.2, "Scroll speed");
                        tweak_slider!(ui, tw.bevel_drag_speed, def.bevel_drag_speed, 0.0005..=0.05, "Drag speed", log);
                        tweak_slider!(ui, tw.bevel_hover_px, def.bevel_hover_px, 1.0..=32.0, "Hover radius px");
                        tweak_slider!(ui, tw.blend_k_speed, def.blend_k_speed, 0.0001..=0.02, "Blend-k /scroll", log);
                    });

                    ui.separator();

                    // -- Lighting --
                    section!(ui, "Lighting", {
                        tw.light_azimuth_speed = def.light_azimuth_speed;
                        tw.light_elevation_speed = def.light_elevation_speed;
                    });
                    ui.indent("tw_light", |ui| {
                        tweak_slider!(ui, tw.light_azimuth_speed, def.light_azimuth_speed, 0.01..=5.0, "Azimuth deg/px");
                        tweak_slider!(ui, tw.light_elevation_speed, def.light_elevation_speed, 0.01..=5.0, "Elevation deg/px");
                    });

                    ui.separator();

                    // -- Raycast --
                    section!(ui, "Raycast (CPU)", {
                        tw.ray_max_steps = def.ray_max_steps;
                        tw.ray_hit_threshold = def.ray_hit_threshold;
                        tw.ray_normal_eps = def.ray_normal_eps;
                        tw.ray_max_dist = def.ray_max_dist;
                        tw.sdf_placement_bias = def.sdf_placement_bias;
                    });
                    ui.indent("tw_raycast", |ui| {
                        ui.horizontal(|ui| {
                            let mut steps = tw.ray_max_steps as i32;
                            let changed = tw.ray_max_steps != def.ray_max_steps;
                            ui.add(egui::Slider::new(&mut steps, 32..=512).text("Max steps"));
                            tw.ray_max_steps = steps as u32;
                            if changed {
                                if ui.small_button("x").on_hover_text("Reset to code default").clicked() {
                                    tw.ray_max_steps = def.ray_max_steps;
                                }
                                if ui.small_button("S").on_hover_text("Save current value as your default").clicked() {
                                    save_tweaks = true;
                                }
                            }
                        });
                        tweak_slider!(ui, tw.ray_hit_threshold, def.ray_hit_threshold, 0.0001..=0.1, "Hit threshold", log);
                        tweak_slider!(ui, tw.ray_normal_eps, def.ray_normal_eps, 0.0001..=0.05, "Normal epsilon", log);
                        tweak_slider!(ui, tw.ray_max_dist, def.ray_max_dist, 5.0..=500.0, "Max distance");
                        tweak_slider!(ui, tw.sdf_placement_bias, def.sdf_placement_bias, 0.0..=0.02, "Placement bias", log);
                    });

                    ui.separator();

                    // -- Selection / Interaction --
                    section!(ui, "Selection", {
                        tw.selection_circle_px = def.selection_circle_px;
                        tw.depth_pick_drift_px = def.depth_pick_drift_px;
                        tw.pie_menu_radius = def.pie_menu_radius;
                    });
                    ui.indent("tw_selection", |ui| {
                        tweak_slider!(ui, tw.selection_circle_px, def.selection_circle_px, 4.0..=40.0, "Circle radius px");
                        tweak_slider!(ui, tw.depth_pick_drift_px, def.depth_pick_drift_px, 5.0..=60.0, "Depth pick drift px");
                        tweak_slider!(ui, tw.pie_menu_radius, def.pie_menu_radius, 5.0..=50.0, "Pie menu radius px");
                    });

                    ui.separator();

                    // -- Smart Mirror --
                    section!(ui, "Smart Mirror", {
                        tw.mirror_rotate_speed = def.mirror_rotate_speed;
                        tw.mirror_snap_angle = def.mirror_snap_angle;
                    });
                    ui.indent("tw_mirror", |ui| {
                        tweak_slider!(ui, tw.mirror_rotate_speed, def.mirror_rotate_speed, 0.001..=0.05, "Rotate speed", log);
                        tweak_slider!(ui, tw.mirror_snap_angle, def.mirror_snap_angle, 5.0..=90.0, "Snap angle (deg)");
                    });

                    ui.separator();

                    // -- Misc --
                    section!(ui, "Misc", {
                        tw.through_cut_depth = def.through_cut_depth;
                        tw.falloff_hover_px = def.falloff_hover_px;
                        tw.duplicate_offset = def.duplicate_offset;
                        tw.instance_offset = def.instance_offset;
                        tw.toast_duration = def.toast_duration;
                    });
                    ui.indent("tw_misc", |ui| {
                        tweak_slider!(ui, tw.through_cut_depth, def.through_cut_depth, 1.0..=100.0, "Through-cut min depth");
                        tweak_slider!(ui, tw.falloff_hover_px, def.falloff_hover_px, 1.0..=32.0, "Falloff hover px");
                        tweak_slider!(ui, tw.instance_offset, def.instance_offset, 0.0..=5.0, "Instance offset");
                        tweak_slider!(ui, tw.toast_duration, def.toast_duration, 0.5..=10.0, "Toast duration (s)");
                    });

                    ui.separator();

                    // -- Bevel Limits (yellow) --
                    section!(ui, "Bevel Limits", {
                        tw.bevel_max_radius = def.bevel_max_radius;
                        tw.blend_k_max = def.blend_k_max;
                        tw.bevel_scroll_mult = def.bevel_scroll_mult;
                    });
                    ui.indent("tw_bevel_limits", |ui| {
                        tweak_slider!(ui, tw.bevel_max_radius, def.bevel_max_radius, 0.1..=10.0, "Max bevel radius", yellow);
                        tweak_slider!(ui, tw.blend_k_max, def.blend_k_max, 0.5..=64.0, "Max blend-k", yellow);
                        tweak_slider!(ui, tw.bevel_scroll_mult, def.bevel_scroll_mult, 1.0..=50.0, "Scroll multiplier", yellow);
                    });

                    ui.separator();

                    // -- Create Tool --
                    section!(ui, "Create Tool", {
                        tw.create_sink_depth = def.create_sink_depth;
                        tw.create_alt_rot_speed = def.create_alt_rot_speed;
                        tw.create_min_extent = def.create_min_extent;
                    });
                    ui.indent("tw_create", |ui| {
                        tweak_slider!(ui, tw.create_sink_depth, def.create_sink_depth, 0.0..=0.1, "Sink depth (cuts)", yellow, log);
                        tweak_slider!(ui, tw.create_alt_rot_speed, def.create_alt_rot_speed, 0.05..=2.0, "Alt rotate deg/px");
                        tweak_slider!(ui, tw.create_min_extent, def.create_min_extent, 0.0001..=0.01, "Min extent", yellow, log);
                    });

                    ui.separator();

                    // -- Raymarcher (red: stability critical) --
                    section!(ui, "Raymarcher", {
                        tw.ray_step_min = def.ray_step_min;
                        tw.ray_dist_mult = def.ray_dist_mult;
                        self.render.march_steps = 48;
                        self.render.march_resolution = 1.0;
                        self.render.march_hit_threshold = 0.001;
                    });
                    // March quality controls — locals avoid borrow conflict with `tw`;
                    // dirty flag uses outer Cell so mark_dirty runs after closures drop `tw`.
                    ui.indent("tw_raymarch", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let mut steps_i = self.render.march_steps as i32;
                            ui.add(egui::Slider::new(&mut steps_i, 8..=128).step_by(4.0).text("March Steps"));
                            self.render.march_steps = steps_i as u32;
                            if self.render.march_steps != 48 {
                                if ui.small_button("x").on_hover_text("Reset to code default").clicked() {
                                    self.render.march_steps = 48;
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let r = ui.add(egui::Slider::new(&mut self.render.march_resolution, 0.25..=1.0).text("March Resolution"));
                            if r.changed() { march_render_dirty.set(true); }
                            if (self.render.march_resolution - 1.0).abs() > f32::EPSILON {
                                if ui.small_button("x").on_hover_text("Reset to code default").clicked() {
                                    self.render.march_resolution = 1.0; march_render_dirty.set(true);
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("May cause visual artifacts if set too extreme");
                            ui.add(egui::Slider::new(&mut self.render.march_hit_threshold, 0.0001..=0.01).text("Hit Threshold").logarithmic(true));
                            if (self.render.march_hit_threshold - 0.001).abs() > f32::EPSILON {
                                if ui.small_button("x").on_hover_text("Reset to code default").clicked() {
                                    self.render.march_hit_threshold = 0.001;
                                }
                            }
                        });
                        ui.separator();
                        tweak_slider!(ui, tw.ray_step_min, def.ray_step_min, 0.001..=0.1, "Step minimum", red, log);
                        tweak_slider!(ui, tw.ray_dist_mult, def.ray_dist_mult, 0.5..=2.0, "Distance multiplier", yellow);
                        // (BVH acceleration checkbox REMOVED 2026-07-14 — analytical display path stripped)
                        ui.separator();
                        // March Quality (moved from main render panel)
                        if ui.add(egui::Slider::new(&mut self.render.march_relaxation, 1.0..=1.8).text("March Relaxation").step_by(0.01))
                            .on_hover_text("Over-relaxation factor. 1.0 = standard, higher = faster but may overshoot near smooth blends.")
                            .changed() { march_render_dirty.set(true); }
                        {
                            let mut bs = self.render.march_bisection_steps as f32;
                            if ui.add(egui::Slider::new(&mut bs, 0.0..=6.0).text("Bisection Steps").step_by(1.0))
                                .on_hover_text("Hit refinement steps. 0 = fastest, 3 = default, 6 = highest precision.")
                                .changed() { self.render.march_bisection_steps = bs as u32; march_render_dirty.set(true); }
                        }
                        ui.separator();
                        ui.checkbox(&mut self.render.march_adaptive_enabled, "Adaptive steps")
.default_menu("render.march_adaptive_enabled", serde_json::json!(self.render.march_adaptive_enabled))
                            .on_hover_text("Scale step budget + hit threshold with distance.\nNear pixels get full steps, far pixels use fewer.");
                        if self.render.march_adaptive_enabled {
                            ui.add(egui::Slider::new(&mut self.render.march_lod_scale, 0.1..=4.0).text("LOD scale"))
                                .on_hover_text("Multiplier on pixel-size threshold.\n<1.0 = aggressive (fewer steps), >1.0 = conservative.");
                        }
                    });

                    ui.separator();

                    // -- Distance LOD --
                    if sec_match(&["LOD", "Distance", "Threshold", "Screen"]) {
                    section!(ui, "Distance LOD", {
                        self.render.lod_dist_enabled = false;
                        self.render.lod_dist_threshold_0 = 0.3;
                        self.render.lod_dist_threshold_1 = 0.1;
                        self.render.lod_dist_threshold_2 = 0.03;
                        self.render.lod_min_steps = 16;
                    });
                    ui.indent("tw_dist_lod", |ui| {
                        ui.checkbox(&mut self.render.lod_dist_enabled, "Enable distance LOD")
.default_menu("render.lod_dist_enabled", serde_json::json!(self.render.lod_dist_enabled));
                        if self.render.lod_dist_enabled {
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                ui.add(egui::Slider::new(&mut self.render.lod_dist_threshold_0, 0.05..=1.0).text("Full quality above"));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                ui.add(egui::Slider::new(&mut self.render.lod_dist_threshold_1, 0.01..=0.5).text("Medium above"));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                ui.add(egui::Slider::new(&mut self.render.lod_dist_threshold_2, 0.001..=0.2).text("Low above").logarithmic(true));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("Too low may miss thin features");
                                let mut ms = self.render.lod_min_steps as i32;
                                ui.add(egui::Slider::new(&mut ms, 4..=64).text("Min march steps"));
                                self.render.lod_min_steps = ms as u32;
                            });
                        }
                    });
                    }

                    ui.separator();

                    // -- Foveated Rendering --
                    if sec_match(&["Foveated", "Edge", "Pixel", "Screen"]) {
                    section!(ui, "Foveated Rendering", {
                        self.render.foveated_enabled = false;
                        self.render.foveated_inner = 0.3;
                        self.render.foveated_outer = 0.9;
                        self.render.foveated_min_steps = 32;
                        self.render.foveated_threshold_mult = 10.0;
                        self.render.foveated_max_pixel_scale = 4.0;
                    });
                    ui.indent("tw_foveated", |ui| {
                        ui.checkbox(&mut self.render.foveated_enabled, "Enable foveated rendering")
.default_menu("render.foveated_enabled", serde_json::json!(self.render.foveated_enabled));
                        if self.render.foveated_enabled {
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                ui.add(egui::Slider::new(&mut self.render.foveated_inner, 0.0..=1.0).text("Inner radius"));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_green, "\u{25CF}");
                                ui.add(egui::Slider::new(&mut self.render.foveated_outer, 0.0..=1.0).text("Outer radius"));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("Steps at screen edge");
                                let mut ms = self.render.foveated_min_steps as i32;
                                ui.add(egui::Slider::new(&mut ms, 4..=64).text("Edge min steps"));
                                self.render.foveated_min_steps = ms as u32;
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("Hit threshold multiplier at screen edge");
                                ui.add(egui::Slider::new(&mut self.render.foveated_threshold_mult, 1.0..=50.0).text("Edge threshold mult").logarithmic(true));
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text("Max pixel block size at screen edge");
                                ui.add(egui::Slider::new(&mut self.render.foveated_max_pixel_scale, 1.0..=8.0).text("Edge max block size"));
                            });
                        }
                    });
                    }

                    ui.separator();

                    // -- LOD Debug --
                    if sec_match(&["LOD", "Debug", "Colors", "Tint"]) {
                    ui.strong("LOD Debug");
                    ui.indent("tw_lod_debug", |ui| {
                        ui.checkbox(&mut self.render.lod_debug_colors, "Debug color overlay")
                            .on_hover_text("Tint viewport by LOD tier:\nGreen=full, Yellow=medium, Orange=low, Red=minimal\nBlue=foveated edge, Black grid=pixel blocks");
                    });
                    }

                    ui.separator();

                    // -- Visual Feedback --
                    section!(ui, "Visual Feedback", {
                        tw.snap_indicator_px = def.snap_indicator_px;
                        tw.arrow_tip_px = def.arrow_tip_px;
                        tw.face_outline_width = def.face_outline_width;
                        tw.hud_font_size = def.hud_font_size;
                    });
                    ui.indent("tw_visual", |ui| {
                        tweak_slider!(ui, tw.snap_indicator_px, def.snap_indicator_px, 2.0..=20.0, "Snap indicator px");
                        tweak_slider!(ui, tw.arrow_tip_px, def.arrow_tip_px, 8.0..=50.0, "Arrow tip px");
                        tweak_slider!(ui, tw.face_outline_width, def.face_outline_width, 0.5..=5.0, "Face outline width");
                        tweak_slider!(ui, tw.hud_font_size, def.hud_font_size, 8.0..=20.0, "HUD font size");
                    });

                    ui.separator();

                    // -- System --
                    section!(ui, "System", {
                        tw.repaint_ms = def.repaint_ms;
                        tw.server_timeout_ms = def.server_timeout_ms;
                        tw.floor_detect_threshold = def.floor_detect_threshold;
                        tw.scale_min_clamp = def.scale_min_clamp;
                    });
                    ui.indent("tw_system", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_yellow, "\u{25CF}").on_hover_text(
                                "Idle frames are event-driven (no continuous spin). Set 0 = force uncapped idle repaint (stress/scoreboard). Non-zero is ignored at true idle."
                            );
                            let mut ms_i32 = tw.repaint_ms as i32;
                            let changed = tw.repaint_ms != def.repaint_ms;
                            ui.add(egui::Slider::new(&mut ms_i32, 0..=100).text("Repaint interval (ms)"));
                            tw.repaint_ms = ms_i32 as u32;
                            if changed {
                                if ui.small_button("x").clicked() { tw.repaint_ms = def.repaint_ms; }
                                if ui.small_button("S").clicked() { save_tweaks = true; }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let mut ms_i32 = tw.server_timeout_ms as i32;
                            let changed = tw.server_timeout_ms != def.server_timeout_ms;
                            ui.add(egui::Slider::new(&mut ms_i32, 500..=10000).text("Server timeout (ms)"));
                            tw.server_timeout_ms = ms_i32 as u32;
                            if changed {
                                if ui.small_button("x").clicked() { tw.server_timeout_ms = def.server_timeout_ms; }
                                if ui.small_button("S").clicked() { save_tweaks = true; }
                            }
                        });
                        tweak_slider!(ui, tw.floor_detect_threshold, def.floor_detect_threshold, 0.3..=0.95, "Floor detect Y", yellow);
                        tweak_slider!(ui, tw.scale_min_clamp, def.scale_min_clamp, 0.0001..=0.01, "Scale min clamp", red, log);
                    });

                    ui.separator();

                    // -- UI Throttle --
                    section!(ui, "UI Throttle", {
                        tw.throttle_panels_during_interaction = def.throttle_panels_during_interaction;
                        tw.panel_refresh_interval_ms = def.panel_refresh_interval_ms;
                    });
                    ui.indent("tw_ui_throttle", |ui| {
                        ui.checkbox(&mut tw.throttle_panels_during_interaction, "Throttle panels during interaction")
                            .on_hover_text("Reduce outliner data rebuilds during orbit/drag.\nWidgets still draw every frame; only data preparation is throttled.");
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let mut ms_i32 = tw.panel_refresh_interval_ms as i32;
                            let changed = tw.panel_refresh_interval_ms != def.panel_refresh_interval_ms;
                            ui.add(egui::Slider::new(&mut ms_i32, 50..=1000).text("Refresh interval (ms)"));
                            tw.panel_refresh_interval_ms = ms_i32 as u32;
                            if changed {
                                if ui.small_button("x").clicked() { tw.panel_refresh_interval_ms = def.panel_refresh_interval_ms; }
                                if ui.small_button("S").clicked() { save_tweaks = true; }
                            }
                        });
                        let is_throttling = self.render.interaction_active_frames >= 2 && tw.throttle_panels_during_interaction;
                        ui.label(if is_throttling { "Status: THROTTLING" } else { "Status: normal" });
                    });

                    ui.separator();

                    // -- Experimental (Phase G) --
                    ui.strong("Experimental");
                    ui.indent("tw_experimental", |ui| {
                        // GATE 3: these boxes are the route-override INPUTS to the ONE decision
                        // site (CacheState::dc_mesh_route). The derived route is shown below.
                        ui.checkbox(&mut self.cache.gpu_sdf_mesh, "GPU SDF Mesh (QEF Dual Contouring)")
                            .on_hover_text("Master gate. OFF → CPU chunked-DC oracle route (the parity/golden oracle, ~125ms).\nON → GPU meshing (resident or readback per the box below). Object Mode only.");
                        ui.add_enabled_ui(self.cache.gpu_sdf_mesh, |ui| {
                        ui.checkbox(&mut self.cache.gpu_sdf_mesh_resident, "  └ Resident GPU mesh (G2, indirect draw) [default ON]")
                            .on_hover_text("DEFAULT ON: draw the SDF body straight from its GPU-resident QEF-DC slot via draw_indexed_indirect — replaces the per-frame mesh_cache buffer alloc/upload (the 2.7-6ms dc_dispatch). Readback stays as a fallback. Toggle OFF to revert to the mesh_cache readback path if the surface vanishes or selection breaks.");
                        });
                        ui.label(format!("Derived DC route: {:?} (≥512 brick-resident bodies: {:?})",
                            self.cache.dc_mesh_route(false), self.cache.dc_mesh_route(true)))
                            .on_hover_text("GATE 3: the ONE routing decision (cache_state::dc_mesh_route) derived from the boxes above. The remaining boxes below are orthogonal policies (field source / readback / bake write), not routes.");
                        ui.add_enabled_ui(self.cache.gpu_sdf_mesh_resident, |ui| {
                            ui.checkbox(&mut self.cache.gpu_sdf_mesh_resident_field, "      └ Field from resident buffer (G3) [default ON]")
                                .on_hover_text("DEFAULT ON: feed the QEF-DC density input straight from the GPU-resident bake buffer (no slot.field upload). Bit-parity proven offline; guarded by content-identity + falls back to the CPU-grid upload on any mismatch. Requires Resident GPU mesh ON.");
                        });
                        ui.add_enabled_ui(self.cache.gpu_sdf_mesh_resident && self.cache.gpu_sdf_mesh_resident_field, |ui| {
                            ui.checkbox(&mut self.cache.gpu_sdf_mesh_lazy_readback, "          └ Lazy readback (step 2) [256³ speed; has drag-ghost bug]")
                                .on_hover_text("Step 2: skip the 64MB CPU readback on partial drag bakes — big speedup at 256³. KNOWN ISSUE: a second/ghost surface appears during drag. Default OFF until fixed; toggle ON to trade the ghost for 256³ drag speed.");
                        });
                        ui.add_enabled_ui(self.cache.gpu_sdf_mesh_resident && self.cache.gpu_sdf_mesh_resident_field, |ui| {
                            ui.checkbox(&mut self.cache.gpu_full_bake_path_a, "          └ Path-A full bake (WIP) [in-place full bake; kills drag-ghost + hitch]")
                                .on_hover_text("Path-A for FULL bakes: an escape/settle FULL bake writes DIRECTLY into rv.buffer (GPU2GPU) and skips the slow CPU readback — like the partial in-place path that already keeps small moves ghost-free. Fixes the large/fast-move drag-ghost AND the high-res full-bake hitch. WIP: default OFF until bit-parity-verified; toggle ON to test.");
                        });
                        ui.add_enabled_ui(self.cache.gpu_sdf_mesh_resident, |ui| {
                            ui.checkbox(&mut self.cache.sdf_lod_auto, "      └ Auto SDF mesh LOD (O2b, octree) [default OFF]")
                                .on_hover_text("EXPERIMENTAL A/B: distant SDF bodies re-mesh at octree level 1/2 straight from their resident brick atlas (no re-bake, no re-classify) when one voxel projects below ~0.5/0.25 px. ±30% hysteresis + 1s/body rate limit (no popping); the ACTIVE body always stays full detail. Toggle OFF restores every body to level 0. Watch [SdfLod] in the log (PERF_SPAM).");
                            ui.checkbox(&mut self.cache.gpu_sdf_mesh_sparse, "      └ Sparse brick mesh (Phase 3B) [WIP — memory ∝ surface]")
                                .on_hover_text("EXPERIMENTAL: mesh the DC surface from a SPARSE 8³ brick atlas (classify_grid_to_sparse) instead of the dense res³ field — memory scales with SURFACE AREA, not volume (a few MB vs 512MB at 512³). Dispatch/decode proven bit-exact offline. First step toward the sparse SDF atlas that retires the dense VolumeAtlas (fixes multi-body thrash + corruption + unlocks 1024³). Watch [SparseMesh] in the log (PERF_SPAM) for the brick count. Toggle OFF if the surface looks wrong.");
                        });
                    });
                    ui.separator();

                    // -- Path Tracer --
                    ui.strong("Path Tracer");
                    ui.indent("tw_pathtracer", |ui| {
                        if ui.checkbox(&mut self.render.pt_temporal_reproject, "PT temporal reprojection (P3.5) [default ON]")
.default_menu("render.pt_temporal_reproject", serde_json::json!(self.render.pt_temporal_reproject))
                            .on_hover_text("Reproject the accumulated path-trace image under CAMERA motion instead of resetting to black — orbiting keeps a stable (slightly blurred) image that refines in place. Disocclusion edges restart locally (per-pixel history). Content changes (scene/material edits) always fully reset. Toggle OFF to restore the old reset-on-orbit behavior for A/B.")
                            .changed()
                        {
                            // Restart accumulation so the A/B comparison starts clean either way.
                            // (Cell-deferred dirty — `tw` is mutably borrowed across these closures.)
                            self.render.pt_accumulated_frames = 0;
                            march_render_dirty.set(true);
                        }
                        if ui.checkbox(&mut self.render.pt_live_hybrid_edit, "PT live Hybrid while SDF edit (A) [default ON]")
.default_menu("render.pt_live_hybrid_edit", serde_json::json!(self.render.pt_live_hybrid_edit))
                            .on_hover_text("During SDF reshape under Path Trace, show Hybrid/raster for live bake feedback; resume PT on settle.")
                            .changed()
                        {
                            self.render.pt_was_live_hybrid = false;
                            self.render.pt_accumulated_frames = 0;
                            march_render_dirty.set(true);
                        }
                        if ui.checkbox(&mut self.render.pt_live_draft_mesh, "PT draft mesh while shaping (B) [default ON]")
.default_menu("render.pt_live_draft_mesh", serde_json::json!(self.render.pt_live_draft_mesh))
                            .on_hover_text("Shorter settle for active SDF body so triangle PT can update mid-edit. OFF = full 300 ms settle.")
                            .changed()
                        {
                            march_render_dirty.set(true);
                        }
                        if self.render.pt_live_draft_mesh {
                            let mut ms = self.render.pt_live_draft_settle_ms as f32;
                            if ui.add(egui::Slider::new(&mut ms, 16.0..=300.0).text("Draft settle ms")).changed() {
                                self.render.pt_live_draft_settle_ms = ms as u32;
                                march_render_dirty.set(true);
                            }
                        }
                        if ui.checkbox(&mut self.render.pt_soft_hybrid_resume, "PT soft Hybrid→PT resume (C) [default ON]")
.default_menu("render.pt_soft_hybrid_resume", serde_json::json!(self.render.pt_soft_hybrid_resume))
                            .on_hover_text("Seed PT accum from last Hybrid HDR on exit; short progressive weight instead of black hard reset.")
                            .changed()
                        {
                            self.render.pt_pending_soft_hybrid_seed = false;
                            march_render_dirty.set(true);
                        }
                        if self.render.pt_soft_hybrid_resume {
                            let mut f = self.render.pt_soft_hybrid_frames as f32;
                            if ui.add(egui::Slider::new(&mut f, 1.0..=16.0).text("Hybrid seed frames")).changed() {
                                self.render.pt_soft_hybrid_frames = f as u32;
                                march_render_dirty.set(true);
                            }
                        }
                        if ui.checkbox(&mut self.render.pt_draft_soft_reset, "PT draft soft reset (D) [default ON]")
.default_menu("render.pt_draft_soft_reset", serde_json::json!(self.render.pt_draft_soft_reset))
                            .on_hover_text("Mesh/TLAS updates keep short progressive history instead of frames=0.")
                            .changed()
                        {
                            march_render_dirty.set(true);
                        }
                        if self.render.pt_draft_soft_reset {
                            let mut f = self.render.pt_draft_soft_frames as f32;
                            if ui.add(egui::Slider::new(&mut f, 1.0..=8.0).text("Draft soft frames")).changed() {
                                self.render.pt_draft_soft_frames = f as u32;
                                march_render_dirty.set(true);
                            }
                        }
                        if ui.checkbox(&mut self.render.pt_denoise_end_on_hdr, "PT denoise end-on-HDR (E) [default ON]")
.default_menu("render.pt_denoise_end_on_hdr", serde_json::json!(self.render.pt_denoise_end_on_hdr))
                            .on_hover_text("Even à-trous steps invert pass0 so final filter lands in HDR — no temp→hdr blit.")
                            .changed()
                        {
                            march_render_dirty.set(true);
                        }
                        if ui.checkbox(&mut self.render.pt_fast_mode, "PT Fast mode (F) — half-res while orbiting only")
.default_menu("render.pt_fast_mode", serde_json::json!(self.render.pt_fast_mode))
                            .on_hover_text("Lean spp/bounces + tighter frame budget (half-res under pressure). OFF = quality refine.")
                            .changed()
                        {
                            self.render.pt_accumulated_frames = 0;
                            march_render_dirty.set(true);
                        }
                    });
                    ui.separator();

                    // -- Profiling --
                    ui.strong("Profiling");
                    ui.indent("tw_profiling", |ui| {
                        ui.checkbox(&mut self.ui.show_profiler_overlay, "GPU Profiler overlay");
                        ui.checkbox(&mut self.ui.show_cpu_frame_profiler, "CPU Frame Profiler");
                        ui.checkbox(&mut self.ui.show_perf_hud, "Perf HUD (CPU timings)");
                        ui.checkbox(&mut self.ui.show_bevel_debug, "Bevel Debug Overlay");
                        ui.checkbox(&mut self.cache.body_vol_cache.bake_prof, "Bake path profiler ([BAKEPROF] log)")
                            .on_hover_text("Print one [BAKEPROF] line per body per bake dispatch: path (partial / escape->full / full / first-cascade), resolution, dirty-region count, prim count. Diagnoses the live prim-move full-rebake cost (LIVE_MOVE_PERF.md) — drag a prim with this ON and watch the partial:escape ratio. Default OFF.");
                    });

                    ui.separator();

                    // -- Debug Rendering --
                    ui.strong("Debug Rendering");
                    ui.indent("tw_debug_render", |ui| {
                        // ("Volume-only rendering" checkbox removed 2026-07-13 — it wrote
                        // force_volume_only, which the per-frame driver overwrote before any
                        // dispatch could see it; the flag itself is deleted.)
                        // ("Force Analytical (Debug)" checkbox REMOVED 2026-07-14 — analytical march display path stripped)
                        ui.checkbox(&mut self.show_dirty_regions, "Show Dirty Regions")
                            .on_hover_text("Draw wireframe boxes for dirty bake regions.\nGreen = partial bake, Red = full grid.");
                        // ("Sparse bake (3A debug)" checkbox REMOVED 2026-07-14, S2.6 — the
                        // SparseBricks (mode-2) regime is now structural for Object-Mode FULL
                        // bakes at ≥512; it keys on gridbacked_only alone. The toggle was
                        // runtime-only and its OFF default was review finding R1.)
                        ui.checkbox(&mut self.test_blend_overlay, "Blend Overlay Test")
                            .on_hover_text("Smoke test: 20% green tint over entire viewport via BlendDispatch.\nProves the composite_blend pipeline works end-to-end.");
                        ui.checkbox(&mut self.log_render_mode, "Log Render Mode")
                            .on_hover_text("Print [RenderMode] to console every frame.\nUseful for verifying pipeline selection during interaction episodes.");
                        // ── SSGI stage isolation (SSGI_PLAN.md §4.10) ──────────
                        // A three-dispatch effect that composites into the HDR has
                        // no natural seam to probe: when the result looks wrong,
                        // "gather, energy term, blur, or upsample?" is otherwise
                        // unanswerable without a recompile. Each toggle answers one.
                        // Only toggles whose GATE HAS LANDED are shown — a checkbox
                        // that does nothing is worse than a missing one (R13).
                        // The procedural fill light: an invisible second directional
                        // light hardcoded at 0.2x key since forever. It MOVES THE
                        // TERMINATOR (the lit/unlit break lands where
                        // dot(N,sun) + s*dot(N,fill) = 0, not on the geometric crease),
                        // and it is multiplied by the SUN's shadow map. 0.2 = the
                        // historical look; 0 = a physical terminator.
                        if ui.add(egui::Slider::new(&mut self.render.fill_light_strength, 0.0..=1.0).text("Procedural fill light"))
                            .on_hover_text("Hidden second light, hardcoded at 0.2 until now.
It shifts where the lit/unlit boundary falls, so hard-surface edges do not read cleanly.
Set 0 for a terminator that sits exactly on dot(N,sun)=0.")
                            .changed() { march_render_dirty.set(true); }
                        ui.collapsing("SSGI", |ui| {
                            // Dev enable. The real user-facing toggle lands in the
                            // render settings at Gate 6; until then this is the only
                            // way to exercise the pass, and VG-A needs it.
                            if ui.checkbox(&mut self.render.ssgi_enabled, "Enable SSGI (dev)")
.default_menu("render.ssgi_enabled", serde_json::json!(self.render.ssgi_enabled))
                                .on_hover_text("Ships OFF. Until Gate 2 the gather writes ZERO, so enabling it must change nothing - that is exactly what VG-A checks.")
                                .changed() { march_render_dirty.set(true); }
                            // Live settings, surfaced here so VG-B can actually be
                            // judged. Their real home is the render-settings panel at
                            // Gate 6; HDRI Occlusion belongs with the environment
                            // controls, not with SSGI.
                            if ui.add(egui::Slider::new(&mut self.render.ssgi_intensity, 0.0..=3.0).text("Intensity"))
                                .on_hover_text("Overall strength of the indirect delta. 1.0 = the energy-conserving result.")
                                .changed() { march_render_dirty.set(true); }
                            if ui.add(egui::Slider::new(&mut self.render.ssgi_radius, 0.1..=6.0).text("Radius (world)"))
                                .on_hover_text("How far a gather ray reaches. Larger = light travels further, more cost.")
                                .changed() { march_render_dirty.set(true); }
                            if ui.add(egui::Slider::new(&mut self.render.ssgi_env_occlusion, 0.0..=1.0).text("HDRI Occlusion"))
                                .on_hover_text("How much occluded SKY light SSGI is allowed to REMOVE.
1.0 = energy-consistent: a wall blocks sky and replaces it with bounce (darker, more contrast).
0.0 = additive only: bounce is added, sky is never removed (softer, brighter). Needs an HDRI to do anything.")
                                .changed() { march_render_dirty.set(true); }
                            ui.separator();
                            if ui.checkbox(&mut self.ssgi_debug.master_kill, "Kill SSGI (master)")
                                .on_hover_text("Hard-disable the dispatch AND clear the composite bit, whatever the render setting says. The instant answer to 'is SSGI causing this?'.")
                                .changed() { march_render_dirty.set(true); }
                            if ui.checkbox(&mut self.ssgi_debug.force_const, "Force constant delta")
                                .on_hover_text("Gather writes the constant below instead of marching. Proves the composite path end-to-end with no GI math in play (Gate 1).")
                                .changed() { march_render_dirty.set(true); }
                            if self.ssgi_debug.force_const {
                                if ui.add(egui::Slider::new(&mut self.ssgi_debug.const_value, 0.0..=1.0).text("Constant"))
                                    .changed() { march_render_dirty.set(true); }
                            }
                            if ui.checkbox(&mut self.ssgi_debug.skip_blur, "Skip blur passes")
                                .on_hover_text("Dispatch the gather only - shows the raw per-pixel noise the denoiser hides.")
                                .changed() { march_render_dirty.set(true); }
                            if ui.add(egui::Slider::new(&mut self.ssgi_debug.rays_override, 0..=32).text("Ray override (0 = use setting)"))
                                .on_hover_text("Perf/quality sweeps without touching the render settings.")
                                .changed() { march_render_dirty.set(true); }
                            ui.weak("show-delta / no-env-subtract / nearest-upsample / full-res arrive with Gates 3-4.");
                        });
                        // LAB poly→SDF (iteration 2): convert the SELECTED (else first visible)
                        // closed poly mesh into a MeshVolume primitive inside the active SDF
                        // body's tree — full boolean+blend, poly consumed (destructive, undoable).
                        ui.add_space(4.0);
                        let btn = ui.add_enabled(lab_has_poly,
                            egui::Button::new("◈ Convert poly → SDF primitive"));
                        if btn.on_hover_text(
                            "Select a CLOSED polygon mesh, then click: it becomes a MeshVolume\n\
                             primitive in the active SDF body's tree (union by default — switch to\n\
                             subtract/intersect + blend-K in the shape panel). The poly is consumed.\n\
                             Undoable. Log: docs/planning/lab/poly-mesh-sdf-operand.md").clicked() {
                            do_lab_convert.set(true);
                        }
                        if !lab_has_poly {
                            ui.label(egui::RichText::new("(no closed poly mesh in scene)").small().weak());
                        }
                    });

                    ui.separator();

                    // -- Volume March Optimizations --
                    ui.strong("Volume March");
                    ui.indent("tw_vol_march", |ui| {
                        ui.checkbox(&mut self.render.volume_early_out, "Early-out (background skip)")
.default_menu("render.volume_early_out", serde_json::json!(self.render.volume_early_out))
                            .on_hover_text("Rays that miss all body AABBs skip the march entirely.\nSaves ~70% of pixels on typical scenes. Disable for A/B timing.");
                        ui.checkbox(&mut self.render.volume_cone_step, "Cone-step march")
.default_menu("render.volume_cone_step", serde_json::json!(self.render.volume_cone_step))
                            .on_hover_text("Scale minimum step by pixel world-size at distance.\nFar pixels take bigger steps. Saves 30-50% steps at screen edges.");
                        ui.checkbox(&mut self.render.volume_tricubic, "Tricubic filtering")
.default_menu("render.volume_tricubic", serde_json::json!(self.render.volume_tricubic))
                            .on_hover_text("Catmull-Rom smooth SDF interpolation (8 taps).\nMakes 128³ look like 256³. Costs ~8x texture reads.");
                    });

                    ui.separator();

                    // -- Volume Cache --
                    if sec_match(&["RAM", "Cache", "Volume", "Pool", "Freelist"]) {
                    ui.strong("Volume Cache");
                    ui.indent("tw_volume_cache", |ui| {
                        let mut ram_enabled = self.cache.body_vol_cache.ram_cache_enabled();
                        if ui.checkbox(&mut ram_enabled, "RAM cache enabled")
                            .on_hover_text("Keep evicted/undo'd body grids in RAM.\nAvoids re-bake on undo or body switch (~2-5 ms upload vs 40-200 ms bake).")
                            .changed()
                        {
                            self.cache.body_vol_cache.set_ram_cache_enabled(ram_enabled);
                        }

                        let mut budget_gb = self.cache.body_vol_cache.ram_cache_budget_gb();
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            if ui.add(egui::Slider::new(&mut budget_gb, 0.5..=128.0)
                                .text("RAM budget (GB)")
                                .logarithmic(true))
                                .on_hover_text("Maximum RAM for cached body grids.\nHigher = more bodies survive undo/eviction without re-bake.")
                                .changed()
                            {
                                self.cache.body_vol_cache.set_ram_cache_budget_gb(budget_gb);
                            }
                        });

                        let (ram_count, ram_used, ram_budget) = self.cache.body_vol_cache.ram_cache_stats();
                        let used_mb = ram_used as f64 / (1024.0 * 1024.0);
                        let budget_mb = ram_budget as f64 / (1024.0 * 1024.0);
                        ui.label(format!("  RAM: {} entries, {:.1} / {:.0} MB", ram_count, used_mb, budget_mb));

                        let (gpu_occupied, gpu_max) = self.cache.body_vol_cache.gpu_pool_stats();
                        ui.label(format!("  GPU pool: {} / {} slots", gpu_occupied, gpu_max));
                    });
                    }

                    ui.separator();

                    // -- Snap & Interaction --
                    if sec_match(&["Snap", "Rotation", "Threshold", "Smart"]) {
                    section!(ui, "Snap & Smart Snap", {
                        self.viewport.snap_rotation_deg = 15.0;
                        self.viewport.smart_snap_threshold = 20.0;
                    });
                    ui.indent("tw_snap", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let changed = (self.viewport.snap_rotation_deg - 15.0).abs() > 0.01;
                            ui.add(egui::Slider::new(&mut self.viewport.snap_rotation_deg, 1.0..=90.0).text("Rotation snap (deg)"));
                            if changed {
                                if ui.small_button("x").clicked() { self.viewport.snap_rotation_deg = 15.0; }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let changed = (self.viewport.smart_snap_threshold - 20.0).abs() > 0.01;
                            ui.add(egui::Slider::new(&mut self.viewport.smart_snap_threshold, 5.0..=60.0).text("Smart snap threshold px"));
                            if changed {
                                if ui.small_button("x").clicked() { self.viewport.smart_snap_threshold = 20.0; }
                            }
                        });
                    });
                    }

                    ui.separator();

                    // -- Poly Edit Wire --
                    if sec_match(&["Poly", "Wire", "Wireframe", "Thickness", "Overlay"]) {
                    section!(ui, "Poly Edit Wire", {
                        self.render.poly_wire_opacity = 0.6;
                        self.render.poly_wire_thickness = 1.0;
                        self.render.poly_wire_color = [0.8, 0.9, 1.0];
                    });
                    ui.indent("tw_poly_wire", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.poly_wire_opacity, 0.0..=1.0).text("Opacity"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.poly_wire_thickness, 0.5..=5.0).text("Thickness (px)"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let mut col = egui::Color32::from_rgb(
                                (self.render.poly_wire_color[0] * 255.0) as u8,
                                (self.render.poly_wire_color[1] * 255.0) as u8,
                                (self.render.poly_wire_color[2] * 255.0) as u8,
                            );
                            if ui.color_edit_button_srgba(&mut col).changed() {
                                self.render.poly_wire_color = [col.r() as f32 / 255.0, col.g() as f32 / 255.0, col.b() as f32 / 255.0];
                            }
                            ui.label("Wire color");
                        });
                    });
                    }

                    ui.separator();

                    // -- Path Tracer Advanced --
                    if sec_match(&["Path", "PT", "Denoise", "DOF", "Aperture", "Bounces", "Clamp"]) {
                    section!(ui, "Path Tracer", {
                        self.render.pt_max_bounces = 6;
                        self.render.pt_samples_per_frame = 1;
                        self.render.pt_dof_aperture = 0.0;
                        self.render.pt_dof_falloff = 0.5;
                        self.render.pt_clamp = 10.0;
                        self.render.pt_focus_radius = 0.0;
                    });
                    ui.indent("tw_pt", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_yellow, "\u{25CF}");
                            let mut bounces = self.render.pt_max_bounces as i32;
                            ui.add(egui::Slider::new(&mut bounces, 1..=16).text("Max bounces"));
                            self.render.pt_max_bounces = bounces as u32;
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            let mut spp = self.render.pt_samples_per_frame as i32;
                            ui.add(egui::Slider::new(&mut spp, 1..=16).text("Samples/frame"));
                            self.render.pt_samples_per_frame = spp as u32;
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.pt_dof_aperture, 0.0..=1.0).text("DOF aperture"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.pt_dof_falloff, 0.0..=1.0).text("DOF falloff"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.pt_focus_radius, 0.0..=5.0).text("Focus distance"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_yellow, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.pt_clamp, 1.0..=100.0).text("Firefly clamp").logarithmic(true));
                        });
                        ui.checkbox(&mut self.render.pt_denoise, "Denoise")
.default_menu("render.pt_denoise", serde_json::json!(self.render.pt_denoise));
                        if self.render.pt_denoise {
                            ui.indent("dn_set", |ui| {
                                ui.add(egui::Slider::new(&mut self.render.denoise_strength, 0.0..=1.0).text("Strength").step_by(0.01));
                                let mut r = self.render.denoise_radius as f32;
                                if ui.add(egui::Slider::new(&mut r, 1.0..=8.0).text("Radius").step_by(1.0)).changed() { self.render.denoise_radius = r as u32; }
                            });
                        }
                    });
                    }

                    ui.separator();

                    // -- Environment Map --
                    if sec_match(&["Env", "Environment", "HDR", "Contrast", "Saturation", "Blur", "Brightness"]) {
                    section!(ui, "Environment", {
                        self.render.env_brightness = 1.0;
                        self.render.env_contrast = 1.0;
                        self.render.env_saturation = 1.0;
                        self.render.env_blur = 0.0;
                        self.render.env_reflection_blur = 0.0;
                        self.render.env_enabled = true;
                        self.render.env_bg_visible = true;
                    });
                    ui.indent("tw_env", |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.env_brightness, 0.0..=5.0).text("Brightness"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.env_contrast, 0.1..=3.0).text("Contrast"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.env_saturation, 0.0..=3.0).text("Saturation"));
                        });
                        ui.horizontal(|ui| {
                            ui.colored_label(dot_green, "\u{25CF}");
                            ui.add(egui::Slider::new(&mut self.render.env_blur, 0.0..=1.0).text("Background blur"));
                            ui.add(egui::Slider::new(&mut self.render.env_reflection_blur, 0.0..=1.0).text("Reflection blur"));
                            ui.checkbox(&mut self.render.env_enabled, "HDR enabled")
.default_menu("render.env_enabled", serde_json::json!(self.render.env_enabled));
                            // Image-only hide: lighting/reflections keep the HDRI.
                            ui.checkbox(&mut self.render.env_bg_visible, "Show HDRI in bg")
.default_menu("render.env_bg_visible", serde_json::json!(self.render.env_bg_visible));
                        });
                    });
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Reset All to Code Defaults").clicked() {
                            *tw = ToolSettings::default();
                        }
                        if ui.button("Save All as Defaults").on_hover_text("Save to ~/.azra/tweaks.json").clicked() {
                            save_tweaks = true;
                        }
                    });
                    if save_tweaks {
                        tw.save();
                    }

                    ui.separator();

                    // -- Visual debug (flight recorder) --
                    // Placed after tw.save() so the `tw` field borrow is dead and &mut self
                    // methods (toast) are callable, matching the All Commands section below.
                    ui.strong("Visual debug");
                    ui.indent("tw_visual_debug", |ui| {
                        if crate::vis_recorder::vis_enabled() {
                            ui.label("AZRA_VISLOG=1 — event ring ACTIVE (recording pipeline events)");
                        } else {
                            ui.label("AZRA_VISLOG off — ring idle (set AZRA_VISLOG=1 and restart to record events)");
                        }
                        if ui.button("Dump visual state (visdump)")
                            .on_hover_text("Write a snapshot of the SDF render-cache state for the active body \
                                            + the flight-recorder event ring to %LOCALAPPDATA%/Azra/visdump_N.log.\n\
                                            Read-only: no GPU mutations — safe to click any time.")
                            .clicked()
                        {
                            let snapshot = self.build_visdump_snapshot();
                            match crate::vis_recorder::dump_to_file(&snapshot) {
                                Some(path) => {
                                    eprintln!("[VisDump] wrote {}", path.display());
                                    self.toast(&format!("visdump written: {}", path.display()));
                                }
                                None => self.toast("visdump FAILED — could not write %LOCALAPPDATA%/Azra"),
                            }
                        }
                        if ui.button("Dump UI layout (panels)")
                            .on_hover_text(
                                "Write UI v2 dock/panel sizes, positions, and zone rects to\n\
                                 %LOCALAPPDATA%/Azra/ui_layout_dump.txt (fixed path for live agent attach).\n\
                                 Compare against docs/UI/azra_app_mockup.html oracle.",
                            )
                            .clicked()
                        {
                            self.ui.request_ui_layout_dump = true;
                        }
                    });

                    // ---- All Commands (Phase 2) ----
                    ui.add_space(12.0);
                    ui.separator();
                    ui.collapsing(egui::RichText::new("All Commands").strong(), |ui| {
                        ui.label(egui::RichText::new("Click any command to execute it. Shortcut shown on right.").weak().small());
                        ui.add_space(4.0);
                        let categories = ["File", "Edit", "Transform", "View", "Tools",
                                          "Poly", "NURBS", "UI", "Mode"];
                        for cat in &categories {
                            let cmds: Vec<_> = COMMAND_DEFS.iter()
                                .filter(|c| c.category == *cat)
                                .collect();
                            if cmds.is_empty() { continue; }
                            ui.collapsing(*cat, |ui| {
                                for cmd in &cmds {
                                    let shortcut = cmd.shortcut_act
                                        .map(|a| self.keybindings.label(a))
                                        .unwrap_or_default();
                                    let btn_text = if shortcut.is_empty() {
                                        cmd.name.to_string()
                                    } else {
                                        format!("{:.<40} {}", cmd.name, shortcut)
                                    };
                                    if ui.button(&btn_text).on_hover_text(
                                        cmd.context.unwrap_or("Global")
                                    ).clicked() {
                                        self.execute_command_by_id(cmd.id);
                                    }
                                }
                            });
                        }
                    });
                });
            });
        self.ui.show_tweak_panel = open;
        if march_render_dirty.get() { self.mark_dirty(DirtyFlags::RENDER); }
        if do_lab_convert.get() { self.convert_selected_poly_to_sdf(); }
    }
}
