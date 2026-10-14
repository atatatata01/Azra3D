// ============================================================
// AZRA — DC-mesh curvature (MESH_CURVATURE_PLAN M1)
// ============================================================
// Per-vertex curvature derived from the MESH's own geometry — face normals computed from
// neighbouring vertex POSITIONS via the index buffer — rather than from the analytical SDF.
//
// ── WHY THIS EXISTS ───────────────────────────────────────────────────────────────────────
// The existing vertex rung (`volume_bake.wgsl::curvature_eval`) takes DC vertex positions but
// evaluates `eval_scene_at` at them: a Laplacian of the FIELD, not curvature of the SURFACE.
// Near a boolean seam that is dominated by how the SDF blends, which reads as wrong geometry
// exactly at edges. This kernel answers the other question — "how much do adjacent faces
// disagree?" — which is what a curvature map is supposed to show.
//
// ── THE TRAP THIS AVOIDS ──────────────────────────────────────────────────────────────────
// A DC vertex ALREADY carries a normal at floats [3..5], and it is tempting to use it. Do not:
// `sdf_mesh.wgsl` fills it from `gradient_normal()` — the signed-distance GRADIENT — so it is
// field-derived too and would reproduce the very result this kernel exists to replace. Only
// `position` (floats [0..2]) is genuinely geometric.
//
// ── GATHER, NOT SCATTER (forced, and better) ──────────────────────────────────────────────
// WGSL has no f32 atomics (every atomic in this codebase is `atomic<u32>`), so per-triangle
// results cannot be atomically summed into a per-vertex float buffer. Each thread therefore owns
// ONE VERTEX and walks that vertex's own incident triangles, summing locally. Besides being the
// only option, it is deterministic: float addition is order-dependent, so a scatter would make
// the output vary run to run.
//
// M1 SCOPE: unweighted mean of incident face normals vs the vertex's own averaged normal. That is
// enough to separate flat regions (all faces agree → 0) from edges (faces disagree → high).
// Angle/area weighting and signed convex/concave are M2; radius-as-smoothing is M3.

struct MeshCurvParams {
    vert_count: u32,
    tri_count: u32,
    // Adjacency window: each vertex scans `adj_stride` triangle slots starting at
    // `vert_index * adj_stride`. A vertex with more incident triangles than the stride is
    // truncated — see the note at the scan loop.
    adj_stride: u32,
    /// M3 — how many neighbour-diffusion steps to run over the vertex normals BEFORE measuring.
    /// 0 = measure immediately (M1/M2 behaviour: purely local, only sharp edges register).
    /// Higher = wider measurement baseline, so gentle curvature becomes readable. Driven by the
    /// panel's Radius.
    smooth_iters: u32,
}

@group(0) @binding(0) var<uniform> mc: MeshCurvParams;
// The mesh's own vertex buffer, 8 f32 per vertex (pos.xyz, nrm.xyz, uv.xy) — bound directly,
// never copied.
@group(0) @binding(1) var<storage, read> mc_verts: array<f32>;
// The mesh's own index buffer: 3 u32 per triangle.
@group(0) @binding(2) var<storage, read> mc_idx: array<u32>;
// Vertex → incident-triangle table, `vert_count * adj_stride` entries; `0xFFFFFFFF` = empty slot.
@group(0) @binding(3) var<storage, read> mc_adj: array<u32>;
// Output, 4 f32 per vertex — SAME layout as `curvature_eval` so the existing G-buffer carry
// (`curv_vertex[vertex_index*4 + ch]`) reads it unchanged: [0]=mask [1]=signed [2]=convex [3]=concave.
@group(0) @binding(4) var<storage, read_write> mc_out: array<f32>;
// M3 ping-pong buffers for normal diffusion, 4 f32 per vertex (xyz = accumulated normal,
// w = accumulated area). Two buffers because a diffusion step must read the PREVIOUS state for
// every vertex — reading and writing one buffer would make the result depend on dispatch order,
// which is both wrong and non-deterministic.
@group(0) @binding(5) var<storage, read_write> mc_nrm_a: array<f32>;
@group(0) @binding(6) var<storage, read_write> mc_nrm_b: array<f32>;

const MC_NO_TRI: u32 = 0xffffffffu;

fn mc_pos(vi: u32) -> vec3<f32> {
    let o = vi * 8u;
    return vec3<f32>(mc_verts[o], mc_verts[o + 1u], mc_verts[o + 2u]);
}

/// AREA-WEIGHTED geometric normal of triangle `ti`, from its three POSITIONS.
///
/// Returns `cross(b-a, c-a)` WITHOUT normalizing: that vector's direction is the face normal and
/// its LENGTH is exactly twice the triangle's area. So summing these already area-weights the
/// result — the weighting is free, and it matters (M2).
///
/// WHY WEIGHTING IS REQUIRED, not a refinement: DC meshes have wildly uneven triangle areas at
/// boolean seams. At a vertex where one large face meets two slivers, an UNWEIGHTED mean gives
/// the large face 33% of the vote; area-weighted it gets 96%. Unweighted curvature is therefore
/// dominated by how many slivers happen to touch a vertex rather than by the actual surface
/// shape — which reads as noise exactly where the owner reported edges looking wrong.
///
/// A degenerate (zero-area) triangle returns the zero vector and contributes nothing, which is
/// the correct weight for a face with no area — and avoids the NaN that normalizing it would
/// produce.
fn mc_tri_normal_weighted(ti: u32) -> vec3<f32> {
    let b = ti * 3u;
    if (b + 2u >= arrayLength(&mc_idx)) {
        return vec3<f32>(0.0);
    }
    let a = mc_pos(mc_idx[b]);
    let bb = mc_pos(mc_idx[b + 1u]);
    let c = mc_pos(mc_idx[b + 2u]);
    return cross(bb - a, c - a);
}

// ── M4: GPU-BUILT ADJACENCY ───────────────────────────────────────────────────────────────
// The vertex→triangle table cannot be built on the CPU: the index COUNT lives only in the
// GPU-written `DrawIndexedIndirect` args (there is deliberately no readback — see
// `resident.rs`'s "NO mesh readback" contract), and reading it back every mesh generation would
// reintroduce exactly the stall this whole feature is trying to avoid.
//
// So it is built here, in two passes over the index buffer, using the u32 atomics WGSL does have:
//   `mesh_curvature_adj_clear` — reset the per-vertex fill counters and mark every slot empty
//   `mesh_curvature_adj_fill`  — one thread per TRIANGLE claims a slot in each of its 3 vertices
//
// A vertex with more incident triangles than `adj_stride` is truncated rather than overflowing
// into its neighbour's slots. That is a bounded, local loss of accuracy on a pathological vertex,
// which is far better than corrupting an adjacent vertex's list (and `MC_ADJ_STRIDE` is chosen
// well above the ~6 a DC quad mesh actually produces).

@group(0) @binding(7) var<storage, read_write> mc_adj_rw: array<u32>;
@group(0) @binding(8) var<storage, read_write> mc_adj_fill: array<atomic<u32>>;

@compute @workgroup_size(64, 1, 1)
fn mesh_curvature_adj_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    let vi = gid.x;
    if (vi >= mc.vert_count) { return; }
    if (vi < arrayLength(&mc_adj_fill)) {
        atomicStore(&mc_adj_fill[vi], 0u);
    }
    let base = vi * mc.adj_stride;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = base + k;
        if (slot >= arrayLength(&mc_adj_rw)) { return; }
        mc_adj_rw[slot] = MC_NO_TRI;
    }
}

@compute @workgroup_size(64, 1, 1)
fn mesh_curvature_adj_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ti = gid.x;
    if (ti >= mc.tri_count) { return; }
    let b = ti * 3u;
    if (b + 2u >= arrayLength(&mc_idx)) { return; }
    for (var e = 0u; e < 3u; e = e + 1u) {
        let vi = mc_idx[b + e];
        if (vi >= mc.vert_count || vi >= arrayLength(&mc_adj_fill)) { continue; }
        // Claim the next free slot for this vertex. Order within a vertex's list is
        // nondeterministic (whichever triangle's atomic lands first), which is FINE: the
        // consumers sum over the list, and addition of the SAME set is order-independent up to
        // float rounding — and the seed pass sums area-weighted vectors, not the list order.
        let k = atomicAdd(&mc_adj_fill[vi], 1u);
        if (k >= mc.adj_stride) { continue; } // truncate, never spill into a neighbour
        let slot = vi * mc.adj_stride + k;
        if (slot < arrayLength(&mc_adj_rw)) {
            mc_adj_rw[slot] = ti;
        }
    }
}

/// M3 PASS 1 — seed each vertex's AREA-WEIGHTED normal into `mc_nrm_a`.
///
/// This is the same accumulation the single-pass kernel does inline; splitting it out lets the
/// diffusion pass below run over a stable per-vertex field.
@compute @workgroup_size(64, 1, 1)
fn mesh_curvature_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    let vi = gid.x;
    if (vi >= mc.vert_count) { return; }
    let o = vi * 4u;
    if (o + 3u >= arrayLength(&mc_nrm_a)) { return; }
    var sum = vec3<f32>(0.0);
    var area_sum = 0.0;
    let adj_base = vi * mc.adj_stride;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = adj_base + k;
        if (slot >= arrayLength(&mc_adj)) { break; }
        let ti = mc_adj[slot];
        if (ti == MC_NO_TRI) { break; }
        if (ti >= mc.tri_count) { continue; }
        let wn = mc_tri_normal_weighted(ti);
        let a2 = length(wn);
        if (a2 < 1e-12) { continue; }
        sum = sum + wn;
        area_sum = area_sum + a2;
    }
    mc_nrm_a[o] = sum.x; mc_nrm_a[o + 1u] = sum.y; mc_nrm_a[o + 2u] = sum.z;
    mc_nrm_a[o + 3u] = area_sum;
}

/// M3 PASS 2 — one diffusion step: replace each vertex's normal with the average of itself and
/// its one-ring neighbours. Run N times (ping-ponging A→B→A) to widen the MEASUREMENT BASELINE.
///
/// WHY THIS AND NOT "BLUR THE RESULT": a gently domed surface changes its normal by ~2° per
/// vertex step, so the local signal is `1 - cos(2°) = 0.0006` — invisible, which is why a dome
/// currently reads as flat. Blurring that afterwards AVERAGES it and it stays invisible.
/// Diffusing the NORMALS first means the final comparison is against a neighbourhood several
/// vertices wide, where the normal has turned much further: at a radius of 16 vertices the same
/// dome gives `1 - cos(32°) = 0.152`, ~250x the single-vertex signal. That is what turns a smooth
/// panel into a readable midtone — the reference look — rather than only sharp edges registering.
@compute @workgroup_size(64, 1, 1)
fn mesh_curvature_diffuse(@builtin(global_invocation_id) gid: vec3<u32>) {
    let vi = gid.x;
    if (vi >= mc.vert_count) { return; }
    let o = vi * 4u;
    if (o + 3u >= arrayLength(&mc_nrm_b)) { return; }
    // Start from this vertex's own value, then add every neighbour reachable through its
    // incident triangles (the one-ring).
    var acc = vec3<f32>(mc_nrm_a[o], mc_nrm_a[o + 1u], mc_nrm_a[o + 2u]);
    var w = mc_nrm_a[o + 3u];
    let adj_base = vi * mc.adj_stride;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = adj_base + k;
        if (slot >= arrayLength(&mc_adj)) { break; }
        let ti = mc_adj[slot];
        if (ti == MC_NO_TRI) { break; }
        if (ti >= mc.tri_count) { continue; }
        let b = ti * 3u;
        if (b + 2u >= arrayLength(&mc_idx)) { continue; }
        for (var e = 0u; e < 3u; e = e + 1u) {
            let nv = mc_idx[b + e];
            if (nv == vi || nv >= mc.vert_count) { continue; }
            let no = nv * 4u;
            if (no + 3u >= arrayLength(&mc_nrm_a)) { continue; }
            acc = acc + vec3<f32>(mc_nrm_a[no], mc_nrm_a[no + 1u], mc_nrm_a[no + 2u]);
            w = w + mc_nrm_a[no + 3u];
        }
    }
    mc_nrm_b[o] = acc.x; mc_nrm_b[o + 1u] = acc.y; mc_nrm_b[o + 2u] = acc.z;
    mc_nrm_b[o + 3u] = w;
}

/// M3 PASS 3 — measure. Compare each vertex's OWN (undiffused) face normal against the DIFFUSED
/// neighbourhood normal in `mc_nrm_a`. The angle between them is the curvature over that
/// neighbourhood's width, so `smooth_iters` behaves as a genuine radius.
@compute @workgroup_size(64, 1, 1)
fn mesh_curvature_measure(@builtin(global_invocation_id) gid: vec3<u32>) {
    let vi = gid.x;
    if (vi >= mc.vert_count) { return; }
    let out_base = vi * 4u;
    if (out_base + 3u >= arrayLength(&mc_out)) { return; }

    // This vertex's own local normal (area-weighted, undiffused).
    var local_sum = vec3<f32>(0.0);
    var local_area = 0.0;
    var n_tris = 0u;
    var centroid = vec3<f32>(0.0);
    var c_n = 0u;
    let adj_base = vi * mc.adj_stride;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = adj_base + k;
        if (slot >= arrayLength(&mc_adj)) { break; }
        let ti = mc_adj[slot];
        if (ti == MC_NO_TRI) { break; }
        if (ti >= mc.tri_count) { continue; }
        let wn = mc_tri_normal_weighted(ti);
        let a2 = length(wn);
        if (a2 < 1e-12) { continue; }
        local_sum = local_sum + wn;
        local_area = local_area + a2;
        n_tris = n_tris + 1u;
        let b = ti * 3u;
        if (b + 2u < arrayLength(&mc_idx)) {
            centroid = centroid + (mc_pos(mc_idx[b]) + mc_pos(mc_idx[b + 1u]) + mc_pos(mc_idx[b + 2u])) / 3.0;
            c_n = c_n + 1u;
        }
    }
    if (n_tris == 0u) {
        mc_out[out_base] = 0.0; mc_out[out_base + 1u] = 0.0;
        mc_out[out_base + 2u] = 0.0; mc_out[out_base + 3u] = 0.0;
        return;
    }
    let local_n = normalize(local_sum / max(local_area, 1e-12));

    // CURVATURE = HOW MUCH THE NEIGHBOURHOOD'S NORMALS CANCEL.
    //
    // `mc_nrm_a` holds the summed AREA-WEIGHTED normals over this vertex's neighbourhood — the
    // one-ring at `smooth_iters == 0`, widening with every diffusion step. Dividing by the
    // accumulated weight gives a vector whose LENGTH is 1 when every face agrees and shrinks as
    // they diverge. `1 - length` is therefore the curvature, bounded and continuous.
    //
    // A previous attempt compared `dot(local_normal, normalized_neighbourhood_normal)`. That is
    // WRONG on closed geometry: both sides are unit vectors and a symmetric ring averages back to
    // the vertex's own direction, so every cube corner read 6e-8 instead of 0.39. The cube gate
    // caught it immediately. Cancellation-of-magnitude is the metric M1/M2 validated; M3 only
    // widens the neighbourhood it is measured over.
    let o = vi * 4u;
    var wide = vec3<f32>(mc_nrm_a[o], mc_nrm_a[o + 1u], mc_nrm_a[o + 2u]);
    var wide_w = mc_nrm_a[o + 3u];
    if (mc.smooth_iters == 0u) {
        // No diffusion ran, so `mc_nrm_a` still holds this vertex's OWN seed. Measuring against
        // that would compare a vector with itself (always flat), so build the one-ring here —
        // exactly the M1/M2 neighbourhood.
        var ring = local_sum;
        var ring_w = local_area;
        for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
            let slot = adj_base + k;
            if (slot >= arrayLength(&mc_adj)) { break; }
            let ti = mc_adj[slot];
            if (ti == MC_NO_TRI) { break; }
            if (ti >= mc.tri_count) { continue; }
            let b = ti * 3u;
            if (b + 2u >= arrayLength(&mc_idx)) { continue; }
            for (var e = 0u; e < 3u; e = e + 1u) {
                let nv = mc_idx[b + e];
                if (nv == vi || nv >= mc.vert_count) { continue; }
                let no = nv * 4u;
                if (no + 3u >= arrayLength(&mc_nrm_a)) { continue; }
                ring = ring + vec3<f32>(mc_nrm_a[no], mc_nrm_a[no + 1u], mc_nrm_a[no + 2u]);
                ring_w = ring_w + mc_nrm_a[no + 3u];
            }
        }
        wide = ring;
        wide_w = ring_w;
    }
    let flatness = clamp(length(wide / max(wide_w, 1e-12)), 0.0, 1.0);
    let curv_mag = select(1.0 - flatness, 0.0, n_tris < 2u);

    // Sign: convex if the vertex sits OUTWARD of its neighbourhood centroid along its own normal.
    var signed_curv = curv_mag;
    if (c_n > 0u) {
        let to_v = mc_pos(vi) - centroid / f32(c_n);
        signed_curv = select(-curv_mag, curv_mag, dot(to_v, local_n) >= 0.0);
    }
    mc_out[out_base] = curv_mag;
    mc_out[out_base + 1u] = signed_curv;
    mc_out[out_base + 2u] = max(signed_curv, 0.0);
    mc_out[out_base + 3u] = max(-signed_curv, 0.0);
}

@compute @workgroup_size(64, 1, 1)
fn mesh_curvature(@builtin(global_invocation_id) gid: vec3<u32>) {
    let vi = gid.x;
    if (vi >= mc.vert_count) {
        return;
    }
    let out_base = vi * 4u;
    if (out_base + 3u >= arrayLength(&mc_out)) {
        return;
    }

    // Pass 1: average the incident face normals. A vertex in a flat region sees near-identical
    // normals (mean length ~1); a vertex on an edge sees normals pointing apart, so they partly
    // cancel and the mean is SHORTER. That shortening is the curvature signal, and it is
    // continuous — which is exactly what the point-sampled field version failed to be.
    var sum = vec3<f32>(0.0);
    var area_sum = 0.0;
    var n_tris = 0u;
    let adj_base = vi * mc.adj_stride;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = adj_base + k;
        if (slot >= arrayLength(&mc_adj)) {
            break;
        }
        let ti = mc_adj[slot];
        // The table is filled densely from slot 0, so the first empty slot ends this vertex's
        // list — no need to scan the remaining stride.
        if (ti == MC_NO_TRI) {
            break;
        }
        if (ti >= mc.tri_count) {
            continue;
        }
        let wn = mc_tri_normal_weighted(ti);
        let area2 = length(wn);
        if (area2 < 1e-12) {
            continue; // zero-area triangle contributes nothing (and would NaN if normalized)
        }
        // `wn` is already area-weighted (|wn| == 2 * area). Track the total weight so the mean
        // below divides by AREA, not by triangle count.
        sum = sum + wn;
        area_sum = area_sum + area2;
        n_tris = n_tris + 1u;
    }

    if (n_tris == 0u) {
        // Isolated vertex (no incident triangles recorded): report FLAT rather than leaving the
        // buffer uninitialised. A stale value here is read as real data by the vertex shader.
        mc_out[out_base] = 0.0;
        mc_out[out_base + 1u] = 0.0;
        mc_out[out_base + 2u] = 0.0;
        mc_out[out_base + 3u] = 0.0;
        return;
    }

    // AREA-weighted mean direction. Dividing the summed weighted normals by the summed WEIGHT
    // (not by the count) yields a unit-length vector when all faces agree, and a shorter one as
    // they diverge — the same 0..1 flatness measure as M1, but now a sliver cannot outvote a
    // large face.
    let mean = sum / max(area_sum, 1e-12);
    let flatness = clamp(length(mean), 0.0, 1.0);
    let curv_mag = select(1.0 - flatness, 0.0, n_tris < 2u);

    // SIGN: convex vs concave. The averaged face normal of a convex region points AWAY from the
    // vertex's neighbourhood centroid; a concave region's points toward it. Compare the mean
    // normal against the direction from the incident triangles' centroid to this vertex.
    var centroid = vec3<f32>(0.0);
    var c_n = 0u;
    for (var k = 0u; k < mc.adj_stride; k = k + 1u) {
        let slot = adj_base + k;
        if (slot >= arrayLength(&mc_adj)) { break; }
        let ti = mc_adj[slot];
        if (ti == MC_NO_TRI) { break; }
        if (ti >= mc.tri_count) { continue; }
        let b = ti * 3u;
        if (b + 2u >= arrayLength(&mc_idx)) { continue; }
        centroid = centroid + (mc_pos(mc_idx[b]) + mc_pos(mc_idx[b + 1u]) + mc_pos(mc_idx[b + 2u])) / 3.0;
        c_n = c_n + 1u;
    }
    var signed_curv = curv_mag;
    if (c_n > 0u) {
        let to_v = mc_pos(vi) - centroid / f32(c_n);
        // Outward (same side as the mean normal) = convex = positive.
        signed_curv = select(-curv_mag, curv_mag, dot(to_v, mean) >= 0.0);
    }

    mc_out[out_base] = curv_mag;                       // mask (unsigned magnitude)
    mc_out[out_base + 1u] = signed_curv;               // signed
    mc_out[out_base + 2u] = max(signed_curv, 0.0);     // convex
    mc_out[out_base + 3u] = max(-signed_curv, 0.0);    // concave
}
