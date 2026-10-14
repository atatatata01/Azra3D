// AI State (agent, claude, texture gen, CLIP service, neural beauty) — extracted from types.rs
use eframe::egui;
use std::collections::HashSet;
use std::sync::{Arc, atomic::AtomicBool};
use crate::agent::{AgentBridge, AgentConfig};
use crate::claude::{ClaudeBridge, ClaudeConfig};
use crate::ai_texture::AiTexGenResult;
use crate::gpu_ctx::PendingAiBeautyCapture;
use crate::ai_beauty::AiBeautyResult;

// ==================================================================
// AI State (agent, claude, texture gen, CLIP service)
// ==================================================================

/// One object/mesh slot for multi-prompt AI texturing (node-workflow friendly).
#[derive(Clone, Debug)]
pub(crate) struct AiObjectPromptSlot {
    /// Stable id: `mesh-01`, scene node id string, or material name.
    pub id: String,
    /// Display name: "Leather Strap", "Steel Shaft".
    pub name: String,
    /// Material prompt for this object only.
    pub prompt: String,
    /// Optional scene node (when pulled from selection).
    pub node_id: Option<core_math::scene_graph::NodeId>,
}

pub(crate) struct AiState {
    // Agent (Text-to-CSG / Ollama)
    pub(crate) agent: AgentBridge,
    pub(crate) agent_input: String,
    pub(crate) agent_show_bar: bool,
    pub(crate) agent_config: AgentConfig,
    pub(crate) show_agent_settings: bool,
    // Claude API
    pub(crate) claude: ClaudeBridge,
    pub(crate) claude_config: ClaudeConfig,
    pub(crate) use_claude_for_3d: bool,
    // Code review
    pub(crate) show_code_review: bool,
    pub(crate) code_review_question: String,
    pub(crate) code_review_context: String,
    pub(crate) code_review_result: String,
    pub(crate) code_review_err: Option<String>,
    pub(crate) code_review_file_idx: usize,
    // AI service (CLIP)
    pub(crate) ai_service: crate::ai_client::AiBridge,
    pub(crate) ai_service_status: Option<String>,
    pub(crate) ai_mat_query: String,
    // AI Texture generation
    pub(crate) show_ai_tex_window: bool,
    pub(crate) ai_tex_prompt: String,
    pub(crate) ai_tex_host: String,
    pub(crate) ai_tex_port: u16,
    pub(crate) ai_tex_size: u32,
    pub(crate) ai_tex_seed: u32,
    pub(crate) ai_tex_steps: u32,
    pub(crate) ai_tex_guidance: f32,
    pub(crate) ai_tex_tiling: f32,
    pub(crate) ai_tex_data: Vec<u32>,
    pub(crate) ai_tex_dim: u32,
    pub(crate) ai_tex_textured: HashSet<usize>,
    pub(crate) ai_tex_status: Option<String>,
    pub(crate) ai_tex_rx: Option<std::sync::mpsc::Receiver<(u64, Result<AiTexGenResult, String>)>>,
    /// Generation counter — incremented on each new generation request.
    /// Results with stale generation are discarded.
    pub(crate) ai_tex_generation: u64,
    /// Abort flag — set to true when a new generation supersedes the current one.
    pub(crate) ai_tex_abort: Option<Arc<AtomicBool>>,
    pub(crate) ai_tex_maps_png: [Vec<u8>; 5],
    pub(crate) ai_tex_handles: [Option<egui::TextureHandle>; 5],
    pub(crate) ai_tex_dropping: [Option<egui::TextureHandle>; 5],
    // AI Texture v3
    pub(crate) ai_tex_model: String,
    pub(crate) ai_tex_quality: String,
    pub(crate) ai_tex_last_gen_id: Option<u32>,
    pub(crate) ai_tex_db_total: u32,
    pub(crate) ai_tex_db_liked: u32,
    pub(crate) ai_tex_map_thumbs: [Vec<u32>; 5],
    // Smart Prompt System
    pub(crate) ai_tex_ac_matches: Vec<&'static crate::material_dict::MaterialEntry>,
    pub(crate) ai_tex_ac_sel: Option<usize>,
    pub(crate) ai_tex_enhanced: Option<String>,
    pub(crate) ai_tex_auto_enhance: bool,
    pub(crate) ai_tex_ref_thumb_handle: Option<egui::TextureHandle>,
    pub(crate) ai_tex_show_browser: bool,
    pub(crate) ai_tex_browser_cat: usize,
    /// Per-object / per-mesh material prompts (node workflow input).
    /// e.g. Mesh-01 → "worn leather strap", Mesh-02 → "brushed steel shaft".
    pub(crate) ai_tex_object_slots: Vec<AiObjectPromptSlot>,
    /// When true, Generate runs one PBR set per object slot (not just the global prompt).
    pub(crate) ai_tex_multi_object: bool,
    /// Material-mask map index for regional regen (0=albedo … 4=height); None = full set.
    pub(crate) ai_tex_mask_map: Option<usize>,
    /// Prompt used only for material-mask regional regenerate ("material one").
    pub(crate) ai_tex_mask_prompt: String,
    // ── Neural Beauty (path-tracer / G-buffer AI polish overlay) ──
    pub(crate) show_beauty_window: bool,
    pub(crate) beauty_prompt: String,
    pub(crate) beauty_strength: f32,
    pub(crate) beauty_structure: f32,
    pub(crate) beauty_seed: u32,
    pub(crate) beauty_steps: u32,
    pub(crate) beauty_guidance: f32,
    pub(crate) beauty_max_side: u32,
    /// preview | standard | final | ultra — drives max_side/steps on server.
    pub(crate) beauty_quality: String,
    pub(crate) beauty_use_depth: bool,
    pub(crate) beauty_use_normal: bool,
    pub(crate) beauty_use_edges: bool,
    pub(crate) beauty_use_curvature: bool,
    pub(crate) beauty_overlay_enabled: bool,
    pub(crate) beauty_overlay_opacity: f32,
    pub(crate) beauty_show_split: bool,
    pub(crate) beauty_capture_requested: bool,
    pub(crate) pending_beauty_capture: Option<PendingAiBeautyCapture>,
    pub(crate) beauty_busy: bool,
    pub(crate) beauty_status: Option<String>,
    pub(crate) beauty_rx: Option<std::sync::mpsc::Receiver<(u64, Result<AiBeautyResult, String>)>>,
    pub(crate) beauty_generation: u64,
    pub(crate) beauty_abort: Option<Arc<AtomicBool>>,
    pub(crate) beauty_source_handle: Option<egui::TextureHandle>,
    pub(crate) beauty_result_handle: Option<egui::TextureHandle>,
    pub(crate) beauty_result_png: Vec<u8>,
    pub(crate) beauty_source_rgba: Vec<u8>,
    pub(crate) beauty_source_w: u32,
    pub(crate) beauty_source_h: u32,
    pub(crate) beauty_result_w: u32,
    pub(crate) beauty_result_h: u32,
    /// True after we toasted "PT paused for AI" once this exclusive episode.
    pub(crate) ai_gpu_yield_notified: bool,
    // ── AI render region (viewport crop for efficient resolution) ──
    /// User is drawing a region (LMB drag on viewport).
    pub(crate) beauty_region_draw: bool,
    /// Drag start in viewport-local coords (relative to rect.min).
    pub(crate) beauty_region_drag_start: Option<egui::Pos2>,
    /// Live drag end while drawing (viewport-local).
    pub(crate) beauty_region_drag_cur: Option<egui::Pos2>,
    /// Committed region as normalized UV in the viewport (0–1). None = full frame.
    pub(crate) beauty_region_uv: Option<[f32; 4]>, // min_x, min_y, max_x, max_y
    /// When true, AI result overlay is fitted into the region rect (not full viewport).
    pub(crate) beauty_region_overlay: bool,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            agent: AgentBridge::new(),
            agent_input: String::new(),
            agent_show_bar: false,
            agent_config: AgentConfig::default(),
            show_agent_settings: false,
            claude: ClaudeBridge::new(),
            claude_config: ClaudeConfig::default(),
            use_claude_for_3d: false,
            show_code_review: false,
            code_review_question: String::new(),
            code_review_context: String::new(),
            code_review_result: String::new(),
            code_review_err: None,
            code_review_file_idx: 0,
            ai_service: crate::ai_client::AiBridge::new(),
            ai_service_status: None,
            ai_mat_query: String::new(),
            show_ai_tex_window: false,
            ai_tex_prompt: "rusty metal surface, sci-fi panels".into(),
            ai_tex_host: "127.0.0.1".into(),
            ai_tex_port: 7860,
            // Flux defaults: 1024 / 16 steps / CFG 2.5 (maps to Flux guidance).
            ai_tex_size: 1024,
            ai_tex_seed: 42,
            ai_tex_steps: 16,
            ai_tex_guidance: 2.5,
            ai_tex_tiling: 2.0,
            ai_tex_data: Vec::new(),
            ai_tex_dim: 0,
            ai_tex_textured: HashSet::new(),
            ai_tex_status: None,
            ai_tex_rx: None,
            ai_tex_generation: 0,
            ai_tex_abort: None,
            ai_tex_maps_png: Default::default(),
            ai_tex_handles: Default::default(),
            ai_tex_dropping: Default::default(),
            // Flux.1 Kontext first — local ComfyUI transformer (context polish + textures).
            ai_tex_model: "flux-kontext".into(),
            // Standard default for Flux — Final/Ultra when you have time.
            ai_tex_quality: "standard".into(),
            ai_tex_last_gen_id: None,
            ai_tex_db_total: 0,
            ai_tex_db_liked: 0,
            ai_tex_map_thumbs: Default::default(),
            ai_tex_ac_matches: vec![],
            ai_tex_ac_sel: None,
            ai_tex_enhanced: None,
            ai_tex_auto_enhance: false,
            ai_tex_ref_thumb_handle: None,
            ai_tex_show_browser: false,
            ai_tex_browser_cat: 0,
            ai_tex_object_slots: Vec::new(),
            ai_tex_multi_object: false,
            ai_tex_mask_map: None,
            ai_tex_mask_prompt: String::new(),
            show_beauty_window: false,
            beauty_prompt: "tactical polymer firearm, worn black finish, micro scratches on metal receiver, studio softbox lighting, product photography, sharp micro-detail".into(),
            // Slightly conservative denoise + strong structure lock = less melted geometry.
            beauty_strength: 0.38,
            beauty_structure: 0.82,
            beauty_seed: 42,
            // Flux beauty: fewer steps / 1024 max side (GPU exclusive while running).
            beauty_steps: 16,
            beauty_guidance: 2.5,
            beauty_max_side: 1024,
            beauty_quality: "standard".into(),
            beauty_use_depth: true,
            beauty_use_normal: true,
            beauty_use_edges: true,
            beauty_use_curvature: true,
            beauty_overlay_enabled: false,
            beauty_overlay_opacity: 1.0,
            beauty_show_split: true,
            beauty_capture_requested: false,
            pending_beauty_capture: None,
            beauty_busy: false,
            beauty_status: None,
            beauty_rx: None,
            beauty_generation: 0,
            beauty_abort: None,
            beauty_source_handle: None,
            beauty_result_handle: None,
            beauty_result_png: Vec::new(),
            beauty_source_rgba: Vec::new(),
            beauty_source_w: 0,
            beauty_source_h: 0,
            beauty_result_w: 0,
            beauty_result_h: 0,
            ai_gpu_yield_notified: false,
            beauty_region_draw: false,
            beauty_region_drag_start: None,
            beauty_region_drag_cur: None,
            beauty_region_uv: None,
            beauty_region_overlay: true,
        }
    }
}

impl AiState {
    /// Flux / Neural Beauty is active — viewport yields PathTrace + CUDA/OptiX.
    pub(crate) fn ai_gpu_exclusive(&self) -> bool {
        self.beauty_busy
            || self.beauty_capture_requested
            || self.pending_beauty_capture.is_some()
            || self.beauty_rx.is_some()
            || self.ai_tex_rx.is_some()
    }

    /// Pixel crop of the GPU viewport buffer from the committed UV region.
    /// Returns (x, y, w, h) clamped to buffer size, or full frame if no region.
    pub(crate) fn beauty_region_pixels(&self, buf_w: u32, buf_h: u32) -> (u32, u32, u32, u32) {
        let Some([u0, v0, u1, v1]) = self.beauty_region_uv else {
            return (0, 0, buf_w, buf_h);
        };
        let bw = buf_w as f32;
        let bh = buf_h as f32;
        let mut x0 = (u0.min(u1) * bw).floor() as i32;
        let mut y0 = (v0.min(v1) * bh).floor() as i32;
        let mut x1 = (u0.max(u1) * bw).ceil() as i32;
        let mut y1 = (v0.max(v1) * bh).ceil() as i32;
        x0 = x0.clamp(0, buf_w as i32);
        y0 = y0.clamp(0, buf_h as i32);
        x1 = x1.clamp(0, buf_w as i32);
        y1 = y1.clamp(0, buf_h as i32);
        let mut w = (x1 - x0).max(0) as u32;
        let mut h = (y1 - y0).max(0) as u32;
        // Minimum useful crop
        if w < 32 || h < 32 {
            return (0, 0, buf_w, buf_h);
        }
        // Snap to even dims (friendly for diffusion)
        w &= !1;
        h &= !1;
        if w < 32 || h < 32 {
            return (0, 0, buf_w, buf_h);
        }
        (x0 as u32, y0 as u32, w, h)
    }
}

/// Match a material description to a preset key
pub(crate) fn match_material_prompt(desc: &str) -> Option<String> {
    let d = desc.to_lowercase();
    if d.contains("black") && (d.contains("metal") || d.contains("rust")) { return Some("black_metal_rust".into()); }
    if d.contains("rust") && d.contains("iron") { return Some("rusty_iron".into()); }
    if d.contains("rust") { return Some("rusty_iron".into()); }
    if d.contains("leather") { return Some("worn_leather".into()); }
    if d.contains("alien") || d.contains("sci-fi") || d.contains("scifi") || d.contains("neon") { return Some("alien_tech".into()); }
    if d.contains("stone") || d.contains("rock") || d.contains("concrete") { return Some("rough_stone".into()); }
    if d.contains("wood") || d.contains("bois") { return Some("aged_wood".into()); }
    if d.contains("chrome") || d.contains("mirror") || d.contains("shiny") { return Some("chrome".into()); }
    if d.contains("military") || d.contains("army") || d.contains("camo") { return Some("military_green".into()); }
    if d.contains("clay") || d.contains("default") { return Some("clay".into()); }
    if d.contains("metal") { return Some("black_metal_rust".into()); }
    if d.contains("iron") { return Some("rusty_iron".into()); }
    None
}
