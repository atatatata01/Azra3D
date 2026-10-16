//! Background pipeline compile + runtime PT/RT dispatch helpers.

use eframe::wgpu;
use crate::GpuCtx;

use super::bgl::{
    build_rt_denoise_bg, build_rt_denoise_bg_hi, build_rt_temporal_bg, build_rt_temporal_bg_hi,
    make_pt_denoise_bg,
};
use super::cache::save_pipeline_cache;
use super::pipelines_lazy::ensure_rt_denoise_pipelines;


/// C3: batch PT frame history copies into one tight encoder block (no intervening
/// compute). Order: raw accumulation output → prev, restir out → prev, gi out → prev.
/// Half and full use parallel texture pairs; extent is lighting resolution.
#[inline]
pub(crate) fn copy_pt_history_chain(
    enc: &mut wgpu::CommandEncoder,
    gpu: &GpuCtx,
    half: bool,
    w: u32,
    h: u32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let ext = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let copy = |enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, dst: &wgpu::Texture| {
        enc.copy_texture_to_texture(src.as_image_copy(), dst.as_image_copy(), ext);
    };
    if half {
        if let (Some(ref src), Some(ref dst)) = (&gpu.pt_half_accum_output, &gpu.pt_half_accum) {
            copy(enc, src, dst);
        }
        if let (Some(ref src), Some(ref dst)) =
            (&gpu.pt_half_restir_out, &gpu.pt_half_restir_prev)
        {
            copy(enc, src, dst);
        }
        if let (Some(ref src), Some(ref dst)) = (&gpu.pt_half_gi_out, &gpu.pt_half_gi_prev) {
            copy(enc, src, dst);
        }
    } else {
        if let (Some(ref src), Some(ref dst)) = (&gpu.pt_accum_output, &gpu.pt_accum_texture) {
            copy(enc, src, dst);
        }
        if let (Some(ref src), Some(ref dst)) = (&gpu.pt_restir_out, &gpu.pt_restir_prev) {
            copy(enc, src, dst);
        }
        if let (Some(ref src), Some(ref dst)) = (&gpu.pt_gi_out, &gpu.pt_gi_prev) {
            copy(enc, src, dst);
        }
    }
}


/// Clear a texture only when the device was created with `Features::CLEAR_TEXTURE`.
/// Skips (no-op) when the feature is unavailable — safer than validation crash.
/// Used for PT reservoir invalidation (camera motion / half→full seed).
#[inline]
pub(crate) fn clear_texture_safe(
    enc: &mut wgpu::CommandEncoder,
    device: &wgpu::Device,
    tex: Option<&wgpu::Texture>,
) {
    let Some(tex) = tex else { return };
    if !device.features().contains(wgpu::Features::CLEAR_TEXTURE) {
        return;
    }
    enc.clear_texture(tex, &wgpu::ImageSubresourceRange::default());
}


/// Dispatch the RT projected-shadow denoise chain for **both** packs (0 = lights 0..3, 1 = 4..7).
/// Spatial bilateral then temporal EMA per pack; returns (history_valid_lo, history_valid_hi).
///
/// Frame ordering: called at the TOP of the post-march block (before the temporal-history copy) so
/// `prev_cam` / temporal_prev_normal_dist still hold the PREVIOUS frame.
/// `blend_alpha`: history weight of the temporal EMA. 0.9 = fixed boil-resistant
/// EMA (~10 effective samples). D4 removed the count-weighted `1−1/N` path — it
/// nested a second 1/N inside the progressive-HDR mean.
pub(crate) fn rt_shadow_denoise_dispatch(
    gpu: &mut GpuCtx,
    enc: &mut wgpu::CommandEncoder,
    width: u32, height: u32,
    prev_cam: &[f32; 16], cur_cam: &[f32; 16],
    history_valid: bool,
    history_valid_hi: bool,
    radius: u32,
    blend_alpha: f32,
) -> (bool, bool) {
    ensure_rt_denoise_pipelines(gpu);
    let ready = gpu.pipelines.rt_denoise_pipeline.is_some()
        && gpu.pipelines.rt_temporal_pipeline.is_some()
        && gpu.rt_shadow_vis.is_some() && gpu.rt_shadow_denoised.is_some()
        && gpu.rt_shadow_history.is_some() && gpu.gbuf_normal.is_some()
        && gpu.temporal_prev_normal_dist.is_some();
    if !ready { return (history_valid, history_valid_hi); }
    // Lazy param uniforms (16 B + 160 B), created once.
    if gpu.buffers.rt_denoise_params_buffer.is_none() {
        gpu.buffers.rt_denoise_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt_denoise_params"), size: std::mem::size_of::<graphics::RtDenoiseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
    }
    if gpu.buffers.rt_temporal_params_buffer.is_none() {
        gpu.buffers.rt_temporal_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rt_temporal_params"), size: std::mem::size_of::<graphics::RtTemporalParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
    }
    let dparams = graphics::RtDenoiseParams { radius, width, height, _pad: 0 };
    gpu.queue.write_buffer(gpu.buffers.rt_denoise_params_buffer.as_ref().unwrap(), 0, dparams.as_bytes());
    let (gx, gy) = ((width + 7) / 8, (height + 7) / 8);

    // Build all bind groups first (mutable) before any dispatch borrows pipelines.
    if gpu.bind_groups.rt_denoise_bg.is_none() { build_rt_denoise_bg(gpu); }
    if gpu.bind_groups.rt_temporal_bg.is_none() { build_rt_temporal_bg(gpu); }
    let hi_ready = gpu.rt_shadow_vis_hi.is_some()
        && gpu.rt_shadow_denoised_hi.is_some()
        && gpu.rt_shadow_history_hi.is_some();
    if hi_ready {
        if gpu.bind_groups.rt_denoise_bg_hi.is_none() { build_rt_denoise_bg_hi(gpu); }
        if gpu.bind_groups.rt_temporal_bg_hi.is_none() { build_rt_temporal_bg_hi(gpu); }
    }

    // ── Pack 0 (lights 0..3) ──
    {
        let tparams = graphics::RtTemporalParams {
            prev_cam_pos: [prev_cam[0], prev_cam[1], prev_cam[2], prev_cam[3]],
            prev_cam_forward: [prev_cam[4], prev_cam[5], prev_cam[6], prev_cam[7]],
            prev_cam_right: [prev_cam[8], prev_cam[9], prev_cam[10], prev_cam[11]],
            prev_cam_up: [prev_cam[12], prev_cam[13], prev_cam[14], prev_cam[15]],
            cur_cam_pos: [cur_cam[0], cur_cam[1], cur_cam[2], cur_cam[3]],
            cur_cam_forward: [cur_cam[4], cur_cam[5], cur_cam[6], cur_cam[7]],
            cur_cam_right: [cur_cam[8], cur_cam[9], cur_cam[10], cur_cam[11]],
            cur_cam_up: [cur_cam[12], cur_cam[13], cur_cam[14], cur_cam[15]],
            screen_size: [width, height],
            blend_alpha,
            depth_reject: 0.05,
            frame_valid: if history_valid { 1 } else { 0 },
            _pad: [0, 0, 0],
        };
        gpu.queue.write_buffer(gpu.buffers.rt_temporal_params_buffer.as_ref().unwrap(), 0, tparams.as_bytes());
        if let (Some(bg), Some(pl)) = (
            gpu.bind_groups.rt_denoise_bg.as_ref(),
            gpu.pipelines.rt_denoise_pipeline.as_ref(),
        ) {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("rt_denoise"),timestamp_writes:None});
            cp.set_pipeline(pl); cp.set_bind_group(0, bg, &[]); cp.dispatch_workgroups(gx, gy, 1);
        }
        if let (Some(bg), Some(pl)) = (
            gpu.bind_groups.rt_temporal_bg.as_ref(),
            gpu.pipelines.rt_temporal_pipeline.as_ref(),
        ) {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("rt_temporal"),timestamp_writes:None});
            cp.set_pipeline(pl); cp.set_bind_group(0, bg, &[]); cp.dispatch_workgroups(gx, gy, 1);
        }
        if let (Some(a), Some(c)) = (gpu.rt_shadow_vis.as_ref(), gpu.rt_shadow_history.as_ref()) {
            enc.copy_texture_to_texture(a.as_image_copy(), c.as_image_copy(),
                wgpu::Extent3d{width, height, depth_or_array_layers:1});
        }
    }

    // ── Pack 1 (lights 4..7) ──
    let mut hi_valid = history_valid_hi;
    if hi_ready {
        let tparams = graphics::RtTemporalParams {
            prev_cam_pos: [prev_cam[0], prev_cam[1], prev_cam[2], prev_cam[3]],
            prev_cam_forward: [prev_cam[4], prev_cam[5], prev_cam[6], prev_cam[7]],
            prev_cam_right: [prev_cam[8], prev_cam[9], prev_cam[10], prev_cam[11]],
            prev_cam_up: [prev_cam[12], prev_cam[13], prev_cam[14], prev_cam[15]],
            cur_cam_pos: [cur_cam[0], cur_cam[1], cur_cam[2], cur_cam[3]],
            cur_cam_forward: [cur_cam[4], cur_cam[5], cur_cam[6], cur_cam[7]],
            cur_cam_right: [cur_cam[8], cur_cam[9], cur_cam[10], cur_cam[11]],
            cur_cam_up: [cur_cam[12], cur_cam[13], cur_cam[14], cur_cam[15]],
            screen_size: [width, height],
            blend_alpha,
            depth_reject: 0.05,
            frame_valid: if history_valid_hi { 1 } else { 0 },
            _pad: [0, 0, 0],
        };
        gpu.queue.write_buffer(gpu.buffers.rt_temporal_params_buffer.as_ref().unwrap(), 0, tparams.as_bytes());
        if let (Some(bg), Some(pl)) = (
            gpu.bind_groups.rt_denoise_bg_hi.as_ref(),
            gpu.pipelines.rt_denoise_pipeline.as_ref(),
        ) {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("rt_denoise_hi"),timestamp_writes:None});
            cp.set_pipeline(pl); cp.set_bind_group(0, bg, &[]); cp.dispatch_workgroups(gx, gy, 1);
        }
        if let (Some(bg), Some(pl)) = (
            gpu.bind_groups.rt_temporal_bg_hi.as_ref(),
            gpu.pipelines.rt_temporal_pipeline.as_ref(),
        ) {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("rt_temporal_hi"),timestamp_writes:None});
            cp.set_pipeline(pl); cp.set_bind_group(0, bg, &[]); cp.dispatch_workgroups(gx, gy, 1);
        }
        if let (Some(a), Some(c)) = (gpu.rt_shadow_vis_hi.as_ref(), gpu.rt_shadow_history_hi.as_ref()) {
            enc.copy_texture_to_texture(a.as_image_copy(), c.as_image_copy(),
                wgpu::Extent3d{width, height, depth_or_array_layers:1});
        }
        hi_valid = true;
    }

    (true, hi_valid)
}


/// Request the FULL deferred pipeline (`deferred_pipeline`) on a **background thread**.
/// Never blocks the frame thread — CLAUDE.md: ensure_* are fallbacks, all heavy compiles
/// go on the warmup/bg path. Callers keep using deferred-lite until `deferred_pipeline`
/// is `Some` (polled via `df_full_compile_rx` in `poll_sdf_compile`).
///
/// Largest WGSL (~170KB SPIR-V); cold compile can take ~25–43s. Pipeline cache makes
/// subsequent launches near-instant.
pub(crate) fn request_deferred_full_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.deferred_pipeline.is_some() {
        return;
    }
    if gpu.pipelines.df_full_compile_started {
        return;
    }
    gpu.pipelines.df_full_compile_started = true;
    let device = gpu.device.clone();
    let cache = gpu.pipelines.pipeline_cache.clone();
    let flag = gpu.pipelines.compiling_heavy_shader.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    gpu.pipelines.df_full_compile_rx = Some(rx);
    let spawn = std::thread::Builder::new()
        .name("azra-df-full".into())
        .spawn(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            let t = std::time::Instant::now();
            let result = crate::panic_safety::catch_recoverable(std::panic::AssertUnwindSafe(|| {
                let cache_ref = (*cache).as_ref();
                let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("deferred_bgl_ondemand"),
                    entries: &graphics::bgl::deferred_bgl_entries(),
                });
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("deferred_full_ondemand"),
                    bind_group_layouts: &[&bgl],
                    push_constant_ranges: &[],
                });
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("DeferredFull (on-demand bg)"),
                    source: wgpu::ShaderSource::Wgsl(graphics::build_deferred_wgsl(None).into()),
                });
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("deferred_full"),
                    layout: Some(&pl),
                    module: &shader,
                    entry_point: Some("deferred_shade"),
                    compilation_options: Default::default(),
                    cache: cache_ref,
                })
            }));
            flag.store(false, std::sync::atomic::Ordering::SeqCst);
            match result {
                Ok(p) => {
                    eprintln!(
                        "[TIMING] DF-full pipeline compiled (on-demand bg): {:?}",
                        t.elapsed()
                    );
                    let _ = tx.send(p);
                    if let Some(ref c) = *cache {
                        save_pipeline_cache(c);
                    }
                }
                Err(e) => {
                    let msg = e
                        .downcast_ref::<String>()
                        .map(|s| s.as_str())
                        .or_else(|| e.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown error");
                    eprintln!("[SHADER] DF-full on-demand bg compile panicked: {}", msg);
                }
            }
        });
    if let Err(e) = spawn {
        eprintln!("[SHADER] failed to spawn DF-full bg compile: {e}");
        gpu.pipelines.df_full_compile_started = false;
        gpu.pipelines.df_full_compile_rx = None;
    }
}


/// Legacy name — **does not block**. Spawns background compile if needed; use lite until ready.
/// Prefer calling [`request_deferred_full_pipeline`] explicitly from non-render code.
pub(crate) fn ensure_deferred_full_pipeline(gpu: &mut GpuCtx) {
    request_deferred_full_pipeline(gpu);
}


/// Section O: variance-guided denoise quality from residual noisy fraction + mode.
/// - `noisy_frac` ∈ [0,1] from adaptive-stop readback (1 = all pixels noisy).
/// - `half_res`: interactive path → leaner filter (budget).
/// - `accum_frames`: progressive history length (minimum confidence for residual gating).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PtDenoiseQuality {
    pub strength: f32,
    /// Number of à-trous steps to run (from STEPS_TABLE).
    pub step_count: u8,
    /// True → skip denoise (image already clean enough).
    pub skip: bool,
}


/// C2: further lean denoise step count when last PT frame overran wall budget.
pub(crate) fn apply_pt_denoise_frame_budget(
    mut q: PtDenoiseQuality,
    last_frame_ms: f32,
    fast: bool,
) -> PtDenoiseQuality {
    if q.skip || q.step_count == 0 {
        return q;
    }
    let scale =
        crate::viewport_render_mode::pt_denoise_budget_scale_with(last_frame_ms, fast);
    if scale >= 0.99 {
        return q;
    }
    let steps = ((q.step_count as f32) * scale).round().max(1.0) as u8;
    q.step_count = steps.min(4);
    if scale < 0.4 {
        q.step_count = 1;
    }
    q
}


/// Resolve denoise budget from residual + interaction mode (Section O).
pub(crate) fn resolve_pt_denoise_quality(
    user_strength: f32,
    noisy_frac: f32,
    half_res: bool,
    accum_frames: u32,
) -> PtDenoiseQuality {
    let s = user_strength.clamp(0.0, 1.0);
    if s <= 1e-4 {
        return PtDenoiseQuality { strength: 0.0, step_count: 0, skip: true };
    }
    let nf = noisy_frac.clamp(0.0, 1.0);

    // Interactive half-res: always lean (1–2 passes) for latency.
    if half_res {
        let steps = if nf > 0.35 || s > 0.7 { 2 } else { 1 };
        return PtDenoiseQuality {
            strength: (s * 1.1).min(1.0), // slightly stronger for noisy 1spp display
            step_count: steps,
            skip: false,
        };
    }

    // Nearly converged via residual: skip denoise so progressive detail is stable.
    if accum_frames >= 32 && nf < 0.06 {
        return PtDenoiseQuality {
            strength: s * 0.15,
            step_count: 1,
            skip: nf == 0.0,
        };
    }
    if accum_frames >= 16 && nf < 0.12 {
        return PtDenoiseQuality {
            strength: s * 0.4,
            step_count: 2,
            skip: false,
        };
    }

    // Full progressive early: scale passes with residual noise.
    let (strength, step_count) = if nf > 0.45 {
        (s, 4u8) // 1,2,4,8
    } else if nf > 0.2 {
        (s * 0.95, 3u8) // 1,2,4
    } else if nf > 0.08 {
        (s * 0.75, 2u8) // 1,2
    } else {
        (s * 0.5, 1u8) // 1 only
    };
    PtDenoiseQuality {
        strength,
        step_count,
        skip: false,
    }
}

#[cfg(test)]
mod denoise_quality_tests {
    use super::resolve_pt_denoise_quality;

    #[test]
    fn noisy_history_does_not_expire() {
        for frames in [96, 512, 4096] {
            let quality = resolve_pt_denoise_quality(1.0, 0.8, false, frames);
            assert!(!quality.skip);
            assert_eq!(quality.step_count, 4);
            assert_eq!(quality.strength, 1.0);
        }
        assert!(!resolve_pt_denoise_quality(1.0, 0.01, false, 4096).skip);
        assert!(resolve_pt_denoise_quality(1.0, 0.0, false, 4096).skip);
    }

    #[test]
    fn interaction_keeps_lean_denoise_budget() {
        for frames in [1, 96, 4096] {
            let quality = resolve_pt_denoise_quality(1.0, 0.8, true, frames);
            assert!(!quality.skip);
            assert_eq!(quality.step_count, 2);
        }
        assert!(resolve_pt_denoise_quality(0.0, 1.0, true, 1).skip);
    }
}


/// Section M+O + A3: multi-pass à-trous with **cached** bind groups.
/// Textures are taken from `gpu` (avoids &Texture + &mut GpuCtx borrow clash).
/// Half: half_accum → half_hdr, temp = half_denoise_temp (RGBA16F).
/// Full: pt_accum → hdr, temp = pt_denoise_temp.
///
/// `end_on_hdr`: when true and step_count is even, pass0 writes accum→temp and
/// subsequent passes are inverted so the final filtered image lands in hdr
/// without a temp→hdr blit (toggle E). When false, classic topology + blit.
pub(crate) fn dispatch_pt_denoise_atrous(
    enc: &mut wgpu::CommandEncoder,
    queue: &wgpu::Queue,
    gpu: &mut GpuCtx,
    quality: PtDenoiseQuality,
    width: u32,
    height: u32,
    gbuf_scale: u32,
    half_res: bool,
    end_on_hdr: bool,
) {
    if quality.skip || quality.step_count == 0 || quality.strength <= 1e-4 {
        return;
    }
    if gpu.pipelines.pt_denoise_pipeline.is_none()
        || gpu.bind_groups.pt_denoise_bgl.is_none()
        || gpu.buffers.pt_denoise_params_buffer.is_none()
        || gpu.gbuf_normal.is_none()
        || gpu.gbuf_albedo.is_none()
    {
        return;
    }
    if gpu.buffers.pt_denoise_pass_params_buffer.is_none() {
        gpu.buffers.pt_denoise_pass_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pt_denoise_pass_params"),
            size: 128,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    let moments = if half_res {
        gpu.buffers.pt_half_moments_buffer.as_ref()
    } else {
        gpu.buffers.pt_moments_buffer.as_ref()
    };
    let Some(moments) = moments else { return };
    if half_res {
        if gpu.pt_half_accum.is_none()
            || gpu.pt_half_hdr.is_none()
            || gpu.pt_half_denoise_temp.is_none()
        {
            return;
        }
    } else if gpu.pt_accum_texture.is_none()
        || gpu.hdr_texture.is_none()
        || gpu.pt_denoise_temp.is_none()
    {
        return;
    }

    const ALL_STEPS: [u32; 4] = [1, 2, 4, 8];
    let n = (quality.step_count as usize).clamp(1, 4);
    let steps = &ALL_STEPS[..n];
    // Even-step inverted topology: pass0 accum→temp, then pong/ping so last write is hdr.
    let invert_even = end_on_hdr && (n % 2 == 0);

    // A3: ensure pass topologies exist (disjoint field borrows only).
    {
        let bgl = gpu.bind_groups.pt_denoise_bgl.as_ref().unwrap();
        let params_buf = gpu.buffers.pt_denoise_params_buffer.as_ref().unwrap();
        let gn = gpu.gbuf_normal.as_ref().unwrap();
        let alb = gpu.gbuf_albedo.as_ref().unwrap();
        let device = &gpu.device;
        if half_res {
            let src = gpu.pt_half_accum.as_ref().unwrap();
            let dst = gpu.pt_half_hdr.as_ref().unwrap();
            let temp = gpu.pt_half_denoise_temp.as_ref().unwrap();
            if gpu.bind_groups.pt_denoise_bg_half.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_half_p0", src, dst, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_half = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_half_p0_temp.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_half_p0_temp", src, temp, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_half_p0_temp = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_half_ping.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_half_ping", dst, temp, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_half_ping = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_half_pong.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_half_pong", temp, dst, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_half_pong = Some(bg);
            }
        } else {
            let src = gpu.pt_accum_texture.as_ref().unwrap();
            let dst = gpu.hdr_texture.as_ref().unwrap();
            let temp = gpu.pt_denoise_temp.as_ref().unwrap();
            if gpu.bind_groups.pt_denoise_bg.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_p0", src, dst, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_p0_temp.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_p0_temp", src, temp, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_p0_temp = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_ping.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_ping", dst, temp, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_ping = Some(bg);
            }
            if gpu.bind_groups.pt_denoise_bg_pong.is_none() {
                let bg = make_pt_denoise_bg(
                    device, bgl, "pt_denoise_pong", temp, dst, gn, alb, params_buf, moments,
                );
                gpu.bind_groups.pt_denoise_bg_pong = Some(bg);
            }
        }
    }

    let (Some(ref pl), Some(ref params_buf)) = (
        &gpu.pipelines.pt_denoise_pipeline,
        &gpu.buffers.pt_denoise_params_buffer,
    ) else {
        return;
    };

    let strength = quality.strength;
    let params_data: [[u32; 8]; 4] = std::array::from_fn(|pass_idx| {
        [
            strength.to_bits(),
            1,
            width,
            height,
            gbuf_scale,
            ALL_STEPS[pass_idx],
            pass_idx as u32,
            0,
        ]
    });
    let pass_params = gpu.buffers.pt_denoise_pass_params_buffer.as_ref().unwrap();
    queue.write_buffer(pass_params, 0, bytemuck::cast_slice(&params_data));
    for (pass_idx, _) in steps.iter().enumerate() {
        enc.copy_buffer_to_buffer(pass_params, pass_idx as u64 * 32, params_buf, 0, 32);

        // Classic: p0=accum→hdr, odd=ping(hdr→temp), even=pong(temp→hdr).
        // Invert even: p0=accum→temp, odd=pong(temp→hdr), even=ping(hdr→temp) → ends on hdr.
        let bg = if half_res {
            if pass_idx == 0 {
                if invert_even {
                    gpu.bind_groups.pt_denoise_bg_half_p0_temp.as_ref()
                } else {
                    gpu.bind_groups.pt_denoise_bg_half.as_ref()
                }
            } else if invert_even {
                if pass_idx % 2 == 1 {
                    gpu.bind_groups.pt_denoise_bg_half_pong.as_ref()
                } else {
                    gpu.bind_groups.pt_denoise_bg_half_ping.as_ref()
                }
            } else if pass_idx % 2 == 1 {
                gpu.bind_groups.pt_denoise_bg_half_ping.as_ref()
            } else {
                gpu.bind_groups.pt_denoise_bg_half_pong.as_ref()
            }
        } else if pass_idx == 0 {
            if invert_even {
                gpu.bind_groups.pt_denoise_bg_p0_temp.as_ref()
            } else {
                gpu.bind_groups.pt_denoise_bg.as_ref()
            }
        } else if invert_even {
            if pass_idx % 2 == 1 {
                gpu.bind_groups.pt_denoise_bg_pong.as_ref()
            } else {
                gpu.bind_groups.pt_denoise_bg_ping.as_ref()
            }
        } else if pass_idx % 2 == 1 {
            gpu.bind_groups.pt_denoise_bg_ping.as_ref()
        } else {
            gpu.bind_groups.pt_denoise_bg_pong.as_ref()
        };
        let Some(bg) = bg else { return };

        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("pt_denoise_atrous"),
                timestamp_writes: None,
            });
            cp.set_pipeline(pl);
            cp.set_bind_group(0, bg, &[]);
            cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
        }
    }

    // Classic even-step only: result in temp → blit to hdr (needs COPY_DST).
    // end_on_hdr inverted topology already finished in hdr — skip blit.
    if !invert_even && steps.len() % 2 == 0 {
        if half_res {
            if let (Some(ref temp), Some(ref dst)) =
                (&gpu.pt_half_denoise_temp, &gpu.pt_half_hdr)
            {
                enc.copy_texture_to_texture(
                    temp.as_image_copy(),
                    dst.as_image_copy(),
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        } else if let (Some(ref temp), Some(ref dst)) =
            (&gpu.pt_denoise_temp, &gpu.hdr_texture)
        {
            enc.copy_texture_to_texture(
                temp.as_image_copy(),
                dst.as_image_copy(),
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}


/// STAGE-3 GPU FINALIZE — compile the four `classify_finalize_*` entry points from an
/// already-built volume_bake module + bake pipeline layout. ONE implementation shared by the
/// background compile thread (`app_update`), the lazy `ensure_classify_bricks_pipeline`
/// fallback and the hardware test harnesses (searched for existing: none — the other volume_bake
/// entries are single-pipeline compiles inline at each site).
pub(crate) fn compile_classify_finalize_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    cache: Option<&wgpu::PipelineCache>,
) -> crate::gpu_ctx::ClassifyFinalizePipelines {
    let mk = |entry: &str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(layout),
        module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache,
    });
    crate::gpu_ctx::ClassifyFinalizePipelines {
        dilate: mk("classify_finalize_dilate"),
        scan: mk("classify_finalize_scan"),
        block_scan: mk("classify_finalize_block_scan"),
        emit: mk("classify_finalize_emit"),
    }
}


/// Manifest of all background-compiled pipelines and their warm/cold status.
/// "Required" = needed before interaction (core SDF + essential post-processing).
/// "Optional" = nice-to-have (path tracing, denoise, brick fill).
pub(crate) struct WarmupManifest;

impl WarmupManifest {
    /// Names of PRODUCTION pipelines needed before the app is fully interactive in the default volume/
    /// raster workflow. Tier 1: the analytical RM/RM-march/DF-full/Fog are NOT here — they compile lazily.
    pub const REQUIRED: &'static [&'static str] = &[
        "RaymarchLite", "DeferredLite",
        "Fxaa", "Temporal", "PrimaryBake",
    ];

    /// Names of lazily-compiled pipelines (analytical / opt-in features; not required for interaction).
    pub const OPTIONAL: &'static [&'static str] = &[
        "DeferredFull", "Fog",
        "Pathtrace", "PtDenoise", "BrickFill", "Ssao",
    ];

    /// True when all PRODUCTION pipelines have been compiled (the analytical set is lazy — not required).
    pub fn is_fully_warm(r: &crate::PipelineRegistry) -> bool {
        r.pipeline_lite.is_some()
            && r.deferred_lite_pipeline.is_some()
            && r.fxaa_pipeline.is_some()
            && r.temporal_reproject_pipeline.is_some()
            && r.primary_bake_pipeline.is_some()
    }

    /// True when ALL pipelines (required + optional) are compiled.
    pub fn is_all_warm(r: &crate::PipelineRegistry) -> bool {
        Self::is_fully_warm(r)
            && r.pt_tri_pipeline.is_some()
            && r.pt_denoise_pipeline.is_some()
            && r.brick_fill_pipeline.is_some()
            && r.ssao.is_some()
    }

    /// Count of warm pipelines out of all 11.
    pub fn warm_count(r: &crate::PipelineRegistry) -> usize {
        [
            r.pipeline_lite.is_some(),
            r.deferred_lite_pipeline.is_some(),
            r.deferred_pipeline.is_some(),
            r.fog_pipeline.is_some(),
            r.fxaa_pipeline.is_some(),
            r.temporal_reproject_pipeline.is_some(),
            r.primary_bake_pipeline.is_some(),
            r.pt_tri_pipeline.is_some(),
            r.pt_denoise_pipeline.is_some(),
            r.brick_fill_pipeline.is_some(),
            r.ssao.is_some(),
        ].iter().filter(|&&b| b).count()
    }
}
