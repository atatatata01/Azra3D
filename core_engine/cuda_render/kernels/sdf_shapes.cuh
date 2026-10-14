#pragma once
// ============================================================
// sdf_shapes.cuh — SDF shape evaluation functions
// Direct port from WGSL compute.wgsl shape functions.
// ============================================================

#include "common.cuh"

__device__ __forceinline__ float sdf_sphere(float3 p, float radius) {
    return length3(p) - radius;
}

__device__ __forceinline__ float sdf_box(float3 p, float3 half_ext) {
    float3 q = sub3(abs3(p), half_ext);
    float3 clamped = max3(q, make_f3(0,0,0));
    return length3(clamped) + fminf(fmaxf(q.x, fmaxf(q.y, q.z)), 0.0f);
}

__device__ __forceinline__ float bevel_2d(float a, float b, float r, unsigned int m) {
    if (m == 1u) { // Round
        float qa = a + r, qb = b + r;
        float2 q = make_float2(qa, qb);
        float2 qc = max2(q, make_float2(0,0));
        return length2(qc) + fminf(fmaxf(qa, qb), 0.0f) - r;
    } else if (m == 2u) { // Chamfer
        return fmaxf(fmaxf(a, b), (a + b + r) * 0.7071067811865476f);
    } else { // None (sharp)
        float2 q = make_float2(a, b);
        float2 qc = max2(q, make_float2(0,0));
        return length2(qc) + fminf(fmaxf(a, b), 0.0f);
    }
}

__device__ float sdf_box_per_edge(float3 p, float3 b, const float* pd, unsigned int idx) {
    unsigned int mode_bits = prim_bev_modes(pd, idx);
    unsigned int up_axis = (mode_bits >> 24u) & 3u;
    float3 bp = p, bh = b;
    if (up_axis == 1u) { bp = make_f3(p.y, p.x, p.z); bh = make_f3(b.y, b.x, b.z); }
    else if (up_axis == 2u) { bp = make_f3(p.x, p.z, p.y); bh = make_f3(b.x, b.z, b.y); }
    float qx = fabsf(bp.x) - bh.x;
    float qy = fabsf(bp.y) - bh.y;
    float qz = fabsf(bp.z) - bh.z;
    unsigned int xi = bp.x > 0.0f ? 1u : 0u;
    unsigned int zi = bp.z > 0.0f ? 1u : 0u;
    unsigned int base_y = bp.y > 0.0f ? 4u : 0u;
    unsigned int e_xz = 8u + xi + zi * 2u;
    unsigned int e_xy = base_y + 2u + xi;
    unsigned int e_yz = base_y + zi;
    float d_xz = bevel_2d(qx, qz, prim_bev_edge_r(pd, idx, e_xz), (mode_bits >> (e_xz * 2u)) & 3u);
    float d_xy = bevel_2d(qx, qy, prim_bev_edge_r(pd, idx, e_xy), (mode_bits >> (e_xy * 2u)) & 3u);
    float d_yz = bevel_2d(qy, qz, prim_bev_edge_r(pd, idx, e_yz), (mode_bits >> (e_yz * 2u)) & 3u);
    return fmaxf(d_xz, fmaxf(d_xy, d_yz));
}

__device__ __forceinline__ float sdf_cylinder(float3 p, float radius, float half_h) {
    float d_r = length2(make_float2(p.x, p.z)) - radius;
    float d_h = fabsf(p.y) - half_h;
    float2 q = max2(make_float2(d_r, d_h), make_float2(0,0));
    return length2(q) + fminf(fmaxf(d_r, d_h), 0.0f);
}

__device__ __forceinline__ float sdf_torus(float3 p, float major, float minor) {
    float2 q = make_float2(length2(make_float2(p.x, p.z)) - major, p.y);
    return length2(q) - minor;
}

__device__ __forceinline__ float sdf_cone(float3 p, float radius, float half_h) {
    float q_len = length2(make_float2(p.x, p.z));
    float norm_f = sqrtf(4.0f * half_h * half_h + radius * radius);
    float d_side = (2.0f * half_h * q_len + radius * (p.y - half_h)) / norm_f;
    return fmaxf(d_side, fabsf(p.y) - half_h);
}

__device__ __forceinline__ float sdf_capsule(float3 p, float radius, float half_h) {
    float cy = fminf(fmaxf(p.y, -half_h), half_h);
    float3 closest = make_f3(0, cy, 0);
    return length3(sub3(p, closest)) - radius;
}

__device__ float sdf_stair(float3 p, float3 he, unsigned int n) {
    float qx = fabsf(p.x) - he.x;
    float qy = fabsf(p.y) - he.y;
    float qz = fabsf(p.z) - he.z;
    float3 qv = make_f3(qx, qy, qz);
    float3 qc = max3(qv, make_f3(0,0,0));
    float d_box = length3(qc) + fminf(fmaxf(qx, fmaxf(qy, qz)), 0.0f);
    float nf = (float)n;
    float step_h = he.y * 2.0f / nf;
    float step_d = he.z * 2.0f / nf;
    float cut = 1e10f;
    for (unsigned int i = 0; i < n; i++) {
        float fi = (float)i;
        float y_top = -he.y + (fi + 1.0f) * step_h;
        float z_front = -he.z + (fi + 1.0f) * step_d;
        float c = fmaxf(p.y - y_top, z_front - p.z);
        cut = fminf(cut, c);
    }
    return fmaxf(d_box, cut);
}

__device__ float sdf_spiral_stair(float3 p, float radius, float half_h, unsigned int n, float step_w) {
    float nf = (float)n;
    float step_angle = 6.283185307f / nf;
    float step_h = half_h * 2.0f / nf;
    float angle = atan2f(p.z, p.x);
    float norm_angle = (angle + 3.141592653f) / step_angle;
    int base_idx = (int)floorf(norm_angle);
    float d = 1e10f;
    for (int offset = -1; offset <= 1; offset++) {
        int si = base_idx + offset;
        float a = (float)si * step_angle - 3.141592653f;
        float y_base = -half_h + (float)si * step_h;
        float ca = cosf(a), sa = sinf(a);
        float local_x = ca * p.x + sa * p.z - radius * 0.5f;
        float local_z = -sa * p.x + ca * p.z;
        float local_y = p.y - y_base - step_h * 0.5f;
        float3 he_step = make_f3(radius * 0.5f, step_h * 0.5f, step_w * 0.5f);
        float3 q = sub3(abs3(make_f3(local_x, local_y, local_z)), he_step);
        float3 qc = max3(q, make_f3(0,0,0));
        float step_d = length3(qc) + fminf(fmaxf(q.x, fmaxf(q.y, q.z)), 0.0f);
        d = fminf(d, step_d);
    }
    // Central column
    float col_r = length2(make_float2(p.x, p.z)) - 0.08f;
    float col_h = fabsf(p.y) - half_h;
    float2 col_q = max2(make_float2(col_r, col_h), make_float2(0,0));
    float column = length2(col_q) + fminf(fmaxf(col_r, col_h), 0.0f);
    return fminf(d, column);
}
