//! Pane surface paint — shared by docks, library, floating panels.

use eframe::egui;
use crate::ui_theme as t;

/// Opaque pane body (mockup `.pane` fill + 1px seam stroke).
pub fn paint_pane(ui: &mut egui::Ui, rect: egui::Rect) {
    ui.painter().rect_filled(rect, t::RADIUS_PANEL, t::SURFACE_PANEL);
    ui.painter().rect_stroke(
        rect,
        t::RADIUS_PANEL,
        egui::Stroke::new(1.0, t::SEAM), egui::StrokeKind::Middle,
    );
}

/// Dock column frame fill (behind panes).
pub fn paint_dock_frame(ui: &mut egui::Ui, rect: egui::Rect) {
    ui.painter().rect_filled(rect, 0.0, t::SURFACE_FRAME);
}

/// 6px restore / head-collapse strip (seam, accent on hover).
pub fn paint_restore_strip(ui: &mut egui::Ui, rect: egui::Rect, active: bool) {
    let fill = if active { t::ACCENT } else { t::SEAM };
    ui.painter().rect_filled(rect, 0.0, fill);
}

/// Header band (tab strip / title) background.
pub fn paint_header(ui: &mut egui::Ui, rect: egui::Rect, rounding_top: bool) {
    let r = if rounding_top {
        egui::Rounding {
            nw: 7,
            ne: 7,
            sw: 0,
            se: 0,
        }
    } else {
        egui::Rounding::ZERO
    };
    ui.painter().rect_filled(rect, r, t::SURFACE_PANEL);
}

/// Close / icon button hit — returns true if clicked.
pub fn icon_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: impl std::hash::Hash,
    glyph: &str,
    tip: &str,
) -> bool {
    let resp = ui
        .interact(rect, ui.id().with(("icon_btn", id)), egui::Sense::click())
        .on_hover_text(tip);
    let col = if resp.hovered() {
        t::TEXT
    } else {
        t::TEXT_DIM
    };
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect.shrink(1.0), 4.0, t::SURFACE_CONTROL);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(13.0),
        col,
    );
    resp.clicked()
}
