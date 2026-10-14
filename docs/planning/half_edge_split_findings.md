# half_edge split — bug-sweep findings (chore/split-half-edge)

Observations made while relocating code for Phase 1 of GOD_FILE_SPLIT_PLAN.md
§3.1 (H1–H11), on the true master base f9cbcd8. **Nothing here was touched in
this branch** — pure-relocation contract. Listed for future triage.

Baseline: `cargo test -p core_math --lib` = **793 passed / 8 ignored**;
`cargo test -p core_math --test serde_golden` = 3 passed. Both counts held
through every H-step.

## Findings (observed, NOT fixed here)

1. **`he_source` last-resort branch returns a known-wrong value**
   (`half_edge/access.rs`): for a LIVE orphan with no valid prev/twin it logs
   and returns `self.he(h).vertex` — the *destination* — with a comment
   admitting "still wrong but logged". Callers cannot distinguish this from a
   valid answer.

2. **`is_edge_live` / `is_face_live` are O(free-list) per call**
   (`half_edge/alloc.rs`): `free_edges.contains(&he.0)` linear scan, called per
   half-edge in overlay/selection loops → O(n·f). Several hot consumers
   (`validate`, `recompute_normals`, `bounding_box`) already hoist a HashSet
   with comments pointing at exactly this cost, but the per-call predicates and
   many op-internal `free_*.contains(...)` checks still scan linearly.

3. **Accessors do unchecked indexing on stale ids** (`half_edge/access.rs`):
   `vert()/he()/face()` index directly; a stale id after delete either panics
   OOB or silently reads a freed-and-reused slot — no liveness gate. There is
   no `is_vert_live` predicate at all (edges and faces have one, vertices
   don't), so consumers cannot even check vertices.

4. **`free_face` clears only `half_edge = NO_EDGE`** while `free_edge` resets
   the whole slot to one canonical dead state and `free_vert` exists as a
   chokepoint; the three chokepoints enforce different amounts of "dead state",
   so a freed face still carries a stale `normal`/`material_id` readable
   through `face()`.

5. **`he_source` dereferences `prev` without a liveness check**
   (`half_edge/access.rs`): guards `prev == NO_EDGE || prev.0 >= len` but a
   freed-but-in-range `prev` passes and is dereferenced — safe today only via
   the canonical-dead-state invariant, not via `is_edge_live(prev)`.

6. **Duplicate boundary predicates**: `is_boundary_vertex` (nav.rs, fan-based)
   and `vertex_is_boundary` (nav.rs, fan + whole-mesh scan fallback) answer the
   same question with different algorithms and can disagree on meshes with
   stale `vertex.half_edge`. Two code paths for one operation (CLAUDE.md
   No-Bandaids). Also duplicated doc lines sit on `loop_ring_walk`
   (`ops_loop_cut.rs` — its doc block is pasted twice back-to-back).

7. **`newell_normal` duplication acknowledged in-code** (geom.rs TODO(dedup)):
   `bevel_op::newell_idx` and app `poly/fill.rs::best_fit_plane_normal` are two
   more copies of the same math (flagged by the original author; outstanding).

8. **`vertex_valence` / boundary checks allocate** (`half_edge/nav.rs`):
   `vertex_fan` returns a fresh `Vec` per query; valence/boundary predicates
   allocate on every call in hot selection paths.

9. **`smooth_laplacian` has no boundary pinning** (`ops_misc.rs`): its own doc
   says it matches Blender's Smooth modifier (which doesn't pin), but on open
   meshes it shrinks the boundary — worth an explicit `lock_boundary` option
   like `loop_relax` already has (that fn takes `lock_boundary: bool`, the
   whole-mesh op doesn't).

10. **`bevel_edges` ignores its documented knobs** (`ops_misc.rs`): the doc
    comment promises `segments` / `shape` control, but `bevel_edges_ex` takes
    them as `_segments` / `_shape` and never reads them — callers asking for
    multi-segment bevels silently get a 1-segment chamfer. (The real bevel
    lives in `bevel_op.rs`; these kernel fns look legacy.)

11. **`from_triangle_soup` silently drops `_normals`** (`builders.rs`): the
    signature promises normal-aware construction; it just forwards to
    `from_triangles` and recomputes.

12. **`debug_cube_topology`** (`ops_misc.rs`): a `pub` stdout debug printer
    shipping in the kernel; dead-ish code kept per relocation contract.

13. **`validate_mesh(context)` only eprintlns** (`validate.rs`): it logs
    failures and returns `()`, so callers cannot gate on it; `validate()` and
    `is_manifold()` must be called separately to actually gate (matches the
    older poly health-report finding).

14. **`vertex_is_boundary` reads `self.he(h).twin` twice per fan step** with a
    comment "Fan may miss pure boundary when only reverse is boundary-marked —
    scan free-safe" — the fallback whole-mesh scan makes the predicate O(E) per
    vertex in the worst case; used inside loops it is O(V·E).

## Split-mechanics notes (for reviewers)

- The bincode-serialized `HVertex` / `HalfEdge` / `HFace` / `HalfEdgeMesh`
  struct definitions moved to `half_edge/types.rs` VERBATIM (field order,
  derives, serde attrs). Only visibility changed on the three free-list fields
  (private → `pub(super)`, serde-neutral, required for the module split).
  `serde_golden` (3 tests) stayed green after every step.
- `best_fit_circle_2d`, `alloc_face`, `free_face`, `free_edge`,
  `find_half_edge` went file-private → `pub(super)` (module-tree-private) so
  cross-submodule callers still compile. No name is newly visible outside
  `core_math::half_edge`.
- Grep gate (H3): zero raw `free_verts/free_edges/free_faces` pushes outside
  `half_edge/alloc.rs`.
- `bounding_box` was colocated into `access.rs` and the count/signature fns
  into `validate.rs` (the plan left them unassigned; contiguity won).
