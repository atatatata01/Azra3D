// core_engine/app/src/gpu_ctx.rs
//
// Slice 29 (app-crate de-god, PURE RELOCATION from main.rs): the GpuCtx family of GPU-plumbing
// data types + the map_state module. These are standalone data types + their own Default/inherent
// impls — NONE are methods on FourthLayerApp/AzraApp. Every flat path (crate::GpuCtx, crate::PendingBake,
// etc.) still resolves via the `pub(crate) use gpu_ctx::*;` re-export in main.rs.

use eframe::egui;
use eframe::wgpu;
use graphics::gpu_volume::SdfGpuVolume;
use app_gpu::gpu_timestamps;

/// Tri-state result of a `map_async` readback. A bare `AtomicBool` can only encode
/// pending vs done — an `Err` (device loss / OOM / aborted map) is indistinguishable
/// from "still pending", so the failed entry never drains: it leaks its staging buffer
/// (~67 MB at 256³) and stalls the body in-flight forever. This makes the failure
/// OBSERVABLE — the poll loop drains BOTH `Ok` and `Err`, processing the former and
/// recovering the latter (free buffer + `clear_in_flight` → re-bake). Satisfies the
/// CLAUDE.md GPU-safety rule (capture the `map_async` Result; capture-then-ignore is
/// the same discard).
pub(crate) mod map_state {
    use std::sync::atomic::{AtomicU8, Ordering};
    pub const PENDING: u8 = 0;
    pub const OK: u8 = 1;
    pub const ERR: u8 = 2;

    /// Shared `map_async` result cell. `store_result` is the callback sink (never drops
    /// `Err`); `peek` is the non-blocking poll read.
    pub struct MapState(AtomicU8);
    impl MapState {
        pub fn new() -> Self { MapState(AtomicU8::new(PENDING)) }
        /// Record the Result delivered to the `map_async` callback. Encodes `Err` as `ERR`
        /// rather than discarding it.
        pub fn store_result<E>(&self, r: &Result<(), E>) {
            let v = if r.is_ok() { OK } else { ERR };
            self.0.store(v, Ordering::SeqCst);
        }
        /// Non-blocking read: `PENDING` / `OK` / `ERR`.
        pub fn peek(&self) -> u8 { self.0.load(Ordering::SeqCst) }
    }

    #[cfg(test)]
    mod tests {
        use super::{MapState, ERR, OK, PENDING};

        #[test]
        fn callback_result_has_distinct_pending_ready_and_failed_states() {
            let ready = MapState::new();
            assert_eq!(ready.peek(), PENDING);
            ready.store_result(&Result::<(), ()>::Ok(()));
            assert_eq!(ready.peek(), OK);

            let failed = MapState::new();
            assert_eq!(failed.peek(), PENDING);
            failed.store_result(&Result::<(), ()>::Err(()));
            assert_eq!(failed.peek(), ERR);
        }
    }
}

/// Primary GPU context — wraps device + queue + all GPU resources.
/// Pending primary-GPU bake job — tracks staging buffer + readback state.
pub(crate) struct PendingBake {
    pub(crate) body_nid: core_math::scene_graph::NodeId,
    pub(crate) generation: u64,
    pub(crate) source_hash: u64,
    /// `None` = path-A full-bake-direct: the bake wrote `rv.buffer` in place, there is NO CPU staging
    /// readback, and the surface is meshed straight from `rv.buffer`. `Some(buf)` = legacy readback path.
    pub(crate) staging_buffer: Option<wgpu::Buffer>,
    /// Tri-state readback status. `OK` → process; `ERR` → drain + recover (the map failed, so the
    /// staging buffer holds no valid data); `PENDING` → leave in flight. Replaces the old `AtomicBool`
    /// that silently dropped map errors → leaked staging buffer + stalled body.
    pub(crate) map_ready: std::sync::Arc<map_state::MapState>,
    pub(crate) grid_size: usize,
    /// Per-axis bake resolution [x, y, z].
    pub(crate) resolution: [u32; 3],
    /// True when this bake covered only a dirty sub-region (not the whole grid). Step 2 lazy readback
    /// skips the CPU to_vec ONLY for partial bakes — a full bake must refresh body_grids (else save/
    /// duplicate would read a stale grid after editing).
    pub(crate) is_partial: bool,
    /// Path-A (`gpu_full_bake_path_a`): this FULL bake wrote `rv.buffer` in place, so `staging_buffer` is
    /// `None`, the readback is skipped, and completion routes through `note_resident_bake_complete`
    /// (body_grids left stale on purpose — the on-demand RAM-undo materialize handles it). Default false.
    pub(crate) field_direct: bool,
    /// S2.4/S2.5 (SparseBricks mode-2): `Some` = this bake wrote the
    /// persistent `gpu.sdf_brick_atlas` POOL (staging_buffer=None, fenced via
    /// on_submitted_work_done); completion routes through `record_sparse_bake` (Full) /
    /// `record_sparse_partial_bake` (Partial) — NEVER `accept_gpu_result` (cbrt-inference on an
    /// empty grid) and never the field-direct/lazy paths. One field carries the discriminant
    /// ("this pending is sparse"), the payload (the atlas generation the brick_resident marker
    /// records) and, for Full, the CPU-retained frozen index — so they can't desync. `None` on
    /// every dense path.
    pub(crate) sparse: Option<SparsePending>,
    /// Bake bounds captured AT DISPATCH (`req.bounds_min/max`). The result MUST be stamped with THESE, not
    /// the live `body_bounds`: an `aabb_escape` can mutate `body_bounds` to new (expanded) bounds while this
    /// bake — computed for the OLD bounds — is still in flight. Stamping the old grid with the new bounds
    /// re-interprets it under a ~2× world extent → the scaled "white box" ghost enrobing the geometry.
    /// Provenance travels with the bake so a concurrent escape can never mis-scale a landing result.
    pub(crate) bounds_min: glam::Vec3,
    pub(crate) bounds_max: glam::Vec3,
    /// GATE 2 / C2 — the bake's dirty VOXEL window `[min, max)` captured AT DISPATCH
    /// (`req.bake_min/max`). Pure TRANSPORT: acceptance feeds it to the cache's single
    /// `DirtyMeshRegion` producer (`note_bake_accepted*`) — partial completions union it,
    /// full completions clear the region. No consumer reads this field directly.
    pub(crate) bake_window: ([u32; 3], [u32; 3]),
}

/// S2.4/S2.5: how a pending SparseBricks (mode-2) bake completes.
pub(crate) enum SparsePending {
    /// S2.4 FULL sparse bake: a fresh classifier index was uploaded at dispatch; `frozen` is the
    /// CPU-retained copy of that EXACT index, handed to `record_sparse_bake` at completion so the
    /// `BodyCacheEntry` keeps it next to the `brick_resident` marker (`atlas_gen`) — the basis of
    /// the S2.5 frozen-index partial path.
    Full {
        atlas_gen: u64,
        frozen: std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>,
    },
    /// S2.5 PARTIAL sparse bake: pool content updated in place over the dilated dirty window.
    /// `atlas_gen` is the entry's brick_resident marker AT TRIGGER TIME — completion validates
    /// against it. `update` (S2.5c) is `Some` when incremental allocation re-uploaded an updated
    /// index at dispatch: `(the updated frozen index to install, the POST-GROW atlas generation
    /// to stamp)` — a pool growth bumps the atlas generation, and the completion must move the
    /// marker to it or every subsequent partial would false-escalate on generation mismatch.
    /// `None` = plain frozen-index partial (hash refresh only; marker/index untouched).
    Partial {
        atlas_gen: u64,
        update: Option<(std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>, u64)>,
    },
}

/// Pending primary-GPU brick fill — tracks staging buffer + readback state.
pub(crate) struct PendingBrickFill {
    pub(crate) body_nid: core_math::scene_graph::NodeId,
    pub(crate) staging_buffer: wgpu::Buffer,
    pub(crate) map_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Pool slot indices for the bricks that were dispatched.
    pub(crate) slots: Vec<u32>,
    /// Total readback size in bytes.
    pub(crate) total_readback_bytes: u64,
}

/// Pending GPU pick — dispatched on click, applied on poll (often same/next frame).
pub(crate) struct PendingPick {
    /// Readback state (render::picking::MAP_PENDING / MAP_OK / MAP_FAILED). Kept for the
    /// abandon path; normal picks complete via [`Self::resolved_raw`] with staging already unmapped.
    pub(crate) map_ready: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Pixel id already decoded + **staging unmapped** (blocking readback on the click frame).
    /// When `Some`, poll must NOT touch the GPU map again — that was the 2026-08-10 crash class:
    /// leaving `picking_staging` mapped across later `queue.submit`s panics wgpu.
    pub(crate) resolved_raw: Option<u64>,
    pub(crate) ctrl: bool,
    pub(crate) shift: bool,
    /// Node the SAME-FRAME CPU raycast already selected for this click, if any. The GPU
    /// confirm must not act again on the same node - with ctrl held, a second
    /// toggle_select_unified would flip the selection straight back OFF (the "cannot
    /// select multiple objects" bug). Only a DISAGREEING GPU result may correct.
    pub cpu_resolved: Option<core_math::scene_graph::NodeId>,
}

/// Phase C (depth-raycast): an in-flight single-pixel readback of `gbuf_normal` at the cursor. The
/// world ray (`ro`,`rd`) captured at copy time reconstructs the hit (`ro + rd·world_dist`) when the
/// readback lands a frame later, so a mid-readback camera move can't skew it. Non-blocking (mirrors
/// `PendingPick`).
pub(crate) struct PendingSurfacePick {
    /// Explicit callback result: `PENDING` waits, `OK` decodes, `ERR` releases the staging buffer so
    /// the next frame can retry. A success-only bool leaves a failed pick in flight forever.
    pub(crate) map_ready: std::sync::Arc<map_state::MapState>,
    pub(crate) ro: glam::Vec3,
    pub(crate) rd: glam::Vec3,
}

/// Result of a background Rayon DC mesh extraction job.
pub(crate) struct DcJobResult {
    pub(crate) body_nid: core_math::scene_graph::NodeId,
    pub(crate) mesh: core_math::mesh::Mesh,
    pub(crate) chunked: core_math::chunked_dc::ChunkedMesh,
    pub(crate) mesh_cache_id: core_math::scene_graph::MeshCacheId,
    pub(crate) source_hash: u64,
    pub(crate) lod: render::mesh_cache::MeshLod,
    pub(crate) generation: u64,
}

/// Pending background DC extraction — Rayon thread produces DcJobResult.
pub(crate) struct PendingDcJob {
    pub(crate) body_nid: core_math::scene_graph::NodeId,
    pub(crate) rx: std::sync::mpsc::Receiver<DcJobResult>,
}

/// STAGE-2 GPU CLASSIFY — an in-flight `classify_bricks` dispatch: per-brick verdicts (one u32
/// per brick, bitcast into the f32 arena at binding 2) copied to a MAP_READ staging buffer.
/// `poll_gpu_classify_readbacks` drains settled entries: decode → `sparse_index_from_verdicts`
/// (the shared CPU finalize) → land through `classify_tx`. The request travels with the flight —
/// generation/regime validation happens ONCE, at the landing (`poll_async_classifies`).
pub(crate) struct PendingGpuClassify {
    pub(crate) request: crate::body_volume_cache::PrimaryBakeRequest,
    pub(crate) staging: wgpu::Buffer,
    pub(crate) map_ready: std::sync::Arc<map_state::MapState>,
    pub(crate) total_bricks: usize,
    pub(crate) dims_bricks: [u32; 3],
    /// Dispatch submit time — the `[GpuClassify] verdicts=` figure (submit → map-ready wall
    /// time: GPU pass + readback latency, not pure kernel time).
    pub(crate) submitted: std::time::Instant,
    /// STAGE-3 GPU FINALIZE: `true` = the dilate/alloc passes ran on-device and `staging` holds
    /// the FINAL index (`total_bricks` u32 tags + 1 trailing `allocated` count) — the drain
    /// validates + lands it directly, no worker finalize. `false` = legacy verdict readback
    /// (`total_bricks` u32 verdicts) finalized by the CPU (`sparse_index_from_verdicts`).
    pub(crate) gpu_finalized: bool,
}

/// STAGE-3 GPU FINALIZE — the four `classify_finalize_*` entry points of volume_bake.wgsl
/// (dilate → per-block scan → block-sum scan → emit), compiled from the SAME module + bake BGL
/// as `classify_bricks` and hot-swapped/ensured alongside it. Always all-or-nothing: a classify
/// either finalizes fully on the GPU or falls back to the CPU finalize as a set.
pub(crate) struct ClassifyFinalizePipelines {
    pub(crate) dilate: wgpu::ComputePipeline,
    pub(crate) scan: wgpu::ComputePipeline,
    pub(crate) block_scan: wgpu::ComputePipeline,
    pub(crate) emit: wgpu::ComputePipeline,
}

/// Messages sent from the background WGSL pipeline compilation thread.
pub(crate) enum PipelineReady {
    DeferredLite(wgpu::ComputePipeline),
    // (Raymarch / RaymarchMarch REMOVED 2026-07-14 — analytical march display path stripped)
    RaymarchLite(wgpu::ComputePipeline),
    SculptRaymarch(wgpu::ComputePipeline),
    DeferredFull(wgpu::ComputePipeline),
    /// Full `pass_fog` + analytic `pass_fog_lite` from the same WGSL module.
    Fog { full: wgpu::ComputePipeline, lite: wgpu::ComputePipeline },
    Fxaa(wgpu::ComputePipeline),
    Pathtrace(wgpu::ComputePipeline),
    PtDenoise(wgpu::ComputePipeline),
    PrimaryBake(wgpu::ComputePipeline),
    /// STAGE-2 GPU CLASSIFY: the `classify_bricks` entry of the SAME volume_bake.wgsl module,
    /// compiled right after the bake pipeline on the background thread (same layout/BGL) —
    /// together with the STAGE-3 finalize entries (dilate/scan/block_scan/emit).
    ClassifyBricks(wgpu::ComputePipeline, ClassifyFinalizePipelines),
    BrickFill(wgpu::ComputePipeline),
    Temporal(wgpu::ComputePipeline),
    /// The shared SDF DC mesher (sdf_mesh.wgsl — 2 modules, 5 pipelines), compiled on the
    /// background thread so the FIRST select's bake-accept → `sdf_populate_resident` never pays
    /// the ~560ms synchronous compile inside poll_primary_bake (the first-select stall,
    /// owner log 5 2026-07-16). Installed into `cache.sdf_mesher` (CacheState, not the registry).
    SdfMesher(crate::sdf_gpu_mesh::SdfMeshGpu),
    /// SSAO pass pipelines (render/src/ssao.rs — 3 tiny compute pipelines, one
    /// module). Background-compiled with the rest; `ensure_ssao_pipelines` is
    /// the lazy fallback (pt_denoise precedent).
    Ssao(render::ssao::SsaoPipelines),
    /// RT projected shadows (BGL + pipeline). CONSOLE_AUDIT B3: background-compile so
    /// first RT-shadow use is not a 90 ms main-thread hitch. Lazy `ensure_rt_shadow_pipeline`
    /// remains the fallback.
    RtShadow {
        bgl: wgpu::BindGroupLayout,
        pipeline: wgpu::ComputePipeline,
    },
}

/// All compiled GPU compute pipelines + compilation state.
/// Invalidated by: shader source changes, node graph recompile, device loss.
/// Rebuilt by: background WGSL compile thread, lazy init on first use (fog/fxaa/pt/bake/brick/temporal).
pub(crate) struct PipelineRegistry {
    pub(crate) pt_warmup: crate::app_poll::PtPipelineWarmup,
    pub(crate) pt_tri_interactive_pipeline: Option<wgpu::ComputePipeline>,
    // WGSL SDF raymarch — lite only (floor + background). The full/march analytical
    // display pipelines were REMOVED 2026-07-14 (dead analytical march path).
    pub(crate) pipeline_lite: Option<wgpu::ComputePipeline>,
    /// Selected-prim outline mask pass (owner feature): marches ONLY the selected prims via
    /// the exact production eval_prim into overlay_texture; composited by BlendMode::MaskOutline.
    /// AUTO pipeline layout (derived from the entry's referenced bindings — a handful of the
    /// shared group-0 slots), lazy-compiled on first prim selection (small stripped stack).
    pub(crate) prim_outline_pipeline: Option<wgpu::ComputePipeline>,
    // (GLSL/SPIR-V BVH pipelines REMOVED 2026-07-14 — dead analytical display path stripped)
    // (volume_preview_pipeline REMOVED 2026-07-13 — SDF volume-raymarch display stripped)
    // Sculpt voxel raymarch (Plan B1) — raymarch the resident sparse atlas during a brush stroke.
    pub(crate) volume_preview_sculpt_pipeline: Option<wgpu::ComputePipeline>,
    // HDR tonemap
    pub(crate) tonemap_pipeline: wgpu::ComputePipeline,
    // Bloom
    pub(crate) bloom_extract_pipeline: wgpu::ComputePipeline,
    pub(crate) bloom_blur_h_pipeline: wgpu::ComputePipeline,
    pub(crate) bloom_blur_v_pipeline: wgpu::ComputePipeline,
    // Deferred shading
    pub(crate) deferred_pipeline: Option<wgpu::ComputePipeline>,
    pub(crate) deferred_lite_pipeline: Option<wgpu::ComputePipeline>,
    // SSR
    pub(crate) ssr_pipeline: wgpu::ComputePipeline,
    // Env GGX prefilter (split-sum IBL) — lazy-compiled on first HDRI install
    // (`ensure_env_prefilter_pipeline`); dispatched from poll, one mip per frame.
    pub(crate) env_prefilter_pipeline: Option<wgpu::ComputePipeline>,
    // Volumetric fog — lazy-compiled on first use
    pub(crate) fog_pipeline: Option<wgpu::ComputePipeline>,
    /// P2 analytic Lite fog (closed-form height haze) — same module, entry `pass_fog_lite`.
    pub(crate) fog_lite_pipeline: Option<wgpu::ComputePipeline>,
    // FSR-style upsampling
    pub(crate) upsample_pipeline: wgpu::ComputePipeline,
    // SDF Bevel Preview
    pub(crate) bevel_preview_pipeline: wgpu::ComputePipeline,
    // Path tracing — lazy-compiled on first use
    /// Triangle PT (Phase 2 — the ONLY path tracer; the analytical PT entry was
    /// DELETED after the tri PT shipped + was owner-verified, per the app-wide
    /// analytical ban. Never reintroduce a pt_pipeline that marches the SDF.)
    pub(crate) pt_tri_pipeline: Option<wgpu::ComputePipeline>,
    /// Hardware ray-query variants of the SAME canonical tri PT (full + interactive). Only
    /// traversal differs; materials/lighting/integrator are the canonical WGSL. Published by
    /// the PT warmup together with `pt_tri_pipeline`; `None` when the device lacks
    /// `app_gpu::native_rt::REQUIRED_FEATURES` (software traversal stays the PT backend).
    pub(crate) native_rt_pipelines: Option<std::sync::Arc<app_gpu::native_rt::NativeRtPipelines>>,
    /// RT projected shadows (RT_SHADOWS_DESIGN Stage 1) — fires occlusion rays against the
    /// triangle-PT BVH (shared PT_TRAVERSAL), writes the per-light visibility texture the
    /// deferred point/spot lighting consumes. Lazy-compiled on the first RT-shadow frame
    /// (`ensure_rt_shadow_pipeline`) — never compiled for users who leave the toggle off.
    pub(crate) rt_shadow_pipeline: Option<wgpu::ComputePipeline>,
    /// RT projected-shadow denoise (RT_SHADOWS_DESIGN Stage 2): spatial bilateral + temporal EMA
    /// reprojection. Both lazy-compiled together with the ray pipeline on the first soft-shadow
    /// frame (`ensure_rt_denoise_pipelines`) — never compiled while softness stays 0.
    pub(crate) rt_denoise_pipeline: Option<wgpu::ComputePipeline>,
    pub(crate) rt_temporal_pipeline: Option<wgpu::ComputePipeline>,
    // (GLSL PT primary/bounce pipelines REMOVED 2026-07-14 — WGSL PT is the sole route)
    // FXAA — lazy-compiled on first use
    #[allow(dead_code)]
    pub(crate) fxaa_pipeline: Option<wgpu::ComputePipeline>,
    // Primary GPU bake — lazy-compiled on first use
    pub(crate) primary_bake_pipeline: Option<wgpu::ComputePipeline>,
    // STAGE-2 GPU CLASSIFY: `classify_bricks` entry of volume_bake.wgsl (bake BGL). Compiled
    // alongside the bake pipeline (background hot-swap) with a lazy `ensure_*` fallback.
    pub(crate) classify_bricks_pipeline: Option<wgpu::ComputePipeline>,
    // STAGE-3 GPU FINALIZE: the four `classify_finalize_*` entries (same module/BGL), compiled
    // and hot-swapped as a set with `classify_bricks`. `None` → the CPU finalize path runs.
    pub(crate) classify_finalize_pipelines: Option<ClassifyFinalizePipelines>,
    /// GPU CURVATURE (`GPU_CURVATURE_KERNEL_PLAN.md` P1): the `curvature_eval` entry of
    /// volume_bake.wgsl (same bake BGL). Third consumer of `eval_scene_at` — it evaluates the
    /// material node graph's SdfCurvature signal against the ANALYTICAL field, so the texture
    /// rung and (P2) the vertex rung cannot diverge. Lazy-compiled on the first curvature bake;
    /// `None` → the CPU `sdf_curvature_masks` path runs unchanged.
    pub(crate) curvature_eval_pipeline: Option<wgpu::ComputePipeline>,
    /// O1 — tiny pass: `counters[0]` (live vert count) → `dispatch_workgroups_indirect` args.
    /// Avoids dispatching over allocated `vert_cap` (often 10–137× the real surface).
    pub(crate) curv_dispatch_args_pipeline: Option<(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    /// O1 pooled — sum `tile_counters[args_base..+n]` → body-level `[vert_count, index_count]`.
    pub(crate) curv_sum_tile_counters_pipeline:
        Option<(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    // Brick fill — lazy-compiled on first use
    pub(crate) brick_fill_pipeline: Option<wgpu::ComputePipeline>,
    // Temporal reproject — lazy-compiled on first use
    pub(crate) temporal_reproject_pipeline: Option<wgpu::ComputePipeline>,
    // PT spatial denoise — lazy-compiled on first use
    pub(crate) pt_denoise_pipeline: Option<wgpu::ComputePipeline>,
    // PT temporal reprojection (P3.5) — lazy-compiled on first camera move in PT mode
    pub(crate) pt_reproject_pipeline: Option<wgpu::ComputePipeline>,
    /// Section I: warp ReSTIR reservoirs under camera motion (lazy).
    pub(crate) pt_restir_reproject_pipeline: Option<wgpu::ComputePipeline>,
    // Section B: depth-aware half→full PT lighting upscale
    pub(crate) pt_upscale_pipeline: Option<wgpu::ComputePipeline>,
    // SSAO (render/src/ssao.rs: main + blur_h + blur_v, one shared BGL) —
    // background-compiled, `ensure_ssao_pipelines` lazy fallback.
    pub(crate) ssao: Option<render::ssao::SsaoPipelines>,
    // Screen-space GI (graphics/src/ssgi.rs: gather + blur_h + blur_v, one shared
    // BGL) — `ensure_ssgi_pipelines` lazy fallback. SSGI ships default-OFF, so the
    // first-enable compile hitch is acceptable and it is not on the warmup path.
    pub(crate) ssgi: Option<graphics::ssgi::SsgiPipelines>,
    // Blend/composite — lazy-compiled on first use
    pub(crate) blend_pipeline: Option<wgpu::ComputePipeline>,
    // Pipeline layout for raymarch family
    pub(crate) raymarch_pipeline_layout: wgpu::PipelineLayout,
    /// Disk-backed GPU pipeline cache — speeds up subsequent launches by reusing compiled kernels.
    /// Wrapped in Arc so it can be shared with background compilation threads.
    pub(crate) pipeline_cache: std::sync::Arc<Option<wgpu::PipelineCache>>,
    /// Background WGSL pipeline compilation (polled each frame; hot-swaps each pipeline as it finishes).
    pub(crate) sdf_compile_rx: Option<std::sync::mpsc::Receiver<PipelineReady>>,
    /// On-demand DF-full compile (first Pbr/Lit use when not eagerly warmed). Polled in
    /// `poll_sdf_compile` — never blocks the frame thread.
    pub(crate) df_full_compile_rx: Option<std::sync::mpsc::Receiver<wgpu::ComputePipeline>>,
    /// True once a DF-full background compile has been started (or the pipeline is Some).
    pub(crate) df_full_compile_started: bool,
    /// True once Raymarch + Raymarch-lite WGSL pipelines are ready.
    pub(crate) wgsl_pipelines_ready: bool,
    /// Safety flag: set during heavy shader compilation to prevent GPU contention.
    pub(crate) compiling_heavy_shader: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Last shader compilation error (node graph produced invalid WGSL). None = healthy.
    pub(crate) shader_compile_error: Option<String>,
}

/// All bind group layouts (BGLs) and bind groups (BGs) for compute pipelines.
/// Invalidated by: buffer/texture recreation (any buffer resize invalidates all BGs referencing it).
/// Rebuilt by: lazy recreation in viewport_render.rs when BG is None + textures exist.
pub(crate) struct BindGroupCache {
    // Raymarch (main)
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) bind_group: Option<wgpu::BindGroup>,
    // (volume_preview_bgl/bg + atlas_tex_gen REMOVED 2026-07-13 — SDF volume-raymarch display stripped)
    // Sculpt voxel raymarch (Plan B1). Rebuilt every frame — the
    // sparse atlas index/pool buffers can be recreated between frames (grow/gc/grid-swap).
    pub(crate) sculpt_raymarch_bgl: wgpu::BindGroupLayout,
    pub(crate) sculpt_raymarch_bg: Option<wgpu::BindGroup>,
    // Tonemap
    pub(crate) tonemap_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) tonemap_bind_group: Option<wgpu::BindGroup>,
    // Bloom
    pub(crate) bloom_extract_bgl: wgpu::BindGroupLayout,
    pub(crate) bloom_blur_bgl: wgpu::BindGroupLayout,
    pub(crate) bloom_extract_bg: Option<wgpu::BindGroup>,
    pub(crate) bloom_blur_h_bg: Option<wgpu::BindGroup>,
    pub(crate) bloom_blur_v_bg: Option<wgpu::BindGroup>,
    // Deferred
    pub(crate) deferred_bgl: wgpu::BindGroupLayout,
    pub(crate) deferred_bg: Option<wgpu::BindGroup>,
    // SSR
    pub(crate) ssr_bgl: wgpu::BindGroupLayout,
    pub(crate) ssr_bg: Option<wgpu::BindGroup>,
    // Fog
    pub(crate) fog_bgl: wgpu::BindGroupLayout,
    pub(crate) fog_bg: Option<wgpu::BindGroup>,
    // Upsample
    pub(crate) upsample_bgl: wgpu::BindGroupLayout,
    pub(crate) upsample_bg: Option<wgpu::BindGroup>,
    // Path trace
    pub(crate) pt_bgl: wgpu::BindGroupLayout,
    pub(crate) pt_bg: Option<wgpu::BindGroup>,
    /// pt_scene_generation the current pt_bg was built against — mismatch = stale
    /// buffer refs, rebuild (generation counters over timing tricks).
    pub(crate) pt_bg_generation: u64,
    /// Material texture-array generation when `pt_bg` was last built (rebuild on grow/upload).
    pub(crate) pt_bg_mat_tex_gen: u64,
    // RT projected shadows (RT_SHADOWS_DESIGN Stage 1). BGL lazy-created with the pipeline
    // (`ensure_rt_shadow_pipeline`) from `rt_shadow_bgl_entries()` — single source. The bind
    // group binds the PT scene SSBOs + G-buffer + vis output; rebuilt on a pt_scene_generation
    // mismatch (buffer realloc) OR when None'd by the resize block (vis/gbuf recreate).
    pub(crate) rt_shadow_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) rt_shadow_bg: Option<wgpu::BindGroup>,
    /// pt_scene_generation the current rt_shadow_bg was built against — mismatch = rebuild.
    pub(crate) rt_shadow_bg_generation: u64,
    /// RT projected-shadow denoise BGLs + bind groups (RT_SHADOWS_DESIGN Stage 2). Reference only
    /// full-res textures + the param uniforms (never the PT SSBOs), so they are invalidated ONLY by
    /// the resize block (None'd there) and rebuilt on demand — no generation counter needed.
    pub(crate) rt_denoise_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) rt_denoise_bg: Option<wgpu::BindGroup>,
    /// P3.2: spatial denoise bind group for pack1 (lights 4..7).
    pub(crate) rt_denoise_bg_hi: Option<wgpu::BindGroup>,
    pub(crate) rt_temporal_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) rt_temporal_bg: Option<wgpu::BindGroup>,
    /// P3.2: temporal bind group for pack1.
    pub(crate) rt_temporal_bg_hi: Option<wgpu::BindGroup>,
    // (PT bounce bind group REMOVED 2026-07-14 — split GLSL PT stripped)
    // Bevel preview
    pub(crate) bevel_preview_bgl: wgpu::BindGroupLayout,
    pub(crate) bevel_preview_bg: Option<wgpu::BindGroup>,
    // FXAA
    #[allow(dead_code)]
    pub(crate) fxaa_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) fxaa_bg: Option<wgpu::BindGroup>,
    // Primary bake
    pub(crate) primary_bake_bgl: Option<wgpu::BindGroupLayout>,
    // Brick fill
    pub(crate) brick_fill_bgl: Option<wgpu::BindGroupLayout>,
    // Temporal reproject
    pub(crate) temporal_reproject_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) temporal_reproject_bg: Option<wgpu::BindGroup>,
    // PT denoise (A3: cached per pass topology — invalidate on texture resize only)
    pub(crate) pt_denoise_bgl: Option<wgpu::BindGroupLayout>,
    /// Full-res pass0: src_first → dst_final (odd step_count / classic topology)
    pub(crate) pt_denoise_bg: Option<wgpu::BindGroup>,
    /// Full-res pass0 alt: src_first → temp (even step_count when end-on-hdr)
    pub(crate) pt_denoise_bg_p0_temp: Option<wgpu::BindGroup>,
    /// Full-res odd passes: dst_final → temp
    pub(crate) pt_denoise_bg_ping: Option<wgpu::BindGroup>,
    /// Full-res even passes after 0: temp → dst_final
    pub(crate) pt_denoise_bg_pong: Option<wgpu::BindGroup>,
    // PT temporal reprojection (P3.5). BG invalidated (None) whenever any referenced
    // texture (gbuf_normal / temporal_prev_normal_dist / pt_accum / scratch) is
    // recreated — all recreated together in the viewport resize block.
    pub(crate) pt_reproject_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) pt_reproject_bg: Option<wgpu::BindGroup>,
    /// Half-res reproject bind group (same BGL as full; half accum + half scratch).
    pub(crate) pt_reproject_bg_half: Option<wgpu::BindGroup>,
    /// Section I: restir reproject BGL + bind groups (full + half).
    pub(crate) pt_restir_reproject_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) pt_restir_reproject_bg: Option<wgpu::BindGroup>,
    pub(crate) pt_restir_reproject_bg_half: Option<wgpu::BindGroup>,
    /// Half-res PT bind group (same layout as pt_bgl, bindings 0/27 → half textures).
    pub(crate) pt_bg_half: Option<wgpu::BindGroup>,
    pub(crate) pt_bg_half_generation: u64,
    pub(crate) pt_bg_half_mat_tex_gen: u64,
    /// Half-res denoise pass0 / p0→temp / ping / pong (same topology as full).
    pub(crate) pt_denoise_bg_half: Option<wgpu::BindGroup>,
    pub(crate) pt_denoise_bg_half_p0_temp: Option<wgpu::BindGroup>,
    pub(crate) pt_denoise_bg_half_ping: Option<wgpu::BindGroup>,
    pub(crate) pt_denoise_bg_half_pong: Option<wgpu::BindGroup>,
    // Section B upscale
    pub(crate) pt_upscale_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) pt_upscale_bg: Option<wgpu::BindGroup>,
    pub(crate) pt_upscale_params_buffer: Option<wgpu::Buffer>,
    // Blend/composite
    pub(crate) blend_bgl: Option<wgpu::BindGroupLayout>,
    // Env GGX prefilter (params uniform + env storage rw). The bind group itself is
    // built fresh per dispatch in poll (primary-bake precedent — nothing cached).
    pub(crate) env_prefilter_bgl: Option<wgpu::BindGroupLayout>,
}

/// All GPU storage/uniform buffers.
/// Invalidated by: data size changes (buffer recreated when capacity exceeded).
/// Rebuilt by: caller recreates + invalidates referencing bind groups.
pub(crate) struct BufferPool {
    // Scene data
    pub(crate) uniform_buffer: wgpu::Buffer,
    pub(crate) prim_buffer: wgpu::Buffer,
    pub(crate) tex_buffer: wgpu::Buffer,           // AI texture data (packed RGBA u32)
    pub(crate) bvh_node_buffer: wgpu::Buffer,
    pub(crate) bvh_index_buffer: wgpu::Buffer,
    pub(crate) light_buffer: wgpu::Buffer,          // Multi-light (binding 26)
    pub(crate) light_link_buffer: wgpu::Buffer,     // Per-prim bitmask (binding 41)
    pub(crate) light_cluster_buffer: wgpu::Buffer,  // P3.1 Phase D tile×Z lists (binding 24)
    pub(crate) compositor_buffer: wgpu::Buffer,     // Viewport compositor params (binding 40)
    /// RT projected-shadow ray-pass params (RtShadowParams, binding 25) + the two denoise param
    /// uniforms (RtDenoiseParams, RtTemporalParams). Lazy — created on the first RT-shadow frame
    /// alongside the pipelines; None until the user enables RT shadows (RT_SHADOWS_DESIGN Stage 2/3).
    pub(crate) rt_shadow_params_buffer: Option<wgpu::Buffer>,
    pub(crate) rt_denoise_params_buffer: Option<wgpu::Buffer>,
    pub(crate) rt_temporal_params_buffer: Option<wgpu::Buffer>,
    // Environment + material
    pub(crate) env_buffer: wgpu::Buffer,            // HDR environment map: [7 mips][SH9] (binding 21)
    /// Split-sum BRDF LUT (deferred binding 39). 8-byte placeholder at startup;
    /// replaced ONCE in poll when the startup LUT worker finishes (env_ibl.rs) —
    /// `GpuUniforms.env_lut_res` stays 0 (shader analytic fallback) until then.
    pub(crate) brdf_lut_buffer: wgpu::Buffer,
    /// Env prefilter dispatch params (32-byte uniform, rewritten per budgeted mip dispatch).
    pub(crate) env_prefilter_params_buffer: Option<wgpu::Buffer>,
    pub(crate) matcap_buffer: wgpu::Buffer,         // MatCap texture (binding 42)
    pub(crate) tex_atlas_buffer: wgpu::Buffer,      // Per-prim texture atlas
    pub(crate) tex_meta_buffer: wgpu::Buffer,       // Per-prim texture metadata
    // Bevel preview
    pub(crate) bevel_edge_buffer: wgpu::Buffer,
    pub(crate) bevel_params_buffer: wgpu::Buffer,
    pub(crate) bevel_grid_cells_buffer: wgpu::Buffer,
    pub(crate) bevel_grid_indices_buffer: wgpu::Buffer,
    // Brick pool
    /// 4-byte read-only-bound dummy storage buffer. Consumers: (a) viewport_render brick-pool
    /// slot when the brick pool is off, (b) primary bake @binding(5) brick_index in dense modes
    /// (0/1) — the SparseBricks mode-2 branch is the shader's only reader. Never written.
    pub(crate) brick_pool_dummy_buf: wgpu::Buffer,
    pub(crate) brick_info_buffer: wgpu::Buffer,     // BrickVolumeInfo uniform
    // Sculpt voxel raymarch (Plan B1): 48-byte SculptParams (grid header + body albedo/rough/metal).
    // Persistent — written each frame, never recreated (Frame Ordering Contract: write_buffer only).
    pub(crate) sculpt_params_buffer: wgpu::Buffer,
    // Fog
    pub(crate) fog_prev_camera_buffer: wgpu::Buffer,
    // Temporal
    pub(crate) temporal_march_flags_buffer: wgpu::Buffer, // TemporalMarchFlags uniform (march pass, binding 45)
    pub(crate) temporal_params_buffer: Option<wgpu::Buffer>, // TemporalParams uniform (reproject pass)
    pub(crate) pt_reproj_params_buffer: Option<wgpu::Buffer>, // PtReprojParams uniform (PT reproject, P3.5)
    // Primary bake
    pub(crate) primary_bake_params_buffer: Option<wgpu::Buffer>, // fixed BakeParams size
    // LAB mesh-SDF operand spike (volume_bake.wgsl @binding 5). Owner: BufferPool.
    // `lab_mesh_grid_buffer` = self-describing operand grid (8 header floats + res³
    // distances), created in poll_lab_mesh_sdf (poll phase, Frame Ordering Contract),
    // replaced whole — never written in place. Safe without a generation counter:
    // the primary bake bind group is rebuilt fresh every dispatch.
    // `lab_mesh_grid_dummy` = zeroed 8-float disabled fallback (res = 0.0), created
    // once with the bake pipeline (ensure_lab_mesh_grid_dummy).
    pub(crate) lab_mesh_grid_buffer: Option<wgpu::Buffer>,
    pub(crate) lab_mesh_grid_dummy: Option<wgpu::Buffer>,
    // LAB poly→SDF exact-silhouette ghost (prim_outline_entry.wgsl bindings 5/6). Grow-only,
    // written from SerializeResult.mesh_{grid,start}_bytes at the full-serialize site whenever
    // the outline is active. Parallel to the prim buffer's slots; rebuilt every outline dispatch
    // bind group. Empty/all-sentinel when no MeshVolume is selected.
    pub(crate) outline_mesh_grid_buffer: Option<wgpu::Buffer>,
    pub(crate) outline_mesh_start_buffer: Option<wgpu::Buffer>,
    /// CurveExtrude outline: profile buffer + per-slot starts (prim_outline bindings 7/8).
    pub(crate) outline_clip_curves_buffer: Option<wgpu::Buffer>,
    pub(crate) outline_clip_start_buffer: Option<wgpu::Buffer>,
    /// T1 live-march image displace atlas (raymarch binding 7 / outline binding 15).
    /// Packed by `serialize_from_graph` (`height_map_bytes`). Dummy 4-byte when empty.
    pub(crate) height_map_buffer: Option<wgpu::Buffer>,
    // PT denoise
    pub(crate) pt_denoise_params_buffer: Option<wgpu::Buffer>,
    // Blend/composite
    pub(crate) blend_params_buffer: Option<wgpu::Buffer>,
    // --- Triangle path tracer scene (Phase 2, bindings 42-47) ---
    // Owner: BufferPool. Written ONLY in poll_background_tasks (update_pt_accel —
    // Frame Ordering Contract). Grow-only reallocation; every realloc bumps
    // `pt_scene_generation`, and viewport_render invalidates `pt_bg` on mismatch
    // (generation counters over timing tricks). Created lazily on the first PT
    // frame; None until then.
    pub(crate) pt_tlas_buffer: Option<wgpu::Buffer>,      // 42: TLAS BvhNodes
    pub(crate) pt_instance_buffer: Option<wgpu::Buffer>,  // 43: PtInstanceGpu
    pub(crate) pt_blas_buffer: Option<wgpu::Buffer>,      // 44: concatenated BLAS BvhNodes
    pub(crate) pt_tri_buffer: Option<wgpu::Buffer>,       // 45: PtTri
    pub(crate) pt_vert_buffer: Option<wgpu::Buffer>,      // 46: PtVertex
    pub(crate) pt_meta_buffer: Option<wgpu::Buffer>,      // 47: counts uniform (PtMetaGpu)
    pub(crate) pt_material_buffer: Option<wgpu::Buffer>,  // 48: RasterMaterial per instance
    /// Phase A adaptive stop: two u32 atomics [noisy_pixels, total_pixels] (8 bytes, padded 16).
    pub(crate) pt_stats_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_denoise_pass_params_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_restir_reproj_params_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_moments_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_half_moments_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_reproj_moments_buffer: Option<wgpu::Buffer>,
    pub(crate) pt_half_reproj_moments_buffer: Option<wgpu::Buffer>,
    /// Staging for async readback of pt_stats (MAP_READ).
    pub(crate) pt_stats_staging: Option<wgpu::Buffer>,
    /// True while a map_async on pt_stats_staging is outstanding.
    /// While true, do NOT copy into staging (mapped / map-pending buffer).
    pub(crate) pt_stats_map_pending: std::sync::atomic::AtomicBool,
    /// A1′: map_async completion cell (Ok/Err/Pending). Set when a map is started;
    /// drained by `try_complete_pt_stats_readback` without `Maintain::Wait`.
    pub(crate) pt_stats_map_ready: Option<std::sync::Arc<map_state::MapState>>,
    /// Last read noisy/total fraction bits (f32). Updated when map completes (next frame).
    pub(crate) pt_noisy_fraction_bits: std::sync::atomic::AtomicU32,
    /// Section K: env luminance CDF for PT importance sampling (binding 54).
    /// Always present (dummy empty header when no HDRI). Rebuilt on env install/clear.
    pub(crate) pt_env_cdf_buffer: Option<wgpu::Buffer>,
    /// Bumped on ANY pt_* buffer (re)creation; `pt_bg` caches the generation it
    /// was built against and rebuilds on mismatch.
    pub(crate) pt_scene_generation: u64,
}

/// Single access point for all GPU operations. Replaces the old dual
/// `wgpu_render_state` + `GpuState` pattern.
pub(crate) struct GpuCtx {
    pub(crate) native_rt: Option<app_gpu::native_rt::NativeRtRenderer>,
    pub(crate) native_rt_geometry_epoch: u64,
    pub(crate) native_rt_active: bool,
    /// Latched PT interaction kernel: `true` = the half-res path dispatches the interactive
    /// (ReSTIR) kernel, `false` = the full kernel at the interaction budget (the interactive
    /// warmup stage is not installed, or landed mid-episode). Flips only outside an interaction
    /// episode/cooldown, like `native_rt_active` (`app_poll::latched_pt_choice`).
    pub(crate) pt_interactive_active: bool,
    pub(crate) native_rt_status: app_gpu::native_rt::NativeRtStatus,
    // Device + queue — cloned Arcs from render_state for direct field access
    pub(crate) device: std::sync::Arc<wgpu::Device>,
    pub(crate) queue: std::sync::Arc<wgpu::Queue>,
    // Full eframe render state (needed for egui texture registration + adapter info)
    pub(crate) render_state: eframe::egui_wgpu::RenderState,
    // --- Sub-structs ---
    pub(crate) pipelines: PipelineRegistry,
    /// PR-4b: 8×8 sculpt crop-pick resources (own pipeline/textures/staging — never
    /// `picking_staging`). Created once in the poll phase (Frame Ordering Contract).
    pub(crate) sculpt_pick: Option<crate::poly::sculpt_pick_gpu::SculptPickGpu>,
    pub(crate) bind_groups: BindGroupCache,
    pub(crate) buffers: BufferPool,
    // --- GPU volume (3D texture + metadata, not a simple buffer) ---
    pub(crate) gpu_volume: SdfGpuVolume,
    pub(crate) bvh_node_count: u32,
    // --- Textures ---
    pub(crate) texture: Option<wgpu::Texture>, pub(crate) texture_size: (u32,u32),
    pub(crate) texture_id: Option<egui::TextureId>,
    /// View A display cache while split (main `texture` holds the last rendered pane, usually B).
    pub(crate) split_pane_texture: Option<wgpu::Texture>,
    pub(crate) split_pane_texture_id: Option<egui::TextureId>,
    pub(crate) split_pane_size: (u32, u32),
    pub(crate) hdr_texture: Option<wgpu::Texture>,
    pub(crate) bloom_a: Option<wgpu::Texture>, pub(crate) bloom_b: Option<wgpu::Texture>,
    pub(crate) gbuf_normal: Option<wgpu::Texture>,
    pub(crate) gbuf_material: Option<wgpu::Texture>,
    pub(crate) gbuf_albedo: Option<wgpu::Texture>,
    pub(crate) ssr_texture: Option<wgpu::Texture>,
    pub(crate) fog_texture: Option<wgpu::Texture>,
    pub(crate) fog_prev_texture: Option<wgpu::Texture>,
    pub(crate) upsample_texture: Option<wgpu::Texture>,
    pub(crate) pt_accum_texture: Option<wgpu::Texture>,
    pub(crate) pt_accum_output: Option<wgpu::Texture>,
    /// Progressive PBR refine (PROGRESSIVE_PBR_PLAN.md Phase 1): running mean of
    /// composited HDR frames. `in` is read by pass_tonemap (binding 63), `out` is
    /// storage-written (binding 64), then copied out→in after tonemap — the
    /// pt_accum copy-chain pattern.
    /// E1: 1×1 dummies until first accumulate frame (saves ~full-res×2 RGBA16F when
    /// Progressive Refine is off); upgraded via `ensure_pbr_accum_full`.
    pub(crate) pbr_accum_in: Option<wgpu::Texture>,
    pub(crate) pbr_accum_out: Option<wgpu::Texture>,
    /// Live edge size of `pbr_accum_*` (1×1 dummy or full viewport).
    pub(crate) pbr_accum_size: (u32, u32),
    // ── Auto reflection probe (PROGRESSIVE_PBR_PLAN.md Phase 3, probe_bake.rs) ──
    /// RGBA16F cube (6 layers, full mip chain) at `probe_face_size`² per face.
    /// Mip-0 faces are captured from
    /// probe-camera frames rendered through the REAL raster+deferred pipeline (one
    /// face per idle refine frame); mips are 2×2 box-reduced. Persistent (viewport-
    /// size independent) — recreated ONLY when the resolution setting changes.
    pub(crate) probe_cube: Option<wgpu::Texture>,
    /// Cube face edge the live `probe_cube` was created at, in texels. Compared against
    /// `RenderSettings::probe_face_size` in poll; a mismatch drops the cube and nulls
    /// `ssr_bg`, which re-enters the texture-creation block (viewport_render.rs:881) and
    /// rebuilds both at the new size. 0 = no cube yet.
    pub(crate) probe_face_size: u32,
    pub(crate) probe_sampler: Option<wgpu::Sampler>,
    /// `ProbeParams` uniform for pass_ssr (binding 67): center+enabled, extent+mips.
    pub(crate) probe_params_buffer: Option<wgpu::Buffer>,
    /// `ProbeCaptureParams` uniform for probe_capture (binding 2).
    pub(crate) probe_capture_buffer: Option<wgpu::Buffer>,
    pub(crate) probe_capture_pipeline: Option<wgpu::ComputePipeline>,
    pub(crate) probe_capture_bgl: Option<wgpu::BindGroupLayout>,
    pub(crate) probe_mip_pipeline: Option<wgpu::ComputePipeline>,
    pub(crate) probe_mip_bgl: Option<wgpu::BindGroupLayout>,
    /// Bake scheduler: next face to capture (0..5; 6 = round complete/idle).
    pub(crate) probe_next_face: u32,
    /// True once a round has been captured AND passed the validity verdict
    /// (`probe_bake::PROBE_MIN_FREE_FRACTION`) — drives the `enabled` flag in
    /// ProbeParams (SSR ignores the probe until then).
    pub(crate) probe_baked: bool,
    /// The last judged round FAILED validity (eye inside geometry) — the probe is
    /// withheld and shading stays on the HDRI IBL. UI status only.
    pub(crate) probe_rejected: bool,
    /// Last measured free-space fraction of the probe eye (NaN = never measured).
    pub(crate) probe_free_fraction: f32,
    /// Bake-round generation: bumped when a round starts, the probe is toggled off,
    /// or the cube is recreated. A verdict judges only its own round's content.
    pub(crate) probe_round_gen: u32,
    /// `probe_round_gen` of the round whose verdict is in flight.
    pub(crate) probe_verdict_round: u32,
    /// 16-byte MAP_READ copy of `probe_sh[9]` (strength, free fraction, 0, valid).
    pub(crate) probe_verdict_staging: Option<wgpu::Buffer>,
    /// Pending `map_async` result for `probe_verdict_staging` (GPU Safety rule).
    pub(crate) probe_verdict_rx:
        Option<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    /// Refine signature at the start of the current bake round — a sig change
    /// (content/lighting/camera-quality edit) schedules a fresh round.
    pub(crate) probe_baked_sig: u64,
    pub(crate) probe_center: [f32; 3],
    pub(crate) probe_extent: [f32; 3],
    /// Auto LIGHT probe (Phase 3b): SH9 irradiance projected from the baked probe
    /// cube's 8×8 mip. 10×vec4 storage buffer — [0..8] RGB coefficients (GPU-written
    /// by `probe_sh_project`), [9] = (strength — CPU-written each frame, free fraction —
    /// GPU-written, 0, valid — CPU-written by the validity verdict). Deferred binding 51.
    /// Created EAGERLY (zeroed ⇒ valid=0 ⇒ shader no-op) so deferred_bg can always bind it.
    pub(crate) probe_sh_buffer: Option<wgpu::Buffer>,
    pub(crate) probe_sh_pipeline: Option<wgpu::ComputePipeline>,
    pub(crate) probe_sh_bgl: Option<wgpu::BindGroupLayout>,
    /// Last strength written to `probe_sh_buffer[9].x` (A3 write-on-change).
    /// `NaN` = never written → first frame always uploads.
    pub(crate) probe_sh_strength_written: f32,
    /// Bumped whenever `probe_cube` is (re)created. Probe view/BG caches key on this
    /// (PBR_REFINE_HARDENING_PLAN Phase B3) so a face-size change rebuilds once.
    pub(crate) probe_cube_generation: u64,
    /// Generation the B3 cache was built against (`0` = empty).
    pub(crate) probe_view_cache_gen: u64,
    /// HDR size the capture bind groups were built against (resize → rebuild BGs only).
    pub(crate) probe_capture_hdr_size: (u32, u32),
    /// Cached per-face mip-0 D2 views into `probe_cube` (layer = face).
    pub(crate) probe_face_mip0: Option<[wgpu::TextureView; 6]>,
    /// Cached `probe_capture` bind groups (one per face; bind HDR + face mip0 + params).
    pub(crate) probe_capture_bgs: Option<[wgpu::BindGroup; 6]>,
    /// Cached cross-face mip reduce: src cube view per level (mip-1 → mip).
    pub(crate) probe_mip_src_views: Vec<wgpu::TextureView>,
    /// Cached cross-face mip reduce: dst D2Array view per level.
    pub(crate) probe_mip_dst_views: Vec<wgpu::TextureView>,
    /// Cached `probe_mip_reduce_cube` bind groups (one per mip ≥ 1).
    pub(crate) probe_mip_bgs: Vec<wgpu::BindGroup>,
    /// Cached SH projection source view (8×8 mip as 6-layer D2Array).
    pub(crate) probe_sh_view: Option<wgpu::TextureView>,
    /// Cached `probe_sh_project` bind group.
    pub(crate) probe_sh_bg: Option<wgpu::BindGroup>,
    /// CPU copy of the last uploaded GpuUniforms — the probe mini-frame patches its
    /// camera into this instead of rebuilding the whole per-frame config.
    pub(crate) last_gu: Option<graphics::GpuUniforms>,
    /// P3.5 PT reprojection scratch: the reproject pass storage-writes here, then the
    /// result is copied into pt_accum (compute can't read+write one texture in place).
    /// Recreated with pt_accum in the resize block.
    pub(crate) pt_reproj_scratch: Option<wgpu::Texture>,
    /// Section M: full-res à-trous denoise ping-pong (STORAGE+TEXTURE).
    pub(crate) pt_denoise_temp: Option<wgpu::Texture>,
    /// Section B: half-res PT lighting (25% pixels while camera moves).
    /// Recreated with full-res textures in the resize block. Size = ceil(w/2)×ceil(h/2).
    pub(crate) pt_half_hdr: Option<wgpu::Texture>,
    pub(crate) pt_half_accum: Option<wgpu::Texture>,
    pub(crate) pt_half_accum_output: Option<wgpu::Texture>,
    pub(crate) pt_half_denoise_temp: Option<wgpu::Texture>,
    /// Section C: half-res reproject scratch (storage write → copy into half_accum).
    pub(crate) pt_half_reproj_scratch: Option<wgpu::Texture>,
    pub(crate) pt_half_size: (u32, u32),
    /// Section G: ReSTIR DI reservoirs (rgba32f). `out` is storage-written by PT; copied
    /// into `prev` after dispatch for temporal/spatial reuse next frame.
    /// Full-res pair matches HDR; half-res pair matches `pt_half_size`.
    pub(crate) pt_restir_prev: Option<wgpu::Texture>,
    pub(crate) pt_restir_out: Option<wgpu::Texture>,
    pub(crate) pt_half_restir_prev: Option<wgpu::Texture>,
    pub(crate) pt_half_restir_out: Option<wgpu::Texture>,
    /// Section R: temporal 1-bounce GI (rgba16f, history in alpha).
    pub(crate) pt_gi_prev: Option<wgpu::Texture>,
    pub(crate) pt_gi_out: Option<wgpu::Texture>,
    pub(crate) pt_half_gi_prev: Option<wgpu::Texture>,
    pub(crate) pt_half_gi_out: Option<wgpu::Texture>,
    pub(crate) gbuf_surface_id: Option<wgpu::Texture>,
    pub(crate) gbuf_motion: Option<wgpu::Texture>,
    pub(crate) gbuf_curvature: Option<wgpu::Texture>,
    pub(crate) gbuf_emission: Option<wgpu::Texture>,
    pub(crate) raster_depth_texture: Option<wgpu::Texture>,
    pub(crate) _raster_depth_dummy: wgpu::Texture,
    /// RT projected shadows visibility (RT_SHADOWS_DESIGN Stage 1): full-res RGBA16F, one
    /// channel per point/spot light. The rt_shadow pass storage-writes it; the deferred pass
    /// samples it at binding 40. Recreated ONLY in the viewport resize block (together with the
    /// G-buffer it aligns with + the deferred_bg / rt_shadow_bg that reference it — so no cached
    /// view can dangle). Allocated regardless of the toggle so the deferred layout is stable;
    /// the deferred shader reads it only when `config.rt_shadows_on`. STORAGE + TEXTURE binding.
    pub(crate) rt_shadow_vis: Option<wgpu::Texture>,
    /// P2.2: second RT pack (lights 4..7). Full-res RGBA16F; always allocated with pack0.
    pub(crate) rt_shadow_vis_hi: Option<wgpu::Texture>,
    /// RT projected-shadow denoise scratch (RT_SHADOWS_DESIGN Stage 2). `rt_shadow_denoised` (B) =
    /// the spatial-bilateral output; `rt_shadow_history` (C) = the previous frame's accumulated
    /// visibility for temporal reprojection (COPY_DST — the final result is copied into it each
    /// frame). E1: 1×1 dummies until first soft-penumbra denoise frame; upgraded via
    /// `ensure_rt_denoise_full`.
    pub(crate) rt_shadow_denoised: Option<wgpu::Texture>,
    pub(crate) rt_shadow_history: Option<wgpu::Texture>,
    /// P3.2: pack1 denoise scratch + history (same Stage-2 chain as pack0).
    pub(crate) rt_shadow_denoised_hi: Option<wgpu::Texture>,
    pub(crate) rt_shadow_history_hi: Option<wgpu::Texture>,
    /// Live edge size of the RT denoise/history pairs.
    pub(crate) rt_denoise_size: (u32, u32),
    /// SSAO half-res textures + params + bind groups (render/src/ssao.rs).
    /// Recreated ONLY in the viewport resize block, together with the G-buffer
    /// it reads and the deferred_bg that samples its output (binding 38) — so
    /// none of the cached references can ever dangle. White-filled on creation
    /// and on toggle-off (deferred then reads 1.0 — the pre-SSAO behavior).
    pub(crate) ssao_res: Option<render::ssao::SsaoResources>,
    /// Half-res SSGI ping-pong + params. Recreated in the SAME viewport resize
    /// block as the G-buffer it reads and the tonemap bind group that samples its
    /// output, so no cached reference can dangle. Zero-filled on creation and on
    /// every frame the pass does not dispatch (the composite adds it
    /// unconditionally — zero is the additive identity).
    pub(crate) ssgi_res: Option<graphics::ssgi::SsgiResources>,
    /// 1³ R32Float storage texture dummy — bound as bake output_tex when the
    /// resident volume doesn't match the bake body (writes are discarded).
    pub(crate) bake_tex_dummy: wgpu::Texture,
    /// Volume Atlas — single 3D texture that held ALL bodies for the (now stripped)
    /// SDF volume-raymarch display. NO LONGER CREATED (2026-07-13): stays `None` for the
    /// app's lifetime — every consumer is `Option`-tolerant (bake dispatch falls back to
    /// pad=0 / dummy texture, exactly the legacy no-atlas path). Field kept so the bake
    /// plumbing compiles unchanged; delete with the remaining atlas code in a later slice.
    pub(crate) volume_atlas: Option<graphics::volume_atlas::VolumeAtlas>,
    pub(crate) temporal_prev_normal_dist: Option<wgpu::Texture>,
    pub(crate) temporal_validity_mask: Option<wgpu::Texture>,
    pub(crate) temporal_dummy_mask: wgpu::Texture,
    // --- Blend/overlay textures (lazy-allocated) ---
    pub(crate) overlay_texture: Option<wgpu::Texture>,
    pub(crate) overlay_view: Option<wgpu::TextureView>,
    pub(crate) blend_temp_texture: Option<wgpu::Texture>,
    pub(crate) blend_temp_view: Option<wgpu::TextureView>,
    // --- Misc state ---
    pub(crate) viewport_size: (u32, u32),
    pub(crate) raster: render::RasterPipeline,
    pub(crate) shadows: render::ShadowMapSystem,
    pub(crate) picking: render::PickingPipeline,
    /// Single-flight depth-texel readback for world-space overlay markers (light gizmos).
    /// See `render::depth_probe` — the CPU raycast it replaced was VIEWPORT_PERF item A.
    pub(crate) light_depth_probe: render::depth_probe::DepthProbe,
    /// Depth-tested overlay lines (edit wireframe / object wireframe). Draws into the
    /// composited LDR texture with `raster.depth_view` attached read-only, so the hardware
    /// hides edges behind nearer surfaces. Owns its pooled instance buffer + params uniform;
    /// its bind group borrows `raster.camera_buffer` (created once, never recreated).
    pub(crate) line_overlay: render::LineOverlayPipeline,
    /// Depth-tested selected-face fill (Blender Overlay parity). See face_overlay.rs.
    pub(crate) face_overlay: render::FaceOverlayPipeline,
    pub(crate) gpu_ts: Option<gpu_timestamps::GpuTimestamps>,
    pub(crate) bake_pending: Vec<PendingBake>,
    /// STAGE-2 GPU CLASSIFY: in-flight `classify_bricks` dispatches awaiting verdict readback.
    pub(crate) classify_gpu_pending: Vec<PendingGpuClassify>,
    pub(crate) brick_fill_pending: Vec<PendingBrickFill>,
    pub(crate) temporal_prev_cam: [f32; 16],
    pub(crate) temporal_frame_valid: bool,
    /// Whether `rt_shadow_history` (C) holds a valid previous accumulated frame the RT temporal
    /// pass may blend (RT_SHADOWS_DESIGN Stage 2). Set false on resize, on RT-off, and whenever the
    /// RT chain is skipped (interaction) — so the resume/first frame uses the current sample only
    /// (no stale-history pop), then re-accumulates. Set true after each successful temporal write.
    pub(crate) rt_shadow_history_valid: bool,
    /// P3.2: pack1 temporal history valid (independent of pack0 — first soft frame is current-only).
    pub(crate) rt_shadow_history_valid_hi: bool,
    /// GPU Surface-Nets mesher (sculpt Phase 1) — lazily created compute pipelines. Persisted here
    /// (single owner, per GPU-resource ownership rule) so live meshing doesn't recompile per frame.
    pub(crate) voxel_mesh: Option<crate::voxel_gpu_mesh::VoxelMeshGpu>,
    /// In-flight async GPU mesh batch (chunk index + mesh-cache id, + the readback). At most one at
    /// a time: dispatched one frame, collected the next (the CPU never blocks on the GPU).
    pub(crate) voxel_mesh_inflight: Option<(Vec<(usize, core_math::scene_graph::MeshCacheId)>, crate::voxel_gpu_mesh::VoxelMeshInflight)>,
    /// Persistent res³ density buffer for the GPU mesher (Phase 3c). Filled once via `to_dense`, then
    /// patched per frame by the dirty Z-band — NOT re-uploaded whole every dispatch (the 512³ sparse
    /// trap). Recreated on resolution change; cleared (`= None`) on a grid swap so it re-fills.
    pub(crate) voxel_mesh_density: Option<wgpu::Buffer>,
    pub(crate) voxel_mesh_density_res: u32,
    /// Persistent GPU brick atlas (INDEX + POOL) for the SPARSE mesh path (res ≥ 256, Phase 4b-2). The
    /// live mesh dispatch reads this instead of a res³ buffer — the 1024³ enabler (a 1024³ dense buffer
    /// is 4 GB). Synced per-brick from the CPU dirty-tracking before each dispatch; `= None` on grid swap
    /// so it re-uploads from the new grid. Dense grids (< 256) use `voxel_mesh_density` instead.
    pub(crate) voxel_mesh_atlas: Option<crate::voxel_gpu_mesh::VoxelAtlasGpu>,
    /// GPU-RESIDENT per-chunk mesh buffers (Phase 6), one slot per chunk linear index (parallel to
    /// `VoxelObject.chunks`). When `gpu_resident_mesh` is on, the mesher writes these directly and the
    /// rasteriser draws them via `draw_indexed_indirect` — NO per-chunk CPU readback (removes the
    /// ~5000 chunks/sec ceiling that caused the fast-stroke tearing). Empty = CPU readback path active.
    pub(crate) voxel_chunk_gpu: Vec<Option<crate::voxel_gpu_mesh::VoxelChunkGpu>>,
    /// Generation of the MESH DENSITY SOURCE buffers (the dense `voxel_mesh_density` OR the sparse
    /// `voxel_mesh_atlas` index/pool). Bumped whenever any of those buffers is RECREATED (resolution
    /// change, grid swap, sparse atlas full-upload / grow). Passed into `dispatch_chunks_resident` as
    /// `src_gen` so a realloc invalidates EVERY cached per-chunk bind group (the BG binds these buffers at
    /// bindings 0/6/7 — a dangling reference would otherwise survive in the cache). Incremental per-brick
    /// uploads (write_buffer into the SAME buffer) do NOT bump it — the buffer identity is unchanged.
    pub(crate) voxel_mesh_src_gen: u32,
    /// Deferred pool-grow request (the shared vertex/index/indirect pool — POOL fix). The dispatch path
    /// (input-handler time) sets this to the slot count it needs when `alloc_slot` runs dry mid-stroke;
    /// `poll_background_tasks` reads + clears it and (re)allocates the pool there — the buffer create MUST
    /// be in poll (Frame Ordering Contract), never in the dispatch. Chunks that couldn't get a slot stay
    /// dirty and retry next frame against the grown pool. 0 = none.
    pub(crate) voxel_pool_grow_to: u32,
    /// G1 (Phase G): per-body GPU-resident SDF mesh slots (QEF Dual Contouring output), keyed by body
    /// NodeId (bodies are dozens). Each slot owns its verts/idx/indirect buffers; G2 draws them via
    /// `draw_indexed_indirect`. Allocated ONLY in `poll_background_tasks` (Frame Ordering). Dormant
    /// (empty) until the resident populate + draw is wired (G2).
    #[allow(dead_code)]
    pub(crate) sdf_body_mesh: std::collections::HashMap<core_math::scene_graph::NodeId, crate::sdf_gpu_mesh::SdfBodyMeshSlot>,
    /// Stage B-1 (Tiled-DC): ONE shared slotted mesh pool for all SPARSE (brick-resident) SDF
    /// bodies — shared verts + idx + per-slot `DrawIndexedIndirect` args + per-slot instance
    /// buffer, drawn in ONE `multi_draw_indexed_indirect` (raster_gbuffer + raster_pick, lockstep).
    /// Mirrors the voxel sculpt's `VoxelMeshGpu` pool. Created lazily on the first sparse mesh
    /// dispatch in `poll_background_tasks` (Frame Ordering Contract); grows/rebuilds only in poll;
    /// `generation` bumps on every buffer recreate / slot relocation. Dense bodies keep their
    /// per-body `SdfBodyMeshSlot` buffers and draws.
    pub(crate) sdf_mesh_pool: Option<crate::sdf_gpu_mesh::SdfMeshPool>,
    /// Phase D1 GPU instance culling (PHASE_D1_INSTANCE_CULL_PLAN.md): compute frustum cull
    /// over the persistent `raster.instance_buffer`, compacting survivors into per-batch
    /// indirect draw args. Created + grown ONLY in `poll_background_tasks` (Frame Ordering
    /// Contract), sized from `draw_stream.len()`; a frame where it isn't ready falls back to
    /// the CPU cull path (fail-closed). Owner of all cull-pass buffers; invalidation via
    /// its `generation` + `raster.instance_buffer_generation` (GPU_INVALIDATION_MATRIX.md).
    pub(crate) raster_cull: Option<render::instance_cull::InstanceCullSystem>,
    /// Phase D2 Hi-Z max-depth pyramid (PHASE_D2_HZB_PLAN.md). Created/resized ONLY in
    /// `poll_background_tasks` (`poll_hzb`, keyed on `raster.depth_size`); built each
    /// frame after the G-buffer pass; consumed by the instance cull's occlusion test.
    /// `valid=false` → cull falls back to frustum-only (fails closed).
    pub(crate) hzb: Option<render::hzb::HzbSystem>,
    /// Phase D4 cluster residency (PHASE_D4_CLUSTER_RASTER_PLAN.md): per-cid DAG
    /// vertex/index/meta buffers for the cluster cut + multi-draw. Entries
    /// added/dropped ONLY in poll (`poll_cluster_bake` upload arm), ≤1 DAG/frame;
    /// `generation` bumps on any change (GPU_INVALIDATION_MATRIX.md).
    pub(crate) cluster_pool: render::cluster_gpu::ClusterGpuPool,
    /// Phase D4 cluster-cut compute (regions/args/counters). Created in poll when
    /// the first DAG goes resident; dispatched in `dispatch_raster_pass`.
    pub(crate) cluster_cut: Option<render::cluster_cut::ClusterCutSystem>,
    /// GPU_CURVATURE_KERNEL_PLAN.md P2 — per-body persistent vertex-curvature buffers, keyed by
    /// body `NodeId`, owned INDEPENDENTLY of `sdf_body_mesh`/`sdf_mesh_pool`'s physical backing
    /// store (see `gpu_curvature.rs`'s module header for why: mirroring the pool's bump/rebuild
    /// lockstep for a second buffer would be its own large, high-risk change; a wholesale rebuild
    /// keyed on the mesh's own generation is provably correct without touching `pool.rs` at all).
    /// Populated by `poll_vertex_curvature` (poll phase — Frame Ordering Contract); consumed by
    /// the G-buffer vertex shader via a group-2 bind, with a 1-element dummy bound for every body
    /// that has no entry here (device-loss audit: every draw site must always have SOMETHING
    /// bound at group 2).
    pub(crate) sdf_body_curvature: std::collections::HashMap<core_math::scene_graph::NodeId, crate::gpu_curvature::CurvVertexSlot>,
    /// MESH curvature pipelines (MESH_CURVATURE_PLAN M4), compiled on first use. Lazy because
    /// building five compute pipelines at startup would cost every user who never adds a
    /// curvature layer.
    pub(crate) mesh_curv_pipelines: Option<crate::gpu_curvature::MeshCurvPipelines>,
    /// Phase 3A / P2-B5 MULTI-TENANT: PERSISTENT generation-counted GPU brick atlases (INDEX
    /// tags + POOL arena), ONE PER BODY, for the sparse SDF bake + mesh path. Written at bake
    /// dispatch (upload_index → mode-2 bake fills the pool) and read by `mesh_into_slot_sparse`
    /// (POLL context throughout — Frame Ordering Contract). Keyed by body NodeId so editing /
    /// selecting between sparse bodies no longer steals the single atlas (the pre-B5 behavior:
    /// every switch escalated the other body to a full re-classify + re-bake). Grow bumps each
    /// atlas's own `generation` (counters over timing tricks). Total VRAM is bounded by
    /// `sdf_atlas_budget_bytes` via LRU eviction (`evict_sdf_atlases_over_budget`) — an evicted
    /// body keeps its drawn mesh (the slot is separate) and self-heals with a full sparse
    /// re-bake on its next edit (the dispatch-side atlas identity check fails → escalate).
    pub(crate) sdf_brick_atlases: std::collections::HashMap<core_math::scene_graph::NodeId, graphics::SdfBrickAtlasGpu>,
    /// LRU stamps for `sdf_brick_atlases` (poll-frame counter at last bake/mesh access).
    pub(crate) sdf_atlas_lru: std::collections::HashMap<core_math::scene_graph::NodeId, u64>,
    /// Total VRAM budget across ALL sparse brick atlases — derived from adapter VRAM at init
    /// (the `VolumeAtlas::new_auto` precedent: a VRAM fraction with a hard cap, never a
    /// works-on-my-machine constant). ~12 fully-resident 512³ bodies at the 2 GiB cap.
    pub(crate) sdf_atlas_budget_bytes: u64,
    /// GPU brush compute pipeline (sculpt Phase 2) — lazily created. The brush density buffer is now a
    /// PER-DISPATCH box buffer (Phase 4: bounded by the brush AABB, not res³), so no persistent buffer.
    pub(crate) voxel_brush: Option<crate::voxel_gpu_mesh::VoxelBrushGpu>,
    /// In-flight async GPU brush batch (sculpt async readback). At most one at a time: stamps are
    /// dispatched in the input handler, the edited slice is collected the next frame in
    /// `poll_background_tasks` (`voxel_brush_collect`) — the CPU never blocks on the ~4ms readback.
    pub(crate) voxel_brush_inflight: Option<crate::voxel_gpu_mesh::VoxelBrushInflight>,
    /// Phase C (depth-raycast): 256-byte staging buffer for the single-pixel readback of `gbuf_normal`
    /// at the cursor (world normal + world_dist in .w) → the voxel sculpt brush position comes from the
    /// rendered surface, not a CPU raycast. Lazily created on first use.
    pub(crate) surface_pick_staging: Option<wgpu::Buffer>,
    /// Reused MAP_READ staging for GPU pool → CPU materialise (full or partial). Avoids create_buffer
    /// hitch every stroke release at 1024³ (surface shell can be tens of MB).
    pub(crate) voxel_pool_rb_staging: Option<(wgpu::Buffer, u64)>,
    /// Phase A.2 (GPU brick-snapshot undo): bounded undo/redo ring of per-stroke GPU POOL-slot snapshots
    /// for the ATLAS sculpt path. Restores are GPU→GPU copies — NO readback on stroke OR on undo.
    pub(crate) voxel_atlas_undo: crate::voxel_gpu_mesh::AtlasUndoRing,
    /// In-progress atlas-stroke snapshot accumulator (first-touch per-brick, pre-scatter). `Some` only
    /// during a GPU-resident stroke; consumed on pen-up into an `AtlasUndoEntry`.
    pub(crate) voxel_atlas_undo_building: Option<crate::voxel_gpu_mesh::AtlasUndoBuilder>,
    /// Material PBR texture maps (albedo, …) — one generation-counted RGBA8 `texture_2d_array`
    /// shared by the raster G-buffer pipeline and the SDF volume-preview pipeline. Lazily created
    /// on first map load (in `poll_background_tasks`, Frame Ordering Contract). `None` until then.
    pub(crate) material_textures: crate::material_textures::MaterialTextureArray,
    /// Session material-bake pipeline cache (WGSL hash → compute pipeline). Lives with the
    /// device so entries never outlive it (unlike a process-static map).
    pub(crate) mat_bake_pipe_cache: crate::gpu_material_bake::MatBakePipeCache,
}

/// Async screenshot state — holds GPU staging buffer + readback channel between frames.
pub(crate) struct PendingScreenshot {
    pub(crate) staging: wgpu::Buffer,
    pub(crate) rx: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bpr: u32,
    /// If set, write a resized PNG here (library thumbnail) instead of a file dialog.
    pub(crate) fixed_path: Option<std::path::PathBuf>,
    /// Square thumb edge length when `fixed_path` is set (default 128).
    pub(crate) thumb_size: u32,
}

/// Multi-AOV GPU readback for Neural Beauty (beauty LDR + gbuf_normal + optional curvature).
/// All textures are Rgba16Float (8 bytes/pixel). Depth is packed in gbuf_normal.w.
pub(crate) struct PendingAiBeautyCapture {
    pub(crate) beauty_staging: wgpu::Buffer,
    pub(crate) normal_staging: wgpu::Buffer,
    pub(crate) curvature_staging: Option<wgpu::Buffer>,
    pub(crate) beauty_rx: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) normal_rx: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    pub(crate) curvature_rx: Option<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Bytes-per-row for f16 RGBA (aligned).
    pub(crate) bpr: u32,
    pub(crate) beauty_ready: Option<Vec<u8>>,
    pub(crate) normal_ready: Option<Vec<u8>>,
    pub(crate) curvature_ready: Option<Vec<u8>>,
}

// ============================================================
// P2-B5 — sparse brick atlas multi-tenancy: LRU eviction
// ============================================================

/// Evict least-recently-used sparse brick atlases until the total VRAM footprint fits
/// `budget_bytes`. `keep` (the body being baked/meshed THIS dispatch) is never evicted — the
/// budget may be exceeded by one body's own atlas, which is the single-tenant floor. Returns the
/// evicted `(body, freed_bytes)` list for the caller's log line.
///
/// An evicted body loses only its GPU brick residency: its mesh slot (drawn geometry) is
/// untouched, and its `BodyCacheEntry.brick_resident` claim now dangles — every consumer checks
/// atlas identity first (`sdf_brick_atlases.get(nid)` → None), so the next EDIT of that body
/// fails the dispatch-side check, escalates, and a full sparse re-bake recreates the atlas.
/// Bodies never touched again cost nothing (their stamp ages to the front of the LRU).
///
/// Free function over the maps (not a `GpuCtx` method) so the policy is unit-testable with a
/// headless device and two small maps — no 100-field GpuCtx construction.
pub(crate) fn evict_sdf_atlases_over_budget(
    atlases: &mut std::collections::HashMap<core_math::scene_graph::NodeId, graphics::SdfBrickAtlasGpu>,
    lru: &mut std::collections::HashMap<core_math::scene_graph::NodeId, u64>,
    keep: core_math::scene_graph::NodeId,
    budget_bytes: u64,
) -> Vec<(core_math::scene_graph::NodeId, u64)> {
    let mut evicted = Vec::new();
    loop {
        let total: u64 = atlases.values().map(|a| a.vram_bytes()).sum();
        if total <= budget_bytes {
            break;
        }
        // Oldest stamp among evictable (non-`keep`) atlases; a missing stamp counts as oldest
        // (never touched — e.g. an orphan left by a deleted body — evicts first).
        let victim = atlases.keys()
            .filter(|&&nid| nid != keep)
            .min_by_key(|&&nid| lru.get(&nid).copied().unwrap_or(0))
            .copied();
        let Some(victim) = victim else { break }; // only `keep` remains — the single-tenant floor
        let freed = atlases.remove(&victim).map(|a| a.vram_bytes()).unwrap_or(0);
        lru.remove(&victim);
        evicted.push((victim, freed));
    }
    evicted
}

/// The multi-tenant atlas VRAM budget — the `VolumeAtlas::new_auto` precedent (a VRAM fraction
/// with a hard cap): 25% of adapter VRAM, capped at 2 GiB (~12 resident 512³ bodies), floor
/// 256 MiB (always at least one full-size body even on tiny adapters / vram_bytes == 0).
pub(crate) fn sdf_atlas_budget_from_vram(vram_bytes: u64) -> u64 {
    ((vram_bytes / 4).min(2 << 30)).max(256 << 20)
}
