#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuUniforms {
    pub cam_pos: [f32;4], pub cam_forward: [f32;4], pub cam_right: [f32;4], pub cam_up: [f32;4],
    pub max_steps: u32, pub far_clip: f32, pub hit_threshold: f32, pub normal_epsilon: f32,
    pub floor_y: f32, pub fade_dist: f32, pub cell_size: f32, pub line_thickness: f32,
    pub grid_res: u32, pub grid_bounds: f32,
    pub num_primitives: u32,
    pub render_mode: u32,
    pub view_mode: u32,
    pub render_quality: u32,
    pub _pad1: u32, pub _pad2: u32,
    // ^^^^^ Now used for texture: _pad1 = tex_size (u32), _pad2 = tex_tiling*100 (u32)
    // Light (pre-computed on CPU)
    pub light_dir: [f32;4],   // xyz = normalized key light direction, w = intensity
    pub light_color: [f32;4], // rgb = key light color, w = ambient intensity
    // Post-processing (Phase 1: HDR pipeline)
    pub exposure: f32,          // EV adjustment (default 1.0)
    pub bloom_threshold: f32,   // luminance threshold for bloom extraction (future)
    pub bloom_intensity: f32,   // bloom mix strength (future)
    pub post_flags: u32,        // bitfield: bit 0 = ACES enabled
    // Volumetric fog (Phase 5)
    pub fog_density: f32,        // 0.0 = no fog (default 0.02)
    pub fog_height_falloff: f32, // exponential height falloff (default 0.5)
    pub fog_start: f32,          // distance where fog begins (default 1.0)
    pub fog_scatter: f32,        // god ray scattering intensity (default 0.5)
    // HDR environment map
    pub env_width: u32,          // equirectangular env map width (0 = no env map)
    pub env_height: u32,         // equirectangular env map height
    pub bg_top_packed: u32,      // RGB packed: R<<16 | G<<8 | B (bg gradient top)
    pub bg_bottom_packed: u32,   // RGB packed: R<<16 | G<<8 | B (bg gradient bottom)
    // HDR environment adjustments
    pub env_brightness: f32,    // multiplier (default 1.0)
    pub env_contrast: f32,      // 1.0 = no change
    pub env_saturation: f32,    // 0.0 = grayscale, 1.0 = normal
    pub env_blur: f32,          // background/sky blur 0..1 (sample_env only)
    // Volumetric fog advanced (rows 15-16)
    pub fog_phase_g: f32,       // Mie phase asymmetry (default 0.7, range 0..0.95)
    pub fog_absorption: f32,    // absorption coefficient (default 0.3, range 0..2)
    pub fog_noise_scale: f32,   // 3D noise frequency (default 3.0, range 0.5..10)
    pub fog_noise_strength: f32,// noise density modulation (default 0.4, range 0..1)
    pub fog_max_distance: f32,  // max fog ray distance (default 30.0, range 5..100)
    pub fog_shadow_steps: f32,  // volumetric shadow steps as f32 (default 4, 0=off)
    pub fog_temporal_blend: f32,  // 0.0 = off, 0.9 = default temporal weight
    pub fog_frame_index: u32,     // frame counter (0 = no previous data available)
    // Raytracing / Path tracing settings (row 17-20)
    pub rt_bounces: u32,          // reflection bounce count (1-8, raytrace mode)
    pub rt_shadow_samples: u32,   // soft shadow rays (1-16, raytrace mode)
    pub pt_max_bounces: u32,      // max path bounces (1-32, pathtracer)
    pub pt_samples_per_frame: u32,// samples per pixel per frame (1…1e6, user-chosen)
    pub pt_accumulated: u32,      // frames accumulated so far (0 = fresh)
    pub pt_frame_seed: u32,       // random seed (frame index for noise)
    pub pt_focus_uv_x: f32,      // mouse focus position (normalized 0-1, -1 = no focus)
    pub pt_focus_uv_y: f32,
    pub pt_focus_radius: f32,     // focus radius in UV space (0.05 = tight, 1.0 = full image)
    pub pt_focus_falloff: f32,    // samples falloff outside focus (0 = hard cutoff, 1 = full)
    pub rt_env_samples: u32,     // environment importance samples (1-16, raytrace mode)
    pub rt_caustics: u32,        // 0=off, 1=on (raytrace mode)
    pub pt_dof_aperture: f32,    // depth-of-field aperture (0.0=off, 0.01-1.0)
    pub pt_denoise: u32,         // 0=off, 1=on (spatial denoise)
    pub pt_clamp: f32,           // firefly clamp value (1.0-100.0, default 10.0)
    pub sym_enabled: u32,         // 0=off, 1=on (global symmetry mirror)
    // Global symmetry plane (vec4-aligned)
    /// xyz = symmetry plane origin. **w = procedural fill-light strength**
    /// (repurposed 2026-08-06 from a documented-unused slot — same offset/size, so
    /// the 640-byte struct span and every WGSL mirror are untouched).
    pub sym_origin: [f32; 4],
    pub sym_normal: [f32; 4],     // xyz = plane normal, w = radial_count (0=off, 2..32=N-way)
    // Selection outline overlay (screen-space edge detection)
    pub sel_rim_color: [f32; 3],  // RGB outline color
    pub sel_rim_enabled: u32,     // 0=off, 1=on
    pub sel_rim_intensity: f32,   // rim lighting intensity (default 0.0)
    pub sel_rim_power: f32,       // outline width in pixels (1-10+)
    pub sel_rim_base: f32,        // rim base additive (default 0.0)
    pub dither_strength: f32,     // dither intensity multiplier (0.0-2.0, default 1.0)
    // Scene bounding box for early ray termination (set each frame from body AABBs)
    pub scene_aabb_min: [f32; 4], // xyz = world-space AABB min, w = march_relaxation (omega, 1.0-1.8)
    pub scene_aabb_max: [f32; 4], // xyz = world-space AABB max, w = march_bisection_steps (as f32)
    // Debug rendering mode: 0=normal, 1=march-only (white/black), 2=step-count heatmap
    pub debug_mode: u32,
    pub interaction_no_shadows: u32, // 1 = skip soft_shadow during interaction
    pub bvh_enabled: u32,            // 0=off, 1=BVH acceleration for large bodies
    pub sdf_aa_enabled: u32,         // 1 = SDF silhouette anti-aliasing
    // MatCap settings (row 28)
    pub matcap_rotation: f32,        // radians
    pub matcap_edge_enabled: u32,    // 0=off, 1=on
    pub matcap_edge_threshold: f32,  // normal-angle threshold (0-1)
    pub matcap_edge_thickness: f32,  // pixel width (1-3)
    // MatCap edge color (row 29)
    pub matcap_edge_color: [f32; 3], // RGB linear
    pub selected_body_index: u32,
    // Workplane grid — GPU-rendered behind geometry (Auto mode)
    pub wp_normal: [f32; 4],  // xyz = plane normal, w = spacing (0 = disabled)
    pub wp_origin: [f32; 4],  // xyz = plane origin, w = opacity
    pub wp_color: [f32; 4],   // rgb = grid color (linear), w = curvature_enabled (0.0/1.0)
    // ---- LOD system (bytes 512-575) ----
    pub lod_dist_enabled: u32,           // 0=off, 1=on
    pub lod_dist_threshold_0: f32,       // full quality above this screen fraction (default 0.3)
    pub lod_dist_threshold_1: f32,       // medium above this (default 0.1)
    pub lod_dist_threshold_2: f32,       // low above this (default 0.03)
    pub lod_min_steps: f32,              // minimum march steps for any LOD (default 16.0)
    pub foveated_enabled: u32,           // 0=off, 1=on
    pub foveated_inner: f32,             // full quality radius from center (0-1, default 0.3)
    pub foveated_outer: f32,             // min quality radius from center (0-1, default 0.9)
    pub foveated_min_steps: f32,         // steps at screen edge (default 32.0)
    pub foveated_threshold_mult: f32,    // hit_threshold multiplier at edge (default 10.0)
    pub foveated_max_pixel_scale: f32,   // max block size at edge (default 4.0)
    pub lod_debug_colors: u32,           // 0=off, 1=tint by LOD level
    pub lod_scene_screen_size: f32,      // CPU-computed: scene AABB screen fraction
    pub march_adaptive_enabled: u32,    // 0=off (fixed max_steps), 1=per-pixel adaptive
    pub march_lod_scale: f32,            // pixel-size threshold multiplier (default 1.0)
    pub shadow_blur_radius: u32,  // 0=off, 1-3=bilateral blur kernel radius (pixels)
    // ---- Atlas multi-body (bytes 576-591) ----
    pub atlas_body_count: u32,  // number of valid bodies in atlas_bodies buffer
    // Active-body albedo texture map (SDF volume-preview path). The VP shader reads ONE material
    // per frame (the active body), so the map travels in the uniform instead of the prim buffer —
    // avoids growing FLOATS_PER_PRIM / touching the GLSL BVH path. Repurposed from former _pad_atlas.
    // `slot`: low 31 bits = 1-based texture array slot (0 = no map); bit 31 = projection (0=triplanar,
    // 1=UV — UV is unused for SDF, always triplanar, but kept for layout symmetry).
    pub mat_albedo_map_slot: u32,
    pub mat_albedo_map_strength: f32, // 0..1 influence
    pub mat_albedo_map_uv_scale: f32, // tiling (world units per tile, triplanar)
    // ---- IBL environment (bytes 592-607, 2026-07-18) ----
    /// Y-axis rotation of the environment (radians). Applied in sample_env_at_level —
    /// rotates IBL sampling AND the background together (deferred, PT, fog).
    pub env_rotation: f32,
    /// BRDF LUT resolution (split-sum second term, deferred binding 39). 0 until the
    /// startup LUT worker's upload lands — the shader falls back to the analytic
    /// Karis approximation, so there is never a wrong/black window.
    pub env_lut_res: u32,
    /// RT projected shadows global toggle (RT_SHADOWS_DESIGN Stage 1). 1 = deferred
    /// point/spot lights read the RT visibility buffer (all-geometry occlusion) instead
    /// of the SDF-only `soft_shadow` march; 0 = legacy behavior. The sun stays CSM either
    /// way. Repurposed from `_pad_env0` — SAME offset/size (u32), so no WGSL struct span
    /// changes and the post_process layout guard is unaffected.
    pub rt_shadows_on: u32,
    /// P2/P3: bitfield of visible lights with volumetric enabled (bit i = GPU light index i).
    /// 0 = no vol lights (fog skips vol path). Non-zero bits drive compact vol iteration in fog.
    /// Repurposed from `_pad_env1` (same offset/size).
    pub fog_has_vol_lights: u32,
    // ---- SSR quality controls (bytes 608-623, 2026-07-18 quality pass) ----
    /// DDA march step budget (UI "Quality"). Clamped 8..128 in the shader.
    pub ssr_max_steps: u32,
    /// 0..1 scale on hit confidence (UI "Intensity"): 0 = pure IBL, 1 = full SSR swap.
    pub ssr_intensity: f32,
    /// Base world-space thickness for the depth-crossing test (adaptive terms
    /// are added per-step in the shader).
    pub ssr_thickness: f32,
    /// Viewport tonemap curve (pass_tonemap `viewport_tonemap`): 0 = ACES fitted
    /// "Classic" (default — bit-identical to the pre-selector hardcoded curve),
    /// 1 = AgX, 2 = Filmic (Hable), 3 = Reinhard, 4 = PBR Neutral, 5 = None/Linear.
    /// Repurposed from _pad_ssr0 (2026-07-18) — struct size stays 624.
    pub tonemapper: u32,
    // ---- DC-wire deferred gate (bytes 624-639, 2026-07-23, append-only tail) ----
    /// Wireframe overlay thickness in px; 0.0 = overlay off — the deferred dilation
    /// block early-outs at zero cost. Mirrors the raster wire pass gate
    /// (raster_gbuffer want_gpu_wire); populated in viewport_render.
    pub dc_wire_px: f32,
    /// Extra mip bias for IBL specular reflections only (0..1). Repurposed from
    /// `_pad_wire[0]` — same offset/size (f32), struct stays 640.
    pub env_reflection_blur: f32,
    /// Progressive-refine sub-pixel jitter in NDC units (PROGRESSIVE_PBR_PLAN.md
    /// Phase 1). The raster proj is translated by +this (clip space) and every
    /// compute `camera_ray()` subtracts it, so G-buffer content and ray
    /// reconstruction agree per frame. [0,0] whenever accumulation is off —
    /// every path bit-identical to the non-progressive pipeline. Repurposed from
    /// `_pad_wire` — same offset/size (2×32-bit), struct stays 640.
    pub pbr_jitter: [f32; 2],
    // ---- Fog color (bytes 640-655, append-only 2026-08-07) ----
    /// Volumetric fog ambient / haze tint (linear RGB). Was hardcoded
    /// `vec3(0.35, 0.4, 0.5)` in fog_entry.wgsl.
    pub fog_color: [f32; 3],
    /// MatCap material-colour overlay amount (view_mode 1 only): 0 = pure matcap (the
    /// pre-overlay path, bit-identical), 1 = matcap × G-buffer base colour. Repurposed
    /// from `_pad_fog_color` — same offset/size (f32), struct stays 656, no BGL churn.
    /// Zero is the "off" reading, so every caller that does not populate it is unchanged.
    pub matcap_tint: f32,
}
impl GpuUniforms { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }
const _: () = assert!(std::mem::size_of::<GpuUniforms>() == 656,
    "GpuUniforms size changed — check WGSL alignment (light_dir must stay at byte offset 128)");

/// Previous frame camera data for temporal reprojection (64 bytes)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PrevCamera {
    pub cam_pos: [f32;4],
    pub cam_forward: [f32;4],
    pub cam_right: [f32;4],
    pub cam_up: [f32;4],
}
impl PrevCamera { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// Temporal reprojection parameters (Level 7). Uploaded each frame as a uniform buffer.
/// Must match TemporalParams in temporal_reproject.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TemporalParams {
    pub prev_cam_pos: [f32; 4],
    pub prev_cam_forward: [f32; 4],
    pub prev_cam_right: [f32; 4],
    pub prev_cam_up: [f32; 4],
    pub cur_cam_pos: [f32; 4],
    pub cur_cam_forward: [f32; 4],
    pub cur_cam_right: [f32; 4],
    pub cur_cam_up: [f32; 4],
    pub screen_size: [u32; 2],
    pub dirty_rect_min: [u32; 2],
    pub dirty_rect_max: [u32; 2],
    pub camera_moved: u32,
    pub frame_valid: u32,
}
impl TemporalParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// PT temporal reprojection parameters (P3.5). Must match PtReprojParams in
/// pt_reproject.wgsl. Camera packing follows GpuUniforms/camera_ray:
/// pos.w = ortho half-width (0 = perspective), forward.w = fov_scale.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PtReprojParams {
    pub prev_cam_pos: [f32; 4],
    pub prev_cam_forward: [f32; 4],
    pub prev_cam_right: [f32; 4],
    pub prev_cam_up: [f32; 4],
    pub cur_cam_pos: [f32; 4],
    pub cur_cam_forward: [f32; 4],
    pub cur_cam_right: [f32; 4],
    pub cur_cam_up: [f32; 4],
    /// Lighting buffer size (dispatch domain — half or full).
    pub screen_size: [u32; 2],
    pub hist_cap: f32,
    pub depth_reject: f32,
    /// Full G-buffer size. When equal to screen_size, 1:1. When larger (e.g. 2×),
    /// guides are sampled at (px * full/screen) so half-res lighting can use full-res depth/normal.
    pub full_size: [u32; 2],
    pub _pad: [u32; 2],
}
impl PtReprojParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// Temporal coherence flag passed to the march shader via a tiny uniform buffer (binding 45).
/// When temporal_enabled == 1 and the validity mask says reuse, the march skips that pixel.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TemporalMarchFlags {
    pub temporal_enabled: u32,
    pub _pad: [u32; 3], // pad to 16 bytes (vec4 alignment)
}
impl TemporalMarchFlags { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// RT projected-shadow ray-pass params (RT_SHADOWS_DESIGN Stage 3). Dedicated uniform
/// (binding 25) — kept OUT of the locked-624B GpuUniforms. Must match `RtShadowParams` in
/// rt_shadow_entry.wgsl. Runtime-only (never serialized — the ssr_max_steps precedent).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RtShadowParams {
    /// 0..1 → the light's angular half-radius (scaled to tan in-shader). 0 = hard (Stage-1).
    pub softness: f32,
    /// Effective rays over the light disc (1 = hard center ray = Stage-1 identical).
    pub samples: u32,
    /// Per-frame jitter rotation seed; 0 = static per-pixel pattern (deterministic, no boiling
    /// when temporal accumulation is off).
    pub frame_index: u32,
    /// Soft penumbra resolution: 0 = half-res 2×2 splat (viewport perf), 1 = full-res (quality;
    /// Samples then visibly cleans the penumbra). Hard path (samples==1) always full-res.
    pub full_res: u32,
    /// Progressive-refine AUTO SUN (Phase 2c, "light leaking" fix): tan of the sun's
    /// cone half-angle while the idle accumulator averages — the sun then traces
    /// leak-free RT shadows WITHOUT its per-light opt-in. < 0 = auto sun disabled.
    /// Already a tan (NOT scaled by the shader's RT_MAX_TAN slider mapping).
    pub sun_tan: f32,
    pub _pad: [u32; 3],
}
impl RtShadowParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// Screen-space GI params (SSGI_PLAN.md). Must match `SsgiParams` in
/// `ssgi_entry.wgsl` — 48 bytes, uniform-layout legal (multiple of 16, and the
/// trailing `vec2<u32>` sits at offset 40, which is 8-aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SsgiParams {
    pub half_size: [u32; 2],
    pub full_size: [u32; 2],
    /// Overall delta strength; 1.0 = the energy-conserving result.
    pub intensity: f32,
    /// March reach, world units.
    pub radius: f32,
    /// "HDRI Occlusion": how much occluded environment light the delta may REMOVE.
    /// 1.0 = energy-consistent, 0.0 = additive only (bit-equal to the no-HDRI path).
    pub env_occlusion: f32,
    /// Cosine-weighted rays per half-res pixel.
    pub rays: u32,
    /// Dev stage toggles (§4.10). Bit 1 = force constant, 2 = no env subtract.
    /// Every bit's OFF state is the production path.
    pub debug_flags: u32,
    /// Constant the gather writes when `debug_flags & 1` — Gate 1's neutrality proof.
    pub dbg_const: f32,
    /// Progressive-refine frame term (Gate 4 temporal arm): rotates the IGN ray
    /// pattern per frame ONLY while the PBR accumulator averages (0 = the static
    /// deterministic pattern — SSGI_PLAN.md §4.3's no-boiling constraint).
    pub frame_index: u32,
    /// C2 (PBR_REFINE_HARDENING_PLAN): probe diffuse blend weight
    /// (`strength × ready × enabled`). Shader scales env-subtraction by `(1 − w)`.
    /// Was an unused pad slot — same size/offset, no BGL churn.
    pub probe_diffuse_w: f32,
}
impl SsgiParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }
const _: () = assert!(std::mem::size_of::<SsgiParams>() == 48, "SsgiParams must match ssgi_entry.wgsl");

/// RT-shadow spatial bilateral denoise params (RT_SHADOWS_DESIGN Stage 2). Must match
/// `RtDenoiseParams` in rt_shadow_denoise.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RtDenoiseParams {
    pub radius: u32,  // bilateral kernel half-size in pixels (0 = passthrough)
    pub width: u32,
    pub height: u32,
    pub _pad: u32,
}
impl RtDenoiseParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

/// RT-shadow temporal reprojection params (RT_SHADOWS_DESIGN Stage 2). Mirrors PtReprojParams'
/// camera packing (pos.w = ortho half-width, forward.w = fov_scale) so the same unproject/reproject
/// math applies. Must match `RtTemporalParams` in rt_shadow_temporal.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RtTemporalParams {
    pub prev_cam_pos: [f32; 4],
    pub prev_cam_forward: [f32; 4],
    pub prev_cam_right: [f32; 4],
    pub prev_cam_up: [f32; 4],
    pub cur_cam_pos: [f32; 4],
    pub cur_cam_forward: [f32; 4],
    pub cur_cam_right: [f32; 4],
    pub cur_cam_up: [f32; 4],
    pub screen_size: [u32; 2],
    pub blend_alpha: f32,  // EMA feedback weight for valid reprojected history (e.g. 0.9)
    pub depth_reject: f32, // relative depth tolerance for disocclusion (e.g. 0.05)
    pub frame_valid: u32,  // 0 = no valid history (first frame / resize / resume) → current only
    pub _pad: [u32; 3],
}
impl RtTemporalParams { pub fn as_bytes(&self) -> &[u8] { bytemuck::bytes_of(self) } }

#[derive(Clone,Copy)]
pub struct GridConfig { pub floor_y:f32, pub fade_dist:f32, pub cell_size:f32, pub line_thickness:f32 }
impl Default for GridConfig { fn default()->Self{Self{floor_y:-1.5,fade_dist:15.0,cell_size:1.0,line_thickness:0.02}} }

#[derive(Clone,Copy)]
pub struct LightConfig {
    pub azimuth: f32,     // degrees, 0=front, 90=right
    pub elevation: f32,   // degrees, 0=horizon, 90=top
    pub intensity: f32,   // irradiance at normal incidence (default π → white card = radiance 1)
    pub color: [f32;3],   // RGB 0-1
    pub ambient: f32,     // 0-1
}
impl Default for LightConfig {
    fn default() -> Self {
        // Same default sun as the scene graph's LightData (irradiance π — see DEFAULT_SUN_IRRADIANCE).
        Self { azimuth: 45.0, elevation: 55.0, intensity: core_math::scene_graph::DEFAULT_SUN_IRRADIANCE, color: [1.0, 0.98, 0.95], ambient: 0.15 }
    }
}
impl LightConfig {
    /// Compute normalized world-space direction from azimuth + elevation
    pub fn direction(&self) -> [f32; 3] {
        let az = self.azimuth.to_radians();
        let el = self.elevation.to_radians();
        let x = az.sin() * el.cos();
        let y = el.sin();
        let z = -az.cos() * el.cos();
        let len = (x*x + y*y + z*z).sqrt();
        if len < 0.001 { return [0.0, 1.0, 0.0]; }
        [x/len, y/len, z/len]
    }
}

pub struct RenderConfig {
    pub width:usize, pub height:usize,
    pub cam_orbit:glam::Quat, pub cam_dist:f32, pub cam_target:glam::Vec3,
    pub grid_res:u32, pub grid_bounds:f32,
    pub max_steps:u32, pub far_clip:f32, pub hit_threshold:f32, pub normal_epsilon:f32,
    pub grid:GridConfig, pub num_primitives: u32,
    pub render_mode: u32, pub view_mode: u32, pub render_quality: u32,
    pub light: LightConfig,
    pub tex_size: u32,     // AI texture dimensions (square), 0 = no texture
    pub tex_tiling: f32,   // tiling scale (default 2.0)
    pub exposure: f32,     // HDR exposure (default 1.0)
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_flags: u32,  // bit 1 = bloom enabled
    pub fog_density: f32,
    pub fog_height_falloff: f32,
    pub fog_start: f32,
    pub fog_scatter: f32,
    pub env_width: u32,
    pub env_height: u32,
    pub bg_color_top: [f32; 3],    // viewport gradient top
    pub bg_color_bottom: [f32; 3], // viewport gradient bottom
    pub ortho: bool,               // orthographic projection
    pub fov_deg: f32,              // perspective FOV in degrees (default 90)
    pub env_brightness: f32,       // HDR env brightness multiplier
    pub env_contrast: f32,         // HDR env contrast
    pub env_saturation: f32,       // HDR env saturation
    pub env_blur: f32,             // HDR background/sky blur (sample_env)
    pub env_reflection_blur: f32,  // extra IBL specular mip bias 0..1
    pub env_rotation: f32,         // HDR env Y rotation (radians)
    pub env_lut_res: u32,          // BRDF LUT resolution (0 = analytic fallback)
    pub ssr_max_steps: u32,        // SSR DDA step budget (UI "Quality")
    pub ssr_intensity: f32,        // SSR confidence scale 0..1 (UI "Intensity")
    /// Procedural fill-light strength (0.2 = the legacy hardcoded value, 0 = off).
    pub fill_light: f32,
    pub ssr_thickness: f32,        // SSR base depth-test thickness (world units)
    pub tonemapper: u32,           // viewport tonemap curve (0 = ACES/Classic default)
    pub fog_phase_g: f32,
    pub fog_absorption: f32,
    pub fog_noise_scale: f32,
    pub fog_noise_strength: f32,
    pub fog_max_distance: f32,
    pub fog_shadow_steps: f32,
    pub fog_temporal_blend: f32,
    pub fog_frame_index: u32,
    /// Fog ambient tint (linear RGB).
    pub fog_color: [f32; 3],
    // Raytracing / path tracing
    pub rt_bounces: u32,
    pub rt_shadow_samples: u32,
    pub pt_max_bounces: u32,
    pub pt_samples_per_frame: u32,
    pub pt_accumulated: u32,
    pub pt_frame_seed: u32,
    pub pt_focus_uv: [f32; 2],
    pub pt_focus_radius: f32,
    pub pt_focus_falloff: f32,
    pub rt_env_samples: u32,
    pub rt_caustics: bool,
    /// RT projected shadows global toggle (RT_SHADOWS_DESIGN Stage 1) → GpuUniforms.rt_shadows_on.
    pub rt_shadows: bool,
    pub pt_dof_aperture: f32,
    pub pt_denoise: bool,
    pub pt_clamp: f32,
    // MatCap settings
    pub matcap_rotation: f32,        // radians
    pub matcap_edge_enabled: bool,
    pub matcap_edge_threshold: f32,
    pub matcap_edge_thickness: f32,
    pub matcap_edge_color: [f32; 3], // linear RGB
    // Workplane grid (GPU-rendered, behind geometry in Auto mode)
    pub wp_normal: [f32; 4],  // xyz = plane normal, w = spacing (0 = disabled)
    pub wp_origin: [f32; 4],  // xyz = plane origin, w = opacity
    pub wp_color: [f32; 4],   // rgb = grid color (linear), w = curvature_enabled (0.0/1.0)
}
impl Default for RenderConfig {
    fn default()->Self{Self{
        width:640,height:360,cam_orbit:glam::Quat::IDENTITY,cam_dist:4.0,cam_target:glam::Vec3::ZERO,
        grid_res:128,grid_bounds:2.0,max_steps:128,far_clip:20.0,hit_threshold:0.001,normal_epsilon:0.005,
        grid:GridConfig::default(), num_primitives:0, render_mode:0, view_mode:0, render_quality:2,
        light:LightConfig::default(), tex_size:0, tex_tiling:2.0, exposure:1.0,
        bloom_threshold:1.0, bloom_intensity:0.5, bloom_flags:0,
        fog_density:0.0, fog_height_falloff:0.5, fog_start:1.0, fog_scatter:0.5,
        env_width:0, env_height:0,
        bg_color_top: [0.28, 0.28, 0.30], bg_color_bottom: [0.14, 0.14, 0.14], ortho: false, fov_deg: 90.0,
        env_brightness: 1.0, env_contrast: 1.0, env_saturation: 1.0, env_blur: 0.0,
        env_reflection_blur: 0.0,
        env_rotation: 0.0, env_lut_res: 0,
        ssr_max_steps: 48, ssr_intensity: 1.0, ssr_thickness: 0.35, fill_light: 0.2, tonemapper: 0,
        fog_phase_g: 0.7, fog_absorption: 0.3, fog_noise_scale: 3.0, fog_noise_strength: 0.0,
        fog_max_distance: 30.0, fog_shadow_steps: 0.0,
        fog_temporal_blend: 0.9, fog_frame_index: 0,
        fog_color: [0.35, 0.40, 0.50],
        rt_bounces: 3, rt_shadow_samples: 4,
        pt_max_bounces: 6, pt_samples_per_frame: 1,
        pt_accumulated: 0, pt_frame_seed: 0,
        pt_focus_uv: [-1.0, -1.0], pt_focus_radius: 0.15, pt_focus_falloff: 0.5,
        rt_env_samples: 4, rt_caustics: false, rt_shadows: false,
        pt_dof_aperture: 0.0, pt_denoise: false, pt_clamp: 10.0,
        matcap_rotation: 0.0, matcap_edge_enabled: false, matcap_edge_threshold: 0.3,
        matcap_edge_thickness: 1.0, matcap_edge_color: [0.006, 0.006, 0.007],
        wp_normal: [0.0, 1.0, 0.0, 1.0], wp_origin: [0.0, 0.0, 0.0, 1.0], wp_color: [0.3, 0.3, 0.3, 1.0],
    }}
}
impl RenderConfig {
    pub fn to_gpu_uniforms(&self) -> GpuUniforms {
        let cp=self.cam_target+self.cam_orbit*glam::Vec3::new(0.0,0.0,self.cam_dist);
        let f=(self.cam_orbit*glam::Vec3::NEG_Z).normalize();
        let r=(self.cam_orbit*glam::Vec3::X).normalize();
        let u=(self.cam_orbit*glam::Vec3::Y).normalize();
        let fov_scale = (self.fov_deg.to_radians() * 0.5).tan(); // 1.0 at 90°
        let ld = self.light.direction();
        GpuUniforms{
            cam_pos:[cp.x,cp.y,cp.z, if self.ortho { self.cam_dist } else { 0.0 }],cam_forward:[f.x,f.y,f.z, fov_scale],
            cam_right:[r.x,r.y,r.z,0.0],cam_up:[u.x,u.y,u.z,0.0],
            max_steps:self.max_steps,far_clip:self.far_clip,
            hit_threshold:self.hit_threshold,normal_epsilon:self.normal_epsilon,
            floor_y:self.grid.floor_y,fade_dist:self.grid.fade_dist,
            cell_size:self.grid.cell_size,line_thickness:self.grid.line_thickness,
            grid_res:self.grid_res,grid_bounds:self.grid_bounds,
            num_primitives:self.num_primitives, render_mode:self.render_mode,
            view_mode:self.view_mode, render_quality:self.render_quality,
            _pad1:self.tex_size, _pad2:(self.tex_tiling * 100.0) as u32,
            light_dir:[ld[0], ld[1], ld[2], self.light.intensity],
            light_color:[self.light.color[0], self.light.color[1], self.light.color[2], self.light.ambient],
            exposure: self.exposure,
            bloom_threshold: self.bloom_threshold,
            bloom_intensity: self.bloom_intensity,
            post_flags: 1 | self.bloom_flags, // bit 0 = ACES, bit 1 = bloom
            fog_density: self.fog_density,
            fog_height_falloff: self.fog_height_falloff,
            fog_start: self.fog_start,
            fog_scatter: self.fog_scatter,
            env_width: self.env_width,
            env_height: self.env_height,
            // Pack sRGB [f32;3] into u32: R<<16 | G<<8 | B
            bg_top_packed: ((self.bg_color_top[0] * 255.0) as u32) << 16
                 | ((self.bg_color_top[1] * 255.0) as u32) << 8
                 | ((self.bg_color_top[2] * 255.0) as u32),
            bg_bottom_packed: ((self.bg_color_bottom[0] * 255.0) as u32) << 16
                 | ((self.bg_color_bottom[1] * 255.0) as u32) << 8
                 | ((self.bg_color_bottom[2] * 255.0) as u32),
            env_brightness: self.env_brightness,
            env_contrast: self.env_contrast,
            env_saturation: self.env_saturation,
            env_blur: self.env_blur,
            fog_phase_g: self.fog_phase_g,
            fog_absorption: self.fog_absorption,
            fog_noise_scale: self.fog_noise_scale,
            fog_noise_strength: self.fog_noise_strength,
            fog_max_distance: self.fog_max_distance,
            fog_shadow_steps: self.fog_shadow_steps,
            fog_temporal_blend: self.fog_temporal_blend,
            fog_frame_index: self.fog_frame_index,
            rt_bounces: self.rt_bounces,
            rt_shadow_samples: self.rt_shadow_samples,
            pt_max_bounces: self.pt_max_bounces,
            pt_samples_per_frame: self.pt_samples_per_frame,
            pt_accumulated: self.pt_accumulated,
            pt_frame_seed: self.pt_frame_seed,
            pt_focus_uv_x: self.pt_focus_uv[0],
            pt_focus_uv_y: self.pt_focus_uv[1],
            pt_focus_radius: self.pt_focus_radius,
            pt_focus_falloff: self.pt_focus_falloff,
            rt_env_samples: self.rt_env_samples,
            rt_caustics: if self.rt_caustics { 1 } else { 0 },
            pt_dof_aperture: self.pt_dof_aperture,
            pt_denoise: if self.pt_denoise { 1 } else { 0 },
            pt_clamp: self.pt_clamp,
            sym_enabled: 0,
            sym_normal: [1.0, 0.0, 0.0, 0.0],
            sel_rim_color: [1.0, 0.6, 0.1],
            sel_rim_enabled: 1,
            sel_rim_intensity: 0.0,
            sel_rim_power: 1.0,  // outline width in pixels
            sel_rim_base: 0.0,
            dither_strength: 0.7,
            scene_aabb_min: [0.0; 4],
            scene_aabb_max: [0.0; 4],
            debug_mode: 0,
            interaction_no_shadows: 0,
            bvh_enabled: 0,
            sdf_aa_enabled: 0,
            matcap_rotation: self.matcap_rotation,
            matcap_edge_enabled: if self.matcap_edge_enabled { 1 } else { 0 },
            matcap_edge_threshold: self.matcap_edge_threshold,
            matcap_edge_thickness: self.matcap_edge_thickness,
            matcap_edge_color: self.matcap_edge_color,
            selected_body_index: 0,
            wp_normal: self.wp_normal,
            wp_origin: self.wp_origin,
            wp_color: self.wp_color,
            // LOD defaults (populated from RenderSettings in viewport_render.rs)
            lod_dist_enabled: 0,
            lod_dist_threshold_0: 0.3,
            lod_dist_threshold_1: 0.1,
            lod_dist_threshold_2: 0.03,
            lod_min_steps: 16.0,
            foveated_enabled: 0,
            foveated_inner: 0.3,
            foveated_outer: 0.9,
            foveated_min_steps: 32.0,
            foveated_threshold_mult: 10.0,
            foveated_max_pixel_scale: 4.0,
            lod_debug_colors: 0,
            lod_scene_screen_size: 1.0,
            march_adaptive_enabled: 0, march_lod_scale: 1.0, shadow_blur_radius: 2,
            atlas_body_count: 0,
            mat_albedo_map_slot: 0, mat_albedo_map_strength: 1.0, mat_albedo_map_uv_scale: 1.0,
            env_rotation: self.env_rotation,
            env_lut_res: self.env_lut_res,
            rt_shadows_on: if self.rt_shadows { 1 } else { 0 },
            fog_has_vol_lights: 0, // set by app after to_gpu_uniforms
            ssr_max_steps: self.ssr_max_steps,
            ssr_intensity: self.ssr_intensity,
            sym_origin: [0.0, 0.0, 0.0, self.fill_light],
            ssr_thickness: self.ssr_thickness,
            tonemapper: self.tonemapper,
            // DC-wire deferred gate (bytes 624-639) — populated in viewport_render.
            dc_wire_px: 0.0,
            env_reflection_blur: self.env_reflection_blur,
            pbr_jitter: [0.0; 2], // populated in viewport_render while accumulating
            fog_color: self.fog_color,
            // MatCap overlay is patched in by viewport_render (off = 0.0 here).
            matcap_tint: 0.0,
        }
    }
}
