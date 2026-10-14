// ============================================================
// pathtrace.cu — Progressive Monte Carlo path tracer
// Port from WGSL compute.wgsl fn pass_pathtrace
// ============================================================

#include "materials.cuh"

// Cosine-weighted hemisphere sampling
__device__ __forceinline__ float3 cosine_hemisphere(float3 n, unsigned int* seed) {
    float r1 = rand_float(seed);
    float r2 = rand_float(seed);
    float phi = 2.0f * 3.14159f * r1;
    float cos_theta = sqrtf(1.0f - r2);
    float sin_theta = sqrtf(r2);

    // Build tangent frame
    float3 tang;
    if (fabsf(n.y) < 0.999f) tang = normalize3(cross3(make_f3(0,1,0), n));
    else tang = normalize3(cross3(make_f3(1,0,0), n));
    float3 bitan = cross3(n, tang);

    return normalize3(add3(add3(
        mul3f(tang, cosf(phi) * sin_theta),
        mul3f(bitan, sinf(phi) * sin_theta)),
        mul3f(n, cos_theta)));
}

extern "C" __global__ void pathtrace_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float*       __restrict__ prim_data,
    const float4*      __restrict__ pt_accum_in,
    float4*            __restrict__ hdr_out,
    unsigned int width,
    unsigned int height
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;
    unsigned int pixel = gy * width + gx;

    float w = (float)width, h = (float)height;
    float aspect = w / h;

    // Seed RNG
    unsigned int seed = pcg_hash(gx + gy * width + cfg->pt_frame_seed * width * height);

    // Focus-adaptive SPP
    float2 pixel_uv = make_float2((float)gx / w, (float)gy / h);
    unsigned int spp = cfg->pt_samples_per_frame > 0 ? cfg->pt_samples_per_frame : 1u;
    if (cfg->pt_focus_uv_x >= 0.0f) {
        float fd = length2(make_float2(pixel_uv.x - cfg->pt_focus_uv_x, pixel_uv.y - cfg->pt_focus_uv_y));
        if (fd < cfg->pt_focus_radius) spp *= 4u;
    }

    float3 sample_sum = make_f3(0,0,0);

    for (unsigned int s = 0; s < spp; s++) {
        float jx = rand_float(&seed) - 0.5f;
        float jy = rand_float(&seed) - 0.5f;
        float uv_x = (((float)gx + jx) / w) * 2.0f - 1.0f;
        float uv_y = -((((float)gy + jy) / h) * 2.0f - 1.0f);

        CameraRay cam = camera_ray(cfg, uv_x, uv_y, aspect);
        float3 ray_o = cam.origin;
        float3 ray_d = cam.dir;
        float3 throughput = make_f3(1,1,1);
        float3 radiance = make_f3(0,0,0);

        for (unsigned int bounce = 0; bounce <= cfg->pt_max_bounces; bounce++) {
            float2 rr = trace_ray(cfg, prim_data, ray_o, ray_d, cfg->far_clip);
            if (rr.y < 0.5f) {
                // Sky
                float sky_t = ray_d.y * 0.5f + 0.5f;
                float3 sky_top = make_f3(
                    (float)((cfg->bg_top_packed >> 16u) & 0xFF) / 255.0f,
                    (float)((cfg->bg_top_packed >> 8u) & 0xFF) / 255.0f,
                    (float)(cfg->bg_top_packed & 0xFF) / 255.0f);
                float3 sky_bot = make_f3(
                    (float)((cfg->bg_bottom_packed >> 16u) & 0xFF) / 255.0f,
                    (float)((cfg->bg_bottom_packed >> 8u) & 0xFF) / 255.0f,
                    (float)(cfg->bg_bottom_packed & 0xFF) / 255.0f);
                radiance = add3(radiance, mul3(throughput, mul3f(mix3(sky_bot, sky_top, sky_t), 0.5f)));
                break;
            }

            float3 hit_p = add3(ray_o, mul3f(ray_d, rr.x));
            float3 n = calc_normal(cfg, prim_data, hit_p);

            // Material
            float3 alb = make_f3(0.66f, 0.585f, 0.53f);
            float rough = 0.65f, metal = 0.0f;
            if (cfg->num_primitives > 0u) {
                unsigned int hp = find_hit_prim(cfg, prim_data, hit_p);
                MatResult mat = eval_material(cfg, prim_data, hp, hit_p, n);
                alb = mat.color; rough = mat.roughness; metal = mat.metallic;
                if (length3(mat.emission) > 0.001f) {
                    radiance = add3(radiance, mul3(throughput, mat.emission));
                }
            }

            // NEE: direct light sampling (primary directional light)
            float3 l_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));
            float l_int = cfg->light_dir.w;
            float3 l_col = make_f3(cfg->light_color.x, cfg->light_color.y, cfg->light_color.z);
            float ndl = fmaxf(dot3(n, l_dir), 0.0f);
            if (ndl > 0.0f) {
                float shd = soft_shadow(cfg, prim_data, add3(hit_p, mul3f(n, 0.01f)), l_dir, 0.02f, 8.0f, 16.0f);
                float dw = 1.0f - metal * 0.7f;
                radiance = add3(radiance, mul3(throughput, mul3f(mul3(alb, l_col), l_int * ndl * dw * shd)));
                // Specular direct
                float3 h_s = normalize3(sub3(l_dir, ray_d)); // -ray_d + l_dir
                float ndh_s = fmaxf(dot3(n, h_s), 0.0f);
                float a_s = rough * rough;
                float a2_s = a_s * a_s;
                float d_s = ndh_s * ndh_s * (a2_s - 1.0f) + 1.0f;
                float spec_v = (a2_s / (3.14159f * d_s * d_s + 0.0001f)) * ndl * (0.04f + 0.76f * metal) * l_int;
                float3 spec_col = mix3(make_f3(1,1,1), alb, metal);
                radiance = add3(radiance, mul3(throughput, mul3f(mul3(spec_col, l_col), spec_v * shd)));
            }

            // Ambient on first bounce
            if (bounce == 0u) {
                radiance = add3(radiance, mul3(throughput, mul3f(alb, cfg->light_color.w * 0.3f)));
            }

            // Russian Roulette after bounce 2
            if (bounce >= 2u) {
                float rr_p = fmaxf(throughput.x, fmaxf(throughput.y, throughput.z));
                if (rand_float(&seed) > rr_p) break;
                throughput = mul3f(throughput, 1.0f / rr_p);
            }

            // Sample next bounce
            float is_spec_r = rand_float(&seed);
            float spec_prob = 0.04f + 0.86f * metal;
            if (is_spec_r < spec_prob) {
                // Specular reflection
                float ndrd = dot3(n, ray_d);
                float3 refl = sub3(ray_d, mul3f(n, 2.0f * ndrd));
                float px = (rand_float(&seed) - 0.5f) * rough;
                float py = (rand_float(&seed) - 0.5f) * rough;
                float3 tang;
                if (fabsf(n.y) < 0.999f) tang = normalize3(cross3(make_f3(0,1,0), n));
                else tang = normalize3(cross3(make_f3(1,0,0), n));
                float3 bitan = cross3(n, tang);
                ray_d = normalize3(add3(refl, add3(mul3f(tang, px), mul3f(bitan, py))));
                if (dot3(ray_d, n) < 0.0f) ray_d = refl;
                throughput = mul3(throughput, mix3(make_f3(1,1,1), alb, metal));
            } else {
                ray_d = cosine_hemisphere(n, &seed);
                throughput = mul3(throughput, alb);
            }
            ray_o = add3(hit_p, mul3f(n, 0.02f));
        }
        sample_sum = add3(sample_sum, radiance);
    }

    float3 new_color = mul3f(sample_sum, 1.0f / (float)spp);

    // Progressive accumulation
    float3 old_color = make_f3(pt_accum_in[pixel].x, pt_accum_in[pixel].y, pt_accum_in[pixel].z);
    float weight = 1.0f / (float)(cfg->pt_accumulated + 1u);
    float3 blended = mix3(old_color, new_color, weight);

    hdr_out[pixel] = make_float4(blended.x, blended.y, blended.z, 1.0f);
}
