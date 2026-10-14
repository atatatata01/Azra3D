// ============================================================
// AZRA — Temporal Reprojection (Level 7)
// Produces a validity mask: 1 = reuse previous frame, 0 = re-march.
// Dispatched BEFORE the march pass each frame.
// ============================================================

struct TemporalParams {
    prev_cam_pos: vec4<f32>,       // xyz = prev camera position
    prev_cam_forward: vec4<f32>,   // xyz = prev cam forward, w = prev fov_scale
    prev_cam_right: vec4<f32>,     // xyz = prev cam right
    prev_cam_up: vec4<f32>,        // xyz = prev cam up, w = prev ortho_hw
    cur_cam_pos: vec4<f32>,        // xyz = current camera position
    cur_cam_forward: vec4<f32>,    // xyz = current cam forward, w = current fov_scale
    cur_cam_right: vec4<f32>,      // xyz = current cam right
    cur_cam_up: vec4<f32>,         // xyz = current cam up, w = current ortho_hw
    screen_size: vec2<u32>,
    dirty_rect_min: vec2<u32>,     // screen-space rect of dirty region (always invalidated)
    dirty_rect_max: vec2<u32>,
    camera_moved: u32,             // 1 if camera moved, 0 if static
    frame_valid: u32,              // 0 on first frame or after resize (no valid history)
}

@group(0) @binding(0) var prev_normal_dist: texture_2d<f32>;  // previous frame gbuf_normal (normal.xyz + dist in .w)
@group(0) @binding(1) var<uniform> params: TemporalParams;
@group(0) @binding(2) var validity_mask: texture_storage_2d<r32uint, write>;

// Reconstruct world position from screen coords using camera vectors (inverse camera ray)
fn screen_to_world(px: vec2<f32>, dist: f32, cam_pos: vec3<f32>, cam_fwd: vec3<f32>, cam_right: vec3<f32>, cam_up: vec3<f32>, fov_scale: f32, ortho_hw: f32, w: f32, h: f32) -> vec3<f32> {
    let uv_x = (px.x / w) * 2.0 - 1.0;
    let uv_y = -((px.y / h) * 2.0 - 1.0);
    let aspect = w / h;
    if (ortho_hw > 0.0) {
        let origin = cam_pos + cam_right * (uv_x * ortho_hw * aspect) + cam_up * (uv_y * ortho_hw);
        let rd = normalize(cam_fwd);
        return origin + rd * dist;
    } else {
        let fs = select(fov_scale, 1.0, fov_scale <= 0.0);
        let rd = normalize(cam_fwd + cam_right * (uv_x * aspect * fs) + cam_up * (uv_y * fs));
        return cam_pos + rd * dist;
    }
}

// Project world position to screen coords using camera vectors
fn world_to_screen(world_p: vec3<f32>, cam_pos: vec3<f32>, cam_fwd: vec3<f32>, cam_right: vec3<f32>, cam_up: vec3<f32>, fov_scale: f32, ortho_hw: f32, w: f32, h: f32) -> vec3<f32> {
    let aspect = w / h;
    if (ortho_hw > 0.0) {
        let d = world_p - cam_pos;
        let u = dot(d, cam_right) / (ortho_hw * aspect);
        let v = dot(d, cam_up) / ortho_hw;
        let dist = dot(d, normalize(cam_fwd));
        let sx = (u * 0.5 + 0.5) * w;
        let sy = (-v * 0.5 + 0.5) * h;
        return vec3(sx, sy, dist);
    } else {
        let d = world_p - cam_pos;
        let z = dot(d, normalize(cam_fwd));
        if (z <= 0.0) { return vec3(-1.0, -1.0, -1.0); }
        let fs = select(fov_scale, 1.0, fov_scale <= 0.0);
        let u = dot(d, cam_right) / (z * aspect * fs);
        let v = dot(d, cam_up) / (z * fs);
        let sx = (u * 0.5 + 0.5) * w;
        let sy = (-v * 0.5 + 0.5) * h;
        return vec3(sx, sy, z);
    }
}

@compute @workgroup_size(8, 8, 1)
fn temporal_reproject(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = params.screen_size.x;
    let h = params.screen_size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    // Fast path: camera moved or no valid history — invalidate everything
    if (params.camera_moved != 0u || params.frame_valid == 0u) {
        textureStore(validity_mask, coord, vec4<u32>(0u, 0u, 0u, 0u));
        return;
    }

    // Check if this pixel falls within the dirty rect (always re-march)
    if (gid.x >= params.dirty_rect_min.x && gid.x < params.dirty_rect_max.x &&
        gid.y >= params.dirty_rect_min.y && gid.y < params.dirty_rect_max.y) {
        textureStore(validity_mask, coord, vec4<u32>(0u, 0u, 0u, 0u));
        return;
    }

    // Read previous frame's normal+dist at the SAME screen position
    // (camera is static, so same pixel = same ray)
    let prev = textureLoad(prev_normal_dist, coord, 0);
    let prev_normal = prev.xyz;
    let prev_dist = prev.w;

    // Background pixels (dist == 0) are always valid when camera is static
    if (prev_dist <= 0.0) {
        textureStore(validity_mask, coord, vec4<u32>(1u, 0u, 0u, 0u));
        return;
    }

    // Geometry pixel with valid normal — mark as reusable
    // (camera is static and pixel is outside dirty rect, so the SDF result hasn't changed)
    textureStore(validity_mask, coord, vec4<u32>(1u, 0u, 0u, 0u));
}
