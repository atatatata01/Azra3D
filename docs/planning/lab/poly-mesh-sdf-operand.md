# Poly Mesh → SDF Operand (modifier + destructive stamp) — Lab Log

- **Status:** PROMISING <!-- feasibility spike GREEN — perf measured, architecture mapped -->
- **Touches:** poly (PolygonalBody/PolyMesh) + SDF bake (volume_bake.wgsl, BodyVolumeCache) + graphics (serialize HOT buffer, BGL ×3) + io_bridge (mesh_to_sdf) + core_math (SdfShape/FieldEvaluator) + scene graph
- **Started:** 2026-07-03   ·   **Owner:** lab (`feat/lab`)

## 1. Goal / vision

### FULL PURPOSE (owner-confirmed 2026-07-03, iteration-1 in-app review)
**Make a polygon mesh become a NATIVE SDF PRIMITIVE inside an SDF body's primitive tree** — so precise
poly geometry (modeled or imported) can be combined with smooth SDF operations as if it had always been
SDF. The mesh stops being a foreign object sitting *next* to the SDF body; the SDF engine *understands*
it — unions, subtracts, intersects, and smooth-blends it exactly like a built-in sphere/box, and it
lives in the tree where you select/reorder/edit it.
- **Owner's 4 use cases (ALL of them — must be general):** (a) combine poly + SDF freely, (b) poly as
  cutter/insert (subtract holes / add hard-surface detail), (c) import outside assets (OBJ/CAD) into
  SDF, (d) base in poly + detail in SDF.
- **Sharp edges:** SMOOTH / SDF-look is FINE (owner-confirmed). Grid conversion is the right tech; no
  sharp-feature-preservation research needed. Higher res = crisper but never razor-sharp — acceptable.
- **First build = DESTRUCTIVE convert** (owner choice): select ONE poly mesh → "Convert to SDF
  primitive" → becomes a MeshVolume prim in an SDF body, original poly consumed. Live-modifier mode is
  a LATER iteration.
- **Quality = tighter-fit + higher-res** (owner choice): fit the grid tightly to the mesh AABB, raise
  res adaptively (≈192–256³) so small meshes aren't coarse. The iteration-1 "broken surface" was a
  global hard-`min()` union over a loose 128³ grid + DC facets — fixed by making it an in-tree prim
  with smooth blend + a tight/high-res grid.

### Why iteration 1 was the WRONG SHAPE (owner feedback, kept as the pivot record)
The i1 spike unioned ONE grid globally, AFTER the whole boolean tree, into EVERY body — so it was
union-only, all-bodies, and left the poly mesh duplicated as a ghost. Owner's four corrections map 1:1
to the fix: per-mesh (not global) + in the tree (not after it) + full booleans/blend (free once it's a
prim) + clean surface (tight/high-res). i1 still earned its keep: it proved the mesh→SDF perf
(+0.12 ms/bake) and the side-buffer plumbing that iteration 2 reuses.

### Legacy two-mode framing (still the eventual end state)
1. **Live modifier (non-destructive):** the MeshVolume prim stays linked to a live PolygonalBody and
   re-converts (debounced) on poly edits. LATER.
2. **Destructive convert:** consume the poly, bake its field into a MeshVolume prim. **FIRST (i2).**

This is the mirror image of the sanctioned "SDF enters a sculpt as a boolean stamp on the density
field" rule — a poly mesh enters the SDF world **as a sampled distance field**, never as a second
renderer (Display Invariant safe). Prior art: Substance Modeler, Blender Mesh-to-Volume, MagicaCSG.

**"Done" (i2) =** select a poly cube → Convert to SDF primitive → it appears in an SDF body's tree as a
MeshVolume prim → set its boolean to Subtract with blend-K and it smoothly cuts the body → undo
restores the poly → save/load round-trips.

## 2. Current approach
**Pre-baked per-mesh SDF grid, sampled by the body bake shader from a side BUFFER (the clip_curves
model), never a per-prim texture binding.** Conversion (`region_mesh_to_sdf`, already shipping in the
MOVE tool) runs async off-frame: draft 64³ instantly (~65ms), refine 128³ in background (~0.5s), only
when the SOURCE MESH changes — transform changes sample through the inverse transform (zero
re-conversion). Live poly edits re-convert only the dirty AABB region (8–27ms, debounced). The grid is
DERIVED data: persist only the source node ref + resolution; re-derive on load (no giant blobs in .fl4).
CPU side plugs in via the `sculpt_layers.delta_grid` precedent (per-prim sampled grid blended in
`world_distance`, primitives.rs:491) so FieldEvaluator/picking/tools work unchanged.

## 3. Attempts / iterations  (newest first — append every session)
| Date | What I tried | Outcome |
|------|--------------|---------|
| 2026-07-14 | **Iteration 4 — SELECTION GHOST: exact mesh silhouette (two-stage).** Owner report: selecting a linked Mesh prim FLOODED the whole viewport red. Root cause: the `prim_outline` overlay marches the SHARED analytical `eval_prim` (sdf_prim_eval.wgsl), which had NO MeshVolume (st==10) branch → `d` kept its init value → marcher "hit" at t=0 on every pixel. **Fix A (committed 504a7a4):** analytical `eval_prim` st==10 → bounded BOX (grid half-extent packed into HOT params[12-14] via `corner_abs_max`); stops the flood. **Fix B (owner chose "exact silhouette"):** the outline marches `serialize_from_graph`'s buffer (full 292-stride, parallel per-body walk with scope markers) — and there is NO free field in the 292 layout ([61-63] "reserved" are actually mirror/falloff). So the offset rides in a NEW **parallel `mesh_start` u32 array** emitted INSIDE serialize_from_graph's own walk (aligned by construction, `debug_assert` guards len==slots), + a packed `mesh_grid` side buffer; both in `SerializeResult`, gated so cost≈0 when no MeshVolume is selected (one u32/slot, no grid packed). Bound as bindings 5/6 to the prim_outline pipeline (auto-layout picks them up); `prim_outline_entry.wgsl` st==10 samples the SAME grid the bake uses → exact converted-mesh silhouette + the normal crisp contour/pale-cutter fill. Partial serialize disabled when outline_active (needs the fresh start array). | **build green (exe); graphics 44/44 (serialize tests exercise the alignment assert); GPU parity STILL exact.** Reverted a first wrong attempt (flat `serialize_primitives` order ≠ graph order) before landing the in-walk emission. NOT visually smoke-tested. |
| 2026-07-14 | **Iteration 3 — LIVE MODIFIER shipped ("Link Poly → SDF Modifier").** Owner-confirmed UX: auto-hide the poly, POLY DRIVES the transform, on-release debounced refresh. Synced feat/lab to master first (ff, 174 commits — master had integrated i2/i2b and FIXED the old GLSL sync failure; SDF worktree appended `HalfSpace` after MeshVolume, append rule respected). **Key insight that killed the perceived "boolean integration" problem: it was ALREADY solved by i2 — a MeshVolume IS a prim, so booleans/blend (incl. master's new X pie, which edits `prim.operation`) work with zero new code.** What i3 actually built: (a) `MeshVolume` gains `source_poly: Option<NodeId>` (variant-internal field = save-format safe for old files WITHOUT MeshVolume prims; files WITH them from 2026-07-03..13 break — lab-acceptable, noted in the variant doc); (b) **non-centered grids** — linked grids are authored in the POLY's local frame (`bounds_min` = poly-local AABB min) so the prim transform EQUALS the poly transform (no center field, no matrix); `half_extent()` → `corner_abs_max()` (correct for both centered + off-center) across 8 call sites; (c) transform follow: every poll, linked prims copy the poly's decomposed world transform via `write_prim_transform` (no-op guarded → costs nothing when still; drives partial rebakes like a gizmo drag when moving; DERIVED state — no undo, poly's undo re-syncs naturally); (d) geometry refresh: `poly_sync_mesh_cache` (THE choke point every poly retessellation funnels through) stamps `pending_refresh[nid]`; poll fires a re-convert 350ms after the LAST stamp (per-frame stamps during drags ⇒ fires on release), swaps the grid Arc on all linked prims, rebakes; open-mesh edits freeze the imprint with a one-shot warning; (e) refresh converts the DISPLAY mesh (`apply_poly_modifiers` made pub(crate) — mirror/subdiv/solidify included); (f) UI: outliner "🔗 Link as SDF Modifier", viewport ctx menu + Add menu live items, properties panel shows link status + "Refresh now" + "Make Static" (unlink keeps last grid). | **build green; core_math 449/449, graphics 44/44 (GLSL sync fixed on master), GPU parity STILL EXACT (sign_flips=0, near\|dv\|=0.0000) with non-centered grids + link field, bake regression 3/3.** NOT visually smoke-tested. |
| 2026-07-03 | **Iteration 2b — promoted convert from the dev panel to first-class UI (owner request "make it default + right-click context menu").** Refactored `convert_selected_poly_to_sdf` → `convert_poly_to_sdf(nid)` core + selected wrapper. Wired THREE surfaces: (1) menu bar **Add → "◈ Poly → SDF Primitive"** (enabled when a convertible poly exists); (2) **viewport right-click context menu** in Object Mode → "◈ Convert Poly → SDF Primitive" (empty-space branch of `draw_context_menu`, deferred `do_convert_poly` flag — a poly under cursor gives SDF-raycast `hit=None` so it lands there); (3) **outliner poly right-click** → "◈ Convert to SDF Primitive" (per-node via new `OutlinerDeferred.convert_to_sdf`, targets the clicked node — the precise path). No InputSystem registration needed (UI menu items, not new bound key/mouse actions; RMB-opens-context-menu already exists at input_viewport.rs:1360). Dev Tweaks button kept as a secondary access. | **build green.** UI plumbing follows existing patterns exactly (add_primitive_immediate / subdivide_node deferred). Viewport-menu convert uses selected-else-first poly (raycast is SDF-only, doesn't select the poly under cursor) — the outliner menu is the per-node-precise path. NOT visually smoke-tested. |
| 2026-07-03 | **Iteration 2 — `SdfShape::MeshVolume` primitive SHIPPED (destructive convert, in-tree, full booleans).** New `MeshSdfData` + `SdfShape::MeshVolume{grid:Arc<MeshSdfData>}` variant appended LAST; CPU `distance()` trilinear-samples the grid; GPU: `serialize_mesh_grids_for_bake` packs per-prim grids into the binding-5 side buffer (clip_curves pattern, offset in hot[86], st==10), `volume_bake.wgsl` `eval_mesh_volume` samples it IN the scope loop so union/subtract/intersect/blend-K all apply per-prim; `convert_selected_poly_to_sdf()` (Developer Tweaks button) converts the selected closed poly → MeshVolume prim in the active SDF body (adaptive tight-fit 128³, poly consumed, `transaction()` undo). i1 global operand union removed. 15 SdfShape match sites across 6 crates updated (2 subagents). | **worked — GPU↔CPU parity EXACT.** New test `mesh_volume_bake_matches_cpu` (real serialize→bake path): `sign_flips=0, nan=0, near|dv|=0.0000`, inside_voxels=2321 (mesh carves the interior at the offset prim position). core_math 374/374, graphics 42/43 (1 pre-existing GLSL fail), existing GPU bake tests still green. Build green all crates. NOT yet visually smoke-tested in the running app. |
| 2026-07-03 | **Iteration 1 — GPU spike SHIPPED (bake-path union, dev toggle).** `mesh_grid` @binding(5) self-describing buffer (header: res, world grid-min, cell; data x+y·res+z·res²) trilinear-sampled + unioned after the prim stack machine in `volume_bake.wgsl`; ALL 6 bake BGLs collapsed to new `graphics::bgl::bake_bgl_entries()` (fixes the pre-existing BGL-Single-Source violation); `lab_mesh_sdf.rs` (toggle → thread `region_mesh_to_sdf` 128³ → poll-phase upload → rebake all); Developer-Tweaks checkbox; A/B bench test. 4-lens adversarial review found + FIXED: (1) body-local vs world frame mismatch → `BakeParams.body_local_to_world` mat4 (144→208 B, appended), (2) bake source hash didn't include operand state → `lab_operand_gen` mixed into hash, (3) no open-mesh gate → half-edge `twin == NO_TWIN` scan skips open bodies, (4) `map_async \|_\|{}` in bench → channel-captured Result. | **worked — measured bake delta at 128³/8 prims: +0.12 ms (~3–6% of a ~2–4 ms bake, within run noise; 3 A/B runs)**. Enable/disable proven by centre-voxel sign flip (+1.80 → −0.97 = the operand sphere). Build green; graphics tests 42/43 (1 failure PRE-EXISTING: `pathtrace_primary_full.comp` `FLOATS_PER_PRIM 288u` vs Rust 292 — report to MASTER); both existing GPU bake tests still byte-green (disabled dummy = proven no-op). NOT yet visually smoke-tested in the running app. |
| 2026-07-03 | Feasibility recon (terrain map of all integration seams) + CPU perf spike: benchmarked the production converter `io_bridge::mesh_to_sdf::region_mesh_to_sdf` (release) on real hardware — `io_bridge/tests/mesh_sdf_perf.rs` (`cargo test -p io_bridge --release --test mesh_sdf_perf -- --ignored --nocapture`) | **worked — feature is feasible AND performant.** Numbers below. Verdict: conversion cost is one-shot/async (never per-frame); render-side cost is ~1 trilinear fetch per voxel per operand during rebake (cheaper than one beveled-box analytical eval); zero new per-frame work when idle. |

**Measured conversion cost (this machine, release):**
| Scenario | Tris | Grid | Time | Memory |
|---|---|---|---|---|
| Full convert (draft) | 5k–80k | 64³ | 64–70 ms | 1.0 MB |
| Full convert (standard) | 5k–80k | 128³ | **~0.5 s** (tri-count barely matters — BVH) | 8.4 MB |
| Full convert (hi-res) | 20k | 256³ | 5.0 s | 67 MB |
| Region update (live edit) | 20k | 24³ of 128³ | **7.8 ms** | — |
| Region update (live edit) | 20k | 48³ of 128³ | 27 ms | — |
| Region update (live edit) | 80k | 32³ of 128³ | 21 ms | — |

**Why the live modifier stays smooth (the performance argument):**
- Moving/rotating/scaling the operand = NO re-conversion. The bake samples the cached grid through the
  prim's inverse transform — same cost class as moving any analytical prim (partial rebakes already
  touch only ~9–11% of voxels).
- The VP volume-preview shader is UNTOUCHED — it raymarches the per-BODY baked texture and never
  evaluates prims. Only `volume_bake.wgsl`'s `eval_prim` gains a branch. For scale: the VP shader
  already does dozens–hundreds of trilinear samples per PIXEL per frame and holds ~3.5ms @128³.
- Re-conversion happens only on actual mesh-geometry edits, and region-scoped it's 8–27ms debounced —
  the same round-trip the sculpt MOVE tool already ships (`input_sculpt.rs:828-942`).

## 4. Dead-ends / graveyard  (do NOT retry — this is the whole point)
*(Paid for in iteration 1 — caught by adversarial review, fixed before commit:)*
- **Authoring the operand grid in world space and sampling it with the bake's `world_pos`** —
  `world_pos` in `volume_bake.wgsl` is MISNAMED: under `BODY_LOCAL_PRIMS` production bakes are in the
  TOP body's LOCAL frame (`bake_dispatch.rs` grid AABB from local prims; serialize with `Vec3::ZERO`).
  A world-space grid sampled with local coords lands the imprint displaced/rotated by each body's own
  transform — and the origin-ZERO bench can't catch it (local == world there). Fix shipped:
  `BakeParams.body_local_to_world` (mat4 appended, 144→208 B; `brick_fill.wgsl`'s params copy reads
  only <144-byte offsets — untouched). Any future per-body operand data must think in this frame.
- **Rebaking via `mark_all_bodies_dirty` + `trigger_body_vol_bake` alone for a non-prim bake input** —
  `trigger_bake`'s source-hash skip (`bake_dispatch.rs:351`) DROPS the dirty mark when prim data is
  unchanged. Any bake input living outside prim data MUST be mixed into the source hash
  (shipped: `lab_operand_gen`). "Mark dirty + trigger" is NOT sufficient invalidation here.
- **Feeding arbitrary poly bodies into `region_mesh_to_sdf` unguarded** — SignMethod::Normal signs by
  winding with a closed-mesh assumption; a poly PLANE (creatable primitive) or user-deleted face makes
  half the padded cube negative → union floods every bake solid. Gate on half-edge topology
  (`any(twin == NO_TWIN)` = open → skip), which is authoritative and free — positional welding or
  index-based watertight checks on `to_raster_data` output DON'T work (it splits verts per face).

*(Pre-seeded from recon — landmines confirmed in code, not yet paid for in blood:)*
- **Per-prim sampled 3D TEXTURE binding in the bake shader** — worker GPUs bind a 1³ dummy at
  binding(3) and can't see the primary's atlas (`gpu_jobs.rs:341-350`); a texture operand would work on
  the primary GPU only. Use the **clip_curves side-buffer model** instead (HOT header field →
  `@binding(4)`-style shared storage buffer, `volume_bake.wgsl:107-111`) — identical on all GPUs.
- **`SignMethod::Raycast` for the conversion** — inverts sign on axis-aligned/coplanar faces (verified,
  documented at `mesh_to_sdf.rs:202-212`). Always `Normal` (winding), as `region_mesh_to_sdf` does.
- **Serializing the sampled grid into .fl4** — bincode field trap (see
  `project_bincode_save_format_field_trap`); grids are DERIVED. Persist source-node ref + res, re-derive
  on load (async, draft-first).
- **Inserting an `SdfShape` variant mid-enum** — bincode is positional; corrupts every save. Append at
  END only.
- **Assuming an unknown `st` is harmless on GPU** — `eval_prim` chains (`volume_bake.wgsl:580-614` and
  the march/preview parity copies) have NO default branch: unhandled shape ⇒ `d = 0.0` ⇒ "surface
  everywhere" garbage. Any new variant needs an explicit branch (or a far-distance guard) in EVERY
  shader that switches on `st`, even the deprecated analytical ones.

## 5. Cross-subsystem notes & collisions
- **Files this will touch** (MASTER: watch for collisions with poly/sculpt/cache/viewport windows):
  - `core_math/src/sdf_shapes.rs` (+`SdfShape::MeshVolume` appended at END) or a per-prim field à la
    `sculpt_layers` — decision pending (§6)
  - `core_math/src/primitives.rs` (`world_distance` CPU sampling, `sculpt_layer.rs` grid precedent)
  - `graphics/src/serialize.rs` (HOT header fields; strides 292/92/224 must stay in agreement)
  - `graphics/src/volume_bake.wgsl` (`eval_prim` branch + operand side buffer)
  - **BGL ×3 in lockstep if any binding changes:** `gpu_jobs.rs:257`, `app_poll.rs:340`,
    `app_update.rs:542` (+2 bind-group builders)
  - `io_bridge/src/mesh_to_sdf.rs` (conversion — likely unchanged, already has `region_mesh_to_sdf`)
  - `app/src/body_volume_cache/*` (operand grid cache + dirty propagation), scene graph (node ref),
    properties panel + outliner (assign UI), undo (`transaction()`)
- **Invariants that bind:** Display Invariant (mesh enters as DATA in the bake, never a render path —
  safe by design); Frame Ordering Contract + generation counters for the operand grid GPU buffer;
  transaction/undo for assign/stamp; derived-data-not-serialized; shader-parity landmine above;
  poly pillar red lines (NO `HalfEdgeMesh` fields — grid cache lives app-side / `PolyMesh` runtime side).
- **Benchmark artifact:** `io_bridge/tests/mesh_sdf_perf.rs` (`#[ignore]`d; release-only run) +
  glam dev-dep in `io_bridge/Cargo.toml`.

### Iteration-1 implementation footprint (for MASTER collision watch)
`graphics/src/bgl.rs` (+`buf_rw`/`sto3d`/`bake_bgl_entries`), `graphics/src/volume_bake.wgsl`
(binding 5 + `sample_mesh_grid` + union + BakeParams mat4), `app_gpu/src/gpu_jobs.rs` (BakeParams
+ IDENTITY_MAT4 + worker dummy), `app/src/{gpu_init,gpu_ctx,app_poll,app_update,app_new,main,
win_tweaks}.rs`, `app/src/lab_mesh_sdf.rs` (new), `app/src/body_volume_cache/{mod,bake_dispatch}.rs`
(`lab_operand_gen` in source hash), `app/src/sdf_gpu_mesh/tests_bake.rs` (+bench).
**Known spike limitations (accepted + documented, not bugs):** worker-GPU and CPU-fallback bakes bind
the disabled dummy → no operand on those paths; transform-only body moves (zero-rebake) drag the baked
imprint until the next rebake; body scale ≠ 1 mixes world/local distance metrics (surface position
stays exact; Lipschitz >1 possible for the VP march when scale < 1).

## 6. Open questions / risks
- **Enum variant vs per-prim field?** `SdfShape::MeshVolume{..}` (enum-native, forces exhaustive-match
  updates everywhere — good guardrail) vs a per-prim `Option<MeshOperand>` field like `sculpt_layers`
  (less invasive, but bincode struct-field trap applies to `SdfPrimitive` — would need the FL4
  side-channel). Leaning enum-variant-appended-at-END + grid re-derived (no serialization of the grid).
- **Open / non-manifold source meshes:** winding sign is unreliable. Need `is_watertight` gate +
  fallback (unsigned shell? warn toast? refuse?). Poly pillar meshes are usually closed but user meshes
  won't always be.
- **Dirty propagation across pillars:** poly edit → which SDF bodies reference this node → mark operand
  grid stale → debounced region re-convert → `mark_body_dirty`. Needs a reverse-reference map; must not
  couple the poly pillar to SDF internals (probably an app-layer watcher on mesh generation counter).
- **Resolution policy:** per-operand res (64 draft / 128 default / 256 opt-in)? Memory budget for many
  operands (8.4 MB each @128³ f32; R16 halves it)?
- **256³ full convert is 5s** — must never block; background + draft-first is mandatory, and region
  updates should be preferred even at high res.

## 7. Next step — ITERATION 2: `SdfShape::MeshVolume` primitive (destructive convert)

**Architecture** (per owner-confirmed purpose above):
- **`SdfShape::MeshVolume { grid: Arc<MeshSdfData> }`** — new variant APPENDED at the END of the enum
  (bincode rule; a new variant with fields is safe, old .fl4 never had it). `MeshSdfData { res: u32,
  bounds_min: Vec3, cell: f32, dist: Arc<Vec<f32>> }` — the body-LOCAL sampled grid. `Arc` = cheap undo
  snapshots; heap = no 280-float stack overflow. Grid serializes inline with the prim (self-contained;
  destructive convert means there's no poly left to re-derive from, so the grid IS the authored data).
- **Body-local, not world:** convert the poly's world tris INTO the target body's local frame ONCE at
  creation → grid is body-local → bake samples it with the body-local eval point directly. This RETIRES
  the i1 `body_local_to_world` matrix hack (the prim is inside the body, so no per-body transform).
- **GPU eval (the reuse):** repurpose the i1 `mesh_grid` @binding(5) from ONE global grid to a MULTI-grid
  side buffer (clip_curves pattern): pack each MeshVolume prim's grid, store its float offset in a spare
  HOT field, and sample it INSIDE the scope stack machine in `eval_prim` (st == MESHVOL, new st value
  after CurveExtrude) — so boolean+blend apply per-prim automatically. Reuse `sample_mesh_grid`
  parameterized by offset.
- **CPU eval:** `SdfShape::distance` MeshVolume arm trilinear-samples `grid.dist` → picking/tools/DC-CPU
  work unchanged (the `sculpt_layers.delta_grid` sampling precedent).
- **Convert UX:** `convert_poly_to_sdf()` (scene_actions) — gather closed poly world tris → body-local →
  adaptive res (tight AABB, ~192–256³) `region_mesh_to_sdf` → create SdfPrimitive{MeshVolume, op=Union
  default} in a NEW SDF body wrapping it (literal "wrap into a sdf body") → remove the poly →
  `transaction()` for undo → mark dirty + rebake. Trigger = outliner/viewport context-menu item or a
  button on a selected PolygonalBody.

**Build order (DONE):** (1) core_math MeshSdfData + variant + CPU distance + round-trip unit test ✓;
(2) graphics serialize st + side-buffer pack ✓; (3) volume_bake.wgsl eval branch ✓; (4) app convert
action + UI ✓; (5) retire i1 global toggle from the union path ✓.

### i2 KNOWN LIMITATIONS (documented, not bugs — for MASTER + next iteration)
- **Res capped at 128³** (owner wanted higher). The per-prim grid is (re)serialized + uploaded to the GPU
  EVERY bake (clip_curves-style side buffer), so 8 MB is the safe per-bake cost. **Next opt:** GPU-resident
  mesh buffer keyed by a content hash so a static converted mesh isn't re-uploaded per bake → then bump to
  192–256³. The tight FIT is already delivered (bigger half of the surface-quality win).
- **Worker GPUs bind the disabled dummy** at binding 5 (mesh_grid_floats not plumbed through
  `GpuJobKind::BodyVolumeBake`) → a MeshVolume prim contributes NOTHING on a worker-baked body. Fine on
  single-GPU machines (no workers). Plumb it through for multi-GPU.
- **Domain-warp / surface modifiers not applied to MeshVolume in the bake** (`eval_mesh_volume` does
  rigid+scale+mirror only; CPU `world_distance` applies the full stack). Converted prims are created
  modifier-free so the gap is currently unreachable; port the warp block if MeshVolume ever gets modifiers.
- **`BakeParams.body_local_to_world`** (i1 vestige) is now UNUSED but still in the struct/WGSL — harmless
  16 floats; remove in a cleanup pass (touches gpu_jobs + WGSL + 2 app_poll + 3 tests).
- **`gpu.buffers.lab_mesh_grid_buffer` / `lab_mesh_grid_dummy` + `ensure_lab_mesh_grid_dummy`** now unused
  on the primary path (binding 5 is per-dispatch) — dead, remove in cleanup.

## 7. Next step
1. **Visual smoke test of the LIVE MODIFIER (needs the human):** poly cube + SDF body → outliner
   right-click the poly → "🔗 Link as SDF Modifier". Verify: poly hides, a linked "Mesh" prim imprints at
   the poly's position; unhide the poly (outliner eye), MOVE it → imprint follows live; enter poly edit,
   drag verts, release → imprint re-converts ~0.4s later; set the prim's boolean to Subtract + blend (X pie
   works too); "Make Static" freezes it; undo of the link restores poly visibility + removes the prim.
2. **i3 known limitations (logged, not bugs):** body rotation/scale not compensated in the follow
   (translation only vs body origin — bodies are usually untransformed); refresh converts serially (one at
   a time, 128³ ≈ 0.5–2.5s debug); deleting the source poly freezes the imprint silently (properties shows
   "(deleted)"); linked-prim gizmo isn't locked — moving it is overwritten next frame by the follow
   (effective lock; proper UI lock = polish).
3. **Then, in priority order:** (a) GPU-resident mesh buffer (content-hash keyed) → unlock 192–256³ + make
   refreshes cheaper; (b) draft-res (64³) live refresh DURING edit drags, refine on release (the "live
   draft" option deferred from the owner's choice); (c) carry the poly's material onto the prim;
   (d) the i1-vestige cleanup (`body_local_to_world` in BakeParams, `lab_mesh_grid_*` GpuCtx fields).

### Deferred to later iterations
Live-modifier mode; materials carried from poly; analytical-path parity (dead path — skip); sharp-feature
preservation (NOT wanted per owner).
