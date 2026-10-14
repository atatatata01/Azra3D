// Phase D4 — per-frame cluster cut (PHASE_D4_CLUSTER_RASTER_PLAN.md).
//
// One dispatch per (clustered cid, pass). Thread per (instance, cluster):
// frustum sphere test + screen-space error cut + backface cone, then append a
// DrawIndexedIndirect into this dispatch's args region (fixed cap, index_count=0
// slots self-skip in the multi-draw — the SdfMeshPool pattern).
//
// The error decision MUST stay in lockstep with render/src/cluster_gpu.rs
// (`error_px` / `cut_selects`) — the CPU replay is the correctness reference.
//
// Bind group layout: graphics::bgl::cluster_cut_bgl_entries() — single source.

struct CutParams {
    planes: array<vec4<f32>, 6>,
    // xyz = camera position, w = tan(fov_y / 2).
    cam: vec4<f32>,
    // x = viewport_h, y = threshold_px, z = bitcast n_clusters, w = bitcast n_instances.
    metrics: vec4<f32>,
    // Structural instance-buffer slots of up to 4 instances sharing this cid.
    instance_slots: vec4<u32>,
    // x = args_base (slots), y = counter index, z = passthrough, w = cap.
    region: vec4<u32>,
    // x = skip_cone (1 for shadow passes — backface-to-CAMERA is wrong for a
    // light; a cone-culled caster must still cast).
    // y = force_l0: select ONLY level-0 clusters and skip every other test, so the
    //     drawn set is fixed and camera-independent. Diagnostic: if an artifact
    //     survives this, the SELECTION is exonerated and the fault is in the draw
    //     path (args/barriers/bindings). z/w reserved.
    flags: vec4<u32>,
}

// Mirrors render::mesh_gpu::InstanceData (144 B) — matches instance_cull.wgsl.
struct InstanceData {
    model_matrix: mat4x4<f32>,
    normal_matrix: array<vec4<f32>, 3>,
    ids: vec4<u32>,
    curv_params: vec4<f32>,
}

// Mirrors render::cluster_gpu::ClusterGpu (80 B).
struct ClusterGpu {
    bounds: vec4<f32>,
    cone: vec4<f32>,
    self_group: vec4<f32>,
    parent_group: vec4<f32>,
    lod: vec4<f32>,
}

struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: u32,
    first_instance: u32,
}

// Counters: [0]=tested [1]=frustum [2]=cone [3]=not_in_cut [4]=selected [5]=overflow,
// then one region counter per dispatch at [8 + region.y].
@group(0) @binding(0) var<uniform> params: CutParams;
@group(0) @binding(1) var<storage, read> instances: array<InstanceData>;
@group(0) @binding(2) var<storage, read> clusters: array<ClusterGpu>;
@group(0) @binding(3) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(4) var<storage, read_write> counters: array<atomic<u32>>;

@compute @workgroup_size(64)
fn cs_cluster_reset(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.region.w) { return; }
    let o = params.region.x + gid.x;
    draw_args[o] = DrawArgs(0u, 0u, 0u, 0u, 0u);
}

// Screen-space error of `err` over world sphere — LOCKSTEP with cluster_gpu.rs.
//
// ORDER MATTERS: zero error projects to ZERO pixels at any distance, INCLUDING
// when the camera is inside the sphere. Level 0 is the source geometry (error 0)
// and must ALWAYS pass its own "fine enough" test — otherwise a cluster whose
// sphere contains the camera fails BOTH tests (self=INF, parent=INF) and is
// dropped, punching a hole that moves with the camera. That was the mountain
// flicker: the biggest spheres are the ones most likely to contain the camera.
fn error_px_ws(center: vec3<f32>, radius: f32, err: f32) -> f32 {
    if (err <= 0.0) { return 0.0; }      // exact geometry — never needs refining
    if (err >= 1e29) { return 1e30; }    // root sentinel (parent_error = INF)
    // NO infinity for "camera inside the sphere": clamping the distance keeps the
    // value FINITE and strictly proportional to `err`, which is what makes the cut
    // total. With an infinity here, a cluster deep inside a big object had
    // self = parent = INF, so BOTH tests failed and NO level drew that region — a
    // moving hole in the terrain (the 2026-08-04 "flicker everywhere").
    let d = max(distance(center, params.cam.xyz) - radius, 0.05);
    return err * params.metrics.x / (2.0 * d * params.cam.w);
}

// Monotone open band (nv_cluster_lod / clusterlod): if object-space parent_err
// ≥ self_err but perspective projects parent *smaller* than self, no τ satisfies
// self ≤ τ < parent → undrawn hole. Only fix inversions — do NOT inflate by a
// large absolute eps (self+1e-3 broke L0 bands when errors project ≪1px).
fn projected_cut_band(self_c: vec3<f32>, self_r: f32, self_err: f32,
                      par_c: vec3<f32>, par_r: f32, par_err: f32) -> vec2<f32> {
    let self_px = error_px_ws(self_c, self_r, self_err);
    var parent_px = error_px_ws(par_c, par_r, par_err);
    if (par_err < 1e29 && par_err + 1e-20 >= self_err) {
        let min_open = max(abs(self_px) * 1e-5, 1e-12);
        if (parent_px < self_px + min_open) {
            parent_px = self_px + min_open;
        }
    }
    return vec2<f32>(self_px, parent_px);
}

fn sphere_outside_plane(center: vec3<f32>, radius: f32, plane: vec4<f32>) -> bool {
    return dot(plane.xyz, center) + plane.w < -radius;
}

@compute @workgroup_size(64)
fn cs_cluster_cut(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n_clusters = bitcast<u32>(params.metrics.z);
    let n_instances = bitcast<u32>(params.metrics.w);
    if (gid.x >= n_clusters * n_instances) { return; }
    let inst_i = gid.x / n_clusters;
    let c = clusters[gid.x % n_clusters];
    let inst_slot = params.instance_slots[inst_i];
    let m = instances[inst_slot].model_matrix;
    atomicAdd(&counters[0], 1u);

    // World-space scale: per-axis basis lengths; max drives radii, spread
    // decides whether the backface cone stays valid.
    let sx = length(m[0].xyz);
    let sy = length(m[1].xyz);
    let sz = length(m[2].xyz);
    let smax = max(sx, max(sy, sz));
    let smin = min(sx, min(sy, sz));

    if (params.flags.y == 1u) {
        // force_l0 diagnostic: fixed set, no camera dependence at all.
        if (c.lod.x != 0.0) { return; }
    } else if (params.region.z == 0u) { // passthrough sabotage skips ALL rejection
        // Error cut on the flattened GROUP spheres (identical for all siblings —
        // the crack-free invariant). Monotone band — see projected_cut_band.
        let sg = (m * vec4<f32>(c.self_group.xyz, 1.0)).xyz;
        let pg = (m * vec4<f32>(c.parent_group.xyz, 1.0)).xyz;
        let band = projected_cut_band(
            sg, c.self_group.w * smax, c.lod.x * smax,
            pg, c.parent_group.w * smax, c.lod.y * smax,
        );
        let self_px = band.x;
        let parent_px = band.y;
        let threshold = params.metrics.y;
        if (!(self_px <= threshold && threshold < parent_px)) {
            atomicAdd(&counters[3], 1u);
            return;
        }
        // Frustum on the cluster's own sphere. flags.z = skip visibility tests
        // (diagnostic: isolates "holes from culling" vs "holes from the cut").
        let center = (m * vec4<f32>(c.bounds.xyz, 1.0)).xyz;
        let radius = c.bounds.w * smax;
        if (params.flags.z == 0u) {
            for (var p = 0u; p < 6u; p = p + 1u) {
                if (sphere_outside_plane(center, radius, params.planes[p])) {
                    atomicAdd(&counters[1], 1u);
                    return;
                }
            }
        }
        // Backface cone (G12) — only under near-uniform scale (a sheared cone
        // is invalid; skipping is conservative-correct, not a hack). Shadow
        // passes skip it entirely (flags.x): cone is camera-view-dependent.
        if (params.flags.x == 0u && params.flags.z == 0u && c.cone.w < 1.0 && smin > 0.9 * smax) {
            let axis = normalize((m * vec4<f32>(c.cone.xyz, 0.0)).xyz);
            let to_cluster = normalize(center - params.cam.xyz);
            if (dot(to_cluster, axis) >= c.cone.w + radius / max(distance(center, params.cam.xyz), 1e-6)) {
                atomicAdd(&counters[2], 1u);
                return;
            }
        }
    }

    let slot = atomicAdd(&counters[8u + params.region.y], 1u);
    if (slot >= params.region.w) {
        atomicAdd(&counters[5], 1u); // overflow — dropped, visible on scoreboard
        return;
    }
    atomicAdd(&counters[4], 1u);
    let o = params.region.x + slot;
    draw_args[o] = DrawArgs(
        bitcast<u32>(c.lod.w),  // index_count
        1u,                     // instance_count
        bitcast<u32>(c.lod.z),  // first_index
        0u,                     // base_vertex
        inst_slot,              // first_instance → InstanceData fetch in the raster VS
    );
}
