//! History panel — live view of the undo / redo stacks.
//!
//! Wired to `PanelKind::History`. Lists every `UndoEntry.description` so the user can see
//! what they did. Click an entry to jump there (multi-step undo/redo).

use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;

impl AzraApp {
    /// Draw the History dock/floating panel body.
    pub(crate) fn ui2_draw_history(&mut self, ui: &mut egui::Ui) {
        // Snapshot labels first — drawing loops must not borrow `history` while `undo`/`redo` run.
        let undo_labels: Vec<String> = self
            .history
            .undo_stack
            .iter()
            .map(|e| e.description.clone())
            .collect();
        let redo_labels: Vec<String> = self
            .history
            .redo_stack
            .iter()
            .map(|e| e.description.clone())
            .collect();
        let undo_n = undo_labels.len();
        let redo_n = redo_labels.len();
        let can_undo = undo_n > 0;
        let can_redo = redo_n > 0;
        let pending = self.pending_episode.as_ref().map(|e| e.description.clone());
        let max_entries = self.history.max_entries;

        let mut undo_steps: Option<u32> = None;
        let mut redo_steps: Option<u32> = None;
        let mut do_undo = false;
        let mut do_redo = false;

        // ---- Toolbar ----
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui
                .add_enabled(can_undo, egui::Button::new(format!("{} Undo", ph::ARROW_U_UP_LEFT)))
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                do_undo = true;
            }
            if ui
                .add_enabled(can_redo, egui::Button::new(format!("{} Redo", ph::ARROW_U_UP_RIGHT)))
                .on_hover_text("Ctrl+Y / Ctrl+Shift+Z")
                .clicked()
            {
                do_redo = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("{undo_n} undo · {redo_n} redo"));
            });
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        if let Some(ref d) = pending {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ui_theme::ACCENT,
                    format!("{}  In progress", ph::CIRCLE),
                );
                ui.strong(d.as_str());
            });
            ui.weak("Release / confirm to commit to history");
            ui.add_space(4.0);
            ui.separator();
        }

        let h = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("v2_history_scroll")
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                // ---- Future (redo) ----
                if redo_n > 0 {
                    ui.label(
                        egui::RichText::new("REDO (future)")
                            .size(10.0)
                            .color(ui_theme::TEXT_SECTION)
                            .strong(),
                    );
                    ui.add_space(2.0);
                    // oldest future first; tip of redo stack is back (first Ctrl+Y).
                    for (i, desc) in redo_labels.iter().enumerate() {
                        let steps = (redo_n - i) as u32;
                        let label = format!("{}  {}", ph::ARROW_CLOCKWISE, desc);
                        let resp = ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(label)
                                        .color(ui_theme::TEXT_DIM)
                                        .size(12.0),
                                )
                                .fill(egui::Color32::TRANSPARENT)
                                .frame(false)
                                .min_size(egui::vec2(ui.available_width(), 20.0)),
                            )
                            .on_hover_text(format!("Redo {steps} step(s) to reach this"));
                        if resp.clicked() {
                            redo_steps = Some(steps);
                        }
                    }
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                }

                // ---- Present ----
                ui.horizontal(|ui| {
                    ui.colored_label(ui_theme::ACCENT, format!("{}  Now", ph::DOT_OUTLINE));
                    ui.weak("current scene state");
                });
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ---- Past (undo), newest first ----
                if undo_n == 0 {
                    ui.weak("No actions yet — edits appear here as you work.");
                } else {
                    ui.label(
                        egui::RichText::new("UNDO (past)")
                            .size(10.0)
                            .color(ui_theme::TEXT_SECTION)
                            .strong(),
                    );
                    ui.add_space(2.0);
                    for (rev_i, desc) in undo_labels.iter().rev().enumerate() {
                        let steps = (rev_i + 1) as u32;
                        let is_tip = rev_i == 0;
                        let icon = if is_tip {
                            ph::ARROW_COUNTER_CLOCKWISE
                        } else {
                            ph::CIRCLE
                        };
                        let text = format!("{icon}  {desc}");
                        let rich = if is_tip {
                            egui::RichText::new(text)
                                .color(ui_theme::TEXT)
                                .size(12.5)
                                .strong()
                        } else {
                            egui::RichText::new(text).color(ui_theme::TEXT).size(12.0)
                        };
                        let fill = if is_tip {
                            ui_theme::SURFACE_FIELD
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let resp = ui
                            .add(
                                egui::Button::new(rich)
                                    .fill(fill)
                                    .frame(is_tip)
                                    .min_size(egui::vec2(ui.available_width(), 22.0)),
                            )
                            .on_hover_text(if is_tip {
                                "Most recent action — Undo once (Ctrl+Z)".into()
                            } else {
                                format!("Undo {steps} step(s) to go back past this")
                            });
                        if resp.clicked() {
                            undo_steps = Some(steps);
                        }
                    }
                }

                ui.add_space(10.0);
                ui.weak(format!("Stack depth {undo_n} / max {max_entries}"));
            });

        // Apply after the UI borrow ends.
        if do_undo {
            self.undo();
        } else if do_redo {
            self.redo();
        } else if let Some(n) = undo_steps {
            for _ in 0..n {
                self.undo();
            }
        } else if let Some(n) = redo_steps {
            for _ in 0..n {
                self.redo();
            }
        }
    }
}
