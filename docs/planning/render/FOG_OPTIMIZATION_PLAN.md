# Fog Pass Optimization Plan — AZRA

**Status:** P0 + P1 + P2 + P3 **SHIPPED** 2026-07-19  
**Date:** 2026-07-19  
**Owner:** Dim  
**Context:** Fog feels **super slow** even on a **single cube, no textures**.  
**Related:** `graphics/src/fog_entry.wgsl`, `app/src/viewport_render.rs` (fog dispatch + uniforms), `app_types` fog settings, tonemap composite `post_flags & 8`.

### Shipped (P0+P1+P2+P3)
- Defaults: `fog_noise_strength=0`, `fog_shadow_steps=0`
- Steps: Draft 8 / Low 12 / Med **16** / High 32 / Ultra 48 (was 12–96)
- Half-res: quality &lt; 3 → each thread writes 2×2 full-res texels; dispatch at half grid
- Skip `vol_light_inscatter` when no VOL light enabled
- Optical-depth early-out when density×range negligible
- UI hover cost hints on Noise / Shadow Steps
- **P2 Analytic Lite** (`pass_fog_lite`): closed-form height fog, half-res 2×2
  - Used while **orbiting** (no fog pop) and **Draft** quality without volumetric lights
  - No SDF march / noise / god-rays; cost ≈ G-buffer load + math
- **P3 Pay-for-features**
  - Full sun shafts only when God Rays **and** Shadow Steps &gt; 0
  - Draft–Med: depth-buffer probes (≤2); High/Ultra: SDF `fog_shadow`
  - `fog_has_vol_lights` CPU uniform (was `_pad_env1`) — no per-pixel light scan

---

## 0. Goal

Make atmospheric fog **interactive** on simple scenes (one body, few lights) while keeping a **High** quality mode for hero shots / stills.

**Success metrics (1080p, one cube, 1 sun, fog ON, Medium quality):**

| Metric | Today (est.) | Target |
|--------|----------------|--------|
| Fog GPU time (prof_pass_times[3]) | often multi-ms → tens of ms | **&lt; 1.0 ms** default path |
| Frame hit while orbiting (fog ON) | heavy hitch | **no worse than fog OFF + 0.5 ms** |
| Look (default) | soft height fog + god-ray tint | **same class of look**, less noise |

---

## 1. Why it is slow today (cost model)

### 1.1 Full-resolution ray march every pixel

`pass_fog` runs at **full viewport** (`dispatch (w+7)/8 × (h+7)/8`).

At 1920×1080 ≈ **2.07M** threads.

### 1.2 Step count tied to global `render_quality`

From `fog_entry.wgsl`:

| `render_quality` | March steps | FBM octaves |
|------------------|------------|-------------|
| 0 Draft | 12 | 2 |
| 1 Low | 32 | 3 |
| 2 Medium | **64** | **4** |
| 3+ High/Ultra | **96** | **5** |

So Medium alone: **2M × 64 ≈ 132M** samples / frame before shadows.

### 1.3 Per-step work is heavy (even for one cube)

Each step does approximately:

1. Height falloff `exp(...)`  
2. **Optional 3D FBM** — default `fog_noise_strength = 0.4` (**ON**) → multi-octave hash noise  
3. **`fog_shadow`** → loop `fog_shadow_steps` (default **4**) each calling **`scene_sdf`**  
4. **Always** `vol_light_inscatter(...)` → loops **all lights**, and for volumetric ones another `fog_shadow`

Even with **one cube**:

- `scene_sdf` is not free (scope stack / prim eval / or volume sample).  
- Shadow ray × steps × pixels dominates.  
- FBM multiplies cost when noise is on (default).

### 1.4 Worst-case order of magnitude (Medium, 1080p)

Rough:

```
pixels × steps × (1 + shadow_steps × cost_sdf + lights × …)
≈ 2e6 × 64 × (FBM + 4×SDF + light loop)
```

That is **film-grade volume** cost, not “viewport soft fog for a cube”.

### 1.5 Fixed costs that do not help simple scenes

| Feature | Default | Needed for 1 cube? |
|---------|---------|---------------------|
| Full-res march | always | No → half-res OK |
| FBM noise | strength 0.4 | No → off by default |
| SDF fog shadows | 4 steps | Optional for soft depth fog |
| `vol_light_inscatter` every step | always called | No if no volumetric lights |
| Temporal blend | 0.9 | Keep — allows **fewer** spatial steps |

### 1.6 Recent correctness note (do not regress)

2026-07-19: fog uniforms were corrupted by dead VP packing (`fog_density = from_bits(vol_flags)`).  
**Optimization must never restomp fog_* with volume-AO packing.** Fog fields stay authored fog parameters.

---

## 2. Design principles

1. **Simple fog is free-ish** — height + Beer's law can be closed-form / few samples.  
2. **Pay for god-rays only when requested** — scatter/shadows/volumetric lights are premium.  
3. **Half-res + temporal** for interactive; full-res only on High or when idle.  
4. **Shader variants / branches** so empty volumetric path compiles away cost.  
5. **Quality ladder independent of global render_quality** when possible (user fog quality).  
6. **Profile first** — gate work on `prof_pass_times[3]` / timestamp `PASS_FOG`.

---

## 3. Quality ladder (product)

Expose **Fog quality** (or map from Draft→Ultra with sane caps):

| Fog mode | Res | Steps | Noise | SDF shadow | Vol lights | Target use |
|----------|-----|-------|-------|------------|------------|------------|
| **Off** | — | 0 | — | — | — | default |
| **Lite** | ½ | 0–1 analytic | off | off | off | always interactive |
| **Standard** | ½ | 8–16 | off / low | 0–2 | if any VOL on | default when Fog ON |
| **High** | full | 32–48 | optional | 2–4 | full | stills / hero |
| **Ultra** | full | 64–96 | on | 4–8 | full | current Medium/High look |

**Recommendation:** default Fog ON → **Standard (half-res, no noise, no shadow)**.  
Scatter/god-rays slider can auto-bump to High path when `fog_scatter > 0` *and* user wants shafts.

---

## 4. Implementation plan (phased)

### P0 — Cheap wins (1–2 days) — **do first**

**P0.1 Skip empty work**

- If `!fog_enabled && !any_volumetric_light()` → already skipped.  
- If fog ON but **no volumetric lights**: **do not call** `vol_light_inscatter` (branch or `#ifdef` / separate entry).  
- If `fog_shadow_steps == 0` or `num_primitives == 0`: already returns 1.0 — **default shadow steps to 0** for Lite/Standard.

**P0.2 Defaults for simple scenes**

Change defaults in `types_render.rs` / project:

| Setting | Now | Proposed default |
|---------|-----|------------------|
| `fog_noise_strength` | 0.4 | **0.0** |
| `fog_shadow_steps` | 4 | **0** (Lite) / 2 (Standard) |
| `fog_temporal_blend` | 0.9 | keep 0.9 |
| march steps | 32–96 via global quality | **cap 16** for Standard |

**P0.3 Early out by optical depth**

- If `density * max_distance < ε` → write identity `(0,0,0,1)` and return.  
- If surface is very close (`depth < fog_start`) already early-outs — good.

**P0.4 Rebuild fog_bg when missing** (already fixed 2026-07-19) — keep.

**Exit P0:** Fog ON, cube scene, orbit: fog cost **&lt; 2 ms** at 1080p; look is smooth height haze without noise/shafts.

---

### P1 — Half-resolution fog + upscale (2–3 days) — **biggest FPS win**

**P1.1 Allocate half-res fog target**

- `fog_half` (and optional `fog_half_prev`) at `(w+1)/2 × (h+1)/2`.  
- Dispatch fog at half-res.

**P1.2 Upscale into full-res fog texture for tonemap**

Options (pick one):

- **A (simple):** bilinear sample in tonemap from half fog (change binding).  
- **B (better):** 1-pass bilateral / depth-aware upscale using `gbuf_normal.w` so edges stay sharp.

**P1.3 Temporal at half-res**

- Reproject at half; fewer pixels → more budget for steps if needed.

**Exit P1:** ~**4×** fewer primary march threads; same Lite/Standard look at cube scale.

---

### P2 — Analytic / Lite fog path (1–2 days)

**P2.1 Closed-form height fog (no ray march)**

For Lite mode (and interaction):

```
// optical depth from camera to surface along ray with exp height falloff
// Beer: T = exp(-τ); fog_rgb ≈ L_ambient * (1-T) * scatter_tint
// Store (fog_rgb, T) for tonemap: hdr = hdr * T + fog_rgb
```

No `scene_sdf`, no FBM, 1 sample of depth from G-buffer.

**P2.2 Interaction policy**

Today fog is **skipped** while interacting (`!interaction_active`). Prefer:

- Interaction → **Lite analytic** (keep atmosphere while orbiting), not full skip (skip causes pop).

**Exit P2:** Orbit with fog feels continuous; cost ≈ single texture load + math.

---

### P3 — God rays / shadows only when paid for (2 days)

**P3.1 Split features**

| Feature | Gate |
|---------|------|
| Height + density | Fog master |
| Noise FBM | `fog_noise_strength > 0.01` (default 0) |
| Sun shaft scatter | `fog_scatter > ε` **and** shadow_steps &gt; 0 or dedicated “God rays” toggle |
| Per-light volumetric | only if `any_volumetric_light()` |

**P3.2 Cheaper shadow**

- Prefer **depth buffer soft shadow** or **1–2 samples** toward sun using G-buffer depth, not full `scene_sdf` march, for Standard.  
- Keep SDF `fog_shadow` only for High/Ultra or “Accurate shafts”.

**P3.3 Volumetric lights**

- Precompute `any_vol` on CPU; push as uniform `fog_has_vol_lights`.  
- Shader: `if (fog_has_vol_lights == 0) { /* skip entire vol path */ }`.

**Exit P3:** Cube + fog haze free; enabling God Rays clearly costs more (documented in UI hover).

---

### P4 — Shader / pipeline hygiene (1 day)

**P4.1 Fog quality uniform** independent of global Draft/Low/Med/High when user sets Fog panel quality.

**P4.2 Optional dual entry points**

- `pass_fog_lite` (analytic / few steps, no SDF)  
- `pass_fog_full` (current march)

Compile both lazily; select by quality.

**P4.3 Profiling**

- Always record `PASS_FOG` when fog active.  
- Optional HUD line: `Fog: X.X ms | mode=Standard | half | steps=12 | sh=0`.

**P4.4 Tests**

- Naga validate both entries.  
- GPU readback: Lite identity when density=0; Lite non-zero τ when density high + far floor.  
- Regression: fog uniforms not stomped by volume AO packing (unit test on upload path or golden bits).

---

## 5. Suggested execution order

```
P0 (defaults + skip vol + shadow 0 + step cap)
  → measure PASS_FOG on cube
P1 (half-res + upscale)
  → measure again (expect ~3–4×)
P2 (analytic Lite + interaction)
  → orbit feel
P3 (gate god-rays / better cheap shadows)
P4 (quality enum + dual pipeline + tests)
```

Do **not** start with Ultra feature work. Cube case is solved by **P0+P1+P2**.

---

## 6. Concrete file touch list

| File | Changes |
|------|---------|
| `graphics/src/fog_entry.wgsl` | Lite path, step tables, skip vol, cheaper shadow branch |
| `graphics/src/build fog` (if any) | dual entry / defines |
| `app/src/viewport_render.rs` | half-res dispatch, quality uniforms, interaction Lite, **never** stomp fog fields |
| `app/src/gpu_init.rs` | half fog textures, rebuild_fog_bg, optional second pipeline |
| `app_types/src/types_render.rs` | fog quality enum, safer defaults |
| `app/src/properties_render.rs` | Fog quality combo; hover costs; default noise 0 |
| `docs/planning/render/FOG_OPTIMIZATION_PLAN.md` | this doc |

---

## 7. Non-goals (this plan)

- Replacing PT in-scatter fog (separate; already uses density fields).  
- Full volumetric path-tracer media.  
- Matching Houdini pyro volume.  
- Fixing unrelated volume AO features (keep on `scene_aabb_min.w` only).

---

## 8. Risk / mitigation

| Risk | Mitigation |
|------|------------|
| Half-res soft edges on cube silhouette | depth-aware upscale (P1.B) |
| Temporal ghosting | confidence reject already exists; lower blend on camera cut |
| Users miss noisy “cinematic” fog | High/Ultra + noise slider default off |
| God-rays look weaker | explicit Standard vs High; scatter auto-hint |

---

## 9. One-liner for implementers

> **Default fog must not ray-march 64 full-res steps with FBM + 4× SDF shadows per step.**  
> Use **half-res + few steps + no noise + no SDF shadow** for the cube case; reserve the current march for High/Ultra and optional god-rays.

---

## 10. Immediate manual test checklist (after each phase)

1. One cube, PBR, Fog ON, density 0.05, height falloff 0.4 — haze visible, orbit smooth.  
2. Fog OFF — frame time drops by roughly PASS_FOG.  
3. PathTrace with fog density &gt; 0 — no full-screen black (composite bit stays off for PT).  
4. Volumetric light ON — shafts only when quality allows; cost increases measurably.  
5. Noise strength 0 vs 0.5 — cost delta visible in profiler.

---

**Next step when you say GO:** implement **P0 + P1** first (defaults, skip vol path, shadow 0, half-res fog). That is the largest win for “one cube, no texture.”
