// core_engine/app/src/main.rs
#[macro_use]
mod profiling;
// agent, ai_client, claude, material_dict live in app_ai crate
pub(crate) use app_ai::{agent, ai_client, claude, material_dict};

/// Body-local prim coordinate space — SINGLE SOURCE OF TRUTH (was duplicated in 15 files).
/// `true` in production: prim positions are body-local. Flip to `false` only for legacy
/// world-space debugging. See CLAUDE.md "Prim Position Space".
pub(crate) const BODY_LOCAL_PRIMS: bool = true;
mod app_commands;
mod app_poll;
mod app_poll_ai;
mod app_poll_voxel;
mod app_poll_pipeline;
mod app_poll_gpu;
mod app_poll_bake;
mod app_poll_bake_poll;
mod app_poll_misc;
mod ai_texture;
mod ai_beauty;
mod ao_bake_poll;
mod bake_utils;
mod env_ibl;
mod file_dialog;
#[cfg(test)] mod dc_baseline; // Gate 0 — DC-consolidation baseline harness (golden/roundtrip/pick/JSON)
mod body_volume_cache;
mod bool_cut;
mod clip_curve_tool;
mod curve_point_edit;
mod spine_point_edit;
mod camera;
mod content_library;
mod library_folders;
mod library_index_cache;
mod library_scan;
mod create_tool;
mod drawing;
mod draw_gizmo;
mod draw_create;
mod draw_viewport_overlays;
mod draw_workplane;
mod draw_selection;
mod draw_light;
mod draw_transform;
mod draw_poly;
mod draw_poly_wire;
mod overlay_lines;
mod overlay_faces;
mod edge_detect;
mod gizmo;
mod gpu_init; pub(crate) use gpu_init::GpuInitState;
mod panic_safety;
// gpu_jobs, gpu_timestamps, gpu_worker live in app_gpu crate
pub(crate) use app_gpu::{gpu_jobs, gpu_timestamps, gpu_worker, WorkerGpu};
mod input;
mod input_keyboard;
mod input_viewport;
mod input_select;
mod input_sculpt;
mod voxel_surface_sculpt;
mod surface_validate; // spike detector (fuzz harness + AZRA_SCULPT_VALIDATE live validator)
#[cfg(test)] mod surface_fuzz; // seeded spike-hunting fuzz harness over the REAL surface pipeline
mod transform;
mod transform_targets;
mod mat_preview;
mod pivot;
mod action_center;
#[allow(dead_code)]
mod shader_nodes;
mod node_graph_bake;
/// GPU curvature kernel dispatch (GPU_CURVATURE_KERNEL_PLAN.md P1) — the CPU half of
/// `volume_bake.wgsl::curvature_eval`, the third consumer of `eval_scene_at`.
mod gpu_curvature;
/// GPU material-map bake dispatch (GPU_MATERIAL_BAKE_PLAN.md P1) — blocking test/manual-verify
/// entry over `material_bake.wgsl`; production integration must use the async cross-frame fence
/// (see the module header), never call it from a per-frame path.
mod gpu_material_bake;
/// Process-wide GPU unit-test lock (O6 class). Prevents concurrent request_device from
/// dc_baseline + sdf_gpu_mesh aborting the process under default cargo test threads.
#[cfg(test)]
mod gpu_test_harness;
mod smart_snap;
mod symmetry;
mod ui_theme;
mod ui_fields;      // Gate-1 reusable field kit (permanent)
mod ui_menus;       // ONE menu contract (close on explicit close / outside click / Esc)
mod tool_params;    // Gate-2 data-driven Tool Parameters catalog + render (permanent)
mod tool_params_sculpt; // Live · Sculpt: Blender header fields (not just radius/strength)
mod properties_panel; // Gate-5 Properties (Object/Modifiers/SDF/Material) stub
mod apply_transform; // Freeze / Apply Location·Rotation·Scale·All (poly + SDF)
mod object_link;     // Object-mode long-press LMB → Parent / Match Location·Rotation·Scale
mod library_panel;    // Gate-7 Library bottom overlay (mockup #dock-bottom)
mod ui2;
mod sym_pie;
mod align_pie;
mod merge_pie;
mod material_assign;
mod material_panel;  // Material parameter panel P0 — doc model + lowering seam (MATERIAL_PANEL_SPEC.md)
mod material_editor;
mod material_import;
mod material_textures;
mod menus;
mod menu_shared;
/// Scene ops — one file per function (node-ready). See ui2/INTEGRATION_RULES §0.
mod ops;
mod outliner;
mod outliner_draw;
mod outliner_panel;
mod outliner_dnd;
mod outliner_drop_slots;
mod outliner_draw_body;
mod outliner_draw_light;
mod outliner_draw_material;
mod outliner_draw_poly;
mod project;
mod project_save;
mod project_load;
mod project_new;
mod properties;
mod properties_light;
mod properties_material_node;
mod properties_poly;
mod properties_nurbs;
mod properties_sdf;
mod properties_sdf_transform;
mod properties_sdf_shape;
mod properties_sdf_material;
mod properties_viewport;
mod ui2_history;
mod ui2_channels;
mod properties_render;
mod properties_profiler;
mod properties_compositor;
mod workspace;
mod voxel_gpu_mesh;
mod sdf_gpu_mesh; // Phase G — GPU QEF Dual-Contouring mesher (G0 harness + parity test)
mod workplane;
mod ui_windows;
mod win_settings;
mod win_ai;
mod win_node_editor;
mod seldiag;
mod visdiff;
mod parity_gate;
mod bridgediag;
mod nurbsdiff;
mod soak;
mod win_export;
mod win_export_gltf;
mod win_texture;
mod win_grid_workplane;
mod win_dialog;
mod win_tweaks;
mod win_layout;
mod win_command;
mod ui_panels;
mod brush_library;
mod brush_tip_lib;
mod convert;
mod nurbs_tools;
mod pie_menu;
mod poly_selection;
mod poly_select_gpu;
mod light_depth_probe;
mod poly;
mod bake_diag;      // "why is this map empty?" reason channel for the bake path
mod mesh_map_bake;  // Substance-style one-button mesh map bake → texture slots
mod poly_curvature; // κ on the half-edge mesh → CurvGrid (docs/planning/render/POLY_CURVATURE_PLAN.md P3/P4)
mod poly_drag_tools;
mod poly_groups;
mod poly_knife;
mod poly_prop_edit;
mod poly_pipe;
mod experimental;
mod face_ops;
mod panel;
mod creation;
mod dev_tools;
mod stress_test;
mod blend_dispatch;
mod dirty;
mod frame_scheduler;
mod instance_actions;
mod array_dup;
mod extract_intersection;
mod stack_sdf;
mod macro_system;
mod mirror_actions;
mod outliner_actions;
mod prim_actions;
mod body_library;
mod cache_state;
mod cluster_bake;
mod clipboard;
mod hierarchy_order;
mod input_system;
mod scene_actions;
mod terrain; // Mesh Terrain Phase 1 — island + tile constants (ADR-TERRAIN-001)
mod light_pack;
mod light_cluster;
mod scene_state;
mod project_state;
mod selection;
mod selection_unified;
mod snapping;
mod tool_mode;
mod tool_settings;
mod undo;
mod ui_update;
mod viewport;
mod viewport_header;
mod viewport_hints;
mod viewport_render;
mod viewport_overlays;
mod vis_recorder;
mod warming;

// ============================================================================
// DE-GOD ROUND 1 — per-pillar module declarations.
// Plan: docs/planning/APP_CRATE_DEGOD_PLAN.md. Each window adds ONLY the `mod` /
// `pub(crate) use` lines under ITS OWN header below, so the four worktrees never
// conflict on this region. Sync `master` into your branch before starting.
//
// --- master  (slice 29: main.rs type-def extraction) ---
mod gpu_ctx; pub(crate) use gpu_ctx::*;
mod voxel_sculpt_types; pub(crate) use voxel_sculpt_types::*;
mod sculpt_state; pub(crate) use sculpt_state::{SculptState, SculptStrokeMethod};
mod app_state; pub(crate) use app_state::*;
// --- master  (slice 30: main.rs constructor + bridge helpers extraction) ---
mod app_new;
mod user_default_fields; // startup apply of per-user "Set as default" toggles
// --- master  (slices 14-16: viewport_render.rs cohesive-piece extraction) ---
mod viewport_render_mode; pub(crate) use viewport_render_mode::*;
mod viewport_render_mode_auto;
#[cfg(feature = "cuda")] mod cuda_render_dispatch;
// --- master  (slices 17-18: raster_render.rs mesh-prep + shared-draw-helper extraction) ---
// raster_common holds free functions (draw_resident_pool*, draw_sdf_resident_slot, body_aabb_bounds)
// shared by the G-buffer + picking passes; consumers import them explicitly via
// `use crate::raster_common::{...}`. raster_mesh_prep is an `impl AzraApp` module (methods resolve
// crate-wide, no re-export needed).
mod raster_common;
mod raster_mesh_prep;
mod mesh_presentation; // GATE 4 — MeshPresentation facade + shared CPU-materialize job
mod normal_bake; // High→low normal bake preview (Quick Normal Bake)
mod pt_scene;
mod pt_native;
// raster_gbuffer (slice 19: dispatch_raster_pass) and raster_pick (slice 20: pick_node_at /
// poll_pending_pick) are `impl AzraApp` modules — methods resolve crate-wide, no re-export needed.
mod raster_gbuffer;
mod probe_bake;
mod draw_stream;
mod raster_pick;
// --- master  (multi_gpu: main.rs multi-GPU settings extraction) ---
mod multi_gpu;
// --- master  (slice 32: eframe::App update() + init_gpu_ctx extraction) ---
mod app_update;

// --- general (slice 8: app_poll pipeline-compile split) ---

// --- sculpt  (slice 1: voxel_gpu_mesh brush split) ---

// --- poly    (round-1 de-god of poly_selection.rs, optional) ---

// --- lab     (mesh-SDF operand spike — docs/planning/lab/poly-mesh-sdf-operand.md) ---
mod lab_mesh_sdf; pub(crate) use lab_mesh_sdf::LabMeshSdfState;
// ============================================================================
use eframe::egui;
use eframe::wgpu;
use glam::{Quat, EulerRot, Vec3};
use graphics::LightConfig;
use core_math::sdf_volume::{SdfVolume, BrushSettings};
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, scene_sdf, eval_scene, BoxBevel};
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};
use graphics::gpu_volume::SdfGpuVolume;
use std::collections::{HashSet, HashMap, VecDeque};

/// Gate for the per-frame performance `eprintln!` diagnostics ([SPIKE] update(), [PollDetail], etc.).
/// OFF by default — every sculpt frame is >10ms, so these flood the console. Flip to `true` to profile.
/// Env `AZRA_PERF_SPAM=1` also enables at process start (checked once via OnceLock).
pub(crate) const PERF_SPAM: bool = false;

/// Runtime gate: compile-time PERF_SPAM OR env AZRA_PERF_SPAM=1.
#[inline]
pub(crate) fn perf_spam_on() -> bool {
    if PERF_SPAM {
        return true;
    }
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| std::env::var_os("AZRA_PERF_SPAM").is_some())
}

/// BAKE PROFILER (enable with env `AZRA_BAKE_PROF=1`). Accounts each Object-Mode bake readback as either
/// FAST (GPU field-direct / lazy — no CPU copy) or SLOW (a 64MB GPU→CPU `to_vec`), and prints ONE easy-to-
/// paste summary line every 10 edits. Zero cost when the env var is unset. Call `bakeprof_record(is_fast, ms)`
/// from the readback branches; `is_fast=false` passes the measured copy time. The summary tells the whole
/// story in one line: how many edits hit the slow per-edit copy and how expensive it is.
pub(crate) fn bakeprof_record(is_fast: bool, slow_copy_ms: f64) {
    if std::env::var_os("AZRA_BAKE_PROF").is_none() { return; }
    use std::cell::RefCell;
    thread_local! {
        // (fast_count, slow_count, slow_ms_total, slow_ms_max)
        static ACC: RefCell<(u32, u32, f64, f64)> = RefCell::new((0, 0, 0.0, 0.0));
    }
    ACC.with(|c| {
        let mut c = c.borrow_mut();
        if is_fast { c.0 += 1; } else { c.1 += 1; c.2 += slow_copy_ms; c.3 = c.3.max(slow_copy_ms); }
        let total = c.0 + c.1;
        if total >= 10 {
            let avg_slow = if c.1 > 0 { c.2 / c.1 as f64 } else { 0.0 };
            eprintln!(
                "[BAKEPROF] last {} edits: {} FAST (GPU, no copy) | {} SLOW (64MB CPU copy, avg {:.1}ms, worst {:.1}ms)  <-- SLOW count is the per-edit lag",
                total, c.0, c.1, avg_slow, c.3);
            *c = (0, 0, 0.0, 0.0);
        }
    });
}


// ==========================================================================
// Windows WndProc subclass — intercepts WM_SYSKEYDOWN for Alt+letter keys.
// Currently unused (gizmo uses click-based flow). Kept for future Alt shortcuts.
// To re-enable: uncomment the module and call ensure_installed() in raw_input_hook.
/*
#[cfg(target_os = "windows")]
pub(crate) mod win_kb_hook {
    use std::sync::atomic::{AtomicU8, AtomicIsize, Ordering};

    pub const BIT_G: u8 = 1;
    pub const BIT_R: u8 = 2;
    pub const BIT_S: u8 = 4;
    pub static PENDING: AtomicU8 = AtomicU8::new(0);
    static ORIG_WNDPROC: AtomicIsize = AtomicIsize::new(0);

    type HWND = *mut std::ffi::c_void;
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> HWND;
        fn CallWindowProcW(prev: isize, hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize;
        fn PostMessageW(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> i32;
    }
    #[cfg(target_pointer_width = "64")]
    extern "system" {
        fn SetWindowLongPtrW(hwnd: HWND, index: i32, new: isize) -> isize;
    }
    #[cfg(target_pointer_width = "32")]
    unsafe fn SetWindowLongPtrW(hwnd: HWND, index: i32, new: isize) -> isize {
        extern "system" { fn SetWindowLongW(hwnd: *mut std::ffi::c_void, i: i32, v: i32) -> i32; }
        SetWindowLongW(hwnd, index, new as i32) as isize
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_SYSKEYDOWN: u32 = 0x0104;
    const WM_SYSCHAR: u32    = 0x0106;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_KEYMENU: usize  = 0xF100;
    const WM_NULL: u32       = 0x0000;

    unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
        if msg == WM_SYSCOMMAND && (wp & 0xFFF0) == SC_KEYMENU { return 0; }
        if msg == WM_SYSKEYDOWN {
            let is_repeat = (lp as u32 >> 30) & 1 != 0;
            if !is_repeat {
                let bit = match wp as u32 { 0x47 => BIT_G, 0x52 => BIT_R, 0x53 => BIT_S, _ => 0 };
                if bit != 0 {
                    PENDING.fetch_or(bit, Ordering::SeqCst);
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    return 0;
                }
            } else { match wp as u32 { 0x47 | 0x52 | 0x53 => return 0, _ => {} } }
        }
        if msg == WM_SYSCHAR { match (wp as u32) | 0x20 { 0x67 | 0x72 | 0x73 => return 0, _ => {} } }
        CallWindowProcW(ORIG_WNDPROC.load(Ordering::SeqCst), hwnd, msg, wp, lp)
    }

    pub fn ensure_installed() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            unsafe {
                let title_wide: Vec<u16> = "FourthLayer AI".encode_utf16().chain(std::iter::once(0)).collect();
                let hwnd = FindWindowW(std::ptr::null(), title_wide.as_ptr());
                if hwnd.is_null() { return; }
                let orig = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_proc as *const () as isize);
                if orig != 0 { ORIG_WNDPROC.store(orig, Ordering::SeqCst); }
            }
        });
    }
}
*/

// The binary entry point. All boot logic (panic hook, wgpu native-backend config, run_native) +
// `GpuInitState` live in `gpu_init.rs` (slice 32b) — the crate root keeps only this thin `fn main`.
fn main() -> eframe::Result {
    crate::gpu_init::run()
}

// GpuInfo moved to app_state.rs (slice 29). GpuInitState + fn main body moved to gpu_init.rs (slice 32b).

// map_state module + the GpuCtx family (PendingBake, PendingBrickFill, PendingPick,
// PendingSurfacePick, DcJobResult, PendingDcJob, PipelineReady, PipelineRegistry,
// BindGroupCache, BufferPool, GpuCtx, PendingScreenshot) moved to gpu_ctx.rs (slice 29).

#[allow(dead_code)]  // types.rs contains infrastructure types prepared for future migration
mod types;
mod viewport_frame;
pub(crate) use types::*;
pub(crate) use app_types::KeyBindings;
pub(crate) use app_types::keybindings;

// InteractionEpisode + DiagState moved to app_state.rs (slice 29).
// UndoKind + the voxel-sculpt data types (VoxelUndoStack, VoxelColorUndoStack, VoxelObject,
// MeshGrab, PendingGrab, VoxelBrushSettings, SculptProfile, VoxelRebuildMsg, PendingVoxelRebuild)
// moved to voxel_sculpt_types.rs (slice 29).

/// High→low **normal bake preview** (Quick Normal Bake).
/// LO mesh is shaded with normals transferred from the high/raw extract.
/// Dismissed when the camera moves (after grace) — restores LO fold normals.
#[derive(Clone)]
pub(crate) struct QuickNormalGhost {
    pub nid: core_math::scene_graph::NodeId,
    /// High-source extract res label.
    pub res: u32,
    pub cam_orbit: glam::Quat,
    pub cam_dist: f32,
    pub cam_target: glam::Vec3,
    /// Frames left before camera-dismiss arms.
    pub grace_frames: u32,
    /// High source: "raw" | "dc" | "grid".
    pub source: &'static str,
    /// Low LO triangle count / high hit stats for UI.
    pub low_tris: u32,
    pub high_tris: u32,
    pub hit_pct: u32,
}

// ==================================================================
// Application
// ==================================================================

/// In-flight HDR decode + mip build (worker thread → poll).
pub(crate) struct PendingEnvLoad {
    pub path: std::path::PathBuf,
    pub label: String,
    pub rx: std::sync::mpsc::Receiver<Result<(Vec<f32>, Vec<f32>, u32, u32), String>>,
}

/// One-slot cache of the last fully prepared env (path → base + mips + size).
pub(crate) struct PreparedEnvCache {
    pub path: std::path::PathBuf,
    pub pixels: Vec<f32>,
    pub mips: Vec<f32>,
    pub w: u32,
    pub h: u32,
}

struct FourthLayerApp {
    pub(crate) is_dirty:bool, pub(crate) dirty: DirtyFlags, // (light_config → scene, last_rect_size → viewport — Phase 1)
    pub(crate) render: RenderSettings,
    pub(crate) compositor: CompositorStack,
    pub(crate) screenshot_requested: bool,
    pub(crate) pending_screenshot: Option<PendingScreenshot>,
    /// AZRA_VISDIFF harness state (visdiff.rs): frame counter + pending shot path.
    pub(crate) visdiff_frame: u32,
    pub(crate) visdiff_shot: Option<std::path::PathBuf>,
    /// AZRA_NURBSDIFF harness (nurbsdiff.rs): NURBS tool-path visual gate. Shares
    /// `visdiff_shot` screenshot path with VISDIFF/SOAK.
    pub(crate) nurbsdiff_frame: u32,
    /// Set by sequential-fillet step (P0/P3). False until that step succeeds.
    pub(crate) nurbsdiff_seq_ok: bool,
    /// Set by cylinder rim fillet step (P1).
    pub(crate) nurbsdiff_rim_ok: bool,
    /// Set by top-loop batch fillet step (Plasticity-class).
    pub(crate) nurbsdiff_loop_ok: bool,
    /// AZRA_SOAK harness (soak.rs): self-driving production-API session + px verdicts.
    pub(crate) soak: soak::SoakHarness,
    pub(crate) prev_cam_uniforms: graphics::PrevCamera,
    pub(crate) viewport: ViewportState,
    pub(crate) tool: ToolState,
    pub(crate) ui: UiWindowState,
    pub(crate) commands: Vec<AppCommand>,
    pub(crate) command_log: Vec<AppCommand>,
    pub(crate) macro_recording: bool,
    pub(crate) macro_buffer: Vec<AppCommand>,
    pub(crate) brush:BrushSettings, // (work_mode moved to tool: ToolState — Phase 1)
    // SDF sculpt brush state
    pub(crate) sdf_sculpt_op: core_math::primitives::SculptOp,
    pub(crate) sdf_sculpt_radius: f32,
    pub(crate) sdf_sculpt_strength: f32,
    pub(crate) sdf_sculpt_active: bool,
    pub(crate) sdf_brush_preview_pos: Option<Vec3>,
    pub(crate) sdf_sculpt_falloff: core_math::sculpt_layer::BrushFalloff,
    #[allow(dead_code)] // FUTURE: sculpt brush stroke smoothing
    pub(crate) sdf_stroke_interpolator: core_math::sculpt_layer::StrokeInterpolator,
    pub(crate) sculpt_source_visible: Option<Vec<bool>>, // visibility before sculpt enter
    pub(crate) scene: scene_state::SceneState,
    // Cache state — GPU upload caches, mesh cache, volume caches, DC extraction jobs
    pub(crate) cache: cache_state::CacheState,
    pub(crate) sel: SelectionState,
    // (usel moved to sel: SelectionState — Phase 1)
    // Modeling context — derived from selected node type each frame
    pub(crate) modeling_context: ModelingContext,
    /// Scratch segment list for the depth-tested GPU line overlay (see overlay_lines.rs).
    /// Persistent so the per-frame rebuild reuses its allocation instead of allocating.
    pub(crate) overlay_lines: Vec<render::line_overlay::LineInstance>,
    /// Fingerprint of last `build_overlay_lines` inputs (skip rebuild on camera-only frames).
    pub(crate) overlay_lines_fp: u64,
    pub(crate) overlay_lines_depth_count: u32,
    /// POLY_VIEWPORT_PERF P6.2: last wire rebuild hit a hard segment/edge cap (honest UI).
    pub(crate) overlay_wire_capped: bool,
    /// Segment count after last wire rebuild (Perf HUD).
    pub(crate) overlay_wire_seg_count: u32,
    /// One-shot toast already shown for the current capped episode.
    pub(crate) overlay_wire_cap_toast_shown: bool,
    /// Selected-face triangle verts for the depth-tested GPU face fill (overlay_faces.rs).
    /// Blender Overlay parity — replaces the always-on-top egui face fill that bled at angles.
    pub(crate) overlay_faces: Vec<render::face_overlay::FaceVertex>,
    pub(crate) overlay_faces_fp: u64,
    /// Per–mesh-cache-id body-local edge cache (rebuild only when content_stamp changes).
    pub(crate) wire_local_cache: std::collections::HashMap<u64, overlay_lines::WireLocalEdgeCache>,
    /// Per–poly-body body-local edge cache (POLY_VIEWPORT_PERF P1b.4).
    /// Keyed by NodeId; stamp = display/base mesh identity. Transform-only changes re-emit
    /// without re-walking half-edges (mirrors SDF `wire_local_cache`).
    pub(crate) poly_wire_local_cache: std::collections::HashMap<
        core_math::scene_graph::NodeId,
        overlay_lines::WireLocalEdgeCache,
    >,
    /// Quick Normal ghost: high-res reference topology wire (dismisses on camera move).
    pub(crate) quick_normal_ghost: Option<QuickNormalGhost>,
    pub(crate) poly_work_mode: PolyWorkMode,
    pub(crate) nurbs_work_mode: NurbsWorkMode,
    pub(crate) poly_tool: PolyTool,
    pub(crate) loop_cut_count: u32,
    /// Loop Cut display toggle (tool settings): true = live WYSIWYG geometry while sliding, false =
    /// yellow preview until confirm. Default false (preview).
    pub(crate) loop_cut_live: bool,
    /// Loop Cut SELECTED: when ON and edges are selected, selection drives the cut (partial span
    /// limit + multi-ring: every independent ring in the selection is cut). When OFF, selection is
    /// ignored — free full ring from hover/seed, including through n-gons. Default ON so multi-
    /// selection keeps working; turn off for free control when leftover edges would force the cut.
    pub(crate) loop_cut_selected: bool,
    /// Loop Cut FOLLOW CURVATURE: place each inserted vertex on the curvature-following rail curve
    /// instead of the straight chord, so a loop cut across a rounded form sits ON the round rather
    /// than flattening it. Default OFF — matches Blender's `smoothness = 0` and leaves every
    /// existing cut bit-identical until the user asks for it.
    pub(crate) loop_cut_follow_curvature: bool,
    /// Strength of `loop_cut_follow_curvature`: 0 = straight chord, 1 = fully on the arc through the
    /// rail's endpoint normals. Above 1 exaggerates past the arc (allowed, occasionally wanted).
    pub(crate) loop_cut_curvature: f32,
    /// MESHmachine-style Loop Select (Alt+click edge loop / Alt+Ctrl ring) settings.
    /// See Tool Parameters → Loop Select while in Edge mode.
    ///
    /// When true, always use angle-based LSelect even if the selection is all sharp edges
    /// (MESHmachine "Always Loop Select"). When false, all-sharp selection uses connected
    /// sharp edges instead of a geometric loop.
    pub(crate) loop_sel_always_loop: bool,
    /// Maximum turn from straight (degrees) when growing a loop. Higher = longer loops
    /// (MESHmachine 0.8+). Default 41 matches the MeshMachine panel default in the owner shot.
    pub(crate) loop_sel_min_angle: f32,
    /// Cap the walk length with `loop_sel_steps` (MESHmachine "Use Edge Limit").
    pub(crate) loop_sel_use_edge_limit: bool,
    /// Max edges walked **per direction** from the seed when `loop_sel_use_edge_limit` is on.
    pub(crate) loop_sel_steps: u32,
    /// Inset mode (Blender `I` vs `I,I`): false = REGION / "group faces" (the Blender DEFAULT — a
    /// multi-face selection is ONE region, only its outer boundary is inset), true = INDIVIDUAL (each
    /// face gets its own ring). Toggled in the inset tool settings and by tapping I again.
    pub(crate) poly_inset_individual: bool,
    /// Circle (LoopTools) interior-relax passes — an AZRA extension over Blender, which never touches
    /// the region's interior. Persists the user's last choice from the Circle redo panel; 0 = Blender
    /// parity. See `poly/circle.rs`.
    pub(crate) poly_circle_relax: u32,
    pub(crate) knife_points: Vec<Vec3>,       // world-space cut points for knife tool
    pub(crate) knife_snap_pos: Option<Vec3>,  // current snap preview position
    /// What the knife cursor is snapped to: 0 = free on a face (will NOT land on an edge!),
    /// 1 = vertex, 2 = edge. Drives the distinct hover markers — a mid-face point must be
    /// visually unmistakable from an edge snap (owner: "I was sure my cut was fine").
    pub(crate) knife_snap_kind: u8,
    /// The snapped edge's world endpoints (kind==2), for highlighting the target edge itself.
    pub(crate) knife_snap_edge: Option<(Vec3, Vec3)>,
    /// Free-polyline knife personality (owner spec 2026-08-03, R1 "first click decides"):
    /// armed when the FIRST knife click lands on empty space. Points are then placed on a
    /// view-aligned plane captured at that click (`knife_free_plane` = origin,u,v,normal) and
    /// the cut extrudes through the body along the captured view.
    ///
    /// `knife_free_slice` = Enter/RMB finish mode only (false = SUBTRACT, true = SCRIBE).
    /// **Not** the S key: S is a one-shot "apply through-slice now" verb, not a toggle
    /// (owner 2026-08-06). The confirm gesture's cursor doubles as the open-line side pick.
    pub(crate) knife_free: bool,
    pub(crate) knife_free_slice: bool,
    /// Drawing plane: origin, u, v, plane_normal. For view-aligned free knife the normal
    /// equals the camera forward; for shape-on-face (R/C press) the normal is the face normal.
    pub(crate) knife_free_plane: Option<(Vec3, Vec3, Vec3, Vec3)>,
    /// Camera forward frozen when the free plane was captured. **Slice / through-cut always
    /// project along this** (Blender knife cut-through), never along a face normal — face
    /// plane only decides where the polyline is drawn (owner: face press → all S slices failed).
    pub(crate) knife_free_view: Option<Vec3>,
    /// Free-knife SHAPE polyline (owner 2026-08-05: "while in knife tool pressing r brings a
    /// rectangular polyline, c a circular"). 0 = off, 1 = rectangle, 2 = circle.
    ///
    /// While armed, LMB drag sizes the shape: press = anchor (rect corner / circle centre),
    /// cursor = opposite corner / rim. The shape is written straight into `knife_points` on
    /// every drag update, so the ordinary polyline overlay IS the live preview — no second
    /// draw path to keep in sync.
    pub(crate) knife_shape_kind: u8,
    /// World anchor on the knife plane, set on press. `None` = armed, waiting for the press.
    pub(crate) knife_shape_anchor: Option<Vec3>,
    /// Live cursor point on the knife plane while dragging the shape out.
    pub(crate) knife_shape_cursor: Option<Vec3>,
    /// Bevel / circle segment count — default **3**. Tool Parameters + wheel:
    ///   * Rect mid-drag: **Shift+scroll** = segments; plain scroll = corner bevel amount
    ///   * Circle mid-drag: scroll = segments
    /// Wheel is hard-gated: mid-drag only (`knife_shape_scroll_armed`).
    pub(crate) knife_shape_segments: u32,
    /// Accumulator for discrete segment-notch wheel (one physical tick → ±1 segment).
    pub(crate) knife_shape_scroll_accum: f32,
    /// Rectangle corner bevel amount while sizing (owner 2026-08-06). 0 = sharp; 1 = max
    /// fillet (half of min side). Scroll adjusts this only mid-drag; idle knife / R-armed
    /// with nothing drawn does not consume the wheel.
    pub(crate) knife_shape_corner_bevel: f32,
    /// Screen-space rubber-band corners for the armed knife shape (same UI as region select).
    /// `None` while armed-but-not-yet-pressed; set on press/drag, cleared on release/cancel.
    pub(crate) knife_shape_screen_a: Option<egui::Pos2>,
    pub(crate) knife_shape_screen_b: Option<egui::Pos2>,
    /// Alt held while sizing a knife shape: draw from **center** (press = center, drag = half-extent).
    /// Without Alt, rectangle is corner→corner (circle stays press=center / drag=radius).
    pub(crate) knife_shape_from_center: bool,
    /// In-plane rotation of the knife rectangle (radians, around shape center).
    /// Hold **R** mid-drag + mouse move to spin; closer to rect = faster (angular track).
    /// **Ctrl** while rotating snaps to `viewport.snap_rotation_deg`.
    pub(crate) knife_shape_rotation_rad: f32,
    /// Last mouse pos while R-rotate is active (for angular delta).
    pub(crate) knife_shape_rot_last_mouse: Option<egui::Pos2>,
    /// Last mouse pos while Space-move is active (translate the whole shape without resizing).
    pub(crate) knife_shape_move_last_mouse: Option<egui::Pos2>,
    /// Linear array count for knife **rectangle** shape (owner 2026-08-07).
    /// 1 = single rect. **Ctrl+D** mid-drag starts at 2 (one duplicate); **scroll**
    /// while count ≥ 2 steps the count (1..=32). Spacing = tile along the longer
    /// side of the rect (edge-to-edge).
    pub(crate) knife_shape_array_count: u32,
    // ── Cut DEPTH phase (owner 2026-08-09: "while dragging, E defines the thickness of the
    //    cut; user needs to rotate the camera; cut confirms on LMB release") ───────────────
    /// `E` pressed mid-drag → the rect/circle is LOCKED and the tool enters the depth phase.
    /// The LMB release that ends sizing no longer cuts; the camera is free (MMB orbit); the
    /// NEXT LMB drag pulls the depth and ITS release commits the cut.
    pub(crate) knife_shape_depth_mode: bool,
    /// Cut depth in world units along the drawing plane's INWARD normal (−n). 0 = through
    /// cut (the historical cookie-cutter), >0 = blind pocket that stops at this depth.
    /// SIGNED since 2026-08-11: <0 extrudes OUT of the surface instead (the height of a
    /// [`crate::poly_knife::KnifeBoolOp::Union`] boss). See `poly_knife_shape_depth_track`.
    pub(crate) knife_shape_depth: f32,
    /// An LMB press landed while the depth phase was live, so the NEXT release is a confirm.
    /// False right after `E`, which is what stops the sizing drag's own release from cutting.
    pub(crate) knife_shape_depth_committable: bool,
    /// MMB held during the depth phase: freeze depth accumulation while the camera orbits
    /// (mirrors `CreateState::depth_orbit_frozen`).
    pub(crate) knife_shape_depth_frozen: bool,
    /// After a successful cut, select what the cut PRODUCED — the boolean's new faces (Face
    /// mode), or the new rim edges when it made none (Edge mode).
    /// **ON by default** since 2026-08-11 (owner: "knife cut should also auto set selection,
    /// set the resulting geo from boolean"); the toggle is the opt-out.
    pub(crate) knife_select_after_cut: bool,
    /// The face selection the last cut's auto-select WROTE — compared by value in
    /// `knife_has_user_face_scope()` so the tool never reads its own output back as the user's
    /// "scope the cut to these faces" instruction. Empty when the last cut selected edges/nothing.
    pub(crate) knife_last_result_faces: std::collections::HashSet<u32>,
    /// Which boolean the next SHAPE cut runs — `U` = Union, `I` = Intersect, again = Subtract.
    /// STICKY for the session like `knife_free_slice` (a refused cut must not silently throw the
    /// user's choice away); shown in the depth HUD and Tool Parameters. Never serialized.
    pub(crate) knife_bool_op: crate::poly_knife::KnifeBoolOp,
    pub(crate) nurbs_tool: NurbsTool,
    pub(crate) poly_sel: PolySelection,
    /// DEBUG overlay: draw every mesh vertex as a dot + its index (+ coords) so vertex positions can be
    /// cross-referenced against a reference tool (Blender). Toggled with Shift+I in poly edit mode.
    pub(crate) poly_vertex_debug: bool,
    pub(crate) poly_proportional: bool,
    pub(crate) poly_prop_radius: f32,
    pub(crate) poly_prop_falloff: PropFalloff,
    /// User-placed proportional-edit origin (WORLD space), moved by dragging the diamond on
    /// the falloff ring. `None` = AUTO: follow the resolved Action Centre, which is the only
    /// behaviour that existed before the handle. Tool state, NOT scene data — same class as
    /// `poly_prop_radius`, so it takes no undo entry and is not serialized into the `.fl4`.
    pub(crate) poly_prop_origin: Option<Vec3>,
    /// Cursor is within grab range of the proportional origin diamond this frame. Set by the
    /// overlay draw (which already projects the handle) and read by input to claim the press,
    /// exactly like `tool.blend_k_hovered` — one hit-test, never two.
    pub(crate) poly_prop_handle_hovered: bool,
    /// Legacy single-slot dirty (confirm/cancel flush). Prefer `poly_display_dirty`.
    pub(crate) poly_mesh_dirty: Option<core_math::scene_graph::NodeId>,
    /// PLAN_FOR_GROK_VIEWPORT_PERF P1: mark-then-drain — ops request sync; one drain per frame
    /// coalesces N marks on the same body into one `poly_sync_mesh_cache`.
    pub(crate) poly_display_dirty: std::collections::HashSet<core_math::scene_graph::NodeId>,
    /// Syncs that actually ran this frame (Perf HUD / SLOWPOLL). Reset each update start.
    pub(crate) poly_sync_calls_this_frame: u32,
    pub(crate) poly_sync_calls_last_frame: u32,
    /// Marks requested this frame (before drain).
    pub(crate) poly_sync_marks_this_frame: u32,
    pub(crate) poly_sync_marks_last_frame: u32,
    /// CONSOLE_AUDIT A1: pick-cache rebuilds this frame (count + ms). Reset each update start.
    pub(crate) poly_pick_rebuilds_this_frame: u32,
    pub(crate) poly_pick_rebuild_ms_this_frame: f32,
    /// CONSOLE_AUDIT A1: `poly_update_extrude_drag` attribution (ms, sum if multi-call).
    pub(crate) extrude_perf_calls: u32,
    pub(crate) extrude_perf_delta_ms: f32,
    pub(crate) extrude_perf_sync_ms: f32,
    pub(crate) extrude_perf_total_ms: f32,
    /// CONSOLE_AUDIT A2: `diag_frame` of last extrude drag update (once-per-frame gate).
    pub(crate) extrude_update_frame: u64,
    pub(crate) poly_pick_cache: PolyPickCache,
    /// Display-mesh generation cache (bug #6 + POLY_VIEWPORT_PERF P1a/P1b):
    /// last `(base content signature, modifier stack hash)` **and** the resulting display
    /// `HalfEdgeMesh` per poly body. `poly_sync_mesh_cache` skips re-evaluation when the key
    /// matches and the GPU solid is drawable; selection outlines / overlay wires **must**
    /// consume this Arc instead of re-running `apply_poly_modifiers` every frame (C10).
    pub(crate) poly_display_cache: std::collections::HashMap<
        core_math::scene_graph::NodeId,
        crate::poly::mesh_sync::PolyDisplayMeshCache,
    >,
    /// Poly edit generation (B1): bumped at mutation choke points (`poly_request_display_sync`,
    /// `poly_sync_after_position_edit`, `poly_force_display_resync`, `poly_release_shared_display_arcs`,
    /// `poly_sync_all_meshes`). Invalidates memoized display keys, pick mesh keys, wire stamps,
    /// and LOD poll latches — one counter for all consumers. Do **not** key idle skips on Arc ptr
    /// alone (in-place `Arc::make_mut` keeps the pointer).
    pub(crate) poly_edit_generation: u64,
    /// Memo of `(csig, stack_key)` per body, valid while `gen == poly_edit_generation`.
    /// Avoids full `content_signature` O(mesh) on every pick/LOD poll frame when nothing edited.
    /// `RefCell` so `&self` paths (`poly_pick_mesh_key`) can fill the memo on first miss.
    pub(crate) poly_display_key_memo: std::cell::RefCell<
        std::collections::HashMap<core_math::scene_graph::NodeId, (u64, u64, u64)>,
    >,
    /// B2: last latched `(poly_edit_generation, camera-distance bucket)` for `poll_poly_lod_policy`.
    /// When both match, LOD poll is a no-op (~0 idle cost).
    pub(crate) poly_lod_latch_gen: u64,
    pub(crate) poly_lod_latch_cam_bucket: u64,
    /// P0 / P1a: count of `apply_poly_modifiers` evaluations this frame (orbit goal = 0).
    /// Reset at the start of each `update`; `poly_modifier_evals_last_frame` is what the HUD shows.
    pub(crate) poly_modifier_evals_this_frame: u32,
    pub(crate) poly_modifier_evals_last_frame: u32,
    /// Last `draw_node_selection_outlines` CPU cost (ms) for Perf HUD.
    pub(crate) poly_sel_outline_ms: f32,
    /// POLY_VIEWPORT_PERF P2a: per-body modifier **prefix** meshes (after each stack slot).
    /// Scrubbing Array after Subdivide reuses the subdiv mesh instead of re-Catmull-Clark.
    pub(crate) poly_prefix_cache: std::collections::HashMap<
        core_math::scene_graph::NodeId,
        crate::poly::mesh_sync::PolyPrefixBodyCache,
    >,
    /// Approximate host bytes held by all prefix mesh Arcs (eviction budget).
    pub(crate) poly_prefix_total_bytes: usize,
    /// Monotonic clock for LRU eviction of prefix bodies.
    pub(crate) poly_prefix_clock: u64,
    /// V1 incremental Remesh-modifier state: ONE (body, slot) retains its voxel grid so a
    /// live edit under an active Remesh resamples only the dirty region. Runtime-only;
    /// dropped (and reconciled if lattice-diverged) at `commit_episode`.
    pub(crate) poly_remesh_incr: Option<crate::poly::remesh::RemeshIncrCache>,
    /// PolySculpt clone-once working mesh (S0b). Runtime-only; keyed by body.
    pub(crate) poly_sculpt_working:
        std::collections::HashMap<core_math::scene_graph::NodeId, crate::poly::sculpt::PolySculptWorking>,
    /// Mesh Terrain runtime stream (Phase 2) — not serialized; rebuilt from scene names on load later.
    pub(crate) terrain_world: crate::terrain::TerrainWorld,
    /// Background terrain display remesh (D3 settle) — rayon jobs; poll applies results.
    pub(crate) terrain_remesh_jobs: Vec<crate::terrain::PendingTerrainRemeshJob>,
    /// Per-body remesh generation — results with a lower gen are discarded.
    pub(crate) terrain_remesh_gen: std::collections::HashMap<core_math::scene_graph::NodeId, u64>,
    /// Bodies whose displayed remesh currently sits on a LATCHED (episode-inflated) lattice
    /// and therefore differs from what a stateless eval would produce. Recorded per-eval
    /// rather than read off `poly_remesh_incr` at commit time, because that single slot is
    /// overwritten whenever a different body's stack is walked mid-episode (a display-dirty
    /// drain, a boolean cutter update…) — reading only the surviving slot would strand
    /// every other body on a coarser lattice. Drained + resynced by
    /// `commit_episode`/`cancel_episode`.
    pub(crate) poly_remesh_diverged: std::collections::HashSet<core_math::scene_graph::NodeId>,
    /// Mesh Cleanup live session: base mesh snapshot before preview edits
    /// (`None` = tool idle / no preview). Restore on Cancel / Live OFF / tool leave.
    pub(crate) mesh_cleanup_base:
        Option<(core_math::scene_graph::NodeId, std::sync::Arc<core_math::half_edge::HalfEdgeMesh>)>,
    /// PLAN_FOR_GROK_VIEWPORT_PERF P3: per-body poly display LOD state (medium/far meshes).
    pub(crate) poly_lod_cache:
        std::collections::HashMap<core_math::scene_graph::NodeId, crate::poly::lod::PolyLodBodyState>,
    /// P3/P4: content_key → shared coarse MeshCacheIds (one decimate per unique mesh).
    /// Content keys seen by the PREVIOUS LOD pass — a key must survive one pass before a build
    /// is spawned, so a mesh changing every frame never starts one.
    /// EWMA of the interactive poly display-sync cost — the FrameScheduler cost hint for the
    /// budgeted drag path, learned rather than guessed.
    pub(crate) poly_sync_cost_ms: f32,
    /// When the interactive solid last actually synced (staleness ceiling).
    pub(crate) poly_sync_last_at: std::time::Instant,
    pub(crate) poly_lod_prev_keys: std::collections::HashSet<(u64, u64)>,
    /// Content keys with a coarse-LOD build in flight (one job per key).
    pub(crate) poly_lod_jobs: std::collections::HashSet<(u64, u64)>,
    pub(crate) poly_lod_tx: std::sync::mpsc::Sender<crate::poly::lod::PolyLodBakeResult>,
    pub(crate) poly_lod_rx: std::sync::mpsc::Receiver<crate::poly::lod::PolyLodBakeResult>,
    pub(crate) poly_lod_shared:
        std::collections::HashMap<(u64, u64), crate::poly::lod::PolyLodSharedMeshes>,
    /// AZRA_MASTER_ROADMAP B.1: structural drawable NodeIds (rebuild on SCENE/MESH only).
    pub(crate) draw_stream: Vec<core_math::scene_graph::NodeId>,
    pub(crate) draw_stream_stale: bool,
    /// 1 if rebuilt this frame, else 0 (scoreboard / exit gate).
    pub(crate) draw_stream_rebuilds_last_frame: u32,
    /// Structural draw list (transform-derived): InstanceData + materials + kinds.
    /// Rebuilt on SCENE/MESH / selection-outline / LOD switch — **not** on camera motion.
    /// Per-frame work is cull mask only (frustum + occlusion) over this cached array.
    /// C.9 prerequisite: GPU cannot drive draws while CPU rebuilds the list every frame.
    pub(crate) structural_materials: Vec<render::mesh_gpu::RasterMaterial>,
    pub(crate) structural_draw_calls: Vec<(
        core_math::scene_graph::MeshCacheId,
        render::mesh_gpu::InstanceData,
        u8,
        Option<(u32, u32)>,
    )>,
    pub(crate) structural_draw_kind: Vec<u8>,
    pub(crate) structural_draw_kind_payload: Vec<u64>,
    /// World-space AABB per structural draw (same index as `structural_draw_calls`).
    /// Built on structural rebuild only — D1 prep: camera frames frustum-test these without
    /// re-transforming 8 corners × N (linear O(N) plane tests stay cheap at 100k).
    pub(crate) structural_world_aabb: Vec<(glam::Vec3, glam::Vec3)>,
    pub(crate) structural_draw_stale: bool,
    /// `scene.scene_graph.material_generation()` at last structural rebuild.
    /// Mismatch → force rebuild so RENDER-only material content edits stay visible.
    pub(crate) structural_material_gen: u64,
    /// 1 if structural list rebuilt this frame (scoreboard `structReb`).
    pub(crate) structural_draw_rebuilds_last_frame: u32,
    /// Hash of selection-outline NodeIds baked into InstanceData sel_bit; mismatch → rebuild.
    pub(crate) structural_outline_key: u64,
    /// Cached `any_sdf_rasterized` from last structural rebuild.
    pub(crate) structural_any_sdf: bool,
    /// D1/T1: frustum plane fingerprint from last packed upload. When unchanged and
    /// structural list is hot, skip InstanceData collect + GPU write (buffer still valid).
    pub(crate) raster_upload_plane_key: u64,
    /// Phase B probe mini-frame: square side in texels (0 = normal main-view frame).
    /// When >0, raster uses a top-left s×s viewport/scissor, skips HZB rebuild, and
    /// does not touch `raster_upload_plane_key` (B1/B2 of PBR_REFINE_HARDENING_PLAN).
    pub(crate) probe_frame_side: u32,
    /// D1 GPU cull: batch partition cached at last cull-input upload (index-aligned with
    /// the GPU batch_meta/draw_args buffers). Encode iterates this; camera frames never
    /// re-derive it. Valid iff `render.cull_batches_ready == len()`.
    pub(crate) cull_batches: Vec<crate::raster_common::MeshBatch>,
    /// D1/T1: poly ensure found nothing to recover — skip O(N) poly_bodies walk next frames.
    pub(crate) poly_meshes_drawable_ok: bool,
    /// Manual double-click detection for edit-mode loop/ring select (egui's double_clicked_by is
    /// unreliable in the viewport, esp. now that a box-select starts on every press). (time_sec, pos).
    pub(crate) poly_last_click_t: f64,
    pub(crate) poly_last_click_p: eframe::egui::Pos2,
    /// ACTIVE (last-selected) vertex — the Blender "merge at last" target. Set on each vertex click /
    /// shift-add; CLEARED by a box-select (no single last → merge defaults to centroid). None = no active.
    pub(crate) poly_active_vertex: Option<u32>,
    /// Selection history (Blender-style) for Vertex / Face / Edge modes — drives Shift+double
    /// face loop direction and Shift+scroll "Next/Prev Active". Most-recent selection is at the
    /// back. Cap ~32; cleared on mode switch / clear-all. Runtime-only.
    pub(crate) poly_face_history: Vec<u32>,
    pub(crate) poly_edge_history: Vec<u32>,
    /// Vertex-mode selection history for Shift+scroll Next/Prev Active (Blender oracle).
    pub(crate) poly_vert_history: Vec<u32>,
    /// Region-select shape (Alt+LMB drag) chosen from the viewport context menu. Default Box.
    pub(crate) poly_sel_shape: SelectionShape,
    /// LMB paint-select brush radius (px). Scroll-resizable while painting; persists between strokes.
    pub(crate) poly_paint_radius: f32,
    /// GPU select buffer (Blender model, PAINT_SELECT_AUDIT.md P1): whole-viewport face-ID image for
    /// occlusion-correct poly selection. Runtime-only; refreshed in poll_background_tasks.
    /// Depth-test verdict per light gizmo (`light.id` → hidden behind geometry), sampled from the
    /// raster depth buffer one texel per light and consumed one frame later
    /// (VIEWPORT_PERF_2026-08-12 item A — this replaced a full-scene CPU raycast per light per
    /// frame that cost 31 ms of a 33 ms frame on a 323k-tri body). A light with no entry draws:
    /// the probe fails OPEN, because a visible icon in the wrong place is recoverable and an
    /// invisible light is not.
    /// Geometry-fault count for the active poly body (viewport border light). Maintained in the
    /// poll phase by `poll_poly_geometry_issues`; the painter only reads it. The audit is three
    /// O(mesh) walks, so it must never run from a draw call.
    pub(crate) poly_geom_issues: Option<u32>,
    /// `(poly_edit_generation, node id)` the count above was computed for.
    pub(crate) poly_geom_issues_key: Option<(u64, u64)>,
    /// Generation seen by the PREVIOUS poll — settle latch, so the audit never lands mid-edit.
    pub(crate) poly_geom_issues_last_gen: Option<u64>,
    /// When the last audit ran — throttle clock, see `GEOM_ISSUE_MIN_INTERVAL`.
    pub(crate) poly_geom_issues_at: std::time::Instant,
    pub(crate) light_occluded: std::collections::HashMap<u32, bool>,
    /// `(light.id, its own linear view depth)` for the probe currently in flight, in copy order.
    pub(crate) light_probe_inflight: Vec<(u32, f32)>,
    pub(crate) poly_pick_image: Option<crate::poly_select_gpu::PolyPickImage>,
    pub(crate) poly_pick_pending: Option<crate::poly_select_gpu::PolyPickPending>,
    /// Camera-settle latch for the select-buffer refresh (render only once the camera stops moving).
    pub(crate) poly_pick_last_cam_key: u64,
    /// Mesh-settle latch: same idea as the camera latch, but for the mesh content. A live edit
    /// (bevel/extrude/inset drag) changes `mesh_key` every frame; without this, the whole face-ID
    /// pass + full readback re-runs each of those frames. You cannot paint/region-select mid-edit,
    /// so defer the refresh to the frame the mesh stops changing.
    pub(crate) poly_pick_last_mesh_key: u64,
    /// Merge menu (Alt+M): screen position to draw the option popup at, or None when closed.
    pub(crate) poly_merge_menu_pos: Option<eframe::egui::Pos2>,
    pub(crate) nurbs_sel: NurbsSelection,
    /// Polyline (CurveExtrude) profile-point editor selection: indices into the active prim's
    /// `profile.verts`. Active whenever exactly one CurveExtrude prim is selected (Object mode).
    /// NOT part of UnifiedSelection (those are scene objects); this is sub-element state, cleared
    /// when the owning prim deselects (`curve_pt_owner` mismatch).
    pub(crate) curve_pt_sel: std::collections::BTreeSet<usize>,
    pub(crate) curve_pt_hover: Option<usize>,
    /// prim_uid the current `curve_pt_sel` belongs to — selection clears when this changes.
    pub(crate) curve_pt_owner: Option<u32>,
    /// Tier-3 "Copy angle" pick mode: the selected edge `(i, j)` whose slope will be set to the
    /// next segment the user LMB-clicks. `None` = not armed. Esc / a successful pick clears it.
    pub(crate) curve_copy_angle_arm: Option<(usize, usize)>,
    /// SpinePipe 3D point editor (Phase A): indices into `SpinePipe.points`.
    pub(crate) spine_pt_sel: std::collections::BTreeSet<usize>,
    pub(crate) spine_pt_hover: Option<usize>,
    pub(crate) spine_pt_owner: Option<u32>,
    pub(crate) spine_pt_drag: Option<crate::spine_point_edit::SpinePtDrag>,
    pub(crate) bevel_pv: BevelPreviewState,
    /// Confirmed poly bevel stack (last entry = most recent). Exact Unbevel peels the top.
    /// Stacked bevels (edge then corner) each push; full-remove / cancel pops.
    pub(crate) poly_bevel_history: Vec<PolyBevelHistory>,
    // (viewport_mouse + viewport_rect moved to viewport: ViewportState — Phase 1)
    // (pie menus + context menu moved to ui: UiWindowState)
    pub(crate) history:UndoHistory, // (toasts moved to ui: UiWindowState — Phase 1)
    /// Pending episode transaction — holds pre-mutation snapshot during slider drags / gizmo drags.
    /// Pushed to undo stack on commit_episode(). See TRANSACTION_DESIGN.md.
    pub(crate) pending_episode: Option<UndoEntry>,
    /// Voxel-sculpt runtime cluster (undo/redo stacks, stroke recorders, the VoxelObject, pending atlas
    /// restores) — grouped into one substate (de-God-Object Phase 1). Access as `self.sculpt.<field>`.
    pub(crate) sculpt: SculptState,
    // Keyboard shortcut system
    pub(crate) keybindings: KeyBindings,
    // Outliner state (unified sub-struct)
    pub(crate) outliner: OutlinerState,
    // Layer/object systems removed (2026-03-25) — scene graph is sole hierarchy.
    pub(crate) ai: AiState,
    pub(crate) gpu: Option<GpuCtx>, pub(crate) gpu_info: GpuInfo,
    pub(crate) worker_gpus: Vec<WorkerGpu>,
    pub(crate) job_dispatcher: Option<gpu_jobs::GpuJobDispatcher>,
    /// Background GPU pipeline compilation — None once complete
    gpu_init: Option<GpuInitState>,
    /// When true, shader pipelines are rebuilt next frame (e.g. node graph changed)
    pub(crate) shader_dirty: bool,
    // Auto-managed texture server subprocess (spawned on launch, killed on exit)
    pub(crate) texture_server_proc: Option<std:: process::Child>,
    // (startup_toast moved to ui: UiWindowState — de-God-Object Phase 1)
    // (`force_analytical_debug` DELETED 2026-07-14 — the analytical SDF march display path was
    //  stripped; SDF renders from its baked DC mesh via the raster pass.)
    // (`force_volume_only` + `sdf_edit_active` DELETED 2026-07-13, legacy-volume cleanup slice:
    //  the SDF volume-raymarch display was removed in 459aac2 and `sdf_edit_active` was never
    //  wired — both flags were provably constant false, so every reader was simplified away.)
    /// LAB spike: poly-mesh-as-SDF-operand dev toggle + conversion state.
    /// NOT persisted (dev-tweaks precedent). See lab_mesh_sdf.rs.
    pub(crate) lab_mesh_sdf: LabMeshSdfState,
    /// Developer debug: draw wireframe boxes for dirty bake regions in viewport overlay.
    pub(crate) show_dirty_regions: bool,
    /// Developer debug: blend dispatch smoke test — green tint overlay on entire viewport.
    pub(crate) test_blend_overlay: bool,
    /// SSGI dev stage toggles (SSGI_PLAN.md §4.10). ONE struct, not eight bools —
    /// main.rs is under an active god-file split. Every default is the production
    /// value, so the shipping build is `SsgiDebug::default()`.
    pub(crate) ssgi_debug: app_types::SsgiDebug,
    pub(crate) log_render_mode: bool,
    // Boolean edge detection + click-to-bevel
    pub(crate) bool_edges: Vec<BoolEdgeCurve>,
    pub(crate) bool_edge_hovered: Option<usize>,
    pub(crate) bool_edge_dirty: bool,
    // Async GPU pick — dispatched on click, polled next frame (one-frame-late)
    pub(crate) pending_pick: Option<PendingPick>,
    /// Library material drop awaiting the async GPU pick result: the dropped library item index.
    /// Consumed by `handle_pending_pick` (import instead of select). See material_import.rs.
    pub(crate) pending_material_drop: Option<usize>,
    /// In-flight library-material import decode workers (one receiver per drop; decoded maps
    /// stream back as `(MaterialId, MapKind, MAP_RES² RGBA8, display name, normal-green-is-DX)`).
    /// Drained by `poll_material_imports`.
    pub(crate) material_import_rx: Vec<std::sync::mpsc::Receiver<(core_math::primitives::MaterialId, core_math::pbr_material::MapKind, Vec<u8>, String, bool)>>,
    /// Phase C (depth-raycast): in-flight surface-pick readback for the voxel sculpt brush position.
    pub(crate) pending_surface_pick: Option<PendingSurfacePick>,
    // (show_bool_edges moved to ui: UiWindowState)
    // (lights + next_light_id moved to scene: SceneState)
    // (edit_target + selected_faces moved to sel: SelectionState)
    // (raw_tab_events + tab_physically_held + any_text_focused moved to input_system: InputSystem — Phase 1)
    // (AI v3 + Claude + CLIP fields moved to ai: AiState)
    // (shader node graph moved to ui: UiWindowState)
    // HDR environment map (IBL pipeline — env_ibl.rs)
    pub(crate) env_hdr_data: Vec<f32>,    // level 0 RGB f32 triplets (width*height*3 floats)
    pub(crate) env_hdr_mip_data: Vec<f32>, // full buffer image: [7 mips][SH9 27 floats]
    pub(crate) env_hdr_width: u32,
    pub(crate) env_hdr_height: u32,
    /// Env worker → poll delivery (decode + placeholder mips + SH9 happen off-thread).
    pub(crate) env_load_tx: std::sync::mpsc::Sender<crate::env_ibl::EnvLoadResult>,
    pub(crate) env_load_rx: std::sync::mpsc::Receiver<crate::env_ibl::EnvLoadResult>,
    /// Bumped on New/Load project — stale worker results are dropped by generation.
    pub(crate) env_load_gen: u64,
    /// Set by the Clear button / project reset; the buffer swap happens in poll
    /// (Frame Ordering Contract — panels never touch GPU resources).
    pub(crate) env_clear_pending: bool,
    /// Mips still awaiting their GPU GGX prefilter dispatch (1..7; one per poll frame).
    pub(crate) env_prefilter_queue: Vec<u32>,
    /// The file the CURRENT environment was decoded from (set on install, cleared with the env).
    /// Project saves persist it (path + intensity + rotation) in the `FL4ENVIR` side-channel.
    pub(crate) env_source_path: Option<std::path::PathBuf>,
    /// True once the startup BRDF-LUT worker's buffer upload landed (env_lut_res gate).
    pub(crate) brdf_lut_uploaded: bool,
    /// Deferred file dialogs (rfd modal-in-frame crash-class fix — file_dialog.rs).
    pub(crate) file_dialogs: crate::file_dialog::FileDialogService,
    /// Brush-tip library thumbnail + active form preview textures (sculpt panel).
    pub(crate) tip_preview_cache: crate::brush_tip_lib::BrushTipPreviewCache,
    // MatCap texture
    pub(crate) matcap_data: Vec<u8>,      // RGBA8 pixels
    pub(crate) matcap_width: u32,
    pub(crate) matcap_height: u32,
    // Texture atlas (per-primitive textures)
    pub(crate) tex_atlas_data: Vec<u32>,       // packed RGBA pixels, all textures contiguous
    pub(crate) tex_atlas_meta: Vec<[u32; 4]>,  // (offset, width, height, 0) per texture
    pub(crate) tex_atlas_next_id: u32,         // next available texture ID (1-based)
    pub(crate) tex_atlas_dirty: bool,          // needs GPU re-upload
    // Background thumbnail loader
    pub(crate) thumb_bridge: crate::content_library::ThumbnailBridge,
    // Content Library
    pub(crate) library: LibraryState,
    /// In-flight library scan worker (library_scan.rs) — `Some` while a scan runs; the result
    /// (items + decoded first matcap) is installed in `poll_library_scan` (Frame Ordering).
    pub(crate) library_scan_rx: Option<std::sync::mpsc::Receiver<crate::library_scan::LibraryScanResult>>,
    // Developer tweak settings (runtime-adjustable magic numbers)
    pub(crate) tweak: ToolSettings,
    // UI layout configuration (panel positions, sizes, theme)
    pub(crate) layout: UiLayoutConfig,
    // (command_palette moved to ui: UiWindowState — Phase 1)
    // Workspace manager (switchable workspaces + customizable panels)
    pub(crate) workspace_mgr: WorkspaceManager,
    // (_ws_dragging + ws_add_popup + ws_add_search + ws_context_menu moved to ui: UiWindowState — Phase 1)
    // CUDA rendering backend (feature-gated)
    #[cfg(feature = "cuda")]
    pub(crate) cuda_renderer: Option<cuda_render::CudaRenderer>,
    // Project state — path, save state, auto-save, recent files
    pub(crate) project: project_state::ProjectState,
    // (show_unsaved_dialog + pending_action_after_save moved to ui: UiWindowState)
    // (mat_preview_cache moved to cache: CacheState — Phase 1)
    pub(crate) experimental: experimental::ExperimentalFlags,
    // Panel registry (drag/reorder/show-hide panels)
    pub(crate) panel_layout: panel::UiLayout,
    // Performance counters (dev overlay)
    pub(crate) perf: PerfCounters,
    pub(crate) scheduler: frame_scheduler::FrameScheduler,
    /// Diagnostic frame counter — incremented once per update() call.
    pub(crate) diag_frame: u64,
    /// Previous-frame diagnostic snapshot for delta-only prints.
    pub(crate) diag_prev: DiagState,
    /// Episode-based render path latching — locks pipeline + resolution at interaction start.
    pub(crate) interaction_episode: InteractionEpisode,
    // --- Prim serialization change-detection caches ---
    /// Previous selection set hash — detects selection changes that need prim buffer re-upload.
    pub(crate) prev_sel_hash: u64,
    /// Previous edit_target — detects edit-target changes that affect render overrides in prim buffer.
    pub(crate) prev_edit_target: Option<usize>,
    /// Previous show_sel_rim — detects toggling the selection outline, which changes serialized selection bits.
    pub(crate) prev_show_sel_rim: bool,
    // (isolated_body_nid moved to scene: SceneState)
    pub(crate) isolate_context_ghost: bool,
    // Prim clipboard (legacy — kept for backward compat, use clipboard instead)
    pub(crate) prim_clipboard: Vec<SdfPrimitive>,
    /// Universal clipboard — copy/paste any scene object (prims, bodies, lights, etc.)
    pub(crate) clipboard: clipboard::Clipboard,
    /// Context-aware input dispatch system (V1).
    pub(crate) input_system: input_system::InputSystem,
    pub(crate) ui2: ui2::Ui2State,
    // Body library thumbnail capture (set after async save completes)
    pub(crate) library_thumbnail_path: Option<std::path::PathBuf>,
    /// Optional node to focus isolation for the library thumb capture.
    pub(crate) library_thumbnail_focus_nid: Option<core_math::scene_graph::NodeId>,
    /// Background library save job (serialize+write off UI thread — never join on draw).
    pub(crate) pending_library_save: Option<PendingLibrarySave>,
    /// Queued kitbash save intent — build+serialize only starts in poll (never on the Save click).
    pub(crate) pending_library_save_request: Option<PendingLibrarySaveRequest>,
    // (post_load_bodies + post_load_warmup moved to cache: CacheState — Phase 1)
    /// Previous frame wall time (ms) — used to skip heavy GPU dispatches after long frames (TDR guard)
    pub(crate) prev_frame_wall_ms: f32,
    // (pending_file_open moved to project: ProjectState — Phase 1)
}

/// Alias so that modules using the new name compile against the old struct.
pub(crate) type AzraApp = FourthLayerApp;

/// Async kitbash save — result polled in `poll_background_tasks` (never blocks draw).
pub(crate) struct PendingLibrarySave {
    pub(crate) rx: std::sync::mpsc::Receiver<Result<(std::path::PathBuf, std::path::PathBuf), String>>,
    pub(crate) display_name: String,
    pub(crate) focus_nid: Option<core_math::scene_graph::NodeId>,
    /// Sanitize notes to show when save completes (MeshVolume→Box, etc.).
    pub(crate) notes: Vec<String>,
}

/// Save click only stores this — heavy clone/serialize never runs in the UI click handler.
pub(crate) struct PendingLibrarySaveRequest {
    pub(crate) nid: core_math::scene_graph::NodeId,
    pub(crate) name: String,
    pub(crate) dir: std::path::PathBuf,
}

// GPU init functions (pipeline_cache_path, load_pipeline_cache_data, save_pipeline_cache,
// ensure_fog_pipeline, ensure_pt_pipeline, spawn_texture_server) moved to gpu_init.rs

impl Drop for FourthLayerApp {
    fn drop(&mut self) {
        // Uncaught panic: do not call PipelineCache::get_data (driver) and do
        // not drop the device/queue. The hook exits first; this is the backstop.
        panic_safety::exit_if_panicking();
        // Save GPU pipeline cache to disk for faster next startup.
        if let Some(ref gpu) = self.gpu {
            if let Some(ref cache) = *gpu.pipelines.pipeline_cache {
                gpu_init::save_pipeline_cache(cache);
            }
        }
        // Intentionally do NOT kill the AI texture server on exit.
        // It is shared across Azra sessions (port 7860). Killing it caused
        // "connection refused" the next time the user opened AI Beauty.
        // Drop the handle without kill if present (legacy spawns).
        if let Some(child) = self.texture_server_proc.take() {
            std::mem::forget(child);
        }
    }
}

// FourthLayerApp::new() + the scene-graph/flat-cache bridge helpers (ensure_sdf_body,
// build_graph_from_flat, rebuild_prim_body_map, toast, derive_modeling_context) moved to app_new.rs (slice 30).

// active_worker_gpus + draw_multi_gpu_settings moved to multi_gpu.rs (multi_gpu extraction).

// eframe::App impl (raw_input_hook + update) and FourthLayerApp::init_gpu_ctx (slice 31)
// moved to app_update.rs (slice 32).


#[cfg(test)]
mod tests;
