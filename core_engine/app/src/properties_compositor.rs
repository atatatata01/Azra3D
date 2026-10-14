//! Compositor stack sub-panel — extracted from properties_viewport.rs.

use eframe::egui;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_props_compositor(&mut self, ui: &mut egui::Ui) {
            // ---- Viewport Compositor Stack ----
            egui::CollapsingHeader::new(egui::RichText::new("\u{1F3AC} Compositor").strong().size(13.0).color(egui::Color32::from_rgb(200, 140, 255)))
                .default_open(false).show(ui, |ui| {
                // Master enable
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.compositor.enabled, "Enable Compositor").changed() {
                        self.transaction_snapshot_only("Toggle compositor");
                    }
                });
                // Presets
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("comp_presets").selected_text("\u{1F4BE} Load Preset").width(160.0).show_ui(ui, |ui| {
                        let presets = CompositorStack::builtin_presets();
                        let mut cat = String::new();
                        for p in &presets {
                            if p.category != cat {
                                cat = p.category.clone();
                                ui.separator();
                                ui.label(egui::RichText::new(&cat).strong().size(11.0));
                            }
                            if ui.button(&p.name).clicked() {
                                self.transaction_snapshot_only("Apply compositor preset");
                                self.compositor = p.stack.clone();
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        ui.label(egui::RichText::new("Custom").strong().size(11.0));
                        if ui.button("\u{1F4C2} Load from file...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                                if let Ok(preset) = CompositorStack::load_preset(&path) {
                                    self.transaction_snapshot_only("Load compositor preset");
                                    self.compositor = preset.stack;
                                }
                            }
                            ui.close_menu();
                        }
                    });
                    if ui.button("\u{1F4BE} Save").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name("compositor_preset.json").save_file() {
                            let _ = self.compositor.save_preset("Custom", "User", &path);
                        }
                    }
                    if ui.button("\u{1F4F7} Screenshot").clicked() {
                        self.commands.push(AppCommand::Screenshot);
                    }
                });
                ui.add_space(2.0);
                if self.compositor.enabled {
                    // Add effect combo
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("add_comp_effect").selected_text("+ Add Effect").width(160.0).show_ui(ui, |ui| {
                            for et in CompositorEffectType::all() {
                                if ui.button(format!("{} {}", et.icon(), et.label())).clicked() {
                                    self.transaction_snapshot_only("Add compositor effect");
                                    self.compositor.effects.push(CompositorEffect::new(*et));
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                    ui.add_space(4.0);

                    // Draw each effect in stack
                    let effect_count = self.compositor.effects.len();
                    let mut remove_idx: Option<usize> = None;
                    let mut swap_pair: Option<(usize, usize)> = None;
                    let mut solo_idx: Option<usize> = None;

                    for ei in 0..effect_count {
                        let et = self.compositor.effects[ei].effect_type;
                        let card_col = if self.compositor.effects[ei].enabled {
                            egui::Color32::from_rgba_unmultiplied(140, 100, 220, 20)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(80, 80, 80, 15)
                        };
                        egui::Frame::none().fill(card_col).inner_margin(4.0).rounding(4.0).show(ui, |ui| {
                            // Header: eye + solo + name + reorder + delete
                            ui.horizontal(|ui| {
                                // Eye toggle
                                let eye = if self.compositor.effects[ei].enabled { "\u{1F441}" } else { "\u{25CB}" };
                                if ui.small_button(eye).on_hover_text("Toggle").clicked() {
                                    self.transaction_snapshot_only("Toggle compositor effect");
                                    self.compositor.effects[ei].enabled = !self.compositor.effects[ei].enabled;
                                }
                                // Solo button
                                let solo_label = if self.compositor.effects[ei].solo { "S" } else { "s" };
                                if ui.small_button(solo_label).on_hover_text("Solo (Alt+click)").clicked() {
                                    solo_idx = Some(ei);
                                }
                                // Label
                                let expand_label = format!("{} {}", et.icon(), et.label());
                                if ui.selectable_label(self.compositor.effects[ei].expanded, expand_label).clicked() {
                                    self.compositor.effects[ei].expanded = !self.compositor.effects[ei].expanded;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").on_hover_text("Remove").clicked() { remove_idx = Some(ei); }
                                    if ei + 1 < effect_count { if ui.small_button("\u{25BC}").on_hover_text("Move down").clicked() { swap_pair = Some((ei, ei + 1)); } }
                                    if ei > 0 { if ui.small_button("\u{25B2}").on_hover_text("Move up").clicked() { swap_pair = Some((ei, ei - 1)); } }
                                });
                            });

                            // Per-effect parameters (when expanded)
                            if self.compositor.effects[ei].expanded && self.compositor.effects[ei].enabled {
                                match et {
                                    CompositorEffectType::Bloom => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].bloom_threshold, 0.0..=5.0).text("Threshold"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].bloom_intensity, 0.0..=10.0).text("Intensity"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].bloom_radius, 1.0..=100.0).text("Radius"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].bloom_tint).changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].bloom_anamorphic, -1.0..=1.0).text("Anamorphic"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].bloom_emission_only, "Emission Only").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::DepthOfField => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].dof_focus_dist, 0.1..=100.0).text("Focus Dist").logarithmic(true));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].dof_aperture, 1.0..=32.0).text("f-stop"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].dof_max_blur, 1.0..=50.0).text("Max Blur"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let shapes = ["Circle", "Hex", "Oct", "Anamorphic"];
                                        egui::ComboBox::from_id_salt(format!("dof_shape_{}", ei)).selected_text(shapes[self.compositor.effects[ei].dof_bokeh_shape as usize % 4]).show_ui(ui, |ui| {
                                            for (j, s) in shapes.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].dof_bokeh_shape == j, *s).clicked()
                                                    && self.compositor.effects[ei].dof_bokeh_shape != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].dof_bokeh_shape = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].dof_chromatic, 0.0..=1.0).text("Chromatic"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::MotionBlur => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].mblur_intensity, 0.0..=2.0).text("Intensity"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].mblur_samples, 4..=32).text("Samples"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].mblur_max_length, 1.0..=50.0).text("Max Length"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].mblur_shutter_angle, 0.0..=360.0).text("Shutter \u{00B0}"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::ChromaticAberration => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].ca_intensity, 0.0..=1.0).text("Intensity"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let modes = ["Radial", "Uniform", "Barrel"];
                                        egui::ComboBox::from_id_salt(format!("ca_mode_{}", ei)).selected_text(modes[self.compositor.effects[ei].ca_mode as usize % 3]).show_ui(ui, |ui| {
                                            for (j, m) in modes.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].ca_mode == j, *m).clicked()
                                                    && self.compositor.effects[ei].ca_mode != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].ca_mode = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                    }
                                    CompositorEffectType::Sharpen => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].sharpen_strength, 0.0..=5.0).text("Strength"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].sharpen_radius, 0.5..=10.0).text("Radius"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].sharpen_threshold, 0.0..=0.5).text("Threshold"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].sharpen_clarity, "Clarity Mode").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::LensDistortion => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].distort_barrel, -1.0..=1.0).text("Barrel"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].distort_chromatic, "Chromatic").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].distort_scale, 0.8..=1.2).text("Scale"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::Fog => {
                                        if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].fog_color).changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_density, 0.0..=1.0).text("Density"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_start, 0.0..=50.0).text("Start"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_end, 1.0..=200.0).text("End"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let modes = ["Linear", "Exponential", "Height"];
                                        egui::ComboBox::from_id_salt(format!("fog_mode_{}", ei)).selected_text(modes[self.compositor.effects[ei].fog_mode as usize % 3]).show_ui(ui, |ui| {
                                            for (j, m) in modes.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].fog_mode == j, *m).clicked()
                                                    && self.compositor.effects[ei].fog_mode != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].fog_mode = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        if self.compositor.effects[ei].fog_mode == 2 {
                                            let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_height_density, 0.0..=1.0).text("Height Density"));
                                            if r.changed() {
                                                if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                            let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_height_baseline, -10.0..=10.0).text("Baseline"));
                                            if r.changed() {
                                                if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].fog_scatter, 0.0..=1.0).text("Scatter"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::Outline => {
                                        let sources = ["Normal+Depth", "Normal", "Depth"];
                                        egui::ComboBox::from_id_salt(format!("ol_src_{}", ei)).selected_text(sources[self.compositor.effects[ei].outline_source as usize % 3]).show_ui(ui, |ui| {
                                            for (j, s) in sources.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].outline_source == j, *s).clicked()
                                                    && self.compositor.effects[ei].outline_source != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].outline_source = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].outline_width, 0.5..=5.0).text("Width"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let mut oc = egui::Color32::from_rgba_unmultiplied(
                                            (self.compositor.effects[ei].outline_color[0]*255.0) as u8,
                                            (self.compositor.effects[ei].outline_color[1]*255.0) as u8,
                                            (self.compositor.effects[ei].outline_color[2]*255.0) as u8,
                                            (self.compositor.effects[ei].outline_color[3]*255.0) as u8,
                                        );
                                        if ui.color_edit_button_srgba(&mut oc).changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.compositor.effects[ei].outline_color = [oc.r() as f32/255.0, oc.g() as f32/255.0, oc.b() as f32/255.0, oc.a() as f32/255.0];
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].outline_normal_thresh, 0.1..=1.0).text("Normal Thresh"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].outline_depth_thresh, 0.001..=0.5).text("Depth Thresh").logarithmic(true));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].outline_surface_id, "Surface ID Outlines").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let fills = ["Outline Only", "Flat Color", "Cel Shade"];
                                        egui::ComboBox::from_id_salt(format!("ol_fill_{}", ei)).selected_text(fills[self.compositor.effects[ei].outline_fill_mode as usize % 3]).show_ui(ui, |ui| {
                                            for (j, f) in fills.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].outline_fill_mode == j, *f).clicked()
                                                    && self.compositor.effects[ei].outline_fill_mode != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].outline_fill_mode = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        if self.compositor.effects[ei].outline_fill_mode == 2 {
                                            let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].outline_cel_steps, 2..=16).text("Cel Steps"));
                                            if r.changed() {
                                                if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        }
                                    }
                                    CompositorEffectType::Vignette => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].vignette_intensity, 0.0..=2.0).text("Intensity"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].vignette_color).changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let shapes = ["Circular", "Elliptical", "Rectangular"];
                                        egui::ComboBox::from_id_salt(format!("vig_shape_{}", ei)).selected_text(shapes[self.compositor.effects[ei].vignette_shape as usize % 3]).show_ui(ui, |ui| {
                                            for (j, s) in shapes.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].vignette_shape == j, *s).clicked()
                                                    && self.compositor.effects[ei].vignette_shape != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].vignette_shape = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].vignette_radius, 0.2..=1.5).text("Radius"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].vignette_softness, 0.1..=2.0).text("Softness"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::FilmGrain => {
                                        let noises = ["White", "Perlin", "Worley"];
                                        egui::ComboBox::from_id_salt(format!("grain_type_{}", ei)).selected_text(noises[self.compositor.effects[ei].grain_noise_type as usize % 3]).show_ui(ui, |ui| {
                                            for (j, n) in noises.iter().enumerate() {
                                                let j = j as u32;
                                                if ui.selectable_label(self.compositor.effects[ei].grain_noise_type == j, *n).clicked()
                                                    && self.compositor.effects[ei].grain_noise_type != j
                                                {
                                                    self.transaction_snapshot_only("Edit compositor");
                                                    self.compositor.effects[ei].grain_noise_type = j;
                                                    self.mark_dirty(DirtyFlags::RENDER);
                                                }
                                            }
                                        });
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grain_intensity, 0.0..=0.5).text("Intensity"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grain_size, 0.5..=5.0).text("Size"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].grain_monochromatic, "Monochromatic").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].grain_animate, "Animate").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        if ui.checkbox(&mut self.compositor.effects[ei].grain_luminance_respond, "Respond to Luminance").changed() {
                                            self.transaction_snapshot_only("Edit compositor");
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    }
                                    CompositorEffectType::ColorGrading => {
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_hue_shift, -180.0..=180.0).text("Hue Shift"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_saturation, 0.0..=3.0).text("Saturation"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_vibrance, 0.0..=2.0).text("Vibrance"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_brightness, -1.0..=1.0).text("Brightness"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_contrast, 0.0..=3.0).text("Contrast"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_gamma, 0.1..=5.0).text("Gamma").logarithmic(true));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_highlights, -1.0..=1.0).text("Highlights"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        let r = ui.add(egui::Slider::new(&mut self.compositor.effects[ei].grade_shadows, -1.0..=1.0).text("Shadows"));
                                        if r.changed() {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        ui.separator();
                                        ui.label("Color Balance");
                                        ui.horizontal(|ui| {
                                            ui.label("Shadows:");
                                            if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].grade_balance_shadows).changed() {
                                                self.transaction_snapshot_only("Edit compositor");
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("Midtones:");
                                            if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].grade_balance_midtones).changed() {
                                                self.transaction_snapshot_only("Edit compositor");
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("Highlights:");
                                            if ui.color_edit_button_rgb(&mut self.compositor.effects[ei].grade_balance_highlights).changed() {
                                                self.transaction_snapshot_only("Edit compositor");
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }
                                        });
                                    }
                                }
                            }

                            // ── Mask section (collapsible) ──
                            ui.separator();
                            let mask_count = self.compositor.effects[ei].masks.len();
                            let mask_label = if mask_count == 0 {
                                "Mask: None (affects all)".to_string()
                            } else {
                                format!("Mask: {} layer{}", mask_count, if mask_count == 1 { "" } else { "s" })
                            };

                            ui.horizontal(|ui| {
                                let arrow = if self.compositor.effects[ei].mask_expanded { "\u{25BC}" } else { "\u{25B6}" };
                                if ui.selectable_label(self.compositor.effects[ei].mask_expanded, format!("{} {}", arrow, mask_label)).clicked() {
                                    self.compositor.effects[ei].mask_expanded = !self.compositor.effects[ei].mask_expanded;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    // Mask preview eye
                                    let previewing = self.compositor.mask_preview_effect == Some(ei);
                                    let eye_col = if previewing { egui::Color32::GREEN } else { egui::Color32::GRAY };
                                    if ui.add(egui::Button::new(egui::RichText::new("\u{1F441}").color(eye_col)).frame(false))
                                        .on_hover_text("Preview mask (red/green overlay)")
                                        .clicked()
                                    {
                                        self.compositor.mask_preview_effect = if previewing { None } else { Some(ei) };
                                    }
                                });
                            });

                            if self.compositor.effects[ei].mask_expanded {
                                let mut mask_remove: Option<usize> = None;

                                for mi in 0..self.compositor.effects[ei].masks.len() {
                                    egui::Frame::none()
                                        .fill(egui::Color32::from_rgba_unmultiplied(60, 60, 80, 20))
                                        .inner_margin(3.0)
                                        .rounding(2.0)
                                        .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            // Combinator (skip for first mask)
                                            if mi > 0 {
                                                let ci = match &self.compositor.effects[ei].masks[mi].combinator {
                                                    MaskCombinator::Multiply => 0usize, MaskCombinator::Add => 1, MaskCombinator::Subtract => 2,
                                                };
                                                let comb_labels = ["Mul", "Add", "Sub"];
                                                egui::ComboBox::from_id_salt(format!("mask_comb_{}_{}", ei, mi))
                                                    .selected_text(comb_labels[ci])
                                                    .width(40.0)
                                                    .show_ui(ui, |ui| {
                                                        if ui.selectable_label(ci == 0, "Multiply").clicked() && ci != 0 {
                                                            self.transaction_snapshot_only("Edit compositor");
                                                            self.compositor.effects[ei].masks[mi].combinator = MaskCombinator::Multiply;
                                                            self.mark_dirty(DirtyFlags::RENDER);
                                                        }
                                                        if ui.selectable_label(ci == 1, "Add").clicked() && ci != 1 {
                                                            self.transaction_snapshot_only("Edit compositor");
                                                            self.compositor.effects[ei].masks[mi].combinator = MaskCombinator::Add;
                                                            self.mark_dirty(DirtyFlags::RENDER);
                                                        }
                                                        if ui.selectable_label(ci == 2, "Subtract").clicked() && ci != 2 {
                                                            self.transaction_snapshot_only("Edit compositor");
                                                            self.compositor.effects[ei].masks[mi].combinator = MaskCombinator::Subtract;
                                                            self.mark_dirty(DirtyFlags::RENDER);
                                                        }
                                                    });
                                            }

                                            // Source type label
                                            let src_label = match &self.compositor.effects[ei].masks[mi].source {
                                                CompositorMaskSource::None => "None",
                                                CompositorMaskSource::Noise3D { .. } => "3D Noise",
                                                CompositorMaskSource::SurfaceId { .. } => "Object ID",
                                                CompositorMaskSource::MaterialId { .. } => "Material ID",
                                                CompositorMaskSource::Texture { .. } => "Texture",
                                                CompositorMaskSource::SdfDistance { .. } => "SDF Distance",
                                                CompositorMaskSource::Curvature { .. } => "Curvature",
                                                CompositorMaskSource::Depth { .. } => "Depth",
                                                CompositorMaskSource::NormalDirection { .. } => "Normal",
                                                CompositorMaskSource::Fresnel { .. } => "Fresnel",
                                                CompositorMaskSource::Vignette { .. } => "Vignette",
                                            };
                                            ui.label(egui::RichText::new(src_label).strong().size(11.0));

                                            // Invert checkbox
                                            if ui.checkbox(&mut self.compositor.effects[ei].masks[mi].invert, "Inv").changed() {
                                                self.transaction_snapshot_only("Edit compositor");
                                                self.mark_dirty(DirtyFlags::RENDER);
                                            }

                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui.small_button("X").on_hover_text("Remove mask").clicked() {
                                                    mask_remove = Some(mi);
                                                }
                                            });
                                        });

                                        // Per-source parameters (deferred undo — destructured borrows prevent direct self calls)
                                        let mut mask_dirty = false;
                                        match &mut self.compositor.effects[ei].masks[mi].source {
                                            CompositorMaskSource::Noise3D { scale, octaves, persistence, lacunarity, contrast, seed } => {
                                                let r = ui.add(egui::Slider::new(scale, 0.1..=20.0).text("Scale"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(octaves, 1..=8).text("Octaves"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(persistence, 0.0..=1.0).text("Persistence"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(lacunarity, 1.0..=4.0).text("Lacunarity"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(contrast, 0.0..=5.0).text("Contrast"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::DragValue::new(seed).prefix("Seed: "));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::SurfaceId { ids, invert: _ } => {
                                                ui.label(format!("Object IDs: {:?}", ids));
                                                ui.horizontal(|ui| {
                                                    ui.label("Add ID:");
                                                    let id_resp = ui.add(egui::DragValue::new(&mut 0u32).range(0..=9999));
                                                    if id_resp.changed() {
                                                        // User can type an ID to add
                                                    }
                                                });
                                            }
                                            CompositorMaskSource::MaterialId { ids, invert: _ } => {
                                                ui.label(format!("Material IDs: {:?}", ids));
                                            }
                                            CompositorMaskSource::Texture { slot, channel, invert: _ } => {
                                                let r = ui.add(egui::DragValue::new(slot).prefix("Slot: ").range(0..=15));
                                                if r.changed() { mask_dirty = true; }
                                                let ch_labels = ["R", "G", "B", "A"];
                                                egui::ComboBox::from_id_salt(format!("tex_ch_{}_{}", ei, mi))
                                                    .selected_text(ch_labels[*channel as usize % 4])
                                                    .show_ui(ui, |ui| {
                                                        for (j, l) in ch_labels.iter().enumerate() {
                                                            if ui.selectable_label(*channel == j as u32, *l).clicked()
                                                                && *channel != j as u32
                                                            {
                                                                *channel = j as u32;
                                                                mask_dirty = true;
                                                            }
                                                        }
                                                    });
                                            }
                                            CompositorMaskSource::SdfDistance { near, far, invert: _ } => {
                                                let r = ui.add(egui::Slider::new(near, 0.0..=10.0).text("Near"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(far, 0.1..=50.0).text("Far"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::Curvature { min_k, max_k, invert: _ } => {
                                                let r = ui.add(egui::Slider::new(min_k, -5.0..=0.0).text("Min K"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(max_k, 0.0..=5.0).text("Max K"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::Depth { near, far, invert: _ } => {
                                                let r = ui.add(egui::Slider::new(near, 0.0..=10.0).text("Near"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(far, 1.0..=200.0).text("Far"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::NormalDirection { threshold, invert: _ } => {
                                                let r = ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::Fresnel { power, invert: _ } => {
                                                let r = ui.add(egui::Slider::new(power, 0.1..=10.0).text("Power"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::Vignette { radius, softness } => {
                                                let r = ui.add(egui::Slider::new(radius, 0.1..=2.0).text("Radius"));
                                                if r.changed() { mask_dirty = true; }
                                                let r = ui.add(egui::Slider::new(softness, 0.01..=2.0).text("Softness"));
                                                if r.changed() { mask_dirty = true; }
                                            }
                                            CompositorMaskSource::None => {}
                                        }
                                        // Episode only when a mask slider actually changed.
                                        if mask_dirty {
                                            if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                    });
                                    ui.add_space(1.0);
                                }

                                if let Some(ri) = mask_remove {
                                    self.transaction_snapshot_only("Remove compositor mask");
                                    self.compositor.effects[ei].masks.remove(ri);
                                }

                                // "+ Add Mask" button with source type selector
                                ui.horizontal(|ui| {
                                    if ui.button("+ Add Mask").clicked() { self.transaction_snapshot_only("Add compositor mask");
                                        // Default to Depth mask as a sensible starting point
                                        self.compositor.effects[ei].masks.push(CompositorMaskLayer {
                                            source: CompositorMaskSource::None,
                                            combinator: MaskCombinator::Multiply,
                                            invert: false,
                                        });
                                    }
                                    // Quick-add type selector for the last mask
                                    if !self.compositor.effects[ei].masks.is_empty() {
                                        let last = self.compositor.effects[ei].masks.len() - 1;
                                        let is_none = matches!(self.compositor.effects[ei].masks[last].source, CompositorMaskSource::None);
                                        if is_none {
                                            egui::ComboBox::from_id_salt(format!("mask_type_{}_{}", ei, last))
                                                .selected_text("Select type...")
                                                .show_ui(ui, |ui| {
                                                    if ui.selectable_label(false, "Object ID").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::SurfaceId { ids: Vec::new(), invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Material ID").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::MaterialId { ids: Vec::new(), invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "3D Noise").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Noise3D { scale: 5.0, octaves: 3, persistence: 0.5, lacunarity: 2.0, contrast: 1.0, seed: 0 };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Texture").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Texture { slot: 0, channel: 0, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "SDF Distance").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::SdfDistance { near: 0.0, far: 5.0, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Depth").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Depth { near: 0.0, far: 50.0, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Curvature").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Curvature { min_k: -1.0, max_k: 1.0, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Normal").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::NormalDirection { threshold: 0.5, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Fresnel").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Fresnel { power: 2.0, invert: false };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                    if ui.selectable_label(false, "Vignette").clicked() {
                                                        self.transaction_snapshot_only("Edit compositor");
                                                        self.compositor.effects[ei].masks[last].source = CompositorMaskSource::Vignette { radius: 0.8, softness: 0.5 };
                                                        self.mark_dirty(DirtyFlags::RENDER);
                                                    }
                                                });
                                        }
                                    }
                                });
                            }
                        });
                        ui.add_space(2.0);
                    }

                    // Apply solo toggle
                    if let Some(si) = solo_idx {
                        self.transaction_snapshot_only("Solo compositor effect");
                        let was_solo = self.compositor.effects[si].solo;
                        // Clear all solos first
                        for e in &mut self.compositor.effects { e.solo = false; }
                        if !was_solo { self.compositor.effects[si].solo = true; }
                    }
                    // Apply remove / swap
                    if let Some(ri) = remove_idx { self.transaction_snapshot_only("Remove compositor effect"); self.compositor.effects.remove(ri); }
                    if let Some((a, b)) = swap_pair { self.transaction_snapshot_only("Reorder compositor effect"); self.compositor.effects.swap(a, b); }

                    // Color Management section (always last)
                    ui.add_space(4.0); ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(200, 200, 200), "Color Management");
                    let tm_labels = ["ACES", "Filmic", "AgX", "Reinhard", "PBR Neutral", "None"];
                    egui::ComboBox::from_id_salt("tonemapper").selected_text(tm_labels[self.compositor.tonemapper as usize % 6]).show_ui(ui, |ui| {
                        for (j, l) in tm_labels.iter().enumerate() {
                            let j = j as u32;
                            if ui.selectable_label(self.compositor.tonemapper == j, *l).clicked()
                                && self.compositor.tonemapper != j
                            {
                                self.transaction_snapshot_only("Edit compositor");
                                self.compositor.tonemapper = j;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        }
                    });
                    let r = ui.add(egui::Slider::new(&mut self.compositor.cm_exposure, -10.0..=10.0).text("Exposure EV"));
                    if r.changed() {
                        if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui.checkbox(&mut self.compositor.cm_auto_exposure, "Auto Exposure").changed() {
                        self.transaction_snapshot_only("Edit compositor");
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let r = ui.add(egui::Slider::new(&mut self.compositor.cm_white_balance_temp, 2000.0..=12000.0).text("Temp K"));
                    if r.changed() {
                        if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let r = ui.add(egui::Slider::new(&mut self.compositor.cm_white_balance_tint, -1.0..=1.0).text("Tint"));
                    if r.changed() {
                        if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let r = ui.add(egui::Slider::new(&mut self.compositor.lut_intensity, 0.0..=1.0).text("LUT Intensity"));
                    if r.changed() {
                        if !self.has_active_episode() { self.begin_episode("Edit compositor"); }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            });

    }
}
