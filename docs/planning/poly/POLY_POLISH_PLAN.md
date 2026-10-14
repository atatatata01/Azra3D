# POLY_POLISH_PLAN.md — Blender-Parity Polish of the Poly Toolset (READ FIRST, EVERY POLY-POLISH TASK)

> **This is the accumulator for polishing AZRA's poly modeling to Blender parity, tool by tool.**
> Same discipline as `BEVEL.md`: Blender is the oracle, every tool is diffed vertex-for-vertex against a
> headless-Blender reference, and every session appends what it learned (§7 log + §8 graveyard) so we never
> re-derive a dead-end. If you are about to polish a poly tool, read §0 → §2 before touching a line, then
> pick the next unchecked item in §4/§5 and run the §3 loop on it.

**Scope:** the **polygon** edit-mode toolset — the half-edge kernel (`core_math/src/half_edge.rs`), the
mesh ops (`core_math/src/mesh_ops/*`, `bevel_op.rs`, `mesh_boolean.rs`), and the app tool layer
(`app/src/poly/*`, `poly_drag_tools.rs`). The **bevel** has its own deeper knowledge base — `BEVEL.md` is
authoritative for anything bevel; this plan covers everything else and defers bevel detail to it.

**Source of truth for WHAT is broken / missing:** the 2026-07-07 poly audit (artifact + task output). This
plan turns those findings into an oracle-gated execution order.

---

## §0. READ-FIRST PROTOCOL (how not to ship a watertight-but-wrong tool)

1. **Blender is the oracle. Never guess geometry.** For every tool, generate the Blender ground truth (§2)
   and diff vertex-for-vertex. **Topology gates (watertight / manifold) are NECESSARY BUT NOT SUFFICIENT** —
   the whole bevel saga (BEVEL.md §6) proved a mesh can be watertight + manifold and still geometrically
   wrong (spikes, folds, flipped normals, non-parallel edges). Only a Blender diff catches those.
2. **Match Blender's DEFAULTS, not just its capability.** Reproduce the tool at the settings the app ships
   (e.g. extrude region by default, merge At Center, inset even-offset), not the easiest variant.
3. **Gate at the app default, not the trivial case.** (BEVEL.md §0.5: the seg=1 bevel test stayed green for
   months while the app tore at the seg=3 default.) One-quad extrude passing ≠ region extrude working.
4. **No bandaids (CLAUDE.md).** A watertight-but-wrong result is still wrong; the missing gate is usually
   *geometric*, add it. Never downgrade an assert or special-case a caller to make a test pass.
5. **The integrity guard must be able to SEE the corruption before any fix is trustworthy** — this is why
   §4 Phase 1 (the manifold classifier) comes first. Until `validate()`/`is_manifold()` can reject a
   non-manifold/degenerate result, a passing test proves nothing.
6. **One tool per prompt / per agent (CLAUDE.md Feature Discipline).** Each tool = its own oracle, its own
   golden test, its own commit. Do not batch two tools into one change.
7. **When you finish, WRITE IT DOWN:** append the dead-ends to §8 and a one-line §7 session entry, update
   `COMPLETE_BUG_TRACKER.md`, and the `project_poly_modeling_health_report` memory. This file only pays off
   if it accumulates.

---

## §1. Kernel invariants the polish depends on (from BEVEL.md §2, half_edge.rs)

- **`he.vertex` = DESTINATION**, not source. Source is `mesh.he_source(he)`.
- **`he.prev` may be `NO_EDGE`** — check before walking.
- **`vertex_fan(v)`** = incident half-edges in cyclic order; `.len()` = degree.
- **HashMap/HashSet iteration is random** — sort keys wherever output IDs / merge order must be stable.
- **`compute_face_normal` = Newell** (area-weighted, sign-correct on non-convex faces).
- **`from_polygons` twins by REVERSE direction + vertex INDEX** — shared edges must be wound opposite by
  their two faces or they become boundary edges (tears). Weld coincident points to ONE index (WeldMap).
- **RED LINE (never lifted):** do NOT change `HalfEdgeMesh`'s bincode-serialized shape (`HVertex`/`HFace`/
  `HalfEdge`/`HalfEdgeMesh` field layout). New authored state rides the `PolyMesh` wrapper or in-blob
  `serde(default)` FL4 tail. (feedback_half_edge_hard_frozen; project_bincode_save_format_field_trap.)

---

## §2. The Blender-oracle method (generalized from BEVEL.md §7)

Blender's mesh ops are compiled C — drive Blender **headless** to emit a reference mesh and diff it.

- **Binaries (verified present):** `C:\Program Files\Blender Foundation\Blender 4.5\blender.exe` (and 5.0).
- **Command:** `blender.exe --background --python <tool>_ref.py -- <out.json>`. `bl_info` tracebacks in
  background mode are harmless noise.
- **Coordinate mapping (CRITICAL — diff only works if inputs match):** build the Blender primitive to match
  AZRA's exactly. Blender `mesh.primitive_cube_add(size=2)` → **±1**, matching AZRA `new_box(Vec3::ONE)`.
  For a custom input, author the SAME vertex list in both the `.py` and the Rust test.
- **The script (`docs/planning/poly/<tool>_ref.py`, re-createable):** build the mesh → enter edit mode →
  select the target elements **by geometry** (position/index, so AZRA can select the same ones) → run the
  op (`bmesh.ops.*` or `bpy.ops.mesh.*` at the documented default settings) → dump
  `{"verts":[[x,y,z]…], "faces":[[i,j,k…]…], "selection":{…}}` to the JSON out-path. Keep a
  `poly_oracle.py` helper with the shared build/select/dump plumbing (Phase 0).
- **Blender settings to pin per tool** (record them in the script + the test comment): the op variant
  (e.g. Extrude Region vs Individual), and any modal defaults (Merge mode, Inset even/depth, Solidify
  offset −1 = inward).

**The AZRA golden test (`core_math/src/<op>.rs` `#[cfg(test)]`, the pattern the bevel tests already use):**
1. Build the **identical** input mesh (hardcoded verts/faces).
2. Assert the input is closed (boundary edge count == 0) when it should be.
3. Run the AZRA op at the same settings.
4. Assert **watertight** (`boundary_half_edge_count == 0` when closed) **AND manifold**
   (`out.is_manifold().is_ok()`, Phase 1) — necessary, not sufficient.
5. **Vertex-for-vertex diff:** assert `out.vertices.len() == oracle nv`, `out.faces.len() == oracle nf`,
   and for each oracle vertex position, assert some output vertex is within `eps` (1e-3 typical; loosen to
   the documented fullness-class tolerance only where BEVEL.md already established it).
6. Add the **geometric gates** relevant to the tool (in-plane, outward normals, no bbox spike, adjacency/
   twin sharing for extrude, etc.). Transcribe the oracle numbers into the test (self-contained, no runtime
   JSON I/O — matches the existing bevel gates).

**Regression protocol (run before declaring any item done):**
`cargo test -p core_math` (full suite green) → `cargo build -p app` dev **and** `--release` (kill a locked
`app.exe` first) → generate/refresh the Blender ref and diff → append §7/§8 → update the bug tracker.

---

## §3. The per-tool loop (do this for each item in §4/§5)

1. **Oracle.** Write/refresh `<tool>_ref.py`, run headless Blender, capture `<tool>_ref.json`. Record the
   exact Blender settings.
2. **Reproduce.** Implement/repair the AZRA op so its output matches — real ID-preserving half-edge ops
   preferred over `from_polygons` rebuilds (feedback_half_edge_hard_frozen), except where the tool is
   inherently a rebuild (bevel).
3. **Gate.** Add the golden test (§2). Watertight + manifold + vertex-for-vertex + geometric gates, at the
   app-default settings.
4. **Wire.** App-layer entry (keybind/menu via `InputSystem`, one producer, undo through the transaction/
   PolyEdit path). Confirm the app default matches the oracle settings.
5. **Verify.** Full regression protocol (§2). Manual scenario described.
6. **Record.** §7 session line + §8 graveyard for any dead-end + bug tracker + memory pointer.

---

## §4. Execution order — CORRECTNESS FIRST (the owner's reported bugs)

Ranked so each step stands on the one before it. **Phase 1 is the keystone and unblocks all the rest.**

- [x] **PHASE 1 — Integrity classifier (audit bug #1, the root cause). CLASSIFIER + BOOLEAN GATE DONE
  2026-07-07; local_ops gate DEFERRED to 2a/2b.** Added `HalfEdgeMesh::is_manifold() -> Result<(),String>`
  (half_edge.rs, after `validate()`) detecting: (a) an undirected edge with **>2 incident faces** OR a
  directed edge used by 2 faces same-winding (the coplanar-boolean pair), (b) a live face with <3 verts / a
  repeated vertex / a self-loop half-edge, (c) a live `vertex.half_edge` pointing at a FREED/OOB edge or an
  edge not sourced there (the `delete_face` bug). 5 unit tests (clean cube + open box pass; non-manifold
  edge / self-loop / stale pointer rejected). Wired **`poly/boolean.rs` `result_installable`** on it (no
  boolean test regressed). **`mesh_ops/local_ops.rs` run/run_snapshot gate DEFERRED** — enabling it now
  fails **5 extrude tests** because `is_manifold()` (correctly) rejects `extrude_face`'s non-manifold output
  = **empirical confirmation of audit bug #3**. Per "implement the fix before the gate that requires it,"
  the local_ops gate lands with Phase 2a (extrude) / 2b (delete). 402 core_math green, app builds.

- [~] **PHASE 2a — Extrude. TWIN-RELINK + GATE DONE 2026-07-07; region (multi-face) still TODO.**
  Rewrote `extrude_face` (half_edge.rs) to re-link ALL twins: wall-top↔cap edge, wall-bottom↔the saved
  base neighbour (else boundary), and adjacent walls' verticals; and to point every touched
  `vertex.half_edge` at a genuine OUTGOING edge (the old code stored an incoming one — the exact
  `is_manifold()` mislink). **ENABLED the deferred `local_ops` run/run_snapshot is_manifold gate** — the
  5 extrude tests now pass *because* extrude is manifold-correct. Oracle `extrude_ref.py`
  (Blender Extrude Region+Move, bottom face +Y 0.5 → nv=12/nf=10) matches vertex-for-vertex
  (`extrude_face_matches_blender_oracle`). Two dead-ends hit + fixed: dropped `he0.prev` cyclic link
  (OOB panic); and the composition `extrude→inset` surfaced `delete_face`'s dangling `vertex.half_edge`
  (audit #6) which I fixed here (see 2b). **REMAINING:** `extrude_region(faces)` for MULTI-face selections
  (weld shared interior edges = Blender default E); single-face extrude is already correct. 403 core_math
  green, app builds.

- [~] **PHASE 2b — Delete / Dissolve (audit #4,#5,#6,#10,#11).** DONE 2026-07-07: `delete_face` #6 fix;
  **`delete_vertex` + `delete_edge`** kernel ops (both delegate to the fixed `delete_face` for incident
  faces, then free the loose vertex / drop loose endpoints) — Blender-vertex-exact vs `delete_ref.py`
  (Delete Vertices corner (1,1,1) → nv=7/nf=3; Delete Edges → nv=8/nf=4), manifold. Added
  `PolyEdit::DeleteVertex/DeleteEdge` + dispatch, `poly_delete_vertices/edges` handlers, and **routed the
  Delete key by work mode** in `key_dispatch.rs` — the Vertex/Edge Delete dead keys (audit #4/#5) now work.
  **`dissolve_face` region-merge (#10) DONE 2026-07-07** (`dissolve_faces_region`, Blender-exact hexagon).
  **`dissolve_edge` same-face-slit guard (#11) DONE 2026-07-15** — refuses, Blender-confirmed (see §7).
  **REMAINING:** `DissolveVertex` (merge fan → one face).
  ~~NOTE: `apply_in_place` still gates on `validate()` only (not `is_manifold()`) — tighten once
  dissolve_edge (#11) + merge (#8) are fixed.~~ **STALE — CORRECTED 2026-07-15.** This note was already
  false when written: `938a78c` (2026-07-07, the same day) added `m.is_manifold()...?` to the snapshot
  tier at `mesh_ops/mod.rs:453`, as §6 of this very doc records at its "(#1) gated `apply_in_place`'s
  snapshot tier on `is_manifold()`" line. `DissolveEdge` is snapshot tier, so it has been gated on
  `is_manifold()` all along; the checklist line was simply never ticked. Only the **ElementDelta** tier
  (`mod.rs:335`, `validate()` only — SplitEdge/ExtrudeFace/FlipFace) remains un-tightened, and neither
  #11 nor #8 blocks it. Anything inheriting this line (WORK_ORDER.md:132-135 and its HARD dep #9 at
  :1026-1030) inherits the error.

- [x] **PHASE 2c — Boolean robustness (audit #2,#7,#12) DONE 2026-07-07.** **#2 outcome:** the feared
  coplanar mesh-break does NOT reproduce — `merge_coplanar` already dissolves coplanar/coincident faces on
  every realistic config (probed union/diff/intersect × coplanar-top / through-cut / flush / slab /
  IDENTICAL / thin-slab; all closed manifolds, only empty A−B → clean None). The concrete fix landed:
  `polygons_to_mesh` now guards BOTH the merged AND the triangulated-fallback path on `is_manifold()` and
  returns `None` if neither is a clean closed manifold (was `Some(build(&faces))` returned UNVALIDATED at
  :760 — the audit's latent bug). So the kernel can no longer EMIT non-manifold geometry from this path.
  Gate `coplanar_and_coincident_booleans_stay_manifold` (6 configs + Blender oracle bbox). **(a) local-frame
  BSP (#7) DONE 2026-07-07** —
  `mesh_boolean` recenters on the combined AABB, runs the BSP in small-coord local space, shifts back;
  translation-invariant (Blender-consistent), gated. ~~**REMAINING:** (b) coplanar/coincident face-pair drop
  in `polygons_to_mesh` + stop returning `build(&faces)` unvalidated (:733) — audit #2.~~ **(b) RESOLVED
  2026-07-23 — and the owner's beveled-subtract sliver had a DIFFERENT root cause** (POLY-BOOL-BEVEL,
  COMPLETE_BUG_TRACKER.md): `split_targeted`'s plane groups kept ONE representative cutter for the
  finite-support test, so ear-clipped multi-triangle walls vetoed required splits; the per-edge veto then
  chopped polygons without seam verts → `Polygon::new → None` → whole walls silently dropped. Fixed:
  full plane-group support union + per-polygon chord decision (split fully or emit whole) + VOLUME
  INVARIANT GATE in `mesh_boolean` (A−B ∈ [vol(A)−vol(B), vol(A)]) so wrong-but-manifold slivers fail
  honestly. Gates: `difference_beveled_prism_flush_bottom_matches_oracle` (bool_bevel_ref.py oracle) +
  `difference_beveled_stress_matrix` (16 configs). The unvalidated `build(&faces)` return was already
  fixed by Phase 2c's `manifold_ok` gates. **(c) BSP recursion
  depth cap (#12) DONE 2026-07-07** — `MAX_BSP_DEPTH`/`MAX_BSP_POLYS` backstops, no more stack overflow on
  degenerate input. Original text:
  (a) run the BSP in a **local frame** centered on
  the combined AABB (stop far-from-origin cracking); (b) drop cancelling coincident/coplanar face pairs in
  `polygons_to_mesh`, stop returning `build(&faces)` unvalidated (:733); (c) recursion **depth cap /
  work-stack** so dense/self-intersecting input fails cleanly. Oracle: extend `bool_ref.py` / `bool_hard.py`
  with a translated-far-from-origin case + a coplanar-overlap case. Gate: manifold (Phase 1) + vertex diff.

- [ ] **PHASE 2d — Merge (audit #8 self-loop, #13 nondeterministic center) + Merge-by-Distance.** Compute
  the target once (centroid / first / last), weld all to one survivor, collapse resulting self-loops +
  degenerate faces. Add spatial-hash **Merge by Distance**. Oracle: `merge_ref.py` (At Center / By Distance).

- [~] **PHASE 2e — Inset. EVEN OFFSET DONE 2026-07-15 (audit #15); region-inset + Individual toggle TODO.**
  `inset_face_depth` offsets each edge along its in-plane inward normal and intersects adjacent offsets
  (`v' = v + t·(n1+n2)/(1+dot(n1,n2))`) — Blender "even", vertex-for-vertex vs `inset_ref.py`/`.json`
  (box(2,1,1) +Y 4x2 face, thickness 0.3 → inner ring ±1.7/±0.7). Gates
  `inset_face_matches_blender_oracle_even_offset` + `inset_face_even_offset_uniform_gap_on_irregular_ngon`.
  **REMAINING:** `inset_region(faces)` (Blender's `I` DEFAULT — interior shared edges of a multi-face
  selection are NOT inset; the app currently insets each face Individually) + an Individual toggle + expose
  `depth`. See §7 (2026-07-15).

- [ ] **PHASE 2f — Bridge Edge Loops (audit #14).** Real `bridge_loops(loop_a, loop_b)` face-strip; keep the
  1/2-vert fabrication as a separate "make quad". Oracle: `bridge_ref.py` (two N-edge loops).

---

## §5. Execution order — THICKEN + FEATURE GAPS (after §4 correctness)

**Thicken family (audit report — scaffolding already exists):**
- [x] **Solidify DONE 2026-07-07** — `HalfEdgeMesh::solidify(thickness, offset)`: inner shell along the
  vertex normal (offset ∈ [-1,1] biases the two shells), reversed-winding inner face copies, rim quads per
  open boundary (all via `create_face` auto-twin). Wired the `poly/mesh_sync.rs` stub (was `_ => {}`) — the
  Solidify modifier UI/data are now live. Oracle `solidify_ref.py`: closed cube → hollow shell nv=16/nf=12
  (no rim), open quad → slab nv=8/nf=6 (+4 rim), both closed + manifold. Gated on those topology
  invariants (`solidify_matches_blender_oracle_invariants`) since Blender's exact vertex-normal averaging
  differs from ours. Delivers the owner's "extrude → thicken" request. 410 core_math green, app builds.
- [ ] **Shrink/Fatten (Alt+S)** — per-vertex-normal transform, no topology change (dodges the twin bug). (S)
- [ ] **Extrude Along Normals (Alt+E)** — region-extrude with cap verts on the smoothed vertex normal. (S,
  after 2a)

**Missing tools, prioritized (cheap wins first):**
- [x] **Circle (LoopTools) DONE 2026-07-15** — `HalfEdgeMesh::circle_region(faces, radius, flatten,
  regular, relax_passes)`: reshape a face region's boundary loop onto a circle. Pure reshape (all ids +
  counts + watertightness preserved). Blender-oracle gated vertex-for-vertex at LoopTools' real defaults
  (`circle_ref.py`/`.json`; the addon DOES drive headless). App: `poly/circle.rs`, Alt+C via InputSystem
  (`ACTION_POLY_CIRCLE_REGION`), Q/poly menus, `cmd_poly_circle`, F9-style "Adjust Circle" redo panel;
  selection stays on the region so `Circle → I → E` chains into a round boss. See §7 (2026-07-15).
  **REMAINING:** the interactive radius HANDLE (the op + redo panel ship a numeric radius; the kernel
  already returns per-vertex radial dirs so a drag needs no re-fit).
- [ ] **LoopTools rest-of-family** — `Relax`, `Flatten`, `Space`, `Curve`, `GStretch` are all now
  oracle-able the same way (`addon_utils.enable("bl_ext.blender_org.looptools")` works headless). Circle's
  `region_boundary_loops()` + `newell_normal()` + `best_fit_circle_2d()` are the shared scaffolding.
- [ ] **Symmetrize** — kernel `symmetrize()/delete_half()/flip_across()` EXIST + tested, just unwired. Add
  the app op + UI (free win, S). Oracle: `symmetrize_ref.py`.
- [ ] **Separate (P)** — split selected faces/islands into a new `PolygonalBody`. (M)
- [ ] **Bisect** — plane cut + fill/clear sides; reuse the knife plane-crossing infra. (M)
- [ ] **Shade Smooth/Flat per-face + auto-smooth angle** (partial today). (S)
- [ ] **Limited Dissolve** — angle-threshold planar merge (boolean has an internal one to expose). (M)
- [ ] **Edge Slide / Vertex Slide** (standalone, not just loop-cut-time). (M each)
- [ ] **Rip / Rip Fill (V / Alt+V)**. (M)
- [ ] **Select Similar (Shift+G)** — by normal/area/material/valence. (M)
- [ ] **Snap-to-geometry (vertex/edge/face)** for edit-mode transforms. (M)
- [ ] **Grid Fill** (L), **Spin** (M), **Screw** (M).
- [ ] **Merge/Collapse edges & faces to a point** (M).

Bevel remaining gaps (rounded N-way / edge-loop k=2 M_ADJ grid) are tracked in **BEVEL.md §9**, not here.

---

## §6. Governance / where this runs

- **Branch:** foundational correctness (Phase 1) lands on **master** (the trunk every pillar syncs FROM);
  the poly-foundation window rebases onto it. Feature items may ride `feat/poly-foundation` and integrate.
- **half_edge.rs is editable** for topology ops (freeze lifted 2026-07-06) — prefer ID-preserving ops over
  `from_polygons` rebuilds. The ONLY red line is the serialized shape (§1).
- **Every op through the transaction / PolyEdit path** (undo). Every new keybind through `InputSystem`.
- **Definition of done per item:** build green (dev+release) · Blender vertex diff passes · full core_math
  suite green · undo/save-load considered · one manual scenario · §7/§8 + bug tracker updated.

---

## §7. Session log (append newest at top)

- **2026-07-16 — MULTI-EDGE `dissolve_edges` REGION collapse DONE (Blender `bmo_dissolve_edges` parity,
  oracle-gated BY IDENTITY). Fixes the owner's "Ctrl+X on >1 edge does nothing" + the single-edge 8/11/5
  divergence in one shot.** ROOT: `app/src/poly/dissolve.rs` Edge branch emitted ONE
  `PolyEdit::DissolveEdge` PER selected edge in a SEQUENTIAL batch; the first merge made a later edge's
  faces share a vertex → slit (`face_a==face_b`) → correctly REFUSED at `half_edge.rs:2173` → atomic batch
  rolled back → nothing changed. A sequential batch STRUCTURALLY cannot reproduce Blender's region
  collapse. FIX = 2 new kernel fns + thin plumbing, NO parallel op path:
  (1) **`join_edge_kill_vert(v)`** (`half_edge.rs`, port of `bmesh_kernel_join_edge_kill_vert`) — the
  keystone AZRA lacked: collapse an interior VALENCE-2 vertex, removing it + one edge and merging the two
  collinear edges into one, KEEPING both adjacent faces (face count unchanged — NOT `dissolve_vertex`,
  which merges the fan). ID-preserving relink (`he_av`→a-b, `he_bv`→b-a survive; `he_vb`/`he_va` freed via
  the `free_edge` chokepoint; `v` marked loose). Refuses (no-op, byte-untouched) on non-interior /
  non-valence-2 verts.
  (2) **`dissolve_edges_region(edges)`** orchestrator — union the faces bounded by SELECTED edges only
  (reuses `dissolve_faces_region`'s BFS = the over-merge guard: an UNSELECTED shared edge never links two
  faces) → one n-gon per component → **duplicate-face cull** (`kill_duplicate_faces`: a merged face whose
  whole boundary twins to ONE existing face is the closed-cup collapse — drop it so the 4-top cap
  collapses onto the -Y quad instead of stranding a doubled face) → **valence sweep** of the selected-edge
  endpoints (`join_edge_kill_vert` to fixpoint) = the `use_verts` cleanup.
  Plumbing: `PolyEdit::DissolveEdgesRegion { edges }` appended at the END of the enum (bincode index rule)
  + `compute`/`targets` arms (mirror `DissolveFaces`); dissolve.rs Edge branch now emits ONE
  all-or-nothing region op (refusal returns the whole set via `refused_targets()`, stays selected + toast).
  Oracle (`scratchpad/der_oracle.py`, Blender 4.5 `bmesh.ops.dissolve_edges use_verts=True
  use_face_split=False`, selections by COORDINATE): **single → 6/9/5** (valence-2 endpoints removed),
  **2-shared → 5/7/4**, **4-top → 4/4/1** (cap→single quad) — ALL MATCHED BY VERTEX IDENTITY (incl. the
  HARD 4-top). Tests: `dissolve_edges_region_{single,two_shared,four_top}_matches_blender` +
  `join_edge_kill_vert_refuses_valence_three` + op-path `dissolve_edges_region_op_applies_and_restores`
  (through `apply_in_place`, proves the validate()+is_manifold() gate accepts it + snapshot inverse
  restores). Orphan-scan (`assert_post_op_clean`) green on all. `cargo test -p core_math` 493/0/6;
  `cargo build -p app` clean; no serialized-shape change. Closes REMAINING (a) from the 2026-07-16 entry
  below. Owner re-tests the full app path (input→tool→kernel→to_raster_data→render) per
  feedback_verify_app_path_not_kernel.

- **2026-07-16 — `he_source` orphan-warning class KILLED across the whole delete/dissolve family +
  oracle-identity gates. (`435f4d8` + follow-up commit.)** Owner saw repeating `[WARN] he_source: no
  valid prev or twin for he HalfEdgeId(N)` during EVERY dissolve/delete. NOT a loop-splice bug — the live
  relink already matches `bmesh_kernel_join_face_kill_edge`. It was a **freed-slot dereference**: freed
  half-edges left in inconsistent states (`delete_face`/`delete_edge`/`collapse_self_loops` nulled only
  face+twin, leaving next/prev dangling and walkable), and consumers (`overlay_lines`, `draw_selection`,
  `draw_poly`) walked those dead slots each frame and called `he_source` → warn branch. FIX (structural,
  mirrors `free_face`): **`free_edge(he)`** chokepoint (canonical dead state, idempotent — every
  `free_edges.push` routes through it, one push left in the crate) + **`is_edge_live(he)`** predicate +
  `he_source` silent on dead slots (warns ONLY on a genuine LIVE orphan now) + the three draw consumers
  skip non-live edges. Also closed the **gate asymmetry** — `local_rebuild_ops::run_snapshot` now gates
  `is_manifold()` too. Tests RED→GREEN (`freed_slots_canonical_*`, `zero_live_orphans_*`,
  `is_edge_live_recognizes_freed_slots`); oracle-identity gate (`dd_family_ref.json`,
  `dissolve_delete_family_matches_blender_oracle_counts` + `dissolve_vertex_merged_ngon_by_identity`) —
  dissolve/delete vert/edge/face + region MATCH Blender 4.5 by (v,e,f) and vertex identity. `cargo test
  -p core_math` 488/0/6; `cargo build -p app` clean; no serialized-shape change.
  **REMAINING (documented in COMPLETE_BUG_TRACKER, NOT half-shipped):** (a) `dissolve_edge` lacks the
  operator-level valence-2 `use_verts` cleanup — Blender default `dissolve_edges` on 1 cube edge =
  **6/9/5** (endpoints removed), AZRA = **8/11/5** (hexagon, kept). Follow-up = `join_edge_kill_vert`
  primitive + REGION `dissolve_edges` (join bounding faces + valence-2 sweep) matching oracle [2]/[3]/[4]
  — the [4] 4-top-edge collapse also needs slit-edge + valence-1 cleanup, hence deferred over rushing a
  corrupting impl. (b) "Delete Only Faces" (`DEL_ONLYFACES`) — kernel already correct (`dissolve_face`
  per face; matches oracle [11]/[12]); only a distinct app menu/keybind is missing.

- **2026-07-15 — `dissolve_edge` same-face SLIT guard (#11) DONE. Blender-oracle gated: REFUSE, not split.**
  `half_edge.rs:2078` now carries `if face_a == face_b { return None; }`, joining the `NO_TWIN` (:2051) and
  `NO_FACE` (:2055) guards. A **slit** edge has its half-edge and twin on the SAME face, so the documented
  contract at :2047 ("merge the **two** faces on either side") has no two faces to merge — precondition
  violated exactly like its two siblings.
  **What the unguarded body does** (line numbers are POST-fix, i.e. current): :2105 sets `face_a`'s rep to
  `he_next`, then :2108 (with `face_b` *being* `face_a`) nulls that same rep and :2109 pushes the slot onto
  `free_faces` — **freeing a face a live ring of half-edges still names**. Next `create_face()` reused the
  FaceId → two logical faces, one id, silently. `validate()` is **blind** (it only checks next/prev/twin
  round-trips, which a slit satisfies), so nothing downstream caught it. The relabel loop was unsound too:
  the splice preceding it cuts the single ring in two, so it never met its `cur == he_next` terminator and
  exited only via the `safety < 10000` cap.
  *(These cites were written PRE-fix and pointed at :2056/:2082/:2085/:2086 — the entry's own 22-line
  comment block shifted every one of them. Corrected at integration. Exactly the reference-layer rot the
  same-day CLAUDE_MD_AUDIT found in 25 rules; a doc that cites lines it moves is the disease, not a note
  about it.)*
  **Oracle (real, not a punt):** the config IS constructible in Blender — but only via unvalidated
  `from_pydata`; every normalizing entry point rejects it (`faces.new` → "same vert used multiple times";
  `mesh.validate()` **deletes** the face outright). On the constructed slit, `bmesh.ops.dissolve_edges`
  leaves it **bit-identical** (nf=1 ne=4 nv=5, ring [0,1,2,1,3]) across all four use_verts × use_face_split
  combos on **both 4.5 and 5.0**, and `bpy.ops.mesh.dissolve_edges` (the Ctrl+X path) returns FINISHED while
  changing nothing. A positive control (two quads sharing an edge → nf 2→1) proves the harness really
  dissolves, so "unchanged" is refusal, not a dead harness. **Never split, delete, or repair here.**
  **Refusal, not an assert** — a slit is a *reachable input*, not an internal invariant violation:
  `dissolve_edge` itself produces one when merging faces that already share >1 edge, so an assert would
  crash on a real cube-corner Ctrl+X. `None` is already the fn's honored refusal contract (`mod.rs:409` →
  `InvalidParams`; `local_rebuild_ops.rs:194-197`), and the guard sits **above** the snapshot so a refusal
  leaves the mesh byte-identical.
  **Test:** `dissolve_edge_refuses_same_face_slit` (`half_edge.rs`, by the `dissolve_*` cluster). Fixture is
  one `from_polygons(&[vec![0,1,3,1,2]])` call — no hand-authored arrays — the same repeated-vertex-ring
  trick that is the only thing that works in Blender. Asserts refusal + **no mutation** (empty free lists,
  face count, face rep non-null, no live he → freed face, and a field-by-field he diff; `HalfEdge` derives
  no `PartialEq`). Proven **RED** on unfixed code ("must refuse slit half-edge 1") → **GREEN** after.
  `cargo test -p core_math` 477 passed / 0 failed (476 baseline + 1), all 12 `dissolve` tests green incl.
  `golden_dissolve_edge_on_cube`; `cargo build -p app` clean. No new state → serialized-shape red line
  untouched.
  **NOT fixed here (one bug per task) — file these:** (1) **`apply_in_place` does not roll back on refusal**
  — `mod.rs:340` clones `before`, :452-453 reject, and the *mutated* `m` still returns to `undo.rs:397`,
  which `if let Ok(inv)` skips without restoring. On a cube corner, `DissolveEdge{5}` is a *normal* dissolve
  that legitimately makes a slit, is correctly rejected — **and the slit stays on the live mesh with no undo
  record**. This guard downgrades that repro from "two faces share a FaceId" to "mesh has a slit"; it does
  not close it. Rank at/above #11. (2) **`delete_face:1263`** — same missing-self-guard class, and worse:
  its corruption (two live faces sharing FaceId(0)) is **invisible to `is_manifold()`**. All 5 callers
  already do the check externally (tribal knowledge); mirror `dissolve_face:2113`. (3) **`merge_vertices:1183`**
  unguarded `free_verts.push` (= item 41). (4) **`from_polygons:2944` / `create_face:1979` accept repeated
  vertices** (only `len < 3` is rejected) — the upstream slit *producers*. (5) **AZRA's `validate()` passes a
  slit Blender's `validate()` deletes** — another instance of the known weak-guard root cause.

- **2026-07-15 — CIRCLE (LoopTools) region op — NEW TOOL, kernel + app, Blender-oracle gated 3 ways.**
  `HalfEdgeMesh::circle_region(faces, radius, flatten, regular, relax_passes)` reshapes a face region's
  boundary loop onto a circle (owner: select a 4x4 grid on a cube face → Circle → I → E = a round boss).
  **PURE RESHAPE — zero topology change** (vertex/face/half-edge counts and every id preserved; no
  `from_polygons` rebuild), so watertightness is preserved structurally rather than by repair.
  **LOOPTOOLS IS AVAILABLE HEADLESS** — it is a bundled extension, `addon_utils.enable(
  "bl_ext.blender_org.looptools")` works in `--background` (I expected an addon dead-end and did not get
  one). So this shipped with a REAL oracle: `circle_ref.py`/`circle_ref.json`, 4 cases.
  **THE TASK BRIEF'S MATH WAS WRONG ON TWO COUNTS — the oracle disproved both** (§0.1 "never guess
  geometry"; the brief said "radius=None => use the mean distance (Blender's default auto)" and "push each
  boundary vertex to radius R at its own angle"):
  1. **Blender's auto radius is a GEOMETRIC LEAST-SQUARES fit, not the mean distance.** They coincide
     EXACTLY when the loop is symmetric about its centroid — i.e. on every square/grid test — which is
     why this is a §0.3 trap. On the deliberately asymmetric oracle case the LSQ radius is **1.138111**
     and the mean is **1.137707**. Ported `circle_calculate_best_fit` (Gauss-Newton on (cx,cy,r) from
     (0,0,1)) as `best_fit_circle_2d`.
  2. **Blender's DEFAULT is `regular=True`** (vertices spread at EQUAL angles, anchored on the loop vertex
     closest to the centroid), not "its own angle" (that is `regular=False`). §0.2 = match the default;
     both are implemented, `regular` defaults to Blender's.
  **THE BIG FIND — LoopTools computes a best-fit centre and then THROWS IT AWAY.**
  `circle_project_regular`/`_non_regular` take each vertex's DIRECTION about the fitted centre `(x0,y0)`
  but write back `[cos(t)*r, sin(t)*r]` with **no `+x0,+y0`**, and those 2D coords are relative to `com`
  — so the circle is centred on the loop **CENTROID** while its radius is the best fit about a centre
  that is discarded. Confirmed empirically 3 ways (out-centre − com = (0,0) while the true LSQ centre is
  at +0.0247; and out-angle-about-com == in-angle-about-**fitted-centre**, to 3 dp, on 16/16 verts).
  Reproduced faithfully — it is invisible on symmetric regions and produces a perfect circle either way.
  **Gates (9 in core_math + 2 in app, all green):** three VERTEX-FOR-VERTEX oracle diffs (defaults /
  `regular=false` / `custom_radius=0.75`), equidistant+22.5°-spacing, closed-box-face watertight +
  manifold + counts-unchanged + `pos == center+dir*radius`, flatten planarizes a domed loop (and
  `flatten=false` preserves the along-normal offset), relax, determinism across 8 runs, and pinched-region
  refusal. **476 core_math green (was 467, 0 regressions), 178 app green, app builds dev+release.**
  **DIFFED BY IDENTITY, NOT SET MEMBERSHIP** (§8): both sides author the SAME 25 verts with the SAME index
  formula and `from_quads` allocates 1:1 in order, so `VertexId(i)` IS oracle vertex `i`. A set-membership
  check passes even when every vertex takes its NEIGHBOUR's target — the exact twisted-inset trap.
  **THE ORACLE INPUT IS LOAD-BEARING (§0.3):** the "asym" case moves ONE boundary vertex
  (-1,0,0)→(-0.6,0,0), which (a) makes the closest-to-centroid ANCHOR unique (0.625 vs 0.975) so the
  regular distribution is well-defined and reproducible, and (b) breaks the symmetry that makes
  centroid+mean coincide with the LSQ fit — a lazy fit CANNOT pass it. A symmetric grid would have passed
  with the wrong math.
  **DELIBERATE DIVERGENCES (documented on the fn):** (1) plane fit = the loop's Newell normal, because
  LoopTools' PCA covariance is SINGULAR for exactly the planar loops this tool targets and it then falls
  back to the nearest AXIS-ALIGNED normal (only correct for axis-aligned planes) — Newell is exact for any
  planar loop, so identical on every planar region and saner off it; (2) `flatten=false` keeps each
  vertex's along-normal offset instead of LoopTools' raycast-onto-adjacent-faces (non-default both sides,
  identical on a planar region); (3) **`relax_passes` is an AZRA EXTENSION** — Blender's Circle does NOT
  touch the interior (verified: only the 16 loop verts move), but the owner asked for the inner grid not
  to shear. Pinned-boundary **Jacobi** Laplacian (order-independent → deterministic); not a fudge: its
  fixed point is the harmonic solution with the circle as Dirichlet boundary = the classic evenly-
  distributed grid-in-a-disk, **and a regular grid is ALREADY harmonic, so relax is a no-op on geometry
  that doesn't need it**. App default 4 passes, `0` in the redo panel = Blender parity.
  **APP:** `poly/circle.rs` — one producer (`apply_circle`) shared by the Alt+C keybind, the Q/poly menus,
  the command palette (`cmd_poly_circle`) and the F9-style **Adjust Circle** redo panel (radius /
  regular / flatten / relax, live re-run from the pre-op snapshot, one settled undo). Alt+C registered in
  **InputSystem** (`ACTION_POLY_CIRCLE_REGION` + `bind_default`, ACTION_POLY_JOIN dispatch pattern — no
  raw key_pressed block); Alt+C is free in BOTH keymaps (plain `C` is circle **select**, a different
  tool). Selection is LEFT ON THE REGION (Circle → I → E chains). **The app producer is itself gated on
  the Blender oracle** at the exact defaults `poly_circle_faces` passes (`feedback_verify_app_path_not_
  kernel`: a green kernel + a wrong app call is how the inset bug shipped 3 times).
  **UNDO BUG AVOIDED:** `transaction()` snapshots and pushes UNCONDITIONALLY, so calling it around an op
  that can refuse would silently eat the user's next Ctrl+Z. `poly_circle_faces` DRY-RUNS on a scratch mesh
  and only opens the transaction on success (same shape as `poly_join_has_candidates`; cheap — the
  transaction already clones the whole scene graph). **NOTE: `poly_apply_inset_settled` has this SAME
  latent no-op-undo hole** (it calls `transaction()` then returns `!inner.is_empty()`) — left alone per
  scope control, flagged here for whoever next touches inset.
  **REUSE / dedup:** extracted `region_boundary_half_edges()` as THE single region-boundary rule (it is
  also exactly Blender's `circle_get_input` definition) and folded `inset_region_offset_dirs` AND
  `inset_region`'s own inline copy onto it; added `region_boundary_loops()` (ordered loops, refuses a
  pinched boundary like `dissolve_faces_region` refuses a non-disk region); extracted `newell_normal()`
  and folded `compute_face_normal` onto it. **Two more Newell copies remain** — `bevel_op::newell_idx`
  and the app's `poly/fill.rs::best_fit_plane_normal` — flagged in-code, not folded (refactor budget;
  bevel_op is BEVEL.md-governed).
  **REMAINING:** the interactive RADIUS HANDLE (out of scope this pass) — the numeric radius + redo panel
  ship instead, and `CircleLoopFit.boundary` already returns `(VertexId, unit radial dir)` pairs so a drag
  is `pos = center + dir*radius` with no re-fit (§8: never re-derive the pairing from ring order).

- **2026-07-15 — PHASE 2e (part 1): INSET EVEN OFFSET fixed (audit #15), Blender-oracle gated.**
  `inset_face` slid every vertex toward the face CENTROID by `amount` — which only coincides with Blender
  on a REGULAR polygon. New oracle `inset_ref.py`/`inset_ref.json` (box half-extents (2,1,1), the +Y top is
  4x2 = NON-square so the two methods diverge; `bpy.ops.mesh.inset(thickness=0.3, depth=0.0)`, UI defaults
  `use_even_offset=True`): Blender inner ring = **(±1.7, 1.0, ±0.7)** (uniform 0.3 gap from every edge);
  AZRA produced **(±1.7317, 1.0, ±0.8658)** (gaps 0.268/0.134). **`nv=12 nf=10` MATCHED both before and
  after** — i.e. watertight+manifold+topology gates all passed while the geometry was wrong: the exact
  §0.1 trap, only the vertex diff caught it.
  **Fix:** `inset_face_depth(f, thickness, depth)` (new; `inset_face` delegates with depth=0) offsets each
  edge inward along its **in-plane** normal `normal × (b−a)` and intersects the two offset lines at each
  vertex: `v' = v + t·(n1+n2)/(1+dot(n1,n2))` (solves dot(v'−v,n1)=dot(v'−v,n2)=t). Winding-invariant
  (flipping the ring flips BOTH N and the edge dir). `denom→0` (≈180° spike vertex) falls back to a plain
  single-edge offset instead of exploding. Topology path unchanged (rim quads + inner face).
  **Gates:** `inset_face_matches_blender_oracle_even_offset` (vertex-for-vertex vs the oracle + watertight
  + `is_manifold`) and `inset_face_even_offset_uniform_gap_on_irregular_ngon` (property gate: every edge of
  an irregular pentagon keeps a uniform `t` gap — what centroid math cannot do). **463 core_math green
  (0 regressions), app builds.**
  **THE APP-PATH TRAP (nearly shipped a kernel-only fix):** the **I key does NOT call `poly_inset_faces`**
  — it starts `poly_start_inset_drag`, and `poly_update_inset_drag` had the naive centroid rule
  **duplicated verbatim in the app layer** (`dir = (center-orig).normalize(); pos = orig + dir*amount`).
  A kernel-only fix would have left the actual default UX wrong while every core_math test passed
  (`feedback_verify_app_path_not_kernel`; CLAUDE.md "two code paths for one operation"). Fixed by making
  ONE rule: new **`HalfEdgeMesh::inset_offset_dirs(f)`** returns the per-vertex even-offset displacement
  PER UNIT thickness (carries the `1/(1+dot)` scale, NOT normalized) in `face_vertices` order;
  `inset_face_depth` uses it, and `PolyInsetDrag` now stores `offset_dirs` (was `face_centers`) captured
  from it before the inset, so the drag is just `pos = orig + dirs[i]*amount`. Both paths, one rule.
  **UNIT CHANGE (intentional):** `amount` was a corner-displacement toward the centroid; it is now
  Blender's `thickness` (a true edge offset). Same number now insets ~1.41x further on a square — the
  old value was `amount·sin(θ/2)`. Menu default stays 0.1; the I-key drag still starts at 0.
  **REMAINING in 2e (next increment, kept separate per §0.6):** app `poly_inset_faces` maps EACH selected
  face to its own `InsetFace` op = Blender's **Individual** mode, but Blender's `I` default is **REGION**
  (interior shared edges of a multi-face selection are NOT inset). Needs `inset_region(faces)` + an
  Individual toggle, and the app default must be Region (§0.2). Also unexposed: `depth` (the kernel now
  supports it).

- **2026-07-08 — BOOLEAN XL RESOLVED: robust winding-number classifier replaces the csg.js BSP.**
  Owner directive: "finish the boolean, as good as Blender, no corners, poly-only (no SDF/voxel fallback)."
  Root cause found in two layers: **(1) non-determinism** — the coplanar-merge / region-trace / coincident-
  cancel passes drove OUTPUT topology from `HashMap`/`HashSet` iteration order, so the SAME sequential
  boolean gave a different None/garbage EVERY run (this WAS the "usually breaks on the 2nd cut"). Fixed with
  `BTreeMap`/`BTreeSet` (762a4eb). **(2) the csg.js BSP itself** — its invert/clip coplanar tie-break is
  order-dependent; on a 2nd cut over a coplanar seam it emitted spurious FLIPPED faces far from the cut +
  coincident face+flip pairs → same-winding non-manifold → `is_manifold` rejects → boolean did nothing.
  **Fix (06c7f33): a winding-number classifier (libigl/Cork/Manifold approach), the new mainline;** BSP kept
  only as a validated fallback. Pipeline: ear-clip triangulate (NOT fan — fan folds the non-convex L-faces a
  prior cut leaves into back-wound triangles = the spurious-face root; reuses `poly_mesh::earclip_polygon`)
  → split each solid by the other's face planes (AABB broad-phased so flat walls aren't shattered) → classify
  every fragment by the GENERALIZED WINDING NUMBER of the other solid (geometric ground truth, can't produce
  a flipped/spurious face), probing just off both sides along the face normal (decisive in/out, or winding≈½
  = coplanar) → coplanar fragments resolved by op + original-normal parity (`Polygon.source`/`orig_normal`
  provenance, immune to inverts). Plus `greedy_merge_region` dissolves internal coplanar edges even in
  annulus regions (a cylinder-punched wall no longer pins T-junctions on the neighbour → clean quad).
  **VERIFIED:** sequential double corner-bite = clean 12v/8f L-prism, IDENTICAL to Blender EXACT's single
  slab cut (`slab_oracle.py`) and CLEANER than Blender applied sequentially (15v/10f vestigial seam). New
  adversarial battery (triple bite, union chain, diff→intersect, rotated→axis) all watertight manifolds.
  **423/423 core_math green, deterministic, app compiles.** Commits: 762a4eb (determinism+M0 oracle),
  06c7f33 (robust boolean), f7ce5b7 (battery). **§12 XL is DONE** — remaining polish (exact-arithmetic
  predicates for near-tangent adversarial input, true tri-tri SEGMENT intersection to avoid over-split on
  flat-wall-adjacent-to-hole, BVH for perf, non-destructive boolean modifier) is optional future work, not
  blocking; the user's repeated "2nd boolean breaks / messy segmentation" is fixed and Blender-verified.

- **2026-07-08 — BOOLEAN sequential-failure DIAGNOSED + coincident-face cancellation (partial). XL fix
  scoped (§12).** Reproduced the owner's "second boolean adds messy segmentation" on boxes: `box − cut1`
  then `M1 − cut2` fails IDENTICALLY across cases with `is_manifold` = "directed edge used by 2 faces (same
  winding)". So the gate is CORRECTLY rejecting garbage (no corruption), and the root cause is ALGORITHMIC
  (coplanar/coincident faces from the f32 csg.js BSP when a later cut plane aligns with an earlier
  boolean's face), NOT precision (f64 wouldn't fix a same-winding duplicate). Added `cancel_coincident_
  faces` (audit #2's exact recommendation): drop same-winding duplicate faces + cancel opposite-winding
  coincident pairs on the welded face list. Removes the EXACT-coincident class (all 15 boolean tests still
  pass) — but the reproduced sequential cases still fail because their offending faces are NOT exact
  duplicates; they are distinct faces the BSP tessellation leaves sharing an edge in the same winding. That
  is the f32-BSP coplanar-robustness LIMIT — the full fix is exact-predicate CSG (XL, §12). 418 core_math
  green, app builds. **Net today: the boolean never corrupts (gate), and the exact-coincident class is now
  cleaned; full sequential/complex robustness is the scoped XL project.**

- **2026-07-08 — EXTRUDE open-mesh VOLUME semantics (#2) + merge survivor determinism (Blender counter-
  verified).** A fully-OPEN region (no boundary edge twins a non-region face) now extrudes into a closed
  VOLUME like Blender (`extrude_region_volume`): keep the original surface flipped as one shell, duplicate
  ALL region verts as the other, bridge the rim with explicit-twinned walls, then a reverse-direction
  boundary-heal pass catches the lifted-cap interior edge `create_face` misses at a shared center vertex.
  Oracle `extrude_region_open_ref.py`: single quad → box nv=8/nf=6; 3×3 grid (interior center vert) →
  nv=18/nf=16, both watertight. A neighboured region (cube face) still MOVES (nv=16/nf=13 unchanged). Gate
  `extrude_region_open_mesh_makes_volume_matches_blender`. Plus: `poly_merge_vertices` sorts the selection
  so the survivor id is deterministic (was HashSet-order). 418 core_math green, app builds.

- **2026-07-07 — VERIFICATION-DEFECT FIXES (Blender counter-verified). #6 dissolve interior-vertex leak:**
  `dissolve_one_region` now frees region verts NOT on the merged ring (they became loose) — Blender oracle
  `dissolve_grid_ref.py` (dissolve all 4 grid quads) → nv=8/nf=1 (8-gon), center REMOVED; AZRA used to
  leak it (nv=9). Gate `dissolve_faces_region_frees_interior_vertex`. **COUNTER-VERIFY FINDING (extrude #2,
  deferred):** comparing AZRA vs Blender on OPEN-mesh region extrude showed a SEMANTIC divergence — Blender
  builds a closed VOLUME (keeps the original faces + duplicates ALL verts incl. interior; open quad→nv=8/
  nf=6 box, grid→nv=18/nf=16), whereas AZRA's re-point model lifts an open shell. Closed-mesh extrude
  matches Blender vertex-exact (nv=16/nf=13); the open-mesh "extrude creates volume" needs Blender's
  keep-original-if-neighboured logic — a bigger change, and the case is already SAFE (keystone gate rejects
  the current self-loop). Oracles saved: `extrude_region_open_ref.py`. 416 core_math green.

- **2026-07-07 — ADVERSARIAL-VERIFICATION KEYSTONE FIXES (workflow found 10 real defects).** A 21-agent
  read-only verification of the session's 11 commits confirmed 10 defects clustered on one gap: the app's
  shipping edit path installed geometry the unit tests reject. **Fixed the two keystone ones:** (#4) added
  `is_manifold()` check (d) — a **bowtie / fan-junction vertex** (incident faces forming >1 disjoint fan,
  edge-manifold but vertex-non-manifold) now REJECTED via union-find over each vertex's incident faces; this
  is EXACTLY the messy geometry a complex/sequential boolean emits that used to pass the install guard.
  (#1) gated `apply_in_place`'s snapshot tier on `is_manifold()` (was `validate()` only, mod.rs:427) —
  merge/extrude/dissolve non-manifold results are now atomically REFUSED (scene keeps `before`) instead of
  committed to scene+undo. Tests: `is_manifold_rejects_bowtie_vertex` + `is_manifold_accepts_boundary_fan_
  vertex` (no false-positive on open fans). 415 core_math green, app builds. **This addresses the owner's
  "second boolean adds messy segmentation / breaks the mesh" report** — the garbage is now rejected, not
  installed. REMAINING confirmed defects (own increments): extrude_region open-mesh/pinch (#2/#3), merge
  adjacent-collapse (#8), delete_edge/dissolve loose-vert parity (#5/#6/#7), merge survivor determinism.

- **2026-07-07 — BOOLEAN COPLANAR (#2) — the mesh-break does NOT reproduce; hardened the emit path.**
  Empirically probed 11 coplanar/coincident configs (union/diff/intersect × touching / coplanar-top /
  through-cut / flush-face / slab / IDENTICAL / thin-slab / coincident-footprint): ALL produce closed
  manifolds; the only None is the empty A−A difference (correct). `merge_coplanar` already dissolves the
  coplanar faces the audit feared. The concrete fix: `polygons_to_mesh` now guards the merged AND the
  triangulated-fallback build on `is_manifold()` and returns None if neither is a clean closed manifold
  (was `Some(build(&faces))` UNVALIDATED) — the kernel can no longer emit non-manifold geometry. Gate
  `coplanar_and_coincident_booleans_stay_manifold` (+ Blender oracle `bool_coplanar_oracle.py`: touching-box
  union closed+manifold, bbox x∈[-1,3]). 413 core_math green. **Phase 2c (boolean) COMPLETE.** Remaining
  boolean parity (exact-arithmetic robust solver, self-intersection, attribute carry) is deferred XL work.

- **2026-07-07 — VERIFICATION SWEEP + MERGE-AT-CENTER (#13).** Overnight double-check: full core_math suite
  **412 green** (dev) + **release build green** (3m23s) — the regression protocol (dev + release + tests)
  now complete for the whole session's work; every new op stayed Blender-oracle-verified. Then MERGE #13:
  `merge_vertices_to(verts, target)` welds a selection to ONE survivor at an explicit position (Blender
  "Merge at Center" = centroid), replacing the chained-midpoint running average that matched no Blender
  mode. `PolyEdit::MergeVerticesAt`; `poly_merge_vertices` sends ONE op at the computed centroid. Gate
  `merge_vertices_to_centroid_is_deterministic`. (Adjacent-vert self-loop #8 still deferred — caught by the
  gate, not yet collapse-cleaned.) 412 green, app builds.

- **2026-07-07 — REGION EXTRUDE (Blender default E) — completes Phase 2a.** `extrude_region(faces, offset)`
  lifts a face SET as one shell: BOUNDARY verts duplicated (original stays for the wall bottom, lifted for
  the cap), INTERIOR verts (all incident faces in region) moved in place, walls only on the region's outer
  boundary — so interior shared edges stay WELDED (per-face extrude doubled them). Twinned EXPLICITLY (not
  `create_face` — with many detached boundary edges per vertex, `find_half_edge`/`vertex_fan` can't locate
  the neighbour → 32 boundary edges on the first attempt; §8). up_at/down_at pair the two verticals at each
  boundary vertex around the loop. Oracle `extrude_region_ref.py` (box, split top, extrude both top faces
  +Y 0.5) → nv=16/nf=13, exactly 6 verts lifted (not 8 = interior edge welded) — vertex-exact, manifold,
  watertight. Added `PolyEdit::ExtrudeRegion`; the fixed-offset command `poly_extrude_faces` now routes
  multi-face selections through it (single face keeps the element-delta `ExtrudeFace`). 411 core_math green,
  app builds. FOLLOW-UP (needs the user / manual test): the INTERACTIVE extrude drag still lifts per-face —
  routing it through region needs per-frame lifted-vert tracking.

- **2026-07-07 — SOLIDIFY (thicken) shipped — the owner's "extrude → thicken" request.** `solidify(thickness,
  offset)`: inner shell along the vertex normal (offset biases the two shells), reversed inner faces, rim
  quads per open boundary — all via `create_face` auto-twin (leans on the #6 delete_face fix for robust
  boundary-vertex fans). Wired the previously-dead `mesh_sync.rs` Solidify modifier stub. Oracle
  `solidify_ref.py`: cube → hollow shell nv=16/nf=12, open quad → slab nv=8/nf=6, both closed + manifold —
  gated on topology invariants (Blender's exact vertex-normal averaging differs). 410 core_math green.
  NOTE (overnight autonomous run): prioritizing KERNEL ops with auto-verifiable Blender golden tests over
  app-UI-only wiring (Symmetrize etc.) which can't be verified unattended.

- **2026-07-07 — PHASE 2c BOOLEAN recursion cap (#12) — crash backstop.** The BSP `build` picks the first
  polygon as splitter with no balancing, so dense / self-intersecting / stacked-coplanar input degenerates
  the tree toward a linked list (depth ≈ polygon count) → stack overflow (a hard crash). Added a
  `MAX_BSP_DEPTH` cap (`build` carries a depth; at the cap it dumps the tail at the leaf instead of
  recursing) + a `MAX_BSP_POLYS` input ceiling (refuse absurd inputs with None). Since a balanced BSP is
  only ~log2(N) deep, the cap never triggers on real meshes. Gate `bsp_build_depth_is_capped_on_degenerate_
  input` (6000 stacked quads → linear tree → capped, no overflow). **Dead-end (→ §8):** first set the cap
  at 3000 — still overflowed in DEBUG (unoptimized `build` frames are ~2.7KB, so ~3000 blow the 8MB stack);
  lowered to 1000 (3× margin, still 50× any real tree depth). 409 core_math green, app builds. REMAINING
  2c: coplanar-face drop (#2).

- **2026-07-07 — PHASE 2c BOOLEAN local-frame (#7) — the far-from-origin "mesh break" fixed.** The BSP
  classified with a fixed WORLD epsilon (PLANE_EPS=1e-5), so a boolean that worked at the origin CRACKED
  far away (f32 resolution at magnitude C is ~C·1.2e-7 → beyond ~C=100 an on-plane point is randomly
  FRONT/BACK). Fix: `mesh_boolean` now recenters both polygon soups on the combined AABB (`Polygon::
  translated` shifts verts + plane.w exactly), runs the whole BSP + weld + dissolve in that small-coord
  local frame, then shifts the result back to world. Booleans are translation-invariant (matches Blender).
  **Oracle method note:** BSP output CANNOT be vertex-diffed vs Blender's EXACT solver (different
  tessellation — Blender gives nv=14/nf=9 for A−B, ours differs), so `bool_diff_oracle.py` records the
  algorithm-INDEPENDENT invariants our result must also satisfy (closed, manifold, bbox == minuend ±1) and
  the test additionally asserts translation-invariance (identical topology at origin vs +1000). Gate
  `boolean_local_frame_translation_invariant_far_from_origin`. 408 core_math green, app builds. REMAINING
  2c: coplanar-face drop (#2) + BSP recursion cap (#12).

- **2026-07-07 — PHASE 2b DISSOLVE VERTICES — Blender-exact (completes the delete/dissolve X-menu).**
  Oracle `dissolve_vert_ref.py` (Blender Dissolve Vertices on corner (1,1,1)) → nv=7/nf=4, the 3 incident
  faces merge into ONE hexagon, corner removed. `dissolve_vertex` REUSES `dissolve_faces_region` (the fan
  around v IS one face region whose interior vertex is v) then frees the now-loose v. Added
  `PolyEdit::DissolveVertex`; Ctrl+X now routes Vertex/Edge/Face (the Vertex-mode dissolve dead key, audit
  #4, now works). Gate `dissolve_vertex_matches_blender_oracle` (vertex-exact hexagon + corner gone).
  407 core_math green, app builds. All three X-menu delete cells + all three dissolve cells now exist
  (only `dissolve_edge` slit #11 remains). No duplicate merge logic — dissolve_vertex builds on the region
  merge.

- **2026-07-07 — PHASE 2b DISSOLVE FACES region-merge (#10) — Blender-exact.** Oracle
  `dissolve_faces_ref.py` (Blender Dissolve Faces on the cube's +Y and +Z faces) → nv=8/nf=5, the merged
  face a HEXAGON. Replaced the per-face hole-punch (which left non-manifold wire edges on adjacent
  selections) with `dissolve_faces_region`: BFS each edge-connected component, trace its outer boundary
  loop (in-region he whose twin face is out-of-region), free the interior via `delete_face`, rebuild ONE
  n-gon via `create_face` (edges auto-twin the outside). Non-disk regions (pinch/annulus) return None (safe
  no-op). Added `PolyEdit::DissolveFaces { faces }`; Face-mode `poly_dissolve` now sends ONE region op.
  Gate `dissolve_faces_region_matches_blender_oracle` (watertight + manifold + hexagon vertex-exact). 406
  core_math green, app builds. REMAINING 2b: DissolveVertex + dissolve_edge slit (#11).

- **2026-07-07 — PHASE 2b DELETE (vertex + edge) — the owner's dead keys, now working + Blender-exact.**
  Oracle `delete_ref.py` (Blender X-menu Delete Vertices / Delete Edges on a cube): corner delete →
  nv=7/nf=3, edge delete → nv=8/nf=4. Implemented `delete_vertex` (free every incident face via the fixed
  `delete_face`, then free the loose vertex) and `delete_edge` (free the ≤2 adjacent faces + the edge, drop
  loose endpoints) — both manifold + vertex-exact (`delete_vertex_matches_blender_oracle`,
  `delete_edge_matches_blender_oracle`). Added `PolyEdit::DeleteVertex/DeleteEdge` + dispatch +
  `poly_delete_vertices/edges`, and routed the Delete key by work mode (Face/Edge/Vertex) in
  `key_dispatch.rs` — Vertex & Edge delete were dead keys (audit #4/#5). 405 core_math green, app builds.
  Reused `delete_face` throughout (no duplicate deletion logic). NEXT: DissolveVertex + the dissolve #10/#11
  fixes, then tighten `apply_in_place` onto `is_manifold()`.

- **2026-07-07 — PHASE 2a EXTRUDE (twin re-link) + local_ops gate ENABLED + delete_face #6 fix.** Generated
  the first non-bevel Blender oracle (`extrude_ref.py`, Extrude Region+Move) — the faithful user-operator
  path. Rewrote `extrude_face` to re-link every twin (wall-top↔cap, wall-bottom↔saved base neighbour,
  adjacent verticals) + fix the vertex disk pointers → manifold + watertight + vertex-exact to Blender
  (nv=12/nf=10). Enabled the deferred edit-op is_manifold gate; the 5 extrude tests now pass *because*
  extrude is correct. **Two dead-ends (→ §8):** (1) dropped the `he0.prev` cyclic link on rewrite → `he()`
  OOB panic (validate walks `prev.next`); (2) the `extrude→inset` composition failed is_manifold on a
  `mislinked vertex.half_edge` — traced to `delete_face` (used by inset) leaving a dangling pointer (audit
  #6) that truncates `vertex_fan`→`find_half_edge`→mis-paired twins in the next `create_face`. Fixed
  `delete_face` to repoint stale corner pointers to a live outgoing edge (root-cause, advances 2b). 403
  core_math green, app builds. **NEXT:** region extrude (multi-face weld) or Phase 2b delete/dissolve ops.

- **2026-07-07 — PLAN CREATED + Phase 1 keystone landed (classifier + boolean gate).** Generalized the
  BEVEL.md oracle method into this tool-by-tool polish plan, anchored to the 2026-07-07 audit. Confirmed
  Blender 4.5/5.0 present. Added `HalfEdgeMesh::is_manifold()` (the checks `validate()` is blind to) + 5
  unit tests; wired the boolean install guard on it. **KEY EMPIRICAL FINDING:** enabling the classifier on
  the edit-op path (`local_ops` run/run_snapshot) fails exactly 5 tests — ALL `extrude_face` — and no
  others (subdivide/split_edge/loop-cut/knife/bevel all stay green). That precisely confirms audit bug #3
  (extrude never re-links twins → non-manifold) AND proves the classifier is accurate (single, correct
  culprit, zero false positives). Deferred the local_ops gate to Phase 2a per "fix before gate." Next:
  Phase 2a extrude with a Blender oracle → then enable the local_ops gate → green.

---

## §8. Graveyard — poly-polish dead-ends (never retry)

*(Seed from the audit + BEVEL.md §6. Append here when a polish approach fails.)*
- **NEVER assume a Blender "auto/best fit" value is the obvious closed form.** The Circle brief specified
  "radius = the mean distance (Blender's default auto)". It is NOT: LoopTools runs a Gauss-Newton
  GEOMETRIC least-squares fit. Mean and LSQ agree **exactly** on any loop symmetric about its centroid —
  i.e. on every square/grid/cube-face test anyone would naturally write — and diverge only on asymmetric
  input (1.137707 vs 1.138111 on the oracle case). **Lesson:** when a spec names a Blender default,
  READ THE ADDON SOURCE / run the oracle on a deliberately ASYMMETRIC input; a symmetric gate cannot tell
  the two apart, and it would have shipped the wrong fit with a fully green suite. *(2026-07-15)*
- **NEVER assume a bundled Blender addon can't be driven headless.** LoopTools looked like a dead-end
  ("addons may not be enabled in background mode") but `addon_utils.enable("bl_ext.blender_org.looptools")`
  works fine under `--background` in 4.5 (it is an extension now, hence the `bl_ext.blender_org.` prefix —
  plain `"mesh_looptools"` does NOT resolve). **Lesson:** probe for the oracle before settling for
  property-only gates. *(2026-07-15)*
- **Do NOT trust a Blender op's own internal variables to mean what they're named.** LoopTools' Circle
  computes a best-fit centre `(x0,y0)` and then never adds it back: `circle_project_regular` writes
  `[cos(t)*r, sin(t)*r]` in a frame whose ORIGIN is the loop centroid, so the circle is centred on the
  CENTROID while `r` is the best fit about the discarded centre. Reading only the `circle_calculate_best_fit`
  call site would have produced a circle centred at the fitted centre = wrong vs Blender. **Lesson:** trace
  the value to where it is USED, and confirm against dumped vertices (the empirical check caught it:
  out-centre − com = (0,0) while the true LSQ centre sat at +0.0247). *(2026-07-15)*
- **Wrapping an op that can REFUSE in `transaction()` silently eats a Ctrl+Z.** `transaction()` snapshots
  and pushes an undo entry UNCONDITIONALLY — it has no notion of "the closure did nothing". So an op with
  a legitimate no-op path (circle on a pinched region, inset on a bad selection) must decide BEFORE
  opening the transaction — dry-run on a scratch mesh, or a `*_has_candidates()` predicate. **`poly_
  apply_inset_settled` still has this hole.** *(2026-07-15)*
- **A set-membership vertex diff (`vertices.iter().any(|v| close_to(want))`) cannot catch a TWISTED
  result** — the whole §8 inset trap produces the identical position SET with every vertex on its
  NEIGHBOUR's target. It is the pattern most of the existing oracle gates use, and it is only safe when
  the op's failure mode can't permute the ring. **Lesson:** when both sides can be authored from the same
  vertex list (`from_quads`/`from_polygons` allocate 1:1 in order, so `VertexId(i)` is oracle vertex `i`),
  diff BY INDEX. *(2026-07-15)*
- **NEVER zip per-vertex data against `face_vertices(new_face)` — it is DESTINATION-ordered, i.e. the ring
  ROTATED BY ONE vs what `create_face` was given.** `face_vertices` (half_edge.rs:303) pushes
  `he(cur).vertex` (the destination) starting at `face.half_edge`, so `create_face(&[a,b,c,d])` reads back
  as `[b,c,d,a]`. The inset drag computed `inset_offset_dirs` in source order and zipped it against
  `face_vertices(inner)` → **every vertex got its NEIGHBOUR's displacement = a visibly TWISTED / ROTATED
  inset** (owner: "inset rotate face"). It was invisible to every kernel test because `inset_face` is
  internally self-consistent (it builds and consumes the same order) — only the APP path was wrong, for
  the THIRD time in this tool (see the §7 2026-07-15 entries). **Rule: pair by vertex IDENTITY.** Ops that
  hand work to an interactive drag must RETURN `(VertexId, data)` pairs (as `inset_region` ->
  `InsetRegionResult.boundary` does); the drag must never re-derive the correspondence from ring order.
  Gates: `face_vertices_returns_destinations_ring_rotated_by_one` (documents the trap) +
  `inset_region_drag_simulation_lands_blender_ring` (runs the drag's exact steps and diffs Blender).
- **Rewriting an in-place half-edge op and forgetting to close the `prev` cycle.** `extrude_face` rewrite
  set `he3.next = he0` but not `he0.prev = he3` → `validate()`/`he_source` walk `he(NO_EDGE)` → index-OOB
  panic that cascades to every test touching extrude. **Lesson:** when hand-wiring a face loop, set BOTH
  `next` and `prev` on every half-edge (the alloc + the explicit closes), and run the op through
  `validate()` immediately. *(2026-07-07)*
- **Assuming `extrude_face` output was the only extrude bug.** Fixing extrude's twins exposed that
  `inset_face`'s `delete_face` leaves a dangling `vertex.half_edge`, which only bites the NEXT op
  (`create_face`→`find_half_edge`→`vertex_fan` truncates → mis-paired twin). **Lesson:** an op that rebuilds
  via `create_face`/`find_half_edge` is only as correct as every PRIOR op's vertex.half_edge hygiene — fix
  the pointer at the mutation site (`delete_face`), not the reader. *(2026-07-07)*
- **Trusting `validate()` / watertight as proof of correctness.** It only checks twin symmetry + next/prev
  cycles — blind to non-manifold edges, degenerate/self-loop faces, stale `vertex.half_edge`. A watertight
  bevel of a closed mesh can still open holes; a boolean can install shattered topology. **Lesson:** add the
  manifold classifier (Phase 1) AND diff geometry against Blender. (BEVEL.md §6 #11.)
- **`create_face` auto-twinning to stitch region-extrude walls.** With many boundary edges detached per
  vertex, `find_half_edge`→`vertex_fan` can't locate the neighbour (the fan stops at the first NO_TWIN
  sector), so the walls stayed un-twinned → 32 boundary edges. **Lesson:** when an op detaches multiple
  edges around a shared vertex, twin the new geometry EXPLICITLY (capture the neighbour + pair verticals by
  boundary vertex), don't rely on `create_face`'s position/fan search. *(2026-07-07)*
- **A recursion depth cap sized for the RELEASE stack.** Set `MAX_BSP_DEPTH=3000` and the debug test still
  overflowed — unoptimized (`cargo test`) stack frames are ~2.7KB, so ~3000 of them blow the 8MB stack.
  **Lesson:** size recursion caps for the DEBUG build (worst case), not release; verify with an actual
  deep-recursion test. A cap that's 50× the real max still leaves huge margin. *(2026-07-07)*
- **`from_polygons` polygon-soup rebuild as the default topology op.** Loses stable vertex/edge/face IDs +
  provenance (prime suspect for "loop cut drops a vertex"). **Lesson:** prefer real ID-preserving half-edge
  ops; rebuild is a last resort. (feedback_half_edge_hard_frozen.)

---

## §12. XL — Robust boolean solver (the sequential/complex-boolean fix)

**Problem (diagnosed 2026-07-08):** the f32 csg.js solid-BSP (`mesh_boolean.rs`) is not robust on complex /
already-boolean'd / coplanar-heavy input. A second boolean whose cut plane aligns with a face produced by
an earlier boolean yields **non-manifold "same-winding duplicate edge"** output — distinct faces the BSP
tessellation leaves on the same side of an edge. The Phase-1 `is_manifold` install guard REJECTS this
(scene kept intact — no corruption), so today the symptom is "the boolean does nothing," not "garbage
installed." Making these booleans SUCCEED is the goal.

**Ruled out:** f64 arithmetic alone (the failure is a same-winding *duplicate*, not a classification
precision error); `cancel_coincident_faces` (added — removes EXACT coincident faces, a real but narrow
subset; the offending faces here are not exact duplicates).

**Candidate approaches (own project, gated, each Blender-counter-verified):**
1. **Robust coplanar/BSP rework** — track face provenance (which input solid, in/out) through the clip so
   coplanar faces are assigned once and consistently; add a manifold-repair pass that resolves same-winding
   edges by dropping the interior duplicate. Medium-high risk, incremental, keeps the current architecture.
2. **Exact predicates (Shewchuk orient3d/insphere)** on an f64/adaptive BSP — the "correct" fix; robust but
   a substantial rewrite of `split_polygon`/classification. XL.
3. **Adopt a vetted CSG kernel** (e.g. a Rust port of Cork / mesh-arrangements, or `truck`'s boolean once it
   matures — see `reference_truck_cad_kernel_eval`) — biggest change, best long-term robustness.

**Recommendation:** approach 1 first (incremental, in-place, unblocks the common sequential-cut cases) with
the `is_manifold` gate as the safety net throughout; escalate to 2/3 only if 1 plateaus. Gate every step on
the sequential-boolean repro (`box − cut1 − cut2` family) reaching a closed manifold, counter-verified
against Blender's Exact solver for shape/bbox/manifold (not vertex-for-vertex — tessellation differs).
