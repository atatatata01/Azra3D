#pragma once
// ============================================================
// materials.cuh — Material evaluation, tri-planar sampling
// Direct port from WGSL compute.wgsl material functions.
// ============================================================

#include "sdf_eval.cuh"

// ---- Material result ----
struct MatResult {
    float3 color;
    float roughness;
    float metallic;
    float opacity;
    float3 emission;
};

// ---- Material evaluation ----
__device__ MatResult eval_material(const GpuUniforms* cfg, const float* pd, unsigned int hit_prim, float3 p, float3 normal) {
    MatResult mat;
    mat.color = prim_albedo(pd, hit_prim);
    mat.roughness = prim_base_rough(pd, hit_prim);
    mat.metallic = prim_base_metal(pd, hit_prim);
    mat.opacity = prim_base_opacity(pd, hit_prim);

    // Emission
    float em_int = prim_emission_intensity(pd, hit_prim);
    if (em_int > 0.001f) {
        mat.emission = mul3f(prim_emission_color(pd, hit_prim), em_int);
    } else {
        mat.emission = make_f3(0, 0, 0);
    }

    // Note: texture sampling (atlas/triplanar) deferred to later phase
    // when texture buffers are wired into CUDA

    unsigned int quality = cfg->render_quality;
    unsigned int mat_q = prim_mat_quality(pd, hit_prim);
    if (quality < 1u || mat_q == 0u) return mat;

    // Curvature for edge/cavity detection
    unsigned int eff_q = mat_q == 2u ? 4u : quality;
    float curv_eps = eff_q >= 3u ? 0.005f : (eff_q >= 2u ? 0.01f : 0.015f);
    float curv = calc_curvature(cfg, pd, p, curv_eps);

    // --- Noise pattern layer ---
    float np_amt = prim_np_amount(pd, hit_prim);
    if (np_amt > 0.001f) {
        float scale = prim_np_scale(pd, hit_prim);
        unsigned int oct = prim_np_oct(pd, hit_prim);
        float n = fbm3(mul3f(p, scale), oct);
        float mask = n * np_amt;
        mat.color = mix3(mat.color, prim_np_color(pd, hit_prim), mask);
        mat.roughness = mat.roughness + (prim_np_rough(pd, hit_prim) - mat.roughness) * mask;
        mat.metallic = mat.metallic + (prim_np_metal(pd, hit_prim) - mat.metallic) * mask;
    }

    // --- Cavity dirt layer ---
    float cd_amt = prim_cd_amount(pd, hit_prim);
    if (cd_amt > 0.001f) {
        float sharpness = prim_cd_sharp(pd, hit_prim);
        float cavity_mask = gpu_smoothstep(0.0f, -sharpness * 10.0f, curv) * cd_amt;
        mat.color = mix3(mat.color, prim_cd_color(pd, hit_prim), cavity_mask);
        mat.roughness = mat.roughness + (prim_cd_rough(pd, hit_prim) - mat.roughness) * cavity_mask;
        mat.metallic = mat.metallic + (prim_cd_metal(pd, hit_prim) - mat.metallic) * cavity_mask;
    }

    // --- Edge wear layer ---
    float ew_amt = prim_ew_amount(pd, hit_prim);
    if (ew_amt > 0.001f) {
        float sharpness = prim_ew_sharp(pd, hit_prim);
        float edge_mask = gpu_smoothstep(0.0f, sharpness * 10.0f, curv) * ew_amt;
        mat.color = mix3(mat.color, prim_ew_color(pd, hit_prim), edge_mask);
        mat.roughness = mat.roughness + (prim_ew_rough(pd, hit_prim) - mat.roughness) * edge_mask;
        mat.metallic = mat.metallic + (prim_ew_metal(pd, hit_prim) - mat.metallic) * edge_mask;
    }

    return mat;
}
