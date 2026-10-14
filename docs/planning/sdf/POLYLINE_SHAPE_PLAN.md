# POLYLINE_SHAPE_PLAN.md — Polyline SDF Shape (2 tools) + Seam Boolean

**Created:** 2026-07-13 (owner design session). Owner-decided sequencing:
**(0) Seam boolean first → (1) Planar Polyline tool (easiest) → (2) 3D Spine/Pipe tool.**

Owner's ask (verbatim intent): a polyline SDF shape, open or closed; shell-able; bevel OR chamfer
per point; easy point add/remove; blends with other SDFs; open → thickness variable; closed →
bevel/chamfer on the end (extrude-cap) silhouette edge. Plus a new boolean that creates a **cut
line on a surface** (decided: SEAM operator — groove/bead along the A∩B surface intersection,
V / U / square cross-sections, cut-in or raised polarity).

Owner picked the maximal option on every axis (both planar+3D, both wall+tube thickness, all
cross-sections) → split into **two tools** shipped separately rather than one mega-shape.

---

## What already exists (reuse, do NOT duplicate)

| Piece | Where | State |
|---|---|---|
| Closed 2D polyline profile + per-vertex `bevel_radius` (round) | `core_math/src/curves.rs` `CurveProfile`/`CurveVertex` | SHIPPED |
| Extruded profile shape, GPU-baked (shape tag 9) | `SdfShape::CurveExtrude` + clip-curve side-channel in `serialize.rs:274-325` | SHIPPED |
| Winding-number 2D polygon SDF | `curves.rs::sdf_polygon_2d` | SHIPPED |
| Point editor tool (add/remove/drag verts on a workplane) | `app/src/clip_curve_tool.rs` | SHIPPED (extend, don't fork) |
| Rounded/chamfered edge join math | `volume_bake.wgsl::bevel_2d_profiled` (+ CPU twin) | SHIPPED |
| Seam cross-section reference math (hg_sdf Groove/Tongue/Engrave) | git history — deleted in SDF_TOOLS_PLAN ITEM 4 v2 (~2026-07-07) | RECOVERABLE |

---

## Tool 0 — SEAM BOOLEAN (in progress, own build task)

New `BooleanOp` variant, appended LAST (bincode-positional). Params: width, depth,
profile (V=chamfer / U=round / square), raised (bead) flag. Must hold in all eval paths
(CPU `sdf_math.rs`, bake `volume_bake.wgsl`, parity WGSL/GLSL), be cull-proven
(far-B ⇒ result == acc), hash-covered, and get UI with undo. Details + session log live in
`SDF_TOOLS_PLAN.md` ITEM 5.

## Tool 1 — PLANAR POLYLINE (next after seam; "easiest" per owner)

Grow `CurveExtrude` (or a sibling mode on it) rather than a new parallel shape:

- **P1 — closed profile upgrades:**
  - Per-vertex **chamfer** flag next to `bevel_radius` (`CurveVertex` gains a corner-mode; check
    bincode compat — CurveVertex is serialized inside CurveProfile: adding a field is the
    field-trap. Prefer packing corner-mode into the sign of bevel_radius or a parallel
    `#[serde(default)]`-safe route — VERIFY which is safe for in-blob serde, see
    `project_bincode_save_format_field_trap`).
  - **Shell**: `abs(d_profile_extruded) - wall_t` (one line, both CPU + WGSL) with wall_t param.
  - **Rim (cap silhouette) bevel/chamfer**: apply `bevel_2d_profiled` to the profile↔depth max
    join instead of hard `max(d2d, |axis|-half_depth)`.
- **P2 — open profile + wall/ribbon thickness:** open polyline → offset by thickness in-plane
  (distance-to-open-polyline − T/2, no winding fill) → same extrude/shell/rim machinery.
- **Editor:** extend `clip_curve_tool.rs` — click add, del/click remove, drag; per-point
  round/chamfer toggle in properties. All edits undo via transaction/episode.
- **BOUNDING CONTRACT (the load-bearing risk):** `bounding_radius`/`bounding_aabb`/
  `provably_exterior` must cover profile extent + thickness + shell + rim; CurveExtrude already
  opts out of inscribed-sphere donors — keep it that way for open/shell modes. A wrong bound =
  sparse cache drops bricks = holes (the P0-B1 lesson).
- Blending: free (it's a prim in the boolean chain).

## Tool 2 — 3D SPINE / PIPE (last; biggest new math)

> **Full execution plan (2026-07-19):** see **`SPINE_3D_PIPE_PLAN.md`** — Path Object model
> (spine + profile + shell + Bezier/Bevel/Sharp + caps/repetition from body/library), phased A→F.
> Original sketch below remains the Phase-A minimum.

Free 3D polyline (points anywhere), open or closed loop; **round-tube** thickness
(`dist_to_polyline(p) - radius`), per-point corner rounding via rounded joins; this is the
"cables/pipes/tendrils" sweep from FEATURE_IDEAS (2026-03-26) re-scoped to the bake-first
architecture (NOT `sdf_shared.wgsl` — the analytical path is dead; targets = CPU eval +
`volume_bake.wgsl`).

- Storage: reuse the clip-curve side-channel pattern for variable-length 3D points
  (a `CurveProfile`-like list but 3D — new struct, `PolyMesh`-precedent runtime/side-channel
  persistence, never new serialized fields on `HalfEdgeMesh`-class structs).
- **Own bounding derivation** (tube ≠ extruded box): AABB of points inflated by radius + pad;
  `provably_exterior` per-segment capsule test.
- Editor: 3D point placement (viewport click on workplane/surface + gizmo per point).
- Owner expansion (2026-07-19): per-point radius, Bezier joins, custom profile, shell, start/repeat/end
  caps from body list or library — **not v1**; see `SPINE_3D_PIPE_PLAN.md` phases B–F.

---

## Standing rules for all three
- FEATURE_RULES.md 4 questions answered per tool; one feature per prompt.
- Every eval change lands in CPU + bake WGSL (+ parity copies) same task; batch WGSL edits.
- New serializer inputs → into `hash_body_source`/`hash_prim_slice` SAME COMMIT (hash discipline).
- Bincode: enum variants append-only; no new serialized fields on existing structs without the
  field-trap-safe route.
- Gates: `cargo check -p app`, `cargo test -p core_math`, app suite `--test-threads=1`, graphics
  Naga suite when shaders touched.

## Session log
- 2026-07-13 — plan created from owner design session; seam boolean build launched (Tool 0).
- 2026-07-14 — **Tool 1 P1 SHIPPED** (feat/sparse-3a). Per-vertex corner round/chamfer + Shell + Rim on
  `SdfShape::CurveExtrude`.
  - **Finding:** `CurveVertex.bevel_radius` was DEAD (never consumed by any SDF — grep-confirmed; the
    documented "rounding already works" premise was wrong). So round AND chamfer are both newly wired.
  - **Corner mode = SIGN of `bevel_radius`** (bincode-inert, zero `.fl4` risk — all old files had
    `bevel_radius>=0` → read as round/sharp exactly): `>0` round, `<0` chamfer, `0` sharp. Implemented as
    a serialize-time **boundary_polygon expansion** in `curves.rs` (chamfer→2 tangent pts EXACT;
    round→tessellated fillet arc, `CORNER_ARC_SEGMENTS`; sharp→passthrough) shared by CPU eval AND the
    GPU clip-curve buffer → the WGSL winding SDF needs NO corner math (auto parity).
  - **GPU re-evaluates the polygon** (`volume_bake.wgsl::sdf_polygon_2d_gpu`, not a CPU-baked grid) — so
    the expansion feeds it via the (runtime, non-serialized) clip-curve buffer.
  - **Shell + Rim storage:** new `wall_t`/`rim` fields on the variant marked **`#[serde(skip)]`** (blob
    byte-identical → no field-trap) + persisted via a new in-blob `#[serde(default)] curve_params`
    side-channel on `ProjectFile` keyed by `prim_uid` (mirrors `voxel_grids`). `rim` sign = mode
    (+round/−chamfer), magnitude = radius. Shell = `max(d,-d-wall_t)` (**inward-only** — deviates from
    the plan's `abs(d)-wall_t` which grows outward and would violate the bounding contract). Rim reuses
    `bevel_2d_profiled` (CPU twin added to `curves.rs`; WGSL already had it).
  - **Bounding contract:** chamfer/round/shell/rim all cut INWARD only → authored-vertex
    `bounding_radius`/AABB still covers; test probe `corner_bevel_never_grows_outward` + rim/shell probes.
  - Gates green: `cargo check -p app`; `core_math` 465 (6 new curve tests); `graphics` 44 (Naga);
    `app --bins --test-threads=1` 153 (+`curve_params_round_trip`). UI in `properties_sdf_shape.rs`
    (corners combo+size, shell slider, rim slider+round/chamfer toggle) — all undoable via PropsContext.
  - **Not done (per scope):** per-vertex corner UI (P1 applies one treatment to all corners; per-point
    lands with the editor pass), reflex-corner ROUND is approximate (arc uses interior bisector).
- 2026-07-14 — **CREATION PATH SHIPPED** (feat/sparse-3a). `CurveExtrude` had NO creation entry (owner
  couldn't make one — absent from every SDF primitive menu). Now fully authorable.
  - **Default preset:** new `AzraApp::curve_preset()` (`creation.rs`) → square profile (half-size 0.4,
    sharp corners), `half_depth 0.4`, solid, no rim — a valid closed 4-vert winding that bakes to a
    visible surface. Kept OUT of `shape_preset()`/`SHAPE_PRESET_COUNT` (that index-cycled list feeds the
    footprint-drag create tool which scales via radius/half-extent fields a curve has none of).
  - **Menu entries added (all "Polyline", glyph `⌒` U+2312):** menubar **Add** menu (`menus.rs` ~131,
    default-drop + "Draw Polyline…" tool entry); empty-space **Add Primitive** context menu (~897,
    `do_add`); global **Add Object** search palette (~1132, +filter keywords polyline/curve); **shape-swap
    pie** (~533) and RMB **Change Shape** submenu (~840) tuple lists (swap any prim → curve).
  - **Draw tool (SHIPPED, both UX):** menu click = instant default drop; **Draw Polyline…** = click-to-
    place authoring. REUSES the clip-curve draw machinery — added `create_prim: bool` to
    `ClipCurveDrawState`; `start_polyline_draw()` + `polyline_create_commit()` in `clip_curve_tool.rs`
    branch off the SHARED `clip_curve_add_vertex`/`update_cursor`/overlay/Enter-commit/Esc-RMB-cancel
    (no forked draw logic). Commit re-expresses drawn UV points into the new prim's local XZ frame
    (rotation derived from the workplane normal — same as `add_primitive_immediate`) so the surface lands
    exactly where drawn.
  - **Canonical creation:** both paths go through `add_primitive_immediate()` → `add_prim_to_graph()`
    inside `transaction()` — undoable exactly like adding a sphere (verified: same code path, auto-enters
    edit mode + selects + arms gizmo + rebakes). Shape tag 9, baked by existing `volume_bake` → renders +
    pickable. P1 corner/shell/rim panel (`properties_sdf_shape.rs:244`) already handles the arm.
  - **Input discipline:** Draw Polyline is a menu-button action (no new keybind) → no InputSystem entry
    needed. Tool-exit: Enter commits, Esc/RMB cancel (shared clip-curve handlers, not knife RMB-confirm).
  - Gates green: `cargo check -p app`; `app --bins --test-threads=1` 153 pass. core_math untouched.
- 2026-07-14 — **PROFILE-POINT EDITOR SHIPPED** (feat/sparse-3a). Interactive point editing of a selected
  Polyline (`SdfShape::CurveExtrude`) prim, mirroring the poly vertex-edit system. New file
  `app/src/curve_point_edit.rs` (self-contained) + `ActiveTool::CurvePointTransform(CurvePointDrag)`.
  - **ACTIVATION RULE (no mode toggle):** handles show + are editable whenever EXACTLY ONE CurveExtrude
    prim is selected in Object mode — `curve_edit_prim_index()` (models `show_bevel_edges`). Deselect the
    prim → editor + selection auto-clear (`curve_pt_tick` owner-sync).
  - **SELECTION STORAGE:** `AzraApp.curve_pt_sel: BTreeSet<usize>` (profile-vertex indices) + `curve_pt_hover`
    + `curve_pt_owner` (prim_uid; clears sel on change). NOT in `UnifiedSelection` (those are scene objects) —
    same discipline as `poly_sel.selected_vertices`.
  - **6 behaviors — status:**
    1. **Show + single/multi-select + Delete — DONE.** Overlay `draw_curve_point_overlay` (handles colored
       by corner mode: cyan sharp / green round / orange chamfer; selected=white ring; hover=yellow). Pick =
       10px screen dist. Single click = replace; Shift+click = toggle; box-select = drag a rect (points inside).
       Delete guards ≥3 verts (refuse + toast). Route: press starts a SelectionBox (egui click/drag robustness,
       like poly), release interception in `input_viewport.rs` → `curve_pt_click`/`curve_pt_box_select`.
    2. **Grab points — DONE.** `G` → `curve_pt_start_transform(Grab)`; 1:1 cursor-follow via mouse→profile-plane
       UV mapping; X/Y lock UV.x/UV.y. Episode = one undo.
    3. **Shift-add + corner-toggle — DONE (add = click, not drag-follow).** Shift+click on an edge/empty inserts
       a point splitting the nearest edge at the cursor UV (atomic undo, selects it). Corner cycle: click the
       already-sole-selected point → Sharp→Round→Chamfer→Sharp (SIGN of `bevel_radius`, P1 encoding; new bevel
       gets a default radius = 0.3×shorter neighbour edge). *TODO: drag-follow while adding; a dedicated per-key
       corner cycle for multi-select.*
    4. **Rotate/Scale — DONE.** `R`/`S` about the selection centroid in the profile plane; screen-dx driven
       (uses `transform_settings` speeds); numeric buffer + axis lock supported.
    5. **Multi-select — DONE** (click/shift-click/box). 6. **Delete — DONE** (see 1).
  - **PRECEDENCE (documented):** with ANY point selected, `G/R/S` and `Delete` act on POINTS; with none
    selected they fall through to the normal PRIM gizmo/delete. The prim gizmo HANDLES always move the whole
    prim (press-box skipped when `gizmo_hovered`/`gizmo_armed`). Esc/RMB cancel a point transform (restore
    originals); Enter/LMB confirm (Tool Exit Convention).
  - **Undo/hash:** all edits go through `transaction()`/episode → `write_prim` → `mirror_prim_to_graph` → rebake.
    `hash_shape` (bake_utils.rs:624) already covers `profile.verts` (pos + bevel_radius) → moves/adds/deletes/
    corner-changes all retrigger the bake (verified pre-existing).
  - **Tests (in `curve_point_edit.rs`):** edge-split insert index, delete ≥3 guard, corner-cycle sign, centroid.
  - Gates green: `cargo check -p app`; `app --bins --test-threads=1` **157** pass (+4). core_math untouched
    (no serialized-shape change; corner mode still rides the `bevel_radius` sign, positions in `profile.verts`).
- 2026-07-14 — **POINT-EDITOR INTERACTION UPGRADE — TIER 1 SHIPPED** (feat/sparse-3a). Owner-approved
  interaction model layered onto `curve_point_edit.rs`. Reuses the existing modal-transform infra
  (`CurvePointDrag`/`ActiveTool::CurvePointTransform`), the global grid snap (`viewport.snap_grid` +
  `smart_snap_on` + `crate::snap_to_grid` pattern), episode undo, and the SelectionBox press/release
  routing — no new drag tool type.
  - **Direct drag (T1.1/2/3):** LMB press now hit-tests via `curve_pt_press` — on a point → select+grab
    (episode); on an edge → segment drag (both endpoints); shift+edge → insert a point at the cursor UV
    and drag-follow it in ONE episode. Empty press still falls back to the selection box (box-select /
    click-deselect). Shift+click on a point = toggle-select. Per-frame drive in `input_viewport` drag
    chain (gated `d.direct`); keyboard G/R/S modal untouched (skips when `d.direct`). Release commits, or
    drops an unmoved click's empty episode (no zero-delta undo). `CurvePointDrag` gained
    `direct/press_kind/moved/slide_dirs/corner_prev` (app_types).
  - **RMB delete (T1.4):** `curve_pt_rmb_delete` in the RMB chain — deletes the point under the cursor
    (≥3 guard); misses fall through to the normal RMB context/select.
  - **Snapping (T1.5):** in `curve_pt_update_transform(ctrl, shift)`. Ctrl = 45°/H-V-45 (single point →
    "corner angle to 45° from prev vert"; edge/multi → H/V/45 delta). Ctrl+Shift = edge-slide along a
    drag-start-captured adjacent edge (`curve_snap_anchors`). Grid = quantize UV to `snap_grid` when
    `smart_snap_on`. All use the drag-start snapshot, not live neighbours.
  - **Edit keys (T1.6):** `A` select-all, `Ctrl+X` dissolve, `Ctrl+D` subdivide-selected-segments (midpoint
    insert), `Del` delete — registered as `ACT_CURVE_SELECT_ALL/DISSOLVE/SUBDIVIDE` in `keybindings.rs`
    (`context: "Curve point editor"`), pre-evaluated + gated on `curve_edit_prim_index().is_some()`
    (Input System Discipline). `A` takes precedence over global scene select-all when the editor is active.
    Box-select + shift-multi reuse the existing SelectionBox path.
  - **Scroll cycle (T1.7):** scroll over a hovered point/selection cycles corner TYPE Sharp→Round→Chamfer
    (`curve_pt_scroll_cycle`; consumes the scroll so the camera doesn't zoom; sharp→round bumps a default
    radius). Computed before the `do_zoom` closure to avoid a borrow conflict.
  - **Undo/hash:** every edit routes through `transaction()`/episode → `write_prim` → rebake; `hash_shape`
    already covers `profile.verts` → all edits retrigger the bake.
  - **Tests (+7, all pure logic in `curve_point_edit.rs`):** corner-mode cycle fwd/back, H/V/45 snap, 45°
    corner-angle snap, edge-slide projection, snap-anchor capture (single/segment/non-adjacent), subdivide
    insert plan. Gates green: `cargo check -p app`; `app --bins --test-threads=1` **164** pass. core_math
    untouched (no `curves.rs` change — corner mode still rides `bevel_radius` sign).
  - **DEFERRED:** Tier 2 (B live-bevel modal + MMB→diamond handle, model on the blend-k diamond) and Tier 3
    (2-adjacent-point floating toolbar: Flatten + Copy-angle) — TODO, out of this session's budget.
    P2 open/closed profiles remain out of scope (CurveExtrude is closed-only).
- 2026-07-14 — **POINT-EDITOR TIER 2 + TIER 3 SHIPPED** (feat/sparse-3a). Layered onto `curve_point_edit.rs`.
  - **Tier 2 — bevel size handle (B).** New `ActiveTool::CurveBevelTransform(CurveBevelDrag)` (app_types).
    B (gated STRICTLY behind `curve_edit_prim_index().is_some()` && `!curve_pt_sel.is_empty()`, evaluated
    BEFORE the box/cylinder/poly `ACT_BEVEL_TOGGLE` handler in `input_keyboard.rs` via a `curve_bevel_taken`
    consume-flag, so box/cyl/poly bevel is untouched when the editor is inactive) → opens ONE undo episode +
    a **live AUTO-SIZE modal**: each frame the |bevel_radius| of ALL selected points = cursor↔anchor(centroid)
    UV distance (no grab). **LMB** = confirm, **MMB** = hand off to the diamond fine-tune (phase 1: horizontal
    drag delta), **RMB/Esc** = cancel (restore pre-B radii; Esc via `drop_tool`→`cancel_operation`). Corner
    TYPE (round/chamfer) fixed for the gesture (`sign`, from the first point's polarity; sharp→round default);
    scroll-cycle-type (Tier 1) still works alongside. Label `Bevel 0.30` / `Chamfer 0.30`.
  - **Diamond reuse = PARALLEL TOOL + extracted shared draw** (NOT `BlendKTarget::CurveVert`). Rationale:
    `BlendKDrag`'s target-resolution / click-cycle / apply logic is 100% prim/body-boolean-op specific
    (Seam/Extrude/BlendK across a 150-line resolve + 115-line commit); a `CurveVert` variant would inject
    curve branches into five unrelated match arms and risk the box/cyl bevel handles (single-responsibility
    violation). Instead the diamond VISUALS+label were extracted from `draw_blend_k_handle` into a shared
    `AzraApp::draw_diamond_handle(painter, sp, rgb, label, hovered, dragging)` (`draw_viewport_overlays.rs`)
    that BOTH the blend-k handle and the curve-bevel handle call — the diamond is genuinely reused, not forked.
  - **Tier 3 — edge tools floating toolbar.** When EXACTLY 2 ADJACENT selected points form a segment
    (`curve_selected_edge`, incl. the closed wrap), a small egui toolbar (`ui.interact` buttons, like the
    bevel-reset button) is drawn at the segment MIDPOINT (world_to_screen of the mid-UV), 14px above it.
    **Flatten** → snap the edge to H or V (whichever it's closest to), pivoting on the midpoint so both points
    move symmetrically (`curve_flatten_edge`; transaction). **Copy angle** → arms a pick mode
    (`curve_copy_angle_arm: Option<(usize,usize)>` on AzraApp); the next LMB on any other segment
    (`input_viewport` copy-angle block, consumes the click) sets the selected edge's slope to that segment's,
    keeping the selected edge's own length + midpoint and **preserving endpoint order** (reference direction
    flipped 180° when `dot(ref,cur) < 0`, `curve_copy_angle`; transaction). Esc/RMB cancels the pick.
  - **InputSystem:** `ACT_CURVE_BEVEL` = simple(B), context "Curve point editor" (keybindings.rs). Flatten /
    Copy-angle are toolbar buttons (no keybind). Undo: bevel = episode (one entry/gesture); Flatten + Copy-angle
    = `transaction()`. All route write_prim → rebake; `hash_shape` already covers `profile.verts` (pos +
    bevel_radius) so every edit retriggers the bake.
  - **Tests (+5 pure logic in `curve_point_edit.rs`):** bevel distance→radius floor, flatten H/V choice +
    symmetric-pivot midpoint invariance, copy-angle length/midpoint preservation, copy-angle 180° order-flip.
    Gates green: `cargo check -p app`; `app --bins --test-threads=1` **169** pass (+5). core_math untouched
    (no `curves.rs` / serialized-shape change — bevel still rides the `bevel_radius` sign+magnitude).
  - **Known minor wart:** clicking a toolbar button also starts (then click-cancels) a selection box that frame,
    so the point selection clears right after the action runs; the action itself applies correctly first, and
    Copy-angle keeps working because the pick uses the stored `(i,j)`, not the live selection.
- 2026-07-14 — **P2 OPEN PROFILE + WALL/TUBE THICKNESS SHIPPED** (feat/sparse-3a). `CurveExtrude` can now be
  an OPEN polyline (no winding fill) offset into a flat ribbon/WALL or a round TUBE, alongside the P1 closed
  filled polygon (unchanged default).
  - **Storage:** 3 new `#[serde(skip)]` fields on the variant — `open: bool`, `thickness: f32`, `tube: bool`
    (same save-safe route as P1's `wall_t`/`rim`). Persisted via the existing `curve_params` side-channel:
    `CurveParamRecord` APPENDED `open`/`thickness`/`tube` (each `#[serde(default)]`). Pre-P1 files have EMPTY
    `curve_params` → fully safe; only P1-era (2026-07-14) experimental scenes that authored shell/rim would
    need a re-save (documented in project.rs). Round-trip test extended with an open-tube record.
  - **Eval (both paths, parity):** open = `dist_to_open_polyline_2d`/`dist_to_open_polyline_gpu` (unsigned
    open-path distance, NO closing wrap). WALL = `d_path − thickness/2` extruded ±half_depth (optional rim on
    the cap); TUBE = `sqrt(inplane² + axis²) − thickness/2` (round cross-section + hemispherical end-caps;
    half_depth ignored). Round convex corners + round ends come free from the offset (corner `bevel_radius`
    ignored in open mode). CPU `sdf_shapes.rs` twin matches WGSL `volume_bake.wgsl` st==9.
  - **GPU channel (no new hot slots — [12..15] were full):** thickness rides the CurveExtrude clip record's
    FREE mode slot (`clip[+0]`, which the cutter loop skips for st==9): `0.0` = closed, else SIGNED thickness
    (sign<0 ⇒ tube). Open mode serializes the RAW path points (open, no boundary expansion); closed keeps the
    boundary_polygon winding. Self-contained in the record the st==9 branch owns.
  - **BOUNDING CONTRACT (the load-bearing risk):** open ribbon/tube grows OUTWARD from the path by
    thickness/2, so ALL FOUR bounding sites (primitives.rs `bounding_radius`+`local_half_extents`, serialize.rs
    ×2) expand `max_r += thickness/2` and set the tube's Y extent to the tube radius. Prevents the sparse cache
    from dropping bricks → holes (P0-B1 lesson). 2 new core_math tests assert the outward reach + no-fill.
  - **Hash:** `bake_utils.rs::hash_shape` AND `render/mesh_cache.rs` both cover open/thickness/tube → mode
    changes retrigger bake + invalidate the cached mesh.
  - **UI (`properties_sdf_shape.rs`):** Closed/Open toggle; when Open: Wall/Tube toggle + a thickness slider
    ("Wall thickness" / "Tube diameter"); Depth disabled for tubes; Corners+Shell hidden in open mode; Rim kept
    for closed + open-wall. All undoable via PropsContext.
  - **NOT done (P2b, next):** the mockup's open/close VIEWPORT dropdown + shift-click-first-point-to-close in
    the point editor (this shipped the SHAPE + properties UI; the in-viewport open/close UX is the follow-up).
  - Gates green: `core_math` **461** (+2 open tests), `graphics` **41** (Naga), `app --bins --test-threads=1`
    **169** (curve_params_round_trip extended).
