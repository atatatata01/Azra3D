# PATHTRACER_PHASE2_SPEC.md — Unified Triangle Path Tracer over the DC Mesh

**Branch:** `feat/pathtrace-render` · **Date:** 2026-07-16 · **Status:** SPEC — awaiting owner review before code.
**Parent:** `PATHTRACER_PLAN.md` (Phase 2 is now the *only* path — analytical PT banned by owner).
**Scope decision (owner):** trace **all triangle body types at once** — SDF (DC mesh), polygon,
voxel-sculpt, imported. They already share ONE format (`RasterVertex`) in ONE cache (`MeshCache`), so
one BVH path covers every body.

---

## 1. Core idea

The raster pass already turns every body into world-placed triangles. The path tracer will trace the
**same triangles** via a two-level BVH (TLAS over bodies → BLAS per body), built on the CPU, uploaded
as storage buffers, and traversed in a WGSL compute shader that **replaces `trace_ray`**. No analytical
SDF, no `scene_sdf`, no `scene_analytical` — those functions stay compiled for the deferred pass's
reflections/fog but are never called by PT.

**Data flow (per frame):**
```
MeshCache (body-local RasterVertex tris)  ──build (bg thread)──▶  BLAS per body (triangle-leaf BVH)
raster_gbuffer enumeration (world InstanceData + material_idx) ─▶  TLAS (world AABBs over bodies)
                                                   │
                            upload SSBOs (poll_background_tasks, generation-counted)
                                                   ▼
   pass_pathtrace:  camera ray → traverse TLAS → transform ray into body space → traverse BLAS →
                    Möller–Trumbore → interpolate normal + material_idx → shade (RasterMaterial buffer,
                    NEE lights, env) → bounce → accumulate → HDR → shared bloom/tonemap → LDR
```

**Key invariant preserved:** vertices stay **body-local**; the TLAS leaf carries the body's
`model`/`inv_model`/`normal` matrices (straight from the existing `InstanceData`). A ray entering a body
is transformed into body space by `inv_model`, so **moving/rotating a body needs only a TLAS refit — zero
BLAS rebuild, zero re-mesh** (mirrors the raster instance model and the app-wide "body move = no rebake"
rule).

---

## 2. Data structures

### 2.1 CPU — triangle BVH (extend `core_math/src/bvh.rs`, no new file)
Reuse the existing SAH core (`build_bvh`, `Aabb`, `BvhNode` 32 B, `NUM_BINS`). Add a triangle-leaf
builder that returns GPU-ready nodes + a triangle-index remap:

```rust
// core_math/src/bvh.rs  (NEW, alongside build_bvh)
pub struct TriBlas { pub nodes: Vec<BvhNode>, pub tri_order: Vec<u32> } // leaf.first→tri_order slice
pub fn build_tri_blas(positions: &[[f32;3]], indices: &[u32]) -> TriBlas;
```
- Leaf encoding matches `build_bvh`: `count>0` → leaf, triangles at `tri_order[first..first+count]`;
  `count==0` → internal, children `nodes[first]`, `nodes[first+1]`.
- Small multi-tri leaves (target ≤4 tris/leaf) to bound node count.
- Pure function, deterministic, **fully unit-testable with zero GPU** (Milestone A gate).

### 2.2 GPU layouts (new `#[repr(C)] Pod` structs in `render/src/mesh_gpu.rs` or a new `pt_accel.rs`)
```rust
struct PtVertex   { pos: [f32;3], _p0: f32, normal: [f32;3], _p1: f32 }   // 32 B (std430-friendly)
struct PtTri      { v0: u32, v1: u32, v2: u32, material_idx: u32 }        // 16 B — indices into PtVertex
struct PtBlasNode = BvhNode (reused, 32 B)
struct PtInstance { model:[[f32;4];4], inv_model:[[f32;4];4], normal:[[f32;4];3],
                    blas_node_off:u32, tri_off:u32, vert_off:u32, material_base:u32 } // 176 B
struct PtTlasNode = BvhNode (reused) — leaf.first = instance index, leaf.count marks leaf
```
All bodies concatenate into **four global SSBOs**: `pt_verts`, `pt_tris`, `pt_blas_nodes`,
`pt_instances`, plus `pt_tlas_nodes`. Per-body offsets live in `PtInstance`. Materials **reuse the
existing `RasterMaterial` storage buffer** (already resident, indexed by `material_idx`) — no new
material buffer, one source of truth.

### 2.3 Accel cache (new `render/src/pt_accel.rs`)
`PtAccelCache` — parallel to `MeshCache`, keyed by `MeshCacheId`:
- Per body: cached `TriBlas` + its `source_hash` (reuse `MeshCache`'s hash → BLAS invalidates exactly
  when the mesh does). Built on the **rayon background pool** like DC meshing; results drained in `poll`.
- Holds concatenated CPU arrays + dirty flags; hands assembled buffers to the GPU uploader.
- Generation counters per SSBO (Frame Ordering / GPU-lifetime rules).

---

## 3. Build & upload strategy

1. **BLAS build (background):** when `MeshCache` produces/updates a body's mesh, enqueue a `build_tri_blas`
   job for the same `MeshCacheId` on rayon. One BLAS per body; rebuilt only on `source_hash` change
   (sculpt stroke rebuilds only the stroked body, budgeted ≤2/frame like mesh uploads — Frame Budget Rule).
2. **TLAS build (per frame, cheap):** reuse the **exact** body→world enumeration from
   `raster_gbuffer.rs:45-280` (extract it into a shared `fn gather_draw_instances(&self) ->
   Vec<(MeshCacheId, InstanceData, ...)>` so PT and raster can never diverge — REUSE RULE). Build a
   world-AABB TLAS over instances (few hundred bodies → sub-ms median-split). Transform-only changes =
   TLAS refit, no BLAS touch.
3. **Upload (in `poll_background_tasks` ONLY — Frame Ordering Contract):** concatenate per-body BLAS +
   verts + tris into the global SSBOs; (re)create buffers on growth with a **generation bump**; bind
   group rebuilt in `viewport_render` when generation changed. Never create buffers in a panel/input
   handler.

---

## 4. BGL / binding changes

`pt_bgl` is currently duplicated at **`app_new.rs:80`** and **`app_update.rs:529`** (background thread).
Per the BGL-Single-Source rule, **extract `graphics::bgl::pt_bgl_entries()`** and call it from both sites
(kills the duplication that already exists). Add 5 storage-read bindings for the new SSBOs (pick free
slots, e.g. 42–46; keep the dummy 40/41 or repurpose). Update:
- `pt_bgl_entries()` in `bgl.rs` (new single source),
- both `create_bind_group_layout` call sites → call the shared fn,
- `pt_bg` construction in `viewport_render.rs:652` (bind the 5 new buffers),
- the WGSL `@binding(N)` literals in the new traversal entry.

Also bind the existing **`RasterMaterial`** buffer into `pt_bgl` (PT needs it for shading — today PT
reads the SDF `prim_data` materials, which we're abandoning).

---

## 5. WGSL — new traversal + shading

New file `pathtrace_tri_entry.wgsl`, built by a new `build_pathtrace_tri_wgsl()` that concatenates ONLY
what triangle PT needs (header + RNG + lighting + env + material-from-RasterMaterial), **not**
`sdf_prim_eval`/`sdf_scene` (drop the analytical stack machine from the PT build entirely). Contents:
- `traverse_tlas(ray) -> Hit`: short-stack BVH2 over `pt_tlas_nodes`; at each instance leaf, transform
  ray by `inv_model`, call `traverse_blas`.
- `traverse_blas(local_ray, inst) -> Hit`: short-stack over `pt_blas_nodes[blas_node_off..]`; leaf tris
  via **watertight Möller–Trumbore**; keep nearest `t`, barycentrics, tri index.
- On hit: interpolate vertex normal (transform to world by `normal` matrix), fetch `material_idx` from
  `PtTri` → read `RasterMaterial` → PBR shade. NEE over lights (existing `light_data`), env
  (`sample_env`), cosine/GGX BSDF sampling, RR after bounce 2, firefly clamp (`pt_clamp` — finally wired,
  closes B8), progressive accumulate (first-frame guard already in place).
- Write HDR (binding 0) **and** primary-hit world normal + depth to `gbuf_normal`/a PT-owned pair so
  `pt_denoise` gets real guidance (closes B9). DOF via `pt_dof_aperture` in ray gen (closes B8).
- **Naga full-validate test** for `build_pathtrace_tri_wgsl` (not parse-only) — mandatory before first run.

---

## 6. Re-enable path (make PT reachable again — safely)

1. `resolve_render_mode`: replace the dead `shading_mode==PathTrace && nv>0` arm with
   `shading_mode==PathTrace && pt_tri_ready` where `pt_tri_ready = TLAS has ≥1 instance` (geometry
   presence = triangles, not SDF prim count — lifts the old `nv>0` gate, B4). Keep it BELOW `draft_compile`
   but it may sit above `unified_raster` because triangle PT writes HDR only (no G-buffer fusion — the
   same reasoning that was correct before, now actually true since we don't touch `scene_sdf`).
2. Re-apply the geometry-agnostic plumbing from the reverted Phase 0 (tonemap runs for PT; accumulation
   survives the `!is_dirty` gate bounded by `PT_MAX_ACCUM_FRAMES`; reset on real change only).
3. Raster pass still runs underneath (GPU picking / selection MUST keep working — test after).

---

## 7. Milestones (each: `cargo build -p app` + stated test green before the next)

- **A. CPU BLAS builder** — `build_tri_blas` + unit tests (ray-vs-BVH hits match brute-force triangle
  intersection on a cube + a 2-body scene). `cargo test -p core_math`. Zero GPU, zero risk.
- **B. Accel cache + CPU assembly** — `PtAccelCache`, background build, concatenated arrays; assert
  offsets/counts against `MeshCache`. No shader yet. Log sizes.
- **C. GPU upload + BGL** — SSBOs, `pt_bgl_entries()` extraction, generation counters, bind group. No
  dispatch yet (buffers exist, validated).
- **D. Traversal shader (primary rays only)** — `pathtrace_tri_entry.wgsl`, full-validate test, dispatch
  writing albedo-on-hit / env-on-miss. **First visible result** — silhouettes must match raster exactly.
  ⟵ owner visual-verify gate (1 SDF + 1 poly + 1 voxel body all appear).
- **E. Shading + bounces** — RasterMaterial PBR, NEE, env, BSDF, RR, clamp, accumulation, tonemap wiring,
  re-enable in `resolve_render_mode`. Converges to a defensible image. ⟵ owner visual-verify gate.
- **F. Denoiser + DOF + perf (Phase 3 seed)** — PT-owned normals into `pt_denoise`, DOF, half-res during
  interaction, TLAS refit on body move, temporal reproject. Iterative, profiled.

---

## 8. Risks & mitigations
- **Device-loss on first dispatch (as Phase 0 hit).** Mitigate: full naga *validation* (not parse) before
  any run; bounds-check every SSBO index in-shader (clamp offsets); Milestone D dispatches primary-only
  (no bounce recursion) to isolate traversal from shading. Test on the debug build (validation layer on)
  first — turns silent `0xcfffffff` into a precise error.
- **Watchdog TDR from deep traversal** (the documented VXTDR class). Mitigate: bounded stack (depth ≤ 32),
  half-res during interaction, 1 spp/frame progressive.
- **TLAS/raster divergence.** Mitigate: single shared `gather_draw_instances` — PT and raster read the
  same enumeration (REUSE RULE; no second copy of the body-transform logic).
- **BGL duplication.** Mitigate: extract `pt_bgl_entries()` — fixes the existing 2-site duplication rather
  than adding a third.
- **Save/undo/selection.** No serialized state added (accel is pure runtime cache, rebuilt from mesh).
  Selection still flows through the raster picking pass (unchanged). Nothing in Priority-Ladder rungs 1–3.

## 9. CLAUDE.md compliance
Display Invariant (PT = one technique for the view, HDR-only, no G-buffer fusion) ✓ · Frame Ordering
Contract (buffers in `poll_background_tasks`) ✓ · Generation counters ✓ · Frame Budget (bg BLAS, ≤2
uploads/frame) ✓ · BGL single source ✓ · REUSE (shared enumeration, reuse `bvh.rs` SAH + `RasterMaterial`)
✓ · Shader parity/validation tests ✓ · No serialized-format change ✓ · Analytical PT stays dead ✓.
