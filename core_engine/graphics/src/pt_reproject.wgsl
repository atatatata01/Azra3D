// ============================================================
// AZRA — PT Temporal Reprojection (P3.5 + Section C half-res)
// Reprojects previous PT accumulation into the current camera.
// Supports half-res lighting with full-res G-buffer guides:
//   screen_size = lighting dims, full_size = gbuf dims.
// ============================================================

struct PtReprojParams {
    prev_cam_pos: vec4<f32>,
    prev_cam_forward: vec4<f32>,
    prev_cam_right: vec4<f32>,
    prev_cam_up: vec4<f32>,
    cur_cam_pos: vec4<f32>,
    cur_cam_forward: vec4<f32>,
    cur_cam_right: vec4<f32>,
    cur_cam_up: vec4<f32>,
    screen_size: vec2<u32>,
    hist_cap: f32,
    depth_reject: f32,
    full_size: vec2<u32>,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var cur_normal_dist: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: PtReprojParams;
@group(0) @binding(2) var prev_normal_dist: texture_2d<f32>;
@group(0) @binding(3) var prev_accum: texture_2d<f32>;
@group(0) @binding(4) var out_accum: texture_storage_2d<rgba32float, write>;
@group(0) @binding(5) var<storage, read> prev_moments: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> out_moments: array<vec4<f32>>;

// Map lighting pixel → full-res gbuf pixel (center of the footprint).
fn lighting_to_gbuf(lx: u32, ly: u32) -> vec2<i32> {
    let sw = max(params.screen_size.x, 1u);
    let sh = max(params.screen_size.y, 1u);
    let fw = max(params.full_size.x, sw);
    let fh = max(params.full_size.y, sh);
    let gx = i32((lx * fw + sw / 2u) / sw);
    let gy = i32((ly * fh + sh / 2u) / sh);
    return vec2<i32>(
        clamp(gx, 0, i32(fw) - 1),
        clamp(gy, 0, i32(fh) - 1),
    );
}

fn cur_pixel_to_world(full_px: vec2<f32>, dist: f32, fw: f32, fh: f32) -> vec3<f32> {
    let uv_x = (full_px.x / fw) * 2.0 - 1.0;
    let uv_y = -((full_px.y / fh) * 2.0 - 1.0);
    let aspect = fw / fh;
    let fs = select(params.cur_cam_forward.w, 1.0, params.cur_cam_forward.w <= 0.0);
    let rd = normalize(params.cur_cam_forward.xyz
        + params.cur_cam_right.xyz * (uv_x * aspect * fs)
        + params.cur_cam_up.xyz * (uv_y * fs));
    return params.cur_cam_pos.xyz + rd * dist;
}

// Project world → previous *lighting* UV (not full gbuf).
fn world_to_prev_lighting(world_p: vec3<f32>, lw: f32, lh: f32, fw: f32, fh: f32) -> vec3<f32> {
    let aspect = fw / fh;
    let d = world_p - params.prev_cam_pos.xyz;
    let z = dot(d, normalize(params.prev_cam_forward.xyz));
    if (z <= 0.0) { return vec3(-1.0, -1.0, -1.0); }
    let fs = select(params.prev_cam_forward.w, 1.0, params.prev_cam_forward.w <= 0.0);
    let u = dot(d, params.prev_cam_right.xyz) / (z * aspect * fs);
    let v = dot(d, params.prev_cam_up.xyz) / (z * fs);
    // Full-res NDC → lighting pixel
    let full_sx = (u * 0.5 + 0.5) * fw;
    let full_sy = (-v * 0.5 + 0.5) * fh;
    let sx = full_sx * (lw / fw);
    let sy = full_sy * (lh / fh);
    return vec3(sx, sy, z);
}

@compute @workgroup_size(8, 8, 1)
fn pt_reproject(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = params.screen_size.x;
    let h = params.screen_size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let invalid = vec4(0.0, 0.0, 0.0, 0.0);
    let moment_index = gid.y * w + gid.x;
    out_moments[moment_index] = invalid;

    // Ortho: skip. NOT because of the G-buffer depth convention any more — that was
    // fixed 2026-08-06 (gbuf_depth() in render/src/gbuffer.wgsl writes the ray parameter
    // in both projections). world_to_prev_lighting() below is still perspective-only
    // (divides by forward-z with fov_scale, no ortho branch), so it would reproject to
    // the wrong pixels. Give that helper an ortho branch before lifting this.
    if (params.cur_cam_pos.w > 0.0 || params.prev_cam_pos.w > 0.0) {
        textureStore(out_accum, coord, invalid);
        return;
    }

    let fw = f32(max(params.full_size.x, w));
    let fh = f32(max(params.full_size.y, h));
    let lw = f32(w);
    let lh = f32(h);

    let gcoord = lighting_to_gbuf(gid.x, gid.y);
    let cur = textureLoad(cur_normal_dist, gcoord, 0);
    let cur_dist = cur.w;
    if (cur_dist <= 0.0) {
        textureStore(out_accum, coord, invalid);
        return;
    }
    let cur_n = cur.xyz;

    let full_px = vec2(
        (f32(gid.x) + 0.5) * (fw / lw),
        (f32(gid.y) + 0.5) * (fh / lh),
    );
    let world_p = cur_pixel_to_world(full_px, cur_dist, fw, fh);
    let s = world_to_prev_lighting(world_p, lw, lh, fw, fh);
    if (s.z <= 0.0 || s.x < 0.5 || s.y < 0.5 || s.x >= lw - 0.5 || s.y >= lh - 0.5) {
        textureStore(out_accum, coord, invalid);
        return;
    }

    // Disocclusion on full-res guides at the previous lighting footprint center
    let prev_light = vec2<i32>(i32(s.x), i32(s.y));
    let prev_g = lighting_to_gbuf(u32(prev_light.x), u32(prev_light.y));
    let prev_nd = textureLoad(prev_normal_dist, prev_g, 0);
    if (prev_nd.w <= 0.0) {
        textureStore(out_accum, coord, invalid);
        return;
    }
    let expected = length(world_p - params.prev_cam_pos.xyz);
    if (abs(prev_nd.w - expected) > params.depth_reject * max(expected, 1e-3)) {
        textureStore(out_accum, coord, invalid);
        return;
    }
    if (dot(cur_n, prev_nd.xyz) < 0.5) {
        textureStore(out_accum, coord, invalid);
        return;
    }

    // Bilinear tap previous lighting accumulation
    let base = vec2(s.x - 0.5, s.y - 0.5);
    let i0 = vec2<i32>(i32(floor(base.x)), i32(floor(base.y)));
    let f = vec2(fract(base.x), fract(base.y));
    var color = vec3(0.0);
    var hist = 0.0;
    var wsum = 0.0;
    var max_variance = 0.0;
    var spatial_second_moment = 0.0;
    for (var dy = 0; dy < 2; dy += 1) {
        for (var dx = 0; dx < 2; dx += 1) {
            let tc = vec2<i32>(
                clamp(i0.x + dx, 0, i32(w) - 1),
                clamp(i0.y + dy, 0, i32(h) - 1));
            let bw = (select(1.0 - f.x, f.x, dx == 1)) * (select(1.0 - f.y, f.y, dy == 1));
            let tap = textureLoad(prev_accum, tc, 0);
            let moments = prev_moments[u32(tc.y) * w + u32(tc.x)];
            if (tap.a >= 1.0 && moments.z >= 1.0 && bw > 1e-5) {
                color += tap.rgb * bw;
                hist += moments.z * bw;
                if (moments.z > 1.0) {
                    max_variance = max(max_variance, moments.y / (moments.z - 1.0));
                }
                spatial_second_moment += moments.x * moments.x * bw;
                wsum += bw;
            }
        }
    }
    if (wsum < 1e-4) {
        textureStore(out_accum, coord, invalid);
        return;
    }
    color /= wsum;
    hist = clamp(hist / wsum, 1.0, params.hist_cap);
    if (color.r != color.r || color.g != color.g || color.b != color.b) {
        textureStore(out_accum, coord, invalid);
        return;
    }
    textureStore(out_accum, coord, vec4(color, hist));
    let mean_luminance = dot(color, vec3(0.2126, 0.7152, 0.0722));
    let spatial_variance = max(spatial_second_moment / wsum - mean_luminance * mean_luminance, 0.0);
    out_moments[moment_index] = vec4(mean_luminance, (max_variance + spatial_variance) * (hist - 1.0), hist, 0.0);
}
