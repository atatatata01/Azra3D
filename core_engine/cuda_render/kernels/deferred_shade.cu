// ============================================================
// deferred_shade.cu — Deferred PBR shading from G-buffer
// Port from WGSL compute.wgsl fn deferred_shade
// ============================================================

#include "materials.cuh"

extern "C" __global__ void deferred_shade_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float*       __restrict__ prim_data,
    const float4*      __restrict__ gbuf_normal,
    const float4*      __restrict__ gbuf_material,
    const uchar4*      __restrict__ gbuf_albedo,
    float4*            __restrict__ hdr_out,
    unsigned int width,
    unsigned int height
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;
    unsigned int pixel = gy * width + gx;

    float4 gn = gbuf_normal[pixel];
    float4 gm = gbuf_material[pixel];
    uchar4 ga = gbuf_albedo[pixel];

    float3 normal = make_f3(gn.x, gn.y, gn.z);
    float depth = gn.w;
    float mat_rough = gm.x;
    float mat_metal = gm.y;
    float shadow_ao = gm.z;
    float packed_opacity_em = gm.w;
    float mat_opacity = fminf(packed_opacity_em, 1.0f);
    float emission_add = fmaxf(packed_opacity_em - 1.0f, 0.0f);
    float3 albedo = make_f3(ga.x / 255.0f, ga.y / 255.0f, ga.z / 255.0f);
    float flags = ga.w / 255.0f;

    // Background
    if (flags < 0.1f) {
        float3 bg = bg_gradient(cfg, (float)gy, (float)height);
        hdr_out[pixel] = make_float4(bg.x, bg.y, bg.z, 1.0f);
        return;
    }

    // Floor (pre-shaded)
    if (flags < 0.6f) {
        hdr_out[pixel] = make_float4(albedo.x, albedo.y, albedo.z, 1.0f);
        return;
    }

    // VFX/Unlit
    bool is_vfx = (flags > 0.63f && flags < 0.73f) || (flags > 0.83f && flags < 0.93f);
    if (is_vfx) {
        float3 vfx_color = mul3f(albedo, fmaxf(emission_add, 1.0f));
        hdr_out[pixel] = make_float4(vfx_color.x, vfx_color.y, vfx_color.z, 1.0f);
        return;
    }

    // Geometry — reconstruct world position
    bool is_selected = flags > 0.9f;
    float w = (float)width, h = (float)height;
    float uv_x = ((float)gx / w) * 2.0f - 1.0f;
    float uv_y = -(((float)gy / h) * 2.0f - 1.0f);
    float aspect = w / h;
    CameraRay ray = camera_ray(cfg, uv_x, uv_y, aspect);
    float3 p = add3(ray.origin, mul3f(ray.dir, depth));
    float3 view_dir = normalize3(sub3(ray.origin, p));
    unsigned int quality = cfg->render_quality;

    float3 key_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));
    float key_intensity = cfg->light_dir.w;
    float3 key_color = make_f3(cfg->light_color.x, cfg->light_color.y, cfg->light_color.z);
    float ambient_base = cfg->light_color.w;
    float3 fill_dir = normalize3(make_f3(-key_dir.x*0.5f, fabsf(key_dir.y)*0.6f+0.3f, -key_dir.z*0.5f));

    float3 color = make_f3(0,0,0);

    // PBR — single directional light (multi-light deferred to later phase)
    float diffuse_weight = 1.0f - mat_metal * 0.7f;
    float a = mat_rough * mat_rough;
    float a2 = a * a;
    float spec_strength = 0.04f + (0.8f - 0.04f) * mat_metal;

    float ndl = fmaxf(dot3(normal, key_dir), 0.0f);
    float3 total_diffuse = mul3f(key_color, key_intensity * ndl * diffuse_weight * shadow_ao);

    // GGX specular
    float3 total_spec = make_f3(0,0,0);
    if (quality >= 2u) {
        float3 hv = normalize3(add3(view_dir, key_dir));
        float ndh = fmaxf(dot3(normal, hv), 0.0f);
        float d = ndh * ndh * (a2 - 1.0f) + 1.0f;
        float sv = (a2 / (3.14159f * d * d + 0.0001f)) * ndl * spec_strength * key_intensity;
        float3 sc = mix3(make_f3(1,1,1), albedo, mat_metal);
        total_spec = mul3f(mul3(sc, key_color), sv * shadow_ao);
    }

    // Fill light
    float fill_ndl = fmaxf(dot3(normal, fill_dir), 0.0f);
    total_diffuse = add3(total_diffuse, mul3f(key_color, key_intensity * fill_ndl * 0.2f * diffuse_weight * shadow_ao));

    float ambient = ambient_base * shadow_ao;
    color = add3(mul3(albedo, add3(total_diffuse, make_f3(ambient, ambient, ambient))), total_spec);

    // Selection rim
    if (is_selected) {
        float rim = powf(1.0f - fabsf(dot3(normal, view_dir)), 2.5f);
        color = mix3(color, make_f3(1.0f, 0.55f, 0.05f), rim * 0.9f + 0.06f);
    }

    // Alpha-blend for transparent objects
    if (mat_opacity < 0.999f) {
        float3 bg = bg_gradient(cfg, (float)gy, (float)height);
        color = mix3(bg, color, mat_opacity);
    }

    // Emission
    if (emission_add > 0.001f) {
        color = add3(color, mul3f(albedo, emission_add));
    }

    hdr_out[pixel] = make_float4(color.x, color.y, color.z, 1.0f);
}
