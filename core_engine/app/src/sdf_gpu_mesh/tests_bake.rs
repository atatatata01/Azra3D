//! Slice 7 (app-crate de-god): SDF bake footprint / mirror-union tests, relocated verbatim from
//! `sdf_gpu_mesh/mod.rs`.
//!
//! Pure test-code move — no logic change. `headless_device` is SHARED (not duplicated) from the
//! sibling `tests_dc` module via `use super::tests_dc::headless_device;` (it is `pub(super)` there).
#![cfg(test)]

use eframe::wgpu;
use glam::Vec3;
use super::tests_dc::headless_device;

/// Phase-1 de-risk spike (resolution architecture plan): prove a PARTIAL bake dispatch writes the flat
/// output buffer EXACTLY at the in-body voxels inside `[bake_min, bake_max)` and leaves every other voxel
/// BYTE-IDENTICAL — no implicit clear, no atlas-pad stomp, no Z-slice gap. This is the load-bearing
/// assumption behind baking the dirty slab IN PLACE into `rv.buffer` (no CPU re-seed → the comb / trailing
/// / size-change regressions become structurally impossible). Uses `prim_count=0`: the eval loop is
/// skipped, `acc` stays 999.0, so the shader still writes 999.0 to the in-body voxels it dispatches —
/// testing the WRITE FOOTPRINT against the REAL `volume_bake.wgsl`, without serializing a prim.
#[test]
fn partial_bake_writes_exact_footprint() {
    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[BakeSpike] no GPU adapter — skipping"); return; }
    };

    // Bake pipeline — BGL entries from the SINGLE SOURCE graphics::bgl::bake_bgl_entries()
    // (identical to gpu_init::ensure_primary_bake_pipeline).
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("spike_bake_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spike volume_bake"),
        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("spike_bake"), layout: Some(&pl), module: &shader, entry_point: Some("bake_volume"),
        compilation_options: Default::default(), cache: None });

    const SENTINEL: f32 = -42.0;
    const BAKED: f32 = 999.0; // `acc` initial value, written when prim_count==0

    // Dispatch a (possibly Z-sliced) partial bake over `[rmin, rmax)` in PADDED voxel space; read back the
    // body_res³ flat buffer (pre-filled with SENTINEL). Returns (body_res, grid).
    let run = |grid_res: u32, pad: u32, rmin: [u32;3], rmax: [u32;3], z_slices: u32| -> (u32, Vec<f32>) {
        let body_res = grid_res - 2 * pad;
        let n = (body_res as u64).pow(3);
        let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_out"), size: n * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&out, 0, bytemuck::cast_slice(&vec![SENTINEL; n as usize]));
        let prim = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_prim"), size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let clip = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_clip"), size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        // Disabled mesh_grid dummy (@binding 5): zeroed header → res=0 → union no-op.
        let mesh_dummy = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_mesh_dummy"), size: 32,
            usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_params"),
            size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("spike_tex"),
            size: wgpu::Extent3d { width: grid_res, height: grid_res, depth_or_array_layers: grid_res },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[] });
        let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: mesh_dummy.as_entire_binding() }, // dense: binding 6 unread (filler)
            wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
            wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
        ] });
        let base = crate::gpu_jobs::BakeParams {
            grid_res: [grid_res; 3], _pad0: 0,
            bounds_min: [-1.0; 3], _pad1: 0.0, bounds_max: [1.0; 3],
            prim_count: 0, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32, dims_bricks: [0; 3],
            bake_min: rmin, _pad3: 0, bake_max: rmax, _pad4: 0,
            mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
            atlas_offset: [0; 3], atlas_pad: pad, output_mode: 0, _pad5: 0,
            body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
        };
        let wg = |lo: u32, hi: u32| (hi - lo + 3) / 4;
        let (z0, z1) = (rmin[2], rmax[2]);
        let span = z1 - z0;
        for s in 0..z_slices {
            let sz0 = z0 + span * s / z_slices;
            let sz1 = z0 + span * (s + 1) / z_slices;
            if sz1 <= sz0 { continue; }
            let mut p = base; p.bake_min[2] = sz0; p.bake_max[2] = sz1;
            queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
                cp.set_pipeline(&pipeline); cp.set_bind_group(0, &bg, &[]);
                cp.dispatch_workgroups(wg(rmin[0], rmax[0]), wg(rmin[1], rmax[1]), wg(sz0, sz1));
            }
            queue.submit(std::iter::once(enc.finish()));
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("spike_stg"), size: n * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, n * 4);
        queue.submit(std::iter::once(enc.finish()));
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let g: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        staging.unmap();
        (body_res, g)
    };

    // The flat buffer must be BAKED exactly at body voxels whose padded coord ∈ [rmin, rmax), and SENTINEL
    // (byte-identical) everywhere else.
    let check = |name: &str, body_res: u32, g: &[f32], pad: u32, rmin: [u32;3], rmax: [u32;3]| {
        let inb = |bv: u32, a: usize| -> bool { let v = bv + pad; v >= rmin[a] && v < rmax[a] };
        let (mut written, mut bad) = (0usize, 0usize);
        for z in 0..body_res { for y in 0..body_res { for x in 0..body_res {
            let expect = inb(x, 0) && inb(y, 1) && inb(z, 2);
            let val = g[(x + y * body_res + z * body_res * body_res) as usize];
            if expect { written += 1; if val != BAKED { bad += 1; } }
            else if val != SENTINEL { bad += 1; }
        }}}
        eprintln!("[BakeSpike] {name}: body_res={body_res} written={written} mismatches={bad}");
        assert!(written > 0, "{name}: dispatched region wrote nothing");
        assert_eq!(bad, 0, "{name}: partial-bake footprint mismatch — stomped or missed voxels (in-place accumulation is UNSAFE; use path B)");
    };

    // V1: no pad, single dispatch, interior sub-region.
    let (br, g) = run(64, 0, [16, 16, 16], [48, 48, 48], 1);
    check("pad0_single", br, &g, 0, [16, 16, 16], [48, 48, 48]);
    // V2: atlas pad=2 (grid 68 / body 64). Region in PADDED space spanning pad+body voxels.
    let (br, g) = run(68, 2, [10, 10, 10], [54, 54, 54], 1);
    check("pad2", br, &g, 2, [10, 10, 10], [54, 54, 54]);
    // V3: no pad, region split into 5 separate-submit Z-slices — union must equal a single dispatch.
    let (br, g) = run(64, 0, [8, 8, 8], [56, 56, 56], 5);
    check("pad0_zslice5", br, &g, 0, [8, 8, 8], [56, 56, 56]);
}

/// Body-mirror UNION parity: the GPU bake (`volume_bake.wgsl` SCOPE_OPEN/CLOSE stack-machine
/// re-seek) MUST produce the SAME signed-distance field as the canonical CPU `SceneEvaluator`
/// (`eval_scene_body_mirrored`). The CPU is the spec. This is the regression guard for the
/// "symmetry-on EXPLODES the mesh" bug: a divergent union field → garbage voxels → scattered
/// DC triangles. Bakes a real serialized body at `pad=0` so the flat readback maps 1:1 to the
/// grid, then compares every voxel's sign (interior/exterior) and value against the CPU.
///
/// Covers: (a) `mflags==0` byte-identical (the common path stays unchanged), (b) single-axis
/// off-center mirror (the reported failure), (c) two-axis mirror (4 combos, exercises the
/// multi-bit re-seek), (d) a two-prim body inside the mirrored scope (exercises the per-prim
/// boolean-accumulate being applied once per combo, not leaked across combos).
#[test]
fn body_mirror_bake_matches_cpu_eval() {
    use core_math::scene_graph::{SceneGraph, NodePayload};
    use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BlendProfile, PbrMaterial};
    use core_math::sdf_shapes::BoxBevel;
    use core_math::field_eval::{SceneEvaluator, FieldEvaluator};
    use glam::Quat;

    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[MirrorBake] no GPU adapter — skipping"); return; }
    };

    // Build the bake compute pipeline — BGL entries from the SINGLE SOURCE
    // graphics::bgl::bake_bgl_entries() (identical to the production bake).
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirror_bake_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mirror volume_bake"),
        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("mirror_bake"), layout: Some(&pl), module: &shader, entry_point: Some("bake_volume"),
        compilation_options: Default::default(), cache: None });

    // Bake a serialized body into a `res³` flat field (pad=0 → grid==body, flat maps 1:1).
    let bake = |sg: &SceneGraph, body: core_math::scene_graph::NodeId, res: u32, gmin: Vec3, gmax: Vec3| -> Vec<f32> {
        let mut prim_floats = graphics::serialize_body_for_bake_hot(sg, body, Vec3::ZERO);
        let clip_floats = graphics::serialize_clip_curves_for_bake(sg, body, &mut prim_floats);
        let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;

        let n = (res as u64).pow(3);
        let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_out"), size: n * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let prim = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_prim"),
            size: (prim_floats.len() as u64 * 4).max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&prim, 0, bytemuck::cast_slice(&prim_floats));
        let clip = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_clip"),
            size: (clip_floats.len() as u64 * 4).max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        if !clip_floats.is_empty() { queue.write_buffer(&clip, 0, bytemuck::cast_slice(&clip_floats)); }
        // brick_index (@binding 5) dummy — dense mode never reads it (SparseBricks-only).
        let brick_idx = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_brick_idx"), size: 4,
            usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_params"),
            size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        // Disabled mesh_grid dummy (@binding 5): zeroed header → res=0 → union no-op.
        let mesh_dummy = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_mesh_dummy"), size: 32,
            usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("mirror_tex"),
            size: wgpu::Extent3d { width: res, height: res, depth_or_array_layers: res },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[] });
        let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: mesh_dummy.as_entire_binding() }, // dense: binding 6 unread (filler)
            wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
            wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
        ] });
        let p = crate::gpu_jobs::BakeParams {
            grid_res: [res; 3], _pad0: 0,
            bounds_min: gmin.to_array(), _pad1: 0.0, bounds_max: gmax.to_array(),
            prim_count, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32, dims_bricks: [0; 3],
            bake_min: [0; 3], _pad3: 0, bake_max: [res; 3], _pad4: 0,
            // mirror_* params are NOT consumed by the union (it lives in the SCOPE marker);
            // pass the body's values anyway to mirror production wiring exactly.
            mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
            atlas_offset: [0; 3], atlas_pad: 0, output_mode: 0, _pad5: 0,
            body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
        };
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));
        let wg = (res + 3) / 4;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            cp.set_pipeline(&pipeline); cp.set_bind_group(0, &bg, &[]);
            cp.dispatch_workgroups(wg, wg, wg);
        }
        queue.submit(std::iter::once(enc.finish()));
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror_stg"), size: n * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, n * 4);
        queue.submit(std::iter::once(enc.finish()));
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let g: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        staging.unmap();
        g
    };

    // Build a body with one or two boxes + a body-level mirror. Origin at world ZERO,
    // identity transform → world == body-local (matches the bake, origin ZERO).
    let make_body = |boxes: &[(Vec3, Vec3)], mflags: u8, msign: u8, mc: Vec3| -> (SceneGraph, core_math::scene_graph::NodeId) {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let prims: Vec<SdfPrimitive> = boxes.iter().map(|&(pos, he)| {
            SdfPrimitive::new("Box".into(), SdfShape::Box { half_extents: he, bevel: BoxBevel::default() }, pos)
        }).collect();
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Body".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: mflags, mirror_sign: msign, mirror_center: mc,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        (sg, body)
    };

    // Compare GPU bake field vs CPU SceneEvaluator over the whole grid. Reports sign
    // disagreements (interior/exterior flips — the load-bearing topology) and the worst
    // value delta. A correct union has ZERO sign flips and a tiny value delta.
    let compare = |name: &str, sg: &SceneGraph, body: core_math::scene_graph::NodeId, res: u32, gmin: Vec3, gmax: Vec3| {
        let g = bake(sg, body, res, gmin, gmax);
        let eval = SceneEvaluator::new(sg);
        let ext = gmax - gmin;
        let s = (res - 1) as f32;
        let voxel = (ext / s).min_element();
        let (mut sign_flips, mut nan_count, mut near_dv) = (0usize, 0usize, 0.0f32);
        let mut worst: (Vec3, f32, f32) = (Vec3::ZERO, 0.0, 0.0);
        for z in 0..res { for y in 0..res { for x in 0..res {
            let p = gmin + (Vec3::new(x as f32, y as f32, z as f32) / s) * ext;
            let gpu = g[(x + y * res + z * res * res) as usize];
            if !gpu.is_finite() { nan_count += 1; continue; }
            let cpu = eval.eval(p);
            // Sign (inside/outside) is what the DC mesher reads — it MUST agree. Allow a
            // 1-voxel skin near the surface where quantization can legitimately disagree on
            // sign without being a topology error.
            if (gpu < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && gpu.abs() > voxel {
                sign_flips += 1;
            }
            // Value parity is only meaningful NEAR the zero-crossing (where DC interpolates the
            // vertex). Far-field magnitude legitimately differs: the GPU `eval_prim` box returns a
            // bounded distance while the CPU exact box distance keeps growing — irrelevant to the mesh.
            if cpu.abs() < 3.0 * voxel {
                let dv = (gpu - cpu).abs();
                if dv > near_dv { near_dv = dv; worst = (p, gpu, cpu); }
            }
        }}}
        eprintln!("[MirrorBake] {name}: res={res} sign_flips={sign_flips} nan={nan_count} near_surface_max|dv|={near_dv:.4} \
            worst@({:.2},{:.2},{:.2}) gpu={:.3} cpu={:.3}", worst.0.x, worst.0.y, worst.0.z, worst.1, worst.2);
        assert_eq!(nan_count, 0, "{name}: GPU bake produced non-finite voxels (field corruption → mesh explosion)");
        assert_eq!(sign_flips, 0, "{name}: GPU bake disagrees with CPU on inside/outside at {sign_flips} voxels (union field corrupt → scattered DC triangles)");
        assert!(near_dv < 0.05, "{name}: GPU bake value diverges from CPU near the surface (max|dv|={near_dv:.4}) — union not faithful");
    };

    let res = 48u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));

    // (a) NO mirror — byte-identical common path. Off-center box, no mirror: only -Y solid.
    let (sg, b) = make_body(&[(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5))], 0, 0, Vec3::ZERO);
    compare("no_mirror", &sg, b, res, gmin, gmax);

    // (b) Single-axis (Y) off-center mirror about y=0 — THE REPORTED BUG. Both halves solid.
    let (sg, b) = make_body(&[(Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5))], 0b010, 0, Vec3::ZERO);
    compare("mirror_Y_offcenter", &sg, b, res, gmin, gmax);

    // (c) Two-axis (X+Y) mirror, off-center box → 4 reflected copies (4 combos).
    let (sg, b) = make_body(&[(Vec3::new(0.8, -1.0, 0.0), Vec3::splat(0.4))], 0b011, 0, Vec3::ZERO);
    compare("mirror_XY_offcenter", &sg, b, res, gmin, gmax);

    // (d) Two boxes inside the mirrored scope (per-prim union accumulate must be clean per combo).
    let (sg, b) = make_body(&[
        (Vec3::new(0.0, -1.0, 0.0), Vec3::splat(0.5)),
        (Vec3::new(0.7, -1.2, 0.3), Vec3::splat(0.3)),
    ], 0b010, 0, Vec3::ZERO);
    compare("mirror_Y_two_prims", &sg, b, res, gmin, gmax);

    // (e) Box STRADDLING the mirror plane (geometry crosses y=0). The union must keep the
    // whole straddling shape — a fold would have deleted the half on the non-kept side.
    let (sg, b) = make_body(&[(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.4, 0.9, 0.4))], 0b010, 0, Vec3::ZERO);
    compare("mirror_Y_straddle", &sg, b, res, gmin, gmax);

    // (f) OFF-ORIGIN mirror plane (mirror_center.y = 0.5). Reflection center != 0 stresses the
    // `2*c - p` term (a c=0 test can't catch a center-vs-origin mix-up).
    let (sg, b) = make_body(&[(Vec3::new(0.0, -0.5, 0.0), Vec3::splat(0.4))], 0b010, 0, Vec3::new(0.0, 0.5, 0.0));
    compare("mirror_Y_offorigin_plane", &sg, b, res, gmin, gmax);

    // (g) THREE-axis mirror (8 combos) — the maximum re-seek count. Off-center on all axes.
    let (sg, b) = make_body(&[(Vec3::new(0.7, 0.6, 0.8), Vec3::splat(0.35))], 0b111, 0, Vec3::ZERO);
    compare("mirror_XYZ_octant", &sg, b, res, gmin, gmax);

    // ---- CUT mode (mirror_sign bit 7): one-sided fold, "delete the other half" ----
    // The GPU takes a DIFFERENT branch here (ncombo forced to 1, fold instead of reflect), so
    // every case below re-proves CPU/GPU parity on that branch. `compare` alone cannot fail a
    // mode that deletes EVERYTHING (CPU and GPU would agree on an empty field), so each case
    // also probes the CPU field directly: kept side solid, discarded side empty.
    const CUT: u8 = core_math::body_field_ops::MIRROR_CUT_BIT;
    let probe = |name: &str, sg: &SceneGraph, pts: &[(Vec3, bool)]| {
        let eval = SceneEvaluator::new(sg);
        for &(p, want_solid) in pts {
            let d = eval.eval(p);
            assert_eq!(d < 0.0, want_solid,
                "{name}: ({:.2},{:.2},{:.2}) should be {} — got d={d:+.4}",
                p.x, p.y, p.z, if want_solid { "SOLID" } else { "EMPTY" });
        }
    };

    // Two boxes on OPPOSITE halves and opposite x, so union and cut give visibly different
    // fields: the union keeps all four blobs, the cut keeps only the source-half box + its
    // reflection. A symmetric or straddling single box cannot tell the two modes apart (its
    // fold image and its union both come out symmetric) — hence the asymmetric pair.
    let two_sided = [
        (Vec3::new(-0.8, -1.0, 0.0), Vec3::splat(0.35)), // A: −Y half
        (Vec3::new(0.8, 1.0, 0.0), Vec3::splat(0.35)),   // B: +Y half
    ];

    // (j) CUT Y keeping +Y: B survives and mirrors to −Y; A is deleted with its whole column.
    let (sg, b) = make_body(&two_sided, 0b010, CUT, Vec3::ZERO);
    compare("cut_Y_keep_plus", &sg, b, res, gmin, gmax);
    probe("cut_Y_keep_plus", &sg, &[
        (Vec3::new(0.8, 1.0, 0.0), true),    // source B
        (Vec3::new(0.8, -1.0, 0.0), true),   // B mirrored
        (Vec3::new(-0.8, -1.0, 0.0), false), // A deleted (was solid under the union)
        (Vec3::new(-0.8, 1.0, 0.0), false),  // A's reflection never appears
    ]);

    // (k) Same body, flip Y (mirror_sign bit 1) — the source half swaps to −Y, so A survives
    // and B is the one deleted. Proves the flip bits are LIVE (they were inert under the union).
    let (sg, b) = make_body(&two_sided, 0b010, CUT | 0b010, Vec3::ZERO);
    compare("cut_Y_keep_minus", &sg, b, res, gmin, gmax);
    probe("cut_Y_keep_minus", &sg, &[
        (Vec3::new(-0.8, -1.0, 0.0), true),  // source A
        (Vec3::new(-0.8, 1.0, 0.0), true),   // A mirrored
        (Vec3::new(0.8, 1.0, 0.0), false),   // B deleted
        (Vec3::new(0.8, -1.0, 0.0), false),
    ]);

    // (l) TWO-axis cut (X+Y): still ONE fold, four quadrants from the +X+Y source box.
    let (sg, b) = make_body(&[(Vec3::new(0.8, 1.0, 0.0), Vec3::splat(0.35))], 0b011, CUT, Vec3::ZERO);
    compare("cut_XY_quadrants", &sg, b, res, gmin, gmax);
    probe("cut_XY_quadrants", &sg, &[
        (Vec3::new(0.8, 1.0, 0.0), true),
        (Vec3::new(-0.8, 1.0, 0.0), true),
        (Vec3::new(0.8, -1.0, 0.0), true),
        (Vec3::new(-0.8, -1.0, 0.0), true),
    ]);

    // (m) CUT about an OFF-ORIGIN plane (y = 0.5) — stresses `c ± |p−c|` against a c=0 pass.
    let (sg, b) = make_body(&[(Vec3::new(0.0, 1.2, 0.0), Vec3::splat(0.35))], 0b010, CUT, Vec3::new(0.0, 0.5, 0.0));
    compare("cut_Y_offorigin_plane", &sg, b, res, gmin, gmax);
    probe("cut_Y_offorigin_plane", &sg, &[
        (Vec3::new(0.0, 1.2, 0.0), true),   // source
        (Vec3::new(0.0, -0.2, 0.0), true),  // reflected about y=0.5
        (Vec3::new(0.0, 0.5, 0.0), false),  // on the plane, between the halves
    ]);

    // (h) Sphere (different `eval_prim` shape) under a Y mirror — proves the union is shape-agnostic.
    {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let prim = SdfPrimitive::new("Sphere".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(0.0, -1.0, 0.0));
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Body".into(), primitives: vec![prim], mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0b010, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        compare("mirror_Y_sphere", &sg, body, res, gmin, gmax);
    }

    // (i) NESTED SUB-BODY parity (defect (1) of the tracker's "boolean tree inside a
    // sub-body" entry): the CPU SceneEvaluator must now match the GPU scope machine on a
    // nested boolean tree, over the WHOLE grid. SPHERES on purpose — historically the CPU box
    // eval was tri-planar (divergent from the GPU near corners; FIXED 2026-07-13, see
    // gpu_classify_box_euclidean_alignment_gate) and this test predates the fix;
    // sphere eval is formula-identical on both paths, so any voxel disagreement here is a
    // real scope-semantics divergence. Non-zero sub-body origin exercises the prim_shift
    // composition; the smooth variant exercises the scope-level apply_boolean k.
    {
        let mk_nested = |sub_op: BooleanOp| -> (SceneGraph, core_math::scene_graph::NodeId) {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let mut p = SdfPrimitive::new("P".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
            p.operation = BooleanOp::Union { k: 0.0 };
            let top = sg.add_child(root, NodePayload::SdfBody {
                name: "Top".into(), primitives: vec![p], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            let sub_origin = Vec3::new(0.6, 0.2, 0.0);
            let mut a = SdfPrimitive::new("A".into(), SdfShape::Sphere { radius: 0.7 }, Vec3::new(0.8, 0.0, 0.0) - sub_origin);
            a.operation = BooleanOp::Union { k: 0.0 };
            let mut b = SdfPrimitive::new("B".into(), SdfShape::Sphere { radius: 0.4 }, Vec3::new(0.8, 0.4, 0.0) - sub_origin);
            b.operation = BooleanOp::Subtract { k: 0.0 };
            let _sub = sg.add_child(top, NodePayload::SdfBody {
                name: "Sub".into(), primitives: vec![a, b], mesh_cache_id: None,
                body_op: sub_op, origin: sub_origin,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            (sg, top)
        };
        let (sg, top) = mk_nested(BooleanOp::Subtract { k: 0.0 });
        compare("nested_sub_subtract_spheres", &sg, top, res, gmin, gmax);
        let (sg, top) = mk_nested(BooleanOp::Union { k: 0.25 });
        compare("nested_sub_smooth_union_spheres", &sg, top, res, gmin, gmax);
    }

    // (j) SEAM boolean parity gates (ITEM 5): CPU fold_prim/apply_scope_boolean seam_op vs the
    // PRODUCTION volume_bake.wgsl seam_cut — same harness, all 3 profiles x both polarities +
    // a sub-body Seam body_op (scope dispatch). Spheres (formula-identical eval on both paths),
    // seam prim crossing the base surface so the groove/bead actually exists in the grid.
    {
        let mk_seam = |profile: u8, raised: bool| -> (SceneGraph, core_math::scene_graph::NodeId) {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let mut base = SdfPrimitive::new("Base".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
            base.operation = BooleanOp::Union { k: 0.0 };
            let mut seam = SdfPrimitive::new("Seam".into(), SdfShape::Sphere { radius: 0.6 }, Vec3::new(0.9, 0.2, 0.0));
            seam.operation = BooleanOp::Seam { width: 0.08, depth: 0.06, profile, raised };
            let body = sg.add_child(root, NodePayload::SdfBody {
                name: "Body".into(), primitives: vec![base, seam], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            (sg, body)
        };
        for (profile, pname) in [(0u8, "V"), (1u8, "U"), (2u8, "square")] {
            for raised in [false, true] {
                let (sg, b) = mk_seam(profile, raised);
                let tag = if raised { "bead" } else { "groove" };
                compare(&format!("seam_{pname}_{tag}_sphere"), &sg, b, res, gmin, gmax);
            }
        }

        // Sub-body Seam: the seam driver is a CHILD SCOPE's field (body_op = Seam) — pins the
        // scope-machine dispatch (op 5/6 at SCOPE_CLOSE + scope AABB seam skip) vs the CPU
        // apply_scope_boolean arm.
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut base = SdfPrimitive::new("Base".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        base.operation = BooleanOp::Union { k: 0.0 };
        let top = sg.add_child(root, NodePayload::SdfBody {
            name: "Top".into(), primitives: vec![base], mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let sub_origin = Vec3::new(0.9, 0.2, 0.0);
        let mut sub_prim = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 0.6 }, Vec3::ZERO);
        sub_prim.operation = BooleanOp::Union { k: 0.0 };
        let _sub = sg.add_child(top, NodePayload::SdfBody {
            name: "SeamSub".into(), primitives: vec![sub_prim], mesh_cache_id: None,
            body_op: BooleanOp::Seam { width: 0.08, depth: 0.06, profile: 1, raised: false },
            origin: sub_origin,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        compare("seam_subbody_scope_U_groove", &sg, top, res, gmin, gmax);
    }

    // (k) EXTRUDE boolean parity gates: CPU fold_prim/apply_scope_boolean extrude_op vs the
    // PRODUCTION volume_bake.wgsl extrude_op (op 7) — same harness, BOTH signs (raise adds
    // material within the footprint, recess removes) + a sub-body Extrude body_op (scope
    // dispatch, op 7 at SCOPE_CLOSE + the acc-independent op-7 culls). Spheres
    // (formula-identical eval on both paths), extrude prim crossing the base surface so the
    // raised pad / recessed pocket actually exists in the grid.
    {
        let mk_extrude = |strength: f32| -> (SceneGraph, core_math::scene_graph::NodeId) {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let mut base = SdfPrimitive::new("Base".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
            base.operation = BooleanOp::Union { k: 0.0 };
            let mut ext = SdfPrimitive::new("Ext".into(), SdfShape::Sphere { radius: 0.6 }, Vec3::new(0.9, 0.2, 0.0));
            ext.operation = BooleanOp::Extrude { strength, width: 0.08 };
            let body = sg.add_child(root, NodePayload::SdfBody {
                name: "Body".into(), primitives: vec![base, ext], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            (sg, body)
        };
        for (strength, tag) in [(0.06f32, "raise"), (-0.06f32, "recess")] {
            let (sg, b) = mk_extrude(strength);
            compare(&format!("extrude_{tag}_sphere"), &sg, b, res, gmin, gmax);
        }

        // Sub-body Extrude: the footprint driver is a CHILD SCOPE's field (body_op =
        // Extrude) — pins the scope-machine dispatch vs the CPU apply_scope_boolean arm.
        for (strength, tag) in [(0.06f32, "raise"), (-0.06f32, "recess")] {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let mut base = SdfPrimitive::new("Base".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
            base.operation = BooleanOp::Union { k: 0.0 };
            let top = sg.add_child(root, NodePayload::SdfBody {
                name: "Top".into(), primitives: vec![base], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            let mut sub_prim = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 0.6 }, Vec3::ZERO);
            sub_prim.operation = BooleanOp::Union { k: 0.0 };
            let _sub = sg.add_child(top, NodePayload::SdfBody {
                name: "ExtSub".into(), primitives: vec![sub_prim], mesh_cache_id: None,
                body_op: BooleanOp::Extrude { strength, width: 0.08 },
                origin: Vec3::new(0.9, 0.2, 0.0),
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            compare(&format!("extrude_subbody_scope_{tag}"), &sg, top, res, gmin, gmax);
        }
    }

    // (l) SUB-BODY EVAL-ORDER cross-path gates (visual-notch root (B), 2026-07-17): the SAME
    // trees the CPU repro matrix characterizes (field_eval.rs [EvalOrder] tests) must agree
    // CPU↔GPU. Grouping/reordering legitimately changes the FIELD (parenthesization /
    // pairwise-smin non-associativity) — what must NEVER differ is CPU vs GPU on any one tree.
    // Spheres only (formula-identical eval on both paths).
    {
        let sph = |name: &str, pos: Vec3, r: f32, op: BooleanOp| {
            let mut p = SdfPrimitive::new(name.into(), SdfShape::Sphere { radius: r }, pos);
            p.operation = op; p
        };
        let mk_flat = |prims: Vec<SdfPrimitive>| -> (SceneGraph, core_math::scene_graph::NodeId) {
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let body = sg.add_child(root, NodePayload::SdfBody {
                name: "T".into(), primitives: prims, mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            (sg, body)
        };
        let k = 0.25f32;
        let abc = |ka: f32| -> [SdfPrimitive; 3] {[
            sph("A", Vec3::new(-0.4, 0.0, 0.0), 0.8, BooleanOp::Union { k: ka }),
            sph("B", Vec3::new(0.4, 0.1, 0.0), 0.7, BooleanOp::Union { k: ka }),
            sph("C", Vec3::new(0.0, 0.5, 0.2), 0.6, BooleanOp::Union { k: ka }),
        ]};

        // Reordered smooth-union run — both orders, each order CPU↔GPU.
        let [a, b, c] = abc(k);
        let (sg, body) = mk_flat(vec![a.clone(), b.clone(), c.clone()]);
        compare("evalorder_smooth_abc", &sg, body, res, gmin, gmax);
        let (sg, body) = mk_flat(vec![c, b, a]);
        compare("evalorder_smooth_cba", &sg, body, res, gmin, gmax);

        // Grouped smooth run under a HARD group op (the boundary-hardening notch tree).
        {
            let [a, b, c] = abc(k);
            let mut sg = SceneGraph::new();
            let root = sg.root();
            let top = sg.add_child(root, NodePayload::SdfBody {
                name: "T".into(), primitives: vec![a], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            let so = Vec3::new(0.2, 0.3, 0.1);
            let (mut bl, mut cl) = (b, c);
            bl.position -= so; cl.position -= so;
            let _sub = sg.add_child(top, NodePayload::SdfBody {
                name: "S".into(), primitives: vec![bl, cl], mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 }, origin: so,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
                bevel_radius: 0.0, blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(), child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            compare("evalorder_grouped_hard_boundary", &sg, top, res, gmin, gmax);
        }

        // Subtract moved to slot 0 (first-operand seeding rule) — both paths must seed solid.
        let (sg, body) = mk_flat(vec![
            sph("B", Vec3::new(0.0, 0.9, 0.0), 0.4, BooleanOp::Subtract { k: 0.0 }),
            sph("P", Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 }),
        ]);
        compare("evalorder_subtract_first_seed", &sg, body, res, gmin, gmax);
    }
}

/// NESTED SUB-BODY boolean tree (owner bug 2026-07-09: "subtract cube inside a Subtract sub-body
/// not working"). Scene: top body = cube P (Union) + nested child SdfBody S (body_op=Subtract{k:0})
/// containing cube A (Union) and cube B (Subtract{k:0}, overlapping A). Correct semantics:
/// field = P − (A − B). Serialized via the PRODUCTION `serialize_body_for_bake_hot` and baked with
/// the PRODUCTION `volume_bake.wgsl` (bake_volume). Probes 4 hand-computed discriminator points:
///   (a) in P, outside S            → SOLID  (negative)
///   (b) in P ∩ A, outside B        → EMPTY  (sub-body carves)
///   (c) in P ∩ A ∩ B               → SOLID  (inner Subtract removed B from A → no carve) ← THE test
///   (d) in A ∩ B, outside P        → EMPTY
/// If the inner Subtract is ignored (sub-body flattened to union of its prims), (c) reads EMPTY.
/// Optionally (env AZRA_OLD_BAKE_WGSL=path) also bakes with an alternate shader source for
/// regression bisecting; the asserts pin the CURRENT shader only.
#[test]
fn nested_subbody_boolean_tree_bake() {
    use core_math::scene_graph::{SceneGraph, NodePayload};
    use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BlendProfile, PbrMaterial};
    use core_math::sdf_shapes::BoxBevel;
    use core_math::field_eval::{SceneEvaluator, FieldEvaluator};
    use glam::Quat;

    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[NestedSub] no GPU adapter — skipping"); return; }
    };

    // --- Scene graph: top body T { P, S { A, B } } ---
    let mut sg = SceneGraph::new();
    let root = sg.root();
    let mkbox = |name: &str, pos: Vec3, he: Vec3, op: BooleanOp| {
        let mut p = SdfPrimitive::new(name.into(), SdfShape::Box { half_extents: he, bevel: BoxBevel::default() }, pos);
        p.operation = op;
        p
    };
    let prim_p = mkbox("P", Vec3::ZERO, Vec3::splat(1.0), BooleanOp::Union { k: 0.0 });
    let top = sg.add_child(root, NodePayload::SdfBody {
        name: "Top".into(), primitives: vec![prim_p], mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
        bevel_radius: 0.0, blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(), child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
    // App-realistic: a grouped sub-body has a NON-ZERO origin (recalculate_body_origin =
    // prim centroid) and prims stored LOCAL to it — exercises prim_shift in the serializer.
    let sub_origin = Vec3::new(0.8, 0.25, 0.0);
    let prim_a = mkbox("A", Vec3::new(0.8, 0.0, 0.0) - sub_origin, Vec3::splat(0.6), BooleanOp::Union { k: 0.0 });
    let prim_b = mkbox("B", Vec3::new(0.8, 0.5, 0.0) - sub_origin, Vec3::splat(0.4), BooleanOp::Subtract { k: 0.0 });
    let _sub = sg.add_child(top, NodePayload::SdfBody {
        name: "Sub".into(), primitives: vec![prim_a, prim_b], mesh_cache_id: None,
        body_op: BooleanOp::Subtract { k: 0.0 }, origin: sub_origin,
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
        bevel_radius: 0.0, blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(), child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // --- Serialize through the PRODUCTION bake path ---
    let mut prim_floats = graphics::serialize_body_for_bake_hot(&sg, top, Vec3::ZERO);
    let clip_floats = graphics::serialize_clip_curves_for_bake(&sg, top, &mut prim_floats);
    let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;
    eprintln!("[NestedSub] serialized {prim_count} entries (expect 7: OPEN P OPEN A B CLOSE CLOSE)");

    // Grid: bounds -2..2, res 41 → spacing 0.1, probe points land EXACTLY on vertices.
    let res = 41u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let n = (res as u64).pow(3);

    let bake_with = |wgsl_src: &str, label: &str| -> Vec<f32> {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nested_bgl"), entries: &graphics::bgl::bake_bgl_entries() });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(wgsl_src).into()) });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label), layout: Some(&pl), module: &shader, entry_point: Some("bake_volume"),
            compilation_options: Default::default(), cache: None });
        let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ns_out"), size: n * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mk = |label: &str, data: &[f32]| {
            let b = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label),
                size: (data.len() as u64 * 4).max(16),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            if !data.is_empty() { queue.write_buffer(&b, 0, bytemuck::cast_slice(data)); }
            b
        };
        let prim_buf = mk("ns_prim", &prim_floats);
        let clip_buf = mk("ns_clip", &clip_floats);
        let mesh_dummy = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ns_mesh"), size: 32,
            usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ns_params"),
            size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("ns_tex"),
            size: wgpu::Extent3d { width: res, height: res, depth_or_array_layers: res },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[] });
        let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
            wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
        ] });
        let p = crate::gpu_jobs::BakeParams {
            grid_res: [res; 3], _pad0: 0,
            bounds_min: gmin.to_array(), _pad1: 0.0, bounds_max: gmax.to_array(),
            prim_count, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32, dims_bricks: [0; 3],
            bake_min: [0; 3], _pad3: 0, bake_max: [res; 3], _pad4: 0,
            mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
            atlas_offset: [0; 3], atlas_pad: 0, output_mode: 0, _pad5: 0,
            body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
        };
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));
        let wg = (res + 3) / 4;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        { let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
          cp.set_pipeline(&pipeline); cp.set_bind_group(0, &bg, &[]); cp.dispatch_workgroups(wg, wg, wg); }
        queue.submit(std::iter::once(enc.finish()));
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ns_stg"), size: n * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, n * 4);
        queue.submit(std::iter::once(enc.finish()));
        let (mtx, mrx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = mtx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        mrx.recv().expect("map dropped").expect("map failed");
        let g: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        staging.unmap();
        g
    };

    // Probe points (all exact grid vertices at 0.1 spacing) with hand-computed expected signs.
    // P = box he 1.0 @ origin; A = he 0.6 @ (0.8,0,0) → x∈[0.2,1.4]; B = he 0.4 @ (0.8,0.5,0).
    let probes: [(&str, Vec3, bool); 4] = [
        ("a: in P, outside S      → SOLID", Vec3::new(-0.5, 0.0, 0.0), true),
        ("b: in P∩A, outside B    → EMPTY", Vec3::new(0.5, -0.3, 0.0), false),
        ("c: in P∩A∩B (B un-carves)→ SOLID", Vec3::new(0.8, 0.5, 0.0), true),
        ("d: in A∩B, outside P    → EMPTY", Vec3::new(1.1, 0.5, 0.0), false),
    ];
    let vidx = |p: Vec3| -> usize {
        let s = (res - 1) as f32;
        let g = (p - gmin) / (gmax - gmin) * s;
        let (x, y, z) = (g.x.round() as u32, g.y.round() as u32, g.z.round() as u32);
        (x + y * res + z * res * res) as usize
    };

    let g_cur = bake_with(graphics::VOLUME_BAKE_WGSL, "nested current");
    let g_old = std::env::var("AZRA_OLD_BAKE_WGSL").ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|src| bake_with(&src, "nested pre-7ccbb86"));

    let eval = SceneEvaluator::new(&sg);
    let mut failures = Vec::new();
    for (name, p, solid) in probes {
        let cur = g_cur[vidx(p)];
        let old = g_old.as_ref().map(|g| g[vidx(p)]);
        let cpu_scene = eval.eval(p);
        let cpu_body = eval.eval_body(top, p);
        eprintln!("[NestedSub] {name} @({:.1},{:.1},{:.1}): gpu_cur={cur:+.3} gpu_old={:?} cpu_scene={cpu_scene:+.3} cpu_body_top={cpu_body:+.3} expect {}",
            p.x, p.y, p.z, old, if solid { "NEG" } else { "POS" });
        if (cur < 0.0) != solid { failures.push(name); }
        // CPU↔GPU parity (defect (1) fixed): the scope-aware CPU evaluator must agree with
        // the GPU oracle at the discriminator probes. Probes sit on box FACE interiors
        // (corner regions historically diverged — CPU tri-planar box eval, aligned to the
        // GPU exact-Euclidean form 2026-07-13).
        assert!((cpu_scene < 0.0) == solid,
            "CPU SceneEvaluator wrong sign at {name}: {cpu_scene:+.4}");
        assert!((cpu_body < 0.0) == solid,
            "CPU eval_body(top) wrong sign at {name}: {cpu_body:+.4}");
        assert!((cur - cpu_scene).abs() < 2e-3,
            "CPU↔GPU value divergence at {name}: gpu={cur:+.4} cpu={cpu_scene:+.4}");
    }
    assert!(failures.is_empty(),
        "nested sub-body boolean tree wrong in GPU bake at: {failures:?} — P − (A − B) semantics violated");
}

/// LAB poly→SDF (iteration 2) — GPU↔CPU PARITY for a `SdfShape::MeshVolume` prim through the
/// REAL bake path: serialize_body_for_bake_hot (st==10) + serialize_mesh_grids_for_bake (side
/// buffer @binding 5 + hot[86] offset) + volume_bake.wgsl eval_mesh_volume in the scope loop.
/// Bakes a body containing one MeshVolume prim (an analytic sphere grid) and asserts the GPU
/// field matches the CPU SceneEvaluator (which samples the same grid via MeshSdfData::sample) —
/// proving the whole serialize→shader chain is faithful. Also prints the bake time.
/// Log: docs/planning/lab/poly-mesh-sdf-operand.md.
/// Run: cargo test -p app --release mesh_volume_bake_matches_cpu -- --ignored --nocapture
#[test]
#[ignore] // GPU test — run explicitly
fn mesh_volume_bake_matches_cpu() {
    use core_math::scene_graph::{SceneGraph, NodePayload};
    use core_math::primitives::{SdfPrimitive, BooleanOp, BlendProfile, PbrMaterial};
    use core_math::sdf_shapes::{SdfShape, MeshSdfData};
    use core_math::field_eval::{SceneEvaluator, FieldEvaluator};
    use glam::Quat;
    use std::sync::Arc;

    let (_gpu_lock, device, queue) = match headless_device() {
        Some(dq) => dq, None => { eprintln!("[MeshVolBake] no GPU adapter — skipping"); return; }
    };

    // Pipeline — SINGLE SOURCE bake BGL (binding 5 = mesh_grid) + real bake shader.
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("meshvol_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("meshvol volume_bake"),
        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("meshvol_bake"), layout: Some(&pl), module: &shader, entry_point: Some("bake_volume"),
        compilation_options: Default::default(), cache: None });

    // Analytic sphere SDF baked into a shape-local (centered) grid — exactly what
    // convert_tris_to_mesh_grid produces, minus the mesh→SDF sampling.
    let gres = 40u32;
    let half = 1.2f32;
    let cell = 2.0 * half / (gres as f32 - 1.0);
    let sphere_r = 0.7f32;
    let mut dist = vec![0.0f32; (gres as usize).pow(3)];
    for z in 0..gres { for y in 0..gres { for x in 0..gres {
        let p = Vec3::splat(-half) + Vec3::new(x as f32, y as f32, z as f32) * cell;
        dist[(x + y * gres + z * gres * gres) as usize] = p.length() - sphere_r;
    }}}
    let grid = MeshSdfData { res: gres, bounds_min: Vec3::splat(-half), cell, dist: Arc::new(dist) };

    // Body: one MeshVolume prim (Union) offset from origin — proves the per-prim transform
    // (position) places the grid, and hot[86] offset addressing is correct.
    let prim_pos = Vec3::new(0.3, -0.2, 0.1);
    let mut sg = SceneGraph::new();
    let root = sg.root();
    let prim = SdfPrimitive::new("Mesh".into(), SdfShape::MeshVolume { grid: Arc::new(grid), source_poly: None }, prim_pos);
    let body = sg.add_child(root, NodePayload::SdfBody {
        name: "Body".into(), primitives: vec![prim], mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
        bevel_radius: 0.0, blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // Serialize through the REAL bake path.
    let mut prim_floats = graphics::serialize_body_for_bake_hot(&sg, body, Vec3::ZERO);
    let clip_floats = graphics::serialize_clip_curves_for_bake(&sg, body, &mut prim_floats);
    let mesh_floats = graphics::serialize_mesh_grids_for_bake(&sg, body, &mut prim_floats);
    let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;
    assert!(mesh_floats.len() > 8, "mesh grid side buffer should contain a packed grid, got {}", mesh_floats.len());

    let res = 48u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let n = (res as u64).pow(3);
    let mk = |label: &str, data: &[f32], usage: wgpu::BufferUsages| {
        let b = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label),
            size: (data.len() as u64 * 4).max(4), usage, mapped_at_creation: false });
        queue.write_buffer(&b, 0, bytemuck::cast_slice(data));
        b
    };
    let out = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mv_out"), size: n * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let prim_buf = mk("mv_prim", &prim_floats, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
    let clip_buf = mk("mv_clip", &clip_floats, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
    let mesh_buf = mk("mv_mesh", &mesh_floats, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mv_params"),
        size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("mv_tex"),
        size: wgpu::Extent3d { width: res, height: res, depth_or_array_layers: res },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[] });
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
        wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 5, resource: mesh_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 6, resource: mesh_buf.as_entire_binding() }, // dense: binding 6 unread (filler)
        wgpu::BindGroupEntry { binding: 7, resource: mesh_buf.as_entire_binding() }, // curv_positions: unread here (filler)
        wgpu::BindGroupEntry { binding: 8, resource: mesh_buf.as_entire_binding() }, // T1 height_maps: unread here (filler)
    ] });
    let p = crate::gpu_jobs::BakeParams {
        grid_res: [res; 3], _pad0: 0,
        bounds_min: gmin.to_array(), _pad1: 0.0, bounds_max: gmax.to_array(),
        prim_count, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32, dims_bricks: [0; 3],
        bake_min: [0; 3], _pad3: 0, bake_max: [res; 3], _pad4: 0,
        mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
        atlas_offset: [0; 3], atlas_pad: 0, output_mode: 0, _pad5: 0,
        body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
    };
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));

    let wg = (res + 3) / 4;
    let t0 = std::time::Instant::now();
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    { let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
      cp.set_pipeline(&pipeline); cp.set_bind_group(0, &bg, &[]); cp.dispatch_workgroups(wg, wg, wg); }
    queue.submit(std::iter::once(enc.finish()));
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let bake_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mv_stg"), size: n * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&out, 0, &staging, 0, n * 4);
    queue.submit(std::iter::once(enc.finish()));
    let (mtx, mrx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = mtx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    mrx.recv().expect("map dropped").expect("map failed");
    let g: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();

    // Compare GPU field vs CPU SceneEvaluator (same grid, sampled on the CPU).
    let eval = SceneEvaluator::new(&sg);
    let ext = gmax - gmin;
    let s = (res - 1) as f32;
    let voxel = (ext / s).min_element();
    let (mut sign_flips, mut nan, mut near_dv) = (0usize, 0usize, 0.0f32);
    let mut inside = 0usize;
    for z in 0..res { for y in 0..res { for x in 0..res {
        let pt = gmin + (Vec3::new(x as f32, y as f32, z as f32) / s) * ext;
        let gpu = g[(x + y * res + z * res * res) as usize];
        if !gpu.is_finite() { nan += 1; continue; }
        let cpu = eval.eval(pt);
        if gpu < 0.0 { inside += 1; }
        if (gpu < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && gpu.abs() > voxel { sign_flips += 1; }
        if cpu.abs() < 3.0 * voxel { near_dv = near_dv.max((gpu - cpu).abs()); }
    }}}
    eprintln!("[MeshVolBake] res={res} bake={bake_ms:.2}ms inside_voxels={inside} sign_flips={sign_flips} nan={nan} near|dv|={near_dv:.4}");
    assert_eq!(nan, 0, "GPU bake produced non-finite voxels");
    assert!(inside > 100, "MeshVolume sphere should carve out a solid interior (got {inside} inside voxels) — mesh grid not sampled?");
    assert_eq!(sign_flips, 0, "GPU MeshVolume bake disagrees with CPU on inside/outside at {sign_flips} voxels");
    assert!(near_dv < 0.05, "GPU MeshVolume bake value diverges from CPU near surface (max|dv|={near_dv:.4})");
}
