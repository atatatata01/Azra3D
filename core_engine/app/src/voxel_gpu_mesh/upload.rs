// GPU brush density stamp path — Phase 2 (relocated from voxel_gpu_mesh god-file).
// Generation-counter comments travel with their owning code.

use core_math::voxel_sculpt::VoxelGrid;
use eframe::wgpu;

// ---------------------------------------------------------------------------------------------
// GPU BRUSH (sculpt Phase 2) — applies one stamp to the density grid on the GPU (voxel_brush.wgsl).
// ---------------------------------------------------------------------------------------------

/// GPU tip atlas resolution (fixed; CPU tip is resampled into this disc).
pub(crate) const GPU_TIP_RES: u32 = 64;

/// Uniform for voxel_brush.wgsl — must match the WGSL `BrushParams` layout (14 × vec4 = 224 bytes).
///
/// PHASE 0 (BRUSH_SYSTEM_ARCHITECTURE.md §4.2/§6): the kernel now consumes a compiled `StampPlan`'s
/// op-list. The op-driving fields (`kern` + `op0..op3`, the `PackedOp` array) ride this SAME UBO as a
/// FIXED array — NO new storage buffer, NO new BGL binding (the cheapest GPU surface change, §6). The
/// geometric fields (`aabb_*`, `center`, `normal`, `misc`, `box_*`) stay: they carry res / bounds /
/// voxel_size / the AABB box, which `StampPlan` does NOT have. `aabb_max.w` is now the protect-mask
/// enabled flag (the legacy op code moved to `op0.x`). `pack_stamp_plan` folds the plan into the op-tail.
/// Tip basis + contrast ride `tip_t`/`tip_b` (GPU alpha — no CPU force for tips).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrushParams {
    aabb_min: [u32; 4], // xyz = per-stamp effect AABB min cell, w = res
    aabb_max: [u32; 4], // xyz = per-stamp effect AABB max cell (inclusive), w = mask enabled (0/1)
    center:   [f32; 4], // xyz = brush center (world), w = radius
    normal:   [f32; 4], // xyz = normal (Direction resolved CPU-side), w = per-stamp strength
    misc:     [f32; 4], // x = iso, y = bounds, z = voxel_size, w = sgn (invert: -1/+1)
    box_min:  [u32; 4], // xyz = BOX origin (density/snapshot buffers' min cell), w = falloff hardness BITS (f32 bitcast; 0..1)
    box_dim:  [u32; 4], // xyz = BOX dims (buffers are box-local, not res³), w = 0
    // --- StampPlan op-driving tail (rides the SAME UBO — no new binding) ---
    kern:     [u32; 4], // x = kernel_code, y = kernel_exp BITS (f32), z = chan_mask (unused on GPU), w = op_count (1..=MAX_OPS)
    op0:      [u32; 4], // PackedOp[0]: x = op_code, y = flags, z = amount BITS (f32), w = plane_offset BITS (f32)
    op1:      [u32; 4], // PackedOp[1]
    op2:      [u32; 4], // PackedOp[2]
    op3:      [u32; 4], // PackedOp[3]
    tip_t:    [f32; 4], // xyz = tip tangent, w = contrast (0 = no tip)
    tip_b:    [f32; 4], // xyz = tip bitangent, w = tip res (GPU_TIP_RES)
    stroke_tan: [f32; 4], // xyz = UNIT stroke tangent for stroke-axis Pinch (0 = axisymmetric fallback), w unused
}

/// Resample optional CPU tip into a fixed GPU disc (all 1.0 = identity / no tip cost).
pub(crate) fn pack_tip_alpha_gpu(tip: Option<&core_math::voxel_sculpt::BrushTipAlpha>) -> Vec<f32> {
    let n = (GPU_TIP_RES * GPU_TIP_RES) as usize;
    let mut out = vec![1.0f32; n];
    let Some(tip) = tip else { return out; };
    let inv = 1.0 / GPU_TIP_RES as f32;
    for y in 0..GPU_TIP_RES {
        for x in 0..GPU_TIP_RES {
            let u = (x as f32 + 0.5) * inv * 2.0 - 1.0;
            let v = (y as f32 + 0.5) * inv * 2.0 - 1.0;
            out[(y * GPU_TIP_RES + x) as usize] = tip.sample(u, v);
        }
    }
    out
}

/// Tip plane basis for a stamp normal + rotation (matches CPU apply_stamp_plan_with_tip).
pub(crate) fn tip_basis_for_stamp(normal: glam::Vec3, tip_rot: f32) -> (glam::Vec3, glam::Vec3) {
    let (t0, b0) = core_math::voxel_sculpt::brush_tip_basis(normal);
    if tip_rot.abs() <= 1e-6 {
        return (t0, b0);
    }
    let c = tip_rot.cos();
    let s = tip_rot.sin();
    (t0 * c + b0 * s, -t0 * s + b0 * c)
}

impl BrushParams {
    /// Pack a compiled `StampPlan`'s op-driving fields (kernel + op_list) into the UBO tail. This is the
    /// ONE place the op-list crosses to the GPU — it copies the SAME `StampPlan` the CPU `apply_stamp_plan`
    /// consumes, so CPU and GPU stay in lockstep by construction (op-parity is structural, §4.3). The
    /// geometric fields are filled by the caller from the per-stamp AABB/center/normal/etc.
    fn pack_stamp_plan(&mut self, plan: &core_math::brush::StampPlan) {
        let n = plan.op_count.clamp(1, core_math::brush::MAX_OPS as u32);
        self.kern = [plan.kernel_code, plan.kernel_exp.to_bits(), plan.chan_mask, n];
        let pack = |o: &core_math::brush::PackedOp| [o.op_code, o.flags, o.amount.to_bits(), o.plane_offset.to_bits()];
        self.op0 = pack(&plan.op_list[0]);
        self.op1 = pack(&plan.op_list[1]);
        self.op2 = pack(&plan.op_list[2]);
        self.op3 = pack(&plan.op_list[3]);
    }
}

/// Fold the per-stamp `StampPlan` the GPU brush consumes, from the loose stamp scalars the GPU
/// producers carry. SINGLE SOURCE for "legacy op-code → StampPlan op-list": it routes through the
/// SAME `BrushRecipe::from_builtin` + `fold_recipe` the CPU path uses, so the op-list / kernel the GPU
/// loops are byte-identical to the CPU's. `op` is the legacy `VoxelBrush` code (0..=10); Move/Smudge
/// (`u32::MAX`) are CPU-only and never reach a GPU producer. `center`/`normal`/`strength` are the
/// producer's ALREADY-resolved values (Direction + pressure folded in), so the recipe uses neutral
/// Surface/None and the plan packs them verbatim — only the op-list/kernel are read from the plan.
/// Plane/build/flatten knobs for GPU stamps (system settings).
#[derive(Clone, Copy)]
pub(crate) struct GpuPlaneKnobs {
    pub layer_height: f32,
    pub layer_gain: f32,
    pub plane_offset: f32,
    pub plane_bias: f32,
    pub build_rate: f32,
    pub height_limit: f32,
    pub flatten_amount: f32,
    pub volume_preservation: f32,
    pub accumulate: bool,
    pub inflate_speed: f32,
    pub inflate_direction: u32,
    pub thickness_clamp: f32,
    pub pinch_width: f32,
    pub pinch_amount: f32,
    pub crease_width: f32,
    pub crease_depth: f32,
    pub plane_angle: f32,
    pub cut_depth: f32,
    pub fill_holes: bool,
    pub stack_gather: bool,
    pub stack_flatten: bool,
    pub stack_fill: bool,
    pub stack_crease: bool,
    pub stack_inflate: bool,
}

impl Default for GpuPlaneKnobs {
    fn default() -> Self {
        Self {
            layer_height: 0.14, layer_gain: 0.22, plane_offset: 0.10, plane_bias: 0.0,
            build_rate: 2.0, height_limit: 1.0, flatten_amount: 0.5, volume_preservation: 0.0,
            accumulate: true,
            inflate_speed: 1.0, inflate_direction: 0, thickness_clamp: 1.0,
            pinch_width: 1.0, pinch_amount: 1.0, crease_width: 1.0, crease_depth: 1.0,
            plane_angle: 0.0, cut_depth: 1.0, fill_holes: false,
            stack_gather: false, stack_flatten: false, stack_fill: false,
            stack_crease: false, stack_inflate: false,
        }
    }
}

/// Snapshot live tool knobs for GPU stamp fold (keeps dispatch call sites short).
pub(crate) fn gpu_plane_knobs_from_voxel(v: &crate::VoxelObject) -> GpuPlaneKnobs {
    GpuPlaneKnobs {
        layer_height: v.layer_height,
        layer_gain: v.layer_gain,
        plane_offset: v.plane_offset,
        plane_bias: v.plane_bias,
        build_rate: v.build_rate,
        height_limit: v.height_limit,
        flatten_amount: v.flatten_amount,
        volume_preservation: v.volume_preservation,
        accumulate: v.accumulate,
        inflate_speed: v.inflate_speed,
        inflate_direction: v.inflate_direction,
        thickness_clamp: v.thickness_clamp,
        pinch_width: v.pinch_width,
        pinch_amount: v.pinch_amount,
        crease_width: v.crease_width,
        crease_depth: v.crease_depth,
        plane_angle: v.plane_angle,
        cut_depth: v.cut_depth,
        fill_holes: v.fill_holes,
        stack_gather: v.stack_gather,
        stack_flatten: v.stack_flatten,
        stack_fill: v.stack_fill,
        stack_crease: v.stack_crease,
        stack_inflate: v.stack_inflate,
    }
}

fn gpu_stamp_plan(
    op: u32, center: glam::Vec3, normal: glam::Vec3, radius: f32, strength: f32, sgn: f32,
    hardness: f32, falloff_preset: u32, pinch_intensity: f32,
    plane: GpuPlaneKnobs,
) -> core_math::brush::StampPlan {
    use core_math::brush::{self, BuiltinBrush, Direction};
    let builtin = match op {
        0 => BuiltinBrush::Draw, 1 => BuiltinBrush::Erase, 2 => BuiltinBrush::Smooth, 3 => BuiltinBrush::Flatten,
        4 => BuiltinBrush::Clay, 5 => BuiltinBrush::Scrape, 6 => BuiltinBrush::Crease, 7 => BuiltinBrush::Fill,
        8 => BuiltinBrush::ClayBuildup, 9 => BuiltinBrush::Inflate, _ => BuiltinBrush::Pinch, // 10 = Pinch (Move is CPU-only)
    };
    let recipe = brush::BrushRecipe::from_builtin_ex(
        builtin, radius, strength, hardness, falloff_preset, Direction::SurfaceNormal,
        0, false, false, false, [false; 3], 0.10, 0.0, true,
    );
    let hit = brush::SurfaceHit { center, normal, radius, strength };
    let mods = brush::StrokeMods {
        invert: sgn < 0.0,
        drag_delta: glam::Vec3::ZERO,
        pinch_intensity: pinch_intensity.max(0.0),
        layer_height: plane.layer_height.max(0.0),
        layer_gain: plane.layer_gain.max(0.0),
        plane_offset: plane.plane_offset.max(0.0),
        plane_bias: plane.plane_bias,
        build_rate: plane.build_rate.max(0.0),
        height_limit: plane.height_limit.clamp(0.0, 1.0), // 1.0 = off
        flatten_amount: plane.flatten_amount.clamp(0.0, 2.0),
        volume_preservation: plane.volume_preservation.clamp(0.0, 1.0),
        accumulate: plane.accumulate,
        inflate_speed: plane.inflate_speed.clamp(0.0, 4.0),
        inflate_direction: plane.inflate_direction,
        thickness_clamp: plane.thickness_clamp.clamp(0.05, 1.0),
        pinch_width: plane.pinch_width.clamp(0.05, 4.0),
        pinch_amount: plane.pinch_amount.clamp(0.0, 2.0),
        crease_width: plane.crease_width.clamp(0.1, 4.0),
        crease_depth: plane.crease_depth.clamp(0.0, 4.0),
        plane_angle: plane.plane_angle.clamp(-1.0, 1.0),
        cut_depth: plane.cut_depth.clamp(0.0, 4.0),
        fill_holes: plane.fill_holes,
        stack_gather: plane.stack_gather,
        stack_flatten: plane.stack_flatten,
        stack_fill: plane.stack_fill,
        stack_crease: plane.stack_crease,
        stack_inflate: plane.stack_inflate,
    };
    brush::fold_recipe(&recipe, &hit, &mods)
}

#[inline]
pub(crate) fn gpu_mask_is_active(mask: &[f32], density_len: usize) -> bool {
    mask.len() == density_len && mask.iter().any(|&value| value != 0.0)
}

pub(crate) struct VoxelBrushGpu {
    bgl: wgpu::BindGroupLayout,
    pipe: wgpu::ComputePipeline,
    // GPU-resident brush (sculpt Phase 5a): atlas<->box copy pipelines that wrap the UNCHANGED brush
    // kernel above, so it runs on GPU-resident density (no per-frame CPU round-trip).
    gather_bgl: wgpu::BindGroupLayout,
    gather_pipe: wgpu::ComputePipeline,
    scatter_bgl: wgpu::BindGroupLayout,
    scatter_pipe: wgpu::ComputePipeline,
    /// Binding-3 placeholder for unmasked strokes. The shader never indexes it when aabb_max.w == 0.
    zero_mask_buf: wgpu::Buffer,
}

impl VoxelBrushGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
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
            label: Some("voxel_brush_bgl"),
            entries: &[
                entry(0, false), // density (rw)
                uniform(1),      // params
                entry(2, true),  // snapshot (read)
                entry(3, true),  // protect-mask factors 0..1 (read; zeros when no mask)
                entry(4, true),  // tip alpha disc (read; all 1.0 = identity)
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voxel_brush.wgsl"),
            source: wgpu::ShaderSource::Wgsl(graphics::VOXEL_BRUSH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("voxel_brush_pl"), bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("voxel_brush_pipe"), layout: Some(&layout), module: &shader,
            entry_point: Some("apply_brush"), compilation_options: Default::default(), cache: None });

        // GATHER (atlas->box): density(rw) + params + snapshot(rw) + index(ro) + pool(ro).
        let gather_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("voxel_brush_gather_bgl"),
            entries: &[entry(0, false), uniform(1), entry(2, false), entry(3, true), entry(4, true)],
        });
        let gather_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voxel_brush_gather.wgsl"),
            source: wgpu::ShaderSource::Wgsl(graphics::VOXEL_BRUSH_GATHER_WGSL.into()),
        });
        let gather_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("voxel_brush_gather_pl"), bind_group_layouts: &[&gather_bgl], push_constant_ranges: &[] });
        let gather_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("voxel_brush_gather_pipe"), layout: Some(&gather_layout), module: &gather_shader,
            entry_point: Some("gather"), compilation_options: Default::default(), cache: None });

        // SCATTER (box->atlas): density(ro) + params + index(ro) + pool(rw).
        let scatter_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("voxel_brush_scatter_bgl"),
            entries: &[entry(0, true), uniform(1), entry(2, true), entry(3, false)],
        });
        let scatter_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voxel_brush_scatter.wgsl"),
            source: wgpu::ShaderSource::Wgsl(graphics::VOXEL_BRUSH_SCATTER_WGSL.into()),
        });
        let scatter_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("voxel_brush_scatter_pl"), bind_group_layouts: &[&scatter_bgl], push_constant_ranges: &[] });
        let scatter_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("voxel_brush_scatter_pipe"), layout: Some(&scatter_layout), module: &scatter_shader,
            entry_point: Some("scatter"), compilation_options: Default::default(), cache: None });

        let zero_mask_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("voxel_brush_zero_mask"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        {
            let mut mapped = zero_mask_buf.slice(..).get_mapped_range_mut();
            mapped.copy_from_slice(&0.0f32.to_ne_bytes());
        }
        zero_mask_buf.unmap();

        Self { bgl, pipe, gather_bgl, gather_pipe, scatter_bgl, scatter_pipe, zero_mask_buf }
    }

    /// Apply ONE stamp on the GPU over the brush's AABB BOX and read the box back (BLOCKING — for the
    /// parity check). `op` is the VoxelBrush code, `sgn` = -1/+1 (invert). Returns the box-local result
    /// (`subvolume(aabb)` layout); empty if the stamp is fully outside the grid. The caller compares it
    /// to `cpu.density.subvolume(aabb)`. Box = the stamp AABB, so `snap_g`'s box clamp matches the CPU's
    /// AABB-snapshot clamp.
    pub(crate) fn apply_stamp(
        &self, device: &wgpu::Device, queue: &wgpu::Queue, grid: &VoxelGrid,
        op: u32, center: glam::Vec3, normal: glam::Vec3, radius: f32, strength: f32, sgn: f32,
    ) -> Vec<f32> {
        let res = grid.res;
        let Some(aabb) = grid.brush_aabb(center, radius) else { return Vec::new() };
        let [cx, cy, cz, mx, my, mz] = aabb;
        let (bdx, bdy, bdz) = (mx - cx + 1, my - cy + 1, mz - cz + 1);
        let nbytes = (bdx as u64) * (bdy as u64) * (bdz as u64) * 4;

        // Box-local density (read-write) + a pre-stamp snapshot copy of the SAME box (neighbourhood ops).
        let box_density = grid.density.subvolume(aabb);
        let box_mask = if grid.has_mask() { grid.mask_subvolume(aabb) } else { Vec::new() };
        let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vb_density"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        queue.write_buffer(&density_buf, 0, bytemuck::cast_slice(&box_density));
        let snap_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vb_snap"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&snap_buf, 0, bytemuck::cast_slice(&box_density));
        let mask_buf = gpu_mask_is_active(&box_mask, box_density.len()).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vb_mask"), size: nbytes.max(4),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&box_mask));
            buffer
        });
        let mask_active = mask_buf.is_some();
        let tip_upload = pack_tip_alpha_gpu(None);
        let tip_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vb_tip"), size: (tip_upload.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&tip_buf, 0, bytemuck::cast_slice(&tip_upload));

        let (tt, tb) = tip_basis_for_stamp(normal, 0.0);
        let mut params = BrushParams {
            aabb_min: [cx, cy, cz, res], aabb_max: [mx, my, mz, mask_active as u32],
            center: [center.x, center.y, center.z, radius],
            normal: [normal.x, normal.y, normal.z, strength],
            misc: [grid.iso, grid.bounds, grid.voxel_size(), sgn],
            box_min: [cx, cy, cz, 0u32], box_dim: [bdx, bdy, bdz, 0], // falloff bits 0 = 0.0 hardness (parity vs CPU hardness 0.0)
            kern: [0; 4], op0: [0; 4], op1: [0; 4], op2: [0; 4], op3: [0; 4],
            tip_t: [tt.x, tt.y, tt.z, 0.0],
            tip_b: [tb.x, tb.y, tb.z, GPU_TIP_RES as f32],
            // ZERO stroke tangent → axisymmetric Pinch, matching StrokeMods::default drag_delta=0
            // on the CPU stamp path (parity: both take the radial fallback branch).
            stroke_tan: [0.0, 0.0, 0.0, 0.0],
        };
        // Fold the StampPlan op-list (hardness 0.0 matches the packed `box_min.w = 0` → CPU hardness 0.0).
        params.pack_stamp_plan(&gpu_stamp_plan(op, center, normal, radius, strength, sgn, 0.0, 0, 1.0, GpuPlaneKnobs::default()));
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vb_params"), size: std::mem::size_of::<BrushParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vb_bg"), layout: &self.bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: mask_buf.as_ref().unwrap_or(&self.zero_mask_buf).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: tip_buf.as_entire_binding() },
            ] });
        let wg = |n: u32| (n + 3) / 4;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vb_enc") });
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vb_pass"), timestamp_writes: None });
            p.set_pipeline(&self.pipe); p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(wg(bdx), wg(bdy), wg(bdz));
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vb_staging"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(&density_buf, 0, &staging, 0, nbytes);
        queue.submit(std::iter::once(enc.finish()));

        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let _ = rx.recv();
        let out: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        staging.unmap();
        out
    }

    /// LIVE path (ASYNC): apply a batch of stamps over the union AABB BOX on the GPU and START a
    /// non-blocking readback of that box — the CPU never waits here. The readback is consumed a later
    /// frame via [`VoxelBrushInflight::collect`] (mirrors the GPU mesh async path). `box_density` is the
    /// CURRENT CPU grid over the box (`grid.density.subvolume(union)`, box-local layout) — uploaded to a
    /// PER-DISPATCH box buffer (bounded by the brush, NOT res³ — the 1024³ enabler); the stamps run in
    /// place (sequential passes → wgpu barriers accumulate them). All 11 ops dispatch here; neighbourhood
    /// ops (Smooth/Inflate/Pinch) read the pre-frame box snapshot. Pinch's relax is a CPU post-pass at
    /// collect time. `union`/`is_pinch` are carried so the collector can write back + relax + mark chunks.
    /// Stamp tuple: (op, center, normal, radius, strength, sgn, aabb, tip_rotation_radians).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_stamps_region(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        res: u32, bounds: f32, iso: f32, box_density: Vec<f32>,
        box_mask: Vec<f32>,
        union: [u32; 6], is_pinch: bool, hardness: f32, falloff_preset: u32,
        pinch_intensity: f32, plane: GpuPlaneKnobs,
        tip: Option<&core_math::voxel_sculpt::BrushTipAlpha>,
        tip_contrast: f32,
        stroke_tan: glam::Vec3, // unit stroke tangent for stroke-axis Pinch (ZERO = axisymmetric)
        stamps: &[(u32, glam::Vec3, glam::Vec3, f32, f32, f32, [u32; 6], f32)],
    ) -> VoxelBrushInflight {
        // Pack hardness as f32 BITS into box_min.w; falloff preset id into box_dim.w (F1b).
        let falloff = hardness.to_bits();
        let voxel_size = (bounds * 2.0) / (res - 1) as f32;
        let [bx0, by0, bz0, bx1, by1, bz1] = union;
        let (bdx, bdy, bdz) = (bx1 - bx0 + 1, by1 - by0 + 1, bz1 - bz0 + 1);
        let nbytes = (bdx as u64) * (bdy as u64) * (bdz as u64) * 4;
        debug_assert_eq!(box_density.len(), (bdx * bdy * bdz) as usize);
        let mask_active = gpu_mask_is_active(&box_mask, box_density.len());
        let tip_upload = pack_tip_alpha_gpu(tip);
        let tip_c = if tip.is_some() { tip_contrast.clamp(0.05, 4.0) } else { 0.0 };

        // Box-local density buffer (PER-DISPATCH, sized to the brush box not res³) + a pre-frame
        // snapshot copy of the same box so every stamp reads one consistent state (no in-flight races).
        let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_density"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        queue.write_buffer(&density_buf, 0, bytemuck::cast_slice(&box_density));
        let snap_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_snap"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&snap_buf, 0, bytemuck::cast_slice(&box_density));
        let mask_buf = mask_active.then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vbr_mask"), size: nbytes.max(4),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&box_mask));
            buffer
        });
        let tip_bytes = (tip_upload.len() * 4) as u64;
        let tip_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_tip"), size: tip_bytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&tip_buf, 0, bytemuck::cast_slice(&tip_upload));
        let wg = |n: u32| (n + 3) / 4;
        let mut params_bufs: Vec<wgpu::Buffer> = Vec::with_capacity(stamps.len());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vbr_enc") });
        for &(op, center, normal, radius, strength, sgn, [cx, cy, cz, mx, my, mz], tip_rot) in stamps {
            let (tt, tb) = tip_basis_for_stamp(normal, tip_rot);
            let mut params = BrushParams {
                aabb_min: [cx, cy, cz, res], aabb_max: [mx, my, mz, mask_active as u32],
                center: [center.x, center.y, center.z, radius],
                normal: [normal.x, normal.y, normal.z, strength],
                misc: [iso, bounds, voxel_size, sgn],
                box_min: [bx0, by0, bz0, falloff], box_dim: [bdx, bdy, bdz, falloff_preset],
                kern: [0; 4], op0: [0; 4], op1: [0; 4], op2: [0; 4], op3: [0; 4],
                tip_t: [tt.x, tt.y, tt.z, tip_c],
                tip_b: [tb.x, tb.y, tb.z, GPU_TIP_RES as f32],
                stroke_tan: [stroke_tan.x, stroke_tan.y, stroke_tan.z, 0.0],
            };
            params.pack_stamp_plan(&gpu_stamp_plan(op, center, normal, radius, strength, sgn, hardness, falloff_preset, pinch_intensity, plane));
            let pbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vbr_params"), size: std::mem::size_of::<BrushParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&pbuf, 0, bytemuck::bytes_of(&params));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vbr_bg"), layout: &self.bgl, entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: pbuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: mask_buf.as_ref().unwrap_or(&self.zero_mask_buf).as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: tip_buf.as_entire_binding() },
                ] });
            // Separate pass per stamp → wgpu inserts the density write→read barrier between them, so
            // sequential stamps accumulate (additive Draw, plane ops, etc.).
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbr_pass"), timestamp_writes: None });
            p.set_pipeline(&self.pipe); p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(wg(mx - cx + 1), wg(my - cy + 1), wg(mz - cz + 1));
            drop(p);
            params_bufs.push(pbuf);
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_staging"), size: nbytes.max(4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(&density_buf, 0, &staging, 0, nbytes);
        queue.submit(std::iter::once(enc.finish()));

        // Start the map WITHOUT waiting. Both callback outcomes settle the tri-state cell: an error
        // must be distinguishable from Pending or the single in-flight slot blocks every later dab.
        let map_ready = std::sync::Arc::new(crate::map_state::MapState::new());
        let map_ready_cb = map_ready.clone();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            map_ready_cb.store_result(&result);
        });
        VoxelBrushInflight {
            // density_buf is per-dispatch now → keep it alive until the readback completes.
            staging, _keepalive: {
                params_bufs.push(snap_buf);
                params_bufs.push(density_buf);
                if let Some(mask_buf) = mask_buf { params_bufs.push(mask_buf); }
                params_bufs.push(tip_buf);
                params_bufs
            }, map_ready,
            res, union, is_pinch, age: 0, prev_box: box_density,
            stamp_aabbs: stamps.iter().map(|s| s.6).collect(),
        }
    }

    /// GPU-RESIDENT brush dispatch (sculpt Phase 5a/5c): GATHER the atlas over the union box into a
    /// box-local scratch, run the UNCHANGED brush kernel per stamp, then SCATTER the result back into
    /// the atlas POOL in place. NO per-frame CPU readback — the mesh reads the same POOL so the surface
    /// is live; the CPU grid is synced lazily on stroke release (`readback_atlas_box`). Caller MUST have
    /// pre-materialised the box's bricks (`materialize_box`) and synced the atlas so `index_buf`/`pool_buf`
    /// are current. Pinch is NOT routed here (its CPU relax needs per-frame CPU state — use the box path).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_stamps_region_atlas(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        res: u32, bounds: f32, iso: f32, brick_res: u32,
        index_buf: &wgpu::Buffer, pool_buf: &wgpu::Buffer, union: [u32; 6], hardness: f32, falloff_preset: u32,
        pinch_intensity: f32, plane: GpuPlaneKnobs,
        tip: Option<&core_math::voxel_sculpt::BrushTipAlpha>,
        tip_contrast: f32,
        stroke_tan: glam::Vec3, // unit stroke tangent for stroke-axis Pinch (ZERO = axisymmetric)
        stamps: &[(u32, glam::Vec3, glam::Vec3, f32, f32, f32, [u32; 6], f32)],
    ) {
        // Pack hardness as f32 BITS into box_min.w; falloff preset id into box_dim.w (F1b).
        let falloff = hardness.to_bits();
        let voxel_size = (bounds * 2.0) / (res - 1) as f32;
        let [bx0, by0, bz0, bx1, by1, bz1] = union;
        let (bdx, bdy, bdz) = (bx1 - bx0 + 1, by1 - by0 + 1, bz1 - bz0 + 1);
        let nbytes = (bdx as u64) * (bdy as u64) * (bdz as u64) * 4;
        let wg = |n: u32| (n + 3) / 4;
        // Box-local scratch: density (brush rw) + snapshot (pre-frame copy for neighbourhood ops),
        // FILLED by the gather pass from the atlas — not uploaded from the (mid-stroke stale) CPU grid.
        let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_atlas_density"), size: nbytes.max(4), usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let snap_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_atlas_snap"), size: nbytes.max(4), usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        // Atlas strokes are latched off whenever a protect-mask is active. Bind the persistent one-float
        // dummy and mark mask_enabled=0 instead of allocating/uploading a union-sized zero volume.
        let tip_upload = pack_tip_alpha_gpu(tip);
        let tip_c = if tip.is_some() { tip_contrast.clamp(0.05, 4.0) } else { 0.0 };
        let tip_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_atlas_tip"), size: (tip_upload.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&tip_buf, 0, bytemuck::cast_slice(&tip_upload));
        // GatherParams/ScatterParams share layout: box_min.xyz + brick_res(.w), box_dim.xyz + 0.
        let cp: [u32; 8] = [bx0, by0, bz0, brick_res, bdx, bdy, bdz, 0];
        let copy_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_atlas_copy_params"), size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&copy_params, 0, bytemuck::cast_slice(&cp));
        let mut params_bufs: Vec<wgpu::Buffer> = Vec::with_capacity(stamps.len());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vbr_atlas_enc") });
        // 1) GATHER atlas -> box (density + snapshot).
        let gather_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vbr_gather_bg"), layout: &self.gather_bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: copy_params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: index_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: pool_buf.as_entire_binding() },
            ] });
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbr_gather"), timestamp_writes: None });
            p.set_pipeline(&self.gather_pipe); p.set_bind_group(0, &gather_bg, &[]);
            p.dispatch_workgroups(wg(bdx), wg(bdy), wg(bdz));
        }
        // 2) per-stamp BRUSH passes (UNCHANGED kernel, box-local; separate passes accumulate via barriers).
        for &(op, center, normal, radius, strength, sgn, [cx, cy, cz, mx, my, mz], tip_rot) in stamps {
            let (tt, tb) = tip_basis_for_stamp(normal, tip_rot);
            let mut params = BrushParams {
                aabb_min: [cx, cy, cz, res], aabb_max: [mx, my, mz, 0],
                center: [center.x, center.y, center.z, radius],
                normal: [normal.x, normal.y, normal.z, strength],
                misc: [iso, bounds, voxel_size, sgn],
                box_min: [bx0, by0, bz0, falloff], box_dim: [bdx, bdy, bdz, falloff_preset],
                kern: [0; 4], op0: [0; 4], op1: [0; 4], op2: [0; 4], op3: [0; 4],
                tip_t: [tt.x, tt.y, tt.z, tip_c],
                tip_b: [tb.x, tb.y, tb.z, GPU_TIP_RES as f32],
                stroke_tan: [stroke_tan.x, stroke_tan.y, stroke_tan.z, 0.0],
            };
            params.pack_stamp_plan(&gpu_stamp_plan(op, center, normal, radius, strength, sgn, hardness, falloff_preset, pinch_intensity, plane));
            let pbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vbr_atlas_params"), size: std::mem::size_of::<BrushParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&pbuf, 0, bytemuck::bytes_of(&params));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vbr_atlas_brush_bg"), layout: &self.bgl, entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: pbuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: self.zero_mask_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: tip_buf.as_entire_binding() },
                ] });
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbr_atlas_brush"), timestamp_writes: None });
            p.set_pipeline(&self.pipe); p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(wg(mx - cx + 1), wg(my - cy + 1), wg(mz - cz + 1));
            drop(p);
            params_bufs.push(pbuf);
        }
        // 3) SCATTER box -> atlas POOL in place (the mesh reads this POOL).
        let scatter_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vbr_scatter_bg"), layout: &self.scatter_bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: copy_params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: index_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: pool_buf.as_entire_binding() },
            ] });
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbr_scatter"), timestamp_writes: None });
            p.set_pipeline(&self.scatter_pipe); p.set_bind_group(0, &scatter_bg, &[]);
            p.dispatch_workgroups(wg(bdx), wg(bdy), wg(bdz));
        }
        queue.submit(std::iter::once(enc.finish()));
        // No readback / no inflight. The buffers drop here; wgpu keeps them alive for the submitted
        // command buffer until the GPU finishes. The atlas POOL now holds the brush result.
        let _ = params_bufs;
    }

    /// Blocking GPU->CPU readback of the atlas over a box into a dense box-local Vec (release-time CPU
    /// sync for the GPU-resident path). Reuses the gather kernel + a staging copy. One call per stroke.
    pub(crate) fn readback_atlas_box(
        &self, device: &wgpu::Device, queue: &wgpu::Queue,
        brick_res: u32, index_buf: &wgpu::Buffer, pool_buf: &wgpu::Buffer, aabb: [u32; 6],
    ) -> Vec<f32> {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let (dx, dy, dz) = (x1 - x0 + 1, y1 - y0 + 1, z1 - z0 + 1);
        let nbytes = (dx as u64) * (dy as u64) * (dz as u64) * 4;
        let wg = |n: u32| (n + 3) / 4;
        let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_rb_density"), size: nbytes.max(4), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let snap_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_rb_snap"), size: nbytes.max(4), usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let cp: [u32; 8] = [x0, y0, z0, brick_res, dx, dy, dz, 0];
        let cpb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_rb_params"), size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&cpb, 0, bytemuck::cast_slice(&cp));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vbr_rb_enc") });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vbr_rb_bg"), layout: &self.gather_bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: cpb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: snap_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: index_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: pool_buf.as_entire_binding() },
            ] });
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vbr_rb_gather"), timestamp_writes: None });
            p.set_pipeline(&self.gather_pipe); p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(wg(dx), wg(dy), wg(dz));
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbr_rb_staging"), size: nbytes.max(4), usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_buffer_to_buffer(&density_buf, 0, &staging, 0, nbytes);
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let _ = rx.recv();
        let out: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        staging.unmap();
        out
    }
}

/// An in-flight GPU brush batch: the stamps have been dispatched and the edited BOX is mapping
/// asynchronously. Poll the device each frame; when `map_state()` is `Ready`, `collect()` reads the
/// new box. `Failed` must release the flight without collecting. Carries
/// the metadata the collector needs to write back into the CPU grid + record undo (`res`, `union`) and
/// to run Pinch's relax (`is_pinch`). The transient per-stamp buffers + snapshot + the per-dispatch
/// density box buffer are kept alive in `_keepalive` until the GPU work completes.
pub(crate) struct VoxelBrushInflight {
    staging: wgpu::Buffer,
    _keepalive: Vec<wgpu::Buffer>,
    map_ready: std::sync::Arc<crate::map_state::MapState>,
    pub(crate) res: u32,
    pub(crate) union: [u32; 6],
    pub(crate) is_pinch: bool,
    /// Frames waited un-collected. Normal Pending work is never dropped; an explicit map failure is
    /// released without reading because no host-visible result exists.
    pub(crate) age: u32,
    /// The pre-dispatch CPU grid over the union box (box-local), captured at dispatch. The grid in this
    /// box is NOT mutated between dispatch and collect (one batch in flight), so this IS the pre-edit
    /// value — the collector diffs the GPU result against it WITHOUT re-decoding the sparse grid per
    /// voxel (the dominant large-brush writeback cost at 512³).
    prev_box: Vec<f32>,
    /// Each stamp's effect AABB. The collector marks chunks dirty PER-STAMP (the thin swept tube) rather
    /// than over the union AABB — a fast diagonal flick's union box is ~20× the tube and floods the
    /// mesher with un-edited chunks (the fast-stroke "tearing"). Matches the CPU brush's per-stamp marking.
    pub(crate) stamp_aabbs: Vec<[u32; 6]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoxelBrushMapState {
    Pending,
    Ready,
    Failed,
}

#[inline]
pub(crate) fn voxel_brush_map_state(raw: u8) -> VoxelBrushMapState {
    match raw {
        crate::map_state::PENDING => VoxelBrushMapState::Pending,
        crate::map_state::OK => VoxelBrushMapState::Ready,
        crate::map_state::ERR => VoxelBrushMapState::Failed,
        _ => VoxelBrushMapState::Failed,
    }
}

impl VoxelBrushInflight {
    /// Explicit callback state, advanced by `device.poll(Poll)`. Failure is settled work, not a
    /// permanent Pending state, so the owner can release the batch without reading an invalid map.
    pub(crate) fn map_state(&self) -> VoxelBrushMapState {
        voxel_brush_map_state(self.map_ready.peek())
    }
    /// Read the mapped box back to the CPU. Call only when `map_state()` is `Ready` (or after a blocking
    /// `device.poll(Wait)`). Returns `(new_box, pre_edit_box)`, both box-local (union) layout — the
    /// collector diffs them to write back only the CHANGED voxels and record undo without re-decoding.
    pub(crate) fn collect(self) -> (Vec<f32>, Vec<f32>) {
        let out: Vec<f32> = bytemuck::cast_slice(&self.staging.slice(..).get_mapped_range()).to_vec();
        self.staging.unmap();
        (out, self.prev_box)
    }
}
