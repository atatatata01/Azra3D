# Legacy SDF-Sculpt Removal — Plan & Handoff (Fable)

**Status:** C0 ✅ shipped (`e9fa024`). C4 owner gate ✅ ANSWERED. **C1–C3, C5–C6 open.** C7 recommend SKIP.
**Authored:** 2026-07-14 (Opus 4.8) from a 6-lens Feature Dependency Audit, every lens challenged by a
missed-dependent skeptic and a rule-compliance skeptic. Every load-bearing claim below was
re-verified at the code by the lead before writing.
**Companion:** `RESOLUTION_FIDELITY_ROADMAP.md`, `COMPLETE_BUG_TRACKER.md` (S26-2, S26-3).

---

## 0. The owner directive (verbatim) + the answered gate

> *"i dont need the alternative path what per body cache is for, i remember this button was added when
> i was deciding between analytical vs dcmesh"*
> *"the sdf sculpt is also no longer needed it was a crazy idea that never landed"*

**D1 — which "SDF sculpt"? ANSWERED: (A) the raymarch one ONLY.**
Remove the `cpu_volume` toolbar sculpt (9 `Brush*` buttons → `activate_sdf_sculpt_brush` →
`bake_from_primitives` → `rm==1` raymarch). **KEEP** the per-prim Sculpt-**modifier** brush
(properties checkbox → `handle_sdf_sculpt_input` → `ModifierType::Sculpt` → the **production** DC
bake, `volume_bake.wgsl` tag 7). Consequence: `sdf_sculpt_active` and all 7 readers **STAY**, and
blocker B2 evaporates.

**D2 — `ConvertToClay` / `Import OBJ as SDF`? ANSWERED: remove BOTH entry points.**
Not re-pointed at the voxel keeper (there is no voxelize-an-SDF path today —
`workspace.rs:197-200` says so in-code: `// FUTURE (sculpt brush system): voxelize an SDF…`).

**Why the toggle dies with the feature:** `⚡ Per-Body Cache` is the last survivor of the
analytical-vs-DC button cluster — its sibling is still commented at `properties_render.rs:93`
(*"(⟐ Analytical toggle REMOVED 2026-07-14 …)"*). **OFF is not a render mode.** Its only remaining
function is letting the Grid picker resize `cpu_volume`, whose only consumer is the legacy sculpt.
The toggle exists to configure the dead feature.

---

## 1. MUST-STAY — the list to trust (removal here = corruption)

### Save-format red lines
| # | Item | Why |
|---|---|---|
| **S1** | `ModifierType::Sculpt` — `core_math/src/modifiers.rs:78` | **bincode position 7, MID-LIST**, four variants after it, with the file's own warning on line 79. Removing shifts Spherize 8→7, Shell 9→8, Emboss 10→9, Deboss 11→10. **Silent**: old files still parse, every Spherize loads as Sculpt; the newest Emboss/Deboss work breaks worst. **D1=(A) keeps this variant LIVE**, so this is moot for now — but it must never be removed even if (B) is retired later. Tombstone only. |
| **S2** | `RenderSettings.voxel_cache_res` — `project.rs:265-266` | Derive-only struct, 13 fields after it, then 8 more on `ProjectFile`. **`#[serde(default)]` is INERT under bincode.** Empirically proven by the audit: at 64/128 it errors (`invalid u8 while decoding bool … found 128` — the low byte misread as `adaptive_resolution`); at **256/512/1024 it HANGS** (`256u32 LE = [0,1,0,0]`, first byte decodes as a valid `bool false`, the shift propagates into a `Vec<(String,bool)>` length prefix → multi-exabyte alloc). Production has **no** `with_limit` (`project.rs:436`). Keep the field, keep `project_load.rs:133`, keep the runtime twin `types_render.rs:124`. |
| **S3** | `SdfPrimitive.sculpt_layers` + `active_sculpt_layer` — `primitives.rs:216-221` | Serialized `Vec<SculptLayer>`. Out of scope. |
| **S4** | `VoxelNodeRecord` / `voxel_grids` — `project.rs:30-31, 92-96` | The **keeper's** persistence (`core_math::voxel_sculpt::VoxelGridRecord` — a *different type* from `SdfVolume`). Do not confuse. |
| **S5** | `CURRENT_SAVE_VERSION = 7` — `project_load.rs:49` | No wire bytes change. Bumping trips future-version guards for zero benefit. |

### Keeper infrastructure (removal breaks the mainline)
| # | Item | Why |
|---|---|---|
| **K1** | `cpu_volume.resolution[0]` + `.grid_bounds` **as VALUES** | **Blocker B1.** Readers: `dirty.rs:167-169` (**the PRODUCTION DC bake**), `viewport_render.rs:715`, `project_load.rs:400-404`, `app_update.rs:143-144`, `workspace.rs:421-422`, `cuda_render_dispatch.rs:53`. Relocate before the field dies → **C2**. |
| **K2** | `graphics/src/gpu_volume.rs` — **the FILE** | Hosts `ResidentVolume` / `VolumePool` / `VolumePoolSlot` = the per-body cache **keeper**. Only `SdfGpuVolume` is legacy. Deleting the module kills what we're keeping. |
| **K3** | `core_math/src/sdf_volume.rs` — **the FILE** | `properties_sdf.rs:6` uses `FalloffCurve` — live SDF properties UI. Only the brush types + the `SdfVolume` instance go. |
| **K4** | `use_body_vol_cache` — **the FIELD** (~45 readers) | Only the *toggle* goes; the field becomes invariant `true` (already forced at `project_load.rs:405`, `project_new.rs:68`; default true `cache_state.rs:141`). |
| **K5** | `sculpt.voxel_undo_stack` / `voxel_redo_stack` / `voxel_undo_order` | **Shared** legacy (`undo.rs:69/78/129`) ↔ keeper (`undo.rs:103-105`). Delete the legacy *branches*, keep the *stacks*. |
| **K6** | `scene_sdf` / `scene_analytical` — `sdf_scene.wgsl:379-382` | Live for deferred reflections + fog (CLAUDE.md). Narrow to `return scene_analytical(p);` — do not delete. |
| **K7** | `GpuCtx.gpu_volume` | Bound at **6 sites**: binding 2 in `raymarch_bg`/`deferred_bg`/`fog_bg`/`pt_bg` (`viewport_render.rs:510/532/575/647`) **plus** PT bindings 40/41 (`:642-643`). `deferred_bg` is the **production** shading pass. See §3 C7. |

### Selection (P0 — "Selection must ALWAYS work")
- **`input_viewport.rs:1271`** — `sdf_sculpt_active` **suppresses** box-select. If it ever dies, **drop the clause** (box-select unconditional); **never invert it**. Removal *restores* selection, cannot break it.
- **`draw_selection.rs:35`** is a **misnamed brush-cursor fn** (`draw_sdf_brush_preview`) — zero selection content. The real pickers (`raycast_select`, `handle_click_select`) never read the flag. *(D1=(A) means both stay anyway.)*

---

## 2. The two traps that cost the most time — read before touching anything

1. **Cross-mode configuration reads exactly like dead code.** The lead judged the legacy path
   "vestigial → delete it" from an **Object-Mode** grep ("written, never read"). It is an
   **Object-Mode control sizing a Sculpt-Mode resource**. Deleting it blind would have **pinned
   legacy sculpt to 128³** (`app_new.rs:24`) rather than erroring. Verify at the READER, in *every*
   mode.
2. **`sdf_sculpt_active` ≠ the legacy sculpt.** Two identifiers one word apart mean "keeper" and
   "removal target". That collision produced a live misidentification during this very audit.
   **C1 renames them** — documentation does not fix a bad name.

---

## 3. The ordered plan

Each step is independently build-green. **C0 and C4 are separate *tasks*, not commits of one task** —
a single commit spanning UI + state + render + shader + a serialized config + two destructive
features violates one-bug-per-task and the refactor budget.

### C0 — Workspace config tolerance ⟨opus⟩ ✅ **SHIPPED `e9fa024`**
`WorkspacePanel.buttons` now drops unknown `ToolButtonId`s instead of failing the document.
Without it, **C5 silently eats the owner's 9 workspaces** (verified on disk: 9509 B, builtins
RENAMED so `defaults()` cannot rebuild them, 5 custom, references `BrushAdd`/`BrushCarve`/
`BrushSmooth`/`BrushFlatten`/`BrushInflate`/`ConvertToClay` ×2). Gate proven to fail without the fix.
**Let it bake ≥1 release so files rewrite clean before C5.**

### C1 — Tombstone + disambiguating rename ⟨opus⟩ · *adds code, zero behaviour change*
- `modifiers.rs`: tombstone comment on `Sculpt` — *"RETIRED-SAFE: bincode position 7. NEVER remove or reorder — Spherize/Shell/Emboss/Deboss shift and every old .fl4 silently mis-loads."*
- Rename `sdf_sculpt_active` → `sdf_modifier_brush_active`; `activate_sdf_sculpt_brush` → `activate_legacy_voxel_bake_brush`. Sites: `main.rs:344`, `input_sculpt.rs`, `workspace.rs`, `properties_sdf_shape.rs`, `viewport_overlays.rs`, `draw_selection.rs`, `input_viewport.rs`, `app_update.rs`, `app_new.rs`, `project_load.rs`, `project_new.rs`.
- **Gate:** `cargo build -p app`, `cargo test -p core_math`.

### C2 — Relocate res/bounds off `cpu_volume` ⟨opus⟩ · **PREREQUISITE FOR C6**
- Add `pub const SDF_GRID_BOUNDS: f32 = 2.0;` in `bake_utils.rs` beside `pad_grid_bounds`/`GRID_PAD_FRAC`.
- Repoint `dirty.rs:167-169` → `set_params(self.render.voxel_cache_res, SDF_GRID_BOUNDS, sg)`; same for `project_load.rs:400-404` (`pre_sync_params`), `viewport_render.rs:715`, `cuda_render_dispatch.rs:53`, `workspace.rs:421-422`.
- Add `debug_assert_eq!(self.render.voxel_cache_res, self.cache.cpu_volume.resolution[0])` — it has a **natural lifetime**, dying with `cpu_volume` in C6.
- **Safe because** the two are kept in lockstep today (`properties_render.rs:206-208` writes both; `project_load.rs:133-138` sets both; `app_new.rs:24` = 128 == `types_render.rs` default 128). `grid_bounds` is a de-facto constant (sole write `sdf_volume.rs:104`; corroborated by `graphics/src/types.rs:263 grid_res:128, grid_bounds:2.0`).
- **Do NOT invent a serialized field** — that is the S2 trap.
- **Gate:** `cargo build -p app` **AND `cargo check -p app --features cuda`** — the default gate is **blind** to `cuda_render_dispatch.rs:53` (`app/Cargo.toml:7 default = []`). Manual: an SDF body still renders triangles.

### C3 — Per-Body Cache toggle + `GridRoute` ⟨opus⟩ · *independent of C5/C6*
- Delete the toggle (`properties_render.rs:74-101`); ceiling becomes `max_sparse_resolution()` unconditionally; collapse `GridRoute` (`:16-53`) and its `LegacyGlobal` arm (`:200-216`); keep ONE test (picker ceiling == `max_sparse_resolution()`).
- **Keep `use_body_vol_cache` the field (K4) and `voxel_cache_res` (S2).**
- **Rule-POSITIVE:** `properties_render.rs:211` calls `SdfGpuVolume::new` inside `draw_props_render` and nulls three bind groups — a **live Frame Ordering Contract violation** (the rule's own example is "a resolution button"). Deleting it removes a standing violation and a latent load-order bug (`project_load.rs:142` reads the pre-load toggle ~263 lines before `:405` forces it true). **This closes tracker S26-3's frame-ordering half.**
- **Behaviour change to state to the owner:** this removes `voxel_cache_res`'s only runtime writer. Per-body res stays settable (`set_body_resolution`); the *global fallback default* freezes at its load-time value.
- **Gate:** `cargo build -p app`, `cargo test -p app`.

### C4 — ⛔ Owner decision gate ⛔ ✅ **ANSWERED** (see §0)

### C5 — Legacy brush removal ⟨opus⟩ · *needs C0 (shipped) + D1 (answered)*
**ATOMIC** — `sdf_volume.rs` ↔ `sculpt_brush.rs` is a dependency **cycle**; neither half compiles alone.
- **DELETE** `core_math/src/sculpt_brush.rs` (~320 lines) + `lib.rs:22 pub mod sculpt_brush;`
- `sdf_volume.rs`: `BrushType` (`:15`), `BrushSettings` (`:70`), `apply_brush` (`:374`), `apply_brush_advanced` (`:380`), `impl SculptBackend for SdfVolume` (`:397+`). **Verify `:96-97`** — `new_empty` constructs `BrushSettings{brush_type: BrushType::Add}` and calls `apply_brush`; it must die too or `new_empty` won't compile. **KEEP `FalloffCurve` (K3).**
- `workspace.rs`: `:304-312` dispatch, `:70-78` is_active, `:103-104` accent, `:886-890` catalog, `:360-383` activate, `:396-429` exit/discard.
- `app_types/types_workspace.rs`: the 9 variants, their meta, **and `:242 Label(4)`** — else the defaults render a dangling "BRUSH" header over nothing (`workspace.rs:28-44` renders labels unconditionally; **the owner's live "Sculpt" workspace already exhibits this artifact**).
- `main.rs:339 brush:BrushSettings`, `app_new.rs:149`, `draw_gizmo.rs:577 draw_falloff_graph` (**zero callers**), `draw_viewport_overlays.rs:7` import + `:80-86` else-branch.
- **Rehome `draw_viewport_overlays.rs:88 if self.brush.symmetry_x`** — it sits **outside** the voxel/else split, so it runs for the **keeper** cursor. (`symmetry_x` has **no writer anywhere** → inert dead UI, but a compile-blocking read in surviving code. Point at `v.symmetry` or delete.)
- **Category vanishes cleanly** — `workspace.rs:827-831`: `if filtered.is_empty() { continue; }` precedes `ui.label(cat)`. No empty group box.
- **DO FIRST** (CLAUDE.md: "implement the replacement BEFORE removing the original"): the Sculpting workspace's toolbar is *entirely* these 9 buttons; the real `VoxelBrush` buttons live only in `render_tool_properties()` (`workspace.rs:473-491`). **Append voxel-brush `ToolButtonId` variants at the END of the enum**, wire to `v.set_active_brush(b)`, update `Workspace::sculpting()` — otherwise the sculpt room ships with an **empty palette**.
- Per D2: also delete `ConvertToClay` (`workspace.rs:279-301`, catalog `:891`, defaults `types_workspace.rs:227`) and `Import OBJ as SDF` (`menus.rs:73` → `app_update.rs:136-164`).
- **Gate:** `cargo test -p core_math`, `cargo build -p app`. **MANUAL, non-negotiable: launch with the owner's existing `~/.azra/workspaces.json` — all 9 workspaces still present.** This is the C0 payoff and the single highest-risk regression.

### C6 — `cpu_volume` + `rm==1` ⟨fable⟩ · *needs C2 + C5*
- `cache_state.rs:16` + `:136-138` ctor param; `app_new.rs:24,159`; `input_sculpt.rs:288-350` (legacy stroke) + **`:460 clear_to_empty()`** (a **write, never a read**, inside the keeper's `voxel_set_active` — delete the *call*, not the path); `undo.rs:69/78/129` (**keep the stacks**, K5); `dirty.rs:50-55` + `:216 rebake_for_sculpt` (**zero callers**); `viewport_render.rs:300` (the upload — same gate as `:669`, don't orphan it) + `:669-673` (`rm` → const `0u32`); `sculpt_state.rs:19 current_stroke` (**verify the keeper never writes it** before cutting); `workspace.rs:282`; `project_load.rs:134-148` (**keep `:133`** — restore + comment, per the in-repo precedent at `:151-153` for `atlas_xy_resolution`).
- **Shader (ONE batched edit — CLAUDE.md):** narrow `sdf_scene.wgsl:379-382` to `return scene_analytical(p);`; delete `get_voxel`/`scene_voxel` (`:357-374`). **Keep binding 2** (C7).
- **Perf win, not just cleanup:** `dirty.rs:50-55` bakes the full CPU volume gated on `work_mode == Sculpt` alone.
- **Gate:** `cargo test -p graphics` (Naga parse, all variants), `cargo test -p app`, `cargo build -p app`.

### C7 — `gpu_volume` / binding 2 ⟨fable⟩ · **OPTIONAL — RECOMMEND SKIP**
Binding 2 is declared across 6+ BGL sites and is bound in the **production** `deferred_bg`. Renumbering
risks the "bakes fail silently" class (see `feedback_bgl_binding_sites` memory) for **zero user-visible
gain**. Leave binding 2 wired to a small dummy — an **established, documented pattern here**
(`brick_pool_dummy_buf` `gpu_ctx.rs:360`; `dummy_index`/`dummy_pool` in `dc_pipeline.rs`). Delete only
the `scene_voxel` **code path** (C6). Revisit only if a future task must reclaim the slot.

---

## 4. Owner manual scenario (after C5, and again after C6)
1. **Launch with the existing `~/.azra/workspaces.json`** → all **9** workspaces present, builtins still named `SDF`/`Sculpt`/`Model`/`NURBS`, Custom 5–9 intact. *(The single most important check.)*
2. Object Mode → an SDF body still renders **triangles**; select/click-select works; box-select works.
3. Enter the **voxel** sculpt (`enter_sculpt_mode`) → brushes work, palette is **not empty**.
4. The per-prim **"Enable Sculpt Brush"** modifier still works (D1 keeper).
5. **Load a legacy `.fl4`** whose `voxel_cache_res` is 512/1024 → loads without hang or error.
6. Grid picker: 1024³ selectable, sparse bake runs.

## 5. Tiering
**Opus:** C1, C2, C3, C5 (mechanical, gated, compile-enforced).
**Fable:** C6 (render branch + shader + cross-subsystem state). C7 skip.
Rough size: C1 ~60 LOC touched, C2 ~15, C3 ~90 removed, C5 ~500 removed + ~80 added (voxel palette),
C6 ~250 removed. Multi-session.
