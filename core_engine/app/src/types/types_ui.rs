// UI Window / Dialog / Menu State — extracted from types.rs
use eframe::egui;
use crate::*;

/// What the Shader Node Editor is currently bound to.
/// **Primary:** outliner Material (shared, assignable). **Legacy:** per-prim override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeGraphTarget {
    /// Flat prim index in `scene.primitives` (+ prim_uid for map keys).
    Prim(usize),
    /// Outliner `NodePayload::Material` by stable MaterialId.
    Material(core_math::primitives::MaterialId),
}

// ==================================================================
// Performance Counters (dev overlay)
// ==================================================================
pub(crate) struct PerfCounters {
    pub(crate) frame_time_ms: f64,
    pub(crate) frame_history: [f64; 120],
    pub(crate) frame_history_idx: usize,
    pub(crate) dispatch_gpu_ms: f64,
    pub(crate) serialize_graph_ms: f64,
    pub(crate) sync_flat_cache_ms: f64,
    pub(crate) sync_flat_cache_count: u32,
    pub(crate) gpu_buffer_reallocs: u32,
    pub(crate) dirty_flags_this_frame: u32,
    /// B3d: first `mark_dirty` call site this frame (`file:line` via `#[track_caller]`).
    /// Cleared in `begin_frame`. Printed by `[IDLE_SPIN]` so residual was_dirty spins
    /// name the setter instead of guessing.
    pub(crate) first_dirty_caller: Option<String>,
    pub(crate) selected_resolve_ms: f64,
    pub(crate) prim_count: usize,
    pub(crate) light_count: usize,
    pub(crate) node_count: usize,
    // --- Gate 0: Volume cache metrics ---
    /// CPU bake time (last bake, ms).
    pub(crate) vol_bake_cpu_ms: f64,
    /// GPU bake dispatch time (last dispatch, ms).
    pub(crate) vol_bake_gpu_ms: f64,
    /// Volume upload time (SSBO + texture, ms).
    pub(crate) vol_upload_ms: f64,
    /// Body switch time (pool acquire + upload, ms).
    pub(crate) vol_switch_ms: f64,
    /// Volume pool hit/miss this frame (0=no switch, 1=hit, 2=miss).
    pub(crate) vol_pool_event: u8,
    /// Number of active bodies in the scene.
    pub(crate) body_count: usize,
    /// Per-body VRAM estimate (active body, bytes).
    pub(crate) vol_vram_bytes: u64,
    /// Full rebakes this frame.
    pub(crate) vol_full_rebakes: u32,
    /// Partial rebakes this frame.
    pub(crate) vol_partial_rebakes: u32,
    /// Gate 0 — per-body DC populate time accumulated this frame (ms). Was: resident.rs local
    /// eprintln-only; now summed across every `sdf_populate_resident` this frame.
    pub(crate) vol_dc_ms: f64,
    /// Gate 0 — the last-populated body's resident-mesh pool VRAM (bytes), from
    /// `SdfMeshPool::body_vram_bytes`. Distinct from `vol_vram_bytes` (the dense volume-pool path).
    pub(crate) sdf_pool_body_bytes: u64,
    /// Gate 0 — sparse mesh-pool fragmentation (0.0 packed .. 1.0 all holes), from `SdfMeshPool::hole_ratio`.
    pub(crate) sdf_pool_hole_ratio: f32,
    // --- Brick pool metrics (always updated — Properties panel reads these, not just HUD) ---
    /// Total bricks allocated in the pool (free field read).
    pub(crate) brick_used: u32,
    /// Total brick capacity (free field read).
    pub(crate) brick_capacity: u32,
    // --- Frame pacing metrics ---
    /// Total time spent in poll_background_tasks() (ms).
    pub(crate) poll_tasks_ms: f64,
    /// PLAN_FOR_GROK_VIEWPORT_PERF P2: residual after named sub-brackets (ms).
    pub(crate) poll_rest_ms: f32,
    /// Top named poll sub-step this frame (label + ms).
    pub(crate) poll_top_name: &'static str,
    pub(crate) poll_top_ms: f32,
    pub(crate) poll_2nd_name: &'static str,
    pub(crate) poll_2nd_ms: f32,
    /// Material map uploads this poll (G7 plateau unit ~31 ms × N).
    pub(crate) poll_map_uploads: u32,
    /// Phase C re-measure: poly LOD policy cost last frame (ms).
    pub(crate) poly_lod_ms: f32,
    /// Phase C re-measure: CPU occlusion pass cost last frame (ms).
    pub(crate) occlusion_ms: f32,
    /// Split of Hybrid `disp` (CPU wall of `dispatch_gpu_render`).
    /// `raster_ms` = full `dispatch_raster_pass`; `dispatch_rest_ms` = everything after.
    pub(crate) raster_ms: f32,
    pub(crate) raster_frustum_ms: f32,
    pub(crate) raster_sort_ms: f32,
    pub(crate) raster_upload_ms: f32,
    pub(crate) raster_shadow_enc_ms: f32,
    pub(crate) raster_gbuf_enc_ms: f32,
    pub(crate) raster_submit_ms: f32,
    pub(crate) dispatch_rest_ms: f32,
    /// Phase B.4: MeshCacheIds referenced by ≥2 bodies (shared GPU mesh).
    pub(crate) shared_mesh_cids: u32,
    /// Time spent in bake readback within poll_primary_bake() (ms).
    pub(crate) bake_readback_ms: f64,
    /// Number of bake results read back this frame.
    pub(crate) bake_readback_count: u32,
    /// Pipeline switches per second (rolling).
    pub(crate) path_switch_count: u32,
    /// Half-res transitions per second (rolling).
    pub(crate) halfres_transition_count: u32,
    /// Consecutive frames with frame_time > 2x rolling average.
    pub(crate) consecutive_spike_count: u32,
    // --- Pipeline warmup tracking ---
    /// Number of optional pipelines that are compiled (Some).
    pub(crate) pipelines_warm: u32,
    /// Total optional pipeline slots.
    pub(crate) pipelines_total: u32,
    /// Previous pipeline label for switch detection.
    pub(crate) prev_pipeline_label: &'static str,
    /// Previous half-res state for transition detection.
    pub(crate) prev_half_res: bool,
    /// Accumulator for path_switch_count (resets each second).
    pub(crate) path_switch_accum: u32,
    /// Accumulator for halfres_transition_count (resets each second).
    pub(crate) halfres_transition_accum: u32,
    /// Last second boundary for per-second counters.
    pub(crate) last_second: std::time::Instant,
    // --- Mesh Terrain gauges (latched while a TerrainWorld is active) ---
    /// Registered terrain tile count.
    pub(crate) terrain_tiles: u32,
    /// Currently visible (resident) terrain tiles.
    pub(crate) terrain_vis: u32,
    /// Last terrain bake wall-ms (from body_vol_cache finish timing).
    pub(crate) terrain_last_bake_ms: f32,
    /// Last frame vol_dc_ms while terrain active (DC populate).
    pub(crate) terrain_last_dc_ms: f32,
    /// Sum of display tris for resident terrain tiles.
    pub(crate) terrain_tris: u32,
    /// Terrain bodies currently in dirty_bodies.
    pub(crate) terrain_dirty_bodies: u32,
    /// Terrain bodies with a DirtyMeshRegion entry.
    pub(crate) terrain_dirty_regions: u32,
    /// Last settle remesh wall-ms (0 if none yet).
    pub(crate) terrain_last_remesh_ms: f32,
    /// Bodies waiting for post-bake display remesh.
    pub(crate) terrain_pending_remesh: u32,
}

impl Default for PerfCounters {
    fn default() -> Self {
        Self {
            frame_time_ms: 0.0,
            frame_history: [0.0; 120],
            frame_history_idx: 0,
            dispatch_gpu_ms: 0.0,
            serialize_graph_ms: 0.0,
            sync_flat_cache_ms: 0.0,
            sync_flat_cache_count: 0,
            gpu_buffer_reallocs: 0,
            dirty_flags_this_frame: 0,
            first_dirty_caller: None,
            selected_resolve_ms: 0.0,
            prim_count: 0,
            light_count: 0,
            node_count: 0,
            vol_bake_cpu_ms: 0.0,
            vol_bake_gpu_ms: 0.0,
            vol_upload_ms: 0.0,
            vol_switch_ms: 0.0,
            vol_pool_event: 0,
            body_count: 0,
            vol_vram_bytes: 0,
            vol_full_rebakes: 0,
            vol_partial_rebakes: 0,
            vol_dc_ms: 0.0,
            sdf_pool_body_bytes: 0,
            sdf_pool_hole_ratio: 0.0,
            brick_used: 0,
            brick_capacity: 0,
            poll_tasks_ms: 0.0,
            poll_rest_ms: 0.0,
            poll_top_name: "-",
            poll_top_ms: 0.0,
            poll_2nd_name: "-",
            poll_2nd_ms: 0.0,
            poll_map_uploads: 0,
            poly_lod_ms: 0.0,
            occlusion_ms: 0.0,
            raster_ms: 0.0,
            raster_frustum_ms: 0.0,
            raster_sort_ms: 0.0,
            raster_upload_ms: 0.0,
            raster_shadow_enc_ms: 0.0,
            raster_gbuf_enc_ms: 0.0,
            raster_submit_ms: 0.0,
            dispatch_rest_ms: 0.0,
            shared_mesh_cids: 0,
            bake_readback_ms: 0.0,
            bake_readback_count: 0,
            path_switch_count: 0,
            halfres_transition_count: 0,
            consecutive_spike_count: 0,
            pipelines_warm: 0,
            pipelines_total: 0,
            prev_pipeline_label: "",
            prev_half_res: false,
            path_switch_accum: 0,
            halfres_transition_accum: 0,
            last_second: std::time::Instant::now(),
            terrain_tiles: 0,
            terrain_vis: 0,
            terrain_last_bake_ms: 0.0,
            terrain_last_dc_ms: 0.0,
            terrain_tris: 0,
            terrain_dirty_bodies: 0,
            terrain_dirty_regions: 0,
            terrain_last_remesh_ms: 0.0,
            terrain_pending_remesh: 0,
        }
    }
}

impl PerfCounters {
    /// Reset per-frame counters at frame start.
    pub(crate) fn begin_frame(&mut self) {
        self.sync_flat_cache_count = 0;
        self.sync_flat_cache_ms = 0.0;
        self.gpu_buffer_reallocs = 0;
        self.dirty_flags_this_frame = 0;
        self.first_dirty_caller = None;
        self.serialize_graph_ms = 0.0;
        self.dispatch_gpu_ms = 0.0;
        self.selected_resolve_ms = 0.0;
        self.vol_pool_event = 0;
        self.vol_full_rebakes = 0;
        self.vol_partial_rebakes = 0;
        self.vol_dc_ms = 0.0; // per-frame accumulator (pool bytes / hole ratio are latched gauges)
        self.poll_tasks_ms = 0.0;
        self.poll_rest_ms = 0.0;
        self.poll_top_name = "-";
        self.poll_top_ms = 0.0;
        self.poll_2nd_name = "-";
        self.poll_2nd_ms = 0.0;
        self.poll_map_uploads = 0;
        // poly_lod_ms / occlusion_ms / shared_mesh_cids latched for scoreboard (updated in path)
        self.bake_readback_ms = 0.0;
        self.bake_readback_count = 0;
        // Per-second counters: rotate once per second
        if self.last_second.elapsed().as_secs_f64() >= 1.0 {
            self.path_switch_count = self.path_switch_accum;
            self.halfres_transition_count = self.halfres_transition_accum;
            self.path_switch_accum = 0;
            self.halfres_transition_accum = 0;
            self.last_second = std::time::Instant::now();
        }
    }

    /// Record a frame time sample into the ring buffer.
    pub(crate) fn record_frame(&mut self, ms: f64) {
        self.frame_time_ms = ms;
        self.frame_history[self.frame_history_idx] = ms;
        self.frame_history_idx = (self.frame_history_idx + 1) % self.frame_history.len();
    }

    /// Gate 0 — percentile (`p` in 0..1) over the frame-time ring buffer (the existing raw sample
    /// buffer; the plan's "p95 on the existing buffer" — the buffers stored raw samples but only ever
    /// computed avg+max). Non-mutating; skips the zero-filled unused slots. During a drag the ring is
    /// dominated by drag frames, so `frame_percentile(0.95)` is the drag p95 the plan asks for.
    pub(crate) fn frame_percentile(&self, p: f32) -> f64 {
        let mut s: Vec<f64> = self.frame_history.iter().copied().filter(|&x| x > 0.0).collect();
        if s.is_empty() { return 0.0; }
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((p.clamp(0.0, 1.0) as f64) * (s.len() - 1) as f64).round() as usize;
        s[idx]
    }
}

// ==================================================================
// Mesh Cleanup report (Ctrl+X empty edit / Apply)
// ==================================================================

/// Stats from one Mesh Cleanup run — popup + Tool Parameters summary.
#[derive(Clone, Debug, Default)]
pub(crate) struct MeshCleanupReport {
    pub(crate) body_name: String,
    pub(crate) verts_before: usize,
    pub(crate) verts_after: usize,
    pub(crate) edges_before: usize,
    pub(crate) edges_after: usize,
    pub(crate) faces_before: usize,
    pub(crate) faces_after: usize,
    pub(crate) folds_before: usize,
    pub(crate) folds_after: usize,
    pub(crate) dissolve_deg: f32,
    pub(crate) protected_kept: usize,
    /// False when cleanup refused (already clean / gates).
    pub(crate) applied: bool,
    /// Which Blender-parity pass actually fired. Net vert/face deltas alone cannot say — a run
    /// that welds a double AND fills a hole comes out even on counts.
    pub(crate) stats: core_math::mesh_boolean::CleanupStats,
}

impl MeshCleanupReport {
    pub(crate) fn verts_merged(&self) -> isize {
        self.verts_before as isize - self.verts_after as isize
    }
    pub(crate) fn edges_dissolved(&self) -> isize {
        self.edges_before as isize - self.edges_after as isize
    }
    pub(crate) fn faces_removed(&self) -> isize {
        self.faces_before as isize - self.faces_after as isize
    }
    pub(crate) fn folds_fixed(&self) -> isize {
        self.folds_before as isize - self.folds_after as isize
    }
}

// ==================================================================
// UI Window / Dialog / Menu State
// ==================================================================
pub(crate) struct UiWindowState {
    /// Server launch message — shown as a toast on first update(). Moved from FourthLayerApp (de-God-Object Phase 1).
    pub(crate) startup_toast: Option<String>,
    // Toasts + command palette + workspace-edit popups (moved from FourthLayerApp — Phase 1)
    pub(crate) toasts: Vec<Toast>,
    pub(crate) command_palette: CommandPaletteState,
    pub(crate) _ws_dragging: Option<(usize, usize)>,
    pub(crate) ws_add_popup: Option<usize>,
    pub(crate) ws_add_search: String,
    pub(crate) ws_context_menu: Option<usize>,
    // Window visibility flags
    pub(crate) show_profiler: bool,
    pub(crate) show_profiler_overlay: bool,
    pub(crate) show_perf_hud: bool,
    pub(crate) show_bevel_debug: bool,
    pub(crate) show_keybind_settings: bool,
    pub(crate) show_experimental: bool,
    pub(crate) show_about: bool,
    pub(crate) show_shortcut_editor: bool,
    pub(crate) show_add_popup: bool,
    /// When true, Add popup is SDF-only and creates into a sub-body of the selection (Ctrl+Shift+A).
    pub(crate) add_popup_sdf_subbody: bool,
    pub(crate) add_popup_pos: egui::Pos2,
    pub(crate) add_search: String,
    pub(crate) add_search_focus: bool,
    /// Bitmask of Add-menu sections the user expanded this open (secondary groups start
    /// collapsed so only the contextual primary family is exposed). Bits match
    /// menus.rs `AddSec`: Voxel=1, Sdf=2, Poly=4, Nurbs=8, Lights=16. Cleared on open.
    pub(crate) add_sec_user_open: u8,
    pub(crate) show_tool_panel: bool,
    pub(crate) show_tweak_panel: bool,
    /// Mesh→SDF pre-convert dialog: (poly nid, name, tri count, is_closed). None = closed.
    /// Opened by convert_poly_to_sdf; Convert button calls convert_poly_to_sdf_now.
    pub(crate) convert_sdf_dialog: Option<(core_math::scene_graph::NodeId, String, usize, bool)>,
    /// Mesh Cleanup dialog: Some((nid, name, faces, folds)) while open.
    pub(crate) mesh_cleanup_dialog: Option<(core_math::scene_graph::NodeId, String, usize, usize)>,
    pub(crate) mesh_cleanup_flatten: bool,
    pub(crate) mesh_cleanup_merge: bool,
    pub(crate) mesh_cleanup_dissolve_deg: f32,
    // ---- Blender `Mesh > Clean Up` parity knobs (2026-08-11) ----
    /// Merge by Distance threshold. 0 = off. Blender's own default is 1e-4.
    pub(crate) mesh_cleanup_merge_dist: f32,
    /// Degenerate Dissolve threshold (short edges + the zero-area faces they leave). 0 = off.
    pub(crate) mesh_cleanup_degenerate: f32,
    pub(crate) mesh_cleanup_loose_verts: bool,
    pub(crate) mesh_cleanup_loose_edges: bool,
    /// OFF by default: an isolated face is legitimate geometry on a sheet/decal mesh, so deleting
    /// it is a modelling decision, not a repair.
    pub(crate) mesh_cleanup_loose_faces: bool,
    /// Fill Holes side limit. 0 = off (closing a hole is authoring, not repair — opt in).
    pub(crate) mesh_cleanup_fill_holes: usize,
    /// Make Planar Faces iterations. 0 = off (it MOVES vertices).
    pub(crate) mesh_cleanup_planar_iters: u32,
    /// Split Non-Planar Faces angle. 0 = off (it ADDS geometry).
    pub(crate) mesh_cleanup_split_nonplanar_deg: f32,
    pub(crate) mesh_cleanup_split_concave: bool,
    /// Recalculate outward-consistent winding. OFF by default, for two reasons: Blender does not
    /// put it in Clean Up either (it lives under Mesh > Normals), and on a model with a
    /// deliberately INVERTED interior shell — a hollow import, a room, the inner surface of a
    /// glass — "orient outward" would turn that cavity inside out. It is one checkbox away here,
    /// and it is its own menu command (Shift+N).
    pub(crate) mesh_cleanup_recalc_normals: bool,
    /// Live preview while Mesh Cleanup is armed (default ON): param changes re-run cleanup
    /// from a base-mesh snapshot and show the result without committing undo until Apply.
    pub(crate) mesh_cleanup_live: bool,
    /// Last Mesh Cleanup run report (Ctrl+X empty edit / Apply). Drives the result popup + left panel summary.
    pub(crate) mesh_cleanup_result: Option<MeshCleanupReport>,
    /// Grid resolution chosen in the dialog (16..=128 — cap documented on LAB_CONVERT_RES).
    pub(crate) convert_sdf_res: u32,
    pub(crate) tweak_search: String,
    pub(crate) show_layout_settings: bool,
    pub(crate) show_panel_list: bool,
    /// One-shot: dump UI v2 panel size/position to %LOCALAPPDATA%/Azra/ui_layout_dump.txt
    /// (command palette / Developer Tweaks). Consumed after ui2_draw so rects are current.
    pub(crate) request_ui_layout_dump: bool,
    /// Gate-0 theme-verification window (token swatches: code hex vs mockup hex + a
    /// row of native widgets). Throwaway — remove once theme parity is signed off.
    pub(crate) show_token_board: bool,
    pub(crate) show_export_window: bool,
    pub(crate) show_gltf_export: bool,
    pub(crate) gltf_export_path: String,
    pub(crate) gltf_export_status: Option<String>,
    pub(crate) show_texgen_window: bool,
    pub(crate) show_node_editor: bool,
    pub(crate) show_macro_manager: bool,
    pub(crate) show_stress_test: bool,
    pub(crate) stress_test_results: Vec<String>,
    pub(crate) diag_scene_active: bool,
    pub(crate) debug_render_mode: u32, // 0=normal, 1=march-only, 2=step heatmap
    pub(crate) diag_saved_dna: Vec<(usize, Vec<core_math::primitives::DnaRule>)>,
    pub(crate) diag_saved_mods: Vec<(usize, Vec<core_math::primitives::Modifier>)>,
    pub(crate) show_bool_edges: bool,
    pub(crate) show_unsaved_dialog: bool,
    pub(crate) show_overlay_popover: bool,  // viewport overlay settings popup
    pub(crate) show_pt_settings: bool,     // floating PT settings window
    pub(crate) show_body_library: bool,
    pub(crate) body_library_confirm_delete: Option<std::path::PathBuf>,
    /// Cached kitbash entries `(label, path)` — rescan only on open/save/delete/refresh.
    pub(crate) body_library_entries: Vec<(String, std::path::PathBuf)>,
    pub(crate) body_library_needs_scan: bool,
    pub(crate) body_library_scanned_once: bool,
    /// Last kitbash folder used in Save dialog.
    pub(crate) body_library_last_folder: String,
    /// Interactive place after drag/insert from library: move / Ctrl surface-align / scroll resize.
    pub(crate) library_place: Option<LibraryPlaceSession>,
    // Library save dialog
    pub(crate) library_save_nid: Option<core_math::scene_graph::NodeId>,
    pub(crate) library_save_name: String,
    pub(crate) library_save_folder: String,
    /// One-shot focus request for the Name field when the save dialog opens.
    pub(crate) library_save_name_focused: bool,
    // Material assignment popup (Ctrl+M)
    pub(crate) show_mat_assign: bool,
    pub(crate) mat_assign_pos: egui::Pos2,
    pub(crate) mat_assign_search: String,
    /// Selected existing material in the assign popup (None = create-new mode).
    /// Choosing a list row highlights it and swaps the bottom action to Assign.
    pub(crate) mat_assign_selected: Option<u32>,
    pub(crate) mat_assign_new_name: String,     // name for the new material
    // Export state
    pub(crate) export_desc: String,
    pub(crate) export_resolution: u32,
    pub(crate) export_quads: bool,
    pub(crate) export_sharp: bool,
    pub(crate) export_path: String,
    pub(crate) export_status: Option<String>,
    pub(crate) last_mesh_stats: Option<(usize, usize)>,
    pub(crate) export_rx: Option<std::sync::mpsc::Receiver<Result<(String, usize, usize, f32, bool), String>>>,
    /// GATE 4: export == display by default (per-body display resolution via the shared
    /// materialize job); false → the resolution slider overrides through the same pipeline.
    pub(crate) export_display_res: bool,
    /// GATE 4: queued export — the panel only queues; GPU materialize runs in poll
    /// (`process_export_pending`, Frame Ordering Contract).
    pub(crate) export_pending: Option<crate::mesh_presentation::PendingExport>,
    // Texture generator state
    pub(crate) texgen_config: core_math::texgen::TexGenConfig,
    pub(crate) texgen_preset_name: String,
    pub(crate) texgen_status: Option<String>,
    // Keybind editor
    pub(crate) rebinding_action: Option<&'static str>,
    pub(crate) keybind_search: String,
    pub(crate) keybind_conflicts: Vec<(String, String)>,
    // Z-pie menu (legacy fields kept for compatibility)
    pub(crate) z_pie: crate::pie_menu::PieMenuState,
    // Tab pie menu — uses reusable PieMenuState
    pub(crate) tab_pie: crate::pie_menu::PieMenuState,
    // View pie menu (backtick key)
    pub(crate) view_pie: crate::pie_menu::PieMenuState,
    // Reset pie menu (transform reset)
    pub(crate) reset_pie: crate::pie_menu::PieMenuState,
    // F-key utility pie (Focus Camera / Isolate)
    pub(crate) f_pie: crate::pie_menu::PieMenuState,
    /// Shift+O viewport overlay pie (lights, grid, workplane, gizmo, …).
    pub(crate) overlay_pie: crate::pie_menu::PieMenuState,
    // D-key / yellow-dot pivot placement pie
    pub(crate) d_pie: crate::pie_menu::PieMenuState,
    /// When true, d_pie was opened by LMB on yellow OBJECT dot (click-to-select, not hold-D).
    pub(crate) d_pie_click_mode: bool,
    /// Yellow-dot center menu (legacy flat; prefer d_pie).
    pub(crate) pivot_center_menu_open: bool,
    pub(crate) pivot_center_menu_pos: egui::Pos2,
    /// Draw selection pivot marker in the viewport (center edit flow active).
    pub(crate) expose_selection_pivot: bool,
    // T-key Action Center pie (Modo-style center + axes)
    pub(crate) t_pie: crate::pie_menu::PieMenuState,
    // X-key poly boolean pie (Union / Difference / Intersect, cutter → targets)
    pub(crate) bool_pie: crate::pie_menu::PieMenuState,
    /// Poly boolean: keep the cutter object/island after applying (toggled from the pie).
    pub(crate) bool_pie_keep_cutter: bool,
    /// Poly boolean: what happens to the material of the faces the CUTTER contributed.
    /// Sticky across runs (the redo panel writes it back), so the next boolean reuses the choice.
    pub(crate) bool_cutter_material: crate::poly::bool_material::BoolCutterMaterial,
    /// Blender-style redo panel for the last poly Boolean — re-stamps the stored per-face
    /// material plan when the policy changes (the geometry op itself is destructive).
    pub(crate) poly_boolean_adjust: Option<crate::poly::bool_material::PolyBooleanAdjust>,
    /// X-key SDF boolean pie (Union / Subtract / Intersect / Seam — replaces the blind op cycle)
    pub(crate) sdf_bool_pie: crate::pie_menu::PieMenuState,
    /// B-key spine corner type pie (Sharp / Round / Chamfer) while editing SpinePipe points.
    pub(crate) spine_corner_pie: crate::pie_menu::PieMenuState,
    /// Ctrl+R SDF bake-resolution pie (64³…2048³ ladder for the active top-level body).
    pub(crate) res_pie: crate::pie_menu::PieMenuState,
    /// Ctrl+R was quick-tapped (released without moving): the pie stays open in click
    /// mode — LMB confirms the hovered slice, Esc/RMB cancels.
    pub(crate) res_pie_sticky: bool,
    /// When true, bool pie stays open after X release (reopened via special pie BACK) and
    /// confirms on LMB click instead of key-up.
    pub(crate) sdf_bool_pie_sticky: bool,
    /// X-key SDF boolean pie CENTER → special commands (Extract Intersection, …).
    pub(crate) sdf_special_pie: crate::pie_menu::PieMenuState,
    /// When true, special pie was opened with X already up — confirm via LMB, not key-up.
    pub(crate) sdf_special_pie_sticky: bool,
    /// Alt+X poly Symmetrize pie — reuses the shared PieMenuState (drawn via draw_pie_menu).
    pub(crate) sym_pie: crate::pie_menu::PieMenuState,
    /// Alt+A poly Align / Flatten pie (view direction + workplane).
    pub(crate) align_pie: crate::pie_menu::PieMenuState,
    /// Alt+M poly Merge pie (Average / Last / By Distance).
    pub(crate) merge_pie: crate::pie_menu::PieMenuState,
    /// Tool params for the Symmetrize pie (center mode, cut flag, captured target node, anchor).
    pub(crate) sym_pie_params: crate::sym_pie::SymPieParams,
    /// One-frame guard: a pie's confirming LMB click must not also reach viewport input the same
    /// frame (handle_keyboard runs before handle_viewport_input). Set on any pie LMB confirm/cancel,
    /// taken by the viewport input gate.
    pub(crate) pie_eat_click: bool,
    // Tab pie persistent mode (SDF / POLY / NURBS)
    pub(crate) pie_mode: crate::pie_menu::PieMode,
    /// SDF cycle mode: scroll wheel steps through prims in active body
    pub(crate) sdf_cycle_mode: bool,
    pub(crate) last_view_idx: Option<usize>,
    pub(crate) last_edit_mode: Option<WorkMode>,
    pub(crate) last_poly_edit_mode: Option<PolyWorkMode>,
    // Q-key operations menu
    pub(crate) q_menu_open: bool,
    pub(crate) q_menu_pos: egui::Pos2,
    // RMB context menu
    pub(crate) ctx_menu_open: bool,
    pub(crate) ctx_menu_pos: egui::Pos2,
    pub(crate) ctx_menu_prim: Option<usize>,
    /// Type-to-filter on the open context menu (cleared each open).
    pub(crate) ctx_menu_search: String,
    /// Request keyboard focus on the search field once after open.
    pub(crate) ctx_menu_focus_search: bool,
    /// Object-mode long-press LMB drag (Parent / Match TRS) — live while button held.
    pub(crate) object_link_gesture: Option<crate::object_link::ObjectLinkGesture>,
    /// Popup after drop on a target.
    pub(crate) object_link_menu: Option<crate::object_link::ObjectLinkMenu>,
    // Shift+RMB shape swap pie menu
    pub(crate) shape_swap_pie_open: bool,
    pub(crate) shape_swap_pie_pos: egui::Pos2,
    pub(crate) shape_swap_pie_idx: Option<usize>,
    // Profiler data
    pub(crate) prof_frame_times: std::collections::VecDeque<f32>,
    pub(crate) prof_pass_times: [f32; 7],
    pub(crate) prof_pass_names: [&'static str; 7],
    /// Real GPU-side per-pass timings (from timestamp queries). 0.0 if not available.
    /// Per-pass GPU times, indexed by the `gpu_timestamps::PASS_*` slots. Sized 20 to
    /// match `MAX_PASSES` — slots 16/17 are CSM cascades 1/2, added 2026-08-06 so the
    /// profiler stops reporting a third of the real shadow cost. `viewport_render.rs`
    /// copies with a `.min(len)` so a future slot bump cannot overflow this.
    pub(crate) prof_gpu_pass_times: [f32; 20],
    pub(crate) prof_gpu_timestamps_available: bool,
    pub(crate) prof_gpu_mem_mb: f32,
    /// Detailed GPU memory breakdown (computed once per second).
    pub(crate) prof_gpu_mem_textures_mb: f32,
    pub(crate) prof_gpu_mem_buffers_mb: f32,
    pub(crate) prof_gpu_mem_shadow_mb: f32,
    pub(crate) prof_gpu_mem_total_mb: f32,
    pub(crate) prof_gpu_mem_last_update: Option<std::time::Instant>,
    pub(crate) prof_render_res: (u32, u32),
    // CPU frame profiler
    pub(crate) show_cpu_frame_profiler: bool,
    pub(crate) cpu_frame_times: std::collections::VecDeque<f32>,
    /// Per-frame CPU section timers (ms). Indices:
    /// 0 input · 1 mesh · 2 dispatch · 3 submit · 4 overlays · 5 egui_layout · 6 dirty ·
    /// 7 mode_auto · 8 poll · 9 pre_panel_setup · 10 toast_menu_cmds
    /// (Gate 0 FRAME_STALL_PLAN_2026-08-05 — split former `unaccounted` black box).
    pub(crate) cpu_section_ms: [f32; 11],
    pub(crate) cpu_section_names: [&'static str; 11],
    /// Cache for properties panel poly material-uniform scan: (NodeId, Arc mesh ptr, first_mid, uniform).
    /// Invalidated when Arc identity changes (COW) or first face material changes.
    pub(crate) poly_mat_uniform_cache: Option<(core_math::scene_graph::NodeId, usize, u32, bool)>,
    /// Last applied egui style key (`0` = never). Avoids per-frame `style.clone()` + `set_style`
    /// (0.2–0.8 ms residual on full-UI idle — see ui-floor review).
    pub(crate) egui_style_key: u64,
    /// v2: sole interactive dock paint cost last frame (ms). SidePanel only reserves width;
    /// real widgets live in `ui2_repaint_dock_over_library` AFTER CentralPanel — previously
    /// unaccounted in scoreboard `lay=`. Folded into `cpu_section_ms[5]` for honest UI floor.
    pub(crate) ui2_dock_paint_ms: f32,
    // Pending action after unsaved dialog
    pub(crate) pending_action_after_save: Option<PendingAction>,
    /// CR-70b: Countdown frames before executing deferred post-dialog action.
    /// Set to 2 when a dialog button is clicked so egui renders the dialog
    /// dismissal before the blocking file picker opens.
    pub(crate) deferred_action_countdown: u32,
    // Shader node editor
    pub(crate) node_graph: crate::shader_nodes::NodeGraph,
    pub(crate) node_drag_state: crate::shader_nodes::NodeDragState,
    /// What the open node editor is editing (prim override or outliner Material).
    pub(crate) node_graph_target: Option<crate::NodeGraphTarget>,
    /// Legacy: per-primitive node graphs, keyed by prim_uid (still supported).
    pub(crate) node_graphs: std::collections::HashMap<u32, crate::shader_nodes::NodeGraph>,
    /// Outliner Material–owned procedural graphs, keyed by `MaterialId.0`.
    /// This is the primary owner: create Material in outliner → edit graph → bake maps → assign.
    pub(crate) material_node_graphs: std::collections::HashMap<u32, crate::shader_nodes::NodeGraph>,
    /// Material-panel docs, keyed by `MaterialId.0` (MATERIAL_PANEL_SPEC P0). Persisted via the
    /// `FL4MPNLS` tail; absent entries are derived on demand (`derive_doc_from_material`).
    pub(crate) material_panel_docs: std::collections::HashMap<u32, crate::material_panel::MaterialPanelDoc>,
    /// P2 pending destructive-action confirmation for the Material panel
    /// (remove Layered/GraphDriven channel, revert to panel control). RUNTIME-ONLY.
    pub(crate) mp_confirm: Option<crate::material_panel::MpConfirm>,
    /// P2 bake-ownership conflicts, keyed by `MaterialId.0` → bitmask of
    /// `ChannelKind as u32` bits whose graph bake was SKIPPED because the panel
    /// owns the channel (hand-loaded image). Drives the "conflict" badge. RUNTIME-ONLY.
    pub(crate) mp_bake_conflicts: std::collections::HashMap<u32, u32>,
    /// G4 R3 memo: mid -> (material_generation at build, projected doc). Keyed on the
    /// scene graph's material generation — the trusted invalidation signal (bumped by
    /// every material/graph commit via `material_by_id_mut` and explicitly on undo) —
    /// NEVER on a UI mutation counter (the 2026-08-13 memo trap).
    pub(crate) mp_doc_memo: std::collections::HashMap<u32, (u64, crate::material_panel::MaterialPanelDoc)>,
    /// Material-panel layer selection: `material_id -> (channel_kind as u32, layer_index)`.
    ///
    /// OWNED HERE, not in egui's temp store (fix 2026-07-28). It previously lived in
    /// `ui.data()` via `insert_temp`/`get_temp`, which never round-tripped: instrumented over a
    /// live session, 3621 successful WRITEs produced 5276 READs and ZERO of them ever saw a
    /// value, so the docked LAYER PROPERTIES pane always rendered its "Select a layer" empty
    /// state. `insert_temp` is not a reliable cross-frame store for this — the panel's own state
    /// (docs, conflicts) already lives here, so the selection belongs here too.
    pub(crate) mp_layer_sel: std::collections::HashMap<u32, (u32, usize)>,
    /// P3: a layer's "Load…" is in flight — (material id, channel kind, layer
    /// index). When the decoded image's texture-array slot lands
    /// (`apply_map_slot`), the slot id is written into THIS layer (not the
    /// whole channel) and the channel subgraph re-lowers.
    /// `(material, channel, layer index, is_mask)`. `is_mask=true` routes the
    /// decoded slot into the layer's MASK image instead of its content image.
    pub(crate) mp_pending_layer_load:
        Option<(u32, crate::material_panel::model::ChannelKind, usize, bool)>,
    /// P5 user presets — lazily loaded from `material_presets.json` on first
    /// use (`None` = not loaded yet). RUNTIME cache; the file is the truth.
    pub(crate) mp_user_presets: Option<Vec<crate::material_panel::preset::MaterialPreset>>,
    /// P5 destructive preset-apply awaiting confirmation: (material id, preset).
    /// Set when the CURRENT doc has GraphDriven channels or loaded image layers.
    pub(crate) mp_pending_preset: Option<(u32, crate::material_panel::preset::MaterialPreset)>,
    /// P5 "Save as preset…" name prompt: (material id, name buffer).
    pub(crate) mp_preset_naming: Option<(u32, String)>,
    /// P5 user-preset delete awaiting confirmation (index into `mp_user_presets`).
    pub(crate) mp_preset_delete: Option<usize>,
    /// Ctrl+M popup: preset chosen for "+ New Material" (owner D1). Name into
    /// the combined builtin+user list; default "Standard PBR".
    pub(crate) mat_assign_preset: String,
    // Drag-undo flags: save undo once at drag start, reset when drag ends
    pub(crate) undo_drag_nurbs: bool,
    pub(crate) undo_drag_light: bool,
    /// Quick light color popup (C with a lights-only selection, owner 2026-08-07):
    /// (light id, anchor screen pos). Scroll = intensity while open; a viewport
    /// click (or C/Esc) closes it. None = closed.
    pub(crate) light_color_popup: Option<(u32, egui::Pos2)>,
    /// Blender-style "adjust last operation" panel for a just-created poly primitive: lets the user set
    /// segments/radius/height AFTER creation, regenerating the mesh live. `None` when no panel is open.
    pub(crate) poly_create_adjust: Option<PolyCreateAdjust>,
    /// Last cylinder/cone side count (Shift+A adjust). Next create uses this so the user doesn't
    /// re-pick sides every time.
    pub(crate) poly_create_last_sides: u32,
    /// Blender-style redo panel for the last Inset (stays after confirm; see `PolyInsetAdjust`).
    pub(crate) poly_inset_adjust: Option<PolyInsetAdjust>,
    /// Blender-style redo panel for the last Extrude (stays after confirm; see `PolyExtrudeAdjust`).
    pub(crate) poly_extrude_adjust: Option<PolyExtrudeAdjust>,
    /// Live drag for a pipe just created by knife `P` (see `crate::poly_pipe::PipeAdjust`).
    pub(crate) pipe_adjust: Option<crate::poly_pipe::PipeAdjust>,
    /// Live parametric-primitive chord drag (Alt = segments, Shift = round width).
    pub(crate) primitive_drag: Option<crate::poly::primitive_drag::PrimitiveDrag>,
    pub(crate) primitive_drag_kind: Option<crate::poly::primitive_drag::PrimitiveDragKind>,
    /// Blender-style redo panel for the last Bevel (stays after confirm; see `PolyBevelAdjust`).
    pub(crate) poly_bevel_adjust: Option<PolyBevelAdjust>,
    /// Adjust-last-op for NURBS edge fillet (FaceOrigin re-solve from pre-fillet base).
    pub(crate) nurbs_fillet_adjust: Option<NurbsFilletAdjust>,
    /// AUTO_POLYGROUPS: tint faces by polygroup colour in edit-mode overlay.
    pub(crate) poly_show_polygroups: bool,
    /// 2D UV layout over the viewport (Blender UV Editor style).
    pub(crate) show_uv_layout: bool,
    /// UV-square underlay (G5). Runtime-only.
    pub(crate) uv_underlay: crate::mesh_map_bake::UvUnderlaySource,
    /// Fraction of the viewport width the UV editor pane occupies (left side).
    pub(crate) uv_split_frac: f32,
    /// Scene rect the UV split is measured against (set each frame before input).
    pub(crate) uv_host_rect: egui::Rect,
    /// Smart UV Project angle limit (degrees between face normals).
    pub(crate) uv_unwrap_angle_deg: f32,
    /// Island packing margin as fraction of the UV square.
    pub(crate) uv_unwrap_margin: f32,
    /// Max half-angle of a chart's normal cone (degrees).
    pub(crate) uv_unwrap_cone_deg: f32,
    /// What defines an island: `UvUnwrapMethod::as_u32()` (0 = Angle, 1 = Seams only).
    pub(crate) uv_unwrap_method: u32,
    /// Last UV Unwrap result — arms the Tool Parameters redo panel.
    pub(crate) uv_unwrap_adjust: Option<UvUnwrapAdjust>,
    /// Last bake-path reason surfaced to the user, for dedupe. See `bake_diag`.
    pub(crate) last_bake_note: Option<String>,
    /// Per-body bake settings (G1). Missing entry = [`BakeSettings::default`].
    /// Persisted via the `FL4BAKE` tail, never as loose UI scalars.
    pub(crate) mesh_map_settings: std::collections::HashMap<
        core_math::scene_graph::NodeId,
        crate::mesh_map_bake::BakeSettings,
    >,
    /// Wall time of the last ray-cast map, ms.
    pub(crate) last_mesh_map_ms: f32,
    /// Runtime-only viewport cage while hauling Max Front / Rear. Never persisted.
    pub(crate) bake_cage_preview: Option<crate::mesh_map_bake::BakeCagePreview>,
    pub(crate) bake_cage_preview_cache: Option<crate::mesh_map_bake::BakeCagePreviewCache>,
    /// Pin the cage in the viewport (Show cage). Independent of the linger timer.
    pub(crate) bake_cage_preview_pinned: bool,
    /// Named baked-image registry (G4). Pixels ride FL4MATTX; this is the index.
    pub(crate) baked_images: crate::mesh_map_bake::BakedImageRegistry,
    /// Image panel: selected registry name.
    pub(crate) image_panel_selected: Option<String>,
    /// Image panel: in-progress rename buffer.
    pub(crate) image_panel_rename: String,
    /// Blender-style redo panel for the last Circle (stays after confirm; see `PolyCircleAdjust`).
    pub(crate) poly_circle_adjust: Option<PolyCircleAdjust>,
    /// Blender-style redo panel for the last To Sphere (factor slider; see `PolyToSphereAdjust`).
    pub(crate) poly_tosphere_adjust: Option<PolyToSphereAdjust>,
    /// Selected stack slot on a poly body: `(body NodeId, modifier index)`.
    /// Drives edit wire + special boolean cutter wire in the viewport.
    pub(crate) poly_mod_selected: Option<(core_math::scene_graph::NodeId, usize)>,
    /// Modifier card's RESULT-WIRE toggle (the ▦ button). Its OWN state — it used to
    /// flip the global `render.wire_overlay_enabled`, so turning it on silently enabled
    /// the viewport-wide wireframe overlay and the modifier wire followed the user back
    /// into Object mode (owner 2026-08-07: "in object mode the wire frame from modifier
    /// is showing, no wireframe default in object mode"). Two features, two flags: the
    /// global stays the user's explicit Shift+Z viewport overlay; this one is Edit-mode
    /// modifier feedback. Runtime-only — never serialized.
    pub(crate) poly_mod_result_wire: bool,
    /// Memo for modifier-card before→after stats: `((nid, base_sig, stack_hash), per-mod (bv,bf,av,af))`.
    pub(crate) poly_mod_prefix_stats: Option<((u64, u64, u64), Vec<(u32, u32, u32, u32)>)>,
}

/// Parameters for the floating "adjust last create" panel on a poly primitive. UI-only state (undo is
/// captured by regenerating the node mesh + writing the node transform + `transaction_snapshot_only` on
/// release, not from this struct).
///
/// Geometry params (segments/vsegments/rings/radius/height/minor) drive `poly_build_mesh` — the node mesh.
/// Placement params (pos/scale/rot) drive the NODE transform, NOT the mesh, so they compose with the gizmo.
#[derive(Clone)]
pub(crate) struct PolyCreateAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// The primitive's parameters — ONE typed source of truth.
    ///
    /// This replaced a flat bag (`kind: String` + segments/vsegments/rings/radius/height/minor)
    /// where most fields were meaningless for any given kind, the geometry dispatched on a string,
    /// and the same parameters were hand-maintained in three places. See
    /// `docs/planning/poly/PARAMETRIC_PRIMITIVES.md`.
    pub(crate) spec: core_math::primitive_spec::PrimitiveSpec,
    pub(crate) pos: glam::Vec3,   // node local_position (Position X/Y/Z)
    pub(crate) scale: glam::Vec3, // node local_scale (Size / Scale X/Y/Z)
    pub(crate) rot: glam::Vec3,   // node local_rotation Euler degrees YXZ (aligned to selection normal)
    /// When set, the primitive sits ON this surface (center + normal) — no intersection.
    /// Height/radius changes re-offset `pos` along the normal so it stays flush on top.
    pub(crate) place_on: Option<PolyCreatePlaceOn>,
    /// True after a live param change since the last settle — release only pushes undo when dirty
    /// (clicking a slider without changing it must not open an empty history entry).
    pub(crate) needs_settle: bool,
}

/// World-space surface the poly primitive was created on (selection face/edge plane).
#[derive(Clone, Copy)]
pub(crate) struct PolyCreatePlaceOn {
    /// Point on the selection surface (world).
    pub(crate) point: glam::Vec3,
    /// Outward unit normal (world) — primitive +Y aligns to this; sits on the +normal side.
    pub(crate) normal: glam::Vec3,
}

/// Interactive library body place (after drag-drop or Insert).
/// Move with mouse, hold Ctrl to align to surface, scroll to resize, LMB/Enter confirm, Esc cancel.
#[derive(Clone)]
pub(crate) struct LibraryPlaceSession {
    pub(crate) node_id: core_math::scene_graph::NodeId,
    /// Uniform scale multiplier applied on top of [`Self::base_scale`].
    pub(crate) scale_mul: f32,
    /// Node local_scale captured at insert (package scale).
    pub(crate) base_scale: glam::Vec3,
    /// Node local_rotation captured at insert (restored when Ctrl is released).
    pub(crate) base_rot: glam::Vec3,
    /// Swallow the drop-release LMB so the same gesture does not confirm immediately.
    pub(crate) ignore_lmb: bool,
}

/// Blender's "Adjust Last Operation" (F9) redo panel for **Inset** — it STAYS after the op confirms so
/// the settings can be tweaked and the inset RE-RUNS live (the owner: "the tool property goes away when
/// I click to confirm; in Blender it stays there so I can change settings"). Same lifecycle as
/// [`PolyCreateAdjust`]: live regen on change, ONE settled undo on release, auto-dismiss when the node
/// goes away / another object becomes active.
#[derive(Clone)]
pub(crate) struct PolyInsetAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Face ids the inset ran on — valid in `before` (the post-inset mesh no longer has them).
    pub(crate) faces: Vec<u32>,
    /// The mesh BEFORE the inset. Every re-run starts from this, so tweaking never stacks insets.
    pub(crate) before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    pub(crate) thickness: f32,
    pub(crate) depth: f32,
    /// false = Region ("group faces", Blender's I default), true = Individual (I,I).
    pub(crate) individual: bool,
    /// Live change since last settle — release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
}

/// Blender's "Adjust Last Operation" (F9) redo panel for **Extrude** -- it STAYS after the
/// drag confirms so Depth / Wall cleanup can be tweaked and the extrude RE-RUNS live
/// (owner 2026-08-04: "the tool parameter needs to stay on after the user accepts so we
/// can change setting"). Same lifecycle as [`PolyInsetAdjust`]: every change re-runs from
/// `before` (never stacks extrudes), ONE settled undo on release, auto-dismiss when the
/// node goes away or another object becomes active.
#[derive(Clone)]
pub(crate) struct PolyExtrudeAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Region face ids valid in `before`.
    pub(crate) faces: Vec<u32>,
    /// The mesh BEFORE the extrude. Every re-run starts from this.
    pub(crate) before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Drag direction actually used (region normal / world axis / edge axis, already resolved).
    pub(crate) dir: glam::Vec3,
    /// Signed depth; negative = intrude.
    pub(crate) offset: f32,
    /// Mirrors `tweak.extrude_wall_cleanup` (0 Off / nonzero On).
    pub(crate) wall_cleanup: u8,
    /// Live change since last settle -- release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
}

/// Blender-style "Adjust Last Operation" for **NURBS edge fillet**.
/// Every law change re-solves from `faces_before` via
/// `resolve_edges_fillet_from_base_radius` — never compounds (PLAN §4b / Decision 5).
/// Stores full [`core_math::nurbs_fillet::Radius`] so Linear/Law survive re-edit
/// (not collapsed to Const peak).
#[derive(Clone)]
pub(crate) struct NurbsFilletAdjust {
    pub(crate) solid: core_math::scene_graph::NodeId,
    /// Pre-fillet faces (tool snapshot). Every re-run starts here.
    pub(crate) faces_before: Vec<core_math::nurbs::NurbsFace>,
    /// Solid edges that were filleted: (face_idx, edge_id).
    pub(crate) edges: Vec<(u32, u8)>,
    /// Full radius law recovered from FaceOrigin / live tool (Const | Linear | Law).
    pub(crate) radius: core_math::nurbs_fillet::Radius,
    pub(crate) max_radius: f32,
    /// Live change since last settle — release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
}

/// Blender-style "Adjust Last Operation" redo panel for **Bevel** — same lifecycle as
/// [`PolyInsetAdjust`]. Every param change re-runs `bevel_op` from `before` so width/segments/shape
/// never compound (resize-bevel). Unbevel of the last op = width → 0 (restore `before`).
#[derive(Clone)]
pub(crate) struct PolyBevelAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Mesh BEFORE the bevel. Every re-run starts from this.
    pub(crate) before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// `content_signature()` of the live mesh after the last apply. Re-running from `before`
    /// is only legal while the live mesh still matches — any later modeling makes this panel
    /// stale, and applying it would silently revert that work. Checked on interaction (O(mesh)
    /// hash — never per-frame, see overlay_lines.rs), refreshed after every re-apply.
    pub(crate) post_signature: u64,
    /// Half-edge ids beveled (edge mode). Empty when vertex bevel (`verts` non-empty).
    pub(crate) edges: std::collections::HashSet<u32>,
    /// Vertex ids for affect=VERTICES. When non-empty, `edges` is ignored.
    pub(crate) verts: std::collections::HashSet<u32>,
    pub(crate) width: f32,
    pub(crate) segments: u32,
    pub(crate) shape: f32,
    pub(crate) clamp: bool,
    /// Live change since last settle — release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
}

/// Blender's "Adjust Last Operation" (F9) redo panel for **Circle** (LoopTools). Same lifecycle as
/// [`PolyInsetAdjust`]: every change re-runs the op from `before` (so tweaking never stacks circles),
/// settles into ONE undo entry on release, auto-dismisses when the node goes away.
///
/// This is also where the owner's eventual RADIUS HANDLE plugs in: `radius` here is the same number a
/// gizmo would drive, and `circle_region` already returns each boundary vertex's unit radial direction
/// so a drag can slide `pos = center + dir * radius` without re-running the fit.
#[derive(Clone)]
pub(crate) struct PolyCircleAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Face ids the circle ran on (Face-mode). Still valid AFTER the op — `circle_region` is a pure
    /// reshape and preserves every id (unlike inset, which rebuilds the region's faces). EMPTY when the
    /// op came from an edge selection (see `edges`).
    pub(crate) faces: Vec<u32>,
    /// `Some` when the op came from an EDGE selection: the HalfEdgeId u32s of the selected loop. The redo
    /// panel re-runs the EDGE path (`circle_edge_loop`) over these instead of `faces`, so tweaking Radius
    /// / Regular / Flatten keeps using the same producer the op fired through (one path per domain).
    pub(crate) edges: Option<Vec<u32>>,
    /// The mesh BEFORE the circle. Every re-run starts from this.
    pub(crate) before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// `None` = Blender's auto radius (least-squares best fit); `Some(r)` = custom_radius override.
    pub(crate) radius: Option<f32>,
    /// The auto-fit radius of the first loop — seeds the slider when the user enables `radius`.
    pub(crate) fit_radius: f32,
    /// Blender's `flatten` (default true): land the loop exactly in its best-fit plane.
    pub(crate) flatten: bool,
    /// Blender's `regular` (default true): spread vertices at equal angles.
    pub(crate) regular: bool,
    /// AZRA extension (Blender does not relax): Laplacian passes over the region's interior.
    pub(crate) relax_passes: u32,
    /// Live change since last settle — release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
    /// Circle center in mesh/local space (for viewport radius handle).
    pub(crate) center: glam::Vec3,
    /// Unit radial dir in the fit plane — handle sits at `center + handle_dir * r`.
    pub(crate) handle_dir: glam::Vec3,
    /// Fit plane normal (for ray-plane hit while dragging the handle).
    pub(crate) plane_normal: glam::Vec3,
    /// True while LMB is dragging the radius handle in the viewport.
    pub(crate) handle_dragging: bool,
}

/// Blender-style "Adjust Last Operation" redo panel for To Sphere — a single Factor slider (0..1) that
/// re-runs the spherize from the PRE-op snapshot each frame (never stacks) and settles into one undo on
/// release. Mirrors [`PolyCircleAdjust`]. `verts` are gathered from whatever poly mode ran it (Vertex =
/// selected verts, Edge = the edges' endpoints, Face = the faces' verts) so the op works in every mode.
#[derive(Clone)]
pub(crate) struct PolyToSphereAdjust {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// The vertex ids the op ran on (mode-independent — resolved to verts at op time). Preserved across
    /// the pure position edit, so re-runs stay valid.
    pub(crate) verts: Vec<u32>,
    /// The mesh BEFORE the spherize. Every re-run starts from this so tweaking the factor never stacks.
    pub(crate) before: std::sync::Arc<core_math::half_edge::HalfEdgeMesh>,
    /// Blend toward the sphere: 0 = original, 1 = full sphere (Blender `transform.tosphere`).
    pub(crate) factor: f32,
    /// Live change since last settle — release only pushes undo when dirty.
    pub(crate) needs_settle: bool,
}

impl Default for UiWindowState {
    fn default() -> Self {
        Self {
            startup_toast: None,
            toasts: Vec::new(),
            command_palette: CommandPaletteState::default(),
            _ws_dragging: None,
            ws_add_popup: None,
            ws_add_search: String::new(),
            ws_context_menu: None,
            show_profiler: false,
            show_profiler_overlay: false,
            show_perf_hud: false,
            show_bevel_debug: false,
            show_keybind_settings: false,
            show_experimental: false,
            show_about: false,
            show_shortcut_editor: false,
            show_add_popup: false,
            add_popup_sdf_subbody: false,
            add_popup_pos: egui::Pos2::ZERO,
            add_search: String::new(),
            add_search_focus: false,
            add_sec_user_open: 0,
            show_tool_panel: true,
            show_tweak_panel: false,
            convert_sdf_dialog: None,
            mesh_cleanup_dialog: None,
            mesh_cleanup_flatten: true,
            mesh_cleanup_merge: true,
            // Blender Limited Dissolve default is 5°; owner asked 4.5° as the app default
            // (panel + Ctrl+X empty-edit cleanup + Limited Dissolve command).
            mesh_cleanup_dissolve_deg: 4.5,
            mesh_cleanup_merge_dist: 1e-4,
            mesh_cleanup_degenerate: 1e-4,
            mesh_cleanup_loose_verts: true,
            mesh_cleanup_loose_edges: true,
            mesh_cleanup_loose_faces: false,
            mesh_cleanup_fill_holes: 0,
            mesh_cleanup_planar_iters: 0,
            mesh_cleanup_split_nonplanar_deg: 0.0,
            mesh_cleanup_split_concave: false,
            mesh_cleanup_recalc_normals: false,
            mesh_cleanup_live: true, // default ON — tweak knobs and see the mesh update
            mesh_cleanup_result: None,
            convert_sdf_res: 128,
            tweak_search: String::new(),
            show_layout_settings: false,
            show_panel_list: false,
            request_ui_layout_dump: false,
            show_token_board: false, // Gate 0 token board - enable when wired
            show_export_window: false,
            show_gltf_export: false,
            gltf_export_path: String::new(),
            gltf_export_status: None,
            show_texgen_window: false,
            show_node_editor: false,
            show_macro_manager: false,
            show_stress_test: false,
            stress_test_results: Vec::new(),
            diag_scene_active: false,
            debug_render_mode: 0,
            diag_saved_dna: Vec::new(),
            diag_saved_mods: Vec::new(),
            show_bool_edges: false, show_overlay_popover: false, show_pt_settings: false,
            show_unsaved_dialog: false, show_body_library: false,
            body_library_confirm_delete: None,
            body_library_entries: Vec::new(),
            body_library_needs_scan: true,
            body_library_scanned_once: false,
            body_library_last_folder: String::new(),
            library_place: None,
            library_save_nid: None, library_save_name: String::new(), library_save_folder: String::new(),
            library_save_name_focused: false,
            show_mat_assign: false, mat_assign_pos: egui::Pos2::ZERO,
            mat_assign_search: String::new(), mat_assign_selected: None,
            mat_assign_new_name: String::new(),
            mp_user_presets: None,
            mp_pending_preset: None,
            mp_preset_naming: None,
            mp_preset_delete: None,
            mat_assign_preset: "Standard PBR".into(),
            export_desc: String::new(),
            export_resolution: 64,
            export_quads: true,
            export_sharp: true,
            export_path: String::new(),
            export_status: None,
            last_mesh_stats: None,
            export_rx: None,
            export_display_res: true, // GATE 4: export the display mesh by default
            export_pending: None,
            texgen_config: core_math::texgen::TexGenConfig::preset_scratched_metal(),
            texgen_preset_name: "scratched_metal".into(),
            texgen_status: None,
            rebinding_action: None,
            keybind_search: String::new(),
            keybind_conflicts: Vec::new(),
            z_pie: Default::default(),
            tab_pie: crate::pie_menu::PieMenuState::default(),
            view_pie: Default::default(),
            reset_pie: Default::default(),
            f_pie: Default::default(),
            overlay_pie: Default::default(),
            d_pie: Default::default(),
            d_pie_click_mode: false,
            pivot_center_menu_open: false,
            pivot_center_menu_pos: egui::Pos2::ZERO,
            expose_selection_pivot: false,
            t_pie: Default::default(),
            bool_pie: Default::default(),
            bool_pie_keep_cutter: false,
            bool_cutter_material: Default::default(),
            poly_boolean_adjust: None,
            sdf_bool_pie: Default::default(),
            spine_corner_pie: Default::default(),
            res_pie: Default::default(),
            res_pie_sticky: false,
            sdf_bool_pie_sticky: false,
            sdf_special_pie: Default::default(),
            sdf_special_pie_sticky: false,
            sym_pie: Default::default(),
            align_pie: Default::default(),
            merge_pie: Default::default(),
            sym_pie_params: Default::default(),
            pie_eat_click: false,
            pie_mode: crate::pie_menu::PieMode::Sdf,
            sdf_cycle_mode: false,
            last_view_idx: None,
            last_edit_mode: None,
            last_poly_edit_mode: None,
            q_menu_open: false,
            q_menu_pos: egui::Pos2::ZERO,
            ctx_menu_open: false,
            ctx_menu_pos: egui::Pos2::ZERO,
            ctx_menu_prim: None,
            ctx_menu_search: String::new(),
            ctx_menu_focus_search: false,
            object_link_gesture: None,
            object_link_menu: None,
            shape_swap_pie_open: false,
            shape_swap_pie_pos: egui::Pos2::ZERO,
            shape_swap_pie_idx: None,
            prof_frame_times: std::collections::VecDeque::with_capacity(300),
            prof_pass_times: [0.0; 7],
            prof_pass_names: ["Raymarch", "Deferred", "SSR", "Fog", "Bloom", "Tonemap", "Upsample"],
            prof_gpu_pass_times: [0.0; 20],
            prof_gpu_timestamps_available: false,
            prof_gpu_mem_mb: 0.0,
            prof_gpu_mem_textures_mb: 0.0,
            prof_gpu_mem_buffers_mb: 0.0,
            prof_gpu_mem_shadow_mb: 0.0,
            prof_gpu_mem_total_mb: 0.0,
            prof_gpu_mem_last_update: None,
            prof_render_res: (0, 0),
            show_cpu_frame_profiler: false,
            cpu_frame_times: std::collections::VecDeque::with_capacity(120),
            cpu_section_ms: [0.0; 11],
            cpu_section_names: [
                "Input",
                "Mesh",
                "Dispatch",
                "Submit",
                "Overlays",
                "Layout",
                "Dirty",
                "ModeAuto",
                "Poll",
                "Setup",
                "ToastMenu",
            ],
            poly_mat_uniform_cache: None,
            egui_style_key: 0,
            ui2_dock_paint_ms: 0.0,
            pending_action_after_save: None,
            deferred_action_countdown: 0,
            node_graph: crate::shader_nodes::NodeGraph::default(),
            node_drag_state: crate::shader_nodes::NodeDragState::default(),
            node_graph_target: None,
            node_graphs: std::collections::HashMap::new(),
            material_node_graphs: std::collections::HashMap::new(),
            material_panel_docs: std::collections::HashMap::new(),
            mp_confirm: None,
            mp_bake_conflicts: std::collections::HashMap::new(),
            mp_doc_memo: std::collections::HashMap::new(),
            mp_layer_sel: std::collections::HashMap::new(),
            mp_pending_layer_load: None,
            undo_drag_nurbs: false,
            undo_drag_light: false,
            light_color_popup: None,
            poly_create_adjust: None,
            poly_create_last_sides: 16,
            poly_inset_adjust: None,
            poly_extrude_adjust: None,
            pipe_adjust: None,
            primitive_drag: None,
            primitive_drag_kind: None,
            poly_bevel_adjust: None,
            nurbs_fillet_adjust: None,
            poly_show_polygroups: false,
            show_uv_layout: false,
            uv_underlay: crate::mesh_map_bake::UvUnderlaySource::Checker,
            uv_split_frac: 0.42,
            uv_host_rect: egui::Rect::NOTHING,
            uv_unwrap_angle_deg: 66.0,
            uv_unwrap_margin: 0.02,
            uv_unwrap_cone_deg: 75.0,
            uv_unwrap_method: 0,
            uv_unwrap_adjust: None,
            last_bake_note: None,
            mesh_map_settings: std::collections::HashMap::new(),
            last_mesh_map_ms: 0.0,
            bake_cage_preview: None,
            bake_cage_preview_cache: None,
            bake_cage_preview_pinned: false,
            baked_images: crate::mesh_map_bake::BakedImageRegistry::default(),
            image_panel_selected: None,
            image_panel_rename: String::new(),
            poly_mod_selected: None,
            poly_mod_result_wire: false,
            poly_mod_prefix_stats: None,
            poly_circle_adjust: None,
            poly_tosphere_adjust: None,
        }
    }
}

/// Last UV Unwrap outcome — what the redo panel reports back so the user can judge the result
/// (island count is the single most useful number: 1 island on a closed shape means it folded).
#[derive(Clone, Copy, Debug)]
pub(crate) struct UvUnwrapAdjust {
    pub(crate) islands: u32,
    pub(crate) corners: u32,
}
