// ============================================================
// raymarch_gbuf.cu — Main G-buffer raymarching kernel
// Direct port from WGSL compute.wgsl @compute fn raymarch_gbuf
// ============================================================

#include "materials.cuh"

// G-buffer output layout (per pixel):
//   gbuf_normal:   float4 (xyz=normal, w=depth_t)
//   gbuf_material: float4 (roughness, metallic, shadow*ao, opacity+emission_lum)
//   gbuf_albedo:   uchar4 (rgb=albedo/emission-tinted, a=flags*255)
//   flags: 0.0=bg, 0.5=floor, 0.68=VFX, 0.78=geo, 0.88=VFX+sel, 1.0=geo+sel

extern "C" __global__ void raymarch_gbuf_kernel(
    const GpuUniforms* __restrict__ cfg,
    const float*       __restrict__ prim_data,
    float4*            __restrict__ gbuf_normal,
    float4*            __restrict__ gbuf_material,
    uchar4*            __restrict__ gbuf_albedo,
    unsigned int width,
    unsigned int height
) {
    unsigned int gx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned int gy = blockIdx.y * blockDim.y + threadIdx.y;
    if (gx >= width || gy >= height) return;

    unsigned int pixel = gy * width + gx;

    float x = (float)gx, y = (float)gy;
    float w = (float)width, h = (float)height;
    float uv_x = (x / w) * 2.0f - 1.0f;
    float uv_y = -((y / h) * 2.0f - 1.0f);
    float aspect = w / h;

    CameraRay ray = camera_ray(cfg, uv_x, uv_y, aspect);

    unsigned int quality = cfg->render_quality;
    unsigned int steps;
    if (quality >= 4u) steps = 256u;
    else if (quality >= 3u) steps = 200u;
    else if (quality >= 2u) steps = cfg->max_steps;
    else if (quality >= 1u) steps = 80u;
    else steps = 48u;

    float step_mult = quality >= 3u ? 0.5f : (quality >= 2u ? 0.6f : 0.7f);

    // Sphere trace
    float t = 0.0f;
    bool hit = false;
    float3 p = make_f3(0,0,0);

    for (unsigned int i = 0; i < steps; i++) {
        p = add3(ray.origin, mul3f(ray.dir, t));
        float d = scene_sdf(cfg, prim_data, p);
        if (d < cfg->hit_threshold) { hit = true; break; }
        t += d * step_mult;
        if (t > cfg->far_clip) break;
    }

    float3 key_dir = normalize3(make_f3(cfg->light_dir.x, cfg->light_dir.y, cfg->light_dir.z));

    if (hit) {
        float3 normal = calc_normal(cfg, prim_data, p);
        bool is_selected = false;
        unsigned int hit_idx = 0u;
        float3 mat_color = make_f3(0.66f, 0.585f, 0.53f);
        float mat_rough = 0.65f;
        float mat_metal = 0.0f;
        float mat_opacity = 1.0f;
        float3 mat_emission = make_f3(0,0,0);

        if (cfg->num_primitives > 0u) {
            hit_idx = find_hit_prim(cfg, prim_data, p);
            is_selected = prim_is_selected(prim_data, hit_idx);
            mat_color = prim_albedo(prim_data, hit_idx);

            if (cfg->view_mode == 0u) {
                MatResult mat = eval_material(cfg, prim_data, hit_idx, p, normal);
                mat_color = mat.color;
                mat_rough = mat.roughness;
                mat_metal = mat.metallic;
                mat_opacity = mat.opacity;
                mat_emission = mat.emission;
            }
        }

        // Shadow + AO
        float shadow = 1.0f;
        if (quality >= 1u) {
            float shadow_k = quality >= 3u ? 16.0f : (quality >= 2u ? 12.0f : 8.0f);
            shadow = soft_shadow(cfg, prim_data, add3(p, mul3f(normal, 0.01f)), key_dir, 0.02f, 8.0f, shadow_k);
        }
        float ao = 1.0f;
        if (quality >= 1u) ao = calc_ao(cfg, prim_data, p, normal);

        // Flags encoding
        bool is_vfx = cfg->num_primitives > 0u && prim_shader_type(prim_data, hit_idx) == 3u;
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
        gbuf_normal[pixel] = make_float4(normal.x, normal.y, normal.z, t);
        gbuf_material[pixel] = make_float4(mat_rough, mat_metal, shadow * ao, mat_opacity + em_lum);
        gbuf_albedo[pixel] = make_uchar4(
            (unsigned char)__saturatef(stored_color.x) * 255,
            (unsigned char)__saturatef(stored_color.y) * 255,
            (unsigned char)__saturatef(stored_color.z) * 255,
            (unsigned char)(flags * 255.0f)
        );

    } else if (ray.dir.y < -0.001f) {
        // Floor plane
        float t_plane = -(ray.origin.y - cfg->floor_y) / ray.dir.y;
        if (t_plane > 0.0f && t_plane < cfg->far_clip) {
            float3 p_plane = add3(ray.origin, mul3f(ray.dir, t_plane));
            float fade = fmaxf(0.0f, 1.0f - t_plane / cfg->fade_dist);
            if (fade > 0.0f) {
                float gs = 1.0f;
                if (quality >= 2u && cfg->num_primitives > 0u) {
                    gs = 0.3f + 0.7f * soft_shadow(cfg, prim_data, add3(p_plane, make_f3(0, 0.01f, 0)), key_dir, 0.05f, 8.0f, 16.0f);
                    float dist_fade = gpu_smoothstep(cfg->fade_dist * 0.7f, cfg->fade_dist, t_plane);
                    gs = gs + (1.0f - gs) * dist_fade;
                }

                float3 bg = bg_gradient(cfg, y, h);
                float3 grid_col = mul3f(bg, gs);

                // Floor grid lines
                if (cfg->line_thickness > 0.0f) {
                    float cell = cfg->cell_size;
                    float pixel_size = t_plane * 2.0f / h;
                    float thick = fmaxf(cfg->line_thickness, pixel_size * 1.5f);
                    float gx_d = fmodf(fabsf(p_plane.x), cell);
                    gx_d = fminf(gx_d, cell - gx_d);
                    float gz_d = fmodf(fabsf(p_plane.z), cell);
                    gz_d = fminf(gz_d, cell - gz_d);
                    float line_x = 1.0f - gpu_smoothstep(0.0f, thick, gx_d);
                    float line_z = 1.0f - gpu_smoothstep(0.0f, thick, gz_d);
                    float grid_strength = fmaxf(line_x, line_z) * fade;
                    float3 grid_tint = mul3f(make_f3(1,1,1), 0.08f * fade);
                    grid_col = mix3(grid_col, mul3f(add3(bg, grid_tint), gs), grid_strength);
                }

                gbuf_normal[pixel] = make_float4(0, 1, 0, t_plane);
                gbuf_material[pixel] = make_float4(0, 0, 0, 0);
                gbuf_albedo[pixel] = make_uchar4(
                    (unsigned char)(__saturatef(grid_col.x) * 255.0f),
                    (unsigned char)(__saturatef(grid_col.y) * 255.0f),
                    (unsigned char)(__saturatef(grid_col.z) * 255.0f),
                    (unsigned char)(0.5f * 255.0f) // floor flag
                );
                return;
            }
        }
        // Background
        float3 bg = bg_gradient(cfg, y, h);
        gbuf_normal[pixel] = make_float4(0, 0, 0, 0);
        gbuf_material[pixel] = make_float4(0, 0, 0, 0);
        gbuf_albedo[pixel] = make_uchar4(
            (unsigned char)(__saturatef(bg.x) * 255.0f),
            (unsigned char)(__saturatef(bg.y) * 255.0f),
            (unsigned char)(__saturatef(bg.z) * 255.0f),
            0 // bg flag
        );
    } else {
        // Background (sky)
        float3 bg = bg_gradient(cfg, y, h);
        gbuf_normal[pixel] = make_float4(0, 0, 0, 0);
        gbuf_material[pixel] = make_float4(0, 0, 0, 0);
        gbuf_albedo[pixel] = make_uchar4(
            (unsigned char)(__saturatef(bg.x) * 255.0f),
            (unsigned char)(__saturatef(bg.y) * 255.0f),
            (unsigned char)(__saturatef(bg.z) * 255.0f),
            0
        );
    }
}
