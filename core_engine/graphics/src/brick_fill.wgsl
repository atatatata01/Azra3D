// brick_fill.wgsl — fills active bricks directly into the atlas 3D texture.
//
// Architecture (PLAN_BRICK_ATLAS.md): bricks are a WRITE-SIDE optimization.
// The VP shader reads ONLY from the atlas texture. Bricks decide which 8³
// regions to evaluate, then write results into the atlas via textureStore.
//
// One workgroup per sub-brick (4×4×4 voxels). Each 10×10×10 halo region
// (8³ interior + 1-voxel border, Invariant 2) is processed by sub-bricks.
// The brick_list buffer encodes the atlas voxel origin for each sub-brick.
//
// SDF evaluation functions are injected at build time from volume_bake.wgsl.

// ============================================================
// Bindings
// ============================================================
@group(0) @binding(0) var<storage, read> prim_hot: array<f32>;
@group(0) @binding(1) var<uniform> params: BakeParams;
@group(0) @binding(2) var output_tex: texture_storage_3d<r32float, write>;
@group(0) @binding(3) var<storage, read> brick_list: array<u32>;
// brick_list layout: [atlas_voxel_x, atlas_voxel_y, atlas_voxel_z, _reserved] per sub-brick
// Each entry is a 4×4×4 sub-brick origin in atlas texture coordinates.
@group(0) @binding(4) var<storage, read> clip_curves: array<f32>;
// T1 image displace atlas (same layout as volume_bake binding 8). Dummy when unused.
// NOTE: brick_fill BGL is declared at the call site — must include binding 5 if this is live.
// Until fill dispatch packs height maps, bind a 1-float dummy and hot[92] stays NONE.
@group(0) @binding(5) var<storage, read> height_maps: array<f32>;

struct BakeParams {
    grid_res: vec3<u32>,
    _pad0: u32,
    bounds_min: vec3<f32>,
    _pad1: f32,
    bounds_max: vec3<f32>,
    prim_count: u32,
    floats_per_prim: u32,
    _pad2a: u32,
    _pad2b: u32,
    _pad2c: u32,
    bake_min: vec3<u32>,     // unused by fill shader
    _pad3: u32,
    bake_max: vec3<u32>,     // unused by fill shader
    _pad4: u32,
    mirror_flags: u32,       // bit 0=X, bit 1=Y, bit 2=Z
    mirror_center_x: f32,
    mirror_center_y: f32,
    mirror_center_z: f32,
    bevel_radius: f32,
    mirror_sign: u32,        // bit 0=flip X, bit 1=flip Y, bit 2=flip Z
    atlas_offset_x: u32,    // X-texel offset in atlas 3D texture
    atlas_offset_y: u32,    // Y-texel offset in atlas 3D texture
    atlas_offset_z: u32,    // Z-texel offset in atlas 3D texture
    atlas_pad: u32,          // unused by fill shader
    output_mode: u32,        // unused by fill shader (always GridBacked)
    _pad5: u32,
    _pad5c: u32,
}

// ============================================================
// SDF evaluation library (injected from volume_bake.wgsl at build time)
// ============================================================
// {{BAKE_SDF_LIB}}

// ============================================================
// Fill entry point — one workgroup per 4³ sub-brick
// ============================================================
@compute @workgroup_size(4, 4, 4)
fn brick_fill(@builtin(local_invocation_id) lid: vec3<u32>,
              @builtin(workgroup_id) wg_id: vec3<u32>) {
    // Each workgroup handles one 4×4×4 sub-brick from the brick_list.
    // brick_list layout: [grid_voxel_x, grid_voxel_y, grid_voxel_z, 0] per sub-brick.
    // grid_voxel coords are in body's padded grid space (0..grid_res), same as volume_bake.
    let sub_idx = wg_id.x;
    let base = sub_idx * 4u;
    let voxel_origin = vec3<u32>(brick_list[base], brick_list[base + 1u], brick_list[base + 2u]);

    let voxel = voxel_origin + lid;

    // Bounds check: don't write outside the padded grid
    if (any(voxel >= params.grid_res)) { return; }

    // World position: same formula as volume_bake.wgsl entry point.
    // With padding, voxel `pad` maps to bounds_min, voxel `pad+body_res-1` maps to bounds_max.
    let pad = params.atlas_pad;
    let body_res = params.grid_res - vec3(2u * pad);
    let denom = vec3<f32>(max(body_res - vec3(1u), vec3(1u)));
    let t = vec3<f32>(vec3<i32>(voxel) - vec3<i32>(vec3(pad))) / denom;
    var world_pos = mix(params.bounds_min, params.bounds_max, t);

    // Root-body mirror fold (identical to volume_bake.wgsl)
    if ((params.mirror_flags & 1u) != 0u) { let d = abs(world_pos.x - params.mirror_center_x); world_pos.x = params.mirror_center_x + select(d, -d, (params.mirror_sign & 1u) != 0u); }
    if ((params.mirror_flags & 2u) != 0u) { let d = abs(world_pos.y - params.mirror_center_y); world_pos.y = params.mirror_center_y + select(d, -d, (params.mirror_sign & 2u) != 0u); }
    if ((params.mirror_flags & 4u) != 0u) { let d = abs(world_pos.z - params.mirror_center_z); world_pos.z = params.mirror_center_z + select(d, -d, (params.mirror_sign & 4u) != 0u); }

    // Scope-aware stack machine evaluator (identical to volume_bake.wgsl)
    var acc = 999.0;
    var stack_d: array<f32, 8>;   // saved accumulator per depth
    var stack_op: array<u32, 8>;  // scope boolean op
    var stack_k: array<f32, 8>;   // scope blend k
    var stack_bp: array<u32, 8>;  // scope blend profile
    var stack_p: array<vec3f, 8>; // saved evaluation point for mirror restore
    var stack_oi: array<u32, 8>;  // SCOPE_OPEN entry index (for bevel_radius readback)
    var sp = 0u;                  // stack pointer

    var p = world_pos;

    for (var i = 0u; i < params.prim_count; i++) {
        let st = prim_shape_type(i);

        if (st == 254u) {
            // SCOPE_OPEN: push current acc, start fresh child scope
            let scope_op = prim_u(i, 11u);
            let s_min = scope_aabb_min(i);
            let s_max = scope_aabb_max(i);
            let scope_k = prim_f(i, 15u);
            let d_aabb = sd_aabb(p, s_min, s_max);

            // AABB scope-skip optimization
            var skip_scope = false;
            if (d_aabb > 0.0) {
                if ((scope_op == 0u || scope_op == 3u) && d_aabb > acc + scope_k) {
                    skip_scope = true;
                } else if ((scope_op == 1u || scope_op == 4u) && acc >= 0.0 && d_aabb > scope_k) {
                    skip_scope = true;
                }
            }

            if (skip_scope || sp >= 8u) {
                // Fast-forward to matching SCOPE_CLOSE (handles nested scopes)
                var skip_depth = 1u;
                i += 1u;
                while (i < params.prim_count && skip_depth > 0u) {
                    let st_j = prim_shape_type(i);
                    if (st_j == 254u) { skip_depth += 1u; }
                    else if (st_j == 255u) { skip_depth -= 1u; }
                    if (skip_depth > 0u) { i += 1u; }
                }
                continue;
            }

            // Normal path: push and enter scope
            stack_d[sp]  = acc;
            stack_op[sp] = scope_op;
            stack_k[sp]  = scope_k;
            stack_bp[sp] = prim_blend_profile(i);
            stack_p[sp]  = p;
            stack_oi[sp] = i;
            sp += 1u;
            acc = 999.0;

            // Per-scope mirror fold (mirror_sign at field 49)
            let mflags = prim_u(i, 45u);
            if (mflags != 0u) {
                let mc = vec3f(prim_f(i, 46u), prim_f(i, 47u), prim_f(i, 48u));
                let msign = prim_u(i, 49u);
                let dx = abs(p.x - mc.x); let dy = abs(p.y - mc.y); let dz = abs(p.z - mc.z);
                if ((mflags & 1u) != 0u) { p.x = mc.x + select(dx, -dx, (msign & 1u) != 0u); }
                if ((mflags & 2u) != 0u) { p.y = mc.y + select(dy, -dy, (msign & 2u) != 0u); }
                if ((mflags & 4u) != 0u) { p.z = mc.z + select(dz, -dz, (msign & 4u) != 0u); }
            }
            continue;
        }

        if (st == 255u) {
            // SCOPE_CLOSE: pop and combine child result with parent
            if (sp == 0u) { continue; } // safety: malformed stream
            sp -= 1u;

            // Apply body-level bevel (edge rounding)
            let bevel_r = prim_f(stack_oi[sp], 14u);
            if (acc < 998.0 && bevel_r > 0.0) { acc -= bevel_r; }

            // Restore pre-mirror evaluation point
            p = stack_p[sp];

            // Combine child result with parent
            let parent = stack_d[sp];
            if (acc < 998.0 && parent > 998.0) {
                let sop = stack_op[sp];
                if (sop == 1u || sop == 2u || sop == 4u || sop >= 5u) { acc = parent; }
            } else if (acc < 998.0) {
                acc = apply_boolean(parent, acc, stack_op[sp], stack_k[sp], stack_bp[sp],
                                    prim_seam_depth(stack_oi[sp]));
            } else {
                acc = parent;
            }
            continue;
        }

        // --- Regular primitive ---
        let op = prim_u(i, 11u);
        let k_raw = prim_f(i, 15u);

        // Per-prim bounding sphere cull (NEVER skip first prim: acc > 998.0)
        if (acc < 998.0) {
            let prim_center = prim_pos(i);
            let dist_to_prim = length(p - prim_center) - prim_bound_r(i);
            if (dist_to_prim > 0.0) {
                if ((op == 0u || op == 3u) && dist_to_prim > acc + k_raw) { continue; }
                if ((op == 1u || op == 4u) && acc >= 0.0 && dist_to_prim > k_raw) { continue; }
                if (op == 2u && dist_to_prim > k_raw) { continue; }
                // Seam (5/6): EXACT skip beyond width + |acc| (seam_cut wall clamp).
                if ((op == 5u || op == 6u) && dist_to_prim > k_raw + abs(acc)) { continue; }
                // Extrude (7): EXACT skip beyond width (mask saturates), acc-independent.
                if (op == 7u && dist_to_prim > k_raw) { continue; }
            }
        }

        let d = eval_prim(i, p);
        let k = compute_local_k(i, p, k_raw);
        let bp = prim_blend_profile(i);
        let shape = prim_blend_profile_shape(i);
        let lut = prim_blend_curve_lut(i);

        if (acc > 998.0) {
            acc = d;
        } else {
            acc = apply_boolean_shaped(acc, d, op, k, bp, prim_seam_depth(i), shape, lut);
        }
    }

    // Flush any unclosed scopes (safety)
    while (sp > 0u) {
        sp -= 1u;
        let flush_bevel = prim_f(stack_oi[sp], 14u);
        if (acc < 998.0 && flush_bevel > 0.0) { acc -= flush_bevel; }
        p = stack_p[sp];
        let parent = stack_d[sp];
        if (acc < 998.0 && parent > 998.0) {
            let sop = stack_op[sp];
            if (sop == 1u || sop == 2u || sop == 4u || sop >= 5u) { acc = parent; }
        } else if (acc < 998.0) {
            acc = apply_boolean(parent, acc, stack_op[sp], stack_k[sp], stack_bp[sp],
                                prim_seam_depth(stack_oi[sp]));
        } else {
            acc = parent;
        }
    }

    // Apply root-body bevel
    if (acc < 998.0 && params.bevel_radius > 0.0) { acc -= params.bevel_radius; }

    // Write to atlas 3D texture. Grid voxel coords → atlas coords by adding 3D offset.
    let atlas_coord = vec3<u32>(
        voxel.x + params.atlas_offset_x,
        voxel.y + params.atlas_offset_y,
        voxel.z + params.atlas_offset_z
    );
    textureStore(output_tex, atlas_coord, vec4(acc, 0.0, 0.0, 0.0));
}
