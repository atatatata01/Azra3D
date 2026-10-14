# Phase D3 — Cluster DAG bake, commit plan

**Date:** 2026-08-04 · **Branch:** `feat/viewport-perf` · Parent: `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md` §D3.
D1+D2 complete (T1 passed, HZB gate 91-92%).

## Decision: meshopt crate (go/no-go = commit 2 spike)
- `meshopt-rs` vendors meshoptimizer C, builds via the **cc crate only — NO cmake** (cc+MSVC
  proven working here via cuda_render). `build_meshlets` (64v/124t) + `compute_meshlet_bounds`
  (cones for G12) + `simplify` with `LockBorder` = G7's hard locks WITHOUT touching our
  decimator (which is NOT bit-reproducible: RandomState HashMaps at op_impls.rs:2037/2216/...,
  float BinaryHeap ties — a ~2.8k-line audit avoided). meshopt C is deterministic per call.
- Fallback if the spike build fails: greedy meshlets over display_mesh_ops/adjacency.rs +
  additive `locked: Option<&[bool]>` on mesh_decimate_qem + determinism audit (own task).
- Bevy meshlet module STUDY-ONLY; METIS deferred to D4 (recursive bisection suffices for gates).

## Data model
`core_math/src/cluster_dag.rs` (structs+validate, dep-free) — shared pools (positions/normals/
indices) + `Cluster { index range, bounds, cone, self_error, parent_error, group, child_group }`
+ `ClusterGroup` + `LevelRange` (level 0 = finest) + `source_hash`. Invariant: parent_error
monotone non-decreasing up the DAG (D4's cut = one per-cluster test). ~1M tris → ~8k L0
clusters → 11-13 levels. Builder + meshopt dep in **render/src/cluster_build.rs** (core_math
stays dep-free).

## Bake wiring (mirrors body_volume_cache)
`app/src/cluster_bake.rs`: ClusterBakeState in CacheState (mpsc pair, in_flight gen map,
500ms debounce + one-shot bypass, spawn-generation staleness drop), poll ≤1 result/frame.
Trigger: poly bodies > 50k tris on mesh-generation change. SDF DC meshes EXCLUDED until D8
(bake churn). Source: mesh_cache.chunk_mesh_data (pos/normal in [f32;8]) + pt_stamp content key.

## Disk cache (G8)
`data_local_dir()/Azra/cluster_cache/<key>.dagblob`; key = stable hash (xxh3/FNV — NEVER
DefaultHasher) of (positions, indices, params.hash_into, BUILDER_VERSION); `CLUSTER_DAG_V1`
magic + raw little-endian slabs (bincode too slow for <50ms @1M); atomic temp+rename; LRU
sweep by mtime, 2GB budget. Corrupt blob = miss (fail closed, rebuild).

## Gates
1. Bit-identical rebuild (serialize twice, byte-compare; re-run after hash-alloc storm).
2. Watertight seams: per level, group boundary edge sets equal + boundary vert positions
   bit-identical vs the level below (sabotage: locks off must FAIL).
3. Hausdorff: 10k deterministic samples per level vs source via BVH closest-point;
   tol(level) tied to reported self_error.
4. Perf: 1M tris < 5s cold (rayon per-group with PRE-ASSIGNED output ranges — push-ordered
   output breaks determinism, sabotage-tested), < 50ms cached.

## Commits
1. ClusterDag structs + validate + toy-DAG tests (core_math, dep-free).
2. **SPIKE (go/no-go)**: meshopt dep in render + meshlet/simplify smoke + determinism test.
3. L0 meshlet build (`build_level0`) — coverage/≤124/determinism tests.
4. Cluster grouping via recursive bisection (BTreeMap/sorted edges — no hash order).
5. Grouped simplify loop → DAG levels (LockBorder, serial first) + watertight gate + sabotage.
6. Hausdorff gate.
7. Disk cache (G8) + roundtrip/key/corrupt tests.
8. App bake wiring (trigger/debounce/poll, log-only consumption).
9. 1M perf harness + rayon parallel groups + parallel-vs-serial bit-identity sabotage.
10. Docs: gate rows in parent plan, spike evidence.

## GATE RESULTS — D3 COMPLETE 2026-08-04

| Gate | Result |
|---|---|
| Bit-identical rebuild | PASS (build twice, Debug-format equal; meshopt deterministic per call) |
| Watertight group seams | PASS every level + sabotage (unlocked simplify breaks seams) PASS |
| Hausdorff vs source | PASS per level, tol tied to reported self_error |
| 1M cold < 5s | **PASS**: 786k = 1.35s; 3.14M = 5.78s (~1.8s/M → 1M ≈ 1.8s). SERIAL — rayon not needed, deferred |
| 1M cached < 50ms | **PASS at pace**: 786k = 42ms; 3.14M = 153ms (~49ms/M, disk-bound) |
| Live wiring | dense body → spawn → 23-level DAG (root 206 tris) delivered + cached; 2nd run CACHE HIT |

**Two production bugs found by the live path (not by unit tests — the verify-app-path lesson):**
1. Per-group meshopt calls fed the FULL vertex pool (meshopt scales with vertex count) —
   786k build ran for minutes. Fix: compact each group submesh to local vertices (sorted-unique
   remap) → 1.35s.
2. Raster vertex copies are FLAT-SHADED (unwelded — exactly 32 tris per 64-vert meshlet) →
   every edge topologically open → LockBorder locked the whole mesh → zero coarsening
   (786440→786428). Fix: weld by position first (deterministic sorted-bits remap, averaged
   normals, degenerate tris dropped). D4 note: attribute-seam-aware splitting is a refinement;
   DAG pools carry welded geometry.

Trigger source note: graph payload `mesh_cache_id` stays None on the shared-seed path — the
STRUCTURAL DRAW LIST is the authoritative display-cid source (also what D4 consumes).

## Perf watch (regression checks between commits)
Baseline scene-v3 orbit: med 3.4-4.0ms, gpu 0.67-0.68, draws 8. After commits 3-4: gpu/rast
UNCHANGED (0.68/0.8) but med crept to ~6ms — breakdown shows `lay` (egui layout) 2.5→4.2ms at
13.7k scene nodes, varies run-to-run with panel state. NOT caused by D3 (bake code not in the
frame path); pre-existing UI scaling cost — likely the outliner walking 13.7k rows. Tracked as
a follow-up (row virtualization / per-mode providers land with the UI port).

## Risks
Blob speed (raw slabs from day 1); orchestration determinism (stable cluster indices
everywhere); render-crate build bloat (pivot to a tiny `cluster` crate at commit 2 if needed).
