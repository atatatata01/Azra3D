# MODELING_TOOLS_SCOPE.md — AZRA Modeling Toolset: Full Scope + Live-Update Roadmap

**Date:** 2026-06-28 · **Owner:** modeling-tools · **Status:** living document
**Companions:** `MASTER_WORK_LIST.md` (priorities), `BEVEL_QUALITY_PLAN.md` (bevel — referenced, not re-planned here), `VIEWPORT_MODEL.md` (display invariant), `VOLUME_ADR.md` (volume path), `CLAUDE.md` (rules).

This file is the **scope of record** for "a complete modeling toolset with live, interactive update" across AZRA's 4-layer stack. It is concrete (file:line), opinionated on ordering, and honest about effort and the frozen-`half_edge` blocker. It supplements `MASTER_WORK_LIST.md`; it does not duplicate the bevel plan.

Grade legend: 🟢 production · 🟡 rough (exists, works, unpolished or partial) · 🔴 broken (exists, visibly defective) · ⚪ missing (no implementation).

---

## 1. North Star

**Goal:** AZRA reads as a credible modeling application — not a tech demo — on two axes the user ranks first: **completeness** (the table-stakes tool set per layer) AND **live update / interactivity** (edits update the surface continuously, mid-drag, with re-editable history). The structural bet is that AZRA's **SDF graph already delivers the non-destructive, re-editable live-history bar that Blender fakes with modifiers** — lean into that, and bring poly/voxel up to the same interactivity feel.

**The 4-layer stack (per `VIEWPORT_MODEL.md`, authoritative):** SDF → voxel → poly → triangles. Tools are layer-specific; "the toolset" is the union of three per-layer tool sets plus the cross-layer bake-down that lowers everything to triangles for Object Mode.

**Non-negotiable constraints every tool/roadmap item must honor (from `CLAUDE.md`):**

1. **Display Invariant** — one display technique per view; two techniques are NEVER fused over the same pixels. Object Mode = triangles only (SDF baked to mesh + voxel meshed + poly native). Edit Mode is per-object, single-technique. An SDF used inside a sculpt enters as a **boolean modifier that stamps the voxel density field**, never as a second renderer. No new tool may introduce a fused multi-object raymarch.
2. **`half_edge.rs` is HARD-FROZEN** (user directive 2026-06-14; `feedback_half_edge_hard_frozen.md`). All poly tool work is **app-layer only**, via the public API + `from_polygons()` rebuild. Any roadmap item needing core topology math is explicitly flagged BLOCKED and must build an app-layer `bevel_op.rs`-style replacement first.
3. **Transaction / undo discipline** — every user-visible mutation goes through `transaction()` (atomic) or `begin_episode()`/`commit_episode()` (drags). `save_undo()`/`save_delta_undo()` are deleted — never reintroduce. Sculpt is the documented exception (GPU-authority atlas + its own snapshot undo path).
4. **One shared code path for transforms** — gizmo drag, ALT+drag, and modal transform must route through `start_gizmo_drag(...)`. No second transform path. (CLAUDE.md "Two code paths for the same operation" = always a bandaid.)
5. **5-path SDF parity** — any SDF feature must land in WGSL analytical + GLSL BVH + GPU bake (and CPU eval where reachable) in the same task. Shader edits batch into ONE edit (25–43s cold recompile).
6. **Source of truth** — scene graph for SDF/poly; GPU sparse atlas for live sculpt strokes (the documented inversion). No flat-cache writes without graph sync.

**The bar, in one line:** every drag updates the surface every frame (not on release), every parameter stays re-editable, and switching mode/selection never stalls.

---

## 2. Current Toolset Inventory

Single source-of-truth table. Grade reflects the audits (2026-06-28). `file:line` is the primary implementation site. "Live" = does it update the surface continuously during interaction.

| Tool | Layer | Grade | Live | file:line | Note |
|---|---|---|---|---|---|
| **Primitive library** (box/sphere/cyl/cone/torus/capsule + stair/spiral/billboard/curve-extrude) | SDF | 🟢 | n/a | `core_math/src/sdf_shapes.rs:48-191`, `app/src/creation.rs:17-42` | 10 shapes; covers hard-surface. Stair/spiral/billboard 🟡 (clunky/unwired flags). |
| **Boolean — Union/Subtract/SmoothIntersect** | SDF | 🟢 | 🟢 | `core_math/src/sdf_math.rs:122-159`, `primitives.rs:44-53` | Per-prim `blend_k`, bounding-sphere cull, native re-editable. AZRA's strength. |
| **Boolean — hard Intersect** | SDF | 🟡 | 🟢 | `sdf_math.rs:153` | k=0 only; dragging k should auto-promote to SmoothIntersect, currently a manual enum switch. |
| **Blend profiles** (8: poly/chamfer/stairs/columns/groove/tongue/pipe/engrave) | SDF | 🟢 | 🟢 | `sdf_math.rs:49-66`, `modifiers.rs:98-112` | Full CPU+GPU. |
| **Blend falloff** (directional mask) | SDF | 🟢 | 🟢 | `modifiers.rs:40-61`, `primitives.rs:113` | Origin offset + length; evaluates in `eval_scene()`. |
| **Box/Cylinder bevel** (per-edge-group radius, profile, chamfer angle) | SDF | 🔴 | 🟢 | `sdf_shapes.rs:99`, `sdf_prim_eval.wgsl:418,506`, `volume_bake.wgsl:217,296` (+5 GLSL sites) | **NOTCH BUG** — hard-`max()` corner combine at mixed radii. P0 in `BEVEL_QUALITY_PLAN.md` Phase 0 (8 files, 11 sites). Profiles/angles themselves work. |
| **Per-edge radius UI** | SDF | ⚪ | n/a | `app/src/properties_sdf_shape.rs` | Data model ready (`radii[12]`); UI only shows groups. Pure UX gap. |
| **Sphere/Torus/Cone bevel UI** | SDF | ⚪ | n/a | `sdf_shapes.rs` | Only Box+Cylinder get bevel UI. Low priority. |
| **Modifier stack** (Twist/Taper/Smooth/Displace/Mirror/Shear/Sculpt/Spherize/Shell + volume masks) | SDF | 🟢 | 🟢 | `modifiers.rs:68-82`, `primitives.rs:143` | Ordered, CPU+GPU. Sculpt-modifier brush-picker UI hidden (🟡). |
| **Elongation / Subdivision / Clip curves / Instance sync / Material DNA** | SDF | 🟢 | 🟢 | `primitives.rs:122,150,188,193` | All modeled, serialized, wired. Clip-curve GPU uses first curve only. |
| **Gizmo — Translate/Rotate/Scale/Elongate, local/global** | SDF+poly | 🟢 | 🟢 | `app/src/gizmo.rs`, `input_viewport.rs:1115-1370` | Episode-backed, undo-enabled, multi-axis. Shared path. |
| **Nested sub-bodies** (`body_op` composition) | SDF | 🟡 | 🟡 | `scene_graph.rs:265-276,377-403` | Hierarchy solid; **GPU stack machine works, CPU eval stubbed** (`sdf_math.rs:118` TODO) — breaks snapping/picking when `body_op ≠ Union`. |
| **Shell / hollow / thickness** | SDF | 🟢 | 🟢 | `modifiers.rs` Shell; CurveExtrude `wall_t` | Shipped 2026-07 (audit). Spine-native shell still open. |
| **Offset surface** | SDF | 🟡 | 🟢 | Smooth modifier ≈ `d±k` | Works via Smooth; no first-class "Offset" label. |
| **Extrude face** | poly | 🟡 | 🟢 | `app/src/poly/extrude.rs:1-32`, `poly_drag_tools.rs:23-130` | Drag updates each frame. Missing individual/scale/along-edge modes. |
| **Inset face** | poly | 🟡 | ⚪ | `app/src/poly/inset.rs:1-30` | One-shot only. `PolyInsetDrag` state exists (`types_poly.rs:222`), no drag handler. No individual/thickness. |
| **Bevel edge** | poly | 🔴 | 🟡 | `app/src/poly/bevel.rs:1-3` (placeholder), `poly_drag_tools.rs:630-809`, `core_math/src/bevel_op.rs:73-81` (stub) | Single-segment chamfer; `_segments`/`_shape` ignored; corner notch + manifold tears. `bevel_edges_ex` frozen; `bevel_op.rs` v1 is a no-op clone. Drag writes flat cache (T1–T4 violation). See `BEVEL_QUALITY_PLAN.md`. |
| **Loop cut** | poly | 🟡 | ⚪ | `app/src/poly/loop_cut.rs:1-58` | N-cut works (even distribution). No preview, no slide-after-cut, no interactive count. |
| **Knife** | poly | 🟡 | 🟡 | `app/src/poly_knife.rs:1-250+` | Hover snap live. No path-line preview, no per-point undo. RMB confirms (per Tool Exit Convention). Points world-space (stale-on-pan risk). |
| **Bridge** | poly | 🟡 | ⚪ | `app/src/poly/bridge.rs` | 1–2 vert/edge works, manifold-tested. No loop-select, no subdivisions, no twist. |
| **Subdivide (Catmull-Clark)** | poly | 🟡 | ⚪ | `app/src/poly/subdivide.rs:1-36` | 1-pass only; no segments UI, no smooth groups. |
| **Dissolve / Merge / Fill / Delete** | poly | 🟡 | ⚪ | `poly/dissolve.rs`, `merge.rs`, `fill.rs`, `delete.rs` | One-shot, minimal validation. No dissolve-verts, merge-by-distance, delete-verts. |
| **Selection (vert/edge/face)** | poly | 🟢 | 🟢 | `poly/selection.rs`, `input_select.rs`, `types_poly.rs:82-124` | GPU picking + CPU fallback + screen-space cache. 1/2/3 mode keys. |
| **Transforms G/R/S on sub-elements** | poly | 🟡 | 🟢 | `app/src/transform.rs`, `gizmo.rs` | Works via gizmo. No snapping, no visible axis-constraint affordance. |
| **Proportional / soft selection** | poly | 🟡 | ❓ | `main.rs:1067-1069` | Fields exist (`poly_proportional`/`prop_radius`/`prop_falloff`); UI exposure + testing unknown. |
| **Spin / Screw (lathe)** | poly | ⚪ | n/a | — | Classic missing tool. |
| **Symmetrize / Mirror (poly)** | poly | 🟢 | n/a | `half_edge.rs:831-989` (frozen, usable via API) | Works; needs discoverable UI. |
| **Clip-curve draw** | SDF (cross) | 🟡 | 🟢 | `app/src/clip_curve_tool.rs:1-150+` | Cursor preview live. SDF-only; not available for poly. |
| **Voxel brushes** (Draw/Erase/Smooth/Flatten/Clay/ClayBuildup/Scrape/Crease/Fill/Inflate/Pinch) | voxel | 🟢* | 🟢 | `core_math/src/voxel_sculpt.rs:19-46`, `graphics/src/voxel_brush.wgsl:91-164` | 11 brushes, CPU+GPU matching math, falloff+inverted. *Math 🟢 but **brush-picker UI 🔴 missing** — biggest single blocker. |
| **Grab / Move / Snake-Hook brush** | voxel | ⚪ | n/a | — | ZBrush standard; not in enum. |
| **Falloff variety** | voxel | ⚪ | n/a | `sculpt_brush.rs:240` | Only `Smooth` (`smoothstep01`). No Linear/Sharp/S-curve. |
| **Brush cursor preview** | voxel | ⚪ | n/a | — | No on-screen radius/falloff ring. |
| **Live update — mesh (chunked Surface Nets)** | voxel | 🟢 | 🟢 | `voxel_gpu_mesh.rs:224-329`, `voxel_mesh.wgsl` | GPU async dispatch, batched indirect draw. Solves the 1024³ draw-call wall. |
| **Live update — brush GPU atlas** | voxel | 🟡 | 🟡 | `input_sculpt.rs:1030,1854`, `voxel_brush.wgsl` | Box path proven; sparse atlas path experimental (sync bookkeeping, undo caution). |
| **Remesh (Ctrl+R)** | voxel | 🟢 | 🟢 | `input_sculpt.rs:366-410`, `voxel_sculpt.rs:1335-84` | Async, atomic swap, trilinear + surface-chunk skip. UX minimal. |
| **Undo/redo (sculpt)** | voxel | 🟢 | n/a | `voxel_sculpt.rs:62-127`, `input_sculpt.rs:136-166` | Sparse per-brick masks + atlas snapshots + eviction. |
| **Voxel primitives (sphere/box)** | voxel | 🟡 | n/a | `voxel_sculpt.rs:1306-29` | Only 2 shapes; res hardcoded 64³ (`input_sculpt.rs:261`); no creation picker. |
| **Voxel material / shading** | voxel | 🔴 | n/a | `input_sculpt.rs:291-299`, `properties_sdf_material.rs` | Hardcoded clay; full PBR editor **ignored** by voxel mesh. P0.6. |
| **SDF→Voxel boolean stamp** | cross | 🟢 | n/a | body props Stamp/Carve + `fill_from_sdf` | Shipped (2026-07 audit). |
| **Voxel→Poly bake (Surface Nets)** | cross | 🟢 | n/a | `voxel_sculpt.rs:1895-1956` | Watertight quad mesh → single `ImportedMesh` node. No UV unwrap yet. |
| **SDF→Triangles bake (DC, Object Mode)** | cross | 🟡 | n/a | `app_poll.rs` (`dispatch_grid_dc_async`) | Async DC mesh; appears next frame. |
| **Snapping (grid/vert/edge/face/increment)** | universal | 🟢 | 🟢 | `smart_snap.rs` + `snapping.rs` | Smart Snap magnet+Ctrl; V/E/F/G/C; rotated SDF candidates (2026-07-20). Polish dual-grid residual only. |
| **Workplane / custom transform orientation / pivot** | universal | 🟡 | n/a | `scene.pivot` | Partial. Modo-style action-center/workplane missing. |
| **Array / radial array modifier** | universal | ⚪ | n/a | — | Classic non-destructive op, missing. |
| **Duplicate / Delete (object)** | universal | 🟢 | n/a | `scene_actions.rs` (`duplicate_subtree`, `delete_prim`/`delete_body`) | Canonical, reuse-enforced. |

---

## 3. Live-Update / Interactivity Assessment (the user's stated priority)

The question per layer: **do edits update the surface continuously during interaction, or do they lag / appear-on-release / break?**

### 3.1 SDF layer — 🟡 responsive, ~1-frame latency
- **Architecture is built for live.** Volume-only Edit Mode bakes **every frame at Critical priority** during drag (`CLAUDE.md` "Bake Dispatch During Interaction"; `suppress_bake_dispatch` is NOT set in volume-only). Partial bakes hit only the dirty sub-region (~9–11% of voxels); debounce is bypassed when dirty regions exist.
- **The honest latency:** async GPU pipeline = bake dispatch → `map_async` readback (polled once/frame) → texture upload → render. Net **single-frame-late** (Frame N drag → N+1 GPU+upload → N+2 visible). Draft 64³ bakes during drag, full res on release.
- **Re-editable history = 🟢 genuine strength.** The SDF graph IS a live parametric stack: change a parameter on an old prim/boolean and the whole body re-bakes. This is the bar Blender fakes with modifiers, and AZRA has it natively.
- **Gaps:** 1-frame latency (acceptable for a volumetric modeler, not pixel-instant CAD); CPU nested-body eval stubbed (snapping/picking wrong when `body_op ≠ Union`); blend-k on hard Intersect doesn't auto-promote.

### 3.2 Poly layer — 🟡 partial; "apply then see" more than "preview then commit"
- **What's live:** extrude drag (`poly_drag_tools.rs:61-130`), bevel drag (but **wrong pattern** — flat-cache write, `poly_drag_tools.rs:709-721`, T1–T4 violation), knife hover-snap. `DirtyFlags::RENDER` marks the screen each frame.
- **What's one-shot (appears only on confirm):** inset, loop cut, bridge, subdivide, dissolve, merge, fill, delete. No before-confirm preview for any of them. No loop-cut count scroll, no knife path-line, no on-object handles.
- **Modal numeric override mid-drag (type 0.2, scroll segments):** depends on the input-system migration (`MASTER_WORK_LIST` P1a), which is half-done — this is where poly feels roughest.
- **Per-frame cost risk:** interactive drags `mesh.clone()` each frame; at thousands of faces, stacking inset+bevel+loop-cut interactive drags could stutter. Mitigated today because only one tool drags at a time and it releases on confirm.
- **No re-editable poly history** (no modifier/MeshOps stack) — every op is destructive. ⚪.

### 3.3 Voxel layer — 🟢 mesh live, 🔴 hidden behind missing UI
- **Mesh update is genuinely live and fast:** 16³-cell chunked Surface Nets, GPU async dispatch, one batched indirect draw. Re-mesh cost is proportional to the brush, not the volume.
- **Stroke latency:** raycast is 1 frame late (reads previous-frame g-buffer); mesh lags brush by 1–2 frames at high res; chunk-boundary seams visible during fast strokes (`input_sculpt.rs:50-53`).
- **The "stroke appears only on release" class is fixed** — atlas writes `mark_dirty(DirtyFlags::RENDER)` so the raymarch re-dispatches that frame (`CLAUDE.md` Voxel Sculpt rules; was `input_sculpt.rs:1793`).
- **But the entire layer is gated behind a missing brush-picker panel** — the user cannot select a brush or set radius/strength. The interactivity is real; it's just unreachable. This is the #1 ROI fix in the whole codebase.
- **Material is dead (🔴):** voxel mesh renders hardcoded clay, ignoring the PBR editor (P0.6).

### 3.4 Cross-layer / mode-switch — 🟡 actively hardened, regresses easily
- Tab into edit / vert-edge-face switch / isolate must be instant. AZRA's recent history is **littered with "select freeze" fixes** (commits `e9bfe25`, `44b22d3`, `1a7f4bd`) — resident-buffer validity, pool-hit body switch. This area works now but is a known regression magnet; every render-pipeline change must re-test selection (CLAUDE.md: "Selection must ALWAYS work").

**Verdict:** Completeness of *inventory* is ~75%; **live-update feel is ~50%** — strongest in SDF (architecture built for it + native history), weakest in poly (mostly one-shot, half-migrated input), and best-but-hidden in voxel.

---

## 4. Gap Analysis vs Reference (Blender / Plasticity / Modo / ZBrush)

Prioritized by **(value × use-frequency) ÷ effort**, honoring constraints. Bevel deferred to `BEVEL_QUALITY_PLAN.md`.

### Tier A — Highest ROI (cheap, unblocks already-built work)
1. **Voxel brush UI panel** — 11 brushes already exist; user can't pick one. ~3–4h panel work. 🔴→🟢 for the whole sculpt layer. (P2.1)
2. **SDF Box-bevel notch fix** — P0 correctness across 8 files/11 sites. See `BEVEL_QUALITY_PLAN.md` Phase 0. ~1 focused day. Gates all bevel credibility.
3. **SDF Shell + Offset** — `abs(d)-t` and `d-k`, near-free field ops, Plasticity-marquee, make the SDF layer feel CAD-grade. Must land in all 5 paths (parity rule).
4. **Voxel material wiring (P0.6)** — read material from the voxel node, feed G-buffer from the display mesh. ~2h. Turns clay-blobs into textured sculpts.
5. **SDF→Voxel boolean stamp (P2.4)** — `fill_from_sdf()` (~20 lines) + a "Stamp SDF" button. Display-Invariant-blessed (SDF stamps density, no second renderer). The differentiator.

### Tier B — Interactivity bedrock (force-multipliers)
6. **Finish input-system migration (P1a)** — every modal/numeric-override interaction (type-a-value mid-drag, scroll-segments) depends on it. Bedrock for "live feel."
7. **Snapping + workplane/custom-orientation** — the biggest *universal* gap; multiplies the value of every transform and tool across every layer.
8. **Per-edge radius UI (SDF)** — data model ready (`radii[12]`); pure UX. Zero shader cost. `BEVEL_QUALITY_PLAN.md` Phase 2.

### Tier C — Poly quality (turn 🟡→🟢, mostly app-layer)
9. **Interactive previews** for inset (drag handler exists in state, no code), loop-cut (count scroll + preview), knife (path-line + per-point undo). App-layer; no `half_edge` change.
10. **Fix poly bevel transaction pattern** — route drag through `transaction()` + `mirror_prim_to_graph()`, not flat-cache writes (T1–T4).
11. **Poly bevel v1** (multi-segment + corner-fill) — **BLOCKED** on geometry: needs app-layer `bevel_op.rs::bevel_edges_offset()` body (currently a no-op clone), since `half_edge.rs::bevel_edges_ex` is frozen. Multi-day. `BEVEL_QUALITY_PLAN.md` Phase 3.

### Tier D — Genuinely-missing classics
12. **Spin / Screw (lathe)** — clearest missing poly tool.
13. **Array / radial-array modifier** — clearest missing non-destructive op.
14. **CPU nested-body SDF eval** — fixes snapping/picking when `body_op ≠ Union` (GPU already correct).
15. Voxel polish: Grab/Move brush, falloff variety, cursor preview, tablet pressure, multi-shape primitives + resolution picker (P2.2/P2.3).

---

## 5. Prioritized Roadmap (gated phases)

Ordering principle: **fix interactivity/live-update and the highest-use rough tools first; lowest-risk-highest-value before geometry-heavy work.** Each phase: goal · layer · files · risk · verify. Frozen-`half_edge` items flagged.

### Phase 0 — Unlock what's already built (days, low risk)
- **0.1 Voxel brush UI panel.** Goal: brush picker + radius/strength/falloff/lazy/symmetry sliders + primitive type/res + Remesh button. Layer: voxel. Files: `app/src/ui_panels.rs`, `app/src/input_sculpt.rs`, state at `main.rs:883-915`. Risk: low (state wired, panel scaffolding exists). Verify: pick each of 11 brushes, sculpt, confirm live mesh update + undo.
- **0.2 SDF Box-bevel notch fix.** Per `BEVEL_QUALITY_PLAN.md` Phase 0 — replace hard-`max()` corner combine with exact `sdRoundBox` (uniform octant) / smooth `smax` (mixed) across all 8 files/11 sites. Layer: SDF. Risk: **medium** (shader recompile 25–43s, 5-path parity, batch into ONE edit per file). Verify: mixed-radius box corner shows no crease/normal-flip under reflection; `cargo test -p graphics` (35 Naga parse tests).
- **0.3 Voxel material wiring (P0.6).** Goal: voxel mesh reads its material node, feeds real PBR G-buffer channels. Layer: voxel. Files: `properties_sdf_material.rs`, voxel display-mesh material path, `input_sculpt.rs:291-299`. Risk: low–medium (must respect Display Invariant — raster mesh, not raymarch). Verify: change albedo/rough/metal on a sculpt, see it shade under lights.

### Phase 1 — Cheap CAD-grade SDF wins + the differentiator (days, parity-gated)
- **1.1 SDF Shell + Offset.** Goal: `abs(d)-t` (shell) and `d-k` (offset) as modifiers/ops with sliders. Layer: SDF. Files: `modifiers.rs`, `sdf_prim_eval.wgsl`, `volume_bake.wgsl`, GLSL `sdf_functions.glsl` + `*_full.comp`, CPU `field_eval.rs` — **all 5 paths, one task** (parity rule). Risk: medium (shader recompile + parity + `.spv` recompile). Verify: shell a box → hollow wall of thickness t; offset a union → uniformly grown surface; identical in viewport, bake, and PT.
- **1.2 SDF→Voxel boolean stamp (P2.4).** Goal: `fill_from_sdf(sdf_fn)` (~20 lines) + "Stamp SDF" button stamping an SDF prim as +/- density into the active sculpt. Layer: cross (SDF→voxel). Files: `voxel_sculpt.rs`, sculpt panel. Risk: low (Display-Invariant-blessed: density stamp, no second renderer). Verify: stamp a sphere into a sculpt, see additive/subtractive volume, undo.
- **1.3 Per-edge radius UI (SDF).** Goal: expose `radii[12]` behind an "Advanced" collapsible. Layer: SDF. Files: `properties_sdf_shape.rs`. Risk: low (zero shader cost, data ready). Verify: set 3 different edge radii on one box, see them, save/load round-trips.

### Phase 2 — Interactivity bedrock (weeks, medium risk)
- **2.1 Finish input-system migration (P1a).** Goal: modal numeric override + axis keys + segment scroll mid-drag, all via `InputSystem`. Layer: all. Files: `input_system.rs` + tool dispatch sites. Risk: medium (touches every interactive tool; migrate gradually per CLAUDE.md). Verify: extrude → drag → type `0.2` → Enter; no tool drop.
- **2.2 Snapping + workplane/custom-orientation.** Goal: grid/vert/edge/face/increment snap + a workplane and custom transform orientation. Layer: universal. Files: new snap module, `gizmo.rs`, `transform.rs`, `scene.pivot`. Risk: medium–high (pervasive; must share the one transform path). Verify: snap a vert to another vert; set workplane from a face; transforms originate there.

### Phase 3 — Poly quality (weeks; app-layer first, geometry flagged)
- **3.1 Interactive previews + correct transactions (app-layer).** Goal: inset drag handler, loop-cut count-scroll + preview, knife path-line + per-point undo; route poly bevel drag through `transaction()`/`mirror_prim_to_graph()` (kill T1–T4). Layer: poly. Files: `poly_drag_tools.rs`, `poly/inset.rs`, `poly/loop_cut.rs`, `poly_knife.rs`. Risk: low–medium (**no `half_edge` change**; per-frame `clone()` cost — cap work). Verify: each tool previews before confirm; cancel restores; one undo entry per op.
- **3.2 Poly bevel v1 — multi-segment + corner-fill (FROZEN-BLOCKED).** Goal: real bevel via app-layer `bevel_op.rs::bevel_edges_offset()` (currently a no-op clone) + `WeldMap`, rebuilt through `from_polygons()`. Layer: poly/core_math (app-layer only). Files: `core_math/src/bevel_op.rs`, `poly_drag_tools.rs:630-809`. Risk: **high** — geometry work, multi-day, blocked behind the frozen-`half_edge` constraint (must NOT edit `half_edge.rs`). `BEVEL_QUALITY_PLAN.md` Phase 3. Verify: 3-segment bevel on a cube corner, manifold, no notch/tear.
- **3.3 Missing classics:** Spin/Screw (lathe), Array/radial-array modifier. Layer: poly/universal. Risk: medium (app-layer via `from_polygons()` + instancing). Verify: lathe a profile into a revolved solid; array N copies non-destructively.

### Phase 4 — Polish & parity (ongoing)
- CPU nested-body SDF eval (fix snapping/picking for `body_op ≠ Union`, `sdf_math.rs:118`).
- Voxel: Grab/Move brush, falloff variety, cursor preview, tablet pressure, multi-shape primitives + res picker (P2.2/P2.3).
- Sphere/Torus/Cone bevel UI; blend-k auto-promote on hard Intersect; billboard face-camera wiring.

**Recommended execution order (flat):** 0.1 → 0.3 → 0.2 → 1.2 → 1.3 → 1.1 → 2.1 → 2.2 → 3.1 → 3.3 → 3.2 → Phase 4. Rationale: ship the zero-geometry unlocks and the differentiator first; do the risky shader/geometry work (0.2, 1.1, 3.2) only when batched and parity-verified.

---

## 6. Risks & Invariants

- **Frozen `half_edge.rs` (hard).** Any poly tool needing topology math is BLOCKED until an app-layer replacement exists (the `bevel_op.rs` pattern: public API + `WeldMap` + `from_polygons()` rebuild). Flagged items: **3.2 poly bevel v1** (the live blocker), future slide/individual-extrude/individual-inset. Never edit `half_edge.rs` — not even "a one-liner." Mesh ops must be topologically clean or the raster cache desyncs.
- **5-path SDF parity.** Shell/Offset/notch-fix/any SDF feature must land in WGSL analytical + GLSL BVH + GPU bake (+ CPU eval where reachable) **in the same task**. Missing a path = a feature that works in viewport but not in bake/PT. After GLSL edits, recompile `.spv` in the same task; run `cargo test -p graphics`.
- **Shader recompile cost.** Any edit to the 5 `sdf_*` files invalidates 4+ pipeline cache entries → 25–43s cold recompile. **Batch all changes into ONE edit per file.** WGSL must call `neutralize_blend_noise_wgsl()` (noisy blend is GLSL-only) and respect `#region` tag discipline (dangling refs = parse error). `tw!()` only for passes that actually dispatch (timestamp-query device-loss).
- **Undo / save-format.** Every user-visible slider/toggle needs undo via `transaction()`/episode. Sculpt is the documented exception (atlas GPU-authority + its own snapshot path). Save format v6; **bincode enum variants append-only** — new `BooleanOp`/`Modifier`/`NodePayload` variants go at the END (mid-list insert corrupts every save). `VoxelBody` stays last.
- **Display Invariant.** No new tool may fuse two render techniques over the same pixels. SDF-in-sculpt = density stamp (1.2), never a second renderer. Object Mode is triangles-only.
- **Transform path unity.** Gizmo / ALT-drag / modal transform must share `start_gizmo_drag(...)`. Snapping (2.2) and on-object handles (Phase 4) extend that path; they do not add a second one.
- **Selection-freeze regression magnet.** Mode/selection switching has repeatedly regressed (`e9bfe25`, `44b22d3`, `1a7f4bd`). Any render-pipeline or resident-buffer change must re-test click-to-select and Tab-into-edit latency (P0 per CLAUDE.md).
- **Per-frame interactive `clone()` cost.** Poly interactive drags clone the mesh each frame; multiple simultaneous interactive tools at high face counts risk stutter. Cap per-frame work (Frame Budget Rule); release on confirm.
