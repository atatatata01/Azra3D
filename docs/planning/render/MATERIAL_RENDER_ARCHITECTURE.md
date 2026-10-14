# Material & Render Architecture — one bake, three consumers

**Date:** 2026-07-31 · **Branch:** `feat/ui-polish`
**Sits above:** `MATERIAL_FEATURE_COMPLETE_PLAN.md` (build phases P0–P6),
`MATERIAL_SUBSTANCE_PARITY_PLAN.md` (gap measurements), `ADJUST_SLIDER_LAG_2026-07-30.md` (perf
diagnosis), `MATERIAL_PANEL_SPEC.md` (UI), `AZRA_MASTER_ROADMAP.md` + `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md`.
This file does not repeat their content — it fixes the **ownership model**, the **roadmap fit**, the
**live-preview performance ladder**, and the **gap register** in one place.

---

## 1. The architecture in one picture

The geometry side of the roadmap found its hub in `mesh_cache`. The material side has the same
shape, and last week's work already proved it:

```
Node graph (authored, NodePayload::Material)
        │
        ▼  ONE compositor — THE BAKE (CPU oracle + GPU fast path, must stay bit-comparable)
Baked texture array (albedo / rough / metal / normal / height; dual-plane 16-bit)
        │
        ├──▶ G-buffer raster (viewport)     — PREVIEW of the bake, never a second compositor
        ├──▶ Path tracer                     — samples the SAME array (verified: pathtrace_tri_entry.wgsl)
        ├──▶ Phase D vis-buffer resolve (G6) — per-pixel material-ID → SAME array
        └──▶ Phase E game bake / export      — serialize the SAME array (does not exist yet — E1)
```

**The load-bearing decision (already made in `MATERIAL_FEATURE_COMPLETE_PLAN.md`, elevated here to
architecture):** *anything that goes through the bake works in every consumer for free; anything
evaluated live outside it works only in the G-buffer.* PT proved this empirically — it never runs
the G-buffer fragment path, so the vertex-rung curvature carry and the inline layer composite are
invisible to it. Therefore:

> **Rule M1 — the bake is the only compositor.** The G-buffer inline composite is a preview tier
> and is scheduled for deletion (P2). No new feature may add a second place where layers blend.
> **Rule M2 — CPU bake is the oracle, GPU bake is the product.** They must stay pixel-comparable
> (the brush-system golden CPU/GPU parity gate is the precedent); `AZRA_VISDIFF` is the harness.
> **Rule M3 — new map semantics ride the array + the ONE packed word** (`TexMap` bits, mirrored in
> `gbuffer.wgsl` + `shadow.wgsl`; a zero in any new field must decode to what old saves rendered).

This is *more* aligned with the roadmap than Substance's own architecture: Substance bakes to files
and hands off; AZRA's bake is a **live scene resource** that four consumers read in place — which is
exactly what edit-while-virtualized (Phase D) and the runtime split (Phase E) need.

---

## 2. Roadmap fit, consumer by consumer

| Consumer | Contract with the bake | State | Gap |
|---|---|---|---|
| **Viewport PBR** (deferred + IBL/SSAO/RT-shadow chain) | reads array via G-buffer; inline composite still exists | shipping | P2 deletes the inline composite → preview == bake |
| **Path tracer** (offline final, WGSL tri-PT) | samples array directly; never perturbs shading normal | shipping | per-**tri** material indices (PT Phase 3, open); normal-map strategy = bake-side (M1 corollary) |
| **Phase D vis-buffer (G6)** | material resolve pass = per-pixel ID → array sample + BRDF | design only | **no new material work needed if M1 holds** — this is why M1 is an architecture rule, not a preference |
| **Phase E game bake** | export the array: channel-packed ORM + albedo + normal + height, glTF-linked | **does not exist** | **E1 (new):** `gltf_export.rs` writes PBR *factors* only — zero texture/image output today. The export is a serializer over bake products; no new render tech |
| **Terrain (§5 roadmap)** | EXR heightmap → displacement input | decode shipped (EXR/HDR/float-TIFF) | bake-side displacement consumer lands with terrain, not before |

**E1 is the one genuinely missing subsystem** — everything else is a planned refinement of things
that exist. It is also cheap: the maps already exist in GPU/CPU memory; export = readback + PNG/EXR
encode + glTF `images[]`/`textures[]` references. Schedule it in Phase E, but its *shape* is fixed
now so nothing lands that fights it (M1 already guarantees the array is complete).

---

## 3. Live-preview performance — the ladder

**Owner's complaint, measured (`ADJUST_SLIDER_LAG`):** one slider tick = 4 channels × 386 ms CPU
bake @1024² ≈ **1.5 s**. F1 (interactive res cap for image graphs, ~15×) and F2 (re-bake only the
edited channel) **shipped**. The remaining ladder, in leverage order:

| L | Lever | Cost → effect | Status |
|---|---|---|---|
| **L1 ★★★** | **Make the GPU bake the default live path.** `try_gpu_bake_graph` + async `submit_material_bake_gpu`/`drain_inflight` already exist; a 1024² compute dispatch is sub-ms against CPU's 386 ms — **~400× on the raw bake**. The gate is `graph_is_gpu_bakeable`'s reject list: every rejected node type silently drops the whole graph to the CPU path. | **Audit the reject list, close the top offenders** until >95 % of real edits stay GPU. CPU remains the oracle (M2), never the interactive path. | partial — infra shipped, coverage unknown |
| **L2 ★★** | **F3 — cache the pre-adjust prefix.** Contrast/Sat/Gamma/Invert are pure per-pixel functions of the layer's composited output; sample once, re-apply the tail per tick. This is the **same prefix-cache idiom as `fold_display_mesh_cached`** (28–42× there) — one idiom, third application (poly modifiers, NURBS re-solve, now material stacks). | ~10 ms/tick even on CPU | designed (F3), deferred |
| **L3 ★★** | **Uniform-parameter fast path:** a scalar that multiplies a channel (roughness value, metallic factor, tint) must not bake *at all* — write the uniform/factor, `RENDER` dirty only. Bake is for spatially-varying results. | zero-bake for the most common ticks | not built |
| **L4 ★** | **PT accumulation restart policy:** while PT is the active view, a live scrub currently restarts accumulation per tick. Latch: restart on *release* (episode commit), show the raster preview during the drag — same InteractionRenderPolicy pattern the viewport already has. | keeps PT usable while authoring | audit + wire |
| L5 | F4 debounce | last resort only — the standing rule against timing bandaids applies | deferred, likely unnecessary after L1–L3 |

**Gate for the whole ladder:** slider tick → visible update **< 33 ms at 1024², < 100 ms at 4K**,
measured and put on the scoreboard (the perf rows now carry GPU ms — use them). Sabotage: force the
CPU path, confirm the gate fails.

**Regression fences (hard-won, do not re-learn):** the white-on-white enqueue clobber and the
linear-bytes-into-sRGB-array bake are both guarded by `AZRA_VISDIFF` — every L1–L3 change runs it.

---

## 4. Generator quality vs Substance — position and gaps

`MATERIAL_SUBSTANCE_PARITY_PLAN.md` §3 measured the core gaps; they stand. The register below adds
what that plan missed, so this is the complete Substance-class scorecard:

| # | Gap | Where it's covered |
|---|---|---|
| S1 | 5 → ~27 blend modes, declared **once** (extend `MixBlend`: 2 evaluators + 1 mapping, not 4 implementations) | parity plan P-phases — highest leverage, confirmed |
| S2 | Per-layer alpha (compositor carries it; flattened maps cannot) | P1/P2 |
| S3 | **AO generator has no backend** — evaluates a synthetic demo field. The real one exists elsewhere: mesh-AO bake (`ao_bake_poll`). Wire, don't write. | P5, sharpened |
| S4 | Thickness + Position generators | P5 |
| S5 | 2K/4K: `MAP_RES` hardcoded at **59 sites** | P4 (bake_res is working-res; MAP_RES is the ceiling) |
| S6 | 16-bit end-to-end (dual-plane shipped; honesty gaps listed in parity-plan amendments) | P4 |
| S7 | Multi-material per body + masks | P6, own ADR — owner decision |
| S8 | **NEW — neighborhood ops:** Blur / Slope Blur / Distance / Bevel-from-height. Substance's texture-space workhorses; impossible in a per-pixel live eval, **natural in the bake** (M1 makes them free to add as bake passes). | not in any plan — add to P5 |
| S9 | **NEW — seamless/tiling option** on noise + bake outputs (game-engine texturing baseline; PT/viewport don't care but Phase E does) | add to P5 |
| S10 | **NEW — exposed-parameter presets** (a material's tweakable params as a saved pack — the .sbsar idea). Also the natural interface for `AI_MATERIAL_NODE_WORKFLOW.md`: the AI writes a param pack, not raw graph surgery. | post-P3; ties to AI roadmap |
| S11 | **NEW — triplanar projection completeness**: projection bits exist in the packed word (M3); verify all generators respect them or mark unsupported visibly | audit item |

**Honest position vs Substance today:** the *architecture* (graph → one bake → live scene resource,
16-bit, GPU-baked, PT-consistent) is already **better-shaped** than Substance-in-a-DCC integrations.
The *node library and compositor depth* (S1/S2/S8) are where the distance is — bounded, mechanical
work on one enum and two evaluators, not a rewrite. The parity plan's phases remain the build order;
S8–S11 slot into P5+ without reordering anything.

---

## 5. Decisions still owed (unchanged, collected)

1. **Panel ↔ node-graph coupling ADR** (parity plan §7) — one authoring surface or two-way sync.
2. **Multi-material ADR** (P6 / S7).
3. Phase E export format details (E1) — decide *when Phase E starts*, shape is fixed by §2.

Nothing in this document blocks on them; M1–M3 hold under every branch of all three.
