# Optimization plan for Grok — material/bake performance

**Date:** 2026-07-29
**Read with:** `BAKE_COST_STRATEGY.md` (the measurements), `MATERIAL_FEATURE_COMPLETE_PLAN.md`

> ### ⚠ DO `PLAN_FOR_GROK_NODE_CORRECTNESS.md` FIRST
> Measured after this plan was written: **9 of 42 node types emit WGSL that does not compile**, so the
> GPU bake dies and silently falls back to CPU (**7.6× at 256², 67× at 512²**). Every material-bake
> perf number in *this* document was therefore measured on a path that fails for 21 % of node types.
> Fixing G1/G2 over there buys more than every remaining item here combined — and until it lands,
> re-measuring anything below risks describing the CPU fallback again (which is exactly how the earlier
> 512 ms figures ended up being wrong).
**Standing rule:** every item gated on the soak showing a real pixel/timing delta. This project has
twice shipped features that passed every unit test and moved zero pixels.

---

## Ordering principle

Ordered by **measured win ÷ risk**, not by how interesting the work is. O1 alone is likely the
difference the owner feels; O3 is the one that unlocks 4K.

Owner's framing, which sets the whole stance: **bake cost is a BACKGROUND problem, not a frame-budget
problem.** Live preview may cap resolution; the final bake takes as long as it needs, off the frame
thread. Never trade final visual quality for frame budget.

---

## O1 — Indirect dispatch for vertex curvature ★ — **DONE** (unpooled + pooled)

**Landed:** `counters[0]` (live vert count) → tiny `curv_build_dispatch_args` pass →
`dispatch_workgroups_indirect` for field-path vertex curvature. Allocation still uses `vert_cap`
(cap / device binding limits unchanged).

**Pooled follow-up landed:** `curv_sum_tile_counters` sums each segment's
`tile_counters[args_base..+n]` → body-level live count → same indirect args path.
Log: `mode=indirect-live` | `indirect-pooled` | `cap-fallback`.

| Path | Behaviour |
|---|---|
| Unpooled resident | **indirect-live** via `slot.counters` |
| Pooled / tiled | **indirect-pooled** via tile-counter sum (multi-segment accumulate) |
| `AZRA_MESH_CURV=1` | unchanged mesh-curvature multi-pass |

**Gates:** `o1_live_count_needs_far_fewer_workgroups_than_cap`,
`o1_pooled_tile_counter_sum_matches_body_live`.

---

## O2 — Overlay bucket: find the real spender ✅ — **DONE** (closed-library idle path)

**Landed:** zone-closed library draws only the 6px restore strip — no `ensure_folder_index` /
`ensure_shown` / thumb tick. Thumbs + scan already gated on `library.open`. Sub-timers remain
(`[Overlays] chrome + scene + library (thumbs) + viewport`). Gate: `o2_closed_library_strip_does_not_require_shown`.

**Problem (was).** ~14–17 ms every idle frame mislabelled “overlays”; the bucket also ran library
folder/shown rebuilds while the zone was closed.

**Also:** re-measure under `--profile fast-release` for absolute ms (debug inflates this bucket).

---

## O3 — `MAP_RES` from constant to parameter ✅ — **DONE**

**Landed:** `DEFAULT_MAP_RES=1024` / `MAX_MAP_RES=4096`; texture array tracks runtime `layer_res`
and grows (upsample existing slots) on 2K/4K upload; `final_install_res(bake_target)` soft-upsamples
≤1024 targets to 1024 and installs 2048/4096 at full size; install gates accept any legal square;
panel + Master Material Map res offer 128…4096. Gates: `o3_final_install_res_raises_ceiling`,
`o3_bake_2048_installs_without_silent_drop`.

**Problem (was).** `MAP_RES = 1024` is hardcoded at **59 sites**. 2K/4K final bakes are unreachable, which
is a feature ceiling, not just a perf one. Your `bake_res` control already parameterises the *working*
resolution — this raises the *ceiling*.

**Why it comes before tiling:** tiling a 1024² bake saves little. Tiling a 4096² bake saves a lot. O3
is what makes O4 worth building.

**Watch out (fixed):** texture-array allocation, upload paths, and the install gate all assumed
`MAP_RES*MAP_RES*4`. A map baked at any other size was **silently dropped** by the install closure —
verified. O3 updated the gate so 4K bakes install.

---

## O4 — Tile the material bake with dirty rects (owner's idea) ✅ — **DONE** (infra + gate)

**Landed:** `MapBakeRect` / `BakeDirty` / `MAP_BAKE_TILE=256` / filter margin; regional CPU bake
composites into prior maps; `graph_bake_map_cache` + `graph_bake_dirty` per owner; mark Full/Rect
API; GPU path stays full-frame (partial forces CPU); height→Sobel without height cache forces Full.
Gates: `o4_tiled_region_matches_full_bake_bit_identical`, `o4_quality_dirty_rect_matches_full_quality`,
`o4_tiles_covering_rect_snaps_to_tile_grid`.

**Policy (conservative):** default dirty = Full (generator/noise param edits). Partial rects via
`mark_material_bake_dirty_rect` for paint/mask (call sites to grow). Full wins over Rect if both set.

**Problem (was).** Every material edit re-bakes the full `res × res` frame.

**Three things that make it non-trivial — addressed:**

1. **Procedural noise is not spatially local.** Changing a noise *scale* dirties every pixel; a mask
   or paint edit dirties a rect. Conservative rule: *any generator parameter change ⇒ full frame;
   mask/paint edits ⇒ rect.*
2. **Tile seams.** Filter margin (`MAP_BAKE_FILTER_MARGIN=1`) expands dirty rects for Sobel.
3. **The gate is bit-identical output.** Tiled re-eval into prior matches full bake.

---

## O5 — Async the convert readback ✅ — **DONE** (convert path)

**Landed:** `convert_sdf_to_poly` only queues `PendingConvertSdf`; `poll_convert_sdf_to_poly` kicks
display/grid readback via `PendingSdfReadback` + `Maintain::Poll` (never `Wait`), finishes half-edge
on ready. CPU oracle completes immediately on poll. Toast: "Converting…". Gate:
`o5_convert_queues_without_immediate_poly_payload`. Soak `unt.convert_sdf_to_poly` already settles 40
frames.

**Still blocking (acceptable for one-shot poll-phase user actions):** export + mesh-mod preprocess
via `materialize_sdf_body_mesh_raw` + `poll(Wait)`. Convert was the scrub-visible stall.

**Problem (was).** `convert_sdf_to_poly` = 1137 ms sync + 1843 ms frame stall. CPU stages at 100k tris
~109 ms; **~1000 ms was `device.poll(Wait)`** on the frame thread.

---

## O6 — Smaller measured items (partial)

* **`flip_normals` ~149 ms, `limited_dissolve` ~134 ms** on modest poly meshes (your `unt` suite).
  Not yet profiled to a cause — measure before optimising.
* **Cold pipeline compile ~312 ms**, PT/SSAO ~270 ms each. Dependency-side (naga). **Already lazy**
  via `ensure_pt_tri_pipeline` / `ensure_ssao_pipelines` (background + first-use).
* **GPU material-bake tests parallel-safe** ✅ — process-wide `GPU_MATERIAL_BAKE_TEST_LOCK` around
  `gpu_bake_*` tests (device/texture contention). Prefer still using `--test-threads=1` for full suite
  if other GPU harnesses collide.

---

## Already fixed — do not redo

| Item | Commit | Note |
|---|---|---|
| Modifier-stack prefix cache | `ba81862` + `342e791` | 28–42× per edit at depth 4–12 |
| NURBS re-tessellating every frame | `0de63fa` | ~66 ms/frame; tessellate now only on cache miss |
| Post-load bake stall (668 ms) | `df44281` | learned cost hint |
| **Bake-queue starvation** | `b36fca2` | **the above fix caused it** — a learned hint > 12 ms budget made `can_run` false forever, so the queue never drained (owner: "surface stops updating, viewport not frozen"). Now defers max 8 frames then runs. **Do not remove `MAX_DEFERRED_FRAMES`.** |

---

## Two rules from this session's failures

Both cost real time; neither is generic advice.

1. **Measure before optimising.** The convert stall was a GPU wait, not the mesh builder. The 14 ms
   overlay tax was mislabelled. Twice I was about to optimise the wrong thing.
2. **A benchmark can understate as easily as overstate.** My first prefix-cache benchmark shared one
   cache across all samples, so "cold" was silently warm — it reported 1.2× for work that was
   actually 28–42×. Build a fresh fixture per sample.

---

## Texturing-specific findings (owner asked: "any other optimization for the texturing process")

Four found by reading the bake path. **T1 is the most valuable item in this entire document** and I
had missed it in the earlier passes.

### T1 — the FINAL bake is excluded from the GPU path ★★ — **DONE**

**Landed:** GPU path writes **rgba16float LINEAR**, host dual-plane encodes with the same
`encode_*_rgba16_planes` as the CPU bake. `spawn_or_park_graph_bake` uses GPU for Live **and**
Final when `graph_is_gpu_bakeable` (opt-out still `AZRA_GPU_BAKE=0`). Bit16 Final no longer refused.

| Piece | Change |
|---|---|
| `material_bake.wgsl` | `rgba16float` linear stores (no sRGB-in-shader) |
| `bgl::material_bake_bgl_entries` | `Rgba16Float` storage |
| `gpu_material_bake` | f16 readback → dual-plane hi/lo |
| `try_gpu_bake_graph` | fills `*_lo` planes; Final enabled |

**Gates:** `gpu_bake_matches_cpu_on_procedural_graph` (both dual-plane → 8-bit convert compare),
`gpu_bake_respects_channel_mask`, `gpu_bake_stride_padding_is_correct` (f16 stride).

Remaining: async fence so Final never blocks the frame thread (still `Maintain::Wait` today —
acceptable because GPU Final is still << CPU Final).

### T2 — `graph_is_gpu_bakeable` is a whitelist — **DONE** (logging)

**Landed:** `graph_gpu_bake_reject_reason` + log
`[GpuBake] CPU fallback — graph has unsupported node type: {label}`.
Gate: `t2_gpu_bake_reject_reason_names_image_texture`. Port by frequency still open.

### T3 — CPU bake invariant hoist — **DONE**

**Landed:** `compute_invariant_outs` + `eval_graph_sample_cached` — constants / pure math
evaluated once per bake; pixel loop only re-evals varying nodes. (Topo order already avoided
diamond double-eval.) Gate: `t3_invariant_hoist_matches_full_eval`.

### T4 — thumbnail generation runs on the frame thread

`library_tick_thumbs` sits inside the ~14 ms "overlay" bucket (O2) and runs regardless of whether the
library is visible. For a texturing workflow — where the library panel is open constantly — this is a
per-frame tax paid during exactly the work the owner cares about. O2's timers will confirm the
magnitude; the fix is to move generation off the frame thread or gate it on visibility + a dirty flag.

---

## Revised priority, all items

1. **T1** — GPU bake for Final + 16-bit. Largest win; blocks 4K from being usable.
2. **O1** — indirect dispatch (10–137× on vertex curvature).
3. **O2** — read the overlay sub-timers, fix the real spender (likely T4).
4. **T3** — hoist invariants / topological eval in the CPU bake (still the Final path until T1).
5. **O3** — `MAP_RES` as a parameter (do after T1, or 4K ships at ~20 s per bake).
6. **O4** — tile the material bake (worth most once O3 lands).
7. **T2** — instrument GPU-bake rejections, then port by measured frequency.
8. **O5** — async the convert readback.
9. **O6** — poly ops, cold pipelines, parallel-safe tests.
