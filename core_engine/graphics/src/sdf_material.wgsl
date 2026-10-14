// #region SHADOW_AO
// ============================================================
// Lighting helpers
// ============================================================
fn soft_shadow(ro: vec3<f32>, rd: vec3<f32>, mint: f32, maxt: f32, k: f32) -> f32 {
    var res = 1.0; var t = max(mint, 0.001);
    let steps = select(16u, select(24u, 32u, config.render_quality >= 3u), config.render_quality >= 2u);
    for (var i = 0u; i < steps; i++) {
        let d = scene_sdf(ro + rd * t);
        // Inside a subtract cavity the SDF is negative at the shadow origin.
        // Skip forward past the negative region instead of returning full shadow.
        if (d < 0.0) { t += abs(d) + 0.01; continue; }
        if (d < 0.001) { return 0.0; }
        let s = clamp(k * d / t, 0.0, 1.0);
        res = min(res, s * s * (3.0 - 2.0 * s));
        t += clamp(d, 0.02, 0.2);
        if (t > maxt) { break; }
    }
    return clamp(res, 0.0, 1.0);
}

fn calc_ao(p: vec3<f32>, n: vec3<f32>) -> f32 {
    var occ = 0.0; var scale = 1.0;
    let samples = select(3u, select(5u, 8u, config.render_quality >= 3u), config.render_quality >= 2u);
    for (var i = 1u; i <= samples; i++) {
        let h = 0.01 + 0.12 * f32(i) / f32(samples);
        occ += (h - scene_sdf(p + n * h)) * scale;
        scale *= 0.75;
    }
    return clamp(1.0 - 2.5 * occ, 0.0, 1.0);
}

fn specular_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(v + l);
    let ndh = max(dot(n, h), 0.0);
    let ndl = max(dot(n, l), 0.0);
    // Floor roughness so smooth materials still peak; never add a large constant to D's denom
    // (old `+ 0.0001` made roughness 0.05..0.5 look identical).
    let r = clamp(roughness, 0.04, 1.0);
    let a = r * r; let a2 = max(a * a, 1e-7);
    let denom = ndh * ndh * (a2 - 1.0) + 1.0;
    let D = a2 / max(3.14159265 * denom * denom, 1e-7);
    return D * ndl * 0.04;
}
// #endregion SHADOW_AO

// #region RNG_TRACE
// ============================================================
// Random number generation (PCG hash) + sampling helpers
// ============================================================
fn pcg_hash(input: u32) -> u32 {
    let state = input * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}
fn rand_float(seed: ptr<function, u32>) -> f32 {
    *seed = pcg_hash(*seed);
    return f32(*seed) / 4294967295.0;
}

fn cosine_hemisphere(n: vec3<f32>, seed: ptr<function, u32>) -> vec3<f32> {
    let r1 = rand_float(seed);
    let r2 = rand_float(seed);
    let phi = 2.0 * 3.14159 * r1;
    let cos_theta = sqrt(r2);
    let sin_theta = sqrt(1.0 - r2);
    var tangent: vec3<f32>;
    if (abs(n.y) < 0.999) {
        tangent = normalize(cross(vec3(0.0, 1.0, 0.0), n));
    } else {
        tangent = normalize(cross(vec3(1.0, 0.0, 0.0), n));
    }
    let bitangent = cross(n, tangent);
    return normalize(tangent * cos(phi) * sin_theta + bitangent * sin(phi) * sin_theta + n * cos_theta);
}

// SDF ray march helper — returns vec2(t, hit_flag)
fn trace_ray(origin: vec3<f32>, dir: vec3<f32>, max_t: f32) -> vec2<f32> {
    var t = 0.0;
    for (var i = 0u; i < 80u; i++) {
        let p = origin + dir * t;
        let d = scene_sdf(p);
        if (d < config.hit_threshold) { return vec2(t, 1.0); }
        t += d * 0.6;
        if (t > max_t) { break; }
    }
    return vec2(t, 0.0);
}
// #endregion RNG_TRACE

// #region MATERIAL
// ============================================================
// Material evaluation — blends base + edge wear + cavity + noise
// ============================================================
struct MatResult {
    color: vec3<f32>,
    roughness: f32,
    metallic: f32,
    opacity: f32,
    emission: vec3<f32>,
}

fn eval_material(hit_prim: u32, p: vec3<f32>, normal: vec3<f32>) -> MatResult {
    var mat: MatResult;
    mat.color = prim_albedo(hit_prim);
    mat.roughness = prim_base_rough(hit_prim);
    mat.metallic = prim_base_metal(hit_prim);
    mat.opacity = prim_base_opacity(hit_prim);
    // Emission
    let em_int = prim_emission_intensity(hit_prim);
    mat.emission = select(vec3(0.0), prim_emission_color(hit_prim) * em_int, em_int > 0.001);

    // Per-primitive atlas texture override
    let atlas_id = prim_texture_id(hit_prim);
    let shape_t = prim_shape_type(hit_prim);
    if (atlas_id > 0u) {
        if (shape_t == 8u) {
            // Billboard: flat UV from local XY
            let lp = quat_inv_rotate(prim_quat(hit_prim), p - prim_pos(hit_prim)) / prim_scale(hit_prim);
            let w = prim_f(hit_prim, 12u);
            let h = prim_f(hit_prim, 13u);
            let uv = vec2((lp.x / (w * 0.5) + 1.0) * 0.5, 1.0 - (lp.y / (h * 0.5) + 1.0) * 0.5);
            mat.color = atlas_sample_bilinear(atlas_id, uv);
        } else {
            let tiling = prim_tex_tiling(hit_prim);
            mat.color = sample_triplanar_atlas(atlas_id, p, normal, tiling);
        }
        let lum = dot(mat.color, vec3(0.299, 0.587, 0.114));
        mat.roughness = mix(mat.roughness, 1.0 - lum * 0.5, 0.3);
    } else if (config.tex_size > 0u && prim_has_texture(hit_prim)) {
        // Legacy global AI texture override
        let tiling = prim_tex_tiling(hit_prim);
        mat.color = sample_triplanar(p, normal, tiling);
        let lum = dot(mat.color, vec3(0.299, 0.587, 0.114));
        mat.roughness = mix(mat.roughness, 1.0 - lum * 0.5, 0.3);
    }

    let quality = config.render_quality;
    let mat_q = prim_mat_quality(hit_prim); // 0=Optimize, 1=Normal, 2=God
    // Skip procedural layers at Draft quality or material Optimize mode
    if (quality < 1u || mat_q == 0u) { return mat; }

    // Curvature for edge/cavity detection (quality >= 1)
    // God material quality overrides curvature epsilon to finest
    let eff_q = select(quality, 4u, mat_q == 2u);
    let curv_eps = select(0.015, select(0.01, 0.005, eff_q >= 3u), eff_q >= 2u);
    let curv = calc_curvature(p, curv_eps);

    // --- Noise pattern layer ---
    let np_amt = prim_np_amount(hit_prim);
    if (np_amt > 0.001) {
        let scale = prim_np_scale(hit_prim);
        let oct = prim_np_oct(hit_prim);
        let n = fbm3(p * scale, oct);
        let mask = n * np_amt;
        mat.color = mix(mat.color, prim_np_color(hit_prim), mask);
        mat.roughness = mix(mat.roughness, prim_np_rough(hit_prim), mask);
        mat.metallic = mix(mat.metallic, prim_np_metal(hit_prim), mask);
    }

    // --- Cavity dirt layer (concave areas = negative curvature) ---
    let cd_amt = prim_cd_amount(hit_prim);
    if (cd_amt > 0.001) {
        let sharpness = prim_cd_sharp(hit_prim);
        let cavity_mask = smoothstep(0.0, -sharpness * 10.0, curv) * cd_amt;
        mat.color = mix(mat.color, prim_cd_color(hit_prim), cavity_mask);
        mat.roughness = mix(mat.roughness, prim_cd_rough(hit_prim), cavity_mask);
        mat.metallic = mix(mat.metallic, prim_cd_metal(hit_prim), cavity_mask);
    }

    // --- Edge wear layer (convex areas = positive curvature) ---
    let ew_amt = prim_ew_amount(hit_prim);
    if (ew_amt > 0.001) {
        let sharpness = prim_ew_sharp(hit_prim);
        let edge_mask = smoothstep(0.0, sharpness * 10.0, curv) * ew_amt;
        mat.color = mix(mat.color, prim_ew_color(hit_prim), edge_mask);
        mat.roughness = mix(mat.roughness, prim_ew_rough(hit_prim), edge_mask);
        mat.metallic = mix(mat.metallic, prim_ew_metal(hit_prim), edge_mask);
    }

    return mat;
}

// Evaluate material with boolean seam blending between two prims
fn eval_material_blended(p: vec3<f32>, normal: vec3<f32>) -> MatResult {
    let sb = find_seam_blend(p);
    let p1 = u32(sb.x);
    let p2 = u32(sb.y);
    let blend = sb.z;
    let mat1 = eval_material(p1, p, normal);
    if (blend < 0.001 || p1 == p2) { return mat1; }
    let mat2 = eval_material(p2, p, normal);
    var result: MatResult;
    result.color = mix(mat1.color, mat2.color, blend);
    result.roughness = mix(mat1.roughness, mat2.roughness, blend);
    result.metallic = mix(mat1.metallic, mat2.metallic, blend);
    result.opacity = mix(mat1.opacity, mat2.opacity, blend);
    result.emission = mix(mat1.emission, mat2.emission, blend);
    return result;
}

// ============================================================
// Node-graph material evaluation (injected at build time)
// ============================================================
{{NODE_MATERIAL_FN}}
// #endregion MATERIAL
