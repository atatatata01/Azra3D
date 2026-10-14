# SDF_TOOLS_PLAN.md — SDF Body Tools Worktree

**Worktree:** `.claude/worktrees/sdf-tools` · **Branch:** `feat/sdf-tools` · **Launcher:** `Dev_Sdf.bat`
**Created:** 2026-07-06 (off `master` @ dddbae7)

Three SDF-body work items, ordered by the CLAUDE.md Priority Ladder. **Item 1 is a real
correctness defect** (fix first). **Items 2 & 3 are greenfield features** (one feature per prompt,
one prompt per agent — FEATURE_RULES).

The four evaluators that MUST stay in lockstep for every SDF change
(CLAUDE.md "Feature Must Work in All Three Paths" + CPU):
| Path | File | Status |
|---|---|---|
| CPU evaluator | `core_math/src/sdf_math.rs` (`eval_scene`) + `primitives.rs` (`world_distance`) | LIVE (FieldEvaluator, tools) |
| GPU bake | `graphics/src/volume_bake.wgsl` | **LIVE — production SDF Edit render** |
| WGSL analytical | `graphics/src/sdf_prim_eval.wgsl` + `sdf_scene.wgsl` | analytical path DEAD/debug-only |
| GLSL BVH | `graphics/src/glsl/sdf_functions.glsl` | analytical path DEAD/debug-only |

> Per `project_analytical_path_permanently_dead` + CLAUDE.md "Analytical Path Status": the
> analytical march paths are **disabled by default** (`force_volume_only=true`). Production SDF Edit
> Mode renders through the **GPU bake + volume raymarch**. So the load-bearing paths here are the
> **bake (`volume_bake.wgsl`) and the CPU evaluator**. Analytical/GLSL are kept parity-correct only
> so a future re-enable isn't broken, but they are not the render path.

`graphics/src/serialize.rs` is the Rust→GPU float-layout bridge for all three.

---

## ITEM 1 — Intersection leaves stale geometry (BUG, fix first)

**Symptom (owner):** "intersection does not do a proper result — broken mesh that should be gone is
still there." I.e. the cut-away region still shows the un-intersected surface.

### Root cause (pinned) — the per-prim intersect bounding-sphere cull is invalid for Intersect
Both production paths skip an `Intersect` prim when the sample point is outside the cutter's
bounding sphere. **Skipping an intersect keeps the accumulator (the *other* geometry) instead of
emptying it** — so everything outside the cutter survives. That is the bug.

- **GPU bake** — `graphics/src/volume_bake.wgsl:1035-1046`:
  ```wgsl
  if (acc < 998.0) {
      let dist_to_prim = length(p - prim_center) - prim_bound_r(i);
      if (dist_to_prim > 0.0) {
          ...
          if (op == 2u && dist_to_prim > L * k_raw) { continue; }  // hard Intersect: k_raw=0 → skips
      }
  }
  ```
  For hard `Intersect`, `k_raw == 0` ⇒ `continue` for every voxel outside the cutter ⇒ `acc`
  retains the un-intersected surface = stale geometry.
- **CPU** — `core_math/src/sdf_math.rs:143-145`: `if sphere_d > k { continue; }` (same defect).

**Why this is the outlier (not a bandaid to "fix"):** the analytical paths already do NOT cull
intersect — `sdf_scene.wgsl:240-243`, `sdf_scene.wgsl:532-535`, `glsl/sdf_functions.glsl:736-739`
only sphere-skip Union/ChamferUnion (`op==0u || op==3u`). BVH prune already self-disables for
intersect bodies (`sdf_scene.wgsl:30-31`). The cull was correct for Union (skip far → min
unchanged) and Subtract (skip far → `max(acc,-d)=acc`), but for Intersect `max(acc,d)` a far point
must yield `d` (large positive = empty), never `acc`. The cull was simply never made
op-aware in the bake/CPU paths.

### The fix (architecturally correct, matches the already-correct analytical paths)
Make the per-prim cull **op-aware**: never `continue` on an `Intersect`/`SmoothIntersect` (op 2)
prim. A point outside an intersect cutter must fold the cutter's positive distance into the
accumulator (`acc = max(acc, dist_to_prim)` equivalent), i.e. just fall through to the normal
`apply_boolean` eval — do not skip. Mirror the exact change in bake + CPU.

- Edit `volume_bake.wgsl` cull block (~1035-1046): drop op-2 from the skip; only Union/Subtract
  keep the far-skip optimization (matching `sdf_scene.wgsl`).
- Edit `sdf_math.rs:129-146`: same — remove the intersect branch from the `continue` cull.
- **Do NOT touch** `sdf_scene.wgsl` / `glsl` cull (already correct) — verify only.

### Secondary suspect (needs repro, likely NOT a bug)
First prim seeds the accumulator raw and drops its boolean op (`sdf_math.rs:124`,
`sdf_scene.wgsl:237-239`, `volume_bake.wgsl:1058`, `glsl:734`). If the FIRST prim in a body is
marked `Intersect`, its op is silently dropped. This is standard DCC "first prim is the base"
semantics (Blender does the same) — treat as **expected** unless the owner's repro has the cutter
as prim #0. Confirm against the actual failing scene before changing base-prim semantics
(that change is riskier — see CLAUDE.md "RAY_CULL First-Prim Guard").

### Enum reference
`core_math/src/primitives.rs:45-53`: `Union{k}`=0, `Subtract{k}`=1, `Intersect`=2,
`ChamferUnion{k}`=3, `ChamferSubtract{k}`=4, `SmoothIntersect{k}`=5. Both Intersect + SmoothIntersect
serialize to `op_type=2u` (`serialize.rs:635/639/…`), hard-vs-smooth distinguished by `k>0`.

### Definition of done (Item 1)
- [ ] `cargo build -p app`, `cargo test -p graphics` (35 Naga parse tests), `cargo test -p core_math`.
- [ ] Repro scene: box (Add) + sphere (Intersect) → only the lens-shaped overlap survives; region
      outside the sphere is gone in the baked volume preview.
- [ ] Verify Subtract + Union still cull correctly (perf unchanged).
- [ ] `COMPLETE_BUG_TRACKER.md` updated (✅ FIXED + 2026-07-06).
- Files: `volume_bake.wgsl` (primary), `sdf_math.rs`; verify-only `sdf_scene.wgsl`, `glsl/sdf_functions.glsl`.

---

## ITEM 2 — Slice tool (blend + slice-plane) + Slice-Duplicate (greenfield)

**Owner ask:** a "slice" using a blend type + a slice space (plane) to create cut lines; plus a
"slice duplicate" that creates a **new body** so parts can be separated easily.

**Interpretation (confirm w/ owner):** "same worktree" = same body *contents*, not a git worktree.
Slice-Duplicate = duplicate the body, then the two copies keep **opposite sides** of the slice
plane → the object splits cleanly into two independently-editable bodies.

### FEATURE_RULES 4 questions
1. New scene object? **Yes** — Slice-Duplicate adds a body via `duplicate_subtree()`.
2. Mutates state? **Yes** — adds a prim (the slice) + a body; through `transaction()`.
3. Frame-thread work? Bake re-fires on the new/edited body (existing dirty path).
4. New render path? **No** — reuses the SDF eval; the slice is just a new half-space shape.

### Design
There is **no plane/half-space `SdfShape`** today (`sdf_shapes.rs:94-112`: Sphere…MeshVolume; the
`Billboard` thin-slab shape-type 8 is the closest). A slice needs one.

- **New shape `HalfSpace`** (`d = dot(p, n) - offset`), appended LAST to `SdfShape`
  (`sdf_shapes.rs`, bincode append-only rule — see MeshVolume comment `:108-111`). Params: plane
  normal `n` + offset (reuse the prim transform: normal = local +Z axis, offset = position along it).
- **Slice = HalfSpace + `Subtract` (or `Intersect`) + blend K.** The "blend type" is the existing
  `BlendProfile`/blend-K on the prim (`primitives.rs:121/153`, `smin/smax_profiled`) — K>0 gives a
  soft/rounded cut, K=0 a hard cut. No new boolean op needed; reuse Subtract/Intersect.
- **Slice-Duplicate action:** `duplicate_subtree()` (`scene_actions.rs:325`, canonical —
  CLAUDE.md:138) → the original keeps the HalfSpace as Subtract (keeps side A), the duplicate flips
  the HalfSpace normal (keeps side B). Wrap in ONE `transaction()`.

Precedents to study (do NOT duplicate): clip-curve per-prim plane cutters
(`volume_bake.wgsl:690-707`, `primitives.rs:506 apply_clip_curves`), poly knife (`core_math/knife.rs`),
voxel `PlaneClip/PlaneFill` (`core_math/brush/mod.rs:183-198`), mesh-CSG `Plane`
(`core_math/mesh_boolean.rs:72-116`). None operate on SDF bodies as a slice → new work, but the
clip-curve plane frame (`cu/cv`) is the pattern to follow.

### Structural checklist (CLAUDE.md:104-112) for Slice-Duplicate
Mutate graph → `recalculate_body_origin()` (both bodies) → `sync_flat_cache()` →
`mark_node_body_vol_dirty()` (both). New body via the `create_new_sdf_body()` / add_child template
(`scene_actions.rs:100`, `app_new.rs:282`).

### Files
`sdf_shapes.rs` (HalfSpace), `sdf_prim_eval.wgsl` + `volume_bake.wgsl` + `glsl/sdf_functions.glsl`
(eval HalfSpace, all paths), `serialize.rs` (shape params + `bound_r`), `primitives.rs`
(`world_distance` HalfSpace), `scene_actions.rs` (slice-duplicate action), `properties_sdf_shape.rs`
(slice UI: plane + blend), input/tool registration (`input_system.rs` — CLAUDE.md Input Discipline).

### Open design decisions (see owner questions)
- Slice as an **interactive plane-placement tool** (click/drag a plane in the viewport) vs just an
  "Add HalfSpace prim" button in properties. Tool is nicer UX, more work.
- HalfSpace is an infinite plane; a bounded "slab" slice = two HalfSpaces (Intersect) — future.

---

## ITEM 3 — Emboss / Deboss (greenfield)

**Owner ask:** emboss (raised) and deboss (recessed) surface relief.

### FEATURE_RULES 4 questions
1. New scene object? No — a new **modifier** on an existing prim.
2. Mutates state? Yes — modifier property edits via `write_prim()` → `mark_prim_dirty()` (partial
   bake). Slider undo mandatory (CLAUDE.md). Never `DirtyFlags::SCENE` for a slider.
3. Frame-thread work? Partial bake of the prim's dirty region (existing path).
4. New render path? No — a new surface-alteration branch in `eval_prim`.

### Design — PRIMITIVE-driven signed relief (owner decision 2026-07-06)
Owner chose **"primitive"** as the relief driver (not noise/texture/text). Emboss/deboss **stamps a
primitive shape as signed surface relief**: inside a primitive-shaped region the host surface is
pushed OUT (emboss/raise) or IN (deboss/recess) by `strength`, with the primitive's SDF profile
giving the relief shape (rounded dome from a sphere, flat mesa/ridge from a box, etc.).

Reuse the existing `VolumeMask` shape system as the stamp primitive — `VolumeMask` already carries
Sphere/Box/Gradient/Noise3D shapes + a transform + falloff (`modifiers.rs:114-200`, GPU `eval_mask`
`sdf_prim_eval.wgsl:661-701`). So the "primitive" is the mask shape, `mask_w` is the profile, and:
- **Emboss**  = `d -= strength * mask_w` (raise the surface by the stamped primitive)
- **Deboss** = `d += strength * mask_w` (recess it)

This is architecturally the Displace modifier (`primitives.rs:468-472`, `sdf_prim_eval.wgsl:789-793`)
but with the **mask primitive** driving *where + what profile* and the sign driving *raise/recess* —
no new texture-sample path, no curve/text dependency, stays inside the modifier+mask architecture.
(Future richer version: let the stamp be any full `SdfShape`, not just a mask shape.)

- **Append `Emboss`, `Deboss` to `ModifierType`** AFTER `Shell` (`modifiers.rs:69-93`, append-only
  bincode rule at `:79`). Add to `is_surface_alter()` (`:92`) and `label`.
- **New GPU mod-codes** (e.g. 10, 11) in BOTH `serialize.rs` blocks (`:775-781`, `:1184-1190`).
- **`bound_r` padding** (`serialize.rs:730-737`, `:1146-1153`): outward emboss relief MUST add its
  amplitude to the bounding radius or the raised detail gets culled.
- **fBm restriction (CLAUDE.md:299-305):** if the pattern uses noise, use `fbm_wgsl` directly — keep
  it OUT of the stripped `#region NOISY_BLEND` chain.

### Files
`modifiers.rs` (append enum, `label`, `is_surface_alter`), `primitives.rs` (`world_distance`
Pass-3 both stack `:462-490` and legacy `:533-544`), `sdf_prim_eval.wgsl` + `volume_bake.wgsl` +
`glsl/sdf_functions.glsl` (new `mod_t` branch, all paths), `serialize.rs` (mod-code + bound_r),
`properties_sdf_shape.rs` (Add-Modifier combo `:251-276` + per-type UI + card color `:301-306`).

### Relief driver — DECIDED: primitive (mask-shape stamp). Texture/text are future follow-ups.

---

## ITEM 4 — Blend profiles limited to Smooth + Chamfer (done) → curve-controlled blend (direction)

**Owner ask (2026-07-07):** "remove all blend types other than Smooth and Chamfer — the others are
noise for the moment; I'd prefer a curve-controlled blend that can mimic all of them."

### Rule conflict → cleared by owner → full removal (DONE)
`BlendProfile` (`modifiers.rs`) is a **bincode-serialized enum** with positional tags. The first pass
(2026-07-06 evening) flagged that deleting variants would corrupt existing `.fl4` (CLAUDE.md
"Serialized enums — never reorder/remove") and limited only the UI. **Owner cleared it 2026-07-07:
we are in development with NO save files**, so the corruption risk is void → the six seam profiles were
**fully removed from the enum + all Rust references**, leaving only `Polynomial` (UI: "Smooth") +
`Chamfer`:
- `modifiers.rs` — `BlendProfile` reduced to `{ Polynomial, Chamfer }`; append-only note kept for the
  future `Curve` variant.
- `sdf_math.rs::smin_profiled` — dropped the 6 seam-profile arms (+ the now-unused `FRAC_1_SQRT_2`
  import); `smax_profiled` inherits it. The `pub fn smin_stairs` helper is left (public, no warning,
  reusable by the curve-blend later).
- `bake_utils.rs::hash_blend_profile` GPU-tag map 8→2; `properties_sdf.rs` `BLEND_PROFILE_CHOICES` 8→2
  (drives all 3 pickers) + `blend_profile_label` 2 arms; `draw_viewport_overlays.rs` label+color
  matches 2 arms; the two `input_viewport.rs` cycle-hotkeys already a Smooth↔Chamfer toggle; dropped a
  now-unused `BlendProfile` import in `properties_sdf_shape.rs`.
- **Shaders (`volume_bake.wgsl`/`sdf_prim_eval.wgsl`/`glsl`) left as-is** — their numeric
  `smin_profiled` branches for codes 2-7 are now **unreachable dead code** (serialize/bake_utils never
  emit a tag > 1), harmless, and will be rewritten wholesale by the curve-blend. No shader recompile.
- Build `-p app` green (0 errors, 0 new warnings); `cargo test -p core_math` 395+ green.

### Direction — curve-controlled blend (NOT yet built; supersedes the parked profiles)
Replace the fixed profile list with a **user-editable blend curve/ramp** that the smin samples, able
to approximate Smooth, Chamfer, and the parked hard-surface seam shapes. This is a real feature, scope
before building:
- **Data:** a small fixed-size curve (e.g. N control points or a sampled LUT) that is bincode-safe —
  ride it on an **in-blob `#[serde(default)]`** field (the `voxel_grids` precedent) or a new
  `PolyMesh`-style runtime side-channel, NEVER a new serialized field on `BlendProfile`/prim structs
  (bincode field-trap — `project_bincode_save_format_field_trap`). Keep `BlendProfile` as the preset
  selector; add `BlendProfile::Curve` (appended LAST) that reads the curve.
- **Shaders:** sample the curve LUT inside `smin_profiled`/`smax_profiled` across ALL FOUR paths
  (`sdf_math.rs`, `volume_bake.wgsl`, `sdf_prim_eval.wgsl`, `glsl` source) — upload the LUT as a small
  uniform/buffer via `serialize.rs`. Watch the NOISY_BLEND WGSL restriction (CLAUDE.md:299-305).
- **UI:** an egui curve-editor widget; presets (Smooth/Chamfer/…) become curve presets that stamp the
  control points, giving "mimic all blend types" for free.
- Open question for owner: curve as a **profile LUT** (1-D remap of the smin transition — simplest,
  covers smooth↔chamfer↔round) vs a full 2-D seam profile (needed to truly reproduce Groove/Tongue/
  Pipe/Engrave cross-sections). Recommend starting with the 1-D LUT.

---

## ITEM 5 — SEAM boolean (panel lines / scribed grooves / weld beads) — DONE 2026-07-13

### What it is
A first-class **`BooleanOp::Seam { width, depth, profile: u8, raised: bool }`** (appended LAST after
`SmoothIntersect` — bincode-positional safe). Behaves like a union/subtract chain step but instead of
adding/removing prim B's volume, it carves (raised=false) or raises (raised=true) a **feature line
along the curve where B's SURFACE crosses the accumulated surface**: panel lines, scribed seams,
contact grooves, weld beads/tongues. Cross-sections: `profile` 0=V (chamfer), 1=U (elliptical round),
2=Square (`SEAM_PROFILE_*` consts in primitives.rs).

### Math (sdf_math.rs `seam_cut`/`seam_op` — the ONE shared implementation all GPU paths mirror)
Directional hg_sdf fOpGroove/fOpTongue family (a = accumulator, b = seam prim field):
- Square = exactly `max(a, min(a + depth, width - |b|))` (fOpGroove).
- V = normalized slanted wall `(a + depth - (depth/width)|b|) / sqrt(1 + slope²)`, clamped by the wall.
- U = scaled-ellipse bound `min(w,d) * (1 - |(a/d, b/w)|)`, clamped by the wall (|grad| <= 1).
- Raised = exact dual `-seam_cut(-a, b, …)` (fOpTongue).
- **EVERY profile is clamped by the wall term `width - |b|`** — this is load-bearing, see cull.
- The v1 deleted symmetric blend-profiles (Item 4) were smin-riders; this is a per-op design with its
  own params, proper UI + hashing — the two are unrelated code paths.

### Cull proof (ITEM 1 discipline — never assume, prove)
`c <= width - |b|` ⇒ `seam_op == accumulator` EXACTLY whenever `|b| >= width + |acc|` (both
polarities). Op-aware far-skip everywhere: **skip when `dist_to_prim > L*(width + |acc|)`**
(fold_prim, volume_bake.wgsl, sdf_scene.wgsl x2, brick_fill, volume_preview, GLSL). Scope form:
`d_aabb > L*(scope_width + |acc|)`. Pinned by `core_math sdf_math::seam_tests::seam_far_b_is_identity`
(+ `fold_prim_seam_cull_is_exact`); f32 note: exact only with positive margin — structurally provided
because `bounding_radius` pads by width+depth, so `sphere_d` underestimates `|b|` by at least that.

### GPU encoding (zero new floats — FLOATS_PER_PRIM unchanged at 292)
- op_type **5 = seam cut, 6 = seam raised** (raised dual-encoded in the tag, like Chamfer's 3/4).
- **k slot** (hot[15]/legacy[15]) = **width** (`blend_k()` returns width; falloff modulates width via
  `compute_local_k`, depth is NOT modulated).
- **blend_profile slot** (hot[73]/legacy[106]) = seam cross-section (serializer profile_override).
- **depth** = hot[86] (was reserved) / legacy[291] (was padding). Scope markers write the same slots.
- `apply_boolean(…)` gained an `sd` param in all shader copies (+ the neutralize stub); scope machines
  read depth from `stack_oi`/`stack_sd`.

### Semantics decisions
- `is_additive()` = false (never contributes own volume); `category()` = **3** (new — width/depth/
  profile/raised edits are partial-bake param edits; switching to/from Seam is a full rebake).
- **Sub-body Seam: YES** — `body_op` Seam falls out of the same dispatch (scope machine op 5/6 +
  `apply_scope_boolean` arm). Parent-empty rule: Seam discards like Subtract/Intersect (sop >= 5).
- Seam as chain base = inert (existing first-prim rule, nothing special).
- ~~**X-key cycle deliberately excludes Seam** (keeps cycling the primary 3; cycling off a Seam body
  returns to Union). Seam is picked from the op dropdowns/buttons.~~ **SUPERSEDED 2026-07-13:** the
  blind X cycle is GONE — X now opens a boolean PIE (poly-pie interaction: hold X → hover → release)
  with Union / Subtract / Intersect / Seam (groove) / Seam (bead). Seam is a first-class pie pick
  (fresh picks get the shared `ui_panels::seam_op_defaults`). See session log below.
- Classifier: Seam prims join the **carvers** list (interior-proof blocker, chunked_dc.rs — a groove
  carves up to depth below the pristine additive surface); `bounding_radius` pads width+depth; grid
  bounds margin covers `width + depth` for raised beads (bake_utils).
- Blend noise: none for seams (op >= 5 routes to the plain combine).
- Scoped to v1: seam fires along |b|=0 ∩ |a|=0 regardless of the B op that "would have been" — a
  union-seam and subtract-seam look identical by construction (the groove is symmetric in b).

### Hash / undo / UI
- `hash_boolean_op` tag 6 hashes ALL FOUR params (bake identity — same commit as serializer reads).
- Per-prim UI (properties_sdf_shape.rs Boolean card): "~ Seam" button + Width/Depth (log sliders),
  Profile combo (V/U/Square), Raised checkbox — all via `pctx` (episode/transaction undo). Generic
  Blend-K + profile/noise/falloff cards hidden for seams. Sub-body UI (properties_sdf.rs): "Seam" in
  the Boolean combo + same params. Menus: `bool_op_menu_items` gets groove/bead entries (outliner +
  viewport context menu). Outliner badge `≈−`/`≈+` (violet).
- Parity gates: `tests_bake::body_mirror_bake_matches_cpu_eval` gained 7 seam scenes (3 profiles x 2
  polarities + sub-body scope) — CPU SceneEvaluator vs PRODUCTION volume_bake: 0 sign flips,
  near-surface |dv| = 0.0000.
- **GLSL `.spv` NOT recompiled** (same as Items 2/3: glslangValidator absent, analytical path
  permanently dead per owner). GLSL *source* updated in full parity across all 5 files; en route the
  stale `pathtrace_primary_full.comp` `#define FLOATS_PER_PRIM 288u` → 292u — the known
  `validate_glsl_floats_per_prim_sync` failure is now GREEN (44/44 graphics tests).

### FEATURE_RULES 4 questions
1. New scene object? **No** (a BooleanOp variant on existing prims/bodies).
2. Mutates state? Via existing `pctx`/`transaction()`/`AppCommand::SetBooleanOp` paths — undo covered.
3. Frame-thread work? **No** (same bake pipeline; op-aware cull keeps eval bounded).
4. New render path? **No** (evaluates inside the existing 3+2 shader copies).

---

## Cross-cutting rules (all three items)
- **Batch shader edits into ONE edit** per file (CLAUDE.md:264-266) — 25-43s cold recompile each.
- After any change: `cargo build -p app`, `cargo test -p graphics`, `cargo test -p core_math`.
- Serialized enum additions are **append-only** (bincode positional tags — CLAUDE.md:387).
- Every user-visible slider/toggle needs undo via `transaction()`; SDF props use `pctx.changed=true`.
- Update `COMPLETE_BUG_TRACKER.md` per fix; keep this plan's session log below current.

## Session log
- 2026-07-06 — Worktree created, plan written from cross-path investigation. Item 1 root cause
  pinned (bake+CPU intersect cull is op-unaware). Items 2 & 3 scoped, awaiting owner design calls.
- Item 1 FIXED — op-aware cull in volume_bake.wgsl + sdf_math.rs, builds+tests green (2026-07-06).
- Item 2 DONE (2026-07-06) — Slice (HalfSpace) shape + Slice-Duplicate action.
  - New `SdfShape::HalfSpace` **appended LAST** to the enum (unit variant, bincode tag = 11, after
    `MeshVolume`). SDF in prim-local space = `p.z` (infinite plane, normal = local +Z; oriented by the
    prim transform, no shape params). GPU **shape-type index = 11** kept identical across all paths.
  - Eval added to: `sdf_shapes.rs::distance` (CPU), `volume_bake.wgsl` (GPU bake — PRODUCTION),
    `sdf_prim_eval.wgsl` (WGSL analytical parity), and the GLSL **source** `glsl/sdf_functions.glsl`
    (`d = eval_p.z`). **`.spv` NOT recompiled** — the GLSL BVH path is permanently dead/disabled by
    default (`force_volume_only`), recompiling needs glslangValidator, and the task authorised skipping
    it; source stays in parity for any future re-enable.
  - `serialize.rs`: shape_type=11 + zero params in BOTH hot serializers; **`bound_r` = 1e6** in both
    per-prim bound blocks (hot[45]/hot[27]) so the per-prim bounding-sphere cull NEVER far-skips the
    infinite slice (verified: cuts the whole body, not just near the prim). `bounding_radius()` +
    `bounding_aabb_half_extents()` also return 1e6 for HalfSpace (CPU cull never-skip + full-body
    re-cut on slice edit, `mark_prim_dirty_aabb` clamps the region to the grid). Infinite planes are
    **excluded** from `mesh::compute_scene_bounds` + `serialize::compute_body_aabb` so a body with a
    slice never balloons its export/scope AABB to 1e6 (they can't define a finite bound anyway).
  - Slice = HalfSpace + `Subtract` + existing blend-K (K=0 sharp cut, K>0 rounded) — no new BooleanOp.
    `add_primitive_with_op()` extracted from `add_primitive_immediate` (no dup) forces op=Subtract.
  - **Slice-Duplicate** (`scene_actions::slice_duplicate`) reuses the canonical `duplicate_subtree()`
    (searched: found at scene_actions.rs:325 — did NOT write a new duplicator). Original keeps side A;
    the duplicate's copy of the slice is flipped 180° about its local X (quat post-mult + Euler
    round-trip, robust for any orientation) so it keeps side B. ONE `transaction("Slice Duplicate")`,
    Structural Mutation Checklist for both bodies (flat-cache sync + both volumes dirty; membership
    unchanged so no origin recalc). UI: "▱ Add Slice" + "⎘ Slice Duplicate" buttons in
    `properties_sdf_shape.rs` (deferred via new `pctx.add_slice`/`pctx.slice_duplicate`); no keybind.
  - Build `-p app` green; `cargo test -p core_math` green (395+); `cargo test -p graphics` 42/43 green
    (only the KNOWN pre-existing `validate_glsl_floats_per_prim_sync` 292-mismatch fails — untouched).
- Item 3 DONE (2026-07-06) — Emboss / Deboss (primitive-driven signed surface relief).
  - REUSE: copied the **Displace** modifier wiring exactly and swapped the driver from fBm to the
    existing **`VolumeMask`/`eval_mask`** (the mask shape IS the stamp primitive; `mask_w` = the smooth
    profile). No new mask code, no new texture/noise path. Enum **appended after `Shell`**.
  - `ModifierType::Emboss`, `ModifierType::Deboss` **appended LAST** to the enum (after `Shell`, honoring
    the bincode append-only rule) → bincode tags 10 & 11. Added to `label()` + `is_surface_alter()`.
  - **GPU mod-codes: Emboss=10, Deboss=11** — set in BOTH `serialize.rs` mod_type maps (block1 ~[64],
    block2 hot[49]) AND the `bake_utils.rs::hash_modifier_type` exhaustive map (all three had no
    wildcard → required for compile). Codes match the shader branch codes (`mod_t == 10u/11u`).
  - Eval branch `d -= strength*mask_w` (Emboss, raise) / `d += strength*mask_w` (Deboss, recess) added
    to all four paths after the Shell branch: CPU `primitives.rs::world_distance` (stack Pass-3 **and**
    legacy single-modifier path), `volume_bake.wgsl` (PRODUCTION), `sdf_prim_eval.wgsl` (WGSL parity),
    `glsl/sdf_functions.glsl` (GLSL **source** parity).
  - **Emboss `bound_r` pad** (same clip-bug class as Items 1&2): Emboss pushes OUTWARD by `strength`, so
    `strength.abs()` is added to the per-prim bounding radius in BOTH `serialize.rs` pad blocks
    ([45]/hot[27]) AND both CPU bound fns (`bounding_radius` + `bounding_aabb_half_extents`) so the
    raised relief is never culled and the CPU dirty region covers it. **Deboss is inward → no outward
    pad** (falls to `_ => 0.0`).
  - **GLSL `.spv` NOT recompiled** — same rationale as Item 2: the GLSL BVH path is permanently
    dead/disabled by default (`force_volume_only`), recompiling needs glslangValidator, and the task
    authorised skipping it. GLSL *source* is kept in parity for any future re-enable.
  - UI: Emboss + Deboss added to the "+ Add Modifier" picker (`properties_sdf_shape.rs`, default init =
    Sphere-mask stamp + strength 0.15 + feather 0.4) and the in-stack `draw_modifier_editor` type combo
    (`ui_panels.rs`, positive-only "Relief" 0..1 slider), plus the green surface-alter card color. Undo
    via the existing `pctx.changed`/`transaction_snapshot_only` path (no `DirtyFlags::SCENE`).
  - Build `-p app` green; `cargo test -p core_math` green; `cargo test -p graphics` 42/43 (only the
    KNOWN pre-existing `validate_glsl_floats_per_prim_sync` 292-mismatch fails — untouched).
- Item 4 DONE (2026-07-07) — blend profiles cut to Smooth + Chamfer.
  - v1 (UI-only): flagged the bincode save-format risk, limited only the picker + hotkeys.
  - v2 (FULL removal, owner cleared the risk — dev/no-save-files): the six hg_sdf seam profiles
    (Stairs/Columns/Groove/Tongue/Pipe/Engrave) **deleted from `BlendProfile`** + all Rust matches
    (`modifiers.rs`, `sdf_math.rs::smin_profiled`, `bake_utils.rs`, `properties_sdf.rs`,
    `draw_viewport_overlays.rs`, `properties_sdf_shape.rs` import). Enum = `{ Polynomial(UI "Smooth"),
    Chamfer }`. Shader numeric branches 2-7 left as harmless unreachable dead code (curve-blend will
    rewrite them). Build `-p app` green, `cargo test -p core_math` 395+ green. Not committed.
  - NEXT: curve-controlled blend (ITEM 4 direction) reproduces the deleted profiles as curve presets.
- Item 5 DONE (2026-07-13) — SEAM boolean (op 5/6): shared `seam_cut` math in sdf_math.rs mirrored in
  volume_bake/sdf_prim_eval/sdf_scene/brick_fill/volume_preview WGSL + all 5 GLSL sources; exact
  op-aware cull (skip when dist > width+|acc|) proven by unit test; zero new prim floats (depth in
  hot[86]/legacy[291]); 7 CPU-vs-GPU bake parity scenes bit-clean; UI (prim + sub-body) with undo;
  hash tag 6 covers all params; en-route fix: pathtrace_primary_full FLOATS_PER_PRIM 288→292 (the
  known graphics CI failure now green). `.spv` still not recompiled (toolchain absent, path dead).
- 2026-07-13 — X boolean PIE replaces the blind op cycle (SDF context). Hold X → 5-slot pie
  (Union / Subtract / Intersect / Seam groove / Seam bead), same look/interaction as the poly
  boolean pie (`pie_menu::sdf_boolean_pie_def`, Esc/RMB cancel, release confirms). Core op types
  only — smooth/chamfer variants stay on the viewport blend-k/profile controls (owner decision).
  Applies to ALL selected non-base prims, or all selected sub-bodies when no prim is selected,
  preserving each target's blend k / seam params; chain base keeps the "no boolean op" toast (pie
  doesn't open); the 2-body X-nest gesture is unchanged. Prim writes route through the extracted
  `set_prims_boolean_op` (the `AppCommand::SetBooleanOp` path — one mutation path, one undo entry
  for multi-select); body writes mirror the old cycle's transaction. Seam fresh-pick defaults
  extracted to ONE source (`ui_panels::seam_op_defaults` + `SEAM_DEFAULT_*`, now shared by the
  Boolean card, sub-body panel, menus, and pie). InputSystem action `"sdf_boolean_pie"`
  (X, Viewport, remappable — shared-X push-direct pattern like `poly_boolean_pie`).
- 2026-07-14 — X pie v2 (owner feedback): LOCKED LAYOUT + one Seam slot + Emboss/Deboss slots +
  mode-aware viewport handle.
  - **PIE LAYOUT RULE (muscle memory, owner-locked):** every boolean pie anchors the 3 mains at the
    poly pie's positions FOREVER — Union TOP (1), Subtract/Difference RIGHT (2), Intersect BOTTOM (3).
    New ops append LEFT (4) then diagonals (5-8); mains never move. Documented in `pie_menu.rs` on
    both `boolean_pie_def` and `sdf_boolean_pie_def`. SDF pie is now 8-sector: 0=Union(top),
    2=Subtract(right), 4=Intersect(bottom), 6=Seam(left), 1=Emboss(top-right), 3=Deboss(bottom-right),
    5/7=spacers (next ops go bottom-left, then top-left).
  - **ONE Seam slot** (was groove+bead): picking Seam applies groove defaults on a fresh pick and
    preserves ALL params (incl. polarity) on an existing seam (`seam_op_defaults(cur, None)`).
    Polarity moved to the viewport handle.
  - **Mode-aware blend-k diamond** (`edge_detect::BlendHandleMode`, priority Seam op > Emboss/Deboss
    modifier > blend k) — SAME handle, SAME `BlendKDrag` tool + episode undo, no parallel drag tool:
    - Seam prim/body: drag = SIGNED depth — right cuts a groove (+), dragging through zero flips to a
      raised bead (−, maps to `raised`); |d| clamped 0.002..0.5 (card Depth range). Label
      "Seam Groove d:…" (violet) / "Seam Bead d:…" (pink). Width stays a card slider (k slot = width
      untouched by the handle).
    - Emboss/Deboss prim: drag = SIGNED relief strength on the LAST relief modifier — same sign
      convention (positive cuts in = Deboss, negative raises = Emboss), crossing zero flips
      `mod_type`; |s| clamped 0..1. Label "Emboss s:…" (green) / "Deboss s:…" (orange).
    - Everything else: classic blend k (unchanged, clamps at 0), click still cycles Smooth↔Chamfer.
      Click-to-cycle is GATED to blend-k mode (seam/relief click = no-op; diamond still consumes the
      click — no select-through).
  - **Emboss/Deboss pie slots** = the properties "+ Add Modifier" state, not a BooleanOp
    (`scene_actions::apply_relief_pie`): per selected non-base prim, flip an existing relief
    modifier's type in place (strength/mask preserved) or push a fresh one; defaults extracted to the
    ONE shared source `ui_panels::default_modifier` (properties combo now uses it too — dedup). ONE
    transaction per pick; `sync_modifiers_to_gpu_fields` + `write_prim`. Bodies-only selection →
    explanatory toast (relief is per-prim). Pie active-highlight via `sdf_bool_pie_active_slot`
    (same priority as the handle).
  - Hash/undo verified: seam depth/raised in `hash_boolean_op` (tag 6), modifier type/strength in
    `hash_modifier`/`hash_modifier_type` (10/11) — handle drags re-bake + one episode undo entry each.
  - Gates: `cargo check -p app` green (0 new warnings); `cargo test -p app --bins -- --test-threads=1`
    149 passed / 0 failed. core_math untouched.
- 2026-07-14 — X pie v3 (owner feedback): collapse Emboss+Deboss → ONE Extrude slot + seam profile
  click-cycle on the diamond.
  - **Emboss + Deboss → one "Extrude" slot** (owner: "emboss and deboss are the same, it is an extrude
    — use the losange to slide between the two"). SDF pie map is now 0=Union(top), 1=Extrude(top-right),
    2=Subtract(right), 3=spacer(bottom-right, freed), 4=Intersect(bottom), 6=Seam(left), 5/7=spacers.
    Mains stay anchored (Union top / Subtract right / Intersect bottom). Picking Extrude on selected
    non-base prim(s) → `scene_actions::apply_extrude_pie` (renamed from `apply_relief_pie`): if no relief
    modifier, ADD one defaulting to **Emboss** (raise/out) at the shared `ui_panels::default_modifier`
    strength; if it already has Emboss OR Deboss, KEEP it (idempotent — re-pick never resets; the diamond
    switches/adjusts). ONE transaction, undoable. Active-highlight (`sdf_bool_pie_active_slot`) lights slot
    1 for EITHER Emboss or Deboss. The mode-aware handle still slides emboss↔deboss (signed strength drag
    through zero) — unchanged, verified.
  - **Seam profile click-cycle on the diamond** (owner: "you forgot different profile (chamfer, soft)
    when we click on losange"). Mirrors the blend-k Smooth↔Chamfer click-cycle exactly: for a seam target
    the diamond CLICK now cycles the seam profile V→U→Square (`SEAM_PROFILE_V/U/SQUARE`), drag stays =
    signed depth. New helpers `edge_detect::seam_next_profile` + `seam_profile_label` (V=Chamfer, U=Soft,
    Square). Writes p.operation via `write_prim`/`set_prims_boolean_op` (prim) and `set_body_op` (sub-body)
    — same mutation path as the pie, no parallel path; each click = one undo entry (episode commit on the
    drag-release twin, `transaction` on the pure-click twin). Both click-cycle sites (drag-release +
    pure-click fallback) updated in lock-step. Click restores depth to the drag-start value so a click is a
    pure profile toggle. Handle label folds profile+polarity+depth: `Seam Chamfer Groove d:0.042` /
    `Seam Soft Bead d:0.042` / `Seam Square Groove d:…` (prim + body labels, `draw_viewport_overlays.rs`).
  - Hash/undo verified: seam `profile` already in `hash_boolean_op` tag 6; relief type in
    `hash_modifier_type` — no new hash work. Gates: `cargo check -p app` green (0 new warnings);
    `cargo test -p app --bins -- --test-threads=1` 149 passed / 0 failed. core_math untouched.
- 2026-07-14 — EXTRUDE becomes a BOOLEAN OP (owner: "extrude does not work, it looks more like a
  scale — why is it a modifier when this is a core SDF capability") + SEAM handle goes 2-AXIS.
  - **`BooleanOp::Extrude { strength, width }`** appended STRICTLY LAST after Seam (bincode-positional,
    append-only). Math (`core_math::sdf_math::extrude_op`, shared by every path):
    `acc' = acc − strength · mask(b)`, `mask(b) = 1 − smoothstep(0, width, b)` — 1 everywhere inside B,
    0 EXACTLY at `b ≥ width`. strength>0 raises (emboss/extrude out), strength<0 recesses (deboss/in).
    This SUPERSEDES the pie's ModifierType::Emboss/Deboss self-stamp (`d −= strength·mask_w` on the
    prim's OWN field ⇒ uniform inflate ⇒ read as a scale) — the new op displaces the ACCUMULATED
    surface A within B's footprint, like Seam. The Emboss/Deboss enum variants + their properties-panel
    modifier UI are KEPT as a separate legacy feature; the pie/handle no longer touch them
    (`apply_extrude_pie` DELETED; pie slot 1 now routes through map_op → `set_prims_boolean_op`).
  - **Far-identity + cull:** `b ≥ width ⇒ mask = 0 ⇒ acc' == acc` bit-for-bit, INDEPENDENT of acc
    (smoothstep clamp saturates). Op-aware cull = skip when `dist_to_prim > L·width` (tighter than
    Seam's `width + |acc|` form). Proven by `extrude_far_b_is_identity` +
    `fold_prim_extrude_cull_is_exact` (core_math). Not a bounded-gradient SDF (L up to
    1 + 1.5|strength|/width in the band): bake samples directly (safe), march paths keep the ω-guard.
  - **Bounds/classifier:** `bounding_radius`/`bounding_aabb_half_extents` pad `width + |strength|`
    (mirrors Seam's width+depth); grid-bounds margin (`bake_utils` max_cut_k) covers the raise; new
    `category() == 4` joins the BrickClassifier CARVER set (recess raises the chain value like a
    subtract; raise is conservative) — the exterior proof needs no special case (field only changes
    where b < width ⊆ the influence sphere). Seam↔Extrude switch = category change = full rebake.
  - **ALL paths, one commit:** CPU `fold_prim`/`apply_scope_boolean`/legacy sdf_volume; GPU op tag 7
    (next free after seam 5/6), width rides the blend_k slot, SIGNED strength rides the SEAM-DEPTH slot
    (hot[86]/legacy[291] — overloaded, an op is either Seam or Extrude, zero new floats); volume_bake.wgsl
    (both eval sites + scope skip + per-prim cull), sdf_prim_eval.wgsl, sdf_scene.wgsl (both stack
    machines), volume_preview.wgsl, brick_fill.wgsl (own cull; fn via extract_bake_sdf_lib), all 5 GLSL
    parity copies (sdf_functions.glsl + 4 *_full.comp — sources only, no .spv recompile: validator
    absent, documented precedent; analytical disabled by default). `hash_boolean_op` tag 7 hashes both
    params (same commit). Sub-body Extrude falls out of scope dispatch (op 7 at SCOPE_CLOSE), verified.
  - **HANDLE (BlendKDrag, extended in place — no parallel tool):** now 2-AXIS with `start_mouse_y` +
    `start_w`. Extrude: VERTICAL = signed strength (up = raise/out, down through zero = recess/in),
    HORIZONTAL = width. **Seam: VERTICAL = signed depth (down = groove deeper, up through zero = bead —
    physical up-raises-material intuition; NOTE this FLIPS the old right=deeper horizontal mapping,
    owner-directed), HORIZONTAL = width** (clamped to the card slider range 0.005..=0.5). Both axes in
    ONE episode (one undo per drag); click-vs-drag test now checks both axes (<4px). Labels:
    `Seam <Profile> <Polarity> d:… w:…`, `Extrude Out s:… w:…` / `Extrude In s:… w:…`. Seam click still
    cycles profile V→U→Square (restores BOTH axes' drag-start values); Extrude click = no cycle
    (consumes). Blend-k handle for normal ops UNCHANGED (horizontal k).
  - **UI:** pie slot 1 = Extrude op (raise default `ui_panels::extrude_op_defaults`, +0.05/w 0.05,
    ±0.5 slider bound); Boolean card gets "▴ Ext" + signed Strength/Width sliders; sub-body Boolean
    combo gets index 4 = Extrude + sliders; context/outliner menus get Extrude (raise)/(recess);
    outliner ▴/▾ icon; active-highlight keys purely on the op (`BlendHandleMode::Relief` deleted).
  - Tests: core_math extrude_tests (far-identity, both-sign cross-sections ≈ strength deep inside,
    scope-combine parity, exact-cull) + 4 new CPU-vs-GPU bake parity scenes
    (extrude_raise/recess_sphere + extrude_subbody_scope_raise/recess) in tests_bake.rs.
    Migration: none (no save-format change; old files unaffected — append-only enum).
