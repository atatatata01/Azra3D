// ============================================================
// FourthLayer AI — Compute Shader (PBR Material System)
// ============================================================
// render_mode 0 = Analytical SDF primitives (Object mode)
// render_mode 1 = Voxel grid (Sculpt mode)
// render_mode 3 = Volume texture (PT in volume-only mode)
// ============================================================

struct GpuUniforms {
    cam_pos: vec4<f32>,
    cam_forward: vec4<f32>,
    cam_right: vec4<f32>,
    cam_up: vec4<f32>,
    max_steps: u32,
    far_clip: f32,
    hit_threshold: f32,
    normal_epsilon: f32,
    floor_y: f32,
    fade_dist: f32,
    cell_size: f32,
    line_thickness: f32,
    grid_res: u32,
    grid_bounds: f32,
    num_primitives: u32,
    render_mode: u32,
    view_mode: u32,
    render_quality: u32,
    tex_size: u32,       // AI texture dimensions (square), 0 = no texture
    tex_tiling: u32,     // tiling scale * 100 (e.g. 200 = 2.0x)
    light_dir: vec4<f32>,   // xyz = key light direction, w = intensity
    light_color: vec4<f32>, // rgb = key light color, w = ambient intensity
    // Post-processing
    exposure: f32,
    bloom_threshold: f32,
    bloom_intensity: f32,
    post_flags: u32,
    // Volumetric fog
    fog_density: f32,
    fog_height_falloff: f32,
    fog_start: f32,
    fog_scatter: f32,
    // HDR environment map
    env_width: u32,
    env_height: u32,
    bg_top_packed: u32,    // RGB packed: R<<16 | G<<8 | B
    bg_bottom_packed: u32, // RGB packed: R<<16 | G<<8 | B
    // HDR environment adjustments
    env_brightness: f32,   // multiplier (default 1.0)
    env_contrast: f32,     // 1.0 = no change
    env_saturation: f32,   // 0.0 = grayscale, 1.0 = normal
    env_blur: f32,         // background/sky blur 0..1 (sample_env only)
    // Volumetric fog advanced
    fog_phase_g: f32,       // Mie phase asymmetry (0..0.95)
    fog_absorption: f32,    // absorption coefficient
    fog_noise_scale: f32,   // 3D noise frequency
    fog_noise_strength: f32,// noise density modulation
    fog_max_distance: f32,  // max fog ray distance
    fog_shadow_steps: f32,  // volumetric shadow steps
    fog_temporal_blend: f32,  // temporal reprojection blend (0=off, 0.9=default)
    fog_frame_index: u32,     // frame counter (0=no prev data)
    // Raytracing / Path tracing
    rt_bounces: u32,          // reflection bounces (raytrace mode)
    rt_shadow_samples: u32,   // soft shadow ray count (raytrace mode)
    pt_max_bounces: u32,      // max path bounces (pathtracer)
    pt_samples_per_frame: u32,// samples per pixel per frame
    pt_accumulated: u32,      // frames accumulated so far
    pt_frame_seed: u32,       // random seed
    pt_focus_uv_x: f32,      // mouse focus UV (-1 = no focus)
    pt_focus_uv_y: f32,
    pt_focus_radius: f32,     // focus radius in UV space
    pt_focus_falloff: f32,    // falloff outside focus
    rt_env_samples: u32,     // environment importance samples (raytrace mode)
    rt_caustics: u32,        // 0=off, 1=on
    pt_dof_aperture: f32,    // depth-of-field aperture (0=off)
    pt_denoise: u32,         // 0=off, 1=on
    pt_clamp: f32,           // firefly clamp value
    sym_enabled: u32,        // 0=off, 1=on (global symmetry mirror)
    sym_origin: vec4<f32>,   // xyz = symmetry plane origin
    sym_normal: vec4<f32>,   // xyz = symmetry plane normal, w = radial_count (0=off, 2..32=N-way)
    sel_rim_color: vec3<f32>,
    sel_rim_enabled: u32,
    sel_rim_intensity: f32,
    sel_rim_power: f32,
    sel_rim_base: f32,
    dither_strength: f32,
    // Scene bounding box (ray early termination)
    scene_aabb_min: vec4<f32>,
    scene_aabb_max: vec4<f32>,
    // Debug rendering: 0=normal, 1=march-only, 2=step-count heatmap
    debug_mode: u32,
    interaction_no_shadows: u32,
    bvh_enabled: u32,
    sdf_aa_enabled: u32,
    // MatCap settings
    matcap_rotation: f32,
    matcap_edge_enabled: u32,
    matcap_edge_threshold: f32,
    matcap_edge_thickness: f32,
    // MatCap edge color
    matcap_edge_color: vec3<f32>,
    selected_body_index: u32,
    // Workplane grid
    wp_normal: vec4<f32>,
    wp_origin: vec4<f32>,
    wp_color: vec4<f32>,
    // LOD system (bytes 512-575)
    lod_dist_enabled: u32,
    lod_dist_threshold_0: f32,
    lod_dist_threshold_1: f32,
    lod_dist_threshold_2: f32,
    lod_min_steps: f32,
    foveated_enabled: u32,
    foveated_inner: f32,
    foveated_outer: f32,
    foveated_min_steps: f32,
    foveated_threshold_mult: f32,
    foveated_max_pixel_scale: f32,
    lod_debug_colors: u32,
    lod_scene_screen_size: f32,
    march_adaptive_enabled: u32,
    march_lod_scale: f32,
    shadow_blur_radius: u32,  // 0=off, 1-3=bilateral blur kernel (pixels)
    // Atlas multi-body + active-body albedo map (bytes 576-591 — must match types.rs)
    atlas_body_count: u32,
    mat_albedo_map_slot: u32,
    mat_albedo_map_strength: f32,
    mat_albedo_map_uv_scale: f32,
    // IBL environment (bytes 592-607, 2026-07-18)
    env_rotation: f32,   // Y rotation (radians) — applied in sample_env_at_level
    env_lut_res: u32,    // BRDF LUT resolution (0 = analytic split-sum fallback)
    rt_shadows_on: u32,  // RT projected shadows toggle (was _pad_env0; same offset/size)
    fog_has_vol_lights: u32, // P3: CPU flag — any light VOL on (was _pad_env1)
    // SSR quality (bytes 608-623 — must match types.rs; declared for offset parity)
    ssr_max_steps: u32,
    ssr_intensity: f32,
    ssr_thickness: f32,
    tonemapper: u32,
    // DC-wire deferred gate (bytes 624-639, append-only tail — must match types.rs)
    dc_wire_px: f32,
    env_reflection_blur: f32, // extra IBL specular mip bias 0..1 (was _pad_wire0)
    // Progressive-refine sub-pixel jitter, NDC units (was _pad_wire1/2 — same
    // offset/size). The raster proj is translated by +this; camera_ray subtracts
    // it so ray reconstruction matches the jittered G-buffer. 0 when not
    // accumulating — bit-identical to the pre-jitter pipeline.
    pbr_jitter_x: f32,
    pbr_jitter_y: f32,
    // Fog ambient tint (bytes 640-655, append-only 2026-08-07)
    fog_color: vec3<f32>,
    // MatCap material-colour overlay amount (0 = pure matcap). Repurposed from
    // _pad_fog_color — same offset/size, layout parity with types.rs unchanged.
    matcap_tint: f32,
}

fn unpack_rgb(packed: u32) -> vec3<f32> {
    return vec3<f32>(
        f32((packed >> 16u) & 0xFFu) / 255.0,
        f32((packed >> 8u) & 0xFFu) / 255.0,
        f32(packed & 0xFFu) / 255.0
    );
}

// Triangular-PDF dither noise (±1 LSB) — eliminates gradient banding
fn dither_tri(coord: vec2<f32>) -> f32 {
    let n1 = fract(sin(dot(coord, vec2(122.451, 763.527))) * 43758.5453);
    let n2 = fract(sin(dot(coord, vec2(459.313, 217.891))) * 29816.1267);
    return (n1 + n2 - 1.0) / 255.0;
}

// Camera-ray visibility of the HDRI IMAGE (post_flags bit 4096, set when HIDDEN so
// 0 = legacy "sky shows the map"). ONLY the camera-visible background honours this:
// IBL diffuse (env_sh_irradiance), specular (sample_env_specular), SSR/probe fallback
// and PT bounce/NEE lighting keep sampling the map, so metals still reflect a real
// environment over a flat art-directed plate. Muting the environment outright is a
// different control (`env_enabled` → env_width = 0), which kills sky AND lighting.
//
// In the raster pipeline this flag does NOT flatten the sky in the HDR buffer — deferred
// writes the real sky and marks it alpha 0, and `pass_tonemap` swaps the flat colour in at
// display time. Flattening the buffer starved SSR, whose screen-space hits sample exactly
// that buffer. PT has no such buffer, so `pt_miss_camera` substitutes directly.
fn env_bg_hidden() -> bool { return (config.post_flags & 4096u) != 0u; }

fn bg_gradient_dithered(x: f32, y: f32, h: f32) -> vec3<f32> {
    let top = unpack_rgb(config.bg_top_packed);
    let bot = unpack_rgb(config.bg_bottom_packed);
    let t = y / h;
    let base = mix(top, bot, t);
    if ((config.post_flags & 16u) == 0u) { return base; }
    let d = dither_tri(vec2(x, y)) * config.dither_strength;
    return base + vec3(d, d, d);
}

// Generate camera ray. cam_pos.w > 0 = ortho mode (w = half-width).
// cam_forward.w = tan(fov/2) for perspective FOV control (1.0 = 90°).
fn camera_ray(uv_x_in: f32, uv_y_in: f32, aspect: f32) -> array<vec3<f32>, 2> {
    // Progressive-refine un-jitter: the raster proj shifted the image by +jitter
    // (NDC), so the ray that produced this pixel's G-buffer content is the one
    // through (uv − jitter). Zero when accumulation is off.
    let uv_x = uv_x_in - config.pbr_jitter_x;
    let uv_y = uv_y_in - config.pbr_jitter_y;
    let ortho_hw = config.cam_pos.w;
    let fov_scale = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
    if (ortho_hw > 0.0) {
        // Orthographic: parallel rays, shifted origin
        let origin = config.cam_pos.xyz
            + config.cam_right.xyz * (uv_x * ortho_hw * aspect)
            + config.cam_up.xyz * (uv_y * ortho_hw);
        return array<vec3<f32>, 2>(origin, normalize(config.cam_forward.xyz));
    } else {
        // Perspective with FOV
        let rd = normalize(config.cam_forward.xyz + config.cam_right.xyz * (uv_x * aspect * fov_scale) + config.cam_up.xyz * (uv_y * fov_scale));
        return array<vec3<f32>, 2>(config.cam_pos.xyz, rd);
    }
}

@group(0) @binding(0) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(1) var<uniform> config: GpuUniforms;
@group(0) @binding(2) var<storage, read> voxel_grid: array<f32>;
@group(0) @binding(3) var<storage, read> prim_data: array<f32>;
@group(0) @binding(4) var<storage, read> tex_data: array<u32>;

