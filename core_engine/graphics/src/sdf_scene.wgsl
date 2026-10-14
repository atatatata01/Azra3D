// #region BVH_EVAL
// ============================================================
// Per-body BVH traversal with small stack (occupancy-friendly).
// BvhNode layout: 8 floats per node, read from prim_data (binding 3)
// when wired; currently uncalled — exists only to test register pressure.
//   [0-2] aabb_min, [3] left (bits), [4-6] aabb_max, [7] right (bits)
// Leaf: MSB set (0x80000000), lower bits = body-local prim index.
// SCOPE_OPEN fields: [50]=bvh_offset, [51]=node_count, [52]=prim_count, [53]=body_max_k
// ============================================================

fn eval_body_bvh(p: vec3<f32>, scope_idx: u32) -> f32 {
    let bvh_offset = bitcast<u32>(prim_f(scope_idx, 50u));
    let bvh_node_count = bitcast<u32>(prim_f(scope_idx, 51u));
    let body_max_k = prim_f(scope_idx, 53u);

    if (bvh_node_count == 0u) { return 999.0; }

    // First prim in scope is at scope_idx + 1 (after SCOPE_OPEN marker)
    let body_start = scope_idx + 1u;

    var acc = 999.0;
    var stack: array<u32, 12>;
    var sp = 0;

    // Push root node (index 0 relative to this body's BVH)
    stack[0] = 0u;
    sp = 1;

    let leaf_bit = 0x80000000u;
    // Prune radius: for bodies with intersect ops, body_max_k is huge — don't prune
    let prune_r = select(body_max_k, 0.0, body_max_k > 900.0);

    while (sp > 0) {
        sp -= 1;
        let node_local = stack[sp];
        let node_base = (bvh_offset + node_local) * 8u;

        // Read node AABB from prim_data (placeholder — will be bvh_nodes buffer when wired)
        let aabb_min = vec3(prim_data[node_base], prim_data[node_base + 1u], prim_data[node_base + 2u]);
        let left_bits = bitcast<u32>(prim_data[node_base + 3u]);
        let aabb_max = vec3(prim_data[node_base + 4u], prim_data[node_base + 5u], prim_data[node_base + 6u]);
        let right_bits = bitcast<u32>(prim_data[node_base + 7u]);

        // AABB distance test — skip subtree if too far
        let d_aabb = sd_aabb(p, aabb_min, aabb_max);
        if (acc < 998.0 && d_aabb > acc + prune_r) { continue; }

        // Process left child
        if ((left_bits & leaf_bit) != 0u) {
            let local_idx = left_bits & 0x7FFFFFFFu;
            let gi = body_start + local_idx;
            if (prim_u(gi, 3u) < 254u) {
                let d = eval_prim(gi, p);
                let op = prim_u(gi, 11u);
                let k = compute_local_k(gi, p, prim_f(gi, 15u));
                if (acc > 998.0) { acc = d; }
                else { acc = apply_boolean_blend(acc, d, op, k, prim_blend_profile(gi), prim_seam_depth(gi),
                    p, prim_blend_noise_amp(gi), prim_blend_noise_freq(gi),
                    prim_blend_noise_octaves(gi), prim_blend_noise_type(gi)); }
            }
        } else if (sp < 12) {
            stack[sp] = left_bits;
            sp += 1;
        }

        // Process right child
        if ((right_bits & leaf_bit) != 0u) {
            let local_idx = right_bits & 0x7FFFFFFFu;
            let gi = body_start + local_idx;
            if (prim_u(gi, 3u) < 254u) {
                let d = eval_prim(gi, p);
                let op = prim_u(gi, 11u);
                let k = compute_local_k(gi, p, prim_f(gi, 15u));
                if (acc > 998.0) { acc = d; }
                else { acc = apply_boolean_blend(acc, d, op, k, prim_blend_profile(gi), prim_seam_depth(gi),
                    p, prim_blend_noise_amp(gi), prim_blend_noise_freq(gi),
                    prim_blend_noise_octaves(gi), prim_blend_noise_type(gi)); }
            }
        } else if (sp < 12) {
            stack[sp] = right_bits;
            sp += 1;
        }
    }

    return acc;
}
// #endregion BVH_EVAL

fn scene_analytical(p_in: vec3<f32>) -> f32 {
    // Global symmetry: mirror + optional radial N-way
    var p = p_in;
    if (config.sym_enabled != 0u) {
        let so = config.sym_origin.xyz;
        let sn = config.sym_normal.xyz;
        let radial_n = u32(config.sym_normal.w);
        if (radial_n >= 2u) {
            // Radial N-way symmetry around the axis defined by sn through so
            let rel = p - so;
            // Build tangent frame: tu, tv orthogonal to sn
            var tu: vec3<f32>;
            if (abs(sn.y) < 0.9) { tu = normalize(cross(sn, vec3<f32>(0.0, 1.0, 0.0))); }
            else { tu = normalize(cross(sn, vec3<f32>(1.0, 0.0, 0.0))); }
            let tv = cross(sn, tu);
            // Project to 2D angle in tangent plane
            let u_comp = dot(rel, tu);
            let v_comp = dot(rel, tv);
            var angle = atan2(v_comp, u_comp);
            let sector = 6.283185307 / f32(radial_n);
            // Fold into fundamental sector [0, sector)
            angle = angle - sector * floor(angle / sector);
            // Mirror within sector (bilateral symmetry per slice)
            if (angle > sector * 0.5) { angle = sector - angle; }
            let radius = sqrt(u_comp * u_comp + v_comp * v_comp);
            let ax_comp = dot(rel, sn); // component along axis
            p = so + tu * (radius * cos(angle)) + tv * (radius * sin(angle)) + sn * ax_comp;
        } else {
            // Simple mirror: reflect to positive side
            let d = dot(p - so, sn);
            if (d < 0.0) { p = p - 2.0 * d * sn; }
        }
    }
    // Stack machine evaluator: SCOPE_OPEN (shape_type 254) / SCOPE_CLOSE (255)
    // Replaces 3-level prim→object→layer evaluator with arbitrary nesting.
    if (config.num_primitives == 0u) { return 999.0; }

    var acc = 999.0;              // current scope accumulator
    var stack_d: array<f32, 8>;   // saved accumulator per depth
    var stack_op: array<u32, 8>;  // scope boolean op
    var stack_k: array<f32, 8>;   // scope blend k
    var stack_bp: array<u32, 8>;  // scope blend profile
    var stack_p: array<vec3<f32>, 8>; // saved PRE-mirror position (restore + per-combo reflection)
    var stack_oi: array<u32, 8>;      // SCOPE_OPEN index (for body-mirror union re-seek)
    var stack_mflags: array<u32, 8>;  // body mirror axis mask (0 = no mirror)
    var stack_mc: array<vec3<f32>, 8>;// body mirror plane center
    var stack_ncombo: array<u32, 8>;  // reflection combo count (1 when no mirror)
    var stack_combo: array<u32, 8>;   // current combo index
    var stack_smin: array<f32, 8>;    // running min of scope acc across combos (the union)
    var sp = 0u;                  // stack pointer

    for (var i = 0u; i < config.num_primitives; i++) {
        let st = prim_u(i, 3u);  // shape_type

        if (st == 254u) {
            // SCOPE_OPEN: push current acc, start fresh child scope
            // --- Scope AABB skip optimization ---
            // If the point is far from this scope's AABB, skip the entire scope.
            // For Union/ChamferUnion (op 0, 3): safe if d_aabb > acc + k (smooth blend extends by k).
            // For Subtract/ChamferSubtract (op 1, 4): safe if d_aabb > k and acc >= 0.
            let scope_op = prim_u(i, 11u);
            let s_min = scope_aabb_min(i);
            let s_max = scope_aabb_max(i);
            let scope_k = prim_f(i, 15u);
            let d_aabb = sd_aabb(p, s_min, s_max);
            var skip_scope = false;
            if (d_aabb > 0.0) {
                if ((scope_op == 0u || scope_op == 3u) && d_aabb > acc + scope_k) {
                    skip_scope = true;
                } else if ((scope_op == 1u || scope_op == 4u) && acc >= 0.0 && d_aabb > scope_k) {
                    skip_scope = true;
                } else if ((scope_op == 5u || scope_op == 6u) && d_aabb > scope_k + abs(acc)) {
                    // Seam scope: EXACT identity beyond width + |acc| (seam_cut wall clamp).
                    skip_scope = true;
                } else if (scope_op == 7u && d_aabb > scope_k) {
                    // Extrude scope: EXACT identity beyond width (mask saturates), acc-independent.
                    skip_scope = true;
                }
            }
            if (skip_scope || sp >= 8u) {
                // Fast-forward to matching SCOPE_CLOSE (handles nested scopes).
                // Also fast-forward when the stack is full (can't push) so the
                // unmatched SCOPE_CLOSE doesn't underflow / mis-combine.
                var skip_depth = 1u;
                i += 1u;
                while (i < config.num_primitives && skip_depth > 0u) {
                    let st_j = prim_u(i, 3u);
                    if (st_j == 254u) { skip_depth += 1u; }
                    else if (st_j == 255u) { skip_depth -= 1u; }
                    if (skip_depth > 0u) { i += 1u; }
                }
                continue;
            }
            // Normal path: push and enter scope.
            // Body mirror, two modes selected by mirror_sign (cold field 4) bit 7:
            //  - UNION (bit clear): min over the 2^n reflection sign-combos — the scope body is
            //    walked once PER COMBO (the SCOPE_CLOSE re-seek), both sides survive.
            //  - CUT (bit set, "delete the other half"): one-sided fold onto the source half,
            //    ONE walk regardless of axis count. Must match the bake exactly — this path
            //    feeds reflections / fog / path tracing over the SAME body.
            let mflags = prim_u(i, 5u);
            let msign = prim_u(i, 4u);
            let cut = mflags != 0u && (msign & MIRROR_CUT_BIT) != 0u;
            stack_d[sp]  = acc;
            stack_op[sp] = scope_op;
            stack_k[sp]  = scope_k;
            stack_bp[sp] = prim_blend_profile(i);
            stack_p[sp]  = p;
            stack_oi[sp] = i;
            stack_mflags[sp] = mflags;
            stack_mc[sp] = vec3<f32>(prim_f(i, 6u), prim_f(i, 7u), prim_f(i, 12u));
            // ncombo == 1 under CUT, so the CLOSE re-seek never fires (msign is not stacked).
            stack_ncombo[sp] = select(1u << countOneBits(mflags), 1u, cut);
            stack_combo[sp]  = 0u;
            stack_smin[sp]   = 999.0;
            sp += 1u;
            acc = 999.0;
            // Map p for this mode (union combo 0 = identity when no mirror).
            if (cut) {
                p = fold_for_cut(p, mflags, msign, stack_mc[sp - 1u]);
            } else {
                p = reflect_for_combo(p, mflags, stack_mc[sp - 1u], 0u);
            }
            continue;
        }
        if (st == 255u) {
            // SCOPE_CLOSE: end this combo. If more combos remain, re-seek the IP back to
            // SCOPE_OPEN and re-run the scope body with the next reflection. Once all combos
            // are done, the union (scope_min) is combined with the parent scope.
            if (sp > 0u) {
                let top = sp - 1u;
                stack_smin[top] = min(stack_smin[top], acc);
                let combo = stack_combo[top] + 1u;
                if (combo < stack_ncombo[top]) {
                    stack_combo[top] = combo;
                    acc = 999.0;
                    p = reflect_for_combo(stack_p[top], stack_mflags[top], stack_mc[top], combo);
                    i = stack_oi[top]; // loop's i++ lands on the first prim inside the scope
                    continue;
                }
                sp = top;
                let scope_acc = stack_smin[top];
                p = stack_p[top];   // restore pre-mirror position
                let parent = stack_d[top];
                if (scope_acc < 998.0 && parent > 998.0) {
                    // Parent empty: union-like ops keep child, subtract/intersect discard
                    let sop = stack_op[top];
                    if (sop == 1u || sop == 2u || sop == 4u || sop >= 5u) { acc = parent; } else { acc = scope_acc; }
                } else if (scope_acc < 998.0) {
                    acc = apply_boolean(parent, scope_acc, stack_op[top], stack_k[top], stack_bp[top],
                                        prim_seam_depth(stack_oi[top]));
                } else {
                    acc = parent; // child scope empty, keep parent
                }
            }
            continue;
        }

        // --- Regular primitive ---
        let d = eval_prim(i, p);
        let op = prim_u(i, 11u);
        let k_raw = prim_f(i, 15u);
        let k = compute_local_k(i, p, k_raw);

        if (acc > 998.0) {
            acc = d;
        } else {
            if (op == 0u || op == 3u) {
                let sphere_d = length(p - prim_pos(i)) - prim_bound_r(i);
                if (sphere_d > acc) { continue; }
            }
            if (op == 5u || op == 6u) {
                // Seam: EXACT skip beyond width + |acc| of the prim's bounding sphere
                // (seam_cut wall clamp — mirrors fold_prim / volume_bake).
                let sphere_d = length(p - prim_pos(i)) - prim_bound_r(i);
                if (sphere_d > k_raw + abs(acc)) { continue; }
            }
            if (op == 7u) {
                // Extrude: EXACT skip beyond width (mask saturates), acc-independent
                // (mirrors fold_prim / volume_bake).
                let sphere_d = length(p - prim_pos(i)) - prim_bound_r(i);
                if (sphere_d > k_raw) { continue; }
            }
            acc = apply_boolean_blend(acc, d, op, k, prim_blend_profile(i), prim_seam_depth(i),
                p, prim_blend_noise_amp(i), prim_blend_noise_freq(i),
                prim_blend_noise_octaves(i), prim_blend_noise_type(i));
        }
    }

    // Flush any unclosed scopes (safety). Fold the in-flight combo's acc into the
    // scope union first, then combine the union with the parent (mirrors SCOPE_CLOSE).
    while (sp > 0u) {
        sp -= 1u;
        let scope_acc = min(stack_smin[sp], acc);
        let parent = stack_d[sp];
        if (scope_acc < 998.0 && parent > 998.0) {
            let sop = stack_op[sp];
            if (sop == 1u || sop == 2u || sop == 4u || sop >= 5u) { acc = parent; } else { acc = scope_acc; }
        } else if (scope_acc < 998.0) {
            acc = apply_boolean(parent, scope_acc, stack_op[sp], stack_k[sp], stack_bp[sp],
                                prim_seam_depth(stack_oi[sp]));
        } else {
            acc = parent;
        }
    }

    return select(999.0, acc, acc < 998.0);
}

// #region SCENE_QUERY
fn find_hit_prim(p: vec3<f32>) -> u32 {
    var best = 0u; var best_d = 999.0;
    for (var i = 0u; i < config.num_primitives; i++) {
        let st = prim_u(i, 3u);
        if (st == 254u || st == 255u) { continue; } // skip scope markers
        let op = prim_u(i, 11u);
        if (op == 0u || op == 3u) {
            let sphere_d = length(p - prim_pos(i)) - prim_bound_r(i);
            if (sphere_d > best_d) { continue; }
        }
        let d = abs(eval_prim(i, p));
        if (d < best_d) { best_d = d; best = i; }
    }
    return best;
}

// Find two closest prims and a blend factor for material seam blending.
// Returns: x=prim1_idx, y=prim2_idx, z=blend_factor (0.0=all prim1, 1.0=all prim2)
fn find_seam_blend(p: vec3<f32>) -> vec3<f32> {
    var best1 = 0u; var best1_d = 999.0;
    var best2 = 0u; var best2_d = 999.0;
    for (var i = 0u; i < config.num_primitives; i++) {
        let st = prim_u(i, 3u);
        if (st == 254u || st == 255u) { continue; } // skip scope markers
        let op = prim_u(i, 11u);
        if (op == 0u || op == 3u) {
            let sphere_d = length(p - prim_pos(i)) - prim_bound_r(i);
            if (sphere_d > best2_d + 0.5) { continue; }
        }
        let d = abs(eval_prim(i, p));
        if (d < best1_d) {
            best2 = best1; best2_d = best1_d;
            best1 = i; best1_d = d;
        } else if (d < best2_d && i != best1) {
            best2 = i; best2_d = d;
        }
    }
    // Blend factor: when both prims are close to surface, blend materials
    // Uses the blend_k of the closer prim as the transition width
    let k = prim_f(best1, 15u);
    let blend_width = max(k * 0.5, 0.02);
    // Only blend when both distances are within blend width (at a seam)
    if (best2_d > blend_width * 3.0 || config.num_primitives < 2u) {
        return vec3(f32(best1), f32(best1), 0.0);
    }
    let t = smoothstep(0.0, blend_width, best1_d) * (1.0 - smoothstep(0.0, blend_width * 3.0, best2_d));
    return vec3(f32(best1), f32(best2), clamp(t, 0.0, 1.0));
}

fn boolean_color(op: u32) -> vec3<f32> {
    if (op == 0u || op == 3u) { return vec3(0.45, 0.75, 0.45); }  // Union / ChamferUnion → green
    if (op == 1u || op == 4u) { return vec3(0.85, 0.35, 0.3); }   // Subtract / ChamferSubtract → red
    if (op == 2u) { return vec3(0.4, 0.55, 0.85); }               // Intersect → blue
    return vec3(0.7, 0.7, 0.7);
}
// #endregion SCENE_QUERY

fn safe_dir(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    if (l < 0.0001) { return vec3(0.0, 1.0, 0.0); }
    return v / l;
}

// #region SCENE_EVAL
// ============================================================
// Voxel grid evaluation (Sculpt mode)
// ============================================================
fn get_voxel(x: i32, y: i32, z: i32) -> f32 {
    let res = i32(config.grid_res);
    return voxel_grid[clamp(x,0,res-1) + clamp(y,0,res-1)*res + clamp(z,0,res-1)*res*res];
}
fn scene_voxel(p: vec3<f32>) -> f32 {
    let b = config.grid_bounds;
    let q = abs(p) - vec3(b);
    let d_box = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
    if (d_box > 0.0) { return d_box + 0.1; }
    let res_f = f32(config.grid_res - 1u);
    let gp = ((p + vec3(b)) / (2.0 * b)) * res_f;
    let base = vec3<i32>(floor(gp)); let f = fract(gp);
    let bx = base.x; let by = base.y; let bz = base.z;
    return mix(mix(mix(get_voxel(bx,by,bz),get_voxel(bx+1,by,bz),f.x),
                   mix(get_voxel(bx,by+1,bz),get_voxel(bx+1,by+1,bz),f.x),f.y),
               mix(mix(get_voxel(bx,by,bz+1),get_voxel(bx+1,by,bz+1),f.x),
                   mix(get_voxel(bx,by+1,bz+1),get_voxel(bx+1,by+1,bz+1),f.x),f.y),f.z);
}

// (PT_VOLUME region removed 2026-07-13: the rm=3 PT volume mode sampled the resident
// volume's 3D texture, which is gone with the legacy SDF volume-raymarch display.)

fn scene_sdf(p: vec3<f32>) -> f32 {
    if (config.render_mode == 0u) { return scene_analytical(p); }
    return scene_voxel(p); // render_mode == 1u: Sculpt mode
}
// (RAY_CULL region REMOVED 2026-07-14 — consumed only by the deleted analytical march display path)
fn calc_normal(p: vec3<f32>) -> vec3<f32> {
    // Tetrahedron technique: 4 SDF evals instead of 6 (central differences)
    // Ref: https://iquilezles.org/articles/normalsSDF/
    let e = config.normal_epsilon;
    let k = vec2(1.0, -1.0);
    return safe_dir(
        k.xyy * scene_sdf(p + k.xyy * e) +
        k.yyx * scene_sdf(p + k.yyx * e) +
        k.yxy * scene_sdf(p + k.yxy * e) +
        k.xxx * scene_sdf(p + k.xxx * e)
    );
}
// Central-difference stencil, ONE definition: tap i in 1..=6 = +x, -x, +y, -y, +z, -z (step h).
// Shared by `sdf_central_gradient` and `calc_curvature`'s fused tap loop.
fn sdf_central_diff_offset(i: u32, h: f32) -> vec3<f32> {
    var off = vec3(0.0);
    off[(i - 1u) / 2u] = select(-h, h, (i & 1u) == 1u);
    return off;
}
// Central-difference SDF gradient (f(+h) - f(-h) per axis, unnormalized) with ONE `scene_sdf`
// call site. Six separate calls were six full inlined scene evaluations in DF-full (2026-10-13
// cold-compile fix); same taps, same order, same subtraction — identical value.
fn sdf_central_gradient(p: vec3<f32>, h: f32) -> vec3<f32> {
    var g = vec3(0.0);
    for (var i = 1u; i <= 6u; i++) {
        let s = scene_sdf(p + sdf_central_diff_offset(i, h));
        let axis = (i - 1u) / 2u;
        g[axis] = select(g[axis] - s, s, (i & 1u) == 1u);
    }
    return g;
}
fn rem_euclid(x: f32, y: f32) -> f32 { if (abs(y) < 1e-6) { return 0.0; } return x - y * floor(x / y); }
// #endregion SCENE_EVAL
