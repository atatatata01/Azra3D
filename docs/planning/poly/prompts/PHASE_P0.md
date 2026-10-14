# BUILD PROMPT — Phase P0: Decide, unfreeze (scoped), consolidate bevel, scaffold the spine

## (1) TASK
Record the HEX-BL decision + scoped freeze-lift in CLAUDE.md, scaffold the non-serialized `PolyMesh` runtime wrapper + `core_math/src/mesh_ops/` `MeshOp` trait + the test-spine skeleton, and complete `bevel_op.rs` v1 while retiring the wrong-algorithm `bevel_edges_ex` caller — all with ZERO `half_edge.rs` edits and zero behavior change beyond bevel.

## (2) SPEC POINTER
Read **`docs/planning/poly/POLY_FOUNDATION_PLAN.md` IN FULL FIRST** — it is authoritative. This phase is §7 "★ FIRST MILESTONE — Phase 0" plus the Phase 1 bevel gate (§7), governed by the red lines in §0, the test spine in §4.5, the freeze-lift scope in §6.5, and the FL4 rules in §6.2. Do not deviate from the plan; if any instruction here conflicts with the plan, the plan wins and you STOP and report (see Rules Block).

## (3) STEP 0 — AUDIT BEFORE WRITING (No-Duplication rule, mandatory)
Before writing ANY new function, grep the codebase and REPORT findings in the form `searched for "<term>": found <symbol> at <file>:<line>` or `none found`. At minimum search for:
- `bevel_edges`, `bevel_edges_ex`, `bevel_edges_offset`, `WeldMap`, `from_polygons`, `to_polygons` (the bevel + rebuild path — verify `bevel_op.rs:73-81` is the `mesh.clone()` stub and `:77` is the TODO).
- `bevel_edges_ex` callers (expect `poly_drag_tools.rs:709`, app-layer).
- `struct PolyMesh`, `PolygonalBody`, `PolyModifier`, `mesh_cache_id`, `Arc<HalfEdgeMesh>` (confirm `scene_graph.rs:363-370`, variant index 6; do NOT add a variant).
- `validate(` in `half_edge.rs` (confirm `:331-350`), `to_raster_data` (`:1376/:1394`), `HVertex` `position` (`:34`), `HFace` `material_id` (`:53`), `free_faces` (`:68`).
- `mesh_ops`, `MeshOp`, `mesh_eq`, `proptest`, `PolyLayerRecord`, `arc_serde_roundtrip`, `voxel_grids`, `VoxelNodeRecord` (confirm precedent in `project.rs`; FL4ORDER `deserialize(tail)` at `project.rs:356`).

If any function already exists that does what a step needs, EXTEND it (add a param) — never duplicate. Report "searched for existing: found X at Y:Z" or "none found" for every new function before writing it.

## (4) STANDARD RULES BLOCK (distilled from CLAUDE.md + FEATURE_RULES.md — binding)
- **STOP + report on rule conflict.** Before starting, check this prompt against every CLAUDE.md rule. If it contradicts one, STOP and report explicitly: which rule, which part of the prompt, and whether the rule or the prompt is wrong. Never silently override a CLAUDE.md rule — even one written by the owner.
- **No-Bandaids, production-DCC bar.** Every fix must pass the 5-question test (can this class of bug recur? does it hide a problem? does the next dev need tribal knowledge? does it scale? does it compose?). NEVER downgrade `assert`/`debug_assert` to `log`, add timing/frame-skip hacks, special-case one caller, clamp to hide NaN, clone to dodge the borrow checker, or wrap internal logic in `catch_unwind`. If you can't name the root cause, write a diagnostic, not a fix.
- **Build after EVERY change**, even 5 lines: `cargo build -p app`. If it breaks, fix immediately — never pile changes on a broken state. **`cargo test -p core_math` after any change touching `half_edge.rs` or `mesh_ops/`.**
- **Display Invariant.** Object Mode = triangles only, one technique per view; poly bakes to native triangles via `to_raster_data`. Never fuse two render techniques over the same pixels.
- **Frame Budget.** 12ms CPU target; never add unbounded per-frame work — every async/processing loop needs a `MAX_*` cap. GPU resource mutations only in `poll_background_tasks`; bind groups in `viewport_render`; never in panels.
- **FL4 bincode APPEND-ONLY when touching save / custom-data.** Enum variants append-only at the END (never insert/reorder). NEVER add a field to `HalfEdgeMesh` (in-blob, positional → corrupts every `.fl4`). NEVER `#[serde(skip)]` `PolygonalBody.mesh`. NEVER a second trailing magic after `FL4ORDER`. New authored data rides an in-blob `#[serde(default)]` field on `ProjectFile` (the `voxel_grids` precedent) — but this phase only scaffolds harness stubs; no live persisted field yet.
- **Stable IDs, not indices.** Typed newtype ids / `Handle{id,gen}` / `NodeId` / `PrimUid`. No raw element id escapes `core_math` into app selection/undo/tool caches.
- **Transaction / episode undo.** Every user-visible mutation goes through `transaction()` (atomic) or `begin/commit_episode()` (drag). `save_undo`/`save_delta_undo` stay deleted. The app glue (`mesh_actions.rs`) owns editing context; the kernel never does.
- **One feature per prompt.** This phase only (see Scope Guard). Fix one thing; do not refactor things you merely noticed.

## (5) FEATURE_RULES — THE 4 QUESTIONS (pre-answered for THIS phase)
1. **New scene object?** No. Reuse the EXISTING `NodePayload::PolygonalBody` (variant index 6). Do not add a NodePayload variant. The `PolyMesh` wrapper is a RUNTIME app/cache-state struct, never a scene/graph node, never serialized.
2. **Mutates state?** Bevel runs inside the existing `transaction()`/episode model via the app glue. The wrapper/`MeshOp` scaffold and test-spine skeleton are inert (no live mutation path yet). No new serialized state lands this phase.
3. **Frame-thread work?** Bevel eval + `to_raster_data` upload already route through the existing poll/`viewport_render` ordering; this phase adds no new per-frame work and no new GPU resource. No new `MAX_*` cap needed yet.
4. **New render path?** No. Poly stays native triangles in the one Object-Mode G-buffer (Display Invariant). No raymarch, no fused technique.

## (6) THE WORK — ordered, individually-buildable steps (Phase 0 ONLY)
Build + (where it touches core_math) test after EACH step. Do not start step N+1 on a red build.

**Step A — Record the decision + scoped freeze-lift in CLAUDE.md.** Amend the `half_edge.rs` freeze note (~line 352) from a blanket freeze to the SCOPED, GATED form per plan §6.5: bevel v1 needs ZERO `half_edge.rs` edits; the later amendment is scoped to (a) `classify()` as a NEW method leaving `validate()` byte-for-byte intact, (b) internal Euler-op parent-map/inverse-delta edits, (c) subdivide n-gon topology — each gated on full `cargo test -p core_math` green + gold-tier review per op. Record bmesh-rs as STUDY-ONLY (no fork/dep). Do NOT blanket-unfreeze. (Docs only — no build impact, but verify `cargo build -p app` still green.)

**Step B — Scaffold the `PolyMesh` runtime wrapper.** Add `struct PolyMesh { topo: HalfEdgeMesh, cd: CustomData, topo_index: TopoIndex, topo_gen: u64 }` in `core_math` (app/cache-state-adjacent module, NOT a field of `HalfEdgeMesh`, NOT serialized). `CustomData` and `TopoIndex` are EMPTY scaffolds this phase (no columns wired, no disk/radial built). NO `GenTags` yet (demand-driven — plan §4.4). RED LINE: zero new fields on `HalfEdgeMesh`; never `#[serde(skip)]` `PolygonalBody.mesh`. Build.

**Step C — Scaffold `core_math/src/mesh_ops/` + the `MeshOp` trait.** Create the module dir and the trait exactly per plan §4.7 (`type Params: Serialize + Clone; const PROPAGATES: bool; fn apply(&self, m, sel, p) -> Result<EditResult, OpError>`). Define `OpError` (incl. `NonManifoldUnsupported{edge}`), `EditResult`, `TopoDelta`, `OpDelta` as the shared types — empty/minimal but compiling. No op implementations beyond bevel (Step E). Build + `cargo test -p core_math`.

**Step D — Stand up the test-spine skeleton (plan §4.5).** Add: the `mesh_eq(deep)` helper (positions within epsilon + every CustomData column byte-equal + `topo_gen` consistency — degrades gracefully with empty CustomData this phase); the bincode **sentinel-variant guard** test (serialize a `HalfEdgeMesh`, append a sentinel variant after it inside a `PolygonalBody`, assert the sentinel reads back); FL4 round-trip suite STUBS (authored-body round-trip; pre-`PolyLayerRecord` file loads with empty CustomData; both-`child_order`-and-poly-layers file loads correctly; forged-v7 "no FL4ORDER magic" byte-identical — stubs may be `#[ignore]` where the live field doesn't exist yet, but the sentinel guard and `mesh_eq` must RUN and PASS); proptest scaffolding (random-mesh generator + `validate()` check). Build + `cargo test -p core_math`.

**Step E — Complete `bevel_op.rs` v1 (the merge-blocker, ZERO `half_edge.rs` edits).** Implement the TODO at `bevel_op.rs:77` on the pure offset-rail + WeldMap path: per-edge plane offset, corner line-intersection, corner-patch n-gon, affected-face rebuild via `from_polygons` + the tested `WeldMap`. It becomes the CANONICAL `MeshOp` reference template (wire it as the first `mesh_ops/bevel.rs` impl or have `bevel.rs` delegate to it — one path, no duplication). Build + `cargo test -p core_math`.

**Step F — Retire the wrong-algorithm `bevel_edges_ex` caller (SAME task).** Re-route `poly_drag_tools.rs:709` (app-layer) to the completed bevel path and DELETE the `bevel_edges_ex` usage so the two bevel paths collapse to ONE. The OLD bevel must keep working until the new path is verified driving the same UI (see Acceptance). Build `cargo build -p app`.

**Step G — Add the bevel proptest gate.** `apply → inverse → apply == identity` for bevel, asserted with `mesh_eq(deep)` (not just `validate()`). Green before this phase is done. `cargo test -p core_math`.

## (7) ACCEPTANCE CRITERIA
- `cargo build -p app` green after every step; `cargo test -p core_math` green after every core_math-touching step. Final: both green.
- **Validation + fuzz gates:** the proptest random-mesh generator runs `validate()` green; the bevel `apply→inverse→apply==identity` proptest is green using `mesh_eq(deep)` (positions+columns+`topo_gen`), NOT `validate()` alone.
- **Golden-topology gate:** bevel v1 produces the expected corner-patch n-gon + offset rails on a known cube/edge-loop input (a golden-topology assertion: face/vert/edge counts + manifold via `validate()`), not the `mesh.clone()` stub output.
- **Bincode safety gate:** the sentinel-variant guard test runs and PASSES (proves no non-skip field was added to `HalfEdgeMesh`); `arc_serde_roundtrip` still passes; `HalfEdgeMesh` has ZERO new fields; `PolygonalBody.mesh` is NOT serde-skipped.
- **OLD PATH KEEPS WORKING UNTIL THE NEW ONE IS VERIFIED:** existing poly bodies, the modifier stack, and SDF/voxel/Object-Mode render all still work at this commit. The `bevel_edges_ex` caller is removed ONLY after the new bevel path is confirmed driving the same drag UI and producing valid topology.
- No `half_edge.rs` edits in this phase. No new `NodePayload` variant. No new serialized field. No `GenTags` introduced.

## (8) DEFINITION OF DONE
1. Build passes (`-p app`) + test spine green (`-p core_math`).
2. Bevel v1 is real (no `mesh.clone()` stub); the two bevel paths are ONE.
3. Affected callers checked (grep all `bevel_edges_ex` / bevel usages; report each).
4. Undo / save-load impact considered: bevel rides existing `transaction()`/episode; no save-format change; sentinel guard + `arc_serde_roundtrip` green.
5. One manual test scenario described (e.g., "select an edge loop on a cube, drag-bevel, confirm corner-patch n-gon + offset rails, undo restores exact mesh via `mesh_eq`").
6. **Update `COMPLETE_BUG_TRACKER.md`** — mark the bevel-stub item ✅ FIXED + date (2026-06-30); add any new poly-foundation tracking items discovered.

## (9) SCOPE GUARD
THIS PHASE ONLY. No big-bang. Do NOT: implement Euler-op inverse deltas, CustomData columns, Interp propagation, `classify()`, subdivide n-gon fix, `GenTags`/element selection, the op-history DAG, or boolean/remesh providers — those are Phases 2–8. Do NOT add a serialized field this phase (harness stubs only). Do NOT edit `half_edge.rs`. Fix one thing per plan; flag (don't fix) any duplication or other bug you notice. If a step can't be done within these red lines, STOP and report.
