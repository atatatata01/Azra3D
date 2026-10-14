# Live session monitoring — 2026-07-30

---

## "What percentage did viewport performance gain?" — honest answer: **no measured gain, and none claimed**

Asked 2026-07-30. The straight answer is that **Grok's viewport work is a FEATURE, not an
optimisation**, so there is no percentage to report — and inventing one would be worse than saying so.

**What the change actually is.** `material_ranges` on `MeshCacheEntry` + per-range `draw_indexed`
calls in the shadow / raster / wire passes. That is multi-material poly rendering: geometry that
previously drew in **one** call now draws in **N** (one per material). On the GPU side that is
strictly *more* draw calls, not fewer. It exists so a poly body can carry more than one material —
which it could not before at all.

**Why the `raster_mesh_prep.rs` line reduction is not a speedup either.** 108 lines went away and 8
rebuild call sites with them, but they were in `ensure_poly_meshes`, which is a **recovery** path:
it rebuilds only when a cache slot is missing or broken. The consolidation routes recovery through
`poly_sync_mesh_cache` as the single owner. That is a correctness/duplication win (one owner for poly
display data), not a per-frame one.

**The evidence it was never hot.** `ensure_poly_meshes` is already instrumented —
`app_update.rs:481` wraps it in `time_block!`, which prints `[SPIKE]` above **10 ms**. Across 934 log
lines of a real modelling session it printed **zero times**. It was never a measurable share of frame
time, so removing work from it cannot show up as a frame-rate gain.

**What IS still costing frame time**, measured this session and unaddressed by this change:

* `poll_background_tasks` at a steady **19–27 ms every idle frame** (budget is 12 ms)
* **331 `poly_sync` calls across 5 distinct mesh states** — ~7× redundant

That is the real viewport-performance target, and it is a *different* code path from what Grok
touched. See "The one real finding" below, plus the measurement showing a `content_signature` guard
is not the fix.

**To get a real percentage** the app must be run before/after with the same scene and interaction, and
the frame time compared — the `[STALL]`/`[SPIKE]` lines already provide the instrument.

### A/B attempted 2026-07-30 — and it is NOT RUNNABLE. That is itself the finding.

Method: save the four changed files (state B), `git checkout --` them back to HEAD (state A), build
both binaries, run the same scene through each.

**State A does not compile.**

```
error[E0599]: no method named `update_editable_mesh_ex` found for struct `MeshCache`
  --> app\src\poly\mesh_sync.rs:431
```

`poly/mesh_sync.rs` — a *fifth* dirty file, also Grok's, and not part of the four — already calls the
new `update_editable_mesh_ex(cid, raster, tri_count, ranges)` API. So the multi-material work is not a
separable layer sitting on top of the old renderer; it is a **coupled change across three files**
(`mesh_cache.rs` defines it, `raster_gbuffer.rs` draws it, `mesh_sync.rs` feeds it) plus its
callers.

Reverting only the render half leaves the producer calling a function that no longer exists. There is
no "before" to measure without also reverting `mesh_sync.rs`, which would revert unrelated poly work
committed on top of it.

**All four files were restored byte-for-byte** (verified with `cmp`) and the tree compiles again.

**What this confirms rather than contradicts:** the change is a *feature landing*, not a tunable
optimisation. An optimisation can be toggled off to measure it; this cannot, because the old code path
no longer has a caller. The correct number remains **no measured gain**, and the honest reason is that
there is no isolated "before" state to compare against.

**The measurable target is elsewhere** — `poll_background_tasks` at 19–27 ms/idle-frame and the ~7×
`poly_sync` redundancy, both of which live in code that is *not* part of this change and *can* be
A/B'd once addressed.

---

**Setup:** AZRA launched from the ui-polish worktree with stdout/stderr captured, plus a background
watcher scanning for panics, WGSL validation failures, non-manifold results, device loss, frame stalls
> 100 ms, and polls > 60 ms. 934 log lines over the session.

---

## Verdict: the session was clean

| Signal | Result |
|---|---|
| Panics / device loss / validation failures | **0** |
| `manifold=false` | **0** (70 bevels, all `manifold=true`) |
| Frame stalls > 100 ms | **1**, and it is benign — see below |
| Polls > 60 ms | **0** (worst: 27.2 ms) |

**The one stall is startup, not usage.** `[STALL] frame=354ms … unaccounted=343.6 | tool=Idle` sits at
log line 50, on **frame 1**, and the very next line is
`Pathtrace-tri pipeline compiled (background): 289ms`. That is the shader compile, before any user
interaction. Not a regression.

**Bevel is genuinely healthy.** 70 bevel operations, every one reporting `manifold=true`, and the
multi-segment path is live:

```
[BEVEL] edges=1 width=0.302 segments=3 shape=0.50 clamp=on
        -> verts 12->18 faces 10->13 boundary=0(loops 1->0) manifold=true
```

`segments=3` is being honoured — this is `bevel_op.rs`, not the frozen single-segment
`bevel_edges_ex`. Worth recording because the earlier plan draft wrongly claimed bevel was
single-segment-only everywhere; the live log settles it.

---

## The one real finding: `poly_sync` runs ~7× more than the mesh changes

**331 `poly_sync` calls across only 5 distinct mesh states.**

| calls | mesh state |
|---|---|
| 180 | `live_faces=11 raster_verts=46 tris=24` |
| 80 | `live_faces=10 raster_verts=40 tris=20` |
| 38 | `live_faces=13 raster_verts=58 tris=32` |
| 32 | `live_faces=13 raster_verts=55 tris=29` |
| 1 | `live_faces=9 raster_verts=36 tris=18` |

180 consecutive syncs of an **identical** mesh state means the same raster data was rebuilt and
re-uploaded to the GPU 180 times for one unchanged mesh. Each carries a `[CTRLX]` log line and a GPU
index-buffer upload.

That is consistent with `poll_background_tasks` sitting at a steady **~19–27 ms every frame** while
idle — comfortably over the 12 ms FrameScheduler budget, on work whose output is byte-identical to the
previous frame's.

### Why this is worth fixing rather than tolerating

It is the same class as the `SceneEvaluator` memo fixed earlier today (rebuilding a deterministic
result that could not have changed), which measured **12–16×**. The mechanism here is different but
the shape is the same: no guard between "something might have changed" and "rebuild everything".

### What to check first (in order)

1. **Is the `[CTRLX]` instrumentation itself the cost?** It is debug logging added during this
   session's poly work. Measure with it stripped before optimising the sync — the logging may be a
   large share of the 19–27 ms, in which case the sync count is a *symptom*, not the bill.
2. ~~**Does `poly_sync_mesh_cache` have a content guard?**~~ **MEASURED 2026-07-30 — a
   `content_signature` guard is NOT the answer.**

   I benchmarked the hash against the rebuild it would guard:

   | faces | `content_signature` | `to_raster_data` | ratio |
   |---|---|---|---|
   | 96 | 0.023 ms | 0.045 ms | **1.9×** |
   | 384 | 0.091 ms | 0.190 ms | **2.1×** |
   | 1536 | 0.368 ms | 0.773 ms | **2.1×** |

   The hash costs **half** the rebuild, not a fraction of it — both are O(V+E+F) walks over the same
   data. Guarding with it caps the win at ~2× and *adds* cost on the frames where the mesh really did
   change (hash + rebuild instead of rebuild). The pillar review's caveat was right to demand this
   measurement.

   **So the fix is to call it less, not to guard it.** With 331 syncs over 5 distinct states, the
   win is in removing the ~7× redundant *calls* (which is 100 % of the waste), not in making each
   call conditional (which recovers at most half). That points at item 3.
3. **Who calls it 180 times?** There are 102 `poly_sync_mesh_cache` call sites. The likely pattern is
   several per frame from independent paths (input, poll, draw), each defensively syncing.

### What NOT to do

Do not fix this by debouncing or throttling the sync. That hides the cost rather than removing it, and
the project has a standing rule against timing tricks that dodge a problem. The honest fix is a
content guard or fewer call sites.

---

## Not observed, but reported by the owner

Three bugs were reported from the running session that produced **no log output at all** — bridge
refusing unequal loops, G-after-loop-cut arming the wrong tool, and polygroup selection selecting
everything. All three were found by reading code, and all three are now fixed
(`93d3b8f`, `2ac2ab8`).

**That is the monitoring gap worth noting:** the log captures GPU/timing/manifold signals well, but
carries nothing for *tool routing* or *selection state*. A one-line log on tool activation
(`[TOOL] loop_cut -> confirmed -> gizmo(Translate)`) and on selection change
(`[SEL] mode=Group faces=1 -> 11`) would have made all three visible immediately rather than requiring
a repro description.
