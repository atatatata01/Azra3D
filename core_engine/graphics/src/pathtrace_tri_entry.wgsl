// ============================================================
// AZRA — Triangle Path Trace Entry (Phase 2)
// Traces the SAME triangles the raster pass draws (DC mesh / poly / voxel /
// imported) via a two-level BVH: TLAS over instances → ray into body space
// via inv_model → per-body BLAS → watertight Möller–Trumbore.
// Requires sdf_shared.wgsl (GpuUniforms, camera_ray, output_texture) plus the
// LIGHTING + ENV regions of sdf_lighting.wgsl (light accessors, sample_env) —
// assembled by build_pathtrace_tri_wgsl. The analytical SDF is NEVER touched
// here (owner ban, 2026-07-16).
//
// Milestone E: full unidirectional PT — PBR materials from the PT material
// buffer (RasterMaterial layout, shared with raster), NEE over scene lights
// with traced shadows, environment sampling, cosine/GGX bounce sampling,
// Russian roulette, firefly clamp (pt_clamp), thin-lens DOF (pt_dof_aperture),
// progressive accumulation (first-frame guard — never reads uninitialized).
// Section F: NEE + MIS (GGX/Lambert). Section G: ReSTIR DI on bounce-0.
// Section H: env NEE + MIS, R2 AA jitter, adaptive RIS candidate count.
// Section I: restir cam reproject. Section J: SVGF-lite denoise.
// Section K: env luminance CDF importance sampling for HDRI NEE.
// Section L: power-based light selection (intensity×color×atten) for NEE/ReSTIR.
// Section N: RIS multi-candidate NEE on secondary bounces (no extra textures).
// Section P: light cap 32 + variance-guided adaptive SPP (CPU + shader).
// Section Q: per-hit light CDF (build once, sample K) + variance bounces / RR.
// Section R: temporal 1-bounce GI cache (interactive / low bounce depth).
// Section S: spatial GI reuse + warm GI cache on deep paths.
// Section T: normal/depth-guided GI spatial (gbuf_normal binding 57).
// Section U: albedo-guided GI spatial (gbuf_albedo binding 58).
// Section V: variance-aware GI temporal (history decay on sample disagreement).
// Section W: multi-bounce GI (up to 2) on lean path + smarter deep-path warm.
// Section X: gbuf-guided ReSTIR spatial + cold deep-path 2-bounce GI warm.
// Section Y: light cap 64 + metal/roughness-aware GI depth.
// Section Z: adaptive RIS/ReSTIR candidates by light count + single-light fast path.
// Section AA: empty-scene early-out + specular GI for smooth metals.
// Section AB: lean path GI owns env (no double env NEE) + zero-power light early-out.
// Section AC: R2-stratified spatial reuse + adaptive spatial sample count.
// Section AD: hierarchical light CDF (16×16) — up to 256 lights for NEE/ReSTIR.
// Section AE: ReSTIR GI — multi-candidate RIS over secondary paths (lean GI).
// Section AF: BRDF-aware light importance (power × cheap BRDF proxy per hit).
// Section AG: spatial light clustering (8 octants × 2 distance tiers = 16 bins).
// Section AH: full GI path reservoirs — oct direction + W/M in pt_gi, re-eval merge.
// Section AI: specular-lobe spatial-bin boost + ReSTIR DI temporal visibility gate.
// Section AJ: spatial ReSTIR same-light visibility reuse (skip occluded neighbors).
// Section AK: adaptive firefly clamp + variance-aware progressive blend.
// Section AL: ladder complete — A–Z + AA–AL interactive PT stack closed.
// Section B2: interactive half-res lean — skip spatial ReSTIR/GI, min RIS (orbit FPS).
//
// Every traversal is hard-bounded (TLAS stack 32, BLAS stack 64, uploaded
// node counts clamped by watchdog visit caps) — malformed trees cannot hang
// the GPU (TDR class).
// All buffer reads are gated by pt_meta counts.
// ============================================================

// #region PT_TRAVERSAL
// SHARED triangle-BVH traversal — structs, BVH SSBO bindings (42-47), the ray/AABB/
// triangle intersectors, and the two-level TLAS→BLAS walk + any-hit occlusion query.
// Extracted VERBATIM by build_rt_shadow_wgsl (RT_SHADOWS_DESIGN Stage 1) so the RT
// shadow pass fires occlusion rays against the EXACT same traversal the PT display uses
// (one code path, one oracle — trace_scene_cpu in render/src/pt_accel.rs stays the parity
// gate for both). The PT-only pieces caught inside this span (PtMaterial, the pt_accum
// binding 27, the pt_materials binding 48, the RNG) are UNUSED by the RT entry and Naga
// dead-code-strips them — rt_shadow_bgl_entries binds only 0/1/12/26/42-47. In the PT-tri
// build the region tags are inert comments (the whole file is included unchanged).
struct PtBvhNode {
    aabb_min: vec3<f32>,
    first: u32,     // internal: left child (right = first+1). leaf: first tri.
    aabb_max: vec3<f32>,
    count: u32,     // 0 = internal, >0 = leaf (tri count / TLAS: instance marker)
}

struct PtVertexData {
    pos: vec3<f32>,
    _p0: f32,
    normal: vec3<f32>,
    _p1: f32,
    uv: vec2<f32>,
    _p2: vec2<f32>,
}

struct PtTriData {
    v0: u32,
    v1: u32,
    v2: u32,
    material_idx: u32,
}

struct PtInstanceData {
    model: mat4x4<f32>,
    inv_model: mat4x4<f32>,
    normal_mat: array<vec4<f32>, 3>,
    blas_node_off: u32,
    tri_off: u32,
    vert_off: u32,
    material_base: u32,
}

struct PtMeta {
    tlas_count: u32,
    instance_count: u32,
    blas_count: u32,
    tri_count: u32,
    vert_count: u32,
    material_count: u32,
    _pad1: u32,
    _pad2: u32,
}

// RasterMaterial layout (render/src/mesh_gpu.rs — 320 B / 20×vec4). MUST match
// the CPU upload stride exactly — a shorter struct silently mis-indexes every
// material after the first (wrong albedo / missing maps).
struct PtMaterial {
    albedo: vec4<f32>,             // rgb + opacity
    roughness_metallic: vec4<f32>, // x=roughness y=metallic z=emission_intensity w=shader_type
    emission_color: vec4<f32>,
    noise_params: vec4<f32>,
    noise_color: vec4<f32>,
    edge_wear: vec4<f32>,
    edge_wear_color: vec4<f32>,
    cavity_dirt: vec4<f32>,
    cavity_dirt_color: vec4<f32>,
    albedo_map: vec4<f32>,         // slot bits, strength, tiling, proj
    normal_map: vec4<f32>,
    roughness_map: vec4<f32>,
    metallic_map: vec4<f32>,
    emission_map: vec4<f32>,
    bump_map: vec4<f32>,
    displacement_map: vec4<f32>,
    // SSS color map (layout lockstep). NOT sampled here: the triangle PT has no SSS
    // transport yet — it never reads adv_sss either, so there is no flat-vs-map lie
    // to avoid. When random-walk SSS lands, sample this slot exactly like
    // emission_map (pt_sample_mat_rgb) and mix over adv_sss.rgb by strength (y).
    sss_color_map: vec4<f32>,
    adv_params: vec4<f32>,         // sss_radius, ior, clearcoat, clearcoat_rough
    adv_sss: vec4<f32>,            // sss_color.rgb, sheen
    adv_glass: vec4<f32>,          // glass_tint.rgb, sheen_tint
}

@group(0) @binding(27) var pt_accum_in: texture_2d<f32>;
@group(0) @binding(42) var<storage, read> pt_tlas: array<PtBvhNode>;
@group(0) @binding(43) var<storage, read> pt_instances: array<PtInstanceData>;
@group(0) @binding(44) var<storage, read> pt_blas: array<PtBvhNode>;
@group(0) @binding(45) var<storage, read> pt_tris: array<PtTriData>;
@group(0) @binding(46) var<storage, read> pt_verts: array<PtVertexData>;
@group(0) @binding(47) var<uniform> pt_meta: PtMeta;
@group(0) @binding(48) var<storage, read> pt_materials: array<PtMaterial>;
// Material PBR maps (same array the raster G-buffer samples) — real albedo/normal/rough/metal/
// emission/bump/disp channels. Layer 0 = white sentinel.
@group(0) @binding(49) var pt_mat_tex_array: texture_2d_array<f32>;
@group(0) @binding(50) var pt_mat_tex_sampler: sampler;
@group(0) @binding(59) var pt_mat_linear_array: texture_2d_array<f32>;
// Phase A adaptive stop: atomic counters [noisy_pixels, total_pixels]. Cleared each frame.
// Residual = relative change of progressive mean; if residual > pt_focus_falloff (reused as
// residual threshold from CPU), pixel counts as noisy.
struct PtStats {
    noisy: atomic<u32>,
    total: atomic<u32>,
}
@group(0) @binding(51) var<storage, read_write> pt_stats: PtStats;

// ---- Procedural noise layer (VERBATIM port of gbuffer.wgsl hash31/noise3/fbm3, pt_-prefixed
// to avoid colliding with the sdf_* noise family) — the node editor's LIVE preview layer must
// shade IDENTICALLY here (owner 2026-07-23: live drag feedback in the path tracer). ----
fn pt_hash31(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
fn pt_noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(pt_hash31(i + vec3(0.0,0.0,0.0)), pt_hash31(i + vec3(1.0,0.0,0.0)), u.x),
            mix(pt_hash31(i + vec3(0.0,1.0,0.0)), pt_hash31(i + vec3(1.0,1.0,0.0)), u.x), u.y),
        mix(mix(pt_hash31(i + vec3(0.0,0.0,1.0)), pt_hash31(i + vec3(1.0,0.0,1.0)), u.x),
            mix(pt_hash31(i + vec3(0.0,1.0,1.0)), pt_hash31(i + vec3(1.0,1.0,1.0)), u.x), u.y),
        u.z);
}
fn pt_fbm3(p: vec3<f32>, octaves: u32) -> f32 {
    var sum = 0.0; var amp = 0.5; var pos = p;
    let max_oct = min(octaves, 64u);
    for (var i = 0u; i < max_oct; i++) {
        sum += amp * pt_noise3(pos);
        pos = pos * 2.01;
        amp *= 0.5;
    }
    return sum;
}

// ---- Material map sampling (mirrors gbuffer.wgsl; compute-safe textureSampleLevel) ----
fn pt_triplanar_rot_uv(uv: vec2<f32>, q: u32) -> vec2<f32> {
    let qq = q & 3u;
    if (qq == 1u) { return vec2(-uv.y, uv.x); }
    if (qq == 2u) { return vec2(-uv.x, -uv.y); }
    if (qq == 3u) { return vec2(uv.y, -uv.x); }
    return uv;
}
fn pt_sample_mat_triplanar(slot: u32, p: vec3<f32>, normal: vec3<f32>, tiling: f32, packed: u32) -> vec3<f32> {
    // Clamp layer to array bounds — OOB layer index was a device-lost class with maps.
    let layers = u32(textureNumLayers(pt_mat_tex_array));
    let s = i32(min(slot, max(layers, 1u) - 1u));
    var w = abs(normal);
    // Blend bits 5-9 (0 → default 4); rot bits 12-17. Match Hybrid gbuffer.wgsl.
    let bq = (packed >> 5u) & 0x1Fu;
    let blend = select(4.0, f32(bq) * 0.25, bq != 0u);
    w = pow(w, vec3(clamp(blend, 0.25, 32.0)));
    w = w / (w.x + w.y + w.z + 1e-4);
    let rot = (packed >> 12u) & 0x3Fu;
    let rx = rot & 3u;
    let ry = (rot >> 2u) & 3u;
    let rz = (rot >> 4u) & 3u;
    let cx = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.yz * tiling, rx), s, 0.0).rgb;
    let cy = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.xz * tiling, ry), s, 0.0).rgb;
    let cz = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.xy * tiling, rz), s, 0.0).rgb;
    return cx * w.x + cy * w.y + cz * w.z;
}
fn pt_sample_mat_uv(slot: u32, uv: vec2<f32>, tiling: f32) -> vec3<f32> {
    let layers = u32(textureNumLayers(pt_mat_tex_array));
    let s = i32(min(slot, max(layers, 1u) - 1u));
    return textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, uv * tiling, s, 0.0).rgb;
}
fn pt_reconstruct_u16(hi: f32, lo: f32) -> f32 {
    return (hi * 65280.0 + lo * 255.0) / 65535.0;
}

fn pt_sample_mat_precise(maps: texture_2d_array<f32>, slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, uv_mode: bool, packed: u32) -> vec3<f32> {
    if (uv_mode) { return material_plane_linear(maps, slot, uv * tiling); }
    let bq = (packed >> 5u) & 0x1Fu;
    let blend = select(4.0, f32(bq) * 0.25, bq != 0u);
    let weights = material_projection_weights(normal, blend);
    let rot = (packed >> 12u) & 0x3Fu;
    let projected_x = material_plane_linear(maps, slot, pt_triplanar_rot_uv(p.yz * tiling, rot & 3u));
    let projected_y = material_plane_linear(maps, slot, pt_triplanar_rot_uv(p.xz * tiling, (rot >> 2u) & 3u));
    let projected_z = material_plane_linear(maps, slot, pt_triplanar_rot_uv(p.xy * tiling, (rot >> 4u) & 3u));
    return projected_x * weights.x + projected_y * weights.y + projected_z * weights.z;
}
fn pt_srgb_to_lin(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
// `packed` is the full TexMap projection word. bit 10 = dual-plane 16-bit (slot + slot+1).
fn pt_sample_mat_rgb_hp(
    slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32,
    packed: u32, as_srgb_color: bool,
) -> vec3<f32> {
    let wide = (packed >> 2u) & 7u;
    let code = select(select(0u, 1u, (packed & 1u) != 0u), wide, wide != 0u);
    let uv_mode = code == 1u;
    if ((packed & (1u << 10u)) == 0u) {
        if (uv_mode) { return pt_sample_mat_uv(slot, uv, tiling); }
        return pt_sample_mat_triplanar(slot, p, normal, tiling, packed);
    }
    var hi: vec3<f32>;
    var lo: vec3<f32>;
    if ((packed & (1u << 18u)) != 0u) {
        hi = pt_sample_mat_precise(pt_mat_linear_array, slot, p, normal, uv, tiling, uv_mode, packed);
        lo = pt_sample_mat_precise(pt_mat_linear_array, slot + 1u, p, normal, uv, tiling, uv_mode, packed);
    } else {
        hi = pt_sample_mat_precise(pt_mat_tex_array, slot, p, normal, uv, tiling, uv_mode, packed);
        lo = pt_sample_mat_precise(pt_mat_tex_array, slot + 1u, p, normal, uv, tiling, uv_mode, packed);
    }
    var v = vec3<f32>(
        pt_reconstruct_u16(hi.r, lo.r),
        pt_reconstruct_u16(hi.g, lo.g),
        pt_reconstruct_u16(hi.b, lo.b),
    );
    if (as_srgb_color) {
        v = vec3<f32>(pt_srgb_to_lin(v.r), pt_srgb_to_lin(v.g), pt_srgb_to_lin(v.b));
    }
    return v;
}
fn pt_sample_mat_scalar(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, packed: u32) -> f32 {
    return pt_sample_mat_rgb_hp(slot, p, normal, uv, tiling, packed, false).r;
}
fn pt_sample_mat_rgb(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32) -> vec3<f32> {
    // Legacy: `proj` may be a packed word or a bare code — dual-plane only when bit 10 set.
    return pt_sample_mat_rgb_hp(slot, p, normal, uv, tiling, proj, true);
}

// Tangentless triplanar NORMAL mapping — whiteout blend (Ben Golus), ported from
// `render/src/gbuffer.wgsl::sample_mat_triplanar_normal`. No per-vertex tangent basis
// required (DC/voxel meshes have none). Returns perturbed normal in the SAME body-local
// space as `n_geo`. P4 / HANDOFF Decision 3: PT must match Hybrid here.
fn pt_sample_mat_triplanar_normal(
    slot: u32, p: vec3<f32>, n_geo: vec3<f32>, tiling: f32, strength: f32, packed: u32,
) -> vec3<f32> {
    let layers = u32(textureNumLayers(pt_mat_tex_array));
    let s = i32(min(slot, max(layers, 1u) - 1u));
    var w = abs(n_geo);
    let bq = (packed >> 5u) & 0x1Fu;
    let blend = select(4.0, f32(bq) * 0.25, bq != 0u);
    w = pow(w, vec3(clamp(blend, 0.25, 32.0)));
    w = w / (w.x + w.y + w.z + 0.0001);
    let rot = (packed >> 12u) & 0x3Fu;
    let rx = rot & 3u;
    let ry = (rot >> 2u) & 3u;
    let rz = (rot >> 4u) & 3u;
    // Data maps are pre-encoded so sRGB sample recovers raw tangent-space bytes.
    var tx = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.yz * tiling, rx), s, 0.0).rgb * 2.0 - 1.0;
    var ty = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.xz * tiling, ry), s, 0.0).rgb * 2.0 - 1.0;
    var tz = textureSampleLevel(pt_mat_tex_array, pt_mat_tex_sampler, pt_triplanar_rot_uv(p.xy * tiling, rz), s, 0.0).rgb * 2.0 - 1.0;
    // Green-channel convention — MUST match gbuffer.wgsl sample_mat_triplanar_normal: these uv
    // conventions read v DOWN the image, so DirectX green (bit 11 of the packed word) passes
    // through and OpenGL green (the default) is negated.
    let gs = select(-1.0, 1.0, (packed & 2048u) != 0u);
    tx = vec3(tx.x * strength, tx.y * gs * strength, tx.z);
    ty = vec3(ty.x * strength, ty.y * gs * strength, ty.z);
    tz = vec3(tz.x * strength, tz.y * gs * strength, tz.z);
    // Same UV conventions as pt_sample_mat_triplanar (p.yz / p.xz / p.xy) so color+normal align.
    let nx = vec3(tx.xy + n_geo.yz, abs(tx.z) * n_geo.x);
    let ny = vec3(ty.xy + n_geo.xz, abs(ty.z) * n_geo.y);
    let nz = vec3(tz.xy + n_geo.xy, abs(tz.z) * n_geo.z);
    return normalize(nx.zxy * w.x + ny.xzy * w.y + nz.xyz * w.z);
}

// Apply normal_map (+ optional bump_map FD) to a body-local geometric normal, then to world.
// `n_world` is the camera-faced geometric normal; returns the camera-faced shading normal.
fn pt_apply_normal_maps(
    hit: PtHit,
    mat: PtMaterial,
    p_local: vec3<f32>,
    n_local: vec3<f32>,
    face_uv: vec2<f32>,
    n_world: vec3<f32>,
    ray_d: vec3<f32>,
) -> vec3<f32> {
    var n_loc = n_local;
    let nmap_slot = bitcast<u32>(mat.normal_map.x);
    if (nmap_slot != 0u) {
        let tiling = max(mat.normal_map.z, 0.05);
        let strength = clamp(mat.normal_map.y, 0.0, 4.0);
        n_loc = pt_sample_mat_triplanar_normal(
            nmap_slot, p_local, n_local, tiling, strength, bitcast<u32>(mat.normal_map.w));
    }
    // Bump (height): finite-difference surface gradient in body-local space (no screen derivs).
    let bmap_slot = bitcast<u32>(mat.bump_map.x);
    if (bmap_slot != 0u) {
        let tiling = max(mat.bump_map.z, 0.05);
        let strength = clamp(mat.bump_map.y, 0.0, 4.0);
        let packed = bitcast<u32>(mat.bump_map.w);
        let eps = 0.002 / tiling;
        // Local orthonormal frame for FD.
        var t = cross(n_loc, vec3(0.0, 1.0, 0.0));
        if (dot(t, t) < 1e-6) { t = cross(n_loc, vec3(1.0, 0.0, 0.0)); }
        t = normalize(t);
        let b = normalize(cross(n_loc, t));
        let h0 = pt_sample_mat_scalar(bmap_slot, p_local, n_loc, face_uv, tiling, packed);
        let hx = pt_sample_mat_scalar(bmap_slot, p_local + t * eps, n_loc, face_uv, tiling, packed);
        let hy = pt_sample_mat_scalar(bmap_slot, p_local + b * eps, n_loc, face_uv, tiling, packed);
        let dh_dt = (hx - h0) / eps;
        let dh_db = (hy - h0) / eps;
        n_loc = material_bump_normal(n_loc, t, b, dh_dt, dh_db, strength);
    }
    if (nmap_slot == 0u && bmap_slot == 0u) {
        return n_world;
    }
    let nm = pt_instances[hit.instance].normal_mat;
    var nw = normalize(nm[0].xyz * n_loc.x + nm[1].xyz * n_loc.y + nm[2].xyz * n_loc.z);
    // Keep camera-facing convention (same as pt_hit_normal).
    let geometric_world = nm[0].xyz * n_local.x + nm[1].xyz * n_local.y + nm[2].xyz * n_local.z;
    if (dot(geometric_world, n_world) < 0.0) { nw = -nw; }
    return nw;
}

// The triangle BVH builder permits depth 48; depth-first traversal can retain
// one far sibling per level, so keep comfortable headroom. The TLAS is a
// median-balanced tree and needs much less private storage.
const PT_TLAS_STACK_DEPTH: u32 = 32u;
const PT_BLAS_STACK_DEPTH: u32 = 64u;
const PT_MAX_TLAS_VISITS: u32 = 1024u;
const PT_MAX_BLAS_VISITS: u32 = 8192u;
const PT_EPS: f32 = 2e-4;
// Separate transmission bounce budget (owner decision #3, default +8) so thick/stacked
// glass does not exhaust the diffuse pt_max_bounces and render black.
const MAX_GLASS_BOUNCE: u32 = 8u;
const PT_PI: f32 = 3.14159265;

// ---- RNG (copied verbatim from sdf_material.wgsl RNG_TRACE — that region
// also contains the analytical trace_ray, which must not enter this build.
// Keep these three in lockstep with the originals.) ----
fn pt_pcg_hash(input: u32) -> u32 {
    let state = input * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}
fn pt_rand(seed: ptr<function, u32>) -> f32 {
    *seed = pt_pcg_hash(*seed);
    return f32(*seed) / 4294967295.0;
}
fn pt_cosine_hemisphere(n: vec3<f32>, seed: ptr<function, u32>) -> vec3<f32> {
    let r1 = pt_rand(seed);
    let r2 = pt_rand(seed);
    let phi = 2.0 * PT_PI * r1;
    let cos_theta = sqrt(r2);
    let sin_theta = sqrt(1.0 - r2);
    var tangent: vec3<f32>;
    if (abs(n.y) < 0.999) {
        tangent = normalize(cross(vec3(0.0, 1.0, 0.0), n));
    } else {
        tangent = normalize(cross(vec3(1.0, 0.0, 0.0), n));
    }
    let bitangent = cross(n, tangent);
    return normalize(tangent * cos(phi) * sin_theta + bitangent * sin(phi) * sin_theta + n * cos_theta);
}

struct PtHit {
    t: f32,
    u: f32,
    v: f32,
    tri: u32,       // global tri index
    instance: u32,
    hit: bool,
}

// Slab test with unnormalized direction (inv_dir precomputed by caller).
// Returns entry distance, or -1 for a miss. Keeping t-near lets traversal push
// the farther child first so the nearer child is popped first (LIFO stack).
fn pt_ray_aabb_near(orig: vec3<f32>, inv_dir: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>, t_max: f32) -> f32 {
    let t1 = (bmin - orig) * inv_dir;
    let t2 = (bmax - orig) * inv_dir;
    let tmin = max(max(min(t1.x, t2.x), min(t1.y, t2.y)), max(min(t1.z, t2.z), 0.0));
    let tmax = min(min(max(t1.x, t2.x), max(t1.y, t2.y)), min(max(t1.z, t2.z), t_max));
    return select(-1.0, tmin, tmin <= tmax);
}

fn pt_safe_inv(d: vec3<f32>) -> vec3<f32> {
    return vec3(
        1.0 / select(d.x, 1e-30, d.x == 0.0),
        1.0 / select(d.y, 1e-30, d.y == 0.0),
        1.0 / select(d.z, 1e-30, d.z == 0.0),
    );
}

// Möller–Trumbore, front+back faces. Returns vec4(t, u, v, hit_flag).
fn pt_ray_tri(orig: vec3<f32>, dir: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> vec4<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let pv = cross(dir, e2);
    let det = dot(e1, pv);
    if (abs(det) < 1e-9) { return vec4(0.0, 0.0, 0.0, -1.0); }
    let inv_det = 1.0 / det;
    let tv = orig - a;
    let u = dot(tv, pv) * inv_det;
    if (u < 0.0 || u > 1.0) { return vec4(0.0, 0.0, 0.0, -1.0); }
    let qv = cross(tv, e1);
    let v = dot(dir, qv) * inv_det;
    if (v < 0.0 || u + v > 1.0) { return vec4(0.0, 0.0, 0.0, -1.0); }
    let t = dot(e2, qv) * inv_det;
    if (t <= 1e-5) { return vec4(0.0, 0.0, 0.0, -1.0); }
    return vec4(t, u, v, 1.0);
}

// Shadow-ray triangle test: avoids producing barycentrics/hit identity and
// returns immediately once an intersection inside the light segment is found.
fn pt_ray_tri_any(orig: vec3<f32>, dir: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, t_max: f32) -> bool {
    let e1 = b - a;
    let e2 = c - a;
    let pv = cross(dir, e2);
    let det = dot(e1, pv);
    if (abs(det) < 1e-9) { return false; }
    let inv_det = 1.0 / det;
    let tv = orig - a;
    let u = dot(tv, pv) * inv_det;
    if (u < 0.0 || u > 1.0) { return false; }
    let qv = cross(tv, e1);
    let v = dot(dir, qv) * inv_det;
    if (v < 0.0 || u + v > 1.0) { return false; }
    let t = dot(e2, qv) * inv_det;
    return t > 1e-5 && t < t_max;
}

// Traverse one instance's BLAS with the ray already in body space.
// `t` values stay world-consistent because the local direction is NOT
// renormalized (mirror of trace_scene_cpu in render/src/pt_accel.rs — keep
// the two in lockstep, the CPU fn is the parity oracle).
fn pt_trace_blas(inst_idx: u32, orig: vec3<f32>, dir: vec3<f32>, best: ptr<function, PtHit>) {
    let inst = pt_instances[inst_idx];
    let lo = (inst.inv_model * vec4(orig, 1.0)).xyz;
    let ld = (inst.inv_model * vec4(dir, 0.0)).xyz;
    let inv_ld = pt_safe_inv(ld);

    var stack: array<u32, PT_BLAS_STACK_DEPTH>;
    var stack_near: array<f32, PT_BLAS_STACK_DEPTH>;
    var sp: u32 = 0u;
    if (inst.blas_node_off >= pt_meta.blas_count) { return; }
    let root = pt_blas[inst.blas_node_off];
    let root_near = pt_ray_aabb_near(lo, inv_ld, root.aabb_min, root.aabb_max, (*best).t);
    if (root_near < 0.0) { return; }
    stack[0] = inst.blas_node_off;
    stack_near[0] = root_near;
    sp = 1u;
    var visits: u32 = 0u;

    while (sp > 0u && visits < min(pt_meta.blas_count, PT_MAX_BLAS_VISITS)) {
        visits += 1u;
        sp -= 1u;
        let node_idx = stack[sp];
        let node_near = stack_near[sp];
        if (node_near > (*best).t) { continue; }
        if (node_idx >= pt_meta.blas_count) { continue; }
        let node = pt_blas[node_idx];
        if (node.count > 0u) {
            // Leaf: triangles at tris[tri_off + first .. + count].
            for (var k = 0u; k < node.count; k += 1u) {
                let tri_idx = inst.tri_off + node.first + k;
                if (tri_idx >= pt_meta.tri_count) { break; }
                let tri = pt_tris[tri_idx];
                if (tri.v0 >= pt_meta.vert_count || tri.v1 >= pt_meta.vert_count || tri.v2 >= pt_meta.vert_count) { continue; }
                let a = pt_verts[tri.v0].pos;
                let b = pt_verts[tri.v1].pos;
                let c = pt_verts[tri.v2].pos;
                let r = pt_ray_tri(lo, ld, a, b, c);
                if (r.w > 0.0 && r.x < (*best).t) {
                    (*best).t = r.x;
                    (*best).u = r.y;
                    (*best).v = r.z;
                    (*best).tri = tri_idx;
                    (*best).instance = inst_idx;
                    (*best).hit = true;
                }
            }
        } else {
            // Internal: children are body-relative → rebase by blas_node_off.
            let left_idx = inst.blas_node_off + node.first;
            let right_idx = left_idx + 1u;
            var left_near = -1.0;
            var right_near = -1.0;
            if (left_idx < pt_meta.blas_count) {
                let left = pt_blas[left_idx];
                left_near = pt_ray_aabb_near(lo, inv_ld, left.aabb_min, left.aabb_max, (*best).t);
            }
            if (right_idx < pt_meta.blas_count) {
                let right = pt_blas[right_idx];
                right_near = pt_ray_aabb_near(lo, inv_ld, right.aabb_min, right.aabb_max, (*best).t);
            }
            if (left_near >= 0.0 && right_near >= 0.0 && sp + 2u <= PT_BLAS_STACK_DEPTH) {
                let left_first = left_near <= right_near;
                stack[sp] = select(left_idx, right_idx, left_first);
                stack_near[sp] = select(left_near, right_near, left_first);
                stack[sp + 1u] = select(right_idx, left_idx, left_first);
                stack_near[sp + 1u] = select(right_near, left_near, left_first);
                sp += 2u;
            } else if (left_near >= 0.0 && sp < PT_BLAS_STACK_DEPTH) {
                stack[sp] = left_idx;
                stack_near[sp] = left_near;
                sp += 1u;
            } else if (right_near >= 0.0 && sp < PT_BLAS_STACK_DEPTH) {
                stack[sp] = right_idx;
                stack_near[sp] = right_near;
                sp += 1u;
            }
        }
    }
}

fn pt_trace_scene(orig: vec3<f32>, dir: vec3<f32>, t_far: f32) -> PtHit {
    var best: PtHit;
    best.t = t_far;
    best.hit = false;
    if (pt_meta.tlas_count == 0u) { return best; }
    let inv_dir = pt_safe_inv(dir);

    var stack: array<u32, PT_TLAS_STACK_DEPTH>;
    var stack_near: array<f32, PT_TLAS_STACK_DEPTH>;
    var sp: u32 = 0u;
    let root = pt_tlas[0];
    let root_near = pt_ray_aabb_near(orig, inv_dir, root.aabb_min, root.aabb_max, best.t);
    if (root_near < 0.0) { return best; }
    stack[0] = 0u;
    stack_near[0] = root_near;
    sp = 1u;
    var visits: u32 = 0u;

    while (sp > 0u && visits < min(pt_meta.tlas_count, PT_MAX_TLAS_VISITS)) {
        visits += 1u;
        sp -= 1u;
        let node_idx = stack[sp];
        let node_near = stack_near[sp];
        if (node_near > best.t) { continue; }
        if (node_idx >= pt_meta.tlas_count) { continue; }
        let node = pt_tlas[node_idx];
        if (node.count > 0u) {
            // TLAS leaf: first = instance index.
            if (node.first < pt_meta.instance_count) {
                pt_trace_blas(node.first, orig, dir, &best);
            }
        } else {
            let left_idx = node.first;
            let right_idx = left_idx + 1u;
            var left_near = -1.0;
            var right_near = -1.0;
            if (left_idx < pt_meta.tlas_count) {
                let left = pt_tlas[left_idx];
                left_near = pt_ray_aabb_near(orig, inv_dir, left.aabb_min, left.aabb_max, best.t);
            }
            if (right_idx < pt_meta.tlas_count) {
                let right = pt_tlas[right_idx];
                right_near = pt_ray_aabb_near(orig, inv_dir, right.aabb_min, right.aabb_max, best.t);
            }
            if (left_near >= 0.0 && right_near >= 0.0 && sp + 2u <= PT_TLAS_STACK_DEPTH) {
                let left_first = left_near <= right_near;
                stack[sp] = select(left_idx, right_idx, left_first);
                stack_near[sp] = select(left_near, right_near, left_first);
                stack[sp + 1u] = select(right_idx, left_idx, left_first);
                stack_near[sp + 1u] = select(right_near, left_near, left_first);
                sp += 2u;
            } else if (left_near >= 0.0 && sp < PT_TLAS_STACK_DEPTH) {
                stack[sp] = left_idx;
                stack_near[sp] = left_near;
                sp += 1u;
            } else if (right_near >= 0.0 && sp < PT_TLAS_STACK_DEPTH) {
                stack[sp] = right_idx;
                stack_near[sp] = right_near;
                sp += 1u;
            }
        }
    }
    return best;
}

fn pt_occluded_blas(inst_idx: u32, orig: vec3<f32>, dir: vec3<f32>, t_max: f32) -> bool {
    if (inst_idx >= pt_meta.instance_count) { return false; }
    let inst = pt_instances[inst_idx];
    if (inst.blas_node_off >= pt_meta.blas_count) { return false; }
    let lo = (inst.inv_model * vec4(orig, 1.0)).xyz;
    let ld = (inst.inv_model * vec4(dir, 0.0)).xyz;
    let inv_ld = pt_safe_inv(ld);
    let root = pt_blas[inst.blas_node_off];
    let root_near = pt_ray_aabb_near(lo, inv_ld, root.aabb_min, root.aabb_max, t_max);
    if (root_near < 0.0) { return false; }

    var stack: array<u32, PT_BLAS_STACK_DEPTH>;
    var sp = 1u;
    var visits = 0u;
    stack[0] = inst.blas_node_off;
    while (sp > 0u && visits < min(pt_meta.blas_count, PT_MAX_BLAS_VISITS)) {
        visits += 1u;
        sp -= 1u;
        let node_idx = stack[sp];
        if (node_idx >= pt_meta.blas_count) { continue; }
        let node = pt_blas[node_idx];
        if (node.count > 0u) {
            for (var k = 0u; k < node.count; k += 1u) {
                let tri_idx = inst.tri_off + node.first + k;
                if (tri_idx >= pt_meta.tri_count) { break; }
                let tri = pt_tris[tri_idx];
                if (tri.v0 >= pt_meta.vert_count || tri.v1 >= pt_meta.vert_count || tri.v2 >= pt_meta.vert_count) { continue; }
                if (pt_ray_tri_any(lo, ld, pt_verts[tri.v0].pos, pt_verts[tri.v1].pos, pt_verts[tri.v2].pos, t_max)) {
                    return true;
                }
            }
        } else {
            let left_idx = inst.blas_node_off + node.first;
            let right_idx = left_idx + 1u;
            var left_near = -1.0;
            var right_near = -1.0;
            if (left_idx < pt_meta.blas_count) {
                let left = pt_blas[left_idx];
                left_near = pt_ray_aabb_near(lo, inv_ld, left.aabb_min, left.aabb_max, t_max);
            }
            if (right_idx < pt_meta.blas_count) {
                let right = pt_blas[right_idx];
                right_near = pt_ray_aabb_near(lo, inv_ld, right.aabb_min, right.aabb_max, t_max);
            }
            if (left_near >= 0.0 && right_near >= 0.0 && sp + 2u <= PT_BLAS_STACK_DEPTH) {
                let left_first = left_near <= right_near;
                stack[sp] = select(left_idx, right_idx, left_first);
                stack[sp + 1u] = select(right_idx, left_idx, left_first);
                sp += 2u;
            } else if (left_near >= 0.0 && sp < PT_BLAS_STACK_DEPTH) {
                stack[sp] = left_idx;
                sp += 1u;
            } else if (right_near >= 0.0 && sp < PT_BLAS_STACK_DEPTH) {
                stack[sp] = right_idx;
                sp += 1u;
            }
        }
    }
    return false;
}

// Shadow query: true any-hit traversal. Stops at the first triangle in the
// light segment instead of completing a nearest-hit walk and material hit.
fn pt_occluded(orig: vec3<f32>, dir: vec3<f32>, dist: f32) -> bool {
    let t_max = dist - PT_EPS * 4.0;
    if (t_max <= 1e-5 || pt_meta.tlas_count == 0u) { return false; }
    let inv_dir = pt_safe_inv(dir);
    let root = pt_tlas[0];
    let root_near = pt_ray_aabb_near(orig, inv_dir, root.aabb_min, root.aabb_max, t_max);
    if (root_near < 0.0) { return false; }

    var stack: array<u32, PT_TLAS_STACK_DEPTH>;
    var sp = 1u;
    var visits = 0u;
    stack[0] = 0u;
    while (sp > 0u && visits < min(pt_meta.tlas_count, PT_MAX_TLAS_VISITS)) {
        visits += 1u;
        sp -= 1u;
        let node_idx = stack[sp];
        if (node_idx >= pt_meta.tlas_count) { continue; }
        let node = pt_tlas[node_idx];
        if (node.count > 0u) {
            if (pt_occluded_blas(node.first, orig, dir, t_max)) { return true; }
        } else {
            let left_idx = node.first;
            let right_idx = left_idx + 1u;
            var left_near = -1.0;
            var right_near = -1.0;
            if (left_idx < pt_meta.tlas_count) {
                let left = pt_tlas[left_idx];
                left_near = pt_ray_aabb_near(orig, inv_dir, left.aabb_min, left.aabb_max, t_max);
            }
            if (right_idx < pt_meta.tlas_count) {
                let right = pt_tlas[right_idx];
                right_near = pt_ray_aabb_near(orig, inv_dir, right.aabb_min, right.aabb_max, t_max);
            }
            if (left_near >= 0.0 && right_near >= 0.0 && sp + 2u <= PT_TLAS_STACK_DEPTH) {
                let left_first = left_near <= right_near;
                stack[sp] = select(left_idx, right_idx, left_first);
                stack[sp + 1u] = select(right_idx, left_idx, left_first);
                sp += 2u;
            } else if (left_near >= 0.0 && sp < PT_TLAS_STACK_DEPTH) {
                stack[sp] = left_idx;
                sp += 1u;
            } else if (right_near >= 0.0 && sp < PT_TLAS_STACK_DEPTH) {
                stack[sp] = right_idx;
                sp += 1u;
            }
        }
    }
    return false;
}
// #endregion PT_TRAVERSAL

fn pt_hit_uv(hit: PtHit) -> vec2<f32> {
    let triangle = pt_tris[hit.tri];
    return pt_verts[triangle.v0].uv * (1.0 - hit.u - hit.v)
        + pt_verts[triangle.v1].uv * hit.u
        + pt_verts[triangle.v2].uv * hit.v;
}

// World-space interpolated normal at a hit.
fn pt_hit_normal(h: PtHit, dir: vec3<f32>) -> vec3<f32> {
    let tri = pt_tris[h.tri];
    let n0 = pt_verts[tri.v0].normal;
    let n1 = pt_verts[tri.v1].normal;
    let n2 = pt_verts[tri.v2].normal;
    let nl = normalize(n0 * (1.0 - h.u - h.v) + n1 * h.u + n2 * h.v);
    let nm = pt_instances[h.instance].normal_mat;
    var nw = normalize(nm[0].xyz * nl.x + nm[1].xyz * nl.y + nm[2].xyz * nl.z);
    // Face the camera (front+back hits both shade).
    if (dot(nw, dir) > 0.0) { nw = -nw; }
    return nw;
}

// RAW world-space geometric normal — same interpolation + instance transform as
// pt_hit_normal but WITHOUT the camera-facing flip. Refraction needs the true outward
// orientation to distinguish entering (dot(ray_d, n) < 0) from exiting.
fn pt_hit_normal_raw(h: PtHit) -> vec3<f32> {
    let tri = pt_tris[h.tri];
    let n0 = pt_verts[tri.v0].normal;
    let n1 = pt_verts[tri.v1].normal;
    let n2 = pt_verts[tri.v2].normal;
    let nl = normalize(n0 * (1.0 - h.u - h.v) + n1 * h.u + n2 * h.v);
    let nm = pt_instances[h.instance].normal_mat;
    return normalize(nm[0].xyz * nl.x + nm[1].xyz * nl.y + nm[2].xyz * nl.z);
}

fn pt_hit_material(h: PtHit) -> PtMaterial {
    // Safe default when the materials SSBO is empty / not yet uploaded.
    if (pt_meta.material_count == 0u) {
        var d: PtMaterial;
        d.albedo = vec4(0.66, 0.585, 0.53, 1.0);
        d.roughness_metallic = vec4(0.65, 0.0, 0.0, 0.0);
        d.emission_color = vec4(0.0);
        d.noise_params = vec4(0.0);
        d.noise_color = vec4(0.0);
        d.edge_wear = vec4(0.0);
        d.edge_wear_color = vec4(0.0);
        d.cavity_dirt = vec4(0.0);
        d.cavity_dirt_color = vec4(0.0);
        d.albedo_map = vec4(0.0);
        d.normal_map = vec4(0.0);
        d.roughness_map = vec4(0.0);
        d.metallic_map = vec4(0.0);
        d.emission_map = vec4(0.0);
        d.bump_map = vec4(0.0);
        d.displacement_map = vec4(0.0);
        d.sss_color_map = vec4(0.0);
        d.adv_params = vec4(0.0, 1.5, 0.0, 0.1);
        d.adv_sss = vec4(1.0, 0.2, 0.1, 0.0);
        d.adv_glass = vec4(0.0);
        return d;
    }
    let tri = pt_tris[h.tri];
    let inst = pt_instances[h.instance];
    // Per-tri slot when set (multi-material imports); else instance material_base.
    // Note: material_base 0 is a valid first slot — only tri.material_idx==0 means "use base".
    var idx = tri.material_idx;
    if (idx == 0u) { idx = inst.material_base; }
    if (idx >= pt_meta.material_count) { idx = 0u; }
    return pt_materials[idx];
}

// Flat viewport sky — the exact gradient PT already shows when no HDRI is loaded.
fn pt_bg_flat(dir: vec3<f32>) -> vec3<f32> {
    let sky_t = dir.y * 0.5 + 0.5;
    return mix(unpack_rgb(config.bg_bottom_packed), unpack_rgb(config.bg_top_packed), sky_t);
}

fn pt_miss_env(dir: vec3<f32>) -> vec3<f32> {
    if (config.env_width > 0u) {
        return sample_env(dir);
    }
    return pt_bg_flat(dir);
}

// CAMERA-ray miss = the visible background, and the ONLY miss that honours the
// "Show HDRI in background" toggle (post_flags 4096). Bounce misses and NEE keep
// pt_miss_env, so hiding the plate changes what you SEE, never what lights the scene
// (Cycles' World → Ray Visibility → Camera).
fn pt_miss_camera(dir: vec3<f32>) -> vec3<f32> {
    if (env_bg_hidden()) { return pt_bg_flat(dir); }
    return pt_miss_env(dir);
}

// ── Section F: NEE + MIS (GGX microfacet + Lambert, balance heuristic) ──────
// ── Section G: ReSTIR DI reservoirs on bounce-0 (temporal + spatial reuse) ─
// Reservoir packing (rgba32f): y=light index (-1 empty), W=contrib weight, M, w_sum.
// Secondary bounces: Section N RIS multi-candidate NEE (no reservoir texture).

@group(0) @binding(52) var pt_restir_prev: texture_2d<f32>;
@group(0) @binding(53) var pt_restir_out: texture_storage_2d<rgba32float, write>;
// Section R/AH: GI path reservoirs — prev read + out write
// Pack: rg = oct(dir), b = W, a = M  (Section AH)
@group(0) @binding(55) var pt_gi_prev: texture_2d<f32>;
@group(0) @binding(56) var pt_gi_out: texture_storage_2d<rgba16float, write>;
// Section T/U: full-res G-buffer guides (raster under PT)
@group(0) @binding(57) var pt_gbuf_normal: texture_2d<f32>;
@group(0) @binding(58) var pt_gbuf_albedo: texture_2d<f32>;

const RESTIR_CANDIDATES_MIN: u32 = 4u;
const RESTIR_CANDIDATES_MAX: u32 = 8u;
const RESTIR_SPATIAL_MAX: u32 = 4u;
const RESTIR_RADIUS: f32 = 12.0;
const RESTIR_M_CLAMP: f32 = 20.0;
// Section N: secondary-bounce RIS (cheaper than full ReSTIR DI)
const NEE_RIS_CANDIDATES_MIN: u32 = 2u;
const NEE_RIS_CANDIDATES_MAX: u32 = 4u;
// Section P/Y/AD: max lights considered by NEE / ReSTIR (16 → 32 → 64 → 256)
// Hierarchical: PT_LIGHT_GROUPS × PT_LIGHTS_PER_GROUP leaf capacity.
const PT_LIGHT_GROUPS: u32 = 16u;
const PT_LIGHTS_PER_GROUP: u32 = 16u;
const PT_MAX_LIGHTS: u32 = 256u;

override PT_INTERACTIVE: bool = false;

// Section H: integer-scrambled independent jitter and reuse samples.
fn pt_r2(i: u32) -> vec2<f32> {
    return vec2(f32(pt_pcg_hash(i) >> 8u), f32(pt_pcg_hash(i ^ 0x9e3779b9u) >> 8u)) * (1.0 / 16777216.0);
}

// Section B2: detect half-res lighting (PathTraceQuality orbit/settle).
// output_texture is the PT lighting target; gbuf is always full viewport.
fn pt_is_half_res_lighting() -> bool {
    let od = textureDimensions(output_texture);
    let gd = textureDimensions(pt_gbuf_normal);
    // Half when each axis is ~½ full gbuf (allow +1 rounding from odd sizes)
    return od.x * 2u <= gd.x + 2u && od.y * 2u <= gd.y + 2u;
}

// Section Z/AD: adaptive ReSTIR RIS count by light count + progressive quality.
// nL=1 → 1 (no multi-sample waste); many lights → more candidates.
fn pt_restir_candidate_count(nL: u32) -> u32 {
    if (nL <= 1u) { return 1u; }
    // Section B2: orbit/settle — single candidate (latency over multi-light RIS)
    if (pt_is_half_res_lighting()) {
        return 1u;
    }
    var n = RESTIR_CANDIDATES_MIN;
    if (nL >= 8u) { n = 6u; }
    if (nL >= 24u) { n = RESTIR_CANDIDATES_MAX; }
    // Section AD: many-light scenes need denser RIS to cover the hierarchy
    if (nL >= 64u) { n = RESTIR_CANDIDATES_MAX; }
    // Multi-spp only densifies RIS — deep progressive history does NOT force max
    // (was: every idle frame after accum>8 ran 8 candidates → major FPS tax).
    if (config.pt_samples_per_frame >= 2u) { n = max(n, 6u); }
    // Deep progressive: lean candidates (history averages DI; temporal ReSTIR carries)
    if (config.pt_accumulated > 32u && config.pt_samples_per_frame <= 1u) {
        n = min(n, RESTIR_CANDIDATES_MIN + 1u);
    }
    // Cap at light count (no point sampling more than nL unique with replacement)
    return min(n, nL + nL / 2u + 1u);
}

// Section N/Z/AD: secondary NEE RIS count — scales with lights + quality.
fn pt_nee_ris_candidate_count(nL: u32) -> u32 {
    if (nL <= 1u) { return 1u; }
    // Section B2: lean secondary RIS while orbiting
    if (pt_is_half_res_lighting()) {
        return 1u;
    }
    var n = NEE_RIS_CANDIDATES_MIN;
    if (nL >= 8u) { n = 3u; }
    if (nL >= 24u) { n = NEE_RIS_CANDIDATES_MAX; }
    if (nL >= 64u) { n = NEE_RIS_CANDIDATES_MAX; }
    // Only multi-spp densifies — not deep accum (old path forced max forever after 4 frames)
    if (config.pt_samples_per_frame >= 2u) {
        n = max(n, NEE_RIS_CANDIDATES_MAX);
    }
    if (config.pt_accumulated > 32u && config.pt_samples_per_frame <= 1u) {
        n = min(n, NEE_RIS_CANDIDATES_MIN);
    }
    return min(n, nL);
}

struct PtReservoir {
    y: f32,
    W: f32,
    M: f32,
    w_sum: f32,
}

fn pt_res_empty() -> PtReservoir {
    return PtReservoir(-1.0, 0.0, 0.0, 0.0);
}

fn pt_res_from_vec(v: vec4<f32>) -> PtReservoir {
    return PtReservoir(v.r, v.g, v.b, v.a);
}

fn pt_res_to_vec(r: PtReservoir) -> vec4<f32> {
    return vec4(r.y, r.W, r.M, r.w_sum);
}

fn pt_luminance(c: vec3<f32>) -> f32 {
    return max(dot(c, vec3(0.2126, 0.7152, 0.0722)), 0.0);
}

fn pt_fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    let t = clamp(1.0 - cos_theta, 0.0, 1.0);
    let t2 = t * t;
    return f0 + (vec3(1.0) - f0) * (t2 * t2 * t);
}

// Exact (unpolarized) dielectric Fresnel reflectance. NOT the metal Schlick form:
// it is authoritative for TIR at grazing exit angles, which Schlick's f0 gets wrong.
// eta = ior_from / ior_to. cos_i = |dot(-ray_d, nf)| >= 0. Returns reflectance in [0,1];
// 1.0 signals total internal reflection (the caller must reflect, never refract).
fn pt_fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    let s2t = eta * eta * (1.0 - cos_i * cos_i); // sin^2(theta_t) via Snell
    if (s2t >= 1.0) { return 1.0; }              // TIR -> full reflect
    let cos_t = sqrt(1.0 - s2t);
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    return clamp(0.5 * (rs * rs + rp * rp), 0.0, 1.0);
}

fn pt_ggx_D(ndh: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (PT_PI * d * d + 1e-7);
}

// Height-correlated Smith G2 for GGX (Heitz 2014 — the form Cycles/PBRT/Filament use), `a` = GGX
// alpha (= roughness²). Replaced the Schlick-GGX `k = (a+1)²/8` remap (2026-10-13): that is UE4's
// analytic-light "hotness" remap, defined on PERCEPTUAL roughness and wrong for a path tracer —
// fed alpha, even a mirror kept k = 0.125, so G = 0.44 at n·v = 0.2. White-furnace measurement
// (`pt_furnace_gpu_test`): a white smooth METAL returned only 44% of a uniform environment at
// grazing view, 79% at n·v = 0.5 — the owner's "metal renders too black".
fn pt_smith_G2(ndl: f32, ndv: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = ndl * sqrt(a2 + (1.0 - a2) * ndv * ndv);
    let gl = ndv * sqrt(a2 + (1.0 - a2) * ndl * ndl);
    return 2.0 * ndl * ndv / max(gv + gl, 1e-7);
}

// BRDF * cos_theta (incident). v, l point away from surface.
fn pt_eval_f_cos(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, albedo: vec3<f32>, rough: f32, metal: f32) -> vec3<f32> {
    let ndl = max(dot(n, l), 0.0);
    let ndv = max(dot(n, v), 0.0);
    if (ndl <= 0.0 || ndv <= 0.0) { return vec3(0.0); }
    let h = normalize(v + l);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);
    let a = max(rough * rough, 0.001);
    let f0 = mix(vec3(0.04), albedo, metal);
    let F = pt_fresnel_schlick(vdh, f0);
    let D = pt_ggx_D(ndh, a);
    let G = pt_smith_G2(ndl, ndv, a);
    let spec = F * D * G / (4.0 * ndl * ndv + 1e-7);
    let kd = (vec3(1.0) - F) * (1.0 - metal);
    let diff = kd * albedo / PT_PI;
    return (diff + spec) * ndl;
}

// PDF of mixed diffuse (cosine) + GGX specular reflection sampling.
fn pt_pdf_bsdf(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, rough: f32, metal: f32) -> f32 {
    let ndl = max(dot(n, l), 0.0);
    if (ndl <= 0.0) { return 0.0; }
    let h = normalize(v + l);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);
    let a = max(rough * rough, 0.001);
    let pdf_diff = ndl / PT_PI;
    // Specular: D(h) * cos / (4 * |v·h|)
    let pdf_spec = pt_ggx_D(ndh, a) * ndh / max(4.0 * vdh, 1e-5);
    let spec_prob = clamp(metal + (1.0 - rough) * 0.35, 0.05, 0.95);
    return mix(pdf_diff, pdf_spec, spec_prob);
}

fn pt_balance_mis(pdf_a: f32, pdf_b: f32) -> f32 {
    let a2 = pdf_a * pdf_a;
    let b2 = pdf_b * pdf_b;
    return a2 / max(a2 + b2, 1e-10);
}

// ── Section L+Q+AD+AF+AG: hierarchical light selection ───────────────────
// Selection weight ≈ power * atten * BRDF_proxy(n,v,l).
// Section Q: build weights ONCE per shading point, then sample K candidates.
// Section AD: two-level hierarchy (16 bins × leaf scan = 256 capacity).
// Section AF: BRDF-aware — prefer lights in the BRDF lobe (diffuse n·l +
//   cheap specular toward reflection), still unbiased via pdf_pick = w/total.
// Section AG: spatial bins — 8 direction octants × 2 distance tiers (not
//   index-order groups). Coarse sample picks a spatial region, fine scan
//   within that bin. Clusters nearby lights so multi-light scenes sample
//   coherently from the same solid-angle/distance region.

fn pt_light_power_weight(li: u32, p: vec3<f32>) -> f32 {
    let power = pt_luminance(light_color_v(li)) * max(light_intensity_v(li), 0.0);
    let lt = light_type_v(li);
    if (lt == 0u) {
        // Directional: no distance falloff
        return max(power, 1e-6);
    }
    let dist = length(light_pos(li) - p);
    var atten = point_atten(max(dist, 1e-4), light_range_v(li));
    if (lt == 2u) {
        // Spot: cheap orientation factor toward the shading point
        let ldir = (light_pos(li) - p) / max(dist, 1e-4);
        let cos_a = dot(-ldir, light_dir_v(li));
        atten *= max(spot_atten(cos_a, light_spot_inner_cos(li), light_spot_outer_cos(li)), 0.05);
    }
    // Floor keeps very dim/far lights from being completely unsampled
    return max(power * atten, 1e-6);
}

// Section AF: cheap BRDF proxy for light picking (not full GGX eval).
// Below-hemisphere → near-zero so RIS wastes fewer samples on backfaces.
fn pt_light_brdf_proxy(
    n: vec3<f32>,
    v: vec3<f32>,
    ldir: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
) -> f32 {
    let ndl = max(dot(n, ldir), 0.0);
    if (ndl <= 1e-5) {
        return 1e-5; // tiny floor keeps PDF defined for edge cases
    }
    // Diffuse: Lambertian albedo * n·l
    let kd = pt_luminance(albedo) * (1.0 - metal);
    let diff = kd * ndl;

    // Specular: peak toward perfect reflection, width from roughness
    let r = reflect(-v, n);
    let rdl = max(dot(r, ldir), 0.0);
    let a = max(rough * rough, 0.001);
    // Smooth lobe: rdl / (a + (1-a)*rdl) → sharp peak when a→0, flatter when rough
    let lobe = rdl / (a + (1.0 - a) * rdl + 1e-4);
    let f0 = mix(0.04, pt_luminance(albedo), metal);
    // Fresnel-ish view factor (cheap Schlick on n·v)
    let ndv = max(dot(n, v), 0.0);
    let f_sch = f0 + (1.0 - f0) * pow(1.0 - ndv, 5.0);
    let spec = f_sch * lobe * ndl;

    // Soft blend: metals lean specular; dielectrics keep diffuse base
    let proxy = diff + spec * (0.35 + 0.65 * metal + 0.4 * (1.0 - rough));
    return max(proxy, 1e-5);
}

// Section AF: full selection weight = geometric power × BRDF proxy.
fn pt_light_select_weight(
    li: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
) -> f32 {
    let power = pt_light_power_weight(li, p);
    var ldir: vec3<f32>;
    let lt = light_type_v(li);
    if (lt == 0u) {
        ldir = light_dir_v(li); // TO the sun (LightData::direction) — see pt_sample_light
    } else {
        let to_l = light_pos(li) - p;
        let dist = length(to_l);
        if (dist < 1e-5) { return 1e-6; }
        ldir = to_l / dist;
    }
    let brdf = pt_light_brdf_proxy(n, v, ldir, albedo, rough, metal);
    return max(power * brdf, 1e-6);
}

// Section AG: spatial bin for light relative to shading point p.
// Layout: bits [2:0] = direction octant (sign of xyz), bit 3 = distance tier.
// → 8 octants × 2 tiers = 16 bins (matches PT_LIGHT_GROUPS).
// Directionals land in the far tier of their direction octant.
fn pt_dir_to_octant(dir: vec3<f32>) -> u32 {
    var oct = 0u;
    if (dir.x >= 0.0) { oct |= 1u; }
    if (dir.y >= 0.0) { oct |= 2u; }
    if (dir.z >= 0.0) { oct |= 4u; }
    return oct & 7u;
}

fn pt_light_spatial_bin(li: u32, p: vec3<f32>) -> u32 {
    let lt = light_type_v(li);
    var dir: vec3<f32>;
    var dist_tier = 0u;
    if (lt == 0u) {
        dir = light_dir_v(li); // TO the sun (octant of the incoming light)
        dist_tier = 1u; // infinite / far tier
    } else {
        let to_l = light_pos(li) - p;
        let dist = length(to_l);
        dir = to_l / max(dist, 1e-4);
        // Near vs far relative to light range (fallback absolute scale)
        let range = max(light_range_v(li), 1.0);
        dist_tier = select(0u, 1u, dist > range * 0.35);
    }
    return pt_dir_to_octant(dir) | (dist_tier << 3u);
}

// Section AI: boost leaf weights whose spatial bin aligns with the specular lobe.
// Applied to w[i] so hierarchical pdf stays exact (w_i / Σw).
fn pt_specular_bin_boost(bin: u32, n: vec3<f32>, v: vec3<f32>, rough: f32, metal: f32) -> f32 {
    // Diffuse / rough dielectrics: little or no bin preference
    let spec_amt = clamp(metal * 0.85 + (1.0 - rough) * 0.55, 0.0, 1.0);
    if (spec_amt < 0.08) { return 1.0; }

    let r = reflect(-v, n);
    // Representative direction for this octant (unit cube corner)
    let oct = bin & 7u;
    var cdir = vec3(
        select(-1.0, 1.0, (oct & 1u) != 0u),
        select(-1.0, 1.0, (oct & 2u) != 0u),
        select(-1.0, 1.0, (oct & 4u) != 0u),
    );
    cdir = normalize(cdir);
    let align = max(dot(normalize(r), cdir), 0.0);
    // Sharper peak on smooth metals
    let sharpness = mix(1.5, 10.0, (1.0 - rough) * (1.0 - rough));
    let lobe = pow(align + 1e-4, sharpness);
    // Prefer near-tier slightly for local specular highlights
    let tier = bin >> 3u;
    let tier_pref = select(1.12, 1.0, tier == 1u);
    return 1.0 + spec_amt * 3.5 * lobe * tier_pref;
}

fn pt_light_table_weight_in_bin(
    li: u32,
    bin: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
) -> f32 {
    return pt_light_select_weight(li, p, n, v, albedo, rough, metal)
        * pt_specular_bin_boost(bin, n, v, rough, metal);
}

fn pt_light_table_weight(
    li: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
) -> f32 {
    let bin = pt_light_spatial_bin(li, p);
    return pt_light_table_weight_in_bin(li, bin, p, n, v, albedo, rough, metal);
}

// Build the compact spatial-bin prefix CDF. Leaf weights are recomputed only
// inside the selected bin instead of retaining a 256-float function-local
// table per hit, avoiding ~1 KiB of private storage/register spill per lane.
fn pt_light_table_build(
    nL: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    gcdf: ptr<function, array<f32, 16>>,
) -> f32 {
    var bin_w: array<f32, 16>;
    for (var b = 0u; b < PT_LIGHT_GROUPS; b += 1u) {
        bin_w[b] = 0.0;
    }
    let nn = min(nL, PT_MAX_LIGHTS);
    for (var i = 0u; i < nn; i += 1u) {
        let bin = pt_light_spatial_bin(i, p);
        bin_w[bin] += pt_light_table_weight_in_bin(i, bin, p, n, v, albedo, rough, metal);
    }
    // Prefix sum over spatial bins → gcdf
    var total = 0.0;
    for (var g = 0u; g < PT_LIGHT_GROUPS; g += 1u) {
        total += bin_w[g];
        (*gcdf)[g] = total;
    }
    return total;
}

// Section AI: hard visibility for a light center (no soft disk) — ReSTIR temporal gate.
// Returns true if the light is unoccluded from shading point p.
fn pt_light_is_visible(p: vec3<f32>, n: vec3<f32>, li: u32) -> bool {
    let lt = light_type_v(li);
    var ldir: vec3<f32>;
    var dist = 1e30;
    if (lt == 0u) {
        ldir = light_dir_v(li); // TO the sun
        if (dot(n, ldir) <= 0.0) { return false; }
        dist = max(config.far_clip * 0.99, 1e3);
    } else {
        let to_l = light_pos(li) - p;
        dist = length(to_l);
        if (dist < 1e-5) { return false; }
        ldir = to_l / dist;
        if (dot(n, ldir) <= 0.0) { return false; }
        dist = max(dist - PT_EPS * 4.0, 0.0);
    }
    return !pt_occluded(p + n * PT_EPS * 2.0, ldir, dist);
}

fn pt_light_table_pdf(
    li: u32,
    nL: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    total: f32,
) -> f32 {
    if (total < 1e-10 || nL == 0u || li >= nL) {
        return select(0.0, 1.0 / f32(max(nL, 1u)), nL > 0u);
    }
    return max(pt_light_table_weight(li, p, n, v, albedo, rough, metal), 1e-6) / total;
}

// Section AG: hierarchical sample — coarse spatial bin, then fine scan in bin.
// O(log G + nL) worst-case fine scan (only lights in selected bin contribute).
// Requires table from pt_light_table_build (gcdf = spatial bin prefixes).
fn pt_light_table_sample(
    nL: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    total: f32,
    gcdf: ptr<function, array<f32, 16>>,
    seed: ptr<function, u32>,
    out_pdf_pick: ptr<function, f32>,
) -> u32 {
    if (nL == 0u) {
        *out_pdf_pick = 0.0;
        return 0u;
    }
    if (total < 1e-10 || nL == 1u) {
        *out_pdf_pick = 1.0 / f32(nL);
        return 0u;
    }
    let nn = min(nL, PT_MAX_LIGHTS);

    // Coarse: binary search first spatial bin where gcdf[g] >= u
    let u = pt_rand(seed) * total;
    var lo = 0u;
    var hi = PT_LIGHT_GROUPS;
    loop {
        if (lo >= hi) { break; }
        let mid = (lo + hi) / 2u;
        if ((*gcdf)[mid] < u) { lo = mid + 1u; }
        else { hi = mid; }
    }
    let g = min(lo, PT_LIGHT_GROUPS - 1u);

    // Fine: residual mass within spatial bin — scan lights that map to g
    var g_lo = 0.0;
    if (g > 0u) { g_lo = (*gcdf)[g - 1u]; }
    let g_span = max((*gcdf)[g] - g_lo, 0.0);
    let u_fine = clamp(u - g_lo, 0.0, g_span);

    var cum = 0.0;
    var chosen = 0u;
    var found = false;
    if (u_fine <= 1e-10) {
        for (var i0 = 0u; i0 < nn; i0 += 1u) {
            if (pt_light_spatial_bin(i0, p) != g) { continue; }
            if (pt_light_table_weight_in_bin(i0, g, p, n, v, albedo, rough, metal) > 0.0) {
                chosen = i0;
                found = true;
                break;
            }
        }
    } else {
        for (var i = 0u; i < nn; i += 1u) {
            if (pt_light_spatial_bin(i, p) != g) { continue; }
            cum += pt_light_table_weight_in_bin(i, g, p, n, v, albedo, rough, metal);
            if (cum + 1e-8 >= u_fine) {
                chosen = i;
                found = true;
                break;
            }
        }
    }
    // Fallback: any positive-weight light (empty bin / numeric edge)
    if (!found) {
        for (var j = 0u; j < nn; j += 1u) {
            if (pt_light_table_weight(j, p, n, v, albedo, rough, metal) > 0.0) { chosen = j; break; }
        }
    }

    *out_pdf_pick = pt_light_table_pdf(chosen, nn, p, n, v, albedo, rough, metal, total);
    return chosen;
}

// Sample a specific light (optionally random soft disk). `pdf_pick` is the
// discrete selection pdf (Section L power pick, or 1/nL). Returns unshadowed
// f*Le components; out_pdf = pdf_pick * solid-angle density.
// p_hat = luminance(f_cos * Le) for RIS.
fn pt_sample_light(
    li: u32,
    nL: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
    pdf_pick: f32,
    out_ldir: ptr<function, vec3<f32>>,
    out_dist: ptr<function, f32>,
    out_fLe: ptr<function, vec3<f32>>,
    out_pdf: ptr<function, f32>,
    out_phat: ptr<function, f32>,
) -> bool {
    let lt = light_type_v(li);
    var ldir: vec3<f32>;
    var dist = 1e30;
    var atten = 1.0;
    var pdf_light = max(pdf_pick, 1e-12);

    if (lt == 0u) {
        // Directional slot 4..6 is the direction FROM the surface TO the sun — the scene-wide
        // convention (LightData::direction(): el 55° → +y; render/src/shadows.rs dir_to_sun;
        // deferred N·L; rt_shadow_entry occlusion rays; the analytic pathtrace_entry). This
        // file used to NEGATE it at every directional site, so the path tracer lit the
        // ANTI-sun hemisphere: a sun-facing surface got zero direct light and the underside
        // was lit (RASTER-PT-BRIGHTNESS-GAP, AZRA_PARITY gate 2026-10-13). Never negate it.
        ldir = light_dir_v(li);
        let soft = light_rt_radius(li);
        if (soft > 1e-5) {
            let j = pt_cosine_hemisphere(ldir, seed);
            ldir = normalize(mix(ldir, j, clamp(soft * 0.05, 0.0, 0.15)));
        }
    } else {
        let to_l = light_pos(li) - p;
        dist = length(to_l);
        if (dist < 1e-5) { return false; }
        ldir = to_l / dist;
        atten = point_atten(dist, light_range_v(li));
        if (lt == 2u) {
            let cos_a = dot(-ldir, light_dir_v(li));
            atten *= spot_atten(cos_a, light_spot_inner_cos(li), light_spot_outer_cos(li));
        }
        let r_soft = light_rt_radius(li);
        if (r_soft > 1e-5) {
            var t: vec3<f32>;
            if (abs(ldir.y) < 0.999) { t = normalize(cross(vec3(0.0, 1.0, 0.0), ldir)); }
            else { t = normalize(cross(vec3(1.0, 0.0, 0.0), ldir)); }
            let b = cross(ldir, t);
            let u1 = pt_rand(seed);
            let u2 = pt_rand(seed);
            let rr = sqrt(u1) * r_soft;
            let ang = 2.0 * PT_PI * u2;
            let offset = t * (rr * cos(ang)) + b * (rr * sin(ang));
            let to2 = (light_pos(li) + offset) - p;
            dist = length(to2);
            if (dist < 1e-5) { return false; }
            ldir = to2 / dist;
            let area = PT_PI * r_soft * r_soft;
            let cos_l = max(abs(dot(ldir, normalize(offset + ldir * 1e-3))), 0.05);
            pdf_light = pdf_pick * (dist * dist) / max(area * cos_l, 1e-8);
        }
    }

    let ndl = dot(n, ldir);
    if (ndl <= 0.0 || atten <= 1e-6) { return false; }

    let Le = light_color_v(li) * light_intensity_v(li) * atten;
    let f_cos = pt_eval_f_cos(n, v, ldir, albedo, rough, metal);
    let fLe = f_cos * Le;
    let phat = pt_luminance(fLe);
    if (phat < 1e-10 || pdf_light < 1e-10) { return false; }

    *out_ldir = ldir;
    *out_dist = dist;
    *out_fLe = fLe;
    *out_pdf = pdf_light;
    *out_phat = phat;
    return true;
}

fn pt_res_update(r: ptr<function, PtReservoir>, y: f32, w: f32, seed: ptr<function, u32>) {
    (*r).w_sum += w;
    (*r).M += 1.0;
    if ((*r).w_sum > 0.0 && pt_rand(seed) * (*r).w_sum < w) {
        (*r).y = y;
    }
}

// Merge another reservoir stream (temporal/spatial). p_hat_other evaluated at THIS pixel.
fn pt_res_merge(
    r: ptr<function, PtReservoir>,
    other: PtReservoir,
    p_hat_other: f32,
    seed: ptr<function, u32>,
) {
    if (other.y < 0.0 || other.M <= 0.0 || p_hat_other < 1e-10) { return; }
    // Stream weight for the selected sample of `other`
    let w = other.W * p_hat_other * other.M;
    if (w <= 0.0) { return; }
    (*r).w_sum += w;
    (*r).M += other.M;
    if ((*r).w_sum > 0.0 && pt_rand(seed) * (*r).w_sum < w) {
        (*r).y = other.y;
    }
}

fn pt_res_finalize(r: ptr<function, PtReservoir>, p_hat_y: f32) {
    if ((*r).y < 0.0 || (*r).M <= 0.0 || p_hat_y < 1e-10) {
        *r = pt_res_empty();
        return;
    }
    // Unbiased contribution weight
    (*r).W = (*r).w_sum / max((*r).M * p_hat_y, 1e-10);
}

// Target p_hat for a stored light index at this shading point (unshadowed).
// Section AD: prefer cached table PDF when available (avoids rebuild per merge).
fn pt_phat_for_light_table(
    li: u32,
    nL: u32,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
    total: f32,
) -> f32 {
    var ldir = vec3(0.0);
    var dist = 0.0;
    var fLe = vec3(0.0);
    var pdf = 0.0;
    var phat = 0.0;
    let pdf_pick = pt_light_table_pdf(li, nL, p, n, v, albedo, rough, metal, total);
    if (!pt_sample_light(li, nL, p, n, v, albedo, rough, metal, seed, pdf_pick, &ldir, &dist, &fLe, &pdf, &phat)) {
        return 0.0;
    }
    return phat;
}

// Section G+AI+AJ: bounce-0 ReSTIR DI — RIS + temporal/spatial vis-gated reuse.
// Writes the final reservoir to `pt_restir_out` at `coord`.
// Section AI: temporal visibility gate. Section AJ: spatial same-light vis reuse.
fn pt_restir_di(
    coord: vec2<i32>,
    tex_dim: vec2<u32>,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    let nL = min(num_lights(), PT_MAX_LIGHTS);
    if (nL == 0u) {
        textureStore(pt_restir_out, coord, pt_res_to_vec(pt_res_empty()));
        return vec3(0.0);
    }

    var r = pt_res_empty();

    // Section Q+AD+AF+AG+AI: hierarchical BRDF-aware spatial light table once per hit
    var light_gcdf: array<f32, 16>;
    let light_total = pt_light_table_build(nL, p, n, v, albedo, rough, metal, &light_gcdf);
    // Section AB: all lights zero power / out of range — skip RIS work
    if (light_total < 1e-10) {
        textureStore(pt_restir_out, coord, pt_res_to_vec(pt_res_empty()));
        return vec3(0.0);
    }

    // Section AI: cache last visibility test (li, ok) for final-shade reuse
    var vis_li: i32 = -1;
    var vis_ok = false;

    // ── RIS: M independent light candidates (Section Z: scales with nL) ──
    let n_cand = pt_restir_candidate_count(nL);
    for (var c = 0u; c < n_cand; c += 1u) {
        var pdf_pick = 0.0;
        let li = pt_light_table_sample(nL, p, n, v, albedo, rough, metal, light_total, &light_gcdf, seed, &pdf_pick);
        var ldir = vec3(0.0);
        var dist = 0.0;
        var fLe = vec3(0.0);
        var pdf = 0.0;
        var phat = 0.0;
        if (!pt_sample_light(li, nL, p, n, v, albedo, rough, metal, seed, pdf_pick, &ldir, &dist, &fLe, &pdf, &phat)) {
            // Still counts as a candidate attempt with weight 0 (keeps M honest)
            r.M += 1.0;
            continue;
        }
        let w = phat / max(pdf, 1e-10);
        pt_res_update(&r, f32(li), w, seed);
    }

    // Finalize initial reservoir for merge weights
    var phat_y = 0.0;
    if (r.y >= 0.0) {
        phat_y = pt_phat_for_light_table(u32(r.y), nL, p, n, v, albedo, rough, metal, seed, light_total);
        pt_res_finalize(&r, phat_y);
    }

    // ── Temporal reuse (Section AI: visibility gate) ──
    // Same-pixel load: under camera motion, Section I reproject pass warps
    // restir_prev into the current frame first. Gate on non-empty M (zero-init
    // and hard-reset frames stay empty) rather than only pt_accumulated.
    // Occluded temporal lights are dropped — prevents sticky shadow leaks.
    {
        let prev = pt_res_from_vec(textureLoad(pt_restir_prev, coord, 0));
        if (prev.y >= 0.0 && prev.M > 0.0 && prev.y < f32(nL)) {
            let li_p = u32(prev.y);
            let visible = pt_light_is_visible(p, n, li_p);
            vis_li = i32(li_p);
            vis_ok = visible;
            if (visible) {
                var prev_clamped = prev;
                // Cap temporal confidence so a stale bright sample can't dominate forever
                prev_clamped.M = min(prev.M, RESTIR_M_CLAMP * max(r.M, 1.0));
                let phat_prev = pt_phat_for_light_table(li_p, nL, p, n, v, albedo, rough, metal, seed, light_total);
                pt_res_merge(&r, prev_clamped, phat_prev, seed);
                if (r.y >= 0.0) {
                    phat_y = pt_phat_for_light_table(u32(r.y), nL, p, n, v, albedo, rough, metal, seed, light_total);
                    pt_res_finalize(&r, phat_y);
                }
            }
        }
    }

    // ── Spatial reuse (Section X: gbuf guides). Section Z: skip when nL==1 ──
    // Section AC: R2-stratified offsets; fewer taps when temporal confidence is high.
    // Section AJ: same-light visibility reuse — skip neighbors whose light is known occluded;
    //   when a new hard light is first seen, test once and cache for final shade.
    // Section B2: skip spatial entirely on half-res (orbit) — biggest interactive win.
    if (nL > 1u && !pt_is_half_res_lighting()) {
        let dim_i = vec2<i32>(i32(tex_dim.x), i32(tex_dim.y));
        let c_gc = pt_gi_gbuf_coord(coord.x, coord.y, tex_dim);
        let c_nd = textureLoad(pt_gbuf_normal, c_gc, 0);
        let c_alb = textureLoad(pt_gbuf_albedo, c_gc, 0).rgb;
        // Adaptive spatial count: lean when reservoir already strong / progressive deep
        var n_spat = RESTIR_SPATIAL_MAX;
        if (r.M >= RESTIR_M_CLAMP * 0.6) { n_spat = 2u; }
        else if (r.M >= 8.0) { n_spat = 3u; }
        if (nL < 4u) { n_spat = min(n_spat, 2u); }
        if (config.pt_accumulated > 32u && config.pt_samples_per_frame <= 1u) {
            n_spat = min(n_spat, 2u);
        }
        let spat_base = config.pt_frame_seed * 131u
            + u32(coord.x) * 1973u + u32(coord.y) * 9277u;
        // Section AJ: secondary visibility cache (in addition to temporal vis_li)
        var spat_vis_li: i32 = -1;
        var spat_vis_ok = false;
        for (var s = 0u; s < n_spat; s += 1u) {
            let r2s = pt_r2(spat_base + s * 17u);
            let ang = 2.0 * PT_PI * r2s.x;
            let rad = RESTIR_RADIUS * sqrt(r2s.y);
            let nc = coord + vec2<i32>(i32(cos(ang) * rad), i32(sin(ang) * rad));
            if (nc.x < 0 || nc.y < 0 || nc.x >= dim_i.x || nc.y >= dim_i.y) { continue; }
            let neigh = pt_res_from_vec(textureLoad(pt_restir_prev, nc, 0));
            if (neigh.y < 0.0 || neigh.M <= 0.0 || neigh.y >= f32(nL)) { continue; }

            let n_gc = pt_gi_gbuf_coord(nc.x, nc.y, tex_dim);
            let n_nd = textureLoad(pt_gbuf_normal, n_gc, 0);
            if (c_nd.w > 0.0 && n_nd.w > 0.0) {
                if (dot(c_nd.xyz, n_nd.xyz) < 0.85) { continue; }
                let rel = abs(c_nd.w - n_nd.w) / max(c_nd.w, 1e-3);
                if (rel > 0.08) { continue; }
                if (dot(n, n_nd.xyz) < 0.5) { continue; }
            } else if (c_nd.w > 0.0 || n_nd.w > 0.0) {
                continue;
            }
            let n_alb = textureLoad(pt_gbuf_albedo, n_gc, 0).rgb;
            if (length(c_alb - n_alb) > 0.4) { continue; }

            let li_n = u32(neigh.y);
            let soft_n = light_rt_radius(li_n);
            // Section AJ: hard-light visibility reuse / one-shot cache
            if (soft_n <= 1e-5) {
                if (vis_li == i32(li_n)) {
                    if (!vis_ok) { continue; } // known occluded from temporal gate
                } else if (spat_vis_li == i32(li_n)) {
                    if (!spat_vis_ok) { continue; }
                } else if (spat_vis_li < 0) {
                    // First new hard light among neighbors: one shadow ray, cache result
                    let v_ok = pt_light_is_visible(p, n, li_n);
                    spat_vis_li = i32(li_n);
                    spat_vis_ok = v_ok;
                    if (!v_ok) { continue; }
                    // Promote to primary cache if temporal had none
                    if (vis_li < 0) {
                        vis_li = spat_vis_li;
                        vis_ok = spat_vis_ok;
                    }
                }
                // else: second+ distinct hard light — merge unshadowed (cost cap)
            }

            var neigh_c = neigh;
            neigh_c.M = min(neigh.M, RESTIR_M_CLAMP);
            let phat_n = pt_phat_for_light_table(li_n, nL, p, n, v, albedo, rough, metal, seed, light_total);
            pt_res_merge(&r, neigh_c, phat_n, seed);
        }
        if (r.y >= 0.0) {
            phat_y = pt_phat_for_light_table(u32(r.y), nL, p, n, v, albedo, rough, metal, seed, light_total);
            pt_res_finalize(&r, phat_y);
        }
        // Prefer spatial cache for final shade if it matches selected light
        if (spat_vis_li >= 0 && r.y >= 0.0 && spat_vis_li == i32(r.y)) {
            vis_li = spat_vis_li;
            vis_ok = spat_vis_ok;
        }
    }

    // Persist reservoir for next frame (even if final shade fails visibility)
    textureStore(pt_restir_out, coord, pt_res_to_vec(r));

    if (r.y < 0.0 || r.W <= 0.0) { return vec3(0.0); }

    // ── Final shade: visibility (Section AI/AJ: reuse temporal/spatial gate) ──
    let li = u32(r.y);
    var ldir = vec3(0.0);
    var dist = 0.0;
    var fLe = vec3(0.0);
    var pdf = 0.0;
    var phat = 0.0;
    let pdf_pick = pt_light_table_pdf(li, nL, p, n, v, albedo, rough, metal, light_total);
    if (!pt_sample_light(li, nL, p, n, v, albedo, rough, metal, seed, pdf_pick, &ldir, &dist, &fLe, &pdf, &phat)) {
        return vec3(0.0);
    }
    // Reuse cached hard-light shadow when selected light matches; else one occlusion ray.
    // Soft lights still need a fresh ray (center vs soft sample can differ).
    let soft_r = light_rt_radius(li);
    var occluded = false;
    if (soft_r <= 1e-5 && vis_li == i32(li)) {
        occluded = !vis_ok;
    } else {
        occluded = pt_occluded(p + n * PT_EPS * 2.0, ldir, dist - PT_EPS * 4.0);
    }
    if (occluded) {
        return vec3(0.0);
    }

    // MIS vs BSDF for soft lights (delta → weight 1)
    let pdf_b = pt_pdf_bsdf(n, v, ldir, rough, metal);
    let soft = light_rt_radius(li);
    var w_mis = 1.0;
    if (soft > 1e-5 && pdf > 1e-8) {
        w_mis = pt_balance_mis(pdf, pdf_b);
    }
    // ReSTIR: contribution = f*Le * V * W  (p_hat was unshadowed luminance; W absorbs RIS)
    // Scale by phat/luminance so chromatic fLe is preserved: fLe * W * w_mis
    return fLe * r.W * w_mis;
}

// Section N: multi-candidate RIS NEE for secondary bounces.
// Samples K lights with power pick (no shadow), reservoir-updates, then ONE
// visibility ray on the selected light. Cheaper than full ReSTIR DI (no
// temporal/spatial textures) but much lower variance than single-sample NEE.
// Bounce-0 still uses pt_restir_di (temporal+spatial reservoirs).
fn pt_nee(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    let nL = min(num_lights(), PT_MAX_LIGHTS);
    if (nL == 0u) { return vec3(0.0); }

    // Section Z: candidate count scales with nL (1 when single light)
    let n_cand = pt_nee_ris_candidate_count(nL);

    // Section Q+AD+AF+AG: hierarchical BRDF-aware spatial light table, then K RIS
    var light_gcdf: array<f32, 16>;
    let light_total = pt_light_table_build(nL, p, n, v, albedo, rough, metal, &light_gcdf);
    // Section AB: no usable light power at this point
    if (light_total < 1e-10) { return vec3(0.0); }

    var r = pt_res_empty();
    for (var c = 0u; c < n_cand; c += 1u) {
        var pdf_pick = 0.0;
        let li = pt_light_table_sample(nL, p, n, v, albedo, rough, metal, light_total, &light_gcdf, seed, &pdf_pick);
        var ldir = vec3(0.0);
        var dist = 0.0;
        var fLe = vec3(0.0);
        var pdf = 0.0;
        var phat = 0.0;
        if (!pt_sample_light(li, nL, p, n, v, albedo, rough, metal, seed, pdf_pick, &ldir, &dist, &fLe, &pdf, &phat)) {
            r.M += 1.0;
            continue;
        }
        // RIS weight = p_hat / p_source
        let w = phat / max(pdf, 1e-10);
        pt_res_update(&r, f32(li), w, seed);
    }

    if (r.y < 0.0 || r.M <= 0.0) { return vec3(0.0); }

    // Finalize unbiased contribution weight W
    let li = u32(r.y);
    var ldir = vec3(0.0);
    var dist = 0.0;
    var fLe = vec3(0.0);
    var pdf_light = 0.0;
    var phat = 0.0;
    let pdf_pick = pt_light_table_pdf(li, nL, p, n, v, albedo, rough, metal, light_total);
    if (!pt_sample_light(li, nL, p, n, v, albedo, rough, metal, seed, pdf_pick, &ldir, &dist, &fLe, &pdf_light, &phat)) {
        return vec3(0.0);
    }
    pt_res_finalize(&r, phat);
    if (r.W <= 0.0) { return vec3(0.0); }

    // One shadow ray on the selected sample
    if (pt_occluded(p + n * PT_EPS * 2.0, ldir, dist - PT_EPS * 4.0)) {
        return vec3(0.0);
    }

    let pdf_b = pt_pdf_bsdf(n, v, ldir, rough, metal);
    let soft = light_rt_radius(li);
    var w_mis = 1.0;
    if (soft > 1e-5 && pdf_light > 1e-8) {
        w_mis = pt_balance_mis(pdf_light, pdf_b);
    }
    // f*Le * W * MIS  (W absorbs RIS multi-sample weight)
    return fLe * r.W * w_mis;
}

// ── Section K: env luminance CDF importance sampling ──────────────────────
// Buffer: [w,h,total,valid][marginal h][row_cdf h*w]  (built on CPU from L0).
@group(0) @binding(54) var<storage, read> pt_env_cdf: array<f32>;

fn pt_env_cdf_valid() -> bool {
    return arrayLength(&pt_env_cdf) >= 4u && pt_env_cdf[3] > 0.5 && pt_env_cdf[0] >= 1.0 && pt_env_cdf[1] >= 1.0;
}

// Inverse of env_rotate (Y-axis) so CDF map-space dirs become world dirs.
fn pt_env_unrotate(dir: vec3<f32>) -> vec3<f32> {
    let c = cos(config.env_rotation);
    let s = sin(config.env_rotation);
    return vec3(c * dir.x + s * dir.z, dir.y, -s * dir.x + c * dir.z);
}

// Binary search first index where cdf[i] >= u (cdf length n, starts at off).
fn pt_cdf_bsearch(off: u32, n: u32, u: f32) -> u32 {
    var lo = 0u;
    var hi = n;
    loop {
        if (lo >= hi) { break; }
        let mid = (lo + hi) / 2u;
        if (pt_env_cdf[off + mid] < u) { lo = mid + 1u; }
        else { hi = mid; }
    }
    return min(lo, n - 1u);
}

// Sample world direction from env CDF. Writes solid-angle pdf.
fn pt_sample_env_cdf(seed: ptr<function, u32>, out_pdf: ptr<function, f32>) -> vec3<f32> {
    let w = u32(pt_env_cdf[0]);
    let h = u32(pt_env_cdf[1]);
    let total = pt_env_cdf[2];
    let u1 = pt_rand(seed);
    let u2 = pt_rand(seed);
    let y = pt_cdf_bsearch(4u, h, u1);
    let row_off = 4u + h + y * w;
    let x = pt_cdf_bsearch(row_off, w, u2);
    // Continuous UV jitter inside texel
    let u = (f32(x) + pt_rand(seed)) / f32(w);
    let v = (f32(y) + pt_rand(seed)) / f32(h);
    let phi = u * 2.0 * PT_PI - PT_PI;
    let theta = v * PT_PI;
    let st = sin(theta);
    let ct = cos(theta);
    // Map-space direction (matches sample_env_at_level equirect)
    let map_dir = vec3(st * cos(phi), ct, st * sin(phi));
    let world_dir = pt_env_unrotate(map_dir);

    // Discrete map pdf → solid angle pdf
    // p_uv ≈ (func[x,y]/total) but we reconstruct via adjacent CDF deltas
    var py = pt_env_cdf[4u + y];
    if (y > 0u) { py = py - pt_env_cdf[4u + y - 1u]; }
    var px = pt_env_cdf[row_off + x];
    if (x > 0u) { px = px - pt_env_cdf[row_off + x - 1u]; }
    let p_uv = max(px * py, 1e-12); // joint probability of texel
    let sin_theta = max(st, 1e-5);
    // pdf_ω = p_texel * (w*h) / (2π² sinθ)
    *out_pdf = p_uv * f32(w * h) / max(2.0 * PT_PI * PT_PI * sin_theta, 1e-8);
    // Silence unused total (kept for CPU parity / future)
    let _t = total;
    return world_dir;
}

// Solid-angle pdf of a world direction under the env CDF (for MIS on path miss).
fn pt_env_pdf_dir(dir_world: vec3<f32>) -> f32 {
    if (!pt_env_cdf_valid()) { return 0.0; }
    let w = u32(pt_env_cdf[0]);
    let h = u32(pt_env_cdf[1]);
    // Rotate into map space (same as sample_env)
    let dir = env_rotate(dir_world);
    let d = normalize(dir);
    let theta = acos(clamp(d.y, -1.0, 1.0));
    let phi = atan2(d.z, d.x);
    var u = (phi + PT_PI) / (2.0 * PT_PI);
    u = u - floor(u);
    let v = theta / PT_PI;
    let x = min(u32(u * f32(w)), w - 1u);
    let y = min(u32(v * f32(h)), h - 1u);
    var py = pt_env_cdf[4u + y];
    if (y > 0u) { py = py - pt_env_cdf[4u + y - 1u]; }
    let row_off = 4u + h + y * w;
    var px = pt_env_cdf[row_off + x];
    if (x > 0u) { px = px - pt_env_cdf[row_off + x - 1u]; }
    let p_uv = max(px * py, 0.0);
    let sin_theta = max(sin(theta), 1e-5);
    return p_uv * f32(w * h) / max(2.0 * PT_PI * PT_PI * sin_theta, 1e-8);
}

// Section H+K: Environment NEE — HDRI via luminance CDF when available, else
// BSDF-shaped sample. MIS vs path-extension miss using true pdfs.
fn pt_env_nee(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    can_continue: bool,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    var l: vec3<f32>;
    var pdf_env = 0.0;
    var pdf_b = 0.0;
    var f_cos = vec3(0.0);

    if (pt_env_cdf_valid() && config.env_width > 0u) {
        // Section K: importance-sample bright env regions
        l = pt_sample_env_cdf(seed, &pdf_env);
        if (dot(n, l) <= 0.0 || pdf_env < 1e-10) { return vec3(0.0); }
        f_cos = pt_eval_f_cos(n, v, l, albedo, rough, metal);
        if (max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) { return vec3(0.0); }
        pdf_b = pt_pdf_bsdf(n, v, l, rough, metal);
    } else {
        // Fallback (no HDRI / empty CDF): BSDF-shaped env sample (Section H)
        l = pt_sample_bsdf(n, v, albedo, rough, metal, seed, &pdf_b, &f_cos);
        if (pdf_b < 1e-6 || max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) {
            return vec3(0.0);
        }
        if (dot(n, l) <= 0.0) { return vec3(0.0); }
        pdf_env = pdf_b; // same strategy → balance 0.5
    }

    let far_t = max(config.far_clip * 0.99, 1e3);
    if (pt_occluded(p + n * PT_EPS * 2.0, l, far_t)) {
        return vec3(0.0);
    }
    let Le = pt_miss_env(l);
    if (max(Le.r, max(Le.g, Le.b)) < 1e-8) { return vec3(0.0); }

    var w_mis = 1.0;
    if (can_continue) {
        w_mis = pt_balance_mis(pdf_env, max(pdf_b, 1e-10));
    }
    return f_cos * Le * w_mis / max(pdf_env, 1e-8);
}

// ── Section R+W+AE+AH: ReSTIR GI path reservoirs (lean path) ──────────────
// Used when max_bounces ≤ 2 (interactive / settled lean). Samples 1–2 BSDF
// bounces, shades hits (emission + NEE + env).
// Section W: second bounce when max_bounces ≥ 2 for richer color bleed.
// Section AE: multi-candidate RIS over GI paths.
// Section AH: store path reservoirs in pt_gi (oct.xy, W, M); temporal/spatial
//   reuse re-evaluates the stored world-space direction at the current hit.

const GI_RIS_CANDIDATES_MIN: u32 = 1u;
const GI_RIS_CANDIDATES_MAX: u32 = 4u;
const GI_RESTIR_M_CLAMP: f32 = 20.0;
const GI_SPATIAL_N_MAX: u32 = 4u;
const GI_SPATIAL_RADIUS: f32 = 6.0;

// Octahedral unit-vector encode/decode (world-space GI path direction).
fn pt_oct_encode(v: vec3<f32>) -> vec2<f32> {
    var n = v / max(abs(v.x) + abs(v.y) + abs(v.z), 1e-8);
    if (n.z < 0.0) {
        let ox = n.x;
        let oy = n.y;
        n.x = (1.0 - abs(oy)) * select(-1.0, 1.0, ox >= 0.0);
        n.y = (1.0 - abs(ox)) * select(-1.0, 1.0, oy >= 0.0);
    }
    return clamp(n.xy, vec2(-1.0), vec2(1.0));
}

fn pt_oct_decode(f: vec2<f32>) -> vec3<f32> {
    var n = vec3(f.x, f.y, 1.0 - abs(f.x) - abs(f.y));
    if (n.z < 0.0) {
        let ox = n.x;
        let oy = n.y;
        n.x = (1.0 - abs(oy)) * select(-1.0, 1.0, ox >= 0.0);
        n.y = (1.0 - abs(ox)) * select(-1.0, 1.0, oy >= 0.0);
    }
    return normalize(n);
}

// Section AH GI reservoir packing in pt_gi_* (rgba16f):
//   rg = octahedral direction [-1,1], b = W, a = M  (a<=0 → empty)
fn pt_gi_res_pack(dir: vec3<f32>, W: f32, M: f32) -> vec4<f32> {
    let oct = pt_oct_encode(dir);
    return vec4(oct.x, oct.y, W, M);
}

fn pt_gi_res_empty_v() -> vec4<f32> {
    return vec4(0.0, 0.0, 0.0, 0.0);
}

fn pt_gi_res_valid(r: vec4<f32>) -> bool {
    return r.a > 0.5 && r.r == r.r && r.b == r.b;
}

fn pt_shade_secondary(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    // Emission already handled by caller if needed; here NEE + env at secondary
    var L = pt_nee(p, n, v, albedo, rough, metal, seed);
    L += pt_env_nee(p, n, v, albedo, rough, metal, false, seed);
    return L;
}

// Unpack hit material scalars (albedo/rough/metal/emission) for GI path.
// Also samples albedo/rough/metal maps (parity with primary bounce) when present.
fn pt_gi_hit_mat(
    hit: PtHit,
    out_albedo: ptr<function, vec3<f32>>,
    out_rough: ptr<function, f32>,
    out_metal: ptr<function, f32>,
    out_em: ptr<function, vec3<f32>>,
) {
    let mat = pt_hit_material(hit);
    var albedo = mat.albedo.rgb;
    var rough = clamp(mat.roughness_metallic.x, 0.03, 1.0);
    var metal = clamp(mat.roughness_metallic.y, 0.0, 1.0);
    let p_local = pt_verts[pt_tris[hit.tri].v0].pos * (1.0 - hit.u - hit.v)
        + pt_verts[pt_tris[hit.tri].v1].pos * hit.u
        + pt_verts[pt_tris[hit.tri].v2].pos * hit.v;
    let n_local = normalize(
        pt_verts[pt_tris[hit.tri].v0].normal * (1.0 - hit.u - hit.v)
        + pt_verts[pt_tris[hit.tri].v1].normal * hit.u
        + pt_verts[pt_tris[hit.tri].v2].normal * hit.v);
    let face_uv = pt_hit_uv(hit);
    let amap_slot = bitcast<u32>(mat.albedo_map.x);
    if (amap_slot != 0u) {
        let amap_pack = bitcast<u32>(mat.albedo_map.w);
        let s = pt_sample_mat_rgb(
            amap_slot, p_local, n_local, face_uv,
            max(mat.albedo_map.z, 0.05), amap_pack);
        let str = clamp(mat.albedo_map.y, 0.0, 1.0);
        if ((amap_pack & 2u) != 0u) {
            albedo = mix(albedo, s, str);
        } else {
            albedo = mix(albedo, albedo * s, str);
        }
    }
    let rmap_slot = bitcast<u32>(mat.roughness_map.x);
    if (rmap_slot != 0u) {
        let s = pt_sample_mat_scalar(
            rmap_slot, p_local, n_local, face_uv,
            max(mat.roughness_map.z, 0.05), bitcast<u32>(mat.roughness_map.w));
        rough = mix(rough, clamp(s, 0.03, 1.0), clamp(mat.roughness_map.y, 0.0, 1.0));
    }
    let mmap_slot = bitcast<u32>(mat.metallic_map.x);
    if (mmap_slot != 0u) {
        let s = pt_sample_mat_scalar(
            mmap_slot, p_local, n_local, face_uv,
            max(mat.metallic_map.z, 0.05), bitcast<u32>(mat.metallic_map.w));
        metal = mix(metal, clamp(s, 0.0, 1.0), clamp(mat.metallic_map.y, 0.0, 1.0));
    }
    *out_albedo = albedo;
    *out_rough = rough;
    *out_metal = metal;
    *out_em = mat.emission_color.rgb * mat.roughness_metallic.z;
}

// Section AE: adaptive GI RIS candidate count.
// Mirrors stay 1 (delta); rough diffuse gets more; progressive idle densifies.
fn pt_gi_ris_candidate_count(rough: f32, metal: f32) -> u32 {
    if (metal > 0.85 && rough < 0.08) { return 1u; }
    // Section B2: half-res interactive — single GI path candidate
    if (pt_is_half_res_lighting()) {
        return GI_RIS_CANDIDATES_MIN;
    }
    var n = 2u;
    if (rough > 0.35) { n = 3u; }
    // First interactive frames: keep cost low (single path)
    if (config.pt_accumulated <= 1u && config.pt_samples_per_frame <= 1u) {
        return GI_RIS_CANDIDATES_MIN;
    }
    // Multi-spp densifies; deep progressive leans (temporal GI reservoir carries)
    if (config.pt_samples_per_frame >= 2u) {
        n = max(n, 3u);
    } else if (config.pt_accumulated > 6u && config.pt_accumulated <= 24u) {
        n = max(n, 3u);
    }
    if (config.pt_samples_per_frame >= 2u && config.pt_accumulated > 16u) {
        n = GI_RIS_CANDIDATES_MAX;
    }
    if (config.pt_accumulated > 32u && config.pt_samples_per_frame <= 1u) {
        n = min(n, 2u);
    }
    return n;
}

// Trace secondary along fixed direction l; write C = f_cos * L_sec (no /pdf).
fn pt_gi_trace_along(
    p: vec3<f32>,
    n: vec3<f32>,
    l: vec3<f32>,
    f_cos: vec3<f32>,
    gi_bounces: u32,
    allow_second: bool,
    seed: ptr<function, u32>,
    out_C: ptr<function, vec3<f32>>,
) -> bool {
    if (dot(l, n) <= 1e-5 || max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) {
        return false;
    }
    let ro = p + n * PT_EPS * 2.0;
    let hit = pt_trace_scene(ro, l, config.far_clip);
    var L_sec: vec3<f32>;
    if (!hit.hit) {
        L_sec = pt_miss_env(l);
    } else {
        let hp = ro + l * hit.t;
        var hn = pt_hit_normal(hit, l);
        var halbedo = vec3(0.5);
        var hrough = 0.5;
        var hmetal = 0.0;
        var hem = vec3(0.0);
        pt_gi_hit_mat(hit, &halbedo, &hrough, &hmetal, &hem);
        // P4: secondary shading normal also reads normal/bump maps.
        {
            let mat_h = pt_hit_material(hit);
            let p_loc = pt_verts[pt_tris[hit.tri].v0].pos * (1.0 - hit.u - hit.v)
                + pt_verts[pt_tris[hit.tri].v1].pos * hit.u
                + pt_verts[pt_tris[hit.tri].v2].pos * hit.v;
            let n_loc = normalize(
                pt_verts[pt_tris[hit.tri].v0].normal * (1.0 - hit.u - hit.v)
                + pt_verts[pt_tris[hit.tri].v1].normal * hit.u
                + pt_verts[pt_tris[hit.tri].v2].normal * hit.v);
            hn = pt_apply_normal_maps(hit, mat_h, p_loc, n_loc, vec2(hit.u, hit.v), hn, l);
        }
        let hv = -l;
        L_sec = hem + pt_shade_secondary(hp, hn, hv, halbedo, hrough, hmetal, seed);

        // Section W: optional second bounce (skip when re-evaling mirror / 1-bounce)
        if (gi_bounces >= 2u && allow_second) {
            var pdf2 = 0.0;
            var f2 = vec3(0.0);
            let l2 = pt_sample_bsdf(hn, hv, halbedo, hrough, hmetal, seed, &pdf2, &f2);
            if (pdf2 > 1e-6 && max(f2.r, max(f2.g, f2.b)) > 1e-8) {
                let thr1 = f2 / max(pdf2, 1e-8);
                let ro2 = hp + hn * PT_EPS * 2.0;
                let hit2 = pt_trace_scene(ro2, l2, config.far_clip);
                if (!hit2.hit) {
                    L_sec += thr1 * pt_miss_env(l2);
                } else {
                    let hp2 = ro2 + l2 * hit2.t;
                    var hn2 = pt_hit_normal(hit2, l2);
                    var a2 = vec3(0.5);
                    var r2 = 0.5;
                    var m2 = 0.0;
                    var e2 = vec3(0.0);
                    pt_gi_hit_mat(hit2, &a2, &r2, &m2, &e2);
                    {
                        let mat2 = pt_hit_material(hit2);
                        let p2 = pt_verts[pt_tris[hit2.tri].v0].pos * (1.0 - hit2.u - hit2.v)
                            + pt_verts[pt_tris[hit2.tri].v1].pos * hit2.u
                            + pt_verts[pt_tris[hit2.tri].v2].pos * hit2.v;
                        let n2 = normalize(
                            pt_verts[pt_tris[hit2.tri].v0].normal * (1.0 - hit2.u - hit2.v)
                            + pt_verts[pt_tris[hit2.tri].v1].normal * hit2.u
                            + pt_verts[pt_tris[hit2.tri].v2].normal * hit2.v);
                        hn2 = pt_apply_normal_maps(hit2, mat2, p2, n2, vec2(hit2.u, hit2.v), hn2, l2);
                    }
                    let hv2 = -l2;
                    var L3 = e2 + pt_shade_secondary(hp2, hn2, hv2, a2, r2, m2, seed);
                    L3 = min(L3, vec3(12.0));
                    L_sec += thr1 * L3;
                }
            }
        }
        L_sec = min(L_sec, vec3(20.0));
    }
    *out_C = f_cos * L_sec;
    return true;
}

// Section W/AA/AH: one GI path candidate — also returns world direction for reservoir.
fn pt_sample_gi_candidate(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    gi_bounces: u32,
    seed: ptr<function, u32>,
    out_C: ptr<function, vec3<f32>>,
    out_pdf: ptr<function, f32>,
    out_dir: ptr<function, vec3<f32>>,
) -> bool {
    var pdf_b = 0.0;
    var f_cos = vec3(0.0);
    var l: vec3<f32>;
    let mirror = metal > 0.85 && rough < 0.08;
    if (mirror) {
        l = reflect(-v, n);
        if (dot(l, n) <= 0.0) { return false; }
        f_cos = pt_eval_f_cos(n, v, l, albedo, rough, metal);
        pdf_b = max(pt_pdf_bsdf(n, v, l, rough, metal), 1e-3);
        if (max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) { return false; }
    } else {
        l = pt_sample_bsdf(n, v, albedo, rough, metal, seed, &pdf_b, &f_cos);
        if (pdf_b < 1e-6 || max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) {
            return false;
        }
    }
    if (!pt_gi_trace_along(p, n, l, f_cos, gi_bounces, !mirror, seed, out_C)) {
        return false;
    }
    *out_pdf = max(pdf_b, 1e-8);
    *out_dir = l;
    return true;
}

// Section AH: re-evaluate a stored GI direction at the current shading point.
fn pt_eval_gi_dir(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    l: vec3<f32>,
    gi_bounces: u32,
    seed: ptr<function, u32>,
    out_C: ptr<function, vec3<f32>>,
    out_pdf: ptr<function, f32>,
) -> bool {
    let f_cos = pt_eval_f_cos(n, v, l, albedo, rough, metal);
    let pdf_b = max(pt_pdf_bsdf(n, v, l, rough, metal), 1e-8);
    let mirror = metal > 0.85 && rough < 0.08;
    if (!pt_gi_trace_along(p, n, l, f_cos, gi_bounces, !mirror, seed, out_C)) {
        return false;
    }
    *out_pdf = pdf_b;
    return true;
}

// Map lighting pixel → full-res gbuf pixel (half lighting uses scale 2).
fn pt_gi_gbuf_coord(lx: i32, ly: i32, lighting_dim: vec2<u32>) -> vec2<i32> {
    let gdim = textureDimensions(pt_gbuf_normal);
    let lw = max(lighting_dim.x, 1u);
    let lh = max(lighting_dim.y, 1u);
    let gx = i32((u32(max(lx, 0)) * gdim.x + lw / 2u) / lw);
    let gy = i32((u32(max(ly, 0)) * gdim.y + lh / 2u) / lh);
    return vec2<i32>(
        clamp(gx, 0, i32(gdim.x) - 1),
        clamp(gy, 0, i32(gdim.y) - 1),
    );
}

// Section AE+AH: multi-candidate RIS + temporal/spatial path reservoirs.
// Writes packed reservoir to pt_gi_out; returns unbiased GI radiance C*W.
fn pt_restir_gi(
    coord: vec2<i32>,
    tex_dim: vec2<u32>,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    gi_bounces: u32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    let n_cand = pt_gi_ris_candidate_count(rough, metal);

    var w_sum = 0.0;
    var M = 0.0;
    var sel_C = vec3(0.0);
    var sel_dir = vec3(0.0, 1.0, 0.0);
    var sel_phat = 0.0;

    // ── Initial RIS over new path candidates ──
    for (var c = 0u; c < n_cand; c += 1u) {
        var C = vec3(0.0);
        var pdf = 0.0;
        var dir = vec3(0.0);
        M += 1.0;
        if (!pt_sample_gi_candidate(p, n, v, albedo, rough, metal, gi_bounces, seed, &C, &pdf, &dir)) {
            continue;
        }
        let phat = pt_luminance(C);
        if (phat < 1e-10) { continue; }
        let w = phat / max(pdf, 1e-8);
        w_sum += w;
        if (w_sum > 0.0 && pt_rand(seed) * w_sum < w) {
            sel_C = C;
            sel_dir = dir;
            sel_phat = phat;
        }
    }

    // ── Temporal path-reservoir merge (Section AH) ──
    if (config.pt_accumulated > 1u) {
        let prev = textureLoad(pt_gi_prev, coord, 0);
        if (pt_gi_res_valid(prev)) {
            let pdir = pt_oct_decode(prev.rg);
            var C_p = vec3(0.0);
            var pdf_p = 0.0;
            if (pt_eval_gi_dir(p, n, v, albedo, rough, metal, pdir, gi_bounces, seed, &C_p, &pdf_p)) {
                let phat_p = pt_luminance(C_p);
                if (phat_p > 1e-10 && prev.b > 0.0) {
                    let M_p = min(prev.a, GI_RESTIR_M_CLAMP * max(M, 1.0));
                    // Stream weight of prev selected sample
                    let w_p = prev.b * phat_p * M_p;
                    if (w_p > 0.0) {
                        w_sum += w_p;
                        M += M_p;
                        if (w_sum > 0.0 && pt_rand(seed) * w_sum < w_p) {
                            sel_C = C_p;
                            sel_dir = pdir;
                            sel_phat = phat_p;
                        }
                    }
                }
            }
        }
    }

    // ── Spatial path-reservoir merge (gbuf-guided, Section T/U/AC/AH) ──
    // Section B2: skip spatial GI on half-res (multi-tap re-eval is expensive).
    if (config.pt_accumulated > 1u && M > 0.0 && !pt_is_half_res_lighting()) {
        let dim_i = vec2<i32>(i32(tex_dim.x), i32(tex_dim.y));
        let c_gc = pt_gi_gbuf_coord(coord.x, coord.y, tex_dim);
        let c_nd = textureLoad(pt_gbuf_normal, c_gc, 0);
        let c_alb = textureLoad(pt_gbuf_albedo, c_gc, 0).rgb;
        var n_spat = GI_SPATIAL_N_MAX;
        if (M >= GI_RESTIR_M_CLAMP * 0.6) { n_spat = 2u; }
        else if (M >= 8.0) { n_spat = 3u; }
        if (config.pt_accumulated > 32u && config.pt_samples_per_frame <= 1u) {
            n_spat = min(n_spat, 2u);
        }
        let gi_rad = select(GI_SPATIAL_RADIUS * 1.35, GI_SPATIAL_RADIUS, M >= 4.0);
        let spat_base = config.pt_frame_seed * 173u
            + u32(coord.x) * 1493u + u32(coord.y) * 8861u;
        for (var s = 0u; s < n_spat; s += 1u) {
            let r2s = pt_r2(spat_base + s * 23u);
            let ang = 2.0 * PT_PI * r2s.x;
            let rad = gi_rad * sqrt(r2s.y);
            let nc = coord + vec2<i32>(i32(cos(ang) * rad), i32(sin(ang) * rad));
            if (nc.x < 0 || nc.y < 0 || nc.x >= dim_i.x || nc.y >= dim_i.y) { continue; }
            let neigh = textureLoad(pt_gi_prev, nc, 0);
            if (!pt_gi_res_valid(neigh) || neigh.b <= 0.0) { continue; }

            let n_gc = pt_gi_gbuf_coord(nc.x, nc.y, tex_dim);
            let n_nd = textureLoad(pt_gbuf_normal, n_gc, 0);
            if (c_nd.w > 0.0 && n_nd.w > 0.0) {
                if (dot(c_nd.xyz, n_nd.xyz) < 0.85) { continue; }
                let rel = abs(c_nd.w - n_nd.w) / max(c_nd.w, 1e-3);
                if (rel > 0.08) { continue; }
                if (dot(n, n_nd.xyz) < 0.5) { continue; }
            } else if (c_nd.w > 0.0 || n_nd.w > 0.0) {
                continue;
            }
            let n_alb = textureLoad(pt_gbuf_albedo, n_gc, 0).rgb;
            if (length(c_alb - n_alb) > 0.35) { continue; }

            let ndir = pt_oct_decode(neigh.rg);
            var C_n = vec3(0.0);
            var pdf_n = 0.0;
            if (!pt_eval_gi_dir(p, n, v, albedo, rough, metal, ndir, gi_bounces, seed, &C_n, &pdf_n)) {
                continue;
            }
            let phat_n = pt_luminance(C_n);
            if (phat_n < 1e-10) { continue; }
            let M_n = min(neigh.a, GI_RESTIR_M_CLAMP);
            let w_n = neigh.b * phat_n * M_n;
            if (w_n <= 0.0) { continue; }
            w_sum += w_n;
            M += M_n;
            if (w_sum > 0.0 && pt_rand(seed) * w_sum < w_n) {
                sel_C = C_n;
                sel_dir = ndir;
                sel_phat = phat_n;
            }
        }
    }

    if (sel_phat < 1e-10 || M < 1.0 || w_sum <= 0.0) {
        textureStore(pt_gi_out, coord, pt_gi_res_empty_v());
        return vec3(0.0);
    }

    let W = w_sum / (M * sel_phat);
    var L = sel_C * W;
    L = min(L, vec3(20.0));

    // Persist path reservoir for next frame (reproject warps UV; dir is world-space)
    textureStore(pt_gi_out, coord, pt_gi_res_pack(sel_dir, W, min(M, 64.0)));
    return L;
}

// Lightweight single-shot GI (no temporal write) — used when restir not needed.
fn pt_sample_gi(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    gi_bounces: u32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    let n_cand = pt_gi_ris_candidate_count(rough, metal);
    var w_sum = 0.0;
    var M = 0.0;
    var sel_C = vec3(0.0);
    var sel_phat = 0.0;
    for (var c = 0u; c < n_cand; c += 1u) {
        var C = vec3(0.0);
        var pdf = 0.0;
        var dir = vec3(0.0);
        M += 1.0;
        if (!pt_sample_gi_candidate(p, n, v, albedo, rough, metal, gi_bounces, seed, &C, &pdf, &dir)) {
            continue;
        }
        let phat = pt_luminance(C);
        if (phat < 1e-10) { continue; }
        let w = phat / max(pdf, 1e-8);
        w_sum += w;
        if (w_sum > 0.0 && pt_rand(seed) * w_sum < w) {
            sel_C = C;
            sel_phat = phat;
        }
    }
    if (sel_phat < 1e-10 || M < 1.0 || w_sum <= 0.0) { return vec3(0.0); }
    return min(sel_C * (w_sum / (M * sel_phat)), vec3(20.0));
}

fn pt_sample_1bounce_gi(
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
) -> vec3<f32> {
    return pt_sample_gi(p, n, v, albedo, rough, metal, 1u, seed);
}

// Sample next bounce direction (Lambert + GGX mix). Returns dir; writes pdf and f*cos into out.
fn pt_sample_bsdf(
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    seed: ptr<function, u32>,
    out_pdf: ptr<function, f32>,
    out_f_cos: ptr<function, vec3<f32>>,
) -> vec3<f32> {
    let spec_prob = clamp(metal + (1.0 - rough) * 0.35, 0.05, 0.95);
    var l: vec3<f32>;
    if (pt_rand(seed) < spec_prob) {
        // GGX: sample half-vector then reflect (simplified isotropic GGX)
        let a = max(rough * rough, 0.001);
        let u1 = pt_rand(seed);
        let u2 = pt_rand(seed);
        let phi = 2.0 * PT_PI * u1;
        let cos_theta = sqrt((1.0 - u2) / (1.0 + (a * a - 1.0) * u2));
        let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
        // Build TBN around n
        var t: vec3<f32>;
        if (abs(n.y) < 0.999) { t = normalize(cross(vec3(0.0, 1.0, 0.0), n)); }
        else { t = normalize(cross(vec3(1.0, 0.0, 0.0), n)); }
        let b = cross(n, t);
        let h = normalize(t * (sin_theta * cos(phi)) + b * (sin_theta * sin(phi)) + n * cos_theta);
        // A reflection below the horizon is NOT re-drawn: it carries zero BSDF (pdf 0 → every
        // caller terminates the path). Re-drawing it from the cosine lobe made the real sampling
        // density differ from `pt_pdf_bsdf`'s mixture → biased bright: the white furnace read up
        // to 1.79 (energy CREATED) on rough metal at grazing view (`pt_furnace_gpu_test`).
        l = reflect(-v, h);
    } else {
        l = pt_cosine_hemisphere(n, seed);
    }
    *out_pdf = pt_pdf_bsdf(n, v, l, rough, metal);
    *out_f_cos = pt_eval_f_cos(n, v, l, albedo, rough, metal);
    return l;
}

// Smooth (delta) dielectric transmission lobe for ShaderType::Glass.
// n_geo_raw = the RAW geometric normal (NOT camera-faced). `entering` = the ray is
// crossing from air into glass. Picks reflect-vs-transmit by exact dielectric Fresnel
// PROBABILITY, so the branch pdf cancels: the returned weight is just the (transmit-side)
// tint — DO NOT additionally divide by F (that is the classic firefly bug).
// The eta^2 radiance-compression factor is intentionally OMITTED: on a closed solid the
// enter (*eta_in^2) and exit (*1/eta_in^2) cancel to 1.0, and omitting it avoids a
// brightness bug on the common closed-mesh case (see PT_TRANSMISSION_PLAN §3d).
// out_dir = next ray direction; out_inside = the medium bit AFTER this event.
fn pt_sample_dielectric(
    ray_d: vec3<f32>,
    n_geo_raw: vec3<f32>,
    ior: f32,
    tint: vec3<f32>,
    entering: bool,
    inside: bool,
    seed: ptr<function, u32>,
    out_dir: ptr<function, vec3<f32>>,
    out_inside: ptr<function, bool>,
) -> vec3<f32> {
    let air = 1.0;
    let ii = max(ior, 1.0001);
    // eta = ior_from / ior_to; nf oriented to oppose the incoming ray.
    let eta = select(ii / air, air / ii, entering);
    let nf = select(-n_geo_raw, n_geo_raw, entering);
    let cos_i = clamp(abs(dot(ray_d, nf)), 0.0, 1.0);
    let fr = pt_fresnel_dielectric(cos_i, eta);
    // TIR (fr == 1.0) OR Fresnel-probability reflect branch.
    if (pt_rand(seed) >= (1.0 - fr)) {
        *out_dir = reflect(ray_d, nf);
        *out_inside = inside;              // reflection stays on the same side
        return vec3(1.0);                  // reflect side carries no transmit tint
    }
    // Transmit branch: F < 1 guaranteed, so refract() is non-TIR here.
    let rd = refract(ray_d, nf, eta);
    if (dot(rd, rd) < 1e-8) {
        // Numerical guard: refract() returned ~0 despite F<1 -> treat as reflect (no NaN).
        *out_dir = reflect(ray_d, nf);
        *out_inside = inside;
        return vec3(1.0);
    }
    *out_dir = normalize(rd);
    *out_inside = !inside;                 // crossed the interface
    // BEER-LAMBERT ABSORPTION SEAM: v1 = constant surface tint on transmit. Absorption
    // replaces this line with exp(-extinction * interior_segment_length), keyed on the
    // `inside` bool + accumulated hit distance between an enter and its matching exit.
    return tint;
}

@compute @workgroup_size(8, 8, 1)
fn pass_pathtrace_tri(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tex_dim = textureDimensions(output_texture);
    if (gid.x >= tex_dim.x || gid.y >= tex_dim.y) { return; }
    let coord = vec2<i32>(gid.xy);
    let w = f32(tex_dim.x);
    let h = f32(tex_dim.y);
    let aspect = w / h;

    // Section AA: empty scene — sky/env only, skip BVH/materials/GI entirely.
    if (pt_meta.tlas_count == 0u || pt_meta.instance_count == 0u || pt_meta.tri_count == 0u) {
        textureStore(pt_restir_out, coord, pt_res_to_vec(pt_res_empty()));
        textureStore(pt_gi_out, coord, vec4(0.0, 0.0, 0.0, 0.0));
        var sky_sum = vec3(0.0);
        var sky_moments = vec4(0.0);
        let spp_e = max(config.pt_samples_per_frame, 1u);
        for (var se = 0u; se < spp_e; se += 1u) {
            let r2e = pt_r2(config.pt_frame_seed * 7919u + gid.x * 1973u + gid.y * 9277u + se);
            let uv_x = ((f32(gid.x) + 0.5 + r2e.x - 0.5) / w) * 2.0 - 1.0;
            let uv_y = -(((f32(gid.y) + 0.5 + r2e.y - 0.5) / h) * 2.0 - 1.0);
            let cam = camera_ray(uv_x, uv_y, aspect);
            let sky_sample = pt_miss_camera(cam[1]);
            sky_sum += sky_sample;
            pt_add_luminance_sample(&sky_moments, sky_sample);
        }
        pt_store_progressive(coord, sky_sum / f32(spp_e), sky_moments);
        return;
    }

    // Base seed + R2 index (Section H): frame×pixel stream for quasirandom AA.
    var seed = pt_pcg_hash(gid.x + gid.y * tex_dim.x + config.pt_frame_seed * tex_dim.x * tex_dim.y);
    let r2_base = config.pt_frame_seed * 7919u
        + gid.x * 1973u
        + gid.y * 9277u;

    // Adaptive sample count (Section P) — lean caps (was 4× focus + up to 12 spp).
    // - Base from CPU (variance-guided global spp).
    // - 2× in mouse-focus region (idle cursor).
    // - Per-pixel: low history (disocclusion) gets a modest extra sample.
    let pixel_uv = vec2(f32(gid.x) / w, f32(gid.y) / h);
    var spp = max(config.pt_samples_per_frame, 1u);
    if (config.pt_focus_uv_x >= 0.0) {
        let fd = length(pixel_uv - vec2(config.pt_focus_uv_x, config.pt_focus_uv_y));
        if (fd < config.pt_focus_radius) { spp = spp * 2u; }
    }
    if (config.pt_accumulated > 1u) {
        let prev_h = textureLoad(pt_accum_in, coord, 0).a;
        // Fresh / disoccluded: +1 sample only (history progressive cleans the rest)
        if (prev_h < 4.0) { spp = max(spp, 2u); }
        else if (prev_h < 12.0) { spp = max(spp, config.pt_samples_per_frame); }
        // Hard cap — focus×disocclusion must not explode cost
        spp = min(spp, 4u);
    }

    // Thin-lens DOF focal plane: the TLAS root's center — the orbit camera's
    // subject for any normal scene. Zero-aperture = pinhole (slider off).
    var focus_dist = 1e8;
    if (config.pt_dof_aperture > 0.0 && pt_meta.tlas_count > 0u) {
        let root = pt_tlas[0];
        let center = (root.aabb_min + root.aabb_max) * 0.5;
        focus_dist = max(length(center - config.cam_pos.xyz), 0.05);
    }

    var sample_sum = vec3(0.0);
    var sample_moments = vec4(0.0);
    // Default empty reservoir; bounce-0 ReSTIR overwrites on a successful primary hit.
    textureStore(pt_restir_out, coord, pt_res_to_vec(pt_res_empty()));
    // Section AH: GI path reservoir (oct/W/M) — restir_gi writes on lean/warm
    textureStore(pt_gi_out, coord, pt_gi_res_empty_v());
    for (var s = 0u; s < spp; s += 1u) {
        // Section H: R2 sub-pixel jitter (low-discrepancy AA across frames)
        let r2 = pt_r2(r2_base + s);
        let jx = r2.x - 0.5;
        let jy = r2.y - 0.5;
        let uv_x = ((f32(gid.x) + 0.5 + jx) / w) * 2.0 - 1.0;
        let uv_y = -(((f32(gid.y) + 0.5 + jy) / h) * 2.0 - 1.0);
        let cam = camera_ray(uv_x, uv_y, aspect);
        var ray_o = cam[0];
        var ray_d = cam[1];

        // Thin-lens DOF: jitter origin on the aperture disc, re-aim at the focal point.
        if (config.pt_dof_aperture > 0.0) {
            let focal_p = ray_o + ray_d * focus_dist;
            let r = sqrt(pt_rand(&seed)) * config.pt_dof_aperture;
            let ang = 2.0 * PT_PI * pt_rand(&seed);
            let lens = config.cam_right.xyz * (r * cos(ang)) + config.cam_up.xyz * (r * sin(ang));
            ray_o += lens;
            ray_d = normalize(focal_p - ray_o);
        }

        var throughput = vec3(1.0);
        var radiance = vec3(0.0);
        // Section K: BSDF pdf of the last direction sample (for env MIS on miss).
        var last_pdf_b = 0.0;
        // Glass medium tracker (v1: solid, single medium — air outside, one ior inside).
        // This bool is exactly the state Beer-Lambert absorption will consume.
        var inside = false;
        // Separate transmission bounce budget so thick/stacked glass (enter+exit+internal
        // reflections) does not starve the diffuse pt_max_bounces and go black.
        var glass_bounce = 0u;

        for (var bounce = 0u; bounce <= config.pt_max_bounces; bounce += 1u) {
            let hit = pt_trace_scene(ray_o, ray_d, config.far_clip);
            if (!hit.hit) {
                // Primary miss: full env. Path-extension miss: MIS with env CDF pdf.
                var env_w = 1.0;
                if (bounce > 0u) {
                    let pdf_e = pt_env_pdf_dir(ray_d);
                    if (pdf_e > 1e-10 && last_pdf_b > 1e-10) {
                        env_w = pt_balance_mis(last_pdf_b, pdf_e);
                    } else {
                        env_w = 0.5; // fallback when CDF empty
                    }
                }
                // Camera ray (bounce 0) paints the visible background, so it — and only
                // it — honours the hidden-HDRI toggle. env_w is 1.0 there (no MIS), so
                // the flat plate arrives unweighted.
                var miss_col = pt_miss_env(ray_d);
                if (bounce == 0u && env_bg_hidden()) { miss_col = pt_bg_flat(ray_d); }
                // Fog toward environment (same density scale as hit-path fog).
                if (config.fog_density > 1e-6) {
                    let ft = min(config.fog_max_distance, 80.0);
                    let tr = exp(-config.fog_density * ft);
                    let fog_col = mix(unpack_rgb(config.bg_bottom_packed), unpack_rgb(config.bg_top_packed), 0.35)
                        * config.fog_scatter;
                    radiance += throughput * mix(fog_col, miss_col * env_w, tr);
                } else {
                    radiance += throughput * miss_col * env_w;
                }
                break;
            }
            var hit_p = ray_o + ray_d * hit.t;
            var n = pt_hit_normal(hit, ray_d);
            // Raw geometric/shading normal (interpolated + instance-transformed, NOT
            // camera-faced): required to tell entering from exiting for refraction. The
            // camera-faced `n` destroys that distinction (pt_hit_normal :392).
            let n_geo_raw = pt_hit_normal_raw(hit);
            let mat = pt_hit_material(hit);
            // Glass detection — matches the raster G-buffer trigger (gbuffer.wgsl:684).
            let shader_type = u32(mat.roughness_metallic.w + 0.5);
            let g_ior = mat.adv_params.y;
            let glass_tint = mat.adv_glass.xyz;
            let is_glass = shader_type == 2u
                || (shader_type == 4u && mat.albedo.w < 0.995 && g_ior > 1.01);
            // Body-local position for triplanar maps (pre-instance mesh space).
            let p_local = pt_verts[pt_tris[hit.tri].v0].pos * (1.0 - hit.u - hit.v)
                + pt_verts[pt_tris[hit.tri].v1].pos * hit.u
                + pt_verts[pt_tris[hit.tri].v2].pos * hit.v;
            let n_local = normalize(
                pt_verts[pt_tris[hit.tri].v0].normal * (1.0 - hit.u - hit.v)
                + pt_verts[pt_tris[hit.tri].v1].normal * hit.u
                + pt_verts[pt_tris[hit.tri].v2].normal * hit.v);
            let face_uv = pt_hit_uv(hit);

            // Easy volumetric fog along the free path to the hit (Beer's law + simple in-scatter).
            // Only when fog_density > 0 (UI Fog toggle packs 0 when off).
            if (config.fog_density > 1e-6) {
                let seg = max(hit.t, 0.0);
                let h_factor = exp(-max(hit_p.y, 0.0) * config.fog_height_falloff);
                // Density scale matches deferred fog feel without the *8 blowout (review fix).
                let optical = config.fog_density * seg * h_factor;
                let tr = exp(-optical);
                let fog_col = mix(unpack_rgb(config.bg_bottom_packed), unpack_rgb(config.bg_top_packed), 0.4)
                    * config.fog_scatter;
                radiance += throughput * fog_col * (1.0 - tr);
                throughput *= tr;
            }

            // Fast displacement: real height map sample → offset hit along normal.
            // Mid-gray = 0; strength ≈ world amplitude. Secondary rays leave from here.
            let dmap_slot = bitcast<u32>(mat.displacement_map.x);
            if (dmap_slot != 0u) {
                let h = pt_sample_mat_scalar(
                    dmap_slot, p_local, n_local, face_uv,
                    max(mat.displacement_map.z, 0.05), bitcast<u32>(mat.displacement_map.w));
                let amp = (h - 0.5) * 2.0 * clamp(mat.displacement_map.y, 0.0, 2.0);
                hit_p = hit_p + n * amp;
            }

            var albedo = mat.albedo.rgb;
            var rough = clamp(mat.roughness_metallic.x, 0.03, 1.0);
            var metal = clamp(mat.roughness_metallic.y, 0.0, 1.0);

            // Albedo map: absolute (bit1 of proj) = layered Mult stack bake → REPLACE;
            // else classic tint multiply (file import).
            let amap_slot = bitcast<u32>(mat.albedo_map.x);
            if (amap_slot != 0u) {
                let amap_pack = bitcast<u32>(mat.albedo_map.w);
                let s = pt_sample_mat_rgb(
                    amap_slot, p_local, n_local, face_uv,
                    max(mat.albedo_map.z, 0.05), amap_pack);
                let str = clamp(mat.albedo_map.y, 0.0, 1.0);
                if ((amap_pack & 2u) != 0u) {
                    albedo = mix(albedo, s, str);
                } else {
                    albedo = mix(albedo, albedo * s, str);
                }
            }
            // Roughness / metalness maps REPLACE scalar by strength (same as raster)
            let rmap_slot = bitcast<u32>(mat.roughness_map.x);
            if (rmap_slot != 0u) {
                let s = pt_sample_mat_scalar(
                    rmap_slot, p_local, n_local, face_uv,
                    max(mat.roughness_map.z, 0.05), bitcast<u32>(mat.roughness_map.w));
                rough = mix(rough, clamp(s, 0.03, 1.0), clamp(mat.roughness_map.y, 0.0, 1.0));
            }
            let mmap_slot = bitcast<u32>(mat.metallic_map.x);
            if (mmap_slot != 0u) {
                let s = pt_sample_mat_scalar(
                    mmap_slot, p_local, n_local, face_uv,
                    max(mat.metallic_map.z, 0.05), bitcast<u32>(mat.metallic_map.w));
                metal = mix(metal, clamp(s, 0.0, 1.0), clamp(mat.metallic_map.y, 0.0, 1.0));
            }

            // Normal / bump maps (P4 / HANDOFF Decision 3) — same whiteout triplanar as Hybrid.
            // Must run AFTER displacement (hit_p already offset) and BEFORE NEE/GI so lobes see
            // the perturbed shading normal. Geometric `n_geo_raw` stays for glass enter/exit.
            n = pt_apply_normal_maps(hit, mat, p_local, n_local, face_uv, n, ray_d);

            // --- Noise pattern layer (BODY-LOCAL FBM) — VERBATIM parity with gbuffer.wgsl's
            // live-preview layer so dragging node-editor knobs updates the path tracer the
            // SAME FRAME (no bake wait). Seed packing + blend-curve packing identical.
            // Skip when absolute Mult-stack albedo is bound (REPLACE would wipe base×noise bake).
            let np_amt = mat.noise_params.x;
            let amap_is_absolute = (bitcast<u32>(mat.albedo_map.w) & 2u) != 0u;
            if (np_amt > 0.001 && !amap_is_absolute) {
                let nscale = mat.noise_params.y;
                let noct = u32(clamp(mat.noise_params.z, 1.0, 64.0));
                var nseed = 0.0;
                if (mat.edge_wear.x < 0.001) {
                    nseed = mat.edge_wear.w;
                }
                let sp = p_local * nscale + vec3(nseed, nseed * 0.6180339887, nseed * 1.7320508075);
                var noise_val = pt_fbm3(sp, noct);
                let curve_pack = mat.cavity_dirt.w;
                if (curve_pack >= 100.0 && curve_pack < 105.0) {
                    let nblack = mat.cavity_dirt_color.x;
                    let nwhite = mat.cavity_dirt.y;
                    let nkk = max(mat.cavity_dirt.z, 0.001);
                    let nmode = u32(curve_pack - 100.0 + 0.5);
                    let ndenom = max(nwhite - nblack, 1e-6);
                    var x = saturate((noise_val - nblack) / ndenom);
                    if (nmode == 1u) {
                        x = x * x * (3.0 - 2.0 * x); // smoothstep
                    } else if (nmode == 2u) {
                        x = x * x * x * (x * (x * 6.0 - 15.0) + 10.0); // smootherstep
                    } else if (nmode == 3u) {
                        x = pow(x, nkk); // power
                    } else if (nmode == 4u) {
                        x = pow(x, 1.0 / nkk); // levels mid gamma
                    }
                    noise_val = x;
                }
                let nmask = saturate(noise_val * np_amt);
                albedo = mix(albedo, mat.noise_color.xyz, nmask);
                rough = mix(rough, mat.noise_color.w, nmask);
                metal = mix(metal, mat.noise_params.w, nmask);
            }

            // Emission: scalar * color, optionally multiplied by emission map
            var em = mat.emission_color.rgb * mat.roughness_metallic.z;
            let emap_slot = bitcast<u32>(mat.emission_map.x);
            if (emap_slot != 0u) {
                let s = pt_sample_mat_rgb(
                    emap_slot, p_local, n_local, face_uv,
                    max(mat.emission_map.z, 0.05), bitcast<u32>(mat.emission_map.w));
                let em_str = clamp(mat.emission_map.y, 0.0, 1.0);
                // Map alone can drive emission when scalar intensity is 0 (gain = 1).
                let base = select(s * max(mat.roughness_metallic.z, 1.0), em * s, length(em) > 1e-5);
                em = mix(em, base, em_str);
            }
            if (length(em) > 1e-5) {
                radiance += throughput * em;
            }

            // Bounce-0 ambient fill (viewport lights often carry ambient; keeps unlit faces readable)
            if (bounce == 0u && num_lights() > 0u) {
                let amb = light_ambient_rgb_v(0u);
                if (dot(amb, amb) > 1e-10) {
                    radiance += throughput * albedo * (1.0 - metal) * amb * 0.25;
                }
            }

            // ── Glass (dielectric transmission) — a delta lobe. Handle it BEFORE any
            // diffuse NEE / GI-cache machinery: a delta BSDF has zero measure toward an
            // explicitly sampled light, and it must NOT feed the diffuse GI reservoir.
            // OWNER DECISION #1: glass ALWAYS takes the deep continuation path — even in
            // lean/interactive mode (pt_max_bounces <= 2) — so it refracts in the viewport.
            if (is_glass) {
                if (glass_bounce >= MAX_GLASS_BOUNCE) { break; } // trapped-TIR safety cap
                let entering = dot(ray_d, n_geo_raw) < 0.0;
                var out_dir: vec3<f32>;
                var new_inside: bool;
                let w = pt_sample_dielectric(
                    ray_d, n_geo_raw, g_ior, glass_tint, entering, inside,
                    &seed, &out_dir, &new_inside);
                throughput *= w; // w = Fresnel-probability tint; NO extra /pdf, NO eta^2
                // Spawn on the side the ray actually leaves toward (transmit exits behind
                // the face, reflect stays in front) — offset by the RAW normal, never the
                // camera-faced `n` (that re-hits the entry side -> acne).
                ray_o = hit_p + sign(dot(out_dir, n_geo_raw)) * n_geo_raw * PT_EPS * 2.0;
                ray_d = out_dir;
                inside = new_inside;
                last_pdf_b = 0.0;     // delta lobe: env MIS treats as pure specular
                glass_bounce += 1u;   // separate budget (decision #3), off the diffuse limit
                // Do NOT consume the diffuse bounce budget for a glass event.
                if (bounce > 0u) { bounce -= 1u; }
                if (max(throughput.r, max(throughput.g, throughput.b)) < 1e-4) { break; }
                continue;
            }

            // Section G: bounce-0 ReSTIR DI (RIS + temporal/spatial reservoirs).
            // Section N: secondary bounces — multi-candidate RIS NEE + MIS.
            // Section H/K: environment NEE on deep path only (Section AB: lean GI owns env).
            // Section R: when max_bounces ≤ 2, temporal GI replaces deep path.
            let view = -ray_d;
            let use_gi_cache = PT_INTERACTIVE;
            let can_continue = bounce < config.pt_max_bounces && !use_gi_cache;
            if (bounce == 0u && PT_INTERACTIVE) {
                radiance += throughput * pt_restir_di(
                    coord, tex_dim, hit_p, n, view, albedo, rough, metal, &seed);
            } else {
                radiance += throughput * pt_nee(hit_p, n, view, albedo, rough, metal, &seed);
            }
            // Section AB: lean path skips primary env NEE — GI sample includes sky/HDRI
            // (avoids double-counting env with pt_sample_gi miss). Deep path keeps env NEE+MIS.
            if (!use_gi_cache) {
                radiance += throughput * pt_env_nee(
                    hit_p, n, view, albedo, rough, metal, can_continue, &seed);
            }

            // Section R/W/Y/AE/AH: lean path — full GI path reservoirs, then stop.
            // Section Y: 2nd GI bounce only on rough dielectrics (metals/mirrors stay 1-bounce).
            // Section AH: temporal/spatial re-eval of stored world-space path directions.
            if (bounce == 0u && use_gi_cache) {
                var gi_depth = 1u;
                if (config.pt_max_bounces >= 2u && metal < 0.75 && rough > 0.12) {
                    gi_depth = 2u;
                }
                let gi = pt_restir_gi(
                    coord, tex_dim, hit_p, n, view, albedo, rough, metal, gi_depth, &seed);
                radiance += throughput * gi;
                break;
            }


            // Russian roulette — start earlier once progressive history is deep.
            let rr_start = select(2u, 1u, config.pt_accumulated > 32u);
            if (bounce >= rr_start) {
                var p_survive = clamp(max(throughput.r, max(throughput.g, throughput.b)), 0.05, 0.95);
                if (config.pt_accumulated > 24u) {
                    p_survive *= 0.8;
                }
                if (config.pt_accumulated > 64u) {
                    p_survive = min(p_survive, 0.55);
                }
                p_survive = clamp(p_survive, 0.05, 0.95);
                if (pt_rand(&seed) > p_survive) { break; }
                throughput /= p_survive;
            }

            // BSDF sample for next path direction (Lambert + GGX mix).
            // On miss, MIS vs env CDF pdf (Section K) using last_pdf_b.
            var pdf_b = 0.0;
            var f_cos = vec3(0.0);
            let l_next = pt_sample_bsdf(n, view, albedo, rough, metal, &seed, &pdf_b, &f_cos);
            if (pdf_b < 1e-6 || max(f_cos.r, max(f_cos.g, f_cos.b)) < 1e-8) { break; }
            last_pdf_b = pdf_b;
            // throughput *= f * cos / pdf
            throughput *= f_cos / pdf_b;
            ray_d = l_next;
            ray_o = hit_p + n * PT_EPS * 2.0;

            if (max(throughput.r, max(throughput.g, throughput.b)) < 1e-4) { break; }
        }

        // Section AK: adaptive firefly clamp (pt_clamp — 0 disables).
        // Early progressive frames: tighter clamp (kill spikes). Deep history: looser
        // (less bias). Relative guard vs running mean for multi-spp outliers.
        if (config.pt_clamp > 0.0) {
            let age = f32(config.pt_accumulated);
            // 0.45× at start → 1.35× when settled
            let age_scale = mix(0.45, 1.35, clamp(age / 48.0, 0.0, 1.0));
            var cmax = config.pt_clamp * age_scale;
            // Relative: don't exceed ~6× running mean luminance (multi-spp)
            if (s > 0u) {
                let mean_so_far = sample_sum / f32(s);
                let mean_l = max(dot(mean_so_far, vec3(0.2126, 0.7152, 0.0722)), 0.05);
                cmax = min(cmax, max(config.pt_clamp * 0.35, mean_l * 6.0));
            }
            radiance = min(radiance, vec3(cmax));
        }
        sample_sum += radiance;
        pt_add_luminance_sample(&sample_moments, radiance);
    }

    // Section AH: GI path reservoir already committed inside pt_restir_gi

    pt_store_progressive(coord, sample_sum / f32(spp), sample_moments);
}
