// win_ai.rs — Agent settings, code review window
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;
use crate::agent::{AgentBridge, scene_to_json};
use crate::claude::{ClaudeBridge, ClaudeKind, ClaudeRequest};

// Source files available for code review
const REVIEW_FILES: &[(&str, &str)] = &[
    ("main.rs",       "f:\\PROJECT\\FourthLAyerAi\\core_engine\\app\\src\\main.rs"),
    ("agent.rs",      "f:\\PROJECT\\FourthLAyerAi\\core_engine\\app\\src\\agent.rs"),
    ("claude.rs",     "f:\\PROJECT\\FourthLAyerAi\\core_engine\\app\\src\\claude.rs"),
    ("primitives.rs", "f:\\PROJECT\\FourthLAyerAi\\core_engine\\core_math\\src\\primitives.rs"),
    ("lib.rs",        "f:\\PROJECT\\FourthLAyerAi\\core_engine\\graphics\\src\\lib.rs"),
];

impl AzraApp {
    // ================================================================
    // 4. Agent Settings
    // ================================================================
    pub(crate) fn draw_agent_settings(&mut self, ctx: &egui::Context) {
        if !self.ai.show_agent_settings { return; }
        let mut agent_toast: Option<String> = None;
        egui::Window::new("\u{2699} Agent Settings").collapsible(false).resizable(false).default_width(380.0).open(&mut self.ai.show_agent_settings).show(ctx,|ui|{
            ui.heading("\u{1f916} AI Agent Configuration");
            ui.add_space(6.0);

            // Mode toggle
            ui.horizontal(|ui|{
                ui.label("Mode:");
                ui.selectable_value(&mut self.ai.agent_config.use_ollama, true, "\u{1f310} Ollama (Local LLM)");
                ui.selectable_value(&mut self.ai.agent_config.use_ollama, false, "\u{1f9ea} Mock (Hardcoded)");
            });
            ui.add_space(4.0);

            if self.ai.agent_config.use_ollama {
                ui.horizontal(|ui|{ ui.label("Model:"); ui.text_edit_singleline(&mut self.ai.agent_config.model_name); });
                ui.horizontal(|ui|{
                    ui.label("Host:");
                    ui.add(egui::TextEdit::singleline(&mut self.ai.agent_config.ollama_host).desired_width(120.0));
                    ui.label("Port:");
                    let mut port_str = self.ai.agent_config.ollama_port.to_string();
                    if ui.add(egui::TextEdit::singleline(&mut port_str).desired_width(60.0)).changed() {
                        if let Ok(p) = port_str.parse::<u16>() { self.ai.agent_config.ollama_port = p; }
                    }
                });
                ui.add(egui::Slider::new(&mut self.ai.agent_config.temperature, 0.0..=1.0).text("Temperature"));

                ui.add_space(4.0);
                ui.horizontal(|ui|{
                    if ui.button("\u{1f50c} Test Connection").clicked() {
                        let available = AgentBridge::check_ollama(&self.ai.agent_config);
                        self.ai.agent.ollama_available = Some(available);
                        if available { agent_toast = Some("\u{2705} Ollama is running!".into()); }
                        else { agent_toast = Some("\u{274c} Ollama not found. Run: ollama serve".into()); }
                    }
                    match self.ai.agent.ollama_available {
                        Some(true) => { ui.colored_label(egui::Color32::from_rgb(100, 220, 100), "\u{2705} Connected"); }
                        Some(false) => { ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "\u{274c} Offline"); }
                        None => { ui.weak("Not checked yet"); }
                    }
                });
                ui.add_space(4.0);
                ui.weak("Recommended models: qwen2.5:7b, llama3.1:8b, mistral:7b");
                ui.weak("Install: ollama pull qwen2.5:7b");
            } else {
                ui.weak("Mock mode: uses hardcoded responses (no AI needed).");
                ui.weak("Recognized: AK-47, tank, car, sword, head, table, mushroom");
            }

            // Dataset export section
            ui.add_space(8.0); ui.separator(); ui.add_space(4.0);
            ui.heading("\u{1f4ca} Training Dataset Export");
            ui.horizontal(|ui|{
                ui.label("Description:");
                ui.text_edit_singleline(&mut self.ui.export_desc);
            });
            if ui.button("\u{1f4cb} Export Scene as Training JSON").clicked() {
                let json = scene_to_json(&self.scene.primitives, &self.ui.export_desc);
                ui.output_mut(|o| o.copied_text = json);
                agent_toast = Some("\u{1f4cb} Training JSON copied to clipboard!".into());
            }
            ui.weak("Tip: Model your object, describe it, export \u{2192} build your LoRA dataset.");

            // Raw JSON debug
            if !self.ai.agent.last_raw_json.is_empty() {
                ui.add_space(8.0); ui.separator(); ui.add_space(4.0);
                ui.collapsing("\u{1f50d} Last Agent Raw JSON", |ui|{
                    let text = &self.ai.agent.last_raw_json[..self.ai.agent.last_raw_json.len().min(500)];
                    ui.monospace(text);
                    if ui.small_button("\u{1f4cb} Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = self.ai.agent.last_raw_json.clone());
                    }
                });
            }
        });
        // Deferred toast (avoids borrow conflict with window closure)
        if let Some(msg) = agent_toast { self.toast(&msg); }
    }

    // ================================================================
    // 5. Code Review (Claude)
    // ================================================================
    pub(crate) fn draw_code_review(&mut self, ctx: &egui::Context) {
        if !self.ai.show_code_review { return; }
        let mut do_review = false;
        let mut review_load_file = false;
        egui::Window::new("\u{1f50d} Code Review \u{2014} Claude")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .default_height(600.0)
            .open(&mut self.ai.show_code_review)
            .show(ctx, |ui| {
                // API key status
                let key_ok = ClaudeBridge::key_ok(&self.ai.claude_config);
                if !key_ok {
                    ui.colored_label(egui::Color32::from_rgb(255,140,60),
                        "\u{26a0} ANTHROPIC_API_KEY not set \u{2014} add to Run_Azra.bat");
                    ui.separator();
                }

                // File selector + load
                ui.horizontal(|ui| {
                    ui.label("File:");
                    egui::ComboBox::from_id_salt("cr_file_sel")
                        .selected_text(REVIEW_FILES.get(self.ai.code_review_file_idx).map(|(n,_)|*n).unwrap_or(""))
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in REVIEW_FILES.iter().enumerate() {
                                ui.selectable_value(&mut self.ai.code_review_file_idx, i, *name);
                            }
                        });
                    if ui.button("Load").clicked() { review_load_file = true; }
                });
                if review_load_file {
                    if let Some((_, path)) = REVIEW_FILES.get(self.ai.code_review_file_idx) {
                        match std::fs::read_to_string(path) {
                            Ok(content) => {
                                // Truncate to 6000 chars to stay within context
                                self.ai.code_review_context = content.chars().take(6000).collect();
                            }
                            Err(e) => { self.ai.code_review_err = Some(format!("Read error: {}", e)); }
                        }
                    }
                }
                if !self.ai.code_review_context.is_empty() {
                    let lines = self.ai.code_review_context.lines().count();
                    ui.weak(format!("  ({} lines loaded \u{2014} up to 6000 chars)", lines));
                }

                ui.separator();

                // Quick-action chips
                ui.horizontal(|ui| {
                    ui.weak("Quick:");
                    for (lbl, q) in [
                        ("\u{1f41b} Find bugs",       "Review this code for bugs, panics, or logic errors. List each with a fix."),
                        ("\u{26a1} Optimize",         "Identify performance bottlenecks and suggest concrete optimizations."),
                        ("\u{1f4d6} Explain",          "Explain what this code does in plain language."),
                        ("\u{1f512} Safety review",   "Check for unsafe patterns, unwraps, index-out-of-bounds, or memory issues."),
                    ] {
                        if ui.small_button(lbl).clicked() {
                            self.ai.code_review_question = q.to_string();
                        }
                    }
                });

                ui.add_space(4.0);
                ui.label("Question:");
                ui.add(egui::TextEdit::multiline(&mut self.ai.code_review_question)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .hint_text("Ask Claude about the loaded code..."));

                ui.horizontal(|ui| {
                    let can_ask = key_ok && !self.ai.claude.is_thinking && !self.ai.code_review_question.is_empty();
                    if ui.add_enabled(can_ask, egui::Button::new("\u{26a1} Ask Claude")).clicked() {
                        do_review = true;
                    }
                    if self.ai.claude.is_thinking {
                        let secs = self.ai.claude.thinking_seconds();
                        let anim = ["\u{280b}","\u{2819}","\u{2839}","\u{2838}","\u{283c}","\u{2834}","\u{2826}","\u{2827}","\u{2807}","\u{280f}"];
                        ui.colored_label(egui::Color32::from_rgb(180,140,255),
                            format!("{} Claude thinking\u{2026} {:.1}s", anim[(secs*8.0) as usize % 10], secs));
                    }
                    if !self.ai.code_review_result.is_empty() {
                        if ui.small_button("\u{1f4cb} Copy").clicked() {
                            ui.output_mut(|o| o.copied_text = self.ai.code_review_result.clone());
                        }
                        if ui.small_button("\u{1f5d1} Clear").clicked() {
                            self.ai.code_review_result.clear();
                            self.ai.code_review_err = None;
                        }
                    }
                });

                ui.separator();

                // Result area
                if let Some(ref err) = self.ai.code_review_err.clone() {
                    ui.colored_label(egui::Color32::from_rgb(255,100,100), format!("\u{274c} {}", err));
                } else if !self.ai.code_review_result.is_empty() {
                    egui::ScrollArea::vertical().id_salt("cr_result").max_height(300.0).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut self.ai.code_review_result.clone())
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace)
                            .interactive(false));
                    });
                } else {
                    ui.add_space(8.0);
                    ui.weak("Load a file, type a question, and click \u{26a1} Ask Claude.");
                }

                // Claude model selector
                ui.add_space(4.0); ui.separator();
                ui.horizontal(|ui| {
                    ui.weak("Model:");
                    for m in ["claude-haiku-4-5-20251001","claude-sonnet-4-6","claude-opus-4-6"] {
                        let short = m.split('-').next().unwrap_or(m);
                        let short = format!("{} {}", short.chars().next().unwrap_or(' ').to_uppercase(), &short[short.char_indices().nth(1).map(|(i,_)|i).unwrap_or(0)..]);
                        if ui.selectable_label(self.ai.claude_config.model == m, short).clicked() {
                            self.ai.claude_config.model = m.to_string();
                        }
                    }
                    ui.separator();
                    ui.weak("API key:");
                    let key_display = if key_ok { "\u{2705} set" } else { "\u{274c} not set" };
                    ui.weak(key_display);
                });
            });
        // Deferred review submit (outside borrow)
        if do_review {
            let cfg = self.ai.claude_config.clone();
            self.ai.claude.send(ClaudeRequest {
                kind: ClaudeKind::CodeReview,
                prompt: self.ai.code_review_question.clone(),
                context: self.ai.code_review_context.clone(),
                image_b64: String::new(),
            }, &cfg);
        }
    }
}
