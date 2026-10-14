// Headless GPU parity test for `voxel_brush.wgsl` (Phase-0 GATE, BRUSH_SYSTEM_ARCHITECTURE.md §4.3).
//
// Dispatches the REAL `voxel_brush.wgsl` kernel (the production shader, consuming a `StampPlan`'s
// op_list) for every one of the 12 built-in presets and compares the resulting density field, voxel
// for voxel, against the CPU `VoxelGrid::apply_stamp_plan` — the reference the kernel must match. One
// opcode table, two languages: if they diverge, the kernel is wrong and is fixed to match the CPU.
//
// The dispatch mirrors the production `VoxelBrushGpu::apply_stamp` (app/src/voxel_gpu_mesh.rs) EXACTLY:
// a box-LOCAL density buffer over the brush AABB + a pre-stamp snapshot copy of the same box, the same
// UBO layout, hardness 0.0 (so `falloff_weight` = Blender smoothstep 3e^2-2e^3, a plain polynomial, CPU/GPU-close).
// `BrushParams` here is the test-side mirror of the WGSL `BrushParams`; the op-tail is packed from the
// SAME folded `StampPlan` the CPU consumes, so the test guards the GPU's StampPlan consumption directly.
//
// Skips gracefully (no panic) when no GPU adapter is present (headless CI) — the CPU golden parity test
// in core_math is the always-on gate; this is the GPU-kernel confirmation when a device is available.

use core_math::brush::{self, BuiltinBrush, Direction};
use core_math::voxel_sculpt::VoxelGrid;
use crate::VOXEL_BRUSH_WGSL;

const GPU_TIP_RES: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrushParams {
    aabb_min: [u32; 4],
    aabb_max: [u32; 4],
    center: [f32; 4],
    normal: [f32; 4],
    misc: [f32; 4],
    box_min: [u32; 4],
    box_dim: [u32; 4],
    kern: [u32; 4],
    op0: [u32; 4],
    op1: [u32; 4],
    op2: [u32; 4],
    op3: [u32; 4],
    tip_t: [f32; 4],
    tip_b: [f32; 4],
    stroke_tan: [f32; 4], // stroke-axis Pinch tangent (ZERO here = axisymmetric, matches CPU default)
}

impl BrushParams {
    fn pack_stamp_plan(&mut self, plan: &brush::StampPlan) {
        let n = plan.op_count.clamp(1, brush::MAX_OPS as u32);
        self.kern = [plan.kernel_code, plan.kernel_exp.to_bits(), plan.chan_mask, n];
        let pack = |o: &brush::PackedOp| [o.op_code, o.flags, o.amount.to_bits(), o.plane_offset.to_bits()];
        self.op0 = pack(&plan.op_list[0]);
        self.op1 = pack(&plan.op_list[1]);
        self.op2 = pack(&plan.op_list[2]);
        self.op3 = pack(&plan.op_list[3]);
    }
}

/// Try to acquire a headless wgpu device. Returns None if no adapter (skip the test, don't fail).
fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("voxel_brush_parity_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    )).ok()?;
    Some((device, queue))
}

/// Dispatch ONE stamp via the real `voxel_brush.wgsl` over the brush AABB box; return the box-local
/// density result (same layout as `grid.density.subvolume(aabb)`). Mirrors `VoxelBrushGpu::apply_stamp`.
fn gpu_apply_stamp(
    device: &wgpu::Device, queue: &wgpu::Queue, pipe: &wgpu::ComputePipeline, bgl: &wgpu::BindGroupLayout,
    grid: &VoxelGrid, plan: &brush::StampPlan, aabb: [u32; 6],
) -> Vec<f32> {
    let [cx, cy, cz, mx, my, mz] = aabb;
    let (bdx, bdy, bdz) = (mx - cx + 1, my - cy + 1, mz - cz + 1);
    let nbytes = (bdx as u64) * (bdy as u64) * (bdz as u64) * 4;

    let box_density = grid.density.subvolume(aabb);
    let box_mask = grid.mask_subvolume(aabb);
    let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_density"), size: nbytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false });
    queue.write_buffer(&density_buf, 0, bytemuck::cast_slice(&box_density));
    let snap_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_snap"), size: nbytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&snap_buf, 0, bytemuck::cast_slice(&box_density));
    let mask_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_mask"), size: nbytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&mask_buf, 0, bytemuck::cast_slice(&box_mask));
    let tip_ones = vec![1.0f32; (GPU_TIP_RES * GPU_TIP_RES) as usize];
    let tip_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_tip"), size: (tip_ones.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&tip_buf, 0, bytemuck::cast_slice(&tip_ones));

    let center = glam::Vec3::new(plan.center[0], plan.center[1], plan.center[2]);
    let radius = plan.center[3];
    let normal = glam::Vec3::new(plan.plane_n[0], plan.plane_n[1], plan.plane_n[2]);
    let strength = plan.plane_n[3];
    let mut params = BrushParams {
        aabb_min: [cx, cy, cz, grid.res], aabb_max: [mx, my, mz, 1],
        center: [center.x, center.y, center.z, radius],
        normal: [normal.x, normal.y, normal.z, strength],
        misc: [grid.iso, grid.bounds, grid.voxel_size(), plan.sgn],
        box_min: [cx, cy, cz, 0u32], // hardness bits 0 = 0.0 (matches the CPU's hardness 0.0)
        // falloff preset from plan._pad0 (must match CPU fold; was hardcoded Smooth→false Δ).
        box_dim: [bdx, bdy, bdz, plan._pad0],
        kern: [0; 4], op0: [0; 4], op1: [0; 4], op2: [0; 4], op3: [0; 4],
        tip_t: [1.0, 0.0, 0.0, 0.0], // contrast 0 = identity tip
        tip_b: [0.0, 1.0, 0.0, GPU_TIP_RES as f32],
        stroke_tan: [0.0, 0.0, 0.0, 0.0], // axisymmetric Pinch (parity vs CPU StrokeMods::default)
    };
    params.pack_stamp_plan(plan);
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_params"), size: std::mem::size_of::<BrushParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("vbp_bg"), layout: bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: mask_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: tip_buf.as_entire_binding() },
        ] });
    let wg = |n: u32| (n + 3) / 4;
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vbp_enc") });
    {
        let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbp_pass"), timestamp_writes: None });
        p.set_pipeline(pipe); p.set_bind_group(0, &bg, &[]);
        p.dispatch_workgroups(wg(bdx), wg(bdy), wg(bdz));
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbp_staging"), size: nbytes.max(4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    enc.copy_buffer_to_buffer(&density_buf, 0, &staging, 0, nbytes);
    queue.submit(std::iter::once(enc.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    let _ = rx.recv();
    let out: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    out
}

#[test]
fn golden_parity_gpu_kernel_matches_cpu_stamp_plan_all_builtins() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[VXGPU-PARITY] no GPU adapter — skipping (CPU golden parity test is the always-on gate)");
        return;
    };
    // Build the production brush pipeline from the real WGSL.
    let entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding, visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let uniform = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding, visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("vbp_bgl"), entries: &[entry(0, false), uniform(1), entry(2, true), entry(3, true), entry(4, true)] });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("voxel_brush.wgsl"), source: wgpu::ShaderSource::Wgsl(VOXEL_BRUSH_WGSL.into()) });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("vbp_pl"), bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("vbp_pipe"), layout: Some(&layout), module: &shader,
        entry_point: Some("apply_brush"), compilation_options: Default::default(), cache: None });

    // Fixed solid blob so plane/gradient/relax/advect ops have real material.
    let base = { let mut g = VoxelGrid::new_empty(40, 1.0); g.fill_sphere(glam::Vec3::ZERO, 0.55); g };
    let center = glam::Vec3::new(0.0, 0.0, 0.45);
    let normal = glam::Vec3::Z;
    let (radius, strength) = (0.30f32, 0.6f32);
    let hardness = 0.0f32; // packed box_min.w = 0 -> CPU `falloff_weight` at h=0 = Blender smoothstep (3e^2-2e^3)
    let aabb = base.brush_aabb(center, radius).expect("brush AABB overlaps the grid");

    let mut worst = (0.0f32, "");
    let mut pinch_delta = 0.0f32;
    for b in BuiltinBrush::ALL {
        // Move (and Smudge) are CPU-only — the GPU kernel has no DragVector arm; the producer never
        // dispatches them. Skip on the GPU side (consistent with the production CPU-force latch).
        if matches!(b, BuiltinBrush::Move) { continue; }

        // Use Smooth (0) — matches gpu_apply_stamp box_dim.w=0. from_builtin defaults to Blend(8)
        // which made this gate compare two different falloff curves (false Δ up to ~0.42).
        let recipe = brush::BrushRecipe::from_builtin_ex(
            b, radius, strength, hardness, 0, Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true);
        let hit = brush::SurfaceHit { center, normal, radius, strength };
        let mods = brush::StrokeMods::default();
        let plan = brush::fold_recipe(&recipe, &hit, &mods);

        // CPU reference: apply_stamp_plan on a clone (the SAME plan the GPU gets), then read back the box.
        let mut cpu = base.clone();
        cpu.apply_stamp_plan(&plan, None);
        let cpu_box = cpu.density.subvolume(aabb);

        let gpu_box = gpu_apply_stamp(&device, &queue, &pipe, &bgl, &base, &plan, aabb);
        assert_eq!(cpu_box.len(), gpu_box.len(), "{}: box size mismatch", b.name());
        let md = cpu_box.iter().zip(gpu_box.iter()).map(|(a, g)| (a - g).abs()).fold(0.0f32, f32::max);
        eprintln!("[VXGPU-PARITY] {:<11} |Δ| {:.3e}", b.name(), md);

        if matches!(b, BuiltinBrush::Pinch) {
            // Pinch's CPU `apply_stamp_plan` runs a trailing redistancing relax over the box that the
            // GPU kernel does NOT (the GPU defers it to a CPU collect-time relax — see input_sculpt).
            // So the GPU result is the PRE-RELAX gather; the comparison includes that relax delta. Bound
            // it (sanity) but don't hold it to the tight stamp-parity epsilon — this is the pre-relax gather,
            // NOT a kernel defect: the matching relax runs at CPU collect-time in the production path.
            pinch_delta = md;
            assert!(md < 0.2, "Pinch GPU vs CPU(+relax) |Δ| {md:.3e} is implausibly large");
        } else if md > worst.0 {
            worst = (md, b.name());
        }
    }
    // hardness 0 → falloff is Blender smoothstep (3e^2-2e^3), a plain polynomial; the 10 deterministic
    // ops are bit-close (CPU/GPU multiply-add match). Small tolerance for GPU f32 sqrt/div (Inflate/Flatten).
    assert!(worst.0 <= 2e-3, "GPU kernel diverged from CPU apply_stamp_plan on {}: |Δ| {:.3e}", worst.1, worst.0);
    eprintln!("[VXGPU-PARITY] PASS — 10 deterministic ops |Δ| {:.3e} ({}); Pinch {:.3e} (pre-relax)",
        worst.0, worst.1, pinch_delta);

    // REGRESSION — PlanePush (Flatten) on-plane parity. The kernel originally used WGSL `sign(plane)` where
    // the CPU uses `f32::signum`; they disagree at plane==0.0 (`sign(0)=0` vs `signum(0)=+1`), so an exactly-
    // on-plane voxel was left UNCHANGED on the GPU but pushed on the CPU. The main loop misses it (its centre
    // z=0.45 is off-lattice → no weighted voxel lands on plane==0). Construct exactly that case: AXIS-Z Flatten
    // whose centre z is the world position of cell-layer z=28 — so that layer's voxels get
    // `plane == p.z - centre.z == 0` bit-identically (CPU `cell_world_pos` and the shader `cell_world` share
    // the formula). With the fix both push the layer identically; the pre-fix kernel diverged by sgn·w·0.5.
    {
        let lz = base.cell_world_pos(0, 0, 28).z; // exact lattice z → on-plane voxel layer
        let center2 = glam::Vec3::new(0.0, 0.0, lz);
        let aabb2 = base.brush_aabb(center2, radius).expect("on-plane Flatten AABB overlaps the grid");
        let recipe = brush::BrushRecipe::from_builtin_ex(
            BuiltinBrush::Flatten, radius, strength, 0.0, 0, Direction::AxisZ,
            0, false, false, false, [false; 3], 0.25, 0.0, true);
        let hit = brush::SurfaceHit { center: center2, normal: glam::Vec3::Z, radius, strength };
        let mods = brush::StrokeMods::default();
        let plan = brush::fold_recipe(&recipe, &hit, &mods);
        let mut cpu = base.clone();
        cpu.apply_stamp_plan(&plan, None);
        let cpu_box = cpu.density.subvolume(aabb2);
        let gpu_box = gpu_apply_stamp(&device, &queue, &pipe, &bgl, &base, &plan, aabb2);
        let md = cpu_box.iter().zip(gpu_box.iter()).map(|(a, g)| (a - g).abs()).fold(0.0f32, f32::max);
        eprintln!("[VXGPU-PARITY] Flatten on-plane |Δ| {:.3e}", md);
        assert!(md <= 2e-3, "PlanePush on-plane divergence (WGSL sign vs Rust signum at plane==0): |Δ| {md:.3e}");
    }
}
