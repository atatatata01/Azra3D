# GPU Mesh Extraction (Option A) — phased plan

**Status:** scoped 2026-06-21 (grounded in a code-mapping pass, adversarially reviewed).
**Companion:** `ROADMAP_MVP.md` (Phase G milestone) · `VOLUME_ADR.md` (volume strategy).

> Goal: move Object-Mode SDF-body mesh extraction off the CPU (~125 ms Dual Contouring) onto
> the GPU (~2–5 ms), so heavy edits (body bevel, blend-K, boolean, large moves) update fluidly
> and the live-edit "surface disappears" churn is closed **structurally**.

---

## 0. Decisions locked (2026-06-21, user directive)

- **Algorithm = QEF Dual Contouring (sharp).** NOT Surface Nets. The mesher MUST preserve bevels,
  boolean creases, and hard corners. It matches the existing CPU `chunked_dc` QEF reference — which
  also makes the G0 parity test exact (GPU vs CPU, identical algorithm).
- **One shared GPU mesher for BOTH SDF bodies AND voxel sculpt.** The voxel subsystem's Surface Nets
  (`voxel_mesh.wgsl`) is a lesser fit for hard-surface features. QEF DC is a strict quality *superset*
  (degrades to the Surface-Nets centroid on cells with no sharp feature → organic sculpts lose
  nothing, and gain crisp creases from flatten/pinch). Build the QEF DC mesher as a **shared core
  module**; wire SDF bodies first (G1–G4), then **migrate voxel sculpt onto it (G6)**.
- **GPU, not CPU.** The ~125 ms CPU DC is replaced outright as the production path.
- **Why voxel originally used Surface Nets** (context, not excuse): it fit organic clay sculpting where
  rounding is acceptable; a reasonable per-use-case choice, but unifying on the best algorithm is the
  correct architecture going forward.
- **The hard part is built honestly, not skipped:** per-cell QEF on the GPU needs a least-squares/SVD
  solve; WGSL Naga inlining may force a GLSL/SPIR-V entry point (as the BVH path already required). G0
  exists to build that shader RIGHT and prove parity with CPU *before* it touches the live view — the
  opposite of a band-aid.

> **Added phase — G6: migrate voxel sculpt onto the shared QEF DC mesher** (replace `voxel_mesh.wgsl`
> Surface Nets). Effort **M**; lands after the SDF path (G1–G4) is stable so sculpt isn't destabilized
> mid-flight. The mesher is designed shared from day 1 so this is an adoption, not a rewrite.

---

## Progress (2026-06-22, autonomous session)

- **G0 — DONE + committed** (`adc98cd`, `9bb2fe1`, `46c2829`). QEF DC shader written, Naga-validated,
  and parity-proven vs CPU `chunked_dc` (identical vertex counts on beveled box + boolean; non-symmetric
  box too). All three G0 risks retired: WGSL viable (Cramer's rule, no GLSL), compiles, output matches.
- **G1 core — DONE + committed** (`ad8b1c5`). `SdfMeshGpu` resident dispatch + GPU-generated
  `DrawIndexedIndirect` args, no readback, device-loss-safe (atomicMin clamp). Test proves the indirect
  index_count equals the real mesh count exactly.
- **G1 (per-body resident output pool) — DONE + committed** (`SdfBodyMeshSlot` + `GpuCtx.sdf_body_mesh`).
  Per-body slot owning verts/idx/indirect/field/params/cell_vert/counters; `mesh_into_slot` dispatches
  the QEF-DC passes into it (no mesh readback). **Scoping decision (can't-verify-live build):** all
  buffers are SLOT-OWNED → the bind group references nothing external → the `ResidentVolume.generation`
  device-loss hazard (map RANK-1) is kept OUT of G1; the `rv.buffer` no-readback INPUT optimization +
  its generation counter is deferred to **G3** (verifiable live). G1's field is uploaded from the CPU
  grid (same source as the readback path) → fully offline-testable. Test `sdf_dc_slot_parity`: the slot
  mesh is BIT-IDENTICAL to the golden (mean 0.0000, indirect index_count exact). Dormant (`GpuCtx`
  field empty) until G2 populates + draws.
- **G2 — DONE + committed** (`569a091`), behind the OFF-by-default `cache.gpu_sdf_mesh_resident`
  sub-flag (Developer Tweaks → "Resident no-readback"). Poll-phase `sdf_populate_resident` (ensure slot
  + `mesh_into_slot`, no readback) intercepts `dispatch_grid_dc_async` ahead of the readback path; the
  raster pass draws the slot via `draw_indexed_indirect` in BOTH the G-buffer pass and the picking pass
  (`draw_sdf_resident_slot`). A body with a populated slot bypasses the display_state + mesh_cache gates
  (slot existence = proof of a completed bake). Removes the mesh readback AND the per-frame 8MB grid
  re-upload (the residual drag cost). Shadow pass auto-skips resident bodies (no GpuMesh → no shadow,
  acceptable for the sub-flag). Frame ordering: slot created/resized + DC submitted in
  `poll_background_tasks`, raster submits later same frame → indirect args populated before the draw
  reads them; atomicMin clamp = no OOB / no device loss.
  - **Status: needs live user verification.** Toggle "Resident no-readback" ON → expect the surface to
    draw with zero per-frame readback/upload (buttery during drag) AND click-select still works (P0).
    If the surface vanishes or selection breaks, toggle OFF (falls back to the G-live readback path).
  - **Slot lifetime:** orphaned slots (body deleted / reparented-out / replaced by project load) are
    pruned in `process_dirty`'s SCENE block (`retain` keeps only slots whose NodeId is still a live
    SdfBody) — structural, not per-frame. No leak.
- **Live (readback path) — DONE + committed, behind flag** (`d7fa242`, `fd46523`, + readback-defer fix).
  `cache.gpu_sdf_mesh` (Developer Tweaks, OFF by default) routes `dispatch_grid_dc_async` to
  `gpu_mesh_to_cache`: GPU-mesh the body grid → `mesh_cache` → existing raster draw. Reuses the proven
  draw (no new device-loss path). Two drag-gates fixed so it updates LIVE during edits (not just on
  release): (1) `begin_episode` no longer suppresses bakes when GPU meshing is on; (2) `poll_primary_bake`
  no longer defers the readback during a drag when GPU meshing is on. Object Mode only; SDF Edit unaffected.
  - **Status: needs live user verification** (built behind a flag; couldn't verify live in the autonomous
    session). Expected: surface meshes live during bevel/move/blend, sharp features preserved, no
    stall/vanish (the generation race is closed). Residual: a per-frame blocking GPU readback (~few ms)
    caps drag framerate (~50fps, not buttery 60) — removed by G3.
- **G3 (resident DENSITY input, no grid upload) — NEXT, needs verification.** G2 already made the
  OUTPUT resident (no mesh readback) and removed the per-frame grid re-upload by uploading
  `slot.field` from the CPU grid only when a bake lands. G3 is the remaining INPUT optimization: feed the
  QEF-DC field directly from the bake's resident density (`output_buf` / `rv.buffer`) instead of the CPU
  grid, eliminating the `slot.field` write entirely. This is the device-loss-risky bit deliberately kept
  out of G1/G2 — the DC bind group would then reference an EXTERNAL resident buffer that can realloc, so
  it needs `ResidentVolume.generation` threaded into bind-group invalidation (Risk RANK-1). Can't be
  offline-verified → defer to a live session. The compute side is already proven (G1 core test).

---

## Progress (2026-06-28 — G3 done + verified, G4 cut-over landed)

The 2026-06-28 marathon (~12 commits `5b7c87a..5617bde`) effectively COMPLETED G3 and the G4 cut-over, and
advanced G5. The `[GpuMeshPath] field_direct=true → field-direct (rv.buffer)` trace is now the steady-state.

- **G3 — DONE + USER-VERIFIED.** The resident-DENSITY-input optimization landed: `mesh_into_slot_from_resident`
  meshes the QEF-DC field DIRECTLY from `rv.buffer` (no `slot.field` upload) when `resident_buffer_current()`
  holds (the `buf_valid && rv_ok && res_ok && bounds_ok` gate, `app_poll.rs:712-729`). The Risk-RANK-1
  realloc/dangle hazard is closed by the `resident_buffer_valid` content-identity flag + bake-bounds provenance:
  `accept_gpu_result` / field-direct now stamp from the bake's DISPATCH bounds (`PendingBake.bounds_min/max`),
  and `gpu_texture_upload_done` is guarded to the active body that actually wrote `rv.buffer` (+`gpu_texture_upload_body`).
  Generation-race + partial re-mesh: lazy readback skips the `to_vec` on partial bakes; the settle re-tighten
  restores `resident_buffer_valid`. **User-confirmed:** no disappear / no ghost / no stale frame on bevel+boolean;
  select/duplicate freeze gone (poll 13-49ms); escape bounded. The white-box ghost (a field-direct-at-wrong-bounds
  bug this very optimization exposed) was root-caused + fixed structurally. See COMPLETE_BUG_TRACKER `2026-06-28`.
- **G4 — cut-over LANDED (verify save/export readback preserved).** Defaults flipped ON (`0269893`:
  `gpu_full_bake_path_a`, `gpu_sdf_mesh_sparse`→ now gated dense ≤512); the GPU resident mesh is the production
  Object-Mode path. The on-demand CPU-DC fallback is PRESERVED (GATE-2 audit verified save→reload re-bakes every
  SDF body; duplicate re-meshes). **G4 housekeeping VERIFIED (2026-06-28):** the cut-over stranded NO on-demand
  consumer — export is self-contained (own `export_resolution` slider + a background bake from `scene.primitives`,
  `win_export.rs`; never reads the live `body_grids`/resident mesh); brick-classify is PARKED (early-returns on
  `!use_brick_pool`); save/duplicate keep `body_grids` (lazy readback skips the `to_vec` ONLY on partial bakes,
  full bakes still populate it). R16Float-only GPUs = **documented limitation**: the no-readback resident path
  needs R32F storage (the user's HW reports `FLOAT32_FILTERABLE: true`); on R16-only HW the CPU-DC readback path
  remains intact as the fallback (`gpu_sdf_mesh_resident` can be forced off). **→ G4 DONE.**
- **G5 — sparse UN-PARKED (pending live 512 verify), 1024 + S2.3 open.** Dense 512³ Object-Mode mesh works.
  The "sparse renders BLACK" theory was **empirically DISPROVEN** (2026-06-28): the NORMAL-parity test
  (`sdf_dc_sparse_brick_decode_parity`, extended from SPIKE B's position-only check) measures degenerate
  sparse=0/dense=0 and sparse-vs-dense normal angle mean=0.00°/max=0.03° on a 96³ box exercising all 3 brick
  tags — the sparse decode is bit-exact in position AND normal. The black was the white-box bounds bug (since
  fixed). Sparse RE-ENABLED for `slot.res>256` (`878bd31`): ≤256 dense, >256 sparse (~61MB pool vs 515MB dense
  field at 512³). **Needs live verify at 512³** (renders lit + VRAM drops). 1024³ tiled multi-buffer pool not
  started. Asymmetric per-axis savings (S2.3) deferred — see `S2.3_PERAXIS_MIGRATION.md` (105 sites / 57 crash).

**Net: the plan's CORE (G0–G4: GPU mesh extraction, no readback, generation-race closed, cut-over) is DONE.
What's left to "finish" = G5 high-res (un-park sparse via the normal fix; 1024 tiled pool) + the per-axis (S2.3)
restore + the G4 doc/verify housekeeping.**

---

## 1. Why (the real mechanism — corrected premise)

In **Object Mode** the GPU bake is **not** discarded before meshing — Object Mode runs CPU DC
(`force_volume_only` / no-DC is the *SDF-Edit-Mode* volume-raymarch path). The live-edit breakage is
a **generation race**:

- `trigger_bake` bumps the body generation every bake (`bake_dispatch.rs:286`).
- The CPU DC takes ~125 ms on a background thread (`[DC-bg]`, `chunked_dc.rs:620`).
- `poll_dc_jobs` discards any DC result whose `generation < current_generation` (`app_poll.rs:724-727`).

So under a *continuous* full-body edit (bevel slider, blend-K), each new bake supersedes the prior
generation **before its DC lands** → the DC never displays → the old mesh keeps drawing or the body
vanishes. A ~2–5 ms GPU mesh finishes within the bake cadence, so a new bake rarely supersedes an
unlanded mesh — the race closes structurally (a throughput fix, **not** a band-aid).

**What GPU meshing fixes:** live-bevel/blend/boolean churn + surface-disappear during continuous
edits; unlocks >256³ bodies CPU DC cannot mesh.
**What it does NOT fix:** recenter-shift, stale-region, invalidation-coherence bugs — those are
**logic** errors (Structural Mutation Checklist / dirty-flag / body-origin math). A faster mesher
meshes the *corrupted* grid just as fast. **Stabilize those on CPU DC first.**

---

## 2. Current vs ideal pipeline

**Current (Object Mode):** GPU bake → **(a)** dense f32 STORAGE `output_buf` (`app_poll.rs:964`)
**(b)** direct write to the resident R32Float 3D texture (`app_poll.rs:1067-1082`) → **readback**
`Vec<f32>` (`app_poll.rs:1496-1504`, exists *only* to feed CPU DC) → **second** `grid.to_vec`
(`app_poll.rs:589`) → **~125 ms CPU DC** (`chunked_dc.rs` QEF + tetra normals + repair) → `mesh_cache`
→ `upload_pending` CPU→GPU vertex/index (`mesh_cache.rs:349`) → `draw_indexed` (`raster_render.rs:982`).

**Ideal:** GPU bake → dense density buffer (already exists) → **GPU mesh compute** (QEF DC) writing a
**resident vertex/index/indirect pool** → `draw_indexed_indirect`. No staging, no `map_async`, no
`Vec<f32>`, no rayon DC, no `upload_raw`. Eliminates readback + 2nd copy + CPU DC + upload, and closes
the generation race.

---

## 3. Reuse vs build-new

**Reused (~80% of the plumbing — from `voxel_gpu_mesh.rs` + `voxel_mesh.wgsl`):** the entire
no-readback resident-pool architecture — shared `pool_verts`/`pool_idx`/`pool_indirect`/
`pool_counters`/`pool_params`, the `ensure_pool` poll-phase allocation (`voxel_gpu_mesh.rs:362`), the
GPU `index_count`→indirect-args copy with no CPU readback (`voxel_gpu_mesh.rs:676`), cached per-slot
bind groups with combined-generation invalidation (`voxel_gpu_mesh.rs:57-63`), the `atomicMin` +
overflow device-loss guards (`voxel_mesh.wgsl:191,246`), and `draw_resident_pool_with_instance`
multi-draw-indirect + per-slot fallback (`raster_render.rs:40-61`). **As input:** the bake's dense
`output_buf` and the already-resident R32Float texture — no new density resource. `MeshDensitySource::
Dense(&wgpu::Buffer)` (`voxel_gpu_mesh.rs:27`) already accepts exactly this format.

**Built new:**
1. **`graphics/src/sdf_mesh.wgsl`** — GPU Dual-Contouring with per-cell **QEF** (port `solve_qef`
   `chunked_dc.rs:436`), signed-distance sign/iso (solid `dist<=0`, iso `0`), tetra SDF-gradient
   normals (`chunked_dc.rs:48`) + degenerate-normal repair (`chunked_dc.rs:80`). The voxel mesher is
   **Surface Nets** (averages crossings, no QEF, no repair, occupancy iso 0.5) → reusing it as-is
   **rounds off sharp bevels/booleans** and reintroduces the flat-gray-normal bug. **This is the
   single largest new build and the central risk.**
2. **`app/src/sdf_gpu_mesh.rs`** — `SdfMeshGpu` owner + **per-BODY** slot map (bodies are dozens, not
   12k chunks) + adaptive (non-fixed-stride) slot sizing.
3. **`ResidentVolume.generation`** counter (currently absent, `gpu_volume.rs:181`) — folded into the
   slot bind-group invalidation, or a texture realloc dangles the BG → device loss.
4. Indirect-draw switch for the Object-Mode SDF draw (currently CPU-known `draw_indexed`).
5. GPU dirty-region partial re-mesh + lazy on-demand CPU-grid path for save/export consumers.

---

## 4. Phases

| # | Title | Goal | Effort | Key risk |
|---|---|---|---|---|
| **G0** | **Offline GPU-DC parity harness** (zero render-state) | Prove a GPU compute shader meshes the SDF grid at CPU-DC quality (QEF sharp features + repaired normals) **before** touching any live path. Debug-button, blocking readback, persists nothing. Golden parity test vs `chunked_dc` on a beveled box + boolean-subtract box. Decide **WGSL vs GLSL** for QEF here (Naga inlining may force GLSL, like the BVH path). | **L** | QEF may be infeasible in WGSL; Surface-Nets shortcut WILL fail the sharp-edge parity test — *that failure is the point of doing it offline*. |
| **G1** | **SDF resident mesh pool + Frame-Ordering alloc** (not drawn yet) | Stand up `SdfMeshGpu` in `GpuCtx` (mirror `VoxelMeshGpu`): one BGL, QEF + quad-emit pipelines, shared pools, per-body slot. Reuse `ensure_pool` poll-phase alloc + indirect copy + device-loss guards **verbatim**. **Add `ResidentVolume.generation`** + fold into combined-gen BG invalidation. Deferred-grow flag (never create a buffer mid-dispatch). Register owner in `GPU_INVALIDATION_MATRIX.md`. | **L** | Adaptive slot sizing is genuinely new (voxel fixed-stride doesn't transfer). `ResidentVolume.generation` must thread through every realloc site. |
| **G2** | **Indirect-draw behind a flag** (CPU DC still authoritative) | Draw the GPU pool in Object Mode behind `cache.gpu_sdf_mesh` (Developer Tweaks, default **false**). Switch the SDF body draw to `draw_indexed_indirect` for the pool case only (never read the count back — that reintroduces the readback). Dispatch in `poll_background_tasks` after the bake submit; BGs in `viewport_render`; per-frame cap + FrameScheduler ticket + Z-slice TDR guard. **Verify picking/selection still works** (Feature Dependency Audit). | **M** | Two live paths exist (acceptable only as a transition with a committed cut-over). **Picking regression = P0.** |
| **G3** | **Generation-race closure + partial GPU re-mesh** | Wire the slot's generation to `current_generation` so a stale slot is structurally overwritten (replaces the CPU stale-discard for the GPU case). GPU dirty-region partial re-mesh keyed on `last_bake_dirty_range` (mirror CPU `update_dirty_chunks`). **Stress-test continuous bevel/blend-K on a boolean body → no disappear/stale frame.** | **M** | Bake→mesh same-frame barrier subtlety. Partial-remesh slot management. |
| **G4** | **Cut-over: GPU default, CPU DC → on-demand fallback** | Flip default true on capable HW. **Keep** the readback **on-demand/lazy** for save/export/duplicate/brick-classify (never delete — data-loss risk). Keep `dispatch_grid_dc_async` as the export/save one-shot + no-indirect-feature fallback. R16Float-only GPUs (no direct texture write) fall back to CPU DC (documented). | **M** | Premature readback removal = save/export/duplicate data-loss regression. R16-only GPUs break the no-readback premise. |
| **G5** | **>256³ TextureOnly bodies** (new capability, deferrable past MVP) | Mesh large bodies CPU DC literally cannot handle: add a `texture_3d`/atlas sub-slab density-read path to `sdf_mesh.wgsl`. | **L** | Second shader input path; per-axis dims are a cubic-invariant foot-gun if half-done. Schedule only after G0–G4 stable. |

---

## 5. Top risks (ranked)

1. **Quality regression** — Surface-Nets reuse rounds off sharp edges/bevels/creases (no QEF) + drops
   normal repair → flat-gray-normal corruption. **A new QEF DC shader is mandatory**; if WGSL can't do
   QEF (Naga inlining), move to GLSL/SPIR-V. *Phase G0 retires this offline.*
2. **Device loss / TDR** — OOB `draw_indexed_indirect` → NVIDIA device loss. Reuse the `atomicMin`
   clamp + overflow guards verbatim; Z-slice large dispatches.
3. **Bind-group dangle** — `ResidentVolume` reallocs on body-switch/AABB-grow/res-change with **no
   generation counter today**. Add one and fold into slot invalidation (G1).
4. **Sequencing/entanglement** — a new GPU path on unstable invalidation logic makes both harder to
   debug. Gate-first, GPU-later; G0 parallel only because it's render-state-free.
5. **Premature readback removal** — readback also feeds save/export/duplicate/brick-classify. Keep
   on-demand; never delete CPU DC.
6. **Bake→mesh same-frame ordering** — submit bake before the mesh dispatch, or read prior-frame texture.
7. **Adaptive slot sizing** — SDF tri-count unbounded; count-first or grow-on-overflow.
8. **R16Float-only GPUs** — bake doesn't direct-write the texture; documented CPU DC fallback there.

---

## 6. Open questions (decide in G0/G1)

- **Quality bar:** must GPU mesh reproduce CPU-DC sharp QEF features, or is Surface-Nets rounding OK
  for a preview with CPU DC reserved as export LOD? (Editing bevels/booleans implies **QEF required** —
  confirm.)
- **WGSL vs GLSL** for the QEF solve (Naga inlining) — G0 answers before G1 commits a language.
- **Per-body pool vs shared slot-strided pool** — bodies are dozens; per-body may be simpler, but reuse
  the indirect-draw + generation machinery.
- **Field source** — dense `output_buf` (simplest) vs R32Float `texture_3d` (free trilinear; required
  for >256³). G0 starts with the buffer; G5 needs the texture.

---

## 7. Sequencing (one line)

**Stabilize the SDF spine on CPU DC (Roadmap Phases 0–2) → run G0 (offline parity) in parallel now →
cut GPU meshing into the live path (G1–G4) only once the invalidation/recenter logic is stable.**
