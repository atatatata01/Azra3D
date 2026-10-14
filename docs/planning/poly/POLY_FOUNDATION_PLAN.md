# POLY_FOUNDATION_PLAN.md

## AZRA Polygon Modeling Foundation — Final Architecture

**Status:** Authoritative. The kernel-architecture decision for AZRA's first-class polygon pillar. Supersedes the hard freeze on `half_edge.rs` with a *scoped, gated* lift.
**Scope:** The polygon modeling subsystem (`core_math/src/half_edge.rs`, `bevel_op.rs`, the new `mesh_ops/` module, `NodePayload::PolygonalBody`, the runtime `PolyMesh` wrapper). This is the *hard-surface manifold-first* modeler — CPU-authority, transaction()/episode undo — NOT the voxel sculpt subsystem (GPU-authority, `BRUSH_SYSTEM_ARCHITECTURE.md`) and NOT the SDF body system.

---

## 0. THE DECISION (read first)

**Chosen kernel: HEX-BL — "BMesh-Lite, the evolved half-edge foundation."**
Grow AZRA's proven typed-ID half-edge kernel **in place** into a BMesh-Lite. We do **not** rewrite the kernel, **not** fork bmesh-rs, **not** adopt an attribute-centric SoA topology decouple, and **not** retype any serialized field. Instead we add four *additive* layers, all gated **bevel-v1-first**, all carried on a non-serialized runtime wrapper:

1. A runtime **CustomData** SoA sidecar (UV / crease / seam / sharp / vcolor) with a per-op-verified **Interp propagation contract** — attributes flow *only* through ops that natively emit a parent-map.
2. **Demand-driven generation-tagged handles** carried on the runtime wrapper (never inside the frozen mesh), introduced only when the first cross-op consumer lands.
3. Typed **MeshOps** generalizing `bevel_op.rs`'s offset-rail + WeldMap pattern — **bevel v1 first**, with **zero `half_edge.rs` edits**.
4. A **runtime op-history DAG** that is a *projection* over the persisted `Vec<PolyModifier>` — never a retyped serialized field — giving delta + parametric undo, attribute flow, and a credibly-scoped path to exceed Blender.

**The load-bearing principle (the one rule that governs every layer):**
> **All new poly state is RUNTIME state on a wrapper. The serialized substrate — `HalfEdgeMesh` and `Vec<PolyModifier>` — is NEVER extended in-place. Persistence of new authored data rides IN-BLOB `#[serde(default)]` fields on `ProjectFile`, mirroring the proven `voxel_grids`/`VoxelNodeRecord` precedent.**

The codebase already proves this pattern is right: `bevel_op.rs` is *pure geometric math* that reads the public half-edge API and rebuilds via `from_polygons` + a tested `WeldMap`, leaving the kernel untouched. We are generalizing what already half-ships, not inventing a new mesh kernel.

### Why half-edge-evolve, and not the alternatives

| Candidate | Verdict | Reason |
|---|---|---|
| **Evolve current typed-ID half-edge (HEX-BL)** | **CHOSEN** | Production-ready *today* for quad-dominant manifold modeling (tests pass, Catmull-Clark correct, primitives render, `to_raster_data` GPU-ready). Typed IDs already prevent the index-aliasing class. `bevel_op.rs` proves the pure-op pattern. Additive layers carry zero kernel-rewrite risk. |
| **Fork / port bmesh-rs as the base** | **REJECTED — STUDY-ONLY** | Stale (last commit 2018, ~7★), unmaintained, no published crate, no GPU/undo/FL4 integration proof. Forking = solo maintenance of a dead codebase. **HIGH-VALUE REFERENCE** for the radial/disk-cycle encoding and operator design (its author wrote Blender's BMesh) — read it; never depend on it. |
| **Attribute-centric SoA topology decouple (Houdini-style)** | **REJECTED for v1** | Migrating `HVertex.position` / `HFace.material_id` out of the structs into SoA columns is a **3–5 week regression risk** across ~81 internal callers + app readers — both research panels concede this. We get the *attribute* win from a **sidecar** (extensible data SoA) while geometry stays AoS (GPU-upload ready). No kernel surgery. |
| **alum / OpenMesh as kernel** | **REJECTED — REFERENCE-ONLY** | AZRA's kernel (newtype IDs, free-list, `he.vertex=DESTINATION`, `validate()`) is *stricter* than both. Adding a second half-edge crate creates dual-kernel confusion. Borrow subdivision pseudocode (Loop/Sqrt3) if ever needed; do not depend. |
| **New `PolyMesh` NodePayload variant** | **REJECTED** | Orphans the already-committed `NodePayload::PolygonalBody` (variant index 6) + its modifier stack, and tempts a save-format break. We reuse the committed variant verbatim. |
| **f64 "P" CAD-precision kernel now** | **DEFERRED (typed STUB behind a flag)** | A genuine differentiator for robust booleans, but a multi-quarter retrofit. Recorded as a future opt-in column; not built in this plan. |

**What we explicitly REFUSE to do (the red lines that protect the save format):**
- **Never add a field to `HalfEdgeMesh`.** It derives `Serialize` and rides the FL4 blob in-blob via `Arc<HalfEdgeMesh>` (the `arc_serde_roundtrip` test serializes it *through* bincode). bincode is positional — a new field shifts bytes and corrupts **every** existing `.fl4` (manifesting as a misread *next* enum variant). All new state lives on the non-serialized `PolyMesh` wrapper.
- **Never `#[serde(skip)]` `PolygonalBody.mesh`.** It is in-blob and is *expected* to round-trip through bincode; skipping it corrupts every existing poly save.
- **Never add a second trailing magic after `FL4ORDER`.** The FL4ORDER loader does `bincode::deserialize(tail)` (`project.rs:356`), which requires the slice to be *exactly one object*. A trailing `[FL4POLY][blob]` makes the existing FL4ORDER deserialize error on the leftover bytes — corrupting any file that has **both** an authored `child_order` and authored poly layers. Authored layers persist as a NEW `#[serde(default)] Vec<PolyLayerRecord>` **field on `ProjectFile`** instead (the `voxel_grids` precedent).
- **Never retype `modifiers: Vec<PolyModifier>`.** It is the trailing field of an in-blob variant; replacing its *type* (Vec → DAG node) is a bincode-layout break that corrupts every saved `PolygonalBody` even though no variant index moves. The DAG is a `#[serde(skip)]` runtime projection rebuilt from the Vec.

---

## 1. NORTH STAR

**AZRA polygon modeling must beat Blender on the things Blender got architecturally wrong, and meet it on everything else — without ever risking the save format or the frame budget.**

Concretely, a year from the first commit:
- A modeler can extrude / inset / bevel / loop-cut / dissolve / subdivide a quad-dominant hard-surface mesh, with UVs and creases that **survive** the operations that can carry them (and *loudly declare* a drop on the ones that can't yet).
- Undo on a local edit costs **O(touched)**, not a whole-scene copy; a past op can be **re-opened and re-parameterized** (true parametric redo Blender's snapshot undo cannot do).
- The mesh round-trips through FL4 with its custom data, renders as native triangles in Object Mode alongside SDF + voxel (Display Invariant), and the whole thing extends with `#[serde(default)]` in-blob fields so the format **never** risks the variant-shift corruption class.

This is not "port Blender." It is "keep the clean kernel, add the four things BMesh proved valuable, and structurally refuse the dozen things BMesh got wrong."

---

## 2. BLENDER MISTAKES WE REFUSE TO REPEAT

The single most important section. Each row is a documented BMesh limitation and the AZRA mechanism that avoids — or beats — it.

| Blender / BMesh mistake | How AZRA avoids / beats it |
|---|---|
| **Full-mesh-copy undo** (T88044): a 1M-vert edit clones 1M verts into the undo stack | Delta op-records + **Arc-COW per body**. O(touched) undo for LOCAL ops (extrude one face, move verts). Whole-mesh ops (subdivide, bevel-all) fall back to a **per-BODY** Arc snapshot — O(body), still strictly cheaper than Blender's whole-**scene** copy. |
| **Per-element malloc / pointer-soup cache misses** on dense adjacency walks | Dense typed-ID `Vec<HVertex>/Vec<HalfEdge>/Vec<HFace>` (cache-contiguous, free-list reuse). Disk/radial cycles are **angle-sorted index sidecars on the runtime wrapper**, never per-element heap pointers, never new fields on the in-blob mesh. |
| **Undefined radial/disk order** (mesh-dependent, non-reproducible bakes) | `TopoIndex` stores **deterministic angle-sorted** fans, patched incrementally on `*_mut` ops. Adjacency walks and face-visit order are reproducible — testable ops, reproducible bakes. |
| **Untyped operator slot-dict** (`op.outputs['geom.out']`, read the C to learn the slots) | `trait MeshOp` with compile-time `Params` / `EditResult` + a `PROPAGATES` capability flag. No string lookup, no runtime slot dict — every caller is type-checked. |
| **Custom-data double-indirection** (`uv_layers[name][loop]`) | Explicit typed domains: UV/seam/sharp keyed by `HalfEdgeId`, crease by `UndirectedEdgeKey`, color by `VertexId`. Direct `column[id]` — no layer-name walk. |
| **Silent attribute loss across operators** | Every op that can propagate returns a parent-map; the Interp contract auto-propagates **only** for ops past their per-op `layer_flow` gate. Ops without a verified parent-map set `PROPAGATES=false` and **declare the drop loudly** (UI surface + `debug_assert`) — never silently drop. |
| **Modifier-stack vs Geometry-Nodes dual system** (two architectures, neither deep) | **ONE** op-history DAG that is a runtime projection over the persisted `Vec<PolyModifier>`. Modifiers and edit-mode ops share the **same MeshOp code path** (Bevel-as-tool == Bevel-as-modifier). The serialized field is never retyped. |
| **N-gon subdivision interior bug** (T31537, topological half: subdivide only splits the boundary, leaves the interior) | `subdivide_catmull_clark`'s n-gon handling is **audited and fixed in the op** (face-point + per-edge split → n quads, interior vertex created), gated by a pentagon→5-quads test — **NOT** masked by a display-only limit-surface evaluator. |
| **N-gon tessellation shading bands** (T31537, shading half: fan-from-vertex-0 on non-planar n-gons) | Object Mode stays **native triangles**; read `sharp` for split normals; never assume n-gon coplanarity. The Display Invariant already enforces this. |
| **Stale-index aliasing** (freed slot reused, old reference acts on new element) | Demand-driven **generation-tagged handles** (on the wrapper, not the frozen mesh): a freed-then-reused slot is un-resolvable for a stale `Handle{id,gen}`. Element selection is Handle-native from its first commit — no half-migrated raw-id state. |
| **Universal non-manifold seek tax** (BMesh always promises non-manifold → pays O(K) radial seek on every adjacency query, including the 95% manifold case) | **Manifold-by-default** with an `assert_manifold()` O(2) twin fast path. Non-manifold is **opt-in overflow** via the radial sidecar; `classify()` is a NEW method, never weaker than `validate()`. |
| **Silent save corruption from format fragility** | Authored layers ride an IN-BLOB `#[serde(default)]` field (the real `voxel_grids` precedent), never a second trailing magic that breaks FL4ORDER's `deserialize(tail)`; `HalfEdgeMesh` gets **zero** new fields; a bincode **sentinel-variant guard test** catches any future non-skip field in CI. |
| **Single-threaded BMesh→Mesh conversion** (elements don't know their indices; needed a 2024 two-phase rework) | Typed-ID elements give `par_iter` over verts/faces with **zero index-lookup** — parallel-ready by construction. |
| **Editing state baked into the mesh** (selection/hide flags live on the BMesh → deep-copy to branch) | Editing context lives in `SceneState`; the mesh stores geometry + topology only. Branching an edit forks the undo stack, not the mesh (Arc substrate makes it free). |

---

## 3. WHERE WE EXCEED BLENDER

Not just "avoid the mistakes" — these are capabilities Blender's architecture cannot cheaply add.

- **Delta + parametric op-record undo** for local edits, **per-BODY** (not per-scene) snapshot for global ops: re-open a past op, change its param, re-evaluate downstream. Blender's state-snapshot undo loses op semantics and copies the whole scene.
- **Generation-tagged handles**: structurally impossible to act on a freed element. Blender's pointer/index model has no equivalent guard.
- **Declarable-manifold fast path**: `assert_manifold()` guarantees O(2) twin walks Blender can't, because it *always* promises non-manifold.
- **Typed custom-data domains with a per-op-verified Interp contract** — no silent attribute loss (ops that can't yet propagate declare it loudly), no slot-name guessing.
- **Arc-COW bodies**: undo / branch a small edit without copying large *untouched* bodies (Blender deep-copies the whole BMesh). Verified: the Arc substrate already ships in the scene graph.
- **Single op-history DAG as a runtime projection** over the persisted modifier Vec — no modifier-stack-vs-nodes split, no save-format break.
- **Limit-surface subdivision DISPLAY** (OpenSubdiv eval-only, Apache-2.0) backed by a **topologically-correct quad-emitting** Catmull-Clark bake — arbitrary level, low memory, no hidden n-gon interior bug.
- **Default-correct USD/primvar export** (corner-uv→faceVarying, edge-crease→creases, face-material→GeomSubsets) — beats Blender's lossy USD by construction.
- **Opt-in f64 "P" CAD-precision column** behind a flag for robust booleans — a retrofit Blender's f32-everywhere kernel can't cheaply make.
- **Deterministic angle-sorted adjacency order** → reproducible bakes and testable ops where Blender's undefined radial order causes mesh-dependent results.
- **Format-safe extensibility**: all new persistent poly data rides in-blob `#[serde(default)]` fields guarded by a bincode sentinel test — the format extends without ever risking variant-shift corruption.

---

## 4. THE KERNEL — DATA MODEL, INVARIANTS, AND THE TEST SPINE

### 4.1 The substrate stays; the wrapper carries everything new

Verified against the live repo: `HVertex.position` (`half_edge.rs:34`), `HFace.material_id` (`:53`), trailing field `free_faces` (`:68`), `validate()` (`:331-350` = next/prev + twin only). The substrate is unchanged:

```
Vec<HVertex> / Vec<HalfEdge> / Vec<HFace>   // typed newtypes, free-lists,
                                            // he.vertex = DESTINATION, NO_TWIN / NO_FACE sentinels
```

`HVertex.position` and `HFace.material_id` **stay concrete struct fields** — SoA-migrating them is the 3–5 week regression both runner-ups conceded. Geometry stays AoS (GPU-upload ready). Extensible attributes go SoA — in the **sidecar**, never on the structs.

The **runtime wrapper** is the key serialization-safety divergence:

```rust
// NOT serialized. Lives in app/cache state, rebuilt on load. ZERO freeze-lift to introduce.
struct PolyMesh {
    topo:     HalfEdgeMesh,   // the serialized substrate (Arc in the node)
    cd:       CustomData,     // §4.2 — runtime SoA sidecar
    gen:      GenTags,        // §4.4 — demand-driven, introduced with first cross-op consumer
    topo_index: TopoIndex,    // §4.3 — disk + radial cycles, derived
    topo_gen: u64,            // bumps on topology change → drives lazy re-eval + index patch
}
```

`CustomData`, `GenTags`, `TopoIndex`, and `topo_gen` live **on the wrapper, never as fields of `HalfEdgeMesh`.** This is the red line that keeps the save format intact.

The body owns `Arc<HalfEdgeMesh>` + the persisted `Vec<PolyModifier>` on the **existing** `NodePayload::PolygonalBody { name, mesh, mesh_cache_id, material, modifiers }` (verified `scene_graph.rs:363-370`, variant index 6).

### 4.2 Custom data model (the attribute win without kernel surgery)

Per-element / per-loop layers in a SEPARATE SoA `CustomData` sidecar on the runtime wrapper. Domains are **explicit and type-checked**, killing BMesh's double-indirection:

```rust
struct CustomData {                              // all columns dense, parallel to the element vector
  uv:     LayerStore<HalfEdgeId, Vec2>,          // CORNER domain — UV differs per face-corner
  seam:   BitLayer<HalfEdgeId>,                  // mirrored to twin
  sharp:  BitLayer<HalfEdgeId>,                  // hard-edge / autosmooth split
  crease: LayerStore<UndirectedEdgeKey, f32>,    // EDGE — Catmull-Clark clamp 0..1
  vcolor: LayerStore<VertexId, [u8;4]>,
  vcrease:LayerStore<VertexId, f32>,
  extra:  HashMap<LayerKey, DynLayer>,           // (domain,name) -> typed column
}
```

**The Interp propagation contract (grafted from the STRATA research, with the per-op correction folded in).** Attributes auto-propagate **for free ONLY for ops that natively emit a correct parent-map.** Each column declares `Interp ∈ {Constant, Copy(parent), Lerp(parents,w), Average(ring), Zero}`. Every MeshOp that supports propagation returns a `TopoDelta` carrying `new_id → source element(s) + weights`; a generic `apply_delta` propagates each column by its Interp — but ONLY over the touched parent-map set (Frame Budget: never a full-column sweep).

The 11 existing Euler ops emit **no** parent-map today and mutate in place. Reconstructing one *post-hoc from outside* the op (especially `subdivide_catmull_clark`'s face/edge/vertex-point crease-weighted masks) is exactly Blender's error-prone hand-wiring. Therefore the parent-map for any op that creates/weights new elements **must be emitted from INSIDE that op** (a scoped freeze-lift on that specific function — §6.5). Rollout is op-by-op:

- **Stage A:** CustomData with `Interp = Copy/Constant` only — trivially-preserving ops (extrude, inset) keep UV with no weight math.
- **Stage B:** add parent-map + `Lerp`/`Average` ONE OP AT A TIME, each gated by its OWN per-op test (`uv_preserved_after_extrude` ships before `crease_survives_subdivide`; also `uv_after_loop_cut`, `uv_after_dissolve`). The `layer_flow` matrix is **per-op**, not one matrix.
- Until an op has a proptest-verified parent-map it MUST **declare "drops layers" loudly** (a per-op `PROPAGATES: bool` capability flag the UI surfaces + a `debug_assert`), never silently drop. **"Attributes flow for free" is demoted to "for ops that natively emit a parent-map."**

Immediate payoff: `subdivide_catmull_clark` reads `crease` to clamp edges; `to_raster_data` (`half_edge.rs:1376/:1394`, hardcodes `uv=[0,0]`) reads `uv` for the live slot and `sharp` for per-face split vs averaged normals.

### 4.3 Non-manifold strategy — TOLERATE, don't promise

AZRA poly is hard-surface manifold-first. Non-manifold support exists only so import / boolean / conversion never corrupt or panic — **not** as a modeling target.

- `twin` is the canonical 2-manifold link and hot path. `TopoIndex.radial` is the overflow: for an undirected edge with N≥3 incident half-edges, set twin to `NO_TWIN` with a `non_manifold` marker carried on the **wrapper's** edge sidecar (NOT a new `HalfEdge` field — red line), so "NO_TWIN here" means *consult radial*, distinct from a true boundary. `disk = TopoIndex.disk[v]`, **angle-sorted** (deterministic, vs BMesh undefined). Manifold = radial len 2, boundary = 1, non-manifold = ≥3.
- **A CLASSIFIER, added as a NEW method, never weakening today's guard.** `classify() -> MeshClass{ Manifold, ManifoldWithBoundary, NonManifold{edges,verts} }` is added ALONGSIDE `validate()` (`:331-350`), which stays **byte-for-byte intact** and still called everywhere it is now. `classify()` adds three production invariants: (a) orphan-vertex (every vertex reachable from some face), (b) interior-edge-has-exactly-2-faces (via radial), (c) boundary loops closed + `face.half_edge` points onto its own face. **ROLLOUT GATE:** ops route through `classify()` **only after** `classify()` has proptest coverage — a half-done classifier returning `Manifold` for a broken mesh would HIDE violations (the exact No-Bandaids "downgrade assert to log" pattern).
- `assert_manifold()` lets a caller DECLARE manifold and take the guaranteed O(2) twin walk — the performance lever Blender structurally can't pull.
- Editing ops on non-manifold regions **fail cleanly** (`Err(OpError::NonManifoldUnsupported{edge})`) — never emit-and-warn, never emit garbage. Sanctioned cleanup ops (split non-manifold edge, delete loose geometry) are the way out, added incrementally.

Intentionally LESS than a bmesh-rs full non-manifold modeler, MORE than today's manifold-only kernel. bmesh-rs is STUDY-ONLY: read its disk/radial encoding only if an opt-in non-manifold tier is ever built; never fork.

### 4.4 Stable handles — demand-driven, never speculative

Generation vectors are **not** added speculatively to the kernel. Element selection is NET-NEW (UnifiedSelection stays `SceneItemId` for bodies/prims; there is no legacy raw-id element selection to migrate).

- Build element-select FIRST with raw ids scoped to a SINGLE op/frame (safe — no aliasing within one op).
- Introduce `GenTags{ vgen, egen, fgen }` on the `PolyMesh` wrapper (NOT in `HalfEdgeMesh` → zero freeze-lift) ONLY when the first cross-op consumer lands (persistent element selection or falloff caches across drag frames), in the SAME task as that consumer.
- `Handle{ id:u32, gen:u32 }` is the public currency; `resolve(h) -> Option<Index>` checks gen, killing ABA aliasing. Raw ids stay kernel-internal for hot traversal. **RULE enforced in review: no raw element id ever escapes `core_math` into app selection/undo/tool caches.**

### 4.5 The Euler-operator core + fuzz/golden harness — THE SPINE

The 11 existing TESTED Euler ops (`split_edge`, `extrude_face`, `inset_face`, `loop_cut`, `merge_vertices`, `delete_face`, `dissolve_edge`, `dissolve_face`, `flip_face`, `subdivide_catmull_clark`, `subdivide_faces`) are the foundation. The test harness is the **spine of the whole project** — every layer above is gated on it:

- **`validate()` stays the topology guard** (next/prev + twin). `classify()` adds the production invariants (§4.3) as a NEW method.
- **`mesh_eq(deep)` oracle** — the strengthened validation oracle. The `apply→inverse→apply` proptest must restore FULL structural + attribute equality, NOT just `validate()` topology: positions within epsilon AND every CustomData column byte-equal AND consistent `topo_gen` behavior. Today's `validate()` (next/prev + twin only) would PASS a mesh whose UVs/positions were silently corrupted by a wrong inverse delta. Every op proptest uses `mesh_eq` before that op may claim delta-undo.
- **Per-op `layer_flow` test** — ships in the SAME PR as that op's parent-map (Gate 2c). `uv_preserved_after_extrude`, `crease_survives_subdivide`, `uv_after_loop_cut`, `uv_after_dissolve`, pentagon→5-quads (subdivide interior).
- **Proptest fuzzing** — random mesh generation + topology checks (orphan-vertex / interior-edge / boundary-loop), green before `classify()` routes any op.
- **bincode sentinel guard** — serialize a `HalfEdgeMesh`, append a sentinel variant after it inside a `PolygonalBody`, assert the sentinel reads back. CI guard catching any future non-skip field on `HalfEdgeMesh`.
- **FL4 round-trip suite** — authored-body round-trip; pre-`PolyLayerRecord` poly file loads with empty CustomData; a file with BOTH a custom `child_order` AND authored poly layers loads correctly; the forged-v7 "no FL4ORDER magic" test still byte-identical.

`cargo test -p core_math` runs the whole spine continuously. No layer merges with a red spine.

### 4.6 Undo model — DELTA + PARAMETRIC, benchmark-honest

Two tiers riding the EXISTING transaction() / episode model (`save_undo`/`save_delta_undo` stay deleted):

- **(1) OP-RECORD (parametric history):** `OpRecord{ op, params, selection: Vec<Handle>, inverse: OpDelta }`. Each transaction pushes one record; undo applies `inverse`, redo re-runs `apply`. Params are RETAINED → re-open a past op, tweak a param, re-evaluate downstream — true parametric redo.
- **(2) OP-DELTA (the memory win):** `OpDelta{ verts_changed, edges/faces added/removed, layer_changes }`. Inverse is NOT a mesh clone.

**Honest cost model (the corrected claim):** O(touched) undo holds for LOCAL ops (extrude one face, move verts). WHOLE-MESH ops — `subdivide_catmull_clark` and bevel-all-edges, which are among the MOST COMMON destructive poly ops and touch ~all elements — fall back to a per-BODY Arc snapshot, so their undo is **O(body), NOT O(touched).** This is still strictly cheaper than Blender's whole-SCENE copy (T88044). Additionally: Arc-COW only avoids copying *other* bodies; within the edited body, `make_mut` clones the whole `HalfEdgeMesh` on first touch per undo step — so the in-place `*_mut` path emitting a real element-delta (NOT the snapshot) is the REQUIRED path for split/extrude before claiming the dense-single-body local-edit win.

Integration is verified-real: `PolygonalBody.mesh` is `Arc<HalfEdgeMesh>` (`scene_graph.rs:365`, passing undo round-trip `:1951-1972` / `arc_serde_roundtrip :1978`), so structural sharing is FREE. `transaction()` (atomic) and `begin/commit_episode()` (drag = one coalesced record, deltas merged each frame, `auto_commit_episodes` catches leaks) map directly on. A `topo_gen` counter detects no-op replays.

### 4.7 Operator layer — Rust-native typed ops

Reject BMesh's untyped slot-dict. Each op is compile-time typed:

```rust
trait MeshOp {
  type Params: Serialize + Clone;        // parametric inputs, retained for re-edit
  const PROPAGATES: bool;                // false = op declares 'drops layers' until its parent-map ships
  fn apply(&self, m: &PolyMesh, sel: &[Handle], p: &Self::Params)
      -> Result<EditResult, OpError>;    // EditResult = new PolyMesh OR (TopoDelta + parent-map + inverse OpDelta)
}
```

Ops live in `core_math/src/mesh_ops/` (split.rs, extrude.rs, inset.rs, loop_cut.rs, dissolve.rs, bevel.rs, weld.rs, subdivide.rs). Ops stay PURE where rebuild is cheap (`mesh → params → from_polygons` via WeldMap) and become in-place `*_mut` only where O(1) locality matters (split_edge / extrude), behind `validate()` in debug and op-records always. A thin app glue layer in `mesh_actions.rs` calls ops inside `transaction()` / episodes — the kernel never owns editing context.

**Falloff (Modo/PHENON graft):** `FalloffProfile{ radius, curve }` is an optional `Params` field on transform-class ops, **SHARED with the existing brush kernel** (`BRUSH_SYSTEM_ARCHITECTURE.md`) — not a duplicate primitive.

### 4.8 Non-destructive evaluation / modifier model — ONE DAG, as a projection

ONE parametric op-history DAG that unifies undo + modifier stack + node graph (PHENON's cleanest idea) — built as a RUNTIME PROJECTION, never a serialized-field retype. The infra partly EXISTS: `PolygonalBody` carries `modifiers: Vec<PolyModifier>` with 8 types (Subdivide / Mirror / Solidify / Smooth / Triangulate / Decimate / Array / Bevel — `scene_graph.rs:192-200,363-370`).

**Save-format strategy pinned before building (hard gate):**
- `modifiers: Vec<PolyModifier>` stays the persisted wire format **permanently**. New `PolyModifier` variants are APPENDED at the END only (enum append-only rule).
- The DAG is a `#[serde(skip)]` RUNTIME view rebuilt from the Vec on load: a linear modifier stack maps to a chain DAG trivially.
- Replacing the field's TYPE (`Vec<PolyModifier>` → a DAG node type) is **FORBIDDEN** — a bincode-layout break on the trailing field of an in-blob variant.
- Branching-DAG topology, when needed, persists as a NEW `#[serde(default)] Vec<PolyDagEdgeRecord>` field on `ProjectFile` (same in-blob precedent as `voxel_grids`/`PolyLayerRecord`), reattached by `node_id` — never by mutating the modifiers field.

**Model:** base half-edge mesh (edit-mode authority, the Arc) + the runtime op-history DAG → a derived display/bake mesh. Each node is a pure `eval(PolyMesh) -> PolyMesh` keyed on `(input topo_gen, param hash)` for LAZY re-eval from the changed node forward. Modifiers and edit-mode ops are the SAME MeshOp implementations — Bevel-as-modifier and Bevel-as-tool share ONE code path.

**Subdivision topology correctness (the deeper T31537 half):** the OpenSubdiv-style LIMIT-SURFACE evaluator (Apache-2.0) is **DISPLAY-ONLY**; the BAKE/EDIT topology comes from a correct quad-emitting Catmull-Clark. `subdivide_catmull_clark`'s n-gon handling MUST be audited directly: if it fans an n-gon from vertex 0 (naive tessellation leaving the interior unsplit), that is the real T31537 bug and is FIXED in the op (face-point + per-edge split → n quads, interior vertex created), NOT papered over by the display layer. Gated by an n-gon subdivide topology test (pentagon → 5 quads, interior vertex created).

---

## 5. OSS USAGE — REFERENCE vs BORROW

The rule: **build-custom > fork > adopt.** New operators come in via pure modules reading the half-edge public API (the `bevel_op.rs` pattern), or external crates plugged at a boundary (convert to/from plain index+vertex buffers). Never refactor or duplicate the kernel. No GPL linking — GPL sources are read-only algorithmic references.

| Source | License | Role | How AZRA uses it |
|---|---|---|---|
| **bmesh-rs** | MIT | **STUDY-ONLY** | Reference for non-manifold disk/radial encoding + operator API shape (its author wrote Blender's BMesh). Cherry-pick pseudocode; **never fork/depend** (stale 2018, ~7★, no GPU/undo/FL4 proof). |
| **boolmesh** | MPL-2.0 (file-level copyleft) | **BORROW (provider)** | Mesh boolean node. Wrapped in a boundary-respecting `core_math` module: `HalfEdgeMesh → to_polygons → boolmesh → from_polygons`, f64 internally, SDF-roundtrip fallback on degenerate input. Modifications to boolmesh source stay open; our wrapper stays proprietary. |
| **baby_shark** | MIT | **BORROW (provider)** | Decimate / remesh modifier fast-path (QEM decimation). Crate or FFI; pipes through `from_polygons`. Custom QEM only if crate friction outweighs benefit. |
| **OpenSubdiv** | Apache-2.0 | **BORROW (display eval)** | Limit-surface subdivision **DISPLAY-ONLY**. Never the source of edit/bake topology. Thin Rust ecosystem → port the GPU eval strategy (~300-line compute) or FFI; not the modeler. |
| **xatlas** | MIT | **BORROW (FFI, deferred)** | UV unwrap/pack. FFI binding in `io_bridge` (`xatlas-sys` + safe wrapper). Export-time, not in-viewport. Already flagged CRITICAL; not in this plan's critical path. |
| **alum / OpenMesh** | Apache-2.0 | **REFERENCE-ONLY** | Half-edge API ergonomics + Loop/Sqrt3 subdivision pseudocode. AZRA's kernel is stricter; do not add as a dependency (dual-kernel confusion). |
| **Blender `bevel.c`** | GPL-3.0 | **READ-ONLY REFERENCE** | Quality benchmark for offset-rail + corner-patch n-gon. Cannot link/port. AZRA's `bevel_op.rs` independently converged on the same design — validate against it, source nothing. |
| **CGAL** | GPL-3.0 (dual) | **READ-ONLY REFERENCE** | Robust predicate chaining (sign-of-determinant CCW tests), offset-surface math. Re-implement predicates from the published papers; never link. |

Document all external sources in a `SOURCES.md` (owner decision on visibility).

---

## 6. AZRA INTEGRATION

### 6.1 Display Invariant — poly is the easiest citizen

Object Mode = triangles only, one technique. Poly: eval the DAG, call `to_raster_data()` (`half_edge.rs:1376` → `[px,py,pz,nx,ny,nz,u,v]` + u32 indices, per-face split verts, CCW matching `wgpu FrontFace::Ccw`), upload to `mesh_cache`, rasterize into the ONE Object-Mode G-buffer alongside SDF-baked + voxel-meshed triangles. Two concrete fills: wire the per-corner `uv` layer into the hardcoded `[0,0]` slot (`:1394`); read `sharp` for split-vs-averaged normals. Edit Mode = that object's single technique (raster of the edit mesh), no fused raymarch.

### 6.2 FL4 save — additive, in-blob, zero variant churn

Reuse the EXISTING `NodePayload::PolygonalBody` (variant index 6 — mid-enum). DO NOT add a new variant. The three hazards and their resolutions:
1. `PolygonalBody.mesh` is IN-BLOB (verified `arc_serde_roundtrip` serializes it through bincode). **Red line:** never serde-skip the mesh; never add a field to `HalfEdgeMesh`.
2. Authored layers (uv/crease/seam/sharp) persist as a NEW `#[serde(default)] Vec<PolyLayerRecord>` field on `ProjectFile`:
   ```rust
   PolyLayerRecord{ node_id:u64, uv:Vec<(u32,Vec2)>, crease:Vec<(EdgeKey,f32)>, seam:Vec<u32>, sharp:Vec<u32> }
   ```
   — mirroring `voxel_grids`/`VoxelNodeRecord` EXACTLY, reattached by `node_id` on load. Safe because `ProjectFile` is a STRUCT (not a bincode-tagged enum): a trailing `#[serde(default)]` field is the proven pattern. **NOT** a second trailing magic (which would break FL4ORDER's `deserialize(tail)`).
3. Branching DAG topology, if ever built, rides a `#[serde(default)] Vec<PolyDagEdgeRecord>` field — never a retyped modifiers field.

CustomData / TopoIndex / gen / topo_gen are runtime-only on the wrapper. bincode big-serialize runs on the dedicated 8MB-stack thread (verified `save_binary`/`load_binary`). Append-only-tail discipline (new `NodePayload` variants AFTER `SdfObject=11`) is recorded for any future variant. **Mandatory before merge** (the FL4 suite, §4.5): authored-body round-trip; pre-`PolyLayerRecord` file loads with empty CustomData; both-`child_order`-and-poly-layers file loads correctly; forged-v7 byte-identical; sentinel guard.

### 6.3 Frame ordering / GPU / picking / gizmos

- Modifier eval + `to_raster_data` happen in `poll_background_tasks` (or on scope-leave), bind groups in `viewport_render`, never in panels. `mesh_cache` uses a **generation counter** so a re-evaluated mesh invalidates its bind group structurally.
- **Frame Budget caps are EXPLICIT**, documented next to the existing bake/DC/mesh caps: `apply_delta` propagation bounded to the touched parent-map set (never a full-column sweep); `TopoIndex` patched incrementally for `*_mut` ops (full rebuild only on pure-rebuild ops); global ops (Catmull-Clark) route through commit-cooldown at DRAFT resolution via the InteractionRenderPolicy latch the SDF path uses — never per-interaction-frame at full quality.
- **Picking / gizmo stay decoupled** from the kernel (ray-cast against the rendered triangle mesh). Vert/edge/face element-select is NEW app-layer interaction storing `Handle{id,gen}` from its first commit — no legacy raw-id element selection to coexist with. UnifiedSelection stays `SceneItemId` for bodies/prims.

### 6.4 Undo / transaction

Ops run inside the existing `transaction()` / episode model; Arc-COW per body means undo touches only that body (§4.6).

### 6.5 Freeze-lift scope — decoupled and explicit

**Bevel v1 lands FIRST with ZERO `half_edge.rs` edits** (`bevel_op.rs` is pure; `bevel_edges_ex` retirement is app-layer in `poly_drag_tools.rs:709`). The CLAUDE.md ~line 352 freeze amendment is a SEPARATE, LATER gate scoped explicitly to:
- (a) ADD `classify()` + the 3 invariants as a NEW method, leaving `validate()` byte-for-byte intact;
- (b) internal edits to specific Euler ops to emit parent-maps + inverse deltas (notably `subdivide_catmull_clark`'s provenance/weights);
- (c) subdivide n-gon topology correctness.

Each is gated on full `cargo test -p core_math` green + gold-tier review **per op/step**. `classify()` must be proptest-covered and "never weaker than `validate()`" before any op routes through it. bmesh-rs recorded STUDY-ONLY.

**USD export (STRATA graft, deferred post-op-core):** corner uv→faceVarying primvar, edge crease→creases, face `material_id`→GeomSubsets, points/faceVertexIndices from `to_raster_data`.

---

## 7. PHASED ROADMAP

**Invariant for EVERY phase:** the shipped poly bodies, the existing modifier stack, and the SDF/voxel/Object-Mode render keep working at every commit. `cargo build -p app` + the test spine (§4.5) after each. Each phase is independently shippable and reversible.

### ★ FIRST MILESTONE — Phase 0: Decide, unfreeze, consolidate bevel, scaffold the spine

Record the HEX-BL decision + the freeze-lift scope (§6.5) in CLAUDE.md (amend ~line 352 to the scoped, gated form; do NOT blanket-unfreeze). Scaffold the `PolyMesh` runtime wrapper (empty `CustomData`/`TopoIndex`/`topo_gen`, no `GenTags` yet) — **all on the wrapper, zero `HalfEdgeMesh` fields.** Scaffold `core_math/src/mesh_ops/` + the `MeshOp` trait. Stand up the **test-spine skeleton**: `mesh_eq(deep)` helper, the bincode sentinel guard, the FL4 round-trip suite stubs, proptest scaffolding. **Retire the wrong-algorithm `bevel_edges_ex` caller and complete `bevel_op.rs` v1** (this IS the early bevel unblock — see Phase 1 for the gate). No behavior change beyond bevel. *This proves the wrapper + harness are the safe foundation and collapses the two bevel paths.*

### Phase 1 — Bevel v1 (the merge-blocker that unblocks everything)

Implement the TODO at `bevel_op.rs:77` (per-edge plane offset, corner line-intersection, corner-patch n-gon, affected-face rebuild) on the pure offset-rail + WeldMap path — **ZERO `half_edge.rs` edits.** It becomes the CANONICAL `MeshOp` reference template. Retire the frozen wrong-algorithm `bevel_edges_ex` caller (`poly_drag_tools.rs:709` — app-layer) in the SAME task so the two bevel paths collapse to ONE. **HARD MERGE GATE: nothing in phases 2+ merges until bevel v1 lands AND `bevel_edges_ex` is deleted, with the `apply→inverse→apply==identity` proptest green** (using `mesh_eq`). Depends on Phase 0.

### Phase 2 — Euler-op inverse deltas (delta undo)

Wrap all 11 tested Euler ops to emit INVERSE DELTAS ONLY (undo), NO parent-map (Gate 2a). Independently shippable; delivers delta-undo immediately via the existing transaction()/episode model. Needs only external observation of changed elements for the rebuild-path ops; the in-place `*_mut` element-delta path is REQUIRED for split/extrude before claiming the dense single-body local-edit win. `mesh_eq` gates every op's `apply→inverse` proptest. Depends on Phase 1.

### Phase 3 — CustomData sidecar, Copy/Constant only

Add the `CustomData` SoA sidecar on the wrapper with `Interp = Copy/Constant` ONLY (Gate 2b): extrude/inset preserve UV trivially. Wire the per-corner `uv` layer into `to_raster_data`'s hardcoded `[0,0]` slot (`:1394`) and `sharp` into split-vs-averaged normals. Add the FL4 `PolyLayerRecord` in-blob field + its round-trip suite (authored body; pre-record file with empty CustomData; both-`child_order`-and-layers file; forged-v7 byte-identical; sentinel guard). Depends on Phase 2.

### Phase 4 — Per-op Interp propagation (Lerp/Average, one op at a time)

Add parent-map + `Lerp`/`Average` ONE OP AT A TIME (Gate 2c), each in the SAME PR as its `layer_flow` test. Ops that create/weight new elements (notably `subdivide_catmull_clark`) emit the parent-map from INSIDE the op — **the scoped freeze-lift (§6.5b)** per-op, gated on full `cargo test -p core_math` green + gold-tier review. Ops without a verified parent-map stay `PROPAGATES=false` and declare the drop loudly. Includes the `classify()` classifier + 3 invariants as a NEW method (§6.5a), proptest-covered before any op routes through it. Depends on Phase 3.

### Phase 5 — Subdivide n-gon topology fix + correct quad-emitting Catmull-Clark

Audit `subdivide_catmull_clark`'s n-gon handling directly (the T31537 topological half). If it fans from vertex 0, FIX it in the op (face-point + per-edge split → n quads, interior vertex created), gated by the pentagon→5-quads interior-vertex test. This is the **bake/edit** topology — distinct from any display-only limit-surface eval. Crease reads from CustomData clamp the subdivision. Depends on Phase 4 (needs the crease layer + parent-map infra).

### Phase 6 — Demand-driven handles + element selection + falloff

Build vert/edge/face element-select as NEW app-layer interaction. Introduce `GenTags` on the wrapper ONLY now — with the first cross-op consumer (persistent element selection / cross-frame falloff cache). `Handle{id,gen}` is Handle-native from its first commit; raw element ids forbidden from escaping `core_math`. Add `FalloffProfile` to transform-class ops, sharing the existing brush kernel. `TopoIndex` (disk + radial, angle-sorted) lands here, patched incrementally for `*_mut` ops, gated on the proptest fuzz suite (orphan/interior-edge/boundary-loop). Depends on Phase 5.

### Phase 7 — Op-history DAG runtime projection + lazy eval

Build the DAG as a `#[serde(skip)]` runtime projection over the persisted `Vec<PolyModifier>` (linear stack → chain DAG). Lazy re-eval keyed on `(input topo_gen, param hash)` from the changed node forward; DRAFT resolution under interaction via the InteractionRenderPolicy latch, full quality on commit-cooldown; explicit `MAX_*` caps beside the existing bake/DC/mesh caps. Modifiers and edit ops share the SAME MeshOp code path. Branching topology (if needed) rides a `#[serde(default)] Vec<PolyDagEdgeRecord>` field — never a retyped modifiers field. Depends on Phase 6.

### Phase 8 — Boolean + remesh providers (boundary-wrapped)

Wrap **boolmesh** (MPL-2.0, file-isolated) as the mesh-boolean DAG node behind a `from_polygons` boundary (f64 internally, SDF-roundtrip fallback on degenerate input); wrap **baby_shark** (MIT) for decimate/remesh. Both convert to/from plain index+vertex buffers for manifold preservation, validated against the WeldMap quantization. Deferred: OpenSubdiv display eval, xatlas UV (FFI in `io_bridge`), USD export, f64 "P" CAD column — each its own gated follow-up. Depends on Phase 7.

---

## 8. TOP RISKS + MITIGATIONS

1. **FL4 corruption (the #1 trap, MIS-DIAGNOSED in the prior design).** (a) A second trailing magic would break FL4ORDER's `bincode::deserialize(tail)` (`project.rs:356`); (b) adding a field to the in-blob `HalfEdgeMesh` shifts bytes and corrupts every `.fl4`. *Mitigation:* authored layers as an IN-BLOB `#[serde(default)] Vec<PolyLayerRecord>` on `ProjectFile` (`voxel_grids` precedent); ZERO new `HalfEdgeMesh` fields (all runtime state on the wrapper); never serde-skip `PolygonalBody.mesh`; bincode sentinel-variant guard + both-`child_order`-and-layers round-trip test mandatory before merge.

2. **Per-op parent-map correctness — the real cost of "no attribute loss."** A wrong parent-map silently corrupts UV/crease — the exact failure being claimed avoided. *Mitigation:* "flow for free" demoted to "for ops that emit a parent-map"; op-by-op rollout (Gate 2a → 2b → 2c with each op's `layer_flow` test in its PR); ops without a verified parent-map set `PROPAGATES=false` and declare the drop loudly; `subdivide_catmull_clark`'s parent-map emitted from INSIDE the op (scoped freeze-lift); `mesh_eq(deep)` oracle gates every proptest.

3. **Delta-undo dense-mesh win is overstated for the two most common destructive ops** (subdivide, bevel-all both touch ~all elements → per-body Arc snapshot → O(body)). *Mitigation:* claim corrected to O(touched) for LOCAL ops / O(body) for global ops (still better than Blender's O(scene)); the in-place `*_mut` element-delta path is REQUIRED for split/extrude before claiming the dense single-body win; `mesh_eq(deep)` gates every op's `apply→inverse`.

4. **Bevel v1 is still a `mesh.clone()` stub (`bevel_op.rs:73-81`) and is the merge-blocker for the whole roadmap.** *Mitigation:* land bevel v1 FIRST with ZERO `half_edge.rs` edits (it is pure); retire `bevel_edges_ex` (`poly_drag_tools.rs:709`, app-layer) in the SAME task with the `apply→inverse==identity` proptest green; nothing in phases 2+ merges until this closes.

5. **N-gon subdivide may have a real interior-topology bug (T31537) masked by a display-only evaluator.** *Mitigation:* audit `subdivide_catmull_clark`'s n-gon handling directly; if it fans from vertex 0, fix it in the op (face-point + per-edge split → n quads), gated by pentagon→5-quads; the limit-surface evaluator is explicitly DISPLAY-ONLY and never the source of edit/bake topology.

6. **Freeze-lift scope is broader than cosmetic `validate()` additions and must be authorized precisely.** *Mitigation:* bevel needs NO lift; the CLAUDE.md ~line 352 amendment is a SEPARATE later gate scoped to (a) `classify()` as a NEW method leaving `validate()` intact, (b) internal Euler-op parent-map/inverse-delta edits, (c) subdivide n-gon topology — each gated on full `cargo test -p core_math` green + gold-tier review per op; `classify()` proptest-covered and "never weaker than `validate()`" before any op routes through it.

7. **Generation-handle infra must be DEMAND-DRIVEN, not speculative kernel machinery.** *Mitigation:* no gen vectors until the first cross-op consumer (persistent element selection / cross-frame falloff cache) lands, in the same task, carried on the wrapper (not `HalfEdgeMesh` → no freeze-lift); element selection Handle-native from day one; raw element ids forbidden from escaping `core_math`.

8. **Frame Budget under DAG eval + propagation.** *Mitigation:* cache eval keyed on `topo_gen` + param-hash; `apply_delta` bounded to the touched parent-map set (never a full-column sweep); `TopoIndex` patched incrementally for `*_mut` ops (full rebuild only on pure-rebuild ops); global ops route through commit-cooldown at draft resolution; explicit `MAX_*` caps documented beside the existing bake/DC/mesh caps.

9. **DAG-as-serialized-field-retype would corrupt saves.** *Mitigation:* `modifiers: Vec<PolyModifier>` stays the permanent wire format (append-only new variants); the DAG is a `#[serde(skip)]` runtime projection rebuilt from the Vec; branching topology, if needed, persists as a separate in-blob `#[serde(default)] Vec<PolyDagEdgeRecord>` — recorded as a hard gate.

10. **Non-manifold scope-creep toward a bmesh-rs fork.** *Mitigation:* ops return `Err(NonManifoldUnsupported)` by default; only explicit cleanup ops handle it; bmesh-rs recorded STUDY-ONLY; revisit fork only on a concrete blocking workflow.

11. **Deferred grafts (op-history DAG, f64 column, USD, OpenSubdiv) are multi-quarter.** *Mitigation:* shipping is gated (bevel v1 → Euler inverse-delta wrappers → CustomData Copy/Constant → per-op Interp → subdivide fix → demand-driven handles → DAG projection → boolean), each gate independently regression-tested, or the "beats Blender" claim outruns test coverage.

---

**Bottom line:** The winning kernel is **the proven typed-ID half-edge, evolved in place into a BMesh-Lite** — never rewritten, never forked, never SoA-migrated. All new state is RUNTIME state on a non-serialized `PolyMesh` wrapper; `HalfEdgeMesh` and `Vec<PolyModifier>` are never extended in-place; new authored data rides in-blob `#[serde(default)]` fields on `ProjectFile`. The test spine (`mesh_eq` deep oracle + per-op `layer_flow` + proptest fuzz + bincode sentinel + FL4 round-trip) gates every layer. Bevel v1 lands first and unblocks everything; attributes flow only through ops that emit a verified parent-map (and declare the drop loudly when they can't); undo is O(touched) for local edits and O(body) for global ones — strictly beating Blender's O(scene). The composability is in additive runtime layers, never in a kernel rewrite — the only shape that survives AZRA's append-only save format, frame budget, and No-Bandaids bar.
