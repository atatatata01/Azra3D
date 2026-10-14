# SDF Toolset — Live Audit + Forward Plan

**Date:** 2026-07-20  
**Status:** Living plan (code wins over older docs)  
**Method:** Explore agents audited shapes/booleans/modifiers, snapping/transform, and UX/menus.  
**Rule:** Do not re-list shipped features as “missing.” Older docs (`MODELING_TOOLS_SCOPE.md`, `SPINE_3D_PIPE_PLAN.md` session log, etc.) are stale on snap, spine, shell, stamp, blend profiles.

---

## 0. Corrections (previous review errors)

| Earlier claim | Live reality |
|---------------|--------------|
| Snapping missing | **Smart Snap is shipped** — magnet + Ctrl; V/E/F/G filters; wired into gizmo, create, pivot, poly grab, etc. |
| 3D spine = Phase A only | **SpinePipe is a full 3D path solid**: points, closed, per-point radius scale, Sharp/Bezier/Chamfer, Bezier handles, exclusive editor, CPU+GPU bake |
| Shell missing | **ModifierType::Shell** + CurveExtrude wall shell |
| SDF→Voxel stamp missing | **Body props Stamp/Carve** → `fill_from_sdf` |
| 8 blend profiles | **Only Smooth + Chamfer** (intentional) |

---

## 1. Current SDF toolset (what users actually have)

### 1.1 Primitives (`SdfShape`)

| Shape | User can… | Access |
|-------|-----------|--------|
| Sphere, Box, Cylinder, Torus, Cone, Capsule | Footprint create, resize, boolean chain | Left strip + Add + Shift+A |
| Stair, SpiralStair | Architectural steps | Strip + Add (not in Shift+A popup) |
| CurveExtrude (Polyline) | Closed fill + shell/rim; open wall/tube; per-corner round/chamfer | Add / Draw Polyline / Shift+A |
| SpinePipe (3D spine) | Free 3D path tube, taper per point, Sharp/Bezier/Chamfer, handles | Add / Shift+A → spine edit tool |
| HalfSpace | Infinite plane cut | Properties “Add Slice” |
| MeshVolume | Poly as SDF operand (static or live link) | Convert / link poly→SDF |
| Billboard | Thin card | Shape swap (weak create path) |

### 1.2 Booleans & surface ops

| Op | User can… | Access |
|----|-----------|--------|
| Union / Subtract / Intersect | Hard or smooth (k) CSG | X pie, properties, create A/S/I, outliner |
| Seam | Panel line / groove / bead (V/U/Square, raised) | X pie + properties |
| Extrude (boolean) | Raise/recess surface in footprint | X pie + properties |
| Extract Intersection | Door-from-wall style solid extract | X pie MORE… / tool settings |
| Nested body Subtract | Two-body X nest | Select 2 bodies + X |
| Blend K + Smooth/Chamfer | Live fillet style | Diamond handle + properties |
| Blend falloff + blend noise | Directional / noisy blends | Properties |

### 1.3 Modifiers (prim stack)

Mirror, Twist, Taper, Shear, Smooth, Displace, Sculpt, Spherize, **Shell**, Emboss, Deboss, **Array** (linear + circular).  
Body field mods: Volume Mirror, Body Bevel, Body Smooth (CPU-heavy).

### 1.4 Interactive tools (live)

| Tool | How user uses it |
|------|------------------|
| Create (Modo-style) | Arm shape → LMB footprint → extrude; surface/floor/WP; create-time bool op |
| Gizmo G/R/S + Elongate (E) | Standard transform; elongate faces |
| Face push/pull | Resize box faces |
| Bevel mode (B) | SDF edge bevel edit |
| Smart Mirror (Alt+X) | Place plane, mirror/instance |
| Clip curve (C) | Draw polygon cutter on prim/body |
| Curve point edit | Edit polyline profile verts, corners, G/R/S |
| Spine point edit | Exclusive 3D path authoring (see §1.5) |
| Slice + Slice Duplicate | HalfSpace cut + split into two bodies |
| Shift+D | Duplicate + gizmo |
| Ctrl+Shift+D | Linear/circular array session (scroll count; M → Array mod) |
| Group Ctrl+G / Instance Ctrl+I | Body grouping / instances |
| Convert SDF↔Poly / poly→SDF | Outliner / menus |
| Stamp SDF into sculpt | Body props when voxel active |

### 1.5 3D Spine — shipped behavior (do not replan as greenfield)

**Data:** `points`, `radius`, `closed`, `joins`, `point_scale`, `handles_in/out` (max 64 pts).  
**Math:** expand Sharp/Bezier/Chamfer centerline → tube with lerped per-point radius; CPU + GPU bake (st=12).

**User workflow:**
1. Add Spine → exclusive point-edit modal.
2. Ctrl+click add points on workplane; LMB move; Shift multi; RMB delete; Shift+edge insert.
3. Scroll / B pie: Sharp → Bezier → Chamfer; yellow Bezier handles (Ctrl = linked turn).
4. S scale (radius or points); R twist; Ctrl+drag surface snap; Shift angle snap.
5. Space/Enter → Object mode; G/R/S whole pipe; boolean like any prim.

### 1.6 Snapping — shipped behavior (do not replan as greenfield)

**Primary system: Smart Snap** (`smart_snap.rs` + `snapping.rs`)

| Control | Effect |
|---------|--------|
| Magnet bar toggle | Permanent smart snap |
| Hold **Ctrl** | Momentary snap |
| **V** | Vertices / corners / poles |
| **E** | Edge midpoints |
| **F** | Face centers |
| **G** | Floor grid intersections |
| Eye | Visible-only occlusion filter |
| Threshold px | Pick radius |

**Wired into:** create footprint, gizmo/modal transform, pivot (Ctrl), poly G/R/S increments, Smart Mirror (Ctrl grid/angle), face push-pull increments, knife (own mesh snap), spine (grid + surface + angle), curve profile (45°/grid).

**Candidates:** analytic SDF feature points (box corners, cyl rims, spine points, …) + poly mesh V/E/F + floor grid.

---

## 2. True remaining gaps (code-verified)

Only items with incomplete product value or dead/orphan paths. Grouped by user value.

### Tier A — High user value / polish of existing power

| ID | Gap | What user experiences today | What shipping fixes |
|----|-----|----------------------------|---------------------|
| A1 | **Snap dual-toggle / dual grid** | `smart_snap_on` vs `snap_enabled`; `snap_grid` vs `grid_cell_size` — tools disagree | One mental model: one magnet, one grid size, consistent Ctrl |
| A2 | **Snap C (Center of Mass) no-op** | Bar toggle C does nothing (`SnapFilter::All` never emitted) | Real COM / selection-center targets |
| A3 | **SDF snap ignores rotation** (most shapes) | Rotated box corners snap as if axis-aligned | Candidates use prim `rotation_quat` (spine/billboard already do) |
| A4 | **Elongate not snapped** | E face drag ignores smart snap | Elongate uses same snap as G |
| A5 | **BoolCut orphan** | `bool_cut.rs` exists; **no key/menu/strip entry** | Wire BoolCut as discoverable tool (or delete if Slice replaces it) |
| A6 | **Polyline/Spine not on left strip** | Only Add / Shift+A | Catalog buttons for path tools |
| A7 | **Stair/Spiral missing from Shift+A** | Inconsistency | Add to popup list |

### Tier B — Path Object completion (spine is NOT missing; endgame is)

| ID | Gap | User story when done |
|----|-----|----------------------|
| B1 | **Custom 2D profile on spine** | Same 3D path, square/duct/beam section instead of only round tube |
| B2 | **Spine-native shell** | Explicit hollow pipe wall (not only general Shell modifier) |
| B3 | **Caps + path repetition** | Flange at ends; clips/bolts along arc length from body/library |

### Tier C — CAD / hard-surface depth

| ID | Gap | User story when done |
|----|-----|----------------------|
| C1 | **Curve-controlled blend** | User-drawn blend ramp replaces fixed Smooth/Chamfer; presets mimic old stairs/groove looks |
| C2 | **Finite slab slice** | Thick/limited cut without infinite HalfSpace side effects |
| C3 | **Per-edge box bevel UI** | Advanced 12-edge radii (data already there; agent noted groups-of-4 — verify Advanced collapsible still complete) |
| C4 | **First-class Offset surface** | Solid grow/shrink (`d ± t`) labeled clearly (Smooth is close but not the product name) |

### Tier D — Parity / reliability (not new tools)

| ID | Gap | Why it matters |
|----|-----|----------------|
| D1 | **CPU vs GPU parity** | Subdivision offset GPU-only; CylinderBevel CPU ignores; multi-mod stack often CPU bake |
| D2 | **Nested sub-body transforms** | Translation-only compose; rot/scale incomplete |
| D3 | **Nested non-Union pick/snap quality** | Hierarchy works; tool queries can lag GPU truth |
| D4 | **Workplane “center place”** | Ctrl+Shift+RMB claims center; implements surface hit |

### Tier E — Dead code / discoverability cleanup

| ID | Item | Action |
|----|------|--------|
| E1 | `ArrayDupSession` / `start_shift_dup_array` | Dead; live array is Ctrl+Shift+D — remove or rewire |
| E2 | `SnapMode` enum | Saved only; document dead or drive logic |
| E3 | SDF sculpt brush | Properties-only — consider strip/mode visibility |
| E4 | HalfSpace slice | Properties-only — optional hotkey |

### Explicitly NOT missing (agents confirmed shipped)

- Smart Snap (V/E/F/G + magnet + Ctrl)  
- 3D Spine with Bezier/Chamfer/taper/editor  
- Shell modifier  
- Array linear/circular  
- Seam + Extrude booleans  
- SDF→Voxel stamp/carve  
- Extract Intersection  
- Smart Mirror  
- Clip curve / polyline / curve editor  
- Box bevel groups + profiles/angles  

---

## 3. Forward plan (phased)

### Phase 0 — Trust the live toolset (docs + discoverability) — 0.5–1 day

**Goal:** Stop agents/docs from rediscovering “missing snap/spine.”

1. Mark stale sections in `SPINE_3D_PIPE_PLAN.md` (A/B/E largely shipped; remaining B1–B3 only).  
2. Mark `MODELING_TOOLS_SCOPE.md` rows for snap, shell, stamp as shipped.  
3. Strip catalog: Polyline + Spine buttons; Shift+A: Stair/Spiral.  
4. BoolCut: either wire entry (strip/key) or archive file with comment “superseded by Create Slice / HalfSpace.”

**User impact:** Features already built become findable; fewer false “missing” reports.

---

### Phase 1 — Snap polish (make what you have feel one system) — 1–2 days

**Goal:** One snap mental model; fix dead C; fix rotated SDF candidates.

| Task | User-facing result | Files (likely) |
|------|--------------------|----------------|
| 1.1 Unify magnet / Enable Snap | One toggle drives all tools (spine included) | `smart_snap.rs`, spine, `win_grid_workplane.rs` |
| 1.2 One grid step | Create, gizmo, face, poly share same step | `transform.rs`, `face_ops.rs`, create settings |
| 1.3 Emit COM/selection-center candidates | C filter works | `snapping.rs` |
| 1.4 Rotate SDF candidates | Snapping a rotated box hits true corners | `snapping.rs` |
| 1.5 Elongate respects snap | E drag quantizes / teleports like G | `transform.rs` / gizmo elongate |

**Verify:** Create + G + E + spine with magnet on; rotated box corner snap; C label moves to selection center.

---

### Phase 2 — Path Object endgame (spine already solid) — multi-session

**Goal:** Round tube path → full path solid. **Do not rebuild A/B/E.**

| Task | What it does | How user uses it |
|------|--------------|------------------|
| **2.1 Custom profile (B1)** | Sweep 2D profile (circle or custom polygon) along 3D spine | Select spine → Section: Custom → edit profile like polyline → pipe becomes duct/beam |
| **2.2 Spine shell (B2)** | Inward wall thickness on tube/profile | Slider “Wall”; solid → hollow pipe |
| **2.3 Caps + repeat (B3)** | Stamp bodies at ends + array along length | Props: Start/End cap from body/library; Count/spacing; Union stamps as children |

**Order:** 2.1 → 2.2 → 2.3 (profile before caps alignment).  
**Reuse:** CurveExtrude profile editor UX; MeshVolume stamp policy from spine plan; clip side-buffer packing.  
**Gates:** CPU+bake parity; bounds never underestimate; hash includes profile/shell/caps.

---

### Phase 3 — Hard-surface blend authorship — 2–4 days

**Goal:** Curve-controlled blend (replaces desire for 6 fixed profiles).

| Task | User workflow |
|------|---------------|
| 3.1 Data: curve LUT or control points on blend (field-trap-safe) | Properties: Blend → Curve editor |
| 3.2 Sample curve in smin/smax (CPU + bake + parity) | Drag k diamond; curve shape changes fillet |
| 3.3 Presets stamp curves | Buttons: Smooth, Chamfer, Soft step, Groove-like |

**Out of scope for 3.x:** Seam (already first-class intersection feature line).

---

### Phase 4 — Slice UX completeness — 1–2 days

| Task | User workflow |
|------|---------------|
| 4.1 Wire **BoolCut** OR document Create-op Slice as the tool | Surface click → thin box cut with depth/radius |
| 4.2 Optional finite slab (two halfspaces or thick box) | “Slab thickness” for limited kerf |
| 4.3 Hotkey for Add Slice | Faster than properties-only |

---

### Phase 5 — Parity & nested reliability — ongoing

| Task | Why |
|------|-----|
| CPU subdivision offset match GPU | Tools/picking match display |
| CPU CylinderBevel match GPU | Beveled cyl correct under snap/DC |
| Nested body rot/scale | Assemblies as cutters behave correctly |
| Multi-mod GPU path (or honest UI) | Stack >1 mod doesn’t surprise bake path |

---

## 4. Priority order (recommended)

```
P0  Phase 0 — docs + strip discoverability (Polyline/Spine/BoolCut decision)
P1  Phase 1 — snap unify + rotation + C + elongate
P2  Phase 2.1 — custom spine profile (biggest new product surface on path tools)
P3  Phase 3 — curve blend (hard-surface look control)
P4  Phase 2.2–2.3 — shell + caps/repeat
P5  Phase 4–5 — slice polish + parity
```

Rationale: **Do not invent tools you already have.** Maximize discoverability and snap reliability first; then finish the path object users already open every day; then blend authorship.

---

## 5. Definition of done (product)

A user who knows Plasticity/Blender hard-surface can:

1. Find **Polyline** and **Spine** on the tool strip without hunting Add menus.  
2. Trust **one snap magnet** for create, G, E, spine, and pivot.  
3. Draw a **3D path**, set **Bezier**, taper points, then assign a **square profile** and **hollow wall**.  
4. Cap a pipe with a library flange and **repeat** clips along the path.  
5. Author a **custom boolean blend** curve for panel soft joins.  
6. Slice with either infinite plane or thick cut without orphan tools.

---

## 6. Agent evidence index

| Domain | Agent findings anchor |
|--------|----------------------|
| Shapes/bools/mods/tools | Live inventory §1–9 of explore audit 2026-07-20 |
| Snapping | Smart snap solid; SnapMode dead; C no-op; rotation partial |
| UX/access | X pie, Alt+X mirror, Ctrl+Shift+D array, stamp/shell/spine shipped; BoolCut orphan |

---

## 7. Session note

This plan **replaces** any “missing snap / missing 3D spline” work items. Future tasks must cite this file or re-grep live code before claiming a feature gap.

---

## 8. Execution log

### 2026-07-20 — Phase 0 + Phase 1 shipped

**Phase 0 — Discoverability**
- `ToolButtonId`: `ShapePolyline`, `ShapeSpine`, `ToolBoolCut` (+ meta, default SDF strip, catalog)
- Workspace handlers: polyline draw, spine create+edit, BoolCut arm
- Shift+A: Stair, Spiral, Draw Polyline…
- BoolCut fully wired: LMB footprint → release depth → LMB confirm / RMB cancel
- Stale docs patched (spine plan, MODELING_TOOLS_SCOPE rows)

**Phase 1 — Snap polish**
- Rotated SDF snap candidates (`snap_world` with `rotation_quat`)
- C filter: prim center + selection centroid (`SnapFilter::All`)
- Elongate grid-quantizes when snap active
- Magnet / Enable Snap unified (`set_smart_snap_on`; spine uses either flag)
- CurveExtrude / MeshVolume snap feature points added

**Still open:** Phase 2 spine profile/shell/caps, Phase 3 curve blend, nested parity.
