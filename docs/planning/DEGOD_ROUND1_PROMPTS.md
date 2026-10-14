# De-God Round 1 — Paste-Ready Prompts (one per worktree window)

Base for this round: **master @ `f05775b`** (baseline + de-god plan + per-pillar `main.rs` anchors).
Full plan: [`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md) · raw maps: [`APP_CRATE_DEGOD_DATA.json`](./APP_CRATE_DEGOD_DATA.json).

**Non-colliding by design:** each window edits only files it owns + creates its own new module + adds its `mod` line under its own header in `main.rs`'s `DE-GOD ROUND 1` anchor block. Only the **master** window rewrites `main.rs` body.

**Shared rules block (in every prompt):**
- PURE CODE RELOCATION — no logic change, no behavior change, no opportunistic dedup. If a move needs a logic change, STOP and report.
- `cargo build -p app` MUST pass before commit. Never leave a broken build.
- Add your `mod` / `pub(crate) use` line ONLY under YOUR header in the `main.rs` anchor block. Never add a `pub(crate) fn` to `main.rs`.
- Visibility widening (priv → `pub(crate)`) to enable a move is allowed; nothing else.
- ONE slice. Do not touch files owned by other pillars. When green: commit, then tell the MASTER window it's ready to integrate.

---

## 1) MASTER window — Slice 29 (main.rs type-def extraction)

```
You are on the `master` worktree (F:/PROJECT/FourthLAyerAi, branch master).
Execute Slice 29 from docs/planning/APP_CRATE_DEGOD_PLAN.md — a PURE CODE RELOCATION.

Move these TYPE DEFINITIONS out of core_engine/app/src/main.rs into three new sibling modules
(structs/enums + their inherent impls + the `map_state` mod — data only, no AzraApp methods):

  gpu_ctx.rs           <- map_state mod, PendingBake, PendingBrickFill, PendingPick,
                          PendingSurfacePick, DcJobResult, PendingDcJob, PipelineReady,
                          PipelineRegistry, BindGroupCache, BufferPool, GpuCtx, PendingScreenshot
  voxel_sculpt_types.rs<- UndoKind, VoxelUndoStack(+impl), VoxelColorUndoStack(+impl),
                          VoxelObject(+impl), MeshGrab(+impl), PendingGrab, VoxelBrushSettings,
                          SculptProfile, VoxelRebuildMsg, PendingVoxelRebuild
  app_state.rs         <- InteractionEpisode(+Default), DiagState, GpuInfo

Do NOT move: fn main, GpuInitState, the FourthLayerApp struct/impl, its constructor, update(),
any AzraApp method, or the consts (BODY_LOCAL_PRIMS, bakeprof_record, PERF_SPAM).

Steps: create the 3 files with the needed `use` imports; cut the definitions from main.rs; add
`mod gpu_ctx; pub(crate) use gpu_ctx::*;` (and the other two) UNDER the "--- master ---" header
in the anchor block so every existing `crate::GpuCtx` / `crate::VoxelObject` path still resolves.
Build green, run `cargo test -p app --no-run`, then commit. Verify main.rs dropped ~1085 lines.

RULES: pure relocation only, cargo build -p app must pass, no pub(crate) fn in main.rs,
visibility-widening only. If any move needs a logic change, STOP and report.
```

---

## 2) GENERAL window — C1 crash fix + Slice 8 (app_poll split)

```
You are on the `general` worktree (.claude/worktrees/general, branch dev/general).
FIRST: `git merge master` to get the clean base + de-god plan + main.rs anchors (fast-forward,
no conflicts expected — commit any WIP first).

Do two things in core_engine/app/src/app_poll.rs (general-owned), from APP_CRATE_DEGOD_PLAN.md:

C1 (crash fix, do first): at app_poll.rs ~lines 497, 513, 568, 569 the AI/Claude error toasts
slice strings by BYTE index (`&s[..n.min(80)]`) which PANICS on multi-byte UTF-8. Replace each
with `s.chars().take(N).collect::<String>()` (same N). Pure fix, no other change.

Slice 8 (pure relocation): move the method `poll_sdf_compile` (app_poll.rs ~262-439) into a new
sibling module `poll_pipeline_compile.rs` as `impl AzraApp { fn poll_sdf_compile(...) }`, moving
any fn-local `use` with it. Add `mod poll_pipeline_compile;` UNDER the "--- general ---" header in
the main.rs anchor block (no re-export needed — it's an inherent method).

Build green (cargo build -p app), then commit both. Tell MASTER when ready to integrate.

RULES: pure relocation only, cargo build -p app must pass, no pub(crate) fn in main.rs,
visibility-widening only, touch only app_poll.rs + the new module + your anchor line.
```

---

## 3) SCULPT window — Slice 1 (voxel_gpu_mesh brush split)

```
You are on the `sculpt-tool` worktree (.claude/worktrees/sculpt-tool, branch feat/sculpt-tool).
FIRST: commit your WIP, then `git merge master` to get the base + plan + main.rs anchors; resolve
any conflicts favoring BOTH sides (master's changes are docs + the anchor comment block only).

Execute Slice 1 from APP_CRATE_DEGOD_PLAN.md — a PURE CODE RELOCATION in
core_engine/app/src/voxel_gpu_mesh.rs (sculpt-owned). Move into a new sibling module
`voxel_gpu_brush.rs`:
  BrushParams + pack_stamp_plan, gpu_stamp_plan, VoxelBrushGpu (+ its impls), VoxelBrushInflight.

Keep call sites working: they reference `crate::voxel_gpu_mesh::*`, so add
`pub(crate) use crate::voxel_gpu_brush::*;` INSIDE voxel_gpu_mesh.rs (facade re-export), and add
`mod voxel_gpu_brush;` UNDER the "--- sculpt ---" header in the main.rs anchor block. Verify the
~24 `crate::voxel_gpu_mesh::...` sites still resolve.

Build green (cargo build -p app), then commit. Tell MASTER when ready to integrate.

RULES: pure relocation only, cargo build -p app must pass, no pub(crate) fn in main.rs,
visibility-widening only, touch only voxel_gpu_mesh.rs + the new module + your anchor line.
```

---

## 4) POLY window — no god-file this round (optional de-god)

```
You are on the `poly-foundation` worktree (.claude/worktrees/poly-foundation, branch
feat/poly-foundation). Round 1 has NO poly god-file in APP_CRATE_DEGOD_PLAN.md.

Preferred: keep doing your poly-foundation feature work — you'll pick up shared-file slices in a
later round (see plan §5 coordination).

OR, if you want to help de-god now: split poly_selection.rs (~1066 lines, poly-owned). It was not
mapped by the analysis fleet, so FIRST map it yourself (list its distinct responsibilities + clean
seams), then do ONE low-risk pure-relocation extraction into a new sibling module, with a facade
re-export if call sites are module-qualified. Add `mod <new>;` UNDER the "--- poly ---" header in
the main.rs anchor block. Note: `git merge master` here is a larger merge (this branch diverged) —
only do it if you take the de-god option.

RULES: pure relocation only, cargo build -p app must pass, no pub(crate) fn in main.rs,
visibility-widening only, one extraction, touch only poly-owned files + your anchor line.
```

---

## Integration (MASTER owns this)
When a window says its slice is green + committed, the MASTER window merges that branch into
`master`. The only shared edit is the `main.rs` anchor block, where each pillar added lines under a
different header → 3-way merges are clean. Build once more on master after each merge.
