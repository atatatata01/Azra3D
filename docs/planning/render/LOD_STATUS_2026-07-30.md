# Auto-LOD status — do we have Nanite-style object-level LOD?

**Date:** 2026-07-30
**Owner:** *"do we have like auto LOD, like Nanite for large scene performance, on the object level?"*

**Short answer: yes for SDF bodies, no for poly bodies, and it is not Nanite-class — it is
per-object discrete LOD, which is the right target anyway.**

---

## 1. What already exists, and it is more than you might expect

### Screen-space LOD chooser with hysteresis — `sdf_gpu_mesh/lod_policy.rs`

```rust
pub(crate) fn choose_lod_level(projected_voxel_px: f32, current: u32, max_level: u32) -> u32
```

This is a real implementation, not a stub:

* **Signal is projected voxel size in pixels** — the correct metric (screen-space error), not raw
  camera distance, so a big object far away and a small object near behave sensibly.
* **Deadband hysteresis** on both directions — refine needs the boundary *overshot*, coarsen needs it
  *undershot*, so a body sitting exactly on a boundary does not flip-flop.
* **Rate limit** of 1 switch/second/body (`LOD_MIN_SWITCH_INTERVAL`) on top of the deadband.
* **Resolution-aware ceiling** — `lod_max_level_for_res` refuses to coarsen below 32 cells, because
  "the coarse mesh collapses to mush" past that.
* **Tested against oscillation**: `lod_chooser_never_oscillates_for_a_static_signal`.

Three tiers (L0/L1/L2). Driven from `poll_sdf_lod_policy` (`app_poll_gpu.rs:750`).

### A second, separate LOD ladder for mesh quality — `raster_mesh_prep.rs:218`

`upgrade_mesh_lod` walks `Preview → Medium → High`, upgrading only when the scene is idle and no
jobs are pending. This is progressive refinement (start coarse, sharpen when the user stops moving)
rather than distance LOD.

### Frustum culling — `raster_gbuffer.rs:537`

`self.render.frustum_cull` with `last_frustum_culled` reporting. Object-level, already on.

### Indirect drawing — `draw_indexed_indirect` for pooled voxel chunks

GPU-driven draw submission already exists for the voxel path, including per-entry `instance_count`
zeroing to disable pool entries without a CPU round trip.

---

## 2. The gap: poly bodies get NONE of it

Every LOD path above is gated to SDF:

* `upgrade_mesh_lod` iterates `self.scene.scene_graph.sdf_bodies()` — poly bodies are not in the loop.
* `poll_sdf_lod_policy` is SDF by name and by scope.
* Decimation exists and is excellent (`DecimateParams`, 14 params, 5 modes including a
  `Master` importance-map mode) — but **nothing drives it from distance or screen size**. Grep for
  decimate + lod/distance/screen/projected returns one hit, and it is the SDF bake's internal
  `detail_lo_keep_ratio`, not a viewport LOD.

**So: a scene of 200 poly objects draws every triangle of every object at full resolution, every
frame, regardless of whether an object is 4 pixels tall.** That is the large-scene problem the
question is really about.

---

## 3. Is Nanite the right goal? No — and this matters

Nanite is *virtualised micropolygon geometry*: meshes are pre-decomposed into a BVH of ~128-triangle
clusters, and the renderer selects a cut through that hierarchy per-frame per-cluster, then rasterises
sub-pixel triangles in a compute shader rather than the hardware rasteriser.

Three reasons that is the wrong next step here:

1. **It is a rendering *architecture*, not a feature.** It replaces the hardware raster path for
   opaque geometry. AZRA's Display Invariant says one technique per view and forbids fusing
   techniques over the same pixels — Nanite is exactly a second technique.
2. **It solves a problem AZRA does not have yet.** Nanite pays off at film-asset density (millions of
   triangles per object). AZRA's poly bodies are hand-modelled and its SDF bodies bake to DC meshes at
   ≤512³. The bottleneck measured this session was **`poll_background_tasks` at 19–27 ms on an IDLE
   frame with one cube** — not triangle throughput.
3. **The cheap 90 % is untaken.** Per-object discrete LOD + culling gets most of the large-scene win
   for a fraction of the work, and the machinery is already written for SDF.

---

## 4. What to actually build, in order

### A. Extend the existing LOD policy to poly bodies ★★★

`choose_lod_level` is generic over "projected size in pixels" — it does not care what produced the
mesh. What is missing is (a) including poly bodies in the walk, and (b) a source of coarse meshes,
which **already exists**: `DecimateParams` with `DecimateMode::Master`.

Pre-decimate each poly body once at, say, 50 % and 15 % on first use, cache the two coarse meshes
beside the full one in `MeshCache`, and let the existing chooser pick between three
`MeshCacheId`s. This is wiring, not new algorithms.

*Gate:* switching LOD must not change silhouette beyond a threshold at the switch distance, and the
chooser must not oscillate (reuse `lod_chooser_never_oscillates_for_a_static_signal`).

### B. Occlusion culling ★★

Frustum culling is in; occlusion is not. For interior scenes (the case where object counts explode)
this typically beats LOD. A GPU depth-pyramid test against last frame's depth is the standard cheap
version, and the depth buffer already exists.

### C. Instanced draw for duplicates ★★

`Shift+D` duplicates are separate bodies with separate draw calls. If they share a `MeshCacheId`
they can share one instanced draw. `draw_indexed_indirect` + `instance_count` plumbing already exists
on the voxel path — this extends it to poly/SDF.

### D. Only then consider cluster LOD ★

If A–C land and triangle count is *still* the measured bottleneck, the Nanite-shaped idea worth
stealing is **per-cluster LOD without the software rasteriser**: split large meshes into clusters,
choose a cut per frame, draw with the hardware rasteriser. That captures most of the benefit while
staying inside one render technique.

---

## 5. The honest framing

The question assumes triangle throughput is the large-scene bottleneck. **On this codebase, today,
measurement says otherwise** — the frame budget is being spent on redundant CPU work
(`poly_sync` firing ~7× per mesh change, `poll_background_tasks` at 2× budget on an idle frame with
one object) long before triangles matter.

Fix that first. It costs less than any LOD work and it is measurable now. Then item A, which reuses
machinery that is already written and tested.
