# Assembly Viewport — Millions-of-Polys Scene Renderer (Plan)

**Goal (owner Thread 3):** a read-only, Unreal/Blender-fast viewport that shows *everything baked* and
stays fast with **millions of polys / thousands of objects**. This is `RENDER_ARCHITECTURE.md §3.3`
(Assembly viewport) + §7 Phase 2 + §8 (the perf work).

**Branch:** `feat/assembly-viewport`, off master tip (`ef8bf98`). **Isolated from master and
sculpt-tool** (owner directive: do not work on the master tree). Render-core is master-owned in the
pillar model, so this lives on its own branch until it reaches parity and is ready to merge.

## Current state (code recon @ d51e024)
- **No compartmentalization yet.** One fused "Hybrid" renderer (`viewport_render.rs::dispatch_gpu_render`,
  ~1806 lines); per-frame it picks ONE `RenderMode` via `resolve_render_mode` and shares ONE G-buffer
  across SDF + raster. There is **no `active_renderer = match room {…}` switch** (RENDER_ARCHITECTURE
  Phase 1 is NOT done). The raster mesh pass (`raster_gbuffer.rs::dispatch_raster_pass`) is an additive
  pre-pass gated by `viewport_mode==Hybrid && !force_volume_only && !volume_preview_active`.
- **Draw cost = ~4N per frame** for N drawable bodies: N in the G-buffer loop + 3N across the shadow
  cascades, each a `draw_indexed(0..1)` (instance count 1). No batching.
- **No object frustum culling, no occlusion culling, no distance LOD.** The gather
  (`raster_gbuffer.rs:53-339`) filters only on `visible` / content / isolation — every off-screen and
  distant object still submits 4 full-res draws. Shadows redraw the whole scene 3×.
- The only frustum math in the repo is the shadow-cascade fit (`render/shadows.rs::frustum_corners_world`).

## Building blocks to REUSE (already exist)
- Raster G-buffer pipeline + 4 color targets + `RasterVertex` (32B) / `InstanceData` (128B, `step_mode:
  Instance`) layouts — `render/pipeline.rs`, `render/mesh_gpu.rs`.
- Deferred shade + full post (SSR, fog, bloom, tonemap, FXAA, DoF, upsample) — `viewport_render.rs:1653-1704`
  (entangled with the SDF path; must be extractable).
- Cascaded shadow maps (3×1024²) — `render/shadows.rs`.
- Persistent capacity mesh buffers + in-place partial upload (`GpuMesh::update_raw`, `mesh_cache.rs
  upload_pending` 5 ms budget) — the doc's "P-A" is largely done.
- Pooled per-frame instance buffer (grow/shrink) — `pipeline.rs:363-396`.
- Indirect-draw primitives: `multi_draw_indexed_indirect` (voxel chunk pool) + `draw_indexed_indirect`
  (resident SDF slot) — `raster_common.rs` (today single-body-scoped; the plumbing to generalize).
- **Unified drawable source:** `mesh_cache` (keyed by `MeshCacheId`) + `scene_graph.all_nodes()`.
  All 4 domains (SDF-DC, voxel Surface-Nets, poly, imported) already resolve to `(MeshCacheId,
  InstanceData, transform)` in the gather. **This IS the Assembly input.**
- Present contract: each renderer writes `gpu.hdr_texture` (Rgba16Float) → shared tonemap → egui image
  (`app_update.rs:290`). Assembly satisfies the same contract.

## Gated build plan (lowest-risk / biggest-win first — each additive, each shippable)

- **A1 — Object frustum culling (START HERE).** Compute a per-object world-space AABB (cached mesh
  bounds × transform) and test against the 6 camera frustum planes; drop culled objects from the draw
  gather (and shadow-only where a caster is off-screen but shadows into view). **Biggest cheap win,
  self-contained, low risk, benefits the CURRENT viewport too.** Reuses the frustum math pattern from
  `shadows.rs`. No compartmentalization needed. Gate behind a `RenderSettings` toggle for A/B.
- **A2 — General instancing + resident scene pool.** Generalize `raster_common.rs::draw_resident_pool`
  from single-body voxel chunks to ALL baked scene meshes: a persistent vertex/index/indirect mega-pool +
  per-instance transform array, one `multi_draw_indexed_indirect` over visible objects. Removes the
  per-object draw overhead (the 4N wall) and batches duplicated meshes.
- **A3 — Distance LOD.** Extend `MeshLod` (today an idle-time DC-quality tier) to camera-distance
  selection at draw time; per-object LOD chains. Cuts vertex/fill for distant objects.
- **A4 — Compartmentalized Assembly room.** Extract a standalone raster core from `dispatch_gpu_render`
  (gather + deferred + post, producing hdr_texture+depth+picking, NO SDF compute) behind a new
  `AppRoom`/`active_renderer` switch (RENDER_ARCHITECTURE Phase 1/4). The read-only "feels super fast"
  viewport. Keep the fused Hybrid path as default until Assembly reaches parity.
- **A5 — Shadow-at-scale + occlusion.** Cull/batch shadow cascades (stop redrawing the whole scene 3×),
  cache static casters; then Hi-Z occlusion culling last (biggest complexity, smallest incremental win
  until A1-A4 land).

**Why this order (not the "extract core first" order):** A1-A3 are additive perf features on the
existing gather (`raster_gbuffer.rs`, already de-god'd out of the render method) — they deliver the
millions-of-polys wins immediately and benefit the current viewport, WITHOUT touching the 1806-line
`dispatch_gpu_render` god-method. The big compartmentalization refactor (A4) lands after the perf
pieces exist and can be moved into the clean path wholesale. Frustum cull (A1) is needed under any
ordering, so it's the uncontroversial first step.

## Definition of done (per step)
1. `cargo build -p app` green; `cargo test -p graphics` green after any shader touch.
2. Measurable draw-call / frame-time reduction (add a counter: drawn vs culled objects, draw calls).
3. Correctness: nothing that should be visible is culled (the classic frustum-cull bug); selection/
   picking still works (P0); shadows still correct.
4. One manual A/B scenario (many duplicated bodies, orbit — watch draw count drop).
