# Progressive PBR Refinement — "infinite quality" accumulation for the raster viewport

**Status: Phases 1–3 SHIPPED 2026-08-06 (Phase 3 = auto reflection probe v1);
Phase 3b (auto LIGHT probe) SHIPPED 2026-08-07. Follow-up fix/improve program:
`PBR_REFINE_HARDENING_PLAN.md` (Phases A–E, from the 2026-08-07 full review).**
Owner request 2026-08-06.

**Phase 2c — leak-proof refined sun (owner "light leaking" screenshot 2026-08-07:
"usually we thicken walls, but that is artistically restrictive — automate the fix"):**
- Root cause of thin-wall leaks is structural to shadow MAPS: the anti-acne bias
  (normal offset + slope bias, texel-scaled) lets light through any wall thinner
  than the bias in world units. Rays need no bias — a paper-thin wall blocks a ray
  exactly. A concurrent 2026-07-28 change had already taught `rt_shadow_entry.wgsl`
  to trace DIRECTIONAL lights (sun = angular cone at RT_SUN_DIST), but per-light
  opt-in only.
- **The automated fix:** while the refine accumulator averages (post_flags bit 256),
  the sun traces RT shadows WITHOUT per-light opt-in: `RtShadowParams.sun_tan`
  (< 0 = off) carries the "Sun Softness (idle)" cone; the ray pass takes the disc
  path even at 1 sample/frame with per-frame golden-ratio RADIUS jitter (a fixed
  0.5 converges to a ring, not a disc), and the 1/N accumulator integrates the
  cone. Deferred's sun swap + contact-shadow suppression gain the bit-256 arm;
  `rt_shadows_active()` keeps the TLAS resident whenever progressive + the new
  **"Leak-Proof Sun Shadows"** toggle (default ON) are enabled. Interactive frames
  keep fast CSM (leaks only visible while moving).
- **Two pre-existing gaps fixed on the way:** `any_rt_soft_light()` excluded
  directional lights (stale "sun stays on CSM" doc) — a sun-only scene with an
  authored Source Radius silently collapsed to the hard center ray; and the Lit
  early-return branch never applied the 07-28 sun RT swap (full builds kept CSM
  for the sun in Lit while PBR traced).

**Phase 3 as-built (auto reflection probe v1 — `app/src/probe_bake.rs`):**
- ONE scene-centered 128² RGBA16F cubemap (full mip chain, ~1MB). While the refine
  accumulator is idle-averaging (N ≥ 4), each frame captures ONE face: the camera is
  swapped to the probe basis, `dispatch_raster_pass` + the full deferred pipeline
  render into the ordinary G-buffer/HDR (own submits — transient scratch, the main
  frame re-renders right after), `probe_capture` resolves the HDR's central 90°
  square into the face, and `probe_mip_reduce` box-builds the mips.
- **Orientation is structurally safe**: probe_capture re-projects each texel's
  SPEC-defined cube direction through the exact render-camera basis (carried in the
  params), so capture and hardware cube sampling cannot disagree.
- **Placement/invalidation**: probe center = scene AABB midpoint, proxy = AABB
  half-extents ×1.25 (from `last_gu.scene_aabb_*`); a bake round is keyed to
  `pbr_refine_sig` — any content/lighting change schedules a fresh 6-frame round,
  and the previous probe keeps serving while faces are replaced in place.
- **Consumption**: `pass_ssr` bindings 65/66/67 — the probe is the (1 − confidence)
  fallback with the SAME energy-conserving envelope: traced → probe → env cross-fade
  at every confidence seam (screen edge, march distance, roughness fade, miss, and
  the rough > SSR_ROUGH_MAX early-out). Parallax-corrected against the proxy AABB.
  Probes display only when SSR is enabled (documented v1 coupling).
- **v1 limits (documented)**: probe faces reuse the main view's SSAO (subtle);
  RT point-light shadows are off inside probe frames (screen-space visibility is
  view-locked — sun CSM is fully correct); mips are box, not GGX; single probe
  (multi-probe/authored probes = the owner-deferred later tier; the cube-array-
  ready data layout is the extension point).

**Phase 3b as-built (auto LIGHT probe — SHIPPED 2026-08-07, owner "the auto probe
should auto place light probe"):** the same bake doubles as a light probe.
- On round completion (Phase D), `probe_sh_project` (probe.wgsl, 1×1×1 dispatch)
  projects the cube's 8×8 mip (bound as a 6-layer D2Array view at mip
  `log2(N)−3`) into SH9 RGB irradiance coefficients with per-texel solid-angle
  weights, into a 10×vec4 storage buffer (`probe_sh_buffer`, created eagerly and
  zero-filled ⇒ valid=0 ⇒ structurally inert).
- **Ownership split**: rows [0..8] + [9].w (valid) are GPU-written by the
  projection; [9].x (strength) is CPU-written every frame from the settings
  (4-byte `write_buffer`) — the toggle/slider act immediately, off writes 0.
- **Consumption**: deferred binding 51 (`deferred_bgl_entries()` single source).
  `probe_sh_irradiance()` = Ramamoorthi eval, π-normalized to the same
  exit-radiance convention as `env_sh_irradiance`. Four hooks, PBR + Lit
  symmetric: no HDRI → the flat ambient gains the probe term (AO-occluded);
  HDRI → diffuse IBL `mix(env_sh, probe_sh, weight)` with the bent normal —
  never both (ambient double-count class).
- Settings `pbr_refine_light_probe` (default on) + `light_probe_strength`
  (0..1, default 1), both hashed into the refine sig; UI under the probe group
  ("Auto Light Probe" / "Bounce Strength").
- **v1 limits**: irradiance sampled at ONE point (the probe center) applied
  scene-wide — correct-ish for rooms, coarse for large multi-room scenes
  (multi-probe blending is the same later tier as authored reflection probes);
  probe faces render before this round's SH exists, so a round's faces carry the
  PREVIOUS round's bounce (second-bounce lag, converges over rounds — a feature:
  cheap infinite-bounce approximation).

**Phase 2 as-built (all frame terms gated on the accumulate verdict — static/legacy
values otherwise, so the interactive path stays bit-identical):**
- **Shadow PCF rotation**: `ShadowUniforms.params.z` carries a golden-angle rotation
  (`raster_gbuffer` feeds `(N % 64) × 2.3999632`); `gbuffer.wgsl` rotates the 2×2/3×3
  tap patterns by it (rot 0 = exact identity). Cascade fitting now reads the
  UN-jittered camera so texel snapping stays stable under Phase-1 jitter.
- **Idle shadow-resolution ladder**: `pbr_refine_shadow_boost` (runtime, 0–2) steps
  at N=32 (2×) and N=128 (4×), capped by the new "Idle shadow cap" setting
  (`pbr_refine_shadow_res_cap`, default 4096, UI row under the Progressive Refine
  toggle). Applied in `app_poll` via `set_resolution` (Frame Ordering Contract);
  the boost is hashed into the refine sig so each step restarts accumulation on the
  new maps; any ineligibility (interaction) restores the user's base immediately.
- **SSAO/GTAO**: `SsaoParams.frame_jitter` (was `_pad`) animates the IGN rotation
  (`+ frame × 5.588238`) only while accumulating — AO converges to the true
  hemisphere integral.
- **SSR**: `ssr_hash` gains a per-frame offset behind post_flags bit 256, and the
  march budget doubles (`ssr_max_steps × 2`, ≤128) while accumulating.
- **RT shadows**: `rt_shadow_denoise_dispatch` takes `blend_alpha`; the caller
  passes count-weighted `1 − 1/N` while accumulating (true running mean → converges)
  and the legacy fixed 0.9 EMA otherwise.

**Phase 2b — accurate refined shadows (owner screenshot 2026-08-06: "shadows not
accurate" — directional smears + contact leaks in the converged image):**
- Diagnosis: two smear sources in refined frames. (1) The deferred ANALYTICAL SDF
  soft-shadow march runs while idle and samples the field, not the displayed baked
  DC mesh — field inflation self-shadows convex edges and misregisters cast shadows;
  the accumulator faithfully averages the smear. (2) The Phase-2 PCF tap rotation
  swept the square pattern through a ~1.41× annulus — it widened the fixed blur but
  could never produce a geometric penumbra.
- Fix: while refining, occlusion comes from ONE accurate source. Each idle frame
  renders the CSM cascades from a sun direction jittered inside a small cone
  ("Sun Softness (idle)", `pbr_refine_sun_angle_deg`, default 1°, R3 sequence →
  concentric disc), so the 1/N mean converges to the PHYSICAL area-light penumbra of
  the rasterized geometry — with correct contact hardening. The analytical
  soft_shadow/calc_ao duplicate is suppressed on refined frames
  (`gu.interaction_no_shadows`), deciding the "two occlusion systems coexist" item
  for refined frames only; shading keeps the un-jittered sun (no specular shimmer).
  The PCF tap rotation is retired (uniform fed 0; shader keeps the capability).

**Phase 1 as-built (deviations from the plan text below, all quality-neutral):**
- Accumulation happens INSIDE `pass_tonemap` (post_flags bit 256) right after the
  SSR/SSGI/bloom/fog composites — not in a separate resolve pass. Same math, same
  point in the chain, zero composite-code duplication and no new pipeline: bindings
  63 (`pbr_accum_in`) / 64 (`pbr_accum_out`) on tonemap_bgl, out→in copied after the
  dispatch (the `copy_pt_history_chain` pattern). `pt_accumulated` carries the PBR
  frame count outside PathTrace.
- Jitter consistency shipped for the raster proj (`raster_gbuffer.rs`, clip-space
  translate) + every GpuUniforms `camera_ray()` (sdf_shared + post_process,
  `pbr_jitter` in the `_pad_wire` tail slots — struct stays 640). NOT jittered, by
  decision: `raymarch_lite`'s floor/background (its GpuUniforms copy is a truncated
  prefix; the floor grid simply keeps today's aliasing — zero-mean sub-pixel
  disagreement at mesh↔floor boundaries converges), and SSAO/SSR's own pixel→ray
  reconstruction (zero-mean error, converges; their per-frame jitter terms are
  Phase 2 work anyway).
- Overlay passes (line/face) bind a new `camera_nojitter_buffer` so wires never
  shimmer over the stable accumulated image.
- FXAA auto-skips once N ≥ 8 (the average out-resolves it; FXAA-of-average softens).
- Converged behavior: at N = 4096 repaint requests stop (idle GPU goes fully quiet)
  but the accumulate bit stays on, so any dirty tick keeps displaying the frozen
  average instead of popping back to a raw aliased frame.
- Reset signature is a curated hash (lights, env, fov, exposure/tonemapper, every
  post toggle+param, bg, grid, selection via `prev_sel_hash` with one frame of lag,
  viewport size) + content-dirty flags (`prim_buf_stale`, `light_gpu_data_stale`,
  mesh-cache pending) + PT-style camera epsilons. Known gap: time-animated VFX
  blended into HDR would smear into the average — extend the signature when VFX
  animate, never reset on is_dirty.
- Split view is safe by construction (alternating cameras trip the epsilon every
  dispatch → accumulation never passes the store-only N=1 frame).
**Owner ask (verbatim intent):** a setting for the PBR viewport that adds progressive
rendering — slowly increasing shadow resolution, better raytrace, better reflection rays —
"like a path tracer but on PBR tech, the progressive will continue to infinite the quality."
Follow-up in the same session: **auto reflection probes** — "I never want to add reflection
probe by hand for now; later, for more advanced settings, we will let the user craft. For
fast product shots, small props, or a basic room, auto probes are fine."

Industry precedent: Blender EEVEE / Workbench **viewport sample accumulation** — when the
camera goes idle, keep rendering cheap jittered frames and average them, so AA, soft
shadows, AO and glossy reflections converge toward reference. Interaction stays exactly as
fast as today because refinement runs ONLY while idle.

`RENDERER_ROADMAP.md` Tier-1 item #2 ("TAA — NEXT") is the moving-camera sibling of this
feature; this plan is the idle-only half, which needs **no motion vectors** and is far
lower-risk. Landing this first also de-risks TAA later (jitter plumbing is shared).

---

## Why this is cheap here — recon findings (2026-08-06)

1. **The averaging math already exists and is proven**: `pathtrace_entry.wgsl:152-162` —
   `mix(old, new, 1/N)` running mean into `pt_accum`, with the `hdr → accum` write-back
   in `copy_pt_history_chain()` (`gpu_init/pipelines_bg.rs:18-59`). Copy, don't invent.
2. **Idle/reset discipline already exists and is battle-tested**: camera-moved epsilon
   compares (`viewport_render.rs:1151-1162`), `pt_lighting_sig` (`:1197-1222`),
   `pt_mouse_idle_secs` (`:1226`), interaction episode + half-res gate (`:100-175`).
   The hard-won warning at `viewport_render.rs:1144-1147` applies verbatim: **reset only
   on camera/lighting/scene signature — NEVER on blanket `is_dirty`.**
3. **Every stochastic raster pass is deliberately frozen per-pixel today**, each with a
   comment citing "no TAA in this engine": SSAO/GTAO IGN (`ssao.wgsl:20-26, :64-68`),
   SSR hash (`post_process.wgsl:264-268`), SSGI (`ssgi_entry.wgsl:60-65`), shadow PCF
   (no frame term at all, `gbuffer.wgsl:536-581`). This feature is exactly the
   accumulator that lifts that constraint — the freeze comments become "frame term is
   nonzero only while the progressive accumulator is active."
4. **RT shadows already have per-frame jitter + temporal history**
   (`rt_shadow_entry.wgsl:170`, `rt_shadow_temporal.wgsl`) but blend with a FIXED
   0.9 EMA (`pipelines_bg.rs:222`) — it smooths, never converges. Switching that alpha
   to count-weighted `1/N` while idle is the single highest-value, lowest-risk change.
5. **Shadow map resolution is already runtime-switchable with generation counters**:
   `ShadowMapSystem::set_resolution()` (`shadows.rs:292`), ladder
   `[1024, 2048, 4096, 8192]` (`shadows.rs:17`), applied from `app_poll.rs:284`
   (Frame Ordering Contract respected).
6. **The redraw pump must be extended** — `viewport_render.rs:193-205`:
   `if !self.is_dirty && !pt_accumulating { return; }`. Add a sibling
   `pbr_accumulating` condition or idle frames never dispatch.
7. **Fog already does per-frame temporal accumulation on the raster path**
   (`fog_frame_index`, `fog_entry.wgsl:437-443`) — the shipped example of the
   increment/reset discipline.

## FEATURE_RULES gate answers

- **New scene object?** No (Phase 3 probes are runtime render resources, not graph nodes;
  manual/authored probes are explicitly deferred by the owner).
- **Mutates state?** Runtime render state only. All new settings are runtime-only —
  deliberately NOT in the bincode `ProjectFile` (save-format red line, `ssr_max_steps`
  precedent).
- **Frame-thread work?** Yes — one accumulate dispatch per idle frame + probe faces
  baked ≤1 face/frame (Frame Budget Rule cap).
- **New render path?** No new depth convention. Accumulation is a compute post pass on
  `hdr_texture` (same class as PT accum). Probe bake re-uses the existing raster
  G-buffer + deferred pipelines into off-screen cube faces — one technique, its own
  targets, Display-Invariant clean.

---

## Design

### The master switch

`render.pbr_progressive_enabled` (runtime-only, default **OFF** — a new heavy-ish idle
workload must not change behavior silently; `ssgi_enabled` precedent). UI: "Progressive
Refine" checkbox in the anti-aliasing/quality group of both settings panels (next to
FXAA / Wireframe AA), hover text stating it only works while the camera is idle and can
be left off on low-end machines (owner's performance-setting directive 2026-08-06).

State machine (all reusing PT's machinery, renamed generically or duplicated minimally):

- **Interacting** (camera moving / tool busy / episode active): exactly today's pipeline.
  Zero jitter, zero accumulation, zero cost. Latched by the existing interaction episode.
- **Idle**: each frame renders with per-frame jitter (below), then
  `pbr_accum = mix(pbr_accum, hdr, 1/N)`; display the accumulated result. N grows
  without bound → "progressive continues to infinite quality" (float mean; N capped at
  ~1e6 for numerics, indistinguishable from infinite).
- **Reset** (N→0): camera-moved epsilon test, lighting signature, scene edits (the PT
  reset sites list at `viewport_render.rs:1140-1300` is the checklist), resize,
  shading-mode change, any quality-affecting slider.

### Phase 1 — the accumulator + converging AA (the foundation)

1. New `pbr_accum` RGBA16F texture (clone the `pt_accum` descriptor,
   `viewport_render.rs:937`).
2. Composite-then-accumulate: today `pass_tonemap` composites SSR/SSGI/fog deltas at
   tonemap time (`post_process.wgsl:1275, :1291`). Split it: a small HDR-composite step
   folds those deltas into HDR first, then the accumulate pass averages, then tonemap
   reads the average with its composite bits off. This mirrors the PT chain exactly and
   makes EVERYTHING upstream converge (accumulating post-tonemap LDR would be
   mathematically wrong — tonemap is nonlinear).
3. Camera sub-pixel jitter while idle: port the **R2 low-discrepancy sequence** from
   `pathtrace_tri_entry.wgsl:2506`. MUST be applied consistently to BOTH camera
   representations — the raster `view_proj` AND the compute `cam_right/cam_up + fov_scale`
   ray reconstruction — or the G-buffer and deferred/SSAO/SSR disagree by a sub-pixel and
   the error is a systematic bias, not converging noise.
4. Extend the redraw pump (`viewport_render.rs:205`) with `pbr_accumulating`.
5. FXAA is skipped once N ≥ ~8 — accumulated jitter converges edges better than FXAA
   can, and FXAA-of-an-average softens them back.

**Exit gate 1:** static scene, idle 2s → visibly smoother edges than FXAA; orbit →
bit-identical to today's interactive pipeline; no accumulation restarts from UI/poll
ticks (the `:1144` trap), verified by an on-screen N counter (debug overlay).

### Phase 2 — converging shadows, AO, reflections ("better raytrace / reflection rays")

Each item = a frame term added to an existing deterministic seed, active ONLY while
accumulating (frame term forced 0 otherwise — interactive path stays bit-identical):

1. **Shadow PCF**: per-frame golden-angle rotation of the PCF disc + sub-texel cascade
   offset → penumbra converges toward area-light quality with the same 4/9 taps per
   frame. Respect the texel-snapping note (`shadows.rs:426`) — jitter is applied *after*
   snap, as a separate uniform, so the interactive path's stability is untouched.
2. **Progressive shadow resolution** (owner's "slowly increasing shadow resolution"):
   on idle, step the ladder 1024 → 2048 → 4096 (→ 8192 behind an "Ultra" toggle) at
   accumulation milestones (e.g. N=1, N=32, N=128) via the existing
   `set_resolution()` + generation path from poll; each step restarts accumulation
   (cheap — the first frame at the higher res already looks better than the old
   average). Interaction restores the interactive resolution.
3. **SSAO/GTAO**: add `frame_index` to `SsaoParams` (free `_pad` slots at
   `ssao.wgsl:42/:47`), fold into `ign()` → AO converges to the true hemisphere
   integral. (Note `SSAO_VISIBILITY_FINDING.md`: the visible payoff is bounded until
   AO's application point is widened — convergence still removes the dither pattern.)
4. **SSR** ("better reflection rays"): per-frame term in `ssr_hash()`, plus an idle
   step-budget boost (`ssr_max_steps` ×2..4 while accumulating — idle frames have the
   budget) and idle removal of the interaction skip (`viewport_render.rs:2612`). Glossy
   cone taps converge to a proper filtered reflection instead of a static dither.
5. **RT shadows**: while accumulating, replace the fixed `blend_alpha 0.9` EMA with
   count-weighted `1/N` (needs a history-count source; the accumulator's N uniform
   serves) → raytraced soft shadows genuinely converge instead of plateauing.
6. **SSGI**: when its Gate-2 march lands (`SSGI_PLAN.md`), it inherits the frame term
   for free — the plan's §4.3 "no jitter without an accumulator" objection is exactly
   what this feature retires.

**Exit gate 2:** A/B stills at N=1 vs N≥256: penumbra gradient smooth (no PCF banding),
AO dither gone, glossy SSR stable and filtered. Interactive path pixel-diff == zero
(AZRA_VISDIFF rig).

### Phase 3 — auto reflection probes (owner follow-up request)

Goal: local specular reflections for objects the HDRI can't provide (product shots,
props, small rooms) with ZERO authoring. Manual/craftable probes are explicitly **later**
(owner). Design for the auto tier:

1. **One auto probe** placed at the scene-content centroid (scene AABB center, weighted
   toward the camera target); a `auto_reflection_probes` toggle (default ON when
   progressive is on; independent checkbox). No graph node, no gizmo, nothing to author.
2. **Bake**: 6 cube faces at 128–256px through the existing G-buffer raster + deferred
   pipelines (no SSR/SSAO/SSGI/no probes in the probe render — one bounce, no
   recursion), ≤1 face per frame (Frame Budget Rule), only while idle, re-baked when the
   scene-settle signal fires (reuse `dirty_bodies` quiet + lighting_sig — the same
   signature that resets accumulation schedules a probe re-bake).
3. **Prefilter**: GGX roughness mip chain (compute prefilter, standard split-sum — the
   env IBL path's convolution is the in-repo reference).
4. **Consumption**: in deferred specular, where SSR confidence < 1, fall back
   SSR → probe (parallax-corrected against the scene AABB as the proxy volume) → global
   HDRI. This slots into the existing SSR blend seam (`ssr_intensity` already models
   "hit confidence → IBL fallback").
5. **Progressive tie-in**: probe faces re-render with the accumulation jitter and can
   themselves accumulate a few frames per face while idle — reflections sharpen over
   time like everything else.
6. **Later (out of scope now, owner-deferred)**: multiple probes (per-room/cluster
   auto-placement by scene-graph body clustering), authored probe volumes, probe
   blending. The auto probe's data layout (cubemap array, per-probe proxy AABB) is
   designed array-first so N probes is an extension, not a rewrite.

**Exit gate 3:** chrome sphere on a floor in a closed room reflects the room (not just
HDRI) with SSR off; probe re-bakes automatically after moving a wall; total probe cost
while interacting = zero (bake is idle-only).

---

## Settings summary (all runtime-only, save-format untouched)

| Setting | Default | Where |
|---|---|---|
| `pbr_progressive_enabled` — "Progressive Refine" | OFF | AA/quality group, both panels |
| `pbr_progressive_max_shadow_res` — idle shadow ladder cap | 4096 | Progressive sub-group |
| `auto_reflection_probes` — "Auto Reflection Probe" | ON (gated by progressive for bake scheduling; consumption works once baked) | Progressive sub-group |
| Debug: N counter overlay | OFF | Developer tweaks |

Everything runs idle-only, so the performance floor on low-end machines is exactly
today's pipeline; the toggles exist to also shed the idle cost (owner performance
directive).

## Risks / traps (from recon)

- **Jitter consistency** raster `view_proj` vs compute ray reconstruction (bias, not
  noise, if they diverge) — Phase 1 item 3.
- **Reset discipline**: `viewport_render.rs:1144-1147` — signature-based resets only.
- **Ortho**: PT's reprojection helpers are perspective-only; this feature does hard
  resets (no reprojection) so ortho is safe by construction — do NOT add reprojection
  without an ortho branch (see the 2026-08-06 ortho depth fix's still-open list).
- **Ghosting is impossible by design** (unlike TAA): accumulation only ever runs on a
  provably static camera+scene and resets on any signature change.
- **Memory**: +1 RGBA16F full-res (pbr_accum) + probe cubemap (256² ×6 ×mips ≈ 3MB).
- **`ShadingMode::Raytrace` is vestigial** (CUDA-only consumers; `raytrace_entry.wgsl`
  does not exist; the WGSL fallback is worse than PBR). This plan does NOT build on it.
  Candidate for removal or for becoming the "Progressive" mode's badge later — decision
  deferred.

## Phase order & rough effort

1. Phase 1 (accumulator + jittered AA + pump/reset wiring): the enabling core.
2. Phase 2 (shadow/AO/SSR frame terms + shadow-res ladder + RT-shadow 1/N): several
   small independent shader edits once Phase 1 exists — batch WGSL edits per file
   (shader-recompile cost rule).
3. Phase 3 (auto probe): the largest single piece (cube bake + prefilter + deferred
   consumption); independent of Phase 2, depends on Phase 1's idle scheduler only.

One phase per session (gated-plan discipline).
