# Console Audit Fix Plan — 2026-08-06

**Source:** owner's full console capture, 2026-08-06 session (build warnings + runtime log,
RTX 4090 / Vulkan, `fast-release` profile).
**Companion plans:** `docs/planning/render/FRAME_STALL_PLAN_2026-08-05.md` (Gates 0–2 done;
this plan continues it), `docs/planning/DEAD_CODE_STRIP_PLAN.md` (owns the warning debt;
Phase D below feeds its owner-gated slices).

Read `CLAUDE.md` first. One task per session, one bug per task, `cargo build -p app` after
every change, update `COMPLETE_BUG_TRACKER.md` when a task closes (✅ FIXED + date; add new
entries for A1/B1/C1 if untracked).

## Status board

| Task | What | Class | Status |
|------|------|-------|--------|
| A1 | Poly Extrude drag ~6 FPS — diagnostic | P1 perf bug | DONE 2026-08-06 |
| A2 | Poly Extrude drag — fix (double update path) | P1 perf bug | DONE 2026-08-06 (owner smoke still useful) |
| B1 | `POLYGON_MODE_LINE` never requested | Confirmed bug | DONE 2026-08-06 |
| B2 | Pipeline cache: 7× identical 37 MB writes/launch | Perf/robustness | DONE 2026-08-06 |
| B3 | RT-shadow lazy compile = 91 ms main-thread hitch | Rule violation | DONE 2026-08-06 (main pass; denoise still lazy) |
| B4 | Log hygiene batch (trunk "no debug logging" bar) | Cleanup | DONE 2026-08-06 |
| C1 | Library 427 ms single-frame catastrophe | P2 perf bug | DONE 2026-08-06 (amortized shown scan) |
| D1 | Unwired-feature decision matrix (owner) | Product gate | OWNER |
| E1 | `ssgi_view_owned` noop `.clone()` | One-liner | DONE 2026-08-06 |

Ordering: A1 first (biggest user-facing pain, and A2's shape depends on it). B1–B4 are
independent of A and of each other — any can run in parallel sessions. C1 after B4 lands
(cleaner logs make the repro readable). D1 is an owner decision session, not agent work.

---

## The evidence (owner's 2026-08-06 log, verbatim)

```
[STALL] frame=159ms | layout=156.5 input=0.1  ... | tool=Poly Extrude
[STALL] frame=239ms | layout=158.1 input=78.5 ... | tool=Poly Extrude
[STALL] frame=160ms | layout=78.4  input=79.0 ... | tool=Poly Extrude
[STALL] frame=160ms | layout=78.2  input=79.6 ... | tool=Poly Extrude          (repeats)

[LIB-BODY] folder_col=182.4ms seam=0.0ms items=245.0ms total=427.3ms (tree_rows=130 shown=2278)
[STALL] frame=1434ms | dispatch=965.8(submit=0.1) overlays=466.3 ... | tool=Idle
  (~100 preceding frames: library paint 1.0–2.6ms, same 130 rows / 2278 items)

[CACHE] Saved 36940442 bytes to ...pipeline_cache.bin      (×7 in one launch, size never changes)

[TIMING] RT-shadow pipeline compiled (lazy): 90.6013ms
[SPIKE] poll_background_tasks: 91.4ms

[GPU] POLYGON_MODE_LINE: false — DC wire falls back to CPU edges   (on an RTX 4090 / Vulkan)

[TIMING] DF-lite pipeline compiled synchronously (deferred ready for GLSL BVH)   (path deleted 2026-07-14)
[Azra] Viewport active — GLSL BVH ready, WGSL compiling in background            (same)

warning: `app` (bin "app") generated 427 warnings
```

Healthy baseline for contrast: 53.8 ms total startup, 14 background pipelines warm in 303 ms,
bevel drags manifold-clean at every width, library steady-state ~1–2.6 ms (Gate 2 of the
2026-08-05 plan held).

---

## Phase A — Poly Extrude drag runs at ~6 FPS

### A1 — Diagnostic (DO THIS FIRST, no fix in this task)

**Problem.** Steady-state `frame≈160ms` with `layout≈78` **and** `input≈78` every frame of an
extrude drag. Two different phases, near-identical cost, stable across frames. `mesh=0.0`,
`overlays≈0.6` — the raster sync and overlay buckets are NOT the cost.

**Why a diagnostic, not a fix.** The 2026-08-05 plan's Gate 1 (lazy smart-snap occlusion)
supposedly brought `input` under 5 ms for bevel/gizmo drags — yet extrude shows 79 ms. Either
the extrude path dodges that fix or this is a different cost. CLAUDE.md: if you can't state
the root cause with confidence, diagnose first.

**Hypotheses to test, in order:**

1. **Pick-cache double rebuild.** The drag path calls `poly_invalidate_pick_cache()` every
   frame (`extrude_drag.rs:2070` shows the pattern; the live drag path does the same). If the
   cache rebuilds lazily on next query, and it is queried once during input (hover/pick) and
   once during layout (panel or overlay), that is **two full rebuilds per frame** — matching
   the twin ~78 ms signature exactly.
2. **Smart snap not covered.** `poly_update_extrude_drag` (`extrude_drag.rs:896`, called from
   `input_viewport.rs:2231/:2484` and `input_keyboard/handle.rs:1035`) may hit
   `find_smart_snap` on a path Gate 1 didn't patch.
3. **Per-frame push-through boolean.** `poly_extrude_try_push_through` runs a mesh boolean;
   if probed every drag frame rather than on crossing, that's tens of ms in input.
4. **Layout-side panel work.** `tool_params.rs` extrude section is `.changed()`-gated (checked
   2026-08-06 — does NOT fire during a viewport drag), so if layout is hot it is something
   else drawn during panels. Time the panel pass to find it.

**Task.** Add scoped timers (existing `cpu_section_ms` pattern) inside:
`poly_update_extrude_drag` (sub-split: snap / apply_delta / push-through probe / pick-cache /
display-sync), the pick-cache rebuild itself (count rebuilds per frame + caller), and the
layout pass when `tool=Poly Extrude`. Print one `[EXTRUDE_PERF]` line per slow frame.

**Acceptance.** One log line attributing ≥90% of both the 78 ms input and the 78 ms layout
buckets to named functions, plus a one-paragraph root-cause statement. No behavior change.

### A2 — Fix (scope set by A1's finding)

Likely shapes, per hypothesis: (1) rebuild the pick cache at most once per frame per
invalidation generation (generation counter, per CLAUDE.md GPU-lifetime rule — same pattern,
CPU-side), never twice; (2) extend Gate 1's lazy occlusion to the extrude path; (3) probe
push-through only when the offset sign/interval crosses the surface, not every frame.

**Do NOT:** throttle to every-Nth-frame, skip work while "interacting", or cache across
frames keyed on mouse position — the 2026-08-05 plan already bans all three for this class.

**Acceptance.** `input < 5ms` and `layout < 10ms` steady-state during an extrude drag on the
same scene (paste before/after `[STALL]`/`[EXTRUDE_PERF]` lines). `cargo test -p app` and
`-p core_math` green. Bug tracker updated.

---

## Phase B — Confirmed quick wins (independent; one per session)

### B1 — `POLYGON_MODE_LINE` is never requested at device creation

**Root cause (CONFIRMED).** `gpu_init/mod.rs` (~lines 270–330) adapter-gates and requests
TIMESTAMP_QUERY, SPIRV_SHADER_PASSTHROUGH, FLOAT32_FILTERABLE, MULTI_DRAW_INDIRECT(_COUNT),
INDIRECT_FIRST_INSTANCE, CLEAR_TEXTURE, SHADER_INT64(+atomics) — but **never**
`Features::POLYGON_MODE_LINE`. `render/src/pipeline.rs:329` then checks
`device.features().contains(POLYGON_MODE_LINE)`, which can never be true, so
`wire_pipeline`/`wire_pipeline_front` are `None` on **all** hardware and DC wireframe always
takes the CPU-edges fallback. NVIDIA Vulkan supports it; the log's `false` on a 4090 is the
smoking gun.

**Task.** Add the adapter-gated request in `gpu_init/mod.rs` next to its siblings:
```rust
if adapter.features().contains(wgpu::Features::POLYGON_MODE_LINE) {
    features |= wgpu::Features::POLYGON_MODE_LINE;
    eprintln!("[GPU] POLYGON_MODE_LINE: true");
}
```
No other change — `pipeline.rs` already handles both branches (fails closed).

**Acceptance.** Log shows `POLYGON_MODE_LINE: true` and DC wire renders via the GPU pass
(owner smoke: wireframe display mode on an SDF body). CPU-edges fallback still works when the
feature is absent (don't delete it). Feature Dependency Audit note: enabling a feature adds
nothing to remove, but verify the wire pass's G-buffer writes still respect the Display
Invariant (same depth convention — it draws into the same G-buffer, so yes by construction).

### B2 — Pipeline cache: ~260 MB of identical disk writes per launch

**Root cause (CONFIRMED).** `save_pipeline_cache` (`gpu_init/cache.rs:37`) is called from 9
sites — after each pipeline batch (`app_update.rs:1345/1437/1470/1484/1569/1585/1642`,
`pipelines_bg.rs:355`, `main.rs:932`). Each call serializes the **entire** cache via
`get_data()` and rewrites all ~37 MB even when byte-identical: the logged size never changed
across all 7 saves this session.

**Task.** In `cache.rs`, remember the last-saved digest (length + cheap hash of `data`, e.g.
FxHash/xxhash over the bytes — an `AtomicU64`/`Mutex<Option<(usize,u64)>>` module-static is
fine; saves happen on two threads). Skip the write when unchanged, log
`[CACHE] unchanged — skip`. Keep the atomic tmp+rename write exactly as is (it exists for
crash-truncation safety — see the comment at `cache.rs:40`), keep the Tier-1 final save and
the exit save unconditional call sites (they'll self-skip when clean).

**Do NOT** remove the intermediate call sites — the comment at `app_update.rs:1481` explains
they protect an interrupted background compile. Skipping identical bytes preserves that.

**Acceptance.** A warm launch logs ≥5 skipped saves and ≤2 real writes; a cold launch (delete
the .bin) still persists correctly and relaunches warm.

### B3 — RT-shadow pipelines compile lazily on the main thread (91 ms hitch)

**Rule violated:** CLAUDE.md Frame Budget — "All pipelines compile on the background thread.
`WarmupManifest` tracks readiness. `ensure_*` are fallbacks only." The log shows
`RT-shadow pipeline compiled (lazy): 90.6ms` inside `[SPIKE] poll_background_tasks: 91.4ms` —
first RT-shadow use eats a frame.

**Task.** Add RT-shadow (+ its denoise pipelines, 1.2 ms) to the background compile set in
`pipelines_bg.rs` / the `app_update.rs` background thread, hot-swap like the other 14, keep
`ensure_rt_shadow_*` as the fallback. Mind the BGL Single Source rule — background thread must
use the same `*_bgl_entries()` the lazy path uses (grep before writing; report findings).

**Acceptance.** Log shows RT-shadow compiled (background) + hot-swapped; enabling RT shadows
first-time causes no `[SPIKE] poll_background_tasks` > 20 ms.

### B4 — Log hygiene (trunk bar: no debug logging)

One batch task, logging only, zero behavior change:

1. `[LIB]` / `[LIB-BODY]` thresholds: `library_panel.rs:544` and `:1401` fire at **> 1.0 ms**
   — every frame with the library open (it's most of the console). Restore a sane gate:
   `> 8.0ms || AZRA_PERF_SPAM=1` (mirrors the `[Overlays]` gate at `app_update.rs:847`). The
   1 ms threshold was a deliberate stall-hunt tool — keep it reachable via the env var.
2. Stale render-path messages: `app_update.rs:1353` ("deferred ready for GLSL BVH") and
   `:1902` ("GLSL BVH ready") describe a path deleted 2026-07-14. Reword to what is true
   (DF-lite ready / viewport active, WGSL compiling in background). Do not touch code.
3. `[BEVEL]` per-drag-step lines (`bevel` log in the drag path): gate behind
   `AZRA_PERF_SPAM` or a debug flag — 13 lines per width drag is debug output.
4. `[Render] frame=N ...` early-frame lines: keep frame 1, gate the rest behind the same flag.

**Owner action (not code):** `AZRA_KNIFE_DUMP` is set in the launch environment — every knife
action writes a repro JSON to `%TEMP%`. Unset it unless actively capturing knife repros.

**Acceptance.** Idle session with library open produces no per-frame log lines; stall/spike
diagnostics unchanged. `cargo build -p app` green.

---

## Phase C — Library single-frame catastrophe (427 ms)

### C1 — Find and amortize the one-frame rebuild

**Problem.** ~100 frames at 1–2.6 ms, then ONE frame at 427.3 ms (`folder_col=182.4` +
`items=245.0`) with identical content (130 rows / 2278 shown). Both sub-buckets exploded in
the same frame ⇒ a shared upstream invalidation — almost certainly the Gate-2 list cache
rebuilding synchronously in one frame (scan generation bump, filter key change, or thumb-batch
arrival invalidating the cache).

**Task.** (a) Log the cache key that changed when a rebuild fires (one line, gated > 8 ms).
(b) Reproduce with the existing headless harness (`library_panel.rs:2670`, the Gate-2 repro)
by forcing a generation bump. (c) Fix by amortizing: incremental rebuild with a per-frame cap
(`const MAX_LIB_REBUILD_ROWS_PER_FRAME`, Frame Budget Rule) or build the new list off-thread
and swap when ready (list data is CPU-only — no Frame Ordering Contract issue; do NOT touch
GPU resources from a panel).

**Do NOT** revive the ui2 idle-gating / cached-picture approach (banned in the 2026-08-05
plan, commit `8fc7936`).

**Acceptance.** Forced full rebuild (touch assets dir or bump scan generation at runtime)
never costs > 8 ms in any single frame; list correctness (filter/category/scroll) unchanged;
the headless harness numbers pasted before/after.

---

## Phase D — Unwired feature surfaces (OWNER decision session)

The 427-warning pile is mostly **features that exist but are unreachable** — the same class
`UNWIRED_ACTIONS.md` tracks from the trunk consolidation. Each needs wire-or-delete; deletes
route through `DEAD_CODE_STRIP_PLAN.md` (its rule 6 already marks these owner-gated), wires
are one-feature-per-session tasks under `FEATURE_RULES.md`.

| Surface | Evidence | Wire = | Delete = |
|---------|----------|--------|----------|
| Prim clipboard (`prim_actions.rs:1245` copy/cut/paste, `clipboard.rs` PastePosition variants) | never called → Ctrl+C/X/V on prims does nothing | Register 3 actions in `ACTION_DEFS` (`keybindings.rs`), dispatch via `kb.pressed`; **check chord collisions by hand** (`Ctrl+I` precedent); paste inside `transaction()`; follow Structural Mutation Checklist (adds prims to a body) | strip slice |
| Array duplicate (`array_dup.rs`, 6 methods + toolbar) | whole tool unreachable | wire `start_shift_dup_array` to its trigger + toolbar draw call | strip slice |
| Poly symmetry tool (`mirror_actions.rs:15`) | never called; likely superseded by Alt+X symmetry pie (shipped) | — | probable delete, owner confirms |
| Body warming (`warming.rs`) | never called | wire into scope-leave / MRU bake warmup | strip slice |
| PT adaptive budget (`viewport_render_mode.rs:19–193`, 5 fns/consts) | PT never adapts accumulation/denoise to frame cost | wire into PT dispatch via FrameScheduler | strip slice |
| FrameScheduler classes (`frame_scheduler.rs:7` Critical/High/IdleOnly, `WorkLogEntry.class`) | only Medium/Low flow through the scheduler CLAUDE.md says owns prioritization | classify existing per-frame work | shrink enum |
| `InputSystem` remnants (`input_system.rs`) | being retired per CLAUDE.md — expected | — | strip slice when retirement lands |

**Output of D1:** a filled wire/delete column, then one task per row.

---

## Phase E — Warning debt residue

- **E1:** `viewport_render.rs:947` — `.clone()` on `&TextureView` is a noop (copies the
  reference; `TextureView: !Clone`). Remove the call and rename `ssgi_view_owned` →
  `ssgi_view` so the name stops claiming ownership it doesn't have. One-liner.
- Everything else stays with `DEAD_CODE_STRIP_PLAN.md`'s slices — do not blanket
  `cargo fix`; many warnings are Phase D product decisions.

---

## Constraints and traps (all tasks)

- **Branch discipline:** all of this is non-UI work — branch off `master`, merge back with
  review. None of it touches `ui2/` (B4's library thresholds live in `library_panel.rs`,
  which is the panel body, not the v2 shell chrome).
- **Concurrent sessions are live** — the working tree has ~50 modified files including
  `extrude_drag.rs` and `poly_knife.rs`. Stage files **explicitly**, never `git add -A`;
  test failures in another session's files are theirs — confirm before claiming a regression.
- **No bandaids** — every fix here must pass CLAUDE.md's 5-question test; A2 and C1 in
  particular must fix the invalidation/ordering, not add throttles or frame skips.
- **Measurement discipline** — same scene, warm cache, before/after lines pasted. Never
  report a speedup against a cold first frame (the 965 ms dispatch on frame 2 is warmup, not
  a bug — do not "fix" it).
- **Definition of Done** per CLAUDE.md + `COMPLETE_BUG_TRACKER.md` update per task.
