# AZRA — Viewport & Mode Model (canonical)
**Written: 2026-06-19.** This is the confirmed interaction + render model. It **supersedes the "3 global rooms" framing** previously written into CLAUDE.md (Room Display Invariant) and MASTER_WORK_LIST. CLAUDE.md and MASTER_WORK_LIST will be re-aligned to this doc.

Status of decisions below: **CONFIRMED with the project owner** (2026-06-19 design session) unless marked OPEN.

---

## The one-sentence model

> **The app lives in Object Mode — one unified, fully-rasterized viewport showing every object together. You edit an object by selecting it and pressing Tab, which drops you into that object's own type-aware Edit Mode. Edit is per-object (like Blender).**

There are no global "rooms" you switch between. There is **one home (Object Mode)** and **per-object edit contexts** you enter and leave.

---

## Representation stack (unchanged — this is the *data*, the model below is the *navigation*)

```
Layer 1  SDF body        parametric, boolean        edited in: SDF Edit Mode
Layer 2  Voxel density   volumetric sculpt          edited in: Sculpt (voxel) Edit Mode
Layer 3  Polygon mesh    vertices/edges/faces        edited in: Poly Edit Mode
                              │  (every layer bakes to)
Layer 0  Triangles  →  the Object-Mode unified raster + final render
```

**Triangles are the render currency.** Everything bakes to triangles to be shown together in Object Mode.

---

## Object Mode (the home — the "overall viewport")

- **The app opens here.** It shows **all objects, all rasterized, together**: one G-buffer, one depth, one shading pass.
  - **voxel** → its Surface Nets mesh (already produced today)
  - **poly** → native triangles (already)
  - **SDF** → a **cached triangle mesh baked from its volume** (DC / surface-nets extraction; the machinery exists — `dispatch_grid_dc_async` — currently gated off in volume-only mode and to be revived for this view). Re-baked **only when the SDF is edited**, so Object Mode pays no per-frame SDF cost.
- **No fused render, by construction** — everything is triangles, so the SDF-raymarch ↔ voxel-raster collision that caused the crash (fixed structurally in `4f1a30a`) simply cannot occur here.
- Tools = object-level: select / move / rotate / scale / add / delete / duplicate. The outliner here is the **scene object list** (see Per-Mode Outliner).

**CONFIRMED trade-off (superseded 2026-07-14):** the original decision kept a smooth volume raymarch for SDF Edit Mode. That path was DELETED 2026-07-14 — **SDF now renders from its baked DC mesh (triangles) in EVERY mode, Edit included**. The bake is partial-region + debounced, so editing stays live. *(Original owner quote — "you have the green light, this is the only solution" — applied to Object Mode baking; Edit Mode later converged to the same representation.)*

---

## Edit Mode (per-object, type-aware)

- **Enter:** select an object → **Tab** (or double-click). The app reads the object's type and opens the matching editor.
- **Per-object:** you edit the *selected* object only. **Two voxel spheres = two separate edit contexts** — you sculpt whichever is selected. There is no shared "sculpt room"; the sculpt environment operates on the active object. *(Owner confirmed: edit-by-object, like Blender.)*
- **The editors:**
  | Object type | Edit Mode shows | Render technique |
  |---|---|---|
  | **Voxel** | the sculpt environment (brushes, the live sculpted surface) | raster mesh ≤512³ / atlas raymarch >512³ (already built) |
  | **SDF** | the body's prims + boolean structure; prim gizmos | baked DC mesh (triangles, same as Object Mode — analytical raymarch DELETED 2026-07-14) |
  | **Poly** | the mesh with vertex/edge/face sub-modes | native raster |
- **Exit:** Tab / Esc → back to Object Mode. On exit the object's Object-Mode representation is refreshed (e.g. an edited SDF re-bakes its display mesh; an edited voxel re-meshes).

**Data-model implication (to build):** `self.voxel` becomes the **active edit target**, not a singleton — voxel objects persist as scene nodes; entering Edit on a selected voxel loads it as the working set, exiting flushes it back. Same pattern SDF already uses (`active_body_nid`).

---

## The Mode indicator (primary navigation) — replaces the confusing "workspace"

The current UI has **three competing concepts** — *Workspace* (panel-layout tabs, Ctrl+1–4), *WorkMode* (Object/Sculpt/Vertex/Edge/Face), *ModelingContext* (object type). The user can't tell "where am I?" because three systems each answer it. **Collapse to ONE visible thing: the Mode breadcrumb.**

```
OBJECT MODE (home):
│ ◉ Object ▾          [move][rotate][scale][add]        │   everything rasterized

EDIT a voxel (select → Tab):
│ ◉ Object  ›  🔨 Sculpt · "Sphere.001"        [Tab ↩]  │   brush tools, this object

EDIT an SDF body:
│ ◉ Object  ›  ✎ SDF · "Body.002"              [Tab ↩]  │   prim tools, volume raymarch

EDIT a poly mesh:
│ ◉ Object  ›  ✎ Mesh · "Mesh.003"     [V][E][F]        │   vertex/edge/face
```

- The breadcrumb **names the mode AND the object** → "where am I?" is always answered, and two spheres are never ambiguous.
- The **"Object" segment is the way home** (click, or Tab/Esc).
- **The mode drives the panel layout automatically** (Sculpt → brush panel; SDF → prim/properties; Object → object tools). No separate workspace to manage.
- Optional polish: subtle **per-mode accent color** (Object neutral, Sculpt clay/orange, SDF blue) on the breadcrumb + a thin viewport border tint, for peripheral awareness.

**"Workspace" fate — CONFIRMED option A:** *demote* it. The Mode breadcrumb is primary; mode auto-applies a sensible default layout. Saved custom layouts survive as a small optional **"Layout ▾"** menu (not loud tabs that masquerade as modes). Nothing is lost; it just stops competing with the mode indicator.

---

## Per-Mode Outliner

Each mode is a different workflow, so the outliner *content* is per-mode — but the *machinery* (draw loop, selection, drag-and-drop, rename, deferred-mutation) is mode-independent and must stay single. Today's outliner is **mode-blind**: `draw_right_panel` (`outliner_draw.rs:10`) always walks the scene graph and `draw_outliner_sdf_body` unconditionally expands every SDF body's flat prim array (`outliner.rs:36`) with ∪/−/∩ boolean badges + modifier letters — concepts that are meaningless in Object mode and wrong in Sculpt/Poly.

### What each mode shows
- **Object Mode** — a flat list of **top-level objects, all types as peers**: SDF `▢SDF`, voxel `🧊VOX·256³`, poly `POLY`, nurbs `NURBS`, mesh `MESH`, lights, cameras. Groups expand (pure scene structure). **No SDF prims, no boolean badges, no sub-object anything** — a body is one opaque object. *(Headline fix — the assembly view.)* Actions: select / rename / visibility / lock / reorder-into-Groups / duplicate / convert / delete. Double-click or Tab → enter that object's Edit Mode.
- **SDF Edit** (`Object › SDF · 'Body'`) — the active body's **prim + boolean tree only** (shape icons, ∪/−/∩ badges, modifier letters, nested sub-bodies). Essentially today's outliner, scoped to one body. Actions: reorder prims, change boolean op, group into sub-body (**same-body only**), delete/dup prim.
- **Sculpt Edit** (`Object › Sculpt · 'Sphere'`) — the voxel object's structure: header (name, resolution, live tri/voxel count) + **layers** (synthetic "Base" layer until the layer system lands) + masks. No prims, no mesh-wrapper leaf. (Deferred plumbing — `layer_vis_toggle`, `layer_rename_commit`, `layer_select`, `dissolve_layer_id` — is already stubbed in `OutlinerState`, `outliner_panel.rs:28`.)
- **Poly Edit** (`Object › Mesh · 'Hull'`) — header (vert/edge/face counts) + the **modifier stack** as rows (`PolygonalBody.modifiers`, today only in the properties panel). **Never individual vertices/edges/faces** (100k+ rows — element selection stays in the viewport, selection-P0). `half_edge.rs` untouched.

### Architecture — one data-driven outliner, a `RowProvider` per mode
Build **one** mode-aware outliner, **not four widgets**. The shared machinery (deferred-mutation `outliner_draw.rs:104`, the canonical selection writers `selection_unified.rs:11`, `check_double_click` rename, DnD release ordering, row virtualization) is mode-independent — cloning it four times guarantees the rot we're killing. Only the **row model** varies:
- A flat `OutlinerRow { id, depth, icon, label, badges, flags, action_mask }` is the *only* thing the draw loop knows.
- A `RowProvider` trait yields `rows()` for the current mode — the *only* mode-aware code. Concrete `ObjectProvider` / `SdfEditProvider` / `SculptProvider` / `PolyEditProvider`, each ~80–150 lines owning one "what to show" policy.
- Draw loop: `provider.rows()` → `visual_order` (range-select source, unchanged) → one generic `draw_row` (eye/lock/icon/label/badges/rename/select/drag) → `provider.apply()` dispatches deferred actions. Action legality rides on each row's `action_mask`, so the generic context-menu/DnD code never branches on mode.
- **Why it kills the mode-blind bug structurally:** "show prims? show bool ops?" is decided in exactly one place per mode (`Provider::rows`), not implicitly by "whatever's in the graph." Object mode's provider simply never emits prim rows → the SDF tree can't leak into the home view. New mode = one provider impl, zero machinery changes.

### Mode source (correction to the synthesis)
The design synthesis assumed no mode enum exists — **incorrect**. `WorkMode` (Object/Sculpt/Vertex/Edge/Face, `types_poly.rs:140`, in `tool.work_mode`) and `ModelingContext` (`types_poly.rs:12`, in `modeling_context`) **already exist** and are the live mode state (the P0.1 fix uses them). So `OutlinerMode` is a thin **derive** from `tool.work_mode` + `modeling_context` + the active object — *not* new desyncable state. The Mode breadcrumb, the viewport renderer, and the outliner therefore all read the same existing `work_mode`: one source of truth, already present.

### Confirmed decisions (2026-06-19)
- **Lights/Cameras in Edit modes:** a pinned, collapsed **"Scene Globals"** section at the bottom of every edit mode holds lights + cameras — focused edit tree, lights one click away. ✅ CONFIRMED.
- **Voxel object identity:** voxel is promoted to a **first-class `VoxelBody` graph node** (peer of `SdfBody`), replacing the `ImportedMesh` masquerade (`input_sculpt.rs:297`) + the single global `self.voxel`. This is what makes "multiple voxels, each its own edit context" real (per-object edit, `🧊VOX` chip, breadcrumb naming). Small scene-graph migration — sequenced early in the foundation. ✅ CONFIRMED.

### Migration (smallest-first; detailed steps go in MASTER_WORK_LIST)
Fix the still-live audit bugs first (**O3** body-header ctrl-click toggle, **O4** drop-on-prim cross-body guard, **O6** `frame_selected` body-local→world) → derive `OutlinerMode` → extract `OutlinerRow` + generic drawer, route the existing SDF path through it (pixel-identical refactor) → add `ObjectProvider` (Object mode hides prims — the headline fix lands here) → decouple "select body" from "enter edit scope" (click selects, Tab/dbl-click enters) → `SculptProvider` → `PolyEditProvider`. Each step builds green, preserves selection-P0, uses `check_double_click`, leaves frozen files alone.

---

## Invariants (the spirit of the old Room Invariant, reframed)

- **Never two depth conventions over one frame** *(AMENDED 2026-08-03, owner-ratified — Phase D Q1 gate)*. All paths writing the same pixels share ONE depth encoding, ONE render-target set, one resolve. The old "one technique per view" wording over-constrained: the crash class (`4f1a30a`) was colliding depth conventions, not technique count. Nanite-class cluster raster (sw+hw rasterizers → one visibility buffer, one depth encoding) is legal; a partial cluster/plain-raster mix racing over shared pixels is not.
- **No fused multi-object raymarch.** Mixing representations happens only by *baking to triangles* (Object Mode) — never by running two raymarch/raster paths over the same frame.
- **Selection always works** in every mode (P0).
- **Edit is per-object**; the active object is the edit subject.

---

## Roadmap impact (re-sequencing)

This **moves the unified Object viewport to the foundation**. It was filed as the late "Assembly room" capstone (old P6); the owner's "overall viewport first" correctly promotes it, because it is the home every other mode branches from. New ordering intent:

- **Foundation:** the unified Object-Mode raster (SDF bake-to-mesh wired into the raster draw list + cached) + the Mode breadcrumb + per-mode outliner architecture.
- Then per-object Edit Modes refine on top (voxel sculpt is largely built; SDF edit exists; poly edit needs the quality pass).
- The crash is already neutralized (`4f1a30a`); the unified viewport is the permanent solution.

MASTER_WORK_LIST will be re-sequenced to match once the outliner spec lands.
