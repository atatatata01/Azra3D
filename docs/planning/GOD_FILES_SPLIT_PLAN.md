# God-Files Split Plan — code health program (master board)

> **Authored 2026-08-03 from the LIVE tree** (`f24b86b` + uncommitted WIP audit). Supersedes the
> app-crate portion of `APP_CRATE_DEGOD_PLAN.md` for *sequencing*; that doc stays authoritative
> for its **mega-method borrow analysis** (§ below) and its per-file responsibility maps.
>
> **Goal:** healthy code — no file over ~2,000 lines unless it is a single cohesive responsibility.
> **NOT a build-time plan.** Rust compiles per crate: splitting files inside a crate changes
> rebuild time by ~nothing. Build time has its own two levers, both separate from this program:
> the `fast-release` profile (landed 2026-08-03, `Run_Fast.bat`) and **crate extraction**
> (Wave 5, the only wave that may claim a build-time win — and must MEASURE it).

---

## 0. Contract (every slice, non-negotiable — inherited from APP_CRATE_DEGOD_PLAN)

1. **PURE RELOCATION.** No logic change, no signature change, no opportunistic dedup/rename.
   Doc comments travel WITH their code — especially load-bearing invariant comments
   (`imported_persistent` single-writer, frame-ordering notes, serde append-only warnings).
2. **One file per commit.** `cargo build -p app` green after every slice. `cargo test -p core_math`
   after any core_math slice; `cargo test -p graphics` after any graphics slice.
3. **Serialized shapes are untouchable.** Relocating code is fine; **reordering any
   `#[derive(Serialize)]` struct field or enum variant is save-corruption** (bincode is
   positional). Files carrying this risk are tagged 🔴 below and get a mandatory
   before/after `git diff` review of every serialized type.
4. **Module-dir pattern:** `foo.rs` → `foo/mod.rs` (re-exports + shared types) + topic files,
   exactly like the existing precedents (`half_edge/`, `voxel_sculpt/`, `material_panel/`,
   `sdf_gpu_mesh/`, `body_volume_cache/`, `nurbs_fillet/`, `shader_nodes/`, `ui2/`).
5. **A file with uncommitted WIP is FROZEN for splitting** until that WIP lands. Splitting under
   live WIP is how mixed commits happen (three near-misses on 2026-08-03 alone).
6. Domain rules still apply: read `BEVEL.md` before touching `bevel_op.rs`; `half_edge` serialized
   shape red line; shader-adjacent files batch their changes (pipeline cache invalidation).

## 0b. Preconditions (before ANY wave starts)

- [x] **WIP-zero** — 2026-08-03: Grok's 37-file stream reviewed by a three-agent panel
      (zero blockers, 6 WARNs logged in COMPLETE_BUG_TRACKER) and landed as `a4dcc05`.
      Worktree cleanup same day: 17 worktrees + 22 branches deleted, 2 dirty trees rescued
      to `wip/perfbase-rescue-20260801` and `feat/kernel-locality`. All 🔒WIP freeze tags
      below are LIFTED.
- [x] **Baseline numbers** (this machine, 2026-08-03): `--release` cold ≈ 11–13 min,
      incremental (app-crate change) ≈ 6–8 min (opt3 + fat LTO + CGU=1);
      `fast-release` cold ≈ 2m45s, incremental ≈ ≤2 min; `cargo test -p core_math` ≈ 25 s
      (807 tests). Prediction on record: Waves 1–4 ≈ zero build-time delta; only Wave 5
      may move these numbers and must show it.

---

## 1. Inventory — every file ≥1,500 lines (live tree, 2026-08-03)

Priorities: **P0** = worst offenders, multi-responsibility, split first. **P1** = clear split.
**P2** = split when touched / after P0-P1. **KEEP** = big but cohesive, do not split.
**🔒WIP** = frozen until Grok's work lands. **🔴** = serialized-shape risk. **⚠️** = special rule.

### core_math (Wave 1 owner: poly/kernel conversation)

| File | Lines | Prio | Notes / target layout |
|---|---|---|---|
| `bevel_op.rs` | 5,565 | **P0** ⚠️ | Read `BEVEL.md` FIRST. → `bevel_op/{mod,offsets,weld,rails,corners,caps,clamp,tests}.rs` — §4 pipeline STEPs are the natural seams |
| `half_edge/tests.rs` | 5,257 | P2 🔒WIP | Tests-only → split by op family (`tests_topology`, `tests_bevel`, `tests_boolean`…) once WIP lands |
| `display_mesh_ops.rs` | 4,041 | **P0** | → `display_mesh_ops/{mod,modifier_fold,adjacency,normals,tests}.rs` (prefix-cache memo stays intact WITH its flaky-bench note) |
| `chunked_dc.rs` | 3,764 | P1 | → `chunked_dc/{mod,grid,extract,stitch,tests}.rs` |
| `scene_graph.rs` | 3,519 | **P0** 🔴 | NodePayload serde append-only; `VoxelBody` doc comment is authoritative. → `scene_graph/{mod,node,payload,transform,materials,queries,validate,tests}.rs` — payload.rs gets the serde warning banner |
| `mesh_boolean.rs` | 2,809 | P1 | → `mesh_boolean/{mod,bsp,classify,rebuild,tests}.rs`; keep `mesh_boolean_manifold.rs` beside it (Wave 5 crate candidate) |
| `nurbs_fillet/solve.rs` | 2,774 | P2 | Already in a module dir — internal split only if NURBS work resumes |
| `voxel_sculpt/{tests,ops,grid}.rs` | 2,958/2,676/2,548 | P2 | Already a module dir; ops.rs → per-operator files when sculpt work resumes (sculpt owner) |
| `half_edge/ops_misc.rs` | 2,267 | P1 🔒WIP | The "misc" name is the smell — split into named op families once WIP lands |
| `nurbs_fillet/tests.rs` | 2,277 | P3 | tests-only |

### app — input/tools (Wave 2 owner: general/master conversations)

| File | Lines | Prio | Notes / target layout |
|---|---|---|---|
| `input_sculpt.rs` | 5,904 | **P0** | → `input_sculpt/{mod,stroke,brush_apply,falloff,ui_state}.rs`. APP_CRATE_DEGOD_PLAN slice map applies |
| `input_viewport.rs` | 3,178 | P1 ⚠️🔒WIP | Mega-method warning: `handle_viewport_input` holds a long `&mut` borrow — extract WHOLE per old plan, no interior carve |
| `viewport_render.rs` | 3,138 | P1 ⚠️ | Same warning for `dispatch_gpu_render` — move satellite fns out (serialize block, texture-creation block), keep the dispatch body whole |
| `stress_test.rs` | 2,825 | P2 | → per-harness files (`instance`, `dense`, `multi_mesh`, `ui`) |
| `gpu_init.rs` | 2,778 | P1 | → `gpu_init/{mod,bgl,pipelines_bg,pipelines_lazy,cache}.rs` |
| `nurbs_tools.rs` | 2,554 | P2 🔒WIP | frozen (WIP) |
| `poly_drag_tools.rs` | 2,437 | P1 🔒WIP | → per-tool files under `ops/poly/` once WIP lands |
| `gizmo.rs` | 2,373 | P1 | Old plan has the slice map; `apply_gizmo` transform path recently touched (mark_transform_dirty) — comments travel |
| `voxel_gpu_mesh.rs` | 2,271 | P1 | Old plan: byte-for-byte movable (sculpt owner) |
| `mesh_presentation.rs` | 2,223 | P2 | Facade — cohesive; split only the test half out |
| `input_keyboard.rs` | 2,117 | P1 🔒WIP | → `input_keyboard/{mod,global,poly_modal,pies,transform_arm}.rs` once WIP lands |
| `poly_selection.rs` | 1,869 | P2 | paint/pick/convert seams |
| `poly/mesh_sync.rs` | 1,669 | P2 ⚠️ | Single-writer `imported_persistent` invariant comments MUST travel intact |
| `poly_knife.rs` | ~1,300 post-WIP | KEEP | Fresh feature, cohesive, under threshold once landed |

### app — materials / node graph (Wave 3 owner: materials conversation)

| File | Lines | Prio | Notes / target layout |
|---|---|---|---|
| `node_graph_bake.rs` | **9,393** | **P0** — single worst file in the repo | → `node_graph_bake/{mod,graph_eval,bake_dispatch,texture_cache,packing,m4_scoreboard,tests}.rs` |
| `material_panel/lower.rs` | 4,461 | **P0** | Already in a module dir but lower.rs re-fused → split by section (layers, maps, preview, actions) |
| `material_panel/view.rs` | 2,273 | P1 | same treatment |
| `material_panel/layer_ui.rs` | 1,721 | P2 | borderline |
| `shader_nodes/graph.rs` / `editor.rs` | 2,197/2,113 | P2 ⚠️ | `node_type.rs` serde enum append-only — do not relocate node types across files |
| `content_library.rs` | 1,880 | P2 🔒WIP | frozen (WIP) |
| `library_panel.rs` | 2,469 | P2 🔒WIP | frozen (WIP) |

### app — render/GPU/project (Wave 4 owner: master conversation)

| File | Lines | Prio | Notes / target layout |
|---|---|---|---|
| `body_volume_cache/bake_dispatch.rs` | 3,277 | P1 | already module dir; split dispatch vs readback vs region math |
| `sdf_gpu_mesh/tests_*.rs` | 3,738/2,739 | P3 | tests-only |
| `sdf_gpu_mesh/resident.rs` | 2,065 | P2 | |
| `project.rs` | 2,150 | **P1** 🔴🔴 | THE most dangerous file: v7 + FL4ORDER side-channel + 8MB-stack serialize thread. → `project/{mod,format,migrate,autosave,io_thread}.rs`. Mandatory serialized-type diff review; save-load round-trip test before AND after |
| `graphics/serialize.rs` | 2,738 | P1 🔴 | GPU buffer layout = shader contract. → `serialize/{mod,prims,materials,texmaps,bvh}.rs`; FLOATS_PER_PRIM and packing tables stay in one file with their WGSL-mirror comments |
| `raster_gbuffer.rs` | 1,960 | P2 | occlusion / instance-upload / encode seams (all recently touched — comments travel) |
| `ui_panels.rs` | 2,018 | P2 | |
| `viewport_overlays.rs` | 1,867 | P2 | |
| `soak.rs` | 3,374 | P3 | dev harness |
| `app_update.rs` | 1,814 | P2 🔒WIP | update() mega-method — old plan: extract whole, never carve |
| `menus.rs` / `scene_actions.rs` / `pie_menu.rs` / `properties_*` / others 1.5–1.8k | | P2 | split when touched (CLAUDE.md file-organization rule) |

### KEEP (big but cohesive — do not split without a new reason)
`main.rs` (893 — already de-godded), `app_poll.rs` (723), `mesh_boolean_manifold.rs`,
`half_edge/` current topic files except ops_misc/tests, `raster_common.rs`.

---

## 2. Waves (SERIAL, not parallel — one wave in flight at a time)

Parallel-by-owner was the old model; it predates this week's evidence that concurrent WIP on
shared files produces mixed commits. One wave at a time, cold files only, small commits.

| Wave | Scope | Gate to start | Definition of done |
|---|---|---|---|
| **0** | Preconditions (§0b): WIP-zero + baseline numbers | — | both checkboxes ticked |
| **1** | core_math P0s: `bevel_op`, `display_mesh_ops`, `scene_graph` (+`chunked_dc`, `mesh_boolean` P1) | Wave 0 | core_math tests green ×5 commits; scene_graph serde diff reviewed |
| **2** | app input/tools P0-P1: `input_sculpt`, `gpu_init`, `gizmo`, `voxel_gpu_mesh`, then the WIP-frozen files as they unfreeze | Wave 1 | build green per slice; manual smoke: sculpt stroke, gizmo drag, K knife |
| **3** | materials: `node_graph_bake` (the 9.4k monster), `material_panel/lower`+`view` | Wave 1 (independent of 2 but NOT concurrent) | bake produces identical output on a reference graph (visdiff rig) |
| **4** | render/GPU/project: `bake_dispatch`, `serialize.rs`, `project.rs` LAST | Waves 1–3 | save-load round-trip on a rich `.fl4` before/after `project.rs` split |
| **5** | **Build-time (the only wave allowed to claim it):** extract crates — candidates: `mesh_boolean`(+manifold) → `core_boolean`; `voxel_sculpt` → `core_sculpt`; `texgen`/`material_dna` → `core_texgen`. MEASURE incremental build before/after each extract; an extract that doesn't move the number gets reverted | Wave 1 | numbers published in this doc |
| 6 (opt) | tests P3 files | anytime after 1 | — |

## 3. Standing rules while the program runs

- Feature work continues; a feature touching a P0/P1 file does NOT wait — the split waits for it.
- Every completed slice: tick the row here (file, commit hash, date). This table is the único
  status board — no second tracker.
- If a "pure relocation" turns out to need a logic change: STOP, land the relocation without it,
  file the logic issue separately (No-Bandaids rule).
- The mega-methods (`update`, `dispatch_gpu_render`, `handle_viewport_input`) are moved WHOLE or
  not at all — APP_CRATE_DEGOD_PLAN §borrow-analysis is the authority.

## 4. Session log

- 2026-08-03 — plan authored from live tree.
- 2026-08-03 (later) — **Wave 0 CLOSED**: WIP reviewed+landed (a4dcc05), worktrees cleaned, baselines recorded. Wave 1 (core_math P0s) is GO.
- 2026-08-03 (evening) — **Wave 1 BATCH 1 MERGED** (4 Grok agents orchestrated in parallel worktrees, Claude relocation-audited every slice, serial merges; zero relocation violations across ~30 slices):
  - `input_sculpt.rs` 5,904 → `input_sculpt/{mod 644, stroke 2205, brush_apply 1627, falloff 769, ui_state 511}` (c4d751e)
  - `bevel_op.rs` 5,565 → `bevel_op/{mod 1908, offsets, weld, rails, corners, caps, clamp-seam, tests 2602}` — clamp stays in the STEP-1 fn body (documented seam, cannot split a fn); BEVEL.md §11 logged (295ea99)
  - `display_mesh_ops.rs` 4,041 → `display_mesh_ops/{mod 579, modifier_fold 2835, adjacency, normals, tests}` (24303e1)
  - `node_graph_bake.rs` 9,393 → `node_graph_bake/{mod 265, bake_dispatch 3295, graph_eval 2465, texture_cache, packing, m4_scoreboard, tests 2537}`; gpu_material_bake source-gate test path updated (8044e43)
  - **Wave 1b follow-ups (bounced-for-depth, queued P1):** `display_mesh_ops/modifier_fold.rs` (2,835 — peel per-operator impls into op_impls.rs); `node_graph_bake/bake_dispatch.rs` (3,295 — dispatch/queue vs readback vs rect math); `node_graph_bake/graph_eval.rs` (2,465 — node eval vs graph plumbing). Same contract; any agent may take them.
  - 809 core_math tests green post-merge; freeze on the four path families LIFTED.
- 2026-08-03 (late night) — **BATCH 3 MERGED** (4 Grok agents, master `a1ba833`): Wave 1b CLEARED — `bake_dispatch` 3,295→1,185+bake_maps 1,007+bake_poll 1,249; `graph_eval` 2,465→graph_eval/{mod 1,233, eval_nodes 1,338}; `modifier_fold` 2,835→177 driver + op_impls 2,811 (flat operator library, cohesive). Fresh: `gizmo` 2,373→5 files (mod 9, mark_transform_dirty comments verified glued); `voxel_gpu_mesh` 2,271→4 files (mod 21); `gpu_init` 2,778→5 files (bgl single-source intact). **Twelve god-files converted in one day, zero relocation violations.** Remaining board: reserved-for-Claude 🔴 files (project, serialize) + mega-method files (input_viewport, viewport_render satellites) + P2 split-when-touched tail.
- 2026-08-03 (night) — **BATCH 2 MERGED** (same protocol, 4 Grok agents): `scene_graph.rs` 3,519 → 8 files (all serialized types **md5-verified byte-identical** — the 🔴 red line held); `chunked_dc.rs` 3,764 → 5 files (mod 37); `mesh_boolean.rs` 2,809 → 5 files (chain-merge fix + oracle tests verified intact); `material_panel/lower.rs` 4,461 → 5 files (mod 2,103 — thin when touched). Master `dd307de`, 809 tests green. **Eight god-files down; remaining P0/P1 board: Wave 1b residuals + gpu_init, gizmo, viewport_render satellites, input_viewport, voxel_gpu_mesh, poly_drag_tools, input_keyboard, project 🔴, graphics/serialize 🔴.**
