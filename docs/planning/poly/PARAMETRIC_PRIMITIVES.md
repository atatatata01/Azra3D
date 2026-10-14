# Parametric Poly Primitives — Implementation Plan

**Status:** PLAN (not started)
**Owner-requested:** 2026-08-04 — "make our primitives more dynamic for the poly mesh: segments, number of sides, parametric settings the user can change before committing and starting to model"
**Owner ruling 2026-08-04:** *"I don't have any .fl4, we are in development — make it for real, not a bandaid."* → **The save-format-compat constraint is lifted for this work.** Design for correctness, then migrate the dev assets (§7). Do not contort the data model to preserve bincode tags.

---

## 0. Read before touching anything

- `CLAUDE.md` — whole file. Especially **No Hacks**, **No Bandaids (the 5-question test)**, **Transaction Model**, **Single Responsibility + Mandatory Reuse**, **Poly Kernel Decision (HEX-BL)**, **half_edge.rs red line**.
- `docs/planning/poly/POLY_FOUNDATION_PLAN.md`
- `docs/planning/poly/KNIFE_LIVE_CUT_AND_PIPE.md` — the Pipe modifier, for the live-edit/transaction pattern (NOT for where parameters live; see §2.1).

---

## 1. What exists today (audited 2026-08-04)

### 1.1 Kernel builders — `core_math/src/half_edge/builders.rs`

| fn | line | params today | gap |
|---|---|---|---|
| `new_box` | :11 | `half_extents: Vec3` | no subdivisions at all |
| `new_plane` | :38 | `w, d, subdivs_w, subdivs_d` | complete |
| `new_uv_sphere` | :68 | `radius, segments, rings` | complete (winding is test-gated by the comment at :99 — do not "fix" it) |
| `new_cylinder` | :148 | `radius, height, segments` | no vertical segments, no cap-fill choice |
| `new_cone` | :312 | `radius, height, segments` | all-triangle, no vertical segments, no truncation |
| `new_torus` | :275 | `major_r, minor_r, major_segs, minor_segs` | complete |
| `new_pipe` | :183 | `path, radius, segments, closed, caps` | knife/Pipe generator, not an Add-Mesh primitive |

### 1.2 App layer

- `creation.rs:579 poly_build_mesh(&PolyCreateAdjust) -> Option<HalfEdgeMesh>` — the single geometry producer for both create and adjust. **Dispatches on `a.kind: String`** (`"box" | "plane" | "sphere" | ...`). Clamps `seg 3..=256`, `vseg 1..=256`, `rings 1..=256`, `radius >= 0.001`.
- `creation.rs:602 build_segmented_cylinder` / `:638 build_segmented_cone` — app-local `from_polygons` builders used when `vsegments > 1`. **Kernel geometry living in the app.**
- `creation.rs:218 create_poly_mesh(gen: &str)` — hardcoded per-kind defaults :222-236, placement :330, `transaction("Create poly mesh")` :269-291 → `NodePayload::PolygonalBody`.
- `ops/poly/mesh_create.rs:10-32` — six thin ops fns (sanctioned entry layer, enforced by `scripts/check_ops_gate.ps1`).
- Entry points, all five must stay in sync: Shift+A `input_keyboard/handle.rs:254` → `menus.rs:1641-1678`; `ui2/menus.rs:281-286`; `ui2/draw/placement.rs:126-155`; `ui2/tools/activate.rs:103-108`.

### 1.3 The redo panel that already exists

`ui.poly_create_adjust: Option<PolyCreateAdjust>` (`types/types_ui.rs:535-553`), set at `creation.rs:297`, drawn by `tool_params.rs:1007`. On `geom_changed` it rebuilds and swaps the node's `Arc<HalfEdgeMesh>` wholesale (:1118-1126) with `RENDER` only, committing one `transaction_snapshot_only("Adjust {kind}")` on release (:1141). Auto-dismisses via `poly_create_adjust_auto_accept_if_stale` :970.

The **live-edit/commit lifecycle here is correct and is kept.** What is wrong is where the parameters live.

### 1.4 The four real defects

1. **Parameters are not state.** `NodePayload::PolygonalBody` (`payload.rs:129-136`) = `{ name, mesh, mesh_cache_id, material, modifiers }`. Creation params are stored **nowhere**. When the F9 panel dismisses, "16 sides" is gone forever. SDF prims are inherently re-parameterizable (`SdfPrimitive.shape` is live state); poly prims are not. **This asymmetry is the bug.**
2. **Stringly-typed dispatch.** `kind: String` matched in `poly_build_mesh` — a typo is a silent `None`, and the compiler cannot tell you a primitive is missing an arm.
3. **Parameters are duplicated in three places** — `PolyCreateAdjust`'s flat fields (`types_ui.rs:535`), the slider block (`tool_params.rs:1032-1089`), and the static catalog mirror (`tool_params.rs:156-167`) — with no shared type forcing them to agree.
4. **Kernel geometry in the app** (`build_segmented_cylinder`/`build_segmented_cone`).

---

## 2. Design (ratified — build this, don't re-litigate)

### 2.1 A parametric primitive's mesh is DERIVED. The parameters are the object.

```rust
// core_math/src/primitive_spec.rs  (new file)

/// Source of truth for a PolygonalBody's geometry.
pub enum MeshSource {
    /// `mesh` IS the truth: imported, or a primitive the user has applied.
    Authored,
    /// `mesh` is a derived cache. `spec` is the truth; rebuild via `spec.build()`.
    Primitive(PrimitiveSpec),
}

/// One typed struct per primitive. NOT a flat bag of every field.
pub enum PrimitiveSpec {
    Box(BoxSpec), Plane(PlaneSpec), Circle(CircleSpec),
    UvSphere(UvSphereSpec), IcoSphere(IcoSphereSpec),
    Cylinder(CylinderSpec), Cone(ConeSpec), Torus(TorusSpec),
}

impl PrimitiveSpec {
    /// The ONE geometry producer. Clamps internally; total, never fails.
    pub fn build(&self) -> HalfEdgeMesh;
    pub fn label(&self) -> &'static str;
}
```

`NodePayload::PolygonalBody` becomes:

```rust
PolygonalBody {
    name: String,
    source: MeshSource,        // NEW — the truth
    mesh: Arc<HalfEdgeMesh>,   // derived when source == Primitive
    mesh_cache_id: ...,
    material: ...,
    modifiers: Vec<PolyModifier>,
}
```

**Why typed structs and not fields on `PolyModifier`:** `PolyModifier` (`node.rs:329`) is one flat struct carrying every modifier's fields at once — a torus's `minor_r` would sit next to a bevel's `bool_op` in memory and on disk, and nothing stops code from reading a field that is meaningless for the active type. That shape exists for save-compat reasons that no longer apply. Copying it into primitives would propagate the anti-pattern. Per-variant structs make illegal states unrepresentable and let the compiler enforce exhaustiveness.

**Why not "just keep the mesh and forget the params":** that is today's behavior and it is exactly what the owner asked to fix.

### 2.2 Committing = converting to `Authored`

"Before committing" (owner's words) is now a real, inspectable state: `source == Primitive`. Commit is one explicit conversion:

- **Explicit:** an **Apply / Make Editable** button in the properties panel. `source = Authored`; `mesh` keeps its current derived value. One transaction, undoable.
- **Implicit, but never silent:** entering component (vertex/edge/face) edit mode on a `Primitive` body **auto-converts to `Authored` first**, inside one undoable transaction, with a status-bar note. Rationale: the alternatives are both bandaids — silently discarding the user's topology edits on the next slider drag (data loss), or trying to replay edits onto a regenerated mesh (a topology-diff system we are not building and do not need).
- Adding a modifier does **not** force conversion — the stack already evaluates on top of a derived base.

### 2.3 Invariant to enforce structurally

> When `source == Primitive(spec)`, `mesh` is byte-equal to `spec.build()`.

Enforced by construction: **one** function mutates a spec — `set_primitive_spec(nid, spec)` — which writes the spec, rebuilds the mesh, bumps the poly edit generation, and marks `RENDER`. No other code path may write `mesh` on a `Primitive` body. Add `debug_assert!` on that invariant in `validate()`/the display-sync path so a violation trips in dev rather than shipping a stale mesh. *(CLAUDE.md: enforce correctness structurally, not by tribal knowledge.)*

### 2.4 One parameter set, one UI

`PolyCreateAdjust` loses its duplicated geometry fields and becomes `{ spec: PrimitiveSpec, placement }`. The F9 redo panel and the properties panel call **the same** `draw_primitive_spec_params(ui, &mut spec) -> bool` (returns changed). The static catalog (`tool_params.rs:156-167`) is derived from `PrimitiveSpec`, not hand-maintained. `poly_build_mesh`'s string dispatch is deleted; callers use `spec.build()`.

---

## 3. Parameter set

| primitive | parameters |
|---|---|
| **Box** | `size: Vec3` (non-uniform), `seg_x/seg_y/seg_z: u32`, **`round: RoundSpec`** |
| **Plane** | `width, depth`, `subdivs_w, subdivs_d` |
| **Circle** *(new)* | `radius, sides, fill: CapFill`, **`arc: ArcSpec`** |
| **UV Sphere** | `radius, segments, rings`, **`arc: ArcSpec`** |
| **Ico Sphere** *(new)* | `radius, subdivisions: u32` |
| **Cylinder** | `radius, height, sides, vsegments, cap_fill: CapFill`, **`arc: ArcSpec`**, **`round: RoundSpec`** |
| **Cone** | `radius_bottom, radius_top` (top>0 ⇒ frustum), `height, sides, vsegments, cap_fill`, **`arc: ArcSpec`** |
| **Torus** | `major_r, minor_r, major_segs, minor_segs`, **`arc: ArcSpec`** (major sweep) |

`CapFill { None, NGon, TriFan }` — declared once in `core_math` beside the builders, used by circle/cylinder/cone.

### 3.1 `ArcSpec` — partial sweep ("remove a slice of the pie")

Owner-requested 2026-08-04. **Generalized deliberately**: this is one concept shared by every revolved primitive, not a cylinder special case. A cylinder-only `angle` field would be duplicated math the first time someone wants a half-torus.

```rust
pub struct ArcSpec {
    pub sweep_deg: f32,   // 0 < sweep <= 360; default 360 (full)
    pub start_deg: f32,   // rotation offset of the arc start; default 0
    pub close: ArcClose,  // what to do with the two open ends
}
pub enum ArcClose {
    Open,   // no wall — a shell/ribbon, valid but non-manifold-with-boundary
    Wall,   // flat radial faces at both ends (the pie-slice look) — DEFAULT
}
```

Rules:
- `sweep_deg == 360` must produce **byte-identical** geometry to today's full primitive (the seam is welded, `close` is ignored). This is a regression test, not a hope — a partial-arc path that subtly changes the full case is exactly the class of bug this plan exists to avoid.
- `sweep < 360`: `sides` counts the segments across the **swept portion**, so tessellation density stays constant as the user drags the slider (Blender's behavior). Clamp `sides >= 2` when partial (a 1-segment arc is degenerate), `>= 3` when full.
- `close: Wall` on a cylinder/cone adds two radial quad-fans joining the side wall to the axis; on a torus it adds two disc caps at the tube cross-section. The wall faces must be **planar and correctly wound**, and `validate()` must stay clean — assert no non-manifold edge at the seam.
- `close: Open` yields a boundary-edge mesh. That is legal (a plane already has boundary), but it must be *intentional*: the mesh must pass `validate()` as manifold-with-boundary, never as broken topology.
- UI: a single 0–360 slider plus a start-offset drag. Snapping at 90/180/270 when the user holds the usual snap modifier.

### 3.2 `RoundSpec` — rounded corners / rim bevel

Owner-requested 2026-08-04, for Box and Cylinder.

```rust
pub struct RoundSpec {
    pub radius: f32,      // 0.0 = off (default) — off must be byte-identical to today
    pub segments: u32,    // 1..=16, arc resolution of the round
}
```

**MANDATORY BEFORE WRITING ANY OF THIS:** read `docs/planning/poly/BEVEL.md` in full — §6 (graveyard of ~22 failed approaches), §7 (Blender oracle), §4 (pipeline), §5 (invariants). Do not re-derive a dead end. **When done, append to BEVEL.md**: any new dead end → §6, a one-line §11 session-log entry, and update the `project_bevel_corner_reference` memory. The task is not complete until that KB is updated.

Implementation direction (verify against BEVEL.md before committing to it):
- **Reuse `bevel_op.rs`** — build the primitive, then run the existing bevel over its hard edges. Do **not** write a second corner-rounding math path; that is the duplication rule, and a parallel implementation would diverge from the general bevel's fixes over time.
- The known bevel gap is the **multi-edge miter at a degree>3 vertex** (guarded + toasted today). Box corners and cylinder rim vertices are **degree-3**, i.e. the case that already works — confirm this holds for the *segmented* box (`seg_x/y/z > 1`), where interior edges meet differently, before claiming the feature is done.
- Edge selection: Box → all 12 edges. Cylinder → the two cap rims only (not the vertical side seams). With `ArcSpec::Wall` active, the wall/side junction edges are also rim edges — decide and document whether they round.
- **Clamp `radius` against geometry:** box → `min(size)/2`; cylinder → `min(radius, height/2)`. Exceeding it self-intersects. Clamp at the single entry point (`PrimitiveSpec::build()`), and bound the UI slider to the same range so the user never watches a value silently change.
- `radius == 0.0` skips the bevel pass entirely — asserted byte-identical to the un-rounded mesh.

**Cost note for sequencing:** `RoundSpec` is the only parameter in §3 that depends on another subsystem. If the bevel interaction turns out to be deeper than a rim-edge selection, **ship §3 without it and land it as its own gate** — do not let it block the rest. Partial delivery with a stated gap beats a stalled plan.

Clamps live in **one** place, `PrimitiveSpec::build()` (today they're in `poly_build_mesh:579`): `sides 3..=256`, `vsegments/rings/subdivs 1..=256`, `radius >= 0.001`, `minor_r <= major_r`, `ico subdivisions 0..=5` (6 ≈ 1.3M tris — a hard cap, not a hint). Clamping is not a bandaid here because it is the definition of the valid domain, applied at the single entry point, with the UI sliders bounded to the same range so the user never sees a value silently change.

---

## 4. Gated phases

Each gate: `cargo build -p app` + `cargo test -p core_math` green before starting the next. One gate per session. Flag deviations **before** deviating.

### Gate 0 — Kernel: fully parametric builders ✅ DONE 2026-08-04

Landed as `core_math/src/primitive_spec.rs` (+ `primitive_round.rs`), registered in `lib.rs`.
`cargo test -p core_math` → **871 lib tests pass, 0 fail** (34 of them new).

Deviation from the plan, deliberate: the legacy `new_box`/`new_cylinder`/... signatures were NOT
changed. There are **332 call sites** (110 production, 222 test), many keyed on the cube's exact
vertex ORDERING (corner indices are hard-coded across the bevel/knife/unbevel suites). Churning
them bought nothing. Instead `PrimitiveSpec::build()` is the parametric entry point and delegates
to the legacy builder whenever the spec is at default parameters — one implementation, with
`default_*_matches_legacy` tests so the paths cannot drift.

Two findings worth keeping:

1. **`RoundSpec` is NOT blocked by the BEVEL.md §9 #4 gap.** The KB warns that a corner with 3+
   beveled edges flat-clamps at `segments > 1`, which would have made rounded boxes cosmetic. It
   does not apply: a ±1 cube with all 12 edges at offset 0.3 / segments 2 produces **v=56 f=54 —
   exactly Blender's output** (`rounded_cube_ref.py`). Pinned by
   `rounded_cube_matches_the_blender_oracle_topology`. Gate 0b was therefore folded into Gate 0.
2. **`serde_golden::golden_scene_graph_deserializes_exactly` already FAILS on `master` @ cb30dde**
   (`InvalidBoolEncoding(8)`), verified by disabling the new modules and re-running. Pre-existing,
   not caused by this work — but it means the `SceneGraph` layout has ALREADY drifted from its
   golden blob on the trunk. Gate 2 regenerates the goldens for v8 anyway; flag it to the owner
   separately, because on master it means saves may already be broken.

Bug found and fixed during the gate: the walled partial sweep initially emitted a separate axis
vertex per cap and per wall. `from_polygons` twins by vertex INDEX, not coordinate, so four
coincident-but-distinct axis points left the seam unwelded and the slice non-manifold
(`directed edge 17->8 used by 2 faces (same winding)`). Fixed by creating the axis vertices once
and sharing them; gated by `walled_partial_sweep_is_closed` (validate + manifold + zero boundary
loops — `is_manifold` alone accepts open meshes and would not have caught it).

Original scope, for reference:
1. Fold `build_segmented_cylinder` (`creation.rs:602`) and `build_segmented_cone` (:638) into `half_edge/builders.rs` as a `vsegments` parameter on `new_cylinder`/`new_cone`. Delete the app copies; update call sites.
2. `new_box`: non-uniform `size: Vec3` + `seg_x/seg_y/seg_z`. `new_plane`: non-uniform `width`/`depth`.
3. Add `CapFill`; thread through cylinder/cone.
4. Add `new_circle`, `new_ico_sphere`.
5. `ArcSpec` (§3.1) on circle / cylinder / cone / uv_sphere / torus.
6. Tests (`core_math/tests/`): every primitive × several param combos → `validate()` clean, expected Euler characteristic, no non-manifold edge, no orphaned twin, consistent winding. Arc-specific: `sweep == 360` byte-identical to the full primitive; `Wall` seams manifold and planar; `Open` valid as manifold-with-boundary. **Plus a regression guard: default-parameter meshes are byte-identical to pre-change output** (the uv_sphere winding tests at builders.rs:99 must stay green).

*Exit: kernel fully parametric (minus `RoundSpec`); app behavior unchanged.*

### Gate 0b — `RoundSpec` (§3.2), separable
**Read `BEVEL.md` first; append to it when done.** Rim/corner rounding for Box + Cylinder via `bevel_op.rs` reuse. Tests: `radius == 0` byte-identical to un-rounded; clamped radius never self-intersects; segmented box (`seg > 1`) corners stay degree-3-clean; `validate()` green. **If the bevel interaction proves deeper than rim-edge selection, stop and report — do not block Gate 1.**

### Gate 1 — `PrimitiveSpec` as the single parameter type ✅ DONE 2026-08-04

`cargo build -p app` green; `cargo test -p core_math` 871 pass / 0 fail.

What landed:
- `PolyCreateAdjust` now carries `spec: PrimitiveSpec` — the flat
  `kind: String` + segments/vsegments/rings/radius/height/minor bag is **gone**.
- `poly_build_mesh` and its `match a.kind.as_str()` string dispatch **deleted**, along with the
  app-local `build_segmented_cylinder` / `build_segmented_cone` (kernel geometry that had been
  living in the app). All geometry now comes from `spec.build()`.
- New `app/src/poly/primitive_params_ui.rs` — the ONE slider widget, used by the F9 redo panel and
  ready for the Gate 2 properties block.
- `tool_params::poly_primitive()` (the static catalog) is now **derived from the spec defaults**
  instead of being a third hand-maintained copy of the parameter list.
- `poly_create_half_along_y` derives its half-extent from the spec instead of a parallel table of
  magic numbers, so a new primitive can no longer forget it and spawn half-buried.
- `spec_sides_to_remember()` replaces the `matches!(gen, "cylinder" | "cone")` string test that
  was repeated at five call sites.
- Circle + Ico Sphere added to **all five** entry points (`menus.rs`, `ui2/menus.rs`,
  `ui2/draw/placement.rs`, `ui2/tools/activate.rs`, `ops/poly/mesh_create.rs`) and to the Shift+A
  search filter.
- Three `stress_test` files (`instance`, `multi_mesh`, `dense`) were populating a full
  `PolyCreateAdjust` — a UI struct — purely to reach the mesh builder. They now call `spec.build()`
  directly.

Not yet done (Gate 2): parameters are still ephemeral. The F9 panel edits them live and commits one
undo entry, but nothing is stored on the node, so they still vanish when the panel dismisses.

Original scope, for reference:
1. New `core_math/src/primitive_spec.rs` — §2.1 types, `build()`, `label()`, clamps, `Default` per variant.
2. Rewrite `poly_build_mesh` callers to `spec.build()`; **delete** `poly_build_mesh` and the `kind: &str` dispatch. `create_poly_mesh(gen: &str)` becomes `create_poly_mesh(spec: PrimitiveSpec)`; update `ops/poly/mesh_create.rs:10-32`.
3. `PolyCreateAdjust` → `{ spec, placement }`. One shared `draw_primitive_spec_params`; derive the catalog from the enum.
4. Add Circle + Ico Sphere to **all five** entry points (§1.2). All five or none.
5. Unit tests: every `PrimitiveSpec` variant builds a valid mesh at default and at clamp boundaries.

*Exit: every parameter in §3 adjustable in the F9 panel. Still ephemeral. Independently shippable.*

### Gate 2 — `MeshSource` on the node (the real fix) ✅ DONE 2026-08-04

`cargo build -p app` green. `cargo test -p app` **536 pass / 0 fail**. `cargo test -p core_math`
**876 pass / 0 fail** (only the pre-existing `serde_golden` failure inherited from master remains).

**The design in §2.1 is unchanged — the PERSISTENCE mechanism changed, twice, for reasons worth
recording:**

1. The plan said "break the format, bump v7→v8". That turned out to be unnecessary *and*
   insufficient. `NodePayload::PolygonalBody`'s trailing field is `modifiers`, and bincode gives a
   trailing **enum-variant** field no `#[serde(default)]` rescue — so a naked `source` field would
   corrupt saves regardless of the version number. `source` is therefore `#[serde(skip)]`.
2. I first carried it in an **in-blob `#[serde(default)]`** field on `ProjectFile`, following the
   `poly_layers` / `curve_params` precedent. **This broke every committed `.fl4` fixture** with
   `Deserialize error: failed to fill whole buffer` (`dc_gate0_committed_ref_fl4_loads_unchanged`
   and `intrude_owner_fixtures_pinned` caught it). bincode is positional with no field names:
   `serde(default)` cannot rescue bytes that were never written. The precedent only works for files
   already saved with the field present.
3. Final: a **self-delimiting `FL4PPRM` tail section** (`project/io.rs`), written last,
   emitted only when at least one body is parametric. This is the format's actual documented
   extension mechanism — "the MAGIC trailing section, not the version number, is the
   format-extension signal". `CURRENT_SAVE_VERSION` stays **7**.

**Consequence: §7's dev-asset migration is CANCELLED — not deferred.** Nothing breaks. The bug
repros, the DC benchmark reference and `default_scene.fl4` all load untouched, and a pre-parametric
file simply has no section, so its poly bodies load as `Authored` — correct, since their geometry
never was derived from a spec.

**The invariant is enforced structurally, without touching ~80 call sites.** `MeshSource::Primitive`
stores `built_sig`, the `content_signature()` of the mesh the spec produced. `poly_live_spec(nid)`
returns the spec only if the mesh still matches. Any of the ~80 places that mutate a body's mesh
(bevel, boolean, knife, extrude, slide, …) invalidates the spec automatically, with zero
cooperation and zero tribal knowledge; the body simply stops being parametric, and the properties
panel says "Edited — no longer parametric" instead of offering sliders that would silently rebuild
the mesh over the user's work. Pinned by `an_edited_mesh_stops_being_parametric`.

What landed:
- `MeshSource` + `MeshSource::build()` (the only way to pair a spec with its signature).
- `PolygonalBody.source`, `#[serde(skip)]`; 24 construction sites updated (conversion / separate /
  symmetry / duplicate all correctly produce `Authored`).
- `app/src/poly/primitive_source.rs`: `poly_live_spec`, `poly_spec_went_stale`,
  `set_primitive_spec` (THE mutator), `poly_make_editable`,
  `poly_auto_apply_before_component_edit`, save/load record collection.
- `ProjectFile.poly_params` + `PolyParamRecord` + the `FL4PPRM` section; save wired in
  `project_save.rs`, load in `project_load.rs`.
- Properties panel block with live sliders (shared widget from Gate 1) + **Make Editable**.
- Auto-convert at **both** doors into component editing — `poly_switch_work_mode` (undoable, with a
  toast) and `poly_prefer_work_mode` (the bevel-arm / extrude / duplicate path).
- The F9 panel now routes its rebuild through `set_primitive_spec` instead of writing the mesh
  directly, so it can no longer leave the spec and mesh out of step.
- Tests: `poly_params_round_trip` (app), `mesh_source_build_pairs_spec_with_its_signature`,
  `an_edited_mesh_stops_being_parametric`, `authored_never_reports_a_spec`,
  `rebuilding_from_an_edited_spec_restores_liveness`, `mesh_source_round_trips_through_bincode`.

**Known limitation (deliberate, not a bug):** duplicating a parametric body produces an `Authored`
copy — `DupData::Poly` carries only name/mesh/material. Honest default; making duplicates stay
parametric is a small follow-up, not a correctness issue.

Original scope, for reference:
1. Add `source: MeshSource` to `NodePayload::PolygonalBody` (`payload.rs:129-136`). **This breaks the save format — that is authorized (§7).** Bump `ProjectFile` version 7 → 8.
2. `set_primitive_spec(nid, spec)` — the single spec mutator (§2.3). Live drag mutates with `RENDER`; one `transaction_snapshot_only` on release (existing `tool_params.rs:1141` / `poly_pipe.rs:211` pattern).
3. `create_poly_mesh` sets `source: Primitive(spec)`. Import/`from_mesh` paths set `Authored`.
4. Properties panel: when `source == Primitive`, show the params (same shared widget) + **Make Editable**. When `Authored`, show nothing new.
5. Component-edit auto-convert (§2.2), one transaction, status-bar note.
6. `debug_assert!` the §2.3 invariant in the display-sync path.
7. Load path: reject v7 with a clear error (post-migration), or run the v7 shim if §7 hasn't been executed yet.
8. Tests: save→load→spec preserved and still editable; undo of a spec change restores both spec and mesh; Make Editable is undoable; `serde_golden.rs` (:119, :295) regenerated for v8.

*Exit: sides/segments editable forever, survives save/load, fully undoable.*

### Gate 3 — Polish (only if asked)
Presets; per-primitive default memory (generalize the existing `ui.poly_create_last_sides`); on-canvas drag-to-set radius/height at creation.

---

## 5. Invariants / red lines (still in force)

- **`HalfEdgeMesh`'s serialized shape is still untouchable.** The format break authorized here is for `NodePayload::PolygonalBody` + `ProjectFile` version — **not** a license to retype/reorder/remove `HVertex`/`HFace`/`HalfEdge` fields or SoA-migrate them. That red line is about kernel correctness and the DC/GPU paths, not about old saves.
- `PrimitiveSpec`, `MeshSource`, `CapFill` are new serialized enums. Once Gate 2 lands and dev assets are migrated, **the append-only rule re-arms on them.** This is a one-time window, not a new policy — say so in the commit message.
- No new `NodePayload` variant (reuse index-6 `PolygonalBody`). No second trailing magic after `FL4ORDER`. No `#[serde(skip)]` on `PolygonalBody.mesh`.
- One geometry producer kernel-side (`builders.rs`), one spec-side (`PrimitiveSpec::build`), one spec mutator (`set_primitive_spec`). Two code paths for one primitive = automatic reject.
- Param edits mark `RENDER`. Only structural node add/remove marks `SCENE`. Never `SCENE` for a slider.
- Every user-visible slider has undo.
- Prefer ID-preserving half-edge construction over polygon-soup rebuilds; any op that could produce non-manifold or an orphaned twin must assert against it.
- **Fail closed, never fake:** if a param combo can't yield correct topology, clamp at the single entry point or refuse with a toast. Never emit a wrong-topology mesh, a placeholder shape, or a silently substituted primitive.

## 6. Flagged debt — NOT this task

`PolyModifier` (`node.rs:329`) is a flat god-struct holding every modifier type's fields simultaneously, with `#[serde(default)]` accretion (`bevel_shape` :355, `bool_op` :364, `pipe_path` :391-402). It should become per-variant payload structs exactly like `PrimitiveSpec`, and the format-break window is when that would be cheapest. **It is out of scope here** (scope control: one feature per prompt). File it in `MASTER_WORK_LIST.md` and decide separately, before the window closes.

## 7. Dev-asset migration (must be done in Gate 2, not deferred)

There are no customer saves, but there are dev assets that will not load under v8:

- `docs/planning/poly/repro/{knife_mess, knife_mess_bak, intrude_04, intrude_05, test-02..test-05}.fl4` — **bug repros for open tracked issues.** These capture specific broken topology that may not be reproducible by hand. Losing them loses the repro.
- `docs/planning/sdf/bench/dc_consolidation_ref.fl4` — DC consolidation benchmark reference.
- `core_engine/soak_out_*/soak_roundtrip.fl4` — regenerable, ignore.
- `%LOCALAPPDATA%\Azra\default_scene.fl4` — user's working scene.

**Procedure:** keep a v7 read-only shim (a `#[derive(Deserialize)] PolygonalBodyV7` shadow struct that maps to `source: Authored`) → load each asset above → re-save as v8 → verify each still reproduces its bug / benchmark → **then delete the shim in the same PR.** A versioned loader that exists only long enough to migrate is not debt; one that lingers untested is. Note the migration in `COMPLETE_BUG_TRACKER.md` next to the repro-linked bugs.

## 8. Definition of done (per gate)

1. `cargo build -p app` green. 2. `cargo test -p core_math` green. 3. All callers grepped and reported. 4. Undo + save/load verified. 5. One manual test scenario written down. 6. `COMPLETE_BUG_TRACKER.md` updated if any bug was touched.

**Manual scenario for Gate 2:** Shift+A → Poly Cylinder → F9 panel: sides 6, vsegments 4 → click elsewhere (panel dismisses) → reselect → Properties shows the Cylinder spec, still sides=6 → set 24, mesh updates live → Ctrl+Z restores 6 → save, quit, relaunch, load → still 24 and still editable → Tab into edit mode → status bar reports auto-convert to editable, Properties no longer shows spec params → Ctrl+Z → back to parametric with sides=24 → Make Editable → spec gone, topology fully editable.
