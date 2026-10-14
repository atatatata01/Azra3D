// ============================================================
// optix_programs.cu — OptiX RT core programs for SDF ray tracing
// Hardware-accelerated BVH traversal with custom SDF intersection
// ============================================================

#include "sdf_eval.cuh"
#include <optix_device.h>

// ---- Launch parameters (shared between host and device) ----
struct OptixLaunchParams {
    const GpuUniforms* cfg;
    const float*       prim_data;
    float4*            gbuf_normal;
    float4*            gbuf_material;
    uchar4*            gbuf_albedo;
    unsigned int       width;
    unsigned int       height;
    OptixTraversableHandle traversable;
};

extern "C" __constant__ OptixLaunchParams params;

// ---- Payload: data carried per ray ----
// We use 8 payload registers to carry hit info
// [0-2]: hit position (float3)
// [3]:   hit distance (float)
// [4-6]: hit normal (float3)
// [7]:   hit prim index (uint, packed as float)

static __forceinline__ __device__ void setPayloadFloat3(unsigned int base, float3 v) {
    optixSetPayload_0(base == 0 ? __float_as_uint(v.x) : optixGetPayload_0());
    optixSetPayload_1(base == 0 ? __float_as_uint(v.y) : (base == 4 ? __float_as_uint(v.x) : optixGetPayload_1()));
    optixSetPayload_2(base == 0 ? __float_as_uint(v.z) : (base == 4 ? __float_as_uint(v.y) : optixGetPayload_2()));
    // Simplified: just use direct calls per base
}

// Cleaner approach: set individual payload slots
static __forceinline__ __device__ void storeHitPayload(float3 pos, float t, float3 normal, unsigned int prim_idx) {
    optixSetPayload_0(__float_as_uint(pos.x));
    optixSetPayload_1(__float_as_uint(pos.y));
    optixSetPayload_2(__float_as_uint(pos.z));
    optixSetPayload_3(__float_as_uint(t));
    optixSetPayload_4(__float_as_uint(normal.x));
    optixSetPayload_5(__float_as_uint(normal.y));
    optixSetPayload_6(__float_as_uint(normal.z));
    optixSetPayload_7(prim_idx);
}

// ---- Raygen: generate primary camera rays ----
extern "C" __global__ void __raygen__primary() {
    const unsigned int gx = optixGetLaunchIndex().x;
    const unsigned int gy = optixGetLaunchIndex().y;
    if (gx >= params.width || gy >= params.height) return;
    const unsigned int pixel = gy * params.width + gx;

    const GpuUniforms* cfg = params.cfg;
    float w = (float)params.width, h = (float)params.height;
    float uv_x = ((float)gx / w) * 2.0f - 1.0f;
    float uv_y = -(((float)gy / h) * 2.0f - 1.0f);
    float aspect = w / h;

    CameraRay ray = camera_ray(cfg, uv_x, uv_y, aspect);

    // Initialize payload (miss values)
    unsigned int p0 = 0, p1 = 0, p2 = 0, p3 = 0;
    unsigned int p4 = 0, p5 = 0, p6 = 0, p7 = 0xFFFFFFFF;

    optixTrace(
        params.traversable,
        make_float3(ray.origin.x, ray.origin.y, ray.origin.z),
        make_float3(ray.dir.x, ray.dir.y, ray.dir.z),
        0.0f,                          // tmin
        cfg->far_clip,                 // tmax
        0.0f,                          // rayTime
        0xFF,                          // visibilityMask
        OPTIX_RAY_FLAG_NONE,           // rayFlags
        0,                             // SBT offset (ray type 0 = primary)
        1,                             // SBT stride (1 ray type)
        0,                             // missSBT index
        p0, p1, p2, p3, p4, p5, p6, p7
    );

    // Decode payload
    float hit_t = __uint_as_float(p3);
    unsigned int hit_prim = p7;

    if (hit_prim == 0xFFFFFFFF) {
        // Miss — background
        float3 bg = bg_gradient(cfg, (float)gy, h);
        params.gbuf_normal[pixel] = make_float4(0, 0, 0, 0);
        params.gbuf_material[pixel] = make_float4(0, 0, 0, 0);
        params.gbuf_albedo[pixel] = make_uchar4(
            (unsigned char)(__saturatef(bg.x) * 255.0f),
            (unsigned char)(__saturatef(bg.y) * 255.0f),
            (unsigned char)(__saturatef(bg.z) * 255.0f),
            0);
        return;
    }

    float3 hit_pos = make_f3(__uint_as_float(p0), __uint_as_float(p1), __uint_as_float(p2));
    float3 normal = make_f3(__uint_as_float(p4), __uint_as_float(p5), __uint_as_float(p6));

    // Material evaluation
    const float* pd = params.prim_data;
    bool is_selected = prim_is_selected(pd, hit_prim);
    float3 mat_color = prim_albedo(pd, hit_prim);
    float mat_rough = 0.65f, mat_metal = 0.0f, mat_opacity = 1.0f;
    float3 mat_emission = make_f3(0, 0, 0);

    if (cfg->view_mode == 0u) {
        MatResult mat = eval_material(cfg, pd, hit_prim, hit_pos, normal);
        mat_color = mat.color;
        mat_rough = mat.roughness;
        mat_metal = mat.metallic;
        mat_opacity = mat.opacity;
        mat_emission = mat.emission;
    }

    // Shadow + AO (reuse SDF-based computation for now)
    unsigned int quality = cfg->render_quality;
    float3 key_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));
    float shadow = 1.0f;
    if (quality >= 1u) {
        float shadow_k = quality >= 3u ? 16.0f : (quality >= 2u ? 12.0f : 8.0f);
        shadow = soft_shadow(cfg, pd, add3(hit_pos, mul3f(normal, 0.01f)), key_dir, 0.02f, 8.0f, shadow_k);
    }
    float ao = 1.0f;
    if (quality >= 1u) ao = calc_ao(cfg, pd, hit_pos, normal);

    // Flags encoding
    bool is_vfx = prim_shader_type(pd, hit_prim) == 3u;
    float flags;
    if (is_vfx) flags = is_selected ? 0.88f : 0.68f;
    else flags = is_selected ? 1.0f : 0.78f;

    // Emission tinting
    float em_lum = length3(mat_emission);
    float3 stored_color = mat_color;
    if (em_lum > 0.001f) {
        float em_weight = __saturatef(em_lum / (em_lum + 1.0f));
        stored_color = mix3(mat_color, mul3f(mat_emission, 1.0f / fmaxf(em_lum, 0.001f)), em_weight);
    }

    // Write G-buffer
    params.gbuf_normal[pixel] = make_float4(normal.x, normal.y, normal.z, hit_t);
    params.gbuf_material[pixel] = make_float4(mat_rough, mat_metal, shadow * ao, mat_opacity + em_lum);
    params.gbuf_albedo[pixel] = make_uchar4(
        (unsigned char)(__saturatef(stored_color.x) * 255.0f),
        (unsigned char)(__saturatef(stored_color.y) * 255.0f),
        (unsigned char)(__saturatef(stored_color.z) * 255.0f),
        (unsigned char)(flags * 255.0f));
}

// ---- Custom intersection: single-prim SDF sphere tracing ----
// Called by RT core when a ray hits an AABB in the BVH.
// We sphere-trace the single primitive within the AABB.
extern "C" __global__ void __intersection__sdf() {
    const unsigned int prim_idx = optixGetPrimitiveIndex();
    const float3 ray_o = optixGetWorldRayOrigin();
    const float3 ray_d = optixGetWorldRayDirection();
    const float tmin = optixGetRayTmin();
    const float tmax = optixGetRayTmax();

    const GpuUniforms* cfg = params.cfg;
    const float* pd = params.prim_data;

    // Sphere trace within this primitive's AABB
    float t = tmin;
    const unsigned int max_steps = 48u;
    float3 o = make_f3(ray_o.x, ray_o.y, ray_o.z);
    float3 d = make_f3(ray_d.x, ray_d.y, ray_d.z);

    for (unsigned int i = 0; i < max_steps; i++) {
        float3 p = add3(o, mul3f(d, t));

        // Evaluate single primitive SDF
        float dist = eval_prim(cfg, pd, prim_idx, p);

        if (dist < cfg->hit_threshold) {
            // Verify against full scene SDF (handles boolean operations)
            float scene_d = scene_sdf(cfg, pd, p);
            if (scene_d < cfg->hit_threshold * 2.0f) {
                // True hit — report intersection
                // Store primitive index as attribute for closest-hit
                optixReportIntersection(t, 0, prim_idx);
            }
            return;
        }

        t += dist * 0.7f;
        if (t > tmax) return;
    }
}

// ---- Closest hit: verify and store hit data ----
extern "C" __global__ void __closesthit__sdf() {
    const float t = optixGetRayTmax();
    const float3 ray_o = optixGetWorldRayOrigin();
    const float3 ray_d = optixGetWorldRayDirection();

    const GpuUniforms* cfg = params.cfg;
    const float* pd = params.prim_data;

    float3 o = make_f3(ray_o.x, ray_o.y, ray_o.z);
    float3 d = make_f3(ray_d.x, ray_d.y, ray_d.z);
    float3 hit_pos = add3(o, mul3f(d, t));

    // Compute normal via central differences on full scene SDF
    float3 normal = calc_normal(cfg, pd, hit_pos);

    // Find the actual hit primitive (may differ from AABB prim due to booleans)
    unsigned int hit_prim = find_hit_prim(cfg, pd, hit_pos);

    storeHitPayload(hit_pos, t, normal, hit_prim);
}

// ---- Miss: background/sky ----
extern "C" __global__ void __miss__background() {
    // Signal miss via prim_idx = 0xFFFFFFFF (already initialized in raygen)
    optixSetPayload_7(0xFFFFFFFF);
}
