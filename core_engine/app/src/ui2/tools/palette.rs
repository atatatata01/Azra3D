//! Tools palette body UI — pillar list + category rail + row paint.

use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::super::*; // ToolPillar, ToolItem, ToolCmd, ui_theme
use super::*; // tool_categories, pillar_key, tool_name_matches

impl FourthLayerApp {
    /// Tools palette body for ONE pillar — mockup `.tp-pbody`:
    ///   list (tools folder + commands folder)  |  category RAIL ON THE RIGHT.
    pub(crate) fn ui2_tools_pillar(&mut self, ui: &mut egui::Ui, pillar: ToolPillar) {
        let cats = tool_categories(pillar);
        let cur_cat = self.ui2.tool_cat.min(cats.len().saturating_sub(1));
        let mut set_cat: Option<usize> = None;
        let mut clicked_tool: Option<&'static str> = None;
        let mut clicked_cmd: Option<&'static str> = None;
        let mut toggle_sec: Option<String> = None;

        const RAIL_W: f32 = 26.0;
        const TOOL_H: f32 = 30.0; // .tp-tool
        let body_h = ui.available_height().max(40.0);
        let body_w = ui.available_width().max(40.0);
        // Pin to live pane size so list + rail reflow when dock width / height changes.
        ui.set_min_size(egui::vec2(body_w, body_h));
        ui.set_max_size(egui::vec2(body_w, body_h));
        let list_w = (body_w - RAIL_W - 1.0).max(40.0);
        let pk = pillar_key(pillar);
        let cat = &cats[cur_cat];
        let tools_key = format!("{pk}:{}", cat.name);
        let cmds_key = format!("{pk}:{}:__cmds", cat.name);
        let tools_open = !self.ui2.tool_collapsed.contains(&tools_key);
        let cmds_open = !self.ui2.tool_collapsed.contains(&cmds_key);

        // Apply scroll style so list thumb is CONTROL, not light text.
        {
            let s = &mut ui.style_mut().spacing.scroll;
            s.floating = false;
            s.foreground_color = false;
            s.bar_width = 7.0;
            s.bar_inner_margin = 3.0;
            s.bar_outer_margin = 3.0;
            ui.style_mut().visuals.widgets.inactive.bg_fill = ui_theme::SURFACE_CONTROL;
            ui.style_mut().visuals.extreme_bg_color = ui_theme::SURFACE_WELL;
        }

        egui::Frame::none().fill(ui_theme::SURFACE_PANEL).show(ui, |ui| {
            ui.set_min_height(body_h);
            ui.set_max_height(body_h);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;

                // ── LEFT: .tp-list ─────────────────────────────────────────────
                ui.allocate_ui_with_layout(
                    egui::vec2(list_w, body_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_height(body_h);
                        ui.set_max_width(list_w);
                        let active_key = self.ui2.active_tool.as_str();
                        let list_h = ui.available_height();
                        egui::ScrollArea::vertical()
                            .id_salt(("v2_tp_list", pillar as u8))
                            .auto_shrink([false, false])
                            .max_height(list_h)
                            .show(ui, |ui| {
                                // Tools folder (mockup: toolsHtml first)
                                if tp_sec(ui, cat.name, tools_open, pillar.accent()) {
                                    toggle_sec = Some(tools_key.clone());
                                }
                                if tools_open {
                                    if cat.tools.is_empty() {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new("No tools in this category")
                                                .size(11.0)
                                                .italics()
                                                .color(ui_theme::TEXT_FAINT),
                                        );
                                    }
                                    for t in cat.tools {
                                        if tp_tool_row(ui, t, pillar, active_key) {
                                            clicked_tool = Some(t.name);
                                        }
                                    }
                                }

                                // Commands folder (mockup: cmdsHtml after tools)
                                if !cat.cmds.is_empty() {
                                    let cmd_title = format!("{} Commands", cat.name);
                                    if tp_sec(ui, &cmd_title, cmds_open, pillar.accent()) {
                                        toggle_sec = Some(cmds_key.clone());
                                    }
                                    if cmds_open {
                                        for cmd in cat.cmds {
                                            if tp_cmd_row(ui, cmd) {
                                                clicked_cmd = Some(cmd.name);
                                            }
                                        }
                                    }
                                }
                                ui.add_space(8.0);
                            });
                    },
                );

                // ── RIGHT: mockup body[data-tab="v2"] .tp-rail / .tp-rtab ────────
                // Frame gutter, flush square tabs, active = panel ground + outer
                // 2px hue bar + top/bottom hairlines (editor-tab signature).
                let rail_rect = {
                    let r = ui.available_rect_before_wrap();
                    egui::Rect::from_min_size(
                        egui::pos2(r.right() - RAIL_W, r.top()),
                        egui::vec2(RAIL_W, body_h),
                    )
                };
                let _ = ui.allocate_exact_size(egui::vec2(RAIL_W, body_h), egui::Sense::hover());
                // Ground is --frame (not --toolcol): darker strip so active --panel
                // reads as lifted page, not a tinted pill in the same surface.
                ui.painter()
                    .rect_filled(rail_rect, 0.0, ui_theme::SURFACE_FRAME);
                ui.painter().vline(
                    rail_rect.left() + 0.5,
                    rail_rect.y_range(),
                    egui::Stroke::new(1.0, ui_theme::SEAM),
                );

                let hue = pillar.accent();
                let cat_font = egui::FontId::proportional(10.5);
                // padding:14px 0 on vertical-rl tab → lead/trail along the label axis
                const TAB_PAD: f32 = 14.0;
                let mut y = rail_rect.top(); // padding:0 on v2 rail — edge to edge
                for (ci, cdef) in cats.iter().enumerate() {
                    let active = ci == cur_cat;
                    // Per-tab height from label length (mockup flex:0 0 auto), not uniform.
                    let name_w = ui
                        .painter()
                        .layout_no_wrap(cdef.name.to_string(), cat_font.clone(), ui_theme::TEXT)
                        .size()
                        .x;
                    let cell_h = name_w + TAB_PAD * 2.0;
                    // Active margin-left:-1px — tab ground covers the seam so list
                    // --panel runs unbroken into the selected tab.
                    let x0 = if active {
                        rail_rect.left() - 1.0
                    } else {
                        rail_rect.left()
                    };
                    let tw = if active { RAIL_W + 1.0 } else { RAIL_W };
                    let trect =
                        egui::Rect::from_min_size(egui::pos2(x0, y), egui::vec2(tw, cell_h));
                    let id = ui.make_persistent_id(("v2_tp_rail", pillar as u8, ci));
                    let resp = ui.interact(trect, id, egui::Sense::click());
                    let hovered = resp.hovered();

                    if active {
                        // Active ground = page (--panel). Square, no radius.
                        ui.painter()
                            .rect_filled(trect, 0.0, ui_theme::SURFACE_PANEL);
                        // Hairlines close the tab against the dark strip (top/bottom).
                        ui.painter().hline(
                            trect.x_range(),
                            trect.top() + 0.5,
                            egui::Stroke::new(1.0, ui_theme::BORDER),
                        );
                        ui.painter().hline(
                            trect.x_range(),
                            trect.bottom() - 0.5,
                            egui::Stroke::new(
                                1.0,
                                if hovered {
                                    ui_theme::BORDER_HOVER
                                } else {
                                    ui_theme::BORDER
                                },
                            ),
                        );
                        // Hue bar: visual outer (right) edge, full height, 2px.
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(rail_rect.right() - 2.0, trect.top()),
                            egui::pos2(rail_rect.right(), trect.bottom()),
                        );
                        ui.painter().rect_filled(bar, 0.0, hue);
                    } else if hovered {
                        // Hover ground: mix(panel 50%, frame) — never outranks active.
                        ui.painter().rect_filled(
                            trect,
                            0.0,
                            tp_mix(ui_theme::SURFACE_PANEL, ui_theme::SURFACE_FRAME, 0.50),
                        );
                        // Bar preview: 40% opacity, scaleY ~0.55 centered.
                        let bar_h = cell_h * 0.55;
                        let bar_y0 = trect.center().y - bar_h * 0.5;
                        let bar = egui::Rect::from_min_size(
                            egui::pos2(rail_rect.right() - 2.0, bar_y0),
                            egui::vec2(2.0, bar_h),
                        );
                        ui.painter().rect_filled(
                            bar,
                            0.0,
                            egui::Color32::from_rgba_unmultiplied(hue.r(), hue.g(), hue.b(), 102),
                        );
                    }

                    // Label: inactive whisper of hue into --dim; active full --text.
                    let col = if active {
                        ui_theme::TEXT
                    } else if hovered {
                        tp_mix(hue, ui_theme::TEXT_SECONDARY, 0.45)
                    } else {
                        tp_mix(hue, ui_theme::TEXT_DIM, 0.22)
                    };
                    // vertical-rl + rotate(180°) → bottom-to-top upright labels
                    let angle = -std::f32::consts::FRAC_PI_2;
                    let galley = ui.painter().layout_no_wrap(
                        cdef.name.to_string(),
                        cat_font.clone(),
                        col,
                    );
                    let rot = egui::emath::Rot2::from_angle(angle);
                    let pos = trect.center() - rot * (galley.size() * 0.5);
                    let mut shape = egui::epaint::TextShape::new(pos, galley, col);
                    shape.angle = angle;
                    ui.painter().add(shape);
                    if hovered {
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        set_cat = Some(ci);
                    }
                    y += cell_h; // gap:0 on v2
                }
            });
        });

        if let Some(c) = set_cat {
            self.ui2.tool_cat = c;
        }
        if let Some(key) = toggle_sec {
            if !self.ui2.tool_collapsed.remove(&key) {
                self.ui2.tool_collapsed.insert(key);
            }
        }
        if let Some(name) = clicked_tool {
            self.ui2_tool_activate(pillar, name);
        }
        if let Some(name) = clicked_cmd {
            self.ui2_tool_command(pillar, name);
        }
    }
}

/// CSS `color-mix(in srgb, a t%, b)` — amount `t` of `a` into `b`.
fn tp_mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let u = 1.0 - t;
    egui::Color32::from_rgb(
        (a.r() as f32 * t + b.r() as f32 * u).round() as u8,
        (a.g() as f32 * t + b.g() as f32 * u).round() as u8,
        (a.b() as f32 * t + b.b() as f32 * u).round() as u8,
    )
}

/// Mockup `.tp-sec` — collapsible folder header (tools or commands).
/// Returns true when the header is clicked (toggle collapse).
pub(crate) fn tp_sec(ui: &mut egui::Ui, title: &str, open: bool, chip: egui::Color32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 26.0),
        egui::Sense::click(),
    );
    ui.painter().rect_filled(rect, 0.0, ui_theme::SURFACE_BAR);
    ui.painter().hline(
        rect.x_range(),
        rect.top() + 0.5,
        egui::Stroke::new(1.0, ui_theme::SEAM),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, ui_theme::SEAM),
    );
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ui_theme::SURFACE_HEADER);
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    // Caret
    let caret = if open { ph::CARET_DOWN } else { ph::CARET_RIGHT };
    ui.painter().text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        caret,
        egui::FontId::proportional(9.0),
        ui_theme::TEXT_FAINT,
    );
    // Hue chip
    ui.painter().rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 24.0, rect.center().y),
            egui::vec2(6.0, 6.0),
        ),
        2.0,
        chip,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        egui::FontId::proportional(ui_theme::FONT_CAPS),
        if resp.hovered() {
            ui_theme::TEXT_SECONDARY
        } else {
            ui_theme::TEXT_SECTION
        },
    );
    resp.clicked()
}

/// Mockup `.tp-tool` row — icon · name · key · optional wedge. Returns true on click.
pub(crate) fn tp_tool_row(
    ui: &mut egui::Ui,
    t: &ToolItem,
    pillar: ToolPillar,
    active_key: &str,
) -> bool {
    const TOOL_H: f32 = 30.0;
    let sense = if t.ready {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), TOOL_H), sense);
    let is_active = t.ready && tool_name_matches(t.name, active_key);
    if is_active {
        ui.painter().rect_filled(rect, 0.0, ui_theme::SELECTED_ROW);
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top()),
                egui::pos2(rect.left() + 2.0, rect.bottom()),
            ),
            0.0,
            ui_theme::ACCENT,
        );
    } else if t.ready && resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ui_theme::SURFACE_HEADER);
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    let icon_col = if !t.ready {
        ui_theme::TEXT_FAINT
    } else {
        pillar.accent()
    };
    let name_col = if !t.ready {
        ui_theme::TEXT_FAINT
    } else if is_active {
        egui::Color32::WHITE
    } else {
        ui_theme::TEXT_SECONDARY
    };
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        t.icon,
        egui::FontId::proportional(15.0),
        icon_col,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 36.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        t.name,
        egui::FontId::proportional(ui_theme::FONT_UI),
        name_col,
    );
    // Shortcut (mockup .tp-key)
    if !t.key.is_empty() {
        let key_col = if is_active {
            egui::Color32::from_white_alpha(0x8c)
        } else if resp.hovered() {
            ui_theme::TEXT_DIM
        } else {
            ui_theme::TEXT_FAINT
        };
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            t.key,
            egui::FontId::monospace(ui_theme::FONT_MONO),
            key_col,
        );
    } else if !t.ready {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "soon",
            egui::FontId::proportional(9.0),
            ui_theme::TEXT_FAINT,
        );
    }
    // Variants wedge (mockup .tp-wedge)
    if t.variants {
        let tip = egui::pos2(rect.right(), rect.bottom());
        let col = if is_active {
            egui::Color32::WHITE
        } else if resp.hovered() {
            ui_theme::ACCENT_LIGHT
        } else {
            ui_theme::TEXT_FAINT
        };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                tip,
                egui::pos2(tip.x - 7.0, tip.y),
                egui::pos2(tip.x, tip.y - 7.0),
            ],
            col,
            egui::Stroke::NONE,
        ));
    }
    t.ready && resp.clicked()
}

/// Mockup `.tp-cmd` row — quieter one-shot verb. Returns true on click when ready.
pub(crate) fn tp_cmd_row(ui: &mut egui::Ui, cmd: &ToolCmd) -> bool {
    const CMD_H: f32 = 28.0;
    let sense = if cmd.ready {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), CMD_H), sense);
    if cmd.ready && resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ui_theme::SURFACE_HEADER);
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    let name_col = if !cmd.ready {
        ui_theme::TEXT_FAINT
    } else if resp.hovered() {
        ui_theme::TEXT
    } else {
        ui_theme::TEXT_LABEL
    };
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        cmd.name,
        egui::FontId::proportional(ui_theme::FONT_UI),
        name_col,
    );
    // Never paint a shortcut on not-ready cmds (INTEGRATION_RULES §4 — no toast+fake key).
    if !cmd.ready {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "soon",
            egui::FontId::proportional(9.0),
            ui_theme::TEXT_FAINT,
        );
    } else if !cmd.key.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            cmd.key,
            egui::FontId::monospace(ui_theme::FONT_MONO),
            if resp.hovered() {
                ui_theme::TEXT_DIM
            } else {
                ui_theme::TEXT_FAINT
            },
        );
    }
    cmd.ready && resp.clicked()
}
