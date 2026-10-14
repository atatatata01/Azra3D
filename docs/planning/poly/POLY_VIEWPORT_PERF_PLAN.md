# POLY_VIEWPORT_PERF_PLAN.md

## Poly Modeling Viewport Performance — Large Mesh Scalability

**Status:** **VALIDATED — READY TO IMPLEMENT.** Every claim below was verified against live code at
the cited `file:line`, and the §5 baseline was produced by *running* the harness, not by estimation.
**Date:** 2026-07-30 (proposal) · 2026-07-30 (validated + corrected)
**Branch / worktree:** `feat/ui-polish` — `F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish`
**Goal:** make poly *edit-mode* and *object-mode* viewport interactive at high face counts. This is
**not** the Assembly "millions of objects" renderer and **not** SDF/voxel bake perf — see §2.

---

## 0. How to use this document (for the implementing agent)

This document is **validated**. Do not re-derive it. The claim matrix in §4 has already been confirmed
against the live code; treat those `file:line` anchors as ground truth as of 2026-07-30.

1. Read §1–§4 for the architecture and the confirmed problem set.
2. Read §5 for the measured baseline. **Do not claim a win without re-running that harness.**
3. Implement **one phase per PR**, in the §7 order. Each phase has acceptance criteria — meet them.
4. Obey §8 (red lines). Two of them are save-format / architecture rules that override convenience.
5. If a `file:line` anchor no longer matches, the code moved — **re-locate it and note the drift in
   §12**. Do not silently implement against a guess.

**Do not** expand scope into sculpt, SDF bake, path tracer, or the Assembly multi-draw pool.

**Before writing any code, re-read the project `CLAUDE.md`.** The rules that bite hardest here:
No Hacks, No Bandaids, Single Responsibility + Mandatory Reuse (grep before writing any new
function), Frame Ordering Contract, and Frame Budget Rule.

---

## 1. Problem statement

### Owner question
> Does the poly modeling viewport have all optimizations possible for faster viewport performance and
> large amounts of poly rendering?

### Answer (validated)
**No.**

The **solid GPU draw path is genuinely good** — indexed mesh, capacity-backed in-place buffer rewrite,
a correct display-cache skip, depth-tested GPU wire, and a settle-gated GPU face-ID pick. Those are
load-bearing wins and §4 lists them as PRESERVE.

The **CPU side feeding it is not large-mesh complete**, and the dominant cost is not what it looks
like. It is not fill rate, not draw calls, and not triangle count. It is **redundant CPU evaluation of
the modifier stack** — in several places, unguarded, sometimes every frame.

### The measured core of the problem

`apply_poly_modifiers` on a 900-face body with a `Subdivide` level-3 modifier costs **108.1 ms**.
The `content_signature` check that can prove the call is unnecessary costs **0.028 ms**.

**That is a ~3800× ratio.** Every un-guarded call site is therefore worth eliminating *before* any
GPU-side optimization is attempted. This single fact sets the entire phase order in §7.

### Pain symptoms → confirmed cause

| Symptom | Cause (confirmed, not hypothesis) |
|---------|-----------------------------------|
| **Orbit is slow when a poly body is selected** | `draw_node_selection_outlines` runs the FULL stack every frame, uncached — `draw_selection.rs:409`. **This is the headline bug.** |
| Drag tools hitch every frame (bevel/extrude/slide/grab) | Live full `poly_sync_mesh_cache` per drag frame — 17 sites in `poly_drag_tools.rs` |
| Subdivide + Array freezes on slider scrub | Full `apply_poly_modifiers` from base on every dirty — no prefix cache (`mesh_sync.rs:303`) |
| Selecting many faces lags even when solid is fine | egui CPU ear-clip + project per frame (`draw_poly.rs:490-505`) |
| Wire rebuilds when you merely *move* a body | Poly wire has no local edge cache; fingerprint hashes `world_transform` (`overlay_lines.rs:149,349`) |
| Adding Array/Mirror balloons mesh + upload | Geometry merge, not instances (`mesh_sync.rs:327,345,358`) |
| Selection highlights the wrong face on a modified body | Pick uses **base** mesh + fan; solid uses **display** mesh + ear-clip (`poly_select_gpu.rs:176`) — a **correctness** bug |

---

## 2. Goals / non-goals

### Goals (poly viewport only)
1. **Interactive edit** on a single active poly body up to W1/W2 (§5) without multi-frame freezes.
2. **Idle / orbit cost is near-constant.** No modifier evaluation, no retess, no overlay rebuild, no
   pick rebuild on camera-only motion. *(Today this is violated — see §4 C10.)*
3. **One display evaluation per dirty.** Solid, wire, silhouette, and pick consumers share one result
   produced by one owner. *(Today there are two producers — see §4 C15.)*
4. **Honest UI.** If a path is capped, LODed, or proxied, the user is told (no silent truncation).
5. **No save-format break.** Runtime-only caches; never extend serialized `HalfEdgeMesh`.

### Non-goals (explicit — do not drift into these)
| Non-goal | Why |
|----------|-----|
| Assembly multi-draw / millions-of-objects pool | Separate plan: `docs/planning/ASSEMBLY_VIEWPORT_PLAN.md` |
| SDF volume bake / brick LOD | Different pillar, already more advanced |
| Nanite-class meshlets / software occlusion | Not required for DCC-class poly edit |
| Changing half-edge kernel layout / bincode shape | `POLY_FOUNDATION_PLAN.md` forbids |
| Making modifier math itself faster (boolean CSG, Catmull-Clark) | Tool quality, separate track. **This plan makes us stop *calling* it, not make it faster.** |
| Path-tracer BLAS build cost | PT owns that; only invalidation coupling matters |

---

## 3. Architecture (validated)

```
HalfEdgeMesh (scene, Arc base)
    │
    ├─ PRODUCER A (correct, cached) ── poly_sync_mesh_cache(nid)   app/src/poly/mesh_sync.rs:38
    │     skip if (content_signature, stack_key) hit AND GPU solid drawable   :49-118
    │     ├─ apply_poly_modifiers(base, mods, cutters)   :303   FULL stack, NO prefix cache
    │     ├─ raster_data_ngon_with_materials(...)        core_math/poly_mesh.rs:248
    │     │     ear-clip + auto-smooth corner normals (per-corner exploded verts)
    │     └─ MeshCache::update_editable_mesh_ex → upload_pending
    │           render/mesh_cache.rs, mesh_gpu.rs:126 (in-place VB/IB rewrite)
    │
    └─ PRODUCER B (BUG — see C15) ── ensure_poly_meshes()   app/src/raster_mesh_prep.rs:117
          called every frame (Hybrid only) from app_update.rs:476
          cache-miss-gated, but calls raster_data_ngon on the *** BASE *** mesh:
          NO modifiers applied, NO poly_display_cache update  → different geometry than A

Per frame draw:
    solid  → raster_gbuffer.rs draw_indexed  (+ object frustum cull :537, default ON)
    wire   → overlay_lines.rs → LineOverlayPipeline   (fingerprint-gated :52-58 — orbit is free)
    sel    → draw_poly.rs egui face fill / edges / verts        (CPU, per frame)
           → draw_selection.rs:363 draw_node_selection_outlines (CPU, per frame, *** UNCACHED ***)
    pick   → poly_select_gpu.rs face-ID image, settle-gated :158-163, async readback
             ^ built from BASE mesh + fan triangulation :176-196 (≠ solid geometry)
```

### Related systems — do not confuse
| System | Role | Used by poly edit? |
|--------|------|--------------------|
| `mesh_mod_prefix_cache` | SDF display mesh-ops fold cache | **No** — poly stack is independent |
| `draw_resident_pool` / multi-draw | Voxel/SDF pools | **No** |
| `poll_sdf_lod_policy` | SDF distance LOD | **No** |
| `wire_local_cache` (`overlay_lines.rs:351-384`) | SDF wire local edge cache | **No — but poly should copy it.** See P1b.4 |
| Assembly plan A1–A2 | Scene-scale multi-object | Parallel track, reuse later |

---

## 4. Current state matrix — ALL CONFIRMED against live code

Validated 2026-07-30. C1–C14 were proposed and all confirmed; **C15–C17 were discovered during
validation and are new.**

| ID | Claim | Verdict | Evidence |
|----|-------|---------|----------|
| C1 | Solid uses indexed GPU mesh with capacity-backed in-place rewrite | **CONFIRMED** | `render/src/mesh_gpu.rs:126` `update_raw` → `queue.write_buffer`, grows to next-pow2 only on overflow; rationale at `:49-52` |
| C2 | Display retess skipped when `(content_signature, stack_hash)` unchanged AND solid drawable | **CONFIRMED** | `poly/mesh_sync.rs:49-118`. Correctly folds boolean **cutter** content into the key at `:80-101` |
| C3 | Normals computed at sync, not per orbit frame | **CONFIRMED** | `core_math/poly_mesh.rs:248`. **Caveat:** `auto_smooth_corner_normal` `:131` is a per-corner connected-fan walk — raster alone = 3.24 ms at 4096 faces (44% of sync) |
| C4 | Wire uses GPU depth-tested lines; orbit needs no rebuild | **CONFIRMED** | `overlay_lines.rs:52-58` fingerprint early-out; `render/src/line_overlay.rs` |
| C5 | Overlay rebuild is fingerprint-gated; caps exist | **CONFIRMED** | `overlay_lines.rs:26-31` — `MAX_OVERLAY_SEGMENTS=400_000`, `MAX_SDF_WIRE_PER_BODY=200_000`, `MAX_POLY_WIRE=100_000`. **Caps are silent** → P6.2 |
| C6 | Face selection fill is egui, not GPU | **CONFIRMED** | `draw_poly.rs:490-505` earclip→`convex_polygon` per frame; `overlay_faces.rs:13` is a no-op that only clears |
| C7 | GPU face-ID pick rebuilds fan + full-viewport readback on settle | **CONFIRMED** | `poly_select_gpu.rs:171-263`: fresh `create_buffer_init` `:219`, fan `:187`, `copy_full` `:259`. Settle gate `:158-163` is correct — **preserve it** |
| C8 | `apply_poly_modifiers` has NO prefix cache | **CONFIRMED** | `mesh_sync.rs:303-311` — `base.clone()` then sequential `for m in modifiers` |
| C9 | Array / Duplicate / ArrayCircular **merge** geometry | **CONFIRMED** | `mesh_sync.rs:327`, `:345`, `:358` — all `merge_meshes` |
| C10 | Selection rim / mod wires re-run `apply_poly_modifiers` independently | **CONFIRMED — SEVERELY UNDERSTATED** | 3 sites. `overlay_lines.rs:258` and `:280` are fingerprint-gated (acceptable). **`draw_selection.rs:409` is an egui painter with NO gate — every frame.** See §4.1 |
| C11 | Many drag tools live-sync every frame | **CONFIRMED** | 17 sites in `poly_drag_tools.rs`; explicit comments at `:434`, `:804`, `:1232`, `:1883`. `poly_update_transform` (vertex grab) `:1025` full-syncs at `:1232` |
| C12 | Object frustum cull is optional, object-AABB only | **CONFIRMED — and already ON** | `raster_gbuffer.rs:537-555`; **default `frustum_cull: true`** at `app_types/src/types_render.rs:427`. Flips to shadow-only, does not skip the draw |
| C13 | No poly display LOD / proxy mesh | **CONFIRMED** | No hits across `app/src`. *(But see §6 P-LOD note — likely the wrong lever.)* |
| C14 | Face overlay GPU path half-built / abandoned | **CONFIRMED — with a written reason** | `overlay_faces.rs:1-7` documents the triangulation-mismatch cause precisely. `render/src/face_overlay.rs` still present (264 lines) |
| **C15** | **A SECOND tessellation producer bypasses modifiers AND the display cache** | **NEW — CONFIRMED** | `raster_mesh_prep.rs:117` `ensure_poly_meshes` → `raster_data_ngon(mesh)` on the **base** mesh, no `poly_display_cache` write. Called every frame (Hybrid) from `app_update.rs:476` |
| **C16** | **Pick geometry ≠ solid geometry — a correctness bug, not just perf** | **NEW — CONFIRMED** | `poly_select_gpu.rs:176-196` base mesh + **fan**; solid = display mesh + **ear-clip** (`poly_mesh.rs:299`). On any modified body the ID image does not match the screen |
| **C17** | **Poly wire has no local edge cache; SDF has one in the same file** | **NEW — CONFIRMED** | `overlay_lines.rs:349` (poly: re-walks + re-transforms to world every rebuild) vs `:351-384` (SDF: `wire_local_cache` keyed on `content_stamp`, body-local). Fingerprint hashes `world_transform` `:149-150` → **moving a body rebuilds all its wire** |

### 4.1 The headline bug (C10, expanded)

`draw_selection.rs:363` `draw_node_selection_outlines` is an **egui painter**, invoked from
`viewport_overlays.rs:500` and `:1265`. At `:409` it calls:

```rust
let cutters = self.poly_bool_cutters_for(nid, modifiers);
let display = Self::apply_poly_modifiers(mesh, modifiers, &cutters);
```

for **every selected poly body**. There is **no fingerprint, no cache, and no settle gate** — unlike
`overlay_lines.rs`, which has all three.

Consequence: any *selected* poly body carrying a modifier stack re-evaluates that entire stack
(Catmull-Clark, Boolean, Bevel, Array) **on every single frame, including pure camera orbit.** At the
measured rate (§5) that is **108 ms/frame** on a modest 900-face body — a hard freeze, from an overlay
that only needed a silhouette.

This is almost certainly the "orbit is slow" symptom, and it is a **hours-sized fix**. It is P1a.

### Already-good — PRESERVE, do not regress
- **Display-cache skip** (C2) — the bug-#6 fix. Critical.
- **In-place GPU rewrite** (C1) — never return to per-frame `create_buffer_init`.
- **Depth-correct GPU wire** (C4) — never revive CPU backface-wire heuristics.
- **Pick settle gates** (C7, `poly_select_gpu.rs:158-163`) — keep; P5 must not remove them.
- **`POLY_FOUNDATION_PLAN.md` serialization red lines.**

---

## 5. Baseline + targets

### 5.1 Measured baseline (already collected — do not re-derive, only re-run to compare)

Harness **already exists in-tree** — do not write a new one:

```bash
cd core_engine
cargo test -p core_math --test circle_perf --release -- --nocapture
```

Results, this machine, 2026-07-30:

| Measurement | Result |
|---|---|
| `subdivide_catmull_clark` throughput | **~1.0 ms per 1k output faces** (stable 4k→57k) |
| `apply_poly_modifiers`(subdiv=4) + raster, 4096 faces | **7.35 ms** — raster alone **3.24 ms** (44%) |
| Redo-panel 30 re-syncs, **no** modifier | 4.3 ms total (**0.14 ms/frame**) |
| Redo-panel 30 re-syncs, **subdiv=4** | 1391.6 ms total (**46.4 ms/frame**) |
| **BOSS: 900-face body + subdiv=3, 30-frame drag** | **3244 ms — 108.1 ms/frame** |
| `content_signature` skip check, 900 faces | **0.028 ms** |
| `content_signature` skip check, 3600 faces | **0.113 ms** |

**The ratio that drives this plan: 108.1 ms of avoidable work, guarded by a 0.028 ms check.**

### 5.2 Workload definitions

| Workload | Mesh | Modifiers | Mode | Authorable today? |
|----------|------|-----------|------|-------------------|
| **W0** | Cube / 1–2k faces | none | edit + orbit | Yes |
| **W1** | ~50k tris display | Mirror or Array ×3 | edit, move verts | Yes |
| **W2** | ~200k tris display | Subdivide 1–2 + Array | orbit + select 1k faces | Yes |
| **W3** | ~500k–1M tris display | dense base mesh | object mode, orbit only | **NO — needs an import fixture. See below.** |

> **W3 blocker (honest):** there is no way to author a 500k–1M-triangle poly body in-app *without* a
> heavy modifier stack, which conflates stack cost with display cost and makes the number
> uninterpretable. **W3 is deferred until an imported dense-mesh fixture exists.** Do not report a W3
> number produced by stacking Subdivide — that measures §5.1, not display scaling.

### 5.3 Frame budgets (single active poly body, Hybrid raster, mid GPU)

| Scenario | Target | Hard fail |
|----------|--------|-----------|
| Orbit idle W1 | ≤ 8 ms overlays + solid, steady | > 16 ms sustained |
| **Orbit idle W1, body SELECTED, stack enabled** | **≤ 8 ms (i.e. ~0 ms of modifier work)** | any per-frame `apply_poly_modifiers` |
| Vert grab W1 (topology stable) | ≤ 16 ms median edit frame | multi-frame freezes |
| Bevel drag W1 | ≤ 33 ms median (30 fps floor) | > 100 ms spikes common |
| Face-select highlight 1k faces W2 | ≤ 4 ms selection overlay | > 12 ms |
| Orbit W2 object mode | ≤ 16 ms | freezes |

### 5.4 Instrumentation

`time_block!` already exists and is in use (`app_update.rs:176`, `:447`, `:471-483`). Add:

- `poly_sync.modifiers_ms` / `poly_sync.raster_ms` / `poly_sync.upload_ms`
- `poly_overlay.rebuild_ms` / `poly_overlay.draw_ms`
- `poly_sel.outline_ms` ← **must isolate `draw_node_selection_outlines`**
- `poly_pick.rebuild_ms` / `poly_pick.readback_ms`

**Add one counter that is not a timer:** `poly_sync.modifier_evals_this_frame`. The goal state for
orbit is **literally zero**, and a counter proves that in a way a millisecond number cannot.

---

## 6. Phased plan

One PR-sized theme per phase. Each ships value alone.

---

### Phase 0 — Measure
**Risk:** Low · **Effort:** S · **Ships alone:** Yes · **Blocked by:** nothing

| Task | Detail |
|------|--------|
| P0.1 | Add the §5.4 timers + the `modifier_evals_this_frame` counter. Optional debug HUD line |
| P0.2 | **Extend** `core_math/tests/circle_perf.rs` — do not write a new harness |
| P0.3 | Record W0–W2 app-side numbers into §11 |

**Acceptance**
- [x] Timers + eval counter visible without a special build — Perf HUD: `Poly mod evals: N | sel outline X.XXms` (`viewport_overlays.rs`); counter fields on `AzraApp`
- [ ] §11 baseline table filled for W0–W2 (W3 deferred per §5.2) — **owner manual after relaunch**
- [ ] `circle_perf.rs` numbers reproduced on the target machine — optional re-run; §5.1 already populated

---

### Phase 1a — Kill uncached per-frame modifier evaluation ⭐ **DO THIS FIRST**
**Risk:** Low · **Effort:** S (hours) · **Ships alone:** Yes · **Blocked by:** nothing

Highest value-to-risk ratio in the plan. No product decision required. No new architecture.

| Task | Detail |
|------|--------|
| P1a.1 | **`draw_selection.rs`** — stop calling `apply_poly_modifiers` in an every-frame egui painter. **DONE 2026-07-30:** `poly_display_cache` now stores `PolyDisplayMeshCache { key, mesh: Arc<HalfEdgeMesh> }`; `poly_cached_display_mesh(nid)` is the single consumer API; outline uses it (`draw_node_selection_outlines` is `&mut self`). |
| P1a.2 | Audit **every** `apply_poly_modifiers` call site for a gate. Live set after P1a: `mesh_sync` (eval only on key miss ✅), `poly_cached_display_mesh` (eval only on miss ✅), `overlay_lines` selection/mod wires → cache ✅ + fingerprint gate, `lab_mesh_sdf.rs` (debounced convert path — still direct apply; not per-frame), tests (direct apply OK). |
| P1a.3 | Assert the invariant: **zero modifier evaluations on a camera-only frame.** Use the P0.1 counter — HUD shows green when `poly_modifier_evals_last_frame == 0`. |

**Acceptance**
- [x] Code path: orbit no longer calls uncached `apply_poly_modifiers` from selection outline
- [ ] `modifier_evals_this_frame == 0` while orbiting with a selected, stacked poly body — **verify in Perf HUD**
- [ ] Orbit with W1 + selected body + Subdivide: **no** measurable stack cost — **manual**
- [x] Selection outline invalidates with key (content_sig + stack hash + cutter fold) — same as solid skip
- [ ] BOSS-shaped manual test (900-face + subdiv=3, selected, orbit) drops from ~108 ms/frame to ~0 — **manual**

---

### Phase 1b — Single display evaluation (one producer, shared by all consumers)
**Risk:** Medium · **Effort:** M · **Ships alone:** Yes · **Blocked by:** P1a  
**Status: IMPLEMENTED 2026-07-30** (with P1a display Arc cache as foundation)

| Task | Detail |
|------|--------|
| P1b.1 | **DONE (P1a):** `PolyDisplayMeshCache { key, mesh: Arc<HalfEdgeMesh> }` filled by `poly_sync_mesh_cache` / `poly_cached_display_mesh` |
| P1b.2 | **DONE:** selection rim, mod wires, object wire → `poly_append_display_wire` → cache only |
| P1b.3 | **DONE:** `ensure_poly_meshes` only calls `poly_sync_mesh_cache` — no base-only `raster_data_ngon` path (C15 closed) |
| P1b.4 | **DONE:** `poly_wire_local_cache: HashMap<NodeId, WireLocalEdgeCache>` — body-local edges; transform re-emit only when stamp matches |
| P1b.5 | **DONE:** display stamp = `key.0 ^ key.1.rotl(17)`; base edit wire uses `content_signature ^ BA5E…` |

**Acceptance**
- [x] Wire / rim consumers do not call `apply_poly_modifiers` on cache hit
- [x] Object wire uses **display** mesh (matches solid with modifiers)
- [x] C15 resolved: single producer (`poly_sync_mesh_cache`)
- [x] Moving a body reuses local edge list (stamp-gated walk)
- [ ] Manual: Subdivide 2 + orbit/move feels free of stack re-eval (Perf HUD mod_evals=0)

**Anti-patterns**
- Do **not** serialize the display cache into FL4
- Do **not** store GPU buffers in the cache without explicit lifetime rules

---

### Phase 2a — Modifier prefix cache
**Risk:** Medium · **Effort:** L · **Ships alone:** Yes · **Blocked by:** P1b  
**Status: IMPLEMENTED 2026-07-30**

| Task | Detail |
|------|--------|
| P2a.1 | **DONE:** `PolyPrefixBodyCache` stores mesh after each stack slot; `apply_poly_modifiers_cached` |
| P2a.2 | **DONE:** longest matching prefix_hash walk; rebuild from first mismatch only |
| P2a.3 | **DONE:** 256 MiB global budget + 16 body LRU; Perf HUD shows `Poly prefix: N bodies \| MB` |

**Acceptance**
- [x] Code: Array scrub reuses Subdivide prefix (prefix_hash match on slots below dirty index)
- [x] Host memory capped (`POLY_PREFIX_BUDGET_BYTES` / `POLY_PREFIX_MAX_BODIES`)
- [x] Free-path oracle tests still pass (`apply_poly_modifiers` unchanged for tests)
- [ ] Manual: Subdivide + Array count scrub feels live (no multi-second hitch)

---

### Phase 4 — Selection / highlight off egui, onto GPU
**Risk:** Medium · **Effort:** L · **Ships alone:** Yes · **Blocked by:** P1b (needs shared tess)  
**Status: IMPLEMENTED 2026-07-30** (face selection fill; polygroups still egui)

| Task | Detail |
|------|--------|
| P4.1 | **DONE:** `build_overlay_faces` uses same `earclip_polygon` as solid `raster_data_ngon` + normal pull |
| P4.2 | **DONE:** Hybrid depth-tested GPU fill; outline stays egui; X-Ray when `!visible_only` |
| P4.3 | **DEFERRED:** polygroup GPU colour (still egui path in `draw_poly`) |
| P4.4 | **DONE:** egui fill fallback when GPU list empty or not Hybrid |

**Acceptance**
- [x] GPU path live for face/group selection in Hybrid
- [x] Same ear-clip as solid (triangulation mismatch root cause addressed)
- [x] `overlay_faces.rs` no longer a permanent no-op
- [ ] Manual: multi-face select, orbit grazing — fill stays on faces, no bleed
- [ ] P4.3 polygroups GPU still open

---

### Phase 3 — Dirty-domain partial updates (topology-stable edits)
**Risk:** High · **Effort:** L · **Ships alone:** Yes · **Blocked by:** P1b  
**Status: IMPLEMENTED 2026-07-30**

| Task | Detail |
|------|--------|
| P3.1 | **DONE:** `poly_mods_need_geometry_reapply` — geo stack vs normal-only/empty |
| P3.2 | **DONE:** `poly_sync_after_position_edit` re-rasters without Subdivide/Array/etc. when possible; `topology_signature` added for future tighter paths |
| P3.3 | **Honest:** still runs `raster_data_ngon` (normals); win is **skipping geometry modifier re-eval** + releasing display Arcs before `make_mut` |
| P3.4 | **DONE:** grab/rotate/scale, shrink/fatten, extrude drag, inset drag → position path; bevel/extrude **start**/topology ops stay full `poly_sync_mesh_cache` |

**Acceptance**
- [x] Grab without geo modifiers skips `apply_poly_modifiers` / prefix rebuild
- [x] Stack with Subdivide still full-reapplies (correct)
- [x] Topology tools unchanged (full sync)
- [ ] Manual: cage grab feels lighter; with Subdivide still pays stack cost (expected)

---

### Phase 5 — Scale GPU pick (**correctness first, then perf**)
**Risk:** Medium · **Effort:** M–L · **Ships alone:** Yes · **Blocked by:** P1b  
**Status: IMPLEMENTED 2026-07-30** (core correctness + VB reuse; half-res / BVH deferred)

> **Frame this as a correctness fix.** Per C16, pick used **fan** while solid used **ear-clip** — concave
> faces disagreed. Edit selection always uses **base FaceIds**; pick still builds the **base cage**
> (with ear-clip) so FaceIds stay valid. With geometry modifiers the solid is the display mesh —
> pick selects the cage (correct for edit). With no geo mods, pick == solid triangulation.

| Task | Detail |
|------|--------|
| P5.1 | **DONE:** ear-clip + base FaceId; capacity VB `upload_poly_vertices`; mesh_key folds display cache key (stack) |
| P5.2 | **DEFERRED:** half-res / hierarchical readback |
| P5.3 | **KEPT:** existing CPU ray fallback while image pending |
| P5.4 | **PRESERVED:** cam + mesh settle gates |

**Acceptance**
- [x] Concave faces: pick triangulation matches solid ear-clip (no fan)
- [x] No per-settle `create_buffer_init`
- [x] Settle gates unchanged
- [ ] Manual: face click on cage (no mods / with mods) selects expected face
- [ ] P5.2 half-res still open

---

### Phase 2b — Draw Array/Mirror as instances (display-only) 🔒 **OWNER-BLOCKED**
**Risk:** Medium–High · **Effort:** L · **Ships alone:** Yes · **Blocked by:** P2a **+ §9 answers**

| Task | Detail |
|------|--------|
| P2b.1 | **Mirror:** optional display path drawing flipped instances instead of merging (edit base stays unmirrored) |
| P2b.2 | **Array / Duplicate / ArrayCircular:** display as N instance transforms of the last prefix mesh |
| P2b.3 | **Keep the merge path for every consumer needing welded soup:** Boolean-after-Array, Mesh→SDF, export, and **`lab_mesh_sdf.rs:390`** — the poly→SDF link consumes `apply_poly_modifiers` output directly and would silently break under an instance-only Array |
| P2b.4 | Interaction with P5: instanced draws have no per-instance face IDs. Resolve pick semantics before shipping |

**Acceptance**
- [ ] Visual parity for Mirror X and Array linear (Blender-oracle style, as in `mesh_sync.rs` tests)
- [ ] VRAM / host memory does not grow unbounded
- [ ] Poly→SDF link still produces correct geometry for an instanced Array

---

### Phase 6 — Object-mode / multi-body hygiene
**Risk:** Low–Medium · **Effort:** S–M · **Ships alone:** Yes · **Blocked by:** P1b  
**Status: IMPLEMENTED 2026-07-30**

> Reduced from the original scope: local wire cache was P1b.4; frustum default was already ON.

| Task | Detail |
|------|--------|
| P6.1 | **DONE:** inactive poly object-wire uses stride 4; selected/active/mod-selected = full density |
| P6.2 | **DONE:** one-shot toast + Perf HUD `Wire segs: N CAP` when `MAX_OVERLAY_SEGMENTS` / per-body wire cap hit |
| P6.3 | **DONE:** documented in `raster_gbuffer.rs` — frustum default ON; culled → shadow-only (not dropped); HUD shows cull counts |

**Acceptance**
- [x] Inactive multi-body wire thinned (stride 4)
- [x] Cap is not silent (toast + HUD)
- [x] Frustum cull documented + HUD
- [ ] Manual: many poly bodies + wire on stays interactive

---

> **On poly display LOD (old C13):** confirmed absent, but deliberately **not** a phase. The measured
> bottleneck is CPU modifier evaluation and retessellation, not GPU fill or triangle throughput. Adding
> LOD before P1–P3 land would optimize the wrong end of the pipe. Revisit only if post-P3 measurements
> show GPU-side cost dominating.

---

## 7. Priority order

```
P0   Measure                              ← ~half already exists (circle_perf.rs)
 → P1a  Kill uncached per-frame evals      ← ⭐ hours; ~108 ms/frame → ~0; zero product risk
 → P1b  Single display eval + wire cache + dual-producer fix
 → P2a  Prefix cache                       ← unlocks Subdivide+Array scrubbing
 → P4   GPU selection                      ← the "why is select laggy when solid is fine" fix
 → P3   Partial position updates           ← drag tools
 → P5   Pick scale (correctness + perf)
 → P2b  Instances                          🔒 owner-gated
 → P6   Multi-body hygiene + cap honesty
```

**Rationale.** P1a is first because it is the largest measured win, the smallest diff, and carries no
semantic change. P1b makes "one display evaluation" structurally true rather than aspirational. P2a
unlocks stack scrubbing. P4 addresses the most-reported perceived lag. P3 and P5 are deeper. P2b is
last because it is the only phase needing product decisions.

**Nothing blocks P0, P1a, P1b, P2a, P3, P4, or P5.** Seven phases of unblocked work.

---

## 8. Risks & red lines

| Risk | Mitigation |
|------|------------|
| Display cache desync (wire ≠ solid) | Single producer in `poly_sync_mesh_cache`; every consumer is read-only |
| **A second display producer reappears** | **RED LINE:** exactly one function owns the poly display mesh. C15 is this bug already present — P1b.3 closes it. Do not add a third |
| Instanced Array breaks Boolean / export / poly→SDF | Force materialization when any consumer needs welded soup — incl. `lab_mesh_sdf.rs:390` |
| **P2b introduces a second display technique** | **RED LINE — Display Invariant (CLAUDE.md):** poly stays triangles-only in one raster path. Instancing must be one technique with N transforms, never a second renderer fused over the same pixels |
| Partial update misses normals / creases | Topology hash + full rebuild on any topology-gen bump. Remember `auto_smooth_corner_normal` is per-corner (P3.3) |
| Face overlay picks the wrong face (known history) | Must share the solid triangulation / face-id attribute — `overlay_faces.rs:1-7` explains the prior failure |
| Prefix-cache memory explosion | Hard budget + drop-oldest. Frame Budget Rule: cap per-frame work |
| **FL4 corruption** | **RED LINE:** runtime-only caches. Never extend `HalfEdgeMesh`'s serialized shape, never add a `NodePayload` variant, never a second trailing magic after `FL4ORDER`. See `POLY_FOUNDATION_PLAN.md` |
| GPU resource mutation in a panel/paint callback | **Frame Ordering Contract:** buffer/texture creation belongs in `poll_background_tasks`, never in an egui draw or input handler. P4/P5 both touch this |
| Scope creep into Assembly | Refuse the multi-draw mega-pool here |

---

## 9. Owner decisions

**Blocking P2b only.** Every other phase may proceed.

1. May the Mirror modifier display as instances when the next modifier is a Boolean?
2. Should Array instances participate in poly edit selection as a single body? *(expected: yes)*
3. **(added by validation)** What should the poly→SDF link (`lab_mesh_sdf.rs:390`) receive for an
   instanced Array — forced materialization, or only the base instance?

**Non-blocking but worth answering:** is W3 (500k–1M tris) worth building an import fixture for, or do
we cap this plan at W2? (§5.2)

---

## 10. Test plan

### Automated
| Test | Phase |
|------|-------|
| Existing `mesh_sync.rs` oracle suite (`apply_array_matches_blender_oracle`, mirror bitmask, bevel parity) still passes | all |
| **New: `modifier_evals_this_frame == 0` on a camera-only frame** | **P1a** |
| New: display-cache hit skips the second modifier eval (spy/counter) | P1b |
| New: prefix cache rebuilds only from the dirty index | P2a |
| New: position-only path does not change face count / index buffer | P3 |
| **New: face pick on a modified body returns the face under the cursor** | **P5** |
| Paint/box-select suite + pick settle smoke | P5 |
| `cargo test -p core_math` green | all |
| `cargo build -p app` after every change | all |

### Manual owner scripts
1. **P1a proof:** cube → Subdivide 3 → select the body → orbit. Must be smooth (this freezes today).
2. Cube → Subdivide 2 → Array 8 → orbit and scrub the Array count (P1b/P2a).
3. Bevel drag on a dense mesh — compare against the P0 baseline (P3).
4. Select half the faces — measure highlight cost (P4).
5. **P5 correctness:** enable Subdivide, click a face, confirm the selected face is the one clicked.
6. Object mode, many poly bodies — frustum + wire caps (P6).

---

## 11. Baseline numbers (fill during P0)

`circle_perf.rs` kernel numbers are already in §5.1. This table is **app-side, in-viewport**.

| Workload | machine | poly_sync.ms | overlay.ms | sel_outline.ms | pick.ms | modifier_evals/frame (orbit) | fps orbit | notes |
|----------|---------|--------------|------------|----------------|---------|------------------------------|-----------|-------|
| W0 | | | | | | | | |
| W1 | | | | | | | | |
| W1 (selected + stack) | | | | | | **← must be 0 after P1a** | | |
| W2 | | | | | | | | |

---

## 12. Anchor drift log

If a `file:line` in §3/§4 no longer matches, record the correction here rather than implementing
against a guess.

| Date | Anchor as written | Actual location | Notes |
|------|-------------------|-----------------|-------|
| | | | |

---

## 13. Key files

| Role | Path (under `core_engine/`) |
|------|------------------------------|
| Sync / modifiers / stack hash | `app/src/poly/mesh_sync.rs` |
| **Uncached selection outline (P1a target)** | **`app/src/draw_selection.rs:363,409`** |
| **Second tessellation producer (C15)** | **`app/src/raster_mesh_prep.rs:117`** |
| Drag tools / live retess | `app/src/poly_drag_tools.rs` |
| Raster triangulation + normals | `core_math/src/poly_mesh.rs` |
| GPU mesh + upload | `render/src/mesh_gpu.rs`, `render/src/mesh_cache.rs` |
| Solid draw + frustum cull | `app/src/raster_gbuffer.rs` |
| Wire (poly `:349`, SDF cache `:351`) | `app/src/overlay_lines.rs`, `render/src/line_overlay.rs` |
| Selection egui | `app/src/draw_poly.rs`, `app/src/draw_selection.rs` |
| Face overlay (dead, documented) | `app/src/overlay_faces.rs`, `render/src/face_overlay.rs` |
| GPU pick | `app/src/poly_select_gpu.rs` |
| Poly→SDF consumer (P2b hazard) | `app/src/lab_mesh_sdf.rs:390` |
| Perf harness | `core_math/tests/circle_perf.rs` |
| Frame order / timers | `app/src/app_update.rs` |
| Foundation constraints | `docs/planning/poly/POLY_FOUNDATION_PLAN.md` |
| Assembly (separate track) | `docs/planning/ASSEMBLY_VIEWPORT_PLAN.md` |

---

## 14. Relationship to other plans

| Plan | Relationship |
|------|--------------|
| `POLY_FOUNDATION_PLAN.md` | Kernel + serialization red lines — this plan **obeys** |
| `ASSEMBLY_VIEWPORT_PLAN.md` | Scene-scale multi-object — **complement**, not substitute |
| `PAINT_SELECT_AUDIT.md` | Origin of the GPU pick design; P5 extends it |
| `LIVE_MOVE_PERF.md` | SDF prim move bake — **out of scope** |
| `sculpt/*PERF*` | Voxel/sculpt room — **out of scope** |
| Modifier feature work | Features *create* geometry; this plan makes *display* of it scale |

---

## 15. Summary

AZRA's poly viewport has a competent **GPU** path — indexed meshes, in-place buffer rewrites,
depth-correct wire, settle-gated GPU picking. The bottleneck is **CPU-side redundant evaluation of the
modifier stack**, and it is quantified: 108.1 ms of avoidable work guarded by a 0.028 ms check.

The worst instance is an object-mode selection outline that re-runs the entire stack **every frame,
uncached, during pure camera orbit** — a small fix with the largest payoff (P1a). Beyond it, the plan
makes "one display evaluation" structurally true (P1b, which also closes a second, divergent
tessellation producer), adds prefix caching (P2a), moves selection fills to the GPU (P4), enables
partial updates for topology-stable drags (P3), and repairs a **correctness** bug where pick geometry
does not match displayed geometry (P5). Instanced Array/Mirror (P2b) is last and owner-gated.

Assembly multi-draw and SDF bake remain separate tracks. Save format and half-edge serialization are
untouched.

---

## Change log

| Date | Change |
|------|--------|
| 2026-07-30 | Initial proposal (post poly viewport audit) |
| 2026-07-30 | **VALIDATED against live `feat/ui-polish` + CORRECTED.** C1–C14 all confirmed; C15–C17 discovered and added (dual tessellation producer; pick≠solid geometry; poly wire has no local cache). C10 found severely understated → promoted to the new **P1a** (`draw_selection.rs:409` runs the full stack every frame, uncached). Baseline measured by running `circle_perf.rs` (§5.1) — BOSS case 108.1 ms/frame vs a 0.028 ms skip check. Corrections: old P6.3 already satisfied (`frustum_cull` defaults true); P4's "no written reason" criterion already met (`overlay_faces.rs:1-7`); W3 marked unmeasurable pending an import fixture; old P6.2 promoted into P1b.4; P5 reframed as correctness-first; `lab_mesh_sdf.rs:390` added as a P2b hazard; Display Invariant + single-producer red lines added. Phase order revised. **Status → READY TO IMPLEMENT (P0+P1a = first PR).** |
