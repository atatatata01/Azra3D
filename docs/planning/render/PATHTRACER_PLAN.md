# PATHTRACER_PLAN.md — Why the Path Tracer Never Worked, and the Plan to Make It Real

**Branch:** `feat/pathtrace-render` (worktree `dev/pathtrace`, from master @ 5f67fdf)
**Date:** 2026-07-16
**Status:** DIRECTION LOCKED (2026-07-16, owner) — **the PT intersector is DC-mesh triangles ONLY.
The analytical SDF march is banned for the path tracer exactly as it is banned everywhere else.**

> **OWNER DIRECTIVE (2026-07-16):** *"We do not use the analytical path anymore in any way, only DC
> mesh."* This retro-actively KILLS Phase 0/1 as written (they resurrected the analytical PT to
> "prove the pipeline"). Confirmed empirically the same day: making the analytical PT reachable
> device-lost the GPU (`0xcfffffff`) on the first dispatch — the `trace_ray → scene_sdf →
> scene_analytical` path is dead, unmaintained code that must NOT be revived. PT is **re-gated OFF**
> (reverted to master's guard order) and stays off until the triangle intersector (Phase 2) exists.
> Phase 2 is no longer "the core" — it is the **only** path. There is no analytical PT.
**Rule check (CLAUDE.md):** Path tracing is explicitly the surviving WGSL compute pass
(`pathtrace_entry.wgsl` → `build_pathtrace_wgsl` → `pt_pipeline`). The 2026-07-14 deletion ban covers
the **analytical SDF *display*** path (GLSL BVH + WGSL march). A **triangle BVH used only by the PT
compute pass** does not reintroduce that path and does not violate the Display Invariant (PT is ONE
technique for the whole view — it replaces raster for that frame, never fuses with it).

---

## 1. ROOT CAUSE — three independent kills, any one of which is fatal

The user-visible symptom "PT never works" is over-determined. Verified against code:

### K1 — The PathTrace dispatch is UNREACHABLE (certain)
`viewport_render.rs:1053` hardcodes `unified_raster: true` into `resolve_render_mode`
(`viewport_render_mode.rs:43-91`). Arm 0 (`if inputs.unified_raster → AnalyticalLite`, line 64) sits
**above** the PathTrace arm (line 71), so `RenderMode::PathTrace` can never be returned. This was
**deliberate** — commit `a718110`: *"resolve_render_mode's unified_raster early-return KEPT (hardcoded
true): removing it would make the dead PathTrace arm reachable."* The UI side (PT button
`viewport_header.rs:103`, Z-pie `input_keyboard.rs:757`, render panel `properties_render.rs:392`)
works correctly and sets `ShadingMode::PathTrace` — it just never translates into a dispatch.

### K2 — Even if dispatched, PT output never reaches the screen (certain)
PT writes HDR (binding 0) then sets `skip_deferred_pp = true` (`viewport_render.rs:1114`). Tonemap —
the ONLY HDR→LDR writer — is inside `if !skip_deferred_pp` (`:1409`). The screen shows the LDR
texture (`:490`). Comment at `:1462` admits it: *"PathTrace sets skip_deferred_pp and never rewrites
ldr."* → PT mode would show a stale frame.

### K3 — PT traces geometry that no longer exists as the product's geometry (the user's own diagnosis, confirmed)
`trace_ray` (`sdf_material.wgsl:74-84`) → `scene_sdf` (`sdf_scene.wgsl:379-382`) →
`scene_analytical` (analytical SDF prim march) or `scene_voxel` (legacy dense grid). **There is no
ray-triangle intersection reachable from PT anywhere in WGSL.** Since 2026-07-14, production geometry
is triangles: SDF bodies → baked DC meshes, poly bodies → half-edge `to_raster_data`, voxel bodies →
Surface Nets. PT sees none of it. It can only see analytical SDF prims (and `nv > 0` is even a gate
in Arm 1, so pure poly/voxel scenes are doubly excluded). The rm=3 "trace the baked volume" mode was
also deleted in `a718110` (bindings 40/41 are now dummy fills, `viewport_render.rs:646-651`).

---

## 2. COMPLETE BUG LIST (ranked)

| # | Sev | Bug | Evidence |
|---|-----|-----|----------|
| B1 | CRITICAL | PathTrace arm unreachable — `unified_raster` hardcoded `true` | `viewport_render.rs:1053`, `viewport_render_mode.rs:64` |
| B2 | CRITICAL | PT skips tonemap → HDR never reaches LDR/screen | `viewport_render.rs:1114`, `:1409`, `:1462` |
| B3 | CRITICAL | PT intersects only analytical SDF — blind to DC meshes, poly bodies, voxel bodies, imported meshes | `sdf_material.wgsl:74-84`, `sdf_scene.wgsl:379-382`; no triangle intersector in any PT-reachable WGSL |
| B4 | HIGH | `nv > 0` gate in Arm 1 skips PT for prim-less (poly/voxel-only) scenes | `viewport_render_mode.rs:71` |
| B5 | HIGH | Accumulation resets on ANY `self.is_dirty` → may never converge | `viewport_render.rs:690` |
| B6 | HIGH | 80-step, 0.6-relaxed march too shallow for secondary bounces → false misses / energy leaks to env | `sdf_material.wgsl:76-83`, `pathtrace_entry.wgsl:141` |
| B7 | HIGH | `find_hit_prim` desyncs from traced surface; voxel-mode hits get hardcoded default albedo | `sdf_scene.wgsl:290-304`, `pathtrace_entry.wgsl:61,64` |
| B8 | MEDIUM | `pt_clamp` (firefly clamp) and `pt_dof_aperture` (DOF) are UI-exposed but never read by the shader | `properties_render.rs:396,400` vs `pathtrace_entry.wgsl` (no refs) |
| B9 | MEDIUM | `pt_denoise` edge-stops on `gbuf_normal` that PT never writes → stale/garbage guidance | `viewport_render.rs:1081,1089`; GBUF_BINDINGS stripped from PT (`shader_build.rs:129`) |
| B10 | MEDIUM | `pt_accum` texture never cleared; first-frame correctness rests on `pt_accumulated >= 1` masking | `viewport_render.rs:644,697`, `pathtrace_entry.wgsl:149-151` |
| B11 | LOW | Non-physical hacks bias the image: bounce-0-only ambient, ad-hoc GGX on top of NEE, `1 - metal*0.7` fudge | `pathtrace_entry.wgsl:100,102-108,113-115` |
| B12 | LOW | Light loop hard-capped at 8, no importance sampling | `pathtrace_entry.wgsl:78` |
| B13 | LOW | `validate_pt_shader` is parse-only (naga_parse) — other shaders were upgraded to full validate after a parse-only test let a runtime error through; PT was not | `shader_build.rs:330-339,437-441` |
| B14 | LOW | Silent no-op if `pt_bg`/`pt_pipeline` is None at dispatch — no error surfaced | `viewport_render.rs:1068` |

---

## 3. WHAT THE GPU ALREADY HAS (inventory that shapes the design)

- **Triangles for EVERY body type, one format, one cache.** `MeshCache`/`GpuMesh` (`render/src/mesh_cache.rs:97`, `mesh_gpu.rs:53`): `RasterVertex` 32 B (pos/normal/uv) + u32 indices, **body-local**, world transform applied per-draw via `InstanceData` (128 B: model + normal matrix + material_idx + body_index + flags). SDF-DC, poly, voxel-SN, imported — all land here. This is the ONLY representation that covers everything.
- **BVH machinery exists but is prim-based, not triangle-based.** `core_math/src/bvh.rs`: SAH `build_bvh` (`:87`), GPU-ready 32 B `BvhNode` (`:21`), `LEAF_BIT` child encoding (`:399`). Reusable core; needs a triangle-leaf builder.
- **Resident SDF fields** for SDF bodies: `VolumeAtlas` (single R32Float 3D texture, shelf-packed per-body cells + world AABB/uvw params, `volume_atlas.rs:348`) and per-body `ResidentVolume` buffers. Sphere-traceable today — optional high-quality intersector later.
- **Sparse sculpt atlas + working traversal**: `VoxelAtlasGpu` (u32 tag index + 16³ occupancy bricks) with `sculpt_voxel_raymarch.wgsl` already doing the sparse walk — liftable as a second intersector later.
- **Shading inputs all resident**: `light_data` storage (16 f32/light), `RasterMaterial` storage buffer (160 B, indexed by `material_idx`), env map + mips, matcap. Materials are keyed to `material_idx` — triangle records must carry it.
- **No hardware RT**: wgpu 22, no ray_query/acceleration-structure features. CUDA/OptiX crate exists (feature-gated off) and is an SDF tracer — its OptiX GAS is over prim AABBs, not triangles. Ignore for mainline; note OptiX triangle-GAS as a possible later `--features cuda` fast path.

---

## 4. THE PLAN — gated phases, each independently shippable

### ~~Phase 0 — Resurrect the analytical plumbing~~ — ABANDONED 2026-07-16 (owner directive)
Tried, reverted. Making the analytical PT reachable device-lost the GPU on first dispatch, and the
owner banned analytical PT outright. The geometry-agnostic plumbing lessons still stand and fold
into Phase 2: PT output MUST go through the shared bloom→tonemap→LDR chain (never `skip_deferred_pp`);
accumulation must survive the `!is_dirty` render gate (PT converges on frames nothing marked dirty)
and reset only on real change; the accum texture's first frame must not read uninitialised memory.
KEPT from the abandoned attempt (geometry-agnostic, dormant until PT re-enables): the shader
first-frame accum guard (`pathtrace_entry.wgsl`) and the full-validate PT shader test
(`shader_build.rs`).

### ~~Phase 1 — Correctness of the analytical tracer~~ — ABANDONED 2026-07-16
The analytical intersector is deleted-in-spirit; there is nothing to make correct. `pt_clamp`/DOF
(B8), march depth (B6), material desync (B7), shading hacks (B11) all get re-addressed natively in
the triangle tracer.

### Phase 2 — THE ONLY PATH: unified triangle path tracing over the DC mesh
1. **B1:** feed `unified_raster` a real condition (`shading_mode != ShadingMode::PathTrace` at
   minimum), or move the PathTrace arm above the unified-raster guard in `resolve_render_mode`.
   Keep AnalyticalLite as the fallback for every non-PT mode.
2. **B4:** drop/replace the `nv > 0` gate (Phase 0 interim: keep it; Phase 2 removes it when
   triangles arrive).
3. **B2:** run tonemap in the PT branch. Cleanest: PT sets a `pt_needs_tonemap` and the tonemap
   dispatch condition becomes `!skip_deferred_pp || pt_needs_tonemap` (PT still skips deferred, SSR,
   fog, bloom for now).
4. **B10:** clear `pt_accum` (zero-init or explicit clear) on reset instead of relying on the
   weight-1 mask.
5. **B5:** narrow the accumulation reset — reset on camera change + `scene_modified`-class dirt, NOT
   blanket `is_dirty` (selection highlight / UI hover must not nuke convergence).
6. **B14:** log once (`log::warn!`) if pipeline/bind group missing at dispatch.
7. **B13:** upgrade `validate_pt_shader` from parse to full naga validate.
Acceptance: PT button → analytical SDF prims render, image visibly converges when camera is still,
resets on orbit, no stale-frame display. Selection still works (render-path change → test per CLAUDE.md).

### Phase 1 — Correctness of the existing tracer (S/M, ~1 day)
8. **B8:** apply `pt_clamp` to per-sample radiance; implement thin-lens DOF using
   `pt_dof_aperture` + focus distance in ray gen.
9. **B6:** raise march quality for PT: adaptive step count (e.g. 128–256 with early-out),
   relaxation 0.9 with fallback, distance-proportional epsilon; secondary rays get the same budget.
10. **B7:** carry hit prim ID out of the march (nearest-prim at hit point *of the traced field*),
    voxel-mode hits sample sculpt material instead of hardcoded albedo.
11. **B11:** remove bounce-0 ambient + metal fudge; single consistent BRDF (Lambert + GGX with
    proper Fresnel/probabilities) so the image converges to something defensible.
12. **B12:** (defer if time) uniform light *selection* (pick 1 of N per sample, weight by 1/pdf)
    instead of the 8-light cap.
Acceptance: PT matches deferred "look" for a 3-prim SDF test scene (albedo/emission/env), fireflies
clamped, DOF slider does something.

### Phase 2 — THE REAL FIX: unified triangle path tracing (L, the core of this branch)
PT must trace what the raster pass draws: **the triangles in MeshCache**. Design:
- **BLAS per body:** CPU SAH triangle BVH (extend `core_math/bvh.rs` with a triangle-leaf builder
  reusing the existing SAH core + 32 B `BvhNode`), built over the body-local `RasterVertex` data.
  Built on the **background thread** (rayon, like DC meshing), keyed to `MeshCacheId` + the cache's
  existing `source_hash`/generation counters. Never on the frame thread (Frame Budget Rule).
- **TLAS over instances:** small world-space BVH over per-body world AABBs, rebuilt cheaply
  (few-hundred bodies max) whenever any `InstanceData` transform or BLAS changes. Body transforms
  stay per-instance — verts remain body-local; rays transform into body space at TLAS leaves
  (matches the raster instance model, zero re-bake on body move — same invariant as the rest of the
  app).
- **New SSBOs** (BGL entries added in `bgl.rs` — single source; buffers created/grown in
  `poll_background_tasks` per Frame Ordering Contract; generation counters per buffer):
  `pt_tlas_nodes`, `pt_blas_nodes`, `pt_tris` (v0/v1/v2 indices + material_idx + body_index),
  `pt_verts` (pos+normal, world-transform applied in-shader via the instance matrix table),
  `pt_instances` (model/inv-model/normal matrices, blas offset).
- **WGSL traversal kernel** replaces `trace_ray` for PT: short-stack BVH2 traversal, watertight
  Möller–Trumbore, interpolated normals. Shade from `RasterMaterial` storage buffer (same buffer the
  raster pass uses — one material truth, no duplication).
- **Remove the `nv > 0` gate** — geometry presence = "any TLAS leaves", not prim count.
- **B9:** PT writes primary-hit normal + depth (either into `gbuf_normal`/depth or a PT-owned pair)
  so `pt_denoise` gets real guidance; keep G-buffer contract in mind (PT is its own view technique —
  the deferred consumers are skipped, but the denoiser inputs must be written by PT itself).
Acceptance: a scene with 1 SDF body + 1 poly body + 1 voxel body renders ALL of them in PT,
matching raster silhouettes exactly (same triangles). Blender-style parity check on a cube.

### Phase 3 — SUPER PERFORMANT (GPU) (M/L, iterative — instrument-then-cut)
Priority order, each measured with the existing `TIMESTAMP_QUERY` profiler (`tw!()` only on passes
that actually dispatch):
- **P3.1 Interaction responsiveness:** half-resolution PT during camera interaction + upscale,
  latched via `InteractionRenderPolicy` (zero path switches mid-episode); full res on settle.
  1 spp/frame progressive; the accumulation loop already exists.
- **P3.2 BVH quality/refit:** transform-only changes = TLAS refit (no BLAS rebuild — body moves are
  free, same as raster); sculpt strokes = BLAS rebuild only for the stroked body, budgeted like mesh
  uploads (≤2/frame). BVH4 widening only if traversal profiling says divergence-bound.
- **P3.3 Wavefront split** (only if the megakernel profiles register/divergence-bound): separate
  raygen / trace / shade dispatches with ray queues. Do NOT pre-build this — measure first.
- **P3.4 Sampling efficiency:** blue-noise/low-discrepancy sampler (Sobol/Owen or R2 sequence),
  NEE with light selection pdf, existing focus-adaptive SPP kept, firefly clamp from Phase 1.
- **P3.5 Temporal reuse:** `temporal_reproject.wgsl` exists — reproject accumulation on camera move
  instead of full reset (turns "reset to black" into "smeared but converging"). Big perceived-perf win.
- **P3.6 Denoiser upgrade:** feed pt_denoise albedo+normal+depth from PT primary hits (SVGF-lite:
  temporal accumulation + edge-aware à-trous — the pieces exist as separate shaders already).
- **P3.7 Hybrid intersectors (optional, later):** SDF bodies via `VolumeAtlas` sphere-trace
  (analytic-quality surfaces, no tessellation error) and voxel bodies via the sparse atlas walk from
  `sculpt_voxel_raymarch.wgsl`, dispatched per TLAS-leaf body kind. Triangle path stays the
  baseline for everything.
- **P3.8 CUDA/OptiX** (`--features cuda` only): triangle GAS in `optix_accel.rs` for hardware RT.
  Strictly optional; wgpu compute is the mainline.

---

## 5. CONSTRAINTS CARRIED FROM CLAUDE.md (checked, all compatible)
- Display Invariant: PT = the single technique for the view while active. Never composited with the
  raster G-buffer over the same pixels; gizmos/grid overlay handling deferred to a later polish item.
- Shader edits batched (5 `sdf_*` files → 25-43 s recompile) — Phase 1 shader changes in ONE edit.
- `neutralize_blend_noise_wgsl` stays applied to PT builds; never unstub NOISY_BLEND.
- BGL single source (`bgl.rs`), Frame Ordering Contract, generation counters — all new PT buffers
  comply (Phase 2 explicitly lists them).
- `cargo test -p graphics` after every shader change; upgrade PT test to full validate (Phase 0).
- Frame Budget: BVH builds background-thread + budgeted uploads; no unbounded per-frame loops.
- Bug tracker: on landing each phase, mark B1–B14 in `COMPLETE_BUG_TRACKER.md`.

## 6. SESSION LOG
- 2026-07-16 (reproject worktree) — **P3.5 temporal reprojection SHIPPED (uncommitted).** Camera
  motion in PT no longer resets accumulation to black: a new `pt_reproject.wgsl` compute pass
  gather-reprojects the previous accumulation into the new camera (unproject via THIS frame's
  raster depth `gbuf_normal.w` — the raster pass runs under PT — validated against the PREVIOUS
  frame's depth/normal copy; off-screen / depth-mismatch / normal-mismatch / was-background →
  invalid). **Accumulation became PER-PIXEL:** `pt_accum` alpha now carries the pixel's history
  length (0 = local restart, weight = 1/(hist+1); f16-exact cap 2048) — a disocclusion restarts
  ONLY those pixels; with reprojection off, alpha counts frames and reproduces the old global 1/N
  exactly. Reprojected history clamped to 32 (ghost limiter). CONTENT changes still fully reset:
  every content mutation zeroes `pt_accumulated_frames` at its source (pt_scene assemble/refit,
  sliders, load) before the camera check — the `frames > 0` gate routes them to the full reset;
  camera-only frames set frames=1 (dispatch at 2 → shader reads history). NOT reused: the raster
  `temporal_reproject.wgsl` — it's a re-march validity MASK that invalidates the whole screen on
  camera move (the opposite semantic), and its params pack ortho in `up.w`, which this app fills
  with march_pixel_scale; `pt_reproject.wgsl` moves COLOR and matches `camera_ray` packing
  (ortho=pos.w, fov=fwd.w). Ortho viewports fall back to full reset (gbuffer .w is eye-distance,
  not along-ray, in ortho). Plumbing per the rules: `pt_reproject_bgl_entries()` in
  `graphics::bgl` (single source), scratch texture (storage-write → copy into pt_accum; no
  in-place compute RW) created in the resize block alongside pt_accum with the bind group nulled
  there, params uniform + lazy `ensure_pt_reproject_pipeline` (pt_denoise pattern), and the PT
  arm now maintains `temporal_prev_normal_dist`/`temporal_prev_cam` (previously non-PT-only).
  Developer Tweaks: "PT temporal reprojection (P3.5)" toggle, DEFAULT ON (owner A/B; flipping
  resets accumulation). Gates: app build green, graphics 43/43 (new full-validate
  `validate_pt_reproject_shader`), app bins 190/190. Owner test: PT mode, orbit → image stays
  (slightly smeared, refines on stop; edges resolve without trails); edit a material mid-orbit →
  full reset (correctness); toggle OFF → old reset-to-black returns.
- 2026-07-16 (cache-perf worktree) — Sparse-snapshot pump finished: windowed monolithic dispatch
  (`mesh_into_slot_sparse`, surface-brick bbox — replaces the 1023³ full-grid LOD dispatch that
  ballooned classify to 265ms), quiet-gate + landing/drop logs kept. PT-warm UX: one-shot
  `[PtWarm]` log + toast when PT is entered while `pt_tri_pipeline` is cold (latch re-arms when
  the pipeline lands). **P3.2 TLAS refit fast path shipped**: transform/material-only changes
  (every gizmo-drag frame) re-upload ONLY tlas+instances+materials+meta — the blas/tris/verts
  SSBOs stay untouched (`PtAccelCache::refit`, shared walk with `assemble`, offset-parity test).
  Gates: build green, render pt_accel 6/6, app bins 185/185. Next: temporal reprojection (P3.5).
- 2026-07-16 — **Phase 2 SHIPPED + OWNER-VERIFIED ("this is awesome"): the triangle path tracer
  works end-to-end.** Milestones A–E in one session: A = `build_tri_blas` (core_math/bvh.rs, SAH
  triangle-leaf BLAS, 6 ground-truth tests vs brute force, 27176ce). B = `PtAccelCache` +
  `trace_scene_cpu` parity oracle (render/pt_accel.rs, 9b2481d). C+D = content stamps on
  `CachedMesh`, `pt_scene.rs` gather/assemble/upload (6 SSBOs bindings 42-47, poll-phase, generation
  counters), `pt_bgl_entries()` single-source extraction, `pathtrace_tri_entry.wgsl` traversal
  (bounded loops — a bad tree can't TDR), owner-verified clay render of SDF+poly+voxel bodies
  matching raster silhouettes (987d538). E = full PT: RasterMaterial buffer (binding 48), NEE
  lights + traced shadows (LIGHTING region extracted), sample_env (ENV region), bounces + RR +
  pt_clamp + DOF + accumulation w/ first-frame NaN guard — owner-verified, ran clean (c4f0df6).
  F (this commit) = interaction clamp (1 spp / ≤2 bounces while orbiting), BLAS build budget
  (≤4/frame), **legacy analytical PT entry DELETED** (pathtrace_entry.wgsl, build_pathtrace_wgsl,
  ensure_pt_pipeline, pt_pipeline — the bg-compile slot now warms the TRI pipeline; never
  reintroduce an SDF-marching PT). Remaining Phase 3 perf ideas: TLAS refit-only fast path,
  temporal reprojection, SVGF-class denoiser, per-tri materials for multi-material imports,
  resident-SDF direct GPU mesh source. Spec: PATHTRACER_PHASE2_SPEC.md.
- 2026-07-16 — Worktree `dev/pathtrace` created (`feat/pathtrace-render` from master 5f67fdf).
  3-agent diagnostic sweep (pipeline wiring / shader / GPU geometry inventory). Root causes K1–K3
  confirmed; B1–B14 catalogued; phased plan written. No code changed yet.
- 2026-07-16 — **Phase 0 REVERTED same day (owner directive: analytical banned).** Enabling the
  analytical PT device-lost the GPU (`0xcfffffff`) on the first dispatch — frames 1–5 ran fine on
  AnalyticalLite (floor+bg, touches no scene eval), then clicking PT dispatched `trace_ray →
  scene_analytical` (dead since the analytical display path was deleted 2026-07-14) and the GPU
  faulted with no diagnostic (release build = no validation layer). Owner then confirmed the path
  tracer must NEVER use analytical — DC-mesh triangles only. Re-gated PT OFF (reverted
  `viewport_render.rs` + `viewport_render_mode.rs` to master); app is safe (clicking PT falls back
  to the normal raster view). Kept the shader accum-guard + PT full-validate test. Next = Phase 2
  triangle intersector, now the sole path.
- ~~2026-07-16 — **Phase 0 SHIPPED** (B1, B2, B5, B10, B13, B14).~~ (superseded by the revert above) Discovery during implementation:
  B5 was worse than catalogued — the `!is_dirty → return` render gate also meant PT dispatched
  exactly once and froze (no repaint→dirty chain), so accumulation was doubly impossible. Fixed
  with a bounded gate bypass (`PT_MAX_ACCUM_FRAMES = 4096` in `viewport_render_mode.rs`); the
  `|| is_dirty` reset term is now CORRECT (convergence frames run with is_dirty false) and was
  kept. `skip_deferred_pp` removed entirely — tonemap runs for all modes; PT now also gets the
  overlay-line wireframe pass. B4 (`nv > 0` gate) deliberately KEPT until Phase 2. Build green,
  41/41 graphics tests (PT test now full-validate). NOT yet manually verified in the running app
  (owner: PT button on an SDF-prim scene → image appears, converges when still, resets on orbit).
- 2026-07-16 — **PT update-latency pass (feat/sparse-3a, uncommitted).** Owner repro: 1024³ sparse
  edit in PT → surface updates ~3s after release. Measured headlessly on a synthetic 857K-vert /
  1.71M-tri UV sphere (`bench_blas_build_1_7m`, run with `cargo test -p render --release --
  --ignored bench_blas_build_1_7m --nocapture`): BLAS build **1090ms → ~360ms** (3×) via a
  parallel SAH builder in `core_math/src/bvh.rs` — rayon-forked subtrees (`build_parallel`,
  splice layout `[parent, l_root, r_root, l_rest, r_rest]` keeps the adjacent-children encoding;
  WGSL traversal untouched), parallel 3-axis single-pass SAH binning + centroid/AABB reductions
  (exact — min/max unions are associative), parallel prim/vert/tri maps (rayon collect preserves
  order; node count bit-identical to sequential: 3,419,045 both). Landing stall
  (`[SLOWPOLL] rest(top)` 207–222ms): `PendingSdfReadback::take` copied the staging **capacity**
  (potentially 100s of MB) then rebuilt verts one-by-one — now copies only the counter-reported
  live prefix as one bytemuck memcpy per array; `update_editable_mesh` splits pos/normals in
  parallel. `assemble` measured 43ms at 1.7M tris (tri-rebase parallelized; the 109MB blas-node
  memcpy dominates — per-mesh incremental SSBO upload NOT done, would be a third offset path
  next to assemble/refit). `pt_source_data` clone measured **13.8ms** — left as-is (Arc-ifying
  mesh_cache storage not worth the churn at that cost), instrumented instead. New timings follow
  the [PtSnap]/[PtBlas]/[PtAsm] convention (>5ms gate). Parity: new `parallel_blas_matches_
  brute_force` render test (leaf coverage exactly-once, AABB containment, traversal vs brute
  force at 18K tris — past the 8192 parallel threshold). Settle gate untouched. Gates: app build
  green, app --bins 190 passed, render 20 passed, core_math 502 passed. Expected post-settle
  latency for the owner case: ~3s → **well under 1s** (≈0.3s settle + ~0.4s background BLAS +
  ~70ms assemble/upload, landing poll no longer stalls the frame).
