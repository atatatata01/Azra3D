// ============================================================
// ssr.cu — Screen-space reflections (world-space ray march)
// Port from WGSL compute.wgsl fn pass_ssr
// ============================================================

#include "common.cuh"

extern "C" __global__ void ssr_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float4*      __restrict__ gbuf_normal,
    const float4*      __restrict__ gbuf_material,
    const float4*      __restrict__ hdr_buf,
    float4*            __restrict__ ssr_out,
    unsigned int width,
    unsigned int height
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;
    unsigned int pixel = gy * width + gx;

    float4 gn = gbuf_normal[pixel];
    float4 gm = gbuf_material[pixel];
    float3 normal = make_f3(gn.x, gn.y, gn.z);
    float depth = gn.w;
    float roughness = gm.x;
    float metallic = gm.y;

    // Skip background, rough surfaces, non-metallic
    if (depth <= 0.0f || roughness > 0.6f || metallic < 0.15f) {
        ssr_out[pixel] = make_float4(0, 0, 0, 0);
        return;
    }

    float w = (float)width, h = (float)height;
    float uv_x = ((float)gx / w) * 2.0f - 1.0f;
    float uv_y = -(((float)gy / h) * 2.0f - 1.0f);
    float aspect = w / h;
    CameraRay ray = camera_ray(cfg, uv_x, uv_y, aspect);
    float3 hit_pos = add3(ray.origin, mul3f(ray.dir, depth));
    float3 view_dir = normalize3(sub3(ray.origin, hit_pos));

    // Reflect
    float ndv2 = dot3(normal, view_dir);
    float3 reflect_dir = sub3(mul3f(normal, 2.0f * ndv2), view_dir);
    // Negate because reflect(-view, n)
    reflect_dir = sub3(mul3f(normal, 2.0f * fmaxf(dot3(normal, view_dir), 0.0f)), view_dir);

    // Fresnel
    float ndv = fmaxf(dot3(normal, view_dir), 0.0f);
    float f0 = 0.04f + (0.9f - 0.04f) * metallic;
    float fresnel = f0 + (1.0f - f0) * powf(1.0f - ndv, 5.0f);

    // Ray march
    const unsigned int max_steps = 32u;
    const float step_size = 0.4f;
    const float thickness = 0.35f;
    float3 ref_color = make_f3(0,0,0);
    float mask = 0.0f;
    float3 ray_pos = add3(hit_pos, mul3f(normal, 0.02f));
    float3 fwd = make_f3(cfg->cam_forward.x, cfg->cam_forward.y, cfg->cam_forward.z);
    float3 rgt = make_f3(cfg->cam_right.x, cfg->cam_right.y, cfg->cam_right.z);
    float3 up  = make_f3(cfg->cam_up.x, cfg->cam_up.y, cfg->cam_up.z);

    for (unsigned int i = 0; i < max_steps; i++) {
        ray_pos = add3(ray_pos, mul3f(reflect_dir, step_size));
        float3 rel = sub3(ray_pos, ray.origin);
        float z = dot3(rel, fwd);
        if (z <= 0.01f) break;

        float sx = dot3(rel, rgt) / (z * aspect);
        float sy = dot3(rel, up) / z;
        float screen_x = (sx + 1.0f) * 0.5f;
        float screen_y = (-sy + 1.0f) * 0.5f;
        if (screen_x < 0.0f || screen_x > 1.0f || screen_y < 0.0f || screen_y > 1.0f) break;

        int sample_x = (int)(screen_x * w);
        int sample_y = (int)(screen_y * h);
        sample_x = min(max(sample_x, 0), (int)width - 1);
        sample_y = min(max(sample_y, 0), (int)height - 1);
        float sample_depth = gbuf_normal[sample_y * width + sample_x].w;

        if (sample_depth > 0.0f && z > sample_depth && z < sample_depth + thickness) {
            float4 hdr_sample = hdr_buf[sample_y * width + sample_x];
            ref_color = make_f3(hdr_sample.x, hdr_sample.y, hdr_sample.z);
            float edge_fade = fminf(
                fminf(screen_x, 1.0f - screen_x),
                fminf(screen_y, 1.0f - screen_y)
            ) * 8.0f;
            float dist_fade = 1.0f - (float)i / (float)max_steps;
            float rough_fade = 1.0f - roughness / 0.6f;
            mask = __saturatef(edge_fade) * dist_fade * rough_fade * fresnel;
            break;
        }
    }

    ssr_out[pixel] = make_float4(ref_color.x, ref_color.y, ref_color.z, mask);
}
