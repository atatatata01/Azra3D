# GPU Invalidation Matrix

## Polygon material-node curvature cache

- `GpuCtx.mat_bake_pipe_cache.poly_curvature` owns the background-compiled edge-measurement pipeline, object surface buffers, runtime atlas records, diagnostics, and shared resident graph pipelines. Source meshes and materials remain scene-graph-owned; derived atlas meshes are held in `MeshCache` and never referenced by authored scene nodes.
- Geometry identity is the stable object `NodeId`, source `MeshCacheId`, mesh content stamp and atlas resolution. Rebuilt geometry invalidates the surface binding generation. Material-array recreation and source material signatures invalidate generated output freshness; resident bindings key on array and surface generations. Slider-only graph values reuse the measurement buffer and parameterized graph pipeline.
- Measurement buffers are bounded by a 128 MiB residency budget and the device storage-binding limit. Cache publication/removal invalidates raster structural draws and restarts converged path tracing. Scene replacement removes runtime atlas records; persistence exports only graph-referenced source meshes.
- FrameScheduler Medium work is capped at 64 discovery contexts, two graph preparations, 32 attempts, eight graph dispatches and one geometry build. Shared resident pipeline compilation retains the two-pending/48-entry caps; invalidating a surface releases its resident bind-group references before eviction.

## Resident material and node previews

- `GpuCtx.material_textures` owns the material array, its unorm storage and linear sampled views, sRGB sampled view, sampler, resize pipeline, and GPU-authored slot codecs. Every array replacement recreates both sampled views under the same generation counter. Resident Bit16 bump uses raw byte planes; legacy maps retain sRGB decoding. Resize interpolates reconstructed raw heights, and save/export converts raw planes to the legacy wire encoding. Resident procedural edits write this array directly; CPU pixels are a cache only for imported maps. Save/export explicitly snapshots GPU-authored layers.
- The same owner caches per-slot mip bind groups and codec buffers by array generation. Storage writes use one mip level; resident dispatch regenerates the full chain before sampling. Filtering reconstructs paired values before averaging, then re-encodes the applicable codec. Capacity growth preserves every mip; resolution growth rebuilds them. Imported-map CPU filtering remains unchanged.
- `GpuCtx.mat_bake_pipe_cache.resident` owns structural pipelines, persistent graph/dispatch buffers, and resident bind groups. Compilation is background-only, capped at two pending jobs and 48 entries. Bind groups key on the material-array generation and rebuild after resize/reset/grow.
- `GpuCtx.mat_bake_pipe_cache.thumbnails` owns a separate 64-square array, separate resident pipeline/binding cache, and registered egui native texture IDs. IDs rebuild on thumbnail-array generation changes and are freed when the editor material target changes. Poll dispatch is capped at two visible node updates per frame.
- All resident resource creation/dispatch happens during `poll_graph_bakes`, before UI paint and viewport sampling, through FrameScheduler High tickets. Float changes update buffers, not pipeline structure. No geometry baking or display-path switch is introduced.

Quick-reference for which GPU resources own what, when they go stale, and rebuild policy.

## Sub-Struct Ownership

| Sub-Struct       | Owner    | What It Holds                                    |
|------------------|----------|--------------------------------------------------|
| PipelineRegistry | GpuCtx   | All 22 compute pipelines, pipeline cache, compile state |
| BindGroupCache   | GpuCtx   | All 17 BGLs + 18 BGs                            |
| BufferPool       | GpuCtx   | All 22 GPU buffers (storage + uniform)           |

## Pipelines (PipelineRegistry)

| Pipeline                   | Compiled When               | Invalidated By              | Interaction-Safe |
|----------------------------|-----------------------------|-----------------------------|------------------|
| pipeline (raymarch)        | Background thread / rebuild | Shader source change        | Yes (read-only)  |
| pipeline_lite              | Background thread / rebuild | Shader source change        | Yes              |
| pipeline_march             | Background thread           | Shader source change        | Yes              |
| glsl_bvh_pipeline          | Startup (SPIR-V)            | Never (precompiled .spv)    | Yes              |
| glsl_test_pipeline         | Startup (SPIR-V)            | Never                       | Yes              |
| volume_preview_pipeline    | Background thread / rebuild | Shader source change        | Yes              |
| tonemap_pipeline           | Startup                     | Never                       | Yes              |
| bloom_extract_pipeline     | Startup                     | Never                       | Yes              |
| bloom_blur_h_pipeline      | Startup                     | Never                       | Yes              |
| bloom_blur_v_pipeline      | Startup                     | Never                       | Yes              |
| deferred_pipeline          | Background thread / rebuild | Shader source change        | Yes              |
| deferred_lite_pipeline     | Background thread           | Shader source change        | Yes              |
| ssr_pipeline               | Startup                     | Never                       | Yes              |
| fog_pipeline               | Lazy (first fog frame)      | Shader source change        | No (skip fog)    |
| upsample_pipeline          | Startup                     | Never                       | Yes              |
| bevel_preview_pipeline     | Startup                     | Never                       | Yes              |
| pt_pipeline                | Lazy (first PT frame)       | Shader source change        | No (skip PT)     |
| pt_primary_pipeline        | Startup (GLSL)              | Never                       | Yes              |
| pt_bounce_pipeline         | Startup (GLSL)              | Never                       | Yes              |
| fxaa_pipeline              | Lazy (first FXAA frame)     | Never                       | No (skip FXAA)   |
| primary_bake_pipeline      | Lazy (first bake)           | Never                       | No (CPU fallback)|
| brick_fill_pipeline        | Lazy (first brick fill)     | Never                       | No               |
| temporal_reproject_pipeline| Lazy (first temporal frame)  | Never                       | No               |
| pt_denoise_pipeline        | Lazy (first denoise frame)   | Never                       | No               |

## Bind Group Layouts (BindGroupCache)

| BGL                        | Created At      | Invalidated By | Notes                          |
|----------------------------|-----------------|----------------|--------------------------------|
| bind_group_layout          | Startup         | Never          | Main raymarch BGL              |
| volume_preview_bgl         | Startup         | Never          |                                |
| tonemap_bind_group_layout  | Startup         | Never          |                                |
| bloom_extract_bgl          | Startup         | Never          |                                |
| bloom_blur_bgl             | Startup         | Never          |                                |
| deferred_bgl               | Startup         | Never          |                                |
| ssr_bgl                    | Startup         | Never          |                                |
| fog_bgl                    | Startup         | Never          |                                |
| upsample_bgl               | Startup         | Never          |                                |
| pt_bgl                     | Startup         | Never          |                                |
| pt_bounce_bgl              | Startup         | Never          |                                |
| bevel_preview_bgl          | Startup         | Never          |                                |
| fxaa_bgl                   | Lazy            | Never          |                                |
| primary_bake_bgl           | Lazy            | Never          |                                |
| brick_fill_bgl             | Lazy            | Never          |                                |
| temporal_reproject_bgl     | Lazy            | Never          |                                |
| pt_denoise_bgl             | Lazy            | Never          |                                |

## Bind Groups (BindGroupCache)

| BG                   | Created/Recreated When          | Invalidated By                           |
|----------------------|---------------------------------|------------------------------------------|
| bind_group           | Lazy (first render frame)       | Texture resize, buffer recreate          |
| volume_preview_bg    | Lazy                            | Volume texture change                    |
| tonemap_bind_group   | Lazy                            | HDR texture resize                       |
| bloom_extract_bg     | Lazy                            | HDR texture resize                       |
| bloom_blur_h_bg      | Lazy                            | Bloom texture resize                     |
| bloom_blur_v_bg      | Lazy                            | Bloom texture resize                     |
| deferred_bg          | Lazy                            | G-buffer resize, buffer recreate         |
| ssr_bg               | Lazy                            | G-buffer resize                          |
| fog_bg               | Lazy                            | Fog texture resize                       |
| upsample_bg          | Lazy                            | Upsample texture resize                  |
| pt_bg                | Lazy                            | PT accum texture resize                  |
| pt_bounce_bg         | Lazy                            | PT accum texture resize                  |
| bevel_preview_bg     | Lazy                            | Bevel buffer recreate                    |
| fxaa_bg              | Lazy                            | Texture resize                           |
| temporal_reproject_bg| Lazy                            | Temporal texture resize                  |
| pt_denoise_bg        | Lazy                            | PT accum/G-buffer resize                 |

## Buffers (BufferPool)

| Buffer                     | Created At | Resized When                    | Invalidates BGs                    |
|----------------------------|------------|---------------------------------|------------------------------------|
| uniform_buffer             | Startup    | Never (fixed 1024B)             | bind_group, deferred_bg            |
| prim_buffer                | Startup    | Prim count exceeds capacity     | bind_group, deferred_bg, pt_bg     |
| tex_buffer                 | Startup    | Texture data upload              | bind_group, deferred_bg, pt_bg     |
| bvh_node_buffer            | Startup    | BVH rebuild exceeds capacity    | bind_group, deferred_bg            |
| bvh_index_buffer           | Startup    | BVH rebuild exceeds capacity    | bind_group, deferred_bg            |
| light_buffer               | Startup    | Never (fixed max-lights)        | bind_group, deferred_bg            |
| light_link_buffer          | Startup    | Never (fixed max-prims)         | bind_group, deferred_bg            |
| compositor_buffer          | Startup    | Never (fixed size)              | bind_group, deferred_bg            |
| env_buffer                 | Startup    | HDR env map upload               | deferred_bg, pt_bg                 |
| matcap_buffer              | Startup    | MatCap texture upload             | deferred_bg                        |
| tex_atlas_buffer           | Startup    | Atlas data upload                 | bind_group, deferred_bg            |
| tex_meta_buffer            | Startup    | Atlas metadata upload             | bind_group, deferred_bg            |
| bevel_edge_buffer          | Startup    | Never (fixed max-edges)         | bevel_preview_bg                   |
| bevel_params_buffer        | Startup    | Never (fixed size)              | bevel_preview_bg                   |
| bevel_grid_cells_buffer    | Startup    | Never (fixed size)              | bevel_preview_bg                   |
| bevel_grid_indices_buffer  | Startup    | Never (fixed size)              | bevel_preview_bg                   |
| brick_pool_dummy_buf       | Startup    | Never (4 bytes)                 | bind_group                         |
| brick_info_buffer          | Startup    | Never (fixed size)              | bind_group                         |
| fog_prev_camera_buffer     | Startup    | Never (fixed size)              | fog_bg                             |
| temporal_march_flags_buffer| Startup    | Never (fixed size)              | bind_group                         |
| temporal_params_buffer     | Lazy       | Never (fixed size)              | temporal_reproject_bg              |
| primary_bake_params_buffer | Lazy       | Never (fixed size)              | (per-dispatch BG, not cached)      |
| pt_denoise_params_buffer   | Lazy       | Never (fixed size)              | pt_denoise_bg                      |
| blend_params_buffer        | Lazy       | Never (fixed 32B)               | (per-dispatch BG, not cached)      |

### Blend/Overlay Textures (GpuCtx, lazy)

| Texture              | Format       | Usage                                | Owner    | Lifecycle                         |
|----------------------|-------------|--------------------------------------|----------|-----------------------------------|
| overlay_texture      | Rgba16Float | TEXTURE_BINDING + STORAGE + COPY_DST | GpuCtx   | Lazy alloc, resize with viewport  |
| blend_temp_texture   | Rgba16Float | TEXTURE_BINDING + COPY_DST           | GpuCtx   | Lazy alloc, resize with viewport  |

## Invalidation Rules

1. **Buffer resize** -> set referencing BGs to `None`. BGs are lazily recreated next frame.
2. **Texture resize** -> set referencing BGs to `None`. Textures are recreated in viewport_render.rs.
3. **Shader recompile** -> replace pipeline in PipelineRegistry. BGs are unaffected (BGL unchanged).
4. **Device loss** -> everything invalid. Full reconstruction from GpuInitState path.

## Lazy Pipeline Warming (WarmupManifest)

8 pipelines compile on first use (not at startup):
1. fog_pipeline
2. fxaa_pipeline
3. pt_pipeline (WGSL path tracer)
4. pt_denoise_pipeline
5. primary_bake_pipeline
6. brick_fill_pipeline
7. temporal_reproject_pipeline
8. blend_pipeline (composite_blend.wgsl — debug overlays)

Use `WarmupManifest::from_registry()` to snapshot current warm/cold status.

## Phase D1 — InstanceCullSystem (`GpuCtx.raster_cull`)

Owner of: `instance_cull_params` (uniform), `instance_cull_meta` (STORAGE), `instance_cull_batches`
(STORAGE), `instance_cull_args` (STORAGE|INDIRECT), `instance_cull_visible` (STORAGE|VERTEX), both
cull compute pipelines, and its own cached bind group. Reads (does NOT own)
`RasterPipeline.instance_buffer` via STORAGE.

Invalidation: `InstanceCullSystem.generation` bumps on any owned-buffer recreate
(`ensure_capacity`, poll phase only); `RasterPipeline.instance_buffer_generation` bumps when the
shared instance buffer is recreated (`upload_instances` grow/shrink). The cached bind group keys on
BOTH generations and rebuilds on mismatch — same-frame detection, no timing tricks. Creation/grow
only in `poll_background_tasks` (`poll_instance_cull`); dispatch site never mutates buffers.

## Phase D2 — HzbSystem (`GpuCtx.hzb`)

Owner of: `hzb_pyramid` (R32Float pow2 mip chain, STORAGE|TEXTURE|COPY_SRC), per-mip views +
full-chain view, seed/downsample pipelines, `hzb_params` (per-level 256-B slots), per-level
build bind groups. Reads (does NOT own) `RasterPipeline.depth_texture` via its depth view.

Invalidation: `HzbSystem.generation` bumps on pyramid recreate (`ensure_size`, poll phase only,
keyed on `raster.depth_size`); build bind groups key on (generation, `depth_generation`);
`valid=false` until the first build after any recreate — the instance cull falls back to
frustum-only while false (fail-closed). The cull system's bind group folds the HZB generation
into its cache key (dummy 1×1 far texture bound when the pyramid is absent).

D2 additions to InstanceCullSystem: `occluded_flags` (STORAGE, instance-cap), `counters`
(STORAGE 32 B) + `counter_staging` (MAP_READ, ≤1 readback in flight), CULL_PASSES=5
(camera-early, 3 cascades, camera-late).

### Path-trace progressive precision and variance (2026-10-12)

`GpuCtx` owns separate RGBA32Float full/half accumulation input/output pairs and
reprojection scratch; display HDR and denoise scratch remain RGBA16Float.
`BufferPool` owns full/half luminance Welford moments and their reprojection scratch,
the cached 128-byte denoise-pass upload buffer, and distinct colour/ReSTIR reprojection
uniforms. Resize recreates history/moments together and invalidates PT, reprojection,
and denoise bind groups. Scene/material/camera reset invalidates sample history through
the existing accumulation epoch; camera reprojection transports colour and moments in
the same ordered command stream. Raw FP32 output, never filtered display HDR, is copied
into the next frame's history. Denoise parameters are uploaded once then copied before
each pass; colour and reservoir reprojection never share mutable uniforms.

`PipelineRegistry.pt_warmup` owns one background PT compilation request/result.
Readiness polling is capped at one result per frame and scheduled as High work.
The existing raster draft remains active until all PT display pipelines are ready;
readiness cannot change the latched pipeline during an interaction or cooldown.
The GPU timestamp owner's dedicated PT interval includes reprojection, trace,
history copies, denoise and upscale. Only completed, valid GPU timings drive budgets.

### Path-trace hardware ray-query traversal (native RT, 2026-10-12)

`GpuCtx.native_rt: Option<NativeRtRenderer>` exists only when the device was created with
`app_gpu::native_rt::REQUIRED_FEATURES` (adapter-gated in `gpu_init`). It owns the per-geometry
BLAS cache, packed-index buffers and the TLAS package/binding. It holds NO material data:
the canonical PT group 0 (`pt_bg`/`pt_bg_half`) is passed in every dispatch.
`PipelineRegistry.native_rt_pipelines` (full + interactive + index-pack) is published by the
same one-shot PT warmup as `pt_tri_pipeline`, built from the SAME static specializations with
only traversal swapped; `None` = software traversal stays the backend.

- **Inputs:** `pt_vert_buffer` gains `BLAS_INPUT` while native RT exists (`upload_pod`
  reallocates a live buffer missing that usage, bumping `pt_scene_generation`).
  `pt_tri_buffer` is read by the index-pack pass.
- **Invalidation:** `GpuCtx.native_rt_geometry_epoch` bumps on every FULL PT assemble
  (geometry may change inside reused buffers, which `pt_scene_generation` cannot see) →
  BLAS cache cleared. TLAS-only refits pass a pose signature (model bits + tri/vert offsets)
  as the transform epoch; material edits never touch an acceleration structure. Buffer
  identity/count changes also invalidate geometry. `reset_pt_accel_state` (project/new scene)
  calls `clear()`. A scene the native path rejects is cleared (software fallback, logged).
- **Frame ownership:** in PathTrace the frame-wide encoder is a `PtFrameEncoder`; AS builds
  (<=2 BLAS + 1 TLAS per frame, 1M-triangle budget; a larger single mesh is admitted as
  `IdleOnly` work, never truncated) record into the SAME encoder as PT and post. Staged AS
  promote only via `commit` after that frame's single `submit`; dropped/stale frames are
  rejected by owner/revision/frame id.
- **Backend latch:** `GpuCtx.native_rt_active` changes only outside an interaction episode or
  cooldown (except forced fallback when the native scene is no longer ready); any change resets
  PT accumulation in the same frame, before uniforms. `AZRA_PT_SOFTWARE_TRAVERSAL=1` pins the
  software backend for A/B parity captures.
- **Material texture views (wgpu 25):** `material_tex_array_view` and the per-slot sRGB views
  declare `usage: TEXTURE_BINDING`. A view defaults to ALL texture usages and the array carries
  `STORAGE_BINDING`, which sRGB cannot support — the default made the view invalid and every
  raster/shadow material bind group with it.
