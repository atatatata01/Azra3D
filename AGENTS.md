# AGENTS.md — Rules (Read First, Every Task)

Rules only. For system knowledge (how things work, current state), see ARCHITECTURE.md.

---

## Priority Ladder
1. **Crash / data corruption** — fix immediately
2. **Source-of-truth violations** — flat cache writes without graph sync
3. **Undo / selection / save-load regressions**
4. **Workflow breakage** — fix when touching the area
5. **Polish / perf / UI cleanup** — only if explicitly asked
6. **Opportunistic refactor** — never, unless it's the assigned task

---

## Non-Negotiable Rules

### Worktree Ownership — UI (Non-Negotiable) — NEVER on master
**UI / UX / shell redesign work is FORBIDDEN on the master checkout.** Agents and humans that ignore this pollute master and destroy parallel work.

| | |
|---|---|
| **Worktree path** | `F:\PROJECT\FourthLAyerAi\.Codex\worktrees\ui-polish` |
| **Branch** | `feat/ui-polish` only |
| **Run / build** | `Run_UIv2.bat` or `Dev_UI.bat` (never `Dev_Azra.bat` for UI v2) |
| **Canonical map** | `docs/WORKTREES.md` |

**Before ANY UI task, gate (all three must pass) — if any fails, STOP and switch:**
1. `git rev-parse --abbrev-ref HEAD` → must be `feat/ui-polish`
2. Working directory path must contain `.Codex\worktrees\ui-polish` (or `/ui-polish/`)
3. `git worktree list` shows this checkout is **not** the master root

**What counts as UI (must only land on `feat/ui-polish`):**
- `core_engine/app/src/ui2/**` (entire v2 shell)
- Theme / tokens / design system (`ui_theme.rs`, token boards, mockup parity)
- Dock / appbar / ctxbar / status / tools rail / library shell layout
- Panel chrome, egui style, UI-only menus tied to the v2 shell
- `docs/UI/**`, UI mockups, `UI_UX_DESIGN_RULES.md` content
- Any commit message with `ui2`, `feat(ui2)`, `fix(ui2)`, theme redesign, dock polish

**What is NOT this rule (may stay on master when it is a real product bug, not redesign):**
- Crash / data-corruption / undo / selection fixes that happen to touch a properties panel callback
- Tool math, SDF, poly ops, sculpt, bake, render — those use **their** worktrees when they have one (see `docs/WORKTREES.md`)

**Hard bans:**
- **Never** create or edit `ui2/` on master
- **Never** commit UI redesign to `master`
- **Never** merge `feat/ui-polish` → `master` without explicit owner approval
- **Never** put non-UI work (SDF/poly/sculpt/render) on `feat/ui-polish` — that is how UI polish gets contaminated (e.g. stray `fix(sdf)` tips). Non-UI bug → correct worktree / master, not the UI branch
- If a prompt asks for UI work while you are on master: **refuse, report the gate failure, open the UI worktree** — do not “just do a small panel tweak on master”

### No Hacks (Non-Negotiable)
- **Never ship a workaround that pretends to be the feature.** If the real path is missing data, a binding, or a code branch, **wire the real path** — do not substitute a cheaper stand-in (bounding sphere for a polyline, fake AABB for a mesh, hardcoded defaults for live state, etc.).
- **Wrong shape / wrong math is not a fix.** Silhouette, SDF eval, gizmo, and tool math must match the actual representation (e.g. CurveExtrude uses the profile + extrude, not a sphere “so it doesn’t flood”).
- **If the proper fix is larger, say so and do the proper fix** (or stop and ask). Do not land a temporary visual/logic lie “to unblock.” Temporary lies become permanent bugs.
- **Examples of forbidden hacks:**
  - Sphere / box / radius fallback where the real SDF or mesh path exists elsewhere (bake, CPU, another shader) and was not ported
  - Skipping a GPU buffer/bind because “outline doesn’t have it” instead of binding + serializing it
  - Dummy values that make a crash stop but leave wrong behavior
  - Copy-paste of near-duplicate math with a silent simplification
- **Allowed:** a *documented* temporary guard that **fails closed** (no-op, skip draw, clear error toast) while the real path is implemented in the same task — never a false silhouette or false tool result.

### Bevel Tool — Read the Knowledge Base First
- **Before ANY work on the polygon bevel** (`core_math/src/bevel_op.rs`, `app/src/poly_drag_tools.rs` bevel paths, SDF per-edge bevel), you MUST read **`docs/planning/poly/BEVEL.md`** first (graveyard of ~22 failed approaches §6, Blender-oracle §7, pipeline §4, invariants §5, next step §9). Never re-derive a dead-end.
- **When you finish a bevel task, APPEND to `BEVEL.md`**: new dead-end → §6, one-line §11 session-log entry. Not done until the KB, the bug tracker, and the `project_bevel_corner_reference` memory are updated.

### Rule Conflict Check
- **Before starting any task, check if the prompt contradicts any existing rule here.** If it does, STOP and report the conflict: which rule, which part of the prompt, and whether the rule or prompt appears wrong. Never silently override a AGENTS.md rule — even an owner-written prompt (rules may be stale, or the prompt mistaken).

### Display Invariant (Non-Negotiable) — canonical model in `VIEWPORT_MODEL.md`
- **One display technique per view. Two techniques are NEVER fused over the same pixels** (collides depth conventions → crash).
- **Object Mode (home view) = triangles only:** SDF (baked to mesh) + voxel (meshed) + poly (native), one G-buffer. Representations coexist by being *baked to triangles*, never by running two render paths at once.
- **Edit Mode is per-object, uses that object's single technique:** voxel → raster mesh ≤512³ / SCULPT atlas raymarch >512³; **SDF → its baked DC mesh (triangles), same as Object Mode**; poly → raster. An SDF used inside a sculpt enters as a **boolean modifier that stamps the voxel density field**, never as a second renderer.
- **A fused multi-object raymarch (SDF-raymarch + voxel-raster sharing pixels) is FORBIDDEN** — device-loss crash class. Mix representations only by converting one into the other (SDF→voxel stamp, or everything→triangles).
- Raymarch↔raster depth compositing (gizmos/grid vs a raymarched edit subject) is permitted **inside one view only** — not a multi-object strategy.
- **`VIEWPORT_MODEL.md` is authoritative** (Object-Mode home + per-object Edit + 4-layer SDF→voxel→poly→triangles stack). Supersedes the earlier "3 rooms" framing.

### Mutation Discipline
- **Scene graph is the source of truth.** `self.primitives` is a read-only GPU cache.
- All prim mutations go through `write_prim()` (closure) or `mirror_prim_to_graph()` (after egui widget mutation). Both write graph + flat cache atomically.
- **All mutations go through transaction() or episode transactions.** See Transaction Model below.
- **PropsContext is mandatory for SDF properties.** Never `self.is_dirty = true` in a properties callback — use `pctx.changed = true`.
- Every user-visible slider/toggle in properties panels MUST have undo. No exceptions.
- **Active analytical unit is always the whole top-level SdfBody subtree.** Never a partial mixed subtree.

### Voxel Sculpt System
*(The density-voxel sculpt — `voxel_sculpt.rs`, `VoxelObject`/`self.voxel`, sparse atlas `VoxelAtlasGpu`, Surface Nets, sculpt raymarch — is a SEPARATE subsystem from SDF bodies. These rules deliberately diverge from the SDF rules above.)*
- **Source of truth during a stroke is the GPU sparse atlas (`VoxelAtlasGpu`), NOT the CPU `BrickGrid`.** The CPU grid is a cache, rebuilt lazily (`cpu_grid_stale`) only for rare readers (save, resample, duplicate). INVERTS the "scene graph / CPU is truth" rule — scoped to atlas/sparse strokes only. The Dense (<256³) box path keeps CPU authority.
- **Sculpt undo is its own path** (GPU brick snapshots / CPU delta), NOT `transaction()`.
- **The atlas write is the render-relevant mutation.** A brush that writes the atlas in place MUST `mark_dirty(DirtyFlags::RENDER)` so `viewport_render()` re-dispatches the raymarch that frame. Do not rely on the mesh flush for live display (the "stroke appears only on release" bug — `input_sculpt.rs:1793`).
- **Display = raster mesh ≤512³ / atlas raymarch >512³**, auto by resolution with manual override. Never fused (see Display Invariant).
- Atlas buffer (re)creation + bind-group rebuilds obey the Frame Ordering Contract + generation counters.

### DirtyFlags Discipline
- **`SCENE` is the nuclear option** = structural changes ONLY: add/delete/reparent prim, boolean op change, body membership, undo/redo, project/file load. Sets `prim_buf_stale`, `light_gpu_data_stale`, `scene_modified`; triggers `process_dirty()` → `trigger_body_vol_bake()` + sculpt rebake. Never use as a convenience shortcut for "something changed."
- **`RENDER`** = view-only / visual state: camera move/orbit, isolate toggle, shading mode, selection highlight, resolution change. Only sets `is_dirty` (render gate). Does NOT re-serialize prims or bake.
- **`LIGHT`** = light property edits only. Sets `light_gpu_data_stale`. Also stale on SCENE (light_link_masks depend on prim count); SCENE alone does not otherwise imply LIGHT.
- **Never set SCENE for prim-level property edits** (shape, material, transform sliders). These go through `write_prim()` → `mirror_prim_to_graph()` → `mark_prim_dirty()`, which handles the bake. `rebake_no_undo()` sets SCENE for the prim buffer upload.
- **`mark_prim_dirty_at()` / `mark_prim_dirty()` auto-trigger body volume bakes** via `dirty_bodies` — callers do NOT need to also set SCENE. `process_dirty()` checks `dirty_bodies` independently.
- **Gizmo drag = dirty regions only.** `write_prim_transform()` calls `mark_prim_dirty_aabb()`, adding dirty regions to `dirty_bodies`; partial bakes update only the dirty sub-region. Debounce skipped when dirty_regions exist; full-body bakes debounce at 40ms.
- If you only need prim buffer re-upload, set `prim_buf_stale = true`. If you only need a bake, call `mark_prim_dirty_at()` / `mark_body_dirty()`.

### Prim Position Space
- **Prim positions are body-local.** `prim.position` is relative to the body's local frame (`body.origin`), NOT world space.
- Any code that projects prim positions to screen, builds overlays, computes world-space bounds, feeds a camera target, or feeds the GPU instance pipeline **MUST** apply the body transform first. **Common violation:** passing `p.position` directly to `world_to_screen()`.
- **Body↔world:** use `BodyTransform::local_to_world()` / `world_to_local()` from `scene_graph.rs`. Never inline the math.
- **Moving/rotating/scaling a body NEVER modifies prim positions.** It only changes `body.origin` / `body_rotation` / `body_scale`. Zero rebake.
- **Moving a prim changes its local position only.** Dirty region computed in local space.
- **`serialize_from_graph` applies body transform** — the GPU prim buffer holds world-space positions. Conversion happens at serialization time, not storage time.
- **Save format v7** (`app/src/project.rs:630` writes `version: 7`; corrected 2026-07-15 from a stale "v6"). Prims stored body-local. Materials stored as graph nodes. v5 auto-migrates mat_lib→graph. v4 auto-migrates prim positions. New authored state rides the `FL4ORDER`/serde-default side-channel rather than bumping the version — `project.rs:381`: "the version number stays 7 and this MAGIC is the only format-extension signal."
- **Feature flag `BODY_LOCAL_PRIMS: bool`** gates all body-local logic. `true` in production; flip to `false` to revert to legacy world-space for debugging.

### SDF Body Rendering — Triangles Only
- **SDF display is triangles-only in EVERY mode** — always from the baked DC mesh via the raster pass (Object Mode and while editing).
- **DELETED — never reintroduce:** `RenderMode::VolumePreview`, the VP pipeline, the 812³ `VolumeAtlas`, the `force_volume_only` flag, and `sdf_edit_active`. No volume-only render flag.
- BAKE infrastructure (`volume_bake.wgsl`, mode-2 sparse, GPU classify, `ResidentVolume.rv.buffer` field-direct DC) is unchanged and load-bearing.
- **Analytical SDF display is DELETED (2026-07-14) — never reintroduce.** The GLSL BVH path, the WGSL analytical march (`pipeline`/`pipeline_march`, `RenderMode::AnalyticalMarch`/`AnalyticalFull`), and the `force_analytical_debug`/`bvh_enabled` flags are gone. Only `pipeline_lite` (`AnalyticalLite` = floor + background) and WGSL path tracing remain as compute passes; SDF geometry comes from the baked DC mesh via the raster pass.
- **`write_prim_transform` marks dirty regions** via `mark_prim_dirty_aabb()`. Partial bakes update only the dirty sub-region; debounce skipped when dirty_regions exist; full-body bakes debounce at 40ms.

### Scope-Leave Bake Dispatch
- **Debounce bypass for scope-leave.** `trigger_bake()` has a 40ms debounce vs bake spam. When marking a body dirty on scope leave (switching `active_body_nid`), clear `last_dirty_time = None` so the bake dispatches immediately — scope-leave is one-shot, not a rapid-edit stream.
- **`needs_rebake_on_leave(nid)`** gates scope-leave rebake: fires only if grid is missing or at draft resolution. Bodies with valid full-resolution grids skip rebake on body switch (Tab) entirely.

### Structural Mutation Checklist
- **Any operation that changes prim membership in a body** (add, remove, reparent, group, dissolve, nest) **MUST** do ALL of these in order:
  1. Mutate graph (add/remove prims from `body_primitives_mut`)
  2. `recalculate_body_origin()` for ALL affected bodies
  3. `sync_flat_cache()` — copies compensated positions from graph to flat cache
  4. `mark_node_body_vol_dirty()` for ALL affected bodies — tells volume cache to rebake
- Missing step 2 before 3 → flat cache has stale positions (CR-71).
- Missing step 4 → volume cache retains ghost data from deleted/moved prims (CR-74).
- `mark_dirty(DirtyFlags::SCENE)` alone is NOT sufficient — it sets `prim_buf_stale` but does NOT mark bodies dirty in `BodyVolumeCache.dirty_bodies`.

### Build Discipline
- `cargo build -p app` after EVERY change, even 5 lines. If it breaks, fix IMMEDIATELY — don't pile changes on a broken state.
- `cargo test -p core_math` after any change to `half_edge.rs`.

### Scope Control
- **Fix ONE bug per task.** Don't refactor other things you noticed.
- **Prefer the smallest fix that respects the architecture direction.**
- **Refactor budget:** bug fix = max 1 local extraction + 1 fix. Architecture task = refactor only, no new features.

### Stable IDs, Not Indices
- Vec indices go stale after insert/delete/reorder. Use `prim_uid`, `light.id`, `NodeId`.
- Commands, transforms, delta undo all use stable IDs. Flat index resolution happens at execution time only, via `prim_index_by_uid()`.

### Unified Selection
- ONE selection system: `UnifiedSelection` = `IndexSet<SceneItemId>`.
- NEVER store flat prim indices in selection — use `SceneItemId::Prim(uid)`.
- Writers: `select_single_unified()`, `toggle_select_unified()`, `range_select_unified()`, `clear_selection_unified()`.
- Entering body edit scope clears unified selection.

### No Duplication — Search Before Writing
- **Before writing ANY new function, grep the codebase for existing implementations.** Report "searched for existing: found X at Y:Z" or "none found" before writing.
- **Canonical function registry:**
  - Duplication → `duplicate_subtree()` in `scene_actions.rs`
  - Camera rays → `self.cam_ray(mouse, rect)`
  - Prim creation → `create_prim()` in `creation.rs`
  - Selection writes → `select_single_unified()` / `toggle_select_unified()` / etc.
  - Deletion → `delete_prim()` / `delete_body()` in `scene_actions.rs`
  - Undo → `transaction()` (atomic) or `begin_episode()`/`commit_episode()` (drags)
  - Body origin recalc → `recalculate_body_origin()` in `scene_actions.rs`

### Scene Graph Is The Only Owner
- All scene objects (prims via body, lights, cameras, materials) live in the scene graph. No separate `Vec<T>` for any scene entity type.
- Selection uses `NodeId` for bodies/lights/cameras/materials, `PrimUid` for prims within bodies.
- Mutations go through `scene_graph.get_mut(nid)`. `SdfBody` owns its prim array (`Vec<SdfPrimitive>`).
- **SDF primitives are NOT individual graph nodes.** They live inside `SdfBody`'s prim array.

### Transaction Model
- Every user-visible mutation MUST go through `transaction()` (atomic) or episode transaction (`begin_episode()`/`commit_episode()` for drags). No exceptions.
- **Atomic:** `self.transaction("desc", |s| { s.write_prim(idx, |p| ...); });` — one-shot mutations (buttons, menu actions, keyboard shortcuts).
- **Episode:** `begin_episode()` on drag start, mutations each frame via `write_prim()`/`mirror_prim_to_graph()`, `commit_episode()` on release — ONE undo entry per drag.
- `auto_commit_episodes()` runs end-of-frame to catch leaked episodes (pointer released without explicit commit).
- `transaction_snapshot_only()` for cases where egui already mutated the field (captures pre-mirror state).
- **`save_undo()` and `save_delta_undo()` are DELETED — never reintroduce.**
- `write_prim()` and `mirror_prim_to_graph()` are SceneState helpers called inside transactions; they do NOT create undo entries themselves.

### Materials Are Referenced, Never Embedded
- Prims reference materials by `MaterialId` (u32 newtype). Material data lives in `NodePayload::Material` graph nodes.
- `SdfPrimitive.material_id: MaterialId` — `MaterialId::NONE` (0) = default material.
- Materials are EXPLICITLY ADDED by the user (owner directive 2026-07-17 — no auto "Default Material" node; resolvers fall back to embedded material on `MaterialId::NONE`, no-material renders warm-gray never black). GPU serialization resolves `material_id` → `PbrMaterial` from graph.
- `MaterialLibrary` is a serialization-only type (v4/v5 file compat). No runtime `mat_lib` field — scene graph is the sole runtime source of truth.
- Graph API: `scene_graph.add_material()`, `material_by_id()`, `material_by_id_mut()`, `materials()`, `remove_material()`.

### Frame Budget Rule
- **Never add unbounded per-frame work.** Any loop processing async results MUST have a `const MAX_*_PER_FRAME` cap. Current caps: bake readback ≤1, DC results ≤2, mesh uploads ≤2.
- **FrameScheduler** owns per-frame prioritization. Classes: Critical (always), High (budget permitting), Medium (capped), Low (deferred when busy), IdleOnly. Target 12ms CPU/frame.
- **InteractionRenderPolicy:** pipeline + resolution latched during interaction episodes. Zero path switches during interaction. 8-frame cooldown before quality re-evaluation.
- **All pipelines** compile on the background thread. `WarmupManifest` tracks readiness. `ensure_*_pipeline()` are fallbacks only.

### GPU Resource Ownership
- Every GPU resource (buffer, bind group, pipeline, texture) has ONE documented owner in GpuSubsystem. See `GPU_INVALIDATION_MATRIX.md`.
- Invalidation is event-driven: buffer realloc → specific bind groups invalidated.
- GpuCtx sub-structs: `PipelineRegistry`, `BindGroupCache`, `BufferPool` (plus `RasterPipeline`, `ShadowMapSystem`, `PickingPipeline`).

### Outliner Double-Click
- NEVER use `resp.double_clicked()` for outliner items — egui misfires on already-selected `selectable_label`.
- ALL outliner rename uses `crate::outliner_actions::check_double_click(ui, id)`. No exceptions.

### Tool Exit Convention
- **Right-click** = cancel + exit tool. **Exception: Knife** — RMB confirms.
- **Escape** = cancel + exit tool.
- **Enter** = confirm (knife, draw).
- **Switching tools** = confirm current work, then switch.

### Evaluator Over Ad-Hoc
- Any tool code needing SDF distance at a point MUST use `FieldEvaluator` (`core_math/src/field_eval.rs`). Do not inline SDF eval or traverse the prim list manually.
- `SceneEvaluator` wraps `&SceneGraph` — handles body transforms and boolean chains.
- DC extraction uses baked grids — not required to go through the evaluator.

### Volume ADR
- Before improving any volume/cache path, check `VOLUME_ADR.md` for the chosen mainline. Mainline = `BodyVolumeCache` + sparse/dense bake → DC mesh → raster.
- Do not invest in deprecated paths (composite grid, SVO hybrid). The analytical GLSL BVH path is DELETED (2026-07-14). Brick pool is PARKED.

### Feature Development Discipline
- Every feature prompt must include the standard rules block (`FEATURE_RULES.md`).
- Before starting, answer the 4 questions in `FEATURE_RULES.md` (new scene object? mutates state? frame-thread work? new render path?). After completing, run its regression checklist.
- One feature per prompt, one prompt per agent.

### No Bandaids — Production DCC Standard
AZRA targets production DCC quality (Blender/Modo/Substance class). A fix that "works for now" but creates future problems is debt with interest.

**The Test (answer ALL before committing any fix):**
1. **Can this class of bug ever happen again?** If yes → fix the architecture, not the instance.
2. **Does this fix hide a problem instead of solving it?** (assert→log, try/catch, special-case `if` around a crash = hiding.) If the underlying violation still occurs, the fix is wrong.
3. **Will the next dev need tribal knowledge to avoid breaking it?** If yes, the fix is wrong — enforce correctness structurally.
4. **Does this fix scale?** If 10 more bodies / 50 more prims / 3 more render paths would break it, it's not a fix.
5. **Does this fix compose with the architecture?** If it bypasses `transaction()`, ignores FrameScheduler, skips InteractionEpisode, or works around a dirty flag — it fights the architecture.

**Patterns that are ALWAYS bandaids:**
- Downgrading `assert`/`debug_assert!` to `log`/`eprintln!` — fix the violation, or delete the assert with a comment. Never downgrade.
- Timing delays / frame skips to dodge a race — fix the ordering, not the timing.
- Special-casing one caller (`if caller == "gizmo" { skip }`) — fix the validation or the caller.
- Clamping output to hide NaN/Inf — fix the input or the math.
- Cloning to escape a borrow-checker fight — restructure (read → decide → drop borrows → write).
- "Works on my machine" magic constants — use adaptive computation or a tweak param.
- `catch_unwind` around internal code — only OK for user-generated input (node graphs, scripts, imports), never internal logic.
- Wrapping in `Option` and silently returning `None` where a value should always exist — assert instead.
- Two code paths for the same operation — they MUST share one path (e.g. ALT+LMB drag routes through `start_gizmo_drag(Center)`, not a separate path).

**When writing a bandaid — STOP.** Ask: (1) actual root cause? (2) which layer should prevent this? (3) does an existing system (`transaction`, dirty flags, `FrameScheduler`, `InteractionEpisode`, generation counters) handle it? (4) else, is this a new architectural gap needing a proper primitive? If you can't answer #1 with confidence, write a DIAGNOSTIC prompt first, not a fix.

**The Bar:** every fix should be one a senior engine dev at a AAA studio would approve in review. "It stops the crash" is not approval-worthy; "it eliminates the class of bug by enforcing the invariant structurally" is.

### Single Responsibility Functions + Mandatory Reuse
- Every function does ONE thing. If it does two, split it.
- **REUSE RULE (every task):** before writing ANY new function, grep for existing ones that do the same/similar thing and report what you found. Extend an existing function with a parameter instead of duplicating; if you must create new, explain why existing don't fit.
- A function used by multiple systems (noise, SDF eval, transform math, ray casting) lives in a SHARED module (`core_math`, `scene_graph`, or a utils file) — never inlined in the caller.
- If you find duplicated logic during any task, flag it for extraction even if extraction isn't the task.

---

## Shader Rules

### Split Shader Architecture
- `compute.wgsl` is DEAD code — never compiled into any pipeline.
- SDF shared code is split into 5 files, concatenated at build time by `sdf_shared_all()`:
  - `sdf_shared.wgsl` — header: GpuUniforms, utilities, core bindings
  - `sdf_prim_eval.wgsl` — prim accessors, SDF shapes, modifiers, eval_prim, booleans
  - `sdf_scene.wgsl` — scene_analytical + scene_sdf, scene queries (RAY_CULL region + the analytical march were REMOVED 2026-07-14)
  - `sdf_material.wgsl` — shadow/AO, RNG, material evaluation
  - `sdf_lighting.wgsl` — shading, lights, atlas, environment
- Real deferred: `build_deferred_wgsl()` = `sdf_shared_all()` + `deferred_entry.wgsl`. `build_raymarch_lite_wgsl()` = the self-contained floor+background lite pass. (`build_raymarch_wgsl` / `raymarch_entry.wgsl` — the analytical SDF march display — were REMOVED 2026-07-14; SDF renders from its baked DC mesh via the raster pass.)
- Edit SDF eval → `sdf_prim_eval.wgsl`; scene queries → `sdf_scene.wgsl`. NEVER edit `compute.wgsl`.

### Shader Changes Are Expensive
- Any of the 5 `sdf_*` files invalidate 4+ pipeline cache entries → 25-43s cold recompile. Batch ALL changes into ONE edit.

### GLSL/SPIR-V + BVH Rules — REMOVED 2026-07-14
- The GLSL/SPIR-V BVH path (`graphics/src/glsl/*`, `glsl_pipeline.rs`, `raymarch_bvh*`, `pathtrace_bvh/primary/bounce*`, `sdf_functions.glsl`, `bvh_traversal.glsl`) and the WGSL analytical march were DELETED (dead — `bvh_enabled`/`force_analytical_debug` were constant-false). Do NOT reintroduce a GLSL BVH pipeline, a `.spv` passthrough path, or `build_raymarch_wgsl`/`build_raymarch_march_wgsl`. **Path tracing is WGSL-only** (`pathtrace_entry.wgsl` → `build_pathtrace_wgsl` → `pt_pipeline`).

### March Loop Safety
- **Tetrahedron normals:** `calc_normal()` in `sdf_scene.wgsl` uses 4-eval tetrahedron normals, not 6-eval central differencing. Don't revert. (The over-relaxation march guard was retired with the analytical march display path; `scene_analytical`/`scene_sdf` remain for deferred reflections + fog.)

### Feature Must Work in Both Paths
- Any SDF feature must work in BOTH eval paths (port in the same task — features don't propagate automatically):
  1. **WGSL analytical eval** — `sdf_prim_eval.wgsl` `eval_prim` (deferred, fog, PT, prim outline; `scene_analytical`/`scene_sdf` in `sdf_scene.wgsl`)
  2. **GPU bake** — `volume_bake.wgsl` (DC mesh extraction — the production SDF display)
- The CPU evaluator (`sdf_math.rs` / `field_eval.rs`) must match both.

### Noisy Blend WGSL Restriction
- `apply_boolean_blend()` must NEVER appear in compiled WGSL. All WGSL builds call `neutralize_blend_noise_wgsl()` which strips the `// #region NOISY_BLEND` block and injects a lightweight stub, before `create_shader_module`.
- Naga inlines all WGSL calls; the chain (`apply_boolean_blend` → `smin_noisy` → `eval_blend_noise` → `fbm_wgsl`) expands exponentially → GPU hang / TDR (0xcfffffff). Keep the stub. (Noisy boolean blends were only ever evaluated in the deleted GLSL BVH path, so they no longer render at runtime — do NOT unstub the WGSL chain to "restore" them.)
- `fn fbm_wgsl` stays in WGSL — used directly by surface noise modifiers, not part of the exponential chain.
- The 4 noisy blend functions MUST be wrapped in `// #region NOISY_BLEND` tags in every file containing them (`sdf_prim_eval.wgsl`, `volume_bake.wgsl`). Non-contiguous functions use separate region blocks with the same name — `strip_regions()` handles this.

### Shader Parity
- After ANY shader change: `cargo test -p graphics` (all Naga parse tests must pass).

### Shader Region Tags
- Any variable/function defined inside a `#region` tag MUST have ALL references inside the same tag — else stripping causes parse errors. After adding a reference to a region-scoped var, verify it compiles with ALL build variants (deferred-full, deferred-lite, raymarch-lite, fog, PT, prim-outline).

### G-Buffer Contract
- All 7 G-buffer outputs must be written every frame. Skipping both the raymarch-lite pass and deferred leaves uninitialized textures that crash post-processing.
- Binding numbers defined ONCE in `graphics::gbuffer_format` (Rust constants); WGSL `@binding(N)` literals must match.
- Adding/changing a slot: update `gbuffer_format` constants, then the WGSL writers (raymarch-lite, deferred), verify bindings match. Channel packing documented in `graphics/src/lib.rs` — keep the comment table in sync.

### BGL Single Source
- Bind Group Layout entries for raymarch, deferred, volume_preview defined ONCE in `graphics::bgl` (`raymarch_bgl_entries()`, `deferred_bgl_entries()`, `vol_preview_bgl_entries()`). Startup and background thread call the same functions — NEVER duplicate inline.
- Adding a binding to a shader: update the corresponding `*_bgl_entries()` in `bgl.rs`.

### Blend Dispatch (Texture Compositing)
- Any **new** feature blending two rendered results uses `dispatch_blend()` with the `composite_blend` pipeline. Don't inline blending into existing shaders. Existing inline blending (VFX in deferred, floor grid) stays.
- BlendModes: AlphaOver, Additive, Multiply, ReplaceHit.
- `overlay_texture` (GpuCtx) is the standard scratch target; `blend_temp_texture` is for in-place patterns.
- CONSTRAINT: base and output textures MUST differ — no in-place read+write in compute. Use `blend_temp_texture` for in-place.
- Blend runs AFTER deferred+fog, BEFORE bloom, inside the `!skip_deferred_pp` guard.
- Two-phase API: `prepare_blend()` (mutable, ensures pipeline + uploads params) then `dispatch_blend()` (immutable, borrows views).

### GPU Timestamp Query Safety
- `tw!()` marks a pass slot active. If the pass is then skipped, resolving unwritten queries causes device loss on NVIDIA. Only call `tw!()` for passes that actually dispatch.

---

## Geometry Rules

### Per-Edge Bevel Corner Rule
- **Never bolt a 3D corner sphere onto per-edge bevel via `max()`** — stripe artifacts (proven broken, reverted 2026-03-22).
- Same radius on all groups → `sdRoundBox`. Different radii → smooth interpolation, not hard `max()`.

### Body Mirror AABB Rule
- When computing scope AABBs for bodies with `mirror_flags != 0`, expand AABB to cover the mirrored region. Tight AABB kills the mirrored half.

### Half-Edge Mesh
- `he.vertex` = DESTINATION, not source.
- Never assume `he.prev` is valid — check for NO_EDGE.
- HashMap iteration = random order. Sort when order matters.
- `normalize_or_zero()` always, never bare `normalize()`.

---

## File & Code Rules

### Frozen Files
- Do not modify unless the task explicitly targets them: `shader_nodes.rs`, `compute.wgsl`. (`half_edge.rs` is NO LONGER frozen — see below.)

### Poly Kernel Decision — HEX-BL "BMesh-Lite" (`POLY_FOUNDATION_PLAN.md` is authoritative)
- **Chosen kernel: grow the proven typed-ID half-edge kernel IN PLACE into a BMesh-Lite.** Do NOT rewrite, fork bmesh-rs, SoA-migrate `HVertex.position`/`HFace.material_id`, or retype any serialized field. `bmesh-rs` is **STUDY-ONLY** (never a dep).
- **Load-bearing principle:** ALL new poly state is RUNTIME state on a non-serialized `PolyMesh` wrapper (`core_math/src/poly_mesh.rs`). `HalfEdgeMesh` and `Vec<PolyModifier>` are NEVER extended in-place. New authored persistence rides IN-BLOB `#[serde(default)]` fields on `ProjectFile` (the `voxel_grids`/`VoxelNodeRecord` precedent) — NEVER a second trailing magic after `FL4ORDER` (breaks `project.rs:356`'s `deserialize(tail)`), NEVER a `HalfEdgeMesh` field, NEVER `#[serde(skip)]` on `PolygonalBody.mesh`, NEVER a new `NodePayload` variant (reuse index-6 `PolygonalBody`).

### `half_edge.rs` — Editable for the Poly Pillar (freeze lifted 2026-07-06, owner-directed)
- **Blanket freeze is GONE.** Normal editable file for poly work — add topology ops (loop cut / edge split / dissolve / join), Euler ops, accessors, fix bugs directly. No gate.
- **Prefer real half-edge ops that PRESERVE stable vertex/edge/face IDs over `from_polygons` polygon-soup rebuilds.** A rebuild loses IDs, provenance, disk-stable UIDs — last resort, not default (prime suspect for "loop cut drops a random vertex").
- **The ONE hard red line (NEVER lifted — save-format correctness): do NOT change `HalfEdgeMesh`'s serialized shape.** No retyping/reordering/removing any bincode field on `HVertex`/`HFace`/`HalfEdge`/`HalfEdgeMesh`; no SoA migration of `HVertex.position`/`HFace.material_id`. Changing on-disk layout corrupts EVERY existing `.fl4`. New authored state still prefers the `PolyMesh` wrapper / app layer or in-blob `#[serde(default)]` fields.
- **The bar:** `cargo test -p core_math` after any change; keep `validate()`/`classify()` invariants green; a topology op that could produce non-manifold/orphaned-twin MUST assert against it.

### No Hardcoded Paths
- Never `C:\Users\...`. Use `std::env::temp_dir()`, `dirs::config_dir()`, `%LOCALAPPDATA%`.

### File Organization
- Never add `pub(crate) fn` to `main.rs`.
- New functions: scene → `scene_actions.rs`, prims → `prim_actions.rs`, outliner → `outliner_actions.rs`.
- >500 lines → ask if multiple responsibilities.

### GPU Safety
- `map_async` calls must capture `Result` via `mpsc::channel`. Never discard with `|_| {}`.
- `bincode::serialize` on prim-containing types: dedicated thread with 8MB stack (280-float prims overflow 1MB default).
- **Serialized enums (bincode) — ONLY append new variants at the END; never insert mid-list or reorder.** bincode tags variants by positional index; inserting shifts every later index and corrupts EVERY existing save file. **`NodePayload`'s tail is `… Empty(9), VoxelBody(10), SdfObject(11)` — `SdfObject` is LAST; append new variants AFTER it.** (Corrected 2026-07-15: this rule used to say "VoxelBody must stay last", which stopped being true when SdfObject was appended. Obeying the stale text — moving VoxelBody back to the end — would shift `SdfObject`'s tag and corrupt every save containing one, i.e. the rule would have caused the exact disaster it exists to prevent. The authoritative statement is the doc comment on `VoxelBody` in `core_math/src/scene_graph.rs`, which was correct all along — trust it over this line if they ever disagree again.)
- When recreating ANY buffer, invalidate ALL bind groups referencing it.

### Frame Ordering Contract (Non-Negotiable)
- **GPU resource mutations** (texture/buffer create/replace, atlas grow/resize) MUST happen in `poll_background_tasks()` — NEVER during egui panel draws or input handlers.
- **Bind group creation** happens in `viewport_render()`, AFTER all mutations. **GPU dispatch** (deferred, tonemap, etc.) happens AFTER bind group creation in the same call.
- **Frame order:** `poll_background_tasks` → egui panels → `viewport_render` (bind groups → dispatch → submit).
- **If a panel needs a GPU mutation** (e.g. atlas resize from a resolution button), set a deferred flag (`cache.atlas_rebuild_xy = Some(new_xy)`) and process it in `poll_background_tasks` next frame. NEVER call `device.create_texture()` / `VolumeAtlas::new()` in a panel callback.
- **The invariant:** when any bind group is created, ALL resources it references are final for this frame.

### GPU Resource Lifetime — Generation Counters Over Timing Tricks
- **Every recreatable GPU resource (textures, buffers) MUST have a generation counter.** On replace, increment; consumers compare cached vs current — mismatch = invalidate bind group / re-acquire. Pattern: `resource.generation += 1` on recreate; `if cached_gen != resource.generation { invalidate(); cached_gen = resource.generation; }` on use.
- **Never rely on frame delays / "next frame" logic to handle resource replacement.** If replaced in frame N, the bind group must detect it in frame N. Timing tricks (skip N frames, defer to next poll) are acceptable ONLY for perf optimization when correctness is already guaranteed by generation counters.
- Current implementations: `VolumeAtlas.texture_generation` (atlas 3D texture), `AtlasSlot.generation` (per-slot stale bake detection).

---

## Good Practices

### Architecture-First Thinking
- Ask "what is the simplest solution to the actual need?" before designing systems.

### Brick Pool Status
- **Brick pool: PARKED.** Renders directly from the baked 3D texture — no DC mesh extraction needed. 23 tracked bugs (`BRICK_CACHE_BUG_AUDIT.md`). Do not invest until explicit user request.

### Analytical Path Status — DELETED 2026-07-14
- **The analytical SDF display path (GLSL BVH + WGSL march) is GONE, not merely disabled.** `bvh_enabled` and `force_analytical_debug` no longer exist; `graphics/src/glsl/`, `glsl_pipeline.rs`, `raymarch_entry.wgsl`, `raymarch_march.wgsl`, the RAY_CULL region, `pipeline`/`pipeline_march`, and `RenderMode::GlslBvh`/`AnalyticalMarch`/`AnalyticalFull` were all removed. SDF renders ONLY from its baked DC mesh via the raster pass (Display Invariant). Never reintroduce an analytical/BVH render path. Path tracing is WGSL-only (`pt_pipeline`).

### Feature Dependency Audit (Non-Negotiable)
- **Before disabling ANY system (render path, raster pass, buffer upload, picking pipeline), audit ALL dependents.** Grep every reference; list every caller; verify each has an alternative or is explicitly OK to lose.
- **Disabling a render pass disables its side effects.** The raster pass provides floor grid, depth buffer, GPU node picking, shadow maps — disabling removes ALL of them.
- **Selection must ALWAYS work.** Breaking click-to-select is a P0 regression. Test selection after every render pipeline change.
- **When replacing a system, implement the replacement BEFORE removing the original.** Never leave a functionality gap for even one commit.

### Input System Discipline
*(Rewritten 2026-07-15, owner-ratified. The old rule named `InputSystem` — which has **zero** persistence (grep `save|load|serde|json` in `input_system.rs` = 0), is invisible to the rebind editor (`win_settings.rs:131` iterates `ACTION_DEFS` only; 0 mentions of InputSystem), and holds 22 registrations to `ACTION_DEFS`' 97. Obeying it produced shortcuts users could never rebind — the inverse of the goal. Plan: `docs/planning/input/INPUT_ARCHITECTURE_PLAN.md`.)*
- **Every NEW keyboard/scroll/mouse action MUST be registered in `ACTION_DEFS` (`app_types/src/keybindings.rs`)** — the registry that persists (`~/.azra/keybinds.json`) and appears in the rebind editor. Add the `ActionDef` + a `default_bind` arm; dispatch via `kb.pressed(ACT_ID, i)`. Do not add new `if ctx.input(|i| i.key_pressed(...))` blocks to `handle_keyboard()`.
- **`InputSystem` is being retired.** Do not add to it. Its `InputContext` idea survives into the new model; the struct does not.
- **Check the chord is free before binding it.** 15 of 94 default binds collide today (`Ctrl+Shift+S` = Save-As + Symmetrize, fixed 2026-07-15; `Ctrl+I` = Instance + Invert Selection, open). `find_conflicts` compares `context` prose with `!=` and catches neither — it has false positives AND false negatives, which is how all 15 shipped. Until the plan lands, grep `default_bind` by hand.
- **Same key + different context is CORRECT, not a bug.** `F` = Focus and Poly Fill; `E` = Elongate, Extrude, Poly Extrude. Never "fix" a legitimate context split into a unique chord. A real collision is two actions that can fire in the SAME frame in the SAME state.
- **Keys round-trip to disk structurally — do not hand-maintain key tables** *(rewritten 2026-07-16; the fix landed, this rule's old advice is now actively wrong)*. There is ONE table, `KEY_ALIASES` (`keybindings.rs:50`), holding only AZRA's compact labels (`Esc`/`Del`/`Bksp`/arrows — also the v2 on-disk spellings). Display and parse both derive from it and share ONE total fallback: `key_name` (`:62`) → egui's `Key::name()` (exhaustive, no sentinel) and `key_from_str` (`:76`) → `Key::from_name()`. **A key absent from the table still round-trips** — so binding a new key needs NO edit here, and the old instruction to "verify it is spelled in both tables" describes tables that no longer exist. `key_display` is presentation-only: **never persist it** — persisting a display string is what caused the bug (`_ => "?"` was written to disk for 51 of 104 keys, then read back as None → rebind silently lost on restart). The `every_key_round_trips_*` tests iterate `egui::Key::ALL`, so an egui upgrade that adds keys is covered automatically. *(Historical note: this rule used to claim `ctrl(Plus)`/`ctrl(Minus)` "ship broken" — they did NOT. `save()` only writes non-default binds, so defaults never reached the serializer; their `Ctrl+?` was cosmetic. The real loss was a USER's rebind.)*
- **Key mapping is LOGICAL — the printed letter, not the physical position** (owner decision 2026-07-15; Blender uses physical, we deliberately do not). Prefer `Event::Text` over `key_pressed(Key::NumN)` for digits.
- **Context-first:** always state the context. `Global` is the fallback — prefer `Viewport`/`Outliner`/`Properties`/`Library`.
- **Action IDs are snake_case strings**; category matches the UI section (e.g. `"hierarchy_up"` in `"Hierarchy"`).
- **Old actions migrate gradually** — when touching an old keybind for any reason, migrate it then. Do not batch-migrate.
- **EXEMPT — do not "fix" these:** the ~29 dialog-local `Esc`/`Enter` literals inside modal windows. They are window-scoped, not app actions. Without this exemption the rule reads as violated 29 times and the next dev cannot tell intent from debt.

### Vision Doc
- **Vision doc is reference, not roadmap.** `Nexux4D.pdf` captures product vision/positioning; it does NOT drive priorities. Priorities come from `MASTER_WORK_LIST.md` and `COMPLETE_BUG_TRACKER.md`. AI integration is gated on a solid foundation (MASTER_WORK_LIST P0–P1).

### Subsystem Map (where things live)
- `AzraApp` decomposes into: `scene` (SceneState — graph, prims, active body, pivot), `cache` (CacheState — volume/mesh caches, bake state), `project` (ProjectState — path, auto-save), `viewport`/`viewport.camera`, `tool`, `sel`, `outliner`, `ui`, `ai`, `gpu` (GpuCtx: `PipelineRegistry`, `BindGroupCache`, `BufferPool`, `RasterPipeline`, `ShadowMapSystem`, `PickingPipeline`).
- The Q2-2026 architecture work is **done** — history lives in `ARCHITECTURE.md`; this file keeps only live rules.

### Reference Documents
- **GPU_INVALIDATION_MATRIX.md** — GPU resource ownership, invalidation triggers, rebuild policy
- **VOLUME_ADR.md** — volume representation decision criteria (confirmed Week 9)

### Definition of Done
1. Build passes
2. Bug no longer reproduces
3. Affected callers checked (grep all usages)
4. Undo / save-load impact considered
5. One manual test scenario described

### Bug Tracker
- Every bug fix prompt must include instruction to update `COMPLETE_BUG_TRACKER.md` — mark fixed items ✅ FIXED + date, add new items if untracked.

---

## Build & Run

```bash
cd core_engine && cargo run -p app           # dev (opt-level 2)
cd core_engine && cargo run -p app --release # release
cd core_engine && cargo test -p core_math    # mesh tests
```

### Cargo Workspace (core_engine/)
- **app** — main application, egui/eframe, all UI + tools + AI
- **core_math** — pure math: SDF, half-edge mesh, NURBS, texture gen (no GPU)
- **graphics** — wgpu GPU layer, shaders, uniform structs
- **io_bridge** — file I/O, mesh import/export
- **render** — rasterization pipeline, mesh cache, shadows
- **cuda_render** — optional CUDA path tracer

### App Identity
- Product name: AZRA. Data: `%LOCALAPPDATA%/Azra/`. Struct: `AzraApp`.
