// ============================================================
// AZRA — RT Projected Shadows: spatial bilateral denoise (RT_SHADOWS_DESIGN Stage 2)
// ============================================================
// Edge-stopping joint bilateral over the 4-channel per-light visibility texture the
// rt_shadow pass writes. This is pt_denoise.wgsl's joint-bilateral APPROACH (Dammertz 2010:
// spatial gaussian × normal similarity × depth similarity) — but it filters ALL FOUR channels
// (four packed lights), so it cannot reuse pt_denoise verbatim, which treats the texture as
// rgb+passthrough-alpha and would leave light 3 (the .w channel) un-denoised. Edge-stopping on
// the G-buffer normal+depth keeps shadow silhouettes crisp while the interior penumbra noise is
// smoothed. Self-contained (no SDF header) — every binding is local to this pass.
// ============================================================

struct RtDenoiseParams {
    radius: u32,   // kernel half-size in pixels (0 = passthrough)
    width: u32,
    height: u32,
    _pad: u32,
}

@group(0) @binding(0) var in_vis      : texture_2d<f32>;        // rt_shadow_vis (noisy, RGBA = 4 lights)
@group(0) @binding(1) var gbuf_normal : texture_2d<f32>;        // .xyz = normal, .w = world distance
@group(0) @binding(2) var out_vis     : texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params : RtDenoiseParams;

fn gauss_spatial(offset: vec2<f32>, sigma: f32) -> f32 {
    return exp(-dot(offset, offset) / (2.0 * sigma * sigma));
}
fn weight_normal(n_center: vec3<f32>, n_sample: vec3<f32>) -> f32 {
    let d = max(dot(n_center, n_sample), 0.0);
    return pow(d, 32.0);
}
fn weight_depth(d_center: f32, d_sample: f32) -> f32 {
    if (d_center <= 0.0 || d_sample <= 0.0) { return 0.0; } // sky / miss
    let rel = abs(d_center - d_sample) / max(d_center, 0.001);
    return exp(-rel * rel * 100.0);
}

@compute @workgroup_size(8, 8, 1)
fn rt_shadow_denoise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = gid.xy;
    if (px.x >= params.width || px.y >= params.height) { return; }
    let coord = vec2<i32>(px);

    let center_vis = textureLoad(in_vis, coord, 0);
    let center_nd  = textureLoad(gbuf_normal, coord, 0);
    let center_n   = center_nd.xyz;
    let center_d   = center_nd.w;

    // Background (no surface) or passthrough radius: copy through unchanged so the anchor path
    // and non-surface pixels are bit-stable.
    if (center_d <= 0.0 || params.radius == 0u) {
        textureStore(out_vis, coord, center_vis);
        return;
    }

    let r = i32(params.radius);
    let sigma = f32(params.radius);
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            let sp = clamp(coord + vec2<i32>(dx, dy),
                vec2<i32>(0), vec2<i32>(i32(params.width) - 1, i32(params.height) - 1));
            let s_vis = textureLoad(in_vis, sp, 0);
            let s_nd  = textureLoad(gbuf_normal, sp, 0);
            let w = gauss_spatial(vec2<f32>(f32(dx), f32(dy)), sigma)
                  * weight_normal(center_n, s_nd.xyz)
                  * weight_depth(center_d, s_nd.w);
            sum += s_vis * w;
            wsum += w;
        }
    }
    let denoised = select(center_vis, sum / wsum, wsum > 0.0);
    textureStore(out_vis, coord, denoised);
}
