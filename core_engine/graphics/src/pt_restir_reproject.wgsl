// ============================================================
// AZRA — ReSTIR DI reservoir reprojection (Section I)
// Warps previous-frame reservoirs into the current camera so
// temporal reuse survives orbit (same guides as pt_reproject).
// Reservoir packing (rgba32f): y=light idx (-1 empty), W, M, w_sum.
// Nearest valid tap (no bilinear — light indices are discrete).
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
    hist_cap: f32,       // clamps reservoir M after warp
    depth_reject: f32,
    full_size: vec2<u32>,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var cur_normal_dist: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: PtReprojParams;
@group(0) @binding(2) var prev_normal_dist: texture_2d<f32>;
@group(0) @binding(3) var prev_restir: texture_2d<f32>;
@group(0) @binding(4) var out_restir: texture_storage_2d<rgba32float, write>;

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

fn world_to_prev_lighting(world_p: vec3<f32>, lw: f32, lh: f32, fw: f32, fh: f32) -> vec3<f32> {
    let aspect = fw / fh;
    let d = world_p - params.prev_cam_pos.xyz;
    let z = dot(d, normalize(params.prev_cam_forward.xyz));
    if (z <= 0.0) { return vec3(-1.0, -1.0, -1.0); }
    let fs = select(params.prev_cam_forward.w, 1.0, params.prev_cam_forward.w <= 0.0);
    let u = dot(d, params.prev_cam_right.xyz) / (z * aspect * fs);
    let v = dot(d, params.prev_cam_up.xyz) / (z * fs);
    let full_sx = (u * 0.5 + 0.5) * fw;
    let full_sy = (-v * 0.5 + 0.5) * fh;
    let sx = full_sx * (lw / fw);
    let sy = full_sy * (lh / fh);
    return vec3(sx, sy, z);
}

@compute @workgroup_size(8, 8, 1)
fn pt_restir_reproject(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = params.screen_size.x;
    let h = params.screen_size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    // Empty reservoir: y=-1
    let empty = vec4(-1.0, 0.0, 0.0, 0.0);

    // Ortho: skip (same as accum reproject — and the same post-2026-08-06 reason:
    // the G-buffer depth convention is fixed, but world_to_prev_lighting() is still
    // perspective-only. See pt_reproject.wgsl.)
    if (params.cur_cam_pos.w > 0.0 || params.prev_cam_pos.w > 0.0) {
        textureStore(out_restir, coord, empty);
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
        textureStore(out_restir, coord, empty);
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
        textureStore(out_restir, coord, empty);
        return;
    }

    let prev_light = vec2<i32>(i32(round(s.x - 0.5)), i32(round(s.y - 0.5)));
    let prev_light_c = vec2<i32>(
        clamp(prev_light.x, 0, i32(w) - 1),
        clamp(prev_light.y, 0, i32(h) - 1),
    );
    let prev_g = lighting_to_gbuf(u32(prev_light_c.x), u32(prev_light_c.y));
    let prev_nd = textureLoad(prev_normal_dist, prev_g, 0);
    if (prev_nd.w <= 0.0) {
        textureStore(out_restir, coord, empty);
        return;
    }
    let expected = length(world_p - params.prev_cam_pos.xyz);
    if (abs(prev_nd.w - expected) > params.depth_reject * max(expected, 1e-3)) {
        textureStore(out_restir, coord, empty);
        return;
    }
    if (dot(cur_n, prev_nd.xyz) < 0.5) {
        textureStore(out_restir, coord, empty);
        return;
    }

    let res = textureLoad(prev_restir, prev_light_c, 0);
    // Valid reservoir needs selected light + confidence
    if (res.r < 0.0 || res.b <= 0.0) {
        textureStore(out_restir, coord, empty);
        return;
    }
    // Ghost limiter: clamp M after warp (hist_cap reused from accum reproject)
    let m_cap = max(params.hist_cap, 1.0);
    let m = min(res.b, m_cap);
    textureStore(out_restir, coord, vec4(res.r, res.g, m, res.a));
}
