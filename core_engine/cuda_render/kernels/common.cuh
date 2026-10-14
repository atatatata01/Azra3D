#pragma once
// ============================================================
// common.cuh — Shared types, GPU uniforms, prim accessors, math helpers
// Matches the WGSL compute.wgsl layout byte-for-byte.
// ============================================================

#include <cuda_runtime.h>
#include <math.h>

#ifndef FLOATS_PER_PRIM
#define FLOATS_PER_PRIM 108
#endif

// ---- GpuUniforms (320 bytes, matches Rust GpuUniforms #[repr(C)]) ----
struct GpuUniforms {
    float4 cam_pos;          // xyz=position, w>0 = ortho half-width
    float4 cam_forward;      // xyz=forward dir
    float4 cam_right;        // xyz=right dir
    float4 cam_up;           // xyz=up dir
    unsigned int max_steps;  // 64
    float far_clip;          // 68
    float hit_threshold;     // 72
    float normal_epsilon;    // 76
    float floor_y;           // 80
    float fade_dist;         // 84
    float cell_size;         // 88
    float line_thickness;    // 92
    unsigned int grid_res;   // 96
    float grid_bounds;       // 100
    unsigned int num_primitives; // 104
    unsigned int render_mode;    // 108
    unsigned int view_mode;      // 112
    unsigned int render_quality; // 116
    unsigned int tex_size;   // 120
    unsigned int tex_tiling; // 124
    float4 light_dir;        // 128: xyz=dir, w=intensity
    float4 light_color;      // 144: rgb=color, w=ambient
    float exposure;          // 160
    float bloom_threshold;   // 164
    float bloom_intensity;   // 168
    unsigned int post_flags; // 172
    float fog_density;       // 176
    float fog_height_falloff;// 180
    float fog_start;         // 184
    float fog_scatter;       // 188
    unsigned int env_width;  // 192
    unsigned int env_height; // 196
    unsigned int bg_top_packed;    // 200
    unsigned int bg_bottom_packed; // 204
    float env_brightness;    // 208
    float env_contrast;      // 212
    float env_saturation;    // 216
    float env_blur;          // 220
    float fog_phase_g;       // 224
    float fog_absorption;    // 228
    float fog_noise_scale;   // 232
    float fog_noise_strength;// 236
    float fog_max_distance;  // 240
    float fog_shadow_steps;  // 244
    float fog_temporal_blend;// 248
    unsigned int fog_frame_index; // 252
    unsigned int rt_bounces;      // 256
    unsigned int rt_shadow_samples; // 260
    unsigned int pt_max_bounces;    // 264
    unsigned int pt_samples_per_frame; // 268
    unsigned int pt_accumulated;       // 272
    unsigned int pt_frame_seed;        // 276
    float pt_focus_uv_x;     // 280
    float pt_focus_uv_y;     // 284
    float pt_focus_radius;   // 288
    float pt_focus_falloff;  // 292
    unsigned int rt_env_samples; // 296
    unsigned int rt_caustics;    // 300
    float pt_dof_aperture;   // 304
    unsigned int pt_denoise; // 308
    float pt_clamp;          // 312
    unsigned int _rtpad6;    // 316
};

// ---- Math helpers ----

__device__ __forceinline__ float3 make_f3(float x, float y, float z) { return make_float3(x, y, z); }
__device__ __forceinline__ float4 make_f4(float x, float y, float z, float w) { return make_float4(x, y, z, w); }

__device__ __forceinline__ float dot3(float3 a, float3 b) { return a.x*b.x + a.y*b.y + a.z*b.z; }
__device__ __forceinline__ float length3(float3 v) { return sqrtf(dot3(v, v)); }
__device__ __forceinline__ float3 normalize3(float3 v) { float il = rsqrtf(fmaxf(dot3(v,v), 1e-12f)); return make_f3(v.x*il, v.y*il, v.z*il); }
__device__ __forceinline__ float3 abs3(float3 v) { return make_f3(fabsf(v.x), fabsf(v.y), fabsf(v.z)); }
__device__ __forceinline__ float3 max3(float3 a, float3 b) { return make_f3(fmaxf(a.x,b.x), fmaxf(a.y,b.y), fmaxf(a.z,b.z)); }
__device__ __forceinline__ float3 min3f(float3 a, float3 b) { return make_f3(fminf(a.x,b.x), fminf(a.y,b.y), fminf(a.z,b.z)); }
__device__ __forceinline__ float3 sub3(float3 a, float3 b) { return make_f3(a.x-b.x, a.y-b.y, a.z-b.z); }
__device__ __forceinline__ float3 add3(float3 a, float3 b) { return make_f3(a.x+b.x, a.y+b.y, a.z+b.z); }
__device__ __forceinline__ float3 mul3f(float3 v, float s) { return make_f3(v.x*s, v.y*s, v.z*s); }
__device__ __forceinline__ float3 div3(float3 a, float3 b) { return make_f3(a.x/b.x, a.y/b.y, a.z/b.z); }
__device__ __forceinline__ float3 mul3(float3 a, float3 b) { return make_f3(a.x*b.x, a.y*b.y, a.z*b.z); }
__device__ __forceinline__ float3 cross3(float3 a, float3 b) { return make_f3(a.y*b.z-a.z*b.y, a.z*b.x-a.x*b.z, a.x*b.y-a.y*b.x); }
__device__ __forceinline__ float2 max2(float2 a, float2 b) { return make_float2(fmaxf(a.x,b.x), fmaxf(a.y,b.y)); }
__device__ __forceinline__ float length2(float2 v) { return sqrtf(v.x*v.x + v.y*v.y); }
__device__ __forceinline__ float3 mix3(float3 a, float3 b, float t) { return make_f3(a.x+(b.x-a.x)*t, a.y+(b.y-a.y)*t, a.z+(b.z-a.z)*t); }

__device__ __forceinline__ float gpu_smoothstep(float e0, float e1, float x) {
    float t = __saturatef((x - e0) / (e1 - e0));
    return t * t * (3.0f - 2.0f * t);
}

__device__ __forceinline__ float smin(float a, float b, float k) {
    float h = __saturatef(0.5f + 0.5f * (a - b) / k);
    return a + (b - a) * h - k * h * (1.0f - h);
}

__device__ __forceinline__ float smax(float a, float b, float k) {
    float h = __saturatef(0.5f - 0.5f * (a - b) / k);
    return a + (b - a) * h + k * h * (1.0f - h);
}

__device__ __forceinline__ float chamfer_min(float a, float b, float k) {
    return fminf(fminf(a, b), (a - k + b) * 0.7071067811865476f);
}

// ---- Quaternion inverse rotation ----
__device__ __forceinline__ float3 quat_inv_rotate(float4 q, float3 v) {
    float3 qi = make_f3(-q.x, -q.y, -q.z);
    float3 t = mul3f(cross3(qi, v), 2.0f);
    return add3(add3(v, mul3f(t, q.w)), cross3(qi, t));
}

// ---- PCG hash for RNG ----
__device__ __forceinline__ unsigned int pcg_hash(unsigned int input) {
    unsigned int state = input * 747796405u + 2891336453u;
    unsigned int word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

__device__ __forceinline__ float rand_float(unsigned int* seed) {
    *seed = pcg_hash(*seed);
    return (float)(*seed) / 4294967296.0f;
}

// ---- Noise functions ----
__device__ __forceinline__ float hash31(float3 p) {
    float3 p3 = make_f3(
        p.x * 0.1031f - floorf(p.x * 0.1031f),
        p.y * 0.1031f - floorf(p.y * 0.1031f),
        p.z * 0.1031f - floorf(p.z * 0.1031f));
    float d = p3.x*(p3.y+33.33f) + p3.y*(p3.z+33.33f) + p3.z*(p3.x+33.33f);
    p3 = make_f3(p3.x+d, p3.y+d, p3.z+d);
    float v = (p3.x + p3.y) * p3.z;
    return v - floorf(v);
}

__device__ __forceinline__ float noise3(float3 p) {
    float3 i = make_f3(floorf(p.x), floorf(p.y), floorf(p.z));
    float3 f = make_f3(p.x-i.x, p.y-i.y, p.z-i.z);
    float3 u = make_f3(f.x*f.x*(3.0f-2.0f*f.x), f.y*f.y*(3.0f-2.0f*f.y), f.z*f.z*(3.0f-2.0f*f.z));
    float n000 = hash31(i);
    float n100 = hash31(add3(i, make_f3(1,0,0)));
    float n010 = hash31(add3(i, make_f3(0,1,0)));
    float n110 = hash31(add3(i, make_f3(1,1,0)));
    float n001 = hash31(add3(i, make_f3(0,0,1)));
    float n101 = hash31(add3(i, make_f3(1,0,1)));
    float n011 = hash31(add3(i, make_f3(0,1,1)));
    float n111 = hash31(add3(i, make_f3(1,1,1)));
    float x0 = n000 + (n100 - n000)*u.x;
    float x1 = n010 + (n110 - n010)*u.x;
    float x2 = n001 + (n101 - n001)*u.x;
    float x3 = n011 + (n111 - n011)*u.x;
    float y0 = x0 + (x1 - x0)*u.y;
    float y1 = x2 + (x3 - x2)*u.y;
    return y0 + (y1 - y0)*u.z;
}

__device__ float fbm3(float3 p, unsigned int octaves) {
    float sum = 0.0f, amp = 0.5f;
    unsigned int max_oct = octaves < 6u ? octaves : 6u;
    for (unsigned int i = 0; i < max_oct; i++) {
        sum += amp * noise3(p);
        p = make_f3(p.x*2.01f, p.y*2.01f, p.z*2.01f);
        amp *= 0.5f;
    }
    return sum;
}

// ---- Primitive buffer accessors ----
// prim_data: flat f32 buffer, FLOATS_PER_PRIM stride per primitive
__device__ __forceinline__ float prim_f(const float* prim_data, unsigned int idx, unsigned int field) {
    return prim_data[idx * FLOATS_PER_PRIM + field];
}
__device__ __forceinline__ unsigned int prim_u(const float* prim_data, unsigned int idx, unsigned int field) {
    return __float_as_uint(prim_data[idx * FLOATS_PER_PRIM + field]);
}

__device__ __forceinline__ float3 prim_pos(const float* pd, unsigned int i) {
    return make_f3(prim_f(pd,i,0), prim_f(pd,i,1), prim_f(pd,i,2));
}
__device__ __forceinline__ float4 prim_quat(const float* pd, unsigned int i) {
    return make_f4(prim_f(pd,i,4), prim_f(pd,i,5), prim_f(pd,i,6), prim_f(pd,i,7));
}
__device__ __forceinline__ float3 prim_scale(const float* pd, unsigned int i) {
    return make_f3(prim_f(pd,i,8), prim_f(pd,i,9), prim_f(pd,i,10));
}
__device__ __forceinline__ unsigned int prim_shape_type(const float* pd, unsigned int i) {
    return prim_u(pd, i, 3) & 0xFFu;
}
__device__ __forceinline__ bool prim_is_selected(const float* pd, unsigned int i) {
    return (prim_u(pd, i, 3) & 256u) != 0u;
}
__device__ __forceinline__ unsigned int prim_stair_steps(const float* pd, unsigned int i) {
    unsigned int v = (prim_u(pd, i, 3) >> 16u) & 0xFFFFu;
    return v > 0 ? v : 1u;
}
__device__ __forceinline__ float prim_bound_r(const float* pd, unsigned int i) {
    return prim_f(pd, i, 45);
}
__device__ __forceinline__ unsigned int prim_mirror_bits(const float* pd, unsigned int i) {
    return prim_u(pd, i, 61);
}
__device__ __forceinline__ float3 prim_mirror_center(const float* pd, unsigned int i) {
    return make_f3(prim_f(pd,i,78), prim_f(pd,i,79), prim_f(pd,i,80));
}
__device__ __forceinline__ float prim_bev_edge_r(const float* pd, unsigned int i, unsigned int edge) {
    return prim_f(pd, i, 48 + edge);
}
__device__ __forceinline__ unsigned int prim_bev_modes(const float* pd, unsigned int i) {
    return prim_u(pd, i, 60);
}
// Modifier accessors
__device__ __forceinline__ unsigned int prim_mod_type(const float* pd, unsigned int i) { return prim_u(pd, i, 64); }
__device__ __forceinline__ float prim_mod_strength(const float* pd, unsigned int i) { return prim_f(pd, i, 65); }
__device__ __forceinline__ unsigned int prim_mod_axis(const float* pd, unsigned int i) { return prim_u(pd, i, 66); }
__device__ __forceinline__ float prim_mod_param2(const float* pd, unsigned int i) { return prim_f(pd, i, 67); }
// Volumetric mask accessors
__device__ __forceinline__ unsigned int prim_mask_type(const float* pd, unsigned int i) { return prim_u(pd, i, 68); }
__device__ __forceinline__ float3 prim_mask_center(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,69),prim_f(pd,i,70),prim_f(pd,i,71)); }
__device__ __forceinline__ float3 prim_mask_extents(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,72),prim_f(pd,i,73),prim_f(pd,i,74)); }
__device__ __forceinline__ float prim_mask_feather(const float* pd, unsigned int i) { return prim_f(pd, i, 75); }
__device__ __forceinline__ unsigned int prim_mask_invert(const float* pd, unsigned int i) { return prim_u(pd, i, 76); }
// Material accessors
__device__ __forceinline__ float3 prim_albedo(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,16),prim_f(pd,i,17),prim_f(pd,i,18)); }
__device__ __forceinline__ float prim_base_rough(const float* pd, unsigned int i) { return prim_f(pd, i, 19); }
__device__ __forceinline__ float prim_base_metal(const float* pd, unsigned int i) { return prim_f(pd, i, 20); }
__device__ __forceinline__ float prim_base_opacity(const float* pd, unsigned int i) { return prim_f(pd, i, 77); }
__device__ __forceinline__ float prim_ew_amount(const float* pd, unsigned int i) { return prim_f(pd,i,21); }
__device__ __forceinline__ float3 prim_ew_color(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,22),prim_f(pd,i,23),prim_f(pd,i,24)); }
__device__ __forceinline__ float prim_ew_rough(const float* pd, unsigned int i) { return prim_f(pd,i,25); }
__device__ __forceinline__ float prim_ew_metal(const float* pd, unsigned int i) { return prim_f(pd,i,26); }
__device__ __forceinline__ float prim_ew_sharp(const float* pd, unsigned int i) { return prim_f(pd,i,27); }
__device__ __forceinline__ float prim_cd_amount(const float* pd, unsigned int i) { return prim_f(pd,i,28); }
__device__ __forceinline__ float3 prim_cd_color(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,29),prim_f(pd,i,30),prim_f(pd,i,31)); }
__device__ __forceinline__ float prim_cd_rough(const float* pd, unsigned int i) { return prim_f(pd,i,32); }
__device__ __forceinline__ float prim_cd_metal(const float* pd, unsigned int i) { return prim_f(pd,i,33); }
__device__ __forceinline__ float prim_cd_sharp(const float* pd, unsigned int i) { return prim_f(pd,i,34); }
__device__ __forceinline__ float prim_np_amount(const float* pd, unsigned int i) { return prim_f(pd,i,35); }
__device__ __forceinline__ float3 prim_np_color(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,36),prim_f(pd,i,37),prim_f(pd,i,38)); }
__device__ __forceinline__ float prim_np_rough(const float* pd, unsigned int i) { return prim_f(pd,i,39); }
__device__ __forceinline__ float prim_np_metal(const float* pd, unsigned int i) { return prim_f(pd,i,40); }
__device__ __forceinline__ float prim_np_scale(const float* pd, unsigned int i) { return prim_f(pd,i,41); }
__device__ __forceinline__ unsigned int prim_np_oct(const float* pd, unsigned int i) { return (unsigned int)prim_f(pd,i,42); }
// Blend falloff
__device__ __forceinline__ float3 prim_falloff_packed(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,46),prim_f(pd,i,47),prim_f(pd,i,62)); }
__device__ __forceinline__ float prim_falloff_offset(const float* pd, unsigned int i) { return prim_f(pd,i,63); }
// Shader panel
__device__ __forceinline__ unsigned int prim_shader_type(const float* pd, unsigned int i) { return prim_u(pd,i,84); }
__device__ __forceinline__ unsigned int prim_mat_quality(const float* pd, unsigned int i) { return prim_u(pd,i,85); }
__device__ __forceinline__ float3 prim_emission_color(const float* pd, unsigned int i) { return make_f3(prim_f(pd,i,87),prim_f(pd,i,88),prim_f(pd,i,89)); }
__device__ __forceinline__ float prim_emission_intensity(const float* pd, unsigned int i) { return prim_f(pd,i,90); }
__device__ __forceinline__ unsigned int prim_subdivision(const float* pd, unsigned int i) { return prim_u(pd,i,105); }
// Layer/Object
__device__ __forceinline__ unsigned int prim_layer_id(const float* pd, unsigned int i) { return prim_u(pd,i,81); }
__device__ __forceinline__ unsigned int prim_object_id(const float* pd, unsigned int i) { return prim_u(pd,i,82); }

// ---- Blend falloff + Protect Walls ----
// Full layout: wall_protect margin rides slot [291] for non-Seam/Extrude ops.
__device__ __forceinline__ float compute_local_k(const float* pd, unsigned int idx, float3 p, float k) {
    float k_out = k;
    float3 packed = prim_falloff_packed(pd, idx);
    float fl = length3(packed);
    if (fl >= 0.001f) {
        float3 axis = mul3f(packed, 1.0f / fl);
        float3 origin = add3(prim_pos(pd, idx), mul3f(axis, prim_falloff_offset(pd, idx)));
        float t = __saturatef(dot3(sub3(p, origin), axis) / fl);
        k_out *= t * t * (3.0f - 2.0f * t);
    }
    unsigned int op = prim_u(pd, idx, 11);
    float margin = (op >= 5u) ? 0.0f : prim_f(pd, idx, 291);
    if (margin > 0.001f) {
        float3 local_p = quat_inv_rotate(prim_quat(pd, idx), sub3(p, prim_pos(pd, idx)));
        float3 sc = prim_scale(pd, idx);
        float3 sp = make_f3(
            local_p.x / fmaxf(sc.x, 1e-4f),
            local_p.y / fmaxf(sc.y, 1e-4f),
            local_p.z / fmaxf(sc.z, 1e-4f));
        float3 elong = make_f3(prim_f(pd, idx, 288), prim_f(pd, idx, 289), prim_f(pd, idx, 290));
        unsigned int st = prim_u(pd, idx, 3) & 0xFFu;
        float p0 = prim_f(pd, idx, 12), p1 = prim_f(pd, idx, 13), p2 = prim_f(pd, idx, 14);
        float3 he = make_f3(p0, p1, p2);
        if (st == 0u) he = make_f3(p0, p0, p0);
        else if (st == 2u || st == 5u || st == 6u || st == 8u) he = make_f3(p0, p1, p0);
        he = make_f3(fmaxf(he.x + elong.x, 1e-4f), fmaxf(he.y + elong.y, 1e-4f), fmaxf(he.z + elong.z, 1e-4f));
        // Shortest half-extent = cut depth (lockstep with CPU / volume_bake).
        float wd;
        if (st == 2u || st == 5u || st == 6u || st == 8u) {
            wd = he.x - sqrtf(sp.x * sp.x + sp.z * sp.z);
        } else if (he.x <= he.y && he.x <= he.z) {
            wd = fminf(he.y - fabsf(sp.y), he.z - fabsf(sp.z));
        } else if (he.y <= he.z) {
            wd = fminf(he.x - fabsf(sp.x), he.z - fabsf(sp.z));
        } else {
            wd = fminf(he.x - fabsf(sp.x), he.y - fabsf(sp.y));
        }
        float tw = __saturatef(wd / margin);
        k_out *= tw * tw * (3.0f - 2.0f * tw);
    }
    return k_out;
}

// ---- Background gradient ----
__device__ __forceinline__ float3 bg_gradient(const GpuUniforms* cfg, float y, float h) {
    float t = y / h;
    float r_top = (float)((cfg->bg_top_packed >> 16u) & 0xFF) / 255.0f;
    float g_top = (float)((cfg->bg_top_packed >> 8u) & 0xFF) / 255.0f;
    float b_top = (float)(cfg->bg_top_packed & 0xFF) / 255.0f;
    float r_bot = (float)((cfg->bg_bottom_packed >> 16u) & 0xFF) / 255.0f;
    float g_bot = (float)((cfg->bg_bottom_packed >> 8u) & 0xFF) / 255.0f;
    float b_bot = (float)(cfg->bg_bottom_packed & 0xFF) / 255.0f;
    return make_f3(
        r_top + (r_bot - r_top) * t,
        g_top + (g_bot - g_top) * t,
        b_top + (b_bot - b_top) * t
    );
}

// ---- Camera ray generation ----
struct CameraRay { float3 origin; float3 dir; };

__device__ __forceinline__ CameraRay camera_ray(const GpuUniforms* cfg, float uv_x, float uv_y, float aspect) {
    CameraRay ray;
    float ortho_hw = cfg->cam_pos.w;
    if (ortho_hw > 0.0f) {
        ray.origin = add3(
            make_f3(cfg->cam_pos.x, cfg->cam_pos.y, cfg->cam_pos.z),
            add3(
                mul3f(make_f3(cfg->cam_right.x, cfg->cam_right.y, cfg->cam_right.z), uv_x * ortho_hw * aspect),
                mul3f(make_f3(cfg->cam_up.x, cfg->cam_up.y, cfg->cam_up.z), uv_y * ortho_hw)
            )
        );
        ray.dir = normalize3(make_f3(cfg->cam_forward.x, cfg->cam_forward.y, cfg->cam_forward.z));
    } else {
        ray.origin = make_f3(cfg->cam_pos.x, cfg->cam_pos.y, cfg->cam_pos.z);
        float3 fwd = make_f3(cfg->cam_forward.x, cfg->cam_forward.y, cfg->cam_forward.z);
        float3 rgt = make_f3(cfg->cam_right.x, cfg->cam_right.y, cfg->cam_right.z);
        float3 up  = make_f3(cfg->cam_up.x, cfg->cam_up.y, cfg->cam_up.z);
        ray.dir = normalize3(add3(fwd, add3(mul3f(rgt, uv_x * aspect), mul3f(up, uv_y))));
    }
    return ray;
}
