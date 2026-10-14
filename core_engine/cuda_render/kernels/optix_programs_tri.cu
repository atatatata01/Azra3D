// ============================================================
// optix_programs_tri.cu — Hardware RT triangle path tracer
// RT-core triangle GAS traversal + progressive Monte Carlo PT
// ============================================================

#include "common.cuh"
#include <optix_device.h>

// Per-material packed shading (matches Rust HwRtMaterial, 32 B).
struct HwRtMaterial {
    float3 albedo;
    float  roughness;
    float3 emission;
    float  metallic;
};

// Launch params shared with host (see optix_tri_pipeline.rs LaunchParamsTri).
struct OptixTriLaunchParams {
    const GpuUniforms* cfg;
    const float3*      positions;   // world-space vertex positions
    const float3*      normals;     // world-space vertex normals
    const unsigned int* indices;    // 3 * num_triangles
    const unsigned int* tri_mat;    // material index per triangle
    const HwRtMaterial* materials;
    float4*            pt_accum;    // progressive accumulation (read + write)
    float4*            hdr_out;
    unsigned int       width;
    unsigned int       height;
    unsigned int       num_tris;
    unsigned int       num_mats;
    OptixTraversableHandle traversable;
};

extern "C" __constant__ OptixTriLaunchParams params;

// Payload:
//  p0: hit flag (1/0)
//  p1: t
//  p2-p4: world normal
//  p5: triangle index
//  p6-p7: barycentrics u,v

static __forceinline__ __device__ float3 cosine_hemisphere(float3 n, unsigned int* seed) {
    float r1 = rand_float(seed);
    float r2 = rand_float(seed);
    float phi = 2.0f * 3.14159265f * r1;
    float cos_theta = sqrtf(1.0f - r2);
    float sin_theta = sqrtf(r2);

    float3 tang;
    if (fabsf(n.y) < 0.999f) tang = normalize3(cross3(make_f3(0, 1, 0), n));
    else tang = normalize3(cross3(make_f3(1, 0, 0), n));
    float3 bitan = cross3(n, tang);

    return normalize3(add3(add3(
        mul3f(tang, cosf(phi) * sin_theta),
        mul3f(bitan, sinf(phi) * sin_theta)),
        mul3f(n, cos_theta)));
}

static __forceinline__ __device__ void trace_closest(
    float3 origin, float3 dir, float tmin, float tmax,
    unsigned int* p0, unsigned int* p1, unsigned int* p2, unsigned int* p3,
    unsigned int* p4, unsigned int* p5, unsigned int* p6, unsigned int* p7
) {
    *p0 = 0; *p1 = 0; *p2 = 0; *p3 = 0;
    *p4 = 0; *p5 = 0xFFFFFFFFu; *p6 = 0; *p7 = 0;
    optixTrace(
        params.traversable,
        origin, dir,
        tmin, tmax,
        0.0f,
        OptixVisibilityMask(255),
        OPTIX_RAY_FLAG_DISABLE_ANYHIT,
        0, 1, 0,
        *p0, *p1, *p2, *p3, *p4, *p5, *p6, *p7
    );
}

// ---- Closest hit: hardware triangle intersection result ----
extern "C" __global__ void __closesthit__triangle() {
    const float t = optixGetRayTmax();
    const unsigned int tri = optixGetPrimitiveIndex();
    const float2 bary = optixGetTriangleBarycentrics();
    const float u = bary.x;
    const float v = bary.y;
    const float w = 1.0f - u - v;

    // Interpolated shading normal from vertex attributes
    const unsigned int i0 = params.indices[tri * 3u + 0u];
    const unsigned int i1 = params.indices[tri * 3u + 1u];
    const unsigned int i2 = params.indices[tri * 3u + 2u];
    float3 n0 = params.normals[i0];
    float3 n1 = params.normals[i1];
    float3 n2 = params.normals[i2];
    float3 n = normalize3(add3(add3(mul3f(n0, w), mul3f(n1, u)), mul3f(n2, v)));

    // Face forward against ray
    const float3 ray_d = optixGetWorldRayDirection();
    if (dot3(n, ray_d) > 0.0f) n = mul3f(n, -1.0f);

    optixSetPayload_0(1u);
    optixSetPayload_1(__float_as_uint(t));
    optixSetPayload_2(__float_as_uint(n.x));
    optixSetPayload_3(__float_as_uint(n.y));
    optixSetPayload_4(__float_as_uint(n.z));
    optixSetPayload_5(tri);
    optixSetPayload_6(__float_as_uint(u));
    optixSetPayload_7(__float_as_uint(v));
}

// ---- Miss ----
extern "C" __global__ void __miss__tri() {
    optixSetPayload_0(0u);
    optixSetPayload_5(0xFFFFFFFFu);
}

// ---- Raygen: progressive path tracer using HW traversal ----
extern "C" __global__ void __raygen__pathtrace_tri() {
    const unsigned int gx = optixGetLaunchIndex().x;
    const unsigned int gy = optixGetLaunchIndex().y;
    if (gx >= params.width || gy >= params.height) return;
    if (params.traversable == 0 || params.num_tris == 0u) return;

    const unsigned int pixel = gy * params.width + gx;
    const GpuUniforms* cfg = params.cfg;
    float w = (float)params.width, h = (float)params.height;
    float aspect = w / h;

    unsigned int seed = pcg_hash(gx + gy * params.width + cfg->pt_frame_seed * params.width * params.height);

    unsigned int spp = cfg->pt_samples_per_frame > 0u ? cfg->pt_samples_per_frame : 1u;
    if (cfg->pt_focus_uv_x >= 0.0f) {
        float2 pixel_uv = make_float2((float)gx / w, (float)gy / h);
        float fd = length2(make_float2(pixel_uv.x - cfg->pt_focus_uv_x, pixel_uv.y - cfg->pt_focus_uv_y));
        if (fd < cfg->pt_focus_radius) spp *= 4u;
    }

    float3 sample_sum = make_f3(0, 0, 0);
    const float t_far = cfg->far_clip > 0.0f ? cfg->far_clip : 500.0f;

    for (unsigned int s = 0; s < spp; s++) {
        float jx = rand_float(&seed) - 0.5f;
        float jy = rand_float(&seed) - 0.5f;
        float uv_x = (((float)gx + jx) / w) * 2.0f - 1.0f;
        float uv_y = -((((float)gy + jy) / h) * 2.0f - 1.0f);

        CameraRay cam = camera_ray(cfg, uv_x, uv_y, aspect);
        float3 ray_o = cam.origin;
        float3 ray_d = cam.dir;
        float3 throughput = make_f3(1, 1, 1);
        float3 radiance = make_f3(0, 0, 0);

        unsigned int max_b = cfg->pt_max_bounces > 0u ? cfg->pt_max_bounces : 4u;
        if (max_b > 16u) max_b = 16u;

        for (unsigned int bounce = 0; bounce <= max_b; bounce++) {
            unsigned int p0, p1, p2, p3, p4, p5, p6, p7;
            trace_closest(ray_o, ray_d, 0.001f, t_far, &p0, &p1, &p2, &p3, &p4, &p5, &p6, &p7);

            if (p0 == 0u) {
                // Sky
                float sky_t = ray_d.y * 0.5f + 0.5f;
                float3 sky_top = make_f3(
                    (float)((cfg->bg_top_packed >> 16u) & 0xFFu) / 255.0f,
                    (float)((cfg->bg_top_packed >> 8u) & 0xFFu) / 255.0f,
                    (float)(cfg->bg_top_packed & 0xFFu) / 255.0f);
                float3 sky_bot = make_f3(
                    (float)((cfg->bg_bottom_packed >> 16u) & 0xFFu) / 255.0f,
                    (float)((cfg->bg_bottom_packed >> 8u) & 0xFFu) / 255.0f,
                    (float)(cfg->bg_bottom_packed & 0xFFu) / 255.0f);
                radiance = add3(radiance, mul3(throughput, mul3f(mix3(sky_bot, sky_top, sky_t), 0.5f)));
                break;
            }

            float t = __uint_as_float(p1);
            float3 n = make_f3(__uint_as_float(p2), __uint_as_float(p3), __uint_as_float(p4));
            unsigned int tri = p5;
            float3 hit_p = add3(ray_o, mul3f(ray_d, t));

            // Material
            float3 alb = make_f3(0.66f, 0.585f, 0.53f);
            float rough = 0.65f, metal = 0.0f;
            float3 emission = make_f3(0, 0, 0);
            if (tri < params.num_tris && params.tri_mat && params.materials) {
                unsigned int mi = params.tri_mat[tri];
                if (mi < params.num_mats) {
                    HwRtMaterial mat = params.materials[mi];
                    alb = mat.albedo;
                    rough = mat.roughness;
                    metal = mat.metallic;
                    emission = mat.emission;
                }
            }
            if (length3(emission) > 0.001f) {
                radiance = add3(radiance, mul3(throughput, emission));
            }

            // NEE: directional key light with HW shadow ray
            float3 l_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));
            float l_int = cfg->light_dir.w;
            float3 l_col = make_f3(cfg->light_color.x, cfg->light_color.y, cfg->light_color.z);
            float ndl = fmaxf(dot3(n, l_dir), 0.0f);
            if (ndl > 0.0f && l_int > 0.0f) {
                unsigned int s0, s1, s2, s3, s4, s5, s6, s7;
                float3 shadow_o = add3(hit_p, mul3f(n, 0.002f));
                trace_closest(shadow_o, l_dir, 0.001f, t_far * 0.5f,
                    &s0, &s1, &s2, &s3, &s4, &s5, &s6, &s7);
                float shd = (s0 == 0u) ? 1.0f : 0.0f;
                float dw = 1.0f - metal * 0.7f;
                radiance = add3(radiance, mul3(throughput, mul3f(mul3(alb, l_col), l_int * ndl * dw * shd)));
            }

            // Ambient on first bounce
            if (bounce == 0u) {
                radiance = add3(radiance, mul3(throughput, mul3f(alb, cfg->light_color.w * 0.3f)));
            }

            // Russian roulette
            if (bounce >= 2u) {
                float rr_p = fmaxf(throughput.x, fmaxf(throughput.y, throughput.z));
                rr_p = fminf(rr_p, 0.95f);
                if (rand_float(&seed) > rr_p) break;
                throughput = mul3f(throughput, 1.0f / fmaxf(rr_p, 1e-3f));
            }

            // Next bounce direction
            float is_spec_r = rand_float(&seed);
            float spec_prob = 0.04f + 0.86f * metal;
            if (is_spec_r < spec_prob) {
                float ndrd = dot3(n, ray_d);
                float3 refl = sub3(ray_d, mul3f(n, 2.0f * ndrd));
                float px = (rand_float(&seed) - 0.5f) * rough;
                float py = (rand_float(&seed) - 0.5f) * rough;
                float3 tang;
                if (fabsf(n.y) < 0.999f) tang = normalize3(cross3(make_f3(0, 1, 0), n));
                else tang = normalize3(cross3(make_f3(1, 0, 0), n));
                float3 bitan = cross3(n, tang);
                ray_d = normalize3(add3(refl, add3(mul3f(tang, px), mul3f(bitan, py))));
                if (dot3(ray_d, n) < 0.0f) ray_d = refl;
                throughput = mul3(throughput, mix3(make_f3(1, 1, 1), alb, metal));
            } else {
                ray_d = cosine_hemisphere(n, &seed);
                throughput = mul3(throughput, alb);
            }
            ray_o = add3(hit_p, mul3f(n, 0.002f));

            // Firefly clamp
            float thr_lum = 0.2126f * throughput.x + 0.7152f * throughput.y + 0.0722f * throughput.z;
            float clamp_v = cfg->pt_clamp > 0.0f ? cfg->pt_clamp : 10.0f;
            if (thr_lum > clamp_v) {
                throughput = mul3f(throughput, clamp_v / thr_lum);
            }
        }
        sample_sum = add3(sample_sum, radiance);
    }

    float3 new_color = mul3f(sample_sum, 1.0f / (float)spp);

    // Progressive accumulation — write both accum and display HDR
    float3 old_color = make_f3(0, 0, 0);
    if (params.pt_accum) {
        old_color = make_f3(params.pt_accum[pixel].x, params.pt_accum[pixel].y, params.pt_accum[pixel].z);
    }
    float weight = 1.0f / (float)(cfg->pt_accumulated + 1u);
    float3 blended = mix3(old_color, new_color, weight);
    float4 out = make_float4(blended.x, blended.y, blended.z, 1.0f);
    if (params.pt_accum) {
        params.pt_accum[pixel] = out;
    }
    params.hdr_out[pixel] = out;
}
