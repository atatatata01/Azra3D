// ============================================================
// AZRA — FXAA Compute Shader (post-tonemap)
// Luma-based edge detection + directional blend.
// Runs after tonemap, before upsample. Reads LDR input,
// writes anti-aliased result to storage texture.
// ============================================================

@group(0) @binding(0) var fxaa_input: texture_2d<f32>;
@group(0) @binding(1) var fxaa_output: texture_storage_2d<rgba16float, write>;

// sRGB luminance approximation (fast, no gamma conversion needed for edge detection)
fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.299, 0.587, 0.114));
}

@compute @workgroup_size(8, 8, 1)
fn pass_fxaa(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(fxaa_input);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    // Sample center and 4-neighbors
    let c  = textureLoad(fxaa_input, coord, 0).rgb;
    let n  = textureLoad(fxaa_input, coord + vec2(0, -1), 0).rgb;
    let s  = textureLoad(fxaa_input, coord + vec2(0,  1), 0).rgb;
    let w  = textureLoad(fxaa_input, coord + vec2(-1, 0), 0).rgb;
    let e  = textureLoad(fxaa_input, coord + vec2( 1, 0), 0).rgb;

    let lc = luma(c);
    let ln = luma(n);
    let ls = luma(s);
    let lw = luma(w);
    let le = luma(e);

    // Edge detection: max contrast among neighbors
    let luma_min = min(lc, min(min(ln, ls), min(lw, le)));
    let luma_max = max(lc, max(max(ln, ls), max(lw, le)));
    let luma_range = luma_max - luma_min;

    // Early exit: no edge — pass through unchanged
    // Tuned for SDF: high thresholds → only silhouette edges, not flat surfaces
    let edge_threshold = 0.15;    // relative contrast threshold (was 0.0625)
    let edge_threshold_min = 0.15; // absolute minimum for dark areas — raised to reduce false triggers
    if (luma_range < max(edge_threshold_min, luma_max * edge_threshold)) {
        textureStore(fxaa_output, coord, vec4(c, 1.0));
        return;
    }

    // Sample diagonal neighbors for sub-pixel detail
    let nw = textureLoad(fxaa_input, coord + vec2(-1, -1), 0).rgb;
    let ne = textureLoad(fxaa_input, coord + vec2( 1, -1), 0).rgb;
    let sw = textureLoad(fxaa_input, coord + vec2(-1,  1), 0).rgb;
    let se = textureLoad(fxaa_input, coord + vec2( 1,  1), 0).rgb;
    let lnw = luma(nw);
    let lne = luma(ne);
    let lsw = luma(sw);
    let lse = luma(se);

    // Determine edge direction (horizontal vs vertical)
    let edge_h = abs(-2.0 * lw + lnw + lsw) + abs(-2.0 * lc + ln + ls) * 2.0 + abs(-2.0 * le + lne + lse);
    let edge_v = abs(-2.0 * ln + lnw + lne) + abs(-2.0 * lc + lw + le) * 2.0 + abs(-2.0 * ls + lsw + lse);
    let is_horizontal = edge_h >= edge_v;

    // Step direction perpendicular to edge
    let step_len = select(1.0 / f32(dim.x), 1.0 / f32(dim.y), is_horizontal);
    let luma_pos = select(le, ls, is_horizontal);
    let luma_neg = select(lw, ln, is_horizontal);
    let grad_pos = abs(luma_pos - lc);
    let grad_neg = abs(luma_neg - lc);

    // Choose the steeper gradient direction
    let step_dir = select(-1.0, 1.0, grad_pos >= grad_neg);
    let luma_local_avg = 0.5 * select(luma_neg, luma_pos, grad_pos >= grad_neg) + 0.5 * lc;

    // Sub-pixel aliasing test
    let sub_pixel_offset = clamp(abs((ln + ls + lw + le) * 0.25 - lc) / luma_range, 0.0, 1.0);
    let sub_pixel_blend = (-2.0 * sub_pixel_offset + 3.0) * sub_pixel_offset * sub_pixel_offset;
    let sub_pixel_factor = sub_pixel_blend * sub_pixel_blend * 0.15; // reduced from 0.5 — minimal blur on flat surfaces

    // Edge walk: search along the edge for endpoints
    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    var uv_pos = uv;
    var uv_neg = uv;
    if (is_horizontal) {
        uv_pos.y += step_dir * 0.5;
        uv_neg.y += step_dir * 0.5;
    } else {
        uv_pos.x += step_dir * 0.5;
        uv_neg.x += step_dir * 0.5;
    }

    let search_step = select(vec2(0.0, 1.0), vec2(1.0, 0.0), is_horizontal);
    var dist_pos = 0.0;
    var dist_neg = 0.0;
    var end_pos = false;
    var end_neg = false;
    let search_threshold = luma_range * 0.25;

    // Search positive direction (max 8 steps)
    for (var i = 0; i < 8; i++) {
        uv_pos += search_step;
        let coord_p = vec2<i32>(i32(uv_pos.x), i32(uv_pos.y));
        if (coord_p.x < 0 || coord_p.x >= i32(dim.x) || coord_p.y < 0 || coord_p.y >= i32(dim.y)) { end_pos = true; break; }
        let l = luma(textureLoad(fxaa_input, coord_p, 0).rgb) - luma_local_avg;
        if (abs(l) >= search_threshold) { end_pos = true; break; }
        dist_pos += 1.0;
    }
    // Search negative direction (max 8 steps)
    for (var i = 0; i < 8; i++) {
        uv_neg -= search_step;
        let coord_n = vec2<i32>(i32(uv_neg.x), i32(uv_neg.y));
        if (coord_n.x < 0 || coord_n.x >= i32(dim.x) || coord_n.y < 0 || coord_n.y >= i32(dim.y)) { end_neg = true; break; }
        let l = luma(textureLoad(fxaa_input, coord_n, 0).rgb) - luma_local_avg;
        if (abs(l) >= search_threshold) { end_neg = true; break; }
        dist_neg += 1.0;
    }

    // Compute blend factor from edge endpoint distances
    let span = dist_pos + dist_neg;
    let closer = min(dist_pos, dist_neg);
    let edge_blend = 0.5 - closer / max(span, 0.001);

    let pixel_blend = max(edge_blend, sub_pixel_factor);

    // Sample the blended pixel: offset perpendicular to edge
    let offset_x = select(0, i32(step_dir * pixel_blend + 0.5 * sign(step_dir)), !is_horizontal);
    let offset_y = select(i32(step_dir * pixel_blend + 0.5 * sign(step_dir)), 0, !is_horizontal);
    let sample_coord = clamp(coord + vec2(offset_x, offset_y), vec2(0), vec2<i32>(dim) - vec2(1));
    let blended = textureLoad(fxaa_input, sample_coord, 0).rgb;
    let result = mix(c, blended, clamp(pixel_blend, 0.0, 0.5));

    textureStore(fxaa_output, coord, vec4(result, 1.0));
}
