// ============================================================
// AZRA — Screen-Space Ambient Occlusion (SSAO)
// ============================================================
//
// The real ambient-occlusion source for raster pixels (SHADOWSTALE follow-up:
// the old "AO" was the sun's shadow map leaking into the ambient term — removed
// 2026-07-17d, leaving raster pixels with NO occlusion source. This pass is the
// proper primitive that entry named).
//
// Pipeline shape (3 entry points, one module, half resolution):
//   ssao_main   — hemisphere occlusion estimate from the FULL-res G-buffer
//                 (gbuf_normal: xyz = world normal, w = world distance from the
//                 camera position — the raster + SDF convention), written to a
//                 HALF-res rgba16float target (r = raw AO, g = center depth).
//   ssao_blur_h — depth-aware separable blur, horizontal.
//   ssao_blur_v — depth-aware separable blur, vertical + strength apply:
//                 out.r = mix(1.0, ao, strength). Depth rides in .g for the
//                 bilateral upsample in deferred_entry.wgsl (ssao_factor()).
//
// Sampling: 12 cosine-distributed hemisphere taps around the G-buffer normal,
// generated in-shader from a golden-angle spiral, rotated per pixel by
// INTERLEAVED GRADIENT NOISE (deterministic, stable per pixel — no per-frame
// jitter, no random()). Radius is in WORLD units; taps are placed in world
// space and reprojected to the screen, so the screen-space kernel scales with
// distance automatically (no magic pixel radius). A range check fades
// occluders further than the radius (no halo bleeding).
//
// Consumed by: deferred_entry.wgsl @binding(38) — ambient/IBL multiplier for
// raster pixels in PBR + Lit ONLY. Direct sun light and Solid mode never see
// this texture (AO is sky occlusion; Solid is pure form shading by decision).

struct SsaoParams {
    cam_pos: vec4<f32>,      // xyz = eye position, w = ortho half-width (0 = perspective)
    cam_forward: vec4<f32>,  // xyz = forward, w = fov_scale (tan(fov_y / 2))
    cam_right: vec4<f32>,    // xyz = right
    cam_up: vec4<f32>,       // xyz = up
    full_size: vec2<u32>,    // G-buffer resolution
    half_size: vec2<u32>,    // AO target resolution
    radius: f32,             // hemisphere radius in WORLD units
    strength: f32,           // 0..1 — applied in *_blur_v as mix(1, ao, strength)
    bias: f32,               // world-space occluder bias (self-occlusion guard)
    frame_jitter: f32,       // progressive-refine IGN frame term (0 = static pattern)
    // ── GTAO tier fields (ignored by the SSAO entry points) ──
    slices: u32,             // GTAO azimuthal slice count (directional sampling)
    steps: u32,              // GTAO horizon-march steps per side
    thickness: f32,          // GTAO horizon falloff distance beyond radius (world units)
    _pad2: f32,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: SsaoParams;

const SSAO_TAPS: u32 = 12u;
const GOLDEN_ANGLE: f32 = 2.3999632;
const PI: f32 = 3.14159265;
// Occlusion contrast (raw estimate → perceptual). Same default as the app's
// long-standing volume_ao_intensity (1.5) — the point-sampled estimator
// underestimates contact occlusion for near walls (taps sample points, not
// rays), and this recovers the visible contact term without darkening open
// surfaces (0 occlusion stays 0).
const SSAO_INTENSITY: f32 = 1.5;

// Interleaved gradient noise (Jimenez 2014) — deterministic per-pixel PHASE,
// purely spatial. The progressive-refine frame term is applied at the CALL
// SITES as `fract(frame · R2-constant + phase)` — NOT as a diagonal shift of
// the IGN field. The old shift (`px + frame·5.588238`) walked every sampling
// dimension along the SAME correlated 1D path, so per-pixel means converged to
// structured streaks (worst on steep AO gradients — the owner's "occlusion
// feels weird" crease streaks, same defect class as the RT penumbra dots).
// Decorrelated per-dimension advances + independent per-pixel phases make every
// pixel integrate the same full domain. Jitter without an accumulator would
// boil, which is why frame advance is gated on the accumulate bit CPU-side.
fn ign(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * px.x + 0.00583715 * px.y));
}

// Reconstruct the camera ray for a FULL-res pixel center. Matches the packing of
// camera_ray() in sdf_shared.wgsl and CameraUniforms/gbuffer.wgsl: cam_pos.w > 0
// selects orthographic (parallel rays from a shifted origin).
// Returns array [origin, direction].
fn ssao_camera_ray(px: vec2<f32>) -> array<vec3<f32>, 2> {
    let w = f32(params.full_size.x);
    let h = f32(params.full_size.y);
    let aspect = w / h;
    let uv_x = ((px.x + 0.5) / w) * 2.0 - 1.0;
    let uv_y = -(((px.y + 0.5) / h) * 2.0 - 1.0);
    let ortho_hw = params.cam_pos.w;
    let fs = select(params.cam_forward.w, 1.0, params.cam_forward.w <= 0.0);
    if (ortho_hw > 0.0) {
        let origin = params.cam_pos.xyz
            + params.cam_right.xyz * (uv_x * ortho_hw * aspect)
            + params.cam_up.xyz * (uv_y * ortho_hw);
        return array<vec3<f32>, 2>(origin, normalize(params.cam_forward.xyz));
    }
    let rd = normalize(params.cam_forward.xyz
        + params.cam_right.xyz * (uv_x * aspect * fs)
        + params.cam_up.xyz * (uv_y * fs));
    return array<vec3<f32>, 2>(params.cam_pos.xyz, rd);
}

// Project a world position to FULL-res pixel coordinates.
// Returns vec3(px, py, valid) — valid = 0.0 when behind the camera.
fn ssao_project(world: vec3<f32>) -> vec3<f32> {
    let w = f32(params.full_size.x);
    let h = f32(params.full_size.y);
    let aspect = w / h;
    let d = world - params.cam_pos.xyz;
    let ortho_hw = params.cam_pos.w;
    var uv_x: f32;
    var uv_y: f32;
    if (ortho_hw > 0.0) {
        uv_x = dot(d, params.cam_right.xyz) / (ortho_hw * aspect);
        uv_y = dot(d, params.cam_up.xyz) / ortho_hw;
    } else {
        let fs = select(params.cam_forward.w, 1.0, params.cam_forward.w <= 0.0);
        let fz = dot(d, params.cam_forward.xyz);
        if (fz <= 1e-4) { return vec3(0.0, 0.0, 0.0); }
        uv_x = dot(d, params.cam_right.xyz) / (fz * aspect * fs);
        uv_y = dot(d, params.cam_up.xyz) / (fz * fs);
    }
    let px = (uv_x + 1.0) * 0.5 * w - 0.5;
    let py = (1.0 - uv_y) * 0.5 * h - 0.5;
    return vec3(px, py, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn ssao_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    // Representative full-res pixel for this half-res texel (top-left of the 2x2
    // quad — the upsample in deferred maps full pixel p to half coord p/2, so
    // half texel h is centered on full pixel 2h).
    let fcoord = hcoord * 2;
    let g = textureLoad(src_tex, fcoord, 0);
    let n = g.xyz;
    let t = g.w;
    // Background / no-geometry pixels: fully open.
    if (t <= 1e-4 || dot(n, n) < 0.25) {
        textureStore(dst_tex, hcoord, vec4(1.0, 0.0, 0.0, 0.0));
        return;
    }
    let nrm = normalize(n);
    let ray = ssao_camera_ray(vec2<f32>(f32(fcoord.x), f32(fcoord.y)));
    let p = ray[0] + ray[1] * t;

    // Tangent basis around the surface normal.
    let helper = select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(nrm.y) > 0.9);
    let tangent = normalize(cross(helper, nrm));
    let bitangent = cross(nrm, tangent);

    // Per-pixel deterministic spiral rotation (stable — keyed on the half-res
    // pixel). While accumulating, the rotation advances a golden-ratio sequence
    // from the per-pixel phase: rotation is SSAO's only stochastic dimension, so
    // a well-distributed 1D sequence per pixel converges every pixel to the same
    // hemisphere integral (frame_jitter == 0 ⇒ bit-identical static pattern).
    let ao_phase = ign(vec2<f32>(f32(gid.x), f32(gid.y)));
    let rot = select(
        ao_phase,
        fract(params.frame_jitter * 0.61803398875 + ao_phase),
        params.frame_jitter > 0.0,
    ) * 2.0 * PI;

    let full_max = vec2<i32>(i32(params.full_size.x) - 1, i32(params.full_size.y) - 1);
    var occlusion = 0.0;
    for (var i = 0u; i < SSAO_TAPS; i++) {
        let fi = (f32(i) + 0.5) / f32(SSAO_TAPS);
        let ang = f32(i) * GOLDEN_ANGLE + rot;
        // Cosine-distributed hemisphere direction (z = up along the normal).
        let sin_t = sqrt(fi);
        let cos_t = sqrt(1.0 - fi);
        let dir_local = vec3(cos(ang) * sin_t, sin(ang) * sin_t, cos_t);
        // Cluster tap distances toward the center (finer near-field occlusion).
        // Linear ramp: a quadratic ramp left most taps too short to reach past
        // nearby occluders — contact corners read 0.85 instead of ≤0.75
        // (measured by the readback harness).
        let scale = mix(0.2, 1.0, fi);
        let dir = tangent * dir_local.x + bitangent * dir_local.y + nrm * dir_local.z;
        let sample_pos = p + dir * (params.radius * scale);

        let proj = ssao_project(sample_pos);
        if (proj.z < 0.5) { continue; }
        let spx = vec2<i32>(i32(round(proj.x)), i32(round(proj.y)));
        if (spx.x < 0 || spx.y < 0 || spx.x > full_max.x || spx.y > full_max.y) { continue; }

        let stored_t = textureLoad(src_tex, spx, 0).w;
        if (stored_t <= 1e-4) { continue; } // sky — no occluder
        // Same metric as the stored G-buffer depth: the ray parameter along
        // ssao_camera_ray(), which is what reconstructed `p` above already uses.
        // Perspective = |rel|; ortho = forward distance (cam_pos.w > 0). Comparing
        // a Euclidean distance against an ortho depth compares two different
        // quantities and mis-occludes every off-axis pixel — keep in lockstep with
        // gbuf_depth() in render/src/gbuffer.wgsl.
        let s_rel = sample_pos - params.cam_pos.xyz;
        let sample_t = select(length(s_rel), dot(s_rel, params.cam_forward.xyz), params.cam_pos.w > 0.0);
        // Adaptive bias: fixed world-space floor + a distance-proportional term
        // (f16 depth precision falls off with range — prevents far-field acne).
        let bias_t = params.bias + t * 0.002;
        if (stored_t < sample_t - bias_t) {
            // Range check: occluders far in front of the shell fade out
            // smoothly — no halos from foreground silhouettes.
            let range = smoothstep(0.0, 1.0, params.radius / max(abs(t - stored_t), 1e-4));
            occlusion += range;
        }
    }
    let ao = clamp(1.0 - occlusion * SSAO_INTENSITY / f32(SSAO_TAPS), 0.0, 1.0);
    textureStore(dst_tex, hcoord, vec4(ao, t, 0.0, 0.0));
}

// Depth-aware separable blur. σ_spatial = 2.5 texels (radius 4), σ_depth scales
// with the SSAO world radius so edges between depth-disjoint surfaces stay hard.
fn ssao_blur(hcoord: vec2<i32>, dir: vec2<i32>) -> vec2<f32> {
    let center = textureLoad(src_tex, hcoord, 0);
    let ao0 = center.r;
    let d0 = center.g;
    if (d0 <= 1e-4) { return vec2(ao0, d0); } // background — passthrough
    let hmax = vec2<i32>(i32(params.half_size.x) - 1, i32(params.half_size.y) - 1);
    let sigma_s = 2.5;
    let sigma_d = max(params.radius * 0.5, 1e-3);
    var sum = 0.0;
    var wsum = 0.0;
    for (var o = -4; o <= 4; o++) {
        let c = clamp(hcoord + dir * o, vec2<i32>(0, 0), hmax);
        let s = textureLoad(src_tex, c, 0);
        if (s.g <= 1e-4) { continue; }
        let dd = s.g - d0;
        let w = exp(-f32(o * o) / (2.0 * sigma_s * sigma_s))
              * exp(-(dd * dd) / (2.0 * sigma_d * sigma_d));
        sum += s.r * w;
        wsum += w;
    }
    let blurred = select(ao0, sum / wsum, wsum > 1e-4);
    return vec2(blurred, d0);
}

@compute @workgroup_size(8, 8, 1)
fn ssao_blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = ssao_blur(hcoord, vec2<i32>(1, 0));
    textureStore(dst_tex, hcoord, vec4(r.x, r.y, 0.0, 0.0));
}

@compute @workgroup_size(8, 8, 1)
fn ssao_blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = ssao_blur(hcoord, vec2<i32>(0, 1));
    // Strength applied HERE (affine in ao — commutes with the deferred-side
    // bilinear upsample). Toggle-off never reaches this pass: the resources are
    // white-filled instead and the dispatches are skipped entirely.
    let strengthened = mix(1.0, r.x, params.strength);
    textureStore(dst_tex, hcoord, vec4(strengthened, r.y, 0.0, 0.0));
}

// ============================================================
// GTAO — Ground Truth Ambient Occlusion + bent normals
// ============================================================
//
// Horizon-based AO (Jimenez et al. 2016, "Practical Realtime Strategies for
// Accurate Indirect Occlusion"). For each pixel we sweep N azimuthal slices; in
// each slice plane (spanned by the view vector V and an in-plane tangent T) we
// march the depth buffer to both horizons, integrate the cosine-weighted
// visibility arc against the PROJECTED geometric normal, and accumulate the
// bent normal (the average unoccluded direction). Output layout mirrors the
// SSAO tier so it feeds deferred_entry.wgsl @binding(38) with ZERO extra
// plumbing, but the previously-unused b,a channels now carry the octahedral
// bent normal:
//   r = AO factor (visibility, mix(1,ao,strength) applied in gtao_blur_v),
//   g = center depth (bilateral upsample key — same as SSAO),
//   b,a = octahedral-encoded bent normal (b=a=0 ⇒ "no bent normal" ⇒ deferred
//         falls back to the geometric normal, exactly like the SSAO/off path).
// GTAO consumes ONLY gbuf_normal (xyz=world normal, w=world distance) — the same
// single input as SSAO, so it reuses the identical bind groups.

// Sign(x) that never returns 0 — octahedral wrap needs a definite sign.
fn sign_nz(v: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(select(-1.0, 1.0, v.x >= 0.0), select(-1.0, 1.0, v.y >= 0.0));
}

// Octahedral unit-vector encode → [-1,1]^2 (Cigolle et al. 2014).
fn oct_encode(n_in: vec3<f32>) -> vec2<f32> {
    let n = n_in / (abs(n_in.x) + abs(n_in.y) + abs(n_in.z) + 1e-8);
    var e = n.xy;
    if (n.z < 0.0) {
        e = (vec2<f32>(1.0) - abs(vec2<f32>(n.y, n.x))) * sign_nz(n.xy);
    }
    return e;
}

@compute @workgroup_size(8, 8, 1)
fn gtao_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    let fcoord = hcoord * 2;
    let g = textureLoad(src_tex, fcoord, 0);
    let n = g.xyz;
    let t = g.w;
    // Background / no-geometry: fully open, no bent normal (b=a=0).
    if (t <= 1e-4 || dot(n, n) < 0.25) {
        textureStore(dst_tex, hcoord, vec4(1.0, 0.0, 0.0, 0.0));
        return;
    }
    let nrm = normalize(n);
    let ray = ssao_camera_ray(vec2<f32>(f32(fcoord.x), f32(fcoord.y)));
    let p = ray[0] + ray[1] * t;
    let v_dir = -ray[1]; // surface → camera (unit; valid for perspective + ortho)

    let slices = max(params.slices, 1u);
    let steps = max(params.steps, 1u);
    // Slice azimuth + step offset. GTAO samples TWO stochastic dimensions, so
    // while accumulating they advance DECORRELATED R2 fractions from INDEPENDENT
    // per-pixel phases (the RT-dots lesson: driving two dimensions off one
    // correlated sequence collapses the sample set to a 1D curve — pixels then
    // converge to different values = permanent crease streaks).
    let px_f = vec2<f32>(f32(gid.x), f32(gid.y));
    let phase_a = ign(px_f);
    let phase_s = ign(px_f + vec2<f32>(0.5, 0.0));
    var rot: f32;
    var jitter: f32;
    if (params.frame_jitter > 0.0) {
        rot = fract(params.frame_jitter * 0.7548777 + phase_a) * PI;
        jitter = fract(params.frame_jitter * 0.5698403 + phase_s);
    } else {
        rot = phase_a * PI;                 // bit-identical static pattern
        jitter = fract(phase_s + 0.5);
    }
    let full_max = vec2<i32>(i32(params.full_size.x) - 1, i32(params.full_size.y) - 1);
    let fade = max(params.thickness, 1e-3);

    var occlusion = 0.0;
    var bent_accum = vec3<f32>(0.0);
    var wsum = 0.0;

    for (var s = 0u; s < slices; s++) {
        let phi = rot + f32(s) * (PI / f32(slices));
        // Screen direction (y-down) → world direction, then the in-plane tangent
        // T ⟂ V. cam_up maps to screen -y, so sin uses -cam_up.
        let dir_world = params.cam_right.xyz * cos(phi) - params.cam_up.xyz * sin(phi);
        var tang = dir_world - v_dir * dot(dir_world, v_dir);
        let tlen = length(tang);
        if (tlen < 1e-5) { continue; }
        tang = tang / tlen;

        // Projected geometric normal onto the slice plane.
        let plane_n = normalize(cross(v_dir, tang));
        let proj_n = nrm - plane_n * dot(nrm, plane_n);
        let proj_len = length(proj_n);
        if (proj_len < 1e-4) { continue; }
        let proj_nn = proj_n / proj_len;
        let cos_nq = clamp(dot(proj_nn, v_dir), -1.0, 1.0);
        let n_angle = acos(cos_nq) * select(-1.0, 1.0, dot(proj_nn, tang) >= 0.0);
        let sn = sin(n_angle);

        // March both horizons: cos of the angle from V to the highest occluder.
        var cos_h_pos = -1.0; // +tang side (h2)
        var cos_h_neg = -1.0; // -tang side (h1)
        for (var k = 0u; k < steps; k++) {
            let sd = params.radius * (f32(k) + 1.0 - jitter) / f32(steps);
            // +tang side
            let sp_p = ssao_project(p + tang * sd);
            if (sp_p.z >= 0.5) {
                let spx = vec2<i32>(i32(round(sp_p.x)), i32(round(sp_p.y)));
                if (spx.x >= 0 && spx.y >= 0 && spx.x <= full_max.x && spx.y <= full_max.y) {
                    let st = textureLoad(src_tex, spx, 0).w;
                    if (st > 1e-4) {
                        let sray = ssao_camera_ray(vec2<f32>(f32(spx.x), f32(spx.y)));
                        let hv = sray[0] + sray[1] * st - p;
                        let hlen = length(hv);
                        if (hlen > 1e-4) {
                            let ch = dot(hv / hlen, v_dir);
                            let w = clamp(1.0 - (hlen - params.radius) / fade, 0.0, 1.0);
                            if (ch > cos_h_pos) { cos_h_pos = mix(cos_h_pos, ch, w); }
                        }
                    }
                }
            }
            // -tang side
            let sp_n = ssao_project(p - tang * sd);
            if (sp_n.z >= 0.5) {
                let spx = vec2<i32>(i32(round(sp_n.x)), i32(round(sp_n.y)));
                if (spx.x >= 0 && spx.y >= 0 && spx.x <= full_max.x && spx.y <= full_max.y) {
                    let st = textureLoad(src_tex, spx, 0).w;
                    if (st > 1e-4) {
                        let sray = ssao_camera_ray(vec2<f32>(f32(spx.x), f32(spx.y)));
                        let hv = sray[0] + sray[1] * st - p;
                        let hlen = length(hv);
                        if (hlen > 1e-4) {
                            let ch = dot(hv / hlen, v_dir);
                            let w = clamp(1.0 - (hlen - params.radius) / fade, 0.0, 1.0);
                            if (ch > cos_h_neg) { cos_h_neg = mix(cos_h_neg, ch, w); }
                        }
                    }
                }
            }
        }

        // Horizon angles from V (h2 toward +tang, h1 toward -tang), clamped to
        // the hemisphere around the projected normal (essential — an open
        // surface must integrate to full visibility around N, not around V).
        var h2 = acos(clamp(cos_h_pos, -1.0, 1.0));
        var h1 = -acos(clamp(cos_h_neg, -1.0, 1.0));
        h1 = n_angle + max(h1 - n_angle, -0.5 * PI);
        h2 = n_angle + min(h2 - n_angle,  0.5 * PI);

        // GTAO inner integral (cosine-weighted visibility arc).
        let inner = 0.25 * (-cos(2.0 * h1 - n_angle) + cos_nq + 2.0 * h1 * sn)
                  + 0.25 * (-cos(2.0 * h2 - n_angle) + cos_nq + 2.0 * h2 * sn);
        occlusion += proj_len * inner;

        // Bent normal: the mid-horizon direction in the slice plane, weighted by
        // the slice's projected-normal length. For an open surface bent_angle =
        // n_angle ⇒ slice_bent = the projected normal, and Σ proj_len·proj_nn = N.
        let bent_angle = (h1 + h2) * 0.5;
        let slice_bent = v_dir * cos(bent_angle) + tang * sin(bent_angle);
        bent_accum += slice_bent * proj_len;
        wsum += proj_len;
    }

    var ao = 1.0;
    var bent = nrm;
    if (wsum > 1e-4) {
        // Contrast lift (matches the SSAO tier's point-estimator compensation:
        // the sparse-slice horizon underestimates near-contact occlusion).
        ao = clamp(pow(clamp(occlusion / f32(slices), 0.0, 1.0), SSAO_INTENSITY), 0.0, 1.0);
        let bl = length(bent_accum);
        bent = select(nrm, bent_accum / bl, bl > 1e-4);
    }
    let oct = oct_encode(bent);
    textureStore(dst_tex, hcoord, vec4(ao, t, oct.x, oct.y));
}

// Depth-aware separable blur for the GTAO tier — denoises the AO (r) AND the
// octahedral bent normal (b,a) with shared bilateral weights; depth (g) passes
// through. Structurally identical to ssao_blur but carries the 2 extra channels.
fn gtao_blur(hcoord: vec2<i32>, dir: vec2<i32>) -> vec4<f32> {
    let center = textureLoad(src_tex, hcoord, 0);
    let d0 = center.g;
    if (d0 <= 1e-4) { return center; } // background — passthrough
    let hmax = vec2<i32>(i32(params.half_size.x) - 1, i32(params.half_size.y) - 1);
    let sigma_s = 2.5;
    let sigma_d = max(params.radius * 0.5, 1e-3);
    var sum_ao = 0.0;
    var sum_oct = vec2<f32>(0.0);
    var wsum = 0.0;
    for (var o = -4; o <= 4; o++) {
        let c = clamp(hcoord + dir * o, vec2<i32>(0, 0), hmax);
        let s = textureLoad(src_tex, c, 0);
        if (s.g <= 1e-4) { continue; }
        let dd = s.g - d0;
        let w = exp(-f32(o * o) / (2.0 * sigma_s * sigma_s))
              * exp(-(dd * dd) / (2.0 * sigma_d * sigma_d));
        sum_ao += s.r * w;
        sum_oct += vec2<f32>(s.b, s.a) * w;
        wsum += w;
    }
    if (wsum <= 1e-4) { return center; }
    return vec4(sum_ao / wsum, d0, sum_oct.x / wsum, sum_oct.y / wsum);
}

@compute @workgroup_size(8, 8, 1)
fn gtao_blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(dst_tex, hcoord, gtao_blur(hcoord, vec2<i32>(1, 0)));
}

@compute @workgroup_size(8, 8, 1)
fn gtao_blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.half_size.x || gid.y >= params.half_size.y) { return; }
    let hcoord = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = gtao_blur(hcoord, vec2<i32>(0, 1));
    // Strength applies to AO only (the bent normal is a direction — never scaled).
    let strengthened = mix(1.0, r.x, params.strength);
    textureStore(dst_tex, hcoord, vec4(strengthened, r.y, r.z, r.w));
}
