// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use core_math::scene_graph::{NodePayload, NodeId};
use super::*;

pub(crate) const TAB_PAD_X: f32 = 10.0;
pub(crate) const TAB_GAP: f32 = 3.0;
/// Width reserved for the per-tab float grip (multi-tab groups only).
pub(crate) const TAB_DOTS_W: f32 = 14.0;

pub(crate) fn group_tab_strip(
    ui: &mut egui::Ui,
    gi: usize,
    g: &PanelGroup,
    floating: bool,
    act: &mut Option<Act>,
    tab_drag: &mut Option<(u64, usize)>,
    tab_float_drag: &mut Option<(u64, usize)>,
) {
    // Mockup .tp-ptabs / .ptabs: ground is --panel, continuous with body.
    let frame = egui::Frame::none()
        .fill(ui_theme::SURFACE_PANEL)
        .inner_margin(egui::Margin::symmetric(6, 0));
    frame.show(ui, |ui| {
        let strip_w = ui.available_width();
        ui.set_min_width(strip_w);
        ui.set_max_width(strip_w);
        ui.set_height(HEADER_H);
        let strip_r = ui.max_rect().expand2(egui::vec2(6.0, 0.0));
        ui.painter().hline(
            strip_r.x_range(),
            strip_r.bottom() - 0.5,
            egui::Stroke::new(1.0, ui_theme::SEAM),
        );
        // Double-click empty strip area → toggle window collapse (body).
        let strip_id = ui.make_persistent_id(("v2_strip_dbl", g.id));
        let strip_resp = ui.interact(strip_r, strip_id, egui::Sense::click());
        if strip_resp.double_clicked() {
            *act = Some(Act::Collapse(gi));
        }

        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let grip = ui.add(
                egui::Label::new(
                    egui::RichText::new(ph::DOTS_SIX_VERTICAL)
                        .size(15.0)
                        .color(ui_theme::TEXT_FAINT),
                )
                .sense(egui::Sense::click_and_drag()),
            );
            if grip.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
                let gr = grip.rect.expand2(egui::vec2(4.0, 3.0));
                ui.painter()
                    .rect_filled(gr, ui_theme::RADIUS_CONTROL, ui_theme::SURFACE_CONTROL);
                ui.painter().text(
                    grip.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::DOTS_SIX_VERTICAL,
                    egui::FontId::proportional(15.0),
                    ui_theme::ACCENT_LIGHT,
                );
            }
            if grip.dragged() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
                if let Some(p) = grip.interact_pointer_pos() {
                    *act = Some(Act::GripDrag {
                        id: g.id,
                        floating,
                        pos: p,
                        delta: grip.drag_delta(),
                        released: false,
                    });
                }
            }
            if grip.drag_stopped() {
                if let Some(p) = grip.interact_pointer_pos() {
                    *act = Some(Act::GripDrag {
                        id: g.id,
                        floating,
                        pos: p,
                        delta: egui::Vec2::ZERO,
                        released: true,
                    });
                }
            }
            // Grip menu — full mockup openPg: Collapse · Head · Float · Close · Hide zone · Add
            let popup_id = ui.make_persistent_id(("v2_grip", g.id));
            if grip.clicked() || grip.secondary_clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(
                ui,
                popup_id,
                &grip,
                egui::PopupCloseBehavior::CloseOnClick,
                |ui| {
                    ui.set_min_width(180.0);
                    let cl = if g.collapsed { "Expand" } else { "Collapse" };
                    if ui
                        .button(format!("{}  {}", ph::CARET_UP, cl))
                        .clicked()
                    {
                        *act = Some(Act::Collapse(gi));
                    }
                    let hc = if g.head_coll {
                        "Show header"
                    } else {
                        "Reduce header"
                    };
                    if ui
                        .button(format!("{}  {}", ph::MINUS, hc))
                        .clicked()
                    {
                        *act = Some(Act::HeadCollapse(gi));
                    }
                    let fl = if floating { "Dock panel" } else { "Float panel" };
                    if ui
                        .button(format!("{}  {}", ph::ARROWS_OUT, fl))
                        .clicked()
                    {
                        *act = Some(Act::ToggleFloat(gi));
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new("DOCK TO")
                            .size(9.5)
                            .color(ui_theme::TEXT_SECTION),
                    );
                    ui.horizontal(|ui| {
                        for (label, side) in [
                            ("Left", DockSide::Left),
                            ("Right", DockSide::Right),
                            ("Top", DockSide::Top),
                            ("Bottom", DockSide::Bottom),
                        ] {
                            if ui.selectable_label(g.side == side, label).clicked() {
                                *act = Some(Act::MoveToSide {
                                    group_id: g.id,
                                    side,
                                });
                            }
                        }
                    });
                    ui.separator();
                    if ui
                        .button(
                            egui::RichText::new(format!("{}  Close panel", ph::X))
                                .color(ui_theme::DESTRUCTIVE),
                        )
                        .clicked()
                    {
                        *act = Some(if floating {
                            Act::Remove(g.id)
                        } else {
                            Act::Close(gi)
                        });
                    }
                    if matches!(g.side, DockSide::Left | DockSide::Right) {
                        ui.separator();
                        let hide = if g.side == DockSide::Left {
                            "Hide left column"
                        } else {
                            "Hide right column"
                        };
                        if ui
                            .button(format!("{}  {}", ph::CARET_LEFT, hide))
                            .clicked()
                        {
                            *act = Some(Act::HideZone(g.side));
                        }
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new("ADD PANEL")
                            .size(9.5)
                            .color(ui_theme::TEXT_SECTION),
                    );
                    for k in PanelKind::all() {
                        if ui.button(format!("{}  {}", ph::PLUS, k.label())).clicked() {
                            *act = Some(Act::AddTab(gi, k));
                        }
                    }
                },
            );
            ui.add_space(4.0);

            // Measured tab overflow → ⌄ only when tabs don't fit. Active always visible.
            const PLUS_W: f32 = 22.0;
            const CARET_W: f32 = 22.0;
            const CARET_GAP: f32 = 4.0; // gap between ⌄ and +
            // Each tab box = text + 10px pad each side. Gaps between boxes = TAB_GAP only
            // (pads live INSIDE the exact allocation — no double-counted spacer).
            // Multi-tab groups always reserve ⋮ grip width so layout doesn't jump on hover.
            let multi = g.tabs.len() > 1;
            let dots_reserve = if multi { TAB_DOTS_W } else { 0.0 };
            let widths: Vec<f32> = (0..g.tabs.len()).map(|ti| {
                let sz = if ti == g.active { 12.5 } else { 11.0 };
                let fam = if ti == g.active { egui::FontFamily::Name("azra_semibold".into()) }
                          else { egui::FontFamily::Proportional };
                ui.fonts(|f| f.layout_no_wrap(g.tabs[ti].label().to_owned(),
                    egui::FontId::new(sz, fam), ui_theme::TEXT).size().x)
                    + TAB_PAD_X * 2.0
                    + dots_reserve
            }).collect();
            let full_w = ui.available_width();
            let sum: f32 = if widths.is_empty() { 0.0 } else {
                widths.iter().sum::<f32>() + TAB_GAP * (widths.len().saturating_sub(1) as f32)
            };
            let overflow = sum > (full_w - PLUS_W).max(0.0);
            let right_w = PLUS_W + if overflow { CARET_W + CARET_GAP } else { 0.0 };
            let tabs_avail = (full_w - right_w).max(0.0);
            let mut visible: Vec<usize> = Vec::new();
            let mut hidden: Vec<usize> = Vec::new();
            let mut used = 0.0;
            for ti in 0..g.tabs.len() {
                let need = widths[ti] + if visible.is_empty() { 0.0 } else { TAB_GAP };
                if !overflow || used + need <= tabs_avail {
                    visible.push(ti);
                    used += need;
                } else {
                    hidden.push(ti);
                }
            }
            if overflow && !visible.contains(&g.active) {
                if let Some(p) = hidden.iter().position(|&x| x == g.active) { hidden.remove(p); }
                if let Some(last) = visible.pop() { hidden.push(last); }
                visible.push(g.active);
                visible.sort_unstable();
            }
            // Tab drag reorder (mockup: dragged tab lifts, neighbours slide .12s).
            let dragging_this = tab_drag.map(|(id, _)| id == g.id);
            let drag_from = tab_drag.filter(|(id, _)| *id == g.id).map(|(_, i)| i);
            let mut hover_insert: Option<usize> = None;

            // Recompute used after forced-active swap so we never allocate past tabs_avail.
            let mut placed = 0.0f32;
            for (vi, &ti) in visible.iter().enumerate() {
                if vi > 0 {
                    ui.add_space(TAB_GAP);
                    placed += TAB_GAP;
                }
                let tw = widths[ti].min((tabs_avail - placed).max(0.0));
                placed += tw;
                let active = ti == g.active;
                let is_src = drag_from == Some(ti);
                let (box_r, resp) = ui.allocate_exact_size(
                    egui::vec2(tw, HEADER_H),
                    egui::Sense::click_and_drag(),
                );
                let box_r = egui::Rect::from_min_max(
                    egui::pos2(box_r.left(), strip_r.top()),
                    egui::pos2(box_r.right(), strip_r.bottom()),
                );
                // Neighbours slide when another tab is dragged over them.
                let slide = if dragging_this.is_some() && !is_src {
                    let t = ui.ctx().animate_bool_responsive(
                        egui::Id::new(("v2_tab_slide", g.id, ti)),
                        resp.hovered() || resp.dragged(),
                    );
                    t * 4.0 // subtle 4px shift
                } else {
                    0.0
                };
                let draw_r = box_r.translate(egui::vec2(slide, 0.0));
                let top_r = egui::Rounding {
                    nw: 5,
                    ne: 5,
                    sw: 0,
                    se: 0,
                };
                if is_src {
                    // Lifted tab (mockup .__drag opacity .92)
                    ui.painter().rect_filled(
                        draw_r,
                        top_r,
                        ui_theme::SURFACE_CONTROL.gamma_multiply(0.92),
                    );
                    ui.painter().rect_stroke(
                        draw_r,
                        top_r,
                        egui::Stroke::new(1.0, ui_theme::ACCENT), egui::StrokeKind::Middle,
                    );
                } else if active {
                    ui.painter()
                        .rect_filled(draw_r, top_r, ui_theme::SURFACE_PANEL);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(draw_r.left(), draw_r.bottom() - 2.0),
                            draw_r.max,
                        ),
                        0.0,
                        ui_theme::ACCENT,
                    );
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(draw_r, top_r, egui::Color32::from_white_alpha(8));
                }
                // Drop marker: accent rule before the hovered tab while dragging.
                if drag_from.is_some() && !is_src && resp.hovered() {
                    hover_insert = Some(ti);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(draw_r.left() - 1.5, draw_r.top() + 4.0),
                            egui::pos2(draw_r.left() + 1.5, draw_r.bottom() - 4.0),
                        ),
                        1.0,
                        ui_theme::ACCENT,
                    );
                }
                let col = if active {
                    ui_theme::ACCENT_LIGHT
                } else if resp.hovered() {
                    ui_theme::TEXT_SECONDARY
                } else {
                    ui_theme::TEXT_DIM
                };
                let sz = if active { 12.5 } else { 11.0 };
                let fam = if active {
                    egui::FontFamily::Name("azra_semibold".into())
                } else {
                    egui::FontFamily::Proportional
                };
                // Label sits to the right of ⋮ when multi-tab (dots on the left).
                let label_center = if multi {
                    egui::pos2(
                        draw_r.left() + TAB_DOTS_W + (draw_r.width() - TAB_DOTS_W) * 0.5,
                        draw_r.center().y,
                    )
                } else {
                    draw_r.center()
                };
                ui.painter().text(
                    label_center,
                    egui::Align2::CENTER_CENTER,
                    g.tabs[ti].label(),
                    egui::FontId::new(sz, fam),
                    col,
                );

                // ── Per-tab ⋮ grip (left of title — float this tab only) ─────
                let peeling = tab_float_drag.map(|(id, t)| id == g.id && t == ti).unwrap_or(false);
                if multi {
                    let dots_r = egui::Rect::from_min_max(
                        egui::pos2(draw_r.left() + 1.0, draw_r.top() + 3.0),
                        egui::pos2(draw_r.left() + TAB_DOTS_W + 1.0, draw_r.bottom() - 3.0),
                    );
                    let dots_id = ui.make_persistent_id(("v2_tab_dots", g.id, ti));
                    let dots = ui.interact(dots_r, dots_id, egui::Sense::click_and_drag());
                    let show_dots = dots.hovered()
                        || dots.dragged()
                        || resp.hovered()
                        || peeling
                        || active;
                    if show_dots {
                        if dots.hovered() || peeling {
                            ui.painter().rect_filled(
                                dots_r.shrink2(egui::vec2(1.0, 2.0)),
                                3.0,
                                ui_theme::SURFACE_CONTROL,
                            );
                        }
                        ui.painter().text(
                            dots_r.center(),
                            egui::Align2::CENTER_CENTER,
                            ph::DOTS_THREE_VERTICAL,
                            egui::FontId::proportional(12.0),
                            if dots.hovered() || peeling {
                                ui_theme::ACCENT_LIGHT
                            } else {
                                ui_theme::TEXT_FAINT
                            },
                        );
                    }
                    if dots.hovered() || dots.dragged() {
                        ui.output_mut(|o| {
                            o.cursor_icon = if dots.dragged() {
                                egui::CursorIcon::Grabbing
                            } else {
                                egui::CursorIcon::Grab
                            };
                        });
                    }
                    // Drag ⋮ → peel tab into float (not reorder).
                    if dots.drag_started() {
                        *tab_float_drag = Some((g.id, ti));
                        *tab_drag = None; // cancel in-group reorder
                    }
                    if dots.dragged() {
                        if let Some(p) = dots.interact_pointer_pos() {
                            *act = Some(Act::TabFloatDrag {
                                group_id: g.id,
                                ti,
                                pos: p,
                                delta: dots.drag_delta(),
                                released: false,
                            });
                        }
                    }
                    if dots.drag_stopped() {
                        if let Some(p) = dots
                            .interact_pointer_pos()
                            .or_else(|| ui.input(|i| i.pointer.latest_pos()))
                        {
                            *act = Some(Act::TabFloatDrag {
                                group_id: g.id,
                                ti,
                                pos: p,
                                delta: egui::Vec2::ZERO,
                                released: true,
                            });
                        }
                        *tab_float_drag = None;
                    }
                    // Click ⋮ → tab-only menu (float / close / close others).
                    let tab_menu_id = ui.make_persistent_id(("v2_tab_menu", g.id, ti));
                    if dots.clicked() && !dots.dragged() {
                        ui.memory_mut(|m| m.open_popup(tab_menu_id));
                    }
                    egui::containers::old_popup::popup_above_or_below_widget(
                        ui,
                        tab_menu_id,
                        &dots,
                        egui::AboveOrBelow::Below,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(160.0);
                            ui.label(
                                egui::RichText::new(g.tabs[ti].label())
                                    .size(10.0)
                                    .color(ui_theme::TEXT_DIM)
                                    .strong(),
                            );
                            ui.separator();
                            if ui
                                .button(format!("{}  Float tab", ph::ARROWS_OUT))
                                .clicked()
                            {
                                let p = dots.rect.center();
                                *act = Some(Act::FloatTab {
                                    group_id: g.id,
                                    ti,
                                    pos: p,
                                });
                                ui.close_menu();
                            }
                            if ui
                                .button(format!("{}  Close tab", ph::X))
                                .clicked()
                            {
                                *act = Some(Act::CloseTab { gi, ti });
                                ui.close_menu();
                            }
                            if g.tabs.len() > 1
                                && ui
                                    .button(format!("{}  Close other tabs", ph::TRASH))
                                    .clicked()
                            {
                                *act = Some(Act::CloseOtherTabs { gi, ti });
                                ui.close_menu();
                            }
                        },
                    );
                }

                // Label drag = reorder (only if not peeling via ⋮).
                let peeling_any = tab_float_drag.is_some();
                if resp.drag_started() && !peeling_any {
                    // Ignore if press started on the ⋮ zone (left of title)
                    let on_dots = multi
                        && resp
                            .interact_pointer_pos()
                            .map(|p| p.x <= draw_r.left() + TAB_DOTS_W + 1.0)
                            .unwrap_or(false);
                    if !on_dots {
                        *tab_drag = Some((g.id, ti));
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
                    }
                }
                if resp.dragged() && tab_drag.map(|(id, _)| id == g.id).unwrap_or(false) {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
                }
                if resp.drag_stopped() {
                    if let Some((id, from)) = *tab_drag {
                        if id == g.id {
                            let to = hover_insert.unwrap_or(from);
                            if to != from {
                                *act = Some(Act::ReorderTab { gi, from, to });
                            }
                        }
                    }
                    *tab_drag = None;
                }
                // Double-click tab → collapse body (same as strip).
                if resp.double_clicked() {
                    *act = Some(Act::Collapse(gi));
                } else if resp.clicked() && !resp.dragged() && !peeling {
                    *act = Some(Act::SetActive(gi, ti));
                }
            }
            // Clear drag if released outside.
            if ui.input(|i| i.pointer.any_released()) {
                if let Some((id, _)) = *tab_drag {
                    if id == g.id && hover_insert.is_none() {
                        // keep until drag_stopped on a tab; if pointer up with no target, clear
                        if !ui.input(|i| i.pointer.primary_down()) {
                            *tab_drag = None;
                        }
                    }
                }
            }
            let rem = ui.available_width();
            if rem > right_w { ui.add_space(rem - right_w); }
            if overflow {
                let (ov_r, ov) = ui.allocate_exact_size(egui::vec2(CARET_W, HEADER_H), egui::Sense::click());
                if ov.hovered() {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                    ui.painter().rect_filled(
                        ov_r.shrink2(egui::vec2(2.0, 5.0)), ui_theme::RADIUS_CONTROL, ui_theme::SURFACE_CONTROL);
                }
                ui.painter().text(ov_r.center(), egui::Align2::CENTER_CENTER, ph::CARET_DOWN,
                    egui::FontId::proportional(13.0),
                    if ov.hovered() { ui_theme::TEXT } else { ui_theme::TEXT_DIM });
                let ov_id = ui.make_persistent_id(("v2_ov", g.id));
                if ov.clicked() { ui.memory_mut(|m| m.toggle_popup(ov_id)); }
                egui::popup_below_widget(ui, ov_id, &ov, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(150.0);
                    for &ti in &hidden {
                        if ui.button(g.tabs[ti].label()).clicked() { *act = Some(Act::SetActive(gi, ti)); }
                    }
                });
                ui.add_space(CARET_GAP);
            }
            // The strip's `+` is a `.ptab` (11px), not an icon button — hence TEXT_LABEL @11.
            let (add_r, add) = ui.allocate_exact_size(egui::vec2(PLUS_W, HEADER_H), egui::Sense::click());
            if add.hovered() { ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand); }
            ui.painter().text(add_r.center(), egui::Align2::CENTER_CENTER, ph::PLUS,
                egui::FontId::proportional(11.0), ui_theme::TEXT_LABEL);
            let add_id = ui.make_persistent_id(("v2_add", g.id));
            if add.clicked() { ui.memory_mut(|m| m.toggle_popup(add_id)); }
            egui::popup_below_widget(ui, add_id, &add, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(150.0);
                ui.label(egui::RichText::new("ADD PANEL").size(9.5).color(ui_theme::TEXT_SECTION));
                for k in PanelKind::all() {
                    if ui.button(k.label()).clicked() { *act = Some(Act::AddTab(gi, k)); }
                }
            });
        });
    });
}

/// `border:2px dashed` — egui has a dashed *line* but no dashed rect, so corners square off.
pub(crate) fn dashed_rect(p: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let c = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    for w in c.windows(2) {
        p.extend(egui::Shape::dashed_line(&[w[0], w[1]], stroke, 5.0, 4.0));
    }
}

// Float 8-handle resize: use `chrome::draw_handles` only (INTEGRATION_RULES §6).

/// Fit dock pane heights into `avail` this frame.
///
/// `PanelGroup.height` is a *preferred basis* in px (`0` = flex/auto). Display heights
/// always sum to `avail - gaps` so resizing the app window never clips the bottom
/// pane (Tool Parameters / Properties) out of the column. Matches mockup flex column
/// behaviour: fixed basis where set, remainder shared, proportional scale when tight.
pub(crate) fn fit_dock_heights(
    dock: &[PanelGroup],
    docked: &[usize],
    avail: f32,
    gap: f32,
) -> Vec<f32> {
    let n = docked.len();
    if n == 0 {
        return Vec::new();
    }
    let gaps = gap * n.saturating_sub(1) as f32;
    let target = (avail - gaps).max(0.0);
    let mut heights = vec![0.0_f32; n];
    let mut open: Vec<usize> = Vec::new();
    let mut collapsed_sum = 0.0_f32;

    for (pos, &gi) in docked.iter().enumerate() {
        let g = &dock[gi];
        let head = if g.head_coll { 6.0 } else { HEADER_H };
        if g.collapsed {
            heights[pos] = head;
            collapsed_sum += head;
        } else {
            open.push(pos);
            // Preferred basis; 0 means flex.
            heights[pos] = if g.height > 0.0 {
                g.height.max(head + PANE_MIN_H)
            } else {
                0.0
            };
        }
    }

    let open_budget = (target - collapsed_sum).max(0.0);
    if open.is_empty() {
        return heights;
    }

    let min_open = |pos: usize| -> f32 {
        let g = &dock[docked[pos]];
        let head = if g.head_coll { 6.0 } else { HEADER_H };
        head + PANE_MIN_H
    };

    let mut autos: Vec<usize> = Vec::new();
    let mut fixed_sum = 0.0_f32;
    for &pos in &open {
        if heights[pos] <= 0.0 {
            autos.push(pos);
        } else {
            fixed_sum += heights[pos];
        }
    }

    if autos.is_empty() {
        // All open panes have a basis — scale proportionally into open_budget.
        let sum: f32 = open.iter().map(|&p| heights[p]).sum();
        if sum > 0.5 {
            let scale = open_budget / sum;
            for &p in &open {
                heights[p] = (heights[p] * scale).max(min_open(p));
            }
        } else {
            let share = open_budget / open.len() as f32;
            for &p in &open {
                heights[p] = share.max(min_open(p));
            }
        }
    } else {
        // Fixed panes keep basis (scaled down if they leave no room for autos).
        let auto_min_total: f32 = autos.iter().map(|&p| min_open(p)).sum();
        let max_fixed = (open_budget - auto_min_total).max(0.0);
        if fixed_sum > max_fixed && fixed_sum > 0.5 {
            let scale = max_fixed / fixed_sum;
            for &p in &open {
                if !autos.contains(&p) {
                    heights[p] = (heights[p] * scale).max(min_open(p));
                }
            }
            fixed_sum = open
                .iter()
                .filter(|p| !autos.contains(p))
                .map(|&p| heights[p])
                .sum();
        }
        let rem = (open_budget - fixed_sum).max(0.0);
        let share = rem / autos.len() as f32;
        for &p in &autos {
            heights[p] = share.max(min_open(p));
        }
    }

    // Pixel remainder → last open pane so the column always fills (no dead gap / clip).
    let used: f32 = heights.iter().sum();
    if let Some(&last) = open.last() {
        let delta = open_budget + collapsed_sum - used;
        // Only adjust open budget remainder onto last open pane.
        let open_used: f32 = open.iter().map(|&p| heights[p]).sum();
        let open_delta = open_budget - open_used;
        heights[last] = (heights[last] + open_delta).max(min_open(last));
        let _ = delta;
    }

    heights
}

/// Dispatch pane body content into a Ui that already has the correct max size
/// (mockup `.pane-body{flex:1;min-height:0;overflow:auto}`).
pub(crate) fn fill_pane_body(app: &mut FourthLayerApp, ui: &mut egui::Ui, g: &PanelGroup) {
    // Pin width/height so children reflow when the dock or splitter moves.
    let sz = ui.available_size();
    ui.set_min_size(sz);
    ui.set_max_size(sz);
    match g.active_kind() {
        Some(PanelKind::Outliner) => app.ui2_outliner(ui),
        Some(PanelKind::ToolParams) => app.ui2_tool_params_pane(ui),
        Some(PanelKind::Properties) => app.ui2_draw_properties(ui),
        Some(PanelKind::History) => app.ui2_draw_history(ui),
        Some(PanelKind::Image) => app.ui2_draw_image_panel(ui),
        Some(PanelKind::Channels) => app.ui2_draw_channels(ui),
        Some(PanelKind::NodeEditor) => app.draw_node_editor_panel(ui),
        Some(PanelKind::Library) => crate::library_panel::draw_in_pane(
            ui,
            &mut app.ui2.library,
            &mut app.library,
        ),
        // Viewport shading-bar settings panels. Tall pages (PBR / PT) scroll.
        Some(k @ (PanelKind::WireSettings
        | PanelKind::SolidSettings
        | PanelKind::MaterialSettings
        | PanelKind::PbrSettings
        | PanelKind::PtSettings
        | PanelKind::MatCapSettings
        | PanelKind::EnvSettings
        | PanelKind::RenderSettingsGlobal)) => {
            let h = ui.available_height();
            egui::ScrollArea::vertical()
                .id_salt(("v2_settings_scroll", k.label()))
                .auto_shrink([false, false])
                .max_height(h)
                .show(ui, |ui| {
                    let iw = ui.available_width();
                    ui.set_min_width(iw);
                    ui.set_max_width(iw);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.add_space(2.0);
                    match k {
                        PanelKind::WireSettings => app.ui2_draw_wire_settings(ui),
                        PanelKind::SolidSettings => app.ui2_draw_solid_settings(ui),
                        PanelKind::MaterialSettings => app.ui2_draw_material_settings(ui),
                        PanelKind::PbrSettings => app.ui2_draw_pbr_settings(ui),
                        PanelKind::PtSettings => app.ui2_draw_pt_settings(ui),
                        PanelKind::MatCapSettings => app.ui2_draw_matcap_settings(ui),
                        PanelKind::EnvSettings => app.ui2_draw_env_settings(ui),
                        PanelKind::RenderSettingsGlobal => app.ui2_draw_render_settings_global(ui),
                        _ => unreachable!(),
                    }
                    ui.add_space(6.0);
                });
        }
        Some(k) => match k.tool_pillar() {
            Some(p) => app.ui2_tools_pillar(ui, p),
            None => group_body(ui, g),
        },
        None => group_body(ui, g),
    }
}

/// Gate 3/4: Tool Parameters — live-bound when a prim is selected (Gate 2 catalog +
/// scene writeback). Palette still writes `ui2.active_tool` / `active_pillar` for the
/// no-selection create defaults path.
pub(crate) const ACTIVE_TOOL_ID: &str = "ui2_active_tool";
pub(crate) const ACTIVE_PILLAR_ID: &str = "ui2_active_pillar";

impl FourthLayerApp {
    pub(crate) fn ui2_tool_params_pane(&mut self, ui: &mut egui::Ui) {
        // Keep temp ids in sync for anything still reading them (token board, etc.).
        ui.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(ACTIVE_TOOL_ID),
                self.ui2.active_tool.clone(),
            );
            d.insert_temp(
                egui::Id::new(ACTIVE_PILLAR_ID),
                self.ui2.active_pillar.clone(),
            );
        });
        let h = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("v2_tool_params_scroll")
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                let iw = ui.available_width();
                ui.set_min_width(iw);
                ui.set_max_width(iw);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(2.0);
                // Side gutter: sliders / labels must not touch the pane edge.
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 0))
                    .show(ui, |ui| self.ui2_draw_tool_params(ui));
                ui.add_space(4.0);
            });
    }
}

/// Body of a group = the active tab's content (placeholder for now).
pub(crate) fn group_body(ui: &mut egui::Ui, g: &PanelGroup) {
    let r = ui.max_rect();
    let txt = match g.active_kind() {
        Some(k) => format!("{} — content coming", k.label()),
        None => "empty group · use  +  to add a panel".to_string(),
    };
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, txt,
        egui::FontId::proportional(12.0), ui_theme::TEXT_FAINT);
}
