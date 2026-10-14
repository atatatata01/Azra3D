# AO Bake Backend — Design Plan

**Status:** DESIGN (read-only investigation, no code). Worktree `feat/ui-polish`.
**Scope:** replace the fake AO (`NodeType::AmbientOcclusion` evals a synthetic sine field)
with a REAL mesh-level ambient-occlusion bake, per `MATERIAL_PANEL_SPEC.md` §10 gap 1 +
C3 (bake-as-cache) + D6/D7 (raster AO is Substance-style PREBAKED with explicit rebake +
stale badge; PT does AO live in-shader and uses NONE of this).

All file:line references are as-of this investigation (2026-07-26). Re-grep before editing —
several are approximate (`~`).

---

## 0. Executive summary (the recommended architecture)

Cast occlusion rays on the **CPU against the same triangle BLAS/TLAS the path tracer already
builds** (`render/src/pt_accel.rs`) — it is CPU-resident (`Vec<PtVertex>`/`Vec<PtTri>`), keyed
by `MeshCacheId` + `source_hash`, has a golden CPU tracer (`trace_scene_cpu`), and is fed by a
readback path (`dispatch_sdf_display_readback` → `mesh_cache.update_editable_mesh` →
`mesh_cache.pt_source_data`) that ALREADY solves P7's "SDF display mesh is GPU-only, no CPU
write path" blocker. Store the result as **one AO scalar per CPU vertex** in a per-`MeshCacheId`
side-table (no format change, no UV dependency, works for every body type the BLAS covers), keyed
for staleness on `pt_stamp`/`content_stamp` (which derives from the ONE body identity
`bake_utils::body_content_id` → `BodyContentId`). Dispatch the bake as a **background job drained
in `poll_background_tasks`**, reusing the exact `mesh_cache`/`pt_accel` rayon-spawn + generation-
counter + poll-drain pattern (the material bake in `node_graph_bake.rs` and the PT accel are both
this shape) — never per-frame, fired by the explicit D7 "Rebake AO" button. The baked AO reaches
the deferred shade by riding the **existing per-vertex `uv`→fragment FLAT channel** that voxel
vertex-paint already proved (`render/src/gbuffer.wgsl` `out.uv = in.uv` ~:301, consumed ~:463),
gated by a new instance-flag bit, and multiplying the **ambient term only** — exactly where the
existing SSAO factor is already applied (`deferred_entry.wgsl` ~:665, `lit_ambient * lit_ao_only *
lit_ssao`), so baked mesh-AO becomes a fourth ambient-occlusion multiplier alongside SSAO/GTAO.
PT is untouched: it integrates occlusion live over the same BLAS.

**Honest recommendation on method:** ONE method (CPU ray-cast over the PT BLAS) for ALL body
types in v1 — NOT per-body-type. The BLAS already unifies SDF-DC / poly / voxel / imported meshes
behind `MeshCacheId`, so there is no reason to write an SDF-field hemisphere sampler AND a triangle
sampler. The SDF-field-direct alternative is REJECTED below (§1) because it re-introduces a second
evaluation code path (C3 P0 bug class) and does not cover poly/imported meshes.

---

## 1. Method — WHAT AO actually needs (file:line evidence)

Real AO = for each surface point, the fraction of the hemisphere blocked by nearby geometry.
That is occlusion ray casts. Two candidate substrates exist in THIS engine:

### (A) CPU ray-cast over the PT BLAS/TLAS — **RECOMMENDED**
- `render/src/pt_accel.rs`: `PtAccelCache` holds per-body `BlasBuilt { verts: Vec<PtVertex>,
  tris: Vec<PtTri>, nodes: Vec<BvhNode>, source_hash, cache_id }` (:79-88). CPU-resident. Keyed
  by `MeshCacheId` + `source_hash`.
- `trace_scene_cpu` (:491) + `trace_instance` (:514) + `ray_tri_mt` (:567): a complete, tested
  CPU ray tracer over the assembled TLAS→inv_model→BLAS→Möller-Trumbore. This is the golden
  oracle for the WGSL PT and is **directly reusable** for AO occlusion queries — an AO ray is a
  shadow ray with a short `t_max`.
- The blocker P7 flagged (SDF display mesh is GPU-resident, no CPU read/write path,
  `MATERIAL_PANEL_SPEC.md` §10 gap 6) is **already solved for the BLAS path**: the SDF display
  mesh is read back to CPU via `dispatch_sdf_display_readback` (`pt_scene.rs:350`) →
  `mesh_cache.update_editable_mesh(cid, (verts, indices), tris)` (`pt_scene.rs:229`), and
  `mesh_cache.pt_source_data(id)` (`render/src/mesh_cache.rs:651`) returns owned
  `(Vec<[f32;8]>, Vec<u32>)`. So whenever the BLAS exists, a CPU vertex+index copy exists.
- **Caveat (the honest cost, §0.5):** that readback is currently GATED behind PathTrace mode OR
  RT shadows (`pt_scene.rs:514` `update_pt_accel` early-returns if
  `shading_mode != PathTrace && !rt_shadows_active()`). An AO bake in the raster viewport must
  either (a) trigger the SDF-display readback on demand for the target body (one-shot, not per
  frame — acceptable for an explicit button), or (b) require the body already have a CPU mesh
  (editable/imported/voxel bodies always do; brick-resident SDF ≥512 needs the snapshot). See
  Phase A dispatch for the on-demand readback kick.

### (B) GPU compute hemisphere sampling against the SDF field directly — **REJECTED for v1**
- `SdfFieldSources` (`node_graph_bake.rs` ~:180) + `FieldEvaluator` (`core_math/field_eval.rs`)
  can march the SDF field. An AO ray could sphere-trace the field instead of ray-tri.
- Cheaper per-ray for SDF bodies (no BLAS), reuses existing field eval. BUT:
  1. Covers ONLY SDF bodies — poly/voxel/imported meshes have no field (`has_field()` false,
     the exact garbage-fallback P6 gated in §10 gap 3).
  2. It is a SECOND occlusion evaluation code path parallel to the PT's triangle occlusion →
     the C3 "same evaluation code path for all rungs, divergence is a P0 bug"
     (`MATERIAL_PANEL_SPEC.md` C3, ~line 100) violation, the same trap P7 hit.
  3. The result would differ from PT's live triangle occlusion (PT traces the DC mesh, not the
     analytic field) — the "baked (raster) vs live (path traced)" split D7 requires would then
     show two DIFFERENT occlusion definitions, not one cached vs one live.
- **Verdict:** do not build a field-direct AO path. One triangle-occlusion definition, cached
  for raster, live for PT.

**Per-body-type?** No. The BLAS abstracts body type. v1 = one method, all bodies whose BLAS is
buildable. (Bodies with no triangle mesh at all — none in practice, everything bakes to
triangles per the Display Invariant — are simply skipped.)

---

## 2. WHERE it's computed — dispatch site + job pattern (reuse, don't invent)

- **Frame-ordering:** AO bake is GPU-optional but at minimum a background CPU job that produces a
  buffer; any GPU upload of the result MUST land in `poll_background_tasks`
  (`app_poll.rs:10`) per the Frame Ordering Contract. `update_pt_accel()` is already called there
  (`app_poll.rs:42`) — the AO poll step slots in adjacent to it.
- **Job pattern to reuse (three existing precedents, all identical shape):**
  1. `PtAccelCache::request` (`pt_accel.rs:225`) — `rayon::spawn` a pure build fn, `mpsc` result,
     per-key generation counter, `poll()` drains and discards stale generations (:268).
  2. `MeshCache` background builds (`mesh_cache.rs` :208-343) — same shape.
  3. `node_graph_bake.rs` material bake (:411 `bake_node_graph_maps`) — CPU eval on a background
     thread, "UI only clones the graph + spawns the job" (:7), drained in poll.
- **Recommended:** a new `AoBakeCache` mirroring `PtAccelCache` exactly — `request(cache_id,
  content_stamp, verts, indices, blas_or_rebuild, params)` → `rayon::spawn` the AO integrator
  (cast N rays/vertex against the body's BLAS via `trace_scene_cpu`-style occlusion) → `mpsc` →
  `poll()` drains, discards stale generations, uploads the per-vertex AO buffer. This gives free
  staleness (generation counter), free cancellation of superseded bakes, and Frame-Budget
  compliance (cap requests/frame like `MAX_PT_BLAS_REQUESTS_PER_FRAME`).
- **Explicit, not per-frame:** D7 says re-bake on an explicit button. So `request` is called from
  the "Rebake AO" panel action, NOT every poll. The poll step only DRAINS results + uploads. (An
  optional on-idle auto-rebake is a later nicety; v1 is button-only per D7.)

---

## 3. WHERE it's stored — per-vertex AO scalar (recommended), with tradeoffs

Spec is explicit: AO is **mesh-level, stored on the mesh not the material**
(`MATERIAL_PANEL_SPEC.md` P7 ~line 425: "AO bakes are stored on the mesh… per-material 4K bakes
will exhaust VRAM").

### (a) Per-vertex AO scalar in a per-`MeshCacheId` side-buffer — **RECOMMENDED**
- One `f32` (or packed u8) per CPU vertex, in a parallel `HashMap<MeshCacheId, Vec<f32>>` beside
  the mesh, uploaded as a per-vertex GPU storage/vertex buffer.
- **Does the P7 GPU-resident-mesh blocker apply?** P7 (§10 gap 6) could not STAMP CPU-evaluated
  values into the GPU-resident SDF display vertex buffer (`SdfBodyMeshSlot.verts`, usage
  `STORAGE|VERTEX|COPY_SRC`, no `COPY_DST`). **This design sidesteps it entirely:** the AO value
  is computed on the CPU copy (`pt_source_data`, which has stable vertex ordering matching the
  readback) and stored in a SEPARATE buffer, not written back into the resident display mesh. A
  parallel per-vertex buffer is created fresh with `COPY_DST` — no existing buffer's usage flags
  change. This is exactly P7b "Alternative (3): a dedicated post-mesh… separate small per-vertex
  storage buffer" (`MATERIAL_PANEL_SPEC.md` ~line 688), and P7b's objection to it ("same dual-path
  problem unless (1) lands first") does NOT apply here because AO is NOT the curvature generator —
  it has no live SDF-analytic twin to stay parity with (D7: raster is baked-only, PT is live; there
  is no live-raster AO rung to diverge from).
- **Vertex-ordering caveat (must verify in Phase A):** the AO buffer indexes must match the buffer
  the raster draw uses. Two cases: (i) editable/imported/voxel bodies draw from the SAME CPU
  `imported_raster`/`cpu_mesh` that `pt_source_data` returns → ordering trivially matches. (ii)
  SDF-DC bodies draw from the GPU-resident `SdfBodyMeshSlot.verts` produced by the mesher, while
  the AO is computed on the READ-BACK copy — the readback must preserve vertex order 1:1 with the
  resident buffer. `dispatch_sdf_display_readback` reads the resident verts directly, so order is
  preserved; **verify this holds under partial/windowed rebakes** (the gizmo-drag partial-rebake
  path) — if the resident mesh re-extracts with a different vertex count/order between the AO bake
  and the draw, the AO indexes go stale. This is the staleness signal (§4) doing its job: any
  re-extraction bumps `content_stamp` → AO marked stale → badge + rebake. The AO buffer is only
  VALID while `content_stamp` matches; on mismatch the shader falls back to AO=1 (no darkening),
  never garbage.
- **VRAM:** 1 scalar × vertex count. A 512³ SDF body ~hundreds of K verts → ~1 MB. Trivially
  cheaper than any texture atlas. Scales to many bodies.
- **No UV dependency** (the P7/spec warning: meshes "frequently unwrapped badly or not at all").
- **Interpolation:** per-vertex → interpolated across the face by the rasterizer, exactly like
  the voxel vertex-color route. Approximate below vertex spacing (stated honestly in the panel,
  same caption language P7 already ships for the vertex rung).

### (b) Texture atlas keyed by mesh — **REJECTED for v1**
- Needs a UV set + resolution. Spec P7: "a bake button that requires clean UVs fails exactly when
  it is most needed." Export-oriented, not the modelling path. Defer to a later export feature.

### (c) Sparse per-body AO grid sampled in the shader — **REJECTED for v1**
- A 3D AO grid (like the volume bake) sampled at the world position. No UV, no vertex-order
  dependency. BUT: another large GPU resource per body (VRAM), another sampling path in the
  deferred shader, and resolution/quality tradeoffs. Heavier than per-vertex for smooth AO. Keep
  as the fallback IF the vertex-order caveat above proves fragile under partial rebakes.

**Storage verdict:** per-vertex scalar side-buffer (a). Grid (c) is the documented fallback.

---

## 4. STALENESS — invalidation key (C3/D7)

- **Key on `content_stamp` / `pt_stamp`** (`mesh_cache.rs:642` — "bumped on every content
  mutation, all mesh kinds"), which is the cache's opaque copy of the ONE body identity
  `bake_utils::body_content_id` → `BodyContentId` (`bake_utils.rs:425`, "the ONE graph-level
  body-content identity"; `mesh_cache.rs:705` note). This is the SAME key the PT BLAS uses, so
  AO invalidates exactly when the mesh does — no new invalidation system.
- **Signal:** store `AoBakeCache { built: HashMap<MeshCacheId, { ao: Vec<f32>, stamp: u64 }> }`.
  `is_current(id, stamp) = built[id].stamp == stamp`. Mesh edit → `content_stamp` bumps →
  `is_current` false → **stale**.
- **Panel behaviour (D7 + P6 scaffold):** stale → show the stale badge (the badge slot exists,
  `material_panel/view.rs:1704` `fn badge`, currently used for graph/map/conflict) + enable the
  "Rebake AO" button. The button calls `AoBakeCache::request` with the current stamp. **Never
  display stale AO without saying so** (P7 rule) — the shader gate (§5) drops to AO=1 when the
  bound buffer's stamp ≠ the mesh's current stamp, so a stale bake never darkens wrongly; the
  badge tells the user to rebake.
- **C3 compliance:** AO is a CACHE, not a mode. Live definition (PT) is authoritative; the raster
  bake populates a cache; unbaking = drop the buffer (AO=1 fallback); a file opens identically
  with or without the cache; stale falls back to live (AO=1 in raster; PT unaffected). The AO
  buffer is NOT serialized in v1 (regenerate on demand) — matches `pt_accel` (never serialized)
  and avoids the FL4 format-extension surface.

---

## 5. SHADER CONSUMPTION — how baked AO reaches the shade (raster) + PT untouched

### Raster deferred path
- **Route:** per-vertex AO → vertex shader → FLAT-interpolated → fragment, then multiply the
  **ambient term only**.
- **Carrier:** reuse the proven voxel-vertex-paint channel. `render/src/gbuffer.wgsl`: `out.uv =
  in.uv` (~:301) carries a per-vertex payload vertex→fragment FLAT; the paint path packs a colour
  word into `uv.x` gated by `INSTANCE_FLAG_VERTEX_COLOR` (bit 0, `mesh_gpu.rs:332`), consumed at
  the fragment material eval (~:463, `use_vcol` branch). AO composes WITHOUT conflict: an SDF DC
  mesh / editable mesh is never a painted voxel mesh, so a **new instance-flag bit
  `INSTANCE_FLAG_VERTEX_AO`** + a parallel per-vertex AO attribute (or a second FLAT varying)
  carries the AO scalar independently. (Cleaner than reusing `uv` — AO wants its own varying so it
  coexists with real texture UVs; add one `f32` vertex attribute + one FLAT `@location` out.)
- **Write into the gbuffer AO channel:** the material gbuffer output already has an AO slot —
  `location(1) = material : vec4(roughness, metallic, shadow_ao, opacity_em)`
  (`gbuffer.wgsl:12,431`). The fragment multiplies the baked vertex AO into `shadow_ao` (the `.z`
  channel) so it flows through the existing deferred read (`deferred_entry.wgsl:313`
  `var shadow_ao = gbuf_m.z`).
- **Ambient-only application (already the right spot):** `deferred_entry.wgsl:665`
  `lit_color = lit_albedo * (lit_diff + lit_ambient * lit_ao_only * lit_ssao) + lit_spec`. The
  baked mesh-AO multiplies into `lit_ao_only`/the ambient factor here — it becomes a fourth
  occlusion multiplier ALONGSIDE the existing SSAO (`lit_ssao`, :664) and GTAO bent-normal
  (:186-224). This is the key insight: **the deferred shader already occludes ambient**; baked
  mesh-AO joins that composition, it does not build a new one. (SSAO = screen-space contact; baked
  mesh-AO = large-scale cavity occlusion the screen-space pass misses. Complementary.)
- **AO generator layer vs a direct AO channel:** the P6 AO *generator source* is one consumer
  (its output feeds a channel's map). The simplest v1 wiring is a **direct AO channel** driven by
  the baked buffer (the material's AO strength scales it), which is what the "baked (raster) /
  live (path traced)" split describes. The generator-layer un-gate (§6) is the panel affordance;
  the actual darkening is the gbuffer `shadow_ao` multiply above.

### Path tracer — CONFIRMED untouched
- PT integrates occlusion live in-shader over the same BLAS (`pathtrace_tri_entry.wgsl`; D7:
  "the path tracer evaluates AO live in-shader… no bake needed there"). The AO bake buffer is
  never bound in the PT pass. The panel label must read **"baked (raster) · live (path traced)"**
  (D7 exact requirement).

---

## 6. PANEL WIRING — un-gate the P6 scaffold (minimal changes)

The scaffolding exists; the backend un-gates it.

- `material_panel/lower.rs:339` `generator_source_supported(GeneratorSource::AmbientOcclusion)`
  currently returns false (asserted at :1685). Flip to true once the backend lands.
- `material_panel/view.rs:1305-1316`: the AO combo entry is `add_enabled(false, …)` with the
  disabled tooltip "raster AO needs a prebaked mesh-AO backend that does not exist yet." Enable it
  and swap the tooltip for the D7 label.
- **Add to the generator/AO layer row:** a rung badge `"baked (raster) · live (path traced)"`, a
  **"Rebake AO"** button (calls `AoBakeCache::request(active_body_cache_id, current_stamp, …)`),
  and the **stale badge** (reuse `fn badge` at `view.rs:1704`; wire visibility to
  `!AoBakeCache::is_current(id, stamp)`).
- `material_panel/model.rs:295` `GeneratorSource::AmbientOcclusion` already exists; keep it.
- **Active-body scope caveat (§10 gap 5):** the panel gate + bake must resolve the body the
  material is ASSIGNED to, not the active body, OR (v1, matching P6) read the active body's
  cache_id and document the same limitation. Keep gate and bake reading the SAME source so they
  never disagree (the P6 discipline).

---

## 7. Phased build

| Phase | Content | Size | Risk |
|---|---|---|---|
| **A** | `AoBakeCache` (mirror `PtAccelCache`): per-vertex CPU AO integrator over the body's BLAS via a `trace_scene_cpu`-style occlusion query (N hemisphere rays, cosine-weighted, short `t_max`), `rayon::spawn` + generation counter + `poll()` drain in `poll_background_tasks`. On-demand SDF-display readback kick for the target body if no CPU mesh yet. Golden CPU test (occlusion of a known concave shape). **SDF + editable/imported/voxel bodies whose BLAS is buildable** — the BLAS already abstracts type, so this is NOT SDF-only. | **L** | **Med** — vertex-order match between AO buffer and raster draw buffer for SDF-DC bodies under partial rebakes (§3 caveat); mitigated by stamp-gated fallback to AO=1. |
| **B** | Shader consumption: new `INSTANCE_FLAG_VERTEX_AO` bit + per-vertex AO attribute/varying (`gbuffer.wgsl`), multiply into `shadow_ao` (`.z`), verify it flows to `deferred_entry.wgsl:665` ambient multiply beside SSAO. Upload the per-vertex AO buffer (Frame Ordering: in poll). | **M** | **Med** — BGL/vertex-format touch (audit the binding sites per the BGL-single-source rule); shader recompile cost (batch into one edit). PT pass must NOT bind it. |
| **C** | Panel un-gate: `generator_source_supported` → true, enable combo entry, "Rebake AO" button, stale badge, `"baked (raster) · live (path traded)"` label; wire button → `AoBakeCache::request`, badge → `is_current`. | **S** | **Low** — scaffold exists. |
| **D** *(opt)* | Poly/imported-only hardening + texture-atlas export rung (b) for baked-AO export; on-idle auto-rebake. | **M** | Low — additive. |

Recommended v1 = A+B+C. B is the only shader/BGL-invasive phase; isolate + test
(`cargo test -p graphics` Naga parity after any shader edit).

---

## 8. OWNER DECISIONS needed

1. **v1 body-type scope:** all-BLAS-bodies (recommended — SDF-DC + editable + imported + voxel,
   one method) vs SDF-bodies-only first cut? (I recommend all-BLAS: the BLAS already unifies them,
   restricting to SDF adds gating work for no saving.)
2. **Per-vertex vs grid storage** if the SDF-DC vertex-order-under-partial-rebake caveat (§3(a))
   proves fragile: accept the stamp-gated AO=1 fallback (per-vertex, cheaper) or move to a per-body
   AO grid (c) (heavier VRAM + a new sampling path, but order-independent)?
3. **Ray count default** (quality vs bake time): e.g. 64 / 128 / 256 rays per vertex, and whether
   it's a fixed default or a panel quality knob. Bake is background + explicit, so a higher default
   is affordable; propose 128 with a low/med/high preset.
4. **Occlusion radius / `t_max`** default in world units (large-scale cavity vs contact). Propose a
   world-unit radius (C4-style) defaulting to ~a fraction of the body AABB diagonal, exposed as a
   knob — mirrors the P6 curvature "radius in world units" contract.
5. **Composition with existing SSAO:** baked mesh-AO `min()` with SSAO (physically the more
   occluded wins), or multiply (double-darkening risk), or replace SSAO for baked bodies? Propose
   `min()` (standard: take the strongest occlusion, avoid stacking).
6. **Serialize the AO cache in the .fl4?** Recommend NO for v1 (regenerate on demand, matches
   `pt_accel`, avoids FL4 format-extension surface). Confirm.

---

## 9. Disproportionate-cost flags (§0.5)

- **The readback gate (§1 caveat) is the single biggest risk.** The CPU mesh the AO baker needs
  only exists today when PT/RT-shadows is active (`pt_scene.rs:514`). For a raster-only user
  baking AO, the target body's SDF display mesh must be read back on demand
  (`dispatch_sdf_display_readback`). That readback is ~10ms GPU + hundreds of MB for a dense body
  (`pt_scene.rs:245` note), but it is ONE-SHOT per explicit bake (not per frame), so it is
  budget-compatible — as long as it's kicked from poll and drained async, never inline in the
  panel callback (Frame Ordering Contract). If a body is at brick-resident ≥512 with a huge tri
  count, the same `PT_SNAP_MAX_TRIS = 1_200_000` ceiling (`pt_scene.rs:213`) applies: reject +
  toast, AO unavailable for that body until Grid is lowered. **This is the honest limit to state
  in the panel.**
- **Vertex-order coupling for SDF-DC bodies under partial rebakes** (§3(a)) — the reason the grid
  fallback (c) exists. Not disproportionate if the stamp-gated AO=1 fallback is accepted, but it's
  the correctness edge to test hardest.
- Not disproportionate: the ray-cast itself (background rayon, CPU tracer already exists), the
  storage (per-vertex is ~1 MB/body), or the shader composition (SSAO ambient-multiply site
  already exists).

---

## 10. Reuse targets (registry)

| Need | Reuse | Location |
|---|---|---|
| Triangle accel + CPU tracer | `PtAccelCache` / `trace_scene_cpu` | `render/src/pt_accel.rs:188,491` |
| CPU mesh copy of SDF display mesh | `dispatch_sdf_display_readback` → `mesh_cache.update_editable_mesh` / `pt_source_data` | `pt_scene.rs:350,229`; `mesh_cache.rs:514,651` |
| Background job pattern | `PtAccelCache::request` (rayon+mpsc+generation) | `pt_accel.rs:225,268` |
| Poll dispatch site | `poll_background_tasks` / `update_pt_accel` | `app_poll.rs:10,42` |
| Staleness key | `content_stamp`/`pt_stamp` ← `body_content_id`/`BodyContentId` | `mesh_cache.rs:642,705`; `bake_utils.rs:425` |
| Per-vertex FLAT payload → fragment | voxel vertex-paint `uv` route + instance flag | `gbuffer.wgsl:301,463`; `mesh_gpu.rs:332` |
| Ambient occlusion multiply site | SSAO/GTAO ambient application | `deferred_entry.wgsl:665,664,186` |
| Gbuffer AO channel | `shadow_ao` = material `.z` | `gbuffer.wgsl:12,431`; `deferred_entry.wgsl:313` |
| Panel gate to flip | `generator_source_supported` + AO combo entry + `fn badge` | `material_panel/lower.rs:339`; `view.rs:1305,1704` |
| Bake job precedent | `bake_node_graph_maps` (background CPU eval, poll-drained) | `node_graph_bake.rs:411,7` |

---

*Written 2026-07-26 on `feat/ui-polish` — design only, no code, no commits.*
