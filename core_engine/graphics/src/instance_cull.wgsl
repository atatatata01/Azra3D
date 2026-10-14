// Phase D1 — GPU-driven instance frustum culling (PHASE_D_VIRTUAL_GEOMETRY_PLAN.md §D1).
//
// Two entry points, dispatched back-to-back each culled pass:
//   cs_reset_args    — one thread per batch: seeds the DrawIndexedIndirect args from BatchMeta
//                      with instance_count = 0 (GPU-side zeroing; zero CPU writes per frame).
//   cs_frustum_cull  — one thread per instance: positive-vertex AABB vs 6 frustum planes
//                      (same Gribb–Hartmann convention as raster_common::aabb_outside_frustum,
//                      including the [0,1]-depth near plane), then compacts survivors into
//                      visible_instances at batch.out_base + atomicAdd(instance_count).
//
// The same shader culls the camera view and each shadow cascade — the caller supplies the
// pass's frustum planes in CullParams and points bindings 4/5 at that pass's args/output
// region. `passthrough != 0` marks every instance visible (the sabotage switch for the D1
// gate — draw volume explodes, proving the cull is load-bearing).
//
// Bind group layout: graphics::bgl::instance_cull_bgl_entries() — single source, entry-for-entry.

// Pass modes: cascades never sample the HZB (a camera-depth pyramid is wrong for
// light-space visibility); the camera pass samples it when the pyramid is valid.
// MODE_EARLY writes occluded_flags for the C4 late pass instead of dropping silently.
const MODE_FRUSTUM_ONLY: u32 = 0u;
const MODE_EARLY: u32 = 1u;

struct CullParams {
    planes: array<vec4<f32>, 6>,
    // For hzb_occluded(): projects world AABB corners to NDC (same view_proj the
    // planes were derived from).
    view_proj: mat4x4<f32>,
    n_instances: u32,
    n_batches: u32,
    passthrough: u32,
    // Region bases for this pass (camera = 0; shadow cascade c = region 1+c):
    // args_base in DrawArgsAtomic entries, visible_base in InstanceData slots.
    args_base: u32,
    visible_base: u32,
    mode: u32,
    // Valid viewport extent inside the pow2 HZB (texels at mip 0) + mip count.
    viewport_w: u32,
    viewport_h: u32,
    hzb_size: u32,
    mip_count: u32,
    _pad0: u32,
    _pad1: u32,
}

// Mirrors render::mesh_gpu::InstanceData (144 bytes). Copied wholesale on survival;
// the cull never inspects fields, so layout drift is caught by the Rust-side size assert.
struct InstanceData {
    model_matrix: mat4x4<f32>,
    normal_matrix: array<vec4<f32>, 3>,
    ids: vec4<u32>,          // material_idx, body_index, flags, _pad
    curv_params: vec4<f32>,
}

// World-space AABB + owning batch for one instance. Built once per structural rebuild.
struct CullMeta {
    aabb_min: vec3<f32>,
    batch: u32,
    aabb_max: vec3<f32>,
    _pad: u32,
}

// Per-batch draw geometry + where its compacted instances start in visible_instances.
struct BatchMeta {
    index_count: u32,
    first_index: u32,
    base_vertex: u32,
    out_base: u32,
}

// wgpu::util::DrawIndexedIndirectArgs with an atomic instance_count.
struct DrawArgsAtomic {
    index_count: u32,
    instance_count: atomic<u32>,
    first_index: u32,
    base_vertex: u32,
    first_instance: u32,
}

@group(0) @binding(0) var<uniform> params: CullParams;
@group(0) @binding(1) var<storage, read> instances: array<InstanceData>;
@group(0) @binding(2) var<storage, read> cull_meta: array<CullMeta>;
@group(0) @binding(3) var<storage, read> batch_meta: array<BatchMeta>;
@group(0) @binding(4) var<storage, read_write> draw_args: array<DrawArgsAtomic>;
@group(0) @binding(5) var<storage, read_write> visible_instances: array<InstanceData>;
// D2: HZB max-depth pyramid (full mip chain; dummy 1x1 when the system is absent).
@group(0) @binding(6) var hzb: texture_2d<f32>;
// D2: per-instance "frustum-passed but HZB-failed" marks for the C4 late pass.
@group(0) @binding(7) var<storage, read_write> occluded_flags: array<u32>;
// D2/C5: cull counters [tested, frustum_culled, hzb_culled, late_recovered, ...].
@group(0) @binding(8) var<storage, read_write> counters: array<atomic<u32>, 8>;

// Counter slots (camera passes only — cascade passes never touch them):
// [0] tested  [1] frustum_culled  [2] hzb_culled_early  [3] late_recovered
const CTR_TESTED: u32 = 0u;
const CTR_FRUSTUM: u32 = 1u;
const CTR_HZB_EARLY: u32 = 2u;
const CTR_LATE_RECOVERED: u32 = 3u;

@compute @workgroup_size(64)
fn cs_reset_args(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = gid.x;
    // Camera-early reset (args_base==0) also zeroes the frame's counters — it is the
    // first cull dispatch of the frame, so every later atomicAdd lands on zeroes.
    if (params.args_base == 0u && b < 8u) {
        atomicStore(&counters[b], 0u);
    }
    if (b >= params.n_batches) { return; }
    let m = batch_meta[b];
    let o = params.args_base + b;
    draw_args[o].index_count = m.index_count;
    atomicStore(&draw_args[o].instance_count, 0u);
    draw_args[o].first_index = m.first_index;
    draw_args[o].base_vertex = m.base_vertex;
    draw_args[o].first_instance = 0u;
}

// Positive-vertex test: for each plane pick the AABB corner farthest along the plane
// normal; if even that corner is behind the plane, the whole box is outside.
fn aabb_outside_plane(mn: vec3<f32>, mx: vec3<f32>, plane: vec4<f32>) -> bool {
    let p = vec3<f32>(
        select(mn.x, mx.x, plane.x >= 0.0),
        select(mn.y, mx.y, plane.y >= 0.0),
        select(mn.z, mx.z, plane.z >= 0.0),
    );
    return dot(plane.xyz, p) + plane.w < 0.0;
}

// Conservative HZB occlusion test: project the world AABB's 8 corners; any corner
// crossing the near plane (w ≤ eps) → visible. Otherwise the screen rect picks the
// mip where it spans ≤ 2×2 texels; occluded iff the box's NEAREST depth is farther
// than the MAX occluder depth over 4 samples (standard-Z, HZB is a max pyramid).
fn hzb_occluded(mn: vec3<f32>, mx: vec3<f32>) -> bool {
    var ndc_min = vec3<f32>(1e30, 1e30, 1e30);
    var ndc_max = vec3<f32>(-1e30, -1e30, -1e30);
    for (var c = 0u; c < 8u; c = c + 1u) {
        let corner = vec3<f32>(
            select(mn.x, mx.x, (c & 1u) != 0u),
            select(mn.y, mx.y, (c & 2u) != 0u),
            select(mn.z, mx.z, (c & 4u) != 0u),
        );
        let h = params.view_proj * vec4<f32>(corner, 1.0);
        if (h.w <= 1e-4) { return false; } // crosses/behind near plane — keep
        let ndc = h.xyz / h.w;
        ndc_min = min(ndc_min, ndc);
        ndc_max = max(ndc_max, ndc);
    }
    // NDC [-1,1] → mip-0 texel rect inside the pow2 pyramid (viewport occupies the
    // top-left viewport_w × viewport_h texels; y flips).
    let vp = vec2<f32>(f32(params.viewport_w), f32(params.viewport_h));
    let uv_min = clamp(vec2<f32>(ndc_min.x, -ndc_max.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    let uv_max = clamp(vec2<f32>(ndc_max.x, -ndc_min.y) * 0.5 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
    let t_min = uv_min * vp;
    let t_max = uv_max * vp;
    let span = max(t_max.x - t_min.x, t_max.y - t_min.y);
    let mip = min(u32(max(ceil(log2(max(span, 1.0))), 0.0)), params.mip_count - 1u);
    let inv = 1.0 / f32(1u << mip);
    let c_min = vec2<i32>(t_min * inv);
    let c_max = vec2<i32>(t_max * inv);
    let d0 = textureLoad(hzb, c_min, i32(mip)).x;
    let d1 = textureLoad(hzb, vec2<i32>(c_max.x, c_min.y), i32(mip)).x;
    let d2 = textureLoad(hzb, vec2<i32>(c_min.x, c_max.y), i32(mip)).x;
    let d3 = textureLoad(hzb, c_max, i32(mip)).x;
    let occ_max = max(max(d0, d1), max(d2, d3));
    return max(ndc_min.z, 0.0) > occ_max + 1e-5;
}

// C4 late pass: retest ONLY the instances the early pass rejected by HZB (flag==1)
// against the FRESH pyramid (rebuilt from this frame's early-survivor depth between
// the two dispatches). Survivors compact into this pass's own region (camera-late,
// params slot 4) and are drawn by the second G-buffer render pass — never appended
// to the early args (those draws are already encoded).
@compute @workgroup_size(64)
fn cs_hzb_late(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_instances) { return; }
    if (occluded_flags[i] == 0u) { return; } // early pass drew it or frustum-culled it
    let cm = cull_meta[i];
    if (hzb_occluded(cm.aabb_min, cm.aabb_max)) { return; } // truly hidden this frame
    atomicAdd(&counters[CTR_LATE_RECOVERED], 1u);
    let m = batch_meta[cm.batch];
    let slot = params.visible_base + m.out_base
        + atomicAdd(&draw_args[params.args_base + cm.batch].instance_count, 1u);
    visible_instances[slot] = instances[i];
}

@compute @workgroup_size(64)
fn cs_frustum_cull(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_instances) { return; }
    let cm = cull_meta[i];
    // Camera-early pass (args_base==0) owns the frame counters.
    let count_here = params.args_base == 0u;
    if (count_here) { atomicAdd(&counters[CTR_TESTED], 1u); }
    if (params.mode == MODE_EARLY) { occluded_flags[i] = 0u; }
    if (params.passthrough == 0u) {
        for (var p = 0u; p < 6u; p = p + 1u) {
            if (aabb_outside_plane(cm.aabb_min, cm.aabb_max, params.planes[p])) {
                if (count_here) { atomicAdd(&counters[CTR_FRUSTUM], 1u); }
                return;
            }
        }
        if (params.mode == MODE_EARLY && hzb_occluded(cm.aabb_min, cm.aabb_max)) {
            occluded_flags[i] = 1u; // C4 late pass retests against the fresh HZB
            if (count_here) { atomicAdd(&counters[CTR_HZB_EARLY], 1u); }
            return;
        }
    }
    let m = batch_meta[cm.batch];
    let slot = params.visible_base + m.out_base
        + atomicAdd(&draw_args[params.args_base + cm.batch].instance_count, 1u);
    visible_instances[slot] = instances[i];
}
