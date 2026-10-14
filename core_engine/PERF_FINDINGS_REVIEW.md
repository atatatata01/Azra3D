# PERF_FINDINGS review — two corrections before acting on it

**Date:** 2026-07-29
**Reviews:** `PERF_FINDINGS.md` (f7d1d13)

The measurement process was right — production APIs, real console profiling, mined spikes. Two
things must be corrected before anyone spends a day optimising, because both change what the
numbers mean.

## Correction 1 — the run was a DEBUG build, and AZRA's own code was UNOPTIMISED

The report's re-run instructions use `.\target\debug\app.exe`, and `Cargo.toml` says:

```toml
[profile.dev]
opt-level = 0            # <-- AZRA's own crates

[profile.dev.package."*"]
opt-level = 2            # <-- dependencies only
```

So every number is for app code compiled at **opt-level 0**, with only third-party crates
optimised. That does not invalidate the report, but it scales different subsystems very
differently:

| Subsystem | Debug distortion | Why |
|---|---|---|
| Overlay / UI / panel code | **Large** | scalar-heavy, allocation-heavy, all app-crate |
| Modifier fold | Small | dominated by `Vec` allocation + memcpy, which is in `std` |
| GPU dispatch / bake readback | ~None | GPU-bound; CPU submit is a thin wrapper |
| Pipeline compile | ~None | naga/wgpu are dependencies, already opt-level 2 |

Cross-check that supports this: Grok measured the modifier fold at depth4 = 21 ms (debug); my
standalone `bench_display_mesh_ops` measured 31 ms at **twice the mesh size** in `--release`. The
fold barely moves between profiles, exactly as predicted — so the P1 modstack finding is SOLID.

**The P0 "14 ms idle overlay tax" is the one most inflated by this**, because it is the most
app-crate, most scalar-heavy bucket in the list. It may still be a real problem. It is not yet a
measured 14 ms.

**Action:** re-run with `cargo run -p app --profile fast-release` (opt-level 2, thin LTO — exists
precisely for this) before optimising anything in that bucket.

## Correction 2 — "overlays" is mislabelled; it is not viewport overlays

The bucket is a timer span (`app_update.rs:575` → `:622`), not a function. What actually runs
inside it:

```
:583  ensure_library_scanned()
:587  library_panel::draw_in_central(...)
:595  library_panel::shown_item_indices(...)
:599  library_tick_thumbs(ctx, &want)      <-- thumbnail generation
:610  ui2 dock/panel construction
:618  draw_viewport_overlays(...)          <-- the only actual "overlays"
:621  draw_ai_beauty_viewport_overlay(...)
```

So the 14 ms is **library panel + thumbnail ticking + dock construction + overlays**, attributed
entirely to the last item by its name. Optimising `draw_viewport_overlays` could easily produce no
improvement at all.

`library_tick_thumbs` is the standout suspect — thumbnail work in the per-frame path, running
every frame regardless of whether the library is visible or changed.

**Action:** split that bucket into per-call timers BEFORE optimising. Grok's own proposed next step
("instrument/split the overlays 14 ms") is exactly right — it should happen first, not after.

## What survives unchanged

* **P0 post-load bake stall** (64³ 95 ms → 128³ 668 ms, `[STALL] frame=686 ms`). GPU/readback
  bound, so debug distortion is minimal. This is real and is arguably the worst user-visible
  defect in the report — a multi-hundred-ms freeze after every load.
* **P1 modifier fold.** Confirmed independently in release (b979035 measured the prefix-cache
  ceiling at 118 ms → 43 ms on depth 8). Fix is scoped and measured, just not landed.
* **P1 curvature dispatches `vert_cap` (2,048,384) not live vert count.** Structural, not a timing
  artifact. Note this is by design — the true count is GPU-only ("NO mesh readback") — so the fix
  is an indirect dispatch, not a readback.
* **P2 cold pipeline compile** (~312 ms). Dependency-side (naga), so debug-independent.

## Coverage caveat worth keeping

The report is honest that stub tools (most Sculpt brushes, all NURBS, several modifiers) have
nothing to time. Worth restating: **"no perf data" is not "no perf problem"** for those paths — it
means untested, and they should not be read as green.

## Recommended order (revised)

1. Split the overlays bucket into real per-call timers (cheap, and it decides whether P0 #1 exists)
2. Re-run under `--profile fast-release` to get numbers that reflect shipped performance
3. Fix the post-load bake stall — real, user-visible, debug-independent
4. Land the modifier prefix cache — already measured, already scoped
