# RENDER ARCHITECTURE — Compartmentalized Viewports

**Status:** Design (approved direction 2026-06-14). Supersedes the single "Hybrid" renderer.
**Owner decision log** at the bottom. Read this before any render-pipeline change.

---

## 1. Context — why we're re-architecting

Today there is **one** viewport renderer (`app/src/viewport_render.rs`, the "Hybrid"
path). Every frame it tries to be *all techs at once*: SDF analytical raymarch (lite / march
/ full), volume raymarch, GLSL BVH, the path tracer, the raster mesh pass, and a shared
deferred + post stack — gated by a thicket of flags (`force_volume_only`, `viewport_mode`,
`use_lite`, `render_mode`, `bvh_enabled`…). They all share **one G-buffer**.

Every voxel rendering bug we hit was a *consequence of this fusion*, not a voxel bug:

| Symptom | Real cause |
|---|---|
| Voxel black in Lit/Solid | SDF light-linking / SDF shadow-AO run on a raster pixel |
| Path-trace **crashes the viewport** (GPU TDR) | PT dispatched on an empty-SDF scene |
| Background **flickers** when orbiting | raster pass + SDF march race over one G-buffer |
| **Slow** at 192³ | a full-screen raymarch of an *empty* SDF scene every frame |

**The root cause:** SDF renders by *marching math*; everything else renders *triangles*.
Forcing those two fundamentally different render models through one pipeline creates an
impedance mismatch we keep patching. The fix is to **compartmentalize**: give each tech the
viewport it deserves, and move finished work between them by **baking to meshes**.

> Reference target: 3D-Coat shows voxel + surface together with no trouble, and has a fast
> read-only render/assembly view. Blender/Unreal viewports stay fast with millions of polys.
> We want that — by *not* asking one renderer to do procedural marching + interactive
> high-poly raster + large-scene raster simultaneously.

---

## 2. Principles

1. **One render model per viewport.** A viewport is either "march SDF math" or "raster
   triangles" — never both fused.
2. **SDF is quarantined.** It is the only march-based tech and the source of the bug class,
   so it lives alone in its own editing viewport.
3. **Triangles coexist freely.** Voxel sculpts and surface (poly/NURBS/imported) meshes are
   all triangles → they share viewports with no special-casing (this is the 3D-Coat behavior
   the user wants).
4. **Editing viewports are domain-specific and interactive; the Assembly viewport is
   read-only and fast.** Don't make the fast large-scene renderer also carry edit tooling.
5. **Cross-domain transfer is by BAKING.** Finish an SDF body → Dual-Contour it to a mesh.
   Finish a voxel sculpt → Surface-Nets it to a mesh. Those meshes flow to Assembly.
6. **One scene graph, the source of truth.** Each node already carries its domain
   (`SdfBody` / `ImportedMesh` / `PolygonalBody` / `NurbsBody`). Viewports are *views* over
   the same graph, filtered + rendered by domain.
7. **Each renderer owns its pipelines and its G-buffer contract.** No shared gates, no
   cross-tech flags. A bug in one renderer cannot manifest in another.

---

## 3. The three viewports

A shared **shell** (camera, navigation, gizmos, selection, final tonemap + present, egui UI)
wraps exactly **one active domain renderer**, chosen by the current room (`WorkMode` /
`ModelingContext`).

### 3.1 SDF viewport (procedural editing) — *quarantined*
- **Edits:** `SdfBody` nodes (CSG, blends, booleans, the procedural modeling).
- **Tech:** the existing raymarch / volume / GLSL-BVH / path-tracer machinery — *unchanged*,
  but now it ONLY ever runs here, and ONLY ever sees SDF prims. No raster meshes composited
  in, no "what if there are 0 prims" edge cases.
- **Output → Assembly:** `dispatch_grid_dc_async` (Dual Contouring, already exists) bakes the
  SDF body to a triangle mesh when you "send to assembly" / leave the room.
- **Optimized for:** analytic beauty (smooth shapes, real PT, soft shadows, AO).

**Scaling considerations — volume cache (leave room; mostly already mitigated).** The SDF
viewport inherits the `BodyVolumeCache` machinery (full ownership table in **VOLUME_ADR.md**).
Three large-hierarchy risks, reconciled with what exists today:
- **Nested invalidation → want subtree-dirty.** In-body edits already bake *dirty regions*
  (`bake_body_grid_region`, O(dirty voxels), not O(res³)). The gap is *nested* bodies: editing
  a child currently dirties the **whole parent**, so a tiny Prop could rebake a huge Ship hull.
  *Genuine future work — keep the dirty model open to **subtree / region-propagated dirty**.*
- **Composite rebuild — already avoided in mainline.** `rebuild_composite()` is
  O(N_bodies × res³) (~667 ms @ 10 bodies) BUT is **skipped when `use_body_vol_cache=true`**
  (the default): the app renders from **per-body** caches + a GPU `ResidentVolume` pool, never a
  rebuilt composite (composite is legacy SVO-only). So it does **not** scale with body count in
  mainline. If per-body dense grids get heavy, the path is the **sparse brick pool**
  (`graphics::brick_pool`, exists but PARKED — `BRICK_CACHE_TIERS_PLAN.md`), not a composite.
- **Memory + eviction — budgets + LRU already exist.** GPU `ResidentVolume` pool = LRU eviction +
  freelist; RAM cache tier = 2 GB budget; brick pool = 256 MB budget. Per-body dense grids
  (8 MB @128³) are the pressure at 100+ bodies → levers are the existing budgets/LRU + the parked
  brick pool. *Un-park the sparse brick pool before artists hit the wall — they will make 100+
  bodies without hesitation — rather than deferring it to a late phase.*

### 3.2 Sculpt / Surface viewport (high-poly editing) — *voxel + surface together*
- **Edits:** `ImportedMesh` (voxel sculpts), `PolygonalBody`, `NurbsBody`, imported meshes —
  all triangle meshes, coexisting (the 3D-Coat experience).
- **Tech:** **pure raster** → G-buffer → deferred shade → post. NO SDF compute, NO PT, NO
  volume. Shading modes: **MatCap** (the sculpt look), Clay/PBR, Lit, Solid, Wireframe.
- **Optimized for:** one (or few) **huge, rapidly-changing** meshes — chunked incremental
  meshing + persistent GPU buffers + partial upload (the 3D-Coat-perf work). This is where
  millions of editable polys live.
- **Output → Assembly:** the mesh is already a mesh; it just appears there read-only.

> **Decision (Q3 2026 revision):** the Sculpt viewport MAY raymarch its OWN resident voxel atlas
> during active strokes (isolated to the sculpt atlas; never fused with the analytical SDF/BVH/PT
> path), overriding the earlier sculpt=raster-only rule — to make 1024³ render cost independent of
> poly count. The raymarch is an additive G-buffer overlay pass (Plan B1, `sculpt_voxel_raymarch.wgsl`)
> that reads ONLY the sculpt's sparse atlas + camera + the G-buffer; the triangle mesher still runs on
> idle/release for export, UVs, physics, and the Assembly viewport. Isolation is the mitigation for the
> quarantined fused-march bug class (§0 of `PLAN_SCULPT_RAYMARCH.md`).

### 3.3 Assembly viewport (read-only, large scene) — *Unreal/Blender-fast*
- **Shows:** everything, as baked/optimized triangle meshes — DC'd SDF bodies, voxel sculpts,
  poly/NURBS, imports. **Nothing is editable here.**
- **Tech:** a clean PBR raster scene renderer — **instancing, frustum + occlusion culling,
  LOD, shadow maps, full beauty post**. Built for *many objects, millions of polys, high FPS*.
- **Optimized for:** scale + framerate (layout, look-dev, final composition, fast project
  preview). The "feels super fast" viewport.

| | SDF | Sculpt/Surface | Assembly |
|---|---|---|---|
| Render model | march math | raster | raster |
| Editable | yes (SDF) | yes (meshes) | **no** |
| Domains | `SdfBody` | `ImportedMesh`,`PolygonalBody`,`NurbsBody` | all (baked) |
| Scale focus | few bodies | 1 huge mesh | many objects |
| Heavy tech | PT / volume / BVH | chunked mesh + partial upload | instancing / cull / LOD |
| Bug surface | isolated | isolated | isolated |

---

## 4. Shared shell (common to all viewports)

These are NOT duplicated per renderer — they live in the shell and call into the active
renderer:
- **Camera / navigation / workplane** (`viewport.camera`, orbit, ortho).
- **Gizmos, selection, picking, overlays** (transform gizmo, outliner sync, brush ring).
- **Final present:** every renderer produces one **HDR color texture**; the shell runs the
  shared **tonemap → egui texture → viewport rect** (today's `overlay_texture`/tonemap path).
- **Input + tools**, undo/transaction model, the scene graph.
- **Renderer selection:** `active_renderer = match room { SdfRoom → Sdf, SculptRoom/PolyRoom →
  SculptSurface, AssemblyRoom → Assembly }`. One enum, one switch.

The contract each renderer satisfies: *given the scene graph + camera + a target size,
produce a final HDR texture (and a depth/picking buffer for selection).* Nothing else.

---

## 5. Data model & baking pipeline

- **Scene graph stays the single source of truth.** Every object already has a domain node
  type. A renderer simply **filters** the graph to its domain(s) and draws.
- **Each editable domain has a "baked mesh" representation** consumed by Assembly:
  - `SdfBody` → `dispatch_grid_dc_async` (Dual Contouring) → triangle mesh in `mesh_cache`. *(exists)*
  - Voxel (`ImportedMesh` + density grid) → `VoxelMeshCache` Surface-Nets → mesh. *(exists)*
  - `PolygonalBody` / `NurbsBody` → already meshes / tessellated. *(exists)*
- **"Send to Assembly" / leaving an edit room** triggers (or refreshes) the bake. The baked
  mesh is tagged with its source node so edits can round-trip (re-enter the edit room → edit
  the live representation → re-bake).
- **Assembly never re-derives geometry** — it only consumes baked meshes (that's why it's
  fast and read-only).

Open question (§9): live vs. on-demand bake (do edits stream to Assembly, or only on an
explicit "apply"?). Recommended: on-demand + an optional "live ghost" (cheap).

---

## 6. Mapping to the current code

| New concept | Built from today's code |
|---|---|
| Shell present | the existing HDR→tonemap→egui path in `viewport_render.rs` |
| Renderer switch | extends `WorkMode`/`ModelingContext`/`ViewportRenderMode` (add an `AppRoom`/renderer enum) |
| SDF renderer | the existing Hybrid path, **stripped** to SDF-only (remove raster-mesh + the 0-prim edge cases) |
| Sculpt/Surface renderer | the existing `dispatch_raster_pass` + deferred, **stripped** to raster-only (no SDF compute dispatch) + the voxel mesher + chunked upload |
| Assembly renderer | a new read-only raster scene path reusing the raster pipeline + deferred + post, with instancing/cull/LOD added |
| SDF→mesh bake | `dispatch_grid_dc_async` (DC) — already there |
| voxel→mesh | `VoxelMeshCache` — already there |
| mesh upload | `render/mesh_cache.rs` + `mesh_gpu.rs` (gets the persistent-buffer / partial-upload work) |

**Net:** we are not writing three renderers from scratch — we are **splitting one fused
renderer into three focused ones** and adding the perf work where it belongs.

---

## 7. Migration plan (phased, each phase shippable, lowest-risk first)

### Phase 1 — Sculpt/Surface viewport (raster-only) ⭐ biggest immediate win
Carve a dedicated **raster-only** render path selected when in the Sculpt/Surface room. It
dispatches *only* raster → deferred → post; it never touches SDF compute / PT / volume.
- **Fixes structurally:** voxel black-in-modes, PT crash, bg flicker, empty-march lag — they
  cannot occur because the SDF machinery is not dispatched.
- **Home for perf:** chunked meshing + persistent GPU buffers + partial upload land here → 3D-Coat poly counts.
- **Risk:** LOW — additive new path behind a room check; the existing Hybrid stays for SDF.
- **Replaces** the interim patches (`nv==0` PT gate, force-lite) with the real structural fix.

### Phase 2 — Assembly viewport (read-only, fast scene)
A read-only raster scene renderer: gather all baked meshes, instancing + frustum cull + LOD +
shadow maps + full post. Built for many objects at high FPS. Shares the raster core with
Phase 1 (editable=false, scaled for object count).

### Phase 3 — Quarantine the SDF viewport
Make the existing Hybrid/FullSdf path the **SDF-only** editing renderer (active only in the
SDF room). Remove the raster-mesh compositing and every `nv==0`/raster special-case from it —
raster meshes now live in Sculpt/Assembly. SDF bodies bake to meshes (DC) for Assembly.

### Phase 4 — Unify the raster core + formalize baking
Extract the shared raster renderer used by Sculpt/Surface (editable) and Assembly (read-only).
Formalize the bake pipeline + round-trip tagging. Delete the dead fused-path code + flags.

---

## 8. Performance notes (where the 3D-Coat-class work lives)
- **Sculpt/Surface + Assembly raster renderers** own:
  - **P-A persistent GPU buffers + partial upload** — stop `create_buffer_init` every frame
    (`mesh_gpu.rs:83`); capacity buffers + `queue.write_buffer`, grow-only.
  - **P-B chunked meshing** — split the voxel grid into chunks; re-mesh + re-upload only the
    chunk(s) the brush touched. Per-frame cost scales with the *brush*, not the *total*.
- **SDF renderer** keeps its existing budget rules (FrameScheduler, interaction latch) —
  unchanged, just no longer dragged down by raster compositing.
- **Assembly** is a classic scene renderer: instancing, frustum/occlusion cull, LOD, draw
  batching — the standard path to "fast with millions of polys".

---

## 9. Open questions (decide during Phase 1 design)
1. **Bake timing:** live-stream edits to Assembly, on explicit "apply", or both (+ cheap live
   ghost)? *Recommended: on-demand + optional ghost.*
2. **Round-trip:** re-entering an edit room edits the live representation and re-bakes — confirm
   the source-node tagging + when stale bakes refresh.
3. **One scene vs. per-room scenes:** is it one graph filtered per room (recommended), or
   separate documents per room with explicit import?
4. **Shared lighting/environment:** do all viewports share the scene lights/HDRI, or does each
   room have its own look? *Recommended: shared scene lights; Sculpt can add a matcap that
   ignores them.*
5. **Selection across rooms:** selecting a baked object in Assembly → jump to its edit room?

---

## Decision log
- **2026-06-14:** Approved compartmentalized direction (user). SDF is quarantined in its own
  viewport (it's the march-based outlier + the bug source). Voxel + surface coexist in one
  raster editing viewport (3D-Coat-style). A read-only, Unreal/Blender-fast **Assembly**
  viewport shows everything baked, optimized for large scenes. Cross-domain transfer = bake to
  mesh. **Chosen first step: write this design doc, then Phase 1 (Sculpt/Surface raster-only).**
- The interim render patches (`nv==0` PT gate, force-lite, deferred raster-pixel fixes) are
  **stopgaps** that Phase 1 + Phase 3 replace structurally. Keep them until Phase 1 lands so
  the app stays usable in the meantime.
