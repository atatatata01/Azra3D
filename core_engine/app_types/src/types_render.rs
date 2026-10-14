// Render Settings — extracted from types.rs
use eframe::egui;
use glam::{Quat, Vec3};
use crate::*;

/// Computed once per frame. Determines EXACTLY which GPU dispatch path fires.
/// Only ONE variant is active — dispatches are mutually exclusive by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Path tracing — replaces all other render paths including deferred.
    PathTrace,
    // VolumePreview REMOVED 2026-07-13 (owner-directed): the legacy SDF volume-raymarch
    // display path is stripped — the DC mesh (Object-Mode raster) is the sole SDF display.
    // (GlslBvh / AnalyticalMarch / AnalyticalFull REMOVED 2026-07-14: the analytical SDF
    //  march display path is stripped. SDF renders from its baked DC mesh via the raster pass.)
    /// WGSL analytical — lite: floor + background only. The sole remaining raymarch display mode.
    AnalyticalLite,
}

// ==================================================================
// Render Settings (Phase 1 refactor — extracted from FourthLayerApp)
// ==================================================================
pub struct RenderSettings {
    pub exposure: f32,
    pub bloom_enabled: bool,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub ssr_enabled: bool,
    /// SSR march step budget (screen-space DDA steps, UI "Quality"). Runtime-only —
    /// deliberately NOT in the bincode ProjectFile (save-format red line, the
    /// env_rotation_deg precedent).
    pub ssr_max_steps: u32,
    /// SSR blend scale 0..1 (UI "Intensity"): scales hit confidence — 0 = pure
    /// IBL specular, 1 = full traced-reflection swap. Runtime-only.
    pub ssr_intensity: f32,
    /// Viewport tonemap curve (Render panel "Tonemapper" dropdown):
    /// 0 = ACES fitted "Classic" (default — bit-identical to the pre-selector
    /// hardcoded curve), 1 = AgX, 2 = Filmic (Hable/Uncharted 2), 3 = Reinhard,
    /// 4 = PBR Neutral (Khronos), 5 = None/Linear (debug clamp). Runtime-only —
    /// deliberately NOT in the bincode ProjectFile (save-format red line, the
    /// ssr_max_steps precedent).
    pub tonemapper: u32,
    /// Screen-space ambient occlusion (half-res hemisphere + depth-aware blur,
    /// render/src/ssao.rs). Ambient/IBL multiplier for raster pixels in
    /// PBR/Lit ONLY — Solid stays pure form shading (owner ruling), and direct
    /// sun/point light is never multiplied by AO. Default ON.
    pub ssao_enabled: bool,
    /// 0..1 — applied as mix(1, ao, strength) in the AO blur pass.
    pub ssao_strength: f32,
    /// Hemisphere radius in WORLD units (screen footprint scales with distance).
    pub ssao_radius: f32,
    /// AO tier when `ssao_enabled`: true = GTAO (horizon-based, adds bent
    /// normals → directional diffuse-IBL self-shading), false = SSAO (cheaper
    /// hemisphere fallback, no bent normal). Runtime-only (save-format red line).
    /// Default GTAO — the fix for flat IBL-lit surfaces.
    pub use_gtao: bool,
    /// Strength of the hardcoded PROCEDURAL FILL LIGHT (`deferred_entry.wgsl`).
    ///
    /// There is a second, invisible directional light in the PBR/Lit shading: a
    /// fixed direction derived from the key light (roughly opposite in the
    /// horizontal plane and lifted upward), added at 0.2x key intensity. It is not
    /// in the scene graph, has no UI, and until 2026-08-06 could not be turned off.
    ///
    /// It moves the lighting TERMINATOR. The lit/unlit boundary is not where
    /// `dot(N, sun) = 0` but where `dot(N,sun) + 0.2*dot(N,fill) = 0`, so on
    /// hard-surface geometry the shading break does not land on the crease the
    /// modeller expects — the "shadow is in a weird place" report.
    ///
    /// Default 0.2 = exactly the previous behaviour. 0.0 = pure key + ambient,
    /// which is what makes a terminator sit on its edge.
    pub fill_light_strength: f32,
    /// Screen-space one-bounce indirect diffuse (docs/planning/render/SSGI_PLAN.md).
    /// Runtime-only (save-format red line — same class as `use_gtao`). Ships OFF:
    /// a new heavy pass must not change any existing scene's look silently.
    pub ssgi_enabled: bool,
    /// Overall strength of the indirect delta. 1.0 = the energy-conserving result.
    pub ssgi_intensity: f32,
    /// Screen-space march reach in WORLD units.
    pub ssgi_radius: f32,
    /// Cosine-weighted rays per half-res pixel. Fixed per build — never traded
    /// against camera motion (a lighting pop is worse than a cost, §4.5).
    pub ssgi_rays: u32,
    /// **"HDRI Occlusion"** — advanced HDR control (owner request 2026-08-06).
    /// How much environment light SSGI may REMOVE in directions it proved are
    /// blocked. 1.0 = energy-consistent (occluded sky is subtracted, which is what
    /// makes the delta signed: red gained, blue removed beside a red wall).
    /// 0.0 = additive only — bounce added, sky never removed; softer and brighter,
    /// knowingly non-physical, and at 0.0 the result is bit-equal to the no-HDRI
    /// path. Does nothing without an HDRI (`env_width == 0` ⇒ the term is zero),
    /// which is why it belongs with the environment controls, not the SSGI block.
    pub ssgi_env_occlusion: f32,
    pub fog_enabled: bool,
    pub fog_density: f32,
    pub fog_height_falloff: f32,
    pub fog_scatter: f32,
    pub fog_phase_g: f32,
    pub fog_absorption: f32,
    pub fog_noise_scale: f32,
    pub fog_noise_strength: f32,
    pub fog_max_distance: f32,
    pub fog_shadow_steps: f32,
    pub fog_temporal_blend: f32,
    pub fog_frame_index: u32,
    /// Volumetric fog ambient / in-scatter tint (linear RGB). Default cool haze.
    pub fog_color: [f32; 3],
    pub upsample_enabled: bool,
    pub resolution_scale: f32,
    pub render_quality: u32,
    pub shading_mode: ShadingMode,
    pub viewport_mode: ViewportRenderMode,
    // Raytracing
    pub rt_bounces: u32,
    pub rt_shadow_samples: u32,
    pub rt_env_samples: u32,
    pub rt_caustics: bool,
    // Path tracing
    pub pt_max_bounces: u32,
    pub pt_samples_per_frame: u32,
    pub pt_accumulated_frames: u32,
    pub pt_frame_seed: u32,
    pub pt_last_cam_orbit: Quat,
    pub pt_last_cam_dist: f32,
    pub pt_last_cam_target: Vec3,
    pub pt_mouse_idle_secs: f32,
    pub pt_last_mouse_pos: Option<egui::Pos2>,
    pub pt_focus_radius: f32,
    pub pt_dof_aperture: f32,
    pub pt_dof_falloff: f32,
    pub pt_denoise: bool,
    pub denoise_strength: f32,
    pub denoise_radius: u32,
    pub pt_clamp: f32,
    /// Progressive stop mode while the camera is idle:
    /// - `0` Infinite — keep shooting 1 (or few) samples/frame forever until camera moves
    /// - `1` Noise threshold — stop when measured residual ≤ `pt_noise_threshold`
    /// - `2` Max frames — stop after `pt_max_accum_frames` only
    /// Quality comes from many light frames, not one heavy freeze-frame.
    pub pt_stop_mode: u8,
    /// Residual stop (mode 1): continue until estimated noisy-pixel fraction drops below this.
    /// Lower = cleaner / longer. Only used when `pt_stop_mode == 1`.
    pub pt_noise_threshold: f32,
    /// Hard frame cap (mode 2, and safety ceiling for mode 1).
    pub pt_max_accum_frames: u32,
    /// When true: 1 spp while orbiting/dragging, then `pt_samples_per_frame` while idle.
    /// Keep samples/frame small (1–4) so frames stay interactive; refine over time.
    pub pt_adaptive_spp: bool,
    /// Runtime: true once progressive stop condition is met (idle until camera/scene moves).
    pub pt_converged: bool,
    /// Runtime: hash of packed light data + environment params + FOV. Light/env/FOV
    /// edits change radiance without moving orbit/dist/target, so `cam_moved` never
    /// fires — this signature restarts accumulation instead (0 = not yet sampled).
    pub pt_lighting_sig: u64,
    /// Runtime: last frame used half-res PT lighting (Section B). Used to reset accum on scale change.
    pub pt_was_half_res: bool,
    /// C2: EMA of last PathTrace wall-clock frame ms (encode+submit). Runtime only — not saved.
    /// Used to lean spp / bounces / denoise when over budget.
    pub pt_last_frame_ms: f32,
    pub pt_show_spp: bool,
    pub pt_transparent_bg: bool,
    /// P3.5: reproject the PT accumulation under CAMERA motion instead of a
    /// global reset-to-black (content changes still fully reset). Default ON;
    /// Developer Tweaks toggle for owner A/B.
    pub pt_temporal_reproject: bool,
    /// **A — Live Hybrid while SDF editing in PathTrace.** When ON, PathTrace display
    /// yields to Hybrid/raster during an SDF edit episode so partial bakes give live
    /// shape feedback; PT resumes and resets accumulation on settle. Toggleable.
    pub pt_live_hybrid_edit: bool,
    /// **B — Draft mesh into PT while shaping.** When ON, active SDF body pushes mesh
    /// into the PT accel with a shorter settle window so triangle PT updates mid-edit
    /// (noisy, 1-spp-class). When OFF, full quiet/settle (300 ms) before PT mesh update.
    pub pt_live_draft_mesh: bool,
    /// Settle window (ms) for draft mesh (B) on the **active** body. Clamped ≥16.
    /// Inactive bodies always use the full `PT_SNAP_SETTLE` (300 ms).
    pub pt_live_draft_settle_ms: u32,
    /// **C — Soft Hybrid → PT resume.** When ON, exiting live Hybrid seeds `pt_accum`
    /// from the last Hybrid HDR (deferred image) and starts progressive with a short
    /// history weight instead of a black hard reset. Toggleable.
    pub pt_soft_hybrid_resume: bool,
    /// Progressive seed weight after Hybrid exit (1–16). Higher = hold Hybrid color longer.
    pub pt_soft_hybrid_frames: u32,
    /// **D — Draft mesh soft reset.** When ON, TLAS/mesh uploads keep a short progressive
    /// history (variance-aware blend collapses ghosts) instead of frames=0 black restart.
    pub pt_draft_soft_reset: bool,
    /// Soft history frames on draft/scene mesh update (1–8).
    pub pt_draft_soft_frames: u32,
    /// **E — Denoise ends on HDR.** When ON, even step_count uses an inverted pass0
    /// topology (accum→temp first) so the last à-trous pass lands in hdr — no final blit.
    /// When OFF, classic topology + temp→hdr copy (needs COPY_DST on hdr).
    pub pt_denoise_end_on_hdr: bool,
    /// **F — Fast Path Trace.** Prefer FPS over peak quality: leaner spp/bounces,
    /// stronger frame-budget throttle (can force half-res lighting when over budget),
    /// and snappier interactive response. Toggle OFF for production-quality refine.
    pub pt_fast_mode: bool,
    /// Runtime: true last frame when A forced Hybrid feedback (detect exit → PT accum reset).
    pub pt_was_live_hybrid: bool,
    /// Runtime one-shot: next PathTrace frame should seed pt_accum from current HDR
    /// (Hybrid soft resume). Cleared after consume in the PT arm.
    pub pt_pending_soft_hybrid_seed: bool,
    /// One-shot "path tracer compiling" notice latch (runtime only, never saved).
    /// Set when PT is entered while `pt_tri_pipeline` is still cold (raster shows
    /// meanwhile); cleared when the pipeline lands so a later shader-edit recompile
    /// re-notifies. Owner UX: silence looked like a broken PT toggle.
    pub pt_warm_notified: bool,
    // Background
    pub bg_color_top: [f32; 3],
    pub bg_color_bottom: [f32; 3],
    pub ortho: bool,
    pub fov_deg: f32,  // perspective field of view in degrees (10-170, default 90)
    // HDR environment adjustments
    pub env_brightness: f32,
    pub env_contrast: f32,
    pub env_saturation: f32,
    /// Background / sky blur (0 = sharp, 1 = max mip). Used by `sample_env` only —
    /// does NOT soft-focus IBL specular reflections (see `env_reflection_blur`).
    pub env_blur: f32,
    /// Extra mip bias on IBL specular reflections only (0 = roughness-only mips,
    /// 1 = force max blur). Independent of background blur so lookdev can keep
    /// sharp metal reflections while soft-focusing the visible sky.
    /// Runtime-only (not in bincode ProjectFile — same class as env_rotation_deg).
    pub env_reflection_blur: f32,
    /// Mute HDR/IBL without unloading the map. LMB on the shading-bar HDR button.
    /// When false, GPU gets env_width=0 so sky falls back to the solid gradient and
    /// IBL terms drop out; the loaded HDRI + sliders stay resident for instant re-enable.
    pub env_enabled: bool,
    /// Show the HDRI **image** in the viewport background. When false the sky falls
    /// back to the flat `bg_color_top`/`bg_color_bottom` gradient while EVERY HDR
    /// lighting term keeps sampling the map — SH diffuse fill, `sample_env_specular`
    /// reflections, SSR/probe fallback, PT bounce + NEE lighting are untouched.
    /// Lookdev: dark flat plate, real metal. This is camera-ray visibility only
    /// (Cycles' World → Ray Visibility → Camera), NOT a mute — `env_enabled` is the
    /// mute, and it zeroes `env_width` so both the sky AND the lighting drop out.
    /// Reaches the GPU as `post_flags` bit 4096 (set when HIDDEN, so 0 = legacy).
    /// Runtime-only — not in the bincode ProjectFile (same save-format red line as
    /// `env_reflection_blur`; per-project persistence would need an FL4ENVIR bump).
    pub env_bg_visible: bool,
    /// Flat colour the viewport shows in place of the hidden HDRI (linear RGB). Dark grey by
    /// default rather than the near-black `bg_color_top`: the point of hiding the plate is a
    /// calm neutral ground for lookdev, and pure black reads as "renderer broken". Drives the
    /// whole background while hidden — it overrides `bg_color_top`/`bg_color_bottom` on the way
    /// to the GPU (so the gradient goes flat), which is also what keeps the see-through-object
    /// blend and the final composite showing the same colour. Runtime-only, like
    /// `env_bg_visible`.
    pub env_bg_color: [f32; 3],
    /// Environment Y rotation in degrees (rotates IBL + background together).
    /// Runtime-only, like SSAO — deliberately NOT in the bincode ProjectFile
    /// (save-format red line; per-project HDRI persistence is a flagged follow-up).
    pub env_rotation_deg: f32,
    // CUDA backend (feature-gated, no effect without --features cuda)
    pub use_cuda: bool,
    pub cuda_backend: u8, // 0=Ptx, 1=OptiX
    // Wireframe overlay (drawn on top of any shading mode)
    pub wire_overlay_enabled: bool,
    pub wire_overlay_opacity: f32,    // 0.0-1.0 (0 = invisible)
    pub wire_overlay_thickness: f32,  // 0 = off, >0 screen-space px width
    pub wire_overlay_color: [f32; 3], // RGB 0-1 (default black)
    // ── Modifier RESULT wire (the ▦ on a stack card) — its OWN style ──────────────
    // Owner 2026-08-08: "why is my main wireframe 2 colours, needs to be black".
    // It was: this wire had no style of its own and BORROWED `viewport.sel_color`
    // (the selection tint), so with the object wire drawing `wire_overlay_color`
    // (black) at the same time the viewport showed two different wire colours.
    // Defaults to black to match the object wire; right-click the ▦ to change.
    // Runtime-only — NOT in the bincode ProjectFile (same red line as wire_aa_enabled).
    pub mod_wire_color: [f32; 3],
    pub mod_wire_opacity: f32,   // 0.0-1.0
    pub mod_wire_thickness: f32, // 0.5-5.0 px
    pub wire_overlay_backface: bool,  // show back-facing edges too
    // Output dithering (triangular PDF — optional banding fix). Default OFF: texturing
    // Mult stacks read cleaner without film grain; enable from Viewport Overlays if needed.
    pub dither_enabled: bool,
    pub dither_strength: f32,  // multiplier (0.0-2.0, default 0.7)
    // Poly edit wireframe (always-on when in poly context)
    pub poly_wire_color: [f32; 3],    // RGB 0-1
    pub poly_wire_opacity: f32,       // 0.0-1.0
    pub poly_wire_thickness: f32,     // 0.5-5.0 px
    pub poly_wire_xray: bool,         // true = draw ALL edges (full x-ray); false = cull back-facing edges (visible surface)
    // Worker GPU enabled state (persisted per adapter name, applied after enumeration)
    pub worker_gpu_enabled: Vec<(String, bool)>,
    // Matcap settings
    pub matcap_bg_color: [f32; 3],
    pub matcap_bg_gradient: bool,
    pub matcap_bg_gradient_top: [f32; 3],
    pub matcap_rotation: f32,
    pub matcap_edge_enabled: bool,
    pub matcap_edge_color: [f32; 3],
    pub matcap_edge_thickness: f32,
    pub matcap_edge_threshold: f32,
    /// MatCap material-colour overlay: multiply the matcap sample by the surface's own
    /// baked base colour (albedo maps, noise and curvature layers included), so a MatCap
    /// viewport shows the assigned materials instead of one uniform sphere colour.
    /// RUNTIME-ONLY, deliberately NOT in the bincode `ProjectFile` — same call as SSAO /
    /// shadow-map res / SSR quality (adding a field to the in-blob `RenderSettings` is the
    /// save-format field trap; persisting it needs its own FL4 tail section).
    pub matcap_material_tint: bool,
    /// Overlay amount, 0..1 — lerp between the pure matcap and matcap × base colour.
    /// Kept across toggles so unchecking never loses the authored value.
    pub matcap_material_tint_strength: f32,
    // Interaction degradation
    pub interaction_half_res: bool,
    pub interaction_shadows: bool,
    /// Frames since last interaction ended. When < 3, half-res is active.
    pub interaction_idle_frames: u32,
    /// Consecutive frames of active interaction. Half-res only after 3+.
    pub interaction_active_frames: u32,
    // Post-processing
    pub fxaa_enabled: bool,
    pub sdf_edge_aa_enabled: bool,
    /// Analytic anti-aliasing for the overlay wireframe pass (line_overlay.wgsl: 1px
    /// coverage ramp + round caps). The wire pass draws AFTER FXAA at 1spp, so without
    /// this the wires have raw rasterizer staircase edges. Default ON; the toggle exists
    /// for low-end GPUs (AA widens each wire quad by 2px → slightly more fragment work).
    /// Runtime-only — deliberately NOT in the bincode ProjectFile (save-format red line;
    /// same class as env_rotation_deg / SSAO).
    pub wire_aa_enabled: bool,
    /// "Progressive Refine" (PROGRESSIVE_PBR_PLAN.md Phase 1) — idle-only accumulation
    /// for the PBR/Lit Hybrid viewport. While the camera is idle each frame re-renders
    /// with R2 sub-pixel jitter and is averaged 1/N into `pbr_accum` inside
    /// `pass_tonemap`, so edge aliasing (and later Phase-2 stochastic passes) converge
    /// toward reference. Interaction frames are bit-identical to the non-progressive
    /// pipeline (jitter forced 0, accumulate bit off). Default OFF (a new idle GPU
    /// workload must not appear silently — the ssgi_enabled precedent). Runtime-only —
    /// NOT in the bincode ProjectFile (save-format red line).
    pub pbr_progressive_enabled: bool,
    /// E2 (PBR_REFINE_HARDENING): idle-refine cost tier. `false` = Low (no SSGI/SSR
    /// idle doubles — cheaper long refine), `true` = High (current boosts). Runtime-only.
    pub idle_quality_high: bool,
    /// Runtime: frames accumulated in `pbr_accum` (0 = fresh). The PBR twin of
    /// `pt_accumulated_frames` — resets on camera epsilon / lighting+quality signature /
    /// scene-content dirt, NEVER on blanket is_dirty (viewport_render.rs:1144 rule).
    pub pbr_refine_frames: u32,
    /// Runtime: last camera pose folded into the PBR accumulation (epsilon compares —
    /// the pt_last_cam_* precedent; bit-equal compares thrash on quat renormalize).
    pub pbr_refine_last_cam_orbit: Quat,
    pub pbr_refine_last_cam_dist: f32,
    pub pbr_refine_last_cam_target: Vec3,
    /// Runtime: lighting/quality/selection signature guarding the accumulation
    /// (0 = not yet sampled — the pt_lighting_sig precedent).
    pub pbr_refine_sig: u64,
    /// Runtime: last frame's `mesh_cache.pending_count() > 0` — accumulation resets on
    /// the FALLING edge (a job completed → the displayed mesh changed), never on the
    /// level (a job merely pending reset every frame → the owner's "restarting the
    /// loop" 2026-08-07).
    pub pbr_refine_mesh_pending_prev: bool,
    /// Runtime: why the accumulator last reset (settings-panel status line — the
    /// AZRA_REFINE_LOG diagnosis without a terminal). 0 = none yet, 1 = camera,
    /// 2 = scene content, 3 = settings/signature, 4 = interaction/ineligible.
    pub pbr_refine_last_reset: u8,
    /// Runtime: this frame's sub-pixel jitter in NDC units. Applied to the raster proj
    /// (raster_gbuffer) and mirrored into `GpuUniforms.pbr_jitter` for camera_ray
    /// un-jittering. [0,0] whenever not accumulating.
    pub pbr_refine_jitter_ndc: [f32; 2],
    /// Runtime: true exactly on frames the accumulator is folding (the refine block's
    /// per-frame verdict, readable by raster_gbuffer / app_poll which run outside
    /// dispatch_gpu_render's locals). Drives the Phase-2 frame terms (PCF rotation,
    /// SSAO/SSR jitter, RT-shadow 1/N) — all forced to their static values when false.
    pub pbr_refine_active: bool,
    /// Runtime: idle shadow-resolution ladder level (0 = user's base, 1 = 2×, 2 = 4×),
    /// stepped at accumulation milestones (N=32, N=128) and restored to 0 on
    /// interaction. Applied in app_poll (Frame Ordering Contract) as
    /// `min(base << boost, pbr_refine_shadow_res_cap)`. Hashed into the refine sig so
    /// every step restarts accumulation at the new resolution.
    pub pbr_refine_shadow_boost: u32,
    /// "Idle shadow cap" — the ceiling the idle ladder may raise shadow maps to
    /// (2048/4096). E2 dropped the 8192 tier (with Leak-Proof RT sun owning most
    /// refine frames, 4096 + cone-jitter is visually equivalent). Runtime-only.
    pub pbr_refine_shadow_res_cap: u32,
    /// "Sun Softness" (degrees, 0–5, default 1.0): the sun's angular half-radius while
    /// the refine accumulator averages. Each idle frame samples one hard CSM shadow
    /// from a jittered direction inside this cone, so the 1/N mean converges to the
    /// PHYSICAL area-light penumbra of the displayed geometry — wide far from contact,
    /// razor-sharp at contact (the accuracy the fixed-radius PCF blur can't produce).
    /// 0 = keep shadows perfectly hard while refining. Runtime-only setting.
    pub pbr_refine_sun_angle_deg: f32,
    /// "Leak-Proof Sun Shadows" (Phase 2c, owner "light leaking" 2026-08-07): while the
    /// refine accumulator averages, the sun's shadows are RAY-TRACED against the real
    /// triangle scene instead of sampled from the cascaded maps. Rays need no bias, so
    /// thin walls block light exactly — the automated fix for shadow-map light leaking
    /// (the alternative was manually thickening walls). Cone = "Sun Softness (idle)".
    /// Interactive frames keep fast CSM. Runtime-only setting.
    pub pbr_refine_rt_sun: bool,
    /// "Reflections (idle)" — per-tech opt-out inside Progressive Refine: while the
    /// accumulator averages, SSR runs at FULL traced strength (converged by the 1/N
    /// mean — jitter can't boil) even when the standalone SSR toggle is off, so
    /// chrome picks up on-screen geometry, not just the HDRI (owner 2026-08-07).
    /// The baked auto probe covers what the screen can't see. Runtime-only.
    pub pbr_refine_reflections: bool,
    /// "Global Illumination (idle)" — per-tech opt-out inside Progressive Refine
    /// (owner 2026-08-07: "let the user toggle the tech, so I can easily opt out of
    /// GI or RT reflection"). ON (default): Progressive Refine implies SSGI. OFF:
    /// no GI unless the standalone SSGI toggle is on. Runtime-only.
    pub pbr_refine_gi: bool,
    /// "Physical Light Falloff" (2026-08-07 "fix the light"): point/spot lights use
    /// true windowed inverse-square attenuation (post_flags bit 1024, all paths —
    /// deferred, PT, fog). The legacy artistic 1/(1+d²) was ~5× too dim in the
    /// near-field. OFF restores the pre-2026-08-07 look bit-for-bit. Runtime-only.
    pub physical_light_falloff: bool,
    /// Runtime: true while a completed probe bake is being served (mirrors
    /// GpuCtx.probe_baked so CPU gates outside the gpu borrow can read it). Drives the
    /// probe-only SSR-pass dispatch when the SSR toggle is off — a chrome material must
    /// reflect the probe without the user knowing SSR exists (owner 2026-08-07).
    pub pbr_probe_ready: bool,
    /// "Auto Reflection Probe" (PROGRESSIVE_PBR_PLAN.md Phase 3, owner: "I never want
    /// to add reflection probes by hand for now"): one scene-centered cubemap captured
    /// through the real raster+deferred pipeline, one face per idle refine frame, and
    /// consumed by SSR as the fallback where the screen-space march can't see. Baking
    /// requires Progressive Refine (idle scheduling); the baked probe keeps serving
    /// until the scene changes. Runtime-only setting.
    pub auto_reflection_probe: bool,
    /// Auto-reflection-probe cube face edge in texels (128 / 256 / 512).
    ///
    /// This is the SHARPNESS ceiling of every reflection the probe supplies, and the
    /// probe supplies full weight wherever SSR has no confident hit — which is most of
    /// a simple scene. It is NOT affected by `env_reflection_blur`, so at 128 (the
    /// original hardcoded value) reflections read as blurry no matter where that slider
    /// sits. Owner hit exactly that on 2026-08-06.
    ///
    /// VRAM is negligible: 6 faces x N^2 x RGBA16F x ~1.33 for mips
    /// = 1 MB at 128, 4 MB at 256, 17 MB at 512.
    pub probe_face_size: u32,
    /// "Light Probe (idle)" — while Progressive Refine runs, project scene lights into
    /// low-order SH and add as ambient. Off ⇒ strength forced to 0 (no SH ambient).
    /// Runtime-only; pairs with `light_probe_strength`.
    pub pbr_refine_light_probe: bool,
    /// SH ambient strength when `pbr_refine_light_probe` is on (0 = silent).
    pub light_probe_strength: f32,
    pub dof_enabled: bool,
    pub dof_focus_dist: f32,
    pub dof_aperture: f32,
    pub dof_max_blur: f32,
    // Voxel cache
    pub voxel_cache_res: u32,
    // March tuning
    // (bvh_enabled DELETED 2026-07-14 — analytical BVH display path stripped; the GPU
    //  RenderConfig.bvh_enabled uniform slot stays, hardcoded 0 in viewport_render.)
    pub march_bisection_steps: u32,
    pub march_relaxation: f32,
    // Developer Tweaks: raymarch quality (ephemeral — not saved to project)
    pub march_steps: u32,           // march loop iterations (8-128, default 48)
    pub march_resolution: f32,      // march-only resolution scale (0.25-1.0, default 1.0)
    pub march_hit_threshold: f32,   // SDF hit distance (0.0001-0.01, default 0.001)
    // 3-tier LOD system
    pub lod_dist_enabled: bool,
    pub lod_dist_threshold_0: f32,  // full quality above this screen fraction
    pub lod_dist_threshold_1: f32,  // medium above this
    pub lod_dist_threshold_2: f32,  // low above this
    pub lod_min_steps: u32,         // minimum march steps for any LOD
    /// A1 (Assembly) object frustum culling: skip off-screen meshes from the main G-buffer pass
    /// (they still cast shadows). Diagnostics carry last frame's culled / total drawable counts.
    pub frustum_cull: bool,
    pub last_frustum_culled: u32,
    pub last_frustum_total: u32,
    /// Occlusion culling — TWO implementations coexist post-merge (2026-08-01); open
    /// DECISION item: pick one by matrix numbers, delete the other.
    /// 1. CPU AABB Hi-Z (64×36, `AZRA_CPU_OCCLUSION=1`): front-to-back AABB fill with the
    ///    B4 cost/low-yield latch + plane-key invalidation (verified: 100k @ 8.9 ms,
    ///    86% yield amortized). `AZRA_NO_HIZ=1` forces off (scoreboard A/B).
    /// 2. D2 GPU Hi-Z (prev-frame depth pyramid, `render::hiz`): AABB vs 64×36 mip grid;
    ///    orbit-safe via view_proj stamp. Its unlatched cost at 100k was the +8 ms R2/R9
    ///    delta — re-measure with a latch before choosing.
    pub occlusion_cull: bool,
    pub last_occlusion_culled: u32,
    pub last_occlusion_tested: u32,
    /// Frames remaining to skip full Hi-Z (set after a low-yield pass). 0 = run full pass.
    pub occlusion_skip_frames: u32,
    /// Consecutive low-yield full passes (for diagnostics / re-probe cadence).
    pub occlusion_low_yield_streak: u32,
    /// view_proj fingerprint when the skip latch was armed. Any camera/plane change
    /// must force a full Hi-Z pass next frame (cannot freeze 78k cull flags across orbit).
    pub occlusion_latch_plane_key: u64,
    /// P4 / C.6 instancing: draw-call merges for identical MeshCacheId (+ index range).
    /// `last_instance_draws` = total `draw_indexed` issued on the mesh_cache path (main G-buffer).
    /// C.6 multi-mesh gate: N distinct meshes × M instances → draws ≈ N (not N×M).
    /// `last_instance_batches` = multi-instance draws (inst_count > 1).
    /// `last_instance_merged` = extra instances beyond 1 that rode those batches.
    pub last_instance_draws: u32,
    pub last_instance_batches: u32,
    pub last_instance_merged: u32,
    /// C.7: 1 if instance `write_buffer` ran this frame, 0 if content-matched skip.
    pub last_instance_uploads: u32,
    /// D1: batch count uploaded to the GPU cull system on the last structural rebuild.
    /// 0 = cull inputs not resident (capacity miss / empty scene) → CPU cull path only.
    pub cull_batches_ready: u32,
    /// D2: AZRA_HZB_DUMP one-shot latch (diagnostic readback fired).
    pub hzb_dumped: bool,
    /// D2/C5: instances the late pass recovered this frame (disocclusion metric —
    /// >0 while revealing hidden geometry proves the two-pass path is live).
    pub hzb_late_recovered: u32,
    // ── Phase-D live kill switches (Developer Tweaks ▸ Virtual Geometry) ─────────
    // Runtime toggles so a visual artifact can be bisected against each technique
    // WITHOUT restarting. Initial values come from the AZRA_* env vars at startup,
    // so headless harness runs behave exactly as before.
    /// D1 GPU instance culling (off → CPU frustum/occlusion path).
    pub dbg_gpu_cull: bool,
    /// D2 HZB occlusion test in the instance cull (off → frustum only).
    pub dbg_hzb: bool,
    /// D2 two-pass late recovery (off → early-only; disocclusion WILL pop).
    pub dbg_hzb_late: bool,
    /// D4 clustered geometry path (off → source meshes through the normal path).
    pub dbg_clusters: bool,
    /// D4 sabotage: select every cluster regardless of error (L0 everywhere).
    pub dbg_cluster_passthrough: bool,
    /// D4 diagnostic: pin the cut to level 0 (fixed, camera-independent set).
    /// An artifact that survives this is NOT a selection bug.
    pub dbg_cluster_force_l0: bool,
    /// D4 visualization: paint clusters by LOD level (hue) + id (brightness).
    pub dbg_cluster_view: bool,
    /// D4 cut scoreboard: last GPU `selected` count (counters[4], 1–2 frames lag).
    /// **Sum across camera + shadow cascade cuts** — use `cluster_cut_cam_selected` for view.
    pub cluster_cut_selected: u32,
    /// Camera-region only selected count (region counter for cid-slot 0 / pass 0).
    /// This is what multi_draw draws in the G-buffer — not the multi-pass sum.
    pub cluster_cut_cam_selected: u32,
    /// D4 cut scoreboard: last GPU `overflow` count (counters[5]) — dropped args = F-HOLE.
    pub cluster_cut_overflow: u32,
    /// Consecutive counter samples with overflow > 0.
    pub cluster_overflow_streak: u32,
    /// Consecutive clear (overflow==0) samples while sticky MESH fallback is on.
    pub cluster_overflow_clear_streak: u32,
    /// Frames until next CPU cut partition coverage probe (0 = due).
    pub cluster_coverage_probe_age: u32,
    /// Round-robin index into stable_cid_slots for the coverage probe (multi-cid scenes).
    pub cluster_coverage_probe_rr: u32,
    /// WP11 policy: when true, force CLUSTERED → MESH (never silent holes from args cap).
    pub cluster_overflow_mesh_fallback: bool,
    /// C.7: 1 if material `write_buffer` ran this frame, 0 if content-matched skip.
    pub last_material_uploads: u32,
    pub foveated_enabled: bool,
    pub foveated_inner: f32,        // full quality radius from center (0-1)
    pub foveated_outer: f32,        // min quality radius from center (0-1)
    pub foveated_min_steps: u32,    // steps at screen edge
    pub foveated_threshold_mult: f32, // hit_threshold multiplier at edge
    pub foveated_max_pixel_scale: f32, // max block size at edge
    pub lod_debug_colors: bool,     // tint by LOD level
    // Adaptive step count: per-pixel budget scales with distance
    pub march_adaptive_enabled: bool,
    pub march_lod_scale: f32,
    /// Bilateral blur radius on shadow/AO (0=off, 1-3=soft shadows with hard edges)
    pub shadow_blur_radius: u32,
    // Sun shadow quality — the cascaded-shadow-map raster path (render/src/shadows.rs).
    // Runtime-only, like SSAO — deliberately NOT in the bincode ProjectFile (save-format
    // red line). (The old volume_shadow_* block was REMOVED 2026-07-17f: its only shadow
    // consumer was the VP shader, deleted 2026-07-13, and its uniform packing was still
    // stomping the live matcap + fog uniform fields.)
    /// Per-cascade shadow map resolution: 1024 / 2048 / 4096 / 8192. Texture recreation is
    /// applied in poll_background_tasks (Frame Ordering Contract) and generation-counted
    /// (`ShadowMapSystem.generation`). VRAM at D32: 3 cascades × res² × 4 B =
    /// 12 / 48 / 192 / 768 MB.
    pub shadow_map_resolution: u32,
    /// PCF penumbra radius in shadow texels ("Shadow Softness", 0–3). 0 = hard 1-tap;
    /// 0.5 = classic tight 2x2. The receiver anti-acne normal offset is DERIVED from
    /// this (radius + 1 texel) in gbuffer.wgsl — softening never reintroduces acne.
    pub shadow_pcf_softness: f32,
    /// Receiver normal offset in TEXELS, ADDED to the PCF radius ("Normal Offset"
    /// slider). Default 1.0 = the historical hardcoded `pcf_radius + 1`.
    ///
    /// This moves the sample POSITION, so raising it shifts the shadow boundary along
    /// the surface (peter-panning) — that is exactly the "shadow not landing in the
    /// right position" the owner reported on 2026-08-06. Lower it to re-seat the
    /// boundary and fight the resulting acne with the depth biases below instead.
    pub shadow_normal_offset: f32,
    /// Constant receiver depth bias in TEXELS of the selected cascade ("Depth Bias"
    /// slider). Fights acne WITHOUT moving the shadow boundary — prefer this over the
    /// normal offset. Default 0.0 (neutral).
    ///
    /// Texels, not NDC: the cascade ortho window carries ~50 units of caster headroom,
    /// so receivers span only ~0.03 NDC and an NDC slider is unusable (adversarial
    /// review, 2026-08-06). Texels also make the value resolution-independent.
    pub shadow_depth_bias: f32,
    /// Depth bias in TEXELS multiplied by tan(angle between surface and light) ("Slope
    /// Bias" slider). Targets grazing angles, where one shadow texel spans a depth range
    /// growing as tan(theta). Default 0.0 (neutral).
    pub shadow_slope_bias: f32,
    /// Ray-traced projected shadows for point/spot lights (RT_SHADOWS_DESIGN.md).
    /// Global toggle (Stage 1): ON = fire BVH occlusion rays against the triangle
    /// PT accel and feed the deferred point/spot `shd` from an all-geometry
    /// visibility buffer (the coverage hole poly/voxel/imported casters close);
    /// OFF = the legacy SDF-only `soft_shadow` march. The sun stays CSM regardless.
    /// Runtime-only — deliberately NOT in the bincode ProjectFile (save-format
    /// red line, the ssr_max_steps / env_rotation_deg precedent). Default OFF: no
    /// PT-accel residency / VRAM tax until the user asks (RT_SHADOWS_DESIGN §3).
    pub rt_shadows: bool,
    /// RT soft-shadow penumbra width (RT_SHADOWS_DESIGN Stage 3): 0..1 = the light's angular
    /// half-radius as seen from the surface. 0 = hard contact shadow (BIT-IDENTICAL to Stage 1);
    /// higher = wider, contact-hardening penumbra. Default a small nonzero so enabling RT shadows
    /// immediately reads as soft. Runtime-only (save-format red line — the ssr_max_steps precedent).
    pub rt_shadow_softness: f32,
    /// RT soft-shadow rays over the light disc (RT_SHADOWS_DESIGN Stage 3): 1..16. 1 = hard
    /// (single center ray). Higher = smoother penumbra at higher cost. Runtime-only. (Distinct
    /// from the legacy analytical-raytrace `rt_shadow_samples`, which is a different subsystem.)
    pub rt_shadow_ray_samples: u32,
    /// Soft RT at full screen resolution (quality). OFF = half-res 2×2 splat (~4× cheaper,
    /// staircase penumbra — the default for viewport performance). ON = per-pixel disc rays so
    /// **Samples** actually cleans the edge. Runtime-only performance toggle.
    pub rt_shadow_full_res: bool,
    /// Monotonic per-frame counter driving the RT soft-shadow disc-jitter rotation for temporal
    /// supersampling (analogous to fog_frame_index). Runtime-only; reset to 0 on resize.
    pub rt_shadow_frame: u32,
    // Atlas texture resolution (256, 512, 1024). Higher = sharper edges, more VRAM.
    pub atlas_xy_resolution: u32,
    // Volume march optimizations (developer toggles)
    pub volume_early_out: bool,    // skip march for rays that miss all body AABBs
    pub volume_cone_step: bool,    // scale min step by pixel world-size at distance
    // Volume tricubic filtering (Catmull-Rom — makes 128³ look like 256³)
    pub volume_tricubic: bool,
    // SDF ambient occlusion (volume preview only)
    // Computation params (affect raw AO quality):
    pub volume_ao_enabled: bool,
    pub volume_ao_radius: f32,       // first sample distance in voxels
    pub volume_ao_step_scale: f32,   // multiplier between successive steps
    pub volume_ao_samples: u32,      // number of samples (1-8)
    // Artistic params (affect visual result independently):
    pub volume_ao_opacity: f32,      // 0 = invisible, 1 = full strength
    pub volume_ao_intensity: f32,    // raw AO contrast (how aggressively occlusion is detected)
    pub volume_ao_bias: f32,         // sd/d threshold shift (-1..1, negative = more occlusion, positive = less)
    pub volume_ao_color: [f32; 3],   // tint color for occluded areas (default: black)
    pub volume_ao_invert: bool,      // flip: darken convex instead of concave
    pub volume_ao_in_orbit: bool,    // keep AO active during camera orbit
    // force_volume_preview REMOVED 2026-07-13: the SDF volume-raymarch display is stripped.
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            bloom_enabled: false, bloom_threshold: 1.0, bloom_intensity: 0.5,
            ssr_enabled: false,
            ssr_max_steps: 48, ssr_intensity: 1.0,
            tonemapper: 0,
            ssao_enabled: true, ssao_strength: 0.7, ssao_radius: 0.5, use_gtao: true,
            fill_light_strength: 0.2, // 0.2 = the long-standing hardcoded value
            // SSGI ships OFF (the ssr_enabled precedent). Defaults are the
            // production values; HDRI Occlusion 1.0 = energy-consistent.
            // Radius 4.0 (was 1.5, 2026-08-07): the owner's "no second-bounce feel" —
            // 1.5 world units cannot carry a sunlit floor patch up a room's wall;
            // GI reach must be room-scale to read as GI at all.
            ssgi_enabled: false, ssgi_intensity: 1.0, ssgi_radius: 4.0,
            ssgi_rays: 8, ssgi_env_occlusion: 1.0,
            fog_enabled: false, fog_density: 0.03, fog_height_falloff: 0.4, fog_scatter: 0.6,
            // P0 fog defaults: no noise / no SDF shadow march (interactive cube scenes).
            // User can re-enable Shadow Steps + Noise for god-ray look (costs more).
            fog_phase_g: 0.7, fog_absorption: 0.3, fog_noise_scale: 3.0, fog_noise_strength: 0.0,
            fog_max_distance: 30.0, fog_shadow_steps: 0.0,
            fog_temporal_blend: 0.9, fog_frame_index: 0,
            fog_color: [0.35, 0.40, 0.50], // matches former hardcoded fog_ambient
            upsample_enabled: false,
            resolution_scale: 1.0, render_quality: 2,
            shading_mode: ShadingMode::SolidHdr,
            viewport_mode: ViewportRenderMode::Hybrid,
            rt_bounces: 3, rt_shadow_samples: 4, rt_env_samples: 4, rt_caustics: false,
            pt_max_bounces: 6, pt_samples_per_frame: 1, pt_accumulated_frames: 0, pt_frame_seed: 0,
            pt_last_cam_orbit: Quat::IDENTITY, pt_last_cam_dist: 0.0, pt_last_cam_target: Vec3::ZERO,
            pt_mouse_idle_secs: 0.0, pt_last_mouse_pos: None, pt_focus_radius: 0.15,
            // Denoise ON by default for Phase A lookdev speed (bilateral edge-stop).
            pt_dof_aperture: 0.0, pt_dof_falloff: 0.5, pt_denoise: true, denoise_strength: 0.65, denoise_radius: 3, pt_clamp: 10.0,
            // Default: keep refining forever while camera is still (Octane continuous).
            pt_stop_mode: 0,
            pt_noise_threshold: 0.02,
            pt_max_accum_frames: 65536,
            pt_adaptive_spp: true,
            pt_converged: false,
            pt_lighting_sig: 0,
            pt_was_half_res: false,
            pt_last_frame_ms: 0.0,
            pt_show_spp: true, pt_transparent_bg: false, pt_temporal_reproject: true,
            pt_live_hybrid_edit: true,
            pt_live_draft_mesh: true,
            pt_live_draft_settle_ms: 80,
            pt_soft_hybrid_resume: true,
            pt_soft_hybrid_frames: 4,
            pt_draft_soft_reset: true,
            pt_draft_soft_frames: 2,
            pt_denoise_end_on_hdr: true,
            // Default OFF: Fast keeps half-res while orbiting (good FPS) but was easy to
            // confuse with permanent scanline banding when budget demoted full→half.
            pt_fast_mode: false,
            pt_was_live_hybrid: false,
            pt_pending_soft_hybrid_seed: false,
            pt_warm_notified: false,
            bg_color_top: [0.003, 0.003, 0.003],
            bg_color_bottom: [0.003, 0.003, 0.003],
            ortho: false, fov_deg: 90.0,
            env_brightness: 1.0, env_contrast: 1.0, env_saturation: 1.0, env_blur: 0.20,
            env_reflection_blur: 0.0,
            env_enabled: true,
            env_bg_visible: true,
            env_bg_color: [0.14, 0.14, 0.15],
            env_rotation_deg: 0.0,
            use_cuda: false, cuda_backend: 0,
            // Visible cyan on both dark/light lit viewports (was black@0.35 — often invisible).
            wire_overlay_enabled: false, wire_overlay_opacity: 0.65,
            // 0.5 pt ≈ a crisp 1px line after the shader's ×DPI + sub-pixel fade
            // (line_overlay.wgsl:147). The old 1.25 rendered 2–4px on hidpi — owner
            // 2026-08-19: "wire frame is like 5 times too thick by default".
            wire_overlay_thickness: 0.5, wire_overlay_color: [0.0, 0.0, 0.0],
            mod_wire_color: [0.0, 0.0, 0.0], mod_wire_opacity: 0.95, mod_wire_thickness: 1.25,
            wire_overlay_backface: false,
            dither_enabled: false, dither_strength: 0.7,
            poly_wire_color: [0.0, 0.0, 0.0], poly_wire_opacity: 0.6, poly_wire_thickness: 1.2,
            poly_wire_xray: false,
            worker_gpu_enabled: Vec::new(),
            // MatCap viewport BG — flat near-black by default (owner: UI gray picker R2 G2 B2).
            // Optional “UI gray” button still snaps to SURFACE_PANEL (0x1c1c1c).
            matcap_bg_color: [2.0 / 255.0, 2.0 / 255.0, 2.0 / 255.0],
            matcap_bg_gradient: false,
            matcap_bg_gradient_top: [0.14, 0.14, 0.14],
            matcap_rotation: 0.0,
            matcap_edge_enabled: false,
            matcap_edge_color: [0.0, 0.0, 0.0],
            matcap_edge_thickness: 1.0,
            matcap_edge_threshold: 0.3,
            matcap_material_tint: false,
            matcap_material_tint_strength: 1.0,
            // Interaction degradation
            interaction_half_res: false,
            interaction_shadows: false,
            interaction_idle_frames: 3,
            interaction_active_frames: 0,
            // Post-processing
            fxaa_enabled: false,
            sdf_edge_aa_enabled: true,
            wire_aa_enabled: true,
            pbr_progressive_enabled: false,
            idle_quality_high: true,
            pbr_refine_frames: 0,
            pbr_refine_last_cam_orbit: Quat::IDENTITY,
            pbr_refine_last_cam_dist: 0.0,
            pbr_refine_last_cam_target: Vec3::ZERO,
            pbr_refine_sig: 0,
            pbr_refine_last_reset: 0,
            pbr_refine_mesh_pending_prev: false,
            pbr_refine_jitter_ndc: [0.0; 2],
            pbr_refine_active: false,
            pbr_refine_shadow_boost: 0,
            pbr_refine_shadow_res_cap: 4096,
            pbr_refine_sun_angle_deg: 1.0,
            pbr_refine_rt_sun: true,
            pbr_refine_gi: true,
            pbr_refine_reflections: true,
            physical_light_falloff: true,
            pbr_probe_ready: false,
            auto_reflection_probe: true,
            probe_face_size: 256, // was a hardcoded 128 — too soft for the quality bar
            pbr_refine_light_probe: true,
            light_probe_strength: 1.0,

            dof_enabled: false,
            dof_focus_dist: 5.0,
            dof_aperture: 0.05,
            dof_max_blur: 0.01,
            // Voxel cache
            voxel_cache_res: 128,
            // March tuning
            march_bisection_steps: 3,
            march_relaxation: 1.0,
            // Developer Tweaks: raymarch quality (ephemeral)
            march_steps: 48,
            march_resolution: 1.0,
            march_hit_threshold: 0.001,
            // 3-tier LOD
            lod_dist_enabled: false,
            lod_dist_threshold_0: 0.3,
            lod_dist_threshold_1: 0.1,
            lod_dist_threshold_2: 0.03,
            lod_min_steps: 16,
            frustum_cull: true,
            last_frustum_culled: 0,
            last_frustum_total: 0,
            occlusion_cull: true,
            last_occlusion_culled: 0,
            last_occlusion_tested: 0,
            occlusion_skip_frames: 0,
            occlusion_low_yield_streak: 0,
            occlusion_latch_plane_key: 0,
            last_instance_draws: 0,
            cull_batches_ready: 0,
            hzb_dumped: false,
            hzb_late_recovered: 0,
            dbg_gpu_cull: true,
            dbg_hzb: true,
            dbg_hzb_late: true,
            dbg_clusters: false,
            dbg_cluster_passthrough: false,
            dbg_cluster_force_l0: false,
            dbg_cluster_view: false,
            cluster_cut_selected: 0,
            cluster_cut_cam_selected: 0,
            cluster_cut_overflow: 0,
            cluster_overflow_streak: 0,
            cluster_overflow_clear_streak: 0,
            cluster_coverage_probe_age: 0,
            cluster_coverage_probe_rr: 0,
            cluster_overflow_mesh_fallback: false,
            last_instance_batches: 0,
            last_instance_merged: 0,
            last_instance_uploads: 0,
            last_material_uploads: 0,
            foveated_enabled: false,
            foveated_inner: 0.3,
            foveated_outer: 0.9,
            foveated_min_steps: 32,
            foveated_threshold_mult: 10.0,
            foveated_max_pixel_scale: 4.0,
            lod_debug_colors: false,
            march_adaptive_enabled: true,
            march_lod_scale: 1.0,
            shadow_blur_radius: 2, // default: 2px bilateral blur (soft shadows, hard edges)
            shadow_map_resolution: 1024, // default stays 1024 (owner can opt into 2K/4K/8K)
            shadow_pcf_softness: 0.5,    // = the classic 2x2 kernel; slider allows 0 = hard
            // Bias defaults are NEUTRAL — exactly the pre-slider behaviour. Pinned by
            // render::shadows::tests::shadow_bias_defaults_are_neutral.
            shadow_normal_offset: 1.0,
            shadow_depth_bias: 0.0,
            shadow_slope_bias: 0.0,
            rt_shadows: false,           // RT projected shadows OFF by default (no accel/VRAM tax)
            rt_shadow_softness: 0.15,    // small nonzero so enabling RT reads soft immediately (Stage 3)
            rt_shadow_ray_samples: 6,    // 6 disc rays — soft yet cheap; temporal+bilateral clean it
            rt_shadow_full_res: false,   // half-res soft = viewport perf default; ON = quality
            rt_shadow_frame: 0,
            atlas_xy_resolution: 256,
            volume_early_out: true,
            volume_cone_step: true,
            volume_tricubic: true,
            volume_ao_enabled: true,
            volume_ao_radius: 1.5,
            volume_ao_step_scale: 2.0,
            volume_ao_samples: 5,
            volume_ao_opacity: 1.0,
            volume_ao_intensity: 1.5,
            volume_ao_bias: 0.0,
            volume_ao_color: [0.0, 0.0, 0.0],
            volume_ao_invert: false,
            volume_ao_in_orbit: true,
        }
    }
}

/// Developer-only SSGI stage toggles (SSGI_PLAN.md §4.10, binding via R13).
///
/// One struct rather than eight loose fields on `AzraApp`: `main.rs` is under an
/// active god-file split and does not need eight more bools.
///
/// **Every default is the production value**, so a build with `SsgiDebug::default()`
/// is the shipping build — a toggle can only ever make the pass more inspectable,
/// never silently change what ships. Runtime-only; never serialized.
///
/// These exist because a three-dispatch effect that composites into the HDR has no
/// natural seam to probe: when the result looks wrong, "gather, energy term, blur,
/// or upsample?" is otherwise unanswerable without a recompile.
#[derive(Clone, Copy, Debug)]
pub struct SsgiDebug {
    /// Hard-disable the dispatch AND clear the composite bit, whatever the user
    /// setting says. The instant answer to "is SSGI causing this?".
    pub master_kill: bool,
    /// Gather writes `const_value` instead of marching — proves the composite path
    /// end to end with no GI math in play (Gate 1's mechanism, kept live).
    pub force_const: bool,
    pub const_value: f32,
    /// Tonemap outputs the delta INSTEAD of adding it. Fastest read on whether the
    /// delta is invisible or blown out, and on ray count / noise.
    pub show_delta: bool,
    /// Drop the environment subtraction. Toggling it is a live double-count check:
    /// the difference between the two images IS the term deferred already applied.
    pub no_env_subtract: bool,
    /// Dispatch the gather only — shows the raw per-pixel noise the denoiser hides.
    pub skip_blur: bool,
    /// Bypass the depth+normal bilateral gate on upsample, deliberately exposing
    /// the leaks and halos the gate suppresses.
    pub nearest_upsample: bool,
    /// Dispatch at FULL resolution — this is Gate 4's reference render, so the
    /// upsample-error number stays re-measurable. Changes a GPU resource size, so
    /// it must be applied via a deferred flag in `poll_background_tasks`, never
    /// from the panel callback (Frame Ordering Contract).
    pub full_res: bool,
    /// 0 = use the user setting. Perf/quality sweeps without touching the UI.
    pub rays_override: u32,
}

impl Default for SsgiDebug {
    fn default() -> Self {
        Self {
            master_kill: false,
            force_const: false,
            const_value: 0.0,
            show_delta: false,
            no_env_subtract: false,
            skip_blur: false,
            nearest_upsample: false,
            full_res: false,
            rays_override: 0,
        }
    }
}
