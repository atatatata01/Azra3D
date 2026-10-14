// volume_bake.wgsl — standalone compute shader for SDF volume baking on worker GPUs.
//
// Evaluates the same SDF pipeline as compute.wgsl but writes distances to a
// flat f32 grid instead of raymarching. No camera, no lighting, no G-buffer.
//
// SDF functions are copied VERBATIM from compute.wgsl for numerical parity.

// ============================================================
// Bindings
// ============================================================
@group(0) @binding(0) var<storage, read> prim_hot: array<f32>;
@group(0) @binding(1) var<uniform> params: BakeParams;
@group(0) @binding(2) var<storage, read_write> output_grid: array<f32>;
// Direct-to-texture output: writes baked SDF directly to the resident 3D texture.
// Eliminates the entire CPU round-trip (readback → write_texture). The VP shader
// sees the result on the SAME frame the bake completes.
// When the texture is not bound (old pipeline), this binding points to a 1³ dummy.
@group(0) @binding(3) var output_tex: texture_storage_3d<r32float, write>;
@group(0) @binding(4) var<storage, read> clip_curves: array<f32>;
// LAB_MESH_OPERAND spike: self-describing mesh-SDF operand grid, unioned into every
// bake after the prim stack machine. Header floats: [0]=res (0.0/dummy = disabled),
// [1..3]=grid min corner (world space), [4]=cell size, [5..7]=reserved. Distances
// follow at offset 8 in x + y*res + z*res*res order (region_mesh_to_sdf layout).
// When no operand is loaded, a small zeroed dummy buffer is bound (res=0 -> no-op).
@group(0) @binding(5) var<storage, read> mesh_grid: array<f32>;
// SparseBricks (output_mode=2) brick→pool-handle map: per-brick tags over the body voxel grid
// (EXTERIOR 0xFFFFFFFF / INTERIOR 0xFFFFFFFE sentinels, else a dense-packed pool handle into the
// pool arena bound at binding 2). OWN binding — the lab↔cache merge briefly parked this on
// binding 5 via a bitcast through mesh_grid, which corrupted every sparse bake once the
// sparse_bake_debug toggle was ON (production binds the LAB mesh grids at 5 unconditionally).
// Modes 0/1 bind a 4-byte filler; only mode 2 reads it.
@group(0) @binding(6) var<storage, read> brick_index: array<u32>;
// CURVATURE kernel (entry `curvature_eval`) sample POSITIONS, in the SAME body-local frame as
// `eval_scene_at` — 3 f32 per sample, tightly packed (x,y,z,x,y,z,…). Read ONLY by
// `curvature_eval` and ONLY when `curv_seed_mode() == 0` (explicit-position mode); the planar
// triplanar mode (1) derives every seed from the AABB on-device and never touches this buffer.
// Every other entry point binds a 4-byte dummy here (see `bake_bgl_entries` binding 7).
@group(0) @binding(7) var<storage, read> curv_positions: array<f32>;
// T1 image displace: packed height maps (header w/h/axis + f32 samples). hot[92] = start index
// (0xFFFFFFFF = none → FBM). Empty dummy (1 float) when no maps.
@group(0) @binding(8) var<storage, read> height_maps: array<f32>;

struct BakeParams {
    grid_res: vec3<u32>,
    // GPU-classify (entry `classify_bricks` ONLY): voxels per brick edge (8 in production).
    // Repurposed spare padding (was _pad0) — offset/size UNCHANGED; zero in bake dispatches.
    brick_dim: u32,
    bounds_min: vec3<f32>,
    // GPU-classify ONLY: the classifier band (world units). Was _pad1 — offset/size UNCHANGED.
    classify_band: f32,
    bounds_max: vec3<f32>,
    prim_count: u32,
    floats_per_prim: u32,
    // Brick-grid dims for SparseBricks (output_mode=2): bricks per axis over the BODY voxel grid.
    // Repurposed spare padding (was _pad2a/_pad2b/_pad2c) — field offsets UNCHANGED. Zero in
    // modes 0/1. (Struct is 208 bytes: 144 through _pad5 + 64 for body_local_to_world.)
    dims_bricks_x: u32,
    dims_bricks_y: u32,
    dims_bricks_z: u32,
    bake_min: vec3<u32>,     // inclusive start of region to bake
    // GPU-classify ONLY: the CPU-computed non-Lipschitz margin (NONLIPSCHITZ_MARGIN_FACTOR ×
    // brick diagonal, world units) — passed in rather than recomputed on GPU so the classify
    // thresholds are BIT-IDENTICAL to BrickClassifier's. Was _pad3 (u32) — offset/size
    // UNCHANGED (u32→f32 relabel of dead padding; the Rust struct keeps `_pad3: u32` and the
    // classify caller stores `margin.to_bits()`).
    classify_margin: f32,
    bake_max: vec3<u32>,     // exclusive end of region to bake
    // O1 (GPU-classify ONLY): coarse-cull gate. 1 = the prim snapshot is provably 1-Lipschitz
    // (coarse_cull_safe on the CPU) and classify_bricks may cull whole 4³-brick super-cells from
    // ONE center eval; 0 = pre-O1 behavior verbatim. Was _pad4 — offset/size UNCHANGED (the
    // Rust struct keeps `_pad4: u32`; bake dispatches store 0).
    coarse_cull: u32,
    mirror_flags: u32,       // bit 0=X, bit 1=Y, bit 2=Z
    mirror_center_x: f32,
    mirror_center_y: f32,
    mirror_center_z: f32,
    bevel_radius: f32,
    mirror_sign: u32,        // bit 0=flip X, bit 1=flip Y, bit 2=flip Z
    atlas_offset_x: u32,    // X-texel offset in atlas 3D texture (0 when no atlas)
    atlas_offset_y: u32,    // Y-texel offset in atlas 3D texture
    atlas_offset_z: u32,    // Z-texel offset in atlas 3D texture
    atlas_pad: u32,          // padding voxels on each side in atlas (0 = no padding)
    output_mode: u32,        // 0=GridBacked (flat buffer + texture), 1=TextureOnly, 2=SparseBricks (8³ brick pool via brick_index; binding 2 = pool arena)
    _pad5: u32,
    // LAB mesh operand: the baked body's local→world transform (column-major).
    // Bake sample points are in the TOP body's LOCAL frame (BODY_LOCAL_PRIMS);
    // the operand grid header is WORLD space. Identity when the operand is disabled.
    body_local_to_world: mat4x4<f32>,
}

// ============================================================
// Primitive data accessors (HOT buffer layout, stride = params.floats_per_prim)
// ============================================================
fn prim_f(idx: u32, field: u32) -> f32 { return prim_hot[idx * params.floats_per_prim + field]; }
fn prim_u(idx: u32, field: u32) -> u32 { return bitcast<u32>(prim_hot[idx * params.floats_per_prim + field]); }

fn prim_pos(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,0u), prim_f(idx,1u), prim_f(idx,2u)); }
fn prim_quat(idx: u32) -> vec4<f32> { return vec4(prim_f(idx,4u), prim_f(idx,5u), prim_f(idx,6u), prim_f(idx,7u)); }
fn prim_scale(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,8u), prim_f(idx,9u), prim_f(idx,10u)); }
fn prim_shape_type(idx: u32) -> u32 { return prim_u(idx, 3u) & 0xFFu; }
fn prim_op_type(idx: u32) -> u32 { return prim_u(idx, 11u); }
fn prim_bound_r(idx: u32) -> f32 { return prim_f(idx, 27u); }

// Bevel (hot[32..43] = radii, hot[44] = modes)
fn prim_bev_edge_r(idx: u32, edge: u32) -> f32 { return prim_f(idx, 32u + edge); }
fn prim_bev_modes(idx: u32) -> u32   { return prim_u(idx, 44u); }

// Mirror (hot[45-48])
fn prim_mirror_bits(idx: u32) -> u32 { return prim_u(idx, 45u); }
fn prim_mirror_center(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 46u), prim_f(idx, 47u), prim_f(idx, 48u)); }

// Stair steps (packed in shape_type field bits 16-31)
fn prim_stair_steps(idx: u32) -> u32 { return max((prim_u(idx, 3u) >> 16u) & 0xFFFFu, 1u); }

// Blend falloff (hot[28-31])
fn prim_falloff_packed(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 28u), prim_f(idx, 29u), prim_f(idx, 30u)); }
fn prim_falloff_offset(idx: u32) -> f32 { return prim_f(idx, 31u); }

// Modifier data (hot[49-69])
fn prim_mod_type(idx: u32) -> u32 { return prim_u(idx, 49u); }
fn prim_mod_strength(idx: u32) -> f32 { return prim_f(idx, 50u); }
fn prim_mod_axis(idx: u32) -> u32 { return prim_u(idx, 51u); }
fn prim_mod_param2(idx: u32) -> f32 { return prim_f(idx, 52u); }
fn prim_mask_type(idx: u32) -> u32 { return prim_u(idx, 53u); }
fn prim_mask_center(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,54u), prim_f(idx,55u), prim_f(idx,56u)); }
fn prim_mask_extents(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,57u), prim_f(idx,58u), prim_f(idx,59u)); }
fn prim_mask_feather(idx: u32) -> f32 { return prim_f(idx, 60u); }
fn prim_mask_invert(idx: u32) -> u32 { return prim_u(idx, 61u); }
fn prim_mask_noise_freq(idx: u32) -> f32 { return prim_f(idx, 62u); }
fn prim_mask_noise_octaves(idx: u32) -> u32 { return prim_u(idx, 63u); }
fn prim_mask_noise_lacunarity(idx: u32) -> f32 { return prim_f(idx, 64u); }
fn prim_mask_noise_gain(idx: u32) -> f32 { return prim_f(idx, 65u); }
fn prim_mask_noise_threshold(idx: u32) -> f32 { return prim_f(idx, 66u); }
fn prim_mask_noise_offset(idx: u32) -> vec3<f32> { return vec3(prim_f(idx,67u), prim_f(idx,68u), prim_f(idx,69u)); }

// Subdivision + blend profile (hot[72-73])
fn prim_subdivision(idx: u32) -> u32 { return prim_u(idx, 72u); }
fn prim_blend_profile(idx: u32) -> u32 { return prim_u(idx, 73u); }
// Seam depth (hot[86], op 5/6) / extrude SIGNED strength (op 7) — slot overloaded, an op
// is either Seam or Extrude. Width rides the blend_k slot (hot[15]) for both;
// seam cross-section rides the blend_profile slot (0=V,1=U,2=Square).
fn prim_seam_depth(idx: u32) -> f32 { return prim_f(idx, 86u); }
// Lipschitz constant (hot[74] — bake path only; march path stores body_index here)
// L=1.0 for standard shapes (exact SDFs). L>1.0 for twist/bend modifiers (future).
// Used to tighten per-prim sphere skip: skip when dist_to_prim > L * (acc + k).
fn prim_lipschitz(idx: u32) -> f32 { return prim_f(idx, 74u); }
// Blend-zone noise displacement (hot[80-83])
fn prim_blend_noise_amp(idx: u32) -> f32 { return prim_f(idx, 80u); }
fn prim_blend_noise_freq(idx: u32) -> f32 { return prim_f(idx, 81u); }
fn prim_blend_noise_octaves(idx: u32) -> u32 { return prim_u(idx, 82u); }
fn prim_blend_noise_type(idx: u32) -> u32 { return prim_u(idx, 83u); }
// Clip curve header (hot[84-87]) — polygon data lives in separate clip_curves buffer
fn prim_clip_packed(idx: u32) -> u32 { return prim_u(idx, 84u); }
fn prim_has_clips(idx: u32) -> bool { return (prim_clip_packed(idx) & 1u) != 0u; }
fn prim_clip_count(idx: u32) -> u32 { return (prim_clip_packed(idx) >> 16u) & 0xFFFFu; }
fn prim_clip_buf_start(idx: u32) -> u32 { return prim_u(idx, 85u); }
// LAB poly→SDF: hot[86] = float index where this MeshVolume prim's grid header starts in
// the mesh_grid side buffer (@binding 5). Only meaningful when shape_type == 10. This accessor
// references NO binding, so it is safe inside the extract_bake_sdf_lib range (brick_fill).
fn prim_mesh_grid_start(idx: u32) -> u32 { return prim_u(idx, 86u); }

// Protect walls margin (hot[87]) — 0 = off
fn prim_wall_protect(idx: u32) -> f32 { return prim_f(idx, 87u); }
// Elongation (hot[88-90]) — domain warp extending shape along axes
fn prim_elongation(idx: u32) -> vec3<f32> { return vec3(prim_f(idx, 88u), prim_f(idx, 89u), prim_f(idx, 90u)); }
// T1 image height-map atlas start (hot[92]); 0xFFFFFFFF = no image (FBM displace)
fn prim_height_map_start(idx: u32) -> u32 { return prim_u(idx, 92u); }

// Bilinear sample of a packed height map in height_maps[] (CPU DisplaceHeightMap parity).
// Header at `start`: w,h,axis,pad then w*h samples in [0,1]. Returns height in [-1,1].
fn sample_height_map(start: u32, sp: vec3<f32>, freq: f32) -> f32 {
    if (start == 0xFFFFFFFFu) { return 0.0; }
    let w = bitcast<u32>(height_maps[start]);
    let h = bitcast<u32>(height_maps[start + 1u]);
    let axis = bitcast<u32>(height_maps[start + 2u]);
    if (w == 0u || h == 0u) { return 0.0; }
    let f = max(freq, 0.1);
    var u: f32;
    var v: f32;
    // uv_from_local: axis 0=Y→XZ, 1=X→YZ, 2=Z→XY
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
    // Wrap + pixel-center sample (CPU rem_euclid parity)
    let wf = f32(w);
    let hf = f32(h);
    var uf = (u - floor(u)) * wf; // fract wrap
    var vf = (v - floor(v)) * hf;
    uf = uf - 0.5;
    vf = vf - 0.5;
    // wrap negative
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
    return h01 * 2.0 - 1.0; // [0,1] → [-1,1] like FBM
}

// Scope marker AABB accessors (SCOPE_OPEN field layout)
fn scope_aabb_min(i: u32) -> vec3<f32> {
    return vec3(prim_f(i, 0u), prim_f(i, 1u), prim_f(i, 2u));
}

fn scope_aabb_max(i: u32) -> vec3<f32> {
    return vec3(prim_f(i, 8u), prim_f(i, 9u), prim_f(i, 10u));
}

fn sd_aabb(p: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> f32 {
    let center = (bmin + bmax) * 0.5;
    let half = (bmax - bmin) * 0.5;
    let d = abs(p - center) - half;
    return length(max(d, vec3(0.0))) + min(max(d.x, max(d.y, d.z)), 0.0);
}

// Body-mirror UNION helper: reflect `p` across the subset of enabled mirror axes
// selected by `combo`. `mflags` bit 0=X,1=Y,2=Z marks the enabled axes; combo bit
// j (counting only enabled axes, low to high) toggles reflection of the j-th enabled
// axis across plane `mc[axis]` (p_axis = 2*mc_axis - p_axis). combo 0 = identity.
// Leaf function (no recursion) — inlines linearly, NOT the forbidden NOISY_BLEND chain.
fn reflect_for_combo(p_in: vec3<f32>, mflags: u32, mc: vec3<f32>, combo: u32) -> vec3<f32> {
    var p = p_in;
    var bit = 0u; // index among enabled axes
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

// Body-mirror CUT bit in mirror_sign (hot field 49) — `body_field_ops::MIRROR_CUT_BIT`.
const MIRROR_CUT_BIT: u32 = 128u;

// Body-mirror CUT helper ("delete the other half"): fold `p` onto the SOURCE half of every
// enabled axis — `p_axis = mc ± |p_axis − mc|`, minus when that axis' flip bit (msign bits 0-2)
// is set. Replaces the 2^n-combo union with ONE scope evaluation: geometry on the discarded
// side maps away and straddling geometry is bisected at the plane. CPU twin (bit-for-bit):
// `field_eval::fold_for_cut`. Leaf function — inlines linearly.
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

// ============================================================
// Quaternion rotation (copied verbatim from compute.wgsl)
// ============================================================
fn quat_inv_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let qi = vec4(-q.x, -q.y, -q.z, q.w);
    let t = 2.0 * cross(qi.xyz, v);
    return v + qi.w * t + cross(qi.xyz, t);
}

// ============================================================
// Analytical SDF shapes (copied verbatim from compute.wgsl)
// ============================================================
fn sdf_sphere(p: vec3<f32>, radius: f32) -> f32 { return length(p) - radius; }
// Signed distance to a closed 2D polygon. Positive outside, negative inside.
// Uses winding-number-based sign. Reads vertex data from the clip_curves buffer.
// `verts_start` = index into clip_curves[] where vertex xy pairs begin.
fn sdf_polygon_2d_gpu(p: vec2<f32>, verts_start: u32, vert_count: u32) -> f32 {
    if (vert_count < 3u) { return 999.0; }
    var d_sq = 1e20;
    var winding: i32 = 0;
    for (var i = 0u; i < vert_count; i++) {
        let j = (i + 1u) % vert_count;
        let vi = vec2(clip_curves[verts_start + i * 2u], clip_curves[verts_start + i * 2u + 1u]);
        let vj = vec2(clip_curves[verts_start + j * 2u], clip_curves[verts_start + j * 2u + 1u]);
        let e = vj - vi;
        let w = p - vi;
        let t = clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        let closest = vi + e * t;
        let ds = dot(p - closest, p - closest);
        d_sq = min(d_sq, ds);
        if (vi.y <= p.y) {
            if (vj.y > p.y && (e.x * w.y - e.y * w.x) > 0.0) { winding += 1; }
        } else {
            if (vj.y <= p.y && (e.x * w.y - e.y * w.x) < 0.0) { winding -= 1; }
        }
    }
    let sign_val = select(1.0, -1.0, winding != 0);
    return sqrt(d_sq) * sign_val;
}

// Unsigned distance to an OPEN polyline (segments i→i+1, NO closing wrap). The P2 open-profile
// primitive: offsetting by −thickness/2 gives a ribbon; combining with the axis gives a tube.
// Matches `dist_to_open_polyline_2d` in curves.rs for CPU/GPU parity.
fn dist_to_open_polyline_gpu(p: vec2<f32>, verts_start: u32, vert_count: u32) -> f32 {
    if (vert_count < 2u) { return 1e9; }
    var d = 1e9;
    for (var i = 0u; i < vert_count - 1u; i++) {
        let a = vec2(clip_curves[verts_start + i * 2u], clip_curves[verts_start + i * 2u + 1u]);
        let b = vec2(clip_curves[verts_start + (i + 1u) * 2u], clip_curves[verts_start + (i + 1u) * 2u + 1u]);
        let e = b - a;
        let w = p - a;
        let t = clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        let closest = a + e * t;
        d = min(d, length(p - closest));
    }
    return d;
}

// Extruded 2D circle (infinite or capped cylinder). Positive outside, negative inside.
fn sdf_extruded_circle(p: vec3<f32>, radius: f32, axis: u32, depth: f32, depth_inf: bool) -> f32 {
    var d2d: f32;
    var axis_d: f32;
    if (axis == 1u) { d2d = length(p.yz) - radius; axis_d = abs(p.x); }
    else if (axis == 2u) { d2d = length(p.xy) - radius; axis_d = abs(p.z); }
    else { d2d = length(p.xz) - radius; axis_d = abs(p.y); }
    if (depth_inf) { return d2d; }
    return max(d2d, axis_d - depth);
}
fn sdf_box(p: vec3<f32>, half_ext: vec3<f32>) -> f32 {
    let q = abs(p) - half_ext;
    return length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}
fn bevel_2d(a: f32, b: f32, r: f32, m: u32) -> f32 {
    if (m == 1u) {
        let q = vec2(a + r, b + r);
        return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    } else if (m == 2u) {
        // Chamfer — EXACT 2D prism SDF (BEVEL-NOTCH fix 2026-07-17); vertex sectors use
        // the true vertex distance. KEEP LINE-PARALLEL with sdf_shapes.rs + sdf_prim_eval.wgsl.
        if (a >= b + r && b + r >= 0.0) { return length(vec2(a, b + r)); }
        if (b >= a + r && a + r >= 0.0) { return length(vec2(a + r, b)); }
        return max(max(a, b), (a + b + r) * 0.7071067811865476);
    } else {
        let q = vec2(a, b);
        return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0);
    }
}
// BEVEL-NOTCH fix (2026-07-17): tri-planar max + exact roundbox-rho corner term.
// KEEP LINE-PARALLEL with CPU sdf_shapes.rs + sdf_prim_eval.wgsl (full rationale there).
// mode_bits: 2b×12 edges, bits 24-25=up_axis, bits 26-27=algo (0=PerEdge, 1=Uniform).
fn sdf_box_per_edge(p: vec3<f32>, b: vec3<f32>, idx: u32) -> f32 {
    let mode_bits = prim_bev_modes(idx);
    let up_axis = (mode_bits >> 24u) & 3u;
    let algo = (mode_bits >> 26u) & 3u;
    var bp = p;
    var bh = b;
    if (up_axis == 1u) { bp = vec3(p.y, p.x, p.z); bh = vec3(b.y, b.x, b.z); }
    else if (up_axis == 2u) { bp = vec3(p.x, p.z, p.y); bh = vec3(b.x, b.z, b.y); }
    // Uniform algo: exact sdRoundBox from max active edge radius (all-edge clean corners).
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
    let xi = select(0u, 1u, bp.x > 0.0);
    let zi = select(0u, 1u, bp.z > 0.0);
    let base_y = select(0u, 4u, bp.y > 0.0);
    let e_xz = 8u + xi + zi * 2u;
    let e_xy = base_y + 2u + xi;
    let e_yz = base_y + zi;
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
fn prim_bevel_profile_top(idx: u32) -> f32 { return prim_f(idx, 75u); }
fn prim_bevel_profile_bot(idx: u32) -> f32 { return prim_f(idx, 76u); }
fn prim_bevel_profile_vert(idx: u32) -> f32 { return prim_f(idx, 77u); }
fn prim_chamfer_angle_top(idx: u32) -> f32 { return prim_f(idx, 78u); }
fn prim_chamfer_angle_bot(idx: u32) -> f32 { return prim_f(idx, 79u); }
fn edge_profile(idx: u32, edge: u32) -> f32 {
    if (edge >= 8u) { return prim_bevel_profile_vert(idx); }
    if (edge >= 4u) { return prim_bevel_profile_top(idx); }
    return prim_bevel_profile_bot(idx);
}
fn edge_chamfer_angle(idx: u32, edge: u32) -> f32 {
    // Vert angle not in HOT buffer — default 45°
    if (edge >= 8u) { return 45.0; }
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
        let q = max(vec2(a + r, b + r), vec2(0.0));
        let d_round = length(q) + min(max(a + r, b + r), 0.0) - r;
        return mix(d_chamfer, d_round, profile);
    } else {
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
    let col_r = length(vec2(p.x, p.z)) - 0.08;
    let col_h = abs(p.y) - half_h;
    let column = length(max(vec2(col_r, col_h), vec2(0.0))) + min(max(col_r, col_h), 0.0);
    return min(d, column);
}

// ============================================================
// 3D Noise (copied verbatim from compute.wgsl)
// ============================================================
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
    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}
fn fbm_wgsl(p: vec3<f32>, octaves: u32) -> f32 {
    var val = 0.0; var amp = 0.5; var fp = p;
    for (var o = 0u; o < octaves; o++) {
        val += amp * (noise_3d_wgsl(fp) * 2.0 - 1.0);
        fp *= 2.0; amp *= 0.5;
    }
    return val;
}

// fBm with explicit lacunarity/gain (for noise mask)
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
// Modifier system (copied verbatim from compute.wgsl)
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
    if (mt == 1u) {
        let d = length(p - center);
        let r = max(extents.x, 0.001);
        w = gpu_smoothstep(r, r * (1.0 - max(feather, 0.001)), d);
    } else if (mt == 2u) {
        let ext = max(extents, vec3(0.001));
        let q = abs(p - center) / ext;
        let d = max(q.x, max(q.y, q.z));
        w = gpu_smoothstep(1.0, 1.0 - max(feather, 0.001), d);
    } else if (mt == 3u) {
        let fl = length(extents);
        if (fl < 0.001) { return 1.0; }
        let dir = extents / fl;
        let t = clamp(dot(p - center, dir) / fl, 0.0, 1.0);
        w = t * t * (3.0 - 2.0 * t);
    } else if (mt == 4u) {
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

// ============================================================
// eval_prim — evaluate single primitive SDF (copied from compute.wgsl)
// ============================================================
fn eval_prim(idx: u32, p: vec3<f32>) -> f32 {
    let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
    let sc = prim_scale(idx);
    var sp = local_p / sc;
    // Mirror modifier
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
    // Domain warp modifiers (Twist, Taper, Shear)
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
    else if (st == 8u) {
        let he = vec3(p0 * 0.5, p1 * 0.5, 0.005);
        let q = abs(eval_p) - he;
        d = length(max(q, vec3(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
    }
    else if (st == 9u) { // CurveExtrude: profile from clip_curves buffer, local XZ, extruded Y.
        // p0 = half_depth, p1 = wall_t (shell), p2 = rim (extrude-cap bevel; sign=mode, mag=radius).
        // The clip record's mode slot (clip[+0]) carries the P2 mode: 0.0 = CLOSED (filled winding
        // polygon, corners pre-expanded at serialize time → no corner math here); nonzero = OPEN
        // polyline offset by |slot| in-plane (sign<0 ⇒ round tube, else flat ribbon/wall).
        if (prim_has_clips(idx)) {
            let buf_start = prim_clip_buf_start(idx);
            let mode_slot = clip_curves[buf_start];
            let vc = bitcast<u32>(clip_curves[buf_start + 1u]);
            let cu = vec3(clip_curves[buf_start + 2u], clip_curves[buf_start + 3u], clip_curves[buf_start + 4u]);
            let cv = vec3(clip_curves[buf_start + 5u], clip_curves[buf_start + 6u], clip_curves[buf_start + 7u]);
            let verts_start = buf_start + 8u;
            let uv = vec2(dot(eval_p, cu), dot(eval_p, cv));
            if (mode_slot != 0.0 && vc >= 2u) {
                // OPEN (P2): unsigned distance to the open path, offset by half the thickness.
                let thickness = abs(mode_slot);
                let d_path = dist_to_open_polyline_gpu(uv, verts_start, vc);
                if (mode_slot < 0.0) {
                    // Round tube: sqrt(inplane² + axis²) − radius (round cross-section + end-caps).
                    d = length(vec2(d_path, eval_p.y)) - thickness * 0.5;
                } else {
                    // Flat ribbon / wall: in-plane band, extruded ±half_depth (optional rim on cap edge).
                    let d_band = d_path - thickness * 0.5;
                    if (p0 > 0.001) {
                        let cap = abs(eval_p.y) - p0;
                        let rim_prof = select(0.0, 1.0, p2 > 0.0);
                        d = bevel_2d_profiled(d_band, cap, abs(p2), rim_prof, 45.0);
                    } else {
                        d = d_band;
                    }
                }
            } else if (vc >= 3u) {
                // CLOSED (P1): filled winding polygon + rim + shell.
                d = sdf_polygon_2d_gpu(uv, verts_start, vc);
                if (p0 > 0.001) {
                    // RIM: round (rim>0) / chamfer (rim<0) the profile↔cap join; rim==0 ⇒ hard max.
                    let cap = abs(eval_p.y) - p0;
                    let rim_prof = select(0.0, 1.0, p2 > 0.0);
                    d = bevel_2d_profiled(d, cap, abs(p2), rim_prof, 45.0);
                }
                // SHELL: hollow INWARD only (outer surface unchanged → bounding contract preserved).
                if (p1 > 0.0001) { d = max(d, -d - p1); }
            }
        }
    }
    else if (st == 11u) { // HalfSpace / slice: infinite plane through origin, normal = local +Z
        d = eval_p.z;
    }
    else if (st == 12u) { // SpinePipe: 3D polyline tube with per-point radius scale
        // Layout: [base_r][count][closed][pad×5] + xyz*n + point_scale*n. Owned by st==12 only.
        // Effective radius at vertex i = base_r * scale[i]; lerped along segments.
        var d_spine = 1e6;
        if (prim_has_clips(idx)) {
            let buf_start = prim_clip_buf_start(idx);
            let base_r = clip_curves[buf_start];
            let pc = min(bitcast<u32>(clip_curves[buf_start + 1u]), 64u);
            let closed = clip_curves[buf_start + 2u] > 0.5;
            let pts = buf_start + 8u;
            let sc0 = pts + pc * 3u; // scales start after xyz block
            if (pc == 1u) {
                let a = vec3(clip_curves[pts], clip_curves[pts + 1u], clip_curves[pts + 2u]);
                let ra = base_r * max(clip_curves[sc0], 0.001);
                d_spine = length(eval_p - a) - ra;
            } else if (pc >= 2u) {
                var best = 1e20;
                let segs = select(pc - 1u, pc, closed && pc >= 3u);
                for (var i = 0u; i < segs; i = i + 1u) {
                    let j = select(i + 1u, 0u, i + 1u >= pc);
                    let a = vec3(
                        clip_curves[pts + i * 3u],
                        clip_curves[pts + i * 3u + 1u],
                        clip_curves[pts + i * 3u + 2u],
                    );
                    let b = vec3(
                        clip_curves[pts + j * 3u],
                        clip_curves[pts + j * 3u + 1u],
                        clip_curves[pts + j * 3u + 2u],
                    );
                    let ra = base_r * max(clip_curves[sc0 + i], 0.001);
                    let rb = base_r * max(clip_curves[sc0 + j], 0.001);
                    let ab = b - a;
                    let ab2 = max(dot(ab, ab), 1e-20);
                    let t = clamp(dot(eval_p - a, ab) / ab2, 0.0, 1.0);
                    let r = mix(ra, rb, t);
                    best = min(best, length(eval_p - (a + ab * t)) - r);
                }
                d_spine = best;
            }
        }
        d = d_spine;
    }
    // Surface alteration modifiers
    if (mod_t == 3u) {
        let mask_w = eval_mask(idx, sp);
        d -= prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 4u) {
        let mask_w = eval_mask(idx, sp);
        let freq = max(prim_mod_param2(idx), 0.1);
        let hm_start = prim_height_map_start(idx);
        var noise_val: f32;
        if (hm_start != 0xFFFFFFFFu) {
            noise_val = sample_height_map(hm_start, sp, freq);
        } else {
            noise_val = fbm_wgsl(sp * freq, 4u);
        }
        d += noise_val * prim_mod_strength(idx) * mask_w;
    } else if (mod_t == 7u) {
        let mask_w = eval_mask(idx, sp);
        let str = prim_mod_strength(idx);
        let sculpt_op = u32(prim_mod_param2(idx));
        let ax = prim_mod_axis(idx);
        let smc = prim_mask_center(idx);
        let s = str * mask_w;
        if (sculpt_op == 0u) { d -= s; }
        else if (sculpt_op == 1u) { d += s; }
        else if (sculpt_op == 2u) {
            var axis_val = sp.y - smc.y;
            if (ax == 1u) { axis_val = sp.x - smc.x; }
            else if (ax == 2u) { axis_val = sp.z - smc.z; }
            d += (min(abs(axis_val), abs(str)) * -sign(axis_val)) * mask_w * 0.5;
        } else if (sculpt_op == 3u) {
            let proximity = exp(-abs(d) * 4.0);
            d -= s * proximity;
        } else if (sculpt_op == 4u) {
            var dx = sp.x - smc.x; var dz = sp.z - smc.z;
            if (ax == 1u) { dx = sp.y - smc.y; dz = sp.z - smc.z; }
            else if (ax == 2u) { dx = sp.x - smc.x; dz = sp.y - smc.y; }
            let lateral = sqrt(dx * dx + dz * dz);
            d += lateral * s * 0.3;
        } else if (sculpt_op == 5u) {
            var dx = sp.x - smc.x; var dz = sp.z - smc.z;
            if (ax == 1u) { dx = sp.y - smc.y; dz = sp.z - smc.z; }
            else if (ax == 2u) { dx = sp.x - smc.x; dz = sp.y - smc.y; }
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
    // SDF subdivision rounding
    let subdiv = prim_subdivision(idx);
    if (subdiv >= 6u)      { d -= 0.18; }
    else if (subdiv == 5u) { d -= 0.10; }
    else if (subdiv == 4u) { d -= 0.05; }
    else if (subdiv == 3u) { d -= 0.02; }
    else if (subdiv == 2u) { d -= 0.008; }
    else if (subdiv == 1u) { d -= 0.003; }
    // Clip curves: iterate ALL clips from the separate clip_curves buffer.
    // st==9 CurveExtrude and st==12 SpinePipe own the clip buffer with a different layout —
    // must NOT run the generic 2D cutter walk on them (OOB / garbage → device lost).
    if (prim_has_clips(idx) && st != 9u && st != 12u) {
        let cc_count = prim_clip_count(idx);
        var buf_pos = prim_clip_buf_start(idx);
        for (var ci = 0u; ci < cc_count; ci++) {
            let cc_mode = bitcast<u32>(clip_curves[buf_pos]);
            let vc = min(bitcast<u32>(clip_curves[buf_pos + 1u]), 64u);
            if (vc >= 3u) {
                let cu = vec3(clip_curves[buf_pos + 2u], clip_curves[buf_pos + 3u], clip_curves[buf_pos + 4u]);
                let cv = vec3(clip_curves[buf_pos + 5u], clip_curves[buf_pos + 6u], clip_curves[buf_pos + 7u]);
                let verts_start = buf_pos + 8u;
                let clip_d = sdf_polygon_2d_gpu(vec2(dot(eval_p, cu), dot(eval_p, cv)), verts_start, vc);
                if (cc_mode == 0u) { d = max(d, -clip_d); }    // Subtract
                else { d = max(d, clip_d); }                     // KeepInside
            }
            buf_pos += 8u + vc * 2u; // advance to next clip
        }
    }
    return d * min_s;
}

// ============================================================
// Boolean ops (copied verbatim from compute.wgsl)
// ============================================================
fn smin(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return min(a, b); } let h=clamp(0.5+0.5*(a-b)/k,0.0,1.0); return mix(a,b,h)-k*h*(1.0-h); }
fn smax(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return max(a, b); } let h=clamp(0.5-0.5*(a-b)/k,0.0,1.0); return mix(a,b,h)+k*h*(1.0-h); }
fn chamfer_min(a: f32, b: f32, k: f32) -> f32 { if (k <= 1e-6) { return min(a, b); } return min(min(a,b), (a-k+b)*0.7071067811865476); }
fn smin_stairs(a: f32, b: f32, k: f32) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    let n = 4.0; // step count
    let s = k / n;
    let u = b - k;
    let x = u - a + s;
    let period = 2.0 * s;
    let m = x - period * floor(x / period); // GLSL-style mod (WGSL % is remainder)
    return min(min(a, b), 0.5 * (u + a + abs(m - s)));
}
// Circular fillet (IQ). profile 2 = Circular.
fn smin_circular(a: f32, b: f32, k: f32) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    let kk = k / (1.0 - sqrt(0.5));
    let h = max(kk - abs(a - b), 0.0) / kk;
    return min(a, b) - kk * 0.5 * (1.0 + h - sqrt(max(1.0 - h * (h - 2.0), 0.0)));
}
// Multipoint blend-profile curve: 4 y-samples at x=0,1/3,2/3,1 packed in hot[80-83]
// when blend noise is off (noise uses u32 octave count 1..16 in hot[82] bits).
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
fn prim_blend_curve_lut(idx: u32) -> vec4<f32> {
    // Noise mode: amp>0 and octave field is a small integer bit-pattern
    let amp = prim_f(idx, 80u);
    let oct_u = prim_u(idx, 82u);
    if (amp > 0.001 && oct_u >= 1u && oct_u <= 16u) {
        // Identity transfer (linear) — noise path owns these slots
        return vec4(0.0, 0.333333, 0.666667, 1.0);
    }
    return vec4(prim_f(idx, 80u), prim_f(idx, 81u), prim_f(idx, 82u), prim_f(idx, 83u));
}
// Polynomial smin with mix-weight warped by multipoint curve LUT
fn smin_poly_curved(a: f32, b: f32, k: f32, lut: vec4<f32>) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    var h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    h = lut4_sample(lut, h);
    return a * h + b * (1.0 - h) - k * h * (1.0 - h);
}
// Blend profiles (append-only): 0=Smooth, 1=Chamfer, 2/3=legacy → Smooth.
// Curve LUT feature removed (owner). shape/lut kept for call-site parity only.
fn smin_profiled(a: f32, b: f32, k: f32, profile: u32) -> f32 {
    return smin_profiled_shape(a, b, k, profile, 1.0);
}
fn smin_profiled_shape(a: f32, b: f32, k: f32, profile: u32, shape: f32) -> f32 {
    return smin_profiled_curve(a, b, k, profile, shape, vec4(0.0, 0.333333, 0.666667, 1.0));
}
fn smin_profiled_curve(a: f32, b: f32, k: f32, profile: u32, shape: f32, lut: vec4<f32>) -> f32 {
    _ = shape;
    _ = lut;
    if (profile == 1u) {
        return chamfer_min(a, b, k);
    }
    return smin(a, b, k);
}
fn smax_profiled(a: f32, b: f32, k: f32, profile: u32) -> f32 { return -smin_profiled(-a, -b, k, profile); }
fn smax_profiled_shape(a: f32, b: f32, k: f32, profile: u32, shape: f32) -> f32 {
    return -smin_profiled_shape(-a, -b, k, profile, shape);
}
fn smax_profiled_curve(a: f32, b: f32, k: f32, profile: u32, shape: f32, lut: vec4<f32>) -> f32 {
    return -smin_profiled_curve(-a, -b, k, profile, shape, lut);
}

// #region NOISY_BLEND
// Blend-zone noise displacement
fn eval_blend_noise(p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32) -> f32 {
    if (amp < 1e-6) { return 0.0; }
    let n = fbm_wgsl(p * freq, max(octaves, 1u));
    return n * amp;
}
fn smin_noisy(a: f32, b: f32, k: f32, p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32, bp: u32) -> f32 {
    if (k <= 1e-6) { return min(a, b); }
    let base = smin_profiled(a, b, k, bp);
    if (amp < 1e-6) { return base; }
    let h = clamp(0.5 + 0.5 * (a - b) / k, 0.0, 1.0);
    let mask = 4.0 * h * (1.0 - h);
    let n = eval_blend_noise(p, amp, freq, octaves, noise_type);
    return base + n * mask;
}
fn smax_noisy(a: f32, b: f32, k: f32, p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32, bp: u32) -> f32 {
    return -smin_noisy(-a, -b, k, p, amp, freq, octaves, noise_type, bp);
}
// #endregion NOISY_BLEND

// ============================================================
// SEAM boolean (op 5 = cut groove, op 6 = raised bead) — CPU twin: sdf_math.rs seam_cut.
// a = accumulator field, b = seam prim field, w = half-width (b-units), dep = depth (a-units),
// profile: 0=V, 1=U (elliptical), 2=Square (hg_sdf fOpGroove). EVERY profile is clamped by the
// wall term (w - |b|), so far from B (|b| >= w + |a|) the result is EXACTLY a — the op-aware
// culls below rely on this identity (skip when dist_to_prim > width + |acc|).
// Formulations mirror the CPU float-for-float (division + sqrt, same order) for bake parity.
// ============================================================
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

// EXTRUDE boolean (op 7) — CPU twin: sdf_math.rs extrude_op (keep float-for-float identical;
// smoothstep written out for parity). a = accumulator, b = extrude prim field, w = falloff
// band, s = SIGNED strength (>0 raise/emboss out, <0 recess/deboss in). The mask saturates
// to 0 EXACTLY at b >= w — the op-aware culls rely on this identity (skip when
// dist_to_prim > L * width, no accumulator term needed unlike Seam).
fn extrude_op(a: f32, b: f32, w0: f32, s: f32) -> f32 {
    let w = max(w0, 1e-4);
    let t = clamp(b / w, 0.0, 1.0);
    let mask = 1.0 - t * t * (3.0 - 2.0 * t);
    return a - s * mask;
}

// Lateral inset (inside +) for Protect Walls.
// axis: 0=Auto (shortest extent = depth), 1=X walls, 2=Y walls, 3=Z walls.
// KEEP LOCKSTEP with core_math::sdf_math::wall_protect_lateral_inset_axis.
fn wall_protect_lateral_inset(sp: vec3<f32>, he: vec3<f32>, st: u32, axis: u32) -> f32 {
    // Cylinder(2) / Capsule(5) / Cone(6) / SpiralStair(8): radial side wall
    if (st == 2u || st == 5u || st == 6u || st == 8u) {
        return he.x - length(vec2(sp.x, sp.z));
    }
    if (axis == 1u) { return he.x - abs(sp.x); }
    if (axis == 2u) { return he.y - abs(sp.y); }
    if (axis == 3u) { return he.z - abs(sp.z); }
    // Auto: shortest half-extent = cut depth; protect the other two faces
    let dx = he.x; let dy = he.y; let dz = he.z;
    if (dx <= dy && dx <= dz) {
        return min(he.y - abs(sp.y), he.z - abs(sp.z));
    } else if (dy <= dz) {
        return min(he.x - abs(sp.x), he.z - abs(sp.z));
    } else {
        return min(he.x - abs(sp.x), he.y - abs(sp.y));
    }
}

fn prim_blend_profile_shape(idx: u32) -> f32 { return prim_f(idx, 91u); }

fn compute_local_k(idx: u32, p: vec3<f32>, k: f32) -> f32 {
    var k_out = k;
    // Directional falloff (smoothstep ramp)
    let packed = prim_falloff_packed(idx);
    let fl = length(packed);
    if (fl >= 0.001) {
        let axis = packed / fl;
        let origin = prim_pos(idx) + axis * prim_falloff_offset(idx);
        let t = clamp(dot(p - origin, axis) / fl, 0.0, 1.0);
        k_out *= t * t * (3.0 - 2.0 * t);
    }
    // Protect walls: hot[87] = axis*100 + margin
    let raw_wp = prim_wall_protect(idx);
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
        let wd = wall_protect_lateral_inset(sp, he, st, wall_axis);
        let tw = clamp(wd / margin, 0.0, 1.0);
        k_out *= tw * tw * (3.0 - 2.0 * tw);
    }
    return k_out;
}

fn apply_boolean(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32) -> f32 {
    // Shape handle defaults to 1 when not passed (legacy callers).
    return apply_boolean_shaped(result, d, op, k, bp, sd, 1.0, vec4(0.0, 0.333333, 0.666667, 1.0));
}

/// Alias used by the lightweight apply_boolean_blend stub (shader_build.rs).
fn apply_boolean_lut(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32, shape: f32, lut: vec4<f32>) -> f32 {
    return apply_boolean_shaped(result, d, op, k, bp, sd, shape, lut);
}

fn apply_boolean_shaped(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32, shape: f32, lut: vec4<f32>) -> f32 {
    if (op == 0u) { return select(min(result, d), smin_profiled_curve(result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 1u) { return select(max(result, -d), -smin_profiled_curve(-result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 2u) { return select(max(result, d), smax_profiled_curve(result, d, k, bp, shape, lut), k > 0.0); }
    else if (op == 3u) { return select(min(result, d), smin_profiled_curve(result, d, k, 1u, shape, lut), k > 0.0); }
    else if (op == 4u) { return select(max(result, -d), -smin_profiled_curve(-result, d, k, 1u, shape, lut), k > 0.0); }
    else if (op == 5u) { return seam_cut(result, d, k, sd, bp); }
    else if (op == 6u) { return -seam_cut(-result, d, k, sd, bp); }
    else if (op == 7u) { return extrude_op(result, d, k, sd); }
    return result;
}
// #region NOISY_BLEND
fn apply_boolean_blend(result: f32, d: f32, op: u32, k: f32, bp: u32, sd: f32,
                       p: vec3<f32>, amp: f32, freq: f32, octaves: u32, noise_type: u32) -> f32 {
    // Noise only when amp>0 and octaves is a real count (1..16). Curve LUT packs plain 0..1
    // floats into these slots — bitcast octaves is huge, so noise stays off.
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

// ============================================================
// Bake entry point — scope-aware stack machine
// ============================================================

// LAB poly→SDF (iteration 2) — per-prim mesh distance grid sampling. The mesh_grid buffer
// (@binding 5) packs one grid per MeshVolume prim: [res, min.xyz, cell, 3 reserved] header at
// `base` + res^3 signed distances. These functions reference mesh_grid, so they MUST stay
// BELOW the "Bake entry point" marker — extract_bake_sdf_lib() slices from `fn prim_f(` to
// this marker into brick_fill.wgsl (no mesh_grid binding). MeshVolume is therefore evaluated
// in the MAIN scope loop (below), never in eval_prim (which is extracted).
fn mesh_grid_at(base: u32, x: u32, y: u32, z: u32, res: u32) -> f32 {
    return mesh_grid[base + 8u + x + y * res + z * res * res];
}

/// Trilinear sample of the grid whose header starts at `base`, evaluated at a shape-local
/// point `mp`. Clamp-sample inside the box + euclidean distance to the box outside it
/// (continuous upper bound). Mirrors MeshSdfData::sample (core_math) for CPU/GPU parity.
fn sample_mesh_grid_at(base: u32, mp: vec3<f32>) -> f32 {
    if (base + 9u > arrayLength(&mesh_grid)) { return 1e6; }
    let res = u32(mesh_grid[base]);
    if (res < 2u) { return 1e6; }
    let cell = mesh_grid[base + 4u];
    if (cell <= 0.0) { return 1e6; }
    let gmin = vec3(mesh_grid[base + 1u], mesh_grid[base + 2u], mesh_grid[base + 3u]);
    let hi = f32(res - 1u);
    let g = (mp - gmin) / cell;
    let g_cl = clamp(g, vec3(0.0), vec3(hi));
    let d_out = length(g - g_cl) * cell;
    let g0 = min(vec3<u32>(g_cl), vec3(res - 2u));
    let f = clamp(g_cl - vec3<f32>(g0), vec3(0.0), vec3(1.0));
    let c000 = mesh_grid_at(base, g0.x,      g0.y,      g0.z,      res);
    let c100 = mesh_grid_at(base, g0.x + 1u, g0.y,      g0.z,      res);
    let c010 = mesh_grid_at(base, g0.x,      g0.y + 1u, g0.z,      res);
    let c110 = mesh_grid_at(base, g0.x + 1u, g0.y + 1u, g0.z,      res);
    let c001 = mesh_grid_at(base, g0.x,      g0.y,      g0.z + 1u, res);
    let c101 = mesh_grid_at(base, g0.x + 1u, g0.y,      g0.z + 1u, res);
    let c011 = mesh_grid_at(base, g0.x,      g0.y + 1u, g0.z + 1u, res);
    let c111 = mesh_grid_at(base, g0.x + 1u, g0.y + 1u, g0.z + 1u, res);
    let c00 = mix(c000, c100, f.x);
    let c10 = mix(c010, c110, f.x);
    let c01 = mix(c001, c101, f.x);
    let c11 = mix(c011, c111, f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z) + d_out;
}

/// MeshVolume prim distance: transform the sample point into shape-local space (rigid +
/// scale + mirror — the same first block as eval_prim), sample the grid, and rescale to the
/// world metric by min(scale). Domain-warp/surface modifiers are NOT applied (converted
/// prims are created modifier-free; documented spike limitation — parity note in the lab log).
fn eval_mesh_volume(idx: u32, p: vec3<f32>) -> f32 {
    let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
    let sc = prim_scale(idx);
    var sp = local_p / sc;
    let mb = prim_mirror_bits(idx);
    let mc = prim_mirror_center(idx);
    if ((mb & 1u) != 0u) { sp.x = abs(sp.x - mc.x) + mc.x; }
    if ((mb & 2u) != 0u) { sp.y = abs(sp.y - mc.y) + mc.y; }
    if ((mb & 4u) != 0u) { sp.z = abs(sp.z - mc.z) + mc.z; }
    let raw = sample_mesh_grid_at(prim_mesh_grid_start(idx), sp);
    return raw * min(sc.x, min(sc.y, sc.z));
}

@compute @workgroup_size(4, 4, 4)
fn bake_volume(@builtin(global_invocation_id) gid: vec3<u32>) {
    // Offset gid by bake_min to get actual voxel coordinates
    let voxel = gid + params.bake_min;
    // Exclusive upper bound check
    if (any(voxel >= params.bake_max)) { return; }

    // Atlas padding: when atlas_pad > 0, grid_res includes 2*pad extra voxels per axis.
    // Body data lives at voxels [pad .. pad+body_res) where body_res = grid_res - 2*pad.
    // Padding voxels [0..pad) and [pad+body_res..grid_res) contain valid SDF data
    // evaluated at world positions extending beyond [bounds_min, bounds_max].
    let pad = params.atlas_pad;
    let body_res = params.grid_res - vec3(2u * pad);

    // Flat buffer index: ONLY for voxels inside the body data region [pad, pad+body_res).
    // Padding voxels are written to the atlas texture but NOT to the flat output buffer.
    // Index formula matches CPU body_volume_cache.rs: grid[(ix + iy*res + iz*res*res)]
    let in_body = all(voxel >= vec3(pad)) && all(voxel < vec3(pad) + body_res);
    let bv = voxel - vec3(pad); // body-relative voxel (only valid when in_body)
    let idx = bv.x + bv.y * body_res.x + bv.z * body_res.x * body_res.y;

    // SparseBricks (output_mode=2) SENTINEL EARLY-OUT: a voxel whose brick carries an
    // EXTERIOR/INTERIOR sentinel is NEVER written in mode 2 (the pool write below checks the
    // same tag, and mode 2 never textureStores) — yet the full prim-stack eval used to run
    // anyway, wasting 60-90% of a partial bake's ALU over a drag window's swept interior/
    // exterior bulk. Placed AFTER the pad/in_body/bv computation and gated on `in_body` (NOT on
    // the raw bounds check: `bv = voxel - pad` underflows u32 for padding voxels; mode 2 forces
    // pad=0 today but the shader is shared, so the guard is structural). The 4×4×4 workgroup
    // spans one octant of a single 8³ brick — all 64 threads read the same index entry: a
    // uniform, coherent branch (near-zero cost, whole workgroup retires together).
    if (params.output_mode == 2u) {
        if (!in_body) { return; } // mode 2 writes body voxels only (no texture, no padding)
        let eo_brick = bv / 8u;
        let eo_linear = eo_brick.x
            + eo_brick.y * params.dims_bricks_x
            + eo_brick.z * params.dims_bricks_x * params.dims_bricks_y;
        if (brick_index[eo_linear] >= 0xFFFFFFFEu) { return; } // sentinel — never written
    }

    // World position: endpoint-inclusive linear interpolation matching CPU path.
    // With padding, voxel `pad` maps to bounds_min, voxel `pad+body_res-1` maps to bounds_max.
    // Padding voxels extrapolate beyond the nominal bounds (valid SDF evaluation).
    // CPU: p = -bounds + ix * step, where step = (bounds*2) / (res-1)
    // Equivalent: mix(bounds_min, bounds_max, (voxel - pad) / (body_res - 1))
    let denom = vec3<f32>(max(body_res - vec3(1u), vec3(1u)));
    let t = vec3<f32>(vec3<i32>(voxel) - vec3<i32>(vec3(pad))) / denom;
    var world_pos = mix(params.bounds_min, params.bounds_max, t);

    // NOTE: the root body's mirror is NOT applied here. The top-level body is emitted as a
    // SCOPE_OPEN carrying its mirror_flags/center at fields 45-49 (serialize.rs), so the
    // scope-machine UNION below handles it correctly. The old root-level fold here was a
    // redundant SECOND application (abs∘abs is idempotent) that pre-folded world_pos and
    // collapsed an off-center / two-sided body before the union could run — removed.
    // (`params.mirror_flags`/`mirror_sign`/`mirror_center_*` remain in the params struct;
    //  they still drive the grid bounds expansion on the CPU side.)

    // Scene eval extracted into eval_scene_at() so the GPU-classify entry (classify_bricks)
    // reuses the EXACT same chain — ONE call site here + ONE there (Naga inlines calls; two
    // linear copies, no exponential expansion).
    let acc = eval_scene_at(world_pos);

    // (Iteration-1 global operand union removed — MeshVolume is now a real prim evaluated
    //  inside the scope loop above, so it participates in the boolean tree per-prim.)

    // Root-body bevel is ALREADY applied once at the root SCOPE_CLOSE via field-14 (prim_f(stack_oi,14u)),
    // which handles root + nested bodies uniformly (the top-level body is emitted as a SCOPE_OPEN). Re-
    // subtracting params.bevel_radius here DOUBLE-applied the root bevel — a 2× inflate the moment the user
    // raised the Edge Bevel slider. Removed. params.bevel_radius stays in BakeParams for compat but is no
    // longer consumed. (Review #1. CPU eval_scene applies 0× root bevel — a separate parity follow-up.)

    // Flat buffer: only write body-data voxels (skip padding voxels).
    // The flat buffer is body_res^3, not grid_res^3 (no padding in CPU readback).
    // In TextureOnly mode (output_mode=1), the flat buffer is a dummy — skip writes.
    if (in_body && params.output_mode == 0u) {
        output_grid[idx] = acc;
    }
    // SparseBricks (output_mode=2): write this voxel into its 8³ brick in the pool arena.
    // In this mode binding 2 (output_grid) IS the pool buffer — same wgpu buffer type,
    // rebound at dispatch. Layout matches the production decode EXACTLY:
    //   sdf_mesh.wgsl fld(): brick_pool[tag * 512u + (x&7) + (y&7)*8 + (z&7)*64]
    //   CPU BrickGridAccessor (core_math/chunked_dc.rs): pool[handle*512 + lx + ly*8 + lz*64]
    // Brick coords use the BODY-space voxel (bv — pre-atlas-offset, the same domain as the
    // flat write above). Sentinel handles (EXTERIOR 0xFFFFFFFF / INTERIOR 0xFFFFFFFE) are
    // CPU-owned constants with no pool slot — never written here.
    if (in_body && params.output_mode == 2u) {
        let brick = bv / 8u;
        let brick_linear = brick.x
            + brick.y * params.dims_bricks_x
            + brick.z * params.dims_bricks_x * params.dims_bricks_y;
        // (`handle` is a WGSL reserved keyword — hence `pool_handle`.)
        // lab↔cache binding reconciliation DONE: brick_index owns binding 6; mesh_grid keeps
        // binding 5 (a sparse body may contain MeshVolume prims — the scope loop samples
        // mesh_grid regardless of output mode, so BOTH must be live in mode 2). The merge's
        // interim bitcast-through-mesh_grid read is gone — it made every sparse bake read LAB
        // mesh floats as pool handles (the stepped-ledge surface corruption).
        let pool_handle = brick_index[brick_linear];
        if (pool_handle < 0xFFFFFFFEu) {
            let local = (bv.x & 7u) + (bv.y & 7u) * 8u + (bv.z & 7u) * 64u;
            output_grid[pool_handle * 512u + local] = acc;
        }
    }
    // Direct-to-texture: write ALL voxels including padding to the atlas sub-region.
    // atlas_offset shifts the write into the correct 3D cell of the atlas.
    // When all offsets = 0, this writes to a per-body ResidentVolume (backward compat).
    // SparseBricks (mode 2) has no texture target — binding 3 is a 1³ dummy; skip the store.
    if (params.output_mode != 2u) {
        let atlas_voxel = vec3<u32>(
            voxel.x + params.atlas_offset_x,
            voxel.y + params.atlas_offset_y,
            voxel.z + params.atlas_offset_z
        );
        textureStore(output_tex, atlas_voxel, vec4(acc, 0.0, 0.0, 0.0));
    }
}

// ============================================================
// Scene field at one world point — the scope-aware stack machine extracted VERBATIM from
// bake_volume (the mode-0/1/2 bake eval, proven CPU-parity by tests_bake/tests_sparse_bake).
// Two call sites only: bake_volume (1 eval/voxel) and classify_bricks (9 evals/brick, looped
// so Naga inlines a SINGLE copy there). Any eval change here changes BOTH consumers — that is
// the point (the classifier must agree with the field the bake writes).
// ============================================================
fn eval_scene_at(world_pos: vec3<f32>) -> f32 {
    // Scope-aware stack machine evaluator (matches scene_analytical() in sdf_shared.wgsl)
    var acc = 999.0;
    var stack_d: array<f32, 8>;   // saved accumulator per depth
    var stack_op: array<u32, 8>;  // scope boolean op
    var stack_k: array<f32, 8>;   // scope blend k
    var stack_bp: array<u32, 8>;  // scope blend profile
    var stack_p: array<vec3f, 8>; // saved PRE-mirror evaluation point (for restore + per-combo reflection)
    var stack_oi: array<u32, 8>;  // SCOPE_OPEN entry index (for bevel_radius readback + re-seek)
    var stack_mflags: array<u32, 8>; // body mirror axis mask for this scope (0 = no mirror)
    var stack_mc: array<vec3f, 8>;   // body mirror plane center for this scope
    var stack_ncombo: array<u32, 8>; // number of reflection combos for this scope (1 when no mirror)
    var stack_combo: array<u32, 8>;  // current combo index in [0, ncombo)
    var stack_smin: array<f32, 8>;   // running min of the scope accumulator across combos (the union)
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
            // Scope max Lipschitz constant (stored at hot[74] in bake path).
            // L=1.0 for all standard shapes. When L>1 (future: twist/bend),
            // the skip threshold widens to stay conservative.
            let scope_L = prim_lipschitz(i);

            // AABB scope-skip optimization (Lipschitz-adjusted)
            // Union:    skip when d_aabb > L * (acc + scope_k)
            // Subtract: skip when acc >= 0 and d_aabb > L * scope_k
            var skip_scope = false;
            if (d_aabb > 0.0) {
                if ((scope_op == 0u || scope_op == 3u) && d_aabb > scope_L * (acc + scope_k)) {
                    skip_scope = true;
                } else if ((scope_op == 1u || scope_op == 4u) && acc >= 0.0 && d_aabb > scope_L * scope_k) {
                    skip_scope = true;
                } else if ((scope_op == 5u || scope_op == 6u) && d_aabb > scope_L * (scope_k + abs(acc))) {
                    // Seam scope: EXACT identity beyond width + |acc| of the child field
                    // (wall clamp in seam_cut) — same proof as the per-prim seam cull.
                    skip_scope = true;
                } else if (scope_op == 7u && d_aabb > scope_L * scope_k) {
                    // Extrude scope: EXACT identity beyond width of the child field
                    // (smoothstep mask saturates), independent of acc.
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
            stack_p[sp]  = p;           // PRE-mirror point — reflected per combo, restored at CLOSE
            stack_oi[sp] = i;

            // Body mirror, two modes selected by mirror_sign (field 49) bit 7:
            //  - UNION (bit clear, the legacy mode every pre-cut save stores): min over the 2^n
            //    reflection sign-combos of the n enabled axes. Geometry on EITHER side of each
            //    plane survives; the scope body is walked once PER COMBO (the CLOSE re-seek).
            //  - CUT (bit set, "delete the other half"): one-sided fold onto the source half —
            //    ONE walk of the scope body regardless of axis count, and the non-source half is
            //    gone. mirror_sign bits 0-2 pick which half is the source.
            // Both are byte-identical to the un-mirrored path when mflags == 0.
            let mflags = prim_u(i, 45u);
            let msign = prim_u(i, 49u);
            let cut = mflags != 0u && (msign & MIRROR_CUT_BIT) != 0u;
            stack_mflags[sp] = mflags;
            stack_mc[sp] = vec3f(prim_f(i, 46u), prim_f(i, 47u), prim_f(i, 48u));
            // ncombo == 1 under CUT, so the SCOPE_CLOSE re-seek below never fires and never
            // needs the sign — that is why msign is not stacked.
            stack_ncombo[sp] = select(1u << countOneBits(mflags), 1u, cut);
            stack_combo[sp]  = 0u;
            stack_smin[sp]   = 999.0;
            sp += 1u;
            acc = 999.0;

            // Enter the scope with the point mapped for this mode. Union combo 0 = no bits set
            // => identity (p unchanged) when there is no mirror.
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
            if (sp == 0u) { continue; } // safety: malformed stream
            let top = sp - 1u;

            // Apply body-level bevel (edge rounding) to THIS combo's accumulator.
            let bevel_r = prim_f(stack_oi[top], 14u);
            if (acc < 998.0 && bevel_r > 0.0) { acc -= bevel_r; }

            // Fold this combo's result into the scope union.
            stack_smin[top] = min(stack_smin[top], acc);

            let combo = stack_combo[top] + 1u;
            if (combo < stack_ncombo[top]) {
                // More reflection combos to evaluate: re-seek to SCOPE_OPEN+1 and reflect for `combo`.
                stack_combo[top] = combo;
                acc = 999.0;
                p = reflect_for_combo(stack_p[top], stack_mflags[top], stack_mc[top], combo);
                i = stack_oi[top]; // loop's i++ lands on the first prim inside the scope
                continue;
            }

            // All combos done: pop, restore pre-mirror point, combine union with parent.
            sp = top;
            let scope_acc = stack_smin[top];
            p = stack_p[top];

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
            continue;
        }

        // --- Regular primitive ---
        let op = prim_u(i, 11u);
        let k_raw = prim_f(i, 15u);

        // Per-prim bounding sphere cull (Lipschitz-adjusted): skip prims whose
        // influence sphere cannot affect the accumulator at this voxel position.
        // bound_r already includes shape extent + scale + blend k + modifier padding.
        // NEVER skip the first prim in a scope (acc > 998.0) — RAY_CULL rule.
        //
        // Lipschitz adjustment: for a prim with gradient bound L, the SDF at
        // distance dist_to_prim from the bounding sphere satisfies:
        //   eval_prim(i, p) >= dist_to_prim / L
        // So we skip when dist_to_prim / L > acc + k, i.e. dist_to_prim > L*(acc+k).
        // At L=1.0 (standard shapes) this is identical to the original condition.
        if (acc < 998.0) {
            let prim_center = prim_pos(i);
            let dist_to_prim = length(p - prim_center) - prim_bound_r(i);
            if (dist_to_prim > 0.0) {
                let L = prim_lipschitz(i);
                // Union / ChamferUnion: prim too far to blend with current surface
                if ((op == 0u || op == 3u) && dist_to_prim > L * (acc + k_raw)) { continue; }
                // Subtract / ChamferSubtract: point outside prim and already outside surface
                if ((op == 1u || op == 4u) && acc >= 0.0 && dist_to_prim > L * k_raw) { continue; }
                // Seam (5/6): EXACT skip — seam_cut's wall clamp makes the op the identity
                // whenever |b| >= width + |acc| (b >= dist_to_prim / L out here). Proven by
                // core_math seam_tests::seam_far_b_is_identity; mirrors fold_prim (sdf_math.rs).
                if ((op == 5u || op == 6u) && dist_to_prim > L * (k_raw + abs(acc))) { continue; }
                // Extrude (7): EXACT skip — the smoothstep mask saturates to 0 whenever
                // b >= width, independent of acc. Proven by extrude_far_b_is_identity.
                if (op == 7u && dist_to_prim > L * k_raw) { continue; }
                // Intersect / SmoothIntersect (op==2u): NEVER far-skip. max(acc,d) needs a
                // far-outside point to yield d (large positive = empty), never acc. Skipping it
                // keeps the un-intersected surface = stale geometry. Only Union/ChamferUnion and
                // Subtract/ChamferSubtract can be safely culled here (matches sdf_scene.wgsl,
                // which skips only op==0u||op==3u). See SDF_TOOLS_PLAN Item 1.
            }
        }

        // MeshVolume (st==10) samples its per-prim distance grid (mesh_grid @binding 5),
        // evaluated HERE in the main loop (not eval_prim, which is extracted into brick_fill
        // and has no mesh_grid binding). It then flows through the identical boolean+blend
        // below, so union/subtract/intersect/smooth-K all apply exactly like a native shape.
        var d = eval_prim(i, p);
        if (st == 10u) { d = eval_mesh_volume(i, p); }
        let k = compute_local_k(i, p, k_raw);
        let bp = prim_blend_profile(i);
        let shape = prim_blend_profile_shape(i);
        let lut = prim_blend_curve_lut(i);

        if (acc > 998.0) {
            acc = d;
        } else {
            // Profile type + shape scale + multipoint curve LUT.
            acc = apply_boolean_shaped(acc, d, op, k, bp, prim_seam_depth(i), shape, lut);
        }
    }

    // Flush any unclosed scopes (safety — matches scene_analytical)
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

    return acc;
}

// ============================================================
// GPU-classify spike (stage 1, TEST-ONLY dispatch today): one thread per BRICK, replicating
// BrickClassifier::classify_one_brick (core_math/chunked_dc.rs) — 9 analytic samples (8 corners
// + center) of the brick's clamped vertex span, thresholded with the CPU-computed band+margin.
// The CPU's superblock/containment-proof passes are pure SPEEDUPS with flag-identical output,
// so brute-forcing every brick here is verdict-equivalent by construction.
//
// Bindings: SAME bake BGL (graphics::bgl::bake_bgl_entries). Verdicts go to binding 2
// (output_grid, read_write) as one u32 per brick, bitcast into the f32 arena — binding 6
// (brick_index) is read-only storage in this BGL, so it cannot take the output; a classify
// dispatch binds a dims³-f32 buffer at binding 2 and dummies elsewhere (never runs concurrently
// with a bake). Verdict encoding: 0 = EXTERIOR, 1 = INTERIOR, 2 = SURFACE.
//
// Uniform reuse: grid_res.x = nverts, bounds_min/max = mirror-expanded bake bounds,
// dims_bricks_* = brick-grid dims, prim_count/floats_per_prim as in a bake; brick_dim /
// classify_band / classify_margin ride the relabeled spare pads (see BakeParams).
// O1 hierarchical classify — the workgroup coarse cull. workgroup_size(4,4,4) = 64 bricks =
// EXACTLY one 4³-brick super-cell (= one 32³-voxel tile: the octree pyramid's first level).
// Thread 0 evaluates the field ONCE at the super-cell's clamped-span center; if the 1-Lipschitz
// bound (half-diagonal + band + the classifier's non-Lipschitz margin scaled ×4 to the coarse
// diagonal) proves every 9-sample of every brick inside stays strictly outside the fine
// thresholds, all 64 bricks take the uniform verdict WITHOUT their 9 evals — BIT-IDENTICAL to
// the fine result by construction:
//   EXTERIOR cull: d_c − R > band+margin ⇒ every sample > band+margin ⇒ fine mn > band+margin
//                  (surface test fails) and mx > band+margin > −margin (interior fails) ⇒ 0.
//   INTERIOR cull: d_c + R < −(band+margin) ⇒ every sample < −(band+margin) ⇒ surface fails
//                  (mx < −(band+margin)) and mx ≤ −margin holds ⇒ 1.
// Gated on params.coarse_cull (CPU: coarse_cull_safe — provably 1-Lipschitz folds only).
// BARRIER UNIFORMITY: the workgroupBarrier precedes the out-of-bounds return — every thread of
// the workgroup reaches it (WGSL requires barriers in uniform control flow).
var<workgroup> coarse_skip: u32; // 0 = no cull, 1 = all-EXTERIOR, 2 = all-INTERIOR

@compute @workgroup_size(4, 4, 4)
fn classify_bricks(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let dims = vec3(params.dims_bricks_x, params.dims_bricks_y, params.dims_bricks_z);

    let nverts = params.grid_res.x;
    let bd = params.brick_dim;
    let last = f32(nverts - 1u);
    var denom0 = 1.0;
    if (nverts > 1u) { denom0 = f32(nverts - 1u); }
    let extent0 = params.bounds_max - params.bounds_min;

    if (li == 0u) {
        coarse_skip = 0u;
        if (params.coarse_cull == 1u) {
            // The workgroup's brick span, clamped to the grid (partial edge groups shrink).
            let b0 = min(wid * 4u, dims - vec3(1u));
            let b1 = min(wid * 4u + 3u, dims - vec3(1u));
            // Clamped vertex span of the whole super-cell — same construction as the per-brick
            // spans below, so every brick's 9 sample points provably lie inside [sv0, sv1].
            let sv0 = min(vec3<f32>(b0 * bd), vec3(last));
            let sv1 = min(vec3<f32>(b1 * bd + (bd - 1u)), vec3(last));
            let center = (sv0 + sv1) * 0.5;
            let d_c = eval_scene_at(params.bounds_min + center / denom0 * extent0);
            // R = world half-diagonal of the span; margin ×4 = the fine classifier's own
            // non-Lipschitz allowance (factor × diagonal) scaled from brick to super-cell diag.
            let r = 0.5 * length((sv1 - sv0) / denom0 * extent0);
            let thresh = r + params.classify_band + params.classify_margin * 4.0;
            if (d_c > thresh) { coarse_skip = 1u; }
            else if (d_c < -thresh) { coarse_skip = 2u; }
        }
    }
    workgroupBarrier();

    if (any(gid >= dims)) { return; }
    if (coarse_skip != 0u) {
        let bi0 = gid.x + gid.y * dims.x + gid.z * dims.x * dims.y;
        // 1 → EXTERIOR (0), 2 → INTERIOR (1).
        output_grid[bi0] = bitcast<f32>(coarse_skip - 1u);
        return;
    }
    // Clamped vertex span of this brick — BrickClassifier::brick_span_verts verbatim.
    let v0 = min(vec3<f32>(gid * bd), vec3(last));
    let v1 = min(vec3<f32>(gid * bd + (bd - 1u)), vec3(last));

    // Endpoint-inclusive vertex→world mapping — BrickClassifier::vertex_world verbatim
    // (grid_min + g/denom * extent, NOT mix(): same operation order as the CPU for parity).
    var denom = 1.0;
    if (nverts > 1u) { denom = f32(nverts - 1u); }
    let extent = params.bounds_max - params.bounds_min;

    var mn = 1e30;
    var mx = -1e30;
    for (var c = 0u; c < 9u; c++) {
        var g: vec3<f32>;
        if (c < 8u) {
            g = vec3(
                select(v0.x, v1.x, (c & 1u) != 0u),
                select(v0.y, v1.y, (c & 2u) != 0u),
                select(v0.z, v1.z, (c & 4u) != 0u),
            );
        } else {
            g = (v0 + v1) * 0.5; // center sample
        }
        let d = eval_scene_at(params.bounds_min + g / denom * extent);
        mn = min(mn, d);
        mx = max(mx, d);
    }

    // BrickClassifier::classify_one_brick thresholds verbatim (Surface wins ties; the interior
    // cutoff is -margin, NOT -(band+margin)).
    let band = params.classify_band;
    let margin = params.classify_margin;
    var verdict = 0u; // EXTERIOR
    if (mn <= band + margin && mx >= -(band + margin)) {
        verdict = 2u; // SURFACE
    } else if (mx <= -margin) {
        verdict = 1u; // INTERIOR
    }
    let bi = gid.x + gid.y * dims.x + gid.z * dims.x * dims.y;
    output_grid[bi] = bitcast<f32>(verdict);
}

// ============================================================
// GPU-classify FINALIZE (stage 3) — dilation + dense-packed handle allocation on-device, the
// GPU port of core_math::chunked_dc::sparse_index_from_verdicts (dilate_surface_bricks_1ring +
// emit_sparse_index). FULL classifies only: a fresh index has an empty freed list, so the whole
// verdicts→index chain runs on the GPU and ONE readback returns the final index.
//
// Bindings: SAME bake BGL. The finalize bind group binds the VERDICT buffer (written by
// classify_bricks) at binding 6 (brick_index, read-only u32 — exactly its element type) and a
// scratch/output WORK buffer at binding 2 (output_grid, read_write; u32 values bitcast through
// the f32 arena, the classify_bricks precedent). Work-buffer layout, in u32 slots
// (T = dims_bricks volume, NB = ceil(T / 256)):
//   [0,       T)        handled mask (dilated store flag, 0/1)
//   [T,       2T)       per-element EXCLUSIVE prefix within its 256-block
//   [2T,      2T+NB)    per-block sums, then (after block_scan) exclusive block offsets
//   [2T+NB,   3T+NB)    final index (handle | BRICK_TAG_* sentinel) — the readback region
//   [3T+NB]             allocated (total handled count) — read back with the index
//
// SCAN-ORDER PROOF: the CPU emit_sparse_index walks bi = 0..T in brick-linear order
// (bx + by·dx + bz·dx·dy) assigning slot++ to each stored brick — i.e. each stored brick's
// handle IS the count of stored bricks at smaller bi. handle(bi) = block_offset(bi/256) +
// in-block exclusive prefix — the same count, purely integer adds in a fixed tree order —
// so handles are BIT-IDENTICAL to the CPU's, deterministically, on any adapter.
//
// Determinism: every pass is integer-only, no atomics, no float accumulation; the dilate is a
// GATHER over the 26-neighbourhood (equivalent to the CPU's scatter: brick b is stored iff some
// in-range brick n with |n-b|_inf <= 1 is Surface — the CPU guard clamps n to the grid and b is
// in range by construction, so the two formulations mark exactly the same set).

const CLASSIFY_SCAN_WG: u32 = 256u;

fn fin_total() -> u32 { return params.dims_bricks_x * params.dims_bricks_y * params.dims_bricks_z; }
fn fin_nblocks() -> u32 { return (fin_total() + CLASSIFY_SCAN_WG - 1u) / CLASSIFY_SCAN_WG; }
fn fin_read(i: u32) -> u32 { return bitcast<u32>(output_grid[i]); }
fn fin_write(i: u32, v: u32) { output_grid[i] = bitcast<f32>(v); }

// 2D-GRID FLATTEN (2026-07-17): 256³ bricks = 16.7M elements = 65536 workgroups of 256 —
// ONE over max_compute_workgroups_per_dimension (65535), so the encode side dispatches a 2D
// grid (x ≤ 65535, y = rows). The flat workgroup index is wg.x + wg.y * nwg.x — for a 1D
// dispatch (y = 1, wg.y = 0, the entire ≤1536³ range) this is EXACTLY wg.x, so sub-65535
// dispatches are bit-identical to the old 1D form. Tail workgroups past the last block are
// guarded by the existing `bi >= total` early-outs (+ the wg_flat < nb guard in the scan).
fn fin_flat_wg(wg: vec3<u32>, nwg: vec3<u32>) -> u32 { return wg.x + wg.y * nwg.x; }

// Pass F1: 26-ring dilation (dilate_surface_bricks_1ring), gather form. mask[bi] = 1 iff this
// brick or any in-range 26-neighbour has a SURFACE verdict. One thread per brick, 2D-flattened.
@compute @workgroup_size(256)
fn classify_finalize_dilate(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let total = fin_total();
    let bi = fin_flat_wg(wg, nwg) * CLASSIFY_SCAN_WG + lid.x;
    if (bi >= total) { return; }
    let dx = params.dims_bricks_x;
    let dy = params.dims_bricks_y;
    let dims = vec3<i32>(i32(dx), i32(dy), i32(params.dims_bricks_z));
    let b = vec3<i32>(i32(bi % dx), i32((bi / dx) % dy), i32(bi / (dx * dy)));
    var handled = 0u;
    for (var oz = -1; oz <= 1; oz++) {
        for (var oy = -1; oy <= 1; oy++) {
            for (var ox = -1; ox <= 1; ox++) {
                let n = b + vec3(ox, oy, oz);
                if (any(n < vec3(0)) || any(n >= dims)) { continue; }
                let ni = u32(n.x) + u32(n.y) * dx + u32(n.z) * dx * dy;
                if (brick_index[ni] == 2u) { handled = 1u; } // SURFACE verdict
            }
        }
    }
    fin_write(bi, handled);
}

var<workgroup> fin_scan: array<u32, 256u>;

// Pass F2: per-256-block Hillis-Steele scan of the handled mask → in-block EXCLUSIVE prefix
// per element + the block total. Each workgroup touches only its own disjoint regions.
@compute @workgroup_size(256)
fn classify_finalize_scan(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let total = fin_total();
    let wg_flat = fin_flat_wg(wg, nwg);
    let bi = wg_flat * CLASSIFY_SCAN_WG + lid.x;
    var v = 0u;
    if (bi < total) { v = fin_read(bi); }
    fin_scan[lid.x] = v;
    workgroupBarrier();
    for (var off = 1u; off < CLASSIFY_SCAN_WG; off = off << 1u) {
        var add = 0u;
        if (lid.x >= off) { add = fin_scan[lid.x - off]; }
        workgroupBarrier();
        fin_scan[lid.x] = fin_scan[lid.x] + add;
        workgroupBarrier();
    }
    if (bi < total) { fin_write(total + bi, fin_scan[lid.x] - v); }
    // wg_flat < nb: a 2D grid may carry tail workgroups past the last block — their block-sum
    // write would land INSIDE the index region [2T+NB, 3T+NB). (For 1D dispatches nwg.x == nb
    // exactly, so the guard never fires — bit-identical.)
    if (lid.x == CLASSIFY_SCAN_WG - 1u && wg_flat < fin_nblocks()) {
        fin_write(2u * total + wg_flat, fin_scan[lid.x]);
    }
}

// Pass F3: EXCLUSIVE scan of the block sums (serial, one thread — NB ≤ 8192 at 1024³, trivially
// fast and trivially deterministic) + the grand total (= allocated) into the readback tail.
@compute @workgroup_size(1)
fn classify_finalize_block_scan(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x != 0u) { return; }
    let total = fin_total();
    let nb = fin_nblocks();
    var run = 0u;
    for (var blk = 0u; blk < nb; blk++) {
        let s = fin_read(2u * total + blk);
        fin_write(2u * total + blk, run);
        run = run + s;
    }
    fin_write(3u * total + nb, run); // allocated
}

// Pass F4: emit the index (emit_sparse_index verbatim): stored → dense-packed handle (block
// offset + in-block prefix); else INTERIOR verdict → 0xFFFFFFFE; else EXTERIOR → 0xFFFFFFFF.
// `store` wins over `interior` — an apron brick may be fully inside; it still needs real data.
@compute @workgroup_size(256)
fn classify_finalize_emit(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let total = fin_total();
    let bi = fin_flat_wg(wg, nwg) * CLASSIFY_SCAN_WG + lid.x;
    if (bi >= total) { return; }
    let nb = fin_nblocks();
    var tag = 0xFFFFFFFFu; // BRICK_TAG_EXTERIOR
    if (fin_read(bi) == 1u) {
        tag = fin_read(2u * total + bi / CLASSIFY_SCAN_WG) + fin_read(total + bi);
    } else if (brick_index[bi] == 1u) { // INTERIOR verdict
        tag = 0xFFFFFFFEu; // BRICK_TAG_INTERIOR
    }
    fin_write(2u * total + nb + bi, tag);
}

// ============================================================
// SURFACE CURVATURE (entry `curvature_eval`) — the GPU port of the CPU
// `sdf_curvature_masks` (app/src/node_graph_bake.rs:324-408), the material node graph's
// SdfCurvature / Curvature / Cavity signal.
//
// WHY IT LIVES HERE (docs/planning/GPU_CURVATURE_KERNEL_PLAN.md §1): this file already owns
// `eval_scene_at` — the scope-aware analytical stack machine — and its header states the design
// contract that a change to that function changes every consumer BY DESIGN. This is the third
// consumer. It authors NO second field eval: every distance below goes through `eval_scene_at`,
// exactly like `bake_volume` and `classify_bricks`.
//
// ── COORDINATE FRAME (the plan's #1 risk, RESOLVED) ─────────────────────────────────────────
// `eval_scene_at`'s parameter is spelled `world_pos`, but it is a MISNOMER inherited from the
// pre-BODY_LOCAL_PRIMS era: the prim stream at binding 0 is produced by
// `graphics::serialize_body_for_bake_hot(sg, body, ZERO)`, which keeps every prim in the TOP
// BODY'S LOCAL frame (serialize.rs:271-279 — "The bake frame is the TOP body's local frame"),
// so `eval_scene_at` consumes BODY-LOCAL points.
// The CPU `SdfFieldSources::sdf_at` (node_graph_bake.rs:175-190) evaluates
// `core_math::sdf_math::eval_scene` over `scene_graph.body_primitives(body)` — the SAME stored,
// un-transformed, BODY-LOCAL prim array (the struct's own doc at node_graph_bake.rs:140 says
// "body-local space = bake domain"). `SdfPrimitive::world_distance` is the same misnomer: it
// takes points in the prim's PARENT frame, which for a body's own prim array is body-local.
// => BOTH SIDES ARE BODY-LOCAL. No conversion at the seam; the AABB passed in below is likewise
//    the body-local AABB the CPU `body_aabb` computes. Feeding world points here would silently
//    produce plausible-but-wrong curvature — which is precisely what the golden parity test
//    (`app/src/sdf_gpu_mesh/tests_gpu_curvature.rs`) exists to catch.
//
// ── CPU-SIDE CONSTANTS (deliberate, plan §2.2) ──────────────────────────────────────────────
// `e`, `accept`, `diag` and the AABB are NOT re-derived here. They are computed by the CPU with
// the exact expressions `sdf_curvature_masks` uses today and passed in through the params tail,
// so the two consumers share PROVABLY identical constants rather than two float expressions that
// merely look alike.
//
// ── THE THREE EPSILONS (do not collapse — CPU :339/:258/:371) ───────────────────────────────
//   e          — the curvature finite-difference step, CPU-computed
//   0.002      — the Newton projection gradient step, a FIXED constant (project_to_surface :258)
//   e * 0.5    — the surface-normal gradient step (:371)
// ============================================================

// The curvature kernel reuses the BakeParams tail. Every field is spare padding for this entry
// (no grid, no bricks, no atlas) — offsets/size UNCHANGED, the `classify_bricks` precedent.
//   bounds_min / bounds_max  = the CPU `body_aabb` (already PADDED — node_graph_bake.rs:232)
//   grid_res.x               = sample count N
//   grid_res.y               = region (0 = |curv| | 1 = convex | 2 = concave)
//   grid_res.z               = seed mode (0 = single seed from curv_positions, 1 = 6-seed planar)
//   brick_dim                = sample stride in floats (3 for a packed xyz buffer)
//   classify_band            = e        (CPU: clamp to min_axis — NOT full diag)
//   classify_margin          = accept   (CPU: max(band, diag*0.08, 0.05))
//   bevel_radius             = feature_display_scale  (CPU: min_axis * ~0.866 for κ renorm)
//   mirror_center_x/y/z      = grow / contrast / strength
// Seeds recompute full AABB diag from bounds_min/max (push distance); renorm uses feature only.
fn curv_n() -> u32        { return params.grid_res.x; }
fn curv_region() -> u32   { return params.grid_res.y; }
fn curv_seed_mode() -> u32{ return params.grid_res.z; }
/// First OUTPUT entry index. 0 for the planar/seed grid paths; the vertex rung passes the draw's
/// `base_vertex` (rides `coarse_cull`, which only `classify_bricks` reads — the tail-repurposing
/// precedent documented at the top of this file).
fn curv_out_base() -> u32 { return params.coarse_cull; }
fn curv_e() -> f32        { return params.classify_band; }
fn curv_accept() -> f32   { return params.classify_margin; }
/// Short-axis κ display scale (CPU `feature_display_scale`) — NOT full body diagonal.
fn curv_feature() -> f32  { return params.bevel_radius; }
fn curv_grow() -> f32     { return params.mirror_center_x; }
fn curv_contrast() -> f32 { return params.mirror_center_y; }
fn curv_strength() -> f32 { return params.mirror_center_z; }

// CPU `SdfFieldSources::sdf_grad` (:236-248) verbatim: central differences at step `e`,
// `e` floored at 1e-5, scaled by 0.5/e. Same operation ORDER as the CPU for bit-closeness.
fn curv_grad(p: vec3<f32>, e_in: f32) -> vec3<f32> {
    let e = max(e_in, 1e-5);
    let dx = (eval_scene_at(vec3(p.x + e, p.y, p.z)) - eval_scene_at(vec3(p.x - e, p.y, p.z))) * (0.5 / e);
    let dy = (eval_scene_at(vec3(p.x, p.y + e, p.z)) - eval_scene_at(vec3(p.x, p.y - e, p.z))) * (0.5 / e);
    let dz = (eval_scene_at(vec3(p.x, p.y, p.z + e)) - eval_scene_at(vec3(p.x, p.y, p.z - e))) * (0.5 / e);
    return vec3(dx, dy, dz);
}

// CPU `project_to_surface` (:251-277) verbatim: 10 unit-gradient Newton steps onto the
// `sdf - grow = 0` isosurface, early exit at |d| < 1e-4, FIXED gradient step 0.002 (NOT `e`),
// final acceptance at max(accept, 1e-3). Returns w = 1.0 on accept, 0.0 on reject — the caller
// uses it as a mask so a rejected seed contributes to NEITHER sum (the CPU's `continue`).
struct CurvProj { p: vec3<f32>, ok: f32 }
fn curv_project(seed: vec3<f32>, grow: f32, accept: f32) -> CurvProj {
    let e = 0.002;
    var p = seed;
    for (var it = 0u; it < 10u; it++) {
        let d = eval_scene_at(p) - grow;
        if (abs(d) < 1e-4) { return CurvProj(p, 1.0); }
        let g = curv_grad(p, e);
        let gl = max(length(g), 1e-6);
        p -= (g / gl) * d;
    }
    let d = abs(eval_scene_at(p) - grow);
    if (d < max(accept, 1e-3)) { return CurvProj(p, 1.0); }
    return CurvProj(p, 0.0);
}

// Duff/Hughes-Möller orthonormal tangent basis perpendicular to normalized `n`. IDENTICAL
// formula to CPU `tangent_basis` (app/src/node_graph_bake.rs) and
// `calc_curvature_tangent_basis` (sdf_prim_eval.wgsl) — keep all three in lockstep.
fn curv_tangent_basis(n: vec3<f32>) -> mat2x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t1 = vec3(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let t2 = vec3(b, s + n.y * n.y * a, -n.y);
    return mat2x3<f32>(t1, t2);
}

// TANGENT-PLANE mean-curvature Laplacian (2026-07-27 fix — replaces the old WORLD-AXIS 7-tap
// stencil). CPU `SdfFieldSources::calc_curvature` (node_graph_bake.rs) is the production oracle;
// this mirrors it exactly: normal from `curv_grad` (clamped to a LOCAL step regardless of the
// (possibly large) Laplacian tap `e`), tangent basis via `curv_tangent_basis`, then a 4-tap
// Laplacian ALONG THE TANGENTS ONLY. The old stencil second-differenced fixed X/Y/Z regardless
// of local surface orientation — at large `e` (the owner's Cavity radius 0.190) the tap along
// whichever axis happened to be TANGENT to the surface escaped the local feature and sampled
// unrelated geometry, flipping the sign so carved cavities read convex. Sign PROVED empirically
// on known geometry (sphere/carved-box probes, see the CPU doc comment): raw tangent-plane
// Laplacian already reads positive-on-convex / negative-on-concave, matching this file's
// `convex = max(curv, 0.0)` / `concave = max(-curv, 0.0)` reduce below with NO extra flip.
fn curv_laplacian(p: vec3<f32>, e_in: f32) -> f32 {
    let e = max(e_in, 1e-4);
    let d = eval_scene_at(p);
    let ge = clamp(e, 1e-4, 0.01);
    let g = curv_grad(p, ge);
    let gl = length(g);
    let n = select(vec3(0.0, 0.0, 1.0), g / max(gl, 1e-6), gl > 1e-6);
    let tb = curv_tangent_basis(n);
    let t1 = tb[0];
    let t2 = tb[1];
    let lap = eval_scene_at(p + e * t1) + eval_scene_at(p - e * t1)
            + eval_scene_at(p + e * t2) + eval_scene_at(p - e * t2)
            - 4.0 * d;
    return lap / (e * e);
}

// Seed `s` of the CPU's list (:345-363), computed on demand.
//
// Planar bake domain -> 6 triplanar seeds: 3 on the AABB mid-planes (axis 2, 1, 0) and 3 pushed
// `diag*0.55` beyond bmax along that axis. `ext` is the CPU's `(bmax-bmin).max(splat(1e-4))`
// (:336). Non-planar -> ONE seed read from `curv_positions`, axis 0, weight 1.0 — the CPU's
// `else` branch, which is also the vertex rung's path (P2).
//
// Returned by value, and the `s` dispatch is a chain of scalar comparisons, so NO array is
// indexed by a dynamic value anywhere in this kernel — see the note at the call site for why
// that is load-bearing rather than stylistic.
struct CurvSeed { p: vec3<f32>, axis: u32 }
fn curv_seed_at(s: u32, i: u32, planar: bool, bmin: vec3<f32>, bmax: vec3<f32>, diag: f32) -> CurvSeed {
    if (!planar) {
        let b = i * params.brick_dim;
        return CurvSeed(
            vec3(curv_positions[b], curv_positions[b + 1u], curv_positions[b + 2u]), 0u);
    }
    // The pixel's (u,v) — the CPU pixel loop's `p = [u*2-1, v*2-1, 0]` (:605-607) mapped back
    // through `((p+1)*0.5).clamp(0,1)` (:346-347), i.e. exactly the pixel-centre UV. Reproduced
    // on-device so this consumer uploads no position buffer at all (plan §3.2).
    let res = u32(round(sqrt(f32(curv_n()))));
    let u = clamp((f32(i % res) + 0.5) / f32(res), 0.0, 1.0);
    let v = clamp((f32(i / res) + 0.5) / f32(res), 0.0, 1.0);
    let ext = max(bmax - bmin, vec3(1e-4));
    let c = (bmin + bmax) * 0.5;
    let out_d = diag * 0.55;
    // p_xy (axis 2), p_xz (axis 1), p_yz (axis 0), then the same three pushed beyond bmax.
    if (s == 0u) { return CurvSeed(vec3(bmin.x + u * ext.x, bmin.y + v * ext.y, c.z), 2u); }
    if (s == 1u) { return CurvSeed(vec3(bmin.x + u * ext.x, c.y, bmin.z + v * ext.z), 1u); }
    if (s == 2u) { return CurvSeed(vec3(c.x, bmin.y + u * ext.y, bmin.z + v * ext.z), 0u); }
    if (s == 3u) { return CurvSeed(vec3(bmin.x + u * ext.x, bmin.y + v * ext.y, bmax.z + out_d), 2u); }
    if (s == 4u) { return CurvSeed(vec3(bmin.x + u * ext.x, bmax.y + out_d, bmin.z + v * ext.z), 1u); }
    return CurvSeed(vec3(bmax.x + out_d, bmin.y + u * ext.y, bmin.z + v * ext.z), 0u);
}

// CPU shaping (:397-401): smoothstep(clamp(x*contrast*4, 0, 1)) * strength, clamped.
fn curv_shape(x: f32, c: f32, strength: f32) -> f32 {
    let t = clamp(x * c * 4.0, 0.0, 1.0);
    let s = t * t * (3.0 - 2.0 * t);
    return clamp(s * strength, 0.0, 1.0);
}

// One thread per sample. Output at binding 2 (`output_grid`, the established non-grid output
// arena — the `classify_bricks` precedent): 4 f32 per sample,
// [i*4+0] = mask, [i*4+1] = signed curv (−1..1), [i*4+2] = convex mask, [i*4+3] = concave mask.
// workgroup_size 64 (NOT 256) — `eval_scene_at` carries eleven array<_,8> stacks, so register
// pressure caps occupancy (plan §2.3).
//
// 2D DISPATCH FLATTEN (2026-07-27): dense/sparse mesh vert_cap at 512³+ can need
// >65535 workgroups of 64 — past `max_compute_workgroups_per_dimension`. A pure 1D
// `dispatch(n/64, 1, 1)` was silently invalid / empty at 512 → vertex curvature vanished.
// CPU dispatches (wx, wy, 1) with wx≤65535; flat index = gid.x + gid.y * (nwg.x * 64).
@compute @workgroup_size(64)
fn curvature_eval(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = gid.x + gid.y * (nwg.x * 64u);
    if (i >= curv_n()) { return; }

    let bmin = params.bounds_min;
    let bmax = params.bounds_max;
    let e = curv_e();
    let accept = curv_accept();
    // Full AABB diagonal for planar seed push only (recomputed — not the κ renorm scale).
    let ext_seed = max(bmax - bmin, vec3(1e-4));
    let diag = max(length(ext_seed), 1e-4);
    // Short-axis feature display scale from CPU (elongate / multi-prim safe).
    let feature = curv_feature();
    let grow = curv_grow();
    let contrast = max(curv_contrast(), 0.001);
    let strength = curv_strength();
    let planar = curv_seed_mode() == 1u;

    // ── SEEDS (CPU :345-363) ────────────────────────────────────────────────────────────────
    // Planar bake domain: 6 triplanar seeds — 3 on the AABB mid-planes (axis 2, 1, 0) and 3
    // pushed diag*0.55 beyond bmax along that axis. `ext` is the CPU's
    // `(bmax-bmin).max(splat(1e-4))` (:336). Non-planar: ONE seed at the given position, axis 0,
    // weight 1.0 — the CPU's `else` branch, which is also the vertex rung's path (P2).
    // Seeds are computed INSIDE the loop from `s` (`curv_seed_at`) rather than prefilled into a
    // `array<vec3<f32>, 6>`. Both forms express the identical CPU seed list, but a function-local
    // array read with a LOOP-VARIABLE index cannot live in registers — it is demoted to scratch
    // memory, and in a kernel where Naga has already inlined ~500 copies of `eval_scene_at` (each
    // carrying eleven `array<_,8>` stacks of its own) that spill produced measurably wrong results
    // on real hardware: the 6-seed path diverged from the CPU by up to 1.65 in `curv` — including
    // sign flips — while the mathematically identical SINGLE-seed path agreed to 2e-4. Indexing
    // by a dynamic value is the only difference between those two paths, so it stays out.
    // (Verified by `tests_gpu_curvature`; the single-seed agreement is what proves the projection,
    // gradient, laplacian, shaping and coordinate frame were all correct throughout.)
    var nseed = 1u;
    if (planar) { nseed = 6u; }

    // ── PER SEED (CPU :367-384) ─────────────────────────────────────────────────────────────
    var sum_k = 0.0;
    var sum_w = 0.0;
    for (var s = 0u; s < nseed; s++) {
        let sd = curv_seed_at(s, i, planar, bmin, bmax, diag);
        let pr = curv_project(sd.p, grow, accept);
        // MASKED accumulate, not an average over 6 slots: a rejected projection contributes to
        // neither sum — the exact semantics of the CPU's `continue` (:368-370).
        if (pr.ok == 0.0) { continue; }
        let g = curv_grad(pr.p, e * 0.5);
        let gl = max(length(g), 1e-6);
        let n = g / gl;
        var w = 1.0;
        if (planar) {
            var a = abs(n.x);
            if (sd.axis == 1u) { a = abs(n.y); }
            else if (sd.axis == 2u) { a = abs(n.z); }
            w = max(a * a, 0.05);
        }
        let k = curv_laplacian(pr.p, e);
        sum_k += k * w;
        sum_w += w;
    }

    // ── REDUCE + SHAPE (CPU :386-407) ───────────────────────────────────────────────────────
    // `curv_out_base()` is 0 for every grid dispatch (planar + seed-position tests), so those
    // paths are bit-for-bit unchanged. The VERTEX RUNG passes the draw's `base_vertex`, because
    // the G-buffer VS reads this buffer with `@builtin(vertex_index)` — which INCLUDES
    // `base_vertex` — and a pooled body's vertices start partway into its group buffer. Writing
    // at the same base the draw reads from is what keeps that indexing in bounds; see
    // `gpu_curvature.rs`'s "BASE-VERTEX PARITY" note.
    let o = (curv_out_base() + i) * 4u;
    if (sum_w < 1e-6) {
        output_grid[o] = 0.0;
        output_grid[o + 1u] = 0.0;
        output_grid[o + 2u] = 0.0;
        output_grid[o + 3u] = 0.0;
        return;
    }
    let k_raw = sum_k / sum_w;
    // RENORMALIZED 2026-07-27 (tangent-plane) + 2026-07-28 (feature = min-axis scale, not full
    // body diag). Elongate / Shift+D multi-prim AABBs must not inflate |curv| or force huge FD
    // steps. `feature` is CPU `feature_display_scale` (min_axis * ~0.866). KEEP IDENTICAL to the
    // CPU oracle — golden-parity gate relationship.
    let curv_x = k_raw * feature;
    let curv = curv_x / (1.0 + abs(curv_x));
    let convex = max(curv, 0.0);
    let concave = max(-curv, 0.0);
    let cvx_m = curv_shape(convex, contrast, strength);
    let ccv_m = curv_shape(concave, contrast, strength);
    var mask = curv_shape(abs(curv), contrast, strength);
    if (curv_region() == 1u) { mask = cvx_m; }
    else if (curv_region() == 2u) { mask = ccv_m; }
    output_grid[o] = mask;
    output_grid[o + 1u] = curv;
    output_grid[o + 2u] = cvx_m;
    output_grid[o + 3u] = ccv_m;
}
