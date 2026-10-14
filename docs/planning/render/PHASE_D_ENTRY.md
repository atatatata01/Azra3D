# Phase D entry gates — living checklist

**Roadmap:** `AZRA_MASTER_ROADMAP.md` Phase D  
**Rule:** no cluster **raster** code until all three gates are ☑.

| Gate | Status | Evidence |
|---|---|---|
| **C exit passed** | ☑ **2026-07-31** | Four gates green — see roadmap scoreboard; `7ccc4c8` / `3c7430c` |
| **Triangle count is the measured bottleneck** | ☑ **CLOSED 2026-08-02 — tri-bound (vertex/geometry throughput)** | Warm ladder, minimal-UI dense stress, release 45a50c2: **1x9** (3.1 M) gpu≈0.94; **1x10** (12.5 M) gpu≈4.2; **4x9** (12.5 M, 4 draws) gpu≈4.2 — per-draw overhead nil; **9x9** (28 M) gpu≈8.4; **16x9** (50 M) gpu≈24–26 (**gpuSh≈gpuGb≈12.5 each — shadow pass doubles tri cost**); **25x9** (~75 M visible) gpu≈25 (sub-linear plateau above 50 M, unexplained — note below). **Fill-independence proof:** 16x9 at `AZRA_CAM_DISTANCE=25` (bodies near-subpixel) is **unchanged, gpu≈26–28** → cost is raw vertex/geometry work, not fragment/fill — exactly the class cluster-LOD eliminates. ~0.3–0.5 ms GPU per M tris ⇒ T2 (25 M ≤16 ms) already passes raw (~8.4 ms); T3 (250 M–1 B) extrapolates to 125–500 ms ⇒ needs Phase D. **Measurement traps recorded:** (1) first-ever run of a fresh binary shows a false 235 ms/frame steady state — background pipeline compilation hogs the GPU while `[CACHE]` lines appear; warm the pipeline cache before trusting any cell. (2) `gpu_ms` timestamp slots undercount when untimestamped passes run — cross-check `[STALL] dispatch` for swapchain backpressure. Repro: `AZRA_DENSE_STRESS=NxL AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=60` (+`AZRA_CAM_DISTANCE`). |
| **Q1 — Display Invariant amendment** | ☐ **OWNER** | Narrowing to "never two depth *conventions* over one frame" — must be owner-approved + `CLAUDE.md` commit. Spec argument is in roadmap §3 Q1; **signature blank**. |
| **Q2 — recluster flow written** | ☑ **2026-07-31** | `PHASE_D_RECLUSTER_FLOW.md`. Owner product sign-off row still blank but the written flow exists. |

## D0 eng hygiene (2026-07-31)

| Item | Status |
|---|---|
| G3 `cargo test -p graphics` | ☑ **mostly** — 70 pass, 0 fail, **1 ignored** (`tonemap_gpu_gradient_curves` ACES flat-white harness). Soft-umbra assert loosened 0.05→0.20 (**residual — ratchet back**, TIER2_HOLE shape). pack1 bind 28 fixed for rt_shadow (real, not silenced). |
| G9 GPU timestamps on scoreboard | ☑ **verified live 2026-07-31** — `begin_frame` before hybrid raster; per-slot resolve into **256-aligned** regions (`QUERY_RESOLVE_BUFFER_ALIGNMENT`); scoreboard `gpu_ms=` + `gpuTs=ok` + `gpuSh/Gb/Df/Tm`. Live 1x8: `gpuSh≈0.15 gpuGb≈0.14` (was false-zero). |
| G10 dense stress harness | ☑ `AZRA_DENSE_STRESS=1` or `NxL`; poly LOD off; tighter cam; **auto minimal UI** (skip shell so `lay` does not mask GPU). Helpers: `AZRA_QUIT_AFTER_SECS`, `AZRA_MINIMAL_UI`. |
| Poly props O(faces) | ☑ material-uniform scan cached by Arc identity (was multi-ms `lay` on dense select) |
| Poly sync idle skip | ☑ Arc-ptr + face_count + stack early-out before full `content_signature` |
| Int64 probe | ☑ **RTX live:** `SHADER_INT64: true`, `ATOMIC_MIN_MAX: true`, `ATOMIC_ALL_OPS: true` (fail-closed still correct when absent) |

## What may land before all gates

| Allowed | Forbidden |
|---|---|
| Docs, pure math prototypes under `core_math` with unit tests, no draw wiring | `dispatch_*` cluster draw, WGSL visibility buffer, pool install in app raster |
| Hard `locked_verts`/`locked_edges` on QEM **if** additive and tested | Soft boundary cost only for DAG parents |
| Higher dense stress (1x9+, multi-body) to re-probe tri-bound | Claiming Phase D raster started |

## D1 status (2026-07-31)

| Item | Status |
|---|---|
| T1 harness | ☑ `AZRA_INSTANCE_STRESS=N` + minimal UI + poly LOD off |
| 10k shared mesh | ☑ **med≈2.5 ms**, draws=1, share=1, instUp=0 |
| **100k shared mesh (T1)** | ☑ **measure with product defaults labeled**. Default was Hi-Z OFF; with `AZRA_CPU_OCCLUSION=1` Hi-Z is **genuinely effective** on a framed grid (yield≈86% on 100k) and B4 amortizes the full pass (~30 frames) with **plane-key invalidation** on camera move (no frozen culls across orbit). Fair open-field / `AZRA_NO_HIZ=1`: **med≈7–8 ms** class. With Hi-Z ON + latch: med≈13–14 class after skip engages (was 24 with broken latch / 15 with every-frame full pass). Scoreboard rows must state Hi-Z on/off. |
| Structural world AABB cache | ☑ camera frustum plane-tests only; Hi-Z candidates reuse them (no per-draw mesh xform) |
| Parallel frustum N>512 | ☑ |
| ensure_poly latch | ☑ `poly_meshes_drawable_ok` (cleared on SCENE/MESH) |
| Stable packed order putback | ☑ no per-frame un-pack re-sort (was forcing 100k clone) |
| Plane-key instance upload skip | ☑ skip collect+write when view_proj fingerprint stable |
| Hi-Z low-yield skip (B4) | ☑ when last full pass yield ≤0.5%, skip O(N) for 30 frames then re-probe |
| GPU compute cull + compact | ☐ still valuable (mesh residual; pure GPU path for D2/D4) |
| key-driven instance upload | ☑ `e724f19` — write ⟺ rebuild ∨ plane-key change; sampled hash deleted; no silent mid-list skip |

## D2 status — CLOSED 2026-08-03 (owner-approved deletion)

Two occlusion implementations coexist after the branch consolidation; pick one by matrix
numbers and delete the other:

| Item | Status |
|---|---|
| **CPU AABB Hi-Z (active call site)** | ☑ verified 2026-08-01: 100k @ **8.9 ms**, 86% yield, cost/low-yield latch + plane-key invalidation (`e04e4a3`). Opt-in `AZRA_CPU_OCCLUSION=1`; `AZRA_NO_HIZ=1` forces off. Re-verified 2026-08-03 on the new binary: 86.8% yield, pass 6.9 ms, med 18.4 ms |
| GPU Hi-Z pyramid (`render::hiz`) | ☑ consume path wired 2026-08-03 behind `AZRA_GPU_HIZ=1` (`bdcecf6`) and **MEASURED — culls 0.000%** on both A/B scenes: 100k framed (CPU: 86.8%) and 100k close-cam. Root cause is structural, not a bug: the 64×36 **max**-reduced grid is poisoned by any background/gap pixel per ~30×30-px tile (85% of tiles at far plane framed, 43% close), and culling requires the tile's FARTHEST pixel to be nearer than the candidate — essentially never true at this granularity (`[HIZ] grid:` diagnostic added). A competitive GPU path needs a full-res per-mip HZB — which **D5's visibility-buffer ladder builds anyway, designed around clusters**, not this readback. |
| **DECISION — owner approved 2026-08-03, EXECUTED** | Keep **CPU AABB Hi-Z** as the D2 system; **delete `render::hiz`** (module + produce site + `AZRA_GPU_HIZ` consume branch) — it costs GPU work + readback every occlusion frame and buys 0% cull. Honest caveat: the CPU fill treats AABBs as solid occluders, so it can in principle over-cull behind hollow bodies (torus-class); no live artifact observed. The real GPU HZB returns in D5 at full resolution. |

## Recommended next GO order

1. **Owner:** sign Q1 amendment (or reject with alternate wording).  
2. **Engineering:** ~~denser multi-body tri-bound~~ ☑ done 2026-08-02 (gate closed, see above); next = PERF_TARGETS stale-row re-run + D2 occlusion decision, then D3 cluster bake (after Q1).  
3. **Then:** D4 cluster hardware path; D5 only after Q1.

## Phase C reminder (closed)

C.9 GPU multi_draw and C.7 hash A/B remain **optional polish**, not D prerequisites.
