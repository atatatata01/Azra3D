//! In-viewport sculpt TOOL HEADER — Blender `VIEW3D_HT_tool_header` +
//! `properties_paint_common.brush_shared_settings` (direction / size / strength)
//! plus the three header popovers (`VIEW3D_PT_tools_brush_settings`, `_texture`,
//! `_stroke` in `space_view3d_toolbar.py`).
//!
//! Lives at the TOP of the 3D scene rect (the empty strip above the mesh), NOT
//! in the ctx-bar chrome. Same hit rule as `ui2_shading_overlay`: the Area only
//! eats input inside its well + popover; the rest of the viewport orbits.

use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_sculpt_header_overlay(
        &mut self,
        ctx: &egui::Context,
        _ui: &mut egui::Ui,
        rect: egui::Rect,
    ) {
        if self.tool.work_mode != WorkMode::Sculpt {
            self.ui2.sculpt_header_popover = None;
            return;
        }
        if rect.width() < 80.0 || rect.height() < 48.0 {
            return;
        }

        let inset = 10.0;
        let pos = egui::pos2(rect.left() + inset, rect.top() + inset);
        let max_w = (rect.width() - inset - 200.0).clamp(220.0, 1100.0);

        let mut open_pop: Option<SculptHeaderPopover> = None;
        let mut brush_anchor = pos;
        let mut tex_anchor = pos;
        let mut stroke_anchor = pos;
        let mut dyntopo_anchor = pos;

        let area_id = egui::Id::new("v2_sculpt_header");
        egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(rect)
            .show(ctx, |ui| {
                ui.set_max_width(max_w);
                let frame = egui::Frame::none()
                    .fill(ui_theme::SURFACE_WELL)
                    .stroke(egui::Stroke::new(1.0, ui_theme::SEAM))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(8, 4));
                frame.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.horizontal_wrapped(|ui| {
                        if self.sculpt.voxel.is_some() {
                            self.ui2_sculpt_header_row(ui, &mut open_pop, &mut brush_anchor, &mut tex_anchor, &mut stroke_anchor);
                        } else if self.sculpt.poly_edit.is_some()
                            || self.poly_sculpt_target().is_some()
                        {
                            self.ui2_sculpt_header_row_poly(
                                ui,
                                &mut open_pop,
                                &mut brush_anchor,
                                &mut tex_anchor,
                                &mut stroke_anchor,
                                &mut dyntopo_anchor,
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("Sculpt · pick a voxel or poly body")
                                    .size(ui_theme::FONT_TAB)
                                    .color(ui_theme::TEXT_FAINT),
                            );
                        }
                    });
                });
            });

        if let Some(k) = open_pop {
            self.ui2.sculpt_header_popover = if self.ui2.sculpt_header_popover == Some(k) {
                None
            } else {
                Some(k)
            };
        }

        if let Some(kind) = self.ui2.sculpt_header_popover {
            let anchor = match kind {
                SculptHeaderPopover::Brush => brush_anchor,
                SculptHeaderPopover::Texture => tex_anchor,
                SculptHeaderPopover::Stroke => stroke_anchor,
                SculptHeaderPopover::Dyntopo => dyntopo_anchor,
            };
            let pop_w = 300.0_f32.min(rect.width() - 20.0).max(200.0);
            let pop_x = anchor.x.clamp(rect.left() + 6.0, (rect.right() - pop_w - 6.0).max(rect.left() + 6.0));
            let pop_y = (pos.y + 32.0).min(rect.bottom() - 80.0);
            let max_h = (rect.bottom() - pop_y - 12.0).clamp(120.0, 520.0);
            let close = self.ui2_draw_sculpt_header_popover(
                ctx,
                kind,
                egui::pos2(pop_x, pop_y),
                pop_w,
                max_h,
            );
            if close {
                self.ui2.sculpt_header_popover = None;
            } else {
                let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                let click = ctx.input(|i| i.pointer.primary_clicked() || i.pointer.secondary_clicked());
                if esc {
                    self.ui2.sculpt_header_popover = None;
                } else if click && open_pop.is_none() {
                    let pop_id = egui::Id::new(("v2_sculpt_head_pop", kind.ord()));
                    let pointer = ctx.input(|i| i.pointer.interact_pos());
                    let inside_pop = ctx
                        .memory(|m| m.area_rect(pop_id))
                        .map(|r| pointer.map_or(false, |p| r.contains(p)))
                        .unwrap_or(false);
                    let inside_bar = ctx
                        .memory(|m| m.area_rect(area_id))
                        .map(|r| pointer.map_or(false, |p| r.contains(p)))
                        .unwrap_or(false);
                    if !inside_pop && !inside_bar {
                        self.ui2.sculpt_header_popover = None;
                    }
                }
            }
        }
    }

    fn ui2_sculpt_header_row(
        &mut self,
        ui: &mut egui::Ui,
        open_pop: &mut Option<SculptHeaderPopover>,
        brush_anchor: &mut egui::Pos2,
        tex_anchor: &mut egui::Pos2,
        stroke_anchor: &mut egui::Pos2,
    ) {
        {
            let Some(v) = self.sculpt.voxel.as_mut() else { return };

            let brush_lbl = crate::VoxelObject::factory_label(v.brush);
            ui.label(
                egui::RichText::new(format!("{} {brush_lbl}", ph::PAINT_BRUSH))
                    .size(ui_theme::FONT_TAB)
                    .color(ui_theme::SCULPT_ORANGE)
                    .strong(),
            );

            // Blender `brush.direction` expand=True (Add / Subtract).
            let add_on = v.brush_add;
            if ui
                .selectable_label(add_on, egui::RichText::new("Add").size(ui_theme::FONT_TAB))
                .on_hover_text("Add (Zadd / Blender ADD). Ctrl flips this stroke.")
                .clicked()
            {
                v.brush_add = true;
            }
            if ui
                .selectable_label(!add_on, egui::RichText::new("Subtract").size(ui_theme::FONT_TAB))
                .on_hover_text("Subtract (Zsub / Blender SUBTRACT). Ctrl flips this stroke.")
                .clicked()
            {
                v.brush_add = false;
            }

            ui.add_space(4.0);
            ui.label(egui::RichText::new("Size").size(10.0).color(ui_theme::TEXT_FAINT));
            ui.add(
                egui::DragValue::new(&mut v.radius)
                    .speed(0.005)
                    .range(0.01..=2.0)
                    .max_decimals(3),
            )
            .on_hover_text("Brush size (Blender Radius). Z / X or scroll.");
            let p2r = v.pressure_to_radius;
            if ui
                .selectable_label(p2r, ph::HAND_POINTING)
                .on_hover_text("Pressure → Radius (Blender use_pressure_size)")
                .clicked()
            {
                v.pressure_to_radius = !p2r;
                if v.pressure_to_radius {
                    v.pressure_enabled = true;
                }
            }

            ui.label(egui::RichText::new("Strength").size(10.0).color(ui_theme::TEXT_FAINT));
            ui.add(
                egui::DragValue::new(&mut v.strength)
                    .speed(0.01)
                    .range(0.0..=1.0)
                    .max_decimals(2),
            )
            .on_hover_text("Brush strength (Blender Strength). Shift+Z / Shift+X.");
            let p2s = v.pressure_to_strength;
            if ui
                .selectable_label(p2s, ph::HAND_POINTING)
                .on_hover_text("Pressure → Strength (Blender use_pressure_strength)")
                .clicked()
            {
                v.pressure_to_strength = !p2s;
                if v.pressure_to_strength {
                    v.pressure_enabled = true;
                }
            }

            let cur_fo = v.falloff_preset;
            egui::ComboBox::from_id_salt("v2_sculpt_hdr_falloff")
                .selected_text(
                    egui::RichText::new(cur_fo.label())
                        .size(ui_theme::FONT_TAB)
                        .color(ui_theme::TEXT_SECONDARY),
                )
                .width(78.0)
                .show_ui(ui, |ui| {
                    for fo in core_math::voxel_sculpt::FalloffPreset::ALL {
                        if ui.selectable_label(v.falloff_preset == fo, fo.label()).clicked() {
                            v.falloff_preset = fo;
                        }
                    }
                });

            ui.add_space(4.0);
            for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                let on = v.symmetry[i];
                let col = if on {
                    match i {
                        0 => ui_theme::AXIS_X,
                        1 => ui_theme::AXIS_Y,
                        _ => ui_theme::AXIS_Z,
                    }
                } else {
                    ui_theme::TEXT_FAINT
                };
                if ui
                    .selectable_label(on, egui::RichText::new(*axis).size(ui_theme::FONT_TAB).color(col))
                    .on_hover_text(format!("Mirror {axis} (Blender sculpt symmetry)"))
                    .clicked()
                {
                    v.symmetry[i] = !on;
                }
            }
        }

        ui.add_space(4.0);
        let (b, t, s) = (
            header_pop_btn(ui, "Brush Settings", ph::PAINT_BRUSH, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Brush)),
            header_pop_btn(ui, "Texture", ph::IMAGE, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Texture)),
            header_pop_btn(ui, "Stroke", ph::LINE_SEGMENTS, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Stroke)),
        );
        *brush_anchor = b.rect.left_bottom();
        *tex_anchor = t.rect.left_bottom();
        *stroke_anchor = s.rect.left_bottom();
        if b.clicked() {
            *open_pop = Some(SculptHeaderPopover::Brush);
        }
        if t.clicked() {
            *open_pop = Some(SculptHeaderPopover::Texture);
        }
        if s.clicked() {
            *open_pop = Some(SculptHeaderPopover::Stroke);
        }
    }

    /// Same header as voxel, bound to `sculpt.poly_*` (poly object / PolySculpt).
    fn ui2_sculpt_header_row_poly(
        &mut self,
        ui: &mut egui::Ui,
        open_pop: &mut Option<SculptHeaderPopover>,
        brush_anchor: &mut egui::Pos2,
        tex_anchor: &mut egui::Pos2,
        stroke_anchor: &mut egui::Pos2,
        dyntopo_anchor: &mut egui::Pos2,
    ) {
        self.sculpt.poly_ensure_defaults();
        let brush = self.sculpt.poly_brush.unwrap_or(core_math::voxel_sculpt::VoxelBrush::Draw);
        let brush_lbl = crate::VoxelObject::factory_label(brush);
        ui.label(
            egui::RichText::new(format!("{} {brush_lbl}", ph::PAINT_BRUSH))
                .size(ui_theme::FONT_TAB)
                .color(ui_theme::SCULPT_ORANGE)
                .strong(),
        );

        let add_on = self.sculpt.poly_tool.brush_add;
        if ui
            .selectable_label(add_on, egui::RichText::new("Add").size(ui_theme::FONT_TAB))
            .on_hover_text("Add (Zadd). Ctrl flips this stroke.")
            .clicked()
        {
            self.sculpt.poly_tool.brush_add = true;
        }
        if ui
            .selectable_label(!add_on, egui::RichText::new("Subtract").size(ui_theme::FONT_TAB))
            .on_hover_text("Subtract (Zsub). Ctrl flips this stroke.")
            .clicked()
        {
            self.sculpt.poly_tool.brush_add = false;
        }

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Size").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.add(
            egui::DragValue::new(&mut self.sculpt.poly_radius)
                .speed(0.005)
                .range(0.01..=4.0)
                .max_decimals(3),
        );
        let p2r = self.sculpt.poly_pressure_to_radius;
        if ui
            .selectable_label(p2r, ph::HAND_POINTING)
            .on_hover_text("Pressure → Radius")
            .clicked()
        {
            self.sculpt.poly_pressure_to_radius = !p2r;
        }

        ui.label(egui::RichText::new("Strength").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.add(
            egui::DragValue::new(&mut self.sculpt.poly_strength)
                .speed(0.01)
                .range(0.0..=2.0)
                .max_decimals(2),
        );
        let p2s = self.sculpt.poly_pressure_to_strength;
        if ui
            .selectable_label(p2s, ph::HAND_POINTING)
            .on_hover_text("Pressure → Strength")
            .clicked()
        {
            self.sculpt.poly_pressure_to_strength = !p2s;
        }

        let cur_fo = self.sculpt.poly_tool.falloff_preset;
        egui::ComboBox::from_id_salt("v2_sculpt_hdr_falloff_poly")
            .selected_text(
                egui::RichText::new(cur_fo.label())
                    .size(ui_theme::FONT_TAB)
                    .color(ui_theme::TEXT_SECONDARY),
            )
            .width(78.0)
            .show_ui(ui, |ui| {
                for fo in core_math::voxel_sculpt::FalloffPreset::ALL {
                    if ui
                        .selectable_label(self.sculpt.poly_tool.falloff_preset == fo, fo.label())
                        .clicked()
                    {
                        self.sculpt.poly_tool.falloff_preset = fo;
                    }
                }
            });

        ui.add_space(4.0);
        for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
            let on = self.sculpt.poly_symmetry[i];
            let col = if on {
                match i {
                    0 => ui_theme::AXIS_X,
                    1 => ui_theme::AXIS_Y,
                    _ => ui_theme::AXIS_Z,
                }
            } else {
                ui_theme::TEXT_FAINT
            };
            if ui
                .selectable_label(on, egui::RichText::new(*axis).size(ui_theme::FONT_TAB).color(col))
                .on_hover_text(format!("Mirror {axis}"))
                .clicked()
            {
                self.sculpt.poly_symmetry[i] = !on;
            }
        }

        // Dyntopo — GLOBAL sculpt tool state on the header next to symmetry (owner
        // 2026-08-19). Blender tech since DYNTOPO_PLAN Phase B: local edge
        // subdivide/collapse under the brush, per stamp, on the working mesh.
        ui.add_space(4.0);
        {
            let on = self.sculpt.dyn_topo;
            let col = if on { ui_theme::SCULPT_ORANGE } else { ui_theme::TEXT_FAINT };
            let resp = ui
                .selectable_label(on, egui::RichText::new("Dyntopo").size(ui_theme::FONT_TAB).color(col))
                .on_hover_text(
                    "Dynamic topology (Blender tech): edges under the brush subdivide/\
                     collapse to the detail size DURING the stroke — the rest of the mesh \
                     keeps its exact topology. Caret = settings. A topology-changing \
                     stroke undoes as one whole step.",
                );
            *dyntopo_anchor = resp.rect.left_bottom();
            if resp.clicked() {
                self.sculpt.poly_ensure_defaults();
                self.sculpt.dyn_topo = !on;
            }
            // Settings caret (Blender's Dyntopo panel analog).
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(ph::CARET_DOWN)
                            .size(10.0)
                            .color(if self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Dyntopo) {
                                ui_theme::ACCENT_LIGHT
                            } else {
                                ui_theme::TEXT_FAINT
                            }),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .frame(false),
                )
                .on_hover_text("Dyntopo settings — detailing mode, detail size, refine method")
                .clicked()
            {
                *open_pop = Some(SculptHeaderPopover::Dyntopo);
            }
        }

        if self.sculpt.poly_edit.is_some()
            && ui.button(egui::RichText::new("Done").size(ui_theme::FONT_TAB)).clicked()
        {
            self.poly_sculpt_leave_edit();
        }

        ui.add_space(4.0);
        let (b, t, s) = (
            header_pop_btn(ui, "Brush Settings", ph::PAINT_BRUSH, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Brush)),
            header_pop_btn(ui, "Texture", ph::IMAGE, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Texture)),
            header_pop_btn(ui, "Stroke", ph::LINE_SEGMENTS, self.ui2.sculpt_header_popover == Some(SculptHeaderPopover::Stroke)),
        );
        *brush_anchor = b.rect.left_bottom();
        *tex_anchor = t.rect.left_bottom();
        *stroke_anchor = s.rect.left_bottom();
        if b.clicked() {
            *open_pop = Some(SculptHeaderPopover::Brush);
        }
        if t.clicked() {
            *open_pop = Some(SculptHeaderPopover::Texture);
        }
        if s.clicked() {
            *open_pop = Some(SculptHeaderPopover::Stroke);
        }
    }
}

fn header_pop_btn(ui: &mut egui::Ui, label: &str, icon: &str, open: bool) -> egui::Response {
    let text = format!("{icon} {label} {}", ph::CARET_DOWN);
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(ui_theme::FONT_TAB)
                .color(if open { ui_theme::ACCENT_LIGHT } else { ui_theme::TEXT_SECONDARY }),
        )
        .fill(if open { ui_theme::SURFACE_CONTROL } else { egui::Color32::TRANSPARENT })
        .frame(open),
    )
    .on_hover_text(match label {
        "Brush Settings" => "Brush Settings — Blender VIEW3D_PT_tools_brush_settings",
        "Texture" => "Texture / tip mapping — Blender VIEW3D_PT_tools_brush_texture",
        _ => "Stroke method + spacing — Blender VIEW3D_PT_tools_brush_stroke",
    })
}
