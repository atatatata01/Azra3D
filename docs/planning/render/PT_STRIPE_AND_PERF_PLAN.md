# PT_STRIPE_AND_PERF_PLAN.md — Persistent horizontal banding + path-tracer performance

**Status:** 2026-10-13 — WGSL corrections + native RTX routing integrated on wgpu 25; full-res viewport probe: no banding (row/col 0.50–0.90), RTX ≡ software (|Δ| ≤ 8e-5), zero GPU errors; DF-full cold-compile blocker fixed (never → ~40 s). OPEN: owner glossy-rail scene, dense-scene RTX speed, ~4 min first-launch PT compile. Details: COMPLETE_BUG_TRACKER.md 2026-10-13 sections.
**Created:** 2026-08-09
**Owner symptom:** dense horizontal stripes on a dark glossy rail in PathTrace mode; they do **not**
fade as SPP climbs.
**Scope:** WGSL triangle path tracer only (`pathtrace_tri_entry.wgsl`, `pt_denoise.wgsl`,
`pt_upscale.wgsl`, `viewport_render_mode.rs`). No SDF/analytical work — that path is deleted
(CLAUDE.md, Analytical Path Status).

---

## 0. Provenance — what this plan merges

Two independent reviews fed this. Both are cited so the next dev can see what was kept and why.

| Source | Kept | Corrected / dropped |
|---|---|---|
| **Grok review** | The A/B protocol (denoise on/off, Fast on/off, cold static vs post-orbit); `AZRA_PT_FULL_INTERACT` as the confirming lever; denoise named as a suspect; perf inventory of existing levers | Lead hypothesis (half-res history leaking into full progressive) is **provably closed** — see §1. Proposed `pt_accum` clear is a **no-op** — see §1.3. "Primary from G-buffer = huge" is overstated and has a hard blocker — see §4.2 |
| **Claude review** | Refutation of the leak hypothesis; the idle/Fast-off shortcut that skips the A/B; reframing denoise as the primary suspect; the R2-jitter/DOF blocker on G-buffer primary | Its "iso-depth contour banding" hunch was directionally right but under-specified; §2 replaces it with the two concrete kernel defects found in `pt_denoise.wgsl` |

**Neither review reproduced the artifact.** G0 exists to close that gap before any pixel changes.

---

## 1. What is already closed — do not re-derive

Every future triage of this artifact should start here. The half-res→full "zipper leak" is the
obvious-looking answer and it is wrong.

### 1.1 Half and full progressive history are physically separate

`copy_pt_history_chain` writes `pt_half_hdr → pt_half_accum` on the half path and
`hdr_texture → pt_accum_texture` on the full path
([`gpu_init/pipelines_bg.rs:36-58`](../../../core_engine/app/src/gpu_init/pipelines_bg.rs#L36-L58)).
The full-res PT pass binds `pt_accum_texture`. It **cannot read half history.**

### 1.2 The transition hard-resets, and the shader honours the reset

half→full sets `pt_accumulated_frames = 0`
([`viewport_render.rs:1854`](../../../core_engine/app/src/viewport_render.rs#L1854)), and every
history read in the tracer is gated on `config.pt_accumulated > 1u`
([`pathtrace_tri_entry.wgsl:2484`](../../../core_engine/graphics/src/pathtrace_tri_entry.wgsl#L2484),
[`:2512`](../../../core_engine/graphics/src/pathtrace_tri_entry.wgsl#L2512),
[`:2535`](../../../core_engine/graphics/src/pathtrace_tri_entry.wgsl#L2535)). At 0 the pass
overwrites.

### 1.3 The one function that *could* leak is dead

`seed_pt_full_from_half` upscales half history into full accum — but `pt_seed_full_from_half` is
initialised `false` ([`:1847`](../../../core_engine/app/src/viewport_render.rs#L1847)), set `false`
on half→full ([`:1853`](../../../core_engine/app/src/viewport_render.rs#L1853)), and set `true`
nowhere. Already tracked as **`PT-DEADCODE-SEEDFULL`**
([`COMPLETE_BUG_TRACKER.md:3702`](../../../COMPLETE_BUG_TRACKER.md#L3702)).

**Corollary — the proposed "also clear the `pt_accum` texture, not just the counter" hardening is a
no-op.** The texture is never read while the counter is 0. It would add a full-res clear per
transition and change no pixel. Rejected under CLAUDE.md *No Bandaids* test #2 (a fix that hides
nothing but costs something).

### 1.4 In Quality mode the upscale is not in the idle pipeline at all

`resolve_pt_quality` returns `half_res` **only** when `interaction_active`, or when
`fast && idle_frames < 2`
([`viewport_render_mode.rs:227-252`](../../../core_engine/app/src/viewport_render_mode.rs#L227-L252)).
`pt_fast_mode` defaults **off** ([`types_render.rs:664`](../../../core_engine/app_types/src/types_render.rs#L664)).
The upscale dispatch lives entirely inside the `if half` branch
([`viewport_render.rs:2739-2775`](../../../core_engine/app/src/viewport_render.rs#L2739-L2775)).

> **Stripes visible on a static idle frame with Fast off ⇒ the upscale is refuted by construction.**
> This is one glance at the Fast toggle, not a two-minute A/B. It is G0's first check.

---

## 2. The actual suspects — two real defects in `pt_denoise.wgsl`

These are code defects on their own terms — both diverge from standard SVGF in ways that produce
**structured, deterministic, non-averaging** error. They are the leading stripe candidates, but
they are worth fixing regardless of what G0 concludes.

### 2.1 `weight_depth` has no depth-gradient term → filtering collapses to iso-depth contours

```wgsl
fn weight_depth(d_center: f32, d_sample: f32) -> f32 {
    let rel = abs(d_center - d_sample) / max(d_center, 0.001);
    return exp(-rel * rel * 100.0);
}
```
([`pt_denoise.wgsl:55-59`](../../../core_engine/graphics/src/pt_denoise.wgsl#L55-L59))

On a surface at a grazing angle, neighbouring pixels on the *same plane* differ in depth
substantially. With à-trous `step = 4` the tap is 4 texels away, so `rel` is large across the
slope direction and ~0 along the iso-depth direction. `exp(-rel²·100)` reaches full rejection by
`rel ≈ 0.3`. The filter therefore becomes **anisotropic, aligned to iso-depth contours** — it
smears along the surface's contour lines and rejects across them.

On a long, near-planar, grazing rail the iso-depth contours run *along the rail*. That is a
deterministic, screen-space-fixed, direction-locked structure — precisely "stripes that do not
clean up with more time," because it is bias, not variance.

**Fix (standard SVGF):** normalise by the projected depth gradient rather than by centre depth:

```
w_d = exp( -|d_c - d_s| / (σ_z · |∇d · (p - q)| + ε) )
```

`∇d` is available by finite-differencing `gbuf_normal.w`, or analytically from the view-space
normal and depth (cheaper, no extra taps). This makes the weight ~1 for any tap on the same plane
regardless of viewing angle, which is the entire point of the gradient term.

### 2.2 `weight_luma` divides by centre luma → collapses and biases on dark pixels

```wgsl
fn weight_luma(c_center: vec3<f32>, c_sample: vec3<f32>) -> f32 {
    let lc = luma(c_center);           // floored at 1e-4
    let rel = abs(lc - ls) / lc;
    let k = select(16.0, 8.0, params.pass_idx > 0u);
    return exp(-rel * rel * k);
}
```
([`pt_denoise.wgsl:66-73`](../../../core_engine/graphics/src/pt_denoise.wgsl#L66-L73))

The only floor on the denominator is `1e-4`. On a **dark** surface `lc` is tiny, so ordinary
Monte-Carlo noise makes `rel` explode and every tap rejects — the filter degenerates to the centre
pixel in some places and not others. Worse, the rejection is *correlated with the noise itself*
(a tap is kept because it happened to be close to the centre sample), which is a classic
denoiser bias: it does not average away over frames.

Dark + glossy is exactly the worst case, and exactly the material in the screenshot.

**Fix (standard SVGF):** drive the luma edge-stop from estimated variance, not from relative
distance to the centre sample:

```
w_l = exp( -|l_c - l_s| / (σ_l · sqrt(Var) + ε) )
```

Minimum viable version if a variance channel is too invasive: add a real absolute floor to the
denominator (`max(lc, ε_abs)` with `ε_abs` tied to exposure), which removes the dark-pixel
collapse without new plumbing. Ship the floor first, measure, then decide on variance.

### 2.3 The denoise never fully switches off until late

In-shader `hist_fade = 1.0 - smoothstep(4.0, 48.0, hist) * 0.92`
([`:95`](../../../core_engine/graphics/src/pt_denoise.wgsl#L95)) bottoms out at **0.08, not 0**.
CPU-side `resolve_pt_denoise_quality` only returns `skip: true` at `accum ≥ 96`, or `accum ≥ 48 &&
nf < 0.05`, or `accum ≥ 32 && (nf < 0.03 || accum ≥ 64)`
([`gpu_init/pipelines_bg.rs:444-473`](../../../core_engine/app/src/gpu_init/pipelines_bg.rs#L444-L473)).

So a scene whose residual estimate stays above those thresholds keeps a permanent ~8% blend of a
biased filter forever. Any structured error from §2.1/§2.2 is therefore **permanently visible**,
not merely slow to fade. This explains the "won't clean up with more time" observation without
requiring the half-res path at all.

---

## 3. Gates

Standard repo discipline: each gate independently shippable, build green, one concern per gate.
Do not start gate N+1 before N's exit criteria are met.

### G0 — Lock the diagnosis (no pixel changes, ~30 min)

Nobody has reproduced this yet. Everything downstream is conditional on G0.

1. Reproduce with the owner's scene. Record: **Fast mode state, denoise state, `pt_accumulated`
   from the `pt_show_spp` overlay, and whether the camera moved during accumulation.**
2. **First check (§1.4):** Fast off + static idle + stripes present ⇒ upscale refuted, go straight
   to §2. Do not run the rest of the A/B.
3. If Fast was on, re-test with Fast off and with `AZRA_PT_FULL_INTERACT=1` to separate the
   half-res path from everything else.
4. Denoise OFF, cold accumulate, no orbit. Stripes gone ⇒ G1. Stripes remain ⇒ G0b.
5. Confirm SPP is actually climbing (overlay), not parked at converged with frozen history.

**G0b (only if denoise is exonerated):** capture the raw `pt_accum` before denoise and before
tonemap and check whether the structure exists in the HDR accumulation itself. If it does, the
suspects are ReSTIR spatial reuse and the residual/adaptive-spp feedback, not the display chain.

**Exit:** one named bucket, with the toggle state that produces and suppresses the artifact.

### G1 — Fix the à-trous edge-stop functions (the likely real fix)

Gated on G0 pointing at denoise; §2.1 and §2.2 are defensible fixes regardless.

- G1a — `weight_luma` absolute floor (§2.2 minimum viable). Smallest change, highest
  dark-surface payoff. Ship and look.
- G1b — `weight_depth` gradient normalisation (§2.1). The structural fix for grazing surfaces.
- G1c — only if G1a/G1b leave residual structure: variance-driven luma weight (real SVGF), which
  needs a variance channel and is a larger change.
- G1d — decide whether `hist_fade`'s 0.08 floor should reach 0 (§2.3). A permanent fractional
  blend of *any* biased filter is a standing liability; if the filter is correct after G1a/G1b
  this can stay, and that should be a stated decision rather than an accident.

**Exit:** stripes absent on the G0 repro with denoise ON; `cargo test -p graphics` green (Naga
parse/validate for all build variants); no visible loss of denoise strength on a genuinely noisy
first-idle frame.

**Regression watch:** these weights are shared by the half-res interactive path (`gbuf_scale = 2`),
so check orbit quality too, not just idle.

### G2 — Remove the closed-out dead paths

Bookkeeping, but it stops the next reviewer re-deriving §1 (both reviews did).

- Delete `seed_pt_full_from_half` + `pt_seed_full_from_half` and their call sites, **or** wire the
  flag deliberately. Do not leave it in the current "exists but unreachable" state — that is what
  made it read as a live leak path twice. Close `PT-DEADCODE-SEEDFULL`.
- Resolve `PT-HALFHIST-STALE` ([tracker:3699](../../../COMPLETE_BUG_TRACKER.md#L3699)): full→half
  sets `pt_accumulated_frames = f.min(4).max(1)` and never clears `pt_half_accum`, so with
  temporal reproject OFF (non-default) stale half lighting blends for ~4 frames. Either set 0 on
  that transition or clear the texture. Narrow, cheap, and it is a genuine leak — unlike the one
  the reviews chased.
- Add a short "already closed" note beside the §1 code so the next reader finds the refutation at
  the site, not only in this doc.

**Exit:** both tracker entries resolved; build green.

### G3 — Performance, instrument-then-cut

Follows `PATHTRACER_PLAN.md` Phase 3 discipline. **Measure before cutting** — the PT dispatch is
already timestamped (`tw!(gpu_timestamps::PASS_RAYMARCH)`).

- **G3a — get the breakdown first.** Per-pass GPU ms at a fixed idle frame: PT dispatch, denoise
  chain, history copies, upscale. Then, inside the tracer, the split between primary traversal,
  shadow/NEE rays, ReSTIR (DI candidates + spatial), and GI. Nothing below is worth doing on
  intuition; both reviews guessed at this ordering and neither measured it.
- **G3b — cheap policy wins (low risk, do while G3a runs):** cap the à-trous at 1–2 passes under
  budget pressure (partly present); lean spatial ReSTIR harder once `pt_accumulated` is deep
  (thresholds exist at 32 — check whether they bind in practice); widen the mouse-focus radius.
- **G3c — bounce budget split.** Lookdev default of 1–2 secondary bounces with full depth reserved
  for Final is standard viewport-PT convention and is a genuine lever. Needs a UI decision, not
  just a constant.
- **G3d — BLAS/TLAS quality.** Tighter leaves / better SAH reduces node visits and helps *every*
  ray class, unlike the primary-only ideas. Rank against G3a's numbers.

**Exit:** a measured before/after table at a fixed scene + resolution, in `PERF_MATRIX_BASELINE.md`
format.

---

## 4. Rejected — with reasons (do not re-propose)

### 4.1 Clearing `pt_accum` on half→full
No-op. See §1.3.

### 4.2 "Primary hit from the G-buffer, trace only secondaries"

Rejected **as a default path**. Two problems:

- **Overstated payoff.** Primary rays are the cheapest and most coherent rays in the frame. The
  cost is the incoherent work both reviews listed first — shadow/NEE/GI over multiple bounces.
  Removing primary is a modest percentage, and G3a must produce the number before anyone spends
  a day on it.
- **Hard blocker.** Primary rays carry the **Section H R2 sub-pixel jitter**
  ([`pathtrace_tri_entry.wgsl:2540`](../../../core_engine/graphics/src/pathtrace_tri_entry.wgsl#L2540))
  — that jitter *is* the progressive anti-aliasing — and DOF (`pt_dof_aperture`). A raster
  G-buffer supplies exactly one fixed sample position per pixel. Substituting it locks primary
  visibility to the raster grid, killing silhouette AA and removing DOF. Under CLAUDE.md
  *No Hacks* that is a cheaper stand-in replacing the real path.

**Allowed variant:** a Fast-mode-only primary-from-G-buffer path, with jitter and DOF explicitly
disabled in that mode and the trade stated in the UI. Not the Quality/Final path, ever.

Note also that the G-buffer is **already** bound and consumed — `pt_gbuf_normal` / `pt_gbuf_albedo`
guide ReSTIR and the upscale. The cheap integration win there is taken; what remains is the
expensive, lossy part.

### 4.3 Reintroducing an analytical/SDF path or a GLSL BVH
Deleted 2026-07-14, owner-directed, permanent. PT is WGSL-only. (CLAUDE.md)

### 4.4 Raising default SPP to 4–8
The code documents this as a major idle FPS killer
([`viewport_render_mode.rs:277`](../../../core_engine/app/src/viewport_render_mode.rs#L277)).
The variance-guided ladder already spends samples where residual is high.

---

## 5. Definition of Done

1. `cargo build -p app` green after every gate.
2. `cargo test -p graphics` green after any shader edit (Naga parse for **all** build variants).
3. The G0 repro no longer shows stripes, verified in the app — not inferred from the kernel.
4. Half-res orbit quality checked alongside idle (the weights are shared).
5. `COMPLETE_BUG_TRACKER.md` updated: new entry for the banding class, `PT-DEADCODE-SEEDFULL` and
   `PT-HALFHIST-STALE` resolved.
6. This doc gets a §6 session-log line per gate.

**Manual test scenario:** load the rail scene, PathTrace, Fast off, denoise on. Let SPP climb to
64+ from a static camera. No horizontal structure on the dark glossy rail. Then orbit and release —
structure must not appear during settle or persist after.

---

## 6. Session log

- **2026-08-09** — Plan created. Merged two reviews; refuted the half-res-leak hypothesis against
  the code (§1); located two concrete SVGF divergences in `pt_denoise.wgsl` (§2). No code changed.
- **2026-10-12** — Reused `pt_pcg_hash` for integer-safe jitter/spatial streams instead of
  taking `fract` of large FP32 products. Dedicated FP32 raw accumulation now weights actual
  samples across variable-SPP batches and has no 2048 history cap. Welford sample moments
  replace change-of-mean as the noise estimate; colour and variance reproject together.
  Fixed ordered denoise uniforms, separate colour/reservoir reprojection uniforms,
  plane-gradient filtering, and variance-guided luminance filtering. No unconditional
  96-frame denoise shutdown. Idle transport depth stays fixed; GPU budget adjusts sample
  work, not the estimator. Removed unused full-resolution GI warming (it could not feed
  the physically separate half-resolution history). Heavy PT pipelines compile in one
  background readiness job; no frame-thread ensure fallback. GPU budgets use completed
  GPU intervals, not CPU encode time. The previous permanent-8-percent conclusion in
  section 2.3 was incorrect even before these changes: CPU policy skipped at 96 frames.
  GPU regressions cover long indices/counts, reset, variable-SPP weighting, stationary
  noise, dark detail, slopes, edges, and ordered multi-pass parameters. Native RTX must
  retain canonical material WGSL; flat-material OptiX is not a release substitute.
