# Poly Mesh Booleans v1 — BSP Kernel + X Pie

Status: SHIPPED (v1). Kernel `core_math/src/mesh_boolean.rs`, app layer `app/src/poly/boolean.rs`,
UI `pie_menu.rs::boolean_pie_def` + dispatch in `input_keyboard.rs`, action `poly_boolean_pie`
(X, Viewport) in `input_system.rs`.

## 1. Kernel approach

`pub fn mesh_boolean(a, b, BoolOp{Union|Difference|Intersect}) -> Option<HalfEdgeMesh>` —
classic solid-BSP CSG (csg.js formulation):

1. **Fan-triangulate** every live face of both meshes into planar polygons (planes via
   Newell's method, `normalize_or_zero()` everywhere).
2. **Build two BSP trees** (splitter = first polygon's plane per node). Split classification
   uses a plane-thickness epsilon of `1e-5`; coplanar polygons use the standard BSP
   coplanar-front/back assignment (normal-agreement).
3. **Clip A against B and B against A** with the csg.js invert/clip sequences:
   union = A-outside + B-outside; difference = A-outside + B-inside-flipped;
   intersect = A-inside + B-inside.
4. **Rebuild**: merged soup → tolerance weld (`1e-4`, spatial hash with 27-cell neighborhood
   search — deliberately NOT `bevel_op::WeldMap`, whose exact-cell quantization misses
   near-coincident points straddling a cell border) → **T-junction repair** (any welded vertex
   lying on another ring edge's interior is inserted into that edge, sorted by parameter) →
   **coplanar dissolve** (see 4b) → `HalfEdgeMesh::from_polygons` (twin pairing by vertex index;
   `half_edge.rs` untouched).

**4b. Coplanar dissolve (Blender "limited dissolve" parity — 2026-07-03).** The BSP output is
triangle soup even on untouched faces (the "random edges" the user saw). `merge_coplanar()`
reproduces Blender's clean n-gon output: union-find faces into **coplanar connected regions**
(share an edge + normals agree within ~1.8° + verts within `1e-3` of the neighbour plane),
replace each region that is a **single simple boundary loop** with that loop (a hole/annulus →
two loops, or a pinch → kept as its triangles — never worse), then **dissolve straight-through
vertices** (valence-2, edge directions collinear within ~0.8° — iterated to a fixed point;
valence-≥3 junctions are kept so twins never crack). Euler χ is invariant (merge: −1 face −1 edge;
vertex dissolve: −1 vertex −1 edge), so a watertight input stays watertight. The rebuild
**compacts** to referenced-only vertices (dissolve orphans some welded points). GUARDED: if the
merged mesh isn't watertight+`validate()`-clean it falls back to the triangulated rebuild.
Verified vertex/face-for-face against Blender's Exact solver: cube−cube corner (14 v / 9 f =
6 quads + 3 hex), 45°-rotated cutter (10 v / 7 f = 5 quads + 2 pent). Curved cutters (cylinder)
keep every wall facet distinct (different planes ⇒ never merged); annulus caps stay triangulated
(Blender bridges them — a representation choice, both watertight).

`None` is returned for: open inputs (any live boundary half-edge — checked via the shared
`boundary_half_edge_count()`), degenerate inputs, or empty results (e.g. intersect of
disjoint solids).

Test gates (`mesh_boolean.rs #[cfg(test)]`, all green): overlapping offset cubes for all 3 ops
(watertight, `validate()` ok, plausible vertex counts, bbox checks, Euler characteristic
V−E+F = 2), difference of non-touching cubes (A's bbox back, watertight), cube minus a
through-prism (watertight genus 1: V−E+F = 0), disjoint intersect → None, open mesh → None.

## 2. UI — X pie (hold-to-open)

* Action `poly_boolean_pie`, category "Poly", default **X**, `InputContext::Viewport`,
  registered in `input_system.rs`. X+NONE+Viewport is *shared* with `sculpt_brush_size_up`
  (and the legacy `ACT_CYCLE_OP` X in the SDF context, and the transform-modal axis
  constraint) — all disambiguated by state, same documented pattern as
  `ACTION_BEVEL_TOGGLE_CLAMP`: the pie only opens in the **Polygonal** modeling context,
  outside Sculpt work mode, with an idle tool and idle poly tool. The binding is pushed
  directly (not `bind_default`, which dedups by event+mods+context and would drop it) so
  `binding_for()` resolves and user remaps are honored.
* Reuses the existing `PieMenuState` system (`ui.bool_pie`): hold X → hover → release.
  Items: **Union / Difference / Intersect / Keep Cutter toggle** (persistent flag
  `ui.bool_pie_keep_cutter`, default OFF, shown as ON/OFF in the item label — a pie item
  instead of a checkbox because the pie executes on key-release). Center hub =
  **"LIVE…"** → toast "Live boolean — coming soon" (reserved for the live-boolean prompt).
  Escape / right-click cancels (Tool Exit Convention).

## 3. Selection semantics (cutter → targets)

* **Object mode** — selected PolygonalBody nodes in selection order (`UnifiedSelection` is
  an insertion-ordered IndexSet): FIRST = cutter, rest = targets. With only one selected,
  targets = every other PolygonalBody whose world-space AABB intersects the cutter's.
* **Edit mode (sub-elements)** — the selection's vertices/edges/faces seed a flood fill
  (`bfs_linked_vertices`, reused from `poly_selection.rs`); the containing island(s) are
  extracted (`from_polygons` subset copy) as the cutter. Targets = the remainder of the same
  mesh (when its AABB overlaps) + every other intersecting PolygonalBody.
  *Deviation from the original spec:* `PolySelection` stores sub-elements in `HashSet`s, so
  "the island of the FIRST-selected element" is unknowable — ALL islands touched by the
  selection form the cutter (multi-component cutters are fine for the BSP).
* **Execution** — both meshes transformed to world space (`scene_graph.world_transform`),
  kernel runs, result transformed back into the target's local frame and installed.
  **Difference is target-centric: target − cutter** (the spec text's literal
  `mesh_boolean(cutter, target, Difference)` would compute cutter − target, which
  contradicts the cutter workflow; union/intersect are symmetric).
* **Undo** — booleans are computed BEFORE the transaction (pure reads; an all-failed run
  creates no undo entry); then ONE `transaction()` installs every target result, deletes the
  cutter object / detaches the cutter island (unless Keep Cutter), clears stale sub-element
  selection, and re-syncs mesh caches (`poly_sync_mesh_cache`).
* **Install guard** (mirrors `poly_bevel_regenerate`): a target result is installed only if
  the kernel returned Some AND `validate().is_ok()` AND `boundary_half_edge_count == 0`.
  Failed targets are left byte-identical, with a summary toast. The cutter is only
  deleted/detached when at least one target succeeded.

## 4. Known limitations (v1)

* **BSP robustness**: f32 + epsilon, not exact arithmetic. Near-coplanar/near-tangent
  face pairs can produce slivers or non-manifold output — such results are *rejected by the
  install guard* rather than repaired. Large-coplanar-overlap cases (two cubes sharing a
  face) are the classic weak spot.
* **Open meshes unsupported** (kernel returns None). Non-manifold inputs likewise fail the
  input boundary census.
* Output flats are **dissolved to clean n-gons** (§4b); the remaining rough spot is
  **annulus caps** (a face with a hole from a through-cut) — kept triangulated rather than
  bridged into a single n-gon like Blender. Watertight either way.
* No material transfer from the cutter; the target keeps its own material. Face
  `material_id`s are reset (from_polygons rebuild).
* Booleans run on the **base mesh**; the modifier stack is re-applied on top by
  `poly_sync_mesh_cache` (consistent with all other poly ops).
* BSP recursion is unbounded (stack depth ~ tree depth); very large or pathological meshes
  could get deep. Fine at DCC-typical counts; revisit with an explicit stack if needed.
* Sub-element path can't honor "first-selected island = cutter" (HashSet selection storage,
  see §3). If ordered sub-element selection lands in `PolySelection` later, tighten this.
* Live/preview boolean (center pie button) is reserved — toast only.

## 5. Duplication flags

* `poly_drag_tools.rs::poly_bevel_regenerate` has an inline boundary-count closure that
  predates `mesh_boolean::boundary_half_edge_count` — migrate it when that file is next
  touched (not done here: bevel files are guarded by the BEVEL.md protocol).
