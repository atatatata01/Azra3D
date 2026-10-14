// ============================================================
// G-Buffer Rasterization Shader
// ============================================================
//
// Renders triangle meshes into the same G-buffer format as the SDF
// compute raymarcher, so all existing post-processing (deferred_shade,
// SSR, fog, bloom, tonemap, upsample) works unchanged.
//
// Raster pass writes 4 of 7 G-buffer slots (32B/sample hardware limit).
// surface_id, curvature, emission are written by SDF compute imageStore.
//   location(0) = normal_depth  : Rgba16Float  → vec4(world_normal, world_distance)
//   location(1) = material      : Rgba16Float  → vec4(roughness, metallic, shadow_ao, opacity_em)
//   location(2) = albedo_flags  : Rgba16Float  → vec4(albedo.rgb, flags)
//   location(3) = body_index    : R32Float     → f32(body_index)  // C.5: exact past 2048; 28 B total
//                 (probe capture frames: PROBE_BACKFACE_MARK on back-facing fragments)
//
// flags: 1.0 = standard rasterized object, 0.0 = sky/background
//
// Procedural material layers (noise, edge wear, cavity dirt) are evaluated
// here to match the SDF compute path's eval_material(). Curvature for
// edge/cavity detection is approximated from screen-space normal derivatives.

// ---- Uniforms ----

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    cam_pos: vec4<f32>,    // xyz = position, w = ortho_dist or 0
    near_far: vec2<f32>,
    _pad: vec2<f32>,
    // Move-grab vertex displacement (must match render::camera::CameraUniforms tail; size = MAX_GRAB_INSTANCES).
    grab_radii: vec4<f32>,            // z = active instance count; x/y/w unused (radii are PER-INSTANCE below)
    grab_anchors: array<vec4<f32>, 32>, // xyz = world anchor, w = inner_r (world) per instance
    grab_drags: array<vec4<f32>, 32>,   // xyz = world drag,   w = outer_r (world) per instance
    // GPU DC-wire edge color (rgb) + opacity (a) — fs_gbuffer_wire only.
    wire_color: vec4<f32>,
    // x = 1 → auto reflection-probe capture frame (probe_bake.rs); see PROBE_BACKFACE_MARK.
    frame_flags: vec4<f32>,
};

// Probe capture frames only: written to location(3) (body_index) for a BACK-facing
// fragment — the eye looks at the inside of a surface, i.e. it sits inside solid
// geometry along that ray. probe.wgsl `probe_capture` reads it to measure the
// probe's free-space fraction (AUTO-PROBE-INSIDE-GEOMETRY). Negative can never be
// a body_index (dense 0-based); body_index has no consumer in a probe frame
// (post-process never runs on one, and the main view re-renders the target).
const PROBE_BACKFACE_MARK: f32 = -1.0;

struct MaterialData {
    albedo: vec4<f32>,              // rgb + opacity
    roughness_metallic: vec4<f32>,  // x=roughness, y=metallic, z=emission_intensity, w=shader_type
    emission_color: vec4<f32>,      // rgb + 0
    noise_params: vec4<f32>,        // x=amount, y=scale, z=octaves, w=metallic
    noise_color: vec4<f32>,         // rgb=color, w=roughness
    edge_wear: vec4<f32>,           // x=amount, y=roughness, z=metallic, w=sharpness
    edge_wear_color: vec4<f32>,     // rgb=color, w=0
    cavity_dirt: vec4<f32>,         // x=amount, y=roughness, z=metallic, w=sharpness
    cavity_dirt_color: vec4<f32>,   // rgb=color, w=0
    // Albedo texture map: x=slot (bitcast u32, 0=none), y=strength, z=uv_scale,
    // w=projection pack (bitcast u32: bit0 UV/Box, bit1 absolute sample). MUST match RasterMaterial.
    albedo_map: vec4<f32>,
    // Normal / roughness / metallic — LINEAR data (CPU pre-encodes to sRGB; hardware decode
    // returns raw values — see material_textures.rs decode_file_to_data_map).
    normal_map: vec4<f32>,
    roughness_map: vec4<f32>,
    metallic_map: vec4<f32>,
    // Emission (color, sRGB), bump height (linear), displacement height (linear).
    emission_map: vec4<f32>,
    bump_map: vec4<f32>,
    displacement_map: vec4<f32>,
    // SSS color (color, sRGB). Sampled rgb REPLACES adv_sss.xyz by strength (y).
    sss_color_map: vec4<f32>,
    // Advanced lobes (realtime approx; full transport = path tracer).
    // x=sss_radius, y=ior, z=clearcoat, w=clearcoat_roughness
    adv_params: vec4<f32>,
    // x..z=sss_color, w=sheen
    adv_sss: vec4<f32>,
    // x..z=glass_tint, w=sheen_tint
    adv_glass: vec4<f32>,
};

struct ShadowData {
    cascade_vp_0: mat4x4<f32>,
    cascade_vp_1: mat4x4<f32>,
    cascade_vp_2: mat4x4<f32>,
    cascade_splits: vec4<f32>,  // xyz = split distances, w unused
    texel_world: vec4<f32>,     // xyz = world size of one shadow texel per cascade, w unused
    // x: 1.0 = a visible directional light (sun) exists → shadows apply; 0.0 = no sun →
    // compute_shadow() returns 1.0 without sampling (a deleted sun must not leave a stale
    // shadow multiply behind — the round-4 zero-light dark-banding artifact).
    // yzw = direction TO the sun (slope-scaled bias, see normal_offset_texels).
    flags: vec4<f32>,
    // Shadow quality (2026-07-17f). x = per-cascade map resolution in texels (texel_size
    // derives from it — never a hardcoded map size); y = PCF radius in texels ("Shadow
    // Softness", 0.5 = the classic 2x2 kernel). zw unused. MUST match ShadowUniforms
    // (render/src/shadows.rs).
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<storage, read> materials: array<MaterialData>;
// Material PBR texture maps (RGBA8 sRGB array). Layer 0 = white sentinel ("no map").
@group(0) @binding(2) var mat_tex_array: texture_2d_array<f32>;
@group(0) @binding(3) var mat_tex_sampler: sampler;
@group(0) @binding(4) var mat_linear_array: texture_2d_array<f32>;

// Group 2 (GPU_CURVATURE_KERNEL_PLAN.md P2): the per-body vertex-curvature carry. ONE f32 per
// mesh vertex SLOT, laid out exactly like `volume_bake.wgsl`'s `curvature_eval` output —
// `[i*4+0]=mask [i*4+1]=signed curv [i*4+2]=convex [i*4+3]=concave` — because the buffer bound
// here IS that kernel's own output buffer, never copied or repacked (`gpu_curvature.rs`'s
// `encode_curvature_vertex_dispatch`). Indexed by `@builtin(vertex_index)`, which for an indexed
// pool draw equals `index_buffer[i] + base_vertex`, and the pool's `base_vertex` is ALREADY the
// absolute vertex-slot index (`pool.rs:558` `base_vertex = vert_off/32`) — so this needs NO
// per-instance offset uniform, unlike a naive "vertex index within this draw" assumption would.
// Read ONLY when `INSTANCE_FLAG_VERTEX_CURVATURE` (bit 2) is set on the instance; every other draw
// binds a tiny dummy here (RasterPipeline::curvature_dummy_bind_group). The flag MUST only be set
// when a real per-body buffer is bound — but a flag/dummy mismatch (or stale cap) would OOB the
// storage array and remove the device. Always guard with arrayLength (defense in depth).
@group(2) @binding(0) var<storage, read> curv_vertex: array<f32>;

// ---- Material Map Sampling ----
// (Section markers are load-bearing: render/src/tests_triplanar.rs extracts this section VERBATIM
// into a headless GPU probe — the tested code IS the shipped code, it cannot drift.)
//
// BOX (triplanar) projection samples at the BODY-LOCAL position with BODY-LOCAL normal weights
// (2026-07-17 owner decision): textures STICK to a body when it moves/rotates — moving a body
// changes only its instance matrix, never the local-space projection. `p`/`normal` here are
// therefore the mesh-local inputs (vertex position/normal before the instance transform), which
// for SDF DC meshes, poly bodies and voxel chunks are all body-local by construction.

// ---- Mip selection ----
// Per-fragment texel-footprint context, set ONCE at the top of fs_gbuffer (uniform control
// flow — derivative ops are illegal inside the non-uniform `if (slot != 0)` branches below,
// which is why every sample here is an explicit-level textureSampleLevel). Zero = "no
// derivatives available" (vertex-shader displacement, the shadow pass, the headless probe)
// and selects mip 0 — exactly the pre-mip behaviour.
var<private> mat_fw_world: f32 = 0.0;  // max |d p_local / d pixel| (body-local units)
var<private> mat_fw_uv: f32 = 0.0;     // max |d uv / d pixel| (UV units)

// LOD from a texture-space footprint (tiles per pixel): × layer resolution = texels per pixel.
// Without this, minification aliases every map into per-pixel noise at distance/grazing angles
// (the drag-drop "broken material" speckle), and the bump path's dpdx of that noise exploded.
fn mat_lod(fw_tiles: f32) -> f32 {
    if (fw_tiles <= 0.0) { return 0.0; }
    let texels = fw_tiles * f32(textureDimensions(mat_tex_array).x);
    return clamp(log2(max(texels, 1.0)), 0.0, f32(textureNumLevels(mat_tex_array) - 1u));
}

// Triplanar sample of the material texture array at a body-local position, blended by |normal|^4.
// `slot` is the array layer (0 = white sentinel ⇒ returns white).
fn sample_mat_triplanar(slot: u32, p: vec3<f32>, normal: vec3<f32>, tiling: f32) -> vec3<f32> {
    return sample_mat_triplanar_b(slot, p, normal, tiling, TRIPLANAR_BLEND_DEFAULT, 0u);
}

/// Triplanar blend exponent. Higher = sharper transition between the three planes (less cross-
/// fade, so less blurring on flat faces but harder seams on curved ones); lower = softer.
/// 4.0 is the value this shader hardcoded before the control existed, so it stays the default and
/// existing materials are unchanged.
const TRIPLANAR_BLEND_DEFAULT: f32 = 4.0;

/// Rotate UV by `q` quarters CW (0/90/180/270). Per-plane control for Box projection.
fn triplanar_rot_uv(uv: vec2<f32>, q: u32) -> vec2<f32> {
    let qq = q & 3u;
    if (qq == 1u) { return vec2<f32>(-uv.y, uv.x); }
    if (qq == 2u) { return vec2<f32>(-uv.x, -uv.y); }
    if (qq == 3u) { return vec2<f32>(uv.y, -uv.x); }
    return uv;
}

/// As `sample_mat_triplanar`, with explicit blend sharpness + per-plane 90° rotations
/// (`rot_pack` = rot_x | rot_y<<2 | rot_z<<4, each 0..=3).
fn sample_mat_triplanar_b(slot: u32, p: vec3<f32>, normal: vec3<f32>, tiling: f32, blend: f32, rot_pack: u32) -> vec3<f32> {
    var w = abs(normal);
    // Clamped: blend <= 0 makes every weight 1 (a flat 1/3 average = fully blurred), and very
    // large exponents underflow all three weights to 0 on near-45-degree normals, which the
    // epsilon below would then turn into black.
    w = pow(w, vec3<f32>(clamp(blend, 0.25, 32.0)));
    w = w / (w.x + w.y + w.z + 0.0001);
    let lod = mat_lod(mat_fw_world * tiling);
    let rx = rot_pack & 3u;
    let ry = (rot_pack >> 2u) & 3u;
    let rz = (rot_pack >> 4u) & 3u;
    let cx = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.yz * tiling, rx), i32(slot), lod).rgb;
    let cy = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.xz * tiling, ry), i32(slot), lod).rgb;
    let cz = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.xy * tiling, rz), i32(slot), lod).rgb;
    return cx * w.x + cy * w.y + cz * w.z;
}

// UV sample of the material texture array (poly meshes with real UVs).
fn sample_mat_uv(slot: u32, uv: vec2<f32>, tiling: f32) -> vec3<f32> {
    let lod = mat_lod(mat_fw_uv * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, uv * tiling, i32(slot), lod).rgb;
}

// SPHERICAL projection: body-local direction -> longitude/latitude. Single-sample, so unlike
// triplanar there is nothing to blend — it pinches at the poles by construction (every longitude
// meets there), which is correct for ball-like bodies and wrong for boxes. Matches
// `sphere_uv` in the CPU/bake path.
fn sample_mat_sphere(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let d = normalize(select(p, vec3<f32>(0.0, 1.0, 0.0), length(p) < 1e-6));
    // atan2 spans -PI..PI -> 0..1; asin spans -PI/2..PI/2 -> 0..1.
    let u = atan2(d.z, d.x) * 0.15915494 + 0.5;   // 1/(2*PI)
    let v = asin(clamp(d.y, -1.0, 1.0)) * 0.31830989 + 0.5;   // 1/PI
    // Angular projection: uv moves ~1/(PI*radius) per world unit (isotropic approximation).
    let lod = mat_lod(mat_fw_world * tiling / max(length(p) * 3.14159265, 1e-4));
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, vec2<f32>(u, v) * tiling, i32(slot), lod).rgb;
}

// CYLINDRICAL projection: angle around +Y, height along Y. Single-sample; seams at the Y caps
// where the surface turns to face the axis and the projection degenerates.
fn sample_mat_cylinder(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let u = atan2(p.z, p.x) * 0.15915494 + 0.5;
    // v (height) moves 1:1 with world units — the dominant axis for radius > 1/(2*PI).
    let lod = mat_lod(mat_fw_world * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, vec2<f32>(u, p.y) * tiling, i32(slot), lod).rgb;
}

fn material_projection_uv(proj: u32, p: vec3<f32>, uv: vec2<f32>) -> vec2<f32> {
    if (proj == 1u) { return uv; }
    if (proj == 2u) {
        let direction = normalize(select(p, vec3<f32>(0.0, 1.0, 0.0), length(p) < 1e-6));
        return vec2<f32>(atan2(direction.z, direction.x) * 0.15915494 + 0.5,
                         asin(clamp(direction.y, -1.0, 1.0)) * 0.31830989 + 0.5);
    }
    if (proj == 3u) { return vec2<f32>(atan2(p.z, p.x) * 0.15915494 + 0.5, p.y); }
    return p.xy;
}

// PLANAR projection: flat down body-local -Z (the XY plane). Single-sample; stretches on faces
// parallel to the projection axis — the classic decal mapping, useful precisely because it does
// NOT wrap.
fn sample_mat_planar(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let lod = mat_lod(mat_fw_world * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, p.xy * tiling, i32(slot), lod).rgb;
}

/// ONE dispatcher for every projection — every call site routes through here, so adding a
/// projection cannot leave one channel behind (albedo/emission/roughness/metallic all call this).
/// `proj` is the unpacked `MapProjection` code; `blend` + `rot_pack` are honoured only by Box.
fn sample_mat_proj(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32, blend: f32, rot_pack: u32) -> vec3<f32> {
    if (proj == 1u) { return sample_mat_uv(slot, uv, tiling); }
    if (proj == 2u) { return sample_mat_sphere(slot, p, tiling); }
    if (proj == 3u) { return sample_mat_cylinder(slot, p, tiling); }
    if (proj == 4u) { return sample_mat_planar(slot, p, tiling); }
    return sample_mat_triplanar_b(slot, p, normal, tiling, blend, rot_pack);
}

// Tangentless triplanar NORMAL mapping — "whiteout" blend (Ben Golus, "Normal Mapping for a
// Triplanar Shader"; the industry-standard choice: cheap, no tangent basis needed — DC/voxel
// meshes have none — and free of the mirrored-seam artifacts of the naive swizzle). Per axis the
// tangent-space sample (xy = slope, z = up) is swizzled into the planes' frames, xy summed with
// the geometric normal's plane components, z sign-carried; the three axis normals blend with the
// same |n|^4 weights as the color sample. `strength` scales the tangent-space xy slope.
// Returns the perturbed normal in the SAME (body-local) space as `n_geo`.
//
// `packed` = the TexMap projection word. Bit 11 (TEXMAP_GREEN_DX_BIT) picks the green-channel
// convention: this file's uv conventions sample v DOWN the image, so a DirectX map (green = +v)
// reads directly, while an OpenGL map (green = up-image — ambientCG / Blender / glTF, the
// default) needs its green negated. Unflipped GL maps were the "normal looks inverted" report.
fn sample_mat_triplanar_normal(slot: u32, p: vec3<f32>, n_geo: vec3<f32>, tiling: f32, strength: f32, packed: u32) -> vec3<f32> {
    var w = abs(n_geo);
    // Same blend + rot as color triplanar so seams match.
    let blend = unpack_blend(packed);
    let rot_pack = (packed >> 12u) & 0x3Fu;
    w = pow(w, vec3<f32>(clamp(blend, 0.25, 32.0)));
    w = w / (w.x + w.y + w.z + 0.0001);
    // Decode tangent-space normals (maps are pre-encoded so the sRGB decode returns raw bytes).
    let lod = mat_lod(mat_fw_world * tiling);
    let rx = rot_pack & 3u;
    let ry = (rot_pack >> 2u) & 3u;
    let rz = (rot_pack >> 4u) & 3u;
    var tx = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.yz * tiling, rx), i32(slot), lod).rgb * 2.0 - 1.0;
    var ty = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.xz * tiling, ry), i32(slot), lod).rgb * 2.0 - 1.0;
    var tz = textureSampleLevel(mat_tex_array, mat_tex_sampler, triplanar_rot_uv(p.xy * tiling, rz), i32(slot), lod).rgb * 2.0 - 1.0;
    let gs = select(-1.0, 1.0, (packed & 2048u) != 0u); // bit 11: DX keeps green, GL flips
    tx = vec3<f32>(tx.x * strength, tx.y * gs * strength, tx.z);
    ty = vec3<f32>(ty.x * strength, ty.y * gs * strength, ty.z);
    tz = vec3<f32>(tz.x * strength, tz.y * gs * strength, tz.z);
    // Whiteout blend: add the geometric normal's in-plane components, keep the sampled z magnitude
    // signed by the geometric axis direction, then swizzle each plane normal back into 3D.
    // NOTE the swizzles are derived for THIS file's uv conventions (they differ from the textbook
    // form, which samples the X plane as p.zy — we sample p.yz to stay aligned with the color
    // triplanar above, so color and normal features land on the same texels):
    //   X plane: uv = p.yz → u=Y, v=Z, up=X → contribution (up, u, v) = .zxy
    //   Y plane: uv = p.xz → u=X, v=Z, up=Y → contribution (u, up, v) = .xzy
    //   Z plane: uv = p.xy → u=X, v=Y, up=Z → contribution (u, v, up) = .xyz
    let nx = vec3<f32>(tx.xy + n_geo.yz, abs(tx.z) * n_geo.x);
    let ny = vec3<f32>(ty.xy + n_geo.xz, abs(ty.z) * n_geo.y);
    let nz = vec3<f32>(tz.xy + n_geo.xy, abs(tz.z) * n_geo.z);
    return normalize(nx.zxy * w.x + ny.xzy * w.y + nz.xyz * w.z);
}

/// Decode the projection code from a packed word. Mirrors `TexMap::unpack_projection`
/// (core_math/src/pbr_material.rs) — bits 2-4 hold the full code; a word written before those
/// bits existed has them zero and falls back to the legacy bit0.
fn unpack_proj_code(packed: u32) -> u32 {
    let wide = (packed >> 2u) & 0x7u;
    if (wide != 0u) { return wide; }
    return packed & 1u;
}

/// Decode the triplanar blend sharpness from bits 5-9. Mirrors `TexMap::unpack_blend`
/// (core_math/src/pbr_material.rs): 0.25 steps, and code 0 means "written before this field
/// existed" and decodes to the legacy hardcoded default.
fn unpack_blend(packed: u32) -> f32 {
    let q = (packed >> 5u) & 0x1Fu;
    if (q == 0u) { return TRIPLANAR_BLEND_DEFAULT; }
    return f32(q) * 0.25;
}

/// Decode per-plane triplanar rotation pack (bits 12-17). Mirrors `TexMap::unpack_triplanar_rot`.
fn unpack_triplanar_rot(packed: u32) -> u32 {
    return (packed >> 12u) & 0x3Fu;
}

// 16-bit dual-plane reconstruct: plane `slot` = high bytes, plane `slot+1` = low bytes
// (each component pre-encoded for sRGB array so hardware decode recovers 0..255).
// Matches `material_textures::encode_*_rgba16_planes` + TexMap.high_precision (bit 10).
fn reconstruct_u16_channel(hi: f32, lo: f32) -> f32 {
    return (hi * 65280.0 + lo * 255.0) / 65535.0;
}

fn reconstruct_rgb16(hi: vec3<f32>, lo: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        reconstruct_u16_channel(hi.r, lo.r),
        reconstruct_u16_channel(hi.g, lo.g),
        reconstruct_u16_channel(hi.b, lo.b),
    );
}

fn sample_mat_precise_proj(maps: texture_2d_array<f32>, slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32, blend: f32, rot_pack: u32) -> vec3<f32> {
    if (proj != 0u) {
        return material_plane_linear(maps, slot, material_projection_uv(proj, p, uv) * tiling);
    }
    let weights = material_projection_weights(normal, blend);
    let projected_x = material_plane_linear(maps, slot, triplanar_rot_uv(p.yz * tiling, rot_pack & 3u));
    let projected_y = material_plane_linear(maps, slot, triplanar_rot_uv(p.xz * tiling, (rot_pack >> 2u) & 3u));
    let projected_z = material_plane_linear(maps, slot, triplanar_rot_uv(p.xy * tiling, (rot_pack >> 4u) & 3u));
    return projected_x * weights.x + projected_y * weights.y + projected_z * weights.z;
}

// sRGB 0..1 → linear (for 16-bit color planes stored as sRGB u16).
fn srgb_to_linear_1(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn srgb_to_linear_v3(s: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_1(s.r), srgb_to_linear_1(s.g), srgb_to_linear_1(s.b));
}

// Sample RGB; if high_precision (bit 10), dual-plane 16-bit reconstruct.
// `as_srgb_color`: true for albedo/emission (reconstruct sRGB then → linear);
// false for data (rough/metal/height/normal already linear).
fn sample_mat_proj_hp(
    slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>,
    tiling: f32, packed: u32, as_srgb_color: bool,
) -> vec3<f32> {
    let proj = unpack_proj_code(packed);
    let blend = unpack_blend(packed);
    let rot_pack = unpack_triplanar_rot(packed);
    if ((packed & (1u << 10u)) == 0u) {
        return sample_mat_proj(slot, p, normal, uv, tiling, proj, blend, rot_pack);
    }
    var hi: vec3<f32>;
    var lo: vec3<f32>;
    if ((packed & (1u << 18u)) != 0u) {
        hi = sample_mat_precise_proj(mat_linear_array, slot, p, normal, uv, tiling, proj, blend, rot_pack);
        lo = sample_mat_precise_proj(mat_linear_array, slot + 1u, p, normal, uv, tiling, proj, blend, rot_pack);
    } else {
        hi = sample_mat_precise_proj(mat_tex_array, slot, p, normal, uv, tiling, proj, blend, rot_pack);
        lo = sample_mat_precise_proj(mat_tex_array, slot + 1u, p, normal, uv, tiling, proj, blend, rot_pack);
    }
    let v = reconstruct_rgb16(hi, lo);
    if (as_srgb_color) {
        return srgb_to_linear_v3(v);
    }
    return v;
}

// Grayscale data-map sample (roughness / metallic / height).
fn sample_mat_scalar(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32) -> f32 {
    return sample_mat_proj(slot, p, normal, uv, tiling, proj, TRIPLANAR_BLEND_DEFAULT, 0u).r;
}

fn sample_mat_scalar_packed(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, packed: u32) -> f32 {
    return sample_mat_proj_hp(slot, p, normal, uv, tiling, packed, false).r;
}
// ---- End Material Map Sampling ----

// Shadow map bindings (group 1)
@group(1) @binding(0) var<uniform> shadow_data: ShadowData;
@group(1) @binding(1) var shadow_map_0: texture_depth_2d;
@group(1) @binding(2) var shadow_map_1: texture_depth_2d;
@group(1) @binding(3) var shadow_map_2: texture_depth_2d;
@group(1) @binding(4) var shadow_sampler: sampler_comparison;

// ---- Noise (matches compute.wgsl: hash31 → noise3 → fbm3) ----

fn hash31(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash31(i + vec3(0.0,0.0,0.0)), hash31(i + vec3(1.0,0.0,0.0)), u.x),
            mix(hash31(i + vec3(0.0,1.0,0.0)), hash31(i + vec3(1.0,1.0,0.0)), u.x), u.y),
        mix(mix(hash31(i + vec3(0.0,0.0,1.0)), hash31(i + vec3(1.0,0.0,1.0)), u.x),
            mix(hash31(i + vec3(0.0,1.0,1.0)), hash31(i + vec3(1.0,1.0,1.0)), u.x), u.y),
        u.z);
}

fn fbm3(p: vec3<f32>, octaves: u32) -> f32 {
    var sum = 0.0; var amp = 0.5; var pos = p;
    let max_oct = min(octaves, 64u);
    for (var i = 0u; i < max_oct; i++) {
        sum += amp * noise3(pos);
        pos = pos * 2.01;
        amp *= 0.5;
    }
    return sum;
}

// ---- Vertex ----

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // Instance data
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    @location(6) model_3: vec4<f32>,
    @location(7) normal_mat_0: vec4<f32>,
    @location(8) normal_mat_1: vec4<f32>,
    @location(9) normal_mat_2: vec4<f32>,
    @location(10) material_idx: u32,
    @location(11) body_index: u32,
    @location(12) flags: u32,
    // Vertex-curvature shape: x=remap_min y=remap_max z=opacity(intensity) w=soft_hard(power)
    @location(13) curv_params: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) @interpolate(flat) material_idx: u32,
    @location(4) @interpolate(flat) body_index: u32,
    @location(5) @interpolate(flat) flags: u32,
    // Per-vertex paint colour, carried FLAT (provoking vertex) so the bit-packed word in uv.x is never
    // float-interpolated across a triangle (which would blend two packed words into a garbage colour at
    // paint boundaries). vcol_packed = the 0x_FF_RR_GG_BB word as f32-bits; vcol_flag = uv.y (painted).
    @location(6) @interpolate(flat) vcol_packed: f32,
    @location(7) @interpolate(flat) vcol_flag: f32,
    // BODY-LOCAL position + normal for box (triplanar) material projection — the pre-instance-
    // transform vertex data, so the projection sticks to a moving/rotating body (owner decision
    // 2026-07-17). local_normal drives the triplanar blend weights + whiteout normal mapping.
    @location(8) local_position: vec3<f32>,
    @location(9) local_normal: vec3<f32>,
    // Normal matrix columns (per-instance ⇒ flat): map the perturbed LOCAL normal back to world
    // for the G-buffer. Passing the matrix is exact under non-uniform scale (it is the instance's
    // inverse-transpose); re-deriving it per-pixel from derivatives would be lossy.
    @location(10) @interpolate(flat) nmat0: vec3<f32>,
    @location(11) @interpolate(flat) nmat1: vec3<f32>,
    @location(12) @interpolate(flat) nmat2: vec3<f32>,
    // GPU_CURVATURE_KERNEL_PLAN.md P2/P3 — per-vertex curvature sample (channel selected by
    // INSTANCE_FLAG_VERTEX_CURV_CONVEX/_CONCAVE, default = signed curv). SMOOTH interpolation,
    // NOT flat: `vcol_packed`'s flat precedent applies to a BITCAST-PACKED integer word, where
    // interpolating two packed words produces a garbage colour — that reasoning does not transfer
    // to a continuous scalar. Curvature is a real-valued field sampled at each DC vertex; smoothly
    // interpolating it across a triangle is exactly what makes the shaded result follow the 3D
    // surface instead of faceting per-triangle (owner requirement: "follows the 3D shape"). 0.0
    // when `INSTANCE_FLAG_VERTEX_CURVATURE` is unset (the dummy binding is never read in that case).
    @location(14) vertex_curv: f32,
    // Remap / opacity(intensity) / soft_hard for Vertex rung (flat — per instance).
    @location(15) @interpolate(flat) curv_params: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let normal_mat = mat3x3<f32>(
        in.normal_mat_0.xyz,
        in.normal_mat_1.xyz,
        in.normal_mat_2.xyz,
    );

    var world_pos = (model * vec4(in.position, 1.0)).xyz;
    var world_normal = normalize(normal_mat * in.normal);

    // Material displacement map: offset along the geometric normal by sample * strength
    // (strength ≈ world-space amplitude). Sampled in BODY-LOCAL space so tiles stick to the body.
    // Applied BEFORE grab preview so Move tool still tracks the displaced surface.
    {
        let mat = materials[in.material_idx & 0x3FFFFFFFu];
        let dmap_slot = bitcast<u32>(mat.displacement_map.x);
        if (dmap_slot != 0u) {
            let n_local = normalize(in.normal);
            let h = sample_mat_scalar_packed(
                dmap_slot, in.position, n_local, in.uv,
                mat.displacement_map.z, bitcast<u32>(mat.displacement_map.w));
            // Center height around mid-gray so mid-gray = no displacement (standard height maps).
            let amp = (h - 0.5) * 2.0 * clamp(mat.displacement_map.y, 0.0, 4.0);
            world_pos = world_pos + world_normal * amp;
        }
    }

    // MOVE-GRAB: soft-round tip falloff w=(1-t²)² (classic DCC brush tip) + symmetry-safe blend.
    // SYMMETRY FIX: primary + mirror used to SUM at the mid-plane → double displacement (weird ridge
    // when the stroke crosses the mirror line). Active stroke instances [0, n_active) use
    // weighted-average drag * max(w) so mid-plane influence stays single-strength. Deferred pending
    // instances (after n_active) still accumulate by raw sum (separate past strokes).
    // grab_radii.z = total instances, grab_radii.w = active stroke instance count (symmetry octants).
    let grab_n = u32(camera.grab_radii.z);
    let n_active = u32(camera.grab_radii.w);
    if (grab_n > 0u && (in.flags & 2u) != 0u) {
        // --- Active stroke (symmetry group): normalize so mid-plane is not 2× ---
        var sum_w = 0.0;
        var sum_wd = vec3(0.0);
        var max_w = 0.0;
        let n_act = min(n_active, grab_n);
        for (var gi = 0u; gi < n_act; gi = gi + 1u) {
            let outer = max(camera.grab_drags[gi].w, 1e-6);
            let gdist = distance(world_pos, camera.grab_anchors[gi].xyz);
            let t = clamp(gdist / outer, 0.0, 1.0);
            let e = 1.0 - t * t;
            let gfall = e * e; // soft round tip
            sum_w = sum_w + gfall;
            sum_wd = sum_wd + camera.grab_drags[gi].xyz * gfall;
            max_w = max(max_w, gfall);
        }
        if (sum_w > 1e-5) {
            world_pos = world_pos + (sum_wd / sum_w) * max_w;
        }
        // --- Deferred past strokes: independent accumulation ---
        for (var gi = n_act; gi < grab_n; gi = gi + 1u) {
            let outer = max(camera.grab_drags[gi].w, 1e-6);
            let gdist = distance(world_pos, camera.grab_anchors[gi].xyz);
            let t = clamp(gdist / outer, 0.0, 1.0);
            let e = 1.0 - t * t;
            let gfall = e * e;
            world_pos = world_pos + camera.grab_drags[gi].xyz * gfall;
        }
    }

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4(world_pos, 1.0);
    out.world_position = world_pos;
    out.world_normal = world_normal;
    out.uv = in.uv;
    out.material_idx = in.material_idx;
    out.body_index = in.body_index;
    out.flags = in.flags;
    // Forward the (raw, un-interpolated) vertex UV as the FLAT paint-colour channels. For a textured
    // mesh USE_VERTEX_COLOR is off so these are ignored; for a voxel paint mesh uv.x is the packed word.
    out.vcol_packed = in.uv.x;
    out.vcol_flag = in.uv.y;
    // GPU_CURVATURE_KERNEL_PLAN.md P2/P3: read the vertex-curvature carry ONLY when the flag is
    // set — every other draw (including the dummy-bound ones) leaves this at 0.0. The CHANNEL
    // (1=signed curv default, 2=convex, 3=concave — the kernel's native 4-wide output, see
    // `curv_vertex`'s doc comment) is selected by the two extra flag bits `raster_gbuffer.rs` sets
    // from the Generator layer's own `GeneratorOutput` choice (`CurvVertexSlot::out_channel`) — so
    // the fragment stage samples what the panel actually asked for, not a hardcoded channel.
    //
    // BOUNDS GUARD: unguarded OOB on `curv_vertex` is a device-removal class (silent GPU lost,
    // no Rust panic). Flag can race with a dummy bind or a smaller buffer after remesh —
    // arrayLength turns that into a wrong-pixel, not a crash.
    out.vertex_curv = 0.0;
    if ((in.flags & 4u) != 0u) {
        var chan = 1u;
        if ((in.flags & 8u) != 0u) { chan = 2u; }       // INSTANCE_FLAG_VERTEX_CURV_CONVEX
        else if ((in.flags & 16u) != 0u) { chan = 3u; } // INSTANCE_FLAG_VERTEX_CURV_CONCAVE
        let idx = in.vertex_index * 4u + chan;
        if (idx < arrayLength(&curv_vertex)) {
            out.vertex_curv = curv_vertex[idx];
        }
    }
    out.curv_params = in.curv_params;
    // Body-local projection inputs (see VertexOutput doc). The grab displacement above is a
    // world-space preview effect and deliberately does NOT move the local projection.
    out.local_position = in.position;
    out.local_normal = in.normal;
    out.nmat0 = normal_mat[0];
    out.nmat1 = normal_mat[1];
    out.nmat2 = normal_mat[2];
    return out;
}

// ---- Shadow Sampling ----

/// PCF shadow sampling for a given cascade. The footprint radius (in texels) is
/// shadow_data.params.y — the "Shadow Softness" control. texel_size derives from the
/// map resolution in shadow_data.params.x (1024/2048/4096/8192 — never hardcoded).
fn sample_shadow_cascade(
    world_pos: vec3<f32>,
    cascade_vp: mat4x4<f32>,
    shadow_map: texture_depth_2d,
    bias_texels: f32,
    texel_world_c: f32,
) -> f32 {
    let light_clip = cascade_vp * vec4(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;

    // Convert from NDC [-1,1] XY to UV [0,1] (wgpu: Y is flipped)
    let uv = vec2(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);

    // Out of shadow map bounds → fully lit
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 1.0;
    }

    // The depth bias arrives in TEXELS (like the normal offset) and is converted to NDC
    // z HERE, because the two are wildly different scales: the cascade's ortho depth
    // window is ~50 world units of caster headroom plus the slice (shadows.rs builds
    // `orthographic_rh(.., -r, 50+r)`), so the receivers all live in a band only
    // ~2r/(50+2r) ≈ 0.03 NDC wide. A slider in raw NDC would have its entire useful
    // travel inside the first ~1% — the reason this conversion exists.
    //
    // depth_range is recovered FROM THE MATRIX rather than by repeating the 50: glam's
    // orthographic_rh puts 1/(near-far) in the z row, and the view rows are unit-length
    // rotations, so |row2(cascade_vp).xyz| == 1/(far-near) exactly. Deriving it means a
    // future change to the headroom in shadows.rs cannot silently desync this shader.
    let z_row = vec3(cascade_vp[0].z, cascade_vp[1].z, cascade_vp[2].z);
    let depth_range = 1.0 / max(length(z_row), 1e-8);
    let ndc_bias = bias_texels * texel_world_c / depth_range;

    // Reference depth (wgpu NDC z is [0,1]), pulled TOWARD the light by the depth bias.
    // The comparison is LessEqual, so a smaller reference reads as lit — subtracting
    // clears self-shadowing. Unlike the normal offset this does NOT move the sample
    // position, so it never shifts the shadow boundary along the surface.
    let depth = light_ndc.z - ndc_bias;

    let texel_size = 1.0 / shadow_data.params.x;
    let r = shadow_data.params.y; // PCF radius in texels
    // Progressive-refine PCF rotation (params.z, radians — golden-angle sequence).
    // Rotating the tap pattern per frame makes the idle 1/N accumulation converge
    // to a filled penumbra disc with the same taps/frame. 0 when not refining —
    // cos(0)=1 / sin(0)=0 makes the rotation an exact identity (bit-identical).
    let rot = shadow_data.params.z;
    let rot_cs = cos(rot);
    let rot_sn = sin(rot);
    let rot2 = mat2x2<f32>(rot_cs, rot_sn, -rot_sn, rot_cs);
    var shadow = 0.0;
    // Softness 0 = hard contact (single comparison). Sharpest possible for the map res.
    if (r < 1e-4) {
        return textureSampleCompare(shadow_map, shadow_sampler, uv, depth);
    }
    if (r <= 1.0) {
        // Tight footprint: the classic 2x2 quad at ±r texels (r = 0.5 reproduces the
        // original fixed kernel exactly), rotated per frame while refining.
        shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + (rot2 * vec2(-r, -r)) * texel_size, depth);
        shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + (rot2 * vec2( r, -r)) * texel_size, depth);
        shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + (rot2 * vec2(-r,  r)) * texel_size, depth);
        shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + (rot2 * vec2( r,  r)) * texel_size, depth);
        return shadow * 0.25;
    }
    // Wide footprint (softness > 1 texel): 3x3 grid at ±r texels. The comparison
    // sampler's hardware bilinear filters between taps, so 9 taps cover a smooth
    // penumbra up to the slider max (3 texels). Uniform-value branch — no divergence.
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let o = (rot2 * (vec2(f32(dx), f32(dy)) * r)) * texel_size;
            shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + o, depth);
        }
    }
    return shadow / 9.0;
}

// Receiver normal-offset bias, in TEXELS of the selected cascade (the world size comes
// from shadow_data.texel_world, computed per frame on the CPU — adaptive, not a magic
// world-space constant). Derivation: the PCF footprint reaches params.y texels past the
// center and caster depth is quantized per shadow texel, so a receiver can sit up to
// ~1 texel "inside" its own caster sample on a sloped surface → offsetting the receiver
// (params.y + 1) texels along the surface normal clears its own caster texel at any
// cascade footprint AND any softness — the offset tracks the footprint by construction,
// so widening the penumbra can never reintroduce acne. Grazing-angle slope is
// additionally handled by the hardware slope-scale depth bias on the shadow render
// pipeline (shadows.rs DepthBiasState).
/// Normal-offset bias in TEXELS of the selected cascade.
///
/// `params.w` is the live "Normal Offset" slider (ShadowBias::normal_offset_texels,
/// default 1.0 = the historical hardcoded `pcf_radius + 1`).
///
/// This offset moves the sample POSITION, which is why raising it shifts the shadow
/// boundary along the surface (peter-panning). REVERTED 2026-08-06: a grazing-angle
/// slope scale — `base * (1 + min(tan(theta), 4))` — was tried HERE and made the image
/// visibly WORSE (owner: "WORST"), because up to 5x the base offset on near-edge-on
/// faces smeared the lookup into a soft gradient. Slope compensation lives in
/// `shadow_depth_bias()` instead, where it shifts depth and not position.
/// Repro: docs/planning/render/repro/shadow-acne.fl4
fn normal_offset_texels() -> f32 {
    return shadow_data.params.y + shadow_data.params.w;
}

/// Receiver DEPTH bias in TEXELS of the selected cascade: constant + slope-scaled.
///
/// `texel_world.w` = constant term, `cascade_splits.w` = slope term (both live sliders,
/// both default 0). Depth bias fights acne without moving the shadow boundary, so it is
/// the preferred knob; the slope term targets grazing angles, where one shadow texel
/// spans a depth range growing as tan(theta) and the comparison flips across texels.
///
/// UNITS: texels, matching `normal_offset_texels`, so all three bias sliders are
/// scene- and resolution-independent. `sample_shadow_cascade` converts to NDC z.
fn shadow_depth_bias(n: vec3<f32>) -> f32 {
    let konst = shadow_data.texel_world.w;
    let slope = shadow_data.cascade_splits.w;
    // Exact early-out: with the slope slider at 0 this is bit-identical to `konst`,
    // so the default (0,0) path cannot perturb the historical image at all.
    if (slope <= 0.0) {
        return konst;
    }
    let ndl = clamp(dot(normalize(n), normalize(shadow_data.flags.yzw)), 0.0, 1.0);
    // tan(theta) diverges as the surface goes edge-on; bound it so a slider nudge can
    // never detach the shadow outright.
    let tan_t = sqrt(max(1.0 - ndl * ndl, 0.0)) / max(ndl, 0.05);
    return konst + slope * min(tan_t, 16.0);
}

/// Select cascade and sample shadow based on view-space depth.
/// `n` = surface normal; the receiver position is offset along it by a per-cascade
/// texel-scaled amount before projecting into the shadow map (anti-acne).
fn compute_shadow(world_pos: vec3<f32>, n: vec3<f32>) -> f32 {
    // No sun in the scene → structurally lit. The CPU derives this flag from the scene
    // graph every frame (shadows.rs update_cascades), so deleting the last directional
    // light disables the shadow multiply the same frame — the maps and matrices left in
    // the bindings are never sampled.
    if (shadow_data.flags.x < 0.5) {
        return 1.0;
    }
    // View-space depth for cascade selection
    let view_pos = camera.view * vec4(world_pos, 1.0);
    let view_depth = -view_pos.z; // negate for RH camera

    // Select cascade, then apply that cascade's normal-offset bias (position) and the
    // shared depth bias (comparison). The two are independent knobs on purpose.
    let offs = normal_offset_texels();
    let db = shadow_depth_bias(n);
    if (view_depth < shadow_data.cascade_splits.x) {
        let tw = shadow_data.texel_world.x;
        let p = world_pos + n * (tw * offs);
        return sample_shadow_cascade(p, shadow_data.cascade_vp_0, shadow_map_0, db, tw);
    } else if (view_depth < shadow_data.cascade_splits.y) {
        let tw = shadow_data.texel_world.y;
        let p = world_pos + n * (tw * offs);
        return sample_shadow_cascade(p, shadow_data.cascade_vp_1, shadow_map_1, db, tw);
    } else if (view_depth < shadow_data.cascade_splits.z) {
        let tw = shadow_data.texel_world.z;
        let p = world_pos + n * (tw * offs);
        return sample_shadow_cascade(p, shadow_data.cascade_vp_2, shadow_map_2, db, tw);
    }
    return 1.0; // Beyond all cascades → fully lit
}

// ---- Fragment ----

// Raster pass writes 4 color targets only. The 32-byte/sample hardware limit
// (wgpu/Vulkan) prevents attaching all 7 G-buffer textures to a render pass.
// surface_id, curvature, and emission are written by the SDF compute pass
// via storage texture imageStore (no render pass limit).
//
// IMPORTANT: raster pixels do NOT get a sentinel surface_id "from the clear
// color" — gbuf_surface_id (R32Uint) is a write-only STORAGE texture with no
// RENDER_ATTACHMENT/COPY_DST usage, so it is never cleared and the raster pass
// cannot write it. Over a raster-covered pixel the SDF compute raymarch
// early-returns without touching gbuf_surface_id (raymarch_entry.wgsl:181,
// raymarch_lite.wgsl:176), leaving a STALE prim index from a prior SDF frame.
// Consumers that need a per-pixel prim id (deferred light-linking) MUST treat
// raster pixels as the 0xFFFFFFFF "no SDF prim / all lights" sentinel — see the
// is_raster_pixel routing in deferred_entry.wgsl (PBR + Lit branches). curvature
// and emission are likewise stale for raster pixels but the deferred pass derives
// those from baked material/albedo, so the stale read is harmless there.
// Camera forward in world space. CameraUniforms carries no explicit forward
// vector, so derive it from the view matrix: look_at_rh stores -forward in the
// third ROW of the rotation block, i.e. the .z of each of the first three COLUMNS.
fn cam_forward() -> vec3<f32> {
    return -vec3<f32>(camera.view[0].z, camera.view[1].z, camera.view[2].z);
}

// THE G-buffer depth convention: the ray parameter `t` along camera_ray().
// This is the ONE quantity every consumer reconstructs a world position with
// (deferred, SSR, SSAO/GTAO, fog, temporal reprojection):  p = origin + dir * t.
//
//   perspective — camera_ray() normalizes dir from cam_pos, so t == length(rel).
//   orthographic — camera_ray() shifts the origin perpendicular to forward and
//                  keeps dir == forward, so t == dot(rel, forward).
//
// Until 2026-08-06 this shader wrote length(rel) unconditionally, with no ortho
// branch, while every consumer assumed the ray parameter. In perspective the two
// are identical, which is why it went unnoticed; in orthographic they diverge by
// length(rel) - dot(rel, forward), several world units at the screen edge. That
// silently broke every screen-space consumer in ortho views — most visibly SSR,
// whose hit test (delta = ssr_metric(p) - sample_depth > 0) could never fire, so
// orthographic reflections were ~zero. Keep this in lockstep with ssr_metric()
// in post_process.wgsl and the sample metric in render/src/ssao.wgsl.
fn gbuf_depth(world_pos: vec3<f32>) -> f32 {
    let rel = world_pos - camera.cam_pos.xyz;
    // cam_pos.w = ortho half-height (0 = perspective)
    return select(length(rel), dot(rel, cam_forward()), camera.cam_pos.w > 0.0);
}

struct GBufferOutput {
    @location(0) normal_depth: vec4<f32>,   // Rgba16Float: world_normal.xyz, ray-parameter depth (gbuf_depth)
    @location(1) material: vec4<f32>,       // Rgba16Float: roughness, metallic, shadow_ao, opacity+em_lum
    @location(2) albedo_flags: vec4<f32>,   // Rgba16Float: linear albedo.rgb, flags (1.0 = object)
    @location(3) motion: f32,               // R32Float: body_index (C.5 exact ints past 2048)
};

@fragment
fn fs_gbuffer(in: VertexOutput) -> GBufferOutput {
    // Texel-footprint context for material-map mip selection. MUST run here, in uniform control
    // flow — derivative ops are illegal inside the non-uniform map branches below (which is why
    // the samplers use explicit-level sampling in the first place).
    let mat_dpx = dpdx(in.local_position);
    let mat_dpy = dpdy(in.local_position);
    mat_fw_world = max(length(mat_dpx), length(mat_dpy));
    let mat_duvx = dpdx(in.uv);
    let mat_duvy = dpdy(in.uv);
    mat_fw_uv = max(length(mat_duvx), length(mat_duvy));

    let mat = materials[in.material_idx & 0x3FFFFFFFu]; // mask off selection (bit31) + ghost (bit30)

    // Ray-parameter depth along camera_ray() — the one convention every consumer
    // reconstructs with. Perspective: |rel|. Ortho: forward distance. See gbuf_depth().
    let world_dist = gbuf_depth(in.world_position);

    // Normal (re-normalize after interpolation). TWO-SIDED: flip it to face the camera so a face wound
    // INWARD — which an imperfect boolean / extrude can leave — still shades as a solid surface instead
    // of a black "hole" (the reported "extrude/boolean looks open"). A correctly-wound outward face is
    // already facing the camera where it's visible, so this is a no-op for clean meshes.
    var n = normalize(in.world_normal);
    let view_dir = normalize(camera.cam_pos.xyz - in.world_position);
    // The same facing test as the flip — recorded before it erases the sign.
    let back_facing = dot(n, view_dir) < 0.0;
    if (back_facing) { n = -n; }

    let n_geo_world = n;

    // Shadow (from cascaded shadow maps). Normal-offset anti-acne bias is applied inside
    // compute_shadow, scaled by the SELECTED cascade's texel world-size (the old fixed
    // 0.03 offset was <5% of a cascade-2 texel → sawtooth self-shadowing acne).
    let shadow = compute_shadow(in.world_position, n);

    // --- Base material ---
    var color = mat.albedo.xyz;
    var roughness = mat.roughness_metallic.x;
    var metallic = mat.roughness_metallic.y;
    let opacity = mat.albedo.w;

    // --- Per-vertex paint colour (voxel sculpt-vertex-paint) ---------------------------------
    // The instance's USE_VERTEX_COLOR flag (bit 0) is set ONLY for painted voxel meshes; for every
    // other mesh uv stays a texture coordinate and this branch is skipped. uv.y > 0.5 marks a painted
    // vertex (unpainted voxels emit uv=(0,0) and keep the body material). uv.x holds the packed
    // 0x_FF_RR_GG_BB word as a bitcast f32 → unpack to rgb. Painted vertices bypass the procedural
    // clay layers below so the chosen colour shows exactly (that darkening is what made pink read as
    // dark clay — the reported bug).
    let use_vcol = (in.flags & 1u) != 0u && in.vcol_flag > 0.5;
    if (use_vcol) {
        let packed = bitcast<u32>(in.vcol_packed);
        let r = f32((packed >> 16u) & 0xFFu) / 255.0;
        let g = f32((packed >> 8u) & 0xFFu) / 255.0;
        let b = f32(packed & 0xFFu) / 255.0;
        color = vec3<f32>(r, g, b);
    }

    // --- Material texture maps (body-local box projection / mesh UV) ---
    // slot 0 = white sentinel ⇒ no-op. Box (triplanar) samples at the BODY-LOCAL position with
    // the BODY-LOCAL normal (textures stick to moving bodies); poly meshes may pick UV.
    // Painted voxel vertices keep their exact colour (maps skipped entirely).
    let p_local = in.local_position;
    let n_local = normalize(in.local_normal);

    // Albedo map — the ONLY layer compositor for Hybrid (P2 / HANDOFF Decision 1).
    // Multi-layer stacks (Noise Mult, curvature, masks) are fully flattened in the bake;
    // this path samples the result exactly like PT. No second inline composite.
    let pre_map_color = color;
    let amap_slot = bitcast<u32>(mat.albedo_map.x);
    if (!use_vcol && amap_slot != 0u) {
        let amap_strength = mat.albedo_map.y;
        let amap_tiling = mat.albedo_map.z;
        // w packed: bit0 = legacy UV(1)/Box(0), bit1 = absolute sample (layered Mult stack bake),
        // bits 2-4 = full MapProjection code. MUST match TexMap::unpack_projection.
        let amap_pack = bitcast<u32>(mat.albedo_map.w);
        let amap_absolute = (amap_pack & 2u) != 0u;
        // Dual-plane 16-bit when high_precision (bit 10); color stored as sRGB u16.
        let sampled = sample_mat_proj_hp(
            amap_slot, p_local, n_local, in.uv, amap_tiling, amap_pack, true);
        let str = clamp(amap_strength, 0.0, 1.0);
        if (amap_absolute) {
            // Layered bake already is base×noise1×noise2×… — REPLACE, never a free tint Mult
            // (that double Mult crushes dark stacks worse than Photoshop 8-bit).
            color = mix(pre_map_color, sampled, str);
        } else {
            // Classic PBR: scalar albedo is a TINT (final = albedo * sample).
            color = mix(color, color * sampled, str);
        }
    }

    // (P2) Vertex-curvature live composite REMOVED. INSTANCE_FLAG_VERTEX_CURVATURE / curv_params
    // / vertex_curv may still be bound for tooling, but they must not re-composite over the bake.
    // Curvature layers land in the albedo map via SdfCurvature in the panel bake chain.

    // Roughness / metallic maps: the sampled grayscale REPLACES the scalar by map strength
    // (ambientCG-style value maps hold the actual roughness/metalness — a tint-multiply against
    // the scalar default would darken them; replace is the correct semantic, unlike albedo).
    let rmap_slot = bitcast<u32>(mat.roughness_map.x);
    if (!use_vcol && rmap_slot != 0u) {
        let s = sample_mat_scalar_packed(rmap_slot, p_local, n_local, in.uv,
            mat.roughness_map.z, bitcast<u32>(mat.roughness_map.w));
        roughness = mix(roughness, s, clamp(mat.roughness_map.y, 0.0, 1.0));
    }
    let mmap_slot = bitcast<u32>(mat.metallic_map.x);
    if (!use_vcol && mmap_slot != 0u) {
        let s = sample_mat_scalar_packed(mmap_slot, p_local, n_local, in.uv,
            mat.metallic_map.z, bitcast<u32>(mat.metallic_map.w));
        metallic = mix(metallic, s, clamp(mat.metallic_map.y, 0.0, 1.0));
    }

    // Normal map: perturb via the tangentless whiteout triplanar blend in LOCAL space, then map
    // to world through the instance's normal matrix. ALWAYS triplanar — UV-space normal mapping
    // needs a per-vertex tangent basis, which no AZRA mesh carries (documented limitation; the
    // projection code on normal_map is ignored). The shadow factor above deliberately used the
    // GEOMETRIC normal: the normal-offset anti-acne bias is derived for the true surface slope,
    // and a detail-perturbed offset would re-introduce acne on high-frequency maps.
    let nmap_slot = bitcast<u32>(mat.normal_map.x);
    if (!use_vcol && nmap_slot != 0u) {
        let n_pert_local = sample_mat_triplanar_normal(
            nmap_slot, p_local, n_local, mat.normal_map.z, mat.normal_map.y,
            bitcast<u32>(mat.normal_map.w));
        let nmat = mat3x3<f32>(in.nmat0, in.nmat1, in.nmat2);
        n = normalize(nmat * n_pert_local);
        if (dot(in.world_normal, n_geo_world) < 0.0) { n = -n; }
    }

    // Bump map (height): screen-space gradient → normal perturbation (Mikkelsen-style,
    // no tangent basis required). Applied after the normal map so both can stack.
    let bmap_slot = bitcast<u32>(mat.bump_map.x);
    if (!use_vcol && bmap_slot != 0u) {
        let bump_h = sample_mat_scalar_packed(bmap_slot, p_local, n_local, in.uv,
            mat.bump_map.z, bitcast<u32>(mat.bump_map.w));
        let strength = clamp(mat.bump_map.y, 0.0, 4.0);
        // Surface-gradient bump (works on unparametrized meshes).
        let sigma_x = dpdx(in.world_position);
        let sigma_y = dpdy(in.world_position);
        let dbs = dpdx(bump_h);
        let dbt = dpdy(bump_h);
        n = material_bump_normal(n, sigma_x, sigma_y, dbs, dbt, strength);
    }

    // --- Noise pattern layer (BODY-LOCAL FBM — instant live scrub from node editor) ---
    // Evaluated every fragment on GPU — no CPU bake, no texture upload. Sliding Scale/Octaves
    // updates uniforms same-frame. amount=1 + albedo=color0 + noise_color=color1 = Noise→Albedo.
    // Seed offset: when edge_wear is off, edge_wear.w carries the Noise Seed (live path).
    //
    // ABSOLUTE Mult-stack maps already contain base×noise1×… — this live path is REPLACE
    // (mix(base, noise_color, mask)), NOT Mult. Applying it over an absolute bake wipes the
    // photo map into pure B&W noise. CPU clears noise_pattern on Mult install; this is the
    // shader-side belt-and-braces (mid-frame race / stale uniforms).
    let np_amt = mat.noise_params.x;
    let amap_is_absolute = (bitcast<u32>(mat.albedo_map.w) & 2u) != 0u;
    if (!use_vcol && np_amt > 0.001 && !amap_is_absolute) {
        let scale = mat.noise_params.y;
        let oct = u32(clamp(mat.noise_params.z, 1.0, 64.0));
        var seed = 0.0;
        if (mat.edge_wear.x < 0.001) {
            seed = mat.edge_wear.w;
        }
        let sp = p_local * scale + vec3(seed, seed * 0.6180339887, seed * 1.7320508075);
        var noise_val = fbm3(sp, oct);
        // Advanced blend curve (node-editor live path). Packed when cavity is off:
        //   cavity_dirt.w = 100 + mode (0 lin .. 4 levels)  — real cavity sharpness is ~0.5–10
        //   cavity_dirt.y = white, .z = k (power/mid), cavity_dirt_color.rgb = black
        //   cavity_dirt.x (amount) stays 0 so the cavity dirt layer does not run.
        let curve_pack = mat.cavity_dirt.w;
        if (curve_pack >= 100.0 && curve_pack < 105.0) {
            let black = mat.cavity_dirt_color.x;
            let white = mat.cavity_dirt.y;
            let kk = max(mat.cavity_dirt.z, 0.001);
            let mode = u32(curve_pack - 100.0 + 0.5);
            let denom = max(white - black, 1e-6);
            var x = saturate((noise_val - black) / denom);
            if (mode == 1u) {
                x = x * x * (3.0 - 2.0 * x); // smoothstep
            } else if (mode == 2u) {
                x = x * x * x * (x * (x * 6.0 - 15.0) + 10.0); // smootherstep
            } else if (mode == 3u) {
                x = pow(x, kk); // power
            } else if (mode == 4u) {
                x = pow(x, 1.0 / kk); // levels mid gamma
            }
            // mode 0: black/white remap only
            noise_val = x;
        }
        let mask = saturate(noise_val * np_amt);
        color = mix(color, mat.noise_color.xyz, mask);
        roughness = mix(roughness, mat.noise_color.w, mask);
        metallic = mix(metallic, mat.noise_params.w, mask);
    }

    // --- Screen-space curvature (approximation for edge wear + cavity dirt) ---
    // Uses dpdx/dpdy of world normal as a proxy for surface curvature.
    // Positive = convex (edges), negative = concave (cavities).
    let ew_amt = mat.edge_wear.x;
    let cd_amt = mat.cavity_dirt.x;
    if (!use_vcol && (ew_amt > 0.001 || cd_amt > 0.001)) {
        let dndx = dpdx(n);
        let dndy = dpdy(n);
        // Curvature magnitude: divergence of normal field
        let curv = (dndx.x + dndy.y) * 50.0; // scale to roughly match SDF curvature range

        // Cavity dirt (concave = negative curvature) — applied first, matches compute.wgsl order
        if (cd_amt > 0.001) {
            let sharpness = mat.cavity_dirt.w;
            let cavity_mask = smoothstep(0.0, -sharpness * 10.0, curv) * cd_amt;
            color = mix(color, mat.cavity_dirt_color.xyz, cavity_mask);
            roughness = mix(roughness, mat.cavity_dirt.y, cavity_mask);
            metallic = mix(metallic, mat.cavity_dirt.z, cavity_mask);
        }

        // Edge wear (convex = positive curvature) — applied second, matches compute.wgsl order
        if (ew_amt > 0.001) {
            let sharpness = mat.edge_wear.w;
            let edge_mask = smoothstep(0.0, sharpness * 10.0, curv) * ew_amt;
            color = mix(color, mat.edge_wear_color.xyz, edge_mask);
            roughness = mix(roughness, mat.edge_wear.y, edge_mask);
            metallic = mix(metallic, mat.edge_wear.z, edge_mask);
        }
    }

    // Emission luminance (packed into material channel). Emission MAP multiplies the
    // scalar emission (color * intensity) — grayscale maps act as intensity masks;
    // color maps tint the emission.
    var em_rgb = mat.emission_color.xyz * mat.roughness_metallic.z;
    let emap_slot = bitcast<u32>(mat.emission_map.x);
    if (!use_vcol && emap_slot != 0u) {
        var em_sample: vec3<f32>;
        let eproj = bitcast<u32>(mat.emission_map.w);
        em_sample = sample_mat_proj_hp(emap_slot, p_local, n_local, in.uv,
            mat.emission_map.z, eproj, true);
        // If scalar emission is black, the map alone drives emission (intensity as gain).
        let base_em = select(
            em_sample * max(mat.roughness_metallic.z, 1.0),
            em_rgb * em_sample,
            length(em_rgb) > 1e-5);
        em_rgb = mix(em_rgb, base_em, clamp(mat.emission_map.y, 0.0, 1.0));
    }
    let em_lum = length(em_rgb);

    // --- Advanced lobes (realtime approximate evaluator) ---
    // Same PbrMaterial fields as Properties / Master Material. Full glass transport,
    // random-walk SSS, and caustics belong to the path-tracer evaluator — here we bake
    // cheap visual cues into albedo/rough/metal so deferred lighting picks them up.
    let shader_type = u32(mat.roughness_metallic.w + 0.5);
    let sss_radius = mat.adv_params.x;
    let ior = max(mat.adv_params.y, 1.0);
    let clearcoat = mat.adv_params.z;
    let cc_rough = mat.adv_params.w;
    var sss_col = mat.adv_sss.xyz;
    // SSS color map (color, sRGB — same projection machinery as the emission map).
    // Sampled rgb REPLACES the scalar sss_color by map strength (roughness/metallic
    // replace semantic: the panel's layered bake already holds the full composite).
    let smap_slot = bitcast<u32>(mat.sss_color_map.x);
    if (!use_vcol && smap_slot != 0u) {
        let sproj = bitcast<u32>(mat.sss_color_map.w);
        let sss_sample = sample_mat_proj(smap_slot, p_local, n_local, in.uv,
            mat.sss_color_map.z, unpack_proj_code(sproj),
            unpack_blend(sproj), unpack_triplanar_rot(sproj));
        sss_col = mix(sss_col, sss_sample, clamp(mat.sss_color_map.y, 0.0, 1.0));
    }
    let sheen = mat.adv_sss.w;
    let glass_tint = mat.adv_glass.xyz;
    let sheen_tint = mat.adv_glass.w;
    let ndv = max(dot(n, view_dir), 0.0);

    // Subsurface: warm wrap tint + softer dielectric (shader Subsurface/Advanced or radius>0)
    let sss_on = sss_radius > 0.001
        && (shader_type == 1u || shader_type == 4u || sss_radius > 0.05);
    if (sss_on && !use_vcol) {
        let wrap = clamp(sss_radius * 0.22, 0.0, 0.9);
        let edge = 1.0 - ndv;
        color = mix(color, mix(color, sss_col, 0.7), wrap * (0.35 + 0.65 * edge));
        roughness = mix(roughness, min(roughness + 0.12 * wrap, 1.0), wrap);
        metallic = metallic * (1.0 - wrap * 0.55);
    }

    // Clearcoat: slightly glossier top coat (preview of dual-lobe)
    if (clearcoat > 0.01 && !use_vcol) {
        roughness = mix(roughness, min(roughness, max(cc_rough, 0.04)), clearcoat * 0.55);
        color = color * (1.0 - clearcoat * 0.06);
    }

    // Glass: dielectric + IOR-aware F0 cue + optional tint (opacity already drives alpha mix)
    let glass_on = shader_type == 2u || (shader_type == 4u && opacity < 0.995 && ior > 1.01);
    if (glass_on && !use_vcol) {
        // F0 from IOR; deferred uses mix(0.04, albedo, metal) — fake higher F0 via mild metal.
        let f0 = pow((ior - 1.0) / (ior + 1.0), 2.0);
        let f0_boost = clamp((f0 - 0.04) / 0.16, 0.0, 1.0);
        metallic = mix(metallic, min(metallic + f0_boost * 0.35, 0.55), 0.85);
        if (length(glass_tint) > 0.01) {
            color = mix(color, color * glass_tint, 0.75);
        }
        // Clear glass tends glossy unless artist raised roughness
        roughness = mix(roughness, min(roughness, 0.08), 0.35);
    }

    // Sheen: grazing brightening (cloth / velvet cue)
    if (sheen > 0.01 && !use_vcol) {
        let rim = pow(1.0 - ndv, 3.0) * sheen;
        let tint = mix(vec3(1.0), color, clamp(sheen_tint, 0.0, 1.0));
        color = color + tint * rim * 0.45;
    }

    // VFX / Unlit: push emission so deferred additive path reads as unlit-ish
    if (shader_type == 3u) {
        // Keep albedo; emission_intensity already in roughness_metallic.z — boost stored em_lum
        // by treating non-black albedo as emissive base when intensity is low.
    }

    // Emission tinting of stored albedo (matches SDF raymarch_gbuf convention):
    // If emissive, blend albedo toward normalized emission color so deferred_shade
    // can reconstruct emission direction from the albedo channel.
    var stored_color = color;
    var out_em_lum = em_lum;
    if (shader_type == 3u) {
        // VFX: ensure some emissive read so unlit materials don't go fully dark in shade
        out_em_lum = max(em_lum, 0.35);
        stored_color = mix(stored_color, color, 0.5);
    }
    if (out_em_lum > 0.001) {
        let em_weight = clamp(out_em_lum / (out_em_lum + 1.0), 0.0, 1.0);
        let em_dir = select(color, em_rgb / max(length(em_rgb), 0.001), length(em_rgb) > 1e-5);
        stored_color = mix(color, em_dir, em_weight);
    }

    // flags: 0.0=bg, 0.5=floor, 0.61=ghost, 0.68/0.88=vfx, 0.74=dc_wire_edge, 0.78=geometry, 1.0=selected
    // Selection state packed in material_idx bit 31, ghost in bit 30 by the CPU.
    let is_selected = (in.material_idx & 0x80000000u) != 0u;
    let is_ghost = (in.material_idx & 0x40000000u) != 0u;
    var gbuf_flags = select(select(0.78, 0.61, is_ghost), 1.0, is_selected);
    var out: GBufferOutput;
    out.normal_depth = vec4(n, world_dist);
    out.material = vec4(roughness, metallic, shadow, opacity + out_em_lum);
    var final_color = stored_color;
    // Cluster debug view (camera._pad.y flag + uv.y>=2.0 marker carried ONLY by the
    // cluster debug vertex stream — normal meshes can never take this branch):
    // hue = LOD level, brightness jitter = cluster id → live LOD rings + patch seams.
    if (camera._pad.y > 0.5 && in.uv.y >= 2.0) {
        let t = clamp(in.uv.x, 0.0, 1.0) * 5.0;
        let hue = vec3<f32>(
            clamp(abs(t - 2.5) - 0.5, 0.0, 1.0),
            clamp(1.5 - abs(t - 1.5), 0.0, 1.0),
            clamp(1.5 - abs(t - 3.5), 0.0, 1.0),
        );
        let idv = fract(in.uv.y);
        // Loud unlit palette: VFX flag + emission so deferred lighting cannot wash
        // LOD rings to mud. material.w = opacity(1) + em_lum(2).
        final_color = (hue * 0.85 + vec3<f32>(0.12)) * (0.65 + 0.35 * idv);
        out.material = vec4(1.0, 0.0, 1.0, 1.0 + 2.0);
        gbuf_flags = 0.88;
    }
    out.albedo_flags = vec4(final_color, gbuf_flags);
    out.motion = select(f32(in.body_index), PROBE_BACKFACE_MARK,
        back_facing && camera.frame_flags.x > 0.5);
    return out;
}

// GPU DC-wire pass: POLYGON_MODE_LINE over the same triangle meshes.
// Flags 0.74 sits between VFX (<0.73) and geometry (0.78).
// material.xy = (thickness_px, opacity) for deferred dilation / blend.
// camera.wire_color = rgb + opacity; camera._pad.x = thickness (set by CPU).
@fragment
fn fs_gbuffer_wire(in: VertexOutput) -> GBufferOutput {
    let world_dist = gbuf_depth(in.world_position);
    var n = normalize(in.world_normal);
    let view_dir = normalize(camera.cam_pos.xyz - in.world_position);
    if (dot(n, view_dir) < 0.0) { n = -n; }
    let col = camera.wire_color.xyz;
    let opacity = clamp(camera.wire_color.a, 0.0, 1.0);
    let thickness = max(camera._pad.x, 0.0);
    var out: GBufferOutput;
    out.normal_depth = vec4(n, world_dist);
    out.material = vec4(thickness, opacity, 0.0, 0.0);
    out.albedo_flags = vec4(col, 0.74); // DC wire edge marker
    out.motion = f32(in.body_index);
    return out;
}
