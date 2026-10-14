// ============================================================
// AZRA — Deferred Shading Entry Point
// Part of the split compute shader. Requires sdf_shared.wgsl.
// ============================================================

// ============================================================
// Pass 2: Deferred Shading — reads G-Buffer → writes HDR
// ============================================================

// Screen-space contact shadows: short world-space march toward light,
// reprojected to screen to sample depth buffer for nearby occluders.
// Catches small-scale shadow detail (screw heads, panel gaps, contact zones)
// that SDF soft_shadow misses. Cost: ~8 texture samples per pixel.
fn contact_shadow(cs_p: vec3<f32>, cs_n: vec3<f32>, cs_ld: vec3<f32>) -> f32 {
    if (config.cam_pos.w > 0.0) { return 1.0; } // skip orthographic
    let cs_cp = config.cam_pos.xyz;
    let cs_cf = config.cam_forward.xyz;
    let cs_cr = config.cam_right.xyz;
    let cs_cu = config.cam_up.xyz;
    let cs_fs = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
    let cs_dim = textureDimensions(gbuf_normal_in);
    let cs_w = f32(cs_dim.x); let cs_h = f32(cs_dim.y);
    let cs_asp = cs_w / cs_h;
    // Tweakable constants (future: expose via uniforms)
    let cs_max_dist = 0.5;  // world-space reach of contact shadows
    let cs_strength = 0.7;  // max darkening (0=none, 1=full black)
    let cs_bias = 0.02;     // surface bias to prevent self-shadowing
    let cs_steps = 8;       // quality vs cost (8 ≈ 0.3ms)
    let cs_step_sz = cs_max_dist / f32(cs_steps);
    var cs_ray = cs_p + cs_n * cs_bias;
    for (var cs_i = 0; cs_i < cs_steps; cs_i++) {
        cs_ray += cs_ld * cs_step_sz;
        let cs_d = cs_ray - cs_cp;
        let cs_fz = dot(cs_d, cs_cf);
        if (cs_fz <= 0.0) { break; }
        let cs_ux = dot(cs_d, cs_cr) / (cs_fz * cs_asp * cs_fs);
        let cs_uy = dot(cs_d, cs_cu) / (cs_fz * cs_fs);
        if (abs(cs_ux) > 1.0 || abs(cs_uy) > 1.0) { break; }
        let cs_sx = i32((cs_ux + 1.0) * 0.5 * cs_w);
        let cs_sy = i32((1.0 - cs_uy) * 0.5 * cs_h);
        if (cs_sx < 0 || cs_sx >= i32(cs_dim.x) || cs_sy < 0 || cs_sy >= i32(cs_dim.y)) { break; }
        let cs_sd = textureLoad(gbuf_normal_in, vec2<i32>(cs_sx, cs_sy), 0).w;
        let cs_rd = length(cs_d);
        // Occlusion: stored depth closer than sample, within thickness limit
        if (cs_sd > 0.0 && cs_sd < cs_rd - 0.005 && (cs_rd - cs_sd) < cs_max_dist) {
            let cs_fade = 1.0 - f32(cs_i + 1) / f32(cs_steps);
            return 1.0 - cs_fade * cs_strength;
        }
    }
    return 1.0;
}

// ── SSAO (render/src/ssao.wgsl → half-res factor texture, binding 38) ──
// r = mix(1, ao, strength) (strength already applied by the blur_v pass),
// g = the half-res center depth used for the depth-aware upsample below.
// When SSAO is disabled the texture is white-filled (r=1, g=0) — this function
// then returns 1.0 (the 2026-07-17d behavior). Kept OUTSIDE every #region:
// referenced by the always-present PBR ambient/IBL terms (df_lite included).
@group(0) @binding(38) var ssao_in: texture_2d<f32>;

// ── RT projected shadows visibility (RT_SHADOWS_DESIGN Stage 1 + P2.2 dual pack) ──
// Pack0 binding 40: lights 0..3 (RGBA). Pack1 binding 50: lights 4..7 (RGBA).
// RT still packs first 8 shade lights. Always bound so the layout is stable.
@group(0) @binding(40) var rt_shadow_vis_in: texture_2d<f32>;
@group(0) @binding(50) var rt_shadow_vis_hi_in: texture_2d<f32>;

// ── Auto LIGHT probe SH9 irradiance (Phase 3b, probe_bake.rs) ──
// [0..8] = RGB coefficients projected from the baked reflection-probe cube;
// [9] = (strength — CPU-owned, 0, 0, valid — GPU-owned). Zero buffer = inert.
// Kept OUTSIDE every #region: referenced by the always-present ambient terms.
@group(0) @binding(51) var<storage, read> probe_sh: array<vec4<f32>, 10>;

// Blend weight for probe-vs-HDRI diffuse ambient: the strength slider clamped
// to [0,1], zeroed until a bake round has published coefficients.
fn probe_sh_weight() -> f32 {
    return clamp(probe_sh[9].x, 0.0, 1.0) * probe_sh[9].w;
}

// Ramamoorthi-Hanrahan irradiance from the probe SH, π-normalized to the same
// exit-radiance convention as env_sh_irradiance (caller multiplies albedo).
// Strength is NOT applied here — callers weight via probe_sh_weight().
fn probe_sh_irradiance(n: vec3<f32>) -> vec3<f32> {
    if (probe_sh[9].w < 0.5) { return vec3<f32>(0.0); }
    let c1 = 0.429043; let c2 = 0.511664; let c3 = 0.743125;
    let c4 = 0.886227; let c5 = 0.247708;
    let e = c4 * probe_sh[0].rgb
        + 2.0 * c2 * (probe_sh[1].rgb * n.y + probe_sh[2].rgb * n.z + probe_sh[3].rgb * n.x)
        + 2.0 * c1 * (probe_sh[4].rgb * n.x * n.y + probe_sh[5].rgb * n.y * n.z + probe_sh[7].rgb * n.x * n.z)
        + c3 * probe_sh[6].rgb * n.z * n.z - c5 * probe_sh[6].rgb
        + c1 * probe_sh[8].rgb * (n.x * n.x - n.y * n.y);
    return max(e, vec3<f32>(0.0)) * (1.0 / 3.14159265);
}

// ── Probe PREVIEW ("Show Probes", post_flags bit 2048) ──
// The probe cube + params, same resources ssr_bgl carries at 65/66/67. Used only
// to ray-trace the two preview balls at the probe center (a chrome ball showing
// the raw capture, an irradiance ball showing the SH lighting) so the user can
// SEE both probe contents in-viewport. Kept OUTSIDE every #region.
struct ProbePvParams {
    center_enabled: vec4<f32>, // xyz = probe center (world), w = 1 when baked+on
    extent_mips: vec4<f32>,    // xyz = proxy AABB half-extents, w = max mip index
};
@group(0) @binding(65) var probe_cube_pv: texture_cube<f32>;
@group(0) @binding(66) var probe_samp_pv: sampler;
@group(0) @binding(67) var<uniform> probe_params_pv: ProbePvParams;

// Nearest positive ray-sphere intersection distance, or -1.
fn probe_ball_hit(ro: vec3<f32>, rd: vec3<f32>, c: vec3<f32>, r: f32) -> f32 {
    let oc = ro - c;
    let b = dot(oc, rd);
    let disc = b * b - (dot(oc, oc) - r * r);
    if (disc < 0.0) { return -1.0; }
    let t = -b - sqrt(disc);
    return select(-1.0, t, t > 0.0);
}

// ── P3.1 Phase D: CPU clustered light lists (tile × log-Z) ──
// Header: tiles_x,y,z_slices,tile_px, max_per,enabled, screen_w,h, near_bits,far_bits
@group(0) @binding(24) var<storage, read> light_cluster: array<u32>;
const CLUSTER_HDR: u32 = 10u;
const MAX_SHADE_DEFERRED: u32 = 32u;

fn cluster_on() -> bool {
    return arrayLength(&light_cluster) >= CLUSTER_HDR && light_cluster[5u] != 0u;
}

fn cluster_z_slice(view_z: f32) -> u32 {
    let near = bitcast<f32>(light_cluster[8u]);
    let far = bitcast<f32>(light_cluster[9u]);
    let z_slices = max(light_cluster[2u], 1u);
    let n = max(near, 0.01);
    let f = max(far, n + 1.0);
    let z = clamp(view_z, n, f);
    let denom = max(log(f / n), 1e-6);
    let t = clamp(log(z / n) / denom, 0.0, 0.9999);
    return u32(t * f32(z_slices));
}

// view_z: positive distance along cam_forward (CPU assignment uses same).
fn cluster_base(coord: vec2<i32>, view_z: f32) -> u32 {
    let tiles_x = max(light_cluster[0u], 1u);
    let tiles_y = max(light_cluster[1u], 1u);
    let z_slices = max(light_cluster[2u], 1u);
    let tile = max(light_cluster[3u], 1u);
    let max_per = light_cluster[4u];
    let stride = 1u + max_per;
    let tx = min(u32(coord.x) / tile, tiles_x - 1u);
    let ty = min(u32(coord.y) / tile, tiles_y - 1u);
    let tz = min(cluster_z_slice(view_z), z_slices - 1u);
    let ci = ty * tiles_x * z_slices + tx * z_slices + tz;
    return CLUSTER_HDR + ci * stride;
}

fn cluster_count_at(coord: vec2<i32>, view_z: f32) -> u32 {
    if (!cluster_on()) { return 0u; }
    let max_per = light_cluster[4u];
    let base = cluster_base(coord, view_z);
    if (base >= arrayLength(&light_cluster)) { return 0u; }
    return min(light_cluster[base], max_per);
}

fn cluster_light_at(coord: vec2<i32>, view_z: f32, slot: u32) -> u32 {
    let base = cluster_base(coord, view_z);
    return light_cluster[base + 1u + slot];
}

// How many light slots to evaluate this pixel, and resolve slot → shade index.
// Cluster path: per-tile list. Fallback: global shade list (capped at 32).
fn light_eval_count(coord: vec2<i32>, view_z: f32, depth: f32) -> u32 {
    if (cluster_on() && depth > 0.0) {
        return cluster_count_at(coord, view_z);
    }
    return min(num_lights(), MAX_SHADE_DEFERRED);
}

fn light_eval_li(coord: vec2<i32>, view_z: f32, depth: f32, slot: u32) -> u32 {
    if (cluster_on() && depth > 0.0) {
        return cluster_light_at(coord, view_z, slot);
    }
    return slot;
}

// Visibility of point/spot light `li` at this pixel (1.0 = lit). li >= 8 → 1.0.
// Explicit channel select — no dynamic vector indexing (portability).
fn rt_light_vis(coord: vec2<i32>, li: u32) -> f32 {
    if (li >= 8u) { return 1.0; }
    let v = select(
        textureLoad(rt_shadow_vis_in, coord, 0),
        textureLoad(rt_shadow_vis_hi_in, coord, 0),
        li >= 4u,
    );
    let ch = li % 4u;
    if (ch == 0u) { return v.x; }
    if (ch == 1u) { return v.y; }
    if (ch == 2u) { return v.z; }
    return v.w;
}

// The AO_UPSAMPLE region tag is EXTRACTION-only, exactly like ENV_BRDF below:
// NO deferred variant ever strips it, and it must never appear in any *_EXCLUDE
// list. build_ssgi_wgsl() extracts it so the SSGI pass reproduces the IDENTICAL
// ssao_factor / bent normal / multi_bounce_ao terms the deferred pass applied —
// its energy delta subtracts exactly what deferred added, and duplicated copies
// of these four functions would drift apart silently (SSGI_PLAN.md §4.9).
// These stay referenced by the always-present PBR ambient/IBL terms, so the
// region boundary must contain the whole group, tags included.
// #region AO_UPSAMPLE
// Depth-aware bilinear upsample of the half-res SSAO factor at a full-res
// pixel. Half texel h is centered on full pixel 2h (the SSAO pass samples the
// top-left pixel of each 2x2 quad), so the bilinear position is coord * 0.5.
// Taps whose stored depth disagrees with this pixel's depth are rejected
// (silhouette edges must not bleed AO across depth-disjoint surfaces); if all
// four disagree — or the texture is the disabled white fill (depth 0) — the
// factor falls back to 1.0 (never darken on uncertainty).
fn ssao_factor(coord: vec2<i32>, depth: f32) -> f32 {
    let hdim = vec2<i32>(textureDimensions(ssao_in));
    let hf = vec2<f32>(coord) * 0.5;
    let base = vec2<i32>(floor(hf));
    let f = hf - vec2<f32>(base);
    let bw = vec4<f32>(
        (1.0 - f.x) * (1.0 - f.y),
        f.x * (1.0 - f.y),
        (1.0 - f.x) * f.y,
        f.x * f.y,
    );
    var offs = array<vec2<i32>, 4>(
        vec2<i32>(0, 0), vec2<i32>(1, 0), vec2<i32>(0, 1), vec2<i32>(1, 1),
    );
    var sum = 0.0;
    var wsum = 0.0;
    for (var i = 0; i < 4; i++) {
        let c = clamp(base + offs[i], vec2<i32>(0, 0), hdim - vec2<i32>(1, 1));
        let s = textureLoad(ssao_in, c, 0);
        if (s.g <= 1e-4) { continue; } // background texel or disabled white fill
        let rel = abs(s.g - depth) / max(depth, 1e-3);
        let wd = exp(-rel * rel * 400.0); // ~5% relative depth difference → weight e^-1
        let w = bw[i] * wd;
        sum += s.r * w;
        wsum += w;
    }
    if (wsum < 1e-4) { return 1.0; }
    return clamp(sum / wsum, 0.0, 1.0);
}

// ── GTAO bent normal (render/src/ssao.wgsl gtao_* tier packs the octahedral
// bent normal into b,a of the binding-38 texture; SSAO/off leave them 0). The
// bent normal is the average UNOCCLUDED direction — sampling the diffuse IBL
// (env_sh_irradiance) with it instead of the geometric normal gives real
// directional self-shading from the environment (the flat-cube fix). ──
// Kept OUTSIDE every #region (always-present PBR path, df_lite strip safe).
fn oct_decode_bent(e: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    if (n.z < 0.0) {
        let sx = select(-1.0, 1.0, n.x >= 0.0);
        let sy = select(-1.0, 1.0, n.y >= 0.0);
        n = vec3<f32>((1.0 - abs(n.y)) * sx, (1.0 - abs(n.x)) * sy, n.z);
    }
    return normalize(n);
}

// Depth-aware bilinear upsample of the half-res bent normal (same weighting as
// ssao_factor). b,a ≈ 0 ⇒ "no bent normal" (SSAO tier / off / background /
// non-raster pixels) ⇒ return the geometric normal — identical lighting to the
// pre-GTAO path. Only pixels the GTAO pass wrote carry a real bent normal.
fn ao_bent_normal(coord: vec2<i32>, depth: f32, geo_n: vec3<f32>) -> vec3<f32> {
    let hdim = vec2<i32>(textureDimensions(ssao_in));
    let hf = vec2<f32>(coord) * 0.5;
    let base = vec2<i32>(floor(hf));
    let f = hf - vec2<f32>(base);
    let bw = vec4<f32>(
        (1.0 - f.x) * (1.0 - f.y),
        f.x * (1.0 - f.y),
        (1.0 - f.x) * f.y,
        f.x * f.y,
    );
    var offs = array<vec2<i32>, 4>(
        vec2<i32>(0, 0), vec2<i32>(1, 0), vec2<i32>(0, 1), vec2<i32>(1, 1),
    );
    var sum = vec2<f32>(0.0);
    var wsum = 0.0;
    for (var i = 0; i < 4; i++) {
        let c = clamp(base + offs[i], vec2<i32>(0, 0), hdim - vec2<i32>(1, 1));
        let s = textureLoad(ssao_in, c, 0);
        if (s.g <= 1e-4) { continue; }
        if (abs(s.b) < 1e-5 && abs(s.a) < 1e-5) { continue; } // no bent normal here
        let rel = abs(s.g - depth) / max(depth, 1e-3);
        let wd = exp(-rel * rel * 400.0);
        let w = bw[i] * wd;
        sum += vec2<f32>(s.b, s.a) * w;
        wsum += w;
    }
    if (wsum < 1e-4) { return geo_n; } // SSAO/off/uncertain → geometric normal
    return oct_decode_bent(sum / wsum);
}

// Jimenez multi-bounce approximation ("Practical Realtime Strategies…"): a
// per-channel remap of the scalar visibility that recovers the light a single-
// bounce AO discards, tinted by albedo. Reduces to `vec3(vis)` for black
// albedo and to `1` when vis==1 (open surfaces unchanged).
fn multi_bounce_ao(vis: f32, albedo: vec3<f32>) -> vec3<f32> {
    let a = 2.0404 * albedo - vec3<f32>(0.3324);
    let b = -4.7951 * albedo + vec3<f32>(0.6417);
    let c = 2.7552 * albedo + vec3<f32>(0.6903);
    let x = vec3<f32>(vis);
    return max(x, ((x * a + b) * x + c) * x);
}

// Specular occlusion (Lagarde/Frostbite): the horizon AO also occludes the
// specular lobe, scaled by roughness and N·V — stops metals over-brightening at
// grazing angles inside occluded areas. Returns 1 for open surfaces (ao==1).
fn specular_occlusion(ndv: f32, ao: f32, rough: f32) -> f32 {
    return clamp(pow(ndv + ao, exp2(-16.0 * rough - 1.0)) - 1.0 + ao, 0.0, 1.0);
}
// #endregion AO_UPSAMPLE

// ── Split-sum BRDF LUT (deferred binding 39, generated once at startup by
// env_ibl.rs::generate_brdf_lut; row = roughness, column = N·V) ──
// Kept OUTSIDE every #region: referenced by the always-present PBR IBL terms
// (df_lite included). While config.env_lut_res == 0 (LUT worker still running)
// the analytic Karis approximation answers instead — never a wrong/black window.
// The ENV_BRDF region tag is EXTRACTION-only (the PT-tri precedent): no deferred
// variant ever strips it; process_post_process_wgsl() extracts it so pass_ssr
// computes the IDENTICAL split-sum envelope the deferred applied (exact
// energy-conserving SSR↔IBL-specular swap, no duplicated math).
// #region ENV_BRDF
@group(0) @binding(39) var<storage, read> brdf_lut: array<vec2<f32>>;

fn env_brdf(ndv_in: f32, rough_in: f32) -> vec2<f32> {
    let ndv = clamp(ndv_in, 0.0, 1.0);
    let rough = clamp(rough_in, 0.0, 1.0);
    let res = config.env_lut_res;
    if (res == 0u) {
        // Karis mobile split-sum approximation (EnvBRDFApprox).
        let c0 = vec4(-1.0, -0.0275, -0.572, 0.022);
        let c1 = vec4(1.0, 0.0425, 1.04, -0.04);
        let r4 = vec4(rough) * c0 + c1;
        let a004 = min(r4.x * r4.x, exp2(-9.28 * ndv)) * r4.x + r4.y;
        return vec2(-1.04 * a004 + r4.z, 1.04 * a004 + r4.w);
    }
    // Manual bilinear fetch (storage buffer — no sampler).
    let fx = ndv * f32(res) - 0.5;
    let fy = rough * f32(res) - 0.5;
    let x0 = u32(clamp(floor(fx), 0.0, f32(res - 1u)));
    let y0 = u32(clamp(floor(fy), 0.0, f32(res - 1u)));
    let x1 = min(x0 + 1u, res - 1u);
    let y1 = min(y0 + 1u, res - 1u);
    let sx = clamp(fx - f32(x0), 0.0, 1.0);
    let sy = clamp(fy - f32(y0), 0.0, 1.0);
    let v00 = brdf_lut[y0 * res + x0];
    let v10 = brdf_lut[y0 * res + x1];
    let v01 = brdf_lut[y1 * res + x0];
    let v11 = brdf_lut[y1 * res + x1];
    return mix(mix(v00, v10, sx), mix(v01, v11, sx), sy);
}
// #endregion ENV_BRDF

// ── Normalized direct-light BRDF (RASTER-DIRECT-UNNORMALIZED, 2026-10-13) ──
// The deferred direct terms used `albedo·E·N·L` (no 1/π) and `F·D·N·L` (no Smith G, no
// 1/(4·N·L·N·V)) while the raster IBL (SH E/π, split-sum) and the path tracer are normalized —
// sun-lit raster read π× (diffuse) and 4–9× (metal) brighter than PT (AZRA_PARITY gate). These
// are the path tracer's EXACT definitions (pathtrace_tri_entry.wgsl): `pt_fresnel_schlick`,
// `pt_ggx_D` (GGX/Trowbridge-Reitz, alpha = roughness²), `pt_smith_G2` (height-correlated
// Smith, Heitz 2014) and `pt_eval_f_cos` (kd = (1 − F(v·h))·(1 − metal), Lambert albedo/π).
// Light intensity is IRRADIANCE at normal incidence (a directional "W/m²"): a white Lambert
// card facing a sun of intensity π has exit radiance 1.0. Kept OUTSIDE every #region
// (referenced by the always-present PBR loop and by the Lit branch).
fn direct_fresnel(cos_t: f32, f0: vec3<f32>) -> vec3<f32> {
    let t = clamp(1.0 - cos_t, 0.0, 1.0);
    let t2 = t * t;
    return f0 + (vec3(1.0) - f0) * (t2 * t2 * t);
}
fn direct_ggx_d(ndh: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * d * d + 1e-7);
}
fn direct_smith_g2(ndl: f32, ndv: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = ndl * sqrt(a2 + (1.0 - a2) * ndv * ndv);
    let gl = ndv * sqrt(a2 + (1.0 - a2) * ndl * ndl);
    return 2.0 * ndl * ndv / max(gv + gl, 1e-7);
}
// GGX alpha for a perceptual roughness — the PT's `max(rough², 0.001)`.
fn direct_alpha(rough: f32) -> f32 {
    return max(rough * rough, 0.001);
}
// Specular BRDF·cos for unit irradiance from direction `l`: F·D·G / (4·N·L·N·V) · N·L.
// `a` may be widened by a sphere light's subtended angle (Karis) by the caller.
fn direct_spec(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, f0: vec3<f32>, a: f32) -> vec3<f32> {
    let ndl = dot(n, l);
    let ndv = dot(n, v);
    if (ndl <= 0.0 || ndv <= 0.0) { return vec3(0.0); }
    let h = normalize(v + l);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);
    let F = direct_fresnel(vdh, f0);
    return F * (direct_ggx_d(ndh, a) * direct_smith_g2(ndl, ndv, a) / (4.0 * ndv + 1e-7));
}
// Diffuse BRDF·cos WITHOUT albedo (the caller multiplies albedo once, after summing lights):
// (1 − F(v·h))·(1 − metal)/π · N·L.
fn direct_diffuse(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, f0: vec3<f32>, metal: f32) -> vec3<f32> {
    let ndl = max(dot(n, l), 0.0);
    if (ndl <= 0.0) { return vec3(0.0); }
    // Diffuse deliberately keeps no N·V gate (unlike the PT's joint early-out): a shading
    // normal can face slightly away from the eye at silhouettes; that must not black out the
    // Lambert term. v + l ≈ 0 only there — guard the half-vector.
    let hv = v + l;
    let vdh = select(0.0, max(dot(v, hv * inverseSqrt(max(dot(hv, hv), 1e-12))), 0.0), dot(hv, hv) > 1e-8);
    return (vec3(1.0) - direct_fresnel(vdh, f0)) * ((1.0 - metal) * ndl / 3.14159265);
}

@compute @workgroup_size(8, 8, 1)
fn deferred_shade(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let tex_dim_full = textureDimensions(output_texture);
    // Phase B1 (PBR_REFINE_HARDENING_PLAN): probe mini-frames encode the square
    // side in cam_right.w (>0). Raster wrote top-left s×s with FOV 90° / aspect 1;
    // reconstruct rays with s (not full viewport) so UV matches that viewport.
    let probe_side = u32(config.cam_right.w);
    let tex_dim = select(
        tex_dim_full,
        vec2<u32>(min(probe_side, tex_dim_full.x), min(probe_side, tex_dim_full.y)),
        probe_side > 0u,
    );
    if (global_id.x >= tex_dim.x || global_id.y >= tex_dim.y) { return; }
    let coord = vec2<i32>(i32(global_id.x), i32(global_id.y));

    let gbuf_n = textureLoad(gbuf_normal_in, coord, 0);
    let gbuf_m = textureLoad(gbuf_material_in, coord, 0);
    let gbuf_a = textureLoad(gbuf_albedo_in, coord, 0);

    let normal = gbuf_n.xyz;
    let depth = gbuf_n.w;

    // ── Probe PREVIEW balls ("Show Probes", post_flags bit 2048) ──
    // Two ray-traced spheres at the probe: a CHROME ball (raw cube sample — what
    // reflections receive) and an IRRADIANCE ball above it (SH eval × strength —
    // what the ambient receives). Drawn before shading so they cover background
    // pixels too; depth-tested against the G-buffer. Probe bake frames mask the
    // bit out so the balls can never capture themselves. Flat 0.25 gray until
    // the first bake round lands (placement preview).
    if ((config.post_flags & 2048u) != 0u) {
        let pv_w = f32(tex_dim.x);
        let pv_h = f32(tex_dim.y);
        let pv_uvx = (f32(global_id.x) / pv_w) * 2.0 - 1.0;
        let pv_uvy = -((f32(global_id.y) / pv_h) * 2.0 - 1.0);
        let pv_ray = camera_ray(pv_uvx, pv_uvy, pv_w / pv_h);
        let pv_c = probe_params_pv.center_enabled.xyz;
        let pv_e = probe_params_pv.extent_mips.xyz;
        let pv_r = clamp(0.10 * min(pv_e.x, min(pv_e.y, pv_e.z)), 0.1, 0.5);
        let baked = probe_params_pv.center_enabled.w >= 0.5;
        var pv_col = vec3<f32>(-1.0); // sentinel: no ball hit
        let pv_t = probe_ball_hit(pv_ray[0], pv_ray[1], pv_c, pv_r);
        if (pv_t > 0.0 && (depth <= 0.001 || pv_t < depth)) {
            let pn = normalize(pv_ray[0] + pv_ray[1] * pv_t - pv_c);
            if (baked) {
                pv_col = textureSampleLevel(probe_cube_pv, probe_samp_pv, reflect(pv_ray[1], pn), 0.0).rgb;
            } else {
                pv_col = vec3<f32>(0.25);
            }
        }
        let pv_c2 = pv_c + vec3<f32>(0.0, pv_r * 2.6, 0.0);
        let pv_t2 = probe_ball_hit(pv_ray[0], pv_ray[1], pv_c2, pv_r);
        if (pv_t2 > 0.0 && (depth <= 0.001 || pv_t2 < depth)
            && (pv_col.x < 0.0 || pv_t2 < pv_t)) {
            let pn2 = normalize(pv_ray[0] + pv_ray[1] * pv_t2 - pv_c2);
            if (probe_sh[9].w >= 0.5) {
                pv_col = probe_sh_irradiance(pn2) * probe_sh[9].x;
            } else {
                pv_col = vec3<f32>(0.25);
            }
        }
        if (pv_col.x >= 0.0) {
            textureStore(output_texture, coord, vec4<f32>(pv_col, 1.0));
            return;
        }
    }

    // Bilateral blur on shadow/AO: soft interiors, hard geometric edges.
    // Samples neighbors weighted by spatial distance AND normal similarity.
    // Pixels with different normals (edges) reject blur → preserves hard silhouettes.
    var shadow_ao = gbuf_m.z;
    let blur_r = config.shadow_blur_radius;
    if (blur_r > 0u && depth > 0.001) {
        let ri = i32(blur_r);
        let sigma_s = f32(blur_r);
        var sum = 0.0;
        var wsum = 0.0;
        for (var dy = -ri; dy <= ri; dy++) {
            for (var dx = -ri; dx <= ri; dx++) {
                let sc = coord + vec2<i32>(dx, dy);
                if (sc.x < 0 || sc.y < 0 || sc.x >= i32(tex_dim.x) || sc.y >= i32(tex_dim.y)) { continue; }
                let sn = textureLoad(gbuf_normal_in, sc, 0);
                let sm = textureLoad(gbuf_material_in, sc, 0).z;
                // Spatial Gaussian
                let d2 = f32(dx * dx + dy * dy);
                let ws = exp(-d2 / (2.0 * sigma_s * sigma_s));
                // Normal edge rejection (hard edges preserved)
                let ndot = max(dot(normal, sn.xyz), 0.0);
                let wn = exp(-(1.0 - ndot) * 4.0); // steep falloff at normal discontinuity
                // Depth edge rejection
                let dd = abs(depth - sn.w) / max(abs(depth), 0.01);
                let wd = select(0.0, 1.0, dd < 0.1);
                let w = ws * wn * wd;
                sum += sm * w;
                wsum += w;
            }
        }
        if (wsum > 0.001) { shadow_ao = sum / wsum; }
    }

    // Basic G-buffer material (used for early-out paths: bg, floor, VFX)
    let gbuf_albedo = gbuf_a.rgb;
    let gbuf_rough = gbuf_m.x;
    let gbuf_metal = gbuf_m.y;
    let gbuf_opacity_em = gbuf_m.w;
    let flags = gbuf_a.a;

    // Background (flags ≈ 0)
    if (flags < 0.1) {
        if (config.view_mode == 1u) {
            // MatCap: always use user bg gradient (already baked into gbuf by raymarch)
            textureStore(output_texture, coord, vec4(gbuf_albedo, 0.0));
        } else if (config.env_width > 0u) {
            let x = f32(global_id.x); let y = f32(global_id.y);
            let w = f32(tex_dim.x); let h = f32(tex_dim.y);
            let uv_x = (x / w) * 2.0 - 1.0;
            let uv_y = -((y / h) * 2.0 - 1.0);
            let aspect = w / h;
            let fs = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
            let rd = normalize(config.cam_forward.xyz + config.cam_right.xyz * (uv_x * aspect * fs) + config.cam_up.xyz * (uv_y * fs));
            // The PHYSICAL sky always lands in the HDR buffer, even when the plate is hidden.
            // This buffer is not merely "what you see": SSR samples it for screen-space hits,
            // the reflection probe captures it, bloom extracts from it. Writing the flat colour
            // HERE made every reflection that lands on a sky pixel return the flat colour — and
            // since SSR runs only when idle, reflections survived an orbit and died on release
            // (owner 2026-08-09 "reflection appear only when i rotate camera").
            //
            // Hiding is therefore a MARK, not a substitution: alpha 0 flags the pixel as
            // "background, do not expose / do not post-process", and `pass_tonemap` swaps in
            // the flat colour at display time. Alpha 1 (shown) keeps the legacy exposure path.
            let bg_a = select(1.0, 0.0, env_bg_hidden());
            textureStore(output_texture, coord, vec4(sample_env(rd), bg_a));
        } else {
            // User bg color: alpha=0 to skip exposure+ACES in tonemap
            textureStore(output_texture, coord, vec4(gbuf_albedo, 0.0));
        }
        return;
    }

    // Floor grid (flags ≈ 0.5) — already pre-shaded
    // alpha=0 → tonemap treats as passthrough (no exposure/ACES) since grid colors are final
    if (flags < 0.58) {
        textureStore(output_texture, coord, vec4(gbuf_albedo, 0.0));
        return;
    }

    // Ghosted body (flags ≈ 0.61) — dimmed spatial context for Focus Mode
    if (flags < 0.63) {
        let luma = dot(gbuf_albedo, vec3(0.2126, 0.7152, 0.0722));
        let ghost_color = vec3(luma) * 0.3;
        textureStore(output_texture, coord, vec4(ghost_color, 1.0));
        return;
    }

    // DC wire edge (flags ≈ 0.74) + screen-space thickness dilation.
    // material.x = thickness (px), material.y = opacity. Connected to DC Wire Settings.
    {
        var wire_hit = false;
        var wire_col = gbuf_albedo;
        var wire_op = 1.0;
        if (flags > 0.73 && flags < 0.76) {
            wire_hit = true;
            wire_op = clamp(gbuf_m.y, 0.0, 1.0);
        } else if (config.dc_wire_px > 0.0) {
            // Dilate: if a nearby wire pixel's thickness reaches this pixel, paint it.
            // Gated on config.dc_wire_px (0 when the wire overlay is off — zero cost),
            // and the search radius is bounded by the actual thickness, not a fixed 12.
            let max_r = min(i32(ceil(config.dc_wire_px * 0.5 + 0.5)), 12);
            var best_d = 1e9;
            for (var dy = -max_r; dy <= max_r; dy++) {
                for (var dx = -max_r; dx <= max_r; dx++) {
                    if (dx == 0 && dy == 0) { continue; }
                    let d = sqrt(f32(dx * dx + dy * dy));
                    if (d > f32(max_r) || d >= best_d) { continue; }
                    let nc = coord + vec2<i32>(dx, dy);
                    if (nc.x < 0 || nc.y < 0 || nc.x >= i32(tex_dim.x) || nc.y >= i32(tex_dim.y)) { continue; }
                    let na = textureLoad(gbuf_albedo_in, nc, 0);
                    if (na.a > 0.73 && na.a < 0.76) {
                        let nm = textureLoad(gbuf_material_in, nc, 0);
                        let thick = max(nm.x, 0.0);
                        if (thick > 0.001 && d <= thick * 0.5 + 0.5) {
                            wire_hit = true;
                            wire_col = na.rgb;
                            wire_op = clamp(nm.y, 0.0, 1.0);
                            best_d = d;
                        }
                    }
                }
            }
        }
        if (wire_hit && wire_op > 0.001) {
            // alpha=0 → tonemap passthrough (linear UI colour). Opacity fades toward black.
            let out_c = wire_col * wire_op;
            textureStore(output_texture, coord, vec4(out_c, 0.0));
            return;
        }
    }

    // VFX/Unlit (flags ≈ 0.68 or 0.88) — pure emission, no PBR
    let is_vfx = (flags > 0.63 && flags < 0.73) || (flags > 0.83 && flags < 0.93);
    if (is_vfx) {
        let vfx_em_add = max(gbuf_opacity_em - 1.0, 0.0);
        let vfx_opacity = min(gbuf_opacity_em, 1.0);
        var vfx_color = gbuf_albedo * max(vfx_em_add, 1.0);
        let vfx_selected = flags > 0.83;
        if (vfx_selected && config.sel_rim_enabled != 0u) {
            // Selected coverage edge (flags < 0.9 = not selected / bg), same as main PBR rim.
            var vfx_edge = false;
            let vfx_ow = max(i32(config.sel_rim_power), 1);
            let vfx_bx = i32(tex_dim.x); let vfx_by = i32(tex_dim.y);
            for (var d = 1; d <= 10; d++) {
                if (d > vfx_ow || vfx_edge) { break; }
                let L = coord + vec2<i32>(-d, 0); let R = coord + vec2<i32>(d, 0);
                let U = coord + vec2<i32>(0, -d); let Dwn = coord + vec2<i32>(0, d);
                if (L.x < 0 || textureLoad(gbuf_albedo_in, L, 0).a < 0.9) { vfx_edge = true; }
                if (!vfx_edge && (R.x >= vfx_bx || textureLoad(gbuf_albedo_in, R, 0).a < 0.9)) { vfx_edge = true; }
                if (!vfx_edge && (U.y < 0 || textureLoad(gbuf_albedo_in, U, 0).a < 0.9)) { vfx_edge = true; }
                if (!vfx_edge && (Dwn.y >= vfx_by || textureLoad(gbuf_albedo_in, Dwn, 0).a < 0.9)) { vfx_edge = true; }
            }
            if (vfx_edge) { vfx_color = config.sel_rim_color; }
        }
        // Alpha-blend
        if (vfx_opacity < 0.999) {
            var bg = bg_gradient_dithered(f32(global_id.x), f32(global_id.y), f32(tex_dim.y));
            if (config.env_width > 0u && !env_bg_hidden()) {
                let bx2 = f32(global_id.x); let by2 = f32(global_id.y);
                let fs2 = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
                let brd2 = normalize(config.cam_forward.xyz + config.cam_right.xyz * ((bx2/f32(tex_dim.x))*2.0-1.0) * f32(tex_dim.x)/f32(tex_dim.y) * fs2 + config.cam_up.xyz * (-((by2/f32(tex_dim.y))*2.0-1.0)) * fs2);
                bg = sample_env(brd2);
            }
            vfx_color = mix(bg, vfx_color, vfx_opacity);
        }
        textureStore(output_texture, coord, vec4(vfx_color, 1.0));
        return;
    }

    // MatCap early return — fastest possible path: normal → matcap lookup → done.
    // No shadows, no AO, no DNA, no PBR, no multi-light loop. Just shape visualization.
    if (config.view_mode == 1u) {
        // Rotate camera basis by matcap_rotation (radians, around view Z)
        let mc_cos = cos(config.matcap_rotation);
        let mc_sin = sin(config.matcap_rotation);
        let mc_right = config.cam_right.xyz * mc_cos + config.cam_up.xyz * mc_sin;
        let mc_up    = config.cam_up.xyz * mc_cos - config.cam_right.xyz * mc_sin;
        let mc = matcap_color(normal, mc_right, mc_up);
        var mc_out = mc;

        // Material-colour overlay ("MatCap + my materials"): multiply the matcap by the
        // surface's own base colour. `gbuf_albedo` is what the RASTER G-buffer pass baked
        // for this pixel — the material's base colour with its albedo map, noise and
        // curvature layers already applied (render/gbuffer.wgsl fs_gbuffer, which is
        // view-mode agnostic), and it is LINEAR, same space as the pre-linearized matcap
        // sample. So the product is albedo × shading: the PBR model minus the lights.
        // Every displayed body is a raster pixel here (Display Invariant: SDF renders from
        // its baked DC mesh), so there is no representation this silently misses.
        // config.matcap_tint == 0 → untouched, bit-identical to the pre-overlay path.
        if (config.matcap_tint > 0.0) {
            mc_out = mix(mc_out, mc_out * gbuf_albedo, config.matcap_tint);
        }

        // Screen-space normal-edge highlight (draws dark creases/silhouettes)
        if (config.matcap_edge_enabled != 0u) {
            let mc_bx = i32(tex_dim.x); let mc_by = i32(tex_dim.y);
            let step = max(i32(config.matcap_edge_thickness), 1);
            // Sample center normal + 4 cardinal neighbors from GBuffer
            let nL = textureLoad(gbuf_normal_in, clamp(coord + vec2<i32>(-step, 0), vec2<i32>(0), vec2<i32>(mc_bx-1, mc_by-1)), 0).xyz;
            let nR = textureLoad(gbuf_normal_in, clamp(coord + vec2<i32>( step, 0), vec2<i32>(0), vec2<i32>(mc_bx-1, mc_by-1)), 0).xyz;
            let nU = textureLoad(gbuf_normal_in, clamp(coord + vec2<i32>(0, -step), vec2<i32>(0), vec2<i32>(mc_bx-1, mc_by-1)), 0).xyz;
            let nD = textureLoad(gbuf_normal_in, clamp(coord + vec2<i32>(0,  step), vec2<i32>(0), vec2<i32>(mc_bx-1, mc_by-1)), 0).xyz;
            // Max angular difference among pairs (background has zero normals → silhouette edge)
            let lenL = length(nL); let lenR = length(nR); let lenU = length(nU); let lenD = length(nD);
            let d_horiz = select(1.0 - dot(nL / lenL, nR / lenR), 2.0, lenL < 0.01 || lenR < 0.01);
            let d_vert  = select(1.0 - dot(nU / lenU, nD / lenD), 2.0, lenU < 0.01 || lenD < 0.01);
            let edge_val = max(d_horiz, d_vert);
            let thr = config.matcap_edge_threshold;
            if (edge_val > thr) {
                let blend = saturate((edge_val - thr) / max(thr, 0.01));
                mc_out = mix(mc_out, config.matcap_edge_color, blend);
            }
        }

        // Selection outline (keep for UX)
        let mc_selected = flags > 0.9;
        if (mc_selected && config.sel_rim_enabled != 0u) {
            var mc_sel_edge = false;
            let mc_ow = max(i32(config.sel_rim_power), 1);
            let mc_bx2 = i32(tex_dim.x); let mc_by2 = i32(tex_dim.y);
            for (var d = 1; d <= 10; d++) {
                if (d > mc_ow || mc_sel_edge) { break; }
                let L = coord + vec2<i32>(-d, 0); let R = coord + vec2<i32>(d, 0);
                let U = coord + vec2<i32>(0, -d); let Dwn = coord + vec2<i32>(0, d);
                if (L.x < 0 || textureLoad(gbuf_albedo_in, L, 0).a < 0.9) { mc_sel_edge = true; }
                if (!mc_sel_edge && (R.x >= mc_bx2 || textureLoad(gbuf_albedo_in, R, 0).a < 0.9)) { mc_sel_edge = true; }
                if (!mc_sel_edge && (U.y < 0 || textureLoad(gbuf_albedo_in, U, 0).a < 0.9)) { mc_sel_edge = true; }
                if (!mc_sel_edge && (Dwn.y >= mc_by2 || textureLoad(gbuf_albedo_in, Dwn, 0).a < 0.9)) { mc_sel_edge = true; }
            }
            if (mc_sel_edge) { mc_out = config.sel_rim_color; }
        }
        // alpha=0 → tonemap treats as passthrough (saturate + gamma, no exposure/ACES)
        textureStore(output_texture, coord, vec4(mc_out, 0.0));
        return;
    }

    // #region DEFERRED_MATERIAL
    // ONE surface-material evaluation for every material-consuming view (2026-10-13 cold-compile
    // fix). Lit (6) and PBR / Solid+HDR / RT (0/3/4) used to each carry their own dna → node →
    // [blended] → plain chain; the driver inlines every call, so DF-full held two full copies of
    // the material stack (plus DNA's SDF probes) and its cold compile took minutes. The views are
    // mutually exclusive (Lit returns early), the inputs below are the SAME derivations both
    // branches used (camera ray at gbuf depth, raster test, stale-sid sentinel), and the chain
    // and gates are unchanged: blended stays PBR-only at quality >= 2. Both branches read
    // `surf_mat` / `surf_mat_valid`; LIT_BRANCH and DEFERRED_MATERIAL are stripped together
    // (df_lite) or kept together (DF-full), so the cross-region reference is always resolved.
    var surf_mat: MatResult;
    var surf_mat_valid = false;
    {
        let sm_lit = config.view_mode == 6u;
        let sm_pbr = config.view_mode == 0u || config.view_mode == 3u || config.view_mode == 4u;
        let sm_w = f32(tex_dim.x); let sm_h = f32(tex_dim.y);
        let sm_uv_x = (f32(global_id.x) / sm_w) * 2.0 - 1.0;
        let sm_uv_y = -((f32(global_id.y) / sm_h) * 2.0 - 1.0);
        let sm_ray = camera_ray(sm_uv_x, sm_uv_y, sm_w / sm_h);
        let sm_p = sm_ray[0] + sm_ray[1] * depth;
        let sm_vd = normalize(sm_ray[0] - sm_p);
        let sm_snap = textureLoad(raster_depth_in, coord, 0).w;
        let sm_is_raster = sm_snap > 0.0 && abs(sm_snap - depth) < 0.05;
        let sm_sid = select(textureLoad(gbuf_surface_id_in, coord, 0).r, 0xFFFFFFFFu, sm_is_raster);
        if ((sm_lit || sm_pbr) && config.num_primitives > 0u && sm_sid < config.num_primitives && !sm_is_raster) {
            if (dna_rule_count(sm_sid) > 0u) {
                surf_mat = eval_material_dna(sm_sid, sm_p, normal, sm_vd);
            } else if (prim_has_node_graph(sm_sid)) {
                surf_mat = eval_node_material(sm_sid, sm_p, normal);
            } else if (sm_pbr && config.render_quality >= 2u) {
                surf_mat = eval_material_blended(sm_p, normal);
            } else {
                surf_mat = eval_material(sm_sid, sm_p, normal);
            }
            surf_mat_valid = true;
        }
    }
    // #endregion DEFERRED_MATERIAL

    // #region LIT_BRANCH
    // Lit mode (view_mode==6) — direct lighting with materials, no reflections/IBL/fog/bloom.
    // Faster than PBR but shows materials + shadows. Good for modeling with context.
    if (config.view_mode == 6u) {
        let lit_selected = flags > 0.9;
        let lit_sid_raw = textureLoad(gbuf_surface_id_in, coord, 0).r;
        let lit_x = f32(global_id.x); let lit_y = f32(global_id.y);
        let lit_w = f32(tex_dim.x); let lit_h = f32(tex_dim.y);
        let lit_uv_x = (lit_x / lit_w) * 2.0 - 1.0;
        let lit_uv_y = -((lit_y / lit_h) * 2.0 - 1.0);
        let lit_aspect = lit_w / lit_h;
        let lit_ray = camera_ray(lit_uv_x, lit_uv_y, lit_aspect);
        let lit_cam = lit_ray[0];
        let lit_rd = lit_ray[1];
        let lit_p = lit_cam + lit_rd * depth;
        let lit_vd = normalize(lit_cam - lit_p);

        // Material evaluation (same as PBR path but always evaluates)
        // Skip for raster-sourced pixels — material already baked in G-buffer
        let lit_raster_snap = textureLoad(raster_depth_in, coord, 0).w;
        let lit_is_raster = lit_raster_snap > 0.0 && abs(lit_raster_snap - depth) < 0.05;
        // Raster pixels carry a STALE surface_id (the compute raymarch early-returns without
        // writing it), so light_linked() would reject every light → black. Route them through
        // the existing "no SDF prim / all lights" sentinel (0xFFFFFFFFu) so all lights apply.
        let lit_sid = select(lit_sid_raw, 0xFFFFFFFFu, lit_is_raster);
        var lit_albedo = gbuf_a.rgb;
        var lit_rough = gbuf_m.x;
        var lit_metal = gbuf_m.y;
        var lit_opacity = min(gbuf_m.w, 1.0);
        var lit_emission = max(gbuf_m.w - 1.0, 0.0);
        // Same gate as before (sid valid, not raster) — evaluated once above as `surf_mat`.
        if (surf_mat_valid) {
            let mat = surf_mat;
            lit_albedo = mat.color;
            lit_rough = mat.roughness;
            lit_metal = mat.metallic;
            lit_opacity = mat.opacity;
            let em_lum = length(mat.emission);
            lit_emission = em_lum;
            if (em_lum > 0.001) {
                let em_w = clamp(em_lum / (em_lum + 1.0), 0.0, 1.0);
                lit_albedo = mix(lit_albedo, mat.emission / max(em_lum, 0.001), em_w);
            }
        }

        // Shadow + AO (separated: shadow for direct, AO for ambient)
        let lit_key_dir = safe_dir(config.light_dir.xyz);
        var lit_shadow = shadow_ao;
        var lit_ao_only = shadow_ao;
        // Raster pixels: the baked gbuf_material.z is a directional shadow factor, not AO —
        // it must never dim the ambient term (same split as the PBR branch below).
        if (lit_is_raster) {
            lit_ao_only = 1.0;
        }
        // Raster pixels already carry a baked shadow-map value in shadow_ao; don't recompute
        // it with SDF queries (soft_shadow/calc_ao march the prim buffer, which is empty for a
        // voxel scene → degenerate → black). SDF pixels still recompute below.
        // ANALYTICAL SUN SHADOW/AO RECOMPUTE — REMOVED 2026-08-07 (owner decision;
        // same rationale as the PBR branch's SHADOW_AO comment: field shadows of
        // geometry that is no longer displayed). Lit march pixels keep shadow_ao
        // as written (1.0) and get the RT sun swap below when active.
        // Sun RT swap — 2026-08-07 gap fix: this Lit early-return branch never applied
        // the 2026-07-28 sun swap, so full builds kept CSM for the sun in Lit while
        // PBR traced (lite builds strip LIT_BRANCH and were covered by accident).
        // Includes the refine AUTO SUN (post_flags bit 512) — same condition as the
        // PBR branch and the ray pass. Contact shadow is skipped when active (the
        // traced visibility already resolves contact-scale occlusion).
        let lit_sun_rt = config.rt_shadows_on != 0u && find_sun_index() < 8u
            && (light_rt_enabled(find_sun_index()) || (config.post_flags & 512u) != 0u)
            && depth > 0.0 && config.interaction_no_shadows == 0u;
        if (depth > 0.0 && config.interaction_no_shadows == 0u && !lit_sun_rt) {
            lit_shadow *= contact_shadow(lit_p, normal, lit_key_dir);
        }
        if (lit_sun_rt) {
            lit_shadow = rt_light_vis(coord, find_sun_index()) * lit_ao_only;
        }

        // Multi-light direct lighting (no IBL, no SDF reflections)
        // P3.1 Phase D: cluster list when enabled, else global shade list (≤32).
        let lit_view_z = max(dot(lit_p - config.cam_pos.xyz, config.cam_forward.xyz), 0.01);
        let lit_n_lights = light_eval_count(coord, lit_view_z, depth);
        var lit_diff = vec3(0.0);
        var lit_spec = vec3(0.0);
        // Flat ambient RGB (strength × tint). config.light_color.w is legacy scalar strength.
        var lit_ambient = vec3(config.light_color.w);
        // Normalized direct BRDF (direct_* helpers — the PT's exact D/G/F, see the PBR branch).
        let lit_f0 = mix(vec3(0.04), lit_albedo, lit_metal);
        let lit_a = direct_alpha(lit_rough);
        for (var slot = 0u; slot < lit_n_lights; slot++) {
            let li = light_eval_li(coord, lit_view_z, depth, slot);
            if (!light_linked(lit_sid, li)) { continue; }
            let lt = light_type_v(li);
            let l_int = light_intensity_v(li);
            if (l_int < 1e-5) {
                if (li == 0u) { lit_ambient = light_ambient_rgb_v(li); }
                continue;
            }
            let l_col = light_color_v(li);
            var l_dir: vec3<f32>;
            var atten = 1.0;
            var lit_to_light = vec3(0.0); // unnormalized (sphere-light spec below)
            var lit_l_dist = 1e9;
            if (lt == 0u) {
                l_dir = light_dir_v(li);
            } else {
                let l_pos = light_pos(li);
                lit_to_light = l_pos - lit_p;
                lit_l_dist = length(lit_to_light);
                l_dir = lit_to_light / max(lit_l_dist, 0.001);
                atten = point_atten(lit_l_dist, light_range_v(li));
                if (lt == 2u) {
                    let spot_dir = light_dir_v(li);
                    let cos_a = dot(-l_dir, spot_dir);
                    atten *= spot_atten(cos_a, light_spot_inner_cos(li), light_spot_outer_cos(li));
                }
                if (atten < 0.01) { continue; }
            }
            // Per-light shadow: RT or nothing. The analytical SDF `soft_shadow`
            // fallback was REMOVED 2026-08-07 (owner order, completing the analytical
            // retirement): it marched the SDF FIELD — geometry that is no longer what
            // is displayed, blind to poly/voxel bodies. A point/spot light casts
            // shadows ONLY via its per-light RT toggle now. Do not reintroduce.
            var l_shd = lit_shadow;
            if (config.rt_shadows_on != 0u && light_rt_enabled(li) && lt != 0u && li < 8u && depth > 0.0 && config.interaction_no_shadows == 0u) {
                l_shd = rt_light_vis(coord, li) * lit_ao_only;
            } else if (lt != 0u) {
                // Non-RT (or beyond-pack) point/spot: unshadowed, ambient-occluded only.
                l_shd = lit_ao_only;
            }
            let l_e = l_col * (l_int * atten * l_shd); // irradiance at normal incidence
            lit_diff += l_e * direct_diffuse(normal, lit_vd, l_dir, lit_f0, lit_metal);
            // Sphere-light specular (Karis representative point — see the PBR branch).
            var s_dir = l_dir;
            var s_a = lit_a;
            let lr_w = light_rt_radius(li);
            if (lt != 0u && lr_w > 1e-4) {
                let r_vec = reflect(-lit_vd, normal);
                let c2r = lit_to_light - r_vec * dot(lit_to_light, r_vec);
                let closest = lit_to_light - c2r * saturate(lr_w / max(length(c2r), 1e-4));
                s_dir = normalize(closest);
                s_a = saturate(lit_a + lr_w / (2.0 * max(lit_l_dist, 1e-3)));
            }
            lit_spec += l_e * direct_spec(normal, lit_vd, s_dir, lit_f0, s_a);
            if (li == 0u) { lit_ambient = light_ambient_rgb_v(li); }
        }
        // No visible lights → no flat ambient. (Do not inject env_brightness fill:
        // that kept the cube lit after the sun was eye-hidden. Real IBL still runs
        // when env_width > 0 via the split-sum path; this is only the sun Ambient term.)
        if (lit_n_lights == 0u) { lit_ambient = vec3(0.0); }
        // Fill light from primary directional (skip when no lights — stale config)
        if (lit_n_lights > 0u) {
            let lit_fill = normalize(vec3(-lit_key_dir.x * 0.5, abs(lit_key_dir.y) * 0.6 + 0.3, -lit_key_dir.z * 0.5));
            // Same procedural fill as the PBR branch — same control + same AO gate
            // (owner "we look to have 2 shadow paths" 2026-08-07, see the PBR fill).
            // Lambert-normalized like every direct light ((1 − metal)/π; no F — a fake bounce).
            lit_diff += config.light_color.xyz * config.light_dir.w * max(dot(normal, lit_fill), 0.0)
                      * config.sym_origin.w * ((1.0 - lit_metal) / 3.14159265) * lit_ao_only;
        }
        // SSAO: ambient-only occlusion for raster pixels (never the direct
        // terms — a contact shadow blocks the sky, not the sun/point lights).
        var lit_ssao = 1.0;
        if (lit_is_raster) { lit_ssao = ssao_factor(coord, depth); }
        // Flat ambient is cut to 12% when an environment exists — IBL REPLACES it below.
        // Without this the two terms double-count and everything washes out (the same
        // ambient double-count class as the sun-Ambient fix). Mirrors the PBR branch's
        // `select(ambient_flat, ambient_flat * 0.12, config.env_width > 0u)`.
        let lit_amb_flat = lit_ambient * lit_ao_only * lit_ssao;
        var lit_amb = select(lit_amb_flat, lit_amb_flat * 0.12, config.env_width > 0u);
        // Auto LIGHT probe (Phase 3b) — mirrors the PBR branch: real bounce ambient
        // when no HDRI supplies diffuse IBL. (1 − metal): diffuse term only —
        // metals get probe energy via the specular probe path.
        if (config.env_width == 0u) {
            lit_amb += probe_sh_irradiance(normal) * probe_sh_weight()
                     * (1.0 - lit_metal) * lit_ao_only * lit_ssao;
        }
        var lit_color = lit_albedo * (lit_diff + lit_amb) + lit_spec;

        // IBL for RASTER geometry (poly bodies + baked SDF/DC meshes).
        //
        // Owner 2026-07-30: "I still need to go to path tracing preview before metalness and
        // roughness work in PBR." Root cause: this branch computed its final colour from ANALYTIC
        // PUNCTUAL LIGHTS ONLY and returned before ever reaching the split-sum block the SDF branch
        // runs. A metal has no diffuse term (`lit_dw = 1 - metal*0.7`) and its specular is a mirror
        // of the environment — with nothing to reflect it reads as flat dark grey no matter how
        // correct the metallic map is. Path Trace looked right because it samples the env properly.
        //
        // This is a PORT of the PBR branch's split-sum block, not new machinery: SH9 irradiance,
        // the GGX-prefiltered chain and the BRDF LUT are already built and already bound.
        //
        // The invariant it restores: REPRESENTATION MUST NOT CHANGE SHADING — the same material on
        // an SDF body and a poly body should light the same way.
        //
        // Deliberately NOT ported: the SDF_REFLECT sphere-trace probe. It marches `scene_sdf`, which
        // describes only analytic SDF prims — a poly body is not in that field, so the probe would
        // reflect the wrong scene (or nothing). Env reflection is the correct term here.
        if (config.env_width > 0u) {
            let lit_ndv = max(dot(normal, lit_vd), 0.0);
            let lit_f0 = mix(vec3(0.04), lit_albedo, lit_metal);

            // Diffuse IBL: pre-convolved SH9 irradiance, sampled through the bent normal so the
            // surface integrates the sky it can actually SEE. AO occludes ambient only (17e
            // contract); multi-bounce recovers the albedo-tinted inter-reflection single-bounce AO
            // crushes. Brightness lives inside env_sh_irradiance — do not multiply it again.
            let lit_bent = ao_bent_normal(coord, depth, normal);
            var lit_env_diff = env_sh_irradiance(lit_bent);
            // Auto LIGHT probe: same probe-vs-HDRI diffuse blend as the PBR branch.
            lit_env_diff = mix(lit_env_diff, probe_sh_irradiance(lit_bent), probe_sh_weight());
            let lit_mb = multi_bounce_ao(lit_ssao, lit_albedo);
            let lit_kd = (1.0 - lit_metal) * lit_ao_only;
            lit_color += lit_albedo * lit_env_diff * lit_kd * lit_mb;

            // Specular IBL: mips 1..6 are GGX-prefiltered (roughness = mip/6). NOT attenuated by
            // AO — a chrome surface must reflect the environment at every angle, including the
            // bottom hemisphere. Split-sum second term (F0·A + B) with roughness/N·V-aware
            // specular occlusion so metals do not over-brighten at grazing angles.
            let lit_refl = reflect(-lit_vd, normal);
            let lit_refl_color = sample_env_specular(lit_refl, lit_rough);
            let lit_ab = env_brdf(lit_ndv, lit_rough);
            let lit_spec_occ = specular_occlusion(lit_ndv, lit_ssao, lit_rough);
            lit_color += lit_refl_color * (lit_f0 * lit_ab.x + vec3(lit_ab.y)) * lit_spec_occ;
        }

        // Alpha-blend transparent objects
        if (lit_opacity < 0.999) {
            let bg = bg_gradient_dithered(f32(global_id.x), f32(global_id.y), f32(tex_dim.y));
            lit_color = mix(bg, lit_color, lit_opacity);
        }
        // Emission
        if (lit_emission > 0.001) {
            lit_color = lit_color + lit_albedo * lit_emission;
        }
        // Selection outline
        if (lit_selected && config.sel_rim_enabled != 0u) {
            var lit_edge = false;
            let lit_ow = max(i32(config.sel_rim_power), 1);
            let lit_bx = i32(tex_dim.x); let lit_by = i32(tex_dim.y);
            for (var d = 1; d <= 10; d++) {
                if (d > lit_ow || lit_edge) { break; }
                let L = coord + vec2<i32>(-d, 0); let R = coord + vec2<i32>(d, 0);
                let U = coord + vec2<i32>(0, -d); let Dwn = coord + vec2<i32>(0, d);
                if (L.x < 0 || textureLoad(gbuf_albedo_in, L, 0).a < 0.9) { lit_edge = true; }
                if (!lit_edge && (R.x >= lit_bx || textureLoad(gbuf_albedo_in, R, 0).a < 0.9)) { lit_edge = true; }
                if (!lit_edge && (U.y < 0 || textureLoad(gbuf_albedo_in, U, 0).a < 0.9)) { lit_edge = true; }
                if (!lit_edge && (Dwn.y >= lit_by || textureLoad(gbuf_albedo_in, Dwn, 0).a < 0.9)) { lit_edge = true; }
            }
            if (lit_edge) { lit_color = config.sel_rim_color; }
        }
        // alpha=1 → tonemap applies exposure+ACES (Lit produces HDR values from multi-light)
        textureStore(output_texture, coord, vec4(lit_color, 1.0));
        return;
    }
    // #endregion LIT_BRANCH

    // Geometry (flags > 0.6)
    let is_selected = flags > 0.9;
    let prim_sid_raw = textureLoad(gbuf_surface_id_in, coord, 0).r; // for light linking
    let x = f32(global_id.x); let y = f32(global_id.y);
    let w = f32(tex_dim.x); let h = f32(tex_dim.y);
    let uv_x = (x / w) * 2.0 - 1.0;
    let uv_y = -((y / h) * 2.0 - 1.0);
    let aspect = w / h;
    let shade_ray = camera_ray(uv_x, uv_y, aspect);
    let cam_pos = shade_ray[0];
    let ray_dir = shade_ray[1];
    let p = cam_pos + ray_dir * depth;
    let view_dir = normalize(cam_pos - p);
    let quality = config.render_quality;

    var mat_rough = gbuf_m.x;
    var mat_metal = gbuf_m.y;
    var albedo = gbuf_a.rgb;
    var mat_opacity = min(gbuf_m.w, 1.0);
    var emission_add = max(gbuf_m.w - 1.0, 0.0);
    // Raster-pixel detection + surface_id sentinel. These MUST live OUTSIDE every #region
    // (DEFERRED_MATERIAL / SHADOW_AO get stripped in the df_lite build) because is_raster_pixel
    // and prim_sid are referenced by the always-present PBR light loop + shadow_ao gate below.
    let raster_snap = textureLoad(raster_depth_in, coord, 0).w;
    let is_raster_pixel = raster_snap > 0.0 && abs(raster_snap - depth) < 0.05;
    // Raster pixels (voxel / poly / baked-SDF triangles) carry a STALE surface_id: the raster
    // G-buffer pass writes only 4 of 7 channels (32B/sample limit) and physically CANNOT write
    // gbuf_surface_id, and the SDF compute raymarch early-returns over raster-covered pixels
    // without writing it (raymarch_entry.wgsl:181, raymarch_lite.wgsl:176). So the channel holds
    // a prim index left over from a prior SDF frame. Feeding that stale id to light_linked() with
    // user Include/Exclude light-linking would wrongly skip lights for the now-raster surface →
    // partial darkening. Route raster pixels through the "no SDF prim / all lights" sentinel
    // (0xFFFFFFFFu, light_linked() returns true) — same handling as the Lit branch above.
    let prim_sid = select(prim_sid_raw, 0xFFFFFFFFu, is_raster_pixel);
    // #region DEFERRED_MATERIAL
    // Full material evaluation (DNA/node/blended) — deferred from RM pass
    // Skip for raster-sourced pixels: raster G-buffer already has fully baked materials
    // (noise, edge wear, cavity dirt evaluated in fs_gbuffer). surface_id is stale for
    // raster pixels because the compute raymarch early-returns without writing it.
    // Evaluate full material for PBR (0), Solid+HDR (3), and RT fallback (4).
    // Wireframe (2) uses G-buffer albedo only — no benefit from DNA eval.
    // Evaluated once above (`surf_mat`, same gates: view 0/3/4, sid valid, not raster).
    if (surf_mat_valid) {
        let mat = surf_mat;
        albedo = mat.color;
        mat_rough = mat.roughness;
        mat_metal = mat.metallic;
        mat_opacity = mat.opacity;
        let em_lum = length(mat.emission);
        emission_add = em_lum;
        // Tint albedo toward emission for HDR glow
        if (em_lum > 0.001) {
            let em_w = clamp(em_lum / (em_lum + 1.0), 0.0, 1.0);
            albedo = mix(albedo, mat.emission / max(em_lum, 0.001), em_w);
        }
    }
    // #endregion DEFERRED_MATERIAL

    let key_dir = safe_dir(config.light_dir.xyz);
    let key_intensity = config.light_dir.w;
    let key_color = config.light_color.xyz;
    // Legacy scalar ambient strength from config (fallback before multi-light ambient RGB).
    let ambient_base = config.light_color.w;
    let fill_dir = normalize(vec3(-key_dir.x * 0.5, abs(key_dir.y) * 0.6 + 0.3, -key_dir.z * 0.5));

    // Compute shadow/AO in deferred for march kernel pixels (which write shadow_ao = 1.0)
    var final_shadow = shadow_ao;  // directional shadow (direct light only)
    var final_ao     = shadow_ao;  // ambient occlusion (ambient + direct)
    // Raster pixels: gbuf_material.z holds the DIRECTIONAL shadow-map factor, not AO.
    // A cast shadow blocks the sun, never the sky — multiplying ambient by it black-bands
    // every face turned away from the sun (and with the sun deleted, the stale factor WAS
    // the whole image — the round-4 zero-light artifact). Until a real screen-space AO
    // source exists for raster pixels, their ambient is unoccluded. (Kept OUTSIDE the
    // SHADOW_AO region: is_raster_pixel and this gate must survive the df_lite strip.)
    if (is_raster_pixel) {
        final_ao = 1.0;
    }
    // Real SSAO for raster pixels — ambient/IBL multiplier ONLY (PBR view_mode
    // 0; view_mode 6 covers Lit in the df_lite build, where the LIT_BRANCH
    // region is stripped and Lit pixels shade through this branch — the full
    // build's Lit early-return applies it above, so it can never double-apply).
    // Solid (3) stays pure form shading by owner decision; RT fallback (4) and
    // Wireframe (2) are untouched. Disabled toggle → white texture → 1.0.
    // Kept OUTSIDE the SHADOW_AO region (df_lite strip safe).
    var ssao_f = 1.0;
    if (is_raster_pixel && (config.view_mode == 0u || config.view_mode == 6u)) {
        ssao_f = ssao_factor(coord, depth);
    }
    // #region SHADOW_AO
    // ANALYTICAL SUN SHADOW/AO RECOMPUTE — REMOVED 2026-08-07 (owner: "old tech that
    // causes problems, we no longer see analytical — remove it"; the standing
    // "two occlusion systems coexist" decision item, now decided). The block that
    // lived here marched `soft_shadow`/`calc_ao` against the ANALYTICAL SDF FIELD for
    // non-raster pixels — shadows of geometry that is no longer what is displayed
    // (everything renders as baked DC meshes since 2026-07-14), inflated at convex
    // edges (the owner's smear screenshot), and blind to poly/voxel bodies. Occlusion
    // sources now: cascaded shadow maps baked by the raster pass (raster pixels), the
    // RT sun swap below (all pixels, opt-in or refine bit 512), GTAO/SSAO for ambient.
    // Consequence, deliberate: analytical march pixels (the raymarch-lite floor and
    // background) carry no sun shadow OUTSIDE refined/RT frames — the proper fix is
    // rasterizing the shadow-catcher floor, not resurrecting field marching.
    // Do NOT reintroduce a field-based shadow march here.
    // #endregion SHADOW_AO
    // ── SUN RT SHADOWS (2026-07-28, RT_SHADOWS_DESIGN's deferred "Sun RT area shadows") ────────
    // The sun is not part of the point/spot loop below — it shades through `final_shadow`, which
    // above came from cascaded shadow maps (raster pixels) or the SDF `soft_shadow` march (march
    // pixels). When the sun opts in to RT, REPLACE that term with its traced visibility: soft,
    // all-geometry (poly/voxel/imported/NURBS, not just SDF), and self-shadowing for free since
    // `pt_occluded` traces the body's own triangles.
    //
    // Opt-in is enforced upstream in `LightData::serialize_to_gpu` — a directional light only
    // reports `light_rt_enabled` once the user sets a nonzero Source Radius — so this reads the
    // same one slot every other RT consumer reads and cannot disagree with the panel.
    //
    // Deliberately OUTSIDE `#region SHADOW_AO`: that whole region is stripped from the deferred-
    // LITE build, and the sun must keep its RT shadows in lite exactly as point/spot lights do
    // (the coverage point the Stage-1 design makes for the point/spot swap).
    // `find_sun_index` returns the FIRST enabled directional light; a second sun would keep CSM.
    // Progressive-refine AUTO SUN (post_flags bit 512): while the idle accumulator
    // averages AND the ray pass is live, the sun is RT-traced even without its
    // per-light opt-in — leak-free through any wall thickness (the CSM-bias
    // thin-wall leak fix, owner 2026-08-07). Bit 512 is set CPU-side only when the
    // ray pass actually dispatches with a sun cone (viewport_render), so this can
    // never multiply by a vis channel the ray pass left at 1.0.
    if (config.rt_shadows_on != 0u && depth > 0.0 && config.interaction_no_shadows == 0u) {
        let sun_i = find_sun_index();
        if (sun_i < 8u && (light_rt_enabled(sun_i) || (config.post_flags & 512u) != 0u)) {
            // AO is kept as a separate ambient term — RT replaces only the DIRECT occlusion,
            // mirroring `shd = rt_light_vis(coord, li) * final_ao` in the point/spot branches.
            final_shadow = rt_light_vis(coord, sun_i) * final_ao;
        }
    }
    // Screen-space contact shadow (applied to all surface pixels, both pre-computed and deferred shadow)
    // Skipped when the sun is RT-shadowed: the traced visibility already resolves contact-scale
    // occlusion against real geometry, so the screen-space approximation would double-darken it.
    let sun_rt_active = config.rt_shadows_on != 0u && find_sun_index() < 8u
        && (light_rt_enabled(find_sun_index()) || (config.post_flags & 512u) != 0u);
    if (depth > 0.0 && config.interaction_no_shadows == 0u && !sun_rt_active) {
        final_shadow *= contact_shadow(p, normal, key_dir);
    }

    var color = vec3(0.0);

    // MatCap (view_mode==1) handled by early return above — never reaches here.
    if (config.view_mode == 2u) {
        // Wireframe
        var wc = wireframe_color(p, normal, albedo);
        color = wc;
    } else if (config.view_mode == 3u) {
        // Solid+HDR — apply volume shadow+AO (combined in gbuf_material.z by VP shader)
        var sc = solid_hdr_color(normal, view_dir, albedo);
        // Split shadow and AO: shadow dims the directional component,
        // AO dims the ambient component. Combined value approximates both.
        // Solid/clay preview: DROP the directional cast-shadow term (final_shadow). A boolean SUBTRACT cutter
        // rendered as a shadow caster blocks the key light and drives final_shadow→0, turning the whole base
        // body dark ("everything disappears" on boolean); cast shadows are also unreliable for DC meshes, and
        // every pro DCC's Solid mode omits them. Keep a 0.55·sc floor + 0.45 of the remaining occlusion signal
        // so the form always reads: occluded → 0.55·sc (clearly visible), unoccluded → ≈sc (normal bodies unchanged).
        color = sc * 0.55 + sc * 0.45 * final_ao;
    } else if (config.view_mode == 4u) {
        // Raytrace — compiled as separate pipeline (raytrace_entry.wgsl)
        // Fallback: render as PBR so viewport isn't black while RT pipeline compiles
        color = albedo * final_shadow * ambient_base;
    } else {
        // PBR — multi-light loop (Phase D: cluster list when enabled, else ≤32 global)
        let view_z = max(dot(p - config.cam_pos.xyz, config.cam_forward.xyz), 0.01);
        let n_lights = light_eval_count(coord, view_z, depth);
        var total_diffuse = vec3(0.0);
        var total_spec = vec3(0.0);
        var scene_ambient = vec3(ambient_base);
        // Normalized direct BRDF — the PT's exact definitions (direct_* helpers above):
        // Lambert (1 − F)(1 − metal)·albedo/π + Cook-Torrance F·D·G/(4·N·L·N·V), GGX alpha =
        // max(roughness², 0.001). The old `1 − 0.7·metal` diffuse weight (metals kept 30%
        // Lambert) and the un-normalized `F·D·N·L` lobe are gone. NEVER add a large constant to
        // the D denominator (the old `+ 0.0001` killed glossy peaks — "Roughness does nothing").
        let direct_f0 = mix(vec3(0.04), albedo, mat_metal);
        let direct_a = direct_alpha(mat_rough);

        for (var slot = 0u; slot < n_lights; slot++) {
            let li = light_eval_li(coord, view_z, depth, slot);
            // Light linking: skip if this light doesn't affect this prim
            if (!light_linked(prim_sid, li)) { continue; }
            let lt = light_type_v(li);
            let l_int = light_intensity_v(li);
            // P1: dead intensity → skip shadow + BRDF entirely
            if (l_int < 1e-5) {
                if (li == 0u) { scene_ambient = light_ambient_rgb_v(li); }
                continue;
            }
            let l_col = light_color_v(li);
            var l_dir: vec3<f32>;
            var atten = 1.0;
            var to_light_v = vec3(0.0); // unnormalized P→light (sphere-light spec below)
            var l_dist = 1e9;
            if (lt == 0u) {
                l_dir = light_dir_v(li);
            } else {
                let l_pos = light_pos(li);
                to_light_v = l_pos - p;
                l_dist = length(to_light_v);
                l_dir = to_light_v / max(l_dist, 0.001);
                atten = point_atten(l_dist, light_range_v(li));
                if (lt == 2u) {
                    let spot_dir = light_dir_v(li);
                    let cos_a = dot(-l_dir, spot_dir);
                    atten *= spot_atten(cos_a, light_spot_inner_cos(li), light_spot_outer_cos(li));
                }
                // P1: out of range / cone → no soft_shadow, no GGX
                if (atten < 0.01) { continue; }
            }
            // Per-light shadow: directional uses precomputed key shadow, point/spot compute their own
            var shd = final_shadow;
            // RT projected shadows (Stage 4 hybrid): a point/spot light reads the all-geometry
            // visibility buffer ONLY when the GLOBAL toggle is on AND this light's own RT-enable slot
            // is set (light_rt_enabled). li<4 → RGBA channel.
            // P1: when RT is ON and light is outside the 4-pack (or RT disabled per-light), do NOT
            // fall back to expensive SDF soft_shadow (soft_shadow storms). Unshadowed lit instead.
            if (config.rt_shadows_on != 0u && light_rt_enabled(li) && lt != 0u && li < 8u && depth > 0.0 && config.interaction_no_shadows == 0u) {
                shd = rt_light_vis(coord, li) * final_ao;
            } else if (lt != 0u) {
                // Non-RT (or beyond-pack) point/spot: unshadowed, ambient-occluded only.
                // The analytical SDF `soft_shadow` fallback that lived here was REMOVED
                // 2026-08-07 (owner order, completing the analytical retirement): it
                // marched the SDF FIELD — not the displayed meshes, blind to poly/voxel
                // casters. Point/spot shadows come ONLY from the per-light RT toggle
                // now. Do not reintroduce a field march here.
                // #region SHADOW_AO
                // (region kept for the df_lite strip machinery; intentionally empty)
                // #endregion SHADOW_AO
                shd = final_ao;
            }
            let l_e = l_col * (l_int * atten * shd); // irradiance at normal incidence
            total_diffuse += l_e * direct_diffuse(normal, view_dir, l_dir, direct_f0, mat_metal);
            // Cook-Torrance GGX (always on — roughness/metal must read in PBR without HDRI).
            // F0 is dielectric 0.04 or metal albedo — do not multiply an extra 0.08 (that
            // double-dimmed dielectrics so the lobe looked flat across roughness).
            {
                // Sphere-light specular (Karis representative point, 2026-08-07 "fix the
                // light"): Source Radius (WORLD units, slot 21) snaps the light vector to
                // the closest point on the bulb sphere along the reflection ray and widens
                // α by the subtended angle — a round highlight of the authored bulb size
                // instead of the delta-light pinpoint. Radius 0 = exact legacy math.
                // Diffuse keeps the center direction (sub-percent error at bulb scales).
                var s_dir = l_dir;
                var s_a = direct_a;
                let lr_w = light_rt_radius(li);
                if (lt != 0u && lr_w > 1e-4) {
                    let r_vec = reflect(-view_dir, normal);
                    let c2r = to_light_v - r_vec * dot(to_light_v, r_vec);
                    let closest = to_light_v - c2r * saturate(lr_w / max(length(c2r), 1e-4));
                    s_dir = normalize(closest);
                    s_a = saturate(direct_a + lr_w / (2.0 * max(l_dist, 1e-3)));
                }
                total_spec += l_e * direct_spec(normal, view_dir, s_dir, direct_f0, s_a);
            }
            if (li == 0u) { scene_ambient = light_ambient_rgb_v(li); }
        }
        // No visible lights → no flat ambient. (Do not inject env_brightness fill:
        // that kept the cube lit after the sun was eye-hidden. Real IBL still runs
        // when env_width > 0 via the split-sum path; this is only the sun Ambient term.)
        if (n_lights == 0u) { scene_ambient = vec3(0.0); }
        // Procedural fill from primary directional (skip when no lights — stale config)
        if (n_lights > 0u) {
            // Procedural fill light — an invisible SECOND directional light, not in the
            // scene graph. Strength was a hardcoded 0.2 until 2026-08-06; it is now
            // config.sym_origin.w (default 0.2, so the look is unchanged).
            //
            // Why it is exposed: it MOVES THE TERMINATOR. The lit/unlit break is not
            // where dot(N,sun)=0 but where dot(N,sun) + s*dot(N,fill) = 0, so on
            // hard-surface geometry it does not land on the crease the modeller
            // expects — reported as "the shadow is in a weird place". Set it to 0
            // for a physical terminator.
            //
            // Gated by AO, NOT by the sun's shadow factor (owner "we look to have
            // 2 shadow paths" 2026-08-07): the fill is a fake BOUNCE — sun
            // visibility must not kill it exactly where bounce light belongs. The
            // old × final_shadow coupling also made the CSM and refine-RT paths
            // disagree wildly: CSM's grazing acne let ~half the fill leak into
            // sun-shadowed walls (gray + drip streaks) while the leak-proof RT
            // sun returned an exact 0 there (pitch black). AO is smooth,
            // path-independent, and occludes the fill where a bounce would
            // actually be blocked.
            // Lambert-normalized like every direct light ((1 − metal)/π; no F — a fake bounce).
            total_diffuse += key_color * key_intensity * max(dot(normal, fill_dir), 0.0)
                           * config.sym_origin.w * ((1.0 - mat_metal) / 3.14159265) * final_ao;
        }
        // Flat ambient from the key light. When an HDRI is loaded it used to stay full-strength
        // and drown diffuse IBL — Intensity then only read as specular reflections. Dim the flat
        // ambient so SH diffuse fill tracks env_brightness (lookdev: one Intensity for lighting).
        let ambient_flat = scene_ambient * final_ao * ssao_f;
        var ambient = select(ambient_flat, ambient_flat * 0.12, config.env_width > 0u);
        // Auto LIGHT probe (Phase 3b): with no HDRI, the room's only ambient was the
        // flat gray term — the baked probe supplies real directional bounce
        // irradiance (sun patches, colored walls, point-lit rooms). With an HDRI the
        // probe instead BLENDS INTO the diffuse IBL below — never both (the ambient
        // double-count class).
        if (config.env_width == 0u) {
            // (1 − metal): this is a DIFFUSE term — metals get probe energy through
            // the specular probe path instead (double-count otherwise; mirrors the
            // IBL block's kd).
            ambient += probe_sh_irradiance(normal) * probe_sh_weight()
                     * (1.0 - mat_metal) * final_ao * ssao_f;
        }
        var base_color = albedo * (total_diffuse + ambient) + total_spec;

        // IBL: split-sum environment lighting (2026-07-18).
        // Diffuse = SH9 irradiance; specular = GGX-prefiltered chain × BRDF LUT.
        // Both terms apply env_brightness inside sample_env_specular / env_sh_irradiance.
        if (config.env_width > 0u) {
            let ndv = max(dot(normal, view_dir), 0.0);
            let f0 = mix(vec3(0.04), albedo, mat_metal);

            // Diffuse IBL: pre-convolved SH9 irradiance. GTAO supplies a BENT
            // normal (the average unoccluded direction) — sampling the sky the
            // surface can actually SEE gives directional self-shading from the
            // environment (the flat-cube fix). SSAO/off return the geometric
            // normal here, so the look is unchanged in those tiers.
            let bent_normal = ao_bent_normal(coord, depth, normal);
            var env_diffuse = env_sh_irradiance(bent_normal);
            // Auto LIGHT probe: the probe cube is the ACTUAL incident radiance at the
            // scene — walls occluding the sky, bounced sunlight — while the HDRI SH
            // assumes no scene exists at all. Blend the diffuse IBL toward the probe.
            env_diffuse = mix(env_diffuse, probe_sh_irradiance(bent_normal), probe_sh_weight());
            // AO occludes ambient only (the 17e contract). Multi-bounce recovers
            // the albedo-tinted inter-reflection a single-bounce AO would crush.
            // Brightness is inside env_sh_irradiance — do not multiply again.
            let mb = multi_bounce_ao(ssao_f, albedo);
            let kd = (1.0 - mat_metal) * final_ao;
            base_color += albedo * env_diffuse * kd * mb;

            // Specular IBL: mips 1..6 are GGX-prefiltered (roughness = mip/6) — the
            // split-sum first term. Not attenuated by AO — a chrome sphere must
            // reflect the environment at all angles, including the bottom hemisphere.
            let refl = reflect(-view_dir, normal);
            var refl_color = sample_env_specular(refl, mat_rough);

            // #region SDF_REFLECT
            // SDF reflection probe: one-bounce sphere trace for real reflections.
            // P1 perf: chrome-class only (metal high + rough low), Medium+ quality.
            // Interaction already uses df_lite which strips this entire region.
            // Draft/Low skip; High/Ultra get more steps.
            let want_sdf_refl = quality >= 2u && mat_metal > 0.5 && mat_rough < 0.25
                && config.num_primitives > 0u;
            if (want_sdf_refl) {
                let refl_origin = p + normal * 0.02;
                var t = 0.0;
                var hit = false;
                let refl_steps = select(24u, 48u, quality >= 3u);
                for (var step = 0u; step < refl_steps; step++) {
                    let rp = refl_origin + refl * t;
                    let d = scene_sdf(rp);
                    if (d < 0.001) { hit = true; break; }
                    t += d;
                    if (t > 20.0) { break; }
                }
                if (hit) {
                    let refl_pos = refl_origin + refl * t;
                    // Inline normal via central differences on scene_sdf
                    let ne = 0.001;
                    let refl_n = safe_dir(sdf_central_gradient(refl_pos, ne));
                    // Evaluate material at reflection hit point
                    let r_prim = find_hit_prim(refl_pos);
                    var r_albedo = vec3(0.6);
                    if (r_prim < config.num_primitives) {
                        let r_mat = eval_material(r_prim, refl_pos, refl_n);
                        r_albedo = r_mat.color;
                    }
                    // Shade hit point with direct lighting (key + fill)
                    let r_ndl = max(dot(refl_n, key_dir), 0.0);
                    let r_key = key_color * key_intensity * r_ndl;
                    let r_fill_dir = normalize(vec3(-key_dir.x, 0.3, -key_dir.z));
                    let r_fill = key_color * key_intensity * max(dot(refl_n, r_fill_dir), 0.0) * 0.2;
                    let r_ambient = scene_ambient;
                    // Sample env at reflected normal for indirect
                    let r_env = sample_env_mip(refl_n, f32(ENV_MIP_LEVELS - 1u)) * config.env_brightness * 0.5;
                    // Key + fill are irradiances → Lambert /π (normalized like the main
                    // direct terms); the flat ambient is already a radiance.
                    refl_color = r_albedo * ((r_key + r_fill) / 3.14159265 + r_ambient) + r_env;
                }
            }
            // #endregion SDF_REFLECT

            // Split-sum second term (F0·A + B) — replaces the old ad-hoc
            // fresnel × (1 − r²) weighting (over-bright at grazing, energy leak on rough).
            // Specular occlusion (roughness + N·V aware) darkens the specular lobe
            // in occluded areas so metals don't over-brighten at grazing angles.
            let ab = env_brdf(ndv, mat_rough);
            let spec_occ = specular_occlusion(ndv, ssao_f, mat_rough);
            base_color += refl_color * (f0 * ab.x + vec3(ab.y)) * spec_occ;
        }

        color = base_color;
    }

    // Alpha-blend with background for transparent objects
    // Skip for MatCap/Wireframe/Lit — handled in their own early-return branches
    if (config.view_mode != 1u && config.view_mode != 2u && config.view_mode != 6u && mat_opacity < 0.999) {
        var bg = bg_gradient_dithered(f32(global_id.x), f32(global_id.y), f32(tex_dim.y));
        if (config.env_width > 0u && !env_bg_hidden()) {
            let bx = f32(global_id.x); let by = f32(global_id.y);
            let bw = f32(tex_dim.x); let bh = f32(tex_dim.y);
            let buv_x = (bx / bw) * 2.0 - 1.0;
            let buv_y = -((by / bh) * 2.0 - 1.0);
            let baspect = bw / bh;
            let bfs = select(config.cam_forward.w, 1.0, config.cam_forward.w <= 0.0);
            let brd = normalize(config.cam_forward.xyz + config.cam_right.xyz * (buv_x * baspect * bfs) + config.cam_up.xyz * (buv_y * bfs));
            bg = sample_env(brd);
        }
        color = mix(bg, color, mat_opacity);
    }

    // Add emission AFTER alpha-blend so emissives glow at full intensity
    // even through transparent surfaces (feeds into bloom)
    // Skip for MatCap/Wireframe/Lit — no emission in passthrough modes
    if (config.view_mode != 1u && config.view_mode != 2u && config.view_mode != 6u && emission_add > 0.001) {
        color = color + albedo * emission_add;
    }

    // Unified selection outline: edge where SELECTED coverage ends (not selected / bg).
    // flags: 1.0 = selected, 0.78 = unselected geo, ~0 = clear bg. Threshold 0.9.
    // Do NOT use raw screen-bounds alone as the only signal — pair with flags so a
    // full-frame selected mesh does not paint a solid viewport rectangle.
    if (is_selected && config.sel_rim_enabled != 0u) {
        var is_edge = false;
        let ow = max(i32(config.sel_rim_power), 1);
        let ew = i32(tex_dim.x); let eh = i32(tex_dim.y);
        for (var d = 1; d <= 10; d++) {
            if (d > ow || is_edge) { break; }
            let L = coord + vec2<i32>(-d, 0); let R = coord + vec2<i32>(d, 0);
            let U = coord + vec2<i32>(0, -d); let Dwn = coord + vec2<i32>(0, d);
            // OOB = non-selected (silhouette at clip). Neighbor flags < 0.9 = unselected/bg.
            if (L.x < 0 || textureLoad(gbuf_albedo_in, L, 0).a < 0.9) { is_edge = true; }
            if (!is_edge && (R.x >= ew || textureLoad(gbuf_albedo_in, R, 0).a < 0.9)) { is_edge = true; }
            if (!is_edge && (U.y < 0 || textureLoad(gbuf_albedo_in, U, 0).a < 0.9)) { is_edge = true; }
            if (!is_edge && (Dwn.y >= eh || textureLoad(gbuf_albedo_in, Dwn, 0).a < 0.9)) { is_edge = true; }
        }
        if (is_edge) { color = config.sel_rim_color; }
    }

    // MatCap/Wireframe: alpha=0 so tonemap treats it as non-scene (straight passthrough)
    // Lit mode (6) uses alpha=1 via its own early-return; PBR/Solid/RT need tonemapping too
    let out_alpha = select(1.0, 0.0, config.view_mode == 1u || config.view_mode == 2u);
    textureStore(output_texture, coord, vec4(color, out_alpha));
}
