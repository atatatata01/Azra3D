# Remesh live-incr — measured numbers

Worktree: `F:\PROJECT\FourthLAyerAi\.claude\worktrees\remesh-live-incr`  
Branch: `feat/remesh-live-incr`  
Machine: same Windows box, `--release`, median-of-5 where the harness says so.

## Before (current remesh, no regional S3)

| res | fixture | drag frame | S1 seal | S2 extract | S3 project* | S4 from_mesh |
|-----|---------|------------|---------|------------|-------------|--------------|
| 128 | crust   | 7.1 ms     | 0.9     | 3.1        | 0.5         | 2.2          |
| 128 | solid   | 17.4 ms    | 1.0     | 5.9        | 5.3         | 4.6          |
| 256 | crust   | 22.5 ms    | 3.5     | 11.5       | 1.2         | 7.8          |
| 256 | solid   | **55.5 ms**| 3.8     | 23.5       | 11.9        | 17.0         |

GATE0 solid drag: 128 = 17.8 ms, 256 = 56.5 ms. Incremental 6/6 on solids.

\* S3 column is the **uncached full project** the stage harness still times separately. The production frame is the `drag frame` column.

## What landed

1. **Clip source triangles** to the dirty AABB before the seeded band sample (`source_tris_overlapping`). A dense cage no longer triangulates every face each grab.
2. **Regional S3** when extract vertex count is stable: re-project only remesh verts inside the dirty box; copy last projected positions elsewhere. Bit-identical (gated by the second nudge in `incremental_interior_nudge_is_bit_identical`).
3. Fast drags (~0.75 cell/frame) flip a cell every frame, so vertex numbering changes and S3 falls back to a full project — **no regression vs before**. A spatial-hash stitch was measured and rejected (solid @256 went 56 → 96 ms).

Live contract: every drag frame still remeshes and returns a new `HalfEdgeMesh`. The surface moves with the cage. Nothing is deferred to mouse-up.

## After (same machine, same harness)

Fast drag (~0.75 cell/frame — topology flips every frame, full S3):

| res | fixture | drag frame | vs before |
|-----|---------|------------|-----------|
| 128 | crust   | 6.9 ms     | −0.2 (noise) |
| 128 | solid   | 17.3 ms    | −0.1 |
| 128 | dense   | 18.7 ms    | (new fixture) |
| 256 | crust   | 23.2 ms    | +0.7 (noise) |
| 256 | solid   | **55.2 ms**| −0.3 |
| 256 | dense   | 56.2 ms    | (new fixture) |

GATE0 solid drag after: 128 = 14.9 ms (−2.9), 256 = 57.0 ms (+0.5). Incremental still 6/6.

Slow drag (0.1 cell/frame — regional S3 can fire):

| res | fixture | drag frame |
|-----|---------|------------|
| 128 | solid   | 13.9 ms    |
| 128 | dense   | 14.8 ms    |
| 256 | solid   | **51.3 ms**|
| 256 | dense   | **47.5 ms**|

Dense @256 slow is 47.5 vs 56.2 fast: **−8.7 ms** on the same cage when the splice engages. Fast grab still remeshes every frame (live); it just cannot skip S3 while cells are flipping.

## How to test

```
F:\PROJECT\FourthLAyerAi\.claude\worktrees\remesh-live-incr\Run_RemeshLive.bat
```

Add Voxel Remesh to a poly body, grab a cage vertex. The remeshed surface must update live. Resolution 128 scrubs live; 256 applies on slider release, but a **vertex drag stays live at any resolution**.

## A / B / C (unmerged — `feat/remesh-live-incr` only)

Do **not** merge to master until the owner has tested this build.

| lever | what |
|-------|------|
| **A** | Project cache keyed by cz slab (stable cell key), not compact vertex index. Fast-grab cell flips remumber the extract; reused slabs keep last projected positions. |
| **B** | `from_mesh_draft` during the drag (no TwinLinker, no `recompute_normals`). Episode commit re-evals with `from_mesh` on the retained grid so the stored mesh is a manifold. Surface does not pop (positions match). |
| **C** | `Qef` + 2³ collapse (`surface_nets_octree`). Thresh 0 = bit-identical to uniform QEF. Parent-first QEF is **faster** than uniform S2+S4 (24.5 → 15.3 ms @256) — not wired; remesh cache is still uniform. |

### Before A/B/C (regional S3 already on this branch)

| res | fixture | drag frame | GATE0 drag |
|-----|---------|------------|------------|
| 128 | solid   | 17.3 ms    | 14.9 ms    |
| 256 | solid   | **55.2 ms**| 57.0 ms    |

S4 `from_mesh` @256 solid was **16.9 ms**. Incremental 6/6.

### After A/B/C (same machine, same harness)

| res | fixture | drag frame | S4 from_mesh | S4 draft | GATE0 drag |
|-----|---------|------------|--------------|----------|------------|
| 128 | crust   | 5.3 ms     | 2.3          | 1.0      | 5.5 ms     |
| 128 | solid   | **9.9 ms** | 4.6          | 1.9      | 11.4 ms    |
| 128 | dense   | 10.1 ms    | 4.6          | 1.9      | —          |
| 256 | crust   | 17.7 ms    | 7.6          | 3.7      | 18.8 ms    |
| 256 | solid   | **36.1 ms**| 16.7         | **8.0**  | **40.0 ms**|
| 256 | dense   | 33.5 ms    | 16.9         | 8.1      | —          |

Solid @256 drag: **55.2 → 36.1 ms (−35%)**. GATE0 solid @256: **57.0 → 40.0 ms**. Incremental still 6/6. Live contract unchanged: every drag frame remeshes; nothing is deferred to mouse-up (only TwinLinker is deferred to release).

## C — octree extract (isolated S2, 2026-08-13)

Remesh drag **before wiring C** (current live path, `d99ca20`):

| res | fixture | drag frame | S2 extract | S4 from_mesh | S4 draft | GATE0 |
|-----|---------|------------|------------|--------------|----------|-------|
| 128 | solid   | 8.2 ms     | 4.4        | 3.4          | 1.3      | 9.4   |
| 256 | solid   | **28.6 ms**| **18.5**   | 12.3         | 5.7      | 31.6  |

Isolated extract (`measure_octree_extract`, sphere, no remesh wiring):

| res | path | time | verts/quads |
|-----|------|------|-------------|
| 256 | uniform QEF | **13.1 ms** | 90k / 90k |
| 256 | octree eps=0 | 13.3 ms | same (wrapper) |
| 256 | octree collapse | **150 ms** | **22.5k / 22.5k (0.25×)** |

**Verdict:** collapse is the right *mesh* direction (4× fewer quads → S4 would drop). This extract is the wrong *impl* (serial HashMap, 11× slower). Do **not** wire it into remesh. Next C slice: same 2³ collapse on the existing parallel cz-slab extract.

## C — slab-parallel collapse (2026-08-13, not wired)

Same machine, `--release`, median-of-5. Isolated sphere extract (`measure_octree_extract`). Remesh modifier still calls uniform `surface_nets_placed(Qef)`.

### Before (HashMap extract, this session re-run)

| res | path | S2 | verts/quads | S4 from_mesh |
|-----|------|----|-------------|--------------|
| 256 | uniform QEF | **14.2 ms** | 90k / 90k | — |
| 256 | HashMap collapse | **150–161 ms** | 22.5k / 22.5k (0.25×) | — |

Live remesh (unwired, `d99ca20` / `23a142c`): solid@256 drag **28.6 ms**, S2 **18.5**, S4 12.3 / draft 5.7, GATE0 31.6.

### After (parallel cz-slabs + chunk-sparse `cell_vert` + compact QEF)

| res | path | S2 | verts/quads | S4 from_mesh / draft | S2+S4 |
|-----|------|----|-------------|----------------------|-------|
| 128 | uniform | 3.6 | 21.7k / 21.7k | 2.6 / 1.1 | **6.2** |
| 128 | slab collapse | 5.8 | 5.4k / 4.1k (0.19×) | 0.6 / 0.2 | **6.4** |
| 256 | uniform | **13.3** | 90k / 90k | 9.5 / 4.7 | **22.8** |
| 256 | slab collapse | **26.2** | 22.5k / 17.3k (0.19×) | **2.3 / 0.8** | **28.5** |

`octree_zero_threshold_matches_uniform_qef`, `octree_collapse_is_manifold_and_coarser`, `octree_collapse_validates_at_128` pass.

### Verdict

| question | answer |
|----------|--------|
| Right *impl* vs HashMap? | **Yes.** 150 → 26 ms (6×). |
| Right *remesh* direction? | **No, not yet.** S2+S4 22.8 → 28.5 ms. S4 win (9.5 → 2.3) does not cover the extra extract (13 → 26). |
| Wire into remesh? | **No.** Incremental cache + `incremental_interior_nudge` stay on uniform QEF. |

Collapse still pays a full fine Phase A, then merges. The slice that can flip S2+S4 under uniform: classify the 2³ block first and QEF **only** the parent (or leftover leaves) — that cuts Phase A ~4× and keeps the S4 cut.

## C — parent-first QEF (2026-08-13, not wired)

Same machine, `--release`, median-of-5. Isolated sphere. Remesh still uniform QEF.

### Before (slab collapse, full fine Phase A — previous slice)

| path @256 | S2 | S4 | S2+S4 |
|-----------|----|----|-------|
| uniform | 13.3 | 9.5 | **22.8** |
| slab collapse (QEFs every leaf, then merge) | **26.2** | 2.3 | **28.5** |

### After (classify 2³ first; QEF parent or leftover leaves only)

| res | path | S2 | verts/quads | S4 from_mesh / draft | S2+S4 |
|-----|------|----|-------------|----------------------|-------|
| 128 | uniform | 3.6 | 21.7k / 21.7k | 2.7 / 1.2 | **6.2** |
| 128 | parent-first eps=1e-4 | 3.2 | 5.4k / 4.1k (0.19×) | 0.8 / 0.1 | **4.0** |
| 256 | uniform | **15.0** | 90k / 90k | 9.5 / 4.6 | **24.5** |
| 256 | parent-first eps=1e-4 | **13.1** | 22.5k / 17.3k (0.19×) | **2.2 / 0.8** | **15.3** |

`octree_zero_threshold_matches_uniform_qef`, `octree_collapse_is_manifold_and_coarser`, `octree_collapse_validates_at_128` pass.

### Verdict

| question | answer |
|----------|--------|
| Faster than last collapse? | **Yes.** S2 26.2 → 13.1; S2+S4 28.5 → 15.3. |
| Faster than uniform remesh extract? | **Yes.** S2+S4 **24.5 → 15.3 (−38%)**. S2 itself 15.0 → 13.1. |
| Wire into remesh? | **Not yet.** Incremental cz-slab cache and `incremental_interior_nudge` assume uniform vertex numbering. Collapse also changes the displayed mesh (0.19× quads). Next: wire behind a remesh flag and update the cache. |

## C — wired behind `remesh_collapse` (2026-08-13, not on master)

`PolyModifier.remesh_collapse` (new remesh defaults **on**; load of old files stays **off**). Incremental SDF resample still runs. Extract cache is skipped while collapse is on (vertex numbering is not cz-slab stable). Persist via `FL4PRMCL`, not by widening `FL4PRMSH`.

`incremental_interior_nudge_is_bit_identical` still passes (that path leaves collapse off).

### Remesh drag frame (`measure_drag_frame_stages`, this session)

| res | path | drag frame | S2 | S4 from_mesh / draft | S2+S4 | quads |
|-----|------|------------|----|----------------------|-------|-------|
| 128 | solid uniform | 8.4 | 4.6 | 3.4 / 1.5 | 8.0 | 27.8k |
| 128 | solid collapse | **5.2** | 3.5 | 0.5 / 0.2 | **4.1** | 5.3k |
| 256 | solid uniform | 27.3 | 17.7 | 12.2 / 5.6 | 29.9 | 112k |
| 256 | solid collapse | **26.2** | 14.2 | **2.1 / 1.0** | **16.3** | 21.5k |

S2+S4 @256: **29.9 → 16.3 ms (−45%)**. Full drag frame @256 only **27.3 → 26.2** because S1 seal + dirty resample still run whole-field, and collapse cannot reuse regional slabs. @128 the frame cut is real (**8.4 → 5.2**).

### How to test

Add Voxel Remesh to a poly body. **Collapse flats** is on for a new remesh. Surface should be coarser and still live under a vertex drag. Uncheck the box to get the old uniform lattice. Do not merge to master until that has been tried.

## Regional collapse extract (2026-08-13)

Parent-even-z slabs + leftover cz planes are retained (`OctreeCollapseCache`). A dirty point box recomputes only the dilated parent/leftover slabs; stitch + quads still walk the whole surface. Collapse still skips the uniform cz-slab project cache (numbering is parent-then-leftover, not compact index).

Parity: `cached_collapse_extract_matches_full`, `cached_collapse_unchanged_grid_recomputes_nothing`, `cached_collapse_cold_cache_ignores_local_hint`. Remesh: `remesh_collapse_incremental_reuses_parent_slabs`.

### After (`fast-release`, same harness, this session)

| res | path | drag frame | S2 full | S2 cached(None) | S4 draft | parents / leftover recomputed |
|-----|------|------------|---------|-----------------|----------|-------------------------------|
| 128 | solid uniform | 10.0 | 5.0 | — | 2.6 | 64/127 slabs |
| 128 | solid collapse | **8.7** | 4.6 | 2.6 | 0.6 | 34 parents / 69 leftover |
| 256 | solid uniform | 34.8 | 20.5 | — | 10.7 | 80/255 slabs |
| 256 | solid collapse | **36.5** | 25.3 | **11.9** | 2.8 | 42 parents / 85 leftover |

`S2 cached(None)` is stitch + quads + relax with zero QEF — the extract floor. A vertex grab still pays that floor plus the dilated dirty parents. Seal (~4 ms @256) and full project (collapse has no slab S3) are unchanged.

@128 collapse stays the faster live path. @256 the frame does **not** beat uniform yet: QEF work dropped, the leftover is seal + whole-surface stitch/quads + full project. Next lever is cheaper seal or slab-keyed project on the parent/leftover lists.

## Dirty collapse project + seal skip (2026-08-13)

Collapse S3 now reuses last snaps on clean parent/leftover slabs (`apply_collapse_slab_project`). Whole-field seal runs only when the dirty box **lost air** (an edit that cannot fill air cannot create a pocket). Outward vertex pulls still seal; inward / no-flip writes skip the flood.

Gate: `remesh_collapse_incremental_projects_dirty_slabs_only` (res 128 — at 64 the ±1-chunk dilation spans the lattice).

### Before (same harness, this session, `fast-release`)

| res | path | drag frame |
|-----|------|------------|
| 128 | solid uniform | 9.9 |
| 128 | solid collapse | **8.3** |
| 256 | solid uniform | 34.5 |
| 256 | solid collapse | **26.7** |

### After

| res | path | drag frame | S3 dirty verts | vs before |
|-----|------|------------|----------------|-----------|
| 128 | solid uniform | 10.0 | — | noise |
| 128 | solid collapse | **6.4** | 6283 / 6984 | **−1.9** |
| 256 | solid uniform | 34.3 | — | noise |
| 256 | solid collapse | **20.9** | 15159 / 28071 | **−5.8** |

Collapse @256 is now faster than uniform (20.9 vs 34.3). Seal skip does **not** fire on the outward-pull harness (air count drops). The frame cut is the dirty project.

## Default collapse + skip-relax-while-dragging (2026-08-14)

New remesh: Collapse flats ON, Skip relax while dragging ON. Old files unchanged.

Same harness, `fast-release`, this session:

| res | path | drag frame |
|-----|------|------------|
| 128 | uniform (old default) | 10.9 |
| 128 | collapse, relax on drag | 6.5 |
| 128 | collapse + skip relax (**new default**) | **6.4** |
| 256 | uniform (old default) | **34.7** |
| 256 | collapse, relax on drag | 22.9 |
| 256 | collapse + skip relax (**new default**) | **20.7** |

256 new-default vs old-default: **34.7 → 20.7 ms (−40%)**. Almost all of that is collapse. Skip-relax is **−2.2 ms** at 256 (22.9 → 20.7) and noise at 128 — stitch/quads still dominate the extract floor.
