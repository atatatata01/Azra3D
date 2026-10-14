# Phase D4 — GPU cluster selection + hardware raster, commit plan

**Date:** 2026-08-04 · Branch `feat/viewport-perf` · Parent `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md` §D4.
D0-D3 complete. Gate: **T2 = 25M source tris ≤16ms** (AZRA_DENSE_STRESS=8x9 ≈ 25.2M), silhouette
<1px at switch, watertight on the RENDERED cut.

## Decisions
- **Multi-draw**: fixed-cap self-skip first (MAX_VISIBLE_CLUSTERS_PER_CID=16384 args/region,
  index_count=0 self-skip — the SdfMeshPool pattern, zero new features). wgpu 22.1 HAS
  MULTI_DRAW_INDIRECT_COUNT (bit 34) + multi_draw_indexed_indirect_count — commit 7 adds it
  adapter-gated as a measured A/B optimization. Overflow = drop + scoreboard counter (visible).
- **ClusterGpu (80B)**: bounds sphere + cone + SELF group sphere + PARENT group sphere +
  (self_error, parent_error, index_offset, index_count). Group spheres FLATTENED per cluster so
  all siblings decide identically → crack-free cut. Draw iff self_err_px ≤ 1.0 < parent_err_px;
  err_px = err * max_scale * viewport_h / (2·max(dist−r,eps)·tan(fov/2)); inside sphere = ∞.
- **Residency**: `GpuCtx.cluster_pool` (ClusterGpuPool: per-cid vertex [f32;8] uv=0 (same raster
  pipeline, zero shader changes) + index + ClusterGpu storage buffers, stamp, generation).
  Upload in poll_cluster_bake, ≤1 DAG/frame (~100MB write_buffer hitch accepted for D4; chunked
  = D7 follow-up, flagged).
- **Cut shader**: NEW cluster_cut.wgsl + ClusterCutSystem (mirror InstanceCullSystem: 5 regions =
  camera-early + 3 cascades + camera-late, 256B param slots, generation-keyed bind groups).
  Thread per (instance, cluster); reads InstanceData from raster.instance_buffer (already
  STORAGE); frustum sphere + error cut + backface cone (skipped under non-uniform scale —
  conservative) + HZB (commit 6); appends args with first_instance = structural slot
  (INDIRECT_FIRST_INSTANCE already on).
- **Exclusion**: new DRAW_KIND_CLUSTERED marked special → structurally out of D1 batches/encode.
  Picking stays on source mesh (P0 selection). Shadows: cascade cut regions (frustum-only) in
  commit 5 — source-mesh shadow draw removed only when the replacement lands.
- **Flat-shading/uv=0 fidelity** on clustered bodies: disclosed D3 limitation, G6/D5 material
  resolve territory, not a D4 gate (dense stress untextured).

## T2 GATE — PASS 2026-08-04 (commit 5 build, 928×654 window, orbit 0.5°/f)

`AZRA_DENSE_STRESS=8x9` = 8 unique bodies × 3.146M = **25.2M source tris**, all 8 routed
to the cluster path (warm cache; cold run bakes 8 DAGs in background without frame stalls):

| Path | gpu_ms | gpuSh | gpuGb | frame med |
|---|---|---|---|---|
| Raw raster | 6.6–7.2 | 3.4–3.7 | 3.0–3.4 | 0.85 |
| **Clustered** | **4.6–5.3** | **0.36–0.76** | 4.2–4.9 | 0.70 |

**T2 ≤16ms: PASS with 3× headroom** (raw also passes on this 4090 — the cluster win at this
scale is ~30% total and 9× on shadows; gbuf is currently HIGHER clustered (empty-slot fetches
8×16384 args ×4 passes + per-124-tri draw granularity) — commit 7's MULTI_DRAW_INDIRECT_COUNT
targets exactly that. The structural win compounds at T3 densities + streaming.)

Also fixed in commit 5: the cut compute was encoded AFTER the G-buffer pass (commit-4 bug) —
multi-draws consumed one-frame-stale args; now encoded before the shadow passes.

## Status / deferrals
- Commits 1-5 + T2 gate: DONE. Cluster shadows live (cascade cut regions, cone-skip).
- **Commit 6 (cluster HZB early/late): DEFERRED to D5** — flagged deviation. Rationale: T2
  passes without it; cluster-level occlusion belongs with the vis-buffer work where the HZB
  feeds the same pass; D4 clusters get frustum+cone+error only.
- Commit 7 (count path): DONE (74f6dd1) — 25M clustered gpu 4.6-5.3 → **3.5-4.1ms** (raw 6.6-7.2,
  ~2× win); counted cascade draws read ~0.00ms. Fixed-cap fallback kept (adapter-gated).
- Commit 8 (watertight-on-rendered-cut readback) + 9 (silhouette visdiff): open; the CPU-side
  partition gates + owner visual inspection stand in meanwhile.
- Commit 10 (default-on): NOT flipped — clusters stay behind AZRA_CLUSTERS=1 until 7-9 land
  and the owner signs the visual check.

## Commits (perf check after EVERY one: AZRA_DENSE_STRESS=1x9 + demo scene, gpu_ms med)
1. ClusterGpu flatten + CPU cut_replay + partition/monotone tests (pure, no GPU).
2. Residency pool + poll upload arm (≤1 DAG/frame) + matrix row. Nothing drawn.
3. cluster_cut.wgsl (reset+cut, frustum+cone+error) + ClusterCutSystem + counters readback —
   proves cut sizes match cut_replay. No draws.
4. **Clustered raster**: DRAW_KIND_CLUSTERED + multi_draw in G-buffer pass A; AZRA_CLUSTERS=1
   opt-in; sabotage AZRA_CLUSTER_CUT_PASSTHROUGH=1 (L0 everywhere). Measure empty-slot overhead.
5. Shadow cascades via cut regions (replacement-before-removal).
6. HZB early/late for clusters (flags + cs_cluster_cut_late + late region in
   gbuffer_raster_late); sabotage AZRA_CLUSTER_HZB_NO_LATE=1 must pop.
7. MULTI_DRAW_INDIRECT_COUNT A/B (adapter-gated, fixed-cap fallback).
8. Watertight-on-rendered-cut: args readback vs cut_replay exact match; boundary edges of the
   replayed cut == source mesh boundary (reuse D3 gate fn).
9. Silhouette gate: analytic (τ=1px + Hausdorff bound) + visdiff at first-switch distance;
   sabotage τ=32 must fail visibly.
10. Default ON + **T2 gate run 8x9 ≈ 25.2M tris ≤16ms** + sabotage suite (passthrough, no-cone,
    no-late) + cap sizing from measured worst cut + docs.
