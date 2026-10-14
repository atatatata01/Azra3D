# Phase D1 — GPU-driven instance culling, commit-by-commit plan

**Date:** 2026-08-03 · **Branch:** `feat/viewport-perf` (worktree `.claude/worktrees/viewport-perf`)
**Parent:** `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md` §D1. Entry gates: ALL CLOSED (Q1 signed `b8b56d1`,
tri-bound closed `601ffe2`, graphics suite green 72/72).

**Worktree build note:** no cmake on this machine — manifold links via the offline path:
`MANIFOLD_CSG_LIB_DIR=F:\PROJECT\FourthLAyerAi\.claude\worktrees\viewport-perf\manifold-prebuilt`
+ `MANIFOLD_CSG_LIB_KIND=static` (libs copied from the root target's cmake build).

**Architecture.** The persistent instance buffer is `gpu.raster.instance_buffer` (one owner,
gains STORAGE usage); written ONLY on structural rebuild, never on camera frames. A compute pass
reads it + a per-instance CullMeta buffer (world AABB + batch id from `structural_world_aabb`),
tests `frustum_planes`, compacts survivors into `visible_instances` (VERTEX|STORAGE) within each
batch's reserved region, atomicAdd-ing `instance_count` in GPU DrawIndexedIndirect args. CPU
encodes one `draw_indexed_indirect` per mesh batch (`draw_indexed_indirect` is core wgpu — no
feature gate needed for the fallback story). Shadows get their OWN cull dispatch per cascade into
separate args regions — the camera-culled set is NEVER bound for shadows (a behind-camera caster
still casts; preserves C.2). Picking is already decoupled (own per-draw instance buffers).

## Commits

1. **DONE** — `graphics/src/instance_cull.wgsl` (`cs_reset_args` + `cs_frustum_cull`,
   positive-vertex AABB vs 6 planes, `passthrough` sabotage flag), `bgl::instance_cull_bgl_entries()`
   (uni 0, ro 1–3, rw 4–5), `INSTANCE_CULL_WGSL` const, parse+VALIDATE test. 72/72 green.
2. `render/src/instance_cull.rs` — `InstanceCullSystem` (buffers, capacities, `generation: u64`,
   pipeline Option). Owner: `GpuCtx.raster_cull`. Create/grow ONLY in `poll_background_tasks`
   (next to SdfMeshPool site `app_poll_gpu.rs:471`), sized from draw stream; background-compiled
   pipeline; unready frame → `gpu_cull_active=false` (fail-closed CPU path). Add STORAGE to
   `raster_instances` usage (`render/src/pipeline.rs:392,:580`). Document in
   GPU_INVALIDATION_MATRIX.md.
3. Batch derivation: extract shared `mesh_instance_batches()` into `raster_common.rs` (dedupe the
   main-pass merge loop `raster_gbuffer.rs:1621-1652` and `count_mesh_instance_draws`); structural
   upload of cull_meta/batch_meta/instances. Camera frames upload ONLY the CullParams uniform.
4. Dispatch (reset+cull) in `dispatch_raster_pass` before the G-buffer pass; per-batch
   `draw_indexed_indirect` encode when `gpu_cull_active`. Special kinds (voxel/SDF pool/resident)
   unchanged. `AZRA_NO_GPU_CULL=1` → CPU loop verbatim. `tw!()` only when dispatched; `gpuCull` slot.
5. Shadow cascades: args/visible regions ×(1+NUM_CASCADES); one cull dispatch per cascade with
   that cascade's ortho planes; shadow loop (`raster_gbuffer.rs:1363-1445`) encodes from region 1+c.
   Dependency audit: picking unaffected; wire pass stays CPU-listed; `ssao.rs:643` binding still valid.
6. Camera-only frames: skip CPU frustum/occlusion/C.6-sort for mesh entries under GPU cull; drop
   `plane_key` from `need_inst_upload`. Verify orbit @100k → `structReb=0 instUp=0`.
   **Known trade-off:** CPU `OcclusionBuffer` (86.8% keeper) bypassed for mesh instances until D2
   GPU hi-z; A/B via `AZRA_NO_GPU_CULL=1`, publish the number.
7. Gate tests: partition-agreement test (`mesh_instance_batches` vs `count_mesh_instance_draws`);
   sabotage `AZRA_CULL_PASSTHROUGH=1` (drawn instances explode) + `AZRA_NO_GPU_CULL=1`
   (instUp=1/frame + CPU ms explode @100k).
8. T1 measure (release, 1440p, med 120 frames): `AZRA_INSTANCE_STRESS=100000 AZRA_SCOREBOARD=1`,
   orbit → `med ≤ 16`, `draws ≈ N_meshes`, `instUp=0`, `structReb=0`; multi-mesh variant proves
   draws = N_meshes not N×M; record rows in `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md` §D1.

## Measurements (2026-08-03, release, camera idle, RTX-class primary)

| Scene | Config | med | draws | instUp | frust/sort/occ CPU | gpu_ms | Verdict |
|---|---|---|---|---|---|---|---|
| 100k instances (1 mesh) | GPU cull | **8.1 ms** | **1** | **0** | 0.0 / 0.0 / 0.0 | 2.09 (sh 0.86 + gb 1.00) | **T1 PASS (≤16)** |
| 10k instances | GPU cull | 5.1 ms | 1 | 0 | 0 | 0.35 | pass |

Sabotage (100k, release, camera idle):
- `AZRA_NO_GPU_CULL=1` (CPU path): **med 16.1 ms, draws=7850**, dispatch 13.3 ms, gpu_ms 5.1
  (shadow 1.9 + gbuf 2.6) — the GPU path halves the frame and collapses 7850 encodes to 1.
  CPU occlusion culled 78k/90k yet still lost: encode cost dominates. Load-bearing: PROVEN.
- `AZRA_CULL_PASSTHROUGH=1`: med 8.1→8.1, gpu_ms 2.09→2.16 (gbuf 1.00→1.08). Barely moves
  because the default stress camera has ~everything in-frustum — the sabotage needs a
  zoomed/orbit camera with real frustum-out volume to explode. Re-run with orbit in commit 8.

**Commit-8 closure rows (2026-08-04, release, AZRA_ORBIT=0.5 continuous orbit):**
- Multi-mesh proof: `AZRA_MULTI_MESH_STRESS=5x2000` (10k instances) → **draws=5 exactly** (= N
  meshes, not N×M), instUp=0, med 0.65 ms. PASS.
- Zoomed sabotage (`AZRA_INSTANCE_STRESS=100000 AZRA_CAM_DIST=8`, real frustum-out volume):
  GPU cull **0.46 ms** vs `AZRA_CULL_PASSTHROUGH=1` **1.77 ms** GPU (shadow 0.16→1.11,
  gbuf 0.24→0.59) — ~4× GPU work without the cull. Load-bearing on GPU cost: PROVEN.
- Demo scene v2 (5447 instances, 6 shared meshes): 7 draws, 1.7 ms under orbit, per-cascade
  shadow cull live.

**D1 status: measurement-complete.** Open: owner visual sign-off (shadows under GPU cull;
PT half-res interaction fix 01ee689).

Caveats: camera idle (orbit sweep still to be scripted); shadow pass still CPU-encoded from
cull bits (all-zero under GPU cull = everything casts — correct, unculled; commit 5 fixes);
single-mesh scene — multi-mesh draws=N proof pending (commit 8).

Commit-5 design note: one shared CullParams uniform can NOT serve camera + 3 cascade
dispatches in one encoder (queue.write_buffer applies before submit — last write wins).
Needs per-pass param slots (4 × 256-aligned) with per-pass bind groups or dynamic offsets.

## Rule-conflict notes
- Frame Ordering vs render-phase sizing → poll-phase sizing + fail-closed frame + generation
  counters (not a timing trick).
- "camera-only frame uploads nothing" read as instance-bytes = 0 (`instUp=0`); the 112-B
  CullParams uniform per frame is the same class as the camera uniform.
