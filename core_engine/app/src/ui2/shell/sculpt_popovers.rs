//! Bodies for the sculpt tool-header popovers. Mirrors Blender:
//!   * Brush  → `brush_settings` + `brush_settings_advanced` (sculpt)
//!   * Texture → `brush_texture_settings` + tip orientation
//!   * Stroke  → `StrokePanel` + `SmoothStrokePanel`
//! Bound to the SAME `sculpt.voxel` / `sculpt.stroke_method` fields as the
//! floating Sculpt Brush panel — no duplicate state.

use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_draw_sculpt_header_popover(
        &mut self,
        ctx: &egui::Context,
        kind: SculptHeaderPopover,
        pos: egui::Pos2,
        width: f32,
        max_h: f32,
    ) -> bool {
        let mut want_close = false;
        let area_id = egui::Id::new(("v2_sculpt_head_pop", kind.ord()));
        egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(ctx.screen_rect())
            .show(ctx, |ui| {
                let frame = egui::Frame::none()
                    .fill(ui_theme::SURFACE_PANEL)
                    .stroke(egui::Stroke::new(1.0, ui_theme::SEAM))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 8));
                frame.show(ui, |ui| {
                    ui.set_width(width - 20.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(kind.label())
                                .size(11.5)
                                .strong()
                                .color(ui_theme::TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(ph::X).size(12.0).color(ui_theme::TEXT_DIM),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .frame(false),
                                )
                                .on_hover_text("Close")
                                .clicked()
                            {
                                want_close = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(max_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            match kind {
                                SculptHeaderPopover::Brush => self.ui2_sculpt_brush_popover(ui),
                                SculptHeaderPopover::Texture => self.ui2_sculpt_texture_popover(ui),
                                SculptHeaderPopover::Stroke => self.ui2_sculpt_stroke_popover(ui),
                                SculptHeaderPopover::Dyntopo => self.ui2_sculpt_dyntopo_popover(ui),
                            }
                        });
                });
            });
        want_close
    }

    fn ui2_sculpt_brush_popover(&mut self, ui: &mut egui::Ui) {
        if self.sculpt.voxel.is_none() {
            self.ui2_sculpt_brush_popover_poly(ui);
            return;
        }
        let Some(v) = self.sculpt.voxel.as_mut() else {
            return;
        };
        ui.checkbox(&mut v.per_brush_settings, "Per-brush settings")
            .on_hover_text("Each base op remembers its own knobs (Blender per-brush).");
        {
            let on = v.sculpt_on_surface;
            if ui
                .selectable_label(on, if on { "● Sculpt on Surface" } else { "○ Free (off surface)" })
                .on_hover_text("Blender / ZBrush stick-to-mesh. Off = free placement on miss.")
                .clicked()
            {
                v.sculpt_on_surface = !on;
            }
        }
        ui.checkbox(&mut v.accumulate, "Accumulate")
            .on_hover_text("Blender use_accumulate — additive dabs vs approach-to-target.");
        ui.add_enabled_ui(!v.sculpt_on_surface, |ui| {
            ui.checkbox(&mut v.front_faces_only, "Front Faces Only")
                .on_hover_text("Blender use_frontface. Forced on while Sculpt on Surface.");
        });
        ui.checkbox(&mut v.auto_smooth, "Auto Smooth")
            .on_hover_text("Blender auto_smooth_factor — mild relax after each dab.");
        ui.add_enabled_ui(v.auto_smooth, |ui| {
            ui.add(egui::Slider::new(&mut v.auto_smooth_intensity, 0.05..=1.0).text("Auto smooth"));
        });
        ui.add(egui::Slider::new(&mut v.falloff, 0.0..=1.0).text("Hardness"))
            .on_hover_text("Blender hardness — 0 soft full-radius curve, 1 hard core.");
        ui.label(egui::RichText::new("Direction").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal_wrapped(|ui| {
            for d in core_math::voxel_sculpt::BrushDirection::ALL {
                if ui
                    .selectable_label(v.brush_dir == d, d.label())
                    .on_hover_text(d.tip())
                    .clicked()
                {
                    v.brush_dir = d;
                }
            }
        });
        ui.label(egui::RichText::new("Auto Mask").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal_wrapped(|ui| {
            for m in core_math::voxel_sculpt::AutoMaskMode::ALL {
                if ui.selectable_label(v.auto_mask == m, m.label()).clicked() {
                    v.auto_mask = m;
                }
            }
        });
        ui.add_enabled_ui(v.auto_mask.is_on(), |ui| {
            ui.add(egui::Slider::new(&mut v.auto_mask_intensity, 0.05..=1.0).text("AM str"));
            ui.checkbox(&mut v.auto_mask_invert, "Invert");
        });
        ui.separator();
        let label = crate::VoxelObject::factory_label(v.brush);
        if ui.button(format!("Reset {label} factory")).clicked() {
            v.apply_brush_factory(v.brush);
        }
        if ui
            .button(format!("{} Remesh", ph::ARROWS_CLOCKWISE))
            .on_hover_text("Voxel remesh (Ctrl+R) — Blender VIEW3D_PT_sculpt_voxel_remesh")
            .clicked()
        {
            v.remesh_open = true;
            v.remesh_target_res = v.grid.res;
        }
    }

    fn ui2_sculpt_texture_popover(&mut self, ui: &mut egui::Ui) {
        if self.sculpt.voxel.is_none() {
            self.ui2_sculpt_texture_popover_poly(ui);
            return;
        }
        // Tip section (owner 2026-08-19): thumbnail SQUARE of the active tip, click → the
        // library grid right here, drag a library thumb onto the square, Import… for
        // image / ABR (Photoshop) / .blend files. Shared widgets with the Sculpt Brush panel.
        let lib_open_id = egui::Id::new("v2_tex_pop_lib_open_voxel");
        let mut lib_open = ui.data_mut(|d| *d.get_temp_mut_or(lib_open_id, false));
        let mut want_import = false;
        let mut toast: Option<String> = None;
        {
            let Some(v) = self.sculpt.voxel.as_mut() else {
                return;
            };
            ui.label(egui::RichText::new("Brush Tip").size(10.0).color(ui_theme::TEXT_FAINT));
            ui.horizontal(|ui| {
                let (clicked, dropped) = crate::brush_tip_lib::active_tip_square(
                    ui,
                    &mut self.tip_preview_cache,
                    v.brush_tip.as_ref(),
                    &v.brush_tip_name,
                    72.0,
                );
                if clicked {
                    lib_open = !lib_open;
                }
                if let Some((name, path, tip)) = dropped {
                    v.brush_tip = Some(tip);
                    v.brush_tip_name = name.clone();
                    v.brush_tip_path = Some(path.to_string_lossy().to_string());
                    toast = Some(format!("Tip: {name}"));
                }
                ui.vertical(|ui| {
                    let tip_name = if v.brush_tip_name.is_empty() {
                        "(no tip — radial falloff only)"
                    } else {
                        v.brush_tip_name.as_str()
                    };
                    ui.label(
                        egui::RichText::new(tip_name)
                            .size(ui_theme::FONT_TAB)
                            .color(ui_theme::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        if ui.small_button("Library…").clicked() {
                            lib_open = !lib_open;
                        }
                        if ui
                            .small_button("Import…")
                            .on_hover_text("Load brush shapes: images, Photoshop ABR, .blend alphas")
                            .clicked()
                        {
                            want_import = true;
                        }
                        if v.brush_tip.is_some() && ui.small_button("Clear").clicked() {
                            v.brush_tip = None;
                            v.brush_tip_name.clear();
                            v.brush_tip_path = None;
                        }
                    });
                });
            });
            if lib_open {
                if let Some((name, path, tip)) = crate::brush_tip_lib::tip_library_grid(
                    ui,
                    &mut self.tip_preview_cache,
                    &v.brush_tip_name,
                    "v2_tex_pop_voxel",
                ) {
                    v.brush_tip = Some(tip);
                    v.brush_tip_name = name.clone();
                    v.brush_tip_path = Some(path.to_string_lossy().to_string());
                    toast = Some(format!("Tip: {name}"));
                }
            }
        }
        ui.data_mut(|d| d.insert_temp(lib_open_id, lib_open));
        if want_import {
            self.request_brush_tip_file_dialog();
        }
        if let Some(t) = toast {
            self.toast(&t);
        }
        let Some(v) = self.sculpt.voxel.as_mut() else {
            return;
        };
        ui.separator();
        // Blender texture_slot.map_mode ≈ our TipOrientation (closest analog).
        ui.label(egui::RichText::new("Mapping").size(10.0).color(ui_theme::TEXT_FAINT))
            .on_hover_text(
                "Blender map_mode analog: Fixed / Follow Stroke (Rake) / View Plane / Random.",
            );
        ui.horizontal_wrapped(|ui| {
            for o in TipOrientation::ALL {
                let sel = TipOrientation::from_u32(v.tip_orient) == o
                    || (v.tip_follow_stroke
                        && o == TipOrientation::FollowStroke
                        && v.tip_orient == TipOrientation::Fixed.as_u32());
                if ui.selectable_label(sel, o.label()).on_hover_text(o.tip()).clicked() {
                    v.tip_orient = o.as_u32();
                    v.tip_follow_stroke = o == TipOrientation::FollowStroke;
                }
            }
        });
        ui.add(egui::Slider::new(&mut v.tip_angle, 0.0..=360.0).text("Angle").suffix("°"))
            .on_hover_text("Blender texture_slot.angle.");
        ui.add(egui::Slider::new(&mut v.tip_contrast, 0.25..=2.0).text("Contrast"))
            .on_hover_text("Tip mid-point contrast (1 = identity).");
        ui.add(egui::Slider::new(&mut v.dyn_random_rotation, 0.0..=1.0).text("Random Rotation"))
            .on_hover_text("Blender use_random + random_angle — 0 off, 1 = ±180°.");
    }

    fn ui2_sculpt_stroke_popover(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Stroke Method")
                .size(10.0)
                .color(ui_theme::TEXT_FAINT),
        )
        .on_hover_text("Blender brush.stroke_method (StrokePanel).");
        let cur = self.sculpt.stroke_method;
        let mut pick: Option<SculptStrokeMethod> = None;
        for m in SculptStrokeMethod::ALL {
            let suffix = if m.implemented() { "" } else { "  (Space)" };
            let lbl = format!("{}{suffix}", m.label());
            if ui
                .selectable_label(cur == m, lbl)
                .on_hover_text(m.tip())
                .clicked()
            {
                pick = Some(m);
            }
        }
        if let Some(m) = pick {
            self.sculpt.stroke_method = m;
            if !m.implemented() {
                self.toast(&format!("{} not wired yet — using Space", m.label()));
            }
        }
        ui.separator();
        let method = self.sculpt.stroke_method.effective();
        let (sp, jit, lazy) = if let Some(v) = self.sculpt.voxel.as_mut() {
            (&mut v.stroke_spacing, &mut v.dyn_jitter, &mut v.lazy)
        } else {
            (
                &mut self.sculpt.poly_tool.stroke_spacing,
                &mut self.sculpt.poly_tool.dyn_jitter,
                &mut self.sculpt.poly_tool.lazy,
            )
        };
        let space_on = matches!(
            method,
            SculptStrokeMethod::Space | SculptStrokeMethod::Airbrush
        );
        ui.add_enabled_ui(space_on || !method.implemented(), |ui| {
            ui.add(egui::Slider::new(sp, 0.03..=0.40).text("Spacing"))
                .on_hover_text("Blender brush.spacing — fraction of radius (Space / Airbrush).");
        });
        ui.add(egui::Slider::new(jit, 0.0..=1.0).text("Jitter"))
            .on_hover_text("Blender brush.jitter — position noise as × radius.");
        ui.add(egui::Slider::new(lazy, 0.0..=1.0).text("Stabilize"))
            .on_hover_text("Blender use_smooth_stroke / LazyMouse. 0 = off.");
        if method == SculptStrokeMethod::Airbrush {
            let mut rate = if self.sculpt.airbrush_rate > 1e-4 {
                self.sculpt.airbrush_rate
            } else {
                0.09
            };
            if ui
                .add(egui::Slider::new(&mut rate, 0.016..=0.50).text("Rate").suffix(" s"))
                .on_hover_text("Blender brush.rate — seconds between still-cursor dabs.")
                .changed()
            {
                self.sculpt.airbrush_rate = rate;
            }
        }
    }

    /// Dyntopo settings (Blender VIEW3D_PT_sculpt_dyntopo analog) — Blender tech since
    /// DYNTOPO_PLAN Phase B: these are the refine KERNEL's knobs (tool state, no undo
    /// entries), not modifier params. Detailing Constant / Brush + Refine Method.
    fn ui2_sculpt_dyntopo_popover(&mut self, ui: &mut egui::Ui) {
        self.sculpt.poly_ensure_defaults();
        ui.checkbox(&mut self.sculpt.dyn_topo, "Dyntopo (dynamic topology)")
            .on_hover_text(
                "Edges under the brush subdivide/collapse to the detail size DURING the \
                 stroke; the rest of the mesh keeps its exact topology (Blender tech). \
                 Direct Surface sculpt only — Sculpt-layer slots keep fixed topology.",
            );
        if !self.sculpt.dyn_topo {
            ui.label(
                egui::RichText::new("Toggle on — settings below apply to the next stroke.")
                    .size(10.0)
                    .color(ui_theme::TEXT_FAINT),
            );
        }
        ui.separator();
        // Detailing (Blender: Relative/Constant/Brush — ours: Constant / Brush).
        ui.label(egui::RichText::new("Detailing").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.sculpt.poly_dyntopo_brush_detail, "Constant")
                .on_hover_text("Fixed target edge length (the slider below), Blender Constant Detail.")
                .clicked()
            {
                self.sculpt.poly_dyntopo_brush_detail = false;
            }
            if ui
                .selectable_label(self.sculpt.poly_dyntopo_brush_detail, "Brush")
                .on_hover_text(
                    "Detail follows brush size (Blender Brush Detail) — a smaller brush \
                     carves finer topology.",
                )
                .clicked()
            {
                self.sculpt.poly_dyntopo_brush_detail = true;
            }
        });
        if self.sculpt.poly_dyntopo_brush_detail {
            ui.add(
                egui::Slider::new(&mut self.sculpt.poly_dyntopo_detail, 4.0..=48.0)
                    .text("Detail"),
            )
            .on_hover_text("Edges across the brush diameter (higher = finer under the brush).");
        } else {
            ui.add(
                egui::Slider::new(&mut self.sculpt.poly_dyntopo_const_detail, 0.005..=0.3)
                    .logarithmic(true)
                    .text("Detail size"),
            )
            .on_hover_text("Target edge length in scene units (lower = finer).");
        }
        // Refine method (Blender: Subdivide Edges / Collapse Edges / Subdivide Collapse).
        ui.label(egui::RichText::new("Refine Method").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.sculpt.poly_dyntopo_collapse, "Subdivide Edges")
                .on_hover_text("Only split long edges — never simplify.")
                .clicked()
            {
                self.sculpt.poly_dyntopo_collapse = false;
            }
            if ui
                .selectable_label(self.sculpt.poly_dyntopo_collapse, "Subdivide Collapse")
                .on_hover_text("Split long edges AND collapse short ones (Blender default).")
                .clicked()
            {
                self.sculpt.poly_dyntopo_collapse = true;
            }
        });
        ui.label(
            egui::RichText::new(
                "A topology-changing stroke commits the mesh and undoes as one whole step.",
            )
            .size(10.0)
            .color(ui_theme::TEXT_FAINT),
        );
    }

    fn ui2_sculpt_brush_popover_poly(&mut self, ui: &mut egui::Ui) {
        {
            let v = &mut self.sculpt.poly_tool;
            ui.checkbox(&mut v.sculpt_on_surface, "Sculpt on Surface");
            ui.checkbox(&mut v.accumulate, "Accumulate");
            ui.add_enabled_ui(!v.sculpt_on_surface, |ui| {
                ui.checkbox(&mut v.front_faces_only, "Front Faces Only");
            });
            ui.checkbox(&mut v.auto_smooth, "Auto Smooth");
            ui.add_enabled_ui(v.auto_smooth, |ui| {
                ui.add(egui::Slider::new(&mut v.auto_smooth_intensity, 0.05..=1.0).text("Auto smooth"));
            });
            ui.add(egui::Slider::new(&mut v.falloff, 0.0..=1.0).text("Hardness"));
        }
        self.sculpt.poly_hardness = self.sculpt.poly_tool.falloff;
        ui.label(egui::RichText::new("Direction").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal_wrapped(|ui| {
            for d in core_math::voxel_sculpt::BrushDirection::ALL {
                if ui
                    .selectable_label(self.sculpt.poly_tool.brush_dir == d, d.label())
                    .clicked()
                {
                    self.sculpt.poly_tool.brush_dir = d;
                }
            }
        });
        ui.label(egui::RichText::new("Auto Mask").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal_wrapped(|ui| {
            for m in core_math::voxel_sculpt::AutoMaskMode::ALL {
                if ui
                    .selectable_label(self.sculpt.poly_tool.auto_mask == m, m.label())
                    .clicked()
                {
                    self.sculpt.poly_tool.auto_mask = m;
                }
            }
        });
        ui.separator();
        if ui
            .button(format!("{} Remesh", ph::ARROWS_CLOCKWISE))
            .on_hover_text("Voxel remesh this poly body (Ctrl+R) — rebuilds even topology")
            .clicked()
        {
            self.poly_sculpt_open_remesh();
        }
    }

    fn ui2_sculpt_texture_popover_poly(&mut self, ui: &mut egui::Ui) {
        // Same tip section as the voxel popover, bound to the PolySculpt tip
        // (`sculpt.poly_tip`) — poly stamps sample the tip since 2026-08-19.
        let lib_open_id = egui::Id::new("v2_tex_pop_lib_open_poly");
        let mut lib_open = ui.data_mut(|d| *d.get_temp_mut_or(lib_open_id, false));
        let mut want_import = false;
        let mut toast: Option<String> = None;
        ui.label(egui::RichText::new("Brush Tip").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal(|ui| {
            let (clicked, dropped) = crate::brush_tip_lib::active_tip_square(
                ui,
                &mut self.tip_preview_cache,
                self.sculpt.poly_tip.as_ref(),
                &self.sculpt.poly_tip_name,
                72.0,
            );
            if clicked {
                lib_open = !lib_open;
            }
            if let Some((name, path, tip)) = dropped {
                self.sculpt.poly_tip = Some(tip);
                self.sculpt.poly_tip_name = name.clone();
                self.sculpt.poly_tip_path = Some(path.to_string_lossy().to_string());
                toast = Some(format!("Tip: {name}"));
            }
            ui.vertical(|ui| {
                let tip_name = if self.sculpt.poly_tip_name.is_empty() {
                    "(no tip — radial falloff only)"
                } else {
                    self.sculpt.poly_tip_name.as_str()
                };
                ui.label(
                    egui::RichText::new(tip_name)
                        .size(ui_theme::FONT_TAB)
                        .color(ui_theme::TEXT_SECONDARY),
                );
                ui.horizontal(|ui| {
                    if ui.small_button("Library…").clicked() {
                        lib_open = !lib_open;
                    }
                    if ui
                        .small_button("Import…")
                        .on_hover_text("Load brush shapes: images, Photoshop ABR, .blend alphas")
                        .clicked()
                    {
                        want_import = true;
                    }
                    if self.sculpt.poly_tip.is_some() && ui.small_button("Clear").clicked() {
                        self.sculpt.poly_tip = None;
                        self.sculpt.poly_tip_name.clear();
                        self.sculpt.poly_tip_path = None;
                    }
                });
            });
        });
        if lib_open {
            if let Some((name, path, tip)) = crate::brush_tip_lib::tip_library_grid(
                ui,
                &mut self.tip_preview_cache,
                &self.sculpt.poly_tip_name,
                "v2_tex_pop_poly",
            ) {
                self.sculpt.poly_tip = Some(tip);
                self.sculpt.poly_tip_name = name.clone();
                self.sculpt.poly_tip_path = Some(path.to_string_lossy().to_string());
                toast = Some(format!("Tip: {name}"));
            }
        }
        ui.data_mut(|d| d.insert_temp(lib_open_id, lib_open));
        if want_import {
            self.request_brush_tip_file_dialog();
        }
        if let Some(t) = toast {
            self.toast(&t);
        }
        ui.separator();
        let v = &mut self.sculpt.poly_tool;
        ui.label(egui::RichText::new("Mapping").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.horizontal_wrapped(|ui| {
            for o in TipOrientation::ALL {
                let sel = TipOrientation::from_u32(v.tip_orient) == o;
                if ui.selectable_label(sel, o.label()).on_hover_text(o.tip()).clicked() {
                    v.tip_orient = o.as_u32();
                    v.tip_follow_stroke = o == TipOrientation::FollowStroke;
                }
            }
        });
        ui.add(egui::Slider::new(&mut v.tip_angle, 0.0..=360.0).text("Angle").suffix("°"));
        ui.add(egui::Slider::new(&mut v.tip_contrast, 0.25..=2.0).text("Contrast"));
        ui.add(egui::Slider::new(&mut v.dyn_random_rotation, 0.0..=1.0).text("Random Rotation"));
    }
}
