// ============================================================
// AZRA — Path Trace Entry Point
// Part of the split compute shader. Requires sdf_shared.wgsl.
// ============================================================

// Pathtrace-specific binding (NOT in sdf_shared.wgsl)
@group(0) @binding(27) var pt_accum_in: texture_2d<f32>;

@compute @workgroup_size(8, 8, 1)
fn pass_pathtrace(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tex_dim = textureDimensions(output_texture);
    if (gid.x >= tex_dim.x || gid.y >= tex_dim.y) { return; }
    let coord = vec2<i32>(gid.xy);
    let w = f32(tex_dim.x); let h = f32(tex_dim.y);
    let aspect = w / h;

    // Seed RNG from pixel + frame
    var seed = pcg_hash(gid.x + gid.y * tex_dim.x + config.pt_frame_seed * tex_dim.x * tex_dim.y);

    // Focus-adaptive sample count
    let pixel_uv = vec2(f32(gid.x) / w, f32(gid.y) / h);
    var spp = max(config.pt_samples_per_frame, 1u);
    if (config.pt_focus_uv_x >= 0.0) {
        let fd = length(pixel_uv - vec2(config.pt_focus_uv_x, config.pt_focus_uv_y));
        if (fd < config.pt_focus_radius) {
            spp = spp * 4u;
        }
    }

    var sample_sum = vec3(0.0);
    for (var s = 0u; s < spp; s++) {
        // Sub-pixel jitter for anti-aliasing
        let jx = rand_float(&seed) - 0.5;
        let jy = rand_float(&seed) - 0.5;
        let uv_x = ((f32(gid.x) + jx) / w) * 2.0 - 1.0;
        let uv_y = -(((f32(gid.y) + jy) / h) * 2.0 - 1.0);

        let cam = camera_ray(uv_x, uv_y, aspect);
        var ray_o = cam[0];
        var ray_d = cam[1];
        var throughput = vec3(1.0);
        var radiance = vec3(0.0);

        for (var bounce = 0u; bounce <= config.pt_max_bounces; bounce++) {
            let rr = trace_ray(ray_o, ray_d, config.far_clip);
            if (rr.y < 0.5) {
                // Sky / environment
                if (config.env_width > 0u) {
                    radiance += throughput * sample_env(ray_d);
                } else {
                    let sky_t = ray_d.y * 0.5 + 0.5;
                    radiance += throughput * mix(unpack_rgb(config.bg_bottom_packed), unpack_rgb(config.bg_top_packed), sky_t) * 0.5;
                }
                break;
            }

            let hit_p = ray_o + ray_d * rr.x;
            let n = calc_normal(hit_p);

            // Material
            var alb = vec3(0.66, 0.585, 0.53);
            var rough = 0.65;
            var metal = 0.0;
            if (config.num_primitives > 0u) {
                let hp = find_hit_prim(hit_p);
                var mat: MatResult;
                if (dna_rule_count(hp) > 0u) { mat = eval_material_dna(hp, hit_p, n, -ray_d); }
                else if (prim_has_node_graph(hp)) { mat = eval_node_material(hp, hit_p, n); }
                else { mat = eval_material(hp, hit_p, n); }
                alb = mat.color; rough = mat.roughness; metal = mat.metallic;
                // Emission contributes radiance directly
                if (length(mat.emission) > 0.001) {
                    radiance += throughput * mat.emission;
                }
            }

            // Next Event Estimation: direct light sampling
            let nl = min(num_lights(), 8u);
            for (var li = 0u; li < nl; li++) {
                let lt = light_type_v(li);
                let l_int = light_intensity_v(li);
                let l_col = light_color_v(li);
                var l_dir: vec3<f32>;
                var atten = 1.0;
                if (lt == 0u) {
                    l_dir = light_dir_v(li);
                } else {
                    let lp = light_pos(li);
                    let to_l = lp - hit_p;
                    let dist = length(to_l);
                    l_dir = to_l / max(dist, 0.001);
                    atten = point_atten(dist, light_range_v(li));
                    if (lt == 2u) {
                        atten *= spot_atten(dot(-l_dir, light_dir_v(li)), light_spot_inner_cos(li), light_spot_outer_cos(li));
                    }
                }
                let ndl = max(dot(n, l_dir), 0.0);
                if (ndl > 0.0) {
                    let shd = soft_shadow(hit_p + n * 0.01, l_dir, 0.02, 8.0, 16.0);
                    let dw = 1.0 - metal * 0.7;
                    radiance += throughput * alb * l_col * l_int * ndl * dw * atten * shd;
                    // Specular direct — GGX D × Schlick F (no +0.0001; that killed rough response)
                    let h_s = normalize(-ray_d + l_dir);
                    let ndh_s = max(dot(n, h_s), 0.0);
                    let r_s = clamp(rough, 0.04, 1.0);
                    let a_s = r_s * r_s; let a2_s = max(a_s * a_s, 1e-7);
                    let d_s = ndh_s * ndh_s * (a2_s - 1.0) + 1.0;
                    let D_s = a2_s / max(3.14159265 * d_s * d_s, 1e-7);
                    let vdh_s = max(dot(-ray_d, h_s), 0.0);
                    let f0_s = mix(vec3(0.04), alb, metal);
                    let F_s = f0_s + (vec3(1.0) - f0_s) * pow(1.0 - vdh_s, 5.0);
                    radiance += throughput * F_s * D_s * ndl * l_int * atten * shd * l_col;
                }
            }

            // Ambient on first bounce
            if (bounce == 0u) {
                radiance += throughput * alb * config.light_color.w * 0.3;
            }

            // Russian Roulette after bounce 2
            if (bounce >= 2u) {
                let rr_p = max(max(throughput.x, max(throughput.y, throughput.z)), 0.01);
                if (rand_float(&seed) > rr_p) { break; }
                throughput /= rr_p;
            }

            // Sample next bounce direction
            let is_spec = rand_float(&seed) < mix(0.04, 0.9, metal);
            if (is_spec) {
                let refl = reflect(ray_d, n);
                let px = (rand_float(&seed) - 0.5) * rough;
                let py = (rand_float(&seed) - 0.5) * rough;
                var tang: vec3<f32>;
                if (abs(n.y) < 0.999) { tang = normalize(cross(vec3(0.0, 1.0, 0.0), n)); }
                else { tang = normalize(cross(vec3(1.0, 0.0, 0.0), n)); }
                let bitan = cross(n, tang);
                ray_d = normalize(refl + tang * px + bitan * py);
                if (dot(ray_d, n) < 0.0) { ray_d = refl; }
                throughput *= mix(vec3(1.0), alb, metal);
            } else {
                ray_d = cosine_hemisphere(n, &seed);
                throughput *= alb;
            }
            ray_o = hit_p + n * 0.02;
        }
        sample_sum += radiance;
    }

    let new_color = sample_sum / f32(spp);

    // Progressive accumulation blend. First accumulated frame MUST NOT read pt_accum_in:
    // the texture is never cleared, and mix(garbage, new, 1.0) still evaluates garbage*0.0 —
    // NaN/Inf bits in uninitialized memory poison the pixel permanently (NaN*0 = NaN).
    var blended = new_color;
    if (config.pt_accumulated > 1u) {
        let old_color = textureLoad(pt_accum_in, coord, 0).rgb;
        let weight = 1.0 / f32(config.pt_accumulated);
        blended = mix(old_color, new_color, weight);
    }

    textureStore(output_texture, coord, vec4(blended, 1.0));
}
