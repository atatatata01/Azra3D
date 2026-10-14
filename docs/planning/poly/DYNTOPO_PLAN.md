# DYNTOPO — Blender-tech Dynamic Topology (gated plan)

Owner directive 2026-08-19: **"DYNTOPO NEEDS TO BE LIKE BLENDER TECH."**
Local edge subdivide/collapse under the brush, during the stroke, on the half-edge mesh —
untouched geometry keeps its exact vertices/ids forever. This REPLACES the interim
live-Dynamesh dyntopo (Remesh-modifier-based, 2026-08-19) once Phase B lands; the modifier
path stays as the *Voxel Remesh* feature it always was.

## What Blender does (reference: sculpt dyntopo, `sculpt_dyntopo.c` model)
- Before applying a brush step, refine topology inside the brush radius:
  - **Subdivide** edges longer than `detail_len` (long-edge queue, recursive).
  - **Collapse** edges shorter than `0.4 × detail_len` (short-edge queue).
- Dyntopo meshes are **triangles**; a quad touched by the brush is triangulated first.
- Detail modes: Relative (screen px) / Constant / Brush-relative.
- Undo is a topology-aware log (NOT sparse position deltas).

## Non-negotiables (from CLAUDE.md)
- `HalfEdgeMesh` serialized shape untouched. All ops are runtime Euler ops.
- Compose EXISTING ID-preserving ops — never `from_polygons` rebuilds:
  `split_edge` (euler.rs:12), `split_face_between` (euler.rs:82),
  `merge_vertices_to` (ops_dissolve.rs:55). No duplicated math.
- Every refine call is budget-capped (Frame Budget rule) and must keep
  `validate()` / `is_manifold()` green — asserted by kernel tests.
- `cargo test -p core_math` after every change.

## Phases

### Phase A — refine kernel (core_math) — ✅ LANDED 2026-08-19
`half_edge/ops_dyntopo.rs`: `HalfEdgeMesh::dyntopo_refine_region(&DyntopoParams) -> DyntopoResult`.
1. Fan-triangulate region faces in place (`split_face_between`, stable ids).
2. Long-edge queue: `split_edge(he, 0.5)` + re-triangulate the two adjacent quads.
   An edge bordering a non-tri face outside the region triangulates that face first
   (topology ripples ≤ 1 ring past the brush, same as Blender).
3. Short-edge queue (optional, `collapse: true`): link-condition guard (common-neighbor
   count ≤ 2, else skip — never a bowtie), then `merge_vertices_to_ex(.., false)` at the
   midpoint. One `recompute_normals_region(touched)` pass at the end.
4. Hard `max_ops` budget; `budget_hit` reported honestly.
Tests: region fully refined to ≤ detail, far verts bit-identical (id + position),
manifold after subdivide/collapse/budget-abort, quads outside the brush stay quads.

**Known Phase-A cost, budgeted not hidden:** `merge_vertices_to_ex` redirects via a full
half-edge scan and `collapse_self_loops` is O(mesh) — bounded by `max_ops`, replaced in B2.

### Phase B — stroke integration (app) — ✅ LANDED 2026-08-19 (B1 + B3 + B4; B2 open)
B1. Call the kernel per stamp on `w.mesh` (working) over the dab region BEFORE
    `displace_verts_for_dabs`; detail from the Dyntopo settings popover (Constant =
    world detail from resolution analog; Brush = radius / detail).
    Runtime-state patching on topo change (the dead-zone class lives here — see
    `project_poly_sculpt_dead_zone_class` memory):
    - `VertGrid`: insert new verts / remove collapsed (or region rebuild; grid is never
      rebuilt today — add an explicit invalidate+rebuild on refine).
    - CSR incident-face cache: rebuild on refine (lazy CSR already exists §8.2).
    - `deposit_applied` / `first_touch`: vert ids survive splits (new verts get fresh
      entries); collapsed survivors keep theirs; document the merge rule.
    - Display: `poly_sculpt_flush_display` full resync on the frames a refine actually
      changed topology (steady-state strokes over refined ground no-op → zero cost).
B2. Localize collapse: fan-local redirect + local self-loop splice (extend
    `merge_vertices_to_ex`, one implementation) — removes the O(mesh) scans.
B3. Undo: a stroke whose refine changed topology commits through the EXISTING
    count-mismatch path (`poly_sculpt_write_direct`) wrapped in one transaction —
    whole-mesh undo for topo strokes, sparse undo for pure-position strokes. The sparse
    episode is INVALID the moment refine fires; convert at first refine, never mix.
B4. UI: Dyntopo toggle switches to this tech; Detailing popover gains Relative
    (screen-px) mode; the Remesh-modifier path returns to being plain Voxel Remesh.

### Phase C — polish (later)
Detail-flood fill (Blender's detail flood), topology rake, sidecar (UV/seam/crease)
survival policy for refined regions, perf gate on the owner's 161k fixture.

## Session log
- 2026-08-19: Plan created; Phase A landed (kernel + `merge_vertices_to_ex` param
  extension + 4 kernel tests, core_math green except 3 pre-existing owner-WIP reds).
- 2026-08-19 (same session, owner "GO"): **Phase B landed.**
  * B1: `poly_dyntopo_refine_for_dabs` runs the kernel per stamp in
    `poly_sculpt_apply_dabs` BEFORE displacement (this stamp already sculpts the refined
    verts); on topo change `rebuild_working_derived` rebuilds display raster + corner CSR
    + VertGrid wholesale (never partial — the dead-zone class), display re-derives via
    the established count-mismatch fallback (`poly_display_dirty` + MESH). Budget 800
    ops/frame (sized by the collapse's O(mesh) scan; refine continues next stamp on
    `budget_hit`). Layer-slot strokes fail closed (gate = `poly_sculpt_layer_slot`, the
    SAME predicate the commit choke uses — gating on `poly_edit` alone could desync).
  * B3: `commit_episode`'s topo-diverged branch — at commit the document is still
    pre-stroke, so it captures a standard `Full` snapshot ("Dyntopo stroke"), installs
    the working via `poly_sculpt_write_direct` (now pub(crate)), drops the working, and
    pushes ONE entry; undo/redo ride the battle-tested Full arm. Sparse path untouched
    for topo-fixed strokes (gated test).
  * B4: header Dyntopo toggle = global tool state (`sculpt.dyn_topo`); popover =
    kernel knobs (Detailing Constant edge-length / Brush edges-across, Refine Method
    Subdivide Edges / Subdivide Collapse). The v2 modifier-based dyntopo
    (`poly_dyntopo_set`/`enabled`/`apply_brush_detail`) is REMOVED; Remesh modifier is
    plain Voxel Remesh again. Gates: `dyntopo_stroke_lands_with_whole_step_undo`,
    `dyntopo_off_keeps_sparse_path` + episode lifecycle suite green.
  * B2 (localized collapse, no O(mesh) scans) and Phase C remain open.
- 2026-08-23 (owner: "dyntopo still do not work well — bug with symmetry, slow remesh,
  check Blender code"): **B2a landed — seeded candidate gather.** The kernel's queue
  SEEDING was two whole-mesh scans PER DAB (`ops_dyntopo.rs` face scan + half-edge scan),
  unbounded by the 800-op budget and ×2–8 under symmetry — the dominant steady-state cost,
  not the Euler ops. `dyntopo_refine_region_seeded` now takes the stroke's
  `sculpt_radius_query` verts and gathers faces/edges through their fans in O(region) —
  Blender parity (pbvh_bmesh.cc `long/short_edge_queue_create` iterate ONLY
  brush-intersecting PBVH nodes; verified against v4.2 source). Whole-mesh arm kept for
  `None` callers/tests. Symmetry fixes in the same session (sculpt.rs stroke driver):
  plane-epsilon dedup ported from `expand_voxel_symmetry` (on-plane dab no longer deposits
  twice), mirror instances INTERLEAVED per sample so a drained op budget refines both
  sides equally (old order refined only the original side when the budget died mid-frame),
  and the stroke tangent is computed from primary samples BEFORE expansion (it used to
  span original→mirrored point). Still open: B2 proper (`merge_vertices_to_ex` fan-local
  redirect + local self-loop splice) and Phase C. Blender reference facts for B2/C:
  collapse threshold 0.4×detail (`BKE_pbvh_bmesh_detail_size_set`), collapse runs BEFORE
  subdivide, collapse survivor prefers the boundary vert and does NOT move when on a
  boundary (`pbvh_bmesh_collapse_edge`), symmetry = re-run the whole brush action per
  mirrored pass (`do_symmetrical_brush_actions` → `SCULPT_cache_calc_brushdata_symm`), no
  seam welding anywhere.
