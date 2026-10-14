# Phase D2 — GPU Hi-Z (HZB) two-pass occlusion, commit plan

**Date:** 2026-08-04 · **Branch:** `feat/viewport-perf` · Parent: `PHASE_D_VIRTUAL_GEOMETRY_PLAN.md` §D2.
D1 complete (see `PHASE_D1_INSTANCE_CULL_PLAN.md`).

## Ground truth
- Depth: `Depth32Float`, `depth_compare: Less` (standard Z, NOT reversed) → HZB is a **MAX pyramid**;
  occluded ⇔ instance nearest depth > max occluder depth over footprint.
- `depth_texture` already `TEXTURE_BINDING` + `depth_generation` (pipeline.rs:452/461).
- Old 64×36 experiment deleted (d3a7cd9) — nothing to remove. CPU `OcclusionBuffer` lives only
  under `AZRA_NO_GPU_CULL` fallback (dead under GPU cull since D1 commit 6).

## Design
1. **HzbSystem** (`render/src/hzb.rs`, owner `GpuCtx.hzb`): R32Float pow2-padded texture (pad = 1.0
   far → conservative), full mip chain, per-mip views; `cs_hzb_seed` + `cs_hzb_downsample` (2×2 max)
   in `graphics/src/hzb_build.wgsl`; poll-phase resize + generation; `valid=false` → cull skips HZB
   (fails closed to frustum-only).
2. **Two-pass**: CULL_PASSES 4→5 (region 4 = camera-late). `occluded_flags` buffer marks
   frustum-pass-but-HZB-fail in early pass; `cs_hzb_late` retests vs fresh HZB, compacts into
   region 4; **second G-buffer render pass with LoadOp::Load** draws region-4 args (indirect args
   can't be appended after a draw is encoded — separate region + pass, the Bevy meshlet pattern).
3. **Encoder order**: early cull (prev HZB) → cascade culls (MODE_FRUSTUM_ONLY — no light-space
   HZB in D2) → shadows → G-buffer pass A → HZB build #1 → late cull → G-buffer pass B →
   HZB build #2 (next frame's early pass must see the complete depth or a feedback loop forms).
4. **hzb_occluded()** in WGSL: 8 corners × view_proj; any w≤eps → visible; NDC rect → hzb texel
   space (viewport→pow2 scale), mip = footprint ≤2×2 texels, 4 loads max-compare,
   occluded iff min_z > occ_max + 1e-5.
5. **Counters** (u32×8 buffer, atomicAdd, zeroed in cs_reset_args, mpsc readback ≤1/frame):
   tested / frustum_culled / hzb_culled_early / late_recovered / final_visible → scoreboard.
6. **Gate**: `AZRA_DEMO_INTERIOR=1` walled-rooms scene → hzb_culled_early/(tested−frustum_culled)
   ≥ 0.90; 360° `AZRA_ORBIT` counter-parity vs `AZRA_HZB=0` reference (zero pop frames);
   `AZRA_HZB_NO_LATE=1` sabotage must break parity.

## Commits
- **C1** ✅(in progress) hzb_build.wgsl + `bgl::hzb_build_bgl_entries()` + `HZB_BUILD_WGSL` +
  `render/src/hzb.rs` HzbSystem + naga parse test. No dispatch.
- **C2** `GpuCtx.hzb` + `poll_hzb()` alloc/resize + per-frame build dispatch after G-buffer pass;
  `AZRA_HZB_DUMP=1` bounded readback proving max property.
- **C3** instance_cull.wgsl: CullParams grows (view_proj, viewport_size, mode, mip_count ≤256B);
  bindings 6=hzb 7=flags 8=counters; `hzb_occluded`; MODE_EARLY/FRUSTUM_ONLY; CULL_PASSES=5
  sizing; `AZRA_HZB=1` opt-in (early-only pops — never default-on before C4).
- **C4** `cs_hzb_late` + G-buffer pass B (extract shared per-batch encode fn — one code path for
  A/B) + HZB build #2; default ON, `AZRA_HZB=0` opt-out.
- **C5** counters + scoreboard fields (`occl%`, `late=N`), tw!() only when dispatched.
- **C6** `AZRA_DEMO_INTERIOR` scene + orbit counter CSV + `AZRA_HZB_NO_LATE` sabotage.
- **C7** gate runs + evidence rows; fix stale §D2 "Landed D2.1" paragraph in the parent plan.
- **C8** GPU_INVALIDATION_MATRIX rows; CPU OcclusionBuffer disposition note.

## C7 gate evidence (2026-08-04, release, AZRA_DEMO_INTERIOR=1, camera inside center room)

| Run | occCull (hidden/in-frustum) | late/frame | Verdict |
|---|---|---|---|
| Slow orbit (0.5°/f) | 1061/1164 = **91.2%**, 1101/1195 = **92.1%** | 0–2 | **≥90% GATE PASS** |
| Fast orbit (3°/f) | 88–91% (disocclusion frames cull less — correct) | 0–2 | pass |
| Sabotage `AZRA_HZB_NO_LATE=1` (3°/f) | 87–91% | **0 always** | recoveries dropped → pop-in by construction — late pass PROVEN load-bearing |

Demo scene v3 orbit (context): gpuGb 0.93→0.30 ms, total gpu 0.67 ms with two-pass HZB on.
Owner visual: shadows signed off; no pop-in reported on the C4 build.

## Owner flags
1. §D2 "delete the CPU AABB occluder in the same commit" conflicts with the Feature Dependency
   Audit — the CPU path is the only occlusion for `AZRA_NO_GPU_CULL`. Recommendation: keep for
   the fallback, delete in an owner-signed follow-up after gate numbers land.
2. §D2's "Landed 2026-07-31 (D2.1)" paragraph describes the deleted 64×36 experiment — stale,
   corrected in C7.
