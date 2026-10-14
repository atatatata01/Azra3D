// ============================================================
// AZRA — Sculpt Voxel Raymarch (Plan B1)
//
// Raymarches the GPU-resident SPARSE voxel atlas (index tags + brick pool) of the
// active sculpt DURING a brush stroke, writing color+normal+DEPTH into the G-buffer
// so it composites with the floor/gizmo/mesh exactly like volume_preview does.
//
// Derived correct-by-construction from two already-proven pieces:
//   - STRUCTURE: volume_preview.wgsl (camera_ray, ray_aabb, write_gbuf with depth in
//     gbuf_normal_out.w, the raster_depth_in occlusion skip).
//   - DENSITY DECODE: voxel_mesh.wgsl `dens()` / `sample_world()` / `gradient_normal()`
//     (sparse atlas tag → pool lookup; occupancy in [0,1], surface at iso=0.5).
//
// This is NOT an SDF. The field is OCCUPANCY in [0,1]; the surface is where
// density >= iso. The outward normal is -gradient(density).
//
// Isolation (Plan B0 / RENDER_ARCHITECTURE.md §3.x revision): this shader reads ONLY
// the sculpt atlas + camera + the G-buffer. It NEVER touches the analytical SDF/BVH/PT
// path — that is the whole point (avoids the old fused-march bug class).
// ============================================================

// --- GpuUniforms: SAME struct/layout as volume_preview.wgsl (binding 1) ---
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
    scene_aabb_min: vec4<f32>,
    scene_aabb_max: vec4<f32>,
    debug_mode: u32,
    interaction_no_shadows: u32,
    bvh_enabled: u32,
    sdf_aa_enabled: u32,
    matcap_rotation: f32,
    matcap_edge_enabled: u32,
    matcap_edge_threshold: f32,
    matcap_edge_thickness: f32,
    matcap_edge_color: vec3<f32>,
    selected_body_index: u32,
    wp_normal: vec4<f32>,
    wp_origin: vec4<f32>,
    wp_color: vec4<f32>,
    _lod_0: vec4<f32>,
    _lod_1: vec4<f32>,
    _lod_2: vec4<f32>,
    _lod_3: vec4<f32>,
    atlas_body_count: u32,
    _pad_atlas_0: u32,
    _pad_atlas_1: u32,
    _pad_atlas_2: u32,
}

// --- Sculpt params (matches Rust payload; 48 bytes) ---
// Header + body PBR so sparse raymarch matches the triangle-mesh material (no clay-grey flash at 256³).
struct SculptParams {
    brick_res: u32, // bricks per axis (sparse atlas). 0 = dense (unsupported here → empty)
    res: u32,       // grid resolution (cells per axis)
    iso: f32,       // surface threshold (default 0.5)
    bounds: f32,    // world half-extent: grid spans [-bounds, +bounds]^3
    albedo_rough: vec4<f32>, // xyz = base color, w = roughness
    metal_pad: vec4<f32>,    // x = metallic, yzw unused
}

// --- Bindings (sculpt_raymarch_bgl — dedicated layout) ---
@group(0) @binding(1) var<uniform> config: GpuUniforms;
@group(0) @binding(2) var<storage, read> atlas_index: array<u32>;   // brick_res³ tags (EMPTY/FULL/handle)
@group(0) @binding(7) var<storage, read> atlas_pool: array<f32>;    // pool_cap × 4096 f32 brick arena
@group(0) @binding(4) var<uniform> sculpt: SculptParams;
@group(0) @binding(29) var raster_depth_in: texture_2d<f32>;
@group(0) @binding(9) var gbuf_normal_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var gbuf_material_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(11) var gbuf_albedo_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(30) var gbuf_surface_id_out: texture_storage_2d<r32uint, write>;
// C.5: r32float — body_index exact past 2048 (MRT ≤32 B/sample)
@group(0) @binding(32) var gbuf_motion_out: texture_storage_2d<r32float, write>;
@group(0) @binding(34) var gbuf_curvature_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(36) var gbuf_emission_out: texture_storage_2d<rgba16float, write>;

// ============================================================
// Camera ray — VERBATIM from volume_preview.wgsl
// ============================================================
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

// Ray-AABB intersection (tmin, tmax) — VERBATIM from volume_preview.wgsl
fn ray_aabb(ro: vec3<f32>, rd_inv: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> vec2<f32> {
    let t0 = (bmin - ro) * rd_inv;
    let t1 = (bmax - ro) * rd_inv;
    let tmin = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
    let tmax = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
    return vec2(max(tmin, 0.0), tmax);
}

// ============================================================
// Density decode — adapted from voxel_mesh.wgsl `dens` / `sample_world` / `gradient_normal`.
// Occupancy in [0,1]; sparse atlas only (brick_res > 0). The dense branch (brick_res==0)
// has NO buffer bound here, so it returns 0.0 (never reads an unbound buffer).
// ============================================================

// Density read; 0.0 outside the grid. SPARSE atlas decode: brick index from global coords,
// EMPTY/FULL tag from `atlas_index`, else fetch from `atlas_pool` — EXACTLY voxel_mesh.wgsl::dens.
fn dens(x: i32, y: i32, z: i32) -> f32 {
    let r = i32(sculpt.res);
    if (x < 0 || y < 0 || z < 0 || x >= r || y >= r || z >= r) { return 0.0; }
    let br = sculpt.brick_res;
    if (br == 0u) { return 0.0; } // dense path has no bound buffer here — guard, never read
    let bi = (u32(x) >> 4u) + (u32(y) >> 4u) * br + (u32(z) >> 4u) * br * br;
    let tag = atlas_index[bi];
    if (tag == 0xffffffffu) { return 0.0; } // TAG_EMPTY
    if (tag == 0xfffffffeu) { return 1.0; } // TAG_FULL
    return atlas_pool[tag * 4096u + ((u32(x) & 15u) + (u32(y) & 15u) * 16u + (u32(z) & 15u) * 256u)];
}

// World position -> fractional grid coordinate (matches voxel_mesh.wgsl::world_to_grid).
fn world_to_grid(p: vec3<f32>) -> vec3<f32> {
    let s = f32(sculpt.res - 1u);
    return ((p + vec3<f32>(sculpt.bounds)) / (2.0 * sculpt.bounds)) * s;
}

// EMPTY-SPACE SKIP: is the 16-voxel brick containing world point `p` TAG_EMPTY (no surface inside)?
// Out-of-bounds counts as empty. One cheap index read — no trilinear, no pool fetch.
fn brick_empty(p: vec3<f32>) -> bool {
    let gi = vec3<i32>(floor(world_to_grid(p)));
    let r = i32(sculpt.res);
    if (any(gi < vec3<i32>(0)) || any(gi >= vec3<i32>(r))) { return true; }
    let br = sculpt.brick_res;
    let bi = (u32(gi.x) >> 4u) + (u32(gi.y) >> 4u) * br + (u32(gi.z) >> 4u) * br * br;
    if (bi >= arrayLength(&atlas_index)) { return true; }
    return atlas_index[bi] == 0xffffffffu; // TAG_EMPTY
}

// World-space t to EXIT the 16-voxel brick containing `p`, along ray `rd` — the DDA skip distance
// through an empty brick. The surface is never inside an empty brick, so this is always safe.
fn brick_exit_dt(p: vec3<f32>, rd: vec3<f32>) -> f32 {
    let g = world_to_grid(p);
    let vw = (2.0 * sculpt.bounds) / f32(max(sculpt.res - 1u, 1u)); // world units per grid unit
    let bmin = floor(g / 16.0) * 16.0;                              // brick min corner (grid)
    var dt = 1.0e30;
    for (var a = 0u; a < 3u; a = a + 1u) {
        let rda = rd[a];
        if (abs(rda) < 1.0e-9) { continue; }
        let face = select(bmin[a], bmin[a] + 16.0, rda > 0.0);
        let dta = (face - g[a]) * vw / rda;                         // world t to that face
        if (dta > 0.0) { dt = min(dt, dta); }
    }
    return dt;
}

// Trilinear density sample at a world position (matches voxel_mesh.wgsl::sample_world).
fn sample_density(p: vec3<f32>) -> f32 {
    let g = world_to_grid(p);
    let b = vec3<i32>(floor(g));
    let f = g - floor(g);
    let c000 = dens(b.x,     b.y,     b.z);
    let c100 = dens(b.x + 1, b.y,     b.z);
    let c010 = dens(b.x,     b.y + 1, b.z);
    let c110 = dens(b.x + 1, b.y + 1, b.z);
    let c001 = dens(b.x,     b.y,     b.z + 1);
    let c101 = dens(b.x + 1, b.y,     b.z + 1);
    let c011 = dens(b.x,     b.y + 1, b.z + 1);
    let c111 = dens(b.x + 1, b.y + 1, b.z + 1);
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    let y0 = mix(x00, x10, f.y);
    let y1 = mix(x01, x11, f.y);
    return mix(y0, y1, f.z);
}

// Outward normal = -gradient(density) (density decreases outward) — matches voxel_mesh.wgsl::gradient_normal.
fn gradient_normal(p: vec3<f32>, e: f32) -> vec3<f32> {
    let g = vec3<f32>(
        sample_density(p + vec3<f32>(e, 0.0, 0.0)) - sample_density(p - vec3<f32>(e, 0.0, 0.0)),
        sample_density(p + vec3<f32>(0.0, e, 0.0)) - sample_density(p - vec3<f32>(0.0, e, 0.0)),
        sample_density(p + vec3<f32>(0.0, 0.0, e)) - sample_density(p - vec3<f32>(0.0, 0.0, e)),
    );
    let n = -g;
    let l = length(n);
    if (l < 1e-12) { return vec3<f32>(0.0, 1.0, 0.0); }
    return n / l;
}

// ============================================================
// Write all 7 G-buffer outputs (helper) — same contract/formats as volume_preview.wgsl::write_gbuf.
// motion/curvature/emission = zeros (B1 skips temporal/curvature extras).
// ============================================================
fn write_gbuf(coord: vec2<i32>, normal: vec3<f32>, dist: f32,
              albedo: vec3<f32>, rough: f32, metal: f32, flags: f32, sid: u32) {
    textureStore(gbuf_normal_out, coord, vec4(normal, dist));
    textureStore(gbuf_material_out, coord, vec4(rough, metal, 1.0, 1.0));
    textureStore(gbuf_albedo_out, coord, vec4(albedo, flags));
    textureStore(gbuf_surface_id_out, coord, vec4<u32>(sid, 0u, 0u, 0u));
    textureStore(gbuf_emission_out, coord, vec4(0.0));
    textureStore(gbuf_curvature_out, coord, vec4(0.0));
    textureStore(gbuf_motion_out, coord, vec4(0.0));
}

// Surface-id sentinel for "the sculpt" (click-select returns the voxel body).
const SCULPT_SURFACE_ID: u32 = 0x5C01Au; // stable tag; click-select maps it to the voxel node

// ============================================================
// Entry point
// ============================================================
@compute @workgroup_size(8, 8, 1)
fn sculpt_voxel_raymarch(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tex_dim = textureDimensions(gbuf_normal_out);
    if (gid.x >= tex_dim.x || gid.y >= tex_dim.y) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    // Raster depth: the voxel mesh + floor were already written this frame. Skip pixels
    // the raster covers and that are closer than our hit (mesh/floor occludes).
    let raster_sample = textureLoad(raster_depth_in, coord, 0);
    let raster_dist = raster_sample.w;

    let x = f32(gid.x); let y = f32(gid.y);
    let w = f32(tex_dim.x); let h = f32(tex_dim.y);
    let uv_x = (x / w) * 2.0 - 1.0;
    let uv_y = -((y / h) * 2.0 - 1.0);
    let aspect = w / h;
    let cam_ray = camera_ray(uv_x, uv_y, aspect);
    let cam_pos = cam_ray[0];
    let ray_dir = cam_ray[1];

    // Sculpt world AABB = the cube [-bounds, +bounds]^3.
    let bmin = vec3<f32>(-sculpt.bounds);
    let bmax = vec3<f32>( sculpt.bounds);
    let rd_inv = vec3(
        select(1.0 / ray_dir.x, 1e20, abs(ray_dir.x) < 1e-8),
        select(1.0 / ray_dir.y, 1e20, abs(ray_dir.y) < 1e-8),
        select(1.0 / ray_dir.z, 1e20, abs(ray_dir.z) < 1e-8)
    );
    let trange = ray_aabb(cam_pos, rd_inv, bmin, bmax);

    // Ray misses the grid box → nothing to do (leave whatever raster/lite wrote).
    if (!(trange.x < trange.y && trange.y > 0.0)) { return; }

    // Voxel world size = the DDA step base. Occupancy is not Lipschitz, so use a fixed
    // step (~0.75 voxel) — NO over-relaxation (that is an SDF technique).
    let voxel_world = (2.0 * sculpt.bounds) / f32(max(sculpt.res - 1u, 1u));
    let step = voxel_world * 0.75;
    let iso = sculpt.iso;

    // March end: clip against the raster mesh/floor depth so they occlude correctly.
    let t_end = min(trange.y, select(config.far_clip, min(config.far_clip, raster_dist), raster_dist > 0.0));

    var t = trange.x + step * 0.5; // step just past the box entry
    var prev_t = t;
    var hit = false;
    var hit_t = 0.0;
    // With empty-space skipping a grazing 1024³ ray crosses ~110 bricks + a few fine steps near the
    // surface, so 768 is ample. Without the skip the same ray needs ~1400 fine steps and overruns the
    // cap → far/grazing surfaces vanished when zoomed out (the reported bug).
    let max_iter = 768u;
    for (var i = 0u; i < max_iter; i++) {
        if (t >= t_end) { break; }
        let p = cam_pos + ray_dir * t;
        // EMPTY-SPACE SKIP: jump whole empty bricks. max(dt,step) guarantees forward progress even on a
        // brick face; the +step lands just inside the next brick so its near boundary is sampled finely.
        if (brick_empty(p)) {
            prev_t = t;
            t = t + max(brick_exit_dt(p, ray_dir), step) + step;
            continue;
        }
        if (sample_density(p) >= iso) {
            // Crossing between prev_t (outside) and t (inside) — binary refine the surface.
            var lo = prev_t;
            var hi = t;
            for (var j = 0u; j < 6u; j++) {
                let mid = (lo + hi) * 0.5;
                let mp = cam_pos + ray_dir * mid;
                if (sample_density(mp) >= iso) { hi = mid; } else { lo = mid; }
            }
            hit_t = (lo + hi) * 0.5;
            hit = true;
            break;
        }
        prev_t = t;
        t += step;
    }

    if (!hit) { return; }
    // Respect raster occlusion: a raster pixel closer than our hit already won.
    if (raster_dist > 0.0 && hit_t >= raster_dist) { return; }

    let hp = cam_pos + ray_dir * hit_t;
    // Match mesh normals (CPU cell_surface_vertex_d / GPU sn_vertex): 1.0 voxel = half the
    // AA/brush iso band. Wider stencils clip on density plateaus → quantized (ringed) normals.
    let normal = gradient_normal(hp, voxel_world * 1.0);

    // Body material from SculptParams (same PBR as VoxelBody mesh path — no hardcoded clay flash).
    let albedo = sculpt.albedo_rough.xyz;
    let rough = sculpt.albedo_rough.w;
    let metal = sculpt.metal_pad.x;
    let flags = 0.78; // PBR, no selection rim (same encoding as volume_preview)

    write_gbuf(coord, normal, hit_t, albedo, rough, metal, flags, SCULPT_SURFACE_ID);
}
