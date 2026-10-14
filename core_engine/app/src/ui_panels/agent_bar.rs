//! Agent / Claude command bar (pure-relocated from ui_panels.rs).

use eframe::egui;
use core_math::primitives::PbrMaterial;
use crate::{AzraApp, DirtyFlags, match_material_prompt};
use crate::agent::{AgentStatus, scene_to_json};
use crate::claude::{ClaudeBridge, ClaudeKind, ClaudeRequest};

impl AzraApp {
    pub(crate) fn draw_agent_bar(&mut self, ctx: &egui::Context) {
        if self.ai.agent_show_bar || self.ai.agent.is_thinking() {
            // Esc closes the bar
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !self.ai.agent.is_thinking() {
                self.ai.agent_show_bar = false;
                self.ai.agent_input.clear();
            }
            let screen = ctx.input(|i| i.screen_rect());
            let bar_w = 520.0_f32.min(screen.width() - 40.0);
            let bar_x = screen.center().x - bar_w / 2.0;
            let bar_y = screen.top() + screen.height() * 0.32;
            let mut submit_prompt: Option<String> = None;
            let mut mat_apply: Option<String> = None;
            egui::Area::new(egui::Id::new("fl_cmd_bar"))
                .fixed_pos(egui::pos2(bar_x, bar_y))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(20,22,28,245))
                        .rounding(12.0)
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80,130,200,120)))
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_min_width(bar_w);
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(100,160,255), "✨ AZRA");
                                if self.ai.agent_config.use_ollama {
                                    match self.ai.agent.ollama_available {
                                        Some(true) => { ui.colored_label(egui::Color32::from_rgb(80,180,80), "●"); }
                                        Some(false) => { ui.colored_label(egui::Color32::from_rgb(255,100,100), "●"); }
                                        None => { ui.colored_label(egui::Color32::from_rgb(180,180,80), "●"); }
                                    }
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.weak("Esc  ·  Enter");
                                    ui.add_space(8.0);
                                    let claude_ok = ClaudeBridge::key_ok(&self.ai.claude_config);
                                    let use_claude_lbl = if self.ai.use_claude_for_3d { "✨ Claude" } else { "🤖 Ollama" };
                                    if ui.add_enabled(claude_ok, egui::Button::new(use_claude_lbl).small())
                                        .on_hover_text(if claude_ok { "Toggle: Claude API / Ollama" } else { "Set ANTHROPIC_API_KEY to use Claude" })
                                        .clicked() { self.ai.use_claude_for_3d = !self.ai.use_claude_for_3d; }
                                });
                            });
                            ui.add_space(6.0);
                            if self.ai.claude.is_thinking {
                                let secs = self.ai.claude.thinking_seconds();
                                let anim = ["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
                                let frame = anim[(secs * 8.0) as usize % anim.len()];
                                ui.colored_label(egui::Color32::from_rgb(180, 140, 255),
                                    format!("{} Claude generating... ({:.1}s)", frame, secs));
                            } else if self.ai.agent.is_thinking() {
                                let secs = self.ai.agent.thinking_seconds();
                                let anim = ["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
                                let frame = anim[(secs * 8.0) as usize % anim.len()];
                                ui.colored_label(egui::Color32::from_rgb(130, 200, 255),
                                    format!("{} Generating... ({:.1}s)", frame, secs));
                            } else if let AgentStatus::Error(ref e) = self.ai.agent.status {
                                ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("❌ {}", e));
                                if ui.small_button("Dismiss").clicked() { self.ai.agent.status = AgentStatus::Idle; }
                            } else {
                                let resp = ui.add(egui::TextEdit::singleline(&mut self.ai.agent_input)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Describe what to generate... 'mat: black metal' or 'shader: weathered bronze'")
                                    .font(egui::TextStyle::Monospace));
                                let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
                                if (resp.lost_focus() && enter_pressed) || ui.button("⚡ Go").clicked() {
                                    let prompt = self.ai.agent_input.trim().to_string();
                                    if !prompt.is_empty() { submit_prompt = Some(prompt); }
                                }
                            }
                            if !self.ai.agent.is_thinking() && self.ai.agent_input.is_empty() && matches!(self.ai.agent.status, AgentStatus::Idle) {
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.weak("Objects:");
                                    for label in ["🔫 AK-47","🗡 Sword","🚗 Car","💀 Head","🤖 Robot","🛡 Tank"] {
                                        if ui.small_button(label).clicked() {
                                            let clean = label.chars().skip_while(|c| !c.is_alphanumeric()).collect::<String>();
                                            submit_prompt = Some(clean);
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.weak("Materials:");
                                    for (key, label) in PbrMaterial::preset_names() {
                                        if ui.small_button(*label).clicked() { mat_apply = Some(key.to_string()); }
                                    }
                                });
                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    ui.colored_label(egui::Color32::from_gray(70), "💡");
                                    ui.colored_label(egui::Color32::from_gray(90), "Type naturally — AI understands modifiers, shapes & materials");
                                });
                            }
                        });
                });
            // Execute submit (deferred to avoid borrow issues)
            if let Some(prompt) = submit_prompt {
                let lower = prompt.to_lowercase();
                let mat_key = if lower.starts_with("mat:") || lower.starts_with("mat ") {
                    let desc = lower.trim_start_matches("mat:").trim_start_matches("mat ").trim();
                    match_material_prompt(desc)
                } else if lower.contains("rust") && (lower.contains("metal") || lower.contains("iron")) {
                    Some("black_metal_rust".to_string())
                } else if lower.contains("leather") {
                    Some("worn_leather".to_string())
                } else if lower.contains("chrome") || lower.contains("mirror") {
                    Some("chrome".to_string())
                } else if lower.contains("stone") || lower.contains("rock") {
                    Some("rough_stone".to_string())
                } else if lower.contains("wood") {
                    Some("aged_wood".to_string())
                } else if lower.contains("alien") || lower.contains("sci-fi") || lower.contains("scifi") {
                    Some("alien_tech".to_string())
                } else if lower.contains("military") || lower.contains("army") {
                    Some("military_green".to_string())
                } else {
                    None
                };
                if let Some(key) = mat_key {
                    mat_apply = Some(key);
                    self.ai.agent_input.clear();
                } else if (lower.starts_with("shader:") || lower.starts_with("graph:") || lower.starts_with("node:"))
                    && ClaudeBridge::key_ok(&self.ai.claude_config)
                {
                    // AI Node Graph Generation — strip prefix and send to Claude
                    let desc = prompt.splitn(2, ':').nth(1).unwrap_or("").trim().to_string();
                    let cfg = self.ai.claude_config.clone();
                    self.ai.claude.send(ClaudeRequest {
                        kind: ClaudeKind::GenerateNodeGraph,
                        prompt: desc,
                        context: String::new(),
                        image_b64: String::new(),
                    }, &cfg);
                    self.ai.agent_input.clear();
                    self.toast("🎨 Generating node graph...");
                } else if self.ai.use_claude_for_3d && ClaudeBridge::key_ok(&self.ai.claude_config) {
                    let scene_json = scene_to_json(&self.scene.primitives, "");
                    let cfg = self.ai.claude_config.clone();
                    self.ai.claude.send(ClaudeRequest { kind: ClaudeKind::Generate3D, prompt, context: scene_json, image_b64: String::new() }, &cfg);
                    self.ai.agent_input.clear();
                    self.toast("✨ Claude thinking...");
                } else {
                    let cfg = self.ai.agent_config.clone();
                    self.ai.agent.send_prompt(prompt, &cfg);
                    self.ai.agent_input.clear();
                    self.toast("🤖 Agent thinking...");
                }
            }
            if let Some(ref key) = mat_apply {
                let mat = PbrMaterial::preset(key);
                let label = PbrMaterial::preset_names().iter().find(|(k,_)| *k == key.as_str()).map(|(_,l)|*l).unwrap_or(key.as_str());
                if !self.sel.usel.has_prims() && !self.scene.primitives.is_empty() {
                    let desc = format!("Material: {} (all)", label);
                    let n = self.scene.primitives.len();
                    self.transaction(&desc, |s| {
                        for i in 0..n { let m = mat.clone(); s.write_prim(i, |p| p.material = m); }
                    });
                    self.toast(&format!("🎨 Applied {} to all objects", label));
                    self.mark_dirty(DirtyFlags::RENDER);
                } else if self.sel.usel.has_prims() {
                    let desc = format!("Material: {}", label);
                    let sel: Vec<usize> = self.selected_prim_indices_unified();
                    // Empty resolve → no empty undo.
                    if !sel.is_empty() {
                        self.transaction(&desc, |s| {
                            for &i in &sel { let m = mat.clone(); s.write_prim(i, |p| p.material = m); }
                        });
                        self.toast(&format!("🎨 Applied {} to {} object(s)", label, sel.len()));
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                } else {
                    self.toast("No objects to apply material to");
                }
                self.ai.agent_input.clear();
            }
        }
    }

}
