// AZRA — Post-Processing Compute Shaders (split from compute.wgsl for fast compilation)

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
    // Scene bounding box (ray early termination); .w = march params
    scene_aabb_min: vec4<f32>, // xyz = world min, w = march_relaxation (omega)
    scene_aabb_max: vec4<f32>, // xyz = world max, w = march_bisection_steps
    // Debug rendering
    debug_mode: u32,
    interaction_no_shadows: u32,
    bvh_enabled: u32,       // 0=off, 1=BVH acceleration for large bodies
    sdf_aa_enabled: u32,    // 1 = SDF silhouette anti-aliasing
    // MatCap settings
    matcap_rotation: f32,       // radians
    matcap_edge_enabled: u32,   // 0=off, 1=on
    matcap_edge_threshold: f32, // normal-angle threshold (0-1)
    matcap_edge_thickness: f32, // pixel width (1-3)
    matcap_edge_color: vec3<f32>, // RGB linear
    selected_body_index: u32,
    // Workplane grid (GPU-rendered, behind geometry in Auto mode)
    wp_normal: vec4<f32>,    // xyz = normal, w = spacing (0=disabled)
    wp_origin: vec4<f32>,    // xyz = origin, w = opacity
    wp_color: vec4<f32>,     // rgb = color (linear), w = curvature_enabled (0/1)
    // ---- LOD system (bytes 512-575) — unused here, layout padding only ----
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
    shadow_blur_radius: u32,
    // ---- Atlas multi-body (bytes 576-591) — unused here, layout padding only ----
    atlas_body_count: u32,
    mat_albedo_map_slot: u32,
    mat_albedo_map_strength: f32,
    mat_albedo_map_uv_scale: f32,
    // ---- IBL environment tail (bytes 592-607) — this copy was a 512-byte prefix
    // until the SSR quality pass needed env_rotation/env_lut_res; now full. ----
    env_rotation: f32,       // Y-axis env rotation (radians) — used by the extracted ENV region
    env_lut_res: u32,        // BRDF LUT resolution (0 = analytic Karis fallback)
    _pad_env0: u32,          // layout: matches rt_shadows_on in full GpuUniforms
    fog_has_vol_lights: u32, // P3: layout match (fog pass uses full sdf_shared)
    // ---- SSR quality controls (bytes 608-623, 2026-07-18) ----
    ssr_max_steps: u32,      // DDA march step budget (UI "Quality")
    ssr_intensity: f32,      // 0..1 blend scale on hit confidence (UI "Intensity")
    ssr_thickness: f32,      // base world-space thickness for the depth-crossing test
    tonemapper: u32,         // viewport tonemap curve (see viewport_tonemap; 0 = ACES/Classic)
    // ---- DC-wire deferred gate (bytes 624-639, append-only tail — must match types.rs) ----
    dc_wire_px: f32,
    env_reflection_blur: f32, // extra IBL specular mip bias 0..1 (was _pad_wire0)
    // Progressive-refine sub-pixel jitter, NDC units (was _pad_wire1/2 — same
    // offset/size). Subtracted in camera_ray so SSR / compositor reconstruction
    // matches the jittered G-buffer. 0 when not accumulating.
    pbr_jitter_x: f32,
    pbr_jitter_y: f32,
    // Fog ambient tint (bytes 640-655 — layout parity with types.rs)
    fog_color: vec3<f32>,
    // MatCap material-colour overlay amount — unused here, kept for layout parity.
    matcap_tint: f32,
}

// Triangular-PDF dither noise — raw [-1, 1] output.
// Amplitude scaled by config.dither_strength (pre-multiplied with 1/(2^bits-1) on CPU).
fn dither_tri(coord: vec2<f32>) -> f32 {
    let n1 = fract(sin(dot(coord, vec2(122.451, 763.527))) * 43758.5453);
    let n2 = fract(sin(dot(coord, vec2(459.313, 217.891))) * 29816.1267);
    return n1 + n2 - 1.0;
}

// Generate camera ray. cam_pos.w > 0 = ortho mode (w = half-width).
// cam_forward.w = tan(fov/2) for perspective FOV control (1.0 = 90°).
fn camera_ray(uv_x_in: f32, uv_y_in: f32, aspect: f32) -> array<vec3<f32>, 2> {
    // Progressive-refine un-jitter — must match sdf_shared.wgsl camera_ray exactly.
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

// ============================================================
// Bindings
// ============================================================
@group(0) @binding(1) var<uniform> config: GpuUniforms;
@group(0) @binding(5) var hdr_input: texture_2d<f32>;
@group(0) @binding(6) var ldr_output: texture_storage_2d<{{LDR_FORMAT}}, write>;
@group(0) @binding(7) var bloom_read: texture_2d<f32>;
@group(0) @binding(8) var bloom_write: texture_storage_2d<rgba16float, write>;
@group(0) @binding(12) var gbuf_normal_in: texture_2d<f32>;
@group(0) @binding(13) var gbuf_material_in: texture_2d<f32>;
@group(0) @binding(14) var gbuf_albedo_in: texture_2d<f32>;
@group(0) @binding(15) var ssr_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(16) var ssr_input: texture_2d<f32>;
@group(0) @binding(18) var fog_input: texture_2d<f32>;
@group(0) @binding(19) var upsample_input: texture_2d<f32>;
@group(0) @binding(20) var upsample_output: texture_storage_2d<{{LDR_FORMAT}}, write>;
@group(0) @binding(31) var gbuf_surface_id_in: texture_2d<u32>;
@group(0) @binding(33) var gbuf_motion_in: texture_2d<f32>;
@group(0) @binding(35) var gbuf_curvature_in: texture_2d<f32>;
@group(0) @binding(37) var gbuf_emission_in: texture_2d<f32>;
@group(0) @binding(40) var<storage, read> comp: array<f32>;
// SSGI half-res signed radiance delta (SSGI_PLAN.md). rgb = delta, a = the
// half-res guide depth carried in-band so the upsample below never has to
// re-derive a guide coordinate.
@group(0) @binding(62) var ssgi_in: texture_2d<f32>;
// Progressive PBR refine (PROGRESSIVE_PBR_PLAN.md Phase 1): the running mean of
// fully composited HDR frames. `in` = last frame's mean (copied from `out` after
// tonemap — the pt_accum copy-chain pattern; compute can't read+write one
// texture). Only pass_tonemap references these, so only tonemap_bgl carries them.
@group(0) @binding(63) var pbr_accum_in: texture_2d<f32>;
@group(0) @binding(64) var pbr_accum_out: texture_storage_2d<rgba16float, write>;

// Auto reflection probe (PROGRESSIVE_PBR_PLAN.md Phase 3): scene-centered cubemap
// captured through the real raster+deferred pipeline while idle (probe_bake.rs).
// Consumed ONLY by pass_ssr as the (1 − confidence) fallback — where the screen-
// space march can't see (off-screen geometry), the probe swaps in for the plain
// env IBL with the same energy-conserving envelope. Only ssr_bgl carries these.
struct ProbeParams {
    center_enabled: vec4<f32>, // xyz = probe center (world), w = 1 when baked+on
    extent_mips: vec4<f32>,    // xyz = proxy AABB half-extents, w = max mip index
};
@group(0) @binding(65) var probe_cube: texture_cube<f32>;
@group(0) @binding(66) var probe_samp: sampler;
@group(0) @binding(67) var<uniform> probe_params: ProbeParams;

// Parallax-corrected probe lookup: intersect the reflected ray with the probe's
// proxy AABB and sample toward the intersection point — a flat floor then reflects
// the actual wall it faces instead of an infinitely-distant one. Mip by roughness
// (box-prefiltered chain). Returns rgb + a = 1 when the probe contributed.
fn probe_sample(refl: vec3<f32>, world: vec3<f32>, roughness: f32) -> vec4<f32> {
    if (probe_params.center_enabled.w < 0.5) { return vec4(0.0); }
    let c = probe_params.center_enabled.xyz;
    let e = probe_params.extent_mips.xyz;
    let inv = 1.0 / refl; // IEEE inf on zero components — min/max below handle it
    let t1 = (c - e - world) * inv;
    let t2 = (c + e - world) * inv;
    // C4: full slab test — require a forward intersection with the proxy AABB.
    // Surfaces outside the box previously used a misaimed tmax-only direction
    // (and a NaN path when the ray missed entirely). Fall back to un-parallaxed
    // `refl` when there is no valid hit in front of the surface.
    let tmin = max(max(min(t1.x, t2.x), min(t1.y, t2.y)), min(t1.z, t2.z));
    let tmax = min(min(max(t1.x, t2.x), max(t1.y, t2.y)), max(t1.z, t2.z));
    var dir = refl;
    if (tmax > max(tmin, 0.0)) {
        dir = normalize(world + refl * tmax - c);
    }
    let max_mip = probe_params.extent_mips.w;
    // D4 Karis-style mip remap: continuous blur character at traced→probe→env
    // hand-offs (linear roughness·max_mip undersamples mid-gloss).
    let mip = clamp(roughness * (1.7 - 0.7 * roughness) * max_mip, 0.0, max_mip);
    // Near-matte fade (owner wall-smudge screenshot 2026-08-07): past
    // SSR_ROUGH_MAX the specular lobe is so wide that the low-res probe reads as
    // smudges painted on flat walls — and deferred's smooth IBL + the SH light
    // probe already own that regime. Hand over instead of smearing. The fade
    // rides alpha, which probe_delta already multiplies.
    let fade = 1.0 - smoothstep(SSR_ROUGH_MAX, 0.95, roughness);
    return vec4(textureSampleLevel(probe_cube, probe_samp, dir, mip).rgb, fade);
}

// Depth+normal-aware bilinear upsample of the half-res SSGI delta at a full-res
// pixel. Half texel h is centered on full pixel 2h (the SSAO convention the
// gather uses), so the bilinear position is coord * 0.5.
//
// Two gates, and both matter for a COLOUR payload in a way they do not for a
// scalar AO factor: the squared-relative DEPTH weight (from ssao_factor) stops
// radiance crossing a silhouette, and the pow(ndot, 8) NORMAL weight (from
// pt_upscale) stops it crossing a crease — an AO factor leaking is a wrong shade
// of grey, a radiance leak is a coloured fringe.
//
// Total rejection returns vec3(0) — the ADDITIVE identity, the exact analogue of
// ssao_factor's `return 1.0`. pt_upscale instead falls back to the nearest
// sample, which commits to a value it just decided it could not trust; for a
// signed delta that would paint light onto a surface with no evidence for it.
fn ssgi_delta(coord: vec2<i32>, depth: f32, normal: vec3<f32>) -> vec3<f32> {
    let hdim = vec2<i32>(textureDimensions(ssgi_in));
    let hf = vec2<f32>(coord) * 0.5;
    let base = vec2<i32>(floor(hf));
    let f = hf - vec2<f32>(base);
    let bw = vec4<f32>(
        (1.0 - f.x) * (1.0 - f.y),
        f.x * (1.0 - f.y),
        (1.0 - f.x) * f.y,
        f.x * f.y,
    );
    var offs = array<vec2<i32>, 4>(
        vec2<i32>(0, 0), vec2<i32>(1, 0), vec2<i32>(0, 1), vec2<i32>(1, 1),
    );
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var i = 0; i < 4; i++) {
        let c = clamp(base + offs[i], vec2<i32>(0, 0), hdim - vec2<i32>(1, 1));
        let s = textureLoad(ssgi_in, c, 0);
        if (s.a <= 1e-4) { continue; }              // background / undispatched
        let rel = abs(s.a - depth) / max(depth, 1e-3);
        let wd = exp(-rel * rel * 400.0);           // ~5% relative depth diff -> e^-1
        let gn = textureLoad(gbuf_normal_in, c * 2, 0).xyz;
        let ndot = max(dot(normalize(gn + vec3(1e-5)), normalize(normal + vec3(1e-5))), 0.0);
        let wn = pow(ndot, 8.0);
        let w = bw[i] * wd * wn;
        sum += s.rgb * w;
        wsum += w;
    }
    if (wsum < 1e-4) { return vec3<f32>(0.0); }
    return sum / wsum;
}

// ============================================================
// SSR — Screen-Space Reflections (screen-space DDA march)
// Quality pass 2026-07-18: pixel-space DDA stepping (kills fixed-world-step
// over/undershoot banding), 4-iteration binary refinement (crisp contacts),
// roughness cone-blur resolve + smooth roughness fade-out (IBL takes over),
// smooth screen-border / backface / toward-camera rejection, and an
// energy-conserving composite: the output is a DELTA
//     rgb = envelope × confidence × (traced radiance − IBL specular radiance)
//     a   = confidence
// added to HDR in pass_tonemap. envelope = split-sum second term (F0·A + B,
// the EXACT term deferred applied — extracted ENV_BRDF region + same LUT), and
// ibl term = sample_env_mip(refl, rough·6) × brightness (the EXACT deferred
// IBL specular). At confidence 1 the IBL specular is fully replaced by the
// traced reflection; at 0 nothing changes. Energy is never double-added, and
// SSAO never touches it (specular ≠ ambient — the 17e contract).
// ============================================================

// Tunables (documented, adaptive where it matters):
const SSR_ROUGH_FADE_START: f32 = 0.45; // confidence starts fading here…
const SSR_ROUGH_MAX: f32 = 0.7;         // …and is 0 here (IBL takes over)
const SSR_MAX_WORLD_DIST: f32 = 60.0;   // reflection ray length cap (world units)
const SSR_NEAR_Z: f32 = 0.05;           // near-plane clip for the marched segment
const SSR_EDGE_FADE_UV: f32 = 0.08;     // screen-border fade band (uv units)
const SSR_REFINE_ITERS: u32 = 4u;       // binary-search iterations on crossing
const SSR_CONE_SCALE: f32 = 0.35;       // cone-blur radius = rough × scale × hit px dist

// Deterministic per-pixel hash (decorrelates DDA stepping + cone tap rotation).
// Frame-static by default — SSR output must be deterministic frame-to-frame.
// EXCEPT while the progressive-refine accumulator is averaging (post_flags bit
// 256, Phase 2): a per-frame offset then varies the DDA phase + cone rotation so
// the 1/N mean converges to a properly filtered reflection. Jitter without an
// accumulator boils — the accumulate bit is exactly the guarantee one exists.
fn ssr_hash(coord: vec2<i32>) -> f32 {
    var p = vec2<f32>(coord);
    if ((config.post_flags & 256u) != 0u) {
        p += f32(config.pt_accumulated % 1024u) * vec2(17.13, 9.77);
    }
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

// Project a world point to (uv.xy, forward-z). Perspective AND ortho branches —
// must match camera_ray()'s conventions exactly.
fn ssr_project(world: vec3<f32>, aspect: f32) -> vec3<f32> {
    let rel = world - config.cam_pos.xyz;
    let z = dot(rel, config.cam_forward.xyz);
    let ortho_hw = config.cam_pos.w;
    let fov_scale = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
    var sx: f32; var sy: f32;
    if (ortho_hw > 0.0) {
        sx = dot(rel, config.cam_right.xyz) / (ortho_hw * aspect);
        sy = dot(rel, config.cam_up.xyz) / ortho_hw;
    } else {
        let zz = max(z, 1e-4);
        sx = dot(rel, config.cam_right.xyz) / (zz * aspect * fov_scale);
        sy = dot(rel, config.cam_up.xyz) / (zz * fov_scale);
    }
    return vec3((sx + 1.0) * 0.5, (-sy + 1.0) * 0.5, z);
}

// Camera-distance metric matching the G-buffer depth semantics: perspective
// stores RAY DISTANCE from the camera; ortho stores forward march distance
// (per-pixel origins are offset perpendicular to forward, so forward-z matches).
fn ssr_metric(world: vec3<f32>) -> f32 {
    let rel = world - config.cam_pos.xyz;
    let z = dot(rel, config.cam_forward.xyz);
    return select(length(rel), z, config.cam_pos.w > 0.0);
}

// Perspective-correct point along the marched segment at screen-space fraction u.
// Screen-linear interpolation corresponds to linear interpolation of P/z and 1/z
// (standard perspective-correct attribute interpolation); ortho is plain lerp.
fn ssr_point_at(p0: vec3<f32>, p1: vec3<f32>, z0: f32, z1: f32, u: f32) -> vec3<f32> {
    if (config.cam_pos.w > 0.0) { return mix(p0, p1, u); }
    let inv_z = mix(1.0 / z0, 1.0 / z1, u);
    return mix(p0 / z0, p1 / z1, u) / inv_z;
}

@compute @workgroup_size(8, 8, 1)
fn pass_ssr(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(ssr_output);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    let coord = vec2<i32>(gid.xy);

    let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
    let gbuf_m = textureLoad(gbuf_material_in, coord, 0);
    let normal = gbuf_n.xyz;
    let depth = gbuf_n.w;
    let roughness = gbuf_m.x;
    let metallic = gbuf_m.y;

    // Skip background — nothing reflects there.
    if (depth <= 0.0) {
        textureStore(ssr_output, coord, vec4(0.0));
        return;
    }

    // Reconstruct world position and reflection direction
    // D4: pixel-center (+0.5) so ray reconstruction matches deferred/raster NDC.
    // Parentheses matter: `-(u*2-1)`, not `(-u)*2-1`.
    let w = f32(dim.x); let h = f32(dim.y);
    let uv_x = ((f32(gid.x) + 0.5) / w) * 2.0 - 1.0;
    let uv_y = -(((f32(gid.y) + 0.5) / h) * 2.0 - 1.0);
    let aspect = w / h;
    let ssr_cam = camera_ray(uv_x, uv_y, aspect);
    let cam_origin = ssr_cam[0];          // per-pixel origin (ortho-correct)
    let ray_dir = ssr_cam[1];
    let hit_pos = cam_origin + ray_dir * depth;
    let view_dir = normalize(cam_origin - hit_pos);
    let reflect_dir = reflect(-view_dir, normal);

    // Split-sum specular envelope — the EXACT weight deferred applied to its
    // IBL specular (same env_brdf: LUT when uploaded, Karis until then).
    let albedo = textureLoad(gbuf_albedo_in, coord, 0).rgb;
    let ndv = max(dot(normal, view_dir), 0.0);
    let f0 = mix(vec3(0.04), albedo, metallic);
    let ab = env_brdf(ndv, roughness);
    let envelope = f0 * ab.x + vec3(ab.y);
    // No visible specular energy → nothing to replace (also skips most rough
    // dielectrics — the perf gate that replaced the old hard metallic cutoff).
    if (max(envelope.x, max(envelope.y, envelope.z)) < 0.005) {
        textureStore(ssr_output, coord, vec4(0.0));
        return;
    }

    // C1: deferred applied envelope × spec_occ × ibl. The SSR/probe delta must
    // subtract/add the OCCLUDED term — otherwise conf=1 over-darkens corners and
    // the probe over-brights occluded metals when there is no HDRI.
    let ssao_f = ssao_factor(coord, depth);
    let spec_occ = specular_occlusion(ndv, ssao_f, roughness);

    // The IBL specular radiance deferred used for this pixel (to be subtracted
    // out by hit confidence — the per-pixel IBL fallback lives in this delta).
    var ibl_refl = vec3(0.0);
    if (config.env_width > 0u) {
        // Match deferred specular IBL: roughness mips + independent reflection blur.
        let max_mip = f32(ENV_MIP_LEVELS - 1u);
        let mip = clamp(roughness * max_mip + config.env_reflection_blur * max_mip, 0.0, max_mip);
        ibl_refl = sample_env_mip(reflect_dir, mip) * config.env_brightness;
    }

    // Auto reflection probe fallback (Phase 3): the probe-for-env swap applied to
    // whatever fraction of this pixel SSR cannot supply — full weight where the
    // march is skipped or misses, (1 − conf) on partial hits. Same envelope, same
    // delta form as the traced swap → energy-conserving by construction.
    // Opacity matches the confidence term's scaling (deferred alpha-blends its
    // specular by mat_opacity; gbuf_m.w > 1 = emission → 1).
    let opacity = min(gbuf_m.w, 1.0);
    let probe = probe_sample(reflect_dir, hit_pos, roughness);
    let probe_delta = envelope * spec_occ * (probe.a * opacity) * (probe.rgb - ibl_refl);

    // Past the roughness fade-out the march never runs — probe/IBL take over fully.
    if (roughness > SSR_ROUGH_MAX) {
        textureStore(ssr_output, coord, vec4(probe_delta, 0.0));
        return;
    }

    // Rays reflecting back toward the camera have no on-screen information —
    // fade them out smoothly instead of showing streak garbage.
    let cam_fade = smoothstep(-0.95, -0.55, dot(reflect_dir, ray_dir));
    if (cam_fade <= 0.0) {
        textureStore(ssr_output, coord, vec4(probe_delta, 0.0));
        return;
    }

    // ── Screen-space DDA segment setup ──
    // March between the projections of the segment start (small offset off the
    // surface) and end (ray cap, clipped to the near plane), stepping uniformly
    // in SCREEN space — equal pixel coverage per step, no world-step banding.
    let p_start = hit_pos + normal * 0.02 + reflect_dir * 0.01;
    var t_end = SSR_MAX_WORLD_DIST;
    let dz = dot(reflect_dir, config.cam_forward.xyz);
    if (config.cam_pos.w <= 0.0) {
        let z_start = dot(p_start - config.cam_pos.xyz, config.cam_forward.xyz);
        if (z_start + t_end * dz < SSR_NEAR_Z) {
            // Clip the endpoint to the near plane (ray heads behind the camera)
            t_end = max((SSR_NEAR_Z - z_start) / dz, 1e-3);
        }
    }
    let p_end = p_start + reflect_dir * t_end;
    let s0 = ssr_project(p_start, aspect);
    let s1 = ssr_project(p_end, aspect);
    let px0 = s0.xy * vec2(w, h);
    let px1 = s1.xy * vec2(w, h);
    let screen_len_px = max(length(px1 - px0), 1e-3);

    let max_steps = clamp(config.ssr_max_steps, 8u, 128u);
    // ≥2px stride: below that adjacent steps land on the same texel (wasted).
    let du = max(1.0 / f32(max_steps), 2.0 / screen_len_px);
    let jitter = ssr_hash(coord); // deterministic per-pixel phase

    var hit = false;
    var u_lo = 0.0;      // last screen-fraction in FRONT of the depth buffer
    var u_hi = 0.0;      // first screen-fraction BEHIND it (within thickness)
    var m_prev = ssr_metric(p_start);
    var uv_hit = vec2(0.0);

    for (var i = 0u; i < max_steps; i++) {
        let u = min((f32(i) + 0.5 + (jitter - 0.5)) * du, 1.0);
        let p = ssr_point_at(p_start, p_end, s0.z, s1.z, u);
        let s = ssr_project(p, aspect);
        if (s.x < 0.0 || s.x > 1.0 || s.y < 0.0 || s.y > 1.0 || s.z < SSR_NEAR_Z) { break; }
        let sc = vec2<i32>(i32(s.x * w), i32(s.y * h));
        let sample_n = textureLoad(gbuf_normal_in, sc, 0);
        let sample_depth = sample_n.w;
        let m = ssr_metric(p);
        if (sample_depth > 0.0) {
            let delta = m - sample_depth;
            // Adaptive thickness: base + this step's own depth advance (coarse
            // strides need a thicker acceptance band) + 1% depth (far surfaces).
            let thick = config.ssr_thickness + max(m - m_prev, 0.0) * 1.5 + 0.01 * sample_depth;
            // Backface rejection: a valid hit faces AGAINST the ray. This also
            // structurally rejects self-hits (own surface faces WITH the ray).
            if (delta > 0.0 && delta < thick && dot(sample_n.xyz, reflect_dir) < 0.0) {
                hit = true;
                u_hi = u;
                uv_hit = s.xy;
                break;
            }
        }
        // Not a hit (in front, sky, backface, or passed far behind a thin
        // occluder): advance the refinement floor so the binary search only
        // ever bisects the LAST stride — never re-finds a rejected surface.
        u_lo = u;
        m_prev = m;
        if (u >= 1.0) { break; }
    }

    if (!hit) {
        // March saw nothing on screen — the probe supplies the whole reflection.
        textureStore(ssr_output, coord, vec4(probe_delta, 0.0));
        return;
    }

    // ── Binary-search refinement: crisp contact points ──
    for (var k = 0u; k < SSR_REFINE_ITERS; k++) {
        let um = 0.5 * (u_lo + u_hi);
        let p = ssr_point_at(p_start, p_end, s0.z, s1.z, um);
        let s = ssr_project(p, aspect);
        let sc = vec2<i32>(clamp(i32(s.x * w), 0, i32(dim.x) - 1), clamp(i32(s.y * h), 0, i32(dim.y) - 1));
        let sample_depth = textureLoad(gbuf_normal_in, sc, 0).w;
        if (sample_depth > 0.0 && ssr_metric(p) > sample_depth) {
            u_hi = um;
            uv_hit = s.xy;
        } else {
            u_lo = um;
        }
    }

    // ── Roughness-aware resolve: cone-blur taps around the hit ──
    // Blur radius grows with roughness AND the marched pixel distance (cone
    // aperture): a mirror stays sharp, glossy contacts blur with distance.
    let hit_px = uv_hit * vec2(w, h);
    let march_px = length(hit_px - vec2<f32>(coord));
    let radius_px = clamp(roughness * SSR_CONE_SCALE * march_px, 0.0, 20.0);
    let ang = ssr_hash(coord + vec2(17, 59)) * 6.2831853;
    let ca = cos(ang); let sa = sin(ang);
    var ref_color = vec3(0.0);
    var tap_w = 0.0;
    for (var t = 0u; t < 4u; t++) {
        var off = vec2(0.0);
        if (t == 1u) { off = vec2(1.0, 0.0); }
        if (t == 2u) { off = vec2(-0.5, 0.866); }
        if (t == 3u) { off = vec2(-0.5, -0.866); }
        let rot = vec2(off.x * ca - off.y * sa, off.x * sa + off.y * ca) * radius_px;
        let tc = vec2<i32>(
            clamp(i32(hit_px.x + rot.x), 0, i32(dim.x) - 1),
            clamp(i32(hit_px.y + rot.y), 0, i32(dim.y) - 1));
        // Weight taps by scene coverage (skip taps that fell on background)
        let tap_depth = textureLoad(gbuf_normal_in, tc, 0).w;
        let tw = select(select(0.0, 1.0, tap_depth > 0.0), 1.0, t == 0u);
        ref_color += textureLoad(hdr_input, tc, 0).rgb * tw;
        tap_w += tw;
    }
    ref_color /= max(tap_w, 1.0);

    // ── Confidence (all fades smooth — no hard cutoffs) ──
    let edge_d = min(min(uv_hit.x, 1.0 - uv_hit.x), min(uv_hit.y, 1.0 - uv_hit.y));
    let edge_fade = smoothstep(0.0, SSR_EDGE_FADE_UV, edge_d);
    let dist_fade = 1.0 - smoothstep(0.6, 1.0, u_hi);
    let rough_fade = 1.0 - smoothstep(SSR_ROUGH_FADE_START, SSR_ROUGH_MAX, roughness);
    // Opacity: deferred alpha-blends its specular by mat_opacity — the swap must
    // scale the same way or transparent pixels over-subtract (opacity is computed
    // with the probe fallback above).
    let conf = edge_fade * dist_fade * rough_fade * cam_fade * opacity
             * clamp(config.ssr_intensity, 0.0, 1.0);

    // Energy-conserving delta (see header comment). Negative components are
    // legitimate — rgba16float holds them; pass_tonemap adds this to HDR.
    // The probe fills the (1 − conf) remainder the march couldn't supply, so the
    // reflection cross-fades traced → probe → env instead of dropping to env at
    // every confidence fade (screen edge, march distance, roughness).
    // C1: both the traced swap and the probe remainder carry spec_occ (probe_delta
    // already does; conf path multiplies here).
    let delta_rgb = envelope * spec_occ * conf * (ref_color - ibl_refl) + probe_delta * (1.0 - conf);
    textureStore(ssr_output, coord, vec4(delta_rgb, conf));
}

fn aces_narkowicz(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    return saturate((x * (a * x + b)) / (x * (c * x + d) + e));
}

fn filmic_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = max(vec3(0.0), x - 0.004);
    return (a * (6.2 * a + 0.5)) / (a * (6.2 * a + 1.7) + 0.06);
}

fn agx_tonemap(color_in: vec3<f32>) -> vec3<f32> {
    // Canonical AgX (Troy Sobotka) — inset matrix → log2 → polynomial sigmoid → outset matrix
    let agx_mat = mat3x3<f32>(
        vec3(0.842479062253094,  0.0423282422610123, 0.0423756549057051),
        vec3(0.0784335999999992, 0.878468636469772,  0.0784336),
        vec3(0.0792237451477643, 0.0791661274605434, 0.879142973793104)
    );
    let val = max(color_in, vec3(1e-10));
    var x = agx_mat * val;
    x = clamp(log2(x), vec3(-12.47393), vec3(4.026069));
    x = (x - vec3(-12.47393)) / vec3(16.5);
    let x2 = x * x;
    let x4 = x2 * x2;
    x = 15.5 * x4 * x2
        - 40.14  * x4 * x
        + 31.96  * x4
        - 6.868  * x2 * x
        + 0.4298 * x2
        + 0.1191 * x
        - 0.00232;
    let agx_mat_inv = mat3x3<f32>(
        vec3( 1.19687900512017,  -0.0528968517574562, -0.0529716355144438),
        vec3(-0.0980208811401368, 1.15190312990417,   -0.0980434501171241),
        vec3(-0.0990297440797205,-0.0989611768448433,  1.15107367264116)
    );
    return saturate(agx_mat_inv * x);
}

fn reinhard_tonemap(x: vec3<f32>) -> vec3<f32> {
    return x / (x + vec3(1.0));
}

// Hable / Uncharted 2 filmic operator — LINEAR output (linear_to_srgb runs
// after; unlike `filmic_tonemap` above, which is Hejl-Dawson with gamma baked
// in and is kept verbatim for the legacy compositor path). Classic shoulder/
// toe fit, exposure bias 2, normalized to white point W = 11.2 so a bright
// HDRI sun rolls off to exactly 1.0 instead of clipping to a flat patch.
fn hable_partial(x: vec3<f32>) -> vec3<f32> {
    let ha = 0.15; let hb = 0.50; let hc = 0.10; let hd = 0.20; let he = 0.02; let hf = 0.30;
    return ((x * (ha * x + vec3(hc * hb)) + vec3(hd * he))
          / (x * (ha * x + vec3(hb)) + vec3(hd * hf))) - vec3(he / hf);
}

fn hable_tonemap(x: vec3<f32>) -> vec3<f32> {
    let curr = hable_partial(x * 2.0);
    let white_scale = vec3(1.0) / hable_partial(vec3(11.2));
    return saturate(curr * white_scale);
}

// Viewport tonemapper selector (config.tonemapper — Render panel "Tonemapper").
//   0 = ACES fitted (Narkowicz) — the "Classic" DEFAULT, bit-identical to the
//       pre-selector hardcoded curve (existing scenes keep their look).
//   1 = AgX (best hue preservation under bright saturated light)
//   2 = Filmic (Hable / Uncharted 2)      3 = Reinhard
//   4 = PBR Neutral (Khronos)             5+ = None / Linear (raw clamp, debug)
// Contract: input is EXPOSED linear HDR; output is LINEAR [0,1] — linear_to_srgb
// runs after the curve. (The compositor's own selector at comp[92] is separate
// and unchanged, including its legacy gamma-baked filmic.)
fn viewport_tonemap(x: vec3<f32>, tm: u32) -> vec3<f32> {
    switch (tm) {
        case 0u: { return aces_narkowicz(x); }
        case 1u: { return agx_tonemap(x); }
        case 2u: { return hable_tonemap(x); }
        case 3u: { return reinhard_tonemap(x); }
        case 4u: { return saturate(pbr_neutral_tonemap(x)); }
        default: { return saturate(x); }
    }
}

fn pbr_neutral_tonemap(color_in: vec3<f32>) -> vec3<f32> {
    // Khronos PBR Neutral — minimal color shift, preserves base color fidelity
    let start_compression = 0.8 - 0.04;
    let desaturation = 0.15;
    var color = color_in;
    let x = min(color.r, min(color.g, color.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    color -= vec3(offset);
    let peak = max(color.r, max(color.g, color.b));
    if (peak < start_compression) { return color; }
    let d = 1.0 - start_compression;
    let new_peak = 1.0 - d * d / (peak + d - start_compression);
    color *= new_peak / peak;
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(color, vec3(new_peak), g);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

// ============================================================
// Compositor — binding 40 storage buffer
// ============================================================

// Compositor buffer layout (floats):
// [0] enabled  [1] time  [2] vp_w  [3] vp_h
// --- Bloom [4..11] ---
// [4] bloom_on  [5] threshold  [6] intensity  [7] radius
// [8] tint_r  [9] tint_g  [10] tint_b  [11] anamorphic
// --- DoF [12..17] ---
// [12] dof_on  [13] focus_dist  [14] aperture  [15] max_blur  [16] bokeh_shape  [17] chromatic
// --- Motion Blur [18..22] ---
// [18] mblur_on  [19] intensity  [20] samples  [21] max_length  [22] shutter_angle
// --- Chromatic Aberration [23..25] ---
// [23] ca_on  [24] intensity  [25] mode
// --- Sharpen [26..30] ---
// [26] sharpen_on  [27] strength  [28] radius  [29] threshold  [30] clarity
// --- Lens Distortion [31..34] ---
// [31] distort_on  [32] barrel  [33] chromatic  [34] scale
// --- Fog [35..44] ---
// [35] fog_on  [36] fog_r  [37] fog_g  [38] fog_b  [39] density
// [40] start  [41] end  [42] mode  [43] height_density  [44] height_base
// --- Outline [45..54] ---
// [45] outline_on  [46] source  [47] width  [48] color_r  [49] color_g
// [50] color_b  [51] color_a  [52] normal_thresh  [53] depth_thresh  [54] surface_id_on
// [55] fill_mode  [56] cel_steps
// --- Vignette [57..63] ---
// [57] vignette_on  [58] intensity  [59] color_r  [60] color_g  [61] color_b
// [62] shape  [63] radius  [64] softness  [65] blend
// --- Film Grain [66..73] ---
// [66] grain_on  [67] noise_type  [68] intensity  [69] size
// [70] monochromatic  [71] animate  [72] blend  [73] luminance_respond
// --- Color Grading [74..91] ---
// [74] grade_on  [75] hue_shift  [76] saturation  [77] vibrance
// [78] brightness  [79] contrast  [80] gamma  [81] highlights  [82] shadows
// [83] bal_shad_r  [84] bal_shad_g  [85] bal_shad_b
// [86] bal_mid_r  [87] bal_mid_g  [88] bal_mid_b
// [89] bal_hi_r  [90] bal_hi_g  [91] bal_hi_b
// --- Color Management [92..97] ---
// [92] tonemapper  [93] exposure  [94] auto_exposure  [95] wb_temp  [96] wb_tint  [97] lut_intensity
// --- Mask params per effect (11 effects × 6 floats = 66) [100..165] ---
// mask_type, invert, p0, p1, p2, p3  (per effect, same order as above)

fn comp_f(i: u32) -> f32 { return comp[i]; }

// ============================================================
// Compositor Masks — Sprint 3
// ============================================================

// Simple 3D hash for noise
fn hash3(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn noise3d(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(mix(hash3(i), hash3(i + vec3(1.0, 0.0, 0.0)), u.x),
                   mix(hash3(i + vec3(0.0, 1.0, 0.0)), hash3(i + vec3(1.0, 1.0, 0.0)), u.x), u.y),
               mix(mix(hash3(i + vec3(0.0, 0.0, 1.0)), hash3(i + vec3(1.0, 0.0, 1.0)), u.x),
                   mix(hash3(i + vec3(0.0, 1.0, 1.0)), hash3(i + vec3(1.0, 1.0, 1.0)), u.x), u.y), u.z);
}

fn fbm_noise(p: vec3<f32>, octaves: u32, persistence: f32, lacunarity: f32) -> f32 {
    var val = 0.0; var amp = 0.5; var freq = 1.0;
    var pp = p;
    for (var o = 0u; o < octaves; o++) {
        val += amp * noise3d(pp * freq);
        amp *= persistence;
        freq *= lacunarity;
    }
    return val;
}

// Evaluate a single mask source for a pixel
// mask_base = offset into comp[] for this mask's 6 floats
// world_pos = reconstructed world position, norm = surface normal, depth = linear depth
// surface_id = per-pixel surface ID, curvature = (k1, k2), uv = screen UV
fn eval_comp_mask(mask_base: u32, world_pos: vec3<f32>, norm: vec3<f32>, depth: f32,
             surface_id: u32, curvature: vec2<f32>, uv: vec2<f32>, view_dir: vec3<f32>) -> f32 {
    let mtype = u32(comp[mask_base]);
    let invert = comp[mask_base + 1u];
    let p0 = comp[mask_base + 2u];
    let p1 = comp[mask_base + 3u];
    let p2 = comp[mask_base + 4u];
    let p3 = comp[mask_base + 5u];

    var m = 1.0;
    switch (mtype) {
        case 0u: { m = 1.0; } // None
        case 1u: { // 3D Noise — p0=scale, p1=octaves, p2=persistence, p3=contrast
            let scale = max(p0, 0.01);
            let raw = fbm_noise(world_pos * scale, u32(max(p1, 1.0)), max(p2, 0.01), 2.0);
            m = saturate((raw - 0.5) * max(p3, 0.1) + 0.5);
        }
        case 2u: { // Surface ID — p0=target_id (match if equal)
            m = select(0.0, 1.0, surface_id == u32(p0));
        }
        case 3u: { // SDF Distance — p0=near, p1=far (ramp)
            // Using depth as proxy (actual SDF distance not stored per-pixel in tonemap)
            let range = max(p1 - p0, 0.001);
            m = saturate((depth - p0) / range);
        }
        case 4u: { // Curvature — p0=min_k, p1=max_k
            let k = length(curvature);
            let range = max(p1 - p0, 0.001);
            m = saturate((k - p0) / range);
        }
        case 5u: { // Depth — p0=near, p1=far
            let range = max(p1 - p0, 0.001);
            m = saturate((depth - p0) / range);
        }
        case 6u: { // Normal Direction — p0=threshold
            let ndotv = max(dot(norm, view_dir), 0.0);
            m = select(0.0, 1.0, ndotv > p0);
        }
        case 7u: { // Fresnel — p0=power
            let ndotv = max(dot(norm, view_dir), 0.0);
            m = pow(1.0 - ndotv, max(p0, 0.1));
        }
        case 8u: { // Vignette — p0=radius, p1=softness
            let center_uv = uv - vec2(0.5);
            let d = length(center_uv) * 2.0;
            m = smoothstep(max(p0, 0.01), max(p0, 0.01) + max(p1, 0.01), d);
        }
        case 9u: { // Material ID — p0=target_id (match if equal to material_id channel)
            // Material ID is packed with surface ID in gbuf; for now, use surface_id as proxy
            m = select(0.0, 1.0, surface_id == u32(p0));
        }
        case 10u: { // Texture — p0=slot, p1=channel (placeholder: use UV gradient)
            let ch = u32(p1);
            let tex_val = vec4(uv.x, uv.y, 0.5, 1.0); // Placeholder until texture binding
            m = tex_val[ch];
        }
        default: { m = 1.0; }
    }
    // Invert if requested
    if (invert > 0.5) { m = 1.0 - m; }
    return m;
}

// Get the mask value for effect at index eidx (0=bloom, 1=dof, ...)
fn get_effect_mask(eidx: u32, world_pos: vec3<f32>, norm: vec3<f32>, depth: f32,
                   sid: u32, curv: vec2<f32>, uv: vec2<f32>, vdir: vec3<f32>) -> f32 {
    let base = 100u + eidx * 6u;
    return eval_comp_mask(base, world_pos, norm, depth, sid, curv, uv, vdir);
}

// ============================================================
// Compositor Effects — Sprints 4-7
// ============================================================

// D1: neighborhood taps for compositor effects. While progressive refine is
// averaging (bit 256), read the running mean so DoF/MB/sharpen don't re-inject
// per-frame noise into an otherwise converged image. Center pixel still uses
// the caller's just-updated `hdr`.
fn compositor_neigh(coord: vec2<i32>) -> vec3<f32> {
    if ((config.post_flags & 256u) != 0u && config.pt_accumulated > 1u) {
        return textureLoad(pbr_accum_in, coord, 0).rgb;
    }
    return textureLoad(hdr_input, coord, 0).rgb;
}

// --- Sharpen (Unsharp Mask) ---
fn apply_sharpen(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, mask: f32) -> vec3<f32> {
    let str = comp_f(27u);
    let rad = comp_f(28u);
    let thresh = comp_f(29u);
    let clarity = comp_f(30u);
    let r = i32(max(rad, 1.0));
    // Sample neighbors for blur
    var blur = vec3(0.0); var wt = 0.0;
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            let sc = vec2<i32>(clamp(coord.x + dx, 0, i32(dim.x) - 1), clamp(coord.y + dy, 0, i32(dim.y) - 1));
            let d2 = f32(dx * dx + dy * dy);
            let w = exp(-d2 / (2.0 * rad * rad));
            blur += compositor_neigh(sc) * w;
            wt += w;
        }
    }
    blur /= max(wt, 0.001);
    var detail = hdr - blur;
    // Threshold: ignore small differences (anti-noise)
    let detail_len = length(detail);
    if (detail_len < thresh) { return hdr; }
    // Clarity mode: only apply to midtones
    if (clarity > 0.5) {
        let lum = dot(hdr, vec3(0.2126, 0.7152, 0.0722));
        let mid_weight = 1.0 - abs(lum - 0.5) * 2.0;
        detail *= max(mid_weight, 0.0);
    }
    return hdr + detail * str * mask;
}

// --- Outline / Edge Detection ---
fn apply_outline(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, mask: f32) -> vec3<f32> {
    let source = u32(comp_f(46u));
    let line_w = comp_f(47u);
    let line_col = vec4(comp_f(48u), comp_f(49u), comp_f(50u), comp_f(51u));
    let n_thresh = comp_f(52u);
    let d_thresh = comp_f(53u);
    let sid_on = comp_f(54u);
    let fill_mode = u32(comp_f(55u));
    let cel_steps = max(comp_f(56u), 2.0);

    let s = i32(max(line_w, 1.0));
    var edge = 0.0;

    // Normal-based edge detection (Sobel on normal)
    if (source == 0u || source == 2u) { // Normal+Depth or Normal
        let nc = textureLoad(gbuf_normal_in, coord, 0).xyz;
        let nl = textureLoad(gbuf_normal_in, vec2(max(coord.x - s, 0), coord.y), 0).xyz;
        let nr = textureLoad(gbuf_normal_in, vec2(min(coord.x + s, i32(dim.x) - 1), coord.y), 0).xyz;
        let nt = textureLoad(gbuf_normal_in, coord + vec2(0, -s), 0).xyz;
        let nb = textureLoad(gbuf_normal_in, coord + vec2(0, s), 0).xyz;
        let ndx = length(nr - nl);
        let ndy = length(nb - nt);
        let ne = max(ndx, ndy);
        if (ne > n_thresh) { edge = max(edge, ne); }
    }

    // Depth-based edge detection
    if (source == 0u || source == 1u) { // Normal+Depth or Depth
        let dc = textureLoad(gbuf_normal_in, coord, 0).w; // depth in normal.w
        let dl = textureLoad(gbuf_normal_in, vec2(max(coord.x - s, 0), coord.y), 0).w;
        let dr = textureLoad(gbuf_normal_in, vec2(min(coord.x + s, i32(dim.x) - 1), coord.y), 0).w;
        let dt = textureLoad(gbuf_normal_in, coord + vec2(0, -s), 0).w;
        let db = textureLoad(gbuf_normal_in, coord + vec2(0, s), 0).w;
        let ddx = abs(dr - dl);
        let ddy = abs(db - dt);
        let de = max(ddx, ddy);
        if (de > d_thresh) { edge = max(edge, 1.0); }
    }

    // Surface ID boundary detection
    if (sid_on > 0.5) {
        let sc = textureLoad(gbuf_surface_id_in, coord, 0).r;
        let sl = textureLoad(gbuf_surface_id_in, vec2(max(coord.x - s, 0), coord.y), 0).r;
        let sr = textureLoad(gbuf_surface_id_in, vec2(min(coord.x + s, i32(dim.x) - 1), coord.y), 0).r;
        let st2 = textureLoad(gbuf_surface_id_in, coord + vec2(0, -s), 0).r;
        let sb = textureLoad(gbuf_surface_id_in, coord + vec2(0, s), 0).r;
        if (sc != sl || sc != sr || sc != st2 || sc != sb) { edge = 1.0; }
    }

    edge = saturate(edge) * mask;

    // Fill mode
    if (fill_mode == 1u && edge < 0.01) {
        // Flat fill: use albedo
        return textureLoad(gbuf_albedo_in, coord, 0).rgb * 0.5;
    }
    if (fill_mode == 2u) {
        // Cel shade: quantize shading
        let lum = dot(hdr, vec3(0.2126, 0.7152, 0.0722));
        let q = floor(lum * cel_steps) / cel_steps;
        let cel = hdr * (q / max(lum, 0.001));
        if (edge > 0.01) {
            return mix(cel, line_col.rgb, line_col.a * edge);
        }
        return cel;
    }

    // Default: outline only
    if (edge > 0.01) {
        return mix(hdr, line_col.rgb, line_col.a * edge);
    }
    return hdr;
}

// --- Chromatic Aberration ---
fn apply_ca(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, mask: f32) -> vec3<f32> {
    let intensity = comp_f(24u) * mask;
    let mode = u32(comp_f(25u));
    let uv = vec2(f32(coord.x) / f32(dim.x), f32(coord.y) / f32(dim.y));
    let center = uv - vec2(0.5);

    var offset_r: vec2<f32>;
    var offset_b: vec2<f32>;

    if (mode == 0u) { // Radial
        offset_r = center * intensity * 0.02;
        offset_b = -center * intensity * 0.02;
    } else { // Uniform
        offset_r = vec2(intensity * 0.01, 0.0);
        offset_b = vec2(-intensity * 0.01, 0.0);
    }

    let cr = vec2<i32>(clamp(vec2<f32>(vec2(coord)) + offset_r * vec2<f32>(vec2(dim)), vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));
    let cb = vec2<i32>(clamp(vec2<f32>(vec2(coord)) + offset_b * vec2<f32>(vec2(dim)), vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));

    let r = textureLoad(hdr_input, cr, 0).r;
    let g = hdr.g;
    let b = textureLoad(hdr_input, cb, 0).b;
    return vec3(r, g, b);
}

// --- Lens Distortion ---
fn apply_lens_distortion(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, mask: f32) -> vec3<f32> {
    let barrel = comp_f(32u) * mask;
    let do_chrom = comp_f(33u) > 0.5;
    let scale = comp_f(34u);

    let uv = vec2(f32(coord.x) / f32(dim.x), f32(coord.y) / f32(dim.y));
    let center = (uv - vec2(0.5)) * 2.0; // [-1, 1]
    let r2 = dot(center, center);
    let distort = 1.0 + barrel * r2;

    let duv = center * distort * 0.5 / scale + vec2(0.5);
    let sc = vec2<i32>(clamp(duv * vec2<f32>(vec2(dim)), vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));

    if (do_chrom) {
        let distort_r = 1.0 + barrel * 1.02 * r2;
        let distort_b = 1.0 + barrel * 0.98 * r2;
        let uv_r = center * distort_r * 0.5 / scale + vec2(0.5);
        let uv_b = center * distort_b * 0.5 / scale + vec2(0.5);
        let cr = vec2<i32>(clamp(uv_r * vec2<f32>(vec2(dim)), vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));
        let cb = vec2<i32>(clamp(uv_b * vec2<f32>(vec2(dim)), vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));
        return vec3(textureLoad(hdr_input, cr, 0).r, textureLoad(hdr_input, sc, 0).g, textureLoad(hdr_input, cb, 0).b);
    }
    return textureLoad(hdr_input, sc, 0).rgb;
}

// --- Vignette ---
fn apply_vignette(hdr: vec3<f32>, uv: vec2<f32>, mask: f32) -> vec3<f32> {
    let intensity = comp_f(58u) * mask;
    let vig_color = vec3(comp_f(59u), comp_f(60u), comp_f(61u));
    let shape = u32(comp_f(62u));
    let radius = comp_f(63u);
    let softness = max(comp_f(64u), 0.01);
    let blend = u32(comp_f(65u));

    let center = uv - vec2(0.5);
    var d: f32;
    if (shape == 1u) { // Elliptical
        d = length(center * vec2(1.0, 0.6)) * 2.0;
    } else if (shape == 2u) { // Rectangular
        let ac = abs(center) * 2.0;
        d = max(ac.x, ac.y);
    } else { // Circular
        d = length(center) * 2.0;
    }
    let v = smoothstep(radius, radius + softness, d) * intensity;

    if (blend == 1u) { // Screen
        return mix(hdr, vec3(1.0) - (vec3(1.0) - hdr) * (vec3(1.0) - vig_color), v);
    } else if (blend == 2u) { // Overlay
        let mid = vec3(0.5);
        let overlay = select(2.0 * hdr * vig_color, vec3(1.0) - 2.0 * (vec3(1.0) - hdr) * (vec3(1.0) - vig_color), hdr > mid);
        return mix(hdr, overlay, v);
    }
    // Default: Multiply (darken)
    return mix(hdr, hdr * vig_color, v);
}

// --- Film Grain ---
fn apply_film_grain(hdr: vec3<f32>, uv: vec2<f32>, coord: vec2<i32>, mask: f32) -> vec3<f32> {
    let intensity = comp_f(68u) * mask;
    let grain_size = max(comp_f(69u), 0.5);
    let mono = comp_f(70u) > 0.5;
    let animate = comp_f(71u) > 0.5;
    let blend = u32(comp_f(72u));
    let lum_respond = comp_f(73u) > 0.5;

    let time_seed = select(0.0, comp_f(1u), animate);
    let p = vec3(uv * vec2(comp_f(2u), comp_f(3u)) / grain_size, time_seed * 60.0);

    var grain: vec3<f32>;
    if (mono) {
        let n = hash3(p + vec3(0.0, 0.0, 42.17)) * 2.0 - 1.0;
        grain = vec3(n);
    } else {
        grain = vec3(
            hash3(p + vec3(0.0, 0.0, 42.17)) * 2.0 - 1.0,
            hash3(p + vec3(0.0, 0.0, 73.29)) * 2.0 - 1.0,
            hash3(p + vec3(0.0, 0.0, 91.43)) * 2.0 - 1.0,
        );
    }

    // Luminance-responsive: more grain in shadows
    var str = intensity;
    if (lum_respond) {
        let lum = dot(hdr, vec3(0.2126, 0.7152, 0.0722));
        str *= (1.0 - saturate(lum));
    }

    if (blend == 0u) { // Add
        return hdr + grain * str;
    } else if (blend == 1u) { // Multiply
        return hdr * (1.0 + grain * str);
    } else if (blend == 3u) { // Soft Light
        let g = grain * str * 0.5 + 0.5;
        return select(hdr * (2.0 * g), vec3(1.0) - 2.0 * (vec3(1.0) - hdr) * (vec3(1.0) - g), g > vec3(0.5));
    }
    // Default (2): Overlay
    let g = grain * str * 0.5 + 0.5;
    return select(2.0 * hdr * g, vec3(1.0) - 2.0 * (vec3(1.0) - hdr) * (vec3(1.0) - g), hdr > vec3(0.5));
}

// --- Depth of Field (simplified gather) ---
fn apply_dof(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, depth: f32, mask: f32) -> vec3<f32> {
    let focus_dist = comp_f(13u);
    let aperture = max(comp_f(14u), 1.0);
    let max_blur = comp_f(15u);

    let coc = abs(depth - focus_dist) / aperture;
    let blur_radius = min(coc * max_blur, max_blur) * mask;
    if (blur_radius < 0.5) { return hdr; }

    let r = i32(min(blur_radius, 10.0));
    var acc = vec3(0.0); var wt = 0.0;
    let sigma = max(blur_radius * 0.5, 0.5);
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            let d2 = f32(dx * dx + dy * dy);
            if (d2 > blur_radius * blur_radius) { continue; }
            let sc = vec2<i32>(clamp(coord.x + dx, 0, i32(dim.x) - 1), clamp(coord.y + dy, 0, i32(dim.y) - 1));
            let w = exp(-d2 / (2.0 * sigma * sigma));
            acc += compositor_neigh(sc) * w;
            wt += w;
        }
    }
    return acc / max(wt, 0.001);
}

// --- Motion Blur (velocity gather) ---
fn apply_motion_blur(hdr: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>, mask: f32) -> vec3<f32> {
    let intensity = comp_f(19u) * mask;
    let samples = u32(max(comp_f(20u), 2.0));
    let max_len = comp_f(21u);
    let shutter = comp_f(22u) / 360.0;

    let vel = textureLoad(gbuf_motion_in, coord, 0).rg * intensity * shutter;
    let vel_len = length(vel);
    if (vel_len < 0.5) { return hdr; }
    let clamped_vel = vel * min(max_len / max(vel_len, 0.001), 1.0);

    var acc = hdr; var wt = 1.0;
    for (var i = 1u; i < samples; i++) {
        let t = (f32(i) / f32(samples - 1u)) * 2.0 - 1.0;
        let sc = vec2<i32>(clamp(vec2<f32>(vec2(coord)) + clamped_vel * t, vec2(0.0), vec2<f32>(vec2(dim)) - 1.0));
        acc += compositor_neigh(sc);
        wt += 1.0;
    }
    return acc / wt;
}

// --- Compositor Fog (replaces/extends existing fog) ---
fn apply_comp_fog(hdr: vec3<f32>, depth: f32, world_pos: vec3<f32>, mask: f32) -> vec3<f32> {
    let fog_col = vec3(comp_f(36u), comp_f(37u), comp_f(38u));
    let density = comp_f(39u) * mask;
    let start = comp_f(40u);
    let end_d = comp_f(41u);
    let mode = u32(comp_f(42u));
    let h_density = comp_f(43u);
    let h_base = comp_f(44u);

    if (density < 0.001) { return hdr; }

    var fog_factor: f32;
    let range = max(end_d - start, 0.001);

    if (mode == 1u) { // Exponential
        fog_factor = 1.0 - exp(-density * max(depth - start, 0.0));
    } else if (mode == 2u) { // Height fog
        let linear = saturate((depth - start) / range) * density;
        let height_factor = exp(-max(world_pos.y - h_base, 0.0) * max(h_density, 0.01));
        fog_factor = linear * height_factor;
    } else { // Linear
        fog_factor = saturate((depth - start) / range) * density;
    }

    return mix(hdr, fog_col, saturate(fog_factor));
}

// --- Color Grading ---
fn apply_color_grading(col: vec3<f32>, mask: f32) -> vec3<f32> {
    var c = col;
    let hue_shift = comp_f(75u) * mask;
    let sat = mix(1.0, comp_f(76u), mask);
    let vibrance = mix(1.0, comp_f(77u), mask);
    let brightness = comp_f(78u) * mask;
    let contrast = mix(1.0, comp_f(79u), mask);
    let gamma = mix(1.0, comp_f(80u), mask);
    let highlights = comp_f(81u) * mask;
    let shadows = comp_f(82u) * mask;

    // Brightness
    c += vec3(brightness);

    // Contrast (around 0.5 midpoint)
    c = (c - 0.5) * contrast + 0.5;

    // Gamma
    c = pow(max(c, vec3(0.0)), vec3(1.0 / max(gamma, 0.1)));

    // Saturation
    let lum = dot(c, vec3(0.2126, 0.7152, 0.0722));
    c = mix(vec3(lum), c, sat);

    // Vibrance (boost undersaturated more)
    let max_ch = max(c.r, max(c.g, c.b));
    let min_ch = min(c.r, min(c.g, c.b));
    let current_sat = select(0.0, (max_ch - min_ch) / max(max_ch, 0.001), max_ch > 0.001);
    let vib_factor = mix(vibrance, 1.0, current_sat);
    c = mix(vec3(lum), c, vib_factor);

    // Highlights / Shadows
    let lum2 = dot(c, vec3(0.2126, 0.7152, 0.0722));
    let hi_mask = smoothstep(0.5, 1.0, lum2);
    let sh_mask = 1.0 - smoothstep(0.0, 0.5, lum2);
    c += vec3(highlights * hi_mask + shadows * sh_mask);

    // Color balance (shadow / midtone / highlight tints)
    let bal_s = vec3(comp_f(83u), comp_f(84u), comp_f(85u));
    let bal_m = vec3(comp_f(86u), comp_f(87u), comp_f(88u));
    let bal_h = vec3(comp_f(89u), comp_f(90u), comp_f(91u));
    let mid_mask = 1.0 - hi_mask - sh_mask;
    c *= bal_s * sh_mask + bal_m * max(mid_mask, 0.0) + bal_h * hi_mask;

    // Hue shift
    if (abs(hue_shift) > 0.1) {
        let angle = hue_shift * 3.14159 / 180.0;
        let cs = cos(angle); let sn = sin(angle);
        let k = vec3(0.57735);
        c = c * cs + cross(k, c) * sn + k * dot(k, c) * (1.0 - cs);
    }

    return max(c, vec3(0.0));
}

// --- White Balance ---
fn apply_white_balance(col: vec3<f32>, temp: f32, tint: f32) -> vec3<f32> {
    // Simplified: temp shifts blue↔orange, tint shifts green↔magenta
    let temp_shift = (temp - 6500.0) / 6500.0; // normalized
    var c = col;
    c.r *= 1.0 + temp_shift * 0.2;
    c.b *= 1.0 - temp_shift * 0.2;
    c.g *= 1.0 - tint * 0.15;
    c.r *= 1.0 + tint * 0.05;
    return max(c, vec3(0.0));
}

// ============================================================
// Viewport Depth of Field (deferred post-process, HDR space)
// Two-ring hexagonal gather blur scaled by circle of confusion.
// Gated on post_flags bit 5. Runs in PBR/Lit, suppressed during interaction.
// ============================================================
fn apply_viewport_dof(coord: vec2<i32>, dim: vec2<u32>) -> vec3<f32> {
    // D1: use compositor_neigh so refine frames don't sparkle the bokeh.
    let center = compositor_neigh(coord);
    let center_depth = textureLoad(gbuf_normal_in, coord, 0).w;
    if (center_depth <= 0.0) { return center; } // background — no DOF

    let focus = config.sel_rim_base;      // TODO: wire dof_focus_dist into GpuUniforms (currently 0.0 — viewport DOF disabled)
    let aperture = config.pt_dof_aperture;
    if (aperture <= 0.0 || focus <= 0.0) { return center; }

    // Circle of confusion (pixels) — thin lens approximation
    let coc = clamp(abs(center_depth - focus) / max(focus, 0.01) * aperture * f32(dim.y) * 0.008, 0.0, 12.0);
    if (coc < 0.5) { return center; } // in focus — skip gather

    // Two-ring hexagonal gather (12 taps + center)
    var sum = center;
    var wt = 1.0;
    let r_inner = coc * 0.4;
    let r_outer = coc * 0.85;
    var hex = array<vec2<f32>, 6>(
        vec2(1.0, 0.0), vec2(0.5, 0.866), vec2(-0.5, 0.866),
        vec2(-1.0, 0.0), vec2(-0.5, -0.866), vec2(0.5, -0.866)
    );

    for (var ring = 0u; ring < 2u; ring++) {
        let r = select(r_inner, r_outer, ring == 1u);
        for (var i = 0u; i < 6u; i++) {
            let off = hex[i] * r;
            let sc = coord + vec2<i32>(i32(round(off.x)), i32(round(off.y)));
            if (sc.x < 0 || sc.y < 0 || sc.x >= i32(dim.x) || sc.y >= i32(dim.y)) { continue; }

            let s_depth = textureLoad(gbuf_normal_in, sc, 0).w;
            let s_color = compositor_neigh(sc);

            // Foreground leaking guard: sharp foreground shouldn't bleed into focused bg
            var w = 1.0;
            if (s_depth > 0.0 && s_depth < center_depth * 0.95) {
                let s_coc = abs(s_depth - focus) / max(focus, 0.01) * aperture * f32(dim.y) * 0.008;
                w = smoothstep(0.0, 1.5, s_coc);
            }

            sum += s_color * w;
            wt += w;
        }
    }

    return sum / wt;
}

// ============================================================
// Body Silhouette Outline (screen-space edge detection on body_index)
// ============================================================
// Reads body_index from gbuf_motion.r (written by both raymarch and raster paths).
// Samples a cross neighborhood and detects edges where selected coverage changes.
//
// CRITICAL: gbuf_motion is cleared to 0.0 and body_index 0 is a VALID body id.
// Background pixels must NOT be treated as body 0 — that made silhouette fail and
// left only screen-edge hits → a green RECTANGLE around the viewport
// (owner 2026-08-06: "need GPU outline around the object, not a square around viewport").
// Gate "is selected body" with albedo flags (geometry > ~0.6; bg/floor stay out).
fn body_outline_is_sel(coord: vec2<i32>, sel_bi: u32) -> bool {
    let flags = textureLoad(gbuf_albedo_in, coord, 0).a;
    // Geometry / selected / ghost / vfx write flags ≥ ~0.61; clear bg is 0; floor ~0.5.
    if (flags < 0.58) {
        return false;
    }
    let bi = u32(round(textureLoad(gbuf_motion_in, coord, 0).r));
    return bi == sel_bi;
}

fn apply_body_outline(color: vec3<f32>, coord: vec2<i32>, dim: vec2<u32>) -> vec3<f32> {
    let sel_bi = config.selected_body_index;
    let center_is_sel = body_outline_is_sel(coord, sel_bi);

    // Cross kernel (cheaper than full 3x3)
    var offsets = array<vec2<i32>, 4>(
        vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1),
    );
    var is_outline = false;
    for (var i = 0u; i < 4u; i++) {
        let nc = coord + offsets[i];
        if (nc.x < 0 || nc.y < 0 || nc.x >= i32(dim.x) || nc.y >= i32(dim.y)) {
            // Clip edge: only outline if THIS pixel is selected geometry (object silhouette
            // at the frame border). Never treat empty border as a selection edge alone.
            if (center_is_sel) { is_outline = true; break; }
            continue;
        }
        let neigh_is_sel = body_outline_is_sel(nc, sel_bi);
        // Outline when selected coverage changes (object edge against bg or other bodies).
        if (center_is_sel != neigh_is_sel) {
            is_outline = true; break;
        }
    }
    // Second ring for 2px+ outline width
    if (!is_outline) {
        var offsets2 = array<vec2<i32>, 4>(
            vec2(-2, 0), vec2(2, 0), vec2(0, -2), vec2(0, 2),
        );
        for (var i = 0u; i < 4u; i++) {
            let nc = coord + offsets2[i];
            if (nc.x < 0 || nc.y < 0 || nc.x >= i32(dim.x) || nc.y >= i32(dim.y)) {
                if (center_is_sel) { is_outline = true; break; }
                continue;
            }
            let neigh_is_sel = body_outline_is_sel(nc, sel_bi);
            if (center_is_sel != neigh_is_sel) {
                is_outline = true; break;
            }
        }
    }

    if (is_outline) {
        return config.sel_rim_color;
    }
    return color;
}

// ============================================================
// Tonemap + Compositor composite
// ============================================================
@compute @workgroup_size(8, 8, 1)
fn pass_tonemap(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(hdr_input);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    // Debug render modes: pass through albedo directly, no post-processing
    if (config.debug_mode != 0u) {
        let dbg = textureLoad(gbuf_albedo_in, coord, 0);
        textureStore(ldr_output, coord, dbg);
        return;
    }

    let hdr_sample = textureLoad(hdr_input, coord, 0);
    var hdr = hdr_sample.rgb;
    let is_scene = hdr_sample.a > 0.5;
    // HIDDEN HDRI PLATE (post_flags 4096) — the swap happens HERE, at display time, and
    // deliberately NOT in deferred. Deferred keeps writing the physical sky so SSR, the
    // reflection probe and bloom all still sample a real environment (flattening it there
    // killed every screen-space reflection of the sky); it only marks the pixel alpha 0, which
    // this pass reads back as `!is_scene` — so the plate also skips exposure/ACES, DOF, SSR and
    // SSGI exactly like the no-HDRI user background does. Gated on a loaded env so a hidden
    // toggle with no HDRI cannot flatten the user's own bg gradient.
    let bg_hidden = (config.post_flags & 4096u) != 0u
        && config.env_width > 0u
        && !is_scene
        && config.view_mode != 1u; // MatCap owns its own background
    if (bg_hidden) {
        hdr = vec3<f32>(
            f32((config.bg_top_packed >> 16u) & 0xFFu) / 255.0,
            f32((config.bg_top_packed >> 8u) & 0xFFu) / 255.0,
            f32(config.bg_top_packed & 0xFFu) / 255.0,
        );
    }
    let uv = vec2(f32(gid.x) / f32(dim.x), f32(gid.y) / f32(dim.y));

    // Viewport DOF (post_flags bit 5) — gather blur in HDR, before bloom/fog/SSR
    if ((config.post_flags & 32u) != 0u && is_scene) {
        hdr = apply_viewport_dof(coord, dim);
    }

    // MatCap/Wireframe: skip bloom/fog/SSR — these are PBR post-effects that contaminate
    // the clean passthrough output (fog tints bg, bloom bleeds, SSR adds reflections)
    let skip_post = config.view_mode == 1u || config.view_mode == 2u;

    // SSR composite (post_flags bit 2) — energy-conserving DELTA:
    // ssr.rgb = envelope × confidence × (traced − IBL-specular), so this add
    // swaps the deferred IBL specular for the traced reflection by hit
    // confidence (never double-adds energy; SSAO untouched — 17e contract).
    // Runs BEFORE bloom and fog so reflections bloom and get fogged like the
    // rest of the scene (they used to be pasted over the fog).
    if (!skip_post && (config.post_flags & 4u) != 0u && is_scene) {
        hdr += textureLoad(ssr_input, coord, 0).rgb;
        // fp guard only: the subtraction is exact by construction (same env
        // chain + same BRDF envelope), rounding can leave ~-1e-6 components.
        hdr = max(hdr, vec3(0.0));
    }

    // SSGI composite (post_flags bit 128) — the diffuse twin of the SSR add, and
    // placed beside it for the same reason: before bloom and fog, so indirect
    // light blooms and fogs like every other kind.
    //
    // Unlike SSR this delta is LEGITIMATELY negative wherever geometry occludes
    // the environment (the "HDRI Occlusion" term removes sky deferred applied),
    // so the clamp below is not an fp guard — it is load-bearing, and without it
    // negative radiance would flow into the fog composite two blocks down.
    if (!skip_post && (config.post_flags & 128u) != 0u && is_scene) {
        let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
        let d = ssgi_delta(coord, gbuf_n.w, gbuf_n.xyz);
        // Clamp to whichever is lower-bounded by the PRE-EXISTING value, not to 0.
        // A plain max(hdr, 0) is NOT inert at d == 0: hdr can already be slightly
        // negative here from the dithered background (bg_gradient_dithered adds a
        // signed +/-0.00275 with no clamp) reaching alpha-1 pixels through the
        // transparent-PBR and VFX blends. A hard clamp would raise those to exactly
        // 0 the moment SSGI is switched on, breaking the bit-identical promise for
        // a reason unrelated to SSGI — precisely what VG-A asks the owner to look
        // for. This form leaves hdr untouched when d == 0, and still stops SSGI's
        // own (legitimately negative) delta from driving a channel below where it
        // started. SSR's clamp is a plain max because its delta is non-negative.
        hdr = max(hdr + d, min(hdr, vec3(0.0)));
    }

    // Composite bloom (quarter-res → manual bilinear upsample)
    if (!skip_post && (config.post_flags & 2u) != 0u) {
        let bloom_dim = textureDimensions(bloom_read);
        let bx = f32(gid.x) * f32(bloom_dim.x) / f32(dim.x);
        let by = f32(gid.y) * f32(bloom_dim.y) / f32(dim.y);
        let ix = i32(floor(bx)); let iy = i32(floor(by));
        let fx = fract(bx); let fy = fract(by);
        let b00 = textureLoad(bloom_read, vec2(ix, iy), 0).rgb;
        let b10 = textureLoad(bloom_read, vec2(ix+1, iy), 0).rgb;
        let b01 = textureLoad(bloom_read, vec2(ix, iy+1), 0).rgb;
        let b11 = textureLoad(bloom_read, vec2(ix+1, iy+1), 0).rgb;
        let bloom = mix(mix(b00, b10, fx), mix(b01, b11, fx), fy);
        hdr += bloom * config.bloom_intensity;
    }
    // Fog composite (post_flags bit 3) — legacy pipeline fog
    if (!skip_post && (config.post_flags & 8u) != 0u) {
        let fog = textureLoad(fog_input, coord, 0);
        hdr = hdr * fog.a + fog.rgb;
    }
    // (SSR composite moved ABOVE bloom — 2026-07-18 quality pass.)

    // Progressive PBR refine (post_flags bit 256) — 1/N running mean of the fully
    // composited HDR (deferred + SSR + SSGI + bloom + fog), the raster twin of
    // pathtrace_entry.wgsl's accumulation. `pt_accumulated` carries the PBR refine
    // frame count in non-PathTrace modes. Everything below (compositor effects,
    // tonemap curve, dither, outline) runs on the AVERAGE — deterministic, so the
    // displayed image is stable while quality converges. Accumulating pre-tonemap
    // is deliberate: tonemap is nonlinear, so averaging LDR would converge to the
    // wrong value. First frame (N == 1) skips the read — pbr_accum_in is garbage
    // after a reset, and mix weight 1/1 ignores it anyway.
    if ((config.post_flags & 256u) != 0u) {
        if (config.pt_accumulated > 1u) {
            let prev = textureLoad(pbr_accum_in, coord, 0).rgb;
            hdr = mix(prev, hdr, 1.0 / f32(config.pt_accumulated));
        }
        textureStore(pbr_accum_out, coord, vec4<f32>(hdr, hdr_sample.a));
    }

    // ===== Compositor Effects =====
    let comp_on = comp_f(0u) > 0.5 && is_scene;
    if (comp_on) {
        // Read G-buffer data for masks
        let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
        let norm = gbuf_n.xyz;
        let depth_raw = gbuf_n.w;
        // Already world-space distance from compute.wgsl
        let linear_depth = depth_raw;
        let curv_sample = textureLoad(gbuf_curvature_in, coord, 0);
        let curvature = curv_sample.rg;
        let sid = textureLoad(gbuf_surface_id_in, coord, 0).r;
        // Reconstruct world pos from depth via camera_ray (FOV-correct)
        let ndc = vec2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let recon_aspect = f32(dim.x) / f32(dim.y);
        let recon_ray = camera_ray(ndc.x, ndc.y, recon_aspect);
        let world_pos = recon_ray[0] + recon_ray[1] * linear_depth;
        let view_dir = normalize(config.cam_pos.xyz - world_pos);

        // --- DoF (must read neighbors, apply early) ---
        if (comp_f(12u) > 0.5) {
            let m = get_effect_mask(1u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_dof(hdr, coord, dim, linear_depth, m);
        }

        // --- Motion Blur ---
        if (comp_f(18u) > 0.5) {
            let m = get_effect_mask(2u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_motion_blur(hdr, coord, dim, m);
        }

        // --- Compositor Fog ---
        if (comp_f(35u) > 0.5) {
            let m = get_effect_mask(3u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_comp_fog(hdr, linear_depth, world_pos, m);
        }

        // --- Sharpen ---
        if (comp_f(26u) > 0.5) {
            let m = get_effect_mask(4u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_sharpen(hdr, coord, dim, m);
        }

        // --- Outline ---
        if (comp_f(45u) > 0.5) {
            let m = get_effect_mask(5u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_outline(hdr, coord, dim, m);
        }

        // --- Chromatic Aberration ---
        if (comp_f(23u) > 0.5) {
            let m = get_effect_mask(6u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_ca(hdr, coord, dim, m);
        }

        // --- Lens Distortion ---
        if (comp_f(31u) > 0.5) {
            let m = get_effect_mask(7u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_lens_distortion(hdr, coord, dim, m);
        }

        // --- Vignette ---
        if (comp_f(57u) > 0.5) {
            let m = get_effect_mask(8u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_vignette(hdr, uv, m);
        }

        // --- Film Grain ---
        if (comp_f(66u) > 0.5) {
            let m = get_effect_mask(9u, world_pos, norm, linear_depth, sid, curvature, uv, view_dir);
            hdr = apply_film_grain(hdr, uv, coord, m);
        }
    }

    if (is_scene) {
        // MatCap & Wireframe: bypass exposure/tonemap/grading (gamma still applies)
        let is_passthrough = (config.view_mode == 1u || config.view_mode == 2u);

        // --- Color Management (always last) ---
        var exposed = hdr;
        if (!is_passthrough) {
            if (comp_on) {
                // White balance
                let wb_temp = comp_f(95u);
                let wb_tint = comp_f(96u);
                if (abs(wb_temp - 6500.0) > 1.0 || abs(wb_tint) > 0.001) {
                    exposed = apply_white_balance(exposed, wb_temp, wb_tint);
                }
                // Compositor exposure
                let comp_exposure = comp_f(93u);
                exposed *= pow(2.0, comp_exposure) * max(config.exposure, 0.001);
            } else {
                exposed *= max(config.exposure, 0.001);
            }
        }

        // Tonemapping
        var ldr: vec3<f32>;
        if (is_passthrough) {
            ldr = saturate(exposed);
        } else if (comp_on) {
            let tm = u32(comp_f(92u));
            switch (tm) {
                case 0u: { ldr = aces_narkowicz(exposed); }
                case 1u: { ldr = filmic_tonemap(exposed); }
                case 2u: { ldr = agx_tonemap(exposed); }
                case 3u: { ldr = reinhard_tonemap(exposed); }
                case 4u: { ldr = pbr_neutral_tonemap(exposed); }
                default: { ldr = saturate(exposed); } // None/Linear
            }
        } else {
            ldr = viewport_tonemap(exposed, config.tonemapper);
        }

        // Color grading (post-tonemap)
        if (!is_passthrough && comp_on && comp_f(74u) > 0.5) {
            let gm = get_effect_mask(10u, vec3(0.0), vec3(0.0), 0.0, 0u, vec2(0.0), uv, vec3(0.0));
            ldr = apply_color_grading(ldr, gm);
        }

        // MatCap/Wireframe: sRGB passthrough — skip gamma (would double-correct)
        var gamma = select(linear_to_srgb(max(ldr, vec3(0.0))), saturate(ldr), is_passthrough);

        // Mask preview overlay (red=masked-out, green=affected)
        if (comp_on && comp_f(98u) > 0.5) {
            let mp_idx = u32(comp_f(98u)) - 1u;
            // Re-read G-buffer for mask eval (original vars out of scope)
            let mp_n = textureLoad(gbuf_normal_in, coord, 0);
            let mp_norm = mp_n.xyz;
            let mp_draw = mp_n.w;
            let mp_depth = mp_draw; // Already world-space distance from compute.wgsl
            let mp_curv = textureLoad(gbuf_curvature_in, coord, 0).rg;
            let mp_sid = textureLoad(gbuf_surface_id_in, coord, 0).r;
            let mp_ndc = vec2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let mp_wpos = config.cam_pos.xyz + config.cam_forward.xyz * mp_depth
                + config.cam_right.xyz * mp_ndc.x * mp_depth * 0.5
                + config.cam_up.xyz * mp_ndc.y * mp_depth * 0.5;
            let mp_vdir = normalize(config.cam_pos.xyz - mp_wpos);
            let mv = get_effect_mask(mp_idx, mp_wpos, mp_norm, mp_depth, mp_sid, mp_curv, uv, mp_vdir);
            let overlay = mix(vec3(0.8, 0.1, 0.1), vec3(0.1, 0.8, 0.1), mv);
            gamma = mix(gamma, overlay, 0.5);
        }

        // Body silhouette outline (screen-space edge detection on body_index).
        // 0xFFFFFFFF = no selection (0 is a valid dense body_index — C.5).
        if (config.selected_body_index != 0xFFFFFFFFu) {
            gamma = apply_body_outline(gamma, coord, dim);
        }
        // Universal dither — applied to all pixels (scene + background)
        if ((config.post_flags & 16u) != 0u) {
            let d = dither_tri(vec2(f32(coord.x), f32(coord.y))) * config.dither_strength;
            gamma += vec3(d, d, d);
        }
        textureStore(ldr_output, coord, vec4(gamma, 1.0));
    } else {
        var bg_out = hdr;
        // Body silhouette outline on background pixels (body edge meets background)
        if (config.selected_body_index != 0xFFFFFFFFu) {
            bg_out = apply_body_outline(bg_out, coord, dim);
        }
        // Universal dither — applied to all pixels (scene + background)
        if ((config.post_flags & 16u) != 0u) {
            let d = dither_tri(vec2(f32(coord.x), f32(coord.y))) * config.dither_strength;
            bg_out += vec3(d, d, d);
        }
        textureStore(ldr_output, coord, vec4(bg_out, 1.0));
    }

}

// ============================================================
// Bloom passes
// ============================================================

// Extract bright pixels from HDR → quarter-res bloom texture
@compute @workgroup_size(8, 8, 1)
fn bloom_extract(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(bloom_write);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    // Sample 2x2 block from HDR (bloom is half-res of HDR)
    let hdr_dim = textureDimensions(hdr_input);
    let sx = f32(gid.x) * f32(hdr_dim.x) / f32(dim.x);
    let sy = f32(gid.y) * f32(hdr_dim.y) / f32(dim.y);
    let base = vec2<i32>(i32(sx), i32(sy));
    let s0 = textureLoad(hdr_input, base, 0);
    let s1 = textureLoad(hdr_input, base + vec2(1, 0), 0);
    let s2 = textureLoad(hdr_input, base + vec2(0, 1), 0);
    let s3 = textureLoad(hdr_input, base + vec2(1, 1), 0);
    var avg = (s0.rgb + s1.rgb + s2.rgb + s3.rgb) * 0.25;
    // A HIDDEN HDRI plate (post_flags 4096) must not bloom. The sky deliberately stays in the
    // HDR buffer for SSR / probe / IBL, so without this a bright sun would glow straight through
    // the flat colour the user asked for and betray the hidden environment. Alpha is deferred's
    // "this pixel is background" mark, so object pixels (alpha 1) keep contributing and their
    // halos still spill across the plate in the composite — only the sky's own energy is cut.
    if ((config.post_flags & 4096u) != 0u) {
        let w0 = step(0.5, s0.a); let w1 = step(0.5, s1.a);
        let w2 = step(0.5, s2.a); let w3 = step(0.5, s3.a);
        let wsum = w0 + w1 + w2 + w3;
        if (wsum <= 0.0) {
            textureStore(bloom_write, vec2<i32>(gid.xy), vec4(0.0, 0.0, 0.0, 1.0));
            return;
        }
        avg = (s0.rgb * w0 + s1.rgb * w1 + s2.rgb * w2 + s3.rgb * w3) / wsum;
    }
    // Soft threshold
    let lum = dot(avg, vec3(0.2126, 0.7152, 0.0722));
    let soft = max(0.0, lum - config.bloom_threshold);
    let contribution = soft / (soft + 1.0);
    textureStore(bloom_write, vec2<i32>(gid.xy), vec4(avg * contribution, 1.0));
}

// Separable Gaussian blur — shared implementation
fn bloom_blur_impl(gid: vec3<u32>, dir: vec2<i32>) {
    let dim = textureDimensions(bloom_read);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    let coord = vec2<i32>(gid.xy);
    // 9-tap Gaussian (sigma ~2.5)
    let w0 = 0.2270; let w1 = 0.1945; let w2 = 0.1216; let w3 = 0.0540; let w4 = 0.0162;
    var color = textureLoad(bloom_read, coord, 0).rgb * w0;
    color += textureLoad(bloom_read, clamp(coord + dir * 1, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w1;
    color += textureLoad(bloom_read, clamp(coord - dir * 1, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w1;
    color += textureLoad(bloom_read, clamp(coord + dir * 2, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w2;
    color += textureLoad(bloom_read, clamp(coord - dir * 2, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w2;
    color += textureLoad(bloom_read, clamp(coord + dir * 3, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w3;
    color += textureLoad(bloom_read, clamp(coord - dir * 3, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w3;
    color += textureLoad(bloom_read, clamp(coord + dir * 4, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w4;
    color += textureLoad(bloom_read, clamp(coord - dir * 4, vec2(0), vec2<i32>(dim) - 1), 0).rgb * w4;
    textureStore(bloom_write, coord, vec4(color, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn bloom_blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    bloom_blur_impl(gid, vec2<i32>(1, 0));
}

@compute @workgroup_size(8, 8, 1)
fn bloom_blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    bloom_blur_impl(gid, vec2<i32>(0, 1));
}

// ============================================================
// FSR-style Upsampling — Catmull-Rom bicubic + CAS
// ============================================================

// Catmull-Rom spline weights for fractional offset t (0-1)
fn cr_weights(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        -0.5*t3 + t2 - 0.5*t,
         1.5*t3 - 2.5*t2 + 1.0,
        -1.5*t3 + 2.0*t2 + 0.5*t,
         0.5*t3 - 0.5*t2
    );
}

@compute @workgroup_size(8, 8, 1)
fn pass_upsample(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dim = textureDimensions(upsample_output);
    if (gid.x >= out_dim.x || gid.y >= out_dim.y) { return; }

    let in_dim = vec2<f32>(textureDimensions(upsample_input));
    let out_dimf = vec2<f32>(out_dim);

    // Map output pixel to input texture space
    let uv = (vec2<f32>(gid.xy) + 0.5) / out_dimf;
    let src = uv * in_dim - 0.5;
    let base = vec2<i32>(floor(src));
    let frac = src - floor(src);
    let max_coord = vec2<i32>(in_dim) - 1;

    // Catmull-Rom weights
    let wx = cr_weights(frac.x);
    let wy = cr_weights(frac.y);

    // 4x4 bicubic sampling
    var color = vec3<f32>(0.0);
    for (var j = 0; j < 4; j = j + 1) {
        let y_coord = clamp(base.y + j - 1, 0, max_coord.y);
        let w_y = select(select(select(wy.w, wy.z, j == 2), wy.y, j == 1), wy.x, j == 0);
        for (var i = 0; i < 4; i = i + 1) {
            let x_coord = clamp(base.x + i - 1, 0, max_coord.x);
            let w_x = select(select(select(wx.w, wx.z, i == 2), wx.y, i == 1), wx.x, i == 0);
            color += textureLoad(upsample_input, vec2<i32>(x_coord, y_coord), 0).rgb * w_x * w_y;
        }
    }

    // Contrast-Adaptive Sharpening (CAS)
    let nearest = clamp(vec2<i32>(round(src)), vec2(0), max_coord);
    let c = textureLoad(upsample_input, nearest, 0).rgb;
    let n = textureLoad(upsample_input, clamp(nearest + vec2(0, -1), vec2(0), max_coord), 0).rgb;
    let s = textureLoad(upsample_input, clamp(nearest + vec2(0, 1), vec2(0), max_coord), 0).rgb;
    let e = textureLoad(upsample_input, clamp(nearest + vec2(1, 0), vec2(0), max_coord), 0).rgb;
    let w = textureLoad(upsample_input, clamp(nearest + vec2(-1, 0), vec2(0), max_coord), 0).rgb;

    // Per-channel local contrast → reduce sharpening in high-contrast areas
    let mn = min(min(n, s), min(e, w));
    let mx = max(max(n, s), max(e, w));
    let peak = vec3(1.0) - saturate((mx - mn) * 4.0);
    let detail = c - 0.25 * (n + s + e + w);
    color = saturate(color + detail * peak * 0.5);

    textureStore(upsample_output, vec2<i32>(gid.xy), vec4(color, 1.0));
}
