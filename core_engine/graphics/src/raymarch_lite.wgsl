// ============================================================
// AZRA — Raymarch Lite (floor + background only)
// Self-contained. No sdf_shared.wgsl dependency.
// Used when all SDF bodies have raster meshes.
// ~10 registers per thread, full GPU occupancy.
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
    tex_size: u32,
    tex_tiling: u32,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    exposure: f32,
    bloom_threshold: f32,
    bloom_intensity: f32,
    post_flags: u32,
    fog_density: f32,
    fog_height_falloff: f32,
    fog_start: f32,
    fog_scatter: f32,
    env_width: u32,
    env_height: u32,
    bg_top_packed: u32,
    bg_bottom_packed: u32,
    env_brightness: f32,
    env_contrast: f32,
    env_saturation: f32,
    env_blur: f32,
    fog_phase_g: f32,
    fog_absorption: f32,
    fog_noise_scale: f32,
    fog_noise_strength: f32,
    fog_max_distance: f32,
    fog_shadow_steps: f32,
    fog_temporal_blend: f32,
    fog_frame_index: u32,
    rt_bounces: u32,
    rt_shadow_samples: u32,
    pt_max_bounces: u32,
    pt_samples_per_frame: u32,
    pt_accumulated: u32,
    pt_frame_seed: u32,
    pt_focus_uv_x: f32,
    pt_focus_uv_y: f32,
    pt_focus_radius: f32,
    pt_focus_falloff: f32,
    rt_env_samples: u32,
    rt_caustics: u32,
    pt_dof_aperture: f32,
    pt_denoise: u32,
    pt_clamp: f32,
    sym_enabled: u32,
    sym_origin: vec4<f32>,
    sym_normal: vec4<f32>,
    sel_rim_color: vec3<f32>,
    sel_rim_enabled: u32,
    sel_rim_intensity: f32,
    sel_rim_power: f32,
    sel_rim_base: f32,
    dither_strength: f32,
    scene_aabb_min: vec4<f32>, // xyz = world min, w = march_relaxation (omega)
    scene_aabb_max: vec4<f32>, // xyz = world max, w = march_bisection_steps
    debug_mode: u32,
    interaction_no_shadows: u32,
    bvh_enabled: u32,
    sdf_aa_enabled: u32,
    // MatCap settings (must match sdf_shared.wgsl for byte alignment)
    matcap_rotation: f32,
    matcap_edge_enabled: u32,
    matcap_edge_threshold: f32,
    matcap_edge_thickness: f32,
    matcap_edge_color: vec3<f32>,
    selected_body_index: u32,
    // Workplane grid
    wp_normal: vec4<f32>,    // xyz = normal, w = spacing (0=disabled)
    wp_origin: vec4<f32>,    // xyz = origin, w = opacity
    wp_color: vec4<f32>,     // rgb = color (linear), w = curvature_enabled (0/1)
}

// --- Bindings (must match raymarch_bgl layout) ---
@group(0) @binding(1) var<uniform> config: GpuUniforms;
@group(0) @binding(2) var voxel_tex: texture_3d<f32>;
@group(0) @binding(6) var voxel_sampler: sampler;
@group(0) @binding(3) var<storage, read> prim_hot: array<f32>;
@group(0) @binding(4) var<storage, read> tex_data: array<u32>;
@group(0) @binding(5) var<storage, read> prim_cold: array<f32>;
@group(0) @binding(43) var<storage, read> bvh_nodes: array<f32>;
@group(0) @binding(9) var gbuf_normal_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var gbuf_material_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(11) var gbuf_albedo_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(22) var<storage, read> tex_atlas: array<u32>;
@group(0) @binding(23) var<storage, read> tex_atlas_info: array<vec4<u32>>;
@group(0) @binding(29) var raster_depth_in: texture_2d<f32>;
@group(0) @binding(30) var gbuf_surface_id_out: texture_storage_2d<r32uint, write>;
// C.5: r32float — body_index as f32 exact past 2048 (MRT ≤32 B/sample)
@group(0) @binding(32) var gbuf_motion_out: texture_storage_2d<r32float, write>;
@group(0) @binding(34) var gbuf_curvature_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(36) var gbuf_emission_out: texture_storage_2d<rgba16float, write>;
// Temporal coherence (L7): validity mask + flag buffer (must match BGL even though lite doesn't use them)
struct TemporalMarchFlags { temporal_enabled: u32, _pad0: u32, _pad1: u32, _pad2: u32, }
@group(0) @binding(44) var temporal_validity_mask: texture_2d<u32>;
@group(0) @binding(45) var<uniform> temporal_flags: TemporalMarchFlags;

// --- Helpers (inlined from sdf_shared.wgsl) ---

fn unpack_rgb(packed: u32) -> vec3<f32> {
    return vec3<f32>(
        f32((packed >> 16u) & 0xFFu) / 255.0,
        f32((packed >> 8u) & 0xFFu) / 255.0,
        f32(packed & 0xFFu) / 255.0
    );
}

fn dither_tri(coord: vec2<f32>) -> f32 {
    let n1 = fract(sin(dot(coord, vec2(122.451, 763.527))) * 43758.5453);
    let n2 = fract(sin(dot(coord, vec2(459.313, 217.891))) * 29816.1267);
    return (n1 + n2 - 1.0) / 255.0;
}

fn bg_gradient_dithered(x: f32, y: f32, h: f32) -> vec3<f32> {
    let top = unpack_rgb(config.bg_top_packed);
    let bot = unpack_rgb(config.bg_bottom_packed);
    let t = y / h;
    let base = mix(top, bot, t);
    if ((config.post_flags & 16u) == 0u) { return base; }
    let d = dither_tri(vec2(x, y)) * config.dither_strength;
    return base + vec3(d, d, d);
}

fn camera_ray(uv_x: f32, uv_y: f32, aspect: f32) -> array<vec3<f32>, 2> {
    let ortho_hw = config.cam_pos.w;
    let fov_scale = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
    if (ortho_hw > 0.0) {
        let origin = config.cam_pos.xyz
            + config.cam_right.xyz * (uv_x * ortho_hw * aspect)
            + config.cam_up.xyz * (uv_y * ortho_hw);
        return array<vec3<f32>, 2>(origin, normalize(config.cam_forward.xyz));
    } else {
        let rd = normalize(config.cam_forward.xyz + config.cam_right.xyz * (uv_x * aspect * fov_scale) + config.cam_up.xyz * (uv_y * fov_scale));
        return array<vec3<f32>, 2>(config.cam_pos.xyz, rd);
    }
}

fn rem_euclid(x: f32, y: f32) -> f32 {
    if (abs(y) < 1e-6) { return 0.0; }
    return x - y * floor(x / y);
}

// --- Entry point ---

@compute @workgroup_size(8, 8, 1)
fn raymarch_gbuf(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let tex_dim = textureDimensions(gbuf_normal_out);
    if (global_id.x >= tex_dim.x || global_id.y >= tex_dim.y) { return; }
    let coord = vec2<i32>(i32(global_id.x), i32(global_id.y));

    // Raster early-exit: raster already wrote this pixel's G-buffer
    let raster_sample = textureLoad(raster_depth_in, coord, 0);
    if (raster_sample.w > 0.0) { return; }

    let x = f32(global_id.x); let y = f32(global_id.y);
    let w = f32(tex_dim.x); let h = f32(tex_dim.y);
    let uv_x = (x / w) * 2.0 - 1.0;
    let uv_y = -((y / h) * 2.0 - 1.0);
    let aspect = w / h;
    let cam_ray = camera_ray(uv_x, uv_y, aspect);
    let cam_pos = cam_ray[0];
    let ray_dir = cam_ray[1];

    // --- Plane intersections: floor and workplane ---
    let bg3 = bg_gradient_dithered(x, y, h);
    var best_t = config.far_clip + 1.0;
    var best_type = 0u; // 0=none, 1=floor, 2=workplane
    var floor_fade = 0.0;

    // Floor plane
    if (ray_dir.y < -0.001 && config.fade_dist > 0.0) {
        let t_f = -(cam_pos.y - config.floor_y) / ray_dir.y;
        if (t_f > 0.0 && t_f < config.far_clip) {
            let fd = max(0.0, 1.0 - (t_f / config.fade_dist));
            if (fd > 0.0) {
                best_t = t_f;
                best_type = 1u;
                floor_fade = fd;
            }
        }
    }

    // Workplane (arbitrary orientation, behind geometry)
    if (config.wp_normal.w > 0.0) {
        let denom = dot(ray_dir, config.wp_normal.xyz);
        if (abs(denom) > 0.001) {
            let t_wp = dot(config.wp_origin.xyz - cam_pos, config.wp_normal.xyz) / denom;
            if (t_wp > 0.0 && t_wp < best_t) {
                best_t = t_wp;
                best_type = 2u;
            }
        }
    }

    if (best_type == 1u) {
        // Floor grid
        let p_plane = cam_pos + ray_dir * best_t;
        var grid_col = bg3;
        if (config.line_thickness > 0.0) {
            let cell = config.cell_size;
            let pixel_size = best_t * 2.0 / h;
            let thick = max(config.line_thickness, pixel_size * 1.5);
            let gx = rem_euclid(p_plane.x, cell);
            let gz = rem_euclid(p_plane.z, cell);
            let gx_d = min(gx, cell - gx);
            let gz_d = min(gz, cell - gz);
            let line_x = 1.0 - smoothstep(0.0, thick, gx_d);
            let line_z = 1.0 - smoothstep(0.0, thick, gz_d);
            let grid_strength = max(line_x, line_z) * floor_fade;
            let ax_thick = thick * 2.5;
            let ax_x = 1.0 - smoothstep(0.0, ax_thick, abs(p_plane.z));
            let ax_z = 1.0 - smoothstep(0.0, ax_thick, abs(p_plane.x));
            let grid_tint = vec3(0.08 * floor_fade);
            let red_axis = vec3(0.35, 0.08, 0.08) * floor_fade;
            let green_axis = vec3(0.08, 0.35, 0.08) * floor_fade;
            grid_col = mix(grid_col, (bg3 + grid_tint), grid_strength);
            grid_col = mix(grid_col, (bg3 + red_axis), ax_x * floor_fade);
            grid_col = mix(grid_col, (bg3 + green_axis), ax_z * floor_fade);
        }
        textureStore(gbuf_normal_out, coord, vec4(0.0, 1.0, 0.0, best_t));
        textureStore(gbuf_material_out, coord, vec4(0.0));
        textureStore(gbuf_albedo_out, coord, vec4(grid_col, 0.5));
        textureStore(gbuf_surface_id_out, coord, vec4<u32>(0xFFFFFFFFu, 0u, 0u, 0u));
        textureStore(gbuf_emission_out, coord, vec4(0.0));
        textureStore(gbuf_curvature_out, coord, vec4(0.0));
        textureStore(gbuf_motion_out, coord, vec4(0.0));
        return;
    }

    if (best_type == 2u) {
        // Workplane grid (arbitrary orientation)
        let p_wp = cam_pos + ray_dir * best_t;
        let wp_fade_dist = config.fade_dist * 2.0 + 20.0; // visible further than floor
        let wp_fade = max(0.0, 1.0 - (best_t / wp_fade_dist)) * config.wp_origin.w;
        if (wp_fade > 0.02) {
            let n = config.wp_normal.xyz;
            // Derive tangent frame from normal
            var tu: vec3<f32>;
            var tv: vec3<f32>;
            if (abs(n.y) > 0.7) {
                tu = vec3(1.0, 0.0, 0.0);
                tv = normalize(cross(tu, n));
            } else {
                tu = normalize(cross(vec3(0.0, 1.0, 0.0), n));
                tv = normalize(cross(tu, n));
            }
            let local = p_wp - config.wp_origin.xyz;
            let u_coord = dot(local, tu);
            let v_coord = dot(local, tv);
            let spacing = config.cell_size;
            let pixel_size = best_t * 2.0 / h;
            let thick = max(config.line_thickness, pixel_size * 1.5);
            let gu_d = rem_euclid(u_coord, spacing);
            let gv_d = rem_euclid(v_coord, spacing);
            let gu_min = min(gu_d, spacing - gu_d);
            let gv_min = min(gv_d, spacing - gv_d);
            let line_u = 1.0 - smoothstep(0.0, thick, gu_min);
            let line_v = 1.0 - smoothstep(0.0, thick, gv_min);
            let grid_strength = max(line_u, line_v) * wp_fade;
            let ax_thick = thick * 2.5;
            let ax_u = 1.0 - smoothstep(0.0, ax_thick, abs(v_coord));
            let ax_v = 1.0 - smoothstep(0.0, ax_thick, abs(u_coord));
            let wire_alpha = max(grid_strength, max(ax_u, ax_v) * wp_fade);
            // Wire grid: only draw pixels on grid lines, skip empty space
            if (wire_alpha >= 0.02) {
                let grid_tint = vec3(0.08 * wp_fade);
                var wp_col = mix(bg3, bg3 + grid_tint, grid_strength);
                let ax_col_u = vec3(0.35, 0.08, 0.08) * wp_fade; // red axis
                let ax_col_v = vec3(0.08, 0.35, 0.08) * wp_fade; // green axis
                wp_col = mix(wp_col, bg3 + ax_col_u, ax_u * wp_fade);
                wp_col = mix(wp_col, bg3 + ax_col_v, ax_v * wp_fade);
                textureStore(gbuf_normal_out, coord, vec4(n, best_t));
                textureStore(gbuf_material_out, coord, vec4(0.0));
                textureStore(gbuf_albedo_out, coord, vec4(wp_col, 0.5));
                textureStore(gbuf_surface_id_out, coord, vec4<u32>(0xFFFFFFFFu, 0u, 0u, 0u));
                textureStore(gbuf_emission_out, coord, vec4(0.0));
                textureStore(gbuf_curvature_out, coord, vec4(0.0));
                textureStore(gbuf_motion_out, coord, vec4(0.0));
                return;
            }
        }
    }

    // Background gradient
    textureStore(gbuf_normal_out, coord, vec4(0.0, 0.0, 0.0, 0.0));
    textureStore(gbuf_material_out, coord, vec4(0.0));
    textureStore(gbuf_albedo_out, coord, vec4(bg3, 0.0));
    textureStore(gbuf_surface_id_out, coord, vec4<u32>(0xFFFFFFFFu, 0u, 0u, 0u));
    textureStore(gbuf_emission_out, coord, vec4(0.0));
    textureStore(gbuf_curvature_out, coord, vec4(0.0));
    textureStore(gbuf_motion_out, coord, vec4(0.0));
}
