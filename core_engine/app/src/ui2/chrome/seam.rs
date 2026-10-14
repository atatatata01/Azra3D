//! Pointer-driven edge resize + zone restore strip.
//! Same approach as dock L/R edges — no fragile Area/pivot hit-testing.

use eframe::egui;
use super::zone::{ZoneEdge, ZoneState};
use super::{SEAM_HIT, ZONE_STRIP};

/// Result of interacting with a seam or restore strip this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct SeamResult {
    pub resized: bool,
    pub hidden: bool,
    pub reopened: bool,
    pub hovered: bool,
}

/// Draw the resize seam on the **viewport side** of a panel rect (pointer-driven).
pub fn draw_resize_seam(
    ctx: &egui::Context,
    panel_rect: egui::Rect,
    zone: &mut ZoneState,
    id_salt: impl std::hash::Hash,
) -> SeamResult {
    if !zone.open {
        return SeamResult::default();
    }

    let hit = match zone.edge {
        ZoneEdge::Left => egui::Rect::from_min_max(
            egui::pos2(panel_rect.right() - 2.0, panel_rect.top()),
            egui::pos2(panel_rect.right() + SEAM_HIT, panel_rect.bottom()),
        ),
        ZoneEdge::Right => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left() - SEAM_HIT, panel_rect.top()),
            egui::pos2(panel_rect.left() + 2.0, panel_rect.bottom()),
        ),
        ZoneEdge::Bottom => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.top() - SEAM_HIT),
            egui::pos2(panel_rect.right(), panel_rect.top() + 2.0),
        ),
        ZoneEdge::Top => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.bottom() - 2.0),
            egui::pos2(panel_rect.right(), panel_rect.bottom() + SEAM_HIT),
        ),
    };

    // Persist drag across frames via egui temp memory (keyed by id_salt).
    let drag_id = egui::Id::new(("ui2_seam_drag", &id_salt));
    let mut dragging = ctx.data_mut(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));

    let (hover_pos, primary_down, primary_pressed, delta) = ctx.input(|i| {
        (
            i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()),
            i.pointer.primary_down(),
            i.pointer.primary_pressed(),
            i.pointer.delta(),
        )
    });
    let hovering = hover_pos.map(|p| hit.contains(p)).unwrap_or(false);
    let mut result = SeamResult {
        hovered: hovering || dragging,
        ..Default::default()
    };

    if dragging {
        if primary_down {
            let grow = zone.edge.outward_delta(delta.x, delta.y);
            let hid = zone.apply_resize_delta(grow);
            result.resized = true;
            result.hidden = hid;
            if hid {
                dragging = false;
            }
            // Accent line
            paint_edge_line(ctx, panel_rect, zone.edge, true);
            ctx.output_mut(|o| {
                o.cursor_icon = if zone.edge.is_horizontal() {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                };
            });
        } else {
            dragging = false;
        }
    } else if hovering {
        paint_edge_line(ctx, panel_rect, zone.edge, true);
        ctx.output_mut(|o| {
            o.cursor_icon = if zone.edge.is_horizontal() {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::ResizeVertical
            };
        });
        if primary_pressed {
            dragging = true;
            zone.hide_slop = 0.0;
        }
    }

    ctx.data_mut(|d| d.insert_temp(drag_id, dragging));
    result
}

/// Draw the 6px restore strip when a zone is hidden (pointer-driven).
pub fn draw_restore_strip(
    ctx: &egui::Context,
    panel_rect: egui::Rect,
    zone: &mut ZoneState,
    id_salt: impl std::hash::Hash,
) -> SeamResult {
    if zone.open {
        return SeamResult::default();
    }

    let hit = match zone.edge {
        ZoneEdge::Left => egui::Rect::from_min_max(
            panel_rect.left_top(),
            egui::pos2(panel_rect.right() + 8.0, panel_rect.bottom()),
        ),
        ZoneEdge::Right => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left() - 8.0, panel_rect.top()),
            panel_rect.right_bottom(),
        ),
        ZoneEdge::Bottom => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.bottom() - ZONE_STRIP - 8.0),
            panel_rect.right_bottom(),
        ),
        ZoneEdge::Top => egui::Rect::from_min_max(
            panel_rect.left_top(),
            egui::pos2(panel_rect.right(), panel_rect.top() + ZONE_STRIP + 8.0),
        ),
    };

    let paint = match zone.edge {
        ZoneEdge::Left => {
            egui::Rect::from_min_size(panel_rect.left_top(), egui::vec2(ZONE_STRIP, panel_rect.height()))
        }
        ZoneEdge::Right => egui::Rect::from_min_max(
            egui::pos2(panel_rect.right() - ZONE_STRIP, panel_rect.top()),
            panel_rect.right_bottom(),
        ),
        ZoneEdge::Bottom => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.bottom() - ZONE_STRIP),
            panel_rect.right_bottom(),
        ),
        ZoneEdge::Top => {
            egui::Rect::from_min_size(panel_rect.left_top(), egui::vec2(panel_rect.width(), ZONE_STRIP))
        }
    };

    let (hover_pos, primary_down, primary_pressed, delta) = ctx.input(|i| {
        (
            i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()),
            i.pointer.primary_down(),
            i.pointer.primary_pressed(),
            i.pointer.delta(),
        )
    });
    let hovering = hover_pos.map(|p| hit.contains(p)).unwrap_or(false);
    let mut result = SeamResult {
        hovered: hovering,
        ..Default::default()
    };

    let layer = egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new(("ui2_restore_paint", id_salt)),
    );
    ctx.layer_painter(layer)
        .rect_filled(paint, 0.0, if hovering { crate::ui_theme::ACCENT } else { crate::ui_theme::SEAM });

    if hovering {
        ctx.output_mut(|o| {
            o.cursor_icon = if zone.edge.is_horizontal() {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::ResizeVertical
            };
        });
        if primary_pressed {
            zone.reopen(None);
            result.reopened = true;
        } else if primary_down {
            let outward = zone.edge.outward_delta(delta.x, delta.y);
            if outward > 0.0 {
                let base = zone.size.max(zone.min);
                zone.reopen(Some((base + outward).clamp(zone.min, zone.max)));
                result.reopened = true;
                result.resized = true;
            }
        }
    }
    result
}

fn paint_edge_line(ctx: &egui::Context, panel_rect: egui::Rect, edge: ZoneEdge, active: bool) {
    if !active {
        return;
    }
    let edge_tag = match edge {
        ZoneEdge::Left => 0u8,
        ZoneEdge::Right => 1,
        ZoneEdge::Bottom => 2,
        ZoneEdge::Top => 3,
    };
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new(("ui2_seam_line", edge_tag)));
    let p = ctx.layer_painter(layer);
    let line = match edge {
        ZoneEdge::Left => egui::Rect::from_min_max(
            egui::pos2(panel_rect.right(), panel_rect.top()),
            egui::pos2(panel_rect.right() + 2.0, panel_rect.bottom()),
        ),
        ZoneEdge::Right => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left() - 2.0, panel_rect.top()),
            egui::pos2(panel_rect.left(), panel_rect.bottom()),
        ),
        ZoneEdge::Bottom => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.top() - 2.0),
            egui::pos2(panel_rect.right(), panel_rect.top()),
        ),
        ZoneEdge::Top => egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), panel_rect.bottom()),
            egui::pos2(panel_rect.right(), panel_rect.bottom() + 2.0),
        ),
    };
    p.rect_filled(line, 0.0, crate::ui_theme::ACCENT);
}
