# SPINE_3D_PIPE_PLAN — SDF 3D Spine / Pipe / Path Object

**Created:** 2026-07-19  
**Parent:** `POLYLINE_SHAPE_PLAN.md` Tool 2  
**Status:** Phase A IN PROGRESS  
**Worktree:** `.claude/worktrees/sdf-spine` · **Branch:** `feat/sdf-spine` · **Launcher:** `Dev_Spine.bat`  
(Do **not** pile onto `feat/poly-phase0`.)

---

## 0. What you asked for (owner intent)

A **3D path object** that can:

| Intent | Translation |
|--------|-------------|
| Shell | Hollow pipe (inner void), not only solid tube |
| Shape profile based on 3D curve edit | Cross-section editable as a **profile** (2D curve UI), swept along a **3D spine** |
| Resize each point | Per-spine-point radius / scale along the path |
| 3 point types: Bezier / Bevel / Sharp | Spine corner continuity: smooth handles, chamfer join, hard corner |
| Begin cap · repetition · end cap | Path-array of **bodies** (from scene list or library) at start, along length, end |

**Honest reframe:** this is not “Tool 2 round tube” from the original plan.  
It is a **Path Object** (Blender curve + bevel + array; Modo curve extrude + clone along path).  
That is the right product ambition — but it must ship in **layers**, or it will thrash eval, bounds, serialize, and editor in one PR.

---

## 1. What already exists (reuse — do NOT fork)

| Piece | Location | Use for spine |
|-------|----------|----------------|
| 2D profile verts + Sharp/Round/Chamfer (sign of `bevel_radius`) | `core_math/src/curves.rs` `CurveVertex` | **Profile** cross-section (not spine) |
| Planar path + wall/tube + shell + rim | `SdfShape::CurveExtrude` | Precedent for open tube math + shell bounding rules |
| Point editor (select/G/R/S/bevel/flatten) | `app/src/curve_point_edit.rs` | **UX pattern** for profile; spine editor is new (3D) |
| Variable-length GPU side buffer | clip-curve serialize (`serialize_clip_curves_*`) | Spine points + profile verts (dynamic length) |
| `curve_params` side-channel | `ProjectFile` keyed by `prim_uid` | Extra fields without bincode field-trap |
| Body library insert/save | `app/src/body_library.rs` | Cap / repeat sources |
| Scene graph bodies as operands | `NodePayload::SdfBody` + boolean chain | Caps as linked sub-bodies / boolean stamps |
| MeshVolume (poly→SDF grid) | `SdfShape::MeshVolume` | Optional heavy path for complex cap meshes |

**Hard limit today:** `MAX_CURVE_PROFILE_VERTS = 14` for old hot-slot packing. Spine/profile must use the **side-channel buffer** (clip-curve style), not hot floats.

**Important:** current `CurveExtrude` is **planar only** (profile in XZ, extrude Y or open path in plane). A free 3D spine is a **new shape variant**, not a flag on CurveExtrude.

---

## 2. Conceptual model (two curves, not one)

```
                    ┌─────────────────────────────────────┐
                    │  SPINE (3D path)                     │
                    │  points in prim-local 3D             │
                    │  Sharp / Bevel / Bezier              │
                    │  per-point radius scale              │
                    └─────────────────┬───────────────────┘
                                      │ sweep / pipe
                    ┌─────────────────▼───────────────────┐
                    │  PROFILE (2D cross-section)         │
                    │  CurveProfile in a local frame       │
                    │  circle default · or custom polygon  │
                    │  shell / rim on section              │
                    └─────────────────┬───────────────────┘
                                      │ optional
              ┌───────────────────────┼───────────────────────┐
              ▼                       ▼                       ▼
         START CAP              REPETITION                 END CAP
     body / library          body / library            body / library
     aligned to T0           along arc-length          aligned to T1
```

### Naming (avoid confusion)

| Term | Meaning |
|------|---------|
| **Spine** | 3D path the solid follows |
| **Profile** | 2D shape of the solid in the plane perpendicular to spine tangent |
| **Join type** | How spine corners blend (Sharp / Bevel / Bezier) |
| **Cap** | Boolean stamp of another body at end(s) |
| **Repetition** | Discrete clones of a body along arc-length (array), not a continuous boolean |

**“Shape profile based on a 3D curve edit”** should mean:

- Spine: edited in **3D** (viewport)
- Profile: edited in **2D** (same UX as today’s polyline editor, UV plane)

Editing the profile as a free 3D curve is the wrong default (cross-section is 2D by definition).  
If you later want a **3D profile** (non-planar section), that is a separate product — reject for v1–v3.

---

## 3. Full parameter set

### 3.1 Spine parameters

| Param | Type | Default | Notes |
|-------|------|---------|--------|
| `points[]` | list of `SpinePoint` | 2–3 pts | Prim-local positions |
| `closed` | bool | false | Loop path |
| `resolution` | u32 | 8–16 | Samples per segment for eval / caps alignment |
| `twist` | f32 | 0 | Global roll (degrees) of profile frame |
| `twist_mode` | enum | Minimal | Minimal twist vs fixed up (rail) |
| `normal_hint` | Vec3 | Y | Up for Frenet/rail frame |

### 3.2 Per-point (`SpinePoint`)

| Param | Type | Default | Notes |
|-------|------|---------|--------|
| `pos` | Vec3 | — | Local space |
| `join` | enum | Sharp | **Sharp · Bevel · Bezier** |
| `radius` | f32 | 1.0 | Multiplier on global radius / profile scale |
| `bevel` | f32 | 0 | Join setback when join=Bevel (world units or fraction of min edge) |
| `handle_in` | Vec3 | auto | Bezier only (local offset from pos) |
| `handle_out` | Vec3 | auto | Bezier only |
| `handle_linked` | bool | true | Mirror handles when Bezier |
| `twist_offset` | f32 | 0 | Optional per-point roll |
| `weight` | f32 | 1.0 | Future NURBS-ish; **defer** |

### 3.3 Profile / section parameters

| Param | Type | Default | Notes |
|-------|------|---------|--------|
| `section` | enum | Circle | Circle · CustomProfile |
| `radius` | f32 | 0.05–0.1 | Global tube radius (× point.radius) |
| `profile` | CurveProfile 2D | unit circle approx | When CustomProfile; verts Sharp/Round/Chamfer |
| `profile_scale` | f32 | 1 | Global scale of custom profile |
| `shell` | f32 | 0 | Wall thickness inward (same contract as CurveExtrude shell) |
| `rim` | f32 | 0 | Cap-edge round/chamfer of solid ends (open spine only) |
| `caps_mode` | enum | Hemisphere | Open ends: flat · round (tube free ends) · none |

**Circle** is a profile preset (not a separate shape): either analytical circle or N-gon profile.

### 3.4 Cap & repetition (path array)

| Param | Type | Default | Notes |
|-------|------|---------|--------|
| `start_cap` | CapSlot | None | See CapSlot |
| `end_cap` | CapSlot | None | |
| `repeat` | RepeatSlot | None | Mid-path array |

**Cap policy (owner 2026-07-19 — LOCKED):**

> On assign: **duplicate + bake** the chosen body/library assembly into a **frozen MeshVolume stamp**
> (child prim under the pipe body). **No live link.**  
> To change: **rechoose source → rebake and replace** the stamp.  
> Editing the original body after stamp does nothing until rechoose.  
> Repeats share one baked grid asset, many poses.

**CapSlot**

| Field | Type | Notes |
|-------|------|--------|
| `source` | None · BodyNodeId · LibraryPath | Scene body **or** library asset (reload UI only) |
| `align` | enum | Tangent · Tangent+Up · World |
| `offset` | f32 | Along-path inset from end (avoid double material) |
| `scale` | f32 | Uniform scale of stamp |
| `boolean` | Union · Subtract | Usually Union for decorative caps |
| `stamp_res` | u32 | MeshVolume grid resolution at bake |

**RepeatSlot**

| Field | Type | Notes |
|-------|------|--------|
| `source` | BodyNodeId · LibraryPath | |
| `count` | u32 · or spacing | Fixed count **or** spacing in world units |
| `start_t` / `end_t` | 0–1 | Usable length fraction (leave room for caps) |
| `align` | same as caps | |
| `scale` | f32 | |
| `boolean` | Union · Subtract | |
| `phase` | f32 | Rotate first instance |

### 3.5 Boolean / body membership

| Param | Type | Notes |
|-------|------|--------|
| Prim `operation` | existing BooleanOp | Spine solid participates in body like any prim |
| Blend K / seam | existing | Free once it’s a prim |

---

## 4. What you forgot (important)

| Gap | Why it matters |
|-----|----------------|
| **Frenet / rail frame** | Profile orientation along 3D curve; without it, section spins (classic “twisty rope” bug) |
| **Closed loop** | Caps off; joins first↔last; special case for Bezier |
| **Self-intersection / min radius** | Tight Bezier + large radius → self-overlap SDF artifacts |
| **Bounding contract** | Sparse bake drops bricks if AABB/radius wrong (P0-B1 class hole) |
| **Max points / tessellation budget** | Bake cost ∝ segments × profile samples |
| **UV / materials** | Path object materials; optional later |
| **Mirror / symmetry** | Body mirror already exists; document interaction |
| **Convert to poly / MeshVolume** | Escape hatch when SDF fidelity fails |
| **Cap boolean order** | Caps must not double-carve the tube end (offset + depth) |
| **Library ↔ scene ID** | Library insert makes **new** nodes; spine stores path/string or stamped prims, not raw library handles forever |
| **Undo model** | Spine edit = episode; cap source change = one-shot; array rebuild = one transaction |
| **Pick / outliner** | One prim vs group (spine + cap children) — pick which hierarchy |
| **Open vs closed profile** | Custom profile closed only for solid fill; open profile = ribbon along spine (v3+) |
| **Scale non-uniform** | Per-axis profile scale (ellipse) — easy win, often wanted |

---

## 5. Does the design make sense?

### Yes — if layered

| Idea | Sense check |
|------|-------------|
| 3D spine + round tube | **Yes** — classic, well-defined SDF (`capsule chain` / polyline distance − r) |
| Per-point radius | **Yes** — lerp radius along segments |
| Sharp / Bevel joins | **Yes** — matches 2D CurveVertex semantics, extended to 3D |
| Bezier joins | **Yes but expensive** — need handle UX + dense sampling; not v1 |
| Custom 2D profile sweep | **Yes** — industry standard; heavier eval (Frenet + 2D SDF in section plane) |
| Shell | **Yes** — same inward-only rule as CurveExtrude (`max(d, -d - wall)`) |
| Start/end caps from bodies | **Yes as stamps/children** — not as pure analytic SDF of arbitrary mesh |
| Repetition along path | **Yes as array of prims/bodies** — not as one magical continuous field |

### No / careful

| Idea | Risk |
|------|------|
| “Profile is a 3D curve edit” | Confuses spine vs section. Profile = 2D editor. |
| Caps “live linked” to library without bake | ID/path churn; prefer **stamp on assign** + optional re-resolve |
| One mega-shape that embeds full body graphs | Bloats prim; better **spine prim + generated child prims** for caps/array |
| Bezier + custom profile + shell + caps in one milestone | Token/context explosion; will rot |

### Recommended hierarchy (scene graph)

```
SdfBody "Pipe.001"
  ├── Prim SpinePipe (the continuous SDF tube/profile sweep)
  ├── Prim/Body CapStart   (optional stamp, transform locked to T=0)
  ├── Prim/Body CapEnd
  └── Group "Repeat"
        ├── stamp_0 … stamp_n   (transforms along arc-length)
```

**Why:** caps/array use **existing** SDF bodies and library insert; spine only owns continuous solid.  
One undo for “rebuild array” is easier than embedding meshes inside the spine prim.

**Alternative (simpler v1):** spine prim only; caps/array are **manual** (user places library bodies).  
Then automation is a later tool (“Apply path array”).

---

## 6. Math notes (eval)

### Phase A — solid round tube (open/closed)

```
d = dist_to_3d_polyline_or_curve(p, samples) - radius(t)
```

- Sharp: polyline segments (capsule union)
- Bevel: cut corners (truncate edges by bevel distance) then capsule chain
- Bezier: sample cubic segments at `resolution`

### Phase C — custom profile

At sample point with frame `(T, N, B)` and local radius scale `s(t)`:

```
q = project p into section plane at nearest spine sample
d2 = profile.sdf_2d(q / s(t)) * s(t)
d  = combine with along-path distance (or pure “infinite” sweep local)
```

Use **rail / parallel transport** for `(N,B)` to avoid Frenet flips at inflection.

### Shell

```
if shell > 0: d = max(d, -d - shell)   // inward only — preserve bounds
```

### Caps / repeat

Not pure distance of one prim: **place** referenced bodies, boolean Union/Subtract in body order.

---

## 7. Storage / serialize (field-trap safe)

**New shape variant** (append LAST to `SdfShape`):

```text
SdfShape::SpinePipe { … hot constants … }
```

**Variable data** (not bincode-inline lists on hot prim if possible):

| Data | Channel |
|------|---------|
| Spine points + handles + join + radius | Side buffer (clip-curve pattern) **or** `spine_params` ProjectFile map by `prim_uid` |
| Custom profile verts | Same side channel / curve_params extension |
| Cap/repeat slots | Side channel (NodeId remapped on load; library as path string) |

Rules (standing from POLYLINE plan):

- Enum variants **append-only**
- Prefer `#[serde(skip)]` + side-channel for growing fields
- Hash spine + profile + slots in `hash_shape` **same commit**
- Bounds: AABB of spine points + max radius + shell pad + profile extent

---

## 8. Editor UX (by phase)

| Phase | Editor |
|-------|--------|
| A | Click-to-place 3D points (workplane/surface), G on points, Del, closed toggle |
| B | Per-point radius (scroll / properties), join Sharp/Bevel |
| C | “Edit profile” mode → reuse 2D `curve_point_edit` on profile UV |
| D | Shell slider (properties) |
| E | Bezier handles (draw handles, drag, linked toggle) |
| F | Cap/repeat UI: dropdown scene bodies + “from library…” |

Keyboard: isolate spine edit context like curve point editor (don’t steal poly G/R/S when inactive).

---

## 9. FEATURE_RULES (4 questions) — whole feature

1. **New scene object?** Yes — new `SdfShape` variant + optional child stamps.  
2. **Mutates state?** Yes — `transaction` / episodes for edits.  
3. **Frame-thread work?** Bake dirty via existing body volume path.  
4. **New render path?** No — CPU + `volume_bake.wgsl` only (analytical dead).

---

## 10. Phased delivery (token-safe slices)

Each phase = **one agent / one prompt series**, own branch or stacked PR, gates green before next.

### Phase A — Spine tube MVP (shippable alone)

**Goal:** open 3D polyline, constant radius, solid circle section, Sharp only.

- `SdfShape::SpinePipe` + side-channel points  
- CPU distance + bake WGSL  
- Bounds + hash  
- Create: draw tool (click points) + properties radius/closed  
- Tests: capsule chain vs analytic, bound never underestimates  

**Out:** Bezier, bevel, profile, shell, caps.

### Phase B — Per-point radius + Sharp/Bevel

- `SpinePoint.radius`, join Bevel  
- Properties + scroll radius on selection  
- Tests: radius lerp, bevel corner shortens path  

### Phase C — Custom profile (2D)

- `section = CustomProfile`  
- Profile editor reuses 2D point editor  
- Rail frame + twist  
- Tests: square profile sweep parity CPU/GPU  

### Phase D — Shell + end style

- Shell inward  
- Rim / open-end hemisphere option for circle section  

### Phase E — Bezier joins

- Handles, linked, sample resolution UI  
- Handle overlay + drag  
- Tests: C1 continuity sample, no self-intersect clamp  

### Phase F — Caps + repetition

- CapSlot / RepeatSlot UI  
- Library + body picker  
- Rebuild stamps as children in **one transaction**  
- Align frames at arc-length samples  
- Tests: count spacing, end alignment  

### Phase G (optional) — polish

- Convert spine→poly mesh  
- Non-uniform profile scale  
- Linked caps live update  

---

## 11. Definition of done (product)

| Milestone | User can… |
|-----------|-----------|
| A | Draw a 3D cable, boolean it with a box, undo |
| B | Taper the cable by point radii; hard vs chamfer corners |
| C | Sweep a square profile along the path |
| D | Hollow pipe with wall thickness |
| E | Smooth organic Bezier path |
| F | Screw-on end fittings from library + repeated clips along path |

---

## 12. Risk register

| Risk | Mitigation |
|------|------------|
| Scope = full DCC path tool | Ship A alone; F is optional product surface |
| Bake cost | Cap max points / resolution; progressive LOD later |
| Frame flip | Parallel transport; expose “up” vector |
| Cap double-material | Inset end along −T by offset |
| Library IDs after insert | Stamp copies; store source path for “reload from library” |
| Token limit mid-feature | One phase per session; plan file is source of truth |

---

## 13. Verdict

| Question | Answer |
|----------|--------|
| Does the design make sense? | **Yes** as a **phased Path Object**: spine + profile + optional path-array. |
| Forgot parameters? | **Yes** — frame/twist, closed, bounds, tess budget, stamp vs link, undo, hierarchy. |
| Build everything now? | **No.** Start **Phase A** only after this plan is accepted. |
| Original Tool 2 sketch? | Phase A **is** that sketch; your list is Phases A–F. |

---

## 14. Recommended next action

1. Confirm hierarchy: **spine prim + stamp children** (recommended) vs all-in-one prim.  
2. Confirm v1 product cut: **A only** (tube) vs **A+B** (taper).  
3. Open `feat/sdf-spine` and implement Phase A only.

---

## 15. Session log

- 2026-07-19 — Full plan written from owner feature list + code audit of `CurveExtrude` / `curves.rs` / body library. Supersedes one-paragraph Tool 2 sketch for execution detail; parent sequencing still: Tool 0/1 done → Tool 2 phases A→F.
- 2026-07-19 — **Worktree + Phase A started.** Branch `feat/sdf-spine` @ master `cd3b293`. Cap stamp policy locked (freeze MeshVolume, rechoose=rebake). Phase A landed:
  - `SdfShape::SpinePipe { points, radius, closed }` (shape type **12**, after HalfSpace)
  - CPU `dist_to_polyline_3d` + tube distance; unit test `polyline_3d_segment_and_tube`
  - GPU bake `volume_bake.wgsl` st==12 via clip side-buffer point packing
  - serialize/hash/bounds + menus/properties radius+closed + `spine_preset()`
  - Launcher `Dev_Spine.bat`
  - **Not yet:** 3D point editor, draw tool, Bezier/taper/profile/shell/caps (B–F)
