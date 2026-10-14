//! Pane collapse — two independent flags (mockup).
//!
//! 1. **Body collapse** (`.pane.collapsed`): head stays, body hidden.
//! 2. **Head collapse** (`.pane.__headcoll`): head becomes a 6px restore line.
//!
//! Compose with [`super::zone::ZoneState`] for full zone hide (6px strip at column edge).

use eframe::egui;
use super::{frame, HEADER_H, ZONE_STRIP};

/// Collapse flags for one pane / panel group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneCollapse {
    /// Body hidden; tab strip / title remains.
    pub body: bool,
    /// Header reduced to a 6px accent restore line.
    pub head: bool,
}

impl PaneCollapse {
    pub fn expanded() -> Self {
        Self { body: false, head: false }
    }

    /// Display height of the head region this frame.
    pub fn head_height(self) -> f32 {
        if self.head {
            ZONE_STRIP
        } else {
            HEADER_H
        }
    }

    /// Toggle body collapse. Expanding body always re-shows the head.
    pub fn toggle_body(&mut self) {
        self.body = !self.body;
        if !self.body {
            self.head = false;
        }
    }

    /// Toggle head collapse (6px line).
    pub fn toggle_head(&mut self) {
        self.head = !self.head;
    }

    /// Height of a collapsed-body pane (head only).
    pub fn collapsed_pane_height(self) -> f32 {
        self.head_height()
    }
}

/// Draw the 6px head-restore strip when `collapse.head`. Click → expand head.
/// Returns `true` if the user requested expand.
pub fn draw_head_restore_strip(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: impl std::hash::Hash,
) -> bool {
    let resp = ui.interact(
        rect,
        ui.id().with(("head_restore", id)),
        egui::Sense::click(),
    );
    frame::paint_restore_strip(ui, rect, resp.hovered() || resp.dragged());
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}
