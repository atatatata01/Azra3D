# Procedural Op-Graph Feasibility Audit + Baseline Benchmark

Date: 2026-08-01 · Branch: `master` · Benchmark: `core_engine/op_bench` (committed with this doc; kernel source untouched)
Method: `std::time::Instant`, warmup, median + p95, run count `n` printed per row (criterion is not a workspace dep; none added). Release profile (`opt-level 3, lto`). Raw output: `op_bench` reproduces it with `cargo run -p op_bench --release`.
`REMAINING_ISSUES.md`: **NOT FOUND** anywhere in the repo. Kernel TODO markers: 2, both benign ([half_edge.rs:110](../core_engine/core_math/src/half_edge.rs#L110), :5252).

---

## Verdict

**Not feasible on the kernel as it stands; feasible after a bounded, nameable locality-and-provenance rework — and the op-graph layer itself is the cheap part.** Three of your four kill criteria fire, two of them as measured numbers, and I am saying them out loud. **(C) Local ops scale with mesh size:** a one-edge-loop bevel costs 0.85 ms → 23 ms → 281 ms → 1.94 s across S/M/L/XL because bevel, loop cut, subdivide, poke, knife, and boolean all end in a whole-mesh `from_polygons` rebuild — and even `extrude_face`, the kernel's one genuinely local topology op, costs 207 ms at XL because it ends with a full-mesh `recompute_normals()`. **(4) Snapshot-assisted replay on scene M cannot clear 16 ms:** a single rebuild-tier op inside the replay window costs 17–19 ms on M by itself; the expected cost of a 100-op window at a realistic op mix is ~35 ms. **(1) The boolean cannot carry the provenance you need without replacement — and it needs replacement anyway:** intersection vertices are bare `Vec3` lerps, so "this vertex = A-edge-i × B-face-j" is never computed and cannot be recorded at any price; and as measured, a box − 1,536-face curved solid returns `None` outright, while a box − 24,576-face solid takes **281 seconds** and returns a geometrically wrong 6-face result. Only kill criterion (2) does **not** fire: the kernel is structurally pure (no app coupling, no rayon, no rand/time, one write-only `thread_local!` diagnostic), and byte-identical replay is blocked by exactly ~11 enumerable `HashMap`-iteration sites plus the per-process hasher seed — mechanically fixable in about a week; the benchmark's own op mix already replays bincode-byte-identical across two separate processes. What makes the plan rescuable rather than dead: a serializable-shaped op enum (`PolyEdit`, 22 variants) with a single `apply_in_place` dispatch **and parametric redo that already re-runs ops** exists and works; op params are already plain data for every kernel op except knife; the file format already has a proven extension mechanism; and op-log serialization measures at 20 B/op — 60× smaller than the mesh blob at M. The work your plan assumed was already true — local cost, IDs that survive re-evaluation, boolean provenance — is precisely the work that does not exist.

---

## Top 5 risks, ranked

### R1 — The boolean: wrong tool for provenance, and disqualified on robustness/perf regardless — **kill criterion 1, fired**
- **What it is:** a float-epsilon split-by-planes + generalized-winding-number classifier (`robust_boolean`, [mesh_boolean.rs:1763-1849](../core_engine/core_math/src/mesh_boolean.rs#L1763)) with a csg.js BSP as *fallback* (:1865-1888) — the fallback being the path self-documented (:1387-1396) as producing spurious flipped faces. Not exact predicates, not intersection-curve based, despite the header's comparison to libigl/Cork/Manifold (:1396 — they classify this way *after* an exact intersection stage this code doesn't have).
- **Provenance:** internal `Polygon.source` is one bit (A or B, :116-123). `material_id` propagation: **zero** — every output face gets material 0 via `from_polygons` ([half_edge.rs:4847](../core_engine/core_math/src/half_edge.rs#L4847)), even though sibling ops already know the snapshot-and-restore fix ([app/src/poly/boolean.rs:106-139](../core_engine/app/src/poly/boolean.rs#L106)). Face-level origin tags are a cheap retrofit up to the stitcher (`fid` is in scope at fragment creation, :409-435; `inherit` at :171-175 is a single choke point; the `ring_src`/`face_src` arrays already ride in lockstep, :836-968) — but everything downstream of :978 (`merge_coplanar`, `dissolve_collinear`, the five `polish` passes ending in a *second* `from_polygons` at :1141) destroys the correspondence. **Vertex/edge-level names on the intersection curve — what stable naming through booleans actually requires — are impossible in this design:** intersection points are created as untagged lerps at :256 and :1580 and become indistinguishable from input corners the moment they enter `Polygon.verts`.
- **Measured:** box−box: 0.7–0.9 ms ✓. Box − 1,536-face CC ball: **`None`**. Box − 24,576-face CC ball: **280,960 ms (4.7 minutes), output 6 faces** — the carve was dissolved away (consistent with the audited coplanar/dissolve defects). Hard input ceiling `MAX_BSP_POLYS = 262_144` triangles (:75). Absolute epsilons (`PLANE_EPS=1e-5` :42; a hardcoded 1e-2 collinear-dissolve threshold at :588/:1147 that will eat real corners on sub-centimeter models); documented unguarded damage to ≥30-sided cylinders (:58-59).
- **Cost:** face-tags-to-stitcher ~1-2 days (fixes material loss for free, worth doing regardless). A provenance-carrying replacement (Manifold-class exact/intersection-curve boolean, which yields the intersection topology naming needs *as a byproduct*): 4-8 weeks — and the robustness/perf findings say this was needed with or without the op-graph plan.

### R2 — Locality: rebuild-tier ops are O(mesh); even "local" ops carry O(mesh) tails — **kill criterion C, fired as measured**
- `from_polygons` production callers: bevel ([bevel_op.rs:245](../core_engine/core_math/src/bevel_op.rs#L245), :1503), loop cut ([half_edge.rs:2345](../core_engine/core_math/src/half_edge.rs#L2345)), partial subdivide (:5401 — copies **all** vertex positions at :5329 and re-emits **every** face to subdivide one), poke (:5449), knife ([knife.rs:559](../core_engine/core_math/src/knife.rs#L559)), boolean (:999, :1141), display triangulate ([poly_mesh.rs:225](../core_engine/core_math/src/poly_mesh.rs#L225)).
- Two kernels are *quadratic*, worse than the rebuilds: `extrude_region`'s interior test ([half_edge.rs:1130-1137](../core_engine/core_math/src/half_edge.rs#L1130)) and `dissolve_one_region`'s loose-vertex sweep (:3349-3357) — O(sel·F) with a heap allocation in the innermost step. Weld scans **every half-edge per merged vertex** (:1391-1394).
- Universal per-op tax: whole-mesh `recompute_normals` (:645; ~35 call sites; also baked into `from_polygons` at :4883), whole-mesh `validate()` after every `PolyEdit` ([mesh_ops/mod.rs:598](../core_engine/core_math/src/mesh_ops/mod.rs#L598)) plus whole-mesh `is_manifold()` on the Snapshot tier (:607-622), and free-lists stored as `Vec<u32>` probed with linear `.contains()` inside hot loops (:649, :656, :4725) — O(F·|free|) once deletions accumulate.
- **Cost:** incremental normals + region-bounded validate + ID-preserving in-place implementations of loop cut/bevel/subdivide: the single largest work item, est. 6-10 weeks for the headline ops. This is the gating work — §3 shows no replay strategy clears 16 ms until it lands.

### R3 — No element provenance, no generation checking; stale IDs resolve to the *wrong live element* silently
- Elements are newtyped raw `u32` slots with **free-list recycling** ([half_edge.rs:192-200](../core_engine/core_math/src/half_edge.rs#L192), alloc pops the free list at :218-249) — the ABA hazard is undetectable in principle: `is_edge_live()` cannot distinguish "your edge" from "a different edge that inherited your slot."
- The intended fix exists and is inert: `Handle { id, gen }` with `gen` carried but never validated ([mesh_ops/mod.rs:34-48](../core_engine/core_math/src/mesh_ops/mod.rs#L34)); `GenTags` deferred, explicitly, three times ([poly_mesh.rs:13](../core_engine/core_math/src/poly_mesh.rs#L13), :550-551, mesh_ops/mod.rs:35). `TopoDelta` — the parent-map carrier — is an empty struct whose `is_empty()` hard-returns `true` (:86-94); `EditResult.parents` has been empty since it was declared (:234).
- Rebuild-tier ops return **no remap**. Nuance that helps: `VertexId`s *do* survive `from_polygons` in the subdivide/poke/loop-cut paths (positions are rebuilt index-parallel, documented at [half_edge.rs:5419-5422](../core_engine/core_math/src/half_edge.rs#L5419)) and `EdgeKey` (canonical vertex pair, [poly_mesh.rs:245-252](../core_engine/core_math/src/poly_mesh.rs#L245)) is a genuinely durable key — but `FaceId`s shift with free-list holes, `HalfEdgeId`s are fully re-keyed (:4853), and the bevel rebuilds vertices through a position-quantizing `WeldMap` ([bevel_op.rs:59-85](../core_engine/core_math/src/bevel_op.rs#L59)), so nothing survives it.
- Already-shipped consequences of this exact bug class: `.fl4` persists authored UV/crease/seam/sharp keyed by raw element id (`PolyLayerRecord`, [project.rs:273-284](../core_engine/app/src/project.rs#L273)) — correct today *only* because save→load is the one path that never renumbers; and after a boolean install, `poly_custom_data` is never invalidated ([cache_state.rs:157](../core_engine/app/src/cache_state.rs#L157) vs [poly/boolean.rs:466-497](../core_engine/app/src/poly/boolean.rs#L466)) so authored attributes silently re-alias onto arbitrary new elements.
- **Cost:** GenTags + `from_polygons` remap table + populated `TopoDelta`: ~40-80 h (per-op parent maps) on top of R2, before the naming scheme itself.

### R4 — Cross-process determinism fails at ~11 enumerable sites — **kill criterion 2 does NOT fire; this is the cheap one**
- Structurally pure: `core_math` cannot name `app` (deps: bytemuck/glam/rayon/serde, [core_math/Cargo.toml:6-11](../core_engine/core_math/Cargo.toml)); no rayon in the kernel; no rand/time/thread-IDs/pointer ordering; one write-only `thread_local!` bevel diagnostic ([bevel_op.rs:19-32](../core_engine/core_math/src/bevel_op.rs#L19)).
- But every hash collection is `std::RandomState` (per-process seed), and these sites let hash order drive output: ring **winding** in `edge_loop_rings` ([half_edge.rs:2081](../core_engine/core_math/src/half_edge.rs#L2081) — its own doc claims "Deterministic"; its sibling `loop_space` sorts correctly at :2848-2850), the mutating fixpoints in `dissolve_edges_region` (:3587) and `rip_edges` (:3744/:3785), allocation order in `extrude_region` (:1143/:1156), `extrude_region_volume` (:1260-1310), `dissolve_faces_region` (:3275), `dissolve_one_region` (:3307/:3321), the unsorted hole-seal in bevel ([bevel_op.rs:1480](../core_engine/core_math/src/bevel_op.rs#L1480) — the one unsorted site in an otherwise `[Codex P3]`-sorted file), the frozen `bevel_edges_ex` (:4960), plus one unordered f32 accumulation in app extrude-slide normals ([poly_drag_tools.rs:101-103](../core_engine/app/src/poly_drag_tools.rs#L101)).
- Empirical: op_bench's scene log (plane → extrudes → loop cut → bevel → 2,000 translates) replays **bincode-byte-identical**, including across two separate processes — the mix avoids the bad sites. The boolean already fixed this class wholesale with `BTreeMap` and documents the historical bug it caused ([mesh_boolean.rs:24-30](../core_engine/core_math/src/mesh_boolean.rs#L24)).
- Trap for the fix: the existing test oracle `mesh_eq` is *deliberately* slot/winding-agnostic ([test_spine.rs:47-56](../core_engine/core_math/src/mesh_ops/test_spine.rs#L47)) — it passes on exactly the divergences that break byte-identity. The only true two-process byte gate in the repo covers the DC path, not this kernel ([dc_baseline.rs:402-425](../core_engine/app/src/dc_baseline.rs#L402)); copy that pattern.
- **Cost: ~1 week** — sort the 11 sites, return the bevel diagnostics by value, add a two-process bincode-hash replay gate + an ID-strict oracle.

### R5 — The render path re-tessellates and re-uploads the whole body per edit; a fast graph would still *feel* slow
- `poly_sync_mesh_cache` clones the base mesh, re-runs the full modifier stack (up to 256× CC face explosion), full `raster_data_ngon` with a heap-allocating `vertex_fan` per face-corner ([poly_mesh.rs:158](../core_engine/core_math/src/poly_mesh.rs#L158)), rebuilds both CPU arrays, clones them again for the persistent copy, and full-range `write_buffer`s both GPU buffers ([mesh_sync.rs:51-76](../core_engine/app/src/poly/mesh_sync.rs#L51), [mesh_cache.rs:514-563](../core_engine/render/src/mesh_cache.rs#L514), [mesh_gpu.rs:142](../core_engine/render/src/mesh_gpu.rs#L142)). No partial upload path exists for poly; the `content_signature` memo gate ([mesh_sync.rs:38-49](../core_engine/app/src/poly/mesh_sync.rs#L38)) by construction never fires during a drag. The bevel drag adds ~8 redundant full-mesh integrity passes per mouse-move frame ([poly_drag_tools.rs:1278-1352](../core_engine/app/src/poly_drag_tools.rs#L1278)).
- Measured: `to_raster_data` alone = 4.3 ms (M), 45 ms (L), 194 ms (XL) — the *display* path exceeds the entire 16 ms budget at L before the kernel does anything.
- **Order matters:** partial upload is *impossible to write correctly* today because `from_polygons` re-keys everything — no stable vertex range exists to patch. R2 (and its remap tables) must land first. **Cost: 2-3 weeks, strictly after R2.**

---

## Audit table (Part 1)

| # | Question | Finding | Key evidence | Blocker? |
|---|---|---|---|---|
| 1.1a | Ops pure functions? | Yes structurally — `core_math` can't reach app state; ops are `&mut self` or `&mesh → Mesh`; atomic single-write dispatch | core_math/Cargo.toml:6-11; [mesh_ops/mod.rs:413-425](../core_engine/core_math/src/mesh_ops/mod.rs#L413) | No |
| 1.1b | Global state | One write-only `thread_local!` diagnostic (bevel HUD); nothing else | [bevel_op.rs:19-32](../core_engine/core_math/src/bevel_op.rs#L19) | No (trivial) |
| 1.1c | Hash-order → geometry | 11 order-sensitive sites (winding, allocation order, mutating fixpoints); ~80% of sites already hardened | [half_edge.rs:2081](../core_engine/core_math/src/half_edge.rs#L2081), :1143, :3275, :3587, :3744; [bevel_op.rs:1480](../core_engine/core_math/src/bevel_op.rs#L1480) | Yes — ~1 wk |
| 1.1d | Float order / rayon | No rayon in kernel; one unordered f32 sum in app | [poly_drag_tools.rs:101-103](../core_engine/app/src/poly_drag_tools.rs#L101) | No |
| 1.1e | rand/time/pointer order | NOT FOUND (fixed-seed LCGs only; `Instant` in tests only) | [primitives.rs:1099](../core_engine/core_math/src/primitives.rs#L1099) | No |
| 1.1f | Byte-identical replay possible? | Yes after 1.1c: measured byte-identical in- and cross-process for the bench op mix; hasher seed + the 11 sites are the whole gap | op_bench DETERMINISM rows | Conditional |
| 1.2a | Element references | Newtyped raw `u32`, free-list **recycling**, no generation counter → ABA undetectable; `Handle.gen` exists, inert | [half_edge.rs:192-200](../core_engine/core_math/src/half_edge.rs#L192), [mesh_ops/mod.rs:34-48](../core_engine/core_math/src/mesh_ops/mod.rs#L34) | **Yes** |
| 1.2b | Survive topology op / compaction / save-load | In-place ops: slot-stable. `from_polygons` (≈6 mutating sites): verts survive in some paths, faces shift with holes, half-edges always re-keyed, **no remap returned**. Save-load: byte-exact incl. free-lists — IDs round-trip perfectly | [half_edge.rs:5419-5422](../core_engine/core_math/src/half_edge.rs#L5419), :4853, :191-200 | **Yes** (rebuild tier) |
| 1.2c | Existing naming/provenance | NOT FOUND at mesh-element level. `TopoDelta` empty by design; `CornerParentMap` is heuristic, UV-only, extrude/inset-only, discarded. `FaceOrigin` exists **only in the nurbs-modeling worktree** — exactly the needed pattern, not in the main tree | [mesh_ops/mod.rs:86-94](../core_engine/core_math/src/mesh_ops/mod.rs#L86); [propagate.rs:33-88](../core_engine/core_math/src/mesh_ops/propagate.rs#L33) | **Yes** — R3 |
| 1.3a | Params as data | Every kernel op except knife: plain data. Knife takes a camera closure (`&dyn Fn(Vec3)->Option<Vec2>`) | [knife.rs:125-134](../core_engine/core_math/src/knife.rs#L125) | No (knife: redesign) |
| 1.3b | Common interface | `PolyEdit` (22 variants) + `apply_in_place → Result<OpDelta, OpError>` + batch runner; **undo redo already re-runs ops parametrically** | [mesh_ops/mod.rs:267-317](../core_engine/core_math/src/mesh_ops/mod.rs#L267), :413; [undo.rs:568](../core_engine/app/src/undo.rs#L568), :333-350 | No — head start |
| 1.3c | Single `enum Op` w/o rewrites | 15 ops YES (already variants; serde derive missing but shape is ready), 12 PARTIAL (mechanical param extraction), knife NO, scene-level boolean NO | op-param audit §Q5 | No |
| 1.4 | Boolean provenance | R1: face-level = cheap to the stitcher, destroyed after it; edge/vertex-level = **impossible without algorithm replacement**; algorithm needs replacement anyway (measured) | R1 | **Yes — the big one** |
| 1.5a | Selection representation | Three raw `HashSet<u32>` on app state ([types_poly.rs:55-64](../core_engine/app_types/src/types_poly.rs#L55)), edge domain keyed by `HalfEdgeId` — the least stable domain; no poly analogue of `sanitize_selection_unified`; staleness masked per-frame at draw time | [selection_unified.rs:85-107](../core_engine/app/src/selection_unified.rs#L85); [draw_poly.rs:265](../core_engine/app/src/draw_poly.rs#L265) | Partial |
| 1.5b | Selection persisted? | Session-only (not in `.fl4`); **is** in undo via `SelectionSnapshot`; geometry undo does *not* restore poly sub-selection | [types_transform.rs:24-32](../core_engine/app_types/src/types_transform.rs#L24) | No |
| 1.5c | Query-based selection at replay | Nothing exists, but ops already take explicit sets — a query→set resolver slots in front of existing signatures. Feasible; measured binding cost is negligible (§E) | — | No |
| 1.6a | Serde present | Yes — mesh + all element types derive; full mesh rides bincode inside `ProjectFile`; round-trip test exists | [half_edge.rs:191](../core_engine/core_math/src/half_edge.rs#L191), :9954-9973 | No |
| 1.6b | Versioning story | `version: u32` frozen at 7, effectively dead; real mechanism = `#[serde(default)]` in-blob fields + self-delimiting magic tails (`FL4ORDER`…`FL4NODES`). A graph section = one more magic tail. `PolyModifier` already serializes params-as-data. **Trap:** `PolyLayerRecord` persists element-id-keyed user data that a replaying graph would silently re-bind wrong | [project.rs:529-566](../core_engine/app/src/project.rs#L529), :273-284; [scene_graph.rs:290-315](../core_engine/core_math/src/scene_graph.rs#L290) | No (trap noted) |
| 1.7 | Locality per op | R2 + §C. Bounded topology, O(mesh) tails: split_edge, extrude_face, dissolve_edge, weld. Whole-mesh rebuild: bevel, loop cut, subdiv, poke, knife, boolean, triangulate. Quadratic: extrude_region, dissolve regions | R2 | **Yes — kill criterion C** |
| 1.8 | Cross-cutting blockers | (a) Undo Snapshot tier = whole-mesh clone for 21 of 24 `PolyEdit` variants ([mesh_ops/mod.rs:381-383](../core_engine/core_math/src/mesh_ops/mod.rs#L381)); (b) boolean destroys `material_id` + stale `poly_custom_data` re-aliasing (R3); (c) `new_uv_sphere` is **not watertight** (boundary half-edges > 0 — found by this benchmark; it silently disqualifies any boolean using the stock sphere); (d) the owner's active non-destructive direction is the voxel-remesh modifier pipeline ([MODIFIER_STACK_PIPELINE.md](../MODIFIER_STACK_PIPELINE.md)) — the op-graph plan overlaps its territory; reconcile before building both | — | Strategic |

---

## Part 2 — Baseline benchmark

### Scenes (procedural, deterministic; exact counts)

Mixed op sequence per scene: n×n quad plane → 8 face extrudes → 1 loop cut → 1 edge-loop bevel → thousands of vertex translates → final normal recompute. Boolean is measured separately (the plane is open; `mesh_to_polygons` rejects open meshes at [mesh_boolean.rs:404](../core_engine/core_math/src/mesh_boolean.rs#L404)).

| Scene | Verts | Half-edges | Faces | Ops in log | First build | Peak heap | Mesh struct |
|---|---|---|---|---|---|---|---|
| S | 1,042 | 3,896 | 973 | 511 | 3.4 ms | 0.9 MB | 0.15 MB |
| M | 20,072 | 79,136 | 19,783 | 2,011 | 68.1 ms | 17.3 MB | 4.0 MB |
| L | 203,898 | 811,966 | 202,991 | 3,011 | 770.7 ms | 143.6 MB | 32.0 MB |
| XL | 1,003,048 | 4,004,166 | 1,001,041 | 3,011 | 4,682 ms | 898.3 MB | 128.0 MB |

Determinism: replaying each log in the same process → `content_signature` equal **and bincode byte-identical** at all four sizes; S/M signatures also matched across two separate process invocations (`0x148fc5210908e4a5`, `0xbc043b929e421042`). This op mix avoids the 11 sites in R4.

### A. Per-op cost, isolated (median ms; p95 in parentheses where it matters)

| Op | S | M | L | XL | Scaling |
|---|---|---|---|---|---|
| translate ×1 (from ×1000 batch) | 0.0000013 | 0.000055 | 0.00078 | 0.0034 | O(1) compute, cache-miss-bound |
| extrude_face ×1 | 0.157 | 3.53 | 42.2 | 206.9 | **O(mesh)** — full `recompute_normals` tail |
| dissolve_edge ×1 | 0.138 | 2.84 | 34.8 | 156.7 | **O(mesh)** — same tail |
| weld (merge 2 verts) | 0.148 | 3.14 | 38.9 | 177.8 | **O(mesh)** — full half-edge scan + tail |
| bevel 1 edge-loop (10-12 edges) | 0.906 | 19.5 | 237.0 | 1,525 | **O(mesh)** — `from_polygons` rebuild |
| loop_cut ×1 | 0.756 | 17.8 | 208.0 | 1,570 | **O(mesh)** — rebuild |
| subdivide_faces ×1 face | 0.771 | 17.3 | 210.1 | 1,615 | **O(mesh)** — re-emits every face |
| recompute_normals (full) | 0.138 | 2.98 | 34.4 | 157.3 | the universal tail, isolated |
| to_raster_data (full) | 0.137 | 4.30 | 45.1 | 194.0 | display path, per edit today |

Boolean (closed operands; box − CC-subdivided box; `new_uv_sphere` rejected as non-watertight):
box−box (6+6 faces): **0.7–0.9 ms OK** · box − 1,536-face ball: **FAILED (`None`)** · box − 24,576-face ball: **280,960 ms, output 6 faces (geometrically wrong)** · ≥262,144 tris: `None` by hard ceiling. Single run; too slow to repeat — flagged as such.

### B. Full rebuild (replay whole log from scratch)

| Scene | Median | p95 | vs 16 ms |
|---|---|---|---|
| S (511 ops) | 3.0 ms | 3.2 ms | 0.19× |
| M (2,011 ops) | 76.5 ms | 80.6 ms | **4.8×** |
| L (3,011 ops) | 871 ms | 885 ms | **54×** |
| XL (3,011 ops) | 4,676 ms | 5,377 ms | **292×** |

### C. Locality check — the plan's core assumption — **FAILS LOUDLY**

Same one-edge-loop bevel (10-12 edges), same single loop cut, same single extrude, on growing meshes:

| Op (constant-size edit) | S | M | L | XL | XL/S ratio |
|---|---|---|---|---|---|
| bevel 1 edge-loop | 0.854 | 23.4 | 281.1 | 1,943 | **2,275×** |
| loop_cut 1 cut | 0.736 | 21.3 | 251.0 | 1,389 | **1,887×** |
| extrude_face (best case) | 0.157 | 3.56 | 36.1 | 199.1 | **1,268×** |

Cost scales with **mesh size**, not ring size, for every op measured — including the op with genuinely local topology. The ~1,000× growth for a ~1,000× face-count growth is exactly linear-in-mesh. **Measurement C's kill condition is met.** It is fixable (the O(mesh) components are enumerable: `from_polygons` contract, `recompute_normals` tail, full-mesh validate, linear free-list probes), but it is kernel work, not graph work.

### D. Cache behaviour (full half-edge ring walk; bytes = live element structs touched)

| Scene | Working set | Median | Effective bandwidth |
|---|---|---|---|
| S | 0.1 MB | 0.0035 ms | 36.2 GB/s |
| M | 2.4 MB | 0.078 ms | 32.8 GB/s |
| L | 24.8 MB | 3.78 ms | **6.9 GB/s** |
| XL | 122.3 MB | 13.0 ms | 9.8 GB/s |

Cache-miss rate: NOT MEASURED (no `perf` on Windows); the bandwidth cliff is the proxy. Corroborating signal: random-access vertex translate costs 1.3 ns (S) → 55 ns (M) → 781 ns (L) → 3,418 ns (XL) per translate — a ~2,600× penalty that is pure memory latency.

### E. Name-resolution proxy (HashMap<u64,u32>, keyspace = element count)

Per-lookup: 5.0-5.1 ns (S) · 5.0-6.5 ns (M) · 6.0-11.0 ns (L) · 6.0-11.4 ns (XL). 10,000 lookups at XL = 0.114 ms. **Binding overhead is negligible** at any plausible bindings-per-op count — three orders of magnitude below op costs.

### F. Op-log serialization (params only, bincode; bench-local `OpRecord` enum as the proxy)

| Scene | Ops | Log size | B/op | Serialize | Deserialize | Full-mesh blob (contrast) |
|---|---|---|---|---|---|---|
| S | 511 | 10,257 B | 20.1 | 0.0015 ms | 0.0043 ms | 0.12 MB |
| M | 2,011 | 40,257 B | 20.0 | 0.0060 ms | 0.0150 ms | 2.42 MB |
| L | 3,011 | 60,257 B | 20.0 | 0.0096 ms | 0.0225 ms | 24.80 MB |
| XL | 3,011 | 60,257 B | 20.0 | 0.0086 ms | 0.0224 ms | 122.25 MB |

The graph-as-document is I/O-trivial: ~20 bytes and ~10 ns of codec per op. At M the op log is **60× smaller** than the mesh blob the `.fl4` stores today.

### G. Memory

| Scene | Mesh structure | GPU raster buffers | Peak heap during build |
|---|---|---|---|
| S | 0.15 MB | 0.14 MB | 0.9 MB |
| M | 4.0 MB | 2.87 MB | 17.3 MB |
| L | 32 MB | 29.4 MB | 143.6 MB |
| XL | 128 MB | 145.1 MB | 898.3 MB |

Mesh struct ≈ GPU buffers ≈ 1:1. Peak heap during the op sequence runs ~4-7× the final mesh — the `from_polygons` rebuild churn. Snapshot-based replay must budget for that multiplier, not the resting size.

---

## Part 2.3 — Projection arithmetic (scene M, 16 ms scrub budget)

Representative 900-op history mix (typical modeling session shape): 850 translates, 30 extrudes, 10 dissolves, 5 loop cuts, 4 bevels, 1 subdivide.

**Naive full replay:**
`850×0.000055 + 30×3.53 + 10×2.84 + 5×17.75 + 4×19.45 + 1×17.31`
`= 0.05 + 105.9 + 28.4 + 88.8 + 77.8 + 17.3 ≈ 318 ms` → **20× over budget. FAILS.**
(Measured floor: the bench's own 2,011-op M log — far translate-heavier than realistic — replays in 76.5 ms, already 4.8× over.)

**Snapshot every 100 ops, replay last ≤100 (expected mix = 1/9 of the above):**
`≈ 0.005 + 3.33×3.53 + 1.11×2.84 + 0.56×17.75 + 0.44×19.45 + 0.11×17.31`
`≈ 11.8 + 3.2 + 9.9 + 8.7 + 1.9 ≈ 35.5 ms` (+ snapshot restore, unmeasured, est. 2-5 ms for the 2.4 MB mesh) → **~2.4× over budget. FAILS in expectation** — and any window containing even one rebuild-tier op (17-19.5 ms) busts regardless. Corollary: to have a chance, snapshots must be placed *after every rebuild-tier op*, not on a fixed count; a fixed-interval window clears 16 ms only if it contains zero rebuild-tier ops and ≤4 extrude-class ops.

**Dirty-region replay, 12 of 900 ops intersect (proportional mix):**
`≈ 11.3×0.000055 + 0.4×3.53 + 0.13×2.84 + 0.067×17.75 + 0.053×19.45 + 0.013×17.31`
`≈ 1.41 + 0.38 + 1.19 + 1.04 + 0.23 ≈ 4.2 ms` → **CLEARS 16 ms in expectation** — but with ~35 ms worst case if two rebuild-tier ops land in the dirty set. Conditional pass only.

**Binding overhead** (measurement E, assume 100 name resolutions per replayed op at M's 6.5 ns): naive 900 ops → +0.59 ms; snapshot window 100 → +0.065 ms; dirty 12 → +0.008 ms. **Negligible in every strategy.** Even 10,000 bindings/op adds only 0.065 ms per op.

**Break-even points:**
- Naive replay on M exceeds 16 ms at **history length 1** if that op is rebuild-tier (17.3-19.5 ms each), at ~5 ops if extrude-class (3.53 ms), and essentially never for pure translates (290k fit).
- Mesh-size break-even for a *single* rebuild-tier op ≈ **~18k faces** (rebuild ≈ 0.9 ms per 1k faces): above scene-M size, one bevel/loop-cut/subdivide alone blows the frame.
- Snapshot-assisted (window 100) exceeds 16 ms whenever expected window cost `100 × 0.353 ms/op ≈ 35 ms` — i.e., at the measured mix it is *always* over on M; it drops under 16 ms only for windows ≤ ~45 ops **and** zero rebuild-tier ops inside.

**Bottom line: on scene M today — naive FAILS (20×), snapshot-assisted FAILS (~2.4× expected, unbounded tail), dirty-region PASSES conditionally (4.2 ms expected) and only degrades to failure when a rebuild-tier op is dirty.** After R2 (rebuild-tier ops brought to ring-proportional cost, ≈ their S-scene numbers of 0.7-0.9 ms), the same arithmetic gives: snapshot window ≈ 3.5 ms ✓, dirty-region ≈ 0.5 ms ✓, and even naive 900-op replay ≈ 12-15 ms — marginal but plausible. **The 16 ms budget is decided entirely by R2, not by the graph machinery.**

---

## Hardware note (from measurement D)

The workload is **cache-bound, not clock-bound, precisely at the mesh sizes the plan targets.** Effective traversal bandwidth collapses 33 → 6.9 GB/s crossing from M (2.4 MB working set, cache-resident) to L (24.8 MB, DRAM); random-access element writes pay up to ~2,600× latency penalty at XL. Half-edge traversal is pointer-chasing with 20-28 B nodes — latency-bound, not bandwidth-bound. **Favor large L3 (X3D-class V-Cache) over clock for L/XL cages**; an L-scene working set (~25-32 MB) fits substantially inside a 96-128 MB L3, which would move L-scale traversal back toward the ~33 GB/s cache-resident regime. For S/M control cages, everything fits in any modern L2/L3 and clock wins. Since the plan's readout meshes are XL-class but its *edited* cages are S/M-class, V-Cache benefits the readout/replay path specifically — the path the 16 ms budget stresses.

---

## What I got wrong (assumptions in the plan the code contradicts)

1. **"An op is a pure function of (mesh, params)" framed purity as the risk — it's the non-risk.** The kernel is already structurally pure and 80% determinism-hardened; the boolean already went through this exact hardening and documents why ([mesh_boolean.rs:24-30](../core_engine/core_math/src/mesh_boolean.rs#L24)). One week of sorts closes it. The plan budgeted worry in the wrong place.
2. **"Local cost — replay cost proportional to the dirty region" assumed locality was a property to *preserve*. It doesn't exist.** Not for one op measured — even `extrude_face` is O(mesh) via its normals tail, and the undo system already tacitly admits this: 21 of 24 `PolyEdit` variants take whole-mesh Snapshot undo ([mesh_ops/mod.rs:381-383](../core_engine/core_math/src/mesh_ops/mod.rs#L381)). The plan's cheapest-sounding requirement is the kernel's largest missing piece.
3. **"Provenance through booleans" presumed a boolean worth building on.** As measured, the boolean fails a 1.5k-face curved operand outright and takes 4.7 minutes with a wrong result at 24k faces. Provenance is moot until the algorithm is replaced; the plan should treat boolean replacement as a prerequisite, not an enhancement.
4. **The plan didn't anticipate that the file format already persists element-id-keyed user data.** `PolyLayerRecord` UV/seam/sharp keyed by raw `HalfEdgeId.0` ([project.rs:273-284](../core_engine/app/src/project.rs#L273)) is correct only because nothing ever renumbers between save and load. Graph re-evaluation breaks that contract silently — authored UVs bind to wrong corners with no failure signal. The graph must own or version-gate that data.
5. **"Dormancy … re-binds if the referent returns" assumes staleness is detectable.** It isn't, in principle: free-list slot recycling means a dead ID resolves to a different live element, and the `gen` field that would catch it has been deliberately deferred three times ([poly_mesh.rs:13](../core_engine/core_math/src/poly_mesh.rs#L13), :550, [mesh_ops/mod.rs:35](../core_engine/core_math/src/mesh_ops/mod.rs#L35)) as "demand-driven, first cross-op consumer." The op-graph *is* that first consumer — GenTags stops being optional the day this plan starts.
6. **Binding overhead was worth benchmarking but not worth worrying about** — 5-11 ns per lookup, three orders below op cost. Strategy choice is unaffected by naming cost.
7. **Unasked-but-relevant:** the owner's active non-destructive direction ([MODIFIER_STACK_PIPELINE.md](../MODIFIER_STACK_PIPELINE.md)) is a voxel-remesh modifier pipeline whose boolean runs in voxel space precisely to dodge the mesh boolean's weaknesses. The op-graph plan and that pipeline overlap; deciding their relationship (op-graph as the poly pillar's history, voxel stack as the organic pillar's) belongs before any build starts.
8. **One thing the plan under-claimed:** more infrastructure exists than "none" — `PolyEdit` + `apply_in_place` + parametric redo is a working single-object op log in the undo stack today ([undo.rs:333-350](../core_engine/app/src/undo.rs#L333)), and the identity audit's verdict is that the *shape* of the answer (ops-as-data) is already proven end-to-end for 22 operations.

---

## Recommended first spike (≤3 weeks)

**Make ONE rebuild-tier op local and ID-stable, then re-run measurement C.** Loop cut is the right specimen (self-contained quad-strip retessellation; already documented as the op that forced its own demotion out of delta undo, [mesh_ops/mod.rs:379-380](../core_engine/core_math/src/mesh_ops/mod.rs#L379)):

1. Implement `loop_cut_multi` in-place: split only the crossed quad strip's faces/edges, preserve all untouched element IDs, no `from_polygons` (the half_edge.rs freeze is lifted for poly work; the one red line — serialized shape — is untouched by this).
2. Region-bounded normal recompute (dirty faces + their vertex fans only) — this alone also un-breaks `extrude_face`'s scaling for free.
3. Return the remap/parent table for the strip — the first real `TopoDelta` population, which R3 needs anyway.
4. Re-run `op_bench` C: success = loop cut on XL ≈ loop cut on S (≈1 ms, ring-proportional), byte-identical two-process replay, `validate()`/oracle suite green.

This attacks the **riskiest measured assumption** (locality — kill criterion C, the one that gates every replay strategy per §2.3), produces the remap machinery the naming layer needs, and is honestly falsifiable in three weeks: if an in-place loop cut can't be made ID-stable and ring-proportional on this kernel, the op-graph needs a different kernel, and you want to know that now. The boolean is deliberately *not* the spike — its verdict is already known (replace; evaluate Manifold/exact-predicate options in parallel as a decision process, not an experiment). The 1-2-day face-tag retrofit to the boolean stitcher is worth doing opportunistically regardless, since it also fixes the shipped material_id loss.
