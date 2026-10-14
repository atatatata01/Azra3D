# AZRA — Master Roadmap: fastest viewport → billion triangles → Nanite → runtime

**Date:** 2026-07-30
**Status:** strategic plan. Owner-answered on six scoping questions (§0). Backed by a 24-agent
architecture study (48 high-confidence findings, adversarially verified).
**Supersedes:** the P5 section of `PLAN_FOR_GROK_VIEWPORT_PERF.md` and the conclusion of
`BILLION_TRIANGLE_ARCHITECTURE.md` — both written for a DCC-only product, before the game/film scope.

---

## 0. What AZRA is — owner-decided 2026-07-30

> "Right now we are a modeling software but at some point Nanite could be used to create my own game."
> "Imagine an Unreal where you can easily model inside."
> "But also we still have the full path tracer for image production or high end video production."
> "So it will not be just a game engine."

**One scene, three consumers.** Six scoping answers define the build:

| # | Question | Answer | Consequence |
|---|---|---|---|
| 1 | Billion triangles: instanced or unique? | **Both, seriously** | Phase D + streaming are **mandatory**, not optional |
| 2 | Platform? | **Windows/Linux desktop** | Full Nanite path is possible — no Metal tax |
| 3 | PT at city scale? | **Offline final frame** (minutes/hours OK) | Out-of-core BLAS; tractable, reuses cluster LODs |
| 4 | Terrain? | **SDF + EXR displacement, heightfield for local detail** | Not a new pillar — see §5 |
| 5 | Editor scale? | **Editor authors, runtime renders big** | **No scene-graph rewrite. No Phase 0.** |
| 6 | PT interactive at scale? | **No — offline** | PT stays a parallel track, not a roadmap driver |

**Answer 5 is the single biggest simplification.** The worst case — interactively editing 100k+ nodes —
would have forced a delta/COW undo model and a scene-graph rewrite before any rendering work. It is
off the table. What remains still has to be fixed (§2), but it is repair, not replacement.

---

## 0b. Execution status vs plan — RECONCILED 2026-07-30

**Read this before starting anything.** The architecture study in §2 read a snapshot; Grok landed
Phase-C-shaped work in the working tree around the same time. Verified against the working tree today:

| Item | Plan phase | Honest status | Evidence |
|---|---|---|---|
| `[CTRLX]` gating, pick-invalidate, mark-then-drain, perf HUD, `rest(top)` instrumentation | A | **DONE** | `60462cc`+ |
| **§2a fixes A1–A6** | **A** | **DONE + EXIT GATE PASSED** — idle frame ≈1.8–3.0 ms (see scoreboard) | `d419368`… + always-on `record_frame` |
| Poly LOD (display LOD, `poly_lod_cache`, edit forces full) | C.4 | **IN TREE, NOT MEASURED** | `app/src/poly/lod.rs` |
| Occlusion culling | C.3 | **IN TREE, DEFAULT OFF** — CPU AABB Hi-Z (64×36). Enable with `AZRA_CPU_OCCLUSION=1` / UI. **Not** GPU last-frame-depth Hi-Z yet | `occlusion_cull` |
| Shadow-aware culling | C.2 | **DONE (v1)** — frustum-out skips main+shadows; occlusion = main only | `DRAW_CULL_FRUSTUM` / `DRAW_CULL_OCCLUDED` |
| Instanced draws | C.1 / B.4 | **DONE + MEASURED** — 10k single med≈6.6 ms, draws=1, share=1 | `AZRA_INSTANCE_STRESS=10000` |
| Multi-mesh batch gate | **C.6** | **DONE** — pure sort + sabotage; 5×2000 draws=5 | `count_mesh_instance_draws` / `AZRA_MULTI_MESH_STRESS` |
| Skip instance/mat re-upload | **C.7** | **DONE** — content-hash skip; scoreboard `instUp`/`matUp` | `upload_instances` / `upload_materials` |
| Kind flags not HashMap | **C.8** | **DONE** — `draw_kind` parallel Vec; O(1) special-draw tests | `DRAW_KIND_*` |
| Outliner virtualization | **C.10** | **DONE (v1)** — leaf clip-cull + ui2 row cache/window; lay 100→10k soft-flat | `outliner_draw_poly` / `ui2/outliner` |
| Submit packing after cull | **C.11** | **DONE** — visible-first pack on working list; draws=1 @ 10k single | `raster_gbuffer` post-cull sort |
| Overlay fingerprint tax | **C-exit** | **DONE** — no `content_signature`×N when wire idle | `overlay_lines.rs` / `3c7430c` |
| Uniform `mesh_cache_id` on duplicate | **B.4** | **DONE + fixed** — share on Shift+D; COW only on content divergence | mesh_sync |
| Structural draw stream | **B.1** | **DONE (v1)** — NodeId list rebuild on SCENE/MESH; camera reuses | `draw_stream.rs` |
| Structural draw **list** | **B.1 ext / C.9 prep** | **DONE (v1)** — transform cache + per-frame cull mask | `raster_gbuffer.rs` |
| PT gather uses draw stream | **B.2** | **DONE (v1)** | `pt_scene.rs` |
| Scene BVH frustum | **B.3** | **DONE (v1)** — mid-band only (8..512) | `raster_common` |
| Object-ID ceiling (2048) | **C.5** | **DONE** — `gbuf_motion` R32Float | `d264bcc` |
| GPU-driven multi_draw | **C.9** | **NOT STARTED** — optional; C exit no longer blocked | — |

**Status vocabulary:** *IN TREE, NOT MEASURED* = the code exists and runs; **no before/after number
exists yet**, so it is not a "win." *CODE DONE* = landed; **exit gate** is the measured number.

**⚠ Line numbers in this document are pointers as of 2026-07-30, not scripture.** They drift within a
few lines as the tree moves. Navigate by symbol and behaviour; if a cited line no longer matches, the
finding is probably still true a few lines away — re-grep before concluding it was wrong.

### Three things this reconcile makes explicit

1. **Phase C work shipped before the Phase A exit gate.** This plan says *"no rendering win is
   measurable through a 25 ms CPU floor."* That was not followed. The code is useful infrastructure —
   but **its before/after numbers cannot be trusted yet**, and Phase C's exit gate (10k instances
   interactive) is still blocked by A, not by missing LOD code.

2. **⚠ Unnamed risk: the new work ADDS per-frame CPU cost to a frame already 2× over budget.**
   A CPU AABB Hi-Z rasterizes an occluder per object, per frame, on the CPU — in the same
   `poll`/gather phase that is the measured problem. Poly LOD adds a per-body screen-size evaluation.
   **Both must be measured against the pre-existing idle floor before they are called wins**, and it is
   entirely possible one of them made the floor worse. Measure first; do not assume direction.

3. **Draw-time instance batching is NOT Phase B.4.** Sharing by content key at draw time does not
   satisfy B's exit gate (*"N duplicated cubes upload one mesh"*) — the meshes are still uploaded
   separately. It is a **C.1 preview**, and it can fight a proper COW/authoring-time share design if
   anyone records it as B.4 complete. It is recorded here as preview so nobody does.

### Scoreboard — fill on every GO, keep it living

| date | scene | frame (release unless noted) | draws | inst+ | share | stream | notes / commit |
|---|---|---|---|---|---|---|---|
| 2026-07-30 | idle 1 cube (debug) | **1.8–3.0 ms** | — | — | — | — | A exit; poll ≈0.07 |
| 2026-07-30 | 100× shared poly | **4.4–4.9 ms** | 1 | 99 | 1 | reb=0 N=100 | B exit / single-mesh best |
| 2026-07-30 | 10k shared poly | **~45–52 ms** ⚠ | ~1 | ~9k | 1 | reb=0 N=10k | single-mesh best; ~120k tris; `803a43e` era |
| 2026-07-30 | 10k shared poly | **~39–41 ms** | 1 | 9999 | 1 | reb=0 N=10k | after rigid N + BVH band + ensure cid (`bbbab92`) |
| 2026-07-30 | **5 meshes × 200, NO sort** | **~7–12 ms** | **819** | 181 | 5 | reb=0 N=1000 | **C.6 fail shape** `AZRA_NO_DRAW_SORT=1` |
| 2026-07-30 | **5 meshes × 200, WITH sort** | **~7–12 ms** | **5** | **964** | 5 | reb=0 N=1000 | **C.6 pass** production sort |
| 2026-07-30 | camera-only (after structural) | (see 10k / multi) | — | — | — | reb=0 | C.7: `instUp=0 matUp=0` observed on multi-mesh idle cam |
| 2026-07-30 | **5×200 after struct list** | **med≈12.9 ms** | 253† | 160 | 5 | streamReb=0 structReb=0 N=1000 | †frustum holes split batches (no re-sort after cull); `instUp=0` |
| 2026-07-30 | **5×2000 multi-mesh 10k** | **med≈123 ms** | 2165† | 1053 | 5 | stream/struct reb=0 N=10k | pre-pack fix; layout dominated |
| 2026-07-30 | **10k single-mesh + struct list** | **med≈53 ms** | 2107† | 1188 | 1 | stream/struct reb=0 N=10k | pre-outliner fix; **lay≈43 ms** |
| 2026-07-30 | **5×200 + submit pack** | **med≈8 ms** | **5** | 408 | 5 | structReb=0 N=1000 | packing restored after cull |
| 2026-07-30 | **10k single + outliner cull** | **med≈10.2 ms** | **1** | 7849 | 1 | structReb=0 N=10k | **v1 leaf clip-cull**; lay≈3 (was 43) |
| 2026-07-30 | **10k single + ui2 cache** | **med≈10.5 ms** | **1** | 9191 | 1 | structReb=0 N=10k | `AZRA_UI=v2`; lay≈0.3; ol_build cached |
| 2026-07-30 | **5×2000 multi 10k WITH sort** | **med≈36 ms** | **5** | 3213 | 5 | stream/struct reb=0 N=10k | pre-overlay fix; **disp≈32** was overlay fingerprint |
| 2026-07-30 | **5×2000 multi 10k NO sort** | **med≈41.5 ms** | **~7.6k** | ~1.8k | 5 | same | sabotage: draws explode; +~5 ms vs sorted |
| 2026-07-30 | **5×2000 multi 10k ui2** | **med≈35.7 ms** | **5** | 6–10k | 5 | same | `AZRA_UI=v2`; lay≈0.3; same disp floor |
| 2026-07-31 | **disp split** | — | — | — | — | — | rast≈1.5, dRest≈31 — not raster; overlay fingerprint |
| 2026-07-31 | **5×2000 after overlay fix** | **med≈6.5 ms** | **5** | 3213 | 5 | reb=0 N=10k | **disp≈2.0** rast≈1.6 dRest≈0.4; C exit multi PASS |
| 2026-07-31 | **10k single after overlay fix** | **med≈6.6 ms** | **1** | 3294 | 1 | reb=0 N=10k | disp≈1.9; was 10 ms after outliner only |
| 2026-07-31 | **C.10 lay N=100** | med≈0.9 ms | 1 | — | 1 | N=100 | **lay≈0.4** |
| 2026-07-31 | **C.10 lay N=1k** | med≈1.3 ms | 1 | — | 1 | N=1k | **lay≈0.7** |
| 2026-07-31 | **C.10 lay N=10k** | med≈6.2 ms | 1 | 3294 | 1 | N=10k | **lay≈2.0–3.5** (was 43; soft growth, not linear) |

> **C.6 is the load-bearing row.** Single-mesh 10k always collapses to one draw even without sort.
> Multi-mesh without sort → **819 draws**; with sort → **5 draws** (= N meshes). Sabotage:
> `AZRA_NO_DRAW_SORT=1`. Pure unit gate: `count_mesh_instance_draws` (5 vs 1000 on interleaved pattern).
>
> 10k single-mesh remains CPU-gather bound (~120k tris), not geometry — Nanite would not help that case.
>
> **C.7 caveat:** content-hash over the instance array is a full CPU pass; net win vs `write_buffer`+PCIe
> is **unmeasured** (hypothesis). `instUp=0` on idle camera is observed; hash cost vs skip not A/B'd.

**Measured (AZRA_SCOREBOARD=1, `scoreboard.log`, steady after cold start):**
```
[SCOREBOARD] frame=1.78ms poll=0.07ms top=dpoll 0.02 | disp=0.5 ov=0.5 lay=0.3
[SCOREBOARD] frame=1.89ms poll=0.07ms top=dpoll 0.03 | disp=0.5 ov=0.5 lay=0.3
[SCOREBOARD] frame=1.83ms poll=0.06ms top=dpoll 0.02 | disp=0.5 ov=0.5 lay=0.3
[SCOREBOARD] frame=3.05ms poll=0.07ms top=dpoll 0.02 | disp=1.5 ov=0.7 lay=0.3
cold-start only: poll=17.6ms top=ai 15.9 (one-shot matcap/AI preload)
```

### Phase A exit gate — **PASSED (idle frame ≪ 12 ms)**

Steady idle **frame 1.8–3.0 ms** vs budget 12 ms and vs baseline poll 19–27 ms. The
historical `poll_background_tasks` floor is gone; remaining cost is GPU dispatch + overlays
+ layout at &lt;2 ms each. Next GO: **re-measure C** (poly LOD / occ / inst) under load, then
Phase B — not more A freebies unless a one-cube scene regresses.

**How to fill a row:** launch with `AZRA_SCOREBOARD=1`, one cube, camera idle 5s, copy a
`[SCOREBOARD]` line (poll / top / rest). Frame ms is also on the Perf HUD.

**Rule 4 says publish a number for every step. This table is where they go.** A row with blanks is an
admission, which is fine; a missing row is not.

**The FIRST action of the next GO is to fill row 2 with a live idle capture — not to add a feature.**
Row 1's blanks are the reason nothing in §0b can be called a win yet.

### Disposition rule for the C work already in tree

When A exits and the C work is re-measured, each item gets one of three verdicts — **decided by the
number, not by sunk cost**:

| verdict | when | action |
|---|---|---|
| **keep** | measurably pays on idle *and* on a populated scene | leave on, record the number |
| **flag off** | costs more than it saves at small/idle scale | **DONE default OFF** (`occlusion_cull: false`; `AZRA_CPU_OCCLUSION=1` to measure) — keep OFF until GPU last-frame-depth Hi-Z replaces it |
| **replace** | superseded by the Phase-C form (GPU last-frame-depth Hi-Z) | delete the interim path in the same commit that lands the replacement |

Until A exits: **do not expand CPU occlusion resolution, and do not add poly LOD levels.**

---

## 1. The through-line: the mesh hub already exists

Verified in code, not assumed. Every representation converges on **`mesh_cache`**:

```
SDF body    ──GPU bake──▶ DC mesh ──┐
Voxel body  ──Surface Nets────────▶ ├──▶ mesh_cache ──┬──▶ raster G-buffer (viewport)
Poly body   ──to_raster_data──────▶ ┘                 └──▶ PT BLAS (path tracer)
```

`pt_scene.rs:271` names `mesh_cache` in-comment as **"PT's BLAS source"**; the raster path draws from
the same cache. The hub is load-bearing today and already serves two of three consumers. Clusters
become the **third reader**, not a parallel stack.

The study confirmed the Display Invariant is **enforced structurally, not by convention** — the three
pillars genuinely coexist as triangles in one render pass. That is a real asset.

---

## 2. What the study found — the actual ceilings

48 findings, all high confidence. These are the load-bearing ones. **Note how few are about triangles.**

### 2a. The CPU floor is structural, and this is almost certainly `rest(top)`

| Finding | Evidence |
|---|---|
| **8–12 FULL scene-graph walks per frame**, several near-worthless | `app_poll.rs:394` — `sdf_bodies().count()` for `perf.body_count`, **ungated** (cf. `app_update.rs:735`, which *is* gated). **Precision:** `sdf_bodies()` is `self.nodes.iter().filter(…)` (`scene_graph.rs:1083-1088`), so `.count()` traverses **every node** — the iteration is full, only the *yield* is filtered. `viewport_render_mode_auto.rs:21` computes `has_raster` via a full `all_nodes().any(...)` that cannot short-circuit in an SDF-only scene. **Correction:** its result is not fully dead — it feeds a toast after forcing Hybrid — so **cache it or gate it on scene-dirty; do not blind-delete.** |
| **`world_transform` recomputed leaf→root on every call**, trig per ancestor, nothing cached | `scene_graph.rs:1055-1071`; 3× `to_radians` + `Quat::from_euler` (6 sin/cos) **per ancestor level**. `overlay_lines.rs:180` calls it once per visible node per frame *just to build a change-detection fingerprint*. |
| **Draw-list build is O(N × S × D)** | `raster_gbuffer.rs:73-76` — for every node, every frame: linear scan of the selection set × `is_descendant_of` root-walk. **Select-All makes the draw list O(N²·D).** |
| **String format + HashMap change-detection per SdfBody per frame** | `raster_gbuffer.rs:103-110` — **precisely:** the `eprintln` *is* gated by `perf_spam_on()`; what is **not** gated is the work feeding it — `format!("{nid:?}")`, `.to_string()`, and the `HashMap<String,_>` lookup/insert run every frame regardless. Two allocations + a hash per SdfBody per frame for a diagnostic nobody is reading. |
| **A scene BVH is built and uploaded every frame and read by nothing** | `build_scene_bvh` → `viewport_render.rs:596-602` → bound at binding 5. The only WGSL reader, `eval_body_bvh`, exists solely as a definition — leftover of the deleted analytical march. |

**Nothing here is a triangle problem.** This is the mechanism behind the 19–27 ms idle frame, and it is
an O(N) curve — it gets worse with scene size before geometry ever matters.

### 2b. Assets that already exist (the good news, and it is substantial)

| Asset | Where | Gap |
|---|---|---|
| **A GPU-driven cluster arena with GPU-written indirect draw args** — the single hardest Nanite building block | `sdf_gpu_mesh/pool.rs`, `sdf_mesh.wgsl:607-621` (`dc_tile_args` writes `index_count`/`first_index`/`base_vertex` from the GPU), `raster_common.rs:291` (`multi_draw_indexed_indirect`) | SDF-only; tiles ~2.8k tris (≈20× too coarse vs Nanite's 128); **no per-tile LOD chain**; visibility decided per-*body* on the CPU |
| **QEM edge-collapse simplifier**, 5 modes, crease locking, hashed cache | `display_mesh_ops.rs` (4041 lines), `mesh_presentation.rs:630-700` | Boundary protection is a **soft cost multiplier** (`cost *= boundary_mult`) — a bias, never a veto. **That is exactly what produces cracks in a cluster DAG.** Needs hard `locked_verts`/`locked_edges` + determinism. Additive params, not a rewrite. |
| **Instance-aware scene assembly**: geometry dedup by `MeshCacheId`, TLAS over placements, **refit fast path** for transform-only change | `pt_accel.rs:374-455` (`assemble_impl`) | Used **only** by the path tracer. The raster path shares none of it. **This is the working model of the instance table the raster path lacks.** |
| **A scene AABB tree over top-level bodies** | `build_scene_bvh`, tested at `bvh.rs:1153-1198` | Already built every frame; wired to a dead consumer. Repointing it at the raster gather turns the O(N) frustum scan into O(log N + visible) — a **free** win. |

**AZRA is much closer to GPU-driven rendering than a from-scratch project.** The arena, GPU-authored
draw args, per-cluster regrow, and generation-counter invalidation are built, tested, and in production.

### 2c. Real blockers for the software rasteriser (Phase D only)

| Blocker | Detail |
|---|---|
| **No 64-bit atomics** | `gpu_init.rs:2650-2690` requests no `SHADER_INT64*`. Every atomic in every `.wgsl` is 32-bit. Nanite's software raster is `atomicMax` on a 64-bit `(depth<<32)\|payload`. |
| **Object-ID channel saturates at 2048** | ~~`body_index` → `gbuf_motion.r` was `Rgba16Float` (f16 exact ints only to 2048).~~ **C.5 DONE:** motion is `R32Float` (not RGBA32 — 4-MRT stays ≤32 B/sample); dense 0-based `body_index_map`; all raster writers + `selected_body_index` aligned. Exact ints to 2^24. |
| **Depth-convention mismatch** — the real obstacle | Compositing depth is **fp16 EUCLIDEAN world distance** (`gbuffer.wgsl:607`, read by deferred/SSAO/SSR/fog/RT-shadows/reproject), while the hardware depth attachment is standard 0..1 NDC. Nanite natively produces packed NDC for atomicMax. To composite it must *also* emit euclidean distance (an unproject per pixel) **and** populate the NDC attachment or every overlay draws through it. |

**The last one is a data-format problem, not a rule problem.** That distinction matters — see §3.

---

## 3. The two hard questions — now answered

### Q1 — Does Nanite violate the Display Invariant?

**Answer: the letter, yes. The mechanism, no. And there is already a shipping legal precedent.**

I previously wrote *"do NOT implement Nanite proper"* (`PLAN_FOR_GROK_VIEWPORT_PERF.md` P5). That was
too cautious, and the study found the reason: **a second technique over the same pixels already ships.**

The **sculpt voxel raymarch** runs a compute shader over the *same pixels* in the *same frame* as the
hardware raster pass, overwriting all seven G-buffer slots wherever nearer (`viewport_render.rs:2198-2241`,
`sculpt_voxel_raymarch.wgsl:286-318`). It is legal because of three specific things:

1. It arbitrates against a **bit-exact read-only copy** of the raster's own depth — never a second live depth buffer.
2. The competing raster draw of that same body is **REMOVED from the G-buffer pass entirely**, not depth-tested against (`raster_gbuffer.rs:44`, `:832`).
3. It deliberately does not reuse the raymarch timestamp slot (double-writing an unresolved query = device loss).

**That is the template.** The invariant's real content is *never two depth CONVENTIONS over one frame* —
which is what commit `4f1a30a` actually fixed. Nanite's sw+hw paths write **one** visibility buffer with
**one** depth encoding, resolved in **one** pass. No second convention, no second render-target set.

**⚠ Still an owner call, and it must be written down.** Per `CLAUDE.md`'s Rule Conflict Check, the
amendment — narrowing the invariant to "never two depth *conventions* over one frame" — must be
explicitly approved and committed **before** any cluster raster code lands. Not a silent
reinterpretation by whoever implements it. **This is a Phase-D entry gate.**

What remains genuinely forbidden: a *partial* Nanite where some objects are clustered and some are
hardware-rastered, racing over shared pixels. The removal step (point 2 above) is what prevents it.

### Q2 — How do you edit a clustered mesh? (the "model inside Unreal" problem)

In UE you cannot — Nanite geometry is immutable at runtime, so you round-trip to a DCC. **That single
limitation is the gap this product occupies, so it cannot be an afterthought.**

AZRA already has the structural answer, built for an unrelated reason — the Object/Edit mode split:

```
Object Mode  →  body is CLUSTERED (immutable, virtualised, fast)
Enter Edit   →  body DROPS OUT of the cluster set into its live representation,
                drawn by the normal raster path
Leave Edit   →  body RE-CLUSTERS from mesh_cache, asynchronously, debounced
```

The edited body is the *only* unclustered one, drawn by the existing raster path — a per-object
technique choice, already legal, already how SDF/voxel/poly coexist. **The cluster build is a bake, and
AZRA's entire architecture is bakes** (`BodyVolumeCache`, `dirty_bodies`, debounce, async readback).
Re-clustering on edit-leave is the same shape as `needs_rebake_on_leave()`.

This is the strongest architectural asset AZRA has for the stated product, and it exists by accident.
Protect it deliberately from here on.

---

## 4. Phases

Each phase has a measurable entry gate. No phase starts on an unmeasured predecessor.

### Phase A — Kill the CPU floor *(instrumentation DONE; A1–A6 OPEN — A is NOT finished)*

**Why first:** 19–27 ms per *idle* frame, one cube, against a 12 ms budget. **No rendering win is
measurable through a 25 ms floor.** Every later before/after number is a lie until this is fixed.

| Item | Status |
|---|---|
| Gate `[CTRLX]` logging behind `AZRA_CTRLX` | **DONE** (Grok) |
| Pick-invalidate only on real sync | **DONE** (Grok) |
| `poly_display_dirty` mark-then-drain + 29 call sites | **DONE** (Grok) |
| Perf HUD (`Poly sync: N calls \| M marks`) | **DONE** (Grok) |
| Instrument `rest(top)` | **DONE** (Grok) |
| **Fix what `rest(top)` is — the §2a list** | **OPEN — this is the real Phase A** |

**Do these next. Nothing else starts until the exit gate is published.**

| # | Item | Cost | Note | Status |
|---|---|---|---|---|
| A1 | Gate **only** `sdf_bodies().count()` behind `show_perf_hud` | one line | free; brick_used/cap always written (Properties); dead body_active/sparsity deleted | **DONE** (regression fixed 2026-07-30) |
| A2 | Scene-dirty-gate the `has_raster` walk | one line | **do not delete** — toast only | **DONE** — only walks when forcing Hybrid |
| A3 | Move BODYVIS `format!`/HashMap **inside** `perf_spam_on()` | one line | free | **DONE** |
| A4 | Stop building + uploading the scene BVH nothing reads | small | `BUILD_DEAD_SCENE_BVH=false`; builder kept for B | **DONE** (builder retained) |
| A5 | Cache `world_transform` + invalidate on `get_mut`/hierarchy/`sdf_*_mut` | real work | `Mutex` memo + ancestor fill | **DONE** |
| A6 | Replace O(N×S×D) selection with per-frame outline set | real work | walk-up from selection once | **DONE** |

**Exit gate: PASSED 2026-07-30** — steady idle **frame 1.8–3.0 ms** on §0b scoreboard
(`AZRA_SCOREBOARD=1` → `scoreboard.log`). Cold-start `ai` ~16 ms is one-shot, not idle.

**Next:** re-measure C-phase work under real load (poly LOD / occ / inst cost vs benefit), then
Phase B. Plateau hunt (32/124/465 ms) — re-check under stress; not visible in idle scoreboard.

### Phase B — Make the hub explicit

**Entry gate:** Phase A exit passed.

The hub is real but emergent, undeclared, unenforced. Phases C/D/E all hang off it.

1. **One draw-stream builder.** Scene walk → flat, sorted, representation-agnostic draw list. Raster,
   PT BLAS, and later clusters all consume it. Rebuilt on structural change only (`SCENE` dirty), not
   per frame — camera motion re-culls, it does not rebuild.
2. **Adopt `pt_accel::assemble_impl`'s model** (§2b). It already does geometry dedup by `MeshCacheId`,
   a TLAS over placements, and a **refit fast path** for transform-only change. Do not reinvent this —
   generalise it out of the PT and let both readers share it.
3. **Repoint `build_scene_bvh` at the raster gather** — O(N) frustum scan → O(log N + visible), using a
   structure already built and tested.
4. **Fix `mesh_cache_id` sharing on duplicate.** Study correction: duplicate nulls it for Poly/NURBS/SDF
   but **not** for ImportedMesh/VoxelBody — it is *inconsistent*, not uniformly broken. Make it uniform:
   same mesh → same cache id. Precondition for instancing, and a correctness win alone.

**Exit gate:** N duplicated cubes upload **one** mesh; draw-stream rebuild count is 0 on a camera-only
frame. Publish both.

**B exit — PASSED 2026-07-30** (`AZRA_INSTANCE_STRESS=100 AZRA_SCOREBOARD=1`):
| Evidence | Result |
|---|---|
| Draw-stream camera-only | `streamReb=0` `streamN=100` (idle after cold start) |
| Shared mesh | `PASS B.4: 100 bodies → 1 MeshCacheId` · scoreboard `share=1` |
| Multi-instance | `inst+99` (99 instances merged into multi-draws) |
| Frame | **~4.4–4.9 ms** @ 100 shared poly boxes (disp ~2.1–2.3) |
| How to re-run | `AZRA_INSTANCE_STRESS=N` or Stress Test → instance scale buttons |

Also: empty-stream no longer forced-rebuilds every frame (`ensure` only on `draw_stream_stale`).

### Phase C — Scene scale, hardware only

**Entry gate:** Phase B exit passed. **⚠ Partially started out of order — see §0b.**

"Large forest / epic cityscape" first becomes possible here, **with no Nanite technology**.
**Scope note:** C is *instance* scale (forest, city — repeated assets). Unique high-detail geometry
does not pass C's spirit no matter how well C lands; that is D's job. Answer 1 needs both.

1. **Instanced draws.** *(PARTIAL — draw-time batching landed; authoring-time share did not.)* Every draw today is `draw_indexed(.., 0..1)`; instancing exists only in two
   special-case pools, neither of which generalises, and the SDF pool **structurally models one instance
   per body**. Shared `mesh_cache_id` (Phase B) → one draw with `instance_count`.
2. **Fix culling so it actually removes work.** Frustum culling today *demotes* objects between four
   lockstep draw loops — **a culled object still pays 3 full shadow-cascade draw calls**, and culling
   explicitly exempts the production SDF route and the voxel resident route.
3. **Occlusion culling (Hi-Z).** *(PARTIAL — a CPU AABB Hi-Z at 64×36 landed, main pass only.)* The
   form this plan specifies is **GPU Hi-Z from the previous frame's depth**, which is already produced
   every frame. The CPU version costs frame-thread time per object — exactly the resource Phase A is
   trying to recover — so it must be justified by measurement or replaced.
4. **Poly LOD.** **DONE** — display-only Fast 50 %/15 %, driven by `choose_lod_level`, edit forces
   full, `poly_lod_auto` default ON (`app/src/poly/lod.rs`). **Remaining: measure after Phase A, then
   tune thresholds — nothing else.** Do not add more LOD levels before that measurement.
   **⚠ `MeshLod` is still NOT a distance LOD** — it is a one-way CPU-DC quality ladder, SDF-only by
   construction, with grid-resolution semantics (`Preview 64³`…). Keep poly LOD on its own field;
   do not retrofit it onto `MeshLod` and leave the doc comments lying.
5. **Fix the 2048 object-ID ceiling** (§2c) — **DONE (C.5):** `gbuf_motion` → `R32Float` (MRT budget); dense `body_index_map` + all raster writers.

### C.6–C.9 — closing the 10k gap (added 2026-07-30 after the first honest 10k measurement)

**Measured: 10 000 shared poly = ~45–52 ms** (release) at **only ~120k triangles** — ~4× over the 12 ms
budget. **This is not a geometry problem.** 120k triangles is nothing for any GPU, so the entire cost
is CPU gather + submission. Two consequences, both load-bearing:

* **Clusters/Nanite would not help this case at all.** Phase D is for *unique high-detail* geometry.
  "10k cheap objects" is a submission problem. Do not reach for D to fix C.
* **⚠ The 10k row is the BEST case and currently hides the real problem — see C.6.**

#### C.6 ★★★ — Sort the draw list + multi-mesh gate

**Sort:** production **does** sort `draw_calls` by `(cid, irange, cull)` before consecutive batching
(`raster_gbuffer` P4 block). Batching merges only consecutive same `(cid, irange)`.

**Why a gate still mattered:** the 10k single-mesh stress is the *best* case — one `cid` is always
consecutive even without sort. A realistic scene with 5 interleaved tree meshes would explode to
~N×M draws if sort were missing.

**Gate (DONE 2026-07-30):**
| Check | How |
|---|---|
| Pure | `raster_common::count_mesh_instance_draws` + tests `c6_multi_mesh_sort_collapses_to_n_meshes` / `c6_single_mesh_best_case_hides_without_gate` |
| Sabotage | unsorted interleaved → N×M draws; sorted → N |
| Runtime spawn | `AZRA_MULTI_MESH_STRESS=5x200` or Stress Test → C.6 buttons; scoreboard `draws=` |

**Do not** claim C.6 complete from single-mesh 10k alone.

#### C.7 ★★ — Stop rebuilding the per-frame arrays

**DONE 2026-07-30:** `upload_materials` / `upload_instances` content-hash skip (`last_*_hash`).
Sort for batching no longer keys on cull flags so camera frustum does not reshuffle InstanceData
(buffer stays byte-identical when transforms/materials unchanged).

**Gate:** scoreboard `instUp=0 matUp=0` on camera-only after the first structural frame
(same shape as `streamReb=0`).

#### C.8 ★★ — Per-entry HashMap lookups × 5 passes

**DONE 2026-07-30:** parallel `draw_kind` / `draw_kind_payload` Vecs (`DRAW_KIND_MESH|VOXEL|SDF_*`)
set once at gather. All frustum/occlusion/sort/shadow/gbuffer/wire loops use O(1) kind flags —
no per-entry `HashMap::contains_key` on the 10k path.

#### C.10 ★★★ — Outliner virtualization — **DONE 2026-07-31** (`bed6c17`)

Was ~43 ms of `lay` at 10k (no row virtualization). Fix landed as:
* **v1:** leaf clip-cull (`outliner_draw_poly` — off-screen = `add_space` only)
* **ui2:** `ol_build` cache + viewport paint window (`ui2/outliner/mod.rs`)

**Gate re-measure 2026-07-31** (release, single-mesh stress, default UI):

| N | med | **lay** | draws |
|---|---|---|---|
| 100 | ~0.9 | **~0.4** | 1 |
| 1 000 | ~1.3 | **~0.7** | 1 |
| 10 000 | ~6.2 | **~2.0–3.5** | 1 |

Not perfectly flat (v1 still walks roots for clip spacing), but **~15× better than 43 ms** and
not linear in N. **PASS** for C exit. Residual soft growth is optional polish (true
`show_rows` / skip off-screen `add_space` walk).

#### C.11 ★★ — draws==1 after structural list — **DONE 2026-07-31** (`bed6c17`)

Post-cull **submit packing** (visible-before-culled on the working list only; structural cache
restored cid-sorted). 10k single-mesh: **draws=1**, `structReb=0`, `instUp=0`. **PASS.**

#### C.9 ★ — GPU-driven submission (optional after C exit)

`multi_draw_indexed_indirect` for the mesh_cache path. **Not required for C exit** — at med≈6.5 ms
the addressable target is ~2 ms of `disp`/`rast`. Do when GPU-driven Nanite/D work needs it.

---

**Exit gate — all four — PASSED 2026-07-31** (`3c7430c` + C.10/C.11):

| # | gate | result |
|---|---|---|
| 1 | 10k interactive **median** + tris stated | **single ~120k tris med≈6.6 ms; multi ~1.7M tris med≈6.5 ms** |
| 2 | C.6 multi-mesh green | **draws=5** @ 5×2000; sabotage ~7.6k |
| 3 | C.10 lay 100→10k | **0.4 → 2–3.5 ms** (not 43) |
| 4 | C.11 draws==1 @ 10k single | **draws=1**, structReb=0, instUp=0 |

Also: overlay fingerprint tax fixed (`content_signature`×N idle). Next phase: **D** (clusters)
or optional C.9 / GPU Hi-Z for real overdraw scenes.

**⚠ Re-measure the 10k row.** It was taken in `803a43e`, *before* `4db147a` restored
`resolve_poly_body_material` per object (a per-object face walk the perf commit had removed). If
correctness cost perf, record that honestly in the scoreboard — do not leave a stale faster number
standing.

**C.1 10k measure 2026-07-30** (`AZRA_INSTANCE_STRESS=10000 AZRA_SCOREBOARD=1`):

| build | frame | disp | mesh | plod | share | inst+ | stream | tris |
|---|---|---|---|---|---|---|---|---|
| debug (pre poly hot-path) | ~240 ms | ~124 | ~31 | ~28 | 1 | 2576 | N=10k reb=0 | box 12 × 10k |
| debug (post hot-path) | **~140 ms** | ~72 | ~3 | ~1.2 | 1 | 2576 | N=10k reb=0 | same |
| **release** (pre rigid/BVH band) | **~45–52 ms** (~20–22 fps) | ~12–16 | ~1.2 | ~0.4 | **1** | **~9123** | N=10k reb=0 | **~120k tris** (12×10k) |
| **release** (rigid N + BVH mid-band + ensure cid-skip) | **~39–41 ms** (~24–25 fps) | **~9.4–10** | ~1.3 | ~0.4 | **1** | **inst+9999** | N=10k reb=0 | same |
| **release** (struct list + submit pack + outliner cull) | **med≈10.2 ms** (~98 fps) | **~6–8** | ~1.3 | ~0.4 | **1** | **inst+7–9k** | N=10k reb=0 | **lay≈3** (was 43) |

B.4 holds at 10k (one MeshCacheId). Multi-instance merges visible instances (`draws=1`).
**C exit: PASS (10k single-mesh)** — ~10 ms median release, interactive. Multi-mesh 5×200
and **5×2000 (10k bodies)** also green (`draws=5`).

**5×2000 multi-mesh 10k (release, default camera):**
| | med | draws | disp | lay | notes |
|---|---|---|---|---|---|
| pre-overlay-fix (WITH sort) | ~36 ms | 5 | ~32 | ~3 | dRest owned the 32 ms |
| NO sort sabotage | ~41.5 ms | ~7.6k | ~38 | ~2 | gate holds |
| **post overlay fingerprint fix** | **~6.5 ms** | **5** | **~2** | ~3 | **PASS** |

Draw-count **settled** (5 vs ~7.6k). The 32 ms `disp` floor was **not** geometry/overdraw/
shadows — it was `overlay_fingerprint()` calling `content_signature()` on every poly body
every frame (O(verts)×N even with wire OFF). Fix: skip body walk when no wire/edit overlay
will emit; when walking, use Arc ptr + mesh_cache stamp (O(1)).

**C exit: PASS** — 10k single med≈6.6 ms, 10k multi med≈6.5 ms, draws correct, release.

### Overdraw A/B (2026-07-30) — is disp fragment-bound?

Hypothesis: dense overlapping 5×2000 fills the viewport → severe overdraw; 7 M tris/frame
at 32 ms is only ~220 M tris/sec on RTX 40-class, so not geometry-throughput limited.
Test: same 5×2000 scene, pull camera far (`AZRA_CAM_DISTANCE`) so the grid is a small
screen blob — triangle inventory unchanged; only on-screen pixel coverage changes.

| cam distance | med | **disp** | inst+ | screen coverage |
|---|---|---|---|---|
| default (~fill) | ~36 ms | **~31** | ~3.2k (frustum) | fills viewport |
| 80 | ~36 ms | **~31.5** | ~8.7k | mostly fills |
| 400 | ~36 ms | **~31.5** | ~10k | small |
| 800 | ~36 ms | **~31** | ~10k | tiny |
| **1500** | **~36 ms** | **~31.5** | ~10k | **tiny** |

**Verdict: disp HOLDS → not fragment/overdraw-bound** (for the measured `disp=` = CPU wall
of `dispatch_gpu_render`, which includes raster encode + queue submit + non-blocking poll).
Screen footprint does not move the number. Pulling the camera far also *increases* visible
instances (less frustum cull) with no disp rise — so the floor is not “shade every pixel
many times” either.

Implication for ordering (resolved 2026-07-31):
1. ~~test overdraw~~ → negative
2. ~~profile `dispatch_gpu_render`~~ → **rast≈1.5 ms, dRest≈31 ms**; shadows A/B no-op
3. ~~root cause~~ → **`overlay_fingerprint` content_signature × 10k** (fixed)
4. GPU Hi-Z / C.9 / C.7 A/B remain optional polish — not required for C exit

### Phase D — Cluster geometry (Nanite-class)

**Mandatory** — answer 1 was "both instanced and unique, seriously," and unique geometry cannot be
solved by instancing.

**Entry gates — all must be signed off before ANY cluster *raster* code lands**
(see also `PHASE_D_ENTRY.md` / `PHASE_D_RECLUSTER_FLOW.md`; **execution detail:
`PHASE_D_VIRTUAL_GEOMETRY_PLAN.md`** — gap list G1–G13, Rust-fit design, targets T0–T5, phases D0–D8):

| | Gate | Evidence |
|---|---|---|
| ☑ | Phase C exit passed | 2026-07-31 — four gates green (`7ccc4c8`); single/multi 10k med≈6.5 ms |
| ☑ | Triangle count is the *measured* bottleneck | **CLOSED 2026-08-02** (feat/perf-tribound, merged `601ffe2`) — GPU linear ~0.3–0.5 ms/M tris, fill-INDEPENDENT (50M near-subpixel = same 26 ms) → vertex/geometry-bound; shadow pass pays the tri bill twice. |
| ☑ | **Q1 — owner-approved Display Invariant amendment** | owner: Dimitri Neron · date: 2026-08-03 · `CLAUDE.md` commit: `b8b56d1` — invariant narrowed to "never two depth conventions over one frame"; owner: "this is an old rule from the analytical SDF render era, no longer valid — replace with a rule that works with what we built." |
| ☑ | **Q2 — recluster flow written** | `docs/planning/render/PHASE_D_RECLUSTER_FLOW.md` (2026-07-31) — job owner, debounce, pop, mid-recluster cancel |

Clusters without the Q2 edit round-trip reproduce exactly the UE limitation this product exists to
beat. Q1 unsigned means someone is reinterpreting a non-negotiable rule alone — forbidden by
`CLAUDE.md`'s Rule Conflict Check.

Then:
1. **Cluster builder as a bake.** `mesh_cache` triangles → ~128-tri clusters → simplified parents → DAG.
   Background thread, debounced, content-keyed — the `BodyVolumeCache` shape. Never the frame thread.
   **Reuse the QEM simplifier**, adding hard `locked_verts`/`locked_edges` + determinism (§2b) — a soft
   cost multiplier lets shared boundaries move, and any movement between adjacent groups is a crack.
2. **Generalise `SdfMeshPool` beyond SDF** and cut tiles ~20× finer. The arena and GPU-authored indirect
   args already work; what is missing is a per-tile LOD chain and per-cluster GPU visibility (today it
   is per-body on the CPU, and a listed body draws all its live tiles).
3. **Two-pass Hi-Z occlusion over clusters** (builds on Phase C item 3).
4. **Visibility buffer + software raster** — **only** if measured, and only as a total replacement of
   the opaque pass. Requires the §2c blockers resolved: request `SHADER_INT64_ATOMIC_MIN_MAX`, or use
   a 32-bit packed `depth24|clusterID8` compromise; and solve the euclidean-vs-NDC depth emission.
5. **Streaming.** Cluster pages in/out by distance — required by answer 1.

**Exit gate:** a scene impossible in Phase C, measured, **with the edit round-trip working** (enter edit
on a clustered body, modify, leave, re-cluster) and no visible pop.

### Phase E — Runtime split (the game)

**Entry gate:** Phase D exit passed. Mostly packaging and asset-baking — the renderer already exists.

1. **Bake target.** Scene + clusters + materials → runtime format (`io_bridge`'s `export_glb` is the
   seed; the runtime format is a superset with clusters, instances, streaming pages).
2. **Runtime crate.** Loads baked assets, drives the Phase-D renderer. No editor, no undo, no half-edge kernel.
3. **Streaming**, already built in Phase D.

**Out of scope until asked:** gameplay, physics, audio, scripting.

### Parallel track — Path tracer (offline, independent)

Answer 3 = **offline final frame**, which makes this tractable. PT reads `mesh_cache` via its BLAS
builder and keeps working across every phase provided Phase B preserves the hub contract.

* Phase B's shared `mesh_cache_id` shares BLAS across instances — same change, free win.
* Phase D's cluster LODs become **secondary-ray geometry** (coarse levels for indirect bounces) — a
  standard optimisation, free once D exists.
* City-scale needs **out-of-core / streaming BLAS**. Offline budget makes this engineering, not research.
* **The TLAS is already instance-aware with a refit fast path** — the hardest part is done.

---

## 5. Terrain — the owner's proposal, assessed

> "We could use SDF displacement, with EXR image map to deform, with primitive… voxel base could also
> be nice… but if we want local height detail deformation we need at the end a heightfield (create shape
> in SDF and voxel and turn it into mesh with height map for local detail)."

**This is right, it composes with the existing architecture, and it is not a new pillar.** The
instinct at the end — *"at the end we need a heightfield for local detail"* — is the correct conclusion,
and here is precisely why:

**Baking displacement into the SDF voxel grid caps detail at grid resolution.** A 512³ grid over 1 km
of terrain is ~2 m per voxel. Fine detail cannot survive that, no matter how good the EXR map is. So
the split has to be:

| Scale | Mechanism | Where it lands |
|---|---|---|
| **Large shape** (hills, valleys, mesas) | SDF primitive + EXR displacement modifier, baked to mesh | Existing pipeline. Low-frequency features survive grid resolution fine. |
| **Local detail** (rocks, erosion, tracks) | Heightmap displacement applied at **mesh** level — at **cluster-build time** in Phase D | Bypasses the voxel resolution ceiling entirely: each cluster displaces at whatever density that LOD needs. |

That second row is exactly how UE 5.3+ Nanite Displacement works, and it is why terrain **wants** Phase D
rather than a separate terrain system: the cluster LOD *is* the terrain LOD.

**Work items, both additive:**
1. A displacement modifier on the SDF side with an **image-map input** (EXR). Needs a texture source in
   the SDF eval path — and per `CLAUDE.md`, it must land in **both** eval paths (`sdf_prim_eval.wgsl`
   *and* `volume_bake.wgsl`) plus the CPU evaluator, in the same task.
2. Displacement at cluster-build time in Phase D.

**No new `NodePayload` variant, no new pillar, no new LOD scheme.** Revisit only if terrain authoring
turns out to need heightfield-native editing tools (sculpt-on-heightmap), which is a UX question, not
an architecture one.

---

## 6. Order, and what would change it

| # | Phase | Gate | Serves |
|---|---|---|---|
| A | Kill CPU floor | — | all three |
| B | Explicit mesh hub | A under budget | all three |
| C | Instancing + real culling + poly LOD | B exit | viewport, game |
| D | Clusters + streaming | C exit **+ Q1 amended + Q2 specified** | game, big scenes, terrain detail |
| E | Runtime split | D exit | game only |
| — | Path tracer (offline) | independent | film / video |

**What would legitimately reorder this:** a measurement showing triangle throughput — not CPU time, not
draw-call count — binds *before* Phase C is done. Nothing observed suggests it: an idle frame with one
cube at 2× budget is not a triangle problem, and the study found the O(N) mechanisms behind it.

**What would not:** wanting Nanite sooner. The cluster builder consumes `mesh_cache`; the selection pass
consumes the draw stream. Both are Phase B deliverables. **Starting D early does not skip B — it builds
B badly, inside D, under deadline.**

### Known ceiling, deliberately accepted (answer 5)

`transaction()` deep-clones the **entire** `SceneGraph` per user action and retains 50 snapshots
(`undo.rs:570-591`, `app_new.rs:253`). At ~10k nodes that is already hundreds of MB retained and a
multi-millisecond hitch per click; at ~100k it is out of memory. The code itself is aware
(`undo.rs:672-674` refuses to wrap selection clicks for this reason).

Because the editor authors at moderate scale (answer 5), **this is not Phase 0.** But it *is* the hard
ceiling on authored scene size, and if authoring workflows start feeling heavy, this is the cause. A
delta/COW undo model is the fix when that day comes.

---

## 7. Standing rules for every phase

1. `cargo build -p app` after every change; `cargo test -p core_math` after any `half_edge.rs` change;
   `cargo test -p graphics` after any shader change.
2. **Gate on the property, not on "it changed."** Three features have shipped whose gate passed
   *because* the output was broken.
3. **Sabotage-check every new gate** — revert the fix, confirm the gate FAILS, restore.
4. **Publish a before/after number for every step.** A measured dead end is a result (`AdjacencyCache`
   and `content_signature`-as-proposed were both correctly killed by measurement).
5. One item per commit, stating what was *measured*.
6. **Never change `HalfEdgeMesh`'s serialized shape.** Clusters, LODs and runtime data are derived —
   caches and bake products, never fields on the serialized mesh. New authored state rides the in-blob
   `#[serde(default)]` path (`voxel_grids` precedent).
7. **G-Buffer Contract changes** (the object-ID format fix, any Nanite slot) touch `graphics::gbuffer_format`
   constants **and** the raymarch-lite, sculpt-raymarch and deferred writers **in lockstep**. All 7
   outputs written every frame.

---

## 8. Doc corrections this study forced

* **`VIEWPORT_MODEL.md` is stale on the one point this roadmap depends on** — it still describes SDF
  Edit Mode as raymarching. It no longer does (analytical path deleted 2026-07-14; SDF renders from its
  baked DC mesh in every mode). Fix before Phase D, because Q1's argument reads that file.
* **`PLAN_FOR_GROK_VIEWPORT_PERF.md` P5** says "do NOT implement Nanite proper." Superseded by §3 —
  keep the file for P1–P4, which stand.
* `CLAUDE.md`'s Display Invariant needs the §3 amendment **before** Phase D, not during it.
