# PBR Light Performance Plan — AZRA

**Branch / worktree:** `perf/pbr-lights` @ `.claude/worktrees/pbr-lights`  
**Base:** `4b33053` (post light UX)  
**Status:** COMPLETE 2026-07-19 (P0–P3.1 A–D shipped on perf/pbr-lights)  
**Scope:** PBR / Lit deferred path — measure first, then cheap gates, then architecture later.

---

## Goals

Make multi-light PBR interactive without tech-demo cost:

1. **Measure** RT / deferred / fog so we know what wins  
2. **P1 gates** — early-out, avoid soft_shadow storms, gate SDF_REFLECT  
3. **P2** (later) — vol light compact list, RT >4 packing, half-res RT  
4. **P3** (later) — clustered light lists  

---

## Shipped this branch

### P0 — Instrumentation
- [x] `PASS_RT_SHADOW` GPU timestamp slot (15)  
- [x] HUD line `RT Shadow: GPU X.XXms` when active  

### P1 — Cheap wins in deferred
- [x] Early-out light loop when `atten < 0.01` or intensity ~0 (skip shadow + BRDF)  
- [x] When global RT on and light index ≥4: **no SDF soft_shadow fallback** (unshadowed lit) — avoids soft_shadow storms  
- [x] SDF_REFLECT: only chrome-class (`metal > 0.5 && rough < 0.25`), quality ≥2, steps 24/48  

### P2 — Medium wins
- [x] `fog_has_vol_lights` = **bitfield** of VOL lights; fog iterates only set bits  
- [x] RT soft penumbra: **half-res 2×2 splat** + half dispatch (~4× fewer soft rays)  
- [x] RT dual pack: **8 lights** (pack0 RGBA + pack1 `rt_shadow_vis_hi`) — matches deferred cap  

### P3 — Quality / architecture
- [x] Soft denoise + temporal on pack1 (lights 4..7) — same Stage-2 chain as pack0  
- [x] P3.1 Phase A+B: CPU shade list packer + link remap + HUD (pp/src/light_pack.rs)
- [x] P3.1 Phase C: shade hysteresis (sticky score + hold boost + order stabilize)
- [x] P3.1 Phase D: clustered deferred (32 shade lights, CPU tile x Z lists, binding 24)  

**Design doc (full mapping / remap plan):**  
[`P3_CLUSTERED_LIGHT_CULL_DESIGN.md`](./P3_CLUSTERED_LIGHT_CULL_DESIGN.md)

---

## Next (follow-up)

| ID | Item |
|----|------|
| P3.1 Phase A | **SHIPPED** Single packer (lights+masks+vol) + prim_uid link resolve |
| P3.1 Phase B | **SHIPPED** CPU top-K shade list + shade-space link remap + sun@0 + HUD |
| P3.1 Phase C | **SHIPPED** sticky score + hold boost + order stabilize |
| P3.1 Phase D | **SHIPPED** 32 shade + CPU tile x log-Z clusters; RT still 8 |

---

## Measure checklist

1. 1 sun only → baseline deferred  
2. 4 point + RT hard → RT Shadow GPU ms  
3. 8 point + RT on → deferred should **not** explode (no soft_shadow on 5–8)  
4. Chrome SDF metal, Medium vs Draft → SDF_REFLECT cost  

---

## Files

| File | Change |
|------|--------|
| `app_gpu/src/gpu_timestamps.rs` | `PASS_RT_SHADOW` |
| `app/src/viewport_render.rs` | tw! on RT pass |
| `app/src/viewport_overlays.rs` | HUD |
| `graphics/src/deferred_entry.wgsl` | light early-out, RT overflow, SDF_REFLECT gate |
