// ============================================================
// AZRA — Volumetric Fog Entry Point
// Part of the split compute shader. Requires sdf_shared.wgsl.
// ============================================================

// Fog-specific bindings (NOT in sdf_shared.wgsl)
@group(0) @binding(12) var gbuf_normal_in: texture_2d<f32>;
@group(0) @binding(17) var fog_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(18) var fog_input: texture_2d<f32>;
@group(0) @binding(24) var fog_prev: texture_2d<f32>;

struct PrevCamera {
    cam_pos: vec4<f32>,
    cam_forward: vec4<f32>,
    cam_right: vec4<f32>,
    cam_up: vec4<f32>,
}
@group(0) @binding(25) var<uniform> prev_cam: PrevCamera;

// ============================================================
// Volumetric Fog — production quality atmospheric rendering
// Quality-adaptive ray march, Mie phase, 3D FBM noise,
// SDF volumetric shadows, stochastic jitter, Beer's law
// ============================================================

// --- Hash-based 3D value noise (no texture needed) ---
fn fog_hash13(p3: vec3<f32>) -> f32 {
    var q = fract(p3 * 0.1031);
    q += dot(q, q.zyx + 31.32);
    return fract((q.x + q.y) * q.z);
}

fn fog_noise3d(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(fog_hash13(i + vec3(0.0, 0.0, 0.0)), fog_hash13(i + vec3(1.0, 0.0, 0.0)), u.x),
            mix(fog_hash13(i + vec3(0.0, 1.0, 0.0)), fog_hash13(i + vec3(1.0, 1.0, 0.0)), u.x), u.y),
        mix(mix(fog_hash13(i + vec3(0.0, 0.0, 1.0)), fog_hash13(i + vec3(1.0, 0.0, 1.0)), u.x),
            mix(fog_hash13(i + vec3(0.0, 1.0, 1.0)), fog_hash13(i + vec3(1.0, 1.0, 1.0)), u.x), u.y),
        u.z);
}

fn fog_fbm(p: vec3<f32>, octaves: u32) -> f32 {
    var val = 0.0;
    var amp = 0.5;
    var freq = 1.0;
    for (var i = 0u; i < octaves; i++) {
        val += amp * fog_noise3d(p * freq);
        freq *= 2.0;
        amp *= 0.5;
    }
    return val;
}

// --- Mie phase function (dual-lobe: forward + back scatter) ---
fn mie_phase(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_theta;
    let forward = (1.0 - g2) / (4.0 * 3.14159 * pow(max(denom, 0.0001), 1.5));
    let gb = -g * 0.3;
    let gb2 = gb * gb;
    let back_denom = 1.0 + gb2 - 2.0 * gb * cos_theta;
    let back = (1.0 - gb2) / (4.0 * 3.14159 * pow(max(back_denom, 0.0001), 1.5));
    return forward * 0.75 + back * 0.25;
}

// --- World → UV (current camera) for depth-based fog shadows ---
fn fog_world_to_uv(world_pos: vec3<f32>, dim: vec2<f32>) -> vec2<f32> {
    let to_point = world_pos - config.cam_pos.xyz;
    let ortho_hw = config.cam_pos.w;
    let aspect = dim.x / max(dim.y, 1.0);
    if (ortho_hw > 0.0) {
        let sx = dot(to_point, config.cam_right.xyz) / (ortho_hw * aspect);
        let sy = dot(to_point, config.cam_up.xyz) / ortho_hw;
        return vec2(sx * 0.5 + 0.5, -sy * 0.5 + 0.5);
    }
    let d = dot(to_point, config.cam_forward.xyz);
    if (d < 0.01) { return vec2(-1.0); }
    let proj = to_point / d;
    let fov_s = config.cam_forward.w;
    let sx = dot(proj, config.cam_right.xyz) / (aspect * max(fov_s, 1e-4));
    let sy = dot(proj, config.cam_up.xyz) / max(fov_s, 1e-4);
    return vec2(sx * 0.5 + 0.5, -sy * 0.5 + 0.5);
}

// --- P3 Standard: G-buffer depth soft shadow (no scene_sdf) ---
// March a few steps toward the light; if a sample projects behind opaque G-buffer
// depth, treat the light path as occluded. Approximate but free vs SDF.
fn fog_shadow_depth(p: vec3<f32>, ld: vec3<f32>, steps: u32, max_dist: f32) -> f32 {
    if (steps == 0u) { return 1.0; }
    let dim_u = textureDimensions(gbuf_normal_in);
    let dim = vec2<f32>(f32(dim_u.x), f32(dim_u.y));
    let max_t = min(12.0, max(max_dist - 0.05, 0.3));
    let n = min(steps, 2u); // Standard never pays more than 2 depth probes
    var shadow = 1.0;
    for (var i = 1u; i <= n; i++) {
        let t = (f32(i) / f32(n)) * max_t;
        let sp = p + ld * t;
        let uv = fog_world_to_uv(sp, dim);
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { continue; }
        let coord = vec2<i32>(i32(uv.x * dim.x), i32(uv.y * dim.y));
        let scene_d = textureLoad(gbuf_normal_in, coord, 0).w;
        if (scene_d <= 0.0) { continue; } // sky / empty
        let view_t = dot(sp - config.cam_pos.xyz, config.cam_forward.xyz);
        // Sample sits behind the surface at this pixel → light ray hits geometry.
        if (view_t > scene_d + 0.12) {
            shadow = 0.0;
            break;
        }
    }
    return shadow;
}

// --- High/Ultra: SDF volumetric shadow (uses scene_sdf → voxel O(1) when cached) ---
fn fog_shadow_sdf(p: vec3<f32>, ld: vec3<f32>, steps: u32, max_dist: f32) -> f32 {
    if (steps == 0u || config.num_primitives == 0u) { return 1.0; }
    // Cap the shadow march at the light distance (finite point/spot lights) so geometry BEHIND
    // the light can't falsely occlude the beam. Directional lights (sun at infinity) pass a large
    // sentinel → min() leaves the original 15-unit fog reach. A 0.05 pullback keeps a light that
    // sits just in front of a surface from self-shadowing on that surface.
    let max_t = min(15.0, max(max_dist - 0.05, 0.2));
    var t = 0.2;
    var shadow = 1.0;
    let fog_omega = 1.25; // mild over-relaxation on fog shadow rays
    for (var i = 0u; i < steps; i++) {
        let sp = p + ld * t;
        let d = scene_sdf(sp);
        shadow = min(shadow, smoothstep(0.0, 0.3, d));
        if (shadow < 0.01) { break; }
        t += max(d * fog_omega, max_t / f32(steps));
        if (t > max_t) { break; }
    }
    return shadow;
}

// P3: route shadow — free when steps=0; depth probes on Draft–Medium; SDF on High/Ultra.
fn fog_shadow(p: vec3<f32>, ld: vec3<f32>, steps: u32, max_dist: f32) -> f32 {
    if (steps == 0u) { return 1.0; }
    if (config.render_quality >= 3u) {
        return fog_shadow_sdf(p, ld, steps, max_dist);
    }
    return fog_shadow_depth(p, ld, steps, max_dist);
}

// ============================================================
// Per-light VOLUMETRIC scattering (god rays / light shafts / spot cones / point glow)
// Same ray-march-with-shadow math as the fog above, but scoped PER LIGHT and driven by each
// light's own authored params (enabled / density / softness / zone). Reuses fog_shadow +
// mie_phase. Runs inside the fog march loop so step positions & transmittance are shared.
// ============================================================

// Light buffer stride — MUST match FLOATS_PER_LIGHT (sdf_lighting.wgsl) and
// LightData::serialize_to_gpu (scene_graph.rs). 16 base + 4 volumetric + 2 RT-shadow (Stage 4).
// The LIGHTING region (which defines the shared accessors) is stripped from the fog build, so the
// volumetric accessors are declared locally here; fog does not read the RT-shadow floats (20..21)
// but the stride MUST still count them or every light past index 0 misreads.
const VL_STRIDE: u32 = 25u;
fn vl_count() -> u32 { return bitcast<u32>(light_data[0u]); }
fn vl_off(i: u32) -> u32 { return 1u + i * VL_STRIDE; }
fn vl_norm(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    if (l < 1e-5) { return vec3(0.0, 1.0, 0.0); }
    return v / l;
}

// Match deferred `point_atten` so volume energy peaks AT the lamp (Unreal-style bulb glow).
// MUST track sdf_lighting.wgsl's point_atten including the bit-1024 physical branch —
// a falloff mismatch makes the glow disagree with the surfaces it lights.
fn vol_point_atten(dist: f32, range: f32) -> f32 {
    if ((config.post_flags & 1024u) != 0u) {
        let inv_sq = 1.0 / max(dist * dist, 1e-4);
        if (range <= 1e-4) { return inv_sq; }
        let dr2 = (dist / range) * (dist / range);
        let win = saturate(1.0 - dr2 * dr2);
        return inv_sq * win * win;
    }
    if (range <= 1e-4) { return 1.0 / (1.0 + dist * dist); }
    let d = dist / range;
    return saturate(1.0 - d * d) / (1.0 + dist * dist);
}

// In-scatter radiance from all volumetric-enabled lights at world point `p` for view ray `ray_dir`.
// Returns radiance density (NOT yet multiplied by the march step size or transmittance — the caller
// applies those, matching the fog integration). Zero when no volumetric lights are active.
//
// Point lights use isotropic-ish phase + 1/r² atten so the glow is CENTERED on the lamp
// (was Mie-only → bright blob between camera and light, looked "off-center").
// Spot/Directional keep Mie for classic shafts.
//
// P2: `config.fog_has_vol_lights` is a bitfield of VOL lights (CPU). Iterate only set bits so
// 1-of-N vol scenes do not pay a full light-buffer scan every fog step.
fn vol_light_inscatter(p: vec3<f32>, ray_dir: vec3<f32>, shadow_steps: u32) -> vec3<f32> {
    var total = vec3(0.0);
    let n = vl_count();
    var vol_bits = config.fog_has_vol_lights;
    if (vol_bits == 0u || n == 0u) { return total; }
    // Mask to lights that actually exist in the buffer
    if (n < 32u) {
        vol_bits = vol_bits & ((1u << n) - 1u);
    }
    for (var i = 0u; i < n; i++) {
        if ((vol_bits & (1u << i)) == 0u) { continue; }
        let o = vl_off(i);
        // Bitfield is source of truth; skip re-read of enabled flag (still safe if stale: density 0).
        let ltype = u32(light_data[o]);
        let density = max(light_data[o + 17u], 0.0);
        let softness = clamp(light_data[o + 18u], 0.02, 0.99);
        let zone = max(light_data[o + 19u], 0.001); // effective reach (serialize resolves 0 -> range)
        let l_intensity = light_data[o + 7u];
        let l_color = vec3(light_data[o + 8u], light_data[o + 9u], light_data[o + 10u]);
        // Stored dir = LightData.direction(): for Directional it is the vector TOWARD the source
        // (the L vector); for Spot it is the aim/travel direction (matches deferred_entry.wgsl).
        let l_dir = vl_norm(vec3(light_data[o + 4u], light_data[o + 5u], light_data[o + 6u]));

        var to_light: vec3<f32>; // from p toward the light source (shadow-march direction)
        var reach = 1.0;         // spatial falloff 0..1 (softness feathers the edge)
        var shadow_dist = 1.0e30; // shadow-march cap: light distance (finite) or infinity (directional)
        var dist = 0.0;
        if (ltype == 0u) {
            // Directional: atmospheric shafts. No distance falloff — the shafts span the march.
            to_light = l_dir;
            shadow_dist = 1.0e30; // light at infinity → no cap (original 15u fog reach)
        } else {
            let l_pos = vec3(light_data[o + 1u], light_data[o + 2u], light_data[o + 3u]);
            let d = l_pos - p;
            dist = length(d);
            to_light = d / max(dist, 1e-4);
            shadow_dist = dist; // cap the shadow march AT the light — geometry behind it can't occlude
            // Radial reach: full inside, feathered to 0 at `zone`; softness widens the feather band.
            reach = 1.0 - smoothstep(zone * (1.0 - softness), zone, dist);
            // Energy peaks at the light center (matches deferred point lighting).
            reach *= vol_point_atten(dist, zone);
            if (ltype == 2u) {
                // Spot cone gate — cos of angle between (light->p) and the aim dir (as deferred).
                let inner_c = light_data[o + 12u];
                let outer_c = light_data[o + 13u];
                let cos_a = dot(-to_light, l_dir);
                // Push the outer edge outward by softness so a soft beam has a wider, feathered rim.
                let soft_outer = outer_c - softness * (inner_c - outer_c);
                reach *= smoothstep(soft_outer, inner_c, cos_a);
            }
        }
        if (reach <= 0.001) { continue; }
        let cos_ang = dot(ray_dir, to_light);
        // Point: mostly isotropic (centered bulb). Spot/Dir: strong Mie shafts.
        var ph: f32;
        if (ltype == 1u) {
            let iso = 0.079577; // 1/(4π)
            let mie = mie_phase(cos_ang, config.fog_phase_g * 0.35);
            ph = mix(iso, mie, 0.22);
        } else {
            ph = mie_phase(cos_ang, config.fog_phase_g);
        }
        let sh = fog_shadow(p, to_light, shadow_steps, shadow_dist);
        // Slight near-boost for point so the core reads at the icon, not along the view ray.
        var core = 1.0;
        if (ltype == 1u) {
            core = 1.0 + 1.5 * exp(-dist * dist / max(zone * zone * 0.08, 1e-4));
        }
        total += l_color * l_intensity * density * ph * sh * reach * core;
    }
    return total;
}

// --- Temporal reprojection: project world pos into previous frame's screen ---
fn reproject_fog(world_pos: vec3<f32>, dim: vec2<f32>) -> vec4<f32> {
    let to_point = world_pos - prev_cam.cam_pos.xyz;
    let prev_ortho_hw = prev_cam.cam_pos.w;
    let aspect = dim.x / dim.y;

    var prev_uv: vec2<f32>;
    if (prev_ortho_hw > 0.0) {
        let sx = dot(to_point, prev_cam.cam_right.xyz) / (prev_ortho_hw * aspect);
        let sy = dot(to_point, prev_cam.cam_up.xyz) / prev_ortho_hw;
        prev_uv = vec2(sx * 0.5 + 0.5, -sy * 0.5 + 0.5);
    } else {
        let d = dot(to_point, prev_cam.cam_forward.xyz);
        if (d < 0.01) { return vec4(0.0); }
        let proj = to_point / d;
        let fov_s = prev_cam.cam_forward.w;
        let sx = dot(proj, prev_cam.cam_right.xyz) / (aspect * fov_s);
        let sy = dot(proj, prev_cam.cam_up.xyz) / fov_s;
        prev_uv = vec2(sx * 0.5 + 0.5, -sy * 0.5 + 0.5);
    }

    if (prev_uv.x < 0.0 || prev_uv.x > 1.0 || prev_uv.y < 0.0 || prev_uv.y > 1.0) {
        return vec4(0.0);
    }
    let prev_coord = vec2<i32>(prev_uv * dim);
    return textureLoad(fog_prev, prev_coord, 0);
}

// P2/P3: CPU bitfield of VOL lights (non-zero = at least one). No per-pixel light-buffer scan.
fn fog_any_vol_light() -> bool {
    return config.fog_has_vol_lights != 0u;
}

@compute @workgroup_size(8, 8, 1)
fn pass_fog(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(fog_output);
    // P1: half-res march for Draft/Low/Medium (render_quality < 3).
    // Each invocation covers a 2×2 block in full-res fog_output (~4× fewer marches).
    // High/Ultra (quality ≥ 3) keep full-res for hero look.
    let half_res = config.render_quality < 3u;
    let block = select(1u, 2u, half_res);
    let bx = gid.x * block;
    let by = gid.y * block;
    if (bx >= dim.x || by >= dim.y) { return; }

    // Sample depth at block center (stable for 2×2 splat).
    let sample_x = i32(bx + (block - 1u) / 2u);
    let sample_y = i32(by + (block - 1u) / 2u);
    let coord = vec2<i32>(sample_x, sample_y);

    let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
    let depth = gbuf_n.w;

    let w = f32(dim.x); let h = f32(dim.y);
    let uv_x = (f32(sample_x) + 0.5) / w * 2.0 - 1.0;
    let uv_y = -((f32(sample_y) + 0.5) / h * 2.0 - 1.0);
    let aspect = w / h;
    let fog_ray = camera_ray(uv_x, uv_y, aspect);
    let cam_pos = fog_ray[0];
    let ray_dir = fog_ray[1];

    let max_fog_dist = min(config.fog_max_distance, config.far_clip);
    let max_dist = select(max_fog_dist, min(depth, max_fog_dist), depth > 0.0);
    let march_start = max(config.fog_start, 0.1);

    // Identity fog: no in-scatter, full transmittance (tonemap: hdr * 1 + 0).
    let identity = vec4(0.0, 0.0, 0.0, 1.0);
    if (max_dist <= march_start) {
        for (var dy = 0u; dy < block; dy++) {
            for (var dx = 0u; dx < block; dx++) {
                let c = vec2<i32>(i32(bx + dx), i32(by + dy));
                if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                    textureStore(fog_output, c, identity);
                }
            }
        }
        return;
    }

    let has_vol = fog_any_vol_light();
    let total_range = max_dist - march_start;
    // Optical-depth early-out when haze is negligible and no volumetric lights.
    if (!has_vol && config.fog_density * total_range < 1e-4) {
        for (var dy = 0u; dy < block; dy++) {
            for (var dx = 0u; dx < block; dx++) {
                let c = vec2<i32>(i32(bx + dx), i32(by + dy));
                if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                    textureStore(fog_output, c, identity);
                }
            }
        }
        return;
    }

    // Quality-adaptive step count (P0: Standard caps — not film-volume budgets).
    // Draft/Low/Med: few steps (half-res). High/Ultra: more steps at full res.
    var num_steps: u32;
    var noise_octaves: u32;
    switch (config.render_quality) {
        case 0u: { num_steps = 8u;  noise_octaves = 1u; } // Draft
        case 1u: { num_steps = 12u; noise_octaves = 2u; } // Low
        case 2u: { num_steps = 16u; noise_octaves = 2u; } // Medium (was 64)
        case 3u: { num_steps = 32u; noise_octaves = 3u; } // High (was 96)
        default: { num_steps = 48u; noise_octaves = 4u; } // Ultra
    }
    // P3 gates: shadow march only when user paid (Shadow Steps > 0).
    // Scatter tint without shadows stays cheap (shadow = 1).
    let shadow_steps = u32(config.fog_shadow_steps);
    let want_sun_shafts = config.fog_scatter > 0.01 && shadow_steps > 0u;
    // When shafts requested, use shadow_steps; else never call shadow march.
    let eff_shadow_steps = select(0u, shadow_steps, want_sun_shafts || has_vol);

    let light_dir = normalize(config.light_dir.xyz);
    let light_color = config.light_color.xyz;
    let light_intensity = config.light_dir.w;
    let cos_angle = dot(ray_dir, light_dir);
    let phase = mie_phase(cos_angle, config.fog_phase_g);
    // Directional scatter amount: full when shafts on; soft tint when scatter-only (no shadow cost).
    let scatter_amt = select(
        config.fog_scatter * 0.35, // soft key tint without occlusion (cheap)
        config.fog_scatter,        // full god-ray strength when shadow steps paid
        want_sun_shafts,
    );

    // Stochastic jitter to break banding
    let jitter = fog_hash13(vec3(f32(gid.x), f32(gid.y), f32(gid.x ^ gid.y) * 0.137));

    var accumulated = vec3(0.0);
    var transmittance = 1.0;
    let fog_ambient = config.fog_color;

    for (var i = 0u; i < num_steps; i++) {
        // Quadratic step distribution: denser near camera, sparser far away
        let frac = (f32(i) + jitter) / f32(num_steps);
        let t = march_start + frac * frac * total_range;
        // Adaptive step_size for density integration (derivative of quadratic mapping)
        let step_size = 2.0 * frac * total_range / f32(num_steps);
        let p = cam_pos + ray_dir * t;

        // Height-based exponential density
        let height_factor = exp(-max(0.0, p.y - config.floor_y) * config.fog_height_falloff);

        // 3D FBM noise modulation (default strength 0 → skipped)
        var noise_mod = 1.0;
        if (config.fog_noise_strength > 0.01) {
            let n = fog_fbm(p * config.fog_noise_scale, noise_octaves);
            noise_mod = mix(1.0, n, config.fog_noise_strength);
            noise_mod = max(noise_mod, 0.0);
        }

        let local_density = config.fog_density * height_factor * noise_mod * step_size;

        // Physical absorption/scattering split (Beer's law)
        let extinction = local_density * (1.0 + config.fog_absorption);
        let scattering = local_density;

        // Shadow: free when eff_shadow_steps=0; depth (Standard) or SDF (High/Ultra).
        let shadow = fog_shadow(p, light_dir, eff_shadow_steps, 1.0e30);

        // In-scattering: Mie phase * light * shadow + ambient
        let in_scatter = (light_color * light_intensity * phase * scatter_amt * shadow
                         + fog_ambient * 0.3) * scattering;

        accumulated += in_scatter * transmittance;

        // Per-light volumetric shafts only when CPU flag says VOL lights exist.
        if (has_vol) {
            let vol_in = vol_light_inscatter(p, ray_dir, max(shadow_steps, 1u)) * step_size;
            accumulated += vol_in * transmittance;
        }

        transmittance *= exp(-extinction);

        if (transmittance < 0.005) { break; }
    }

    var final_fog = vec4(accumulated, transmittance);

    // Temporal reprojection blend
    if (config.fog_temporal_blend > 0.01 && config.fog_frame_index > 0u) {
        let mid_t = (march_start + max_dist) * 0.5;
        let mid_world = cam_pos + ray_dir * mid_t;
        let dim_f = vec2<f32>(f32(dim.x), f32(dim.y));
        let prev_fog = reproject_fog(mid_world, dim_f);

        var confidence = config.fog_temporal_blend;
        // Reject disoccluded samples (prev was empty/black)
        if (prev_fog.a <= 0.001 && prev_fog.r + prev_fog.g + prev_fog.b < 0.001) {
            confidence = 0.0;
        }
        final_fog = mix(final_fog, prev_fog, confidence);
    }

    // Write block (1×1 full-res or 2×2 half-res splat)
    for (var dy = 0u; dy < block; dy++) {
        for (var dx = 0u; dx < block; dx++) {
            let c = vec2<i32>(i32(bx + dx), i32(by + dy));
            if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                textureStore(fog_output, c, final_fog);
            }
        }
    }
}

// ============================================================
// P2 — Analytic Lite fog (closed-form height haze, no ray march)
// Target: interaction orbit + Draft when no volumetric lights.
// Cost: 1 G-buffer load + math. No scene_sdf, FBM, or god-rays.
// Output layout matches full pass: rgba = (in_scatter.rgb, transmittance)
// tonemap: hdr = hdr * a + rgb
// ============================================================

/// Integrate dens * exp(-beta * max(0, y - floor_y)) along ray for t in [t0, t1].
/// y(t) = cam_y + dir_y * t. Piecewise split at floor crossing.
fn fog_optical_depth_height(
    cam_y: f32, dir_y: f32, t0: f32, t1: f32,
    dens: f32, beta: f32, floor_y: f32,
) -> f32 {
    if (t1 <= t0 || dens <= 1e-8) { return 0.0; }
    // Degenerate falloff → uniform slab
    if (beta < 1e-5) {
        return dens * (t1 - t0);
    }

    // Split interval where y crosses floor_y (height_factor switches 1 ↔ exp).
    var ts: array<f32, 3>;
    var n = 0u;
    ts[0] = t0; n = 1u;
    if (abs(dir_y) > 1e-5) {
        let t_cross = (floor_y - cam_y) / dir_y;
        if (t_cross > t0 + 1e-4 && t_cross < t1 - 1e-4) {
            ts[1] = t_cross;
            n = 2u;
        }
    }
    ts[n] = t1;
    n = n + 1u;

    var tau = 0.0;
    for (var s = 0u; s + 1u < n; s++) {
        let a = ts[s];
        let b = ts[s + 1u];
        if (b <= a) { continue; }
        let y_mid = cam_y + dir_y * ((a + b) * 0.5);
        if (y_mid < floor_y) {
            // Below floor: height_factor = 1
            tau += dens * (b - a);
        } else {
            // Above floor: dens * exp(-beta * (cam_y + dir_y*t - floor_y))
            let k = beta * dir_y;
            let base = dens * exp(-beta * (cam_y - floor_y));
            if (abs(k) < 1e-5) {
                tau += dens * exp(-beta * (y_mid - floor_y)) * (b - a);
            } else {
                // ∫_a^b base * exp(-k t) dt = base * (exp(-k a) - exp(-k b)) / k
                tau += base * (exp(-k * a) - exp(-k * b)) / k;
            }
        }
    }
    return max(tau, 0.0);
}

@compute @workgroup_size(8, 8, 1)
fn pass_fog_lite(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(fog_output);
    // Always half-res 2×2 splat — Lite is the cheap path.
    let block = 2u;
    let bx = gid.x * block;
    let by = gid.y * block;
    if (bx >= dim.x || by >= dim.y) { return; }

    let sample_x = i32(bx);
    let sample_y = i32(by);
    let coord = vec2<i32>(sample_x, sample_y);

    let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
    let depth = gbuf_n.w;

    let w = f32(dim.x); let h = f32(dim.y);
    let uv_x = (f32(sample_x) + 0.5) / w * 2.0 - 1.0;
    let uv_y = -((f32(sample_y) + 0.5) / h * 2.0 - 1.0);
    let aspect = w / h;
    let fog_ray = camera_ray(uv_x, uv_y, aspect);
    let cam_pos = fog_ray[0];
    let ray_dir = fog_ray[1];

    let max_fog_dist = min(config.fog_max_distance, config.far_clip);
    let max_dist = select(max_fog_dist, min(depth, max_fog_dist), depth > 0.0);
    let march_start = max(config.fog_start, 0.1);

    let identity = vec4(0.0, 0.0, 0.0, 1.0);
    if (max_dist <= march_start || config.fog_density < 1e-6) {
        for (var dy = 0u; dy < block; dy++) {
            for (var dx = 0u; dx < block; dx++) {
                let c = vec2<i32>(i32(bx + dx), i32(by + dy));
                if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                    textureStore(fog_output, c, identity);
                }
            }
        }
        return;
    }

    let dens = config.fog_density;
    let beta = config.fog_height_falloff;
    var tau = fog_optical_depth_height(
        cam_pos.y, ray_dir.y, march_start, max_dist, dens, beta, config.floor_y,
    );
    // Match full path: extinction includes absorption factor on optical depth.
    let T = exp(-tau * (1.0 + config.fog_absorption));

    let light_dir = normalize(config.light_dir.xyz);
    let light_color = config.light_color.xyz;
    let light_intensity = config.light_dir.w;
    let cos_angle = dot(ray_dir, light_dir);
    let phase = mie_phase(cos_angle, config.fog_phase_g);
    let fog_ambient = config.fog_color;
    // Single-scatter approx: L * (1-T). No SDF shadow / noise / per-light vol.
    let L = fog_ambient * 0.3 + light_color * light_intensity * phase * config.fog_scatter;
    let fog_rgb = L * (1.0 - T);
    let final_fog = vec4(fog_rgb, T);

    for (var dy = 0u; dy < block; dy++) {
        for (var dx = 0u; dx < block; dx++) {
            let c = vec2<i32>(i32(bx + dx), i32(by + dy));
            if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                textureStore(fog_output, c, final_fog);
            }
        }
    }
}
