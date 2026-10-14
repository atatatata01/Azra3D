// win_layout.rs — UI layout settings window
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;

impl AzraApp {
    // ================================================================
    // Layout Settings Window (Phase 1)
    // ================================================================
    pub(crate) fn draw_layout_settings(&mut self, ctx: &egui::Context) {
        if !self.ui.show_layout_settings { return; }
        let mut open = true;
        egui::Window::new("Layout Settings")
            .open(&mut open)
            .default_width(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                let mut changed = false;

                // ---- Preset selector ----
                ui.horizontal(|ui| {
                    ui.label("Preset:");
                    let presets = ["Default", "Minimal", "Wide"];
                    for name in &presets {
                        if ui.selectable_label(self.layout.active_preset == *name, *name).clicked() {
                            self.layout = match *name {
                                "Minimal" => crate::UiLayoutConfig::preset_minimal(),
                                "Wide" => crate::UiLayoutConfig::preset_wide(),
                                _ => crate::UiLayoutConfig::default(),
                            };
                            changed = true;
                        }
                    }
                });
                ui.separator();

                // ---- Panel visibility ----
                ui.label(egui::RichText::new("Panel Visibility").strong());
                changed |= ui.checkbox(&mut self.layout.show_tool_panel, "Tool Panel").changed();
                changed |= ui.checkbox(&mut self.layout.show_properties_panel, "Properties Panel").changed();
                changed |= ui.checkbox(&mut self.layout.show_viewport_header, "Viewport Header").changed();
                changed |= ui.checkbox(&mut self.layout.show_content_library, "Content Library").changed();
                ui.add_space(4.0);

                // ---- Panel sides ----
                ui.label(egui::RichText::new("Panel Sides").strong());
                ui.horizontal(|ui| {
                    ui.label("Tool Panel:");
                    if ui.selectable_label(self.layout.tool_panel_side == PanelSide::Left, "Left").clicked() {
                        self.layout.tool_panel_side = PanelSide::Left; changed = true;
                    }
                    if ui.selectable_label(self.layout.tool_panel_side == PanelSide::Right, "Right").clicked() {
                        self.layout.tool_panel_side = PanelSide::Right; changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Properties:");
                    if ui.selectable_label(self.layout.properties_panel_side == PanelSide::Left, "Left").clicked() {
                        self.layout.properties_panel_side = PanelSide::Left; changed = true;
                    }
                    if ui.selectable_label(self.layout.properties_panel_side == PanelSide::Right, "Right").clicked() {
                        self.layout.properties_panel_side = PanelSide::Right; changed = true;
                    }
                });
                ui.add_space(4.0);

                // ---- Panel widths ----
                ui.label(egui::RichText::new("Panel Sizes").strong());
                changed |= ui.add(egui::Slider::new(&mut self.layout.tool_panel_width, 32.0..=200.0)
                    .text("Tool Width")).changed();
                changed |= ui.add(egui::Slider::new(&mut self.layout.properties_panel_width, 150.0..=500.0)
                    .text("Properties Width")).changed();
                changed |= ui.add(egui::Slider::new(&mut self.layout.header_height, 20.0..=48.0)
                    .text("Header Height")).changed();
                changed |= ui.add(egui::Slider::new(&mut self.layout.outliner_height_fraction, 0.2..=0.8)
                    .text("Outliner %")).changed();
                ui.add_space(4.0);

                // ---- Toolbar sections ----
                ui.collapsing("Toolbar Sections", |ui| {
                    let all_sections = [
                        (ToolbarSection::Mode, "Mode Buttons"),
                        (ToolbarSection::Shape, "Shape Creation"),
                        (ToolbarSection::BooleanOp, "Boolean Ops"),
                        (ToolbarSection::GizmoMode, "Gizmo Mode"),
                        (ToolbarSection::Brush, "Sculpt Brushes"),
                        (ToolbarSection::PolyTools, "Poly Tools"),
                        (ToolbarSection::NurbsTools, "NURBS Tools"),
                    ];
                    for (sec, label) in &all_sections {
                        let has = self.layout.toolbar.sections.contains(sec);
                        let mut checked = has;
                        if ui.checkbox(&mut checked, *label).changed() {
                            if checked && !has {
                                self.layout.toolbar.sections.push(*sec);
                            } else if !checked && has {
                                self.layout.toolbar.sections.retain(|s| s != sec);
                            }
                            changed = true;
                        }
                    }
                    if ui.button("Reset Order").clicked() {
                        self.layout.toolbar = crate::ToolbarConfig::default();
                        changed = true;
                    }
                });

                // ---- Theme ----
                ui.collapsing("Theme", |ui| {
                    if ui.checkbox(&mut self.layout.theme.dark_mode, "Dark Mode").changed() { changed = true; }
                    changed |= ui.add(egui::Slider::new(&mut self.layout.theme.font_size, 8.0..=20.0)
                        .text("Font Size")).changed();
                    ui.horizontal(|ui| {
                        ui.label("Accent:");
                        changed |= ui.color_edit_button_srgb(&mut self.layout.theme.accent_color).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Selection:");
                        changed |= ui.color_edit_button_srgb(&mut self.layout.theme.selection_color).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Viewport BG:");
                        changed |= ui.color_edit_button_srgb(&mut self.layout.theme.viewport_bg).changed();
                    });
                    changed |= ui.add(egui::Slider::new(&mut self.layout.theme.panel_bg_alpha, 100..=255)
                        .text("Panel Opacity")).changed();
                    if ui.button("Reset Theme").clicked() {
                        self.layout.theme = crate::ThemeConfig::default();
                        changed = true;
                    }
                });

                ui.add_space(8.0);
                if ui.button("Reset All to Default").clicked() {
                    self.layout = crate::UiLayoutConfig::default();
                    changed = true;
                }

                if changed {
                    self.layout.active_preset = "Custom".into();
                    self.layout.save();
                }
            });
        if !open { self.ui.show_layout_settings = false; }
    }
}
