//! Panel scrollbar styling — one place, every scrollable pane body uses this.

use eframe::egui;
use crate::ui_theme as t;

/// Apply the mockup-faithful solid scrollbar to the current ui scope.
/// Call at the top of any pane body that scrolls (outliner, tools, library, properties).
pub fn apply_panel_scroll_style(ui: &mut egui::Ui) {
    let mut scroll = egui::style::ScrollStyle::solid();
    scroll.bar_width = 7.0;
    scroll.handle_min_length = 20.0;
    scroll.bar_inner_margin = 2.0;
    scroll.bar_outer_margin = 0.0;
    scroll.floating = false;
    scroll.foreground_color = false;
    ui.style_mut().spacing.scroll = scroll;
    // Explicit bar colors from tokens
    ui.visuals_mut().widgets.inactive.bg_fill = t::SURFACE_CONTROL;
    ui.visuals_mut().widgets.hovered.bg_fill = t::BORDER_HOVER;
    ui.visuals_mut().widgets.active.bg_fill = t::ACCENT_DIM;
}

/// Convenience: `ScrollArea` with panel chrome defaults.
pub fn panel_scroll_area(id_salt: impl std::hash::Hash) -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .id_source(id_salt)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
}
