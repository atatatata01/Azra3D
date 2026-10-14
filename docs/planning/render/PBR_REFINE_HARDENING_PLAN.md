# PBR Refine Hardening Plan — fix + improve backlog from the 2026-08-07 full review

**Status: Phase A–E SHIPPED (2026-08-07). Plan complete.**
Source: the 3-agent + self audit of the Progressive PBR system (findings + the 9
same-day fixes are logged in `COMPLETE_BUG_TRACKER.md` "Progressive-PBR full
review"). Parent doc: `PROGRESSIVE_PBR_PLAN.md` (Phases 1–3b shipped).
Discipline: **one phase per session**, each phase ends build-green with
`cargo test -p graphics` (84) + `cargo test -p render` (100) passing and its
gate scenario verified. Never blanket `is_dirty`; all new controls runtime-only
(save-format red line); interactive path stays bit-identical.

### Phase A changelog
- **A1** Gate CSM cascade render + sun-cone jitter + shadow ladder climb while
  Leak-Proof RT sun owns the frame (`pbr_refine_active && pbr_refine_rt_sun &&
  RT buffers ready`). Probe mini-frames keep CSM (force `pbr_refine_active=false`).
- **A2** Refine cadence: 16 ms (N&lt;256) → 50 ms (N&lt;1024) → 250 ms (else).
- **A3** `probe_sh` strength: write-on-change via `GpuCtx.probe_sh_strength_written`.

### Phase B changelog
- **B1** Probe mini-frames: `s = min(2×face, min(w,h))`; raster viewport/scissor
  top-left s×s (FOV 90° square); deferred dispatch s×s only; capture `src_min=(0,0)`;
  `cam_right.w = s` so deferred UV matches the sub-rect (`deferred_entry.wgsl`).
- **B2** Probe frames: frustum-only GPU cull (no HZB), skip HZB rebuild, skip CPU
  Hi-Z, do not touch `raster_upload_plane_key` / instance re-upload.
- **B3** Cache probe face/mip/SH views + capture/mip/SH bind groups keyed on
  `probe_cube_generation` (+ HDR size for capture BGs).

### Phase C changelog
- **C1** pass_ssr: `spec_occ = specular_occlusion(ndv, ssao_factor, rough)` multiplies
  both IBL subtract and probe/traced add; ssr_bgl + AO_UPSAMPLE extract + ssao@38.
- **C2** SSGI `probe_diffuse_w` (was pad): env-subtraction × `(1 − w)`.
- **C3** SSGI binary refine uses world-uniform `p_start + d*(t_end*um)`.
- **C4** `probe_sample` full slab test (`tmax > max(tmin, 0)`).
- **C5** SSGI clamp single `ao·mb`; near-plane only when `dz < 0`; frame-term comment.

### Phase D changelog
- **D1** `compositor_neigh()` reads `pbr_accum_in` when bit 256 + N&gt;1 for DoF/MB/sharpen/viewport-DoF.
- **D2** Drop `pbr_probe_ready` from refine sig (no auto second bake round).
- **D3** Probe publish: `pbr_refine_frames = 0` (accum restart without sig churn).
- **D4** Per-light RT disc phase; probe_capture bilinear; Karis probe mip; SSR +0.5 UV;
  delete count-weighted `rt_alpha` (fixed 0.9).

### Phase E changelog
- **E1** `pbr_accum_*` + RT denoise pairs: 1×1 dummies; full-res on first need
  (`ensure_pbr_accum_full` / `ensure_rt_denoise_full`).
- **E2** `idle_quality_high` Low/High; `resolution_scale` in Progressive Refine UI;
  drop 8192 shadow-cap tier (max 4096).
- **E3** probe_bake comments (sig / raymarch-lite floor); rt_alpha nested-EMA retired in D4.

---

## Phase A — Stop paying for discarded work (perf, biggest win, lowest risk)

**A1 — Gate CSM machinery off while the RT sun owns the frame.**
While accumulating with Leak-Proof Sun on, post_flags bit 512 replaces the sun's
CSM term in BOTH deferred branches — the cascades' only consumer — yet every
refine frame still: jitters the sun direction (cascades can never cache),
re-renders 3 cascades at LADDER-BOOSTED resolution, and climbs the ladder at
N=32/128 (each climb reallocates 3 cascade maps AND restarts accumulation).
- Do: compute one `sun_owned_by_rt` predicate (same condition as bit 512,
  CPU-side) and gate: (a) the sun-cone jitter in `raster_gbuffer.rs`, (b) the
  ladder climb + its accumulation restart in `viewport_render.rs`, (c) the
  cascade render passes for the sun when it is the only shadow-casting
  directional. KEEP cascades for: probe mini-frames (they set
  `rt_shadows_on = 0` and legitimately consume CSM), the RT-sun-off opt-out,
  and interaction frames (bit 512 off).
- Files: `viewport_render.rs` (refine block + ladder), `raster_gbuffer.rs`
  (jitter + cascade dispatch), `app_poll.rs` (ladder application).
- Risk: MEDIUM — the probe-frame and opt-out paths must keep CSM; verify with
  Leak-Proof toggled both ways + a probe bake round.
- Win: ms-class per refine frame (vertex-bound scenes), kills 2 pointless
  accumulation restarts per idle session, up to ~192 MB texture churn gone.

**A2 — Taper the refine cadence.**
16 ms until N=256 (unchanged), 50 ms until N=1024, then 250 ms to the 4096 cap.
1/N steps past ~1/1000 are invisible; this cuts ~75% of the long-tail idle GPU
burn while keeping "quality to infinite".
- Files: `viewport_render.rs` (:426 cadence). Risk: NIL.

**A3 — probe_sh strength: write-on-change.** The 4-byte upload currently runs
every frame even with the feature off. Cache last-written value on GpuCtx.
- Files: `viewport_render.rs`, `gpu_ctx.rs`. Risk: NIL.

**Gate A:** AZRA_REFINE_LOG=1 shows no ladder restarts while Leak-Proof on;
shadows still correct with Leak-Proof OFF (ladder + cone jitter return);
probe round still bakes; suites green.

---

## Phase B — Probe bake cost (perf, ~halves bake-round frame cost) — SHIPPED 2026-08-07

**B1 — Probe mini-frames render a SUB-RECT, not the full viewport.** ✅
`s = min(2 × face_size, min(w, h))`; raster viewport/scissor top-left s×s
(FOV 90° square via `probe_frame_side`); deferred dispatch s×s only;
capture `src_min=(0,0)`; `cam_right.w = s` so deferred UV matches
(`deferred_entry.wgsl`). Texels outside the rect go stale — capture never
reads them. Fog/SSR/tonemap do NOT run in probe frames.
- Files: `probe_bake.rs`, `raster_gbuffer.rs`, `deferred_entry.wgsl`.

**B2 — Probe frames must not pollute main-view culling.** ✅
Probe frames: frustum-only GPU cull (no HZB mode), skip HZB rebuild, skip
CPU Hi-Z, never rewrite `raster_upload_plane_key` / instance buffer.
- Files: `raster_gbuffer.rs`, `probe_bake.rs` (`probe_frame_side` gate).

**B3 — Cache probe views + bind groups** ✅
Per-face mip0 views, capture BGs (HDR-size keyed), mip reduce views/BGs,
SH view/BG — all keyed on `probe_cube_generation`. Steady-state: 0 creates
per face after warm.
- Files: `gpu_ctx.rs`, `probe_bake.rs`, `viewport_render.rs`, `app_poll.rs`.

**Gate B:** `cargo test -p graphics` 84 + `cargo test -p render` 100 green;
probe preview balls unchanged path; [PROBE] log center/extent unchanged.

---

## Phase C — Energy accounting (quality: over-dark corners / over-bright metals) — SHIPPED 2026-08-07

**C1 — SSR/probe delta must subtract OCCLUDED ibl.** ✅
`ssao_factor` + `specular_occlusion` via AO_UPSAMPLE extract; multiply both the
IBL subtract and probe/traced add. ssr_bgl + ssr_bg + ssr_gpu_test bind ssao@38.
- Files: `post_process.wgsl`, `shader_build.rs`, `app_new.rs`, `viewport_render.rs`,
  `ssr_gpu_test.rs`.

**C2 — SSGI env-occlusion must respect the light-probe blend.** ✅
`SsgiParams.probe_diffuse_w` (was pad); shader scales env-subtraction by `(1 − w)`.
- Files: `types.rs`, `ssgi_entry.wgsl`, `viewport_render.rs`, `ssgi_gpu_test.rs`.

**C3 — SSGI binary-refine parameterization.** ✅
Refine: `q = p_start + d * (t_end * um)` (world-uniform, matches coarse loop).

**C4 — probe_sample slab test.** ✅
`tmin` + require `tmax > max(tmin, 0)`; else un-parallaxed `refl`.

**C5 — SSGI small fixes.** ✅
Clamp includes ao·mb once (no second multiply); near-plane only when `dz < 0`;
frame-term comment updated.

**Gate C:** `cargo test -p graphics` 84 + `cargo test -p render` 100 green.

---

## Phase D — Convergence completeness (quality, polish) — SHIPPED 2026-08-07

**D1 — Compositor effects read the CONVERGED image.** ✅
`compositor_neigh()` → `pbr_accum_in` when bit 256 and N&gt;1; used by DoF, motion
blur, sharpen, viewport DoF. (Read previous mean — same-pass can't read
`pbr_accum_out`.)

**D2 — Single first bake round.** ✅
Decision: remove `pbr_probe_ready` from refine sig (not an intentional second
bounce). Scene/toggle/res changes still invalidate via remaining hash fields.

**D3 — Probe re-publish restarts accumulation.** ✅
`pbr_refine_frames = 0` at round publish in `probe_bake.rs`.

**D4 — Sampler polish batch.** ✅
Per-light RT disc phase; probe_capture bilinear; Karis mip remap; SSR +0.5 UV;
`rt_alpha` always 0.9 (count-weighted branch deleted).

**Gate D:** `cargo test -p graphics` 84 + `cargo test -p render` 100 green.

---

## Phase E — Memory + low-end tier — SHIPPED 2026-08-07

**E1 — Lazy-allocate refine-only textures.** ✅
1×1 `pbr_accum_*` + RT denoise/history dummies at resize; full-res via
`ensure_pbr_accum_full` / `ensure_rt_denoise_full` (tonemap BG rebuild on swap).

**E2 — Idle Quality Low/High.** ✅
`idle_quality_high` gates SSGI×2 and SSR×2 idle boosts; `resolution_scale` slider
in Progressive Refine UI; shadow cap 2048/4096 only (8192 gone).

**E3 — Doc/comment debt.** ✅
probe_bake sig/viewport + no raymarch-lite floor notes; rt_alpha nested EMA
retired with D4.

**Gate E:** `cargo test -p graphics` 84 + `cargo test -p render` 100; app green.

---

## Execution order + why

A → B → C → D → E. A and B are pure cost removal (no look change — safest
early); C changes pixel output where it is currently WRONG (needs the visual
gates); D refines converged output; E is resource hygiene. Each phase is
sized for one session. If the owner wants a quality-first order: C → A → B →
D → E is acceptable (C is independent of A/B).

## Out of scope (tracked elsewhere)

- Emissive-drag material report (diagnostic pending, tracker "Material system"
  entry — needs AZRA_MAP_APPLY=1 evidence).
- Multi-probe / authored probes, GGX prefilter tier (owner-deferred later
  tier, parent plan).
- Intrude fixture load failures (poly stream, predates this work).
