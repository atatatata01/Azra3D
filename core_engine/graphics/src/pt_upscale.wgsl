// ============================================================
// pt_upscale.wgsl — Depth-aware half → full lighting upscale (Section B)
//
// Path-trace lighting at half resolution; keep full-res G-buffer guides
// (normal + depth in gbuf_normal) so silhouettes stay sharp.
//
// Depth-aware bilinear + light edge soften. Reduces horizontal zipper
// that nearest-only upscale produced on flat surfaces.
// ============================================================

@group(0) @binding(0) var half_color: texture_2d<f32>;   // half-res PT lighting (rgba16f)
@group(0) @binding(1) var gbuf_normal: texture_2d<f32>;  // full-res: xyz=normal, w=depth
@group(0) @binding(2) var full_hdr: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: PtUpscaleParams;

struct PtUpscaleParams {
    full_w: u32,
    full_h: u32,
    half_w: u32,
    half_h: u32,
}

fn pt_upscale_guide_coord(hc: vec2<i32>) -> vec2<i32> {
    let fw = max(params.full_w, 1u);
    let fh = max(params.full_h, 1u);
    let hw = max(params.half_w, 1u);
    let hh = max(params.half_h, 1u);
    return clamp(vec2<i32>(
            i32((u32(hc.x) * fw + hw / 2u) / hw),
            i32((u32(hc.y) * fh + hh / 2u) / hh)),
        vec2<i32>(0), vec2<i32>(i32(fw) - 1, i32(fh) - 1));
}

fn pt_upscale_sample_half(
    hc: vec2<i32>,
    half_max: vec2<i32>,
    d0: f32,
    n0: vec3<f32>,
    depth_tol: f32,
) -> vec4<f32> {
    let hcc = clamp(hc, vec2<i32>(0), half_max);
    let ng = textureLoad(gbuf_normal, pt_upscale_guide_coord(hcc), 0);
    let ndot = dot(normalize(ng.xyz + vec3(1e-5)), normalize(n0 + vec3(1e-5)));
    if (abs(ng.w - d0) <= depth_tol && ndot >= 0.85) {
        return vec4<f32>(textureLoad(half_color, hcc, 0).rgb, 1.0);
    }
    return vec4<f32>(0.0);
}

@compute @workgroup_size(8, 8, 1)
fn pass_pt_upscale(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.full_w || gid.y >= params.full_h) { return; }
    let full = vec2<i32>(i32(gid.x), i32(gid.y));

    let fw = max(params.full_w, 1u);
    let fh = max(params.full_h, 1u);
    let hw = max(params.half_w, 1u);
    let hh = max(params.half_h, 1u);
    // Continuous half-res position of this full pixel's center
    let half_pos = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2(0.5))
        * vec2<f32>(f32(hw), f32(hh)) / vec2<f32>(f32(fw), f32(fh)) - vec2(0.5);
    let hx = i32(floor(half_pos.x));
    let hy = i32(floor(half_pos.y));
    let fx = half_pos.x - f32(hx);
    let fy = half_pos.y - f32(hy);
    let half_max = vec2<i32>(i32(hw) - 1, i32(hh) - 1);

    let guide = textureLoad(gbuf_normal, full, 0);
    let d0 = guide.w;
    let n0 = guide.xyz;
    let depth_tol = max(0.002, abs(d0) * 0.008);

    // Background / sky (no depth): plain bilinear average
    if (d0 <= 1e-5) {
        var sum = vec3(0.0);
        var wsum = 0.0;
        for (var oy = 0; oy < 2; oy++) {
            for (var ox = 0; ox < 2; ox++) {
                let hc = clamp(vec2<i32>(hx + ox, hy + oy), vec2<i32>(0), half_max);
                let wx = select(1.0 - fx, fx, ox == 1);
                let wy = select(1.0 - fy, fy, oy == 1);
                let w = max(wx, 0.0) * max(wy, 0.0);
                sum += textureLoad(half_color, hc, 0).rgb * w;
                wsum += w;
            }
        }
        textureStore(full_hdr, full, vec4(sum / max(wsum, 1e-5), 1.0));
        return;
    }

    // Depth/normal-gated bilinear: reject taps that cross silhouettes, renormalize.
    // This kills the horizontal zipper from nearest-only selection on flat surfaces.
    var acc = vec3(0.0);
    var wsum = 0.0;
    var fallback = vec3(0.0);
    var fallback_set = false;

    for (var oy = 0; oy < 2; oy++) {
        for (var ox = 0; ox < 2; ox++) {
            let hc = vec2<i32>(hx + ox, hy + oy);
            let hcc = clamp(hc, vec2<i32>(0), half_max);
            let col = textureLoad(half_color, hcc, 0).rgb;
            if (!fallback_set) {
                fallback = col;
                fallback_set = true;
            }
            let wx = select(1.0 - fx, fx, ox == 1);
            let wy = select(1.0 - fy, fy, oy == 1);
            var w = max(wx, 1e-4) * max(wy, 1e-4);

            let fc = pt_upscale_guide_coord(hcc);
            let g = textureLoad(gbuf_normal, fc, 0);
            let dd = abs(g.w - d0);
            let ndot = max(dot(normalize(g.xyz + vec3(1e-5)), normalize(n0 + vec3(1e-5))), 0.0);
            // Soft gate: far depth / bad normal → drop weight instead of hard reject
            // (hard reject caused 1-tap nearest and zipper bands).
            let depth_w = exp(-dd / max(depth_tol, 1e-4));
            let normal_w = pow(clamp(ndot, 0.0, 1.0), 8.0);
            w *= depth_w * normal_w;

            acc += col * w;
            wsum += w;
        }
    }

    var sharp: vec3<f32>;
    if (wsum > 1e-4) {
        sharp = acc / wsum;
    } else {
        // All taps rejected — nearest half texel
        let center_hc = clamp(vec2<i32>(hx, hy), vec2<i32>(0), half_max);
        sharp = textureLoad(half_color, center_hc, 0).rgb;
    }

    // Light guided unsharp on a depth-gated cross (reduces blockiness without row bias)
    if (d0 > 1e-5) {
        let c0 = sharp;
        var guided = vec4<f32>(c0, 1.0);
        guided += pt_upscale_sample_half(vec2<i32>(hx - 1, hy), half_max, d0, n0, depth_tol);
        guided += pt_upscale_sample_half(vec2<i32>(hx + 1, hy), half_max, d0, n0, depth_tol);
        guided += pt_upscale_sample_half(vec2<i32>(hx, hy - 1), half_max, d0, n0, depth_tol);
        guided += pt_upscale_sample_half(vec2<i32>(hx, hy + 1), half_max, d0, n0, depth_tol);
        let blur = guided.rgb / max(guided.w, 1e-5);
        // Mild sharpen only — strong unsharp re-introduced half-texel blocks as lines
        sharp = clamp(c0 + (c0 - blur) * 0.18, vec3(0.0), vec3(64.0));
    }

    textureStore(full_hdr, full, vec4(sharp, 1.0));
}
