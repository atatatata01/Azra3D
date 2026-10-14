//! Slice 6 (app-crate de-god): SDF DC parity tests, relocated verbatim from `sdf_gpu_mesh/mod.rs`.
//!
//! Pure test-code move — no logic change. `headless_device` lives here and is shared with
//! `tests_bake.rs` (Slice 7) via `use super::tests_dc::headless_device;`, so it is `pub(super)`.
#![cfg(test)]

use eframe::wgpu;
use glam::Vec3;
// The DC plumbing / readback / resident items the tests exercise are re-exported at the
// `sdf_gpu_mesh` module level (glob re-exports in `mod.rs`); pull them in here.
use crate::sdf_gpu_mesh::*;

fn sd_round_box(p: Vec3, b: Vec3, r: f32) -> f32 {
    let q = p.abs() - b;
    q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0) - r
}
fn sd_sphere(p: Vec3, r: f32) -> f32 { p.length() - r }

fn bake_grid<F: Fn(Vec3) -> f32>(res: u32, grid_min: Vec3, grid_max: Vec3, f: F) -> Vec<f32> {
    let ext = grid_max - grid_min;
    let mut g = vec![0.0f32; (res * res * res) as usize];
    let s = (res - 1) as f32;
    for z in 0..res { for y in 0..res { for x in 0..res {
        let p = grid_min + (Vec3::new(x as f32, y as f32, z as f32) / s) * ext;
        g[(x + y * res + z * res * res) as usize] = f(p);
    }}}
    g
}

fn cpu_mesh(grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3) -> core_math::mesh::Mesh {
    // chunked_dc's grid_resolution counts CELLS (nverts = res+1 grid points); our `res` is grid
    // POINTS, so cells = res - 1 — matching the GPU shader.
    let mut cm = core_math::chunked_dc::ChunkedMesh::new(res - 1, 32, grid_min, grid_max, true);
    cm.extract_all(grid);
    cm.to_mesh()
}

/// Soft-fail headless GPU. Holds the process-wide [`crate::gpu_test_harness::GpuTestLock`] until
/// the returned lock is dropped — bind it to a local for the whole test body.
///
/// `let (_gpu_lock, device, queue) = headless_device()?;`
pub(super) fn headless_device()
-> Option<(crate::gpu_test_harness::GpuTestLock, wgpu::Device, wgpu::Queue)> {
    crate::gpu_test_harness::headless_device()
}

/// Mean + max nearest-neighbour distance (in voxels) from each `a` vertex to the `b` set. O(n·m).
pub(super) fn parity(a: &[Vec3], b: &[Vec3], voxel: f32) -> (f32, f32) {
    let mut sum = 0.0f32; let mut mx = 0.0f32;
    for &p in a {
        let mut best = f32::MAX;
        for &q in b { best = best.min(p.distance_squared(q)); }
        let d = best.sqrt() / voxel;
        sum += d; mx = mx.max(d);
    }
    (sum / a.len().max(1) as f32, mx)
}

fn run_parity(name: &str, grid: &[f32], res: u32, grid_min: Vec3, grid_max: Vec3) {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping {name}"); return; }
    };
    let voxel = ((grid_max - grid_min) / (res - 1) as f32).min_element();
    let (gpu_v, gpu_i) = mesh_sdf_grid_gpu(&device, &queue, grid, res, grid_min, grid_max);
    let cpu = cpu_mesh(grid, res, grid_min, grid_max);
    let gpu_pos: Vec<Vec3> = gpu_v.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
    eprintln!("[SDF-DC] {name}: gpu_verts={} gpu_idx={} cpu_verts={} cpu_quads={}",
        gpu_pos.len(), gpu_i.len(), cpu.vertices.len(), cpu.quads.len());
    assert!(gpu_pos.len() > 100, "{name}: too few GPU vertices ({})", gpu_pos.len());
    assert!(gpu_i.len() >= 3 && gpu_i.len() % 3 == 0, "{name}: bad GPU index count ({})", gpu_i.len());
    let ratio = gpu_pos.len() as f32 / cpu.vertices.len().max(1) as f32;
    assert!(ratio > 0.6 && ratio < 1.6, "{name}: vertex-count ratio {ratio:.2} out of range");
    let (mean_gc, max_gc) = parity(&gpu_pos, &cpu.vertices, voxel);
    let (mean_cg, _) = parity(&cpu.vertices, &gpu_pos, voxel);
    eprintln!("[SDF-DC] {name}: GPU→CPU mean={mean_gc:.3} max={max_gc:.3} | CPU→GPU mean={mean_cg:.3} (voxels)");
    assert!(mean_gc < 0.30 && mean_cg < 0.30, "{name}: mean nn distance too large");
    assert!(max_gc < 2.5, "{name}: max nn distance too large (geometry mismatch)");
}

#[test]
fn sdf_dc_parity_beveled_box() {
    let res = 32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(0.8), 0.18));
    run_parity("beveled_box", &grid, res, gmin, gmax);
}

#[test]
fn sdf_dc_parity_boolean_subtract() {
    let res = 32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(0.8), 0.05);
        let s = sd_sphere(p - Vec3::new(0.6, 0.6, 0.6), 0.6);
        b.max(-s)
    });
    run_parity("boolean_subtract", &grid, res, gmin, gmax);
}

#[test]
fn sdf_dc_parity_nonsymmetric_box() {
    // Off-center, anisotropic grid box — proves the grid_min/grid_max mapping (not a centered cube),
    // which is what real body bake bounds look like.
    let res = 40;
    let gmin = Vec3::new(-1.2, -0.6, -2.0);
    let gmax = Vec3::new(2.0, 1.4, 0.8);
    let center = Vec3::new(0.3, 0.2, -0.5);
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p - center, Vec3::new(0.7, 0.5, 0.6), 0.15));
    run_parity("nonsymmetric_box", &grid, res, gmin, gmax);
}

/// G1: the RESIDENT path writes the index count into the DrawIndexedIndirect args with NO readback;
/// assert that GPU-generated count equals the real mesh index count and stays within the buffer.
#[test]
fn sdf_dc_resident_indirect() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping resident"); return; }
    };
    let res = 32u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(0.8), 0.18));
    // Reference index count from the proven readback path.
    let (_rv, ri) = mesh_sdf_grid_gpu(&device, &queue, &grid, res, gmin, gmax);
    let ref_icount = ri.len() as u32;

    let (cv_cap, vert_floats, idx_cap) = dc_caps(res);
    let mk = |bytes: u64, extra: wgpu::BufferUsages, label: &str| device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST | extra,
        mapped_at_creation: false });
    let field = mk((grid.len() * 4) as u64, wgpu::BufferUsages::empty(), "field");
    queue.write_buffer(&field, 0, bytemuck::cast_slice(&grid));
    let params = device.create_buffer(&wgpu::BufferDescriptor { label: Some("params"),
        size: std::mem::size_of::<DcParamsGpu>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&params, 0, bytemuck::bytes_of(&dc_params(res, gmin, gmax)));
    let cell_vert = mk(cv_cap * 4, wgpu::BufferUsages::empty(), "cell_vert");
    let verts = mk(vert_floats * 4, wgpu::BufferUsages::VERTEX, "verts");
    let idx = mk(idx_cap * 4, wgpu::BufferUsages::INDEX, "idx");
    let counters = mk(8, wgpu::BufferUsages::empty(), "counters");
    // Indirect args: [index_count, instance_count=1, first_index=0, base_vertex=0, first_instance=0].
    let indirect = device.create_buffer(&wgpu::BufferDescriptor { label: Some("indirect"), size: 20,
        usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    queue.write_buffer(&indirect, 0, bytemuck::cast_slice(&[0u32, 1, 0, 0, 0]));

    let mesher = SdfMeshGpu::new(&device);
    mesher.dispatch_resident(&device, &queue, &field, &params, &cell_vert, &verts, &idx, &counters, &indirect, res);

    // Read back the indirect args + verts (debug only — the live path never does this).
    let s_ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_ind"), size: 20,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_verts = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_v"), size: vert_floats * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
    enc.copy_buffer_to_buffer(&indirect, 0, &s_ind, 0, 20);
    enc.copy_buffer_to_buffer(&verts, 0, &s_verts, 0, vert_floats * 4);
    queue.submit(std::iter::once(enc.finish()));
    s_ind.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    s_verts.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let ind: Vec<u32> = bytemuck::cast_slice(&s_ind.slice(..).get_mapped_range()).to_vec();
    let vflat: Vec<f32> = bytemuck::cast_slice(&s_verts.slice(..).get_mapped_range()).to_vec();
    s_ind.unmap(); s_verts.unmap();

    eprintln!("[SDF-DC] resident indirect args = {:?} (ref icount {})", ind, ref_icount);
    // GPU-generated index_count must equal the readback path's, prove instance_count seed survived,
    // and never exceed the index buffer length (the atomicMin device-loss guard).
    assert_eq!(ind[0], ref_icount, "indirect index_count != real mesh index count");
    assert_eq!(ind[1], 1, "instance_count seed clobbered");
    assert!(ind[0] as u64 <= idx_cap, "indirect index_count {} exceeds idx buffer cap {}", ind[0], idx_cap);
    // Resident verts populated (non-zero positions present).
    let nonzero = vflat.chunks(8).take(64).filter(|c| c[0] != 0.0 || c[1] != 0.0 || c[2] != 0.0).count();
    assert!(nonzero > 0, "resident vertex buffer looks empty");
}

/// G1: the per-body SdfBodyMeshSlot resident dispatch must produce the SAME mesh as the proven
/// readback path (identical shader + grid). Reads the slot's verts/counters/indirect back and diffs.
#[test]
fn sdf_dc_slot_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping slot parity"); return; }
    };
    let res = 32u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(0.8), 0.18));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Golden via the proven readback path.
    let (gv, gi) = mesh_sdf_grid_gpu(&device, &queue, &grid, res, gmin, gmax);
    let golden: Vec<Vec3> = gv.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();

    // Resident slot path (the G1 type).
    let mesher = SdfMeshGpu::new(&device);
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot(&device, &queue, &mut slot, &grid, gmin, gmax);

    let (_cv, vert_floats, _idx) = dc_caps(res);
    let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_v"), size: vert_floats * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_cnt = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_cnt"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_ind"), size: 20,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
    enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_floats * 4);
    enc.copy_buffer_to_buffer(&slot.counters, 0, &s_cnt, 0, 8);
    enc.copy_buffer_to_buffer(&slot.indirect, 0, &s_ind, 0, 20);
    queue.submit(std::iter::once(enc.finish()));
    s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    s_cnt.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    s_ind.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_cnt.slice(..).get_mapped_range()).to_vec();
    let ind: Vec<u32> = bytemuck::cast_slice(&s_ind.slice(..).get_mapped_range()).to_vec();
    s_v.unmap(); s_cnt.unmap(); s_ind.unmap();

    let vcount = cnt[0] as usize;
    eprintln!("[SDF-DC] slot_parity: slot_verts={} slot_idx(counters)={} indirect={:?} | golden_verts={} golden_idx={}",
        vcount, cnt[1], ind, golden.len(), gi.len());
    // Same shader + same grid → identical vertex/index counts, and the GPU-written indirect index_count
    // must match the golden index count exactly (proves the slot's indirect-args path).
    assert_eq!(vcount, golden.len(), "slot vertex count != golden");
    assert_eq!(ind[0], gi.len() as u32, "slot indirect index_count != golden index count");
    assert_eq!(ind[1], 1, "instance_count seed clobbered");

    let resident: Vec<Vec3> = (0..vcount).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
    let (mean, mx) = parity(&resident, &golden, voxel);
    eprintln!("[SDF-DC] slot_parity: resident→golden mean={mean:.4} max={mx:.4} voxels");
    // Identical compute → positions coincide to ~0 (allow a hair for float/order).
    assert!(mean < 0.05 && mx < 0.5, "slot resident mesh diverges from golden (mean={mean:.4} max={mx:.4})");
}

/// G3 GATE (mandatory before any resident-density wiring): meshing from an EXTERNAL density buffer
/// (`mesh_into_slot_from_resident`, the production G3 entry) must produce the SAME mesh as uploading
/// the grid into `slot.field` (`mesh_into_slot`, the proven G1/G2 path). The ONLY difference between
/// the two paths is the field buffer SOURCE — same shader, same params, same grid bytes — so this
/// isolates exactly the G3 risk the adversarial de-risk flagged: a hidden layout / padding / res-vs-
/// cells / alignment flaw when the field comes from a copied GPU buffer instead of a host write. If
/// this fails, the resident `rv.buffer` is NOT a drop-in field source and G3 stops here.
#[test]
fn sdf_dc_resident_field_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping resident-field parity"); return; }
    };
    let res = 32u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(0.8), 0.18));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let mesher = SdfMeshGpu::new(&device);

    // Helper: dispatch a slot, read back (vcount, indirect_index_count, vertex positions).
    let readback = |slot: &SdfBodyMeshSlot| -> (usize, u32, Vec<Vec3>) {
        let (_cv, vert_floats, _idx) = dc_caps(res);
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_v"), size: vert_floats * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_cnt = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_cnt"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_ind"), size: 20,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_floats * 4);
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_cnt, 0, 8);
        enc.copy_buffer_to_buffer(&slot.indirect, 0, &s_ind, 0, 20);
        queue.submit(std::iter::once(enc.finish()));
        s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_cnt.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_ind.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_cnt.slice(..).get_mapped_range()).to_vec();
        let ind: Vec<u32> = bytemuck::cast_slice(&s_ind.slice(..).get_mapped_range()).to_vec();
        s_v.unmap(); s_cnt.unmap(); s_ind.unmap();
        let vcount = cnt[0] as usize;
        let verts: Vec<Vec3> = (0..vcount).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
        (vcount, ind[0], verts)
    };

    // Path A: the proven upload-to-slot.field path.
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot(&device, &queue, &mut slot_a, &grid, gmin, gmax);
    let (va, ia, verts_a) = readback(&slot_a);

    // Path B: an EXTERNAL STORAGE|COPY_DST buffer holding the identical grid (mirrors rv.buffer being a
    // GPU→GPU copy of the bake output), bound as the field via the G3 production entry.
    let field_bytes = (res as u64).pow(3) * 4;
    let ext_field = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ext_resident_field"), size: field_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&ext_field, 0, bytemuck::cast_slice(&grid));
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_from_resident(&device, &queue, &mut slot_b, &ext_field, gmin, gmax);
    let (vb, ib, verts_b) = readback(&slot_b);

    eprintln!("[SDF-DC] resident_field_parity: A(field upload) verts={va} idx={ia} | B(external buf) verts={vb} idx={ib}");
    // Same shader + params + grid bytes → identical counts (deterministic, order-independent).
    assert_eq!(va, vb, "external-field vertex count != upload-field vertex count");
    assert_eq!(ia, ib, "external-field indirect index_count != upload-field index_count");
    // Geometric parity (vertex emission ORDER may differ between two GPU dispatches even with identical
    // input, so compare spatially — a layout/axis-swap/off-by-one flaw would blow `mean` up immediately).
    let (mean, mx) = parity(&verts_b, &verts_a, voxel);
    eprintln!("[SDF-DC] resident_field_parity: B→A mean={mean:.4} max={mx:.4} voxels");
    assert!(mean < 0.001 && mx < 0.01,
        "external-field mesh diverges from upload-field mesh (mean={mean:.4} max={mx:.4}) — rv.buffer is NOT a drop-in field source");
}

/// Phase 3B Step 2: a 512³ resident slot must ALLOCATE within the GPU binding limit. Before the
/// surface cap, `dc_caps(512)` sizes verts at 511³·8·4 ≈ 4.3GB > max_storage_buffer_binding_size →
/// an invalid buffer (the OOM wall that makes 512³ Object-Mode meshing impossible). The cap clamps
/// the surface-bounded output buffers so the slot fits; field/cell_vert (≈0.5GB) already fit the
/// real ~2GB limit.
#[test]
fn sdf_slot_512_fits_binding_limit() {
    let (_gpu_lock, device, _queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping slot-512 fit"); return; }
    };
    let max_bind = device.limits().max_storage_buffer_binding_size as u64;
    if 512u64.pow(3) * 4 > max_bind {
        eprintln!("[SDF-DC] adapter binding limit {}MB < 512³ field (512MB) — skipping", max_bind >> 20);
        return;
    }
    let mut slot = SdfBodyMeshSlot::new(&device, 512);
    // R3b: outputs are placeholders until a dispatch sizes them — force the DENSE (worst-case)
    // sizing here, exactly what the dense dispatch arms request.
    let (dv, di) = dc_output_caps_dense(max_bind, 512);
    assert!(
        slot.ensure_output_caps(&device, dv, di, dc_caps(512).0),
        "ensure_output_caps @512 must succeed under bind limit"
    );
    for (name, sz) in [("field", slot.field.as_ref().map_or(0, |b| b.size())), ("cell_vert", slot.cell_vert.size()),
                       ("verts", slot.verts.size()), ("idx", slot.idx.size())] {
        assert!(sz <= max_bind, "512³ slot `{}` = {}MB exceeds binding limit {}MB", name, sz >> 20, max_bind >> 20);
    }
    eprintln!("[SDF-DC] slot@512 fits: field={}MB cell_vert={}MB verts={}MB idx={}MB (limit {}MB)",
        slot.field.as_ref().map_or(0, |b| b.size()) >> 20, slot.cell_vert.size() >> 20, slot.verts.size() >> 20, slot.idx.size() >> 20, max_bind >> 20);
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// Phase 3 — GPU sparse-brick decode parity (was SPIKE B; now validates the PRODUCTION shader).
//
// Proves the WGSL sparse field read in `sdf_mesh.wgsl` (the 8³-brick decode + signed empty tags +
// the `brick_res` bitcast switch) yields a mesh IDENTICAL to the dense path and the production
// golden. Originally a SPIKE that patched the shader in-memory; Phase 3B promoted that patch into
// the real shader (bindings 6/7 + dual `fld`), so this test now builds the production pipeline
// (`build_dc_pipelines`) — the decode it validates is the one that actually ships.
// ───────────────────────────────────────────────────────────────────────────────────────────

/// Build the Phase-3 sparse brick set from a dense `res³` grid via the PRODUCTION classifier
/// (`core_math::chunked_dc::classify_grid_to_sparse`). Returns `(index, pool, bricks_per_axis)` —
/// the GPU atlas layout the shader decodes. Tight band (2·cell) keeps deep-interior bricks tagged
/// INTERIOR so the spike exercises that decode branch.
fn build_sparse_bricks(grid: &[f32], res: u32, gmin: Vec3, gmax: Vec3) -> (Vec<u32>, Vec<f32>, u32) {
    let cell_world = ((gmax - gmin) / (res - 1) as f32).min_element();
    let sb = core_math::chunked_dc::classify_grid_to_sparse(grid, res, 8, 2.0 * cell_world);
    (sb.index, sb.pool, sb.dims_bricks[0])
}

/// Dispatch the sparse-patched DC over the 8 bindings and read back `(vcount, index_count, positions)`.
fn dispatch_sparse(
    device: &wgpu::Device, queue: &wgpu::Queue,
    bgl: &wgpu::BindGroupLayout, pv: &wgpu::ComputePipeline, pq: &wgpu::ComputePipeline,
    field: &wgpu::Buffer, index_buf: &wgpu::Buffer, pool_buf: &wgpu::Buffer,
    res: u32, gmin: Vec3, gmax: Vec3, br: u32,
) -> (usize, u32, Vec<Vec3>, Vec<Vec3>) {
    let (cv_cap, vert_floats, idx_cap) = dc_caps(res);
    let mk = |bytes: u64, label: &str| device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false });
    let mut params = dc_params(res, gmin, gmax);
    params.gext[3] = f32::from_bits(br); // brick_res into gext.w (0 = dense), matches voxel SnParams.misc.w
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sp_params"),
        size: std::mem::size_of::<DcParamsGpu>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));
    // R3c: the sparse path brick-local-addresses cell_vert (handle*512 + local) — size for the
    // worst case (every brick a handle: br³×512) OR the dense region-local map, whichever is
    // larger, so this transient helper covers both br=0 and br>0 dispatches.
    let cv_elems = cv_cap.max((br as u64).pow(3) * 512);
    let cell_vert = mk(cv_elems * 4, "sp_cv");
    let verts = mk(vert_floats * 4, "sp_verts");
    let idx = mk(idx_cap * 4, "sp_idx");
    let counters = mk(8, "sp_cnt");
    queue.write_buffer(&counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sp_bg"), layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: field.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: cell_vert.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: verts.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: idx.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: counters.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: index_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: pool_buf.as_entire_binding() },
        ],
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sp_enc") });
    encode_dc_passes(&mut enc, pv, pq, &bg, res - 1);
    let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_v"), size: vert_floats * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    enc.copy_buffer_to_buffer(&verts, 0, &s_v, 0, vert_floats * 4);
    enc.copy_buffer_to_buffer(&counters, 0, &s_c, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    s_c.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    s_v.unmap(); s_c.unmap();
    let vcount = cnt[0] as usize;
    let pos: Vec<Vec3> = (0..vcount).map(|i| { let o = i*8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
    // Vertex layout = [px,py,pz, nx,ny,nz, 0,0] (8 floats); normal is floats 3..6.
    let nrm: Vec<Vec3> = (0..vcount).map(|i| { let o = i*8; Vec3::new(vflat[o+3], vflat[o+4], vflat[o+5]) }).collect();
    (vcount, cnt[1], pos, nrm)
}

#[test]
fn sdf_dc_sparse_brick_decode_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping sparse-brick parity"); return; }
    };
    // Grid sized so the solid is several bricks thick (deep INTERIOR bricks survive beyond the
    // band+apron) AND has empty margin each side (deep EXTERIOR bricks): res 96 = 12 bricks/axis,
    // box half 1.4 in a [-3,3] grid → surface ≈ bricks 3..8, interior ≈ 5..6, exterior ≈ 0 / 11.
    // Exercises all three decode paths (real + INTERIOR/-far + EXTERIOR/+far).
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Production golden (unpatched dense shader).
    let (gv, _gi) = mesh_sdf_grid_gpu(&device, &queue, &grid, res, gmin, gmax);
    let golden: Vec<Vec3> = gv.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();

    let (bgl, pv, pq) = build_dc_pipelines(&device, None); // PRODUCTION pipeline (sparse path now native)
    let mk_field = |data: &[f32], label: &str| {
        let b = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label),
            size: ((data.len() * 4) as u64).max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&b, 0, bytemuck::cast_slice(data));
        b
    };

    // (1) Patched DENSE path (br=0): proves the string-surgery patch leaves dense behaviour intact.
    let dense_field = mk_field(&grid, "sp_dense_field");
    let dummy_idx = device.create_buffer(&wgpu::BufferDescriptor { label: Some("dummy_idx_u32"),
        size: 4, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&dummy_idx, 0, bytemuck::cast_slice(&[0u32]));
    let dummy_pool = mk_field(&[0.0f32], "dummy_pool");
    let (vd, id, pos_d, nrm_d) = dispatch_sparse(&device, &queue, &bgl, &pv, &pq, &dense_field, &dummy_idx, &dummy_pool, res, gmin, gmax, 0);

    // (2) Patched SPARSE path (br>0): the real decode under test.
    let (index, pool, br) = build_sparse_bricks(&grid, res, gmin, gmax);
    assert!(br >= 2, "need ≥2 bricks/axis to exercise seams (got {br})");
    let dummy_field = mk_field(&[0.0f32], "sp_sparse_field_dummy");
    let index_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sp_index"),
        size: (index.len() * 4) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&index_buf, 0, bytemuck::cast_slice(&index));
    let pool_buf = mk_field(&pool, "sp_pool");
    let (vs, is_, pos_s, nrm_s) = dispatch_sparse(&device, &queue, &bgl, &pv, &pq, &dummy_field, &index_buf, &pool_buf, res, gmin, gmax, br);

    let interior_tags = index.iter().filter(|&&t| t == 0xffff_fffe).count();
    let real_bricks = index.iter().filter(|&&t| t < 0xffff_fffe).count();
    let exterior_tags = index.iter().filter(|&&t| t == 0xffff_ffff).count();
    eprintln!("[SDF-DC] sparse_decode: bricks/axis={br} | real={real_bricks} interior={interior_tags} exterior={exterior_tags} pool_floats={}", pool.len());
    eprintln!("[SDF-DC] sparse_decode: golden_verts={} | patched_dense v={vd} i={id} | patched_sparse v={vs} i={is_}", golden.len());

    // The geometry must actually exercise all three tag paths (else the decode is undertested).
    assert!(real_bricks > 0 && interior_tags > 0 && exterior_tags > 0,
        "sparse classify must produce real + interior + exterior bricks (got real={real_bricks} interior={interior_tags} exterior={exterior_tags})");

    // Patched dense == production golden (patch is inert when br=0).
    assert_eq!(vd, golden.len(), "patched dense vertex count != golden");
    let (mean_d, max_d) = parity(&pos_d, &golden, voxel);
    assert!(mean_d < 0.001 && max_d < 0.01, "patched dense diverged from golden (mean={mean_d:.5} max={max_d:.5})");

    // Sparse decode == dense (same counts + bit-exact-class spatial parity).
    assert_eq!(vs, vd, "sparse vertex count != dense vertex count");
    assert_eq!(is_, id, "sparse index count != dense index count");
    let (mean_s, max_s) = parity(&pos_s, &pos_d, voxel);
    eprintln!("[SDF-DC] sparse_decode: sparse→dense mean={mean_s:.5} max={max_s:.5} voxels");
    assert!(mean_s < 0.001 && max_s < 0.01,
        "GPU sparse-brick decode diverged from dense (mean={mean_s:.5} max={max_s:.5}) — decode arithmetic is wrong");

    // ─── NORMAL parity (the SPIKE-B gap that let the black render through) ───────────────────────
    // Positions are bit-exact (asserted above), but the VERTEX NORMAL is sampled via gradient_normal at
    // offset points that can land in EXTERIOR/INTERIOR sentinel bricks the QEF's corner reads never touch.
    // Pair each sparse vertex to its nearest dense vertex by position and compare normals. This is the
    // measurement that turns "renders black" into a number.
    let nlen = |n: Vec3| n.length();
    let deg_s = nrm_s.iter().filter(|&&n| nlen(n) < 0.5).count();
    let deg_d = nrm_d.iter().filter(|&&n| nlen(n) < 0.5).count();
    let mut ang_sum = 0.0f32; let mut ang_max = 0.0f32; let mut paired = 0usize;
    for (i, &ps) in pos_s.iter().enumerate() {
        let ns = nrm_s[i];
        if nlen(ns) < 0.5 { continue; }
        let mut best = f32::MAX; let mut bj = 0usize;
        for (j, &pd) in pos_d.iter().enumerate() { let d = ps.distance_squared(pd); if d < best { best = d; bj = j; } }
        let nd = nrm_d[bj];
        if nlen(nd) < 0.5 { continue; }
        let ang = ns.normalize().dot(nd.normalize()).clamp(-1.0, 1.0).acos().to_degrees();
        ang_sum += ang; ang_max = ang_max.max(ang); paired += 1;
    }
    let ang_mean = ang_sum / paired.max(1) as f32;
    eprintln!("[SDF-DC] sparse_decode NORMALS: degenerate sparse={deg_s}/{vs} dense={deg_d}/{vd} | \
        angle(sparse vs dense) mean={ang_mean:.2}deg max={ang_max:.2}deg over {paired} paired");
    // EXPECTED TO FAIL initially — this is the black-render bug being measured. Once the sparse normal
    // path is fixed, this becomes the regression guard that un-parks sparse.
    assert!(deg_s <= deg_d + (vs / 100).max(2),
        "sparse produced FAR more degenerate (zero) normals than dense ({deg_s} vs {deg_d}) — black render");
    assert!(ang_mean < 5.0 && ang_max < 35.0,
        "sparse normals diverge from dense (mean={ang_mean:.2}deg max={ang_max:.2}deg) — black render bug");
}

/// Phase 3A slice 1 GATE: the PRODUCTION sparse entry — `mesh_into_slot_sparse` reading the
/// PERSISTENT `graphics::SdfBrickAtlasGpu` (classify → ensure_capacity → upload, exactly the
/// app_poll pre-pass sequence) — must produce the same mesh as the proven dense slot path
/// (`mesh_into_slot`). Also exercises the grow-only watermark: the atlas starts deliberately
/// undersized, `ensure_capacity` grows it (exactly one generation bump), and a repeat call at the
/// same need is a strict no-op (no bump, no realloc). This is the persistent-atlas twin of the
/// transient-buffer decode parity above.
#[test]
fn sdf_dc_sparse_atlas_slot_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping sparse-atlas slot parity"); return; }
    };
    // Same geometry as the decode-parity test: multi-brick surface + deep INTERIOR + EXTERIOR tags.
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let mesher = SdfMeshGpu::new(&device);

    let readback = |slot: &SdfBodyMeshSlot| -> (usize, u32, Vec<Vec3>) {
        // R3b: sparse slots size verts surface-proportionally — copy the slot's ACTUAL buffer size.
        let vert_bytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_v"), size: vert_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_cnt = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_cnt"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_ind"), size: 20,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rb") });
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_bytes);
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_cnt, 0, 8);
        enc.copy_buffer_to_buffer(&slot.indirect, 0, &s_ind, 0, 20);
        queue.submit(std::iter::once(enc.finish()));
        s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_cnt.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_ind.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_cnt.slice(..).get_mapped_range()).to_vec();
        let ind: Vec<u32> = bytemuck::cast_slice(&s_ind.slice(..).get_mapped_range()).to_vec();
        s_v.unmap(); s_cnt.unmap(); s_ind.unmap();
        let vcount = cnt[0] as usize;
        let verts: Vec<Vec3> = (0..vcount).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
        (vcount, ind[0], verts)
    };

    // Golden: the proven dense slot path.
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot(&device, &queue, &mut slot_a, &grid, gmin, gmax);
    let (va, ia, verts_a) = readback(&slot_a);

    // Classify with the SAME band math as the app_poll pre-pass.
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, 2.0 * cell_world);
    let index_needed = sb.index.len() as u32;
    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    assert!(pool_bricks > 0, "test geometry must allocate real bricks");

    // Grow-only watermark. `new` quantum-rounds, so a res-96 body already fits the initial
    // capacity: (1) sufficient capacity is a strict no-op (no bump, no realloc); (2) exceeding
    // EITHER capacity grows and bumps the generation exactly once.
    let mut atlas = graphics::SdfBrickAtlasGpu::new(&device, index_needed, pool_bricks);
    assert_eq!(atlas.generation, 0);
    assert!(atlas.index_capacity >= index_needed && atlas.pool_capacity_bricks >= pool_bricks);
    let caps = (atlas.index_capacity, atlas.pool_capacity_bricks);
    atlas.ensure_capacity(&device, &queue, index_needed, pool_bricks);
    assert_eq!(atlas.generation, 0, "grow-only watermark: sufficient capacity must be a no-op");
    assert_eq!((atlas.index_capacity, atlas.pool_capacity_bricks), caps, "no-op grow must not realloc");
    atlas.ensure_capacity(&device, &queue, caps.0 + 1, caps.1 + 1);
    assert_eq!(atlas.generation, 1, "grow must bump the generation exactly once");
    assert!(atlas.index_capacity > caps.0 && atlas.pool_capacity_bricks > caps.1, "grow must raise capacity");

    atlas.upload(&queue, &sb.index, &sb.pool, sb.dims_bricks, res,
        core_math::scene_graph::NodeId(42));
    assert_eq!(atlas.res, res, "upload must stamp the explicit content res");
    assert_eq!(atlas.body_nid, Some(core_math::scene_graph::NodeId(42)));

    // Production sparse entry over the persistent atlas.
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas, gmin, gmax, core_math::scene_graph::NodeId(42), None);
    let (vb, ib, verts_b) = readback(&slot_b);

    eprintln!("[SDF-DC] sparse_atlas_slot_parity: dense v={va} i={ia} | atlas-sparse v={vb} i={ib} \
        (index cap={} pool cap={} bricks, gen={})", atlas.index_capacity, atlas.pool_capacity_bricks, atlas.generation);
    // Same shader + same field content → identical counts + bit-exact-class spatial parity
    // (matches the transient-buffer decode parity thresholds).
    assert_eq!(vb, va, "atlas-sparse vertex count != dense vertex count");
    assert_eq!(ib, ia, "atlas-sparse indirect index_count != dense index_count");
    let (mean, mx) = parity(&verts_b, &verts_a, voxel);
    eprintln!("[SDF-DC] sparse_atlas_slot_parity: sparse→dense mean={mean:.5} max={mx:.5} voxels");
    assert!(mean < 0.001 && mx < 0.01,
        "persistent-atlas sparse mesh diverged from dense (mean={mean:.5} max={mx:.5})");
}

/// Windowed-DC EXTREME case: a SMALL body in a LARGE grid — the escape-grown-grid scenario the
/// window optimization targets (a cube occupying a fraction of the volume). The handle-brick bbox
/// is ~13% of the grid per axis, so `mesh_into_slot_sparse` dispatches the DC over a small window
/// instead of the full 95³ cell grid. The mesh MUST still be bit-exact vs the dense full-grid path
/// (same counts, same vertices): cells outside the surface's brick bbox hold no surface, so
/// windowing them out changes nothing. This proves the window math (bbox → cell range → a0/a1/vmin
/// halo) at the small-body extreme, not just the ~47% body in the parity test above.
#[test]
fn sdf_dc_sparse_windowed_small_body_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping windowed small-body parity"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0)); // 6-unit grid
    // Half-extent 0.4 box → ~0.8/6 ≈ 13% of each axis. Off-CENTER so the window is not symmetric
    // (exercises non-zero cmin/cmax and the a0/vmin halo on all sides).
    let center = Vec3::new(0.9, -0.6, 0.3);
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p - center, Vec3::splat(0.4), 0.1));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let mesher = SdfMeshGpu::new(&device);

    let readback = |slot: &SdfBodyMeshSlot| -> (u32, u32, Vec<Vec3>) {
        // R3b: sparse slots size verts surface-proportionally — copy the slot's ACTUAL buffer size.
        let vert_bytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("wv"), size: vert_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("wc"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_bytes);
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        queue.submit(std::iter::once(enc.finish()));
        s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_c.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        s_v.unmap(); s_c.unmap();
        let vc = cnt[0] as usize;
        let verts = (0..vc).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
        (cnt[0], cnt[1], verts)
    };

    // Golden: dense full-grid mesh.
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot(&device, &queue, &mut slot_a, &grid, gmin, gmax);
    let (va, ia, verts_a) = readback(&slot_a);
    assert!(va > 0, "dense mesh produced no vertices — bad test scene");

    // Windowed sparse over the atlas.
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, 2.0 * cell_world);
    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(&device, sb.index.len() as u32, pool_bricks);
    atlas.upload(&queue, &sb.index, &sb.pool, sb.dims_bricks, res, core_math::scene_graph::NodeId(7));

    // The window must actually be SMALLER than the grid — else this proves nothing.
    let (bmin, bmax) = (atlas.surface_brick_min, atlas.surface_brick_max);
    let full_bricks = sb.dims_bricks[0].max(1);
    let win_bricks = bmax[0].saturating_sub(bmin[0]) + 1;
    assert!(win_bricks < full_bricks,
        "vacuous: surface window ({win_bricks} bricks) is not smaller than the grid ({full_bricks})");

    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas, gmin, gmax, core_math::scene_graph::NodeId(7), None);
    let (vb, ib, verts_b) = readback(&slot_b);

    eprintln!("[SDF-DC] windowed_small_body: window={win_bricks}/{full_bricks} bricks/axis | dense v={va} i={ia} | windowed v={vb} i={ib}");
    assert_eq!(vb, va, "windowed vertex count != dense (window dropped or duplicated surface)");
    assert_eq!(ib, ia, "windowed index_count != dense");
    let (mean, mx) = parity(&verts_b, &verts_a, voxel);
    assert!(mean < 0.001 && mx < 0.01,
        "windowed small-body mesh diverged from dense (mean={mean:.5} max={mx:.5})");
}

/// R3c (Tiled-DC stage A) ADVERSARIAL gate: surface faces landing EXACTLY on brick boundaries —
/// the GPU twin of `chunked_dc::brick_accessor_apron_parity_adversarial`. This is the worst case
/// for the brick-local `cell_vert` addressing: the quad pass's neighbor reads (cv at c−1) cross
/// the brick seam on every boundary-face quad, so a wrong owning-brick rule (or a sentinel-owned
/// neighbor that SHOULD have held a vertex) drops/duplicates quads right at the seam. Must stay
/// bit-exact vs the dense slot path. Also reports the dense vs sparse cell_vert byte sizes.
#[test]
fn sdf_dc_sparse_cell_vert_brick_boundary_parity() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping brick-boundary cell_vert parity"); return; }
    };
    // res 97 → 96 cells = 12 bricks + 1 boundary vert; box half 1.0 in [-3,3] puts the faces at
    // grid coords 32 and 64 — EXACT brick boundaries (multiples of 8), the seam hazard.
    let res = 97u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.0), 0.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let mesher = SdfMeshGpu::new(&device);

    let readback = |slot: &SdfBodyMeshSlot| -> (u32, u32, Vec<Vec3>) {
        let vert_bytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("bb_v"), size: vert_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("bb_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_bytes);
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        queue.submit(std::iter::once(enc.finish()));
        s_v.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        s_c.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        s_v.unmap(); s_c.unmap();
        let verts = (0..cnt[0] as usize).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
        (cnt[0], cnt[1], verts)
    };

    // Golden: dense slot path (region-local cell_vert addressing).
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot(&device, &queue, &mut slot_a, &grid, gmin, gmax);
    let (va, ia, verts_a) = readback(&slot_a);
    assert!(va > 0, "dense mesh produced no vertices — bad test scene");

    // Sanity: the surface really sits on brick boundaries — both boundary-adjacent brick layers
    // must be handles (else the seam hazard is not exercised and the parity below is vacuous).
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, 2.0 * cell_world);
    let bpa = sb.dims_bricks[0];
    let mid = bpa / 2;
    for bx in [3u32, 4] { // bricks either side of grid coord 32 (= brick boundary 4*8)
        let bi = (bx + mid * bpa + mid * bpa * bpa) as usize;
        assert!(sb.index[bi] < 0xffff_fffe,
            "brick ({bx},{mid},{mid}) not a handle — face is not on the brick seam, test vacuous");
    }

    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(&device, sb.index.len() as u32, pool_bricks);
    atlas.upload(&queue, &sb.index, &sb.pool, sb.dims_bricks, res, core_math::scene_graph::NodeId(11));
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas, gmin, gmax, core_math::scene_graph::NodeId(11), None);
    let (vb, ib, verts_b) = readback(&slot_b);

    eprintln!("[R3c] brick_boundary: dense v={va} i={ia} cell_vert={}KB | sparse v={vb} i={ib} cell_vert={}KB ({} bricks)",
        slot_a.cell_vert.size() >> 10, slot_b.cell_vert.size() >> 10, atlas.allocated_bricks);
    assert_eq!(vb, va, "brick-boundary sparse vertex count != dense (cell_vert brick addressing broke the seam)");
    assert_eq!(ib, ia, "brick-boundary sparse index_count != dense (seam quads dropped/duplicated)");
    let (mean, mx) = parity(&verts_b, &verts_a, voxel);
    assert!(mean < 0.001 && mx < 0.01,
        "brick-boundary sparse mesh diverged from dense (mean={mean:.5} max={mx:.5})");
    // The sparse map must actually be smaller than the dense map (the point of stage A).
    assert!(slot_b.cell_vert.size() < slot_a.cell_vert.size(),
        "sparse cell_vert ({}) not smaller than dense ({})", slot_b.cell_vert.size(), slot_a.cell_vert.size());
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// R3b — surface-proportional output sizing: the measurement that backs VERTS_PER_BRICK_HEADROOM
// + the overflow-grow gate.
// ───────────────────────────────────────────────────────────────────────────────────────────

/// R3b sizing helper for the tests below: classify → persistent atlas upload → sparse slot mesh →
/// blocking counters read. Returns (allocated_bricks, vert_count, index_count, slot).
fn measure_sparse_scene<F: Fn(Vec3) -> f32>(
    device: &wgpu::Device, queue: &wgpu::Queue, mesher: &SdfMeshGpu,
    res: u32, gmin: Vec3, gmax: Vec3, f: F,
) -> (u64, u32, u32, SdfBodyMeshSlot) {
    let grid = bake_grid(res, gmin, gmax, f);
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, 2.0 * cell_world);
    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(device, sb.index.len() as u32, pool_bricks);
    atlas.upload(queue, &sb.index, &sb.pool, sb.dims_bricks, res, core_math::scene_graph::NodeId(9));
    let mut slot = SdfBodyMeshSlot::new(device, res);
    mesher.mesh_into_slot_sparse(device, queue, &mut slot, &atlas, gmin, gmax, core_math::scene_graph::NodeId(9), None);
    // Read the counters directly (test-side, blocking).
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("m_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map callback dropped").expect("counters map failed");
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    s_c.unmap();
    (atlas.allocated_bricks as u64, cnt[0], cnt[1], slot)
}

/// R3b MEASUREMENT (gate a): actual DC vertices per ALLOCATED surface brick across real scene
/// shapes at 256/512 — the datum behind `VERTS_PER_BRICK_HEADROOM`. Run with `--nocapture` to see
/// the table; the assert checks the production factor comfortably covers the measured max
/// (≥1.5×) so a drifting classifier band or shader change re-opens the sizing question loudly.
#[test]
fn sdf_dc_sparse_verts_per_brick_measurement() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping measurement"); return; }
    };
    let mesher = SdfMeshGpu::new(&device);
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    type Scene = (&'static str, fn(Vec3) -> f32);
    let scenes: [Scene; 3] = [
        ("sphere-subtract", |p| sd_sphere(p, 2.0).max(-sd_sphere(p - Vec3::new(1.2, 0.8, 0.0), 1.3))),
        ("mirrored-twins",  |p| sd_round_box(p - Vec3::new(1.2, 0.0, 0.0), Vec3::splat(0.9), 0.15)
                                 .min(sd_round_box(p + Vec3::new(1.2, 0.0, 0.0), Vec3::splat(0.9), 0.15))),
        ("elongated-bar",   |p| sd_round_box(p, Vec3::new(2.6, 0.35, 0.35), 0.1)),
    ];
    let mut max_vpb = 0.0f64;
    for res in [256u32, 512] {
        for (name, f) in scenes {
            let (bricks, vc, ic, slot) = measure_sparse_scene(&device, &queue, &mesher, res, gmin, gmax, f);
            assert!(bricks > 0 && vc > 0, "{name}@{res}: empty scene — measurement is vacuous");
            let vpb = vc as f64 / bricks as f64;
            let ipv = ic as f64 / vc as f64;
            max_vpb = max_vpb.max(vpb);
            eprintln!("[R3b-MEASURE] {name:>16} @{res}: bricks={bricks} verts={vc} idx={ic} -> {vpb:.1} verts/brick, {ipv:.2} idx/vert");
            // R3c: cell_vert bytes, dense (cells³ map) vs sparse (bricks×512 brick-local map).
            eprintln!("[R3c-CELLVERT] {name:>16} @{res}: dense={}MB sparse={}MB ({} bricks x 2KB)",
                (dc_caps(res).0 * 4) >> 20, slot.cell_vert.size() >> 20, bricks);
        }
    }
    eprintln!("[R3b-MEASURE] max verts/allocated-brick = {max_vpb:.1}; production factor = {VERTS_PER_BRICK_HEADROOM}");
    assert!(VERTS_PER_BRICK_HEADROOM as f64 >= max_vpb * 1.5,
        "VERTS_PER_BRICK_HEADROOM ({VERTS_PER_BRICK_HEADROOM}) no longer >=1.5x the measured max \
         ({max_vpb:.1} verts/brick) — re-measure and update the constant + its comment");
}

/// R3b OVERFLOW-GROW gate (gate b): force an output overflow with a deliberately tiny headroom
/// factor, prove (1) the atomicMin clamp kept the truncated dispatch safe, (2) the production
/// detection (`poll_counter_overflow` over the dispatch-issued readback) fires, (3) the production
/// grow (`grow_output_caps`, x1.5/exact-need, generation bump) + re-dispatch yields counts
/// IDENTICAL to an ample-cap run, spot-verified spatially. No device loss = the test completing.
#[test]
fn sdf_dc_sparse_output_overflow_grow() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping overflow-grow"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let scene = |p: Vec3| sd_round_box(p, Vec3::splat(1.4), 0.18);
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Reference: ample caps (the production factor).
    let mesher_ok = SdfMeshGpu::new(&device);
    let (_b, ref_v, ref_i, ref_slot) = measure_sparse_scene(&device, &queue, &mesher_ok, res, gmin, gmax, scene);
    assert!(ref_v > 0, "reference scene produced no vertices");
    assert!(!ref_slot.output_overflow(ref_v, ref_i),
        "reference run overflowed the production factor — VERTS_PER_BRICK_HEADROOM is too small");

    // Overflow run: headroom=2 verts/brick is far below any real surface (measured ~20-25).
    let mut mesher_tiny = SdfMeshGpu::new(&device);
    mesher_tiny.verts_per_brick_headroom = 2;
    let (_b2, vc, ic, mut slot) = measure_sparse_scene(&device, &queue, &mesher_tiny, res, gmin, gmax, scene);
    let (cap_v0, cap_i0, gen0) = (slot.vert_cap, slot.idx_cap, slot.generation);
    assert!(slot.output_overflow(vc, ic), "tiny caps did not overflow (verts={vc} cap={cap_v0}) — vacuous test");

    // PRODUCTION detection path: the dispatch already kicked the async counters readback;
    // poll it exactly as poll_sdf_mesh_output_overflow does.
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let counts = slot.poll_counter_overflow().expect("overflow readback must report the truncation");
    assert_eq!(counts[0], vc, "readback vert count mismatch");

    // PRODUCTION grow + re-dispatch (the poll-phase handler body).
    assert!(slot.grow_output_caps(&device, counts[0] as u64), "grow must succeed below the dense ceiling");
    assert_eq!(slot.generation, gen0 + 1, "output grow must bump the slot generation exactly once");
    assert!(slot.vert_cap >= vc as u64 && slot.idx_cap > cap_i0, "grow must raise both caps");
    // Rebuild the atlas content (measure_sparse_scene owns its atlas; recreate identically).
    let grid = bake_grid(res, gmin, gmax, scene);
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, 2.0 * cell_world);
    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(&device, sb.index.len() as u32, pool_bricks);
    atlas.upload(&queue, &sb.index, &sb.pool, sb.dims_bricks, res, core_math::scene_graph::NodeId(9));
    let (g0, g1) = slot.last_sparse_bounds.expect("sparse dispatch must stamp last_sparse_bounds");
    mesher_tiny.mesh_into_slot_sparse(&device, &queue, &mut slot, &atlas, g0, g1, core_math::scene_graph::NodeId(9), None);

    // Read back the re-meshed counts + verts and compare against the ample-cap reference.
    let readback_counts = |slot: &SdfBodyMeshSlot| -> ([u32; 2], Vec<Vec3>) {
        let vert_bytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("og_v"), size: vert_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("og_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vert_bytes);
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();
        s_v.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx2.send(r); });
        s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        rx.recv().expect("map dropped").expect("map failed");
        rx.recv().expect("map dropped").expect("map failed");
        let vflat: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        s_v.unmap(); s_c.unmap();
        let verts = (0..cnt[0] as usize).map(|i| { let o = i * 8; Vec3::new(vflat[o], vflat[o+1], vflat[o+2]) }).collect();
        ([cnt[0], cnt[1]], verts)
    };
    let ([gv, gi], verts_grown) = readback_counts(&slot);
    let (_, verts_ref) = readback_counts(&ref_slot);
    eprintln!("[R3b-GROW] truncated verts={vc} (cap {cap_v0}) -> grown cap v={} i={} gen={} -> re-mesh verts={gv} idx={gi} (ref v={ref_v} i={ref_i})",
        slot.vert_cap, slot.idx_cap, slot.generation);
    assert_eq!(gv, ref_v, "re-meshed vertex count != ample-cap reference");
    assert_eq!(gi, ref_i, "re-meshed index count != ample-cap reference");
    assert!(!slot.output_overflow(gv, gi), "re-mesh still overflows after the grow");
    let (mean, mx) = parity(&verts_grown, &verts_ref, voxel);
    assert!(mean < 0.001 && mx < 0.01,
        "grown re-mesh diverged spatially from the ample-cap mesh (mean={mean:.5} max={mx:.5})");
}

/// REGRESSION (2026-07-13 — the multi-body "surface breaks + resolution change can't recover"
/// bug): a DENSE mesh dispatched into a slot whose CAPACITY came from the sparse ≥512 era
/// (capacity_res = 1024) must size its dense buffers (field + cell_vert) from the DISPATCH res,
/// never from capacity. Pre-fix, `mesh_into_slot*` requested `dc_caps(capacity_res).0` for
/// cell_vert (1023³ × 4B = 4.28GB) and `ensure_field` sized the field at capacity (4.29GB) —
/// both exceed `max_storage_buffer_binding_size` (2.1GB) → `sdf_dc_bg` BufferRangeTooLarge →
/// encoder-invalid cascade, and the GROW-ONLY cap poisoned the slot permanently: every later
/// dense mesh (including the Grid-picker recovery re-bake) failed, so the surface froze.
/// AZRA_AUTOREPRO=2 + AZRA_AUTOREPRO_RES=1024 reproduced the exact owner sequence.
#[test]
fn sdf_dc_dense_into_1024_capacity_slot_stays_bindable() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SDF-DC] no GPU adapter — skipping 1024-capacity regression"); return; }
    };
    let res = 64u32; // the Grid-picker drop to a dense resolution
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(0.8), 0.18));

    let mesher = SdfMeshGpu::new(&device);
    // Sparse-era slot: capacity_res = 1024 (buffers are lazy 4-byte placeholders until sized).
    let mut slot = SdfBodyMeshSlot::new(&device, 1024);
    slot.res = res; // capacity reuse at the new logical res (the production ensure block)

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    mesher.mesh_into_slot(&device, &queue, &mut slot, &grid, gmin, gmax);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let err = pollster::block_on(device.pop_error_scope());
    assert!(err.is_none(),
        "dense mesh into a 1024-capacity slot raised a validation error (the encoder-invalid \
         cascade class): {err:?}");

    // Every dense buffer the DC bind group binds entire must be bindable.
    let max_bind = device.limits().max_storage_buffer_binding_size as u64;
    let field_size = slot.field.as_ref().expect("dense field allocated").size();
    assert!(field_size <= max_bind, "field {}MB exceeds the binding limit", field_size >> 20);
    assert!(slot.cell_vert.size() <= max_bind,
        "cell_vert {}MB exceeds the binding limit", slot.cell_vert.size() >> 20);
    // Sized from the DISPATCH res, not capacity: cells³ at 64, not at 1024.
    let (cv_res, ..) = dc_caps(res);
    assert!(slot.cell_vert_cap >= cv_res && slot.cell_vert_cap < dc_caps(1024).0,
        "cell_vert cap {} not sized from the dispatch res (cells³={})", slot.cell_vert_cap, cv_res);

    // And the mesh actually landed: GPU-written indirect index_count is nonzero.
    let s_ind = device.create_buffer(&wgpu::BufferDescriptor { label: Some("s_ind"), size: 20,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.indirect, 0, &s_ind, 0, 20);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    s_ind.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map dropped").expect("map failed");
    let ind: Vec<u32> = bytemuck::cast_slice(&s_ind.slice(..).get_mapped_range()).to_vec();
    s_ind.unmap();
    assert!(ind[0] > 0, "dense mesh into the 1024-capacity slot produced no triangles");
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// Stage B-1 — SdfMeshPool: one shared slotted pool + multi-draw args for sparse bodies.
// ───────────────────────────────────────────────────────────────────────────────────────────

/// Test-side readback of a slot's counters ([vert_count, index_count]), its pool indirect entry
/// (5 u32), and its pool vertex region positions. GPU Safety rule: map Results captured.
fn pool_readback(
    device: &wgpu::Device, queue: &wgpu::Queue,
    pool: &SdfMeshPool, nid: core_math::scene_graph::NodeId, slot: &SdfBodyMeshSlot,
) -> ([u32; 2], [u32; 5], Vec<Vec3>) {
    let ps = pool.slot(nid).expect("pooled slot must exist");
    let vbytes = ps.vert_region_bytes();
    let mk = |label: &str, size: u64| device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_c = mk("p_c", 8);
    let s_i = mk("p_i", 20);
    let s_v = mk("p_v", vbytes);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let grp = pool.group_of(nid).expect("pooled slot has a group"); // Phase 4
    enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
    enc.copy_buffer_to_buffer(&grp.indirect, ps.indirect_offset(), &s_i, 0, 20);
    enc.copy_buffer_to_buffer(&grp.verts, ps.vert_off, &s_v, 0, vbytes);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    for b in [&s_c, &s_i, &s_v] {
        let tx = tx.clone();
        b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    for _ in 0..3 { rx.recv().expect("map dropped").expect("map failed"); }
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    let ind: Vec<u32> = bytemuck::cast_slice(&s_i.slice(..).get_mapped_range()).to_vec();
    let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    s_c.unmap(); s_i.unmap(); s_v.unmap();
    let vcount = (cnt[0] as usize).min(ps.vert_cap as usize);
    let verts = (0..vcount).map(|i| { let o = i * 8; Vec3::new(vf[o], vf[o+1], vf[o+2]) }).collect();
    ([cnt[0], cnt[1]], [ind[0], ind[1], ind[2], ind[3], ind[4]], verts)
}

/// Classify `grid` into a persistent atlas (production pre-pass sequence). Shared by the pool tests.
fn atlas_for_grid(
    device: &wgpu::Device, queue: &wgpu::Queue, grid: &[f32], res: u32,
    gmin: Vec3, gmax: Vec3, nid: core_math::scene_graph::NodeId,
) -> graphics::SdfBrickAtlasGpu {
    let cell_world = ((gmax - gmin) / (res - 1).max(1) as f32).min_element().max(1e-6);
    let sb = core_math::chunked_dc::classify_grid_to_sparse(grid, res, 8, 2.0 * cell_world);
    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(device, sb.index.len() as u32, pool_bricks);
    atlas.upload(queue, &sb.index, &sb.pool, sb.dims_bricks, res, nid);
    atlas
}

/// Stage B-1 PARITY GATE: a 3-sparse-body scene meshed via the SHARED pool must produce, per
/// body, IDENTICAL vert/idx counts (and matching vertex positions) to the per-body-slot path —
/// both run in this test, same atlases. Also asserts the multi-draw args wiring: each body's
/// indirect entry carries first_index/base_vertex equal to its region offsets, and the draw-call
/// economics (3 per-body draws to 1 multi-draw over slot_count_hi entries) + pool VRAM are logged.
#[test]
fn sdf_pool_parity_multi_body() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfPool] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let scenes: Vec<(core_math::scene_graph::NodeId, Box<dyn Fn(Vec3) -> f32>)> = vec![
        (core_math::scene_graph::NodeId(101), Box::new(|p| sd_round_box(p, Vec3::splat(1.4), 0.18))),
        (core_math::scene_graph::NodeId(102), Box::new(|p: Vec3| sd_sphere(p - Vec3::new(0.4, 0.2, -0.3), 1.1))),
        (core_math::scene_graph::NodeId(103), Box::new(|p: Vec3| sd_round_box(p - Vec3::new(-0.8, 0.5, 0.6), Vec3::new(1.6, 0.5, 0.9), 0.05))),
    ];
    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false); // headless: fallback args (first_instance=0)

    let mut per_body_counts: Vec<[u32; 2]> = Vec::new();
    let mut per_body_verts: Vec<Vec<Vec3>> = Vec::new();
    let mut atlases = Vec::new();
    // Reference: the proven per-body-slot sparse path (pool = None).
    for (nid, f) in &scenes {
        let grid = bake_grid(res, gmin, gmax, f);
        let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, *nid);
        let mut slot = SdfBodyMeshSlot::new(&device, res);
        mesher.mesh_into_slot_sparse(&device, &queue, &mut slot, &atlas, gmin, gmax, *nid, None);
        assert!(!slot.pooled, "pool=None must stay unpooled");
        // counters + own-buffer verts readback
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("r_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let vbytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("r_v"), size: vbytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vbytes);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        for b in [&s_c, &s_v] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        for _ in 0..2 { rx.recv().unwrap().unwrap(); }
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        s_c.unmap(); s_v.unmap();
        let verts: Vec<Vec3> = (0..cnt[0] as usize).map(|i| { let o = i * 8; Vec3::new(vf[o], vf[o+1], vf[o+2]) }).collect();
        assert!(cnt[1] > 0, "reference sparse mesh empty — vacuous");
        per_body_counts.push([cnt[0], cnt[1]]);
        per_body_verts.push(verts);
        atlases.push(atlas);
    }

    // Pool path: same atlases, fresh slots, ONE shared pool.
    for (k, (nid, _)) in scenes.iter().enumerate() {
        let mut slot = SdfBodyMeshSlot::new(&device, res);
        mesher.mesh_into_slot_sparse(&device, &queue, &mut slot, &atlases[k], gmin, gmax, *nid, Some(&mut pool));
        assert!(slot.pooled, "sparse dispatch with a pool must pool the slot");
        let (cnt, ind, verts) = pool_readback(&device, &queue, &pool, *nid, &slot);
        let ps = pool.slot(*nid).unwrap();
        assert_eq!(cnt, per_body_counts[k], "body {k}: pooled counts != per-slot counts");
        assert_eq!(ind[0], cnt[1], "body {k}: indirect index_count != counters");
        assert_eq!(ind[2] as u64, ps.idx_off / 4, "body {k}: first_index != region offset");
        assert_eq!(ind[3] as u64, ps.vert_off / 32, "body {k}: base_vertex != region offset");
        let (mean, mx) = parity(&verts, &per_body_verts[k], voxel);
        assert!(mean < 0.001 && mx < 0.01, "body {k}: pooled mesh diverged (mean={mean:.5} max={mx:.5})");
        // instance_count is the CPU visibility switch — seeded 0 until a frame marks it drawn.
        assert_eq!(ind[1], 0, "body {k}: instance_count must be visibility-owned (seeded 0)");
    }
    eprintln!("[SdfPool] parity: {} bodies — draw calls {} -> 1 (multi-draw over {} slots), pool VRAM {:.1}MB",
        scenes.len(), scenes.len(), pool.slot_count_hi(), pool.vram_bytes() as f64 / 1.0e6);
    assert_eq!(pool.slot_count_hi(), scenes.len() as u32);
}

/// Stage B-1 GROW/RELOCATE GATE: (a) an undersized pooled region overflows, `grow_slot` (the
/// pool twin of R3b `grow_output_caps`) relocates it, generation bumps, and the re-mesh lands
/// counts IDENTICAL to an amply-sized reference; (b) a forced full REBUILD (request exhausting
/// the tail) preserves the OTHER body's region content and indirect index_count bit-exactly.
/// No device loss (Wait polls throughout).
#[test]
fn sdf_pool_grow_relocate_preserves_counts() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfPool] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let (nid_a, nid_b) = (core_math::scene_graph::NodeId(201), core_math::scene_graph::NodeId(202));
    let grid_a = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let grid_b = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.2));
    let atlas_a = atlas_for_grid(&device, &queue, &grid_a, res, gmin, gmax, nid_a);
    let atlas_b = atlas_for_grid(&device, &queue, &grid_b, res, gmin, gmax, nid_b);

    // Reference counts at ample headroom (per-body slot path).
    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false);
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas_a, gmin, gmax, nid_a, None);
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("g_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_c, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().unwrap().unwrap();
    let ref_counts: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    s_c.unmap();

    // B first (its region must survive A's grow/rebuild), then A with a TINY headroom mesher —
    // the deterministic overflow forcing (same trick as the R3b output-grow test).
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, Some(&mut pool));
    let (b_before_cnt, b_before_ind, b_before_verts) = pool_readback(&device, &queue, &pool, nid_b, &slot_b);
    assert!(b_before_ind[0] > 0, "body B meshed empty — vacuous");

    let mut mesher_tiny = SdfMeshGpu::new(&device);
    mesher_tiny.verts_per_brick_headroom = 1; // undersized on purpose: guaranteed overflow
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher_tiny.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, Some(&mut pool));
    assert!(slot_a.pooled);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let overflow = slot_a.poll_counter_overflow().expect("tiny headroom must overflow");
    assert!(overflow[0] as u64 > slot_a.vert_cap, "overflow readback must exceed the region cap");

    // Production grow path (poll_sdf_mesh_output_overflow's pooled arm).
    let max_bind = device.limits().max_storage_buffer_binding_size as u64;
    let (dv, di) = dc_output_caps_dense(max_bind, slot_a.capacity_res);
    let gen_before = pool.generation;
    assert!(pool.grow_slot(&device, &queue, nid_a, overflow[0] as u64, dv, di), "grow_slot must grow");
    assert!(pool.generation > gen_before, "relocation must bump the pool generation");
    {
        let ps = pool.slot(nid_a).unwrap();
        slot_a.vert_cap = ps.vert_cap;
        slot_a.idx_cap = ps.idx_cap;
    }
    let (g0, g1) = slot_a.last_sparse_bounds.unwrap();
    mesher_tiny.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, g0, g1, nid_a, Some(&mut pool));
    let (a_cnt, a_ind, _) = pool_readback(&device, &queue, &pool, nid_a, &slot_a);
    assert_eq!(a_cnt[0], ref_counts[0], "post-grow vert count != ample reference");
    assert_eq!(a_ind[0], ref_counts[1], "post-grow index_count != ample reference");

    // (b) Force a full REBUILD: request a region larger than the remaining tail. B must survive.
    let huge = (pool.vram_bytes() / 32).max(slot_a.vert_cap * 4); // guaranteed > tail
    let gen2 = pool.generation;
    pool.ensure_slot(&device, &queue, nid_a, huge, huge * 6).expect("rebuild must fit under the cap");
    assert!(pool.generation > gen2, "rebuild must bump the generation");
    let (b_after_cnt, b_after_ind, b_after_verts) = pool_readback(&device, &queue, &pool, nid_b, &slot_b);
    assert_eq!(b_after_cnt, b_before_cnt, "rebuild changed body B's counters (it must not touch them)");
    assert_eq!(b_after_ind[0], b_before_ind[0], "rebuild lost body B's indirect index_count");
    assert_eq!(b_after_verts.len(), b_before_verts.len());
    let (mean, mx) = parity(&b_after_verts, &b_before_verts, ((gmax - gmin) / (res - 1) as f32).min_element());
    assert!(mean < 1e-6 && mx < 1e-6, "rebuild corrupted body B's region content (mean={mean} max={mx})");
    // A re-meshes after the rebuild (its indirect was reseeded to 0) — counts identical again.
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, g0, g1, nid_a, Some(&mut pool));
    let (a2_cnt, ..) = pool_readback(&device, &queue, &pool, nid_a, &slot_a);
    assert_eq!(a2_cnt[0], ref_counts[0], "post-rebuild re-mesh vert count != reference");
    eprintln!("[SdfPool] grow/relocate: overflow verts={} grown cap v={} i={}, pool gen {} -> {}, VRAM {:.1}MB — B preserved bit-exact",
        overflow[0], slot_a.vert_cap, slot_a.idx_cap, gen_before, pool.generation, pool.vram_bytes() as f64 / 1.0e6);
}

/// S0.3 RATCHET GATE (2026-07-14) — rebuild sizing must anchor headroom to **demand**, never to the
/// pool's own previous size.
///
/// The regression this locks out: `rebuild()` sized the new buffer as
/// `need.max(self.verts_bytes * 3 / 2)`, i.e. `buffer(n+1) = max(need, 1.5 × buffer(n))` — a
/// recurrence on itself. Because `need` tracks the surface (≈ +12 %/step as bricks accumulate)
/// while the multiplier is a flat +50 %, `1.5 × buffer` outruns `need` at the FIRST rebuild and the
/// need term can never win again: the pool then multiplies by 1.5 per rebuild REGARDLESS of demand,
/// each size becoming the next base, monotone, never shrinking, up to `safe_max` saturation.
/// Measured on a real 1024³ drag (owner, 2026-07-14, RTX 4090): buffers went
/// verts 262→393→590→885 MB / idx 197→295→443→664 MB across four rebuilds while true need rose only
/// 262→324 MB (bricks 170 784→210 612, +23 %) — **2.74× over-allocated** and climbing. Harmless on a
/// 24 GB card; fatal on 8 GB.
///
/// This replays that exact brick sequence (scaled ÷100 to keep the test small — the ratchet is
/// scale-free, it is driven by the +12 %/step vs +50 % ratio, not by absolute size) and asserts the
/// INVARIANT rather than the formula: after every rebuild, each region buffer is within 1.5× of the
/// live packed footprint. Under the old code this fails from the 3rd rebuild on (buffer 590 MB vs
/// the 475 MB bound); a `.max(self.verts_bytes * 3/2)` reintroduced anywhere trips it again.
#[test]
fn sdf_pool_rebuild_sizing_tracks_demand_not_history() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfPool] no GPU adapter — skipping"); return; }
    };
    // Mirror of pool.rs's private `align256` — regions are 256B-aligned for storage sub-ranges.
    let align256 = |b: u64| -> u64 { ((b.max(4) + 255) / 256) * 256 };
    let nid = core_math::scene_graph::NodeId(301);
    let mut pool = SdfMeshPool::new(&device, false);

    // The real logged brick counts ÷100. want_v = bricks × VERTS_PER_BRICK_HEADROOM (48) is exactly
    // what `dc_output_caps_sparse` feeds `ensure_slot` on the sparse path (resident.rs:185,197).
    let bricks: [u64; 4] = [1707, 1916, 2060, 2106];
    let mut rebuilds = 0u64;
    let mut last_gen = pool.generation;
    for (step, b) in bricks.iter().enumerate() {
        let want_v = b * 48;
        let want_i = want_v * 6;
        pool.ensure_slot(&device, &queue, nid, want_v, want_i)
            .expect("pool must accept a surface-proportional sparse request this small");
        if pool.generation != last_gen { rebuilds += 1; last_gen = pool.generation; }

        // `need` = the live packed footprint (one slot here — the same quantity rebuild() sums).
        let need_v = align256(want_v * 32);
        let need_i = align256(want_i * 4);
        let grp = pool.group_of(nid).expect("slot exists");
        let (buf_v, buf_i) = (grp.verts.size(), grp.idx.size());
        assert!(buf_v <= need_v * 3 / 2,
            "step {step} (bricks={b}): vert buffer {buf_v} exceeds 1.5× demand {} — the pool is \
             ratcheting off its own previous size instead of tracking need (pool.rs rebuild sizing)",
            need_v * 3 / 2);
        assert!(buf_i <= need_i * 3 / 2,
            "step {step} (bricks={b}): index buffer {buf_i} exceeds 1.5× demand {} — ratchet \
             reintroduced (pool.rs rebuild sizing)", need_i * 3 / 2);
        assert!(buf_v >= need_v && buf_i >= need_i,
            "step {step}: buffers ({buf_v},{buf_i}) smaller than demand ({need_v},{need_i}) — \
             the mesher would write OOB");
    }

    // A REPEAT of the final request must be a strict no-op: grow-only sufficiency short-circuits
    // (pool.rs:172-174), so a stable surface reaches a FIXED POINT — the property the ratchet broke.
    let gen_settled = pool.generation;
    let settled_grp = pool.group_of(nid).expect("slot exists");
    let (v_settled, i_settled) = (settled_grp.verts.size(), settled_grp.idx.size());
    for _ in 0..3 {
        pool.ensure_slot(&device, &queue, nid, bricks[3] * 48, bricks[3] * 48 * 6).unwrap();
    }
    assert_eq!(pool.generation, gen_settled, "a settled surface must not rebuild — fixed point lost");
    let settled_grp = pool.group_of(nid).expect("slot exists");
    assert_eq!((settled_grp.verts.size(), settled_grp.idx.size()), (v_settled, i_settled),
        "a settled surface must not grow the pool — ratchet reintroduced");

    let need_v_final = align256(bricks[3] * 48 * 32);
    eprintln!("[SdfPool] ratchet gate: {} rebuilds over {} steps; final verts {:.2}MB vs demand \
        {:.2}MB ({:.2}× — bound 1.50×), settled fixed point held over 3 repeats",
        rebuilds, bricks.len(), v_settled as f64 / 1.0e6, need_v_final as f64 / 1.0e6,
        v_settled as f64 / need_v_final as f64);
}

/// Slice b SETTLE-TRIM GATE (2026-07-16): a transient brick peak leaves the grow-only caps at
/// ~2×+ the settled need (owner log: verts 1073MB + idx 846MB + cell_vert ~1GB pinned by ONE
/// gesture). A quiet-window mesh dispatch with `slot.may_trim` set must (a) shrink the pool
/// region + cell_vert toward the current need (hysteresis: fire under ½, land at 1.25×/exact),
/// (b) land counts IDENTICAL to the pre-trim mesh, (c) preserve the OTHER body's pooled content
/// bit-exactly across the trim rebuild, and (d) NOT fire when caps are already proportionate
/// (the deadband — no churn on a stable surface).
#[test]
fn sdf_pool_settle_trim_shrinks_and_preserves() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfTrim] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let (nid_a, nid_b) = (core_math::scene_graph::NodeId(301), core_math::scene_graph::NodeId(302));
    let grid_a = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let grid_b = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.2));
    let atlas_a = atlas_for_grid(&device, &queue, &grid_a, res, gmin, gmax, nid_a);
    let atlas_b = atlas_for_grid(&device, &queue, &grid_b, res, gmin, gmax, nid_b);

    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false);

    // B meshes normally (its content must survive A's trim rebuild).
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, Some(&mut pool));
    let (b_cnt, b_ind, b_verts) = pool_readback(&device, &queue, &pool, nid_b, &slot_b);
    assert!(b_ind[0] > 0, "body B meshed empty — vacuous");

    // A meshes with a FAT mesher (simulate the transient peak: huge headroom → oversized caps).
    let mut mesher_fat = SdfMeshGpu::new(&device);
    mesher_fat.verts_per_brick_headroom = 200; // ≥4× the measured 22.6+headroom → guaranteed >2× need
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    mesher_fat.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, Some(&mut pool));
    assert!(slot_a.pooled);
    let (a_fat_cnt, _, a_fat_verts) = pool_readback(&device, &queue, &pool, nid_a, &slot_a);
    let fat_vert_cap = pool.slot(nid_a).unwrap().vert_cap;
    let fat_cv_cap = slot_a.cell_vert_cap;

    // QUIET-WINDOW RE-MESH with the normal (settled) headroom + may_trim: must SHRINK.
    slot_a.may_trim = true;
    let gen_before = slot_a.generation;
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, Some(&mut pool));
    assert!(!slot_a.may_trim, "may_trim must be one-shot (consumed)");
    let trimmed_vert_cap = pool.slot(nid_a).unwrap().vert_cap;
    assert!(trimmed_vert_cap < fat_vert_cap / 2,
        "trim must shrink the pool region (fat={fat_vert_cap} → {trimmed_vert_cap})");
    assert!(slot_a.generation > gen_before, "trim relocation must bump the slot generation");
    assert!(slot_a.cell_vert_cap <= fat_cv_cap,
        "cell_vert must not grow across a trim (fat={fat_cv_cap} → {})", slot_a.cell_vert_cap);

    // (b) Counts + geometry identical after the trim (content rewritten by the same dispatch).
    let (a_cnt, a_ind, a_verts) = pool_readback(&device, &queue, &pool, nid_a, &slot_a);
    assert_eq!(a_cnt, a_fat_cnt, "trimmed mesh counts != pre-trim counts");
    assert_eq!(a_ind[0], a_cnt[1], "indirect index_count != counters after trim");
    let (mean, mx) = parity(&a_verts, &a_fat_verts, voxel);
    assert!(mean < 0.001 && mx < 0.01, "trimmed mesh diverged (mean={mean:.5} max={mx:.5})");

    // (c) B's content preserved bit-exactly across A's trim rebuild.
    let (b_cnt2, b_ind2, b_verts2) = pool_readback(&device, &queue, &pool, nid_b, &slot_b);
    assert_eq!(b_cnt2, b_cnt, "body B counts changed across A's trim rebuild");
    assert_eq!(b_ind2[0], b_ind[0], "body B indirect index_count changed across A's trim");
    let (bm, bx) = parity(&b_verts2, &b_verts, voxel);
    assert!(bm < 1e-6 && bx < 1e-6, "body B geometry not preserved bit-exactly (mean={bm} max={bx})");

    // (d) DEADBAND: a second quiet re-mesh at the SAME need must NOT trim again (caps are ~1.25×).
    slot_a.may_trim = true;
    let cap_before = pool.slot(nid_a).unwrap().vert_cap;
    let pool_gen_before = pool.generation;
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, Some(&mut pool));
    assert_eq!(pool.slot(nid_a).unwrap().vert_cap, cap_before,
        "proportionate caps must not shrink (hysteresis deadband)");
    assert_eq!(pool.generation, pool_gen_before,
        "no-op trim must not rebuild the pool (churn)");
    eprintln!("[SdfTrim] OK: pool region {fat_vert_cap} → {trimmed_vert_cap} verts, counts identical, B preserved, deadband holds");
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// C1 — Tiled incremental DC: per-tile regions + per-tile DrawIndexedIndirect entries.
// ───────────────────────────────────────────────────────────────────────────────────────────

/// Read back a TILED body's pool state: per-tile counters, per-tile args entries, per-tile
/// vertex positions, per-tile index arrays. GPU Safety rule: map Results captured.
#[allow(clippy::type_complexity)]
pub(super) fn tiled_readback(
    device: &wgpu::Device, queue: &wgpu::Queue, pool: &SdfMeshPool, nid: core_math::scene_graph::NodeId,
) -> (Vec<[u32; 2]>, Vec<[u32; 5]>, Vec<Vec<Vec3>>, Vec<Vec<u32>>) {
    tiled_readback_seg(device, queue, pool, nid, 0)
}

/// Phase 4b — segment-addressed twin of `tiled_readback` (a multi-group body's tiles live in
/// per-segment slots, each in its own group).
#[allow(clippy::type_complexity)]
pub(super) fn tiled_readback_seg(
    device: &wgpu::Device, queue: &wgpu::Queue, pool: &SdfMeshPool,
    nid: core_math::scene_graph::NodeId, seg: u32,
) -> (Vec<[u32; 2]>, Vec<[u32; 5]>, Vec<Vec<Vec3>>, Vec<Vec<u32>>) {
    let s = pool.slot_seg(nid, seg).expect("tiled slot segment must exist");
    let n = s.tiles.len() as u64;
    assert!(n > 0, "tiled_readback on a body without tiles");
    let mk = |label: &str, size: u64| device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label), size: size.max(4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let s_c = mk("t_c", n * 8);
    let s_a = mk("t_a", n * 20);
    let s_v = mk("t_v", s.vert_region_bytes());
    let s_i = mk("t_i", s.idx_region_bytes());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let grp = pool.group(s.group); // Phase 4: the slot's buffers live in its group
    enc.copy_buffer_to_buffer(&grp.tile_counters, s.args_base as u64 * 8, &s_c, 0, n * 8);
    enc.copy_buffer_to_buffer(&grp.indirect, s.indirect_offset(), &s_a, 0, n * 20);
    enc.copy_buffer_to_buffer(&grp.verts, s.vert_off, &s_v, 0, s.vert_region_bytes());
    enc.copy_buffer_to_buffer(&grp.idx, s.idx_off, &s_i, 0, s.idx_region_bytes());
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    for b in [&s_c, &s_a, &s_v, &s_i] {
        let tx = tx.clone();
        b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    for _ in 0..4 { rx.recv().expect("map dropped").expect("map failed"); }
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    let args: Vec<u32> = bytemuck::cast_slice(&s_a.slice(..).get_mapped_range()).to_vec();
    let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    let ix: Vec<u32> = bytemuck::cast_slice(&s_i.slice(..).get_mapped_range()).to_vec();
    s_c.unmap(); s_a.unmap(); s_v.unmap(); s_i.unmap();
    let mut counters = Vec::new();
    let mut argsv = Vec::new();
    let mut verts = Vec::new();
    let mut idx = Vec::new();
    for (k, t) in s.tiles.iter().enumerate() {
        counters.push([cnt[k * 2], cnt[k * 2 + 1]]);
        argsv.push([args[k * 5], args[k * 5 + 1], args[k * 5 + 2], args[k * 5 + 3], args[k * 5 + 4]]);
        let vc = (cnt[k * 2] as u64).min(t.vert_cap) as usize;
        verts.push((0..vc).map(|i| {
            let o = (t.rel_vert as usize + i) * 8;
            Vec3::new(vf[o], vf[o + 1], vf[o + 2])
        }).collect());
        let ic = (cnt[k * 2 + 1] as u64).min(t.idx_cap) as usize;
        idx.push(ix[t.rel_idx as usize..t.rel_idx as usize + ic].to_vec());
    }
    (counters, argsv, verts, idx)
}

/// C1 GATE — the tiled full re-mesh must match the proven monolithic dispatch: identical TOTAL
/// index count (tile anchor windows partition the surface — every quad emitted exactly once),
/// vertex parity bit-exact-class in BOTH directions (halo vertices are DUPLICATES of the same
/// solved positions, never new geometry), per-tile args self-consistent (index_count ≤ the tile's
/// idx cap; first_index/base_vertex point at the tile's own region; every emitted index < that
/// tile's vertex count — self-contained index spaces, the C2 prerequisite).
#[test]
fn sdf_tiled_full_matches_untiled() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfTiled] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(501);
    // Boolean subtract: creases + a surface spanning many tiles AND brick seams.
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(1.4), 0.18);
        let s = sd_sphere(p - Vec3::new(0.9, 0.9, 0.9), 0.7);
        b.max(-s)
    });
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    assert!(atlas.tile_stats.len() > 8, "scene must span many tiles (got {})", atlas.tile_stats.len());
    let mesher = SdfMeshGpu::new(&device);

    // Monolithic reference (the proven unpooled path).
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas, gmin, gmax, nid, None);
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("m_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let vbytes = slot_ref.verts.size();
    let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("m_v"), size: vbytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_c, 0, 8);
    enc.copy_buffer_to_buffer(&slot_ref.verts, 0, &s_v, 0, vbytes);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    for b in [&s_c, &s_v] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    for _ in 0..2 { rx.recv().unwrap().unwrap(); }
    let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    s_c.unmap(); s_v.unmap();
    let (ref_vc, ref_ic) = (cnt[0], cnt[1]);
    let ref_verts: Vec<Vec3> = (0..ref_vc as usize).map(|i| { let o = i * 8; Vec3::new(vf[o], vf[o+1], vf[o+2]) }).collect();
    assert!(ref_ic > 0, "monolithic reference meshed empty — vacuous");

    // Tiled production dispatch into a shared pool.
    let mut pool = SdfMeshPool::new(&device, false); // headless: fallback args (first_instance=0)
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None),
        "tiled dispatch must succeed at this size");
    assert!(slot.pooled && slot.tiled, "tiled dispatch must mark the slot pooled+tiled");
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert_eq!(slot.poll_tile_overflow().map(|o| o.len()), Some(0),
        "default headroom must not overflow this scene");

    let (counters, args, tile_verts, tile_idx) = tiled_readback(&device, &queue, &pool, nid);
    let ps = pool.slot(nid).unwrap();
    let n = ps.tiles.len();
    // Per-tile args self-consistency + self-contained index spaces.
    let mut sum_ic = 0u64;
    for (k, t) in ps.tiles.iter().enumerate() {
        let [vc, ic] = counters[k];
        assert!(vc as u64 <= t.vert_cap, "tile {k}: vert counter {vc} exceeds cap {} (overflow)", t.vert_cap);
        assert!(ic as u64 <= t.idx_cap, "tile {k}: index counter exceeds cap");
        let (vb, ib) = ps.tile_abs(t);
        assert_eq!(args[k][0], ic, "tile {k}: args index_count != counters");
        assert_eq!(args[k][1], 1, "tile {k}: args instance_count must be 1 (draw-list visibility)");
        assert_eq!(args[k][2] as u64, ib, "tile {k}: first_index != tile region base");
        assert_eq!(args[k][3] as u64, vb, "tile {k}: base_vertex != tile region base");
        assert_eq!(args[k][4], 0, "tile {k}: first_instance must be 0 on the fallback pool");
        for &i in &tile_idx[k] {
            assert!((i as u64) < vc as u64, "tile {k}: emitted index {i} out of its own vertex range {vc}");
        }
        sum_ic += ic as u64;
    }
    // THE partition invariant: every quad emitted exactly once across tiles.
    assert_eq!(sum_ic, ref_ic as u64,
        "Σ per-tile index_count ({sum_ic}) != monolithic index_count ({ref_ic}) — quads dropped or duplicated");
    // Vertex parity BOTH directions (tiled has halo duplicates at identical positions).
    let all_tiled: Vec<Vec3> = tile_verts.iter().flatten().copied().collect();
    assert!(all_tiled.len() >= ref_verts.len(), "tiled vertex total below monolithic — vertices missing");
    let (m1, x1) = parity(&all_tiled, &ref_verts, voxel);
    let (m2, x2) = parity(&ref_verts, &all_tiled, voxel);
    eprintln!("[SdfTiled] parity: tiles={n} tiled_verts={} mono_verts={ref_vc} idx={sum_ic} | \
        tiled→mono mean={m1:.5} max={x1:.5} | mono→tiled mean={m2:.5} max={x2:.5} (voxels)",
        all_tiled.len());
    assert!(m1 < 0.001 && x1 < 0.01, "tiled→mono vertex divergence (mean={m1:.5} max={x1:.5})");
    assert!(m2 < 0.001 && x2 < 0.01, "mono→tiled vertex divergence (mean={m2:.5} max={x2:.5})");
}

/// C1 GATE — multi-body tiled pool: (a) two tiled bodies coexist; (b) a per-tile OVERFLOW (tiny
/// headroom) is detected by the tile-counters readback, floors raise ONLY the hit tiles, and the
/// re-mesh lands Σ index_count identical to the ample reference; (c) body B's tiles are preserved
/// BIT-EXACT across body A's region growth/rebuild; (d) draw prep semantics: the per-pass draw
/// list gates the per-body multi-draw (clear → empty; mark → listed once; a freed body drops out).
#[test]
fn sdf_tiled_pool_multi_body_grow_and_visibility() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfTiled] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let (nid_a, nid_b) = (core_math::scene_graph::NodeId(601), core_math::scene_graph::NodeId(602));
    let grid_a = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let grid_b = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.2));
    let atlas_a = atlas_for_grid(&device, &queue, &grid_a, res, gmin, gmax, nid_a);
    let atlas_b = atlas_for_grid(&device, &queue, &grid_b, res, gmin, gmax, nid_b);
    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false);

    // Ample reference for A (monolithic, unpooled).
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas_a, gmin, gmax, nid_a, None);
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ga_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_c, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().unwrap().unwrap();
    let ref_counts: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    s_c.unmap();

    // B tiled first (must survive A's growth).
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, &mut pool, None));
    let (b_cnt, b_args, b_verts, _) = tiled_readback(&device, &queue, &pool, nid_b);
    assert!(b_args.iter().any(|a| a[0] > 0), "body B meshed empty — vacuous");

    // A tiled with a TINY headroom — deterministic per-tile overflow. (Tiled sizing reads the
    // per-tile field since the C2 envelope follow-up split it from the monolithic headroom.)
    let mut mesher_tiny = SdfMeshGpu::new(&device);
    mesher_tiny.tile_verts_per_brick_headroom = 1;
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher_tiny.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, &mut pool, None));
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let over = slot_a.poll_tile_overflow().expect("readback must complete");
    assert!(!over.is_empty(), "tiny headroom must overflow at least one tile");
    let floors_before = slot_a.tile_cap_floor.len();
    assert!(slot_a.raise_tile_floors(&over), "floors must rise on first overflow");
    assert_eq!(slot_a.tile_cap_floor.len(), floors_before + over.len(),
        "exactly the overflowed tiles get floors");
    // Re-mesh with the raised floors (the poll_sdf_mesh_output_overflow arm) — same tiny mesher.
    assert!(mesher_tiny.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, &mut pool, None));
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert_eq!(slot_a.poll_tile_overflow().map(|o| o.len()), Some(0),
        "raised floors must clear the overflow");
    let (a_cnt, ..) = tiled_readback(&device, &queue, &pool, nid_a);
    let sum_ic: u64 = a_cnt.iter().map(|c| c[1] as u64).sum();
    assert_eq!(sum_ic, ref_counts[1] as u64, "post-grow tiled Σ index_count != ample reference");

    // (c) B preserved bit-exact across A's growth (region append/rebuild).
    let (b_cnt2, b_args2, b_verts2, _) = tiled_readback(&device, &queue, &pool, nid_b);
    assert_eq!(b_cnt2, b_cnt, "body B tile counters changed across A's growth");
    for (k, (a1, a2)) in b_args.iter().zip(b_args2.iter()).enumerate() {
        assert_eq!(a1[0], a2[0], "body B tile {k} index_count changed across A's growth");
    }
    let flat_b: Vec<Vec3> = b_verts.iter().flatten().copied().collect();
    let flat_b2: Vec<Vec3> = b_verts2.iter().flatten().copied().collect();
    assert_eq!(flat_b.len(), flat_b2.len());
    let (bm, bx) = parity(&flat_b2, &flat_b, voxel);
    assert!(bm < 1e-6 && bx < 1e-6, "body B geometry not bit-preserved (mean={bm} max={bx})");

    // (d) Draw-prep semantics: the per-pass draw list is the visibility gate.
    pool.clear_frame_visibility(&queue);
    assert!(pool.frame_drawn().is_empty(), "clear must empty the draw list");
    let inst = render::mesh_gpu::InstanceData::from_transform(
        Vec3::ZERO, Vec3::ZERO, Vec3::ONE, 0, 0);
    pool.mark_drawn(&queue, nid_a, &inst);
    pool.mark_drawn(&queue, nid_a, &inst); // idempotent
    assert_eq!(pool.frame_drawn(), &[nid_a], "mark_drawn must list the body exactly once");
    assert!(pool.slot(nid_a).unwrap().draw_entries() > 1, "tiled body must draw as N entries");
    pool.free_slot(&queue, nid_a);
    assert!(pool.frame_drawn().is_empty(), "a freed body must drop out of the draw list");
    assert!(pool.slot(nid_a).is_none());
    eprintln!("[SdfTiled] multi-body OK: A tiles overflowed {} → floors → Σidx={} == ref; B bit-preserved; draw list gates",
        over.len(), sum_ic);
}

/// C2 GATE — (a) a dirty window touching NO tiles dispatches ZERO tiles and leaves the mesh
/// bit-exact; (b) a tile GROW inside the dirty set (forced via a raised cap floor — the overflow
/// path's shape) re-allocates ONLY that tile (tail-append into the region's slack): neighbors'
/// entries, args, and vertex bytes are bit-preserved, per-tile index_counts and Σidx unchanged.
#[test]
fn sdf_tiled_incremental_empty_window_and_grow_isolation() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfTiledInc] no GPU adapter — skipping"); return; }
    };
    // res 128 (4 tiles/axis) with the body INSIDE the middle tiles: the grid corner holds NO
    // live tile, so a corner window has a genuinely empty tile intersection.
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(701);
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(0.9), 0.18);
        let s = sd_sphere(p - Vec3::new(0.7, 0.7, 0.7), 0.55);
        b.max(-s)
    });
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    assert!(atlas.tile_stats.iter().all(|st| st.tile != [0, 0, 0]),
        "premise: the grid-corner tile must hold no surface");
    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false);
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None));
    let n_total = pool.slot(nid).unwrap().tiles.len() as u32;
    assert!(n_total >= 8, "scene must span several tiles");
    assert_eq!(slot.last_dispatched_tiles, (n_total, n_total), "full mesh dispatches every tile");
    let (cnt0, args0, verts0, idx0) = tiled_readback(&device, &queue, &pool, nid);

    // (a) EMPTY intersection: a grid-corner window with no surface tiles → zero dispatches,
    // everything (counters, args, indices, vertex BITS) unchanged.
    let dirty = crate::sdf_gpu_mesh::SdfTileDirty::Voxels([0, 0, 0], [2, 2, 2]);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, Some(&dirty)));
    assert_eq!(slot.last_dispatched_tiles, (0, n_total), "no-tile window must dispatch ZERO tiles");
    let (cnt1, args1, verts1, idx1) = tiled_readback(&device, &queue, &pool, nid);
    assert_eq!(cnt1, cnt0, "empty-window re-mesh changed counters");
    assert_eq!(args1, args0, "empty-window re-mesh changed args");
    assert_eq!(idx1, idx0, "empty-window re-mesh changed indices");
    for (k, (a, b)) in verts0.iter().zip(verts1.iter()).enumerate() {
        assert_eq!(a.len(), b.len(), "tile {k}: vertex count changed on a no-op");
        for (va, vb) in a.iter().zip(b.iter()) {
            assert_eq!(va.to_array().map(f32::to_bits), vb.to_array().map(f32::to_bits),
                "tile {k}: vertex bits changed on a no-op re-mesh");
        }
    }

    // (b) GROW isolation: force need > cap on one mid-surface tile via its overflow floor and
    // dirty-set exactly that tile (the poll_sdf_mesh_output_overflow C2 shape).
    let tiles_before: Vec<crate::sdf_gpu_mesh::SdfTileEntry> = pool.slot(nid).unwrap().tiles.clone();
    let grow_pos = tiles_before.len() / 2;
    let old_entry = tiles_before[grow_pos];
    let grow_key = crate::sdf_gpu_mesh::tile_key(old_entry.tile);
    let floor = ((old_entry.vert_cap * 2).min(crate::sdf_gpu_mesh::SDF_TILE_MAX_VERTS)) as u32;
    assert!((floor as u64) > old_entry.vert_cap, "floor must force a grow");
    slot.tile_cap_floor.insert(grow_key, floor);
    let dirty = crate::sdf_gpu_mesh::SdfTileDirty::Tiles(vec![grow_key]);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, Some(&dirty)));
    assert_eq!(slot.last_dispatched_tiles, (1, n_total), "grow must re-dispatch exactly the grown tile");
    let (wm_v_before, _) = {
        // Old packing high-water from the PRE-grow table (tail-append target).
        let wm = tiles_before.iter().fold((0u64, 0u64), |(v, i), t| {
            (v.max(t.rel_vert + t.vert_cap), i.max(t.rel_idx + t.idx_cap))
        });
        wm
    };
    {
        let s = pool.slot(nid).unwrap();
        let t = &s.tiles[grow_pos];
        assert_eq!(t.vert_cap, floor as u64, "grown tile must land at its floor");
        assert!(t.rel_vert >= wm_v_before, "grow must TAIL-append, never overwrite live regions");
        for (i, e) in s.tiles.iter().enumerate() {
            if i == grow_pos { continue; }
            let o = &tiles_before[i];
            assert!(e.rel_vert == o.rel_vert && e.vert_cap == o.vert_cap
                && e.rel_idx == o.rel_idx && e.idx_cap == o.idx_cap,
                "clean tile {i}: region moved by a neighbor's grow");
        }
    }
    let (cnt2, args2, verts2, idx2) = tiled_readback(&device, &queue, &pool, nid);
    let sum0: u64 = cnt0.iter().map(|c| c[1] as u64).sum();
    let sum2: u64 = cnt2.iter().map(|c| c[1] as u64).sum();
    assert_eq!(sum2, sum0, "Σ index_count changed across a same-field grow");
    for (k, (c2, c0)) in cnt2.iter().zip(cnt0.iter()).enumerate() {
        assert_eq!(c2, c0, "tile {k}: counters changed across a same-field grow");
    }
    for (k, (a2, a1)) in args2.iter().zip(args1.iter()).enumerate() {
        if k == grow_pos {
            assert_eq!(a2[0], a1[0], "grown tile: index_count must be field-identical");
        } else {
            assert_eq!(a2, a1, "clean tile {k}: args changed by a neighbor's grow");
        }
    }
    for (k, (v2, v1)) in verts2.iter().zip(verts1.iter()).enumerate() {
        if k == grow_pos {
            // Re-dispatched: same geometry, emission ORDER may differ — spatial parity, not bits.
            assert_eq!(v2.len(), v1.len(), "grown tile: vertex count changed");
            let (m, x) = parity(v2, v1, voxel);
            assert!(m < 1e-6 && x < 1e-6, "grown tile geometry diverged (mean={m} max={x})");
        } else {
            assert_eq!(idx2[k], idx1[k], "clean tile {k}: indices changed");
            for (va, vb) in v2.iter().zip(v1.iter()) {
                assert_eq!(va.to_array().map(f32::to_bits), vb.to_array().map(f32::to_bits),
                    "clean tile {k}: vertex bits changed by a neighbor's grow");
            }
        }
    }
    eprintln!("[SdfTiledInc] empty-window no-op + grow isolation OK: {n_total} tiles, grew tile {grow_pos} \
        {}→{} verts, neighbors bit-preserved", old_entry.vert_cap, floor);
}

/// C2 envelope follow-up gate (2026-07-16): the tiled path must FIT the pool ceiling on realistic
/// 1024 surfaces — the C1 envelope finding was Σ per-tile caps at the monolithic 48 v/brick
/// packing 41.6M vert caps (1.33 GB > 1 GiB) for measured scene-A@1024 (866,667 halo bricks),
/// forcing a monolithic fallback exactly where the incremental win matters. Pure-math pin against
/// the measured constants: if someone raises TILE_VERTS_PER_BRICK_HEADROOM (or shrinks the pool
/// cap) past the point where that scene falls back again, this fails loudly with the numbers.
#[test]
fn tile_headroom_fits_measured_1024_envelope() {
    const MEASURED_SCENE_A_1024_HALO_BRICKS: u64 = 41_600_000 / 48; // 866,667 (2026-07-16 log)
    let vert_caps = MEASURED_SCENE_A_1024_HALO_BRICKS * TILE_VERTS_PER_BRICK_HEADROOM;
    let vert_bytes = vert_caps * 32;
    let idx_bytes = vert_caps * 6 * 4;
    assert!(vert_bytes <= SDF_POOL_MAX_BUFFER_BYTES,
        "tile vert caps ({:.0}MB) burst the pool ceiling — realistic 1024 scenes would fall back \
         to monolithic and lose the C2 incremental win", vert_bytes as f64 / 1.0e6);
    assert!(idx_bytes <= SDF_POOL_MAX_BUFFER_BYTES,
        "tile idx caps ({:.0}MB) burst the pool ceiling", idx_bytes as f64 / 1.0e6);
    // And the headroom must stay ABOVE the measured density (~22.6 v/brick) or real surfaces
    // would overflow every tile on first mesh (churn, not safety).
    assert!(TILE_VERTS_PER_BRICK_HEADROOM >= 24,
        "tile headroom below measured density — every tile overflows on first mesh");
}

/// O2 LOD GATE — a level-1 mesh extracted from the SAME brick atlas (no re-bake/re-classify):
/// (a) non-empty and ~¼ the triangles of level 0 (surface area in cells shrinks 4× per level);
/// (b) geometry within coarse tolerance of the level-0 surface (every L1 vertex within 2 L1-cells
///     of an L0 vertex — the resolution loss bound, NOT a parity claim);
/// (c) a subsequent level-0 dispatch of the SAME slot restores the full mesh bit-comparable to a
///     fresh L0 reference (LOD switching cannot poison the slot).
#[test]
fn sdf_lod_level1_meshes_from_same_atlas() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfLod] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(401);
    let grid = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.7));
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    let mesher = SdfMeshGpu::new(&device);

    let read_slot = |slot: &SdfBodyMeshSlot| -> ([u32; 2], Vec<Vec3>) {
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("l_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let vbytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("l_v"), size: vbytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vbytes);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        for b in [&s_c, &s_v] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        for _ in 0..2 { rx.recv().unwrap().unwrap(); }
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        s_c.unmap(); s_v.unmap();
        let verts: Vec<Vec3> = (0..cnt[0] as usize).map(|i| { let o = i * 8; Vec3::new(vf[o], vf[o+1], vf[o+2]) }).collect();
        ([cnt[0], cnt[1]], verts)
    };

    // Level-0 reference.
    let mut slot0 = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot0, &atlas, gmin, gmax, nid, None);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let (c0, v0) = read_slot(&slot0);
    assert!(c0[1] > 0, "L0 reference empty — vacuous");
    assert_eq!(slot0.lod_level, 0);

    // Level-1 from the SAME atlas.
    let mut slot1 = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse_lod(&device, &queue, &mut slot1, &atlas, gmin, gmax, 1, None);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let (c1, v1) = read_slot(&slot1);
    assert!(c1[1] > 0, "L1 mesh empty — the stride path produced nothing");
    assert_eq!(slot1.lod_level, 1, "slot must record its LOD level");
    let ratio = c0[1] as f64 / c1[1] as f64;
    assert!((2.5..=6.5).contains(&ratio),
        "L1 triangle count should be ~¼ of L0 (idx ratio {ratio:.2}, L0={} L1={})", c0[1], c1[1]);
    // (b) resolution-loss bound: every L1 vertex within 2 L1-cells of the L0 surface.
    let tol = voxel * 2.0 * 2.0; // 2 cells × stride 2
    let (mean, mx) = parity(&v1, &v0, voxel);
    let _ = mean;
    assert!(mx * voxel <= tol + 1e-4,
        "L1 vertex strayed {:.4} world units from the L0 surface (tol {:.4})", mx * voxel, tol);

    // (c) LOD switching cannot poison the slot: re-mesh slot1 at level 0 → counts match slot0.
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot1, &atlas, gmin, gmax, nid, None);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let (c_back, v_back) = read_slot(&slot1);
    assert_eq!(c_back, c0, "level-0 re-mesh after LOD must restore the exact full mesh counts");
    assert_eq!(slot1.lod_level, 0, "level-0 dispatch must reset lod_level");
    let (bm, bx) = parity(&v_back, &v0, voxel);
    assert!(bm < 1e-6 && bx < 1e-6, "restored L0 mesh diverged (mean={bm} max={bx})");

    eprintln!("[SdfLod] OK: L0 idx={} L1 idx={} (ratio {ratio:.2}), L1 within {:.3}w of L0, L0 restore bit-exact",
        c0[1], c1[1], tol);
}

/// O2b gate: the WINDOWED LOD dispatch (surface-brick bbox, the production path) produces the
/// SAME mesh as the O2a full-grid LOD sweep — the window covers every level cell that can emit,
/// so the output is identical (counts + vertex parity). Small OFF-CENTER body in a large grid so
/// the window is a real fraction of the grid (non-vacuous), at levels 1 and 2.
#[test]
fn sdf_lod_windowed_matches_full_grid() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfLod] no GPU adapter — skipping windowed parity"); return; }
    };
    let res = 129u32; // 128 cells — lod_max_level_for_res = 2, both levels exercisable
    let (gmin, gmax) = (Vec3::splat(-4.0), Vec3::splat(4.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(402);
    let center = Vec3::new(1.1, -0.7, 0.4); // off-center: asymmetric window on all sides
    let grid = bake_grid(res, gmin, gmax, |p| sd_round_box(p - center, Vec3::splat(0.55), 0.12));
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    let mesher = SdfMeshGpu::new(&device);

    // Non-vacuous: the surface window must be a strict subset of the grid.
    let (wmin, wmax) = crate::sdf_gpu_mesh::surface_cell_window(&atlas, res)
        .expect("surface exists");
    assert!(wmax[0] - wmin[0] + 1 < res - 1, "vacuous: window spans the whole grid");

    let read_slot = |slot: &SdfBodyMeshSlot| -> ([u32; 2], Vec<Vec3>) {
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("wl_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let vbytes = slot.verts.size();
        let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("wl_v"), size: vbytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        enc.copy_buffer_to_buffer(&slot.verts, 0, &s_v, 0, vbytes);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        for b in [&s_c, &s_v] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        for _ in 0..2 { rx.recv().unwrap().unwrap(); }
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        let vf: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
        s_c.unmap(); s_v.unmap();
        let verts: Vec<Vec3> = (0..cnt[0] as usize).map(|i| { let o = i * 8; Vec3::new(vf[o], vf[o+1], vf[o+2]) }).collect();
        ([cnt[0], cnt[1]], verts)
    };

    for level in [1u32, 2] {
        // Full-grid LOD sweep (the O2a behavior) — explicit full window.
        let mut slot_full = SdfBodyMeshSlot::new(&device, res);
        mesher.mesh_into_slot_sparse_lod(&device, &queue, &mut slot_full, &atlas, gmin, gmax,
            level, Some(crate::sdf_gpu_mesh::lod_full_window(res)));
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let (cf, vf) = read_slot(&slot_full);
        assert!(cf[1] > 0, "L{level} full-grid mesh empty — vacuous");

        // Windowed (production default).
        let mut slot_win = SdfBodyMeshSlot::new(&device, res);
        mesher.mesh_into_slot_sparse_lod(&device, &queue, &mut slot_win, &atlas, gmin, gmax, level, None);
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let (cw, vw) = read_slot(&slot_win);
        assert_eq!(slot_win.lod_level, level);

        assert_eq!(cw, cf, "L{level}: windowed counts != full-grid counts (window dropped/duplicated surface)");
        let (mean, mx) = parity(&vw, &vf, voxel);
        assert!(mean < 1e-6 && mx < 1e-6,
            "L{level}: windowed vertices diverged from full-grid (mean={mean} max={mx})");
        eprintln!("[SdfLod] windowed==full @L{level}: v={} i={} (window {:?}..{:?} of {} cells)",
            cw[0], cw[1], wmin, wmax, res - 1);
    }
}

/// P1 REGRESSION (2026-07-16, "Auto SDF mesh LOD (O2b) makes a sparse body VANISH"): the
/// production sequence is a body whose CURRENT mesh lives in the shared pool (the tiled sparse
/// path — `mesh_into_slot_sparse_tiled` mirrors the POOL region caps into `slot.vert_cap`/
/// `slot.idx_cap` at resident.rs:567 while the slot's OWN `verts`/`idx` are still the 4-byte
/// placeholders from `SdfBodyMeshSlot::new`), then the O2b policy switches it to L1 through the
/// UNPOOLED LOD entry. `ensure_output_caps` used to judge sufficiency against the logical caps —
/// the stale POOL numbers — so the L1 request (¼ the L0 need, always below the pool's 2.5×-churn
/// region caps) no-op'd and the dispatch bound the 4-byte placeholders as its output: every
/// vertex write dropped by the arrayLength guard → all cells NO_VERT → zero quads → indirect
/// index_count 0 → the body draws NOTHING. Same class hits the monolithic pool-cap fallback
/// (`mesh_into_slot_sparse` Err arm) whenever its need is under the stale pool caps.
/// The gate: after a pooled tiled mesh, an L1 LOD dispatch on the SAME slot must produce the
/// exact counts a FRESH slot produces, and the slot's own buffers must actually hold them.
#[test]
fn sdf_lod_after_pooled_tiled_slot_not_empty() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfLod] no GPU adapter — skipping pooled→LOD regression"); return; }
    };
    let res = 129u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let nid = core_math::scene_graph::NodeId(403);
    let grid = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.7));
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    let mesher = SdfMeshGpu::new(&device);

    let read_counters = |slot: &SdfBodyMeshSlot| -> [u32; 2] {
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pl_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        rx.recv().unwrap().unwrap();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        s_c.unmap();
        [cnt[0], cnt[1]]
    };

    // ── Production shape: the slot's current mesh is POOLED TILED — logical caps mirror the
    // pool region, own verts/idx are still the placeholders.
    let mut pool = SdfMeshPool::new(&device, false);
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None),
        "premise: the tiled pool dispatch must succeed at this size");
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert!(slot.pooled && slot.tiled, "premise: mesh lives in the pool");
    assert!(slot.vert_cap > 0 && slot.idx_cap > 0, "premise: logical caps mirror the POOL region");
    assert_eq!(slot.verts.size(), 4, "premise: own verts is still the 4-byte placeholder");
    assert_eq!(slot.idx.size(), 4, "premise: own idx is still the 4-byte placeholder");

    // ── The O2b policy switch: L1 through the unpooled LOD entry on the SAME slot.
    mesher.mesh_into_slot_sparse_lod(&device, &queue, &mut slot, &atlas, gmin, gmax, 1, None);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let c_lod = read_counters(&slot);

    // Fresh-slot L1 reference (the sdf_lod_level1_meshes_from_same_atlas shape — known good).
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse_lod(&device, &queue, &mut slot_ref, &atlas, gmin, gmax, 1, None);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let c_ref = read_counters(&slot_ref);
    assert!(c_ref[1] > 0, "vacuous: fresh-slot L1 reference is empty");

    // The bug: c_lod == [N, 0] (vertex counter ticked, every write dropped, zero quads) and
    // slot.verts still 4 bytes — the drawn body VANISHES.
    assert!(slot.verts.size() > 4 && slot.idx.size() > 4,
        "LOD dispatch after a pooled era bound the 4-byte placeholder outputs (verts={}B idx={}B) — \
         stale POOL region caps masked the own-buffer grow", slot.verts.size(), slot.idx.size());
    assert_eq!(c_lod, c_ref,
        "pooled→LOD dispatch diverged from the fresh-slot L1 reference (lod={c_lod:?} ref={c_ref:?}) — empty/truncated mesh = the vanish bug");
    assert_eq!(slot.lod_level, 1);
    assert!(!slot.pooled, "LOD entry is unpooled by design");
    eprintln!("[SdfLod] pooled→LOD regression OK: v={} i={} (own verts now {}B)",
        c_lod[0], c_lod[1], slot.verts.size());
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// O3 PAYOFF MEASUREMENT (2026-07-16) — per-tile LOD headroom vs per-body O2b.
//
// Pure-CPU (classify → tile_occupancy → grid-gradient curvature). No GPU: the artifact is the
// tile-level histogram + the tri-reduction ratios, all derivable from the sparse index + the
// baked grid. Answers question 2 of the D1/D2 measure-first gate: does per-TILE level (projected
// size + local curvature per 32³-cell tile) beat per-BODY O2b (one level per body) by enough to
// justify O3's crack/seam complexity? Run:
//   cargo test -p app --bins o3_per_tile_lod_payoff -- --ignored --nocapture --test-threads=1
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// Projected pixels of ONE voxel at a given world distance (mirrors `lod_policy` perspective
/// approximation: voxel_world · (h/2) / (dist · tan(fov/2))).
fn projected_voxel_px(voxel_world: f32, dist: f32, viewport_h: f32, tan_half_fov: f32) -> f32 {
    if dist <= 1e-6 { return f32::INFINITY; }
    voxel_world * (viewport_h * 0.5) / (dist * tan_half_fov)
}

/// Per-tile curvature proxy = normal spread ∈ [0,1]. Central-difference gradient at each
/// near-surface grid sample inside the tile's brick bbox; accumulate UNIT normals; spread =
/// 1 − |mean|. 0 = perfectly flat, →1 = highly curved / high-frequency. (The brief's suggested
/// "per-tile vertex-normal spread" proxy — principled and cheap.)
fn tile_curvature(
    grid: &[f32], res: u32, band: f32, bmin: [u32; 3], bmax: [u32; 3], brick_dim: u32,
) -> f32 {
    let r = res as i32;
    let at = |x: i32, y: i32, z: i32| -> f32 {
        let xc = x.clamp(0, r - 1); let yc = y.clamp(0, r - 1); let zc = z.clamp(0, r - 1);
        grid[(xc + yc * r + zc * r * r) as usize]
    };
    let mut sum = Vec3::ZERO;
    let mut n = 0u32;
    // Sample voxel centers spanning the tile's brick bbox (step 2 voxels to bound cost).
    let (vlo, vhi) = (
        [bmin[0] * brick_dim, bmin[1] * brick_dim, bmin[2] * brick_dim],
        [(bmax[0] + 1) * brick_dim, (bmax[1] + 1) * brick_dim, (bmax[2] + 1) * brick_dim],
    );
    let step = 2i32;
    let mut z = vlo[2] as i32;
    while z < vhi[2] as i32 {
        let mut y = vlo[1] as i32;
        while y < vhi[1] as i32 {
            let mut x = vlo[0] as i32;
            while x < vhi[0] as i32 {
                if at(x, y, z).abs() <= band {
                    let g = Vec3::new(
                        at(x + 1, y, z) - at(x - 1, y, z),
                        at(x, y + 1, z) - at(x, y - 1, z),
                        at(x, y, z + 1) - at(x, y, z - 1),
                    );
                    if g.length_squared() > 1e-12 { sum += g.normalize(); n += 1; }
                }
                x += step;
            }
            y += step;
        }
        z += step;
    }
    if n == 0 { return 0.0; }
    (1.0 - (sum.length() / n as f32)).clamp(0.0, 1.0)
}

/// Curvature cap: how coarse a tile is ALLOWED to go regardless of projected size. High-frequency
/// tiles keep detail (silhouette / feature preservation), flat tiles may coarsen fully.
fn curvature_max_level(spread: f32, hard_max: u32) -> u32 {
    if spread > 0.18 { 0 } else if spread > 0.07 { 1.min(hard_max) } else { hard_max }
}

#[test]
#[ignore]
fn o3_per_tile_lod_payoff() {
    use graphics::sdf_brick_atlas::{tile_occupancy, SDF_TILE_BRICKS};
    // Representative bodies (world extent ±3 → radius ~3): the brief's three archetypes.
    type Scene = (&'static str, fn(Vec3) -> f32);
    let scenes: [Scene; 3] = [
        // Organic / statue-like: lumpy union of offset spheres (varied curvature everywhere).
        ("organic-statue", |p| {
            let a = sd_sphere(p - Vec3::new(0.0, 0.6, 0.0), 1.7);
            let b = sd_sphere(p - Vec3::new(0.9, -0.8, 0.4), 1.1);
            let c = sd_sphere(p - Vec3::new(-1.0, -0.5, -0.3), 1.0);
            let d = sd_sphere(p - Vec3::new(0.2, 1.7, 0.1), 0.7);
            a.min(b).min(c).min(d)
        }),
        // Mostly-flat / boxy: big rounded box → 6 large flat faces, curvature only at edges.
        ("flat-box", |p| sd_round_box(p, Vec3::splat(2.2), 0.18)),
        // Bevelled mechanical part: rounded box with a bored cylinder (flats + a few curved features).
        ("mech-bevel", |p| {
            let body = sd_round_box(p, Vec3::new(2.2, 1.3, 1.6), 0.12);
            let bore = (Vec3::new(p.x, 0.0, p.z).length() - 0.8).max(p.y.abs() - 2.0);
            body.max(-bore)
        }),
    ];

    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let ext = gmax - gmin;
    let body_center = (gmin + gmax) * 0.5;
    let body_radius = (ext.length()) * 0.5;
    let viewport_h = 1080.0f32;
    let tan_half = (45.0f32.to_radians() * 0.5).tan(); // 45° vertical FOV

    // Camera framings: distance as a multiple of body radius. "fills" = asset in the foreground;
    // "far" = background asset. Camera looks at body_center down −Z (offset gives depth spread).
    let framings: [(&str, f32); 4] = [
        ("near-fills", 2.2),   // near-camera hero
        ("medium", 6.0),       // mid-ground
        ("far", 16.0),         // background
        ("very-far", 40.0),    // distant crowd
    ];

    for res in [512u32, 1024] {
        let voxel_world = ext.min_element() / (res - 1) as f32;
        let cell_world = (ext / (res - 1).max(1) as f32).min_element().max(1e-6);
        let band = 2.0 * cell_world;
        let hard_max = crate::sdf_gpu_mesh::lod_policy::lod_max_level_for_res(res);
        eprintln!("\n════════ O3 PAYOFF @ res={res}  (voxel_world={voxel_world:.5}, tile=32³cells, hard_max_level=L{hard_max}) ════════");
        for (sname, f) in scenes {
            let grid = bake_grid(res, gmin, gmax, f);
            let sb = core_math::chunked_dc::classify_grid_to_sparse(&grid, res, 8, band);
            let brick_dim = sb.brick_dim;
            let tiles = tile_occupancy(&sb.index, sb.dims_bricks);
            let total_bricks: u64 = tiles.iter().map(|t| t.bricks as u64).sum();
            if total_bricks == 0 { eprintln!("  {sname:>14}@{res}: EMPTY — skipped"); continue; }

            for (fname, dist_mult) in framings {
                let cam_dist = body_radius * dist_mult;
                let cam_pos = body_center + Vec3::new(0.0, 0.0, cam_dist);

                // Per-body O2b: one level from the body-CENTER projected voxel size.
                let body_px = projected_voxel_px(voxel_world, cam_dist, viewport_h, tan_half);
                let l_body = crate::sdf_gpu_mesh::lod_policy::choose_lod_level(body_px, 0, hard_max);

                // Per-tile O3: level per tile from ITS projected size, capped by ITS curvature.
                let mut hist = [0u64; 3];          // tile counts per level
                let mut hist_bricks = [0u64; 3];   // brick-weighted (≈ tri weight) per level
                let mut o3_cost = 0.0f64;          // Σ bricks / 4^Ltile
                let mut sum_curv = 0.0f64;
                for t in &tiles {
                    // Tile world center from its handle-brick bbox midpoint.
                    let cb = [
                        (t.bmin[0] + t.bmax[0]) as f32 * 0.5,
                        (t.bmin[1] + t.bmax[1]) as f32 * 0.5,
                        (t.bmin[2] + t.bmax[2]) as f32 * 0.5,
                    ];
                    let cvox = Vec3::new(cb[0], cb[1], cb[2]) * brick_dim as f32
                        + Vec3::splat(brick_dim as f32 * 0.5);
                    let cw = gmin + (cvox / (res - 1) as f32) * ext;
                    let tdist = (cam_pos - cw).length();
                    let tpx = projected_voxel_px(voxel_world, tdist, viewport_h, tan_half);
                    let curv = tile_curvature(&grid, res, band, t.bmin, t.bmax, brick_dim);
                    sum_curv += curv as f64;
                    let cap = curvature_max_level(curv, hard_max);
                    let l_proj = crate::sdf_gpu_mesh::lod_policy::choose_lod_level(tpx, 0, hard_max);
                    let l_tile = l_proj.min(cap);
                    hist[l_tile as usize] += 1;
                    hist_bricks[l_tile as usize] += t.bricks as u64;
                    o3_cost += t.bricks as f64 / 4f64.powi(l_tile as i32);
                }
                let full_cost = total_bricks as f64;                       // all L0
                let o2b_cost = total_bricks as f64 / 4f64.powi(l_body as i32);
                let mean_curv = sum_curv / tiles.len() as f64;

                eprintln!(
                    "  {sname:>14} [{fname:>9}] d={cam_dist:5.1} bodypx={body_px:6.3}=L{l_body} | \
tiles={:4} curv̄={mean_curv:.3} | hist L0/L1/L2 = {:>4}/{:>4}/{:>4} \
(bricks {:>6}/{:>6}/{:>6}) | tris×: full=1.00 o2b={:.3} o3={:.3} | o3-vs-o2b={:.2}x o3-vs-full={:.2}x",
                    tiles.len(), hist[0], hist[1], hist[2],
                    hist_bricks[0], hist_bricks[1], hist_bricks[2],
                    o2b_cost / full_cost, o3_cost / full_cost,
                    o2b_cost / o3_cost, full_cost / o3_cost,
                );
            }
        }
    }
    eprintln!("\n[O3-NOTE] 'o3-vs-o2b' >1 = O3 fewer tris than whole-body O2b at the SAME framing \
(O3 also keeps near/curved tiles at L0 = higher quality). 'o3-vs-full' = reduction vs full-res. \
The verdict metric is whether tiles SPLIT across levels within one body (mixed histogram) at \
representative framings — a single-level histogram means per-body O2b already captures the win.");
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// Phase 4 — multi-buffer pool: buffer GROUPS (per-group rebuild/generation, total budget).
// ───────────────────────────────────────────────────────────────────────────────────────────

/// PHASE-4 GATE (a)+(b): two tiled bodies whose combined footprint exceeds ONE group buffer land
/// in TWO groups, BOTH pooled+tiled, each with Σ per-tile index_count identical to its unpooled
/// monolithic reference; then growing the second body rebuilds ONLY its group — the first
/// group's generation is untouched and the first body's tiled geometry is bit-preserved.
/// `max_group_bytes` is the test's deterministic forcing knob (the headroom-shrink pattern);
/// production keeps SDF_POOL_MAX_BUFFER_BYTES.
#[test]
fn sdf_pool_multi_group_two_bodies_tiled_parity_and_isolated_rebuild() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfPoolMG] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let (nid_a, nid_b) = (core_math::scene_graph::NodeId(901), core_math::scene_graph::NodeId(902));
    let grid_a = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let grid_b = bake_grid(res, gmin, gmax, |p| sd_sphere(p, 1.3));
    let atlas_a = atlas_for_grid(&device, &queue, &grid_a, res, gmin, gmax, nid_a);
    let atlas_b = atlas_for_grid(&device, &queue, &grid_b, res, gmin, gmax, nid_b);
    let mesher = SdfMeshGpu::new(&device);

    // Unpooled monolithic references (Σ tiled index_count must equal these — the C1 invariant).
    let mut refs = [0u32; 2];
    for (k, (nid, atlas)) in [(nid_a, &atlas_a), (nid_b, &atlas_b)].iter().enumerate() {
        let mut slot = SdfBodyMeshSlot::new(&device, res);
        mesher.mesh_into_slot_sparse(&device, &queue, &mut slot, atlas, gmin, gmax, *nid, None);
        let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mg_c"), size: 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&slot.counters, 0, &s_c, 0, 8);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        rx.recv().unwrap().unwrap();
        let cnt: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
        s_c.unmap();
        assert!(cnt[1] > 0, "reference {k} meshed empty — vacuous");
        refs[k] = cnt[1];
    }

    // A tiled at production knobs → group 0.
    let mut pool = SdfMeshPool::new(&device, false);
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, &mut pool, None));
    assert!(slot_a.pooled && slot_a.tiled);
    let ga = pool.slot(nid_a).unwrap().group;

    // Shrink the per-group cap to EXACTLY what group ga holds now: the combined-rebuild need
    // (A+B) cannot fit one group, but B alone can → B must land in a NEW group.
    let grp_a = pool.group(ga);
    pool.max_group_bytes = grp_a.verts.size().max(grp_a.idx.size());
    let mut slot_b = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, &mut pool, None),
        "B must stay on the POOLED tiled path via a second group — the Phase-4 point");
    assert!(slot_b.pooled && slot_b.tiled, "B fell off the pooled tiled path");
    let gb = pool.slot(nid_b).unwrap().group;
    assert_ne!(ga, gb, "combined footprint exceeds one group buffer — B must get its OWN group");
    assert!(pool.group_count() >= 2, "expected >=2 groups, got {}", pool.group_count());

    // Parity: Σ per-tile index_count == the monolithic reference, per body.
    let (a_cnt, ..) = tiled_readback(&device, &queue, &pool, nid_a);
    let (b_cnt, _b_args, b_verts, _) = tiled_readback(&device, &queue, &pool, nid_b);
    let sum_a: u64 = a_cnt.iter().map(|c| c[1] as u64).sum();
    let sum_b: u64 = b_cnt.iter().map(|c| c[1] as u64).sum();
    assert_eq!(sum_a, refs[0] as u64, "body A (group {ga}) Σ index_count != reference");
    assert_eq!(sum_b, refs[1] as u64, "body B (group {gb}) Σ index_count != reference");

    // (b) GROUP-ISOLATED REBUILD: relax the cap and grow B — only B's group rebuilds.
    pool.max_group_bytes = SDF_POOL_MAX_BUFFER_BYTES;
    let gen_a_before = pool.group(ga).generation;
    let gen_b_before = pool.group(gb).generation;
    let (bv, bi) = {
        let s = pool.slot(nid_b).unwrap();
        (s.vert_cap, s.idx_cap)
    };
    assert_eq!(pool.ensure_slot(&device, &queue, nid_b, bv * 2, bi * 2), Ok(true),
        "grow must relocate B");
    assert_eq!(pool.slot(nid_b).unwrap().group, gb,
        "with the cap relaxed, B must grow INSIDE its own group (current group is first candidate)");
    assert!(pool.group(gb).generation > gen_b_before, "B's group must bump its generation");
    assert_eq!(pool.group(ga).generation, gen_a_before,
        "A's group generation changed — a rebuild leaked across groups (Phase-4 isolation broken)");
    // A's tiled geometry bit-preserved (its group's buffers were never touched).
    let (a_cnt2, ..) = tiled_readback(&device, &queue, &pool, nid_a);
    assert_eq!(a_cnt2, a_cnt, "body A per-tile counters changed across B's group rebuild");
    // B re-meshes into its grown region (requester content is never carried) — counts return.
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, &mut pool, None));
    let (b_cnt2, _b_args2, b_verts2, _) = tiled_readback(&device, &queue, &pool, nid_b);
    let sum_b2: u64 = b_cnt2.iter().map(|c| c[1] as u64).sum();
    assert_eq!(sum_b2, refs[1] as u64, "post-grow re-mesh of B != reference");
    let flat_b: Vec<Vec3> = b_verts.iter().flatten().copied().collect();
    let flat_b2: Vec<Vec3> = b_verts2.iter().flatten().copied().collect();
    let (bm, bx) = parity(&flat_b2, &flat_b, voxel);
    assert!(bm < 0.001 && bx < 0.01, "B's re-meshed geometry diverged (mean={bm} max={bx})");
    eprintln!("[SdfPoolMG] OK: A in g{ga}, B in g{gb} ({} groups), Σidx A={sum_a} B={sum_b} == refs, \
               B grow bumped g{gb} gen {}→{} while g{ga} stayed {} — total pool {:.1}MB",
        pool.group_count(), gen_b_before, pool.group(gb).generation, gen_a_before,
        pool.vram_bytes() as f64 / 1.0e6);
}

/// PHASE-4 GATE (c): a body that cannot fit ANY group (per-group cap) NOR a new group (total
/// budget) falls back to the per-body slot buffers EXACTLY as the single-buffer pool did — same
/// counts as the unpooled reference, `slot.pooled == false`, pool state untouched (no group
/// created, no generation churn).
#[test]
fn sdf_pool_budget_exhausted_falls_back_per_body() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfPoolMG] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    // B is meshed at a HIGHER res so its region need exceeds any group's remaining bump tail —
    // a tail append allocates no new memory, so the budget correctly never blocks it; the gate
    // must present a body that needs NEW allocation everywhere.
    let res_b = 128u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let (nid_a, nid_b) = (core_math::scene_graph::NodeId(911), core_math::scene_graph::NodeId(912));
    let grid_a = bake_grid(res, gmin, gmax, |p| sd_round_box(p, Vec3::splat(1.4), 0.18));
    let grid_b = bake_grid(res_b, gmin, gmax, |p| sd_sphere(p, 1.3));
    let atlas_a = atlas_for_grid(&device, &queue, &grid_a, res, gmin, gmax, nid_a);
    let atlas_b = atlas_for_grid(&device, &queue, &grid_b, res_b, gmin, gmax, nid_b);
    let mesher = SdfMeshGpu::new(&device);
    let mut pool = SdfMeshPool::new(&device, false);

    // A pools normally, then the walls close: per-group cap at A's group size AND total budget
    // at the current allocation — B can neither join a group nor open a new one.
    let mut slot_a = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_a, &atlas_a, gmin, gmax, nid_a, &mut pool, None));
    let ga = pool.slot(nid_a).unwrap().group;
    let grp_a = pool.group(ga);
    pool.max_group_bytes = grp_a.verts.size().max(grp_a.idx.size());
    pool.total_budget_bytes = pool.vram_bytes(); // >= current regions, < regions + another body
    // Consume group ga's bump tail exactly (a tail append allocates no new memory, so the budget
    // correctly never blocks it — B must find NO tail anywhere for the budget gate to be the
    // deciding wall).
    let (tail_v, tail_i) = pool.group_tail_bytes(ga);
    if tail_v >= 512 && tail_i >= 512 {
        let filler = core_math::scene_graph::NodeId(913);
        assert_eq!(pool.ensure_slot(&device, &queue, filler, tail_v / 32 - 8, tail_i / 4 - 64), Ok(true),
            "tail filler must bump-append");
        assert_eq!(pool.slot(filler).unwrap().group, ga, "filler must land in ga's tail");
    }
    let groups_before = pool.group_count();
    let gen_before = pool.generation;

    let mut slot_b = SdfBodyMeshSlot::new(&device, res_b);
    assert!(!mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, &mut pool, None),
        "tiled path must report it cannot host B");
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_b, &atlas_b, gmin, gmax, nid_b, Some(&mut pool));
    assert!(!slot_b.pooled, "budget exhausted → B must fall back to per-body buffers");
    assert!(pool.slot(nid_b).is_none(), "no pool slot may linger for a fallen-back body");
    assert_eq!(pool.group_count(), groups_before, "no group may be created past the budget");
    assert_eq!(pool.generation, gen_before, "a failed placement must not churn the pool");

    // The per-body mesh is REAL: counts match an unpooled reference.
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res_b);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas_b, gmin, gmax, nid_b, None);
    let mk = || device.create_buffer(&wgpu::BufferDescriptor { label: Some("bx_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let (s_b, s_r) = (mk(), mk());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_b.counters, 0, &s_b, 0, 8);
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_r, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    for b in [&s_b, &s_r] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    for _ in 0..2 { rx.recv().unwrap().unwrap(); }
    let cb: Vec<u32> = bytemuck::cast_slice(&s_b.slice(..).get_mapped_range()).to_vec();
    let cr: Vec<u32> = bytemuck::cast_slice(&s_r.slice(..).get_mapped_range()).to_vec();
    s_b.unmap(); s_r.unmap();
    assert!(cr[1] > 0, "reference empty — vacuous");
    assert_eq!(cb, cr, "fallen-back per-body mesh != unpooled reference");
    eprintln!("[SdfPoolMG] budget fallback OK: B per-body (idx={}) with pool untouched ({} groups, gen={})",
        cb[1], pool.group_count(), pool.generation);
}

// ───────────────────────────────────────────────────────────────────────────────────────────
// Phase 4b — per-body MULTI-GROUP tiling (segments): one huge body's tiles span ≥2 groups.
// ───────────────────────────────────────────────────────────────────────────────────────────

/// Quantized position keys for order-independent vertex-set parity (halo duplicates collapse).
fn quant_keys(vs: &[Vec3], voxel: f32) -> std::collections::HashSet<[i64; 3]> {
    let q = (voxel * 0.25).max(1e-6);
    vs.iter().map(|v| [(v.x / q).round() as i64, (v.y / q).round() as i64, (v.z / q).round() as i64]).collect()
}

/// Phase 4b GATE (a) — a body whose tiled packing exceeds ONE group buffer stays POOLED+TILED
/// across ≥2 segments in ≥2 DISTINCT groups, with mesh parity vs the monolithic unpooled
/// reference: Σ per-tile index_count across ALL segments == the reference index_count, every
/// tile's indices self-contained (< its vertex count), and the union of tile vertices matches
/// the reference vertex set (quantized, both directions).
#[test]
fn sdf_tiled_multi_segment_body_parity_across_groups() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfSeg] no GPU adapter — skipping"); return; }
    };
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(801);
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(1.4), 0.18);
        let s = sd_sphere(p - Vec3::new(0.9, 0.9, 0.9), 0.7);
        b.max(-s)
    });
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    assert!(atlas.tile_stats.len() > 8, "scene must span many tiles");
    let mesher = SdfMeshGpu::new(&device);

    // Monolithic unpooled reference.
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas, gmin, gmax, nid, None);
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sg_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let vbytes = slot_ref.verts.size();
    let s_v = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sg_v"), size: vbytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_c, 0, 8);
    enc.copy_buffer_to_buffer(&slot_ref.verts, 0, &s_v, 0, vbytes);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    for b in [&s_c, &s_v] { let tx = tx.clone(); b.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    for _ in 0..2 { rx.recv().unwrap().unwrap(); }
    let cr: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    let vr: Vec<f32> = bytemuck::cast_slice(&s_v.slice(..).get_mapped_range()).to_vec();
    s_c.unmap(); s_v.unmap();
    let ref_idx_count = cr[1] as u64;
    assert!(ref_idx_count > 0, "reference empty — vacuous");
    let ref_verts: Vec<Vec3> = (0..cr[0] as usize)
        .map(|i| Vec3::new(vr[i * 8], vr[i * 8 + 1], vr[i * 8 + 2])).collect();

    // Force multi-segment: shrink the per-group cap so the packing exceeds one group buffer
    // (the test-knob pattern — placement decisions are need/cap ratios, not absolute bytes).
    let total_v: u64 = atlas.tile_stats.iter()
        .map(|st| (st.halo_bricks as u64 * TILE_VERTS_PER_BRICK_HEADROOM).clamp(32, crate::sdf_gpu_mesh::SDF_TILE_MAX_VERTS))
        .sum();
    let mut pool = SdfMeshPool::new(&device, false);
    pool.max_group_bytes = (total_v * 32 * 2 / 3).max(2 << 20);
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None),
        "multi-segment tiled dispatch must stay POOLED+TILED");
    assert!(slot.pooled && slot.tiled);
    let nsegs = pool.segment_count(nid);
    assert!(nsegs >= 2, "packing exceeding one group must partition into >=2 segments (got {nsegs})");
    let groups: std::collections::HashSet<usize> = (0..nsegs)
        .map(|s| pool.slot_seg(nid, s).unwrap().group).collect();
    assert!(groups.len() >= 2, "segments must land in >=2 DISTINCT groups (got {:?})", groups);
    assert_eq!(slot.last_tiled_stamp.unwrap().segments, nsegs, "stamp must carry the segmentation");

    // Parity across ALL segments.
    let mut sum_idx = 0u64;
    let mut all_verts: Vec<Vec3> = Vec::new();
    let mut n_tiles_total = 0usize;
    for seg in 0..nsegs {
        let (cnt, args, verts, idx) = tiled_readback_seg(&device, &queue, &pool, nid, seg);
        n_tiles_total += cnt.len();
        for (k, c) in cnt.iter().enumerate() {
            assert_eq!(args[k][0], c[1], "seg {seg} tile {k}: args index_count != counter");
            for &i in &idx[k] {
                assert!((i as usize) < verts[k].len().max(1),
                    "seg {seg} tile {k}: index {i} escapes its tile's vertex space");
            }
            sum_idx += c[1] as u64;
            all_verts.extend_from_slice(&verts[k]);
        }
    }
    assert_eq!(sum_idx, ref_idx_count,
        "sum of per-tile index_count across {nsegs} segments != monolithic reference");
    let ka = quant_keys(&all_verts, voxel);
    let kb = quant_keys(&ref_verts, voxel);
    let miss_a: usize = kb.difference(&ka).count();
    let miss_b: usize = ka.difference(&kb).count();
    assert!(miss_a == 0 && miss_b == 0,
        "vertex-set parity failed across segments (ref-only={miss_a} seg-only={miss_b})");
    eprintln!("[SdfSeg] multi-segment parity OK: {n_tiles_total} tiles across {nsegs} segments in \
        {} groups, sum_idx={sum_idx} == ref", groups.len());
}

/// Phase 4b GATE (b) — C2 incremental on a MULTI-SEGMENT body: (1) an empty dirty window
/// dispatches ZERO tiles and leaves every segment bit-exact; (2) a single-tile grow in a
/// NON-ZERO segment re-dispatches exactly that tile INTO ITS OWN segment/group — the other
/// segment's counters/args/verts stay bit-identical, Σ index_count unchanged.
#[test]
fn sdf_tiled_multi_segment_incremental_dirty_only() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfSegInc] no GPU adapter — skipping"); return; }
    };
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let nid = core_math::scene_graph::NodeId(802);
    // Body INSIDE the middle tiles (the grid corner holds NO live tile — empty-window premise).
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(0.9), 0.18);
        let s = sd_sphere(p - Vec3::new(0.7, 0.7, 0.7), 0.55);
        b.max(-s)
    });
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    assert!(atlas.tile_stats.iter().all(|st| st.tile != [0, 0, 0]),
        "premise: the grid-corner tile must hold no surface");
    let mesher = SdfMeshGpu::new(&device);
    let total_v: u64 = atlas.tile_stats.iter()
        .map(|st| (st.halo_bricks as u64 * TILE_VERTS_PER_BRICK_HEADROOM).clamp(32, crate::sdf_gpu_mesh::SDF_TILE_MAX_VERTS))
        .sum();
    let mut pool = SdfMeshPool::new(&device, false);
    pool.max_group_bytes = (total_v * 32 * 2 / 3).max(2 << 20);
    let mut slot = SdfBodyMeshSlot::new(&device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None));
    let nsegs = pool.segment_count(nid);
    assert!(nsegs >= 2, "need >=2 segments for this gate (got {nsegs})");
    let n_total: u32 = (0..nsegs).map(|s| pool.slot_seg(nid, s).unwrap().tiles.len() as u32).sum();
    let base: Vec<_> = (0..nsegs).map(|s| tiled_readback_seg(&device, &queue, &pool, nid, s)).collect();

    // (1) empty window: zero dispatches, every segment bit-exact.
    let dirty = crate::sdf_gpu_mesh::SdfTileDirty::Voxels([0, 0, 0], [2, 2, 2]);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, Some(&dirty)));
    assert_eq!(slot.last_dispatched_tiles, (0, n_total), "no-tile window must dispatch ZERO tiles");
    for seg in 0..nsegs {
        let now = tiled_readback_seg(&device, &queue, &pool, nid, seg);
        assert_eq!(now.0, base[seg as usize].0, "seg {seg}: counters changed on a no-op");
        assert_eq!(now.1, base[seg as usize].1, "seg {seg}: args changed on a no-op");
        assert_eq!(now.3, base[seg as usize].3, "seg {seg}: indices changed on a no-op");
    }

    // (2) grow ONE tile of segment 1 via its overflow floor; dirty-set exactly that tile.
    let grow_seg = 1u32;
    let tiles_before = pool.slot_seg(nid, grow_seg).unwrap().tiles.clone();
    let grow_pos = tiles_before.len() / 2;
    let old_entry = tiles_before[grow_pos];
    let grow_key = crate::sdf_gpu_mesh::tile_key(old_entry.tile);
    let floor = ((old_entry.vert_cap * 2).min(crate::sdf_gpu_mesh::SDF_TILE_MAX_VERTS)) as u32;
    assert!((floor as u64) > old_entry.vert_cap, "floor must force a grow");
    slot.tile_cap_floor.insert(grow_key, floor);
    let dirty = crate::sdf_gpu_mesh::SdfTileDirty::Tiles(vec![grow_key]);
    assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, Some(&dirty)));
    assert_eq!(slot.last_dispatched_tiles, (1, n_total),
        "grow must re-dispatch exactly the grown tile (in its own segment)");
    {
        let s = pool.slot_seg(nid, grow_seg).unwrap();
        assert_eq!(s.tiles[grow_pos].vert_cap, floor as u64, "grown tile must land at its floor");
        for (i, e) in s.tiles.iter().enumerate() {
            if i == grow_pos { continue; }
            let o = &tiles_before[i];
            assert!(e.rel_vert == o.rel_vert && e.vert_cap == o.vert_cap,
                "seg {grow_seg} clean tile {i}: region moved by a neighbor's grow");
        }
    }
    // Segment 0 must be BIT-untouched; segment 1's clean tiles too; total idx unchanged.
    let sum_before: u64 = base.iter().flat_map(|b| b.0.iter()).map(|c| c[1] as u64).sum();
    let mut sum_after = 0u64;
    for seg in 0..nsegs {
        let now = tiled_readback_seg(&device, &queue, &pool, nid, seg);
        sum_after += now.0.iter().map(|c| c[1] as u64).sum::<u64>();
        if seg != grow_seg {
            assert_eq!(now.0, base[seg as usize].0, "seg {seg}: counters changed by another segment's grow");
            assert_eq!(now.1, base[seg as usize].1, "seg {seg}: args changed by another segment's grow");
            assert_eq!(now.3, base[seg as usize].3, "seg {seg}: indices changed by another segment's grow");
            for (k, (a, b)) in now.2.iter().zip(base[seg as usize].2.iter()).enumerate() {
                for (va, vb) in a.iter().zip(b.iter()) {
                    assert_eq!(va.to_array().map(f32::to_bits), vb.to_array().map(f32::to_bits),
                        "seg {seg} tile {k}: vertex bits changed by another segment's grow");
                }
            }
        } else {
            for (k, c) in now.0.iter().enumerate() {
                assert_eq!(*c, base[seg as usize].0[k],
                    "seg {seg} tile {k}: counters changed across a same-field grow");
            }
            let (m, x) = parity(&now.2[grow_pos], &base[seg as usize].2[grow_pos], voxel);
            assert!(m < 1e-6 && x < 1e-6, "grown tile geometry diverged (mean={m} max={x})");
        }
    }
    assert_eq!(sum_after, sum_before, "total index_count changed across a same-field grow");
    eprintln!("[SdfSegInc] multi-segment incremental OK: {n_total} tiles / {nsegs} segments, \
        grew seg {grow_seg} tile {grow_pos} {}->{}, other segment bit-preserved", old_entry.vert_cap, floor);
}

/// Phase 4b GATE (c) — the >dense-ceiling surface either MESHES FULLY (preferred: the
/// multi-segment path has no dense-ceiling clamp) or degrades LOUDLY: over the pool budget the
/// tiled dispatch refuses with the `envelope_over_budget` latch armed (→ toast at the poll
/// layer) and the monolithic fallback lands cleanly (stale segments retired — nothing stale can
/// draw). Never silent truncation on the shipped path.
#[test]
fn sdf_tiled_over_dense_ceiling_meshes_fully_or_degrades_loudly() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfSegCeil] no GPU adapter — skipping"); return; }
    };
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-3.0), Vec3::splat(3.0));
    let nid = core_math::scene_graph::NodeId(803);
    let grid = bake_grid(res, gmin, gmax, |p| {
        let b = sd_round_box(p, Vec3::splat(1.4), 0.18);
        let s = sd_sphere(p - Vec3::new(0.9, 0.9, 0.9), 0.7);
        b.max(-s)
    });
    let atlas = atlas_for_grid(&device, &queue, &grid, res, gmin, gmax, nid);
    let mesher = SdfMeshGpu::new(&device);
    let total_v: u64 = atlas.tile_stats.iter()
        .map(|st| (st.halo_bricks as u64 * TILE_VERTS_PER_BRICK_HEADROOM).clamp(32, crate::sdf_gpu_mesh::SDF_TILE_MAX_VERTS))
        .sum();

    // Monolithic reference (full field) for the "meshes fully" comparison.
    let mut slot_ref = SdfBodyMeshSlot::new(&device, res);
    mesher.mesh_into_slot_sparse(&device, &queue, &mut slot_ref, &atlas, gmin, gmax, nid, None);
    let s_c = device.create_buffer(&wgpu::BufferDescriptor { label: Some("cl_c"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot_ref.counters, 0, &s_c, 0, 8);
    queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    { let tx = tx.clone(); s_c.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); }); }
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().unwrap().unwrap();
    let cr: Vec<u32> = bytemuck::cast_slice(&s_c.slice(..).get_mapped_range()).to_vec();
    s_c.unmap();
    let ref_idx_count = cr[1] as u64;

    // Case 1 — MESHES FULLY: capacity_res shrunk so the dense ceiling sits FAR below the real
    // surface (dense_v(16) = 15^3 = 3375 verts < total packing). Pre-4b this was the exact
    // "tiled packing exceeds the dense ceiling — monolithic fallback" refusal; now the tiled
    // path must dispatch and produce the COMPLETE mesh.
    {
        let (dense_v, _) = dc_output_caps_dense(device.limits().max_storage_buffer_binding_size as u64, 16);
        assert!(total_v > dense_v, "premise: packing ({total_v}) must exceed the shrunk dense ceiling ({dense_v})");
        let mut pool = SdfMeshPool::new(&device, false);
        let mut slot = SdfBodyMeshSlot::new(&device, res);
        slot.capacity_res = 16;
        assert!(mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None),
            ">dense-ceiling packing must STAY TILED (the 4b point)");
        assert!(slot.pooled && slot.tiled && !slot.envelope_fallback && !slot.envelope_over_budget);
        let nsegs = pool.segment_count(nid);
        let mut sum_idx = 0u64;
        for seg in 0..nsegs {
            let (cnt, ..) = tiled_readback_seg(&device, &queue, &pool, nid, seg);
            sum_idx += cnt.iter().map(|c| c[1] as u64).sum::<u64>();
        }
        assert_eq!(sum_idx, ref_idx_count, ">dense-ceiling tiled mesh is INCOMPLETE vs reference");
        eprintln!("[SdfSegCeil] case 1 OK: packing {total_v} > shrunk ceiling {dense_v}, still tiled \
            ({nsegs} segs), sum_idx={sum_idx} == ref (COMPLETE)");
    }

    // Case 2 — DEGRADES LOUDLY: budget too small for the segmentation, so tiled refuses, the
    // over-budget latch arms (the poll layer toasts it), the monolithic fallback lands with all
    // higher segments retired and a single seeded entry (nothing stale can draw).
    {
        let mut pool = SdfMeshPool::new(&device, false);
        pool.max_group_bytes = (total_v * 32 * 2 / 3).max(2 << 20); // force >=2 segments
        pool.total_budget_bytes = pool.max_group_bytes; // cannot host 2 segments' regions
        let mut slot = SdfBodyMeshSlot::new(&device, res);
        slot.capacity_res = 16; // real surface > dense ceiling: the fallback truncates -> LOUD
        assert!(!mesher.mesh_into_slot_sparse_tiled(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None),
            "over-budget packing must refuse tiled");
        assert!(slot.envelope_fallback, "hysteresis must arm");
        assert!(slot.envelope_over_budget,
            "over-budget + >dense-ceiling MUST latch the loud-degradation flag (toast source)");
        // Monolithic fallback (the production sequence) — must retire stale segments.
        mesher.mesh_into_slot_sparse(&device, &queue, &mut slot, &atlas, gmin, gmax, nid, Some(&mut pool));
        assert!(pool.segment_count(nid) <= 1, "stale segments must be retired on monolithic fallback");
        if let Some(s) = pool.slot(nid) {
            assert!(s.tiles.is_empty(), "monolithic fallback must shed the tile table");
            assert_eq!(s.draw_entries(), 1, "monolithic slot draws exactly one entry");
        }
        eprintln!("[SdfSegCeil] case 2 OK: over-budget refused LOUDLY (latch armed), monolithic \
            fallback clean (segments retired)");
    }
}

/// Phase 4b — the OWNER-LOG 2048 case at REAL scale (77.3M per-tile vert caps ≈ 4.33 GB of
/// regions): the pool's allocation path must host it POOLED across multiple real ~1 GiB groups
/// under the 6 GiB budget, with distinct groups. #[ignore]d: allocates ~4.3 GB VRAM
/// (16 GB-class GPU). The full 2048 bake→mesh e2e remains
/// `sparse_2048_runs_like_1024_end_to_end` + the owner manual test — this gate pins the exact
/// allocation/partition math the owner log tripped.
#[test]
#[ignore]
fn sdf_pool_2048_owner_log_case_stays_pooled_multi_group() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfSeg2048] no GPU adapter — skipping"); return; }
    };
    let nid = core_math::scene_graph::NodeId(901);
    let mut pool = SdfMeshPool::new(&device, false);
    // The owner-log packing: v=77,345,016 caps. Partition exactly as the tiled path does
    // (80% fill of the per-group cap; 1.25x region slack per segment).
    let total_v: u64 = 77_345_016;
    let pool_max = device.limits().max_buffer_size
        .min(device.limits().max_storage_buffer_binding_size as u64)
        .min(pool.max_group_bytes);
    let seg_v_fill = (pool_max / 32).saturating_mul(4) / 5;
    let mut seg_sizes: Vec<u64> = Vec::new();
    let mut rest = total_v;
    while rest > 0 {
        let s = rest.min(seg_v_fill);
        seg_sizes.push(s);
        rest -= s;
    }
    assert!(seg_sizes.len() >= 2, "owner-log packing must need >=2 segments (got {})", seg_sizes.len());
    let want_bytes: u64 = seg_sizes.iter().map(|v| (v * 5 / 4) * 56).sum();
    assert!(want_bytes <= pool.total_budget_bytes,
        "owner-log packing ({}MB) must fit the pool budget ({}MB)",
        want_bytes >> 20, pool.total_budget_bytes >> 20);
    let t0 = std::time::Instant::now();
    for (si, v) in seg_sizes.iter().enumerate() {
        let (wv, wi) = ((v * 5 / 4).min(pool_max / 32), (v * 6 * 5 / 4).min(pool_max / 4));
        pool.ensure_slot_seg(&device, &queue, nid, si as u32, wv.max(*v), wi.max(v * 6))
            .unwrap_or_else(|_| panic!("segment {si} refused — the owner-log case fell out of the pool"));
    }
    let groups: std::collections::HashSet<usize> = (0..pool.segment_count(nid))
        .map(|s| pool.slot_seg(nid, s).unwrap().group).collect();
    assert!(groups.len() >= 2, "77M-cap body must span >=2 real groups (got {:?})", groups);
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    eprintln!("[SdfSeg2048] OWNER-LOG CASE POOLED: {} segments in {} groups, {:.2}GB regions \
        allocated in {:?} (budget {:.1}GB)",
        seg_sizes.len(), groups.len(), want_bytes as f64 / 1.0e9, t0.elapsed(),
        pool.total_budget_bytes as f64 / 1.0e9);
    pool.free_slot(&queue, nid);
}

/// Phase 4c GATE — DEMAND-SIZE: a dense multi-segment body's idx region is TIGHTENED to ≤ 0.75×
/// its verts region (the universal `idx_cap == 6×vert_cap` ⇒ idx bytes = 24/32 = 0.75× vert bytes)
/// instead of the ×1.5 headroom clamping idx up to the per-buffer ceiling too. Pre-fix every dense
/// group allocated verts==idx==max (≈269 MB/segment of pure idx over-allocation); post-fix idx sits
/// at ~0.75× verts. Also pins that the pool DEMAND-SIZES (a group's idx never exceeds its verts —
/// it is never "flat at max regardless of need").
#[test]
fn sdf_pool_demand_sizes_idx_below_verts_for_dense_multi_segment() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfIdx] no GPU adapter — skipping"); return; }
    };
    let nid = core_math::scene_graph::NodeId(920);
    let mut pool = SdfMeshPool::new(&device, false);
    // Dense multi-segment packing: verts pinned at the per-group ceiling (the owner-log 2048 shape,
    // idx = 6× verts as every real caller provides).
    let pool_max = device.limits().max_buffer_size
        .min(device.limits().max_storage_buffer_binding_size as u64).min(pool.max_group_bytes);
    let seg_v_fill = (pool_max / 32).saturating_mul(4) / 5;
    // Three FULL segments (each at the 80% fill target ⇒ ×1.25 pins verts at the ceiling), so every
    // group is the dense/ceiling case the idx tightening targets.
    let segs: Vec<u64> = vec![seg_v_fill, seg_v_fill, seg_v_fill];
    assert!(segs.len() >= 2, "need a multi-segment body for this gate");
    for (si, v) in segs.iter().enumerate() {
        let (wv, wi) = ((v * 5 / 4).min(pool_max / 32), (v * 6 * 5 / 4).min(pool_max / 4));
        pool.ensure_slot_seg(&device, &queue, nid, si as u32, wv.max(*v), wi.max(v * 6)).unwrap();
    }
    let mut checked = 0;
    for s in 0..pool.segment_count(nid) {
        let gi = pool.slot_seg(nid, s).unwrap().group;
        let (vb, ib) = pool.group_region_bytes(gi);
        // Verts pinned near the ceiling for this dense packing…
        assert!(vb >= pool_max * 9 / 10, "seg {s}: dense verts region should sit near the ceiling (got {vb})");
        // …and idx tightened to ≤ 0.75× verts (+ tiny align slop), NOT clamped up to == verts.
        assert!(ib <= vb * 3 / 4 + (1 << 20),
            "seg {s}: idx region {ib} not tightened below 0.75× verts {vb} (the ×1.5 clamp waste)");
        assert!(ib < vb, "seg {s}: idx region {ib} must be strictly below verts {vb} (demand-sized, not flat-max)");
        checked += 1;
    }
    assert!(checked >= 2, "gate must have checked the multi-segment groups");
    eprintln!("[SdfIdx] demand-size OK: {checked} dense groups, idx ≤ 0.75× verts (was == max pre-fix)");
    pool.free_slot(&queue, nid);
}

/// Phase 4c GATE — RECYCLE: a rebuild/new-group REUSES a retired region buffer instead of creating
/// fresh (skipping create + zero-init). A first body grows through several rebuilds (retiring its
/// prior region buffers); a SECOND body following the same size trajectory then reuses those
/// retirees — the multi-body / scene-load-burst case Phase 4's multi-buffer pool exists for. Also
/// pins the free-list stays BOUNDED and a settle-quiet drain empties it.
#[test]
fn sdf_pool_recycle_reuses_retired_buffer() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[SdfRecycle] no GPU adapter — skipping"); return; }
    };
    let mut pool = SdfMeshPool::new(&device, false);
    pool.max_group_bytes = 64 << 20; // small groups → rebuilds at small sizes (test-knob pattern)
    let a = core_math::scene_graph::NodeId(921);
    let b = core_math::scene_graph::NodeId(922);
    // Body A grows in steps — each grow that exhausts its group's tail rebuilds, RETIRING the old
    // region buffers into the free-list.
    for k in 1..=8u64 { let v = k * 400_000; let _ = pool.ensure_slot(&device, &queue, a, v, v * 6); }
    let hits_after_a = pool.recycle_hits();
    assert_eq!(hits_after_a, 0, "a lone monotonic-GROW body has no same-or-smaller need to reuse (grow > retired)");
    assert!(pool.retired_len() > 0, "A's growth must have retired buffers into the free-list");
    // Body B follows A's trajectory → its rebuilds/births need sizes A already retired → REUSE.
    for k in 1..=8u64 { let v = k * 400_000; let _ = pool.ensure_slot(&device, &queue, b, v, v * 6); }
    assert!(pool.recycle_hits() > 0,
        "second similar body must REUSE the first's retired buffers (recycle_hits stayed 0)");
    // Bounded: the free-list never exceeds its byte cap (a couple of 64 MiB-class buffers here).
    assert!(pool.retired_len() <= 64, "recycle free-list must stay bounded (got {})", pool.retired_len());
    // Settle-quiet drain empties it (the mesh layer calls this when a body meshes quiet).
    pool.drain_recycle();
    assert_eq!(pool.retired_len(), 0, "drain_recycle must release the whole reserve");
    eprintln!("[SdfRecycle] OK: A retired then B reused {} buffers; free-list bounded + drains clean",
        pool.recycle_hits());
    pool.free_slot(&queue, a);
    pool.free_slot(&queue, b);
}
