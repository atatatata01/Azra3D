//! Readback layer: the blocking + async staging-map paths.
//!
//! `dispatch_to_staging` (async kick), `PendingSdfReadback` (the non-blocking staging-map state the
//! live drag uses to avoid the `poll(Wait)` freeze), `dispatch_readback` (blocking test/G1 wrapper),
//! and the `mesh_sdf_grid_gpu` free-fn harness. Depends on the DC plumbing via `super::*`.
//! `PendingSdfReadback` is re-exported by the parent `mod` so `crate::sdf_gpu_mesh::PendingSdfReadback`
//! (cache_state.rs:110) keeps resolving.

use eframe::wgpu;
use glam::Vec3;

use super::*;

/// G0 readback harness: GPU-mesh a dense `res³` signed-distance grid, returning `(verts, indices)`.
/// Blocking (one submit + poll Wait). G1 uses the resident `SdfMeshGpu` instead.
/// Build buffers, dispatch the two passes, and read back `(verts, indices)`. Shared by the free-fn
/// harness (fresh pipelines per call) and `SdfMeshGpu::mesh_readback` (cached pipelines). Blocking.
pub(crate) fn dispatch_to_staging(
    device: &wgpu::Device, queue: &wgpu::Queue, bgl: &wgpu::BindGroupLayout,
    pv: &wgpu::ComputePipeline, pq: &wgpu::ComputePipeline,
    grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3,
) -> PendingSdfReadback {
    assert_eq!(grid.len() as u32, res * res * res, "grid must be cubic res³");
    let (cv_cap, vert_floats, idx_cap) = dc_caps(res);

    let mk = |bytes: u64, label: &str| device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false });
    let field = mk((grid.len() * 4) as u64, "sdf_dc_field");
    queue.write_buffer(&field, 0, bytemuck::cast_slice(grid));
    let params = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sdf_dc_params"),
        size: std::mem::size_of::<DcParamsGpu>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&params, 0, bytemuck::bytes_of(&dc_params(res, grid_min, grid_max)));
    let cell_vert = mk(cv_cap * 4, "sdf_dc_cell_vert");
    let verts = mk(vert_floats * 4, "sdf_dc_verts");
    let idx = mk(idx_cap * 4, "sdf_dc_indices");
    let counters = mk(8, "sdf_dc_counters");
    queue.write_buffer(&counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));
    let d_index = dummy_storage(device, "sdf_dc_dummy_index");
    let d_pool = dummy_storage(device, "sdf_dc_dummy_pool");

    let bg = dc_bind_group(device, bgl, &field, &params, &cell_vert, verts.as_entire_binding(), idx.as_entire_binding(), &counters, &d_index, &d_pool);

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sdf_dc_enc") });
    encode_dc_passes(&mut enc, pv, pq, &bg, res - 1);
    let s_counters = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_counters"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_verts = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_verts"), size: vert_floats * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_idx = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_idx"), size: idx_cap * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    enc.copy_buffer_to_buffer(&counters, 0, &s_counters, 0, 8);
    enc.copy_buffer_to_buffer(&verts, 0, &s_verts, 0, vert_floats * 4);
    enc.copy_buffer_to_buffer(&idx, 0, &s_idx, 0, idx_cap * 4);
    queue.submit(std::iter::once(enc.finish()));

    // Async readback: map the staging buffers and return immediately. The map callbacks fire over the
    // next 1-2 frames (driven by the frame loop's device.poll(Poll)); NO blocking poll(Wait) here.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    for s in [&s_counters, &s_verts, &s_idx] {
        let r = ready.clone();
        s.slice(..).map_async(wgpu::MapMode::Read, move |res| {
            if res.is_ok() { r.fetch_add(1, std::sync::atomic::Ordering::Release); }
        });
    }
    PendingSdfReadback { s_verts, s_idx, s_counters, ready, vert_cap: cv_cap, idx_cap }
}

/// PT snapshot readback (consolidation fix): copy an UNPOOLED slot's counters + verts + idx into
/// fresh MAP_READ stagings and map them asynchronously - the same 3-buffer/AtomicU32 contract as
/// `dispatch_to_staging`. The caller owns the (temporary) slot; the copies are bounded by the
/// slot's surface-proportional caps. POLL-PHASE ONLY (Frame Ordering Contract).
pub(crate) fn readback_slot(
    device: &wgpu::Device, queue: &wgpu::Queue, slot: &super::SdfBodyMeshSlot,
) -> PendingSdfReadback {
    let vert_floats = (slot.vert_cap * 8).max(1);
    let idx_cap = slot.idx_cap.max(1);
    let s_counters = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pt_snap_counters"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_verts = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pt_snap_verts"), size: (vert_floats * 4).max(4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_idx = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pt_snap_idx"), size: (idx_cap * 4).max(4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pt_snap_enc") });
    enc.copy_buffer_to_buffer(&slot.counters, 0, &s_counters, 0, 8);
    enc.copy_buffer_to_buffer(&slot.verts, 0, &s_verts, 0, (vert_floats * 4).min(slot.verts.size()));
    enc.copy_buffer_to_buffer(&slot.idx, 0, &s_idx, 0, (idx_cap * 4).min(slot.idx.size()));
    queue.submit(std::iter::once(enc.finish()));
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    for st in [&s_counters, &s_verts, &s_idx] {
        let r = ready.clone();
        st.slice(..).map_async(wgpu::MapMode::Read, move |res| {
            if res.is_ok() { r.fetch_add(1, std::sync::atomic::Ordering::Release); }
        });
    }
    PendingSdfReadback { s_verts, s_idx, s_counters, ready, vert_cap: slot.vert_cap, idx_cap }
}

/// An in-flight async SDF mesh readback. The 3 staging buffers map over 1-2 frames; `ready()` is true
/// once all three callbacks fired, then `take()` reads them into `(verts, indices)`. This is what lets
/// the live GPU-mesh path avoid the blocking `device.poll(Wait)` stall (the per-frame freeze).
pub(crate) struct PendingSdfReadback {
    s_verts: wgpu::Buffer,
    s_idx: wgpu::Buffer,
    s_counters: wgpu::Buffer,
    ready: std::sync::Arc<std::sync::atomic::AtomicU32>,
    vert_cap: u64,
    idx_cap: u64,
}

#[allow(dead_code)] // mesh_sdf_grid_gpu (tests) uses the blocking wrapper; the live path uses ready()/take().
impl PendingSdfReadback {
    pub(crate) fn ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire) >= 3
    }
    /// Read the mapped staging into `(verts, indices)`. Call only when `ready()`.
    ///
    /// Copies only the LIVE prefix (`counters`-reported vert/idx counts), never the
    /// staging CAPACITY: the old full-capacity `to_vec()` + per-vertex rebuild copied
    /// up to hundreds of MB on the frame thread and was the bulk of the ~200ms
    /// [SLOWPOLL] landing stall on a 1024³ snapshot. Each array is now ONE bounded
    /// memcpy via a bytemuck cast ([f32; 8] is Pod, align 4 — same layout as 8 f32s).
    pub(crate) fn take(self) -> (Vec<[f32; 8]>, Vec<u32>) {
        let cnt: [u32; 2] = {
            let m = self.s_counters.slice(..).get_mapped_range();
            let c: &[u32] = bytemuck::cast_slice(&m);
            [c[0], c[1]]
        };
        let vcount = (cnt[0] as u64).min(self.vert_cap) as usize;
        let icount = (cnt[1] as u64).min(self.idx_cap) as usize;
        let verts: Vec<[f32; 8]> = {
            let m = self.s_verts.slice(..).get_mapped_range();
            let all: &[f32] = bytemuck::cast_slice(&m);
            let take_f = (vcount * 8).min(all.len() - all.len() % 8);
            bytemuck::cast_slice::<f32, [f32; 8]>(&all[..take_f]).to_vec()
        };
        let indices: Vec<u32> = {
            let m = self.s_idx.slice(..).get_mapped_range();
            let all: &[u32] = bytemuck::cast_slice(&m);
            all[..icount.min(all.len())].to_vec()
        };
        self.s_counters.unmap(); self.s_verts.unmap(); self.s_idx.unmap();
        (verts, indices)
    }
}

/// Blocking variant (tests + G1 mesh_readback): dispatch + wait + read.
pub(crate) fn dispatch_readback(
    device: &wgpu::Device, queue: &wgpu::Queue, bgl: &wgpu::BindGroupLayout,
    pv: &wgpu::ComputePipeline, pq: &wgpu::ComputePipeline,
    grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3,
) -> (Vec<[f32; 8]>, Vec<u32>) {
    let p = dispatch_to_staging(device, queue, bgl, pv, pq, grid, res, grid_min, grid_max);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    p.take()
}

/// G0 readback harness: GPU-mesh a dense `res³` signed-distance grid in box `[grid_min, grid_max]`,
/// returning `(verts, indices)`. Compiles fresh pipelines per call — test-only; the live path uses
/// the cached `SdfMeshGpu::dispatch_async`.
#[allow(dead_code)]
pub(crate) fn mesh_sdf_grid_gpu(
    device: &wgpu::Device, queue: &wgpu::Queue, grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3,
) -> (Vec<[f32; 8]>, Vec<u32>) {
    let (bgl, pv, pq) = build_dc_pipelines(device, None);
    dispatch_readback(device, queue, &bgl, &pv, &pq, grid, res, grid_min, grid_max)
}
