# PAINT_SELECT_AUDIT.md — our drag/paint selection vs Blender's (2026-07-18)

Owner: "double check code of blender for paint select, i think what we have is a mess, i feel it
create lot of bug." **Verdict: the owner is right.** The mess is ONE architectural choice — CPU
per-element occlusion raycasts — and every bug/hack of the last week hangs off it.

## 1. Blender's actual architecture (verified from `view3d_select.cc`, main branch)

- **Occlusion is a GPU ID buffer, never CPU rays.** `use_zbuf = !XRAY_FLAG_ENABLED(v3d)`; when on,
  `DRW_select_buffer_bitmap_from_poly/circle/rect()` renders element IDs to an offscreen buffer once
  and yields a **visibility bitmap**; selection is `BLI_BITMAP_TEST_BOOL(select_bitmap, index)` per
  element (`edbm_backbuf_check_and_select_verts/edges/faces`). There is **zero per-element
  ray-triangle testing anywhere in the file.**
- **X-ray path = plain 2D tests, no occlusion:** `mesh_foreachScreenVert/Edge/Face` projection
  callbacks with simple geometry (point-in-lasso / `edge_inside_circle` = segment-to-center
  distance / face-center test). No sampling along edges; a single closed-form test per element.
- **The select-buffer cache lives for the whole modal gesture** (`editselect_buf_cache_*` via
  `wm_userdata`) — rendered once, reused across mouse-moves, freed at gesture end.
- Tools are predicates over ONE mechanism: box/lasso/circle differ only in the 2D region test —
  same as our `poly_screen_test` intent, which is the one part of our design that matches.

## 2. Our implementation (`poly_selection.rs` 1546 lines + `input_viewport.rs` choreography)

| # | Ours | Cost / bug it caused |
|---|---|---|
| 1 | `poly_front_surface_depth` = O(faces) ray-triangle scan **per sample** (`poly_screen_test`, per-edge up to 48 samples × per stroke dozens of probes) | O(samples × faces) per frame — the **busy-spinner**; scales quadratically with mesh size |
| 2 | Depth-ray misses at silhouettes → added `faces_camera` back-face heuristic (tolerance −0.05) | "paint selects hidden geometry" class; tolerance is a magic constant |
| 3 | Depth bias `cam.distance * 0.02` | second magic constant; silhouette elements flicker in/out of selectability |
| 4 | Edge test = sample every 6px (≤48) each with its own occlusion ray | the "drag didn't pick all edges" fix that multiplied cost |
| 5 | Face test = boundary sampling + `point_in_polygon` probes | the "ring-touch selects" fix — more per-frame geometry |
| 6 | 4 overlapping tool states (paint / B-box / C-circle / RMB-region) + click fallbacks + `poly_click_done` flags in `input_viewport.rs` (13 dispatch sites) | the release-choreography bugs we kept patching |
| 7 | Separate hover `rebuild_pick_cache` with its own projection + occlusion | two parallel visibility systems that can disagree |

Blender needs none of #1–#5: the ID buffer IS visibility.

## 3. The fix that removes the class (recommended): GPU element-ID selection

AZRA already renders node IDs to a picking buffer (`raster_pick.rs`, `PickingPipeline`, node_id via
`material_idx`, async readback). Extend that to poly SUB-ELEMENTS, Blender-style:

- **Pass:** when a poly edit gesture starts, render the active body's elements to the pick target:
  faces as index-colored triangles; edges/verts as GPU lines/points with their indices (or derive
  edge/vert visibility from the face bitmap + depth like Blender's `select_buffer_bitmap` variants).
- **Cache per gesture** (Blender's model): render once at gesture start (+ on camera change),
  readback ONCE async (mpsc, Frame Budget cap), keep the bitmap for every mouse-move of the stroke.
- **Selection test becomes:** 2D region predicate (shape) AND bitmap[element] (visibility). Deletes
  `poly_front_surface_depth` occlusion path, `faces_camera`, both magic constants, edge/face
  sampling loops. X-ray mode = skip the bitmap, exactly Blender.
- Hover pick reads the same buffer (one visibility system).
- Frame Ordering: buffer creation in poll, bind groups + dispatch in viewport_render — the picking
  pass already complies.

**Phasing:** P1 face-ID buffer + face/edge/vert bitmaps + route `poly_screen_test` through it (kill
#1–#5). P2 unify the 4 tool states into one gesture struct (kill #6). P3 hover through the same
buffer (kill #7).

## 4. Interim done already
The CPU path stays as the X-ray path (it IS Blender's X-ray model) — front_only just stops using rays.
