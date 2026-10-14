// win_grid_workplane.rs — Grid settings + work plane settings windows
// Extracted from ui_windows.rs
use crate::ui_menus::DefaultMenu;
use eframe::egui;
use crate::*;

impl AzraApp {
    // ---- Grid Settings Window ----
    pub(crate) fn draw_grid_settings(&mut self, ctx: &egui::Context) {
        if !self.viewport.show_grid_settings { return; }
        let mut open = true;
        egui::Window::new("Grid Settings")
            .open(&mut open)
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.viewport.grid_visible, "Show Grid")
.default_menu("viewport.grid_visible", serde_json::json!(self.viewport.grid_visible));
                ui.add_space(4.0);
                // Grid mode selector
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    if ui.selectable_label(self.viewport.grid_mode == 0, "Wire").clicked() {
                        self.viewport.grid_mode = 0;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui.selectable_label(self.viewport.grid_mode == 1, "Shadow Catcher").clicked() {
                        self.viewport.grid_mode = 1;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
                ui.add_space(4.0);
                ui.add_enabled_ui(self.viewport.grid_visible, |ui| {
                    // Scale
                    ui.label(egui::RichText::new("Scale").strong().small());
                    ui.add(egui::Slider::new(&mut self.viewport.grid_cell_size, 0.1..=5.0)
                        .text("Cell Size")
                        .logarithmic(true));
                    ui.add(egui::Slider::new(&mut self.viewport.grid_subdivisions, 1..=10)
                        .text("Subdivisions"));
                    ui.add_space(4.0);

                    // Floor height
                    ui.label(egui::RichText::new("Position").strong().small());
                    let r = ui.add(egui::Slider::new(&mut self.viewport.grid_floor_y, -5.0..=5.0)
                        .text("Floor Y"));
                    if r.changed() { self.mark_dirty(DirtyFlags::RENDER); }
                    ui.add_space(4.0);

                    // Color
                    ui.label(egui::RichText::new("Color").strong().small());
                    ui.add(egui::Slider::new(&mut self.viewport.grid_opacity, 0.0..=1.0).text("Opacity"));
                    ui.horizontal(|ui| {
                        ui.label("Lines:");
                        let mut c = egui::Color32::from_rgb(
                            (self.viewport.grid_color[0] * 255.0) as u8,
                            (self.viewport.grid_color[1] * 255.0) as u8,
                            (self.viewport.grid_color[2] * 255.0) as u8,
                        );
                        if ui.color_edit_button_srgba(&mut c).changed() {
                            self.viewport.grid_color = [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
                        }
                    });
                    ui.checkbox(&mut self.viewport.grid_show_axes, "Show Axis Lines")
.default_menu("viewport.grid_show_axes", serde_json::json!(self.viewport.grid_show_axes));
                    if self.viewport.grid_show_axes {
                        ui.horizontal(|ui| {
                            ui.label("  X:");
                            let mut cx = egui::Color32::from_rgb(
                                (self.viewport.grid_axis_x_color[0] * 255.0) as u8,
                                (self.viewport.grid_axis_x_color[1] * 255.0) as u8,
                                (self.viewport.grid_axis_x_color[2] * 255.0) as u8,
                            );
                            if ui.color_edit_button_srgba(&mut cx).changed() {
                                self.viewport.grid_axis_x_color = [cx.r() as f32 / 255.0, cx.g() as f32 / 255.0, cx.b() as f32 / 255.0];
                            }
                            ui.label("  Z:");
                            let mut cz = egui::Color32::from_rgb(
                                (self.viewport.grid_axis_z_color[0] * 255.0) as u8,
                                (self.viewport.grid_axis_z_color[1] * 255.0) as u8,
                                (self.viewport.grid_axis_z_color[2] * 255.0) as u8,
                            );
                            if ui.color_edit_button_srgba(&mut cz).changed() {
                                self.viewport.grid_axis_z_color = [cz.r() as f32 / 255.0, cz.g() as f32 / 255.0, cz.b() as f32 / 255.0];
                            }
                        });
                    }
                    ui.add_space(4.0);

                    // Snapping (synced with viewport magnet bar — one mental model)
                    ui.label(egui::RichText::new("Snapping").strong().small());
                    let mut snap_on = self.viewport.snap_enabled || self.viewport.smart_snap_on;
                    if ui.checkbox(&mut snap_on, "Enable Snap").changed() {
                        self.set_smart_snap_on(snap_on);
                    }
                    ui.add_enabled_ui(self.viewport.snap_enabled || self.viewport.smart_snap_on, |ui| {
                        ui.add(egui::Slider::new(&mut self.viewport.snap_grid, 0.01..=2.0)
                            .text("Grid Size").logarithmic(true));
                    });
                    ui.add(egui::Slider::new(&mut self.viewport.snap_rotation_deg, 1.0..=90.0)
                        .text("Rotation Snap (deg)"));
                    ui.horizontal_wrapped(|ui| {
                        for &deg in &[5.0f32, 10.0, 15.0, 30.0, 45.0, 90.0] {
                            let label = format!("{}°", deg as i32);
                            if ui.selectable_label((self.viewport.snap_rotation_deg - deg).abs() < 0.01, &label).clicked() {
                                self.viewport.snap_rotation_deg = deg;
                            }
                        }
                    });
                    ui.weak("Hold Shift during rotation drag to snap");
                    ui.add_space(4.0);

                    // Presets
                    ui.label(egui::RichText::new("Presets").strong().small());
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Dark").clicked() {
                            self.viewport.grid_color = [0.35, 0.35, 0.38];
                            self.viewport.grid_opacity = 0.6;
                        }
                        if ui.button("Light").clicked() {
                            self.viewport.grid_color = [0.6, 0.6, 0.65];
                            self.viewport.grid_opacity = 0.4;
                        }
                        if ui.button("Subtle").clicked() {
                            self.viewport.grid_color = [0.3, 0.3, 0.32];
                            self.viewport.grid_opacity = 0.25;
                        }
                        if ui.button("Bold").clicked() {
                            self.viewport.grid_color = [0.5, 0.5, 0.55];
                            self.viewport.grid_opacity = 0.9;
                        }
                    });
                });
            });
        if !open { self.viewport.show_grid_settings = false; }
    }

    // ---- Work Plane Settings Window ----
    pub(crate) fn draw_workplane_settings(&mut self, ctx: &egui::Context) {
        if !self.viewport.wp_show_settings { return; }
        let mut open = true;
        egui::Window::new("Work Plane Settings")
            .open(&mut open)
            .resizable(false)
            .default_width(260.0)
            .show(ctx, |ui| {
                // Mode indicator
                let mode_lbl = match self.viewport.wp_mode {
                    WorkPlaneMode::Auto   => "Auto (camera-based)",
                    WorkPlaneMode::Locked => "Locked (surface)",
                };
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.strong(mode_lbl);
                });
                if self.viewport.wp_mode == WorkPlaneMode::Locked {
                    if ui.button("Reset to Auto").clicked() {
                        self.reset_workplane();
                    }
                }
                ui.add_space(4.0);

                // Orientation
                ui.label(egui::RichText::new("Orientation").strong().small());
                let mut is_axis = self.viewport.wp_settings.orientation == WorkPlaneOrientation::AxisSnap;
                if ui.radio_value(&mut is_axis, true, "Axis-Snap (XY/XZ/YZ)").changed() {
                    self.viewport.wp_settings.orientation = WorkPlaneOrientation::AxisSnap;
                }
                if ui.radio_value(&mut is_axis, false, "Camera-Facing").changed() {
                    self.viewport.wp_settings.orientation = WorkPlaneOrientation::CameraFacing;
                }
                ui.add_space(4.0);

                // Spacing
                ui.label(egui::RichText::new("Appearance").strong().small());
                ui.add(egui::Slider::new(&mut self.viewport.wp_settings.spacing, 0.5..=10.0)
                    .text("Spacing")
                    .logarithmic(true));
                ui.add(egui::Slider::new(&mut self.viewport.wp_settings.opacity, 0.0..=1.0)
                    .text("Opacity"));
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut c = egui::Color32::from_rgb(
                        (self.viewport.wp_settings.color[0] * 255.0) as u8,
                        (self.viewport.wp_settings.color[1] * 255.0) as u8,
                        (self.viewport.wp_settings.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        self.viewport.wp_settings.color = [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
                    }
                });
                ui.add_space(4.0);

                // Origin
                ui.label(egui::RichText::new("Origin").strong().small());
                ui.add(egui::DragValue::new(&mut self.viewport.wp_settings.origin.x).speed(0.05).prefix("X: "));
                ui.add(egui::DragValue::new(&mut self.viewport.wp_settings.origin.y).speed(0.05).prefix("Y: "));
                ui.add(egui::DragValue::new(&mut self.viewport.wp_settings.origin.z).speed(0.05).prefix("Z: "));
                ui.add_space(4.0);

                // Saved workplanes
                ui.label(egui::RichText::new("Saved Planes").strong().small());
                let mut load_idx: Option<usize> = None;
                let mut del_idx: Option<usize> = None;
                for (i, saved) in self.viewport.wp_saved.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("Load").clicked() { load_idx = Some(i); }
                        if ui.small_button("X").clicked() { del_idx = Some(i); }
                        ui.label(&saved.name);
                    });
                }
                if let Some(i) = load_idx { self.load_workplane(i); }
                if let Some(i) = del_idx { self.viewport.wp_saved.remove(i); }
                ui.horizontal(|ui| {
                    if ui.button("Save Current").clicked() {
                        let n = self.viewport.wp_saved.len() + 1;
                        self.save_workplane(format!("Plane {}", n));
                    }
                });
                ui.add_space(4.0);

                // Reset defaults
                if ui.button("Reset Defaults").clicked() {
                    self.viewport.wp_settings = WorkPlaneSettings::default();
                    self.reset_workplane();
                }
            });
        if !open { self.viewport.wp_show_settings = false; }
    }
}
