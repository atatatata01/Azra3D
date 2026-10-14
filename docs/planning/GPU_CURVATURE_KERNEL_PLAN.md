# GPU Curvature Kernel — ONE Analytical Evaluator, TWO Consumers

**Status:** P1 SHIPPED (a3ad48e). P2 (vertex carry/stamp) + P3 (panel un-gate) SHIPPED 2026-07-27
— see the addendum at the end of this file for what was actually built, the one honest remaining
gap (UV-space poly bake), and the semantic test that proves the fix.
**Branch:** intended for `feat/ui-polish` (release trunk). Investigated at `da25395`;
line numbers re-verified against `9834553` (`fix/blend-k-profiles`, whose parent is `da25395`).
**Author:** investigation 2026-07-27

> **Base note.** During this investigation the `ui-polish` worktree was checked out from
> `feat/ui-polish` onto a new branch `fix/blend-k-profiles` by a concurrent session, which
> committed `9834553` (blend profile tri-mode). That commit touches `volume_bake.wgsl` and
> `sdf_prim_eval.wgsl`, but **only the blend-profile functions** (`smin_profiled_curve` et al.).
> Every citation in this plan was re-verified at `9834553`: `eval_scene_at` is still at
> `volume_bake.wgsl:1293`, the binding table at `:11-32`, and `calc_curvature` at
> `sdf_prim_eval.wgsl:387` — all unchanged. Only the `@compute` entry-point lines shifted by ~3
> (`bake_volume` `:1168`→`:1171`, `classify_bricks` `:1536`→`:1539`); both spellings appear below.
> **This file is untracked and must be committed onto `feat/ui-polish`, not onto
> `fix/blend-k-profiles`.**
**Supersedes:** the "Vertex rung is unbuildable" finding recorded in
`core_engine/app/src/material_panel/model.rs:336-343`

---

## 0. Why this exists

The material panel's generator **Vertex rung** (`GeneratorRung::VertexCache`,
`material_panel/model.rs:348`) is declared but permanently downgraded to `Auto`.
The doc comment on it states the blocker exactly:

> *"the production DC display mesh is GPU-resident (`SdfBodyMeshSlot.verts`, written by
> `sdf_mesh.wgsl`, drawn via `draw_indexed_indirect` — vertices never reach the CPU), so
> stamping CPU-evaluated `sdf_curvature_masks` values per vertex has no route that keeps the
> SAME evaluation code path for both rungs (C3: divergence via a second WGSL port is a P0 bug
> by definition)."*
> — `material_panel/model.rs:337-343`

Two substitutes were considered and **rejected**:

| Rejected | Why |
|---|---|
| Grid-Laplacian off the baked voxel grid | Trilinear 2nd derivatives are zero-in-cell / singular-at-faces; sparse bricks return ±100 sentinels. Garbage signal. |
| A second WGSL port of the curvature math for the vertex rung only | Makes Vertex and Texture rungs compute **different quantities under one label**. Spec C3 P0 / CLAUDE.md *No Hacks*. |

**The approved direction (owner):** build **ONE GPU compute kernel** that evaluates curvature
against the **analytical** field, and switch **both** consumers onto it. One formula, two
consumers, **parity structural rather than promised**.

> The `sdf_mesh.wgsl` finding in the prior investigation is **confirmed**: it is a standalone
> module bound to the baked voxel grid with zero references to `scene_sdf`/`eval_prim`. It is
> *not* the host. But the conclusion drawn from it — "therefore no GPU route exists" — was
> **wrong**: `volume_bake.wgsl` is a different module that already does exactly this (§1).

---

## 1. Where the analytical field is ALREADY GPU-reachable

Every shader that reaches the analytical prim data, and its fitness as a host:

| Shader | Reaches analytical field? | Fitness as host |
|---|---|---|
| `deferred_entry.wgsl` | Yes, via `sdf_shared_all()` → `scene_sdf` (`sdf_scene.wgsl:379`) | **No** — screen-space fragment pass. Needs the full G-buffer, camera, lights, atlas, shadow BGL. Cannot be dispatched over N arbitrary points. |
| `fog_entry.wgsl`, `prim_outline_entry.wgsl` | Yes (same concat) | **No** — same screen-space constraint. |
| `pathtrace_entry.wgsl` | Analytical PT is **DELETED**; PT is triangle-based (`pathtrace_tri_entry.wgsl`) | **No** |
| `brick_fill.wgsl` | Partially — hosts `extract_bake_sdf_lib()` (`shader_build.rs:351-376`), which gives `eval_prim` but **stops before** `eval_scene_at`, and excludes bindings + `BakeParams` | **No** — you'd get per-prim eval without the scope/boolean stack machine. |
| **`volume_bake.wgsl`** | **Yes — self-contained, with a purpose-built entry point** | **★ THE HOST** |

### 1.1 Why `volume_bake.wgsl` is the host

It already contains the exact function this kernel needs, with the exact signature:

```wgsl
// volume_bake.wgsl:1293
fn eval_scene_at(world_pos: vec3<f32>) -> f32
```

And its header comment states the design contract that this plan extends
(`volume_bake.wgsl:1287-1291`):

> *"Two call sites only: `bake_volume` (1 eval/voxel) and `classify_bricks` (9 evals/brick…).
> **Any eval change here changes BOTH consumers — that is the point** (the classifier must agree
> with the field the bake writes)."*

The file is **fully self-contained** — it carries its own verbatim copy of the SDF library
(`eval_prim` at `:643`, accessors `:85-156`), stated at `volume_bake.wgsl:1-6`. It does **not**
concat `sdf_shared`/`sdf_prim_eval`. Build path is a single text transform, no concatenation:

```rust
// gpu_init.rs:1783-1788
source: wgpu::ShaderSource::Wgsl(
    graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()
),
```

**Adding a third consumer of `eval_scene_at` is the pattern the file was built for.**
`classify_bricks` (`:1536` @ da25395 / `:1539` @ 9834553) is the closer template of the two
existing ones — it evaluates at positions it computes itself (`:1592-1606`), ignoring
grid/atlas/pad geometry.

### 1.2 Binding table (`volume_bake.wgsl:11-32`, all `@group(0)`)

| # | Type | Role | Curvature kernel |
|---|---|---|---|
| 0 | `storage, read` `array<f32>` `prim_hot` | **Analytical** prim stream | **keep** |
| 1 | `uniform` `BakeParams` | Analytical scoping + grid fields | **keep** (fill 3 fields) |
| 2 | `storage, read_write` `array<f32>` `output_grid` | Output arena | **repurpose → curvature out** |
| 3 | `texture_storage_3d<r32float, write>` | Output texture | bind 1³ dummy (`gpu_jobs.rs:268-272`) |
| 4 | `storage, read` `clip_curves` | **Analytical** | **keep** |
| 5 | `storage, read` `mesh_grid` | **Analytical** | **keep** |
| 6 | `storage, read` `brick_index` | Grid-specific | filler (precedent `app_poll_bake.rs:1383`) |

BGL is single-source: `graphics/src/bgl.rs:302-312` `bake_bgl_entries()`.
**Positions input needs one new binding (7).** That touches the single-source BGL shared by all
five volume_bake pipelines — additive only, no reordering.

### 1.3 Per-body scoping — no re-binding problem

There is **no** `prim_start` in `BakeParams`. A **fresh per-body, per-dispatch buffer** is
allocated and uploaded (`app_poll_bake.rs:157-163`), so the buffer always holds exactly one
body's prims starting at index 0. Only three fields scope the eval:

- `prim_count` (`volume_bake.wgsl:43`), `floats_per_prim` (`:44`), `body_local_to_world` (`:79`)

Booleans/mirror/sub-bodies ride **inside** the prim stream as a scope-stack machine
(`SCOPE_OPEN` = 254 at `:1314`, `SCOPE_CLOSE` = 255 at `:1383`, depth 8). Model the params fill
on `classify_bake_params` (`app_poll_bake.rs:1272-1297`) — already the minimal non-grid helper.

> ⚠ **Frame semantics:** `eval_scene_at` takes points in the **top body's LOCAL frame** despite
> the parameter name `world_pos` (`volume_bake.wgsl:76-79`, `gpu_jobs.rs:239-242`).
> The CPU `sdf_at` takes **world** (`node_graph_bake.rs:175-194`). Positions must be converted
> at the seam. **This is the single highest-risk parity detail in the whole plan.**

---

## 2. The kernel

### 2.1 The CPU algorithm being mirrored (`node_graph_bake.rs:324-408`)

Verified line by line. Note **three distinct epsilons** — do not collapse them.

```
e      = max(eps, diag*0.01, 1e-4)            :339   ← curvature FD step
accept = max(band, diag*0.08, 0.05)           :340   ← Newton acceptance
diag   = length(bmax-bmin) of the PADDED AABB :337   (pad = max_element*0.05, floor 0.02, :232)

SEEDS  :345-363
  if is_planar_bake_domain(p)  (|x|<=1.05 && |y|<=1.05 && |z|<=0.08, :312):
      u,v from p.xy; 6 seeds = 3 mid-plane (axis 2,1,0) + 3 offset by diag*0.55 beyond bmax
  else: 1 seed at p, axis 0, weight 1.0

PER SEED  :367-384
  ps = project_to_surface(seed, grow, accept)          :251-277
       10 iters; d = sdf(p)-grow; exit |d|<1e-4;
       g = sdf_grad(p, 0.002)  ← FIXED step, NOT e
       p -= (g/|g|) * d                                 (unit-step Newton)
       accept if |d| < max(accept, 1e-3) else SKIP seed (continue — contributes to neither sum)
  n  = normalize(sdf_grad(ps, e*0.5))                  :371  ← e*0.5, a THIRD step
  w  = planar ? max(n[axis]^2, 0.05) : 1.0             :374-379
  k  = calc_curvature(ps, e)                           :381

CURVATURE  :282-293  (7 taps, no /6)
  lap = Σ(6 neighbors ±e) - 6*d ;  return lap/(e*e)

REDUCE  :386-407
  if sum_w < 1e-6 -> (0,0,0,0)
  k_raw = sum_k/sum_w
  curv  = clamp(k_raw * diag * 0.15, -1, 1)
  shape(x) = clamp(smoothstep01(clamp(x*contrast*4,0,1)) * strength, 0, 1)
  mask = region==1 ? shape(convex) : region==2 ? shape(concave) : shape(|curv|)
  return (mask, curv, convex_mask, concave_mask)
```

**The core formula already agrees across three ports** — CPU `node_graph_bake.rs:282-293`, WGSL
`sdf_prim_eval.wgsl:387-394`, CUDA `sdf_eval.cuh:301-308` are line-for-line identical
(7-tap Laplacian / e², same sign, no `/6`). **What does not exist on GPU is everything around
it**: `body_aabb`/`diag`, `project_to_surface`, triplanar seeds, and the `diag*0.15` + shaping.

### 2.2 Kernel signature

```wgsl
// volume_bake.wgsl — 7th entry point, modeled on classify_bricks (:1536)
struct CurvParams {          // rides in the existing BakeParams tail or a new UBO
    n:        u32,           // sample count
    region:   u32,           // 0 both | 1 convex | 2 concave
    seed_mode:u32,           // 0 = single-seed (vertex rung / explicit Position)
                             // 1 = 6-seed triplanar (texture rung planar domain)
    _pad:     u32,
    bmin:     vec3<f32>,     // CPU-computed body AABB (mirrors body_aabb, :199-234)
    bmax:     vec3<f32>,
    e:        f32,           // = max(eps, diag*0.01, 1e-4)      — CPU-computed
    accept:   f32,           // = max(band, diag*0.08, 0.05)     — CPU-computed
    diag:     f32,
    grow:     f32,
    contrast: f32,
    strength: f32,
}

@group(0) @binding(7) var<storage, read> curv_positions: array<f32>; // N*3 (or vertex buf, §4)

@compute @workgroup_size(64)
fn curvature_eval(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= cp.n) { return; }
    // 1 or 6 seeds -> project_to_surface -> weight -> calc_curvature -> weighted mean -> shape
    // output_grid[i*4 + 0..3] = (mask, curv, convex, concave)
}
```

`e`, `accept`, `diag`, `bmin/bmax` are **computed on the CPU exactly as today** and passed in.
This is deliberate: `body_aabb` (`node_graph_bake.rs:199-234`) iterates prims with
`bounding_aabb_half_extents()` and is cheap, once per dispatch — recomputing it per thread on
GPU would be both wasteful and a **new divergence surface**. Keeping it CPU-side makes those
constants *provably* identical between the two consumers.

### 2.3 Cost and honest simplification assessment

Worst case per sample (planar, 6 seeds):
`6 × (10 Newton × (1 + 6 grad) + 6 normal + 7 curvature) ≈ 498 evals`.
Each eval is a full `eval_scene_at` — a loop over `prim_count` with an 8-deep scope stack.

| Concern | Verdict |
|---|---|
| Divergent Newton loop (early exit at `|d|<1e-4`) | **Keep as-is.** Warp cost = worst lane, but 10 is the cap and typical is 2-3. No simplification. |
| Seed `continue` on failed projection | **Must replicate exactly** as a masked average (accumulate `w` only on success) — not an average over 6 slots. |
| 6-seed triplanar | **Keep for the texture rung.** For the **vertex rung** it does not apply: a vertex already *is* a surface point, so it is the `else` branch (1 seed, w=1.0, `is_planar_bake_domain` false). **This is not a simplification — it is the same code path the CPU takes for a non-planar `p`.** |
| Register pressure from 11 `array<_,8>` stacks | Real. Mitigate by workgroup_size 64 (not 256). Measure occupancy in P1. |

**No simplification is proposed.** If profiling later forces one, the rule stands: it applies to
**both** consumers or it is rejected.

### 2.4 Two honest gaps that must be closed in P1

1. **`prim_uid != 0` scoping.** CPU `sdf_at` (`:178-182`) bypasses `eval_scene` entirely for a
   named prim and calls `prim.world_distance(pv)` — **no booleans**. `eval_scene_at` has no such
   variant. **Mitigation:** the material panel always emits `uid = 0` (`model.rs:300`,
   `lower.rs:608/847`), so the panel path is unaffected. For non-zero uid the kernel must either
   receive a single-prim stream (trivial — the per-dispatch buffer already holds an arbitrary
   prim list) or fall back to CPU. **Do the single-prim-stream route**; it is exact.
2. **`scene_sdf` render_mode dispatch.** Not applicable here — `volume_bake.wgsl` has no
   `render_mode` switch (that is `sdf_scene.wgsl:379`, the render lineage). Another reason
   volume_bake is the correct host.

---

## 3. Consumer A — texture bake

### 3.1 Today

- Entry `bake_node_graph_maps_res` (`node_graph_bake.rs:543`), **single-threaded nested pixel
  loop** at `:603-666`, res ∈ {64,128,256,512} (default `BAKE_WORK_RES = 256`, `:24`),
  bilinear-upsampled to `MAP_RES` 1024 (`:448-451`).
- Runs on a **named worker thread** (`spawn_graph_bake_quality`, `:774-805`), result via
  `mpsc` → drained by `poll_graph_bakes` (`:2783`) called from `app_poll.rs:102`.
- **The worker has no GPU access** — `self.gpu` is `Option<..>` on `AzraApp` and is not passed in.
- Uploads: `app_poll.rs:112-155` → `material_textures::upload_map` (`:195`), budget 4/frame live
  else 1/frame.

### 3.2 The seam — precompute, don't restructure

**Key insight (verified):** in the planar bake domain the seeds are a pure function of `(u,v)`
and the body AABB (`node_graph_bake.rs:345-360`), and `p` is a pure function of the pixel
(`:605-607`). **The entire curvature field is therefore precomputable as a `res²` buffer before
the pixel loop runs.**

**Design:**
1. In `poll_background_tasks` (the legal dispatch window), when a bake is enqueued and
   `graph_uses_sdf_field(&graph)` is true (`:156`), dispatch `curvature_eval` over `res²`
   positions — the UV grid, generated on the GPU from `(bmin,bmax,diag)` so no position buffer
   upload is needed for this consumer (bind a 4-byte dummy at binding 7 and use `seed_mode=1`).
2. Read back `res² × 4 × f32` via the **`MapState` fence pattern**, not a blocking wait:
   `map_async` + `Arc<MapState>` (`app_poll_bake.rs:905-928`), drained in a later poll like
   `poll_primary_bake` (`app_poll_bake_poll.rs:14`).
3. Hand the readback to the worker as a **new field on `SdfFieldSources`** —
   `curv_cache: Option<CurvGrid>`. `sdf_curvature_masks` becomes a **UV lookup** when the cache
   is present and the sample is in the planar domain; otherwise it falls through to today's CPU
   path unchanged.

**Why readback rather than staying on GPU:** the curvature value is not a texture — it is an
*input to a node graph* that then runs arbitrary downstream nodes (Remap, Invert, mixes,
`lower.rs:594-597`) on the CPU. Keeping it on GPU would require porting the whole graph
evaluator. Readback of `256²×4×4B = 1 MiB` once per bake is negligible against the ~500 evals ×
65k pixels it replaces.

**Threading/auto-apply impact:**
- The coalescing latch (`schedule_live_material_graph_bake`, `:2577-2588`, one live bake in
  flight via `graph_live_in_flight`) is **unchanged** — the dispatch simply becomes a
  precondition of the spawn rather than the spawn happening immediately.
- The repaint keep-alive (`app_poll.rs:104-109`) already covers a pending bake; extend its
  condition to also cover "curvature dispatch in flight".
- The `Live` → `Final` settle upgrade (`:2856-2858`) is unaffected — it re-enqueues, which
  re-dispatches. Cache the curvature grid keyed by (body content, res, params) so the Final pass
  reuses the Live grid rather than re-dispatching.

**Frame ordering:** dispatch and readback both occur inside `poll_background_tasks`
(`app_poll.rs:10`), which is the documented legal window (`material_textures.rs:191`).
The bake worker itself continues to touch zero wgpu.

### 3.3 The non-planar branch

An explicit world `Position` wired into the node's input 0 (`:1581-1585`, `:1605-1609`,
`:1647-1651`) is **not** a pure function of the pixel. Two options:
- **P1:** keep the CPU path for this branch (it is the 1-seed case, ~83 evals — 6× cheaper).
- **P2+:** a second dispatch keyed on the evaluated positions (needs the graph evaluated once to
  produce positions — a two-pass bake). **Defer; do not build in P1.**

---

## 4. Consumer B — vertex stamp

### 4.1 Getting positions in — no readback at all

`SdfBodyMeshSlot.verts` is created with **`STORAGE | VERTEX | COPY_SRC`**
(`sdf_gpu_mesh/resident.rs:1707`); pool groups get `STORAGE|VERTEX|COPY_SRC|COPY_DST`
(`pool.rs:214`). **A compute shader can bind it read-only as a storage buffer with no usage
change.** `sdf_mesh.wgsl` already binds it `storage, read_write` (`:56`).

So binding 7 is the **vertex buffer itself**, `array<f32>`, 8 f32/vertex, position at
`o+0..2`. The kernel reads `curv_positions[i*8 + 0..2]`. **Zero position upload, zero readback.**

> ⚠ **Vertex count is GPU-only** (`indirect` args are GPU-written; CPU knows only `vert_cap`,
> `resident.rs:1617`). Dispatch over `vert_cap` with an in-shader guard, or use
> `dispatch_workgroups_indirect`. Do **not** wait for a counter readback.

### 4.2 The carry problem — three alternatives evaluated

`RasterVertex` is **32 bytes**, `position`/`normal`/`uv`, `LAYOUT` at `render/src/mesh_gpu.rs:17`.

| | (i) storage buffer + `vertex_index` | (ii) uv slot + instance flag | (iii) new vertex attribute |
|---|---|---|---|
| Stride change | none | none | **32→36/40** |
| Pool edits | curvature buffer tracks group alloc/rebuild/trim/generation | **none** | 12+ hardcoded `32` literals |
| Bind group | free (group 0 b4, or free group 2) | none | none |
| Mesher change | separate compute pass | 2 lines | 2 lines + `8u`→`10u` |
| Device-loss risk | **Low** | **Low-Med** | **HIGH** |

#### Why (iii) is disqualified

`pool.rs:87-89` states the invariant explicitly:

> *"256B-align (storage-buffer sub-range offsets require %256==0; **vertex stride 32** and index
> stride 4 both divide 256, so **`base_vertex = off/32`** and `first_index = off/4` stay exact
> integers)."*

At stride 36, `gcd(36,256)=4` → `off/36` is **non-integral** for most regions. `base_vertex` is a
*vertex index*, not a byte offset, so the region base becomes **unaddressable by any draw**
(`pool.rs:558`, `:1060`, `:183`). At stride 40, `256/40 = 6.4` → needs `lcm(40,256) = 1280`-byte
alignment, a 12-site rewrite, **+25% vertex VRAM** (≈ +620 MB at the documented 2048-body case,
`pool.rs:64-65`, against a 6 GiB budget). Only 64 bytes is alignment-free = **+100% VRAM**.

**Device-loss reasoning:** a non-integral `base_vertex` silently truncates, producing indirect
draws that read **past the end of the vertex region** into a neighbouring slot or past the buffer
— out-of-bounds indexed draws are a classic device-removal / TDR class, and because the args are
GPU-written the corruption is invisible to CPU asserts. **Reject.**

#### Why (ii) is not recommended despite being cheapest

The mesher writes uv as **hard-coded zeros** — `verts[o+6u] = 0.0; verts[o+7u] = 0.0;`
(`sdf_mesh.wgsl:401` and `:542`), so the channel is genuinely dead for SDF. Discrimination
machinery exists and is proven (`INSTANCE_FLAG_VERTEX_COLOR`, `mesh_gpu.rs:331`, read at
`gbuffer.wgsl:307-308`/`:469-471`), and **SDF DC meshes never set any instance flag**
(`raster_gbuffer.rs:73` builds its instance with no `with_flags`; the only setter is the
`VoxelBody` arm at `:296-298`). So they can coexist.

**But uv is not actually free for SDF:** `gbuffer.wgsl:262-266` samples the material
**displacement map** with `in.uv`, and triplanar reads it. An SDF body with a displacement slot
set would read curvature-as-uv → garbage displacement. Closing that needs extra gating in the VS
*and* the material eval — i.e. it stops being 2 lines, and it permanently entangles two unrelated
features in one channel. **Device-loss risk is low** (no OOB), but it violates *No Hacks*: it is
a stand-in that works until someone assigns a displacement map.

#### ★ RECOMMENDED: (i) parallel storage buffer indexed by `@builtin(vertex_index)`

**The pool's design is a gift here.** For indexed draws, `vertex_index = index_buffer[i] +
base_vertex` (WGSL/Vulkan `VertexIndex` / D3D12 `SV_VertexID` + `BaseVertexLocation`). And the
pool's `base_vertex` **is** the absolute vertex index into the group buffer:

- `pool.rs:558` — `base_vertex = (s.vert_off / 32)`
- `pool.rs:183` — `tile_abs` = `vert_off/32 + t.rel_vert`
- `sdf_mesh.wgsl:619` — `indices[o+3u] = t.amin.w; // base_vertex (absolute vertex offset in pool verts)`
- `sdf_mesh.wgsl:543` — `cell_vert[sl] = vi; // TILE-LOCAL index — the draw applies base_vertex`

So a parallel `curvature: array<f32>`, one f32 per vertex slot, **allocated and offset exactly
like `grp.verts`, is indexable directly by `@builtin(vertex_index)` with NO per-instance base
offset uniform.** That is the cleanest possible outcome and it falls out of the existing design
rather than being bolted on.

Feasibility of a storage buffer in the VS is **already proven in this pipeline**: `materials`
(binding 1) is a read-only storage buffer with `VERTEX | FRAGMENT` visibility
(`pipeline.rs:86-94`), required by `vs_main`'s `materials[in.material_idx]` at
`gbuffer.wgsl:260`. Adding binding 4 to the same group is architecturally identical. Group 2+ is
free (wgpu default `max_bind_groups` = 4).

**Non-pool draw paths** (CPU meshes and unpooled SDF slots draw with `base_vertex = 0` into their
own buffers — `raster_common.rs:130`, `raster_gbuffer.rs:643/749/865`, `raster_pick.rs:308`)
must **flag-gate to curvature = 0**, or bind a per-draw buffer. Since the SDF pool multi-draw
rebinds only 1-3× per pass (group-major sorted, `raster_common.rs:164-175`), a **per-group
curvature buffer bound alongside `grp.verts`** fits the existing rebind rhythm exactly.

**Device-loss reasoning:** the only failure mode is a stale/undersized curvature buffer after a
pool realloc, giving an OOB *storage read* — which in WGSL is **bounds-clamped, not
device-removing**. Mitigate structurally with the pool's existing **generation counter**
(`pool.rs:238`, `resident.rs:1611-1614`): bump on curvature-buffer recreate, and reallocate it in
lockstep with `rebuild_group` / `place_slot_bump` / `trim_slot`. This is the
CLAUDE.md-mandated generation-counter pattern, not a timing trick. Real but bounded work.

### 4.3 Where results land

A per-group `curvature: wgpu::Buffer` (`STORAGE`, sized `vert_cap` f32) parallel to `grp.verts`,
owned by the pool, generation-tracked. Written by `curvature_eval`, read by the G-buffer VS,
passed to the FS as a FLAT varying, consumed by the material eval in place of the texture lookup
when the layer's rung is `VertexCache`.

---

## 5. Phasing

Each phase independently shippable and testable.

### P1 — Kernel + texture-rung switchover (**M**, risk **Med**)
- Add `curvature_eval` entry to `volume_bake.wgsl`; extend `bake_bgl_entries()` with binding 7;
  add the params; compile alongside `classify_finalize` (`gpu_init.rs:1866` is a ready-made
  "N entries off one module+layout" helper).
- Dispatch + `MapState` readback in poll; feed `SdfFieldSources.curv_cache`; UV-lookup in
  `sdf_curvature_masks`.
- **Gate: golden parity test.** GPU kernel vs current CPU `sdf_curvature_masks` over a fixed body
  set, tolerance on the signed `curv` output. Extend the existing CPU tests at
  `node_graph_bake.rs:3199` (sphere, uid 1) and `:3218` (box, `[0.9,0.9,0.0]`, uid 2).
- **Testable headless: yes** (compute + readback + numeric compare, the `graphics` GPU-test
  harness pattern). **Owner's eyes: not required** for parity, only for bake *speed* feel.
- **Ship value on its own:** the texture bake stops being ~500 CPU scene-evals × 65k pixels.

### P2 — Vertex stamp + carry (**L**, risk **Med-High**)
- Per-group curvature buffer + generation lockstep in `pool.rs`.
- Dispatch `curvature_eval` over `slot.verts` after DC (guard by `vert_cap`, no counter wait).
- Binding 4 on the raster BGL; VS reads by `vertex_index`; FLAT varying; flag-gate non-pool draws.
- **Testable headless: partly** — the kernel output can be readback-compared against the P1
  golden. **Owner's eyes required** for the visual result (shading correctness, no popping on
  pool rebuild).

### P3 — Panel un-gate (**S**, risk **Low**)
- `GeneratorRung::VertexCache` stops lowering as `Auto` (`model.rs:344`, `lower.rs`).
- Update the `model.rs:336-343` doc comment — it currently documents the blocker this work removes.
- **Owner's eyes required** (it is a UI affordance).

---

## 6. Risks / escalations

### Top 3

1. **★ Coordinate-frame parity (`world` vs body-local).** `eval_scene_at` takes **body-local**
   despite its parameter name (`volume_bake.wgsl:76-79`); CPU `sdf_at` takes **world**
   (`node_graph_bake.rs:175-194`). Every seed, every Newton step, every ±e tap must be in the
   right frame. A silent frame mismatch produces *plausible-looking but wrong* curvature — the
   worst failure mode, because it passes eyeballing and fails only the golden test. **The P1
   parity gate exists primarily to catch this.**

2. **Frame budget for the vertex dispatch.** ~500 evals/sample is fine at 65k texels; at a
   **million-vertex** body it is ~500M scene evals — far past a frame. **Mitigations, all
   required:** (a) the vertex rung uses the **1-seed** path (~83 evals, 6× cheaper) — this is the
   CPU's own non-planar branch, not a simplification; (b) dispatch **once per bake, not per
   frame** — curvature is static until the body's field changes, so key it on the existing
   `BodyContentId`; (c) reuse the TDR slicing pattern (`app_poll_bake.rs:795-830`) and respect
   `MAX_BAKES_PER_FRAME = 4` (`:38`) and the 1500 ms TDR guard (`app_poll.rs:316-318`).
   **Escalate if** a body's vertex count makes even a sliced one-shot dispatch exceed budget.

3. **Pool-realloc lockstep for the curvature buffer.** The curvature buffer must be reallocated,
   relocated and generation-bumped on every `rebuild_group` / `place_slot_bump` / `trim_slot`.
   Miss one and vertices read a stale neighbour's curvature — a *wrong-data*, not a crash. This is
   the bulk of P2's risk and the reason P2 is sized **L**.

### Other

- **Prim buffer re-binding per body:** not a problem — a fresh per-body buffer is already
  allocated per dispatch (`app_poll_bake.rs:157-163`). Curvature inherits this. It does mean
  **one dispatch per body**, not one for the scene.
- **Huge/sparse bodies:** `eval_scene_at` is analytical — cost scales with `prim_count` and scope
  depth (max 8, `:1343`), **not** with grid resolution or sparsity. Sparse bricks and their ±100
  sentinels are irrelevant here; that was the rejected grid-Laplacian's problem, and avoiding it
  is the whole point of using the analytical field.
- **Register pressure:** 11 × `array<_,8>` stacks in `eval_scene_at`. Use workgroup_size 64;
  measure occupancy in P1.
- **Non-planar `Position` input:** CPU fallback in P1 (see §3.3). Not a parity break — it is the
  same CPU code as today.
- **Shader recompile cost:** `volume_bake.wgsl` is standalone, so this does **not** invalidate the
  5 `sdf_*` render pipelines (no 25-43s cold recompile). It does rebuild the 6 volume_bake
  pipelines. Run `cargo test -p graphics` (Naga parse) after the shader edit.

### Not disproportionate

The §0.5 test passes: P1 alone pays for itself on bake performance and lands the golden parity
harness; P2/P3 are additive. Nothing here reintroduces a deleted path (no analytical *render*
path, no BVH, no GLSL) — `volume_bake.wgsl` is live, load-bearing, production bake
infrastructure, and this adds a third consumer to a function whose stated purpose is to have
multiple consumers that cannot diverge.

---

## 7. Summary

| | |
|---|---|
| **Host/template** | `volume_bake.wgsl`, new 7th entry point modeled on `classify_bricks` (`:1539`), calling the existing `eval_scene_at` (`:1293`) |
| **Kernel** | `curvature_eval(@workgroup_size(64))`; positions in at binding 7, `(mask,curv,convex,concave)` out at binding 2 |
| **Consumer A** | Precompute a `res²` curvature grid in poll → `MapState` readback → `SdfFieldSources.curv_cache` → UV lookup in `sdf_curvature_masks` |
| **Consumer B** | Bind `slot.verts` directly as the position input (it is already `STORAGE`) — no upload, no readback |
| **Carry** | **(i) parallel per-group storage buffer indexed by `@builtin(vertex_index)`** — pool `base_vertex` is already the absolute vertex index, so no offset uniform; generation-counter lockstep |
| **Parity** | Structural: one `eval_scene_at`, one curvature formula, CPU-computed `e`/`accept`/`diag` shared by both consumers |
| **Phases** | P1 kernel + texture (M/Med, headless-testable) → P2 vertex + carry (L/Med-High) → P3 un-gate (S/Low) |

---

## 8. Addendum (2026-07-27) — P2/P3 AS BUILT, and the honest gap

### 8.1 What shipped, vs the design above

The plan's Consumer B design (§4) shipped almost exactly as specified, with one deliberate
deviation on the carry's OWNERSHIP model (§4.2's option (i) storage buffer indexed by
`@builtin(vertex_index)` shipped as designed; the plan's assumption that it must live in
`pool.rs`'s per-group bump allocator did NOT):

- **Dispatch**: `gpu_curvature::encode_curvature_vertex_dispatch` (new fn) binds the mesh's own
  `verts` buffer (pool group's or the unpooled slot's) as `curv_positions` (binding 7,
  `SEED_POSITIONS` mode) and writes the kernel's NATIVE 4-wide output directly into a fresh
  persistent buffer — no compaction pass, no readback. `poll_vertex_curvature`
  (`node_graph_bake.rs`, called from `app_poll.rs`) is the poll-phase orchestrator: resolves the
  active body's material graph, finds `VertexCache`-rung generator keys
  (`vertex_curv_keys_for_graph`), checks staleness, dispatches.
- **Ownership — CHANGED from the plan**: instead of a per-group buffer living inside
  `SdfMeshPool` (tracked through `place_slot_bump`/`rebuild_group`/`trim_slot`), the carry buffer
  is owned in a NEW, INDEPENDENT map — `GpuCtx.sdf_body_curvature: HashMap<NodeId,
  CurvVertexSlot>` — keyed by body `NodeId`, rebuilt WHOLESALE (never incrementally patched)
  whenever `(mesh_generation, vert_cap)` no longer matches the mesh's CURRENT state. This was a
  deliberate scope call: mirroring the pool's bump-allocator lockstep for a second buffer was the
  plan's own stated "L, Med-High risk" bulk of P2 (§5, §6 risk 3); a wholesale rebuild keyed on
  the mesh's OWN generation counters is provably correct without touching `pool.rs`'s allocator
  internals at all, at the cost of a full re-dispatch (not a patch) on every re-bake — acceptable
  under the plan's own "once per bake, not per frame" cost model (§6 risk 2).
- **Carry**: group 2 on the raster pipeline (`RasterPipeline::curvature_bgl` +
  `curvature_dummy_bind_group`, `render/src/pipeline.rs`), one `storage, read` buffer, VERTEX-only
  visibility. `gbuffer.wgsl`'s `vs_main` reads `curv_vertex[vertex_index*4+1]` (the kernel's
  `curv` channel) gated by a NEW instance flag `INSTANCE_FLAG_VERTEX_CURVATURE` (bit 2,
  `render/src/mesh_gpu.rs`), carried to the fragment stage as a FLAT varying (`vertex_curv`).
- **Device-loss audit**: every draw site sharing the raster pipeline's bind-group-2 layout now
  binds SOMETHING at group 2 — see the report body for the full table. The picking pipeline's
  OWN layout (camera-only) correctly gets no group-2 call at all (`raster_common::CurvBind` is
  `Option<..>`; `None` for picking). This audit caught a REAL gap immediately: `render/src/
  ssao.rs`'s GPU test harness draws through the same raster pipeline and needed the same dummy
  bind — proven by `cargo test -p render` failing validation before the fix and passing after.
- **Rung UI (P3)**: un-gated in `material_panel/view.rs`. New Generator layers auto-default to
  `VertexCache` when the active body has no authored UVs (`GenCtx::has_uvs`, sourced from
  `cache.poly_custom_data[nid].uv` — the HONEST per-body signal, not the "any poly body exists in
  the scene" heuristic the materials review triage flagged as P1-2, which is left as-is; fixing
  it is a separate, already-tracked item).

### 8.2 The one honest gap: UV-space bake for poly bodies with real UVs

**Not built this session.** The plan's §3 texture-rung design (precompute a `res²` grid over the
PLANAR bake domain) is what P1 already shipped, and it remains correct for its actual use case —
a material's texture-rung generator, regardless of target — but it is NOT a true "bake at each
texel's real surface position via the UV→surface mapping" implementation. Building that requires:
evaluating the mesh's UV→3D mapping per texel (not a plane), which needs either a per-triangle
rasterization-into-UV-space pass or a UV-parametrized surface query — a second, separate kernel
design, not a small addition to `curvature_eval`. It was correctly out of scope for P2 (the plan's
own §3.3 already deferred the non-planar/explicit-Position branch to "P2+"), and remains out of
scope here because **every generator-eligible body today is SDF-only** (`GenCtx::has_sdf` gates
the "Add generator" button) and no SDF body has real UVs (`sdf_mesh.wgsl` hardwires `uv=0`) — so
the gap has ZERO live impact today. It matters only the day poly-body generators are enabled.
Named follow-on: **"UV-space Generator bake for poly bodies with authored UVs"** — do not build it
speculatively; the vertex rung already gives poly bodies (which DO have real vertex positions on
their actual surface, unlike a bare UV coordinate) a correct fallback via the SAME kernel, so this
is a quality/parity improvement for a texture-based workflow, not a functionality gap.

### 8.3 The semantic gate

`sdf_gpu_mesh::tests_gpu_curvature::vertex_rung_box_flat_face_reads_zero_edge_reads_high` — THE
test that would have caught the "white gift-wrap cross." Dispatches the PRODUCTION vertex-rung
kernel path (`SEED_POSITIONS` mode, same fn signature `encode_curvature_vertex_dispatch` uses in
production) over hand-placed box "vertices" standing in for a real DC mesher's output. Measured:

| probe | curv | mask | convex |
|---|---|---|---|
| face center (+Z) | +0.00000 | 0.0000 | — |
| face center (−X) | +0.00000 | 0.0000 | — |
| edge midpoint | +0.98606 | 1.0000 | 1.0000 |
| corner | +0.99293 | 1.0000 | 1.0000 |

Flat faces read exactly zero; the true dihedral edge and corner read strongly convex, with the
corner (3 faces meeting) reading higher than the edge (2 faces meeting) — the correct physical
ordering. A planar-domain bake reprojected via triplanar could not produce this pattern by
construction (it has no notion of "this UV pixel maps to a flat region of this particular shape").
