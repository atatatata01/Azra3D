# App-Crate De-God Plan (Gated, Behavior-Preserving)

> **Generated:** 2026-07-02 by the MASTER worktree via a 19-agent analysis fleet (10 per-file responsibility maps + 8 runtime crash-risk audits + 1 synthesis pass).
> **Scope:** the overloaded `core_engine/app` crate (~67k lines) — the "overloaded shell."
>
> **Contract for every slice below (non-negotiable):**
> - Each slice is a **PURE CODE RELOCATION** — no logic change, no behavior change, no opportunistic dedup.
> - `cargo build -p app` must pass after **each** slice; one responsibility per slice; small & reviewable.
> - Never add `pub(crate) fn` to `main.rs`. `half_edge.rs` is FROZEN. `voxel_sculpt.rs` is core_math (sculpt-owned), not an app-crate slice.
> - The **Owner worktree** column assigns each slice to one of the 4 conversations (`master` / `poly` / `sculpt` / `general`) so they don't collide on the same files.
>
> This is REFERENCE for execution — run slices **one per prompt, gated, in order**. Raw per-file analysis data lives in [`APP_CRATE_DEGOD_DATA.json`](./APP_CRATE_DEGOD_DATA.json).
>
> **At a glance:** 10 god-files mapped, 54 extraction moves proposed, 4 real runtime crash-risks found across 8 audited files.

---

# AZRA De-God-Code Plan — `app` Crate

## 1. Executive Summary

The `app` crate is ~67k lines with a ~200-field `FourthLayerApp` god-struct spread across ~130 sibling `impl AzraApp` files, and eight oversized files (main.rs, app_poll.rs, viewport_render.rs, input_sculpt.rs, gizmo.rs, input_viewport.rs, voxel_gpu_mesh.rs, sdf_gpu_mesh.rs, raster_render.rs, input_keyboard.rs) each fusing 4–12 responsibilities. The plan is **28 gated pure-relocation slices** ordered low-risk-first: the two GPU-plumbing files (voxel_gpu_mesh, sdf_gpu_mesh) and the free-function extractions are byte-for-byte moves; the mega-methods (`update()`, `dispatch_gpu_render`, `handle_viewport_input`) are held **whole** because their interiors are one long-lived `&mut self.gpu` borrow that cannot be sub-split without a logic-changing restructure — those get a single guarded extract-method step, not a carve-up. Four **live per-frame UTF-8 byte-slice panics** in `app_poll.rs` (AI/Claude error toasts) are the top crash-hardening priority and are worktree-independent one-liners.

Slices are assigned to the four worktrees by ownership so the conversations never touch the same file: **master** owns main.rs/viewport_render/sdf_gpu_mesh/raster_render, **general** owns app_poll/gizmo/input_viewport/input_keyboard, **sculpt** owns input_sculpt/voxel_gpu_mesh, **poly** has no god-file (participates only via the shared-crate coordination rules in §5).

---

## 2. Ordered De-God Slices

Ordered by dependency and risk (enabling low-risk moves first; the two forced extract-method steps last). Every slice is `cargo build -p app`-gated. "New module" = new `mod` line in main.rs (never a `pub(crate) fn` in main.rs).

| # | Slice | Target file(s) | What moves | New module(s) | Owner | Risk | Verify |
|---|-------|----------------|------------|---------------|-------|------|--------|
| **CRASH FIXES (do first, independent of relocations)** |
| C1 | Char-safe AI/Claude error toasts | app_poll.rs:497,513,568,569 | Replace 4 `&s[..n.min(80/60)]` byte slices with `.chars().take(N).collect::<String>()` | — | general | low | build + paste a >80-char accented Claude error → no panic |
| **PURE-RELOCATION GPU-PLUMBING (zero borrow entanglement, whole-type moves)** |
| 1 | Voxel GPU brush split | voxel_gpu_mesh.rs | `BrushParams`+`pack_stamp_plan`, `gpu_stamp_plan`, `VoxelBrushGpu`(+impls), `VoxelBrushInflight` | `voxel_gpu_brush.rs` | sculpt | low | build; check ~24 `crate::voxel_gpu_mesh::*` sites still resolve via facade re-export |
| 2 | Voxel GPU undo split | voxel_gpu_mesh.rs | `BRICK_SLOT_BYTES`, `AtlasUndoEntry`, `AtlasUndoRing`, `AtlasUndoBuilder`, `snapshot_current` | `voxel_gpu_undo.rs` | sculpt | low | build; facade adds `pub(crate) use voxel_gpu_brush::*; ...undo::*;` |
| 3 | SDF DC pipeline plumbing | sdf_gpu_mesh.rs → dir module | `DcParamsGpu`+`dc_params/_sparse/_caps`, `build_dc_pipelines`, `dc_bind_group`, `dummy_storage`, `encode_dc_passes` (widen priv→`pub(crate)`) | `sdf_gpu_mesh/dc_pipeline.rs` | master | low | build; convert `sdf_gpu_mesh.rs`→`sdf_gpu_mesh/mod.rs` |
| 4 | SDF readback layer | sdf_gpu_mesh/ | `dispatch_to_staging`, `PendingSdfReadback`(+impl), `dispatch_readback`, `mesh_sdf_grid_gpu` | `sdf_gpu_mesh/readback.rs` | master | low | build; re-export `PendingSdfReadback` so `cache_state.rs:110` resolves |
| 5 | SDF resident mesher | sdf_gpu_mesh/ | `SdfMeshGpu`(+impl), `SdfBodyMeshSlot`(+new) | `sdf_gpu_mesh/resident.rs` | master | low | build; re-export both so main.rs:794 / cache_state.rs:75 / app_poll resolve |
| 6 | SDF DC parity tests | sdf_gpu_mesh/ | headless_device + all `sdf_dc_*` tests + sparse helpers | `sdf_gpu_mesh/tests_dc.rs` (`#[cfg(test)]`) | master | medium | `cargo test -p app` (35 tests still pass) |
| 7 | SDF bake tests | sdf_gpu_mesh/ | `partial_bake_writes_exact_footprint`, `body_mirror_bake_matches_cpu_eval` (+headless_device copy) | `sdf_gpu_mesh/tests_bake.rs` | master | medium | `cargo test -p app` |
| **PURE-RELOCATION `impl AzraApp` METHOD MOVES (inherent-impl split; no re-exports)** |
| 8 | Poll: pipeline compile hot-swap | app_poll.rs:262-439 | `poll_sdf_compile` | `poll_pipeline_compile.rs` | general | low | build |
| 9 | Poll: AI channels | app_poll.rs:442-573,2479-2510 | `poll_agent/_claude/_ai_service/_ai_texture` | `poll_ai.rs` | general | low | build (needs `use crate::claude::ClaudeKind;`) |
| 10 | Poll: misc drains | app_poll.rs:2448-2606 | `poll_gpu_jobs`, `handle_pending_pick`, `poll_export`, `poll_warming` | `poll_misc.rs` | general | low | build |
| 11 | Poll: voxel atlas undo restore | app_poll.rs:2618-2672 | `voxel_apply_atlas_restore`, `apply_one_atlas_restore` | fold into `voxel_gpu_mesh.rs` **(sculpt-owned)** | sculpt | low | build — **see §5 cross-worktree note** |
| 12 | Poll: DC + GPU-mesh path | app_poll.rs:664-899,901-1124 | `gpu_mesh_to_cache`, `poll_sdf_mesh_jobs`, `sdf_populate_resident`, `dispatch_grid_dc_async`, `poll_dc_jobs` | `poll_dc_mesh.rs` | general | medium | build |
| 13 | Poll: SDF bake pipeline | app_poll.rs:576-654,1128-1903,1908-2274,2280-2444 | `poll_body_vol_cache`, `update_resident_volume`, `dispatch_pending_primary_bakes`, `cpu_fallback_bakes`, `poll_primary_bake`, `dispatch_brick_fill` | `poll_bake.rs` | general | medium | build; grep fn-local `use` moved with fns |
| 14 | Viewport: render-mode decision | viewport_render.rs:10-120 | `resolve_render_mode` free fn + `GpuReadiness`/`RenderModeInputs` + `DRAFT_*` consts | `viewport_render_mode.rs` | master | low | build |
| 15 | Viewport: auto-mode policy | viewport_render.rs:2039-2094 | `update_viewport_mode_auto` | `viewport_render_mode_auto.rs` | master | low | build |
| 16 | Viewport: CUDA dispatch | viewport_render.rs:1910-2037 | `dispatch_cuda_render` (`#[cfg(feature="cuda")]`) | `cuda_render_dispatch.rs` | master | low | build **with and without** `--features cuda` |
| 17 | Raster: mesh provisioning | raster_render.rs:102-348 | `poll_mesh_cache`, `ensure_body/poly/nurbs_meshes`, `upgrade_mesh_lod`, `body_aabb_bounds` | `raster_mesh_prep.rs` | master | low | build (keep `self.poll_sdf_mesh_jobs()` cross-call) |
| 18 | Raster: shared draw helpers | raster_render.rs:40-99 | `draw_resident_pool[_with_instance]`, `draw_sdf_resident_slot`, `body_aabb_bounds` → widen priv→`pub(crate)` | `raster_common.rs` | master | low | build |
| 19 | Raster: G-buffer pass | raster_render.rs:355-929 | `dispatch_raster_pass` (as `impl AzraApp`, **not** free fn) | `raster_gbuffer.rs` | master | medium | build |
| 20 | Raster: picking | raster_render.rs:936-1205 | `pick_node_at`, `poll_pending_pick` | `raster_pick.rs` | master | medium | build; **selection must still work** (P0 manual test) |
| 21 | Raster: scene-authoring stragglers | raster_render.rs:1209-1301 | `select_scene_node`, `deselect_scene_node`, `import_gltf_dialog` → append into existing `scene_actions.rs` | scene_actions.rs | master | low | build |
| 22 | Gizmo: scene-edit commands | gizmo.rs:197-554 | `delete_selected`, `duplicate_selected`, `duplicate_scene_node`, `duplicate_voxel_object`, `focus_selected` → append `scene_actions.rs` | scene_actions.rs | general | low | build — **coordinate with #21 (same file, different worktree)** |
| 23 | Gizmo: transform write-back | gizmo.rs:33-193 | `apply_to_sdf_prim`, `apply_to_scene_node` → widen priv→`pub(crate)` (keep WARNING banner) | `transform_apply.rs` | general | medium | build **before** #24/#25 |
| 24 | Gizmo: modal transform | gizmo.rs:557-931,953-1109 | `start/cancel/confirm/update_modal_transform`, `capture_transform_num_input`, array/pre-grab helpers | `transform_modal.rs` | general | medium | build (depends on #23) |
| 25 | Gizmo: pro-gizmo drag | gizmo.rs:1114-1594 (+dead 1602-1718) | `start/update/finish/cancel_gizmo_drag` (+`update_body_origin_from_selection`) | `gizmo_drag.rs` | general | low | build (depends on #23) |
| **EXTRACT-BLOCK-THEN-MOVE (`impl AzraApp` split + flags-struct/bool-return; behavior-preserving, not byte-for-byte)** |
| 26 | Sculpt: chunk math free fns | input_sculpt.rs | `VOXEL_CHUNK`, `SMOOTH_ALL_ITERS`, `voxel_chunk_dim/_anchor/_chunks_for_region`, `prune_front_undo_markers`, `half_to_f32`, test mod | `voxel_chunk_math.rs` | sculpt | low | build; **repoint `ui_panels.rs:646`** to `crate::voxel_chunk_math::SMOOTH_ALL_ITERS` |
| 27 | Sculpt: split the one `impl AzraApp` | input_sculpt.rs | lifecycle/actions → `voxel_actions.rs`; GPU mesh/atlas/collectors → `voxel_mesh_gpu_pipeline.rs`; move/resample → `voxel_move_resample.rs`; keep 3 input entry points | 3 new modules | sculpt | medium | build after each of the 3 sub-moves |
| 28 | Input-keyboard: decode-then-act split | input_keyboard.rs | sculpt keys, rebind, pies, active-tool (bool-return), normal-block (flags-struct by value) | `input_keyboard_{sculpt,rebind,pies,tools,normal}.rs` | general | medium | build; all pie open+drive pairs moved together |
| **FORCED EXTRACT-METHOD (single guarded step each — the only genuinely non-mechanical pieces)** |
| 29 | main.rs: low-risk type defs | main.rs:397-886,332-339 | GpuCtx family → `gpu_ctx.rs`; voxel-sculpt types → `voxel_sculpt_types.rs`; `InteractionEpisode`/`DiagState`/`GpuInfo` → `app_state.rs` | 3 new modules | master | low | build (all pub(crate), no methods) |
| 30 | main.rs: constructor + bridges | main.rs:1617-1992 | `FourthLayerApp::new()` + `ensure_sdf_body`/`build_graph_from_flat`/`rebuild_prim_body_map`/`toast`/`derive_modeling_context` | `app_new.rs` | master | medium | build (transcription-error-caught-by-compiler) |
| 31 | main.rs: extract `init_gpu_ctx` | main.rs:2136-2643 | Extract the inline first-frame BGL-recreate + compile-thread-spawn + `GpuCtx{..}` literal into `fn init_gpu_ctx(&mut self, ctx)` — **its own gated task** | (method on FourthLayerApp) | master | high | build; first-frame still compiles pipelines once (guarded by `gpu_init.is_some()`) |
| 32 | main.rs: move `update()` + `main()` | main.rs:2106-3025,247-395 | `eframe::App` impl (now-slim `update`) → `app_update.rs`; `fn main()`+`GpuInitState` → extend `gpu_init.rs` | `app_update.rs` | master | high | build; keep `BODY_LOCAL_PRIMS`/`PERF_SPAM`/`bakeprof_record` in main.rs as name anchors |

---

## 3. Extraction Blockers & How to Handle Them

### A. Long-lived `&mut self.gpu` borrow (viewport_render.rs, main.rs::update) — DO NOT SUB-SPLIT
`dispatch_gpu_render` holds `if let Some(gpu) = &mut self.gpu { … }` across ~1500 lines (viewport_render:398–1905) while simultaneously reading `self.scene/cache/render/viewport/ui` and writing `self.cache.*`/`self.render.*`/`self.ui.prof_pass_times[..]` in interleaved expressions. Same shape in `update()` (main.rs:2136–2643). Any attempt to lift a phase into a `&mut self` method re-borrows `self` while `gpu` is live → **E0499/E0502**.
- **Rule:** move these methods **whole**. Slices 14–16 relocate only the *already-free* items (`resolve_render_mode`, the cfg-gated CUDA fn, the auto-policy method) that sit *outside* the borrow. The borrowed core is **not** a de-god slice.
- **The one exception (slice 31):** the first-frame init block inside `update()` is guarded by `if self.gpu_init.is_some()` and runs once — extract it via **read-phase → build locals → `self.gpu = Some(GpuCtx{..})`** into `init_gpu_ctx`. This is param-threading of `device/queue`, not a carve of the frame loop. Gate it as its own step before moving `update()`.

### B. Visibility widening (private `fn` shared across a new module boundary) — mechanical, not logic
Three slices cross a new module line with a currently-private fn:
- `apply_to_sdf_prim` / `apply_to_scene_node` (slice 23) — called by both modal and gizmo halves → widen to `pub(crate) fn`.
- DC plumbing free fns + `DcParamsGpu` (slice 3) — needed by readback/resident/test siblings → `pub(crate)`.
- `draw_resident_pool*` / `draw_sdf_resident_slot` / `body_aabb_bounds` (slice 18) — shared by G-buffer + picking → `pub(crate)` in `raster_common.rs`.
- **Pattern:** widen visibility in the **owning** slice, land it green, *then* move the consumers. Never move a consumer before its dependency is visible (violates "don't pile changes on a broken state").

### C. Disjoint-field borrow reliance (raster) — relocate as METHODS, never free-fn-ify
`dispatch_raster_pass` and `pick_node_at` read `&self.cache.mesh_cache`/`&self.scene.scene_graph` **while** `let Some(gpu) = &mut self.gpu` is live. This compiles only because `gpu/cache/scene` are disjoint fields under one `&mut self`. Converting either to `fn(app: &mut AzraApp)` breaks (can't hold `&mut app.gpu` + `&app.cache`). **Constraint:** slices 17/19/20 relocate as `impl AzraApp` methods only.

### D. Control-flow chains that need a bool return (input_keyboard, input_viewport)
The active-tool `if is_transform() … else if … else { normal }` chain (input_keyboard:364–1232) and the primary-drag `if dragged_by(Primary){ … else if … }` chain (input_viewport:600–1094) are `else if`-joined and **cannot** have one arm lifted without the extracted head returning `-> bool` ("I consumed the frame") so the parent gates the `else` tail. Decode-then-act flags (the `kb = &self.keybindings` borrow at input_keyboard:90–195; the `shift/alt/ctrl`/`pd`/`*_just_finished` locals in input_viewport) must be passed **by value in a flags struct** (the existing `PolyKeyFlags`/`NurbsKeyFlags` pattern) — an extracted fn must **never** re-borrow `self.keybindings` while also taking `&mut self`. This is why slices 26–28 are marked "not byte-for-byte" but remain behavior-preserving.

> Note: input_viewport.rs is **not** a de-god slice in this plan — its primary-drag chain is a control-flow restructure (§D) with pervasive read-then-drop borrow ordering; it is a **later, separate** task after the eight listed files are stabilized. It is owned by **general**.

### E. Inlined borrow-split ordering that must be reproduced verbatim
Several arms copy fields out of `tool.current.as_X()` into a local tuple **before** calling `self.write_prim`/`self.rebake` to drop the `self.tool` borrow (input_viewport falloff/blend-k/bevel; app_poll `rv_bake_buf` Arc-clone at 1372; the gpu2gpu re-borrow at 1712). When these move, the **read-then-drop ordering is load-bearing** — a naive reorder past the mutable call fails borrowck. Move the block atomically; do not "tidy" the ordering.

---

## 4. Crash-Hardening Backlog

Only `real_crash_risk` items from the audit. All four are the same class (UTF-8 byte-slice panic on non-ASCII AI/network error strings) on the **per-frame `poll_background_tasks` path**, so they crash the live frame, not a background thread. Fix all four in one commit (slice **C1**) using the codebase's existing `.chars().take(N).collect()` idiom (`win_ai.rs:140`).

| File:Line | Trigger | Fix | Severity |
|-----------|---------|-----|----------|
| app_poll.rs:497 | `poll_claude`/`GenerateNodeGraph`: serde_json parse-error string ≥80 bytes with a multi-byte codepoint at byte 80 → `&s[..80]` panics "not a char boundary". AI-echoed accented/CJK/emoji content is the reliable trigger. | `let m: String = e.to_string().chars().take(80).collect();` then interpolate `m`. | **P0 crash** |
| app_poll.rs:513 | `poll_claude` Err arm: Claude API error `String` ≥80 bytes, non-ASCII provider/echo text, byte 80 mid-codepoint. Full error already preserved in `code_review_err` via `e.clone()` on the prior line. | `let short: String = e.chars().take(80).collect();` | **P0 crash** |
| app_poll.rs:568 | `poll_ai_service`/`AiResponse::Error`: AI-service error `String` ≥60 bytes (OS error text, file paths, server msg), byte 60 mid-codepoint. | `let short: String = e.chars().take(60).collect();` (compute once, reuse for 568+569) | **P0 crash** |
| app_poll.rs:569 | Same error String, second independent byte-slice — panics even if 568 is fixed alone. | Reuse the shared `short` from 568. | **P0 crash** |

No `real_crash_risk` findings surfaced in viewport_render.rs, sdf_gpu_mesh.rs, voxel_gpu_mesh.rs, raster_render.rs, gpu_init.rs, or body_volume_cache/*. The remaining audited sites are invariant-safe or test-only.

---

## 5. Sequencing & Guardrails

### Do first, globally
1. **Slice C1 (crash fixes)** on the **general** worktree, isolated commit, merged to master before any relocation churn touches app_poll.rs. It is 4 one-liners with zero relocation overlap — land it immediately so subsequent app_poll splits rebase onto a fixed file.

### Per-worktree ownership (no two worktrees edit the same file)
- **master** (integration + shared render): main.rs (slices 29–32), viewport_render.rs (14–16), sdf_gpu_mesh.rs→dir (3–7), raster_render.rs (17–21).
- **general** (cache/outliner/input/poll/gizmo): app_poll.rs (C1, 8–13), gizmo.rs (22–25), input_keyboard.rs (28), and later input_viewport.rs.
- **sculpt** (voxel sculpt): input_sculpt.rs (26–27), voxel_gpu_mesh.rs (1–2), and the app_poll voxel-undo fold (slice 11).
- **poly**: owns no god-file here — participates only in the shared-file protocol below.

### Two shared-file collision points (must be serialized across worktrees)
1. **`scene_actions.rs`** receives appends from **both** master (slice 21: raster stragglers) and **general** (slice 22: gizmo scene-edit commands). These are additive `impl AzraApp` appends. **Protocol:** master lands #21 first (small), general rebases and lands #22. Coordinate via a single "scene_actions append lock" — whichever worktree holds it finishes and merges before the other starts. They append disjoint methods so a post-rebase conflict is a trivial hunk, but sequence them anyway.
2. **`voxel_gpu_mesh.rs` fold (slice 11)** moves two methods *out of* app_poll.rs (general-owned) *into* voxel_gpu_mesh.rs (sculpt-owned). **Protocol:** general performs the *cut* from app_poll.rs and hands the two method bodies to sculpt via the commit; sculpt performs the *paste* into voxel_gpu_mesh.rs. Do #11 **after** sculpt's slices 1–2 land (facade already established) and **after** general's slice 13 (so app_poll's bake split doesn't move around the voxel-undo methods mid-flight). If cross-worktree handoff is friction, the fallback is a new `poll_voxel_undo.rs` under general — but the sculpt fold is the better cohesion.

### `main.rs` sequencing (master, internal order is strict)
`29 (types) → 30 (new + bridges) → 31 (extract init_gpu_ctx) → 32 (move update + main)`. Never reorder: 31 must precede 32 because moving `update()` before its init block is extracted just relocates the unmaintainable fusion. 31 is the **only** slice in the whole plan that is not a relocation — flag it in review as an extract-method with a guarded once-per-run block.

### Guardrails on every slice
- `cargo build -p app` after **each** slice; if red, fix before the next slice (never pile onto a broken state).
- Register every new module as a flat `mod name;` in main.rs alongside the existing ~130 (main.rs:11–134). **Never add a `pub(crate) fn` to main.rs.**
- Facade re-exports (`pub(crate) use submod::Type;`) are required and sufficient wherever an external `crate::sdf_gpu_mesh::*` / `crate::voxel_gpu_mesh::*` path exists (~24 sites for voxel, several for sdf) — verify the re-export before moving, so call sites stay at **zero edits**.
- The **god-struct declaration, `AzraApp` alias, and `Drop`** stay in main.rs and every field stays `pub(crate)` — the ~130 sibling impl blocks depend on it.
- `half_edge.rs` (FROZEN) and `voxel_sculpt.rs` live in **core_math**, not the app crate — no slice touches them.
- **Selection is P0:** after raster slices 19/20 (G-buffer + picking), manually click-select a body — any regression blocks the merge.
- `cargo test -p app` after slices 6/7 (sdf tests) and after any test-module move; the 35 Naga/parity tests must stay green.

### Definition of Done (per slice)
1. `cargo build -p app` passes.
2. Behavior identical — pure relocation (or, for 26/28/31, behavior-preserving extract with the compiler proving the flags/borrows).
3. All grep'd call sites resolve unchanged (facade re-exports in place; the two known reference edits — `ui_panels.rs:646` for slice 26, and the `crate::voxel_gpu_mesh::*` facade for slices 1–2 — are made and verified).
4. No new `pub(crate) fn` in main.rs; new modules registered; visibility widenings are the *only* signature changes and are noted in the commit.
5. A senior engine dev can review the diff as a move (LHS deleted == RHS added, modulo `use`/visibility) — if the diff shows logic changes, the slice is wrong and must be re-scoped.

Relevant absolute paths verified this session: `F:\PROJECT\FourthLAyerAi\core_engine\app\src\app_poll.rs` (crash lines 497/513/568/569 confirmed live on poll path), `F:\PROJECT\FourthLAyerAi\core_engine\app\src\ui_panels.rs` (line 646 = sole external `SMOOTH_ALL_ITERS` ref), `F:\PROJECT\FourthLAyerAi\core_engine\app\src\win_ai.rs:140` (char-safe truncation idiom to mirror), `F:\PROJECT\FourthLAyerAi\core_engine\app\src\main.rs` (flat `mod` list + existing `pub(crate) use` re-export precedent for the facade pattern).