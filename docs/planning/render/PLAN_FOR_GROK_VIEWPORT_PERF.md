# Plan for Grok — viewport performance + poly LOD

> ## ⚠ `AZRA_MASTER_ROADMAP.md` §0b IS THE AUTHORITY ON STATUS. This file is HISTORICAL, for P1–P4 only.
> **A fresh GO on this file must NOT re-open Phase C work.** The next sanctioned action is A1–A6 in the
> master roadmap, until idle-one-cube is published under 12 ms.
>
> **This file is SUBORDINATE to `AZRA_MASTER_ROADMAP.md`.** Read that first — it holds the
> product scope (modeling + game runtime + offline PT), the phase gates, and the reconciled status.
> Two specific overrides:
> * **P5 "do NOT implement Nanite proper" is SUPERSEDED.** A second technique over the same pixels
>   already ships legally (the sculpt voxel raymarch); see master roadmap §3. Nanite is the stated end
>   goal, gated on an owner-approved Display Invariant amendment — not forbidden.
> * **P3/P4 marked DONE below are Phase-C work that landed before the Phase-A exit gate.** Their
>   numbers are not trustworthy until idle-one-cube is under 12 ms. See master roadmap §0b — the CPU
>   Hi-Z and poly LOD both *add* frame-thread cost and may have made the floor worse.
>
> P1–P4's item list stands and is still the right work. The ordering claim in this file's header
> ("the ordering is the point") was correct and was not followed.

**Date:** 2026-07-30
**Sources:** `LIVE_SESSION_2026-07-30.md` (measured), `LOD_STATUS_2026-07-30.md` (survey).
**Everything below was measured or verified in code — no estimates presented as facts.**

### Execution status (2026-07-30, ui-polish)

| Item | Status |
|------|--------|
| Gate CTRLX logging (`AZRA_CTRLX=1` to enable) | **DONE** |
| Pick invalidate only on real sync (not early-out) | **DONE** |
| Mark-then-drain `poly_display_dirty` + 29 call sites | **DONE** |
| Perf HUD: `Poly sync: N calls \| M marks` | **DONE** |
| P2 instrument rest(top) deeper | **DONE** — named sub-timers + top/2nd/rest/maps on HUD + SLOWPOLL |
| P3 Poly LOD | **DONE** — display-only Fast 50%/15%; SDF `choose_lod_level`; edit force full; `poly_lod_auto` default ON |
| P4 Occlusion pyramid / instances | **DONE** — CPU AABB Hi-Z (64×36) after frustum; poly content-key mesh share; multi-instance draw batching; HUD `occ`/`inst` |
| P5 Cluster LOD | **DEFERRED** — plan gate: only if drawn tris still dominate after P1–P4. HUD now shows `Poly LOD: L0/L1/L2 \| tris full→drawn` for that decision. Shared coarse LOD pool by content_key. |

**Already shipped earlier** (do not re-implement): display Arc cache, modifier prefix cache, position-only grab path, GPU face fill, pick ear-clip, wire stride/cap honesty — see `poly/POLY_VIEWPORT_PERF_PLAN.md`.

---

## Read this first — the ordering is the point

The owner asked for Nanite-style object LOD. **That is item 4, not item 1**, because measurement says
triangles are not the bottleneck yet:

> `poll_background_tasks` sits at **19–27 ms every IDLE frame** — with one cube on screen, against a
> **12 ms** FrameScheduler budget.

Nothing about triangle throughput explains an idle frame costing 2× budget with one object. Fix the
CPU waste first: it is cheaper, it is measurable today, and it is a prerequisite for measuring LOD
honestly — you cannot see a rendering win through a 25 ms CPU floor.

### Standing rules

1. `cargo build -p app` after every change; `cargo test -p core_math` after any `half_edge.rs` change.
2. **Gate on the property, not on "it changed."** Three features have now shipped whose gate passed
   *because* the output was broken.
3. **Sabotage-check every new gate** — revert the fix, confirm the gate FAILS, restore.
4. **Measure before and after, and publish the number.** If a step does not move the metric, say so
   and stop — a measured dead end is a result (see `AdjacencyCache`, O3-as-proposed).
5. One item per commit, stating what was *measured*.

---

## P1 ★★★ — `poly_sync_mesh_cache` fires ~7× per actual change

**Measured over a real modelling session: 331 sync calls across FIVE distinct mesh states.**

| calls | mesh state |
|---|---|
| 180 | `live_faces=11 raster_verts=46 tris=24` |
| 80 | `live_faces=10 raster_verts=40 tris=20` |
| 38 | `live_faces=13 raster_verts=58 tris=32` |
| 32 | `live_faces=13 raster_verts=55 tris=29` |

180 consecutive syncs of a **byte-identical** mesh. Each one rebuilds raster data and re-uploads an
index buffer.

### The fix is FEWER CALLS, not a guard — this is measured, do not skip it

I benchmarked the obvious guard (`content_signature`) against the rebuild it would protect:

| faces | `content_signature` | `to_raster_data` | ratio |
|---|---|---|---|
| 96 | 0.023 ms | 0.045 ms | **1.9×** |
| 384 | 0.091 ms | 0.190 ms | **2.1×** |
| 1536 | 0.368 ms | 0.773 ms | **2.1×** |

The hash costs **half** the rebuild — both are O(V+E+F) walks over the same data. Guarding caps the win
at ~2× **and adds cost** on frames where the mesh really did change. Removing redundant calls recovers
**100 %** of the waste.

### Where the calls are

`poly_sync_mesh_cache` has ~50 call sites. The top offenders:

| file | calls |
|---|---|
| `poly_drag_tools.rs` | 12 |
| `poly/circle.rs` | 7 |
| `symmetry.rs` | 6 |
| `poly/slide.rs` | 5 |
| `poly/loop_cut.rs` | 5 |
| `poly/boolean.rs` | 5 |

The pattern to look for: an op syncs, then its *caller* syncs, then the frame loop syncs again —
each defensively, none aware of the others.

**Suggested shape:** a `poly_display_dirty: HashSet<NodeId>` on `CacheState`. Ops *mark* rather than
sync; one drain point in the poll phase syncs each dirty body **once per frame**. That is the same
mark-then-drain pattern `dirty_bodies` already uses for volume bakes, so it is an established idiom
here rather than a new one.

**Gate:** a headless test that applies N ops to one body and asserts the sync count is **1 per frame**,
not 1 per op. And the existing poly suites must stay green — a missed mark shows up as a stale
viewport, which is worse than the redundancy.

**⚠ First, check the `[CTRLX]` logging.** It is debug instrumentation added during today's poly work
and it fires on every sync. Measure with it stripped **before** optimising: if the logging is a large
share of the 19–27 ms, the sync count is a symptom, not the bill.

---

## P2 ★★ — Find what else is in `poll_background_tasks`

`SLOWPOLL` reports every sub-timer as **zero** while the total is 19–27 ms — all of it lands in
`rest(top)`, i.e. code that is not instrumented.

Also seen: **three stable plateaus at ~32 ms, ~124 ms, ~465 ms**. Those are roughly 1× / 4× / 15× a
~31 ms unit, which smells like *N iterations of one ~31 ms operation* rather than three separate
costs. Counting iterations per poll would confirm or kill that in a single run.

**Already eliminated — do not redo:** `app_poll_gpu.rs:568`'s `Maintain::Wait` is behind an unset
`AZRA_PROBE`; there are no `recv_timeout`s or hardcoded blocking delays in the poll path (the three
`from_millis(100)` hits are `request_repaint_after`, non-blocking).

**Task: instrument `rest(top)` and REPORT before fixing.** This is a diagnostic step, not a fix.

---

## P3 ★★ — Poly LOD, reusing the SDF chooser

Only after P1/P2, because a 25 ms CPU floor hides any rendering win.

**The chooser already exists and is good.** `sdf_gpu_mesh/lod_policy.rs::choose_lod_level` takes
*projected pixels* → level, with deadband hysteresis both ways, a resolution-aware ceiling, and a test
asserting it never oscillates on a static signal. **It is generic over "how big is this on screen" and
does not care what produced the mesh.**

What is missing is two things:

1. **Poly bodies are not in the walk.** `upgrade_mesh_lod` (`raster_mesh_prep.rs:229`) and
   `ensure_body_meshes` (`:46`) both iterate `scene_graph.sdf_bodies()`.
2. **No coarse poly meshes to switch to.** But `DecimateParams` already exists with 14 parameters and
   5 modes including `DecimateMode::Master` (importance map + QEM). Pre-decimate each poly body once
   at ~50 % and ~15 % on first use, cache all three, let the chooser pick.

**⚠ `MeshLod` is NOT reusable as-is.** Its variants are documented as grid resolutions —
`Preview (64³)`, `Medium (128³)`, `High (256³)` (`render/src/mesh_cache.rs:17`). That is SDF bake
semantics. A poly body has no grid. Either add a separate poly LOD field or generalise the enum's
*meaning* — do not silently reuse it and leave the doc comments lying.

**Gates:**
* Silhouette: switching level must not move the object's screen-space outline beyond a threshold at
  the switch distance. (LOD that pops is worse than no LOD.)
* No oscillation: reuse `lod_chooser_never_oscillates_for_a_static_signal` against the poly path.
* Triangle count actually drops at distance — assert it, or the feature is decorative.

---

## P4 ★ — Occlusion culling, then instancing

**Occlusion culling before cluster LOD.** For interior scenes — the case where object counts explode —
it typically beats LOD outright, and the depth buffer already exists. A GPU depth-pyramid test against
last frame's depth is the standard cheap version. Frustum culling is already in
(`raster_gbuffer.rs:537`).

**Instanced draw for duplicates.** `Shift+D` copies are separate bodies with separate draw calls. If
they share a `MeshCacheId` they can share one instanced draw. `draw_indexed_indirect` +
`instance_count` plumbing already exists on the voxel path.

---

## P5 — Cluster LOD (the Nanite-shaped idea), ONLY if measurement demands it

If P1–P4 land and triangle count is *still* the measured bottleneck, the piece worth stealing from
Nanite is **per-cluster LOD without the software rasteriser**: split large meshes into clusters, choose
a cut per frame, draw with the hardware rasteriser.

**Do NOT implement Nanite proper.** It replaces the hardware raster path for opaque geometry, and
CLAUDE.md's Display Invariant forbids fusing a second technique over the same pixels — that is a
device-loss crash class here, not a style preference.

---

## Order, and the one rule that matters

| # | Item | Why here |
|---|---|---|
| 1 | `poly_sync` redundancy | ~7× measured waste; guard proven wrong, so removal is the fix |
| 2 | Instrument `rest(top)` | 19–27 ms/idle-frame is unaccounted; diagnose before fixing |
| 3 | Poly LOD | reuses a tested chooser; blocked on 1–2 for honest measurement |
| 4 | Occlusion + instancing | bigger interior-scene win than LOD, cheaper than clusters |
| 5 | Cluster LOD | only if triangles measurably dominate after 1–4 |

**Publish a before/after number for every step.** The owner asked "what percentage" about work that
had none — that question should always have an answer, and where it does not, say so plainly instead
of implying a win.
