// #region SDF_PRIM_EVAL
// ============================================================
// Primitive data accessors ({{FLOATS_PER_PRIM}} floats per primitive, injected from Rust)
// ============================================================
// T1 live-march image displace atlas. Binding 15 is free on raymarch and prim_outline
// (outline owns 5–8 for mesh/clip). Naga DCE strips when entry never samples.
@group(0) @binding(15) var<storage, read> height_maps: array<f32>;

fn prim_f(idx: u32, field: u32) -> f32 { return prim_data[idx * {{FLOATS_PER_PRIM}}u + field]; }
fn prim_u(idx: u32, field: u32) -> u32 { return bitcast<u32>(prim_data[idx * {{FLOATS_PER_PRIM}}u + field]); }

// Bilinear sample — keep in lockstep with volume_bake.wgsl sample_height_map.
fn sample_height_map_live(start: u32, sp: vec3<f32>, freq: f32) -> f32 {
    if (start == 0xFFFFFFFFu) { return 0.0; }
    let w = bitcast<u32>(height_maps[start]);
    let h = bitcast<u32>(height_maps[start + 1u]);
    let axis = bitcast<u32>(height_maps[start + 2u]);
    if (w == 0u || h == 0u) { return 0.0; }
    let f = max(freq, 0.1);
    var u: f32;
    var v: f32;
    if (axis == 1u) {
        u = sp.y * f * 0.5 + 0.5;
        v = sp.z * f * 0.5 + 0.5;
    } else if (axis == 2u) {
        u = sp.x * f * 0.5 + 0.5;
        v = sp.y * f * 0.5 + 0.5;
    } else {
        u = sp.x * f * 0.5 + 0.5;
        v = sp.z * f * 0.5 + 0.5;
    }
    let wf = f32(w);
    let hf = f32(h);
    var uf = (u - floor(u)) * wf;
    var vf = (v - floor(v)) * hf;
    uf = uf - 0.5;
    vf = vf - 0.5;
    uf = uf - floor(uf / wf) * wf;
    vf = vf - floor(vf / hf) * hf;
    if (uf < 0.0) { uf = uf + wf; }
    if (vf < 0.0) { vf = vf + hf; }
    let x0 = u32(floor(uf)) % w;
    let y0 = u32(floor(vf)) % h;
    let x1 = (x0 + 1u) % w;
    let y1 = (y0 + 1u) % h;
    let fx = uf - floor(uf);
    let fy = vf - floor(vf);
    let base = start + 4u;
    let p00 = height_maps[base + y0 * w + x0];
    let p10 = height_maps[base + y0 * w + x1];
    let p01 = height_maps[base + y1 * w + x0];
    let p11 = height_maps[base + y1 * w + x1];
    let a = mix(p00, p10, fx);
    let b = mix(p01, p11, fx);
    let h01 = mix(a, b, fy);
    return h01 * 2.0 - 1.0;
}

fn prim_pos(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,0u), prim_f(idx,1u), prim_f(idx,2u)); }
fn prim_quat(idx: u32) -> vec4<f32> { return vec4(prim_f(idx,4u), prim_f(idx,5u), prim_f(idx,6u), prim_f(idx,7u)); }
fn prim_scale(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,8u), prim_f(idx,9u), prim_f(idx,10u)); }
fn prim_shape_type(idx: u32) -> u32 { return prim_u(idx, 3u) & 0xFFu; }
fn prim_is_selected(idx: u32) -> bool { return (prim_u(idx, 3u) & 256u) != 0u; }
fn prim_op_type(idx: u32) -> u32 { return prim_u(idx, 11u); }

// Material accessors
fn prim_albedo(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,16u), prim_f(idx,17u), prim_f(idx,18u)); }
fn prim_base_rough(idx: u32) -> f32 { return prim_f(idx,19u); }
fn prim_base_metal(idx: u32) -> f32 { return prim_f(idx,20u); }
// Edge wear
fn prim_ew_amount(idx: u32) -> f32 { return prim_f(idx,21u); }
fn prim_ew_color(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,22u), prim_f(idx,23u), prim_f(idx,24u)); }
fn prim_ew_rough(idx: u32) -> f32 { return prim_f(idx,25u); }
fn prim_ew_metal(idx: u32) -> f32 { return prim_f(idx,26u); }
fn prim_ew_sharp(idx: u32) -> f32 { return prim_f(idx,27u); }
// Cavity dirt
fn prim_cd_amount(idx: u32) -> f32 { return prim_f(idx,28u); }
fn prim_cd_color(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,29u), prim_f(idx,30u), prim_f(idx,31u)); }
fn prim_cd_rough(idx: u32) -> f32 { return prim_f(idx,32u); }
fn prim_cd_metal(idx: u32) -> f32 { return prim_f(idx,33u); }
fn prim_cd_sharp(idx: u32) -> f32 { return prim_f(idx,34u); }
// Noise pattern
fn prim_base_opacity(idx: u32) -> f32 { return prim_f(idx,77u); }
fn prim_np_amount(idx: u32) -> f32 { return prim_f(idx,35u); }
fn prim_np_color(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,36u), prim_f(idx,37u), prim_f(idx,38u)); }
fn prim_np_rough(idx: u32) -> f32 { return prim_f(idx,39u); }
fn prim_np_metal(idx: u32) -> f32 { return prim_f(idx,40u); }
fn prim_np_scale(idx: u32) -> f32 { return prim_f(idx,41u); }
fn prim_np_oct(idx: u32) -> u32 { return u32(prim_f(idx,42u)); }

// Per-primitive AI texture flag (field 43) and tiling (field 44)
fn prim_has_texture(idx: u32) -> bool { return prim_f(idx, 43u) > 0.5; }
fn prim_tex_tiling(idx: u32) -> f32 { let v = prim_f(idx, 44u); return select(2.0, v, v > 0.01); }
// Bounding sphere radius (field 45) — conservative world-space bound for GPU culling
fn prim_bound_r(idx: u32) -> f32 { return prim_f(idx, 45u); }
// Box per-edge bevel data (fields 48-60, Box shape only)
// Radii: floats [48-59] = edges 0-11 (0-3=bot, 4-7=top, 8-11=vert)
// Modes: float [60] as u32 = packed 2 bits per edge (0=none,1=round,2=chamfer)
fn prim_bev_edge_r(idx: u32, edge: u32) -> f32 { return prim_f(idx, 48u + edge); }
fn prim_bev_modes(idx: u32) -> u32   { return prim_u(idx, 60u); }
fn prim_mirror_bits(idx: u32) -> u32 { return prim_u(idx, 61u); }
fn prim_mirror_center(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 78u), prim_f(idx, 79u), prim_f(idx, 80u)); }
fn prim_layer_id(idx: u32) -> u32 { return prim_u(idx, 81u); }
fn prim_stair_steps(idx: u32) -> u32 { return max((prim_u(idx, 3u) >> 16u) & 0xFFFFu, 1u); }
fn prim_object_id(idx: u32) -> u32 { return prim_u(idx, 82u); }
// Blend falloff data: [46]=dir.x*len, [47]=dir.y*len, [62]=dir.z*len, [63]=origin_offset
fn prim_falloff_packed(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 46u), prim_f(idx, 47u), prim_f(idx, 62u)); }
fn prim_falloff_offset(idx: u32) -> f32 { return prim_f(idx, 63u); }
// Modifier data: [64-67] modifier type/strength/axis/param2, [68-76] mask
fn prim_mod_type(idx: u32) -> u32 { return prim_u(idx, 64u); }
fn prim_mod_strength(idx: u32) -> f32 { return prim_f(idx, 65u); }
fn prim_mod_axis(idx: u32) -> u32 { return prim_u(idx, 66u); }
fn prim_mod_param2(idx: u32) -> f32 { return prim_f(idx, 67u); }
fn prim_mask_type(idx: u32) -> u32 { return prim_u(idx, 68u); }
fn prim_mask_center(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,69u), prim_f(idx,70u), prim_f(idx,71u)); }
fn prim_mask_extents(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,72u), prim_f(idx,73u), prim_f(idx,74u)); }
fn prim_mask_feather(idx: u32) -> f32 { return prim_f(idx, 75u); }
fn prim_mask_invert(idx: u32) -> u32 { return prim_u(idx, 76u); }
// Noise mask accessors [108-115]
fn prim_mask_noise_freq(idx: u32) -> f32 { return prim_f(idx, 108u); }
fn prim_mask_noise_octaves(idx: u32) -> u32 { return prim_u(idx, 109u); }
fn prim_mask_noise_lacunarity(idx: u32) -> f32 { return prim_f(idx, 110u); }
fn prim_mask_noise_gain(idx: u32) -> f32 { return prim_f(idx, 111u); }
fn prim_mask_noise_threshold(idx: u32) -> f32 { return prim_f(idx, 112u); }
fn prim_mask_noise_offset(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,113u), prim_f(idx,114u), prim_f(idx,115u)); }
// T1 image height-map atlas start (full layout [292]); 0xFFFFFFFF = no image (FBM displace)
fn prim_height_map_start(idx: u32) -> u32 { return prim_u(idx, 292u); }

// Phase B.1 shader panel accessors [84-104]
fn prim_shader_type(idx: u32) -> u32 { return prim_u(idx, 84u); }
fn prim_mat_quality(idx: u32) -> u32 { return prim_u(idx, 85u); }
fn prim_normal_strength(idx: u32) -> f32 { return prim_f(idx, 86u); }
fn prim_emission_color(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,87u),prim_f(idx,88u),prim_f(idx,89u)); }
fn prim_emission_intensity(idx: u32) -> f32 { return prim_f(idx, 90u); }
fn prim_subdivision(idx: u32) -> u32 { return prim_u(idx, 105u); }
fn prim_blend_profile(idx: u32) -> u32 { return prim_u(idx, 106u); }
// Seam depth (legacy field [291], op_type 5/6 only). Width rides blend_k [15],
// cross-section rides the blend_profile slot [106] (0=V,1=U,2=Square). Hot twin: hot[86].
fn prim_seam_depth(idx: u32) -> f32 { return prim_f(idx, 291u); }
// Scope marker AABB accessors (fields 0-2 = min, 8-10 = max)
fn scope_aabb_min(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,0u), prim_f(idx,1u), prim_f(idx,2u)); }
fn scope_aabb_max(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,8u), prim_f(idx,9u), prim_f(idx,10u)); }
/// Signed distance from point p to AABB (positive = outside, 0 = on surface, negative = inside).
fn sd_aabb(p: vec3<f32>, aabb_min: vec3<f32>, aabb_max: vec3<f32>) -> f32 {
    let q = max(aabb_min - p, p - aabb_max);
    return length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

// Body-mirror UNION helper: reflect `p` across the subset of enabled mirror axes
// selected by `combo`. `mflags` bit 0=X,1=Y,2=Z marks enabled axes; combo bit j
// (counting only enabled axes, low to high) toggles reflection of the j-th enabled
// axis across plane `mc[axis]` (p_axis = 2*mc_axis - p_axis). combo 0 = identity.
// The body mirror is a TRUE UNION over all 2^n combos, NOT a one-sided fold.
// Leaf function (no recursion) — inlines linearly, NOT the forbidden NOISY_BLEND chain.
fn reflect_for_combo(p_in: vec3<f32>, mflags: u32, mc: vec3<f32>, combo: u32) -> vec3<f32> {
    var p = p_in;
    var bit = 0u;
    if ((mflags & 1u) != 0u) {
        if ((combo >> bit) & 1u) != 0u { p.x = 2.0 * mc.x - p.x; }
        bit += 1u;
    }
    if ((mflags & 2u) != 0u) {
        if ((combo >> bit) & 1u) != 0u { p.y = 2.0 * mc.y - p.y; }
        bit += 1u;
    }
    if ((mflags & 4u) != 0u) {
        if ((combo >> bit) & 1u) != 0u { p.z = 2.0 * mc.z - p.z; }
        bit += 1u;
    }
    return p;
}

// Body-mirror CUT bit in mirror_sign — `body_field_ops::MIRROR_CUT_BIT`.
const MIRROR_CUT_BIT: u32 = 128u;

// Body-mirror CUT ("delete the other half"): fold `p` onto the SOURCE half of every enabled
// axis (minus side when that axis' flip bit is set in `msign`). ONE scope evaluation instead
// of the 2^n-combo union. CPU twin: `field_eval::fold_for_cut`; GPU-bake twin: the identical
// function in `volume_bake.wgsl` (all three must stay bit-for-bit equal).
fn fold_for_cut(p_in: vec3<f32>, mflags: u32, msign: u32, mc: vec3<f32>) -> vec3<f32> {
    var p = p_in;
    if ((mflags & 1u) != 0u) {
        let d = abs(p.x - mc.x);
        p.x = select(mc.x + d, mc.x - d, (msign & 1u) != 0u);
    }
    if ((mflags & 2u) != 0u) {
        let d = abs(p.y - mc.y);
        p.y = select(mc.y + d, mc.y - d, (msign & 2u) != 0u);
    }
    if ((mflags & 4u) != 0u) {
        let d = abs(p.z - mc.z);
        p.z = select(mc.z + d, mc.z - d, (msign & 4u) != 0u);
    }
    return p;
}
fn prim_has_node_graph(idx: u32) -> bool { return prim_f(idx, 107u) > 0.5; }

// #region DNA
// --- Material DNA accessors [116..279] ---
fn dna_rule_count(idx: u32) -> u32 { return u32(prim_f(idx, 116u)); }
fn dna_age(idx: u32) -> f32 { return prim_f(idx, 117u); }
// Rule field accessor: rule_idx [0..7], field [0..19]
fn dna_rf(prim_idx: u32, rule_idx: u32, field: u32) -> f32 {
    return prim_f(prim_idx, 120u + rule_idx * 20u + field);
}

// ============================================================
// Material DNA — Geometry-Responsive Rule Evaluator
// ============================================================

// Read geometric input value at a surface point
// Pre-computed geometry inputs for DNA evaluation (no SDF calls inside the loop).
// This prevents the Vulkan compiler from inlining scene_sdf into the DNA rule loop.
struct DnaGeoInputs {
    curv: f32,
    sdf_dist: f32,
    thickness: f32,
}

fn dna_read_input(input_id: u32, p: vec3<f32>, normal: vec3<f32>, geo: DnaGeoInputs, view_dir: vec3<f32>) -> f32 {
    switch (input_id) {
        case 0u: { return geo.curv; }                                   // Curvature (Laplacian)
        case 1u: { return select(0.0, 1.0, geo.curv > 0.0); }         // Convexity (binary)
        case 2u: { return clamp(-geo.curv * 0.1, 0.0, 1.0); }         // CavityDepth (neg curv → 0..1)
        case 3u: { return acos(clamp(normal.y, -1.0, 1.0)); }         // SlopeAngle (0=flat, PI/2=vert)
        case 4u: { return abs(dot(normal, view_dir)); }                // ViewAngle (0=grazing, 1=head-on)
        case 5u: { return noise3(p); }                                  // Noise3D (raw, caller scales)
        case 6u: { return p.y; }                                        // World Y
        case 7u: { return geo.sdf_dist; }                               // SDF Distance (pre-computed)
        case 8u: { return 0.0; }                                        // BoolBlend (TODO: wire seam data)
        case 9u: { return geo.thickness; }                               // Thickness (pre-computed)
        default: { return 0.0; }
    }
}

// Evaluate a single condition → mask value [0..1]
fn dna_eval_condition(prim_idx: u32, rule_idx: u32, p: vec3<f32>, normal: vec3<f32>, geo: DnaGeoInputs, view_dir: vec3<f32>) -> f32 {
    let cond_type = u32(dna_rf(prim_idx, rule_idx, 0u));
    let geo_input = u32(dna_rf(prim_idx, rule_idx, 1u));
    let pa = dna_rf(prim_idx, rule_idx, 2u);
    let pb = dna_rf(prim_idx, rule_idx, 3u);
    let pc = dna_rf(prim_idx, rule_idx, 4u); // falloff or direction.z

    switch (cond_type) {
        case 0u: { return 1.0; } // Always
        case 1u: { // Range: input in [min..max] with falloff
            let v = dna_read_input(geo_input, p, normal, geo, view_dir);
            let lo = smoothstep(pa - pc, pa, v);
            let hi = 1.0 - smoothstep(pb, pb + pc, v);
            return lo * hi;
        }
        case 2u: { // GreaterThan: input > threshold with falloff
            let v = dna_read_input(geo_input, p, normal, geo, view_dir);
            return smoothstep(pa - pc, pa + pc, v);
        }
        case 3u: { // LessThan: input < threshold with falloff
            let v = dna_read_input(geo_input, p, normal, geo, view_dir);
            return 1.0 - smoothstep(pa - pc, pa + pc, v);
        }
        case 4u: { // Direction: normal within angle of direction vector
            let dir = safe_dir(vec3(pa, pb, pc));
            let angle = dna_rf(prim_idx, rule_idx, 1u); // angle in radians
            let d = dot(normal, dir);
            let threshold = cos(angle);
            return smoothstep(threshold - 0.1, threshold, d);
        }
        case 5u: { // NoiseThreshold: noise at scale > threshold
            let scale = pa;
            let threshold = pb;
            let falloff = pc;
            let n = fbm3(p * scale, 4u);
            return smoothstep(threshold - falloff, threshold + falloff, n);
        }
        case 6u: { // AltitudeBand: world Y in [min_y..max_y]
            let lo = smoothstep(pa - pc, pa, p.y);
            let hi = 1.0 - smoothstep(pb, pb + pc, p.y);
            return lo * hi;
        }
        case 7u: { // BooleanZone: blend weight > min
            return select(0.0, 1.0, abs(geo.curv) > pa);
        }
        default: { return 0.0; }
    }
}

// Apply blend mode: blend the rule's surface into the running material
fn dna_blend(base: vec3<f32>, rule_col: vec3<f32>, mode: u32, mask: f32) -> vec3<f32> {
    var blended: vec3<f32>;
    switch (mode) {
        case 0u: { blended = rule_col; }                                    // Normal
        case 1u: { blended = base * rule_col; }                             // Multiply
        case 2u: { blended = 1.0 - (1.0 - base) * (1.0 - rule_col); }     // Screen
        case 3u: { blended = base + rule_col; }                             // Add
        default: { blended = rule_col; }
    }
    return mix(base, blended, mask);
}

// Evaluate full Material DNA stack → returns MatResult
fn eval_material_dna(hit_prim: u32, p: vec3<f32>, normal: vec3<f32>, view_dir: vec3<f32>) -> MatResult {
    var mat: MatResult;
    // Start with base material (albedo/roughness/metallic/opacity/emission)
    mat.color = prim_albedo(hit_prim);
    mat.roughness = prim_base_rough(hit_prim);
    mat.metallic = prim_base_metal(hit_prim);
    mat.opacity = prim_base_opacity(hit_prim);
    let em_int = prim_emission_intensity(hit_prim);
    mat.emission = select(vec3(0.0), prim_emission_color(hit_prim) * em_int, em_int > 0.001);

    let count = dna_rule_count(hit_prim);
    if (count == 0u) { return mat; }

    let age = dna_age(hit_prim);

    // Pre-compute ALL geometry inputs ONCE (no SDF calls inside the rule loop).
    // This prevents the Vulkan driver from inlining scene_sdf into every rule iteration.
    let quality = config.render_quality;
    let mat_q = prim_mat_quality(hit_prim);
    let eff_q = select(quality, 4u, mat_q == 2u);
    let curv_eps = select(0.015, select(0.01, 0.005, eff_q >= 3u), eff_q >= 2u);
    var geo: DnaGeoInputs;
    geo.curv = calc_curvature(p, curv_eps);
    geo.sdf_dist = scene_sdf(p);
    let interior = p - normal * 0.05;
    geo.thickness = abs(scene_sdf(interior));

    // Evaluate rules bottom-to-top (rule 0 is base, rule N is topmost)
    for (var i = 0u; i < count; i++) {
        // Age gate: skip if age is outside [age_min, age_max]
        let age_min = dna_rf(hit_prim, i, 5u);
        let age_max = dna_rf(hit_prim, i, 6u);
        let age_mask = smoothstep(age_min - 0.05, age_min, age) * (1.0 - smoothstep(age_max, age_max + 0.05, age));
        if (age_mask < 0.001) { continue; }

        // Slot 19: negative = invisible, positive = enable bitmask
        let slot19 = dna_rf(hit_prim, i, 19u);
        if (slot19 < -0.5) { continue; }
        let en_bits = u32(max(slot19, 0.0));

        // Evaluate condition
        var mask = dna_eval_condition(hit_prim, i, p, normal, geo, view_dir);
        mask *= dna_rf(hit_prim, i, 15u); // intensity
        mask *= age_mask;

        if (mask < 0.001) { continue; }

        // Read rule surface + apply only enabled properties
        let blend_mode = u32(dna_rf(hit_prim, i, 16u));
        // bit 0 = albedo
        if ((en_bits & 1u) != 0u) {
            let rule_albedo = vec3(dna_rf(hit_prim,i,7u), dna_rf(hit_prim,i,8u), dna_rf(hit_prim,i,9u));
            mat.color = dna_blend(mat.color, rule_albedo, blend_mode, mask);
        }
        // bit 1 = roughness
        if ((en_bits & 2u) != 0u) {
            mat.roughness = mix(mat.roughness, dna_rf(hit_prim, i, 10u), mask);
        }
        // bit 2 = metalness
        if ((en_bits & 4u) != 0u) {
            mat.metallic = mix(mat.metallic, dna_rf(hit_prim, i, 11u), mask);
        }
        // bit 3 = emission
        if ((en_bits & 8u) != 0u) {
            let rule_emission = vec3(dna_rf(hit_prim,i,12u), dna_rf(hit_prim,i,13u), dna_rf(hit_prim,i,14u));
            if (length(rule_emission) > 0.001) {
                mat.emission = mix(mat.emission, rule_emission, mask);
            }
        }
    }

    return mat;
}
// #endregion DNA

// #region LEGACY_TEXTURE
// ============================================================
// Tri-planar Texture Sampling (AI-generated texture on mesh)
// ============================================================
fn tex_fetch(ix: u32, iy: u32) -> vec3<f32> {
    let size = config.tex_size;
    if (size == 0u) { return vec3(0.5, 0.5, 0.5); }
    let cx = ix % size;
    let cy = iy % size;
    let packed = tex_data[cy * size + cx];
    let r = f32((packed >> 24u) & 0xFFu) / 255.0;
    let g = f32((packed >> 16u) & 0xFFu) / 255.0;
    let b = f32((packed >> 8u) & 0xFFu) / 255.0;
    return vec3(r, g, b);
}

fn tex_sample_bilinear(uv: vec2<f32>) -> vec3<f32> {
    let size = f32(config.tex_size);
    let fuv = fract(uv) * size;
    let ix = u32(fuv.x);
    let iy = u32(fuv.y);
    let fx = fract(fuv.x);
    let fy = fract(fuv.y);
    let c00 = tex_fetch(ix, iy);
    let c10 = tex_fetch(ix + 1u, iy);
    let c01 = tex_fetch(ix, iy + 1u);
    let c11 = tex_fetch(ix + 1u, iy + 1u);
    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

fn sample_triplanar(p: vec3<f32>, normal: vec3<f32>, tiling: f32) -> vec3<f32> {
    // Blending weights from absolute normal
    var w = abs(normal);
    w = pow(w, vec3(4.0)); // sharper blend (reduces stretching artifacts)
    w = w / (w.x + w.y + w.z + 0.0001);
    // Sample from 3 projections
    let uv_x = p.yz * tiling; // YZ plane
    let uv_y = p.xz * tiling; // XZ plane (top-down)
    let uv_z = p.xy * tiling; // XY plane
    return tex_sample_bilinear(uv_x) * w.x
         + tex_sample_bilinear(uv_y) * w.y
         + tex_sample_bilinear(uv_z) * w.z;
}
// #endregion LEGACY_TEXTURE

fn quat_inv_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let qi = vec4(-q.x, -q.y, -q.z, q.w);
    let t = 2.0 * cross(qi.xyz, v);
    return v + qi.w * t + cross(qi.xyz, t);
}

// #region LEGACY_NOISE
// ============================================================
// 3D Noise (value noise + FBM)
// ============================================================
fn hash31(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash31(i + vec3(0.0,0.0,0.0)), hash31(i + vec3(1.0,0.0,0.0)), u.x),
            mix(hash31(i + vec3(0.0,1.0,0.0)), hash31(i + vec3(1.0,1.0,0.0)), u.x), u.y),
        mix(mix(hash31(i + vec3(0.0,0.0,1.0)), hash31(i + vec3(1.0,0.0,1.0)), u.x),
            mix(hash31(i + vec3(0.0,1.0,1.0)), hash31(i + vec3(1.0,1.0,1.0)), u.x), u.y),
        u.z);
}

fn fbm3(p: vec3<f32>, octaves: u32) -> f32 {
    var sum = 0.0; var amp = 0.5; var pos = p;
    let max_oct = min(octaves, 6u);
    for (var i = 0u; i < max_oct; i++) {
        sum += amp * noise3(pos);
        pos = pos * 2.01;
        amp *= 0.5;
    }
    return sum;
}
// #endregion LEGACY_NOISE

// #region CURVATURE
// ============================================================
// SDF Curvature (Laplacian) — detects edges vs cavities
// ============================================================
// TANGENT-PLANE mean-curvature Laplacian (2026-07-27 fix). Mirrors CPU
// `SdfFieldSources::calc_curvature` (app/src/node_graph_bake.rs) + `tangent_basis` — SAME
// tangent-basis formula (Duff/Hughes-Möller), SAME 4-tap order, so CPU/GPU stay in parity
// (`tests_gpu_curvature.rs` golden gate). The old 6-neighbour WORLD-AXIS stencil second-
// differenced along fixed X/Y/Z regardless of local surface orientation: at large `e` (e.g. the
// owner's Cavity radius 0.190) the tap along whichever axis happened to be TANGENT to the
// surface escaped the local feature and sampled unrelated geometry, flipping the sign so carved
// cavities read convex. Mean curvature only lives IN the tangent plane, so tap only there.
// Sign PROVED empirically on known geometry (see the CPU doc comment): raw tangent-plane
// Laplacian already reads positive-on-convex / negative-on-concave — no extra flip needed.
fn calc_curvature_tangent_basis(n: vec3<f32>) -> mat2x3<f32> {
    // n need not be pre-normalized by the caller here — callers pass a normalized gradient.
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t1 = vec3(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let t2 = vec3(b, s + n.y * n.y * a, -n.y);
    return mat2x3<f32>(t1, t2);
}
// ONE `scene_sdf` call site (2026-10-13). The 11 taps used to be 11 separate calls; the driver
// inlines every call, and `calc_curvature` itself is reached from ~8 material-eval sites in
// DF-full, so the curvature probe alone expanded to ~240K of DF-full's ~307K inlined statements
// and the NVIDIA cold compile never finished (>5 min, tens of GB). A loop over the SAME taps in
// the SAME order with the SAME summation keeps the value identical while the inlined size drops
// 11×. Tap order: 0 = centre, 1..6 = ±x ±y ±z gradient, 7..10 = ±t1 ±t2 Laplacian (the tangent
// basis is built once, at tap 7, from the finished gradient). Do NOT unroll back into calls.
fn calc_curvature(p: vec3<f32>, e: f32) -> f32 {
    // Gradient step for the surface normal: same scale discipline as CPU (`sdf_grad` clamps to
    // [1e-4, 0.01] regardless of the (possibly large) Laplacian tap step `e`), so the normal
    // estimate stays a LOCAL probe even when `e` itself is large.
    let ge = clamp(e, 1e-4, 0.01);
    var d = 0.0;
    var g = vec3(0.0);
    var t1 = vec3(0.0);
    var t2 = vec3(0.0);
    var lap = 0.0;
    for (var i = 0u; i < 11u; i++) {
        var q = p;
        if (i >= 1u && i <= 6u) {
            q = p + sdf_central_diff_offset(i, ge); // same stencil as `sdf_central_gradient`
        } else if (i >= 7u) {
            if (i == 7u) {
                let gl = length(g);
                // Degenerate gradient (∇f ≈ 0) — stable fallback basis instead of NaN/Inf.
                let n = select(vec3(0.0, 0.0, 1.0), g / max(gl, 1e-6), gl > 1e-6);
                let tb = calc_curvature_tangent_basis(n);
                t1 = tb[0];
                t2 = tb[1];
            }
            let t = select(t2, t1, i <= 8u);
            q = select(p - e * t, p + e * t, (i & 1u) == 1u);
        }
        let s = scene_sdf(q);
        if (i == 0u) {
            d = s;
        } else if (i <= 6u) {
            let axis = (i - 1u) / 2u;
            // gx = f(+x) - f(-x): the + tap (odd) lands first, the − tap subtracts.
            g[axis] = select(g[axis] - s, s, (i & 1u) == 1u);
        } else {
            lap = lap + s;
        }
    }
    lap = lap - 4.0 * d;
    return lap / (e * e);
}
// #endregion CURVATURE

// ============================================================
// Analytical SDF shapes
// ============================================================
fn sdf_sphere(p: vec3<f32>, radius: f32) -> f32 { return length(p) - radius; }
fn sdf_box(p: vec3<f32>, half_ext: vec3<f32>) -> f32 {
    let q = abs(p) - half_ext;
    return length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}
// 2D bevel helper: computes rounded/chamfered/sharp corner in one plane.
fn bevel_2d(a: f32, b: f32, r: f32, m: u32) -> f32 {
    if (m == 1u) { // Round
        let q = vec2(a + r, b + r);
        return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    } else if (m == 2u) { // Chamfer — EXACT 2D prism SDF (BEVEL-NOTCH fix 2026-07-17).
        // The bare plane-max form underestimates near the two chamfer vertices (stripe
        // fuel) and breaks the corner term's octant-boundary domination bound. The two
        // vertex sectors use the true vertex distance. Zero sign change (sectors are
        // strictly outside). KEEP LINE-PARALLEL with CPU sdf_shapes.rs + volume_bake.wgsl.
        if (a >= b + r && b + r >= 0.0) { return length(vec2(a, b + r)); }
        if (b >= a + r && a + r >= 0.0) { return length(vec2(a + r, b)); }
        return max(max(a, b), (a + b + r) * 0.7071067811865476);
    } else { // None (sharp corner)
        let q = vec2(a, b);
        return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0);
    }
}

// Box SDF with 12 independent per-edge bevels — tri-planar formulation + corner term.
// Computes 3 independent 2D bevels (XZ, XY, YZ planes), combines with max(), then maxes
// in an EXACT uniform roundbox-rho corner field (rho = min effective octant radius).
// BEVEL-NOTCH fix (2026-07-17): the bare tri-planar max UNDERESTIMATES distance in
// 3-edge corner sectors (up to ~22%) with C0 ridge surfaces where two plane fields tie —
// the deferred soft_shadow/calc_ao march painted those as vertical stripe bands on carved
// surfaces (BEVEL.md §6 #18 + §11 2026-07-17). max(planes, corner) is continuous and
// 1-Lipschitz (march-safe, never overestimates its own zero set), reduces EXACTLY to
// sdRoundBox when all 12 edges are Round with one radius, and keeps sharp corners sharp
// (any None edge in the octant -> rho = 0 -> exact sharp-box corner combine). Surface
// moves ONLY inside corner blend neighborhoods (old tri-cylinder bulge <= 0.226*rho
// shaved to the rho-sphere). KEEP LINE-PARALLEL with CPU sdf_shapes.rs + volume_bake.wgsl.
// GPU data: floats [48-59]=edge radii 0-11, float[60]=packed modes (2 bits each)
// + up_axis (bits 24-25) + algo (bits 26-27: 0=PerEdge, 1=Uniform).
fn sdf_box_per_edge(p: vec3<f32>, b: vec3<f32>, idx: u32) -> f32 {
    let mode_bits = prim_bev_modes(idx);
    // Permute coordinates based on bevel up_axis (bits 24-25): 0=Y, 1=X, 2=Z
    let up_axis = (mode_bits >> 24u) & 3u;
    let algo = (mode_bits >> 26u) & 3u;
    var bp = p;
    var bh = b;
    if (up_axis == 1u) { bp = vec3(p.y, p.x, p.z); bh = vec3(b.y, b.x, b.z); }       // X up
    else if (up_axis == 2u) { bp = vec3(p.x, p.z, p.y); bh = vec3(b.x, b.z, b.y); }   // Z up
    // Uniform: exact sdRoundBox (best when all edges share one radius).
    if (algo == 1u) {
        var rmax = 0.0;
        for (var e = 0u; e < 12u; e++) {
            let m = (mode_bits >> (e * 2u)) & 3u;
            if (m != 0u) { rmax = max(rmax, prim_bev_edge_r(idx, e)); }
        }
        let min_he = min(bh.x, min(bh.y, bh.z));
        rmax = min(max(rmax, 0.0), min_he);
        let q = abs(bp) - bh + vec3(rmax);
        return length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0) - rmax;
    }
    let qx = abs(bp.x) - bh.x;
    let qy = abs(bp.y) - bh.y;
    let qz = abs(bp.z) - bh.z;
    // Determine octant → look up 3 edge indices (one per plane)
    let xi = select(0u, 1u, bp.x > 0.0);
    let zi = select(0u, 1u, bp.z > 0.0);
    let base_y = select(0u, 4u, bp.y > 0.0);
    let e_xz = 8u + xi + zi * 2u;       // XZ plane (vertical edge, along Y)
    let e_xy = base_y + 2u + xi;         // XY plane (edge along Z)
    let e_yz = base_y + zi;              // YZ plane (edge along X)
    // Compute 3 independent 2D bevels (clamp radius to smallest half-extent — matches CPU path)
    let min_he = min(bh.x, min(bh.y, bh.z));
    let m_xz = (mode_bits >> (e_xz * 2u)) & 3u;
    let m_xy = (mode_bits >> (e_xy * 2u)) & 3u;
    let m_yz = (mode_bits >> (e_yz * 2u)) & 3u;
    let r_xz = min(prim_bev_edge_r(idx, e_xz), min_he);
    let r_xy = min(prim_bev_edge_r(idx, e_xy), min_he);
    let r_yz = min(prim_bev_edge_r(idx, e_yz), min_he);
    let d_xz = bevel_2d(qx, qz, r_xz, m_xz);
    let d_xy = bevel_2d(qx, qy, r_xy, m_xy);
    let d_yz = bevel_2d(qy, qz, r_yz, m_yz);
    // Corner term: rho = min effective radius (None -> 0), exact roundbox-rho combine.
    let eff_xz = select(0.0, max(r_xz, 0.0), m_xz != 0u);
    let eff_xy = select(0.0, max(r_xy, 0.0), m_xy != 0u);
    let eff_yz = select(0.0, max(r_yz, 0.0), m_yz != 0u);
    let rho = min(eff_xz, min(eff_xy, eff_yz));
    let qc = vec3(qx, qy, qz) + vec3(rho);
    let d_corner = length(max(qc, vec3(0.0))) + min(max(qc.x, max(qc.y, qc.z)), 0.0) - rho;
    return max(max(d_xz, max(d_xy, d_yz)), d_corner);
}
fn sdf_cylinder(p: vec3<f32>, radius: f32, half_h: f32) -> f32 {
    let d_r = length(vec2(p.x, p.z)) - radius;
    let d_h = abs(p.y) - half_h;
    return length(max(vec2(d_r, d_h), vec2(0.0))) + min(max(d_r, d_h), 0.0);
}
fn sdf_cylinder_beveled(p: vec3<f32>, radius: f32, half_h: f32, idx: u32) -> f32 {
    let d_r = length(vec2(p.x, p.z)) - radius;
    let d_top = p.y - half_h;
    let d_bot = -p.y - half_h;
    let r_top = prim_bev_edge_r(idx, 0u);
    let r_bot = prim_bev_edge_r(idx, 1u);
    let mb = prim_bev_modes(idx);
    let m_top = mb & 3u;
    let m_bot = (mb >> 2u) & 3u;
    let d_top_edge = bevel_2d(d_r, d_top, r_top, m_top);
    let d_bot_edge = bevel_2d(d_r, d_bot, r_bot, m_bot);
    return max(d_top_edge, d_bot_edge);
}

// ---- Profile Bevel (continuous -1..+1: concave → chamfer → fillet) ----
fn prim_bevel_profile_top(idx: u32) -> f32 { return prim_f(idx, 118u); }
fn prim_bevel_profile_bot(idx: u32) -> f32 { return prim_f(idx, 119u); }
fn prim_bevel_profile_vert(idx: u32) -> f32 { return prim_f(idx, 280u); }
fn prim_chamfer_angle_top(idx: u32) -> f32 { return prim_f(idx, 281u); }
fn prim_chamfer_angle_bot(idx: u32) -> f32 { return prim_f(idx, 282u); }
fn prim_chamfer_angle_vert(idx: u32) -> f32 { return prim_f(idx, 283u); }
// Blend-zone noise displacement (slots 284-287)
fn prim_blend_noise_amp(idx: u32) -> f32 { return prim_f(idx, 284u); }
fn prim_blend_noise_freq(idx: u32) -> f32 { return prim_f(idx, 285u); }
fn prim_blend_noise_octaves(idx: u32) -> u32 { return prim_u(idx, 286u); }
fn prim_blend_noise_type(idx: u32) -> u32 { return prim_u(idx, 287u); }
// Elongation ([288-290]) — domain warp extending shape along axes
fn prim_elongation(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 288u), prim_f(idx, 289u), prim_f(idx, 290u)); }
fn edge_profile(idx: u32, edge: u32) -> f32 {
    if (edge >= 8u) { return prim_bevel_profile_vert(idx); }
    if (edge >= 4u) { return prim_bevel_profile_top(idx); }
    return prim_bevel_profile_bot(idx);
}
fn edge_chamfer_angle(idx: u32, edge: u32) -> f32 {
    if (edge >= 8u) { return prim_chamfer_angle_vert(idx); }
    if (edge >= 4u) { return prim_chamfer_angle_top(idx); }
    return prim_chamfer_angle_bot(idx);
}
fn has_bevel_profile(idx: u32) -> bool {
    return abs(prim_bevel_profile_top(idx)) > 0.001 || abs(prim_bevel_profile_bot(idx)) > 0.001 || abs(prim_bevel_profile_vert(idx)) > 0.001;
}

fn bevel_2d_profiled(a: f32, b: f32, r: f32, profile: f32, chamfer_angle_deg: f32) -> f32 {
    if (r <= 0.0) { return max(a, b); }
    let theta = radians(chamfer_angle_deg);
    let st = sin(theta);
    let ct = cos(theta);
    let d_chamfer = max(max(a, b), st * (a + r) + ct * b);
    if (profile >= 0.0) {
        // Convex: chamfer → round fillet
        let q = max(vec2(a + r, b + r), vec2(0.0));
        let d_round = length(q) + min(max(a + r, b + r), 0.0) - r;
        return mix(d_chamfer, d_round, profile);
    } else {
        // Concave: chamfer → inward scoop
        let u = max(vec2(r + a, r + b), vec2(0.0));
        let d_concave = max(a, b) - (r - length(u));
        return mix(d_chamfer, d_concave, -profile);
    }
}

fn sdf_box_per_edge_profiled(p: vec3<f32>, b: vec3<f32>, idx: u32) -> f32 {
    let mode_bits = prim_bev_modes(idx);
    let up_axis = (mode_bits >> 24u) & 3u;
    var bp = p;
    var bh = b;
    if (up_axis == 1u) { bp = vec3(p.y, p.x, p.z); bh = vec3(b.y, b.x, b.z); }
    else if (up_axis == 2u) { bp = vec3(p.x, p.z, p.y); bh = vec3(b.x, b.z, b.y); }
    let qx = abs(bp.x) - bh.x;
    let qy = abs(bp.y) - bh.y;
    let qz = abs(bp.z) - bh.z;
    let xi = select(0u, 1u, bp.x > 0.0);
    let zi = select(0u, 1u, bp.z > 0.0);
    let base_y = select(0u, 4u, bp.y > 0.0);
    let e_xz = 8u + xi + zi * 2u;
    let e_xy = base_y + 2u + xi;
    let e_yz = base_y + zi;
    let min_he = min(bh.x, min(bh.y, bh.z));
    let d_xz = bevel_2d_profiled(qx, qz, min(prim_bev_edge_r(idx, e_xz), min_he), edge_profile(idx, e_xz), edge_chamfer_angle(idx, e_xz));
    let d_xy = bevel_2d_profiled(qx, qy, min(prim_bev_edge_r(idx, e_xy), min_he), edge_profile(idx, e_xy), edge_chamfer_angle(idx, e_xy));
    let d_yz = bevel_2d_profiled(qy, qz, min(prim_bev_edge_r(idx, e_yz), min_he), edge_profile(idx, e_yz), edge_chamfer_angle(idx, e_yz));
    return max(d_xz, max(d_xy, d_yz));
}

fn sdf_cylinder_beveled_profiled(p: vec3<f32>, radius: f32, half_h: f32, idx: u32) -> f32 {
    let d_r = length(vec2(p.x, p.z)) - radius;
    let d_top = p.y - half_h;
    let d_bot = -p.y - half_h;
    let r_top = prim_bev_edge_r(idx, 0u);
    let r_bot = prim_bev_edge_r(idx, 1u);
    let d_top_edge = bevel_2d_profiled(d_r, d_top, r_top, prim_bevel_profile_top(idx), prim_chamfer_angle_top(idx));
    let d_bot_edge = bevel_2d_profiled(d_r, d_bot, r_bot, prim_bevel_profile_bot(idx), prim_chamfer_angle_bot(idx));
    return max(d_top_edge, d_bot_edge);
}

fn sdf_torus(p: vec3<f32>, major: f32, minor: f32) -> f32 {
    let q = vec2(length(vec2(p.x, p.z)) - major, p.y);
    return length(q) - minor;
}
fn sdf_cone(p: vec3<f32>, radius: f32, half_h: f32) -> f32 {
    let q_len = length(vec2(p.x, p.z));
    let norm_f = sqrt(4.0 * half_h * half_h + radius * radius);
    let d_side = (2.0 * half_h * q_len + radius * (p.y - half_h)) / norm_f;
    return max(d_side, abs(p.y) - half_h);
}
fn sdf_capsule(p: vec3<f32>, radius: f32, half_h: f32) -> f32 {
    let closest = vec3(0.0, clamp(p.y, -half_h, half_h), 0.0);
    return length(p - closest) - radius;
}

fn sdf_stair(p: vec3<f32>, he: vec3<f32>, n: u32) -> f32 {
    // Box with stepped cutouts: steps go along Z, rising along Y
    let qx = abs(p.x) - he.x;
    let qy = abs(p.y) - he.y;
    let qz = abs(p.z) - he.z;
    let d_box = length(max(vec3(qx, qy, qz), vec3(0.0))) + min(max(qx, max(qy, qz)), 0.0);
    let nf = f32(n);
    let step_h = he.y * 2.0 / nf;
    let step_d = he.z * 2.0 / nf;
    var cut = 1e10f;
    for (var i = 0u; i < n; i++) {
        let fi = f32(i);
        let y_top = -he.y + (fi + 1.0) * step_h;
        let z_front = -he.z + (fi + 1.0) * step_d;
        let c = max(p.y - y_top, z_front - p.z);
        cut = min(cut, c);
    }
    return max(d_box, cut);
}

fn sdf_spiral_stair(p: vec3<f32>, radius: f32, half_h: f32, n: u32, step_w: f32) -> f32 {
    let nf = f32(n);
    let step_angle = 6.283185307 / nf;
    let step_h = half_h * 2.0 / nf;
    let angle = atan2(p.z, p.x);
    let norm_angle = (angle + 3.141592653) / step_angle;
    let base_idx = i32(floor(norm_angle));
    var d = 1e10f;
    for (var offset = -1; offset <= 1; offset++) {
        let si = base_idx + offset;
        let a = f32(si) * step_angle - 3.141592653;
        let y_base = -half_h + f32(si) * step_h;
        let ca = cos(a); let sa = sin(a);
        let local_x = ca * p.x + sa * p.z - radius * 0.5;
        let local_z = -sa * p.x + ca * p.z;
        let local_y = p.y - y_base - step_h * 0.5;
        let he_step = vec3(radius * 0.5, step_h * 0.5, step_w * 0.5);
        let q = abs(vec3(local_x, local_y, local_z)) - he_step;
        let step_d = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
        d = min(d, step_d);
    }
    // Central column
    let col_r = length(vec2(p.x, p.z)) - 0.08;
    let col_h = abs(p.y) - half_h;
    let column = length(max(vec2(col_r, col_h), vec2(0.0))) + min(max(col_r, col_h), 0.0);
    return min(d, column);
}

// ============================================================
// 3D Noise (hash-based value noise + fBm)
// ============================================================
fn hash3d_gpu(p: vec3<f32>) -> f32 {
    var h = dot(p, vec3(127.1, 311.7, 74.7));
    h = sin(h) * 43758.5453;
    return fract(h);
}

fn noise3d_gpu(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - f * 2.0);
    let n000 = hash3d_gpu(i);
    let n100 = hash3d_gpu(i + vec3(1.0, 0.0, 0.0));
    let n010 = hash3d_gpu(i + vec3(0.0, 1.0, 0.0));
    let n110 = hash3d_gpu(i + vec3(1.0, 1.0, 0.0));
    let n001 = hash3d_gpu(i + vec3(0.0, 0.0, 1.0));
    let n101 = hash3d_gpu(i + vec3(1.0, 0.0, 1.0));
    let n011 = hash3d_gpu(i + vec3(0.0, 1.0, 1.0));
    let n111 = hash3d_gpu(i + vec3(1.0, 1.0, 1.0));
    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}

fn fbm3d_gpu(p: vec3<f32>, octaves: u32, lacunarity: f32, gain: f32) -> f32 {
    var val = 0.0;
    var amp = 1.0;
    var freq = 1.0;
    var max_amp = 0.0;
    for (var i = 0u; i < min(octaves, 8u); i++) {
        val += noise3d_gpu(p * freq) * amp;
        max_amp += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    if (max_amp > 0.001) { return (val / max_amp) * 2.0 - 1.0; }
    return 0.0;
}

// ============================================================
// Modifier system — domain warps + surface alterations + volumetric masks
// ============================================================
fn gpu_smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if (abs(e1 - e0) < 1e-6) { return select(0.0, 1.0, x >= e0); }
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn eval_mask(idx: u32, p: vec3<f32>) -> f32 {
    let mt = prim_mask_type(idx);
    if (mt == 0u) { return 1.0; }
    let center = prim_mask_center(idx);
    let extents = prim_mask_extents(idx);
    let feather = prim_mask_feather(idx);
    let inv = prim_mask_invert(idx);
    var w = 1.0;
    if (mt == 1u) { // Sphere
        let d = length(p - center);
        let r = max(extents.x, 0.001);
        w = gpu_smoothstep(r, r * (1.0 - max(feather, 0.001)), d);
    } else if (mt == 2u) { // Box
        let ext = max(extents, vec3(0.001));
        let q = abs(p - center) / ext;
        let d = max(q.x, max(q.y, q.z));
        w = gpu_smoothstep(1.0, 1.0 - max(feather, 0.001), d);
    } else if (mt == 3u) { // Gradient
        let fl = length(extents);
        if (fl < 0.001) { return 1.0; }
        let dir = extents / fl;
        let t = clamp(dot(p - center, dir) / fl, 0.0, 1.0);
        w = t * t * (3.0 - 2.0 * t);
    } else if (mt == 4u) { // Noise 3D
        let nf = prim_mask_noise_freq(idx);
        let no = prim_mask_noise_octaves(idx);
        let nl = prim_mask_noise_lacunarity(idx);
        let ng = prim_mask_noise_gain(idx);
        let nt = prim_mask_noise_threshold(idx);
        let noff = prim_mask_noise_offset(idx);
        let np = (p - center + noff) * nf;
        let n = fbm3d_gpu(np, no, nl, ng);
        let n01 = clamp(n * 0.5 + 0.5, 0.0, 1.0);
        let half_f = max(feather, 0.001) * 0.5;
        let lo = max(nt - half_f, 0.0);
        let hi = min(nt + half_f, 1.0);
        w = gpu_smoothstep(lo, hi, n01);
    }
    if (inv != 0u) { w = 1.0 - w; }
    return w;
}

fn apply_twist_wgsl(p: vec3<f32>, angle_per_unit: f32, axis: u32) -> vec3<f32> {
    var a = 0.0;
    if (axis == 1u) { a = p.x * angle_per_unit; }
    else if (axis == 2u) { a = p.z * angle_per_unit; }
    else { a = p.y * angle_per_unit; }
    let s = sin(a); let c = cos(a);
    if (axis == 1u) { return vec3(p.x, c*p.y - s*p.z, s*p.y + c*p.z); }
    else if (axis == 2u) { return vec3(c*p.x - s*p.y, s*p.x + c*p.y, p.z); }
    else { return vec3(c*p.x - s*p.z, p.y, s*p.x + c*p.z); }
}

fn apply_taper_wgsl(p: vec3<f32>, strength: f32, axis: u32, end_scale: f32) -> vec3<f32> {
    var axis_val = p.y;
    if (axis == 1u) { axis_val = p.x; }
    else if (axis == 2u) { axis_val = p.z; }
    let t = axis_val * strength;
    let sc = max(1.0 + t * (end_scale - 1.0), 0.01);
    if (axis == 1u) { return vec3(p.x, p.y / sc, p.z / sc); }
    else if (axis == 2u) { return vec3(p.x / sc, p.y / sc, p.z); }
    else { return vec3(p.x / sc, p.y, p.z / sc); }
}

fn apply_shear_wgsl(p: vec3<f32>, strength: f32, axis: u32, drive_axis: u32) -> vec3<f32> {
    var drive_val = p.y;
    if (drive_axis == 1u) { drive_val = p.x; }
    else if (drive_axis == 2u) { drive_val = p.z; }
    let offset = drive_val * strength;
    if (axis == 1u) { return vec3(p.x + offset, p.y, p.z); }
    else if (axis == 2u) { return vec3(p.x, p.y, p.z + offset); }
    else { return vec3(p.x, p.y + offset, p.z); }
}

fn eval_prim(idx: u32, p: vec3<f32>) -> f32 {
    let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
    let sc = prim_scale(idx);
    var sp = local_p / sc;
    // Mirror modifier: fold space per axis with adjustable center (matches CPU world_distance)
    let mb = prim_mirror_bits(idx);
    let mc = prim_mirror_center(idx);
    if ((mb & 1u) != 0u) { sp.x = abs(sp.x - mc.x) + mc.x; }
    if ((mb & 2u) != 0u) { sp.y = abs(sp.y - mc.y) + mc.y; }
    if ((mb & 4u) != 0u) { sp.z = abs(sp.z - mc.z) + mc.z; }
    // Elongation domain warp: extend shape along axes preserving cross-section
    let elong = prim_elongation(idx);
    if (elong.x > 0.0) { sp.x = sp.x - clamp(sp.x, -elong.x, elong.x); }
    if (elong.y > 0.0) { sp.y = sp.y - clamp(sp.y, -elong.y, elong.y); }
    if (elong.z > 0.0) { sp.z = sp.z - clamp(sp.z, -elong.z, elong.z); }
    // Domain warp modifiers (Twist, Taper, Shear): transform coordinates before shape eval
    let mod_t = prim_mod_type(idx);
    var eval_p = sp;
    if (mod_t == 1u || mod_t == 2u || mod_t == 6u) {
        let mask_w = eval_mask(idx, sp) * prim_mod_strength(idx);
        let ax = prim_mod_axis(idx);
        if (mod_t == 1u) { eval_p = apply_twist_wgsl(sp, mask_w, ax); }
        else if (mod_t == 2u) { eval_p = apply_taper_wgsl(sp, mask_w, ax, prim_mod_param2(idx)); }
        else if (mod_t == 6u) { eval_p = apply_shear_wgsl(sp, mask_w, ax, u32(prim_mod_param2(idx))); }
    }
    let min_s = min(sc.x, min(sc.y, sc.z));
    let st = prim_shape_type(idx);
    let p0 = prim_f(idx, 12u); let p1 = prim_f(idx, 13u); let p2 = prim_f(idx, 14u);
    var d = 0.0;
    if (st == 0u) { d = sdf_sphere(eval_p, p0); }
    else if (st == 1u) {
        if (has_bevel_profile(idx)) { d = sdf_box_per_edge_profiled(eval_p, vec3(p0, p1, p2), idx); }
        else if (prim_bev_modes(idx) == 0u) { d = sdf_box(eval_p, vec3(p0, p1, p2)); }
        else { d = sdf_box_per_edge(eval_p, vec3(p0, p1, p2), idx); }
    }

    else if (st == 2u) {
        if (has_bevel_profile(idx)) { d = sdf_cylinder_beveled_profiled(eval_p, p0, p1, idx); }
        else if (prim_bev_modes(idx) == 0u) { d = sdf_cylinder(eval_p, p0, p1); }
        else { d = sdf_cylinder_beveled(eval_p, p0, p1, idx); }
    }
    else if (st == 3u) { d = sdf_torus(eval_p, p0, p1); }
    else if (st == 4u) { d = sdf_cone(eval_p, p0, p1); }
    else if (st == 5u) { d = sdf_capsule(eval_p, p0, p1); }
    else if (st == 6u) { d = sdf_stair(eval_p, vec3(p0, p1, p2), prim_stair_steps(idx)); }
    else if (st == 7u) { d = sdf_spiral_stair(eval_p, p0, p1, prim_stair_steps(idx), p2); }
    else if (st == 8u) { // Billboard: thin slab
        let he = vec3(p0 * 0.5, p1 * 0.5, 0.005);
        let q = abs(eval_p) - he;
        d = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
    }
    else if (st == 9u) {
        // CurveExtrude: real profile eval lives in prim_outline (clip_curves side buffer).
        // Fail CLOSED here (outside) so a missing outline override never floods the viewport
        // with d=0. Never use a sphere stand-in (No Hacks rule).
        d = 1e6;
    }
    else if (st == 10u) { // MeshVolume: analytical path can't sample the per-prim grid (bound in
        // the bake only), so draw the grid's BOUNDING BOX as the silhouette/ghost. params[12-14]
        // hold the conservative half-extent (serialize.rs). Bounds the prim_outline ghost so a
        // MeshVolume cutter no longer marches as "everywhere" (d=0) and floods the viewport.
        let he = vec3(p0, p1, p2);
        let q = abs(eval_p) - he;
        d = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
    }
    else if (st == 12u) {
        // SpinePipe points live in the clip side buffer (bake-only). Fail CLOSED exterior
        // like CurveExtrude — never a sphere stand-in (No Hacks rule).
        d = 1e6;
    }
    else if (st == 11u) { // HalfSpace / slice: infinite plane through origin, normal = local +Z
        d = eval_p.z;
    }
    // Surface alteration modifiers (Smooth, Displace, Sculpt): adjust distance after shape eval
    if (mod_t == 3u) {
        let mask_w = eval_mask(idx, sp);
        d -= prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 4u) {
        let mask_w = eval_mask(idx, sp);
        let freq = max(prim_mod_param2(idx), 0.1);
        let hm_start = prim_height_map_start(idx);
        var noise_val: f32;
        if (hm_start != 0xFFFFFFFFu) {
            noise_val = sample_height_map_live(hm_start, sp, freq);
        } else {
            noise_val = fbm_wgsl(sp * freq, 4u);
        }
        d += noise_val * prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 7u) {
        // Sculpt modifier: param2 encodes SculptOp (0=Push,1=Pull,2=Flatten,3=Inflate,4=Pinch,5=Crease)
        let mask_w = eval_mask(idx, sp);
        let str = prim_mod_strength(idx);
        let sculpt_op = u32(prim_mod_param2(idx));
        let ax = prim_mod_axis(idx);
        let mc = prim_mask_center(idx);
        let s = str * mask_w;
        if (sculpt_op == 0u) { // Push
            d -= s;
        } else if (sculpt_op == 1u) { // Pull
            d += s;
        } else if (sculpt_op == 2u) { // Flatten
            var axis_val = sp.y - mc.y;
            if (ax == 1u) { axis_val = sp.x - mc.x; }
            else if (ax == 2u) { axis_val = sp.z - mc.z; }
            d += (min(abs(axis_val), abs(str)) * -sign(axis_val)) * mask_w * 0.5;
        } else if (sculpt_op == 3u) { // Inflate
            let proximity = exp(-abs(d) * 4.0);
            d -= s * proximity;
        } else if (sculpt_op == 4u) { // Pinch
            var dx = sp.x - mc.x; var dz = sp.z - mc.z;
            if (ax == 1u) { dx = sp.y - mc.y; dz = sp.z - mc.z; }
            else if (ax == 2u) { dx = sp.x - mc.x; dz = sp.y - mc.y; }
            let lateral = sqrt(dx * dx + dz * dz);
            d += lateral * s * 0.3;
        } else if (sculpt_op == 5u) { // Crease
            var dx = sp.x - mc.x; var dz = sp.z - mc.z;
            if (ax == 1u) { dx = sp.y - mc.y; dz = sp.z - mc.z; }
            else if (ax == 2u) { dx = sp.x - mc.x; dz = sp.y - mc.y; }
            let lateral = sqrt(dx * dx + dz * dz);
            let groove = max(abs(s) - lateral, 0.0);
            d += groove * mask_w * sign(str) * 0.5;
        }
    } else if (mod_t == 8u) { // Spherize: morph result toward a sphere
        let mask_w = eval_mask(idx, sp);
        let t = clamp(prim_mod_strength(idx) * mask_w, 0.0, 1.0);
        let p2 = prim_mod_param2(idx);
        let r = select(0.5, p2, p2 > 0.0);
        d = mix(d, length(sp) - r, t);
    } else if (mod_t == 9u) { // Shell / onion: hollow the surface
        let mask_w = eval_mask(idx, sp);
        d = abs(d) - prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 10u) { // Emboss: raised relief — stamp primitive (mask shape) pushes surface OUT
        let mask_w = eval_mask(idx, sp);
        d -= prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 11u) { // Deboss: recessed relief — stamp primitive pushes surface IN
        let mask_w = eval_mask(idx, sp);
        d += prim_mod_strength(idx) * mask_w;
    }
    // SDF subdivision rounding: higher level = smoother edges
    let subdiv = prim_subdivision(idx);
    if (subdiv >= 6u)      { d -= 0.18; }
    else if (subdiv == 5u) { d -= 0.10; }
    else if (subdiv == 4u) { d -= 0.05; }
    else if (subdiv == 3u) { d -= 0.02; }
    else if (subdiv == 2u) { d -= 0.008; }
    else if (subdiv == 1u) { d -= 0.003; }
    return d * min_s;
}

// Boolean ops
fn smin(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return min(a, b); } let h=clamp(0.5+0.5*(a-b)/k,0.0,1.0); return mix(a,b,h)-k*h*(1.0-h); }
fn smax(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return max(a, b); } let h=clamp(0.5-0.5*(a-b)/k,0.0,1.0); return mix(a,b,h)+k*h*(1.0-h); }
fn chamfer_min(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return min(a, b); } return min(min(a,b), (a-k+b)*0.7071067811865476); }
// Circular fillet. Profiles: 0=Smooth(poly), 1=Chamfer, 2=Circular. shape scales k.
fn smin_circular(a: f32, b: f32, k: f32) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    let kk = k / (1.0 - sqrt(0.5));
    let h = max(kk - abs(a - b), 0.0) / kk;
    return min(a, b) - kk * 0.5 * (1.0 + h - sqrt(max(1.0 - h * (h - 2.0), 0.0)));
}
// Multipoint blend curve: 4 y-samples at x=0,1/3,2/3,1 in full-layout [284-287] when noise off.
fn lut4_sample(lut: vec4<f32>, t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0) * 3.0;
    let i = min(u32(floor(x)), 2u);
    let f = fract(x);
    var ya: f32;
    var yb: f32;
    if (i == 0u) { ya = lut.x; yb = lut.y; }
    else if (i == 1u) { ya = lut.y; yb = lut.z; }
    else { ya = lut.z; yb = lut.w; }
    return mix(ya, yb, f);
}
fn prim_blend_curve_lut_full(idx: u32) -> vec4<f32> {
    let amp = prim_f(idx, 284u);
    let oct_u = prim_u(idx, 286u);
    if (amp > 0.001 && oct_u >= 1u && oct_u <= 16u) {
        return vec4(0.0, 0.333333, 0.666667, 1.0);
    }
    return vec4(prim_f(idx, 284u), prim_f(idx, 285u), prim_f(idx, 286u), prim_f(idx, 287u));
}
fn smin_poly_curved(a: f32, b: f32, k: f32, lut: vec4<f32>) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    var h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    h = lut4_sample(lut, h);
    return a * h + b * (1.0 - h) - k * h * (1.0 - h);
}
fn smin_profiled(a: f32, b: f32, k: f32, profile: u32) -> f32 {
    return smin_profiled_shape(a, b, k, profile, 1.0);
}
fn smin_profiled_shape(a: f32, b: f32, k: f32, profile: u32, shape: f32) -> f32 {
    return smin_profiled_curve(a, b, k, profile, shape, vec4(0.0, 0.333333, 0.666667, 1.0));
}
// profile tags (append-only, match BlendProfile enum):
// 0 = Smooth · 1 = Chamfer · 2/3 = legacy Circular/Curve → Smooth (curve feature removed)
// `shape` and `lut` kept for call-site parity; unused for live profiles.
fn smin_profiled_curve(a: f32, b: f32, k: f32, profile: u32, shape: f32, lut: vec4<f32>) -> f32 {
    _ = shape;
    _ = lut;
    if (profile == 1u) {
        return chamfer_min(a, b, k);
    }
    // 0 Smooth, 2 Circular, 3 Curve → classic poly smin
    return smin(a, b, k);
}
fn smax_profiled(a: f32, b: f32, k: f32, profile: u32) -> f32 { return -smin_profiled(-a, -b, k, profile); }
fn smax_profiled_shape(a: f32, b: f32, k: f32, profile: u32, shape: f32) -> f32 {
    return -smin_profiled_shape(-a, -b, k, profile, shape);
}
fn smax_profiled_curve(a: f32, b: f32, k: f32, profile: u32, shape: f32, lut: vec4<f32>) -> f32 {
    return -smin_profiled_curve(-a, -b, k, profile, shape, lut);
}
// 3D value noise for displacement
fn wgsl_hash(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += vec3(dot(q, vec3(q.y + 33.33, q.z + 33.33, q.x + 33.33)));
    return fract((q.x + q.y) * q.z);
}
fn noise_3d_wgsl(p: vec3<f32>) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n000 = wgsl_hash(i); let n100 = wgsl_hash(i + vec3(1.0,0.0,0.0));
    let n010 = wgsl_hash(i + vec3(0.0,1.0,0.0)); let n110 = wgsl_hash(i + vec3(1.0,1.0,0.0));
    let n001 = wgsl_hash(i + vec3(0.0,0.0,1.0)); let n101 = wgsl_hash(i + vec3(1.0,0.0,1.0));
    let n011 = wgsl_hash(i + vec3(0.0,1.0,1.0)); let n111 = wgsl_hash(i + vec3(1.0,1.0,1.0));
    let x0 = mix(mix(n000,n100,u.x), mix(n010,n110,u.x), u.y);
    let x1 = mix(mix(n001,n101,u.x), mix(n011,n111,u.x), u.y);
    return mix(x0, x1, u.z);
}
fn fbm_wgsl(p: vec3<f32>, octaves: u32) -> f32 {
    var val = 0.0; var amp = 0.5; var fp = p;
    for (var o = 0u; o < octaves; o++) {
        val += amp * (noise_3d_wgsl(fp) * 2.0 - 1.0);
        fp *= 2.0; amp *= 0.5;
    }
    return val;
}

// SEAM boolean (op 5 = cut groove, op 6 = raised bead) — CPU twin: sdf_math.rs seam_cut,
// bake twin: volume_bake.wgsl seam_cut (keep the three float-for-float identical).
// Wall clamp (w - |b|) makes the op EXACTLY the identity when |b| >= w + |a| — the
// op-aware culls rely on this.
fn seam_cut(a0: f32, b: f32, w0: f32, d0: f32, profile: u32) -> f32 {
    let w = max(w0, 1e-4);
    let dep = max(d0, 1e-4);
    let s = abs(b);
    let wall = w - s;
    var c: f32;
    if (profile == 1u) {        // U: elliptical bottom, |grad| <= 1
        let m = min(w, dep);
        c = m * (1.0 - sqrt((a0 / dep) * (a0 / dep) + (b / w) * (b / w)));
    } else if (profile == 2u) { // Square: flat bottom, vertical walls
        c = a0 + dep;
    } else {                    // 0 = V: normalized slanted wall
        let slope = dep / w;
        c = (a0 + dep - slope * s) / sqrt(1.0 + slope * slope);
    }
    return max(a0, min(c, wall));
}

// EXTRUDE boolean (op 7) — CPU twin: sdf_math.rs extrude_op, bake twin: volume_bake.wgsl
// extrude_op (keep the three float-for-float identical; smoothstep written out for parity).
// s = SIGNED strength (>0 raise/emboss out, <0 recess/deboss in). The mask saturates to 0
// EXACTLY at b >= w — the op-aware culls rely on this identity (skip when
// dist_to_prim > width, no accumulator term needed unlike Seam).
fn extrude_op(a: f32, b: f32, w0: f32, s: f32) -> f32 {
    let w = max(w0, 1e-4);
    let t = clamp(b / w, 0.0, 1.0);
    let mask = 1.0 - t * t * (3.0 - 2.0 * t);
    return a - s * mask;
}

// Position-dependent blend falloff + Protect Walls.
// Directional: k *= smoothstep(dot(p-origin, dir)/length).
// Wall protect: full-layout packs margin in [291] when op is not Seam/Extrude (5/6/7);
// hot-layout uses hot[87] (bake path has its own compute_local_k in volume_bake.wgsl).
// Lateral inset: axis 0=Auto, 1=X, 2=Y, 3=Z (lockstep with CPU + volume_bake).
fn wall_protect_lateral_inset_full(sp: vec3<f32>, he: vec3<f32>, st: u32, axis: u32) -> f32 {
    if (st == 2u || st == 5u || st == 6u || st == 8u) {
        return he.x - length(vec2(sp.x, sp.z));
    }
    if (axis == 1u) { return he.x - abs(sp.x); }
    if (axis == 2u) { return he.y - abs(sp.y); }
    if (axis == 3u) { return he.z - abs(sp.z); }
    let dx = he.x; let dy = he.y; let dz = he.z;
    if (dx <= dy && dx <= dz) {
        return min(he.y - abs(sp.y), he.z - abs(sp.z));
    } else if (dy <= dz) {
        return min(he.x - abs(sp.x), he.z - abs(sp.z));
    } else {
        return min(he.x - abs(sp.x), he.y - abs(sp.y));
    }
}

fn blend_k_curve_full(t: f32, curve_type: u32, power: f32) -> f32 {
    let tt = clamp(t, 0.0, 1.0);
    let pw = clamp(power, 0.25, 8.0);
    if (curve_type == 0u) { return tt; }
    if (curve_type == 2u) { return pow(tt, pw); }
    let s = tt * tt * (3.0 - 2.0 * tt);
    if (abs(pw - 2.0) < 0.01) { return s; }
    return pow(s, pw * 0.5);
}

fn compute_local_k(idx: u32, p: vec3<f32>, k: f32) -> f32 {
    var k_out = k;
    // Curve packed in blend_noise_type high nibble when available: type in low 2 bits of u32
    // Full layout: recover from [287] bits — prefer Smooth default if not set.
    // Encode at serialize: we store type*10+power in [287] when noise type was free...
    // Use [287] as curve pack (type*10+power); noise type stays 0 for bake.
    let cpack = prim_f(idx, 287u);
    var ctype = 1u;
    var cpow = 2.0;
    if (cpack >= 0.25 && cpack < 30.0) {
        ctype = u32(cpack / 10.0);
        cpow = cpack - f32(ctype) * 10.0;
    }
    let packed = prim_falloff_packed(idx);
    let fl = length(packed);
    if (fl >= 0.001) {
        let axis = packed / fl;
        let origin = prim_pos(idx) + axis * prim_falloff_offset(idx);
        let t = clamp(dot(p - origin, axis) / fl, 0.0, 1.0);
        k_out *= blend_k_curve_full(t, ctype, cpow);
    }
    // Full-layout wall protect: [291] = axis*100 + margin for normal blend ops.
    let op = prim_op_type(idx);
    let raw_wp = select(prim_f(idx, 291u), 0.0, op >= 5u);
    let wall_axis = u32(raw_wp / 100.0);
    let margin = raw_wp - f32(wall_axis) * 100.0;
    if (margin > 0.001) {
        let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
        let sc = prim_scale(idx);
        let sp = local_p / max(sc, vec3(1e-4));
        let elong = prim_elongation(idx);
        let st = prim_shape_type(idx);
        let p0 = prim_f(idx, 12u); let p1 = prim_f(idx, 13u); let p2 = prim_f(idx, 14u);
        var he = vec3(p0, p1, p2);
        if (st == 0u) { he = vec3(p0, p0, p0); }
        else if (st == 2u || st == 5u || st == 6u || st == 8u) { he = vec3(p0, p1, p0); }
        he = max(he + elong, vec3(1e-4));
        let wd = wall_protect_lateral_inset_full(sp, he, st, wall_axis);
        let tw = clamp(wd / margin, 0.0, 1.0);
        k_out *= blend_k_curve_full(tw, ctype, cpow);
    }
    return k_out;
}

// Note: full-layout has no dedicated shape float; curve LUT lives in [284-287] when noise off.
// Shape defaults to 1.0 — bake/hot path carries the scalar shape handle.
fn apply_boolean(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32) -> f32 {
    // Without prim index, use identity curve. Prefer apply_boolean_at when idx is known.
    return apply_boolean_lut(result, d, op, k, bp, sd, 1.0, vec4(0.0, 0.333333, 0.666667, 1.0));
}
fn apply_boolean_lut(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32, shape: f32, lut: vec4<f32>) -> f32 {
    if (op == 0u) { return select(min(result, d), smin_profiled_curve(result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 1u) { return select(max(result, -d), -smin_profiled_curve(-result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 2u) { return select(max(result, d), smax_profiled_curve(result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 3u) { return select(min(result, d), smin_profiled_curve(result, d, k, 1u, shape, lut), k > 0.0); }
    else if (op == 4u) { return select(max(result, -d), -smin_profiled_curve(-result, d, k, 1u, shape, lut), k > 0.0); }
    // Seam: k = width, sd = depth, bp = cross-section (0=V,1=U,2=Square). 6 = raised dual.
    else if (op == 5u) { return seam_cut(result, d, k, sd, bp); }
    else if (op == 6u) { return -seam_cut(-result, d, k, sd, bp); }
    // Extrude: k = width, sd = SIGNED strength (>0 raise, <0 recess).
    else if (op == 7u) { return extrude_op(result, d, k, sd); }
    return result;
}
fn apply_boolean_at(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32, idx: u32) -> f32 {
    let lut = prim_blend_curve_lut_full(idx);
    return apply_boolean_lut(result, d, op, k, bp, sd, 1.0, lut);
}
// #region NOISY_BLEND
// Blend-zone noise displacement: noise masked to the blend transition region.
// Naga inlines all WGSL function calls — this 5-deep chain causes exponential
// code expansion. Stripped by strip_regions in all WGSL builds; the stub injected
// by neutralize_blend_noise_wgsl delegates to apply_boolean instead.
// Noisy blends are evaluated ONLY in the GLSL BVH path (real function calls).
fn eval_blend_noise(p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32) -> f32 {
    if (amp < 1e-6 || octaves == 0u || octaves > 16u) { return 0.0; }
    let n = fbm_wgsl(p * freq, max(octaves, 1u));
    return n * amp;
}
fn smin_noisy(a: f32, b: f32, k: f32, p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32, bp: u32) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    let base = smin_profiled(a, b, k, bp);
    if (amp < 1e-6 || octaves == 0u || octaves > 16u) { return base; }
    let h = clamp(0.5 + 0.5 * (a - b) / k, 0.0, 1.0);
    let mask = 4.0 * h * (1.0 - h);
    let n = eval_blend_noise(p, amp, freq, octaves, noise_type);
    return base + n * mask;
}
fn smax_noisy(a: f32, b: f32, k: f32, p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32, bp: u32) -> f32 {
    return -smin_noisy(-a, -b, k, p, amp, freq, octaves, noise_type, bp);
}
fn apply_boolean_blend(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32,
                       p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32) -> f32 {
    // Noise only when amp>0 and octaves is a real count (curve LUT uses plain 0..1 floats).
    let noise_on = amp > 0.001 && octaves >= 1u && octaves <= 16u;
    // Seam/Extrude ops (5/6/7) have no noisy variant — always the plain combine.
    if (!noise_on || op >= 5u) { return apply_boolean(result, d, op, k, bp, sd); }
    if (op == 0u) { return select(min(result, d), smin_noisy(result, d, k, p, amp, freq, octaves, noise_type, bp), k > 0.0); }
    else if (op == 1u) { return select(max(result, -d), smax_noisy(result, d, k, p, amp, freq, octaves, noise_type, bp), k > 0.0); }
    else if (op == 2u) { return select(max(result, d), smax_noisy(result, d, k, p, amp, freq, octaves, noise_type, bp), k > 0.0); }
    else if (op == 3u) { return select(min(result, d), smin_noisy(result, d, k, p, amp, freq, octaves, noise_type, 1u), k > 0.0); }
    else if (op == 4u) { return select(max(result, -d), smax_noisy(result, d, k, p, amp, freq, octaves, noise_type, 1u), k > 0.0); }
    return result;
}
// #endregion NOISY_BLEND
// #endregion SDF_PRIM_EVAL
