// ============================================================
// ssgi_entry.wgsl — screen-space one-bounce indirect diffuse
//
// Plan: docs/planning/render/SSGI_PLAN.md. Assembled by build_ssgi_wgsl():
//   sdf_shared.wgsl (GpuUniforms, unpack_rgb, camera_ray, output_texture@0)
// + env_data@21 declared inline (sdf_lighting.wgsl declares it OUTSIDE its regions)
// + ssao_in@38 declared inline (deferred_entry.wgsl likewise)
// + region ENV          from sdf_lighting.wgsl  (sample_env_raw, env_sh_irradiance)
// + region AO_UPSAMPLE  from deferred_entry.wgsl (ssao_factor, ao_bent_normal,
//                                                 multi_bounce_ao)
// + this file
//
// Runs at HALF resolution after deferred + SSR, before fog. Writes a signed
// radiance DELTA that pass_tonemap adds beside the SSR add. The delta is signed
// by construction: where geometry occludes the environment, SSGI removes the sky
// deferred already applied and puts the bounced radiance in its place.
//
// GATE 1 STATUS: the gather is a constant (debug flag DBG_FORCE_CONST) so the
// composite path can be proven neutral before any GI math exists. The march
// lands in Gate 2.
//
// Depth convention: gbuf_normal.w is the ray parameter t along camera_ray(), in
// BOTH projections (render/src/gbuffer.wgsl gbuf_depth(), fixed 2026-08-06).
// ============================================================

// Half-res delta target. NOTE: binding 0 is the sdf_shared header's
// `output_texture` — reused, never re-declared (SSGI_PLAN.md §4.9).

@group(0) @binding(5)  var hdr_in: texture_2d<f32>;          // deferred-lit, pre-fog
@group(0) @binding(12) var gbuf_normal_in: texture_2d<f32>;  // xyz=normal, w=depth
@group(0) @binding(13) var gbuf_material_in: texture_2d<f32>;// x=rough y=metal z=sh*ao w=opac+em
@group(0) @binding(14) var gbuf_albedo_in: texture_2d<f32>;  // rgb=albedo, a=flags
@group(0) @binding(29) var raster_depth_in: texture_2d<f32>; // raster snapshot, is_raster gate
@group(0) @binding(60) var ssgi_src: texture_2d<f32>;        // blur ping-pong source

// ── Dev debug flags (SSGI_PLAN.md §4.10). Every bit's OFF state is production. ──
const DBG_FORCE_CONST: u32   = 1u;  // gather writes params.dbg_const instead of marching
const DBG_NO_ENV_SUBTRACT: u32 = 2u;  // drop the env subtraction (raw gather)
const DBG_SKIP_BLUR: u32     = 4u;  // (consumed CPU-side: skip the two blur dispatches)

struct SsgiParams {
    // xy = half-res size, zw = full-res size
    half_size: vec2<u32>,
    full_size: vec2<u32>,
    intensity: f32,
    radius: f32,
    // "HDRI Occlusion" — how much occluded environment light SSGI may REMOVE.
    // 1.0 = energy-consistent (default), 0.0 = additive only. User-facing
    // advanced HDR setting; does nothing when env_width == 0.
    env_occlusion: f32,
    rays: u32,
    debug_flags: u32,
    dbg_const: f32,
    // Progressive-refine frame term (Gate 4, temporal-accumulation arm): nonzero
    // ONLY while the PBR accumulator is averaging (CPU-gated) — 0 keeps the
    // pattern deterministic per pixel (no boiling without an integrator, the
    // SSGI_PLAN.md §4.3 constraint, which the accumulator exactly inverts).
    frame_index: u32,
    // C2: light-probe diffuse blend weight (strength × ready × enabled). Deferred
    // ambient is mix(env_sh, probe_sh, w); env-subtraction scales by (1 − w) so
    // SSGI never removes energy deferred replaced with the probe.
    probe_diffuse_w: f32,
};
@group(0) @binding(61) var<uniform> ssgi: SsgiParams;

const SSGI_GOLDEN_ANGLE: f32 = 2.3999632;

// Interleaved gradient noise — deterministic per pixel when frame_index == 0.
// While the progressive accumulator averages, the standard 5.588238 offset
// animates the pattern and each frame samples a different ray rotation — the
// 1/N mean converges to the full hemisphere integral ("infinite rays" over
// time, owner 2026-08-07). Jitter without an accumulator would boil; the CPU
// only sets frame_index while the accumulator is live.
fn ssgi_ign(px: vec2<f32>) -> f32 {
    // R2 low-discrepancy per-frame offset (owner "GI feels broken while
    // restarting" 2026-08-07): the previous diagonal slide (f × 5.588238 on both
    // axes) moved the whole pattern coherently — readable as "marching" texture
    // before convergence. R2 offsets decorrelate frames without directionality.
    // frame_index 0 → zero offset → the exact static pattern.
    let f = f32(ssgi.frame_index);
    let p = px + vec2(fract(f * 0.7548777) * 64.0, fract(f * 0.5698403) * 64.0);
    return fract(52.9829189 * fract(0.06711056 * p.x + 0.00583715 * p.y));
}

// ── Screen-space marching ───────────────────────────────────────────────────
// Projection / metric / interpolation are ports of pass_ssr's (post_process.wgsl
// :222-255), deliberately identical rather than re-derived: they are the ONE
// convention that agrees with what gbuf_depth() writes, in both projections.
// Keep them in lockstep with SSR — a divergence here is invisible in perspective
// and silently wrong in ortho, which is the 2026-08-06 bug class.

const SSGI_NEAR_Z: f32 = 0.05;
const SSGI_EDGE_FADE_UV: f32 = 0.08;
const SSGI_REFINE_ITERS: u32 = 4u;
// 32 steps over `ssgi.radius` world units. Raised from 24 after Gate 2 measured a
// missed hit at the far probe with radius 3.0: coarse strides let a ray cross a
// surface between samples faster than the adaptive thickness band can absorb.
// Gate 5 may tune this against the perf budget — it is the main quality/cost dial
// after ray count.
const SSGI_MAX_STEPS: u32 = 32u;

/// World -> (uv.xy in 0..1, forward-z). Perspective AND ortho branches.
fn ssgi_project(world: vec3<f32>, aspect: f32) -> vec3<f32> {
    let rel = world - config.cam_pos.xyz;
    let z = dot(rel, config.cam_forward.xyz);
    let ortho_hw = config.cam_pos.w;
    let fov_scale = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
    var sx: f32;
    var sy: f32;
    if (ortho_hw > 0.0) {
        sx = dot(rel, config.cam_right.xyz) / (ortho_hw * aspect);
        sy = dot(rel, config.cam_up.xyz) / ortho_hw;
    } else {
        let zz = max(z, 1e-4);
        sx = dot(rel, config.cam_right.xyz) / (zz * aspect * fov_scale);
        sy = dot(rel, config.cam_up.xyz) / (zz * fov_scale);
    }
    return vec3<f32>((sx + 1.0) * 0.5, (-sy + 1.0) * 0.5, z);
}

/// The quantity gbuf_normal.w holds: ray parameter along camera_ray().
/// Perspective = |rel|; ortho = forward distance.
fn ssgi_metric(world: vec3<f32>) -> f32 {
    let rel = world - config.cam_pos.xyz;
    let z = dot(rel, config.cam_forward.xyz);
    return select(length(rel), z, config.cam_pos.w > 0.0);
}

/// Perspective-correct point at screen-space fraction u along a segment.
/// Screen-linear interpolation is linear in P/z and 1/z; ortho is a plain lerp.
fn ssgi_point_at(p0: vec3<f32>, p1: vec3<f32>, z0: f32, z1: f32, u: f32) -> vec3<f32> {
    if (config.cam_pos.w > 0.0) { return mix(p0, p1, u); }
    let inv_z = mix(1.0 / z0, 1.0 / z1, u);
    return mix(p0 / z0, p1 / z1, u) / inv_z;
}

/// March one ray. Returns vec4(radiance, confidence); confidence 0 = miss.
///
/// Every guard here exists because SSR already paid for it (§6 T10): a start bias
/// along BOTH normal and ray, backface rejection that structurally kills self-hits,
/// an adaptive thickness band, a >=2px stride so consecutive steps cannot land on
/// one texel, a near-plane clip, a smooth screen-edge fade instead of a hard break,
/// and binary refinement between the last-in-front and first-behind samples.
fn ssgi_march(p: vec3<f32>, n: vec3<f32>, d: vec3<f32>, aspect: f32, dim: vec2<f32>) -> vec4<f32> {
    let p_start = p + n * 0.02 + d * 0.01;
    var t_end = ssgi.radius;
    if (config.cam_pos.w <= 0.0) {
        let dz = dot(d, config.cam_forward.xyz);
        // C5: near-plane clip only when the ray heads toward/behind the camera
        // (dz < 0). Rays receding from the camera never cross the near plane.
        if (dz < 0.0) {
            let z_start = dot(p_start - config.cam_pos.xyz, config.cam_forward.xyz);
            if (z_start + t_end * dz < SSGI_NEAR_Z) {
                t_end = max((SSGI_NEAR_Z - z_start) / dz, 1e-3);
            }
        }
    }
    // Step uniformly in WORLD space, not screen space — the one place this march
    // deliberately departs from pass_ssr.
    //
    // SSR marches screen-uniformly because a reflection search wants even coverage
    // of the pixels it might hit. A GI gather is different: its reach is a bounded
    // world radius, and screen-uniform steps become ever-larger world steps as a
    // ray recedes from the camera, so a ray heading away skips hits entirely.
    // Measured at Gate 2: with screen-uniform stepping, orthographic matched the
    // CPU oracle to 4 decimals at every probe while perspective missed two of
    // three — exactly the projections where the two schemes differ, since
    // ssgi_point_at reduces to a plain lerp in ortho. Raising the step count to 96
    // also fixed it, at 3x the cost, which confirmed sampling density rather than
    // a formula error.
    //
    // SSR's `>=2px screen stride` floor is deliberately NOT carried over. It exists
    // to stop screen-uniform steps from resampling one texel — but against
    // world-uniform steps it is a category error: a receding ray has a SHORT screen
    // path, so the floor enlarges `du` and coarsens the world step exactly where
    // sampling must be finest. Measured: it was the remaining cause of the
    // perspective far probe reading 0 while the oracle said -0.114, after
    // world-uniform stepping had already fixed the near probe.
    let du = 1.0 / f32(SSGI_MAX_STEPS);

    var u_lo = 0.0;
    var u_hi = 0.0;
    var hit = false;
    var uv_hit = vec2<f32>(0.0);
    var m_prev = ssgi_metric(p_start);

    for (var i = 0u; i < SSGI_MAX_STEPS; i++) {
        let u = min((f32(i) + 0.5) * du, 1.0);
        let q = p_start + d * (t_end * u);
        let s = ssgi_project(q, aspect);
        if (s.x < 0.0 || s.x > 1.0 || s.y < 0.0 || s.y > 1.0 || s.z < SSGI_NEAR_Z) { break; }
        let sc = vec2<i32>(i32(s.x * dim.x), i32(s.y * dim.y));
        let sample_n = textureLoad(gbuf_normal_in, sc, 0);
        let sample_depth = sample_n.w;
        let m = ssgi_metric(q);
        if (sample_depth > 0.0) {
            let thick = 0.05 + max(m - m_prev, 0.0) * 1.5 + 0.01 * sample_depth;
            let delta = m - sample_depth;
            // A valid hit faces AGAINST the ray. This also structurally rejects
            // self-hits: the origin surface faces WITH the ray.
            if (delta > 0.0 && delta < thick && dot(sample_n.xyz, d) < 0.0) {
                hit = true;
                u_hi = u;
                uv_hit = s.xy;
                break;
            }
        }
        // Advance the refinement floor past every rejected sample, so the binary
        // search can only ever bisect the LAST stride.
        u_lo = u;
        m_prev = m;
        if (u >= 1.0) { break; }
    }
    if (!hit) { return vec4<f32>(0.0); }

    // C3: refine in the same world-uniform parameter as the coarse loop
    // (q = p_start + d * (t_end * um)). Screen-space ssgi_point_at was a
    // different curve — bisect could leave the coarse bracket on receding rays.
    for (var k = 0u; k < SSGI_REFINE_ITERS; k++) {
        let um = 0.5 * (u_lo + u_hi);
        let q = p_start + d * (t_end * um);
        let s = ssgi_project(q, aspect);
        let sc = vec2<i32>(
            clamp(i32(s.x * dim.x), 0, i32(dim.x) - 1),
            clamp(i32(s.y * dim.y), 0, i32(dim.y) - 1),
        );
        let sd = textureLoad(gbuf_normal_in, sc, 0).w;
        if (sd > 0.0 && ssgi_metric(q) > sd) { u_hi = um; uv_hit = s.xy; } else { u_lo = um; }
    }

    let hc = vec2<i32>(
        clamp(i32(uv_hit.x * dim.x), 0, i32(dim.x) - 1),
        clamp(i32(uv_hit.y * dim.y), 0, i32(dim.y) - 1),
    );
    // Radiance = the lit HDR at the hit. This is radiance toward the CAMERA reused
    // as radiance toward the shading point (view-independence assumed) — the
    // standard SSGI approximation, documented in §4.2 rather than hidden.
    let radiance = textureLoad(hdr_in, hc, 0).rgb;
    // Smooth screen-edge fade: a hard cutoff leaves a visible seam where rays
    // start leaving the frame.
    let e = min(min(uv_hit.x, 1.0 - uv_hit.x), min(uv_hit.y, 1.0 - uv_hit.y));
    let conf = smoothstep(0.0, SSGI_EDGE_FADE_UV, e);
    return vec4<f32>(radiance, conf);
}

// Receiver test. Flags, never alpha: alpha and pixel class are independent axes
// (Wireframe stores lit geometry with alpha 0). Accepts the 0.78 geometry band
// and the 1.0 selected band; rejects background, floor/workplane, ghost, VFX and
// DC wire. v1 additionally requires a RASTER pixel, because deferred forces
// final_ao = 1.0 only there — including sculpt-atlas pixels would over-subtract.
fn ssgi_is_receiver(c: vec2<i32>, depth: f32) -> bool {
    if (depth <= 0.0) { return false; }
    let f = textureLoad(gbuf_albedo_in, c, 0).a;
    let is_geo = (f > 0.76 && f < 0.83) || f > 0.93;
    if (!is_geo) { return false; }
    let raster_snap = textureLoad(raster_depth_in, c, 0).w;
    return raster_snap > 0.0 && abs(raster_snap - depth) < 0.05;
}

@compute @workgroup_size(8, 8, 1)
fn ssgi_gather(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= ssgi.half_size.x || gid.y >= ssgi.half_size.y) { return; }
    let h = vec2<i32>(i32(gid.x), i32(gid.y));
    // Half texel h maps to full pixel 2h — the SSAO convention (ssao.wgsl:123-126),
    // so the gather origin is an exact G-buffer pixel. NOT pt_upscale's box
    // convention, which would shift the result half a pixel.
    let c = h * 2;

    let gn = textureLoad(gbuf_normal_in, c, 0);
    let depth = gn.w;

    // Non-receivers write zero RGB and keep the guide depth. Neutrality comes from
    // zero radiance, never from alpha.
    if (!ssgi_is_receiver(c, depth)) {
        // Guide depth 0 = "no data here", NOT "zero radiance here". Both consumers
        // (ssgi_blur, and ssgi_delta in post_process.wgsl) skip taps with a <= 1e-4.
        // Storing the real depth instead would make a rejected pixel look like a
        // valid zero sample at a plausible depth, so the blur and the upsample
        // would average its zero into neighbouring receivers — a dark seam ~8 px
        // wide along every geometry/floor-grid boundary, i.e. exactly the halo
        // class VG-C rejects on. ssao.wgsl:132 writes 0 for the same reason.
        textureStore(output_texture, h, vec4<f32>(0.0));
        return;
    }

    // View mode: PBR (0) and Lit (6) only — the modes whose ambient/IBL term this
    // delta is defined to correct. Solid/MatCap/Wireframe/RT never applied it.
    if (!(config.view_mode == 0u || config.view_mode == 6u)) {
        textureStore(output_texture, h, vec4<f32>(0.0));   // no data (see above)
        return;
    }

    if ((ssgi.debug_flags & DBG_FORCE_CONST) != 0u) {
        // Gate 1: prove the composite is neutral at 0 and touches exactly the
        // receiver pixels at non-zero, with no GI math in play.
        textureStore(output_texture, h, vec4<f32>(vec3<f32>(ssgi.dbg_const), depth));
        return;
    }

    // ── The terms deferred actually applied (SSGI_PLAN.md §4.2) ─────────────
    let n = gn.xyz;
    // The BENT normal, not the geometric one: deferred evaluates its diffuse
    // environment at ao_bent_normal(), and this delta is defined to cancel that
    // exact term. With GTAO on (the default tier) they differ most inside corners,
    // which is the only place SSGI does anything.
    let n_s = ao_bent_normal(c, depth, n);
    let albedo = textureLoad(gbuf_albedo_in, c, 0).rgb;
    let gm = textureLoad(gbuf_material_in, c, 0);
    let metal = gm.y;                       // .y is metallic; .x is roughness
    let opacity = min(gm.w, 1.0);           // .w packs opacity + emissive luminance
    let ssao_f = ssao_factor(c, depth);
    let mb = multi_bounce_ao(ssao_f, albedo);
    // is_raster receivers only (enforced above), and deferred forces final_ao = 1
    // on exactly those, so the AO deferred applied to its env term is 1.0.
    let ao_d = 1.0;
    let has_env = config.env_width > 0u;

    let dim = vec2<f32>(f32(ssgi.full_size.x), f32(ssgi.full_size.y));
    let aspect = dim.x / dim.y;
    let cam = camera_ray(
        (f32(c.x) + 0.5) / dim.x * 2.0 - 1.0,
        -((f32(c.y) + 0.5) / dim.y * 2.0 - 1.0),
        aspect,
    );
    let p = cam[0] + cam[1] * depth;

    // Cosine-weighted hemisphere about n_s, rotated per half-res pixel by
    // interleaved gradient noise. frame_index 0 → static pattern (no boiling
    // without an integrator); while the PBR accumulator averages, ssgi_ign
    // rotates the pattern per frame so the 1/N mean converges to the full
    // hemisphere (Gate 4 temporal arm).
    let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n_s.y) > 0.9);
    let tangent = normalize(cross(helper, n_s));
    let bitangent = cross(n_s, tangent);
    let rot = ssgi_ign(vec2<f32>(f32(h.x), f32(h.y))) * 6.2831853;

    let n_rays = max(ssgi.rays, 1u);
    var l_sum = vec3<f32>(0.0);
    var s_sum = vec3<f32>(0.0);
    for (var i = 0u; i < n_rays; i++) {
        let fi = (f32(i) + 0.5) / f32(n_rays);
        let ang = f32(i) * SSGI_GOLDEN_ANGLE + rot;
        let sin_t = sqrt(fi);
        let cos_t = sqrt(1.0 - fi);
        let dir = normalize(
            tangent * (cos(ang) * sin_t) + bitangent * (sin(ang) * sin_t) + n_s * cos_t
        );
        let r = ssgi_march(p, n_s, dir, aspect, dim);
        if (r.w > 0.0) {
            l_sum += r.rgb * r.w;
            if (has_env) {
                // RAW radiance, never a prefiltered mip: mips 1..6 are GGX-convolved,
                // and cosine-averaging one collapses the subtraction to a constant —
                // the "GI that looks like flat ambient" failure §4.9 rejects.
                s_sum += sample_env_raw(dir) * config.env_brightness * r.w;
            }
        }
    }

    let inv_n = 1.0 / f32(n_rays);
    // Never remove more energy than deferred added. Structural, not a tolerance:
    // env_sh_irradiance reconstructs only l<=2 while a raw-radiance estimator
    // carries every band, so a small bright sun makes individual samples
    // legitimately exceed the applied term.
    // C5: clamp already includes ao·mb once; do NOT multiply ao·mb again on
    // subtraction (was double-applied when the clamp bound).
    let applied = ao_d * mb * env_sh_irradiance(n_s);
    var s_term = min(s_sum * inv_n, applied);
    if ((ssgi.debug_flags & DBG_NO_ENV_SUBTRACT) != 0u) { s_term = vec3<f32>(0.0); }
    // C2: scale env removal by (1 − probe_diffuse_w) — deferred already replaced
    // that fraction of env_sh with probe_sh.
    let env_scale = 1.0 - clamp(ssgi.probe_diffuse_w, 0.0, 1.0);
    let delta = opacity * ssgi.intensity * albedo * (1.0 - metal)
              * (l_sum * inv_n - ssgi.env_occlusion * env_scale * s_term);

    textureStore(output_texture, h, vec4<f32>(delta, depth));
}

// ── Separable bilateral denoise, half-res, ping-pong (Gate 4) ───────────────
// Same shape as ssao_blur (render/src/ssao.wgsl:190-212): background texels are
// skipped by their in-band guide depth, and total rejection returns the source
// unchanged rather than inventing a value.
//
// 2026-08-07 denoiser upgrade (owner "we do not have the denoiser"; the SSGI
// review's Gate-4 items): (a) NORMAL edge-stopping — the depth-only guide blurred
// straight across creases, muddying corners (exactly where GI matters most);
// (b) convergence-aware reach — while the progressive accumulator integrates
// (frame_index > 0), the spatial sigma tightens as 1/√N so the CONVERGED mean is
// sharp: the accumulator replaces spatial filtering over time. frame_index 0
// (static / non-refining) keeps the full 2.5σ — unchanged behavior.
fn ssgi_blur(h: vec2<i32>, dir: vec2<i32>) -> vec4<f32> {
    let center = textureLoad(ssgi_src, h, 0);
    let d0 = center.a;
    if (d0 <= 1e-4) { return center; }
    let hmax = vec2<i32>(i32(ssgi.half_size.x) - 1, i32(ssgi.half_size.y) - 1);
    let n_acc = f32(ssgi.frame_index);
    let sigma_s = select(
        2.5,
        max(2.5 * inverseSqrt(1.0 + n_acc / 16.0), 0.8),
        n_acc > 0.0,
    );
    let sigma_d = max(ssgi.radius * 0.5, 1e-3);
    let n0 = textureLoad(gbuf_normal_in, h * 2, 0).xyz;
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var o = -4; o <= 4; o++) {
        let hc = clamp(h + dir * o, vec2<i32>(0), hmax);
        let s = textureLoad(ssgi_src, hc, 0);
        if (s.a <= 1e-4) { continue; }
        let dd = s.a - d0;
        // Normal edge-stop: taps across a crease contribute ~nothing.
        let ns = textureLoad(gbuf_normal_in, hc * 2, 0).xyz;
        let wn = pow(max(dot(n0, ns), 0.0), 16.0);
        let w = exp(-f32(o * o) / (2.0 * sigma_s * sigma_s))
              * exp(-(dd * dd) / (2.0 * sigma_d * sigma_d))
              * wn;
        sum += s.rgb * w;
        wsum += w;
    }
    if (wsum <= 1e-4) { return center; }
    return vec4<f32>(sum / wsum, d0);
}

@compute @workgroup_size(8, 8, 1)
fn ssgi_blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= ssgi.half_size.x || gid.y >= ssgi.half_size.y) { return; }
    let h = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(output_texture, h, ssgi_blur(h, vec2<i32>(1, 0)));
}

@compute @workgroup_size(8, 8, 1)
fn ssgi_blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= ssgi.half_size.x || gid.y >= ssgi.half_size.y) { return; }
    let h = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(output_texture, h, ssgi_blur(h, vec2<i32>(0, 1)));
}
