//! Render-mode decision (pure) — extracted from viewport_render.rs (de-god slice 14).
//!
//! The `resolve_render_mode` free fn + its input structs + the DRAFT_* consts.
//! These are already free (non-method) items written specifically to dodge the
//! `&self` vs `&mut self.gpu` borrow conflict in `dispatch_gpu_render`.

use crate::*;

/// While the heaviest pipeline (deferred-FULL) compiles on the background thread (minutes on a cold
/// cache), render a THROTTLED draft with the already-live lite/raster path instead of blacking out the
/// viewport for the whole compile. Set `false` to restore the old hard-skip (the conservative TDR-safe
/// behaviour) if a driver ever starves on the concurrent compile.
pub(crate) const DRAFT_RENDER_DURING_COMPILE: bool = true;
/// Repaint interval while drafting during a heavy compile (~8 fps) — keeps the viewport live for camera
/// orbit while leaving the GPU ample slack for the driver's pipeline compilation (TDR mitigation).
pub(crate) const DRAFT_COMPILE_REPAINT_MS: u64 = 120;
/// Fallback hard cap when settings are unavailable. Prefer
/// `RenderSettings::pt_max_accum_frames` / `pt_target_frames_for_threshold`.
pub(crate) const PT_MAX_ACCUM_FRAMES: u32 = 4096;

/// Legacy quality-proxy frame budget (kept for UI estimates only).
/// Real stop is `pt_stop_mode`: Infinite / Noise residual / Max frames.
pub(crate) fn pt_target_frames(threshold: f32, max_frames: u32) -> u32 {
    let t = threshold.clamp(0.001, 0.5);
    let raw = ((0.04 / t) * (0.04 / t) * 16.0).round() as u32;
    raw.clamp(16, max_frames.max(16))
}

/// True while PathTrace should keep dispatching progressive samples this session.
/// Camera motion / scene dirt clears `pt_converged` and restarts accumulation.
#[inline]
pub(crate) fn pt_should_keep_sampling(
    stop_mode: u8,
    converged: bool,
    frames: u32,
    max_frames: u32,
) -> bool {
    if converged {
        return false;
    }
    match stop_mode {
        // Infinite: always keep going until something marks converged=false path via cam move
        0 => true,
        // Noise: keep going until residual stop sets converged (max_frames = safety only)
        1 => frames < max_frames.max(16),
        // Max frames only
        _ => frames < max_frames.max(16),
    }
}

/// A4: progressive PT repaint interval (ms) while idle and not converged.
/// Clean residual → slower cadence (save GPU/heat); noisy or early → ~60 Hz.
/// Camera motion / interaction callers pass `interaction_active = true` → always 16.
#[inline]
pub(crate) fn pt_progressive_repaint_ms(
    interaction_active: bool,
    accum_frames: u32,
    noisy_frac: f32,
) -> u64 {
    if interaction_active {
        return 16;
    }
    let nf = noisy_frac.clamp(0.0, 1.0);
    // Very clean long history: maintenance drip (~20 fps)
    if accum_frames >= 48 && nf < 0.04 {
        return 50;
    }
    // Settling clean: ~30 fps
    if accum_frames >= 24 && nf < 0.08 {
        return 33;
    }
    // Mild residual: slight throttle
    if accum_frames >= 16 && nf < 0.15 {
        return 24;
    }
    // Active progressive refine
    16
}

// ── Section A: interactive quality ladder (half-res lighting while moving) ────

/// C2: target completed GPU budget for a PathTrace frame (ms). Above this we lean
/// spp / denoise; only interactive half-res may also lean bounce depth.
pub(crate) const PT_FRAME_BUDGET_MS: f32 = 28.0;
/// F — tighter budget when Fast Path Trace is on (prefer FPS).
pub(crate) const PT_FRAME_BUDGET_FAST_MS: f32 = 20.0;

/// Path-trace quality for this frame. Policy only — shaders consume the fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PathTraceQuality {
    /// 0.5 = half width×height (25% pixels), 0.67 settling, 1.0 full progressive.
    pub resolution_scale: f32,
    pub samples_per_pixel: u32,
    pub max_bounces: u32,
    /// True when lighting is traced below full viewport resolution.
    pub half_res: bool,
}

/// C2: pressure 0 = under budget, 1 = 2× budget, etc. (from completed GPU-frame ms).
#[inline]
pub(crate) fn pt_budget_pressure(last_frame_ms: f32) -> f32 {
    pt_budget_pressure_with(last_frame_ms, PT_FRAME_BUDGET_MS)
}

#[inline]
pub(crate) fn pt_budget_pressure_with(last_frame_ms: f32, budget_ms: f32) -> f32 {
    if last_frame_ms <= 0.0 || last_frame_ms < budget_ms {
        return 0.0;
    }
    ((last_frame_ms - budget_ms) / budget_ms).clamp(0.0, 3.0)
}

/// C2: lean PathTraceQuality when completed GPU frames overran the budget.
///
/// Full progressive may lean spp only under pressure. **Never force half-res lighting
/// when the quality ladder already chose full progressive** — demoting idle full→half
/// under budget pressure left permanent horizontal zipper bands (half lighting
/// upscaled every frame, then progressive-averaged into history).
/// Half-res is only for interaction/settle from `resolve_pt_quality`.
pub(crate) fn apply_pt_frame_budget(
    mut q: PathTraceQuality,
    last_frame_ms: f32,
    fast: bool,
) -> PathTraceQuality {
    let budget = if fast {
        PT_FRAME_BUDGET_FAST_MS
    } else {
        PT_FRAME_BUDGET_MS
    };
    let p = pt_budget_pressure_with(last_frame_ms, budget);
    if p <= 0.0 {
        return q;
    }
    // Mild overage: cap multi-spp
    q.samples_per_pixel = 1;
    if q.half_res {
        q.max_bounces = 1;
    }
    // Only *stay* lean on half if we were already half (orbit/settle). Do not
    // demote full progressive → half under load (causes persistent scanline banding).
    if q.half_res && fast && p > 0.4 {
        q.resolution_scale = 0.5;
        q.max_bounces = q.max_bounces.min(2);
    }
    if p > 0.75 {
        if q.half_res {
            q.resolution_scale = 0.5;
        }
    }
    q
}

/// C2: scale denoise step budget (1.0 = full, lower when over budget).
#[inline]
pub(crate) fn pt_denoise_budget_scale(last_frame_ms: f32) -> f32 {
    pt_denoise_budget_scale_with(last_frame_ms, false)
}

#[inline]
pub(crate) fn pt_denoise_budget_scale_with(last_frame_ms: f32, fast: bool) -> f32 {
    let budget = if fast {
        PT_FRAME_BUDGET_FAST_MS
    } else {
        PT_FRAME_BUDGET_MS
    };
    let p = pt_budget_pressure_with(last_frame_ms, budget);
    if p <= 0.0 {
        return 1.0;
    }
    if p > 1.25 {
        return 0.35; // force ~1 step
    }
    if p > 0.5 {
        return if fast { 0.4 } else { 0.55 };
    }
    if fast {
        0.6
    } else {
        0.75
    }
}

impl PathTraceQuality {
    pub fn lighting_size(self, full_w: u32, full_h: u32) -> (u32, u32) {
        if self.half_res || self.resolution_scale <= 0.51 {
            ((full_w + 1) / 2, (full_h + 1) / 2)
        } else if self.resolution_scale < 0.99 {
            let s = self.resolution_scale.clamp(0.5, 1.0);
            (((full_w as f32) * s).round().max(64.0) as u32,
             ((full_h as f32) * s).round().max(64.0) as u32)
        } else {
            (full_w, full_h)
        }
    }
}

/// Resolve interactive PT quality.
///
/// - **Moving** (camera/object drag): 50% lighting, 1 spp, 2 bounces — never freezes.
/// - **Settling** (first few idle frames): 67% lighting, 1 spp, 3 bounces.
/// - **Idle progressive**: full res, variance-guided spp with the fixed user bounce target.
/// - **Fast** (`fast=true`): fixed three-bounce ceiling, adaptive 1–2 spp.
///
/// `user_spp` / `user_bounces` are the UI knobs (keep spp small: 1–4).
/// `noisy_frac` is residual adaptive-stop fraction (0=clean, 1=all noisy).
/// `accum_frames` is progressive history length.
pub(crate) fn resolve_pt_quality(
    interaction_active: bool,
    idle_frames_after_move: u32,
    user_spp: u32,
    user_bounces: u32,
    noisy_frac: f32,
    accum_frames: u32,
    fast: bool,
) -> PathTraceQuality {
    let base_spp = user_spp.max(1);
    let user_b = user_bounces.max(1);
    if interaction_active {
        PathTraceQuality {
            resolution_scale: 0.5,
            samples_per_pixel: 1,
            max_bounces: user_b.min(if fast { 1 } else { 2 }),
            half_res: true,
        }
    } else if idle_frames_after_move < 2 {
        // One-frame settle after orbit, then full progressive so cleanup starts immediately.
        // Keep half only in Fast mode (FPS). Quality mode jumps to full-res right away so
        // half-res upscale zipper never enters progressive history after a short orbit.
        if fast {
            PathTraceQuality {
                resolution_scale: 0.5,
                samples_per_pixel: 1,
                max_bounces: user_b.min(2),
                half_res: true,
            }
        } else {
            PathTraceQuality {
                resolution_scale: 1.0,
                samples_per_pixel: 1,
                max_bounces: user_b,
                half_res: false,
            }
        }
    } else if fast {
        // F — Fast progressive: 1 spp (rarely 2), short bounce depth; history cleans.
        let nf = noisy_frac.clamp(0.0, 1.0);
        let spp = if nf > 0.5 && base_spp >= 2 && accum_frames < 16 {
            2
        } else {
            1
        };
        PathTraceQuality {
            resolution_scale: 1.0,
            samples_per_pixel: spp,
            max_bounces: user_b.min(3),
            half_res: false,
        }
    } else {
        // Section P: variance-guided SPP — spend samples where residual is high,
        // lean once the image is mostly clean (progressive still averages).
        // Cap multi-spp hard: doubling to 8 was a major idle FPS killer.
        let nf = noisy_frac.clamp(0.0, 1.0);
        let spp = if nf > 0.45 {
            base_spp.max(1).min(3)
        } else if nf > 0.2 {
            base_spp.max(1).min(2)
        } else if nf < 0.05 && accum_frames >= 48 {
            1 // clean enough: 1 spp/frame, history does the rest
        } else if nf < 0.1 && accum_frames >= 24 {
            base_spp.min(2)
        } else {
            base_spp.min(2)
        };
        PathTraceQuality {
            resolution_scale: 1.0,
            samples_per_pixel: spp,
            max_bounces: user_b,
            half_res: false,
        }
    }
}

#[cfg(test)]
mod pt_quality_tests {
    use super::*;

    #[test]
    fn moving_is_half_res_one_spp_two_bounces() {
        let q = resolve_pt_quality(true, 0, 4, 8, 1.0, 0, false);
        assert!(q.half_res);
        assert!((q.resolution_scale - 0.5).abs() < 1e-5);
        assert_eq!(q.samples_per_pixel, 1);
        assert_eq!(q.max_bounces, 2);
        let (w, h) = q.lighting_size(1920, 1080);
        assert_eq!((w, h), (960, 540));
    }

    #[test]
    fn idle_is_full_res_user_spp() {
        // Mid residual keeps two samples and the fixed user bounce target.
        let q = resolve_pt_quality(false, 20, 2, 6, 0.3, 10, false);
        assert!(!q.half_res);
        assert_eq!(q.samples_per_pixel, 2);
        assert_eq!(q.max_bounces, 6);
        assert_eq!(q.lighting_size(1920, 1080), (1920, 1080));
    }

    #[test]
    fn settling_only_first_idle_frame() {
        // Quality mode: settle is already full-res (avoids half zipper in progressive history).
        let q = resolve_pt_quality(false, 0, 4, 8, 1.0, 0, false);
        assert!(!q.half_res, "quality settle must not use half-res lighting");
        let q2 = resolve_pt_quality(false, 2, 4, 8, 0.5, 2, false);
        assert!(!q2.half_res, "full progressive must start by idle frame 2");
        // Fast mode may keep half for one settle frame
        let qf = resolve_pt_quality(false, 0, 4, 8, 1.0, 0, true);
        assert!(qf.half_res, "fast settle may keep half-res");
    }

    #[test]
    fn variance_boosts_spp_when_noisy() {
        // Quality mode: noisy residual may lift spp; still hard-capped (no more ×2→8)
        let q = resolve_pt_quality(false, 20, 1, 4, 0.6, 5, false);
        assert!(q.samples_per_pixel >= 1 && q.samples_per_pixel <= 3);
    }

    #[test]
    fn variance_leans_spp_when_clean() {
        let q = resolve_pt_quality(false, 20, 4, 4, 0.02, 64, false);
        assert_eq!(q.samples_per_pixel, 1);
    }

    #[test]
    fn fast_mode_stays_lean_when_idle() {
        // Early noisy: at most 2 spp; never the old multi-spp explosion
        let q = resolve_pt_quality(false, 20, 4, 8, 0.6, 5, true);
        assert!(q.samples_per_pixel <= 2);
        assert!(q.max_bounces <= 3);
        // Settled: pure 1 spp
        let q2 = resolve_pt_quality(false, 20, 4, 8, 0.1, 30, true);
        assert_eq!(q2.samples_per_pixel, 1);
    }

    #[test]
    fn a4_repaint_fast_when_interacting() {
        assert_eq!(pt_progressive_repaint_ms(true, 100, 0.01), 16);
    }

    #[test]
    fn a4_repaint_throttles_when_clean_idle() {
        assert_eq!(pt_progressive_repaint_ms(false, 64, 0.02), 50);
        assert_eq!(pt_progressive_repaint_ms(false, 32, 0.05), 33);
        assert_eq!(pt_progressive_repaint_ms(false, 5, 0.5), 16);
    }

    #[test]
    fn c2_budget_no_pressure_under_target() {
        assert_eq!(pt_budget_pressure(20.0), 0.0);
        let q = resolve_pt_quality(false, 20, 4, 6, 0.3, 10, false);
        let q2 = apply_pt_frame_budget(q, 20.0, false);
        assert_eq!(q2, q);
    }

    #[test]
    fn c2_budget_leans_when_over() {
        let q = resolve_pt_quality(false, 20, 4, 8, 0.5, 5, false);
        let lean = apply_pt_frame_budget(q, 50.0, false); // well over 28ms
        assert_eq!(lean.samples_per_pixel, 1);
        assert_eq!(lean.max_bounces, 8);
        assert!(pt_denoise_budget_scale(50.0) < 0.6);
    }

    #[test]
    fn c2_budget_fast_does_not_demote_full_to_half() {
        // Idle full progressive + budget overage: lean spp only — never reintroduce half-res
        // banding into progressive history.
        let q = resolve_pt_quality(false, 20, 1, 4, 0.5, 5, true);
        assert!(!q.half_res);
        let lean = apply_pt_frame_budget(q, 45.0, true);
        assert!(!lean.half_res, "must not demote full progressive to half under budget pressure");
        assert_eq!(lean.samples_per_pixel, 1);
    }

    #[test]
    fn c2_budget_keeps_half_when_already_half() {
        let q = resolve_pt_quality(true, 0, 1, 4, 1.0, 0, true);
        assert!(q.half_res);
        let lean = apply_pt_frame_budget(q, 45.0, true);
        assert!(lean.half_res, "orbit half-res may stay half under pressure");
        assert_eq!(lean.samples_per_pixel, 1);
    }

    #[test]
    fn variance_does_not_reduce_bounces_when_clean() {
        let q = resolve_pt_quality(false, 20, 1, 8, 0.05, 64, false);
        assert_eq!(q.max_bounces, 8);
    }

    #[test]
    fn variance_keeps_bounces_when_noisy() {
        // Quality path keeps the user bounce target regardless of residual.
        let q = resolve_pt_quality(false, 20, 1, 8, 0.5, 5, false);
        assert_eq!(q.max_bounces, 8);
    }

    #[test]
    fn full_progressive_bounce_target_is_invariant() {
        for fast in [false, true] {
            for user_bounces in [0, 1, 2, 3, 6, 12] {
                let expected = if fast { user_bounces.max(1).min(3) } else { user_bounces.max(1) };
                for idle_frames in [0, 1, 2, 20] {
                    for age in [0, 2, 24, 48, 96, 4096] {
                        for noise in [0.0, 0.04, 0.2, 0.5, 1.0] {
                            let quality = resolve_pt_quality(false, idle_frames, 4, user_bounces, noise, age, fast);
                            if quality.half_res {
                                continue;
                            }
                            assert_eq!(quality.max_bounces, expected);
                            for budget_ms in [0.0, 5.0, 20.0, 28.0, 40.0, 100.0, 1000.0] {
                                let budgeted = apply_pt_frame_budget(quality, budget_ms, fast);
                                assert_eq!(budgeted.max_bounces, expected, "fast={fast}, bounces={user_bounces}, idle={idle_frames}, age={age}, noise={noise}, ms={budget_ms}");
                                assert!(!budgeted.half_res);
                                assert_eq!(budgeted.resolution_scale, 1.0);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn full_progressive_budget_adapts_samples_only() {
        for fast in [false, true] {
            let quality = resolve_pt_quality(false, 20, 4, 8, 1.0, 0, fast);
            assert!(quality.samples_per_pixel > 1);
            assert_eq!(apply_pt_frame_budget(quality, 5.0, fast), quality);
            let budgeted = apply_pt_frame_budget(quality, 100.0, fast);
            assert_eq!(budgeted.samples_per_pixel, 1);
            assert_eq!(budgeted.max_bounces, quality.max_bounces);
        }
    }
}

/// Pre-computed pipeline readiness flags, extracted before the mutable gpu borrow
/// to avoid borrow conflicts with `resolve_render_mode`.
pub(crate) struct GpuReadiness {
    pub(crate) wgsl_pipelines_ready: bool,
}

/// Inputs to `resolve_render_mode` extracted from `FourthLayerApp` fields before
/// the mutable gpu borrow begins, sidestepping the `&self` vs `&mut self.gpu` conflict.
pub(crate) struct RenderModeInputs {
    pub(crate) shading_mode: ShadingMode,
    /// Number of instances in the assembled TRIANGLE PT scene (Phase 2). The
    /// PathTrace arm requires > 0 — geometry presence is measured in triangles,
    /// never in SDF prim count (the analytical PT is banned, owner 2026-07-16).
    pub(crate) pt_instance_count: u32,
    /// P0.2-full: true in Object Mode (== !sdf_edit_active). Forces the no-march AnalyticalLite path so
    /// SDF renders from its baked mesh via the raster pass — never a G-buffer-writing march/VP that
    /// would fuse with the voxel raster over one G-buffer (device-loss class). PathTrace is exempt
    /// (checked first): it writes HDR only, never the G-buffer.
    pub(crate) unified_raster: bool,
    /// True while the deferred-FULL pipeline is compiling on the background thread. Forces the lightest
    /// renderable path (AnalyticalLite) so a heavy march/PT/BVH submission
    /// never starves the concurrent driver compile (TDR class). See `DRAFT_RENDER_DURING_COMPILE`.
    pub(crate) draft_compile: bool,
    /// **A — Live Hybrid while SDF editing:** when true, PathTrace yields this frame so Hybrid
    /// raster (partial bakes) shows live shape feedback. `pt_live_hybrid_edit` + SDF edit episode.
    pub(crate) pt_live_hybrid_feedback: bool,
    /// **AI exclusive:** Flux / Neural Beauty is running — yield PathTrace so the GPU isn't
    /// thrashing between PT and torch (4090 class: both want most of the VRAM).
    pub(crate) ai_gpu_exclusive: bool,
}

/// Compute the single `RenderMode` for this frame.  First match in the priority
/// ladder wins — all later arms are unreachable by construction.
///
/// Free function (not a method) to avoid borrow conflicts with the mutable
/// `self.gpu` borrow in `dispatch_gpu_render`.
pub(crate) fn resolve_render_mode(
    inputs: &RenderModeInputs,
    nv: u32,
    use_lite: bool,
    interaction_active: bool,
    gpu_ready: &GpuReadiness,
) -> RenderMode {
    // -1. Heavy-compile draft — while deferred-FULL compiles on the background thread, force the lightest
    //     renderable path so a heavy march / PT / GLSL-BVH submission can't starve the concurrent driver
    //     compile (TDR class — the case the old hard-skip guarded). AnalyticalLite (the
    //     Display Invariant forbids a volume raymarch fused with the raster G-buffer).
    //     The deferred pass auto-falls-back to deferred-lite.
    if inputs.draft_compile {
        return RenderMode::AnalyticalLite;
    }
    // 0. TRIANGLE path trace (Phase 2 — the only PT; the analytical PT is banned, owner
    //    2026-07-16). Sits ABOVE the unified-raster guard: this does NOT reintroduce the
    //    G-buffer fusion hazard the guard exists for (commit 4f1a30a) — the triangle PT
    //    shader writes ONLY the HDR texture (no G-buffer bindings at all), and the deferred
    //    pass (the G-buffer consumer) is separately gated off in PT mode, so the displayed
    //    image is PT's HDR alone: one display technique for the view (Display Invariant
    //    holds). The raster pass still runs underneath for GPU picking (selection must
    //    always work), overlay-line depth, and denoiser normals — its color contribution is
    //    fully overwritten by PT.
    //    Gate: the assembled TLAS must have ≥1 instance — geometry presence measured in
    //    TRIANGLES (any body kind), not SDF prim count. An empty scene falls through to the
    //    raster path (nothing to trace; the traversal is loop-bounded either way).
    //    **A — Live Hybrid while SDF editing:** when `pt_live_hybrid_feedback`, skip PT this
    //    frame so the unified-raster arm shows live mesh bake feedback (toggleable).
    if inputs.shading_mode == ShadingMode::PathTrace
        && inputs.pt_instance_count > 0
        && !inputs.pt_live_hybrid_feedback
        && !inputs.ai_gpu_exclusive
    {
        return RenderMode::PathTrace;
    }
    // 1. P0.2-full — Object Mode (unified raster). SDF renders from its BAKED MESH via the raster pass;
    //    the primary compute pass must NEVER march the SDF into the G-buffer here, or it
    //    fuses with the voxel raster over one G-buffer (the device-loss class, commit 4f1a30a).
    //    AnalyticalLite writes only background + floor and early-exits where the raster already wrote.
    //    (PT is exempt and sits above — it writes HDR only, never the G-buffer.)
    if inputs.unified_raster {
        return RenderMode::AnalyticalLite;
    }
    // (2. Volume preview REMOVED 2026-07-13 — the SDF volume-raymarch display path is stripped.)
    // 2b. Raster-only scene (no SDF primitives — e.g. a voxel sculpt, which is an ImportedMesh
    //     drawn by the raster pass). `use_lite` hard-requires nv>0, so without this a voxel
    //     scene falls to the heavy AnalyticalMarch/Full path EVERY frame — a full-screen
    //     raymarch of an EMPTY SDF scene — which is the persistent rotate-lag. The lite path
    //     writes only background+floor (no marching) and already handles raster meshes; the
    //     raster pass draws the voxel on top. Skips the wasted per-frame SDF march.
    if nv == 0 {
        return RenderMode::AnalyticalLite;
    }
    // (3. GLSL BVH / 5. AnalyticalMarch / 6. AnalyticalFull arms REMOVED 2026-07-14 —
    //     the dead analytical SDF march display path is stripped. AnalyticalLite (floor +
    //     background) is the sole non-PathTrace compute pass; SDF renders from its baked
    //     DC mesh via the raster pass.)
    // 4. Analytical Lite — the only remaining raymarch display mode.
    let _ = (use_lite, interaction_active, gpu_ready);
    RenderMode::AnalyticalLite
}
