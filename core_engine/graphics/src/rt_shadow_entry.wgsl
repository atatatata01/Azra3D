// ============================================================
// AZRA — Ray-Traced Projected Shadows: visibility pass (RT_SHADOWS_DESIGN Stage 1 + 3)
// ============================================================
// Reconstructs world P from the deferred G-buffer (the EXACT reconstruction the
// deferred pass uses for SDF_REFLECT: p = cam_o + ray_d * depth, depth = gbuf_normal.w),
// then fires occlusion rays per point/spot light against the triangle-PT BVH (the shared
// PT_TRAVERSAL region → pt_occluded) and writes per-light visibility into RGBA: channel i =
// light index i, up to 4 lights. Directional lights (the sun) stay on CSM and are never
// written (left 1.0); lights >=4 keep their legacy term in deferred.
//
// Stage 4 (per-light hybrid): each light's own RT-enable slot decides whether the pass traces
// rays for it (a non-RT light stays 1.0 and NO rays fire — deferred keeps it on legacy
// soft_shadow), and each light's own soft radius drives its disc (radius 0 inherits the global
// rt_params.softness). The GLOBAL toggle gates whether this whole pass runs at all.
//
// Stage 1 = ONE hard center ray per light. Stage 3 = N rays jittered over a DISC of the
// light's angular radius (rt_params.softness / per-light radius), perpendicular to the light dir,
// distributed by a per-pixel-rotated golden-angle (Vogel) spiral plus a per-frame temporal
// rotation. visibility = (# unoccluded rays)/N. softness==0 or samples==1 collapses to exactly
// ONE center ray → BIT-IDENTICAL to Stage 1 (the readback no-regression anchor). Because the
// rays fan from the receiver point out to the light disc, the penumbra is physically
// contact-hardening: an occluder near the receiver blocks the whole bundle (sharp), one far
// from it blocks only a fraction (wide).
//
// Consumed inside the SINGLE deferred view (Display Invariant compliant — it writes ONE
// visibility scalar per light the deferred lighting multiplies in; NOT a fused
// multi-object raymarch). Assembled by build_rt_shadow_wgsl: SDF header (config/binding 1,
// output_texture/binding 0, camera_ray) + PT_TRAVERSAL (pt_occluded + bindings 42-47) +
// the LIGHTING region (light accessors) + light_data(26) + gbuf_normal(12) + rt_params(25)
// declared here.
//
// Every traversal loop is hard-bounded by the shared region — a malformed BVH can produce
// a wrong shadow but can NEVER hang the GPU (TDR class). All reads gated by pt_meta counts.
// ============================================================

// G-buffer normal + depth (normal.xyz, world-distance in .w). Declared here because the
// GBUF_BINDINGS region of sdf_lighting.wgsl is NOT pulled into this build.
@group(0) @binding(12) var gbuf_normal_in: texture_2d<f32>;

// Dedicated params uniform (RT_SHADOWS_DESIGN Stage 3). Kept OUT of the locked-624B GpuUniforms
// (config) — a dedicated uniform, matching the DenoiseParams / PtReprojParams pattern. Only this
// pass reads it; the deferred / PT / fog consumers of `config` are unaffected.
struct RtShadowParams {
    softness: f32,    // 0..1 → light angular radius (tan of half-angle, scaled by RT_MAX_TAN)
    samples: u32,     // effective ray count over the disc (1 = hard center ray = Stage-1 identical)
    frame_index: u32, // per-frame jitter rotation (0 = static per-pixel → deterministic, no boiling)
    full_res: u32,    // 0 = soft half-res 2×2 splat (fast); 1 = full-res soft (Samples cleans edge)
    // Progressive-refine AUTO SUN (Phase 2c, owner "light leaking" 2026-08-07): tan of
    // the sun's cone half-angle while the idle accumulator averages ("Sun Softness
    // (idle)"). < 0 = auto sun disabled. Already a tan — NOT scaled by RT_MAX_TAN.
    sun_tan: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
@group(0) @binding(25) var<uniform> rt_params: RtShadowParams;

// P2.2: two RGBA packs → 8 lights (matches deferred min(num_lights, 8)).
// Pack 0 (binding 0 / output_texture): lights 0..3
// Pack 1 (binding 28 / output_texture_hi): lights 4..7
// Soft denoise/temporal still process pack 0; pack 1 stays raw (still far better than SDF soft_shadow).
const RT_SHADOW_MAX_LIGHTS: u32 = 8u;
const RT_SHADOW_PACK: u32 = 4u;

// Second pack storage (binding 28 — free of the SDF-header 0..27 and BVH 42..47 ranges).
@group(0) @binding(28) var output_texture_hi: texture_storage_2d<rgba16float, write>;
const RT_GOLDEN_ANGLE: f32 = 2.3999632; // 2π(1-1/φ) — even Vogel-spiral disc coverage
const RT_TWO_PI: f32 = 6.2831853;
// Softness slider (0..1) → tan of the light's angular half-radius as seen from the surface.
// 0.35 ≈ 19° half-angle: a broad, obviously-soft area light at softness=1, while the 0.15
// default reads as a tight near-contact penumbra. This is the UI-range mapping (documented,
// adaptive to the slider) — not a hidden magic constant.
const RT_MAX_TAN: f32 = 0.35;
/// Virtual ray length for a DIRECTIONAL light (the sun), which has no position. Stands in for
/// "infinity": large enough that any occluder in a normal scene is crossed, small enough to keep
/// f32 precision sane in the BVH traversal (`pt_ray_aabb` works in reciprocals — 1e30 here would
/// make slab intersections degenerate). A caster further than this from the receiver casts no sun
/// shadow, which is the intended trade, not an oversight.
const RT_SUN_DIST: f32 = 1.0e4;

// Interleaved gradient noise (Jimenez 2014) — the SAME function ssao.wgsl uses. Deterministic
// per pixel; the per-frame rotation is added on top, so a static frame_index yields a stable
// (non-boiling) pattern for the no-temporal fallback.
fn rt_ign(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * px.x + 0.00583715 * px.y));
}

@compute @workgroup_size(8, 8, 1)
fn rt_shadow(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(output_texture);
    // Soft penumbra: half-res 2×2 splat by default (~4× fewer rays, viewport perf).
    // full_res==1 → per-pixel disc rays so Samples actually cleans the edge (quality mode).
    // Hard path (samples == 1) always full-res for Stage-1 bit-identical anchor.
    let soft_half = rt_params.samples > 1u && rt_params.full_res == 0u;
    let block = select(1u, 2u, soft_half);
    let bx = gid.x * block;
    let by = gid.y * block;
    if (bx >= dim.x || by >= dim.y) { return; }
    let sample_x = bx + (block - 1u) / 2u;
    let sample_y = by + (block - 1u) / 2u;
    let coord = vec2<i32>(i32(sample_x), i32(sample_y));

    // Pack 0 = lights 0..3, pack 1 = lights 4..7. Default fully lit.
    var vis_lo = array<f32, 4>(1.0, 1.0, 1.0, 1.0);
    var vis_hi = array<f32, 4>(1.0, 1.0, 1.0, 1.0);

    let gn = textureLoad(gbuf_normal_in, coord, 0);
    let depth = gn.w;
    // Background / floor-less / far pixels (.w <= 0) are not surfaces → cast no shadow.
    if (depth > 0.0001) {
        let normal = gn.xyz;
        // Reconstruct world P EXACTLY like the deferred pass (per-pixel aligned with the
        // surface deferred shades — same formula for raster AND SDF-compute pixels).
        let w = f32(dim.x);
        let h = f32(dim.y);
        let uv_x = ((f32(sample_x) + 0.5) / w) * 2.0 - 1.0;
        let uv_y = -(((f32(sample_y) + 0.5) / h) * 2.0 - 1.0);
        let ray = camera_ray(uv_x, uv_y, w / h);
        let p = ray[0] + ray[1] * depth;
        // Shadow-ray origin = P + n*bias. The fixed PT_EPS the path tracer uses is WRONG here:
        // the PT reconstructs P from its own full-precision hit, but this pass reconstructs P from
        // the f16 G-buffer depth (gbuf_normal.w), whose quantization step (~depth/1024) dwarfs
        // PT_EPS at any real distance → the origin snaps to f16 depth SHELLS and the fixed bias
        // can't lift it clear of the surface → concentric-ring self-shadow acne on lit faces.
        // Scale the normal offset with depth (a few f16 ULPs) so it always clears the shell; the
        // ~depth*0.004 offset is imperceptible for contact shadows.
        let bias = max(PT_EPS, depth * (4.0 / 1024.0));
        let origin = p + normal * bias;

        let count = min(num_lights(), RT_SHADOW_MAX_LIGHTS);
        let n = max(rt_params.samples, 1u);
        for (var i = 0u; i < count; i = i + 1u) {
            // Stage 4 per-light hybrid: this light only casts RT shadows if its own RT-enable slot
            // is set. A non-RT light is left fully lit and NO rays are traced for it.
            //
            // DIRECTIONAL (the sun) is opt-in, 2026-07-28 (RT_SHADOWS_DESIGN §"Sun RT area
            // shadows", previously deferred): it used to `continue` unconditionally here and stay
            // on CSM. It now takes the SAME path as point/spot with two substitutions, because a
            // directional light has a DIRECTION but no POSITION:
            //   • ray dir  = the light's own direction (not `light_pos - p`, which is meaningless)
            //   • distance = RT_SUN_DIST, a scene-scale cap standing in for "infinity" — the ray
            //     must keep traversing past where a point light would have terminated.
            // The softness disc below is then built at that virtual distance, so the SAME
            // tangent-basis + golden-angle sampling produces an ANGULAR cone for the sun (the
            // design doc's "directional lights sample an angular cone") with no second code path.
            // Default is OFF per light (`properties_light.rs`), because a sun ray is uncapped and
            // fires on EVERY lit pixel — the most expensive RT light in any scene.
            // Progressive-refine AUTO SUN (post_flags bit 512 + sun_tan >= 0): while the
            // idle accumulator averages, the sun traces leak-free RT shadows WITHOUT its
            // per-light opt-in — the automated fix for light leaking through thin walls
            // (shadow-map bias leaks are structural: any wall thinner than the bias lets
            // light through; a ray needs no bias, so wall thickness stops mattering).
            // Bit 512 is the SAME key deferred's sun swap uses — one signal, no drift.
            let is_dir = light_type_v(i) == 0u;
            let refine_sun = is_dir
                && (config.post_flags & 512u) != 0u
                && rt_params.sun_tan >= 0.0;
            if (!light_rt_enabled(i) && !refine_sun) { continue; }
            let to_l = light_pos(i) - p;
            let raw_dist = length(to_l);
            // A directional light's `light_pos` is not a real position — never divide by it.
            if (!is_dir && raw_dist < 1e-4) { continue; }
            let dist = select(raw_dist, RT_SUN_DIST, is_dir);
            let ldir = select(to_l / max(raw_dist, 1e-4), light_dir_v(i), is_dir);
            let lr = light_rt_radius(i);
            var soft_tan = rt_params.softness * RT_MAX_TAN;
            if (lr > 0.0) {
                // Per-light Source Radius is WORLD units for point/spot (unified
                // 2026-08-07 "fix the light" — same meaning as the PT sampler and the
                // deferred sphere-light highlight): the penumbra angle is the bulb's
                // true subtended angle from this receiver. Directional keeps the
                // legacy angular meaning (a world radius at infinity subtends zero).
                soft_tan = select(lr / max(raw_dist, 1e-3), lr * RT_MAX_TAN, is_dir);
            }
            // Auto sun with no authored Source Radius: the refine cone angle ("Sun
            // Softness (idle)") IS the disc — consistent with the CSM sun-cone jitter
            // it replaces while refining.
            if (refine_sun && lr <= 0.0) {
                soft_tan = rt_params.sun_tan;
            }
            var v = 1.0;
            // While the accumulator averages (bit 256) EVERY soft light takes the disc
            // path even at 1 sample/frame — the per-frame rotation + radius jitter make
            // the 1/N accumulation converge the full disc with one cheap ray per frame.
            // (The refine path deliberately runs samples=1 FULL-RES: the half-res 2×2
            // splat + depth-blind denoise bled visibility across thin-wall silhouettes
            // once penumbras widened — the owner's "leak-proof breaks when softer".)
            let refine_acc_gate = (config.post_flags & 256u) != 0u;
            if ((n == 1u && !refine_sun && !refine_acc_gate) || soft_tan <= 0.0) {
                // HARD center ray — BIT-IDENTICAL to Stage 1 for pack 0.
                if (pt_occluded(origin, ldir, dist)) { v = 0.0; }
            } else {
                let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(ldir.y) > 0.9);
                let tan_u = normalize(cross(helper, ldir));
                let tan_v = cross(ldir, tan_u);
                // `dist * soft_tan` IS the angular formulation: the disc subtends a fixed half-angle
                // regardless of `dist`, so a directional light with `dist = RT_SUN_DIST` samples a
                // CONE of that same half-angle. One formula, both light types.
                let disc_r = dist * soft_tan;
                // Disc CENTER: a real point light's own position, but for a directional light
                // `light_pos` is not a position — the centre is the virtual sun point at
                // `origin + ldir * RT_SUN_DIST`, i.e. straight up the ray. Using `light_pos` here
                // for the sun would aim every sample ray at an arbitrary scene point.
                let disc_c = select(light_pos(i), origin + ldir * dist, is_dir);
                // Disc sample coordinates.
                // NON-refining: static per-pixel rotation, mid-radius (deterministic).
                // REFINING (bit 256): a TRUE 2D low-discrepancy sequence with per-pixel
                // phase in BOTH angle and radius (owner "why do I see those dots"
                // 2026-08-07): the previous scheme drove angle AND radius from the
                // same frame counter, so each pixel integrated a thin rotated SPIRAL
                // through the disc — pixels converged to slightly DIFFERENT values,
                // a permanent stipple no accumulation could average away. With
                // decorrelated R2 fractions and independent per-pixel phases, every
                // pixel's sample set covers the same full disc → all pixels converge
                // to the same smooth penumbra.
                let px_f = vec2<f32>(f32(sample_x), f32(sample_y));
                let phase_a = rt_ign(px_f);
                let refine_acc = (config.post_flags & 256u) != 0u;
                let f_idx = f32(rt_params.frame_index);
                // D4: per-light phase offset — decorrelates disc samples across lights
                // pre-convergence (same pixel no longer shares one spiral phase for every
                // soft caster → multi-light penumbra overlap averages cleanly).
                let light_phase = f32(i) * 0.6180339887;
                var rot: f32;
                var fj: f32;
                if (refine_acc) {
                    let phase_r = rt_ign(px_f + vec2<f32>(37.0, 91.0));
                    rot = fract(f_idx * 0.7548777 + phase_a + light_phase) * RT_TWO_PI;
                    fj = fract(f_idx * 0.5698403 + phase_r + light_phase * 0.381966);
                } else {
                    rot = phase_a * RT_TWO_PI + f_idx * RT_GOLDEN_ANGLE + light_phase * RT_TWO_PI;
                    fj = 0.5;
                }
                var occ = 0u;
                for (var s = 0u; s < n; s = s + 1u) {
                    let fi = (f32(s) + fj) / f32(n);
                    let rr = sqrt(fi) * disc_r;
                    let ang = f32(s) * RT_GOLDEN_ANGLE + rot;
                    let disc_pt = disc_c + tan_u * (cos(ang) * rr) + tan_v * (sin(ang) * rr);
                    let d = disc_pt - origin;
                    let dl = length(d);
                    let dir = d / max(dl, 1e-4);
                    // HORIZON CLIP (owner "still leaking at 20% softness" 2026-08-07):
                    // a cone sample below the receiver's tangent plane is self-occluded
                    // by definition — but TRACING it grazes along the receiver's own
                    // (often thin) geometry, where any-hit precision misses report
                    // "clear" and wide cones wash whole walls with false light. Count
                    // it occluded without tracing: physically correct (that part of
                    // the light disc is behind the surface) AND leak-proof.
                    if (dot(dir, normal) < 1e-3) {
                        occ = occ + 1u;
                        continue;
                    }
                    if (pt_occluded(origin, dir, dl)) { occ = occ + 1u; }
                }
                v = 1.0 - f32(occ) / f32(n);
            }
            // Explicit channel assign (no dynamic array index write — portability).
            if (i < RT_SHADOW_PACK) {
                if (i == 0u) { vis_lo[0] = v; }
                else if (i == 1u) { vis_lo[1] = v; }
                else if (i == 2u) { vis_lo[2] = v; }
                else { vis_lo[3] = v; }
            } else {
                let j = i - RT_SHADOW_PACK;
                if (j == 0u) { vis_hi[0] = v; }
                else if (j == 1u) { vis_hi[1] = v; }
                else if (j == 2u) { vis_hi[2] = v; }
                else { vis_hi[3] = v; }
            }
        }
    }

    // Write 1×1 (hard full-res) or 2×2 splat (soft half-res) to both packs.
    let out_lo = vec4<f32>(vis_lo[0], vis_lo[1], vis_lo[2], vis_lo[3]);
    let out_hi = vec4<f32>(vis_hi[0], vis_hi[1], vis_hi[2], vis_hi[3]);
    for (var dy = 0u; dy < block; dy++) {
        for (var dx = 0u; dx < block; dx++) {
            let c = vec2<i32>(i32(bx + dx), i32(by + dy));
            if (u32(c.x) < dim.x && u32(c.y) < dim.y) {
                textureStore(output_texture, c, out_lo);
                textureStore(output_texture_hi, c, out_hi);
            }
        }
    }
}
