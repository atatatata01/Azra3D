# Bevel — Master Plan (gated, phased)

**Goal:** best-in-class poly edge bevel (Blender BMesh / Plasticity quality). Replace the broken
single-segment "slide endpoint along neighbour edge" chamfer with a true **offset** operation +
corner patch + profile/segments.

## Root cause (verified against code)
The current bevel is the **frozen** `HalfEdgeMesh::bevel_edges_ex` (`half_edge.rs:1558`) — a per-edge
endpoint slide with **`_segments`/`_shape` unused**, **"NO CORNER FILLS"** (`:1754`), and
`from_polygons` (`:1452`) welds **by VertexId index, not position** so corner verts tear. All three
reported symptoms (can't round / multi-edge connection breaks / non-parallel rails) fall out of that
one wrong formulation.

## Architecture (the freeze-respecting route)
- **New pure module `core_engine/core_math/src/bevel_op.rs`** + `pub mod bevel_op;` in `lib.rs`.
  Reads the mesh through the **public API only** (`vertices/half_edges/faces` fields, `he_source`,
  `vertex_fan`, `face_vertices`, `face_half_edges`, `edge_faces`, `compute_face_normal`,
  `is_boundary_edge`) and rebuilds via public `from_polygons`. **`half_edge.rs` is never touched.**
  Entry: `pub fn bevel_edges_offset(mesh, selected, params) -> HalfEdgeMesh`.
- **Position-weld discipline (structural fix for the tear):** `bevel_op` owns `positions: Vec<Vec3>` +
  a `WeldMap` (HashMap keyed by quantized position, 1e-5 grid) + `push_welded(pos) -> usize`. Two faces
  meeting at one physical point ALWAYS get the same index; two distinct points never collide.
- **App glue in `app/src/poly/bevel.rs`** (the existing placeholder) — assembles `BevelParams`, caches
  the per-frame plan, forwards to `core_math::bevel_op`. **ONE reroute:** `poly_drag_tools.rs:709`
  (`bevel_edges_ex(...)` → `poly::bevel::build_bevel(...)`). Frozen `bevel_edges_ex` stays, orphaned.
- App crate keeps **zero geometry math**.

## v1 algorithm — offset chamfer (segments=1, shape ignored)
Per selected edge (skip boundary / NO_FACE):
1. `e = (dst-src).normalize()`. For each incident face F: `nF = compute_face_normal(F)`,
   in-plane perpendicular `inF = nF.cross(e).normalize()`, **sign fixed** by the face-centroid test
   (`if (centroidF - edge_mid).dot(inF) < 0 { inF = -inF }`).
2. Rail = original edge translated by `inF * width` → **parallel to the edge by construction** (the
   property the frozen algo lacked). Four welded rail points per edge (left/right × src/dst).
3. **Corner = line intersection:** at each `(vertex V, face F)` with two beveled edges, the corner
   point is the intersection of the two in-plane offset lines (2D solve in the face basis), welded
   once and SHARED by both faces → rails meet cleanly. Near-parallel (cross<1e-6) → averaged endpoints.
4. **Corner patch:** at a vertex with k≥2 beveled edges, sort the new ring points CCW about
   `compute_vertex_normal(V)` and emit ONE n-gon patch face (k=2 ⇒ a quad).
5. **Strip quad** per edge `[l_src,l_dst,r_dst,r_src]` (winding checked). Rebuild affected faces
   (every face in the beveled verts' fans) substituting resolved corner indices; emit the rest
   unchanged. `from_polygons(positions, faces)` → result (validates in debug).

## Phases + GATES
| Phase | Scope | Fixes | GATE (approval before next) |
|---|---|---|---|
| **v1 offset chamfer** | bevel_op.rs core + weld + corner + reroute; segments/shape ignored | parallel rails + real corner patch + manifold weld | **GATE 1:** `cargo test -p core_math` green; manual Ctrl+B on cube edge / edge-loop / 3-valence corner — rails parallel, watertight corner, undo restores. Owner signs off the corner LOOK. |
| **v2 segments + superellipse** | segments>1 strip subdivision; shape 0=concave .5=circular >.5=convex; valence-2 corners | wires the dead segments/shape inputs to geometry | **GATE 2:** v2 tests green (counts scale, watertight at seg=8); cube-edge round vs Blender side-by-side. valence≥3+seg>1 explicitly deferred behind CUTOFF. |
| **v3 profile/miter/clamp/normals/materials/modifier** | custom profile, miter inner/outer, clamp UI, harden normals, strip materials, **non-destructive `PolyModifierType::Bevel`** | removes the dead modifier arm; general valence≥3 solver | **GATE 3:** full matrix incl. valence≥3 (general OR documented CUTOFF); Plasticity parity; modifier save/load round-trips. |

## Test matrix (`cargo test -p core_math`)
manifold_after_single_edge_bevel · vertex_count_v1_one_edge (cube 8→10) · **parallel_rails** (cross<1e-5,
the v1 invariant) · watertight_corner_valence3 (one patch, no boundary edge) · **weld_no_index_collision**
· **weld_no_false_merge** · boundary_edge_skipped · winding_preserved · width_clamp (no NaN/degenerate) ·
v2_segments_scale · v2_superellipse_circular · v3_valence3_or_cutoff · idempotent_zero_width.

## Risks + fallbacks
- **KILLER: valence≥3 corner with segments>1** (the vmesh problem Blender spent years on). **Fallback
  shipped in v2:** a flat **CUTOFF** n-gon cap across the outer ring at multi-edge corners — straights
  still round, only the multi-edge corner is flat-capped. Correct + watertight + ships now; true vmesh
  is a v3 stretch.
- Sign ambiguity → centroid dot test (covered by winding test). Unordered fan → CCW sort about vertex
  normal. Weld-by-index → `push_welded` (the most dangerous failure mode; proven by weld tests).

## Performance — prepare-once / build-per-frame
Today `poly_update_bevel_drag` (`poly_drag_tools.rs:691`) clones the whole mesh + full rebuild **every
pointer-move**. Topology doesn't change during a width drag — only the scalar width. Split:
**PREPARE once** (on bevel activate): immutable `BevelPlan` (EdgeOffsets with unit dirs + base
positions, affected-face substitution map, corner wing lists, static face_lists). **BUILD per-frame:**
recompute only `base + dir*width` + cheap corner intersections, then `from_polygons`. O(edges+corners)
per frame, not a full traversal. Keep the `|Δwidth|<1e-4` early-out; segments bounded [1..32].

## Cleanups (don't block v1)
- **Orphaned GPU SDF preview "Apply" trap** (`properties_poly.rs:60-101`, preview at
  `poly_drag_tools.rs:808+`): a separate raymarched SDF approximation whose "Apply" commits unrelated
  half-edge mesh — a WYSIWYG trap. Preferred: remove the Apply button + SDF-preview block until it has a
  real purpose. (Owner decides.)
- **Dead `PolyModifierType::Bevel` arm** (`properties_poly.rs:268`): sliders render but
  `apply_poly_modifiers` (`mesh_sync.rs:64-73`) drops Bevel via `_ => {}`. Hide/grey "coming soon" until
  v3. A slider that lies is a bandaid.

## The ONE owner decision
**Home of the pure math:** `core_math/src/bevel_op.rs` (recommended — unit-testable via
`cargo test -p core_math`, reuses from_polygons/compute_face_normal, half_edge stays frozen) vs forcing
it entirely into the app crate. Everything else follows from this + the per-gate approvals.
