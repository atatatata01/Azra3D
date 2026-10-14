# Phase D — Virtual Geometry (Nanite-class), Rust-native execution plan

**Date:** 2026-07-31 · **Branch:** `feat/ui-polish` (release trunk)
**Parent:** `AZRA_MASTER_ROADMAP.md` §Phase D — this file is its execution detail.
**Baseline (Phase C exit, measured):** idle 1 cube **1.8–3.0 ms** · 10k single-mesh **med 6.6 ms** ·
10k multi-mesh (~1.7 M tris) **med 6.5 ms**, `draws=5`, `disp≈2.0`, `structReb=0`, `instUp=0`.

**Goal in one sentence:** a viewport that renders **billions of virtualized triangles** like Unreal,
while staying **editable in place** like Blender never was at scale and Unreal never is at all —
plus the offline path tracer reading the same scene.

---

## 0. Corrections to the blocker list (verified 2026-07-31)

Two findings change the Phase D cost estimate — one removes a blocker, one was already fixed:

1. **The software rasterizer is NOT blocked on a wgpu upgrade.** The architecture study said "no
   64-bit atomics in this stack." True at *device-request* level, but verified in the registry
   source: **wgpu-types 22.0 ships `SHADER_INT64` (bit 55), `SHADER_INT64_ATOMIC_MIN_MAX` (bit 60),
   `SHADER_INT64_ATOMIC_ALL_OPS` (bit 61)** — `wgpu-types-22.0.0/src/lib.rs:887,929,937`. No egui/eframe
   chain upgrade needed. What remains is: request the feature at device creation, **probe adapter
   support** (RTX 40-class Vulkan/DX12 exposes buffer int64 atomics), and use a **`u64` storage
   buffer** for the visibility buffer — buffer atomics, not image atomics, so the rarer
   `image_atomic_int64` extension is never needed.
2. **The object-ID ceiling is already fixed** (C.5: `R32Float` motion target + dense `body_index_map`).

Still true and still blockers (see §2): the fp16-euclidean vs NDC depth duality, QEM boundary
locking, and the red `graphics` test suite.

---

## 1. Why AZRA can actually beat both — the honest competitive frame

| | Blender | Unreal | AZRA's position |
|---|---|---|---|
| 10k separate objects, full shading | struggles (CPU-bound per-object) | fine | **6.5 ms today** — already past Blender's weak point |
| Billions of tris | no | Nanite | Phase D target — same technique class |
| **Edit clustered geometry in place** | n/a (no clusters) | **cannot** — round-trip to a DCC | **Object/Edit drop-out + async recluster** — the differentiator |
| Offline PT of the same scene | Cycles (separate design) | Path Tracer | same `mesh_cache`, BLAS already instance-aware |

The moat is not "Nanite in Rust." It is **edit-while-virtualized**: Tab into a clustered body, it
drops to its live representation, you sculpt/model, Tab out, it re-clusters in the background. Unreal
structurally cannot do this; Blender has nothing to drop out *of*. Every phase below protects that.

---

## 2. What we missed — the gap list (answer to "did we miss something")

| # | Gap | Status | Owner action? |
|---|---|---|---|
| G1 | **Q1 Display-Invariant amendment unsigned.** §3 of the roadmap argues it; the checkbox is empty. No cluster-raster code may land before it. | OPEN | **YES — sign it** |
| G2 | **Q2 recluster spec** (who owns the job, debounce, pop policy, re-enter mid-recluster). | **WRITTEN** `PHASE_D_RECLUSTER_FLOW.md` (2026-07-31); owner product sign-off row still blank | owner sign-off |
| G3 | **`cargo test -p graphics` is RED** (5 stale `rt_shadow_bg_test` failures). This is the mandatory post-shader gate, and Phase D is shader-heavy. Fix before any D shader work — a red gate hides every new regression. | OPEN | no |
| G4 | **GPU Hi-Z does not exist** (C.3 was CPU, default-off). D's two-pass occlusion needs the pyramid anyway → build once as D2, **delete** the CPU path per the disposition rule. | OPEN | no |
| G5 | **Depth duality**: compositing depth is fp16 *euclidean* (`gbuf_normal.w`) while overlays/picking read the *NDC* attachment. The vis-buffer resolve must emit **both**. Data-format work, not rule work. | designed, not built | no |
| G6 | **Material resolve pass**: vis-buffer defers shading, so per-pixel material-ID → evaluate. AZRA's `RasterMaterial` array + per-face ranges must survive the indirection. | not designed | no |
| G7 | **QEM determinism + hard boundary locks** — `boundary_strength` is a soft cost multiplier; cluster-group seams need `locked_verts`/`locked_edges` (impossible, not expensive) + bit-reproducible output. Additive params on the existing 4k-line decimator. | not built | no |
| G8 | **Cluster cache persistence.** Nothing stores bake products on disk — every load would re-cluster. Content-addressed cache dir (hash of mesh+params → DAG blob), *never* inside `.fl4` (caches are not authored state). | not designed | no |
| G9 | **GPU time is not on the scoreboard** — was false-green: fields printed but shadow/gbuf always 0 (`begin_frame` after raster + contiguous-only resolve). **FIXED 2026-07-31**: begin_frame before hybrid raster, per-slot resolve, `gpu_ms=` on scoreboard. | **DONE** | no |
| G10 | **Dense stress asset.** `AZRA_DENSE_STRESS=NxL` (bodies×CC levels) spawns unique subdivided bodies; poly LOD forced off for measure. | **DONE** (tri-bound entry gate still OPEN — not dense enough yet) | no |
| G11 | **Streaming/residency design** (answer 1 made it mandatory): fixed geometry-pool budget, page-in by screen-space error, eviction. | not designed | no |
| G12 | **Per-cluster backface cone culling** — standard meshlet win (~30–50 % clusters culled free); was absent from all prior plans. | new | no |
| G13 | **Skinned/deforming geometry under clusters** — explicitly OUT OF SCOPE until the game phase; recorded so nobody designs for it accidentally. | deferred | no |

---

## 3. "Can we integrate the tech to fit Rust architecture better?" — yes, specifically:

1. **Clustering is a bake.** AZRA's whole architecture is bakes (`BodyVolumeCache`, debounce,
   content-keys, async readback, generation counters). The cluster DAG builder is *another consumer
   of `mesh_cache`* on the background thread — no new architectural species. This is a better fit
   than UE's, where Nanite build is an asset-import step divorced from editing.
2. **Determinism is cheap in Rust and load-bearing here.** Cluster-group seams demand both sides
   simplify identically. House rules already ban HashMap-order dependence; add fixed seeds + stable
   sorts and the DAG build becomes bit-reproducible — which also makes G8's cache trivially sound.
3. **Use the crates, study the references.**
   * `meshopt` (Rust bindings to meshoptimizer, MIT) — meshlet building + simplification support.
     Evaluate in D3 spike; wrapping proven C is fine (kernel stays ours).
   * **Bevy's meshlet/virtual-geometry module — the single most relevant reference on earth for us**:
     Nanite-style clusters, two-pass hi-z, software raster, DAG build, **on the same wgpu API**.
     MIT/Apache. Treat as **STUDY-ONLY** (the `bmesh-rs` precedent): read it for wgpu-specific
     pitfalls, never take it as a dep.
   * METIS for graph partition if needed; else recursive bisection over our existing BVH — spike decides.
4. **No mesh shaders required.** wgpu 22 lacks them; the compute-cull → `multi_draw_indexed_indirect`
   path (which `SdfMeshPool` **already ships** — GPU-written indirect args, arena, generation
   counters) covers the whole plan. That's a portability win over UE's mesh-shader fast path.
5. **The structural draw stream (B.1/C.9-prep) is the CPU↔GPU contract.** Persistent instance +
   cluster buffers on the GPU, camera-only frames touch nothing — the Phase C cache discipline
   (`structReb=0`, `instUp=0`) extends unchanged.
6. **Vis-buffer as `u64` storage buffer** (`depth<<32 | vis_id`) with buffer atomics — §0. Fallback
   for adapters without int64 atomics: 32-bit packed `depth24|id8` two-pass. Feature-gated, fails
   closed to the hardware path.

---

## 4. Targets — "better than Blender and Unreal," as numbers not vibes

All at 1440p, release, median over 120 frames, GPU ms on the scoreboard (G9). Each target is a
scoreboard row when hit; blanks are admissions.

| T | Scene | Target | Beats | Phase |
|---|---|---|---|---|
| **T0** ✅ | 10k mixed objects, full shading+shadows | ≤ 8 ms (**6.5 measured**) | Blender object-mode | C (done) |
| **T1** | **100k** instanced objects, GPU-culled | ≤ 16 ms | Blender by ~100×; matches UE instancing | D1–D2 |
| **T2** | **25 M source tris** (5 sculpt-dense bodies), clustered, hardware raster | ≤ 16 ms | Blender outright | D3–D4 |
| **T3** | **250 M–1 B virtualized tris** (unique + instanced mix), streamed | ≤ 33 ms within a fixed geometry-pool budget | UE parity on its home turf | D5–D7 |
| **T4** | **Edit round-trip on a 1 M-tri clustered body**: Tab-in < 150 ms; async recluster ≤ 2 s with **zero frames > 33 ms** during | — | **Unreal cannot do this at all** | D8 |
| **T5** | PT final frame of the T3 scene, offline, cluster-LOD secondary rays | completes within VRAM budget | — | parallel |

**T4 is the flag.** T1–T3 reach parity; T4 is the thing neither competitor has.

---

## 5. Phases

Standing rules apply (build every change; publish a number per step; sabotage every gate; one item
per commit; append-only enums; caches never serialized).

### D0 — Prerequisites *(no cluster **raster** code before Q1 + tri-bound measure; pure math OK)*
1. **Q1 signed** — owner/date/commit in the roadmap table. **OPEN**
2. **Q2 spec written** — ☑ `PHASE_D_RECLUSTER_FLOW.md` (owner product sign-off still optional for D0 math).
3. **`graphics` suite GREEN** — fix the stale `rt_shadow_bg_test` bind group (test must mirror
   production wiring — the lab-cache lesson).
4. **Int64 probe**: request `SHADER_INT64(_ATOMIC_MIN_MAX)` when the adapter has it; log + store
   capability; fail closed to hardware-raster-only. (Only hard for D5+; not for D1–D4 hardware path.)
5. **GPU timestamps → scoreboard** (G9).
6. **Dense stress asset** (G10): `AZRA_DENSE_STRESS=N` spawning ~1 M-tri subdivided bodies — also closes
   the roadmap “tri-bound” entry gate if measurement shows GPU tri work dominates.

### D1 — GPU-driven instance culling *(was C.9; now it has a reason)*
Persistent instance buffer; compute pass does frustum (+ later hi-z) cull → compacts visible
indices → `multi_draw_indexed_indirect`. CPU encodes ≤ N_meshes draws regardless of count.
**Gate:** T1 — 100k instances ≤ 16 ms; camera-only frame uploads nothing. Sabotage: disable compute
cull, watch draws explode.

### D2 — GPU Hi-Z pyramid + two-pass occlusion
Build mip pyramid from prev-frame depth (it already exists every frame); cull instances (D1) and
later clusters (D4) against it; two-pass pattern for disocclusion correctness. **Delete the CPU
AABB occluder in the same commit** (replace-rule). **Gate:** interior scene culls ≥ 90 % of hidden
instances; a 360° orbit shows zero popped-in-late frames (temporal check, not a screenshot).

**D2 COMPLETE 2026-08-04** (`PHASE_D2_HZB_PLAN.md`, commits 281918f…a782e41): full-res R32Float
max-pyramid (`render/src/hzb.rs` — the 64×36 experiment was deleted in d3a7cd9 after its 0% A/B),
two-pass cull (early vs prev-frame HZB → G-buffer A → rebuild → late retest → G-buffer B
LoadOp::Load → rebuild #2), default ON. **Gate: interior scene 91–92% of in-frustum instances
culled (≥90% PASS); `AZRA_HZB_NO_LATE=1` sabotage drops the late recoveries (pop-in) — late pass
proven load-bearing.** Scene-v3 orbit: gbuf GPU 0.93→0.30 ms. CPU `OcclusionBuffer` retained
ONLY for the `AZRA_NO_GPU_CULL` fallback (deletion = owner-signed follow-up — the "delete in the
same commit" clause above conflicted with the Feature Dependency Audit). Cascades frustum-only.

### D3 — Cluster DAG bake — **COMPLETE 2026-08-04** (`PHASE_D3_CLUSTER_DAG_PLAN.md`)
All gates PASS: bit-identical rebuild; watertight seams (+ sabotage); Hausdorff per level;
1M cold ≈1.8s (<5s, serial — rayon deferred as unneeded); 1M cached ≈50ms. meshopt (cc-only
build) chosen over the house decimator (not bit-reproducible). Bake wired app-side
(cluster_bake.rs, body_volume_cache shape; trigger = structural draw list, ≥50k tris,
SDF excluded until D8; cache %LOCALAPPDATA%/Azra/cluster_cache). Two live-path bugs fixed:
per-group full-pool feeding (minutes→1.3s) and flat-shaded unwelded raster copies (zero
coarsening → weld by position). D4 consumes `CacheState.cluster_bake.dags`.
`mesh_cache` triangles → ~128-tri meshlets (meshopt spike) → grouped QEM simplify with **hard locks
+ determinism** (G7) → DAG levels → content-addressed disk cache (G8). Background thread, debounced,
never the frame thread. **Gates:** bit-identical rebuild (determinism); watertight seams between
adjacent groups at every level (the NURBS watertight-gate idea, reused); Hausdorff level-vs-source
< tol (the OCCT-oracle pattern, self-referenced); 1 M tris < 5 s cold, < 50 ms from cache.

### D4 — GPU cluster selection + hardware raster
Persistent DAG buffers; per-frame compute cut by screen-space error + hi-z + **backface cones**
(G12); hardware raster via the D1 indirect path. This is `SdfMeshPool`'s architecture generalized —
finer tiles, LOD chain, per-cluster GPU visibility. **Gate:** T2 — 25 M source tris ≤ 16 ms;
silhouette error < 1 px at switch; no cracks (watertight gate re-run on the *rendered* cut).

### D5 — Visibility buffer + material resolve
`u64` storage-buffer vis-buffer (or 32-bit fallback), full-opaque-pass replacement per the amended
invariant; resolve pass emits **both** depth conventions (G5) + per-pixel material eval (G6).
**Picking becomes free** — object/cluster ID is already per-pixel. **Gate:** every overlay,
selection mode, and gizmo works unchanged (P0 — selection must always work); A/B image diff vs the
hardware path < tol on the regression scenes.

### D6 — Software raster for sub-pixel triangles *(flagged, evidence-gated)*
Only if D4 measurement shows ≥ ~25 % of shaded clusters at < 1 px triangle size. Compute rasterizer
writing the D5 vis-buffer via atomic min — never a second depth convention. **Gate:** GPU-ms win on
the dense-scan scene, A/B, flag default-off until proven.

### D7 — Streaming + residency
Cluster pages in a fixed geometry pool (budget from settings); priority = screen-space error;
async page-in; eviction LRU. **Gate:** T3 — the 1 B-tri scene inside the budget with no frame
> 33 ms and no page-in hitch > 5 ms.

### D8 — Edit round-trip *(the differentiator — do not let it slip)*
Per Q2 spec: Tab-in drops the body from the cluster set to its live representation (existing raster
path — per-object technique choice, already legal); Tab-out re-clusters **only dirty groups**
(dirty-region recluster, not whole-body). **Gate:** T4. Sabotage: whole-body recluster on a 1 M-tri
body must *fail* the zero-hitch clause — proving the incremental path is load-bearing.

---

## 6. Sequencing note

D1+D2 are pure wins **even if clustering slipped a quarter** — they carry T1 alone and nothing in
them is cluster-specific. D3 can start in parallel with D1/D2 (different subsystem, background
bake). D5 is the invariant-sensitive step and lands only after Q1 is signed and D4 is measured.
The PT track stays untouched throughout; it picks up cluster LODs after D3 exists.
