// ============================================================
// AZRA — RT Projected Shadows: temporal reprojection accumulation (RT_SHADOWS_DESIGN Stage 2)
// ============================================================
// Reprojects the previous frame's accumulated per-light visibility into the current frame by
// camera motion (the pt_reproject.wgsl gather-reproject scheme: unproject the current pixel with
// the CURRENT camera + G-buffer depth, project into the PREVIOUS camera, tap the history), with
// depth + normal disocclusion rejection. Valid history is blended with the current spatially-
// denoised visibility via a fixed-alpha EMA (a per-pixel history-count channel is impossible —
// all four RGBA channels carry lights — so a fixed feedback weight replaces pt_reproject's
// count-weighted blend; documented in RT_SHADOWS_DESIGN Stage 2).
//
// Visibility is VIEW-INDEPENDENT (occlusion of a light at a surface point does not depend on the
// camera), so reprojected samples remain valid across camera motion — the reason temporal
// accumulation converges cleanly and why brief history staleness on interaction-resume does not
// pop (it is the same world point's occlusion). frame_valid == 0 (first frame / resize / resume)
// falls back to the current frame only — no ghosting from uninitialised or stale history.
//
// Camera packing matches PtReprojParams / GpuUniforms.camera_ray: pos.w = ortho half-width
// (0 = perspective), forward.w = fov_scale. Self-contained (no SDF header).
// ============================================================

struct RtTemporalParams {
    prev_cam_pos: vec4<f32>,      // xyz = prev camera position, w = prev ortho_hw
    prev_cam_forward: vec4<f32>,  // xyz, w = prev fov_scale
    prev_cam_right: vec4<f32>,    // xyz
    prev_cam_up: vec4<f32>,       // xyz
    cur_cam_pos: vec4<f32>,       // xyz = current camera position, w = cur ortho_hw
    cur_cam_forward: vec4<f32>,   // xyz, w = cur fov_scale
    cur_cam_right: vec4<f32>,     // xyz
    cur_cam_up: vec4<f32>,        // xyz
    screen_size: vec2<u32>,
    blend_alpha: f32,             // EMA feedback weight for valid reprojected history (e.g. 0.9)
    depth_reject: f32,            // relative depth tolerance for disocclusion (e.g. 0.05)
    frame_valid: u32,             // 0 = no valid history → current only
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var cur_normal_dist  : texture_2d<f32>; // gbuf_normal THIS frame (normal.xyz + dist .w)
@group(0) @binding(1) var<uniform> params  : RtTemporalParams;
@group(0) @binding(2) var prev_normal_dist : texture_2d<f32>; // gbuf_normal PREVIOUS frame
@group(0) @binding(3) var prev_vis         : texture_2d<f32>; // accumulated visibility PREVIOUS frame (history)
@group(0) @binding(4) var cur_vis          : texture_2d<f32>; // spatially-denoised visibility THIS frame
@group(0) @binding(5) var out_vis          : texture_storage_2d<rgba16float, write>;

// Unproject a pixel to world space with the CURRENT camera (mirrors camera_ray in sdf_shared.wgsl;
// dist = length along the normalized ray = gbuffer world_dist).
fn cur_pixel_to_world(px: vec2<f32>, dist: f32, w: f32, h: f32) -> vec3<f32> {
    let uv_x = (px.x / w) * 2.0 - 1.0;
    let uv_y = -((px.y / h) * 2.0 - 1.0);
    let aspect = w / h;
    let fs = select(params.cur_cam_forward.w, 1.0, params.cur_cam_forward.w <= 0.0);
    let rd = normalize(params.cur_cam_forward.xyz
        + params.cur_cam_right.xyz * (uv_x * aspect * fs)
        + params.cur_cam_up.xyz * (uv_y * fs));
    return params.cur_cam_pos.xyz + rd * dist;
}

// Project a world point into the PREVIOUS camera. Returns (px, py, z-forward).
fn world_to_prev_screen(world_p: vec3<f32>, w: f32, h: f32) -> vec3<f32> {
    let aspect = w / h;
    let d = world_p - params.prev_cam_pos.xyz;
    let z = dot(d, normalize(params.prev_cam_forward.xyz));
    if (z <= 0.0) { return vec3<f32>(-1.0, -1.0, -1.0); }
    let fs = select(params.prev_cam_forward.w, 1.0, params.prev_cam_forward.w <= 0.0);
    let u = dot(d, params.prev_cam_right.xyz) / (z * aspect * fs);
    let v = dot(d, params.prev_cam_up.xyz) / (z * fs);
    let sx = (u * 0.5 + 0.5) * w;
    let sy = (-v * 0.5 + 0.5) * h;
    return vec3<f32>(sx, sy, z);
}

@compute @workgroup_size(8, 8, 1)
fn rt_shadow_temporal(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = params.screen_size.x;
    let h = params.screen_size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    let cur = textureLoad(cur_vis, coord, 0);
    // No valid history, ortho, or background: pass the current denoised visibility through.
    //
    // The ortho guard's ORIGINAL reason ("gbuffer .w is eye-distance, not along-ray") was
    // fixed 2026-08-06 — gbuf_depth() in render/src/gbuffer.wgsl now writes the ray
    // parameter in both projections. The guard STAYS for a different, still-live reason:
    // world_to_prev_screen() below is perspective-only (it divides by forward-z and
    // fov_scale, with no ortho branch), so reprojection would land on the wrong pixels.
    // Lifting this guard requires giving that helper an ortho branch FIRST.
    let cur_nd = textureLoad(cur_normal_dist, coord, 0);
    let cur_dist = cur_nd.w;
    if (params.frame_valid == 0u || params.cur_cam_pos.w > 0.0 || params.prev_cam_pos.w > 0.0 || cur_dist <= 0.0) {
        textureStore(out_vis, coord, cur);
        return;
    }
    let cur_n = cur_nd.xyz;

    let fw = f32(w);
    let fh = f32(h);
    let world_p = cur_pixel_to_world(vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5), cur_dist, fw, fh);
    let s = world_to_prev_screen(world_p, fw, fh);
    if (s.z <= 0.0 || s.x < 0.5 || s.y < 0.5 || s.x >= fw - 0.5 || s.y >= fh - 0.5) {
        textureStore(out_vis, coord, cur); // off-screen last frame → newly revealed
        return;
    }

    // Disocclusion rejection: the surface we see now must be the surface the previous frame saw
    // at the reprojected pixel (depth + normal).
    let pc = vec2<i32>(i32(s.x), i32(s.y));
    let prev_nd = textureLoad(prev_normal_dist, pc, 0);
    if (prev_nd.w <= 0.0) {
        textureStore(out_vis, coord, cur); // was background → newly revealed
        return;
    }
    let expected = length(world_p - params.prev_cam_pos.xyz);
    if (abs(prev_nd.w - expected) > params.depth_reject * max(expected, 1e-3)) {
        textureStore(out_vis, coord, cur); // depth mismatch → different surface
        return;
    }
    if (dot(cur_n, prev_nd.xyz) < 0.5) {
        textureStore(out_vis, coord, cur); // normal mismatch → silhouette edge
        return;
    }

    // Bilinear tap of the previous accumulation at the reprojected position.
    let base = vec2<f32>(s.x - 0.5, s.y - 0.5);
    let i0 = vec2<i32>(i32(floor(base.x)), i32(floor(base.y)));
    let f = vec2<f32>(fract(base.x), fract(base.y));
    var hist = vec4<f32>(0.0);
    for (var dy = 0; dy < 2; dy += 1) {
        for (var dx = 0; dx < 2; dx += 1) {
            let tc = vec2<i32>(
                clamp(i0.x + dx, 0, i32(w) - 1),
                clamp(i0.y + dy, 0, i32(h) - 1));
            let bw = (select(1.0 - f.x, f.x, dx == 1)) * (select(1.0 - f.y, f.y, dy == 1));
            hist += textureLoad(prev_vis, tc, 0) * bw;
        }
    }
    // NaN guard — never let a poisoned history pixel survive.
    if (hist.x != hist.x || hist.y != hist.y || hist.z != hist.z || hist.w != hist.w) {
        textureStore(out_vis, coord, cur);
        return;
    }
    // Fixed-alpha EMA: blend_alpha weight on reprojected history, (1-alpha) on the fresh sample.
    let blended = mix(cur, hist, params.blend_alpha);
    textureStore(out_vis, coord, clamp(blended, vec4<f32>(0.0), vec4<f32>(1.0)));
}
