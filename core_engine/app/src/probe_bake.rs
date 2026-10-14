// probe_bake.rs — Auto Reflection Probe (PROGRESSIVE_PBR_PLAN.md Phase 3)
//
// One scene-centered cubemap, ZERO authoring (owner: "I never want to add reflection
// probes by hand for now"). While the progressive-refine accumulator is idle-averaging,
// each frame captures ONE cube face by running the REAL raster + deferred pipeline with
// a probe camera (own submits), resolving the HDR's central 90° square into the face,
// and box-reducing the mip chain. The ordinary frame then re-renders everything with
// the true camera — the G-buffer/HDR are transient scratch between sequential submits,
// never aliased within one submission (Display Invariant untouched: same technique,
// same targets, sequential).
//
// Consumption lives in pass_ssr (post_process.wgsl bindings 65/66/67): the probe is
// the (1 − confidence) fallback where the screen-space march can't see, parallax-
// corrected against the scene-AABB proxy volume.
//
// Deliberate v1 limits (documented in the plan): probe faces reuse the main view's
// SSAO texture (view-dependent, slightly wrong in reflections — subtle at probe mip
// scale); the mip chain is a per-face box reduce, not a GGX convolution; re-bakes
// serve the previous probe while the 6-frame round replaces faces in place.
// Probe mini-frames do NOT run raymarch-lite floor/bg (raster+deferred only) — floor
// reflections in faces come from rasterized geometry or the env sky fill in deferred.

use crate::*;
use eframe::wgpu;
use glam::{Mat3, Quat, Vec3};

/// Selectable cube face edges in texels ("Reflection Probe Res"). VRAM is
/// 6 faces x N^2 x RGBA16F x ~1.33 for mips = 1 MB / 4 MB / 17 MB.
pub(crate) const PROBE_FACE_SIZES: [u32; 3] = [128, 256, 512];
/// Fallback face edge when the setting is unset/invalid.
pub(crate) const PROBE_FACE_SIZE_DEFAULT: u32 = 256;

/// Full mip chain for a face edge: N -> 1, i.e. `log2(N) + 1` levels.
///
/// Was a hardcoded 8 (correct only for 128). Deriving it is what lets the face size
/// be a runtime setting without a second constant to keep in sync.
pub(crate) fn probe_mips(face_size: u32) -> u32 {
    face_size.max(1).ilog2() + 1
}

/// Highest mip index the shader may sample: the 8×8-texel level (`log2(N) − 3`).
/// Was `mips − 2` (the 2×2 level) — but the mip chain is PER-FACE box reduce with
/// no cross-face filtering, so the last levels converge to each face's own
/// average, and cube-sampling them paints FACE-SEAM bands across rough surfaces
/// (owner's wall-smudge screenshot 2026-08-07: warm band = ceiling face average,
/// blue band = floor face average, flare = 3-face corner). The 8×8 level keeps
/// enough angular structure that seams stay below visibility while still giving
/// rough reflections a heavy blur.
pub(crate) fn probe_max_sample_mip(face_size: u32) -> f32 {
    probe_mips(face_size).saturating_sub(4) as f32
}

/// Probe VALIDITY rule (AUTO-PROBE-INSIDE-GEOMETRY, 2026-10-13). The capture measures,
/// per cube texel, the fraction of directions that are FREE — they hit nothing or hit
/// a surface from its FRONT (probe.wgsl `probe_capture`, facing marked by gbuffer.wgsl
/// in probe frames); `probe_sh_project` integrates it over the sphere. A round is
/// published only when the eye is in free space along at least this fraction of
/// directions; otherwise the probe FAILS CLOSED — reflections and diffuse ambient stay
/// on the HDRI IBL, exactly as with the probe off.
///
/// Why facing and not a point-in-mesh test: a closed ROOM also encloses the eye, but
/// its walls face INTO the room (front faces, free ≈ 1), while the inside of a lone
/// solid shows only back faces (free ≈ 0) — the auto placement (AABB centre) lands
/// there for any single convex object, whose interior the old probe then served as
/// the "environment" (chrome read as flat grey). Representation-agnostic: everything
/// the probe sees went through the one raster pass. Precedent: Unity APV's back-face
/// validity threshold. Limitation (documented): a room modelled as an OUTWARD-wound
/// closed box is, by orientation, a solid block — it fails closed to the HDRI.
/// 0.5 = "inside material along more directions than in free space"; the two real
/// cases sit at the extremes, so the verdict is not sensitive to the exact value.
pub(crate) const PROBE_MIN_FREE_FRACTION: f32 = 0.5;

/// The publish rule. FAILS CLOSED: an unmeasured (`NaN` — failed readback) or corrupt
/// (out of [0,1], non-finite) fraction is never a valid probe.
pub(crate) fn probe_capture_is_valid(free_fraction: f32) -> bool {
    free_fraction.is_finite()
        && free_fraction <= 1.0 + 1e-4
        && free_fraction >= PROBE_MIN_FREE_FRACTION
}

/// Byte offset of `probe_sh[9]` (= strength, free fraction, 0, valid) in the SH buffer.
const PROBE_SH_META_OFFSET: u64 = 9 * 16;

/// Must match `ProbeCaptureParams` in graphics/src/probe.wgsl (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProbeCaptureParams {
    fwd: [f32; 4],   // xyz = camera forward, w = face index
    right: [f32; 4], // xyz = camera right,   w = src_min.x
    up: [f32; 4],    // xyz = camera up,      w = src_min.y
    sizes: [f32; 4], // x = central-square side, y = face size, z = tan(half fov) of square
}

/// Must match `ProbeParams` in graphics/src/post_process.wgsl (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProbeParams {
    center_enabled: [f32; 4],
    extent_mips: [f32; 4],
}

/// Capture camera basis per cube face: (forward, up); right = fwd × up. Any
/// right-handed basis works — probe_capture re-projects each texel's SPEC cube
/// direction through this exact basis, so capture and hardware cube sampling
/// cannot disagree (the classic cubemap-flip trap is structurally avoided).
const FACES: [([f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),  // +X
    ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), // -X
    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),  // +Y
    ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]), // -Y
    ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),  // +Z
    ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]), // -Z
];

impl AzraApp {
    /// Called from `dispatch_gpu_render` (Hybrid arm) immediately BEFORE the main
    /// raster pass. Captures at most one face per call; no-ops entirely unless the
    /// refine accumulator is active with everything resident.
    pub(crate) fn probe_bake_face(&mut self, width: u32, height: u32) {
        let enabled = self.render.pbr_progressive_enabled && self.render.auto_reflection_probe;
        let refine_sig = self.render.pbr_refine_sig;
        // Warm-up: give the accumulator a few frames before spending idle frames on
        // probe faces (the first refined frames carry the biggest visible AA gains).
        let refining = self.render.pbr_refine_active && self.render.pbr_refine_frames >= 4;

        // The previous round's validity verdict (async readback) publishes or rejects it.
        self.probe_collect_verdict(enabled);

        // ── Phase A: gate + schedule (scoped gpu borrow) ──
        let (face, center) = {
            let Some(gpu) = self.gpu.as_mut() else { return };
            if !enabled {
                // A verdict still in flight belongs to a probe that is now off
                // (probe_collect_verdict also drops it on `!enabled`).
                gpu.probe_rejected = false;
                // Toggle-off: stop serving the probe once (SSR ignores it again).
                // `probe_baked_sig != 0` also covers a REJECTED or in-flight round:
                // without the reset, re-enabling with an unchanged scene would never
                // start a new round (same sig) and the probe would stay off for good.
                if gpu.probe_baked || gpu.probe_baked_sig != 0 {
                    gpu.probe_round_gen = gpu.probe_round_gen.wrapping_add(1);
                    gpu.probe_baked = false;
                    gpu.probe_next_face = 6;
                    gpu.probe_baked_sig = 0;
                    if let Some(buf) = &gpu.probe_params_buffer {
                        let off = ProbeParams { center_enabled: [0.0; 4], extent_mips: [0.0; 4] };
                        gpu.queue.write_buffer(buf, 0, bytemuck::bytes_of(&off));
                    }
                    // SH valid off too: re-enabling must not light from a stale round
                    // before the next one is judged.
                    if let Some(shb) = &gpu.probe_sh_buffer {
                        gpu.queue.write_buffer(shb, PROBE_SH_META_OFFSET + 12, bytemuck::bytes_of(&0.0f32));
                    }
                }
                self.render.pbr_probe_ready = false;
                return;
            }
            if !refining {
                return;
            }
            if gpu.pipelines.deferred_pipeline.is_none()
                || gpu.bind_groups.deferred_bg.is_none()
                || gpu.hdr_texture.is_none()
                || gpu.probe_cube.is_none()
                || gpu.probe_params_buffer.is_none()
                || gpu.last_gu.is_none()
            {
                return;
            }
            if gpu.probe_next_face >= 6 {
                // Round complete — start a fresh one only when the scene/quality
                // signature moved. D2: refine sig tracks lights/content/env/probe
                // toggle+res (NOT pbr_probe_ready — hashing ready forced a redundant
                // second round the moment round 1 published). Comment was wrong that
                // "sig tracks viewport size"; viewport is independent of the cube.
                if gpu.probe_baked_sig == refine_sig {
                    return;
                }
                // One readback staging buffer: the next round may only finish into it
                // once the previous round's verdict is collected (a round is ≥6
                // frames, the readback ~1–2, so this almost never waits).
                if gpu.probe_verdict_rx.is_some() {
                    return;
                }
                // Probe placement: union of the DISPLAYED instance AABBs (the raster
                // draw list — poly, voxel, and SDF-as-mesh alike). The first version
                // read `gu.scene_aabb_*`, which is computed from the SDF PRIM buffer
                // only — a pure-poly room has zero SDF prims, the bounds came back
                // degenerate, and the guard skipped the bake EVERY frame (the owner's
                // "chrome sphere never reflects" 2026-08-07). The analytical floor is
                // deliberately not an instance, so it never inflates the proxy.
                let mut lo = glam::Vec3::splat(f32::INFINITY);
                let mut hi = glam::Vec3::splat(f32::NEG_INFINITY);
                for (a, b) in &self.structural_world_aabb {
                    // Skipped/special/not-yet-cached instances are stored as a
                    // degenerate ZERO AABB (raster_gbuffer) — unioning one drags
                    // the probe toward the world origin. Same skip as the
                    // shadow_far fit loop.
                    if a == b {
                        continue;
                    }
                    lo = lo.min(*a);
                    hi = hi.max(*b);
                }
                if !(hi.x > lo.x && hi.y > lo.y && hi.z > lo.z) {
                    // Fall back to the SDF-prim bounds (pre-raster SDF-only edge case).
                    let g = gpu.last_gu.as_ref().unwrap();
                    lo = glam::Vec3::from_slice(&g.scene_aabb_min[..3]);
                    hi = glam::Vec3::from_slice(&g.scene_aabb_max[..3]);
                    if !(hi.x > lo.x && hi.y > lo.y && hi.z > lo.z) {
                        return; // genuinely empty scene — nothing to reflect
                    }
                }
                gpu.probe_center = [
                    (lo.x + hi.x) * 0.5,
                    (lo.y + hi.y) * 0.5,
                    (lo.z + hi.z) * 0.5,
                ];
                // Proxy AABB: padded so the parallax box sits just outside the
                // geometry (floors/walls land ON the proxy → correct correction).
                gpu.probe_extent = [
                    ((hi.x - lo.x) * 0.5).max(0.25) * 1.25,
                    ((hi.y - lo.y) * 0.5).max(0.25) * 1.25,
                    ((hi.z - lo.z) * 0.5).max(0.25) * 1.25,
                ];
                gpu.probe_next_face = 0;
                gpu.probe_baked_sig = refine_sig;
                gpu.probe_round_gen = gpu.probe_round_gen.wrapping_add(1);
            }
            (gpu.probe_next_face as usize, Vec3::from(gpu.probe_center))
        };

        self.ensure_probe_pipelines();

        // B1: mini-frame side = min(2 × face, min(w,h)). Raster + deferred only
        // cover this square (was full viewport → ≤512² capture, ~56× overshade).
        let face_size_live = self
            .gpu
            .as_ref()
            .map(|g| g.probe_face_size.max(1))
            .unwrap_or(PROBE_FACE_SIZE_DEFAULT);
        let mini_s = (2 * face_size_live).min(width).min(height).max(1);

        // ── Phase B: camera swap + probe raster (own submit inside) ──
        let saved_orbit = self.viewport.camera.orbit;
        let saved_dist = self.viewport.camera.distance;
        let saved_target = self.viewport.camera.target;
        let saved_fov = self.render.fov_deg;
        let saved_ortho = self.render.ortho;
        let saved_jitter = self.render.pbr_refine_jitter_ndc;
        let saved_active = self.render.pbr_refine_active;

        let fwd = Vec3::from(FACES[face].0);
        let up = Vec3::from(FACES[face].1);
        let right = fwd.cross(up).normalize();
        // orbit maps -Z → fwd and Y → up (the from_orbit convention).
        let orbit = Quat::from_mat3(&Mat3::from_cols(right, up, -fwd));
        let dist = 0.05f32;
        self.viewport.camera.orbit = orbit;
        self.viewport.camera.distance = dist;
        // position = target − fwd·dist ⇒ target = center + fwd·dist puts the eye
        // exactly at the probe center.
        self.viewport.camera.target = center + fwd * dist;
        // Square mini-frame: VFOV = HFOV = 90° (fov_scale 1.0 for deferred rays).
        let fov_scale = 1.0f32;
        self.render.fov_deg = 90.0;
        self.render.ortho = false;
        // Probe frames are single samples: no sub-pixel jitter, no sun-cone jitter
        // (pbr_refine_active gates both in raster_gbuffer).
        self.render.pbr_refine_jitter_ndc = [0.0; 2];
        self.render.pbr_refine_active = false;
        // B1/B2 gate for raster_gbuffer (viewport/scissor, frustum-only, no latch).
        self.probe_frame_side = mini_s;

        // Timestamp queries: the main raster writes the same query slots later this
        // frame — stash the query set so the probe passes leave them untouched.
        let ts_stash = self.gpu.as_mut().and_then(|g| g.gpu_ts.take());
        self.dispatch_raster_pass(width, height);
        if let Some(g) = self.gpu.as_mut() {
            g.gpu_ts = ts_stash;
        }

        // Restore the real camera before anything else reads it.
        self.viewport.camera.orbit = saved_orbit;
        self.viewport.camera.distance = saved_dist;
        self.viewport.camera.target = saved_target;
        self.render.fov_deg = saved_fov;
        self.render.ortho = saved_ortho;
        self.render.pbr_refine_jitter_ndc = saved_jitter;
        self.render.pbr_refine_active = saved_active;
        self.probe_frame_side = 0;

        // ── Phase C: deferred shade + capture + mips (own encoder + submit) ──
        let Some(gpu) = self.gpu.as_mut() else { return };
        // Face edge the LIVE cube was created at — never the setting, which may have
        // changed since; poll recreates the texture before this runs again.
        if gpu.probe_cube.is_none()
            || gpu.hdr_texture.is_none()
            || gpu.probe_capture_buffer.is_none()
            || gpu.probe_capture_pipeline.is_none()
            || gpu.probe_capture_bgl.is_none()
            || gpu.probe_mip_pipeline.is_none()
            || gpu.probe_mip_bgl.is_none()
            || gpu.last_gu.is_none()
        {
            return;
        }
        let face_size = gpu.probe_face_size.max(1);
        let mips = probe_mips(face_size);
        let hdr_size = {
            let hdr = gpu.hdr_texture.as_ref().unwrap();
            (hdr.size().width, hdr.size().height)
        };
        let s = mini_s.min(hdr_size.0).min(hdr_size.1).max(1);

        // Probe GpuUniforms: last frame's config with the camera patched — everything
        // else (lights, env, materials) is exactly the scene state.
        let mut pgu = *gpu.last_gu.as_ref().unwrap();
        pgu.cam_pos = [center.x, center.y, center.z, 0.0]; // w=0: perspective
        pgu.cam_forward = [fwd.x, fwd.y, fwd.z, fov_scale];
        // cam_right.w = mini side (B1): deferred reconstructs UV with s, not full res.
        pgu.cam_right = [right.x, right.y, right.z, s as f32];
        pgu.cam_up = [up.x, up.y, up.z, 1.0]; // w = march_pixel_scale slot (unused in probe)
        pgu.pbr_jitter = [0.0; 2];
        // Shadows in probe faces: CSM (raster pixels) + point/spot SDF soft shadows
        // stay LIVE (interaction_no_shadows = 0 — the blunt flag killed them, the
        // 2026-08-07 shadow regression). The analytical SUN recompute no longer
        // exists (removed 2026-08-07, owner decision), so no suppression is needed.
        pgu.interaction_no_shadows = 0;
        // RT point/spot shadows read a SCREEN-SPACE visibility buffer from the main
        // view — meaningless at probe pixels. Off: point lights fall back to SDF
        // soft shadows in reflections; sun CSM is fully correct.
        pgu.rt_shadows_on = 0;
        // Never accumulate / RT-sun / draw preview balls in a probe frame (the
        // balls would capture themselves into the cube).
        // Bit 4096 (hidden HDRI background) is cleared for the same reason the probe
        // is baked at all: a probe IS the reflected environment. Hiding the plate is
        // camera-ray visibility only — leaving the bit set would bake the flat
        // viewport colour into every reflection and silently break the one thing the
        // toggle promises ("dark flat bg, metal still reflects the HDRI").
        pgu.post_flags &= !(256 | 512 | 2048 | 4096);
        // Selection rims are UI, not scene radiance — without this the selected
        // object bakes its green outline into every reflection of it (and every
        // selection change re-rounds the probe via prev_sel_hash, re-capturing
        // the new rim).
        pgu.sel_rim_enabled = 0;
        gpu.queue.write_buffer(&gpu.buffers.uniform_buffer, 0, pgu.as_bytes());

        // Capture reads top-left s×s (matches raster viewport + deferred UV).
        let cap = ProbeCaptureParams {
            fwd: [fwd.x, fwd.y, fwd.z, face as f32],
            right: [right.x, right.y, right.z, 0.0],
            up: [up.x, up.y, up.z, 0.0],
            // tan(half-fov) across the square is 1.0 by construction (90°).
            sizes: [s as f32, face_size as f32, 1.0, 0.0],
        };
        if let Some(cap_buf) = gpu.probe_capture_buffer.as_ref() {
            gpu.queue.write_buffer(cap_buf, 0, bytemuck::bytes_of(&cap));
        }

        // B3: ensure face views + capture BGs (keyed on cube gen + HDR size).
        Self::ensure_probe_view_cache(gpu, face_size, mips, hdr_size);
        if gpu.probe_capture_bgs.is_none() {
            return;
        }

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("probe_bake") });
        // Deferred shade only the mini-rect (B1) — same bind group as main frame.
        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("probe_deferred"),
                timestamp_writes: None,
            });
            cp.set_pipeline(gpu.pipelines.deferred_pipeline.as_ref().unwrap());
            cp.set_bind_group(0, gpu.bind_groups.deferred_bg.as_ref().unwrap(), &[]);
            cp.dispatch_workgroups((s + 7) / 8, (s + 7) / 8, 1);
        }
        // Resolve the top-left 90° square into the cube face (mip 0).
        {
            let bg = &gpu.probe_capture_bgs.as_ref().unwrap()[face];
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("probe_capture"),
                timestamp_writes: None,
            });
            cp.set_pipeline(gpu.probe_capture_pipeline.as_ref().unwrap());
            cp.set_bind_group(0, bg, &[]);
            let g = (face_size + 7) / 8;
            cp.dispatch_workgroups(g, g, 1);
        }
        // Mips are NOT built per face any more: the cross-face reduce needs all
        // six mip-0 faces present, so the whole chain builds once per completed
        // round in Phase D (cheaper too — one mip pass per round, not six).
        gpu.queue.submit(Some(enc.finish()));

        // ── Phase D: advance the round; on completion build mips + SH (+ the
        // free-fraction measurement) and read the verdict back. Publishing (params,
        // SH valid, ready flags) happens in `probe_collect_verdict`, never here: an
        // unjudged probe must not light anything (fail closed).
        gpu.probe_next_face += 1;
        if gpu.probe_next_face >= 6 {
            // ── Round finishers, one encoder: cross-face MIP CHAIN, then the SH9
            // light-probe projection (Phase 3b). Both need all six mip-0 faces
            // present, which is exactly what round completion guarantees.
            // B3: reuse cached mip/SH views + bind groups (0 create per round after warm).
            let mut finished = false;
            if let (Some(sh_pl), Some(mip_pl_ref), Some(sh_buf), Some(staging)) = (
                gpu.probe_sh_pipeline.as_ref(),
                gpu.probe_mip_pipeline.as_ref(),
                gpu.probe_sh_buffer.as_ref(),
                gpu.probe_verdict_staging.as_ref(),
            ) {
                if !gpu.probe_mip_bgs.is_empty() && gpu.probe_sh_bg.is_some() {
                    let mut sh_enc = gpu.device.create_command_encoder(
                        &wgpu::CommandEncoderDescriptor { label: Some("probe_round_finish") },
                    );
                    for (i, bg) in gpu.probe_mip_bgs.iter().enumerate() {
                        let mip = (i as u32) + 1;
                        let mut cp = sh_enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("probe_mip"),
                            timestamp_writes: None,
                        });
                        cp.set_pipeline(mip_pl_ref);
                        cp.set_bind_group(0, bg, &[]);
                        let sz = (face_size >> mip).max(1);
                        cp.dispatch_workgroups((sz + 7) / 8, (sz + 7) / 8, 6);
                    }
                    {
                        let mut cp = sh_enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("probe_sh_project"),
                            timestamp_writes: None,
                        });
                        cp.set_pipeline(sh_pl);
                        cp.set_bind_group(0, gpu.probe_sh_bg.as_ref().unwrap(), &[]);
                        cp.dispatch_workgroups(1, 1, 1);
                    }
                    // The measured free fraction rides probe_sh[9].y → 16-byte readback.
                    sh_enc.copy_buffer_to_buffer(sh_buf, PROBE_SH_META_OFFSET, staging, 0, 16);
                    gpu.queue.submit(Some(sh_enc.finish()));
                    // GPU Safety rule: the map result is captured, never discarded.
                    let (tx, rx) = std::sync::mpsc::channel();
                    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                        let _ = tx.send(r);
                    });
                    gpu.probe_verdict_rx = Some(rx);
                    gpu.probe_verdict_round = gpu.probe_round_gen;
                    finished = true;
                }
            }
            if !finished {
                // No measurement → no verdict → nothing would ever publish this round.
                // Forget its signature so the next idle frame bakes a fresh one.
                gpu.probe_baked_sig = 0;
            }
        }
    }

    /// Collect the async validity readback of the last completed round and either
    /// PUBLISH the probe (reflection params, SH valid, ready flags) or REJECT it —
    /// fail closed to the HDRI IBL (see `PROBE_MIN_FREE_FRACTION`). Non-blocking;
    /// `device.poll` runs once per frame in the poll phase.
    fn probe_collect_verdict(&mut self, enabled: bool) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let Some(rx) = gpu.probe_verdict_rx.as_ref() else { return };
        let mapped = match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Ok(r) => r.is_ok(),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        };
        gpu.probe_verdict_rx = None;
        let meta: Option<[f32; 4]> = if mapped {
            let staging = gpu.probe_verdict_staging.as_ref().unwrap();
            let v = {
                let view = staging.slice(..).get_mapped_range();
                *bytemuck::from_bytes::<[f32; 4]>(&view[..16])
            };
            staging.unmap();
            Some(v)
        } else {
            // Map failed (device loss / aborted): the staging buffer's state is unknown —
            // drop it (recreated lazily) and treat the round as unjudged → not published.
            eprintln!("[PROBE] validity readback FAILED — probe not published");
            gpu.probe_verdict_staging = None;
            None
        };
        // A newer round started, the probe was toggled off, or the cube was recreated
        // (app_poll bumps the generation) — this verdict judges content that is gone.
        if !enabled || gpu.probe_verdict_round != gpu.probe_round_gen {
            return;
        }
        let free = meta.map(|m| m[1]).unwrap_or(f32::NAN);
        gpu.probe_free_fraction = free;
        let valid = probe_capture_is_valid(free);
        let face_size = gpu.probe_face_size.max(1);
        let p = if valid {
            ProbeParams {
                center_enabled: [gpu.probe_center[0], gpu.probe_center[1], gpu.probe_center[2], 1.0],
                extent_mips: [
                    gpu.probe_extent[0],
                    gpu.probe_extent[1],
                    gpu.probe_extent[2],
                    probe_max_sample_mip(face_size), // clamp sampling above the 1x1 mip
                ],
            }
        } else {
            ProbeParams { center_enabled: [0.0; 4], extent_mips: [0.0; 4] }
        };
        gpu.queue
            .write_buffer(gpu.probe_params_buffer.as_ref().unwrap(), 0, bytemuck::bytes_of(&p));
        // SH valid (probe_sh[9].w): the GPU projection left it 0; only a pass sets it.
        if let Some(shb) = gpu.probe_sh_buffer.as_ref() {
            let w: f32 = if valid { 1.0 } else { 0.0 };
            gpu.queue.write_buffer(shb, PROBE_SH_META_OFFSET + 12, bytemuck::bytes_of(&w));
        }
        gpu.probe_baked = valid;
        gpu.probe_rejected = !valid;
        // CPU mirror for gates outside the gpu borrow (probe-only SSR dispatch).
        self.render.pbr_probe_ready = valid;
        // D3: restart progressive accumulation so the new lighting enters at full
        // weight (ladder's single control point — never the refine sig). Either
        // outcome changes what the frame shows (published probe, or the previous
        // round's probe withdrawn).
        self.render.pbr_refine_frames = 0;
        // One line per bake round — the owner-side diagnosis for "no reflections":
        // if this never prints, the bake is blocked (scheduling/residency); if it
        // prints "baked" and chrome stays flat, the fault is in SSR consumption; if it
        // prints "REJECTED", the auto placement put the eye inside geometry.
        eprintln!(
            "[PROBE] {} {}² @ center ({:.2},{:.2},{:.2}) extent ({:.2},{:.2},{:.2}) free {:.3} (min {:.2})",
            if valid { "baked" } else { "REJECTED (eye inside geometry → HDRI IBL)" },
            face_size,
            gpu.probe_center[0], gpu.probe_center[1], gpu.probe_center[2],
            gpu.probe_extent[0], gpu.probe_extent[1], gpu.probe_extent[2],
            free, PROBE_MIN_FREE_FRACTION,
        );
    }

    /// B3: rebuild probe face/mip/SH views + bind groups when the cube generation
    /// or HDR size changes. Steady-state bake rounds create zero new views/BGs.
    fn ensure_probe_view_cache(
        gpu: &mut crate::gpu_ctx::GpuCtx,
        face_size: u32,
        mips: u32,
        hdr_size: (u32, u32),
    ) {
        let cube_gen = gpu.probe_cube_generation;
        let cube_ok = gpu.probe_view_cache_gen == cube_gen
            && gpu.probe_face_mip0.is_some()
            && !gpu.probe_mip_bgs.is_empty()
            && gpu.probe_sh_bg.is_some();
        let capture_ok = cube_ok
            && gpu.probe_capture_bgs.is_some()
            && gpu.probe_capture_hdr_size == hdr_size;

        if cube_ok && capture_ok {
            return;
        }

        if gpu.probe_cube.is_none()
            || gpu.hdr_texture.is_none()
            || gpu.gbuf_motion.is_none()
            || gpu.probe_capture_bgl.is_none()
            || gpu.probe_capture_buffer.is_none()
            || gpu.probe_mip_bgl.is_none()
            || gpu.probe_sh_bgl.is_none()
            || gpu.probe_sh_buffer.is_none()
            || gpu.probe_sampler.is_none()
        {
            return;
        }

        if !cube_ok {
            let face_views: [wgpu::TextureView; 6] = {
                let cube = gpu.probe_cube.as_ref().unwrap();
                std::array::from_fn(|face| {
                    cube.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_mip_level: 0,
                        mip_level_count: Some(1),
                        base_array_layer: face as u32,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                })
            };

            let mut mip_src = Vec::with_capacity(mips.saturating_sub(1) as usize);
            let mut mip_dst = Vec::with_capacity(mips.saturating_sub(1) as usize);
            let mut mip_bgs = Vec::with_capacity(mips.saturating_sub(1) as usize);
            for mip in 1..mips {
                let (src, dst) = {
                    let cube = gpu.probe_cube.as_ref().unwrap();
                    let src = cube.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::Cube),
                        base_mip_level: mip - 1,
                        mip_level_count: Some(1),
                        base_array_layer: 0,
                        array_layer_count: Some(6),
                        ..Default::default()
                    });
                    let dst = cube.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::D2Array),
                        base_mip_level: mip,
                        mip_level_count: Some(1),
                        base_array_layer: 0,
                        array_layer_count: Some(6),
                        ..Default::default()
                    });
                    (src, dst)
                };
                let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("probe_mip_bg"),
                    layout: gpu.probe_mip_bgl.as_ref().unwrap(),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: wgpu::BindingResource::Sampler(
                                gpu.probe_sampler.as_ref().unwrap(),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&dst),
                        },
                    ],
                });
                mip_src.push(src);
                mip_dst.push(dst);
                mip_bgs.push(bg);
            }

            let sh_mip = face_size.ilog2().saturating_sub(3);
            let sh_view = {
                let cube = gpu.probe_cube.as_ref().unwrap();
                cube.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    base_mip_level: sh_mip,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(6),
                    ..Default::default()
                })
            };
            let sh_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("probe_sh_bg"),
                layout: gpu.probe_sh_bgl.as_ref().unwrap(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&sh_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: gpu.probe_sh_buffer.as_ref().unwrap().as_entire_binding(),
                    },
                ],
            });

            gpu.probe_face_mip0 = Some(face_views);
            gpu.probe_mip_src_views = mip_src;
            gpu.probe_mip_dst_views = mip_dst;
            gpu.probe_mip_bgs = mip_bgs;
            gpu.probe_sh_view = Some(sh_view);
            gpu.probe_sh_bg = Some(sh_bg);
            gpu.probe_view_cache_gen = cube_gen;
            // Force capture BG rebuild against fresh face views.
            gpu.probe_capture_bgs = None;
            gpu.probe_capture_hdr_size = (0, 0);
        }

        if gpu.probe_capture_bgs.is_none() || gpu.probe_capture_hdr_size != hdr_size {
            let hdr_view = gpu
                .hdr_texture
                .as_ref()
                .unwrap()
                .create_view(&wgpu::TextureViewDescriptor::default());
            // Probe-frame facing marks (validity measurement). Same size/lifetime block
            // as the HDR texture (viewport_render drops these BGs when it recreates them).
            let facing_view = gpu
                .gbuf_motion
                .as_ref()
                .unwrap()
                .create_view(&wgpu::TextureViewDescriptor::default());
            // Build without holding face_views borrow across create_bind_group loops
            // that also need &gpu.device — reborrow face view per face from stored array.
            let mut cap_bgs_vec: Vec<wgpu::BindGroup> = Vec::with_capacity(6);
            for face in 0..6 {
                let face_view = &gpu.probe_face_mip0.as_ref().unwrap()[face];
                let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("probe_capture_bg"),
                    layout: gpu.probe_capture_bgl.as_ref().unwrap(),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&hdr_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(face_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: gpu.probe_capture_buffer.as_ref().unwrap().as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 8,
                            resource: wgpu::BindingResource::TextureView(&facing_view),
                        },
                    ],
                });
                cap_bgs_vec.push(bg);
            }
            let cap_bgs: [wgpu::BindGroup; 6] = cap_bgs_vec
                .try_into()
                .unwrap_or_else(|_| unreachable!("exactly 6 capture BGs"));
            gpu.probe_capture_bgs = Some(cap_bgs);
            gpu.probe_capture_hdr_size = hdr_size;
        }
    }

    /// Lazy probe pipelines (the fxaa_bgl precedent — self-contained, no app_new /
    /// background-thread involvement; compiled once, ~trivial shaders).
    fn ensure_probe_pipelines(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        // Validity readback staging (16 B = probe_sh[9]). Separate from the pipeline
        // gate: a failed map drops it (probe_collect_verdict) and it is recreated here.
        if gpu.probe_verdict_staging.is_none() {
            gpu.probe_verdict_staging = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("probe_verdict_staging"),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if gpu.probe_capture_pipeline.is_some() {
            return;
        }
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("probe"),
            source: wgpu::ShaderSource::Wgsl(graphics::PROBE_WGSL.into()),
        });
        let tex = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sto = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba16Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let uni = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let cap_bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_capture_bgl"),
            // 8 = G-buffer body_index target (R32Float, probe-frame facing marks).
            entries: &[tex(0), sto(1), uni(2), tex(8)],
        });
        // Cross-face mip reduce: cube view (filterable — hardware seamless edge
        // filtering is the whole point) + sampler + 6-layer D2Array storage dest.
        let mip_bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_mip_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::Cube,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                    count: None,
                },
            ],
        });
        // SH projection: 8×8 mip bound as a 6-layer D2Array + the SH storage buffer.
        let sh_bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_sh_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let cap_pl = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe_capture"),
            layout: Some(&gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&cap_bgl],
                push_constant_ranges: &[],
            })),
            module: &module,
            entry_point: Some("probe_capture"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mip_pl = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe_mip_reduce_cube"),
            layout: Some(&gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&mip_bgl],
                push_constant_ranges: &[],
            })),
            module: &module,
            entry_point: Some("probe_mip_reduce_cube"),
            compilation_options: Default::default(),
            cache: None,
        });
        gpu.probe_capture_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe_capture_params"),
            size: std::mem::size_of::<ProbeCaptureParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let sh_pl = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe_sh_project"),
            layout: Some(&gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&sh_bgl],
                push_constant_ranges: &[],
            })),
            module: &module,
            entry_point: Some("probe_sh_project"),
            compilation_options: Default::default(),
            cache: None,
        });
        gpu.probe_capture_bgl = Some(cap_bgl);
        gpu.probe_mip_bgl = Some(mip_bgl);
        gpu.probe_sh_bgl = Some(sh_bgl);
        gpu.probe_capture_pipeline = Some(cap_pl);
        gpu.probe_mip_pipeline = Some(mip_pl);
        gpu.probe_sh_pipeline = Some(sh_pl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GPU marking + integration is covered end-to-end by the AZRA_PARITY gate
    /// (lone sphere → free 0.000 rejected; `AZRA_PARITY_SCENE=room` → free 1.000 baked).
    /// This pins the CPU decision: the measured extremes, and FAIL-CLOSED on a value
    /// that is not a measurement.
    #[test]
    fn probe_publish_rule_fails_closed() {
        // Measured: eye inside a lone solid / eye in a room's air.
        assert!(!probe_capture_is_valid(0.0));
        assert!(probe_capture_is_valid(1.0));
        // Threshold is inclusive.
        assert!(probe_capture_is_valid(PROBE_MIN_FREE_FRACTION));
        assert!(!probe_capture_is_valid(PROBE_MIN_FREE_FRACTION - 1e-3));
        // Failed readback is NaN; corrupt data is non-finite or outside [0,1].
        assert!(!probe_capture_is_valid(f32::NAN));
        assert!(!probe_capture_is_valid(f32::INFINITY));
        assert!(!probe_capture_is_valid(1.5));
    }
}
