# Terrain via per-prim image displacement — plan (2026-08-04)

**Owner's workflow (this is the load-bearing input, and it changes the verdict):**
> "Most of the edit will be local — add a 20m rock displacement, add another one, add a
> smaller rock. Maybe we have one or two larger displacements, but those we can edit at
> lower res and load the higher resolution mesh and picture slowly."

Terrain is **composed of many LOCAL displaced prims**, not one giant displaced prim. That
distinction is everything: the earlier feasibility verdict ("500m mountain fails, 0.24m per
voxel, multi-hundred-ms bakes") applies to a single 500m primitive. It does **not** apply to
this workflow.

## Why the composition workflow is viable

Measured ceilings (from the feasibility audit): dense/TextureOnly bake caps at **512**,
sparse bricks at **2048**; 1024³ costs 150-300ms wall with 20-70ms frame-thread spikes.

| Unit | Voxel size | Smallest feature | Bake |
|---|---|---|---|
| 20m rock @ 512³ | 3.9 cm | ~8 cm | well under 100ms — **interactive** |
| 20m rock @ 2048³ | 1 cm | ~2 cm | ~300ms — fine for a settle pass |
| 500m single prim @ 2048³ | 24 cm | ~0.5 m | multi-hundred-ms — **not interactive** |

Bakes are **per body** with **dirty-region partials** (`body_volume_cache/mod.rs:313`
`dirty_regions`, `:1814`), so adding rock #37 re-bakes that rock's region, not the terrain.
Thirty 20m rocks are thirty small independent bakes, never one 500m bake.

The owner's "edit at lower res, load higher res slowly" is **already implemented**:
`draft_resolution` (`:321-323`) and `draft_baked_bodies` (`:324-325`) bake at draft
resolution for fast feedback and promote to full res afterwards. This plan must USE that,
not rebuild it.

## The actual blocker (not what it looked like)

Image displacement already exists and already tiles — `DisplaceHeightMap`
(`core_math/src/modifiers.rs:687-732`, bilinear, `rem_euclid` wrapping), wired into CPU eval
(`primitives.rs:835,914`), the bake identity hash (`bake_utils.rs:830-858`), and a full UI
(`properties_sdf_shape.rs:943-1147`).

**The gap is GPU parity.** `sdf_prim_eval.wgsl:888-892` implements Displace as `fbm_wgsl`
only — no image sample — and `volume_bake.wgsl` has no texture binding at all beyond its
R32F output (`graphics/src/bgl.rs:309-319`). So `needs_cpu_full_stack_bake()`
(`primitives.rs:980-984`) returns true whenever `has_image_displace()`, routing every
image-displaced body through the SLOW CPU full-stack bake. **That is the lag — not the
absence of a terrain primitive.**

The material displacement map (`render/src/gbuffer.wgsl:401-416`) is a different feature:
per-MATERIAL, applied in the raster vertex shader, never touching the SDF or booleans. It is
already body-local (`:402`), so "triplanar affects everything" is really "it is per-material
and it is not in the bake".

## NOT doing: a dedicated Terrain primitive

`AZRA_MASTER_ROADMAP.md:25` already decided this — terrain is "SDF + EXR displacement,
heightfield for local detail — **not a new pillar**", and §5 (`:575-605`) concludes "no new
NodePayload variant, no new pillar, no new LOD scheme" because the Phase D cluster LOD *is*
the terrain LOD. The tiling and chunked partial rebuild the owner wants already exist
(`DisplaceHeightMap` wraps; voxel chunks at `voxel_sculpt_types.rs:438-452`). Building a
tiled-LOD terrain primitive would add a 13th payload variant (~25-40 touch points) and a
second LOD scheme that Phase D supersedes.

## Stages

**T1 — image displacement in the GPU bake (THE unlock).** Sample a height texture inside
`volume_bake.wgsl` and `sdf_prim_eval.wgsl` in ONE task (CLAUDE.md both-paths rule), then
delete the `has_image_displace()` branch of `needs_cpu_full_stack_bake`.

**Implementation notes (2026-08-09, T1 bake + live march landed):**
- **Bake:** hot layout **hot[92]** + `HOT_FLOATS_PER_PRIM` = **93**; bake `@binding(8)` atlas.
- **Live march:** full layout **[292]** + `FLOATS_PER_PRIM` = **293**; raymarch/outline
  `@binding(15)` height atlas (avoids outline 5–8). Packed by `serialize_from_graph`.
- Both shaders sample image when index ≠ `HEIGHT_MAP_NONE`, else FBM.
- `needs_cpu_full_stack_bake` no longer forces CPU for single-slot image Displace.
- Bake BGL bindings **0–8**; raymarch BGL includes **15**.

**T2 — CPU evaluator parity.** `sdf_math.rs`/`field_eval.rs` have `fbm_noise` (`:240`) but no
image sampler; required by the both-paths rule.

**T3 — progressive refinement for the 1-2 large displacements.** Use the EXISTING
`draft_resolution` / `draft_baked_bodies` promotion; add mip selection so a big EXR is
sampled at a coarse mip while dragging and the fine mip on settle (the "load the higher
resolution picture slowly" half — the mesh half already works).

**Implementation notes (2026-08-09, T3 landed):**
- `DisplaceHeightMap::downsampled_to_max_side` — repeated 2× box filter.
- Draft bake / early cascade: pack with `DRAFT_HEIGHT_MIP_MAX_SIDE` (64).
- Settle / final cascade: pack full (`max_side = 0`).
- Loader long-side cap raised to `LOAD_HEIGHT_MAP_MAX_SIDE` (2048; was 512).
- CPU eval always samples the full stored map (accuracy).

**Deliberately deferred: vector displacement.** It needs MORE grid resolution than scalar
height to resolve overhangs, so it should not be attempted until T1-T3 prove the resolution
budget in practice.

## Guard rails

- Shader work invalidates 4+ pipeline cache entries (25-43s cold recompile) — batch ALL
  shader edits into ONE pass.
- `cargo test -p graphics` (Naga parse) after any shader change.
- Region tags: any new binding referenced inside a `#region` must have all references inside
  the same tag.
- Many small bodies is the intended shape here; watch draw-call count and mesh-cache memory
  as rock count grows, and measure before optimising.
