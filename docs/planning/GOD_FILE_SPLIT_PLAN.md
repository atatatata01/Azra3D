# God-File Split Plan

**Date:** 2026-08-02  
**Scope:** oversized modules that slow every change (review, merge, tests, locality work).  
**Contract:** pure **code relocation** unless a step explicitly says otherwise. No behavior change, no opportunistic refactors, no “while I’m here.”

---

## 1. Rules (non-negotiable)

| Rule | Detail |
|------|--------|
| **One slice per PR / commit** | One file family moved; green tests; stop. |
| **Public API stable** | External crates keep `use core_math::half_edge::{HalfEdgeMesh, VertexId, …}`. Use `pub use` facades. |
| **Gate every slice** | `cargo test -p core_math --lib` (kernel) or `cargo test -p app --lib` / build (app). Baseline today: **core_math ~768** tests. |
| **No logic in the move** | Cut/paste + `mod` + re-exports. Fix only compile errors from privacy/paths. |
| **Tests move with code** | Prefer `#[cfg(test)] mod tests` in the new file, or `tests_*.rs` sibling under the same dir. |
| **Frozen / ownership** | Do not mix feature work (bevel locality, materials) into a split PR. One concern. |
| **App crate already planned** | See [`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md). **Do not invent a second app split.** This plan owns **core_math** + app files **missing** from that plan only. |

---

## 2. Ownership (no duplicate actions)

| Owner plan | Files | Do not also… |
|------------|--------|----------------|
| **This plan** | `core_math/**` god files; app gaps below | Re-slice `input_sculpt`, `viewport_render`, `gizmo`, `main`, poll, raster, … |
| **[`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md)** | Existing app slices 1–32 (`input_sculpt`, `input_viewport`, `viewport_render`, `gizmo`, `main`, sdf/voxel GPU, poll, …) | Rewrite those slices here or pick new module names for the same moves |

**App files listed only as “gaps”** (not in APP_CRATE_DEGOD_DATA / plan as of 2026-08-02):

| Lines | File | Note |
|------:|------|------|
| ~9.0k | `app/src/node_graph_bake.rs` | **New queue** — not in APP degod |
| ~4.3k | `app/src/material_panel/lower.rs` | **New queue** — folder exists; submodule split only |
| ~2.2k | `app/src/poly_drag_tools.rs` | **New queue** — not in APP degod |

Everything else under `app/` that looks large is **already owned by APP_CRATE_DEGOD** — execute that plan; do not open parallel tickets.

---

## 2b. Inventory — this plan’s work (core_math + app gaps only)

Measured on `feat/kernel-locality` worktree (~line counts, exclude `target/`).

### Tier A — must split (this plan)

| Lines | File | Role |
|------:|------|------|
| **~10.9k** | `core_math/src/half_edge.rs` | Mesh kernel + ops + builders + ~195 tests |
| **~6.3k** | `core_math/src/nurbs_fillet.rs` | Fillet + oracles |
| **~5.4k** | `core_math/src/voxel_sculpt.rs` | Voxel sculpt + tests |
| **~4.9k** | `core_math/src/bevel_op.rs` | Bevel (already outside half_edge — good) |
| **~9.0k** | `app/src/node_graph_bake.rs` | App **gap** (not in APP degod) |

### Tier B — should split (this plan, core_math)

| Lines | File |
|------:|------|
| ~3.8k | `core_math/src/display_mesh_ops.rs` |
| ~3.5k | `core_math/src/chunked_dc.rs` |
| ~3.2k | `core_math/src/scene_graph.rs` |
| ~2.6k | `core_math/src/mesh_boolean.rs` |

### Tier C — execute elsewhere (do not re-plan)

| Plan | Scope |
|------|--------|
| [`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md) | All listed app slices (input, viewport, gizmo, main, poll, …) |
| This document §3.3 gaps only | `node_graph_bake`, `poly_drag_tools`, `material_panel/lower` after APP degod or in parallel **without** overlapping files |

---

## 3. End-state layout (core_math)

### 3.1 `half_edge` → directory module

```
core_math/src/half_edge/
  mod.rs              # re-exports public API (drop-in for half_edge.rs)
  ids.rs              # VertexId, HalfEdgeId, FaceId, NO_*
  types.rs            # HVertex, HalfEdge, HFace, result structs, MirrorAxes
  geom.rs             # newell_normal, face_center_median_weighted
  alloc.rs            # free_vert / free_edge / free_face, alloc_*, reserve_elements, is_*_live
  access.rs           # vert/he/face getters, he_source
  nav.rs              # vertex_fan, face_vertices, face_half_edges, boundary queries, edge_slide_rails*
  normals.rs          # recompute_normals*, recompute_normals_region, weighted
  euler.rs            # split_edge, split_face_between, create_face*, delete_*, install_face_rings_local
  validate.rs         # validate, is_manifold, content_signature (if colocated)
  ops_extrude.rs      # extrude_face, extrude_region, extrude_vertices, …
  ops_inset.rs        # inset_*, circle_region, …
  ops_loop_cut.rs     # loop_ring_*, loop_cut*, face_rail_he
  ops_dissolve.rs     # dissolve_*, merge_vertices, …
  ops_misc.rs         # flip, poke, solidify hooks still in-file, bridge_loops, …
  builders.rs         # from_polygons, from_quads, new_box, new_plane, new_uv_sphere, …
  tests/              # OR #[cfg(test)] modules per file
    mod.rs
    euler.rs
    loop_cut.rs
    builders.rs
    …
```

**Facade (`half_edge/mod.rs`):**

```rust
mod ids;
mod types;
// …
pub use ids::*;
pub use types::*;
// … all current public names

pub struct HalfEdgeMesh { /* fields stay private to crate via pub(crate) modules */ }
// impl blocks live in the submodule files as `impl HalfEdgeMesh { … }`
```

**Caller impact:** ideally **zero**. `lib.rs` keeps `pub mod half_edge;`. Path `core_math::half_edge::HalfEdgeMesh` unchanged.

### 3.2 Other core_math targets (after half_edge)

| File | Target layout |
|------|----------------|
| `bevel_op.rs` | `bevel_op/{mod,params,weld,rails,miter,install,tests_oracle}.rs` |
| `nurbs_fillet.rs` | `nurbs_fillet/{mod,solve,cap,oracle,tests}.rs` |
| `voxel_sculpt.rs` | `voxel_sculpt/{mod,grid,brush,ops,tests}.rs` |
| `display_mesh_ops.rs` | by modifier family |
| `mesh_boolean.rs` | `boolean/{mod,bsp,robust,polish}.rs` |
| `scene_graph.rs` | types / hierarchy / materials / serde |

### 3.3 App targets (gaps only — no APP_CRATE_DEGOD overlap)

**Do not** redefine `input_sculpt` / `viewport_render` / `gizmo` / `main` / poll / raster here — those module names and order live in APP_CRATE_DEGOD.

| File | Suggested split | Collision with APP degod? |
|------|-----------------|---------------------------|
| `node_graph_bake.rs` | schedule / lower / execute / cache / tests | **None** — not in that plan |
| `poly_drag_tools.rs` | extrude_drag / inset_drag / transform_grs / bevel_modal | **None** |
| `material_panel/lower.rs` | submodules by node class under existing `material_panel/` | **None** (extend folder; don’t invent a second material crate) |

---

## 4. Execution order (gated)

### Phase 0 — Prep (½ day)

1. Branch: `chore/split-half-edge` from current trunk (or worktree off `master` / `feat/kernel-locality`).
2. Record baseline:  
   `cargo test -p core_math --lib 2>&1 | tee /tmp/core_math_baseline.txt`  
   Expect **768** (or current green count).
3. No feature commits on this branch.

### Phase 1 — `half_edge` mechanical split (P0)

Each step: move code → fix `use` → **same test count green** → commit.

| Step | Move | From (approx) | Into | Gate |
|------|------|---------------|------|------|
| H1 | Ids + constants + element structs | top of file | `ids.rs`, `types.rs` | test |
| H2 | Free-standing geom helpers | `newell_*` | `geom.rs` | test |
| H3 | Alloc / free chokepoints | `free_*`, `alloc_*`, `reserve_*`, `is_*_live` | `alloc.rs` | test + grep no raw free-list push |
| H4 | Accessors + `he_source` | getters | `access.rs` | test |
| H5 | Navigation | fan, face rings, rails | `nav.rs` | test |
| H6 | Normals | full + region + weighted | `normals.rs` | test |
| H7 | Euler primitives | split_edge, split_face, create_face*, delete_*, install_* | `euler.rs` | test |
| H8 | Validate / manifold / signature | validate stack | `validate.rs` | test |
| H9 | Builders | `from_polygons`, primitives | `builders.rs` | test |
| H10 | Ops by family | extrude / inset / loop_cut / dissolve / misc | `ops_*.rs` | test per file |
| H11 | Tests out of main path | `#[cfg(test)]` blocks | `half_edge/tests/*.rs` | full suite |

**Stop condition for Phase 1:**  
`half_edge/mod.rs` + modules, **no** monolithic `half_edge.rs`, public API greppable, `cargo test -p core_math --lib` green, app still builds against facade.

### Phase 2 — `bevel_op` (P1, after half_edge stable)

| Step | Content |
|------|---------|
| B1 | `params` + `WeldMap` + diagnostics TLS |
| B2 | Rail / clamp / miter math (pure compute) |
| B3 | `install_bevel_region` + local install helpers |
| B4 | Oracle / blender-ref tests → `tests_oracle.rs` |
| B5 | Keep `bevel_edges_offset` / `_mut` as thin entry in `mod.rs` |

Gate: all `bevel` filtered tests + full core_math suite.  
Read [`poly/BEVEL.md`](./poly/BEVEL.md) before B2–B3 (graveyard).

### Phase 3 — `nurbs_fillet` + `voxel_sculpt` (P1)

| Track | Steps |
|-------|--------|
| Fillet | solve / surface build / trim glue / oracle tests |
| Voxel | grid storage / brush apply / symmetrize / tests |

Independent tracks; can parallelize in two worktrees.

### Phase 4 — App

1. **Only** [`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md) for files it already owns (no alternate module names).  
2. **Gaps** (this plan §3.3 only), after or in parallel **if** different files: `node_graph_bake` → `poly_drag_tools` → `material_panel/lower` submodules.  
3. Never open two PRs that both touch the same app path.

### Phase 5 — Optional cleanup (only after A–D green)

- Document max file size guideline: **prefer &lt; 2k lines** for non-test sources; **&lt; 3k** for algorithm modules with embedded oracles.  
- CI optional: warn if new file &gt; 4k (not fail — tests may be large).

---

## 5. How to move a slice (recipe)

```text
1. Create empty module file under the new dir.
2. Cut the contiguous region from the source file.
3. Add necessary `use` at top of new file (crate:: / super::).
4. Make private helpers `pub(crate)` only if another submodule needs them.
5. `mod foo;` + `pub use` in parent mod.rs if names must stay public.
6. cargo test -p core_math --lib
7. Commit: "chore(core_math): split half_edge — <slice name>"
```

**Do not:** rename types, change free-list rules, merge duplicate logic, or “fix” bugs in the same commit.

---

## 6. Risk map

| Risk | Mitigation |
|------|------------|
| `impl HalfEdgeMesh` split across files | Allowed in Rust; keep fields in `mod.rs` or `types` with `pub(super)` fields. |
| Private free-lists | Stay private to `half_edge` crate module; only `alloc.rs` mutates. |
| Circular mods | Dependency order: ids → types → alloc → access → nav → normals → euler → ops → builders. |
| App compile break | Facade re-exports every old public path; `cargo build -p app` after H1, H7, H11. |
| Test count drift | Fail the slice if test count drops without an explicit delete. |
| Merge conflict with locality/feature branches | Split branch only; rebase features after Phase 1 lands. |

---

## 7. Success criteria

| Milestone | Done when |
|-----------|-----------|
| **M1** | `half_edge` is a directory; largest submodule **&lt; ~2.5k lines**; suite green |
| **M2** | `bevel_op` split; oracle tests still pass; suite green |
| **M3** | `nurbs_fillet` + `voxel_sculpt` split |
| **M4** | APP_CRATE_DEGOD Phase through at least slices 1–15 (or current priority list) |
| **M5** | No non-test `.rs` under `core_math` / `app` **&gt; 5k** without an open split ticket |

---

## 8. Suggested first three commits (copy-paste)

```
chore(core_math): half_edge dir facade — move ids + types (H1)
chore(core_math): half_edge — alloc/free chokepoints to alloc.rs (H3)
chore(core_math): half_edge — nav + accessors (H4–H5)
```

After those three, stop and re-baseline before ops/builders (higher conflict risk).

---

## 9. Out of scope (this plan)

- Changing half-edge topology rules or free-list design  
- Bevel algorithm / locality features (separate feature branches)  
- Deleting “dead” ops during the move  
- Rewriting app `update()` borrow structure (see APP_CRATE_DEGOD forced extract-method notes)

---

## 10. Related docs

| Doc | Use |
|-----|-----|
| [`APP_CRATE_DEGOD_PLAN.md`](./APP_CRATE_DEGOD_PLAN.md) | App file-by-file slices |
| [`APP_CRATE_DEGOD_DATA.json`](./APP_CRATE_DEGOD_DATA.json) | Raw responsibility maps |
| [`poly/BEVEL.md`](./poly/BEVEL.md) | Before touching bevel_op split B2+ |
| [`LOCALITY_SPIKE.md`](../LOCALITY_SPIKE.md) | Why half_edge churn is high right now — **land splits after or on a quiet branch** |

---

*Plan only. Execute Phase 0 → H1 when ready; do not open all files at once.*

---

## 11. REVIEW AMENDMENTS (Claude, 2026-08-02 � binding)

1. **Serialization red line (CRITICAL, was missing):** HVertex / HalfEdge / HFace / HalfEdgeMesh
   (half_edge), voxel grid records (voxel_sculpt), and everything scene_graph serializes are
   BINCODE types � struct definitions move VERBATIM (field order + derives identical). Gate:
   the serde golden-gate tests (chore/serde-golden-gate: committed golden .bin blobs
   deserialized + value-asserted) must pass after every slice touching a serialized type.
2. **Baselines refreshed:** core_math = 789 passed / 8 ignored (post sculpt-merge, 2026-08-02).
   File sizes in �2b were measured pre-sculpt-merge; re-measure at fork. voxel_sculpt is ~7-8k now.
3. **Sequencing vs live feature agents:** a hot file is split ONLY by the agent that owns it,
   after its current feature round lands � half_edge/bevel_op after locality round 3;
   node_graph_bake + material_panel/lower by the material agent after M3. Never two agents in
   one file (the parallel-Hi-Z lesson).
4. **scene_graph.rs splits LAST, as a single dedicated slice** with the golden gate � most
   cross-cutting + NodePayload variant order is sacred.
5. **Execution 2026-08-02:** tracks running as parallel isolated worktrees � serde-golden-gate,
   split-half-edge, split-voxel-sculpt, split-nurbs-fillet � each with a mandatory FINDINGS
   file (bug sweep during relocation, no in-slice fixes).
