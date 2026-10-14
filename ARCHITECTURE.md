# ARCHITECTURE.md — System Knowledge

How things work. For rules (what agents must/must not do), see CLAUDE.md.

---

## Scene Graph — Ownership Model

### Ownership Hierarchy
Scene graph owns: SdfBody, Light, Camera, Material, Group, PolyMesh, NurbsSurface nodes.
SdfBody owns: `Vec<SdfPrimitive>` (prim array, not individual graph nodes).
All scene entities accessed via `NodeId`. No parallel `Vec<T>` storage.
Lights: `LightObject` eliminated. All lights stored as `NodePayload::Light(LightData)`.
Materials: `NodePayload::Material { id: MaterialId, name, data: PbrMaterial }`. Prims reference by `MaterialId`.

### Flat Cache Discipline
`self.scene.primitives` (`Vec<SdfPrimitive>`) is a READ-ONLY GPU cache rebuilt from the graph.
Direction: graph → flat cache (via `sync_flat_cache`). Never reverse.
`sync_flat_to_graph()` is DELETED (confirmed pre-Q2, comment stub cleaned).

**Mutation categories:**
- **SDF prim properties** → PropsContext pattern. `pctx.changed` triggers `mirror_prim_to_graph()`.
- **Scene graph nodes** (poly, NURBS) → `self.scene.scene_graph.get_mut(nid)` + `mark_dirty(DirtyFlags::RENDER)`.
- **Lights** → `self.scene.light_by_id_mut(id)` + `mark_dirty(DirtyFlags::LIGHT)`. Graph-native access.
- **Render/compositor settings** → write directly + `mark_dirty(DirtyFlags::RENDER)`.
- **App state** (tool, selection, work mode) → write directly, no dirty flag needed.

---

## Undo System — Transaction Model (undo.rs)

### Transaction API
Two forms:

**Atomic** (discrete actions — button clicks, menu items, keyboard shortcuts):
```rust
self.transaction("Delete body", |s| { s.scene.remove_body(nid); });
```

**Episode** (continuous drags — sliders, gizmo transforms):
```rust
self.begin_episode("Adjust roughness");
// each drag frame: mirror_prim_to_graph(idx); mark_dirty(SCENE);
self.commit_episode();
```

`auto_commit_episodes()` runs at end of each `update()` frame to catch leaked episodes (pointer released without explicit commit). `cancel_episode()` discards without pushing to undo stack (gizmo cancel via Escape/right-click).

`transaction_snapshot_only()` for cases where egui already mutated the field directly — captures pre-mirror state and pushes immediately.

### What Gets Captured
`UndoEntry` contains: `UndoPayload::Full` (Arc prim snapshot + full SceneGraph clone), `active_body_nid`, full `UnifiedSelection`. Undo restores all three, then calls `sync_flat_cache()` to rebuild GPU cache from graph. VecDeque with configurable max depth.

### Deleted Functions
- `save_undo()` — DELETED. 320 call sites migrated to `transaction()` / `begin_episode()`.
- `save_delta_undo()` — DELETED. Absorbed into transaction (Arc sharing handles efficiency).
- `sync_flat_to_graph()` — DELETED pre-Q2, never reintroduced.

### Per-Panel Undo Status
- SDF properties: PropsContext + episode transaction ✓
- Poly/NURBS: episode transaction on drag start ✓
- Lights: ✓ — graph nodes, restored automatically with graph snapshot
- Compositor: unknown — verify before assuming undo exists
- Render settings: no undo needed ✓

---

## Dirty Flags (dirty.rs)

`DirtyFlags::SCENE` triggers SVO bake + GPU serialize. `DirtyFlags::RENDER` triggers viewport repaint only. `DirtyFlags::LIGHT` triggers light buffer update. Legacy `is_dirty` bool eliminated — 349 sites converted to `mark_dirty()`.

**Visibility toggle:** must mark BOTH `DirtyFlags::RENDER` AND parent body as volume-dirty (`mark_node_body_vol_dirty`). Otherwise voxel cache and DC mesh desync.

---

## Save/Load (project.rs)

`save_scene()` writes: scene graph, compositor, material library, light linking, HDR env map path, RenderSettings (70+ fields), worker GPU state. `new_scene()` resets ALL transient state. `apply_to_app()` rebuilds flat cache from graph, recalculates next IDs, validates graph, resets tool/selection state. ProjectFile still stores both primitives and graph (duplication) — v2+ ignores flat cache on load.

---

## Rendering Pipeline

### Dispatch Paths
The analytical SDF display paths (`pipeline`/`pipeline_march` WGSL march + the GLSL BVH kernel) were DELETED 2026-07-14 (dead — gated behind constant-false flags). Remaining compute passes:

1. **`pipeline_lite`** — the sole raymarch pass: writes floor + background only (no SDF march). SDF geometry comes from the baked DC mesh drawn by the raster pass. ~0.08ms.
2. **WGSL path tracing** (`pt_pipeline`) — `pathtrace_entry.wgsl`. The only path tracer (GLSL PT removed).

### Deferred Shading
- `deferred_full` — full PBR: shadows, AO, SSR, reflections, DNA materials. Used at idle.
- `deferred_lite` — stripped: no shadows, no AO, no SSR. Used during interaction for ALL shading modes. Drops deferred cost from ~11ms to ~0.4ms.

### Post-Processing
SSR, fog, bloom, DOF, FXAA. Interaction-aware: passes costing >0.5ms skipped during interaction, re-enabled at idle.

### Tonemapping & Color
- Pipeline is linear sRGB throughout. sRGB sources linearized at sample time.
- AgX tonemapping (canonical, with inset/outset matrices). PBR Neutral (Khronos) available.
- Correct piecewise sRGB transfer functions.
- BT.709 luminance everywhere except FXAA (intentionally BT.601).
- Output: tonemap → sRGB → triangular dither → framebuffer.


---

## Shader File Layout

| File | Role |
|---|---|
| `sdf_shared.wgsl` | Shared SDF functions: accessors, eval_prim, scene_sdf, booleans, apply_boolean |
| `raymarch_lite.wgsl` | The sole raymarch pass (floor + background). Built via `build_raymarch_lite_wgsl()` |
| `deferred_entry.wgsl` | Deferred shading entry. Built via `build_deferred_wgsl()` = shared + entry |
| `fog_entry.wgsl` | Fog post-process |
| `pathtrace_entry.wgsl` | WGSL path tracer (the only PT path) |
| `post_process.wgsl` | SSR, compositor, tonemap, FXAA |
| `volume_bake.wgsl` | GPU voxel bake (scope stack machine matching scene_analytical) |
| `compute.wgsl` | **DEAD.** Legacy monolith. Never compiled. Frozen. |
| (`raymarch_entry.wgsl`, `raymarch_march.wgsl`, `graphics/src/glsl/*`) | **DELETED 2026-07-14** — analytical SDF march + GLSL BVH display path |

### GPU Buffer — Per-Prim Layout
`FLOATS_PER_PRIM = 284`. Hot buffer (80f, @binding(3)) for per-frame data. Cold buffer (224f, @binding(5)) for stable data. Fields 0-115 base, 116-117 DNA headers, 118-119 bevel profiles (top/bot), 120-279 DNA rules (8 × 20 floats), 280 bevel_profile_vert, 281-283 padding.

*Cold buffer authoritative field map: TODO — document exact field indices here.*

### Boolean Operations
3 ops: Union (0), Subtract (1), Intersect (2). ChamferUnion (3) / ChamferSubtract (4) are legacy serde compat — normalized to Union/Subtract + Chamfer profile at load. 3 blend profiles: Smooth/Polynomial (0), Chamfer (1), Stairs (2).

---

## Nested SdfBodies

Scene graph supports arbitrary body nesting. Each SdfBody has `body_op: BooleanOp` for combination with parent scope. GPU uses scope markers (`shape_type` 254=OPEN, 255=CLOSE) evaluated by stack machine (max depth 8). Top-level bodies always union regardless of `body_op`.

**SCOPE_OPEN field layout:** `[0-2]=AABB min, [3]=254, [5]=mirror_flags, [6-7,12]=mirror_center, [8-10]=AABB max, [11]=op_type, [13]=bevel_radius, [15]=blend_k, [106]=blend_profile`.

`volume_bake.wgsl` has a matching scope stack machine. Changes to scope marker layout in `emit_scope_open()` / `emit_scope_close()` must be reflected in BOTH `sdf_shared.wgsl` AND `volume_bake.wgsl`. CPU bake fallback does NOT support nested scopes.

---

## SdfBody Transforms

- `origin` (inside `NodePayload::SdfBody`) = world-space pivot. NOT `node.local_position`.
- **Translate:** `apply_to_scene_node` offsets all prim positions + origin. Recursive for children.
- **Rotate/Scale:** Body pivot code in `gizmo.rs`. Orbits prims around origin. `apply_to_scene_node` does NOT handle R/S for SdfBody — adding it causes double rotation.
- After prim membership changes: call `recalculate_body_origin(body_nid)`.
- PivotMode: Centroid (default), WorldOrigin, BoundingBox, Custom.

---

## Isolate Mode

`isolated_body_nid: Option<NodeId>` — `None` = show all, `Some(nid)` = only render that body + children. Pure visibility filtering — no pipeline changes, no ghost meshes. Reset on `new_scene`, `load_scene`, and if isolated body is deleted.

---

## Properties Panel

### PropsContext + Episode Transaction Pattern
Properties sliders use episode transactions:
- `drag_started` → `begin_episode()` captures pre-mutation snapshot
- `changed` → `mirror_prim_to_graph()` + `mark_dirty(SCENE)` — no new undo entry per frame
- `drag_released` → `commit_episode()` seals ONE undo entry for the entire drag

Button clicks (discrete actions) use atomic `transaction()`.

```rust
let r = ui.add(egui::Slider::new(&mut value, range).text("Label"));
if r.drag_started() { pctx.drag_started = true; }
if r.changed() { pctx.changed = true; pctx.desc = "Description".into(); }
```

**Footgun:** `ui.memory(|m| m.is_anything_being_dragged())` fires for ANY drag in ANY panel. Use `.drag_started()` on the specific response.

---

## Per-Body Cache

Lifecycle: `Clean → VoxelDirty → VoxelBaking → Meshing → Clean` (+ `Failed`). Targeted dirty: only edited body rebakes. GPU bake default (~6ms). `render_mode = 1` in deferred when cached — shadow/AO/DNA via voxel O(1), not analytical O(N).

---

## Materials — Graph-Backed

Materials are `NodePayload::Material` nodes in the scene graph (`core_math/src/scene_graph.rs`).
- `MaterialId` (u32 newtype, `core_math/src/primitives.rs`) references materials by ID.
- `SdfPrimitive.material_id: MaterialId` — `MaterialId::NONE` (0) = use default material.
- Default scene creates a "Default Material" node.
- GPU serialization resolves `material_id` → `PbrMaterial` from graph.
- Graph API: `add_material()`, `material_by_id()`, `material_by_id_mut()`, `materials()`, `remove_material()`, `material_count()`.
- `MaterialLibrary` kept in `scene_state.rs` for backward-compatible save serialization only. Reconstructed from graph nodes during save (`project_save.rs`). On load, graph Material nodes are the source of truth.
- Save format v6: materials stored as graph nodes. v5 files auto-migrate `mat_lib` entries to graph.

---

## Field Evaluator (core_math/src/field_eval.rs)

### FieldEvaluator Trait
CPU-side SDF distance query API for tools, snapping, hit queries.
NOT a replacement for DC extraction (DC uses baked grids).

`SceneEvaluator` wraps `&SceneGraph`:
- `eval(point)` → signed distance (min across all bodies)
- `eval_gradient(point)` → (distance, normal) via tetrahedron method
- `closest_surface(point)` → `SurfaceHit` with `body_nid` + `prim_uid`
- `eval_body(body_nid, point)` → distance for single body
- `cpu_raymarch(evaluator, origin, dir, max_dist)` → sphere-trace to surface

Handles body transforms (`world_to_local`) and boolean chains automatically.

---

## Frame Execution Order
1. `scheduler.begin_frame()` — reset budget counters
2. `poll_background_tasks()` — GPU map_async, pipeline compile, AI, volume cache (budget-gated)
3. `handle_viewport_input()` — keyboard, mouse, tool state
4. `dispatch_gpu_render()` — serialize prims to GPU, run compute shaders (skips when not dirty)
5. Draw viewport image — blit GPU output to egui
6. `draw_viewport_overlays()` — egui painter: wireframes, gizmos, creation overlay, perf HUD
7. `auto_commit_episodes()` — commit dangling undo episodes

### Frame Budget
`FrameScheduler.begin_frame()` resets budget at frame start. Each poll/upload/rebuild checks `scheduler.can_run()` before executing. Over-budget Low/IdleOnly work deferred to next frame. Scheduler work log displayed in perf HUD when enabled.

## Frame Execution Architecture (Q2 2026)

### FrameScheduler (`frame_scheduler.rs`)
Budget-aware per-frame work scheduling. Target: 12ms CPU (leaves 4ms for GPU + vsync).
WorkClasses: Critical → High → Medium → Low → IdleOnly.
Each work site registers a `WorkTicket` before executing. Scheduler checks remaining budget.

### Per-Frame Drain Caps

| Work Site | WorkClass | Cap | Notes |
|-----------|-----------|-----|-------|
| poll_primary_bake | Medium | 1 readback/frame | remaining stay in gpu.bake_pending |
| poll_dc_jobs | Medium | 2 results/frame | remaining stay in channel |
| mesh_cache.upload_pending | Medium | 2 uploads/frame | both imported + DC paths |
| poll_body_vol_cache | Medium | 2 results/frame | bake_rx try_recv loop |
| composite rebuild | Low | deferred during interaction | only runs when composite_dirty |
| volume upload | Low | partial path preferred | root cause fixed: dirty_range consumed prematurely |

### Interaction Render Policy (`InteractionEpisode`)
Episode-based pipeline + resolution latching. On interaction start: latch pipeline (lite/march/full) and resolution scale. No re-evaluation during episode. 8-frame cooldown after interaction ends. Eliminates frame-time variance from mode switching during orbit/drag.

### Pipeline Warmup (`WarmupManifest`)
All 13 pipelines compile on background thread. 6 core pipelines first (Raymarch, RaymarchLite, VolumePreview, DeferredLite, DeferredFull, RaymarchMarch), then 7 lazy (Fog, FXAA, Temporal, PrimaryBake, BrickFill, Pathtrace, PtDenoise). `ensure_*_pipeline()` functions are synchronous fallbacks if background compilation hasn't delivered yet.

---

## GPU Resource Ownership

GpuCtx sub-structs:
- **PipelineRegistry:** all ~22 compute/render pipelines + pipeline cache + compile state
- **BindGroupCache:** ~35 BGLs + bind groups + invalidation helpers
- **BufferPool:** ~22 GPU storage/uniform buffers
- Existing: `RasterPipeline`, `ShadowMapSystem`, `PickingPipeline`

**WarmupManifest** (gpu_init.rs): lists 7 lazy pipelines (fog, fxaa, pathtrace, pt_denoise, primary_bake, brick_fill, temporal) that must compile before interaction.

See `GPU_INVALIDATION_MATRIX.md` for the complete ownership and invalidation table.

---

## App Decomposition (Q2 2026)

FourthLayerApp subsystem structs:
- `self.scene: SceneState` — scene graph, prims, active body, mat_lib, pivot mode (scene_state.rs)
- `self.cache: CacheState` — volume caches, mesh cache, bake state, DC jobs (cache_state.rs)
- `self.project: ProjectState` — file path, auto-save, recent files (project_state.rs)
- `self.viewport.camera: CameraState` — orbit, distance, target (types_viewport.rs)
- Pre-existing: `self.tool`, `self.viewport`, `self.sel`, `self.outliner`, `self.ui`, `self.ai`

---

## File Structure

### Completed Splits
- `types.rs` → 20 files in `types/` module
- `ui_windows.rs` → 12 `win_*.rs` files
- `drawing.rs` → 10 `draw_*.rs` files
- `outliner_draw.rs` → 6 files (hub + body/light/poly/dnd/panel)
- `main.rs` → extracted `app_commands.rs` + `app_poll.rs`
- `project.rs` → `project_save.rs`, `project_load.rs`, `project_new.rs`

### Outliner Deferred Pattern
`OutlinerDeferred` struct — 22 `Option` fields for deferred actions. Draw phase sets `d.field = Some(...)` (no mutations). `process_outliner_deferred(d)` at end consumes struct and executes all. Never use `d` as local variable name (shadows the deferred struct).

---

## UI Art Direction
- Reference: MODO. Warm charcoal background, amber accent (#FF9E3D), violet for AI.
- DM Sans for labels, JetBrains Mono for values.
- Right panel (Outliner + Properties) ALWAYS visible.

---

## Platform
- Windows primary. NVIDIA RTX 4090, Vulkan backend.
- Pipeline cache: `%LOCALAPPDATA%/Azra/pipeline_cache.bin`
- Release: opt-level 3, LTO, codegen-units 1, panic abort.
- Dev: opt-level 2.

---

## Architecture Summary — Q2 2026

### Data Flow
User Input → `transaction()` → SceneState (graph) → `sync_flat_cache()` → GPU buffers → render

### Ownership
- Scene graph → bodies, lights, cameras, materials, groups
- `SdfBody` → `Vec<SdfPrimitive>` (prim array, body-local positions)
- `GpuSubsystem` → `PipelineRegistry`, `BindGroupCache`, `BufferPool`

### Scheduling
`FrameScheduler` → WorkClass priorities → per-frame budget → drain caps → stable interaction policy

### Volume Pipeline
edit → `mark_dirty` → bake dispatch → GPU bake → readback → `BodyVolumeCache` → DC → raster mesh
One mainline path (per `VOLUME_ADR.md`). Composite grid + SVO hybrid removed.

### Undo
`transaction()` for discrete actions. `begin_episode()`/`commit_episode()` for drags.
`auto_commit_episodes()` at frame end. `save_undo()` DELETED.
