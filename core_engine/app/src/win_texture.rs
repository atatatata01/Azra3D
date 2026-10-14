// win_texture.rs — Texture generator + AI texture window
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;
use crate::material_dict;
use crate::claude::{ClaudeBridge, ClaudeKind, ClaudeRequest};
use core_math::texgen::{self, TexGenConfig, TexPattern};

impl FourthLayerApp {
    // ================================================================
    // 8. Texture Generator (procedural)
    // ================================================================
    pub(crate) fn draw_texture_generator(&mut self, ctx: &egui::Context) {
        if !self.ui.show_texgen_window { return; }
        let mut do_generate = false;
        let mut texgen_toast: Option<String> = None;
        egui::Window::new("\u{1f3a8} Texture Generator")
            .collapsible(false).resizable(false).default_width(380.0)
            .open(&mut self.ui.show_texgen_window).show(ctx, |ui| {

            ui.heading("Procedural PBR Maps");
            ui.weak("Generates tileable Albedo, Normal, Roughness, Metalness, Height");
            ui.add_space(6.0);

            // Preset selector
            ui.horizontal_wrapped(|ui| {
                ui.label("Preset:");
                for &(name, label) in TexGenConfig::preset_names() {
                    if ui.selectable_label(self.ui.texgen_preset_name == name, label).clicked() {
                        self.ui.texgen_preset_name = name.into();
                        self.ui.texgen_config = TexGenConfig::from_preset(name);
                    }
                }
            });
            ui.add_space(4.0);

            // Resolution
            ui.horizontal(|ui| {
                ui.label("Resolution:");
                for &(r, label) in &[(512u32, "512"), (1024, "1K"), (2048, "2K"), (4096, "4K")] {
                    if ui.selectable_label(self.ui.texgen_config.width == r, label).clicked() {
                        self.ui.texgen_config.width = r;
                        self.ui.texgen_config.height = r;
                    }
                }
            });
            let mpx = (self.ui.texgen_config.width as f64 * self.ui.texgen_config.height as f64) / 1_000_000.0;
            ui.weak(format!("{}\u{d7}{} ({:.1} Mpx) \u{2014} 5 TGA maps", self.ui.texgen_config.width, self.ui.texgen_config.height, mpx));
            ui.add_space(4.0);

            // Pattern-specific sliders
            match &mut self.ui.texgen_config.pattern {
                TexPattern::Scratches { density, depth, direction, length, width } => {
                    ui.add(egui::Slider::new(density, 0.1..=1.0).text("Density"));
                    ui.add(egui::Slider::new(depth, 0.0..=1.0).text("Depth"));
                    ui.add(egui::Slider::new(direction, 0.0..=180.0).text("Direction \u{b0}"));
                    ui.add(egui::Slider::new(length, 0.1..=1.0).text("Length"));
                    ui.add(egui::Slider::new(width, 0.001..=0.02).text("Width"));
                }
                TexPattern::Rust { amount, pitting, color_var } => {
                    ui.add(egui::Slider::new(amount, 0.0..=1.0).text("Rust Coverage"));
                    ui.add(egui::Slider::new(pitting, 0.0..=1.0).text("Pitting Depth"));
                    ui.add(egui::Slider::new(color_var, 0.0..=1.0).text("Color Variation"));
                }
                TexPattern::BrushedMetal { direction, fineness, depth } => {
                    ui.add(egui::Slider::new(direction, 0.0..=180.0).text("Brush Direction \u{b0}"));
                    ui.add(egui::Slider::new(fineness, 1.0..=50.0).text("Fineness"));
                    ui.add(egui::Slider::new(depth, 0.0..=1.0).text("Groove Depth"));
                }
                TexPattern::Noise { octaves, scale, contrast } => {
                    let mut oct_f = *octaves as f32;
                    if ui.add(egui::Slider::new(&mut oct_f, 1.0..=8.0).text("Octaves")).changed() {
                        *octaves = oct_f as u32;
                    }
                    ui.add(egui::Slider::new(scale, 1.0..=20.0).text("Scale"));
                    ui.add(egui::Slider::new(contrast, 0.5..=2.0).text("Contrast"));
                }
                TexPattern::VoronoiCracks { cell_scale, edge_width, depth } => {
                    ui.add(egui::Slider::new(cell_scale, 2.0..=20.0).text("Cell Scale"));
                    ui.add(egui::Slider::new(edge_width, 0.01..=0.1).text("Crack Width"));
                    ui.add(egui::Slider::new(depth, 0.0..=1.0).text("Crack Depth"));
                }
                TexPattern::WornPaint { wear, chip_scale, .. } => {
                    ui.add(egui::Slider::new(wear, 0.0..=1.0).text("Wear Amount"));
                    ui.add(egui::Slider::new(chip_scale, 1.0..=10.0).text("Chip Scale"));
                }
            }
            ui.add_space(4.0);

            // Common controls
            ui.add(egui::Slider::new(&mut self.ui.texgen_config.normal_strength, 0.5..=3.0).text("Normal Strength"));

            // Base color
            let mut c = self.ui.texgen_config.base_color;
            ui.horizontal(|ui| {
                ui.label("Base Color:");
                if ui.color_edit_button_rgb(&mut c).changed() {
                    self.ui.texgen_config.base_color = c;
                }
                ui.add(egui::Slider::new(&mut self.ui.texgen_config.base_roughness, 0.0..=1.0).text("Roughness"));
            });
            ui.add(egui::Slider::new(&mut self.ui.texgen_config.base_metalness, 0.0..=1.0).text("Metalness"));
            ui.add_space(4.0);

            // Seed
            ui.horizontal(|ui| {
                ui.label("Seed:");
                ui.add(egui::DragValue::new(&mut self.ui.texgen_config.seed).speed(1));
                if ui.button("\u{1f3b2} Random").clicked() {
                    self.ui.texgen_config.seed = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() & 0xFFFFFFFF) as u32;
                }
            });
            ui.add_space(8.0);

            // Generate button
            if ui.button("\u{1f4e6} Generate & Export PBR Maps").clicked() {
                do_generate = true;
            }

            // Status
            if let Some(ref status) = self.ui.texgen_status {
                ui.add_space(4.0);
                ui.colored_label(
                    if status.starts_with('\u{2705}') { egui::Color32::from_rgb(100, 220, 100) }
                    else { egui::Color32::from_rgb(255, 180, 80) },
                    status.as_str()
                );
            }
        });

        if do_generate {
            let t0 = std::time::Instant::now();
            let maps = texgen::generate_pbr_maps(&self.ui.texgen_config);
            let elapsed = t0.elapsed();

            let prefix = format!("fl_{}_{}", self.ui.texgen_preset_name, self.ui.texgen_config.seed);

            match texgen::export_pbr_maps(&maps, &prefix) {
                Ok(paths) => {
                    self.ui.texgen_status = Some(format!(
                        "\u{2705} {} maps exported ({:.1}s)", paths.len(), elapsed.as_secs_f32()
                    ));
                    texgen_toast = Some(format!("\u{2705} {} PBR maps exported as {}_*.tga", paths.len(), prefix));
                }
                Err(e) => {
                    self.ui.texgen_status = Some(format!("\u{274c} {}", e));
                }
            }
        }
        if let Some(msg) = texgen_toast { self.toast(&msg); }
    }

    // ================================================================
    // 9. AI Texture Window (Stable Diffusion) -- the largest block
    // ================================================================
    pub(crate) fn draw_ai_texture_window(&mut self, ctx: &egui::Context) {
        if !self.ai.show_ai_tex_window { return; }
        let mut do_generate_ai = false;
        let mut do_stop_ai = false;
        let mut do_apply = false;
        let mut do_clear = false;
        let mut do_load = false;
        let mut do_save_all = false;
        let mut regen_map: Option<usize> = None;
        let mut save_map: Option<usize> = None;
        let mut do_rate: Option<u32> = None;      // Some(1..=4)
        let mut do_switch_model: Option<String> = None;
        let mut do_enhance: bool = false;
        let mut do_describe_image: Option<String> = None; // Some(base64 image)
        let mut do_pull_slots = false;
        let mut ai_toast: Option<String> = None;

        let ai_exclusive = self.ai.ai_gpu_exclusive();
        let is_flux_model = self.ai.ai_tex_model.contains("flux");

        egui::Window::new("\u{1f9e0} AI Texture (Azra Pipeline)")
            .collapsible(false).resizable(true).default_width(460.0)
            .open(&mut self.ai.show_ai_tex_window).show(ctx, |ui| {

            ui.heading("AI-Generated PBR Texture");
            ui.add_space(4.0);

            // -- Model: Flux.1 Kontext first (local ComfyUI weights), then SDXL fallbacks --
            let model_labels = [
                ("flux-kontext",      "Flux.1 Kontext \u{2605} PRIMARY",         "LOCAL ComfyUI transformer \u{00b7} best for materials + PT beauty context"),
                ("cyberrealistic-xl", "CyberRealistic XL v6 LOCAL",              "LOCAL SDXL \u{00b7} hard-surface fallback (lower VRAM)"),
                ("juggernaut-xl",     "Juggernaut XL Ragnarok LOCAL",             "LOCAL ComfyUI \u{00b7} general product quality"),
                ("fiamix-xl",         "FiaMix XL LOCAL",                          "LOCAL ComfyUI \u{00b7} strong XL mix"),
                ("dreamshaper-xl",    "DreamShaper XL Turbo LOCAL",               "LOCAL \u{00b7} fast turbo (lower max quality)"),
                ("sdxl-base",         "SDXL Base 1.0 LOCAL",                      "LOCAL stock XL (fine-tunes above are better)"),
                ("dreamshaper-8",     "DreamShaper 8 LOCAL",                      "LOCAL SD1.5 \u{00b7} weaker than your XL models"),
                ("realvis-xl",        "RealVisXL V4 (download)",                  "Optional HF download \u{2014} similar class to CyberRealistic XL"),
                ("epicrealism",       "epiCRealism (download)",                   "Optional HF SD1.5 photoreal"),
            ];
            let cur_label = model_labels.iter()
                .find(|m| m.0 == self.ai.ai_tex_model)
                .map(|m| m.1)
                .unwrap_or(&self.ai.ai_tex_model);

            ui.collapsing(format!("\u{1f916} Model: {}", cur_label), |ui| {
                for (key, label, desc) in &model_labels {
                    ui.horizontal(|ui| {
                        let selected = self.ai.ai_tex_model == *key;
                        if ui.selectable_label(selected, *label).clicked() && !selected {
                            do_switch_model = Some(key.to_string());
                        }
                        ui.weak(*desc);
                    });
                }
                ui.add_space(2.0);
                ui.weak("Flux.1 Kontext uses your local ComfyUI weights (no re-download of the 22GB transformer). Same model drives AI Texture + PT Neural Beauty. First load can take 1\u{2013}3 min.");
            });
            ui.add_space(4.0);

            // GPU priority banner when AI job is live
            if ai_exclusive {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 180, 60),
                    "AI owns GPU — Path Trace / CUDA paused until job finishes",
                );
            }

            // -- Quality Preset (Flux-first: fewer steps, capped res for 4090) --
            ui.horizontal(|ui| {
                ui.label("Quality:");
                let is_flux = is_flux_model;
                let presets: [(&str, &str, &str, u32, u32); 4] = if is_flux {
                    [
                        ("preview",  "Preview",  "768px \u{b7} 10 steps \u{b7} Flux fast", 768, 10),
                        ("standard", "Standard", "1024px \u{b7} 16 steps \u{b7} Flux", 1024, 16),
                        ("final",    "Final",    "1024px \u{b7} 24 steps \u{b7} Flux quality", 1024, 24),
                        ("ultra",    "Ultra",    "1280px \u{b7} 28 steps \u{b7} Flux max", 1280, 28),
                    ]
                } else {
                    [
                        ("preview",  "Preview",  "768px \u{b7} 12 steps \u{b7} ~5s", 768, 12),
                        ("standard", "Standard", "1024px \u{b7} 18 steps \u{b7} ~10s on 4090", 1024, 18),
                        ("final",    "Final",    "1024px \u{b7} 24 steps \u{b7} quality", 1024, 24),
                        ("ultra",    "Ultra",    "1280px \u{b7} 28 steps \u{b7} max", 1280, 28),
                    ]
                };
                for (key, label, hint, size, steps) in &presets {
                    if ui.selectable_label(self.ai.ai_tex_quality == *key, *label)
                        .on_hover_text(*hint).clicked()
                    {
                        self.ai.ai_tex_quality = (*key).to_string();
                        self.ai.ai_tex_size = *size;
                        self.ai.ai_tex_steps = *steps;
                        if is_flux {
                            self.ai.ai_tex_guidance = 2.5;
                        }
                    }
                }
                ui.separator();
                ui.label("Seed:");
                ui.add(egui::DragValue::new(&mut self.ai.ai_tex_seed).speed(1));
                if ui.button("\u{1f3b2}").clicked() {
                    self.ai.ai_tex_seed = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() & 0xFFFFFFFF) as u32;
                }
            });

            // Manual overrides (collapsed by default)
            ui.collapsing("\u{2699} Advanced (override preset)", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Size:");
                    for &s in &[512u32, 768, 1024, 1536, 2048] {
                        if ui.selectable_label(self.ai.ai_tex_size == s, format!("{}",s)).clicked() {
                            self.ai.ai_tex_size = s;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.ai.ai_tex_steps, 5..=60).text("Steps"));
                    ui.add(egui::Slider::new(&mut self.ai.ai_tex_guidance, 1.0..=15.0).text("CFG"));
                });
                ui.add(egui::Slider::new(&mut self.ai.ai_tex_tiling, 0.5..=8.0).text("Viewport Tiling"));
            });
            ui.add_space(4.0);

            // == Per-object prompts (node workflow) ==
            ui.collapsing("\u{1f4e6} Per-object prompts (mesh-01 leather, mesh-02 steel\u{2026})", |ui| {
                ui.checkbox(&mut self.ai.ai_tex_multi_object, "Generate one PBR set per object")
                    .on_hover_text("When on, Generate uses each slot's prompt (for multi-material assemblies).");
                ui.horizontal(|ui| {
                    if ui.button("\u{2795} From selection").on_hover_text(
                        "Pull selected scene nodes into slots (names become prompts labels)"
                    ).clicked() {
                        do_pull_slots = true;
                    }
                    if ui.button("\u{2795} Empty slot").clicked() {
                        let n = self.ai.ai_tex_object_slots.len() + 1;
                        self.ai.ai_tex_object_slots.push(crate::AiObjectPromptSlot {
                            id: format!("mesh-{:02}", n),
                            name: format!("Mesh {:02}", n),
                            prompt: String::new(),
                            node_id: None,
                        });
                    }
                    if ui.button("Clear slots").clicked() {
                        self.ai.ai_tex_object_slots.clear();
                    }
                });
                let mut remove_i: Option<usize> = None;
                for (i, slot) in self.ai.ai_tex_object_slots.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&slot.id).monospace().strong());
                        ui.add(egui::TextEdit::singleline(&mut slot.name).desired_width(100.0)
                            .hint_text("name"));
                        ui.add(egui::TextEdit::singleline(&mut slot.prompt).desired_width(200.0)
                            .hint_text("material prompt e.g. worn leather strap"));
                        if ui.small_button("\u{2715}").clicked() { remove_i = Some(i); }
                    });
                }
                if let Some(i) = remove_i {
                    self.ai.ai_tex_object_slots.remove(i);
                }
                if self.ai.ai_tex_object_slots.is_empty() {
                    ui.weak("No slots — select meshes and click From selection, or add empty slots.");
                }
            });
            ui.add_space(4.0);

            // == Smart Prompt System ==
            ui.horizontal(|ui| {
                ui.strong("Prompt");
                ui.separator();
                // Browse button
                let browse_icon = if self.ai.ai_tex_show_browser { "\u{1f4cb}\u{25b2}" } else { "\u{1f4cb} Browse" };
                if ui.small_button(browse_icon).on_hover_text("Browse material categories").clicked() {
                    self.ai.ai_tex_show_browser = !self.ai.ai_tex_show_browser;
                }
            });

            // -- Material Browser (collapsible) --
            if self.ai.ai_tex_show_browser {
                egui::Frame::dark_canvas(ui.style()).inner_margin(6.0).show(ui, |ui| {
                    // Category tabs
                    ui.horizontal_wrapped(|ui| {
                        for (i, cat) in material_dict::CATEGORIES.iter().enumerate() {
                            let col = material_dict::category_color(cat);
                            let selected = self.ai.ai_tex_browser_cat == i;
                            let label = egui::RichText::new(*cat)
                                .color(if selected { egui::Color32::WHITE } else { col })
                                .strong();
                            if ui.selectable_label(selected, label).clicked() {
                                self.ai.ai_tex_browser_cat = i;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    // Entries for selected category
                    let cat = material_dict::CATEGORIES[self.ai.ai_tex_browser_cat];
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        for entry in material_dict::MATERIAL_DICTIONARY.iter().filter(|e| e.category == cat) {
                            ui.horizontal(|ui| {
                                let color = material_dict::category_color(cat);
                                ui.colored_label(color, "\u{25cf}");
                                let clicked = ui.vertical(|ui| {
                                    let n = ui.selectable_label(false,
                                        egui::RichText::new(entry.name).strong());
                                    ui.label(egui::RichText::new(entry.hint).small()
                                        .color(egui::Color32::from_rgb(130, 130, 130)));
                                    n.clicked()
                                }).inner;
                                if clicked {
                                    self.ai.ai_tex_prompt = entry.prompt_fragment.to_string();
                                    self.ai.ai_tex_ac_matches.clear();
                                    self.ai.ai_tex_ac_sel = None;
                                }
                            });
                            ui.add_space(2.0);
                        }
                    });
                });
                ui.add_space(4.0);
            }

            // -- Reference image thumbnail (if a photo was dropped) --
            let ref_tex_id = self.ai.ai_tex_ref_thumb_handle.as_ref().map(|h| h.id());
            let mut clear_ref_thumb = false;
            if let Some(tex_id) = ref_tex_id {
                ui.horizontal(|ui| {
                    ui.image(egui::load::SizedTexture::new(tex_id, egui::vec2(48.0, 48.0)));
                    ui.vertical(|ui| {
                        ui.small("Reference image");
                        if ui.small_button("\u{2715} clear").clicked() { clear_ref_thumb = true; }
                    });
                });
                ui.add_space(2.0);
            }
            if clear_ref_thumb { self.ai.ai_tex_ref_thumb_handle = None; }

            // -- Prompt text field with autocomplete --
            let te_resp = ui.add(
                egui::TextEdit::singleline(&mut self.ai.ai_tex_prompt)
                    .desired_width(f32::INFINITY)
                    .hint_text("type a material, or drag a reference photo here\u{2026}")
            );

            // Update autocomplete on every keystroke
            if te_resp.changed() {
                let q = self.ai.ai_tex_prompt.trim();
                if q.len() >= 2 {
                    self.ai.ai_tex_ac_matches = material_dict::search_materials(q, 7);
                } else {
                    self.ai.ai_tex_ac_matches.clear();
                    self.ai.ai_tex_ac_sel = None;
                }
            }

            // Keyboard navigation in autocomplete
            if !self.ai.ai_tex_ac_matches.is_empty() {
                let (up, down, accept, close) = ctx.input(|i| (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Tab),
                    i.key_pressed(egui::Key::Escape),
                ));
                if close { self.ai.ai_tex_ac_matches.clear(); self.ai.ai_tex_ac_sel = None; }
                if down {
                    self.ai.ai_tex_ac_sel = Some(
                        self.ai.ai_tex_ac_sel.map_or(0, |s| (s + 1).min(self.ai.ai_tex_ac_matches.len() - 1))
                    );
                }
                if up {
                    self.ai.ai_tex_ac_sel = Some(
                        self.ai.ai_tex_ac_sel.map_or(0, |s| s.saturating_sub(1))
                    );
                }
                if accept {
                    if let Some(idx) = self.ai.ai_tex_ac_sel {
                        if let Some(entry) = self.ai.ai_tex_ac_matches.get(idx) {
                            self.ai.ai_tex_prompt = entry.prompt_fragment.to_string();
                            self.ai.ai_tex_ac_matches.clear();
                            self.ai.ai_tex_ac_sel = None;
                        }
                    }
                }
            }

            // Autocomplete dropdown (rendered as an overlay below the text field)
            if !self.ai.ai_tex_ac_matches.is_empty() {
                let drop_pos = egui::pos2(te_resp.rect.min.x, te_resp.rect.max.y + 2.0);
                let drop_width = te_resp.rect.width();
                let mut accept_idx: Option<usize> = None;
                egui::Area::new(egui::Id::new("ai_ac_dropdown"))
                    .fixed_pos(drop_pos)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(drop_width);
                            ui.set_max_width(drop_width);
                            let entries: Vec<_> = self.ai.ai_tex_ac_matches.iter().cloned().collect();
                            for (i, entry) in entries.iter().enumerate() {
                                let selected = self.ai.ai_tex_ac_sel == Some(i);
                                let col = material_dict::category_color(entry.category);
                                let row_resp = ui.horizontal(|ui| {
                                    ui.colored_label(col, "\u{25cf}");
                                    ui.vertical(|ui| {
                                        let nm = egui::RichText::new(entry.name)
                                            .strong()
                                            .color(if selected { egui::Color32::WHITE }
                                                   else { egui::Color32::from_rgb(220, 220, 220) });
                                        ui.label(nm);
                                        ui.label(egui::RichText::new(entry.hint).small()
                                            .color(egui::Color32::from_rgb(115, 115, 115)));
                                    });
                                }).response;
                                // Highlight selected row background
                                if selected {
                                    ui.painter().rect_filled(
                                        row_resp.rect,
                                        3.0,
                                        egui::Color32::from_rgba_unmultiplied(60, 110, 200, 55),
                                    );
                                }
                                // Click to accept
                                if row_resp.clicked() { accept_idx = Some(i); }
                                if row_resp.hovered() { self.ai.ai_tex_ac_sel = Some(i); }
                            }
                            ui.separator();
                            ui.label(egui::RichText::new("\u{2191}\u{2193} navigate  \u{b7}  Tab accept  \u{b7}  Esc close")
                                .small().color(egui::Color32::from_rgb(90, 90, 90)));
                        });
                    });
                if let Some(idx) = accept_idx {
                    if let Some(entry) = self.ai.ai_tex_ac_matches.get(idx) {
                        self.ai.ai_tex_prompt = entry.prompt_fragment.to_string();
                    }
                    self.ai.ai_tex_ac_matches.clear();
                    self.ai.ai_tex_ac_sel = None;
                }
            }

            // -- Dropped image handler (drag & drop reference photo) --
            let dropped_img_b64: Option<String> = ctx.input(|i| {
                for f in &i.raw.dropped_files {
                    if let Some(path) = &f.path {
                        let ext = path.extension()
                            .and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                        if matches!(ext.as_str(), "jpg"|"jpeg"|"png"|"bmp"|"webp") {
                            if let Ok(bytes) = std::fs::read(path) {
                                return Some(Self::encode_base64(&bytes));
                            }
                        }
                    }
                    // Also accept clipboard-pasted image bytes
                    if let Some(bytes) = &f.bytes {
                        return Some(Self::encode_base64(bytes.as_ref()));
                    }
                }
                None
            });
            if let Some(b64) = dropped_img_b64 {
                do_describe_image = Some(b64);
            }

            // Drag-over indicator
            let is_hovering_image = ctx.input(|i| !i.raw.hovered_files.is_empty());
            if is_hovering_image {
                let r = te_resp.rect.expand(4.0);
                ui.painter().rect_stroke(r, 4.0,
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(80, 150, 255)), egui::StrokeKind::Middle);
                ui.painter().rect_filled(r, 4.0,
                    egui::Color32::from_rgba_unmultiplied(60, 120, 255, 30));
                // Hint label
                ui.painter().text(
                    r.center(), egui::Align2::CENTER_CENTER,
                    "\u{1f4f7} Drop to describe material",
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(130, 180, 255),
                );
            }

            // -- Enhance controls --
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let enhance_tip = if ClaudeBridge::key_ok(&self.ai.claude_config) {
                    "Rewrite prompt with AI (Ollama \u{2192} Claude fallback)"
                } else {
                    "Rewrite prompt with Ollama (llama3.2:3b)\nInstall: ollama pull llama3.2:3b\nOr set ANTHROPIC_API_KEY for Claude fallback"
                };
                let can_enhance = !self.ai.claude.is_thinking;
                if ui.add_enabled(can_enhance, egui::Button::new("\u{2728} Enhance"))
                    .on_hover_text(enhance_tip).clicked() {
                    do_enhance = true;
                }
                ui.checkbox(&mut self.ai.ai_tex_auto_enhance, "Auto")
                    .on_hover_text("Auto-enhance before every generation");
                if let Some(ref status) = self.ai.ai_tex_status {
                    if status.contains("Enhancing") {
                        let dots = ".".repeat((std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
                            .as_millis() / 400 % 4) as usize + 1);
                        ui.colored_label(egui::Color32::from_rgb(200, 200, 80),
                            format!("\u{2728} Enhancing{}", dots));
                    }
                }
            });

            // Enhanced prompt preview -- shown until artist accepts or discards
            if let Some(ref enhanced_text) = self.ai.ai_tex_enhanced.clone() {
                ui.add_space(2.0);
                egui::Frame::dark_canvas(ui.style()).inner_margin(6.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("\u{2728} Enhanced preview:")
                        .small().color(egui::Color32::from_rgb(180, 220, 120)));
                    ui.label(egui::RichText::new(enhanced_text.as_str())
                        .small().color(egui::Color32::from_rgb(200, 240, 200)));
                    ui.horizontal(|ui| {
                        if ui.small_button("\u{2705} Use").clicked() {
                            self.ai.ai_tex_prompt = enhanced_text.clone();
                            self.ai.ai_tex_enhanced = None;
                        }
                        if ui.small_button("\u{270f} Edit").clicked() {
                            self.ai.ai_tex_prompt = enhanced_text.clone();
                            self.ai.ai_tex_enhanced = None;
                            // Focus will land on text field next frame
                        }
                        if ui.small_button("\u{274c} Discard").clicked() {
                            self.ai.ai_tex_enhanced = None;
                        }
                    });
                });
                ui.add_space(2.0);
            }
            ui.add_space(4.0);

            // -- Generate / Stop / Load --
            let gen_busy = self.ai.ai_tex_rx.is_some();
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!gen_busy, |ui| {
                    if ui.button("\u{1f9e0} Generate All Maps").clicked() { do_generate_ai = true; }
                    if ui.button("\u{1f3b2} New Seed").on_hover_text("Randomize seed and generate").clicked() {
                        self.ai.ai_tex_seed = (std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
                            .as_nanos() as u32).wrapping_mul(1664525).wrapping_add(1013904223);
                        do_generate_ai = true;
                    }
                });
                if gen_busy {
                    ui.spinner();
                    if ui.add(
                        egui::Button::new(egui::RichText::new("\u{23f9} Stop").color(egui::Color32::from_rgb(255, 120, 100)))
                    ).on_hover_text("Cancel generation (frees UI / GPU exclusive)").clicked() {
                        do_stop_ai = true;
                    }
                }
                if ui.button("\u{1f4c2} Load Texture...").clicked() { do_load = true; }
            });
            ui.add_space(6.0);

            // -- Map previews --
            let has_maps = self.ai.ai_tex_dim > 0;
            if has_maps {
                ui.separator();
                ui.strong("Generated Maps:");
                ui.add_space(4.0);

                let thumb_sz = egui::vec2(76.0, 76.0);
                ui.horizontal_wrapped(|ui| {
                    for i in 0..5 {
                        ui.vertical(|ui| {
                            ui.label(format!("{} {}", Self::MAP_ICONS[i], Self::MAP_LABELS[i]));
                            if let Some(ref handle) = self.ai.ai_tex_handles[i] {
                                ui.image(egui::load::SizedTexture::new(handle.id(), thumb_sz));
                            } else {
                                let colors = [
                                    egui::Color32::from_rgb(180,120,80),
                                    egui::Color32::from_rgb(128,128,220),
                                    egui::Color32::from_rgb(180,180,180),
                                    egui::Color32::from_rgb(80,80,80),
                                    egui::Color32::from_rgb(160,160,160),
                                ];
                                let (rect, _) = ui.allocate_exact_size(thumb_sz, egui::Sense::hover());
                                ui.painter().rect_filled(rect, 4.0, colors[i]);
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                    if !self.ai.ai_tex_maps_png[i].is_empty() {"\u{2713}"} else {"\u{2014}"},
                                    egui::FontId::proportional(20.0), egui::Color32::WHITE);
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("\u{1f4be}").on_hover_text("Save").clicked() { save_map = Some(i); }
                                if ui.small_button("\u{1f504}").on_hover_text("Regenerate").clicked() { regen_map = Some(i); }
                            });
                        });
                        if i < 4 { ui.add_space(4.0); }
                    }
                });
                ui.add_space(4.0);
                if ui.button("\u{1f4be} Save All Maps...").clicked() { do_save_all = true; }
                ui.add_space(2.0);

                // -- Material-mask regional regenerate ("prompt for material one") --
                ui.separator();
                ui.strong("Material mask regen");
                ui.weak("Re-generate one map with a material-specific prompt (white = replace in mask).");
                ui.horizontal(|ui| {
                    ui.label("Map:");
                    for (i, lab) in Self::MAP_LABELS.iter().enumerate() {
                        if ui.selectable_label(self.ai.ai_tex_mask_map == Some(i), *lab).clicked() {
                            self.ai.ai_tex_mask_map = Some(i);
                        }
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut self.ai.ai_tex_mask_prompt)
                    .desired_width(f32::INFINITY)
                    .hint_text("Material one prompt e.g. brushed steel only / worn leather grain"));
                ui.horizontal(|ui| {
                    let can = self.ai.ai_tex_mask_map.is_some()
                        && !self.ai.ai_tex_mask_prompt.trim().is_empty()
                        && self.ai.ai_tex_mask_map
                            .map(|i| !self.ai.ai_tex_maps_png[i].is_empty())
                            .unwrap_or(false);
                    if ui.add_enabled(can, egui::Button::new("\u{1f504} Regenerate with material prompt"))
                        .on_hover_text("Uses current map as base; full-white mask = replace whole map with material prompt")
                        .clicked()
                    {
                        if let Some(i) = self.ai.ai_tex_mask_map {
                            // Queue as map regen with material-specific prompt (full replace mask).
                            self.ai.ai_tex_prompt = self.ai.ai_tex_mask_prompt.clone();
                            regen_map = Some(i);
                        }
                    }
                });
                ui.add_space(2.0);

                // -- Rating --
                if self.ai.ai_tex_last_gen_id.is_some() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Rate this generation:");
                        if ui.button("\u{1f44e} Bad").clicked()     { do_rate = Some(1); }
                        if ui.button("\u{1f610} OK").clicked()      { do_rate = Some(2); }
                        if ui.button("\u{1f44d} Good").clicked()    { do_rate = Some(3); }
                        if ui.button("\u{1f525} Perfect").clicked() { do_rate = Some(4); }
                    });
                }
                ui.add_space(4.0);
            }

            // -- Apply / Clear --
            ui.separator();
            let has_texture = self.ai.ai_tex_dim > 0 || self.ai.ai_tex_maps_png.iter().any(|m| !m.is_empty());
            let has_selection = self.sel.usel.has_prims();
            let can_clear = has_texture
                || !self.ai.ai_tex_textured.is_empty()
                || gen_busy
                || self.ai.ai_tex_handles.iter().any(|h| h.is_some());
            ui.horizontal(|ui| {
                ui.add_enabled_ui(has_texture && has_selection && !gen_busy, |ui| {
                    if ui.button("\u{2705} Apply to Selected").clicked() { do_apply = true; }
                });
                ui.add_enabled_ui(can_clear, |ui| {
                    if ui.button("\u{1f5d1} Clear All")
                        .on_hover_text("Stop generation if running, then clear maps / apply list")
                        .clicked()
                    {
                        do_clear = true;
                    }
                });
                if gen_busy {
                    if ui.button("\u{23f9} Stop")
                        .on_hover_text("Stop generation only (keep maps)")
                        .clicked()
                    {
                        do_stop_ai = true;
                    }
                }
            });
            if !has_selection && has_texture { ui.weak("Select primitives first, then Apply"); }
            if !self.ai.ai_tex_textured.is_empty() {
                ui.weak(format!("{} primitives textured", self.ai.ai_tex_textured.len()));
            }

            // -- DB stats footer --
            if self.ai.ai_tex_db_total > 0 {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.weak(format!("DB: {} generated  |  {} liked", self.ai.ai_tex_db_total, self.ai.ai_tex_db_liked));
                });
            }

            // -- Server settings (collapsed) --
            ui.collapsing("\u{1f50c} Server Settings", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Host:");
                    ui.add(egui::TextEdit::singleline(&mut self.ai.ai_tex_host).desired_width(100.0));
                    ui.label("Port:");
                    let mut port_str = self.ai.ai_tex_port.to_string();
                    if ui.add(egui::TextEdit::singleline(&mut port_str).desired_width(50.0)).changed() {
                        if let Ok(p) = port_str.parse::<u16>() { self.ai.ai_tex_port = p; }
                    }
                });
                ui.weak("Server auto-starts on launch. If not responding, check Python/pip install.");
            });

            // -- Status --
            if let Some(ref status) = self.ai.ai_tex_status {
                ui.add_space(4.0);
                ui.colored_label(
                    if status.starts_with('\u{2705}') { egui::Color32::from_rgb(100, 220, 100) }
                    else if status.starts_with('\u{23f3}') { egui::Color32::from_rgb(255, 200, 80) }
                    else { egui::Color32::from_rgb(255, 100, 100) },
                    status.as_str()
                );
            }
        });

        // ---- Handle actions outside window closure ----
        if do_pull_slots {
            self.ai_tex_pull_slots_from_selection();
        }
        if do_stop_ai {
            self.stop_ai_texture_job("Stopped by user");
            ai_toast = Some("\u{23f9} Generation stopped".into());
        }
        if do_generate_ai {
            // AI owns the GPU — pause Path Trace / CUDA while Flux runs.
            self.begin_ai_gpu_exclusive("Flux AI Texture");
            let q = self.ai.ai_tex_quality.clone();
            // Flux-first presets: fewer steps + cap res (4090 + offload still heavy).
            let is_flux = self.ai.ai_tex_model.contains("flux") || self.ai.ai_tex_model == "flux-kontext";
            let (size, steps, guidance) = if is_flux {
                match q.as_str() {
                    "preview"  => (768u32,  10u32, 2.5f32),
                    "standard" => (1024,    16,    2.5),
                    "final"    => (1024,    24,    2.8),
                    "ultra"    => (1280,    28,    3.0),
                    _          => (self.ai.ai_tex_size.min(1280), self.ai.ai_tex_steps.min(28), 2.5),
                }
            } else {
                // SDXL (Juggernaut etc.): one albedo + derived maps — ~10s on 4090.
                // Do NOT use 40–50 steps / 1536–2048 here (that path was ~1 min).
                match q.as_str() {
                    "preview"  => (768u32, 12u32, 6.5f32),
                    "standard" => (1024,   18,    7.0),
                    "final"    => (1024,   24,    7.5),
                    "ultra"    => (1280,   28,    7.5),
                    _          => (self.ai.ai_tex_size.min(1280), self.ai.ai_tex_steps.min(28), self.ai.ai_tex_guidance),
                }
            };
            self.ai.ai_tex_size = size;
            self.ai.ai_tex_steps = steps;
            self.ai.ai_tex_guidance = guidance;

            // Multi-object path: one PBR set per slot (node workflow).
            if self.ai.ai_tex_multi_object && !self.ai.ai_tex_object_slots.is_empty() {
                let n = self.ai.ai_tex_object_slots.len();
                self.ai.ai_tex_status = Some(format!(
                    "\u{23f3} Multi-object generate: {} targets @ {}px (may take several minutes)...",
                    n, size
                ));
                let host = self.ai.ai_tex_host.clone();
                let port = self.ai.ai_tex_port;
                let seed = self.ai.ai_tex_seed;
                let slots: Vec<(String, String, String)> = self.ai.ai_tex_object_slots.iter()
                    .map(|s| (s.id.clone(), s.name.clone(), s.prompt.clone()))
                    .collect();
                if let Some(ref old_abort) = self.ai.ai_tex_abort {
                    old_abort.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                self.ai.ai_tex_generation += 1;
                let generation = self.ai.ai_tex_generation;
                let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.ai.ai_tex_abort = Some(abort.clone());
                let (tx, rx) = std::sync::mpsc::channel();
                self.ai.ai_tex_rx = Some(rx);
                let auto = self.ai.ai_tex_auto_enhance;
                std::thread::spawn(move || {
                    // Generate first target into the main result channel; remaining saved server-side.
                    let result = if let Some((id, name, prompt)) = slots.first() {
                        let p = if prompt.trim().is_empty() {
                            format!("{}, material surface", name)
                        } else {
                            prompt.clone()
                        };
                        let r = FourthLayerApp::request_ai_texture(
                            &host, port, &p, size, seed, steps, guidance, &q,
                        );
                        // Fire remaining targets sequentially (each becomes a disk set under by_object/).
                        for (i, (tid, tname, tprompt)) in slots.iter().enumerate().skip(1) {
                            if abort.load(std::sync::atomic::Ordering::SeqCst) { break; }
                            let tp = if tprompt.trim().is_empty() {
                                format!("{}, material surface", tname)
                            } else {
                                tprompt.clone()
                            };
                            let _ = FourthLayerApp::request_ai_texture(
                                &host, port, &tp, size, seed.wrapping_add((i as u32).wrapping_mul(9973)),
                                steps, guidance, &q,
                            );
                            let _ = (tid, auto); // reserved for multi-result aggregation
                        }
                        r
                    } else {
                        Err("No object slots".into())
                    };
                    if !abort.load(std::sync::atomic::Ordering::SeqCst) {
                        let _ = tx.send((generation, result));
                    }
                });
            } else {
                self.ai.ai_tex_status = Some(format!(
                    "\u{23f3} Generating ({} preset · {}px · may take 15-120s)...",
                    q, size
                ));
                let host   = self.ai.ai_tex_host.clone();
                let port   = self.ai.ai_tex_port;
                let prompt = self.ai.ai_tex_prompt.clone();
                let seed   = self.ai.ai_tex_seed;
                if let Some(ref old_abort) = self.ai.ai_tex_abort {
                    old_abort.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                self.ai.ai_tex_generation += 1;
                let generation = self.ai.ai_tex_generation;
                let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.ai.ai_tex_abort = Some(abort.clone());
                let (tx, rx) = std::sync::mpsc::channel();
                self.ai.ai_tex_rx = Some(rx);
                std::thread::spawn(move || {
                    let result = FourthLayerApp::request_ai_texture(
                        &host, port, &prompt, size, seed, steps, guidance, &q,
                    );
                    if !abort.load(std::sync::atomic::Ordering::SeqCst) {
                        let _ = tx.send((generation, result));
                    }
                });
            }
        }
        if let Some(mi) = regen_map {
            let map_name = Self::MAP_NAMES[mi];
            let q = self.ai.ai_tex_quality.clone();
            let (size, steps, guidance) = match q.as_str() {
                "preview"  => (512u32, 12u32, 7.0f32),
                "standard" => (1024,   28,    7.5),
                "final"    => (1536,   40,    8.0),
                "ultra"    => (2048,   50,    8.0),
                _          => (self.ai.ai_tex_size, self.ai.ai_tex_steps, self.ai.ai_tex_guidance),
            };
            self.ai.ai_tex_status = Some(format!("\u{23f3} Regenerating {}...", Self::MAP_LABELS[mi]));
            // Material-mask path: pass base PNG so server can soft-composite (full white = replace).
            let base_b64 = if !self.ai.ai_tex_maps_png[mi].is_empty() {
                Some(Self::encode_base64(&self.ai.ai_tex_maps_png[mi]))
            } else {
                None
            };
            match Self::request_single_map_masked(
                &self.ai.ai_tex_host, self.ai.ai_tex_port,
                &self.ai.ai_tex_prompt, map_name, size, self.ai.ai_tex_seed, steps, guidance,
                base_b64.as_deref(),
            ) {
                Ok(png_bytes) => {
                    self.ai.ai_tex_maps_png[mi] = png_bytes;
                    self.ai.ai_tex_status = Some(format!("\u{2705} {} regenerated", Self::MAP_LABELS[mi]));
                    ai_toast = Some(format!("\u{2705} {} map regenerated", Self::MAP_LABELS[mi]));
                }
                Err(e) => { self.ai.ai_tex_status = Some(format!("\u{274c} {}", e)); }
            }
        }
        if let Some(mi) = save_map {
            if let Some(msg) = Self::save_ai_map(mi, &self.ai.ai_tex_maps_png[mi]) {
                self.ai.ai_tex_status = Some(msg.clone());
                ai_toast = Some(msg);
            }
        }
        if do_save_all {
            if let Some(dir) = rfd::FileDialog::new().set_title("Save All Maps \u{2014} Choose Folder").pick_folder() {
                let dp = dir.to_string_lossy().to_string();
                let mut saved = 0;
                for i in 0..5 {
                    if !self.ai.ai_tex_maps_png[i].is_empty() {
                        let fp = format!("{}\\fl_{}_{}.png", dp, Self::MAP_NAMES[i], self.ai.ai_tex_seed);
                        if std::fs::write(&fp, &self.ai.ai_tex_maps_png[i]).is_ok() { saved += 1; }
                    }
                }
                let msg = format!("\u{2705} {} maps saved to {}", saved, dp);
                self.ai.ai_tex_status = Some(msg.clone());
                ai_toast = Some(msg);
            }
        }
        if do_load {
            if let Some(msg) = self.load_texture_file() {
                self.ai.ai_tex_status = Some(msg.clone());
                ai_toast = Some(msg);
            }
        }
        if do_apply {
            let sel_indices = self.selected_prim_indices_unified();
            for idx in &sel_indices { self.ai.ai_tex_textured.insert(*idx); }
            self.cache.prim_buf_stale = true; // textured set affects serialized prim buffer
            self.mark_dirty(DirtyFlags::RENDER);
            ai_toast = Some(format!("\u{2705} Applied to {} primitives", sel_indices.len()));
        }
        if do_clear {
            // Safe path: stop job first, defer texture free, never zero-size GPU buffer.
            self.clear_ai_texture_maps();
            self.ai.ai_tex_status = Some("\u{1f5d1} Cleared".into());
            ai_toast = Some("\u{1f5d1} Cleared".into());
        }
        if let Some(rating) = do_rate {
            if let Some(gen_id) = self.ai.ai_tex_last_gen_id {
                let labels = ["", "\u{1f44e} Bad", "\u{1f610} OK", "\u{1f44d} Good", "\u{1f525} Perfect"];
                Self::request_rate(&self.ai.ai_tex_host, self.ai.ai_tex_port, gen_id, rating);
                let liked = rating >= 3;
                if liked && self.ai.ai_tex_db_liked < self.ai.ai_tex_db_total { self.ai.ai_tex_db_liked += 1; }
                ai_toast = Some(format!("Rated: {}", labels.get(rating as usize).unwrap_or(&"?")));
                self.ai.ai_tex_last_gen_id = None; // one rating per generation
            }
        }
        if let Some(model_key) = do_switch_model {
            self.ai.ai_tex_status = Some(format!("\u{23f3} Loading {} (may take 1-2 min)...", model_key));
            match Self::request_switch_model(&self.ai.ai_tex_host, self.ai.ai_tex_port, &model_key) {
                Ok(label) => {
                    self.ai.ai_tex_model = model_key;
                    self.ai.ai_tex_status = Some(format!("\u{2705} Model loaded: {}", label));
                    ai_toast = Some(format!("\u{2705} Switched to {}", label));
                }
                Err(e) => { self.ai.ai_tex_status = Some(format!("\u{274c} {}", e)); }
            }
        }
        // -- Enhance prompt (Ollama -> Claude fallback) --
        if do_enhance {
            self.ai.ai_tex_status = Some("\u{23f3} Enhancing prompt\u{2026}".into());
            let prompt_snap = self.ai.ai_tex_prompt.clone();
            let ollama_err = match Self::request_enhance_prompt(&self.ai.ai_tex_host, self.ai.ai_tex_port, &prompt_snap, &self.ai.agent_config.model_name) {
                Ok(enhanced) => {
                    self.ai.ai_tex_enhanced = Some(enhanced);
                    self.ai.ai_tex_status = Some("\u{2705} Enhanced! Accept or discard below.".into());
                    None
                }
                Err(e) => Some(e),
            };
            if let Some(e) = ollama_err {
                if ClaudeBridge::key_ok(&self.ai.claude_config) {
                    // Ollama offline -- fall back to Claude API
                    let cfg = self.ai.claude_config.clone();
                    self.ai.claude.send(ClaudeRequest {
                        kind: ClaudeKind::EnhancePrompt,
                        prompt: prompt_snap,
                        context: String::new(),
                        image_b64: String::new(),
                    }, &cfg);
                    self.ai.ai_tex_status = Some("\u{23f3} Enhancing with Claude\u{2026}".into());
                } else {
                    self.ai.ai_tex_status = Some(format!("\u{274c} Enhance failed: {}", e));
                    ai_toast = Some("\u{274c} Ollama unavailable. Run: ollama pull llama3.2:3b  (or set ANTHROPIC_API_KEY)".into());
                }
            }
        }
        // -- Describe reference image (Ollama LLaVA) --
        if let Some(b64) = do_describe_image {
            self.ai.ai_tex_status = Some("\u{23f3} Analysing reference image with LLaVA\u{2026}".into());
            match Self::request_describe_image(&self.ai.ai_tex_host, self.ai.ai_tex_port, &b64) {
                Ok((desc, enhanced)) => {
                    // Fill prompt with raw description; show enhanced as a preview
                    self.ai.ai_tex_prompt = desc.clone();
                    self.ai.ai_tex_ac_matches.clear();
                    if enhanced != desc {
                        self.ai.ai_tex_enhanced = Some(enhanced);
                    }
                    self.ai.ai_tex_status = Some("\u{2705} Material described! Enhanced preview below.".into());
                    // Register a tiny thumbnail from a 1x1 placeholder so the ref
                    // image indicator appears (full thumbnail decoding would need image crate)
                    ai_toast = Some("\u{1f4f7} Reference image described".into());
                }
                Err(_) => {
                    if ClaudeBridge::key_ok(&self.ai.claude_config) {
                        // LLaVA unavailable -- fall back to Claude Vision API
                        let cfg = self.ai.claude_config.clone();
                        self.ai.claude.send(ClaudeRequest {
                            kind: ClaudeKind::DescribeImage,
                            prompt: String::new(),
                            context: String::new(),
                            image_b64: b64,
                        }, &cfg);
                        self.ai.ai_tex_status = Some("\u{23f3} LLaVA unavailable \u{2014} using Claude Vision\u{2026}".into());
                    } else {
                        self.ai.ai_tex_status = Some("\u{274c} Image description failed: LLaVA not running. Run: ollama pull llava:7b  (or set ANTHROPIC_API_KEY)".into());
                        ai_toast = Some("\u{274c} LLaVA not available. Pull llava:7b or add Claude key.".into());
                    }
                }
            }
        }

        if let Some(msg) = ai_toast { self.toast(&msg); }
    }
}
