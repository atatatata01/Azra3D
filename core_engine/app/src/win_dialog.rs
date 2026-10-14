// win_dialog.rs — Unsaved changes dialog
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_unsaved_dialog(&mut self, ctx: &egui::Context) {
        if !self.ui.show_unsaved_dialog { return; }
        let mut open = true;
        egui::Window::new("Save Changes?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                let name = self.project.project_path.as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("Untitled");
                ui.label(format!("Save changes to \"{}\"?", name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save_scene();
                        // CR-70b: Don't execute the pending action immediately —
                        // defer by 2 frames so egui renders the dialog dismissal
                        // before a blocking file picker opens.
                        self.ui.show_unsaved_dialog = false;
                        self.ui.deferred_action_countdown = 2;
                    }
                    if ui.button("Don't Save").clicked() {
                        self.project.scene_modified = false;
                        self.ui.show_unsaved_dialog = false;
                        self.ui.deferred_action_countdown = 2;
                    }
                    if ui.button("Cancel").clicked() {
                        self.ui.pending_action_after_save = None;
                        self.ui.show_unsaved_dialog = false;
                    }
                });
            });
        if !open {
            self.ui.pending_action_after_save = None;
            self.ui.show_unsaved_dialog = false;
        }
    }

    /// Execute deferred post-dialog actions after the countdown expires.
    /// Called once per frame from update(), BEFORE draw_unsaved_dialog.
    /// The 2-frame delay ensures egui renders the dismissed dialog before
    /// a blocking file picker freezes the main thread.
    pub(crate) fn drain_deferred_dialog_action(&mut self, ctx: &egui::Context) {
        if self.ui.deferred_action_countdown == 0 { return; }
        self.ui.deferred_action_countdown -= 1;
        if self.ui.deferred_action_countdown == 0 {
            if let Some(action) = self.ui.pending_action_after_save.take() {
                match action {
                    PendingAction::NewScene => self.new_scene(),
                    PendingAction::OpenFile => self.open_scene(),
                    PendingAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                }
            }
        }
        ctx.request_repaint(); // keep frames ticking during countdown
    }
}
