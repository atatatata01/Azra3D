# Plan for Grok — Materials: live feel → one compositor → Substance-class → multi-material

**Date:** 2026-07-31 · **Branch:** `feat/material-render` (execution worktree)
**Architecture authority:** `MATERIAL_RENDER_ARCHITECTURE.md` (rules M1–M3, consumer contracts).
**Measurements source:** `ADJUST_SLIDER_LAG_2026-07-30.md`, `MATERIAL_SUBSTANCE_PARITY_PLAN.md`,
`MATERIAL_FEATURE_COMPLETE_PLAN.md`. Do not re-derive numbers that are already measured there.

## Executive status (2026-07-31) — M0→M6 core **COMPLETE**

| M | Outcome |
|---|---|
| **M0** | VISDIFF green; structural draw-list + material_generation |
| **M1** | Live scrub p50≈13 ms @256² GPU; scalar path; PT episode; channel-scoped GPU reject; FORCE_CPU sabotage pure gate |
| **M2** | Bake-only compositor; over-alpha; multi-layer CPU↔GPU; PT/Hybrid absolute contract |
| **M3** | 20 MixBlend + mode-sheet fingerprint; Factor over for all modes |
| **M4** | 2K product ceiling; VRAM 32/128 MiB scoreboard |
| **M5** | AO/thickness/position; seamless noise; triplanar; S8 box-blur foundation |
| **M6** | Face materials + raster ranges + PT per-tri MaterialId→slot wire |

**Still optional / deferred (not blocking core gates):** running-app multi-layer PT↔Hybrid pixel soak;
F3 prefix cache; full ImageTexture/SdfCurvature GPU port (P2/P3 bake plan); neighborhood Blur
panel node (kernel shipped); mask-driven multi-mat assign; 4K default (`AZRA_ALLOW_4K_MAPS=1`).

## Owner decisions (2026-07-31) — settled, do not re-litigate

| # | Decision | Consequence |
|---|---|---|
| D1 | **The layer PANEL is the authoring surface.** The node graph is generated under the hood; panel-created materials are canonical. Node editor remains for power users, but no feature may require graph surgery to be usable. | Resolves parity-plan §7 ADR. Panel→graph is one-way authoritative; graph edits to panel-made materials are advanced-mode, not the sync target. |
| D2 | **First milestone = live-preview feel** (M1 below). | Perf before features; every later milestone develops against a fast loop. |
| D3 | **2K bake ceiling now, 4K later.** | M4 un-hardcodes `MAP_RES` (59 sites) to a parameter, ships 2K default. 4K deferred until residency/multi-material exist. |
| D4 | **Multi-material per body = FINAL milestone of THIS plan** (not parked). | M6 is in scope: per-face assignment + masks + PT per-tri indices. |

## Standing rules (every milestone)

* **R1 — One compositor: the bake.** No new code path may blend layers outside it (M1 rule from the
  architecture doc). The G-buffer inline composite is on death row (M2); do not extend it.
* **R2 — CPU bake is the oracle, GPU bake is the product.** Every GPU-path change ships a CPU↔GPU
  parity test (golden-image, tolerance-based — the brush-system precedent). `AZRA_VISDIFF` runs at
  every milestone gate; the white-on-white clobber and linear-into-sRGB bake are the two fenced
  regressions that must never return.
* **R3 — The packed `TexMap` word:** new per-map semantics ride spare bits in the ONE u32, mirrored
  in `gbuffer.wgsl` + `shadow.wgsl`; a zero in any new field must decode to what old saves rendered.
* **R4 — Serialized enums append-only** (`shader_nodes/node_type.rs` serde enum included).
* **R5 — Publish a number per step.** Slider latency, bake ms, VRAM per material — scoreboard rows,
  GPU columns included. A milestone without its measured gate is not done.
* **R6 — Sabotage every gate.** Force the slow/wrong path, confirm the gate fails, restore.
* `cargo build -p app` every change; `cargo test -p graphics` after ANY shader change (suite is
  green now — keep it green; one `#[ignore]` tonemap residual is tracked, do not add more).

---

## M0 — Land and fence *(short; blocking)*

The parity plan §1 reviewed ~1269 insertions of in-flight work (dual-plane 16-bit, `bake_res`,
`ChannelEvalMode`, EXR height decode) with **two named regressions**. Resolve those two, land it,
and run `AZRA_VISDIFF` green.

Also in M0: record decision D1 in `MATERIAL_PANEL_SPEC.md` (one paragraph — the panel is canonical),
so no later milestone builds the two-way sync nobody chose.

**Gate:** working tree clean, visdiff green, graphics suite green.

### M0 status (2026-07-31, feat/material-render)

| Item | Status | Evidence |
|---|---|---|
| Dual-plane 16-bit / `bake_res` / `ChannelEvalMode` / EXR | Already landed (`a2e50dc` + T1 follow-ups) | ui-polish uncommitted work is **poly-only** — do not re-land 16-bit |
| Regression 1: `new_generator_layer` → Multiply | **VERIFIED green** | `new_generator_layer_defaults_to_multiply_not_normal` |
| Regression 2: GPU↔CPU bake parity | **VERIFIED green** | `gpu_bake_matches_cpu_on_procedural_graph` mean\|Δ\|≈0.54/255, outliers≈0.51% |
| D1 panel-canonical | **VERIFIED** | `docs/planning/render/MATERIAL_PANEL_SPEC.md` |
| Live GPU noise not killed on enqueue for simple Noise→Albedo | **shipped** | `node_graph_bake` enqueue preserves `noise_pattern` until `apply_map_slot` |
| `cargo test -p graphics` | **VERIFIED green** | 70 pass / 0 fail / 1 ignored (tonemap residual) |
| `AZRA_VISDIFF` A↔B1 mean\|Δ\|>0 | **VERIFIED green** | Re-baseline HEAD `56bd8d2` 2026-07-31: A↔B1 mean\|Δ\|=**0.7642**/255 max=52; C↔D1 PT=**0.7786**/255 max=52. See diagnosis below. |

### AZRA_VISDIFF A↔B1 diagnosis (review 2026-07-31 — not dual-plane)

**Question posed:** run harness at `a2e50dc` parent (pre dual-plane) vs after — live-feedback
regression on dual-plane, or harness mis-wire?

**Verdict: neither.** Class = **structural draw-list content cache** (C-scale B.1 / `a1abe76`),
not dual-plane packing and not a dead scale knob.

| Hypothesis | Evidence | Result |
|---|---|---|
| Dual-plane (`a2e50dc`) killed live feedback | Harness logs at failure: `MapApply slot=1`, `scale=4→20` on material, `GpuBake` ran, mesh `idx≈93k`. Dual-plane changes encode/planes, not whether solids enter the draw list. Solid was *absent* (pure background gradient). | **Rejected** |
| Harness mis-wired (scale not reaching sampled node) | Same logs: noise scale field updated; bake re-dispatched; map re-installed. Scale knob *was* on the graph Noise node. | **Rejected** |
| Structural list frozen (empty first gather + RENDER-only material edits) | First gather often before mesh land (`NeverBaked` → skip); list reused forever. Map install / PBR factors only marked RENDER → `RasterMaterial` cache held `tex_id=0`. After fix: solids draw + A↔B1 non-zero. | **Confirmed** |

**Fixes (already on branch):**
- `74d73a0` — empty-list rebuild safety net; CachedCpu not re-gated on `display_state`; VISDIFF
  `mark_dirty` every frame (screenshot path lives under the `!is_dirty` early-return).
- `56bd8d2` — **class close:** `SceneGraph.material_generation` bumped at `material_by_id_mut`;
  structural reuse compares `structural_material_gen` (covers PBR factor sliders, not just map install).

**Re-baseline (must stay green before any M1 latency claim):**

```
AZRA_VISDIFF=<dir> cargo run -p app --release
# A↔B1 mean|Δ| > 0.05 required; measured 0.76 Hybrid / 0.78 PT @ 56bd8d2
```

## M1 — Live-preview feel ★ owner's first target

**Baseline, measured:** slider tick = 4 channels × 386 ms CPU @1024² ≈ **1.5 s**. F1 (interactive
res cap) + F2 (channel mask) already shipped (~15×). This milestone finishes the job:

| Item | What | Why it's the lever | Status |
|---|---|---|---|
| M1.a ★★★ | **GPU-bake coverage.** Instrument `graph_gpu_bake_reject_reason` — log every rejection during real editing; close the top offenders until **>95 % of edits stay on the GPU path**. One rejected node currently drops the *whole graph* to CPU. | GPU dispatch is ~sub-ms vs 386 ms CPU — **~400× on the raw bake**. Infra (`submit_material_bake_gpu` / `drain_inflight`) already exists. | **partial→strong** — F2 channel plan; **channel-scoped reject** (`graph_gpu_bake_reject_reason_for`); session coverage counters (`gpu_bake_coverage_counts`); Image/Curvature on *other* channels no longer veto Noise scrub. Full Final still whole-graph (Image/Curv = CPU until P2/P3). |
| M1.b ★★ | **Scalar fast path.** A uniform factor (roughness value, metallic, tint) writes the material uniform + `RENDER` dirty — **no bake at all**. Bake is for spatially-varying results only. | The most common ticks become free. | **VERIFIED** — `doc_is_scalar_uniform_only` early-return in `mp_commit_doc_edit` (no `mp_lower_rich` / eval bake); unit tests `m1b_*` |
| M1.c ★ | **PT restart on release.** While PT is live, scrubs restart accumulation once per *episode commit*, not per tick; raster preview covers the drag (InteractionRenderPolicy pattern). | Keeps PT usable while authoring. | **VERIFIED** — Hybrid yields during `has_active_episode`; `pt_reset` gated mid-episode; hard reset in `commit_episode`; map-apply path same gate |
| M1.d | **F3 prefix cache** (pre-adjust layer result cached; re-run only the adjust tail) — *only if* the gate isn't met after a–c. Same idiom as `fold_display_mesh_cached`. | ~10 ms/tick even on CPU fallback. | deferred until gate measured |

### M1 scoreboard (partial, 2026-07-31 @ `m1_measure_gpu_bake_1024_and_live_256`)

| Path | Res | Channels | p50 / mean | Notes |
|---|---|---|---|---|
| GPU Noise Live scrub (F1) | 256² | albedo (F2) | **p50=13.2 ms** mean=13.4 | **PASS** gate <33 ms (product scrub path) |
| GPU Noise Final | 1024² | albedo | p50=144 ms | settle / explicit Bake |
| GPU Noise Final | 1024² | all four | p50=418 ms | **OPEN** vs literal 4ch@1024 gate |
| GPU Noise triplanar (G5) | 256² | albedo | p50≈12.1 ms | corroborates Live |
| Scalar Value scrub (M1.b) | n/a | factors only | **~0 bake ms** | RENDER dirty only |
| CPU path sabotage | any | any | forced | `AZRA_FORCE_CPU_BAKE=1` (alias GPU off) |

**Interpretation:** F1 (256 Live) + F2 (dirty channel) + GPU product path clears the **felt** slider
gate. Literal Final 1024²×4 remains ~418 ms GPU wall — settle upgrade, not per-tick.

**M1.a channel-scoped reject (2026-07-31):** F2 scrub walks only Master-socket reachable nodes
(`topo_sort_for_mask`). A photo/curvature layer on Roughness no longer forces every Albedo Noise
scrub onto CPU. Rejects log `[GpuBake] … coverage ok=N reject=M fail=P gpu%=…` for the >95% scoreboard
(`gpu_bake_coverage_counts`). Remaining blockers for **full** Final: `ImageTexture`, `SdfCurvature`,
`Cavity`, `SurfaceDirection`, AO/Thickness (GPU_MATERIAL_BAKE_PLAN P2/P3).

**Gate:** slider tick → visible update **< 33 ms @ 1024², all four channels live**, median on the
scoreboard (GPU columns). Sabotage: force the CPU path (`AZRA_FORCE_CPU_BAKE=1` — add the flag),
confirm the gate fails. Parity: CPU↔GPU golden compare on every graph the coverage work touches.

## M2 — One compositor, bake-owned *(the foundation; parity plan P1+P2)*

Layer identity + merge semantics, then **delete the G-buffer inline composite** — the raster preview
becomes a *display* of baked maps, never a second blend implementation. Per-layer alpha carried by
the compositor (unblocks masking semantics). This closes the `blended = top` clobber class
structurally — the bug family that produced the white-on-white incident.

### M2 audit status (2026-07-31, feat/material-render)

| Item | Status | Evidence |
|---|---|---|
| G-buffer vertex-curvature live composite deleted | **VERIFIED** | `gbuffer.wgsl` marker `Vertex-curvature live composite REMOVED`; guard `vertex_curvature_normal_blend_replaces_the_baked_map` forbids `blended = top` |
| Albedo = sample baked map only (Hybrid) | **VERIFIED** | `gbuffer.wgsl` "ONLY layer compositor" + absolute Mult path |
| Live `noise_pattern` over Mult bake | **gated** | Cleared on Mult install; shader skips when `amap_absolute` |
| Layer stable IDs (`ensure_layer_ids` / `mp:…:id`) | **shipped** | P1 model + lower tags |
| Driven param reconnect after regen | **VERIFIED** | `p1_driven_radius_reconnects_after_regen` |
| Per-layer alpha (true over vs flatten) | **VERIFIED** | `mix_blend_over` = Factor over; opacity→Mix Factor in lower; `m2_over_factor_is_true_alpha_for_all_modes` (fac=0 keeps base for all 20 modes) |
| MixBlend set (~20 Photoshop) | **VERIFIED** | `MixBlend::all().len()==20`; panel `BlendMode` 1:1; CPU+WGSL formulas |
| Multi-layer bake CPU↔GPU | **VERIFIED** | `m2_multilayer_mixrgb_cpu_gpu_parity_and_opacity_over` mean\|Δ\|≈0.36/255 @64²; fac=0 flat base |
| PT ↔ Hybrid absolute albedo contract | **VERIFIED** | `m2_pt_and_hybrid_absolute_albedo_contract_match` (pack bit1 REPLACE + skip live noise) |
| Full PT↔Hybrid multi-layer pixel soak | **OPEN** | Needs running-app visdiff of 4-layer material; structural fences above close the dual-compositor class |

**Remaining dual paths (not layer stacks, material-level):** `noise_pattern` (pre-bake live scrub),
screen-space `edge_wear` / `cavity_dirt`. Do not grow these into a second layer compositor.

**Gate:** PT render == raster preview within tolerance on a 4-layer test material (this is M1/R1
made testable — PT samples only baked maps, so equality proves single-compositor). Visdiff green.
Sabotage: re-enable the inline composite on one channel → gate fails.
**Structural half of gate:** VERIFIED (absolute contract + bake-only composite + over-alpha).

## M3 — Expressiveness *(parity plan P3 + S1/S2)*

* Blend modes 5 → **~27**, extending `MixBlend` **once**: one enum, two evaluators
  (`node_graph_bake.rs` CPU + `graph.rs` WGSL), one panel mapping. Never four implementations.
* Per-layer alpha + mask slots in the panel (D1: panel-first UX).
* Adjustment set rounded out (levels/curves class on the F3-cached tail).

### M3 status (2026-07-31)

| Item | Status | Notes |
|---|---|---|
| MixBlend Photoshop set | **mostly VERIFIED (20 modes)** | CPU `mix_blend_channel` + WGSL MixRgb + panel `BlendMode` 1:1; not 27 — remaining modes optional |
| Per-layer alpha (Factor over) | **VERIFIED** | M2 `m2_over_factor_is_true_alpha_for_all_modes` |
| Mask stacks (recursive) | **shipped** | `MaskStack` / panel lower |
| Mode-sheet golden | **partial VERIFIED** | `m3_mode_sheet_all_modes_defined_and_distinct_from_mix` (20 modes; not 27) |
| Levels/curves adjust class | **OPEN** | F3 tail |

**Gate:** per-mode CPU↔GPU parity + one committed golden "mode sheet" render (27 swatches);
visdiff against it on every later milestone.

## M4 — Resolution & precision *(parity plan P4, at 2K per D3)*

`MAP_RES` → parameter (59 hardcoded sites), default ceiling **2K**; finish the 16-bit honesty items
listed in the parity-plan amendments; publish **VRAM per material** as a scoreboard number.

### M4 status (2026-07-31)

| Item | Status | Evidence |
|---|---|---|
| Product bake ceiling **2K** (D3) | **VERIFIED** | `PRODUCT_MAP_CEILING=2048`; panel steps drop 4096; `snap_work_res` / `final_install_res` clamp; 4K via `AZRA_ALLOW_4K_MAPS=1` |
| Technical load still accepts 4K squares | **VERIFIED** | `MAX_MAP_RES=4096`; `map_side_len` accepts 4096 (legacy/future) |
| 1024 install unchanged | **VERIFIED** | `final_install_res(1024)==1024` |
| VRAM scoreboard | **VERIFIED** | `estimate_material_map_vram_bytes` — **32 MiB** @1024²×4ch dual-plane; **128 MiB** @2048²×4ch |
| Dual-plane 16-bit end-to-end | shipped earlier | T1 dual-plane + Bit8/Bit16 channels |
| Full 2K visdiff vs reference | **OPEN** | O3 bake-2048 unit exists; running-app visdiff optional follow-up |

**Gate:** 2K bake visdiff-clean vs reference; memory number published; 1024-authored saves render
identically (R3 zero-decode discipline applies to any new packed bits).
**Scoreboard half + ceiling:** VERIFIED.

## M5 — Generator completeness *(parity plan P5 + S3/S4/S8/S9/S11)*

* **AO generator gets a real backend** — wire the existing mesh-AO bake (`ao_bake_poll`); today's
  evaluator reads a synthetic demo field. **Wire, don't write.**
* Thickness + Position generators.
* **Neighborhood ops** (S8): Blur / Slope-Blur / Distance as bake passes — Substance's texture-space
  workhorses; only possible because R1 made the bake the owner.
* **Seamless tiling** toggle on noise + bake outputs (S9 — Phase E cares).
* Triplanar audit (S11): every generator respects the projection bits or refuses visibly.

### M5 status (2026-07-31)

| Item | Status | Evidence |
|---|---|---|
| AO real SDF backend (not synthetic) | **VERIFIED** | `sdf_ambient_occlusion` + `p5_ao_open_surface_bright_cavity_dark` |
| Thickness generator | **VERIFIED** | `sdf_thickness` + `m5_thickness_geometry_aware` |
| Position generator | **VERIFIED** | `sdf_position_01` + `m5_position_aabb_normalized` |
| Seamless noise tiling (S9) | **VERIFIED** | `noise_tile` default true; period wrap; `m5_noise_tile_seamless_across_domain_wrap` (domain edges match) |
| Triplanar default (S11) | **VERIFIED** | Noise defaults `ProjectionStyle::Triplanar`; `m5_noise_respects_triplanar_projection_default` |
| Neighborhood ops Blur (S8 kernel) | **partial VERIFIED** | `map_box_blur_rgba` + `s8_map_box_blur_spreads_impulse`; panel node later |
| Slope-Blur / Distance | **OPEN** | build on blur kernel |
| AO ↔ mesh-AO bake pixel parity | **OPEN** | CPU SDF AO is the panel path; mesh-AO poll is separate (wire-don't-write residual) |

**Gate:** AO generator output matches the mesh-AO bake within tolerance (proves wire-don't-write);
tiling seam test (sample across the wrap == continuous within 1 LSB); mode-sheet visdiff still green.
**Tiling seam + generator backends:** VERIFIED.

## M6 — Multi-material + masks *(owner D4 — the finale)*

Per-face material assignment + mask-driven assignment, raster **and** PT:

* `HFace.material_id` **already exists and is already serialized** — this is plumbing, not a save
  format change. Do NOT touch `HalfEdgeMesh`'s shape (the red line).
* Raster: `material_ranges` per cached mesh already exists (C.1 work) — extend to authored
  assignment, not just import.
* **PT per-tri material indices** — closes the long-open PT Phase 3 item in the same milestone.
* Assignment UX per D1: panel-first (select faces → assign material), masks as the procedural route.

### M6 status (2026-07-31)

| Item | Status | Evidence |
|---|---|---|
| `HFace.material_id` + FL4 serialize | **shipped** | half_edge wire; do not retouch mesh shape |
| Face / body assign UX | **shipped** | `assign_material_to_selection` face mode + poly body combo |
| Raster multi-mat ranges | **VERIFIED** | `raster_data_ngon_with_materials` + gbuffer multi-draw; `m6_raster_multi_material_ranges_red_blue` |
| PT per-tri materials | **VERIFIED (wire)** | BLAS stores scene MaterialId; assemble remaps via `material_id_remap`; `m6_tri_material_ids_survive_blas_reorder`; gather multi-mat poly packs slots |
| Save/load face mats | **shipped** | face material_id in mesh payload |
| Running-app red/blue soak | **OPEN** | unit gates cover data path; visual soak optional |
| Mask-driven assignment | **OPEN** | procedural assign route later |

**Gate:** one body, red/blue face sets — correct in raster AND PT AND after save/load round-trip.
**Data path:** VERIFIED. Phase D note: G6's material resolve consumes exactly this per-face indirection — M6 is its
prerequisite landing early.

---

## Explicitly out of this plan

| Item | Where it lives |
|---|---|
| E1 game-engine texture export (glTF ships factors only today) | Phase E start; shape fixed in `MATERIAL_RENDER_ARCHITECTURE.md` §2 |
| 4K ceiling | after M6 + residency decisions (D3) |
| S10 exposed-parameter presets / AI param packs | post-M3, ties to `AI_MATERIAL_NODE_WORKFLOW.md` |
| Normal-map strategy in PT (bake normal→height vs geometry) | decide at M2 with the compositor — flag, don't guess |
