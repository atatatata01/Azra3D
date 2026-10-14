# Group / Nest / Duplicate — Surface Bug Log

Living log of the "group / duplicate a primitive → surface vanishes / fragments / flattens to a wall /
log-floods" saga, so ANY session can recall what was tried without re-deriving from scratch.
Append newest findings; do not delete history.

---

## ===== ✅ 2026-07-14 — SPARSE group-into-sub-body: surface lost + [RetightenArm] idle loop =====

**Owner repro:** body at 1024³ (SparseBricks) with prims; outliner-DRAG one prim ONTO another →
drop-onto-prim gesture creates sub-body Group.001 containing both → the WHOLE body's surface
disappears (only selection wireframe), console spams
`[RetightenArm] body=NodeId(1) by=no_copy_completion while IDLE — settle-loop feeder`.

**What is NOT the bug (verified first):**
- `group_selected_into_body` (prim_actions.rs:541) does the FULL Structural Mutation Checklist
  in order (graph mutate → recalc origins → sync_flat_cache → mark_node_body_vol_dirty for both
  bodies + debounce clear). No missing step — the CR-71/74 class does not apply here.
- `hash_body_source` DOES see the group (child_order tag-1 Body ref + body_op, recursive —
  e7aae97), so the dedup can't serve the stale pre-group grid.
- The CACHE CORE handles the mutation correctly end-to-end: new e2e
  `sparse_group_into_subbody_end_to_end` (tests_sparse_bake.rs) replays the exact graph
  mutation against a brick-resident body on real GPU — hash changes, ONE sparse FULL re-bake
  dispatches (never dense), the re-mesh is non-empty and bit-stable, and the settle machinery
  is quiescent afterwards (new `settle_quiescent()` accessor + 3 idle-frame no-dispatch sweep).
- `dispatch_grid_dc_async` / raster draw / pick already handle the 0-own-prims body
  (`body_has_renderable_content`, the has_sub_bodies bail — 12858a8 era fixes intact).

**TWO app-layer roots (both triggered by the gesture making the top body's OWN prim list empty
and creating the scene's first NESTED body):**
1. **`update_lod_resolutions` used OWN prims only** (body_volume_cache/mod.rs ~995) — this was
   the "Build sites still use OWN prims (Codex)" OPEN item below. Grouped body → empty own
   list → `(ZERO,ONE)` degenerate fallback box → tiny screen fraction → adaptive LOD drops the
   1024 sparse body to ≤512 → dense re-bake destroys `brick_resident` (C3 retighten guard off)
   → each dense field-direct completion re-arms the settle (`note_cpu_grid_stale` — the exact
   spam line) while trigger_bake's own full-bake adaptive recompute (real recursive grid
   bounds, bake_dispatch.rs ~676) writes the high res back → **resolution ping-pong that never
   converges**; the surface is destroyed/degraded in the churn. FIX: LOD sizes bodies from
   `collect_body_prims_recursive` — the SAME collection the bake and the settle re-tighten use,
   so LOD and bake can never disagree again; an empty subtree gets NO tier (skip, never the
   guess box). Test: `lod_uses_recursive_prims_for_grouped_body`.
2. **`pick_warming_candidate` treated a NESTED sub-body as warmable** (warming.rs). A nested
   body never owns a grid (trigger_bake skips it — subsumed into the ancestor bake), so
   `has_cache` is false FOREVER → `poll_warming` ran `mark_body_dirty(sub)` + `clear_debounce()`
   + `trigger_body_vol_bake()` EVERY idle frame from the moment Group.001 existed — the idle
   pump that re-ran root 1's LOD pass each frame and held `retighten_idle()==true` (why every
   arm printed "while IDLE"). FIX: warming candidates must be live TOP-LEVEL SdfBodies —
   enforced INSIDE the picker for all three sources (outliner hover of a sub-body row, an MRU
   entry nested after being recorded, the scene scan). Same nested-scope rule as
   `mark_stale_bodies_dirty` / the bake loop. Test: `warming_never_picks_nested_sub_bodies`.

**Pattern for the family:** every consumer that enumerates "bodies" for per-body machinery
(LOD, warming, retighten, resident volume, draw, pick) must (a) resolve NESTED bodies to their
top-level ancestor or skip them, and (b) read the RECURSIVE prim set, never the own list.
Grep audit at fix time (`sg.sdf_bodies()` / `all_nodes()` consumers): roots 1+2 were the two
enumerators feeding the BAKE machinery. Still own-prims but BENIGN for this bug (and still
listed under OPEN below): `ensure_body_meshes` + `upgrade_mesh_lod` (raster_mesh_prep.rs) —
the analytical first-paint Preview path; they SKIP a grouped body (empty own list), and the
grid DC path owns its display mesh, so nothing renders wrong — a grouped body merely gets no
64³ preview before its first bake.

---

## ===== 🎯 ACTUAL FLAT-SLAB ROOT (2026-07-02) — atlas cooldown blocks a resolution GROW =====

**CRITICAL CORRECTION to everything below.** The probe verdict `→ match` was MISREAD as "correct". It only
means `rv.buffer == body_grid` — and BOTH derive from the SAME bake, so if the *bake* is wrong they agree
and still say "match". The provenance fixes below (25a791c/05d3f5a/6db7950/f430bec) fixed REAL GPU-vs-CPU
divergence bugs across a realloc — keep them — but they were **never the flat slab**.

**Ground truth that cracked it:** after a duplicate/group + settle, the SAME bounds bake at three res in
sequence: 128³→**7.0%** inside, 64³→**7.3%**, 128³→**0.9%**. All "match". `0.9% ≈ 7.3%/8 = 64³/128³` — the
fingerprint of a **64³ write into a 128³ buffer**. So the full-res settle bake writes only a 1/8 corner
sub-block → thin slab → the flat surface. The draft (64) is correct because everything agrees at 64³.

**Mechanism (4-agent trace `w4tkwayq8`, high-confidence):** the settle cascade oscillates 64→128 at fixed
bounds. The 64³ draft realloc'd the body's `VolumeAtlas` slot to 64³ and stamped `last_resolution_change`.
The next 128³ FULL bake requests a grow, but `VolumeAtlas::allocate` (graphics/volume_atlas.rs:517-527) hit
the **500ms anti-thrash cooldown and returned the STALE 64³ slot without reallocating**. `effective_res`
(app_poll.rs:1442) reads slot.resolution=64 → the shader dispatches + index-strides at 64³ while the output
buffer + stored grid are 128³ → 1/8 fill. The app-side `stale_atlas_slot` guard (app_poll.rs:1315) forces a
realloc only for the OPPOSITE case (slot LARGER than req — the down-switch ghost slab), so the GROW slipped
through the cooldown.

**Fix `3a97dd8`:** in `VolumeAtlas::allocate`, gate the cooldown early-return on NOT growing. A shrink/equal
may be deferred (thrash guard); a GROW must always free+realloc — returning a smaller slot under-writes the
larger buffer (correctness, not perf). Guarantees the slot is never smaller than the bake it backs. Grows
fire at most once per settle (the 64→128 promotion), never per frame; partial/draft bakes don't change res.
Added an `AZRA_PROBE`-gated `[BAKE-RES]` log flagging `effective_res < req.resolution`. **Pending owner verify:
settle-128 probe inside% should now match the 64 draft (~7%, not 0.9%); no `[BAKE-RES] UNDER-SIZED` line.**

---

## ===== ✅✅ (kept, but NOT the flat slab) provenance fix — buffer-instance leak across realloc =====

The earlier "FINAL" fixes (below) were correct but treated a *symptom*. A fresh run with the
`content_bounds_ok` gate active showed the smoking gun: **`content_bounds_ok=true` right after
`[Retighten]` yet `rv.buffer` = 0.0% empty**, fixed only by a *later* CPU `[VolumeUpload] full`. That is
impossible unless `last_upload_bounds` was stamped against a buffer that never received the content. A
read-only 4-agent workflow trace (`wox2di4we`) + the blocking probe (`device.poll(Wait)` before read,
proving the compute finished) pinned the exact mechanism:

**A buffer-INSTANCE provenance leak across a resident-volume REALLOCATION:**
- **Frame N** — the settle re-tighten does a direct `bake_into_resident`: the compute writes the CORRECT
  field into `rv.buffer` (the OLD buffer instance) and latches `gpu_texture_upload_done=true`
  (`app_poll.rs:~1804`). The dispatch side only permits this when NOT reallocated (`app_poll.rs:1363`).
- **Frame N+1** — `update_resident_volume_opt` hits `needs_alloc` (adaptive res/bounds recompute on the
  settle bake, or a pool recycle), so it **swaps `rv.buffer` for a fresh EMPTY / stale-recycled buffer**
  and sets `resident_volume_reallocated=true` (`resident_volume.rs:219`). The correct field lived in the
  OLD buffer; the NEW one is empty. The SAME call then reaches the `gpu_texture_upload_done` branch
  (`resident_volume.rs:255`), which **did not test `resident_volume_reallocated`** → it blindly set
  `last_upload_bounds=(grid_min,grid_max)` + `resident_buffer_valid=true`, **certifying the empty new
  buffer**. `content_bounds_ok` then reads true and field-direct meshes the empty buffer → 0% inside →
  flat slab / stretched cube. `force_atomic_upload` (TRUE on realloc) *would* have done the correct CPU
  Full re-upload, but the stale flag short-circuited the whole CPU path first.

**Root layer:** `gpu_texture_upload_done` is provenance for a SPECIFIC buffer instance but isn't bound to
it; a realloc between set (N) and consume (N+1) makes it a false claim about a different, empty buffer.
The consume site omitted the `resident_volume_reallocated` guard the DISPATCH site already enforces.

**Fix `f430bec`:** at the consume site, when `resident_volume_reallocated`, VOID the stale flag
(`gpu_texture_upload_done=false`, `_body=None`) and fall through to the CPU else-branch → `force_atomic_upload`
does the single-frame Full re-upload into the NEW buffer, stamping `last_upload_bounds` only after the write
lands. Steady state (no realloc) is untouched → G2 field-direct stays fast; the flag self-clears in
`viewport_render:1087` so the guard can't get stuck off. **Pending owner verify.**

---

## ===== ✅ FINAL ROOT + FIX (2026-07-02, AZRA_PROBE ground truth) — flat slab (symptom-level) =====

**The probe (`AZRA_PROBE=1`, readback of `rv.buffer` vs the CPU oracle grid) settled it.** After EVERY
`[Retighten]`, the resident buffer reads **`GPU-buf inside 0.0% vs CPU-grid inside 6.6%`** with ALL bound
labels agreeing (`rv.bake_bounds == dc_bounds == cpu_bounds`); the NEXT `[VolumeUpload] full` flips the same
config to a **match** (`7.0% vs 7.0%`). So:

- A settle re-tighten / escape **moves the bake bounds and dispatches a bake**, but `rv.buffer`'s CONTENT is
  not re-uploaded until a later frame. In the window between, `sdf_populate_resident` still takes the
  **field-direct** path and meshes `rv.buffer` — mapping stale/empty content under the NEW bounds → 0% inside
  → **the flat slab**.
- The field-direct gate trusted `bounds_ok` (= `rv.bake_bounds`, the LABEL, overwritten to the new bounds
  every frame at `resident_volume.rs:245`) and `buf_valid` (a bare bool). NEITHER checks the bounds the
  buffer CONTENT was actually written at.

**Fix (3 commits, all keep G2 on):**
1. `25a791c` — force a re-upload when the bounds shift (dedup was keyed on `(source_hash,res)` only).
2. `05d3f5a` — track `last_upload_bounds` = the bounds content was *truly* written at (moves only on a real
   write), and drive `force_atomic_upload` off it instead of the `rv.bake_bounds` label (immune to the
   line-245 premature-overwrite timing hole).
3. `6db7950` — **THE KILLER**: gate field-direct on `content_bounds_ok = (last_upload_bounds ≈ mesh bounds)`.
   When the buffer content wasn't written at the bounds we're about to mesh, fall through to the CPU oracle
   grid (`body_grid`, proven present at `app_poll.rs:931`, probe-confirmed correct that frame). Field-direct
   re-engages for free the frame the re-upload lands — both bounds come from the SAME `body_grids` entry, so
   they're bit-identical (no drift → G2 stays fast in steady state). **Fail-safe:** a false `content_bounds_ok`
   only costs one CPU-grid mesh (correct, slower); there is no false-positive path.

New trace: `[GpuMeshPath] ... content_bounds_ok={bool}` shows the gate flipping to `false` (→ CPU fallback)
during a re-tighten and back to `true` once the buffer catches up. **Pending owner verify on a CLEAN,
NOT-running rebuild (the last build failed to relink — app.exe was still locked = stale-binary confound).**

---

## ===== ⭐ DECISIVE LOCALIZATION (2026-07-02, owner-confirmed on a CLEAN rebuild) =====

Owner did `cargo clean -p app -p app_types && cargo run -p app` (confirmed-fresh binary, all fixes incl.
`aca0238`). Two definitive results:

1. **The escape-cap fix (`aca0238`) WORKS.** Log now shows the grid capped at ~1.34× the geometry
   (`grid_max=(1.99,2.18,0.68)` for ~2.3-unit geometry) instead of ballooning to ~8 units. The BALLOON
   is fixed. So the remaining flat/disappear is NOT the grid size.

2. **The bug is the "Resident GPU mesh (G2, indirect draw)" path.** Toggling that OFF in Render settings
   makes the flat plane render CORRECTLY. So:
   - **G2 ON** = GPU bake (`volume_bake.wgsl`) → GPU DC mesh (`sdf_mesh.wgsl`) → `draw_indexed_indirect`
     from the resident slot = **BROKEN** (flat slab + cubes disappear on move).
   - **G2 OFF** = CPU bake (`bake_utils::bake_body_grid`) → CPU chunked-DC → `mesh_cache` → normal raster
     = **CORRECT** (this is the path the CPU `diagnose_scene_bake` measured healthy, 6.7% inside).

   → The field/data is correct; the **GPU DC mesh path** meshes it wrong for off-center/spread/non-cube
   bounds (works for a single centred box → cube-ish bounds). Prime suspects in `graphics/src/sdf_mesh.wgsl`:
   `fld()` assumes a CUBIC res³ buffer (line ~82-85: bounds check `x>=r` + index `x+y*res+z*res*res` all use
   the scalar `res()`), while `cell_world`/`world_to_grid` map per-axis (`gext`). If the GPU-baked buffer is
   ever non-cubic (asymmetric res leaking past the cubic-force at bake_dispatch.rs ~524, or a res/`cbrt`
   mismatch), the cubic index reads wrong slices → slab + fold. Compare the GPU-baked rv.buffer contents vs
   the CPU `bake_body_grid` for the SAME scene/bounds to find the divergence (a GPU readback of rv.buffer +
   diff against the CPU grid is the definitive next probe — needs a GPU run).

   **WORKAROUND (owner unblocked NOW):** keep "Resident GPU mesh (G2)" OFF → correct rendering (slower,
   CPU-meshed). The G2 default is `gpu_sdf_mesh_resident` (search Developer/Render toggles).

   **NEW CLUE (2026-07-02): rapidly changing the cache-resolution picker (64³/128³/256³) ALSO triggers
   the flat plane — RANDOMLY, not every time.** This is a RACE / STALE-GENERATION signature: the resident
   buffer (rv.buffer) is re-sized/re-baked at the new resolution while the GPU DC dispatch reads it with a
   DIFFERENT resolution (or the bounds it was baked over ≠ the bounds dc_params maps it with). Because
   `sdf_mesh.wgsl::fld()` indexes `field[x + y*res + z*res*res]` with a SCALAR `res()`, a res/bounds
   mismatch makes the cubic index read the WRONG slices → slab/fold. "Random" = it only corrupts when the
   mesh dispatch catches the buffer mid-resize (a timing window). Off-center/spread geometry hits the SAME
   mismatch via aabb_escape/retighten changing the bounds. → This is a GENERATION-COUNTER problem
   (CLAUDE.md: "generation counters over timing tricks"): the GPU DC must NEVER mesh a resident buffer whose
   (res, bounds) don't match its dispatch. `res_ok`/`bounds_ok` gates exist (app_poll.rs ~750) but evidently
   pass in a window where the buffer CONTENT is still at the old res/bounds. Fix must bind buffer content to
   a generation the DC verifies, not just compare cached scalars.

   **REDO implication for the GPU-vs-CPU question:** the GPU path is the BROKEN one; the CPU path is
   correct. "Use GPU for speed" is not free — the GPU DC mesher has this bug. Either fix `sdf_mesh.wgsl`
   for non-cube bounds, or (simplest) default G2 OFF until it's fixed, or (best long-term) per-object tight
   cube grids so the GPU path only ever meshes the cube-ish bounds it already handles.

---

## ===== CODEX REVIEW (2026-07-02) — converges with the trace; the ROOT =====
Independent Codex review pinned the same mechanism:
- `sdf_mesh.wgsl:81` indexes density `field[x + y*res + z*res*res]` with a SCALAR cubic `res`;
  `sdf_gpu_mesh.rs:25` sends only scalar res — but `bake_dispatch.rs:517` computes per-axis `res3=[rx,ry,rz]`.
  If any asymmetric grid reaches G2, indexing is WRONG (Codex Critical #2).
- `bake_readback.rs:141` infers scalar res via `cbrt(grid.len())`; `resident_volume.rs:119` stores
  `[bake_resolution;3]` — the resident metadata CANNOT honestly represent a non-cube grid (Codex High #3).
  → `cbrt` of a non-cube/len silently yields a WRONG res → cubic index reads wrong slices → flat slab.
- `raster_render.rs:450` draws the resident slot if it exists + G2 on, WITHOUT verifying it matches the
  current body hash/bounds/res/GENERATION → a stale/wrong-res slot overrides the correct CPU mesh (Codex
  High #4). This is the race the owner's rapid-resolution-change test triggers.
- `app_poll.rs:857` async DC `source_hash` uses only the body's OWN prims — not recursive for grouped/
  nested bodies, inconsistent with the rest of the cache (Codex High #5).
- Recursive bounds/serialization (`bake_utils.rs:54`, `serialize.rs:327`) now look correct — STOP chasing
  those as the cause (Codex Medium).

**SYNTHESIZED ROOT:** the G2 resident path assumes a cubic `res³` buffer and identifies the resident slot
by scalar res (via `cbrt`) with no generation check. On a resolution change (rapid picker) or a non-cube
aspect (spread geometry), the slot's true (res,bounds) and the DC's dispatch disagree → cubic index reads
garbage → flat slab. FIX (keep G2, make it correct): (1) generation-gate the resident slot — DC/draw
verify (res,bounds,hash,gen) match before meshing/drawing, else fall back to the correct CPU mesh; (2)
carry the true grid shape (or provably force cubic before G2) — kill the `cbrt(len)` inference; (3)
recursive source_hash at app_poll.rs:857. Do NOT default G2 off as the fix — fix it so default-ON is correct.

---

## ===== HANDOFF SUMMARY (2026-07-02) — READ THIS FIRST (for Codex / fresh eyes) =====

**App:** AZRA — Rust + wgpu SDF/voxel/poly modeler in `core_engine/`. Branch `fix/outliner-scene-graph-issues`.
**Repro:** new scene → 1 box → duplicate a few times → move/group them → Object Mode (Solid).
**Two bugs that PERSIST:** (1) geometry renders as a FLAT SLAB / thin plane; (2) cubes DISAPPEAR while you
move one, and reappear when you move it back near the others. Owner has spent days on this.

**What is DEFINITELY FIXED (owner-confirmed / CPU-dump-proven):**
- Origin CHURN: `recalculate_body_origin` no longer chases the centroid on every edit (BodyPivotMode
  default Centroid→Custom, `701666e`). Dumps confirm all body origins stay `(0,0,0)`.
- Object-Mode VANISH-on-group: raster draw + pick skipped a top body with 0 OWN prims — fixed with the
  canonical `SceneGraph::body_has_renderable_content` (`4689463`/`12858a8`).
- Sub-body dirty FLOOD (`no_grid_in_mark_prim_dirty_aabb` × hundreds): `write_prim_transform` now routes
  a sub-body prim edit through `mark_node_body_vol_dirty` (climbs to top ancestor) (`701666e`).
- Sub-body ORIGIN COMPOSITION into the top bake frame (`dd9b7a6`).

**KEY MEASUREMENT (CPU, no GPU — `project.rs::diagnose_scene_bake`, #[ignore]):** the scene DATA is fine.
Baking the field at TIGHT bounds → **6.7% voxels inside = healthy boxes**. Baking at BALLOONED bounds
(2.5×) → **0.4% inside = degenerate slab**. So the geometry is correct; a too-large grid under-samples it
into a slab.

**LEADING ROOT (measured):** the bake grid BALLOONS. `aabb_escape` (bake_dispatch.rs) grows the grid as
prims move/duplicate (owner log: grid_max ran to ~8 units for ~2-unit geometry) and it did not shrink back
(the settle re-tighten compared against a STALE inflated reference). A grid 2×+ the geometry under-samples
at fixed 128³ → flat slab / detached blobs / disappear. The old centroid-churn used to keep grids
~cube-shaped + centred, which HID this; removing the churn (correct) exposed it.

**Fixes aimed at the balloon (may NOT all have been TESTED yet — see confound):**
- `893a6a8` — sync `rv.bake_bounds` in `accept_gpu_result` (full-readback path) so the DC maps the buffer
  with the bounds it was baked over (kills off-center squish on the non-field_direct path).
- `eff318f` — `check_idle_retighten` recomputes FRESH tight bounds from the live scene (was stale
  `partial_tight_bounds`) → a ballooned grid is now correctly seen as loose and re-tightens on settle.
- `aca0238` — tighten `aabb_escape` caps: `MAX_GRID_MULT` 2.0→1.35, `ESCAPE_HEADROOM` 0.5→0.15, so the
  grid can't balloon past ~1.35× the geometry DURING a drag (holds sampling ~3% inside = healthy).

**MAJOR CONFOUND — stale binary (bit us 3×):** a RUNNING `app.exe` locks the binary on Windows, so
`cargo build` fails to relink (`error: failed to remove ... app.exe`) and silently keeps the OLD binary.
`aca0238`'s build FAILED to link this way → the escape-cap fix is COMMITTED but likely NEVER RAN in the
owner's tests. ALWAYS: quit AZRA fully → `cargo run -p app` → confirm it recompiled → verify in the log
the grid no longer balloons past ~1.35× (grid_max stays near the geometry, not 5–8 units).

**OPEN QUESTION / next hypotheses if a CONFIRMED-fresh binary still fails:**
- Is the GPU bake (`graphics/src/volume_bake.wgsl`) producing a slab even at TIGHT bounds? (The CPU
  `bake_body_grid` diagnostic proves TIGHT is healthy, but CPU≠GPU — they're separate implementations.)
- Does the field-direct DC (`graphics/src/sdf_mesh.wgsl`, `fld()` assumes a CUBIC res³ buffer at line 82-85;
  `cell_world` is per-axis) mishandle a non-cubic grid? Consider forcing the bake BOUNDS cubic (centred on
  geometry) to restore the demonstrably-working centred-cubic condition.
- The `partial_tight_bounds` Z pad: TIGHT bounds show Z=(-1,1) for z±0.4 boxes — a generous/min pad may be
  part of why "not too loose" mis-fired.

**STRATEGIC FORK (owner-agreed):** if a confirmed-fresh binary with `eff318f`+`aca0238` STILL shows the 2
bugs, STOP patching. The single fixed-resolution SDF grid is the wrong tool for spread-out geometry; scope
a clean rebuild of JUST the grid-sizing / bake-orchestration path (not the whole app).

**KEY FILES for the render path:** `app/src/body_volume_cache/bake_dispatch.rs` (grid sizing, aabb_escape
~393, cubic-res force ~524, check_idle_retighten ~94), `bake_readback.rs` (accept_gpu_result ~110),
`app/src/app_poll.rs` (sdf_populate_resident ~719, dispatch_grid_dc_async ~831, field_direct + bounds sync
~1955-2015, [GpuMeshPath]/[VolumeUpload]), `graphics/src/volume_bake.wgsl` (GPU bake), `graphics/src/sdf_mesh.wgsl`
(GPU DC: fld ~81, cell_world ~96), `app/src/sdf_gpu_mesh.rs` (dc_params ~27, pool/draw), `app/src/raster_render.rs`
(resident draw ~55-93). Diagnostics: `app/src/project.rs::dump_scene_structure` + `::diagnose_scene_bake`
(both #[ignore], driven by env `AZRA_DUMP_SCENE=<path.fl4>`).

---

## ⚠️ TESTING PROTOCOL — READ FIRST (this bit us TWICE)
- **CLOSE the running AZRA app BEFORE rebuilding.** A running `app.exe` **locks the binary** on Windows,
  so `cargo build`/`cargo run` CANNOT replace it — the rebuild silently keeps the OLD code and the app
  keeps running the stale binary. On 2026-07-01 the owner reported "same problem" across MULTIPLE cycles
  while `app.exe` (PID 33372) stayed open since 21:20 — none of commits `701666e`/`12858a8`/`4689463`
  ever ran. Correct sequence: **quit AZRA → `cd core_engine && cargo run -p app` → wait for recompile → test.**
- **Verify the binary is fresh:** `git log -1 --format=%ci` vs `ls -l --time-style=full-iso
  core_engine/target/debug/app.exe`. If the exe mtime is OLDER than the latest commit, the fix is NOT
  running. Also check no stale `app.exe` in `tasklist`.
- **Repro A (live):** new scene → one box → Duplicate → Duplicate → select 2 → **Group** → then
  Duplicate a prim **inside** the group. Switch to Object selection mode.
- **Repro B (saved):** load `C:\Users\Proprio\Desktop\azra scene\cache bug duoplicate group promotove.fl4`.
- **CPU inspection (no GPU):** `AZRA_DUMP_SCENE="<path.fl4>" cargo test -p app dump_scene_structure -- --ignored --nocapture`
  (dumps bodies, sub-bodies, prim ops, origins, computed bake bounds).

---

## PARTIALLY RESOLVED 2026-07-01 (after a CLEAN rebuild — `cargo clean -p app -p app_types`)
Once the fixes actually RAN (they'd never built — running app locked the exe + timestamp misread):
**CONFIRMED FIXED** — vanish GONE (`12858a8`), recalc-origin CHURN gone (`701666e` — owner's log now
has ZERO `recalc_origin` lines vs one-per-action before), flood GONE (`701666e`), sub-body origin
composition correct (`dd9b7a6`).
**Lesson: much of the 2-day loop was a build/deploy problem** (stale binary, locked exe, wrong-worktree,
timestamp misread) — ALWAYS confirm a fresh binary (`cargo clean` + mtime check) before concluding a fix
failed. (Premature "fully resolved" was retracted — owner still saw flatten.)

**DUMP OF `flat.fl4` (2026-07-01, the definitive diagnosis):** 3 identical 0.8 cubes, ALL body origins
(0,0,0) — **churn fix CONFIRMED working**. Box.001@(0,0,0) in Body.001; Box.003@(0.68,0.70) + Box.005@
(1.44,1.54) in Group.001 (sub, origin 0). Bake bounds = (-0.68,-0.69,-0.5)→(2.12,2.24,0.5) = extent
**2.8 × 2.9 × 1.0, NON-CUBIC + OFF-CENTER** (geometry runs diagonally to +X+Y, away from origin 0). Data
is HEALTHY (no stray prim, bounds cover geometry). BUT owner confirms it renders **SQUISHED FLAT +
REPEATED ~5×** across a panel (field-direct GPU DC mesh, `draw_indexed_indirect`).

**→ REAL REMAINING BUG (NEW, exposed BY the churn fix):** the resident field-direct render breaks for
OFF-CENTER / NON-CUBIC bounds. The OLD centroid-chasing origin kept bounds ~centered + ~cube-shaped,
which HID a cubic/center assumption in the bake→mesh→draw chain. `dc_params` (sdf_gpu_mesh.rs:27) uses
per-axis `gext` (looks correct), so the assumption is elsewhere (bake voxel→world mapping? sparse/dense
dims? multi_draw slot count → the ~5× tile? instance transform → squish?). Workflow `wf_4b20061f-424`
tracing bake / DC-mesh / resident-draw to pin it. Candidate fixes: (A) make bake+mesh mapping
consistently per-axis+gmin-based; (B) force bake BOUNDS cubic (expand short axes) so cubic assumptions
hold (voxel-waste tradeoff).

**EARLIER (now understood as part of the above) — the OTHER balloon source (NOT recalc):** `aabb_escape` grows the grid on each
duplicate, then `[Retighten] settle → "already tight, skipped"` declines to refit it. As duplicated
boxes spread across the panel, the grid stops covering the outer ones → they CLIP flat against the grid
boundary → "flatten-to-wall". Different root from the churn; never addressed. Suspects: (a)
`MAX_GRID_MULT=2.0` escape cap (`bake_dispatch.rs:~411`) too small for spread-out duplicates; (b) the
"already tight" retighten check skips when the grid actually clips the geometry. NEXT: dump the SAVED
flattened `.fl4` (`AZRA_DUMP_SCENE=... cargo test -p app dump_scene_structure -- --ignored --nocapture`)
to confirm grid-clips-geometry vs a stray oversized prim, then fix the bounds/retighten logic. Also note
outliner count mismatch (Body.001 (0) + Group.001 (2) but ~8 boxes render) — verify in the dump.
Remaining polish after: rebake flicker (debounce), green-dot marker, build-site OWN-prims, TDR.

## Symptom evolution (what the owner saw, in order)
1. Object-Mode surface **VANISHES** after grouping (all prims moved into a sub-body).
2. After the vanish fix: surface **FRAGMENTS / flattens to a wall** (grid ballooning).
3. Duplicate **inside** a sub-body → no surface + hundreds of `[DirtyFallback] no_grid_in_mark_prim_dirty_aabb`.
4. **"Everything disappears"** on `[Retighten] settle → full re-tighten bake`.

---

## Root causes + fixes (commit → what → status)
- **`dd9b7a6`** — Sub-body origins NOT composed into the top bake frame → nested geometry baked/bounded/
  culled in the wrong frame. FIX: `collect_body_prims_recursive` (bounds) + `serialize_body_for_bake_hot`
  (content) + scope AABB all compose `shift = this_origin - top_origin`. Dump-verified (bounds 1.12→1.98).
  Correct for SAVED scenes with baked-in origins. **VERIFIED (CPU).**
- **`4689463` → refactored `12858a8`** — VANISH root: raster **DRAW** (`raster_render.rs:387`) + **PICK**
  (`:950`) skipped a top-level body with EMPTY own prims (all grouped into sub-bodies), even though its
  geometry lives in the subtree bake. Same bug class as the already-fixed `dispatch_grid_dc_async`. FIX:
  one canonical `SceneGraph::body_has_renderable_content(nid)` (own prims OR child sub-bodies) routes
  draw + pick; regression test `body_has_renderable_content_covers_grouped_bodies`. **CONFIRMED by owner**
  (body now draws — screenshot shows the wall, no longer blank).
- **`701666e`** — CHURN + FLOOD roots (⚠️ NEEDS REBUILD + owner retest — was tested on a stale binary):
  - **Churn:** `recalculate_body_origin` re-centered each body origin to its geometry centroid on EVERY
    structural edit and shifted all prims to compensate → **ballooned the bake grid** (grid_max grew to
    3-6 units) → under-sampled boxes = fragment/flatten; flipped origin (0→0.41→0); churned resident vol.
    FIX: `BodyPivotMode` default `Centroid` → `Custom` (`app_types/src/types_gizmo.rs:25`). `recalc` already
    early-returns for `Custom` (`prim_actions.rs:624`), so origins now stay FIXED; re-centering is opt-in.
    Matches owner directive "all prims same state" + standard DCC (Blender never auto-chases geometry).
  - **Flood:** a prim edit INSIDE a nested sub-body called `mark_prim_dirty_aabb` on the grid-less
    sub-body → floods `no_grid_...` every frame AND the top body never re-baked (nested bodies are
    subsumed / skipped by `bake_dispatch`). FIX: `write_prim_transform` (`prim_actions.rs:~347`) routes a
    sub-body prim edit through `mark_node_body_vol_dirty` (climbs to top ancestor). Top-level bodies keep
    the tight partial path.

---

## OPEN / SUSPECTED (not yet fixed)
- **Resident-volume bounds staleness** (Codex + workflow trace #1 — STRONG suspect for "disappears on
  re-tighten"): a re-bake with NEW bounds (re-tighten / aabb_escape) only syncs `ResidentVolume.bake_bounds`
  inside the `pending.field_direct` branch (`app_poll.rs:1997-2008`). A normal full re-tighten changes
  bounds but leaves `rv.bake_bounds` stale → `bounds_ok=false` in `sdf_populate_resident` (`app_poll.rs:750-755`)
  → field-direct gate fails → CPU-grid fallback → blank. `translate_body_cache` (`mod.rs:882`) shifts
  `body_grids`/`body_bounds` but NOT `rv.bake_bounds`. → FIX DIRECTION: sync `rv.bake_bounds` on ANY
  accepted bake whose bounds changed, not just field_direct.
- **aabb_escape ballooning** independent of recalc: duplicate offsets spread prims → grid grows (bounded
  2x, `bake_dispatch.rs:393` region). May still under-sample. Re-check after 701666e is built.
- **Build sites still use OWN prims** (Codex): `ensure_body_meshes` (`raster_render.rs:135`), LOD upgrade
  (`:311`), `update_lod_resolutions` (`mod.rs:670`) → non-resident fallback mesh + adaptive resolution
  wrong for grouped bodies (0 own prims). Need `collect_body_prims_recursive`, not a guard swap.
- **Green dot** (owner): the outliner's per-prim colored marker (Box.001 green, Box.003 red). It's a UI
  base-prim indicator — cosmetic, SEPARATE from the bake. Owner wants uniform dots (no special "base").
- **~4-6 s TDR frame** on group (`[GPU] WARNING: Previous frame Nms — deferring bake`): synchronous
  group-frame work (recalc + sync_flat_cache + first-bakes + full serialize on one frame). Amortize.

---

## Architecture note (owner-aligned — the real root)
Most symptoms share ONE root: **per-body-origin auto-recalculation** (`recalculate_body_origin`). The clean
model the owner wants ("all primitives, same state; no special base"): origins **FIXED**, sub-bodies are
pure boolean groups with NO independent frame, every prim in ONE coordinate space. `701666e` moves toward
this (Custom default). **Do NOT reintroduce centroid-chasing on structural edits.** Owner explicitly
considered "delete everything and redo" — pushed back: keep the working engine, delete the ONE broken
mechanism (the churn), which is a *subtraction*, not a rewrite.

---

## Key files / symbols
- `app/src/raster_render.rs` — draw loop `:387`, pick loop `:950`, `ensure_body_meshes :135`, LOD upgrade `:311`
- `app/src/prim_actions.rs` — `write_prim_transform ~330`, `recalculate_body_origin :623`,
  `group_selected_into_body ~540`, duplicate `~760`
- `app/src/bake_utils.rs` — `collect_body_prims_recursive :55`, `compute_body_bake_bounds`
- `graphics/src/serialize.rs` — `serialize_body_for_bake_hot :215`, `serialize_body_for_bake_hot_recursive`,
  `compute_body_aabb`
- `app/src/body_volume_cache/` — `bake_dispatch.rs` (nested-body skip `:233`, aabb_escape), `dirty.rs`
  (`mark_prim_dirty_aabb :125`), `cascade.rs`, `resident_volume.rs`, `mod.rs` (`translate_body_cache :882`,
  `update_lod_resolutions :655`)
- `app/src/app_poll.rs` — `sdf_populate_resident ~719`, `dispatch_grid_dc_async ~831`, field_direct bounds
  sync `:1997`, `[GpuMeshPath]` gate `:750`, `[VolumeUpload]`
- `app/src/dirty.rs` — `mark_node_body_vol_dirty :181` (CLIMBS to top ancestor — the correct redirect)
- `app_types/src/types_gizmo.rs` — `BodyPivotMode :25`
- `core_math/src/scene_graph.rs` — `body_has_renderable_content`, `has_child_bodies`, `child_bodies :1170`

## Facts established (don't re-litigate)
- Analytical SDF path (GLSL BVH / WGSL raymarch) is PERMANENTLY dead — never investigate it here.
- Object Mode: `force_volume_only = self.sdf_edit_active` → FALSE in Object Mode (raster from resident slot).
- The vanish was NOT the origin math (dd9b7a6 is correct — no double-shift; workflow trace #5 + Codex agree).
- Pre-existing unrelated: `pathtrace_primary_full.comp` has `FLOATS_PER_PRIM 288u` (should be 292u) — dead path.
