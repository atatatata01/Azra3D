// ============================================================
// AZRA — PT spatial denoise (Section J + M / P3.6 SVGF-lite)
//
// Section M: true multi-pass à-trous — CPU dispatches this kernel
// several times with step = 1,2,4(,8), ping-ponging intermediate
// textures. Each pass is a 3×3 joint bilateral at the given step.
//
// Guides: normal/depth + albedo. History-adaptive strength on pass 0.
// Display-only — accumulation chain is unaffected.
// ============================================================

// Uniform layout MUST match the 32-byte CPU upload ([u32; 8]).
// Do NOT use vec3 pad — WGSL aligns vec3 to 16 B and grows the struct to 48.
struct DenoiseParams {
    strength: f32,   // 0.0 = no effect, 1.0 = full denoise
    radius: u32,     // unused for step kernel (kept for UI compat); reserved
    width: u32,      // noisy_input size (half or full)
    height: u32,
    // Section D: when lighting is half-res, sample full-res gbuf at (px * gbuf_scale).
    gbuf_scale: u32,
    // Section M: à-trous dilation (1,2,4,8)
    step: u32,
    // Section M: pass index (0 = first; history fade applies here)
    pass_idx: u32,
    _pad2: u32,
};

@group(0) @binding(0) var noisy_input : texture_2d<f32>;
@group(0) @binding(1) var gbuf_normal : texture_2d<f32>;
@group(0) @binding(2) var output_hdr  : texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params : DenoiseParams;
@group(0) @binding(4) var gbuf_albedo : texture_2d<f32>;
struct LuminanceMoments {
    mean: f32,
    m2: f32,
    count: f32,
    pad: f32,
};
@group(0) @binding(5) var<storage, read> luminance_moments: array<LuminanceMoments>;

fn gbuf_coord(px: vec2<u32>) -> vec2<i32> {
    let s = max(params.gbuf_scale, 1u);
    let gdim = textureDimensions(gbuf_normal);
    let gx = i32(min(px.x * s + s / 2u, gdim.x - 1u));
    let gy = i32(min(px.y * s + s / 2u, gdim.y - 1u));
    return vec2<i32>(gx, gy);
}

fn luma(c: vec3<f32>) -> f32 {
    return max(dot(c, vec3(0.2126, 0.7152, 0.0722)), 0.0);
}

fn gauss_spatial(offset: vec2<f32>, sigma: f32) -> f32 {
    return exp(-dot(offset, offset) / (2.0 * sigma * sigma + 1e-5));
}

fn weight_normal(n_center: vec3<f32>, n_sample: vec3<f32>) -> f32 {
    let d = max(dot(n_center, n_sample), 0.0);
    return pow(d, 32.0);
}

fn depth_gradient(px: vec2<u32>, center_nd: vec4<f32>) -> vec2<f32> {
    var gradient = vec2<f32>(0.0);
    let coord = vec2<i32>(px);
    let extent = vec2<i32>(i32(params.width), i32(params.height));
    for (var axis = 0u; axis < 2u; axis++) {
        var offset = vec2<i32>(0);
        offset[axis] = 1;
        let lower = coord - offset;
        let upper = coord + offset;
        var lower_slope = 0.0;
        var upper_slope = 0.0;
        var lower_valid = false;
        var upper_valid = false;
        if (all(lower >= vec2<i32>(0))) {
            let guide = textureLoad(gbuf_normal, gbuf_coord(vec2<u32>(lower)), 0);
            lower_valid = guide.w > 0.0 && dot(center_nd.xyz, guide.xyz) > 0.9;
            lower_slope = center_nd.w - guide.w;
        }
        if (all(upper < extent)) {
            let guide = textureLoad(gbuf_normal, gbuf_coord(vec2<u32>(upper)), 0);
            upper_valid = guide.w > 0.0 && dot(center_nd.xyz, guide.xyz) > 0.9;
            upper_slope = guide.w - center_nd.w;
        }
        if (lower_valid && upper_valid) {
            if (lower_slope * upper_slope > 0.0) {
                gradient[axis] = select(upper_slope, lower_slope, abs(lower_slope) < abs(upper_slope));
            }
        } else if (lower_valid) {
            gradient[axis] = lower_slope;
        } else if (upper_valid) {
            gradient[axis] = upper_slope;
        }
    }
    return gradient;
}

fn weight_depth(d_center: f32, d_sample: f32, gradient: vec2<f32>, offset: vec2<f32>) -> f32 {
    if (d_center <= 0.0 || d_sample <= 0.0) { return 0.0; }
    let residual = abs(d_sample - d_center - dot(gradient, offset));
    let tolerance = max(d_center * 0.002, 1e-6) + 0.1 * dot(abs(gradient), abs(offset));
    return exp(-residual / tolerance);
}

fn weight_albedo(a_center: vec3<f32>, a_sample: vec3<f32>) -> f32 {
    let d = length(a_center - a_sample);
    return exp(-d * d * 40.0);
}

fn variance_of_mean(px: vec2<u32>) -> f32 {
    let moments = luminance_moments[px.y * params.width + px.x];
    if (moments.count < 2.0) { return 0.0; }
    return max(moments.m2 / ((moments.count - 1.0) * moments.count), 0.0);
}

fn weight_luma(c_center: vec3<f32>, c_sample: vec3<f32>, variance: f32) -> f32 {
    let lc = luma(c_center);
    let ls = luma(c_sample);
    let sigma = max(4.0 * sqrt(variance), 1e-6);
    return exp(-abs(lc - ls) / sigma);
}

@compute @workgroup_size(8, 8, 1)
fn pass_pt_denoise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = gid.xy;
    if (px.x >= params.width || px.y >= params.height) { return; }

    let center = textureLoad(noisy_input, vec2<i32>(px), 0);
    let center_nd = textureLoad(gbuf_normal, gbuf_coord(px), 0);
    let center_n = center_nd.xyz;
    let center_d = center_nd.w;
    let center_a = textureLoad(gbuf_albedo, gbuf_coord(px), 0).rgb;
    let center_variance = variance_of_mean(px);
    let moments = luminance_moments[px.y * params.width + px.x];

    if (center_d <= 0.0 || params.strength <= 0.0) {
        textureStore(output_hdr, vec2<i32>(px), center);
        return;
    }

    let target_error = 0.0001 + 0.02 * abs(moments.mean);
    let relative_error = sqrt(center_variance) / target_error;
    let variance_strength = select(smoothstep(1.0, 4.0, relative_error), 1.0, moments.count < 2.0);
    let pass_fade = select(1.0, 0.85, params.pass_idx > 0u);
    let eff_strength = clamp(params.strength * variance_strength * pass_fade, 0.0, 1.0);
    if (eff_strength < 1e-4) {
        textureStore(output_hdr, vec2<i32>(px), center);
        return;
    }

    // Section M: single à-trous level — 3×3 neighborhood at `step` texel spacing
    let step = i32(max(params.step, 1u));
    let sigma = f32(step) * 1.5;
    let gradient = depth_gradient(px, center_nd);
    var sum_color = vec3<f32>(0.0);
    var sum_weight = 0.0;

    for (var oy = -1; oy <= 1; oy++) {
        for (var ox = -1; ox <= 1; ox++) {
            let dx = ox * step;
            let dy = oy * step;
            let sp = vec2<i32>(
                clamp(i32(px.x) + dx, 0, i32(params.width) - 1),
                clamp(i32(px.y) + dy, 0, i32(params.height) - 1),
            );
            let s_color = textureLoad(noisy_input, sp, 0).rgb;
            let s_nd = textureLoad(gbuf_normal, gbuf_coord(vec2<u32>(u32(sp.x), u32(sp.y))), 0);
            let s_alb = textureLoad(gbuf_albedo, gbuf_coord(vec2<u32>(u32(sp.x), u32(sp.y))), 0).rgb;

            let w_s = gauss_spatial(vec2<f32>(f32(dx), f32(dy)), sigma);
            let w_n = weight_normal(center_n, s_nd.xyz);
            let offset = vec2<f32>(sp - vec2<i32>(px));
            let w_d = weight_depth(center_d, s_nd.w, gradient, offset);
            let w_a = weight_albedo(center_a, s_alb);
            let sample_px = vec2<u32>(sp);
            let sample_moments = luminance_moments[sample_px.y * params.width + sample_px.x];
            let w_l = select(
                weight_luma(center.rgb, s_color, center_variance + variance_of_mean(sample_px)),
                1.0,
                moments.count < 2.0 || sample_moments.count < 2.0,
            );
            let w = w_s * w_n * w_d * w_a * w_l;

            sum_color += s_color * w;
            sum_weight += w;
        }
    }

    let denoised = select(center.rgb, sum_color / max(sum_weight, 1e-6), sum_weight > 1e-5);
    let final_color = mix(center.rgb, denoised, eff_strength);
    textureStore(output_hdr, vec2<i32>(px), vec4<f32>(final_color, center.a));
}
