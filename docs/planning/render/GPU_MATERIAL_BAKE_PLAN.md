# GPU Material Bake — Plan

**Status:** GPU-resident procedural material preview implemented. Image/scene-dependent and normal/bump/displacement graph outputs retain the exact existing bake path. **Owner directive (2026-07-28):** *"GPU base is the future."*
**Goal:** move procedural material-map baking from a single CPU thread to a GPU compute pass.

## Current interactive path (2026-10-11)

- The existing graph compiler explicitly tags authored float parameters. GPU bake lowering moves those values into binding 5, a submission-owned read-only storage buffer. Exported WGSL retains literal values. The device-owned pipeline cache is keyed by lowered shader structure, not slider values; offsets and noise levels keep their structure even at default values.
- Material node-editor edits dispatch the real eligible graph directly into the shared material array, sampled by the unchanged triangle renderer. The array has an RGBA8 unorm storage view and an sRGB sampled view; channel-specific color/data encoding and adjacent 16-bit planes preserve the existing contract. No staging, readback, CPU encoding, or re-upload occurs on these slider updates.
- Structural pipeline compilation runs on background threads, capped at two jobs and 48 cached graph structures. Warm edits only update persistent parameter buffers; bind groups rebuild on material-array generation changes. Dispatch happens in the poll phase under a High-priority FrameScheduler ticket.
- Visible noise/Voronoi/Musgrave squares use the same graph compiler and resident compute path in a separate 64-square GPU array. Native egui texture IDs rebuild on array generation changes; updates are capped at two per frame. Unsupported upstream graphs retain the exact CPU preview rather than a fake GPU result.
- Only requested channel readbacks are copied and mapped. Completed readback decoding and map resizing run on the worker pool, with at most one finalization handed off per frame. Existing sequence checks still reject obsolete results before installation.
- The update indicator stays inside the existing footer, so bake status cannot change the graph canvas origin.
- GPU-authored map slots have GPU authority. Explicit save/export snapshots their latest pixels; autosave does not run during a slider episode or pending preview. Imported CPU maps retain their CPU cache. GPU array growth/resizing preserves resident layers; the project format stays at version 7. The fallback graph-bake result drain is capped at two per frame, and obsolete results are rejected when resident preview takes ownership.
- Headless regression covers slot reuse, 50 color edits without pipeline recreation, byte-exact dual-plane color encoding, capacity/resolution growth, and export snapshots. A 60-edit 1024-square noise test measured warm CPU submission p95 0.169 ms and GPU-complete p95 0.438 ms on the development GPU. These are isolated compute timings, not measured viewport FPS.
- The final 15-test GPU suite passed with a non-vacuous noise-pixel change assertion. With AZRA also running, the 1024-square test measured CPU submission p95 0.178 ms and GPU-complete p95 1.217 ms. Graphics (74), graph-bake (64), material-array (8), and shader-node (12) tests passed. The broader core-math suite has three unrelated existing edge-selection/dissolve failures.
- Native UI verification used this checkout's rebuilt executable: created a polygon cube, added its material, connected Noise to Albedo through the socket picker, and changed Scale from 4 to 12. The viewport visibly changed without a baking indicator or canvas shift. Automated mouse drags did not change the numeric value, so continuous slider smoothness is not visually verified; headless scrubbing and live numeric edits are verified separately.
- Resident preview also authors bump and normal maps directly on the GPU. Scalar-to-Master-Normal connections insert the existing editable Height to Normal node; its CPU and WGSL implementations differentiate the actual upstream graph rather than the synthetic analyzer field. Height-only connections author bump without implicitly generating a second normal map.
- Dual-plane high-precision maps use shared `material_precision.wgsl` float bilinear filtering in raster and triangle PT. Hardware sRGB filtering quantizes interpolated values on the tested GPU; rounding filtered high/low bytes introduces additional contour steps. Resident Bit16 bump heights store raw high/low bytes, sampled through a linear view of the same array. Runtime-only `TexMap.raw_precision` sets projection bit 11; legacy imported planes retain the sRGB decoder. Resize reconstructs raw height before interpolation and repacking. Save/export converts raw planes to the established legacy encoding without changing the v7 wire layout. Preview has no readback, additional texture allocation, or CPU baking. GPU probes cover carry boundaries and individual 16-bit steps near 0.5 and 1.0.
- This is a graph-compiled resident preview, not a full Substance-style per-node dependency texture cache. Unsupported image/scene inputs and displacement outputs still fall back to the established bake path. Do not describe those as zero-readback.
- Height precision regression: graphics 74, render 95, and resident-preview 3 tests pass, including raw-height resize across byte carries. Warm 1024-square compute completion p95 was 0.656 ms; this is not viewport latency. Rebuilt the same executable and checked Noise-to-Height on a polygon cube at extreme close-up: the original repetitive contour steps are no longer visible. Strong bump still produces harsh lighting patches; do not interpret this precision check as acceptance of all bump shading or continuous slider dragging.

The sections below retain the original planning baseline.

---

## 1. Why (measured, not assumed)

The bake probe (`%TEMP%\azra_bake_probe.log`, added in `ba65d62`) recorded real numbers on the
owner's machine:

| Observation | Value |
|---|---|
| Bake at `res=1024` | **~12.7 s each** |
| Bakes in one session | 3839 (418 at 1024) |
| Time in 1024 bakes alone | **~55 minutes** |
| Curvature on GPU fast path | yes — only 6 CPU fallbacks in 3839 bakes |

Curvature was **not** the bottleneck. The bake itself is, and it has three compounding causes:

1. **Single-threaded.** `spawn_graph_bake_quality` (`node_graph_bake.rs:1232`) is one
   `thread::spawn`. `grep -c "par_iter\|rayon" node_graph_bake.rs` → **0**, despite `rayon` already
   being a dependency of both `app` and `core_math`.
2. **A `HashMap` allocated per pixel.** `node_graph_bake.rs:1438` builds a fresh
   `HashMap<(NodeId, usize), Val>` for *every* pixel, then walks the graph with hashed lookups per
   node output. At 1024² that is ~1.05 M allocations plus millions of hash ops that are pure
   overhead, unrelated to the actual noise math.
3. **No preview tier.** `GraphBakeQuality`'s own doc comment (`node_graph_bake.rs:34`) says
   *"**Live == Final quality** … No downrez preview."* `bake_work_resolution` further forces full
   `MAP_RES` (1024) whenever the graph samples an ImageTexture.

---

## 2. Why the CPU bake exists (do not treat it as an accident)

`docs/planning/render/PROCEDURAL_TEXTURING_DESIGN.md` is explicit. AZRA **already has** graph→WGSL
codegen (`NodeGraph::generate_wgsl`, `build_uber_material_fn`) that injects an uber-material into
deferred. It went unused because of a *visibility* change, not a performance opinion:

- Analytical SDF march display was **deleted** (2026-07-14); every geometry pixel is now a raster
  triangle from the baked DC mesh.
- `deferred_entry.wgsl` **skips** material re-eval when `is_raster_pixel`.
- Raster writes no usable `gbuf_surface_id`, so deferred cannot resolve which prim a pixel belongs
  to.

> *"shipping catalog + persistence + successful Naga compile **without a visibility path** produces
> **zero owner-visible shading** on SDF bodies."*

So the CPU bake was the only way to make the node graph visible at all. It was a reasonable
response to a real constraint. What is *not* reasonable is that it stayed single-threaded with a
per-pixel HashMap once the cost became this visible.

**Constraint this imposes on the plan:** the analytical path is permanently dead by owner decision.
A GPU bake must **write map textures consumed by the raster path**. It must not attempt to revive
analytical shading or live per-pixel deferred eval.

---

## 3. What already exists (the reason this is tractable)

| Piece | Where | State |
|---|---|---|
| Graph → WGSL codegen | `shader_nodes/graph.rs:561` `generate_wgsl`, `:1730` `build_uber_material_fn` | **exists, cached** |
| Entry signature | `eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult` | **matches the CPU sampler 1:1** |
| Generated-code deps | zero references to `tex_atlas` / `eval_scene_at` / `prim_*` bindings (verified by grep) | **self-contained** |

### ⚠️ CORRECTION (2026-07-28) — "node parity" was WRONG

An earlier revision of this plan claimed *"82 `NodeType::` refs in codegen vs 79 in the CPU bake →
parity, not a rewrite."* **That counted references, not fidelity.** Reading the emitted code shows
two nodes are deliberate PLACEHOLDERS in WGSL:

| Node | WGSL emits | Consequence |
|---|---|---|
| `ImageTexture` (`:1343`) | `vec3(1.0, 1.0, 1.0)` — *"WGSL samples white placeholder"* | any photo/texture layer bakes **white** |
| `SdfCurvature` (`:1455`) | Laplacian of a synthetic analytic height field `sn_field_h`, *"preview only"* | curvature is **not** the real surface curvature the CPU computes |

`Cavity` and `PrimSdfMask` sit on the same synthetic field. So the GPU path today is faithful for
**procedural** nodes (noise, constants, math, mix, invert, colour-correct) and **wrong** for image
and SDF-surface nodes — which is exactly why the CPU bake exists and why the codegen was only ever
used for a live *preview*, never for the authored map.

**This does not sink P1, but it re-scopes it honestly:**
- P1 (procedural-only) is unaffected — those nodes ARE faithful, which is why P1 was scoped that way.
- **P2 is no longer "bind the texture array."** It must make `ImageTexture` actually sample it —
  new bindings *and* new codegen.
- **P3 is no longer "bind the prim stream."** It must replace the synthetic height field with real
  `eval_scene_at` curvature. The `curvature_eval` kernel already proves that math on GPU, so the
  work is routing it into the generated function, not inventing it.
- The **per-graph fallback in §5 is now load-bearing, not a nicety**: until P2/P3 land, any graph
  containing `ImageTexture`/`SdfCurvature`/`Cavity`/`PrimSdfMask` MUST take the CPU path, or it
  silently bakes white/wrong. Gate on node type, fail-soft to CPU.
| GPU curvature kernel | `volume_bake.wgsl::curvature_eval` + `gpu_curvature.rs` | **shipped, proven** |
| Prim stream serializer | `serialize_body_for_bake_hot` | **shipped** (already feeds 3 GPU consumers) |
| Compute-bake precedent | `volume_bake.wgsl`, `env_prefilter.wgsl`, `ssao.wgsl` | **pattern to copy** |
| Async readback fence | `PendingCurvReadback` peek/take (`gpu_curvature.rs`) | **shipped** |
| Downrez + upsample | `upsample_maps_selective` (`node_graph_bake.rs:933`) | **exists, currently bypassed** |

**This is a wiring job, not a from-scratch port.** The novel piece is a compute entry point that
evaluates the generated material fn over a `res²` grid and writes texture slots.

---

## 4. Phases

Each phase is independently shippable and independently revertable.

### P0 — Make the CPU bake honest (do this first, regardless of GPU)
Not a detour: it is the **oracle** the GPU port is validated against, and this codebase has a
documented history of GPU/CPU parity bugs.

- **P0a — Rayon over pixel rows.** `rayon` is already a dependency. The bake runs off the frame
  thread, so parallelizing inside it cannot affect UI responsiveness. Expect ~8–16×.
  *Risk:* the per-pixel closure must be `Send`/`Sync`. `SdfFieldSources`/`ImageMapSources` are
  passed by shared reference, which is promising — **verify, do not assume**.
- **P0b — Hoist the per-pixel `HashMap`.** ❌ **ATTEMPTED 2026-07-28, REVERTED. Do not redo blindly.**
  Built exactly as specified: flat `Vec<Option<Val>>` indexed `id * stride + out_i`, allocated once
  per row, `clear()` between pixels, both access sites converted. It **compiled, passed all 19 bake
  tests including determinism — and was SLOWER**:

  | build | P0a only | P0a + P0b |
  |---|---|---|
  | dev (`opt-level 0`) | ~5712 ms | **~6900 ms** (~20 % worse, 3 runs) |
  | `fast-release` | 1248 ms | 1298 ms (noise) |

  **Why the estimate was wrong:** the ~5–20× assumed a hash cost proportional to graph size. The
  measured graph has **4 nodes / 16 scratch slots**. A 4-entry `HashMap` barely hashes, so there was
  no cost to remove — only overhead to add. Instrumented to be sure rather than guessing:
  `NodeOutScratch::for_graph` × 1024 = **0.5 ms total**, so construction was not the regression
  either; the flat-array bookkeeping simply lost to a tiny map.

  **Before retrying, MEASURE THE NODE COUNT OF A REAL GRAPH FIRST.** Panel lowering emits up to ~35
  `add_node` calls per channel (`material_panel/lower.rs`), so a genuine multi-layer material may be
  ~10× the benchmark and could flip the result — but that must be *demonstrated on a representative
  graph*, not assumed. The benchmark used here (`build_owner_repro_graph`) is NOT representative of
  graph size.

  **Real remaining CPU costs, unmeasured** (better P0b candidates): `input_val` linearly scans
  `graph.connections` for every input of every node of every pixel; `Node::output_defs` returns a
  freshly heap-allocated `Vec<Socket>` on each call.
- **P0c — Live preview tier.** Bake at 256² while editing, upsample via the existing
  `upsample_maps_selective`, auto-upgrade to full res when editing settles (the auto-upgrade hook
  already exists at `node_graph_bake.rs:4017`). Expect ~16× fewer pixels while scrubbing.
  *Must keep:* the reason the tier was removed — Mult-over-photo stacks lose sharpness through
  256→1024 upsample. Preview cheaply only when the graph has **no** ImageTexture, or preview
  procedural layers while holding image layers at full res.

**Gate:** golden-image comparison before/after each sub-phase. Identical output, lower wall time.

### P1 — GPU bake, single channel, no readback
Prove the pipeline end to end on the narrowest useful slice.

- New `graphics/src/material_bake.wgsl`: `@compute @workgroup_size(8,8)` entry that computes
  `uv → p`, calls the generated material fn, writes one `rgba8unorm` storage texture.
- Assemble via the existing shader-build pattern: header + generated fn + entry, exactly as
  `build_rt_shadow_wgsl` composes header + `PT_TRAVERSAL` + `LIGHTING` + entry.
- Start with **procedural-only graphs** (noise/constant/math/colour) — no ImageTexture, no SDF
  nodes. Those need extra bindings and come in P2/P3.
- **Frame Ordering Contract:** resource creation and dispatch in `poll_background_tasks` only.
- **Gate:** GPU output matches the CPU oracle within tolerance on a fixed set of graphs.
  *This must be a semantic assertion on known values, not just "GPU == CPU"* — a shared-convention
  bug passes a bare parity test (this codebase has shipped exactly that failure before).

### P1c — Integration seam (how a GPU-baked map becomes visible)

Verified 2026-07-28, and it is the reason P1 does not need any new install machinery:

- `PendingMapLoad` (`material_textures.rs:53`) carries **plain `Vec<u8>` RGBA8 pixels** — it does
  not care who produced them.
- `pending_loads_for_material_selective(mid, maps: BakedNodeMaps, ch)` (`node_graph_bake.rs:1397`)
  turns a `BakedNodeMaps` into those loads, and requires each non-empty channel to be exactly
  `MAP_RES * MAP_RES * 4` bytes (1024²).

So the GPU path integrates by **producing a `BakedNodeMaps` and handing it to the existing
function** — upload, slot assignment, strength and naming are all unchanged. Two consequences:

1. The GPU bake must emit at `MAP_RES` (or upsample to it via the existing
   `upsample_maps_selective`) before install, since the length check is exact.
2. Channels the GPU cannot produce faithfully (height / AO / displacement / normal — absent from
   the 5-field `MatResult`) must be left **empty**, not zero-filled: `push` skips a channel whose
   `pixels.len()` does not match, so an empty Vec correctly means "this path did not author it"
   and preserves whatever the CPU path installed. Zero-filling would silently wipe a real map.

### P2 — ImageTexture support
Bind the material texture array; `ImageTexture` nodes sample it in the compute pass.
Removes the `bake_work_resolution` full-res forcing rationale, since GPU cost at 1024² is trivial.

### P3 — SDF nodes (SdfCurvature / Cavity / PrimSdfMask)
Bind the prim stream via `serialize_body_for_bake_hot` — the same serializer the curvature kernel,
`bake_volume`, and `classify_bricks` already share, so the body-local frame contract is inherited
rather than re-derived. `curvature_eval` already proves this exact wiring works.

### P4 — Retire the CPU path to oracle-only
Keep it compiled and test-only as the parity oracle. Do **not** delete it: it is the reference that
makes P1's gate meaningful, and the fallback when a graph hits an unsupported node.

---

## 5. Risks

| Risk | Mitigation |
|---|---|
| GPU/CPU divergence ships silently | Semantic assertions on known geometry, not bare parity. Keep the CPU oracle. |
| Shader compile cost per graph edit | Cache by graph hash — `generate_wgsl` already has `wgsl_cache`. Compile on the background thread (CLAUDE.md: `ensure_*` are fallbacks only). |
| Readback stalls the frame thread | Reuse the `PendingCurvReadback` cross-frame `MapState` fence. **Never** `Maintain::Wait` on the frame thread — that is the documented cause of an earlier freeze. |
| Node coverage gap | Codegen is at parity today (82 vs 79). Any uncovered node falls back to the CPU path per-graph, fail-soft. |
| Scope creep into live deferred eval | Out of scope. This writes **map textures for the raster path**. Reviving analytical/live-gbuffer eval is a separate decision (`PROCEDURAL_TEXTURING_DESIGN` PR8). |

---

## 6. Sequencing recommendation

**P0 first.** It is roughly an afternoon, needs no new GPU infrastructure, and should make editing
feel live *today* — while producing the oracle P1 is graded against. Then P1–P3 as a proper project
rather than a rush.

The expected end state: a 1024² bake goes from **~12.7 s** to **milliseconds**, and the
Live/Final distinction stops mattering because everything is live.
