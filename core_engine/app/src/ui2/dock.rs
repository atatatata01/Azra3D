// Side docks + floating.
// L/R resize goes through `chrome::{draw_resize_seam, draw_restore_strip, ZoneState}`
// — no parallel edge hit loops (see INTEGRATION_RULES.md).
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;
use super::chrome::{
    draw_handles, draw_resize_seam, draw_restore_strip, ZoneEdge, ZoneState, SEAM_HIT,
};

/// Viewport drag inset = chrome seam hit (keep export name for app_update).
pub(crate) const DOCK_EDGE_HIT: f32 = SEAM_HIT;

impl FourthLayerApp {
    pub(crate) fn ui2_dock(&mut self, ctx: &egui::Context, side: DockSide) -> Option<Act> {
        let mut act: Option<Act> = None;
        let is_left = side == DockSide::Left;
        let zone_hidden = match side {
            DockSide::Left => self.ui2.left_hidden,
            DockSide::Right => self.ui2.right_hidden,
            DockSide::Top | DockSide::Bottom => unreachable!("horizontal dock uses ui2_horizontal_dock"),
            DockSide::Float => false,
        };

        // ── Width (Ui2State, pixel-snapped) ─────────────────────────────────
        let (min_w, max_w) = if is_left {
            (ui_theme::RAIL_MIN, ui_theme::RAIL_MAX)
        } else {
            (ui_theme::DOCK_MIN, ui_theme::DOCK_MAX)
        };
        let ppp = ctx.pixels_per_point();
        let raw = if is_left {
            self.ui2.left_width
        } else {
            self.ui2.right_width
        };
        let width = if zone_hidden {
            6.0
        } else {
            ((raw.clamp(min_w, max_w) * ppp).round() / ppp).clamp(min_w, max_w)
        };
        if !zone_hidden {
            if is_left {
                self.ui2.left_width = width;
            } else {
                self.ui2.right_width = width;
            }
        }

        // ── SidePanel = RESERVE WIDTH ONLY (no interactive widgets) ─────────
        // Content is painted once later on Foreground (after library) so we never
        // double-register DragValue / CollapsingHeader IDs (owner: "Double use of
        // widget ID" banners on Tool Parameters).
        let panel_id = if is_left { "v2_dock_left" } else { "v2_dock_right" };
        let panel = if is_left {
            egui::SidePanel::left(panel_id)
        } else {
            egui::SidePanel::right(panel_id)
        }
        .resizable(false)
        .exact_width(width)
        .width_range(width..=width)
        .show_separator_line(false)
        .frame(egui::Frame::none().fill(ui_theme::SURFACE_PANEL));

        let r = panel.show(ctx, |ui| {
            let h = ui.available_height().max(1.0);
            let s = egui::vec2(width, h);
            ui.set_min_size(s);
            ui.set_max_size(s);
            ui.set_clip_rect(ui.max_rect());
            let _ = ui.allocate_exact_size(s, egui::Sense::hover());
        });
        let prect = r.response.rect;

        if is_left {
            self.ui2.left_panel_rect = prect;
        } else {
            self.ui2.right_panel_rect = prect;
        }

        // Chrome seam / restore (single path for L/R zone geometry).
        self.ui2_dock_chrome_seam(ctx, prect, is_left, min_w, max_w);
        act
    }

    /// Sync Ui2State width/hide ↔ `ZoneState`, then drive chrome seam or restore strip.
    fn ui2_dock_chrome_seam(
        &mut self,
        ctx: &egui::Context,
        prect: egui::Rect,
        is_left: bool,
        min_w: f32,
        max_w: f32,
    ) {
        let edge = if is_left {
            ZoneEdge::Left
        } else {
            ZoneEdge::Right
        };
        let mut zone = ZoneState {
            open: if is_left {
                !self.ui2.left_hidden
            } else {
                !self.ui2.right_hidden
            },
            size: if is_left {
                self.ui2.left_width
            } else {
                self.ui2.right_width
            },
            min: min_w,
            max: max_w,
            hide_slop: if is_left {
                self.ui2.left_hide_slop
            } else {
                self.ui2.right_hide_slop
            },
            edge,
        };
        if zone.open {
            let _ = draw_resize_seam(ctx, prect, &mut zone, ("dock", is_left));
        } else {
            let _ = draw_restore_strip(ctx, prect, &mut zone, ("dock", is_left));
        }
        if is_left {
            self.ui2.left_hidden = !zone.open;
            self.ui2.left_width = zone.size;
            self.ui2.left_hide_slop = zone.hide_slop;
        } else {
            self.ui2.right_hidden = !zone.open;
            self.ui2.right_width = zone.size;
            self.ui2.right_hide_slop = zone.hide_slop;
        }
        // Viewport must not steal camera/gizmo while a seam drag is active.
        let drag_id = egui::Id::new(("ui2_seam_drag", ("dock", is_left)));
        let dragging = ctx.data(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));
        if dragging {
            self.ui2.edge_drag = Some(is_left);
        } else if self.ui2.edge_drag == Some(is_left) {
            self.ui2.edge_drag = None;
        }
    }

    /// Sole interactive paint of a dock column (Foreground, after library).
    /// SidePanel only reserved width — this is the one place widgets are created.
    pub(crate) fn ui2_repaint_dock_over_library(
        &mut self,
        ctx: &egui::Context,
        side: DockSide,
    ) {
        let is_left = side == DockSide::Left;
        let zone_hidden = if is_left {
            self.ui2.left_hidden
        } else {
            self.ui2.right_hidden
        };
        if zone_hidden {
            return;
        }
        let prect = if is_left {
            self.ui2.left_panel_rect
        } else {
            self.ui2.right_panel_rect
        };
        if prect.width() < 8.0 || prect.height() < 8.0 {
            return;
        }

        // Live widgets only when the pointer is over this dock (or a modal/drag forces it).
        // Per-card: hover Outliner must not rebuild Properties/Material on the sibling card
        // (that was the residual multi-ms when "UI is open" but only one pane is used).
        let force_live = self.ui2_dock_force_live(ctx) || side == DockSide::Float;
        let column_live = force_live || self.ui2_dock_wants_live(ctx, side, prect);
        let ptr = ctx.pointer_hover_pos().or_else(|| ctx.input(|i| i.pointer.interact_pos()));

        let mut act: Option<Act> = None;
        let (pad_l, pad_r, pad_y) = if is_left {
            (6.0, 0.0, 6.0)
        } else {
            (0.0, 6.0, 6.0)
        };
        let pane_w = (prect.width() - pad_l - pad_r).max(1.0);
        let pane_h = (prect.height() - 2.0 * pad_y).max(1.0);
        let inner = egui::Rect::from_min_size(
            egui::pos2(prect.left() + pad_l, prect.top() + pad_y),
            egui::vec2(pane_w, pane_h),
        );

        egui::Area::new(egui::Id::new(("v2_dock_top", is_left)))
            .order(egui::Order::Foreground)
            .fixed_pos(prect.min)
            .movable(false)
            .constrain(false)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_min_size(prect.size());
                ui.set_max_size(prect.size());
                ui.set_clip_rect(prect);
                let _ = ui.allocate_exact_size(prect.size(), egui::Sense::hover());
                ui.painter()
                    .rect_filled(prect, 0.0, ui_theme::SURFACE_PANEL);

                ui.allocate_ui_at_rect(inner, |ui| {
                    ui.set_min_size(inner.size());
                    ui.set_max_size(inner.size());
                    ui.set_clip_rect(inner);

                    let docked: Vec<usize> = (0..self.ui2.dock.len())
                        .filter(|&i| self.ui2.dock[i].side == side)
                        .collect();
                    if docked.is_empty() {
                        return;
                    }
                    let gap = 8.0;
                    let card_heights =
                        fit_dock_heights(&self.ui2.dock, &docked, pane_h, gap);
                    let mut tab_drag = self.ui2.tab_drag;
                    let mut tab_float_drag = self.ui2.tab_float_drag;

                    for (pos, &i) in docked.iter().enumerate() {
                        let card_h = card_heights[pos];
                        let g = self.ui2.dock[i].clone();
                        let head_h = if g.head_coll { 6.0 } else { HEADER_H };
                        let (card_rect, _) = ui.allocate_exact_size(
                            egui::vec2(pane_w, card_h),
                            egui::Sense::hover(),
                        );
                        // Per-card live: only the card under the pointer (or force) builds widgets.
                        // Hovering Outliner must not rebuild Properties/Material on the sibling card.
                        let card_live = force_live
                            || (column_live
                                && ptr.map(|p| card_rect.expand(6.0).contains(p)).unwrap_or(false));
                        ui.allocate_ui_at_rect(card_rect, |ui| {
                            ui.set_min_size(card_rect.size());
                            ui.set_max_size(card_rect.size());
                            ui.set_clip_rect(card_rect);
                            ui.painter().rect_filled(
                                card_rect,
                                ui_theme::RADIUS_PANEL,
                                ui_theme::SURFACE_PANEL,
                            );
                            ui.painter().rect_stroke(
                                card_rect,
                                ui_theme::RADIUS_PANEL,
                                egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle,
                            );
                            let mut y = card_rect.top();
                            if g.head_coll {
                                let hr = egui::Rect::from_min_size(
                                    card_rect.min,
                                    egui::vec2(pane_w, 6.0),
                                );
                                if card_live {
                                    let hresp = ui.interact(
                                        hr,
                                        ui.id().with(("headcoll", g.id)),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().rect_filled(
                                        hr,
                                        egui::Rounding {
                                            nw: 7,
                                            ne: 7,
                                            sw: 0,
                                            se: 0,
                                        },
                                        if hresp.hovered() {
                                            ui_theme::ACCENT
                                        } else {
                                            ui_theme::SURFACE_PANEL
                                        },
                                    );
                                    if hresp.clicked() {
                                        act = Some(Act::HeadCollapse(i));
                                    }
                                } else {
                                    ui.painter().rect_filled(hr, 0.0, ui_theme::SURFACE_PANEL);
                                }
                                y = hr.bottom();
                            } else {
                                let head_r = egui::Rect::from_min_size(
                                    card_rect.min,
                                    egui::vec2(pane_w, head_h),
                                );
                                ui.allocate_ui_at_rect(head_r, |ui| {
                                    ui.set_min_size(head_r.size());
                                    ui.set_max_size(head_r.size());
                                    // Always live — see the chrome note in draw/mod.rs. A tab
                                    // strip is a handful of widgets, and it is the one part of a
                                    // panel that must stay clickable and legible at a glance.
                                    group_tab_strip(
                                        ui, i, &g, false, &mut act, &mut tab_drag, &mut tab_float_drag,
                                    );
                                });
                                y = head_r.bottom();
                            }
                            if !g.collapsed {
                                let body_r = egui::Rect::from_min_max(
                                    egui::pos2(card_rect.left(), y),
                                    card_rect.right_bottom(),
                                );
                                if body_r.height() > 1.0 {
                                    ui.allocate_ui_at_rect(body_r, |ui| {
                                        ui.set_min_size(body_r.size());
                                        ui.set_max_size(body_r.size());
                                        ui.set_clip_rect(body_r);
                                        // LIVE captures the paint; IDLE replays it. A panel
                                        // with no usable snapshot (first frame, or the content
                                        // epoch moved while it was idle) draws LIVE rather than
                                        // showing a stub — owner 2026-08-05, panels must never
                                        // be blank just because the pointer is elsewhere.
                                        let slot = g.active_kind().map(|k| {
                                            crate::ui2::idle::slot::pane_body(g.id, k)
                                        });
                                        let bctx = ui.ctx().clone();
                                        let replayed = !card_live
                                            && slot.is_some_and(|s| {
                                                crate::ui2::idle::replay_idle(
                                                    self, &bctx, s, body_r,
                                                )
                                            });
                                        if !replayed {
                                            crate::ui2::chrome::apply_panel_scroll_style(ui);
                                            match slot {
                                                Some(s) => crate::ui2::idle::capture_idle(
                                                    self, &bctx, s, body_r,
                                                    |app| fill_pane_body(app, ui, &g),
                                                ),
                                                None => fill_pane_body(self, ui, &g),
                                            }
                                        }
                                    });
                                }
                            }
                        });

                        let rects = if is_left {
                            &mut self.ui2.left_rects
                        } else {
                            &mut self.ui2.right_rects
                        };
                        rects.push((g.id, card_rect));

                        let neighbour_open = pos + 1 < docked.len()
                            && !self.ui2.dock[i].collapsed
                            && !self.ui2.dock[docked[pos + 1]].collapsed;
                        if neighbour_open {
                            // Splitters only need widgets when the column is in play.
                            if column_live || force_live {
                                let (srect, r) = ui.allocate_exact_size(
                                    egui::vec2(pane_w, gap),
                                    egui::Sense::drag(),
                                );
                                if r.hovered() || r.dragged() {
                                    ui.painter().rect_filled(
                                        srect.shrink2(egui::vec2(24.0, 1.0)),
                                        4.0,
                                        ui_theme::ACCENT,
                                    );
                                    ui.output_mut(|o| {
                                        o.cursor_icon = egui::CursorIcon::ResizeVertical
                                    });
                                }
                                if r.drag_started() {
                                    self.ui2.split_shift = ui.input(|z| z.modifiers.shift);
                                }
                                if r.dragged() && !self.ui2.split_shift {
                                    let next = docked[pos + 1];
                                    let cur = card_heights[pos];
                                    let nxt = card_heights[pos + 1];
                                    let pair =
                                        (cur + nxt).max(PANE_MIN_H * 2.0 + HEADER_H * 2.0);
                                    let new_cur = (cur + r.drag_delta().y).clamp(
                                        HEADER_H + PANE_MIN_H,
                                        pair - (HEADER_H + PANE_MIN_H),
                                    );
                                    self.ui2.dock[i].height = new_cur;
                                    self.ui2.dock[next].height = pair - new_cur;
                                }
                                if r.drag_stopped() && self.ui2.split_shift {
                                    act = Some(Act::InsertGroupAfter(i));
                                    self.ui2.split_shift = false;
                                }
                            } else {
                                let _ = ui.allocate_exact_size(
                                    egui::vec2(pane_w, gap),
                                    egui::Sense::hover(),
                                );
                            }
                        }
                    }
                    if column_live || force_live {
                        self.ui2.tab_drag = tab_drag;
                        self.ui2.tab_float_drag = tab_float_drag;
                    }
                });
            });

        // Apply any tab actions from the top layer
        self.ui2_handle(ctx, act);
        // Keep edge resize live on top of the Foreground dock paint
        let (min_w, max_w) = if is_left {
            (ui_theme::RAIL_MIN, ui_theme::RAIL_MAX)
        } else {
            (ui_theme::DOCK_MIN, ui_theme::DOCK_MAX)
        };
        self.ui2_dock_chrome_seam(ctx, prect, is_left, min_w, max_w);
    }

    pub(crate) fn ui2_floating(&mut self, ctx: &egui::Context) -> Option<Act> {
        let mut act: Option<Act> = None;
        let mut tab_drag = self.ui2.tab_drag;
        let mut tab_float_drag = self.ui2.tab_float_drag;
        let floating: Vec<(u64, PanelGroup)> = self.ui2.dock.iter()
            .filter(|g| g.side == DockSide::Float).map(|g| (g.id, g.clone())).collect();
        for (id, g) in floating {
            let gi = self.ui2.dock.iter().position(|x| x.id == id).unwrap();
            // Collapsed float = header only (same as docked cards). Keep full g.size as the
            // restore size when expanded; visual height shrinks to the tab strip.
            let head_h = if g.head_coll { 6.0 } else { HEADER_H };
            let vis_size = if g.collapsed {
                egui::vec2(g.size.x, head_h)
            } else {
                g.size
            };
            egui::Area::new(egui::Id::new(("v2_floatarea", id)))
                .order(egui::Order::Middle).fixed_pos(g.pos)
                .show(ctx, |ui| {
                    egui::Frame::none().fill(ui_theme::SURFACE_PANEL)
                        .rounding(egui::Rounding::same(ui_theme::RADIUS_PANEL as u8))
                        .stroke(egui::Stroke::new(1.0, ui_theme::BORDER_HOVER))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 22], blur: 60, spread: 0,
                            color: egui::Color32::from_black_alpha(166), // rgba(0,0,0,.65)
                        })
                        .show(ui, |ui| {
                            ui.set_min_size(vis_size); ui.set_max_size(vis_size);
                            group_tab_strip(ui, gi, &g, true, &mut act, &mut tab_drag, &mut tab_float_drag);
                            if !g.collapsed {
                                let body_h = (g.size.y - HEADER_H).max(0.0);
                                ui.allocate_ui(egui::vec2(g.size.x, body_h), |ui| {
                                    ui.set_min_size(egui::vec2(g.size.x, body_h));
                                    ui.set_max_size(egui::vec2(g.size.x, body_h));
                                    fill_pane_body(self, ui, &g);
                                });
                            }
                        });
                    // Resize handles only when expanded — collapsed float is header-only.
                    if !g.collapsed {
                        if let Some(d) = draw_handles(ui, id, g.pos, g.size) {
                            act = Some(Act::ResizeFloat(id, d.pos, d.size));
                        }
                    }
                });
        }
        self.ui2.tab_drag = tab_drag;
        self.ui2.tab_float_drag = tab_float_drag;
        act
    }

    /// Which dock (if any) a screen point is over, and the insertion slot + line geometry.
    ///
    /// Owner 2026-07-30: snap must **not** register against a hidden/collapsed column.
    /// Previously used stored `left_width`/`right_width` (preferred open width), so a
    /// hidden 6px restore strip still claimed a full rail of drop targets — dragging a
    /// floating panel near the edge lit the accent bar and re-docked on release.
    pub(crate) fn ui2_drop_target(&self, ctx: &egui::Context, p: egui::Pos2) -> Option<(DockSide, usize, f32, f32, f32)> {
        let sr = ctx.screen_rect();

        // Zone hide (mockup `__lhide` / `__rhide`) — no snap while the column is collapsed away.
        // Also require a real measured rect (not the 6px restore strip).
        let left_rect = self.ui2.left_panel_rect;
        let right_rect = self.ui2.right_panel_rect;
        let hole = self.ui2.viewport_hole;
        let top_rect = if self.ui2.top_panel_rect.height() > 12.0 {
            self.ui2.top_panel_rect
        } else {
            egui::Rect::from_min_max(
                hole.left_top(),
                egui::pos2(hole.right(), (hole.top() + 48.0).min(hole.bottom())),
            )
        };
        let bottom_rect = if self.ui2.bottom_panel_rect.height() > 12.0 {
            self.ui2.bottom_panel_rect
        } else {
            egui::Rect::from_min_max(
                egui::pos2(hole.left(), (hole.bottom() - 48.0).max(hole.top())),
                hole.right_bottom(),
            )
        };
        let left_open = !self.ui2.left_hidden && left_rect.width() > 12.0;
        let right_open = !self.ui2.right_hidden && right_rect.width() > 12.0;
        let top_open = top_rect.height() > 12.0;
        let bottom_open = bottom_rect.height() > 12.0;

        // Hit-test the *drawn* panel rect this frame — never the preferred open width.
        let (side, rects, x0, x1) = if left_open
            && p.x >= left_rect.left()
            && p.x <= left_rect.right()
            && p.y >= left_rect.top()
            && p.y <= left_rect.bottom()
        {
            (
                DockSide::Left,
                &self.ui2.left_rects,
                left_rect.left() + 8.0,
                left_rect.right() - 8.0,
            )
        } else if right_open
            && p.x >= right_rect.left()
            && p.x <= right_rect.right()
            && p.y >= right_rect.top()
            && p.y <= right_rect.bottom()
        {
            (
                DockSide::Right,
                &self.ui2.right_rects,
                right_rect.left() + 8.0,
                right_rect.right() - 8.0,
            )
        } else if top_open && top_rect.contains(p) {
            (
                DockSide::Top,
                &self.ui2.top_rects,
                top_rect.left() + 8.0,
                top_rect.right() - 8.0,
            )
        } else if bottom_open && bottom_rect.contains(p) {
            (
                DockSide::Bottom,
                &self.ui2.bottom_rects,
                bottom_rect.left() + 8.0,
                bottom_rect.right() - 8.0,
            )
        } else {
            return None;
        };

        // If every group on this side is body-collapsed, do not offer a snap line
        // (user has collapsed the column content — treat like no dock target).
        let side_groups: Vec<&PanelGroup> = self.ui2.dock.iter().filter(|g| g.side == side).collect();
        if !side_groups.is_empty() && side_groups.iter().all(|g| g.collapsed) {
            return None;
        }

        // Slot index among *all* same-side groups (collapsed included) so
        // `ui2_move_to_slot` / tab peel insert stay index-consistent.
        // Snap *line* prefers the nearest non-collapsed card edge when possible.
        let mut idx = 0;
        for (_, r) in rects {
            if p.y > r.center().y {
                idx += 1;
            }
        }

        // Prefer drawing the accent bar on an expanded card boundary.
        let line_y = {
            let open: Vec<&egui::Rect> = rects
                .iter()
                .filter(|(id, _)| {
                    self.ui2
                        .dock
                        .iter()
                        .find(|g| g.id == *id)
                        .map(|g| !g.collapsed)
                        .unwrap_or(false)
                })
                .map(|(_, r)| r)
                .collect();
            if open.is_empty() {
                if rects.is_empty() {
                    match side {
                        DockSide::Top => top_rect.top() + 2.0,
                        DockSide::Bottom => bottom_rect.bottom() - 2.0,
                        _ => sr.top() + ui_theme::H_APPBAR + 8.0,
                    }
                } else if idx == 0 {
                    rects[0].1.top()
                } else {
                    rects[idx - 1].1.bottom()
                }
            } else {
                let mut oidx = 0;
                for r in &open {
                    if p.y > r.center().y {
                        oidx += 1;
                    }
                }
                if oidx == 0 {
                    open[0].top()
                } else {
                    open[oidx - 1].bottom()
                }
            }
        };
        Some((side, idx, line_y, x0, x1))
    }

    pub(crate) fn ui2_move_to_slot(&mut self, id: u64, side: DockSide, slot: usize) {
        let Some(pi) = self.ui2.dock.iter().position(|g| g.id == id) else { return };
        let mut g = self.ui2.dock.remove(pi);
        g.side = side;
        let ids: Vec<u64> = self.ui2.dock.iter().filter(|x| x.side == side).map(|x| x.id).collect();
        let insert_pos = match ids.get(slot) {
            Some(bid) => self.ui2.dock.iter().position(|x| x.id == *bid).unwrap(),
            None => self.ui2.dock.len(),
        };
        self.ui2.dock.insert(insert_pos, g);
    }

    /// View-menu visibility: any of `kinds` present as a tab in a dock group whose
    /// host zone is shown (floating groups always count). Live Ui2State — NOT the
    /// v1 `layout.show_*` flags (those belong to the v1 shell only).
    pub(crate) fn ui2_panel_visible(&self, kinds: &[PanelKind]) -> bool {
        self.ui2.dock.iter().any(|g| {
            g.tabs.iter().any(|t| kinds.contains(t))
                && match g.side {
                    DockSide::Left => !self.ui2.left_hidden,
                    DockSide::Right => !self.ui2.right_hidden,
                    DockSide::Top | DockSide::Bottom => true,
                    DockSide::Float => true,
                }
        })
    }

    /// View-menu toggle for a dock panel.
    /// Hide = remove the tabs (same removal path as `Act::CloseTab`: drop tabs,
    /// then drop empty groups). Show = if the tabs still exist somewhere,
    /// un-collapse their group and un-hide its zone (same state the restore strip
    /// writes); else re-create a group with `kinds` on `default_side`.
    /// Zone widths are untouched, so reopening restores the previous width.
    pub(crate) fn ui2_set_panel_visible(
        &mut self,
        kinds: &[PanelKind],
        default_side: DockSide,
        show: bool,
    ) {
        if show {
            let mut found = false;
            for g in &mut self.ui2.dock {
                if g.tabs.iter().any(|t| kinds.contains(t)) {
                    found = true;
                    g.collapsed = false;
                    g.head_coll = false;
                    match g.side {
                        DockSide::Left => self.ui2.left_hidden = false,
                        DockSide::Right => self.ui2.right_hidden = false,
                        DockSide::Top | DockSide::Bottom => {}
                        DockSide::Float => {}
                    }
                }
            }
            if !found {
                let g = self.ui2.mk(kinds.to_vec(), default_side);
                self.ui2.dock.push(g);
                match default_side {
                    DockSide::Left => self.ui2.left_hidden = false,
                    DockSide::Right => self.ui2.right_hidden = false,
                    DockSide::Top | DockSide::Bottom => {}
                    DockSide::Float => {}
                }
            }
        } else {
            for g in &mut self.ui2.dock {
                let was_active = g.active_kind();
                g.tabs.retain(|t| !kinds.contains(t));
                // Keep the same tab active when it survives; else clamp to 0.
                g.active = was_active
                    .and_then(|k| g.tabs.iter().position(|t| *t == k))
                    .unwrap_or(0);
            }
            self.ui2.dock.retain(|g| !g.tabs.is_empty());
        }
    }

    pub(crate) fn ui2_handle(&mut self, ctx: &egui::Context, act: Option<Act>) {
        match act {
            Some(Act::Collapse(i)) => {
                if let Some(g) = self.ui2.dock.get_mut(i) {
                    g.collapsed = !g.collapsed;
                    // Expanding body keeps head visible.
                    if !g.collapsed {
                        g.head_coll = false;
                    }
                }
            }
            Some(Act::HeadCollapse(i)) => {
                if let Some(g) = self.ui2.dock.get_mut(i) {
                    g.head_coll = !g.head_coll;
                }
            }
            Some(Act::Close(i)) => {
                if i < self.ui2.dock.len() {
                    self.ui2.dock.remove(i);
                }
            }
            Some(Act::InsertGroupAfter(i)) => {
                // Stale index (group closed/reordered same frame) → no-op like every other arm.
                if let Some(g) = self.ui2.dock.get(i) {
                    let side = g.side;
                    let gnew = self.ui2.mk(Vec::new(), side);
                    self.ui2.dock.insert((i + 1).min(self.ui2.dock.len()), gnew);
                }
            }
            Some(Act::AddTab(i, k)) => {
                if let Some(g) = self.ui2.dock.get_mut(i) {
                    g.tabs.push(k);
                    g.active = g.tabs.len() - 1;
                }
            }
            Some(Act::SetActive(i, t)) => {
                if let Some(g) = self.ui2.dock.get_mut(i) {
                    g.active = t;
                    // Gate 4: chrome tab on the Tools group switches the pillar that
                    // Tool Parameters (and the active-tool bus) read.
                    if let Some(p) = g.active_kind().and_then(|k| k.tool_pillar()) {
                        self.ui2.active_pillar = match p {
                            ToolPillar::Sdf => "sdf",
                            ToolPillar::Poly => "poly",
                            ToolPillar::Sculpt => "sculpt",
                            ToolPillar::Nurbs => "nurbs",
                        }
                        .into();
                        self.ui2.tool_cat = 0;
                    }
                }
            }
            Some(Act::ReorderTab { gi, from, to }) => {
                if let Some(g) = self.ui2.dock.get_mut(gi) {
                    if from < g.tabs.len() && to <= g.tabs.len() && from != to {
                        let tab = g.tabs.remove(from);
                        let insert = if to > from { to - 1 } else { to }.min(g.tabs.len());
                        g.tabs.insert(insert, tab);
                        // Keep the same kind active after reorder.
                        g.active = insert.min(g.tabs.len().saturating_sub(1));
                    }
                }
                self.ui2.tab_drag = None;
            }
            Some(Act::ToggleFloat(i)) => {
                if let Some(g) = self.ui2.dock.get_mut(i) {
                    if g.side == DockSide::Float {
                        g.side = DockSide::Right; // re-dock to right by default
                    } else {
                        g.side = DockSide::Float;
                        g.pos = egui::pos2(280.0, 120.0);
                        g.size = egui::vec2(320.0, 400.0);
                    }
                }
            }
            Some(Act::MoveToSide { group_id, side }) => {
                if let Some(group) = self.ui2.dock.iter_mut().find(|group| group.id == group_id) {
                    group.side = side;
                    group.collapsed = false;
                    group.head_coll = false;
                    if matches!(side, DockSide::Top | DockSide::Bottom) && group.height <= 0.0 {
                        group.height = 320.0;
                    }
                }
            }
            Some(Act::HideZone(side)) => match side {
                DockSide::Left => self.ui2.left_hidden = true,
                DockSide::Right => self.ui2.right_hidden = true,
                DockSide::Top | DockSide::Bottom => {}
                DockSide::Float => {}
            },
            Some(Act::Remove(id)) => {
                self.ui2.dock.retain(|g| g.id != id);
            }
            Some(Act::ResizeFloat(id, dp, ds)) => {
                if let Some(g) = self.ui2.dock.iter_mut().find(|g| g.id == id) {
                    g.pos += dp;
                    g.size = egui::vec2((g.size.x + ds.x).max(200.0), (g.size.y + ds.y).max(140.0));
                }
            }
            Some(Act::CloseTab { gi, ti }) => {
                if let Some(g) = self.ui2.dock.get_mut(gi) {
                    if ti < g.tabs.len() {
                        g.tabs.remove(ti);
                        if g.tabs.is_empty() {
                            // drop empty group below
                        } else if g.active >= g.tabs.len() {
                            g.active = g.tabs.len() - 1;
                        } else if g.active > ti {
                            g.active -= 1;
                        } else if g.active == ti {
                            g.active = ti.min(g.tabs.len() - 1);
                        }
                    }
                }
                // Remove empty groups
                self.ui2.dock.retain(|g| !g.tabs.is_empty());
            }
            Some(Act::CloseOtherTabs { gi, ti }) => {
                if let Some(g) = self.ui2.dock.get_mut(gi) {
                    if ti < g.tabs.len() {
                        let keep = g.tabs[ti];
                        g.tabs = vec![keep];
                        g.active = 0;
                    }
                }
            }
            Some(Act::FloatTab { group_id, ti, pos }) => {
                self.ui2_float_single_tab(group_id, ti, pos);
            }
            Some(Act::TabFloatDrag { group_id, ti, pos, delta: _, released }) => {
                let painter = ctx.layer_painter(
                    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("v2_tab_float_ghost")));
                if !released {
                    // Ghost chip while peeling a single tab
                    let label = self.ui2.dock.iter()
                        .find(|g| g.id == group_id)
                        .and_then(|g| g.tabs.get(ti).map(|k| k.label()))
                        .unwrap_or("Tab");
                    let r = egui::Rect::from_min_size(
                        pos + egui::vec2(12.0, -14.0),
                        egui::vec2(160.0, 34.0),
                    );
                    painter.rect_filled(r, 8.0, ui_theme::SELECTED_ROW);
                    dashed_rect(&painter, r, egui::Stroke::new(2.0, ui_theme::ACCENT));
                    painter.text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("release to float  ·  {label}"),
                        egui::FontId::proportional(11.0),
                        ui_theme::ACCENT_LIGHT,
                    );
                    // Drop target highlight if over a dock
                    if let Some((_, _, line_y, x0, x1)) = self.ui2_drop_target(ctx, pos) {
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(x0, line_y - 1.5),
                            egui::pos2(x1, line_y + 1.5),
                        );
                        painter.rect_filled(bar, 2.0, ui_theme::ACCENT);
                    }
                } else {
                    match self.ui2_drop_target(ctx, pos) {
                        Some((side, slot, _, _, _)) => {
                            // Peel tab into a new docked group at slot
                            if let Some(kind) = self.ui2_take_tab(group_id, ti) {
                                let mut ng = self.ui2.mk(vec![kind], side);
                                // Insert at slot among same-side groups
                                let mut insert = 0usize;
                                let mut seen = 0usize;
                                for (i, g) in self.ui2.dock.iter().enumerate() {
                                    if g.side == side {
                                        if seen == slot {
                                            insert = i;
                                            break;
                                        }
                                        seen += 1;
                                        insert = i + 1;
                                    }
                                }
                                self.ui2.dock.insert(insert.min(self.ui2.dock.len()), ng);
                            }
                        }
                        None => {
                            self.ui2_float_single_tab(group_id, ti, pos);
                        }
                    }
                    self.ui2.tab_float_drag = None;
                }
            }
            Some(Act::GripDrag { id, floating, pos, delta, released }) => {
                if floating && !released {
                    if let Some(g) = self.ui2.dock.iter_mut().find(|g| g.id == id) { g.pos += delta; }
                }
                let target = self.ui2_drop_target(ctx, pos);
                if !released {
                    let painter = ctx.layer_painter(
                        egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("v2_droplayer")));
                    if let Some((_, _, line_y, x0, x1)) = target {
                        // 3px accent bar, r2, with the artifact's `0 0 10px 1px` accent glow —
                        // approximated by concentric fading strokes (egui has no shape shadow).
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(x0, line_y - 1.5), egui::pos2(x1, line_y + 1.5));
                        for i in (1..=5).rev() {
                            let g = bar.expand(i as f32 * 1.6);
                            painter.rect_filled(g, 2.0 + i as f32,
                                ui_theme::accent_tint(28 - i as u8 * 4));
                        }
                        painter.rect_filled(bar, 2.0, ui_theme::ACCENT);
                    } else if !floating {
                        let r = egui::Rect::from_min_size(pos + egui::vec2(10.0, -12.0), egui::vec2(180.0, 36.0));
                        painter.rect_filled(r, 8.0, ui_theme::SELECTED_ROW);
                        dashed_rect(&painter, r, egui::Stroke::new(2.0, ui_theme::ACCENT));
                        painter.text(r.center(), egui::Align2::CENTER_CENTER, "release to float",
                            egui::FontId::proportional(11.0), ui_theme::ACCENT_LIGHT);
                    }
                } else {
                    match target {
                        Some((side, slot, _, _, _)) => self.ui2_move_to_slot(id, side, slot),
                        None if !floating => {
                            if let Some(g) = self.ui2.dock.iter_mut().find(|g| g.id == id) {
                                g.side = DockSide::Float; g.pos = pos - egui::vec2(18.0, 14.0);
                            }
                        }
                        None => {}
                    }
                }
            }
            None => {}
        }
    }

    /// Remove one tab from a group; returns its kind. Drops the group if empty.
    pub(crate) fn ui2_take_tab(&mut self, group_id: u64, ti: usize) -> Option<PanelKind> {
        let gi = self.ui2.dock.iter().position(|g| g.id == group_id)?;
        let g = self.ui2.dock.get_mut(gi)?;
        if ti >= g.tabs.len() {
            return None;
        }
        let kind = g.tabs.remove(ti);
        if g.tabs.is_empty() {
            self.ui2.dock.remove(gi);
        } else if g.active >= g.tabs.len() {
            g.active = g.tabs.len() - 1;
        } else if g.active > ti {
            g.active -= 1;
        } else if g.active == ti {
            g.active = ti.min(g.tabs.len().saturating_sub(1));
        }
        Some(kind)
    }

    /// Peel one tab into a floating single-tab panel at `pos`.
    /// Last tab in a group → float the whole group (same end state).
    pub(crate) fn ui2_float_single_tab(&mut self, group_id: u64, ti: usize, pos: egui::Pos2) {
        let Some(gi) = self.ui2.dock.iter().position(|g| g.id == group_id) else {
            return;
        };
        let tabs_n = self.ui2.dock[gi].tabs.len();
        if tabs_n == 0 {
            return;
        }
        if tabs_n == 1 {
            // Whole group becomes float
            if let Some(g) = self.ui2.dock.get_mut(gi) {
                g.side = DockSide::Float;
                g.pos = pos - egui::vec2(18.0, 14.0);
                if g.size.x < 200.0 {
                    g.size = egui::vec2(320.0, 400.0);
                }
            }
            return;
        }
        let Some(kind) = self.ui2_take_tab(group_id, ti) else {
            return;
        };
        let mut ng = self.ui2.mk(vec![kind], DockSide::Float);
        ng.pos = pos - egui::vec2(18.0, 14.0);
        ng.size = egui::vec2(320.0, 400.0);
        self.ui2.dock.push(ng);
    }

    // ── Outliner content (real scene tree wired to the scene graph) ──────────

}
