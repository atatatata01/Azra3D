//! Outliner keyboard navigation.
use eframe::egui;
use crate::*;
use super::types::*;

impl FourthLayerApp {
    /// ↑↓ move selection · Enter/F2 rename · ←/→ collapse/expand · Space toggle expand.
    pub(crate) fn ui2_ol_keyboard(
        &mut self,
        ui: &egui::Ui,
        rows: &[OlRow],
        sel_click: &mut Option<(SceneItemId, SelMode)>,
        do_rename: &mut Option<(SceneNodeId, String)>,
        _ctx_act: &mut Option<OlAct>,
    ) {
        if rows.is_empty() || self.outliner.renaming.is_some() {
            return;
        }
        // Only when pointer is over this panel (don't steal viewport arrow-orbit).
        if !ui.ui_contains_pointer() {
            return;
        }
        let (down, up, enter, left, right, space) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Space),
            )
        });
        if !(down || up || enter || left || right || space) {
            return;
        }

        // Resolve focus row: kb index, else active selection, else first non-root
        let mut idx = self.ui2.ol_kb_row.unwrap_or(0).min(rows.len().saturating_sub(1));
        if self.ui2.ol_kb_row.is_none() {
            if let Some(a) = self.sel.usel.active {
                if let Some(i) = rows.iter().position(|r| r.item.sel_id() == a) {
                    idx = i;
                }
            } else if rows.len() > 1 {
                idx = 1; // skip Scene root
            }
        }

        if down {
            idx = (idx + 1).min(rows.len().saturating_sub(1));
            *sel_click = Some((rows[idx].item.sel_id(), SelMode::Single));
            self.ui2.ol_kb_row = Some(idx);
        } else if up {
            idx = idx.saturating_sub(1);
            if idx == 0 && rows.len() > 1 {
                idx = 1;
            }
            *sel_click = Some((rows[idx].item.sel_id(), SelMode::Single));
            self.ui2.ol_kb_row = Some(idx);
        } else if enter {
            if let Some(t) = rows[idx].rename.clone() {
                *do_rename = Some((t, rows[idx].name.clone()));
            }
        } else if space || right || left {
            if let OlItem::Node(n) = rows[idx].item {
                if rows[idx].has_children {
                    if left || (space && rows[idx].expanded) {
                        self.ol_set_expanded(NodeId(n), false);
                    } else if right || (space && !rows[idx].expanded) {
                        self.ol_set_expanded(NodeId(n), true);
                    }
                }
            }
        }
    }
}