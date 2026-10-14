#pragma once
// ============================================================
// sdf_eval.cuh — Primitive evaluation, boolean ops, scene SDF
// Direct port from WGSL compute.wgsl eval/scene functions.
// ============================================================

#include "sdf_shapes.cuh"

// ---- Volumetric mask evaluation ----
__device__ float eval_mask(const float* pd, unsigned int idx, float3 p) {
    unsigned int mt = prim_mask_type(pd, idx);
    if (mt == 0u) return 1.0f;
    float3 center = prim_mask_center(pd, idx);
    float3 extents = prim_mask_extents(pd, idx);
    float feather = prim_mask_feather(pd, idx);
    unsigned int inv = prim_mask_invert(pd, idx);
    float w = 1.0f;
    if (mt == 1u) { // Sphere
        float d = length3(sub3(p, center));
        float r = fmaxf(extents.x, 0.001f);
        w = gpu_smoothstep(r, r * (1.0f - fmaxf(feather, 0.001f)), d);
    } else if (mt == 2u) { // Box
        float3 ext = max3(extents, make_f3(0.001f, 0.001f, 0.001f));
        float3 q = abs3(div3(sub3(p, center), ext));
        float d = fmaxf(q.x, fmaxf(q.y, q.z));
        w = gpu_smoothstep(1.0f, 1.0f - fmaxf(feather, 0.001f), d);
    } else if (mt == 3u) { // Gradient
        float fl = length3(extents);
        if (fl < 0.001f) return 1.0f;
        float3 dir = mul3f(extents, 1.0f / fl);
        float t = __saturatef(dot3(sub3(p, center), dir) / fl);
        w = t * t * (3.0f - 2.0f * t);
    }
    if (inv != 0u) w = 1.0f - w;
    return w;
}

// ---- Domain warp: Twist ----
__device__ __forceinline__ float3 apply_twist(float3 p, float angle_per_unit, unsigned int axis) {
    float a;
    if (axis == 1u) a = p.x * angle_per_unit;
    else if (axis == 2u) a = p.z * angle_per_unit;
    else a = p.y * angle_per_unit;
    float s = sinf(a), c = cosf(a);
    if (axis == 1u) return make_f3(p.x, c*p.y - s*p.z, s*p.y + c*p.z);
    else if (axis == 2u) return make_f3(c*p.x - s*p.y, s*p.x + c*p.y, p.z);
    else return make_f3(c*p.x - s*p.z, p.y, s*p.x + c*p.z);
}

// ---- Domain warp: Taper ----
__device__ __forceinline__ float3 apply_taper(float3 p, float strength, unsigned int axis, float end_scale) {
    float axis_val;
    if (axis == 1u) axis_val = p.x;
    else if (axis == 2u) axis_val = p.z;
    else axis_val = p.y;
    float t = axis_val * strength;
    float sc = fmaxf(1.0f + t * (end_scale - 1.0f), 0.01f);
    if (axis == 1u) return make_f3(p.x, p.y / sc, p.z / sc);
    else if (axis == 2u) return make_f3(p.x / sc, p.y / sc, p.z);
    else return make_f3(p.x / sc, p.y, p.z / sc);
}

// ---- Evaluate single primitive SDF ----
__device__ float eval_prim(const float* pd, unsigned int idx, float3 p) {
    float3 local_p = quat_inv_rotate(prim_quat(pd, idx), sub3(p, prim_pos(pd, idx)));
    float3 sc = prim_scale(pd, idx);
    float3 sp = div3(local_p, sc);

    // Mirror modifier
    unsigned int mb = prim_mirror_bits(pd, idx);
    float3 mc = prim_mirror_center(pd, idx);
    if (mb & 1u) sp.x = fabsf(sp.x - mc.x) + mc.x;
    if (mb & 2u) sp.y = fabsf(sp.y - mc.y) + mc.y;
    if (mb & 4u) sp.z = fabsf(sp.z - mc.z) + mc.z;

    // Domain warp modifiers
    unsigned int mod_t = prim_mod_type(pd, idx);
    float3 eval_p = sp;
    if (mod_t == 1u || mod_t == 2u) {
        float mask_w = eval_mask(pd, idx, sp) * prim_mod_strength(pd, idx);
        unsigned int ax = prim_mod_axis(pd, idx);
        if (mod_t == 1u) eval_p = apply_twist(sp, mask_w, ax);
        else eval_p = apply_taper(sp, mask_w, ax, prim_mod_param2(pd, idx));
    }

    float min_s = fminf(sc.x, fminf(sc.y, sc.z));
    unsigned int st = prim_shape_type(pd, idx);
    float p0 = prim_f(pd, idx, 12), p1 = prim_f(pd, idx, 13), p2 = prim_f(pd, idx, 14);
    float d = 0.0f;

    if (st == 0u) d = sdf_sphere(eval_p, p0);
    else if (st == 1u) {
        if (prim_bev_modes(pd, idx) == 0u) d = sdf_box(eval_p, make_f3(p0, p1, p2));
        else d = sdf_box_per_edge(eval_p, make_f3(p0, p1, p2), pd, idx);
    }
    else if (st == 2u) d = sdf_cylinder(eval_p, p0, p1);
    else if (st == 3u) d = sdf_torus(eval_p, p0, p1);
    else if (st == 4u) d = sdf_cone(eval_p, p0, p1);
    else if (st == 5u) d = sdf_capsule(eval_p, p0, p1);
    else if (st == 6u) d = sdf_stair(eval_p, make_f3(p0, p1, p2), prim_stair_steps(pd, idx));
    else if (st == 7u) d = sdf_spiral_stair(eval_p, p0, p1, prim_stair_steps(pd, idx), p2);
    else if (st == 8u) { // Billboard: thin slab
        float3 he = make_f3(p0 * 0.5f, p1 * 0.5f, 0.005f);
        float3 q = sub3(abs3(eval_p), he);
        float3 qc = max3(q, make_f3(0,0,0));
        d = length3(qc) + fminf(fmaxf(q.x, fmaxf(q.y, q.z)), 0.0f);
    }

    // Surface alteration: Smooth
    if (mod_t == 3u) {
        float mask_w = eval_mask(pd, idx, sp);
        d -= prim_mod_strength(pd, idx) * mask_w;
    }

    // Subdivision rounding
    unsigned int subdiv = prim_subdivision(pd, idx);
    if (subdiv >= 6u) d -= 0.18f;
    else if (subdiv == 5u) d -= 0.10f;
    else if (subdiv == 4u) d -= 0.05f;
    else if (subdiv == 3u) d -= 0.02f;
    else if (subdiv == 2u) d -= 0.008f;
    else if (subdiv == 1u) d -= 0.003f;

    return d * min_s;
}

// ---- Boolean operation ----
__device__ __forceinline__ float apply_boolean(float result, float d, unsigned int op, float k) {
    if (op == 0u) return k > 0.0f ? smin(result, d, k) : fminf(result, d);
    else if (op == 1u) return k > 0.0f ? smax(result, -d, k) : fmaxf(result, -d);
    else if (op == 2u) return fmaxf(result, d);
    else if (op == 3u) return chamfer_min(result, d, k);
    else if (op == 4u) return fmaxf(result, -(chamfer_min(-result, d, k)));
    return result;
}

// ---- Scene SDF: 3-level evaluator (prim -> object -> layer -> scene) ----
__device__ float scene_analytical(const GpuUniforms* cfg, const float* pd, float3 p) {
    if (cfg->num_primitives == 0u) return 999.0f;

    float scene = 999.0f;
    float layer_acc = 999.0f;
    float obj_acc = 999.0f;
    unsigned int cur_layer = 0u;
    unsigned int cur_obj = 0u;

    for (unsigned int i = 0; i < cfg->num_primitives; i++) {
        unsigned int lid = prim_layer_id(pd, i);
        unsigned int oid = prim_object_id(pd, i);
        unsigned int op = prim_u(pd, i, 11);

        // Object boundary: flush previous object
        if (oid != cur_obj) {
            if (cur_obj != 0u && obj_acc < 998.0f) {
                if (cur_layer == 0u) { scene = scene > 998.0f ? obj_acc : fminf(scene, obj_acc); }
                else { layer_acc = layer_acc > 998.0f ? obj_acc : fminf(layer_acc, obj_acc); }
            }
            cur_obj = oid;
            obj_acc = 999.0f;
        }

        // Layer boundary: flush
        if (lid != cur_layer) {
            if (cur_obj != 0u && obj_acc < 998.0f) {
                if (cur_layer == 0u) { scene = scene > 998.0f ? obj_acc : fminf(scene, obj_acc); }
                else { layer_acc = layer_acc > 998.0f ? obj_acc : fminf(layer_acc, obj_acc); }
                cur_obj = oid;
                obj_acc = 999.0f;
            }
            if (cur_layer != 0u && layer_acc < 998.0f) {
                scene = scene > 998.0f ? layer_acc : fminf(scene, layer_acc);
            }
            cur_layer = lid;
            layer_acc = 999.0f;
        }

        // Evaluate primitive
        float d = eval_prim(pd, i, p);
        float k_raw = prim_f(pd, i, 15);
        float k = compute_local_k(pd, i, p, k_raw);

        if (oid != 0u) {
            // Object member
            if (obj_acc > 998.0f) obj_acc = d;
            else obj_acc = apply_boolean(obj_acc, d, op, k);
        } else {
            if (cur_layer == 0u) {
                if (scene > 998.0f) { scene = d; }
                else {
                    // BVH sphere cull for union ops
                    if (op == 0u || op == 3u) {
                        float sphere_d = length3(sub3(p, prim_pos(pd, i))) - prim_bound_r(pd, i);
                        if (sphere_d > scene) continue;
                    }
                    scene = apply_boolean(scene, d, op, k);
                }
            } else {
                if (layer_acc > 998.0f) layer_acc = d;
                else layer_acc = apply_boolean(layer_acc, d, op, k);
            }
        }
    }

    // Flush trailing object
    if (cur_obj != 0u && obj_acc < 998.0f) {
        if (cur_layer == 0u) { scene = scene > 998.0f ? obj_acc : fminf(scene, obj_acc); }
        else { layer_acc = layer_acc > 998.0f ? obj_acc : fminf(layer_acc, obj_acc); }
    }
    // Flush trailing layer
    if (cur_layer != 0u && layer_acc < 998.0f) {
        scene = scene > 998.0f ? layer_acc : fminf(scene, layer_acc);
    }

    return scene < 998.0f ? scene : 999.0f;
}

// ---- Scene SDF dispatcher ----
__device__ __forceinline__ float scene_sdf(const GpuUniforms* cfg, const float* pd, float3 p) {
    // CUDA only supports analytical mode (render_mode 0)
    return scene_analytical(cfg, pd, p);
}

// ---- Normal calculation (central differences) ----
__device__ __forceinline__ float3 calc_normal(const GpuUniforms* cfg, const float* pd, float3 p) {
    float e = cfg->normal_epsilon;
    return normalize3(make_f3(
        scene_sdf(cfg, pd, add3(p, make_f3(e,0,0))) - scene_sdf(cfg, pd, sub3(p, make_f3(e,0,0))),
        scene_sdf(cfg, pd, add3(p, make_f3(0,e,0))) - scene_sdf(cfg, pd, sub3(p, make_f3(0,e,0))),
        scene_sdf(cfg, pd, add3(p, make_f3(0,0,e))) - scene_sdf(cfg, pd, sub3(p, make_f3(0,0,e)))
    ));
}

// ---- Find closest primitive to surface point ----
__device__ unsigned int find_hit_prim(const GpuUniforms* cfg, const float* pd, float3 p) {
    unsigned int best = 0;
    float best_d = fabsf(eval_prim(pd, 0, p));
    for (unsigned int i = 1; i < cfg->num_primitives; i++) {
        unsigned int op = prim_u(pd, i, 11);
        if (op == 0u || op == 3u) {
            float sphere_d = length3(sub3(p, prim_pos(pd, i))) - prim_bound_r(pd, i);
            if (sphere_d > best_d) continue;
        }
        float d = fabsf(eval_prim(pd, i, p));
        if (d < best_d) { best_d = d; best = i; }
    }
    return best;
}

// ---- Soft shadow ----
__device__ float soft_shadow(const GpuUniforms* cfg, const float* pd, float3 ro, float3 rd, float mint, float maxt, float k) {
    float res = 1.0f, t = mint;
    unsigned int steps = cfg->render_quality >= 3u ? 32u : (cfg->render_quality >= 2u ? 24u : 16u);
    for (unsigned int i = 0; i < steps; i++) {
        float d = scene_sdf(cfg, pd, add3(ro, mul3f(rd, t)));
        if (d < 0.001f) return 0.0f;
        float s = __saturatef(k * d / t);
        res = fminf(res, s * s * (3.0f - 2.0f * s));
        t += fminf(fmaxf(d, 0.02f), 0.2f);
        if (t > maxt) break;
    }
    return __saturatef(res);
}

// ---- Ambient occlusion ----
__device__ float calc_ao(const GpuUniforms* cfg, const float* pd, float3 p, float3 n) {
    float occ = 0.0f, scale = 1.0f;
    unsigned int samples = cfg->render_quality >= 3u ? 8u : (cfg->render_quality >= 2u ? 5u : 3u);
    for (unsigned int i = 1; i <= samples; i++) {
        float h = 0.01f + 0.12f * (float)i / (float)samples;
        occ += (h - scene_sdf(cfg, pd, add3(p, mul3f(n, h)))) * scale;
        scale *= 0.75f;
    }
    return __saturatef(1.0f - 2.5f * occ);
}

// ---- Ray trace helper ----
__device__ float2 trace_ray(const GpuUniforms* cfg, const float* pd, float3 origin, float3 dir, float max_t) {
    float t = 0.0f;
    for (unsigned int i = 0; i < 80u; i++) {
        float3 p = add3(origin, mul3f(dir, t));
        float d = scene_sdf(cfg, pd, p);
        if (d < cfg->hit_threshold) return make_float2(t, 1.0f);
        t += d * 0.6f;
        if (t > max_t) break;
    }
    return make_float2(t, 0.0f);
}

// ---- GGX specular ----
__device__ __forceinline__ float specular_ggx(float3 n, float3 v, float3 l, float roughness) {
    float3 h = normalize3(add3(v, l));
    float ndh = fmaxf(dot3(n, h), 0.0f);
    float ndl = fmaxf(dot3(n, l), 0.0f);
    float a = roughness * roughness;
    float a2 = a * a;
    float denom = ndh * ndh * (a2 - 1.0f) + 1.0f;
    return (a2 / (3.14159f * denom * denom + 0.0001f)) * ndl * 0.04f;
}

// ---- Curvature (Laplacian) ----
// TANGENT-PLANE mean-curvature Laplacian (2026-07-27 fix — direct port of the CPU/WGSL fix,
// keep IDENTICAL math: `SdfFieldSources::calc_curvature` (app/src/node_graph_bake.rs),
// `calc_curvature` (graphics/src/sdf_prim_eval.wgsl), `curv_laplacian` (volume_bake.wgsl)).
// The old 6-neighbour WORLD-AXIS stencil second-differenced fixed X/Y/Z regardless of local
// surface orientation — at large `e` the tap along whichever axis happened to be TANGENT to the
// surface escaped the local feature and sampled unrelated geometry, flipping the sign so carved
// cavities read convex. Mean curvature only lives IN the tangent plane, so tap only there:
// normal from a central-difference gradient (LOCAL step, clamped independent of `e`), Duff/
// Hughes-Möller tangent basis, then a 4-tap Laplacian along (t1, t2) only. Sign proved
// empirically on known geometry (CPU doc comment): raw tangent-plane Laplacian already reads
// positive-on-convex / negative-on-concave — no extra flip needed.
__device__ __forceinline__ void calc_curvature_tangent_basis(float3 n, float3* t1, float3* t2) {
    float s = (n.z >= 0.0f) ? 1.0f : -1.0f;
    float a = -1.0f / (s + n.z);
    float b = n.x * n.y * a;
    *t1 = make_f3(1.0f + s * n.x * n.x * a, s * b, -s * n.x);
    *t2 = make_f3(b, s + n.y * n.y * a, -n.y);
}
__device__ float calc_curvature(const GpuUniforms* cfg, const float* pd, float3 p, float e) {
    float d = scene_sdf(cfg, pd, p);
    float ge = fminf(fmaxf(e, 1e-4f), 0.01f);
    float gx = scene_sdf(cfg, pd, add3(p, make_f3(ge,0,0))) - scene_sdf(cfg, pd, sub3(p, make_f3(ge,0,0)));
    float gy = scene_sdf(cfg, pd, add3(p, make_f3(0,ge,0))) - scene_sdf(cfg, pd, sub3(p, make_f3(0,ge,0)));
    float gz = scene_sdf(cfg, pd, add3(p, make_f3(0,0,ge))) - scene_sdf(cfg, pd, sub3(p, make_f3(0,0,ge)));
    float gl = sqrtf(gx*gx + gy*gy + gz*gz);
    float3 n = (gl > 1e-6f) ? make_f3(gx/gl, gy/gl, gz/gl) : make_f3(0.0f, 0.0f, 1.0f);
    float3 t1, t2;
    calc_curvature_tangent_basis(n, &t1, &t2);
    float lap = scene_sdf(cfg, pd, add3(p, mul3f(t1, e))) + scene_sdf(cfg, pd, sub3(p, mul3f(t1, e)))
              + scene_sdf(cfg, pd, add3(p, mul3f(t2, e))) + scene_sdf(cfg, pd, sub3(p, mul3f(t2, e)))
              - 4.0f * d;
    return lap / (e * e);
}
