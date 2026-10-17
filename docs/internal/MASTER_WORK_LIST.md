# AZRA — Master Work List
**Rewritten: 2026-06-18.** This replaces the stale 2026-03-27 version (which was all analytical-pipeline / brick-pool era — now parked/superseded).
This is the single source of priority. `docs/prompts/ACTIVE_RUNBOOK.md` is superseded by this file.

---

## How to read this — the one rule

> **"Code exists" ≠ "works well."** The codebase has a lot of *compiled* features that are rough, ugly, or barely usable (e.g. the poly bevel tool). Presence in code is the floor, not the ceiling.

**Quality legend (the second axis, separate from "does it compile"):**
- 🟢 **Production** — works well, feels good, user-confirmed
- 🟡 **Rough** — exists and runs, but not user-friendly / needs a real rework pass
- 🔴 **Bad/Broken** — exists but actively poor or unreliable
- ⚪ **Missing** — not built
- ❓ **Needs your verdict** — code exists; *I can't grade feel from code alone* — only your hands set the real grade

Most ❓ grades below are my guess from reading code. **Correct them — your hands-on judgment overrides mine.**

---

## North Star — the architecture everything builds toward

The whole app resolves to one principle that fixes the current crashes *and* unlocks the differentiators:

**4-layer representation stack — each layer bakes down to the next:**
```
Layer 1  SDF bodies         parametric, infinite-res, boolean      → concept / blocking
Layer 2  Voxel density      volumetric, topology-free (DynaMesh)   → rough sculpt
Layer 3  Polygon + displace  ZBrush-style vertex move, UVs, retopo → detailing
                                   │  (all bake to)
Layer 0  Triangles → raster G-buffer → real-time + path-trace + RTX → ASSEMBLY / RENDER
```

**The rule:** *Specialized representations (SDF, voxel) are EDIT-time formats. Triangles are the RENDER-time format.* Things coexist only by sharing a representation — all triangles (Object Mode), or one converted into the other (SDF stamped into voxels). **You never run two display techs fused over the same pixels.**

**The model (canonical: `VIEWPORT_MODEL.md`) — Object Mode home + per-object Edit:**
- **Object Mode** (the home view, where the app opens) — everything rasterized together: SDF (baked to mesh) + voxel (meshed) + poly (native), one G-buffer. The assembly + render + PT + RTX target.
- **Edit Mode** (per-object, via Tab on the selection) — that object's own technique: voxel → raster mesh ≤512³ / atlas raymarch >512³; SDF → volume raymarch of that body; poly → mesh edit. Two spheres = two edit contexts. SDF cutters inside a sculpt enter as *boolean modifiers that stamp the voxel field*, not a second renderer.
- Navigation = a **Mode breadcrumb** ("Object › Sculpt · 'Sphere.001'"); "workspace" tabs demoted to an optional Layout menu; the **outliner is per-mode** (RowProvider architecture).

**What we are NOT building:** a fused multi-object raymarch (SDF-raymarch + voxel-raster over the same pixels). *That was the crash* (fixed structurally in `4f1a30a`). Kill the class, don't patch instances.

---

## Priority order (phases — importance, top to bottom)

| Phase | Theme | Why this rank |
|-------|-------|---------------|
| **P0** | Foundation: unified Object viewport + mode/outliner UX | The home view everything branches from (was the late "Assembly" capstone; promoted per "overall viewport first"). Crash already fixed (4f1a30a). |
| **P1** | Interaction foundation (input + selection) | Bedrock every feature touches. Input is a half-migrated dual-system mess; selection has specific gaps. |
| **P2** | Sculpt room → production | Most-built area; brush math is done but the UI is missing. Primitives + the SDF-boolean wow factor live here. |
| **P3** | Poly modeling → quality rework | Tools *exist* but are rough (bevel is bad). This is polish/UX, not greenfield. |
| **P4** | Shading & materials → polish + unify | ~90% built. Work = polish, parity, and wiring materials into the voxel raymarch. |
| **P5** | UI restructure | Big, cross-cutting. Do *after* features stabilize so layout isn't redone each phase. |
| **P6** | Render: path-trace + RTX | The unified triangle raster now lands in P0; P6 adds path-tracing + hardware RT over it. The differentiator. |
| **P7** | AI (text→model, model→model, refine) | A pillar. text→CSG works; model→model and refine are stubs. Sits on the foundation. |

---

# P0 — Foundation: the unified Object viewport + mode/outliner UX

**Goal:** build the **home view everything branches from** — Object Mode (everything rasterized together) + per-object Edit + the Mode breadcrumb + the per-mode outliner. Canonical spec: `VIEWPORT_MODEL.md`. (Promoted from the late "Assembly" capstone per "overall viewport first.")

**Current state:**
- 🟢 **SDF+voxel crash FIXED** (`4f1a30a` — sculpt exit restores `force_volume_only`; no more fused analytical+raster). User-confirmed.
- 🟢 **Design written** — `VIEWPORT_MODEL.md` (Object/Edit model, breadcrumb, per-mode outliner, confirmed decisions).
- 🟡 SDF still renders **volume-only** (raymarch); Object Mode needs it **baked to a cached triangle mesh** (revive `dispatch_grid_dc_async`, re-bake only on edit).
- 🟡 Voxel is faked as an `ImportedMesh` node + a single global `self.voxel` → needs first-class **`VoxelBody`** for per-object edit.
- 🔴 Outliner is mode-blind (always the SDF prim tree) → needs the **RowProvider** architecture.
- 🔴 Raymarched voxel renders **white in PBR** (hardcoded clay/unlit flags) — fix when wiring materials.

**Work items:**
- **P0.1 ✅ DONE** — SDF+voxel crash fix (`4f1a30a`).
- **P0.2 Unified Object-Mode raster:** revive SDF→DC/surface-nets mesh (cached, re-baked only on edit), wire into the raster draw list alongside the voxel mesh + poly. App opens here, everything together, no fusion.
- **P0.3 Mode breadcrumb + Layout demote:** "Object › Sculpt · 'Sphere.001'" as primary nav; mode drives the default panel layout; demote "workspace" tabs to an optional Layout menu (`viewport_header.rs`, `workspace.rs`).
- **P0.4 Per-mode outliner** (RowProvider: one drawer/selection/DnD/rename engine, per-mode `rows()`). Fix the still-live audit bugs **O3/O4/O6 first**, then: derive `OutlinerMode` (from existing `work_mode`/`modeling_context`) → extract `OutlinerRow` + generic drawer (route the current SDF path through it, pixel-identical) → `ObjectProvider` (hides prims — the headline fix) → decouple select-vs-enter-edit → `Sculpt`/`Poly` providers. Pinned **"Scene Globals"** (lights/cameras) in edit modes.
- **P0.5 First-class `VoxelBody` node:** promote voxel from the `ImportedMesh` masquerade + global `self.voxel` → a real graph node (peer of `SdfBody`); `self.voxel` becomes the active-edit target. Enables "two spheres = two edit contexts."
- **P0.6 Materials on the voxel raymarch** (real-time half): write proper PBR G-buffer channels → kills the white. *(Per-voxel paint later, P4.)*

**Constraints:** No-Bandaids; GPU mutations in `poll_background_tasks`; generation counters; `half_edge.rs` frozen; selection-P0 in every mode; `check_double_click` for rename; deferred mutation in the outliner.
**Done-when:** app opens in a unified Object viewport (SDF+voxel+poly together, no crash); Tab on a selected object enters its per-object edit; the breadcrumb always shows where you are; the outliner is per-mode (Object hides prims); voxels are first-class objects.

---

# P1 — Interaction Foundation (input + selection)

**Goal:** the bedrock every feature stands on. A real, customizable shortcut system and selection that always works.

### P1a — Input system (the dual-system mess)
**Current state:** 🔴 **Two parallel systems.**
- New `InputSystem` (`input_system.rs`) — clean (register → bind → context → resolve) but **only ~5 actions migrated.**
- Legacy `keybindings.rs` (~80 `ACTION_DEFS`) + **~52 hardcoded `ctx.input(...)` blocks** in `input_keyboard.rs` (1220 lines) still do the real work.
- 🟡 A rebinding UI *exists* (`win_settings.rs:85` — search, conflict detection, Default/Modo profiles) but drives the legacy system.
- ⚪ No tool/mode *contexts* (poly vertex/edge/face, SDF vs sculpt are hardcoded conditionals, not rebindable contexts). No import/export of keymaps.

**Work items:**
- **P1a.1** Pick ONE system (InputSystem) and migrate all ~80 actions + 52 hardcoded blocks into it. Delete the legacy path as it empties (per CLAUDE.md: migrate when touching).
- **P1a.2** Add tool/mode `InputContext`s (Sculpt, SDF-edit, Poly-vertex/edge/face) so the same key rebinds per context.
- **P1a.3** Point the rebinding UI at InputSystem; add keymap export/import; startup conflict report.
**Done-when:** every shortcut is registered, rebindable, context-aware; `input_keyboard.rs` has no hardcoded key blocks; one keymap file round-trips.

### P1b — Selection / deselection
**Current state:** 🟢 **mostly solid** (don't rebuild). `UnifiedSelection`/`SceneItemId`; `select_single/toggle/range/clear_unified`; GPU picking + CPU raycast fallback; poly vertex/edge/face selection works.
**Gaps (❓ verify against your feel):**
- 🟡 SDF Object mode selects whole *body*; can't pick an individual prim without switching to Edit mode.
- 🟡 Outliner audit backlog (O1–O8): deferred-pattern violations, DnD bypasses selection API, missing ctrl+click on body header — *may be stale, verify.*
- ⚪ No sub-selection for voxel / NURBS knots / imported-mesh triangles.
- 🟡 Marquee/lasso exist in code but have no discoverable UI affordance.

**Work items:** **P1b.1** prim-pick in SDF Object mode; **P1b.2** re-validate + fix the live O1–O8 items; **P1b.3** discoverable marquee/lasso UI.
**Done-when:** click-select always works across every object type (P0 rule); selection feels predictable; sub-object selection where it matters.

---

# P2 — Sculpt Room → Production

**Goal:** make sculpting *feel* like ZBrush/3D-Coat. The math is largely there; the UX and exposure are not.

**Current state:**
- 🟢 **11 brushes already implemented** (`voxel_sculpt.rs:19`): Draw, Erase, Smooth, Flatten, Clay, Scrape, Crease, Fill, ClayBuildup, Inflate, Pinch. Falloff (Smooth), strength, radius, symmetry-X, lazy-mouse, invert all work.
- 🔴 **Brush UI is missing/hidden** — the brush picker + sliders are wired in state but not drawn in a panel. *Biggest quick win: you can't pick a brush.*
- 🟡 Only **sphere + box** voxel primitives (`fill_sphere`/`fill_box`); created at hardcoded **64³**.
- ⚪ No tablet pressure→strength, no stroke-smoothing (distinct from lazy), no falloff variety (only Smooth), no brush cursor preview, no masking, no Grab/Move brush.
- 🟢 Raster-mesh display (≤512) and the new atlas raymarch (>512) both work; the duality toggle is informal.

**Work items (importance order):**
- **P2.1** **Expose the brush UI** — brush picker + radius/strength/falloff sliders in the sculpt properties panel. (Highest value, brushes already exist.)
- **P2.2** **Voxel primitives (your item 7)** — add cylinder, cone, torus, capsule, plane as base shapes *and* as boolean stamps (reuse SDF defs from `creation.rs`); resolution picker UI (128/256/512/1024) instead of hardcoded 64³.
- **P2.3** **Brush feel** — tablet pressure, stroke smoothing, falloff curves (Linear/Sharp/S-curve), brush cursor preview, masking.
- **P2.4** **SDF-as-voxel boolean modifier (the wow factor)** — SDF cutters stamp into the density field (direct primitive stamps first: box/sphere/cylinder/cone, `max(d,-cutter)`; one-shot apply first, then live non-destructive). One representation displayed → no crash, real CSG, parametric & re-editable. *Nobody else does live analytical CSG on an organic sculpt.*
- **P2.5** **Sculpt plumbing** — formalize the raster≤512 / raymarch>512 toggle (auto by res + manual override); async Ctrl+S readback (no save freeze at 1024³); 1024³ resident-mesh cap scaling.

**Constraints:** sparse atlas keeps hidden interior near-free (implicit tags); empty-space-skip keeps the raymarch cheap; dirty-region + on-change re-stamp keeps boolean modifiers cheap.
**Done-when:** you can pick any brush from the UI, start from any primitive at any resolution, feel pressure + smoothing, and subtract a parametric SDF cutter live.

---

# P3 — Poly Modeling → Quality Rework

**Goal:** turn "the tools compile" into "the tools are good." **You flagged bevel as bad — treat every tool's grade as ❓ until you confirm.**

**Current state:** code exists for **every** tool (`app/src/poly/` + `poly_drag_tools.rs`): extrude, inset, **bevel**, loop cut, knife, bridge, subdivide (Catmull-Clark), dissolve, fill, delete, merge, proportional edit; vertex/edge/face modes + screen-space pick cache. **But "exists" lied once already.** My honest grades pending your verdict:
- 🔴 **Bevel** — you said it's very bad. (There's already a `Dimramdom/PROMPT_Fix_Bevel_Tool.md` — this has bitten before.)
- ❓ Extrude / Inset / Loop cut / Knife / Bridge / Subdivide / Dissolve / Fill — **need your hands-on grade.**
- 🟡 Input audit backlog (T1–T4): bevel/chamfer/falloff drags write **directly to the flat cache**, bypassing `mirror_prim_to_graph` (transaction-discipline violation — a real correctness smell). *Verify, likely still true.*

**Work items:**
- **P3.1** **You grade each tool** 🟢/🟡/🔴 in one pass; I turn the 🔴/🟡 list into fixes.
- **P3.2** **Fix bevel first** (width/segment/profile UX + correctness).
- **P3.3** Fix the T1–T4 flat-cache-write transaction violations (route drags through `write_prim`/`mirror_prim_to_graph`).
**Constraints:** `half_edge.rs` is **HARD-FROZEN** — all fixes app-layer only (your standing directive).
**Done-when:** every tool you use daily is 🟢; no tool writes the flat cache directly.

---

# P4 — Shading & Materials → Polish + Unify (your item 8)

**Goal:** it's ~90% built — make it consistent, correct, and present on every surface (incl. the voxel raymarch).

**Current state:** 🟢/🟡 **broad and deep already.**
- 7 shading modes (`ShadingMode`: Pbr, MatCap, Lit, Wireframe, SolidHdr, Raytrace, PathTrace).
- Full `PbrMaterial` (albedo/rough/metal/opacity + edge-wear/cavity-dirt/noise layers + Subsurface/Glass/VFX/Advanced + clearcoat/anisotropy/sheen); ~17 presets; material DNA.
- Materials as graph nodes by `MaterialId`; per-prim + per-face assignment; material editor UI (`properties_sdf_material.rs`); matcap system (rotation, edge).
- Deferred PBR (contact shadows, bilateral blur, volume shadows/AO, light-linking).
**Gaps:**
- 🔴 Voxel raymarch ignores materials (the P0.3 white — fixed there for single-material; per-voxel paint still ⚪).
- 🟡 Shader-parity audit backlog (S1–S10): chamfer divergence viewport vs bake, BT.601/709 luminance, noise-mask extrapolation, 7-copy noise duplication — *verify, likely partly true.*
- ❓ Material editor *feel* — needs your grade.

**Work items:** **P4.1** single-material voxel (done in P0.3) → **per-voxel material painting** (polypaint; needs a material-id channel in the brick atlas); **P4.2** re-validate + fix S1–S10 parity items; **P4.3** material-editor UX pass.
**Done-when:** every representation shades identically under one material system; viewport == bake; (stretch) polypaint on voxels.

---

# P5 — UI Restructure (your item 5)

**Goal:** from "everything everywhere" to a structured, customizable, dockable UI.

**Current state:** 🟡 **partial scaffolding.** `panel.rs` has a 13-panel registry + saved layout (`ui_layout.json`, dock zones, visibility). **But** true docking is marked `FUTURE`; no theme system; no workspace presets; panel-draw code is intermixed with business logic (outliner draw + tree mutation in the same files; viewport in a 1992-line file). Shortcuts "rudimentary" = the P1a dual-system problem.

**Work items:** **P5.1** real docking + saved workspaces (per-room layouts); **P5.2** theme/style system (centralize egui style, beyond color sliders); **P5.3** decouple panel rendering from logic (panels draw, actions mutate via the deferred/transaction path); **P5.4** customization UX (panel show/hide, drag-dock).
**Why here:** lay out for a *stabilized* feature set (P2–P4), not one still churning.
**Done-when:** you can dock/save a workspace per room; theming works; panel code is separable from logic.

---

# P6 — Render: Assembly + Path Trace + RTX (your item 6)

**Goal:** render SDF + voxel + polygon **together**, with path tracing and hardware RT. This is the differentiator and the capstone.

**Current state:** 🔴 **the big gap.**
- GLSL BVH path tracer exists (`pathtrace_bvh_full.comp`) but is **SDF-only** — it marches SDF functions; **voxels and polys aren't in its BVH.** Voxel-only scenes fall back to `AnalyticalLite` (no PT).
- Each render mode is **isolated** — no unified renderer draws all three together.
- ⚪ **No hardware RTX** — wgpu doesn't expose ray-query/acceleration-structures yet. CUDA OptiX infra exists (`cuda_render`) but isn't wired into the main dispatch.

**Work items:**
- **P6.1** **Assembly room: unified triangle raster** — SDF baked to mesh, voxel meshed (Surface Nets), poly native → one G-buffer, one depth, shadows/picking over the unified set. *This is the thing that crashes today, rebuilt right.*
- **P6.2** **Path trace the unified triangle BVH** (not SDF functions) — so voxel + poly + SDF all path-trace. Voxel/SDF bake to triangles for PT (render-time bake, not interactive).
- **P6.3** **RTX / hardware RT** — decide the path: CUDA OptiX (wire the existing infra) vs wait for wgpu ray-query. Honest: this is gated by ecosystem support.
**Done-when:** a scene with all three representations renders together, real-time and path-traced, with consistent lighting.

---

# P7 — AI (your item 9)

**Goal:** text→model, model→model, text-to-refine-model, and neural beauty polish — AI that makes Azra *feel* finished without replacing direct modeling.

**Current state:**
- 🟢 **text→model (text→CSG) works** — `app_ai/agent.rs` + `claude.rs`; Ollama (local) or Claude; parses to `SdfPrimitive`s; injects via transaction (`creation.rs:133`) → real editable geometry. Mock fallback for 7 object types.
- 🟢 text→texture works (external HTTP server → albedo/normal/rough/metal/height maps).
- 🟢 **P7.4 Neural Beauty (path-tracer overlay) — MVP SHIPPED** — capture beauty + `gbuf_normal` (xyz + depth.w) + curvature; POST `/neural_beauty` on texture server; structure-lock img2img; viewport overlay + before/after UI. Geometry stays editable (look layer only).
- 🔴 **model→model — STUBBED.** No UI/backend to transform one model into another.
- 🔴 **text-to-refine-model — STUBBED.** Scene can be exported as JSON context to Claude, but there's no edit-op schema or merge-back; can't say "make this taller" and have it apply.
- 🟡 AgentBar panel (`ui_panels.rs:127`) — prompt input, Ollama/Claude toggle, injects prims.

**Work items:**
- **P7.1** **text-to-refine-model** — extend the JSON schema with edit ops (scale/rotate/delete/recolor/boolean), pass current scene as context, diff/merge results back. The highest-leverage AI feature (edits beat regenerate).
- **P7.2** **model→model** — transform pipeline (style/shape transfer on existing geometry).
- **P7.3** AI generation history/undo; tighten AgentBar UX.
- **P7.4 ✅ MVP DONE (2026-07-22) — Neural Beauty / AI path-tracer overlay**
  - **What:** still-frame AI polish guided by PT/viewport beauty + depth + normals + silhouette + curvature + text prompt.
  - **Client:** `app/src/ai_beauty.rs`, menu *File → AI Beauty (Path-Tracer Overlay)*, AOV GPU readback in `viewport_render.rs`, overlay in central viewport.
  - **Server:** `ai_backend/server/neural_beauty.py` + `POST /neural_beauty` on texture_server `:7860`.
  - **Controls:** creative strength, structure lock, AOV toggles, seed/steps/CFG, overlay opacity, split before/after, save PNG.
  - **Not yet (follow-ups):** multi-ControlNet models, temporal/video stability, bake polish back into PBR maps, SDXL/Flux quality path, live every-frame (stills only).
- **P7.5** (follow-up) Neural Beauty quality: SDXL/Flux + ControlNet depth/normal; temporal consistency for orbit scrub.
- **P7.6** (follow-up) Project AI Beauty detail into material maps (mode B — not just 2D overlay).
**Constraints:** all backends are blocking clients on worker threads (no UI hang) — keep that. *Per CLAUDE.md vision note: AI (Phase B/C) is gated on a solid foundation — i.e. after P0–P1.* Overlay must never mutate scene graph without an explicit bake action.
**Done-when:** you can refine an existing model by text instruction and see it apply as editable geometry; and you can polish a PT still with AI while keeping silhouette from G-buffer/SDF AOVs.

---

# Cross-cutting — Bugs & Tech-Debt (legacy backlog — VERIFY before trusting)

The 2026-03 audits logged ~100 issues. Many predate the pivots and may be fixed/irrelevant. **Do not treat as current truth — re-validate when touching the area.** Highest-signal survivors to check:
- **R1 / NodeId u64→u32 truncation in picking** (was P0) — verify picking still truncates IDs.
- **T1–T4** flat-cache writes bypassing transactions (folded into P3.3).
- **S1–S2** shader cull/chamfer divergence (folded into P4.2).
- **O1–O8** outliner deferred-pattern violations (folded into P1b.2).
- `COMPLETE_BUG_TRACKER.md` (184KB) is the running log — keep updating it (CLAUDE.md rule), but it needs a triage pass to drop dead entries.

---

# Dead / Superseded — do NOT invest (parked)

From the old work list + March docs, these are off the table per current architecture (CLAUDE.md / `VOLUME_ADR.md`):
- **Brick pool** (23 tracked bugs, `BRICK_CACHE_BUG_AUDIT.md`) — **PARKED.** Volume-only renders the baked texture directly.
- **Analytical pipeline perf** (old P1–P10: selective serialization, hybrid dispatch, intra-body BVH, DC chunking) — **superseded** by volume-only + voxel sculpt. Analytical is debug-only (`force_analytical_debug=false`, `bvh_enabled=false`).
- **Composite grid / SVO hybrid** — dead.
- Nested SdfBodies (old F1) — *still desirable* but re-scope under P0's room model before building.

---

# Doc Hygiene — the MD cleanup (255 files → a handful)

**Proposed** (awaiting your OK before I move/delete anything — all git-recoverable):

**KEEP (active reference):**
`CLAUDE.md` · `ARCHITECTURE.md` · `RENDER_ARCHITECTURE.md` · `VOLUME_ADR.md` · `GPU_INVALIDATION_MATRIX.md` · `FEATURE_RULES.md` · `COMPLETE_BUG_TRACKER.md` (needs triage) · `MASTER_WORK_LIST.md` (this) · `VIEWPORT_MODEL.md` (canonical viewport/mode/outliner design).

**FOLD-IN then archive (current-ish June plans → merged into this list):**
`PLAN_SCULPT_RAYMARCH.md` · `GPU_SCULPT_PLAN.md` · `SCULPT_BRUSH_PLAN.md` · `AZRA_Sculpt_Brush_System.md` · `PRIMITIVE_SYSTEM_PLAN.md` · `VOXEL_SCULPT_INTEGRATION.md` · `REFACTOR_PLAN.md` · `AUDIT_WORKLIST.md` (271KB).

**ARCHIVE → `docs/archive/2026-03/` (historical, ~200 files):**
all of `docs/audits/*` · `docs/prompts/*` · `docs/planning/*` · `Dimramdom/*` · root `PLAN_*` / `AUDIT_*` / `DIAG_*` / `PBR_*` / `BRICK_*` / `*_DIAGNOSIS.md` / `*_AUDIT.md` from March.

**DELETE (pure duplicates):**
`*  (1).md` copies — `FACIAL_SOLVER_SYSTEM (1).md`, `DESIGN_SDF_Guided_PathTracing (1).md`, `PROMPT_DNA_Advanced_Noise_Groups (1).md`, `FULL_CODEBASE_AUDIT_PROMPT (1).md`.

**End state:** ~9 active docs at root + a dated archive folder. Nothing lost (git history + archive), but the working set is legible.

---

## How we use this list
1. Phases run top-to-bottom; within a phase, items are importance-ordered.
2. **You grade the ❓ items** — your hands-on verdict is the real spec.
3. One item per task/prompt (CLAUDE.md). Update `COMPLETE_BUG_TRACKER.md` on bug fixes.
4. This file is the priority source; revise it as reality changes (not the March docs).

> **2026-07-23 (fix/sculpt-feel-perf):** Shipped 3D-Coat-style **Surface vs Voxel sculpting mode toggle** — Surface mode displaces the Surface-Nets mesh vertices directly (Draw/Erase/Inflate/Smooth/Flatten/Move; density-native brushes greyed), one Surface undo entry per stroke, mesh→SDF revoxelization on toggle-back/save/leave-edit via the existing Move-bake landing path. See COMPLETE_BUG_TRACKER.md same date.
> **2026-07-23 (fix/sculpt-feel-perf) — FULL Blender brush set in Surface mode:** implemented **Pinch, Clay/ClayBuildup, Scrape, Crease, and Snake Hook** (new `VoxelBrush::SnakeHook`) as real per-vertex displacement in surface mode, per `docs/planning/sculpt/BLENDER_BRUSH_SPEC.md`. Shared `pinch_axis_pull` extracted to `core_math` (density + surface share one impl). Snake Hook = moving-anchor capsule tube over pristine coords + `crease_pinch` axis squeeze; surface-only (greyed in Voxel mode). Only Fill + Smudge remain greyed in surface mode (documented, tooltips). Oracle-gated (shape): Pinch cross_dy corr 0.757, Clay dr_radial corr 0.324 (deposits outward; box-tip deferred), Snake Hook lateral-pull magnitude within ~5% (rake deferred). See COMPLETE_BUG_TRACKER.md + BLENDER_BRUSH_SPEC.md session log.
