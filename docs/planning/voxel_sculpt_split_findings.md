# voxel_sculpt god-file split — good-practice bug sweep (2026-08-02)

Observations made while relocating `core_math/src/voxel_sculpt.rs` (9115 lines) into
`voxel_sculpt/{mod,brush,grid,ops,tests}.rs`. **Nothing here was fixed** — pure-relocation
contract. Line numbers refer to the NEW files on `chore/split-voxel-sculpt`.

## Near-duplicate function families (post sculpt-merge — do NOT dedupe blindly)
- `apply_brush` vs `apply_brush_legacy` (ops.rs): intentional pair — the golden parity test
  (`tests.rs::brush_parity_tests::golden_parity_stamp_plan_matches_legacy_all_builtins`)
  diffs them voxel-for-voxel. Legacy is load-bearing test oracle, not dead code.
- `apply_stamp_plan` vs `apply_stamp_plan_with_tip` (ops.rs): the former is a thin wrapper,
  fine.
- `sample_static` vs `sample_static_smooth` (ops.rs): two near-identical trilinear samplers
  over `DensityStorage` (one NaN-aware dual-buffer variant). Candidate for one shared core.
- `sample_area_normal` / `sample_area_normal_fast` / `sample_area_normal_quality` (grid.rs):
  three-way variant family; `sample_area_normal` just forwards to `_fast`.
- `elastic_grab_preview` vs `elastic_grab_preview_multi` (ops.rs): single-instance path is
  kept alongside the symmetry multi path; single is NOT expressed as multi-with-one-instance,
  so the two warp loops can drift.
- `DensityStorage::*` vs `BrickGrid::*` mirrored method pairs (`region_const`, `fill_from`,
  `par_fill_from`, `materialize_box*`, `compact_*`, …) — deliberate dispatch wrappers, fine.

## Suspicious reads (unchecked indexing / length assumptions)
- ✅ FIXED 2026-08-02 (fix/voxel-sweep) — `colors: Vec<u32>` unchecked write sites: all paint
  write paths now go through ONE chokepoint `ensure_paint_colors()` (ops.rs) — sparse → no-op
  and any stale dense buffer is dropped; dense → exact `density.len()` realloc, a stale
  wrong-length layout is dropped, never partially reused. `colors_in_sync()` is the shared
  predicate for non-allocating read-modify paths (`restore_delta`). Regression test:
  `paint_write_paths_survive_colors_density_divergence`.
- ✅ FIXED 2026-08-02 (fix/voxel-sweep) — `mask`/`mask_sparse` not migrated on resample:
  `migrate_mask_into()` (grid.rs) migrates the protect-mask world-shape-preserving in BOTH
  directions (dense↔sparse) from `resample` and `resample_to`, writing the storage the dest
  density uses. Regression test: `mask_survives_dense_sparse_resample_round_trip`.
- `StrokeRecorder::record` (brush.rs) decodes flat idx with the res latched at `begin()`;
  a caller recording against a different-res grid gets silently wrong brick masks (no
  assert that recorder res == grid res at the `apply_*` call sites).
- `ElasticGrab` restore loop (ops.rs, `elastic_grab_preview`) guards `iu < density.len()`
  — a pristine map carried across a resolution change is silently truncated rather than
  asserted.

## Symmetry
- Grab symmetry: `grab_symmetry_instances` + `grab_instances_disjoint` pick between a joint
  displace (`grab_density_displace_joint`) and per-instance passes. The per-instance
  fallback path can double-touch voxels where footprints overlap near the mirror plane
  (that is exactly what `grab_instances_disjoint` tries to exclude — the correctness of the
  overlap test carries the whole no-double-apply guarantee; worth a dedicated test).

## Merge-debris signals (compiler warnings, pre-existing on the unsplit file)
- ops.rs:1832 `let mut nxt = …` — `mut` unused (PlanePush arm).
- tests.rs:2024 unused `z1` in the subvolume layout test.
- brush.rs / falloff section: `falloff_curve_only` doc comment mentions presets JSON; the
  oracle test `include_str!`s `docs/planning/sculpt/oracle/falloff_presets_ref.json`
  (path depth adjusted +1 for the module move — only non-relocation edit in the split).

## Split bookkeeping (intentional, documented in commit messages)
- Visibility widenings, all `pub(super)` (invisible outside `core_math::voxel_sculpt`):
  brush.rs `ElasticGrab.pristine`; grid.rs `BrickGrid` fields `index`/`bricks`/`free`,
  `BrickGrid::{new,at,set,index_tag,compact_brick,allocated_bricks,fill_from,par_fill_from}`,
  `VoxelGrid::{idx,mark_dirty}`, free fns `grid_cell_world`, `smoothstep01`, `sub_at`,
  `sample_sub`, consts `BRICK`, `BRICK_VOX`, `TAG_EMPTY`, `TAG_FULL`; ops.rs
  `sample_static`, `grab_instances_disjoint` (test access).
- Serialized types `VoxelGridPayload` / `VoxelGridRecord` moved VERBATIM to grid.rs
  (field order + derives untouched); `serde_golden` gate green on every commit.
- `brush_parity_tests` is now nested inside `mod tests` (test paths gain a `tests::`
  segment; count unchanged). `grab_density_large_pull_keeps_solid_core` remains `#[ignore]`d.
