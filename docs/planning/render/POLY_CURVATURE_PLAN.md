# Curvature on Polygonal Meshes — Plan

**Date:** 2026-08-09
**Owner ask:** *"WE NEED THE CURVATURE TO ALSO WORK WITH POLY MESH"* — the Add-mask menu greys out
Curvature / Ambient Occlusion / Direction on a polygonal body (`Material.001`, Box-triplanar,
Fill layer).
**Status: IMPLEMENTED 2026-08-10 (P0–P5).** See §9 for what shipped and the two places the design
departed from this plan. Prerequisite reading: `CURVATURE_BOTH_MODES.md` (why the rung is a
sampling domain, not a compositor) and `MESH_CURVATURE_PLAN.md` (the geometric kernel, and why the
field Laplacian is the wrong measurement).

---

## 0. One-paragraph summary

The generator family is disabled on poly bodies by ONE flag, but flipping that flag would ship a
lying control: every generator measures an **analytical SDF field**, a poly body has none, and the
no-field path does not fail closed — it falls through to a hardcoded procedural field
(`sn_field_h`) and to *other bodies'* bounds. The geometric kernel that would be correct
(`mesh_curvature.wgsl`) exists but cannot work on poly geometry either, because the poly raster
mesh emits **one vertex per face corner**, so the index buffer it derives adjacency from is
vacuous — it would return κ = 0 everywhere. The right source is the one place poly adjacency is
already exact and already the source of truth: the post-modifier `HalfEdgeMesh`. Measure κ there
on the CPU, scatter it through the existing n-gon emitter (which fixes corner alignment by
construction), and land it in the **existing** `res×res` bake domain that Noise and Image layers
already prove works on poly. No new compositor, no new render path, no save-format change.

---

## 1. Verified blockers

Every claim below was read in the tree at `bb70b97`, not inferred.

### B1 — The UI gate is a single flag

```rust
// layer_ui.rs:1493
let gr = ui
    .add_enabled(gen.has_sdf, egui::Button::new("Curvature"))
    .on_disabled_hover_text("SDF bodies only");
```

Same gate at [layer_ui.rs:394](../../../core_engine/app/src/material_panel/layer_ui.rs#L394)
(Add generator), [:968](../../../core_engine/app/src/material_panel/layer_ui.rs#L968) (source
dropdown), [:1514](../../../core_engine/app/src/material_panel/layer_ui.rs#L1514) (AO),
[:1532](../../../core_engine/app/src/material_panel/layer_ui.rs#L1532) (Direction), and
[channel_row.rs:705](../../../core_engine/app/src/material_panel/view/channel_row.rs#L705).

`has_sdf` is computed once at
[view/mod.rs:391-405](../../../core_engine/app/src/material_panel/view/mod.rs#L391) as
"`body_primitives(active_body_nid)` is `Some` and non-empty". A `PolygonalBody` has no SDF prim
array, so it is always `false`.

### B2 — The no-field path does NOT fail closed (this is the one that forbids just flipping B1)

Two independent traps, both in
[graph_eval/mod.rs](../../../core_engine/app/src/node_graph_bake/graph_eval/mod.rs):

**B2a — analytic fallback.** `sdf_at` ([:146-161](../../../core_engine/app/src/node_graph_bake/graph_eval/mod.rs#L146)):

```rust
if self.body_prims.is_empty() {
    return sn_field_h(p); // analytic fallback
}
```

A poly body would not read as "no curvature" — it would read as **the curvature of a hardcoded
procedural field**, i.e. a plausible-looking pattern with no relationship to the mesh. That is
precisely the "false result that looks like a feature" class CLAUDE.md § *No Hacks* forbids, and
it is worse than the zeros one would naively expect.

**B2b — cross-body bounds leak.** `has_field()`
([:163](../../../core_engine/app/src/node_graph_bake/graph_eval/mod.rs#L163)) is
`!body_prims.is_empty() || !by_uid.is_empty()`, and `snapshot_sdf_field_for_material` fills
`by_uid` with **every prim in the scene**
([bake_dispatch.rs:546](../../../core_engine/app/src/node_graph_bake/bake_dispatch.rs#L546)).
`body_aabb(0)` then falls through to bounding all of them
([:194-198](../../../core_engine/app/src/node_graph_bake/graph_eval/mod.rs#L194)). So in any scene
containing one SDF body, a poly body's generator would silently measure and bound *someone else's*
geometry. This must be fixed whether or not poly curvature ships — a poly-only scene and a mixed
scene currently disagree about what "no field" means.

> Both traps are pre-existing and unreachable today *only because* B1 blocks the UI. B1 is
> therefore load-bearing, not cosmetic: removing it without P0 below is a regression, not a
> feature.

### B3 — The geometric kernel cannot see poly buffers

`encode_mesh_curvature_dispatch`
([gpu_curvature.rs:289](../../../core_engine/app/src/gpu_curvature.rs#L289)) is the right *idea* —
surface curvature from positions + index adjacency, no field. Three things stop it:

| | |
|---|---|
| Env-gated | `AZRA_MESH_CURV=1` ([bake_poll.rs:416,458](../../../core_engine/app/src/node_graph_bake/bake_poll.rs#L416)) — MESH_CURVATURE_PLAN M4 never landed, so it is off even for SDF |
| SDF-only resolvers | `sdf_body_verts_buffer` / `sdf_body_idx_buffer` / `sdf_body_idx_span` ([bake_poll.rs:819-861](../../../core_engine/app/src/node_graph_bake/bake_poll.rs#L819)) all route through `sdf_mesh_presentation` and return `None` for anything outside `sdf_body_mesh` / `sdf_mesh_pool` |
| No STORAGE usage | poly buffers are created `VERTEX \| COPY_DST` and `INDEX \| COPY_DST` ([mesh_gpu.rs:107,111](../../../core_engine/render/src/mesh_gpu.rs#L107)) — a compute pass physically cannot bind them |

### B4 — THE DECIDER: poly raster vertices are unwelded, so index adjacency is vacuous

`MESH_CURVATURE_PLAN.md` §Risks records that the kernel is safe on DC meshes because *"adjacent
faces genuinely share vertices … No seam duplication"*. **That property does not hold for poly.**
From the emitter's own test:

> *"The n-gon emitter always writes one vertex PER FACE CORNER, so sharpness changes the normal
> VECTORS, never the count"* — [mesh_sync.rs:2830](../../../core_engine/app/src/poly/mesh_sync.rs#L2830)

So in the poly GPU buffer every vertex has **exactly one** incident triangle. The kernel's `seed`
pass would find zero normal disagreement at every vertex and `measure` would return κ = 0
everywhere — a uniformly black mask that no amount of Radius or Remap would move. Porting B3 as-is
is not a shortcut to poly curvature; it is a guaranteed silent zero.

This is the fact that picks the design in §3.

---

## 2. What already works (scope honesty)

Do **not** rebuild these. They are proven on poly bodies today:

| Piece | Evidence |
|---|---|
| Materials + the map bake chain on poly | the owner's cube renders a red Fill layer through the same `bake_node_graph_maps_res` path |
| Image / Constant / Noise layers on poly | ungated in the same menu, same lowering, same compositor |
| Box-triplanar and UV sampling of a baked map | `sample_mat_triplanar` at body-local position, `gbuffer.wgsl` |
| The `CurvGrid` shape, reader, shaping (remap / invert / sharpness / blend / mask role) | `sdf_curvature_masks` returns `(mask, curv, convex, concave)`; `curv_caches` lookup is keyed by `CurvKey` |
| Per-vertex → planar splat, bilinear, normal-weighted | `build_vertex_splat_curv_grid` + its `splat_bilinear` ([graph_eval/mod.rs:595,625](../../../core_engine/app/src/node_graph_bake/graph_eval/mod.rs#L595)) |
| Authored UV presence per body | `cache.poly_custom_data[nid].uv` — the honest signal already threaded into `GenCtx::has_uvs` |
| The one display-mesh choke point | `poly_cached_display_mesh(nid)` → `raster_data_ngon_with_normals` ([mesh_sync.rs:745-748](../../../core_engine/app/src/poly/mesh_sync.rs#L745)) |

**The only missing thing is the MEASUREMENT source.** Everything downstream of "produce a
`CurvGrid` for this body" is built, tested and shared with the SDF path.

---

## 3. Design

### 3.1 Measure on the half-edge mesh, on the CPU

Poly adjacency is exact, already in memory, and already the source of truth: `HalfEdgeMesh`. It
needs no welding pass, no spatial hash, no GPU buffer usage change, and no new residency plumbing.
Discrete mean curvature over a half-edge mesh is the standard formulation
`MESH_CURVATURE_PLAN.md` §Algorithm already specifies (per-triangle geometric normals from
*positions*, area-weighted accumulation per vertex, normalise, then N smoothing iterations over
the 1-ring as the Radius control).

**Measure the DISPLAY mesh, never the base mesh.** `poly_cached_display_mesh(nid)` is
post-modifier; the base mesh is not what the user sees. `poly_export_raster`'s doc comment records
that exporters reading the base mesh silently dropped *"THE ENTIRE MODIFIER STACK"*
([mesh_sync.rs:734-737](../../../core_engine/app/src/poly/mesh_sync.rs#L734)) — the same trap,
already paid for once.

**Corner alignment comes free.** Compute κ per `HalfEdgeMesh` vertex, then scatter to raster
corners inside `raster_data_ngon_with_normals`' own corner loop. Same function, same order, same
pass — the per-corner κ array cannot drift out of step with the vertex array, because the same
loop writes both.

#### Rejected alternatives

| Option | Why not |
|---|---|
| Port the GPU kernel (B3) to poly buffers | B4: unwelded corners make it return 0. Would need a weld pass, a welded↔unwelded index map, and a `STORAGE` usage change on the shared mesh cache — strictly more work for a worse-conditioned result |
| Voxelise the poly mesh into an SDF and reuse the field path | Reintroduces exactly the seam/blend artefacts `MESH_CURVATURE_PLAN.md` was written to remove, plus a bake. Measuring a mesh through a field it was converted into is the long way round |
| Vertex-rung display carry (`INSTANCE_FLAG_VERTEX_CURVATURE`) | Dead end by design: the live G-buffer composite was **deliberately removed** ([gbuffer.wgsl:862](../../../core_engine/render/src/gbuffer.wgsl#L862)) so the bake stays the single compositor (ADR-001 Decision 1). Curvature must reach the image through the map, or PT loses it |

### 3.2 Store in the existing bake domain

The measurement domain is the mesh; the **storage** domain must remain the `res×res` grid the
compositor and PT already read, or Decision 1 is violated. Two writers, chosen per body:

* **Body has authored UVs** → rasterise the display mesh's triangles into the map with UV as
  position and κ interpolated across each triangle. Exact, no projection blind spot, and it is the
  standard "bake to UV" every DCC does. This is strictly better than anything the SDF path can do,
  because a poly body can actually *have* UVs.
* **Body has no UVs** (the owner's cube — Box triplanar) → normal-weighted box splat of per-vertex
  κ into the same planar domain, **reusing `splat_bilinear` and the accumulate/normalise structure
  already written** in `build_vertex_splat_curv_grid`. Feed it mesh samples instead of
  field-projected seeds; delete nothing.

Note the asymmetry vs `CURVATURE_BOTH_MODES.md` Phase 3: that document had to invent
volume-stratified seeds because the SDF path has no vertices to start from. Poly does. The
coverage problem that took three phases to solve there does not exist here — every sample is a
real surface point by construction.

### 3.3 What the rung means for poly

`GeneratorRung::Auto` already resolves to `VertexCache` when `!has_uvs`
([layer_ui.rs:724-739](../../../core_engine/app/src/material_panel/layer_ui.rs#L724)). Keep that
rule verbatim; for poly it now selects the *writer* in §3.2 rather than a different measurement.
The measurement is per-vertex in both cases — that is the honest reading of "the rung is a
sampling domain choice", and it makes the control non-lying for the first time on poly.

---

## 4. Phases

Each phase builds green (`cargo build -p app`) and has a gate that fails on the *specific* defect
it exists to prevent. Gates assert coverage and value, never mere difference —
`CURVATURE_BOTH_MODES.md` §"The gate that let this ship" is the cautionary precedent: a
difference-only gate passed on a 77 %-empty grid.

### P0 — Make "no field" actually fail closed *(prerequisite; ships value alone)*

Fixes B2 whether or not the rest lands.

* Add an explicit body-kind discriminator to `SdfFieldSources` (e.g. `field_owner: Option<NodeId>`
  plus `body_is_sdf: bool`) set by `snapshot_sdf_field_for_material`. `has_field()` must mean
  "*this* body has a field", not "some prim exists somewhere".
* `sdf_at`'s `sn_field_h` analytic fallback must not be reachable for a body with no prims of its
  own. Return a defined no-field sentinel and let each caller fail closed the way
  `eval_nodes.rs:586` already documents for Direction.
* `body_aabb` must not fall through to `by_uid.values()` when the caller asked for the active
  body's bounds.

**Gate:** a poly body in a scene that also contains an SDF body produces a curvature grid that is
identically 0 — and the *same* poly body in a poly-only scene produces the same grid. Today those
two scenes disagree, which is the bug.

### P1 — κ on `HalfEdgeMesh` (core_math, pure, no GPU)

* New shared function in `core_math` (search first per CLAUDE.md § *No Duplication* — nothing in
  `core_math/src/` computes mesh curvature today; `bevel_op/corners.rs` and `nurbs.rs` mention
  curvature but neither is a discrete-mesh estimator).
* Per-triangle geometric normals from positions; area-weighted per-vertex accumulation; normalise;
  `smooth_iters` 1-ring diffusion driven by the panel Radius.
* Signed: convex positive, concave negative, matching the three `GeneratorOutput` channels
  (`Signed` / `ConvexOnly` / `ConcaveOnly`) the reader already expects.

**Gate (`cargo test -p core_math`):** on a unit cube, flat faces read ≈ 0 and all 12 edges read
high, with a real spread (not a two-value step); on a subdivided sphere, κ is near-uniform positive
and its variance is below a fixed bound. Both are the non-vacuity checks
`MESH_CURVATURE_PLAN.md` M2 specifies.

### P2 — Per-corner scatter through the n-gon emitter

* Extend `raster_data_ngon_with_normals` (or add a sibling that shares its corner loop) to emit an
  optional per-corner κ array alongside `(verts, indices)`.
* Extending the existing function with a parameter is the CLAUDE.md-preferred move over a
  near-duplicate emitter; a second loop over the same faces is exactly the drift risk the export
  bug already demonstrated.

**Gate:** `κ.len() == verts.len()` for a cube, an n-gon-heavy mesh, and a mesh with a Mark Sharp
edge (normal splits must not change the count relationship — the existing
`export_raster_matches_the_displayed_normals` test already pins that invariant).

### P3 — `CurvGrid` writer: UV raster

* Rasterise display-mesh triangles into `res×res` in UV space, interpolating κ; conservative
  coverage at the edges so seams do not leave holes.
* Output shape must be bit-compatible with `CurvGrid` (`res`, `data` as 4-wide
  `[mask, curv, convex, concave]`, `region`, `key`) so `sdf_curvature_masks`' existing shaping and
  the whole compositor are untouched.

**Gate:** on a UV-unwrapped cube, `coverage(uv_grid) >= 0.8 * coverage(planar_sdf_grid)` for the
equivalent SDF body — the coverage form `CURVATURE_BOTH_MODES.md` mandates — **and** the 12 edges
land in the map at the UV positions the unwrap puts them.

### P4 — `CurvGrid` writer: triplanar splat (no-UV bodies)

* Normal-weighted box splat of per-vertex κ into the planar domain, reusing `splat_bilinear`.
* **Verify, do not assume**, that the planar bake domain a poly body's map is *sampled* from
  matches the domain this writer fills. The anchor is empirical and cheap: a Noise layer already
  bakes and samples correctly on the owner's cube, so match whatever domain that layer lands in.

**Gate:** the owner's Box-triplanar cube shows curvature banding on all 12 edges with no face
reading 0, and coverage ≥ 0.8× the UV path on the same mesh UV-unwrapped.

### P5 — Wire the poll + widen the UI gate

* Poll builds the poly grid when the active body is polygonal, keyed on the display mesh's content
  key (`poly_mesh_gpu_key` is the upload-stamped key; per the
  `poly_display_cache_not_gpu_truth` precedent, early-outs must **not** key on display-cache keys).
* Cap the work: one poly curvature build per frame (`MAX_*_PER_FRAME`, FEATURE_RULES invariant 3),
  registered with `FrameScheduler`. It runs on mesh change, never per frame.
* Gate becomes `gen.has_sdf || gen.has_poly_mesh` **for Curvature and Cavity only**. AO, Thickness,
  Direction and Position keep the SDF-only gate and keep their hover text — see §6.

**Gate:** the owner's repro — add a poly cube, add a Curvature mask, see edge-weighted shading;
undo restores; save → load → the layer still bakes; and a mixed poly+SDF scene keeps both bodies
correct (the P0 gate, re-run at the UI level).

---

## 5. Risks

| Risk | Standing | Mitigation |
|---|---|---|
| **Unwelded corners** | **Resolved by design.** κ is computed on `HalfEdgeMesh` (welded by definition) and scattered to corners; the raster buffer's duplication never enters the measurement | — |
| Modifier stack ignored | Real; already burned this project once | Measure `poly_cached_display_mesh`, never the base mesh (§3.1) |
| CPU cost on dense meshes | Bounded. Per `project_poly_live_feel_bake_dominates`, the 40k-quad class is milliseconds when per-corner `Vec` allocation is avoided | Preallocate; rayon the accumulation; P5's per-frame cap; runs on mesh change only |
| UV seams leave holes in the P3 map | Real for any UV bake | Conservative rasterisation + the coverage gate, which fails on holes rather than passing on difference |
| Radius semantics differ from the SDF path | Real and acceptable | Radius = smoothing iterations (`MESH_CURVATURE_PLAN.md` M3). It is a genuinely different measurement, so the CPU oracle comparison must be **re-scoped**, not "fixed" — same call M4 already made |
| Scope creep into the dormant GPU kernel | Real | Out of scope. `AZRA_MESH_CURV` and MESH_CURVATURE_PLAN M4 stay untouched; this plan neither depends on nor blocks them |

---

## 6. Explicitly out of scope

* **Ambient Occlusion and Thickness on poly.** Both need ray casting against the mesh, not a local
  differential quantity. The path tracer's TLAS/BLAS over raster tris is the plausible source, and
  that is a separate plan. They keep the SDF-only gate and hover text.
* **Direction on poly.** Cheap to add later (`dot(n, axis)` from mesh normals) but it is a
  different generator with its own axis UI; one feature per prompt.
* **Enabling `AZRA_MESH_CURV` / finishing MESH_CURVATURE_PLAN M4 for SDF bodies.** Adjacent, not
  required.
* **Any change to `HalfEdgeMesh`'s serialized shape.** Red line, never lifted. Per-corner κ is
  runtime-only, on the `PolyMesh` wrapper / app layer, never a serialized field.
* **Save format.** Nothing new is persisted. `GeneratorRung` and the panel doc already round-trip.

---

## 7. FEATURE_RULES pre-flight

1. **New scene object type?** No. No `NodePayload` variant, no save-format change.
2. **Mutates scene state?** Only through the existing panel edit path, which is already wrapped —
   adding a layer is a panel doc mutation with undo today. No new mutation surface.
3. **Frame-thread work?** Yes — P5 adds a build step. Needs a `MAX_*_PER_FRAME` cap and a
   `FrameScheduler` `WorkClass` (invariant 3).
4. **New render path or mode?** No. Same compositor, same bake, same raster pass, same shaders. The
   Display Invariant is untouched — no shader edit is required by this plan at all.

---

## 8. Regression checklist (run at P5)

```bash
cd core_engine && cargo build -p app
cd core_engine && cargo test -p core_math      # P1 + P2 gates
cd core_engine && cargo test -p graphics       # unchanged; proves no shader drift
```

Manual, in order:

1. Poly cube → Curvature mask → edges shade, faces do not.
2. Radius up → falloff **widens** (not just saturates differently) — the M3 gate.
3. Convex / Concave / Signed each change the result in the documented direction.
4. Add a Bevel modifier → curvature follows the bevelled edges (display mesh, not base).
5. Undo / redo the layer add. Save → reload → layer still bakes.
6. Mixed scene (poly + SDF body): both read their own geometry; switching active body switches the
   map.
7. SDF body with a Curvature layer is **pixel-unchanged** from before this work.

---

## 9. Session log

* **2026-08-09** — Plan authored. B1–B4 verified in tree at `bb70b97`. B4 (unwelded per-corner
  raster vertices ⇒ the GPU mesh kernel returns κ = 0 on poly) is the finding that rejects the
  obvious "port the existing kernel" approach and selects the half-edge CPU measurement. B2a/B2b
  (analytic-field fallback and cross-body bounds leak) are pre-existing defects masked by the UI
  gate; P0 fixes them independently of poly curvature. No code changed.

* **2026-08-10 — IMPLEMENTED, P0 through P5.** 15 new gates; `cargo build -p app` green,
  `cargo test -p graphics` 85/85, and both `core_math` and `app` show the *same* pre-existing
  failures as clean `HEAD` (810d945) and no others — verified by running the suites in a detached
  worktree at HEAD: core_math 1079→1092 passed with the identical 3 failures
  (`edge_select::directed_loop_crosses_pole_straight`, 2 × `mesh_ops`), app 728→743 passed with
  the identical 5 (`dc_baseline`, 4 × `project::tests`). All 8 pre-date this work and sit in
  areas untouched by it (they are downstream of the uncommitted `bevel_op`/`project` WIP in the
  tree).

  **What shipped**

  | Phase | Where |
  |---|---|
  | P0 fail-closed | `graph_eval/mod.rs` — `NO_FIELD_D` replaces the `sn_field_h` fallback; `has_field()` drops its `by_uid` clause; `body_aabb` drops its `by_uid` fall-through; `Curvature`/`Cavity` nodes lose their synthetic branches |
  | P1 kernel | `core_math/src/mesh_curvature.rs` (new) — `vertex_curvature`, `RingMesh`, `smooth_iters_for_radius` |
  | P3/P4 writers | `app/src/poly_curvature.rs` (new) — `PolyCurvSource::build_grid` + UV-raster and triplanar-raster writers |
  | P5 wiring | `bake_dispatch.rs` (snapshot attach), `bake_maps.rs` (per-key prep), `layer_ui.rs` / `channel_row.rs` / `view/mod.rs` (`GenCtx::has_poly_mesh`) |

  **Two deliberate departures from §3–§4, both because the plan's premise was wrong in the tree:**

  1. **P2 was not needed and was not done.** The plan routed per-corner κ through
     `raster_data_ngon_with_normals` on the assumption that the raster corner stream is the only
     place corner alignment exists. It is not: `CustomData.uv` is `LayerStore<HalfEdgeId, Vec2>`,
     i.e. corner-domain **on the half-edge mesh itself**. κ is therefore paired to UV in kernel
     space using the association the emitter documents in its own comment — *"a corner is the
     half-edge whose DESTINATION vertex is that corner"* (`poly_mesh.rs:1586`) — and the hot
     raster emitter is untouched. Strictly less risk than the planned signature change, and the
     alignment is guaranteed by sharing the convention rather than by a parallel array that could
     drift.
  2. **P4 does NOT reuse `build_vertex_splat_curv_grid`'s projection, and does not splat.** Two
     separate corrections, both found by doing what P4 asked ("verify, do not assume") against
     `gbuffer.wgsl`:
     * That function maps `u = (p[axis] − bmin) / ext` (AABB-normalised 0..1), but
       `sample_mat_triplanar_b` samples raw *body-local × tiling* with repeat addressing. Reusing
       it would have looked like reuse and landed the map offset from where the surface reads it.
       The writer now mirrors the shader's own coordinates, weights (`|n|^blend`, normalised) and
       per-plane rotations. Gated by `p4_triplanar_respects_tiling`.
     * Point-splatting per vertex measured **1.5 % written coverage** on an 8×8×6 cube at 64² —
       the same structural under-coverage `CURVATURE_BOTH_MODES.md` spent three phases fighting.
       A poly body has triangles, so the writer rasterises triangle interiors into each plane
       instead; coverage exceeds 80 %. The coverage problem that document solved simply does not
       have to exist here.

  **A measurement trap worth recording.** The first P3/P4 gates measured coverage as "texels that
  are non-zero" and failed at 1.5 % / 61 %. That metric is wrong for this quantity: a curvature map
  is *legitimately* 0 across every flat face, so it conflates "unwritten" with "flat". Gates now
  assert on **written** coverage (`build_grid_probe`). Related: a plain 6-quad cube has only corner
  vertices and no flat interior at all, so it saturates to 1 everywhere — correctly. Every fixture
  here is subdivided for that reason, and a test that reads "uniform" on a coarse cube is testing
  the fixture, not the kernel.

  **Still out of scope, unchanged from §6:** AO, Thickness, Direction and Position on poly (each
  needs ray casting or a field; they keep the SDF-only gate and now say so in their hover text),
  `AZRA_MESH_CURV` / MESH_CURVATURE_PLAN M4, and any save-format change (nothing new is
  persisted). `HeightToNormal` still calls `sn_field_h` unconditionally — it is a synthetic-height
  demo node, not a gated generator, and was left alone.

  **Not yet verified in the running app.** Everything above is gated by tests; the §8 manual
  checklist has not been run. Items 1–4 and 6 are the ones to try first.

* **2026-08-10 (later) — owner rejected the first cut: *"I SEE NOISE APPEAR BUT THIS IS FAR FROM
  CURVATURE MAP. EVEN IN UV MODE THIS DONT WORK"*,** with a Blender reference (`LP_stone`) showing
  fine crack detail on a low-poly asset. Three separate causes, found by RENDERING the output to
  PNG (`dump_curvature_pngs`, `#[ignore]`d, `AZRA_CURV_DUMP=<dir>`) instead of trusting the gates —
  every numeric gate was green through all three.

  1. **Aliasing in the rasteriser — this was the "noise", and it was mine.** The inside-test
     tolerance was `1e-3 · max(|area|,1) / |area|`, which for any sub-texel triangle is
     `1e-3/|area|`: unbounded. With the ±1 texel scan box, every sub-texel triangle flooded a 3×3
     block with its own κ, and on a dense mesh those blocks overlap and average into mottling.
     Replaced with 4×4 supersampled, coverage-weighted rasterisation, a fixed `1e-6` barycentric
     epsilon, and a nearest-texel fallback (area-weighted, so a sub-texel triangle contributes
     proportionally instead of flooding).
  2. **Saturation — the map was a binary mask, not a gradient.** `POLY_DISPLAY_SCALE` was `1.0`,
     but the shared shaping is `smoothstep(clamp(curv·contrast·4))` over `curv = x/(1+|x|)`, so it
     clips at **κ = 0.30**: a 90° edge and a 60° edge both baked pure white, and below κ = 0.05
     everything was black. Now `1/√3` — the κ of a right-angle box corner, i.e. that corner is the
     white point. The same κ sweep now spans 0.12 → 0.37 → 0.63 → 0.84 → 1.0.
  3. **The reference is not reachable from mesh geometry at all** (owner-confirmed direction:
     *"Both — build the Substance behaviour"*). Crack detail on a low-poly stone lives in the
     **normal map**, not the mesh; Substance Painter's per-pixel curvature algorithm differentiates
     the baked normal map, which is how it gets cracks on a flat LP face. Added that as a second
     source: `∂n.x/∂u + ∂n.y/∂v`, which for `n ∝ (−h_u, −h_v, 1)` equals `−(h_uu + h_vv)` and is
     therefore already positive-on-convex — it sums onto the geometric term with no sign
     correction. Honours the `green_dx` convention (DirectX green inverts only the v-derivative,
     which would otherwise read as plausible directional banding). Needs no UV transform: the
     normal map and the curvature grid are the same texture domain, so a texel-space derivative
     lands on the right surface points for BOTH writers.

  **The writer now accumulates RAW κ and shapes once at the end**, because the shaping is
  non-linear — `shape(a) + shape(b) ≠ shape(a+b)`, so two half-strength features would each shape
  to near-black and their sum would stay near-black, when together they are one full-strength
  feature. `Writer` is scalar accordingly; `into_shaped` produces the 4-wide layout at the end.

  **Two fixture traps that cost time and would cost it again:**
  * A per-vertex hash displacement (`sin(dot(p,k)·43758)`) makes adjacent vertices uncorrelated,
    i.e. a mesh that is genuinely jagged at vertex scale. Its curvature map is *correctly* noise,
    so it looks like the bug whatever the writer does — it can neither confirm nor refute a
    rasteriser artefact. Use smooth, correlated displacement at a wavelength of a few quads.
  * Displacement at too LOW a frequency (half a period across the whole body) is locally flat and
    bakes black — also correct, also uninformative.

  17 gates now (2 new: normal-map-on-flat-geometry, green-convention). Same 8 pre-existing
  failures as clean HEAD and no others (core_math 1097 pass / 3 fail, app 746 pass / 5 fail).
  Still not run in the app — §8 remains outstanding.
