# Live Prim-Move Perf — full rebake per edit is too slow (LOGGED 2026-07-02, fix TOMORROW)

**Status: DIAGNOSED 2026-07-03 (owner [BAKEPROF] capture) + FIRST FIX LANDED (perf/viewport).**
Owner: "the live feeling is still not there — full rebake for every primitive, too slow." Do NOT
rush a fix; diagnose the partial-vs-full bake path first.

## DIAGNOSIS (2026-07-03 — measured, owner drag capture with [BAKEPROF] ON)
One-prim diagonal drag (-x/+y), 128³ grid: **22 dispatches = 16 partial + 5 escape→full + 1 settle-full.**
- **H1 escape→full: CONFIRMED, dominant** (~23% of dispatches were whole-grid bakes). Smoking gun:
  escapes 3/4/5 fired on **tiny overshoots (0.037 / 0.038 / 0.042)** on the OTHER axis of the diagonal —
  because `ESCAPE_HEADROOM` was granted only on the axis+side that already escaped, leaving the second
  axis ~0.01-0.07 from the wall. **3 of 5 full bakes were these wasted re-escapes.**
- **H2 cascade doubling: RULED OUT** — `first(cascade)` fired exactly once (fresh body); `draft=false`
  on all edit dispatches. The 64³+128³ double-upload happens on first bake only.
- **H3 select rebakes: RULED OUT** (when a grid exists) — subsequent selects produced zero bakes. The
  57ms "select bake" was the first-ever bake of a never-baked body (now async GPU cascade).
- **H4 whole-body partial cost: not the bottleneck at 1 prim** (regions=1, partials under 5ms).
- Secondary: `[TriggerBake] 5.5-6.4ms` on escape frames is trigger-thread overhead dominated by the
  multi-line console I/O ([DirtyFallback]+visrec) on those frames — the bake itself is async GPU.

## FIX 1 (landed, perf/viewport): predictive multi-axis escape headroom
`compute_escape_grid_bounds()` (pure fn, bake_dispatch.rs) — on escape, headroom is granted not only
on escaped sides but on any side the tight bounds are **moving toward** (delta vs `prev_tight`, the
previous dispatch's tight bounds from `partial_tight_bounds`) that is **within one headroom of the
wall**. Trailing sides/static axes never granted (anchor never chases the subject — the reverted
re-centering dead-end); `MAX_GRID_MULT` 1.35 cap unchanged (trim prefers non-granted sides).
**Capture-replay unit tests** (`cargo test -p app escape_policy`) assert: bit-parity with legacy when
no movement info; 5 escapes → 3 on the exact captured trace; containment + cap invariants; trailing
sides never granted. Remaining escapes (~1 per 0.3 body-travel) are the cap-mandated cadence.

## FIX 2 (proposed, NOT started): voxel-aligned scrolling escape ("clipmap shift")
The cap fundamentally forces one full 128³ re-fill per ~0.3 body of travel. Endgame: on escape,
TRANSLATE the grid window (voxel-size-preserving, voxel-aligned shift → old content stays exact,
no resampling), GPU-copy the overlapping region, partial-bake only the newly exposed slab + prim
region. Turns every remaining escape-full into partial-sized cost — the doc's goal ("NO full-body
rebake") verbatim. Needs: a GPU shift-copy pass + provenance care (f430bec) — gate behind owner
go-ahead + FEATURE_RULES questions.

## Symptom
Moving / duplicating / selecting a primitive does not feel live. The surface updates in laggy jumps,
not continuously, because **every prim edit re-bakes the WHOLE body** (all prims), and the resolution
cascade runs it TWICE (draft 64³ then full 128³) per edit.

## Evidence (this session's run, Body.001 = NodeId(1))
- On select: `[TriggerBake] 57.3ms dirty=1 sync_baked=1` — a 57ms bake just to select.
- During duplicate/move, a long burst of FULL bakes, repeated per edit:
  ```
  [VolumeUpload] body=NodeId(1) full (res=64³)
  [VolumeUpload] body=NodeId(1) full (res=128³)
  ... (repeats many times as you edit) ...
  ```
- Each edit path fires either `[DirtyFallback] reason=aabb_escape(bounded)` (bounds grew → the escape
  CLEARS dirty_regions → forces a FULL bake) or `[Retighten] settle → full re-tighten bake`.
- `content_bounds_ok` toggles true/false but is no longer a gate (ce9cd42) — informational only now.
- Field-direct IS engaged (`field_direct=true` throughout) — so the bottleneck is the BAKE cost, not
  the mesh/upload path. Full bakes are `full (res=64³)` + `full (res=128³)` = two whole-grid bakes.

## Why it's full every time (hypotheses to verify tomorrow)
1. **Escape → full bake.** `aabb_escape` (bake_dispatch.rs:479) does `dirty_regions.remove(body)` →
   the next bake is FULL, not partial. Duplicating/moving a prim grows the body AABB → escape → full.
   With the tight escape caps (aca0238: headroom 0.15), a moving prim re-escapes often → full bake
   often. Look at whether a prim that stays INSIDE the grid still does partial (it should) vs the
   escape frequency being too high.
2. **Cascade doubles it.** Each dirty triggers draft 64³ THEN full 128³ (two `[VolumeUpload] full`).
   Confirm the cascade (bake_dispatch.rs ~604-620, `!has_existing_grid` gate) isn't re-running the
   64→128 cascade on every edit when a grid already exists.
3. **Select triggers a full bake.** `select_single` → `[TriggerBake] 57ms sync_baked=1`. Selecting a
   prim should NOT rebake the body. Check what marks the body dirty on selection.
4. **Whole-body cost.** Even a correct partial bake is over the whole body's grid region; as prims
   multiply, the per-edit cost grows. Partial bake should touch only the moved prim's dirty AABB
   (mark_prim_dirty_aabb → partial region), NOT the full grid.

## Goal
A prim move should be a fast INCREMENTAL update: partial-bake ONLY the moved prim's dirty sub-region
into rv.buffer in place (field-direct already renders it live), with NO full-body rebake and NO
draft→full cascade per frame. Full/retighten bakes stay for settle only. Target: sub-frame per-edit
bake so dragging a prim is continuously live like before.

## Where to look
- `app/src/body_volume_cache/bake_dispatch.rs` — partial vs full decision, `is_draft`, `has_partial_regions`,
  the `aabb_escape` full-bake conversion (:479), the cascade (:604), escape caps (:442-443).
- `app/src/body_volume_cache/dirty.rs` — `mark_prim_dirty_aabb` (partial region insertion) vs
  `mark_body_dirty` (full). Which does a prim MOVE use? Which does SELECT use (should be none)?
- `app/src/prim_actions.rs` — `write_prim_transform` (prim move → dirty path), duplicate (~760).
- The escape headroom/cap tension: tighter caps (no balloon) → more escapes → more full bakes. May
  need a smarter escape (bigger headroom for the moving prim + keep the balloon cap) OR keep partial
  bakes across small escapes instead of converting to full.

## Related open item (separate)
Residual "a bit of stretching" = dense-grid UNDER-SAMPLING of prims spread across one body at fixed
res. Real fix = sparse SDF bake (Phase 3, resolution-architecture plan). Not the same as this perf item.

## Diagnostic instrument (added 2026-07-03, perf/viewport)
A runtime-gated `[BAKEPROF]` marker now classifies every bake dispatch so the partial-vs-full path
can be MEASURED before cutting. Toggle: **Developer Tweaks → Profiling → "Bake path profiler"**
(runtime, default OFF, no recompile). Field: `BodyVolumeCache::bake_prof`; emit site is
`bake_dispatch.rs` right after `res3` is finalized.

Per body per dispatch it prints:
```
[BAKEPROF] body=NodeId(N) path=<partial|escape->full|full|first(cascade)> res=AxBxC draft=<bool> regions=<n> escaped=<bool> prims=<n>
```
- `path=escape->full` = hypothesis 1 firing (aabb_escape cleared dirty_regions → whole-grid bake).
- `draft=true` (64³) then `draft=false` (128³) back-to-back = hypothesis 2 (cascade doubling).
- `path=full` with no prior region = hypothesis 3 (e.g. select rebaking) — cross-check with `[TriggerBake]`.
- `res` + `regions` = hypothesis 4 (whole-body cost even on partials).

**Method:** turn it ON, drag one prim across a body, count `partial` vs `escape->full` lines. The
escape frequency is the number that justifies the fix (raise moving-prim escape headroom / keep the
partial path across small escapes). Reports the PATH chosen on the trigger thread — the GPU bake wall
time still lands async in `poll_primary_bake`.

## Do NOT regress
Flat slab (atlas grow-cooldown, 3a97dd8), realloc provenance (f430bec), sub-body resolution keyed on
top-level (7051805), field-direct liveness gate (ce9cd42). Verify these still hold after any perf work.
