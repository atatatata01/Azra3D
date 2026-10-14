// ============================================================
// fog.cu — Volumetric fog with Mie scattering + SDF shadows
// Port from WGSL compute.wgsl fn pass_fog
// ============================================================

#include "sdf_eval.cuh"

// Mie phase function (dual-lobe)
__device__ __forceinline__ float mie_phase(float cos_theta, float g) {
    float g2 = g * g;
    float denom = 1.0f + g2 - 2.0f * g * cos_theta;
    float forward = (1.0f - g2) / (4.0f * 3.14159f * powf(fmaxf(denom, 0.0001f), 1.5f));
    float gb = -g * 0.3f;
    float gb2 = gb * gb;
    float back_denom = 1.0f + gb2 - 2.0f * gb * cos_theta;
    float back = (1.0f - gb2) / (4.0f * 3.14159f * powf(fmaxf(back_denom, 0.0001f), 1.5f));
    return forward * 0.75f + back * 0.25f;
}

// SDF volumetric shadow
__device__ float fog_shadow(const GpuUniforms* cfg, const float* pd, float3 p, float3 ld, unsigned int steps) {
    if (steps == 0u || cfg->num_primitives == 0u) return 1.0f;
    float max_t = 15.0f;
    float t = 0.2f;
    float shadow = 1.0f;
    for (unsigned int i = 0; i < steps; i++) {
        float3 sp = add3(p, mul3f(ld, t));
        float d = scene_analytical(cfg, pd, sp);
        shadow = fminf(shadow, gpu_smoothstep(0.0f, 0.3f, d));
        if (shadow < 0.01f) break;
        t += fmaxf(d, max_t / (float)steps);
        if (t > max_t) break;
    }
    return shadow;
}

extern "C" __global__ void fog_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float*       __restrict__ prim_data,
    const float4*      __restrict__ gbuf_normal,
    float4*            __restrict__ fog_out,
    unsigned int width,
    unsigned int height
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;
    unsigned int pixel = gy * width + gx;

    float depth = gbuf_normal[pixel].w;
    float w = (float)width, h = (float)height;
    float uv_x = ((float)gx / w) * 2.0f - 1.0f;
    float uv_y = -(((float)gy / h) * 2.0f - 1.0f);
    float aspect = w / h;
    CameraRay ray = camera_ray(cfg, uv_x, uv_y, aspect);

    float max_fog_dist = fminf(cfg->fog_max_distance, cfg->far_clip);
    float max_dist = depth > 0.0f ? fminf(depth, max_fog_dist) : max_fog_dist;
    float march_start = fmaxf(cfg->fog_start, 0.1f);
    if (max_dist <= march_start) {
        fog_out[pixel] = make_float4(0, 0, 0, 1.0f);
        return;
    }

    unsigned int num_steps, noise_octaves;
    switch (cfg->render_quality) {
        case 0u: num_steps = 12u; noise_octaves = 2u; break;
        case 1u: num_steps = 32u; noise_octaves = 3u; break;
        case 2u: num_steps = 64u; noise_octaves = 4u; break;
        default: num_steps = 96u; noise_octaves = 5u; break;
    }
    unsigned int shadow_steps = (unsigned int)cfg->fog_shadow_steps;

    float3 light_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));
    float3 light_color = make_f3(cfg->light_color.x, cfg->light_color.y, cfg->light_color.z);
    float light_intensity = cfg->light_dir.w;
    float cos_angle = dot3(ray.dir, light_dir);
    float phase = mie_phase(cos_angle, cfg->fog_phase_g);

    // Stochastic jitter
    float jitter = hash31(make_f3((float)gx, (float)gy, (float)(gx ^ gy) * 0.137f));
    float step_size = (max_dist - march_start) / (float)num_steps;

    float3 accumulated = make_f3(0,0,0);
    float transmittance = 1.0f;
    float3 fog_ambient = make_f3(0.35f, 0.4f, 0.5f);

    for (unsigned int i = 0; i < num_steps; i++) {
        float t = march_start + ((float)i + jitter) * step_size;
        float3 p = add3(ray.origin, mul3f(ray.dir, t));

        float height_factor = expf(-fmaxf(0.0f, p.y - cfg->floor_y) * cfg->fog_height_falloff);

        float noise_mod = 1.0f;
        if (cfg->fog_noise_strength > 0.01f) {
            float n = fbm3(mul3f(p, cfg->fog_noise_scale), noise_octaves);
            noise_mod = 1.0f + (n - 1.0f) * cfg->fog_noise_strength;
            noise_mod = fmaxf(noise_mod, 0.0f);
        }

        float local_density = cfg->fog_density * height_factor * noise_mod * step_size;
        float extinction = local_density * (1.0f + cfg->fog_absorption);
        float scattering = local_density;

        float shadow = fog_shadow(cfg, prim_data, p, light_dir, shadow_steps);

        float3 in_scatter = mul3f(
            add3(mul3f(light_color, light_intensity * phase * cfg->fog_scatter * shadow),
                 mul3f(fog_ambient, 0.3f)),
            scattering);

        accumulated = add3(accumulated, mul3f(in_scatter, transmittance));
        transmittance *= expf(-extinction);
        if (transmittance < 0.005f) break;
    }

    fog_out[pixel] = make_float4(accumulated.x, accumulated.y, accumulated.z, transmittance);
}
