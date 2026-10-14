// #region SHADING
// ============================================================
// Shading modes (view_mode: 0=PBR, 1=MatCap, 2=Wireframe, 3=Solid+HDR)
// ============================================================

// MatCap: view-space normal → spherical UV → sample from texture buffer (or fallback gradient)
fn matcap_color(normal: vec3<f32>, cam_right: vec3<f32>, cam_up: vec3<f32>) -> vec3<f32> {
    let vn = vec3(dot(normal, cam_right), dot(normal, cam_up), abs(dot(normal, normalize(cross(cam_right, cam_up)))));
    let uv = vn.xy * 0.5 + 0.5;
    // Sample from matcap texture buffer if loaded
    let mc_w = matcap_data[0];
    let mc_h = matcap_data[1];
    if (mc_w > 0u && mc_h > 0u) {
        let px = clamp(u32(uv.x * f32(mc_w)), 0u, mc_w - 1u);
        let py = clamp(u32((1.0 - uv.y) * f32(mc_h)), 0u, mc_h - 1u);
        let idx = 2u + py * mc_w + px;
        let packed = matcap_data[idx];
        let srgb = vec3(
            f32(packed & 0xFFu) / 255.0,
            f32((packed >> 8u) & 0xFFu) / 255.0,
            f32((packed >> 16u) & 0xFFu) / 255.0
        );
        // Pre-linearize: egui swapchain is sRGB → applies pow(1/2.2) on blit.
        // We counter it here so the final output matches the original PNG sRGB values.
        return pow(max(srgb, vec3(0.0)), vec3(2.2));
    }
    // Fallback: procedural warm/cool gradient (also pre-linearized)
    let warm = pow(vec3(0.95, 0.82, 0.55), vec3(2.2));
    let cool = pow(vec3(0.28, 0.38, 0.72), vec3(2.2));
    let hi   = vec3(1.0);
    let base = mix(cool, warm, uv.y);
    let r    = length(uv - vec2(0.5));
    return mix(base, hi, smoothstep(0.25, 0.0, r) * 0.65);
}

// Wireframe base (view_mode 2): dark flat form under the DC mesh-edge overlay.
// Real DC/poly edges are drawn by the GPU wire pass (or CPU fallback).
fn wireframe_color(_p: vec3<f32>, normal: vec3<f32>, mat_color: vec3<f32>) -> vec3<f32> {
    // Soft n·l so volume still reads; keep very dark so edge overlay dominates.
    let n = normalize(normal);
    let lit = clamp(n.y * 0.35 + 0.45, 0.2, 0.85);
    return mat_color * 0.08 * lit + vec3(0.03);
}

// Solid+HDR: 3-light studio setup using the scene Sun direction.
// Falls back to a default direction when no Sun exists (light_dir = zero).
fn solid_hdr_color(normal: vec3<f32>, view_dir: vec3<f32>, mat_color: vec3<f32>) -> vec3<f32> {
    // Use scene Sun direction (config.light_dir.xyz), fallback if zero.
    var key = config.light_dir.xyz;
    if (dot(key, key) < 0.001) { key = vec3(0.5, 0.8, 0.6); } // default when no Sun
    key = normalize(key);
    let fill = normalize(vec3(-key.x * 0.5, abs(key.y) * 0.6 + 0.3, -key.z * 0.5));
    let ndl      = max(dot(normal, key), 0.0);
    let ndl_fill = max(dot(normal, fill), 0.0);
    let h    = normalize(view_dir + key);
    let spec = pow(max(dot(normal, h), 0.0), 32.0) * 0.35;
    return mat_color * (ndl * 0.55 + ndl_fill * 0.25 + 0.20) + vec3(spec);
}
// #endregion SHADING

// Shared data bindings (used by PT, deferred, raymarch, fog — outside GBUF_BINDINGS
// so pipelines that strip GBUF_BINDINGS still have access)
@group(0) @binding(21) var<storage, read> env_data: array<f32>;
@group(0) @binding(22) var<storage, read> tex_atlas: array<u32>;
@group(0) @binding(23) var<storage, read> tex_atlas_info: array<vec4<u32>>;
@group(0) @binding(26) var<storage, read> light_data: array<f32>;

// #region GBUF_BINDINGS
// ============================================================
// G-Buffer bindings (raymarch/deferred write targets + read-back textures)
// ============================================================
@group(0) @binding(9) var gbuf_normal_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var gbuf_material_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(11) var gbuf_albedo_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(12) var gbuf_normal_in: texture_2d<f32>;
@group(0) @binding(13) var gbuf_material_in: texture_2d<f32>;
@group(0) @binding(14) var gbuf_albedo_in: texture_2d<f32>;
// Raster depth snapshot: copy of G-buffer normal after raster pass (.w = world distance, 0 = no raster)
@group(0) @binding(29) var raster_depth_in: texture_2d<f32>;

// Viewport Compositor G-Buffer extension
@group(0) @binding(30) var gbuf_surface_id_out: texture_storage_2d<r32uint, write>;
@group(0) @binding(31) var gbuf_surface_id_in: texture_2d<u32>;
// C.5: r32float — body_index exact past 2048 (MRT ≤32 B/sample)
@group(0) @binding(32) var gbuf_motion_out: texture_storage_2d<r32float, write>;
@group(0) @binding(33) var gbuf_motion_in: texture_2d<f32>;
@group(0) @binding(34) var gbuf_curvature_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(35) var gbuf_curvature_in: texture_2d<f32>;
@group(0) @binding(36) var gbuf_emission_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(37) var gbuf_emission_in: texture_2d<f32>;

// Light linking: per-prim bitmask (2 u32 per prim = 64 lights max)
// light_link_masks[prim*2] = lo 32 bits, light_link_masks[prim*2+1] = hi 32 bits
// bit N = 1 means light N affects this prim. All 1s = default (all lights).
@group(0) @binding(41) var<storage, read> light_link_masks: array<u32>;

// MatCap texture: [0]=width, [1]=height, [2..]=packed RGBA8 pixels (R|G<<8|B<<16|A<<24)
@group(0) @binding(42) var<storage, read> matcap_data: array<u32>;

fn light_linked(prim_id: u32, light_idx: u32) -> bool {
    if (prim_id == 0xFFFFFFFFu) { return true; }
    let word = select(prim_id * 2u, prim_id * 2u + 1u, light_idx >= 32u);
    let bit = light_idx & 31u;
    return (light_link_masks[word] & (1u << bit)) != 0u;
}
// #endregion GBUF_BINDINGS

// #region LIGHTING
// ---- Multi-light accessors ----
// Stride per light in `light_data`. 16 base + 4 volumetric + 2 RT-shadow + 3 ambient RGB —
// MUST match `LightData::serialize_to_gpu` (scene_graph.rs) and `VL_STRIDE` in fog_entry.wgsl.
const FLOATS_PER_LIGHT: u32 = 25u;
fn num_lights() -> u32 { return bitcast<u32>(light_data[0u]); }
fn light_offset(i: u32) -> u32 { return 1u + i * FLOATS_PER_LIGHT; }
fn light_type_v(i: u32) -> u32 { return u32(light_data[light_offset(i)]); }
fn light_pos(i: u32) -> vec3<f32> { let o=light_offset(i); return vec3(light_data[o+1u],light_data[o+2u],light_data[o+3u]); }
fn light_dir_v(i: u32) -> vec3<f32> { let o=light_offset(i); return safe_dir(vec3(light_data[o+4u],light_data[o+5u],light_data[o+6u])); }
fn light_intensity_v(i: u32) -> f32 { return light_data[light_offset(i)+7u]; }
fn light_color_v(i: u32) -> vec3<f32> { let o=light_offset(i); return vec3(light_data[o+8u],light_data[o+9u],light_data[o+10u]); }
fn light_range_v(i: u32) -> f32 { return light_data[light_offset(i)+11u]; }
fn light_spot_inner_cos(i: u32) -> f32 { return light_data[light_offset(i)+12u]; }
fn light_spot_outer_cos(i: u32) -> f32 { return light_data[light_offset(i)+13u]; }
fn light_shadow_k(i: u32) -> f32 { return light_data[light_offset(i)+14u]; }
/// Ambient strength (0..1+). Multiplies `light_ambient_color_v` for flat fill.
fn light_ambient_v(i: u32) -> f32 { return light_data[light_offset(i)+15u]; }
// RT-shadow (Stage 4): slot 20 = per-light RT enable; slot 21 = angular soft radius.
fn light_rt_enabled(i: u32) -> bool { return light_data[light_offset(i)+20u] > 0.5; }
fn light_rt_radius(i: u32) -> f32 { return light_data[light_offset(i)+21u]; }
/// Flat ambient tint (linear RGB). Authored color, or HDRI-derived when From HDR is on.
fn light_ambient_color_v(i: u32) -> vec3<f32> {
    let o = light_offset(i);
    return vec3(light_data[o+22u], light_data[o+23u], light_data[o+24u]);
}
/// Strength × tint — the effective flat ambient RGB for shading.
fn light_ambient_rgb_v(i: u32) -> vec3<f32> {
    return light_ambient_color_v(i) * light_ambient_v(i);
}
/// Index of the FIRST directional light (the sun), or 8 when there is none.
///
/// Sun RT shadows (2026-07-28) need to look up the sun's own RT-visibility channel, but unlike
/// point/spot lights the sun is not iterated in the deferred light loop — it shades through the
/// `final_shadow` term — so its index is never otherwise in hand. 8 is the sentinel because
/// `rt_light_vis` already returns fully-lit (1.0) for `li >= 8`, matching the RT visibility pack's
/// own capacity, so a miss degrades to "unshadowed" instead of reading a stranger's channel.
/// A scene with two suns keeps the second on shadow maps — deliberate: the RGBA pack has room for
/// four lights total, and silently spending two of them on suns would be worse than the limit.
fn find_sun_index() -> u32 {
    let n = min(num_lights(), 8u);
    for (var i = 0u; i < n; i = i + 1u) {
        if (light_type_v(i) == 0u) { return i; }
    }
    return 8u;
}

fn point_atten(dist: f32, range: f32) -> f32 {
    // Physical windowed inverse-square (post_flags bit 1024, "Physical Light
    // Falloff", default ON 2026-08-07 "fix the light"): true 1/d² near-field —
    // the legacy 1/(1+d²) was ~5× too dim half a unit from the lamp, so lights
    // never produced the hot near pool. UE/Frostbite window (1-(d/R)⁴)² is C1 at
    // the range boundary (no kink). 1cm² clamp caps the pole at the bulb.
    if ((config.post_flags & 1024u) != 0u) {
        let inv_sq = 1.0 / max(dist * dist, 1e-4);
        if (range <= 0.0) { return inv_sq; }
        let dr2 = (dist / range) * (dist / range);
        let win = saturate(1.0 - dr2 * dr2);
        return inv_sq * win * win;
    }
    // Legacy artistic falloff (toggle off) — bit-identical to the pre-2026-08-07 look.
    if (range <= 0.0) { return 1.0 / (1.0 + dist * dist); }
    let d = dist / range;
    return saturate(1.0 - d * d) / (1.0 + dist * dist);
}
fn spot_atten(cos_a: f32, inner_cos: f32, outer_cos: f32) -> f32 {
    return smoothstep(outer_cos, inner_cos, cos_a);
}
// #endregion LIGHTING

// #region ATLAS
// ---- Per-primitive texture atlas sampling ----
fn prim_texture_id(idx: u32) -> u32 { return prim_u(idx, 83u); }

fn atlas_fetch(tex_id: u32, ix: u32, iy: u32) -> vec3<f32> {
    if (tex_id == 0u) { return vec3(0.5, 0.5, 0.5); }
    let tinfo = tex_atlas_info[tex_id - 1u];
    let offset = tinfo.x;
    let w = tinfo.y;
    let h = tinfo.z;
    let cx = ix % w;
    let cy = iy % h;
    let packed = tex_atlas[offset + cy * w + cx];
    let r = f32((packed >> 24u) & 0xFFu) / 255.0;
    let g = f32((packed >> 16u) & 0xFFu) / 255.0;
    let b = f32((packed >> 8u) & 0xFFu) / 255.0;
    return vec3(r, g, b);
}

fn atlas_sample_bilinear(tex_id: u32, uv: vec2<f32>) -> vec3<f32> {
    if (tex_id == 0u) { return vec3(0.5, 0.5, 0.5); }
    let tinfo = tex_atlas_info[tex_id - 1u];
    let size = vec2<f32>(f32(tinfo.y), f32(tinfo.z));
    let fuv = fract(uv) * size;
    let ix = u32(floor(fuv.x));
    let iy = u32(floor(fuv.y));
    let fx = fract(fuv.x);
    let fy = fract(fuv.y);
    let c00 = atlas_fetch(tex_id, ix, iy);
    let c10 = atlas_fetch(tex_id, ix + 1u, iy);
    let c01 = atlas_fetch(tex_id, ix, iy + 1u);
    let c11 = atlas_fetch(tex_id, ix + 1u, iy + 1u);
    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

fn sample_triplanar_atlas(tex_id: u32, p: vec3<f32>, normal: vec3<f32>, tiling: f32) -> vec3<f32> {
    let blend_w = abs(normal);
    let bsum = blend_w.x + blend_w.y + blend_w.z + 0.0001;
    let w = blend_w / bsum;
    let uv_yz = p.yz * tiling;
    let uv_xz = p.xz * tiling;
    let uv_xy = p.xy * tiling;
    let col_x = atlas_sample_bilinear(tex_id, uv_yz);
    let col_y = atlas_sample_bilinear(tex_id, uv_xz);
    let col_z = atlas_sample_bilinear(tex_id, uv_xy);
    return col_x * w.x + col_y * w.y + col_z * w.z;
}
// #endregion ATLAS

// #region ENV
// Mip chain constants — must match env_ibl.rs (ENV_MIP_LEVELS) + env_prefilter.wgsl.
// Mip semantics (2026-07-18, split-sum IBL): mip 0 = source, mips 1..6 are
// GGX-PREFILTERED on the GPU with roughness = mip/6 (the split-sum FIRST term).
// The buffer tail carries 27 floats of pre-convolved SH9 irradiance (diffuse term).
const ENV_MIP_LEVELS: u32 = 7u;

// Rotate a world direction into environment space (Y-axis env rotation control).
// The ONE place rotation is applied — every env consumer (IBL, background,
// reflections, PT, fog) goes through sample_env_at_level.
fn env_rotate(dir: vec3<f32>) -> vec3<f32> {
    let c = cos(config.env_rotation);
    let s = sin(config.env_rotation);
    return vec3(c * dir.x - s * dir.z, dir.y, s * dir.x + c * dir.z);
}

// Sample equirectangular HDR env map at a specific mip level.
// Buffer layout: [level0: w*h*3] [level1: w/2*h/2*3] ... [level6] [SH9: 27 floats].
fn sample_env_at_level(dir_in: vec3<f32>, level: u32) -> vec3<f32> {
    let dir = env_rotate(dir_in);
    let lv = min(level, ENV_MIP_LEVELS - 1u);
    // Compute mip dimensions and buffer offset
    var mip_w = config.env_width;
    var mip_h = config.env_height;
    var offset = 0u;
    for (var i = 0u; i < lv; i++) {
        offset += mip_w * mip_h * 3u;
        mip_w = max(mip_w / 2u, 1u);
        mip_h = max(mip_h / 2u, 1u);
    }
    // Direction → equirectangular UV
    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x);
    var u = (phi + 3.14159265) / (2.0 * 3.14159265);
    u = u - floor(u); // wrap (rotation can push phi outside ±PI)
    let v = theta / 3.14159265;
    let fw = f32(mip_w);
    let fh = f32(mip_h);
    let fx = u * fw - 0.5;
    let fy = v * fh - 0.5;
    let ix0 = u32(floor(fx) + fw) % mip_w;
    let iy0 = u32(clamp(floor(fy), 0.0, fh - 1.0));
    let ix1 = (ix0 + 1u) % mip_w;
    let iy1 = min(iy0 + 1u, mip_h - 1u);
    let sx = fract(fx + fw);
    let sy = fract(fy + fh);
    let b00 = offset + (iy0 * mip_w + ix0) * 3u;
    let b10 = offset + (iy0 * mip_w + ix1) * 3u;
    let b01 = offset + (iy1 * mip_w + ix0) * 3u;
    let b11 = offset + (iy1 * mip_w + ix1) * 3u;
    let c00 = vec3(env_data[b00], env_data[b00+1u], env_data[b00+2u]);
    let c10 = vec3(env_data[b10], env_data[b10+1u], env_data[b10+2u]);
    let c01 = vec3(env_data[b01], env_data[b01+1u], env_data[b01+2u]);
    let c11 = vec3(env_data[b11], env_data[b11+1u], env_data[b11+2u]);
    return mix(mix(c00, c10, sx), mix(c01, c11, sx), sy);
}

// Sample env map with trilinear interpolation between mip levels
fn sample_env_mip(dir: vec3<f32>, level: f32) -> vec3<f32> {
    let li = u32(floor(level));
    let lf = fract(level);
    let c0 = sample_env_at_level(dir, li);
    if (lf < 0.01 || li >= ENV_MIP_LEVELS - 1u) { return c0; }
    let c1 = sample_env_at_level(dir, li + 1u);
    return mix(c0, c1, lf);
}

// Backward compat: sample level 0 (sharp, no blur)
fn sample_env_raw(dir: vec3<f32>) -> vec3<f32> {
    return sample_env_at_level(dir, 0u);
}

// Sample equirectangular HDR environment map with adjustments (blur, brightness, contrast, saturation).
// `env_blur` is BACKGROUND / sky only — IBL specular uses sample_env_specular instead.
fn sample_env(dir: vec3<f32>) -> vec3<f32> {
    if (config.env_width == 0u) { return unpack_rgb(config.bg_top_packed); }

    // Mip-based background blur: slider 0→1 maps to mip level 0→(N-1)
    let mip_level = config.env_blur * f32(ENV_MIP_LEVELS - 1u);
    var col = sample_env_mip(dir, mip_level);

    // Brightness
    col = col * config.env_brightness;

    // Contrast + Saturation (both use luminance)
    let lum = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = mix(vec3(lum), col, config.env_contrast);
    col = mix(vec3(lum), col, config.env_saturation);

    return max(col, vec3(0.0));
}

// Specular IBL: roughness-driven mips + independent reflection-blur bias.
// Lets lookdev keep sharp metal reflections while soft-focusing the visible sky.
// Multiplied by env_brightness so Intensity scales reflections the same as diffuse fill.
fn sample_env_specular(dir: vec3<f32>, roughness: f32) -> vec3<f32> {
    let max_mip = f32(ENV_MIP_LEVELS - 1u);
    let mip = clamp(roughness * max_mip + config.env_reflection_blur * max_mip, 0.0, max_mip);
    return sample_env_mip(dir, mip) * config.env_brightness;
}

// SH9 irradiance (split-sum diffuse term). 27 pre-convolved floats live after the mip
// chain; coefficients are scaled so the result is the cosine-weighted average incoming
// radiance (same scale the old "blurriest mip" diffuse lookup had). Basis constants
// match env_ibl.rs (SH_C0..SH_C4) — keep in lockstep.
// Multiplied by env_brightness so Intensity scales diffuse IBL (fill lighting), not only
// specular reflections / sky (lookdev: one HDR Intensity for the whole environment).
fn env_sh_irradiance(n_in: vec3<f32>) -> vec3<f32> {
    if (config.env_width == 0u) { return vec3(0.0); }
    let n = env_rotate(n_in);
    // Offset of the SH block = full 7-level chain length.
    var mip_w = config.env_width;
    var mip_h = config.env_height;
    var offset = 0u;
    for (var i = 0u; i < ENV_MIP_LEVELS; i++) {
        offset += mip_w * mip_h * 3u;
        mip_w = max(mip_w / 2u, 1u);
        mip_h = max(mip_h / 2u, 1u);
    }
    let b0 = 0.282095;
    let b1 = 0.488603;
    let b2 = 1.092548;
    let b3 = 0.315392;
    let b4 = 0.546274;
    var basis: array<f32, 9>;
    basis[0] = b0;
    basis[1] = b1 * n.y;
    basis[2] = b1 * n.z;
    basis[3] = b1 * n.x;
    basis[4] = b2 * n.x * n.y;
    basis[5] = b2 * n.y * n.z;
    basis[6] = b3 * (3.0 * n.z * n.z - 1.0);
    basis[7] = b2 * n.x * n.z;
    basis[8] = b4 * (n.x * n.x - n.y * n.y);
    var acc = vec3(0.0);
    for (var i = 0u; i < 9u; i++) {
        let base = offset + i * 3u;
        acc += vec3(env_data[base], env_data[base + 1u], env_data[base + 2u]) * basis[i];
    }
    return max(acc, vec3(0.0)) * config.env_brightness;
}

// #endregion ENV

// NOTE: fog bindings (17,18,24,25) live in fog_entry.wgsl
// NOTE: pathtrace binding (27) lives in pathtrace_entry.wgsl
