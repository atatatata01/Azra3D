//! DC-pipeline plumbing: the shared low-level building blocks over `sdf_mesh.wgsl`.
//!
//! `DcParamsGpu` POD + `dc_params`/`dc_params_sparse`/`dc_caps` helpers + `build_dc_pipelines`
//! + `dc_bind_group` + `dummy_storage` + `encode_dc_passes`. Widened priv â†’ `pub(crate)` so the
//! `readback`/`resident` siblings + the test module can construct/read/call them across the module
//! boundary (a visibility change, NOT a logic change â€” pure relocation).

use eframe::wgpu;
use glam::Vec3;
use std::sync::atomic::{AtomicBool, Ordering};

/// Mesher 2.0 feature-plane projection — **B2 toggle only**.
/// When false (default), `dc_params*` leave `gmin.w = 0` and GPU DC matches pre-M2 stock.
/// Set true only around B2 mesh dispatches so stock DC is never affected.
static DC_MESHER2_FEATURE: AtomicBool = AtomicBool::new(false);

/// Enable/disable Mesher 2.0 dual-vert feature projection for subsequent `dc_params*` builds.
#[inline]
pub(crate) fn set_dc_mesher2_feature(on: bool) {
    DC_MESHER2_FEATURE.store(on, Ordering::Relaxed);
}

#[inline]
pub(crate) fn dc_mesher2_feature() -> bool {
    DC_MESHER2_FEATURE.load(Ordering::Relaxed)
}

/// Uniform matching `DcParams` in `sdf_mesh.wgsl` (6 Ã— vec4 = 96 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DcParamsGpu {
    a0: [u32; 4],   // xyz = anchor cell min, w = grid resolution (points per axis)
    a1: [u32; 4],   // xyz = anchor cell max (inclusive), w = cell_vert base offset
    vmin: [u32; 4], // xyz = vertex region min
    vspan: [u32; 4],// xyz = vertex region size
    // xyz = grid_min; w = Mesher2 feature flag (1.0 = on). Iso is hardcoded 0 in WGSL.
    gmin: [f32; 4],
    // pub(crate): the `dispatch_sparse` test helper (mod tests) sets `gext[3]` (brick_res) across the
    // submodule boundary â€” widened from private to keep that test compiling (visibility-only change).
    pub(crate) gext: [f32; 4], // xyz = grid_max - grid_min (box extent), w = unused
}

/// Full-grid params for a `resÂ³` grid mapped into the world box `[grid_min, grid_max]`.
/// `cells = res - 1` per axis. Matches the CPU `chunked_dc` box mapping.
pub(crate) fn dc_params(res: u32, grid_min: Vec3, grid_max: Vec3) -> DcParamsGpu {
    let cells = res - 1;
    let ext = grid_max - grid_min;
    let m2 = if dc_mesher2_feature() { 1.0 } else { 0.0 };
    DcParamsGpu {
        a0: [0, 0, 0, res],
        a1: [cells - 1, cells - 1, cells - 1, 0], // last cell index = res-2
        vmin: [0, 0, 0, 0],
        vspan: [cells, cells, cells, 0],          // a1 - vmin + 1 = res-1 = cells
        gmin: [grid_min.x, grid_min.y, grid_min.z, m2],
        gext: [ext.x, ext.y, ext.z, 0.0],
    }
}

/// Sparse-path params: identical box mapping to `dc_params`, but packs `brick_res` (bricks per axis)
/// into `gext.w` so `sdf_mesh.wgsl`'s `fld()` switches to the 8Â³-brick decode (0 = dense). Proven
/// bit-exact vs dense by `sdf_dc_sparse_brick_decode_parity`.
pub(crate) fn dc_params_sparse(res: u32, grid_min: Vec3, grid_max: Vec3, brick_res: u32) -> DcParamsGpu {
    let mut p = dc_params(res, grid_min, grid_max);
    p.gext[3] = f32::from_bits(brick_res);
    p
}

/// WINDOWED sparse params: identical grid mapping to `dc_params_sparse`, but restricts the DC to
/// the INCLUSIVE cell box `[cmin, cmax]` (the sparse surface's brick bbox in cell coords). `a0/a1`
/// = the quad anchor range, `vmin = cminâˆ’1` for the DC's 1-cell vertex halo, `vspan` = the vertex
/// region size (`cell_vert` is region-local to `[vmin, a1]`, base 0 â€” single dispatch). The iso=0
/// surface lies ENTIRELY inside the handle-brick bbox (+ apron margin), so the emitted mesh is
/// BIT-IDENTICAL to the full-grid dispatch â€” cells outside the box hold no surface. Returns the
/// params plus the two dispatch extents: `vertex_ext` (dc_vertex over `[vmin, a1]` = `vspan`) and
/// `quad_ext` (dc_quads over `[a0, a1]`). `DcParamsGpu` fields are module-private, hence returned.
pub(crate) fn dc_params_sparse_windowed(
    res: u32, grid_min: Vec3, grid_max: Vec3, brick_res: u32,
    cmin: [u32; 3], cmax: [u32; 3],
) -> (DcParamsGpu, [u32; 3], [u32; 3]) {
    let cells = res - 1;
    let ext = grid_max - grid_min;
    let (mut a0, mut a1, mut vmin, mut vspan) = ([0u32; 3], [0u32; 3], [0u32; 3], [0u32; 3]);
    for a in 0..3 {
        a1[a] = cmax[a].min(cells.saturating_sub(1));
        a0[a] = cmin[a].min(a1[a]);
        vmin[a] = a0[a].saturating_sub(1); // 1-cell vertex halo, clamped â‰¥ 0
        vspan[a] = a1[a] - vmin[a] + 1;
    }
    let m2 = if dc_mesher2_feature() { 1.0 } else { 0.0 };
    let p = DcParamsGpu {
        a0: [a0[0], a0[1], a0[2], res],
        a1: [a1[0], a1[1], a1[2], 0], // a1.w = cell_vert base offset = 0 (single dispatch)
        vmin: [vmin[0], vmin[1], vmin[2], 0],
        vspan: [vspan[0], vspan[1], vspan[2], 0],
        gmin: [grid_min.x, grid_min.y, grid_min.z, m2],
        gext: [ext.x, ext.y, ext.z, f32::from_bits(brick_res)],
    };
    let quad_ext = [a1[0] - a0[0] + 1, a1[1] - a0[1] + 1, a1[2] - a0[2] + 1];
    (p, vspan, quad_ext)
}

/// Conservative buffer caps for a `resÂ³` grid: â‰¤1 vertex per cell, generous index headroom.
pub(crate) fn dc_caps(res: u32) -> (u64, u64, u64) {
    let cells = (res - 1) as u64;
    let cells3 = (cells * cells * cells).max(1);
    (cells3, cells3 * 8, (cells3 * 6).max(6)) // (cell_vert/vert cap, vert floats, idx cap)
}

/// Align storage buffer sizes to 256 B (wgpu / Vulkan requirement for many bind paths).
#[inline]
pub(crate) fn dc_align_buffer_bytes(bytes: u64) -> u64 {
    ((bytes.max(4) + 255) / 256) * 256
}

/// Max f32 elements that fit in a single storage binding (bind limit, not total VRAM).
#[inline]
pub(crate) fn dc_max_bind_f32_elems(max_storage_binding: u64) -> u64 {
    max_storage_binding / 4
}

/// Whether `bytes` (after 256-align) can be bound as one storage buffer.
#[inline]
pub(crate) fn dc_bytes_bindable(bytes: u64, max_storage_binding: u64) -> bool {
    dc_align_buffer_bytes(bytes) <= max_storage_binding
}

/// Dense DC is structurally illegal above GridBacked max (512): `cell_vert â‰ˆ (resâˆ’1)Â³Ã—4`
/// exceeds the ~2â€¯GiB storage-binding limit at 1024Â³ (the logged `BufferRangeTooLarge`
/// fingerprint 4282396672). Call before any dense mesh arm.
#[inline]
pub(crate) fn dc_dense_res_legal(res: u32) -> bool {
    res > 0 && res <= crate::gpu_jobs::max_gridbacked_resolution()
}

/// Dense `cell_vert` element count for a dispatch resolution, or `None` if the size
/// cannot be bound (illegal res or over bind limit).
pub(crate) fn dc_cell_vert_cap_dense(
    dispatch_res: u32,
    max_storage_binding: u64,
) -> Option<u64> {
    if !dc_dense_res_legal(dispatch_res) {
        return None;
    }
    let (cv, _, _) = dc_caps(dispatch_res);
    if !dc_bytes_bindable(cv * 4, max_storage_binding) {
        return None;
    }
    Some(cv)
}

/// Sparse brick-local `cell_vert` element count: `allocated_bricks Ã— 512`.
/// Returns `None` if the request cannot be bound as one storage buffer.
pub(crate) fn dc_cell_vert_cap_sparse(
    allocated_bricks: u64,
    max_storage_binding: u64,
) -> Option<u64> {
    let cv = (allocated_bricks.max(1) * 512).max(1);
    if !dc_bytes_bindable(cv * 4, max_storage_binding) {
        return None;
    }
    Some(cv)
}

/// R3b â€” DENSE output caps in ELEMENT counts: (vertex count cap, u32 index cap), i.e. the
/// volume-proportional `dc_caps` bound clamped to the driver-safe ceiling (â‰¤1GiB AND â‰¤half the
/// storage binding limit â€” the POOL_MAX_BUFFER_BYTES lesson: a single multi-GB allocation fails
/// on real drivers even with VRAM free). This is the exact sizing `SdfBodyMeshSlot::new` used to
/// inline; extracted so the dense dispatch arms and the sparse grow ceiling share ONE definition.
pub(crate) fn dc_output_caps_dense(max_storage_binding: u64, res: u32) -> (u64, u64) {
    // Illegal dense res: return zero caps so callers cannot size multi-GB outputs.
    if !dc_dense_res_legal(res) {
        return (0, 0);
    }
    let (_cv, vert_floats, idx_cap) = dc_caps(res);
    let out_cap_units = ((max_storage_binding / 2).min(1 << 30)) / 4; // floats(verts) / u32s(idx) under â‰¤1GiB
    (vert_floats.min(out_cap_units) / 8, idx_cap.min(out_cap_units))
}

/// R3b â€” the sparse-path vertex headroom per ALLOCATED surface brick (a brick = 8Â³ voxels; the
/// hard per-brick bound is 8Â³ = 512 verts, one per cell). MEASURED, not guessed
/// (`sdf_dc_sparse_verts_per_brick_measurement`, 2026-07-09):
///   sphereâˆ’subtract  @256: 20.5 v/brick   @512: 20.3 v/brick   (idx/vert = 6.00)
///   mirrored twins   @256: 21.4 v/brick   @512: 17.1 v/brick   (idx/vert = 6.00)
///   elongated bar    @256: 19.4 v/brick   @512: 22.6 v/brick   (idx/vert = 6.00)
/// Max measured = 22.6 verts/brick â†’ factor 48 â‰ˆ 2.1Ã— headroom. This is a STARTING size, not a
/// correctness bound: an outlier surface overflows the atomicMin-clamped counters, is detected by
/// the counter readback, and the slot GROWS (Ã—1.5, generation-bumped) + re-meshes.
pub(crate) const VERTS_PER_BRICK_HEADROOM: u64 = 48;

/// Per-TILE vertex headroom (C2 follow-up, 2026-07-16). The C1 envelope finding: sizing tiles at
/// the monolithic 48 v/brick packs Î£ per-tile caps past the 1 GiB pool ceiling on realistic 1024
/// surfaces (measured scene-A@1024 = 41.6M vert caps â‰ˆ 1.33 GB) â†’ the tiled path fell back to
/// monolithic exactly where C2 matters most. Tiles can run TIGHTER than the monolithic body:
/// measured density is ~20-22.6 v/brick, and with C2 a per-tile overflow costs only a ~1ms
/// single-tile grow+re-mesh (the proven floor machinery) instead of a whole-body re-mesh â€” the
/// safety net that justified 48 monolithically is two orders of magnitude cheaper per tile.
/// 28 = measured max + ~24% margin; scene-A@1024 then packs ~24.3M vert caps (777 MB) â€” fits.
pub(crate) const TILE_VERTS_PER_BRICK_HEADROOM: u64 = 28;

/// R3b â€” surface-proportional output caps for a brick-resident (sparse) body: element counts
/// (vertex cap, u32 index cap) from the atlas's allocated surface-brick count. Index ratio is the
/// same 6 idx per vertex-cap unit as the dense `dc_caps` bound (DC quads = 6 indices; measured
/// idx/vert â‰ˆ 5.9â€“6.0). Callers clamp to the dense ceiling (`dc_output_caps_dense`) â€” sparse must
/// never exceed what the dense worst case would allocate.
pub(crate) fn dc_output_caps_sparse(allocated_bricks: u64, verts_per_brick: u64) -> (u64, u64) {
    let vert_cap = (allocated_bricks.max(1) * verts_per_brick).max(64);
    (vert_cap, vert_cap * 6)
}

// â”€â”€ Tiled DC (C1) plumbing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Cells per tile axis (= 4 atlas bricks â€” `graphics::sdf_brick_atlas::SDF_TILE_BRICKS Ã— 8`).
pub(crate) const SDF_TILE_CELLS: u32 = 32;
/// Vertex-region span per tile axis (tile + 1-cell halo) â€” mirrors `TILE_VSPAN` in sdf_mesh.wgsl.
pub(crate) const SDF_TILE_VSPAN: u32 = SDF_TILE_CELLS + 1;
/// Hard per-tile vertex bound: one DC vertex per vertex-region cell (33Â³). Per-tile caps are
/// clamped here â€” a cap at this bound can NEVER overflow (the counter counts region cells).
pub(crate) const SDF_TILE_MAX_VERTS: u64 =
    (SDF_TILE_VSPAN as u64) * (SDF_TILE_VSPAN as u64) * (SDF_TILE_VSPAN as u64);
/// cell_vert scratch slots per batch tile â€” mirrors `TILE_CV_STRIDE` in sdf_mesh.wgsl.
pub(crate) const SDF_TILE_CV_STRIDE: u64 = SDF_TILE_MAX_VERTS;
/// Max tiles per batch dispatch: bounds the shared scratch at 1024 Ã— 33Â³ Ã— 4B â‰ˆ 147 MB and the
/// per-pass workgroup count (729 Ã— 1024 â‰ˆ 750k) â€” a body with more live tiles runs âŒˆn/1024âŒ‰
/// batches inside ONE encoder (pass boundaries order the scratch reuse).
pub(crate) const SDF_TILE_BATCH_MAX: u32 = 1024;

/// CPU mirror of `TileInfo` in sdf_mesh.wgsl (4 Ã— vec4<u32> = 64 B).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TileInfoGpu {
    /// xyz = quad anchor min (cells); w = vert base (absolute VERTICES in the pool verts buffer).
    pub(crate) amin: [u32; 4],
    /// xyz = quad anchor max (inclusive); w = vert cap (vertices).
    pub(crate) amax: [u32; 4],
    /// xyz = vertex region min (= max(aminâˆ’1, 0)); w = idx base (absolute u32s in the pool idx).
    pub(crate) vmn: [u32; 4],
    /// x = idx cap (u32s); y = first_instance for the args entry; z,w reserved (C2).
    pub(crate) tmeta: [u32; 4],
}

/// O2 LOD: WINDOWED sparse params at octree `level` â€” cells span `1<<level` voxels, meshed from
/// the SAME brick atlas (no re-bake, no re-classify). The whole body sits at ONE level, so no
/// cracks are possible (the O2 design premise; mixed levels are stage O3). Level 0 over the full
/// grid == the monolithic `dc_params_sparse` full grid, bit-identically (every stride expression
/// reduces).
///
/// `cmin`/`cmax` is the anchor window in LEVEL-0 cells (voxel coordinates) â€” the same window
/// `mesh_into_slot_sparse` derives from `atlas.surface_brick_{min,max}`. Converted to LEVEL cells
/// here with a 1-cell coarsening guard on the low side: a level cell whose corner-0 voxel sits
/// just BEFORE `cmin` can still own a corner voxel inside the window (corners are `1<<level`
/// voxels apart), so the level window is `(cmin>>level)âˆ’1 ..= (cmax+1)>>level`, clamped to the
/// level grid. Cells fully outside the surface-brick bbox read only sentinel bricks at every
/// corner and emit nothing (classify never places EXTERIOR next to INTERIOR without surface
/// bricks between; corner spacing at level â‰¤ 3 is â‰¤ the 8-voxel brick span, so a cell cannot jump
/// the handle layer) â€” windowed output is IDENTICAL to the O2a full-grid sweep. Gate:
/// `sdf_lod_windowed_matches_full_grid`. Returns (params, vertex-pass ext, quad-pass ext) in
/// LEVEL cells.
pub(crate) fn dc_params_sparse_lod(
    res: u32, grid_min: Vec3, grid_max: Vec3, brick_res: u32, level: u32,
    cmin: [u32; 3], cmax: [u32; 3],
) -> (DcParamsGpu, [u32; 3], [u32; 3]) {
    assert!(brick_res > 0, "LOD meshing is sparse-only (dense paths stay level 0)");
    let cells_l = ((res - 1) >> level).max(1);
    let ext = grid_max - grid_min;
    let (mut a0, mut a1, mut vmin, mut vspan) = ([0u32; 3], [0u32; 3], [0u32; 3], [0u32; 3]);
    for a in 0..3 {
        a1[a] = ((cmax[a] + 1) >> level).min(cells_l - 1);
        a0[a] = (cmin[a] >> level).saturating_sub(1).min(a1[a]);
        vmin[a] = a0[a].saturating_sub(1); // 1-cell vertex halo, clamped â‰¥ 0
        vspan[a] = a1[a] - vmin[a] + 1;
    }
    let m2 = if dc_mesher2_feature() { 1.0 } else { 0.0 };
    let p = DcParamsGpu {
        a0: [a0[0], a0[1], a0[2], res],
        a1: [a1[0], a1[1], a1[2], 0], // a1.w = cell_vert base offset = 0 (single dispatch)
        vmin: [vmin[0], vmin[1], vmin[2], level], // vmin.w = LOD level (WGSL stride())
        vspan: [vspan[0], vspan[1], vspan[2], 0],
        gmin: [grid_min.x, grid_min.y, grid_min.z, m2],
        gext: [ext.x, ext.y, ext.z, f32::from_bits(brick_res)],
    };
    let quad_ext = [a1[0] - a0[0] + 1, a1[1] - a0[1] + 1, a1[2] - a0[2] + 1];
    (p, vspan, quad_ext)
}

/// Full-grid LEVEL-0-cell window for `dc_params_sparse_lod` â€” the O2a full-grid sweep, used by
/// the windowed-vs-full parity gate and as the fallback when a caller has no surface bbox.
pub(crate) fn lod_full_window(res: u32) -> ([u32; 3], [u32; 3]) {
    let c = (res - 1).max(1) - 1;
    ([0; 3], [c; 3])
}

/// Per-batch params for the tiled entry points: monolithic grid mapping (res/gmin/gext +
/// brick_res) with `a1.w` = first tile ENTRY of the batch and `vspan.w` = end entry (exclusive).
/// The window fields (a0/a1.xyz/vmin/vspan.xyz) are unused by the tiled entries â€” each tile
/// carries its own window in `TileInfo`.
pub(crate) fn dc_params_tiled(
    res: u32, grid_min: Vec3, grid_max: Vec3, brick_res: u32,
    entry_base: u32, entry_end: u32,
) -> DcParamsGpu {
    let mut p = dc_params_sparse(res, grid_min, grid_max, brick_res);
    p.a1[3] = entry_base;
    p.vspan[3] = entry_end;
    p
}

/// The 9-binding BGL (monolithic 0-7 + `tiles` at 8) + the three tiled pipelines
/// (dc_vertex_tiled, dc_quads_tiled, dc_tile_args) from the SAME sdf_mesh.wgsl module. A separate
/// layout keeps the proven 8-binding monolithic/dense pipelines byte-identical (their entries
/// never statically reference `tiles`, so their layout stays valid unchanged).
pub(crate) fn build_dc_tiled_pipelines(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>)
    -> (wgpu::BindGroupLayout, wgpu::ComputePipeline, wgpu::ComputePipeline, wgpu::ComputePipeline)
{
    let st = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding, visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sdf_dc_tiled_bgl"),
        entries: &[
            st(0, true),  // field (unused on this path â€” dummy; fld() statically references it)
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            st(2, false), // cell_vert (the pool's batch scratch)
            st(3, false), // verts (WHOLE pool buffer â€” per-tile caps live in TileInfo)
            st(4, false), // indices (pool idx; for dc_tile_args: the pool INDIRECT args as u32s)
            st(5, false), // counters (per-tile atomic pairs, indexed by args entry)
            st(6, true),  // brick_index
            st(7, true),  // brick_pool
            st(8, true),  // tiles (TileInfo array, indexed by args entry)
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sdf_mesh.wgsl (tiled)"), source: wgpu::ShaderSource::Wgsl(graphics::SDF_MESH_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sdf_dc_tiled_pl"), bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let mk = |entry: &'static str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry), layout: Some(&layout), module: &shader,
        entry_point: Some(entry), compilation_options: Default::default(), cache });
    (bgl, mk("dc_vertex_tiled"), mk("dc_quads_tiled"), mk("dc_tile_args"))
}

/// Build the BGL + the two compute pipelines from `sdf_mesh.wgsl`. Shared by both layers.
pub(crate) fn build_dc_pipelines(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>)
    -> (wgpu::BindGroupLayout, wgpu::ComputePipeline, wgpu::ComputePipeline)
{
    let st = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding, visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sdf_dc_bgl"),
        entries: &[
            st(0, true),  // field (signed distance)
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            st(2, false), // cell_vert
            st(3, false), // verts
            st(4, false), // indices
            st(5, false), // counters (atomic)
            st(6, true),  // brick_index (sparse atlas tags; dummy on dense path)
            st(7, true),  // brick_pool (sparse atlas arena; dummy on dense path)
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sdf_mesh.wgsl"), source: wgpu::ShaderSource::Wgsl(graphics::SDF_MESH_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sdf_dc_pl"), bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let mk = |entry: &'static str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry), layout: Some(&layout), module: &shader,
        entry_point: Some(entry), compilation_options: Default::default(), cache });
    (bgl, mk("dc_vertex"), mk("dc_quads"))
}

/// Bind group over the 8 buffers in `sdf_mesh.wgsl` binding order. `brick_index`/`brick_pool` are the
/// sparse SDF atlas (Phase 3); on the dense path bind 1-elem dummies (the shader skips them when the
/// params' `brick_res` is 0). `verts`/`idx` are `BindingResource`s (stage B pool): per-body slots pass
/// `buf.as_entire_binding()`, the shared `SdfMeshPool` passes 256B-aligned SUB-RANGES â€” the shader's
/// slot-local writes + `arrayLength` guards work unchanged over a sub-range (the voxel-pool precedent).
pub(crate) fn dc_bind_group(
    device: &wgpu::Device, bgl: &wgpu::BindGroupLayout,
    field: &wgpu::Buffer, params: &wgpu::Buffer, cell_vert: &wgpu::Buffer,
    verts: wgpu::BindingResource, idx: wgpu::BindingResource, counters: &wgpu::Buffer,
    brick_index: &wgpu::Buffer, brick_pool: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sdf_dc_bg"), layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: field.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: cell_vert.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: verts },
            wgpu::BindGroupEntry { binding: 4, resource: idx },
            wgpu::BindGroupEntry { binding: 5, resource: counters.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: brick_index.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: brick_pool.as_entire_binding() },
        ],
    })
}

/// A tiny (1-elem) dummy storage buffer for the unused sparse-atlas bindings on the dense DC path.
pub(crate) fn dummy_storage(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size: 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
}

/// Encode the two passes (vertex then quads). The implicit writeâ†’read barrier between the separate
/// compute passes guarantees every cell_vert is written before any quad reads it.
pub(crate) fn encode_dc_passes(
    enc: &mut wgpu::CommandEncoder, pipe_vertex: &wgpu::ComputePipeline, pipe_quads: &wgpu::ComputePipeline,
    bg: &wgpu::BindGroup, cells: u32,
) {
    // Full-grid: vertex + quads both cover the whole [0, cells) box.
    encode_dc_passes_ext(enc, pipe_vertex, pipe_quads, bg, [cells; 3], [cells; 3]);
}

/// Windowed twin: the vertex pass covers `vertex_ext` cells (the `[vmin, a1]` region) and the quads
/// pass covers `quad_ext` cells (`[a0, a1]`), instead of the full grid. The params already carry the
/// `a0/a1/vmin/vspan` sub-window, so the shader clips exactly to it; this only sizes the dispatch.
/// The implicit writeâ†’read barrier between the two separate passes is unchanged.
pub(crate) fn encode_dc_passes_ext(
    enc: &mut wgpu::CommandEncoder, pipe_vertex: &wgpu::ComputePipeline, pipe_quads: &wgpu::ComputePipeline,
    bg: &wgpu::BindGroup, vertex_ext: [u32; 3], quad_ext: [u32; 3],
) {
    encode_dc_passes_timed(enc, pipe_vertex, pipe_quads, bg, vertex_ext, quad_ext, None);
}

/// Timed twin: when `ts` is Some, the vertex pass writes timestamp queries 0/1 and the quads pass
/// 2/3 of the given query set (GPU-side per-pass timing â€” the R2 prereq diagnostic; the CPU-side
/// `[DcResidentSparse]` split cannot see GPU duration because submits are async). SAFETY: the
/// caller MUST `resolve_query_set(0..4)` in this SAME encoder AFTER this call â€” both pairs are then
/// always written before any resolve, so the unwritten-query device-loss class (CLAUDE.md GPU
/// Timestamp Query Safety) is impossible by construction, not by discipline.
pub(crate) fn encode_dc_passes_timed(
    enc: &mut wgpu::CommandEncoder, pipe_vertex: &wgpu::ComputePipeline, pipe_quads: &wgpu::ComputePipeline,
    bg: &wgpu::BindGroup, vertex_ext: [u32; 3], quad_ext: [u32; 3], ts: Option<&wgpu::QuerySet>,
) {
    let wg = |n: u32| (n + 3) / 4; // workgroup_size = 4,4,4
    let pass_ts = |base: u32| ts.map(|qs| wgpu::ComputePassTimestampWrites {
        query_set: qs,
        beginning_of_pass_write_index: Some(base),
        end_of_pass_write_index: Some(base + 1),
    });
    {
        let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("sdf_dc_vertex"), timestamp_writes: pass_ts(0) });
        p.set_pipeline(pipe_vertex); p.set_bind_group(0, bg, &[]);
        p.dispatch_workgroups(wg(vertex_ext[0]), wg(vertex_ext[1]), wg(vertex_ext[2]));
    }
    {
        let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("sdf_dc_quads"), timestamp_writes: pass_ts(2) });
        p.set_pipeline(pipe_quads); p.set_bind_group(0, bg, &[]);
        p.dispatch_workgroups(wg(quad_ext[0]), wg(quad_ext[1]), wg(quad_ext[2]));
    }
}

#[cfg(test)]
mod cell_vert_bind_limit_tests {
    use super::*;

    /// RTX 4090 / Vulkan-class storage bind ceiling seen in production logs.
    const BIND_2GIB: u64 = 2_147_483_648;

    #[test]
    fn dense_512_cell_vert_bindable_under_2gib() {
        let cv = dc_cell_vert_cap_dense(512, BIND_2GIB).expect("512 dense must bind");
        assert!(dc_bytes_bindable(cv * 4, BIND_2GIB));
        // ~497 MB — well under 2 GiB
        assert!(cv * 4 < BIND_2GIB / 2);
    }

    #[test]
    fn dense_256_cell_vert_bindable() {
        assert!(dc_cell_vert_cap_dense(256, BIND_2GIB).is_some());
    }

    #[test]
    fn dense_1024_cell_vert_rejected() {
        // Fingerprint: (1023)^3 * 4 = 4282396668 > 2 GiB
        assert!(!dc_dense_res_legal(1024));
        assert!(dc_cell_vert_cap_dense(1024, BIND_2GIB).is_none());
        let (cv, _, _) = dc_caps(1024);
        assert_eq!(cv * 4, 4_282_396_668);
        assert!(!dc_bytes_bindable(cv * 4, BIND_2GIB));
    }

    #[test]
    fn dense_2048_rejected() {
        assert!(dc_cell_vert_cap_dense(2048, BIND_2GIB).is_none());
    }

    #[test]
    fn dense_output_caps_zero_for_illegal_res() {
        let (v, i) = dc_output_caps_dense(BIND_2GIB, 1024);
        assert_eq!((v, i), (0, 0));
    }

    #[test]
    fn sparse_surface_bricks_bindable() {
        // 100k surface bricks * 512 * 4 = ~200 MB
        let cv = dc_cell_vert_cap_sparse(100_000, BIND_2GIB).expect("surface sparse must bind");
        assert_eq!(cv, 100_000 * 512);
    }

    #[test]
    fn sparse_full_grid_1024_bricks_rejected() {
        // Full brick index @1024 = 128^3 = 2_097_152 ? ~4 GiB cell_vert
        let full = 128u64 * 128 * 128;
        assert!(dc_cell_vert_cap_sparse(full, BIND_2GIB).is_none());
    }

    #[test]
    fn sparse_max_bindable_bricks_under_2gib() {
        // 2 GiB / (512 * 4) = 1_048_576 bricks
        let max_bricks = BIND_2GIB / (512 * 4);
        assert!(dc_cell_vert_cap_sparse(max_bricks, BIND_2GIB).is_some());
        assert!(dc_cell_vert_cap_sparse(max_bricks + 1, BIND_2GIB).is_none());
    }
}
