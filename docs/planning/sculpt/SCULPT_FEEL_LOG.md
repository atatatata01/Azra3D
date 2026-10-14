# SCULPT FEEL — Running Log

Goal: **improve the *feel* of the voxel sculpting tool**, reverse-engineered against Blender
as the ground-truth oracle (the same discipline the poly/bevel effort uses in
`docs/planning/poly/OVERNIGHT_LOG.md`).

**How to read this log.** One entry per work session / experiment. Newest entries go at the
BOTTOM. Every entry records: what we tried, the Blender reference data it was checked against,
the result, and *why* it did/didn't work — so any future session can recall the reasoning
without re-deriving it. Bugs found along the way get logged here too (with a `BUG:` tag) and
cross-referenced into `COMPLETE_BUG_TRACKER.md`.

Entry template (copied from the poly OVERNIGHT_LOG discipline, adapted for feel work):

```
## [DATE] — [Session/Phase] — [short title]

**Target feel behavior:** [which brush / which lever]
**Hypothesis:** [what we think is wrong / what Blender does differently]
**Blender reference captured:** [oracle script + ref json, or "none — behavioral only"]
**Tried:** [what we changed — file:line]
**Result:** [better/worse/no-change, measured how]
**Why:** [root-cause reasoning]
**Build/test:** [cargo build -p app / cargo test status]
**Kept? / Reverted?:** [decision]
**Next:** [handoff]
```

---

## 2026-07-02 — Session 0 — Orientation, architecture map, and methodology decision

**Purpose:** Establish the full picture of the Sculpting Room and decide *how* to apply the
Blender-oracle methodology to a voxel sculpt system (which is NOT a mesh-op like bevel).

### A. Current-state map (verified by read-only survey of the live tree @ 7068d10)

**The Sculpting Room = `WorkMode::Sculpt`** (`app_types/src/types_poly.rs:140`).
- Object lives as `AzraApp.voxel: Option<VoxelObject>` (`app/src/main.rs:1475`), scene node
  `NodePayload::VoxelBody` (`core_math/src/scene_graph.rs:400`).
- Data model: `VoxelGrid { density, res, bounds, iso, dirty, colors }`
  (`core_math/src/voxel_sculpt.rs:1437`). **Dense `Vec<f32>` when res<256, sparse brick-backed
  `BrickGrid` when res>=256** (threshold `voxel_sculpt.rs:1462`).
- Source of truth *during a stroke* is the **GPU sparse atlas** (`VoxelAtlasGpu`,
  `app/src/voxel_gpu_mesh.rs:73`); CPU grid is a lazy cache. (SCOPED inversion of the
  "CPU is truth" rule — per CLAUDE.md Voxel Sculpt System section.)
- Display (Display Invariant, never fused): **raster Surface-Nets mesh <=512^3**, **atlas
  raymarch >512^3** (`sculpt_voxel_raymarch.wgsl`, gated at `viewport_render.rs:387`). NOTE:
  PLAN B1 raymarch is architecture-approved but **not yet fully wired into the live pipeline** —
  today high-res sparse strokes still pay mesh re-extract cost.
- Per-stroke frame flow: stamp -> (GPU atlas write | CPU box `apply_brush`) ->
  `mark_dirty(RENDER)` -> `viewport_render` re-dispatch; mesh re-extract via
  `voxel_flush_dirty_chunks` (`input_sculpt.rs:1007`), budget `FRAME_BUDGET_MS=14`.
- Undo: its own path (NOT `transaction()`) — parallel CPU-delta + GPU-atlas-snapshot streams,
  strict interleave marker (`undo.rs:133`).

### B. The "feel" levers today (the knobs we will tune) and where they live

| Lever | Where | Current math | Blender does |
|-------|-------|--------------|--------------|
| **Falloff curve** | `voxel_sculpt.rs:134` `falloff_weight` + GPU mirror `graphics/src/voxel_brush.wgsl:100` | `w = e^p`, `e=1-t`, `p = 1 + 7*hardness` (p in [1,8]) | `CurveMapping` presets: Smooth / Smoother / Sphere / Root / Sharp / Sharper / Lin / InvSquare / Constant — each a known formula |
| **Stroke spacing (dabs)** | `sculpt_layer.rs:247` `StrokeInterpolator.push`, default `spacing=0.25` (`brush/mod.rs:298`) | stamp every `0.25 * radius` along path (Catmull-Rom resample) | default **spacing 10%** of radius; "#1 feel lever" per SculptGL audit |
| **Radius / strength** | single producer `input_sculpt.rs:2260`; anti-alias floor `radius.max(1.5*voxel_size)` | 0..1 strength, pressure fold `radius*=(0.4+0.6*pressure)` | strength curve + pressure ramps, per-brush defaults |
| **Smooth / relax** | `voxel_sculpt.rs:2453` `relax_region` (6-neighbor Laplacian), global `smooth_all` (3 iters) | Laplacian toward neighbor avg * w | surface relaxation / cotangent-ish; Shift=smooth everywhere |
| **Grab / Move** | dense mesh-grab `input_sculpt.rs:2334`; sparse elastic-grab `voxel_sculpt.rs:392`; `grab_plateau_falloff:162` (rigid core 0.30..0.80r) | `disp = drag * plateau_falloff(dist)` | Grab = `disp = drag * curve(dist/r)`; Elastic Deform = regularized Kelvinlets (Pixar paper) |
| **LazyMouse** | `input_sculpt.rs:2217`, rope `lazy*70` px, default 0.3 | screen-space rope smoothing | "Stabilize Stroke" radius/factor |
| **Symmetry** | `input_sculpt.rs:2520`, mirror local X/Y/Z | plane-reflect stamp+normal | mirror X/Y/Z + radial |
| **Accumulation** | (implicit — per-stamp clamp 0..1) | clamps density each stamp | "Accumulate" toggle; per-stroke clamp otherwise |

Composable brush system (`BRUSH_SYSTEM_ARCHITECTURE.md`): Phase-0 DONE — `BrushRecipe` +
`StampPlan` POD, 12 brushes map 1:1 to single-op recipes, CPU/GPU byte-identical, golden parity
test. Phase 1 (next) = move hardcoded op constants into recipe data. **Any falloff/spacing change
must keep CPU (`voxel_sculpt.rs`) and GPU (`voxel_brush.wgsl`) byte-identical — the golden parity
test enforces it.**

### C. METHODOLOGY DECISION — how the Blender oracle adapts from bevel to sculpt

**Bevel's oracle = exact vertex parity.** Bevel is a deterministic mesh operation on a tiny
mesh; same input topology -> same output vertices, comparable to 6 decimals
(`nway_pyramid_ref.json`). A Rust proptest asserts `mesh_eq(deep)` within 1e-4.

**That does NOT transfer to voxel sculpt, and we must not pretend it does** (Rule-Conflict-Check /
No-Bandaids honesty). AZRA sculpt is a **density-voxel field -> Surface Nets** mesh; Blender
sculpt is **triangle/dyntopo/multires displacement**. The generated topologies are unrelated, so
vertex-for-vertex parity against Blender is impossible AND meaningless. "Feel" is not a vertex
set — it is the **response dynamics**.

**So the sculpt oracle captures behavior, not vertices.** Three comparable ground-truth forms,
each becoming a `*_ref.json` exactly like the bevel jsons:

1. **Falloff LUTs (1D).** Dump Blender's `brush.curve` (each preset) as `strength(t)` for
   t in [0,1], N samples. Perfectly comparable — a pure 1D function. Rust parity test samples our
   `falloff_weight` and asserts within epsilon of the chosen Blender preset. → `falloff_*_ref.json`
2. **Displacement-vs-distance fields (1D radial).** For Grab/Draw/Smooth on a controlled flat
   grid of verts with known radius/strength/drag, dump Blender's per-vertex displacement as a
   function of distance-from-center. Topology-independent, so comparable to our field/mesh
   displacement sampled the same way. → `grab_field_ref.json`, `draw_field_ref.json`
3. **Behavioral scalars.** Dabs-per-unit-travel (spacing), per-stroke accumulation cap, grab
   elasticity model params. Small JSON of measured numbers. → `spacing_ref.json`

**Consequence for the plan:** we build a small **sculpt oracle harness** (Blender python scripts
under `docs/planning/sculpt/oracle/` + a Rust `sculpt_ref` parity module) once, then every feel
phase reuses it. This is the sculpt analog of bevel's `test_spine.rs` + `*_ref.py`.

### D. Recommended first target (proposed — awaiting owner confirm)

**Phase F1 = Falloff curve**, because it (a) is the single most foundational feel lever — it
affects *every* deposit brush; (b) is the most directly Blender-comparable (pure 1D LUT →
cleanest possible parity gate); (c) is low-risk (one function, mirrored CPU/GPU, already under a
golden parity test); and (d) forces us to build the oracle→Rust→parity loop that all later phases
reuse. Grab/Move (owner-flagged as "good in Blender, nothing to invent") is the strong Phase F2
follow-up once the harness exists.

**Status:** research + methodology recorded. No code changed. Awaiting owner pick of first target
before implementing.

**Next:** on confirm — write `oracle/falloff_ref.py`, capture Blender falloff LUTs, add the Rust
`sculpt_ref` parity test, then reconcile `falloff_weight` to the chosen Blender preset(s).

---

## 2026-07-02 — Session 1 — F0 oracle built + F1 falloff gap analysis (Blender-verified)

**Target feel behavior:** falloff curve (the #1 lever, affects every deposit brush).
**Hypothesis:** AZRA's `falloff_weight` (`e^p`) doesn't match Blender's default sculpt falloff, so
the brush deposits with the wrong shape (wrong "softness").

**Blender reference captured:** `oracle/falloff_ref.py` → `oracle/falloff_presets_ref.json`, run on
**Blender 4.5.1 LTS** headless (`--background --factory-startup`). Dumps every falloff preset as
`w(e)`, e=1−dist/radius (e=1 centre, e=0 rim — same arg as our `falloff_weight` AND Blender's
internal `p`). Presets transcribed from `BKE_brush_curve_strength` (blenkernel/intern/brush.cc):
`SHARP=e²`, `SMOOTH=3e²−2e³` (smoothstep, **Blender's default**), `SMOOTHER=e³(e(6e−15)+10)`,
`ROOT=√e`, `LIN=e`, `SPHERE=√(2e−e²)`, `POW4=e⁴`, `INVSQUARE=e(2−e)`, `CONSTANT=1`.
- **Note:** the live `CurveMapping` cross-check diverges (e.g. SMOOTH maxdiff 0.11) because Blender
  evaluates `curve_preset` ANALYTICALLY at runtime and only reads the CurveMapping widget for the
  `CUSTOM` preset. So `analytic_w` in the json is authoritative; `curvemap_w` just reflects the
  default CUSTOM curve. Recorded so nobody re-chases this.

**Findings (measured, not guessed):**
1. Our `e^p`, p=1+7·hardness ∈ [1,8], reproduces Blender **Linear** (p1, == our hardness-0 default),
   **Sharp** (p2), **Sharper/Pow4** (p4) EXACTLY. It CANNOT reproduce **Smooth** (best-fit err 0.10)
   or **Smoother** (err 0.15) — S-curves aren't power laws — nor **Root/Sphere/InvSquare** (need
   p<1, below our hardness floor). Probe: at e=0.25 SMOOTH=0.156 vs our-default 0.250 (softer rim);
   at e=0.75 SMOOTH=0.844 vs 0.750 (fuller core).
2. **Code reality:** the `Kernel` enum (`brush/mod.rs:57`) already DECLARES `Smooth/Linear/Constant/
   Sphere/Power` and even notes `Sphere = sqrt(1−t²)` (== Blender Sphere). BUT apply (CPU
   `voxel_sculpt.rs` + GPU `voxel_brush.wgsl:257`) ALWAYS computes `fall = falloff_weight(1−t) = e^p`
   and only multiplies a `Power` exp on top (Crease exp3 / ClayBuildup exp2). `from_builtin` emits
   only `Smooth` or `Power` (`mod.rs:376-378`). **`Linear/Constant/Sphere` are dead placeholders;
   there is NO smoothstep anywhere.** `Kernel::Smooth = 0` is MISNAMED — it is the power-law.
3. **Conclusion:** every AZRA brush has always used a pure power-law falloff. Blender's default soft
   smoothstep has never existed in the tool → this is the concrete root of the "doesn't feel like
   Blender" gap.

**Tried:** nothing changed yet — this session is F0 (harness) + measurement only.
**Build/test:** no code touched; `oracle/` is docs. Base committed at `289a3ab`.
**Kept:** oracle harness + `falloff_presets_ref.json` (checked in, ground truth for F1).
**Next / open DECISION (owner):** how to reconcile — (A) minimal: swap the single base curve to
Blender smoothstep as the hardness-0 default; (B) full: wire `kernel_code` in CPU+GPU apply to the
Blender preset family (append `Smoothstep/Smoother/Root/InvSquare` to the frozen enum, fix the
misnamed `Smooth`, add UI picker) — the architecture's evident intended end-state; (C) hybrid:
do (A) now for an immediate feel win, land (B) incrementally as the Kernel slot. All are
core_math+graphics (+sculpt UI panel) — NO collision with the app-crate de-god split. Any choice
must keep CPU↔GPU byte-identical and update the golden parity baseline deliberately.

---

## 2026-07-02 — Session 2 — F1a IMPLEMENTED (default falloff = Blender "Smooth")

**Owner decision:** Hybrid — F1a now (default = Blender smoothstep), F1b next (full preset family).

**Target feel behavior:** the base radial falloff every deposit brush uses.
**Hypothesis (Session 1, Blender-verified):** AZRA's power-law `e^p` can't be Blender's default soft
S-curve; every brush was linear-or-sharper, never Blender-soft.
**Blender reference:** `oracle/falloff_presets_ref.json` SMOOTH = `3e^2-2e^3` (Blender 4.5.1).

**Tried (the ONE curve, mirrored CPU+GPU op-for-op):**
- `voxel_sculpt.rs::falloff_weight` — was `e^(1+7h)`; now `soft*(1-h) + sharp*h`, where
  `soft = 3x^2-2x^3` (Blender Smooth, the h=0 default) and `sharp = x^8` (h=1 pointed core).
- `graphics/src/voxel_brush.wgsl::falloff_weight` — identical formula op-for-op so CPU/GPU stay
  inside the 2e-3 parity gate. Endpoints preserved: `w(e,1) == e^8` exactly (old max-sharp
  unchanged); `w(0,h)=0`, `w(1,h)=1` for all h; monotonic in e and in h.
- The smoothstep<->e^8 mix is explicitly an F1a BRIDGE for the hardness knob until F1b brings the
  real preset family.
- Corrected docs + two stale value-test messages ("hardness 0 is linear") + GPU-parity "identity
  ramp" comments.

**New gate:** `voxel_sculpt::tests::falloff_h0_matches_blender_smooth` — `include_str!`s the checked-in
Blender json, asserts `falloff_weight(e,0)` == SMOOTH within 1e-4 (sculpt analog of poly `*_ref.json`
gates). GREEN.

**Result / Build+test:**
- `cargo test -p core_math` — **327 passed, 0 failed** (3 falloff tests + new Blender gate).
- `cargo test -p graphics` — Naga WGSL-parse GREEN; GPU CPU-parity test GREEN. 1 FAILURE
  (`validate_glsl_floats_per_prim_sync`) is **PRE-EXISTING, unrelated** — `pathtrace_primary_full.comp`
  has `#define FLOATS_PER_PRIM 288u` vs Rust `292` (stale DEAD analytical-path GLSL; every other .comp
  already 292u). My diff has zero FLOATS_PER_PRIM / .comp edits. FLAGGED, not fixed (dead path +
  needs .spv recompile → out of F1a scope).
- `cargo build -p app` — **clean (exit 0)**.

**Why it's right:** single-source curve, both paths change identically (parity holds structurally),
no new render path, no de-god collision (core_math + graphics only). Not a bandaid — makes the
default correct against a proven reference.

**Manual A/B to verify in-app (owner):** Sculpt mode, Draw, hardness 0, one dab on a sphere.
Before = linear cone (hard-ish rim, peaky centre). After = soft-shouldered, fuller-centred dab
(Blender "Smooth"). At hardness 1 both identical (e^8), confirming only the soft end changed.

**Kept.** Committed.

**Next (F1b):** wire `kernel_code` in CPU+GPU apply to the full Blender preset family (append
`Smoother`/`Root`/`InvSquare` to the `Kernel` enum, make `Sphere`/`Linear`/`Constant` live, fix the
misnamed `Smooth=0`, add a UI preset picker). `app/src/types/mod.rs::PropFalloff` ALREADY implements
the Blender family for SDF props — reuse its formulas. Then F2: capture `grab_field_ref.py` (Grab).

---

## 2026-07-02 — Session 3 — FEATURE: one-shot "Symmetrize" button (owner request)

**Request:** "a button to symmetrise the chosen axis" — a one-shot mirror to make the current
sculpt symmetric NOW, distinct from the existing live per-stroke Symmetry X/Y/Z toggles.

**Implemented (mirrors the `smooth_all` whole-grid-op pattern end-to-end):**
- `core_math::VoxelGrid::symmetrize(axis, positive_to_negative, rec)` — copies the master half of
  the density field onto the mirror half across local axis (0=X/1=Y/2=Z). Reads the disjoint SOURCE
  half, writes only the DEST half (no read-after-write aliasing); odd-res centre voxel maps to itself
  and is skipped. Works for dense AND sparse (`BrickGrid::set` allocates a brick on write, verified).
  Records each overwritten voxel's PRE value (first-touch dedup) → ONE reversible sculpt-undo delta.
  Returns changed count (0 when already symmetric). Density only — paint colours NOT mirrored yet
  (documented v1 boundary; the colour-undo lane is separate).
- `app::AzraApp::voxel_symmetrize(axis, positive_to_negative)` — byte-for-byte the `voxel_smooth_all`
  wiring: `materialize_cpu_from_atlas` → `voxel_stroke.begin` → `grid.symmetrize` → `take_dirty` →
  `voxel_mark_chunks_in_region` (live re-mesh) → push `UndoKind::Cpu` (its OWN sculpt-undo path, NOT
  `transaction()`) → `mark_dirty(RENDER)`. Ctrl+Z reverts the whole mirror.
- UI (`ui_panels.rs`): a "Symmetrize" row with X/Y/Z buttons right under the Symmetry toggles;
  deferred flag `symmetrize_axis` set on click, acted on at the panel's action site (same pattern as
  the Smooth All button). Default direction +half → −half; tooltip states it + the undo + shape-only.

**Build/test:** `cargo test -p core_math` — **328 passed, 0 failed** (new
`symmetrize_mirrors_half_idempotent_and_records_reversible_delta`: mirrors the empty half, idempotent
on re-run, recorded delta reverts to the exact pre state). `cargo build -p app` — clean. No shader
touched. core_math + app only — no de-god collision (`ui_panels.rs`/`input_sculpt.rs` are sculpt-owned).

**Kept.** Committed.
**Manual test (owner):** Sculpt mode → sculpt an asymmetric bump on ONE side → click Symmetrize **X**
→ the other side mirrors it. Ctrl+Z reverts. "already symmetric" toast if nothing to do.
**Follow-up:** mirror paint `colors` too (needs the CpuColor undo lane) + optional direction toggle.

---

## 2026-07-02 — Session 4 — F-Clay: Blender Clay Strips oracle + gap analysis

**Owner decision:** reverse-engineer ClayBuildup against **Blender Clay Strips**, **full scope**.

**Blender reference captured:** `oracle/claystrips_ref.py` → `oracle/claystrips_ref.json` (Blender 4.5.1).
Dumps Clay Strips vs Clay vs Draw vs Flatten sculpt params.
- **CAVEAT:** a fresh `bpy.data.brushes.new` gives Blender's GENERIC sculpt defaults, so the curated
  Clay-Strips `tip_roundness` (~square) and `plane_offset` did NOT come through (both read generic:
  tip_roundness=1.0, plane_offset=0.0). To get curated values we'd load the essentials asset-library
  Clay Strips brush (finicky headless) or tune by feel. The AREA-plane finding below is solid.
- **SOLID finding:** `sculpt_plane = AREA`, `area_radius_factor = 0.5`, `normal_radius_factor = 0.5`
  for Clay Strips AND Clay AND Flatten. Falloff `curve_preset = CUSTOM`.

**Blender Clay Strips algorithm (from `do_clay_strips_brush`, corroborated by the params):**
1. **Area plane:** average the surface position (`area_co`) + normal (`area_no`) over the inner
   ~50% of the brush (`area_radius_factor=0.5`). NOT the single contact point.
2. **Target plane:** `plane = area_co + area_no * plane_offset * radius` (small positive offset =
   build up slightly above the surface).
3. **Cube tip:** test vertices with a box/cube tip (`tip_roundness` rounds the corners) → the angular
   "strips" footprint. Round tip = `tip_roundness 1.0`.
4. **Converging fill:** move each vertex TOWARD the plane by `strength·falloff` — it FILLS toward the
   plane and stops (converges). Repeated passes pack evenly; they do NOT keep spiking.

**AZRA ClayBuildup today (voxel_sculpt.rs ClayBuildup arm + from_builtin):**
- Plane = **single contact point + its surface normal** (spikier, less even). NOT area-averaged.
- Deposit = `cur + sgn·(fall²)·strength` where the voxel is below `contact_plane + 0.10·radius`.
  This is **unbounded ADDITIVE** — every stamp adds a fixed amount until the region saturates to
  solid, so the surface **overbuilds past the plane and spikes** on repeated passes. It does NOT
  converge to the plane.
- Falloff base is now smoothstep (F1a) → ClayBuildup = `smoothstep(e)²`.

**The two feel-defining gaps (ranked):**
- **G1 — Converging fill vs unbounded add (BIGGEST).** Blender fills toward a plane and stops →
  packable, even clay. AZRA keeps adding → spikes. Fixing this alone makes it feel like clay.
- **G2 — Area-averaged plane vs contact-point plane.** Blender averages the surface under the inner
  half of the brush → even, smooth build across bumps. AZRA uses one point → follows local wobble.
  Requires a NEW capability: sample the voxel surface under the brush to get avg position + normal.
- **G3 — Square tip (`tip_roundness`), optional aesthetic** — the "strips" look; lower priority.

**Proposed phased implementation (each gated, build+test, log):**
- **Clay-1 (G1):** replace ClayBuildup's unbounded additive with a CONVERGING fill toward an offset
  plane — set voxel density toward the plane profile (fill only, never erode), so the surface stops
  at plane+offset. Biggest feel win. Touches the ClayBuildup arm CPU + the GPU PlaneFill op → GPU
  parity must hold. Uses the CURRENT contact-point plane (no new sampling).
- **Clay-2 (G2):** add an AREA-plane computation for the voxel brush (sample surface points +
  gradient normals under `area_radius_factor·radius`, average), feed it as the deposit plane. Bigger
  (new sampling); improves evenness. CPU + GPU.
- **Clay-3 (G3, optional):** box/cube tip with `tip_roundness`.

**Status:** oracle captured + logged. No code changed this session. Awaiting owner pick of depth
(Clay-1 alone for the big win, or Clay-1→2→3 full). Then implement gated, verifying deposit shape
(converges to plane, no spiking; evenness across a bumpy surface for Clay-2).

---

## 2026-07-02 — Session 5 — Clay-1 IMPLEMENTED (converging fill)

**Owner decision:** Clay-1 first, gated (converging fill = the biggest feel win).

**Target:** replace ClayBuildup's unbounded-additive deposit (which overbuilds/spikes) with a
CONVERGING fill toward the offset plane that STOPS — the Blender Clay Strips behaviour.

**Math (identical across legacy CPU / composable CPU / GPU — the parity chain):**
```
off    = radius * plane_offset                 // 0.10r for ClayBuildup
plane  = (p - center)dot(normal) - off         // >0 above the plane, <0 below
target = clamp(0.5 - plane / voxel_size, 0, 1) // solid half-space bounded by plane; iso=0.5 ON the plane
d0     = target - cur
d      = if sgn>=0 { max(d0,0) } else { min(d0,0) }  // build-only (peaks kept) / carve-only (Ctrl)
new    = cur + d * clamp(wk, 0, 1)             // wk = fall^2*strength; delta shrinks -> converges, no spike
```
Key difference vs old: a voxel ON the plane now converges to iso 0.5 (surface HELD at the plane)
instead of being filled to solid (which pushed the surface PAST the plane -> the spike). Buildup
still happens across fresh strokes (the plane rides the rising surface), but each dwell converges.

**Plumbing (new `converge` flag, ClayBuildup only, bit 7 of PackedOp.flags):**
- `brush/mod.rs`: `OpEntry.converge` (+Default false); `from_builtin` sets it true for ClayBuildup.
- `brush/plan.rs`: `flags::CONVERGE_BIT = 1<<7`. `brush/eval.rs`: pack it + `unpack_converge`.
- `voxel_sculpt.rs`: `apply_stamp_plan` decodes it, precomputes `vs = voxel_size()`, adds the
  converge branch to `Operator::PlaneFill`; `apply_brush_legacy` ClayBuildup arm rewritten to the
  BYTE-IDENTICAL converge math (golden parity holds). Clay & Fill arms untouched.
- `voxel_brush.wgsl`: `CONVERGE_BIT=128u`, decode, converge branch op-for-op (renamed `target`->`tgt`,
  a WGSL reserved word; the Naga-parse test caught it, the app build did not since WGSL is a runtime string).

**Build/test:**
- `cargo test -p core_math` — 328 passed, 0 failed. golden_parity_stamp_plan_matches_legacy GREEN
  (composable == legacy bit-for-bit for ClayBuildup across 4 hardness/invert cases).
  clay_buildup_adds_and_is_additive_only still GREEN (converge with build-sign never erodes).
- `cargo test -p graphics` — voxel_brush Naga-parse GREEN; GPU CPU-parity GREEN ON REAL HARDWARE
  (golden_parity_gpu_kernel — WGSL converge matches CPU within 2e-3 for every builtin). Only failure
  = pre-existing dead-path validate_glsl_floats_per_prim_sync (288 vs 292), not mine.
- `cargo build -p app` — clean.

**Kept.** Committed.
**Manual A/B (owner):** Draw a solid slab, switch to Build (ClayBuildup). Dwell the brush on one
spot: BEFORE it kept pumping material into a growing spike; NOW it fills up to a flat plane and
STOPS. Strokes still build layer-by-layer, but evenly/packably. Ctrl = carve down to the plane.
**Next:** Clay-2 (area-averaged plane, even build across bumps; needs surface sampling under the
brush) then Clay-3 (square tip). Curated Clay Strips plane_offset/tip_roundness still to source.

---

## 2026-07-03 — Session 6 — BUG: multi-second sculpt-room freeze → ASYNC Move voxelize

**Symptom (owner, RTX 4090):** selecting/Move-sculpting a ~128–176³ DENSE body froze frames at
1.6–4.6s (near TDR), `[GPU] WARNING: Previous frame … deferring bake dispatch`.

**Diagnosis trail (recorded honestly — two wrong turns before the truth):**
1. First hypothesis (agent-diagnosed): GPU resident-mesh mega-dispatch; Fix 1 = `resident_cap`
   512→96 + drain floor 64→16 (committed `32e87d0`, kept — real but NOT the dominant cost).
2. Added a temp `[STALL]` frame-phase breakdown (app_update) → proved the cost was CPU-side in
   `input` (1.8–3.0s!) and `mesh` (2.5s) phases, NOT dispatch/bake.
3. Added `time_block!` tags on every candidate → named it exactly:
   `revoxel:object_display_mesh` 0.8–1.2s + `revoxel:region_mesh_to_sdf` 0.5–0.9s =
   **`voxel_revoxelize_instances`** — the Move brush's whole-object mesh→SDF voxelize — running
   SYNCHRONOUSLY on the UI thread from BOTH the deferred flush (2s idle timer, mesh phase) and the
   immediate commit (release, input phase). Cost grows with the sculpt (whole-object gather).

**Fix (structural, the `PendingVoxelRebuild` pattern):** `voxel_spawn_move_bake` clones the grid
and runs gather+displace+`region_mesh_to_sdf` on a WORKER thread; instances STAY in
`pending_move_grabs` (the shader keeps displaying the displaced surface — zero visual change);
`voxel_land_move_bake` applies the SDF region atomically (write + ONE Cpu undo + per-instance dirty
marks + drain), polled each frame from `voxel_flush_dirty_chunks`. The whole-object closed-mesh
gather is UNCHANGED (sign-correctness contract, 722051b regression note) — only moved off-thread.
Writer-pause while in flight: density strokes + undo/redo paused (toast); Move strokes still flow
(dense mesh-grab path only); `smooth_all`/`symmetrize`/`resample`/`stamp_sdf`/`sync_node_mesh`
block-land first; `clear_voxels`/project-load CANCEL (`voxel_cancel_move_bake`).

**Adversarial review (3-lens workflow, 19 agents; each finding independently verified):** confirmed
6 real defects in my first async draft, ALL FIXED before commit:
- **P0** sparse/chunkless Move falls to the ELASTIC path (writes grid every drag frame) — the
  in-flight gate now requires the dense mesh-grab precondition, and `voxel_resample_to` drains the
  whole grab stack so grabs never survive onto a sparse grid; spawn refuses during a rebuild.
- **P0** `stamp_sdf_into_active_sculpt` wrote the grid ungated → block-land barrier added.
- **P0** `clear_voxels` + land resurrected a patch of the deleted sculpt → cancel added.
- **P1** save/leave-edit dropped grabs appended during a flight (`voxel.take()` destroyed them) →
  sync + resample now drain in a flush+block-land LOOP until empty.
- **P2** project load landed a pre-load bake onto reset undo stacks → cancel at load.
- grew-abandon: the spawn-time grow resizes the grid, so even abandoned lands must spawn the mesh
  rebuild (else wrong-sized chunk grid).

**Build:** green. **Owner A/B:** same repro — Move strokes flow freely; the voxelize happens
invisibly ~2s after the last release; `[STALL]`/`release:*` spikes gone (a `move_bake:land_drain`
spike may appear — bounded remesh tail). The temp `[STALL]` + `time_block` instrumentation is KEPT
until owner confirms, then strip.
**Bug class eliminated:** O(object) work on the frame thread for the Move workflow. Remaining known
follow-ups: `duplicate_voxel_object` copies the node grid without draining pending grabs
(pre-existing staleness, general-owned file — flagged, not fixed); worker could be parallelized
(rayon gather) to shorten the background bake, not needed for responsiveness.

---

## 2026-07-06 — Session 7 — BUG: Ctrl+Z on Move STILL fails (carried over from conv 5361dd7a)

**Status:** OPEN. This is the unfinished thread from the previous session — it ended on owner
"ctrl still fail" with a `[MOVEBAKE]` log pasted but never adjudicated. Perf is DONE (Session 6);
this entry is undo-correctness only. Cross-ref: `COMPLETE_BUG_TRACKER.md` +
`.claude/worktrees/sculpt-tool/SCULPT_HANDOFF.md` (root-cause fix plan).

**Prior state (both commits in HEAD dddbae7):**
- `316042c` perf(sculpt): ASYNC Move voxelize on a worker thread — killed the 1.7–4.6s freeze.
- `fb51876` fix(sculpt): grid-grow triggers on the DESTINATION bound only (`|anchor+drag|+outer_r`)
  so inward/tangential border moves no longer grow + reset undo; undo is never silent ("Nothing to
  undo" toast); `[MOVEBAKE] spawn/land` diagnostics added (`input_sculpt.rs:1000` / `:1083`).
- **Outcome: Ctrl+Z STILL failed after `fb51876`.** So grow-reset (scenario B) was NOT the whole
  cause.

**Target feel behavior:** After a Move stroke, Ctrl+Z undoes THAT move — immediately (even before the
~2s deferred voxelize lands) and after it lands. Blender: Move is instantly undoable, one step per
stroke. Nothing new to invent.

**Hypothesis (ranked):**
- **A (primary suspect).** Move defaults to `move_deferred=true`. On release the grab enters
  `pending_move_grabs` with NO undo entry pushed (`input_sculpt.rs:117-152`, explicit "no undo");
  the undo entry only appears ~2s later on bake-land (`:1085`, gated `push_undo && !delta.is_empty()`).
  During the pending window Ctrl+Z has **no handler for pending grabs** → it no-ops or pops the
  PREVIOUS committed entry while the pending move stays displayed. Feels like "undo is broken."
- **B (secondary, thought fixed by `fb51876`).** A grid-grow move still sets `push_undo=false` →
  permanently non-undoable (by design, like Ctrl+R). Confirm grows are actually rare now.
- **C (edge).** `!delta.is_empty()` guard: a move that barely changes density records nothing.

**Blender reference captured:** none — this is an undo/state-machine bug, not a feel-math bug
(behavioral only).

**DIAGNOSTIC (owner repro, 2026-07-07) — `[MOVEBAKE]` output, RTX 4090:**
```
[MOVEBAKE] undo: voxel order stream empty
[MOVEBAKE] GROW fired → res 147³, undo history reset
[MOVEBAKE] spawn: instances=2 grew=true push_undo=false
[MOVEBAKE] land: delta=12563 push_undo=false grew=true undo_order_len=0
[MOVEBAKE] GROW fired → res 174³, undo history reset
[MOVEBAKE] spawn: instances=4 grew=true push_undo=false
[MOVEBAKE] land: delta=58694 push_undo=false grew=true undo_order_len=0
[MOVEBAKE] GROW fired → res 200³, ...    (repeats)
[MOVEBAKE] GROW fired → res 230³, ...
[MOVEBAKE] GROW fired → res 255³, undo history reset   (dense cap hit)
[MOVEBAKE] undo: voxel order stream empty   (× every Ctrl+Z)
```

**Confirmed scenario: B — and it is NOT rare, it fires on EVERY move.** `grew=true` / `push_undo=false`
/ `undo_order_len=0` on every single stroke; the grid balloons 147→255³ in ~5 strokes. Scenario A
(pending window) is MOOT here — the undo handler IS reached ("voxel order stream empty"), the stream is
just always empty. My earlier ranking (A primary) was WRONG; the log corrects it. The `fb51876`
destination-only trigger tightening did NOT stop grows on outward pulls (normal Move sculpting), and
never addressed the real defect: **`voxel_grow_for_move` calls `voxel_undo_reset()` (`input_sculpt.rs:2221`)
so every grow wipes undo, and the grow-move records nothing (`push_undo = !grew = false`).**

**Root cause (two coupled defects):**
1. **grow ⇒ full undo reset** (`:2221`) + grow-move not recorded (`push_undo=false`) = every grow-move is
   non-undoable. The design comment (`:2164` "acceptable + consistent with remesh") is FALSE for Move —
   Move grows constantly, so Move is effectively never undoable.
2. **grow fires far too often + scales res every time.** Trigger over-pads by the FULL `outer_r`
   (`:2183`) where the falloff plateau = 0 (that material doesn't actually move), and only 15% headroom
   (`:2200`) means the next outward pull re-grows immediately; res ∝ bounds → 147→255³ in 5 strokes
   (perf churn + hits the dense cap fast).

**Tried:** [À REMPLIR — voir le plan ci-dessous, implémenter #1 d'abord]
> **FIX PLAN (corrigé — root cause B, no bandaid):**
> **#1 (direct fix, do first): make grid-grow UNDOABLE.** Add a full-grid snapshot undo entry
>   (new `UndoKind::GridSnapshot` on the `voxel_undo_order` lane): in `voxel_grow_for_move`, BEFORE
>   `resample_to`, snapshot the pre-grow grid (res + bounds + density); push it as the grow-move's undo
>   entry; set `push_undo = true` even when `grew`. Ctrl+Z pops it → restore grid identity (res/bounds/
>   density) + remesh. REMOVE the `voxel_undo_reset()` call inside grow (`:2221`). Bound memory: cap
>   grid-snapshot undo depth / compress (pre-grow grids ≤255³ ≈ ≤66 MB each). This ELIMINATES the class
>   "grow is never undoable" structurally.
> **#2 (feel/perf, separate pass after #1): grow far less often.** (a) headroom 15%→~75-100% so a pull
>   doesn't immediately re-grow; (b) tighten the trigger — pad by the radius where plateau still moves
>   material (≈ inner_r or a plateau-threshold radius), not the full outer_r; (c) consider decoupling
>   res-scaling from every grow. Must not clip geometry — verify against outward pulls.
> REJECTED bandaids: pre-size a huge grid (defeats unbounded sculpt); silence via toast only (fb51876
> already toasts — doesn't make undo work); force sync voxelize-then-undo on Ctrl+Z (stall + timing trick).

### IMPLEMENTATION SPEC — #1 (grid-grow undoable), verified against the code 2026-07-07

**Key insight (makes the fix DEEP, not one-level):** a GridSnapshot captures the ENTIRE pre-grow grid
(identity + content), so restoring it puts the grid back to exactly the identity the OLDER Cpu/Atlas/
Color deltas were keyed against. Because the snapshot marker is pushed AFTER those deltas, undo (pops
back/newest-first) always restores the grid BEFORE replaying any older delta → **the full undo history
survives across grows**. So we DELETE the `voxel_undo_reset()` in grow and KEEP every lane — snapshots
guard the older deltas by ordering. (This is strictly better than today's "reset everything".)

**Async composition:** grow is SYNC in `voxel_spawn_move_bake` (`input_sculpt.rs:916`) BEFORE the worker
spawns, and the density write lands later — so the snapshot (taken before `resample_to`) is pre-grow AND
pre-displacement = one Ctrl+Z reverts BOTH grow and move geometry. Undo already blocks while
`pending_move_bake.is_some()` (`undo.rs:17`) → no race with the in-flight bake.

**Exact edits (file:line anchors current as of dddbae7):**
1. `voxel_sculpt_types.rs:14` — `enum UndoKind { Cpu, Atlas, CpuColor }` → add `, GridSnapshot`.
2. `voxel_sculpt_types.rs` — new bounded stack next to `VoxelUndoStack` (:25):
   ```rust
   #[derive(Default)]
   pub(crate) struct VoxelGridUndoStack { deque: std::collections::VecDeque<core_math::voxel_sculpt::VoxelGrid>, total_bytes: usize }
   impl VoxelGridUndoStack {
       const MAX_BYTES: usize = 512 * 1024 * 1024; // snapshots are big (255³·4B ≈ 66MB) — cap tight
       const MAX_ENTRIES: usize = 8;
       fn grid_bytes(g: &core_math::voxel_sculpt::VoxelGrid) -> usize { /* density.len()*4 (dense) or sparse byte count */ }
       pub(crate) fn push(&mut self, g) -> usize { /* evict oldest (front) to fit budget; return evicted count */ }
       pub(crate) fn pop_back(&mut self) -> Option<VoxelGrid> { ... }
       pub(crate) fn clear(&mut self) { ... }
   }
   ```
   Mirror `VoxelUndoStack`'s push/evict exactly (front-eviction, returns evicted count).
3. `PendingMoveBake` (`voxel_sculpt_types.rs:411`) — add `pub pre_grow_snapshot: Option<core_math::voxel_sculpt::VoxelGrid>`.
4. `main.rs` (~:505, next to `voxel_redo_stack`) — add `pub(crate) voxel_grid_undo_stack: VoxelGridUndoStack,`
   and `voxel_grid_redo_stack: VoxelGridUndoStack,`. Init both in `app_new.rs:190`.
5. `voxel_grow_for_move` (`input_sculpt.rs:2166`) — change signature `-> Option<VoxelGrid>`:
   - just before `resample_to` (`:2214`): `let snapshot = self.voxel.as_ref().map(|v| v.grid.clone());`
   - **DELETE `self.voxel_undo_reset();` (`:2221`)** — keep all lanes (see Key insight).
   - return `snapshot` (Some when grew, None on the early `return false` paths).
6. `voxel_spawn_move_bake` (`:916`): `let pre = self.voxel_grow_for_move(&instances); let grew = pre.is_some();`
   and in the `PendingMoveBake { ... }` literal (`:997`) set `pre_grow_snapshot: pre` and `push_undo: true` always.
7. `voxel_land_move_bake` (`:1085`) — replace the `if bake.push_undo && !delta.is_empty()` block:
   ```rust
   if let Some(pre) = bake.pre_grow_snapshot.take() {
       let evicted = self.voxel_grid_undo_stack.push(pre);
       self.voxel_undo_order.push_back(UndoKind::GridSnapshot);
       prune_front_undo_markers(&mut self.voxel_undo_order, UndoKind::GridSnapshot, evicted);
       self.voxel_redo_stack.clear(); self.voxel_redo_order.clear(); self.voxel_grid_redo_stack.clear();
   } else if bake.push_undo && !delta.is_empty() { /* existing Cpu-delta push, unchanged */ }
   ```
8. `undo.rs:33` (undo match) — add arm:
   ```rust
   Some(UndoKind::GridSnapshot) => {
       if let Some(pre) = self.voxel_grid_undo_stack.pop_back() {
           let cur = std::mem::replace(&mut self.voxel.as_mut().unwrap().grid, pre);
           self.voxel_grid_redo_stack.push(cur);
           self.voxel_redo_order.push_back(UndoKind::GridSnapshot);
           self.voxel_spawn_mesh_rebuild();       // grid identity changed → full off-screen remesh
           self.mark_dirty(DirtyFlags::RENDER); self.toast("Undo: grid grow + move");
       }
       return;
   }
   ```
   `undo.rs:101` (redo match) — symmetric: pop `voxel_grid_redo_stack`, swap into `v.grid`, push old into
   `voxel_grid_undo_stack`, push `GridSnapshot` on `voxel_undo_order`, remesh.
9. `voxel_undo_reset` (`:2050`) — add `self.voxel_grid_undo_stack.clear(); self.voxel_grid_redo_stack.clear();`
   so the LEGITIMATE resets (Ctrl+R remesh `:2140`, clear-voxels `:368`, project load) still wipe snapshots.
   (Grow no longer calls this — step 5.)

**THE ONE SUBTLE INVARIANT (eviction staleness) — handle it, don't skip it:** the grid stack and the
delta stacks evict independently. A Cpu delta keyed to grid@147 is only restorable if its guarding
GridSnapshot@147 still exists. Front-eviction (oldest-first) mostly preserves this (a snapshot is NEWER
than the deltas it guards, so the deltas evict first), BUT because the stacks have separate budgets the
grid stack could drop a snapshot while older deltas linger → a later undo would apply a @147-keyed delta
to a @174 grid = CORRUPTION. **Safe handling:** when `VoxelGridUndoStack::push` evicts the oldest
snapshot, TRUNCATE all undo history older than the newest surviving snapshot (drop the front markers +
their backing entries up to and including the evicted snapshot's marker). Cap MAX_ENTRIES low (8) so this
is rare. A production DCC never corrupts on undo — this invariant is not optional.

**Abandon path:** if a grow happened but the bake is abandoned (worker died / grid changed — land early
returns at `:1051`/`:1065`), the live grid is grown-but-undisplaced. Decide: either push the snapshot so
the grow alone is undoable, OR restore the pre-grow snapshot immediately (revert the grow, matching the
"move discarded" toast). Recommend the latter (clean "nothing happened") — thread `pre_grow_snapshot`
into the abandon branches too.

**Fallback if the eviction-truncation is out of budget for one session:** ship a SHALLOW v1 — keep the
`voxel_undo_reset()` on grow (resets older lanes as today) but ALSO push ONE GridSnapshot so the LAST
grow-move is undoable. Strictly better than today (Ctrl+Z does *something*), and it sidesteps the
cross-stack eviction invariant. Upgrade to the deep version after. Document the choice here.

### EXACT BODIES (copy-ready; grows are dense-only so a full `VoxelGrid.clone()` has no GPU state)

`VoxelGrid` = `{ density: DensityStorage, res: u32, bounds: f32, iso, dirty, colors: Vec<u32> }`, derives
`Clone`; `density.len()` = voxel count. Store full clones (preserves paint, no BrickGrid GPU bookkeeping
since dense). `debug_assert!(!g.density.is_sparse())` at push (a grow should never snapshot a sparse grid).

```rust
// voxel_sculpt_types.rs — next to VoxelUndoStack (:25)
#[derive(Default)]
pub(crate) struct VoxelGridUndoStack {
    deque: std::collections::VecDeque<core_math::voxel_sculpt::VoxelGrid>,
    total_bytes: usize,
}
impl VoxelGridUndoStack {
    const MAX_BYTES: usize = 512 * 1024 * 1024; // grow snapshots are big (255³·4 ≈ 66 MB) — cap tight
    const MAX_ENTRIES: usize = 8;
    fn grid_bytes(g: &core_math::voxel_sculpt::VoxelGrid) -> usize {
        g.density.len() * std::mem::size_of::<f32>() + g.colors.len() * std::mem::size_of::<u32>()
    }
    /// Push a pre-grow snapshot; returns #oldest snapshots evicted. The caller MUST then call
    /// `voxel_truncate_history_for_evicted_snapshots(evicted)` — NOT `prune_front_undo_markers` — so the
    /// deltas the evicted snapshots were guarding go too (else a stale-identity delta corrupts on undo).
    pub(crate) fn push(&mut self, g: core_math::voxel_sculpt::VoxelGrid) -> usize {
        debug_assert!(!g.density.is_sparse(), "grow snapshot must be dense (Move is dense-only)");
        let bytes = Self::grid_bytes(&g);
        let mut evicted = 0usize;
        while self.total_bytes + bytes > Self::MAX_BYTES && !self.deque.is_empty() {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::grid_bytes(&old); evicted += 1; }
        }
        while self.deque.len() >= Self::MAX_ENTRIES {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::grid_bytes(&old); evicted += 1; } else { break; }
        }
        self.total_bytes += bytes;
        self.deque.push_back(g);
        evicted
    }
    pub(crate) fn pop_back(&mut self) -> Option<core_math::voxel_sculpt::VoxelGrid> {
        let g = self.deque.pop_back();
        if let Some(ref g) = g { self.total_bytes -= Self::grid_bytes(g); }
        g
    }
    pub(crate) fn clear(&mut self) { self.deque.clear(); self.total_bytes = 0; }
}

// voxel_sculpt_types.rs — ADD to `impl VoxelUndoStack` AND `impl VoxelColorUndoStack`:
pub(crate) fn drop_front(&mut self, n: usize) {
    for _ in 0..n {
        match self.deque.pop_front() {
            Some(d) => self.total_bytes -= Self::entry_bytes(&d),
            None => break,
        }
    }
}
```

```rust
// CORRECTED land push (input_sculpt.rs voxel_land_move_bake, replaces spec step 7's prune line):
if let Some(pre) = bake.pre_grow_snapshot.take() {
    let evicted = self.voxel_grid_undo_stack.push(pre);
    self.voxel_undo_order.push_back(UndoKind::GridSnapshot);
    self.voxel_truncate_history_for_evicted_snapshots(evicted);   // NOT prune_front_undo_markers
    self.voxel_redo_stack.clear();  self.voxel_redo_order.clear();
    self.voxel_grid_redo_stack.clear();  self.voxel_color_redo_stack.clear();
} else if bake.push_undo && !delta.is_empty() { /* existing Cpu-delta push, unchanged */ }
```

```rust
// Truncation helper (impl FourthLayerApp, e.g. in input_sculpt.rs). Grows are dense-only and a
// sparse→dense resample calls voxel_undo_reset(), so NO Atlas marker can be older than a grow snapshot
// → assert it rather than front-dropping GPU atlas handles.
fn voxel_truncate_history_for_evicted_snapshots(&mut self, evicted: usize) {
    let mut remaining = evicted;
    while remaining > 0 {
        match self.voxel_undo_order.pop_front() {
            Some(UndoKind::GridSnapshot) => remaining -= 1,           // backing snapshot already evicted
            Some(UndoKind::Cpu)          => self.voxel_undo_stack.drop_front(1),
            Some(UndoKind::CpuColor)     => self.voxel_color_undo_stack.drop_front(1),
            Some(UndoKind::Atlas)        => debug_assert!(false,
                "Atlas marker older than a grow snapshot — impossible (sparse→dense resample resets undo)"),
            None => break,
        }
    }
}
```

**Why the truncation walk is correct (worked example):** stream (front=oldest→back=newest)
`[Cpu d1, Cpu d2, GridSnap A, Cpu d3, GridSnap B]`. A restores grid identity G0 (guards d1,d2); B restores
G1 (guards d3). Undo pops back→front: B→d3(on G1 ✓)→A→d2(on G0 ✓)→d1 ✓. Evict A (oldest snapshot): d1,d2
lose their identity → the helper front-drops `[Cpu d1, Cpu d2, GridSnap A]`, leaving `[Cpu d3, GridSnap B]`
intact. No stale-identity delta can ever be replayed onto the wrong grid.

**Note (paint):** the full-clone snapshot keeps `colors`, so a grow-undo restores paint too (better than
`VoxelGridRecord`, which drops colour). If you switch to `to_record()`/`from_record()` for memory, document
that grow-undo then loses paint (consistent with save/load).

**Result:** [À REMPLIR]
**Why:** grow changed grid IDENTITY and threw away undo instead of recording a reversible snapshot.
**Adversarial review:** [À REMPLIR — undo/state-machine + concurrency (snapshot vs in-flight async bake) lenses]
**Build/test:** [À REMPLIR — cargo build -p app ; Move-sculpt hard, Ctrl+Z after each stroke incl. grows]
**Kept? / Reverted?:** [À REMPLIR]
**Cleanup:** strip the temp `[MOVEBAKE]` eprintlns (marked "strip with [STALL]") once confirmed.
**Next:** [À REMPLIR — then Bug #2 Smooth iterations; #3 surface-sculpt = separate plan]

## 2026-08-18 — Session 8 — POLYSCULPT-PLANELESS: poly plane brushes were silent no-ops

**Trigger:** owner, right after the ZB_RESPONSIVE_SCULPT_PLAN P0 landing: "can you check our brush
and see what are the difference with blender all our brush feel bad". Diagnosis first (report, no
fix), then owner "go".

**Finding:** the poly sculpt panel exposes 12 brushes, but `poly_sculpt_apply_dabs`
(app/src/poly/sculpt.rs) passed `area_planes = None` for every dab. The shared kernel
(`displace_verts_for_dabs`) fails closed on a missing plane, so **Clay, Build (ClayBuildup) and
Scrape did literally nothing in poly sculpt**; **SnakeHook** hit the kernel's `_ => {}` (no arm —
its tube implementation is voxel-chunk machinery) = also a silent no-op; **Crease** degraded to the
raw dab normal. The kernel comment "unsupported brushes never reach here (input gate + UI grey-out)"
was false for the poly route. The voxel-SURFACE mode was unaffected (it runs the area-frame
pre-pass and its brushes are the oracle-matched ones).

**Fix (landed):**
- `poly_sculpt_apply_dabs` now fits real per-dab area frames through the SAME pure functions the
  voxel path uses (`accumulate_area_frames` + `resolve_area_frame`) over the working mesh — footprint
  widened to the 1.35× `normal_radius_factor` sub-sphere (query via `sculpt_radius_query`, the PR-9
  shared helper), front-face filtered against the dab direction, trench-cancel still fails closed.
  Zero math duplication.
- SnakeHook: gated fail-closed at `handle_poly_sculpt_input` (toast once per stroke) + greyed in BOTH
  poly pickers (sculpt_panel grid, properties_poly combo) until a real poly port exists.
- Pinned by `poly::sculpt::plane_brush_tests`: clay deposits on a flat grid (+0.5·r per-dab cap),
  scrape one-sided shave, off-mesh fail-closed, snake apply no-op pin. Suites green: poly sculpt 23,
  mesh_sync 79, voxel_surface 9.

**Remaining feel gaps vs Blender (diagnosed, NOT fixed — ranked):**
1. **Always-accumulate** — every deposit brush stacks per dab on LIVE coords (= Blender Accumulate-ON
   permanently). Blender's default builds ONE offset layer per stroke from stroke-start positions;
   `first_touch` (poly) / `surface_stroke_snap` (voxel) already hold exactly that data.
2. **Draw direction = single-point normal** (closest-vert in poly, per-dab follow normal in voxel);
   Blender uses the area normal → stable strips. The area-frame machinery now exists on both paths —
   wiring Draw to it is small.
3. **Plane brushes re-fit per dab from the CURRENT deformed surface** → Flatten/Clay chase their own
   deposit (Clay-2, still open).
4. Spec-deferred: Clay Strips square tip + x-stretch, Snake Hook rake, F2 Grab field; clamp
   saturation (Flatten dead above s=2, Smooth 0.9 cap).

## 2026-08-18 — Session 9 — Accumulate-OFF layer deposit (feel gap #2)

**Owner "go" on gap #2 from Session 8's ranking.** AZRA's deposit brushes always stacked per dab
on live coords (= Blender Accumulate-ON permanently); Blender's default lays ONE offset layer per
stroke measured from stroke-start positions.

**Model:** per-stroke signed `deposit_applied` ledger on `PolySculptWorking`, threaded into the
shared kernel's Draw/Erase/Inflate arms. `ledger_step(applied, target=amp·fade)` returns the
top-up to the stroke's max coverage — Blender's `BRUSH_ACCUMULATE`-unset orco model
(`pos = stroke_start + offset·fade`) expressed *incrementally*, so scatter/dirty-corner/undo
bookkeeping is untouched. Properties: scribbling in place → one layer, stops; a weaker rim dab
never pulls the deposit back down (monotone); Ctrl sign-flip mid-stroke snaps to the flipped
field (orco-consistent). Stroke boundary = `begin_poly_sculpt_episode` clears the ledger.

**Wiring:** the existing Brush Settings "Accumulate" checkbox — previously drawn-but-DEAD in poly
mode — is now authoritative. Poly default seeded OFF once via `poly_ensure_defaults`
(+`poly_tool_seeded` guard); `VoxelBrushSettings::default()` stays `true` because the DENSITY
path reads the same field for its PlaneFill blend, and the voxel factory table already encodes
per-brush accumulate (Draw/Flatten/Crease false, Clay/Fill/Build true). Plane brushes ignore the
ledger by design (they converge to a plane). The voxel-SURFACE caller passes `None` for now — its
density twin has the July buildup cap; porting the ledger to surface mode is the recorded
follow-up at the kernel doc.

**Pins:** `poly::sculpt::layer_deposit_tests` — rescrub-caps (5 re-dabs, drift <1e-5), new
stroke = fresh layer, checkbox restores stacking, Inflate arm capped. Suites green: poly sculpt
27, mesh_sync 79, voxel_surface 9, surface_fuzz 5 (262s, exercises the changed kernel via the
voxel stroke paths).

**Remaining from the Session 8 ranking:** #3 area normal for Draw direction (machinery now on
both paths); #4 plane anchored vs chasing (Clay-2); then spec-deferred (Clay square tip, Snake
rake, F2 Grab field).
