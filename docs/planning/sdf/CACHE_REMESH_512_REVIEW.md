# SDF Cache Live-Remesh — 512/1024 Review (2026-07-09)

**Source:** 18-agent verified review (6 subsystem readers + 12 adversarial verifiers) over the full
pipeline on `feat/sparse-3a` (synced with master). 74 findings total; the 12 critical/high were
independently verified against the code. This doc is the surviving, verified set + the execution
plan. Owner direction locked: **1024 = Edit-Mode detail only (512 Object-Mode must be flawless
first); mixed scenes (big blends AND small parts); scrolling-grid escape fix greenlit (~1-2 days).**

---

## THE HEADLINE — why 512 keeps being slow (three verified root causes stacking)

### R1 — `sparse_bake_debug` is default-OFF and NEVER PERSISTED  ✅ VERIFIED (the 553ms mystery)
`cache_state.rs:186` inits it `false`; exhaustive grep: **no serde, no eframe storage** — the
Developer-Tweaks toggle resets OFF on every app launch. `sparse_bake_eligible = sparse_bake_debug
&& gridbacked_only && res >= 512` (mod.rs:665-667) — for an Object-Mode full cubic 512 bake, the
toggle is **the only gate that can fail**. So every session starts in the DENSE 512 regime
(GridBacked since ef6b448 raised the cap 256→512 as a "dense interim"): select body → dense 512³
bake + ~512MB readback = the recurring **553-816ms freeze**, until the toggle is re-enabled by
hand. All the sparse perf work is invisible at launch.
**Fix:** kill the toggle as the gate — sparse at ≥512 becomes structural (S2.6) once hardened
(see R4); interim: persist the toggle + convert the `[SparseWhy]` probe to a runtime gate.

### R2 — Windowed DC degenerates to a FULL-surface re-mesh every drag frame  ✅ VERIFIED ×2
The DC window is `atlas.surface_brick_min/max` — the bbox of **all** handle bricks, never the
partial bake's dirty window (`last_bake_dirty_range` is stamped but provably unused on this path).
At 42%-surface scenes the window ≈ the whole grid: **dc_vertex+dc_quads = ~134M threads each,
~1.8GB of scattered pool reads, ~533MB cell_vert rewrite, whole mesh (~100-300MB) rewritten — every
drag frame.** Estimated 15-40ms GPU/frame = the steady 42-58ms `poll_background_tasks`
backpressure. Verifier confirmed: **no half-step exists** — a compacted brick-list dispatch still
re-meshes the whole surface (atomic-append output). The fix is **3B-S3 tiled incremental DC**.
**Fix (verified design):** 32³-cell tiles (matches `ChunkedMesh` chunking; 4096 tiles @512 — NOT
per-brick, 110k slots of bookkeeping exceeds the win), cloned from the proven voxel precedent
(`voxel_gpu_mesh.rs:31-115`: shared verts/idx/indirect/counters/params pool, per-slot sub-range
bindings, cached per-slot bind groups w/ generation invalidation, ONE `multi_draw_indexed_indirect`).
`sdf_mesh.wgsl` is already batch-ready (`local_idx` consumes per-chunk base `params.a1.w`).
Re-mesh only tiles overlapping the dilated dirty window (+1-cell halo). **Picking must move in
lockstep** (`draw_sdf_resident_slot` is shared by G-buffer AND picking — P0 selection).
~600-800 LOC. Expected: DC 15-40ms → 0.5-2ms/frame.
**Prereq diagnostic (no-bandaids rule):** add the `[DcResident]`-style split timing + GPU
timestamps to `dispatch_resident_sparse` (resident.rs:187-200 — currently zero instrumentation)
to confirm the term before building.

### R3 — VRAM: ~3.2GB of dense-sized buffers PER 512 body, even on the sparse path  ✅ VERIFIED ×2
`SdfBodyMeshSlot::new` (resident.rs:234-266): `field` = 512MiB (**pure dead weight on sparse —
bound but never read**), `cell_vert` = 511³×4 ≈ 533MB (volume-proportional), `verts`+`idx` = 1GiB
each (clamp operative on the 4090). Total scene stack ≈ **6-7GB** (slot + atlas pool ~250MB +
rv.buffer 512MB + VolumeAtlas 2GB + G-buffers/shadows) → oversubscription → **the likeliest root
of the hard crash** (device-loss class; no dump written). At 1024 the slot would need 4GB field +
4GB cell_vert — impossible; only the Object-Mode 512 clamp prevents the attempt.
**Fix:** (a) `slot.field` → `Option<Buffer>`, allocated only for dense dispatches; bind the
existing 4-byte dummy pattern (`SdfMeshGpu::dummy_index/dummy_pool` precedent, same struct) on
sparse — saves 512MB/body, trivial; (b) verts/idx sized surface-proportionally from
`allocated_bricks` (grow-only watermark, ≤512 cells × 1 vert × 8 f32 + ×6 idx per brick);
(c) cell_vert remapped to the DC window/tile extents (dc_params_sparse_windowed already carries
them). Plus: **capture the device-loss reason** (`device.on_uncaptured_error` / DeviceLost
callback) so the next crash names TDR vs OOM.

---

## VERIFIED BUGS (correctness — fix before/alongside perf)

### B1 — `provably_exterior` is UNSOUND for modifier-stack prims  ✅ VERIFIED (my S2.5g/h bug)
`bounding_radius()`/`bounding_aabb_half_extents()` compute `mod_pad` from the **legacy single
`self.modifier` only** — every UI-added modifier in the `self.modifiers` STACK (Displace,
elongation, sculpt layers) contributes **zero pad**. The S2.5g reclaim proof and the S2.5h
classify prefilter treat "outside bounding sphere" as an absolute exterior proof → a displaced
surface extending past the un-padded sphere gets its bricks **wrongly reclaimed/skipped → holes**.
**Fix (structural, in `primitives.rs`, never special-cased in the classifier):** `mod_pad` sums
the per-modifier pads over `self.modifiers` when non-empty (fallback to the legacy arm), shared by
BOTH bound fns. Add an adversarial test: displaced prim near the sphere boundary → classify
superset must hold.

### B2 — Mode-2 bake evaluates the full prim stack for sentinel-brick voxels, then discards  ✅
`bake_volume`'s only early-out is the bounds check; the scope machine + prim loop runs for every
window voxel, and mode 2 drops the write for sentinel bricks at the end — **60-90% of partial-bake
GPU ALU wasted** (2.5-10× cut available; makes 1024-scale partials feasible later).
**Fix:** sentinel early-out reading `brick_index[brick_linear]` — placed AFTER the pad/in_body/bv
computation and gated structurally on `in_body` (NOT right after the bounds check: `bv = voxel -
pad` underflows u32 for padding voxels; mode 2 forces pad=0 today but the shader is shared).
Whole 4×4×4 workgroup spans one brick octant → uniform coherent branch, near-zero cost.

### B3 — Edit-Mode entry at 512 with a brick-resident body = 0.7-1.0s freeze  ✅ (PARTIALLY, refined)
Chain verified: C5 guard marks dense re-bake (sparse produces nothing the volume raymarch can
read) → dense 512 bake + 512MB readback + rv realloc + ~512MB texture-only atomic upload.
**Fix:** "route-0 heal" — a small compute pass decoding the SDF brick atlas (index+pool) directly
into the rv 3D texture (+ buffer), guarded by the same body/res/atlas-generation identity check
`dispatch_grid_dc_async` already uses. Turns mode entry into one cheap GPU pass.

### B4 — DC dispatches have no TDR slicing  ✅ (PARTIALLY — diagnostic first)
The bake Z-slices at MAX_WG; the DC does not (134M threads in one pass at the degenerate window).
Largely mooted by R2's tiling; until then: device-loss capture (R3) + timing (R2 prereq) to
attribute the crash properly before adding slicing.

### B5 — S2.6 blocker: `SdfBrickAtlasGpu` is single-tenant  ✅ VERIFIED
One body at a time (`body_nid: Option<NodeId>`, full-index overwrite on upload). Flipping sparse
default-ON with 2+ bodies at 512 = full-bake ping-pong (57-110ms per body switch, partials never
survive). **Fix:** per-body residency BEFORE S2.6 — `HashMap<NodeId, SdfBrickAtlasGpu>` + VRAM
budget + LRU eviction (mirrors `VolumeAtlas::allocate_or_evict`; pool is surface-proportional,
~250MB/body → 1-2GB budget holds 4-8 bodies). Contracts C3-C5 key on the marker and survive.

### B6 — Instrumentation gaps (why owner logs couldn't attribute the 42-58ms)  ✅ (PARTIALLY)
`[STALL]` omits poll; sub-timers PERF_SPAM-gated; `[SparseWhy]`/`[SparseLoop]` are
cfg(debug_assertions). **Fix:** unconditional `[SLOWPOLL]` breakdown when >40ms during an
interaction episode; convert the two probes to runtime gates (the `bake_prof` pattern).

---

## EXECUTION PLAN (gated slices, in order)

**P0 — correctness + crash attribution (small, immediate): ✅ ALL SHIPPED 2026-07-09**
1. ✅ `e59406f` B1 modifier-stack containment contract (`modifier_pad()` shared by
   `bounding_radius`/`bounding_aabb_half_extents`, ×max_scale; Displace/elongation/mirror folds;
   `has_unbounded_field()` sculpt-layer backstop) + `bounding_radius_contains_world_surface`
   property test (>20k probes, negative control verified).
2. ✅ `0a2fc27` B2 mode-2 sentinel early-out in `volume_bake.wgsl` (after pad/in_body guards;
   60-90% partial ALU cut). Naga + oracle-parity gates green.
3. ✅ `4e7acd3` R3a lazy `slot.field: Option<Buffer>` (512MiB/body dead weight gone on sparse) +
   `on_uncaptured_error`/`set_device_lost_callback` reason capture → visdump.
4. ✅ `991584a` R1 interim: `sparse_bake_debug` **default ON** (persistence deliberately rejected —
   RenderSettings lives in the bincode ProjectFile = .fl4 field-trap; S2.6 removes the toggle) +
   `[SparseWhy]`/`[SparseLoop]` unconditional runtime (pathology-only, fire in release too).
5. ✅ `823c2db` B6 instrumentation: `[SLOWPOLL]` >40ms-during-episode unthrottled arm +
   `[DcResidentSparse]` split timing with windowed extents (the R2 decision datum for 3B-S3).

**P1 — the two big levers (owner-directed):**
6. ✅ `2918158` **Scrolling-grid escape** (2026-07-09): `sparse_index_translate` (core_math, pure;
   3 unit tests incl. the coordinate-convention gold test) + `try_sparse_scroll` first in the
   escape branch — atomic index+bounds commit, exposed slab pushed as ordinary dirty regions, the
   existing frozen-index partial arm does the rest; `sparse_scrolled` flag forces the same-dispatch
   `index_update`. Grow-full stays the fallback (scale-up / oscillation / teleport / non-sparse).
   End-to-end GPU parity test: 1207 bricks retained verbatim, slab handles 194/239 recycled from
   reclaim (pool +45), sign-exact vs oracle, retained bit-identical. Constant extent ⇒ constant
   sampling density on long drags. Owner markers: `[SparseScroll]`, BAKEPROF `scroll->partial`.
6b. ✅ `4a4fe78` **Rigid-translate fast path** (2026-07-09, from the owner's drag log): scroll
   worked at moderate speed but the capped (~1.35×body) window degenerates — slab∪stamp = whole
   grid (TriggerBake 40-64ms) and fast drags out-travel the window (teleport → grow-full loop at
   52-55ms/dispatch). Root insight: an all-prims-equal-delta edit rigid-translates the whole
   body-local field — so the bounds follow the prims and NOTHING classifies/bakes/re-meshes.
   Mesh re-anchors via the derived instance offset `entry.grid_min - slot.meshed_grid_min`
   (stamped by every mesh dispatch; picking in lockstep). Poison-by-default on every generic
   dirty mark (sticky within a burst); fast path placed BEFORE the in_flight insert (a rigid
   consume dispatches no bake). Non-rigid edits (multi-prim partial moves, rotate/scale,
   sliders, mirror) fall through to partial/scroll/full unchanged. 4 cache-level tests; app
   suite 92/92. Owner marker: `[RigidMove]` (1-in-30 throttled).
6c. ✅ `c554c95` **provably_interior** (2026-07-09, from the owner's scale-gesture log): growing
   a prim swallowed its old surface shell and the exterior-only reclaim leaked those handles
   (+46k bricks reused=0 per frame, pool 41k→153k in one gesture). The dual proof:
   `SdfPrimitive::inscribed_radius()` (donor contract — PRISTINE prims only; its property test's
   FIRST RUN caught prim-mirror being a fold, not a copy-union) + classifier donors/carvers/
   interior_enabled + the prefilter (solid interiors skip the 9-eval classify) + the reclaim arm.
   Interior proofs disabled body-wide when any INTERSECTIVE prim exists. Found in passing,
   documented in `interior_prefilter_classify_parity`: the S2.5h EXTERIOR proof legitimately
   overrides conservative corner "Surface" verdicts (the eval's bounded box distance) —
   pre-existing, mesh-benign. Owner-log tail item INVESTIGATED & DROPPED: the "stray 64³ dense
   at settle" is the designed first-bake cascade feedback after an entry-clearing event (undo);
   ~1-3ms, and the S2.5h divert already prevents any dense ≥512 step.
7. **3B-S3 tiled incremental DC** (R2): 32³ tile pool + multi_draw + picking lockstep. Kills the
   15-40ms/frame full-surface re-mesh. (After #5 confirms the term via `[DcResidentSparse]` ext.
   NOTE from the owner's 2026-07-09 logs ×2: no `[DcResidentSparse]` >2ms lines fired — trigger
   CPU dominated both sessions; re-measure after the rigid path before investing here.)

**P2 — S2.6 hardening (flip sparse structural at ≥512):**
8. ✅ `fdb9555` B5 atlas multi-tenancy (2026-07-09): per-body `sdf_brick_atlases`
   HashMap + VRAM-budgeted LRU (25% VRAM cap 2 GiB floor 256 MiB — the VolumeAtlas::new_auto
   precedent; keep-body never evicted; evicted bodies keep their mesh + self-heal on next
   edit; SCENE prune reclaims orphans). Switching between sparse bodies now re-bakes NOTHING.
   Policy unit-tested (`sdf_atlas_lru_eviction_policy`). Marker: `[SparseEvict]`. Follow-up
   noted: eviction REVIVAL via frozen re-upload (skip the re-classify on re-edit).
9. ✅ (R3b half) surface-proportional verts/idx (2026-07-09): slot outputs start as 4-byte
   placeholders; every dispatch sizes them (`ensure_output_caps`, grow-only, generation-bumped) —
   dense arms keep today's exact clamped sizing, the sparse arm sizes from
   `atlas.allocated_bricks × VERTS_PER_BRICK_HEADROOM` (=48; MEASURED max 22.6 verts/brick,
   idx/vert = 6.00, `sdf_dc_sparse_verts_per_brick_measurement`). Overflow (atomicMin-truncated
   mesh) is detected by an 8-byte per-dispatch async counters readback and handled in poll
   (`poll_sdf_mesh_output_overflow`): grow ×1.5 (≥1.25× true count) + re-mesh from the resident
   atlas; gate test `sdf_dc_sparse_output_overflow_grow` (grown re-mesh == ample-cap reference).
   A sparse 512 body's outputs drop 2 GiB → ~80 MB (29.8k-brick sphere-subtract scene); ~296 MB
   at a degenerate 42%-surface scene.
9b. ✅ R3c / Tiled-DC stage A — brick-local `cell_vert` (2026-07-09): on the sparse path the
   cell→vertex map is re-addressed through the SAME brick indirection as the field reads —
   cell `c` in handle brick `H` stores at `cell_vert[H*512 + (c&7 local)]` (`cell_slot()` in
   sdf_mesh.wgsl); sentinel-owned cells have NO storage and provably no vertex (soundness note
   at `cell_slot`: a decoded crossing forces both crossing voxels within `band` of iso → both
   bricks surface → every corner brick of the cell is in the 26-ring apron → handle). Dense path
   keeps region-local addressing byte-identical (dual keyed on `brick_res`, like `fld()`). The
   slot's cell_vert starts as a 4-byte placeholder and is sized per dispatch via
   `ensure_output_caps` (3rd cap, grow-only, generation-bumped; sparse cap = allocated×512 EXACT
   — never clamped, no overflow possible: byte size equals the atlas pool's). @512
   sphere-subtract: 509 MB dense → 58 MB (29.8k bricks × 2 KB); stays surface-proportional at
   1024 — the last dense mesh buffer is gone. Gates: all sparse parity suites bit-exact + new
   `sdf_dc_sparse_cell_vert_brick_boundary_parity` (box faces EXACTLY on brick seams — the
   quad-pass neighbor-read hazard). Old `cell_uniform_sentinel` fast-reject subsumed by the
   NO_SLOT early-return (one tag read). REMAINING (stage B): multi_draw/shared pool.
9c. ✅ Tiled-DC stage B-1 — shared slotted mesh pool + multi-draw (2026-07-13): ONE
   `SdfMeshPool` in GpuCtx (`sdf_gpu_mesh/pool.rs`, mirrors `VoxelMeshGpu`'s pool patterns:
   256B-aligned sub-range compute writes, indirect self-skip, 1 GiB per-buffer self-cap,
   generation bump on realloc) holds ALL sparse bodies' verts/idx as VARIABLE-SIZED regions +
   per-slot `DrawIndexedIndirect` args + a per-slot instance buffer. `mesh_into_slot_sparse`
   binds the body's region as a storage SUB-RANGE (bindings 3/4 — shader `arrayLength` guards
   unchanged) and the DC counters copy targets the slot's args entry (the VoxelMeshGpu
   counters→indirect wiring; verified the draw reads only verts/idx/indirect, so per-body
   `cell_vert`/`counters`/`params` stay slot-owned scratch). Draw: raster_gbuffer + raster_pick
   (LOCKSTEP, P0) each issue ONE `multi_draw_indexed_indirect` over the pool — N sparse bodies
   → 1 draw call per pass; per-frame visibility = CPU-owned `instance_count` (0/1) + per-slot
   InstanceData writes, so hidden/isolated/dense-switched bodies self-skip with the exact
   per-body filter semantics. Args `first_instance = slot index` selects the instance
   (INDIRECT_FIRST_INSTANCE, adapter-gated in gpu_init; fallback = per-slot
   `draw_indexed_indirect` loop over the SAME pool). Slot grow = R3b policy via
   `SdfMeshPool::grow_slot` (relocate/append; tail exhausted → REBUILD: new buffers ≥×1.5,
   other slots' regions GPU-copied + index_count preserved in place, generation bumped,
   requester re-meshed; pool-cap overflow → per-body slot fallback). Body deleted →
   `free_slot` (SCENE prune, dirty.rs). Dense (≤512 non-brick-resident) bodies keep per-body
   slots + draws. Atlas LRU eviction deliberately KEEPS the pool region (an evicted body keeps
   its drawn mesh — the existing per-slot behavior; freeing it would vanish the surface).
   Gates: `sdf_pool_parity_multi_body` (3-body per-slot vs pool counts + vertex parity
   IDENTICAL; draw calls 3→1; pool VRAM 7.1 MB @96³ test scene) +
   `sdf_pool_grow_relocate_preserves_counts` (forced overflow → grow → counts == ample
   reference; forced REBUILD preserves the other body bit-exact; no device loss); full app
   suite 111 green.
10. B3 Edit-Mode route-0 heal (atlas→texture decode pass).
11. ✅ `9ef06bc` **S2.6 — sparse is STRUCTURAL at ≥512** (2026-07-14): `sparse_bake_debug`
    DELETED everywhere (CacheState field + ON-default, the BodyVolumeCache mirror + its `false`
    init, the per-frame `dirty.rs` sync, the "Sparse bake (3A debug)" Developer-Tweaks checkbox,
    the dev_tools flag dump + the AutoRepro toggle-flip). The regime now keys on `gridbacked_only`
    ALONE: `sparse_bake_eligible(res) = gridbacked_only && res >= 512`, and
    `max_mesh_resolution()` = sparse ceiling when `gridbacked_only` else dense — so the Grid
    picker offers 1024 in Object Mode with NO dev setting (the R1 class — a runtime-only flag whose
    default silently decided the regime — is now structurally impossible; a re-added gate would
    have to be persisted, and RenderSettings is the .fl4 field-trap, so a code comment at the old
    field site forbids it). `[SparseLoop]`/`[SparseWhy]` removed (their whole job was attributing
    toggle-era regime surprises). **Semantic equivalence proof:** `gridbacked_only` shares the
    removed mirror's lifecycle exactly — same `false` init in `BodyVolumeCache::new`, same setter
    line in `trigger_body_vol_bake` (dirty.rs) — so pre-sync (both dense) and post-sync (CacheState
    default was already ON, unpersisted) behavior is byte-identical. The ONLY behavior change is
    the intended one: a user can no longer force the dense path by unchecking. Tests: the toggle-OFF
    arm of `sparse_body_resolution_stable_under_draft` now reaches the dense regime the only way
    production actually can — a body BELOW the sparse threshold (base 256, draft 128 → 128);
    `sparse_cache_at` + the two 1024 hardware tests dropped the now-redundant `= true` line
    (they already set `gridbacked_only = true`).
    **Adversarial review caught (and this doc originally repeated) a false premise worth pinning:
    `gridbacked_only` is ALWAYS TRUE in production** — `dirty.rs` sets it unconditionally at the
    top of `trigger_body_vol_bake` (its only non-test writer), and the legacy `force_volume_only`
    that could clear it was DELETED 2026-07-13 ("provably constant false"). So Edit Mode does NOT
    drop a body to dense/TextureOnly any more, and the `gridbacked_only == false` branches
    (`max_gridbacked_resolution` arm of `max_mesh_resolution`, the TextureOnly clamp in
    `effective_resolution`, the conjunct in `sparse_bake_eligible`) are DEAD — kept only as the
    structural guard if an Edit-Mode dense regime ever returns. The live dense regime is `res < 512`.

**1024 (per owner):** Edit-Mode detail only — B3's route-0 heal + the existing TextureOnly volume
path carry it; no Object-Mode 1024 mesh work until 512 is flawless.

**STATUS UPDATE (2026-07-09, owner-directed): 1024 UN-GATED on the sparse Object-Mode pipeline.**
Direction superseded — the DC mesh is the display everywhere; the dense-texture Volume Preview is
legacy and now capped at 512 (`max_textureonly_resolution` 2048→512; a 1024 body never routes
TextureOnly). Cap split: `max_gridbacked_resolution()` stays 512 FOREVER (dense), new
`max_sparse_resolution()=1024` applies only via `BodyVolumeCache::max_mesh_resolution()`
(sparse regime active: `gridbacked_only` — the SAME condition `sparse_bake_eligible` keys on, so a
1024 request exists only when the sparse fork takes it; a
binding-size assert at the dense output allocation makes dense >512 fail loudly, never
device-loss). [Updated by S2.6 (#11): this was `sparse_bake_debug && gridbacked_only` until the
toggle was deleted 2026-07-14.] Sites re-keyed: `effective_resolution` / `compute_resolution_for_body` clamps,
trigger_bake + cascade cubic invariants, `is_texture_only` boundary, cascade ceiling (the ≥512
sparse divert now fires for 1024), Grid picker (1024 selectable exactly when the sparse regime
will take it). Hardware gate `sparse_1024_runs_like_512_end_to_end` (tests_sparse_bake.rs):
sphere+subtract at 512 AND 1024 through production caps → shared GPU classify encode → finalize →
Z-slab-sliced mode-2 atlas bake → windowed sparse mesh. Measured (RTX 4090, debug): 1024³ =
128³ index (8 MB), 182,004 allocated bricks (pool 355.5 MB), 1.46 M verts / 8.74 M idx;
classify 7.8 ms, finalize 87.7 ms (worker-thread tolerable), bake 56 ms (16 slabs), mesh 32 ms.
Ratios vs 512: bricks 3.91×, verts/idx 4.02× — surface-proportional as designed.

## Do NOT regress
All prior invariants (LIVE_MOVE_PERF.md §Do-NOT-regress) + this effort's: binding-6 contract
(brick_index owns 6, mesh_grid stays live in mode 2), reclaim freed-list determinism, windowed-DC
bit-parity gates (`sdf_dc_sparse_atlas_slot_parity`, `sdf_dc_sparse_windowed_small_body_parity`),
S2.5e never-draft-sparse, C3-C5 consumer contracts.

**P1-#6b rigid-translate invariants (4a4fe78):**
- Rigidity is claimed ONLY through `mark_prim_transform_dirty`'s translate arm; every other
  dirty entry point poisons by default — never add a mark that bypasses this.
- Poison is STICKY within a burst (`RigidAccum.poisoned`) — an early slider tick must dominate
  a later translate note (`poisoned_burst_falls_through_to_a_real_bake` pins it).
- The fast path stays ABOVE the generation bump + `in_flight` insert — it dispatches no bake,
  so an in-flight entry would deadlock the coalesce gate (test pins no-phantom-in-flight).
- `slot.meshed_grid_min` is stamped by EVERY mesh dispatch (all three `mesh_into_slot*`); the
  draw offset is DERIVED per frame — never cache or reset it manually.
- Mirror bodies never take the rigid path (body-anchored plane).

**P1 scroll invariants (2918158):**
- A scroll commits index + entry bounds + body_bounds ATOMICALLY in `try_sparse_scroll` — never
  install one without the others (they are meaningless apart).
- A scroll frame MUST ship an `index_update` (the `sparse_scrolled` flag) — the GPU atlas holds
  the pre-scroll index until that upload; skipping it = handles decoded at old brick coords.
- The exposed slab travels as ORDINARY dirty regions — no second window pipeline. Slab coverage
  is pinned by `sparse_scroll_window_translate_matches_oracle`'s `src_lost == 0` assert.
- Retained bricks are NEVER re-baked (`out_bit_mismatch == 0` gate) — that's the entire win.
- Containment stays non-negotiable: scroll only when the snapped window fully holds the tight
  bounds; every other case falls back to grow-full (`compute_escape_grid_bounds` unchanged).

---

## LIVE ATTRIBUTION (2026-07-16, owner session + GPU timestamps — the R2 prereq diagnostic, commit 2533ca9)

Harness (512³, RTX 4090): full-window re-mesh = **3.1ms GPU** at 23% surface, **6.1ms** at the 39%
degenerate case → **R2's 15–40ms estimate is DEAD at 512** (R3c's brick-local cell_vert killed it).

Owner live log (one body, duplicate → Subtract, scale gesture at **1024³**) names the real terms:

1. **[SparseEscalate] full-classify loop — the structural pathology.** The partial window exceeded
   1/8 of the grid on EVERY drag tick (6× in one gesture) → each tick escalated to an async FULL
   1024 classify (**17–30ms GPU+readback + 5–6ms frame-thread validate**) + full sparse bake +
   full re-mesh. The partial/scroll machinery never engaged at 1024 for a scale of a large prim.
2. **[SdfPool] rebuild balloon — the 52–69ms [SLOWPOLL primary] spikes.** Transient brick counts
   exploded during the gesture (164k → 330k → **489,984** → settle 195k), each growth forcing a
   pool REBUILD with fresh allocations up to **verts 1073.7MB + idx 846.7MB** (gen 2→5 in one
   gesture). Both >50ms SLOWPOLLs coincide exactly with a rebuild line. (The 490k transient may
   also be the c554c95 interior-leak class recurring at 1024 with a subtract carver — verify.)
3. **DC re-mesh at 1024 = 10–24ms GPU confirmed** ([DcResidentSparse] GPU vert=14.6 quads=9.3 at
   ext=801³) — R2 IS alive at 1024, scaling with the escalated window. Tiled incremental DC
   (3B-S3) is justified by the 1024 track, not 512.
4. (Sub-512 regime, earlier in log: [TriggerBake] 5.1ms/frame + [DirtyFallback aabb_escape] every
   frame while dragging a prim at draft 64–128³ — the grow-full loop; separate, milder.)

**Slices, in order:** (a) gesture-coalesced escalation — at most ONE full classify in flight during
an episode, ticks in between take the capped partial or skip (the escalate loop is the multiplier
for everything else); (b) pool-rebuild damping — defer rebuilds to settle, per-body fallback
mid-gesture (kills the 52–69ms spikes); (c) tiled DC for 1024 (the 10–24ms GPU term); (d) check the
490k transient brick balloon against the interior-proof donors at 1024.

- 2026-07-16 — **slice (a) SHIPPED: gesture-coalesced escalation.** Per-body cooldown
  (`ESCALATED_FULL_MIN_INTERVAL` = 300ms) on the OVERSIZED-WINDOW escalation arm only: inside the
  cooldown a tick defers exactly like the in-flight coalesce (regions restored, in_flight/timing
  unwound, body re-armed via deferred_bodies) — at most ~3 mid-gesture full pipelines per second
  instead of back-to-back; settle full stays authoritative. Corruption-escalate + first-full arms
  NEVER deferred. Test: `sparse_escalation_cooldown_defers_then_reescalates` (defer leaves no
  in_flight residue — the forever-coalesce trap). App suite 176 green. NEXT: slice (b)
  pool-rebuild damping (the 52-69ms spikes), (c) tiled DC for 1024, (d) 490k brick balloon check.

- 2026-07-16 — **slice (d) resolved by diagnostic + slice (b) SHIPPED: settle-time capacity trim.**
  The 490k transient is NOT a classifier bug: `d_sweep_subtract_scale_brick_counts` (#[ignore]d,
  tests_sparse_bake.rs) shows a static two-sphere boolean never exceeds 235k bricks at any subtract
  radius OR prim scale — the live peak comes from bounds churn (tight grid ⇒ smaller cells ⇒ more
  bricks per surface) and is legitimate-conservative. The DEFECT was grow-only caps CHASING the
  peak and pinning it (verts 1073MB + idx 846MB + cell_vert ~1GB after one gesture). Fix:
  `slot.may_trim` one-shot (set by the poll caller from `sparse_quiet` — no escalation within 2×
  cooldown; timing stays at the edge, the mesh layer is deterministic) → `SdfMeshPool::trim_slot`
  (rebuild with the slot at 1.25×need; other slots GPU-copied) + `SdfBodyMeshSlot::trim_output_caps`
  (cell_vert exact, unpooled verts/idx 1.25×). Hysteresis deadband (fire under ½, land 1.25×) —
  proven no-churn. Gate: `sdf_pool_settle_trim_shrinks_and_preserves` (shrink 3.3×, counts
  identical, other body bit-exact, deadband no-op). App suite 177 green. REMAINING: slice (c)
  tiled DC for the 1024 10-24ms GPU term.

- 2026-07-16 — **slice (c) STAGE C1 BUILT (uncommitted): tiled DC — per-tile regions + per-tile
  DrawIndexedIndirect + one-submit batched dispatch.** Tile = 32³ cells (4³ bricks), stats stamped
  by atlas upload (`tile_occupancy`, sdf_brick_atlas.rs). §R2's "batch-ready" claim VERIFIED FALSE
  for sparse (cell_slot ignores a1.w when brick_res>0; no per-tile counters/output bases exist) →
  three NEW WGSL entries (`dc_vertex_tiled`/`dc_quads_tiled`/`dc_tile_args`, 9-binding layout,
  monolithic entries untouched; QEF factored into shared `dc_cell_solve`). Boundary strategy =
  option (a) halo duplication: per-tile self-contained index spaces (the C2 prerequisite); anchor
  windows PARTITION the surface → Σ per-tile index_count == monolithic EXACTLY (gate-asserted).
  Per-batch (≤1024 tiles) ONE vertex + ONE quads pass over disjoint scratch windows; ONE args-fill
  pass replaces N counter copies; ONE submit total. Draw = ONE multi_draw per body over its args
  block; visibility = the pool's per-pass draw list (mark_drawn/clear_frame_visibility), picking
  lockstep. Overflow = per-tile counters readback → per-tile cap floors → re-mesh. Trim clears
  floors + shrinks the region; churn headroom lives on the REGION (tile caps stay tight). Gates:
  `sdf_tiled_full_matches_untiled` (27 tiles, parity 0.00000, Σidx exact),
  `sdf_tiled_pool_multi_body_grow_and_visibility` (26-tile overflow→floors→ref-exact; B
  bit-preserved). r2 @512: tiled steady 3.30/6.66ms vs monolithic 2.94/5.77ms (+12/+15%, within
  the ≤20% budget — C1 is infrastructure; the win is C2's dirty-tile-only dispatch at 1024).
  C2 SEAM: `mesh_into_slot_sparse_tiled` currently meshes ALL `atlas.tile_stats`; C2 filters that
  list by the dirty window (needs: dirty cell range + the slot's stable tile table/floors) and
  dispatches only those entries' ranges — regions/args of clean tiles are already stable.

- 2026-07-16 — **IDLE SETTLE-LOOP FIXED** (owner log 2: endless "[RetightenArm] by=no_copy_completion
  while IDLE"). Diagnosis (subagent-verified): PRE-EXISTING structural gap — a FIRST bake intercepted
  by the PATH-A no-copy completion never registers in body_grids, so trigger_bake dispatches the 64³
  coarse cascade forever (coarse → no-copy → note_cpu_grid_stale re-arms → advance_cascade re-dirties
  → coarse). Exposed by slice-(a) settle traffic; the trim was NOT implicated. THREE fixes: (1)
  dispatch gate — `resident_full_direct` now requires `has_cached_grid` (first bakes always take the
  readback path, the cascade contract holds by construction); (2) `note_cpu_grid_stale` refuses to
  arm settle for an entry-less body (guard + one-line diagnostic); (3) slice-(a) regression — the
  cooldown deferral now unwinds its generation bump (stranded +1s made in-flight landings compare
  stale and get dropped, prolonging the entry-less window). Tests: entry-less no-copy quiescence +
  generation-unwind assert in the cooldown test. Suite 178 green. Also confirmed from owner log 2:
  settle trim WORKS (595MB pool + 877MB cell_vert freed); escalation rate bounded but mid-gesture
  rebuild spikes remain → slice (c).

- 2026-07-16 — **slice (c) STAGE C2 BUILT (uncommitted): dirty-window incremental tiled DC — the
  payoff stage.** `mesh_into_slot_sparse_tiled` now takes `Option<&SdfTileDirty>` (Voxels window /
  explicit Tiles keys) and, when EVERY staleness guard holds, re-dispatches ONLY the tiles whose
  field support ([32T−3, 32T+33] voxels/axis) overlaps the window — clean tiles keep regions/args/
  TileInfo/counters byte-untouched. Compaction = brief option (a): dirty TileInfos packed into a
  dedicated `tile_info_stage` buffer (binding 8), params range [0, n_dirty); `TileInfo.tmeta.z`
  (the reserved word) carries each tile's REAL entry index — the 3 tiled WGSL entries now address
  counters/args via tmeta.z (full dispatch writes tmeta.z = entry ⇒ one code path, C1 gates
  byte-green). Plumbing: `PendingBake.bake_window` (req.bake_min/max) → sparse PARTIAL acceptance
  unions into `BodyVolumeCache.sparse_mesh_dirty_window` (full acceptance clears) → consumed once
  by the sparse mesh arm; overflow re-mesh passes `Tiles(overflowed keys)` so only hit tiles
  re-dispatch/grow. FULL re-mesh forced by ANY of: no window entry (full bake / first mesh /
  structural), slot not tiled/pooled, stamp mismatch (res, bounds, atlas.generation, dims_bricks
  — `SdfTiledStamp`), pending settle-trim, occupancy changed OUTSIDE the window (contract
  violation + eprintln), tile grow/birth exceeding region tail slack or args_cap. Births append
  entries at the block tail; deaths tombstone in place (`SdfTileEntry.dead`, args zeroed) so no
  neighbor's entry index ever moves. Gates (all green, suite 183): `sdf_tiled_incremental_matches_full`
  (partial mode-2 window re-bake into the SAME atlas → incremental == from-scratch full: per-tile
  args identical, Σidx identical, parity 0.0 both ways, clean tiles bit-preserved, 18/27 dispatched
  gate-asserted), `sdf_tiled_incremental_empty_window_and_grow_isolation` (no-tile window ⇒ 0
  dispatches bit-exact no-op; floor-forced grow tail-appends ONLY that tile, neighbors bit-preserved).
  MEASURED (`c2_measure_incremental`, #[ignore]d): 512 A-shell 64³-window re-mesh **0.73ms wall /
  0.04ms GPU, tiles 20/1300** vs full tiled 3.41ms / monolithic 3.12ms; 1024 A-small **1.42ms wall /
  0.014ms GPU, tiles 8/2972** vs 6.87/6.81ms — the GPU term collapses ~50-200×, wall now dominated
  by per-mesh CPU overhead (submit/params/readback ≈0.7-1.4ms). **C1-ENVELOPE FINDING (pre-existing,
  blocks the 1024 payoff for BIG surfaces):** Σ per-tile caps (halo_bricks×48, ≥32/tile) exceeds the
  1 GiB pool ceiling for realistic 1024 shells (scene-A@1024 Σ=41.6M verts, 0.9-shell 33.9M vs
  ~33.5M ceiling) ⇒ C1 falls back to monolithic BEFORE C2 can engage. Next: shrink per-tile
  headroom toward the measured ~23 v/brick and lean on the proven overflow-floor machinery (or
  split bodies across pool buffers) so wide 1024 surfaces enter the tiled path at all.

- 2026-07-16 — **slice D BUILT (uncommitted): the landed-full "54ms sparse atlas upload" was the
  CPU index scans, not the upload — fused + parallelized.** Split instrumentation first (the
  brief's rule: fix what the numbers say): `upload_index` now returns an `UploadIndexSplitMs`
  and the `[SPIKE] sparse atlas` line prints `create/ensure(grew)/write/bbox/tiles`; the
  `[DcResidentSparse] tiled` line prints `table/pool/infos/encode/submit`. New #[ignore]d harness
  `d_measure_landed_full_1024` (tests_sparse_bake.rs) replays the gesture (growing subtract,
  persistent atlas+pool+slot, z-slab mode-2 bakes, full tiled re-mesh per landing) in the SAME
  opt-level-0 dev profile the owner's logs come from ([profile.dev] opt-level=0; the
  `package."*"=2` override does NOT cover workspace members). MEASURED at 242k/174k bricks:
  the 8.4MB index write_buffer = **0.5-0.8ms** (D1's GPU→GPU copy target — NOT built, would
  have saved under 1ms); `tile_occupancy` = **47-63ms** + `surface_brick_bbox` = **7-8ms** —
  ~87% of the owner's 54ms. Fix: ONE fused single-pass scan (`index_content_scan`), incremental
  coordinates (no per-entry div/mod), rayon-parallel over tile-z layers (each layer absorbs the
  z-halo of the plane below it via a read-only boundary pre-pass; merge in tz order =
  deterministic, bit-identical output). Halo weights are deliberately BUG-COMPATIBLE with the
  reference's duplicate `[c,c]` combo iteration (contributions ×2^(non-hi axes)) — the C1/C2
  headroom constants + envelope gates were calibrated against those values; the reference fns
  stay as oracles. Gate: `fused_scan_matches_reference_oracles` (graphics — 7 dims classes incl.
  non-×4 axes + multi-layer). RESULT (harness, owner-scale): ATLAS phase 54.9-73.9ms →
  **3.9-6.3ms**; expected owner log: `[SPIKE] sparse atlas` largely disappears (<20ms gate) and
  `dispatch_pending_primary_bakes` drops by ~50ms (the atlas SPIKE was NESTED inside it — terms
  1+2 of log 7 were mostly the same milliseconds). D2 (incremental tile-table on fulls) NOT
  built: measured table=0.3ms infos=0.2ms — the brief's guess didn't survive measurement; the
  owner's 52-61ms `mesh_dispatch` did NOT reproduce in the harness (tiled cpu=1-6ms even with a
  1GB pool rebuild) — suspicion is submit-stall behind in-flight z-slab bake work or mid-gesture
  pool relocation; the new tiled split line attributes it on the owner's next run. App suite +
  graphics suite green (see session report).

- 2026-07-16 — **OCTREE O1 + O2a SHIPPED (solo).** O1: workgroup coarse cull in classify_bricks —
  workgroup(4,4,4) = one 4³-brick super-cell = one tile; thread 0's single center eval + the
  1-Lipschitz bound (half-diag + band + margin×4) culls whole cells BIT-IDENTICALLY (derivations in
  shader; 512+1024 parity oracles green with cull active). Gated by coarse_cull_safe (no modifiers/
  Seam/Extrude/MeshVolume). O2a: LOD stride meshing — DcParams.vmin.w = level; a level-L cell spans
  1<<L voxels of the SAME atlas (no re-bake/re-classify); cell_slot/cell_world/vox/solid/corners
  stride-mapped, L0 bit-identical by reduction. mesh_into_slot_sparse_lod (unpooled monolithic —
  LOD bodies are the few/distant ones; active body keeps tiled L0). Gate
  sdf_lod_level1_meshes_from_same_atlas: L1 = 4.00× fewer indices, geometry within 2 L1-cells of
  L0, L0 restore after LOD switch bit-exact. REMAINING: O2b = screen-size level policy + draw
  wiring (hysteresis to avoid popping); O3 = mixed levels per tile + boundary stitching (O0
  research raw claims favor level-per-tile w/ transition handling — RE-VERIFY, spend limit killed
  the verification votes).

- 2026-07-16 — **OCTREE O2b BUILT (this session, uncommitted): automatic per-body LOD policy +
  windowed LOD dispatch.** (1) The O2a full-grid limit is fixed: `mesh_into_slot_sparse_lod` now
  windows to the surface-brick bbox exactly like `mesh_into_slot_sparse` (shared helper
  `surface_cell_window`; level window = `(cmin>>L)−1 ..= (cmax+1)>>L`, 1-cell coarsening guard on
  the low side because level-cell corners are `1<<L` voxels apart). Gate
  `sdf_lod_windowed_matches_full_grid`: windowed == O2a full sweep at L1 AND L2, counts equal +
  vertex parity <1e-6 (window 48³ of 128³ cells in the gate — at 1024 the 1023³-thread/26ms sweep
  becomes a surface-sized dispatch). (2) Policy (`lod_policy.rs`, pure + unit-tested;
  poll wiring `poll_sdf_lod_policy` in app_poll_gpu.rs, called right after
  poll_sdf_mesh_output_overflow): signal = projected pixels of ONE voxel
  (`voxel_world·(h/2)/(d·tan(fov/2))`, body-transform-aware, ortho variant); tiers 0.5px/0.25px;
  ±30% deadband (bands disjoint: 0.35 > 0.325 ⇒ provably no oscillation, fixed-point-tested);
  1s/body rate limit (`cache.sdf_lod_last_switch`); ≤2 switches/frame (Frame Budget); active body
  ALWAYS L0; level clamp `(res−1)>>L ≥ 32`. Current level truth = `slot.lod_level` (content), not
  a shadow map — a normal bake resetting the slot to L0 re-enters the policy for free. L>0 goes
  through the windowed LOD entry (unpooled per-body draw; G-buffer AND picking both route
  unpooled slots through `draw_sdf_resident_slot`, so selection is unchanged); L0 restore goes
  through the NORMAL `sdf_populate_resident` (tiled/pooled). Toggle `cache.sdf_lod_auto`,
  **default OFF** (Developer Tweaks → Experimental → "Auto SDF mesh LOD"); toggling OFF (or
  activating a body) restores L0 through the same path. Suite 189/189 (was 185 + 3 chooser units
  + 1 windowed gate).

## O3 design sketch — mixed levels per TILE (design only, not built)

**Premise from the shipped code.** C1 tiles are already self-contained index spaces: each tile's
vertex pass covers its 32³-cell anchor window +1-cell halo, quads reference ONLY own-tile vertices
(boundary vertices deliberately duplicated across neighbors), each tile owns a pool sub-region +
its own DrawIndexedIndirect entry. That makes per-tile levels structurally natural: `TileInfo`
grows a `level` field (tmeta has 2 reserved u32s — no struct growth needed), `dc_vertex_tiled`/
`dc_quads_tiled` read the tile's level instead of `params.vmin.w`, and the O2a stride math is
already level-parametric per cell. A tile at level L dispatches (32>>L)³ cells into the same
scratch window — caps shrink ÷4 per level exactly like the monolithic LOD entry. The C2 dirty-set
machinery then gives incremental LOD for free: a level change is just a per-tile "dirty" re-mesh
of the tiles that crossed the threshold (screen-space error evaluated per tile bbox, not per
body — big bodies get near-tiles fine / far-tiles coarse).

**The one hard problem is the seam between a level-L tile and a level-(L+1) neighbor.** Two
candidate treatments:
- **(a) Transition strips (dc-style boundary rings).** The coarse tile's boundary layer of cells
  is meshed at the FINE level (one ring), so edge crossings on the shared face are sampled
  identically on both sides — the classic "transvoxel"-style approach adapted to DC. Fits the
  halo architecture (the ring lives inside the tile's own index space, no cross-tile references)
  but complicates the vertex pass: two strides in one tile, and the QEF cell layout in cell_vert
  needs a second addressing region for the ring.
- **(b) Vertex snapping at seams.** Mesh both tiles at their own level; afterwards (in-shader,
  quad pass) snap boundary-cell vertices of the FINE tile onto the coarse tile's edge-crossing
  lattice (quantize the boundary-face position to the coarse stride). Cheaper (no second stride
  in the vertex pass, no layout change) and crack-free by construction on shared faces, but it
  visibly flattens fine detail in the boundary ring and the snap must be deterministic from BOTH
  sides (compute the coarse crossing from the same fld() reads, not from the neighbor's output —
  cross-tile output reads are forbidden by the self-contained-tile invariant).
Preliminary lean: (b) first — it respects the tile independence invariant with the least new
machinery; (a) is the quality upgrade if snapping artifacts are visible.

**What O0 research must confirm before building:**
1. DC-specific seam correctness: does vertex snapping alone guarantee watertightness when the
   coarse side's cell has NO vertex (no crossing at coarse stride) but the fine side has 2+ —
   the "topology mismatch" case transvoxel strips exist to solve? (This is the deciding question
   between (a) and (b), and the O0 raw claims favoring level-per-tile were never verified —
   spend limit killed the verification votes; RE-VERIFY.)
2. Popping economics: is per-tile hysteresis (the O2b chooser per tile) enough, or does mixed-
   level need geomorphing (vertex lerp between levels) to look acceptable at 0.25-0.5px tiers?
3. Budget shape: measured distribution of tiles-per-level for real scenes (the win is only large
   if far bodies are many-tiled; O2b's whole-body levels may already capture ~90% of the value —
   measure BEFORE building O3, per the D1/D2 lesson above: the brief's guess must survive
   measurement).
4. Interaction with C2 dirty tiles: an edit inside a coarse tile must force it (and its seam
   neighbors) back to L0 before the incremental re-mesh — confirm the dirty-set propagation rule
   (dirty ⇒ tile + face-adjacent neighbors at seam-level).

## Session log — 2026-07-16 (P1: "Auto SDF mesh LOD (O2b)" made a sparse 1024 body VANISH)

Root cause (file:line evidence): `SdfBodyMeshSlot.vert_cap`/`idx_cap` are DUAL-MEANING — a pooled
dispatch mirrors the POOL region caps into them (`mesh_into_slot_sparse_tiled`, resident.rs:567;
monolithic pooled arm, resident.rs:330) while the slot's OWN `verts`/`idx` stay the 4-byte
placeholders from `SdfBodyMeshSlot::new`. `ensure_output_caps` (resident.rs:~1480) judged
sufficiency against those logical caps, so the first UNPOOLED dispatch after a pooled era — the
O2b per-body LOD entry (`mesh_into_slot_sparse_lod`, whose L1 need is ~¼ of L0 and always below
the pool's 2.5×-churn region caps) or the monolithic pool-cap fallback — no-op'd and bound the
4-byte placeholders as the DC output: every vertex write dropped by the arrayLength guard →
all cells NO_VERT → zero quads → indirect index_count 0 → the body draws NOTHING. Exactly
"toggle Auto SDF LOD on → body disappears completely".

Fix (structural, No-Bandaids): `ensure_output_caps` now answers capacity questions from the
slot-OWNED buffers themselves (`verts.size()/32`, `idx.size()/4`, `cell_vert.size()/4` — a
placeholder holds 0 usable elements), grows when the OWN buffer is short, and re-anchors
`vert_cap`/`idx_cap` to own-buffer truth on any satisfied own-output request (pool callers pass
0/0 and keep their region caps). Generation bumps only on an actual buffer replacement. This
kills the whole pooled→unpooled transition class, including the previously-blind monolithic
fallback and the stale-cap-poisoned overflow predicate.

Gate: `tests_dc::sdf_lod_after_pooled_tiled_slot_not_empty` (failing-then-passing repro:
pooled tiled mesh → same-slot L1 LOD dispatch must match a fresh-slot L1 reference and must
recreate the own buffers). Full suite: 190 pass. No WGSL touched.

Manual test: two duplicated 1024³ sparse bodies, edit one (slots go pooled/tiled), Developer
Tweaks → enable "Auto SDF mesh LOD (O2b, octree)", zoom out until a body coarsens to L1 —
the body must stay visible (coarser mesh), and zooming back in must restore full detail;
toggling the tweak off must restore level 0 everywhere.

---

# O3 MEASURE-FIRST GATE — verdict + numbers (2026-07-16, measurement-only, uncommitted)

**Mandate:** decide whether to build O3 (mixed per-TILE LOD within one body) BEFORE building it —
the D1/D2 measure-first rule. NO production behavior changed. One `#[ignore]`d pure-CPU harness
added: `app/src/sdf_gpu_mesh/tests_dc.rs::o3_per_tile_lod_payoff` (classify → `tile_occupancy` →
grid-gradient curvature; no GPU, no flakiness). Run:
`cargo test -p app --bins o3_per_tile_lod_payoff -- --ignored --nocapture`.

## VERDICT — **DO NOT BUILD O3. The residuals (escalation + pool) are where the return is.**

The measured tile-level histogram kills the O3 premise: at every camera framing where LOD engages,
a single body's tiles want essentially ONE level. O3's projected-size axis adds **≤8%** over O2b's
per-body level, and its only real differentiator (curvature-adaptive quality) *increases* triangle
count — the opposite of the ">2× fewer tris" payoff O3 was pitched on. Seam/crack machinery
(transition strips or vertex snapping, §O3 sketch) is high implementation-risk for a sub-10% win
that the already-shipped O2b captures. **Ranked next moves below; O3 is dead last.**

## Q2 — Per-tile LOD headroom (THE decision datum) — tile histogram

Three archetype bodies (organic statue = lumpy sphere-union; flat-box = big rounded box; mech-bevel
= rounded box with a bored cylinder), world radius ~3, 1080p / 45° FOV, camera dist as a multiple of
body radius. Level per tile = `choose_lod_level(tile_projected_px)` clamped by a curvature cap
(normal-spread proxy: >0.18 → keep L0, >0.07 → ≤L1, else free). Cost proxy = Σ handle-bricks / 4^L
(surface quads shrink 4×/level). `o3-vs-o2b` >1 ⇒ O3 has fewer tris than whole-body O2b.

**res=1024 (the regime O3 targets):**

| body | framing | body px→L | tiles | **hist L0 / L1 / L2** | o3-vs-o2b | o3-vs-full |
|---|---|---|---|---|---|---|
| organic | near-fills | 0.67→L0 | 3087 | **3087 / 0 / 0** | 1.00× | 1.00× |
| organic | medium | 0.25→L1 | 3087 | **0 / 3087 / 0** | 1.00× | 4.00× |
| organic | far | 0.09→L2 | 3087 | **0 / 62 / 3025** | 0.92× | 14.6× |
| flat-box | medium | 0.25→L1 | 3752 | **0 / 3752 / 0** | 1.00× | 4.00× |
| flat-box | far | 0.09→L2 | 3752 | **0 / 0 / 3752** | 1.00× | 16.0× |
| mech-bevel | medium | 0.25→L1 | 3728 | **44 / 3684 / 0** | 0.94× | 3.77× |
| mech-bevel | far | 0.09→L2 | 3728 | **44 / 21 / 3663** | 0.75× | 12.1× |

(512³ is identical in shape: whole-body-single-level histograms; the largest split is organic-far =
2 L0 / 754 L1 → o3-vs-o2b 0.99×; mech-bevel keeps 72 curved tiles at L0 → o3-vs-o2b **0.73×**, i.e.
O3 *bigger*.)

**Reading:**
1. **The histogram is single-level almost everywhere.** A body of radius 3 seen from any distance
   where coarsening triggers (≥16× radius) has a projected-depth spread across its own tiles far
   below one LOD boundary → every tile picks the SAME projected level. **O2b's one-level-per-body
   already captures ~92–100% of the projected-size win.** O3's projected axis buys ≤8% (the handful
   of L0/L1 tiles in the "far" rows), and only on the nearest slice of a body straddling a boundary.
2. **The only within-body splits are curvature-driven, and they cost MORE tris, not fewer.**
   mech-bevel keeps 44–72 high-curvature tiles at L0 → o3-vs-o2b **0.35–0.94× (O3 is larger)**. That
   is a *quality* lever (feature preservation O2b can't express), NOT the triangle-reduction lever
   O3 was justified by. Flat/organic bodies (uniform curvature) split not at all → 1.00×.
3. **>2× fewer tris at equal near-camera detail? NO — nowhere.** Best projected-only case = 0.99–1.00×.
   The brief's "little (<30%)" threshold isn't even met; it's ~0–8%.

**Why the intuition ("far bodies are many-tiled → big win") fails:** many tiles ≠ many *distances*.
Perspective depth spread within one compact body is negligible against the camera distance that
makes it coarsen. O3 pays off only for bodies whose world extent is comparable to their camera
distance (terrain, a single wall spanning near-to-far) — not DCC assets. AZRA has no such bodies.

## Q1 — Where the remaining DC time goes (from the shipped GPU-timestamp attribution, this branch)

Already instrumented and captured in the LIVE ATTRIBUTION section above (commit 2533ca9 timestamps +
owner logs); re-stated as %-of-edit-latency. **512³ is solved** — a full-window re-mesh is 3.1ms
(23% surface) / 6.1ms (39% degenerate) GPU; nothing here dominates. The cost lives at **1024³**, one
scale-gesture on a large prim (edit→pixel latency ≈ the [SLOWPOLL] spike, 52–69ms):

| term | cost | % of a 60ms spike | status |
|---|---|---|---|
| **[SparseEscalate] full 1024 classify** (window >1/8 grid) | 17–30ms GPU+readback + 5–6ms validate, **×6 per gesture** | the multiplier for everything else | **TOP COST** — coalesced (slice a, 300ms cooldown) |
| **[SdfPool] rebuild balloon** (164k→490k transient bricks) | verts 1073MB + idx 846MB alloc; the 52–69ms spikes | ~100% of the >50ms polls | damped (slice b settle-trim) |
| **DC re-mesh** [DcResidentSparse] @ ext=801³ | 10–24ms GPU (vert 14.6 + quads 9.3) | ~30% | C2 tiled DC collapses it to 0.014–0.04ms GPU (measured) |
| **"54ms sparse atlas upload"** | was CPU index scans (`tile_occupancy` 47–63ms + bbox 7–8ms); the 8.4MB `write_buffer` is only 0.5–0.8ms | ~87% of that line | **fixed** (slice D fused/parallel scan → 3.9–6.3ms) |

**TOP cost = the escalation full-classify loop** — not because one classify is huge, but because the
>1/8 threshold re-fires it every drag tick, re-triggering the full bake + pool rebuild + re-mesh
underneath it. It is the multiplier on all three other rows.

## Q3 — The escalation residual (independent of O3, HIGH return)

`[SparseEscalate] window >1/8 grid → async FULL classify` fires **6× in one scale gesture** at 1024;
each fire = a full 1024³ classify (17–30ms GPU + 5–6ms frame-thread validate) + full sparse bake +
full re-mesh. The partial/scroll/rigid machinery **never engages for a scale of a large prim** (scroll
needs a rigid translate; rigid needs all-prims-equal-delta) — so a boolean-carver scale falls straight
to full. slice (a) shipped a 300ms per-body cooldown that *coalesces* fires (≤~3/s instead of
back-to-back), bounding the multiplier but not removing it. **Feasible residual fix: windowed
reclassify** — the >1/8 threshold forces a full reclassify when the true dirty union is far smaller;
classifying only the union window (the scroll path's `sparse_index_translate` proves windowed index
math is sound) would cut each fire from 1024³ to the edited sub-volume. Medium risk (classify
superset soundness — B1's modifier-pad contract must hold on the window), high value (attacks the
top cost directly).

## Q4 — The pool ceiling (Phase-4 multi-buffer is the real fix, NOT O3)

`SdfMeshPool` per-buffer cap = 1 GiB (`SDF_POOL_MAX_BUFFER_BYTES`). Saturation math (measured
constants): one 1024 body's transient peak = verts 1073MB + idx 846MB (490k bricks mid-gesture);
the C1 envelope gate (`tile_headroom_fits_measured_1024_envelope`) shows scene-A@1024 Σ per-tile
caps = 41.6M verts = **1.33GB > 1 GiB** → forces monolithic fallback. **Two 1024 bodies overflow →
per-body-slot fallback** (owner log). Saturation onset ≈ a single dense-surface 1024 body, or ~two
moderate ones. **Would O3 push the ceiling out? No** — pool pressure comes from the ACTIVE / near
bodies, which O3 keeps at L0 (near = full detail by construction). O3 only coarsens far/flat bodies,
which aren't the ones saturating the pool. **The pool ceiling is a multi-buffer (Phase-4) problem, and
O3 does nothing for it.**

## Ranked next 2–3 DC-cache moves (value / implementation-risk)

1. **Windowed escalation reclassify (Q3).** Highest value (attacks the measured TOP cost + its
   multiplier), medium risk (windowed classify soundness — reuse scroll index math + B1 pad
   contract). slice (a) already coalesces; this removes the per-fire 1024³ cost itself.
2. **Commit + envelope-fix C2 tiled incremental DC (Q1 row 3).** Already BUILT (uncommitted); measured
   to collapse the 10–24ms GPU re-mesh to 0.014–0.04ms. Remaining blocker = the C1 envelope (Σ per-tile
   caps burst the pool on wide 1024 shells → monolithic fallback). Shrink `TILE_VERTS_PER_BRICK_HEADROOM`
   toward the measured ~23 v/brick + lean on the overflow-floor machinery. Low-medium risk (gated,
   parity-tested), real GPU win on the 1024 track.
3. **Multi-buffer `SdfMeshPool` (Q4 / Phase 4).** The real fix for 2+ dense 1024 bodies; medium value
   (only bites at multi-1024 scenes today), medium risk (pool relocation across buffers). Unblocks the
   C2 envelope for big surfaces as a bonus.

~~4. O3 per-tile LOD~~ — **REJECTED by this measurement.** ≤8% projected win over O2b, negative on
curvature, high seam/crack risk. Revisit only if AZRA gains terrain/architectural bodies whose extent
rivals their view distance (none today).

---

- 2026-07-17 — **WINDOWED ESCALATED RECLASSIFY BUILT (uncommitted) — the Q3 residual, ranked #1.**
  The >1/8 oversized-window escalation no longer forces a FULL grid classify for windows in
  **(1/8, 1/2] of the grid's bricks**: `trigger_bake` now routes them to
  `spawn_async_window_reclassify` (bake_dispatch.rs) — the EXACT S2.5b/c frozen-partial machinery
  (`sparse_window_allocate_incompatible`: mini-classify + incremental alloc/retag/reclaim with the
  S2.5f shared free list) run on a background WORKER instead of the frame thread, landing as an
  ordinary `SparseBakeRequest::Partial` through `classify_tx` (same generation/regime validation,
  same dispatch-side atlas identity check, same pool-ceiling self-heal — which already read
  Partial `index_update` allocations). ZERO new index math: bricks outside the window keep tags +
  handles + pool content byte-untouched by construction; the mode-2 bake covers only the dilated
  window (∪ appended bricks), so the follow-up mesh takes the C2 dirty-tile path instead of a
  full re-mesh. **Fallbacks stay FULL by construction:** window > 1/2 of bricks (wholesale index
  replace also resets the pool watermark), bounds/res mismatch (frozen-partial candidate gate),
  structural marks, scroll-fallback, atlas identity/generation mismatch at dispatch, and walk
  corruption (Err → full classify ON THE SAME WORKER). **Cooldown:** the 300ms
  ESCALATED_FULL_MIN_INTERVAL stays on the full arm only; the windowed arm runs freely — the
  in_flight gate serializes flights per body (measured cost justifies it, below) — but still
  stamps `sparse_escalate_last` so `sparse_quiet` (settle-trim damping) + the PT gesture
  heuristics keep seeing the gesture. Telemetry: `[SparseEscalate] … mode=windowed|full …
  wb/total bricks`, worker line `[SparseWindowed] body=… classify/apply ms + alloc/retag/reclaim`.
  **MEASURED (`w_measure_windowed_walk_1024`, #[ignore]d, sphere+subtract at 1024³, 257k bricks):**
  the worker walk = **10.0ms** at 2.7% of bricks, **18.6ms** at 9.1%, **21.5ms** at 21.6%,
  **25.3ms** at 49.3% — vs the full pipeline it replaces (17–30ms GPU classify + readback +
  **5–6ms frame-thread validate** + full 1024 mode-2 bake ~56ms + full re-mesh 10–24ms + full
  atlas index scans). Frame-thread cost of a windowed tick ≈ 0 (spawn + land); the bake is
  window-sized; the mesh is dirty-tile-sized (C2). Expected per-gesture saving at the owner's
  6-fire scale gesture: ~6 × (full bake 56ms + full mesh − window-fraction equivalents) of GPU
  work plus all 6 × 5–6ms frame-thread validates — and the pool-rebuild balloon shrinks because
  windowed fires recycle through the free list instead of re-packing 0..N handles wholesale.
  **Gates (all green, suite 193):** `sparse_oversized_midsize_window_takes_windowed_reclassify`
  (routing: midsize → windowed worker Partial with sub-grid window, no GPU classify queue, no
  cooldown deferral; >1/2 unchanged full via the pre-existing tests);
  `sparse_windowed_escalation_matches_full_classify` (GPU, far-move gesture, 832/4096 bricks:
  windowed walk + windowed bake vs from-scratch full classify + full bake — 0 sign flips vs the
  edited oracle, both-handled window voxels BIT-identical to the reference, every iso-crossing
  brick handled, sentinel signs agree, out-of-window tags + pool bit-untouched; alloc 312 with
  179 recycled + 180 reclaimed exercises the free list);
  `sparse_windowed_reclassify_window_boundary_surface_preserved` (adversarial: an UNEDITED
  sphere's surface straddling the window face — seam crack-free, out-of-window content verbatim).
  NOTE for the reference comparison: handled SETS legitimately differ (the reference full
  classify over-allocates its conservative margin; the incremental walk keeps handles it cannot
  PROVE vacated) — both are mesh-benign supersets of the iso-crossing set, which is the
  structural criterion the gate asserts (plus sign-agreement inside superset bricks).
  Duplicate-prim 1024 crash (Q4): not this bug; no new evidence found — but the windowed path
  reduces the transient pressure on the 1 GiB pool ceiling (no wholesale re-pack mid-gesture),
  and the dispatch-side ceiling self-heal covers the windowed `index_update` path unchanged.
  **Manual owner test:** 1024³ sparse body (sphere + Subtract duplicate), scale-drag the subtract
  prim. EXPECT: `[SparseEscalate] … mode=windowed … bricks=X/2097152` lines during the gesture
  (X between ~262k and ~1.05M), `[SparseWindowed] … classify=…ms apply=…ms` worker lines,
  NO `mode=full` lines unless the window exceeds half the grid, [SLOWPOLL] spikes markedly
  reduced vs log 7, and the settle state identical to before (final full only if a >1/2 window
  or staleness event fires). Undo/redo and body-switch behave as before (structural marks still
  route FULL).

## Correctness bug spotted (report only — NOT fixed here, separate task per brief)

Owner-reported CRASH: duplicate a primitive at 1024³. Chain confirmed by read (not reproduced/
instrumented — this is a measurement task): `gizmo.rs:415 duplicate_prims_no_undo` →
`add_prim_to_graph` → mark-dirty → `dirty.rs:128 rebake_no_undo` → sparse bake / `SdfMeshPool` path.
**Prime suspect = the Q4 pool ceiling**: duplicating a prim into a body that is already a dense 1024
surface pushes the transient brick count (and the R3b/tile caps) toward/over the 1 GiB per-buffer cap
mid-rebake; the per-body-slot fallback / grow-rebuild path (`SdfMeshPool::grow_slot`/`trim_slot`) is
the likeliest device-loss site. Not diagnosed in depth here; hand to the crash-fix task with the
`on_uncaptured_error`/DeviceLost reason capture (R3a, already shipped) enabled to name TDR vs OOM.

## Files touched (uncommitted, measurement-only)
- `core_engine/app/src/sdf_gpu_mesh/tests_dc.rs` — added `#[ignore]`d `o3_per_tile_lod_payoff` +
  three pure helpers (`projected_voxel_px`, `tile_curvature`, `curvature_max_level`). No production
  code, no shaders, no GPU-path changes.
- `docs/planning/sdf/CACHE_REMESH_512_REVIEW.md` — this section.
- Gates: `cargo build -p app` green; app bin test suite unchanged at 197 + the 1 new ignored test.

---

# 2048-VIRTUAL FEASIBILITY SPIKE (2026-07-17, measurement-only, uncommitted)

**Mandate:** can the EXISTING sparse stack (GPU classify → SdfBrickAtlasGpu → mode-2 sparse
bake → windowed/monolithic DC mesh) run a 2048³ body end-to-end, and which component breaks
first? Method: `spike_sparse_2048_feasibility` (`app/src/sdf_gpu_mesh/tests_sparse_bake.rs`,
`#[ignore]`d) drives the REAL production encode/bake/mesh at 1024/1536/2048 on two shapes
(moderate = small sphere; realistic = the canonical sphere−subtract), every GPU stage inside a
validation error scope. The 1024 production cap is bypassed by overriding the queued request's
resolution — no production behavior changed. Run:
`cargo test -p app --bins spike_sparse_2048_feasibility -- --ignored --nocapture --test-threads=1`.

## VERDICT — **2048 RUNS end-to-end TODAY on the CPU-finalize fallback arm.**

One hard break (GPU classify-finalize dispatch limit), one hard ceiling (mesh-pool 1 GiB),
several CPU costs that scale 8×. No device loss, no TDR, no u32 overflow anywhere on the sparse
path (all addressing is brick-local or brick-id-sized; the 8.6e9 res³ flat index simply never
exists in mode 2). Triangle output scales EXACTLY 4×/octave (2.91M → 6.56M @1536 (2.25×) →
11.66M tris @2048) — structural proof the pipeline is CORRECT at 2048, not just alive.

## Measured (dev GPU, max_storage_buffer_binding_size = 2.15GB, max_workgroups_per_dim = 65535)

| stage | 1024 realistic | 1536 realistic | 2048 moderate | 2048 realistic |
|---|---|---|---|---|
| brick index | 128³ = 2.1M (8MB) | 192³ = 7.1M (27MB) | 256³ = 16.8M (64MB) | same |
| GPU classify+readback | 24.3ms | 14.8ms | 10.6ms | 12.7ms |
| GPU finalize | OK | OK | **VALIDATION FAIL** | **VALIDATION FAIL** |
| frame-thread validate | 5.8ms | 18.7ms | n/a (fallback) | n/a (fallback) |
| CPU fallback decode+finalize | — | — | 58 + 351ms | 59 + **449ms** |
| allocated bricks | 182k | 408k | 329k | **723k** |
| pool_buf / cell_vert | 355MB each | 797MB each | 643MB each | **1413MB each** |
| index_content_scan | 4.5ms | 10.8ms | 21.8ms | 23.4ms |
| upload_index | 4.6ms | 13.4ms | 24.6ms | 27.4ms |
| mode-2 bake (z-slabs) | 55.9ms (16) | 116.7ms (24) | 126.5ms (32) | 219.0ms (32) |
| monolithic DC mesh | 29.7ms | 62.5ms | 49.0ms | 118.3ms |
| verts / tris | 1.46M / 2.91M | 3.28M / 6.56M | 1.05M / 1.93M | 5.83M / 11.66M |

## Per-suspect verdicts

1. **Resolution cap — WORKS, one constant.** The hard max is `max_sparse_resolution() == 1024`
   (`app_gpu/src/gpu_jobs.rs:66`), consumed by `BodyVolumeCache::max_mesh_resolution()`
   (`body_volume_cache/mod.rs:781`) which is the SINGLE clamp every consumer
   (`effective_resolution`, `compute_resolution_for_body`, the Grid picker) reads. Widening it
   is a one-liner — but must land LAST (after ranked items 1–3 below), else the picker exposes
   the finalize break to users.
2. **Index scale @ 16.7M entries — CPU costs bite, none fatal.**
   `decode_gpu_finalized_index` frame-thread validate: 5.8ms@2.1M → 18.7ms@7.1M → extrapolated
   **~45ms@16.7M** (unmeasurable directly — the GPU finalize breaks first, see 5). The
   CPU-finalize fallback (`sparse_index_from_verdicts` dilate+alloc) = **351–449ms @16.7M**
   plus 58ms verdict decode — worker-side in production, but it is every full classify's cost
   at 2048 until the finalize fix lands. `index_content_scan` (slice-D fused rayon) holds up:
   21.8–23.4ms; `upload_index` 24.6–27.4ms (64MB write_buffer).
3. **Brick-pool arena — fits at rest, breaks under gesture churn.** realistic-2048 = 723k
   bricks → pool_buf 1413MB (+25% ensure_capacity pad on grow), bound as ONE entire storage
   binding (mode-2 bake binding 2, mesh binding 7) — fits 2148MB with only **1.5× margin**. The
   measured 1024 gesture grew the surface 2.3× mid-drag; the same churn at 2048 (1.7M bricks =
   3.3GB) **exceeds the binding limit** → validation error where 1024 merely spiked. The
   dispatch-side pool-ceiling self-heal must learn a hard clamp at the binding limit BEFORE the
   cap widens.
4. **Mesh pool ceiling — a single realistic 2048 body ALREADY exceeds it.** Monolithic sizing
   (48 v/brick) wants 34.7M verts = 1.11GB > `SDF_POOL_MAX_BUFFER_BYTES` (1 GiB) → per-body
   slot fallback (works — this spike ran unpooled). The TILED sizing (28 v/brick) = 648MB — ONE
   2048 body fits the pool tiled, two never. Per-body fallback caps: the dense ceiling clamped
   the vert cap to 33.5M (spike log `caps v=33554432`) — actual need 5.8M, so truncation only
   begins at ~33.5M verts (a surface ~6× denser than the spike's); overflow is counter-detected
   and grows. **Phase-4 multi-buffer pool covers exactly this item (verts/idx pool) — it does
   NOT cover suspects 3 or 6 (brick arena + cell_vert are separate single-binding buffers).**
5. **Bake dispatch — WORKS; the break is the classify FINALIZE, not the bake.** Z-slab mode-2
   bake: 32 slabs, 219ms GPU, no TDR, workgroup dims 512 ≤ 65535. **THE FIRST HARD BREAK:** the
   STAGE-3 finalize passes (dilate/scan/emit in `encode_gpu_classify_dispatch`,
   `app_poll_bake.rs` `linear_groups = (total+255)/256`) dispatch **65536 workgroups at 256³
   bricks > max_compute_workgroups_per_dimension (65535)** → validation error, the whole
   classify submit discarded. The cliff is exactly between 1536 (27648 groups, OK) and 2048.
   Fix = 2D dispatch grid + flattened gid in the finalize entry points (volume_bake.wgsl) —
   small but a shader change (batch with other WGSL edits). `block_scan`'s single-workgroup
   loop over 65536 block sums is untested at 2048 (unreachable until the dispatch fix) —
   verify then. No u32 voxel-index overflow found: mode 2 never forms a res³ flat index
   (brick-local `handle*512+local`, max ~370M ≪ u32::MAX at 723k bricks; overflow would need
   ~8.3M allocated bricks).
6. **cell_vert brick-local — fits, same churn caveat as 3.** 723k × 512 × 4B = 1413MB single
   binding, exact-sized (`ensure_output_caps` deliberately unclamped, assert green). Breaks at
   ~1.05M allocated bricks (2.1GB) — i.e. the same gesture-churn regime as the arena.
7. **DcParams / sdf_mesh.wgsl addressing — WORKS.** Windowed monolithic mesh at ext ≈1100³
   cells: 118ms, dispatch dims ≤ 512 per axis, output counters sane, no packing limit hit.

## VRAM budget — owner's 4090 (24GB), one realistic-2048 body

Settled: pool 1.42GB + cell_vert 1.41GB + index 64MB + verts/idx (settle-trimmed ~1.25× need)
~0.33GB ≈ **3.2GB**, + PT BLAS/TLAS over 11.7M tris (~0.8–1.0GB) + G-buffer/raster ≈ **~4.5GB
total** — comfortable. Mid-gesture transient with today's 2.5× churn headroom ≈ pool-region
2.6GB + arena/cell_vert growth — **the transient, not the steady state, is what overflows.**
Two realistic 2048 bodies settled ≈ 7–8GB — feasible ONLY with Phase-4 multi-buffer for the
mesh pool and per-body atlases (already per-body today).

## RANKED work list to ship "2048 virtual"

1. **Finalize 2D dispatch fix** — the `linear_groups` dispatches in
   `encode_gpu_classify_dispatch` + the finalize entry points in `volume_bake.wgsl` (flatten a
   2D workgroup grid). Removes the ONLY hard break. Risk: low (parity gate exists —
   `sparse_windowed_escalation_matches_full_classify`). Scope: small (1 shader edit + encode
   math + re-run spike). Verify `block_scan` at nb=65536 while there.
2. **Validate off the frame thread at 16.7M** — the ~45ms extrapolated
   `decode_gpu_finalized_index` walk (and the 64MB index clones per flight) move to the
   classify worker (the windowed-reclassify precedent). Risk: low. Scope: small-medium.
3. **Hard-clamp growth at the binding limit** — arena `ensure_capacity`, cell_vert sizing, and
   the pool-region 2.5× churn headroom must clamp to `max_storage_buffer_binding_size` and
   degrade (escalate/fallback) instead of erroring — at 2048 the measured 1024-gesture churn
   crosses the line (same class as the duplicate-prim 1024 crash suspect). Risk: medium.
   Scope: medium.
4. **Widen `max_sparse_resolution` → 2048** (+ picker admission + extend the
   `sparse_1024_runs_like_512` octave gate). One-liner + tests; LAND LAST, after 1–3.
5. **Phase-4 multi-buffer `SdfMeshPool`** — required for >1 body at 2048 (and already forced
   per-body at 1× realistic-2048 monolithic). Covers suspect 4 ONLY; suspects 3/6 need item 3.
   Risk: medium. Scope: large (already planned).
6. **Measure the partial/windowed machinery at 2048** — escalation walk, tiled path at ~15.5k
   live tiles, settle-trim — none exercised by this spike. Scope: follow-up measurement.

**Executive answer:** 2048-virtual is **4 work-items away** (finalize 2D dispatch, worker-side
validate, binding-limit growth clamps, then the cap widen), with Phase-4 multi-buffer required
only for multi-body 2048 scenes; the first user-visible milestone is **a single 2048³ body
baking, meshing and drawing correctly in Object Mode** (~350ms full-rebuild GPU cost: 13ms
classify + 219ms bake + 118ms mesh), which items 1–4 alone deliver.

## Files touched (uncommitted, spike-only)
- `core_engine/app/src/sdf_gpu_mesh/tests_sparse_bake.rs` — `spike_sparse_2048_feasibility`
  (+ `spike_request`/`spike_run`/`pop_validation_err` helpers), `#[ignore]`d. No production
  code, no shader edits.
- `docs/planning/sdf/CACHE_REMESH_512_REVIEW.md` — this section.
- Gates: `cargo build -p app` green; `cargo test -p app --bins -- --test-threads=1` = 193
  passed, 10 ignored (9 pre-existing + this spike).

---

# 2048-VIRTUAL SHIPPED (2026-07-17, uncommitted — the spike's 4 ranked items)

All four ranked items landed, in order, each gated before the next. The sparse stack now runs
2048³ end-to-end through the FULL GPU path (classify → GPU dilate/scan/emit finalize → atlas →
z-slab mode-2 bake → windowed DC mesh); the CPU-finalize fallback arm is no longer exercised at
any resolution (it survives only as the degraded no-finalize-pipelines path).

## Item 1 — classify-finalize 2D dispatch grid (the one hard break)
- `volume_bake.wgsl`: `classify_finalize_dilate`/`scan`/`emit` now flatten a 2D workgroup grid
  (`fin_flat_wg = wg.x + wg.y * nwg.x`; block index × 256 + lid). ONE batched WGSL edit. The
  scan's block-sum write gained a `wg_flat < nb` guard (tail workgroups of a 2D grid would have
  written INSIDE the index region). For 1D dispatches (y=1) the flatten degenerates to wg.x —
  bit-identical, proven by the untouched classify parity tests (3/3 green).
- `app_poll_bake.rs`: `classify_finalize_grid(groups, max_per_dim)` splits the 65536 linear
  groups at 2048 into 65535×2; dilate/scan/emit dispatch (gx, gy, 1).
- `block_scan`'s serial loop verified at nb = 65536 (previously unreachable).
- NEW GATE `classify_finalize_2d_dispatch_crosses_65535_groups_matches_cpu` (in-suite, ~3s):
  real production encode at 2048 (the smallest possible crossing — 1536 is only 27648 groups),
  GPU-finalized index BIT-IDENTICAL to `sparse_index_from_verdicts` on the same verdicts.

## Item 2 — index validate off the frame thread
- The `gpu_finalized` arm of `poll_gpu_classify_readbacks` no longer clones (64MB at 2048) nor
  validates (~46-50ms at 16.7M entries, measured) on the frame thread. The mapped staging buffer
  travels to a worker thread (wgpu::Buffer is Send+Sync; map already completed) which clones,
  unmaps, runs the UNCHANGED `decode_gpu_finalized_index` validation, and lands through
  `classify_tx` — the exact verdict-arm precedent, same ONE landing point.
- Validation is NOT weakened (lab-cache binding-collision lesson): a corrupt index logs loudly
  and never lands; the 10s bake timeout re-dirties (the P1-#7 dead-worker soundness contract).
  Map-FAILED still aborts synchronously on the frame thread.

## Item 3 — growth paths hard-clamped at the binding limit
- NEW canonical pair in `graphics/src/sdf_brick_atlas.rs`: `pool_ceiling_bricks(max_bind)` and
  `padded_capacity_clamped(needed, ceiling)`. The dispatch-side self-heal
  (app_poll_bake.rs `sparse_over_ceiling` → resolution-drop + toast) and the atlas grow pad now
  share ONE ceiling — they can no longer disagree.
- `SdfBrickAtlasGpu::ensure_capacity` + `new`: the ×1.25 pad and quantum rounding clamp at the
  ceiling (pool AND index). Previously the pad could hand the driver an unbindable pool right
  after the exact `allocated` check passed (at 723k bricks the pad alone is +353MB; the
  measured 1024-gesture 2.3× churn at 2048 crosses 2.15GB). Refusing the PAD is the graceful
  degradation: content always fits, only churn headroom shrinks near the ceiling; needing MORE
  than the ceiling stays refused upstream by the resolution-drop self-heal (never a validation
  error, never an under-need capacity — debug_assert documents the caller contract).
- cell_vert: already structurally bounded — cv = allocated × 2048B and allocated is capped by
  the SAME pool ceiling before any dispatch; the creation-site assert stays.
- Mesh pool: `safe_max` already min(max_bind, 1GiB); `grow_slot` already dense-ceiling clamped
  from max_bind — no change needed (audited).
- NEW GATE `grow_pad_clamps_at_binding_ceiling` (graphics, pure): full pad far from ceiling,
  pad refused at the boundary, exact-fit at the ceiling, always bindable.
- **PT duplicate-prim crash (owner's open 1024 bug): plausibly fixed by this item.** The crash
  class: a duplicate doubles the surface mid-scene → allocated bricks spike → the next
  ensure_capacity grow (pad included) crosses the binding limit → validation error → invalid
  encoder cascade → device loss. The PT snapshot pump's temp slot sizes everything FROM the
  atlas (cv = allocated × 2048, verts dense-ceiling-clamped in ensure_output_caps) and its
  readback stagings are MAP_READ (no binding limit), so bounding the atlas bounds the pump.
  With the pad clamp + the shared ceiling, no path can create an unbindable pool; the worst
  case is the loud resolution-drop toast. Needs an owner repro to confirm.

## Item 4 — cap widened LAST: `max_sparse_resolution` 1024 → 2048
- `app_gpu/src/gpu_jobs.rs` — the single clamp every consumer reads
  (`max_mesh_resolution` → `effective_resolution` / `compute_resolution_for_body` / picker).
- Grid picker (`properties_render.rs`): 2048³ entry added, opt-in per body (never a default),
  with an honest hover (VRAM class, ~0.5s first bake, self-heal note, 16-24GB GPU).
- NEW GATE `sparse_2048_runs_like_1024_end_to_end` (#[ignore]d — ≈4GB transient VRAM):
  the octave contract one octave up, through the REAL picker/admission path.

## Measured (this session, dev profile, same dev GPU as the spike)
| stage (realistic sphere−subtract) | 1024 | 2048 |
|---|---|---|
| GPU classify+finalize+readback | 26.0ms | 31-33ms (65535×2 groups) |
| index validate (now WORKER-side in prod) | 5.7ms | 46-50ms |
| bricks | 182k (355MB pool) | 723k (1413MB pool) |
| mode-2 bake (z-slabs) | 80.5ms | 287-299ms |
| windowed monolithic DC mesh | 34.0ms | 131-137ms |
| verts / tris | 1.46M / 2.91M | 5.83M / 11.66M (exactly 4×/octave) |
| **full-rebuild GPU total** | **~140ms** | **~460ms dev-profile** (the spike's ~350ms class) |

Classify at 2048 = 31ms vs the CPU-fallback arm's 58+449ms — the fallback is dead. The spike
test now ASSERTS the GPU-finalize arm at every resolution (the CPU-fallback branch was removed
from `spike_run`); all 5 spike configs RAN END-TO-END.

## Gates
- `cargo build -p app` after every item; `cargo test -p graphics` 44/44 (WGSL parse + new clamp
  test); `cargo test -p app --bins -- --test-threads=1` **194 passed / 0 failed / 11 ignored**
  (193 baseline + the new 2D gate in-suite; new octave gate ignored); classify parity 3/3
  bit-identical; `cargo test -p core_math` 509 green (untouched, sanity).

## Owner manual test
1. Per-Body Cache ON, create/select an SDF body → Properties ▸ Render ▸ **Grid: 2048³** (new,
   rightmost; hover explains cost). It is opt-in — nothing defaults to 2048.
2. Expect a first full bake ≈ 0.5-1s (release ~0.35-0.5s): console shows
   `[GpuClassify] body=… verdicts+dilate+alloc(gpu)+readback=~30ms clone+validate(worker)=~50ms bricks=~700k/16777216`
   then `[SparseBake] dispatch body=… res=2048³ … pool=~1400MB` and the mesh lands.
3. Drag a prim: partial/windowed path as at 1024. If the surface grows past the pool ceiling
   (~1.05M bricks on a 2.15GB-binding GPU) expect the SELF-HEAL toast
   "Surface too dense for 2048³ on this GPU — resolution dropped to 1024³" — that is the
   designed graceful degradation, not a bug.
4. No frame-thread [SLOWPOLL] from classify at 2048 (the validate line now says `(worker)`).

## Files touched (uncommitted)
- `graphics/src/volume_bake.wgsl` — 2D flatten in the 3 finalize entries + scan tail guard
- `graphics/src/sdf_brick_atlas.rs` — `pool_ceiling_bricks` / `padded_capacity_clamped` +
  clamped `new`/`ensure_capacity` + `grow_pad_clamps_at_binding_ceiling`
- `app/src/app_poll_bake.rs` — `classify_finalize_grid` + 2D dispatch; worker-side
  clone+validate+land for gpu_finalized flights; self-heal ceiling now the shared fn
- `app_gpu/src/gpu_jobs.rs` — `max_sparse_resolution` 2048 (+ rationale doc)
- `app/src/properties_render.rs` — 2048³ picker entry + hovers
- `app/src/sdf_gpu_mesh/tests_sparse_bake.rs` — spike asserts the GPU arm; new
  `classify_finalize_2d_dispatch_crosses_65535_groups_matches_cpu` (in-suite) +
  `sparse_2048_runs_like_1024_end_to_end` (#[ignore]d)

## Still open (unchanged from the spike's ranked list)
- Item 5: Phase-4 multi-buffer `SdfMeshPool` — required for >1 realistic-2048 body per scene.
- Item 6: measure the windowed/escalation machinery at 2048 under real gestures.

---

# 2026-07-17 — Beveled-subtract "stringy sheets" diagnostic: H2 (stale bricks) EXONERATED

Owner screenshot: subtracting a per-edge-beveled cube carves correctly but the carved surface is
full of thin vertical stringy sheets/stripes. Two hypotheses: H1 = the per-edge bevel eval;
H2 = a fresh regression in the incremental machinery (windowed reclassify f833129 / 2048 items
88eef29) leaving retained-but-stale brick content.

## Verdict: NEITHER reproduces in the geometry pipeline — the artifact is SHADING-level (H1-class)

Every layer was gated on hardware (RTX-class dev GPU, new permanent tests):

| Layer | Gate (all GREEN) | Result |
|---|---|---|
| CPU vs WGSL bevel eval | line-by-line (sdf_shapes.rs:174 ≡ volume_bake.wgsl:278 ≡ sdf_prim_eval.wgsl:418) | identical formulas, identical clamp/op order |
| Full classify + mode-2 bake | `sparse_beveled_subtract_full_bake_matches_oracle` (k=0 and k=0.08) | 0 sign flips, near\|dv\|=0.0000, bit-exact vs mode-0 |
| Windowed escalation reclassify (the f833129 path), carve gesture | `sparse_windowed_beveled_subtract_carve` (256³, drag 1.2 through a box body) | window bit-exact vs full-classify reference, out-of-window byte-untouched, 0 crossing holes |
| Bevel-slider partial + C2 incremental dirty-tile mesh | `sparse_beveled_subtract_mesh_verts_on_isosurface` | every DC vertex ≤ 0.24 voxel from the true field's zero set (full AND incremental rounds) |
| O1 workgroup coarse cull (74778de — had ZERO test coverage; production enables it for beveled scenes) | `gpu_classify_coarse_cull_matches_cpu_classifier` (canonical + beveled-subtract scenes) | bit-identical verdicts to the CPU brute force |

Harness note: `run_windowed_escalation_gate` gained a `require_append` parameter — a carve over a
fully-handled window legitimately appends nothing (handled bricks are compatible-by-table and are
refreshed by the windowed bake); the two original far-move gates still require appends.

## Root cause (see BEVEL.md §11 2026-07-17 + COMPLETE_BUG_TRACKER.md BEVEL-NOTCH update)
Deferred lighting marches `soft_shadow` / `calc_ao` / inline reflections against the ANALYTICAL
`scene_sdf` (deferred_entry.wgsl:471-473, 542, 594-606) even though geometry is the baked DC mesh.
The tri-planar hard-max mixed-radii bevel field (the KNOWN open BEVEL-NOTCH bug,
sdf_prim_eval.wgsl:418) has C0 ridge surfaces parallel to the box's vertical edges and
systematically UNDERESTIMATES distance in concave regions — the shadow/AO march renders those as
thin vertical stripe bands exactly on carved (subtract) surfaces.

## Owner manual test scenario
1. Repro: box body → add cube prim → op Subtract → set per-edge bevels (mixed radii) → observe
   stripes on the carved surface in full shading.
2. Switch shading to **Solid/clay** (no analytical shadow/AO marching): stripes should VANISH
   while the carved geometry stays identical → confirms shading-level.
3. Orbit/zoom (mesh unchanged) — stripe bands shift with light/view, never with brick columns.
4. If stripes PERSIST in Solid shading, the diagnosis is wrong: save the .fl4 and report (the 5
   geometry gates would need that scene as a counterexample).

Fix direction (NOT rushed here, per the Per-Edge Bevel Corner Rule + memory
`project_subbody_eval_order_bug` "both hard, do not rush"): radius-aware smooth combine of the 3
planar bevels — never hard max — ported to BOTH sdf_prim_eval.wgsl and volume_bake.wgsl (+ CPU
sdf_shapes.rs) in ONE task, gated by the existing new tests plus a Blender-oracle visual check.

---

# 2026-07-17 — 2048 PERF: cost-based walk escalation + settle-loop consumption (uncommitted)

Owner's 2048 session log named two problems: **P1** — `[SparseWalk] classify=310-363ms` PER TICK
on the FRAME THREAD for ~900³ windows (≈1.6M bricks = 8.6% of the grid — UNDER the 1/8 fractional
escalation threshold, so the windowed worker never engaged while windows that DID escalate ran
fine off-thread); **P2** — the same window re-walked 8-12+ consecutive times with apply=0.0ms and
+0 bricks, each a full frame-thread classify.

## P1 — escalation is now COST-based; the fraction is a secondary guard
`bake_dispatch.rs`: two named consts with the measurement basis documented on them —
`SPARSE_WALK_FRAME_BUDGET_MS = 8.0` (inside the 12ms Frame Budget) and
`SPARSE_WALK_NS_PER_BRICK = 200.0` (≈0.2µs/brick, linear: owner 2048 log 310-363ms/1.6M; owner
1024 log 5-45ms; `w_measure_windowed_walk_1024` 10ms/57k). The frozen-partial decision now
computes `est_walk_ms = window_bricks × 0.2µs` and routes ANY window over the budget to the SAME
`spawn_async_window_reclassify` worker the (1/8, 1/2] fractional escalation uses — no second
off-thread path. `>1/2` still routes FULL (wholesale index replace + watermark reset, cooldown
unchanged); structural marks still route FULL by construction (regions cleared → no frozen-partial
candidate — pre-existing tests pin this). Budget crossing ≈ 40k window bricks, an order of
magnitude above real 512³ drag windows, so small-window latency is untouched (pinned by the
in-test control). Telemetry: the `[SparseEscalate] mode=windowed …` line now says WHICH guard
fired (`window >1/8` vs `est frame-thread walk N.Nms > 8ms budget`).

**Expected frame-thread cost per tick at 2048 (owner's gesture windows, ~1.6M bricks):**
before = 310-363ms synchronous classify; after = ~0 (spawn + land — the ~320ms walk runs on the
worker, same cost, off-thread), plus the windowed mode-2 bake + C2 dirty-tile mesh as before.

## P2 — the settle loop: consumption proven, the real re-arm fixed, the loop coalesced
Static trace of every region producer/consumer (dirty.rs marks, scroll slabs, escalation-deferral
restore, dedup-skip, rigid-translate, dispatch consumption) shows the machinery CONSUMES
correctly: a no-op walk consumes its regions at dispatch, its completion stamps the entry hash
via `record_sparse_partial_bake`, and an identical re-mark then dedup-skips (regions consumed,
no dispatch). What the trace DID find and this change fixes:
1. **A real no-op re-arm:** every sparse partial dispatch — including no-op walks — armed
   `pending_retighten`, state contract C3's `check_idle_retighten` can NEVER consume for a
   brick-resident body (it skips them). That fed wasted settle drains and fired the misleading
   `[RetightenArm] by=partial_dispatch while IDLE — phantom dirty regions?` tripwire on bodies
   whose "phantom" regions were real but whose arm was garbage. Sparse (brick-resident) partial
   dispatches no longer arm it; dense partials keep the arm (their settle re-tighten is
   load-bearing).
2. **The loop's tick multiplier:** each identical-window walk requires a NEW source hash (else
   the dedup consumes) — i.e. a hash-changing mark per tick draining one-per-frame behind the
   310ms synchronous walk backlog (a 12-tick drain = 3.7s of "idle-looking" re-walks). With P1
   the walk is a worker FLIGHT and the in_flight gate coalesces every tick that lands mid-flight
   (regions union, ONE walk per flight over the union window). **Expected settle-loop count:
   8-12 identical frame-thread walks → ≤1-2 worker walks, frame thread ≈ 0 either way.**
3. **Feeder provenance is armed:** the cost-routed windowed arm stamps `sparse_escalate_last`
   (pre-existing behavior of the windowed arm), so `escalation_hot` now fires the writer
   field-diff prints in `prim_actions.rs` during exactly this loop — if an app-layer writer
   really is mutating hashed state at idle, the next owner log NAMES the prim uid + field +
   both values. No same-window-skip hack anywhere: 'no content change' vs 'same window, new
   content' is distinguished by the source hash, never window identity.

## Gates (all green; suite = 198 expected + 2 new = 200 passed, 12 ignored, 0 regressions)
- NEW `sparse_window_over_cost_budget_routes_to_worker` — a 1024³ window at ~2.6% of the grid
  (under 1/8) but ~55k bricks (over the 8ms budget) routes to the windowed worker (ONE async
  flight, no GPU full classify, no sync walk), lands as a sub-grid Partial whose brick count is
  asserted over-budget AND under-fraction (pins that COST decided); control: a small window
  still walks synchronously.
- NEW `noop_walk_consumes_dirty_state_and_never_rearms` — (1) a no-op walk (far-from-surface
  window: +0/0/0, index_update=None) consumes dirty_bodies + regions at dispatch, no re-walk
  next tick without a new edit, and does NOT arm pending_retighten; (2) after the flight
  records, an IDENTICAL re-mark dedup-consumes (no re-walk); (3) anti-hack guard: a real
  hash-changing edit at the SAME window still walks.
- Pre-existing windowed-reclassify parity/boundary gates + bevel-subtract gates + full cache
  suite: green. (Note: one single-process full-suite run can abort abnormally mid-GPU-suite at a
  RANDOM test — reproduced on clean HEAD 637ef32, environmental GPU pressure, NOT this change;
  every module passes run per-module.)

## Files touched (uncommitted)
- `core_engine/app/src/body_volume_cache/bake_dispatch.rs` — the two consts, the cost-based
  `windowed_ok` decision + reason telemetry, the brick-resident `pending_retighten` skip, 2 tests.
- `docs/planning/sdf/CACHE_REMESH_512_REVIEW.md` — this section.

## Owner manual test scenario
1. Per-Body Cache ON, one 2048³ sparse body (sphere + Subtract prim), drag/scale the subtract
   prim through the surface (the log-7 gesture).
2. EXPECT during the gesture: `[SparseEscalate] … mode=windowed est frame-thread walk ~320.0ms >
   8ms budget at 2048³ … bricks=…/16777216` lines and `[SparseWindowed] body=… classify=…ms …
   (async — off the frame thread)` worker lines. **NO `[SparseWalk] classify=300+ms` lines** —
   any remaining `[SparseWalk]` prints must be small (<~8ms estimated ⇒ measured ≲10ms).
3. EXPECT no 300ms+ `[TriggerBake]` spikes from the walk; viewport stays interactive mid-gesture.
4. Release and idle: the settle drain that used to print 8-12 identical `[SparseWalk]` windows
   now shows at most 1-2 `[SparseWindowed]` flights (ticks landing mid-flight defer and union),
   then silence. NO `[RetightenArm] by=partial_dispatch … phantom dirty regions?` lines for the
   sparse body.
5. If identical `[SparseWindowed]` windows still repeat at TRUE idle (hands off), the
   escalation-hot writer prints (`[HealTrace] write_prim_transform … dpos=` /
   `mirror_prim_to_graph uid=… CHANGED:`) will name the feeder — report that line.
6. Regression sweep: 512³ drag nudges still bake per-frame (sync partials), undo/redo and body
   switch (Tab) still route FULL, Save/Load unchanged.

---

# Session 2026-07-17 — Phase 4: the multi-buffer mesh pool (buffer GROUPS)

## What shipped (uncommitted, feat/sparse-3a)
`SdfMeshPool` capacity is no longer ONE 1 GiB-self-capped verts/idx pair — it is a **set of
buffer groups** (`Vec<SdfPoolGroup>`, `pool.rs`). Each group owns its own
verts/idx/indirect/tile_info/tile_counters quintet (each buffer ≤ `SDF_POOL_MAX_BUFFER_BYTES`
= 1 GiB — the reliable-allocation cap stands, now per group), its own bump allocator, args
free-list, and its **own generation counter**. Shared across groups: the instance buffer,
tile_scratch, tile_info_stage, tile_params (transient per-dispatch, never entry-indexed).

- **A body's slot lives ENTIRELY in one group** (`SdfPoolSlot.group`); a tiled body's tiles,
  args block, and counter block never span groups — the C2 entry space stays self-contained.
- **Placement:** first group with bump-tail room → first group whose per-GROUP rebuild
  (repacking ONLY that group's members) fits the per-buffer cap + budget → a NEW group under
  `SDF_POOL_TOTAL_BUDGET_BYTES` (4 GiB Σ region bytes) → the existing per-body slot fallback.
  Feasibility is checked BEFORE any mutation, so a rebuild can no longer fail mid-flight.
- **Per-group rebuild:** `rebuild_group(gi)` repacks only group `gi` — other groups' buffers,
  contents, and generations are untouched (never multiply the ~50ms 1 GB rebuild across
  groups). Demand-anchored ×1.5 sizing kept verbatim (S0.3 ratchet gate still green).
- **Draw:** `draw_sdf_pool_multi` sorts the frame-drawn list group-major and rebinds
  vertex/index buffers only on group change (1-3 rebinds/pass typical), one
  `multi_draw_indexed_indirect` per body over its args block as before. The picking pass calls
  the SAME function (raster_pick.rs:291) and the per-slot fallback (`draw_sdf_pool_slot`) also
  resolves the slot's group — **picking stays lockstep with rendering across groups by
  construction** (P0 traced: pick prep clear_frame_visibility/mark_drawn → draw_sdf_pool_multi
  → same group buffers, same args).
- **Dispatch:** both tiled arms + the monolithic pooled arm in `resident.rs` bind the body's
  group buffers (`pool.group(slot.group)`); the churn-headroom clamps now use
  `pool.max_group_bytes` (== the const in production; test knob otherwise).
- **Knobs (pub(crate), the test-forcing pattern like `verts_per_brick_headroom`):**
  `max_group_bytes` (per group buffer, default 1 GiB), `total_budget_bytes` (default 4 GiB).
- Frame Ordering unchanged: group creation/rebuild only inside `ensure_slot`/`trim_slot`
  (poll-phase-only entry points); render phase stays write_buffer-only. `free_slot` stays
  write-only (an emptied group's buffers shrink at its next rebuild, not at free time).

## What this fixes / does NOT fix (honest scope)
- FIXED: **two+ large bodies** ("request exceeds the pool cap — per-body slot fallback"): 2×
  1024 (or 4× 1024 under budget) now each get their own group and BOTH stay POOLED + TILED
  (incremental C2 path intact). A ≤1 GiB-packing 2048 body coexisting with other sparse
  bodies also stays pooled now (previously any neighbor could push it out of the shared pair).
- NOT fixed (out of scope, pre-existing wall): the **77M-vert monolithic 2048** ("tiled
  packing exceeds the dense ceiling") — that fallback compares Σ per-tile caps against the
  DENSE CEILING (`dc_output_caps_dense`, ~33.5M verts at max_bind 2.1 GB), a per-body
  correctness bound; and a 2.4 GB single-body region also exceeds the 1 GiB per-group-buffer
  cap (a body never spans groups). A realistic 2048 whose tiled packing lands under both walls
  (~≤33M verts / ≤1 GiB — plausible at the 28 v/brick tile headroom for mid-complexity
  surfaces, NOT the 77M log case) stays pooled; the log-case fix is the dense-ceiling
  envelope itself (future work), not more buffers.

## Measured (headless gates; scale-free by the ratchet-test argument — placement decisions are
## driven by need/cap ratios, not absolute bytes)
```
[SdfPool] rebuild g0: 1 slots, verts 10.6MB idx 8.0MB (g_gen=1 groups=1)
[SdfPool] group 1 created (verts 5.7MB + idx 4.3MB) — 2 groups, total 28.5MB / budget 4295MB
[SdfPool] rebuild g1: 1 slots, verts 11.3MB idx 8.5MB (g_gen=2 groups=2)
[SdfPoolMG] OK: A in g0, B in g1 (2 groups), Σidx A=89856 B=48240 == refs,
            B grow bumped g1 gen 1→2 while g0 stayed 2 — total pool 42.3MB
```
Two tiled bodies whose combined packing exceeds one group buffer: both resident POOLED+TILED
simultaneously in 2 groups, Σ per-tile index_count == the per-body monolithic reference for
each, and B's growth rebuilt ONLY g1 (g0 generation + geometry bit-preserved). At real scale
the same ratios put each settled 1024 body (~300-500MB verts) in its own ≤1 GiB group: 2× 1024
= 2 groups, 4× 1024 fits the 4 GiB budget.

## Gates (suite = 202 passed / 0 failed / 12 ignored, ONE single-process
## `cargo test -p app --bins -- --test-threads=1` run, no environmental abort this time)
- NEW `sdf_pool_multi_group_two_bodies_tiled_parity_and_isolated_rebuild` — (a) two-group
  placement + pooled/tiled parity vs per-body reference; (b) group growth rebuilds only that
  group (other group's generation unchanged, content bit-exact).
- NEW `sdf_pool_budget_exhausted_falls_back_per_body` — per-group cap + total budget exhausted
  (tail consumed exactly via `group_tail_bytes`) → tiled arm reports false, monolithic arm
  lands `slot.pooled=false`, per-body counts == unpooled reference, pool untouched (no group,
  no generation churn). Documented insight: a bump-tail append allocates NO new memory, so the
  budget correctly never blocks it — the gate exhausts tails first.
- Existing pool gates updated for groups and ALL GREEN: `sdf_pool_parity_multi_body`,
  `sdf_pool_grow_relocate_preserves_counts`, `sdf_pool_rebuild_sizing_tracks_demand_not_history`
  (ratchet gate now reads the body's group buffers), `sdf_pool_settle_trim_shrinks_and_preserves`,
  `sdf_tiled_full_matches_untiled`, `sdf_tiled_pool_multi_body_grow_and_visibility`,
  `sdf_tiled_incremental_empty_window_and_grow_isolation`, `tile_headroom_fits_measured_1024_envelope`.

## Files touched (uncommitted)
- `core_engine/app/src/sdf_gpu_mesh/pool.rs` — the multi-group restructure (SdfPoolGroup,
  group-scoped rebuild/args/seed/free, placement passes, budget, knobs, group_tail_bytes).
- `core_engine/app/src/sdf_gpu_mesh/resident.rs` — tiled/monolithic/incremental arms bind
  `pool.group(slot.group)` buffers; churn clamps use `pool.max_group_bytes`.
- `core_engine/app/src/raster_common.rs` — group-major `draw_sdf_pool_multi`, group-resolved
  `draw_sdf_pool_slot` (both shared by the G-buffer + picking passes — P0 lockstep).
- `core_engine/app/src/sdf_gpu_mesh/tests_dc.rs` — 2 new gates + group-accessor updates to
  `pool_readback`/`tiled_readback`/the ratchet test.

## Owner manual test scenario (Phase 4)
1. Per-Body Cache ON. Create TWO separate 1024³ sparse bodies (e.g. two sphere+Subtract
   bodies side by side), sculpt/drag each until both have settled full-res grids.
2. EXPECT: on the second body's first big mesh, a `[SdfPool] group 1 created (verts …MB + idx
   …MB) — 2 groups, total …MB / budget 4295MB` line, and NO `[SdfPool] … request exceeds the
   pool cap — per-body slot fallback` for either body. Subsequent `[SdfPool] rebuild g0:` /
   `rebuild g1:` lines carry a group id and only ONE group's MB figure each (never both at once).
3. Drag a subtract prim on body B: EXPECT any `[SdfPool] rebuild g1:` spikes to leave body A's
   viewport mesh untouched (no A re-mesh, no flicker), and `[SdfPool] trim body=… (g1)` at
   settle to name the group.
4. P0 selection: click each body in Object Mode — both must select (the pick pass draws per
   group); hide one body — the other keeps rendering.
5. 2048 check: one 2048³ body — if its tiled packing fits (~≤33M verts), it pools (watch for
   the absence of both fallback lines); the known 77M-vert monolithic case still prints
   `tiled packing exceeds the dense ceiling — monolithic fallback` (dense-ceiling wall,
   documented above, unchanged by Phase 4).
6. Regression sweep: single-body sculpting unchanged (all activity stays in g0), undo/redo,
   body delete (slot freed, group survives), Save/Load unchanged (pool is runtime-only).

---

# Session 2026-07-17 — Phase 4b: per-body MULTI-GROUP tiling (segments) — the 2048 dense-ceiling wall falls

## What shipped (uncommitted, feat/sparse-3a)
The Phase-4 honest-limit paragraph named the last wall: a realistic single 2048 body (owner log:
`tiled packing (v=77345016) exceeds the dense ceiling — monolithic fallback`) fell out of the
pooled+tiled path because (1) the ENVELOPE compared the whole packing against the per-slot dense
ceiling (~33.5M verts) and (2) a body's region had to fit ONE ≤1 GiB group buffer. Both are
per-SLOT bounds, not surface bounds. Phase 4b removes them for tiled bodies via **SEGMENTS**:

- **Design chosen: option (a) at segment granularity.** A body's tile table is partitioned into
  contiguous SEGMENTS; each segment is an ordinary pool slot (region + args block + tile table)
  living wholly in ONE group; segments of one body may live in DIFFERENT groups. Each tile still
  lives wholly in one group (its segment's) — the C2 entry/index space stays self-contained PER
  SEGMENT, so every proven mechanism (per-tile counters/args, tail-append grows, tombstones,
  compacted-stage incremental dispatch, watermarks) applies unchanged inside a segment. No WGSL
  change: the tile shader still binds one group's buffers per dispatch; `tmeta.z` entry routing
  is naturally segment-scoped. Option (b) (tighter measured caps) was judged insufficient alone
  at 77M (the brief's own arithmetic) and is superseded by (a); the floor machinery still
  tightens per-tile caps within segments.
- **Pool (`pool.rs`):** slots keyed `(NodeId, segment)` (`SlotKey`); `slot(nid)` = segment 0 —
  every monolithic caller unchanged. `ensure_slot_seg`/`slot_seg`/`segment_count`/
  `free_segments_from`/`make_monolithic` added; all segments share ONE instance index (owned by
  segment 0 — one InstanceData write covers the body; all args carry the same first_instance).
  `free_slot` frees all segments; `retain_bodies` prunes per body. Rebuild packing order is
  (body, segment) — deterministic.
- **Routing (`resident.rs` tiled full path):** single-segment iff the packing fits BOTH the
  dense ceiling AND one group buffer (the pre-4b policies verbatim — churn 2.5×, settle trim,
  byte-identical behavior); otherwise MULTI-SEGMENT: greedy partition at 80% of the per-group
  caps (the ≥20% remainder is the C2 grow/birth slack), tight 1.25× region sizing, NO dense
  ceiling clamp. Deadband hysteresis at the single↔multi boundary (75%) so a gesture straddling
  it can't repartition every landing. The dispatch encode is ONE shared helper
  (`encode_tiled_segment_full`) used by both arms; one combined counters staging across
  segments (floors are keyed by tile coord — the overflow poll is segment-agnostic).
- **ENVELOPE is now the BUDGET:** the multi-segment packing checks Σ segment regions (1.25×,
  56 B/vert) against `total_budget_bytes` — raised **4 GiB → 6 GiB** (the owner-log 77.3M caps
  = 4.33 GB regions sat just over 4 GiB; 6 GiB hosts it + a 1024 neighbor; 2048 is opt-in on
  16-24 GB GPUs). Over budget ⇒ LOUD monolithic fallback with hysteresis, and if the real
  surface exceeds the dense ceiling the new `slot.envelope_over_budget` latch arms →
  `app_poll_gpu` raises a TOAST ("Surface too complex for the mesh pool budget… display is
  TRUNCATED") — the self-heal precedent, never silent truncation. The monolithic
  counter-overflow "caps at dense ceiling" refusal also toasts now.
- **C2 incremental (`mesh_tiled_incremental`):** plans across all segments against the shared
  atlas stats (occupancy contract unchanged, violations still loud); grows tail-append in the
  tile's OWN segment; births place into any segment with args+region slack (last-first — slack
  pools at the tail); dispatches only segments with dirty tiles (compacted stage per segment,
  shared helper `encode_tiled_segment_incremental`, one combined counters staging).
  **`SdfTiledStamp` gained `segments`** — the tile→segment mapping is part of the table
  identity; a segmentation change forces full (conservative by construction).
- **Draw + PICK (`raster_common.rs`):** `draw_sdf_pool_multi` flattens frame_drawn bodies into
  (group, body, segment) sorted group-major — one multi-draw per segment; the picking pass
  calls the SAME function and `draw_sdf_pool_slot` loops segments → P0 lockstep by construction.
- **Fallback hygiene fix (latent class):** `make_monolithic` — a body falling from tiled to
  monolithic now sheds its stale tile table + higher segments and reseeds its single entry.
  Previously the monolithic arm left the pool tiles populated, so `draw_entries()` still covered
  stale tile args (reachable pre-4b via the envelope fallback).
- Frame Ordering unchanged (all segment ensure/rebuild in poll-phase entry points); generation
  counters per group unchanged (a segment relocation bumps its group's gen).

## Measured
- Owner-log case (77,345,016 vert caps) — `sdf_pool_2048_owner_log_case_stays_pooled_multi_group`
  (#[ignore]d, real allocations on the dev GPU): partitions into **3 segments across 3 real
  ~1 GiB groups**, 4.33 GB regions under the 6 GiB budget, allocation path completes in well
  under a second. The two fallback log lines can no longer fire for this packing.
- Incremental edit cost at 2048: the dirty-tile set of a drag window is unchanged by
  segmentation (same tiles, same per-tile dispatch, same compacted stage) — the measured C2
  INC cost class (~1-3 ms GPU for drag-window tile counts, the `GPU tiled INC` prints) now
  applies to the 77M-vert body instead of the ~15-18 ms full monolithic re-mesh + ~50 ms pool
  rebuild it was paying per edit. Owner verification on the real 2048 scene is the remaining
  datum (manual test below).

## Gates (all green)
- `cargo build -p app` after every change.
- `cargo test -p app --bins -- --test-threads=1` = **205 passed / 0 failed / 13 ignored** —
  ONE single-process run, no environmental abort this time (202 baseline + 3 new in-suite;
  the 2048 allocation gate is the new ignored).
- NEW `sdf_tiled_multi_segment_body_parity_across_groups` (gate a): packing > one group buffer
  ⇒ ≥2 segments in ≥2 DISTINCT groups, Σ per-tile index_count == monolithic reference,
  self-contained per-tile index spaces, quantized vertex-set parity both directions, stamp
  carries the segmentation.
- NEW `sdf_tiled_multi_segment_incremental_dirty_only` (gate b): empty window = 0 dispatches +
  every segment BIT-exact; a floor-forced grow of one tile in segment 1 re-dispatches exactly
  (1, n_total), tail-appends in its own segment, leaves segment 0 bit-identical, Σidx unchanged.
- NEW `sdf_tiled_over_dense_ceiling_meshes_fully_or_degrades_loudly` (gate c): a packing over
  the (shrunk) dense ceiling STAYS TILED and meshes COMPLETE (Σidx == reference) — the pre-4b
  refusal case now meshes fully; over-budget refuses LOUDLY (`envelope_over_budget` latch =
  toast source) and the monolithic fallback retires all stale segments (draws exactly 1 entry).
- NEW #[ignore]d `sdf_pool_2048_owner_log_case_stays_pooled_multi_group` (above).
- All existing tiled/pool/parity/envelope/trim gates green: `sdf_pool_parity_multi_body`,
  `sdf_pool_grow_relocate_preserves_counts`, `sdf_pool_rebuild_sizing_tracks_demand_not_history`,
  `sdf_pool_settle_trim_shrinks_and_preserves`,
  `sdf_pool_multi_group_two_bodies_tiled_parity_and_isolated_rebuild`,
  `sdf_pool_budget_exhausted_falls_back_per_body`, `sdf_tiled_full_matches_untiled`,
  `sdf_tiled_pool_multi_body_grow_and_visibility`,
  `sdf_tiled_incremental_empty_window_and_grow_isolation`, `sdf_tiled_incremental_matches_full`,
  `tile_headroom_fits_measured_1024_envelope`.

## Honest scope / open
- Multi-segment bodies do NOT settle-trim their regions yet (grow-only per segment; a
  repartition on the next full mesh re-sizes tightly at 1.25×, but a shrinking surface keeps
  its old segment caps until it drops back under the single-segment boundary, whose trim is
  unchanged). Bounded by the tight 1.25× multi-seg sizing; follow-up if real sessions pin VRAM.
- The budget is pool-global: two 77M bodies (8.7 GB) exceed 6 GiB — the second degrades loudly.
  That is the designed behavior, not a gap; raise the budget knob when >24 GB GPUs are the target.

## Files touched (uncommitted)
- `core_engine/app/src/sdf_gpu_mesh/pool.rs` — SlotKey segments, shared instance index,
  ensure_slot_seg / free_segments_from / make_monolithic, budget 6 GiB.
- `core_engine/app/src/sdf_gpu_mesh/resident.rs` — segmented tiled full path + budget envelope
  + `envelope_over_budget`; multi-segment C2 incremental; `SdfTiledStamp.segments`; the two
  shared encode helpers; monolithic arm calls `make_monolithic`.
- `core_engine/app/src/raster_common.rs` — segment-aware `draw_sdf_pool_multi` /
  `draw_sdf_pool_slot` (G-buffer + picking share them — P0).
- `core_engine/app/src/app_poll_gpu.rs` — truncation toasts (tiled over-budget latch +
  monolithic ceiling refusal).
- `core_engine/app/src/sdf_gpu_mesh/tests_dc.rs` — `tiled_readback_seg` + the 4 new gates.

## Owner manual test (Phase 4b)
1. Per-Body Cache ON, the realistic 2048 body from log 8 (sphere + subtract, Grid 2048³).
2. On the first full mesh EXPECT: NO `tiled packing … exceeds the dense ceiling — monolithic
   fallback` line and NO `request exceeds the pool cap` line. Instead expect 2-3
   `[SdfPool] group N created (verts …MB + idx …MB) — … / budget 6442MB` lines and a
   `[DcResidentSparse] tiled tiles=… segs=2..3 …` print (the `segs=` field is new).
3. Drag a prim: EXPECT `[DcResidentSparse] GPU tiled INC vert=…ms quads=…ms tiles=k/N` lines
   with k = the drag window's tiles (few dozen) and ~1-3 ms GPU — NOT 15-18 ms monolithic
   re-meshes, and NO ~50 ms `[SdfPool] rebuild` spikes per edit.
4. P0 selection: click the 2048 body (its tiles span groups) — it must select; click another
   body — correct body selected (pick draws the same segment list).
5. Hysteresis: sculpt so the surface shrinks/grows across the old ceiling — no
   tiled/monolithic flip-flops (the `segs=` count may change on FULL re-meshes only).
6. Degradation check (optional): drop the budget knob or use a much denser surface — EXPECT the
   toast "Surface too complex for the mesh pool budget at 2048³ — display is TRUNCATED…" plus
   the `[SdfPool] … exceeds the …MB pool budget — monolithic fallback` log; never a silent
   partial mesh.
7. Regression sweep: 512/1024 bodies behave exactly as before (single segment, same logs),
   two-body scenes still split groups (Phase-4 manual test), undo/redo, Tab, Save/Load.

---

# Session 2026-07-17 — Phase 4c: the ~330ms full-rebuild hitch (idx demand-tighten + buffer recycle)

## The measured problem (owner log 2026-07-17, a 2048³ multi-segment body)
```
[SdfPool] rebuild g0: 1 slots, verts 1073.7MB idx 1073.7MB (g_gen=4 groups=3)
[DcResidentSparse] tiled tiles=11590 segs=3 cpu_total=333.11ms | table=0.61 pool=330.74 infos=0.64
```
The `pool=330.74ms` frame-thread phase = the `_t_pool` block in `mesh_into_slot_sparse_tiled`
(`ensure_slot_seg` × segments). It fires on a FULL tiled rebuild (a bounds-change edit that GROWS
the body ⇒ `bounds_ok=false`).

## MEASURE-FIRST (dev GPU, headless `sdf_pool_2048_owner_log_case…` + a throwaway split probe)
Split the cost — `create_buffer`(CPU) vs first-use zero-init(GPU) vs the region-copy submit:
| stage (this GPU, 6 × ~1 GiB fresh buffers) | measured |
|---|---|
| `create_buffer` per 1 GiB (CPU, warm) | ~2 ms |
| poll after 6 creates, NO use | 0.07 ms → creation does NOT eagerly commit/zero |
| first USE (bind/copy) → wgpu zero-init, 6 GiB | **1.7–3.3 s GPU** (async; surfaced by poll) |
| RE-create 6×1 GiB after free (same process) | ~13 ms → driver reuses pages fine |
| real `ensure_slot_seg` 3-seg loop, CPU-only (no poll) | **~14 ms** |

**The mechanism, stated honestly:** the CPU `_t_pool` is only ~14 ms on an EMPTY dev GPU — the
owner's **330 ms is the SAME six ~1 GiB `create_buffer` calls ballooned by production VRAM
pressure** (DX12/Vulkan committed-resource creation stalls on eviction when VRAM is already full
of atlas+textures+other bodies). *We cannot reproduce the literal 330 ms on an empty test GPU* —
but the driver-independent quantities (bytes allocated, buffer count) are what it is proportional
to, plus a **multi-GB async zero-init that stalls rendering regardless of the CPU number**.

**Two premises in the brief turned out FALSE (proven, not assumed):**
1. *"groups are created at max regardless of need"* — the pool ALREADY demand-sizes (`rebuild_sizes`
   is `need×1.5` capped at `safe_max`; the two-body Phase-4 log shows 10.6 MB / 5.7 MB groups). The
   owner's 2048 groups are ~1 GiB each because a dense 2048 surface GENUINELY needs 77.3 M vert
   caps ≈ 5.4 GB. The only residual waste is IDX (below).
2. *"a retired-buffer recycle avoids the re-allocation"* — for the owner's SINGLE-body event it does
   NOT: rebuilds are monotonic GROWTH (need > any retired buffer), and the only large retirements
   come from `trim`, which must drain to release VRAM. `recycle_hits=0` for a lone grow gesture
   (measured). Recycle DOES fire for MULTI-body / scene-load bursts (`recycle_hits=8` when a second
   similar body reuses the first's retired growth-ladder buffers) — the case Phase-4's multi-buffer
   pool exists for — so it ships, but honestly scoped.

## What shipped (uncommitted, feat/sparse-3a) — two composing levers
### (a) IDX region demand-tighten — `clamp_idx_headroom` (`pool.rs`)
Every pool slot has `idx_cap == 6×vert_cap` (DC = 6 idx/vert — every caller: tile protos `vcap*6`,
`grow_slot`, sparse monolithic), so idx BYTES = 6×4/32 = **0.75× vert bytes ALWAYS**. When a dense
body's verts pin `safe_max`, the raw `need_i×1.5` idx headroom landed on that same ceiling too —
allocating ~1 GiB of idx where ~0.75 GiB is the true bound (**269 MB/segment of pure fresh-alloc +
zero-init waste**). `clamp_idx_headroom(nv, ni, need_i) = ni.min(nv*3/4).max(need_i)` grants idx the
same proportional headroom verts got and **never dips below the demand floor** (`.max(need_i)` ⇒
unconditionally safe, no truncation even if the 6:1 assumption were ever violated). No effect below
the ceiling — a 1024 single-segment body is byte-identical (there `nv*3/4 ≥ need_i*3/2`); only the
ceiling-pinned dense 2048 case tightens. Applied at both sizing sites (`rebuild_sizes` + Pass-3
new-group).

### (b) Retired-buffer RECYCLE (`RetiredBuffers`, `pool.rs`)
A full rebuild/new-group replaces (verts, idx) buffers; instead of dropping the old pair the pool
keeps them in a bounded free-list (`SDF_POOL_RECYCLE_MAX_BYTES` = 2 GiB) and a later
rebuild/birth REUSES one of size ∈ `[need, need*2]` (best-fit) — skipping BOTH `create_buffer`
(the pressure stall) AND wgpu zero-init (the reused buffer stays "initialized"; its never-drawn
GAPS keep stale bytes, exactly as a fresh buffer's uninitialized holes are never drawn — the pool's
existing hole-safety invariant). Reuse still swaps the `wgpu::Buffer` identity ⇒ group generation
bumps ⇒ bind groups rebuild, so it is transparent to the generation-counter machinery. Frame
Ordering unchanged (acquisition stays in the poll-phase `ensure_slot`/`rebuild_group`/`trim_slot`).
**Drained at settle** (`drain_recycle` on a quiet `may_trim` mesh + inside `trim_slot`) so a
gesture's churn reserve never lingers on a now-stable body. The `[need, need*2]` upper bound keeps a
`trim` (need ≪ retired) from pinning a big buffer for a small region.

### Instrumentation (permanent, Slice-D convention)
`[SdfPool] rebuild g{gi}: … | acquire={:.2}ms(recycled N/2) copy_submit={:.2}ms held={}` and
`group … created … | acquire=…(recycled N/2)` — splits the frame-thread rebuild into buffer
acquisition (create OR recycle) vs the region-copy submit, and names recycle hits.

## Measured (before → after, `sdf_pool_2048_owner_log_case`, dev GPU)
```
BEFORE: group N: verts 1073.7MB idx 1073.7MB   → total 6433.3MB
AFTER : group N: verts 1073.7MB idx  805.3MB   → total 5637.1MB   (−796MB = −12.4%)
        acquire=3.4–4.4ms(recycled 0/2) copy_submit=0.3ms       (single-body ⇒ recycle 0, honest)
```
- **idx tightening: −12.4% bytes allocated + proportionally less async zero-init**, on the exact
  owner scenario. This is the driver-independent, single-body win.
- **recycle: 0 on this single-body case** (monotonic growth — nothing to reuse), **but a second
  similar body reuses 8/8 of the first's retired buffers** (`sdf_pool_recycle_reuses_retired_buffer`)
  — a multi-body/scene-load scene skips those creates + zero-inits entirely.
- **HONEST irreducible remainder:** the first-mesh / growth allocation of a dense 2048 body is
  ~4.5–5.6 GB of GENUINE region need (the pool already demand-sizes; verts are real caps at 28
  v/brick). It cannot be recycled (nothing retired yet) nor demand-shrunk below the caps without
  changing the tile headroom (global, out of scope). Under production VRAM pressure that create is
  the bulk of the 330 ms; the −12.4% + the render-stall zero-init cut are the safe wins, and recycle
  covers the repeated/multi-body variants. Lever-3 (make the rebuild rarer) does not apply: a
  same-size bounds change is ALREADY a no-op in `ensure_slot_seg` (the rebuild fires only on genuine
  growth), so there is no spurious rebuild to eliminate.

## Gates (all green)
- `cargo build -p app` after each change.
- `cargo test -p app --bins -- --test-threads=1` = **207 passed / 0 failed / 13 ignored** (one
  single-process run, no environmental abort; 205 baseline + 2 new in-suite).
- NEW `sdf_pool_demand_sizes_idx_below_verts_for_dense_multi_segment` — a dense 3-segment body's
  every group has idx region ≤ 0.75× verts AND strictly < verts (demand-sized, not flat-max).
- NEW `sdf_pool_recycle_reuses_retired_buffer` — lone grow ⇒ `recycle_hits==0` + free-list non-empty;
  a second similar body ⇒ `recycle_hits>0` (reuse); free-list stays bounded; `drain_recycle` empties.
- All Phase-4 + 4b pool/segment/parity/trim/budget/envelope gates green WITH recycle active
  (`sdf_pool_multi_group_two_bodies_…`, `sdf_pool_budget_exhausted_…`, `sdf_pool_parity_multi_body`,
  `sdf_pool_grow_relocate_preserves_counts`, `sdf_pool_rebuild_sizing_tracks_demand_not_history`,
  `sdf_pool_settle_trim_shrinks_and_preserves`, `sdf_tiled_full_matches_untiled`,
  `sdf_tiled_multi_segment_body_parity_across_groups`, `sdf_tiled_multi_segment_incremental_dirty_only`,
  `sdf_tiled_over_dense_ceiling_meshes_fully_or_degrades_loudly`, `tile_headroom_fits_measured_1024_envelope`,
  `sdf_pool_2048_owner_log_case_stays_pooled_multi_group` #[ignore]d).

## Files touched (uncommitted)
- `core_engine/app/src/sdf_gpu_mesh/pool.rs` — `clamp_idx_headroom` + both sizing sites;
  `RetiredBuffers` + `take_region_buffers`/`retire`/`drain_recycle`; rebuild/new-group recycle +
  acquire/submit instrumentation; `group_region_bytes`/`recycle_hits`/`retired_len` accessors.
- `core_engine/app/src/sdf_gpu_mesh/resident.rs` — `drain_recycle()` on a quiet (`may_trim`) tiled mesh.
- `core_engine/app/src/sdf_gpu_mesh/tests_dc.rs` — the 2 new gates.

## Owner manual test (the 2048 bounds-change edit)
1. Per-Body Cache ON, the realistic 2048³ body (sphere + subtract, Grid 2048³). Do the bounds-change
   growth edit from log 7.
2. On the growth full-mesh EXPECT `[SdfPool] rebuild g0 … verts 1073.7MB idx 805.3MB …` and
   `group N created (verts 1073.7MB + idx 805.3MB)` — **idx now ~0.75× verts** (was == verts). Each
   line carries `| acquire=…ms(recycled N/2) copy_submit=…ms`. Total regions ~5.6 GB, not ~6.4 GB.
   The `pool=` phase should drop by the idx share (~12%) plus whatever the smaller footprint relieves
   of VRAM pressure; the async zero-init stall shrinks proportionally.
3. A SECOND large body (or reopening a scene with several) — EXPECT `recycled 1/2` or `2/2` on its
   group creations (it reuses the first body's retired buffers) and a visibly cheaper second-body
   mesh. Single-body edits stay `recycled 0/2` (honest — nothing to reuse).
4. Settle (stop editing): the recycle reserve drains (no lingering GiB-class retired buffers); a
   later `[SdfPool] trim …` still shrinks + frees as before.
5. Regression: 512/1024 bodies byte-identical sizing (idx tightening is a no-op below the ceiling),
   undo/redo, Tab, Save/Load, click-to-select on multi-group bodies unchanged.

---

# GATE 0 — DC-consolidation instrumented baseline (2026-07-17, uncommitted)

Executed **GATE 0 of `DC_CONSOLIDATION_GATED_PLAN.md`** (instrumentation + safety-net tests ONLY —
changes no production mesh/bake/pool/DC path except the one sanctioned determinism fix). All baselines
captured against **HEAD c210425**.

## THE GO / NO-GO DETERMINISM GATE — **GO**
- The golden mesh hash is captured TWICE from two independent `ChunkedMesh` instances (fresh HashMaps
  = the exact condition that exposes non-deterministic chunk order). **All 5 reference bodies are
  byte-identical across the two runs** (and across separate process invocations, incl. the full suite).
- **Root cause found + fixed (the plan's risk #1):** `core_math::chunked_dc::ChunkedMesh::merge_to_mesh`
  concatenated chunks in `self.chunks.values()` order — a `HashMap`, randomized per instance. Same
  vertex/quad SET, shuffled byte order + shifted `base` offsets ⇒ a non-deterministic golden. **Fix:**
  emit chunks in sorted `(cx,cy,cz)` order (minimal, geometry-preserving — same triangles, only order).
  Also a latent reproducibility bug for the CPU-fallback DC (`app_poll_gpu.rs`) and SDF→mesh
  export/convert (`convert.rs`), which are the production callers of `to_mesh()`.
- **Gate proven non-vacuous:** temporarily reverting the sort → the test FAILS with
  `RefBoxBevelSub run_a=0x4f73f3458d80d05a run_b=0xd30de6a505804e90` (5/5 bodies non-deterministic).
  Restoring the sort → GO, matching the pinned goldens. `cargo test -p core_math chunked` stays green
  (`test_to_mesh`, `test_extract_all_and_merge`, `test_chunked_matches_monolithic` — geometry preserved).

## Committed golden hashes (CPU `chunked_dc` oracle, uniform golden_res 96³, FNV-1a over vert-bits+quad-idx)
Canonical source = the CPU oracle (`BodyField::from_graph` → `bake_body_grid_field` →
`ChunkedMesh::to_mesh`), NOT the GPU pool: headless/CI-safe (no adapter → always proves determinism),
pure/deterministic, and already the GPU-DC parity reference (`tests_dc::cpu_mesh`). Trade-off: it does
not exercise the sparse/tiled GPU pool at full regime res (owner-run, Tier B) — the existing `sdf_dc_*`
GPU↔CPU parity tests bridge that, and `dc_gate0_gpu_cpu_golden_bridge` (`#[ignore]`, uses the adapter
assertion) checks the count ratio for body 0.

| # | Body | Regime | golden_res | verts | tris | golden hash |
|---|---|---|---|---|---|---|
| 1 | RefBoxBevelSub    | dense 128³   | 96³ | 18251 | 33856 | `0xdfef82593cbaa428` |
| 2 | RefCsg3Prim       | dense 256³   | 96³ | 26804 | 49384 | `0x23c7b5455afdbfaa` |
| 3 | RefOrganicSparse  | sparse 512³  | 96³ | 12172 | 21768 | `0x6a06b406c632da9c` |
| 4 | RefDetailedSparse | sparse 1024³ | 96³ | 19796 | 36768 | `0x1033d5af4136249c` |
| 5 | RefNestedSubBody  | dense 256³ (recursive child) | 96³ | 18098 | 33820 | `0x11b48acb7f451bde` |

(Body 5's golden goes through `BodyField::from_graph`'s recursive fold — the nested-child case the
review feared was broken; it is deterministic.)

## Deliverables
1. **Golden hash per body** — `dc_baseline::golden_mesh_hash` + `body_golden`, pinned in `GOLDEN_HASHES`.
2. **Reference scene** — `dc_baseline::build_ref_scene` (5 regimes + 1 material), serialized to
   `docs/planning/sdf/bench/dc_consolidation_ref.fl4` (17 KB, via the real `project::save_binary`).
3. **`.fl4` round-trip test** — `dc_gate0_ref_scene_roundtrip`: write→read→assert bodies+prims+materials
   identical (per-prim bincode fingerprint, HashMap-order-immune). First real full-scene round-trip
   (`test_spine.rs` was `#[ignore]`; only the bincode sentinel guard ran before).
4. **GPU-adapter assertion** — `dc_baseline::require_adapter` (PANICS without a device), wired into
   `dc_gate0_gpu_cpu_golden_bridge`. Audited the ~40 SILENT early-return parity tests (tests_dc.rs,
   tests_bake.rs, tests_sparse_bake.rs, tests_gpu_classify.rs, voxel_brush_gpu_parity.rs) — listed in
   the helper's doc; NOT retrofitted (would break headless CI).
5. **Pick regression test** — `dc_gate0_pick_selection_roundtrip`: the headless slice of the GPU pick
   (the part gates 3-4 would regress): the canonical `body_has_renderable_content` gather predicate is
   true for every body (P0 selectable), each anchor projects on-screen via `world_to_screen`, and the
   `UnifiedSelection` single-select (mirroring `select_single_unified`) resolves to one `Node(nid)`.
   Multi-segment lockstep (all `SlotKey=(nid,seg)` map to one nid) is asserted at the identity level;
   the GPU colour-ID readback stays owner-run.
6. **Dead/stderr-only metrics populated** (all additive; no behaviour change; app builds clean, no new
   warnings): `vol_full_rebakes`/`vol_partial_rebakes` (+1 at `app_poll_bake.rs` `is_partial`),
   `vol_dc_ms` (per-body DC populate ms accumulated at `sdf_populate_resident`), `frame_percentile()`
   (p50/p95 over the frame ring — the "p95 on the existing buffer"), `SdfPoolSlot::region_bytes()` +
   `SdfMeshPool::body_vram_bytes()` (bytes/body) + `SdfMeshPool::hole_ratio()` (fragmentation), latched
   into `sdf_pool_body_bytes`/`sdf_pool_hole_ratio`. All shown on one new Perf-HUD (F3) line so the
   owner can read them live. NOTE: the counters measure the BAKE full-vs-partial axis; the mesh-remesh
   axis (C2 tiled-incremental vs full re-mesh) stays in the `[DcResidentSparse]` stderr logs (threading
   it up a return value would touch the hot mesh path — out of Gate-0 scope).
7. **Baseline JSON** — `docs/planning/sdf/bench/dc_consolidation_baseline.json` (via
   `dc_gate0_write_baseline_json`): golden hashes, verts/tris, grid bounds, theoretical mesh VRAM,
   determinism=GO; runtime numbers (frame p50/p95 drag, bake full/partial ms, per-body DC ms, live pool
   VRAM, %incremental, hole ratio) are clearly-marked `_TODO_owner_run` (the exe is owner-locked).

## Gates (green)
- `cargo build -p core_math` + `cargo test -p core_math chunked` (25 passed) — determinism fix.
- `cargo build -p app` — clean (no new warnings; the metric fields are read in the HUD).
- `cargo test -p app --bins -- --test-threads=1` = **210 passed / 0 failed / 16 ignored** (207 baseline
  + 3 new routine; 13 baseline + 3 new ignored). One single-process run, no environmental abort.
- Gate-0 routine tests: `dc_gate0_golden_determinism`, `dc_gate0_ref_scene_roundtrip`,
  `dc_gate0_pick_selection_roundtrip` all green. Ignored (opt-in / artifact-writer):
  `dc_gate0_write_ref_scene_fl4`, `dc_gate0_write_baseline_json`, `dc_gate0_gpu_cpu_golden_bridge`.

## Files touched (uncommitted — NOT committed; integrator merges)
- `core_engine/core_math/src/chunked_dc.rs` — `merge_to_mesh` chunk-key sort (determinism fix).
- `core_engine/app/src/dc_baseline.rs` — NEW `#[cfg(test)]` module (all 7 deliverables' test/bench code).
- `core_engine/app/src/main.rs` — `#[cfg(test)] mod dc_baseline;`.
- `core_engine/app/src/types/types_ui.rs` — 3 perf fields + defaults + `vol_dc_ms` reset + `frame_percentile`.
- `core_engine/app/src/sdf_gpu_mesh/pool.rs` — `SdfPoolSlot::region_bytes`, `SdfMeshPool::body_vram_bytes`/`hole_ratio`.
- `core_engine/app/src/app_poll_bake.rs` — full/partial bake tally → `vol_full_rebakes`/`vol_partial_rebakes`.
- `core_engine/app/src/app_poll_gpu.rs` — `vol_dc_ms` + pool VRAM/hole-ratio latch in `sdf_populate_resident`.
- `core_engine/app/src/viewport_overlays.rs` — one Perf-HUD line for the Gate-0 metrics.
- `docs/planning/sdf/bench/dc_consolidation_ref.fl4`, `.../dc_consolidation_baseline.json` — NEW artifacts.
- `docs/planning/sdf/DC_CONSOLIDATION_GATED_PLAN.md` — Gate-0 baseline table filled with what was captured headlessly.

## Owner-run follow-ups (need the locked exe)
1. Open `dc_consolidation_ref.fl4`, enable Perf HUD (F3) + CPU Frame Profiler + Profiler overlay.
2. Run a scripted deterministic prim drag; read the new HUD line: `DC …ms reB f…/p… …%inc pool …MB h…% p95 …ms`.
3. Fill the `_TODO_owner_run` fields in `dc_consolidation_baseline.json` (frame p50/p95, bake full/partial ms,
   per-body DC ms, live pool VRAM, %incremental, hole ratio) at the real 128/256/512/1024 regime resolutions.

## Flag (report only — NOT fixed here, separate doc fix)
- **CLAUDE.md `mesh uploads ≤2` is STALE** (the plan noted it): the code uses a 5 ms budget + hard cap
  4096 (`render/src/mesh_cache.rs` — the plan cited `mesh_cache.rs:363`). The "DC results ≤2" half is
  still accurate (`MAX_DC_RESULTS_PER_FRAME = 2`). Correct in CLAUDE.md independently of this plan.

# GATE 1 — Unified `BodyContentId` (2026-07-17, uncommitted)

Executed **GATE 1 of `DC_CONSOLIDATION_GATED_PLAN.md`** (behavior-constant cleanliness unification —
per the plan's correction #1, NOT a bug fix; the recursive path already guarded display). Base =
HEAD `7437432` (Gate 0). The two divergent body-content hashes are now ONE.

## What changed (2 hash functions → 1)
- **NEW `BodyContentId(pub u64)`** newtype + `body_content_id(sg, nid, lab_operand_gen)` in
  `app/src/bake_utils.rs` — the ONE graph-level entry (rename of the recursive
  `compute_body_source_hash`; the fold itself stays `hash_body_source`, which `trigger_bake` /
  `advance_cascade` already call — one implementation, structurally cannot drift).
- **DELETED `render::mesh_cache::hash_primitives`** (own-prims-only, non-recursive) + its 3 private
  helpers (`hash_f32`/`hash_vec3`/`hash_discriminant`, orphaned). mesh_cache now stores the u64
  purely as an opaque `source_hash` stamp (tombstone comment at the old site).
- **Semantics direction:** mesh_cache consumers moved own-only → RECURSIVE (the strictly-safe
  direction: a nested-child edit now changes the stamp → re-mesh/re-stamp, never stale). No
  consumer moved recursive → own-only. PT BLAS keying is `content_stamp` (monotonic, bumped on
  every mesh insert) — unaffected; PT sparse-SDF snapshots key on `entry.source_hash`, which was
  already the recursive bake hash. A nested-child edit correctly rebuilds the BLAS (new mesh insert
  → new content_stamp), same as before.

## Callers audited + migrated (grep of both old names = 0 production refs remain)
1. `app_poll_gpu.rs` `dispatch_grid_dc_async` — was `hash_primitives(visible own prims)` → now
   `body_content_id` (guard flow unchanged; degenerate no-content case falls back to stamp 0).
2. `raster_mesh_prep.rs` `ensure_body_meshes` — preview-mesh stamp → `body_content_id` (still
   body-local ⇒ rigid body moves don't re-mesh).
3. `raster_mesh_prep.rs` `upgrade_mesh_lod` — LOD-upgrade stamp → `body_content_id`.
4. `body_volume_cache/undo.rs:103` `invalidate_for_undo` — renamed call, compares `.0` vs
   `entry.source_hash` (identical value to before — same fold).
5. Tests: `bake_dispatch.rs` order/regime tests (4 sites) + `tests_sparse_bake.rs` (2 sites) renamed.
6. Unchanged by design (already the unified fold): `trigger_bake` (bake/atlas `entry.source_hash`),
   `advance_cascade` — both call `hash_body_source` on already-collected recursive prims.

Perf note: no new per-frame recursive walks — `dispatch_grid_dc_async` runs at bake-accept,
`ensure_body_meshes` hashes only DIRTY bodies, `upgrade_mesh_lod` hashes only idle bodies below
High LOD (pre-existing hashing site, same cadence). Hash cost is µs vs ms bakes ⇒ bake full ms
within the +5% gate by construction (owner-run confirms at regime res).

## GO gates
- **Hash functions 2 → 1** — done (`hash_primitives` deleted; `body_content_id` is the survivor).
- **Golden mesh hash 5/5 IDENTICAL bit-for-bit** — `dc_gate0_golden_determinism` green against the
  pinned Gate-0 `GOLDEN_HASHES` (0xdfef8259… / 0x23c7b545… / 0x6a06b406… / 0x1033d5af… /
  0x11b48acb…). Expected: content IDs are stamps, never geometry inputs.
- **Nested-child edit → rebake unchanged** — NEW test `nested_child_edit_bumps_content_id`
  (bake_dispatch.rs `order_hash_and_regime_tests`): child-prim edit bumps the parent's ID; an
  untouched sibling body's ID is stable. Green.
- `cargo build -p app` clean (no new warnings) · `cargo test -p app --bins -- --test-threads=1` =
  **211 passed / 0 failed / 16 ignored** (210 + the new test), ONE run, no environmental abort ·
  `cargo test -p render` = 20 passed (mesh_cache touched) · Gate-0 pick + round-trip green.

## Files touched (uncommitted — integrator merges)
`app/src/bake_utils.rs` · `app/src/app_poll_gpu.rs` · `app/src/raster_mesh_prep.rs` ·
`app/src/body_volume_cache/{undo.rs, bake_dispatch.rs, mod.rs}` ·
`app/src/sdf_gpu_mesh/tests_sparse_bake.rs` · `render/src/mesh_cache.rs`

## Owner manual test
Open `dc_consolidation_ref.fl4`. Body 5 (RefNestedSubBody): enter the NESTED child and drag its
prim — the parent surface must update live (rebake fires; no stale display). Then move the WHOLE
parent body (rigid transform) — NO re-mesh/rebake churn (HUD `reB` counters stay flat). Then edit
body 1 — body 5's mesh must not re-mesh (sibling stability). Ctrl+Z each step — surfaces restore.

## Note for the owner (plan-doc sync)
The worktree has no copy of `DC_CONSOLIDATION_GATED_PLAN.md` (main-dir doc is owner WIP, not
edited from here). To sync: mark **GATE 1 = EXECUTED, GO** (2 → 1 functions; golden 5/5 identical;
`nested_child_edit_bumps_content_id` green; bake-ms gate satisfied structurally, owner-run confirms).

---

# GATE 2 — session log (2026-07-17): ONE `DirtyMeshRegion` from bake-accept — EXECUTED

## Producer/consumer map BEFORE (the three channels, grep-mapped before refactoring)
**Channel 1 — `last_bake_dirty_range`** (per-body voxel range, OVERWRITE semantics, dispatch-time)
- Writers: `bake_dispatch.rs:1403` trigger_bake (AT DISPATCH, every flavor, overwritten per
  dispatch); `bake_dispatch.rs:872` RAM-cache restore (full range); `duplication.rs:116` (full).
- Readers: `app_poll_gpu.rs:895` `dispatch_grid_dc_async` (clone-read → `is_partial_bake` →
  chunked incremental CPU DC); `resident_volume.rs:330` partial-vs-full 3D-texture upload
  decision; consumed (removed) at `resident_volume.rs:275/379` after the upload lands.
- Clears: `mod.rs` invalidate_all / remove_body, `undo.rs:82/165`.

**Channel 2 — `sparse_mesh_dirty_window`** (C2 accumulator, UNION semantics, accept-time)
- Writer: `app_poll_bake_poll.rs:142` sparse PARTIAL acceptance (`note_sparse_mesh_dirty_window`);
  cleared by sparse FULL acceptance (`:145`).
- Reader: `sdf_populate_resident` (`app_poll_gpu.rs:412`) `take_…` → `SdfTileDirty::Voxels` →
  `mesh_into_slot_sparse_tiled` (the C2 crown jewel).

**Channel 3 — Partial window plumbing**: `req.bake_min/max` → `pending.bake_window`
(`app_poll_bake.rs:918`, sole construction) → fed to channel 2 at accept. The
`SparseBakeRequest::Partial` payload (dims / atlas_generation / index_update) is staleness
GUARDS + escalation routing, not a dirty-region source.

## What was unified vs deliberately kept internal
- **Unified → `BodyVolumeCache::dirty_mesh_region: HashMap<NodeId, DirtyMeshRegion>`** (type
  alias `([u32;3],[u32;3])`, mod.rs). Channels 1+2 fields are DELETED. Semantics = channel 2's
  proven C2 contract, now global: UNION on accepted partial, CLEAR on accepted full, entry
  ABSENT = "no proven delta" = full re-mesh / full upload — which was already every consumer's
  conservative default (channel 1's full-range-present entries produced the exact same decisions
  as absence at both dense consumers — verified line-by-line).
- **Single producer, accept-time only** (channel 1 was dispatch-time — a superseded/dropped bake
  could leave a stale claim; now structurally impossible): `note_bake_accepted_window` (union) /
  `note_full_bake_accepted` (clear) / `note_bake_accepted` (router: full-covering window →
  clear) in bake_readback.rs. Producer call sites = the acceptance points: dense GPU readback
  (app_poll_bake_poll.rs step 6, placed BEFORE consumer 6a to preserve the upload's
  partial-window read), no-copy path-A/lazy (`note_resident_bake_complete` branch), sparse
  partial/full landing (same spot as the old C2 calls), CPU-worker `poll()` (window now travels
  in `BodyBakeResult.bake_window`), worker-GPU (app_poll_misc.rs; window added to
  `GpuJobResultKind::BodyVolume` — the executor already knew it), RAM-restore + duplication
  (full grid installed → clear).
- **Consumer contract:** tiled sparse mesher TAKES (`take_dirty_mesh_region`) — now GATED on
  `sparse_bricks.is_some()`: with a unified map an unconditional take WOULD have let a dense
  body's populate eat the region the dense consumers (chunked DC peek + partial upload) still
  need — the one real cross-consumer hazard found in the scout, closed structurally. Chunked
  CPU DC PEEKS (`dispatch_grid_dc_async`); the texture upload consumes after the upload lands
  (same sites in resident_volume.rs, renamed).
- **Deliberately kept internal (thin-Gate choice protecting C2):** (a) `pending.bake_window` /
  `BodyBakeResult.bake_window` / the worker-GPU result window = pure TRANSPORT from dispatch to
  the producer — no mesher reads them; (b) `SdfTileDirty` (mesher API), `SdfTiledStamp` and the
  atlas-generation guards are untouched — the staleness guards now fold AROUND the one region;
  (c) `SparseBakeRequest::Partial`'s payload stays as-is (escalation routing untouched). The C2
  incremental mesher (`mesh_into_slot_sparse_tiled`, tile diff, Phase-4b segments) has ZERO edits.
- **One intentional hardening beyond the merge:** dense FULL accepts now CLEAR instead of leaving
  a full-range entry. Decision-identical at every dense consumer, and it removes a real
  divergence class: a >512 dense-grid body entering the classify-prepass sparse path could have
  handed the tiled mesher a stale full-cover `Voxels` window instead of the correct
  full-table-rebuild `None`.

## Behavior-constancy analysis
- Same window VALUES reach the same consumers (the window travels with the request/result;
  the in_flight guard means the accept's window IS the dispatch's window).
- Union vs channel 1's overwrite diverges ONLY when a consumer was skipped between two accepts
  (inactive body); union is the conservative superset — same meshes (chunked re-extraction is
  deterministic, Gate-0 sorted merge), never-smaller upload window (the overwrite behavior was a
  latent stale-texel bug for multi-partial inactive bodies; union can only fix, never lose).
- %incremental: dispatch-side partial eligibility (window computation, dilation, S2.5b/c
  escalation, the `vol_full/partial_rebakes` counters at app_poll_bake.rs:993) has ZERO edits —
  the counters classify at dispatch and nothing upstream of them changed. Consumer-side
  eligibility preserved: partial accepts still yield a window (union superset of old), full
  accepts still route to full.

## GO gates
- **Dirty-region sources read by meshers: 3 → 1.** grep `last_bake_dirty_range` |
  `sparse_mesh_dirty_window` = 0 production refs; all meshers read `dirty_mesh_region` only.
- **Golden mesh hash 5/5 IDENTICAL bit-for-bit** — `dc_gate0_golden_determinism` green vs the
  pinned Gate-0 `GOLDEN_HASHES` (0xdfef8259… / 0x23c7b545… / 0x6a06b406… / 0x1033d5af… /
  0x11b48acb…); Gate-0 pick + ref-scene round-trip green.
- **New C2 contract violations: 0** — the full suite (incl. tiled INC parity, windowed-bake leak
  gates, Phase-4b segment tests) = **212 passed / 0 failed / 16 ignored** (`cargo test -p app
  --bins -- --test-threads=1`, ONE run, no environmental abort; 211 baseline + the new Gate-2
  test). Mesher asserts untouched.
- **%incremental ≥ baseline** — structurally unchanged (analysis above); the runtime drag
  percentage is owner-run (HUD `p/(f+p)` counter, Gate-0 measurement protocol) — nothing that
  feeds those counters was edited.
- **No new frame-thread work** — one HashMap insert moved from dispatch to accept (same poll
  phase); zero new loops/allocations. Frame p95 owner-run per protocol.
- **NEW test** `gate2_unified_region_partial_union_take_and_full_clear` (tests_sparse_bake.rs):
  two partial landings + a skipped mesh → take = exact union (⇒ same dispatched-tile set,
  since the existing INC gates prove tile-set = f(window) and the mesher API is unchanged);
  peek does not consume; take consumes exactly once; full acceptance clears; the dense router
  clears on full-covering and unions on partial. Green.

## Files touched (uncommitted — integrator merges)
`app/src/body_volume_cache/{mod.rs, bake_readback.rs, bake_dispatch.rs, resident_volume.rs,
undo.rs, duplication.rs}` · `app/src/app_poll_bake_poll.rs` · `app/src/app_poll_bake.rs` ·
`app/src/app_poll_misc.rs` · `app/src/app_poll_gpu.rs` · `app/src/gpu_ctx.rs` (doc) ·
`app/src/sdf_gpu_mesh/resident.rs` (doc) · `app/src/sdf_gpu_mesh/tests_sparse_bake.rs` (new
test) · `app_gpu/src/gpu_jobs.rs` (result window field).

## Owner manual test
Sparse regime (>512, Object Mode): drag a prim in short bursts — HUD %incremental stays at its
usual level and the surface updates live per drag frame (no full-rebuild hitches). Release +
settle (full bake), then drag again — the first tiled mesh after the full is a clean full
rebuild (no stale patch), subsequent drags incremental again. Dense 256 body: slider-drag a
prim — `[VolumeUpload] partial=true` in the debug log, mesh updates live; Ctrl+Z restores.

## Note for the owner (plan-doc sync)
Worktree has no `DC_CONSOLIDATION_GATED_PLAN.md` (main-dir doc not edited from here). To sync:
mark **GATE 2 = EXECUTED, GO** (3 → 1 sources; golden 5/5 identical; suite 212/0; C2 gates
green; %incremental structurally unchanged — owner-run confirms drag p95/% on the scripted
sequence).

# GATE 3 — session log (2026-07-17): flag-matrix → explicit `DcMeshRoute` — EXECUTED

**Goal (plan §GATE 3):** collapse the 5+ DC-routing booleans in `cache_state.rs` into ONE
explicit route enum with ONE derivation site, and compile out the dead brick-pool CPU-DC arm.
Behavior-constant; golden is the metric reine.

## The collapse — bool → enum/policy mapping
| Former bool | Disposition |
|---|---|
| `gpu_sdf_mesh` | INPUT to `CacheState::dc_mesh_route()` — master gate (OFF → `CpuChunkedOracle`) |
| `gpu_sdf_mesh_resident` | INPUT to `dc_mesh_route()` — selects the resident family |
| (body brick-residency + atlas_ok) | INPUT (`brick_resident_atlas_ok` param) — `SparseTiledResident` vs `DenseResident` |
| `gpu_sdf_mesh_resident_field` | NOT a route — per-frame-GUARDED G3 field-source sub-policy inside `DenseResident` (`use_resident_field` provenance guard falls back per frame; an enum cannot express that honestly) |
| `gpu_sdf_mesh_lazy_readback` | NOT a route — READBACK policy (skip CPU `to_vec` on partials); orthogonal, documented, left as bool per plan care-point |
| `gpu_full_bake_path_a` | NOT a route — BAKE-write policy (full bake in place into rv.buffer) |
| `gpu_sdf_mesh_sparse` | NOT a route — legacy dense->512 sparse PRE-PASS inside the resident populate (S2.6 made ≥512 structurally sparse at bake) |

`enum DcMeshRoute { SparseTiledResident, DenseResident, GpuReadbackMeshCache, CpuChunkedOracle }`
(`cache_state.rs`). **Deviation from the plan's "3 variants", declared:** the live matrix has a
4th reachable cell — the legacy G1 GPU-readback-into-mesh_cache path (`gpu_sdf_mesh=ON`,
`resident=OFF`), the documented Developer-Tweaks "surface vanishes / selection breaks" escape
hatch. Deleting it would remove a working fallback with no replacement (CLAUDE.md); it is kept
as an EXPLICIT dev-only variant. Production matrix = the plan's 3.

**One decision site:** `CacheState::dc_mesh_route(brick_resident_atlas_ok)` +
`dc_route_is_resident()` (the draw/pick/PT/LOD family predicate, lockstep by construction).
All former scattered bool tests now consume these: `app_poll_gpu.rs` (dispatch match + sparse
arm + LOD gate), `raster_gbuffer.rs` + `raster_pick.rs` (`is_resident` — pick stays lockstep
with the G-buffer gather, P0), `pt_scene.rs` (field-snapshot arm), `undo.rs` (drag bake
suppress), `app_poll_bake_poll.rs` (drag readback defer + lazy-readback gate).

**Override hook (tests/dev):** the bools REMAIN as storage — the smaller honest change per the
task; Developer Tweaks checkboxes still force every route (all-off = `CpuChunkedOracle`, the
parity oracle), now with the resident box gated under the master box + a live "Derived DC
route: …" label. No UI redesign; test harnesses unaffected (none flipped these bools directly).

**ONE intentional matrix-cell collapse (documented in `dc_mesh_route`'s doc):** the accidental
cell `gpu_sdf_mesh=OFF && resident=ON` previously ran the resident mesher while the
drag-suppress/readback-defer gates treated the app as CPU-DC (self-contradictory, contradicts
the Tweaks tree). Derivation is now hierarchical: master gate OFF → `CpuChunkedOracle`, period.
Default config byte-identical.

## Dead brick-pool CPU-DC arm — compiled out, dependency audit
The `brick_data` clone + `BrickGridAccessor` extract/update arms in `dispatch_grid_dc_async`'s
rayon job (pre-drift `app_poll_gpu.rs:870`) are REMOVED. Audit (grep `use_brick_pool`, every hit):
- Reachability: route == `CpuChunkedOracle` (Tweaks, GPU mesh OFF) AND `use_brick_pool=true`
  (properties toggle, default `false`, brick pool PARKED per CLAUDE.md) AND
  `body_brick_volumes` non-empty (`brick_classify.rs` — itself early-returns unless
  `use_brick_pool`). **No production caller.**
- Correctness of removal: `bake_readback` lands the dense grid in `body_grids` regardless of
  `use_brick_pool`, so the dense-grid CPU DC is valid for brick-pool bodies too — same field,
  same mesh (the accessor was an alternate reader of identical data).
- Untouched dependents (out of scope, still PARKED-functional): brick BAKE path
  (`bake_dispatch.rs`, `brick_classify.rs`, `bake_readback.rs` classify), properties toggle,
  viewport overlays, `core_math` `BrickGridAccessor`/`extract_all_from`/
  `update_dirty_chunks_from` (core_math-owned API + its tests — kept).

## GO gates
- **Golden 5/5 — IDENTICAL bit-for-bit:** `dc_gate0_golden_determinism` green, hashes
  `0xdfef82593cbaa428 / 0x23c7b5455afdbfaa / 0x6a06b406c632da9c / 0x1033d5af4136249c /
  0x11b48acb7f451bde` — byte-equal to the Gate-0 baseline table. GO.
- **Pick (P0):** `dc_gate0_pick_selection_roundtrip` green; pick predicate routed through the
  SAME `dc_route_is_resident()` as the draw gather (lockstep enforced at one site now).
- **Suite:** `cargo test -p app --bins -- --test-threads=1` = **214 passed / 0 failed / 16
  ignored** (212 + 2 new; no environmental aborts this run).
- **NEW tests** (`cache_state.rs::dc_route_tests`): `dc_route_derivation_table` — all 8
  (gpu_sdf_mesh, resident, brick_ok) combos pinned to their route, incl. the collapsed
  accidental cell; `dc_route_default_config_is_resident` — production default routes +
  resident-family predicate lockstep across all combos. Dead-arm removal proven by the audit
  above + compile (code gone; `cargo build -p app` green after every change).
- **Perf:** routing is not hot-path work — the change replaces scattered bool tests with one
  inlineable derivation; zero new allocations/loops on any per-frame path. Runtime regimes
  owner-run per the Gate-0 protocol (nothing feeding the HUD counters was altered).

## Files touched (uncommitted — integrator merges)
`app/src/cache_state.rs` (enum + derivation + docs + 2 tests) · `app/src/app_poll_gpu.rs`
(dispatch match, sparse arm, LOD gate, dead-arm removal) · `app/src/app_poll_bake_poll.rs` ·
`app/src/undo.rs` · `app/src/raster_gbuffer.rs` · `app/src/raster_pick.rs` ·
`app/src/pt_scene.rs` · `app/src/win_tweaks.rs` (box gating + route label) ·
`app/src/dev_tools.rs` (route in the flag dump).

## Owner manual test
1. Default launch: SDF body drags/edits look identical; HUD/log shows `[GpuMeshPath] … RESIDENT`.
2. Developer Tweaks → Experimental: the new "Derived DC route:" label reads `DenseResident
   (…SparseTiledResident)`. Untick "Resident GPU mesh" → label `GpuReadbackMeshCache`; body
   still renders + **click-select still works** (P0). Untick "GPU SDF Mesh" (master) → label
   `CpuChunkedOracle`; edits update on release (CPU oracle cadence), selection still works.
   Re-tick both → back to resident, live drag meshing resumes.
3. A ≥512 body: still meshes sparse (label's second route), no vanish on Tab/scope leave.

## Note for the owner (plan-doc sync)
Main-dir `DC_CONSOLIDATION_GATED_PLAN.md` not edited from this worktree. To sync: mark
**GATE 3 = EXECUTED, GO** (5+ routing bools → 1 enum — 4 explicit variants, 3 production + 1
declared dev-only legacy readback variant kept per "replacement before removal"; orthogonal
policies documented; golden 5/5 identical; dead brick-pool CPU arm compiled out with audit;
suite 214/0; pick test green).

# GATE 4 — session log (2026-07-17): `MeshPresentation` facade + shared materialize + export == display — EXECUTED

**Goal (plan §GATE 4, scope-corrected):** (a) ONE facade answering "has mesh / how to draw /
how to materialize CPU" per SDF body (the resident-vs-mesh_cache split affects ONLY export —
pick was already lockstep via Gate 3, save n/a, collision inexistent); (b) EXPORT consumes the
SAME DC the display shows, killing the independent CPU `mesh::dual_contouring` in win_export.

## What shipped
- **`app/src/mesh_presentation.rs` (NEW)** — the whole gate lives here:
  - `SdfMeshPresentation { None, Resident{pooled}, CachedCpu }` + the PURE derivation
    `derive_presentation(route_resident, slot_present, slot_pooled, pool_region, cpu_mesh)`
    + `AzraApp::sdf_mesh_presentation(nid, cid)` (wraps the Gate-3 route + slot/pool/mesh_cache
    state) and `sdf_cpu_mesh_present(cid)` (the PT-BLAS-source twin).
  - **Shared materialize job**: `dispatch_sdf_display_readback` (the PT pump's dispatch block
    extracted verbatim — sparse temp-slot mesh from the body atlas / dense display-slot copy),
    `display_slot_snapshot_ok` (the pump's slot predicate, now shared), `cpu_oracle_mesh`
    (production twin of `dc_baseline::body_golden` — recursive collect → mirror bounds → pad →
    `BodyField` → `ChunkedMesh` sharp), `mesh_to_verts8`, and
    `AzraApp::materialize_sdf_body_mesh(nid, res_override)` with 3 arms in preference order:
    display readback (default — hash-identical to the viewport) → grid DC through the SAME GPU
    QEF-DC shader (`mesh_readback`) at an override res → CPU `chunked_dc` oracle (no-GPU).
  - **Deferred export**: `PendingExport` + `AzraApp::process_export_pending` (poll phase; the
    panel only queues — Frame Ordering Contract) + `write_obj_objects` (ONE OBJ writer for
    poly+SDF objects, unit-testable).
- **Blocking choice (stated):** export uses a bounded `device.poll(Wait)` readback inside poll —
  the `dispatch_readback` precedent — because export is a one-shot user action; the pump's async
  pattern remains for the per-frame PT path. Only the OBJ file write runs on a thread (UX
  unchanged: "Running in background — UI stays responsive").
- **win_export.rs SDF arm switched:** independent CPU `dual_contouring` + quads/sharp options
  REMOVED; SDF bodies now export per-body (named objects, world-transformed with
  `effective_body_transform` + the rigid-offset re-anchor when display-sourced), triangles,
  sharp QEF — the display mesh. New checkbox "Use display resolution (match viewport)"
  (default ON → `res_override=None` → export == display); the 16..512 slider is now the
  override, routed through the SAME pipeline. Poly/voxel arms untouched (poly writer output
  byte-pattern unchanged: same `v/vn/f a//a b//b c//c`).

## "Has mesh?" predicate migration (3 → 1 facade)
- `raster_gbuffer.rs:129` (`dc_route_is_resident && sdf_body_mesh.contains_key`) → `pres.is_resident()`
- `raster_gbuffer.rs` mesh_cache presence (`mesh_cache.get(cid).is_none()`) → `!pres.has_any_mesh()`
- `raster_gbuffer.rs:~204` Stage-B-1 pooled (slot.pooled && pool.slot(nid)) → `pres.is_pooled()`
- `raster_pick.rs:65-69` same three → the SAME facade call — draw/pick lockstep now by construction
- `pt_scene.rs` gather SdfBody arm (`mesh_cache.get`) → `sdf_cpu_mesh_present(cid)` (facade twin)
- `pt_scene.rs` pump `slot_ok` → shared `display_slot_snapshot_ok`
- NOT migrated (deliberate, listed): `app_poll_gpu.rs:187/287/379/702` + `dev_tools.rs:52` are
  dispatch-internal slot maintenance / debug HUD, not "has a drawable mesh?" gates.

## PT snapshot behavior — code-audit statement (zero change)
The pump keeps VERBATIM: candidate walk + quiet-gate + SETTLE-gate (`PT_SNAP_SETTLE` hash-hold),
`pt_sdf_snap_hash`/`_seen`/`_pending` keying, one-kick-per-poll bound, stale/empty DROP on land,
atlas-identity and slot gates. Only the 11-line dispatch block moved into
`dispatch_sdf_display_readback` (same buffers, same `readback_slot` contract) — diff-inspectable.

## GO gates
- **Facade predicates 3 → 1**: migrated sites listed above; equivalence PROVEN exhaustively by
  `gate4_facade_matches_legacy_predicate_union` (all 32 input combos vs the legacy union).
- **CPU-mesh producers for PT+export = 1 shared materialize**: PT pump and export both call
  `dispatch_sdf_display_readback`; the CPU oracle is `cpu_oracle_mesh` (one fn), pinned to the
  golden pipeline by `gate4_cpu_materialize_matches_golden_pipeline` (verts+quads equal).
- **Export at display res == display (HEADLINE)**: `gate4_export_mesh_matches_display_mesh_at_display_res`
  (GPU, `require_adapter`, `#[ignore]` per Gate-0 discipline) — run on real HW, **5/5 ref bodies
  GREEN**: the default export arm (display-slot readback) is BYTE-hash-identical to the display
  buffers; the res-override arm (same QEF-DC shader over the same grid) is triangle-set-identical
  (order-canonical hash — measured fact: GPU atomic-append emission order is dispatch-
  nondeterministic, identical counts + identical triangle set, permuted order; byte-order
  identity across independent dispatches is physically not a property of the GPU DC, so the
  canonical hash is the correct "same mesh" metric for that arm).
- **Export file validity**: `gate4_export_obj_file_validity` — OBJ round-trip parse: v/vn counts,
  1-based index bounds, finite positions, tri-only faces, writer-reported totals match.
- **Golden 5/5 display**: `dc_gate0_golden_determinism` green, hashes byte-equal to the Gate-0
  table (0xdfef82593cbaa428 / 0x23c7b5455afdbfaa / 0x6a06b406c632da9c / 0x1033d5af4136249c /
  0x11b48acb7f451bde).
- **Suite**: `cargo test -p app --bins -- --test-threads=1` = **217 passed / 0 failed / 17 ignored** (214 baseline + 3 new headless; the GPU headline test is the 17th ignored, run explicitly with --ignored → GREEN 5/5; no environmental abort this run).

## Files touched (uncommitted — integrator merges)
`app/src/mesh_presentation.rs` (NEW: facade + materialize + deferred export + OBJ writer + 4 tests) ·
`app/src/main.rs` (mod decl) · `app/src/raster_gbuffer.rs` · `app/src/raster_pick.rs` ·
`app/src/pt_scene.rs` (facade + shared dispatch; gates verbatim) · `app/src/win_export.rs`
(SDF arm → shared materialize; UI: display-res checkbox, slider = override) ·
`app/src/types/types_ui.rs` (`export_display_res`, `export_pending`) · `app/src/app_poll.rs`
(process_export_pending hook) · `app/src/dc_baseline.rs` (pub(crate) on build_ref_scene /
body_golden / require_adapter / RefBody.nid+name — test-harness visibility only).

## Owner manual test
1. Open a scene with an SDF body (any res), File → Export Mesh (OBJ). Leave "Use display
   resolution (match viewport)" checked → Export. Console shows `[Export] body '…' … via
   DisplayReadback`. Import the OBJ back (or into Blender): the surface is EXACTLY what the
   viewport shows (same silhouette, same bevel/cut detail — it IS the display mesh, world-placed).
2. Uncheck it, set the slider to e.g. 256 → Export: same shape at the override res (same DC
   pipeline, finer/coarser sampling), still matching the viewport's look.
3. A ≥512 sparse body exports too (atlas temp-slot readback) — previously the CPU export
   diverged from the sparse display. Poly bodies in the same scene still export unchanged.

## Note for the owner (plan-doc sync)
Main-dir `DC_CONSOLIDATION_GATED_PLAN.md` not edited from this worktree. To sync: mark
**GATE 4 = EXECUTED, GO** (3 has-mesh predicates → 1 `MeshPresentation` facade, exhaustively
equivalence-tested; PT + export share ONE materialize job — the pump's machinery extracted, its
settle/hash/one-per-poll gates verbatim; export = the display's DC — default arm byte-identical,
override arm same-shader triangle-set-identical [GPU emission order is atomic-append-
nondeterministic — documented]; golden 5/5 identical; export-validity + facade tests new).

---

# Session 2026-07-17 — SUB-BODY EVAL-ORDER (visual-notch root B): diagnosed EXPECTED-MATH, cross-path parity proven

**Task:** diagnose (and fix only if provably a defect) root (B) of the owner's visual-notch bug —
"grouping prims into a sub-body / reordering changes the boolean evaluation order and the surface".

## Findings (repro matrix, CPU dense grid 17³ over [-1.5,1.5]³ + GPU bake 48³ parity)
| Operation | Delta | Class |
|---|---|---|
| Reorder within hard-union run (k=0) | **exactly 0** | invariant — no bug (min associative, culls exact) |
| Group hard-union run into Union{k:0} sub-body (origin shift) | 6e-8, 0 flips | invariant — no bug |
| Nested hard-union chain vs flat | **0** | invariant — no bug |
| Reorder within smooth-union run (equal k=0.25) | max\|d\|=0.0084 (< k) | intentional — pairwise smin non-associative |
| Group smooth-blended prims (default hard group op) | max\|d\|=0.062, 12 sign flips | **THE notch** — intentional: group joins parent via body_op k=0, blend hardens at boundary; group op k=0.25 → residual 0.0084 |
| Group [Union, Subtract] into sub-body | flat +0.300 vs grouped −0.200 at probe | intentional — Subtract scoped to group (parenthesization; the opposite was the fixed sub-body defect) |
| Subtract prim moved to slot 0 | −0.4 vs +0.4 | intentional — first-operand seeds regardless of op (all 3 paths) |

**Cross-path:** CPU `BodyField` ↔ production `volume_bake.wgsl` = 0 sign flips, near-surface max|dv| = 0.0000
on all four new `evalorder_*` scenes (reordered smooth run both orders, grouped-hard-boundary tree,
subtract-first seed). `sdf_scene.wgsl` scope machine reviewed line-by-line: identical first-operand /
empty-parent / mirror-union semantics, culls conservative-only. **No path disagreement → no defect → no
production code changed → golden 5/5 untouched by construction (suite re-run green).**

**Stale-tracker corrections:** eval order never came from `all_primitives_ordered()` (flat-cache only);
`group_selected_into_body` already lands the group at the earliest extracted prim's slot (`insert_slot`).

## New permanent gates (uncommitted)
- `core_math/src/field_eval.rs` — 6 `[EvalOrder]` tests: 3 invariance gates (a regression = real defect)
  + 3 characterization pins (a silent semantic change trips them).
- `app/src/sdf_gpu_mesh/tests_bake.rs` section (l) — 4 `evalorder_*` CPU↔GPU parity scenes inside
  `body_mirror_bake_matches_cpu_eval`.

## Escalated to owner (semantic decisions, NOT shipped)
1. Order-canonical fold for commutative smooth runs (sort by prim_uid) would make reorder invariant but
   CHANGES every existing scene's blend look — decide explicitly.
2. Group-op default: when grouped prims carry smooth k, default the sub-body `body_op` to
   Union{k = max prim k} (removes the visible crease on group). Cheap, but changes group behavior.

## Owner manual test
1. Two spheres overlapping with Smooth Union k≈0.25 + a third overlapping both. Ctrl+G the two rightmost
   → a crease appears where the blend crossed the group boundary (expected math, not corruption).
   Select the group, set its boolean k to 0.25 → the crease softens back.
2. Same three spheres with k=0 (hard union): Ctrl+G and reorder freely → the surface must NOT change at
   all. If it ever does, that is a real regression (the invariance gates would catch it).

# ════════════════════════════════════════
# SESSION 2026-07-17 — BLACK PBR VIEWPORT (root-caused + fixed) + ANALYTICAL-MATERIAL CLEANUP
# ════════════════════════════════════════

## Part 1 — PBR viewport rendered every raster surface black (SDF worst-hit)
**Root cause — NOT materials.** `render/src/shadows.rs update_cascades()` passed the raw light-view
z coordinates (`min_ls.z`, `max_ls.z`, both negative in front of the light) as glam
`Mat4::orthographic_rh(..) `near/far — glam expects POSITIVE distances along -Z. Every caster and
receiver mapped to NDC depth ≈ 2.6–3.6 (outside [0,1]): casters clipped from the shadow maps, and
every receiver's reference depth failed the `LessEqual` compare against the cleared 1.0 →
`compute_shadow()` = 0.0 for ALL raster geometry inside the cascade range. That 0 is baked into
gbuf_material.z by `fs_gbuffer` and (correctly, per VXSOLID) NOT recomputed for raster pixels in
deferred — so PBR multiplied both direct light AND ambient by 0 → black. Solid mode survived only
via its 0.55 floor; since SDF went raster-only (2026-07-14 analytical deletion) SDF inherited it.
**Fix:** cascade math extracted to pure `compute_cascade_vps()` (unit-testable), ortho call now
`(-max_ls.z, -min_ls.z)`, near-plane expansion moved to the light side (`max_ls.z += 50`).
Regression test `cascade_depth_maps_into_unit_range` (render crate) — verified to FAIL on the old code.

**Secondary (the owner's actual "no material input" report):** the raster SDF branch
(`app/src/raster_gbuffer.rs`) read the prim's EMBEDDED `p.material` and ignored `material_id` —
an assigned scene Material node never reached the PBR/raster surface. Fix: canonical resolver
`SceneGraph::resolve_prim_material()` (promoted from serialize.rs's private copy; serialize.rs +
`body_first_albedo_map` + raster_gbuffer all delegate to it). Fallback chain is structurally
non-black: graph material → embedded material → default (warm-gray 0.66/0.585/0.53).
4 new headless tests in `raster_gbuffer.rs::tests` pin the chain (default non-black, assigned
graph material wins, dangling id falls back, Default Material node non-black).

## Part 2 — analytical material system audit (owner-requested)
| Candidate | Consumers found | Verdict |
|---|---|---|
| `volume_preview.wgsl` + `VOLUME_PREVIEW_WGSL` + `build_volume_preview_wgsl` + `build_sdf_eval_block` + `vol_preview_bgl_entries` (incl. analytical `read_body_material`) | build fn: ZERO app callers (only its own Naga tests); BGL: zero callers — bgl.rs itself marked it "DEAD CODE … removed in a later slice" | **DELETED** (this session) |
| `sdf_material.wgsl` eval chain (`eval_material`/`_blended`/`_dna`/`eval_node_material`/`find_hit_prim`) | deferred-FULL (DEFERRED_MATERIAL + SDF_REFLECT + Lit branch); pinned by "Feature Must Work in Both Paths"; shares file with LIVE shadow/AO/RNG | **KEEP** |
| Per-prim texture atlas (`tex_atlas_buffer`/`tex_meta_buffer`, `texture_id`) | populated by `ai_texture.rs`; bound in deferred BGL (atlas 26) | **KEEP** (live feature) |
| Embedded `SdfPrimitive.material` | serialized (save format — red line) + material_id fallback | **KEEP** |
| `MaterialLibrary` | v4/v5 file-compat serialization only (rule-pinned) | **KEEP** |
| pt_bgl "legacy analytical-PT bindings" (2/3/4/21/22/23/26/27/40/41) | pathtrace_tri_entry still declares them; removing = PT shader surgery | **KEEP — flagged** for the PT Phase-3 pass |
| **FINDING (not fixed, out of scope):** deferred-FULL is currently UNREACHABLE — `resolve_render_mode` always returns `AnalyticalLite` (post analytical-deletion), so `use_lite_deferred` is always true (viewport_render.rs:1365-1374) and `ensure_deferred_full_pipeline` never fires. PBR loses SDF_REFLECT reflections + full material eval. Deliberate re-enable (e.g. full deferred for Pbr/Lit idle frames) is a separate owner decision. | | REPORTED |

## Gates
build green; graphics 43/43 (all Naga variants); core_math 520; render 21 (+cascade regression);
app --bins 221/221 (incl. dc_gate0_golden_determinism + pick gates), test-threads=1.

## Owner manual test
1. PBR mode, default-material SDF body → lit warm-gray surface (was black).
2. Assign a scene Material (e.g. red) to a prim → albedo/roughness now visible in PBR (was ignored).
3. Solid mode unchanged; shadows in PBR/Lit now actually darken correctly instead of blackening everything.

---

# Session 2026-07-17b — SHADOWACNE: sawtooth self-shadow blobs in PBR (follow-up to PBRBLACK)

**Symptom** (owner screenshot): beveled SDF cube in PBR shows large jagged sawtooth dark blobs on top/side faces — classic acne + texel crawl. Appeared once 6aa11a2 made shadows actually resolve.

**Root cause, quantified.** Receiver bias in `gbuffer.wgsl` was a fixed `world_pos + n * 0.03`, tuned when compute_shadow()==0 everywhere (any value "worked"). Measured texel world-sizes for the app camera (far = max(dist·20, 500), 45° fov, 1024² maps), AABB fit: cascade 0 = 0.059, c1 = 0.220, c2 = 0.734 world units/texel → the 0.03 offset = 0.5 / 0.14 / 0.04 texels — an order of magnitude short. Plus: ortho box refit to the light-space corner AABB each frame (extent pumps while orbiting) and no texel snapping (grid crawls under any camera motion) → the staircase/sawtooth edges.

**Fix** (uncommitted, feat/sparse-3a):
- `render/src/shadows.rs` — `compute_cascade_vps`: bounding-SPHERE fit (radius invariant under all rigid camera motion → stable texel size), radius quantized to 1/16 (kills float-noise drift of the snap grid), slice center snapped to whole texels (x, y AND z) in a fixed light rotation frame, symmetric extents ±(r+1 texel pad), depth window analytic `ortho(-r..r, near=-r, far=50+r)` — same 50-unit caster headroom as 6aa11a2, D32F so range is a non-issue. Exports per-cascade `texel_world` in `ShadowUniforms` (now 224 B).
- `render/src/gbuffer.wgsl` — `shadow_data.texel_world` field; `compute_shadow(world_pos, n)` applies the bias AFTER cascade selection: `n * texel_world[c] * NORMAL_OFFSET_TEXELS` (1.5, derivation in-shader: 0.5-texel PCF footprint + 1-texel caster depth quantization; grazing slope covered by the existing hardware slope-scale DepthBiasState on the shadow pipeline — constant 2 / slope 2.0 kept). No magic world constant remains.

**Tests**: new `texel_world_size_sane` (texel per cascade in derived bounds, monotone, 1.5-texel offset <1% of extent → no peter-panning) + `texel_snapping_is_stable` (determinism; ±0.05-texel camera translate → bit-identical cascade matrix on at least one side). `cascade_depth_maps_into_unit_range` untouched, green.

**Gates**: app build green; render 23/23; graphics 43/43; app --bins 221/221 (threads=1); golden untouched (geometry-free).

**Owner manual test**: PBR, beveled cube + floor: faces cleanly lit, no sawtooth blobs, no acne while orbiting light/camera; contact shadow at cube base still lands (no peter-panning); Solid mode unchanged.

---

# Session 2026-07-17c — SHADOWACNE-3: the residual dark blob KILLED by a GPU readback harness (light frame was UPSIDE-DOWN)

**Symptom** (owner screenshot after 2a23477): sawtooth gone, but a large SOFT dark blob on the cube's top + upper-left faces, shaped like the cube's own silhouette projected along the light direction. Mandate: no reasoning-only fix — build a headless GPU repro that renders the real shadow pass, evaluates the real `compute_shadow()`, reads back the factors.

**The harness** (`render/src/shadows.rs::tests::shadow_readback_default_sun_cube_and_floor`, skips without an adapter): unit cube (y∈[0,2]) + 24×24 floor quad → the REAL `ShadowMapSystem` pipeline (all 3 cascades, real fitting) → a fragment probe whose shader is the `ShadowData` struct + the whole `// ---- Shadow Sampling ----` section EXTRACTED VERBATIM from gbuffer.wgsl at test time (no copy that can drift) → per-point factors read back via `copy_texture_to_buffer` + `map_async` (Result captured). Probe groups: 25 top-face points, 18 light-facing side points, 1 CPU-ray-oracle-verified floor-in-shadow point, 3 floor-out points. Sun = the app's ACTUAL default (`LightConfig{az:45, el:55}.direction()` = (0.406, 0.819, −0.406), value-pinned), passed exactly as `raster_gbuffer.rs:490` passes it.

**Readback BEFORE (HEAD 2a23477) — the blob, numerically:** ALL 43 lit-face probes read shadow = **0.000** (expected ≥0.95); floor-in-shadow read 0.393 (expected ≤0.2); floor-out read 0.217 / 0.012 / 0.113 (expected ≥0.95). 47/47 probes wrong.

**Root cause — hypothesis "none of the five, but H-A's neighbour": the light FRAME was built 180° inverted.** `LightData::direction()`/`LightConfig::direction()` return the direction TOWARD the sun (default el=55° → y=+0.82, UP) — the N·L convention every shading term uses. `compute_cascade_vps` treated its `light_dir` parameter as the PROPAGATION direction (`eye = center − light_dir·50`, snap frame `look_at(0, light_dir)`). The only production caller passed the to-sun vector unmodified → the shadow camera sat 50 units BELOW the scene looking up. Every object's shadow map contained its own UNDERSIDE; receivers on top/lit faces compared against the bottom-face depth → self-shadow exactly inside the object's own silhouette projected along the (mirrored) light direction — the smooth blob, and mirror-side cast shadows on the floor. Why 2 fixes missed it: both prior tests HAND-WROTE downward (propagation-style) light vectors, so the math was internally consistent in tests and inverted only in the app path; fixes 1+2 (depth window, adaptive bias, snapping) were real and stay.

**Fix** (uncommitted, feat/sparse-3a): `compute_cascade_vps`/`update_cascades` now take **`dir_to_sun` — the scene's native convention** — and derive the propagation direction internally in ONE place (`let light_dir = -dir_to_sun.normalize()`), with the convention documented on the fn. The dual-convention class is eliminated: render consumes what the scene stores, no caller-side negation to forget. `raster_gbuffer.rs` call site unchanged (now correct by construction) + convention comment. The 3 existing shadow unit tests updated to pass to-sun vectors.

**Readback AFTER:** all 47 probes pass — 43 lit-face/floor-out ≥0.95, floor-in-shadow ≤0.2 (the cast shadow still LANDS on the correct side — no peter-panning). Harness proven to fail on HEAD before the fix.

**Gates**: `cargo build -p app` green; `cargo test -p render` 24/24 (+1 harness, 3 prior shadow tests green); WGSL untouched (no graphics gate needed); app --bins threads=1 (see session end); golden untouched.

**Owner manual test**: PBR, cube (or beveled SDF cube) + floor, default sun: top and sun-facing faces FULLY lit at every camera angle (no dark blob hugging a corner); cast shadow on the floor lies on the side AWAY from the sun (down-left of the body for the default upper-right sun) and stays attached at the base; orbit sun elevation/azimuth — the shadow sweeps correctly, no self-shadow reappears; Solid mode unchanged.

---

# Session 2026-07-17d — Round 4: the dark banding that SURVIVED DELETING THE SUN (stale shadow × ambient)

**Owner clue (decisive):** the dark blob on the cube persists with ZERO lights in the scene (cube seen from slightly below; under-side face carries dark streaky banding). With no lights, a PBR pixel = ambient × whatever still multiplies it — so the sun-shadow chain fixed in rounds 1–3 cannot be the source *unless something still applies a shadow factor with no sun*. It did. Culprit: **S-1** (stale/undisabled shadow state after light deletion), compounded by a shadow/AO conflation in the deferred composition. S-2 (calc_ao marching the analytical field) exonerated: `is_raster_pixel` gates the SDF shadow/AO recompute OFF for raster pixels (deferred_entry.wgsl:470), and post-2026-07-14 every geometry pixel is raster.

**Mechanism, traced:**
1. `delete_light` removes the graph node; `sync_light_to_gpu` (scene_actions.rs:334) **early-outs when zero lights remain** → `scene.light_config` retains the dead sun's direction forever.
2. `raster_gbuffer.rs` unconditionally rendered the 3 shadow cascades from that stale direction, and `fs_gbuffer` unconditionally baked `compute_shadow()` into gbuf_material.z.
3. `deferred_entry.wgsl` uses that baked value as BOTH `final_shadow` AND `final_ao` for raster pixels, and `ambient = scene_ambient * final_ao` — the zero-light ambient fallback (0.15·env) was multiplied by a shadow compare against a light that no longer exists. Away-facing faces sit behind the caster's own top surface in the stale light frame → factor 0 with partial-PCF fringes → the soft streaky banding.

**Readback proof (harness extended, not duplicated):** new `shadow_readback_zero_lights_full_ambient` (render/src/shadows.rs) — same real-pipeline probe infra as round 3, camera slightly BELOW, 27 probes on the away-facing faces (bottom / −x / +z). Zero-light spec: every probe must read 1.0. **On HEAD 14f4809: 27/27 FAIL — 0.000 across all face interiors, 0.159–0.435 partial PCF near the top edges (the owner's "soft streaky blob" gradient, numerically).** After the fix: 27/27 pass, with the harness still RENDERING the stale cascade maps — the flag alone neutralizes them.

**Fix (uncommitted, feat/sparse-3a):**
- **S-1 structural gate.** `ShadowUniforms` gains `flags` (x = sun_enabled; struct 224→240 B, size test updated); `update_cascades()` takes `sun_enabled: bool`; `compute_shadow()` (gbuffer.wgsl) returns 1.0 before sampling anything when flags.x == 0. `raster_gbuffer.rs` derives `has_sun` from the SCENE GRAPH every frame (`lights().any(visible && Directional)`) — never a cached flag, so deletion/undo/visibility-toggle/load all disable shadows the same frame with no event plumbing to forget. The 3 cascade depth passes are skipped entirely when no sun exists (stale maps are never sampled thanks to the flag).
- **Shadow ≠ AO for raster pixels.** deferred_entry.wgsl: `final_ao = 1.0` (PBR/Solid geometry branch) and `lit_ao_only = 1.0` (Lit branch) for raster pixels. gbuf_material.z is a DIRECTIONAL shadow-map factor; a cast shadow blocks the sun, never the sky. This is the with-sun half of the same artifact (with the sun re-added, away-facing faces got `ambient × ~0` banding). Both gates live OUTSIDE the SHADOW_AO region (df_lite strip safe); SDF march-pixel recompute path untouched.

**OWNER-VISIBLE TRADE-OFFS (deliberate, stated not silent):**
1. **Cast shadows in PBR/Lit are lighter than before** — the shadowed region keeps full ambient + IBL diffuse (physically correct; previously ambient was also zeroed inside shadows, which is what made them read pitch-black).
2. **Solid mode loses cast-shadow darkening on raster surfaces** (`final_ao` fed its 0.45 occlusion term). This actually *completes* Solid's own documented decision ("DROP the directional cast-shadow term … every pro DCC's Solid mode omits them") — the term had survived through the final_ao alias — and kills the residual "boolean cutter darkens the base body at 0.55 floor" path. But it is a visible change: Solid is now pure form shading for raster pixels.
3. **Raster pixels currently have NO ambient occlusion source** (they never did — they had cast-shadow-as-AO). If contact darkening in PBR is missed, the right future primitive is a real SSAO pass, not re-multiplying ambient by the sun's shadow map.

**Gates:** `cargo build -p app` green; render 25/25 (new harness + 47-probe sun harness + 3 shadow unit tests); graphics 43/43 (all Naga variants, deferred_entry touched); app --bins 221+ threads=1 green; golden untouched.

**Owner manual test:** (1) PBR, cube, DELETE the sun (zero lights): cube uniformly ambient-lit — no dark bands on any face, from any angle (including from below). (2) Re-add a sun: round-3 behavior intact — lit faces full, cast shadow lands on the away side; shadowed floor is dimmer but no longer black (ambient survives). (3) Toggle the sun's visibility eye: shadows switch off/on the same frame. (4) Solid mode: no cast shadows (form shading only).

# Session 2026-07-17e — Real SSAO: the occlusion source 17d promised ("the right future primitive is a real SSAO pass")

**What:** raster pixels lost their (wrong) ambient-darkening source in 17d — the sun-shadow factor was evicted from `final_ao`, deliberately leaving PBR/Lit without contact darkening. This session builds the real primitive: a half-resolution hemisphere SSAO pass feeding the deferred ambient/IBL terms. **The 17d trade-off #3 is now CLOSED.**

**Architecture (new module `render/src/ssao.rs` + `render/src/ssao.wgsl`):**
- 3 compute passes over a HALF-res rgba16float target: `ssao_main` (12 cosine-distributed hemisphere taps around the G-buffer normal, golden-angle spiral rotated per pixel by interleaved gradient noise — deterministic, zero per-frame jitter, hence single-frame stable while orbiting; NO temporal filter in this slice by design) → `ssao_blur_h`/`ssao_blur_v` (depth-aware separable blur; blur_v applies `mix(1, ao, strength)`). r = factor, g = half-res depth.
- **Radius is WORLD-space** (default 0.5, slider 0.1–2.0): taps are placed in world space and reprojected (both perspective + ortho branches, same packing as `camera_ray`), so the screen kernel scales with distance — no magic pixel radius. Range check (`smoothstep(radius / |Δdepth|)`) kills halo bleeding; bias = 0.02 + t·0.002 kills flat-surface acne (readback-proven, probe (a) = 1.000). Occlusion contrast ×1.5 — the same default as the long-standing `volume_ao_intensity`; the quadratic tap-clustering ramp measured 0.854 at the contact corner (too weak) and was re-calibrated to a linear ramp + intensity → 0.736.
- **Deferred consumption** (`deferred_entry.wgsl` binding 38 + `ssao_factor()`): depth-aware bilinear upsample (rejects taps >~5% relative depth difference; falls back to 1.0 on total disagreement — never darkens on uncertainty). Applied ONLY to `ambient` and diffuse-IBL `kd` for **raster pixels in PBR (view_mode 0) and Lit (6)** — never direct sun/point light (AO is sky occlusion), never Solid (owner ruling: pure form shading), never the SDF march-pixel path (it has `calc_ao`). All new code sits OUTSIDE every `#region` (df_lite-strip safe); in the df_lite build Lit pixels shade through the PBR branch, which is why the gate accepts view_mode 6 there — the full build's Lit early-return applies it in the LIT_BRANCH region, so it can never double-apply.
- **Not a Blend Dispatch:** SSAO is a per-pixel factor texture consumed by deferred, not a composite of two rendered results — `dispatch_blend()` (AlphaOver/Additive/…) is the wrong shape; the natural shape is the fog/SSR pattern (compute pass + texture sampled by deferred), which is what shipped.
- **Toggle** "Ambient Occlusion (SSAO)" (render panel, default ON) + AO Strength (0–1, default 0.7) + AO Radius. OFF skips all 3 dispatches (Frame Budget) and white-fills the factor texture ONCE (`SsaoResources::ensure_white`) — deferred samples binding 38 unconditionally and reads exactly 1.0 = the 17d behavior. Settings are runtime (`RenderSettings`) — deliberately NOT added to the bincode `ProjectFile` (save-format red line).
- **Plumbing:** pipelines background-compiled (now "ALL 14") with `ensure_ssao_pipelines()` lazy fallback (pt_denoise precedent) + disk PipelineCache; `SsaoResources` (textures + params + bind groups) created ONLY in the viewport resize block, in the same statement group as the `gbuf_normal` it reads and the `deferred_bg` that samples it — no cached reference can dangle, no generation counter needed beyond that co-location. Profiler: `PASS_SSAO` (slot 14) on the main pass only; fixed the HUD copy that capped at 10 slots and silently dropped FXAA/Shadow/RasterGbuf/Bake from the overlay.

**Readback proof (`render/src/ssao.rs`, headless, real raster G-buffer via RasterPipeline + gbuffer.wgsl, real 3-pass SSAO via the production `dispatch()` — the app and the test share ONE driver):** cube-over-floor, 512², radius 0.5 / strength 0.7, sun disabled (zero-light scenario): (a) open floor **1.000** (≥0.9 — no acne), (b) floor-cube contact 0.08 from the wall **0.736** (≤0.75 — visible contact darkening), (c) cube top-center **1.000** (≥0.9), side face 1.000; ambient-crush guard: every open probe ≥0.5 → `ambient × AO ≥ ambient·0.5`, the cube stays visible with zero lights. (d) two runs **byte-identical**. Perf: **0.071 ms** for all 3 passes at 1920×1080 (half 960×540, GPU timestamps, the owner's 4090) — 20× under the 1.5 ms budget.

**Gates:** `cargo build -p app` green; render 29/29 (+1 ignored bench; both shadow harnesses still green); graphics 43/43 (all Naga variants — deferred_entry touched); app --bins threads=1 green; golden untouched. Uncommitted on feat/sparse-3a.

**Owner manual test:** (1) PBR, cube on floor: soft contact darkening at the base and in crevices; flat open surfaces stay clean (no blotches). (2) AO Strength slider: 0 = 17d look, 1 = strong contact shadowing, live response. (3) Toggle OFF: exact 17d behavior (and the profiler's SSAO slot disappears — dispatches skipped). (4) Solid mode: unchanged, no AO. (5) Orbit in PBR: no shimmer/boiling in the darkened creases (deterministic per-pixel noise, no temporal jitter). (6) Lit mode: same contact darkening on the ambient term only.

---

# Session 2026-07-17f — Shadow quality controls (map resolution + PCF softness) + the dead "Volume Shadows" sliders pruned

**The ask (owner):** "a slider for better shadow quality" → TWO controls in a new **"Sun Shadows"** group in the render panel (next to SSAO), plus an audit of the legacy shadow sliders.

**Control 1 — Shadow Map Resolution (1024² / 2048² / 4096² per cascade).**
- `ShadowMapSystem` gains `resolution` + `generation`; `SHADOW_MAP_SIZE` const demoted to `DEFAULT_SHADOW_MAP_SIZE` (seed only — no hardcoded map size anywhere downstream, including the shader: `texel_size = 1/ShadowData.params.x`).
- **Frame Ordering Contract honored:** the panel ONLY writes `RenderSettings.shadow_map_resolution`; `poll_background_tasks` compares and calls `set_resolution()` — the sole place the cascade depth textures are recreated. Recreation bumps `generation`; `raster_gbuffer` compares `RasterPipeline.shadow_map_gen` (the `mat_tex_gen` pattern) and rebuilds `shadow_bind_group` the SAME frame — generation counters, no timing tricks.
- **Zero bias retuning needed** — the 17b payoff, now proven twice: `texel_world_size_sane` asserts the 2048 texel is exactly half the 1024 texel, and a NEW readback harness variant (`shadow_readback_2048_bias_adapts`) runs the full 47-probe sun spec at 2048² — no acne on lit faces, floor contact shadow still lands.
- VRAM documented in the dropdown tooltip: 3 cascades × D32 = 12 / 48 / **192 MB @ 4096** (fine on the 4090). **Default stays 1024** (no owner-machine profiler measurements from this session to justify a hotter default; the profiler's existing `PASS_SHADOW` slot shows the cascade-0 cost live when the owner bumps it).

**Control 2 — Shadow Softness (PCF radius in texels, 0.5–3.0, default 0.5 = the old fixed 2x2 kernel bit-for-bit).**
- `ShadowUniforms`/`ShadowData` gain `params` (x = resolution, y = pcf radius; struct 240→256 B, size test updated). WGSL: r ≤ 1 keeps the classic 2x2 quad at ±r texels; r > 1 widens to a 3x3 grid at ±r (hardware bilinear comparison fills between taps). Uniform-value branch.
- **The anti-acne offset stays DERIVED, not a second magic constant:** `NORMAL_OFFSET_TEXELS = 1.5` (the 17b "0.5-texel PCF + 1-texel quantization") is replaced by `normal_offset_texels() = params.y + 1.0` — the same derivation with the footprint term now live. New harness variant `shadow_readback_max_softness_no_acne` runs the 47-probe spec at r = 3.0: no acne, no peter-panning (4-texel offset ≪ 1% of cascade extent, asserted in `texel_world_size_sane` at both slider ends).

**Legacy-slider AUDIT (the owner's "leftover testing tech"):**
- **"Volume Shadows" block (Shadow Opacity / Softness / Reach / Bias / Steps + toggle) — DEAD, REMOVED.** Their sole shadow consumer was the VP shader, deleted 2026-07-13. Worse: their uniform packing (viewport_render.rs:932-945) was still stomping the fields it squatted — `matcap_rotation` = reach (40 rad!), `matcap_edge_enabled` = steps (16 → edges force-ON), `matcap_edge_threshold` = bias, and the fog pass's `fog_height_falloff`/`fog_scatter` (fog shares GpuUniforms). Removed the 6 `RenderSettings` fields (runtime-only struct, no serde — save-format safe; not in ProjectFile), the UI block, and the packing. **Side effect fixed for free: MatCap Rotation/Edge sliders now actually reach the GPU** — the hijack was the only writer of those uniforms, so the panel's matcap edge/rotation settings had been silently ignored/corrupted since the VP deletion.
- **Kept (live):** fog "Shadow Steps" (`fog_shadow_steps` — fog god-ray march, fog pass reads it), Raytrace "Shadow Samples" (`rt_shadow_samples` — RT mode uniform), "Interaction Shadows" (`interaction_no_shadows` — SDF soft-shadow skip during drags), `shadow_blur_radius` (deferred bilateral on gbuf shadow — no UI, Developer Tweaks territory). Pre-existing aliasing note: the Volume AO block still packs its params into fog uniform fields (collides when fog + AO both on) — untouched, out of scope, flagged here.

**Harness integrity:** both 17c/17d readback harnesses unchanged in spirit and still green — the probe shader extracts `ShadowData` + the `// ---- Shadow Sampling ----` section VERBATIM from gbuffer.wgsl, so the new params/kernel/bias flowed into the harness automatically (the design paying off). `shadow_factors` gained `(resolution, pcf_radius)` params; the sun spec was parameterized (`run_sun_harness`) and now runs at (1024, 0.5) + (2048, 0.5) + (1024, 3.0).

**Gates:** `cargo build -p app` green; **render 31/31** (was 29 — +2 harness variants; both original shadow harnesses + texel tests green at 1024); **graphics 43/43**; app --bins threads=1 green; golden untouched. Uncommitted on feat/sparse-3a.

**Owner manual test:** (1) PBR, cube + floor with the default sun: bump Shadow Map Resolution 1024² → 4096² — cast-shadow edges visibly crisper, no acne returns while orbiting, contact shadow stays attached (no peter-panning); profiler "Shadow" slot shows the cost. (2) Shadow Softness 0.5 → 3.0: penumbra widens live; back to 0.5: crisp again; no acne at any point of the slider. (3) MatCap mode: Rotation slider now rotates the matcap; Edge Lines toggle/threshold behave as labeled (previously corrupted by the dead packing). (4) Volume Shading section: the old Volume Shadows sliders are gone; SDF shadows are the sun cascades like everything else.
