# PHASE 3 status — Bevel vs GRS next work

**Branch:** `feat/poly-phase0` · **Date:** 2026-07-19

## Bevel M_ADJ — largely DONE (doc was stale)

`BEVEL.md` §8/§9 still said “rounded N-way / k=2 needs M_ADJ.” Live `bevel_op.rs` already has:

- `fill_corner_grid_madj` for rounded shared corners  
- Rounded cube corner / seg2 / seg3 oracles  
- Mixed k=2 / k=3 deg-4 oracles  
- Span-corner rounding  
- N-way apex domes  

**67 bevel-related tests green.** Remaining **narrow** flat-clamp cases only:

- Bevel-on-beveled **k=1** at degree>3 (partial corner) forces global flat for safety  
- Mixed corners with **two consecutive unbeveled** fan edges (rim inserts original V)

Those need careful geometry work (not a weekend “unclamp”). Treat as **bevel backlog**, not “M_ADJ missing.”

## This phase’s real work — GRS polish after D6

| Fix | Problem |
|-----|---------|
| **Ctrl snap** | `snap_active: false` fossil — Ctrl did nothing on poly G/R/S |
| **Rotate angle snap** | Ctrl/Shift + snap_rotation_deg |
| **Scale snap** | Ctrl → 0.1 factor increments |
| **Mirror co-fix** | Pairing + reflect used local verts against world plane → skew on rotated bodies |

Full GRS S3 (delete PolyTransform, one gizmo path) still deferred (HUD risk).

## Manual verify

1. Poly edit G drag + **Ctrl** → grid snap  
2. Poly R drag + **Ctrl** → angle snaps (e.g. 15°)  
3. Symmetry on + body rotated → mirror partner tracks correctly while G  

## 2026-07-19 follow-up — viewport-driven poly G/R/S (choppy fix)

**Problem:** `poly_update_transform` was only called from `handle_keyboard`. Mid-drag frames
where `hover_pos()` is missing on the keyboard path (or keyboard is not re-entered) left the
mesh stuck → choppy G/R/S (same class as GRS §1 `start_modal_transform` fossil).

**Fix:**
- Primary-drag arm in `input_viewport` updates poly transform while LMB held (Alt free-move)
- Hover arm updates poly transform when **no** LMB (keyboard-initiated G/R/S)
- Pivot reuses `poly_selection_center()` (same as gizmo draw) instead of a second hand-rolled centroid

Full S3 (delete `ActiveTool::PolyTransform`) still deferred.

## 2026-07-19 follow-up — undo holes + fill refuse

**WORK_ORDER #8:** typing into Pos/Rot/Scl (or material) sliders fired `changed()` without
`drag_started()` → no episode → no undo. Fix: `transaction_snapshot_only` when
`pending_episode` is none before writing.

**Fill face:** always opened `transaction()` even on collinear / create_face refuse → ate Ctrl+Z.
Fix: dry-run ring + create_face on a clone; transaction only on success (same pattern as circle/inset).

## 2026-07-19 follow-up — Bridge undo honesty

Legacy Bridge always opened `transaction("Bridge")` even when selection was wrong / create_face
failed. Edge-loop path could also push an empty undo if the kernel returned no faces.

Fix: dry-run `bridge_edge_selection` / `try_legacy_bridge` on a mesh clone; toast + return with
no history entry on refuse; transaction only when the dry-run succeeds.

## 2026-07-19 follow-up — hint honesty (WORK_ORDER #22 class)

- Poly Face idle row said **G: Push/Pull** (SDF Face tool) — real G arms Move gizmo. Fixed → **G: Move**.
- Added modal hint arms for **Vertex/Edge Slide**, **Paint select**, **Region select** (were falling
  through to idle rows that advertised the wrong keys).

## 2026-07-19 follow-up — G/R/S + modal no-op undo honesty

| Path | Problem | Fix |
|------|---------|-----|
| Poly edit G/R/S confirm | Enter without moving committed empty episode | `poly_confirm_transform` compares verts to originals → `cancel_transform` |
| Extrude / Inset / Alt+S / loop cut zero | Zero-offset confirm still undid | Cancel paths (already shipped earlier same day) |
| Knife empty / nothing-to-cut | Was already refuse-before-tx | No change needed |
| Object/SDF modal G/R/S | `confirm_modal_transform` always `commit_episode` | No-op detect → `cancel_modal_transform` |
| Object/SDF gizmo release | `finish_gizmo_drag` always committed | No-op detect → `cancel_gizmo_drag` |

Shift-duplicate is **never** a no-op (geometry was created). Poly edit gizmo already routes through
`poly_start_transform` / `poly_confirm_transform`.

## 2026-07-19 follow-up — curve G/R/S + recalculate normals

| Path | Problem | Fix |
|------|---------|-----|
| Curve profile G/R/S confirm | Keyboard Enter always `commit_episode` (direct LMB release already gated on `moved`) | Compare UV to `original`; cancel unless `press_kind==3` (add-point created geometry) |
| Shift+N Recalculate Normals | `transaction()` always pushed even when active node was not a poly mesh | Refuse-before-transaction + toast |

## 2026-07-19 follow-up — curve bevel / flatten / copy-angle no-op

| Path | Problem | Fix |
|------|---------|-----|
| Curve B confirm | Always `commit_episode` even when applied radius×sign matches all originals | Cancel episode (no undo) |
| Flatten edge | Already H/V-aligned still opened transaction | Compare result UV; toast + return |
| Copy angle | Same slope / degenerate ref still opened transaction | Compare result UV; toast + return |

## 2026-07-19 follow-up — loop cut one-shot + rename same-name

| Path | Problem | Fix |
|------|---------|-----|
| Keyboard one-shot loop cut | `poly_loop_cut_commit` always `transaction()` even when multi-cut inserts nothing | Dry-run clone first; toast + return on refuse |
| Outliner rename (poly/body/light/mat/prim) | Double-click rename + Enter without edit pushed empty undo | Skip transaction when name unchanged |

## 2026-07-19 follow-up — material reassign + hide/unhide

| Path | Problem | Fix |
|------|---------|-----|
| Assign Material (poly faces/body/SDF prims) | Re-picking the same material always `transaction()` | Pre-check material_ids; skip if already assigned |
| H Hide Selected | Selection already hidden still pushed undo | Only open tx when something is still visible |
| Alt+H Unhide All | Everything visible still pushed undo | Only open tx when something is hidden |

## 2026-07-19 follow-up — Set* commands / boolean / reset no-op

| Path | Problem | Fix |
|------|---------|-----|
| SetVisible / SetLocked | Same state still `transaction()` | Skip when already at target |
| SetPosition / Rot / Scale | Same value still pushed undo | Epsilon compare before tx |
| set_prims_boolean_op | Re-pick same pie op pushed undo | Filter unchanged ops |
| Body boolean / blend profile UI | Click active item opened tx | Skip when `is_current` |
| Reset pos/rot/scl/elong/transform | Already identity still pushed undo | Needs-change gate before tx |

## 2026-07-19 follow-up — reset bevel / make unique / clear map

| Path | Problem | Fix |
|------|---------|-----|
| Reset corner bevel (Z / overlay) | Already-default still pushed undo | Shared `reset_prim_corner_bevel_undoable` |
| Reset blend-k (MMB bool edge) | Already ~0 still pushed undo | `reset_prim_blend_k_undoable` |
| Context Make Unique | Already unique still snapshot | Gate on `instance_master_id` |
| Context Change Boolean Op | Parallel path to pie (no filter) | Route through `set_prims_boolean_op` |
| Clear material map channel | Already clear still pushed undo | Skip when map not set |

## 2026-07-19 follow-up — unique / DNA / sculpt / solo restore

| Path | Problem | Fix |
|------|---------|-----|
| make_instance_unique API | Already unique still transaction | Gate on master id |
| Clear DNA | Empty DNA still snapshot | Skip when inactive + age 0 |
| Clear Sculpt Layer | Empty grid still snapshot | Skip when cells empty |
| Alt-solo restore prims/lights | Single-item body restore no-op undo | Only restore if something hidden |

## 2026-07-19 follow-up — reorder / node solo / reparent fossil / nurbs

| Path | Problem | Fix |
|------|---------|-----|
| Hierarchy Move Up/Down/First/Last | Already at edge still transaction | `reorder_would_change` gate |
| Node Alt-solo restore | Single sibling restore empty undo | Only if something hidden |
| Deferred reparent | Pure mark-dirty fossil opened undo | Drop empty transaction |
| NURBS move / weight | Zero delta or floor clamp no-op | Skip when nothing would change |

## 2026-07-19 follow-up — smart mirror / clip / empty delete

| Path | Problem | Fix |
|------|---------|-----|
| Smart Mirror apply | Empty sel / re-apply same plane still tx | Plan writes; skip if none change |
| Mirror Instances | Empty/locked-only sel still tx | Refuse-before-transaction |
| Clip Curve commit | Zero resolved clips still tx | Skip when clips empty |
| Delete selected prims | Empty resolved indices still tx | Early return |

## 2026-07-19 follow-up — adjust-panel settle only when dirty

| Path | Problem | Fix |
|------|---------|-----|
| Adjust Create / Inset / Circle / To Sphere | Slider release always `transaction_snapshot_only` even with no param change | `needs_settle` flag; settle only after a live change |

## 2026-07-19 follow-up — bool pie / light dup / compositor reselect

| Path | Problem | Fix |
|------|---------|-----|
| X bool pie (prims) | Toast + path when all ops already match | `set_prims_boolean_op` returns bool; toast only on change |
| X bool pie (bodies) | Re-pick same op still transaction | Filter `next == cur` |
| Duplicate light | Missing id still opened empty tx | Early return if light gone |
| Compositor mode combos | Re-click active mode pushed undo | Guard `!= current` before snapshot |

## 2026-07-19 follow-up — convert-to-poly refuse + double-tx

| Path | Problem | Fix |
|------|---------|-----|
| convert_sdf/imported/nurbs_to_poly | Early return *inside* transaction left empty undo | Prepare mesh first; transaction only on success |
| Outliner Convert→Poly | Outer `Convert node` tx around convert_* | Drop outer wrap (convert owns history) |
| lab_make_prim_static | Already-unlinked still transaction | Skip when `source_poly` is None |

## 2026-07-19 follow-up — flip empty / instance uid / body drop noop

| Path | Problem | Fix |
|------|---------|-----|
| Flip Mirror | Empty body still opened transaction | Skip when no prims in body |
| Create Instance | Burned `next_prim_id` outside tx | Allocate uid inside transaction |
| Body outliner reorder drop | Drop on current slot still tx | `slot_is_noop` for single body/node |

## 2026-07-19 follow-up — body mirror + voxel material undo spam

| Path | Problem | Fix |
|------|---------|-----|
| Body mirror Set from Workplane | Same origin still snapshot | Only when center differs |
| Body mirror writeback | Snapshot when already equal | Compare flags/sign/center first |
| Voxel material sliders | `transaction()` every drag frame | `begin_episode` + live write; settle on release |
| Voxel clay / reset clay | Already-at-target still tx | Compare before open |

## 2026-07-19 follow-up — AI transform + NURBS knot refuse

| Path | Problem | Fix |
|------|---------|-----|
| execute_and_log Set P/R/S | Always snapshot even same value | Compare first; snapshot only when writing |
| nurbs_insert_knot U/V | Non-NURBS active node still tx | Refuse-before-transaction + toast |
| Agent material on empty sel resolve | Empty prim indices still tx | Guard `!sel.is_empty()` |
| DNA input combo re-click | Assign same option as change | Only assign when `!=` |

## 2026-07-19 follow-up — light props episode + billboard uid

| Path | Problem | Fix |
|------|---------|-----|
| Light name / group text | `gained_focus` → empty undo on click-release | `begin_episode` only on first `changed()` |
| Light sliders / color | `drag_started` → empty undo if no move | Same: episode on real `changed()` |
| Volumetric / RT toggles | `begin_episode` for one-shot checkbox | `transaction_snapshot_only` |
| Create Billboard drop | `next_prim_id += 1` then used as uid (collision) | `let uid = next; next += 1; with_uid(..., uid)` |

## 2026-07-19 follow-up — compositor / poly / NURBS / SDF slider episodes

| Path | Problem | Fix |
|------|---------|-----|
| Compositor effect sliders | `drag_started` → empty undo | Episode on first `changed()` |
| Compositor mask sliders | `mask_undo` on drag_started alone | Episode when `mask_dirty` only |
| Poly transform / material | Same drag_started empty undo | Episode on `changed()` |
| NURBS transform / tess / mat | `nurbs_drag_started` → empty episode | Episode on `changed()` |
| SDF seam / extrude / blend / edge bevel | Same | Episode on `changed()` |

## 2026-07-19 follow-up — body scope / ctx resize / NURBS CV / sym axis

| Path | Problem | Fix |
|------|---------|-----|
| Enter body scope (click + menu) | Re-enter same scope still tx | Skip when active+sole-selected already |
| Context-menu shape sliders | `drag_started` empty; typed path no undo | Episode on `shape_edit` + `changed` |
| NURBS CV drag start | `begin_episode` then early return | Resolve CV originals first |
| Set symmetry axis | Re-pick same axis/center still tx | Compare normal+origin first |

## 2026-07-19 follow-up — viewport drag no-op release + combo re-click

| Path | Problem | Fix |
|------|---------|-----|
| Face push/pull release | Click without drag still commit_episode | Cancel when `accumulated_delta` ~0 |
| SDF bevel / falloff / bool-edge release | Same empty undo class | Compare fields to drag-start; cancel if equal |
| Shader type / mat quality / DNA blend | Re-click active option → empty undo | Guard `!=` before change |
| Bevel axis / shear drive / sculpt op / mask / layer blend | Same | Guard `!=` before change |

## 2026-07-19 follow-up — blend-k / NURBS CV / poly start refuse

| Path | Problem | Fix |
|------|---------|-----|
| Blend-k diamond click (Extrude) | Restore-only still commit_episode | Cancel unless discrete cycle |
| Blend-k diamond drag no net change | Always commit | Cancel when k/w == start |
| NURBS CV release | Idle without checking move | `nurbs_confirm_cv_drag` cancel if unmoved |
| Poly extrude/inset start | Episode before non-poly check | Refuse-before-episode + toast |
| Blend profile / falloff axis re-click | Empty undo | Guard `!=` / `!selected` |

## 2026-07-19 follow-up — sculpt stroke / layer / material projection

| Path | Problem | Fix |
|------|---------|-----|
| SDF sculpt brush drag | `begin_episode` on drag_started miss | Episode only after surface hit |
| Sculpt layer eye toggle | `write_prim` with no history | Route through pctx (snapshot + mirror) |
| Sculpt layer re-select active | Always marked changed | Guard `!is_active` / `!= Some(ni)` |
| Material all-maps projection | Re-pick same projection still snapshot | Skip when already uniform |
| Poly Shift+D duplicate | Non-poly still opened transaction | Refuse-before-tx + toast |
