# REMESH_GPU_PLAN.md

## GPU Voxel Remesh — real output, "3D-Coat feel"

Branch `feat/remesh-gpu`, worktree `.claude/worktrees/remesh-gpu`, based on
`feat/remesh-adaptive-octree` @ `b924432` (NOT master — see §6).

Companion to `REMESH_ADAPTIVE_OCTREE_PLAN.md`, which optimised the same modifier on the CPU and is
the source of every baseline number here. Read its §12 measurement log before touching this one.

---

## 0. How to use this document

Same contract as the octree plan: **gates open in order, each gate closes with a measurement, and a
gate whose premise dies gets rewritten before the next one opens.** Three times running on the
octree plan the dominant cost was not the stage it was named after — so no gate here starts with
code.

---

## 1. Goal

Move the Remesh modifier's **field evaluation and DC extract** to the GPU. Output stays a real
`HalfEdgeMesh` consumed by the stack, picking and export — this is not a preview path.

**Target:** ≤ 8 ms modifier eval at res 256 during a sustained vertex drag, which is what lets
`REMESH_LIVE_SCRUB_MAX` (128) be deleted so the resolution slider scrubs live at every resolution.

**Stretch, not promised:** res cap 256 → 512. A 512³ dense CPU grid is a 537 MB allocation, which is
why the cap exists; a GPU-resident field removes that wall. High voxel resolution is the actual
3D-Coat property, but it depends on a sparse GPU field and is out of scope until Gate 4 closes.

---

## 2. Baseline — measured, not assumed

> **SUPERSEDED 2026-08-10 — this table is an OPEN-CRUST measurement.** It is kept because Gates 0/1
> were scoped against it. The figures that matter for a real model are in **§8b**: a solid drag
> frame at res 256 is **62.6 ms**, split S1 4.2 / S2 25.2 / S3 12.7 / S4 22.8. Do not quote the
> table below as "the drag frame".

`feat/remesh-adaptive-octree` @ `b924432`, res 256, incremental drag frame, median of 5:

| stage | ms | moves to GPU? |
|---|---|---|
| S1 seal (run-CCL) | 3.6 | **hard — global connectivity, see Gate 0** |
| S2 DC extract | 11.5 | yes — `sdf_mesh.wgsl` already does this |
| S3 band sample + write | 1.2 | yes |
| S4 `from_mesh` | 9.9 | **no — CPU, stack contract** |
| **total** | **24.9** | |

master is at ~81 ms — the octree branch is unmerged (3 ahead / 13 behind).

**The consequence that shapes this plan:** GPU removes S1+S2+S3 = 16.3 ms and cannot touch S4. So
the GPU work alone lands at ~10 ms and is then bounded by a CPU stage. Hitting the 8 ms target
requires Gate 1, which is CPU work.

---

## 3. The toggle (owner-directed 2026-08-10)

The whole feature sits behind `ExperimentalFlags::gpu_remesh`, default **OFF**
(`app/src/experimental.rs`, checkbox in `win_settings.rs` → 🧪 Experimental).

**Why this is not the "two code paths" violation CLAUDE.md bans.** That rule targets silent forks
where two implementations of one operation drift apart unnoticed. Here the CPU path is the
**reference oracle** and the parity criteria in Gate 2/3 are the structural enforcement: the paths
are required to agree on solid/air classification for 100% of cells, and that agreement is a test
gate, not a hope. The toggle is legal exactly as long as parity is proven.

**If parity cannot be held, the toggle becomes a fork and must be deleted along with the GPU path**
— not shipped as "GPU mode looks a bit different". Gate 5 resolves this either way.

`#[serde(default)]` on the new flag is load-bearing: `ExperimentalFlags::load()` ends in
`unwrap_or_default()`, so a field missing from an existing `~/.azra/experimental.json` fails the
whole parse and silently resets every other flag.

---

## 4. Gates

### Gate 0 — Spike GPU sign — **CLOSED 2026-08-10, results in §8**

Everything else in this plan is plumbing over parts that already exist and are tested. Sign is not,
and it is the one thing that can kill the design.

The CPU field gets its sign from a **border flood** (inside the band sampler) plus a global
`seal_enclosed_air` run-CCL. Both are global connectivity — the GPU-hostile shape.

**The tempting wrong answer:** a winding-number or ray-parity sign is trivially parallel and would
make this gate disappear. It is the wrong *semantics* — sealed internal voids would stop filling
solid, which is the shipped Blender/3D-Coat behaviour (`remesh.rs` §Semantics). Do not adopt it to
make a number look good.

**What the algorithm actually is** (read from `io_bridge/src/mesh_to_sdf.rs`, 2026-08-10, before
any measurement): the band sampler is two phases — a **scatter** (parallel over z-planes, each
triangle writes exact signed distance in band; embarrassingly parallel, straight GPU port) and a
**sign flood** (`flood_air_reach`, span-based 6-connected AIR fill seeded from the region border).
Walkability is "far cell, or in-band POSITIVE" — so the flood walks the air side including in-band
cells, which is what stops a thin shell from walling off open space. That flood, plus
`seal_enclosed_air`'s run-CCL, is the entire global-connectivity cost: **4.8 ms of the 24.9**.

Measure, at res 128 and 256, on the drag fixture plus a sealed-void fixture:

1. **Full-grid iterative label propagation** — passes to converge × cost per pass. Worst case is
   O(res) passes on a tortuous air path; use an atomic "changed" counter with indirect dispatch, not
   a per-pass readback.
2. **Hierarchical** — per-block connected components (8³ or 32³), coarse adjacency graph, solve
   coarse connectivity, propagate down. The standard GPU approach to exactly this problem.
3. **Hybrid — keep the flood on the CPU.** The walkable mask is one bit per cell = **2.1 MB at
   256³** (established by Gate 1a in the octree plan, which is why the bitset rewrite won). So: GPU
   scatters the band → read back a 2.1 MB bitset → CPU runs the *already-proven* run-CCL (3.6 ms) →
   upload the outside mask → GPU finalizes signs and extracts.
   - **Advantage that may decide the gate:** sign semantics stay bit-identical to the CPU path by
     construction, which turns the 100%-classification parity criterion from a research problem into
     a property of the design. Red lines 1 and 6 come along for free.
   - **Cost:** two round trips per frame instead of one (more latency, see Gate 4), and ~4.8 ms
     stays on the CPU — so the frame floor becomes ~4.8 + S4 rather than S4 alone.

**Deliverable:** a number per option and a go/no-go, no production code.
**Stop condition:** if no option beats ~2 ms *and* option 3's latency is unacceptable, the GPU win
drops 16.3 → ~12.7 ms. Stop and re-rank against Gate 1 before building Gates 2–4.

### Gate 0.5 — `REMESHSOLIDNOINCR` — **CLOSED 2026-08-10, results in §8a**

Split `band_finalize`'s `enclosed` counter so `had_enclosed_void` keys on genuine sealed pockets
(in-band positive AND unreached) rather than on any unreached far cell (which is just a solid's
interior). Detail and the proposed fix are in the `REMESHSOLIDNOINCR` bug-tracker entry; the gate is
that `incremental_refuses_fields_with_sealed_voids` still refuses while a new solid-cube case now
goes incremental. Expected ~26 ms; **actual 61.7 ms** — see §8a, and §8b for why.

Red line 1 still binds: the lockout is being pointed at the right signal, not weakened.

### Gate 1 — S4 `from_mesh` twin-linking → CSR (CPU) — **CLOSED 2026-08-10, results in §8c**

Specced in octree plan §12f. 295k half-edges doing a hash remove-or-insert against a map peaking
near 147k entries; the cost is cache misses, not hashing. Replace with direct addressing: count
outgoing edges per vertex → prefix-sum to CSR → find `dst→src` by short linear scan of
`bucket[dst]` (valence ~4). **9.9 → ~4 ms.**

Must reproduce existing pairing semantics **exactly**, including the non-manifold case where 3+
half-edges share an edge and the current code pairs the first two encountered and re-stores the
third — so the replacement walks in the same quad order and consumes entries.

`from_quads` backs every mesh constructor in the codebase; blast radius is the whole poly kernel.
Gate: `cargo test -p core_math` + the 7 twin-pairing pins added in `b924432`.

*This is CPU work in a GPU plan. It is here because it is on the critical path either way and it is
the cheapest 6 ms available. Cuttable — the target then becomes ~10 ms rather than ~5 ms.*

### Gate 2 — GPU triangle → band field

The genuinely new piece. Nothing in the tree voxelizes a triangle mesh on the GPU:
`volume_bake.wgsl` bakes analytic prims only.

- New WGSL: one thread per triangle, splat unsigned distance into its dilated bbox via `atomicMin`
  on sortable-u32-encoded f32. Band is `w = 2·cell`, so work is O(surface), matching the CPU band
  sampler's complexity.
- Sign from whatever Gate 0 chose.
- Input triangles: persistent GPU buffer, re-uploaded on change (~1–2 MB at 40k quads, sub-ms).

**Parity gate** vs `region_mesh_to_sdf_band_stats`. Bit-identical is impossible across float
orderings, so parity is two separate criteria and only one of them has a tolerance:
- solid/air classification identical on **100%** of cells — this decides topology, and it is not
  negotiable;
- `|Δdensity| ≤ 1e-6` elsewhere.

### Gate 3 — GPU DC extract on the band field

`graphics/src/sdf_mesh.wgsl` + `app/src/sdf_gpu_mesh/dc_pipeline.rs` already dual-contour a `res³`
f32 field, with a sparse brick decode and existing parity tests. Wire the band field in.

Traps already recorded and paid for once in the octree plan — do not rediscover:
- Sign adapter is **`iso - density`**, not `density - iso` (§4a-H1). The wrong one fails *silently*:
  it passes a position-only parity test while flipping every quad winding.
- Construct cell counts with `res - 1` (§4a-H2).
- **Verify the DC output is welded.** `chunked_dc` duplicates seam vertices and nothing welds them;
  `from_mesh` needs manifold topology. `sdf_mesh.wgsl` uses a global `cell_vert` table so it should
  be fine — confirm it, do not assume it.
- QEF placement must match CPU `CellPlacement::Qef`. `relax_mesh` (smooth=true, 1.5 ms global
  Laplacian) either ports or stays on the CPU — decide with a measurement.

### Gate 4 — Async plumbing and the latch (the architectural gate)

**The modifier walk cannot dispatch GPU work.** `apply_poly_modifiers_cached` is called from
`properties_poly.rs:768`, an egui panel callback. Creating buffers or dispatching there violates the
Frame Ordering Contract outright.

- walk → *requests* an eval (params + input signature) and consumes the last completed result whose
  signature matches; otherwise keeps the previous mesh. The existing `None` fail-closed contract at
  `mesh_sync.rs:549` already has exactly this shape.
- `poll_background_tasks` → dispatch + `PendingSdfReadback` map.
- Generation counters for invalidation, never frame delays (CLAUDE.md).

**Cost to accept:** `PendingSdfReadback` resolves over 1–2 frames, so the remeshed surface trails
the drag by ~16–32 ms. Standard for a DCC, still a behaviour change — owner ruling required before
this gate closes.

### Gate 5 — Resolve the toggle

Parity held → GPU becomes the path whenever a device exists; CPU survives as test oracle and
headless fallback; the flag either graduates to a real preference ("GPU acceleration") or is
deleted. Parity failed → GPU path and flag both deleted.

**Owner ruling required:** applying the modifier bakes its output into the saved `.fl4`. GPU float
divergence is vendor-dependent, so the same scene remeshed on NVIDIA vs AMD produces slightly
different geometry. Normal for a DCC, but it would be the first time saved geometry in this
codebase depends on hardware.

---

## 5. Red lines

1. **`had_enclosed_void` must not be weakened to buy speed.** That class already shipped a bug
   (false seals filling reachable air, fixed in `da65fbb`).
2. **No GPU dispatch or resource creation inside the modifier walk** (Gate 4).
3. **Display / picking / export share ONE triangulation.** No GPU-display + CPU-export fork.
4. **Solid/air parity is exact.** Only positions get a tolerance.
5. **`FL4PRMSH` may still gain fields, but only while unreleased** (shipped `9ff55fd` on the octree
   branch). `load_binary` propagates a section decode error with `?` and fails the entire load.
6. **Sealed voids fill solid.** Any sign scheme that changes this is wrong regardless of speed.

---

## 6. Where this lands

Based on `feat/remesh-adaptive-octree`, not master: that branch carries the 3.3× CPU work this
builds on (81.3 → 24.9 ms) plus the bench harnesses (`measure_drag_frame_stages`, median-of-5)
every gate here measures with.

**Merge order: octree → master first, then GPU.** Rebasing this onto a master that lacks the octree
work would silently restore the 81 ms baseline and invalidate every number in §2.

---

## 7. Expected outcome

| | @256 drag frame |
|---|---|
| master today | ~81 ms |
| octree branch today (baseline) | 24.9 ms |
| + Gate 1 (S4 CSR) | ~19 ms |
| + Gates 2–4 (GPU field + extract) | **~5 ms + 1–2 frame latency** |

---

## 8. Gate 0 — CLOSED 2026-08-10. Two results, both re-sequence the plan.

Probes (measurement only, no production code):
`io_bridge/tests/gate0_sign_flood_depth.rs`, `gate0_incremental_on_solid`,
`gate0_solid_drag_baseline` (the latter two in `app/src/poly/remesh.rs`, `#[ignore]`).

### R1 — Option 1 (full-grid iterative GPU propagation) is DEAD

A naive GPU frontier kernel advances one cell per dispatch, so `passes ≈ BFS depth`. Measured depth
of the sign flood (level-synchronous BFS over the air set, seeded from the whole border shell):

| fixture | res 128 | res 256 |
|---|---|---|
| cube | 21 | 42 |
| sphere | 39 | 78 |
| cup (shallow) | 76 | 282 |
| cup (deep bore) | 111 | 224 |

Depth scales linearly with resolution (exactly 2× from 128 → 256, as geometry says it must). Even
the trivial cube needs 42 dispatches at 256; a cup needs 282, and after 64 passes a cup has still
only reached 84% of its air. At 16.7M cells per pass this cannot approach the 3.6 ms the CPU
run-CCL already achieves. **Option 1 is closed. If Gates 2–4 proceed, sign goes to option 2
(hierarchical) or option 3 (hybrid).**

*Method note: the first run of this probe was reported with a cup fixture I believed was malformed
(degenerate zero-area disc triangles). Fixing the fixture changed the numbers not at all — the
hypothesis was wrong and the anomaly that prompted it was R2 below. The degenerate-triangle fix was
kept because it is correct, not because it mattered.*

### R2 — the baseline this plan is built on describes a shape nobody models

**V1 incremental remesh never engages on a closed solid.** Filed as `REMESHSOLIDNOINCR` in
`COMPLETE_BUG_TRACKER.md`; root cause is that `band_finalize`'s `enclosed` counter counts a solid's
whole interior, and `had_enclosed_void: enclosed > 0` then latches the one-way lockout forever.

| res | fixture | drag frame | incremental |
|---|---|---|---|
| 256 | plane (open crust) | 26.1 ms | 6/6 |
| 256 | uv sphere (solid) | **191.1 ms** | **0/6** |

Every incremental fixture in the tree is `new_plane`. §2's "24.9 ms" is therefore a *crust* number;
the real drag frame on a model is **191 ms (~5 fps)**, 7.3× worse.

### Consequence — the GPU work is no longer the top lever

Fixing `REMESHSOLIDNOINCR` is a counter split plus a test, and takes a solid drag from **191 → ~26
ms**. The entire GPU build, at its best, saves 16.3 ms. **A new Gate 0.5 must land first**, and §7's
outcome table is void until it does — every figure in it was measured on the crust path.

This does not kill the GPU plan: after Gate 0.5 the solid path lands where the crust path is now,
and the GPU argument returns exactly as written, against an honest baseline. It does re-order the
work, and it means the toggle stays OFF for longer than expected.

## 8a. Gate 0.5 — CLOSED 2026-08-10

`band_finalize` now returns `BandSignStats { far_solid, sealed_pocket }` and the lockout keys on
`sealed_pocket` alone. Named struct, not a re-defined `usize`, so the two counts cannot be conflated
again by the next reader.

| res | fixture | before | after | incremental |
|---|---|---|---|---|
| 128 | uv sphere (solid) | 37.2 ms | **18.7 ms** | 0/6 → **6/6** |
| 256 | uv sphere (solid) | 191.1 ms | **61.7 ms** | 0/6 → **6/6** |
| 256 | plane (open crust) | 26.1 ms | 25.7 ms | 6/6 (unchanged, as it must be) |

**§4's prediction of "~26 ms" was wrong — solids land at 61.7 ms, 3.1× not 7.3×.** A solid drag
frame is genuinely more work than a crust one at the same resolution and the *reason is not
measured*. Before anyone quotes a cause, re-run `measure_drag_frame_stages` on a solid fixture; the
existing breakdown is a crust breakdown and this document has already been burned twice by treating
a crust number as general. Do not assume it is the seal.

Gates: `incremental_engages_on_a_closed_solid` (new; must go incremental AND match the stateless
reference bit-for-bit) paired with the pre-existing `incremental_refuses_fields_with_sealed_voids`.
Both directions pinned. `cargo test -p io_bridge` green including the band golden gates.

## 8b. The solid stage breakdown — measured 2026-08-10, and it re-derives the GPU case

`measure_drag_frame_stages` now runs BOTH fixture classes (the crust path is byte-identical to
before, so §12 of the octree plan stays comparable). Median of 5, release, res 256:

| stage | crust | solid | ratio |
|---|---|---|---|
| frame | 25.3 | **62.6** | 2.5× |
| S1 seal | 4.0 | 4.2 | **1.05×** |
| S2 extract | 11.9 | **25.2** | 2.1× |
| S3 reproject | 1.3 | 12.7 | **9.8×** |
| S4 `from_mesh` | 10.6 | 22.8 | 2.2× |
| output verts | 73 748 | **161 810** | 2.2× |

**The 2.5× gap is output size, not algorithmic difficulty.** The solid emits 2.2× the surface, and
S2/S4 track it almost exactly (2.1×, 2.2×). S1 is flat because the whole-field seal is
output-independent — it costs the same 4 ms whatever comes out. Nothing here is pathological; a
solid simply has more surface than a flat sheet at the same lattice.

**S3 is the exception and is NOT explained.** It grew 9.8× on a 2.2× output — superlinear. Note it
is *not* brute force: `project_points_to_mesh` builds a uniform bin grid
(`bin = max(max_dist, extent/64)`), so the obvious guess is wrong and was checked before being
written here. Cause unknown; it needs its own measurement before anyone optimises it. At 12.7 ms on
a 2 300-triangle source it is not yet urgent, but the growth curve is the thing to establish,
because a real asset's source mesh is far denser than this fixture.

### What this does to the GPU argument

Of a solid's 62.6 ms, the plan's Gates 2–4 address **S1 + S2 = 29.4 ms (47%)**. S4 (22.8) stays CPU
by the stack contract; S3 (12.7) is a mesh spatial query and is not in the GPU plan's scope.

- now: **62.6 ms**
- + Gate 1 (S4 CSR — if the solid's S4 has the crust's 61% twin-link share, ~14 ms addressable): ~48 ms
- + Gates 2–4 (GPU S1+S2): **~19 ms + 1–2 frame latency**

So ~3.3× overall, not the ~5 ms the crust-based §7 projected. **But the GPU case is stronger in
relative terms than it was**: S2 extract is now the single largest stage on a solid at 25.2 ms
(40% of the frame), and S2 is precisely the stage that already has a tested GPU implementation in
`sdf_mesh.wgsl`. The plan's weakest assumption was never S2 — it was the sign flood, which Gate 0
already closed off, and which at 4.2 ms is now the *smallest* stage and barely worth moving.

**That last point deserves a decision.** If S1 is only 4.2 ms, option 3's hybrid (keep the flood on
the CPU, GPU the scatter and extract) costs almost nothing in CPU time and avoids the entire GPU
connectivity problem. Gate 2 should be re-scoped around that before any WGSL is written.

### Revised outcome table (§7 superseded)

| | solid @256 drag frame |
|---|---|
| master today | not measured; incremental was inert there too, so ≥ the 191 ms figure |
| octree branch, before Gate 0.5 | 191.1 ms |
| **after Gate 0.5 (here)** | **61.7 ms** |
| + Gate 1 (S4 CSR) — **done, measured** | **58.2 ms** |
| + Gates 2–4 (GPU S1+S2 = 28.5 ms) | ~30 ms + 1–2 frame latency |

Re-derived from the solid breakdown in §8b, not from the crust split. ~3.3× overall.

## 8c. Gate 1 — CLOSED 2026-08-10. Real, but ~40% of the predicted win.

`from_quads_with_materials` and `from_triangles` now twin-link through one shared `TwinLinker`
(`core_math/src/half_edge/builders.rs`): out-degree count → prefix-sum CSR → short scan of
`bucket[dst]`. One implementation for both constructors, so they cannot drift on non-manifold
pairing. `from_polygons` keeps the packed-key map — it needs a MULTI-map, a different problem.

| | S4 before | S4 after | frame before | frame after |
|---|---|---|---|---|
| 256 crust | 10.6 | **7.9** | 25.3 | **22.9** |
| 256 solid | 22.8 | **17.3** | 62.6 | **58.2** |
| 128 solid | 5.4 | 4.9 | 19.2 | 18.0 |

**Octree plan §12f predicted S4 9.9 → ~4 ms by removing a 6.0 ms twin-linking loop. Actual: −25%,
not −60%.** So roughly 2.7 of the claimed 6.0 ms was recovered.

**Hypothesis, NOT verified — do not act on it without measuring first.** `TwinLinker::new` allocates
and initialises two `Vec<u32>` of 4·Q entries: for the solid that is 647k slots × 2 × 4 B ≈ **5.2 MB
of per-call scratch, memset every call**. That is exactly the cost class that dominated three
earlier stages in the octree plan (§12a bitset, §12b the 66 MB `cell_vert` table, §12f itself), and
the pattern there was always that the named algorithm was not the cost. If someone picks this up:
`dst` is redundant — a bucket entry's destination is already `mesh.he(id).vertex`, so the array
could be dropped entirely for a borrow restructure, halving the scratch.

Gates: `twin_linking_semantics` 7/7 (the adversarial non-manifold pins — three-faces-on-one-edge,
duplicate directed edge, four half-edges, two-forward-then-two-reverse, degenerate self-loops,
first-outgoing vertex half-edge). Every core_math integration target green including
`poly_normal_bake_dump`, `poly_smooth_parity`, `regional_extract_parity`, `serde_golden`,
`manifold_grid_preserve`. core_math lib 1034 pass / 3 fail and app 701 pass / 5 fail — identical
counts to before the change; all 8 are pre-existing on `b924432` (verified by running them there).

## 9. Session log

- **2026-08-10 — plan authored, toggle landed.** Worktree + branch created off
  `feat/remesh-adaptive-octree` @ `b924432`. `ExperimentalFlags::gpu_remesh` added (default OFF,
  `#[serde(default)]`) with an Experimental-window checkbox. Baseline in §2 read from the octree
  plan's §12f, not re-measured. **Correction carried in from the conversation:** an earlier estimate
  in this thread used 49.9 ms as the baseline, which was stale by four commits (`da65fbb`,
  `9aa2a4a`, `e45aa4c`, `b924432` had already landed) — the live figure is 24.9 ms, which cuts the
  GPU-only win from ~34 ms to ~16 ms and is why Gate 1 is in scope.
  No measurement run yet; Gate 0 is open.
- **2026-08-10 — GATE 0 CLOSED, and it re-sequenced the plan.** Full results in §8. Option 1 (GPU
  iterative propagation) is dead on propagation depth (42-282 passes @256). More importantly the
  probe found `REMESHSOLIDNOINCR`: incremental remesh never engages on a closed solid, so the
  24.9ms baseline this plan was justified against is a crust measurement and the real solid drag
  frame is 191ms. New Gate 0.5 inserted ahead of everything; §7 outcome table void until it lands.
  Probes only, no production code touched.
