// win_settings.rs — Keybind settings, experimental settings, about window, shortcut editor
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;

impl AzraApp {
    // ================================================================
    // 1. Keybind Settings (read-only shortcut reference)
    // ================================================================
    pub(crate) fn draw_keybind_settings(&mut self, ctx: &egui::Context) {
        if !self.ui.show_keybind_settings { return; }
        let kb = &self.keybindings;
        let (g,r,s,del,dup,foc,sa) = (kb.label(ACT_GRAB),kb.label(ACT_ROTATE),kb.label(ACT_SCALE),
            kb.label(ACT_DELETE),kb.label(ACT_DUPLICATE),kb.label(ACT_FOCUS),kb.label(ACT_SELECT_ALL));
        let (sp,mp,u,re) = (kb.label(ACT_SHADING_PIE),kb.label(ACT_MODE_PIE),kb.label(ACT_UNDO),kb.label(ACT_REDO));
        let (vf,vr,vt) = (kb.label(crate::ACT_VIEW_FRONT),kb.label(crate::ACT_VIEW_RIGHT),kb.label(crate::ACT_VIEW_TOP));
        let mut open_editor = false;
        egui::Window::new("\u{2328} Shortcuts").collapsible(false).resizable(false).default_width(380.0).open(&mut self.ui.show_keybind_settings).show(ctx,|ui|{
            ui.label("Selection:"); ui.weak("  LMB=Select | Ctrl+LMB=Toggle | Shift+Drag=Box | Alt+Drag=Lasso");
            ui.separator(); ui.label("Modal Transform:");
            ui.weak(format!("  {}=Grab | {}=Rotate | {}=Scale", g, r, s));
            ui.weak("  X/Y/Z=Lock axis | Shift+X/Y/Z=Lock plane | Type digits=Exact value");
            ui.weak("  Enter=Confirm | Esc=Cancel");
            ui.separator(); ui.label("View:");
            ui.weak(format!("  {}=Shading Pie | {}=Mode Pie | {}=Front | {}=Right | {}=Top", sp, mp, vf, vr, vt));
            ui.separator(); ui.label("General:");
            ui.weak(format!("  {}=Undo | {}=Redo | {}=Delete | {}=Duplicate | {}=Focus | {}=All", u, re, del, dup, foc, sa));
            ui.separator(); ui.label("Sculpt:"); ui.weak("  Shift+Paint=Smooth | Ctrl+Paint=Invert brush");
            ui.separator(); ui.label("Navigation:"); ui.weak("  MMB=Orbit | Shift+MMB=Pan | Scroll=Zoom");
            ui.separator();
            if ui.button(format!("\u{2328} Open Shortcut Editor ({})", kb.label(ACT_SHORTCUT_EDITOR))).clicked() { open_editor = true; }
        });
        if open_editor { self.ui.show_shortcut_editor = true; self.ui.show_keybind_settings = false; }
    }

    // ================================================================
    // 1b. Experimental Settings
    // ================================================================
    pub(crate) fn draw_experimental_settings(&mut self, ctx: &egui::Context) {
        if !self.ui.show_experimental { return; }
        egui::Window::new("\u{1F9EA} Experimental")
            .collapsible(false).resizable(false).default_width(340.0)
            .open(&mut self.ui.show_experimental)
            .show(ctx, |ui| {
                ui.label("These features are incomplete or unstable. Enable at your own risk.");
                ui.separator();
                let prev = self.experimental.clone();
                ui.checkbox(&mut self.experimental.multi_segment_bevel, "Multi-segment bevel");
                ui.weak("  Bevel with variable segment counts and arc profiles.");
                ui.add_space(4.0);
                // Vertex bevel is production (Ctrl+B in Vertex mode, BEVEL.md §11 2026-07-03).
                // Checkbox removed — it was a dead gate (never wired) defaulting false and
                // read as "feature disabled". Flag kept in ExperimentalFlags for JSON load.
                ui.checkbox(&mut self.experimental.sdf_sculpt, "SDF sculpt brushes");
                ui.weak("  Direct sculpting on SDF volumes with brush tools.");
                ui.add_space(4.0);
                ui.checkbox(&mut self.experimental.gpu_remesh, "GPU voxel remesh");
                ui.weak("  Evaluates the Remesh modifier's voxel field on the GPU. Output may");
                ui.weak("  differ slightly from the CPU path until parity gates close.");
                if self.experimental.multi_segment_bevel != prev.multi_segment_bevel
                    || self.experimental.sdf_sculpt != prev.sdf_sculpt
                    || self.experimental.gpu_remesh != prev.gpu_remesh
                {
                    self.experimental.save();
                }
            });
    }

    // ================================================================
    // 2. About Window
    // ================================================================
    pub(crate) fn draw_about_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_about { return; }
        egui::Window::new("About").collapsible(false).resizable(false).open(&mut self.ui.show_about).show(ctx,|ui|{
            ui.heading("AZRA"); ui.label("Phase A \u{2014} SDF Modeling + PBR Materials");
            ui.weak("Pro Gizmo, Click-to-Place, Boolean Colors, AI Agent, PBR Node Editor");
            ui.add_space(6.0); ui.separator(); ui.add_space(4.0);
            ui.label(format!("\u{1f5a5} GPU: {}", self.gpu_info.adapter_name));
            ui.label(format!("\u{2699} Backend: {} | Type: {}", self.gpu_info.backend, self.gpu_info.device_type));
            if !self.gpu_info.driver.is_empty() { ui.weak(format!("Driver: {}", self.gpu_info.driver)); }
            ui.add_space(4.0);
            ui.weak("Set WGPU_BACKEND=vulkan|dx12|metal to force backend");
        });
    }

    // ================================================================
    // 3. Shortcut Editor (End key)
    // ================================================================
    pub(crate) fn draw_shortcut_editor(&mut self, ctx: &egui::Context) {
        if !self.ui.show_shortcut_editor { return; }
        let mut rebinding_action_pending: Option<Option<&'static str>> = None;
        let is_rebinding = self.ui.rebinding_action.is_some();
        let mut profile_change: Option<KeyBindings> = None;
        let mut reset_action: Option<&'static str> = None;
        egui::Window::new("\u{2328} Shortcut Editor")
            .default_width(420.0).resizable(true).collapsible(false)
            .open(&mut self.ui.show_shortcut_editor)
            .show(ctx, |ui| {
                // Rebinding mode
                if is_rebinding {
                    ui.colored_label(egui::Color32::from_rgb(255,200,60),
                        "Press any key combination to rebind... (Esc to cancel)");
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        rebinding_action_pending = Some(None);
                    }
                    return;
                }
                // Profile row
                ui.horizontal(|ui| {
                    ui.label("Profile:");
                    if ui.button("Default").clicked() { profile_change = Some(KeyBindings::default()); }
                    if ui.button("Modo").clicked() { profile_change = Some(KeyBindings::profile_modo()); }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset All").clicked() { profile_change = Some(KeyBindings::default()); }
                    });
                });
                // Search
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.text_edit_singleline(&mut self.ui.keybind_search);
                });
                ui.separator();
                // Conflict warnings
                if !self.ui.keybind_conflicts.is_empty() {
                    for (name, label) in &self.ui.keybind_conflicts {
                        ui.colored_label(egui::Color32::from_rgb(255,120,60),
                            format!("\u{26a0} Conflict: \"{}\" also uses {}", name, label));
                    }
                    ui.separator();
                }
                // Actions list from ACTION_DEFS
                let search_lower = self.ui.keybind_search.to_lowercase();
                egui::ScrollArea::vertical().max_height(500.0).show(ui, |ui| {
                    let mut cur_cat = "";
                    for def in ACTION_DEFS {
                        let current = self.keybindings.get(def.id);
                        let lbl = current.label();
                        let matches_search = search_lower.is_empty()
                            || def.name.to_lowercase().contains(&search_lower)
                            || lbl.to_lowercase().contains(&search_lower)
                            || def.category.to_lowercase().contains(&search_lower);
                        if !matches_search { continue; }
                        // Category header
                        if def.category != cur_cat {
                            if !cur_cat.is_empty() { ui.add_space(4.0); }
                            ui.strong(def.category);
                            ui.separator();
                            cur_cat = def.category;
                        }
                        ui.horizontal(|ui| {
                            ui.set_min_width(300.0);
                            ui.label(def.name);
                            // Context hint — derived from the action's Guard (INPUT_ARCHITECTURE_PLAN S2).
                            let ctx_label = def.guard.when().describe();
                            if !ctx_label.is_empty() {
                                ui.weak(format!("({})", ctx_label));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let dflt = default_bind(def.id);
                                let is_modified = *current != dflt;
                                // Per-action reset
                                if is_modified {
                                    if ui.small_button("\u{21BA}").on_hover_text("Reset to default").clicked() {
                                        reset_action = Some(def.id);
                                    }
                                }
                                let btn_text = if is_modified { format!("[{}] *", lbl) } else { format!("[{}]", lbl) };
                                if ui.button(btn_text).clicked() {
                                    rebinding_action_pending = Some(Some(def.id));
                                    self.ui.keybind_conflicts.clear();
                                }
                            });
                        });
                    }
                });
                ui.separator();
                ui.weak(format!("Click binding \u{2192} press new key  |  * = modified  |  {} = toggle editor",
                    self.keybindings.label(ACT_SHORTCUT_EDITOR)));
            });
        if let Some(v) = rebinding_action_pending { self.ui.rebinding_action = v; }
        if let Some(id) = reset_action {
            self.keybindings.set(id, default_bind(id));
            self.keybindings.save();
            self.ui.keybind_conflicts.clear();
        }
        if let Some(kb) = profile_change {
            self.keybindings = kb;
            self.keybindings.save();
            self.ui.keybind_conflicts.clear();
            self.toast("Keybindings updated");
        }
    }
}
