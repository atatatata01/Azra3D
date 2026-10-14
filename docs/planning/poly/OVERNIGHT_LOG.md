# Overnight Build Log — Poly Foundation (feat/poly-foundation)

**Run start:** 2026-06-30 (overnight, unattended)
**Authority:** full bypass; scope = push while gates stay green; isolated worktree, never push/main.
**Engine:** sequential phase agents in this worktree; main agent verifies (build + test + gates + bandaid scan) between phases. Only green+clean advances.
**Base commit:** 7181a14 (off 3a4934a). Branch: feat/poly-foundation.

## Gate contract (a phase is "done" only if all green)
- `cargo build -p app` clean
- `cargo test -p core_math` clean
- phase-specific gates: bevel `apply→inverse→apply==identity` proptest, golden-topology, `validate()`, bincode round-trip + sentinel guard
- bandaid scan clean (no downgraded asserts, no `serde(skip)` on mesh, no new `HalfEdgeMesh` field, no 2nd FL4ORDER magic, no magic constants)

## STOP conditions
- build red and not fixable in a couple tries → stop, log, do not pile on
- a required gate red and not greenable correctly → stop, log
- a fix would require a forbidden (bandaid / red-line) action → stop, log as BLOCKED

---

## Timeline

- **[setup]** worktree created, plan + P0–P8 prompts committed (7181a14). P0 agent dispatched.

---

## 2026-06-30 — Phase P0 COMPLETE (Decide, unfreeze-scoped, scaffold spine, consolidate bevel)

**Implemented (7 ordered steps, build + test after each):**
- **A** — CLAUDE.md: recorded the HEX-BL decision + scoped/gated `half_edge.rs` freeze-lift (bevel/mesh_ops need ZERO edits; the only future edits are (a) `classify()` as a NEW method leaving `validate()` intact, (b) per-op parent-map/inverse-delta, (c) subdivide n-gon — each its own later gate). bmesh-rs STUDY-ONLY. Docs only.
- **B** — `core_math/src/poly_mesh.rs`: non-serialized `PolyMesh { topo, cd, topo_index, topo_gen }` runtime wrapper; EMPTY `CustomData`/`TopoIndex`; NO `GenTags`. `set_topo()` bumps `topo_gen`.
- **C** — `core_math/src/mesh_ops/` + `MeshOp` trait (exactly per §4.7) + `Handle{id,gen}`, `OpError{NonManifoldUnsupported{edge},…}`, `EditResult`, `TopoDelta`, `OpDelta::Snapshot`. `BevelOp` delegates to `bevel_op` (ONE path).
- **D** — `mesh_ops/test_spine.rs`: `mesh_eq(deep)` oracle (positions+face-rings+CustomData, asserts DENSE input), bincode **sentinel-variant guard**, proptest random-mesh `validate()` fuzz, FL4 round-trip stubs (`#[ignore]` → Phase 3). Added `proptest` workspace+core_math dev-dep.
- **E** — `bevel_op.rs` v1: offset-rail + `WeldMap` (replaces the `mesh.clone()` stub). Constant in-plane perpendicular offset, corner line-intersection (welded shared), 4-case affected-face re-trace, **reversed-winding** chamfer strip (the watertight fix), corner-patch n-gon at k≥2. ZERO `half_edge.rs` edits.
- **F** — re-routed `poly_drag_tools.rs:709` from frozen `bevel_edges_ex` → `bevel_edges_offset`. Two app bevel paths → ONE. Frozen `bevel_edges_ex` + its in-file tests kept (old-path safety net); zero LIVE callers remain.
- **G** — `apply→inverse→apply==identity` proptest (96 cases, random mesh+edge-subset+width) via `mesh_eq(deep)`.

**Commits (this branch, never pushed):**
- `d937285` docs(poly): record HEX-BL decision + scoped/gated half_edge.rs freeze-lift in CLAUDE.md
- `4a0c7a0` feat(poly): scaffold non-serialized PolyMesh runtime wrapper (core_math)
- `972b943` feat(poly): scaffold mesh_ops/ module + typed MeshOp trait (core_math)
- `8d4366f` test(poly): stand up the mesh-op test spine skeleton (core_math §4.5)
- `f939f38` feat(bevel): complete bevel_op.rs v1 — offset-rail + WeldMap (replaces clone stub)
- `01aa08c` refactor(bevel): retire the wrong-algorithm bevel_edges_ex caller — two paths collapse to ONE
- `0d7f8b3` test(bevel): add the apply→inverse→apply==identity proptest gate (mesh_eq deep)
- (+ this log/tracker update commit)

**Gate status (all GREEN):**
- `cargo build -p app` — clean (pre-existing warnings only).
- `cargo test -p core_math` — 275 passed, 0 failed, 5 ignored (4 FL4 stubs + 1 pre-existing).
- Golden-topology gate — single-edge cube 8→10 verts / 6→7 faces; watertight edge-loop (0 boundary edges); bevel-all manifold; parallel rails. via `validate()` ✅
- Bevel `apply→inverse→apply==identity` proptest — GREEN via `mesh_eq(deep)` ✅
- Bincode sentinel-variant guard — GREEN (no non-skip `HalfEdgeMesh` field) ✅; `arc_serde_roundtrip` — GREEN ✅
- proptest random-mesh `validate()` fuzz — GREEN ✅

**Red lines held / bandaid scan clean:** `half_edge.rs` UNCHANGED (0 edits, verified via `git diff`); zero new `HalfEdgeMesh` fields; `PolygonalBody.mesh` NOT serde-skipped; no new `NodePayload` variant; no 2nd `FL4ORDER` magic; no new serialized field; no `GenTags`; no downgraded asserts / `catch_unwind` / NaN-clamp / borrow-dodge clone in new code.

**BLOCKED items:** none.

**Notes for the P1 agent:** P0 already delivered bevel v1 + the `bevel_edges_ex` retirement + the identity proptest gate. Per `PHASE_P1.md` Step 0, Steps A–B there become VERIFY-and-finalize; the load-bearing P1 work is confirming bevel is the canonical `MeshOp` template (done — `mesh_ops/bevel.rs` delegates) and that the hard merge gate is green (done). One known v1 scope limit recorded in the tracker: axis-aligned degenerate corners (two side rails coincide) emit no patch (correct/watertight now); the CUTOFF fallback belongs to v2 when segments/profile land.

- **[orchestrator-verify P0 — 2026-06-30]** INDEPENDENTLY VERIFIED GREEN: `half_edge.rs` 0 edits (git diff vs 3a4934a empty); `cargo build -p app` clean; `cargo test -p core_math` passed; red-line/bandaid scan clean. Advancing to P1.

---

## 2026-06-30 — Phase P1 COMPLETE (Bevel v1 — the merge-blocker; VERIFY-and-finalize)

**STEP-0 audit (mandatory, ran FIRST):** Per `PHASE_P1.md`'s own STEP-0 instruction, because P0 already landed bevel, Steps A–B are VERIFY-and-finalize. Independently audited the LIVE tree (not the P0 log's claims) against all 5 P1 steps + the 8 acceptance criteria:

- **Step A — bevel v1 real (no `mesh.clone()` stub):** ✅ `bevel_op.rs::bevel_edges_offset` is the full offset-rail + `WeldMap` algorithm (constant in-plane perpendicular offset, corner line-intersection welded to one shared index, affected-face 4-case re-trace, reversed-winding chamfer strip, corner-patch n-gon at k≥2). The two remaining `mesh.clone()` are the CORRECT zero-width / empty-selection identity early-returns (`:87`, `:112`), not the stub. TODO at old `:77` gone. ZERO `half_edge.rs` edits.
- **Step B — `bevel_edges_ex` caller retired (two paths → ONE):** ✅ `poly_drag_tools.rs:716` calls `core_math::bevel_op::bevel_edges_offset`. `git grep bevel_edges_ex` over `core_engine/**/*.rs` returns ONLY: the frozen def + in-file tests in `half_edge.rs` (old-path safety net) and descriptive COMMENTS in `poly_drag_tools.rs:617/:710` + `lib.rs:16`. **Zero LIVE callers.**
- **Step C — bevel is the canonical `MeshOp` template:** ✅ `mesh_ops/bevel.rs::BevelOp` impls `MeshOp` with `type Params = BevelOpParams`, `const PROPAGATES = false` (declares the layer drop loudly per §4.2), `fn apply -> EditResult { mesh, parents: TopoDelta::new(), inverse: OpDelta::Snapshot }`, DELEGATING to `bevel_op::bevel_edges_offset` (ONE geometry path, no duplicate). Result is `validate()`-guarded (No-Bandaids), `topo_gen` bumped via `set_topo`.
- **Step D — app glue on `transaction()`/episode:** ✅ `poly_activate_bevel` → `begin_episode("Bevel Edge")`; `poly_update_bevel_drag` mutates the body `Arc<HalfEdgeMesh>` each frame via the ONE path; `poly_confirm_bevel` → `commit_episode()` (ONE coalesced undo record per drag); `poly_cancel_bevel_drag` → `cancel_episode()`. Kernel op stays pure; glue owns undo.
- **Step E — HARD MERGE GATE + golden topology:** ✅ `test_spine.rs::bevel_apply_inverse_apply_is_identity` (96 cases: random manifold mesh + random interior-edge subset + random width; asserts `mesh_eq(deep)` for both inverse-restore and apply==apply-after-restore, NOT `validate()` alone). Golden topology in `bevel_op.rs` tests: `cube_single_edge_bevel_golden` (8→10 verts, 6→7 faces, manifold), `edge_loop_corner_patch_watertight` (8→12 verts, 0 boundary edges), `bevel_all_edges_manifold` (0 boundary), `parallel_rails`, `boundary_edge_skipped`, `zero_width_is_identity`. Degenerate/parallel corner handled explicitly in `solve_corner_lines` (denom < 1e-9 → averaged-offset fallback, NO NaN, NO clamp-to-hide).

**AUDIT RESULT:** P1 is FULLY SATISFIED by what P0 landed. Per `PHASE_P1.md` ("if P1 is already fully satisfied by P0, say so explicitly and finalize — don't invent work") + the Scope Guard (P1 ONLY, fix one thing, no P2+), NO new code was written. Finalization only.

**Finalized (docs only — no code change):**
- Recorded "HARD MERGE GATE closed: Phases 2+ unblocked" in `COMPLETE_BUG_TRACKER.md` (Definition of Done #6) with the re-verified gate status.
- This P1 log entry.

**Gate status (all GREEN, independently re-run at P1):**
- `cargo build -p app` — clean (82 pre-existing warnings only, 0 errors).
- `cargo test -p core_math` — **275 passed, 0 failed, 5 ignored** (4 FL4 Phase-3 stubs + 1 pre-existing). `bevel_apply_inverse_apply_is_identity` ✅, `random_mesh_validates` ✅, `mesh_eq_reflexive` ✅, `sentinel_after_halfedgemesh_reads_back` ✅, `halfedgemesh_bincode_roundtrip` ✅, all `bevel_op` golden tests ✅.
- Validation gate — every bevel output passes `validate()` (proptest never produces a failing result) ✅
- Bevel `apply→inverse→apply==identity` proptest (96 cases, `mesh_eq` deep) ✅
- Golden-topology gate (cube single-edge + edge-loop + bevel-all) ✅
- Bincode sentinel-variant guard + `arc_serde_roundtrip` ✅

**Red lines held / bandaid scan clean:** `half_edge.rs` UNCHANGED (git diff vs 3a4934a empty); zero new `HalfEdgeMesh` fields; `PolygonalBody.mesh` NOT serde-skipped; no new `NodePayload` variant; no 2nd `FL4ORDER` magic; no new serialized field; no `GenTags`; bevel `PROPAGATES = false` declares the attribute drop loudly (not silent/faked); no downgraded asserts / `catch_unwind` / NaN-clamp / borrow-dodge clone introduced. No P2+ work (no inverse-delta for the other 10 ops, no CustomData, no `classify()`, no subdivide fix).

**BLOCKED items:** none.

**Rule-conflict check:** none. `PHASE_P1.md` self-consistently anticipates the P0-overlap case in its own Step 0 / Rules Block; finalizing without re-doing landed work is exactly what it instructs.

**Single working tree:** all verification + the doc edits happened in the `poly-foundation` worktree on `feat/poly-foundation`. Never pushed; `main` and other worktrees untouched.

---

## 2026-06-30 — Phase P2 COMPLETE (Euler-op inverse deltas — delta undo, Gate 2a)

**STEP-0 audit (mandatory, ran FIRST — searched the LIVE tree):**
- **The 11 Euler ops** — all `&mut self` in-place, none emit a parent-map/delta. `split_edge` (`half_edge.rs:404`), `extrude_face` (`:499`), `merge_vertices` (`:557`), `delete_face` (`:578`), `inset_face` (`:598`), `loop_cut` (`:629`), `dissolve_edge` (`:717`), `dissolve_face` (`:780`), `flip_face` (`:793`), `subdivide_catmull_clark` (`:1772`), `subdivide_faces` (`:1925`). Mutation character: split/extrude/loop_cut are **ADD-ONLY** (alloc at tail, no free-list push); flip_face is **in-place only**; inset/merge/delete/dissolve_edge/dissolve_face **push to the PRIVATE free-lists**; the two subdivide ops `*self = from_polygons(...)` (whole-mesh rebuild).
- **`free_verts`/`free_edges`/`free_faces` are PRIVATE** (`half_edge.rs:66-68`, no `pub`, no accessor). The decisive constraint: an external `OpDelta` cannot reconstruct the free-list → element-delta is only safe for the add-only/in-place ops; free-list-mutating ops honestly fall to `Snapshot` (red line forbids a `half_edge.rs` accessor edit).
- **Phase-0/1 scaffold:** `OpDelta::Snapshot`, `MeshOp`, `EditResult`, `TopoDelta`, `OpError`, `Handle` (`mesh_ops/mod.rs`) — EXTENDED, not redefined. `mesh_eq`/proptest/`arb_manifold_mesh` (`test_spine.rs`) — EXTENDED. `bevel.rs` = canonical template — followed.
- **App glue:** poly ops live in `app/src/poly/*.rs` (NOT a `mesh_actions.rs` — that file does not exist), each already inside `transaction()`. Undo = `app_types::UndoPayload` (`Full`{graph clone, Arc-COW mesh} / `Delta`{prim}) consumed by `undo.rs:apply_undo_entry`. `transaction`/`begin_episode`/`commit_episode` at `undo.rs:295/306/329`. **`save_undo`/`save_delta_undo` CONFIRMED DELETED** (0 matches). `Arc<HalfEdgeMesh>` + `make_mut` substrate confirmed (`scene_graph.rs`, `arc_serde_roundtrip` green).
- **No-duplication:** `capture_element_delta` / `run_add_only` / `run_snapshot` / `PolyEdit::apply_in_place` are NEW (searched: none equivalent existed; the closest, `OpDelta::Snapshot`, is reused for the snapshot tier). App `poly_op_batch` is NEW glue routed through the EXISTING undo stack — no parallel path.

**Per-op delta strategy (the honest cost split, plan §4.6 / Risk #3):**
- **O(touched) `ElementDelta`** (in-place `*_mut`, add-only/in-place, NO free-list mutation): **split_edge, extrude_face, loop_cut, flip_face** (`mesh_ops/local_ops.rs`). Inverse = restore overwritten pre-existing slots + truncate the appended tail. Capture by EXTERNAL OBSERVATION (`capture_element_delta`) — ZERO `half_edge.rs` edits. Gated dense-input (vert+face density via public count API); sparse input falls back to `Snapshot` (declared loudly, not silent).
- **Honest O(body) `Snapshot`** (free-list-mutating): **inset_face, merge_vertices, delete_face, dissolve_edge, dissolve_face** (`mesh_ops/local_rebuild_ops.rs`). The PRIVATE free-list cannot be rebuilt externally → per-BODY before-image is the correct inverse (still cheaper than Blender's O(scene)). Cost tier encoded in the variant + documented, NOT faked.
- **Honest O(body) `Snapshot`** (whole-mesh rebuild): **subdivide_catmull_clark, subdivide_faces** (`mesh_ops/subdivide.rs`). Wrapped AS-IS (NO n-gon fix — that is Phase 5).

**Implemented (Steps A–G, build + `cargo test -p core_math` after EACH):**
- **B** — `OpDelta::Element(ElementDelta{prev_lens, changed_verts/edges/faces})` + `OpDelta::apply_inverse` + `capture_element_delta`. Made `mesh_eq` free-list-aware (active geometry by face-reachability, public API — the P0-anticipated "free-aware compaction") so sparse meshes from delete/dissolve/merge compare correctly.
- **C** — `SplitEdgeOp`, `ExtrudeFaceOp` (+ `LoopCutOp`/`FlipFaceOp`) as typed `MeshOp`s, `run_add_only` → element delta.
- **D** — `InsetFaceOp`, `MergeVerticesOp`, `DeleteFaceOp`, `DissolveEdgeOp`, `DissolveFaceOp` (`run_snapshot`).
- **E** — `SubdivideCatmullClarkOp`, `SubdivideFacesOp` + the golden-topology gate.
- **F** — `PolyEdit` typed in-place dispatch + `apply_in_place` (the SINGLE app entry — runs the op on `Arc::make_mut`, returns the inverse, no extra clone). `UndoPayload::Poly{nid, ops: Vec<(PolyEdit, OpDelta)>}` (APPENDED to the runtime-only enum), symmetric batch undo/redo arm in `apply_undo_entry`, `poly_op`/`poly_op_batch` helpers. Migrated the menu/keyboard tools — extrude, inset, delete, dissolve(edge+face), merge, loop_cut(single+interactive), flip-normals, subdivide(cc+faces) — to the delta path. Drag/knife/bridge tools kept on the working episode path (same undo stack).
- **G** — `topo_gen` no-op-replay guard test; strengthened `mesh_eq` to also assert `topo_gen` equality (the deep oracle = positions + face rings + CustomData + topo_gen).

**Commits (this branch, never pushed):**
- `c9294d9` feat(poly): P2 Step B — extend OpDelta with O(touched) ElementDelta + apply_inverse
- `ce7e441` feat(poly): P2 Step C — wrap split_edge + extrude_face as O(touched) ElementDelta ops
- `107915b` feat(poly): P2 Step D — wrap the remaining 7 local Euler ops with inverse deltas
- `cf3a930` feat(poly): P2 Step E — wrap subdivide ops (O(body) Snapshot) + golden-topology gate
- `3fc50ce` feat(poly): P2 Step F — wire app glue, delta-undo Euler ops through the existing undo stack
- `9e28b30` feat(poly): P2 Step G — topo_gen no-op guard + strengthen mesh_eq deep oracle
- (+ this log/tracker update commit)

**Build + test status:** `cargo build -p app` — clean (pre-existing warnings only, 0 errors). `cargo test -p core_math` — **303 passed, 0 failed, 5 ignored** (the 5 ignored = Phase-3 FL4 `PolyLayerRecord` stubs).

**Gate status (all GREEN):**
- **Per-op identity (11/11):** `{split_edge, extrude_face, inset_face, loop_cut, merge_vertices, delete_face, dissolve_edge, dissolve_face, flip_face, subdivide_catmull_clark, subdivide_faces}_apply_inverse_apply_is_identity` — each GREEN via `mesh_eq(deep)` (positions + face rings + CustomData + topo_gen), inverse applied to the POST-op mesh (real Ctrl+Z semantics), NOT `validate()` alone. ✅
- **Golden-topology (5):** split/extrude/delete/dissolve_edge/subdivide_cc on a cube — exact vert/edge/face counts + `validate()` after apply AND the same pre-op counts + `validate()` + `mesh_eq` after inverse (deterministic free-list reuse). ✅
- **Honest-cost:** in-code test asserts split's inverse stores ≤8 changed slots (O(touched), no whole-mesh clone); `PolyEdit::is_element_delta` + the variant encode the split; `poly_edit_in_place_round_trips_and_picks_honest_tier` + `poly_edit_batch_undo_redo_cycle` verify the app-path tiers + ordering. ✅
- **Bincode safety:** `sentinel_after_halfedgemesh_reads_back` ✅, `arc_serde_roundtrip` ✅. `half_edge.rs` **byte-for-byte UNCHANGED** (`git diff 6b5e757 HEAD -- half_edge.rs` empty). `UndoPayload` derives only `Clone` — NEVER serialized (undo is in-memory). ✅
- **bevel gate + proptest fuzz** stay GREEN. ✅

**Red lines held / bandaid scan clean:** `half_edge.rs` 0 edits (verified empty diff); zero new `HalfEdgeMesh` fields; `Vec<PolyModifier>` not retyped; `PolygonalBody.mesh` not serde-skipped; no new `NodePayload`/`PolyModifier` variant; no 2nd `FL4ORDER` magic; no new SERIALIZED field (the `UndoPayload::Poly` variant is runtime-only, appended at the enum END); no `GenTags`; no CustomData/Interp/parent-map (`PROPAGATES = false` on every op — Gate 2a); no `classify()`; no subdivide n-gon fix. No downgraded asserts (every op `validate()`-guards its result — a real bug fails the proptest, never silenced), no `catch_unwind`, no NaN-clamp, no magic constant, no clone-to-dodge-borrow (read→decide→write splits in the app glue). The element-delta capture is O(body) TRANSIENT (the external-observation diff) but the STORED delta + undo-apply are O(touched) — documented honestly, the gate (no whole-mesh clone ON THE INVERSE) is met.

**BLOCKED items:** none.

**Rule-conflict check:** none. P2 honored the scoped `half_edge.rs` freeze (zero edits — the prompt's mandate, achieved via external-observation deltas). The plan's "in-place `*_mut` element-delta REQUIRED for split/extrude" is satisfied: those ops mutate in place and emit `OpDelta::Element` (the stored delta + replay are O(touched)); the free-list-private constraint that forces the other free-list-mutating ops to `Snapshot` is the HONEST cost model the plan §4.6/Risk #3 explicitly prescribes — not a deviation.

**Manual test scenario (for the operator):** In a poly body, select one face → Extrude → Ctrl+Z: the mesh restores exactly (the `Poly` undo arm applies `OpDelta::Element` — O(touched), NO whole-body clone). Then with nothing selected → Subdivide (Catmull-Clark) → Ctrl+Z: restores via the per-body `OpDelta::Snapshot` (O(body), honest). Redo re-runs each op. A multi-face Delete/Dissolve is ONE Ctrl+Z (batch entry).

**Notes for the P3 agent:** P2 left `TopoDelta` inert (Gate 2a — no parent-map). The interactive drag-extrude/inset (`poly_drag_tools.rs`), knife (`poly_knife.rs`), and the composite Bridge (`poly/bridge.rs:125` flip_face) still use the EXISTING episode/`transaction` whole-graph-snapshot undo (correct, same stack) — migrate them to `poly_op`/`poly_op_batch` when their episode coalescing is ported (not P2 scope). The 5 FL4 stubs unblock in Phase 3 with `PolyLayerRecord`.

**Single working tree:** all code + doc edits in the `poly-foundation` worktree on `feat/poly-foundation`. Never pushed; `main` and other worktrees untouched.

---

## 2026-06-30 — Phase P3 COMPLETE (CustomData sidecar Copy/Constant + UV/sharp bake + FL4 PolyLayerRecord)

**STEP-0 audit (mandatory, ran FIRST — searched the LIVE tree; "found X at Y:Z" / "none found"):**
- **Phase 0/2 scaffolds EXTENDED, not redefined:** `struct PolyMesh` + empty `CustomData`/`TopoIndex`/`topo_gen` (`poly_mesh.rs:67`), `MeshOp`/`EditResult`/`TopoDelta`/`OpDelta`/`OpError`/`Handle` (`mesh_ops/mod.rs`), `ExtrudeFaceOp`/`InsetFaceOp` (`local_ops.rs`/`local_rebuild_ops.rs`), `mesh_eq`/`arb_manifold_mesh`/proptest/sentinel guard (`test_spine.rs`). Confirmed the wrapper already had an EMPTY `CustomData` field — this phase FILLED it (no 2nd wrapper, no 2nd CustomData type).
- **Column/Interp primitives: `none found` (expected).** `LayerStore`/`BitLayer`/`LayerKey`/`DynLayer`/`Interp`/`EdgeKey`/`UndirectedEdgeKey` existed ONLY as comments in `poly_mesh.rs` — NO live types. Created this phase. (Plan/prompt name `UndirectedEdgeKey`; the live undirected key is `EdgeKey(min,max)` — same concept; `none found` for a separate `UndirectedEdgeKey` type.)
- **`to_raster_data` confirmed `half_edge.rs:1376`;** UV slot `0.0, 0.0` at `:1393-1394`; per-corner emit over `face_vertices(fid)` (`:1390`); face-normal-per-corner. READ from the wrapper layer — NOT edited (red line). Decision: a NEW `PolyMesh::to_raster_data_with_cd` iterates `face_half_edges` in lockstep (corner = half-edge, its dst vertex = the corner vertex) so the UV column keys on `HalfEdgeId`.
- **Id types confirmed** (`half_edge.rs`): `VertexId:13`, `HalfEdgeId:16`, `FaceId:19`; accessors `face_vertices:177`, `face_half_edges:194`, `compute_face_normal:278`, `compute_vertex_normal:288`, `he_source:138`, `vertex_fan:157`. Reused; no id type re-derived.
- **extrude/inset structure** (the Copy correctness anchor): `extrude_face` (`:499`) REUSES the source face's half-edge slots for the top face (their UV stays valid automatically) + appends side-wall verts/edges at the tail. `inset_face` (`:598`) `delete_face`s the source (frees its corner slots) → `create_face` REUSES them via the free-list — so the source corners' UV entries go STALE and MUST be cleared (the silent-corruption hazard). Both append new verts at the tail in source order (`new_vert[i]` ← `src_ring[i]`).
- **FL4 anchors confirmed:** `ProjectFile` is a STRUCT (`project.rs:16`); `voxel_grids: #[serde(default)] Vec<VoxelNodeRecord>` (`:31`); `VoxelNodeRecord { node_id:u64, grid }` (`:60`), reattached by `node_id` (`project_load.rs:205`, collected `project_save.rs:125`). `FL4ORDER` side-channel (`project.rs:308`): `child_orders` is `#[serde(skip)]` (`:43`), `load_binary` does `deserialize_from(&mut cursor)` then sniffs the magic tail (`:347-357`). 8MB-stack `save_binary`/`load_binary` (`:310/:335`). `PolyLayerRecord`: `none found` — created this phase mirroring `VoxelNodeRecord`. `sentinel_after_halfedgemesh_reads_back` + `arc_serde_roundtrip` confirmed green at baseline.
- **App-side store: `none found`** — no app stored `PolyMesh`/`CustomData` per body (the node holds `Arc<HalfEdgeMesh>` only). Added `cache.poly_custom_data: HashMap<NodeId, CustomData>` (the FEATURE_RULES Q1 "app/cache state" home), reattached on load. **No `mesh_actions.rs` exists** (P2 log confirmed; poly glue lives in `app/src/poly/*`). P3 wires bake + persistence; runtime layer-AUTHORING UI / OpDelta `layer_changes` integration is left for a later phase (noted below — P3 has no authoring UI to route through `transaction()` yet).
- **The "5 ignored FL4 stubs" reconciliation:** there are **4** `#[ignore]`d FL4 round-trip stubs (`test_spine.rs::fl4_roundtrip_stubs`). The 5th ignored test the P2 log/orchestrator counted is `voxel_sculpt::tests::bench_mesh_region_192` — an UNRELATED perf probe, NOT an FL4 stub. PHASE_P3.md authoritatively says "the four FL4 tests" (Steps E/Acceptance/DoD). Resolved all **4** FL4 stubs; left the voxel bench `#[ignore]` (out of scope). The hard-stop's "5" is an off-by-one; the FL4 round-trip + sentinel substance is fully met.

**Implemented (Steps A–F, build + `cargo test -p core_math` after EACH; `cargo build -p app` after each app touch):**
- **A** — Filled `CustomData` (`poly_mesh.rs`): `uv`/`seam`/`sharp` (Corner), `crease` (Edge via `EdgeKey`), `vcolor`/`vcrease` (Vertex), `extra` (`LayerKey→DynLayer`); `Interp ∈ {Constant, Copy}` ONLY. `LayerStore` (sparse typed-key), `BitLayer`, `columns_eq` (byte-equal oracle). ZERO `HalfEdgeMesh` fields.
- **B** — `mesh_ops/propagate.rs`: `copy_propagate_corner_uv` + `append_tail_parent_map` (extrude) + `copy_propagate_inset_uv` + `inset_region_corners` (inset, clears stale slots). `ExtrudeFaceOp`/`InsetFaceOp` → `PROPAGATES=true`, emit their OWN parent-map; all others stay `PROPAGATES=false` (loud-drop flag). Bounded to the touched set (never a full-column sweep). `uv_preserved_after_extrude`/`_after_inset` + empty-stays-empty tests.
- **C** — `PolyMesh::to_raster_data_with_cd` (`poly_mesh.rs`): overlay uv into the `[0,0]` slot + sharp-driven split/averaged normals (`smooth_corner_normal`), reusing the kernel accessors (NO `half_edge.rs` edit). Empty sidecar DELEGATES to the kernel `to_raster_data` → byte-identical. App: `cache.poly_custom_data` + `raster_render.rs::ensure_poly_meshes` calls the wrapper method only for authored bodies (zero cost otherwise; existing mesh-cache generation counter invalidates the bind group). Golden bake gates: byte-identical (box/sphere/cyl), uv overlay, sharp split-vs-averaged.
- **D** — `#[serde(default)] poly_layers: Vec<PolyLayerRecord>` on `ProjectFile` (in-blob, among the serialized fields, BEFORE the `#[serde(skip)] child_orders`). `PolyLayerRecord { node_id, uv, crease(EdgeKey), seam, sharp }` mirrors `VoxelNodeRecord`. `EdgeKey: Serialize/Deserialize`. `CustomData::to_layer_tuples`/`from_layer_tuples` (deterministic sorted). Save: collect non-empty sidecars in `from_app`. Load: reattach into `cache.poly_custom_data` by `node_id`.
- **E** — Un-`#[ignore]`d the 4 FL4 stubs (real CustomData↔record bincode round-trips, the part core_math owns) + added `app/src/project.rs::poly_layer_save_tests` (the ProjectFile-level `save_binary`/`load_binary` gates).
- **F** — `mesh_eq` now calls `cd.columns_eq` (byte-equal every column, not just `is_empty()` parity). Copy/Constant propagation proptest (96 cases): extrude/inset preserve pre-existing UV + Copy onto new corners, no NaN, no silent zero, no stale-slot corruption.

**The FL4 persistence MECHANISM used (confirm it mirrors `voxel_grids`):** authored CustomData rides a NEW `#[serde(default)] Vec<PolyLayerRecord>` field on the `ProjectFile` STRUCT — **EXACTLY** the `voxel_grids: Vec<VoxelNodeRecord>` precedent: a trailing serialized `#[serde(default)]` field on a struct (forward/backward-compatible), each record keyed by `node_id`, collected on save by iterating the runtime store (`cache.poly_custom_data`, the analogue of iterating `VoxelBody` nodes), reattached on load by `node_id`. It rides IN-BLOB (consumed by `deserialize_from` BEFORE the FL4ORDER tail sniff), so it is **NOT** a 2nd trailing magic and child_order + poly layers coexist losslessly. Zero `HalfEdgeMesh` field; `PolygonalBody.mesh` not serde-skipped; no new `NodePayload` variant; `Vec<PolyModifier>` not retyped.

**Commits (this branch, never pushed):**
- `4c849b0` feat(poly): P3 Step A — fill CustomData SoA sidecar (Copy/Constant Interp only)
- `8af8029` feat(poly): P3 Step B — Copy/Constant UV propagation over the touched delta (extrude/inset)
- `ad3059b` feat(poly): P3 Step C — wire uv + sharp into the Object-Mode bake (wrapper method)
- `eab3054` feat(poly): P3 Step D — PolyLayerRecord in-blob FL4 field (mirrors voxel_grids exactly)
- `60463bc` test(poly): P3 Step E — promote the 4 FL4 round-trip stubs to live + ProjectFile gates
- `e0b4bbe` test(poly): P3 Step F — mesh_eq(deep) compares CustomData columns + propagation proptest
- (+ this log/tracker update commit)

**Build + test status:** `cargo build` (full workspace) — clean (pre-existing warnings only, 0 errors). `cargo test -p core_math` — **320 passed, 0 failed, 1 ignored** (the 1 ignore = the unrelated `bench_mesh_region_192` voxel perf probe). `cargo test -p app` — **65 passed, 0 failed, 0 ignored**.

**FL4 round-trip gates + sentinel — EXPLICIT pass/fail (the HARD STOP):**
- **empty/no-poly-layer ProjectFile round-trips byte-identical (old-save compat):** PASS — `empty_poly_layers_roundtrips_byte_identical` (no FL4ORDER magic written; loads empty poly_layers).
- **BOTH authored `child_order` AND poly layers round-trip (the exact §0 corruption case):** PASS — `both_child_order_and_poly_layers_round_trip` (FL4ORDER tail present, in-blob poly layer + side-channel child_order both decode; columns + interleave intact).
- **a poly mesh with UV/sharp layers round-trips with the layers intact:** PASS — `authored_poly_body_layers_survive_roundtrip` (uv+sharp+crease+seam survive save→load via `columns_eq`).
- **forged pre-v8 "no poly_layers" loads with default-empty (format-stability):** PASS — `forged_pre_v8_loads_empty_poly_layers`.
- **4 core_math FL4 stubs un-`#[ignore]`d:** PASS — `authored_body_roundtrip`, `pre_record_file_loads_empty_customdata`, `both_child_order_and_poly_layers_load`, `forged_v7_no_magic_byte_identical` (now all run; 0 FL4 stubs ignored).
- **bincode sentinel-variant guard:** PASS — `sentinel_after_halfedgemesh_reads_back` (proves no non-skip `HalfEdgeMesh` field).
- **`arc_serde_roundtrip`:** PASS (`PolygonalBody.mesh` not serde-skipped, round-trips through bincode).
- **3 existing `child_order_save_tests`:** PASS (the v7-blob + v8-interleave + node-id-key tests — adding `poly_layers` did NOT break the existing format).

**Which of the 5 ignored stubs now pass:** the **4** FL4 round-trip stubs (above) are un-ignored and pass. The 5th ("ignored") test is `voxel_sculpt::tests::bench_mesh_region_192`, a perf probe UNRELATED to FL4 — left `#[ignore]` (out of P3 scope). So core_math goes from 5 ignored → **1 ignored** (bench only).

**Red lines held / bandaid scan clean:** `half_edge.rs` **byte-for-byte UNCHANGED** (`git diff 81315db HEAD -- half_edge.rs` empty); zero new `HalfEdgeMesh` fields (sentinel green); `PolygonalBody.mesh` not serde-skipped (arc_serde green); no 2nd FL4ORDER magic (poly_layers is in-blob); no new `NodePayload`/`PolyModifier` variant; `Vec<PolyModifier>` not retyped; `poly_layers` is `#[serde(default)]` (forward/backward compat). No `Lerp`/`Average` Interp (Stage A only); no `classify()`; no subdivide n-gon fix; no `GenTags`/element selection. No downgraded asserts (the propagation `debug_assert`s GUARD against a wrong op-supplied parent-map — they catch real bugs, never silence one); no NaN-clamp (the proptest asserts finiteness, the math is correct, not clamped); no `catch_unwind`; no magic constant; no clone-to-dodge-borrow (the wrapper bake clones the mesh ONLY for authored bodies — a real transient input to the pure raster method, not a borrow dodge). The non-propagating ops keep `PROPAGATES=false` (the UI-surfaceable loud-drop flag) rather than silently zeroing a column.

**Manual test scenario (for the operator):** Author a UV layer on a poly cube + mark one cube edge `sharp` (via the runtime sidecar `cache.poly_custom_data`), save → reload: the UVs and the hard-edge split survive (the FL4 `PolyLayerRecord` round-trip; the Object-Mode bake reads them via `to_raster_data_with_cd`). Extrude a face whose corners carry UV: the new side-wall corners inherit the source corner's UV (Copy-propagation over the touched delta); the top face keeps its UV exactly. A scene with NO authored poly layers saves to a file byte-identical to pre-v8 and bakes byte-identical to the old `to_raster_data`.

**BLOCKED items:** none. All FL4 round-trip + sentinel + arc_serde gates are GREEN; the save format is intact (the 3 existing child_order tests confirm no regression). The save format was NEVER left in a partial/uncertain state.

**Rule-conflict check:** none. P3 honored the scoped `half_edge.rs` freeze (zero edits — the bake wiring lives on the wrapper). The one discrepancy found (the hard-stop's "5 ignored FL4 stubs" vs the actual 4 + 1 unrelated bench) is a count off-by-one in the orchestrator summary, NOT a CLAUDE.md rule conflict; PHASE_P3.md ("four FL4 tests") governs and is satisfied. Reported in STEP-0 above, not silently overridden.

**Notes for the P4 agent:** CustomData undo is NOT yet wired into the app undo stack — `OpDelta` has no `layer_changes` field (the prompt's "already a field on `OpDelta`" was aspirational; it is `Element`/`Snapshot` only). P3 has no runtime layer-AUTHORING UI, so there is nothing yet to route through `transaction()`; the propagation lives inside the op `EditResult.mesh.cd`. When layer-authoring UI + the OpDelta `layer_changes` path land (P4+), route CustomData edits through the existing `transaction()`/episode model and persist via the same `cache.poly_custom_data` ↔ `PolyLayerRecord` path. The non-propagating ops (split/loop_cut/flip/delete/dissolve/merge/subdivide) keep `PROPAGATES=false` until their per-op parent-map ships (Phase 4, Gate 2c).

**Single working tree:** all code + doc edits in the `poly-foundation` worktree on `feat/poly-foundation`. Never pushed; `main` and other worktrees untouched.

---

## 2026-06-30 — BEVEL FLIPPED-FACE FIX (interactive-test finding; owner-authorized kernel edit)

**Symptom (live app, partial edge bevel):** a beveled face shaded inverted (flipped normal). Caught by manual testing — the automated P0/P1 gates passed it.

**Root cause:** `half_edge.rs::compute_face_normal` used the first-3-vertex cross product `(p1-p0)×(p2-p0)`, which returns the WRONG sign at a reflex corner. Bevel emits non-convex faces (the `(false,false)` un-beveled-but-adjacent re-traced faces, `bevel_op.rs:310-339`), so their shading normal flipped. The gate blind spot: `validate()` checks twin SYMMETRY not DIRECTION, and the watertight test counts boundary edges — neither checks a normal. (Topology was correct; only the computed normal was wrong — confirmed via `from_polygons`' reverse-direction twinning, which would have produced boundary edges on a true winding conflict.)

**Fix (rigorous, reproduce-first):**
1. Added `bevel_op::tests::bevel_output_faces_all_point_outward` (3 configs incl. the single-edge case). Ran it FIRST against the old normal → FAILED `config 0 face 4: normal (1,0,0) dot=-1.0000` (the exact flip, reproduced in code).
2. Switched `compute_face_normal` to **Newell's method** (area-weighted, sign-correct for any non-planar/non-convex polygon; sign matches the old form for convex CCW faces → existing meshes unchanged). Owner-authorized scoped-lift edit (d), recorded in CLAUDE.md.
3. Re-ran → gate PASSES; full `cargo test -p core_math` 321 passed / 0 failed / 1 ignored; `flip_face_reverses_normal` + all bevel tests green; `cargo build -p app` clean.

**Lesson:** add an orientation/outward-normal assertion to the standard mesh-op gate set — topology gates do not catch normal flips.

---

## 2026-06-30 — BEVEL TWISTED-ENDPOINT-FACE FIX (the real "edges slide along the wrong face" bug)

**Symptom (live app, single-edge bevel):** the two rail/cut points slid along the wrong face — the endpoint faces looked reversed/twisted. (Distinct from the earlier flipped-NORMAL fix; this one is GEOMETRY.)

**Diagnosis (measure, not guess):** dumped all 7 faces of a single-edge cube bevel. The two rails themselves were correct, but the `(false,false)` endpoint-face re-trace inserted its two cut points in REVERSED order → the endpoint face (F4) self-intersected (verified: ring edges A-B and C-D cross at t=0.889). Topology gates (validate/watertight) passed it because a self-intersecting ring still twins fine in `from_polygons`.

**Root cause:** `order_arc_about_vertex` ordered the inserted cut points by angle around an EXTERNAL normal (vertex normal, then I tried the face normal — neither fixed it). The handedness of that axis flips the in-plane angular order and reverses the pair.

**Fix:** derive the sweep axis from the CORNER ITSELF — `(pe.source - V) × (ce.dest - V)` — and sort the cut points by angle from the pe-side around it. Handedness-free; monotone pe-side→ce-side. Verified F4/F5 now trace as simple polygons.

**Gate:** new `bevel_op::tests::bevel_output_faces_are_simple` — projects each output face to 2D and asserts no two non-adjacent edges cross (single-edge + edge-loop configs). Reproduced the twist; now green.

**Status:** `cargo test -p core_math` 322 passed / 0 failed / 1 ignored; app builds. `half_edge.rs` untouched by this fix (bevel_op.rs only).

---

## 2026-06-30 — ROUND BEVEL (segments + profile) + live contextual scroll

**Feature (owner request):** round/multi-segment bevel, segment count via SCROLL, scroll gated to the bevel tool only.

**Input (already correct):** scroll dispatch is an exclusive `else if self.tool.current.is_poly_bevel()` branch (`input_viewport.rs:160`) — it consumes scroll for segments and cannot fall through to camera zoom or leak into other tools. No change needed; confirmed contextual.

**Geometry (`bevel_op.rs`, the real work — `segments`/`shape` were previously ignored):**
- Per beveled edge, build the rail arc from face L → face R: quadratic Bézier, control pulled toward the ORIGINAL edge vertex by `shape` (0=flat chamfer, 0.5=round fillet, 1=convex bulge). `n_seg` strips instead of one; endpoint `(false,false)` faces and corner patches insert the SAME welded arc points so the round stays watertight. `segments==1` is byte-identical to the flat chamfer.
- **v1 scope (honest):** rounded corners where ≥2 beveled edges MEET are a v2 blend problem (a flat cap can't seal a curved multi-edge corner — it left 16 boundary edges). So a shared-corner selection falls back to a flat, watertight chamfer; isolated edges (the common single-edge case) round fully. Gated by a shared-corner check.

**Live update (`poly_drag_tools.rs`):** extracted `poly_bevel_regenerate()` as the single producer for both width/shape drag and scroll; `poly_bevel_scroll_segments` now rebuilds the mesh when the count changes (was: only updated the counter, no re-mesh until the next mouse move).

**Gates:** `bevel_segments_produce_round_watertight` (segments=4 single edge: +3 strips, watertight, bulges off both faces; loop stays watertight via flat fallback). `bevel_output_faces_are_simple` + `bevel_output_faces_all_point_outward` extended to segments 3–4 configs. Full `cargo test -p core_math` 323 passed / 0 failed; app builds. `half_edge.rs` untouched.

---

## 2026-06-30 — BEVEL-ON-BEVELED FIX (vertex-corner generalization)

**Symptom (user):** bevel 2 bottom edges, then bevel the new vertical edge → mesh tears / stray vertex-edge.

**Diagnosis (measure-first):** reproduced in a test — beveling the vertical edge `(1,1,-1)→(1,-0.75,-1)` tears with 3 boundary edges. The bottom endpoint `(1,-0.75,-1)` is a bevel-created **degree-4** vertex where 2 chamfer strips + 2 re-traced faces meet. The `(false,false)` re-trace inserted the beveled edge's WHOLE arc into BOTH chamfer strips — correct only for a degree-3 cube corner; at degree>3 it over-inserts and leaves a triangular hole `[rail_F2, junction, rail_F5]` unsealed.

**Fix (two parts, general — works at any vertex degree):**
1. **Per-side rail insertion:** a `(false,false)` face cuts its corner only on the side(s) that actually border a beveled-edge face (`pe.twin.prev` / `ce.twin.next`). Both sides on the SAME beveled edge (degree-3) → whole arc as before; distinct/one side (degree>3) → just that side's rail endpoint. One rule, both cases.
2. **STEP 6 residual-hole seal:** for a CLOSED input, any directed edge in `faces_out` with no reverse twin bounds a residual hole — chain them into loops and cap each with the reversed loop (twins every boundary edge → watertight). No-op on a clean cube; skipped for open input so real boundaries survive.

**Gate:** `bevel_on_beveled_watertight` — bevel 2 bottom edges, then bevel EVERY interior edge of the result; each must be manifold + watertight + simple. Full `cargo test -p core_math` 324/0; app builds; `half_edge.rs` untouched.
