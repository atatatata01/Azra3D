// ============================================================
// Raster shared draw helpers (Slice 18)
// ============================================================
//
// Free functions shared by the G-buffer pass (`dispatch_raster_pass`) AND the
// picking pass (`pick_node_at`) in raster_render.rs. Widened to `pub(crate)`
// and re-exported at the crate root (`pub(crate) use raster_common::*;`) so
// the unqualified call sites in the sibling raster modules keep resolving.

use eframe::wgpu;
use glam::{Mat4, Vec3, Vec4};

/// Extract the 6 frustum planes (Gribb–Hartmann) from a column-major view-projection matrix
/// (`clip = view_proj * world`). Each plane `(a,b,c,d)` is normalized with its xyz pointing INSIDE
/// the frustum: a world point `p` is inside a plane iff `a*p.x + b*p.y + c*p.z + d >= 0`.
/// A1 (Assembly) object frustum culling. Order: left,right,bottom,top,near,far.
pub(crate) fn frustum_planes(view_proj: &Mat4) -> [Vec4; 6] {
    let m = view_proj.to_cols_array_2d(); // m[col][row] (column-major)
    let row = |i: usize, k: usize| m[k][i]; // row i, component k
    let plane = |a: f32, b: f32, c: f32, d: f32| {
        let v = Vec4::new(a, b, c, d);
        v / v.truncate().length().max(1e-8)
    };
    [
        plane(row(3, 0) + row(0, 0), row(3, 1) + row(0, 1), row(3, 2) + row(0, 2), row(3, 3) + row(0, 3)), // left
        plane(row(3, 0) - row(0, 0), row(3, 1) - row(0, 1), row(3, 2) - row(0, 2), row(3, 3) - row(0, 3)), // right
        plane(row(3, 0) + row(1, 0), row(3, 1) + row(1, 1), row(3, 2) + row(1, 2), row(3, 3) + row(1, 3)), // bottom
        plane(row(3, 0) - row(1, 0), row(3, 1) - row(1, 1), row(3, 2) - row(1, 2), row(3, 3) - row(1, 3)), // top
        plane(row(2, 0), row(2, 1), row(2, 2), row(2, 3)), // near — [0,1] depth (wgpu/DX): near = row2, NOT row3+row2
        plane(row(3, 0) - row(2, 0), row(3, 1) - row(2, 1), row(3, 2) - row(2, 2), row(3, 3) - row(2, 3)), // far
    ]
}

/// World-space AABB of a local AABB under `model` (transforms all 8 corners). A1 culling.
pub(crate) fn world_aabb(model: &Mat4, lmin: Vec3, lmax: Vec3) -> (Vec3, Vec3) {
    let mut mn = Vec3::splat(f32::INFINITY);
    let mut mx = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { lmin.x } else { lmax.x },
            if i & 2 == 0 { lmin.y } else { lmax.y },
            if i & 4 == 0 { lmin.z } else { lmax.z },
        );
        let w = model.transform_point3(c);
        mn = mn.min(w);
        mx = mx.max(w);
    }
    (mn, mx)
}

/// True if the world-space AABB is fully OUTSIDE the frustum (safe to cull from the main pass).
/// Conservative positive-vertex test: if the AABB corner farthest along a plane normal is still
/// behind that plane, the whole box is outside. Never false-culls a visible box. A1 culling.
pub(crate) fn aabb_outside_frustum(planes: &[Vec4; 6], wmin: Vec3, wmax: Vec3) -> bool {
    for pl in planes {
        let n = pl.truncate();
        let p = Vec3::new(
            if n.x >= 0.0 { wmax.x } else { wmin.x },
            if n.y >= 0.0 { wmax.y } else { wmin.y },
            if n.z >= 0.0 { wmax.z } else { wmin.z },
        );
        if n.dot(p) + pl.w < 0.0 {
            return true;
        }
    }
    false
}

/// AZRA_MASTER_ROADMAP B.3: frustum-cull leaves of a scene BVH.
/// Each leaf's `first` is a draw-call index (`SceneBvhBody.start_entry`).
/// Fully outside subtrees are collected without visiting every leaf individually when the
/// parent AABB is outside — O(visible + culled_groups) vs O(N) linear scan.
pub(crate) fn scene_bvh_collect_frustum_culled(
    nodes: &[core_math::bvh::BvhNode],
    planes: &[Vec4; 6],
) -> Vec<u32> {
    let mut out = Vec::new();
    if nodes.is_empty() {
        return out;
    }
    scene_bvh_frustum_rec(nodes, 0, planes, &mut out);
    out
}

fn scene_bvh_frustum_rec(
    nodes: &[core_math::bvh::BvhNode],
    idx: usize,
    planes: &[Vec4; 6],
    out: &mut Vec<u32>,
) {
    if idx >= nodes.len() {
        return;
    }
    let n = nodes[idx];
    let wmin = Vec3::from_array(n.aabb_min);
    let wmax = Vec3::from_array(n.aabb_max);
    if aabb_outside_frustum(planes, wmin, wmax) {
        // Whole subtree culled — gather all leaf draw indices.
        scene_bvh_collect_leaves(nodes, idx, out);
        return;
    }
    if n.count > 0 {
        // Leaf straddles or is inside — keep (not culled).
        return;
    }
    // Internal: both children.
    let left = n.first as usize;
    scene_bvh_frustum_rec(nodes, left, planes, out);
    scene_bvh_frustum_rec(nodes, left + 1, planes, out);
}

fn scene_bvh_collect_leaves(nodes: &[core_math::bvh::BvhNode], idx: usize, out: &mut Vec<u32>) {
    if idx >= nodes.len() {
        return;
    }
    let n = nodes[idx];
    if n.count > 0 {
        out.push(n.first); // draw index
        return;
    }
    let left = n.first as usize;
    scene_bvh_collect_leaves(nodes, left, out);
    scene_bvh_collect_leaves(nodes, left + 1, out);
}

/// Prefer BVH when draw count is large enough that tree build pays off.
pub(crate) const FRUSTUM_BVH_MIN_DRAWS: usize = 8;
/// Above this draw count, rebuild cost of `build_scene_bvh` dominates linear plane tests
/// (C.1 10k instance forests). Keep BVH for the mid band only.
pub(crate) const FRUSTUM_BVH_MAX_DRAWS: usize = 512;

// ---------------------------------------------------------------------------
// AZRA_MASTER_ROADMAP C.2 — per-draw cull flags (replaces bool shadow_only)
// ---------------------------------------------------------------------------
/// Skip main G-buffer / wire color pass (still may cast shadows).
pub(crate) const DRAW_CULL_MAIN: u8 = 1;
/// Skip shadow cascade draws (C.2: true frustum-out bodies pay nothing).
pub(crate) const DRAW_CULL_SHADOW: u8 = 2;
/// Fully outside camera frustum → skip main + shadows.
pub(crate) const DRAW_CULL_FRUSTUM: u8 = DRAW_CULL_MAIN | DRAW_CULL_SHADOW;
/// Occluded by nearer geometry → skip main, keep shadows (receivers still need them).
pub(crate) const DRAW_CULL_OCCLUDED: u8 = DRAW_CULL_MAIN;

// ---------------------------------------------------------------------------
// C.8 — draw entry kind (parallel Vec, not HashMap)
// ---------------------------------------------------------------------------
/// Ordinary mesh_cache draw (batchable).
pub(crate) const DRAW_KIND_MESH: u8 = 0;
/// Voxel chunk via resident pool (indirect).
pub(crate) const DRAW_KIND_VOXEL_RESIDENT: u8 = 1;
/// SDF body GPU-resident slot (not pooled).
pub(crate) const DRAW_KIND_SDF_RESIDENT: u8 = 2;
/// SDF body in shared SdfMeshPool (pooled multi-draw).
pub(crate) const DRAW_KIND_SDF_POOLED: u8 = 3;

/// D4: mesh_cache draw whose cid has a resident cluster DAG — drawn via the
/// cluster cut's multi-draw, NOT the D1 batch path. Marked "special" so the
/// batch partition + per-batch encodes structurally exclude it (no double draw).
pub(crate) const DRAW_KIND_CLUSTERED: u8 = 4;

#[inline]
pub(crate) fn draw_kind_is_special(kind: u8) -> bool {
    kind != DRAW_KIND_MESH
}

#[inline]
pub(crate) fn draw_skip_main(cull: u8) -> bool {
    cull & DRAW_CULL_MAIN != 0
}
#[inline]
pub(crate) fn draw_skip_shadow(cull: u8) -> bool {
    cull & DRAW_CULL_SHADOW != 0
}

// MERGE 2026-08-01: CPU OcclusionBuffer RESTORED (verified path, e04e4a3 latch). GPU Hi-Z
// (The GPU Hi-Z experiment (render/src/hiz.rs) was measured at 0% cull and DELETED 2026-08-03 — PHASE_D_ENTRY.md D2.)

// ---------------------------------------------------------------------------
// PLAN_FOR_GROK_VIEWPORT_PERF P4 — software hierarchical occlusion (AABB Hi-Z)
// ---------------------------------------------------------------------------
//
// Full GPU depth-pyramid against last-frame depth is the ideal form; this is the
// same *idea* without a GPU readback dependency: low-res depth, front-to-back
// AABB fills, cull only when the whole screen rect is behind existing depth.
// Never false-culls a visible box (rect is inflated AABB projection; test uses
// nearest depth; crosses-near → keep).

/// Default software occlusion buffer resolution (coarse; cost is O(W×H×N) pixels).
pub(crate) const OCCLUSION_BUF_W: usize = 64;
pub(crate) const OCCLUSION_BUF_H: usize = 36;

/// Low-res closest-depth buffer for conservative object occlusion culling.
pub(crate) struct OcclusionBuffer {
    w: usize,
    h: usize,
    /// Closest depth written so far (`1.0` = empty / far). wgpu [0,1] depth: smaller = closer.
    z: Vec<f32>,
}

impl OcclusionBuffer {
    pub(crate) fn new(w: usize, h: usize) -> Self {
        Self {
            w: w.max(1),
            h: h.max(1),
            z: vec![1.0; w.max(1) * h.max(1)],
        }
    }

    /// Project world AABB corners through `view_proj` → screen rect + near/far NDC z.
    /// Returns `None` if any corner is on/behind the near plane (do not cull).
    pub(crate) fn project_aabb(
        view_proj: &Mat4,
        wmin: Vec3,
        wmax: Vec3,
    ) -> Option<(f32, f32, f32, f32, f32, f32)> {
        let mut ndc_min = Vec3::splat(f32::INFINITY);
        let mut ndc_max = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8u32 {
            let c = Vec3::new(
                if i & 1 == 0 { wmin.x } else { wmax.x },
                if i & 2 == 0 { wmin.y } else { wmax.y },
                if i & 4 == 0 { wmin.z } else { wmax.z },
            );
            let clip = *view_proj * Vec4::new(c.x, c.y, c.z, 1.0);
            // Behind / on near: cannot safely cull (and NDC is unstable).
            if clip.w <= 1e-4 {
                return None;
            }
            let ndc = clip.truncate() / clip.w;
            ndc_min = ndc_min.min(ndc);
            ndc_max = ndc_max.max(ndc);
        }
        Some((
            ndc_min.x, ndc_min.y, ndc_min.z,
            ndc_max.x, ndc_max.y, ndc_max.z,
        ))
    }

    /// Convert NDC xy rect to integer buffer bounds (half-open). Empty if fully off-screen.
    fn ndc_to_rect(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<(usize, usize, usize, usize)> {
        // NDC [-1,1] → [0,1] → pixels. Y: NDC +1 is top; buffer row 0 is top (matches most
        // screen maps — only relative coverage matters for occlusion).
        let to_x = |x: f32| ((x * 0.5 + 0.5) * self.w as f32);
        let to_y = |y: f32| ((1.0 - (y * 0.5 + 0.5)) * self.h as f32);
        let mut ix0 = to_x(x0).floor() as i32;
        let mut ix1 = to_x(x1).ceil() as i32;
        let mut iy0 = to_y(y1).floor() as i32; // y1 is max NDC y → smaller buffer y after flip
        let mut iy1 = to_y(y0).ceil() as i32;
        if ix0 > ix1 {
            std::mem::swap(&mut ix0, &mut ix1);
        }
        if iy0 > iy1 {
            std::mem::swap(&mut iy0, &mut iy1);
        }
        ix0 = ix0.max(0);
        iy0 = iy0.max(0);
        ix1 = ix1.min(self.w as i32);
        iy1 = iy1.min(self.h as i32);
        if ix0 >= ix1 || iy0 >= iy1 {
            return None;
        }
        Some((ix0 as usize, iy0 as usize, ix1 as usize, iy1 as usize))
    }

    /// Test whether the world AABB is fully occluded; if not, rasterize it as an occluder.
    /// Returns `true` when safe to skip the main G-buffer draw (still cast shadows).
    pub(crate) fn test_and_insert_aabb(
        &mut self,
        view_proj: &Mat4,
        wmin: Vec3,
        wmax: Vec3,
    ) -> bool {
        let Some((nx0, ny0, nz0, nx1, ny1, nz1)) = Self::project_aabb(view_proj, wmin, wmax) else {
            return false;
        };
        // Degenerate / behind far plane entirely.
        let z_near = nz0.min(nz1).clamp(0.0, 1.0);
        let z_far = nz0.max(nz1).clamp(0.0, 1.0);
        if z_near >= 1.0 {
            return true; // completely past far — treat as culled from main pass
        }
        let Some((x0, y0, x1, y1)) = self.ndc_to_rect(nx0, ny0, nx1, ny1) else {
            return false;
        };
        let area = (x1 - x0) * (y1 - y0);
        let full = self.w * self.h;
        // Huge screen coverage: skip as occluder write AND as cull candidate — AABB would
        // over-claim a full-screen wall. Never false-cull large/close objects.
        if area * 2 > full {
            return false;
        }

        // Fully occluded iff every pixel already has something closer than this box's front.
        let mut occluded = true;
        for y in y0..y1 {
            let row = y * self.w;
            for x in x0..x1 {
                if self.z[row + x] >= z_near - 1e-5 {
                    occluded = false;
                    break;
                }
            }
            if !occluded {
                break;
            }
        }
        if occluded {
            return true;
        }

        // Write nearest depth (solid-ish AABB). Underclaims for hollow shells, overclaims vs
        // true silhouette — acceptable for object-level cull; never used alone as truth for
        // selection/picking (picking path does not call this).
        for y in y0..y1 {
            let row = y * self.w;
            for x in x0..x1 {
                let i = row + x;
                if z_near < self.z[i] {
                    self.z[i] = z_near;
                }
            }
        }
        let _ = z_far; // kept for future Hi-Z mip max-depth experiments
        false
    }
}

/// POOL fix — draw ALL resident voxel-sculpt chunks in ONE pass over the shared vertex/index/indirect
/// pool, replacing the ~12k per-chunk draw calls that were the measured 1024³ draw-bound wall. Binds the
/// whole pool as vertex(0)/index buffer + the shared instance at `inst_slice` (all resident chunks of a
/// body share one InstanceData), then:
///   - if MULTI_DRAW_INDIRECT is available: `multi_draw_indexed_indirect(pool_indirect, 0, pool_cap_slots)`
///     — ONE call; empty slots (index_count=0) self-skip.
///   - else: a per-slot `draw_indexed_indirect` loop (still ONE pool bind — a partial win; native Vulkan/
///     DX12/Metal all expose MULTI_DRAW_INDIRECT, so the fallback is rare).
/// No-op when there are no resident draws or the pool isn't allocated. `gpu` borrows are read-only here.
/// THE per-chunk pool-vs-CPU display classification (single point — G-buffer, shadow and picking
/// draw lists MUST all call this so the pool multi-draw set and the per-chunk CPU-fallback set are
/// the same partition everywhere, provably disjoint per chunk: `true` → the chunk is drawn ONLY by
/// the pool multi-draw (its slot's indirect args are live); `false` → ONLY the per-chunk CPU-mesh
/// fallback may draw it (every slot-drop path zeroes the slot's indirect, so the pool self-skips).
/// `resident_base` = the body-level gate (gpu_resident_mesh && !cpu_paint_path()).
pub(crate) fn voxel_chunk_draws_from_pool(
    gpu: Option<&crate::GpuCtx>,
    resident_base: bool,
    chunk_index: usize,
) -> bool {
    voxel_chunk_pool_partition(
        gpu.map(|g| g.voxel_chunk_gpu.as_slice()).unwrap_or(&[]),
        resident_base,
        chunk_index,
    )
}

/// Core of the partition, on the slot table itself (pure — the GPU pool consistency test drives
/// this exact function to prove the pool/fallback sets are disjoint for every possible slot state).
pub(crate) fn voxel_chunk_pool_partition(
    chunk_slots: &[Option<crate::voxel_gpu_mesh::VoxelChunkGpu>],
    resident_base: bool,
    chunk_index: usize,
) -> bool {
    resident_base && chunk_slots.get(chunk_index).map_or(false, |s| s.is_some())
}

pub(crate) fn draw_resident_pool_with_instance(
    rpass: &mut wgpu::RenderPass<'_>,
    gpu: &crate::GpuCtx,
    has_resident: bool,
    inst_slice: wgpu::BufferSlice<'_>,
) {
    if !has_resident { return; }
    let Some(mesher) = gpu.voxel_mesh.as_ref() else { return; };
    let (Some(pv), Some(pi), Some(pind)) = (mesher.pool_verts(), mesher.pool_idx(), mesher.pool_indirect()) else { return; };
    let cap = mesher.pool_cap_slots();
    if cap == 0 { return; }
    rpass.set_vertex_buffer(0, pv.slice(..));
    rpass.set_vertex_buffer(1, inst_slice);
    rpass.set_index_buffer(pi.slice(..), wgpu::IndexFormat::Uint32);
    if gpu.device.features().contains(wgpu::Features::MULTI_DRAW_INDIRECT) {
        rpass.multi_draw_indexed_indirect(pind, 0, cap);
    } else {
        for slot in 0..cap {
            rpass.draw_indexed_indirect(pind, slot as u64 * crate::voxel_gpu_mesh::POOL_INDIRECT_STRIDE_BYTES);
        }
    }
}

/// Convenience for the G-buffer + shadow passes, which use the SHARED `gpu.raster.instance_buffer` at the
/// byte offset of the first resident draw_call. (The picking pass uses per-draw instance buffers, so it
/// calls `draw_resident_pool_with_instance` directly with its own slice.)
///
/// C.8: `first_resident_dc` is the lowest draw index with `DRAW_KIND_VOXEL_RESIDENT` (or None).
pub(crate) fn draw_resident_pool(
    rpass: &mut wgpu::RenderPass<'_>,
    gpu: &crate::GpuCtx,
    first_resident_dc: Option<usize>,
    instance_stride: u64,
) {
    let Some(first_dc) = first_resident_dc else { return; };
    let byte_off = first_dc as u64 * instance_stride;
    draw_resident_pool_with_instance(rpass, gpu, true, gpu.raster.instance_buffer.slice(byte_off..));
}

/// GPU_CURVATURE_KERNEL_PLAN.md P2 — group-2 curvature carry, shared by every SDF resident/pool
/// draw helper below. `None` means "this pipeline has no group 2 at all" (the PICKING pipeline's
/// own layout is camera-only — `set_bind_group(2, ..)` against it would fail validation outright,
/// not merely bind nothing), so picking passes `None` and these helpers skip the call entirely
/// rather than bind a dummy. The G-buffer pass passes `Some(&(dummy, curv_bgs))`.
pub(crate) type CurvBind<'a> = Option<(
    &'a wgpu::BindGroup,
    &'a std::collections::HashMap<core_math::scene_graph::NodeId, wgpu::BindGroup>,
)>;

fn bind_curvature(rpass: &mut wgpu::RenderPass<'_>, curv: CurvBind<'_>, nid: core_math::scene_graph::NodeId) {
    // DIAGNOSTIC (`AZRA_CURV_BIND=1`) — owner 2026-07-29.
    //
    // Grok's GPU readback proved the curvature BUFFER holds real, radius-sensitive data
    // (signed channel spans 0..0.98; mean moves 0.069 -> 0.270 when radius goes to auto) while the
    // rendered pixels do not change at all. So the loss is downstream of the kernel write, and the
    // two candidates that survive are: this bind falling back to the DUMMY, or the fragment
    // discarding what it read.
    //
    // A dummy fallback here is INVISIBLE by construction — the shader's arrayLength guard turns it
    // into `vertex_curv = 0.0` for every vertex, which renders exactly like "no curvature". That is
    // indistinguishable from a working bind with a flat buffer unless something says which
    // happened. This says it.
    let Some((dummy, curv_bgs)) = curv else {
        if std::env::var("AZRA_CURV_BIND").as_deref() == Ok("1") {
            eprintln!("[CurvBind] {nid:?}: NO CurvBind supplied — pass binds nothing (group 2 keeps \
                       whatever the previous draw left)");
        }
        return;
    };
    match curv_bgs.get(&nid) {
        Some(bg) => {
            if std::env::var("AZRA_CURV_BIND").as_deref() == Ok("1") {
                eprintln!("[CurvBind] {nid:?}: REAL per-body buffer bound");
            }
            rpass.set_bind_group(2, bg, &[])
        }
        None => {
            if std::env::var("AZRA_CURV_BIND").as_deref() == Ok("1") {
                eprintln!("[CurvBind] {nid:?}: DUMMY bound ({} bodies in map) — every vertex reads \
                           0.0, so curvature cannot render for this body",
                    curv_bgs.len());
            }
            rpass.set_bind_group(2, dummy, &[])
        }
    }
}

/// True only when body `nid` has a curvature carry buffer that matches the CURRENT mesh
/// (generation + content_gen + vert_cap). Used by BOTH instance-flag packing
/// (`body_has_current_vertex_curvature`) and group-2 bind construction — they MUST agree.
/// Flag set + dummy bind = OOB storage read → device removal (no Rust panic).
pub(crate) fn vertex_curv_slot_current<'a>(
    gpu: &'a crate::GpuCtx,
    nid: core_math::scene_graph::NodeId,
    pooled: bool,
) -> Option<&'a crate::gpu_curvature::CurvVertexSlot> {
    let body_slot = gpu.sdf_body_mesh.get(&nid)?;
    let content_gen = body_slot.content_gen;
    let (mesh_gen, vert_cap) = if pooled {
        let pool = gpu.sdf_mesh_pool.as_ref()?;
        let slot = pool.slot(nid)?;
        let group = pool.group_of(nid)?;
        (group.generation, slot.vert_cap)
    } else {
        (body_slot.generation, body_slot.vert_cap)
    };
    let curv_slot = gpu.sdf_body_curvature.get(&nid)?;
    if curv_slot.mesh_generation != mesh_gen
        || curv_slot.mesh_content_gen != content_gen
        || curv_slot.cap != vert_cap
    {
        return None; // stale — mesh buffers and/or content rewrote since this curvature bake
    }
    Some(curv_slot)
}

/// GPU_CURVATURE_KERNEL_PLAN.md P2 — build body `nid`'s group-2 bind group directly off `&GpuCtx`
/// (no `&self` needed), for call sites that already have `gpu: &mut GpuCtx` reborrowed disjointly
/// from the rest of `AzraApp` (e.g. `dispatch_raster_pass`, where `self.body_has_current_vertex_
/// curvature` would be illegal to call while `gpu` is live). `pooled` is whatever the caller
/// already resolved via `sdf_mesh_presentation` earlier in the SAME pass — reusing it here avoids
/// a second facade call and keeps this function a pure `&GpuCtx` lookup.
///
/// `None` when the body has no CURRENT curvature buffer (mesh generation/vert_cap mismatch or no
/// entry at all) — the caller binds the shared dummy in that case. Instance flag packing uses the
/// same readiness test (`vertex_curv_slot_current`) so the VS never sees flag+dummy together.
pub(crate) fn curvature_bind_group_for_body(
    gpu: &crate::GpuCtx,
    nid: core_math::scene_graph::NodeId,
    pooled: bool,
) -> Option<wgpu::BindGroup> {
    let curv_slot = vertex_curv_slot_current(gpu, nid, pooled)?;
    Some(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("curv_body_bg"),
        layout: &gpu.raster.curvature_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: curv_slot.buf.as_entire_binding(),
        }],
    }))
}

/// G2 (Phase G): draw ONE SDF body's GPU-resident QEF-DC mesh slot via `draw_indexed_indirect` — the
/// no-readback path. `slot.verts`/`slot.idx` are filled GPU-side by `mesh_into_slot`; `slot.indirect`
/// carries the GPU-written index_count (clamped by the shader's atomicMin → never OOB). Vertex layout
/// [pos.xyz nrm.xyz uv.xy] matches the raster + picking pipelines, so the same pipeline binds it. The
/// shadow pass leaves resident bodies unshadowed (no GpuMesh in mesh_cache → it auto-skips them).
pub(crate) fn draw_sdf_resident_slot(
    rpass: &mut wgpu::RenderPass<'_>,
    gpu: &crate::GpuCtx,
    nid: core_math::scene_graph::NodeId,
    inst_slice: wgpu::BufferSlice<'_>,
    curv: CurvBind<'_>,
) {
    let Some(slot) = gpu.sdf_body_mesh.get(&nid) else { return; };
    bind_curvature(rpass, curv, nid);
    rpass.set_vertex_buffer(0, slot.verts.slice(..));
    rpass.set_vertex_buffer(1, inst_slice);
    rpass.set_index_buffer(slot.idx.slice(..), wgpu::IndexFormat::Uint32);
    rpass.draw_indexed_indirect(&slot.indirect, 0);
}

/// Stage B-1 / C1 (Tiled-DC): draw every pooled SPARSE SDF body this pass's prep marked drawn —
/// ONE `multi_draw_indexed_indirect` per body over its args BLOCK (a tiled body's block holds one
/// `DrawIndexedIndirect` entry per surface tile; a monolithic body's holds one). Per-frame
/// visibility is the pool's draw list (`clear_frame_visibility` + `mark_drawn` in prep) — a
/// hidden / dense-switched body is never listed, so its block is never referenced (per-entry
/// instance_count zeroing cannot scale to thousands of tile entries per frame). first_instance =
/// the body's instance slot in the pool's shared instance buffer (written per pass by
/// `mark_drawn`). Returns true when the pooled draws are HANDLED (fast path drew, or nothing is
/// listed); false → the caller runs the per-slot fallback (`draw_sdf_pool_slot`) inside its draw
/// loop (devices without MULTI_DRAW_INDIRECT / INDIRECT_FIRST_INSTANCE — same geometry, same
/// args buffer).
/// Phase 4 (multi-buffer pool): a `multi_draw_indexed_indirect` reads ONE vertex buffer + ONE
/// index buffer per call, so bodies are drawn PER GROUP — the drawn list is sorted by buffer
/// group and the vertex/index buffers are rebound only when the group changes (typically 1-3
/// rebinds per pass; the shared instance buffer is bound once). BOTH the G-buffer and the picking
/// pass call this same function, so picking stays lockstep with rendering across groups (P0).
/// `curv`: see `CurvBind` — `None` for picking (no group 2 in its layout), `Some` for the
/// G-buffer pass. GPU_CURVATURE_KERNEL_PLAN.md P2.
pub(crate) fn draw_sdf_pool_multi(rpass: &mut wgpu::RenderPass<'_>, gpu: &crate::GpuCtx, curv: CurvBind<'_>) -> bool {
    let Some(pool) = gpu.sdf_mesh_pool.as_ref() else { return true; };
    if pool.frame_drawn().is_empty() { return true; }
    if !pool.multi_draw_ready { return false; }
    // Phase 4b: a body may span multiple SEGMENTS (per-body multi-group tiling) — every segment
    // is one multi-draw over its own args block in its own group. Single-segment bodies (the
    // common case) contribute exactly one entry, as before.
    let mut drawn: Vec<(usize, core_math::scene_graph::NodeId, u32)> = Vec::new();
    for n in pool.frame_drawn() {
        for seg in 0..pool.segment_count(*n) {
            if let Some(s) = pool.slot_seg(*n, seg) {
                drawn.push((s.group, *n, seg));
            }
        }
    }
    drawn.sort_unstable_by_key(|(g, n, seg)| (*g, n.0, *seg)); // group-major → minimal rebinds
    rpass.set_vertex_buffer(1, pool.instances.slice(..));
    let mut bound: Option<usize> = None;
    // GPU_CURVATURE_KERNEL_PLAN.md P2: group 2 rebinds PER BODY (not per group) — each
    // `(gi, nid, seg)` triple is already its own draw call (the multi-draw batches only within
    // one body's tile block, not across bodies), so a per-body curvature bind adds no extra draw
    // calls, only an extra `set_bind_group` alongside ones already happening.
    let mut cur_curv: Option<core_math::scene_graph::NodeId> = None;
    for (gi, nid, seg) in drawn {
        if bound != Some(gi) {
            let grp = pool.group(gi);
            rpass.set_vertex_buffer(0, grp.verts.slice(..));
            rpass.set_index_buffer(grp.idx.slice(..), wgpu::IndexFormat::Uint32);
            bound = Some(gi);
        }
        if cur_curv != Some(nid) {
            bind_curvature(rpass, curv, nid);
            cur_curv = Some(nid);
        }
        let Some(s) = pool.slot_seg(nid, seg) else { continue; };
        rpass.multi_draw_indexed_indirect(&pool.group(gi).indirect, s.indirect_offset(), s.draw_entries());
    }
    true
}

/// Stage B-1 / C1 fallback: draw ONE pooled body via per-entry `draw_indexed_indirect` over its
/// args block, with a caller instance binding (first_instance is 0 in the args when
/// `multi_draw_ready` is false). Used by both the G-buffer and picking loops so the two passes
/// stay lockstep (P0 selection rule).
pub(crate) fn draw_sdf_pool_slot(
    rpass: &mut wgpu::RenderPass<'_>,
    gpu: &crate::GpuCtx,
    nid: core_math::scene_graph::NodeId,
    inst_slice: wgpu::BufferSlice<'_>,
    curv: CurvBind<'_>,
) {
    let Some(pool) = gpu.sdf_mesh_pool.as_ref() else { return; };
    bind_curvature(rpass, curv, nid);
    // Phase 4b: draw EVERY segment of the body (a multi-group body's tiles span segments).
    for seg in 0..pool.segment_count(nid) {
        let Some(s) = pool.slot_seg(nid, seg) else { continue; };
        let grp = pool.group(s.group); // the segment's geometry lives in its buffer group
        rpass.set_vertex_buffer(0, grp.verts.slice(..));
        rpass.set_vertex_buffer(1, inst_slice.clone());
        rpass.set_index_buffer(grp.idx.slice(..), wgpu::IndexFormat::Uint32);
        for e in 0..s.draw_entries() as u64 {
            rpass.draw_indexed_indirect(&grp.indirect,
                s.indirect_offset() + e * crate::sdf_gpu_mesh::SDF_POOL_INDIRECT_STRIDE);
        }
    }
}

/// Compute tight per-body AABB for dual contouring mesh extraction.
/// Delegates to the canonical `compute_body_bake_bounds()` in body_volume_cache.
pub(crate) fn body_aabb_bounds(prims: &[core_math::primitives::SdfPrimitive], mirror_flags: u8, mirror_center: glam::Vec3, bevel_radius: f32) -> (glam::Vec3, glam::Vec3) {
    crate::body_volume_cache::compute_body_bake_bounds(prims, mirror_flags, mirror_center, bevel_radius)
}

// ============================================================
// C.6 — multi-mesh instance batch count (pure, gateable)
// ============================================================

/// Count mesh_cache-path `draw_indexed` groups after optional sort-by-cid.
///
/// Mirrors production consecutive-only merge (`raster_gbuffer` main pass).
/// Entries: `(mesh_key, skip_main)` — skipped entries are dropped (frustum/occlusion).
///
/// **C.6 gate:** N distinct meshes × M instances interleaved →
/// `sort_by_cid = true` ⇒ **N** draws; `false` ⇒ **N×M** draws (sabotage).
pub fn count_mesh_instance_draws(entries: &[(u64, bool)], sort_by_cid: bool) -> u32 {
    let mut visible: Vec<u64> = entries
        .iter()
        .filter(|(_, skip)| !*skip)
        .map(|(cid, _)| *cid)
        .collect();
    if sort_by_cid {
        visible.sort_unstable();
    }
    let mut draws = 0u32;
    let mut i = 0usize;
    while i < visible.len() {
        let c = visible[i];
        let mut j = i + 1;
        while j < visible.len() && visible[j] == c {
            j += 1;
        }
        draws += 1;
        i = j;
    }
    draws
}

// ============================================================
// D1 — structural mesh-instance batch partition (pure, gateable)
// ============================================================

/// One GPU-cull draw batch: a maximal run of consecutive structural draws sharing
/// (mesh, index range), special kinds excluded. `first` is the run's start index in
/// the STRUCTURAL draw list — under GPU cull the instance buffer holds structural
/// order, so `first` is also the batch's `out_base` region in `visible_instances`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeshBatch {
    pub cid: core_math::scene_graph::MeshCacheId,
    pub irange: Option<(u32, u32)>,
    pub first: u32,
    pub count: u32,
}

/// Partition the STRUCTURAL draw list into GPU-cull batches. Unlike the CPU encode
/// merge (`raster_gbuffer` main pass) and `count_mesh_instance_draws`, this ignores
/// cull bits — visibility is decided per instance on the GPU, so a batch spans ALL
/// consecutive same-(cid, irange) mesh entries. Special kinds (voxel/SDF pool/resident)
/// keep their own draw paths and never enter a batch.
///
/// Returned batches are index-aligned with `draw_kind`/`draw_calls`; entries between
/// batches (special kinds) simply belong to none.
pub(crate) fn mesh_instance_batches(
    draw_calls: &[(
        core_math::scene_graph::MeshCacheId,
        render::mesh_gpu::InstanceData,
        u8,
        Option<(u32, u32)>,
    )],
    draw_kind: &[u8],
) -> Vec<MeshBatch> {
    debug_assert_eq!(draw_calls.len(), draw_kind.len());
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < draw_calls.len() {
        if draw_kind_is_special(draw_kind[i]) {
            i += 1;
            continue;
        }
        let (cid, _, _, irange) = draw_calls[i];
        let mut j = i + 1;
        while j < draw_calls.len()
            && !draw_kind_is_special(draw_kind[j])
            && draw_calls[j].0 == cid
            && draw_calls[j].3 == irange
        {
            j += 1;
        }
        out.push(MeshBatch { cid, irange, first: i as u32, count: (j - i) as u32 });
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        aabb_outside_frustum, count_mesh_instance_draws, frustum_planes,
        mesh_instance_batches, world_aabb, DRAW_KIND_MESH, DRAW_KIND_SDF_RESIDENT,
    };
    use glam::{Mat4, Vec3};

    /// D1 partition-agreement gate: `mesh_instance_batches` must produce the same batch
    /// count as `count_mesh_instance_draws` (unsorted, no skips) on any mesh-only list,
    /// with `first`/`count` tiling the list exactly; special kinds belong to no batch.
    #[test]
    fn d1_batch_partition_agrees_and_tiles() {
        use core_math::scene_graph::MeshCacheId;
        let inst = <render::mesh_gpu::InstanceData as bytemuck::Zeroable>::zeroed();
        // N=3 meshes × M=4 grouped instances + one special mid-list + an irange split.
        let mut calls = Vec::new();
        let mut kinds = Vec::new();
        for mesh in 0..3u64 {
            for _ in 0..4 {
                calls.push((MeshCacheId(mesh), inst, 0u8, None));
                kinds.push(DRAW_KIND_MESH);
            }
        }
        calls.insert(5, (MeshCacheId(99), inst, 0, None));
        kinds.insert(5, DRAW_KIND_SDF_RESIDENT);
        // Same mesh as its neighbours but a different index range → its own batch.
        calls.push((MeshCacheId(2), inst, 0, Some((0, 6))));
        kinds.push(DRAW_KIND_MESH);

        let batches = mesh_instance_batches(&calls, &kinds);
        // mesh0(run split by the special at idx 5 → 2 batches) + mesh1 + mesh2 + irange = 5.
        assert_eq!(batches.len(), 5, "{batches:?}");
        // Agreement with the CPU count on the same (special-stripped, unsorted) sequence.
        let cpu: Vec<(u64, bool)> = calls
            .iter()
            .zip(&kinds)
            .filter(|(_, &k)| k == DRAW_KIND_MESH)
            .map(|((cid, _, _, r), _)| {
                // Fold irange into the key the same way the batcher splits on it.
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (cid.0, r).hash(&mut h);
                (h.finish(), false)
            })
            .collect();
        // The special split mesh0 into two runs in `batches` but not in the flat CPU key
        // list — count that split back in.
        assert_eq!(count_mesh_instance_draws(&cpu, false) + 1, batches.len() as u32);
        // Tiling: batches cover every mesh index exactly once, in order.
        let mut covered = vec![false; calls.len()];
        for b in &batches {
            for k in b.first..(b.first + b.count) {
                assert!(!covered[k as usize], "index {k} covered twice");
                covered[k as usize] = true;
            }
        }
        for (i, &k) in kinds.iter().enumerate() {
            assert_eq!(covered[i], k == DRAW_KIND_MESH, "index {i}");
        }
    }

    /// Same construction as `CameraUniforms::from_orbit`: `perspective_rh` ([0,1] depth) × `look_at_rh`.
    fn view_proj() -> Mat4 {
        let eye = Vec3::new(0.0, 0.0, 5.0);
        let proj = Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        proj * view
    }

    #[test]
    fn frustum_keeps_visible_culls_offscreen_and_behind() {
        let planes = frustum_planes(&view_proj());
        let box_at = |c: Vec3, r: f32| (c - Vec3::splat(r), c + Vec3::splat(r));
        // In view at the origin → NOT outside.
        let (a, b) = box_at(Vec3::ZERO, 0.5);
        assert!(!aabb_outside_frustum(&planes, a, b), "origin box must be visible");
        // Far off to the +X side, outside the 60° FOV → culled (left/right planes).
        let (a, b) = box_at(Vec3::new(50.0, 0.0, 0.0), 0.5);
        assert!(aabb_outside_frustum(&planes, a, b), "far off-axis box must be culled");
        // Behind the camera (eye is at z=5 looking toward -Z; z=10 is behind) → culled by the near
        // plane. This is exactly what the [0,1]-depth near-plane fix (near = row2) guards.
        let (a, b) = box_at(Vec3::new(0.0, 0.0, 10.0), 0.5);
        assert!(aabb_outside_frustum(&planes, a, b), "box behind camera must be culled");
        // On-axis, just in front of the camera → visible (guards a WRONG near plane over-culling).
        let (a, b) = box_at(Vec3::new(0.0, 0.0, 4.0), 0.2);
        assert!(!aabb_outside_frustum(&planes, a, b), "on-axis box in front must stay visible");
    }

    #[test]
    fn world_aabb_transforms_all_corners() {
        let m = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let (mn, mx) = world_aabb(&m, Vec3::splat(-1.0), Vec3::splat(1.0));
        assert!((mn - Vec3::new(9.0, -1.0, -1.0)).length() < 1e-4, "min corner {mn:?}");
        assert!((mx - Vec3::new(11.0, 1.0, 1.0)).length() < 1e-4, "max corner {mx:?}");
    }

    // D2 note: the CPU OcclusionBuffer above is the KEEPER (86.8% cull @100k); the GPU Hi-Z module was deleted 2026-08-03.

    /// C.6 ★★★ — multi-mesh batching requires sort. Gate designed to FAIL without sort.
    #[test]
    fn c6_multi_mesh_sort_collapses_to_n_meshes() {
        let n_meshes = 5u64;
        let m_each = 20usize;
        // Interleaved arrival order (draw-stream / all_nodes order): m0,m1,m2,m3,m4,m0,...
        let mut entries = Vec::with_capacity(n_meshes as usize * m_each);
        for _ in 0..m_each {
            for m in 0..n_meshes {
                entries.push((m, false));
            }
        }
        let sorted = count_mesh_instance_draws(&entries, true);
        let unsorted = count_mesh_instance_draws(&entries, false);
        assert_eq!(
            sorted, n_meshes as u32,
            "C.6: with sort, N distinct meshes → N draws (got {sorted})"
        );
        assert_eq!(
            unsorted,
            (n_meshes as usize * m_each) as u32,
            "C.6 sabotage: without sort, interleaved → N×M draws (got {unsorted})"
        );
        assert!(
            unsorted > sorted,
            "C.6: gate must distinguish sorted vs unsorted"
        );
    }

    #[test]
    fn c6_single_mesh_best_case_hides_without_gate() {
        // Single-mesh stress: all cid=0 → consecutive even without sort.
        let entries: Vec<_> = (0..100).map(|_| (0u64, false)).collect();
        assert_eq!(count_mesh_instance_draws(&entries, true), 1);
        assert_eq!(
            count_mesh_instance_draws(&entries, false),
            1,
            "single-mesh stress cannot catch missing sort — that is why C.6 multi-mesh exists"
        );
    }

    #[test]
    fn scene_bvh_frustum_culls_offscreen_leaves() {
        use core_math::bvh::{build_scene_bvh, Aabb, SceneBvhBody};
        use super::scene_bvh_collect_frustum_culled;
        let planes = frustum_planes(&view_proj());
        // Body 0 at origin (visible), body 1 far off +X (culled).
        let bodies = vec![
            SceneBvhBody {
                aabb: Aabb {
                    min: [-0.5, -0.5, -0.5],
                    max: [0.5, 0.5, 0.5],
                },
                start_entry: 0,
                entry_count: 1,
            },
            SceneBvhBody {
                aabb: Aabb {
                    min: [49.5, -0.5, -0.5],
                    max: [50.5, 0.5, 0.5],
                },
                start_entry: 7,
                entry_count: 1,
            },
        ];
        let nodes = build_scene_bvh(&bodies);
        let culled = scene_bvh_collect_frustum_culled(&nodes, &planes);
        assert!(
            culled.contains(&7),
            "off-screen body start_entry=7 must be culled, got {culled:?}"
        );
        assert!(
            !culled.contains(&0),
            "origin body must stay, got {culled:?}"
        );
    }
}
