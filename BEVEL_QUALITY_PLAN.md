# BEVEL_QUALITY_PLAN.md

Phased plan to take AZRA's bevel/fillet quality to Blender-grade (and, where it matters, CAD-grade).
Two bevel systems are in scope: the **parametric SDF box/cylinder bevel** (GPU + CPU, multi-path) and the
**polygon edge bevel** (half-edge mesh, app-layer). Every phase is gated, names files, states the multi-path
parity + regression check, and ends with a concrete verify step.

> Authoritative ground truth verified against the live tree on 2026-06-28 (not worktrees). Line numbers below
> are from the live tree. Worktree copies under `.claude/worktrees/**` are NOT in scope — ignore them.

---

## 1. North Star

"Blender-quality bevel" for AZRA means: **drag a width, get a clean, even, notch-free rounded or chamfered edge
that reads as render-ready under reflections, with corners and junctions that never pinch or self-intersect** —
on both the parametric SDF box and the poly mesh. Concretely, parity with the part of Blender's beveler that
actually matters for a hard-surface artist:

- **Notch-free 3-edge corners.** A box vertex where three beveled edges (possibly of *different* radii) meet
  must round into one smooth region — never a hard-max crease. This is the headline defect today.
- **One width, resolvable intent.** A single `width`/`radius` number, plus a chamfer-vs-fillet selector and a
  superellipse-style profile knob, gives predictable results across non-90° topology.
- **Render-ready shading.** Normals flow smoothly across the bevel into flat faces; no gradient spikes, no
  curvature seam under a moving highlight (the G1→G2 upgrade).
- **No self-intersection, ever.** Width clamps to the largest value that won't fold the surface (already partly
  true for SDF via half-extent clamp; must hold for poly too).
- **Poly bevel that actually bevels.** Multi-segment, profiled, corner-filled — not the frozen single-segment
  endpoint-slide chamfer.

### What CLAUDE.md forces on this work (hard constraints, non-negotiable)

1. **Per-Edge Bevel Corner Rule.** "Never bolt a 3D corner sphere onto per-edge bevel via `max()`." Same radius
   on all groups → use `sdRoundBox`. Different radii → **smooth interpolation, not hard `max()`**. The current
   tri-planar `max(d_xz, max(d_xy, d_yz))` is the exact thing this rule prohibits. Phase 0 is enforcing this
   rule, not adding a feature.
2. **`half_edge.rs` is HARD-frozen** (user directive 2026-06-14, re-confirmed in memory). The poly beveler does
   NOT touch it. All poly bevel work lives app-layer / in `core_math/src/bevel_op.rs`, which reads the mesh
   through its public API and rebuilds via `HalfEdgeMesh::from_polygons`. The frozen
   `HalfEdgeMesh::bevel_edges_ex` is superseded, not edited.
3. **Multi-path parity.** Any SDF shape change must be ported to **all** evaluation paths in the SAME task. The
   audit said "5 paths"; the live tree has the box bevel hard-max in **8 files / 11 call sites** (§2). A Phase-0
   fix that lands in fewer than all of them ships a half-fixed feature and silently diverges paths.
4. **Display Invariant.** This plan never fuses render techniques. SDF bevel quality is improved *inside the
   single SDF field function* — the same improved `eval_prim` feeds volume bake, Edit-Mode raymarch, and the
   Object-Mode triangle bake. The poly bevel produces triangles only. No phase introduces a second renderer.
5. **Shaders are expensive + WGSL inlining hazard.** Any of the 5 `sdf_*` WGSL files invalidates 4+ pipeline
   cache entries (25-43s cold recompile). Batch every SDF field edit into ONE pass. WGSL inlines all calls, so
   the corner fix must stay cheap (no recursion, no per-pixel binary search by default — see §4).

---

## 2. Current State (verified, graded)

### Two systems

**A. Parametric SDF bevel** — `BoxBevel { radii:[f32;12], modes:[BevelMode;12], up_axis }` and
`CylinderBevel { top, bot, top_mode, bot_mode }` in `core_math/src/sdf_shapes.rs:18-45`. Tri-planar: at each
octant it picks 3 edge indices (XZ/XY/YZ), computes 3 independent 2D bevels, **combines with hard max**.

**B. Polygon bevel** — `bevel_edges_ex()` (frozen, in `half_edge.rs`) is a single-segment endpoint-slide
chamfer. Called live at `poly_drag_tools.rs:709`. The intended replacement `bevel_edges_offset()` lives in
`core_math/src/bevel_op.rs:73` and is a **stub returning `mesh.clone()`** (line 80); its `WeldMap` corner-weld
infrastructure is written and unit-tested but the offset/corner/rebuild body is unimplemented and unwired.

### The BEVEL-NOTCH root cause

The tri-planar formulation combines three independently-rounded 2D edge fields with `max()`. When two incident
edges at a vertex have **different radii** (or different modes), the three fields disagree about the iso-surface
at the trihedral corner, so `max()` produces a **non-differentiable crease/notch** — visible as a step under
shading, and a normal flip across the crease. This is mathematically the failure mode the CAD/SDF research
identifies (Part 1B/2) and the exact thing the CLAUDE.md Per-Edge Bevel Corner Rule prohibits.

### Where the notch lives — the REAL path count (live tree, verified)

The audit undercounted at "5 paths." The box bevel `return max(d_xz, max(d_xy, d_yz));` (or `.max().max()` in
Rust) appears at **11 sites across 8 live files**:

| # | File | Line(s) | Role |
|---|------|---------|------|
| 1 | `core_math/src/sdf_shapes.rs` | 99 (`distance`), 245 (`distance_profiled`) | CPU analytical (FieldEvaluator, DC, picking) |
| 2 | `graphics/src/sdf_prim_eval.wgsl` | 418 (`sdf_box_per_edge`), 506 (`_profiled`) | WGSL viewport raymarch + deferred |
| 3 | `graphics/src/volume_bake.wgsl` | 217, 296 | GPU volume bake (mainline render input) |
| 4 | `graphics/src/glsl/sdf_functions.glsl` | 348 | GLSL BVH shared include |
| 5 | `graphics/src/glsl/raymarch_bvh_full.comp` | 320, 553 | GLSL BVH raymarch (merged unit) |
| 6 | `graphics/src/glsl/pathtrace_primary_full.comp` | 421 | GLSL path tracer primary |
| 7 | `graphics/src/glsl/pathtrace_bvh_full.comp` | 438 | GLSL path tracer BVH |
| 8 | `graphics/src/glsl/pathtrace_bounce_full.comp` | 447 | GLSL path tracer bounce |

> The GLSL `*_full.comp` files are the merged compilation units; `sdf_functions.glsl` is the template include.
> Per CLAUDE.md GLSL rules, the canonical fix goes in the template/source, and every `*_full.comp` is
> regenerated (insert markers) + recompiled to `.spv` in the same task. Treat all 8 files as one atomic change.

Analytical/path-trace paths are **disabled by default** (`force_volume_only=true`, `bvh_enabled=false`), so the
*shipping* render input is `volume_bake.wgsl` + the Object-Mode triangle bake (also bake-fed). But the fix MUST
still land in all 8 — divergence between paths is itself a CLAUDE.md violation, and the dev-toggle paths must
not silently rot.

### Gap grading

| Gap | Severity | Where | Note |
|-----|----------|-------|------|
| Hard-max notch at mixed-radii corners | **P0 / HIGH** | 8 files / 11 sites above | Rule violation + visible defect. Phase 0. |
| Poly bevel is frozen single-segment chamfer | **HIGH** | `poly_drag_tools.rs:709` → frozen `bevel_edges_ex`; replacement stub `bevel_op.rs:80` | No corner fill, no profile, no segments. |
| No corner fill / smooth junction (SDF) | **HIGH** | all SDF paths | Subsumed by Phase 0 (exact corner) + Phase 4 (G2). |
| Profiled bevel missing in GLSL | MEDIUM | `sdf_functions.glsl` has only `sdf_box_per_edge`, no `_profiled` | Path divergence; WGSL/CPU have it. |
| Cylinder profiled missing in volume bake | MEDIUM | `volume_bake.wgsl` has `sdf_cylinder_beveled` but not `_profiled` | Viewport has `sdf_cylinder_beveled_profiled` (`sdf_prim_eval.wgsl:509`). |
| No per-edge radius UI (group-of-4 only) | MEDIUM | `properties_sdf_shape.rs:64-85` | Data model already per-edge (`radii[12]`); only UI is grouped. |
| No miter / intersection control | MEDIUM | both | Blender Arc/Patch/Cutoff. Later. |
| G1-only fillet (curvature seam) | MEDIUM | all SDF | Circular profile is G1; cubic/cosine → G2. Part 3. |
| No sphere/torus/cone bevel | LOW-MED | `sdf_shapes.rs` | Only box+cylinder. |
| Silent radius clamp | **LOW (mostly done)** | `properties_sdf_shape.rs:47-57` | UI already clamps slider max per group (`top_max/bot_max/vert_max`). Field still clamps internally; acceptable. |
| Subdivision smoothing hack untied to radius | LOW | `sdf_prim_eval.wgsl` subdivision block | Cosmetic; separate from bevel. |

---

## 3. Target Feature Set (prioritized, mapped to AZRA)

Priority follows Blender's own "what makes it production-grade" order, filtered by AZRA's architecture.

### MUST (the non-negotiable core)

- **Notch-free exact corner (SDF).** For uniform radius across an octant's 3 edges → emit the exact `sdRoundBox`
  form (`length(max(q,0)) + min(maxcomp,0) - r`); the single Euclidean term is the CAD "rolling-ball" corner,
  notch-free and exact. For mixed radii → **smooth-blend the three edge fields** (radius-weighted `smin`/poly
  blend) instead of `max`, so the corner stays C1. (§4.) Maps to: one `sdf_box_per_edge` rewrite, ported to all
  8 paths.
- **Chamfer vs Fillet as distinct operators.** Already present (BevelMode::Round/Chamfer + profile). Keep, but
  route both through the new corner-combine so chamfered corners are also notch-free (chamfer = scaled `(a+b)`
  half-space; fillet = Euclidean `length`). CAD Part 4.
- **Segments + superellipse profile (poly).** The poly beveler must produce N≥2 smooth segments with a
  superellipse `shape` knob (0.25=flat chamfer, 0.5=circular, >0.5=convex). Maps to: finishing
  `bevel_op.rs::bevel_edges_offset` (v1 = offset+corner-patch single segment; v2 = segments+profile), wired at
  `poly_drag_tools.rs:709`.
- **Clamp Overlap (poly).** Per-edge width clamp so dragging width never folds the mesh. SDF already gets this
  free (half-extent clamp). `BevelParams.clamp` field already exists.

### SHOULD (the corner-quality + parity layer)

- **Width Method selector (Offset/Width/Depth/Percent/Absolute).** Blender §2. For SDF box, `radius` ≈ Offset;
  the others are reinterpretations resolved to the same `r`. Mostly a poly concern (non-90° corners). Add the
  enum + solver in the poly path; SDF box is right-angle-only so Offset suffices there.
- **Profiled path parity.** Port `bevel_2d_profiled` + `sdf_box_per_edge_profiled` into GLSL
  (`sdf_functions.glsl` + the 3 path-trace `*_full.comp`), and add `sdf_cylinder_beveled_profiled` to
  `volume_bake.wgsl`. Closes the two MEDIUM path-divergence gaps.
- **Per-edge radius UI.** Expose individual `radii[12]` (collapsible "Advanced: per-edge" under the group
  sliders). Data model already supports it; purely `properties_sdf_shape.rs`. No save-format change (radii
  already serialized).
- **Loop Slide (poly).** Toggle: new inner edges follow existing edge directions (clean flow) vs perpendicular
  (uniform width). Blender §5.
- **Miters (poly + SDF where feasible).** Inner/outer Sharp/Patch/Arc + Spread. Hard problem; poly-first.
  Blender §6.

### LATER (advanced authoring + shading polish)

- **G2 corner profile (cubic/cosine).** A profile selector adding curvature continuity → no reflection seam.
  Part 3. SDF: optional 12-iter binary-search corner profile (gated; expensive in WGSL — GLSL-first).
- **Custom profile curve widget.** Blender §4 custom profile; AZRA already has `CurveProfile` infrastructure
  (`core_math/src/curves.rs`) to reuse.
- **rho / tension knob.** Single scalar morphing elliptical↔parabolic↔hyperbolic (CAD conic fillet). Maps onto
  the profile selector — one slider.
- **Intersection fill (Grid Fill / Cutoff) for poly N-way junctions.** Blender §7.
- **Harden Normals + Face Strength + Material Index for the bevel strip.** Blender §9. Render-ready shading
  integration (poly side; SDF gets smooth normals from the field automatically once the notch is gone).
- **Sphere/torus/cone bevel + variable radius along an edge.** CAD variable-radius fillet.
- **Vertex bevel mode** (Blender §1) — shares the core once segments+profile land.

---

## 4. The Corner Problem — the precise notch fix (one change, all 8 paths)

### Why it breaks (math)

Three independently-rounded edge distances `d_xz, d_xy, d_yz` are each a correct SDF for *their own* 2D edge,
but at the trihedral corner they describe three different iso-surfaces. `max()` of three fields is C0 only: its
gradient is discontinuous along the ridges where the active field switches. With **equal** radii those ridges
happen to coincide with the true corner and the artifact is mild; with **mixed** radii the switch surface cuts
across the rounded region → visible crease/notch + normal flip.

### The fix (two regimes, one function)

The CAD/SDF research gives the canonical answer: **round the 3D corner with a single Euclidean `length()` over
clamped per-axis distances, never by `max()`-combining separately-rounded edges.**

**Regime A — uniform radius in the octant (all 3 edge radii equal, all Round):**
emit the exact rounded box directly. In octant-local space with `q = abs(p) - b + r`:
```
d = length(max(q, 0)) + min(max(q.x, max(q.y, q.z)), 0) - r
```
This is exact (gradient length 1) and rounds the 3-edge corner as a true sphere octant — the "rolling-ball"
corner — with **zero notch**. No `max` of edges anywhere.

**Regime B — mixed radii / mixed modes in the octant:**
keep selecting the active edge radius per region by **coordinate sign first** (the existing octant `xi/zi/base_y`
selection is already correct and stays), but replace the final `max(d_xz, max(d_xy, d_yz))` with a
**smooth combine** instead of hard max:
```
// radius-aware smooth max. k scales with the local corner radii so the blend
// zone matches the bevel, not a fixed constant ("works on my machine" forbidden).
fn smax3(a, b, c, k) -> f32 { return smax(smax(a, b, k), c, k); }
// smax(a,b,k) = polynomial smooth-max, k = blend half-width derived from min radius of the 3 active edges
```
With `k` derived from the active edge radii (e.g. `k = 0.5 * min_nonzero_radius`), the three fields blend into a
single C1 corner region — the smooth-interpolation-not-hard-max that CLAUDE.md mandates. When the three radii are
equal this must reduce to (and is preferably replaced by) Regime A's exact form.

> **Lipschitz note (CLAUDE.md march-loop rule).** `smax`/`smin` underestimate distance in the blend zone
> (non-Lipschitz). That's already handled: the march loops carry the ω=1.4 over-relaxation backtrack guard, and
> the volume bake samples on a grid (no march). Do NOT remove that guard. Prefer Regime A (exact) wherever the
> octant is uniform so most pixels stay on an exact field; Regime B only kicks in at genuinely-mixed corners.

### Decision for Phase 0 (opinionated)

Implement **both regimes in one helper** `box_bevel_combine(d_xz, d_xy, d_yz, r_xz, r_xy, r_yz, modes…)`:
- if all three active edges are Round with equal radius → Regime A exact `sdRoundBox` (compute from `q`, not
  from the three `d_*`);
- else → Regime B `smax3` with `k` from the active radii;
- chamfer edges fold in via the chamfer half-space inside each `d_*`, then the same `smax3` combine (a chamfered
  corner blended smoothly is still notch-free and matches CAD "distance-angle" chamfer at the corner).

This is a **single logical function** authored once, then transcribed verbatim into WGSL (×2 funcs), GLSL
(template + 3 path-trace units + raymarch unit), and Rust (×2). It is small, branch-light, recursion-free →
safe for WGSL inlining. Keep `k` and the equal-radius epsilon as named constants shared in spirit across paths
(documented, not magic).

### Porting to all paths in ONE change (the procedure)

1. Author + validate the helper in **Rust** (`sdf_shapes.rs`) first — it's the only path you can unit-test on
   CPU. Add a test: sample the field along the corner diagonal of a mixed-radius box; assert the numeric
   gradient is continuous (no step > tolerance) where the old hard-max had a jump.
2. Transcribe into **WGSL** `sdf_box_per_edge` + `sdf_box_per_edge_profiled` (`sdf_prim_eval.wgsl:418,506`) and
   `volume_bake.wgsl:217,296` — **one batched edit** (shader recompile is 25-43s; do not iterate).
3. Transcribe into **GLSL** template `sdf_functions.glsl:348`, then regenerate the merged units
   (`raymarch_bvh_full.comp:320,553`, `pathtrace_primary_full.comp:421`, `pathtrace_bvh_full.comp:438`,
   `pathtrace_bounce_full.comp:447`) via the INSERT markers and **recompile the `.spv`** — same task.
4. Run `cargo test -p graphics` (35 Naga parse tests + GLSL FLOATS_PER_PRIM sync) and `cargo test -p core_math`.

No data-model change: `radii[12]` + `modes[12]` already carry everything the combine needs. **No save-format
change, no undo change** for Phase 0.

---

## 5. Phased Roadmap (gated, lowest-risk-highest-value first)

Each phase: **Goal · Files · Parity+regression · Verify.** Do not start a phase until the prior phase's verify
passes. One phase per agent/prompt (CLAUDE.md feature discipline).

### Phase 0 — Kill the notch (P0, do this first)

- **Goal.** Replace the hard-max corner combine with the exact/smooth combine of §4 across all 8 paths. No new
  UI, no new fields, no behavior change for un-beveled or single-radius boxes beyond removing the crease.
- **Files.** `core_math/src/sdf_shapes.rs` (99, 245 + new helper); `graphics/src/sdf_prim_eval.wgsl` (418, 506);
  `graphics/src/volume_bake.wgsl` (217, 296); `graphics/src/glsl/sdf_functions.glsl` (348);
  `graphics/src/glsl/raymarch_bvh_full.comp` (320, 553); `graphics/src/glsl/pathtrace_primary_full.comp` (421);
  `graphics/src/glsl/pathtrace_bvh_full.comp` (438); `graphics/src/glsl/pathtrace_bounce_full.comp` (447);
  recompiled `.spv` for each GLSL unit.
- **Parity + regression.** All 8 sites changed in one task (grep `max(d_xz, max(d_xy, d_yz))` and
  `d_xz.max(d_xy).max(d_yz)` must return **zero** live-tree hits afterward, excluding `.claude/worktrees/**`).
  Regression to check: equal-radius rounded box still looks identical (Regime A reduces to old exact case);
  un-beveled box (`radii=0`) is byte-identical to a plain `sdBox`; chamfer mode still chamfers; volume bake +
  DC mesh + Object-Mode triangle bake all render the corner smooth. Keinert ω=1.4 guard untouched.
- **Verify.** (1) `cargo test -p core_math` new corner-continuity test passes. (2) `cargo test -p graphics`
  35 Naga + FLOATS_PER_PRIM sync pass. (3) `cargo build -p app`; in SDF Edit Mode make a box, set Top group
  r=0.3 Round + Vert group r=0 (mixed) → the corner is a smooth round, no crease/step under orbit. (4) Toggle
  Chamfer on one group → corner still smooth, no notch.

### Phase 1 — Profiled-path parity + cylinder bake (MEDIUM, mechanical, low risk)

- **Goal.** Eliminate path divergence: GLSL gets the profiled box variant; `volume_bake.wgsl` gets
  `sdf_cylinder_beveled_profiled`. No new user-facing feature — closes two parity gaps.
- **Files.** `graphics/src/glsl/sdf_functions.glsl` (+ `*_full.comp` regen + `.spv`); `graphics/src/volume_bake.wgsl`.
- **Parity + regression.** After this, `sdf_box_per_edge_profiled` and `sdf_cylinder_beveled_profiled` exist in
  WGSL viewport, WGSL bake, GLSL, and CPU — all four. Regression: profile slider still behaves in viewport;
  cylinder top/bot profile now matches between viewport and bake (previously bake ignored profile).
- **Verify.** `cargo test -p graphics`; build app; cylinder with Profile Top=+1 looks identical in Edit-Mode
  raymarch and in the baked Object-Mode triangles.

### Phase 2 — Per-edge radius UI (MEDIUM, app-only, zero shader cost)

- **Goal.** Expose individual `radii[12]`/`modes[12]` behind the group sliders ("Advanced ▸ Per-edge"). Group
  sliders stay as the fast path; per-edge for tuning one edge independently.
- **Files.** `core_engine/app/src/properties_sdf_shape.rs` only.
- **Parity + regression.** No shader change (field already per-edge since Phase 0). Every per-edge slider goes
  through `pctx.changed = true` (PropsContext mandatory — never `self.is_dirty`) and gets undo via the normal
  properties transaction. Per-edge max clamp reuses the existing per-group `*_max` logic.
- **Verify.** Build app; set edge 8 r=0.4 Round, edge 9 r=0.1 Chamfer → both render correctly and the shared
  corner between them is smooth (Phase 0 makes mixed-radius corners clean). Undo reverts each edge edit.

### Phase 3 — Poly bevel v1: offset + corner patch (HIGH, the big poly win)

- **Goal.** Implement `bevel_edges_offset` v1 (single-segment offset chamfer with real corner-patch faces via
  `WeldMap`), wire it in place of the frozen `bevel_edges_ex` at the call site. Clamp Overlap on.
- **Files.** `core_math/src/bevel_op.rs` (finish the `mesh.clone()` stub at line 80 per its own TODO);
  `core_engine/app/src/poly_drag_tools.rs:709` (swap `bevel_edges_ex` → `bevel_edges_offset`). **`half_edge.rs`
  NOT touched** — rebuild via `from_polygons` only.
- **Parity + regression.** Poly is a single path (no 8-way concern). Regression: bevel still produces a manifold
  mesh (the `WeldMap` exists precisely to prevent the multi-edge corner tear); undo/save-load of the resulting
  `ImportedMesh` node unaffected (output is still a mesh). Keep `bevel_edges_ex` callable as a fallback until v1
  is verified (CLAUDE.md: implement replacement before removing original).
- **Verify.** `cargo test -p core_math` (manifold + weld tests); build app; bevel a cube edge and a 3-edge
  corner → corner is filled with a real patch face, no tear, no self-intersection at high width.

### Phase 4 — Poly segments + superellipse profile; SDF G2 option (HIGH/LATER split)

- **Goal.** Poly: N≥2 segments sampled along a superellipse profile (`shape` knob). SDF: optional G2 corner
  profile (cubic/cosine) selectable per body for curvature-continuous edges (no reflection seam).
- **Files.** `core_math/src/bevel_op.rs` (v2 body); `core_engine/app/src/poly_drag_tools.rs` (segments/shape
  UI); SDF G2 → the §4 helper across all 8 paths again (batched), gated behind a profile-type field.
- **Parity + regression.** SDF G2 is another all-8-path change — same procedure as Phase 0, same recompile cost;
  do it as ONE batched edit. The G2 binary-search is GLSL-first (WGSL inlining cost) and OFF by default
  (circular remains default). New per-body `profile_type` field → **save-format bump** (append-only enum
  variant; never insert mid-list — bincode positional tagging rule).
- **Verify.** Poly: 8-segment bevel reads as a smooth round; shape=0.25 is a flat chamfer. SDF: enable G2 →
  moving highlight shows no curvature line at the bevel tangent.

### Phase 5 — Miters / intersections / shading integration (LATER)

- **Goal.** Blender miters (inner/outer Sharp/Patch/Arc + Spread), N-way Grid Fill/Cutoff, Harden Normals +
  Face Strength + Material Index for the bevel strip. Width-method selector. Custom-profile curve + rho/tension.
- **Files.** Mostly `bevel_op.rs` + poly UI; Material-Index touches material assignment; reuse
  `core_math/src/curves.rs::CurveProfile` for custom profiles.
- **Parity + regression.** Poly-centric. Material-Index for the bevel strip must reference materials by
  `MaterialId` (CLAUDE.md: materials referenced, never embedded). Any new serialized fields are append-only.
- **Verify.** Per-feature against the Blender behavior in the research doc (miter spread, cutoff face, harden
  normals keeping flat panels flat).

---

## 6. Risks & Invariants

- **Shader recompile cost is the dominant tax.** Each touch of an `sdf_*` WGSL file = 25-43s cold recompile and
  invalidates 4+ pipeline cache entries. Phase 0 and the SDF half of Phase 4 are each **one batched edit across
  all paths** — never iterate a shader edit. Author + debug the logic in Rust (CPU, unit-testable) first, then
  transcribe once.
- **The 8-path parity is the single biggest correctness risk.** The audit's "5 paths" would have left the 3
  path-trace `*_full.comp` units and the raymarch `*_full.comp` divergent. After any SDF field change, grep the
  hard-max pattern (and the new helper name) across the live tree and confirm identical logic in all 8. CI
  already guards Naga parse (35 tests) and GLSL `FLOATS_PER_PRIM` sync — run `cargo test -p graphics` every
  shader phase. Ignore `.claude/worktrees/**` (stale copies; not compiled).
- **`half_edge.rs` is hard-frozen.** No poly phase edits it. `bevel_op.rs` reads via public API and rebuilds via
  `from_polygons`. If a poly feature seems to *need* a `half_edge.rs` change, STOP and report the conflict
  (CLAUDE.md Rule Conflict Check) — do not edit the frozen file.
- **Save-format / undo.** Phases 0-3 add **no** serialized fields → no save bump, no migration. Phase 4's SDF
  `profile_type` and Phase 5's miter/material fields are **append-only** enum/struct additions (bincode tags by
  positional index — inserting mid-list corrupts every existing save; `NodePayload::VoxelBody` must stay last).
  Every new property slider gets undo via the properties transaction + `pctx.changed` (never `self.is_dirty`).
- **Display Invariant holds throughout.** All SDF improvements live inside the field function feeding the single
  bake/raymarch; poly stays triangles. No phase adds a second renderer or fuses techniques.
- **Lipschitz / march safety.** Regime B's smooth combine underestimates distance; the existing ω=1.4 backtrack
  guard must stay. Prefer exact Regime A wherever the octant is uniform to keep most pixels on an exact field.
- **Honest effort estimate.** Phase 0: ~1 focused day (logic is small; cost is the careful 8-path transcription
  + recompiles + the CPU continuity test). Phase 1: ~half day (mechanical). Phase 2: ~half day (UI only).
  Phase 3: **multi-day** — the corner solver + manifold rebuild is the genuinely hard geometry work, even with
  `WeldMap` scaffolding done. Phase 4: SDF G2 ~1 day (all-8 again) + poly segments multi-day. Phase 5: weeks,
  feature-by-feature. The high-value, low-risk wins are Phases 0-2; the poly beveler (3-4) is where the real
  engineering lives.

---

### Phase 0 quick-start (for a fresh session)

1. Read `core_math/src/sdf_shapes.rs:64-100` and `:199-249`. Write `box_bevel_combine` per §4 (Regime A exact +
   Regime B `smax3`). Add a corner-continuity unit test. `cargo test -p core_math`.
2. Transcribe into `sdf_prim_eval.wgsl:395-418` + `:486-506` and `volume_bake.wgsl` (197-217, 276-296) in ONE
   edit. `cargo test -p graphics`.
3. Transcribe into `glsl/sdf_functions.glsl:329-348`, regenerate the 4 `*_full.comp` merged units via INSERT
   markers, recompile `.spv`. `cargo test -p graphics`.
4. `cargo build -p app`; mixed-radius box corner is smooth. Update `COMPLETE_BUG_TRACKER.md` (BEVEL-NOTCH ✅).
