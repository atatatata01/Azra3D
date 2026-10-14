// ============================================================
// Shadow Map — Depth-Only Shader (+ material displacement)
// ============================================================
//
// Renders meshes from the light's perspective into a depth-only texture.
// No fragment shader needed — hardware writes depth automatically.
//
// DISPLACEMENT PARITY (2026-07-18): the caster MUST apply the SAME per-vertex
// material displacement as the G-buffer vertex shader (gbuffer.wgsl vs_main), or
// a surface pushed out by a displacement map casts its shadow from the UNdisplaced
// position — the shadow detaches from the bump as amplitude rises. The material
// storage + texture + sampler are bound as group(1) here (group(0) is the per-
// cascade light uniform); the MaterialData struct + sample_mat_* helpers below are
// copied from gbuffer.wgsl and PINNED against drift by
// `shadow_material_helpers_match_gbuffer` in shadows.rs — if the G-buffer helpers
// or the struct layout change, that test fails until this copy is re-synced.

struct LightViewUniforms {
    light_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> light: LightViewUniforms;

// ---- Material bindings (group 1) ----
// Mirror gbuffer.wgsl group(0) bindings 1..3 (materials / texture array / sampler),
// re-homed to group(1) since group(0) here is the light uniform. Same underlying GPU
// resources (RasterPipeline.material_buffer + MaterialTextureArray), rebound for this
// pipeline by ShadowMapSystem::rebuild_material_bind_group.
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
    albedo_map: vec4<f32>,
    normal_map: vec4<f32>,
    roughness_map: vec4<f32>,
    metallic_map: vec4<f32>,
    emission_map: vec4<f32>,
    bump_map: vec4<f32>,
    displacement_map: vec4<f32>,
    sss_color_map: vec4<f32>,
    // Advanced lobes — lockstep with gbuffer.wgsl / RasterMaterial (320 B).
    adv_params: vec4<f32>,
    adv_sss: vec4<f32>,
    adv_glass: vec4<f32>,
};

@group(1) @binding(0) var<storage, read> materials: array<MaterialData>;
@group(1) @binding(1) var mat_tex_array: texture_2d_array<f32>;
@group(1) @binding(2) var mat_tex_sampler: sampler;

// ---- Material Map Sampling (verbatim copy of gbuffer.wgsl; drift-guarded) ----
// The shadow caster samples the DISPLACEMENT map to move its vertices; if it projected
// differently from the G-buffer the shadow would detach from the surface casting it. So
// every projection lives here too, not just the ones displacement happens to use today.
const TRIPLANAR_BLEND_DEFAULT: f32 = 4.0;

// ---- Mip selection (copy of gbuffer.wgsl) ----
// The shadow pass samples from the VERTEX shader only, so the footprint context stays at its
// zero init → mat_lod returns 0 → mip 0, exactly the pre-mip behaviour. The vars + helper exist
// here so the drift-guarded sampler bodies stay verbatim copies of gbuffer.wgsl.
var<private> mat_fw_world: f32 = 0.0;  // max |d p_local / d pixel| (body-local units)
var<private> mat_fw_uv: f32 = 0.0;     // max |d uv / d pixel| (UV units)

fn mat_lod(fw_tiles: f32) -> f32 {
    if (fw_tiles <= 0.0) { return 0.0; }
    let texels = fw_tiles * f32(textureDimensions(mat_tex_array).x);
    return clamp(log2(max(texels, 1.0)), 0.0, f32(textureNumLevels(mat_tex_array) - 1u));
}

fn triplanar_rot_uv(uv: vec2<f32>, q: u32) -> vec2<f32> {
    let qq = q & 3u;
    if (qq == 1u) { return vec2<f32>(-uv.y, uv.x); }
    if (qq == 2u) { return vec2<f32>(-uv.x, -uv.y); }
    if (qq == 3u) { return vec2<f32>(uv.y, -uv.x); }
    return uv;
}

fn sample_mat_triplanar(slot: u32, p: vec3<f32>, normal: vec3<f32>, tiling: f32) -> vec3<f32> {
    return sample_mat_triplanar_b(slot, p, normal, tiling, TRIPLANAR_BLEND_DEFAULT, 0u);
}

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

fn sample_mat_uv(slot: u32, uv: vec2<f32>, tiling: f32) -> vec3<f32> {
    let lod = mat_lod(mat_fw_uv * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, uv * tiling, i32(slot), lod).rgb;
}

fn sample_mat_sphere(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let d = normalize(select(p, vec3<f32>(0.0, 1.0, 0.0), length(p) < 1e-6));
    // atan2 spans -PI..PI -> 0..1; asin spans -PI/2..PI/2 -> 0..1.
    let u = atan2(d.z, d.x) * 0.15915494 + 0.5;   // 1/(2*PI)
    let v = asin(clamp(d.y, -1.0, 1.0)) * 0.31830989 + 0.5;   // 1/PI
    // Angular projection: uv moves ~1/(PI*radius) per world unit (isotropic approximation).
    let lod = mat_lod(mat_fw_world * tiling / max(length(p) * 3.14159265, 1e-4));
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, vec2<f32>(u, v) * tiling, i32(slot), lod).rgb;
}

fn sample_mat_cylinder(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let u = atan2(p.z, p.x) * 0.15915494 + 0.5;
    // v (height) moves 1:1 with world units — the dominant axis for radius > 1/(2*PI).
    let lod = mat_lod(mat_fw_world * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, vec2<f32>(u, p.y) * tiling, i32(slot), lod).rgb;
}

fn sample_mat_planar(slot: u32, p: vec3<f32>, tiling: f32) -> vec3<f32> {
    let lod = mat_lod(mat_fw_world * tiling);
    return textureSampleLevel(mat_tex_array, mat_tex_sampler, p.xy * tiling, i32(slot), lod).rgb;
}

fn sample_mat_proj(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32, blend: f32, rot_pack: u32) -> vec3<f32> {
    if (proj == 1u) { return sample_mat_uv(slot, uv, tiling); }
    if (proj == 2u) { return sample_mat_sphere(slot, p, tiling); }
    if (proj == 3u) { return sample_mat_cylinder(slot, p, tiling); }
    if (proj == 4u) { return sample_mat_planar(slot, p, tiling); }
    return sample_mat_triplanar_b(slot, p, normal, tiling, blend, rot_pack);
}

fn unpack_proj_code(packed: u32) -> u32 {
    let wide = (packed >> 2u) & 0x7u;
    if (wide != 0u) { return wide; }
    return packed & 1u;
}

fn sample_mat_scalar(slot: u32, p: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, tiling: f32, proj: u32) -> f32 {
    return sample_mat_proj(slot, p, normal, uv, tiling, proj, TRIPLANAR_BLEND_DEFAULT, 0u).r;
}
// ---- End Material Map Sampling ----

struct VertexInput {
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
};

@vertex
fn vs_shadow(in: VertexInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let normal_mat = mat3x3<f32>(
        in.normal_mat_0.xyz,
        in.normal_mat_1.xyz,
        in.normal_mat_2.xyz,
    );
    var world_pos = (model * vec4(in.position, 1.0)).xyz;
    let world_normal = normalize(normal_mat * in.normal);

    // Material displacement — MUST MATCH gbuffer.wgsl vs_main's displacement block exactly
    // (the caster has to move its vertices identically to the G-buffer surface, or the shadow
    // detaches from the bump). Sampled in BODY-LOCAL space (in.position / in.normal) so tiles
    // stick to the body; offset along the world-space geometric normal. A no-op (slot 0 =
    // "no map") for any material without a displacement map — zero cost for the common case.
    {
        let mat = materials[in.material_idx & 0x3FFFFFFFu];
        let dmap_slot = bitcast<u32>(mat.displacement_map.x);
        if (dmap_slot != 0u) {
            let n_local = normalize(in.normal);
            let h = sample_mat_scalar(
                dmap_slot, in.position, n_local, in.uv,
                mat.displacement_map.z, bitcast<u32>(mat.displacement_map.w));
            let amp = (h - 0.5) * 2.0 * clamp(mat.displacement_map.y, 0.0, 4.0);
            world_pos = world_pos + world_normal * amp;
        }
    }

    return light.light_view_proj * vec4(world_pos, 1.0);
}
