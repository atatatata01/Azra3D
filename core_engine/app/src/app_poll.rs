//! Background task polling — agent, Claude, AI service, SVO, AI texture, mesh export, SDF compile.

use eframe::egui;
use glam::Vec3;
use crate::*;
use crate::claude::ClaudeKind;

/// PT warmup stage 1 payload (PT-COLD-WARMUP-BUNDLED): everything the PT DISPLAY needs —
/// the full progressive kernel plus denoise + upscale. Its arrival flips the viewport from
/// the raster draft to PT.
pub(crate) struct PtFullStage {
    pub(crate) tri: eframe::wgpu::ComputePipeline,
    pub(crate) denoise: eframe::wgpu::ComputePipeline,
    pub(crate) denoise_bgl: eframe::wgpu::BindGroupLayout,
    pub(crate) denoise_params: eframe::wgpu::Buffer,
    pub(crate) upscale: eframe::wgpu::ComputePipeline,
    pub(crate) upscale_bgl: eframe::wgpu::BindGroupLayout,
    pub(crate) upscale_params: eframe::wgpu::Buffer,
}

/// One delivery of the staged PT warmup. The single warmup thread sends them in the order
/// Full → Interactive → Native (`gpu_init::compile_pt_warmup_stages`); `poll_background_tasks`
/// adopts each one as it arrives instead of waiting for the slowest (native, ~162 s cold).
pub(crate) enum PtWarmupStage {
    Full(PtFullStage),
    /// The interactive (ReSTIR reuse) kernel — used by the half-res interaction path.
    Interactive(eframe::wgpu::ComputePipeline),
    /// Hardware ray-query pair; `None` = unsupported / rejected (software traversal stays).
    Native(Option<std::sync::Arc<app_gpu::native_rt::NativeRtPipelines>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PtStage {
    Full,
    Interactive,
    Native,
}

/// A warmup result that knows which stage it completes (lets the stage bookkeeping be tested
/// headless with a stand-in payload).
pub(crate) trait PtStaged {
    fn stage(&self) -> PtStage;
}

impl PtStaged for PtWarmupStage {
    fn stage(&self) -> PtStage {
        match self {
            Self::Full(_) => PtStage::Full,
            Self::Interactive(_) => PtStage::Interactive,
            Self::Native(_) => PtStage::Native,
        }
    }
}

/// Per-stage "still compiling" flags of the running PT warmup — one flag per stage, never a
/// single bundled flag (the bundle is what held PT on the draft for ~293 s).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PtPendingStages {
    pub(crate) full: bool,
    pub(crate) interactive: bool,
    pub(crate) native: bool,
}

impl PtPendingStages {
    const ALL: Self = Self { full: true, interactive: true, native: true };

    fn flag(&mut self, stage: PtStage) -> &mut bool {
        match stage {
            PtStage::Full => &mut self.full,
            PtStage::Interactive => &mut self.interactive,
            PtStage::Native => &mut self.native,
        }
    }

    pub(crate) fn any(self) -> bool {
        self.full || self.interactive || self.native
    }
}

pub(crate) struct PtPipelineWarmup<T = PtWarmupStage> {
    receiver: Option<std::sync::mpsc::Receiver<T>>,
    pending: PtPendingStages,
    failed: bool,
}

impl<T> Default for PtPipelineWarmup<T> {
    fn default() -> Self {
        Self { receiver: None, pending: PtPendingStages::default(), failed: false }
    }
}

impl<T> PtPipelineWarmup<T> {
    /// `full_ready` = stage 1 installed. A warmup is (re)requested only when the PT display
    /// kernel is missing — the later stages always ride the same worker.
    fn can_request(&self, full_ready: bool, heavy_compile_active: bool) -> bool {
        !heavy_compile_active && !full_ready && self.receiver.is_none() && !self.failed
    }

    /// Stages the running worker still owes (all false when no warmup is in flight).
    pub(crate) fn pending(&self) -> PtPendingStages {
        self.pending
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    fn begin(&mut self, receiver: std::sync::mpsc::Receiver<T>) {
        assert!(self.receiver.is_none(), "PT warmup already in flight");
        self.receiver = Some(receiver);
        self.pending = PtPendingStages::ALL;
    }
}

impl<T: PtStaged> PtPipelineWarmup<T> {
    /// Next delivered stage, if any. `Err(lost)` = the worker ended (panic / device loss)
    /// owing the `lost` stages; the warmup is then failed and never retried.
    fn take_ready(&mut self) -> Result<Option<T>, PtPendingStages> {
        let Some(receiver) = self.receiver.as_ref() else { return Ok(None); };
        match receiver.try_recv() {
            Ok(result) => {
                let stage = result.stage();
                let flag = self.pending.flag(stage);
                assert!(*flag, "PT warmup delivered stage {stage:?} twice");
                *flag = false;
                if !self.pending.any() {
                    self.receiver = None;
                }
                Ok(Some(result))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let lost = self.pending;
                self.receiver = None;
                self.pending = PtPendingStages::default();
                self.failed = true;
                Err(lost)
            }
        }
    }
}

struct PtCompileWake<T> {
    sender: Option<std::sync::mpsc::Sender<T>>,
    context: egui::Context,
}

impl<T> PtCompileWake<T> {
    /// Hand one stage to the main thread and wake it so `poll_background_tasks` adopts it on
    /// the next frame. A closed receiver (GPU context dropped) discards the stage.
    fn deliver(&self, result: T) {
        let _ = self.sender.as_ref().expect("PT compile sender missing").send(result);
        self.context.request_repaint();
    }
}

impl<T> Drop for PtCompileWake<T> {
    fn drop(&mut self) {
        self.sender.take();
        self.context.request_repaint();
    }
}

impl PtPipelineWarmup {
    fn request(
        &mut self,
        device: &std::sync::Arc<eframe::wgpu::Device>,
        cache: &std::sync::Arc<Option<eframe::wgpu::PipelineCache>>,
        context: &egui::Context,
    ) -> Result<(), String> {
        let device = device.clone();
        let cache = cache.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = PtCompileWake { sender: Some(sender), context: context.clone() };
        let spawn = std::thread::Builder::new().name("azra-pt-warmup".into()).spawn(move || {
            crate::gpu_init::compile_pt_warmup_stages(&device, cache.as_ref().as_ref(), |stage| wake.deliver(stage));
            drop(wake);
            if let Some(cache) = cache.as_ref() {
                crate::gpu_init::save_pipeline_cache(cache);
            }
        });
        match spawn {
            Ok(_) => {
                self.begin(receiver);
                Ok(())
            }
            Err(error) => {
                self.failed = true;
                Err(format!("Cannot start PT pipeline compilation: {error}"))
            }
        }
    }
}

/// Latched PT kernel/backend choice, shared by the traversal backend (software BVH vs hardware
/// ray query) and the interaction kernel (interactive vs full-at-interaction-budget).
/// Outside an interaction episode/cooldown the choice follows availability. Inside one it is
/// held ("zero path switches during interaction") — a stage that finishes compiling mid-drag is
/// adopted only once the latch releases — except the forced fallback when the held choice
/// stopped being available (it can no longer be dispatched).
#[must_use]
pub(crate) fn latched_pt_choice(available: bool, current: bool, latched: bool) -> bool {
    if latched { current && available } else { available }
}

/// What the viewport tells the user about the PT warmup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PtWarmupNotice {
    /// Nothing pending.
    Silent,
    /// The PT display kernel is not installed yet: the viewport shows the raster draft.
    Preparing,
    /// The PT display kernel never arrived (worker failed, error toasted): the draft stays.
    Unavailable,
    /// PT is live; later stages are still compiling (subtle note).
    Refining(&'static str),
}

#[must_use]
pub(crate) fn pt_warmup_notice(full_ready: bool, pending: PtPendingStages, failed: bool) -> PtWarmupNotice {
    if !full_ready {
        return if failed && !pending.any() { PtWarmupNotice::Unavailable } else { PtWarmupNotice::Preparing };
    }
    match (pending.interactive, pending.native) {
        (true, true) => PtWarmupNotice::Refining("Path tracer: compiling interactive + hardware RT kernels — navigation uses the full kernel"),
        (true, false) => PtWarmupNotice::Refining("Path tracer: compiling interactive kernel — navigation uses the full kernel"),
        (false, true) => PtWarmupNotice::Refining("Path tracer: compiling hardware ray tracing — software traversal active"),
        (false, false) => PtWarmupNotice::Silent,
    }
}

/// Install one PT warmup stage. Runs in `poll_background_tasks` only (Frame Ordering Contract);
/// the bind groups dropped here are rebuilt lazily in `viewport_render`.
fn install_pt_warmup_stage(gpu: &mut crate::GpuCtx, stage: PtWarmupStage) {
    match stage {
        PtWarmupStage::Full(full) => {
            gpu.pipelines.pt_tri_pipeline = Some(full.tri);
            gpu.pipelines.pt_denoise_pipeline = Some(full.denoise);
            gpu.bind_groups.pt_denoise_bgl = Some(full.denoise_bgl);
            gpu.buffers.pt_denoise_params_buffer = Some(full.denoise_params);
            gpu.pipelines.pt_upscale_pipeline = Some(full.upscale);
            gpu.bind_groups.pt_upscale_bgl = Some(full.upscale_bgl);
            gpu.bind_groups.pt_upscale_params_buffer = Some(full.upscale_params);
            gpu.bind_groups.pt_bg = None;
            gpu.bind_groups.pt_bg_half = None;
            gpu.bind_groups.pt_upscale_bg = None;
            crate::gpu_init::invalidate_pt_denoise_bgs(gpu);
            eprintln!("[PtWarm] stage 1/3 full kernel + denoise/upscale installed — PT display live");
        }
        PtWarmupStage::Interactive(pipeline) => {
            gpu.pipelines.pt_tri_interactive_pipeline = Some(pipeline);
            gpu.bind_groups.pt_bg_half = None;
            eprintln!("[PtWarm] stage 2/3 interactive kernel installed (adopted once no interaction is latched)");
        }
        PtWarmupStage::Native(native) => {
            eprintln!("[PtWarm] stage 3/3 hardware ray tracing {}", if native.is_some() { "installed" } else { "unavailable — software traversal stays" });
            gpu.pipelines.native_rt_pipelines = native;
            gpu.bind_groups.pt_bg = None;
            gpu.bind_groups.pt_bg_half = None;
        }
    }
}

/// Effective cascaded-shadow-map resolution for this frame (idle refine ladder,
/// PROGRESSIVE_PBR_PLAN.md Phase 2). `base` = the user's "Shadow Map Resolution"
/// (the render panel legitimately offers up to 8192² "ultra"); `boost` = idle
/// ladder step (0..=2); `cap_setting` = "Idle shadow cap" (UI-limited ≤4096, but
/// stale persisted settings may still hold 8192 from before E2 dropped that tier).
///
/// The bounds are composed `min` → `max` so they CANNOT invert. The previous
/// inline form, `cap_setting.clamp(base, 4096)`, assumed `base <= 4096` — an
/// assumption the 8192² base option (added 2026-08-06) silently broke: picking
/// it made `Ord::clamp` panic (`assertion failed: min <= max`, cmp.rs) every
/// frame in poll_background_tasks (TERRAINCLAMPCRASH — the panic log
/// mis-symbolized the frame as ensure_terrain_placeholder_display).
/// For base ≤ 4096 this is the identical median-of-three; for base > 4096 the
/// E2 ceiling caps the LADDER, never the user's base: cap == base, the ladder
/// no-ops, and the base resolution is honored.
#[inline]
pub(crate) fn effective_shadow_res(base: u32, boost: u32, cap_setting: u32) -> u32 {
    let boost = boost.min(2);
    // E2: ladder ceiling hard-capped at 4096 (8192 tier dropped) — but never
    // below `base`; mirrored for display in render_settings.rs ("→ actual").
    let cap = cap_setting.min(4096).max(base);
    if boost == 0 { base } else { (base << boost).min(cap) }
}

impl AzraApp {
    pub(crate) fn pt_pipeline_ready(&self) -> bool {
        self.gpu.as_ref().is_some_and(|gpu| {
            crate::gpu_init::ensure_pt_tri_pipeline(gpu)
                && (!self.render.pt_denoise || crate::gpu_init::ensure_pt_denoise_pipeline(gpu))
                && crate::gpu_init::ensure_pt_upscale_pipeline(gpu)
        })
    }

    fn poll_pt_pipeline_readiness(&mut self, context: &egui::Context) {
        const MAX_PT_PIPELINE_RESULTS_PER_FRAME: usize = 1;
        let ready = self.pt_pipeline_ready();
        let requested = self.render.shading_mode == ShadingMode::PathTrace;
        let Some(gpu) = self.gpu.as_ref() else { return; };
        let heavy_compile_active = gpu.pipelines.compiling_heavy_shader.load(std::sync::atomic::Ordering::SeqCst);
        if gpu.pipelines.pt_warmup.receiver.is_none()
            && !(requested && gpu.pipelines.pt_warmup.can_request(ready, heavy_compile_active))
        {
            return;
        }
        let ticket = crate::frame_scheduler::WorkTicket {
            class: crate::frame_scheduler::WorkClass::High,
            cost_hint_ms: 0.2,
            name: "pt_pipeline_readiness",
        };
        if !self.scheduler.can_run(&ticket) {
            self.scheduler.record_skipped(&ticket);
            context.request_repaint();
            return;
        }
        let started = std::time::Instant::now();
        let mut installed = false;
        let mut error = None;
        if let Some(gpu) = self.gpu.as_mut() {
            let heavy_compile_active = gpu.pipelines.compiling_heavy_shader.load(std::sync::atomic::Ordering::SeqCst);
            if requested && gpu.pipelines.pt_warmup.can_request(ready, heavy_compile_active) {
                // A (re)warm means the installed PT kernels are stale or absent. The staged
                // warmup re-delivers every stage; retire the later-stage kernels now so the new
                // full kernel is never paired with an interactive/native kernel compiled from
                // the previous source. Their consumers fall back structurally (full kernel at
                // the interaction budget; software traversal) until the new stages land.
                gpu.pipelines.pt_tri_interactive_pipeline = None;
                gpu.pipelines.native_rt_pipelines = None;
                gpu.bind_groups.pt_bg = None;
                gpu.bind_groups.pt_bg_half = None;
                error = gpu.pipelines.pt_warmup.request(&gpu.device, &gpu.pipelines.pipeline_cache, context).err();
            }
            for _ in 0..MAX_PT_PIPELINE_RESULTS_PER_FRAME {
                match gpu.pipelines.pt_warmup.take_ready() {
                    Ok(Some(stage)) => {
                        install_pt_warmup_stage(gpu, stage);
                        installed = true;
                    }
                    Ok(None) => break,
                    Err(lost) => {
                        error = Some(if lost.full {
                            "PT pipeline compilation ended without a result; draft rendering remains active".into()
                        } else {
                            format!(
                                "PT warmup ended early (interactive kernel lost: {}, hardware RT lost: {}); path tracing stays on the full kernel / software traversal",
                                lost.interactive, lost.native,
                            )
                        });
                        break;
                    }
                }
            }
            if let Some(message) = &error {
                gpu.pipelines.shader_compile_error = Some(message.clone());
            }
        }
        self.scheduler.record(&ticket, started.elapsed().as_secs_f32() * 1000.0);
        if installed || error.is_some() {
            self.mark_dirty(DirtyFlags::RENDER);
            context.request_repaint();
        }
        if let Some(message) = error {
            self.toast(&message);
        }
    }

    /// Poll all background tasks (non-blocking). Called once per frame from update().
    pub(crate) fn poll_background_tasks(&mut self, ctx: &egui::Context) {
        let _spike_t0 = std::time::Instant::now();

        // Detail Weighted LO: Alt hold-compare + settle-only draft (A+B PoC).
        // Timed separately — was the live `rest(top)=2s` hole in SLOWPOLL (untimed poll head).
        let _t_dlo = std::time::Instant::now();
        self.update_detail_lo_runtime_flags(ctx);
        // After slider scrub: full quality fold (Field + edge-shape + relax).
        self.poll_detail_lo_quality_settle();
        // Quick Normal ghost wire: dismiss when camera moves.
        self.poll_quick_normal_dismiss();
        // Bake Mesh Maps: GPU upload, so it MUST land here and not in the menu callback that
        // queued it (Frame Ordering Contract). One body per frame.
        self.poll_mesh_map_bakes();
        // Surface anything the bake WORKERS reported (they cannot toast). See `bake_diag`.
        self.poll_bake_notes();
        let _dlo_ms = _t_dlo.elapsed().as_secs_f64() * 1000.0;

        // PLAN_FOR_GROK_VIEWPORT_PERF P2: name every sub-step that used to hide inside
        // coarse `ptao` / `mats` buckets (SLOWPOLL rest(top) and 31 ms × N plateaus).
        // DIAGNOSTIC auto-repro driver (env AZRA_AUTOREPRO=1, no-op otherwise).
        let _t = std::time::Instant::now();
        self.harness_open_tick();
        self.harness_mode_tick();
        self.harness_edit_tick();
        self.harness_polysculpt_tick();
        self.harness_sculpt_shots_tick();
        // PR-4b: sculpt crop-pick resources — created HERE (poll), never mid-input
        // (Frame Ordering Contract). One-time; tiny (two 8×8 textures + a 256×8 staging).
        if let Some(gpu) = &mut self.gpu {
            if gpu.sculpt_pick.is_none() {
                gpu.sculpt_pick =
                    Some(crate::poly::sculpt_pick_gpu::SculptPickGpu::create(&gpu.device));
            }
        }
        self.harness_bool_tick();
        self.autorepro_tick();
        // Harness orbit driver (AZRA_ORBIT=<deg/frame>) — moving-camera scoreboard runs.
        self.harness_orbit_tick();
        let _autorepro_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // LAB spike: mesh→SDF operand toggle — conversion drain + grid buffer upload
        let _t = std::time::Instant::now();
        self.poll_lab_mesh_sdf();
        let _lab_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // GPU poly select buffer (per-element ID readback / settle refresh). A pointer press in
        // a poly edit context refreshes a stale buffer SYNCHRONOUSLY (Blender renders the select
        // buffer inside the pick operator) — one blocking render per click, so the same click
        // never gets a different answer depending on how recently the camera settled.
        let _t = std::time::Instant::now();
        let pick_sync = self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && ctx.input(|i| i.pointer.primary_pressed() || i.pointer.secondary_pressed());
        self.poll_poly_select_buffer(pick_sync);
        let _polypick_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // Light-gizmo occlusion: one depth texel per light off the raster depth buffer, consumed
        // a frame later. Replaced a full-scene CPU raycast per light per frame (31 ms of a 33 ms
        // frame with ONE light — VIEWPORT_PERF_2026-08-12 item A).
        self.poll_light_depth_probe();

        // Geometry-fault audit for the viewport border light — three O(mesh) walks, throttled and
        // episode-gated. It used to run from the PAINT path (37 ms of a 156 ms frame at 323k
        // faces, owner 2026-08-12 `[MidOv] frame 38.8`).
        self.poll_poly_geometry_issues();

        // Install finished background coarse-LOD meshes (the decimation itself is off-thread —
        // it measured 628 ms per pass on the frame thread, fast-release, 323k tris).
        self.poll_poly_lod_results();

        // Phase D1: instance-cull GPU resources (create/grow — poll phase only).
        let _t = std::time::Instant::now();
        self.poll_instance_cull();
        // Phase D2: HZB pyramid create/resize (poll phase only).
        self.poll_hzb();
        // Phase D3: cluster-DAG bake trigger + result drain (≤1/frame).
        self.poll_cluster_bake();
        // Demo-harness: share uploaded texture slots across each spawned family.
        crate::stress_test::poll_demo_tex_propagate(self);
        let _instcull_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // Triangle-PT BLAS / SSBO (PathTrace or RT shadows).
        let _t = std::time::Instant::now();
        self.update_pt_accel();
        let _ptaccel_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // Baked mesh-AO drain + rebake requests.
        let _t = std::time::Instant::now();
        self.poll_ao_bake();
        let _ao_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // RT shadow pipeline warm — one-time compile whenever ANY consumer wants rays:
        // the manual toggle OR the progressive-refine auto sun (rt_shadows_active() is
        // the single predicate for both). Gating this on `render.rt_shadows` alone
        // deadlocked the refine (owner "still renders in MatCap" 2026-08-07): refine
        // eligibility waits on this pipeline, while the render-path ensure_ fallback
        // waits on refine eligibility — so with the toggle off, auto sun never started
        // and the not-ready repaint pump span forever.
        let _t = std::time::Instant::now();
        if self.rt_shadows_active() {
            if let Some(gpu) = self.gpu.as_mut() {
                crate::gpu_init::ensure_rt_shadow_pipeline(gpu);
            }
        }
        let _rtshadow_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // Coarse ptao sum (compat with existing SLOWPOLL column).
        let _ptao_ms =
            _autorepro_ms + _lab_ms + _polypick_ms + _ptaccel_ms + _ao_ms + _rtshadow_ms;

        // Material array reset (scene-load bleed fix).
        let _t = std::time::Instant::now();
        if self.cache.material_array_reset_pending {
            self.cache.material_array_reset_pending = false;
            let restore = self.cache.material_maps_restore.take();
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.material_textures.reset(&gpu.device, &gpu.queue);
                if let Some(pixels) = &restore {
                    for px in pixels {
                        gpu.material_textures.upload_map(&gpu.device, &gpu.queue, px, "");
                    }
                }
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        let _matreset_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_file_dialogs();
        let _dialogs_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_env_ibl();
        let _env_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_library_scan(ctx);
        let _libscan_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_material_imports(ctx);
        let _libimp_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_poly_curvature_cache();
        self.poll_graph_curvature();
        let _gcurv_ms = _t.elapsed().as_secs_f64() * 1000.0;

        let _t = std::time::Instant::now();
        self.poll_graph_bakes();
        let _gbake_ms = _t.elapsed().as_secs_f64() * 1000.0;

        // Keep animating while a bake is in flight / more live previews pending.
        if !self.cache.graph_bake_pending.is_empty()
            || self.cache.graph_live_dirty
            || self.cache.graph_live_in_flight
            || !self.cache.graph_curv_pending.is_empty()
            || !self.cache.graph_curv_inflight.is_empty()
        {
            ctx.request_repaint();
        }
        // Material map uploads: budget 1/frame normally; while live-previewing upload more
        // so scrubbed albedo lands the same frame (maps never move geometry).
        // G7: count uploads this poll — if plateaus land here, N×~31 ms GPU texture writes.
        let mut _g7_map_uploads: u32 = 0;
        let _t_mapup = std::time::Instant::now();
        if !self.cache.material_map_loads.is_empty() {
            let max_uploads: usize =
                if self.cache.graph_live_in_flight || self.cache.graph_live_dirty {
                    4
                } else {
                    1
                };
            let mut rest = std::mem::take(&mut self.cache.material_map_loads);
            let batch: Vec<_> = rest.drain(..rest.len().min(max_uploads)).collect();
            // Put remaining back for next frames
            self.cache.material_map_loads = rest;
            let mut any = false;
            for load in batch {
                _g7_map_uploads += 1;
                // Panel Bit16 → dual-plane upload; Bit8 → single-plane hi only.
                let mut high_precision = load.high_precision && !load.pixels_lo.is_empty();
                if let crate::material_textures::MapTarget::Graph(mid) = load.target {
                    if let Some(doc) = self.ui.material_panel_docs.get(&mid.0) {
                        let ckind =
                            crate::material_panel::model::ChannelKind::from_map_kind(load.kind);
                        if let Some(ch) = doc.channel(ckind) {
                            high_precision = matches!(
                                ch.bit_depth,
                                crate::material_panel::model::ChannelBitDepth::Bit16
                            ) && !load.pixels_lo.is_empty();
                        }
                    }
                }
                let slot = match self.gpu.as_mut() {
                    Some(gpu) => {
                        if high_precision {
                            gpu.material_textures.upload_map_16(
                                &gpu.device,
                                &gpu.queue,
                                &load.pixels,
                                &load.pixels_lo,
                                &load.name,
                            )
                        } else {
                            // Bit8: re-encode dual-plane 16-bit bake → classic 8-bit.
                            // Hi-plane alone is NOT a valid 8-bit sRGB/data map (it stores
                            // lut[srgb_u16>>8]); sampling it as Bit8 washed flat Fill colors
                            // vs correct Bit16 reconstruct (owner: 8 vs 16 on solid red).
                            // O3: dual-plane pairs are any legal square (same len), not only MAP_RES².
                            let dual_ok = crate::material_textures::map_side_len(&load.pixels)
                                .is_some()
                                && load.pixels_lo.len() == load.pixels.len();
                            let pixels8 = if dual_ok {
                                use core_math::pbr_material::MapKind;
                                if matches!(
                                    load.kind,
                                    MapKind::Albedo | MapKind::Emission | MapKind::SssColor
                                ) {
                                    crate::material_textures::dual_plane_color16_to_srgb8(
                                        &load.pixels,
                                        &load.pixels_lo,
                                    )
                                } else {
                                    crate::material_textures::dual_plane_data16_to_srgb8(
                                        &load.pixels,
                                        &load.pixels_lo,
                                    )
                                }
                            } else {
                                // Legacy single-plane bake — upload as-is.
                                load.pixels.clone()
                            };
                            gpu.material_textures.upload_map(
                                &gpu.device,
                                &gpu.queue,
                                &pixels8,
                                &load.name,
                            )
                        }
                    }
                    None => continue,
                };
                // slot 0 = rejected upload (wrong size / etc.) — do not write tex_id.
                if slot == 0 {
                    eprintln!("[MaterialMap] skipped apply for '{}' (upload rejected)", load.name);
                    continue;
                }
                let applied = self.apply_map_slot_ex(
                    load.target,
                    load.kind,
                    slot,
                    load.strength,
                    high_precision,
                    load.green_dx,
                );
                // DIAGNOSTIC (`AZRA_MAP_APPLY=1`) — owner 2026-07-29 ("noise in bump does not
                // result in a bump map").
                //
                // The bake and the install gate are both PROVEN good for bump (b548536 + the
                // full-resolution gate): a non-flat map is produced at exactly MAP_RES²×4. So the
                // loss is at or after this apply. `apply_map_slot` returning false is silent —
                // the map uploaded fine, got a real slot, and then simply never reached the
                // material, which the shader reads as `bmap_slot == 0` and skips bump entirely.
                //
                // Logs EVERY apply, not just failures: a bump apply that never appears at all is
                // as informative as one that reports false (it means the load never got drained,
                // e.g. starved by the 1-upload-per-idle-frame budget above).
                if std::env::var("AZRA_MAP_APPLY").as_deref() == Ok("1") {
                    eprintln!(
                        "[MapApply] kind={:?} slot={slot} applied={applied} name='{}'",
                        load.kind, load.name);
                }
                if applied {
                    any = true;
                }
            }
            if any {
                // Materials never move geometry — no rebake_no_undo / SCENE (that was freezing).
                self.cache.prim_buf_stale = true;
                // Structural rebuild: `material_by_id_mut` (apply_map_slot) already bumped
                // material_generation; raster compares that to structural_material_gen.
                self.mark_dirty(DirtyFlags::RENDER);
                self.outliner.outliner_dirty = true;
                // Map PIXELS changed. With name-keyed slot reuse the tex_id/material bytes are
                // UNCHANGED (that's the point — zero bind-group churn), so neither the PT scene
                // hash nor pt_lighting_sig can notice — restart accumulation explicitly or the
                // path tracer keeps showing the pre-upload map (owner live-feel 2026-07-23).
                // M1.c: while scrubbing (active episode), raster shows Live maps; restart PT
                // once per release/settle so every tick does not wipe accumulation.
                if !self.has_active_episode() {
                    self.render.pt_accumulated_frames = 0;
                    self.render.pt_converged = false;
                }
            }
            if !self.cache.material_map_loads.is_empty() {
                ctx.request_repaint(); // drain remaining uploads next frames
            }
        }
        let _mapup_ms = _t_mapup.elapsed().as_secs_f64() * 1000.0;
        // Coarse mats sum (compat with SLOWPOLL column).
        let _mats_ms = _matreset_ms
            + _dialogs_ms
            + _env_ms
            + _libscan_ms
            + _libimp_ms
            + _gcurv_ms
            + _gbake_ms
            + _mapup_ms;

        let _t_warm = std::time::Instant::now();
        // Shadow map resolution ("Shadow Map Resolution", render panel): the cascade depth
        // textures are recreated HERE — never in the panel callback (it only writes
        // RenderSettings.shadow_map_resolution) and never in the render phase (Frame
        // Ordering Contract). set_resolution no-ops when unchanged; on change it bumps
        // ShadowMapSystem.generation, and raster_gbuffer's generation check rebuilds the
        // shadow bind group before the next dispatch samples the new maps.
        // Reflection-probe resolution ("Reflection Probe Res"): dropping the cube and
        // nulling ssr_bg makes viewport_render's :881 gate re-enter the texture-creation
        // block, which rebuilds BOTH at the new size. Done here, never in a panel
        // callback (Frame Ordering Contract). The in-flight bake round is restarted
        // because its already-captured faces belong to the old cube.
        if let Some(gpu) = self.gpu.as_mut() {
            let want = if crate::probe_bake::PROBE_FACE_SIZES.contains(&self.render.probe_face_size) {
                self.render.probe_face_size
            } else {
                crate::probe_bake::PROBE_FACE_SIZE_DEFAULT
            };
            if gpu.probe_cube.is_some() && gpu.probe_face_size != want {
                gpu.probe_cube = None;
                gpu.probe_face_size = 0;
                gpu.bind_groups.ssr_bg = None;
                gpu.probe_baked = false;
                gpu.probe_baked_sig = 0;
                gpu.probe_next_face = 6;
                // A verdict in flight judges the dropped cube — discard it.
                gpu.probe_round_gen = gpu.probe_round_gen.wrapping_add(1);
                // B3: drop cached face/mip views + bind groups with the cube.
                gpu.probe_view_cache_gen = 0;
                gpu.probe_capture_hdr_size = (0, 0);
                gpu.probe_face_mip0 = None;
                gpu.probe_capture_bgs = None;
                gpu.probe_mip_src_views.clear();
                gpu.probe_mip_dst_views.clear();
                gpu.probe_mip_bgs.clear();
                gpu.probe_sh_view = None;
                gpu.probe_sh_bg = None;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        let mut shadow_res_applied = false;
        if let Some(gpu) = self.gpu.as_mut() {
            // Progressive-refine idle ladder (PROGRESSIVE_PBR_PLAN.md Phase 2): while
            // the accumulator is refining, the effective resolution steps up from the
            // user's base (2×/4× at milestones, capped by "Idle shadow cap"). The base
            // setting is never mutated; interaction resets the boost → this restores it.
            // A1: boost is held at 0 while Leak-Proof RT sun owns the frame (viewport_render
            // skips the climb), so this path just re-asserts base resolution.
            // Inversion-proof bounds live in `effective_shadow_res` (top of file):
            // an 8192² base with the 4096 E2 ceiling used to invert a `clamp` here
            // and panic every frame (TERRAINCLAMPCRASH).
            let base = self.render.shadow_map_resolution;
            let effective = effective_shadow_res(
                base,
                self.render.pbr_refine_shadow_boost,
                self.render.pbr_refine_shadow_res_cap,
            );
            if gpu.shadows.resolution != effective {
                gpu.shadows.set_resolution(&gpu.device, effective);
                shadow_res_applied = true;
            }
        }
        if shadow_res_applied {
            self.mark_dirty(DirtyFlags::RENDER); // re-render with the recreated maps
        }

        // CR-70: Staggered post-load bake — mark 2 bodies dirty per frame
        // instead of bulk-marking all bodies in one frame (TDR prevention).
        if self.cache.post_load_warmup > 0 {
            self.cache.post_load_warmup -= 1;
            const BODIES_PER_FRAME: usize = 2;
            for _ in 0..BODIES_PER_FRAME {
                if let Some(nid) = self.cache.post_load_bodies.pop() {
                    self.cache.body_vol_cache.mark_body_dirty(nid);
                }
            }
            if self.cache.body_vol_cache.has_pending_dirty() {
                self.trigger_body_vol_bake();
            }
            ctx.request_repaint(); // keep frames flowing during warmup
        }
        let _warm_ms = _t_warm.elapsed().as_secs_f64() * 1000.0;

        // Drive GPU map_async callbacks ONCE per frame (non-blocking).
        // Without this, map_ready flags for primary bake and picking only update
        // when viewport_render calls device.poll(Poll) — which runs AFTER this
        // function, causing 1-2 extra frames of latency for bake results.
        let _t_dpoll = std::time::Instant::now();
        if let Some(gpu) = &self.gpu {
            let _ = gpu.device.poll(eframe::wgpu::PollType::Poll);
        }
        let _dpoll_ms = _t_dpoll.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _dpoll_ms > 0.5 { eprintln!("[PollDetail] device.poll(Poll) {:.1}ms", _dpoll_ms); }

        // Collect the async GPU brush batch dispatched LAST frame: write it into the CPU grid + record
        // undo + mark chunks. Runs HERE (poll phase, before input) so the next raycast/stamp sees the
        // current surface — identical freshness to the old blocking readback, with the ~4ms stall gone.
        let _t_vox = std::time::Instant::now();
        self.voxel_brush_collect();
        // Phase A.2 (GPU brick-snapshot undo): apply any queued atlas undo/redo restore. The GPU POOL +
        // CPU INDEX restore mutates GPU resources, so it MUST run here in the poll phase (Frame Ordering
        // Contract), not in the undo handler. NO readback.
        self.voxel_apply_atlas_restore();
        // POOL fix: (re)allocate the shared vertex/index/indirect mesh pool here in the poll phase (the
        // pool is multi-GB — buffer creates are banned outside poll, Frame Ordering Contract). Sized to the
        // surface chunk count; honors a deferred grow flagged by the dispatch path. Runs BEFORE the resident
        // dispatch (which is in update(), after poll), so a chunk that gets a slot draws from a final pool.
        self.voxel_ensure_pool();
        // Phase C (depth-raycast): collect the cursor surface-pick (non-blocking) → `last_surface_hit`
        // for the voxel sculpt brush position, so brush placement needs no CPU grid raycast.
        self.poll_pending_surface_pick();
        let _vox_ms = _t_vox.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _vox_ms > 2.0 { eprintln!("[PollDetail] voxel_poll(brush+atlas+pool+pick) {:.1}ms", _vox_ms); }

        // B3c: split the "ai" SLOWPOLL bucket — a 452ms spike was not named (matcap vs
        // sdf compile vs agent vs claude vs aisvc). Frame Budget: none of these may block
        // the UI thread on a TCP connect; worker threads own HTTP.
        let _t_ai = std::time::Instant::now();
        let _t = std::time::Instant::now();
        self.preload_startup_matcap();
        let _matcap_ms = _t.elapsed().as_secs_f64() * 1000.0;
        let _t = std::time::Instant::now();
        self.poll_sdf_compile(ctx);
        self.poll_pt_pipeline_readiness(ctx);
        let _sdfcomp_ms = _t.elapsed().as_secs_f64() * 1000.0;
        let _t = std::time::Instant::now();
        self.poll_agent(ctx);
        let _agent_ms = _t.elapsed().as_secs_f64() * 1000.0;
        let _t = std::time::Instant::now();
        self.poll_claude(ctx);
        let _claude_ms = _t.elapsed().as_secs_f64() * 1000.0;
        let _t = std::time::Instant::now();
        self.poll_ai_service();
        let _aisvc_ms = _t.elapsed().as_secs_f64() * 1000.0;
        let _ai_ms = _t_ai.elapsed().as_secs_f64() * 1000.0;
        if _ai_ms > 50.0 || (crate::perf_spam_on() && _ai_ms > 2.0) {
            eprintln!(
                "[PollDetail] ai_poll total={:.1}ms matcap={:.1} sdfcomp={:.1} agent={:.1} claude={:.1} aisvc={:.1} think_a={} think_c={}",
                _ai_ms, _matcap_ms, _sdfcomp_ms, _agent_ms, _claude_ms, _aisvc_ms,
                self.ai.agent.is_thinking(),
                self.ai.claude.is_thinking,
            );
        }
        let _t_resident = std::time::Instant::now();
        self.update_resident_volume();
        let _resident_ms = _t_resident.elapsed().as_secs_f64() * 1000.0;
        if _resident_ms > 5.0 { eprintln!("[PollDetail] update_resident_volume {:.1}ms", _resident_ms); }

        let _t_bvc = std::time::Instant::now();
        // STAGE-2 GPU CLASSIFY: capability flag for trigger_bake's sparse FULL arms (set BEFORE
        // poll_body_vol_cache → trigger_bake). With a device, spawn_async_classify queues a
        // `classify_bricks` dispatch record instead of the CPU worker thread; a compile failure
        // at dispatch time still falls back to the worker (dispatch_pending_gpu_classifies).
        self.cache.body_vol_cache.gpu_classify_ready = self.gpu.is_some();
        self.poll_body_vol_cache();
        let _bvc_ms = _t_bvc.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _bvc_ms > 1.0 { eprintln!("[PollDetail] poll_body_vol_cache {:.1}ms", _bvc_ms); }

        let _t_primary = std::time::Instant::now();
        self.poll_primary_bake();
        // Mesh Terrain Phase 1: toast bake wall-ms when a TerrainTile_* bake lands.
        self.maybe_toast_terrain_bake();
        // Phase 2: residency only (visibility + dense/GPU free). Never expands / never
        // transaction() — growth is Patch or Expand Terrain Here (user command).
        self.sync_terrain_resident_window(false);
        // R3b: consume any completed DC-counter readbacks — on a detected output overflow grow the
        // slot's verts/idx buffers (generation-bumped) and re-mesh from the still-resident atlas.
        // Poll phase = the only legal grow site (Frame Ordering Contract).
        self.poll_sdf_mesh_output_overflow();
        // O2b: automatic per-body octree-LOD (distant bodies re-mesh coarser from their resident
        // atlas — no re-bake). Poll phase only; no-ops unless cache.sdf_lod_auto (Dev Tweaks).
        self.poll_sdf_lod_policy();
        // GPU CURVATURE P2 (GPU_CURVATURE_KERNEL_PLAN.md): the VERTEX rung. MUST run after mesh
        // populate/overflow/LOD so `SdfBodyMeshSlot::content_gen` bumps from the remesh are
        // visible — otherwise add-prim rewrites verts into the same buffers and κ stays stale
        // until a manual layer eye-toggle forces rebuild.
        if self.poll_vertex_curvature() {
            // New κ buffer / shape params — redraw next frame without waiting for orbit/scrub.
            ctx.request_repaint();
        }
        let _primary_ms = _t_primary.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _primary_ms > 0.5 { eprintln!("[PollDetail] poll_primary_bake {:.1}ms", _primary_ms); }
        let _t_aitex = std::time::Instant::now();
        self.poll_ai_texture(ctx);
        self.poll_ai_beauty(ctx);
        // GATE 4: queued export → materialize SDF bodies via the shared job (poll phase — the
        // temp-slot/staging buffer creation is banned in the panel), then spawn the OBJ writer.
        self.process_export_pending();
        // O5: async Convert SDF→Poly (no poll(Wait) on the frame thread).
        self.poll_convert_sdf_to_poly();
        self.poll_export(ctx);
        // Kitbash library save worker (never join on draw — was freezing the app).
        self.poll_library_save();
        let _aitex_ms = _t_aitex.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _aitex_ms > 2.0 { eprintln!("[PollDetail] poll_ai_texture+export {:.1}ms", _aitex_ms); }
        let _t_jobs = std::time::Instant::now();
        self.poll_gpu_jobs();
        let _jobs_ms = _t_jobs.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _jobs_ms > 2.0 { eprintln!("[PollDetail] poll_gpu_jobs {:.1}ms", _jobs_ms); }
        let _t_dc = std::time::Instant::now();
        self.poll_dc_jobs();
        let _dc_ms = _t_dc.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _dc_ms > 1.0 { eprintln!("[PollDetail] poll_dc_jobs {:.1}ms", _dc_ms); }
        // D3 settle remesh: AFTER DC mesh land so we polish the fresh extract, not the stale mesh.
        // Queued by maybe_toast_terrain_bake (full-res) / stroke-end; 1 body/frame, never during LMB.
        self.poll_terrain_display_remesh();
        self.refresh_terrain_perf_gauges();
        let _t_pick = std::time::Instant::now();
        self.handle_pending_pick();
        let _pick_ms = _t_pick.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _pick_ms > 2.0 { eprintln!("[PollDetail] handle_pending_pick {:.1}ms", _pick_ms); }
        let _t_tail = std::time::Instant::now();
        // Advance volume pool frame counter (LRU tracking).
        self.cache.body_vol_cache.advance_pool_frame();
        // (Atlas frame-advance + VP bind-group generation check REMOVED 2026-07-13 —
        // the VolumeAtlas is no longer created.)
        // AZRA_MASTER_ROADMAP Phase A1 (corrected): gate ONLY the O(N) walk.
        // `sdf_bodies().count()` filters every node — idle must not pay that.
        // brick_used/capacity are free field reads and feed Properties (properties_render.rs)
        // when brick pool is ON with HUD off — must NOT be HUD-gated (Feature Dependency Audit).
        // brick_body_active / brick_sparsity removed: only reader was the HUD, and
        // `active_bricks()` is a real O(indirection) filter count — not free.
        if self.ui.show_perf_hud {
            self.perf.body_count = self.scene.scene_graph.sdf_bodies().count();
        }
        self.perf.brick_used = self.cache.body_vol_cache.brick_pool.used_bricks();
        self.perf.brick_capacity = self.cache.body_vol_cache.brick_pool.capacity();
        let _tail_ms = _t_tail.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _tail_ms > 2.0 { eprintln!("[PollDetail] poll_tail(pool+atlas+metrics) {:.1}ms", _tail_ms); }
        // Dispatch any pending primary GPU bakes (Level 2).
        // Runs after poll_body_vol_cache → trigger_bake which populates requests.
        // CR-70 Fix C: Skip heavy GPU bake dispatches when previous frame was very
        // long (>1500ms). This prevents cascading TDR: a slow frame suggests the GPU
        // command queue is still draining — submitting more heavy compute would push
        // past the ~2s Windows TDR timeout. Bake requests stay queued and dispatch
        // on the next normal-length frame.
        let _t_dispatch = std::time::Instant::now();
        // STAGE-2 GPU CLASSIFY: finalize settled verdict readbacks BEFORE the landing poll so a
        // finished GPU classify lands (via classify_tx) and bakes this same frame. Like the
        // landing itself, this runs even under the TDR guard — it is a readback + CPU finalize,
        // not a new heavy dispatch.
        self.poll_gpu_classify_readbacks();
        // P1-#7: land finished ASYNC full classifies into the request queue BEFORE the dispatch
        // drain so a landed classify bakes this same frame. Runs even when dispatch is deferred
        // by the TDR guard below — landing is CPU-side bookkeeping; the request just queues.
        self.cache.body_vol_cache.poll_async_classifies();
        if self.prev_frame_wall_ms > 1500.0 {
            eprintln!("[GPU] WARNING: Previous frame {:.0}ms — deferring bake dispatch to avoid TDR | {}",
                self.prev_frame_wall_ms, crate::profiling::perf_stamp());
        } else {
            // STAGE-2 GPU CLASSIFY: the classify compute dispatch is GPU work — gated by the
            // same TDR guard as the bakes (queued records simply wait; the in_flight gate
            // keeps the trigger coalescer deferring their bodies).
            self.dispatch_pending_gpu_classifies();
            self.dispatch_pending_primary_bakes();
        }
        let _dispatch_ms = _t_dispatch.elapsed().as_secs_f64() * 1000.0;
        if _dispatch_ms > 5.0 { eprintln!("[PollDetail] dispatch_primary_bakes {:.1}ms", _dispatch_ms); }
        // DISABLED: warming system removed pending Adobe-style always-cache architecture redesign
        // self.poll_warming();
        let _ms = _spike_t0.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _ms > 10.0 { eprintln!("[SPIKE] poll_background_tasks: {:.1}ms | {}", _ms, crate::profiling::perf_stamp()); }
        // DEFINITIVE breakdown on a pathological poll: dump EVERY sub-timing in one line so the slow
        // step is named even if individual [PollDetail]s are below threshold or scrolled off. `rest` =
        // total minus the sum of bracketed steps = the UNBRACKETED top-of-poll (atlas rebuild/compact).
        // DIAGNOSTIC (steady ~35ms idle-poll): the >200ms breakdown always prints; ADDITIONALLY a throttled
        // 1-in-30 breakdown for 25-200ms polls names the sub-step behind the every-frame hitch. `rest(top)`
        // = the UNBRACKETED top-of-poll. Drop the 25ms arm once the steady hitch is pinned + fixed.
        // P0-5: during an interaction episode ANY >40ms poll prints UNTHROTTLED — a 45-150ms drag hitch
        // is exactly the "still slow while moving" report, and 1-in-30 sampling misses it in short drags.
        // (`interaction_episode.active` is set in viewport_render, so the read here reflects the PREVIOUS
        // frame — fine for a continuous drag, only the drag's first frame falls back to the 1-in-30 arm.)
        {
            use std::cell::Cell;
            thread_local! { static DIAG_N: Cell<u32> = Cell::new(0); }
            let show = if _ms > 200.0 { true }
                else if _ms > 40.0 && self.interaction_episode.active { true }
                // 12ms, NOT 25ms. `[SPIKE]` fires above 10ms (profiling.rs:57) while this
                // breakdown needed 25ms, so a poll parked in the 10-25ms band spiked every
                // frame and NEVER named a sub-step. Measured 2026-08-06: poll_background_tasks
                // ran 21.7-22.6ms for 444 CONSECUTIVE frames — a ~45fps ceiling, 20x the whole
                // library panel, and structurally unattributable in that blind spot. Three
                // separate sub-timer splits chased the library instead because this number was
                // invisible. Do not raise this back above the [SPIKE] threshold.
                else if _ms > 12.0 { DIAG_N.with(|n| { let v = n.get().wrapping_add(1); n.set(v); v % 30 == 0 }) }
                else { false };
            if show {
                // Named sub-brackets (P2). rest = total − sum; if rest is large, a NEW hole opened.
                // `maps_n` = material texture uploads this poll — plateaus ~31 ms × N → GPU map writes.
                let sum = _dlo_ms
                    + _autorepro_ms
                    + _lab_ms
                    + _polypick_ms
                    + _ptaccel_ms
                    + _ao_ms
                    + _rtshadow_ms
                    + _matreset_ms
                    + _dialogs_ms
                    + _env_ms
                    + _libscan_ms
                    + _libimp_ms
                    + _gcurv_ms
                    + _gbake_ms
                    + _mapup_ms
                    + _warm_ms
                    + _dpoll_ms
                    + _vox_ms
                    + _ai_ms
                    + _resident_ms
                    + _bvc_ms
                    + _primary_ms
                    + _aitex_ms
                    + _jobs_ms
                    + _dc_ms
                    + _pick_ms
                    + _tail_ms
                    + _dispatch_ms;
                let rest = _ms - sum;
                eprintln!(
                    "[SLOWPOLL] total={:.0}ms | dlo={:.0} lab={:.0} polypick={:.0} ptaccel={:.0} ao={:.0} \
                     mapup={:.0}(n={}) env={:.0} gbake={:.0} gcurv={:.0} lib={:.0}/{:.0} \
                     warm={:.0} dpoll={:.0} vox={:.0} ai={:.0} resident={:.0} bvc={:.0} primary={:.0} \
                     aitex={:.0} jobs={:.0} dc={:.0} pick={:.0} tail={:.0} dispatch={:.0} | rest={:.0}",
                    _ms,
                    _dlo_ms,
                    _lab_ms,
                    _polypick_ms,
                    _ptaccel_ms,
                    _ao_ms,
                    _mapup_ms,
                    _g7_map_uploads,
                    _env_ms,
                    _gbake_ms,
                    _gcurv_ms,
                    _libscan_ms,
                    _libimp_ms,
                    _warm_ms,
                    _dpoll_ms,
                    _vox_ms,
                    _ai_ms,
                    _resident_ms,
                    _bvc_ms,
                    _primary_ms,
                    _aitex_ms,
                    _jobs_ms,
                    _dc_ms,
                    _pick_ms,
                    _tail_ms,
                    _dispatch_ms,
                    rest
                );
            }
        }

        // Rank top-2 named poll costs for Perf HUD (P2).
        {
            let parts: [(&'static str, f32); 24] = [
                ("dlo", _dlo_ms as f32),
                ("lab", _lab_ms as f32),
                ("polypick", _polypick_ms as f32),
                ("ptaccel", _ptaccel_ms as f32),
                ("ao", _ao_ms as f32),
                ("rtshadow", _rtshadow_ms as f32),
                ("matreset", _matreset_ms as f32),
                ("dialogs", _dialogs_ms as f32),
                ("env", _env_ms as f32),
                ("libscan", _libscan_ms as f32),
                ("libimp", _libimp_ms as f32),
                ("gcurv", _gcurv_ms as f32),
                ("gbake", _gbake_ms as f32),
                ("mapup", _mapup_ms as f32),
                ("warm", _warm_ms as f32),
                ("dpoll", _dpoll_ms as f32),
                ("vox", _vox_ms as f32),
                ("ai", _ai_ms as f32),
                ("resident", _resident_ms as f32),
                ("bvc", _bvc_ms as f32),
                ("primary", _primary_ms as f32),
                ("aitex", _aitex_ms as f32),
                ("jobs", _jobs_ms as f32),
                ("dc", _dc_ms as f32),
            ];
            let mut top = ("-", 0.0f32);
            let mut second = ("-", 0.0f32);
            for (name, ms) in parts {
                if ms > top.1 {
                    second = top;
                    top = (name, ms);
                } else if ms > second.1 {
                    second = (name, ms);
                }
            }
            // Include pick/tail/dispatch in ranking too
            for (name, ms) in [
                ("pick", _pick_ms as f32),
                ("tail", _tail_ms as f32),
                ("dispatch", _dispatch_ms as f32),
            ] {
                if ms > top.1 {
                    second = top;
                    top = (name, ms);
                } else if ms > second.1 {
                    second = (name, ms);
                }
            }
            let named_sum = parts.iter().map(|p| p.1 as f64).sum::<f64>()
                + _pick_ms
                + _tail_ms
                + _dispatch_ms
                + _autorepro_ms;
            self.perf.poll_tasks_ms = _ms;
            self.perf.poll_rest_ms = (_ms - named_sum).max(0.0) as f32;
            self.perf.poll_top_name = top.0;
            self.perf.poll_top_ms = top.1;
            self.perf.poll_2nd_name = second.0;
            self.perf.poll_2nd_ms = second.1;
            self.perf.poll_map_uploads = _g7_map_uploads;
        }

        // AZRA_MASTER_ROADMAP §0b scoreboard: set AZRA_SCOREBOARD=1 to print a throttled
        // idle sample (poll ms + top/2nd). Use with one cube, camera idle, HUD optional.
        // Fills the living scoreboard without trusting C-phase "wins."
        if std::env::var_os("AZRA_SCOREBOARD").is_some() {
            use std::cell::Cell;
            use std::time::Instant;
            thread_local! {
                static LAST: Cell<Option<Instant>> = Cell::new(None);
            }
            let now = Instant::now();
            let fire = LAST.with(|c| {
                let due = match c.get() {
                    None => true,
                    Some(t) => now.duration_since(t).as_secs_f32() >= 2.0,
                };
                if due {
                    c.set(Some(now));
                }
                due
            });
            if fire {
                // frame_time_ms is previous complete frame (recorded at end of update).
                // cpu_section: [0]input [1]mesh [2]dispatch [3]submit [4]overlays [5]layout [6]dirty
                let s = &self.ui.cpu_section_ms;
                // Refresh shared-mesh metric for B.4 (cheap when few nodes).
                self.perf.shared_mesh_cids = self.shared_mesh_cache_id_count();
                // Median over the 120-sample ring — single-sample 7–12 ms bands are illegible.
                let frame_med = self.perf.frame_percentile(0.5);
                // G9: GPU pass times (1–2 frames lag). Indices match gpu_timestamps::PASS_*.
                // gpu_ms = sum of all resolved slots (real GPU, not CPU wall). rast/shEnc/gbuf
                // on the prior segment remain CPU encode times — use gpu* for Phase D tuning.
                let g = &self.ui.prof_gpu_pass_times;
                let gpu_shadow = g.get(11).copied().unwrap_or(0.0);
                let gpu_gbuf = g.get(12).copied().unwrap_or(0.0);
                let gpu_def = g.get(1).copied().unwrap_or(0.0);
                let gpu_tonemap = g.get(8).copied().unwrap_or(0.0);
                let gpu_ssao = g.get(14).copied().unwrap_or(0.0);
                let gpu_rt = g.get(15).copied().unwrap_or(0.0);
                let gpu_ms: f32 = g.iter().sum();
                let gpu_ts = if self.ui.prof_gpu_timestamps_available { "ok" } else { "off" };
                let line = format!(
                    "[SCOREBOARD] frame={:.2}ms med={:.2}ms poll={:.2}ms top={} {:.2} 2nd={} {:.2} rest={:.2} \
                     maps={} dirty={:#x} | in={:.1} mesh={:.1} disp={:.1} sub={:.1} ov={:.1} lay={:.1} dty={:.1} \
                     | plod={:.2} occ={:.2} share={} draws={} inst+{} instUp={} matUp={} occCull={}/{} late={} \
                     streamReb={} streamN={} structReb={} structN={} \
                     | clSel={} clCam={} clOv={} clMeshFb={} \
                     | rast={:.1} frust={:.1} sort={:.1} up={:.1} shEnc={:.1} gbuf={:.1} rSub={:.1} dRest={:.1} \
                     | gpu_ms={:.2} gpuTs={} gpuSh={:.2} gpuGb={:.2} gpuDf={:.2} gpuTm={:.2} gpuAo={:.2} gpuRt={:.2}",
                    self.perf.frame_time_ms,
                    frame_med,
                    self.perf.poll_tasks_ms,
                    self.perf.poll_top_name,
                    self.perf.poll_top_ms,
                    self.perf.poll_2nd_name,
                    self.perf.poll_2nd_ms,
                    self.perf.poll_rest_ms,
                    self.perf.poll_map_uploads,
                    self.perf.dirty_flags_this_frame,
                    s[0],
                    s[1],
                    s[2],
                    s[3],
                    s[4],
                    s[5],
                    s[6],
                    self.perf.poly_lod_ms,
                    self.perf.occlusion_ms,
                    self.perf.shared_mesh_cids,
                    self.render.last_instance_draws,
                    self.render.last_instance_merged,
                    self.render.last_instance_uploads,
                    self.render.last_material_uploads,
                    self.render.last_occlusion_culled,
                    self.render.last_occlusion_tested,
                    self.render.hzb_late_recovered,
                    self.draw_stream_rebuilds_last_frame,
                    self.draw_stream.len(),
                    self.structural_draw_rebuilds_last_frame,
                    self.structural_draw_calls.len(),
                    self.render.cluster_cut_selected,
                    self.render.cluster_cut_cam_selected,
                    self.render.cluster_cut_overflow,
                    u32::from(self.render.cluster_overflow_mesh_fallback),
                    self.perf.raster_ms,
                    self.perf.raster_frustum_ms,
                    self.perf.raster_sort_ms,
                    self.perf.raster_upload_ms,
                    self.perf.raster_shadow_enc_ms,
                    self.perf.raster_gbuf_enc_ms,
                    self.perf.raster_submit_ms,
                    self.perf.dispatch_rest_ms,
                    gpu_ms,
                    gpu_ts,
                    gpu_shadow,
                    gpu_gbuf,
                    gpu_def,
                    gpu_tonemap,
                    gpu_ssao,
                    gpu_rt,
                );
                eprintln!("{line}");
                // Windows GUI often swallows stderr — also append cwd scoreboard.log.
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("scoreboard.log")
                {
                    use std::io::Write;
                    let _ = writeln!(f, "{line}");
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::effective_shadow_res;

    /// TERRAINCLAMPCRASH regression: the 8192² "ultra" base the render panel
    /// offers must not panic (`Ord::clamp` min > max) and must be honored as-is
    /// — the E2 ceiling limits the idle LADDER, never the user's base.
    #[test]
    fn shadow_ladder_survives_ultra_base() {
        for boost in 0..=3u32 {
            assert_eq!(effective_shadow_res(8192, boost, 4096), 8192);
            assert_eq!(effective_shadow_res(8192, boost, 2048), 8192);
        }
    }

    /// For every base at or below the 4096 ceiling the min→max composition is
    /// bit-identical to the old `cap.clamp(base, 4096)` (median-of-three), so
    /// no previously-working configuration changes resolution. cap=8192 covers
    /// a stale persisted "Idle shadow cap" from before E2 dropped that tier.
    #[test]
    fn shadow_ladder_matches_old_behavior_at_or_below_ceiling() {
        for &base in &[1024u32, 2048, 4096] {
            for &cap in &[2048u32, 4096, 8192] {
                let old_cap = cap.clamp(base, 4096); // safe: base <= 4096 here
                for boost in 0..=2u32 {
                    let want = if boost == 0 { base } else { (base << boost).min(old_cap) };
                    assert_eq!(
                        effective_shadow_res(base, boost, cap),
                        want,
                        "base={base} cap={cap} boost={boost}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod pt_pipeline_readiness_tests {
    use super::{
        latched_pt_choice, pt_warmup_notice, PtCompileWake, PtPendingStages, PtPipelineWarmup, PtStage, PtStaged,
        PtWarmupNotice,
    };

    /// Stand-in payload: the stage bookkeeping is tested headless, without a GPU device.
    #[derive(Debug, PartialEq, Eq)]
    struct Stub(PtStage);

    impl PtStaged for Stub {
        fn stage(&self) -> PtStage {
            self.0
        }
    }

    fn started() -> (PtPipelineWarmup<Stub>, std::sync::mpsc::Sender<Stub>) {
        let mut warmup = PtPipelineWarmup::<Stub>::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        warmup.begin(receiver);
        (warmup, sender)
    }

    #[test]
    fn cold_pt_starts_once_and_does_not_retry_a_failed_worker() {
        let mut warmup = PtPipelineWarmup::<Stub>::default();
        assert!(warmup.can_request(false, false));
        assert!(!warmup.can_request(true, false));
        assert!(!warmup.can_request(false, true));
        assert!(!warmup.failed);
        let (sender, receiver) = std::sync::mpsc::channel();
        warmup.begin(receiver);
        assert!(!warmup.can_request(false, false));
        assert_eq!(warmup.take_ready(), Ok(None));
        drop(sender);
        let all = PtPendingStages { full: true, interactive: true, native: true };
        assert_eq!(warmup.take_ready(), Err(all));
        assert!(warmup.failed());
        assert!(!warmup.pending().any());
        assert!(!warmup.can_request(false, false));
    }

    /// PT-COLD-WARMUP-BUNDLED: each stage is consumed the moment it arrives — the full stage
    /// is NOT held back until the interactive/native stages finish — and the per-stage pending
    /// flags drain in order. A completed warmup can re-warm after an invalidation.
    #[test]
    fn staged_warmup_delivers_full_first_and_tracks_each_stage() {
        let (mut warmup, sender) = started();
        assert_eq!(warmup.pending(), PtPendingStages { full: true, interactive: true, native: true });
        sender.send(Stub(PtStage::Full)).unwrap();
        assert_eq!(warmup.take_ready(), Ok(Some(Stub(PtStage::Full))));
        assert_eq!(warmup.pending(), PtPendingStages { full: false, interactive: true, native: true });
        assert_eq!(warmup.take_ready(), Ok(None), "later stages still compiling — nothing to adopt");
        assert!(!warmup.can_request(false, false), "the in-flight worker owns the remaining stages");
        sender.send(Stub(PtStage::Interactive)).unwrap();
        assert_eq!(warmup.take_ready(), Ok(Some(Stub(PtStage::Interactive))));
        assert_eq!(warmup.pending(), PtPendingStages { full: false, interactive: false, native: true });
        sender.send(Stub(PtStage::Native)).unwrap();
        assert_eq!(warmup.take_ready(), Ok(Some(Stub(PtStage::Native))));
        assert!(!warmup.pending().any());
        drop(sender);
        assert_eq!(warmup.take_ready(), Ok(None), "a finished worker's hang-up is not a failure");
        assert!(!warmup.failed());
        assert!(!warmup.can_request(true, false));
        assert!(!warmup.can_request(false, true));
        assert!(warmup.can_request(false, false), "invalidated full kernel can re-warm");
    }

    #[test]
    fn worker_lost_after_full_stage_reports_only_the_missing_stages() {
        let (mut warmup, sender) = started();
        sender.send(Stub(PtStage::Full)).unwrap();
        drop(sender);
        assert_eq!(warmup.take_ready(), Ok(Some(Stub(PtStage::Full))));
        assert_eq!(
            warmup.take_ready(),
            Err(PtPendingStages { full: false, interactive: true, native: true })
        );
        assert!(warmup.failed());
    }

    #[test]
    #[should_panic(expected = "delivered stage Full twice")]
    fn a_stage_delivered_twice_is_a_worker_bug() {
        let (mut warmup, sender) = started();
        sender.send(Stub(PtStage::Full)).unwrap();
        sender.send(Stub(PtStage::Full)).unwrap();
        let _ = warmup.take_ready();
        let _ = warmup.take_ready();
    }

    /// The InteractionRenderPolicy latch for the PT interaction kernel (and the traversal
    /// backend): frame-by-frame simulation of the interactive kernel landing MID-DRAG.
    #[test]
    fn interaction_kernel_landing_mid_episode_is_adopted_only_after_the_latch() {
        // (interactive installed, episode/cooldown latched) -> kernel the half-res path runs
        let frames = [
            (false, false, false), // PT live on the full kernel, idle
            (false, true, false),  // drag starts: full kernel at the interaction budget
            (true, true, false),   // interactive lands mid-drag: NOT adopted (no path switch)
            (true, true, false),   // still dragging / cooling down
            (true, false, true),   // latch released: adopted
            (true, true, true),    // next drag runs the interactive kernel throughout
        ];
        let mut active = false;
        for (frame, (installed, latched, expected)) in frames.into_iter().enumerate() {
            active = latched_pt_choice(installed, active, latched);
            assert_eq!(active, expected, "frame {frame}");
        }
        // Forced fallback: a held kernel that disappears (re-warm retired it) is dropped even
        // inside the latch — it can no longer be dispatched.
        assert!(!latched_pt_choice(false, true, true));
        // Never adopts mid-latch, never invents availability.
        assert!(!latched_pt_choice(true, false, true));
        assert!(!latched_pt_choice(false, false, false));
    }

    #[test]
    fn warmup_notice_is_prominent_only_while_the_display_kernel_is_missing() {
        let all = PtPendingStages { full: true, interactive: true, native: true };
        let late = PtPendingStages { full: false, interactive: true, native: true };
        let native_only = PtPendingStages { full: false, interactive: false, native: true };
        let none = PtPendingStages::default();
        assert_eq!(pt_warmup_notice(false, all, false), PtWarmupNotice::Preparing);
        assert_eq!(pt_warmup_notice(false, none, false), PtWarmupNotice::Preparing, "before the first request");
        assert_eq!(pt_warmup_notice(false, none, true), PtWarmupNotice::Unavailable);
        assert!(matches!(pt_warmup_notice(true, late, false), PtWarmupNotice::Refining(note) if note.contains("full kernel")));
        assert!(matches!(pt_warmup_notice(true, native_only, false), PtWarmupNotice::Refining(note) if note.contains("software traversal")));
        assert_eq!(pt_warmup_notice(true, none, false), PtWarmupNotice::Silent);
        assert_eq!(pt_warmup_notice(true, none, true), PtWarmupNotice::Silent, "late-stage loss keeps PT live");
    }

    #[test]
    fn pt_worker_wake_disconnects_before_requesting_repaint() {
        let (sender, receiver) = std::sync::mpsc::channel::<u8>();
        let wake = PtCompileWake { sender: Some(sender), context: eframe::egui::Context::default() };
        wake.deliver(3);
        assert_eq!(receiver.try_recv(), Ok(3));
        drop(wake);
        assert_eq!(receiver.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected));
    }

    #[test]
    fn pt_render_readiness_checks_are_read_only() {
        let _: fn(&crate::GpuCtx) -> bool = crate::gpu_init::ensure_pt_tri_pipeline;
        let _: fn(&crate::GpuCtx) -> bool = crate::gpu_init::ensure_pt_interactive_pipeline;
        let _: fn(&crate::GpuCtx) -> bool = crate::gpu_init::ensure_pt_denoise_pipeline;
        let _: fn(&crate::GpuCtx) -> bool = crate::gpu_init::ensure_pt_upscale_pipeline;
    }
}
