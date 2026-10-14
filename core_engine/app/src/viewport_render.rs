//! GPU/CUDA render dispatch — moved from viewport.rs.

use eframe::egui;
use eframe::wgpu;
use glam::Vec3;
use graphics::RenderConfig;
use crate::*;
use crate::ui_update::{pack_compositor_buf, build_light_link_masks};
// Render-mode decision (resolve_render_mode + GpuReadiness/RenderModeInputs + DRAFT_* consts)
// extracted to viewport_render_mode.rs (de-god slice 14).
use crate::viewport_render_mode::{
    resolve_render_mode, GpuReadiness, RenderModeInputs,
    DRAFT_RENDER_DURING_COMPILE, DRAFT_COMPILE_REPAINT_MS,
};

impl FourthLayerApp {
    pub(crate) fn dispatch_gpu_render(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        profile_scope!("dispatch_gpu_render");
        let _spike_t0 = std::time::Instant::now();
        // Crash diagnostic breadcrumb — written before any GPU work so we know
        // whether the process died inside the render dispatch.
        // CONSOLE_AUDIT B4: frame 1 always; frames 2–5 only under AZRA_PERF_SPAM.
        if self.diag_frame == 1 || (self.diag_frame <= 5 && crate::perf_spam_on()) {
            let gpu_ok = self.gpu.is_some();
            let bg_valid = self.gpu.as_ref().map_or(false, |g| g.bind_groups.bind_group.is_some());
            let vol_cached = self.cache.body_vol_cache.has_cached_grid(self.active_top_body());
            let prim_stale = self.cache.prim_buf_stale;
            let prim_bytes = self.cache.cached_prim_bytes.len();
            eprintln!("[Render] frame={} gpu={} bg_valid={} vol_cached={} prim_stale={} prim_bytes={} rect={}x{}",
                self.diag_frame, gpu_ok, bg_valid, vol_cached, prim_stale, prim_bytes,
                rect.width() as u32, rect.height() as u32);
        }
        // CUDA / OptiX hardware RT branch (feature-gated).
        // Manual toggle: render.use_cuda. Auto: PathTrace + live HW RT triangle pipeline.
        // AI exclusive (Flux / Neural Beauty): skip CUDA entirely — torch owns the GPU.
        #[cfg(feature = "cuda")]
        {
            let ai_exclusive = self.ai.ai_gpu_exclusive();
            let auto_hw_rt = !ai_exclusive
                && matches!(self.render.shading_mode, crate::ShadingMode::PathTrace)
                && self.cuda_renderer.as_ref().map_or(false, |c| c.hw_rt_available());
            let cuda_path_supported = self.render.shading_mode != crate::ShadingMode::PathTrace
                || self.cuda_renderer.as_ref().is_some_and(|renderer| renderer.hw_rt_available());
            if !ai_exclusive && cuda_path_supported && (self.render.use_cuda || auto_hw_rt) {
                if let Some(mut cuda) = self.cuda_renderer.take() {
                    let rendered = self.dispatch_cuda_render(ctx, rect, &mut cuda);
                    self.cuda_renderer = Some(cuda);
                    if rendered { return; }
                }
            }
        }

        // Heavy-shader (deferred-FULL) compile in progress on the background thread? On a cold cache this
        // is minutes. The lite/raster pipelines are already live and the app hot-swaps DF-full in when
        // ready, so rather than black out the viewport for the whole compile, render a THROTTLED DRAFT
        // with the light path (forced via `rm_inputs.draft_compile` below). The throttle + light-only
        // path keep GPU slack for the driver compile — the TDR class the old hard-skip guarded. We fall
        // back to the hard skip + overlay only before ANY pipeline is ready (true cold start) or if
        // DRAFT_RENDER_DURING_COMPILE is off.
        let (compiling_full, lite_ready) = self.gpu.as_ref().map_or((false, false), |g| (
            g.pipelines.compiling_heavy_shader.load(std::sync::atomic::Ordering::SeqCst),
            g.pipelines.wgsl_pipelines_ready,
        ));
        let pt_ready = self.pt_pipeline_ready();
        let pt_cold = self.render.shading_mode == ShadingMode::PathTrace && !pt_ready;
        let draft_compile = (compiling_full || pt_cold) && lite_ready && DRAFT_RENDER_DURING_COMPILE;
        if compiling_full && !draft_compile {
            // Nothing renderable yet (or drafting disabled) — keep the conservative TDR-safe skip.
            if let Some(gpu) = &self.gpu { let _ = gpu.device.poll(wgpu::PollType::Poll); }
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tdr_guard")));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Compiling PBR shaders...",
                egui::FontId::proportional(18.0), egui::Color32::from_rgb(255, 200, 80));
            // B3c: do NOT leave is_dirty latched — that made the end-of-frame throttle call
            // request_repaint() every frame (uncapped Mailbox) for the whole cold compile,
            // drowning the intentional after(100ms) throttle. poll_sdf_compile marks dirty
            // when pipelines land.
            self.is_dirty = false;
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if draft_compile {
            // Keep drawing draft frames (camera stays live) at ~8 fps so the compile keeps GPU slack.
            ctx.request_repaint_after(std::time::Duration::from_millis(DRAFT_COMPILE_REPAINT_MS));
            if !pt_cold {
                // Material compile: a small non-blocking badge instead of the full-screen overlay.
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("draft_compile_badge")));
                painter.text(egui::pos2(rect.left() + 12.0, rect.top() + 12.0), egui::Align2::LEFT_TOP,
                    "Compiling full materials… (draft view)", egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(255, 200, 80));
            }
        }
        // PT warmup notice (PT-COLD-WARMUP-BUNDLED). While the PT display kernel is missing the
        // viewport is NOT path traced — say so prominently (the old 13 pt corner badge read as a
        // broken toggle). Once PT is live, the later stages get a subtle corner note only.
        if self.render.shading_mode == ShadingMode::PathTrace {
            let (pending, failed) = self.gpu.as_ref().map_or((Default::default(), false), |g| {
                (g.pipelines.pt_warmup.pending(), g.pipelines.pt_warmup.failed())
            });
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pt_warmup_notice")));
            let banner = match crate::app_poll::pt_warmup_notice(pt_ready, pending, failed) {
                crate::app_poll::PtWarmupNotice::Preparing => Some((
                    "Preparing path tracer…",
                    "Compiling the path-trace kernel — raster draft shown until it is ready",
                )),
                crate::app_poll::PtWarmupNotice::Unavailable => Some((
                    "Path tracer unavailable",
                    "Path-trace kernel compilation failed — raster draft shown",
                )),
                crate::app_poll::PtWarmupNotice::Refining(note) => {
                    painter.text(egui::pos2(rect.left() + 10.0, rect.bottom() - 10.0), egui::Align2::LEFT_BOTTOM,
                        note, egui::FontId::proportional(11.0), egui::Color32::from_rgba_unmultiplied(200, 200, 200, 150));
                    None
                }
                crate::app_poll::PtWarmupNotice::Silent => None,
            };
            if let Some((title, detail)) = banner {
                let accent = egui::Color32::from_rgb(255, 200, 80);
                let title_galley = painter.layout_no_wrap(title.to_owned(), egui::FontId::proportional(22.0), accent);
                let detail_galley = painter.layout_no_wrap(detail.to_owned(), egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(230, 230, 230));
                let pad = egui::vec2(18.0, 12.0);
                let gap = 4.0;
                let size = egui::vec2(title_galley.size().x.max(detail_galley.size().x), title_galley.size().y + gap + detail_galley.size().y) + pad * 2.0;
                let panel = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 24.0 + size.y * 0.5), size);
                painter.rect_filled(panel, 6.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200));
                painter.rect_stroke(panel, 6.0, egui::Stroke::new(1.0_f32, accent), egui::StrokeKind::Inside);
                let title_pos = egui::pos2(panel.center().x - title_galley.size().x * 0.5, panel.top() + pad.y);
                let detail_pos = egui::pos2(panel.center().x - detail_galley.size().x * 0.5, title_pos.y + title_galley.size().y + gap);
                painter.galley(title_pos, title_galley, accent);
                painter.galley(detail_pos, detail_galley, egui::Color32::from_rgb(230, 230, 230));
            }
        }

        // Billboard face-camera: compute rotation override (read-only — no scene mutation)
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let billboard_yaw = {
            let look = -cam_fwd;
            look.x.atan2(look.z).to_degrees()
        };
        let has_billboards = self.scene.primitives.iter().any(|p| matches!(&p.shape, SdfShape::Billboard { face_camera: true, .. }));
        // Only mark dirty when billboard yaw actually changed (camera rotated),
        // not every frame. Prevents idle RENDER spam + unnecessary serialize.
        let billboard_yaw_changed = has_billboards && (billboard_yaw - self.viewport.camera.prev_billboard_yaw).abs() > 0.01;
        if billboard_yaw_changed { self.mark_dirty(DirtyFlags::RENDER); self.viewport.camera.prev_billboard_yaw = billboard_yaw; }
        // Upload texture atlas if dirty (before bind group recreation)
        if self.tex_atlas_dirty { self.upload_tex_atlas_to_gpu(); self.mark_dirty(DirtyFlags::RENDER); }
        // Split interaction detection into user-driven vs system-driven.
        //
        // user_interacting: the human is actively manipulating something (gizmo,
        //   stroke, camera orbit). Drives episode start/end — pipeline latches here.
        //
        // system_busy: async background work in progress (mesh extraction, bake
        //   readback). Affects FrameScheduler budget only — must NOT start episodes.
        //   Without this split, properties-panel slider edits trigger spurious
        //   episodes via pending_count, causing pipeline switches mid-episode.
        let user_interacting = self.tool.current.is_busy()
            || self.sculpt.stroke_active
            || ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle))
            // Scripted orbit (AZRA_ORBIT) counts as interaction so headless perf runs
            // exercise the same episode/half-res path as a real drag.
            || Self::harness_orbit_step() != 0.0;
        let system_busy = self.cache.mesh_cache.pending_count() > 0;
        // FrameScheduler sees both (budget gating during GPU-heavy frames)
        let interaction_active = user_interacting || system_busy;
        self.scheduler.interaction_active = interaction_active;
        let was_half = self.render.interaction_half_res && self.render.interaction_active_frames >= 3 && self.render.interaction_idle_frames < 3 && self.sculpt.voxel.is_none();
        // Interaction frame counters track USER interaction only
        if user_interacting {
            self.render.interaction_idle_frames = 0;
            self.render.interaction_active_frames = self.render.interaction_active_frames.saturating_add(1);
        } else {
            self.render.interaction_idle_frames = self.render.interaction_idle_frames.saturating_add(1);
            self.render.interaction_active_frames = 0;
        }
        let use_half_res = self.render.interaction_half_res && self.render.interaction_active_frames >= 3 && self.render.interaction_idle_frames < 3 && self.sculpt.voxel.is_none();
        // --- Episode-based render path latching ---
        // Episodes are driven by USER interaction only. Background work (system_busy)
        // must never start or extend an episode — it would create spurious latches
        // with unstable use_lite state (mesh readiness changes during "episodes").
        let episode_prev_active = self.interaction_episode.active;
        if user_interacting && !episode_prev_active {
            // Transition: idle → active — start new episode.
            // Pipeline latch MUST be set here (before the is_dirty gate).
            // If set later in the render dispatch, the is_dirty gate can skip the
            // latch frame entirely → latch never fires → assert.
            //
            // Latch to the no-march lite path in Object Mode; Full/March otherwise.
            // P0.2-full GUARD 2: Object Mode is the only mode — latch the no-march lite path so
            // an episode (orbit / gizmo / 8-frame cooldown) can NEVER dispatch the SDF march into the
            // shared G-buffer alongside the voxel raster. The dispatch arm honors render_mode_latch
            // over render_mode during episodes, so without this every orbit re-opens the fusion crash.
            let (latch, latch_label) = if self.render.shading_mode == ShadingMode::PathTrace && pt_ready && !draft_compile {
                (RenderMode::PathTrace, "pathtrace_tri")
            } else {
                (RenderMode::AnalyticalLite, "raymarch_lite")
            };
            // Path Trace: ALWAYS drop to the half-res PT path for the whole episode.
            // Full-res PT during camera motion measured 320–400 ms/frame on the demo
            // scene (2026-08-04, RTX 4090) — unusable. The generic `use_half_res`
            // needs 3 active frames before it turns on, but the episode latches at
            // frame 1, so for PT it would latch full-res for the entire drag.
            // AZRA_PT_FULL_INTERACT=1 restores full-res interaction for A/B.
            let pt_half = self.render.shading_mode == crate::ShadingMode::PathTrace
                && self.sculpt.voxel.is_none()
                && !std::env::var("AZRA_PT_FULL_INTERACT").is_ok_and(|v| v == "1");
            self.interaction_episode.begin(
                self.diag_frame, latch, latch_label, use_half_res || pt_half,
            );
        }
        if !user_interacting && episode_prev_active {
            // Transition: active → idle — end episode, start cooldown.
            self.interaction_episode.active = false;
            self.interaction_episode.cooldown_remaining = 8;
        }
        if self.interaction_episode.cooldown_remaining > 0 && !self.interaction_episode.active {
            self.interaction_episode.cooldown_remaining -= 1;
        }
        // During episode (or cooldown), use latched half-res instead of recomputed value.
        let use_half_res = if self.interaction_episode.active || self.interaction_episode.cooldown_remaining > 0 {
            self.interaction_episode.use_half_res
        } else {
            use_half_res
        };
        // Transition half→full: force a full-res redraw
        if was_half && !use_half_res { self.mark_dirty(DirtyFlags::RENDER); }
        // UI dock collapse/reopen/resize can change the scene rect without any
        // scene dirty bit. If the GPU texture size lags the live rect, force a
        // re-render so resolution follows (owner: black bar after left reopen).
        // ── Render at PHYSICAL pixels, not egui points ──────────────────────
        //
        // `rect` is an egui Rect in POINTS. Sizing the render target from it meant
        // that at any Windows display scale above 100% the entire viewport rendered
        // BELOW native resolution and was then bilinearly stretched to fill the
        // physical pixels — at 150% that is 44% of native, i.e. every pixel judgement
        // (shadow edges, wire AA, SSGI grain) was being made through an upscale.
        // Recorded open in COMPLETE_BUG_TRACKER.md:75-79.
        //
        // The scale is applied HERE, at the source, and deliberately before the
        // hysteresis gate below: that gate compares against `gpu.texture_size`, which
        // is in physical texels, so a points-vs-texels comparison would differ by the
        // DPI factor every frame and spin `mark_dirty(RENDER)` forever.
        //
        // `resolution_scale` rides on top as the user's cost control. It was DEAD —
        // written to the save file and read back but never consulted by the render
        // path (the generic upsample pass is disabled at :211) — so it is now the
        // natural lever to trade the extra pixels back on a slower GPU.
        let ppp = ctx.pixels_per_point().clamp(0.5, 4.0);
        let res_scale = self.render.resolution_scale.clamp(0.25, 2.0);
        let px_scale = ppp * res_scale;
        let full_w = (rect.width().max(64.0) * px_scale).round().max(64.0) as u32;
        let full_h = (rect.height().max(64.0) * px_scale).round().max(64.0) as u32;
        if let Some(gpu) = self.gpu.as_ref() {
            // B3d: ≥2 px hysteresis — matches viewport rect gate; 1 px A/B thrash was a
            // mark_dirty(RENDER) spin source under idle library/dock layout.
            let (tw, th) = gpu.texture_size;
            let dw = (full_w as i32 - tw as i32).unsigned_abs();
            let dh = (full_h as i32 - th as i32).unsigned_abs();
            if tw != 0 && th != 0 && (dw >= 2 || dh >= 2) {
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        // ── Progressive PBR refine (PROGRESSIVE_PBR_PLAN.md Phase 1) ──────────
        // Idle-only accumulation for the raster viewport: while eligible, every
        // frame renders with R2 sub-pixel jitter and pass_tonemap folds the fully
        // composited HDR into a 1/N running mean (pbr_accum, post_flags bit 256).
        // Resolved BEFORE the redraw pump (so idle frames keep dispatching) and
        // BEFORE the serialize block (so content-dirty flags are still readable
        // here — they are cleared further down). Reset discipline is the PT rule
        // (see the :1144 comment): camera epsilon + signature classes ONLY, never
        // blanket is_dirty.
        //
        // Split view is safe by construction: it dispatches twice per frame with
        // alternating cameras, so the camera epsilon resets every dispatch and
        // accumulation never exceeds the store-only N=1 frame (display-identical
        // to the legacy path).
        const PBR_REFINE_MAX_FRAMES: u32 = 4096; // 1/N step < 0.025% — converged; stop the idle burn
        let mut pbr_accumulate_this_frame = false;
        if self.render.pbr_progressive_enabled {
            // Leak-Proof must WAIT for the RT scene (owner "still breaks when softer"
            // 2026-08-07): before the TLAS/pipeline are resident, refined frames fall
            // back to CSM sun shadows, whose bias leaks (worst at wide cones) would be
            // ACCUMULATED into the average and stay baked in even after rays take
            // over. Not-ready frames simply don't accumulate; the slow repaint below
            // keeps poll frames flowing so the TLAS build finishes.
            // A scene with no raster instances can never build a ray scene (the
            // BLAS/tri buffers stay None by design) — there is also nothing to
            // trace, so RT and CSM are identical there. Without this term an
            // EMPTY scene held !rt_sun_ready forever and the wait-pump below
            // repainted at 20 Hz for the lifetime of the session.
            let scene_has_geo = !self.structural_world_aabb.is_empty();
            let rt_sun_ready = !self.render.pbr_refine_rt_sun
                || !scene_has_geo
                || self.gpu.as_ref().map_or(false, |g| {
                    g.pipelines.rt_shadow_pipeline.is_some()
                        && g.buffers.pt_tlas_buffer.is_some()
                        && g.buffers.pt_blas_buffer.is_some()
                        && g.buffers.pt_tri_buffer.is_some()
                        && g.buffers.pt_vert_buffer.is_some()
                });
            // The wait-pump may only run in a mode refine can actually USE. Outside
            // Pbr/Lit+Hybrid (Matcap, wireframe, PT…) `rt_shadows_active()` rightly
            // never builds the TLAS, so `rt_sun_ready` can never become true — an
            // ungated pump then repaints at 20 Hz forever, which is why "Matcap got
            // slow after visiting progressive PBR" (owner 2026-08-07). Refine in
            // those modes must be a zero-cost no-op, not a busy-wait.
            let mode_ok = matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit)
                && self.render.viewport_mode == ViewportRenderMode::Hybrid;
            if mode_ok && !rt_sun_ready {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            let eligible = mode_ok
                && rt_sun_ready
                && !user_interacting
                && !use_half_res
                && !self.interaction_episode.active
                && self.interaction_episode.cooldown_remaining == 0
                && self.sculpt.voxel.is_none();
            let cam_moved = {
                // Same epsilons as the PT block: bit-equal compares thrash when other
                // systems renormalize the quat / nudge the target by 1 ULP.
                let orbit_moved = self.viewport.camera.orbit
                    .dot(self.render.pbr_refine_last_cam_orbit).abs() < 0.999999; // ~0.08°
                let dist_moved = (self.viewport.camera.distance - self.render.pbr_refine_last_cam_dist)
                    .abs() > 1e-4 * self.render.pbr_refine_last_cam_dist.max(1.0);
                let tgt_moved = (self.viewport.camera.target - self.render.pbr_refine_last_cam_target)
                    .length_squared() > 1e-8;
                orbit_moved || dist_moved || tgt_moved
            };
            // Content dirt is readable HERE (cleared later by serialize/upload):
            // prim re-serialize (SDF/material/transform edits) and light repack
            // invalidate the accumulated average.
            //
            // Mesh extraction is EDGE-detected, not level-tested (owner "restarting
            // the loop" 2026-08-07): `pending_count() > 0` is exactly the signal the
            // interaction-split comment above calls spurious — a queued job can sit
            // pending across many idle frames without the DISPLAYED mesh changing,
            // and testing the level reset accumulation every frame, so the viewport
            // showed raw single-frame jitter noise forever. What actually changes the
            // image is a job COMPLETING (the mesh swaps) — the falling edge.
            let mesh_pending = self.cache.mesh_cache.pending_count() > 0;
            let mesh_completed = self.render.pbr_refine_mesh_pending_prev && !mesh_pending;
            self.render.pbr_refine_mesh_pending_prev = mesh_pending;
            let content_dirty = self.cache.prim_buf_stale
                || self.cache.light_gpu_data_stale
                || mesh_completed;
            // Quality/lighting/selection signature — everything the composited HDR
            // depends on that can change without moving the camera. A missed input
            // shows as a stale average that clears on the next camera nudge; the fix
            // is extending this list, never resetting on is_dirty (:1144 rule).
            // prev_sel_hash covers selection (outline/rim in hdr) with one frame of
            // lag — the pt_last_scene_hash "one frame is fine" precedent.
            let sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for v in &self.cache.cached_light_gpu_data { v.to_bits().hash(&mut h); }
                let r = &self.render;
                for f in [
                    r.env_brightness, r.env_contrast, r.env_saturation, r.env_blur,
                    r.env_reflection_blur, r.env_rotation_deg, r.fov_deg, r.exposure,
                    r.bloom_threshold, r.bloom_intensity, r.ssr_intensity,
                    r.ssgi_intensity, r.ssgi_radius, r.ssgi_env_occlusion,
                    r.fill_light_strength,
                    r.ssao_strength, r.ssao_radius, r.fog_density, r.fog_height_falloff,
                    r.fog_scatter, r.fog_phase_g, r.fog_absorption, r.fog_noise_scale,
                    r.fog_color[0], r.fog_color[1], r.fog_color[2],
                    r.fog_noise_strength, r.fog_max_distance, r.fog_shadow_steps,
                    r.dither_strength, r.wire_overlay_opacity, r.wire_overlay_thickness,
                    r.dof_focus_dist, r.dof_aperture, r.dof_max_blur,
                ] { f.to_bits().hash(&mut h); }
                for c in [r.bg_color_top, r.bg_color_bottom, r.wire_overlay_color, r.env_bg_color] {
                    for f in c { f.to_bits().hash(&mut h); }
                }
                // Hash flags individually — Rust only implements Hash for tuples up to 12 fields.
                r.env_enabled.hash(&mut h);
                r.env_bg_visible.hash(&mut h);
                r.bloom_enabled.hash(&mut h);
                r.ssr_enabled.hash(&mut h);
                r.ssgi_enabled.hash(&mut h);
                r.ssao_enabled.hash(&mut h);
                r.use_gtao.hash(&mut h);
                r.fog_enabled.hash(&mut h);
                r.dither_enabled.hash(&mut h);
                r.wire_overlay_enabled.hash(&mut h);
                r.wire_overlay_backface.hash(&mut h);
                r.dof_enabled.hash(&mut h);
                r.ortho.hash(&mut h);
                r.tonemapper.hash(&mut h);
                r.ssr_max_steps.hash(&mut h);
                r.ssgi_rays.hash(&mut h);
                // Probe resolution / toggle change reflections. D2: do NOT hash
                // `pbr_probe_ready` — that flipped the sig at end of round 1 and
                // forced a redundant second bake of an unchanged scene. D3 restarts
                // accumulation via explicit `pbr_refine_frames = 0` at publish.
                (r.auto_reflection_probe, r.probe_face_size).hash(&mut h);
                // Auto LIGHT probe (Phase 3b): toggling or re-weighting the SH ambient
                // changes converged pixels — restart accumulation so it blends clean.
                r.pbr_refine_light_probe.hash(&mut h);
                r.light_probe_strength.to_bits().hash(&mut h);
                // Preview balls render INSIDE the accumulated HDR (bit 2048) — the
                // toggle must restart accumulation or they fade in/out at 1/N.
                self.viewport.show_probe_gizmo.hash(&mut h);
                // Review 2026-08-07: these change the composited HDR but were
                // missing — their controls read as inert while idle (the exact
                // defect class this hash exists to prevent).
                r.render_quality.hash(&mut h); // fog lite↔full + march tiers
                r.volume_ao_enabled.hash(&mut h);
                r.volume_ao_intensity.to_bits().hash(&mut h);
                r.volume_ao_opacity.to_bits().hash(&mut h);
                // RT projected shadows (audit 2026-08-06). The whole group was absent,
                // so every ray-traced-shadow control read as inert while idle — the
                // owner's "adjusting ray trace does nothing".
                (r.rt_shadows, r.rt_shadow_ray_samples, r.rt_shadow_full_res).hash(&mut h);
                r.rt_shadow_softness.to_bits().hash(&mut h);
                (r.rt_bounces, r.rt_shadow_samples, r.rt_env_samples, r.rt_caustics).hash(&mut h);
                (r.shading_mode as u8).hash(&mut h);
                (self.viewport.grid_visible, self.viewport.grid_mode,
                 self.viewport.show_sel_rim).hash(&mut h);
                self.viewport.grid_cell_size.to_bits().hash(&mut h);
                self.viewport.grid_floor_y.to_bits().hash(&mut h);
                self.viewport.sel_outline_width.to_bits().hash(&mut h);
                for f in self.viewport.sel_rim_color { f.to_bits().hash(&mut h); }
                // Shadow state (Phase 2). The LADDER LEVEL is deliberately NOT hashed
                // (owner "still flicker" 2026-08-07): hashing it while the reset branch
                // also zeroed it made a 2-cycle oscillator — every milestone climb
                // changed the sig → reset → step-down → sig change → reset — so N never
                // passed 32 and shadow maps recreated in a loop. Ladder restarts are
                // now EXPLICIT at the single place the level changes (below).
                (r.shadow_map_resolution,
                 r.pbr_refine_shadow_res_cap, r.shadow_blur_radius).hash(&mut h);
                r.shadow_pcf_softness.to_bits().hash(&mut h);
                // Live shadow-bias sliders (2026-08-06). These reach the GPU through
                // raster_gbuffer's ShadowBias, NOT through GpuUniforms, which is exactly
                // why they were easy to miss here — and a slider missing from this hash
                // is invisible while idle, the defect this whole block exists to prevent.
                r.shadow_normal_offset.to_bits().hash(&mut h);
                r.shadow_depth_bias.to_bits().hash(&mut h);
                r.shadow_slope_bias.to_bits().hash(&mut h);
                r.pbr_refine_sun_angle_deg.to_bits().hash(&mut h);
                r.pbr_refine_rt_sun.hash(&mut h);
                r.pbr_refine_gi.hash(&mut h);
                r.pbr_refine_reflections.hash(&mut h);
                r.physical_light_falloff.hash(&mut h);
                self.prev_sel_hash.hash(&mut h);
                // Viewport size is deliberately NOT hashed (owner "restarting the loop"
                // 2026-08-07): the egui rect can thrash by 1px while idle (the B3d
                // hysteresis note above documents exactly this), and hashing the raw
                // rect made every flicker restart accumulation. Real resizes recreate
                // the render textures, and THAT block already resets the frame counter
                // — the correct, hysteresis-protected signal.
                h.finish().max(1)
            };
            if cam_moved || content_dirty || sig != self.render.pbr_refine_sig || !eligible {
                // Reset diagnostics (AZRA_REFINE_LOG=1): a converged accumulator being
                // reset is invisible in the image except as "it keeps restarting" — log
                // WHICH trigger fired so the next loop is a one-line diagnosis.
                if self.render.pbr_refine_frames >= 30
                    && std::env::var_os("AZRA_REFINE_LOG").is_some()
                {
                    eprintln!(
                        "[REFINE] reset at N={} cam={} prim={} light={} mesh_done={} sig={} eligible={}",
                        self.render.pbr_refine_frames, cam_moved, self.cache.prim_buf_stale,
                        self.cache.light_gpu_data_stale, mesh_completed,
                        sig != self.render.pbr_refine_sig, eligible
                    );
                }
                // Record the trigger for the settings-panel status line (terminal-free
                // diagnosis of "it keeps restarting"). Priority order mirrors severity.
                self.render.pbr_refine_last_reset = if cam_moved {
                    1
                } else if content_dirty {
                    2
                } else if sig != self.render.pbr_refine_sig {
                    3
                } else {
                    4
                };
                self.render.pbr_refine_frames = 0;
                // The shadow ladder deliberately KEEPS its level across content resets
                // (owner "still flicker" 2026-08-07): the audit's step-down here, combined
                // with the level being hashed into `sig`, oscillated reset↔climb forever.
                // A content edit restarts ACCUMULATION but the maps stay hi-res (they are
                // still correct); the level only drops on interaction (the !eligible arm),
                // which is where the perf tax actually matters. The audit's original
                // complaint — "Shadow Map Resolution control looks broken while idle" —
                // is covered because `effective = base << boost` follows the base setting.
                self.render.pbr_refine_last_cam_orbit = self.viewport.camera.orbit;
                self.render.pbr_refine_last_cam_dist = self.viewport.camera.distance;
                self.render.pbr_refine_last_cam_target = self.viewport.camera.target;
                self.render.pbr_refine_sig = sig;
            }
            if eligible {
                // Past MAX the accumulate bit STAYS ON (weight 1/4096 ≈ frozen) so any
                // dirty tick keeps displaying the converged average instead of popping
                // back to a raw aliased frame — but no repaint is requested, so a truly
                // idle viewport stops dispatching entirely.
                pbr_accumulate_this_frame = true;
                if self.render.pbr_refine_frames < PBR_REFINE_MAX_FRAMES {
                    self.render.pbr_refine_frames += 1;
                    // A2 (PBR_REFINE_HARDENING): taper cadence — 16 ms until N=256 (interactive
                    // settle), 50 ms until N=1024, 250 ms thereafter. 1/N past ~1/1000 is
                    // invisible; long-tail idle GPU burn drops ~75% without a quality cliff.
                    let n = self.render.pbr_refine_frames;
                    let ms = if n < 256 {
                        16
                    } else if n < 1024 {
                        50
                    } else {
                        250
                    };
                    ctx.request_repaint_after(std::time::Duration::from_millis(ms));
                }
                // R2 low-discrepancy sub-pixel jitter (the pathtrace_tri Section H
                // sequence), ±0.5px → NDC. Applied to the raster proj in
                // raster_gbuffer and subtracted in every camera_ray (GpuUniforms
                // pbr_jitter) so G-buffer content and ray reconstruction agree.
                let n = self.render.pbr_refine_frames as f64;
                const R2A: f64 = 0.754_877_666_246_692_7;
                const R2B: f64 = 0.569_840_290_998_053_2;
                let jx = ((0.5 + n * R2A).fract() - 0.5) as f32;
                let jy = ((0.5 + n * R2B).fract() - 0.5) as f32;
                self.render.pbr_refine_jitter_ndc =
                    [2.0 * jx / full_w.max(1) as f32, 2.0 * jy / full_h.max(1) as f32];
                // Phase 2: publish the per-frame verdict for raster_gbuffer (PCF/sun
                // jitter) / app_poll (ladder), and step the idle shadow-resolution
                // ladder at milestones. A step EXPLICITLY restarts accumulation right
                // here — the single control point (never via the sig hash: that
                // feedback loop was the "still flicker" oscillator).
                self.render.pbr_refine_active = true;
                // A1 (PBR_REFINE_HARDENING): when Leak-Proof RT sun owns the frame, CSM
                // cascades are discarded in deferred (bit 512). Climbing the idle shadow
                // ladder only re-renders/reallocates maps nobody samples and restarts
                // accumulation twice per session — skip the climb entirely. Cascade
                // ladder returns when RT sun is off (opt-out / not ready).
                let sun_owned_by_rt = self.render.pbr_refine_rt_sun && rt_sun_ready;
                let frames_now = self.render.pbr_refine_frames;
                let boost_target = if sun_owned_by_rt {
                    0
                } else if frames_now >= 128 {
                    2
                } else if frames_now >= 32 {
                    1
                } else {
                    0
                };
                if boost_target > self.render.pbr_refine_shadow_boost {
                    self.render.pbr_refine_shadow_boost = boost_target;
                    // Restart on the new maps (poll recreates them next frame). The
                    // climb can fire at most twice per idle session — never a loop.
                    self.render.pbr_refine_frames = 0;
                    if std::env::var_os("AZRA_REFINE_LOG").is_some() {
                        eprintln!(
                            "[REFINE] ladder climb → boost={} (restart accum)",
                            boost_target
                        );
                    }
                }
            } else {
                self.render.pbr_refine_jitter_ndc = [0.0; 2];
                self.render.pbr_refine_active = false;
                // Interaction (or any ineligibility) restores the user's base shadow
                // resolution — the idle boost must never tax interactive framerate.
                self.render.pbr_refine_shadow_boost = 0;
            }
        } else {
            self.render.pbr_refine_frames = 0;
            self.render.pbr_refine_jitter_ndc = [0.0; 2];
            self.render.pbr_refine_active = false;
            self.render.pbr_refine_shadow_boost = 0;
        }
        // Path-trace progressive refine: keep light frames flowing while idle
        // (infinite / until noise / until max frames) — never one freeze-frame.
        // A — also keep drawing while Hybrid feedback is covering an SDF edit.
        let pt_live_hybrid_early = self.pt_live_hybrid_feedback_active();
        let pt_accumulating = (self.render.shading_mode == ShadingMode::PathTrace
            && crate::viewport_render_mode::pt_should_keep_sampling(
                self.render.pt_stop_mode,
                self.render.pt_converged,
                self.render.pt_accumulated_frames,
                self.render.pt_max_accum_frames,
            ))
            || pt_live_hybrid_early;
        // PBR refine keeps the idle loop drawing until converged (then the repaint
        // requests stop and the pump idles on the frozen average).
        let pbr_refining = pbr_accumulate_this_frame
            && self.render.pbr_refine_frames < PBR_REFINE_MAX_FRAMES;
        if !self.is_dirty && !pt_accumulating && !pbr_refining { return; }
        // Scene changed — invalidate boolean edge cache
        if self.ui.show_bool_edges { self.bool_edge_dirty = true; }
        // Textures always at full resolution — march_resolution only affects SDF dispatch
        let width = full_w;
        let height = full_h;
        let needs_upsample = false; // march resolution handled via block writes
        // Compute march dispatch dimensions (only SDF march runs at reduced resolution)
        let interaction_scale = if use_half_res { 0.5 } else { 1.0 };
        let march_scale = self.render.march_resolution * interaction_scale;
        let march_w = ((full_w as f32 * march_scale).max(32.0)) as u32;
        // (march_h removed — the analytical march display path that used it is stripped 2026-07-14)
        let march_pixel_scale = ((width as f32) / (march_w as f32)).round().max(1.0) as u32;
        let hg = std::collections::HashSet::new(); // layers removed — no hidden groups
        self.sync_light_to_gpu();
        // P3.1: pack lights + link entries + vol bits together (shade list for PBR/Lit).
        // PathTrace keeps full visible list; PBR/Lit top-K by camera importance.
        let cull_lights = !matches!(self.render.shading_mode, ShadingMode::PathTrace);
        // Rebuild when lights dirty OR when cull depends on camera (always repack if cull).
        if self.cache.light_gpu_data_stale
            || self.cache.cached_light_gpu_data.is_empty()
            || cull_lights
        {
            let frame = self.build_frame_lights_gpu(cull_lights, (width, height));
            // Packer order is CAMERA-dependent (top-K + Phase C hysteresis): the cluster
            // list, link masks, and vol bits below upload the NEW shade order every frame,
            // so light_data[] must follow whenever the packed bytes actually changed —
            // otherwise on a plain orbit the shader indexes the new shade order into the
            // OLD light array (wrong light color/position/link, god-ray on wrong light).
            // Compare-before-set keeps the orbit skip when the order didn't change.
            if frame.light_data != self.cache.cached_light_gpu_data {
                self.cache.light_gpu_data_stale = true;
            }
            self.cache.cached_light_gpu_data = frame.light_data.clone();
            self.cache.cached_frame_lights = Some(frame);
        }
        // (viewport mode auto-switch moved to update_viewport_mode_auto — called before render)
        // editing_interaction: sustained tool drag (≥2 frames). During editing,
        // pipeline_march (analytical SDF) gives live feedback instead of volume preview.
        // Volume preview only fills the gap when active body mesh is missing AND not editing.
        // NOTE: no longer gates live_flag (volume-only mode disables analytical overlay —
        // see the stripped volume-preview block). Kept (underscore-prefixed) for documentation +
        // potential future analytical-debug reuse.
        let _editing_interaction = (self.tool.current.is_busy() || self.sculpt.stroke_active)
            && self.render.interaction_active_frames >= 2
            && !self.bevel_pv.active;  // bevel has its own preview overlay on raster mesh
        let active_body_mesh_missing = self.scene.scene_graph.get(self.scene.active_body_nid)
            .and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::SdfBody { mesh_cache_id, .. } =>
                    Some(mesh_cache_id.map_or(true, |cid| self.cache.mesh_cache.get(cid).is_none())),
                _ => None,
            })
            .unwrap_or(false);
        let _ = active_body_mesh_missing; // kept computed for future edit-mode gating diagnostics
        // STRIPPED 2026-07-13 (owner-directed): the SDF volume-raymarch display path
        // (VolumePreview — dense 3D-texture raymarch of SDF bodies) is removed. The DC
        // mesh (raster) is the sole SDF display. The bake infrastructure is untouched.
        // Always-cache: all bodies render from DC mesh cache at all times.
        // No analytical-during-drag mode — the cache pipeline (partial bakes +
        // GPU-to-GPU flat buffer update) provides real-time feedback.

        // Hybrid: raster pass submits its own command buffer before the SDF compute passes.
        // G9: begin_frame MUST run before raster — shadow/gbuf write PASS_SHADOW / PASS_RASTER_GBUF
        // into the same query set. A later begin_frame (old site, post-raster) zeroed active_passes
        // so resolve never covered slots 11/12 → permanent gpuSh=0 gpuGb=0 on the scoreboard.
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(ref mut ts) = gpu.gpu_ts {
                ts.begin_frame();
            }
        }
        let _t_raster = std::time::Instant::now();
        if self.render.viewport_mode == ViewportRenderMode::Hybrid {
            // Auto reflection probe (Phase 3): while idle-refining, capture ONE cube
            // face per frame by running the raster+deferred pipeline with a probe
            // camera and resolving the HDR into the cube (own submits) — then the
            // ordinary raster pass below re-renders everything with the true camera.
            // The G-buffer/HDR are transient scratch between the two submits.
            self.probe_bake_face(width, height);
            self.dispatch_raster_pass(width, height);
        }
        self.perf.raster_ms = _t_raster.elapsed().as_secs_f32() * 1000.0;
        // Bevel preview uploads (before gpu borrow)
        if self.bevel_pv.active {
            if self.bevel_pv.needs_edge_upload {
                self.bevel_preview_upload_edges();
            }
            if self.bevel_pv.needs_param_upload || self.bevel_pv.edge_count > 0 {
                self.bevel_preview_upload_params(rect);
            }
        }
        let comp_snapshot = self.compositor.clone();
        // Shade-order link entries (flat prim indices) from frame packer — must match light buffer.
        let light_snapshot: Vec<(LightLinkMode, Vec<u32>)> = self
            .cache
            .cached_frame_lights
            .as_ref()
            .map(|f| f.link_entries.clone())
            .unwrap_or_default();
        let num_prims = self.scene.primitives.len();
        let _sel_t0 = std::time::Instant::now();
        let sel_set: std::collections::HashSet<usize> = self.selected_prim_indices_for_gpu().into_iter().collect();
        if self.ui.show_perf_hud { self.perf.selected_resolve_ms += _sel_t0.elapsed().as_secs_f64() * 1000.0; }
        // Pre-gpu-borrow: suppress post-process body silhouette for the active edit subject.
        let suppress_body_outline_for_edit = self.outline_suppressed_edit_subject().is_some();
        // Hash the selection set to detect changes without storing the full set.
        let sel_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            sel_set.len().hash(&mut h);
            let mut sorted: Vec<usize> = sel_set.iter().copied().collect();
            sorted.sort_unstable();
            for v in &sorted { v.hash(&mut h); }
            h.finish()
        };
        // High-res sparse (≥512): live raymarch (no mesh tearing). Low-res: triangle mesh path.
        let sculpt_rm_active = self.tool.work_mode == WorkMode::Sculpt
            && self.sculpt.voxel.as_ref().map_or(false, |v| v.use_sculpt_raymarch_overlay());
        // SculptParams (48 bytes): grid header + body PBR so sparse raymarch matches mesh material.
        // (Hardcoded clay albedo caused a visible flash at 256³ when dense mesh → sparse raymarch.)
        let sculpt_rm_params: Option<[u32; 12]> = if sculpt_rm_active {
            let (brick_res, res, iso, bounds) = self.sculpt.voxel.as_ref().map(|v| (
                v.grid.density.brick_res(), v.grid.res, v.grid.iso, v.grid.bounds,
            )).unwrap();
            // Body material from the active VoxelBody node (fallback = default clay grey).
            let (albedo, rough, metal) = self.sculpt.voxel.as_ref()
                .and_then(|v| self.scene.scene_graph.get(v.node))
                .and_then(|n| match &n.payload {
                    core_math::scene_graph::NodePayload::VoxelBody { material, .. } => {
                        Some((material.albedo, material.roughness, material.metallic))
                    }
                    _ => None,
                })
                .unwrap_or((glam::Vec3::new(0.66, 0.585, 0.53), 0.65, 0.0));
            Some([
                brick_res,
                res,
                iso.to_bits(),
                bounds.to_bits(),
                albedo.x.to_bits(), albedo.y.to_bits(), albedo.z.to_bits(), rough.to_bits(),
                metal.to_bits(), 0, 0, 0,
            ])
        } else { None };
        // Selected-prim GPU outline gate — computed BEFORE the gpu borrow.
        // Drives prim-buffer freshness and the outline mask + MaskOutline blend below.
        // Overall setting: Viewport Overlays → "Selection Outline" (`show_sel_rim`).
        // Same toggle in Hybrid and Path Trace (PT used to skip this pass entirely).
        // Use `sel_set` (= selected_prim_indices_for_gpu): **direct prims only**.
        // Object-mode body picks use the body mesh rim (sel_bit), not per-prim outlines.
        // Suppressed while the curve profile point editor is active on an ADDITIVE prim
        // (owner reports 2026-07-23): the blue point/line overlay IS the edit feedback and
        // the pale ghost over it is unwanted. A CUTTER CurveExtrude (Subtract/Intersect)
        // is invisible in the raster mesh — the ghost is its ONLY visual — so cutters keep
        // the standard cutter-ghost treatment. Re-derived every frame, so the outline
        // returns the moment the editor deactivates.
        let outline_active = self.viewport.show_overlays
            && self.viewport.show_sel_rim
            && !sel_set.is_empty()
            && self.curve_edit_prim_index().map_or(true, |i| {
                self.scene.primitives.get(i).map_or(false, |p| !p.operation.is_additive())
            });
        // Depth-tested overlay wireframe: gather this frame's world-space segments BEFORE the
        // `&mut self.gpu` borrow (it walks scene_graph + render settings on self — same reason
        // raster_gbuffer.rs computes its grab_uniform up here). Pure CPU, no GPU calls, so it
        // does not touch the Frame Ordering Contract. The pass itself is encoded after tonemap.
        let _t_ovl = std::time::Instant::now();
        let overlay_line_depth_count = self.build_overlay_lines();
        // POLY_VIEWPORT_PERF P4: depth-tested face selection fill (same ear-clip as solid).
        let overlay_face_xray = self.build_overlay_faces();
        // Scoreboard: when CPU occ is OFF (default), `occ=` reports overlay fingerprint cost
        // (the multi-mesh 10k floor was here — content_signature × N every frame).
        let ovl_ms = _t_ovl.elapsed().as_secs_f32() * 1000.0;
        if !self.render.occlusion_cull {
            self.perf.occlusion_ms = ovl_ms;
        }
        // C2: wall-clock for PathTrace frames (EMA stored after submit).
        // A — Live Hybrid while SDF editing (reuse early flag; BEFORE gpu mut borrow).
        // C — Soft Hybrid→PT resume: seed progressive from last Hybrid HDR (toggleable).
        let pt_live_hybrid = pt_live_hybrid_early;
        if self.render.pt_was_live_hybrid && !pt_live_hybrid {
            self.render.pt_converged = false;
            if self.render.pt_soft_hybrid_resume {
                let soft = self.render.pt_soft_hybrid_frames.clamp(1, 16);
                self.render.pt_accumulated_frames = soft;
                self.render.pt_pending_soft_hybrid_seed = false;
            } else {
                self.render.pt_accumulated_frames = 0;
                self.render.pt_pending_soft_hybrid_seed = false;
            }
        }
        self.render.pt_was_live_hybrid = pt_live_hybrid;
        if let Some(gpu)=&mut self.gpu{
            let device=gpu.device.clone();let queue=gpu.queue.clone();
            // Complete pending screenshot / library thumbnail from previous frame (non-blocking)
            if let Some(pending) = self.pending_screenshot.take() {
                let _ = device.poll(wgpu::PollType::Poll);
                match pending.rx.try_recv() {
                    Ok(Ok(())) => {
                        let slice = pending.staging.slice(..);
                        let data = slice.get_mapped_range();
                        let (w, h, bpr) = (pending.width, pending.height, pending.bpr);
                        // ldr texture is Rgba16Float (8 B/px, post-tonemap 0..1):
                        // decode each f16 channel to u8 while un-padding the 256-aligned rows.
                        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
                        for row in 0..h {
                            let start = (row * bpr) as usize;
                            for x in 0..w as usize {
                                let p = start + x * 8;
                                for c in 0..3 {
                                    let bits =
                                        u16::from_le_bytes([data[p + c * 2], data[p + c * 2 + 1]]);
                                    let v = crate::ai_beauty::f16_bits_to_f32(bits).clamp(0.0, 1.0);
                                    pixels.push((v * 255.0 + 0.5) as u8);
                                }
                                // ldr alpha is not a reliable coverage value — force opaque
                                // (same convention as decode_aovs_from_staging).
                                pixels.push(255);
                            }
                        }
                        drop(data);
                        pending.staging.unmap();
                        let fixed = pending.fixed_path.clone();
                        let thumb = pending.thumb_size; // 0 = full-resolution, uncropped
                        // AZRA_SOAK: keep full-frame RGBA in-process for pixel-diff verdicts
                        // (same decode as the PNG path — no second GPU capture).
                        if self.soak.active && self.soak.waiting_shot {
                            self.soak.last_rgba = Some((w, h, pixels.clone()));
                        }
                        std::thread::spawn(move || {
                            let Some(img) = image::RgbaImage::from_raw(w, h, pixels) else {
                                return;
                            };
                            if let (Some(path), 0) = (fixed.as_ref(), thumb) {
                                if let Err(e) = img.save(path) {
                                    eprintln!("[Screenshot] full-res save failed: {e}");
                                } else {
                                    eprintln!("[Screenshot] full-res saved {} ({w}x{h})", path.display());
                                }
                            } else if let Some(path) = fixed {
                                let thumb = thumb.max(32);
                                // Library kitbash thumbnail / VISDIFF / SOAK fixed path:
                                // center-crop to square + resize. SOAK also holds full-frame
                                // bytes above for in-process px%; the PNG is for humans.
                                let side = w.min(h);
                                let x0 = (w - side) / 2;
                                let y0 = (h - side) / 2;
                                let cropped = image::imageops::crop_imm(&img, x0, y0, side, side)
                                    .to_image();
                                let small = image::imageops::resize(
                                    &cropped,
                                    thumb,
                                    thumb,
                                    image::imageops::FilterType::Triangle,
                                );
                                if let Err(e) = small.save(&path) {
                                    eprintln!("[Library] thumbnail save failed: {e}");
                                } else {
                                    eprintln!("[Library] thumbnail saved {}", path.display());
                                }
                            } else if let Some(path) = rfd::FileDialog::new()
                                .set_title("Save Screenshot")
                                .add_filter("PNG", &["png"])
                                .add_filter("JPEG", &["jpg", "jpeg"])
                                .save_file()
                            {
                                let _ = img.save(&path);
                            }
                        });
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        // Not ready yet — keep waiting next frame
                        self.pending_screenshot = Some(pending);
                    }
                    _ => {
                        eprintln!("[Screenshot] GPU buffer map failed — skipping screenshot");
                    }
                }
            }
            // Poll GPU timestamp readback from previous frame
            if let Some(ref mut ts) = gpu.gpu_ts {
                if ts.poll_results(&device) {
                    if let Some(ms) = ts.completed_frame_ms(gpu_timestamps::PASS_PATH_TRACE) {
                        let previous = self.render.pt_last_frame_ms;
                        self.render.pt_last_frame_ms = if previous <= 0.0 { ms } else { previous * 0.65 + ms * 0.35 };
                    }
                    // Copy ALL resolved slots (the old .min(10) cap silently dropped
                    // the GPU-only passes 10+ — FXAA/Shadow/RasterGbuf/Bake/SSAO —
                    // from the HUD even though the overlays read them).
                    let n = ts.pass_times_ms.len().min(self.ui.prof_gpu_pass_times.len());
                    self.ui.prof_gpu_pass_times[..n].copy_from_slice(&ts.pass_times_ms[..n]);
                }
                self.ui.prof_gpu_timestamps_available = true;
            }
            // Sculpt legacy volume: gpu_volume starts as 1³ (4 B) dummy at init. If the CPU
            // volume is larger (default 128³), recreate before upload — otherwise write_buffer
            // overruns and kills the device ("Copy of 0..8388608 … Destination buffer of size 4").
            if self.tool.work_mode == WorkMode::Sculpt && self.sculpt.voxel.is_none() {
                if let Some(d) = self.cache.cpu_volume.take_dirty_region() {
                    let need = self.cache.cpu_volume.resolution;
                    let have = gpu.gpu_volume.resolution;
                    if need != have
                        || gpu.gpu_volume.gpu_buffer.size()
                            < (need[0] as u64 * need[1] as u64 * need[2] as u64 * 4)
                    {
                        gpu.gpu_volume = graphics::gpu_volume::SdfGpuVolume::new(
                            &device,
                            [need[0], need[1], need[2]],
                        );
                        // gpu_volume is referenced by the compute/deferred/PT bind groups —
                        // recreate ⇒ invalidate ALL of them (CLAUDE.md GPU Safety).
                        gpu.bind_groups.bind_group = None;
                        gpu.bind_groups.deferred_bg = None;
                        gpu.bind_groups.pt_bg = None;
                    }
                    gpu.gpu_volume.upload(&queue, &self.cache.cpu_volume, &d);
                }
            }
            // --- A17: Determine if prim buffer needs re-serialization + upload ---
            // The prim buffer embeds selection bits, render overrides (billboard yaw,
            // edit dimming, footprint hiding, layer offsets), and all prim data.
            // Re-upload is needed when any of these inputs change. When only the
            // camera moved (RENDER-only dirty), we can skip the expensive
            // serialize_from_graph + write_buffer calls (~120KB for 100 prims).
            // CR-76: In volume-only mode, skip prim buffer serialize — UNLESS PT is active
            // and no resident volume exists (PT needs analytical fallback via prim buffer).
            // prim_buf_stale persists so toggling volume-only OFF triggers re-serialize.
            let pt_needs_analytical = self.render.shading_mode == ShadingMode::PathTrace
                && self.cache.body_vol_cache.resident_volume().is_none();
            // Selected-prim GPU outline (`outline_active`, computed before the gpu borrow):
            // the prim_outline pass marches the prim buffer every frame it is visible, so it
            // needs FRESH prim data (live positions during drags) and FRESH selection bits —
            // in every mode, including volume-only, which otherwise skips the serialize.
            let needs_prim_upload = if !pt_needs_analytical && !outline_active {
                // P0.2-full Object Mode: the prim buffer feeds ONLY the bake (which produces the raster
                // mesh) and the analytical march (off here) — it is NOT consumed per-frame by the
                // raster-mesh render, and the selection highlight is drawn by the raster pass (sel_bit),
                // not the prim buffer. So re-serialize ONLY when prims actually change (or the first
                // upload), never on camera/selection frames. This stops the per-frame `[Serialize] full`
                self.cache.prim_buf_stale || self.cache.cached_prim_bytes.is_empty()
            } else {
                self.cache.prim_buf_stale
                    || self.cache.cached_prim_bytes.is_empty()
                    || billboard_yaw_changed
                    || sel_hash != self.prev_sel_hash
                    || self.sel.edit_target != self.prev_edit_target
                    || self.viewport.show_sel_rim != self.prev_show_sel_rim
            };
            let (nv, pb);
            if needs_prim_upload {
                // Build per-prim render overrides (read-only — no scene mutation)
                let footprint_prim = match self.tool.current.as_create() {
                    Some(cs) if cs.phase == CreatePhase::Footprint => Some(cs.prim_idx),
                    _ => None,
                };
                let mut render_overrides: std::collections::HashMap<usize, graphics::RenderPrimOverride> = std::collections::HashMap::new();
                for (i, p) in self.scene.primitives.iter().enumerate() {
                    let mut ovr = graphics::RenderPrimOverride::default();
                    let mut needs_override = false;
                    if matches!(&p.shape, SdfShape::Billboard { face_camera: true, .. }) {
                        ovr.rotation_deg = Some(Vec3::new(0.0, billboard_yaw, 0.0));
                        needs_override = true;
                    }
                    if let Some(target) = self.sel.edit_target {
                        if i != target {
                            ovr.opacity = Some(0.25);
                            needs_override = true;
                        }
                    }
                    if footprint_prim == Some(i) {
                        ovr.hidden = true;
                        needs_override = true;
                    }
                    if needs_override { render_overrides.insert(i, ovr); }
                }
                let empty_sel = std::collections::HashSet::new();
                let gpu_sel = if self.viewport.show_sel_rim { &sel_set } else { &empty_sel };
                let _ser_t0 = std::time::Instant::now();
                profile_scope!("serialize_from_graph");

                // Partial serialize: when exactly one body changed (transform-only drag)
                // and we have valid cached bytes + body ranges, re-serialize only that
                // body's section and patch it into the cached buffer.
                let dirty_body = self.cache.prim_buf_dirty_body;
                // T1: partial body patch rewrites field [292] without re-packing the
                // global height atlas — force full serialize when any prim carries image
                // Displace so live march keeps a coherent atlas.
                let any_image_displace = self.scene.primitives.iter().any(|p| {
                    p.modifier.has_image_displace()
                        || p.modifiers.iter().any(|m| m.has_image_displace())
                });
                let can_partial = dirty_body.is_some()
                    && !self.cache.cached_prim_bytes.is_empty()
                    && !self.cache.cached_body_byte_ranges.is_empty()
                    && self.scene.isolated_body_nid.is_none() // isolation mode changes prim count
                    && render_overrides.is_empty() // overrides may span bodies
                    && !outline_active // LAB: the outline ghost needs the FULL serialize's fresh mesh-grid start array
                    && !any_image_displace;

                let did_partial = if can_partial {
                    let dirty_nid = dirty_body.unwrap();
                    if let Some(&(_, byte_start, byte_end, flat_offset)) =
                        self.cache.cached_body_byte_ranges.iter().find(|(nid, _, _, _)| *nid == dirty_nid)
                    {
                        let body_bytes = graphics::serialize_single_body(
                            &self.scene.scene_graph, dirty_nid, flat_offset,
                            gpu_sel, &self.ai.ai_tex_textured, &hg, &render_overrides,
                        );
                        // Verify size matches — if body gained/lost prims, fall back to full
                        if body_bytes.len() == byte_end - byte_start {
                            self.cache.cached_prim_bytes[byte_start..byte_end].copy_from_slice(&body_bytes);
                            queue.write_buffer(&gpu.buffers.prim_buffer, byte_start as u64, &body_bytes);
                            self.perf.serialize_graph_ms = _ser_t0.elapsed().as_secs_f64() * 1000.0;
                            if self.log_render_mode {
                                eprintln!("[Serialize] mode=partial body={:?} prims={} bytes={} time={:.2}ms",
                                    dirty_nid, body_bytes.len() / (graphics::FLOATS_PER_PRIM * 4),
                                    body_bytes.len(), self.perf.serialize_graph_ms);
                            }
                            true
                        } else {
                            false // size mismatch → fall through to full
                        }
                    } else {
                        false // body not found in ranges → fall through to full
                    }
                } else {
                    false
                };

                if !did_partial {
                    let serialize_mode = if let Some(iso_nid) = self.scene.isolated_body_nid {
                        graphics::SerializeMode::ActiveOnly(iso_nid)
                    } else {
                        graphics::SerializeMode::Full
                    };
                    let ser=graphics::serialize_from_graph(&self.scene.scene_graph,gpu_sel,&self.ai.ai_tex_textured,&hg,&render_overrides,serialize_mode);
                    let(nv_new,pb_new,body_bvh_bytes)=(ser.num_entries,ser.prim_bytes,ser.body_bvh_bytes);
                    self.perf.serialize_graph_ms = _ser_t0.elapsed().as_secs_f64() * 1000.0;
                    if self.log_render_mode {
                        eprintln!("[Serialize] mode=full prims={} bytes={} time={:.2}ms",
                            nv_new, pb_new.len(), self.perf.serialize_graph_ms);
                    }
                    if pb_new.len()as u64>gpu.buffers.prim_buffer.size(){let new_cap=(pb_new.len() as u64).next_power_of_two().max(4096);gpu.buffers.prim_buffer=device.create_buffer(&wgpu::BufferDescriptor{label:Some("PrimData"),size:new_cap,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});gpu.bind_groups.bind_group=None;gpu.bind_groups.tonemap_bind_group=None;gpu.bind_groups.deferred_bg=None;gpu.bind_groups.fog_bg=None;gpu.bind_groups.pt_bg=None;self.perf.gpu_buffer_reallocs+=1;}
                    queue.write_buffer(&gpu.buffers.prim_buffer,0,&pb_new);
                    // LAB poly→SDF + CurveExtrude outline: grow+write mesh-grid/start and
                    // clip-curves/start (parallel to the prim buffer). Full serialize always
                    // produces them (cheap when nothing selected).
                    {
                        let gb=&ser.mesh_grid_bytes; let sb=&ser.mesh_start_bytes;
                        let cb=&ser.clip_curves_bytes; let cs=&ser.clip_start_bytes;
                        let grow=|buf:&mut Option<wgpu::Buffer>, bytes:&[u8], label:&str|{
                            let need=(bytes.len() as u64).max(4);
                            if buf.as_ref().map_or(true,|b|b.size()<need){
                                *buf=Some(device.create_buffer(&wgpu::BufferDescriptor{label:Some(label),size:need.next_power_of_two().max(256),usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}));
                            }
                        };
                        grow(&mut gpu.buffers.outline_mesh_grid_buffer, gb, "OutlineMeshGrid");
                        grow(&mut gpu.buffers.outline_mesh_start_buffer, sb, "OutlineMeshStart");
                        grow(&mut gpu.buffers.outline_clip_curves_buffer, cb, "OutlineClipCurves");
                        grow(&mut gpu.buffers.outline_clip_start_buffer, cs, "OutlineClipStart");
                        grow(&mut gpu.buffers.height_map_buffer, &ser.height_map_bytes, "HeightMaps");
                        if !gb.is_empty(){queue.write_buffer(gpu.buffers.outline_mesh_grid_buffer.as_ref().unwrap(),0,gb);}
                        if !sb.is_empty(){queue.write_buffer(gpu.buffers.outline_mesh_start_buffer.as_ref().unwrap(),0,sb);}
                        if !ser.height_map_bytes.is_empty(){
                            queue.write_buffer(gpu.buffers.height_map_buffer.as_ref().unwrap(),0,&ser.height_map_bytes);
                        }
                        if !cb.is_empty(){queue.write_buffer(gpu.buffers.outline_clip_curves_buffer.as_ref().unwrap(),0,cb);}
                        if !cs.is_empty(){queue.write_buffer(gpu.buffers.outline_clip_start_buffer.as_ref().unwrap(),0,cs);}
                    }
                    self.cache.cached_prim_bytes = pb_new;
                    self.cache.cached_prim_nv = nv_new;
                    self.cache.cached_body_byte_ranges = ser.body_byte_ranges;
                    // Upload body-level BVH
                    if !body_bvh_bytes.is_empty() {
                        if body_bvh_bytes.len() as u64 > gpu.buffers.bvh_node_buffer.size() {
                            gpu.buffers.bvh_node_buffer = device.create_buffer(&wgpu::BufferDescriptor{label:Some("BvhNodes"),size:(body_bvh_bytes.len() as u64).next_power_of_two().max(32),usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});
                            gpu.bind_groups.bind_group = None;                        }
                        queue.write_buffer(&gpu.buffers.bvh_node_buffer, 0, &body_bvh_bytes);
                    }
                    gpu.bvh_node_count = (body_bvh_bytes.len() / 32) as u32;
                }
                self.cache.prim_buf_stale = false;
                self.cache.prim_buf_dirty_body = None;
                self.prev_sel_hash = sel_hash;
                self.prev_edit_target = self.sel.edit_target;
                self.prev_show_sel_rim = self.viewport.show_sel_rim;
                nv = self.cache.cached_prim_nv;
                pb = &self.cache.cached_prim_bytes;
            } else {
                // Camera-only frame: reuse cached prim buffer, skip serialize + upload
                if self.ui.show_perf_hud { self.perf.serialize_graph_ms = 0.0; }
                nv = self.cache.cached_prim_nv;
                pb = &self.cache.cached_prim_bytes;
            }
            // Upload light data — skip upload when lights haven't changed (orbit)
            if self.cache.light_gpu_data_stale {
                let lb = bytemuck::cast_slice::<f32, u8>(&self.cache.cached_light_gpu_data);
                if lb.len() as u64 > gpu.buffers.light_buffer.size() {
                    let new_cap = (lb.len() as u64).next_power_of_two().max(4096);
                    gpu.buffers.light_buffer = device.create_buffer(&wgpu::BufferDescriptor{label:Some("LightData"),size:new_cap,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});
                    // The light buffer (binding 26) is referenced by deferred_bg, pt_bg, fog_bg AND
                    // rt_shadow_bg — invalidate ALL of them on realloc (CLAUDE.md GPU Safety: recreate a
                    // buffer → invalidate EVERY bind group referencing it), or the survivors read a freed
                    // buffer (device loss / garbage). fog_bg + rt_shadow_bg were previously missed here.
                    gpu.bind_groups.deferred_bg = None; gpu.bind_groups.pt_bg = None;
                    gpu.bind_groups.fog_bg = None; gpu.bind_groups.rt_shadow_bg = None;
                    self.perf.gpu_buffer_reallocs += 1;
                }
                queue.write_buffer(&gpu.buffers.light_buffer, 0, lb);
                self.cache.light_gpu_data_stale = false;
            }
            let upsample_stale=(needs_upsample&&(gpu.upsample_texture.is_none()||gpu.viewport_size!=(full_w,full_h)))||(!needs_upsample&&gpu.upsample_texture.is_some());
            // Also rebuild when fog_bg was invalidated (light-buffer realloc) — otherwise fog
            // composite bit stays on while the fog pass never dispatches (stale/zero fog tex).
            if gpu.texture.is_none()||gpu.hdr_texture.is_none()||gpu.gbuf_normal.is_none()||gpu.texture_size!=(width,height)||gpu.bind_groups.bind_group.is_none()||gpu.bind_groups.deferred_bg.is_none()||gpu.bind_groups.ssr_bg.is_none()||gpu.bind_groups.fog_bg.is_none()||upsample_stale{
                // COPY_SRC: blend/outline in-place (hdr → temp), history chain (hdr → pt_accum).
                // COPY_DST: à-trous denoise final blit when step_count is even (temp → hdr).
                // Without COPY_DST, multi-pass PT denoise validation-fails on CommandEncoder::copy_texture_to_texture.
                let hdr_tex=device.create_texture(&wgpu::TextureDescriptor{label:Some("hdr"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::COPY_SRC|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                let hdr_view=hdr_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // RENDER_ATTACHMENT: the depth-tested overlay line pass (line_overlay.rs) draws
                // into this composited texture AFTER tonemap/FXAA, so it must be attachable as a
                // colour target in addition to being a compute storage target.
                let ldr_tex=device.create_texture(&wgpu::TextureDescriptor{label:Some("ldr"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::RENDER_ATTACHMENT|wgpu::TextureUsages::COPY_SRC|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                let ldr_view=ldr_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Progressive PBR refine accumulation pair (E1 lazy): 1×1 dummies keep
                // tonemap BGL valid; full-res allocated on first accumulate frame
                // (`ensure_pbr_accum_full`). Saves full-viewport×2 RGBA16F when Progressive
                // Refine is off (~57–127 MB at typical sizes).
                let pbr_dummy = wgpu::Extent3d{width:1,height:1,depth_or_array_layers:1};
                let pbr_accum_in_tex=device.create_texture(&wgpu::TextureDescriptor{label:Some("pbr_accum_in"),size:pbr_dummy,mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                let pbr_accum_out_tex=device.create_texture(&wgpu::TextureDescriptor{label:Some("pbr_accum_out"),size:pbr_dummy,mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::COPY_SRC,view_formats:&[]});
                let pbr_accum_in_view=pbr_accum_in_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let pbr_accum_out_view=pbr_accum_out_tex.create_view(&wgpu::TextureViewDescriptor::default());
                gpu.pbr_accum_size = (1, 1);
                self.render.pbr_refine_frames = 0;
                let (upsample_tex, upsample_view) = if needs_upsample {
                    let t=device.create_texture(&wgpu::TextureDescriptor{label:Some("upsample"),size:wgpu::Extent3d{width:full_w,height:full_h,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING,view_formats:&[]});
                    let v=t.create_view(&wgpu::TextureViewDescriptor::default());
                    (Some(t), Some(v))
                } else { (None, None) };
                let display_view = if let Some(ref uv)=upsample_view { uv } else { &ldr_view };
                {let mut r=gpu.render_state.renderer.write();if let Some(old)=gpu.texture_id.take(){r.free_texture(&old);}gpu.texture_id=Some(r.register_native_texture(&device,display_view,wgpu::FilterMode::Linear));}
                // G-buffer textures: RENDER_ATTACHMENT for hybrid raster, COPY_SRC on normal for depth snapshot
                let gbuf_f16_desc = wgpu::TextureDescriptor{label:Some("gbuf_f16"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::RENDER_ATTACHMENT|wgpu::TextureUsages::COPY_SRC,view_formats:&[]};
                let gn_tex = device.create_texture(&gbuf_f16_desc);
                let gm_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("gbuf_mat"),..gbuf_f16_desc});
                // Rgba16Float (not Rgba8Unorm): dual-plane Bit16 Mult samples must survive the
                // G-buffer write — 8-bit albedo re-quantized soft Fill×Noise to contour bands.
                let ga_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("gbuf_alb"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::RENDER_ATTACHMENT|wgpu::TextureUsages::COPY_SRC,view_formats:&[]});
                let gn_view = gn_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let gm_view = gm_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let ga_view = ga_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Raster depth snapshot: same format as gbuf_normal, used by SDF compute to preserve raster pixels
                let rd_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("raster_depth"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                let rd_view = rd_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Compositor G-Buffer extension textures
                let sid_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("gbuf_sid"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::R32Uint,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING,view_formats:&[]});
                let sid_view = sid_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // C.5: R32Float body_index — exact ints to 2^24 (past f16's 2048).
                // NOT Rgba32Float: 4×Rgba16Float attachments already hit the 32 B/sample
                // hardware limit; R32Float keeps total at 28 B (3×8 + 4).
                let mot_tex = device.create_texture(&wgpu::TextureDescriptor{
                    label: Some("gbuf_motion"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let mot_view = mot_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let curv_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("gbuf_curv"),..gbuf_f16_desc});
                let curv_view = curv_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let emit_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("gbuf_emit"),..gbuf_f16_desc});
                let emit_view = emit_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Temporal coherence (L7): allocate history + validity mask, used by bind group below
                let temporal_prev_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("temporal_prev_nd"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                let temporal_mask_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("temporal_mask"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::R32Uint,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING,view_formats:&[]});
                let temporal_mask_view = temporal_mask_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // T1 height atlas — dummy 4-byte when no image displace yet this session.
                if gpu.buffers.height_map_buffer.is_none() {
                    gpu.buffers.height_map_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("HeightMapsDummy"),
                        size: 4,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
                }
                let height_map_res = gpu.buffers.height_map_buffer.as_ref().unwrap().as_entire_binding();
                gpu.bind_groups.bind_group=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("raymarch_bg"),layout:&gpu.bind_groups.bind_group_layout,entries:&[
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:2,resource:gpu.gpu_volume.gpu_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:3,resource:gpu.buffers.prim_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:4,resource:gpu.buffers.tex_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:5,resource:gpu.buffers.bvh_node_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:6,resource:gpu.buffers.bvh_index_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:9,resource:wgpu::BindingResource::TextureView(&gn_view)},
                    wgpu::BindGroupEntry{binding:10,resource:wgpu::BindingResource::TextureView(&gm_view)},
                    wgpu::BindGroupEntry{binding:11,resource:wgpu::BindingResource::TextureView(&ga_view)},
                    wgpu::BindGroupEntry{binding:15,resource:height_map_res},
                    wgpu::BindGroupEntry{binding:22,resource:gpu.buffers.tex_atlas_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:23,resource:gpu.buffers.tex_meta_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:29,resource:wgpu::BindingResource::TextureView(&rd_view)},
                    wgpu::BindGroupEntry{binding:30,resource:wgpu::BindingResource::TextureView(&sid_view)},
                    wgpu::BindGroupEntry{binding:32,resource:wgpu::BindingResource::TextureView(&mot_view)},
                    wgpu::BindGroupEntry{binding:34,resource:wgpu::BindingResource::TextureView(&curv_view)},
                    wgpu::BindGroupEntry{binding:36,resource:wgpu::BindingResource::TextureView(&emit_view)},
                    wgpu::BindGroupEntry{binding:44,resource:wgpu::BindingResource::TextureView(&temporal_mask_view)},
                    wgpu::BindGroupEntry{binding:45,resource:gpu.buffers.temporal_march_flags_buffer.as_entire_binding()},
                ]}));
                gpu.raster_depth_texture = Some(rd_tex);
                // SSAO half-res factor texture (deferred binding 38) — recreated HERE,
                // together with the gbuf_normal it reads and the deferred_bg that
                // samples it, so no cached reference can dangle (Frame Ordering:
                // this is the established texture-creation block). White-filled at
                // creation → deferred reads 1.0 until the first SSAO dispatch.
                let ssao_res = render::ssao::SsaoResources::new(&device, &queue, width, height);
                // SSGI half-res delta (SSGI_PLAN.md Gate 1). Same block as the
                // G-buffer it reads and the tonemap_bg that samples it (binding 62),
                // so no cached view can dangle. Zero-filled at creation — the
                // composite adds it unconditionally, so an undispatched frame must
                // read the additive identity, never undefined memory.
                let mut ssgi_res = graphics::ssgi::SsgiResources::new(&device, &queue, width, height);
                // CONSOLE_AUDIT E1: TextureView is !Clone — `.clone()` on &TextureView only
                // copied the reference (noop_method_call warning). Keep a local borrow name.
                let ssgi_view = ssgi_res.output_view();
                // RT projected shadows: pack0 (lights 0..3) + pack1 (lights 4..7, P2.2).
                // Always allocated so deferred BGL is stable; shader reads only when rt_shadows_on.
                let rt_vis_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("rt_shadow_vis"),..gbuf_f16_desc});
                let rt_vis_hi_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("rt_shadow_vis_hi"),..gbuf_f16_desc});
                let rt_vis_view = rt_vis_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let rt_vis_hi_view = rt_vis_hi_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Auto light probe SH9 buffer (deferred binding 51) — persistent,
                // created once, zero-filled so [9].w (valid) = 0 and the shader
                // contributes nothing until a probe bake round projects into it.
                if gpu.probe_sh_buffer.is_none() {
                    gpu.probe_sh_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor{
                        label: Some("probe_sh"),
                        size: 160, // 10 × vec4<f32>
                        // COPY_SRC: [9].y (measured free fraction) → validity readback.
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST
                            | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: false,
                    }));
                }
                // Auto reflection probe resources — created HERE (before deferred_bg,
                // which binds the cube at 65/66/67 for the "Show Probes" preview
                // balls; ssr_bg below shares the same view). Persistent (viewport-
                // size independent). The cube starts black with ProbeParams.enabled
                // = 0, so both consumers ignore it until a bake lands.
                if gpu.probe_cube.is_none() {
                    // Face edge comes from the setting; poll drops the cube + ssr_bg
                    // when it changes, which re-enters this block.
                    let pfs = if crate::probe_bake::PROBE_FACE_SIZES.contains(&self.render.probe_face_size) {
                        self.render.probe_face_size
                    } else {
                        crate::probe_bake::PROBE_FACE_SIZE_DEFAULT
                    };
                    gpu.probe_face_size = pfs;
                    gpu.probe_cube = Some(device.create_texture(&wgpu::TextureDescriptor{
                        label: Some("probe_cube"),
                        size: wgpu::Extent3d{ width: pfs, height: pfs, depth_or_array_layers: 6 },
                        mip_level_count: crate::probe_bake::probe_mips(pfs),
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                        view_formats: &[],
                    }));
                    // B3: invalidate view/BG cache on cube recreate.
                    gpu.probe_cube_generation = gpu.probe_cube_generation.wrapping_add(1);
                    gpu.probe_view_cache_gen = 0;
                    gpu.probe_capture_hdr_size = (0, 0);
                    gpu.probe_face_mip0 = None;
                    gpu.probe_capture_bgs = None;
                    gpu.probe_mip_src_views.clear();
                    gpu.probe_mip_dst_views.clear();
                    gpu.probe_mip_bgs.clear();
                    gpu.probe_sh_view = None;
                    gpu.probe_sh_bg = None;
                    gpu.probe_sampler = Some(device.create_sampler(&wgpu::SamplerDescriptor{
                        label: Some("probe_sampler"),
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::FilterMode::Linear,
                        ..Default::default()
                    }));
                    gpu.probe_params_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor{
                        label: Some("probe_params"),
                        size: 32, // ProbeParams: 2 × vec4<f32>
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
                }
                let probe_cube_view = gpu.probe_cube.as_ref().unwrap().create_view(&wgpu::TextureViewDescriptor{
                    dimension: Some(wgpu::TextureViewDimension::Cube),
                    ..Default::default()
                });
                gpu.bind_groups.deferred_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("deferred_bg"),layout:&gpu.bind_groups.deferred_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&hdr_view)},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:2,resource:gpu.gpu_volume.gpu_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:12,resource:wgpu::BindingResource::TextureView(&gn_view)},
                    wgpu::BindGroupEntry{binding:13,resource:wgpu::BindingResource::TextureView(&gm_view)},
                    wgpu::BindGroupEntry{binding:14,resource:wgpu::BindingResource::TextureView(&ga_view)},
                    wgpu::BindGroupEntry{binding:21,resource:gpu.buffers.env_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:3,resource:gpu.buffers.prim_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:4,resource:gpu.buffers.tex_buffer.as_entire_binding()},
                    // T1 height_maps — deferred reaches sample_height_map_live via scene_sdf()
                    // (SDF reflections), so 15 is a live binding here, not just on raymarch.
                    wgpu::BindGroupEntry{binding:15,resource:gpu.buffers.height_map_buffer.as_ref().unwrap_or(&gpu.buffers.brick_pool_dummy_buf).as_entire_binding()},
                    wgpu::BindGroupEntry{binding:22,resource:gpu.buffers.tex_atlas_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:23,resource:gpu.buffers.tex_meta_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:26,resource:gpu.buffers.light_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:24,resource:gpu.buffers.light_cluster_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:29,resource:wgpu::BindingResource::TextureView(&rd_view)},
                    wgpu::BindGroupEntry{binding:31,resource:wgpu::BindingResource::TextureView(&sid_view)},
                    wgpu::BindGroupEntry{binding:33,resource:wgpu::BindingResource::TextureView(&mot_view)},
                    wgpu::BindGroupEntry{binding:35,resource:wgpu::BindingResource::TextureView(&curv_view)},
                    wgpu::BindGroupEntry{binding:37,resource:wgpu::BindingResource::TextureView(&emit_view)},
                    wgpu::BindGroupEntry{binding:38,resource:wgpu::BindingResource::TextureView(ssao_res.output_view())},
                    wgpu::BindGroupEntry{binding:39,resource:gpu.buffers.brdf_lut_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:40,resource:wgpu::BindingResource::TextureView(&rt_vis_view)},
                    wgpu::BindGroupEntry{binding:41,resource:gpu.buffers.light_link_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:42,resource:gpu.buffers.matcap_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:50,resource:wgpu::BindingResource::TextureView(&rt_vis_hi_view)},
                    wgpu::BindGroupEntry{binding:51,resource:gpu.probe_sh_buffer.as_ref().unwrap().as_entire_binding()},
                    wgpu::BindGroupEntry{binding:65,resource:wgpu::BindingResource::TextureView(&probe_cube_view)},
                    wgpu::BindGroupEntry{binding:66,resource:wgpu::BindingResource::Sampler(gpu.probe_sampler.as_ref().unwrap())},
                    wgpu::BindGroupEntry{binding:67,resource:gpu.probe_params_buffer.as_ref().unwrap().as_entire_binding()},
                ]}));
                gpu.ssao_res = Some(ssao_res);
                // Store the vis textures + invalidate the RT bind group (stale views).
                gpu.rt_shadow_vis = Some(rt_vis_tex);
                gpu.rt_shadow_vis_hi = Some(rt_vis_hi_tex);
                gpu.bind_groups.rt_shadow_bg = None;
                // RT denoise scratch (E1 lazy): 1×1 dummies keep denoise BGL-ready
                // paths from panicking; full-res on first soft-penumbra frame
                // (`ensure_rt_denoise_full`). Soft RT with samples=1 skips denoise entirely.
                let rt_dummy_desc = wgpu::TextureDescriptor{
                    label:Some("rt_denoise_dummy"),
                    size:wgpu::Extent3d{width:1,height:1,depth_or_array_layers:1},
                    mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,
                    format:wgpu::TextureFormat::Rgba16Float,
                    usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::COPY_SRC|wgpu::TextureUsages::COPY_DST,
                    view_formats:&[],
                };
                gpu.rt_shadow_denoised = Some(device.create_texture(&wgpu::TextureDescriptor{label:Some("rt_shadow_denoised"),..rt_dummy_desc}));
                gpu.rt_shadow_history = Some(device.create_texture(&wgpu::TextureDescriptor{
                    label:Some("rt_shadow_history"),
                    usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,
                    ..rt_dummy_desc}));
                gpu.rt_shadow_denoised_hi = Some(device.create_texture(&wgpu::TextureDescriptor{label:Some("rt_shadow_denoised_hi"),..rt_dummy_desc}));
                gpu.rt_shadow_history_hi = Some(device.create_texture(&wgpu::TextureDescriptor{
                    label:Some("rt_shadow_history_hi"),
                    usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,
                    ..rt_dummy_desc}));
                gpu.rt_denoise_size = (1, 1);
                gpu.bind_groups.rt_denoise_bg = None;
                gpu.bind_groups.rt_denoise_bg_hi = None;
                gpu.bind_groups.rt_temporal_bg = None;
                gpu.bind_groups.rt_temporal_bg_hi = None;
                gpu.rt_shadow_history_valid = false;
                gpu.rt_shadow_history_valid_hi = false;
                self.render.rt_shadow_frame = 0; // reset the disc-jitter animation with the history
                // Bevel preview bind group (reads HDR read_write + gbuf_normal)
                gpu.bind_groups.bevel_preview_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("bevel_bg"),layout:&gpu.bind_groups.bevel_preview_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:0,resource:gpu.buffers.bevel_edge_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.bevel_params_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:2,resource:gpu.buffers.bevel_grid_cells_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:3,resource:gpu.buffers.bevel_grid_indices_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:4,resource:wgpu::BindingResource::TextureView(&hdr_view)},
                    wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&gn_view)},
                ]}));
                let ssr_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("ssr"),..gbuf_f16_desc});
                let ssr_view = ssr_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // Probe cube/sampler/params were created above (before deferred_bg,
                // which now also binds them); ssr_bg reuses the same view.
                gpu.bind_groups.ssr_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("ssr_bg"),layout:&gpu.bind_groups.ssr_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&hdr_view)},
                    wgpu::BindGroupEntry{binding:12,resource:wgpu::BindingResource::TextureView(&gn_view)},
                    wgpu::BindGroupEntry{binding:13,resource:wgpu::BindingResource::TextureView(&gm_view)},
                    wgpu::BindGroupEntry{binding:14,resource:wgpu::BindingResource::TextureView(&ga_view)},
                    wgpu::BindGroupEntry{binding:15,resource:wgpu::BindingResource::TextureView(&ssr_view)},
                    wgpu::BindGroupEntry{binding:21,resource:gpu.buffers.env_buffer.as_entire_binding()},
                    // C1: same SSAO factor deferred uses for specular_occlusion.
                    wgpu::BindGroupEntry{binding:38,resource:wgpu::BindingResource::TextureView(gpu.ssao_res.as_ref().unwrap().output_view())},
                    wgpu::BindGroupEntry{binding:39,resource:gpu.buffers.brdf_lut_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:65,resource:wgpu::BindingResource::TextureView(&probe_cube_view)},
                    wgpu::BindGroupEntry{binding:66,resource:wgpu::BindingResource::Sampler(gpu.probe_sampler.as_ref().unwrap())},
                    wgpu::BindGroupEntry{binding:67,resource:gpu.probe_params_buffer.as_ref().unwrap().as_entire_binding()},
                ]}));
                let fog_copy_desc = wgpu::TextureDescriptor{label:Some("fog"),usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::COPY_SRC|wgpu::TextureUsages::COPY_DST,..gbuf_f16_desc};
                let fog_tex = device.create_texture(&fog_copy_desc);
                let fog_prev_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("fog_prev"),..fog_copy_desc});
                // fog_view still feeds the upsample bind group below; fog_prev was only ever
                // read by the inline fog_bg, which rebuild_fog_bg now owns.
                let fog_view = fog_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // fog_bg is NOT built here: `rebuild_fog_bg` (gpu_init::bgl) is the single
                // builder, and it reads the fog textures off `gpu` — which do not receive
                // fog_tex / fog_prev_tex until the assignment block below. Building it here
                // from the locals duplicated the entry list and let it drift from the layout.
                // Deferred to just after the textures land on `gpu`; nothing between here and
                // there reads fog_bg.
                let bw = (width+1)/2; let bh = (height+1)/2;
                let bloom_desc = wgpu::TextureDescriptor{label:Some("bloom"),size:wgpu::Extent3d{width:bw,height:bh,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba16Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::STORAGE_BINDING,view_formats:&[]};
                let ba_tex = device.create_texture(&bloom_desc);
                let bb_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("bloom_b"),..bloom_desc});
                let ba_view = ba_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bb_view = bb_tex.create_view(&wgpu::TextureViewDescriptor::default());
                gpu.bind_groups.tonemap_bind_group=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("tonemap_bg"),layout:&gpu.bind_groups.tonemap_bind_group_layout,entries:&[
                    wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&hdr_view)},
                    wgpu::BindGroupEntry{binding:6,resource:wgpu::BindingResource::TextureView(&ldr_view)},
                    wgpu::BindGroupEntry{binding:7,resource:wgpu::BindingResource::TextureView(&ba_view)},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                    wgpu::BindGroupEntry{binding:16,resource:wgpu::BindingResource::TextureView(&ssr_view)},
                    wgpu::BindGroupEntry{binding:18,resource:wgpu::BindingResource::TextureView(&fog_view)},
                    // Compositor: G-buffer reads
                    wgpu::BindGroupEntry{binding:12,resource:wgpu::BindingResource::TextureView(&gn_view)},
                    wgpu::BindGroupEntry{binding:14,resource:wgpu::BindingResource::TextureView(&ga_view)},
                    wgpu::BindGroupEntry{binding:31,resource:wgpu::BindingResource::TextureView(&sid_view)},
                    wgpu::BindGroupEntry{binding:33,resource:wgpu::BindingResource::TextureView(&mot_view)},
                    wgpu::BindGroupEntry{binding:35,resource:wgpu::BindingResource::TextureView(&curv_view)},
                    wgpu::BindGroupEntry{binding:37,resource:wgpu::BindingResource::TextureView(&emit_view)},
                    // Compositor params buffer
                    wgpu::BindGroupEntry{binding:40,resource:gpu.buffers.compositor_buffer.as_entire_binding()},
                    // SSGI half-res delta — upsampled and added in pass_tonemap.
                    wgpu::BindGroupEntry{binding:62,resource:wgpu::BindingResource::TextureView(ssgi_view)},
                    // Progressive PBR refine accumulation (bit 256): in = last frame's
                    // running mean, out = this frame's (copied out→in after tonemap).
                    wgpu::BindGroupEntry{binding:63,resource:wgpu::BindingResource::TextureView(&pbr_accum_in_view)},
                    wgpu::BindGroupEntry{binding:64,resource:wgpu::BindingResource::TextureView(&pbr_accum_out_view)},
                ]}));
                ssgi_res.ensure_zero(&queue);
                gpu.ssgi_res = Some(ssgi_res);
                gpu.bind_groups.bloom_extract_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("bloom_ext_bg"),layout:&gpu.bind_groups.bloom_extract_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:5,resource:wgpu::BindingResource::TextureView(&hdr_view)},
                    wgpu::BindGroupEntry{binding:8,resource:wgpu::BindingResource::TextureView(&ba_view)},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                ]}));
                gpu.bind_groups.bloom_blur_h_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("bloom_h_bg"),layout:&gpu.bind_groups.bloom_blur_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:7,resource:wgpu::BindingResource::TextureView(&ba_view)},
                    wgpu::BindGroupEntry{binding:8,resource:wgpu::BindingResource::TextureView(&bb_view)},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                ]}));
                gpu.bind_groups.bloom_blur_v_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("bloom_v_bg"),layout:&gpu.bind_groups.bloom_blur_bgl,entries:&[
                    wgpu::BindGroupEntry{binding:7,resource:wgpu::BindingResource::TextureView(&bb_view)},
                    wgpu::BindGroupEntry{binding:8,resource:wgpu::BindingResource::TextureView(&ba_view)},
                    wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                ]}));
                gpu.bloom_a=Some(ba_tex); gpu.bloom_b=Some(bb_tex);
                gpu.gbuf_normal=Some(gn_tex); gpu.gbuf_material=Some(gm_tex); gpu.gbuf_albedo=Some(ga_tex); gpu.ssr_texture=Some(ssr_tex); gpu.fog_texture=Some(fog_tex); gpu.fog_prev_texture=Some(fog_prev_tex);
                // Now that gbuf_normal + both fog textures are on `gpu`, build fog_bg through
                // the one builder that owns its entry list (see the note above where the
                // textures are created).
                crate::gpu_init::rebuild_fog_bg(gpu);
                gpu.gbuf_surface_id=Some(sid_tex); gpu.gbuf_motion=Some(mot_tex); gpu.gbuf_curvature=Some(curv_tex); gpu.gbuf_emission=Some(emit_tex);
                self.render.fog_frame_index = 0; // Reset temporal — no valid prev data after resize
                gpu.temporal_prev_normal_dist = Some(temporal_prev_tex);
                gpu.temporal_validity_mask = Some(temporal_mask_tex);
                gpu.bind_groups.temporal_reproject_bg = None; // Invalidate reproject bind group
                gpu.temporal_frame_valid = false;  // No valid history after resize
                if let Some(ref uv)=upsample_view {
                    gpu.bind_groups.upsample_bg=Some(device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("upsample_bg"),layout:&gpu.bind_groups.upsample_bgl,entries:&[
                        wgpu::BindGroupEntry{binding:19,resource:wgpu::BindingResource::TextureView(&ldr_view)},
                        wgpu::BindGroupEntry{binding:20,resource:wgpu::BindingResource::TextureView(uv)},
                    ]}));
                } else { gpu.bind_groups.upsample_bg=None; }
                gpu.upsample_texture=upsample_tex; gpu.viewport_size=(full_w,full_h);
                gpu.pbr_accum_in=Some(pbr_accum_in_tex); gpu.pbr_accum_out=Some(pbr_accum_out_tex);
                // pbr_accum_size already set to (1,1) above
                // Path trace accumulation texture (same size as HDR, COPY_SRC|COPY_DST for copy)
                let pt_accum_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("pt_accum"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba32Float,usage:wgpu::TextureUsages::TEXTURE_BINDING|wgpu::TextureUsages::COPY_DST,view_formats:&[]});
                gpu.pt_accum_output = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_accum_output"), size: pt_accum_tex.size(), mip_level_count: 1,
                    sample_count: 1, dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }));
                gpu.buffers.pt_moments_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pt_luminance_moments"), size: u64::from(width) * u64::from(height) * 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                gpu.buffers.pt_reproj_moments_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pt_reprojected_moments"), size: u64::from(width) * u64::from(height) * 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }));
                let pt_accum_view = pt_accum_tex.create_view(&wgpu::TextureViewDescriptor::default());
                // PT bind group: invalidated on resize; rebuilt LAZILY in the PathTrace
                // dispatch arm (build_pt_bg) because the triangle-scene SSBOs (bindings
                // 42-47) may not exist yet — update_pt_accel creates them on the first
                // PT frame in the poll phase, before any dispatch (Frame Ordering).
                let _ = pt_accum_view; // view recreated by build_pt_bg
                gpu.bind_groups.pt_bg = None;
                gpu.pt_accum_texture=Some(pt_accum_tex);
                // P3.5 PT reprojection scratch: the reproject pass storage-writes here,
                // then it's copied into pt_accum (no in-place read+write in compute).
                let pt_reproj_tex = device.create_texture(&wgpu::TextureDescriptor{label:Some("pt_reproj_scratch"),size:wgpu::Extent3d{width,height,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:wgpu::TextureFormat::Rgba32Float,usage:wgpu::TextureUsages::STORAGE_BINDING|wgpu::TextureUsages::COPY_SRC,view_formats:&[]});
                gpu.pt_reproj_scratch=Some(pt_reproj_tex);
                // Section M: full-res denoise ping-pong (readable + writable)
                gpu.pt_denoise_temp = Some(device.create_texture(&wgpu::TextureDescriptor{
                    label: Some("pt_denoise_temp"),
                    size: wgpu::Extent3d{width,height,depth_or_array_layers:1},
                    mip_level_count:1, sample_count:1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                }));
                gpu.bind_groups.pt_reproject_bg=None; // refs pt_accum/gbuf_normal/prev_nd/scratch — all recreated here
                gpu.bind_groups.pt_restir_reproject_bg = None;
                crate::gpu_init::invalidate_pt_denoise_bgs(gpu);
                // Section B: half-res PT lighting (25% pixels while camera moves)
                let hw = (width + 1) / 2;
                let hh = (height + 1) / 2;
                let half_desc = wgpu::TextureDescriptor {
                    label: Some("pt_half"),
                    size: wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                };
                gpu.pt_half_hdr = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_hdr"), ..half_desc
                }));
                gpu.pt_half_accum = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_accum"),
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ..half_desc
                }));
                // STORAGE+TEXTURE: reproject write + Section M denoise ping-pong read
                gpu.pt_half_reproj_scratch = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_reproj_scratch"),
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    ..half_desc
                }));
                gpu.pt_half_size = (hw, hh);
                gpu.pt_half_accum_output = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_accum_output"), format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    ..half_desc
                }));
                gpu.buffers.pt_half_moments_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pt_half_luminance_moments"), size: u64::from(hw) * u64::from(hh) * 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                gpu.buffers.pt_half_reproj_moments_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pt_half_reprojected_moments"), size: u64::from(hw) * u64::from(hh) * 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }));
                gpu.pt_half_denoise_temp = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_denoise_temp"), ..half_desc
                }));
                // Section G: ReSTIR DI reservoirs (rgba32f) — full + half
                let restir_usage = wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST;
                let restir_full = wgpu::TextureDescriptor {
                    label: Some("pt_restir"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: restir_usage,
                    view_formats: &[],
                };
                gpu.pt_restir_prev = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_restir_prev"),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ..restir_full
                }));
                gpu.pt_restir_out = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_restir_out"),
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    ..restir_full
                }));
                let restir_half = wgpu::TextureDescriptor {
                    label: Some("pt_half_restir"),
                    size: wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: restir_usage,
                    view_formats: &[],
                };
                gpu.pt_half_restir_prev = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_restir_prev"),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ..restir_half
                }));
                gpu.pt_half_restir_out = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_restir_out"),
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    ..restir_half
                }));
                // Section R: temporal 1-bounce GI (rgba16f, history in alpha)
                let gi_usage = wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST;
                let gi_full = wgpu::TextureDescriptor {
                    label: Some("pt_gi"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: gi_usage,
                    view_formats: &[],
                };
                gpu.pt_gi_prev = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_gi_prev"),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ..gi_full
                }));
                gpu.pt_gi_out = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_gi_out"),
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    ..gi_full
                }));
                let gi_half = wgpu::TextureDescriptor {
                    label: Some("pt_half_gi"),
                    size: wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: gi_usage,
                    view_formats: &[],
                };
                gpu.pt_half_gi_prev = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_gi_prev"),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    ..gi_half
                }));
                gpu.pt_half_gi_out = Some(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pt_half_gi_out"),
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    ..gi_half
                }));
                gpu.bind_groups.pt_bg_half = None;
                gpu.bind_groups.pt_reproject_bg_half = None;
                gpu.bind_groups.pt_restir_reproject_bg = None;
                gpu.bind_groups.pt_restir_reproject_bg_half = None;
                gpu.bind_groups.pt_upscale_bg = None;
                self.render.pt_accumulated_frames = 0; // Reset on resize
                gpu.hdr_texture=Some(hdr_tex);gpu.texture=Some(ldr_tex);gpu.texture_size=(width,height);
                gpu.bind_groups.fxaa_bg=None; // Invalidate FXAA bind group on texture resize
                // Probe capture BGs bind this HDR + gbuf_motion (facing marks) — both
                // were just recreated; the size key alone misses a same-size rebuild.
                gpu.probe_capture_bgs = None;
                gpu.probe_capture_hdr_size = (0, 0);
                // A3: full denoise BG set already cleared above with half textures
            }
            // render_mode: 0=analytical, 1=sculpt voxel. (rm=3 — PT sampling the baked 3D volume
            // texture — was VP-only, keyed on force_volume_only; removed with the legacy volume path.)
            // NEW voxel room (self.sculpt.voxel.is_some()) renders its Surface Nets mesh via the
            // raster path, so the legacy sculpt-voxel raymarch (rm=1) must NOT run.
            // Surface poly sculpt also has no voxel — keep rm=0 or the remesh solid vanishes
            // (legacy cpu_volume raymarch of an empty grid).
            let rm = if self.tool.work_mode == WorkMode::Sculpt
                && self.sculpt.voxel.is_none()
                && self.sculpt.poly_edit.is_none()
            {
                1u32
            } else {
                0u32
            };
            let vm = match self.render.shading_mode { ShadingMode::Pbr=>0, ShadingMode::MatCap=>1, ShadingMode::Wireframe=>2, ShadingMode::SolidHdr=>3, ShadingMode::Raytrace=>4, ShadingMode::PathTrace=>5, ShadingMode::Lit=>6 };
            // Fog composite (bit 8): atmospheric fog and/or volumetric lights.
            // P2: keep composite ON during interaction when global fog is enabled — Lite analytic
            // writes a fresh fog texture so orbit no longer pops fog off.
            // Vol-only (god rays, no global fog) still skips during interaction (expensive).
            // MUST NOT be set in PathTrace: fog compute is non-PT only; PT does its own in-scatter.
            // Cluster debug view: fog would haze the palette to gray — suspend it.
            let fog_composite = !self.render.dbg_cluster_view
                && self.render.shading_mode != ShadingMode::PathTrace
                && (self.render.fog_enabled
                    || (self.scene.any_volumetric_light() && !interaction_active));
            let bloom_flags:u32 = (if self.render.bloom_enabled && !interaction_active { 2u32 } else { 0 })
                // Bit 4 also fires PROBE-ONLY (owner chrome-sphere 2026-08-07): a baked
                // auto probe must reflect without the user enabling SSR — the pass then
                // runs with ssr_intensity 0 (march confidence 0 → pure probe fill).
                // And while the accumulator averages, "Reflections (idle)" runs the
                // traced march at full strength (converged, cannot boil).
                | (if (self.render.ssr_enabled
                        // PBR/Lit only: a baked probe must not paint reflections
                        // into MatCap/Solid/Wireframe (review 2026-08-07). MUST
                        // mirror `ssr_pass_on` below.
                        || (self.render.auto_reflection_probe && self.render.pbr_probe_ready
                            && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit))
                        || (pbr_accumulate_this_frame && self.render.pbr_refine_reflections))
                     && !interaction_active && self.render.shading_mode != ShadingMode::PathTrace { 4u32 } else { 0 })
                | (if fog_composite { 8u32 } else { 0 })
                | (if self.render.dither_enabled { 16u32 } else { 0 })
                | (if self.render.pt_transparent_bg && self.render.shading_mode == ShadingMode::PathTrace { 64u32 } else { 0 })
                // SSGI (bit 128). This condition MUST mirror the dispatch gate below
                // exactly: the tonemap chain runs in EVERY render mode while the SSGI
                // dispatch lives inside the non-PathTrace block, so a bit set without a
                // matching dispatch composites a stale delta from the last raster frame.
                // The undispatched-but-enabled case (lazy pipeline compile, fresh
                // texture) is covered separately by SsgiResources::ensure_zero().
                // Progressive Refine IMPLIES GI (owner 2026-08-07 "progressive render
                // no GI" — the separate SSGI toggle was an invisible prerequisite),
                // with a per-tech opt-out ("Global Illumination (idle)").
                // MUST mirror the dispatch gate at `ssgi_active` below exactly.
                | (if (self.render.ssgi_enabled
                        || (self.render.pbr_progressive_enabled && self.render.pbr_refine_gi))
                     && !self.ssgi_debug.master_kill
                     && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit)
                   { 128u32 } else { 0 })
                // Progressive PBR refine (bit 256): pass_tonemap accumulates the fully
                // composited HDR into pbr_accum. Set only on frames the refine block
                // above marked eligible — never during interaction/PT.
                | (if pbr_accumulate_this_frame { 256u32 } else { 0 })
                // Physical light falloff (bit 1024): windowed inverse-square in
                // point_atten / vol_point_atten (deferred + PT + fog in lockstep).
                | (if self.render.physical_light_falloff { 1024u32 } else { 0 })
                // Probe preview balls (bit 2048): deferred ray-traces a chrome +
                // an irradiance sphere at the probe center. Rides the existing
                // "Show Probes" gizmo toggle; probe_bake masks it out of probe
                // frames so the balls never capture themselves.
                | (if self.viewport.show_probe_gizmo { 2048u32 } else { 0 })
                // Hidden HDRI background (bit 4096): the camera-visible sky falls back
                // to the flat bg gradient (deferred bg/transparent blends, PT primary
                // miss). Set when HIDDEN so an unset bit is the legacy path. IBL,
                // reflections and PT bounce/NEE lighting deliberately ignore it —
                // that separation IS the feature (flat plate, real metal).
                | (if !self.render.env_bg_visible { 4096u32 } else { 0 });
            let fog_d = if self.render.fog_enabled { self.render.fog_density } else { 0.0 };
            // --- Path trace progressive accumulation ---
            // P3.5: set when this frame reprojects the PT accumulation (dispatched in the
            // PathTrace arm below) instead of resetting it.
            let mut pt_reproject_now = false;
            let mut pt_stats_copied = false;
            if self.render.shading_mode == ShadingMode::PathTrace && pt_ready && !draft_compile
                && (!(self.interaction_episode.active || self.interaction_episode.cooldown_remaining > 0)
                    || self.interaction_episode.render_mode_latch == RenderMode::PathTrace) {
                // A1′: drain last frame's residual map (if ready) BEFORE quality resolve.
                // app_poll already ran device.poll(Poll); no Wait here.
                crate::gpu_init::try_complete_pt_stats_readback(gpu);
                // Progressive history must ONLY reset on camera motion (or content edits that
                // already zero frames at their source). NEVER reset on blanket is_dirty —
                // RENDER dirt from poll/UI/selection/bake ticks every frame and was killing
                // accumulation so Infinite never cleaned up.
                // Epsilon compare: bit-equal floats can still thrash if another system
                // renormalizes the quat or nudges target by 1 ULP — that was resetting
                // progressive accum every frame and killing Infinite cleanup.
                let cam_moved = {
                    let o = self.viewport.camera.orbit;
                    let lo = self.render.pt_last_cam_orbit;
                    let orbit_dot = o.dot(lo).abs();
                    let orbit_moved = orbit_dot < 0.999999; // ~0.08°
                    let dist_moved = (self.viewport.camera.distance - self.render.pt_last_cam_dist).abs()
                        > 1e-4 * self.render.pt_last_cam_dist.max(1.0);
                    let tgt = self.viewport.camera.target;
                    let lt = self.render.pt_last_cam_target;
                    let tgt_moved = (tgt - lt).length_squared() > 1e-8;
                    orbit_moved || dist_moved || tgt_moved
                };
                if cam_moved {
                    self.render.pt_converged = false;
                    // P3.5: reproject history under camera motion when possible; else hard reset.
                    let can_reproject = self.render.pt_temporal_reproject
                        && self.render.pt_accumulated_frames > 0
                        && !self.render.ortho
                        && self.render.viewport_mode == ViewportRenderMode::Hybrid
                        && gpu.temporal_frame_valid
                        && gpu.gbuf_normal.is_some()
                        && gpu.temporal_prev_normal_dist.is_some()
                        && (gpu.pt_reproj_scratch.is_some() || gpu.pt_half_reproj_scratch.is_some());
                    if can_reproject {
                        crate::gpu_init::ensure_pt_reproject_pipeline(gpu);
                    }
                    if can_reproject && gpu.pipelines.pt_reproject_pipeline.is_some() {
                        pt_reproject_now = true;
                        // frames = 1 (+1 below → dispatch with pt_accumulated = 2)
                        self.render.pt_accumulated_frames = 1;
                    } else {
                        self.render.pt_accumulated_frames = 0;
                    }
                    self.render.pt_last_cam_orbit = self.viewport.camera.orbit;
                    self.render.pt_last_cam_dist = self.viewport.camera.distance;
                    self.render.pt_last_cam_target = self.viewport.camera.target;
                }
                // Lighting restart: light edits (panel, gizmo drag, undo), environment
                // params, and FOV change radiance without moving orbit/dist/target, so
                // cam_moved never fires — and in Noise/Max stop modes pt_converged stayed
                // true, freezing the image at the old lighting until a camera nudge.
                // Materials already restart via the PT scene hash (pt_scene.rs); lights/
                // env/FOV were the gap. Signature over the packed light data (repacked at
                // frame top when LIGHT-dirty — covers every edit path structurally) + env
                // + fov. Hard reset, no reproject: the old radiance is invalid, unlike
                // camera motion where geometry still matches.
                let lighting_sig = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    for v in &self.cache.cached_light_gpu_data { v.to_bits().hash(&mut h); }
                    self.render.env_brightness.to_bits().hash(&mut h);
                    self.render.env_rotation_deg.to_bits().hash(&mut h);
                    self.render.env_contrast.to_bits().hash(&mut h);
                    self.render.env_saturation.to_bits().hash(&mut h);
                    self.render.env_blur.to_bits().hash(&mut h); self.render.env_reflection_blur.to_bits().hash(&mut h); self.render.env_enabled.hash(&mut h);
                    // Background visibility changes the primary-miss radiance → the PT
                    // accumulation must restart, not blend the two skies at 1/N.
                    self.render.env_bg_visible.hash(&mut h);
                    self.render.fov_deg.to_bits().hash(&mut h);
                    // PT scene content (instances + transforms + MATERIALS — incl. the live
                    // noise-layer writes from node-editor drags): without this, new material
                    // params blend in at 1/hist weight and are invisible until something else
                    // resets — the owner's "noise only shows after the bake" (2026-07-23).
                    // One frame of lag (hash updates in the upload pass) is fine for a drag.
                    self.cache.pt_last_scene_hash.hash(&mut h);
                    h.finish().max(1) // 0 is the "not yet sampled" sentinel
                };
                if self.render.pt_lighting_sig != lighting_sig {
                    if self.render.pt_lighting_sig != 0 {
                        self.render.pt_converged = false;
                        self.render.pt_accumulated_frames = 0;
                        pt_reproject_now = false;
                    }
                    self.render.pt_lighting_sig = lighting_sig;
                }
                // PT traversal backend latch: software BVH <-> hardware ray query. Both run the
                // SAME canonical integrator/materials, but a switch changes the sample stream,
                // so history restarts. The backend only changes outside an interaction
                // episode/cooldown (pipeline latch rule) - except the forced fallback when the
                // native scene stopped being ready, because it can no longer be dispatched.
                // Decided BEFORE the uniforms/increment so the reset lands in this frame.
                let native_ready = !crate::pt_native::software_traversal_forced()
                    && gpu.pipelines.native_rt_pipelines.is_some()
                    && gpu.native_rt.as_ref().is_some_and(|native| native.is_ready());
                let backend_latched = self.interaction_episode.active
                    || self.interaction_episode.cooldown_remaining > 0;
                let use_native = crate::app_poll::latched_pt_choice(native_ready, gpu.native_rt_active, backend_latched);
                if use_native != gpu.native_rt_active {
                    eprintln!("[RTX] PT traversal backend -> {}", if use_native { "hardware ray query" } else { "software BVH" });
                    gpu.native_rt_active = use_native;
                    self.render.pt_converged = false;
                    self.render.pt_accumulated_frames = 0;
                    pt_reproject_now = false;
                }
                // PT interaction-kernel latch (PT-COLD-WARMUP-BUNDLED): the interactive kernel is
                // a later warmup stage. Until it is installed the half-res path runs the full
                // kernel at the interaction budget; one that lands mid-episode is adopted only
                // after the episode + cooldown (same latch rule as the backend above). No history
                // reset: the kernel only drives half-res (motion) frames, and the full-res
                // progressive accumulation never ran it.
                let use_interactive = crate::app_poll::latched_pt_choice(
                    crate::gpu_init::ensure_pt_interactive_pipeline(gpu), gpu.pt_interactive_active, backend_latched);
                if use_interactive != gpu.pt_interactive_active {
                    eprintln!("[PtWarm] PT interaction kernel -> {}", if use_interactive { "interactive" } else { "full (interaction budget)" });
                    gpu.pt_interactive_active = use_interactive;
                }
                self.render.pt_accumulated_frames =
                    self.render.pt_accumulated_frames.saturating_add(1);
                self.render.pt_frame_seed = self.render.pt_frame_seed.wrapping_add(1);
                // Mouse idle tracking for focus
                let mouse_pos = ctx.input(|i| i.pointer.hover_pos());
                if let (Some(cur), Some(prev)) = (mouse_pos, self.render.pt_last_mouse_pos) {
                    if (cur - prev).length() > 2.0 {
                        self.render.pt_mouse_idle_secs = 0.0;
                    } else {
                        self.render.pt_mouse_idle_secs += ctx.input(|i| i.predicted_dt);
                    }
                } else {
                    self.render.pt_mouse_idle_secs += ctx.input(|i| i.predicted_dt);
                }
                self.render.pt_last_mouse_pos = mouse_pos;
                // Progressive stop — quality from many light frames while camera is still.
                let min_frames = 16u32;
                let max_frames = self.render.pt_max_accum_frames.max(16);
                let frac = f32::from_bits(
                    gpu.buffers
                        .pt_noisy_fraction_bits
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                let frames = self.render.pt_accumulated_frames;
                match self.render.pt_stop_mode {
                    0 => {
                        // Infinite: never mark converged; keep shooting until camera/scene moves.
                        self.render.pt_converged = false;
                    }
                    1 => {
                        // Noise threshold: stop when residual is quiet enough (after a short warm-up).
                        // Max frames is only a safety ceiling.
                        if frames >= max_frames {
                            self.render.pt_converged = true;
                        } else if frames >= min_frames
                            && frac <= self.render.pt_noise_threshold
                            && frac < 0.999
                        {
                            self.render.pt_converged = true;
                        }
                    }
                    _ => {
                        // Max frames only
                        if frames >= max_frames {
                            self.render.pt_converged = true;
                        }
                    }
                }
                // Keep the progressive loop alive while idle and not stopped.
                // A4: throttle repaint when residual is clean (Infinite still drips samples
                // at lower Hz — Noise/Max stop via pt_converged as before).
                if crate::viewport_render_mode::pt_should_keep_sampling(
                    self.render.pt_stop_mode,
                    self.render.pt_converged,
                    frames,
                    max_frames,
                ) {
                    let repaint_ms = crate::viewport_render_mode::pt_progressive_repaint_ms(
                        user_interacting,
                        frames,
                        frac,
                    );
                    ctx.request_repaint_after(std::time::Duration::from_millis(repaint_ms));
                }
            } else {
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
            }
            // C — Soft hybrid seed must survive cam-move hard reset same frame so PT
            // actually blends against the seeded Hybrid image (needs pt_accumulated > 1).
            // Compute focus UV for path tracer
            let pt_focus_uv = if self.render.shading_mode == ShadingMode::PathTrace && self.render.pt_mouse_idle_secs >= 5.0 {
                if let Some(mp) = self.render.pt_last_mouse_pos {
                    let uv_x = (mp.x - rect.min.x) / rect.width();
                    let uv_y = (mp.y - rect.min.y) / rect.height();
                    [uv_x, uv_y]
                } else { [-1.0, -1.0] }
            } else { [-1.0, -1.0] };
            // Section A: quality ladder — half-res only for real USER interaction
            // (orbit/gizmo/sculpt). Do NOT use system_busy (mesh pending) — that kept
            // half-res forever and blocked full progressive cleanup.
            let pt_quality = if self.render.shading_mode == ShadingMode::PathTrace {
                // Already inside `&mut self.gpu` — read residual via `gpu` (not self.gpu).
                let noisy = f32::from_bits(
                    gpu.buffers.pt_noisy_fraction_bits
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                let mut q = crate::viewport_render_mode::resolve_pt_quality(
                    user_interacting,
                    self.render.interaction_idle_frames,
                    self.render.pt_samples_per_frame,
                    self.render.pt_max_bounces,
                    noisy,
                    self.render.pt_accumulated_frames,
                    self.render.pt_fast_mode,
                );
                if (self.interaction_episode.active || self.interaction_episode.cooldown_remaining > 0)
                    && self.interaction_episode.render_mode_latch == RenderMode::PathTrace {
                    q.half_res = self.interaction_episode.use_half_res;
                    q.resolution_scale = if q.half_res { 0.5 } else { 1.0 };
                    if q.half_res {
                        q.samples_per_pixel = 1;
                        q.max_bounces = q.max_bounces.min(2);
                    }
                }
                // C2: lean spp/bounces (and half-res in Fast) if last PT frame overran budget
                crate::viewport_render_mode::apply_pt_frame_budget(
                    q,
                    self.render.pt_last_frame_ms,
                    self.render.pt_fast_mode,
                )
            } else {
                crate::viewport_render_mode::PathTraceQuality {
                    resolution_scale: 1.0,
                    samples_per_pixel: 1,
                    max_bounces: 1,
                    half_res: false,
                }
            };
            // B3: half↔full switch.
            // IMPORTANT: do NOT seed full progressive history from half lighting.
            // Upscaled half-res rows create a horizontal zipper that progressive
            // averaging would lock in for dozens of frames (user-visible banding).
            // Hard-reset full accum so the first full-res frames rebuild cleanly.
            if self.render.shading_mode == ShadingMode::PathTrace {
                if self.render.pt_was_half_res != pt_quality.half_res {
                    self.render.pt_converged = false;
                    if self.render.pt_was_half_res && !pt_quality.half_res {
                        // half → full: discard half history (no seed)
                        self.render.pt_accumulated_frames = 0;
                    } else if !self.render.pt_was_half_res && pt_quality.half_res {
                        // full → half: soft restart half progressive
                        let f = self.render.pt_accumulated_frames;
                        self.render.pt_accumulated_frames = f.min(4).max(1);
                    }
                }
                self.render.pt_was_half_res = pt_quality.half_res;
            }
            let mut cfg=RenderConfig{width:width as usize,height:height as usize,cam_orbit:self.viewport.camera.orbit,cam_dist:self.viewport.camera.distance,cam_target:self.viewport.camera.target,grid_res:self.cache.cpu_volume.resolution[0],grid_bounds:self.cache.cpu_volume.grid_bounds,num_primitives:nv,render_mode:rm,view_mode:vm,render_quality:self.render.render_quality,light:self.scene.light_config,tex_size:self.ai.ai_tex_dim,tex_tiling:self.ai.ai_tex_tiling,exposure:self.render.exposure,bloom_threshold:self.render.bloom_threshold,bloom_intensity:self.render.bloom_intensity,bloom_flags,fog_density:fog_d,fog_height_falloff:self.render.fog_height_falloff,fog_start:1.0,fog_scatter:self.render.fog_scatter,env_width:if self.render.env_enabled { self.env_hdr_width } else { 0 },env_height:if self.render.env_enabled { self.env_hdr_height } else { 0 },bg_color_top:if self.render.shading_mode==ShadingMode::MatCap{if self.render.matcap_bg_gradient{self.render.matcap_bg_gradient_top}else{self.render.matcap_bg_color}}else{self.render.bg_color_top},bg_color_bottom:if self.render.shading_mode==ShadingMode::MatCap{self.render.matcap_bg_color}else{self.render.bg_color_bottom},ortho:self.render.ortho,fov_deg:self.render.fov_deg,env_brightness:self.render.env_brightness,env_contrast:self.render.env_contrast,env_saturation:self.render.env_saturation,env_blur:self.render.env_blur,env_reflection_blur:self.render.env_reflection_blur,env_rotation:self.render.env_rotation_deg.to_radians(),env_lut_res:if self.brdf_lut_uploaded{crate::env_ibl::BRDF_LUT_RES}else{0},
            // Phase 2: idle frames have the budget — double the SSR march steps while the
            // refine accumulator averages ("better reflection rays"). Constant across one
            // accumulation run (starts fresh on idle entry), so the mean stays consistent.
            // Idle SSR boost. The ceiling MUST stay above 2x the slider max (96) or the
            // top of the user's range flattens: with the old .min(128), every value from
            // 64 to 96 produced an identical 128 while idle — a dead top third of the
            // "SSR Quality" slider (audit 2026-08-06).
            // E2 Idle Quality: High doubles SSR steps while refining; Low keeps base.
            ssr_max_steps:if pbr_accumulate_this_frame && self.render.idle_quality_high{(self.render.ssr_max_steps*2).min(192)}else{self.render.ssr_max_steps},
            // Three-way (owner "chrome only shows HDR" 2026-08-07): SSR toggle ON →
            // the user's intensity; toggle OFF but ACCUMULATING with "Reflections
            // (idle)" → full traced strength (the 1/N mean converges the jittered
            // march — this is what makes chrome pick up on-screen geometry); else
            // probe-only (intensity 0 → march confidence 0 → pure probe fill).
            ssr_intensity:if self.render.ssr_enabled{self.render.ssr_intensity}
                else if pbr_accumulate_this_frame && self.render.pbr_refine_reflections{1.0}
                else{0.0},fill_light:self.render.fill_light_strength.max(0.0),tonemapper:self.render.tonemapper,fog_phase_g:self.render.fog_phase_g,fog_absorption:self.render.fog_absorption,fog_noise_scale:self.render.fog_noise_scale,fog_noise_strength:self.render.fog_noise_strength,fog_max_distance:self.render.fog_max_distance,fog_shadow_steps:self.render.fog_shadow_steps,fog_temporal_blend:self.render.fog_temporal_blend,fog_frame_index:self.render.fog_frame_index,fog_color:self.render.fog_color,rt_bounces:self.render.rt_bounces,rt_shadow_samples:self.render.rt_shadow_samples,pt_max_bounces:pt_quality.max_bounces,pt_samples_per_frame:{
                // Quality ladder sets base; adaptive can still force 1 spp for first idle frames.
                if self.render.pt_adaptive_spp && self.render.pt_accumulated_frames < 4 {
                    1
                } else {
                    pt_quality.samples_per_pixel
                }
            },pt_accumulated:self.render.pt_accumulated_frames,pt_frame_seed:self.render.pt_frame_seed,pt_focus_uv,pt_focus_radius:self.render.pt_focus_radius,
            // Pack residual threshold into pt_focus_falloff (unused for focus falloff in PT-tri).
            pt_focus_falloff: self.render.pt_noise_threshold,
            rt_env_samples:self.render.rt_env_samples,rt_caustics:self.render.rt_caustics,pt_dof_aperture:self.render.pt_dof_aperture,pt_denoise:self.render.pt_denoise,pt_clamp:self.render.pt_clamp,..Default::default()};
            // Dynamic far clip — must cover the furthest possible geometry (horizon floor creation
            // can place shapes 100s of units away). Sphere tracing cost is SDF-driven, not
            // distance-driven, so a generous far_clip has negligible performance impact.
            // Same helper as the raster camera so ortho zoom-close stays consistent.
            let (_near, far) =
                render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
            cfg.far_clip = far;
            cfg.max_steps = self.render.march_steps;
            cfg.hit_threshold = self.render.march_hit_threshold;
            // GPU grid / shadow catcher floor
            if self.viewport.grid_visible && self.viewport.grid_mode == 1 {
                // Shadow Catcher: GPU floor plane with shadows + grid lines
                cfg.grid.fade_dist = self.viewport.camera.distance * 3.0;
                cfg.grid.line_thickness = 0.02;
            } else {
                // Wire mode uses egui overlay (draw_grid_overlay), not GPU floor
                cfg.grid.fade_dist = 0.0;
                cfg.grid.line_thickness = 0.0;
            }
            // Hidden HDRI plate: the flat colour replaces the viewport gradient at the ONE place
            // every background producer reads it — the raster/lite G-buffer fill, deferred's
            // see-through-object blend, and the `pass_tonemap` composite swap all decode
            // bg_top/bottom, so a single override keeps all three identical. Top == bottom so the
            // plate is genuinely flat. MatCap keeps its own background (hiding an environment is
            // meaningless there), and with no HDRI loaded the user's own gradient is untouched.
            if !self.render.env_bg_visible
                && self.render.env_enabled
                && self.env_hdr_width > 0
                && self.render.shading_mode != ShadingMode::MatCap
            {
                cfg.bg_color_top = self.render.env_bg_color;
                cfg.bg_color_bottom = self.render.env_bg_color;
            }
            cfg.grid.cell_size = self.viewport.grid_cell_size;
            cfg.grid.floor_y = self.viewport.grid_floor_y;
            queue.write_buffer(&gpu.buffers.fog_prev_camera_buffer, 0, self.prev_cam_uniforms.as_bytes());
            let mut gu = cfg.to_gpu_uniforms();
            // Pack march_pixel_scale into cam_up.w (unused by camera ray computation)
            gu.cam_up[3] = march_pixel_scale as f32;
            // Progressive PBR refine: this frame's sub-pixel jitter (camera_ray
            // un-jitters by it), and the frame count rides pt_accumulated — unused by
            // any dispatched pass outside PathTrace, and the PT block zeroes
            // pt_accumulated_frames in non-PT modes.
            gu.pbr_jitter = self.render.pbr_refine_jitter_ndc;
            if pbr_accumulate_this_frame {
                gu.pt_accumulated = self.render.pbr_refine_frames;
            }
            { let sym = &self.viewport.symmetry;
              gu.sym_enabled = if sym.enabled { 1 } else { 0 };
              // .w carries the procedural fill-light strength — preserve it, do not zero it.
              gu.sym_origin = [sym.origin.x, sym.origin.y, sym.origin.z, gu.sym_origin[3]];
              gu.sym_normal = [sym.normal.x, sym.normal.y, sym.normal.z, sym.radial_count as f32];
            }
            // Wire selection rim settings from viewport state.
            // Body-level selection = bright green, prim-level = user-configurable (default orange).
            gu.sel_rim_color = if self.sel.usel.has_nodes() && !self.sel.usel.has_prims() {
                [0.15, 0.9, 0.3] // bright green for body selection
            } else {
                self.viewport.sel_rim_color
            };
            gu.sel_rim_enabled = if self.viewport.show_sel_rim { 1 } else { 0 };
            gu.sel_rim_intensity = 0.0;
            // Authored in POINTS, consumed as target texels — scale with the render
            // target so the outline keeps its apparent width at any display scale.
            gu.sel_rim_power = self.viewport.sel_outline_width * px_scale;
            gu.sel_rim_base = 0.0;
            // Workplane is always rendered via egui overlay (draw_workplane_overlay)
            // for consistent wire-grid appearance. GPU shader workplane disabled.
            gu.wp_normal[3] = 0.0;
            gu.wp_origin[3] = 0.0;
            // Dither amplitude: user strength × 1/(2^bits-1). 8-bit swapchain = 1/255.
            // When 10-bit display detected, change 255.0 → 1023.0.
            gu.dither_strength = self.render.dither_strength / 255.0;
            // Compute scene bounding box for ray early termination
            let (aabb_lo, aabb_hi) = graphics::compute_scene_aabb(pb, nv);
            gu.scene_aabb_min = [aabb_lo[0], aabb_lo[1], aabb_lo[2], self.render.march_relaxation];
            gu.scene_aabb_max = [aabb_hi[0], aabb_hi[1], aabb_hi[2], 0.0];
            gu.debug_mode = self.ui.debug_render_mode;
            // Suppress deferred analytical shadows (soft_shadow / calc_ao) during
            // interaction unless the user opted into interaction shadows.
            //
            // REGRESSION FIX (owner "no more self shadow, no cast shadow" 2026-08-07):
            // Phase 2b briefly OR'd `pbr_accumulate_this_frame` in here to silence the
            // analytical SDF sun-shadow smear while refining — but this flag is BLUNT:
            // it also kills point/spot SDF soft shadows (their ONLY shadow path for
            // non-RT lights), contact shadows, AND gates the RT sun swap itself, so a
            // point-lit scene converged to a shadowless image and Leak-Proof Sun never
            // engaged. The refine suppression now rides its own surgical channel —
            // post_flags bit 512, set below — which silences ONLY the analytical
            // sun/AO recompute and doubles as the RT-sun-swap key.
            gu.interaction_no_shadows = if interaction_active && !self.render.interaction_shadows {
                1
            } else {
                0
            };
            gu.bvh_enabled = 0; // analytical BVH path removed 2026-07-14 — uniform slot kept, always 0
            // RT projected shadows (Stage 1): the deferred point/spot loops read the RT visibility
            // buffer ONLY when this is set. Gate it on the buffer being VALID this frame — toggle on,
            // not PathTrace (deferred doesn't run there), the rt_shadow pipeline compiled (poll-warmed),
            // AND the PT scene SSBOs resident (Stage-0 decouple; None until update_pt_accel assembles a
            // BLAS). The rt_shadow dispatch below runs under the SAME conditions, so the flag can never
            // read a stale buffer. tlas_count==0 (empty scene) is safe: the pass writes vis=1.0 (lit).
            // P3.1: shade-order VOL bitfield from frame pack (falls back to graph-order any-vol).
            gu.fog_has_vol_lights = self
                .cache
                .cached_frame_lights
                .as_ref()
                .map(|f| f.vol_bits)
                .unwrap_or_else(|| self.scene.volumetric_light_bitfield());
            gu.rt_shadows_on = if (self.render.rt_shadows
                || (pbr_accumulate_this_frame && self.render.pbr_refine_rt_sun))
                && self.render.shading_mode != ShadingMode::PathTrace
                && gpu.pipelines.rt_shadow_pipeline.is_some()
                // Gate on the FULL buffer set build_rt_shadow_bg requires — NOT just tlas.
                // The empty-scene refit fast path uploads only tlas/instance/material/meta, leaving
                // blas/tri/vert None; gating on tlas alone would set rt_shadows_on=1 while the bind
                // group fails to build → the pass never writes the vis texture → wgpu's zero-init
                // (0.0 = fully occluded) blacks out ALL point/spot lighting. Match the destructure.
                && gpu.buffers.pt_tlas_buffer.is_some()
                && gpu.buffers.pt_blas_buffer.is_some()
                && gpu.buffers.pt_tri_buffer.is_some()
                && gpu.buffers.pt_vert_buffer.is_some()
            { 1 } else { 0 };
            // Refine RT sun channel (post_flags bit 512): set ONLY when the swap can
            // actually fire this frame — accumulating + Leak-Proof toggle + the RT pass
            // resident (rt_shadows_on already proves pipeline + all four PT buffers).
            // ONE signal keys the ray pass's sun trace AND deferred's swap, so they can
            // never disagree (the "swap reads a vis channel nobody wrote" class). If RT
            // is not ready yet, the bit stays off and refined frames keep plain CSM
            // until it is — degraded, never shadowless. (The bit's former second duty —
            // suppressing the analytical sun dupe — died with that path, 2026-08-07.)
            if gu.rt_shadows_on == 1
                && pbr_accumulate_this_frame
                && self.render.pbr_refine_rt_sun
            {
                gu.post_flags |= 512;
            }
            // LOD system
            gu.lod_dist_enabled = if self.render.lod_dist_enabled { 1 } else { 0 };
            gu.lod_dist_threshold_0 = self.render.lod_dist_threshold_0;
            gu.lod_dist_threshold_1 = self.render.lod_dist_threshold_1;
            gu.lod_dist_threshold_2 = self.render.lod_dist_threshold_2;
            gu.lod_min_steps = self.render.lod_min_steps as f32;
            gu.foveated_enabled = if self.render.foveated_enabled { 1 } else { 0 };
            gu.foveated_inner = self.render.foveated_inner;
            gu.foveated_outer = self.render.foveated_outer;
            gu.foveated_min_steps = self.render.foveated_min_steps as f32;
            gu.foveated_threshold_mult = self.render.foveated_threshold_mult;
            gu.foveated_max_pixel_scale = self.render.foveated_max_pixel_scale;
            gu.lod_debug_colors = if self.render.lod_debug_colors { 1 } else { 0 };
            gu.march_adaptive_enabled = if self.render.march_adaptive_enabled { 1 } else { 0 };
            gu.march_lod_scale = self.render.march_lod_scale;
            gu.shadow_blur_radius = self.render.shadow_blur_radius;
            // DC wire overlay gate for the deferred dilation pass. Mirrors the
            // `want_gpu_wire` condition in raster_gbuffer.rs — 0.0 when the wire
            // pass doesn't run, so the deferred shader skips the dilation loop.
            // Wire-mode vs wire-overlay are INDEPENDENT (owner 2026-07-26):
            //  • Wireframe MODE (vm==2) must always show edges — selecting it shows
            //    wire, period. It gets a guaranteed thickness/opacity floor so a user
            //    who zeroed the overlay controls (for the OTHER modes) still sees the
            //    full wireframe view.
            //  • The "wire on shaded" OVERLAY draws edges over lit modes and is gated
            //    purely on wire_overlay_enabled + its own thickness/opacity.
            // Wire thickness is authored in POINTS; dc_wire_px is consumed as target
            // texels by the deferred dilation. Scale so the wire keeps its apparent
            // width at any display scale (the `min(.., 12)` tap cap in the shader is a
            // cost ceiling and deliberately stays in texels).
            gu.dc_wire_px = px_scale * if self.render.shading_mode == crate::ShadingMode::Wireframe {
                // Mode: use the user's thickness when set, else a sane default (1.5 px).
                if self.render.wire_overlay_thickness > 0.001 {
                    self.render.wire_overlay_thickness
                } else {
                    1.5
                }
            } else if self.render.wire_overlay_enabled
                && self.render.wire_overlay_thickness > 0.001
                && self.render.wire_overlay_opacity > 0.001
            {
                // Overlay on a lit mode.
                self.render.wire_overlay_thickness.max(0.0)
            } else {
                0.0
            };
            // Compute scene screen size: AABB radius / (cam_dist * fov_scale)
            {
                let aabb_center = glam::Vec3::new(
                    (aabb_lo[0] + aabb_hi[0]) * 0.5,
                    (aabb_lo[1] + aabb_hi[1]) * 0.5,
                    (aabb_lo[2] + aabb_hi[2]) * 0.5,
                );
                let aabb_radius = glam::Vec3::new(
                    (aabb_hi[0] - aabb_lo[0]) * 0.5,
                    (aabb_hi[1] - aabb_lo[1]) * 0.5,
                    (aabb_hi[2] - aabb_lo[2]) * 0.5,
                ).length();
                let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * glam::Vec3::new(0.0, 0.0, self.viewport.camera.distance);
                let dist_to_scene = (cam_pos - aabb_center).length().max(0.01);
                let fov_scale = (self.render.fov_deg.to_radians() * 0.5).tan().max(0.01);
                gu.lod_scene_screen_size = aabb_radius / (dist_to_scene * fov_scale);
            }
            // Check if lite raymarch pipeline can be used (all SDF bodies have GPU-uploaded meshes).
            // The expensive all_nodes() walk is cached — invalidated when prim buffer is re-uploaded.
            if needs_prim_upload || self.cache.cached_all_meshes_ready.is_none() {
                self.cache.cached_all_meshes_ready = Some(self.scene.scene_graph.all_nodes().all(|(nid, n)| match &n.payload {
                    core_math::scene_graph::NodePayload::SdfBody { mesh_cache_id, .. } => {
                        let has_gpu_mesh = mesh_cache_id.map_or(false, |cid| {
                            self.cache.mesh_cache.get(cid).map_or(false, |e| e.gpu_mesh.is_some())
                        });
                        // NeverBaked bodies don't block lite mode — they're invisible
                        // (no raster pixels, no march pixels). Blocking lite for them
                        // would let the full march render them analytically, violating
                        // the cache-first product rule.
                        has_gpu_mesh || (self.cache.use_body_vol_cache
                            && matches!(self.cache.body_vol_cache.display_state(nid),
                                crate::body_volume_cache::BodyDisplayState::NeverBaked))
                    },
                    _ => true,
                }));
            }
            // Cache-first: lite march (floor+bg only) is used whenever all bodies
            // have raster meshes AND no tool drag is active.
            //
            // During gizmo/transform drag the raster mesh is stale (DC mesh lags
            // behind prim edits). Disable lite so the full analytical march renders
            // the active body with up-to-date prim positions — instant visual feedback.
            // Non-active bodies also render analytically during drag (acceptable cost).
            //
            // GATE 1 AUDIT: deferred_entry.wgsl's soft_shadow(), calc_ao(), and
            // SDF reflection probe all call scene_sdf(). Prims remain serialized
            // in the GPU buffer so these secondary queries work even though the
            // primary visible surface comes from raster. The deferred shader
            // (built from sdf_shared.wgsl + deferred_entry.wgsl) has access to
            // scene_sdf() regardless of which march pipeline ran.
            //
            let tool_dragging = self.tool.current.is_gizmo()
                || self.tool.current.is_transform()
                || self.tool.current.is_bevel()
                || self.tool.current.is_bool_edge_bevel()
                || self.tool.current.is_falloff_adjust()
                || self.tool.current.is_face_push_pull();
            // Fall back to analytical during drag because the raster DC mesh is 2-4 frames stale.
            let drag_needs_analytical = tool_dragging;
            let use_lite = self.render.viewport_mode == ViewportRenderMode::Hybrid
                && self.cache.use_body_vol_cache
                && self.ui.debug_render_mode == 0
                && nv > 0
                && self.cache.cached_all_meshes_ready.unwrap_or(false)
                && !drag_needs_analytical
                ; // always-cache: lite pipeline when all meshes ready + idle
            // STRIPPED 2026-07-13: VolumePreview AABB uniform + atlas body-params rebuild removed
            // with the SDF volume-raymarch display path (gpu.volume_atlas is no longer created).
            // Fix 1: check resident volume realloc BEFORE any GPU work referencing it
            if self.cache.body_vol_cache.resident_volume_reallocated {
                // PT bind groups reference the resident volume texture (binding 40)
                gpu.bind_groups.pt_bg = None;
                               self.cache.body_vol_cache.resident_volume_reallocated = false;
            }
            // selected_body_index: dense 0-based body_index for post-process silhouette
            // (`apply_body_outline` — bright green body rim). 0xFFFFFFFF = none.
            //
            // Prefer the **active selected node** (poly/nurbs/sdf), not only `active_body_nid`
            // (historically SDF-centric — poly select did not update it, so poly bodies often
            // shared body_index 0 with a stale active and still got a green rim).
            //
            // Edit subject (poly V/E/F, sculpt, NURBS sub-select): always none — component
            // highlights carry the state. Object mode + other selected bodies keep outline
            // via this path / sel_bit / poly selection wire. Selection state untouched.
            // (`suppress_body_outline_for_edit` computed before the gpu mut borrow.)
            gu.selected_body_index = if suppress_body_outline_for_edit
                || !self.viewport.show_sel_rim
            {
                0xFFFF_FFFFu32
            } else {
                // Nested sub-body / Group: no post-process body rim on the parent mesh
                // (that made the whole kit look selected). Sub-body volume uses prim outline.
                //
                // SELECTION-DERIVED ONLY — never `active_body_nid`. That field is edit
                // SCOPE, not selection: `clear_selection_unified()` deliberately leaves it
                // alone, so every deselect (empty viewport click, Alt+A, clicking the Scene
                // root) left it pointing at the last body and this uniform re-lit that body's
                // green rim with an empty selection (owner 2026-08-11: "at root, nothing
                // selected, no highlight should be seen"). The old fallback fired on exactly
                // that case — `active_n.is_some()` is false precisely when nothing is
                // selected — so its own comment ("empty selection → no outline via this
                // path") described the opposite of what it did.
                //
                // The `.filter` also enforces highlight ⊆ selection structurally: a stale
                // `active` that is no longer in the set can never rim an unselected body.
                // Both sibling highlight paths — raster `sel_bit` (`raster_gbuffer.rs`) and
                // the CPU walk (`draw_node_selection_outlines`) — are already
                // selection-only; this one now matches instead of contradicting them.
                let active_n = self
                    .sel
                    .usel
                    .active_node()
                    .filter(|n| self.sel.usel.selected.contains(&SceneItemId::Node(*n)));
                let nested_sub = active_n.map_or(false, |n| {
                    matches!(
                        self.scene.scene_graph.get(n).map(|node| &node.payload),
                        Some(core_math::scene_graph::NodePayload::SdfBody { .. })
                    ) && self.scene.scene_graph.is_sub_body(n)
                });
                if nested_sub {
                    0xFFFF_FFFFu32
                } else {
                    let outline_nid = active_n.filter(|n| {
                        // Only top-level mesh-owning SdfBody / poly / etc. for body rim.
                        self.cache.body_index_map.contains_key(n)
                            && !self.scene.scene_graph.is_sub_body(*n)
                    });
                    outline_nid
                        .and_then(|n| self.cache.body_index_map.get(&n).copied())
                        .map(|i| i as u32)
                        .unwrap_or(0xFFFF_FFFFu32)
                }
            };
            // Active-body albedo texture map → uniform (the VP shader reads one material/frame).
            // bit 31 = projection (always triplanar for SDF, so projection bit stays 0).
            let amap = self.scene.scene_graph.body_first_albedo_map(self.scene.active_body_nid);
            gu.mat_albedo_map_slot = amap.tex_id & 0x7FFF_FFFF;
            gu.mat_albedo_map_strength = amap.strength;
            gu.mat_albedo_map_uv_scale = amap.uv_scale;
            // MatCap uniforms (2026-07-17f). The VP shader's volume-shadow packing used to
            // overwrite these fields (reach→rotation, bias→edge_threshold, steps→edge_enabled)
            // AND the fog pass's fog_height_falloff/fog_scatter — but the VP shader died
            // 2026-07-13, so the packing only corrupted its landlords: MatCap mode rendered
            // with a 40-radian rotation and edges forced on, and the deleted settings were
            // the only writers of these uniforms. Now the fields carry what their names say.
            gu.matcap_rotation = self.render.matcap_rotation.to_radians();
            gu.matcap_edge_enabled = if self.render.matcap_edge_enabled { 1 } else { 0 };
            gu.matcap_edge_threshold = self.render.matcap_edge_threshold;
            gu.matcap_edge_thickness = self.render.matcap_edge_thickness;
            gu.matcap_edge_color = self.render.matcap_edge_color;
            // Material-colour overlay: the toggle collapses into the amount (0 = off), so the
            // shader needs one branch and the strength survives unchecking on the CPU side.
            gu.matcap_tint = if self.render.matcap_material_tint {
                self.render.matcap_material_tint_strength.clamp(0.0, 1.0)
            } else {
                0.0
            };
            // CRITICAL (2026-07-19): DO NOT pack volume-AO / tricubic flags into fog_* uniforms.
            // The VP volume-raymarch shader that used to read those packed bits died 2026-07-13.
            // The fog compute pass (`pass_fog`) and pathtracer BOTH read fog_density /
            // fog_max_distance / fog_phase_g for REAL atmospheric fog. Overwriting them with
            // `f32::from_bits(vol_flags)` (tiny denormals) made fog appear completely broken
            // in PBR, and PT in-scatter used density≈0 (or garbage). Keep fog fields as authored
            // by RenderConfig → to_gpu_uniforms above.
            //
            // Volume AO intensity still rides scene_aabb_min.w only (not a fog field).
            let editing = self.tool.current.is_busy() || self.sculpt.stroke_active;
            let suppress_ao = editing && !self.render.volume_ao_in_orbit;
            if self.render.volume_ao_enabled && !suppress_ao && self.render.volume_ao_opacity > 0.001 {
                gu.scene_aabb_min[3] = self.render.volume_ao_intensity;
            } else {
                gu.scene_aabb_min[3] = 0.0;
            };
            queue.write_buffer(&gpu.buffers.uniform_buffer, 0, gu.as_bytes());
            // Auto reflection probe (Phase 3): stash a CPU copy — the probe mini-frame
            // next frame patches its camera into this instead of rebuilding the whole
            // per-frame config (probe_bake.rs).
            gpu.last_gu = Some(gu);
            // Auto light probe: CPU owns [9].x (strength) of the SH buffer.
            // A3 (PBR_REFINE_HARDENING): write-on-change only — the 4-byte upload used
            // to run every frame even with the feature off (pure waste). Toggle/slider
            // still acts immediately because the next frame sees a different `s`.
            if let Some(shb) = &gpu.probe_sh_buffer {
                let s: f32 = if self.render.pbr_progressive_enabled
                    && self.render.auto_reflection_probe
                    && self.render.pbr_refine_light_probe
                {
                    self.render.light_probe_strength.max(0.0)
                } else {
                    0.0
                };
                let prev = gpu.probe_sh_strength_written;
                if !prev.is_finite() || (s - prev).abs() > 1e-6 {
                    gpu.queue.write_buffer(shb, 144, bytemuck::bytes_of(&s));
                    gpu.probe_sh_strength_written = s;
                }
            }
            // Upload compositor params
            let comp_data = pack_compositor_buf(&comp_snapshot, width, height);
            queue.write_buffer(&gpu.buffers.compositor_buffer, 0, bytemuck::cast_slice(&comp_data));
            // Upload light link masks (per-prim bitmask, 2 u32 per prim)
            let link_data = build_light_link_masks(&light_snapshot, num_prims);
            if (link_data.len() * 4) as u64 > gpu.buffers.light_link_buffer.size() {
                let new_cap = ((link_data.len() * 4) as u64).next_power_of_two().max(4096);
                gpu.buffers.light_link_buffer = device.create_buffer(&wgpu::BufferDescriptor{label:Some("LightLink"),size:new_cap,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});
                gpu.bind_groups.deferred_bg = None; gpu.bind_groups.tonemap_bind_group = None;
            }
            queue.write_buffer(&gpu.buffers.light_link_buffer, 0, bytemuck::cast_slice(&link_data));
            // P3.1 Phase D: upload cluster light lists (tile × Z)
            let cluster_u32: Vec<u32> = self
                .cache
                .cached_frame_lights
                .as_ref()
                .map(|f| f.cluster_data.clone())
                .unwrap_or_else(crate::light_cluster::empty_cluster_data);
            if (cluster_u32.len() * 4) as u64 > gpu.buffers.light_cluster_buffer.size() {
                let new_cap = ((cluster_u32.len() * 4) as u64).next_power_of_two().max(4096);
                gpu.buffers.light_cluster_buffer = device.create_buffer(&wgpu::BufferDescriptor{
                    label: Some("LightCluster"),
                    size: new_cap,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.bind_groups.deferred_bg = None;
            }
            queue.write_buffer(
                &gpu.buffers.light_cluster_buffer,
                0,
                bytemuck::cast_slice(&cluster_u32),
            );
            let frame_t0 = std::time::Instant::now();
            // ONE frame-wide encoder / ONE submission. In PathTrace with hardware ray queries
            // available, the native RT frame owns it: scheduler-budgeted AS builds, the PT
            // dispatch and all post passes share this encoder, and its caches promote only
            // after this frame's submit (`PtFrameEncoder::submit`).
            let mut enc = if self.render.shading_mode == ShadingMode::PathTrace
                && gpu.native_rt.is_some()
                && gpu.pipelines.native_rt_pipelines.is_some()
            {
                let budget = crate::pt_native::build_budget(gpu.native_rt_status, &mut self.scheduler);
                let frame = crate::pt_native::PtFrameEncoder::prepare(gpu, budget);
                gpu.native_rt_status = frame.status();
                frame
            } else {
                crate::pt_native::PtFrameEncoder::Software(device.create_command_encoder(&wgpu::CommandEncoderDescriptor{label:None}))
            };
            // Hybrid: copy raster G-buffer normal (with world_dist in .w) to raster_depth_texture
            // so ALL compute shaders (lite/march/full/volume_preview) can skip raster-covered pixels
            if self.render.viewport_mode == ViewportRenderMode::Hybrid {
                if let (Some(ref gn), Some(ref rd)) = (&gpu.gbuf_normal, &gpu.raster_depth_texture) {
                    enc.copy_texture_to_texture(gn.as_image_copy(), rd.as_image_copy(), wgpu::Extent3d{width, height, depth_or_array_layers:1});
                }
            }
            // ── Temporal coherence (L7): detect camera movement + dispatch reproject ──
            let temporal_camera_moved = {
                let cur = [gu.cam_pos, gu.cam_forward, gu.cam_right, gu.cam_up];
                let prev: [[f32; 4]; 4] = [
                    [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                    [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                    [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                    [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                ];
                let mut moved = false;
                for i in 0..4 {
                    for j in 0..4 {
                        if (cur[i][j] - prev[i][j]).abs() > 1e-6 { moved = true; }
                    }
                }
                moved
            };
            // Determine if the scene changed (prim buffer re-uploaded = SDF edit or selection change)
            let temporal_scene_dirty = needs_prim_upload;
            // Temporal cache: skip marching unchanged pixels. Enabled when camera static,
            // valid history exists, and not path tracing. Works in hybrid/cache mode because
            // the raster pass writes the same G-buffer data frame-to-frame for static scenes —
            // temporal skip prevents the lite/march shader from redundantly re-rendering
            // floor/background pixels.
            let temporal_active = !temporal_camera_moved
                && gpu.temporal_frame_valid
                && gpu.temporal_validity_mask.is_some()
                && gpu.temporal_prev_normal_dist.is_some()
                && self.render.shading_mode != ShadingMode::PathTrace
                // Phase 1 (Sculpt/Surface raster path): temporal reprojection is an SDF-march
                // denoiser/upsampler. A voxel is a crisp raster mesh — reprojection only adds
                // motion flicker + a re-converge "change" when you stop. Skip it for voxels.
                && self.sculpt.voxel.is_none();
            // Upload temporal march flags (binding 45) — tells the march shader whether to read the mask
            {
                let flags = graphics::TemporalMarchFlags {
                    temporal_enabled: if temporal_active && !temporal_scene_dirty { 1 } else { 0 },
                    _pad: [0; 3],
                };
                queue.write_buffer(&gpu.buffers.temporal_march_flags_buffer, 0, flags.as_bytes());
            }
            // Dispatch temporal reproject pass (writes validity_mask)
            if temporal_active {
                crate::gpu_init::ensure_temporal_pipeline(gpu);
                if let (Some(ref prev_nd), Some(ref mask_tex), Some(ref pipeline), Some(ref bgl), Some(ref params_buf))
                    = (&gpu.temporal_prev_normal_dist, &gpu.temporal_validity_mask, &gpu.pipelines.temporal_reproject_pipeline, &gpu.bind_groups.temporal_reproject_bgl, &gpu.buffers.temporal_params_buffer)
                {
                    let tp = graphics::TemporalParams {
                        prev_cam_pos: [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                        prev_cam_forward: [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                        prev_cam_right: [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                        prev_cam_up: [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                        cur_cam_pos: gu.cam_pos,
                        cur_cam_forward: gu.cam_forward,
                        cur_cam_right: gu.cam_right,
                        cur_cam_up: gu.cam_up,
                        screen_size: [width, height],
                        dirty_rect_min: if temporal_scene_dirty { [0, 0] } else { [u32::MAX, u32::MAX] },
                        dirty_rect_max: if temporal_scene_dirty { [width, height] } else { [0, 0] },
                        camera_moved: if temporal_camera_moved { 1 } else { 0 },
                        frame_valid: if gpu.temporal_frame_valid { 1 } else { 0 },
                    };
                    queue.write_buffer(params_buf, 0, tp.as_bytes());
                    // Recreate bind group if needed (textures may have been resized)
                    if gpu.bind_groups.temporal_reproject_bg.is_none() {
                        let prev_view = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
                        let mask_view = mask_tex.create_view(&wgpu::TextureViewDescriptor::default());
                        gpu.bind_groups.temporal_reproject_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("temporal_reproject_bg"), layout: bgl,
                            entries: &[
                                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&prev_view) },
                                wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&mask_view) },
                            ],
                        }));
                    }
                    if let Some(ref bg) = gpu.bind_groups.temporal_reproject_bg {
                        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("temporal_reproject"), timestamp_writes: None });
                        cp.set_pipeline(pipeline);
                        cp.set_bind_group(0, bg, &[]);
                        cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                    }
                }
                // NOTE: In non-hybrid mode, storage textures retain previous frame data.
                // Temporal-skipped pixels keep their values from the last march dispatch.
                // No pre-copy needed — the G-buffer is never cleared between frames.
            }
            // GPU timestamp queries: take out of GpuState to avoid borrow conflicts with ensure_*_pipeline.
            // begin_frame already ran before hybrid raster (see above) so active_passes still holds
            // shadow/gbuf bits written during dispatch_raster_pass.
            let mut gpu_ts = gpu.gpu_ts.take();
            // Helper: build ComputePassTimestampWrites for a given pass slot
            macro_rules! tw {
                ($slot:expr) => {
                    gpu_ts.as_mut().map(|ts| ts.compute_pass_writes($slot))
                };
            }
            // ── Resolve render mode: single enum replaces boolean dispatch soup ──
            let gpu_ready = GpuReadiness {
                wgsl_pipelines_ready: gpu.pipelines.wgsl_pipelines_ready,
            };
            // A — Live Hybrid while SDF editing in PathTrace (toggleable; flag precomputed above).
            let rm_inputs = RenderModeInputs {
                shading_mode: self.render.shading_mode,
                pt_instance_count: self.cache.pt_instance_count, // triangle-PT gate: assembled TLAS instances
                unified_raster: true, // Object Mode is the only mode → no-march AnalyticalLite for every non-PT shading mode (PT arm outranks this guard — it writes HDR only, no G-buffer fusion)
                draft_compile: draft_compile || ((self.interaction_episode.active || self.interaction_episode.cooldown_remaining > 0)
                    && self.interaction_episode.render_mode_latch != RenderMode::PathTrace
                    && self.render.shading_mode == ShadingMode::PathTrace),
                pt_live_hybrid_feedback: pt_live_hybrid,
                // Flux / Neural Beauty owns the GPU while running — pause PT to free VRAM.
                ai_gpu_exclusive: self.ai.ai_gpu_exclusive(),
            };
            let render_mode = resolve_render_mode(&rm_inputs, nv, use_lite, interaction_active, &gpu_ready);
            if self.log_render_mode {
                eprintln!("[RenderMode] {:?}", render_mode);
            }
            let t0 = std::time::Instant::now();
            match render_mode {
                RenderMode::PathTrace => {
                    // Owned lease taken BEFORE the timing closure borrows the encoder. `None` =
                    // software traversal this frame (backend latched off, or the native frame
                    // could not be prepared - fails closed to the software BVH, never a skip).
                    let native_lease = if gpu.native_rt_active { enc.dispatch_lease() } else { None };
                    if gpu.native_rt_active && native_lease.is_none() {
                        eprintln!("[RTX] native scene not dispatchable this frame - software traversal");
                    }
                    let mut dispatch_pt = |mut enc: &mut wgpu::CommandEncoder| {
                    // TRIANGLE path trace (Phase 2). Replaces raymarch + deferred + SSR + fog. Its
                    // HDR output flows through the SHARED bloom→tonemap→FXAA chain below (the sole
                    // HDR→LDR route — skipping it left the screen on a stale LDR frame, kill K2 in
                    // PATHTRACER_PLAN). Traces the raster triangles via TLAS/BLAS — the analytical
                    // SDF is never marched here (owner ban 2026-07-16).
                    // B3: half→full — seed full accum/HDR from half_accum before PT reads history.
                    // Skip cam reproject this frame when seeding (would fight the seed).
                    // C — Soft Hybrid→PT: copy last Hybrid deferred HDR into progressive history.
                    // Prefer full-res seed; skip when half→full already rewrote accum this frame.
                    if self.render.pt_pending_soft_hybrid_seed {
                        self.render.pt_pending_soft_hybrid_seed = false;
                    }
                    // ── P3.5 temporal reprojection: rewrite pt_accum for the NEW camera before
                    // the PT dispatch reads it. Gather-reproject via THIS frame's raster depth
                    // (gbuf_normal.w — the raster pass already submitted), validated against the
                    // PREVIOUS frame's depth/normals; disoccluded pixels get history 0 and
                    // restart locally inside the PT accumulation blend.
                    // Temporal reproject (full or half lighting) before PT dispatch.
                    if pt_reproject_now {
                        crate::gpu_init::ensure_pt_reproject_pipeline(gpu);
                        let half_reproj = pt_quality.half_res
                            && gpu.pt_half_accum.is_some()
                            && gpu.pt_half_reproj_scratch.is_some();
                        if half_reproj {
                            // Section C: reproject half-res history with full-res depth/normal guides.
                            if let (Some(ref pl), Some(ref bgl), Some(ref params_buf), Some(ref gn), Some(ref prev_nd), Some(ref accum), Some(ref scratch)) = (
                                &gpu.pipelines.pt_reproject_pipeline, &gpu.bind_groups.pt_reproject_bgl,
                                &gpu.buffers.pt_reproj_params_buffer, &gpu.gbuf_normal,
                                &gpu.temporal_prev_normal_dist, &gpu.pt_half_accum, &gpu.pt_half_reproj_scratch)
                            {
                                let (hw, hh) = gpu.pt_half_size;
                                let rp = graphics::PtReprojParams {
                                    prev_cam_pos: [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                                    prev_cam_forward: [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                                    prev_cam_right: [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                                    prev_cam_up: [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                                    cur_cam_pos: gu.cam_pos,
                                    cur_cam_forward: gu.cam_forward,
                                    cur_cam_right: gu.cam_right,
                                    cur_cam_up: gu.cam_up,
                                    screen_size: [hw, hh],
                                    hist_cap: 16.0, // short history while orbiting
                                    depth_reject: 0.08,
                                    full_size: [width, height],
                                    _pad: [0; 2],
                                };
                                queue.write_buffer(params_buf, 0, rp.as_bytes());
                                if gpu.bind_groups.pt_reproject_bg_half.is_none() {
                                    let gn_v = gn.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_v = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
                                    let accum_v = accum.create_view(&wgpu::TextureViewDescriptor::default());
                                    let scratch_v = scratch.create_view(&wgpu::TextureViewDescriptor::default());
                                    gpu.bind_groups.pt_reproject_bg_half = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("pt_reproject_bg_half"), layout: bgl,
                                        entries: &[
                                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&gn_v) },
                                            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&prev_v) },
                                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&accum_v) },
                                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&scratch_v) },
                                            wgpu::BindGroupEntry { binding: 5, resource: gpu.buffers.pt_half_moments_buffer.as_ref().expect("half PT moments allocated with history").as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 6, resource: gpu.buffers.pt_half_reproj_moments_buffer.as_ref().expect("half PT reprojection moments allocated with scratch").as_entire_binding() },
                                        ],
                                    }));
                                }
                                if let Some(ref bg) = gpu.bind_groups.pt_reproject_bg_half {
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pt_reproject_half"), timestamp_writes: None,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((hw + 7) / 8, (hh + 7) / 8, 1);
                                }
                                enc.copy_texture_to_texture(
                                    scratch.as_image_copy(), accum.as_image_copy(),
                                    wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
                                );
                                let source = gpu.buffers.pt_half_reproj_moments_buffer.as_ref().expect("half reprojection moments");
                                let target = gpu.buffers.pt_half_moments_buffer.as_ref().expect("half PT moments");
                                enc.copy_buffer_to_buffer(source, 0, target, 0, target.size());
                            }
                            // Section I: warp half-res ReSTIR reservoirs into the new camera
                            crate::gpu_init::ensure_pt_restir_reproject_pipeline(gpu);
                            if let (Some(ref pl), Some(ref bgl), Some(ref params_buf), Some(ref gn), Some(ref prev_nd), Some(ref prev_r), Some(ref out_r)) = (
                                &gpu.pipelines.pt_restir_reproject_pipeline, &gpu.bind_groups.pt_restir_reproject_bgl,
                                &gpu.buffers.pt_restir_reproj_params_buffer, &gpu.gbuf_normal,
                                &gpu.temporal_prev_normal_dist, &gpu.pt_half_restir_prev, &gpu.pt_half_restir_out)
                            {
                                let (hw, hh) = gpu.pt_half_size;
                                // Params already written for half accum reproject above; rewrite for safety
                                // if half accum reproject was skipped but we still have restir textures.
                                let rp = graphics::PtReprojParams {
                                    prev_cam_pos: [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                                    prev_cam_forward: [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                                    prev_cam_right: [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                                    prev_cam_up: [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                                    cur_cam_pos: gu.cam_pos,
                                    cur_cam_forward: gu.cam_forward,
                                    cur_cam_right: gu.cam_right,
                                    cur_cam_up: gu.cam_up,
                                    screen_size: [hw, hh],
                                    hist_cap: 16.0,
                                    depth_reject: 0.08,
                                    full_size: [width, height],
                                    _pad: [0; 2],
                                };
                                queue.write_buffer(params_buf, 0, rp.as_bytes());
                                if gpu.bind_groups.pt_restir_reproject_bg_half.is_none() {
                                    let gn_v = gn.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_v = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_r_v = prev_r.create_view(&wgpu::TextureViewDescriptor::default());
                                    let out_r_v = out_r.create_view(&wgpu::TextureViewDescriptor::default());
                                    gpu.bind_groups.pt_restir_reproject_bg_half = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("pt_restir_reproject_bg_half"), layout: bgl,
                                        entries: &[
                                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&gn_v) },
                                            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&prev_v) },
                                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&prev_r_v) },
                                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&out_r_v) },
                                        ],
                                    }));
                                }
                                if let Some(ref bg) = gpu.bind_groups.pt_restir_reproject_bg_half {
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pt_restir_reproject_half"), timestamp_writes: None,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((hw + 7) / 8, (hh + 7) / 8, 1);
                                }
                                // Warped reservoirs → prev for PT temporal load
                                enc.copy_texture_to_texture(
                                    out_r.as_image_copy(), prev_r.as_image_copy(),
                                    wgpu::Extent3d { width: hw, height: hh, depth_or_array_layers: 1 },
                                );
                            }
                            // Section R: half-res GI reservoir under camera motion — same defect and fix
                            // as full-res above: the bilinear COLOR reproject corrupts the oct/W/M
                            // reservoir, so INVALIDATE it (clear → M=0) instead of warping it (audit
                            // 2026-07-19). pt_restir_gi re-accumulates GI cleanly after the motion.
                            crate::gpu_init::clear_texture_safe(
                                &mut enc, device.as_ref(), gpu.pt_half_gi_prev.as_ref(),
                            );
                        } else if !pt_quality.half_res {
                            if let (Some(ref pl), Some(ref bgl), Some(ref params_buf), Some(ref gn), Some(ref prev_nd), Some(ref accum), Some(ref scratch)) = (
                                &gpu.pipelines.pt_reproject_pipeline, &gpu.bind_groups.pt_reproject_bgl,
                                &gpu.buffers.pt_reproj_params_buffer, &gpu.gbuf_normal,
                                &gpu.temporal_prev_normal_dist, &gpu.pt_accum_texture, &gpu.pt_reproj_scratch)
                            {
                                let rp = graphics::PtReprojParams {
                                    prev_cam_pos: [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                                    prev_cam_forward: [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                                    prev_cam_right: [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                                    prev_cam_up: [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                                    cur_cam_pos: gu.cam_pos,
                                    cur_cam_forward: gu.cam_forward,
                                    cur_cam_right: gu.cam_right,
                                    cur_cam_up: gu.cam_up,
                                    screen_size: [width, height],
                                    hist_cap: 32.0,
                                    depth_reject: 0.05,
                                    full_size: [width, height],
                                    _pad: [0; 2],
                                };
                                queue.write_buffer(params_buf, 0, rp.as_bytes());
                                if gpu.bind_groups.pt_reproject_bg.is_none() {
                                    let gn_v = gn.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_v = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
                                    let accum_v = accum.create_view(&wgpu::TextureViewDescriptor::default());
                                    let scratch_v = scratch.create_view(&wgpu::TextureViewDescriptor::default());
                                    gpu.bind_groups.pt_reproject_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("pt_reproject_bg"), layout: bgl,
                                        entries: &[
                                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&gn_v) },
                                            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&prev_v) },
                                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&accum_v) },
                                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&scratch_v) },
                                            wgpu::BindGroupEntry { binding: 5, resource: gpu.buffers.pt_moments_buffer.as_ref().expect("PT moments allocated with history").as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 6, resource: gpu.buffers.pt_reproj_moments_buffer.as_ref().expect("PT reprojection moments allocated with scratch").as_entire_binding() },
                                        ],
                                    }));
                                }
                                if let Some(ref bg) = gpu.bind_groups.pt_reproject_bg {
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pt_reproject"), timestamp_writes: None,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                                }
                                enc.copy_texture_to_texture(
                                    scratch.as_image_copy(), accum.as_image_copy(),
                                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                                );
                                let source = gpu.buffers.pt_reproj_moments_buffer.as_ref().expect("reprojection moments");
                                let target = gpu.buffers.pt_moments_buffer.as_ref().expect("PT moments");
                                enc.copy_buffer_to_buffer(source, 0, target, 0, target.size());
                            }
                            // Section I: warp full-res ReSTIR reservoirs into the new camera
                            crate::gpu_init::ensure_pt_restir_reproject_pipeline(gpu);
                            if let (Some(ref pl), Some(ref bgl), Some(ref params_buf), Some(ref gn), Some(ref prev_nd), Some(ref prev_r), Some(ref out_r)) = (
                                &gpu.pipelines.pt_restir_reproject_pipeline, &gpu.bind_groups.pt_restir_reproject_bgl,
                                &gpu.buffers.pt_restir_reproj_params_buffer, &gpu.gbuf_normal,
                                &gpu.temporal_prev_normal_dist, &gpu.pt_restir_prev, &gpu.pt_restir_out)
                            {
                                let rp = graphics::PtReprojParams {
                                    prev_cam_pos: [gpu.temporal_prev_cam[0], gpu.temporal_prev_cam[1], gpu.temporal_prev_cam[2], gpu.temporal_prev_cam[3]],
                                    prev_cam_forward: [gpu.temporal_prev_cam[4], gpu.temporal_prev_cam[5], gpu.temporal_prev_cam[6], gpu.temporal_prev_cam[7]],
                                    prev_cam_right: [gpu.temporal_prev_cam[8], gpu.temporal_prev_cam[9], gpu.temporal_prev_cam[10], gpu.temporal_prev_cam[11]],
                                    prev_cam_up: [gpu.temporal_prev_cam[12], gpu.temporal_prev_cam[13], gpu.temporal_prev_cam[14], gpu.temporal_prev_cam[15]],
                                    cur_cam_pos: gu.cam_pos,
                                    cur_cam_forward: gu.cam_forward,
                                    cur_cam_right: gu.cam_right,
                                    cur_cam_up: gu.cam_up,
                                    screen_size: [width, height],
                                    hist_cap: 20.0,
                                    depth_reject: 0.05,
                                    full_size: [width, height],
                                    _pad: [0; 2],
                                };
                                queue.write_buffer(params_buf, 0, rp.as_bytes());
                                if gpu.bind_groups.pt_restir_reproject_bg.is_none() {
                                    let gn_v = gn.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_v = prev_nd.create_view(&wgpu::TextureViewDescriptor::default());
                                    let prev_r_v = prev_r.create_view(&wgpu::TextureViewDescriptor::default());
                                    let out_r_v = out_r.create_view(&wgpu::TextureViewDescriptor::default());
                                    gpu.bind_groups.pt_restir_reproject_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("pt_restir_reproject_bg"), layout: bgl,
                                        entries: &[
                                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&gn_v) },
                                            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&prev_v) },
                                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&prev_r_v) },
                                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&out_r_v) },
                                        ],
                                    }));
                                }
                                if let Some(ref bg) = gpu.bind_groups.pt_restir_reproject_bg {
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pt_restir_reproject"), timestamp_writes: None,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                                }
                                enc.copy_texture_to_texture(
                                    out_r.as_image_copy(), prev_r.as_image_copy(),
                                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                                );
                            }
                            // Section R: GI reservoir under camera motion.
                            // The GI reservoir packs OCT-ENCODED DIRECTIONS + W/M — it must NOT be warped
                            // with the bilinear COLOR reproject (pt_reproject): component-wise averaging of
                            // neighbours' octahedral directions decodes to an INVALID direction, and W is
                            // averaged as if it were a colour channel → silent GI corruption on orbit
                            // (audit 2026-07-19). The reservoir-aware pt_restir_reproject is DI-specific
                            // (empty = r<0, M in .b), incompatible with GI's packing (oct.x may be <0, M in
                            // .a). So on camera motion we INVALIDATE the GI reservoir (clear → M=0) instead
                            // of warping it: pt_restir_gi then treats it as empty and re-accumulates GI
                            // cleanly — correct, just no temporal reuse ACROSS the motion. A GI-aware
                            // reproject (nearest-tap, oct/W/M packing) can be added later as a quality win.
                            crate::gpu_init::clear_texture_safe(
                                &mut enc, device.as_ref(), gpu.pt_gi_prev.as_ref(),
                            );
                        }
                    }
                    {
                        crate::gpu_init::ensure_pt_stats_buffers(gpu);
                        let use_half = pt_quality.half_res
                            && gpu.pt_half_hdr.is_some()
                            && gpu.pt_half_accum.is_some();
                        let (pt_w, pt_h) = if use_half {
                            gpu.pt_half_size
                        } else {
                            (width, height)
                        };
                        if use_half {
                            // Half-res lighting path (Section B): 25% pixels while moving/settling.
                            if gpu.bind_groups.pt_bg_half.is_none()
                                || gpu.bind_groups.pt_bg_half_generation != gpu.buffers.pt_scene_generation
                                || gpu.bind_groups.pt_bg_half_mat_tex_gen != gpu.material_textures.generation
                            {
                                crate::gpu_init::build_pt_bg_half(gpu);
                            }
                            if let Some(ref stats) = gpu.buffers.pt_stats_buffer {
                                queue.write_buffer(stats, 0, bytemuck::bytes_of(&[0u32, 0u32, 0u32, 0u32]));
                            }
                            // Latched interaction kernel (PT-COLD-WARMUP-BUNDLED): interactive when
                            // installed + adopted, else the full kernel at this half-res budget.
                            let interaction_kernel = if gpu.pt_interactive_active {
                                gpu.pipelines.pt_tri_interactive_pipeline.as_ref()
                            } else {
                                gpu.pipelines.pt_tri_pipeline.as_ref()
                            };
                            if let (Some(bg), Some(ref pl)) = (gpu.bind_groups.pt_bg_half.as_ref(), interaction_kernel) {
                                if let (Some(lease), Some(native)) = (native_lease.as_ref(), gpu.pipelines.native_rt_pipelines.as_ref()) {
                                    lease.encode(enc, native, bg, [pt_w, pt_h], gpu.pt_interactive_active)
                                        .expect("native PT dispatch: nonzero extent and ready lease were checked");
                                } else {
                                    let tsw = None;
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pathtrace_tri_half"),
                                        timestamp_writes: tsw,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((pt_w + 7) / 8, (pt_h + 7) / 8, 1);
                                }
                            }
                            // C3: batch half history (hdr→accum, restir, gi) before denoise
                            crate::gpu_init::copy_pt_history_chain(
                                &mut enc, gpu, true, pt_w, pt_h,
                            );
                            // Section D/J/M/O: variance-guided multi-pass à-trous (half, before upscale)
                            if self.render.pt_denoise {
                                let noisy = f32::from_bits(
                                    gpu.buffers.pt_noisy_fraction_bits
                                        .load(std::sync::atomic::Ordering::Relaxed),
                                );
                                let dq = crate::gpu_init::apply_pt_denoise_frame_budget(
                                    crate::gpu_init::resolve_pt_denoise_quality(
                                        self.render.denoise_strength,
                                        noisy,
                                        true, // half_res interactive
                                        self.render.pt_accumulated_frames,
                                    ),
                                    self.render.pt_last_frame_ms,
                                    self.render.pt_fast_mode,
                                );
                                crate::gpu_init::dispatch_pt_denoise_atrous(
                                    &mut enc,
                                    queue.as_ref(),
                                    gpu,
                                    dq,
                                    pt_w,
                                    pt_h,
                                    2, // full gbuf guides
                                    true, // half_res
                                    self.render.pt_denoise_end_on_hdr,
                                );
                            }
                            // Section E: depth-aware upscale (+ light sharpen) → full HDR
                            if let (Some(ref half_hdr), Some(ref gn), Some(ref full_hdr), Some(ref bgl), Some(ref pl), Some(ref params_buf)) = (
                                &gpu.pt_half_hdr,
                                &gpu.gbuf_normal,
                                &gpu.hdr_texture,
                                &gpu.bind_groups.pt_upscale_bgl,
                                &gpu.pipelines.pt_upscale_pipeline,
                                &gpu.bind_groups.pt_upscale_params_buffer,
                            ) {
                                let params = [width, height, pt_w, pt_h];
                                queue.write_buffer(params_buf, 0, bytemuck::cast_slice(&params));
                                if gpu.bind_groups.pt_upscale_bg.is_none() {
                                    let half_v = half_hdr.create_view(&wgpu::TextureViewDescriptor::default());
                                    let gn_v = gn.create_view(&wgpu::TextureViewDescriptor::default());
                                    let hdr_v = full_hdr.create_view(&wgpu::TextureViewDescriptor::default());
                                    gpu.bind_groups.pt_upscale_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("pt_upscale_bg"),
                                        layout: bgl,
                                        entries: &[
                                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&half_v) },
                                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&gn_v) },
                                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&hdr_v) },
                                            wgpu::BindGroupEntry { binding: 3, resource: params_buf.as_entire_binding() },
                                        ],
                                    }));
                                }
                                if let Some(ref bg) = gpu.bind_groups.pt_upscale_bg {
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pt_upscale"),
                                        timestamp_writes: None,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                                }
                            }
                        } else {
                            // Full-res progressive path (idle refine)
                            if gpu.bind_groups.pt_bg.is_none()
                                || gpu.bind_groups.pt_bg_generation != gpu.buffers.pt_scene_generation
                                || gpu.bind_groups.pt_bg_mat_tex_gen != gpu.material_textures.generation
                            {
                                crate::gpu_init::build_pt_bg(gpu);
                            }
                            if let Some(ref stats) = gpu.buffers.pt_stats_buffer {
                                queue.write_buffer(stats, 0, bytemuck::bytes_of(&[0u32, 0u32, 0u32, 0u32]));
                            }
                            if let (Some(bg), Some(ref pl)) =
                                (gpu.bind_groups.pt_bg.as_ref(), gpu.pipelines.pt_tri_pipeline.as_ref())
                            {
                                if let (Some(lease), Some(native)) = (native_lease.as_ref(), gpu.pipelines.native_rt_pipelines.as_ref()) {
                                    lease.encode(enc, native, bg, [width, height], false)
                                        .expect("native PT dispatch: nonzero extent and ready lease were checked");
                                } else {
                                    let tsw = None;
                                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("pathtrace_tri"),
                                        timestamp_writes: tsw,
                                    });
                                    cp.set_pipeline(pl);
                                    cp.set_bind_group(0, bg, &[]);
                                    cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                                }
                            } else {
                                static PT_DISPATCH_WARNED: std::sync::atomic::AtomicBool =
                                    std::sync::atomic::AtomicBool::new(false);
                                if !PT_DISPATCH_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                    eprintln!(
                                        "[PT] PathTrace mode active but pipeline={} bind_group={} — dispatch skipped",
                                        gpu.pipelines.pt_tri_pipeline.is_some(),
                                        gpu.bind_groups.pt_bg.is_some()
                                    );
                                }
                            }
                            // C3: batch full history (hdr→accum, restir, gi)
                            crate::gpu_init::copy_pt_history_chain(
                                &mut enc, gpu, false, width, height,
                            );
                        }
                        // C3: residual stats copy stays adjacent to history chain (same barrier window).
                        // A1′: only if no map outstanding (mapped staging must not receive COPY_DST).
                        let stats_free = !gpu
                            .buffers
                            .pt_stats_map_pending
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if stats_free {
                            if let (Some(ref stats), Some(ref staging)) =
                                (&gpu.buffers.pt_stats_buffer, &gpu.buffers.pt_stats_staging)
                            {
                                enc.copy_buffer_to_buffer(stats, 0, staging, 0, 16);
                                pt_stats_copied = true;
                            }
                        }
                    }
                    // ── P3.5: maintain the depth/camera history in PT mode too (the non-PT
                    // temporal-copy block below is skipped for PathTrace). The raster pass
                    // filled gbuf_normal for THIS camera earlier this frame; next frame's
                    // reproject pass validates against this copy. Encoded AFTER the reproject
                    // dispatch above, which reads the PREVIOUS copy.
                    if let (Some(ref gn), Some(ref prev_nd)) = (&gpu.gbuf_normal, &gpu.temporal_prev_normal_dist) {
                        enc.copy_texture_to_texture(gn.as_image_copy(), prev_nd.as_image_copy(), wgpu::Extent3d{width, height, depth_or_array_layers:1});
                        gpu.temporal_prev_cam = [
                            gu.cam_pos[0], gu.cam_pos[1], gu.cam_pos[2], gu.cam_pos[3],
                            gu.cam_forward[0], gu.cam_forward[1], gu.cam_forward[2], gu.cam_forward[3],
                            gu.cam_right[0], gu.cam_right[1], gu.cam_right[2], gu.cam_right[3],
                            gu.cam_up[0], gu.cam_up[1], gu.cam_up[2], gu.cam_up[3],
                        ];
                        gpu.temporal_frame_valid = true;
                    }
                    // Full-res variance-guided multi-pass à-trous (Section J/M/O).
                    if self.render.pt_denoise && !pt_quality.half_res {
                        let noisy = f32::from_bits(
                            gpu.buffers.pt_noisy_fraction_bits
                                .load(std::sync::atomic::Ordering::Relaxed),
                        );
                        let dq = crate::gpu_init::apply_pt_denoise_frame_budget(
                            crate::gpu_init::resolve_pt_denoise_quality(
                                self.render.denoise_strength,
                                noisy,
                                false,
                                self.render.pt_accumulated_frames,
                            ),
                            self.render.pt_last_frame_ms,
                            self.render.pt_fast_mode,
                        );
                        crate::gpu_init::dispatch_pt_denoise_atrous(
                            &mut enc,
                            queue.as_ref(),
                            gpu,
                            dq,
                            width,
                            height,
                            1,
                            false, // full-res
                            self.render.pt_denoise_end_on_hdr,
                        );
                    }
                    };
                    if let Some(timer) = gpu_ts.as_mut() {
                        timer.time_encoder(gpu_timestamps::PASS_PATH_TRACE, &mut enc, dispatch_pt);
                    } else {
                        dispatch_pt(&mut enc);
                    }
                    self.ui.prof_pass_times[1]=0.0; self.ui.prof_pass_times[2]=0.0; self.ui.prof_pass_times[3]=0.0;
                }
                RenderMode::AnalyticalLite => {
                    // AnalyticalLite is the sole remaining raymarch display pass (floor + background).
                    // The march/full analytical pipelines were REMOVED 2026-07-14; SDF renders from
                    // its baked DC mesh via the raster pass.
                    let (rm_pl, rm_label): (&Option<wgpu::ComputePipeline>, &'static str) =
                        (&gpu.pipelines.pipeline_lite, "raymarch_lite");
                    // Episode jitter detection: track pipeline/resolution switches.
                    // The latch frame resets prev_pipeline_label above, so this only
                    // fires for genuine mid-episode switches (which are real bugs).
                    // Episodes are now driven by user_interacting only (not system_busy),
                    // so spurious episodes from pending_count can no longer occur.
                    if rm_label != self.interaction_episode.prev_pipeline_label {
                        if self.interaction_episode.active {
                            self.interaction_episode.path_switch_count += 1;
                            debug_assert!(false,
                                "Pipeline switch during active episode: {} -> {} (frame={}, start={})",
                                self.interaction_episode.prev_pipeline_label, rm_label,
                                self.diag_frame, self.interaction_episode.start_frame);
                        }
                        self.interaction_episode.prev_pipeline_label = rm_label;
                    }
                    let cur_half = self.interaction_episode.use_half_res;
                    if cur_half != self.interaction_episode.prev_half_res {
                        if self.interaction_episode.active {
                            self.interaction_episode.halfres_transition_count += 1;
                            debug_assert!(false,
                                "Half-res switch during active episode: {} -> {}",
                                self.interaction_episode.prev_half_res, cur_half);
                        }
                        self.interaction_episode.prev_half_res = cur_half;
                    }
                    // Lite uses 1:1 pixel mapping (floor+bg only, no SDF block writes) —
                    // dispatch at full resolution.
                    let (dw, dh) = (width, height);
                    if let (Some(bg), Some(ref pl))=(gpu.bind_groups.bind_group.as_ref(), rm_pl.as_ref()){let tsw=tw!(gpu_timestamps::PASS_RAYMARCH);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some(rm_label),timestamp_writes:tsw});cp.set_pipeline(pl);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((dw+7)/8,(dh+7)/8,1);}
                }
            }
            // ── Sculpt voxel raymarch (Plan B1) ──
            // Runs AFTER the base render-mode pass (which wrote floor/bg) and AFTER the raster pass
            // copied the voxel mesh into raster_depth_in. Raymarches the resident SPARSE atlas and
            // overwrites the G-buffer where the volume surface is closer than the raster mesh/floor.
            // This is an ADDITIVE overlay pass — the triangle mesher still runs (v1 may double-render;
            // gating the mesh off mid-stroke is a follow-up). Isolated to the sculpt atlas only.
            if let Some(params) = sculpt_rm_params {
                let atlas_ready = gpu.voxel_mesh_atlas.is_some()
                    && gpu.pipelines.volume_preview_sculpt_pipeline.is_some();
                if atlas_ready {
                    // Upload SculptParams (write_buffer to the persistent uniform — Frame Ordering OK).
                    queue.write_buffer(&gpu.buffers.sculpt_params_buffer, 0, bytemuck::cast_slice(&params));
                    // Rebuild the bind group every frame: the sparse atlas index/pool buffers can be
                    // recreated between frames (grow / gc / grid swap). Cheap (~0.01ms), zero stale refs.
                    gpu.bind_groups.sculpt_raymarch_bg = None;
                    let raster_depth_ref = gpu.raster_depth_texture.as_ref().unwrap_or(&gpu._raster_depth_dummy);
                    if let (Some(ref gn), Some(ref gm), Some(ref ga), Some(ref sid),
                            Some(ref mot), Some(ref curv), Some(ref emit), Some(atlas))
                        = (&gpu.gbuf_normal, &gpu.gbuf_material, &gpu.gbuf_albedo, &gpu.gbuf_surface_id,
                           &gpu.gbuf_motion, &gpu.gbuf_curvature, &gpu.gbuf_emission, gpu.voxel_mesh_atlas.as_ref()) {
                        gpu.bind_groups.sculpt_raymarch_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor{
                            label:Some("sculpt_raymarch_bg"), layout:&gpu.bind_groups.sculpt_raymarch_bgl, entries:&[
                            wgpu::BindGroupEntry{binding:1,resource:gpu.buffers.uniform_buffer.as_entire_binding()},
                            wgpu::BindGroupEntry{binding:2,resource:atlas.index_buf.as_entire_binding()},
                            wgpu::BindGroupEntry{binding:7,resource:atlas.pool_buf.as_entire_binding()},
                            wgpu::BindGroupEntry{binding:4,resource:gpu.buffers.sculpt_params_buffer.as_entire_binding()},
                            wgpu::BindGroupEntry{binding:9,resource:wgpu::BindingResource::TextureView(&gn.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:10,resource:wgpu::BindingResource::TextureView(&gm.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:11,resource:wgpu::BindingResource::TextureView(&ga.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:29,resource:wgpu::BindingResource::TextureView(&raster_depth_ref.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:30,resource:wgpu::BindingResource::TextureView(&sid.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:32,resource:wgpu::BindingResource::TextureView(&mot.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:34,resource:wgpu::BindingResource::TextureView(&curv.create_view(&wgpu::TextureViewDescriptor::default()))},
                            wgpu::BindGroupEntry{binding:36,resource:wgpu::BindingResource::TextureView(&emit.create_view(&wgpu::TextureViewDescriptor::default()))},
                        ]}));
                    }
                    // Dispatch (timestamp_writes: None — the base pass already consumed PASS_RAYMARCH;
                    // re-marking the same slot would double-write an unresolved query → device loss).
                    if let (Some(bg), Some(ref pl)) = (gpu.bind_groups.sculpt_raymarch_bg.as_ref(), gpu.pipelines.volume_preview_sculpt_pipeline.as_ref()) {
                        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("sculpt_voxel_raymarch"),timestamp_writes:None});
                        cp.set_pipeline(pl); cp.set_bind_group(0,bg,&[]); cp.dispatch_workgroups((width+7)/8,(height+7)/8,1);
                    }
                }
            }
            self.ui.prof_pass_times[0]=t0.elapsed().as_secs_f32()*1000.0;
            // ── Diagnostic: derive pipe label from render_mode ──
            {
                let pipe: &'static str = match render_mode {
                    RenderMode::PathTrace => "pathtrace",
                    RenderMode::AnalyticalLite => "raymarch_lite",
                };
                // Track pipeline switches and half-res transitions for perf HUD
                if !self.perf.prev_pipeline_label.is_empty() && pipe != self.perf.prev_pipeline_label {
                    self.perf.path_switch_accum += 1;
                }
                self.perf.prev_pipeline_label = pipe;
                if use_half_res != self.perf.prev_half_res {
                    self.perf.halfres_transition_accum += 1;
                }
                self.perf.prev_half_res = use_half_res;
                let cur = DiagState {
                    needs_prim_upload,
                    prim_buf_stale: self.cache.prim_buf_stale,
                    is_gizmo: self.tool.current.is_gizmo(),
                    use_lite,
                    nv: nv as u32,
                    interaction: interaction_active,
                    pipeline: pipe,
                };
                if cur != self.diag_prev {
                    self.diag_prev = cur;
                }
            }
            // ── Non-PT post-march passes: temporal copy, deferred, SSR, fog, blend ──
            if render_mode != RenderMode::PathTrace {
                // ── RT projected shadows visibility (RT_SHADOWS_DESIGN Stage 1 + 2 + 3) ──
                // Fire occlusion rays per point/spot light against the triangle-PT BVH (shared
                // PT_TRAVERSAL) → per-light visibility (rt_shadow_vis, deferred binding 40), REPLACING
                // the SDF-only soft_shadow (all-geometry occlusion — the poly/voxel/imported hole).
                // Runs at the TOP of the post-march block — BEFORE the temporal-history copy just below
                // — so the Stage-2 reprojection reads the PREVIOUS frame's temporal_prev_normal_dist +
                // temporal_prev_cam (both overwritten by that copy). The G-buffer is already final
                // (raster + raymarch-lite ran in the march section). Skipped during interaction
                // (deferred suppresses shadows then via interaction_no_shadows) — the SSR/fog latch.
                let rt_interaction_skip = interaction_active && !self.render.interaction_shadows;
                // Progressive-refine AUTO SUN (Phase 2c, owner "light leaking" 2026-08-07):
                // while the accumulator averages, the sun's shadows go ray-traced — leak-
                // free through any wall thickness, no per-light opt-in, no wall thickening.
                let rt_refine_sun = pbr_accumulate_this_frame && self.render.pbr_refine_rt_sun;
                let rt_active = (self.render.rt_shadows || rt_refine_sun)
                    && self.render.shading_mode != ShadingMode::PathTrace
                    && !rt_interaction_skip;
                // Soft penumbra is active only when >1 ray is requested (samples is the global cost
                // knob) AND some light will actually be soft — either the GLOBAL default softness is
                // nonzero (Stage-3 behaviour, all default lights soft) OR a specific light carries its
                // OWN per-light radius (Stage 4: soft independently of the global slider). Otherwise the
                // ray pass collapses to the single hard center ray and the denoise/temporal chain is
                // bypassed so deferred reads the RAW visibility → BIT-IDENTICAL to Stage 1 (anchor).
                // While the accumulator averages, the ray pass runs 1 SAMPLE, FULL-RES
                // (owner "leak-proof breaks when shadows softer" 2026-08-07): the soft
                // half-res 2×2 splat + depth-blind denoise bled visibility across
                // thin-wall silhouettes as penumbras widened. One jittered full-res ray
                // per frame has no splat and no spatial filter to bleed — the main 1/N
                // accumulator is the integrator, and it converges the disc exactly.
                let rt_eff_samples = if pbr_accumulate_this_frame {
                    1
                } else {
                    self.render.rt_shadow_ray_samples
                };
                let rt_penumbra = rt_active
                    && rt_eff_samples > 1
                    && (self.render.rt_shadow_softness > 0.0
                        || self.scene.any_rt_soft_light()
                        // Refine auto sun with a nonzero cone is a soft light too.
                        || (rt_refine_sun && self.render.pbr_refine_sun_angle_deg > 0.0));
                // Temporal accumulation runs whenever the penumbra pass dispatches; its per-frame
                // disc-jitter rotation is animated only when temporal will integrate it (else static).
                let rt_temporal = rt_penumbra;
                if rt_active {
                    // One-time heads-up: the RT visibility buffer packs only 4 lights (RGBA channel =
                    // light index). RT-enabled point/spot lights past the cap fall back to legacy
                    // soft_shadow (texture-array packing is a Stage-4 follow-up — RT_SHADOWS_DESIGN).
                    if self.scene.count_rt_shadow_pointspot_lights() > 8 {
                        static RT_CAP_WARNED: std::sync::Once = std::sync::Once::new();
                        RT_CAP_WARNED.call_once(|| eprintln!(
                            "[RT-SHADOW] more than 8 RT-enabled point/spot lights; deferred only shades 8 and \
                             RT packs 8 (2×RGBA). Extra lights are unshadowed while RT mode is on."));
                    }
                    crate::gpu_init::ensure_rt_shadow_pipeline(gpu); // lazy fallback (normally poll-warmed)
                    if gpu.bind_groups.rt_shadow_bg.is_none()
                        || gpu.bind_groups.rt_shadow_bg_generation != gpu.buffers.pt_scene_generation {
                        crate::gpu_init::build_rt_shadow_bg(gpu);
                    }
                    // Upload the ray-pass params. Bypass values (softness 0 / samples 1) make the
                    // shader take the hard center-ray branch — the Stage-1-identical anchor path.
                    // full_res OFF (default) = soft half-res for viewport perf; ON = quality.
                    let rt_full = rt_penumbra && self.render.rt_shadow_full_res;
                    if let Some(ref pbuf) = gpu.buffers.rt_shadow_params_buffer {
                        // Refined frames run 1 jittered ray with the main accumulator as
                        // the integrator — the frame index must advance on every
                        // accumulating frame (not only when the temporal chain runs).
                        let rt_frame_anim = rt_temporal || pbr_accumulate_this_frame;
                        if rt_frame_anim {
                            self.render.rt_shadow_frame = self.render.rt_shadow_frame.wrapping_add(1);
                        }
                        let params = graphics::RtShadowParams {
                            // Softness passes through while refining (the shader's disc
                            // path runs at n=1 under the accumulate bit) — only the
                            // multi-sample/denoise machinery is bypassed.
                            softness: if rt_penumbra || pbr_accumulate_this_frame { self.render.rt_shadow_softness } else { 0.0 },
                            samples: if rt_penumbra { rt_eff_samples.clamp(1, 16) } else { 1 },
                            // Wrapped: unwrapped counts eventually exceed f32 fract()
                            // precision in the shader's R2 advance (~1e6 frames →
                            // the disc collapses to ~16 rays; every sibling pass
                            // wraps its counter). 4096 ≥ the accumulation cap.
                            frame_index: if rt_frame_anim { self.render.rt_shadow_frame % 4096 } else { 0 },
                            full_res: if rt_full { 1 } else { 0 },
                            sun_tan: if rt_refine_sun {
                                // "Sun Softness (idle)" — the same cone the CSM jitter uses.
                                self.render.pbr_refine_sun_angle_deg.clamp(0.0, 30.0).to_radians().tan()
                            } else {
                                -1.0 // auto sun off
                            },
                            _pad: [0; 3],
                        };
                        gpu.queue.write_buffer(pbuf, 0, params.as_bytes());
                    }
                    if let (Some(bg), Some(ref pl)) =
                        (gpu.bind_groups.rt_shadow_bg.as_ref(), gpu.pipelines.rt_shadow_pipeline.as_ref())
                    {
                        // P0: time only when the pass actually runs (tw! registers active_passes —
                        // never resolve an unused slot). Any-hit rays; penumbra ≤16 samples.
                        let tsw = tw!(gpu_timestamps::PASS_RT_SHADOW);
                        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{
                            label: Some("rt_shadow"),
                            timestamp_writes: tsw,
                        });
                        cp.set_pipeline(pl);
                        cp.set_bind_group(0, bg, &[]);
                        // Soft + half-res (default) → half dispatch (shader 2×2 splat).
                        // Soft + full_res quality toggle → full dispatch so Samples clean the edge.
                        // Hard = full-res always.
                        let (fw, fh) = if rt_penumbra && !rt_full {
                            ((width + 1) / 2, (height + 1) / 2)
                        } else {
                            (width, height)
                        };
                        cp.dispatch_workgroups((fw + 7) / 8, (fh + 7) / 8, 1);
                    }
                    // Stage 2: spatial bilateral + temporal EMA reprojection clean the soft, low-spp
                    // result. Bypassed for the hard anchor path (rt_shadow_vis reaches deferred raw)
                    // AND while the PT scene is not yet resident (no ray bg → rt_shadow_vis is stale;
                    // running the chain would accumulate stale history). It self-heals: once the ray
                    // pass dispatches, the chain resumes with a fresh (invalidated) history.
                    let ray_dispatched = gpu.bind_groups.rt_shadow_bg.is_some()
                        && gpu.pipelines.rt_shadow_pipeline.is_some();
                    if rt_penumbra && ray_dispatched {
                        // E1: promote denoise textures from 1×1 dummies before the chain runs.
                        ensure_rt_denoise_full(gpu, width, height);
                        let cur_cam = [
                            gu.cam_pos[0], gu.cam_pos[1], gu.cam_pos[2], gu.cam_pos[3],
                            gu.cam_forward[0], gu.cam_forward[1], gu.cam_forward[2], gu.cam_forward[3],
                            gu.cam_right[0], gu.cam_right[1], gu.cam_right[2], gu.cam_right[3],
                            gu.cam_up[0], gu.cam_up[1], gu.cam_up[2], gu.cam_up[3],
                        ];
                        let prev_cam = gpu.temporal_prev_cam;
                        // History is only valid if we accumulated into it last frame (rt_shadow_history_valid)
                        // AND a previous frame exists at all (temporal_frame_valid). Either false → the
                        // temporal pass uses the current sample only (resize / RT-toggle / orbit-resume — no pop).
                        let hv = gpu.rt_shadow_history_valid && gpu.temporal_frame_valid;
                        let hv_hi = gpu.rt_shadow_history_valid_hi && gpu.temporal_frame_valid;
                        // D4: fixed EMA α=0.9 only. The count-weighted (1−1/N) branch was
                        // a nested second 1/N inside the progressive-HDR mean (double-
                        // averaged penumbra, and a dead path once PBR refine owns
                        // convergence). Interactive + refine share one boil-resistant α.
                        let (v0, v1) = crate::gpu_init::rt_shadow_denoise_dispatch(
                            gpu, &mut enc, width, height, &prev_cam, &cur_cam, hv, hv_hi,
                            self.render.shadow_blur_radius.max(1),
                            0.9,
                        );
                        gpu.rt_shadow_history_valid = v0;
                        gpu.rt_shadow_history_valid_hi = v1;
                    } else {
                        gpu.rt_shadow_history_valid = false;
                        gpu.rt_shadow_history_valid_hi = false;
                    }
                } else {
                    gpu.rt_shadow_history_valid = false;
                    gpu.rt_shadow_history_valid_hi = false;
                }
                // ── Temporal coherence (L7): copy current gbuf_normal → history for next frame ──
                // Only normal+dist is needed — the reproject pass checks depth validity.
                // Other G-buffer channels persist in the storage textures between frames.
                if let (Some(ref gn), Some(ref prev_nd)) = (&gpu.gbuf_normal, &gpu.temporal_prev_normal_dist) {
                    enc.copy_texture_to_texture(gn.as_image_copy(), prev_nd.as_image_copy(), wgpu::Extent3d{width, height, depth_or_array_layers:1});
                }
                // Store current camera for next frame's temporal comparison
                gpu.temporal_prev_cam = [
                    gu.cam_pos[0], gu.cam_pos[1], gu.cam_pos[2], gu.cam_pos[3],
                    gu.cam_forward[0], gu.cam_forward[1], gu.cam_forward[2], gu.cam_forward[3],
                    gu.cam_right[0], gu.cam_right[1], gu.cam_right[2], gu.cam_right[3],
                    gu.cam_up[0], gu.cam_up[1], gu.cam_up[2], gu.cam_up[3],
                ];
                gpu.temporal_frame_valid = true;
                // ── SSAO (half-res AO factor for raster pixels — PBR/Lit only) ──
                // Encoded AFTER the raymarch-lite G-buffer writes, BEFORE deferred
                // (which samples the result at binding 38). Runs during interaction
                // too — orbit stability is an owner requirement and the 3-pass chain
                // measures ~0.07ms at 1080p (render/src/ssao.rs timing test). When
                // OFF, the dispatches are skipped entirely (Frame Budget) and the
                // factor texture is white-filled ONCE (deferred keeps sampling it
                // unconditionally → exact 2026-07-17d behavior).
                let ssao_active = self.render.ssao_enabled
                    && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit);
                if ssao_active {
                    crate::gpu_init::ensure_ssao_pipelines(gpu); // lazy fallback; normally background-compiled
                    if let (Some(pl), Some(res), Some(gn)) =
                        (gpu.pipelines.ssao.as_ref(), gpu.ssao_res.as_mut(), gpu.gbuf_normal.as_ref())
                    {
                        let radius = self.render.ssao_radius.max(0.01);
                        let ssao_params = render::ssao::SsaoParams {
                            cam_pos: gu.cam_pos,
                            cam_forward: gu.cam_forward,
                            cam_right: gu.cam_right,
                            cam_up: gu.cam_up,
                            full_size: [width, height],
                            half_size: [res.half_size.0, res.half_size.1],
                            radius,
                            strength: self.render.ssao_strength.clamp(0.0, 1.0),
                            bias: render::ssao::SSAO_DEFAULT_BIAS,
                            // Phase 2: animate the IGN rotation ONLY while the refine
                            // accumulator averages (else the pattern would boil).
                            frame_jitter: if pbr_accumulate_this_frame {
                                // 1..=1024, never 0 (0 is the "static pattern"
                                // sentinel — hitting it folded the interactive AO
                                // frame into the mean every 64th frame). 1024-point
                                // sequence: the old %64 capped converged AO at a
                                // 64-point lattice → faint residual crease banding.
                                ((self.render.pbr_refine_frames.wrapping_sub(1) % 1024) + 1) as f32
                            } else {
                                0.0
                            },
                            slices: render::ssao::GTAO_DEFAULT_SLICES,
                            steps: render::ssao::GTAO_DEFAULT_STEPS,
                            thickness: radius, // horizon falloff over one radius (halo guard)
                            _pad2: 0.0,
                        };
                        let ao_mode = if self.render.use_gtao {
                            render::ssao::AoMode::Gtao
                        } else {
                            render::ssao::AoMode::Ssao
                        };
                        let gn_view = gn.create_view(&wgpu::TextureViewDescriptor::default());
                        // tw!() only on the pass that actually dispatches (main; the
                        // blur pair rides untimed). GTAO ~0.16ms at 1080p half-res.
                        let tsw = tw!(gpu_timestamps::PASS_SSAO);
                        render::ssao::dispatch(&mut enc, &device, &queue, pl, res, &gn_view, &ssao_params, ao_mode, tsw, None, None);
                    }
                } else if let Some(res) = gpu.ssao_res.as_mut() {
                    res.ensure_white(&queue); // one-shot on the off-transition
                }

                // (RT projected-shadow chain runs at the TOP of this post-march block — before the
                //  temporal-history copy — so its temporal reprojection reads genuine PREVIOUS-frame
                //  camera + normal. See the "RT projected shadows visibility" block above.)
                // ── Deferred pass ──
                // Object Mode march is AnalyticalLite (floor/bg); idle PBR/Lit/Raytrace want
                // DF-full (GGX + IBL). NEVER sync-compile DF-full on this thread (25–43s freeze
                // on cold cache). Request bg compile; keep lite until `deferred_pipeline` is Some.
                let episode_interaction = if self.interaction_episode.active || self.interaction_episode.cooldown_remaining > 0 { true } else { interaction_active };
                let want_full_pbr = matches!(
                    self.render.shading_mode,
                    ShadingMode::Pbr | ShadingMode::Lit | ShadingMode::Raytrace
                ) && !episode_interaction;
                if want_full_pbr {
                    crate::gpu_init::request_deferred_full_pipeline(gpu);
                }
                let full_ready = gpu.pipelines.deferred_pipeline.is_some();
                let use_full_deferred = want_full_pbr && full_ready;
                let use_lite_deferred = !use_full_deferred;
                let df_pl = if use_full_deferred {
                    gpu.pipelines.deferred_pipeline.as_ref().or(gpu.pipelines.deferred_lite_pipeline.as_ref())
                } else {
                    gpu.pipelines.deferred_lite_pipeline.as_ref().or(gpu.pipelines.deferred_pipeline.as_ref())
                };
                let df_label = if use_full_deferred && gpu.pipelines.deferred_pipeline.is_some() {
                    "deferred"
                } else {
                    "deferred_lite"
                };
                let t0=std::time::Instant::now();
                if let (Some(bg), Some(ref pl))=(gpu.bind_groups.deferred_bg.as_ref(), df_pl){Self::poly_trace("deferred DISPATCH");let tsw=tw!(gpu_timestamps::PASS_DEFERRED);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some(df_label),timestamp_writes:tsw});cp.set_pipeline(pl);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((width+7)/8,(height+7)/8,1);}
                self.ui.prof_pass_times[1]=t0.elapsed().as_secs_f32()*1000.0;
                // Bevel preview: dispatch after deferred shade, writes into HDR
                // S21.10: Tiled dispatch — only cover the screen region containing the bevel AABB
                if self.bevel_pv.active && self.bevel_pv.edge_count > 0 {
                    if let Some(ref bg) = gpu.bind_groups.bevel_preview_bg {
                        let [tx0, ty0] = self.bevel_pv.tile_min;
                        let [tx1, ty1] = self.bevel_pv.tile_max;
                        let tbw = tx1.saturating_sub(tx0);
                        let tbh = ty1.saturating_sub(ty0);
                        if tbw > 0 && tbh > 0 {
                            let tsw = tw!(gpu_timestamps::PASS_BEVEL);
                            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("bevel_preview"),timestamp_writes:tsw});
                            cp.set_pipeline(&gpu.pipelines.bevel_preview_pipeline);
                            cp.set_bind_group(0, bg, &[]);
                            cp.dispatch_workgroups(tbw, tbh, 1);
                        }
                    }
                }
                let t0=std::time::Instant::now();
                // Probe-only mode: dispatch even with the SSR toggle off once a probe is
                // baked (intensity 0 → confidence 0 → pure probe fill; chrome must
                // reflect without the user knowing SSR exists — owner 2026-08-07).
                let ssr_pass_on = (self.render.ssr_enabled
                    // PBR/Lit only — mirrors the bit-4 arm exactly (review 2026-08-07).
                    || (self.render.auto_reflection_probe && self.render.pbr_probe_ready
                        && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit))
                    || (pbr_accumulate_this_frame && self.render.pbr_refine_reflections))
                    && !interaction_active;
                if ssr_pass_on {
                    if let Some(ref bg)=gpu.bind_groups.ssr_bg{let tsw=tw!(gpu_timestamps::PASS_SSR);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("ssr"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.ssr_pipeline);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((width+7)/8,(height+7)/8,1);}
                }
                self.ui.prof_pass_times[2]=if ssr_pass_on{t0.elapsed().as_secs_f32()*1000.0}else{0.0};

                // ── Screen-space GI (SSGI_PLAN.md) ────────────────────────────
                // MUST be encoded AFTER the deferred dispatch, which is what WRITES
                // gpu.hdr_texture: SSGI gathers bounce light out of that lit result. When
                // this block sat above deferred it sampled the PREVIOUS frame's HDR — one
                // frame of lag in the indirect light (crawling under orbit) and undefined
                // memory on the first frame after any resize, since hdr_texture is created
                // in the resize block and not written until deferred runs.
                // Placed after SSR and before fog: fog never writes HDR (it composites in
                // pass_tonemap), so this is still the pre-fog lit result. Gated on shading
                // mode like SSAO — burning the budget in Solid/MatCap/Wireframe for
                // a result the composite throws away would be a defect, not merely
                // waste. When OFF the dispatches are skipped and the delta texture
                // is zero-filled ONCE (the composite adds it unconditionally, so
                // zero is the additive identity — the ensure_white analogue).
                // Progressive Refine implies GI (with per-tech opt-out) — mirrors the
                // bit-128 condition above exactly.
                let ssgi_active = (self.render.ssgi_enabled
                    || (self.render.pbr_progressive_enabled && self.render.pbr_refine_gi))
                    && !self.ssgi_debug.master_kill
                    && matches!(self.render.shading_mode, ShadingMode::Pbr | ShadingMode::Lit);
                let mut ssgi_dispatched = false;
                if ssgi_active {
                    crate::gpu_init::ensure_ssgi_pipelines(gpu);
                    let dbg = self.ssgi_debug;
                    let mut flags = 0u32;
                    if dbg.force_const { flags |= 1; }
                    if dbg.no_env_subtract { flags |= 2; }
                    let rays_base = if dbg.rays_override > 0 { dbg.rays_override } else { self.render.ssgi_rays };
                    // E2 Idle Quality High: double GI rays while refining.
                    let rays = if pbr_accumulate_this_frame && self.render.idle_quality_high {
                        (rays_base * 2).min(64)
                    } else {
                        rays_base
                    };
                    if let (Some(pl), Some(res), Some(gn), Some(gm), Some(ga), Some(rd), Some(hdr_t), Some(ao))
                        = (gpu.pipelines.ssgi.as_ref(), gpu.ssgi_res.as_mut(),
                           gpu.gbuf_normal.as_ref(), gpu.gbuf_material.as_ref(),
                           gpu.gbuf_albedo.as_ref(), gpu.raster_depth_texture.as_ref(),
                           gpu.hdr_texture.as_ref(), gpu.ssao_res.as_ref())
                    {
                        // C2: strength × ready × enabled — same gates as the deferred
                        // probe_sh write. Scales SSGI env-subtraction by (1 − w).
                        let probe_diffuse_w = if self.render.pbr_refine_light_probe
                            && self.render.pbr_probe_ready
                        {
                            self.render.light_probe_strength.clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let params = graphics::SsgiParams {
                            half_size: [res.half_size.0, res.half_size.1],
                            full_size: [width, height],
                            intensity: self.render.ssgi_intensity.max(0.0),
                            radius: self.render.ssgi_radius.max(0.01),
                            env_occlusion: self.render.ssgi_env_occlusion.clamp(0.0, 1.0),
                            rays: rays.clamp(1, 64),
                            debug_flags: flags,
                            dbg_const: dbg.const_value,
                            // Gate 4 temporal arm: rotate the ray pattern per frame only
                            // while the accumulator integrates ("infinite rays" over time).
                            // 1..=4096, never 0 (0 = the static-pattern sentinel;
                            // N=4096 exactly used to collide with it).
                            frame_index: if pbr_accumulate_this_frame {
                                (self.render.pbr_refine_frames.wrapping_sub(1) % 4096) + 1
                            } else {
                                0
                            },
                            probe_diffuse_w,
                        };
                        let v = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
                        // tw! INSIDE the guard that dispatches: marking a slot for a
                        // pass that then does not run resolves unwritten queries and
                        // hangs the GPU. Slot 13 was PASS_BAKE, dead and unreferenced.
                        // NOTE: this times the GATHER only — the blurs cannot share the
                        // pass (usage-scope conflict), so the HUD figure is a lower
                        // bound on the chain. Gate 5 must report it as such.
                        let tsw = tw!(gpu_timestamps::PASS_SSGI);
                        graphics::ssgi::dispatch(
                            &mut enc, &device, &queue, pl, res,
                            &gpu.buffers.uniform_buffer,
                            &v(hdr_t), &v(gn), &v(gm), &v(ga), &v(rd),
                            &gpu.buffers.env_buffer, ao.output_view(),
                            &params, dbg.skip_blur, tsw,
                        );
                        ssgi_dispatched = true;
                    }
                }
                // Zero the delta on EVERY frame we did not actually dispatch — not
                // merely when SSGI is switched off. post_flags bit 128 is set from
                // `ssgi_active` alone, so an enabled-but-undispatched frame (pipeline
                // still compiling, raster_depth or ssao_res absent) would otherwise
                // composite whatever the texture last held. Hanging this off the
                // `else` of the OUTER `if` left that case uncovered, and it only
                // failed to bite because SsgiResources::new happens to zero-fill —
                // an accident, not a structural guarantee (R2).
                if !ssgi_dispatched {
                    if let Some(res) = gpu.ssgi_res.as_mut() {
                        res.ensure_zero(&queue);
                    }
                }
                let t0=std::time::Instant::now();
                // Fog / volumetric pass:
                // - Global fog ON: always (P2 Lite analytic while interacting; full march when idle).
                // - Vol lights only: full pass when idle (god rays); skip while interacting.
                let want_atm = self.render.fog_enabled;
                let want_vol = self.scene.any_volumetric_light();
                let fog_pass_active = want_atm || (want_vol && !interaction_active);
                // Lite: interaction (keep haze while orbiting) OR Draft quality without vol lights.
                let fog_use_lite = want_atm
                    && (interaction_active
                        || (self.render.render_quality == 0 && !want_vol));
                if fog_pass_active {
                    crate::gpu_init::ensure_fog_pipeline(gpu);
                    // If fog_bg was dropped (light realloc) without a full G-buffer rebuild, rebuild it.
                    if gpu.bind_groups.fog_bg.is_none() {
                        crate::gpu_init::rebuild_fog_bg(gpu);
                    }
                    let pl_opt = if fog_use_lite {
                        gpu.pipelines.fog_lite_pipeline.as_ref()
                    } else {
                        gpu.pipelines.fog_pipeline.as_ref()
                    };
                    if let (Some(bg), Some(pl)) = (gpu.bind_groups.fog_bg.as_ref(), pl_opt) {
                        let tsw = tw!(gpu_timestamps::PASS_FOG);
                        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some(if fog_use_lite { "fog_lite" } else { "fog" }),
                            timestamp_writes: tsw,
                        });
                        cp.set_pipeline(pl);
                        cp.set_bind_group(0, bg, &[]);
                        // Lite always half-res; full: Draft/Low/Med half, High/Ultra full.
                        let half = fog_use_lite || self.render.render_quality < 3;
                        let (fw, fh) = if half {
                            ((width + 1) / 2, (height + 1) / 2)
                        } else {
                            (width, height)
                        };
                        cp.dispatch_workgroups((fw + 7) / 8, (fh + 7) / 8, 1);
                    }
                    // Copy current fog → fog_prev for next frame's temporal reprojection (full path).
                    // Lite has no temporal read — still copy so resuming full path has history.
                    if let (Some(ref src), Some(ref dst)) = (&gpu.fog_texture, &gpu.fog_prev_texture) {
                        enc.copy_texture_to_texture(src.as_image_copy(), dst.as_image_copy(), wgpu::Extent3d{width, height, depth_or_array_layers:1});
                    }
                }
                self.ui.prof_pass_times[3]=if fog_pass_active {t0.elapsed().as_secs_f32()*1000.0}else{0.0};
                // === Blend / Overlay Dispatches ===
                // All blend dispatches happen here, after deferred+fog, before bloom.
                // Overlay content is in HDR space — bloom and tonemap process it naturally.
                if self.test_blend_overlay {
                    use crate::blend_dispatch::*;
                    let just_created = ensure_overlay_texture(gpu, width, height);
                    if just_created {
                        // Populate overlay with solid white (f16 1.0 = 0x3C00)
                        if let Some(ref tex) = gpu.overlay_texture {
                            let pixel: [u16; 4] = [0x3C00, 0x3C00, 0x3C00, 0x3C00]; // RGBA16Float 1.0
                            let row_bytes = width as usize * 8; // 4 channels × 2 bytes
                            let data: Vec<u8> = pixel.iter()
                                .flat_map(|v| v.to_le_bytes())
                                .cycle()
                                .take(row_bytes * height as usize)
                                .collect();
                            gpu.queue.write_texture(
                                tex.as_image_copy(),
                                &data,
                                wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(row_bytes as u32), rows_per_image: Some(height) },
                                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                            );
                        }
                    }
                    // In-place blend: copy hdr → temp, blend temp + overlay → hdr
                    if let (Some(ref hdr), Some(ref temp)) = (&gpu.hdr_texture, &gpu.blend_temp_texture) {
                        enc.copy_texture_to_texture(
                            hdr.as_image_copy(), temp.as_image_copy(),
                            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        );
                    }
                    // Phase 1: prepare (mutable — ensures pipeline + uploads params)
                    let params = BlendParams {
                        mode: BlendMode::AlphaOver,
                        tint: [0.2, 0.8, 0.2, 1.0],
                        opacity: 0.2,
                    };
                    let ready = prepare_blend(gpu, &params);
                    // Phase 2: dispatch (immutable — borrows views from gpu)
                    if ready {
                        if let (Some(ref tv), Some(ref ov), Some(ref hdr)) = (&gpu.blend_temp_view, &gpu.overlay_view, &gpu.hdr_texture) {
                            let hdr_v = hdr.create_view(&wgpu::TextureViewDescriptor::default());
                            dispatch_blend(gpu, &mut enc, tv, ov, &hdr_v, width, height);
                        }
                    }
                }
                // Future: debug SDF overlay, etc.
                // === End Blend / Overlay ===
            }
            // === Selected-prim GPU outline (all shading modes including Path Trace) ===
            // Exact per-prim selection contour via prim_outline → MaskOutline composite onto HDR.
            // Gated by Viewport Overlays → Selection Outline (show_sel_rim) — same overall setting
            // as Hybrid mesh rim. Previously only ran in the non-PT post-march block, so PT
            // never showed selection contours.
            if outline_active {
                use crate::blend_dispatch::*;
                crate::gpu_init::ensure_prim_outline_pipeline(gpu);
                ensure_overlay_texture(gpu, width, height);
                // Prefer Selection Outline color (show_sel_rim); fallback sel_color.
                let sc = self.viewport.sel_rim_color;
                let params = BlendParams {
                    mode: BlendMode::MaskOutline,
                    tint: [sc[0], sc[1], sc[2], 1.0],
                    opacity: 1.0,
                };
                let ready = prepare_blend(gpu, &params);
                if ready {
                    if let (
                        Some(ref pl),
                        Some(ref ov),
                        Some(omg),
                        Some(oms),
                        Some(ocb),
                        Some(ocs),
                    ) = (
                        &gpu.pipelines.prim_outline_pipeline,
                        &gpu.overlay_view,
                        &gpu.buffers.outline_mesh_grid_buffer,
                        &gpu.buffers.outline_mesh_start_buffer,
                        &gpu.buffers.outline_clip_curves_buffer,
                        &gpu.buffers.outline_clip_start_buffer,
                    ) {
                        let height_res = gpu.buffers.height_map_buffer.as_ref()
                            .unwrap_or(&gpu.buffers.brick_pool_dummy_buf)
                            .as_entire_binding();
                        let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("prim_outline_bg"),
                            layout: &pl.get_bind_group_layout(0),
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(ov),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: gpu.buffers.uniform_buffer.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 3,
                                    resource: gpu.buffers.prim_buffer.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 5,
                                    resource: omg.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 6,
                                    resource: oms.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 7,
                                    resource: ocb.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 8,
                                    resource: ocs.as_entire_binding(),
                                },
                                // T1: height_maps @15 (sdf_prim_eval) — outline auto-layout includes it.
                                wgpu::BindGroupEntry {
                                    binding: 15,
                                    resource: height_res,
                                },
                            ],
                        });
                        {
                            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                label: Some("prim_outline"),
                                timestamp_writes: None,
                            });
                            cp.set_pipeline(pl);
                            cp.set_bind_group(0, &bg, &[]);
                            cp.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
                        }
                        if let (Some(ref hdr), Some(ref temp)) =
                            (&gpu.hdr_texture, &gpu.blend_temp_texture)
                        {
                            enc.copy_texture_to_texture(
                                hdr.as_image_copy(),
                                temp.as_image_copy(),
                                wgpu::Extent3d {
                                    width,
                                    height,
                                    depth_or_array_layers: 1,
                                },
                            );
                        }
                        if let (Some(ref tv), Some(ref hdr)) =
                            (&gpu.blend_temp_view, &gpu.hdr_texture)
                        {
                            let hdr_v =
                                hdr.create_view(&wgpu::TextureViewDescriptor::default());
                            dispatch_blend(gpu, &mut enc, tv, ov, &hdr_v, width, height);
                        }
                    }
                }
            }
            // HDR→LDR post chain: bloom → tonemap → FXAA. Runs for EVERY render mode — tonemap is
            // the sole writer of the LDR texture egui displays. (Until 2026-07-16 a skip_deferred_pp
            // flag let PathTrace bypass this, which left the screen on a stale LDR frame — kill K2.)
            {
            let t0=std::time::Instant::now();
            if self.render.bloom_enabled && !interaction_active {
                let bw = (width+1)/2; let bh = (height+1)/2;
                if let Some(ref bg)=gpu.bind_groups.bloom_extract_bg{let tsw=tw!(gpu_timestamps::PASS_BLOOM_EXT);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("bloom_ext"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.bloom_extract_pipeline);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((bw+7)/8,(bh+7)/8,1);}
                if let Some(ref bg)=gpu.bind_groups.bloom_blur_h_bg{let tsw=tw!(gpu_timestamps::PASS_BLOOM_H);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("bloom_h"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.bloom_blur_h_pipeline);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((bw+7)/8,(bh+7)/8,1);}
                if let Some(ref bg)=gpu.bind_groups.bloom_blur_v_bg{let tsw=tw!(gpu_timestamps::PASS_BLOOM_V);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("bloom_v"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.bloom_blur_v_pipeline);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((bw+7)/8,(bh+7)/8,1);}
            }
            self.ui.prof_pass_times[4]=if self.render.bloom_enabled && !interaction_active{t0.elapsed().as_secs_f32()*1000.0}else{0.0};
            let t0=std::time::Instant::now();
            // E1: full-res pbr_accum before tonemap writes the mean (1×1 dummies otherwise).
            if pbr_accumulate_this_frame {
                if ensure_pbr_accum_full(gpu, width, height) {
                    // Fresh textures — store-only path until next frame.
                    self.render.pbr_refine_frames = self.render.pbr_refine_frames.min(1);
                }
            }
            if let Some(ref tbg)=gpu.bind_groups.tonemap_bind_group{let tsw=tw!(gpu_timestamps::PASS_TONEMAP);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("tonemap"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.tonemap_pipeline);cp.set_bind_group(0,tbg,&[]);cp.dispatch_workgroups((width+7)/8,(height+7)/8,1);}
            // Progressive PBR refine history chain: tonemap storage-wrote this frame's
            // running mean into pbr_accum_out — copy it to pbr_accum_in for the next
            // frame's read (the copy_pt_history_chain pattern; compute can't
            // read+write one texture in a dispatch).
            if pbr_accumulate_this_frame {
                if let (Some(src), Some(dst)) = (&gpu.pbr_accum_out, &gpu.pbr_accum_in) {
                    enc.copy_texture_to_texture(src.as_image_copy(), dst.as_image_copy(),
                        wgpu::Extent3d{width,height,depth_or_array_layers:1});
                }
            }
            self.ui.prof_pass_times[5]=t0.elapsed().as_secs_f32()*1000.0;
            // --- FXAA pass: after tonemap, before upsample ---
            // Reads LDR (tonemap output), writes to gbuf_albedo (dead after deferred),
            // then copies result back to LDR for upsample/display.
            // Progressive refine: once ≥8 jittered frames are averaged, the edges are
            // already cleaner than FXAA can make them — and FXAA-of-an-average only
            // softens them back. Skip it (it returns untouched when refinement resets).
            let fxaa_superseded = pbr_accumulate_this_frame && self.render.pbr_refine_frames >= 8;
            if self.render.fxaa_enabled && !interaction_active && !fxaa_superseded {
                // Lazily create bind group layout if needed
                if gpu.bind_groups.fxaa_bgl.is_none() {
                    gpu.bind_groups.fxaa_bgl = Some(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{
                        label: Some("fxaa_bgl"),
                        entries: &[
                            wgpu::BindGroupLayoutEntry{binding:0,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:false},view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None},
                            wgpu::BindGroupLayoutEntry{binding:1,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::StorageTexture{access:wgpu::StorageTextureAccess::WriteOnly,format:wgpu::TextureFormat::Rgba16Float,view_dimension:wgpu::TextureViewDimension::D2},count:None},
                        ],
                    }));
                }
                crate::gpu_init::ensure_fxaa_pipeline(gpu);
                if gpu.bind_groups.fxaa_bg.is_none() {
                    if let (Some(ref ldr_tex), Some(ref hdr_tex)) = (&gpu.texture, &gpu.hdr_texture) {
                        let ldr_v = ldr_tex.create_view(&wgpu::TextureViewDescriptor::default());
                        let hdr_v = hdr_tex.create_view(&wgpu::TextureViewDescriptor::default());
                        gpu.bind_groups.fxaa_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor{
                            label:Some("fxaa_bg"), layout:gpu.bind_groups.fxaa_bgl.as_ref().unwrap(),
                            entries:&[
                                wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&ldr_v)},
                                wgpu::BindGroupEntry{binding:1,resource:wgpu::BindingResource::TextureView(&hdr_v)},
                            ],
                        }));
                    }
                }
                if let (Some(ref fxaa_bg), Some(ref fxaa_pl), Some(ref hdr_tex), Some(ref ldr_tex)) = (&gpu.bind_groups.fxaa_bg, &gpu.pipelines.fxaa_pipeline, &gpu.hdr_texture, &gpu.texture) {
                    let tsw = tw!(gpu_timestamps::PASS_FXAA);
                    {let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("fxaa"),timestamp_writes:tsw});cp.set_pipeline(fxaa_pl);cp.set_bind_group(0,fxaa_bg,&[]);cp.dispatch_workgroups((width+7)/8,(height+7)/8,1);}
                    // Copy FXAA result (written to HDR temp) back to LDR for upsample/display
                    enc.copy_texture_to_texture(hdr_tex.as_image_copy(), ldr_tex.as_image_copy(), wgpu::Extent3d{width, height, depth_or_array_layers:1});
                }
            }
            }
            // --- Depth-tested overlay line pass (edit wireframe / object wireframe) ---
            // Raster LINES over raster SOLID sharing ONE depth buffer inside one view — the
            // Display Invariant permits exactly this (no raymarch is fused over these pixels).
            //
            // Runs AFTER tonemap/FXAA (so the wireframe is not tonemapped or blurred) and
            // BEFORE upsample, drawing into the SAME `ldr` texture that is registered as the
            // egui viewport image — i.e. before egui presents it, with no CPU readback.
            //
            // Gate is load-bearing:
            //  - ViewportRenderMode::Hybrid is exactly the condition under which
            //    dispatch_raster_pass ran and filled `raster.depth_view` THIS frame. In FullSdf
            //    nothing rasterized, so there is no depth to test against.
            //  - Tonemap (the SOLE writer of `ldr`) now runs unconditionally above, so these
            //    alpha-blended lines always composite onto a fresh frame — including PathTrace
            //    (until 2026-07-16 PT skipped tonemap and therefore got no GPU wireframe).
            if self.render.viewport_mode == ViewportRenderMode::Hybrid
                && !self.overlay_lines.is_empty() {
                if let (Some(ref ldr_tex), Some(ref dv)) = (&gpu.texture, &gpu.raster.depth_view) {
                    // Depth is sized to the G-buffer; only draw when it matches this pass's
                    // colour target, else wgpu rejects the attachment pair outright.
                    if gpu.raster.depth_size == (width, height) {
                        gpu.line_overlay.upload_params(&queue, width, height, self.render.wire_aa_enabled, px_scale);
                        // `gpu` borrows only self.gpu; self.overlay_lines is a disjoint field.
                        gpu.line_overlay.upload_instances(&device, &queue, &self.overlay_lines);
                        if gpu.line_overlay.bind_group.is_none()
                            || gpu.line_overlay.bind_group_depth_gen != gpu.raster.depth_generation
                        {
                            // camera_buffer is created once and never recreated; the DEPTH
                            // texture (bound as a texture — occlusion is resolved in the
                            // fragment shader, see line_overlay.wgsl) IS recreated on resize
                            // AND on same-size recreates (viewport-mode switch), so the
                            // rebuild keys on depth_generation, never size (the size compare
                            // was the 2026-07-23 mode-switch device-loss regression).
                            // Split borrow: disjoint GpuCtx fields.
                            // NOJITTER camera: overlays draw over the stable accumulated
                            // image — the progressive-refine jitter must not move them.
                            let (lo, ras) = (&mut gpu.line_overlay, &gpu.raster);
                            lo.rebuild_bind_group(&device, &ras.camera_nojitter_buffer, dv, ras.depth_generation);
                        }
                        let ldr_view = ldr_tex.create_view(&Default::default());
                        // No tw!() here: the timestamp system has no slot for this pass, and
                        // marking one that may not dispatch resolves unwritten queries (NVIDIA
                        // device loss).
                        // No depth attachment: the wire pass READS depth as a texture (a
                        // texture cannot be sampled while attached), and never writes it.
                        let mut lp = enc.begin_render_pass(&wgpu::RenderPassDescriptor{
                            label: Some("overlay_lines"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment{
                                view: &ldr_view, resolve_target: None,
                                // Load/Store: composite ON TOP of the tonemapped image.
                                ops: wgpu::Operations{ load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                        let total = self.overlay_lines.len() as u32;
                        let dc = overlay_line_depth_count.min(total);
                        gpu.line_overlay.draw(&mut lp, 0, dc, false);
                        gpu.line_overlay.draw(&mut lp, dc, total - dc, true);
                    }
                }
            }
            // POLY_VIEWPORT_PERF P4: selected-face fill (depth-tested unless X-Ray / !visible_only).
            // Same Hybrid + depth-size gates as the line overlay (shared solid depth).
            if self.render.viewport_mode == ViewportRenderMode::Hybrid
                && !self.overlay_faces.is_empty()
            {
                if let (Some(ref ldr_tex), Some(ref dv)) = (&gpu.texture, &gpu.raster.depth_view) {
                    if gpu.raster.depth_size == (width, height) {
                        gpu.face_overlay.upload_vertices(
                            &device,
                            &queue,
                            &self.overlay_faces,
                        );
                        if gpu.face_overlay.bind_group.is_none()
                            || gpu.face_overlay.bind_group_depth_gen != gpu.raster.depth_generation
                        {
                            let (fo, ras) = (&mut gpu.face_overlay, &gpu.raster);
                            // NOJITTER camera — same rule as the line overlay above.
                            fo.rebuild_bind_group(
                                &device,
                                &ras.camera_nojitter_buffer,
                                dv,
                                ras.depth_generation,
                            );
                        }
                        let ldr_view = ldr_tex.create_view(&Default::default());
                        let mut fp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("overlay_faces"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &ldr_view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                        let count = self.overlay_faces.len() as u32;
                        gpu.face_overlay.draw(&mut fp, count, overlay_face_xray);
                    }
                }
            }
            let t0=std::time::Instant::now();
            if needs_upsample {
                if let Some(ref bg)=gpu.bind_groups.upsample_bg{let tsw=tw!(gpu_timestamps::PASS_UPSAMPLE);let mut cp=enc.begin_compute_pass(&wgpu::ComputePassDescriptor{label:Some("upsample"),timestamp_writes:tsw});cp.set_pipeline(&gpu.pipelines.upsample_pipeline);cp.set_bind_group(0,bg,&[]);cp.dispatch_workgroups((full_w+7)/8,(full_h+7)/8,1);}
            }
            self.ui.prof_pass_times[6]=if needs_upsample{t0.elapsed().as_secs_f32()*1000.0}else{0.0};
            let _t_submit = std::time::Instant::now();
            // Crash diagnostic: breadcrumb before GPU submit — if the process dies
            // between "pre-submit" and "post-submit", the crash is in queue.submit (TDR).
            if self.diag_frame <= 5 {
                let _ = std::fs::write(
                    std::env::temp_dir().join("fourthlayer_gpu_breadcrumb.txt"),
                    format!("frame={} pre-submit pipeline={}", self.diag_frame, self.interaction_episode.prev_pipeline_label),
                );
            }
            enc.submit(gpu);
            if self.diag_frame == 1 || (self.diag_frame <= 5 && crate::perf_spam_on()) {
                let submit_ms = _t_submit.elapsed().as_secs_f32() * 1000.0;
                eprintln!("[Render] frame={} submit={:.1}ms", self.diag_frame, submit_ms);
                let _ = std::fs::write(
                    std::env::temp_dir().join("fourthlayer_gpu_breadcrumb.txt"),
                    format!("frame={} post-submit ok submit_ms={:.1}", self.diag_frame, submit_ms),
                );
            }
            self.ui.cpu_section_ms[3] = _t_submit.elapsed().as_secs_f32() * 1000.0;
            let _ = device.poll(wgpu::PollType::Poll);
            // A1′: residual readback is non-blocking.
            // - Complete any map that became ready (usually previous frame's copy).
            // - If we copied stats this frame, start map_async without Maintain::Wait.
            // Quality/stop already read pt_noisy_fraction_bits (lagged atomic) — unchanged model.
            crate::gpu_init::try_complete_pt_stats_readback(gpu);
            if pt_stats_copied {
                crate::gpu_init::start_pt_stats_readback(gpu);
            }
            // GPU timestamp: resolve queries and initiate readback
            if let Some(ref mut ts) = gpu_ts {
                ts.resolve_and_copy(&device, &queue);
            }
            gpu.gpu_ts = gpu_ts;
            // Screenshot — only if no readback already in flight
            // (stomping a mapped staging buffer freezes / device-loses the GPU).
            // Library kitbash thumbs are ALWAYS placeholder-only (never GPU capture) —
            // that path repeatedly crashed / device-lost under PT+bake.
            let _ = self.library_thumbnail_path.take();
            let _ = self.library_thumbnail_focus_nid.take();
            // VISDIFF harness shots ride the library-thumbnail path (fixed_path, no dialog).
            let lib_thumb: Option<std::path::PathBuf> = self.visdiff_shot.take();
            // Neural Beauty AOV capture (beauty + normal/depth + curvature).
            // Disjoint field borrow: &mut self.ai + &gpu.texture (not &mut self).
            if self.ai.beauty_capture_requested
                && self.ai.pending_beauty_capture.is_none()
                && self.pending_screenshot.is_none()
            {
                if let (Some(beauty), Some(normal)) = (gpu.texture.as_ref(), gpu.gbuf_normal.as_ref()) {
                    let curv = gpu.gbuf_curvature.as_ref();
                    AzraApp::begin_ai_beauty_gpu_capture(
                        &mut self.ai,
                        &device,
                        &queue,
                        beauty,
                        normal,
                        curv,
                        width,
                        height,
                    );
                } else {
                    self.ai.beauty_capture_requested = false;
                    self.ai.beauty_status =
                        Some("\u{274C} Viewport textures not ready \u{2014} wait a frame".into());
                }
            }

            // Never stomp an in-flight screenshot staging buffer (device-loss class +
            // VISDIFF/SOAK can silently re-save the same pixels under a new name).
            // If a capture is already pending, drop this request (visdiff spaces shots
            // far enough that the prior map always completes first).
            let want_user_shot = self.screenshot_requested && self.pending_screenshot.is_none();
            let want_lib_thumb = lib_thumb.is_some() && self.pending_screenshot.is_none();
            let capture = want_user_shot || want_lib_thumb;
            if capture {
                self.screenshot_requested = false;
                if let Some(ref tex) = gpu.texture {
                    // Cap cost: huge viewports (4K+) allocate multi-MB staging — skip GPU thumb
                    // and keep the placeholder already written by the save worker.
                    let px = (width as u64) * (height as u64);
                    let full_res_shot = lib_thumb.is_some() && crate::visdiff::harness_full_res_shots();
                    if lib_thumb.is_some() && !full_res_shot && px > 2_500_000 {
                        eprintln!(
                            "[Library] skip GPU thumb (viewport {width}x{height} too large) — placeholder kept"
                        );
                    } else {
                        // gpu.texture (ldr) is Rgba16Float = 8 bytes/pixel — bpr and the
                        // staging size must use 8, and the read side decodes f16 → u8.
                        // (4 B/px here under-sized the copy → wgpu validation panic.)
                        let bpr = (8 * width + 255) & !255;
                        let staging = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("screenshot_staging"),
                            size: bpr as u64 * height as u64,
                            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                            mapped_at_creation: false,
                        });
                        let mut enc2 = device.create_command_encoder(
                            &wgpu::CommandEncoderDescriptor {
                                label: Some("screenshot_enc"),
                            },
                        );
                        enc2.copy_texture_to_buffer(
                            tex.as_image_copy(),
                            wgpu::ImageCopyBuffer {
                                buffer: &staging,
                                layout: wgpu::ImageDataLayout {
                                    offset: 0,
                                    bytes_per_row: Some(bpr),
                                    rows_per_image: Some(height),
                                },
                            },
                            wgpu::Extent3d {
                                width,
                                height,
                                depth_or_array_layers: 1,
                            },
                        );
                        queue.submit(Some(enc2.finish()));
                        let slice = staging.slice(..);
                        let (tx, rx) = std::sync::mpsc::channel();
                        slice.map_async(wgpu::MapMode::Read, move |result| {
                            let _ = tx.send(result);
                        });
                        self.pending_screenshot = Some(PendingScreenshot {
                            staging,
                            rx,
                            width,
                            height,
                            bpr,
                            fixed_path: lib_thumb,
                            // 512 for VISDIFF pixel-diff fidelity; 0 = full-res uncropped (AZRA_SHOT_FULLRES).
                            thumb_size: if full_res_shot { 0 } else { 512 },
                        });
                    }
                }
            }
            // Cache current camera for next frame's temporal reprojection
            let gu = cfg.to_gpu_uniforms();
            self.prev_cam_uniforms = graphics::PrevCamera { cam_pos: gu.cam_pos, cam_forward: gu.cam_forward, cam_right: gu.cam_right, cam_up: gu.cam_up };
            self.render.fog_frame_index = self.render.fog_frame_index.wrapping_add(1);
            let frame_ms = frame_t0.elapsed().as_secs_f32()*1000.0;
            if self.ui.prof_frame_times.len()>=300{self.ui.prof_frame_times.pop_front();}
            self.ui.prof_frame_times.push_back(frame_ms);
            self.ui.prof_render_res = (width, height);
            // GPU memory report — throttled to once per second
            let should_update_mem = self.ui.prof_gpu_mem_last_update
                .map_or(true, |t| t.elapsed().as_secs_f32() > 1.0);
            if should_update_mem {
                let px = (width as f64) * (height as f64);
                let bpp_f16 = 8.0; let bpp_u8 = 4.0;
                // Render textures: hdr, gbuf_normal, gbuf_material, gbuf_albedo, ssr, fog, fog_prev, pt_accum
                // + compositor gbuf: surface_id (4B), motion (8B), curvature (8B), emission (8B)
                let tex_bytes = px * (bpp_f16 * 8.0 + bpp_u8 * 2.0 + 4.0 + 8.0 * 3.0)
                    + (width as f64 / 2.0) * (height as f64 / 2.0) * bpp_f16 * 2.0 // bloom_a + bloom_b
                    + if needs_upsample { full_w as f64 * full_h as f64 * bpp_u8 } else { 0.0 };
                // Shadow maps: 3 cascades × 2048×2048 × depth32 (4 bytes)
                let shadow_bytes = 3.0 * 2048.0 * 2048.0 * 4.0;
                // Buffers: prim_buffer, uniform_buffer, light_buffer, compositor_buffer, etc.
                let buf_bytes = gpu.buffers.prim_buffer.size() as f64
                    + gpu.buffers.uniform_buffer.size() as f64
                    + gpu.buffers.light_buffer.size() as f64
                    + gpu.buffers.compositor_buffer.size() as f64
                    + gpu.buffers.light_link_buffer.size() as f64
                    + gpu.buffers.env_buffer.size() as f64
                    + gpu.buffers.matcap_buffer.size() as f64
                    + gpu.buffers.tex_buffer.size() as f64
                    + gpu.buffers.tex_atlas_buffer.size() as f64
                    + gpu.buffers.tex_meta_buffer.size() as f64
                    + gpu.buffers.bevel_edge_buffer.size() as f64
                    + gpu.buffers.bevel_params_buffer.size() as f64
                    + gpu.buffers.bevel_grid_cells_buffer.size() as f64
                    + gpu.buffers.bevel_grid_indices_buffer.size() as f64
                    + gpu.buffers.fog_prev_camera_buffer.size() as f64;
                let mb = 1024.0 * 1024.0;
                self.ui.prof_gpu_mem_textures_mb = (tex_bytes / mb) as f32;
                self.ui.prof_gpu_mem_buffers_mb = (buf_bytes / mb) as f32;
                self.ui.prof_gpu_mem_shadow_mb = (shadow_bytes / mb) as f32;
                self.ui.prof_gpu_mem_total_mb = ((tex_bytes + shadow_bytes + buf_bytes) / mb) as f32;
                self.ui.prof_gpu_mem_mb = self.ui.prof_gpu_mem_total_mb;
                self.ui.prof_gpu_mem_last_update = Some(std::time::Instant::now());
            }
        }
        let total_disp = _spike_t0.elapsed().as_secs_f32() * 1000.0;
        if self.ui.show_perf_hud { self.perf.dispatch_gpu_ms = total_disp as f64; }
        // Rest of Hybrid path after raster (deferred / post / tonemap / submit of compute enc).
        self.perf.dispatch_rest_ms = (total_disp - self.perf.raster_ms).max(0.0);
        // Scoreboard drill-down: when the post-raster remainder is heavy, print the buckets we
        // already time so a dRest spike is attributable without a profiler attach.
        // 10 ms + AZRA_SCOREBOARD was too high AND too opt-in to catch the real case: the owner's
        // edit frames ran `dispatch=11.1(submit=0.1)` with `dRest=6.4`, so the breakdown never
        // printed and an 11 ms CPU-side dispatch stayed anonymous. `submit=0.1` and `gpu_ms≈1.6`
        // mean this is CPU work inside the dispatch, not the GPU being slow — exactly what these
        // buckets exist to name. Same reasoning as dropping the `[STALL]` bar during interaction:
        // 4 ms of a 16 ms budget is worth attributing, and healthy frames still print nothing.
        let drest_interesting = self.perf.dispatch_rest_ms > 4.0
            || (self.perf.dispatch_rest_ms + self.perf.raster_ms) > 8.0;
        if drest_interesting
            && (std::env::var_os("AZRA_SCOREBOARD").is_some() || crate::perf_spam_on())
        {
            let p = &self.ui.prof_pass_times;
            eprintln!(
                "[DREST] dispatch={:.1} (raster={:.1} rest={:.1}) ser={:.2} ovl={:.2}                  p0={:.2} p1={:.2} p2={:.2} p3={:.2} p4={:.2} p5={:.2} p6={:.2} submit={:.2}",
                self.perf.dispatch_rest_ms + self.perf.raster_ms, self.perf.raster_ms,
                self.perf.dispatch_rest_ms, self.perf.serialize_graph_ms, ovl_ms,
                p[0], p[1], p[2], p[3], p[4], p[5], p[6], self.ui.cpu_section_ms[3],
            );
        }
        self.is_dirty=false;
    }

    /// Rebuild tonemap bind group after pbr_accum (re)creation. Fail-closed if any
    /// required texture is missing (caller only runs mid-frame with them resident).
    fn rebuild_tonemap_bg(gpu: &mut crate::gpu_ctx::GpuCtx) {
        let (
            Some(hdr),
            Some(ldr),
            Some(ba),
            Some(ssr),
            Some(fog),
            Some(gn),
            Some(ga),
            Some(sid),
            Some(mot),
            Some(curv),
            Some(emit),
            Some(ssgi),
            Some(pbr_in),
            Some(pbr_out),
        ) = (
            gpu.hdr_texture.as_ref(),
            gpu.texture.as_ref(),
            gpu.bloom_a.as_ref(),
            gpu.ssr_texture.as_ref(),
            gpu.fog_texture.as_ref(),
            gpu.gbuf_normal.as_ref(),
            gpu.gbuf_albedo.as_ref(),
            gpu.gbuf_surface_id.as_ref(),
            gpu.gbuf_motion.as_ref(),
            gpu.gbuf_curvature.as_ref(),
            gpu.gbuf_emission.as_ref(),
            gpu.ssgi_res.as_ref(),
            gpu.pbr_accum_in.as_ref(),
            gpu.pbr_accum_out.as_ref(),
        )
        else {
            return;
        };
        let v = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let hdr_v = v(hdr);
        let ldr_v = v(ldr);
        let ba_v = v(ba);
        let ssr_v = v(ssr);
        let fog_v = v(fog);
        let gn_v = v(gn);
        let ga_v = v(ga);
        let sid_v = v(sid);
        let mot_v = v(mot);
        let curv_v = v(curv);
        let emit_v = v(emit);
        let pbr_in_v = v(pbr_in);
        let pbr_out_v = v(pbr_out);
        let ssgi_v = ssgi.output_view();
        gpu.bind_groups.tonemap_bind_group = Some(gpu.device.create_bind_group(
            &wgpu::BindGroupDescriptor {
                label: Some("tonemap_bg"),
                layout: &gpu.bind_groups.tonemap_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&hdr_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&ldr_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&ba_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: gpu.buffers.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 16,
                        resource: wgpu::BindingResource::TextureView(&ssr_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 18,
                        resource: wgpu::BindingResource::TextureView(&fog_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 12,
                        resource: wgpu::BindingResource::TextureView(&gn_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 14,
                        resource: wgpu::BindingResource::TextureView(&ga_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 31,
                        resource: wgpu::BindingResource::TextureView(&sid_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 33,
                        resource: wgpu::BindingResource::TextureView(&mot_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 35,
                        resource: wgpu::BindingResource::TextureView(&curv_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 37,
                        resource: wgpu::BindingResource::TextureView(&emit_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 40,
                        resource: gpu.buffers.compositor_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 62,
                        resource: wgpu::BindingResource::TextureView(ssgi_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 63,
                        resource: wgpu::BindingResource::TextureView(&pbr_in_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 64,
                        resource: wgpu::BindingResource::TextureView(&pbr_out_v),
                    },
                ],
            },
        ));
    }
}

// dispatch_cuda_render moved to cuda_render_dispatch.rs (de-god slice 16, #[cfg(feature="cuda")]).
// update_viewport_mode_auto moved to viewport_render_mode_auto.rs (de-god slice 15).

/// E1: upgrade `pbr_accum_in/out` from 1×1 dummies to full viewport. Returns true if
/// textures were (re)created (caller should keep N≤1 so tonemap doesn't read garbage).
fn ensure_pbr_accum_full(gpu: &mut crate::gpu_ctx::GpuCtx, width: u32, height: u32) -> bool {
    let want = (width.max(1), height.max(1));
    if gpu.pbr_accum_size == want && gpu.pbr_accum_in.is_some() && gpu.pbr_accum_out.is_some() {
        return false;
    }
    let device = gpu.device.clone();
    let extent = wgpu::Extent3d {
        width: want.0,
        height: want.1,
        depth_or_array_layers: 1,
    };
    gpu.pbr_accum_in = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pbr_accum_in"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    }));
    gpu.pbr_accum_out = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pbr_accum_out"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    }));
    gpu.pbr_accum_size = want;
    FourthLayerApp::rebuild_tonemap_bg(gpu);
    true
}

/// E1: upgrade RT denoise/history pairs for soft-penumbra Stage-2 chain.
fn ensure_rt_denoise_full(gpu: &mut crate::gpu_ctx::GpuCtx, width: u32, height: u32) {
    let want = (width.max(1), height.max(1));
    if gpu.rt_denoise_size == want
        && gpu.rt_shadow_denoised.is_some()
        && gpu.rt_shadow_history.is_some()
    {
        return;
    }
    let device = gpu.device.clone();
    let base = wgpu::TextureDescriptor {
        label: Some("rt_denoise"),
        size: wgpu::Extent3d {
            width: want.0,
            height: want.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    gpu.rt_shadow_denoised = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt_shadow_denoised"),
        ..base
    }));
    gpu.rt_shadow_history = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt_shadow_history"),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        ..base
    }));
    gpu.rt_shadow_denoised_hi = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt_shadow_denoised_hi"),
        ..base
    }));
    gpu.rt_shadow_history_hi = Some(device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt_shadow_history_hi"),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        ..base
    }));
    gpu.rt_denoise_size = want;
    gpu.bind_groups.rt_denoise_bg = None;
    gpu.bind_groups.rt_denoise_bg_hi = None;
    gpu.bind_groups.rt_temporal_bg = None;
    gpu.bind_groups.rt_temporal_bg_hi = None;
    gpu.rt_shadow_history_valid = false;
    gpu.rt_shadow_history_valid_hi = false;
}
