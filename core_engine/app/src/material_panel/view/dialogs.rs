//! Material panel dialogs: no-material promotion, confirm, presets.

use eframe::egui;
use egui_phosphor::regular as ph;
use core_math::pbr_material::PbrMaterial;
use core_math::primitives::MaterialId;
use core_math::scene_graph::NodePayload;
use crate::ui_theme as t;
use crate::AzraApp;

use super::super::model::*;
use super::kind_label;

impl AzraApp {
    /// The EMBEDDED material the current selection renders, when it has no
    /// Material node (`material_id == NONE` / dangling). Mirrors the resolution
    /// order of `props_material_resolve` so this shows what the panel would have
    /// shown, and matches `resolve_prim_material`'s fallback — i.e. exactly what
    /// the renderer reads. `None` = nothing selected that has a material at all.
    pub(super) fn mp_embedded_material_of_selection(&self) -> Option<PbrMaterial> {
        if let Some(idx) = self.active_prim_index_unified() {
            if let Some(p) = self.scene.primitives.get(idx) {
                return Some(p.material.clone());
            }
        }
        let nid = self.sel.usel.active_node()?;
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { .. }) => self
                .scene
                .scene_graph
                .body_primitives(nid)
                .and_then(|ps| ps.first().map(|p| p.material.clone())),
            Some(NodePayload::PolygonalBody { material, .. }) => Some(material.clone()),
            _ => None,
        }
    }

    /// NEW-1 — the "no material node" state. Previously a flat dead-end hint
    /// ("No material — assign with Ctrl+M or Materials menu") shown even when the
    /// selected object was visibly rendering its embedded material, which the v2
    /// panel had no way to edit. Now: if something IS rendering, show that look
    /// and offer the one-click promotion that makes it editable.
    pub(super) fn mp_draw_no_material_state(&mut self, ui: &mut egui::Ui) {
        let Some(embedded) = self.mp_embedded_material_of_selection() else {
            // Genuinely nothing selected that carries a material.
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No object selected")
                        .size(11.0)
                        .color(t::TEXT_FAINT),
                );
                ui.label(
                    egui::RichText::new("Select an object to edit its material.")
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
            });
            return;
        };
        let ctx = ui.ctx().clone();
        let preview = self.cache.mat_preview_cache.get_or_create(&ctx, &embedded);
        let mut promote = false;
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.image(egui::load::SizedTexture::new(preview, egui::vec2(56.0, 56.0)));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("This object's material isn't shared yet")
                    .size(11.5)
                    .color(t::TEXT)
                    .strong(),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "It renders the look above, stored on the object itself.\n\
                     Create a material to edit it here, add layers, and reuse it elsewhere.",
                )
                .size(10.0)
                .color(t::TEXT_FAINT),
            );
            ui.add_space(8.0);
            if ui
                .add(egui::Button::new(
                    egui::RichText::new(format!("{} Create material from this object", ph::PLUS))
                        .size(11.0),
                ))
                .on_hover_text(
                    "Keeps the current look exactly — it just becomes an editable, reusable material.",
                )
                .clicked()
            {
                promote = true;
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("or assign an existing one with Ctrl+M")
                    .size(9.5)
                    .color(t::TEXT_FAINT),
            );
        });
        if promote {
            // ONE transaction / one Ctrl+Z, non-destructive (seeded from the
            // embedded material, so the object does not change appearance).
            self.promote_embedded_material_for_selection(None);
        }
    }

    /// Destructive-action confirmation modal (spec C1: "with confirmation").
    /// Same centered-Window pattern as `draw_unsaved_dialog` (win_dialog.rs).
    pub(super) fn mp_draw_confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.ui.mp_confirm else { return };
        let (title, body) = match pending {
            super::super::edit::MpConfirm::RemoveChannel { kind, .. } => (
                "Remove channel?",
                format!(
                    "Remove {} — its layers / graph wiring will be discarded.",
                    kind_label(kind)
                ),
            ),
            super::super::edit::MpConfirm::RevertChannel { kind, .. } => (
                "Revert to panel control?",
                format!(
                    "{} is graph-driven. Reverting discards its node subgraph.",
                    kind_label(kind)
                ),
            ),
            super::super::edit::MpConfirm::YieldChannelToGraph { kind, .. } => (
                "Let the shader tree drive this channel?",
                format!(
                    "{}'s panel layers will be discarded so the shader tree's bake can install here.",
                    kind_label(kind)
                ),
            ),
        };
        let mut open = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Confirm").clicked() {
                        self.ui.mp_confirm = None;
                        match pending {
                            super::super::edit::MpConfirm::RemoveChannel { mid, kind } => {
                                self.mp_remove_channel_confirmed(MaterialId(mid), kind);
                            }
                            super::super::edit::MpConfirm::RevertChannel { mid, kind } => {
                                self.mp_revert_channel_confirmed(MaterialId(mid), kind);
                            }
                            super::super::edit::MpConfirm::YieldChannelToGraph { mid, kind } => {
                                self.mp_conflict_yield_to_graph(MaterialId(mid), kind);
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.ui.mp_confirm = None;
                    }
                });
            });
        if !open {
            self.ui.mp_confirm = None;
        }
    }

    /// P5 preset dialogs: destructive-apply confirm, "Save as preset…" name
    /// prompt, user-preset delete confirm. Same centered-Window pattern as
    /// `mp_draw_confirm_dialog`.
    pub(super) fn mp_draw_preset_dialogs(&mut self, ctx: &egui::Context) {
        // 1 · Destructive apply confirm (current doc has GraphDriven channels
        // or loaded image layers — replacing the doc discards them).
        if let Some((mid, preset)) = self.ui.mp_pending_preset.clone() {
            let mut open = true;
            egui::Window::new("Apply preset?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Applying '{}' replaces this material's channels — its graph wiring / loaded image layers will be discarded.",
                        preset.name
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            self.ui.mp_pending_preset = None;
                            self.mp_apply_preset(MaterialId(mid), &preset);
                        }
                        if ui.button("Cancel").clicked() {
                            self.ui.mp_pending_preset = None;
                        }
                    });
                });
            if !open {
                self.ui.mp_pending_preset = None;
            }
        }
        // 2 · Save-as-preset name prompt.
        if let Some((mid, mut buf)) = self.ui.mp_preset_naming.clone() {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Save material preset")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Preset name:");
                    let r = ui.text_edit_singleline(&mut buf);
                    r.request_focus();
                    let submit = (r.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        || ui.button("Save").clicked();
                    if submit && !buf.trim().is_empty() {
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.ui.mp_preset_naming = None;
                    }
                });
            if done {
                self.ui.mp_preset_naming = None;
                self.mp_save_preset(MaterialId(mid), &buf);
            } else if !open {
                self.ui.mp_preset_naming = None;
            } else if self.ui.mp_preset_naming.is_some() {
                self.ui.mp_preset_naming = Some((mid, buf)); // keep typed text
            }
        }
        // 3 · Delete user preset confirm.
        if let Some(idx) = self.ui.mp_preset_delete {
            let pname = self
                .ui
                .mp_user_presets
                .as_ref()
                .and_then(|l| l.get(idx))
                .map(|p| p.name.clone());
            let Some(pname) = pname else {
                self.ui.mp_preset_delete = None;
                return;
            };
            let mut open = true;
            egui::Window::new("Delete preset?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!("Delete user preset '{pname}'? This cannot be undone."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            self.ui.mp_preset_delete = None;
                            self.mp_delete_user_preset(idx);
                        }
                        if ui.button("Cancel").clicked() {
                            self.ui.mp_preset_delete = None;
                        }
                    });
                });
            if !open {
                self.ui.mp_preset_delete = None;
            }
        }
    }
}
