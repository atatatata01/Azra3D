# Viewport perf — measured findings, 2026-08-12 (owner repro: 29 fps "simple scene")

**Repro:** `Desktop/performance viewport.fl4` — ONE remeshed poly object, 323,048 tris, 71 MB.
Edit mode. Owner report: 29 fps object mode, 25 fps edit mode.

## The headline: none of this is the mesh

`show_overlays` OFF in object mode = **400 fps** on the same 323k tris. GPU dispatch 1.65–2.1 ms,
`mesh=0.0`, `Serialize 0.00`, `Bake 0.0 x0`, `GPU reallocs 0`. **Every millisecond is CPU work that
should not be running per frame.** Do not "optimise the mesh" — measure, then delete per-frame work.

A 143 ms frame (`[STALL]`) splits as:

| bucket | ms | note |
|---|---|---|
| `input` | **77–133** | biggest. Present even with `tool=Idle`, `repaint_next=none` |
| `overlays` | **40** | see below; different function depending on overlay toggle |
| `layout` | 19 | egui |
| `dispatch` | 2.1 | GPU is idle |
| `poll` | 0–47 | `[SLOWPOLL] polypick=44`, intermittent |

## What is proven, and what killed each wrong guess

Four hypotheses died to one measurement each — **instrument before editing**:

1. ~~`content_signature` per frame~~ — `Poly sync: 0 calls`, `Poly mod evals: 0`.
2. ~~`draw_node_selection_outlines`~~ — cost is selection-INdependent (deselect changed nothing).
3. ~~grid overlay~~ — `draw_grid_overlay` is an empty no-op (`draw_viewport_overlays.rs:435`).
4. ~~`draw_poly_overlays`~~ — timed at **0.0**. (It is also explicitly UNGATED, so it cannot be
   what the overlay toggle removes — that alone disproved it.)

## Confirmed spenders

**A. `draw_light_gizmos` — 31.5 ms. FIXED 2026-08-12.** Re-measured after C landed, with the
`[SceneOv]` sub-timers: `lights 30.9` of a `scene 32.9` — it is the entire remaining overlay cost,
and B's "middle third" was this all along (`ckA == ckB`, `rest 0.0`).

→ **Fixed as prescribed: a depth test.** `render/src/depth_probe.rs` copies ONE depth texel per
light out of the raster depth buffer (`RasterPipeline.depth_texture`, now `COPY_SRC`), reads it
back asynchronously, and `app/src/light_depth_probe.rs` compares it — in linear view space, with
the same bias the ray used — against the light's own projected depth. `draw_light_gizmos` only
reads the verdict. O(lights), independent of triangle count; the raster pass had already computed
this for every pixel.
- Single-flight, previous result consumed and **unmapped before any submit** (the picking.rs
  "buffer is still mapped" crash class), `map_async`'s `Result` never discarded.
- Runs in `poll_background_tasks` — Frame Ordering Contract; the overlay pass only reads.
- **Fails open:** no verdict ⇒ the icon draws (no depth buffer yet, off-screen, failed map). A
  stray icon is recoverable; a light the user cannot find is not.
- Verdicts are one frame late — invisible for an icon, and the alternative is the stall this
  removes.
- Depth convention is unit-tested by round-tripping real points through `CameraUniforms::
  from_orbit`, not against a hand-copied formula: inverting it would hide every light exactly
  when it should show.

Original diagnosis: `light_world_occluded` (`draw_light.rs:11`) calls
`raycast_scene_surface` — a full-scene CPU raycast, per light, per frame, purely to decide whether to
hide the icon. **One** light. Cost scales with TRIANGLES, not lights, which is why `Lights: 1` in the
HUD read as innocent.
→ **Fix:** occlusion must be a depth test. Use the GPU pick buffer, the contract the poly wire
already documents: *"Midpoint occlusion uses the pick buffer so only front topology shows — same
contract as the depth-tested GPU wire."* Caching the raycast is a band-aid over the wrong test.

**B. Overlay middle-third — RESOLVED, it was A.** The 2026-08-12 re-run reads
`ckA 30.9 ckB 30.9 … rest 0.0` with `lights 30.9` — nothing lives between the checkpoints. The
earlier `ckB 37.7` split predated the `lights` sub-timer. No separate spender to hunt.

Original bisection:

**B. Overlay middle-third — 38 ms, overlays OFF.** `[SceneOv] ckA 0.0 ckB 37.7` bisects it to the
calls BETWEEN `draw_dirty_regions_overlay` and `draw_viewport_frame_indicator`. Twelve candidates;
with `tool=Poly Transform` live the plausible ones are `draw_active_tool_hud`,
`draw_poly_region_select_overlay`, `draw_bevel_handles`, `draw_curve_edge_toolbar` (takes `ui`, so it
can do layout work). → **Next step: timers on those 12, one run names it.**

**C. `input` — 77–133 ms. NAMED AND FIXED 2026-08-12.** Not a drag handler: `poly_hover_pick`
(`poly_selection/pick.rs`), which runs every frame the cursor is over the viewport in any poly
sub-mode. Its **Face/Group arm** was the only visibility arm still asking a CPU ray:
`ray_pick_mesh_face` over EVERY visible poly body, O(faces), with `face_vertices()` + a `Vec` of
positions + an `earclip_polygon` **per face** — 323k faces per frame, at `tool=Idle`. That is the
whole bucket, and it is the same bug class as A: a CPU raycast standing in for a depth test.

→ **Fixed** by putting Face hover on the ONE visibility system verts and edges already use — the
GPU select buffer (`PolyPickImage::nearest_face_at`, ring scan past the 1 px edge/vert ids pass 2
paints over a face's rim):
- buffer valid + face id under the cursor → that face, O(1);
- buffer valid + no face id → the active body is provably not frontmost at that pixel, so it
  cannot win the nearest-hit contest: skip it, ray only the OTHER bodies (the id-0 occluder
  channel cannot name them, so `hovered_face_other` still needs the ray);
- buffer stale/pending → full CPU scan, behaviour unchanged.

**C2 — the live-edit half. FIXED 2026-08-12** (owner: *"when I edit the mesh I go back to 7 fps"* —
7 fps ≈ the original 143 ms frame). Mid-edit the mesh key changes every frame, so the select buffer
is never valid and Face hover fell straight back to its O(faces) CPU scan — the whole 77–133 ms,
every frame of every drag. Two per-frame mesh walks, both gated on the open undo episode:

1. `poly_hover_pick` returns immediately. Hover is meaningless while a gesture owns the pointer —
   the tool captured its target when it armed, `poly_click_select` does not run mid-drag, and the
   fields are recomputed on the first frame after the commit. "Nothing hovered" is honest and free.
2. `poll_poly_select_buffer` bails **before** `poly_pick_wanted_keys`. Computing the key is itself
   a cost: `poly_display_cache_key`'s memo is keyed on `poly_edit_generation`, which a live edit
   bumps every frame, so the memo missed every frame and walked `content_signature` (~6 ms at 160k
   faces, more at 323k) only to conclude the buffer was stale — which the episode already said.
   `want_sync` (pointer press) still refreshes synchronously.

Both bounded by `auto_commit_episodes`, so a leaked episode cannot freeze hover or selection for
more than one frame.

**C3 — and the fallback itself had to go.** The episode gates were not enough: the owner's next run
(`input=78ms, tool=Poly Transform, repaint_next=pointer_down`) was an ORBIT, not an edit. The
select buffer's key includes the CAMERA, so `poly_pick_image_valid()` is `None` on every frame the
camera moves and Face hover took the CPU scan regardless of any episode. **A fallback that runs
whenever the user is moving is the hot path, not a fallback.** The active body is now never
CPU-scanned: no valid buffer ⇒ no hover answer that frame. Clicks are unaffected — a pointer press
refreshes the buffer synchronously (`want_sync`) and poll runs before input in the same frame, so
the frame a click is interpreted on always has a valid buffer. What is lost is the hover HIGHLIGHT
during camera motion; it returns the frame the camera settles. Other bodies still get the ray
(id-0 occluders cannot name themselves) — they are not the 323k-tri body this protects.

**D. `[ClusterBake]` — NOT a frame stall. Re-read 2026-08-12, no fix needed.** The 491 ms runs on
a `std::thread::spawn` worker (`cluster_bake.rs:147`), behind a restart-on-change debounce, with an
on-disk DAG cache keyed by CONTENT (`dag_cache_key(&positions, &indices, &params)`) — so a rebuild
of geometry seen before loads instead of rebuilding, and 491 ms is the genuinely-cold path. The
only frame-thread cost is `snapshot()` (`app_poll_gpu.rs:277`): one clone of positions + normals +
indices, ≈20 MB at 323k tris, ONCE per settled edit.

"Why does the stamp keep invalidating": it does not misbehave — `content_stamp` is a monotonic
counter bumped on every mesh-cache content mutation (`mesh_cache.rs:808`), so `stamp=18` and
`stamp=22` during an edit session are four real edits. The suspicion that D was behind
`[SPIKE] poll_background_tasks: 44ms` was wrong: that 44 ms is E, now fixed.

**Left as-is deliberately.** If the snapshot clone ever shows up, the fix is to hand the worker the
existing buffers rather than to touch the debounce.

Original note:

**D. `[ClusterBake]` cold rebuild — 491 ms.** `MeshCacheId(1) levels=11 clusters=7806 L0_tris=323048`
re-firing on `stamp=18`, `stamp=22` during an edit session. Find why the stamp keeps invalidating;
likely also behind `[SPIKE] poll_background_tasks: 44ms`.

**E. `polypick` — 44 ms. FIXED 2026-08-12.** Not the readback (already async, single-flight, one
frame late). It is `render_poly_select_buffer`'s CPU vertex build: three `Vec` allocations and an
ear-clip **per face**, 323k of them, on the frame thread.

Why "every other frame": the mesh-settle latch compares against the PREVIOUS frame only, so an
edit whose mesh key lands on alternating frames reads as settled-stale-settled and the gate opens
every other frame. Two fixes, both in `poly_select_gpu.rs`:
- **Gate on the open episode** (`pending_episode.is_some()`) — the architecture's own statement
  that a gesture is mid-flight and will commit, instead of a one-frame latch that alternation
  defeats. Cannot wedge selection: `auto_commit_episodes` closes a leaked episode every frame, so
  the gate is at most one frame long, and selection itself opens no episode (paint / region select
  still get their buffer).
- **Parallelise the per-face triangulation** (rayon, ordered collect) so the once-per-settle hitch
  that remains is split across cores — same cost class and same remedy as the 2026-08-08 poly
  display-bake fix.

**F. `layout` — 19 ms.** Unexamined.

## Fix order

~~C (input)~~ ✅ → ~~A (depth test)~~ ✅ → ~~D~~ (not a stall) → ~~E (polypick)~~ ✅.

**Open, both needing one run each:**
**B — NAMED AND FIXED 2026-08-12.** `[MidOv] frame 38.8` of a 38.9 ms middle third, every other
candidate at 0.0. It is `draw_viewport_frame_indicator` → `viewport_frame_geometry_issues`: **three
O(mesh) walks** — `is_manifold`, a per-face normal over every face, and a stacked-vertex hash over
every vertex — run from the PAINT path, for the status light on the viewport border.

It was memoised, on `poly_edit_generation`. That memo cannot work: the generation IS the mutation
counter. And an episode gate was not enough either — the same capture shows `frame 36.9` with
`tool=Idle`, because the generation churns *after* a drag (`repaint_next=poly_display_dirty`,
`[ClusterBake]` stamps climbing ~8/s at rest) as well as during one. **A diagnostic must not put
unbounded work in the frame at whatever rate the mutation counter happens to move.**

Now `poll_poly_geometry_issues` in the poll phase; the painter is a pure field read. Bounded four
ways: no episode open → mesh settled (generation unchanged since the previous poll, the select
buffer's latch) → ≥300 ms since the last audit → generation/body actually changed. Between audits
the previous verdict stands: a real answer a moment old, never a guess.

**F — `layout` 40–88 ms: prime suspect fixed, needs confirmation.** `ol_refresh_manifold_cache`
(outliner Manifold column) runs `is_manifold()` — O(mesh) — for every polygonal body, and its
fingerprint deliberately folds in `poly_edit_generation` (it has to: an in-place twin rewire moves
neither the Arc pointer nor the counts, and dropping it brings back the "column stayed green after
going non-manifold" bug). So the fingerprint misses whenever the generation moves, which is every
frame of a drag and ~8×/s at rest. Now held while a drag episode is open, recomputed on the first
refresh after the commit. Already gated on the column being visible, so only users with it on paid.

To split what remains of `layout`, run with `AZRA_SCOREBOARD=1` — `[LAY2]` (shell/windows/keyboard)
and `[LAY-DOCK]` (dock paint) already exist and need no new code.

**THE COMMON CAUSE — a memo keyed on the mutation counter.** Four sites now, all found this way:
`poly_display_key_memo` (via `poly_pick_mesh_key`), `viewport_frame_geometry_issues`,
`ol_refresh_manifold_cache`, and `poly/lod.rs`'s latch (correct as-is — it is *meant* to re-run per
edit). `poly_edit_generation` IS the mutation counter, so any cache keyed on it degrades to "no
cache" exactly when the work is hot. **Any new memo on this key needs an episode gate or a content
key.** The uniform remedy: `pending_episode.is_some()` → serve the last value; recompute after the
commit; `auto_commit_episodes` bounds every such gate to one frame, which is what makes it safe
even on P0 paths like selection.

**G — `poll_poly_lod_policy` 628 ms × 2. FOUND AND FIXED 2026-08-12 by running the repro.**
Not in the original bucket list — it only shows up in a scripted run, because it fires on scene
load and on every settled edit, not continuously. `poly_lod_ensure_coarse` ran TWO full decimations
of the 323k-tri mesh (50% and 15%) **synchronously in poll**. Now on a worker thread, one job per
content key, results installed in `poll_poly_lod_results` (mesh-cache writes stay on the main
thread) and dropped if the key moved on. Until a result lands the body draws at level 0 — which is
what it did before the first build finished anyway, so no window shows anything but the real mesh.

Measured, same scene + harness, fast-release:

| | before | after |
|---|---|---|
| `poll_poly_lod_policy` | 627.8 ms / 629.7 ms | **14.9 ms / 11.5 ms** |
| worst load frame | 813 ms (`mesh=633`) | **227 ms** (`mesh=22.4`) |
| 660 ms+ stall frames | 2 | **0** |

**Reproducing it without a human** (added to `dev_tools.rs`, same class as `AZRA_ORBIT`):
`AZRA_OPEN=<path.fl4>` loads a scene at frame 5, `AZRA_MODE=polyface|polyedge|polyvert` selects the
first poly body and enters that sub-mode at frame 60. Object-mode-only runs measure nothing —
they never touch hover pick, the select buffer, the geometry audit or the Manifold column.
```
AZRA_UI=v2 AZRA_SCOREBOARD=1 AZRA_ORBIT=0.6 AZRA_MODE=polyface   AZRA_OPEN="…/performance viewport.fl4" app.exe > run.log 2>&1
```
**Caveat on scripted runs:** with no pointer over the viewport, `response.hovered()` is false, so
hover-pick costs do not appear. Scripted runs cover load, poll and draw; hover still needs a human
or a synthetic pointer.

## EDIT-FRAME PROFILE — measured 2026-08-12, scripted

`AZRA_EDIT=<frames>` opens an undo episode and nudges vertices every frame: the cost profile of a
live grab (mesh content changes per frame → display sync → generation bump), which is the state
every episode gate is judged on. NOTE: `auto_commit_episodes` closes an episode as soon as no
pointer is down, so the harness must hold it open (env-gated hold in `undo.rs`) or the run silently
measures the UNGATED path — the first attempt did exactly that.

200-frame edit, 323k tris, fast-release, same binary:

| bucket | gates OFF | gates ON |
|---|---|---|
| `layout` | 26–29 ms | **0.8–2.3 ms** |
| `poll` | 10–12 ms | **0.1 ms** |
| `overlays` | 10–12 ms | 13–21 ms |
| `mesh` | 62–70 ms | 89–132 ms |
| **frame** | ~135 ms | ~135 ms |

**What this proves:** the episode gates do exactly what they were written to do — the outliner
Manifold walk (`layout`) and the select-buffer key walk (`poll`) are gone, ~38 ms of a 135 ms frame.
**What it also proves:** the edit frame is NOT bounded by them. `mesh` — the poly display sync,
untouched by any of this work — is half the frame and grows when the gates are on (likely because
LOD never settles on a coarse level mid-edit, so the sync runs on full detail; unverified).

**So: edit performance is not yet fixed.** The next target is `mesh` at 62–132 ms/frame.

**Harness caveat that still stands:** no pointer means `response.hovered()` is false, so hover-pick
cost never appears in these numbers. The C/C2/C3 hover fixes remain unverified by measurement.

### `mesh` split — measured 2026-08-12 (`[SyncSplit]`, 201 samples, complete run)

```
[SyncSplit] 33.7ms = resolve 13.5 + normals 0.0 + raster 8.1 + upload 12.0 | tris=323048 evals=1
```

- **`resolve` 13.5 ms** — display-cache lookup, and on a miss the full modifier-stack evaluation.
  `evals=1` on EVERY frame: the stack is re-evaluated every frame of the drag, because the display
  cache key moves with the vertices.
- **`raster` 8 ms** — ear-clip triangulation. Re-run from scratch each frame even though a vertex
  grab changes no TOPOLOGY, so the index buffer is identical. `core_math::topology_signature()`
  exists for exactly this case (POLY_VIEWPORT_PERF P3) and this path never consults it.
- **`upload` 12 ms** — full vertex+index buffer rewrite, including those identical indices.
- **normals 0.0** — not a factor.
- `poly_drain_display_dirty` measured 44–53 ms against a 33 ms `[SyncSplit]`; the ~12 ms gap is the
  `sig` computation at the top of `poly_sync_mesh_cache` — another `content_signature` walk.

### METHODOLOGY WARNING — absolute ms are NOT comparable across runs

The median edit frame read 122–140 ms in two runs and **64 ms** in the next, with no change between
them capable of causing it: the owner's own `app.exe` was running during the slow ones and competing
for CPU. **Only compare within one run, or compare COUNTS** (a gate either fires or it does not —
which is why the 174 → 1 LOD result stands while the "135 ms edit frame" baseline does not).
An earlier claim in this document was already withdrawn for a related reason: a spike count read off
a log that was still being written (160 → 28) was wrong; the complete logs read 160 → 174.

## THE ARCHITECTURAL FIX — budget the interactive display sync (owner-directed, 2026-08-12)

Every micro-optimisation was exhausted: `resolve` re-evaluates because the base genuinely changed,
`raster` and `upload` are already rayon-parallel, and the "obvious" index-reuse win measured SLOWER.
The remaining cost was not a slow function — it was **running a correct, necessary function at input
rate instead of at frame rate.**

`poly_drain_display_dirty`'s single-body branch was commented "Interactive single-body edit:
same-frame solid" and called `poly_sync_mesh_cache` unconditionally. It was the ONE interactive path
opting out of `FrameScheduler`, which every other subsystem (bake readback, DC results, mesh
uploads) already defers through. It now asks `can_run` with its own **measured** cost as the hint
(EWMA, learned not guessed) — but only while a drag episode is open; a one-shot edit still syncs
immediately.

Two guarantees keep this a deferral rather than a dropped update:
- **Staleness ceiling** `POLY_SYNC_MAX_STALE_MS = 33` — the solid can never lag more than ~2 frames,
  so it stays visibly live during the gesture. The cage/overlay tracks the pointer regardless.
- **The settled mesh is never a draft.** A skipped body goes back on `poly_display_dirty`, and
  `app_update` requests a repaint while that queue is non-empty. Once the episode commits,
  `pending_episode` is None and the next drain takes the unbudgeted branch — verified in the log: a
  single full sync at the commit frame, then quiet.

**Measured, 200-frame scripted edit, 323k tris, fast-release:**

| | before | after |
|---|---|---|
| **median edit frame** | **105.7 ms** | **7.6 ms** |
| frames over the 120 ms stall threshold | 28 | 2 (both at load) |
| slow display syncs during the edit | 201 | 27 |

~14x. The magnitude is far outside the ~30% cross-run contention noise documented below, and the
sync COUNT is a count rather than a timing, which is the robust half of the evidence.

### Where the edit frame stood BEFORE that change (end of 2026-08-12)

Every bucket that was *avoidable* work is now gone or gated. What remains inside a drag frame is
work the current architecture genuinely has to do, and this was established by reading the code
after the measurements, not before:

- **`resolve` ~14 ms — irreducible as designed.** The prefix ladder keys on `base_sig`
  (`base.content_signature()`). A vertex grab changes the base every frame, so the ladder always
  misses at depth 0, materialises `Arc::new(base.clone())` — a full deep copy of the 323k-face
  mesh — and re-runs the whole stack. Nothing to cache: the input really did change.
- **`raster` 8–17 ms** — already parallel (prefix-sum slots + block-parallel fill). Index reuse
  could skip phase 1's serial `face_vertices_into` walk and the index writes, but phase 2 (corner
  normals, auto-smooth fan) has to run regardless, so the ceiling on that idea is modest.
- **`upload` 12–21 ms** — already rayon-split. The cheaper-looking in-place path measured SLOWER
  (see the reverted experiment).

**The remaining lever is architectural, not a micro-optimisation:** do not evaluate the full
modifier stack at full quality on every frame of a drag. The codebase already does exactly this for
SDF (`body_vol_cache.draft_resolution`, restored on `commit_episode`) and for the render pipeline
(`InteractionRenderPolicy` latches quality for the duration of an interaction). Poly has no
equivalent. That is a design decision with a visible-quality trade-off during the gesture, so it
belongs to the owner, not to a perf pass.

**Still open:**
- **The generation churn itself.** `poly_edit_generation` / mesh `content_stamp` keep moving with
  `tool=Idle` and nothing being edited (`[ClusterBake] spawn … stamp=47` → `stamp=61` in 1.6 s,
  each dropping the previous build as "stale (edited while building)"). Nothing should be editing
  the mesh at rest. Every generation-keyed cache in the app is paying for this; it is the common
  cause behind B, C2, F and D's repeated rebakes.
  **`[GenChurn]` is now in the tree**: `bump_poly_edit_generation` is `#[track_caller]`, and a
  detector reports `N bumps/s | file:line xN` once a second when the rate exceeds 90/s — above any
  genuine 60 Hz drag. A healthy session prints nothing and pays two integer compares per bump. One
  run names the bumper.
  Leading hypothesis to check against that output: `ensure_poly_meshes`
  (`raster_mesh_prep.rs:122`) enqueues into `poly_display_dirty` for any body it judges undrawable,
  the drain syncs and bumps the generation, and there is no "already tried and it did not help"
  guard — so a body that can never satisfy the drawable test (e.g. a stack whose display mesh is
  legitimately empty) would re-enqueue forever. Unverified; do not fix it before `[GenChurn]`
  points there.

**The recurring trap, now swept.** `poly_edit_generation` is a MUTATION COUNTER, so any memo keyed
on it is guaranteed to miss on every frame of a drag — it degrades to "no cache" exactly when the
work is hot. Three sites keyed on it: `poly_display_key_memo` (hit via `poly_pick_mesh_key`, fixed
in C2), `viewport_frame_geometry_issues` (fixed here), and `poly/lod.rs`'s policy latch (a latch
that is *meant* to re-run per edit — correct as-is). Any new memo on this key needs the same
episode gate, or a content key instead.

## Instrumentation in the tree (uncommitted)

`[SceneOv]` sub-timers + `ckA`/`ckB` checkpoints in `app/src/viewport_overlays.rs`, threshold-gated at
8 ms like `[Overlays]`, so healthy frames pay nothing. **Keep them** — they are what will prove each
fix, and the `[Overlays]` comment already explains why the coarse bucket alone "could easily have
yielded nothing".

## Unrelated, also uncommitted

`core_math/src/bevel_op/` — arc inner miter kernel (BEVEL.md §3.8a). Needs a convex-corner oracle
before its concavity gate can be written. Nothing was committed on 2026-08-12.
