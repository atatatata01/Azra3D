// core_engine/app/src/app_poll_ai.rs
//
// Slice (app_poll de-god, PURE RELOCATION): the AI / agent / Claude / AI-texture poll helpers moved
// out of app_poll.rs. Byte-for-byte identical method bodies — no logic change. poll_background_tasks
// (the orchestrator) stays in app_poll.rs and calls these; they resolve crate-wide as AzraApp methods.

use eframe::egui;
use crate::*;
use crate::claude::ClaudeKind;

impl AzraApp {
    /// Poll Ollama agent (non-blocking).
    /// Repaint is owned by the end-of-frame idle gate (`idle_pending_work`) while thinking —
    /// do not double-request here (masked stuck-thinking spin under continuous idle).
    pub(crate) fn poll_agent(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        if let Some(result) = self.ai.agent.poll() {
            match result {
                Ok(prims) => { self.inject_agent_primitives(prims); }
                Err(e) => { self.toast(&format!("\u{274C} Agent error: {}", e)); }
            }
        }
    }

    /// Poll Claude API (non-blocking). Same repaint policy as `poll_agent`.
    pub(crate) fn poll_claude(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        if let Some(result) = self.ai.claude.poll() {
            match result {
                Ok(resp) => {
                    match resp.kind {
                        ClaudeKind::Generate3D => {
                            match crate::agent::parse_agent_json(&resp.text) {
                                Ok(prims) => { self.inject_agent_primitives(prims); }
                                Err(e) => { self.toast(&format!("\u{274C} Claude parse error: {}", e)); }
                            }
                            self.ai.agent_show_bar = false;
                        }
                        ClaudeKind::CodeReview => {
                            self.ai.code_review_result = resp.text;
                            self.ai.code_review_err = None;
                        }
                        ClaudeKind::EnhancePrompt => {
                            self.ai.ai_tex_enhanced = Some(resp.text);
                            self.ai.ai_tex_status = Some("\u{2705} Enhanced with Claude! Accept or discard below.".into());
                        }
                        ClaudeKind::GenerateNodeGraph => {
                            match serde_json::from_str::<crate::shader_nodes::NodeGraph>(&resp.text) {
                                Ok(mut graph) => {
                                    let wgsl = graph.generate_wgsl();
                                    if let Some(idx) = self.active_prim_index_unified() {
                                        if idx < self.scene.primitives.len() {
                                            let uid = self.scene.primitives[idx].prim_uid;
                                            let name = self.scene.primitives[idx].name.clone();
                                            self.transaction("AI shader", |s| {
                                                s.write_prim(idx, |p| p.node_graph = Some(wgsl));
                                                s.ui.node_graphs.insert(uid, graph);
                                                s.shader_dirty = true;
                                                s.mark_dirty(DirtyFlags::RENDER);
                                            });
                                            self.toast(&format!("\u{1F3A8} AI shader applied to '{}'", name));
                                        }
                                    } else {
                                        self.ui.node_graph = graph;
                                        self.ui.show_node_editor = true;
                                        self.toast("\u{1F3A8} AI shader generated \u{2014} open Node Graph to assign");
                                    }
                                }
                                Err(e) => {
                                    self.toast(&format!("\u{274C} Node graph parse error: {}", &e.to_string()[..e.to_string().len().min(80)]));
                                    self.ai.code_review_result = format!("AI Node Graph (parse failed):\n{}", resp.text);
                                }
                            }
                        }
                        ClaudeKind::DescribeImage => {
                            self.ai.ai_tex_prompt = resp.text.clone();
                            self.ai.ai_tex_ac_matches.clear();
                            self.ai.ai_tex_status = Some("\u{2705} Material described by Claude Vision!".into());
                            self.toast("\u{1F4F7} Reference described by Claude Vision");
                        }
                    }
                }
                Err(e) => {
                    if self.ai.claude.is_thinking == false {
                        self.ai.code_review_err = Some(e.clone());
                        self.toast(&format!("\u{274C} Claude error: {}", &e[..e.len().min(80)]));
                    }
                }
            }
        }
    }

    /// Poll AI service (CLIP material detection, non-blocking).
    pub(crate) fn poll_ai_service(&mut self) {
        if let Some(resp) = self.ai.ai_service.poll() {
            match resp {
                crate::ai_client::AiResponse::ProfileResult(p) => {
                    if let Some(idx) = self.active_prim_index_unified() {
                        if idx < self.scene.primitives.len() {
                            let albedo = Vec3::new(p.albedo[0], p.albedo[1], p.albedo[2]);
                            let rough = p.roughness;
                            let metal = p.metallic;
                            let name = p.matched.unwrap_or("custom".into());
                            self.transaction("AI material", |s| {
                                s.write_prim(idx, |pr| {
                                    pr.material.roughness = rough;
                                    pr.material.metallic = metal;
                                    pr.material.albedo = albedo;
                                });
                                s.mark_dirty(DirtyFlags::RENDER);
                            });
                            self.ai.ai_service_status = Some(format!("Applied: {name}"));
                            self.toast(&format!("AI Material: {name}"));
                        }
                    }
                }
                crate::ai_client::AiResponse::ImageResult(a) => {
                    if let Some(idx) = self.active_prim_index_unified() {
                        if idx < self.scene.primitives.len() {
                            let albedo = Vec3::new(a.pbr.albedo[0], a.pbr.albedo[1], a.pbr.albedo[2]);
                            let rough = a.pbr.roughness;
                            let metal = a.pbr.metallic;
                            let status_msg = format!("{} ({:.0}%)", a.top_material, a.confidence * 100.0);
                            self.transaction("AI texture material", |s| {
                                s.write_prim(idx, |pr| {
                                    pr.material.roughness = rough;
                                    pr.material.metallic = metal;
                                    pr.material.albedo = albedo;
                                });
                                s.mark_dirty(DirtyFlags::RENDER);
                            });
                            self.ai.ai_service_status = Some(status_msg.clone());
                            self.toast(&format!("CLIP: {}", status_msg));
                        }
                    }
                }
                crate::ai_client::AiResponse::Health { ok, clip_loaded } => {
                    self.ai.ai_service_status = Some(if ok { format!("AI Service OK (CLIP: {})", if clip_loaded {"loaded"} else {"not loaded"}) } else { "AI Service error".into() });
                }
                crate::ai_client::AiResponse::Error(e) => {
                    self.ai.ai_service_status = Some(format!("Error: {}", &e[..e.len().min(60)]));
                    self.toast(&format!("AI: {}", &e[..e.len().min(60)]));
                }
            }
        }
    }

    /// Poll Neural Beauty AOV capture + HTTP result (non-blocking).
    pub(crate) fn poll_ai_beauty(&mut self, ctx: &egui::Context) {
        self.poll_ai_beauty_capture(ctx);
        self.poll_ai_beauty_result(ctx);
    }

    /// Poll AI texture generation (non-blocking).
    /// Generation counter prevents stale results from overwriting newer ones.
    pub(crate) fn poll_ai_texture(&mut self, ctx: &egui::Context) {
        if self.ai.ai_tex_rx.is_some() { ctx.request_repaint(); }
        let ai_tex_result = self.ai.ai_tex_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some((gen, result)) = ai_tex_result {
            // Discard stale / stopped results (generation counter check)
            if gen < self.ai.ai_tex_generation {
                // Drain stale message; keep waiting only if rx still valid for a newer job
                // (same rx means this was the only message — clear channel ownership).
                if self.ai.ai_tex_rx.is_some() {
                    // Only clear if no newer job re-used the slot with higher gen pending.
                    // After stop we set rx=None; after supersede we bump gen and replace rx.
                    // Stale gen with live rx of a newer job: keep rx.
                }
                return;
            }
            self.ai.ai_tex_rx = None;
            self.ai.ai_tex_abort = None;
            self.end_ai_gpu_exclusive_if_idle();
            match result {
                Ok(res) => {
                    // Guard: empty / invalid dims must not crash GPU upload
                    let dim = res.dim.max(1);
                    self.ai.ai_tex_last_gen_id = res.gen_id;
                    self.ai.ai_tex_db_total = res.db_total;
                    self.ai.ai_tex_db_liked = res.db_liked;
                    self.ai.ai_tex_dim = dim;
                    self.ai.ai_tex_data = if res.pixels.is_empty() {
                        vec![0u32; (dim as usize).saturating_mul(dim as usize).max(1)]
                    } else {
                        res.pixels.clone()
                    };
                    self.ai.ai_tex_maps_png = res.map_pngs;
                    self.ai.ai_tex_map_thumbs = res.map_thumbs;
                    for i in 0..5 {
                        let thumb = self.ai.ai_tex_map_thumbs[i].clone();
                        self.register_map_preview(ctx, i, &thumb);
                    }
                    self.ai.ai_tex_status = Some(format!("\u{2705} Generated {}\u{00D7}{} \u{2014} 5 maps ready", dim, dim));
                    self.upload_ai_texture_to_gpu();
                    self.mark_dirty(DirtyFlags::RENDER);
                    self.toast(&format!("\u{2705} AI PBR set {}\u{00D7}{}", dim, dim));
                }
                Err(e) => { self.ai.ai_tex_status = Some(format!("\u{274C} {}", e)); }
            }
        }
    }
}
