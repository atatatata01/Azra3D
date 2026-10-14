//! Shared menu fillers — one command path for v1 `menus.rs` and ui2 `menus.rs`.
//!
//! **Rule:** File/Edit/Select core verbs land here. Shell/v1 only paint chrome around them.
//! See `ui2/INTEGRATION_RULES.md` §7.

use eframe::egui;
use crate::*;

/// How to append a shortcut to a menu label.
#[derive(Clone, Copy)]
pub(crate) enum MenuShortcutStyle {
    /// `"Save  (Ctrl+S)"` — v1 menu bar
    Paren,
    /// `"Save\tCtrl+S"` — ui2 app-bar menus (egui right-aligns after tab)
    Tab,
}

impl MenuShortcutStyle {
    pub fn format(self, name: &str, shortcut: &str) -> String {
        if shortcut.is_empty() {
            return name.to_string();
        }
        match self {
            Self::Paren => format!("{name}  ({shortcut})"),
            Self::Tab => format!("{name}\t{shortcut}"),
        }
    }
}

impl FourthLayerApp {
    fn menu_act_label(&self, style: MenuShortcutStyle, name: &str, act: &str) -> String {
        style.format(name, &self.keybindings.label(act))
    }

    /// New / Open / Save / Save As — shared between v1 and ui2.
    pub(crate) fn menu_fill_file_scene(
        &mut self,
        ui: &mut egui::Ui,
        style: MenuShortcutStyle,
    ) {
        if ui
            .button(self.menu_act_label(style, "New Scene", ACT_NEW_SCENE))
            .clicked()
        {
            if self.project.scene_modified {
                self.ui.pending_action_after_save = Some(PendingAction::NewScene);
                self.ui.show_unsaved_dialog = true;
            } else {
                self.new_scene();
            }
            ui.close_menu();
        }
        if ui
            .button(self.menu_act_label(style, "Open…", ACT_OPEN_SCENE))
            .clicked()
        {
            if self.project.scene_modified {
                self.ui.pending_action_after_save = Some(PendingAction::OpenFile);
                self.ui.show_unsaved_dialog = true;
            } else {
                self.open_scene();
            }
            ui.close_menu();
        }
        if ui
            .button(self.menu_act_label(style, "Save", ACT_SAVE_SCENE))
            .clicked()
        {
            self.save_scene();
            ui.close_menu();
        }
        if ui
            .button(self.menu_act_label(style, "Save As…", ACT_SAVE_AS))
            .clicked()
        {
            self.save_scene_as();
            ui.close_menu();
        }
    }

    /// Save as default / Reset default scene.
    pub(crate) fn menu_fill_file_defaults(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Save as Default Scene")
            .on_hover_text("New sessions start with this scene")
            .clicked()
        {
            self.save_as_default_scene();
            ui.close_menu();
        }
        if ui
            .button("Reset Default Scene")
            .on_hover_text("New sessions start empty")
            .clicked()
        {
            self.reset_default_scene();
            ui.close_menu();
        }
    }

    /// Undo / Redo / Delete / Duplicate / Select All / Deselect / Shortcuts.
    pub(crate) fn menu_fill_edit_core(
        &mut self,
        ui: &mut egui::Ui,
        style: MenuShortcutStyle,
    ) {
        if ui
            .add_enabled(
                self.history.can_undo(),
                egui::Button::new(self.menu_act_label(style, "Undo", ACT_UNDO)),
            )
            .clicked()
        {
            self.undo();
            ui.close_menu();
        }
        if ui
            .add_enabled(
                self.history.can_redo(),
                egui::Button::new(self.menu_act_label(style, "Redo", ACT_REDO)),
            )
            .clicked()
        {
            self.redo();
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(self.menu_act_label(style, "Delete", ACT_DELETE))
            .clicked()
        {
            self.delete_selected();
            ui.close_menu();
        }
        if ui
            .button(self.menu_act_label(style, "Duplicate", ACT_DUPLICATE))
            .clicked()
        {
            crate::ops::edit::duplicate::run(self);
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(self.menu_act_label(style, "Select All", ACT_SELECT_ALL))
            .clicked()
        {
            // INTENTIONAL: additive select-all with visible+unlocked filter
            for p in &self.scene.primitives {
                if p.visible && !p.locked {
                    self.sel.usel.selected.insert(SceneItemId::Prim(p.prim_uid));
                }
            }
            if let Some(p) = self.scene.primitives.first() {
                self.sel.usel.active = Some(SceneItemId::Prim(p.prim_uid));
            }
            ui.close_menu();
        }
        if ui
            .button(self.menu_act_label(style, "Deselect All", ACT_DESELECT_ALL))
            .clicked()
        {
            self.deselect_all();
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(self.menu_act_label(style, "Shortcuts…", ACT_SHORTCUT_EDITOR))
            .clicked()
        {
            self.ui.show_keybind_settings = true;
            // ui2 also has show_shortcut_editor
            self.ui.show_shortcut_editor = true;
            ui.close_menu();
        }
    }
}
