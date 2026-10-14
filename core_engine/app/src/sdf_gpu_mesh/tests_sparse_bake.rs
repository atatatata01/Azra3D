//! S2.3 — HARDWARE parity harness for the SparseBricks bake mode (`BakeOutputMode::SparseBricks = 2`,
//! S2.2). TESTS ONLY — proves the (production-unreachable) mode-2 shader branch correct end-to-end
//! on a real adapter before any dispatch path can construct it:
//!
//!   CPU `classify_prims_to_sparse_index` (S2.1) → REAL `volume_bake.wgsl` dispatched with
//!   output_mode=2 (binding 5 = the classifier index verbatim, binding 2 = the brick POOL arena)
//!   → pool readback → CPU `BrickGridAccessor` decode → compared against BOTH
//!     (a) the dense CPU oracle `bake_utils::bake_body_grid` (the bake spec), using the SAME
//!         reused criteria as `tests_bake::body_mirror_bake_matches_cpu_eval` — zero sign flips
//!         outside a 1-voxel skin, near-surface (|cpu| < 3·voxel) max |dv| < 0.05 — and
//!     (b) the SAME shader's mode-0 DENSE output, voxel-for-voxel BIT-EXACT in handled bricks:
//!         mode 2 writes the same `acc` mode 0 writes, so any difference is a ROUTING bug
//!         (brick_linear math / local-offset / sentinel compare), never eval noise.
//!
//!   The mode-0 dispatch doubles as the NEGATIVE CONTROL: same scene, dense mode, dummy 4-byte
//!   binding 5 (exactly what production binds) — asserting it still matches the oracle is the
//!   runtime proof that mode 2's presence did not perturb mode 0.
//!
//! Structure mirrors `tests_bake.rs`: same BGL single source (`graphics::bgl::bake_bgl_entries`),
//! same shader build (`stub_out_blend_noise_wgsl(VOLUME_BAKE_WGSL)`), same runtime
//! adapter-availability gate (no adapter → tests SKIP with a message, not fail — the repo's GPU
//! test convention; none of tests_bake/tests_dc use `#[ignore]`).
//!
//! Searched for existing implementations before writing (REUSE rule): the dispatch/readback
//! harness pattern is test-local closures in `tests_bake.rs` (extracted here into `dispatch_bake`
//! with a Dense/Sparse output switch instead of duplicating it twice); the `sphere_op`/`box_op`
//! prim builders exist only as `#[cfg(test)]`-private helpers inside `core_math/chunked_dc.rs`
//! (not importable across crates — minimal re-statement here, same shapes/ops as the S2.1
//! canonical scenes); the CPU oracle and classifier are consumed directly, never re-implemented.
#![cfg(test)]

use eframe::wgpu;
use glam::{Quat, Vec3};
use std::collections::HashMap;

use super::tests_dc::headless_device;
use core_math::chunked_dc::{
    classify_prims_to_sparse_index, dilate_voxel_window, sparse_index_translate,
    sparse_window_allocate_incompatible,
    sparse_window_bricks_all_handled,
    sparse_window_tags_compatible, BrickClass, BrickClassifier,
    BrickGridAccessor, SparseIndexOnly, VolumeAccessor,
    BRICK_FAR, BRICK_TAG_EXTERIOR, BRICK_TAG_INTERIOR,
};
use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial, SdfPrimitive, SdfShape};
use core_math::scene_graph::{NodeId, NodePayload, SceneGraph};
use core_math::sdf_shapes::BoxBevel;

/// Near-surface GPU-vs-CPU value tolerance — REUSED from
/// `tests_bake::body_mirror_bake_matches_cpu_eval` (`near_dv < 0.05`), not invented here.
/// Applied only where |cpu| < 3·voxel, exactly like the mirror-parity test: far-field magnitude
/// legitimately differs (the GPU `eval_prim` box returns a bounded distance while the CPU exact
/// box distance keeps growing — irrelevant to the mesh).
const NEAR_TOL: f32 = 0.05;

/// Pool pre-fill garbage. The bake grid is `res % 8 == 0`, so every handled brick lies fully
/// in-body and the shader must overwrite ALL `allocated·512` pool floats — any survivor is a
/// missed write. A finite impossible magnitude (real |d| here is ≤ a few world units), not NaN,
/// so bit-pattern survival checks stay exact.
const POOL_GARBAGE: f32 = -12345.5;

// ── prim builders (same shapes/ops as the S2.1 canonical scenes in chunked_dc.rs tests) ─────────

fn sphere_op(pos: Vec3, radius: f32, op: BooleanOp) -> SdfPrimitive {
    let mut p = SdfPrimitive::new("s".into(), SdfShape::Sphere { radius }, pos);
    p.operation = op;
    p
}

fn box_op(pos: Vec3, he: Vec3, op: BooleanOp) -> SdfPrimitive {
    let mut p = SdfPrimitive::new(
        "b".into(),
        SdfShape::Box { half_extents: he, bevel: BoxBevel::default() },
        pos,
    );
    p.operation = op;
    p
}

/// Body wrapper for GPU serialization — same construction as `tests_bake::make_body`. Origin at
/// world ZERO, identity transform → world == body-local (matches the bake frame, origin ZERO).
fn make_body(prims: Vec<SdfPrimitive>, mflags: u8, mc: Vec3) -> (SceneGraph, NodeId) {
    let mut sg = SceneGraph::new();
    let root = sg.root();
    let body = sg.add_child(root, NodePayload::SdfBody {
        name: "Body".into(), primitives: prims, mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: mflags, mirror_sign: 0, mirror_center: mc,
        bevel_radius: 0.0, blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
    (sg, body)
}

// ── GPU harness ─────────────────────────────────────────────────────────────────────────────────

struct BakeGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

/// Build the bake compute pipeline — identical BGL + shader to the production bake
/// (`gpu_init::ensure_primary_bake_pipeline`), same as both tests_bake tests.
fn bake_gpu() -> Option<BakeGpu> {
    let (_gpu_lock, device, queue) = headless_device()?;
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sparse_bake_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sparse volume_bake"),
        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sparse_bake"), layout: Some(&pl), module: &shader, entry_point: Some("bake_volume"),
        compilation_options: Default::default(), cache: None,
    });
    Some(BakeGpu { device, queue, bgl, pipeline })
}

/// Output routing for one bake dispatch of the REAL shader.
enum BakeOut<'a> {
    /// output_mode=0: binding 2 = res³ flat grid (zero-init), binding 5 = 4-byte dummy (exactly
    /// what production binds in dense mode), binding 3 = real res³ texture.
    Dense,
    /// output_mode=2: binding 2 = the POOL arena (`allocated·512` f32 — GARBAGE-prefilled when
    /// `seed` is None, or pre-filled with `seed` for the S2.5 frozen-index PARTIAL round: the
    /// windowed bake must update only its window and keep every other pool float bit-intact),
    /// binding 5 = the classifier index verbatim, binding 3 = 1³ dummy (mode 2 skips the store —
    /// the production `bake_tex_dummy` precedent).
    Sparse { index: &'a [u32], allocated: usize, dims: [u32; 3], seed: Option<&'a [f32]> },
    /// S2.4 integration: output_mode=2 bound EXACTLY as `dispatch_pending_primary_bakes` binds a
    /// production sparse bake — binding 2 = the PERSISTENT `SdfBrickAtlasGpu.pool_buf`, binding 5
    /// = its `index_buf` (already `upload_index`ed by the caller). NO readback of binding 2 (the
    /// pool stays GPU-resident for the mesh dispatch) — returns an empty Vec.
    SparseAtlas { atlas: &'a graphics::SdfBrickAtlasGpu, dims: [u32; 3] },
}

/// Serialize `body`, dispatch ONE bake (`pad=0`; full-grid `bake=[0,res)³` when `window` is
/// None — the same `(res+3)/4` workgroup math as `tests_bake::body_mirror_bake_matches_cpu_eval`
/// — or the S2.5 windowed sub-region `bake=[win_min,win_max)` when Some, matching the production
/// partial dispatch: `voxel = gid + bake_min`, exclusive `bake_max`), read back binding 2 (flat
/// grid in Dense, pool arena in Sparse).
fn dispatch_bake(
    gpu: &BakeGpu, sg: &SceneGraph, body: NodeId,
    res: u32, gmin: Vec3, gmax: Vec3, out: BakeOut,
    window: Option<([u32; 3], [u32; 3])>,
) -> Vec<f32> {
    let mut prim_floats = graphics::serialize_body_for_bake_hot(sg, body, Vec3::ZERO);
    let clip_floats = graphics::serialize_clip_curves_for_bake(sg, body, &mut prim_floats);
    let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;

    let (out_len, output_mode, dims_bricks) = match &out {
        BakeOut::Dense => ((res as usize).pow(3), 0u32, [0u32; 3]),
        BakeOut::Sparse { allocated, dims, .. } => (allocated * 512, 2u32, *dims),
        BakeOut::SparseAtlas { dims, .. } => (0, 2u32, *dims), // pool is external; no readback
    };
    assert!(
        out_len > 0 || matches!(out, BakeOut::SparseAtlas { .. }),
        "empty output buffer — classifier allocated nothing"
    );

    // binding 2 output: test-owned buffer (Dense/Sparse) or the persistent atlas pool (SparseAtlas).
    let out_buf: Option<wgpu::Buffer> = match &out {
        BakeOut::SparseAtlas { .. } => None,
        _ => Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sparse_out"), size: (out_len * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })),
    };
    if let BakeOut::Sparse { seed, .. } = &out {
        match seed {
            // S2.5 partial round: seed the pool with the previous round's content — the windowed
            // bake must leave every out-of-window float bit-intact.
            Some(s) => {
                assert_eq!(s.len(), out_len, "pool seed length must equal allocated*512");
                gpu.queue.write_buffer(out_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(*s));
            }
            // Garbage-prefill: the shader must overwrite every voxel of every handled brick.
            None => gpu.queue.write_buffer(
                out_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(&vec![POOL_GARBAGE; out_len])),
        }
    }
    let prim = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse_prim"), size: (prim_floats.len() as u64 * 4).max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&prim, 0, bytemuck::cast_slice(&prim_floats));
    let clip = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse_clip"), size: (clip_floats.len() as u64 * 4).max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    if !clip_floats.is_empty() { gpu.queue.write_buffer(&clip, 0, bytemuck::cast_slice(&clip_floats)); }

    // binding 6: real classifier index in Sparse, the persistent atlas index in SparseAtlas,
    // 4-byte dummy in Dense (production wiring). binding 5 stays the LAB mesh_grid slot — a
    // zeroed dummy here (res=0 header → operand no-op), exactly production's disabled state.
    let brick_idx: Option<wgpu::Buffer> = match &out {
        BakeOut::Dense => Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sparse_brick_idx_dummy"), size: 4,
            usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false,
        })),
        BakeOut::Sparse { index, .. } => {
            let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sparse_brick_idx"), size: (index.len() * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            gpu.queue.write_buffer(&b, 0, bytemuck::cast_slice(index));
            Some(b)
        }
        BakeOut::SparseAtlas { .. } => None, // bound from the atlas below
    };
    // LAB mesh_grid dummy (binding 5): zeroed header → res=0 → mesh operand disabled, the
    // production state for scenes without MeshVolume prims (wgpu zero-inits new buffers).
    let mesh_dummy = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse_mesh_grid_dummy"), size: 32,
        usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false,
    });
    // binding 3: mode 2 never stores to the texture — 1³ dummy (bake_tex_dummy precedent);
    // dense mode stores unconditionally — full res³ target like tests_bake.
    let tex_res = if matches!(out, BakeOut::Sparse { .. } | BakeOut::SparseAtlas { .. }) { 1 } else { res };
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sparse_tex"),
        size: wgpu::Extent3d { width: tex_res, height: tex_res, depth_or_array_layers: tex_res },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse_params"), size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &gpu.bgl, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: prim.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 2, resource: match &out {
            BakeOut::SparseAtlas { atlas, .. } => atlas.pool_buf.as_entire_binding(),
            _ => out_buf.as_ref().unwrap().as_entire_binding(),
        } },
        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
        wgpu::BindGroupEntry { binding: 4, resource: clip.as_entire_binding() },
        // binding 5: LAB mesh_grid — zeroed 32B dummy (header res=0 → operand disabled).
        wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 6, resource: match &out {
            BakeOut::SparseAtlas { atlas, .. } => atlas.index_buf.as_entire_binding(),
            _ => brick_idx.as_ref().unwrap().as_entire_binding(),
        } },
        wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
        wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
    ] });
    let (bake_min, bake_max) = window.unwrap_or(([0; 3], [res; 3]));
    let p = crate::gpu_jobs::BakeParams {
        grid_res: [res; 3], _pad0: 0,
        bounds_min: gmin.to_array(), _pad1: 0.0, bounds_max: gmax.to_array(),
        prim_count, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32, dims_bricks,
        bake_min, _pad3: 0, bake_max, _pad4: 0,
        // mirror_* uniform params are NOT consumed by the body union (it lives in the serialized
        // SCOPE marker) — zeros, same as tests_bake::body_mirror_bake_matches_cpu_eval.
        mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
        atlas_offset: [0; 3], atlas_pad: 0, output_mode, _pad5: 0,
        // LAB mesh operand disabled in tests — identity (the body union carries no mesh operand).
        body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
    };
    gpu.queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));
    // Per-axis workgroups over the bake window (the production window math; full grid when None).
    let wg = [
        (bake_max[0].saturating_sub(bake_min[0]) + 3) / 4,
        (bake_max[1].saturating_sub(bake_min[1]) + 3) / 4,
        (bake_max[2].saturating_sub(bake_min[2]) + 3) / 4,
    ];
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
        cp.set_pipeline(&gpu.pipeline); cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups(wg[0], wg[1], wg[2]);
    }
    gpu.queue.submit(std::iter::once(enc.finish()));

    // SparseAtlas: the pool stays GPU-resident for the mesh dispatch — nothing to read back.
    if matches!(out, BakeOut::SparseAtlas { .. }) {
        return Vec::new();
    }
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sparse_stg"), size: (out_len * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(out_buf.as_ref().unwrap(), 0, &staging, 0, (out_len * 4) as u64);
    gpu.queue.submit(std::iter::once(enc.finish()));
    // GPU Safety rule: capture the map_async Result (never discard with |_| {}).
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map_async callback dropped").expect("pool/grid readback map failed");
    let g: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    g
}

// ── classify → bake → decode → compare (one scene) ──────────────────────────────────────────────

/// Full S2.3 pipeline for one scene. Panics (with `name`-tagged messages) on any parity failure;
/// returns the classifier index so callers can pin cross-round properties (adversarial test).
///
/// `expect_exterior` / `expect_interior`: per-scene sentinel-coverage guards (a vacuously-passing
/// sentinel check proves nothing). Not universal because the conservative prims classifier
/// legitimately stores EVERY brick for some scenes at this res (e.g. box-minus-sphere, whose
/// margin-inflated box field flags the whole ±1.5 domain — S2.1 measured 512→1728 for it); such
/// scenes still exercise the handled-brick routing + boolean eval, while other scenes in the
/// suite cover the two sentinel skips (EXTERIOR 0xFFFFFFFF, INTERIOR 0xFFFFFFFE).
fn check_scene(
    gpu: &BakeGpu, name: &str,
    prims: &[SdfPrimitive], mflags: u8, mc: Vec3,
    res: u32, gmin: Vec3, gmax: Vec3, band: f32,
    expect_exterior: bool,
    expect_interior: bool,
) -> SparseIndexOnly {
    // 1. CPU classify at the same bounds/res the bake uses (mirror-expanded per the Body Mirror
    //    AABB rule — the ±1.5 domain already covers both halves of the mirrored scene).
    let sp = classify_prims_to_sparse_index(prims, mflags, mc, gmin, gmax, res, 8, band);
    let total_bricks = (sp.dims_bricks[0] * sp.dims_bricks[1] * sp.dims_bricks[2]) as usize;
    assert_eq!(sp.index.len(), total_bricks, "{name}: index length != dims product");
    assert!(sp.allocated > 0, "{name}: classifier allocated nothing — vacuous scene");

    // 2. CPU dense oracle — the bake spec (identical eval + endpoint-inclusive mapping).
    let cpu = crate::bake_utils::bake_body_grid(prims, res, gmin, gmax, mflags, 0, mc);

    // 3. GPU: dense mode-0 (negative control + bit-exact reference) and sparse mode-2.
    let (sg, body) = make_body(prims.to_vec(), mflags, mc);
    let dense = dispatch_bake(gpu, &sg, body, res, gmin, gmax, BakeOut::Dense, None);
    let pool = dispatch_bake(gpu, &sg, body, res, gmin, gmax, BakeOut::Sparse {
        index: &sp.index, allocated: sp.allocated, dims: sp.dims_bricks, seed: None,
    }, None);

    // 4. Full-overwrite proof: res % 8 == 0 → every handled brick is fully in-body → zero
    //    garbage survivors allowed anywhere in the pool.
    let garbage_left = pool.iter().filter(|v| v.to_bits() == POOL_GARBAGE.to_bits()).count();

    // 5. Decode through the production CPU oracle accessor. S2.1 handles are dense-packed
    //    0..allocated in scan order — identity slot_map (the SparseSdfBricks contract).
    let slot_map: HashMap<u32, usize> = (0..sp.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc = BrickGridAccessor::new(&sp.index, &pool, &slot_map, sp.dims_bricks, 8, res - 1);

    // 6. Voxel-exhaustive comparison.
    let ext = gmax - gmin;
    let voxel = (ext / (res - 1) as f32).min_element();
    let (dx, dy) = (sp.dims_bricks[0] as usize, sp.dims_bricks[1] as usize);
    let (mut handled_vox, mut ext_vox, mut int_vox) = (0usize, 0usize, 0usize);
    let mut bitexact_mismatch = 0usize;   // sparse decode vs mode-0 dense (handled bricks)
    let mut sentinel_decode_bad = 0usize; // accessor didn't return ±far for a sentinel brick
    let (mut sign_flips, mut nan_count, mut near_dv, mut overall_dv) = (0usize, 0usize, 0.0f32, 0.0f32);
    let (mut ctrl_sign_flips, mut ctrl_nan, mut ctrl_near_dv) = (0usize, 0usize, 0.0f32);
    let mut sign_violations = 0usize;     // cpu sign inconsistent with an EXTERIOR/INTERIOR sentinel
    let (mut ext_min_cpu, mut int_max_cpu) = (f32::INFINITY, f32::NEG_INFINITY);
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let cpu_v = cpu[x + y * res as usize + z * (res * res) as usize];
        let dense_v = dense[x + y * res as usize + z * (res * res) as usize];
        let s = acc.sample(x, y, z);

        // Negative control: mode-0 dense output (dispatched WITH the dummy binding 5, as
        // production does) vs the CPU oracle — the reused tests_bake criteria.
        if !dense_v.is_finite() { ctrl_nan += 1; }
        else {
            if (dense_v < 0.0) != (cpu_v < 0.0) && cpu_v.abs() > voxel && dense_v.abs() > voxel { ctrl_sign_flips += 1; }
            if cpu_v.abs() < 3.0 * voxel { ctrl_near_dv = ctrl_near_dv.max((dense_v - cpu_v).abs()); }
        }

        let tag = sp.index[(x / 8) + (y / 8) * dx + (z / 8) * dx * dy];
        if tag == BRICK_TAG_EXTERIOR {
            ext_vox += 1;
            if s != BRICK_FAR { sentinel_decode_bad += 1; }
            ext_min_cpu = ext_min_cpu.min(cpu_v);
            // Conservative classifier guarantee: no surface in an EXTERIOR brick → cpu stays
            // on the outside sign (within the reused near-surface tolerance).
            if cpu_v < -NEAR_TOL { sign_violations += 1; }
        } else if tag == BRICK_TAG_INTERIOR {
            int_vox += 1;
            if s != -BRICK_FAR { sentinel_decode_bad += 1; }
            int_max_cpu = int_max_cpu.max(cpu_v);
            if cpu_v > NEAR_TOL { sign_violations += 1; }
        } else {
            handled_vox += 1;
            if !s.is_finite() { nan_count += 1; continue; }
            // (b) Routing proof: the pool voxel must be BIT-IDENTICAL to the same shader's
            // dense mode-0 output — both writes store the same `acc`.
            if s.to_bits() != dense_v.to_bits() { bitexact_mismatch += 1; }
            // (a) GPU-vs-CPU parity, reused tests_bake criteria (1-voxel sign skin,
            // near-surface value tolerance).
            if (s < 0.0) != (cpu_v < 0.0) && cpu_v.abs() > voxel && s.abs() > voxel { sign_flips += 1; }
            let dv = (s - cpu_v).abs();
            overall_dv = overall_dv.max(dv);
            if cpu_v.abs() < 3.0 * voxel { near_dv = near_dv.max(dv); }
        }
    }}}

    let n_ext = sp.index.iter().filter(|&&t| t == BRICK_TAG_EXTERIOR).count();
    let n_int = sp.index.iter().filter(|&&t| t == BRICK_TAG_INTERIOR).count();
    eprintln!(
        "[SparseBake] {name}: res={res} bricks={total_bricks} allocated={} ext={n_ext} int={n_int} | \
         garbage_left={garbage_left} bitexact_mismatch={bitexact_mismatch} sentinel_decode_bad={sentinel_decode_bad} | \
         handled: sign_flips={sign_flips} nan={nan_count} near|dv|={near_dv:.4} overall|dv|={overall_dv:.4} ({handled_vox} vox) | \
         sentinels: sign_violations={sign_violations} ext_min_cpu={ext_min_cpu:.3} ({ext_vox} vox) int_max_cpu={int_max_cpu:.3} ({int_vox} vox) | \
         control: sign_flips={ctrl_sign_flips} nan={ctrl_nan} near|dv|={ctrl_near_dv:.4}",
        sp.allocated,
    );

    // Negative control (mode 0 with the dummy binding 5 still matches the oracle).
    assert_eq!(ctrl_nan, 0, "{name}: NEGATIVE CONTROL — dense mode-0 produced non-finite voxels");
    assert_eq!(ctrl_sign_flips, 0, "{name}: NEGATIVE CONTROL — dense mode-0 no longer matches the CPU oracle (mode 2's presence perturbed mode 0)");
    assert!(ctrl_near_dv < NEAR_TOL, "{name}: NEGATIVE CONTROL — dense mode-0 near-surface divergence {ctrl_near_dv:.4}");
    // Sparse pool integrity + routing.
    assert_eq!(garbage_left, 0, "{name}: {garbage_left} pool floats never written — the mode-2 write missed voxels of handled bricks");
    assert_eq!(bitexact_mismatch, 0, "{name}: sparse decode differs BIT-WISE from the same shader's dense output in handled bricks — brick_linear/local-offset routing bug");
    assert_eq!(sentinel_decode_bad, 0, "{name}: accessor returned non-±far inside a sentinel brick");
    // Sparse decode vs CPU oracle (reused criteria).
    assert_eq!(nan_count, 0, "{name}: sparse bake produced non-finite voxels");
    assert_eq!(sign_flips, 0, "{name}: sparse decode disagrees with the CPU oracle on inside/outside");
    assert!(near_dv < NEAR_TOL, "{name}: sparse decode near-surface divergence {near_dv:.4} (tolerance {NEAR_TOL})");
    // Sentinel-sign consistency (the conservative-classifier guarantee, asserted on hardware).
    assert_eq!(sign_violations, 0, "{name}: CPU oracle sign contradicts an EXTERIOR/INTERIOR sentinel — classifier hole would punch through the sparse bake");
    if expect_exterior {
        assert!(n_ext > 0, "{name}: expected EXTERIOR bricks but classifier tagged none — the 0xFFFFFFFF sentinel skip is unexercised (vacuous coverage)");
    }
    if expect_interior {
        assert!(n_int > 0, "{name}: expected INTERIOR bricks but classifier tagged none — the 0xFFFFFFFE sentinel skip is unexercised (vacuous coverage)");
    }
    sp
}

// ── tests ───────────────────────────────────────────────────────────────────────────────────────

/// The 4 canonical S2.1 scenes (sphere / smooth-blend k>0 / box-minus-sphere / MIRRORED body)
/// plus a deep-interior sphere (radius large enough that bricks classify INTERIOR past the
/// non-Lipschitz margin — exercising the 0xFFFFFFFE side of the shader's sentinel compare),
/// each run through the full classify → mode-2 bake → decode → oracle pipeline, with the mode-0
/// dense dispatch as per-scene negative control and bit-exact routing reference.
#[test]
fn sparse_bake_gpu_pool_matches_cpu_oracle() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };

    // Same domain/res/band conventions as the S2.1 superset gates: 96 vertices (12 bricks/axis —
    // res % 8 == 0, multi-brick but fast), ±1.5 bounds, tight 1.5-cell band.
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();

    // (1) Plain sphere.
    let sphere = vec![sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 })];
    check_scene(&gpu, "sphere", &sphere, 0, Vec3::ZERO, res, gmin, gmax, band, true, false);

    // (2) Smooth-blend (k > 0) union of two spheres.
    let blend = vec![
        sphere_op(Vec3::new(-0.45, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.45, 0.0, 0.0), 0.5, BooleanOp::Union { k: 0.3 }),
    ];
    check_scene(&gpu, "smooth-blend", &blend, 0, Vec3::ZERO, res, gmin, gmax, band, true, false);

    // (3) Box minus sphere (smooth subtract carves a fillet). At this res the conservative
    //     classifier stores ALL bricks (S2.1: 512→1728) — no sentinel coverage expected; this
    //     scene's value is the boolean-chain eval through the mode-2 handled-brick routing.
    let subtract = vec![
        box_op(Vec3::ZERO, Vec3::splat(0.7), BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ];
    check_scene(&gpu, "box-minus-sphere", &subtract, 0, Vec3::ZERO, res, gmin, gmax, band, false, false);

    // (4) MIRRORED body (mirror_flags != 0): off-center box on -X, mirrored across x=0. The ±1.5
    //     domain is the mirror-EXPANDED bounds (Body Mirror AABB rule) — it covers both halves.
    let mirrored = vec![box_op(Vec3::new(-0.8, 0.0, 0.0), Vec3::splat(0.35), BooleanOp::Union { k: 0.0 })];
    let sp = check_scene(&gpu, "mirrored", &mirrored, 0b001, Vec3::ZERO, res, gmin, gmax, band, true, false);
    // The mirror must actually be exercised: the reflected +X half must allocate bricks.
    let dims = sp.dims_bricks;
    let has_pos_x = sp.index.iter().enumerate().any(|(bi, &h)| {
        h < BRICK_TAG_INTERIOR && (bi as u32 % dims[0]) >= dims[0] / 2 + 1
    });
    assert!(has_pos_x, "mirrored scene must allocate bricks on the reflected +X half");

    // (5) Deep-interior BOX: an INTERIOR brick needs all 9 samples deeper than the 2×diag
    //     non-Lipschitz margin (~0.875 world at this res) AND no surface-possible brick in its
    //     1-ring (dilation stores the ring). No sphere fits that inside ±1.5 — the Euclidean
    //     band + ring reaches the center. The box's max-component interior metric is flatter:
    //     with half_extents 1.45, the surface-possible shell stops at |comp| ≥ 0.528 (brick
    //     ring 3/8 per axis), the adjacent ring 4/7 (worst corner d = −0.96 < −0.9223) is
    //     dilation-stored but NOT surface, and the central 2³ bricks (worst corner d = −1.21)
    //     classify INTERIOR → the 0xFFFFFFFE side of the shader's `pool_handle < 0xFFFFFFFEu`
    //     compare is actually exercised. The 4 canonical scenes produce zero INTERIOR bricks
    //     (their deepest |d| < margin), so this scene carries that coverage alone.
    let deep = vec![box_op(Vec3::ZERO, Vec3::splat(1.45), BooleanOp::Union { k: 0.0 })];
    check_scene(&gpu, "deep-interior-box", &deep, 0, Vec3::ZERO, res, gmin, gmax, band, false, true);
}

/// ADVERSARIAL re-classify: a small sphere moves so bricks that WERE handled become EXTERIOR in a
/// SECOND, INDEPENDENT classify + bake round (fresh index, fresh garbage-filled pool). Round 2's
/// decode must match round 2's oracle — pinning the assumption S2.5 relies on: the index is
/// FROZEN per bake round, and correctness after a scene edit comes from re-classify + re-bake,
/// never from stale pool contents.
#[test]
fn sparse_bake_reclassify_round2_frozen_index() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();

    // Round 1: small sphere at -X. Round 2: same sphere moved to +X — 1.2 world units of
    // clearance to the old location, far beyond band + 2×diag margin (~0.93), so round-1 surface
    // bricks MUST reclassify EXTERIOR.
    let r1 = vec![sphere_op(Vec3::new(-0.75, 0.0, 0.0), 0.3, BooleanOp::Union { k: 0.0 })];
    let r2 = vec![sphere_op(Vec3::new(0.75, 0.0, 0.0), 0.3, BooleanOp::Union { k: 0.0 })];

    let sp1 = check_scene(&gpu, "round1-sphere-at-negx", &r1, 0, Vec3::ZERO, res, gmin, gmax, band, true, false);
    let sp2 = check_scene(&gpu, "round2-sphere-at-posx", &r2, 0, Vec3::ZERO, res, gmin, gmax, band, true, false);

    // The adversarial pin: at least one brick handled in round 1 is EXTERIOR in round 2 (a brick
    // that WAS surface no longer is). If this never happens the test is vacuous.
    assert_eq!(sp1.dims_bricks, sp2.dims_bricks);
    let flipped = sp1.index.iter().zip(sp2.index.iter())
        .filter(|(&h1, &h2)| h1 < BRICK_TAG_INTERIOR && h2 == BRICK_TAG_EXTERIOR)
        .count();
    eprintln!("[SparseBake] adversarial: round1 allocated={} round2 allocated={} handled→EXTERIOR flips={flipped}",
        sp1.allocated, sp2.allocated);
    assert!(flipped > 0, "no brick went handled→EXTERIOR between rounds — adversarial move is vacuous");
}

/// S2.4 integration — the PRODUCTION wiring shape end-to-end on hardware:
/// `classify_prims_to_sparse_index` → persistent `SdfBrickAtlasGpu::{ensure_capacity,
/// upload_index}` (INDEX only — the pool is bake-filled) → REAL mode-2 bake bound EXACTLY as
/// `dispatch_pending_primary_bakes` binds it (binding 2 = atlas.pool_buf, binding 5 =
/// atlas.index_buf, dummy texture, no staging) → `SdfMeshGpu::mesh_into_slot_sparse` from the
/// SAME atlas into a real `SdfBodyMeshSlot` → the slot's `DrawIndexedIndirect.index_count` is a
/// nonzero multiple of 3 (a real surface was meshed from the bake-written pool). What this does
/// NOT cover (owner's manual checklist): the request-side eligibility gates in trigger_bake, the
/// poll/record plumbing, and the on-screen draw.
#[test]
fn sparse_bake_atlas_bake_then_mesh_indirect_nonzero() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let prims = vec![sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 })];
    let sp = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(sp.allocated > 0, "classifier allocated nothing — vacuous scene");

    let (sg, body) = make_body(prims, 0, Vec3::ZERO);
    // Persistent atlas — the production get-or-create + grow + INDEX-only upload.
    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, sp.index.len() as u32, sp.allocated as u32);
    atlas.ensure_capacity(&gpu.device, &gpu.queue, sp.index.len() as u32, sp.allocated as u32);
    atlas.upload_index(&gpu.queue, &sp.index, sp.allocated as u32, sp.dims_bricks, res, body);
    assert_eq!(atlas.res, res);
    assert_eq!(atlas.allocated_bricks, sp.allocated as u32);
    assert_eq!(atlas.body_nid, Some(body));

    // Mode-2 bake writes the atlas POOL in place (no readback — the production shape).
    let none = dispatch_bake(&gpu, &sg, body, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: sp.dims_bricks }, None);
    assert!(none.is_empty(), "SparseAtlas dispatch must not read back the pool");

    // Sparse DC mesh from the SAME atlas into a real body slot (the production dispatch shape
    // in sdf_populate_resident's brick-resident branch).
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    mesher.mesh_into_slot_sparse(&gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, body, None);

    // Read back the DrawIndexedIndirect args. GPU Safety rule: capture the map_async Result.
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("indirect_stg"), size: 20,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.indirect, 0, &staging, 0, 20);
    gpu.queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map_async callback dropped").expect("indirect readback map failed");
    let args: Vec<u32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    eprintln!("[SparseBake] integration: indirect args={:?} (bricks={}/{})",
        args, sp.allocated, sp.index.len());
    assert!(args[0] > 0,
        "sparse bake→mesh produced ZERO indices — the atlas pool/index wiring is broken");
    assert_eq!(args[0] % 3, 0, "index_count must be whole triangles");
    assert_eq!(args[1], 1, "instance_count seeded to 1 by SdfBodyMeshSlot::new");
}

/// World-space dirty window → voxel window, the trigger_bake mapping VERBATIM (floor for the
/// inclusive min, ceil + 1 for the exclusive max, clamped to `[0, res)`/`res`).
fn world_window_to_voxel(
    wmin: Vec3, wmax: Vec3, gmin: Vec3, gmax: Vec3, res: u32,
) -> ([u32; 3], [u32; 3]) {
    let extent = gmax - gmin;
    let denom = (res - 1) as f32;
    let t_min = (wmin - gmin) / extent;
    let t_max = (wmax - gmin) / extent;
    (
        [
            (t_min.x * denom).floor().max(0.0) as u32,
            (t_min.y * denom).floor().max(0.0) as u32,
            (t_min.z * denom).floor().max(0.0) as u32,
        ],
        [
            ((t_max.x * denom).ceil() as u32 + 1).min(res),
            ((t_max.y * denom).ceil() as u32 + 1).min(res),
            ((t_max.z * denom).ceil() as u32 + 1).min(res),
        ],
    )
}

/// S2.5 partial-parity — the frozen-index PARTIAL contract end-to-end on hardware:
/// full sparse bake of round 1 → move the prim WITHIN its brick set (a small nudge, the
/// production drag case) → compute the dirty window as production does (old ∪ new prim AABB +
/// 2-voxel margin → voxel window → one-brick dilation) → assert the frozen-index check PASSES
/// (no escalation) → dispatch the PARTIAL mode-2 bake over the dilated window against the
/// FROZEN round-1 index with the pool SEEDED from round 1 → decode the whole volume:
///   - voxels INSIDE the window match round-2's CPU dense oracle (moved scene) with the reused
///     tests_bake criteria (1-voxel sign skin, near-surface |dv| < NEAR_TOL);
///   - voxels OUTSIDE the window are BIT-IDENTICAL to round 1's pool (the windowed write
///     touched nothing else) and still match round-1's oracle where unchanged.
#[test]
fn sparse_partial_frozen_index_window_matches_oracle() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Round 1: sphere at p0. Round 2: nudged +0.08 on X (~2.5 voxels) — well inside the frozen
    // handle set (classifier margin ≈ 2× brick diagonal ≈ 0.875 world at this res).
    let (p0, r) = (Vec3::new(-0.30, 0.0, 0.0), 0.3f32);
    let p1 = Vec3::new(-0.22, 0.0, 0.0);
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 })];

    // Frozen index + round-1 full sparse bake (garbage-prefilled pool, S2.3 shape).
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(sp1.allocated > 0, "vacuous scene");
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    let pool1 = dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: None,
    }, None);

    // CPU dense oracles for both rounds (the bake spec).
    let oracle1 = crate::bake_utils::bake_body_grid(&round1, res, gmin, gmax, 0, 0, Vec3::ZERO);
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin, gmax, 0, 0, Vec3::ZERO);

    // Dirty window, production semantics: old ∪ new prim AABB + 2-voxel margin
    // (mark_prim_dirty_aabb), mapped to voxels (trigger_bake), dilated one brick (S2.5).
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)) - margin;
    let wmax = (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    assert!(
        sparse_window_bricks_all_handled(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max),
        "nudge escaped the frozen brick set — test premise broken (production would escalate)"
    );
    // S2.5b — the PRODUCTION eligibility rule (tag compatibility): a fully-handled window is
    // trivially compatible, so the partial stays reachable under the new check too.
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(
        sparse_window_tags_compatible(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max, &clf2).is_ok(),
        "S2.5b tag-compatibility must allow this fully-handled-window partial"
    );

    // PARTIAL: mode-2 over the dilated window, FROZEN index, pool seeded from round 1 — exactly
    // the production dispatch shape (no upload_index, windowed bake_min/bake_max).
    let (sg2, body2) = make_body(round2, 0, Vec3::ZERO);
    let pool2 = dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: Some(&pool1),
    }, Some((dil_min, dil_max)));

    // Decode both rounds through the production CPU accessor (identity slot_map).
    let slot_map: HashMap<u32, usize> = (0..sp1.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc1 = BrickGridAccessor::new(&sp1.index, &pool1, &slot_map, sp1.dims_bricks, 8, res - 1);
    let acc2 = BrickGridAccessor::new(&sp1.index, &pool2, &slot_map, sp1.dims_bricks, 8, res - 1);

    let (dx, dy) = (sp1.dims_bricks[0] as usize, sp1.dims_bricks[1] as usize);
    let (mut in_vox, mut out_vox, mut changed_in_window) = (0usize, 0usize, 0usize);
    let mut out_bit_mismatch = 0usize;
    let (mut in_sign_flips, mut in_near_dv) = (0usize, 0.0f32);
    let mut out_sign_flips = 0usize;
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let s1 = acc1.sample(x, y, z);
        let s2 = acc2.sample(x, y, z);
        let tag = sp1.index[(x / 8) + (y / 8) * dx + (z / 8) * dx * dy];
        let in_window = (x as u32) >= dil_min[0] && (x as u32) < dil_max[0]
            && (y as u32) >= dil_min[1] && (y as u32) < dil_max[1]
            && (z as u32) >= dil_min[2] && (z as u32) < dil_max[2];
        if in_window {
            in_vox += 1;
            // Window vs round-2 oracle (the MOVED scene) — reused tests_bake criteria.
            let cpu = oracle2[x + y * res as usize + z * (res * res) as usize];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { in_sign_flips += 1; }
            if cpu.abs() < 3.0 * voxel { in_near_dv = in_near_dv.max((s2 - cpu).abs()); }
            if s2.to_bits() != s1.to_bits() { changed_in_window += 1; }
        } else {
            out_vox += 1;
            // Outside the window: round-1 values kept BIT-EXACT (sentinel bricks decode ±far in
            // both rounds; handled bricks were not written outside [bake_min,bake_max)) …
            if s2.to_bits() != s1.to_bits() { out_bit_mismatch += 1; }
            // … and still consistent with round-1's oracle (the unchanged field).
            let _ = tag;
            let cpu = oracle1[x + y * res as usize + z * (res * res) as usize];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { out_sign_flips += 1; }
        }
    }}}
    eprintln!(
        "[SparseBake] partial-parity: res={res} allocated={} window=[{:?}..{:?}] dilated=[{:?}..{:?}] | \
         in: {in_vox} vox sign_flips={in_sign_flips} near|dv|={in_near_dv:.4} changed={changed_in_window} | \
         out: {out_vox} vox bit_mismatch={out_bit_mismatch} sign_flips_vs_oracle1={out_sign_flips}",
        sp1.allocated, win_min, win_max, dil_min, dil_max,
    );
    assert!(changed_in_window > 0,
        "no pool float changed inside the window — the partial dispatch is vacuous");
    assert_eq!(out_bit_mismatch, 0,
        "voxels OUTSIDE the window changed — the windowed mode-2 write leaked past bake_min/max");
    assert_eq!(in_sign_flips, 0,
        "window decode disagrees with round-2's oracle on inside/outside");
    assert!(in_near_dv < NEAR_TOL,
        "window near-surface divergence {in_near_dv:.4} vs round-2 oracle (tolerance {NEAR_TOL})");
    assert_eq!(out_sign_flips, 0,
        "out-of-window decode no longer matches round-1's oracle — stale-region corruption");
}

/// S2.5b REPRO on hardware — the owner-measured flaw: a dragged SOLID whose dirty window covers
/// its own by-design INTERIOR sentinels. The old sentinel-absence rule escalated EVERY drag
/// frame ([TriggerBake] 85-104ms full classify + full 512³ bake); the new tag-compatibility
/// rule keeps the frame PARTIAL. End-to-end: full sparse bake of the deep box → small nudge →
/// production window (old ∪ new AABB + 2-voxel margin → voxel → 1-brick dilation) → assert the
/// window CONTAINS INTERIOR sentinels, the OLD check escalates (the pinned flaw), the NEW check
/// says partial-OK → dispatch the windowed mode-2 partial against the FROZEN index with the
/// pool seeded from round 1 → decode: in-window matches the MOVED scene's oracle (reused
/// tests_bake criteria — INTERIOR sentinels keep decoding `-far`, sign-correct vs the deep
/// interior), out-of-window BIT-IDENTICAL to round 1 (S2.5's partial-parity machinery).
#[test]
fn sparse_partial_interior_sentinels_stay_partial() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    // 128³ over ±2: big enough that a he=1.45 box keeps a 2³ INTERIOR core past the 2×diag
    // margin + dilation ring, while its dirty window stays SMALLER than the grid (a real
    // out-of-window set). Verified premise below, not assumed.
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    let he = Vec3::splat(1.45);
    let nudge = Vec3::new(0.04, 0.0, 0.0); // ≈ 1.3 voxels — a drag-frame step
    let round1 = vec![box_op(Vec3::ZERO, he, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![box_op(nudge, he, BooleanOp::Union { k: 0.0 })];

    // Frozen index + round-1 full sparse bake (garbage-prefilled pool, S2.3 shape).
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let total = (sp1.dims_bricks[0] * sp1.dims_bricks[1] * sp1.dims_bricks[2]) as usize;
    let n_int = sp1.index.iter().filter(|&&t| t == BRICK_TAG_INTERIOR).count();
    assert!(sp1.allocated < total && n_int > 0,
        "premise: INTERIOR sentinels must exist inside the solid (allocated={} total={total} interior={n_int})",
        sp1.allocated);
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    let pool1 = dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: None,
    }, None);

    // CPU dense oracles for both rounds (the bake spec).
    let oracle1 = crate::bake_utils::bake_body_grid(&round1, res, gmin, gmax, 0, 0, Vec3::ZERO);
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin, gmax, 0, 0, Vec3::ZERO);

    // Production window semantics: old ∪ new prim AABB + 2-voxel margin → voxels → dilation.
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (-he).min(-he + nudge) - margin;
    let wmax = he.max(he + nudge) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    assert!(dil_min.iter().any(|&v| v > 0) || dil_max.iter().any(|&v| v < res),
        "premise: window must not be the full grid (out-of-window set must exist)");
    // The window must actually cover INTERIOR sentinels — the exact geometry that broke S2.5.
    let (dx, dy) = (sp1.dims_bricks[0] as usize, sp1.dims_bricks[1] as usize);
    let mut int_in_window = 0usize;
    for bz in dil_min[2] / 8..=(dil_max[2] - 1) / 8 {
        for by in dil_min[1] / 8..=(dil_max[1] - 1) / 8 {
            for bx in dil_min[0] / 8..=(dil_max[0] - 1) / 8 {
                if sp1.index[bx as usize + by as usize * dx + bz as usize * dx * dy] == BRICK_TAG_INTERIOR {
                    int_in_window += 1;
                }
            }
        }
    }
    assert!(int_in_window > 0, "premise: dilated window must cover INTERIOR sentinels");

    // The PINNED FLAW: sentinel absence escalates on the solid's own INTERIOR bricks …
    assert!(
        !sparse_window_bricks_all_handled(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max),
        "old S2.5 rule unexpectedly passed — the repro scene no longer covers the flaw"
    );
    // … while S2.5b tag compatibility (the production rule) keeps the frame PARTIAL.
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let t0 = std::time::Instant::now();
    let verdict = sparse_window_tags_compatible(
        &sp1.index, sp1.dims_bricks, 8, dil_min, dil_max, &clf2);
    let check_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert!(verdict.is_ok(),
        "S2.5b must allow the partial (INTERIOR sentinels still needed Interior): {verdict:?}");

    // PARTIAL: mode-2 over the dilated window, FROZEN index, pool seeded from round 1 — the
    // production dispatch shape (no upload_index, windowed bake_min/bake_max).
    let (sg2, body2) = make_body(round2, 0, Vec3::ZERO);
    let pool2 = dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: Some(&pool1),
    }, Some((dil_min, dil_max)));

    // Decode both rounds through the production CPU accessor (identity slot_map).
    let slot_map: HashMap<u32, usize> = (0..sp1.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc1 = BrickGridAccessor::new(&sp1.index, &pool1, &slot_map, sp1.dims_bricks, 8, res - 1);
    let acc2 = BrickGridAccessor::new(&sp1.index, &pool2, &slot_map, sp1.dims_bricks, 8, res - 1);

    let (mut in_vox, mut out_vox, mut changed_in_window) = (0usize, 0usize, 0usize);
    let mut out_bit_mismatch = 0usize;
    let (mut in_sign_flips, mut in_near_dv) = (0usize, 0.0f32);
    let mut out_sign_flips = 0usize;
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let s1 = acc1.sample(x, y, z);
        let s2 = acc2.sample(x, y, z);
        let in_window = (x as u32) >= dil_min[0] && (x as u32) < dil_max[0]
            && (y as u32) >= dil_min[1] && (y as u32) < dil_max[1]
            && (z as u32) >= dil_min[2] && (z as u32) < dil_max[2];
        if in_window {
            in_vox += 1;
            // Window vs round-2 oracle (the MOVED scene) — reused tests_bake criteria. The
            // INTERIOR sentinels inside the window decode -far: deeply negative, sign-correct
            // vs the oracle's deep interior (that is WHY the partial is safe).
            let cpu = oracle2[x + y * res as usize + z * (res * res) as usize];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { in_sign_flips += 1; }
            if cpu.abs() < 3.0 * voxel { in_near_dv = in_near_dv.max((s2 - cpu).abs()); }
            if s2.to_bits() != s1.to_bits() { changed_in_window += 1; }
        } else {
            out_vox += 1;
            if s2.to_bits() != s1.to_bits() { out_bit_mismatch += 1; }
            let cpu = oracle1[x + y * res as usize + z * (res * res) as usize];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { out_sign_flips += 1; }
        }
    }}}
    eprintln!(
        "[SparseBake] S2.5b repro: res={res} allocated={}/{total} interior={n_int} \
         (in window: {int_in_window}) window=[{:?}..{:?}] tag-check={check_ms:.2}ms | \
         in: {in_vox} vox sign_flips={in_sign_flips} near|dv|={in_near_dv:.4} changed={changed_in_window} | \
         out: {out_vox} vox bit_mismatch={out_bit_mismatch} sign_flips_vs_oracle1={out_sign_flips}",
        sp1.allocated, dil_min, dil_max,
    );
    assert!(out_vox > 0, "out-of-window set is empty — bit-exactness assertion is vacuous");
    assert!(changed_in_window > 0,
        "no pool float changed inside the window — the partial dispatch is vacuous");
    assert_eq!(out_bit_mismatch, 0,
        "voxels OUTSIDE the window changed — the windowed mode-2 write leaked past bake_min/max");
    assert_eq!(in_sign_flips, 0,
        "window decode disagrees with round-2's oracle on inside/outside");
    assert!(in_near_dv < NEAR_TOL,
        "window near-surface divergence {in_near_dv:.4} vs round-2 oracle (tolerance {NEAR_TOL})");
    assert_eq!(out_sign_flips, 0,
        "out-of-window decode no longer matches round-1's oracle — stale-region corruption");
}

// ── WINDOWED ESCALATED RECLASSIFY gates (2026-07-16) ────────────────────────────────────────────
// The oversized-window escalation for windows in (1/8, 1/2] of the grid's bricks no longer runs
// a FULL grid classify: it runs the S2.5b/c window walk (`sparse_window_allocate_incompatible`)
// on a background worker and lands as an ordinary Partial. These gates prove the WALK + WINDOWED
// BAKE at escalation scale produce a final atlas state equivalent to a from-scratch FULL
// classify + full bake of the edited scene — the exact operation the worker performs, on real
// hardware, judged brick-by-brick:
//   - both-HANDLED bricks (windowed ∧ reference): decoded voxels BIT-IDENTICAL inside the window;
//   - windowed-handled / reference-sentinel bricks (the benign superset — incremental alloc never
//     reclaims what it cannot PROVE vacated): no sign change inside the brick and sign == the
//     sentinel's sign ⇒ no DC crossing on either side ⇒ identical mesh;
//   - both-SENTINEL bricks: tags EQUAL (same ±far decode);
//   - reference-handled / windowed-sentinel: FORBIDDEN (a hole — the classify superset broke);
//   - bricks fully OUTSIDE the window: index tag AND pool content byte-untouched.
// Sign vs the edited scene's dense CPU oracle is asserted over the WHOLE grid (total-state
// correctness, not just the window).

/// Is `t` a sentinel tag (EXTERIOR/INTERIOR) as opposed to a pool handle?
fn is_sentinel(t: u32) -> bool { t == BRICK_TAG_EXTERIOR || t == BRICK_TAG_INTERIOR }

/// Shared body of the two windowed-escalation gates. `round1`/`round2` are the pre/post-edit
/// prim lists; the window is derived from `edit_wmin/wmax` (old ∪ new AABB of the edited prim +
/// 2-voxel margin) with production semantics. Returns (alloc, window bricks, total bricks).
#[allow(clippy::too_many_arguments)]
fn run_windowed_escalation_gate(
    label: &str,
    res: u32, gmin: Vec3, gmax: Vec3,
    round1: Vec<SdfPrimitive>, round2: Vec<SdfPrimitive>,
    edit_wmin: Vec3, edit_wmax: Vec3,
    require_append: bool,
) {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseWindowed] no GPU adapter — skipping"); return; }
    };
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Round 1: frozen index + full sparse bake (garbage-prefilled pool, S2.3 shape).
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(sp1.allocated > 0, "vacuous scene");
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    let pool1 = dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: None,
    }, None);

    // Production window semantics (trigger_bake): world AABB + margin → voxels → 1-brick dilate.
    let margin = Vec3::splat(2.0 * voxel);
    let (win_min, win_max) =
        world_window_to_voxel(edit_wmin - margin, edit_wmax + margin, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    // PREMISE: the window sits in the WINDOWED escalation band (1/8, 1/2] of the grid's bricks.
    let wb: u64 = (0..3).map(|a| ((dil_max[a] - dil_min[a] + 7) / 8) as u64).product();
    let total: u64 = (0..3).map(|_| ((res + 7) / 8) as u64).product();
    assert!(wb * 8 > total && wb * 2 <= total,
        "premise: window must be in (1/8, 1/2] of the bricks (got {wb}/{total})");

    // THE WORKER OPERATION, verbatim (spawn_async_window_reclassify): clone the frozen index,
    // walk the dilated window against the EDITED prims, widen the bake box for appended bricks.
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let mut updated = sp1.clone();
    let t_walk = std::time::Instant::now();
    let alloc = sparse_window_allocate_incompatible(&mut updated, 8, dil_min, dil_max, &clf2)
        .expect("windowed walk must not hit index corruption");
    let walk_ms = t_walk.elapsed().as_secs_f64() * 1000.0;
    let (mut bmin, mut bmax) = (dil_min, dil_max);
    if alloc.appended > 0 {
        for a in 0..3 {
            bmin[a] = bmin[a].min(alloc.new_bricks_min[a] * 8);
            bmax[a] = bmax[a].max(((alloc.new_bricks_max[a] + 1) * 8).min(res));
        }
    }
    // Append premise: the two original gates NEED appended bricks (that is what they exercise).
    // A carve gesture over an already-fully-handled window legitimately appends nothing (handled
    // bricks are compatible-by-table and are refreshed by the windowed bake) — such callers pass
    // `require_append = false` and are gated by the oracle/bit-exactness checks below instead.
    if require_append {
        assert!(alloc.appended > 0, "premise: the edit must allocate new surface bricks");
    }

    // Windowed mode-2 bake against the UPDATED index, pool seeded from round 1 and GROWN with
    // garbage for the appended handles (ensure_capacity's content-preserving grow shape — new
    // bricks hold garbage until this very bake writes them; any survivor is a missed write).
    let mut pool_w = pool1.clone();
    pool_w.resize(updated.allocated * 512, POOL_GARBAGE);
    let (sg2, body2) = make_body(round2.clone(), 0, Vec3::ZERO);
    let pool_w = dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax, BakeOut::Sparse {
        index: &updated.index, allocated: updated.allocated, dims: updated.dims_bricks,
        seed: Some(&pool_w),
    }, Some((bmin, bmax)));

    // REFERENCE: from-scratch FULL classify + full sparse bake of the edited scene.
    let sp2 = classify_prims_to_sparse_index(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let pool_r = dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax, BakeOut::Sparse {
        index: &sp2.index, allocated: sp2.allocated, dims: sp2.dims_bricks, seed: None,
    }, None);
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin, gmax, 0, 0, Vec3::ZERO);

    // ── Brick-level index comparison ──
    let (dxb, dyb, dzb) = (sp1.dims_bricks[0] as usize, sp1.dims_bricks[1] as usize,
        sp1.dims_bricks[2] as usize);
    let bw0 = [dil_min[0] / 8, dil_min[1] / 8, dil_min[2] / 8];
    let bw1 = [(dil_max[0] - 1) / 8, (dil_max[1] - 1) / 8, (dil_max[2] - 1) / 8]; // inclusive
    // Oracle sign-crossing per brick (voxels ± a 1-voxel ring, the DC read footprint): the ONE
    // structural soundness criterion — every crossing brick MUST be handled. (Comparing handled
    // SETS against the reference is wrong on both sides: the reference full classify
    // over-allocates its conservative margin far beyond the window, and the incremental walk
    // legitimately keeps handles it cannot PROVE vacated — both are mesh-benign supersets of
    // the crossing set.)
    let crossing = |bx: usize, by: usize, bz: usize| -> bool {
        let (mut neg, mut pos) = (false, false);
        let x0 = (bx * 8).saturating_sub(1); let x1 = ((bx + 1) * 8 + 1).min(res as usize);
        let y0 = (by * 8).saturating_sub(1); let y1 = ((by + 1) * 8 + 1).min(res as usize);
        let z0 = (bz * 8).saturating_sub(1); let z1 = ((bz + 1) * 8 + 1).min(res as usize);
        for z in z0..z1 { for y in y0..y1 { for x in x0..x1 {
            let v = oracle2[x + y * res as usize + z * (res * res) as usize];
            if v.abs() <= voxel { continue; } // skin — either verdict defensible
            if v < 0.0 { neg = true; } else { pos = true; }
            if neg && pos { return true; }
        }}}
        false
    };
    let mut crossing_holes = 0usize;
    let mut out_tag_mismatch = 0usize;
    let mut both_sentinel_mismatch = 0usize;
    let mut superset_bricks = 0usize;
    for bz in 0..dzb { for by in 0..dyb { for bx in 0..dxb {
        let b = bx + by * dxb + bz * dxb * dyb;
        let in_win = (bw0[0] as usize..=bw1[0] as usize).contains(&bx)
            && (bw0[1] as usize..=bw1[1] as usize).contains(&by)
            && (bw0[2] as usize..=bw1[2] as usize).contains(&bz);
        if !in_win && updated.index[b] != sp1.index[b] {
            // The 26-ring apron of an appended Surface brick may legally extend ONE brick
            // beyond the window (the walk's documented widening); everything further out is
            // untouched. Check the brick is within 1 of the window box.
            let near = (bx as i64 - bw0[0] as i64) >= -1 && (bx as i64 - bw1[0] as i64) <= 1
                && (by as i64 - bw0[1] as i64) >= -1 && (by as i64 - bw1[1] as i64) <= 1
                && (bz as i64 - bw0[2] as i64) >= -1 && (bz as i64 - bw1[2] as i64) <= 1;
            if !near { out_tag_mismatch += 1; }
        }
        let (tw, tr) = (updated.index[b], sp2.index[b]);
        if is_sentinel(tw) && crossing(bx, by, bz) { crossing_holes += 1; }
        match (is_sentinel(tw), is_sentinel(tr)) {
            (false, true) => superset_bricks += 1, // benign superset (sign-proved below)
            (true, true) => if tw != tr {
                // Sentinels encode SIGN: the two classifiers judged the SAME edited field
                // (outside the window the field's sign did not change; inside, the walk uses
                // the identical predicate) — a sign disagreement is a real defect.
                both_sentinel_mismatch += 1;
            },
            _ => {}
        }
    }}}
    assert_eq!(crossing_holes, 0,
        "{label}: iso-crossing bricks are sentinels in the windowed index — HOLES in the mesh");
    assert_eq!(out_tag_mismatch, 0,
        "{label}: index tags beyond the window+apron changed — the walk leaked");
    assert_eq!(both_sentinel_mismatch, 0,
        "{label}: sentinel SIGN disagrees with the reference classify");

    // ── Voxel-level decode comparison ──
    let slot_w: HashMap<u32, usize> = (0..updated.allocated as u32).map(|h| (h, h as usize)).collect();
    let slot_1: HashMap<u32, usize> = (0..sp1.allocated as u32).map(|h| (h, h as usize)).collect();
    let slot_r: HashMap<u32, usize> = (0..sp2.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc_w = BrickGridAccessor::new(&updated.index, &pool_w, &slot_w, updated.dims_bricks, 8, res - 1);
    let acc_1 = BrickGridAccessor::new(&sp1.index, &pool1, &slot_1, sp1.dims_bricks, 8, res - 1);
    let acc_r = BrickGridAccessor::new(&sp2.index, &pool_r, &slot_r, sp2.dims_bricks, 8, res - 1);
    let (mut sign_flips, mut near_dv) = (0usize, 0.0f32);
    let (mut in_both_handled_bits, mut out_bits, mut superset_sign_bad) = (0usize, 0usize, 0usize);
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let sw = acc_w.sample(x, y, z);
        // Whole-grid: sign + near-surface value vs the edited scene's dense CPU oracle.
        let cpu = oracle2[x + y * res as usize + z * (res * res) as usize];
        if (sw < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && sw.abs() > voxel { sign_flips += 1; }
        if cpu.abs() < 3.0 * voxel { near_dv = near_dv.max((sw - cpu).abs()); }

        let b = (x / 8) + (y / 8) * dxb + (z / 8) * dxb * dyb;
        let in_window = (x as u32) >= bmin[0] && (x as u32) < bmax[0]
            && (y as u32) >= bmin[1] && (y as u32) < bmax[1]
            && (z as u32) >= bmin[2] && (z as u32) < bmax[2];
        let (tw, tr) = (updated.index[b], sp2.index[b]);
        if in_window {
            if !is_sentinel(tw) && !is_sentinel(tr) {
                // Both handled, both written by the SAME shader over the SAME edited field →
                // BIT-IDENTICAL (the S2.3 bit-exactness contract).
                if sw.to_bits() != acc_r.sample(x, y, z).to_bits() { in_both_handled_bits += 1; }
            } else if !is_sentinel(tw) && is_sentinel(tr) {
                // Benign superset: the windowed handle's TRUE values must agree in sign with
                // the reference sentinel (no crossing can exist ⇒ identical DC output).
                // 1-voxel skin excluded — same tolerance as every sign check in this file.
                let sref = acc_r.sample(x, y, z);
                if (sw < 0.0) != (sref < 0.0) && sw.abs() > voxel { superset_sign_bad += 1; }
            }
        } else if updated.index[b] == sp1.index[b] {
            // Fully out-of-window, untouched brick: pool content bit-identical to round 1.
            if sw.to_bits() != acc_1.sample(x, y, z).to_bits() { out_bits += 1; }
        }
    }}}
    eprintln!(
        "[SparseWindowed] {label}: res={res} window=[{dil_min:?}..{dil_max:?}) bricks={wb}/{total} \
         walk={walk_ms:.1}ms (classify={:.1} apply={:.1}) +{} (reused={}) retag={} reclaim={} \
         pool {}→{} (ref {}) superset_bricks={superset_bricks} | sign_flips={sign_flips} \
         near|dv|={near_dv:.4} in_both_handled_bits={in_both_handled_bits} out_bits={out_bits}",
        alloc.classify_ms, alloc.apply_ms, alloc.appended, alloc.reused, alloc.retagged,
        alloc.reclaimed, sp1.allocated, updated.allocated, sp2.allocated,
    );
    assert_eq!(sign_flips, 0,
        "{label}: windowed-escalation atlas disagrees with the edited scene's oracle on sign");
    assert!(near_dv < NEAR_TOL,
        "{label}: near-surface divergence {near_dv:.4} vs the edited oracle (tol {NEAR_TOL})");
    assert_eq!(in_both_handled_bits, 0,
        "{label}: both-handled window voxels differ from the full-classify reference bake");
    assert_eq!(superset_sign_bad, 0,
        "{label}: a superset handle's values cross iso where the reference proved a sentinel");
    assert_eq!(out_bits, 0,
        "{label}: out-of-window pool content changed — the windowed write leaked");
}

/// GATE 1 — the gesture shape that fires the escalation: a prim moved FAR (well beyond the
/// classifier's conservative margin, so real handles are appended at the new position AND the
/// vacated far side reclaims), with an old ∪ new dirty window in the (1/8, 1/2] brick band.
/// The windowed escalated reclassify must land a final atlas state equivalent to a
/// from-scratch full classify — alloc + reclaim through the shared free list.
#[test]
fn sparse_windowed_escalation_matches_full_classify() {
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let r = 0.6f32;
    let (p0, p1) = (Vec3::new(-0.75, 0.0, 0.0), Vec3::new(0.45, 0.0, 0.0)); // 1.2 > margin ~0.87
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 })];
    run_windowed_escalation_gate(
        "far-move", res, gmin, gmax, round1, round2,
        (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)),
        (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)),
        true,
    );
}

/// GATE 2 — ADVERSARIAL BOUNDARY: a second, UNEDITED sphere whose near-surface band crosses
/// right at the dirty window's face, while the edited prim moves up next to it. The dilate
/// margin must keep the boundary bricks correct; the unedited surface outside the window must
/// stay byte-untouched (index tags + pool content) and the whole-grid oracle + crossing-brick
/// checks prove no seam hole opened across the window face.
#[test]
fn sparse_windowed_reclassify_window_boundary_surface_preserved() {
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let r = 0.7f32;
    // A moves 1.25 on Y (> the ~0.87 classifier margin, so its new top surface really appends).
    let (p0, p1) = (Vec3::new(-1.0, -0.9, 0.0), Vec3::new(-1.0, 0.35, 0.0));
    // Window max-x ≈ p1.x + r + margins + dilation ≈ -0.1; B's surface spans x ∈ [-0.2, 0.9] —
    // its leftmost surface bricks STRADDLE the window face (part re-baked in-window, part kept
    // verbatim from round 1 — the seam across B's own surface must stay crack-free).
    let b_sphere = sphere_op(Vec3::new(0.35, 0.0, 0.0), 0.55, BooleanOp::Union { k: 0.0 });
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 }), b_sphere.clone()];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 }), b_sphere];
    run_windowed_escalation_gate(
        "boundary-surface", res, gmin, gmax, round1, round2,
        (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)),
        (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)),
        true,
    );
}

/// MEASUREMENT (#[ignore]d, `-- --ignored --nocapture`) — the windowed-escalation walk cost at
/// 1024³ for gesture-scale windows (the decision datum vs the full classify's measured
/// 17–30ms GPU + 5–6ms frame-thread validate, LIVE ATTRIBUTION 2026-07-16). Pure CPU — the
/// walk runs on a background worker in production, so this is worker-thread latency, not
/// frame-thread cost (frame-thread cost of the windowed path is ~0: spawn + land).
#[test]
#[ignore]
fn w_measure_windowed_walk_1024() {
    let res = 1024u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let prims = vec![
        sphere_op(Vec3::ZERO, 1.2, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.6, 0.2, 0.0), 0.7, BooleanOp::Subtract { k: 0.0 }),
    ];
    let t0 = std::time::Instant::now();
    let sp = classify_prims_to_sparse_index(&prims, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    eprintln!("[WMeasure] full CPU classify at 1024³: {:.1}ms bricks={}/{}",
        t0.elapsed().as_secs_f64() * 1000.0, sp.allocated, sp.index.len());
    // Post-edit prims (the gesture tick the walk classifies against).
    let prims2 = vec![
        sphere_op(Vec3::ZERO, 1.2, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.6, 0.2, 0.0), 0.85, BooleanOp::Subtract { k: 0.0 }),
    ];
    let clf = BrickClassifier::new(&prims2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    for frac in [0.30f32, 0.45, 0.60, 0.79] {
        let half = (res as f32 * frac * 0.5) as u32;
        let c = res / 2;
        let (wmin, wmax) = ([c - half; 3], [c + half; 3]);
        let (dil_min, dil_max) = dilate_voxel_window(wmin, wmax, 8, [res; 3]);
        let wb: u64 = (0..3).map(|a| ((dil_max[a] - dil_min[a] + 7) / 8) as u64).product();
        let total = (sp.index.len()) as u64;
        let mut best = f32::INFINITY;
        let mut last = None;
        for _ in 0..3 {
            let mut updated = sp.clone();
            let alloc = sparse_window_allocate_incompatible(&mut updated, 8, dil_min, dil_max, &clf)
                .expect("no corruption");
            best = best.min(alloc.classify_ms + alloc.apply_ms);
            last = Some(alloc);
        }
        let a = last.unwrap();
        eprintln!("[WMeasure] window {:.0}%/axis = {wb}/{total} bricks ({:.1}%): \
                   walk best={best:.1}ms (classify={:.1} apply={:.1}) +{} retag={} reclaim={}",
            frac * 100.0, frac * frac * frac * 100.0,
            a.classify_ms, a.apply_ms, a.appended, a.retagged, a.reclaimed);
    }
}

/// S2.5 escalation — the prim moves ACROSS the brick frontier (into formerly-EXTERIOR bricks,
/// the S2.3 adversarial geometry): a GENUINE surface-crossing, so the S2.5b tag-compatibility
/// rule (the production check) must still escalate — with Surface-vs-EXTERIOR evidence — and
/// the escalated FULL bake (fresh classify + full-box mode-2 — S2.3's round-2 machinery,
/// `check_scene`) must match the moved scene's oracle.
#[test]
fn sparse_partial_escalates_across_brick_frontier() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // The S2.3 adversarial move: 1.5 world units — far beyond band + 2×diag margin (~0.93).
    let (p0, r) = (Vec3::new(-0.75, 0.0, 0.0), 0.3f32);
    let p1 = Vec3::new(0.75, 0.0, 0.0);
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 })];

    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);

    // Production window for the move (old ∪ new AABB + margin), dilated. The new sphere sits
    // in bricks that are EXTERIOR in round 1's index — the S2.5b PRODUCTION rule must escalate
    // (its mini-classify needs Surface where the frozen tag is a sentinel: a partial would
    // silently drop the moved surface's writes).
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)) - margin;
    let wmax = (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let inc = sparse_window_tags_compatible(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max, &clf2)
        .expect_err(
            "cross-frontier move NOT flagged for escalation — a partial would write into \
             sentinel bricks and silently drop the moved surface");
    assert_eq!(inc.needed, BrickClass::Surface,
        "frontier cross must escalate as surface-into-sentinel, got {inc:?}");
    assert_eq!(inc.frozen_tag, Some(BRICK_TAG_EXTERIOR),
        "frontier cross must hit a formerly-EXTERIOR brick, got {inc:?}");

    // The escalated FULL bake = S2.3's round-2 machinery verbatim: fresh classify + full-box
    // mode-2 bake + decode vs the moved scene's oracle (+ the mode-0 negative control).
    check_scene(&gpu, "escalated-full-after-frontier-cross", &round2, 0, Vec3::ZERO,
        res, gmin, gmax, band, true, false);
}

/// Blocking readback of a GPU buffer's first `floats` f32s (test-only; the buffer needs COPY_SRC).
fn read_back_f32(gpu: &BakeGpu, buf: &wgpu::Buffer, floats: usize) -> Vec<f32> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("s25c_readback"), size: (floats * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(buf, 0, &staging, 0, (floats * 4) as u64);
    gpu.queue.submit(Some(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map channel").expect("map failed");
    let out = bytemuck::cast_slice::<u8, f32>(&staging.slice(..).get_mapped_range()).to_vec();
    out
}

/// S2.5c — the pool-growth PRESERVATION hazard, unit-level: `ensure_capacity` growing the pool
/// MUST GPU-copy the old bricks into the replacement buffer. Incremental allocation grows the
/// pool MID-DRAG with only a windowed partial bake after — without the copy, every brick outside
/// that window would decode as garbage (slice-1's original grow just swapped buffers).
#[test]
fn sparse_atlas_pool_growth_preserves_content() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    // 16³ = 4096 bricks — exactly one grow quantum, so +1 brick forces a growth.
    let dims = [16u32, 16, 16];
    let brick_count = 4096usize;
    let mut atlas = graphics::SdfBrickAtlasGpu::new(&gpu.device, brick_count as u32, brick_count as u32);
    assert_eq!(atlas.pool_capacity_bricks, 4096, "test premise: exact one-quantum capacity");

    // Known patterned pool + a mixed index (content is irrelevant to the copy — the pattern is).
    let index: Vec<u32> = (0..brick_count as u32).collect();
    let pool: Vec<f32> = (0..brick_count * 512).map(|i| i as f32 * 0.5).collect();
    atlas.upload(&gpu.queue, &index, &pool, dims, 128, NodeId(31));
    let gen_before = atlas.generation;

    // Force a pool growth (index fits; pool needs one brick more than capacity).
    atlas.ensure_capacity(&gpu.device, &gpu.queue, brick_count as u32, brick_count as u32 + 1);
    assert!(atlas.pool_capacity_bricks > 4096, "pool must have grown");
    assert_eq!(atlas.generation, gen_before + 1, "growth must bump the generation exactly once");

    // Every previously-uploaded float must have survived the buffer swap.
    let back = read_back_f32(&gpu, &atlas.pool_buf, brick_count * 512);
    let mismatches = back.iter().zip(pool.iter()).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
    eprintln!("[SparseBake] growth-preservation: {} bricks copied, {} bit-mismatches", brick_count, mismatches);
    assert_eq!(mismatches, 0, "pool growth lost previously-baked brick content");
}

/// S2.5c — INCREMENTAL ALLOCATION end-to-end on hardware (the fast-drag fix): a move FAR beyond
/// the classifier margin lands needs-Surface bricks on EXTERIOR sentinels. The old response was
/// a full re-classify + full-box bake every frame (owner-measured 72-82ms); the new response
/// APPENDS handles for the incompatible bricks (+ the Surface 26-ring apron), re-uploads the
/// index, and bakes only the expanded window. Verifies:
///   - the allocator appends (> 0) and every moved-scene Surface brick in the window is handled;
///   - the windowed mode-2 bake fills the NEW bricks with oracle-true values;
///   - voxels outside the expanded window decode BIT-IDENTICAL to round 1 (old handles survive —
///     production pairs this with the growth-preserving ensure_capacity proven above).
#[test]
fn sparse_partial_incremental_alloc_far_move_matches_oracle() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Far move: beyond the ~2×brick-diagonal margin, so round 2's surface lands in bricks the
    // round-1 classifier left EXTERIOR (asserted below — no vacuous pass).
    let (p0, r) = (Vec3::new(-0.62, 0.0, 0.0), 0.24f32);
    let p1 = Vec3::new(0.62, 0.0, 0.0);
    let round1 = vec![sphere_op(p0, r, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![sphere_op(p1, r, BooleanOp::Union { k: 0.0 })];

    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(sp1.allocated > 0 && sp1.allocated < sp1.index.len(),
        "vacuous premise: round 1 must leave sentinel bricks (allocated {} of {})",
        sp1.allocated, sp1.index.len());
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    let pool1 = dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: None,
    }, None);
    let oracle1 = crate::bake_utils::bake_body_grid(&round1, res, gmin, gmax, 0, 0, Vec3::ZERO);
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin, gmax, 0, 0, Vec3::ZERO);

    // Production window semantics: old ∪ new AABB + 2-voxel margin → voxel window → 1-brick dilation.
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)) - margin;
    let wmax = (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);

    // Premise: the far move IS incompatible with the frozen index (the S2.5b check escalates)…
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(
        sparse_window_tags_compatible(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max, &clf2).is_err(),
        "premise broken: the far move should be incompatible with the frozen index"
    );
    // … and the S2.5c allocator turns that into appended handles instead of a full rebake.
    let mut updated = SparseIndexOnly {
        index: sp1.index.clone(), dims_bricks: sp1.dims_bricks, allocated: sp1.allocated,
        freed: Vec::new(),
    };
    let alloc = sparse_window_allocate_incompatible(
        &mut updated, 8, dil_min, dil_max, &clf2,
    ).expect("far move must allocate, not escalate");
    assert!(alloc.appended > 0, "far move must append handles");
    // S2.5f: the allocator maintains `allocated` itself (recycled handles don't grow the pool).
    assert_eq!(
        updated.allocated,
        sp1.allocated + (alloc.appended - alloc.reused) as usize,
        "allocated watermark must grow by exactly the non-recycled appends"
    );

    // Apron invariant: every brick the MOVED scene needs as Surface inside the window is handled.
    let (dx, dy) = (sp1.dims_bricks[0] as usize, sp1.dims_bricks[1] as usize);
    for bz in dil_min[2] / 8..(dil_max[2].div_ceil(8)).min(sp1.dims_bricks[2]) {
        for by in dil_min[1] / 8..(dil_max[1].div_ceil(8)).min(sp1.dims_bricks[1]) {
            for bx in dil_min[0] / 8..(dil_max[0].div_ceil(8)).min(sp1.dims_bricks[0]) {
                if clf2.classify_one_brick(bx, by, bz) == BrickClass::Surface {
                    let tag = updated.index[bx as usize + by as usize * dx + bz as usize * dx * dy];
                    assert!(tag < BRICK_TAG_INTERIOR,
                        "moved-scene Surface brick ({bx},{by},{bz}) left unhandled after allocation");
                }
            }
        }
    }

    // Production dispatch-window math: dilated window ∪ the new-brick voxel box.
    let mut ebake_min = dil_min;
    let mut ebake_max = dil_max;
    for a in 0..3 {
        ebake_min[a] = ebake_min[a].min(alloc.new_bricks_min[a] * 8);
        ebake_max[a] = ebake_max[a].max(((alloc.new_bricks_max[a] + 1) * 8).min(res));
    }

    // Pool seed = round-1 content at old handles + garbage for the pool EXTENSION — exactly the
    // post-growth state production's preserving ensure_capacity leaves behind. Recycled handles
    // (S2.5f) point INTO pool1's range and keep the vacated brick's stale content — production's
    // state too; the windowed bake must overwrite them (they're in the new-bricks box).
    let mut seed = pool1.clone();
    seed.extend(std::iter::repeat(POOL_GARBAGE)
        .take((updated.allocated - sp1.allocated) * 512));
    let (sg2, body2) = make_body(round2, 0, Vec3::ZERO);
    let pool2 = dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax, BakeOut::Sparse {
        index: &updated.index, allocated: updated.allocated, dims: updated.dims_bricks, seed: Some(&seed),
    }, Some((ebake_min, ebake_max)));

    let slot1: HashMap<u32, usize> = (0..sp1.allocated as u32).map(|h| (h, h as usize)).collect();
    let slot2: HashMap<u32, usize> = (0..updated.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc1 = BrickGridAccessor::new(&sp1.index, &pool1, &slot1, sp1.dims_bricks, 8, res - 1);
    let acc2 = BrickGridAccessor::new(&updated.index, &pool2, &slot2, updated.dims_bricks, 8, res - 1);

    let (mut in_sign_flips, mut in_near_dv) = (0usize, 0.0f32);
    let (mut out_bit_mismatch, mut out_sign_flips, mut out_sliver_bad) = (0usize, 0usize, 0usize);
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let s1 = acc1.sample(x, y, z);
        let s2 = acc2.sample(x, y, z);
        let in_window = (x as u32) >= ebake_min[0] && (x as u32) < ebake_max[0]
            && (y as u32) >= ebake_min[1] && (y as u32) < ebake_max[1]
            && (z as u32) >= ebake_min[2] && (z as u32) < ebake_max[2];
        let li = x + y * res as usize + z * (res * res) as usize;
        if in_window {
            let cpu = oracle2[li];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { in_sign_flips += 1; }
            if cpu.abs() < 3.0 * voxel { in_near_dv = in_near_dv.max((s2 - cpu).abs()); }
        } else {
            // Brick-accurate out-of-window contract (S2.5f): tag mutations (retag/reclaim) are
            // BRICK-granular, so a brick straddling the window boundary legitimately changes the
            // decode of its outside-the-voxel-box sliver too — from the stale stored value to the
            // sentinel of its CURRENT strict verdict. Untouched bricks stay bit-identical; walked
            // slivers must be sign-correct for the CURRENT field (the verdict's own guarantee).
            let bi = (x as u32 / 8) as usize
                + (y as u32 / 8) as usize * sp1.dims_bricks[0] as usize
                + (z as u32 / 8) as usize * (sp1.dims_bricks[0] * sp1.dims_bricks[1]) as usize;
            if sp1.index[bi] == updated.index[bi] {
                if s2.to_bits() != s1.to_bits() { out_bit_mismatch += 1; }
                let cpu = oracle1[li];
                if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { out_sign_flips += 1; }
            } else {
                let cpu2 = oracle2[li];
                if (s2 < 0.0) != (cpu2 < 0.0) && cpu2.abs() > voxel && s2.abs() > voxel { out_sliver_bad += 1; }
            }
        }
    }}}
    eprintln!(
        "[SparseBake] incremental-alloc: appended={} (reused={} reclaim={}) (pool {}→{}) \
         window=[{:?}..{:?}] | in: sign_flips={in_sign_flips} near|dv|={in_near_dv:.4} | \
         out: bit_mismatch={out_bit_mismatch} sign_flips={out_sign_flips} sliver_bad={out_sliver_bad}",
        alloc.appended, alloc.reused, alloc.reclaimed,
        sp1.allocated, updated.allocated, ebake_min, ebake_max,
    );
    assert_eq!(in_sign_flips, 0, "in-window decode disagrees with the moved scene's oracle");
    assert!(in_near_dv < NEAR_TOL, "in-window near-surface divergence {in_near_dv:.4}");
    assert_eq!(out_bit_mismatch, 0,
        "out-of-window content changed in an UNTOUCHED brick — old handles must survive");
    assert_eq!(out_sign_flips, 0, "out-of-window decode no longer matches round-1's oracle");
    assert_eq!(out_sliver_bad, 0,
        "a walked boundary brick's out-of-window sliver is sign-wrong for the current field");
}

/// P1 scrolling-grid escape — END-TO-END mechanism parity. Emulates exactly what
/// `try_sparse_scroll` + the frozen-index partial arm do on an escape frame:
///
///   round-1 full sparse bake at window B0 → the prim moves → SCROLL: window B1 = B0 + k·brick_w
///   (`sparse_index_translate`, retained handles keep their round-1 pool content verbatim) →
///   dirty window = prim stamp ∪ exposed slab (the production region semantics) → mini-classify
///   + `sparse_window_allocate_incompatible` at B1 → windowed mode-2 bake of the MOVED body at B1
///   over a pool seeded with round-1 content + growth garbage (production's preserving
///   ensure_capacity state).
///
/// Contracts pinned:
///   - in-window decode matches the moved scene's dense CPU oracle AT THE NEW BOUNDS
///     (sign-exact outside a 1-voxel skin, near-surface |dv| < NEAR_TOL);
///   - out-of-window UNTOUCHED retained bricks are BIT-IDENTICAL to round-1's decode at the
///     brick-shifted source coords — a scroll re-bakes NOTHING it retained;
///   - walked boundary bricks obey the brick-granular sliver rule (sign-correct for the moved
///     field);
///   - the slab's new handles recycle the scroll-freed slots first (pool growth bounded).
#[test]
fn sparse_scroll_window_translate_matches_oracle() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseBake] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin0, gmax0) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let cell = (gmax0 - gmin0) / (res - 1) as f32;
    let voxel = cell.min_element();
    let band = 1.5 * cell.max_element();

    // Body near the +x wall of B0; the move pushes it toward/over the wall — the scroll case.
    let (p0, r) = (Vec3::new(0.62, 0.08, -0.11), 0.24f32);
    let he = Vec3::new(0.16, 0.22, 0.13);
    let round1 = vec![
        sphere_op(p0, r, BooleanOp::Union { k: 0.0 }),
        box_op(p0 + Vec3::new(-0.3, -0.05, 0.1), he, BooleanOp::Union { k: 0.12 }),
    ];
    let mv = Vec3::new(0.5, 0.0, 0.0); // ≈ 2 bricks of travel in +x
    let round2: Vec<SdfPrimitive> = {
        let mut p = round1.clone();
        p[0].position += mv;
        p[1].position += mv;
        p
    };

    // Round 1: full sparse bake at B0.
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin0, gmax0, res, 8, band);
    assert!(sp1.allocated > 0);
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    let pool1 = dispatch_bake(&gpu, &sg1, body1, res, gmin0, gmax0, BakeOut::Sparse {
        index: &sp1.index, allocated: sp1.allocated, dims: sp1.dims_bricks, seed: None,
    }, None);

    // SCROLL: +2 bricks in x (window chases the moving body). brick_w = 8·cell.
    let k = [2i32, 0, 0];
    let brick_w = cell * 8.0;
    let delta = Vec3::new(k[0] as f32 * brick_w.x, k[1] as f32 * brick_w.y, k[2] as f32 * brick_w.z);
    let (gmin1, gmax1) = (gmin0 + delta, gmax0 + delta);
    // Premise: the scrolled window contains the moved body (else production would grow instead).
    let tight1_max = (p0 + mv + Vec3::splat(r)).max(p0 + mv - Vec3::new(0.3, 0.05, -0.1) + he);
    assert!(tight1_max.x < gmax1.x, "premise: moved body must fit the scrolled window");
    let mut updated = sparse_index_translate(&sp1, k);
    let scroll_freed = updated.freed.len();
    let retained_before = updated.index.iter().filter(|&&t| t < BRICK_TAG_INTERIOR).count();
    assert!(retained_before > 0, "premise: the scroll must retain surface bricks");

    // Production window semantics at B1: prim stamp (old ∪ new AABB + 2-voxel margin) ∪ the
    // exposed slab strip (the part of B1 the old window never covered), then 1-brick dilation.
    let margin = Vec3::splat(2.0 * voxel);
    let stamp_min = (p0 - Vec3::splat(r + 0.35)).min(p0 + mv - Vec3::splat(r + 0.35)) - margin;
    let stamp_max = (p0 + Vec3::splat(r + 0.35)).max(p0 + mv + Vec3::splat(r + 0.35)) + margin;
    let (mut win_min, mut win_max) = world_window_to_voxel(stamp_min, stamp_max, gmin1, gmax1, res);
    let (slab_min, slab_max) = world_window_to_voxel(
        Vec3::new(gmax0.x, gmin1.y, gmin1.z), gmax1, gmin1, gmax1, res);
    for a in 0..3 {
        win_min[a] = win_min[a].min(slab_min[a]);
        win_max[a] = win_max[a].max(slab_max[a]);
    }
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);

    // Mini-classify + allocate at the SCROLLED bounds with the MOVED prims.
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin1, gmax1, res, 8, band);
    let alloc = sparse_window_allocate_incompatible(&mut updated, 8, dil_min, dil_max, &clf2)
        .expect("scroll frame must allocate, not escalate");
    assert!(alloc.appended > 0, "the exposed slab must need surface handles");
    assert!(scroll_freed == 0 || alloc.reused > 0,
        "scroll-freed slots must recycle into the slab's new handles (freed={scroll_freed} reused={})",
        alloc.reused);

    // Dispatch window = dilated ∪ new-brick box (production math).
    let mut ebake_min = dil_min;
    let mut ebake_max = dil_max;
    for a in 0..3 {
        if alloc.appended > 0 {
            ebake_min[a] = ebake_min[a].min(alloc.new_bricks_min[a] * 8);
            ebake_max[a] = ebake_max[a].max(((alloc.new_bricks_max[a] + 1) * 8).min(res));
        }
    }

    // Pool seed = round-1 content + garbage for the growth (preserving ensure_capacity state).
    let mut seed = pool1.clone();
    seed.extend(std::iter::repeat(POOL_GARBAGE).take((updated.allocated - sp1.allocated) * 512));
    let (sg2, body2) = make_body(round2.clone(), 0, Vec3::ZERO);
    let pool2 = dispatch_bake(&gpu, &sg2, body2, res, gmin1, gmax1, BakeOut::Sparse {
        index: &updated.index, allocated: updated.allocated, dims: updated.dims_bricks, seed: Some(&seed),
    }, Some((ebake_min, ebake_max)));

    // Oracles: moved scene at the NEW bounds (in-window truth) — round-1 comparisons use
    // acc1 at the brick-shifted source coords (bit-identity, no oracle needed).
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin1, gmax1, 0, 0, Vec3::ZERO);

    let slot1: HashMap<u32, usize> = (0..sp1.allocated as u32).map(|h| (h, h as usize)).collect();
    let slot2: HashMap<u32, usize> = (0..updated.allocated as u32).map(|h| (h, h as usize)).collect();
    let acc1 = BrickGridAccessor::new(&sp1.index, &pool1, &slot1, sp1.dims_bricks, 8, res - 1);
    let acc2 = BrickGridAccessor::new(&updated.index, &pool2, &slot2, updated.dims_bricks, 8, res - 1);

    let translated_ref = sparse_index_translate(&sp1, k); // pre-allocate tags, for walked detection
    let (mut in_sign_flips, mut in_near_dv) = (0usize, 0.0f32);
    let (mut out_bit_mismatch, mut out_sliver_bad, mut out_src_lost) = (0usize, 0usize, 0usize);
    let vshift = [k[0] * 8, k[1] * 8, k[2] * 8];
    for z in 0..res as usize { for y in 0..res as usize { for x in 0..res as usize {
        let s2 = acc2.sample(x, y, z);
        let in_window = (x as u32) >= ebake_min[0] && (x as u32) < ebake_max[0]
            && (y as u32) >= ebake_min[1] && (y as u32) < ebake_max[1]
            && (z as u32) >= ebake_min[2] && (z as u32) < ebake_max[2];
        let li = x + y * res as usize + z * (res * res) as usize;
        if in_window {
            let cpu = oracle2[li];
            if (s2 < 0.0) != (cpu < 0.0) && cpu.abs() > voxel && s2.abs() > voxel { in_sign_flips += 1; }
            if cpu.abs() < 3.0 * voxel { in_near_dv = in_near_dv.max((s2 - cpu).abs()); }
        } else {
            // Out-of-window: this voxel's brick was either UNTOUCHED by the window-allocate
            // (tag identical to the pure translate) → bit-identity with round-1's decode at the
            // brick-shifted SOURCE coords — or WALKED (retag/reclaim/apron) → brick-granular
            // sliver rule: sign-correct for the moved field.
            let bi = (x as u32 / 8) as usize
                + (y as u32 / 8) as usize * updated.dims_bricks[0] as usize
                + (z as u32 / 8) as usize * (updated.dims_bricks[0] * updated.dims_bricks[1]) as usize;
            if updated.index[bi] == translated_ref.index[bi] {
                let (sx, sy, sz) = (x as i32 + vshift[0], y as i32 + vshift[1], z as i32 + vshift[2]);
                if sx < 0 || sy < 0 || sz < 0
                    || sx >= res as i32 || sy >= res as i32 || sz >= res as i32 {
                    // Source outside B0 = exposed slab — must be in-window by construction.
                    out_src_lost += 1;
                } else {
                    let s1 = acc1.sample(sx as usize, sy as usize, sz as usize);
                    if s2.to_bits() != s1.to_bits() { out_bit_mismatch += 1; }
                }
            } else {
                let cpu2 = oracle2[li];
                if (s2 < 0.0) != (cpu2 < 0.0) && cpu2.abs() > voxel && s2.abs() > voxel { out_sliver_bad += 1; }
            }
        }
    }}}
    eprintln!(
        "[SparseBake] scroll: k={k:?} retained={retained_before} freed={scroll_freed} \
         appended={} (reused={} reclaim={}) pool {}→{} window=[{:?}..{:?}] | \
         in: sign_flips={in_sign_flips} near|dv|={in_near_dv:.4} | \
         out: bit_mismatch={out_bit_mismatch} sliver_bad={out_sliver_bad} src_lost={out_src_lost}",
        alloc.appended, alloc.reused, alloc.reclaimed, sp1.allocated, updated.allocated,
        ebake_min, ebake_max,
    );
    assert_eq!(in_sign_flips, 0, "in-window decode disagrees with the moved scene at B1");
    assert!(in_near_dv < NEAR_TOL, "in-window near-surface divergence {in_near_dv:.4}");
    assert_eq!(out_bit_mismatch, 0,
        "an UNTOUCHED retained brick's content changed — a scroll must re-bake NOTHING it retained");
    assert_eq!(out_sliver_bad, 0,
        "a walked boundary brick's out-of-window sliver is sign-wrong for the moved field");
    assert_eq!(out_src_lost, 0,
        "an exposed-slab brick escaped the bake window — the slab regions must cover it");
}

/// P2-B5 — multi-tenant atlas LRU eviction policy: evicts oldest-stamp first, never evicts
/// `keep` (the body being dispatched — its bake/mesh borrows must not dangle), unstamped
/// (orphaned) atlases evict before stamped ones, and eviction stops exactly at the budget.
#[test]
fn sdf_atlas_lru_eviction_policy() {
    let Some((_gpu_lock, device, _queue)) = headless_device() else {
        eprintln!("[SparseBake] no GPU adapter — skipping"); return;
    };
    let nid = |n: u64| NodeId(n);
    let mk = || graphics::SdfBrickAtlasGpu::new(&device, 64, 64); // rounds to 1 grow-quantum ≈ 8.4MB
    let one = mk().vram_bytes();

    let mut atlases = HashMap::new();
    let mut lru = HashMap::new();
    for (n, stamp) in [(1u64, 30u64), (2, 10), (3, 20)] {
        atlases.insert(nid(n), mk());
        lru.insert(nid(n), stamp);
    }
    atlases.insert(nid(4), mk()); // NO stamp — an orphan (deleted body) must evict first

    // Budget = 2 atlases → evict until 2 remain: orphan 4 first, then oldest stamp (2).
    let evicted = crate::gpu_ctx::evict_sdf_atlases_over_budget(
        &mut atlases, &mut lru, nid(1), one * 2);
    assert_eq!(evicted.iter().map(|(n, _)| n.0).collect::<Vec<_>>(), vec![4, 2]);
    assert!(evicted.iter().all(|&(_, b)| b == one));
    assert!(atlases.contains_key(&nid(1)) && atlases.contains_key(&nid(3)));
    assert!(!lru.contains_key(&nid(2)), "evicted stamps must be dropped");

    // Budget below even ONE atlas: everything but `keep` goes; keep survives (the
    // single-tenant floor — the body being dispatched is never evicted).
    let evicted = crate::gpu_ctx::evict_sdf_atlases_over_budget(
        &mut atlases, &mut lru, nid(1), one / 2);
    assert_eq!(evicted.iter().map(|(n, _)| n.0).collect::<Vec<_>>(), vec![3]);
    assert_eq!(atlases.len(), 1);
    assert!(atlases.contains_key(&nid(1)), "keep must survive even over budget");

    // Under budget: strict no-op.
    let evicted = crate::gpu_ctx::evict_sdf_atlases_over_budget(
        &mut atlases, &mut lru, nid(1), one * 4);
    assert!(evicted.is_empty());
    assert_eq!(atlases.len(), 1);
}

/// S2.5d — sentinel↔sentinel sign flips RETAG instead of allocating (CPU-only, no GPU).
/// A fast drag's swept volume is mostly EXTERIOR↔INTERIOR flips; handling them with pool
/// handles (the S2.5c first cut) bloated the pool with constant-content bricks (+1485 on the
/// owner's first drag frame). The allocator must: retag flips in place, allocate ONLY for
/// needs-Surface (+ apron), agree with a fresh full classify on every window sentinel, and
/// touch nothing outside the window (+ the 1-brick apron reach).
#[test]
fn sparse_window_retag_sentinel_flips() {
    // Sentinel↔sentinel flips need a move MUCH longer than twice the classifier's
    // non-Lipschitz margin (2×brick diagonal ≈ 0.87 world here) — anything shorter transits
    // through the Surface band and allocates instead. The box/cell geometry is EXACTLY the
    // proven interior-core shape from sparse_partial_interior_sentinels_stay_partial (he=1.45
    // at cell ≈0.031 keeps a strict-INTERIOR core past margin + dilation), on a ±5 grid at
    // 320³ (same absolute cell size). The 3.2 move puts the WHOLE moved core (≈3.2±0.27) past
    // where the old scene's EXTERIOR sentinels begin (old surface reach + dilation ≈ 2.6), and
    // the old core lands deep in the moved scene's strict-EXTERIOR zone (< 0.83): both flip
    // directions are populated with whole bricks to spare.
    let res = 320u32;
    let (gmin, gmax) = (Vec3::splat(-5.0), Vec3::splat(5.0));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let he = Vec3::splat(1.45);
    let step = Vec3::new(3.2, 0.0, 0.0);
    let round1 = vec![box_op(Vec3::ZERO, he, BooleanOp::Union { k: 0.0 })];
    let round2 = vec![box_op(step, he, BooleanOp::Union { k: 0.0 })];

    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let n_int = sp1.index.iter().filter(|&&t| t == BRICK_TAG_INTERIOR).count();
    assert!(n_int > 0, "premise: INTERIOR sentinels must exist (got {n_int})");

    // Production window semantics: old ∪ new AABB + 2-voxel margin → voxel window → dilation.
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (-he).min(step - he) - margin;
    let wmax = he.max(step + he) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);

    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let mut sp_upd = SparseIndexOnly {
        index: sp1.index.clone(), dims_bricks: sp1.dims_bricks, allocated: sp1.allocated,
        freed: Vec::new(),
    };
    let alloc = sparse_window_allocate_incompatible(
        &mut sp_upd, 8, dil_min, dil_max, &clf2,
    ).expect("far move must allocate/retag, not escalate");
    let updated = &sp_upd.index;

    assert!(alloc.retagged > 0, "premise/regression: the swept volume must produce retags");
    assert!(alloc.appended > 0, "the moved frontier must allocate Surface bricks");
    // S2.5f: the vacated trailing surface (the old box's frontier inside the window) must give
    // its handles back — this is the pool-leak fix's anti-vacuity.
    assert!(alloc.reclaimed > 0, "the vacated surface must reclaim handles");
    assert_eq!(
        sp_upd.allocated,
        sp1.allocated + (alloc.appended - alloc.reused) as usize,
        "allocated watermark must grow by exactly the non-recycled appends"
    );
    assert_eq!(
        sp_upd.freed.len() as u32,
        alloc.reclaimed - alloc.reused,
        "freed list must hold exactly the reclaimed-but-not-yet-reused handles"
    );

    // Verdict parity with a fresh FULL classify of the moved scene: inside the window, every
    // brick the walk left as a SENTINEL must carry the class the full classifier would write.
    // (Handles are always compatible — an apron may hand a strict-Interior brick a handle.)
    let sp2 = classify_prims_to_sparse_index(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let (dx, dy) = (sp1.dims_bricks[0] as usize, sp1.dims_bricks[1] as usize);
    let lin = |bx: u32, by: u32, bz: u32| bx as usize + by as usize * dx + bz as usize * dx * dy;
    let (b0, b1) = (
        [dil_min[0] / 8, dil_min[1] / 8, dil_min[2] / 8],
        [(dil_max[0] - 1) / 8, (dil_max[1] - 1) / 8, (dil_max[2] - 1) / 8],
    );
    let (mut ext_to_int, mut int_to_ext) = (0usize, 0usize);
    for bz in b0[2]..=b1[2] {
        for by in b0[1]..=b1[1] {
            for bx in b0[0]..=b1[0] {
                let bi = lin(bx, by, bz);
                let (old, new) = (sp1.index[bi], updated[bi]);
                if new >= BRICK_TAG_INTERIOR {
                    // Sentinel after the walk → must match the full classify's sentinel class.
                    let expect = match sp2.index[bi] {
                        t if t >= BRICK_TAG_INTERIOR => t,
                        // sp2 allocated it only as another Surface brick's dilation ring; the
                        // walk's per-brick verdict for the brick itself governs — re-derive it.
                        _ => match clf2.classify_one_brick(bx, by, bz) {
                            BrickClass::Interior => BRICK_TAG_INTERIOR,
                            BrickClass::Exterior => BRICK_TAG_EXTERIOR,
                            BrickClass::Surface => panic!(
                                "walk left a needs-Surface brick ({bx},{by},{bz}) as a sentinel"),
                        },
                    };
                    assert_eq!(new, expect,
                        "window sentinel ({bx},{by},{bz}) disagrees with the full classify");
                    if old == BRICK_TAG_EXTERIOR && new == BRICK_TAG_INTERIOR { ext_to_int += 1; }
                    if old == BRICK_TAG_INTERIOR && new == BRICK_TAG_EXTERIOR { int_to_ext += 1; }
                }
            }
        }
    }
    assert!(ext_to_int > 0, "leading edge must retag EXTERIOR→INTERIOR");
    assert!(int_to_ext > 0, "trailing edge must retag INTERIOR→EXTERIOR");
    // A brick retagged first and handed a handle by a LATER Surface apron counts in BOTH
    // retagged and appended (harmless — the bake writes its truth), so the surviving flips
    // bound retagged from below and retagged+appended from above.
    assert!(
        (ext_to_int + int_to_ext) <= alloc.retagged as usize
            && (alloc.retagged as usize) <= ext_to_int + int_to_ext + alloc.appended as usize,
        "retag count {} outside the observed-flip bounds [{}, {}]",
        alloc.retagged, ext_to_int + int_to_ext,
        ext_to_int + int_to_ext + alloc.appended as usize
    );

    // Handle-space integrity (S2.5f): every live handle is < the watermark and appears EXACTLY
    // once across the whole index (a recycled handle re-pointing two bricks at one pool slot
    // would be silent field aliasing); extension handles are all used; freed handles are
    // referenced by NO brick (they were retagged to sentinels) and are themselves < watermark.
    let mut seen = vec![0u32; sp_upd.allocated];
    for &t in updated.iter() {
        if t < BRICK_TAG_INTERIOR {
            assert!((t as usize) < sp_upd.allocated, "handle {t} beyond the allocated watermark");
            seen[t as usize] += 1;
        }
    }
    assert!(seen.iter().all(|&c| c <= 1), "a pool handle is referenced by two bricks (aliasing)");
    for k in sp1.allocated..sp_upd.allocated {
        assert_eq!(seen[k], 1, "extension handle {k} allocated but referenced by no brick");
    }
    for &h in &sp_upd.freed {
        assert!((h as usize) < sp_upd.allocated, "freed handle {h} beyond the watermark");
        assert_eq!(seen[h as usize], 0, "freed handle {h} still referenced by a brick");
    }

    // Nothing outside the dilated window (+1-brick apron reach) may change.
    let mut outside_changed = 0usize;
    for bz in 0..sp1.dims_bricks[2] {
        for by in 0..sp1.dims_bricks[1] {
            for bx in 0..sp1.dims_bricks[0] {
                let inside_reach = (b0[0].saturating_sub(1)..=(b1[0] + 1)).contains(&bx)
                    && (b0[1].saturating_sub(1)..=(b1[1] + 1)).contains(&by)
                    && (b0[2].saturating_sub(1)..=(b1[2] + 1)).contains(&bz);
                if !inside_reach && updated[lin(bx, by, bz)] != sp1.index[lin(bx, by, bz)] {
                    outside_changed += 1;
                }
            }
        }
    }
    assert_eq!(outside_changed, 0, "the walk mutated bricks beyond the window + apron reach");

    eprintln!(
        "[SparseBake] retag test: appended={} (reused={}) retagged={} reclaimed={} freed_left={} \
         (ext→int={ext_to_int} int→ext={int_to_ext}) pool {}→{} window=[{dil_min:?}..{dil_max:?})",
        alloc.appended, alloc.reused, alloc.retagged, alloc.reclaimed, sp_upd.freed.len(),
        sp1.allocated, sp_upd.allocated,
    );
}

// ── 1024 un-gate — hardware end-to-end vs 512 ───────────────────────────────────────────────────

/// Timings + counts from one full sparse run (classify → bake → mesh) at one resolution.
struct SparseRunStats {
    total_bricks: usize,
    allocated: usize,
    vert_count: u32,
    index_count: u32,
    classify_ms: f64,
    finalize_ms: f64,
    bake_ms: f64,
    mesh_ms: f64,
    pool_mb: f64,
    index_mb: f64,
}

/// 1024 UN-GATE proof: the whole sparse pipeline — REQUEST-side production caps
/// (`sparse_cache` + `set_body_resolution(1024)` → `trigger_bake` must emit a 1024³ classify,
/// exercising `effective_resolution`/`max_mesh_resolution` + the cubic invariant), the SHARED
/// production GPU classify encode + finalize, the mode-2 atlas bake Z-slab sliced exactly like
/// `dispatch_pending_primary_bakes` (one submit per 64-voxel slab — the TDR guard), and the
/// windowed sparse DC mesh — runs a 1024³ body EXACTLY like 512³ on a real adapter. The same
/// scene (sphere + subtract) runs at both resolutions; surface-proportionality is asserted:
/// brick index dims (res/8)³, allocated bricks and mesh counts ≈4× (gate 3–5×) the 512 run.
/// Searched for existing implementations (REUSE rule): request flow =
/// `tests_gpu_classify::gpu_classify_production_flow_index_matches_cpu`; bake+mesh =
/// `sparse_bake_atlas_bake_then_mesh_indirect_nonzero` above — this test composes them at scale,
/// duplicating neither (shared encode/finalize fns, shared `dispatch_bake` harness).
fn sparse_run_at(
    gpu: &BakeGpu,
    classify_pipeline: &wgpu::ComputePipeline,
    finalize: &crate::gpu_ctx::ClassifyFinalizePipelines,
    res: u32,
) -> SparseRunStats {
    use crate::app_poll_bake::{decode_gpu_finalized_index, encode_gpu_classify_dispatch};
    use crate::body_volume_cache::bake_dispatch::{
        rigid_translate_tests::sparse_cache, sparse_classify_band, SPARSE_BRICK_DIM,
    };
    use std::time::Instant;

    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let prims = vec![
        sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ];
    let (sg, nid) = make_body(prims, 0, Vec3::ZERO);

    // REQUEST SIDE — the production caps under test. `set_body_resolution(res)` is exactly what
    // the Grid picker does; `effective_resolution` must admit `res` (1024 requires the sparse
    // regime's `max_mesh_resolution` — the un-gate), trigger_bake must queue a GPU classify at
    // the FULL requested resolution (any silent clamp to 512 fails the assert below).
    let mut cache = sparse_cache(nid, (gmin, gmax));
    cache.gpu_classify_ready = true;
    cache.set_body_resolution(nid, res);
    cache.clear_bake_debounce();
    cache.trigger_bake(&sg, None);
    assert_eq!(cache.pending_gpu_classify.len(), 1, "sparse FULL arm must queue ONE GPU classify at {res}³");
    let q = cache.pending_gpu_classify.pop().unwrap();
    assert_eq!(q.request.resolution, [res; 3],
        "request clamped: expected {res}³ — the resolution cap did not admit it");

    // GPU classify — the SHARED production encode (same fn dispatch_pending_gpu_classifies calls).
    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });
    let (bmin, bmax) = (q.request.bounds_min, q.request.bounds_max);
    let local_prims = q.local_prims.clone();
    let t = Instant::now();
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, classify_pipeline, &gpu.bgl, &dummy_tex,
        q.request, crate::gpu_jobs::IDENTITY_MAT4,
        // STAGE-3: production regime — verdicts→index entirely on-device.
        Some(finalize),
    );
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let classify_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(pending.map_ready.peek(), crate::map_state::OK, "index readback must map OK at {res}³");
    let bpa = res / SPARSE_BRICK_DIM;
    assert_eq!(pending.dims_bricks, [bpa; 3], "brick index dims must be (res/8)³");
    let raw: Vec<u32> = {
        let view = pending.staging.slice(..).get_mapped_range();
        bytemuck::cast_slice(&view).to_vec()
    };
    pending.staging.unmap();
    assert_eq!(raw.len(), pending.total_bricks + 1, "index tags + trailing allocated count");

    // GPU-finalized index: frame-thread validate only (the worker finalize is gone here).
    let t = Instant::now();
    let si = decode_gpu_finalized_index(&raw, pending.dims_bricks)
        .expect("GPU-finalized index failed structural validation");
    let finalize_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(si.allocated > 0, "classifier allocated nothing at {res}³ — vacuous scene");

    // PARITY GATE at production scale (512³ AND 1024³): the on-device dilate+alloc must be
    // BIT-IDENTICAL to the CPU full classify of the request's own snapshot/bounds/band —
    // curved scene, zero verdict divergence (spike gate), deterministic finalize both sides.
    let band = sparse_classify_band(bmin, bmax, res);
    let t_cpu = Instant::now();
    let cpu = core_math::chunked_dc::classify_prims_to_sparse_index(
        &local_prims, 0, Vec3::ZERO, bmin, bmax, res, SPARSE_BRICK_DIM, band,
    );
    let cpu_full_ms = t_cpu.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(si.allocated, cpu.allocated, "{res}³: allocation count diverged");
    assert_eq!(si.index, cpu.index,
        "{res}³: GPU-finalized index must be BIT-IDENTICAL to the CPU classify");
    eprintln!("[Sparse1024] {res}³ GPU-finalize parity OK (cpu full classify+finalize={cpu_full_ms:.1}ms)");
    let total_bricks = si.index.len();
    let pool_mb = si.allocated as f64 * 512.0 * 4.0 / (1024.0 * 1024.0);
    let index_mb = total_bricks as f64 * 4.0 / (1024.0 * 1024.0);

    // Mode-2 atlas bake, Z-SLAB SLICED like production (MAX_WG_Z_PER_SLICE×4 = 64 voxels per
    // submit — dispatch_pending_primary_bakes' TDR guard; dispatch_bake submits once per call).
    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, si.index.len() as u32, si.allocated as u32);
    atlas.ensure_capacity(&gpu.device, &gpu.queue, si.index.len() as u32, si.allocated as u32);
    atlas.upload_index(&gpu.queue, &si.index, si.allocated as u32, si.dims_bricks, res, nid);
    let t = Instant::now();
    let mut z0 = 0u32;
    while z0 < res {
        let z1 = (z0 + 64).min(res);
        let none = dispatch_bake(gpu, &sg, nid, res, gmin, gmax,
            BakeOut::SparseAtlas { atlas: &atlas, dims: si.dims_bricks },
            Some(([0, 0, z0], [res, res, z1])));
        assert!(none.is_empty());
        z0 = z1;
    }
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let bake_ms = t.elapsed().as_secs_f64() * 1000.0;

    // Windowed sparse DC mesh from the bake-written atlas.
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let t = Instant::now();
    mesher.mesh_into_slot_sparse(&gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, None);
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let mesh_ms = t.elapsed().as_secs_f64() * 1000.0;

    // Read back counters [vert_count, index_count] — no device loss means these map OK.
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("counters_stg"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.counters, 0, &staging, 0, 8);
    gpu.queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map_async callback dropped — device lost?")
        .expect("counters readback map failed");
    let counts: Vec<u32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    let (vert_count, index_count) = (counts[0], counts[1]);

    eprintln!(
        "[Sparse1024] res={res}³ bricks={}/{} (index={index_mb:.1}MB pool={pool_mb:.1}MB \
         atlas caps: index={} pool={}) verts={vert_count} idx={index_count} | \
         classify={classify_ms:.1}ms finalize={finalize_ms:.1}ms bake={bake_ms:.1}ms mesh={mesh_ms:.1}ms",
        si.allocated, total_bricks, atlas.index_capacity, atlas.pool_capacity_bricks,
    );
    SparseRunStats {
        total_bricks, allocated: si.allocated, vert_count, index_count,
        classify_ms, finalize_ms, bake_ms, mesh_ms, pool_mb, index_mb,
    }
}

#[test]
fn sparse_1024_runs_like_512_end_to_end() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[Sparse1024] no GPU adapter — skipping"); return; }
    };
    // classify_bricks pipeline from the SAME module + BGL as the bake (production shape).
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    // STAGE-3: the finalize set from the SAME production compile helper (module/layout shared).
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);

    let s512 = sparse_run_at(&gpu, &classify_pipeline, &finalize, 512);
    let s1024 = sparse_run_at(&gpu, &classify_pipeline, &finalize, 1024);

    // Index dims: 64³ and 128³ (the [128;3] gate; dims themselves asserted inside sparse_run_at).
    assert_eq!(s512.total_bricks, 64 * 64 * 64);
    assert_eq!(s1024.total_bricks, 128 * 128 * 128, "1024³ brick index must be 128³ = 2,097,152 entries");

    // Surface-proportionality: doubling res quadruples the surface brick count and the mesh.
    assert!(s512.allocated > 0 && s1024.allocated > 0);
    let brick_ratio = s1024.allocated as f64 / s512.allocated as f64;
    assert!((3.0..=5.0).contains(&brick_ratio),
        "allocated-brick ratio 1024/512 = {brick_ratio:.2}, expected ≈4× (3–5)");
    assert!(s512.index_count > 0 && s512.index_count % 3 == 0);
    assert!(s1024.index_count > 0 && s1024.index_count % 3 == 0,
        "1024³ sparse mesh produced zero indices");
    assert!(s1024.vert_count > 0);
    let idx_ratio = s1024.index_count as f64 / s512.index_count as f64;
    let vert_ratio = s1024.vert_count as f64 / s512.vert_count as f64;
    assert!((3.0..=5.0).contains(&idx_ratio),
        "mesh index ratio 1024/512 = {idx_ratio:.2}, expected ≈4× (3–5)");
    assert!((3.0..=5.0).contains(&vert_ratio),
        "mesh vertex ratio 1024/512 = {vert_ratio:.2}, expected ≈4× (3–5)");

    eprintln!(
        "[Sparse1024] RATIOS 1024/512: bricks={brick_ratio:.2} verts={vert_ratio:.2} idx={idx_ratio:.2} | \
         512: pool={:.1}MB cls={:.0}ms fin={:.0}ms bake={:.0}ms mesh={:.0}ms | \
         1024: pool={:.1}MB index={:.1}MB cls={:.0}ms fin={:.0}ms bake={:.0}ms mesh={:.0}ms",
        s512.pool_mb, s512.classify_ms, s512.finalize_ms, s512.bake_ms, s512.mesh_ms,
        s1024.pool_mb, s1024.index_mb, s1024.classify_ms, s1024.finalize_ms, s1024.bake_ms, s1024.mesh_ms,
    );
}

/// 2048-SHIP item 4 octave gate — the `sparse_1024_runs_like_512` contract, one octave up,
/// through the SAME production request path (`set_body_resolution(2048)` = the Grid picker,
/// `effective_resolution` admission via the widened `max_sparse_resolution`, GPU classify at
/// 256³ bricks over the 2D finalize grid, atlas, z-slab mode-2 bake, windowed DC mesh).
/// `#[ignore]`d like the spike: the realistic scene allocates ~723k bricks (≈1.4GB pool +
/// ≈1.4GB cell_vert transient) — a deliberate near-ceiling run, not suite-budget material.
/// Run: `cargo test -p app --bins sparse_2048_runs_like_1024 -- --ignored --nocapture --test-threads=1`
#[test]
#[ignore] // near-ceiling VRAM (≈4GB transient) + seconds of GPU time — owner/dev-GPU gate
fn sparse_2048_runs_like_1024_end_to_end() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[Sparse2048] no GPU adapter — skipping"); return; }
    };
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);

    let s1024 = sparse_run_at(&gpu, &classify_pipeline, &finalize, 1024);
    let s2048 = sparse_run_at(&gpu, &classify_pipeline, &finalize, 2048);

    assert_eq!(s2048.total_bricks, 256 * 256 * 256, "2048³ brick index must be 256³ = 16,777,216 entries");
    assert!(s1024.allocated > 0 && s2048.allocated > 0);
    let brick_ratio = s2048.allocated as f64 / s1024.allocated as f64;
    assert!((3.0..=5.0).contains(&brick_ratio),
        "allocated-brick ratio 2048/1024 = {brick_ratio:.2}, expected ≈4× (3–5)");
    assert!(s2048.index_count > 0 && s2048.index_count % 3 == 0, "2048³ sparse mesh produced zero indices");
    assert!(s2048.vert_count > 0);
    let idx_ratio = s2048.index_count as f64 / s1024.index_count as f64;
    let vert_ratio = s2048.vert_count as f64 / s1024.vert_count as f64;
    assert!((3.0..=5.0).contains(&idx_ratio),
        "mesh index ratio 2048/1024 = {idx_ratio:.2}, expected ≈4× (3–5)");
    assert!((3.0..=5.0).contains(&vert_ratio),
        "mesh vertex ratio 2048/1024 = {vert_ratio:.2}, expected ≈4× (3–5)");
    eprintln!(
        "[Sparse2048] RATIOS 2048/1024: bricks={brick_ratio:.2} verts={vert_ratio:.2} idx={idx_ratio:.2} | \
         1024: pool={:.1}MB cls={:.0}ms fin={:.0}ms bake={:.0}ms mesh={:.0}ms | \
         2048: pool={:.1}MB index={:.1}MB cls={:.0}ms fin={:.0}ms bake={:.0}ms mesh={:.0}ms",
        s1024.pool_mb, s1024.classify_ms, s1024.finalize_ms, s1024.bake_ms, s1024.mesh_ms,
        s2048.pool_mb, s2048.index_mb, s2048.classify_ms, s2048.finalize_ms, s2048.bake_ms, s2048.mesh_ms,
    );
}

// ── 1024 owner-crash repro: ADD A PRIM to a brick-resident 1024³ body ───────────────────────────
//
// The owner's crash sequence (2026-07-11, day after the 174aa17 un-gate): body at 1024³
// (sparse regime), a NEW cylinder prim added while editing (WorkMode::Vertex — the render regime
// stays Object/raster: the legacy `sdf_edit_active`/`force_volume_only` flags (now deleted) meant
// false and `gridbacked_only` stays TRUE; the 1024 body remains sparse-eligible throughout).
// The new prim's surface never appeared, the console panicked repeatedly, then the app crashed.
//
// This harness drives the PRODUCTION cache call sequence frame-by-frame at the cache level:
// trigger_bake → GPU classify encode (the shared production fn) → finalize → land through
// classify_tx (`submit_classified_request` + `poll_async_classifies`) → the request dispatched
// exactly as `dispatch_pending_primary_bakes` routes it (dense → Z-slab bake + readback +
// accept_gpu_result + advance_cascade; sparse Full → atlas ensure/upload_index + Z-slab mode-2
// bake + record_sparse_bake; sparse Partial → identity check + optional index re-upload +
// windowed mode-2 bake + record_sparse_partial_bake) → sparse DC mesh from the atlas
// (`dispatch_grid_dc_async`'s sparse-arm gate + `mesh_into_slot_sparse`).
//
// Documented gap vs the full app loop: no egui frame, no `update_resident_volume` (a
// brick-resident entry has an empty grid — that fn is a no-op for it in the Object regime), no
// `recalculate_body_origin` (test bodies keep origin ZERO — a legal production state), and the
// dense-arm dispatch replicates `app_poll_bake.rs`'s binding-size assert verbatim so a dense
// >512³ fall-through panics HERE exactly as it would in production.

/// One production-shaped dispatch of `req`. Returns the arm taken.
fn dispatch_request_production(
    gpu: &BakeGpu,
    sg: &SceneGraph,
    nid: NodeId,
    cache: &mut crate::body_volume_cache::BodyVolumeCache,
    atlas_opt: &mut Option<graphics::SdfBrickAtlasGpu>,
    mut req: crate::body_volume_cache::PrimaryBakeRequest,
) -> &'static str {
    use crate::body_volume_cache::SparseBakeRequest;
    let res = req.resolution[0];
    match req.sparse.take() {
        None => {
            // DENSE arm — replicate the production binding-size assert (app_poll_bake.rs, the
            // dense output allocation): a dense GridBacked bake above max_gridbacked_resolution
            // must have routed SparseBricks/TextureOnly upstream. This is the owner's panic
            // class if a 1024 request ever falls through dense.
            let grid_byte_len = req.resolution.iter().map(|&r| r as u64).product::<u64>() * 4;
            assert!(
                grid_byte_len <= gpu.device.limits().max_storage_buffer_binding_size as u64,
                "dense GridBacked bake at {:?} ({}MB) exceeds the storage-binding limit — \
                 a >{}³ request must route SparseBricks (sparse regime) or TextureOnly",
                req.resolution, grid_byte_len >> 20,
                crate::gpu_jobs::max_gridbacked_resolution(),
            );
            assert_eq!([res; 3], req.resolution, "dense cascade/coarse bakes are cubic");
            let grid = dispatch_bake(
                gpu, sg, nid, res, req.bounds_min, req.bounds_max, BakeOut::Dense, None);
            let ok = cache.accept_gpu_result(
                nid, grid, req.generation, req.source_hash,
                Some((req.bounds_min, req.bounds_max)));
            assert!(ok, "dense bake result rejected as stale");
            cache.advance_cascade(nid, sg); // production step 8 (poll_primary_bake)
            "dense"
        }
        Some(SparseBakeRequest::Full(si)) => {
            let index_needed = si.index.len() as u32;
            let pool_bricks = si.allocated as u32;
            let atlas = atlas_opt.get_or_insert_with(|| {
                graphics::SdfBrickAtlasGpu::new(&gpu.device, index_needed, pool_bricks)
            });
            atlas.ensure_capacity(&gpu.device, &gpu.queue, index_needed, pool_bricks);
            atlas.upload_index(&gpu.queue, &si.index, pool_bricks, si.dims_bricks, res, nid);
            let atlas_gen = atlas.generation;
            // Z-slab sliced exactly like dispatch_pending_primary_bakes (64 voxels per submit).
            let mut z0 = 0u32;
            while z0 < res {
                let z1 = (z0 + 64).min(res);
                dispatch_bake(gpu, sg, nid, res, req.bounds_min, req.bounds_max,
                    BakeOut::SparseAtlas { atlas, dims: si.dims_bricks },
                    Some(([0, 0, z0], [res, res, z1])));
                z0 = z1;
            }
            gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            let ok = cache.record_sparse_bake(
                nid, req.source_hash, req.resolution, req.generation, atlas_gen,
                (req.bounds_min, req.bounds_max), std::sync::Arc::new(si));
            assert!(ok, "sparse FULL bake result rejected as stale");
            "sparse_full"
        }
        Some(SparseBakeRequest::Partial { dims_bricks, atlas_generation, index_update }) => {
            let atlas = atlas_opt.as_mut()
                .expect("frozen-index partial dispatched without a live atlas");
            // Production dispatch-side identity check (generation counters, never timing).
            assert_eq!(atlas.body_nid, Some(nid), "partial: atlas body identity mismatch");
            assert_eq!(atlas.generation, atlas_generation, "partial: atlas generation mismatch");
            assert_eq!(atlas.res, res, "partial: atlas res mismatch");
            assert_eq!(atlas.dims_bricks, dims_bricks, "partial: atlas dims mismatch");
            let update = match index_update {
                Some(updated) => {
                    atlas.ensure_capacity(&gpu.device, &gpu.queue,
                        updated.index.len() as u32, updated.allocated as u32);
                    atlas.upload_index(&gpu.queue, &updated.index, updated.allocated as u32,
                        updated.dims_bricks, res, nid);
                    Some((updated, atlas.generation))
                }
                None => None,
            };
            // Windowed mode-2 bake over [bake_min, bake_max), Z-slabbed like production.
            let mut z0 = req.bake_min[2];
            while z0 < req.bake_max[2] {
                let z1 = (z0 + 64).min(req.bake_max[2]);
                dispatch_bake(gpu, sg, nid, res, req.bounds_min, req.bounds_max,
                    BakeOut::SparseAtlas { atlas, dims: dims_bricks },
                    Some(([req.bake_min[0], req.bake_min[1], z0],
                          [req.bake_max[0], req.bake_max[1], z1])));
                z0 = z1;
            }
            gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            let ok = cache.record_sparse_partial_bake(
                nid, req.source_hash, req.generation, atlas_generation, update);
            assert!(ok, "sparse PARTIAL bake result rejected");
            "sparse_partial"
        }
    }
}

/// One production "frame": trigger → GPU classify (encode + finalize + land) → dispatch every
/// request. Loops until the cache is quiescent (no dirty bodies, no pending work) or `max_iters`.
/// Returns the arms taken, in order.
fn pump_production_frames(
    gpu: &BakeGpu,
    classify_pipeline: &wgpu::ComputePipeline,
    dummy_tex: &wgpu::Texture,
    sg: &SceneGraph,
    nid: NodeId,
    cache: &mut crate::body_volume_cache::BodyVolumeCache,
    atlas_opt: &mut Option<graphics::SdfBrickAtlasGpu>,
    max_iters: usize,
) -> Vec<&'static str> {
    use crate::app_poll_bake::{encode_gpu_classify_dispatch, finalize_gpu_classify_verdicts};
    use crate::body_volume_cache::SparseBakeRequest;
    let mut arms = Vec::new();
    for _ in 0..max_iters {
        // One-shot events (add prim / cascade divert / recovery) all clear the debounce in
        // production; the harness has no wall-clock frames, so clear it before each trigger.
        cache.clear_bake_debounce();
        cache.trigger_bake(sg, None);
        // GPU classify: the SAME production encode + finalize + classify_tx landing.
        let queued = std::mem::take(&mut cache.pending_gpu_classify);
        for q in queued {
            let pending = encode_gpu_classify_dispatch(
                &gpu.device, &gpu.queue, classify_pipeline, &gpu.bgl, dummy_tex,
                q.request, crate::gpu_jobs::IDENTITY_MAT4,
                // CPU-finalize path deliberately: this pump exercises the verdict readback +
                // `sparse_index_from_verdicts` fallback e2e (the degraded set / parity oracle).
                None,
            );
            gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
            assert_eq!(pending.map_ready.peek(), crate::map_state::OK,
                "classify verdict readback must map OK");
            let raw: Vec<u32> = {
                let view = pending.staging.slice(..).get_mapped_range();
                bytemuck::cast_slice(&view).to_vec()
            };
            pending.staging.unmap();
            let si = finalize_gpu_classify_verdicts(&raw, pending.dims_bricks)
                .expect("no corrupt verdicts on a healthy adapter");
            let mut request = pending.request;
            request.sparse = Some(SparseBakeRequest::Full(si));
            cache.submit_classified_request(request);
        }
        // WINDOWED RECLASSIFY (2026-07-16): a midsize oversized window spawns a CPU window-walk
        // worker instead of a GPU full classify — pump its landing too (no-op when none).
        cache.wait_async_classifies(std::time::Duration::from_secs(60));
        cache.poll_async_classifies();
        let reqs = std::mem::take(&mut cache.primary_bake_requests);
        if reqs.is_empty() && cache.dirty_bodies.is_empty() {
            break;
        }
        for req in reqs {
            arms.push(dispatch_request_production(gpu, sg, nid, cache, atlas_opt, req));
        }
    }
    arms
}

/// Sparse DC mesh via the production gate (`dispatch_grid_dc_async`'s sparse arm: entry marker
/// == atlas generation, atlas res == entry res) into the persistent slot. Returns index_count.
fn mesh_resident_production(
    gpu: &BakeGpu,
    cache: &crate::body_volume_cache::BodyVolumeCache,
    nid: NodeId,
    atlas: &graphics::SdfBrickAtlasGpu,
    slot: &mut crate::sdf_gpu_mesh::SdfBodyMeshSlot,
    mesher: &crate::sdf_gpu_mesh::SdfMeshGpu,
) -> u32 {
    let (grid, nverts, gmin, gmax, _) = cache.body_grid(nid).expect("entry exists");
    assert!(grid.is_empty(), "brick-resident entry must hold no dense grid");
    let bgen = cache.brick_resident_gen(nid).expect("entry must be brick-resident");
    // The production sparse-arm gate (dispatch_grid_dc_async / sdf_populate_resident).
    assert_eq!(atlas.body_nid, Some(nid), "mesh gate: atlas body");
    assert_eq!(atlas.res, nverts, "mesh gate: atlas res vs entry res");
    assert_eq!(atlas.generation, bgen, "mesh gate: atlas generation vs brick_resident marker");
    // Slot ensure/grow — sdf_populate_resident's exact policy (grow-only, res relabel).
    if slot.capacity_res < nverts {
        *slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, nverts);
    } else {
        slot.res = nverts;
    }
    mesher.mesh_into_slot_sparse(&gpu.device, &gpu.queue, slot, atlas, gmin, gmax, nid, None);
    // Read back the DrawIndexedIndirect args (GPU Safety rule: capture the map Result).
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("indirect_stg"), size: 20,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.indirect, 0, &staging, 0, 20);
    gpu.queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map_async callback dropped").expect("indirect readback map failed");
    let args: Vec<u32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    assert_eq!(args[0] % 3, 0, "index_count must be whole triangles");
    args[0]
}

/// OWNER-CRASH GATE: add a NEW prim (cylinder) to a brick-resident 1024³ body and run the
/// production cache sequence to a fresh mesh — then grow the prim across several "drag frames"
/// (the create-tool shape drag: `mark_prim_dirty` regions → frozen-index partial / escape-full
/// machinery at 1024). Every request a sparse-eligible 1024 body emits must dispatch SPARSE —
/// the dense arm's replicated binding-size assert fails the test exactly where production
/// panicked if a 1024 request ever falls through dense.
#[test]
fn sparse_1024_add_prim_to_resident_body_end_to_end() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[Sparse1024AddPrim] no GPU adapter — skipping"); return; }
    };
    // classify_bricks pipeline + dummy texture — same construction as the 1024 e2e gate.
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });

    let res = 1024u32;
    // The owner scene shape: sphere + subtract (same as the un-gate e2e).
    let prims = vec![
        sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ];
    let (mut sg, nid) = make_body(prims, 0, Vec3::ZERO);

    // Object-regime cache with the Grid picker set to 1024 — the production flag state during
    // the owner's session (the legacy force_volume_only flag is deleted → gridbacked_only
    // stays TRUE even in "SDF Edit" / WorkMode::Vertex).
    let mut cache = crate::body_volume_cache::BodyVolumeCache::new(256, 2.0);
    cache.gridbacked_only = true; // S2.6: the only sparse gate (with res >= 512)
    cache.gpu_classify_ready = true;
    cache.set_body_resolution(nid, res);

    let mut atlas_opt: Option<graphics::SdfBrickAtlasGpu> = None;
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);

    // Phase 1 — first bake at 1024: production first-bake shape is the 64³ dense coarse cascade,
    // whose ≥512 promotion DIVERTS to the sparse full (advance_cascade). Pump to quiescence.
    let arms = pump_production_frames(
        &gpu, &classify_pipeline, &dummy_tex, &sg, nid, &mut cache, &mut atlas_opt, 8);
    eprintln!("[Sparse1024AddPrim] phase1 (first bake) arms={arms:?}");
    assert_eq!(arms.first().copied(), Some("dense"),
        "first bake must be the 64³ dense coarse cascade level");
    assert_eq!(arms.last().copied(), Some("sparse_full"),
        "the 1024 promotion must divert to a sparse FULL bake");
    let atlas = atlas_opt.as_ref().expect("sparse bake created the persistent atlas");
    assert_eq!(atlas.res, res);
    let idx0 = mesh_resident_production(&gpu, &cache, nid, atlas, &mut slot, &mesher);
    assert!(idx0 > 0, "baseline 1024 sparse mesh produced zero indices");

    // Phase 2 — THE OWNER ACTION: add a NEW cylinder prim. Production
    // (`add_prim_to_graph`): graph push → mark_node_body_vol_dirty (mark_body_dirty — full
    // dirty, regions cleared) → clear_bake_debounce.
    let mut cyl = SdfPrimitive::with_uid(
        "Cylinder.001".into(),
        SdfShape::Cylinder { radius: 0.25, half_height: 0.45 },
        Vec3::new(1.1, 0.0, 0.0),
        9001,
    );
    cyl.operation = BooleanOp::Union { k: 0.0 };
    let (old_min, old_max) = {
        let (_, _, gmin, gmax, _) = cache.body_grid(nid).unwrap();
        (gmin, gmax)
    };
    sg.body_primitives_mut(nid).unwrap().push(cyl);
    cache.mark_body_dirty(nid);
    cache.clear_bake_debounce();

    let arms = pump_production_frames(
        &gpu, &classify_pipeline, &dummy_tex, &sg, nid, &mut cache, &mut atlas_opt, 8);
    eprintln!("[Sparse1024AddPrim] phase2 (add cylinder) arms={arms:?}");
    assert!(arms.contains(&"sparse_full"),
        "adding a prim to a brick-resident 1024 body must re-bake SPARSE FULL (got {arms:?})");
    assert!(!arms.contains(&"dense"),
        "a sparse-eligible 1024 body dispatched a DENSE request — the owner's crash class");
    let (new_min, new_max) = {
        let (_, _, gmin, gmax, _) = cache.body_grid(nid).unwrap();
        (gmin, gmax)
    };
    assert!((new_min - old_min).length() > 1e-6 || (new_max - old_max).length() > 1e-6,
        "the added cylinder must change the bake bounds (premise: bounds-changing add)");
    let atlas = atlas_opt.as_ref().unwrap();
    let idx1 = mesh_resident_production(&gpu, &cache, nid, atlas, &mut slot, &mesher);
    assert!(idx1 > 0, "post-add-prim 1024 sparse mesh produced zero indices");

    // Phase 3 — the create-tool DRAG: grow the cylinder over several frames. Each frame is a
    // prim-level edit (`mark_prim_dirty` → dirty region) — the frozen-index PARTIAL /
    // escape-full machinery at 1024.
    let mut last_arms: Vec<&'static str> = Vec::new();
    for (i, hh) in [0.6f32, 0.8, 1.1].into_iter().enumerate() {
        {
            let prims = sg.body_primitives_mut(nid).unwrap();
            let p = prims.iter_mut().find(|p| p.prim_uid == 9001).unwrap();
            p.shape = SdfShape::Cylinder { radius: 0.25 + 0.05 * (i as f32 + 1.0), half_height: hh };
        }
        cache.mark_prim_dirty(nid, 9001, &sg);
        let arms = pump_production_frames(
            &gpu, &classify_pipeline, &dummy_tex, &sg, nid, &mut cache, &mut atlas_opt, 8);
        eprintln!("[Sparse1024AddPrim] phase3 drag frame {i} (hh={hh}) arms={arms:?}");
        assert!(!arms.contains(&"dense"),
            "drag frame {i}: sparse-eligible 1024 body dispatched a DENSE request");
        assert!(!arms.is_empty(), "drag frame {i}: the edit dispatched nothing");
        last_arms = arms;
    }
    let atlas = atlas_opt.as_ref().unwrap();
    let idx2 = mesh_resident_production(&gpu, &cache, nid, atlas, &mut slot, &mesher);
    assert!(idx2 > 0, "post-drag 1024 sparse mesh produced zero indices");
    eprintln!(
        "[Sparse1024AddPrim] OK: idx baseline={idx0} +cylinder={idx1} post-drag={idx2} \
         (final drag arms={last_arms:?}; atlas gen={} pool caps={} bricks)",
        atlas.generation, atlas.pool_capacity_bricks,
    );
}

/// OWNER REPRO 2026-07-14 ("lost surface visibility when I drag a primitive inside another to
/// create sub-body"): a brick-resident SPARSE body whose prims are ALL moved into a NEW child
/// sub-body (the outliner drop-onto-prim Group gesture, `group_selected_into_body`'s exact graph
/// mutation) must (a) change the body source hash, (b) re-bake SPARSE FULL — never dense —,
/// (c) re-mesh to a NON-EMPTY surface, and (d) settle QUIESCENT: no dirty bodies, no pending
/// retighten, no cpu_grid_stale, and further trigger/settle frames dispatch NOTHING (the
/// `[RetightenArm] by=no_copy_completion while IDLE` loop class).
/// Runs at 512 — the same sparse regime as the owner's 1024 (eligibility keys on >=512; the
/// 1024 e2e above already pins that 1024 runs exactly like 512).
#[test]
fn sparse_group_into_subbody_end_to_end() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SparseGroup] no GPU adapter — skipping"); return; }
    };
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });

    let res = 512u32;
    // The owner scene shape: two overlapping boxes (Box.005 + Box.006), unique uids — the
    // child_order machinery keys prims by uid.
    let mut b1 = box_op(Vec3::new(-0.25, 0.0, 0.0), Vec3::splat(0.4), BooleanOp::Union { k: 0.0 });
    b1.prim_uid = 5;
    let mut b2 = box_op(Vec3::new(0.25, 0.1, 0.0), Vec3::splat(0.35), BooleanOp::Union { k: 0.0 });
    b2.prim_uid = 6;
    let (mut sg, nid) = make_body(vec![b1, b2], 0, Vec3::ZERO);
    sg.ensure_body_child_order(nid);

    let mut cache = crate::body_volume_cache::BodyVolumeCache::new(256, 2.0);
    cache.gridbacked_only = true; // S2.6: the only sparse gate (with res >= 512)
    cache.gpu_classify_ready = true;
    cache.set_body_resolution(nid, res);

    let mut atlas_opt: Option<graphics::SdfBrickAtlasGpu> = None;
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);

    // Phase 1 — first bake to a brick-resident entry + non-empty mesh.
    let arms = pump_production_frames(
        &gpu, &classify_pipeline, &dummy_tex, &sg, nid, &mut cache, &mut atlas_opt, 8);
    eprintln!("[SparseGroup] phase1 (first bake) arms={arms:?}");
    assert_eq!(arms.last().copied(), Some("sparse_full"), "first bake must end brick-resident");
    let h_pre = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;
    assert_eq!(cache.body_grid(nid).unwrap().4, h_pre, "entry stamped with the pre-group hash");
    let idx0 = mesh_resident_production(
        &gpu, &cache, nid, atlas_opt.as_ref().unwrap(), &mut slot, &mesher);
    assert!(idx0 > 0, "baseline sparse mesh produced zero indices");

    // Phase 2 — THE GESTURE: `group_selected_into_body`'s exact graph mutation (outliner
    // drop-onto-prim): extract BOTH prims from the top body, create a child sub-body holding
    // them (group inherits the drag target's op = Union; target reset to Union), rebuild
    // child_order, then the production dirty marks (mark_node_body_vol_dirty climbs to the
    // top-level ancestor for BOTH bodies → ONE mark_body_dirty(top); structural events clear
    // the debounce).
    let extracted: Vec<SdfPrimitive> = sg.body_primitives_mut(nid).unwrap().drain(..).collect();
    assert_eq!(extracted.len(), 2);
    let sub = sg.add_child(nid, NodePayload::SdfBody {
        name: "Group.001".into(), primitives: extracted, mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
        bevel_radius: 0.0, blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(), child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
    sg.ensure_body_child_order(sub);
    sg.ensure_body_child_order(nid); // drops the now-dangling prim refs
    sg.insert_child_order(nid, core_math::scene_graph::BodyChildRef::Body(sub), 0);

    let h_post = crate::bake_utils::body_content_id(&sg, nid, 0).unwrap().0;
    assert_ne!(h_pre, h_post,
        "grouping into a sub-body must change the top body's source hash (child_order + Body ref)");

    cache.mark_body_dirty(nid);
    cache.clear_bake_debounce();

    let arms = pump_production_frames(
        &gpu, &classify_pipeline, &dummy_tex, &sg, nid, &mut cache, &mut atlas_opt, 8);
    eprintln!("[SparseGroup] phase2 (group into sub-body) arms={arms:?}");
    assert!(arms.contains(&"sparse_full"),
        "group-into-sub-body must re-bake SPARSE FULL (got {arms:?})");
    assert!(!arms.contains(&"dense"),
        "a sparse-eligible body dispatched a DENSE request after grouping");

    // (c) surface must NOT disappear: entry brick-resident at the NEW hash, mesh non-empty.
    let entry = cache.body_grid(nid).expect("entry must survive the group");
    assert_eq!(entry.4, h_post, "entry must be stamped with the POST-group hash");
    assert!(cache.brick_resident_gen(nid).is_some(), "entry must stay brick-resident");
    let idx1 = mesh_resident_production(
        &gpu, &cache, nid, atlas_opt.as_ref().unwrap(), &mut slot, &mesher);
    assert!(idx1 > 0,
        "surface DISAPPEARED after group-into-sub-body (zero indices) — the owner bug");

    // (d) QUIESCENCE — the settle loop class ([RetightenArm] by=no_copy_completion while IDLE):
    // after the structural re-bake landed, no dirty state may remain and further frames
    // (trigger + idle settle) must dispatch NOTHING.
    assert!(cache.dirty_bodies.is_empty(), "dirty_bodies must be empty after the re-bake landed");
    assert!(cache.settle_quiescent(),
        "settle machinery re-armed (pending_retighten / cpu_grid_stale / cpu_heal_queue) after \
         the structural re-bake — the owner's [RetightenArm] loop feeder");
    for frame in 0..3 {
        cache.clear_bake_debounce();
        let _ = cache.trigger_bake(&sg, None);
        let queued = cache.check_idle_retighten(&sg);
        assert!(!queued, "settle frame {frame}: idle retighten re-queued a bake — settle loop");
        assert!(cache.primary_bake_requests.is_empty() && cache.pending_gpu_classify.is_empty(),
            "settle frame {frame}: an idle frame dispatched work — settle loop");
    }
    eprintln!("[SparseGroup] OK: idx baseline={idx0} post-group={idx1}, quiescent after settle");
}


// ── R2 MEASUREMENT HARNESS (CACHE_REMESH_512_REVIEW §R2 prereq diagnostic) ──────────────────────
//
// Question under test: is the full-window sparse DC re-mesh (window = surface-brick bbox ≈ whole
// grid) still the 15-40ms/drag-frame GPU term the review estimated, or did R3c's brick-local
// cell_vert + NO_SLOT early-return make the mostly-sentinel full-window dispatch cheap?
//
// #[ignore]d: a diagnostic, not a regression gate — run explicitly with
//   cargo test -p app --bins r2_measure -- --ignored --nocapture --test-threads=1
//
// Two scenes at 512³:
//   A "shell":      one big sphere (r≈0.9×half-extent) + a subtract — the window (surface bbox)
//                   spans nearly the WHOLE grid while allocated bricks are only the thin shell.
//                   This is the exact R2 degeneration shape: threads = window volume, not surface.
//   B "degenerate": a 4×4×4 union grid of spheres — surface bricks throughout the volume
//                   (approaches the review's 42%-surface worst case).
//
// Per scene: production classify (GPU, the shared encode) → atlas upload → z-slab mode-2 bake →
// then SIX consecutive mesh_into_slot_sparse dispatches with poll(Wait) around each — the
// steady-state iterations (2..) are the per-drag-frame re-mesh cost (wall≈GPU when waited).
// The new [DcResidentSparse] GPU vert/quads timestamps print via their map_async callback too.
#[test]
#[ignore = "R2 measurement harness — run explicitly with --ignored --nocapture"]
fn r2_measure_full_window_remesh() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[R2Measure] no GPU adapter — skipping"); return; }
    };
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);

    let scene_a: Vec<SdfPrimitive> = vec![
        sphere_op(Vec3::ZERO, 1.30, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(1.1, 0.0, 0.0), 0.6, BooleanOp::Subtract { k: 0.1 }),
    ];
    let mut scene_b: Vec<SdfPrimitive> = Vec::new();
    for x in 0..4 { for y in 0..4 { for z in 0..4 {
        let c = (Vec3::new(x as f32, y as f32, z as f32) - Vec3::splat(1.5)) * 0.72;
        scene_b.push(sphere_op(c, 0.30, BooleanOp::Union { k: 0.05 }));
    }}}

    for (name, prims) in [("A-shell", scene_a), ("B-degenerate", scene_b)] {
        r2_measure_scene(&gpu, &classify_pipeline, &finalize, name, prims, 512);
    }
}

/// C2 measurement — the headline numbers: incremental (64³-window) re-mesh vs full tiled vs
/// monolithic, at 512 AND 1024 (the owner's 10-24ms GPU term lives at 1024). Shares
/// `r2_measure_scene` (its [C2Measure] tail line is the deliverable metric).
#[test]
#[ignore = "C2 measurement harness — run explicitly with --ignored --nocapture"]
fn c2_measure_incremental() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[C2Measure] no GPU adapter — skipping"); return; }
    };
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);
    let scene_a: Vec<SdfPrimitive> = vec![
        sphere_op(Vec3::ZERO, 1.30, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(1.1, 0.0, 0.0), 0.6, BooleanOp::Subtract { k: 0.1 }),
    ];
    r2_measure_scene(&gpu, &classify_pipeline, &finalize, "A-shell", scene_a, 512);
    // 1024 at the FULL scene-A — the C1 envelope finding (Σ per-tile caps at 48 v/brick = 41.6M
    // vert caps > the 1 GiB ceiling → monolithic fallback) was fixed by the per-tile headroom
    // split (TILE_VERTS_PER_BRICK_HEADROOM = 28 → ~24.3M caps, 777MB — fits; the
    // tile_headroom_fits_measured_1024_envelope gate pins it). The [DcResidentSparse] "tiled"
    // lines in this run ARE the proof the tiled path engaged at 1024 on a realistic surface.
    let scene_a_1024: Vec<SdfPrimitive> = vec![
        sphere_op(Vec3::ZERO, 1.30, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(1.1, 0.0, 0.0), 0.6, BooleanOp::Subtract { k: 0.1 }),
    ];
    r2_measure_scene(&gpu, &classify_pipeline, &finalize, "A-shell", scene_a_1024, 1024);
}

fn r2_measure_scene(
    gpu: &BakeGpu,
    classify_pipeline: &wgpu::ComputePipeline,
    finalize: &crate::gpu_ctx::ClassifyFinalizePipelines,
    name: &str,
    prims: Vec<SdfPrimitive>,
    res: u32,
) {
    use crate::app_poll_bake::{decode_gpu_finalized_index, encode_gpu_classify_dispatch};
    use crate::body_volume_cache::bake_dispatch::rigid_translate_tests::sparse_cache;
    use std::time::Instant;

    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let (sg, nid) = make_body(prims, 0, Vec3::ZERO);

    // Production request side (the Grid-picker path, exactly as sparse_run_at drives it).
    let mut cache = sparse_cache(nid, (gmin, gmax));
    cache.gpu_classify_ready = true;
    cache.set_body_resolution(nid, res);
    cache.clear_bake_debounce();
    let t_trig = Instant::now();
    cache.trigger_bake(&sg, None);
    let trigger_ms = t_trig.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(cache.pending_gpu_classify.len(), 1);
    let q = cache.pending_gpu_classify.pop().unwrap();
    assert_eq!(q.request.resolution, [res; 3]);

    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });
    let t = Instant::now();
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, classify_pipeline, &gpu.bgl, &dummy_tex,
        q.request, crate::gpu_jobs::IDENTITY_MAT4, Some(finalize));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let classify_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(pending.map_ready.peek(), crate::map_state::OK);
    let raw: Vec<u32> = {
        let view = pending.staging.slice(..).get_mapped_range();
        bytemuck::cast_slice(&view).to_vec()
    };
    pending.staging.unmap();
    let si = decode_gpu_finalized_index(&raw, pending.dims_bricks)
        .expect("GPU-finalized index failed validation");
    assert!(si.allocated > 0, "scene {name} allocated nothing");

    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, si.index.len() as u32, si.allocated as u32);
    atlas.ensure_capacity(&gpu.device, &gpu.queue, si.index.len() as u32, si.allocated as u32);
    atlas.upload_index(&gpu.queue, &si.index, si.allocated as u32, si.dims_bricks, res, nid);
    let t = Instant::now();
    let mut z0 = 0u32;
    while z0 < res {
        let z1 = (z0 + 64).min(res);
        dispatch_bake(gpu, &sg, nid, res, gmin, gmax,
            BakeOut::SparseAtlas { atlas: &atlas, dims: si.dims_bricks },
            Some(([0, 0, z0], [res, res, z1])));
        z0 = z1;
    }
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let bake_ms = t.elapsed().as_secs_f64() * 1000.0;

    // THE MEASUREMENT: six consecutive full re-mesh dispatches (what every landed partial bake
    // triggers during a drag). Iteration 0 pays one-time allocation (ensure_output_caps); the
    // steady iterations are the R2 per-drag-frame term.
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut remesh_ms = [0.0f64; 6];
    for slot_ms in remesh_ms.iter_mut() {
        let t = Instant::now();
        mesher.mesh_into_slot_sparse(&gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, None);
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        *slot_ms = t.elapsed().as_secs_f64() * 1000.0;
    }
    let steady = remesh_ms[1..].iter().sum::<f64>() / (remesh_ms.len() - 1) as f64;

    // C1: the TILED production path — same atlas, same six-dispatch shape, into the shared pool.
    let mut pool = crate::sdf_gpu_mesh::SdfMeshPool::new(&gpu.device, false);
    let mut slot_t = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mut tiled_ms = [0.0f64; 6];
    for slot_ms in tiled_ms.iter_mut() {
        let t = Instant::now();
        assert!(mesher.mesh_into_slot_sparse_tiled(
            &gpu.device, &gpu.queue, &mut slot_t, &atlas, gmin, gmax, nid, &mut pool, None));
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let _ = slot_t.poll_tile_overflow(); // consume the readback so each iteration re-issues
        *slot_ms = t.elapsed().as_secs_f64() * 1000.0;
    }
    let steady_tiled = tiled_ms[1..].iter().sum::<f64>() / (tiled_ms.len() - 1) as f64;
    let n_tiles = pool.slot(nid).map_or(0, |s| s.tiles.len());

    // C2: INCREMENTAL scenario — re-bake a ~64³-cell window straddling the surface into the
    // SAME atlas, then measure the dirty-window re-mesh vs the full tiled re-mesh above. The
    // window sits on the +X surface of the shell (world x ≈ prim radius) so it overlaps real
    // tiles, not empty interior.
    let h = 32u32;
    // Center the window on the surface's +X extreme (from the atlas's handle-brick bbox) so it
    // straddles real surface tiles for ANY scene/res, not empty interior/exterior.
    let cx = ((atlas.surface_brick_max[0] + 1) * 8).min(res).saturating_sub(4);
    let cc = res / 2;
    let win_min = [cx.saturating_sub(h), cc.saturating_sub(h), cc.saturating_sub(h)];
    let win_max = [(cx + h).min(res), (cc + h).min(res), (cc + h).min(res)];
    let t = Instant::now();
    dispatch_bake(gpu, &sg, nid, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: si.dims_bricks },
        Some((win_min, win_max)));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let rebake_ms = t.elapsed().as_secs_f64() * 1000.0;
    let dirty = crate::sdf_gpu_mesh::SdfTileDirty::Voxels(win_min, win_max);
    let mut inc_ms = [0.0f64; 6];
    for slot_ms in inc_ms.iter_mut() {
        let t = Instant::now();
        assert!(mesher.mesh_into_slot_sparse_tiled(
            &gpu.device, &gpu.queue, &mut slot_t, &atlas, gmin, gmax, nid, &mut pool, Some(&dirty)));
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let _ = slot_t.poll_tile_overflow(); // consume so each iteration re-issues the readback
        *slot_ms = t.elapsed().as_secs_f64() * 1000.0;
    }
    let steady_inc = inc_ms[1..].iter().sum::<f64>() / (inc_ms.len() - 1) as f64;
    let (nd, nt) = slot_t.last_dispatched_tiles;
    eprintln!(
        "[C2Measure] scene={name} res={res} window=64³ rebake={rebake_ms:.1}ms | \
         INCREMENTAL tiles={nd}/{nt} steady={steady_inc:.2}ms (all: {:?}) | \
         vs tiled-full steady={steady_tiled:.2}ms | vs monolithic steady={steady:.2}ms",
        inc_ms.iter().map(|m| (m * 100.0).round() / 100.0).collect::<Vec<_>>(),
    );

    // Window vs grid: the dispatch covers the surface-brick bbox in CELLS (×8 per brick axis).
    let (bmin, bmax) = (atlas.surface_brick_min, atlas.surface_brick_max);
    let ext: Vec<u32> = (0..3).map(|a| ((bmax[a] + 1 - bmin[a]) * 8).min(res - 1)).collect();
    let window_cells = ext.iter().map(|&e| e as u64).product::<u64>();
    let grid_cells = ((res - 1) as u64).pow(3);
    let frac = si.allocated as f64 / si.index.len() as f64 * 100.0;

    eprintln!(
        "[R2Measure] scene={name} res={res} bricks={}/{} ({frac:.1}% surface) | \
         window={}x{}x{} cells ({:.0}% of grid, {:.0}M threads/pass) | \
         remesh first={:.1}ms steady={steady:.2}ms (all: {:?}) | \
         TILED tiles={n_tiles} first={:.1}ms steady={steady_tiled:.2}ms (all: {:?}) | \
         setup: trigger_cpu={trigger_ms:.2}ms classify={classify_ms:.1}ms bake={bake_ms:.0}ms",
        si.allocated, si.index.len(),
        ext[0], ext[1], ext[2],
        window_cells as f64 / grid_cells as f64 * 100.0,
        window_cells as f64 / 1.0e6,
        remesh_ms[0],
        remesh_ms.iter().map(|m| (m * 100.0).round() / 100.0).collect::<Vec<_>>(),
        tiled_ms[0],
        tiled_ms.iter().map(|m| (m * 100.0).round() / 100.0).collect::<Vec<_>>(),
    );
}

// ── Slice (d) DIAGNOSTIC: the 490k transient brick balloon (2026-07-16 owner log) ───────────────
//
// During a scale gesture (duplicate-in-place → Subtract → scale), successive escalated FULL
// classifies at 1024³ reported allocated bricks 164k → 330k → 489,984 → settle 195k. Each growth
// forced an SdfMeshPool REBUILD up to verts 1073MB + idx 846MB (the 52-69ms [SLOWPOLL] spikes),
// and the grow-only watermark pins that VRAM afterwards.
//
// Question: is 490k the TRUE surface-brick count of some transient boolean configuration (two
// near-coincident shells → geometry truth → fix = pool damping), or classifier over-allocation
// (band/interior-proof defect → fix the classifier)?
//
// Sweep the subtract radius through the gesture's keyframes at 1024 with FIXED bounds and print
// allocated bricks per step. CPU classify — bit-identical to the GPU path (parity-gated).
#[test]
#[ignore = "slice-d diagnostic — run explicitly with --ignored --nocapture"]
fn d_sweep_subtract_scale_brick_counts() {
    use core_math::chunked_dc::classify_prims_to_sparse_index;
    use crate::body_volume_cache::bake_dispatch::{sparse_classify_band, SPARSE_BRICK_DIM};

    let res = 1024u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = sparse_classify_band(gmin, gmax, res);
    let total = {
        let bpa = ((res + SPARSE_BRICK_DIM - 1) / SPARSE_BRICK_DIM) as usize;
        bpa * bpa * bpa
    };
    // The owner's gesture: base sphere + a duplicate scaled through it as a Subtract (k=0).
    for r_sub in [0.4f32, 0.6, 0.75, 0.79, 0.80, 0.81, 0.85, 1.0, 1.2] {
        let prims = vec![
            sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
            sphere_op(Vec3::ZERO, r_sub, BooleanOp::Subtract { k: 0.0 }),
        ];
        let t = std::time::Instant::now();
        let si = classify_prims_to_sparse_index(
            &prims, 0, Vec3::ZERO, gmin, gmax, res, SPARSE_BRICK_DIM, band);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "[DSweep] r_sub={r_sub:.2} allocated={:>8} ({:.1}% of {total}) pool={:.0}MB cpu={ms:.0}ms",
            si.allocated, si.allocated as f64 / total as f64 * 100.0,
            si.allocated as f64 * 2048.0 / (1024.0 * 1024.0));
    }

    // HYPOTHESIS 2: the gesture scaled the prim via `prim.scale` (gizmo S), and a scaled prim's
    // SDF is evaluated conservatively (Lipschitz correction underestimates true distance) → the
    // |d| < band test captures far more bricks than the true surface shell. Sweep scale shapes.
    for (label, scale) in [
        ("uniform 1.0", Vec3::ONE),
        ("uniform 1.5", Vec3::splat(1.5)),
        ("uniform 2.0", Vec3::splat(2.0)),
        ("nonuni 2,1,1", Vec3::new(2.0, 1.0, 1.0)),
        ("nonuni 2,2,0.5", Vec3::new(2.0, 2.0, 0.5)),
    ] {
        let mut sub = sphere_op(Vec3::ZERO, 0.6, BooleanOp::Subtract { k: 0.0 });
        sub.scale = scale;
        let prims = vec![sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }), sub];
        let si = classify_prims_to_sparse_index(
            &prims, 0, Vec3::ZERO, gmin, gmax, res, SPARSE_BRICK_DIM, band);
        eprintln!(
            "[DSweep] scale {label:>14}: allocated={:>8} ({:.1}%) pool={:.0}MB",
            si.allocated, si.allocated as f64 / total as f64 * 100.0,
            si.allocated as f64 * 2048.0 / (1024.0 * 1024.0));
    }
}

// ── Slice D MEASUREMENT: attribute the landed-full frame-thread CPU at 1024 (owner log 7) ──────
//
// The owner's three [SPIKE] terms on every LANDED escalated-full at 1024³ (~160k bricks):
//   1. sparse atlas create/grow/upload ≈ 54ms   (inside dispatch_pending_primary_bakes)
//   2. dispatch_pending_primary_bakes ≈ 57ms    (SUPERSET of 1 — same fn, nested timer)
//   3. sdf_populate_resident mesh_dispatch ≈ 52-61ms (full tiled re-mesh)
//
// This harness replays the gesture shape (growing subtract → growing brick counts) against ONE
// persistent atlas + pool + slot — the production state shape — and times each phase per landing.
// The split instrumentation added in slice D ([SPIKE] atlas split, [DcResidentSparse] tiled split)
// prints alongside. NOTE: dev profile builds workspace crates at opt-level 0 (Cargo.toml
// [profile.dev]), the same environment the owner's live numbers come from — CPU scan costs here
// are directly comparable to the owner log.
#[test]
#[ignore = "slice-D measurement harness — run explicitly with --ignored --nocapture"]
fn d_measure_landed_full_1024() {
    use crate::body_volume_cache::bake_dispatch::{sparse_classify_band, SPARSE_BRICK_DIM};
    use std::time::Instant;

    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[DMeasure] no GPU adapter — skipping"); return; }
    };
    let res = 1024u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = sparse_classify_band(gmin, gmax, res);
    let nid = NodeId(901);

    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut pool = crate::sdf_gpu_mesh::SdfMeshPool::new(&gpu.device, false);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mut atlas: Option<graphics::SdfBrickAtlasGpu> = None;

    // The owner's gesture: base sphere + a duplicate scaled through it as Subtract — brick count
    // grows step to step (164k → 330k class), forcing atlas grow + pool region churn like live.
    for (step, r_sub) in [0.5f32, 0.65, 0.8, 0.95, 1.1].into_iter().enumerate() {
        let prims = vec![
            sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
            sphere_op(Vec3::ZERO, r_sub, BooleanOp::Subtract { k: 0.0 }),
        ];
        let t = Instant::now();
        let si = classify_prims_to_sparse_index(
            &prims, 0, Vec3::ZERO, gmin, gmax, res, SPARSE_BRICK_DIM, band);
        let classify_ms = t.elapsed().as_secs_f64() * 1000.0;
        if si.allocated == 0 {
            eprintln!("[DMeasure] step={step} r_sub={r_sub:.2}: surface vanished (subtract swallowed the body) — skipping");
            continue;
        }

        // ── Phase 1: the [SPIKE] atlas window (create / ensure / upload_index split) ─────────
        let index_needed = si.index.len() as u32;
        let pool_bricks = si.allocated as u32;
        let t = Instant::now();
        let a = atlas.get_or_insert_with(|| {
            graphics::SdfBrickAtlasGpu::new(&gpu.device, index_needed, pool_bricks)
        });
        let create_ms = t.elapsed().as_secs_f64() * 1000.0;
        let gen0 = a.generation;
        let t = Instant::now();
        a.ensure_capacity(&gpu.device, &gpu.queue, index_needed, pool_bricks);
        let ensure_ms = t.elapsed().as_secs_f64() * 1000.0;
        let grew = a.generation != gen0;
        let t = Instant::now();
        let split = a.upload_index(&gpu.queue, &si.index, pool_bricks, si.dims_bricks, res, nid);
        let upload_ms = t.elapsed().as_secs_f64() * 1000.0;
        let atlas_total = create_ms + ensure_ms + upload_ms;

        // ── Phase 2: the mode-2 bake, z-slab dispatches like production ──────────────────────
        let (sg, body) = make_body(prims, 0, Vec3::ZERO);
        let t = Instant::now();
        let mut z0 = 0u32;
        while z0 < res {
            let z1 = (z0 + 64).min(res);
            dispatch_bake(&gpu, &sg, body, res, gmin, gmax,
                BakeOut::SparseAtlas { atlas: a, dims: si.dims_bricks },
                Some(([0, 0, z0], [res, res, z1])));
            z0 = z1;
        }
        let bake_encode_ms = t.elapsed().as_secs_f64() * 1000.0; // CPU side (encode+submit)
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let bake_wall_ms = t.elapsed().as_secs_f64() * 1000.0;

        // ── Phase 3: the FULL tiled re-mesh (what every landed full triggers) ────────────────
        let t = Instant::now();
        let tiled_ok = mesher.mesh_into_slot_sparse_tiled(
            &gpu.device, &gpu.queue, &mut slot, a, gmin, gmax, nid, &mut pool, None);
        let mesh_cpu_ms = t.elapsed().as_secs_f64() * 1000.0;
        gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let mesh_wall_ms = t.elapsed().as_secs_f64() * 1000.0;
        let _ = slot.poll_tile_overflow();

        eprintln!(
            "[DMeasure] step={step} r_sub={r_sub:.2} bricks={} ({:.0}MB pool) classify_cpu={classify_ms:.0}ms | \
             ATLAS total={atlas_total:.1}ms (create={create_ms:.1} ensure={ensure_ms:.1} grew={grew} \
             write={:.1} bbox={:.1} tiles={:.1}) | BAKE cpu={bake_encode_ms:.1}ms wall={bake_wall_ms:.0}ms | \
             MESH tiled_ok={tiled_ok} cpu={mesh_cpu_ms:.1}ms wall={mesh_wall_ms:.1}ms tiles={:?}",
            si.allocated, si.allocated as f64 * 2048.0 / (1024.0 * 1024.0),
            split.write_ms, split.bbox_ms, split.tiles_ms,
            slot.last_dispatched_tiles);
    }
}

/// C2 GATE — the incremental dirty-window re-mesh is BIT-IDENTICAL to a from-scratch FULL tiled
/// re-mesh of the same atlas state, and it actually SKIPS clean tiles. Production shape
/// end-to-end: full sparse bake into the persistent atlas → full tiled mesh (stamps the
/// incremental provenance) → nudge the subtract prim → PARTIAL mode-2 re-bake of the dilated
/// dirty window into the SAME atlas pool (frozen index — the S2.5 dispatch) → incremental mesh
/// with that voxel window → compare against a fresh full tiled mesh of the same atlas:
/// per-tile args identical (occupancy unchanged ⇒ identical packing), Σidx identical, vertex
/// parity 0 both directions, clean tiles bit-preserved, dispatched < total gate-asserted.
#[test]
fn sdf_tiled_incremental_matches_full() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[SdfTiledInc] no GPU adapter — skipping"); return; }
    };
    use super::tests_dc::{parity, tiled_readback};
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    // Big shell + a small subtract we nudge: the surface spans many tiles, the edit is LOCAL.
    let (p0, r) = (Vec3::new(0.55, 0.0, 0.0), 0.35f32);
    let p1 = Vec3::new(0.55, 0.06, 0.0);
    let round1 = vec![
        sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
        sphere_op(p0, r, BooleanOp::Subtract { k: 0.0 }),
    ];
    let round2 = vec![
        sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
        sphere_op(p1, r, BooleanOp::Subtract { k: 0.0 }),
    ];
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    assert!(sp1.allocated > 0, "vacuous scene");
    let nid = NodeId(801);
    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, sp1.index.len() as u32, sp1.allocated as u32);
    atlas.upload_index(&gpu.queue, &sp1.index, sp1.allocated as u32, sp1.dims_bricks, res, nid);
    let (sg1, body1) = make_body(round1, 0, Vec3::ZERO);
    dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: sp1.dims_bricks }, None);

    // FULL tiled mesh — stamps the provenance the incremental path validates.
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut pool = crate::sdf_gpu_mesh::SdfMeshPool::new(&gpu.device, false);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(
        &gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None));
    let n_total = pool.slot(nid).unwrap().tiles.len() as u32;
    assert!(n_total > 8, "surface must span many tiles (got {n_total})");
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let _ = slot.poll_tile_overflow(); // consume so the incremental dispatch re-issues one
    let (_, _, verts_before, _) = tiled_readback(&gpu.device, &gpu.queue, &pool, nid);

    // Production-shaped dirty window: old ∪ new prim AABB + 2-voxel margin → voxel window →
    // one-brick dilation (the exact S2.5 recipe, sparse_partial_frozen_index precedent).
    let margin = Vec3::splat(2.0 * voxel);
    let wmin = (p0 - Vec3::splat(r)).min(p1 - Vec3::splat(r)) - margin;
    let wmax = (p0 + Vec3::splat(r)).max(p1 + Vec3::splat(r)) + margin;
    let (win_min, win_max) = world_window_to_voxel(wmin, wmax, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    assert!(sparse_window_bricks_all_handled(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max),
        "nudge escaped the frozen brick set — test premise broken (production would escalate)");

    // PARTIAL mode-2 re-bake of the window into the SAME atlas pool (index frozen, no re-upload
    // → tile_stats and atlas.generation unchanged — the incremental eligibility premise).
    let gen_before = atlas.generation;
    let (sg2, body2) = make_body(round2, 0, Vec3::ZERO);
    dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: sp1.dims_bricks }, Some((dil_min, dil_max)));
    assert_eq!(atlas.generation, gen_before, "a windowed pool write must not bump the generation");

    // INCREMENTAL mesh with the bake's voxel window.
    assert!(mesher.mesh_into_slot_sparse_tiled(
        &gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool,
        Some(&crate::sdf_gpu_mesh::SdfTileDirty::Voxels(dil_min, dil_max))));
    let (nd, nt) = slot.last_dispatched_tiles;
    assert!(nd > 0, "the edit must touch at least one tile");
    assert!(nd < nt, "incremental must SKIP clean tiles (dispatched {nd}/{nt})");
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert_eq!(slot.poll_tile_overflow().map(|o| o.len()), Some(0), "unexpected tile overflow");
    let (inc_cnt, inc_args, inc_verts, _inc_idx) = tiled_readback(&gpu.device, &gpu.queue, &pool, nid);

    // REFERENCE: from-scratch FULL tiled mesh of the SAME atlas state (fresh slot + pool).
    let mut pool_ref = crate::sdf_gpu_mesh::SdfMeshPool::new(&gpu.device, false);
    let mut slot_ref = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(
        &gpu.device, &gpu.queue, &mut slot_ref, &atlas, gmin, gmax, nid, &mut pool_ref, None));
    let (ref_cnt, ref_args, ref_verts, _) = tiled_readback(&gpu.device, &gpu.queue, &pool_ref, nid);

    // Same tile tables (occupancy unchanged ⇒ same keys, same order, same packing) ⇒ the args
    // must be IDENTICAL per tile — including index_count, the geometry truth.
    let s_inc = pool.slot(nid).unwrap();
    let s_ref = pool_ref.slot(nid).unwrap();
    assert_eq!(s_inc.tiles.len(), s_ref.tiles.len(), "tile table diverged");
    let mut sum_inc = 0u64;
    let mut sum_ref = 0u64;
    let mut dirty_seen = 0u32;
    for (k, (ti, tr)) in s_inc.tiles.iter().zip(s_ref.tiles.iter()).enumerate() {
        assert_eq!(ti.tile, tr.tile, "tile {k}: key order diverged");
        assert!(!ti.dead, "no tile should die in this scenario");
        assert_eq!((ti.rel_vert, ti.vert_cap, ti.rel_idx, ti.idx_cap),
                   (tr.rel_vert, tr.vert_cap, tr.rel_idx, tr.idx_cap),
                   "tile {k}: packing diverged (occupancy is unchanged)");
        assert_eq!(inc_cnt[k], ref_cnt[k], "tile {k}: counters != full re-mesh reference");
        assert_eq!(inc_args[k], ref_args[k], "tile {k}: args != full re-mesh reference");
        sum_inc += inc_cnt[k][1] as u64;
        sum_ref += ref_cnt[k][1] as u64;
        // Clean tiles: BIT-preserved vs before the partial bake (their buffers were untouched).
        let in_dirty = (0..3).all(|a| {
            let lo = ti.tile[a] as i64 * 32 - 3;
            let hi = ti.tile[a] as i64 * 32 + 33;
            (dil_min[a] as i64) <= hi && (dil_max[a] as i64) - 1 >= lo
        });
        if in_dirty {
            dirty_seen += 1;
        } else {
            for (va, vb) in inc_verts[k].iter().zip(verts_before[k].iter()) {
                assert_eq!(va.to_array().map(f32::to_bits), vb.to_array().map(f32::to_bits),
                    "clean tile {k}: vertex bits changed by the incremental dispatch");
            }
        }
    }
    assert_eq!(dirty_seen, nd, "dispatched-tile count != dirty predicate count");
    assert_eq!(sum_inc, sum_ref, "sum of index_count: incremental != full reference");
    // Vertex parity 0 (bit-exact-class) BOTH directions across the whole body.
    let all_inc: Vec<Vec3> = inc_verts.iter().flatten().copied().collect();
    let all_ref: Vec<Vec3> = ref_verts.iter().flatten().copied().collect();
    assert_eq!(all_inc.len(), all_ref.len(), "total vertex count diverged");
    let (m1, x1) = parity(&all_inc, &all_ref, voxel);
    let (m2, x2) = parity(&all_ref, &all_inc, voxel);
    assert!(m1 < 1e-6 && x1 < 1e-6, "incremental vs full divergence (mean={m1} max={x1})");
    assert!(m2 < 1e-6 && x2 < 1e-6, "full vs incremental divergence (mean={m2} max={x2})");
    eprintln!("[SdfTiledInc] incremental == full: tiles={nd}/{nt} dispatched, sum_idx={sum_inc}, \
        parity 0.0 both ways, clean tiles bit-preserved");
}

// ── 2048-VIRTUAL FEASIBILITY SPIKE (2026-07-17, #[ignore]d, measurement-only) ────────────────────
//
// Question: can the EXISTING sparse stack (GPU classify → SdfBrickAtlasGpu → mode-2 sparse bake →
// monolithic windowed DC mesh) run a 2048³ body end-to-end, and which component breaks first?
// The production resolution cap (`max_sparse_resolution() == 1024`, enforced by
// `effective_resolution`) is BYPASSED here by overriding the queued request's resolution — the
// spike measures the pipeline BELOW the cap, it does not ship 2048. Every GPU stage runs inside a
// validation error scope so a limit violation is REPORTED (with the driver message) instead of
// aborting the whole spike. CPU parity oracles are skipped above 1024 (a 2048³ CPU classify is
// minutes); structural validation (dense-handle decode) + surface-scaling ratios stand in.
//
// Run: cargo test -p app --bins spike_sparse_2048_feasibility -- --ignored --nocapture --test-threads=1

/// Build a production-shaped FULL-classify request via the real cache trigger at 1024 (the cap),
/// then override the resolution — prim_floats/bounds/hashes stay dispatch-time-provenant.
fn spike_request(
    sg: &SceneGraph, nid: NodeId, gmin: Vec3, gmax: Vec3, res: u32,
) -> crate::body_volume_cache::PrimaryBakeRequest {
    use crate::body_volume_cache::bake_dispatch::rigid_translate_tests::sparse_cache;
    let mut cache = sparse_cache(nid, (gmin, gmax));
    cache.gpu_classify_ready = true;
    cache.set_body_resolution(nid, 1024);
    cache.clear_bake_debounce();
    cache.trigger_bake(sg, None);
    let q = cache.pending_gpu_classify.pop().expect("spike: FULL classify must queue");
    let mut req = q.request;
    req.resolution = [res; 3];
    req.bake_min = [0; 3];
    req.bake_max = [res; 3];
    req
}

fn pop_validation_err(device: &wgpu::Device) -> Option<String> {
    pollster::block_on(device.pop_error_scope()).map(|e| e.to_string())
}

/// Local mirror of `dc_pipeline::VERTS_PER_BRICK_HEADROOM` for the VRAM budget print.
const VERTS_PER_BRICK_HEADROOM_SPIKE: u64 = 48;

/// One end-to-end attempt at `res`. Returns false if a stage failed (already reported).
fn spike_run(
    gpu: &BakeGpu, classify_pipeline: &wgpu::ComputePipeline,
    finalize: &crate::gpu_ctx::ClassifyFinalizePipelines,
    prims: Vec<SdfPrimitive>, res: u32, label: &str,
) -> bool {
    use crate::app_poll_bake::{decode_gpu_finalized_index, encode_gpu_classify_dispatch};
    use std::time::Instant;

    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let (sg, nid) = make_body(prims, 0, Vec3::ZERO);
    let bpa = res / 8;
    let total = (bpa as u64 * bpa as u64 * bpa as u64) as usize;
    eprintln!("\n[Spike2048] ===== {label} @ {res}^3 — brick index {bpa}^3 = {total} entries \
               ({:.1} MB) =====", total as f64 * 4.0 / 1048576.0);

    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("spike_classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });

    // ── Stage 1: GPU classify, GPU-finalize arm first (the production STAGE-3 path) ──
    let req = spike_request(&sg, nid, gmin, gmax, res);
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let t = Instant::now();
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, classify_pipeline, &gpu.bgl, &dummy_tex,
        req, crate::gpu_jobs::IDENTITY_MAT4, Some(finalize));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let classify_ms = t.elapsed().as_secs_f64() * 1000.0;
    let gpu_finalize_err = pop_validation_err(&gpu.device);

    let si = {
        // 2048-SHIP GATE (2026-07-17): the GPU-finalize arm is now the ONLY acceptable path —
        // the 2D dispatch grid removed the 65535-workgroup break, so a validation error here
        // is a REGRESSION, not a known cliff. (The CPU-fallback arm this used to exercise
        // lives on in production only as the degraded no-finalize-pipelines path.)
        assert!(gpu_finalize_err.is_none(),
            "{res}^3 GPU-FINALIZE classify failed validation — the FULL GPU path must run \
             end-to-end at every spike resolution since the 2D dispatch fix: {}",
            gpu_finalize_err.unwrap());
        if pending.map_ready.peek() != crate::map_state::OK {
            eprintln!("[Spike2048] {res}^3 finalized-index readback did not map (device lost?)");
            return false;
        }
        let raw: Vec<u32> = {
            let view = pending.staging.slice(..).get_mapped_range();
            bytemuck::cast_slice(&view).to_vec()
        };
        pending.staging.unmap();
        let t = Instant::now();
        let si = match decode_gpu_finalized_index(&raw, pending.dims_bricks) {
            Some(si) => si,
            None => { eprintln!("[Spike2048] {res}^3 finalized index failed validation"); return false; }
        };
        let validate_ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("[Spike2048] {res}^3 GPU-finalize arm OK: classify+readback={classify_ms:.1}ms \
                   frame-thread validate={validate_ms:.1}ms ({} entries)", si.index.len());
        si
    };
    if si.allocated == 0 { eprintln!("[Spike2048] {res}^3 allocated 0 — vacuous"); return false; }

    // ── Stage 2: CPU index scans at this entry count (suspect 2) ──
    let t = Instant::now();
    let (_bmin, _bmax, tiles) = graphics::sdf_brick_atlas::index_content_scan(&si.index, si.dims_bricks);
    let scan_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[Spike2048] {res}^3 index_content_scan(fused, rayon) = {scan_ms:.1}ms \
               ({} live tiles)", tiles.len());

    // ── VRAM budget (created sizes; ensure_capacity pads +25% on grow) ──
    let alloc = si.allocated as u64;
    let pool_mb = alloc as f64 * 2048.0 / 1048576.0;
    let cv_mb = alloc as f64 * 512.0 * 4.0 / 1048576.0;
    let vcap = (alloc * VERTS_PER_BRICK_HEADROOM_SPIKE).max(64);
    let verts_mb = vcap as f64 * 32.0 / 1048576.0;
    let idx_mb = vcap as f64 * 6.0 * 4.0 / 1048576.0;
    eprintln!("[Spike2048] {res}^3 allocated={alloc} bricks | pool={pool_mb:.0}MB \
               cell_vert={cv_mb:.0}MB verts<={verts_mb:.0}MB idx<={idx_mb:.0}MB \
               (+index {:.0}MB) — single-binding limit = {:.0}MB",
        total as f64 * 4.0 / 1048576.0,
        gpu.device.limits().max_storage_buffer_binding_size as f64 / 1048576.0);

    // ── Stage 3: atlas create + index upload ──
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, si.index.len() as u32, si.allocated as u32);
    atlas.ensure_capacity(&gpu.device, &gpu.queue, si.index.len() as u32, si.allocated as u32);
    let t = Instant::now();
    atlas.upload_index(&gpu.queue, &si.index, si.allocated as u32, si.dims_bricks, res, nid);
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let upload_ms = t.elapsed().as_secs_f64() * 1000.0;
    if let Some(err) = pop_validation_err(&gpu.device) {
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        eprintln!("[Spike2048] {res}^3 ATLAS alloc/upload FAILED (pool cap {} bricks = {:.0}MB): {first}",
            atlas.pool_capacity_bricks, atlas.pool_capacity_bricks as f64 * 2048.0 / 1048576.0);
        return false;
    }
    eprintln!("[Spike2048] {res}^3 atlas OK: index cap={} pool cap={} bricks ({:.0}MB arena) \
               upload(+scan)={upload_ms:.1}ms",
        atlas.index_capacity, atlas.pool_capacity_bricks,
        atlas.pool_capacity_bricks as f64 * 2048.0 / 1048576.0);

    // ── Stage 4: mode-2 sparse bake, production Z-slab slicing ──
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let t = Instant::now();
    let mut z0 = 0u32;
    while z0 < res {
        let z1 = (z0 + 64).min(res);
        dispatch_bake(gpu, &sg, nid, res, gmin, gmax,
            BakeOut::SparseAtlas { atlas: &atlas, dims: si.dims_bricks },
            Some(([0, 0, z0], [res, res, z1])));
        z0 = z1;
    }
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let bake_ms = t.elapsed().as_secs_f64() * 1000.0;
    if let Some(err) = pop_validation_err(&gpu.device) {
        eprintln!("[Spike2048] {res}^3 mode-2 BAKE failed: {err}");
        return false;
    }
    eprintln!("[Spike2048] {res}^3 mode-2 bake ({} z-slabs) = {bake_ms:.1}ms", (res + 63) / 64);

    // ── Stage 5: monolithic windowed sparse DC mesh + counters readback ──
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let t = Instant::now();
    mesher.mesh_into_slot_sparse(&gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, None);
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let mesh_ms = t.elapsed().as_secs_f64() * 1000.0;
    if let Some(err) = pop_validation_err(&gpu.device) {
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        eprintln!("[Spike2048] {res}^3 DC MESH failed validation (vert_cap={} cell_vert_cap={}): {first}",
            slot.vert_cap, slot.cell_vert_cap);
        return false;
    }
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spike_counters_stg"), size: 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&slot.counters, 0, &staging, 0, 8);
    gpu.queue.submit(std::iter::once(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    match rx.recv() {
        Ok(Ok(())) => {}
        other => {
            eprintln!("[Spike2048] {res}^3 counters readback failed: {other:?} — DEVICE LOST");
            return false;
        }
    }
    let counts: Vec<u32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    let (vc, ic) = (counts[0], counts[1]);
    let overflow = vc as u64 > slot.vert_cap || ic as u64 >= slot.idx_cap;
    eprintln!("[Spike2048] {res}^3 DC mesh = {mesh_ms:.1}ms → verts={vc} idx={ic} tris={} \
               (caps v={} i={}){}", ic / 3, slot.vert_cap, slot.idx_cap,
        if overflow { "  ** COUNTER OVERFLOW — slot would grow+re-mesh **" } else { "" });
    vc > 0 && ic > 0
}

#[test]
#[ignore] // spike: minutes of GPU time + deliberately over-cap resolutions
fn spike_sparse_2048_feasibility() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[Spike2048] no GPU adapter — skipping"); return; }
    };
    let l = gpu.device.limits();
    eprintln!("[Spike2048] device limits: max_buffer_size={:.2}GB \
               max_storage_buffer_binding_size={:.2}GB max_workgroups_per_dim={}",
        l.max_buffer_size as f64 / 1.0e9, l.max_storage_buffer_binding_size as f64 / 1.0e9,
        l.max_compute_workgroups_per_dimension);

    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spike classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);

    // Moderate surface: a small sphere — surface bricks stay well under the pool ceiling even
    // at 2048 (locates ADDRESSING/dispatch cliffs without conflating them with VRAM cliffs).
    let moderate = || vec![sphere_op(Vec3::ZERO, 0.35, BooleanOp::Union { k: 0.0 })];
    // Realistic density: the canonical sphere−subtract (same scene as sparse_run_at) — the
    // surface that measured ~4x/octave; at 2048 this is the VRAM-cliff probe.
    let realistic = || vec![
        sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
    ];

    let mut ok = Vec::new();
    for (label, prims, res) in [
        ("realistic-1024-baseline", realistic(), 1024u32),
        ("moderate-1536", moderate(), 1536),
        ("moderate-2048", moderate(), 2048),
        ("realistic-1536", realistic(), 1536),
        ("realistic-2048", realistic(), 2048),
    ] {
        let passed = spike_run(&gpu, &classify_pipeline, &finalize, prims, res, label);
        ok.push((label, res, passed));
    }
    eprintln!("\n[Spike2048] ===== SUMMARY =====");
    for (label, res, passed) in ok {
        eprintln!("[Spike2048]   {label} @ {res}^3 → {}",
            if passed { "RAN END-TO-END" } else { "BROKE (see stage log)" });
    }
}

/// 2048-SHIP item 1 gate — the classify-finalize 2D dispatch grid at >65535 workgroups.
/// 2048³ ⇒ 256³ = 16 777 216 bricks ⇒ 65 536 linear workgroups of 256 — EXACTLY one over
/// `max_compute_workgroups_per_dimension` (65535), the spike's first hard break. This is the
/// SMALLEST possible crossing (brick grids are (res/8)³; 1536³ is 27 648 groups), so a
/// synthetic sub-2048 index cannot exercise the 2D row path — the test drives the REAL
/// production encode at 2048 twice: the GPU-finalize arm (2D grid) and the verdict arm, then
/// gates the GPU index BIT-IDENTICAL against `sparse_index_from_verdicts` on the exact
/// verdicts the GPU finalize consumed (the tests_gpu_classify parity contract, at scale —
/// also the first exercise of `classify_finalize_block_scan`'s serial loop at nb = 65536).
/// Small sphere: surface bricks stay tiny; only the INDEX is at full 2048 scale (~64MB
/// readbacks + ~805MB transient work buffer — the same class the production classify pays).
#[test]
fn classify_finalize_2d_dispatch_crosses_65535_groups_matches_cpu() {
    use crate::app_poll_bake::{
        decode_gpu_classify_verdicts, decode_gpu_finalized_index, encode_gpu_classify_dispatch,
    };
    use core_math::chunked_dc::sparse_index_from_verdicts;

    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[Classify2D] no GPU adapter — skipping"); return; }
    };
    let res = 2048u32;
    let bpa = res / 8;
    let total = (bpa * bpa * bpa) as usize;
    let max_dim = gpu.device.limits().max_compute_workgroups_per_dimension;
    let linear_groups = ((total + 255) / 256) as u32;
    assert!(linear_groups > max_dim,
        "anti-vacuity: this test exists to cross the per-dimension limit \
         ({linear_groups} groups vs limit {max_dim})");
    let (gx, gy) = crate::app_poll_bake::classify_finalize_grid(linear_groups, max_dim);
    assert!(gx <= max_dim && gy >= 2 && (gx as u64 * gy as u64) >= linear_groups as u64,
        "grid must cover every group within the limit (got {gx}x{gy})");

    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("classify2d volume_bake"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&gpu.bgl], push_constant_ranges: &[],
    });
    let classify_pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks"), layout: Some(&pl), module: &shader,
        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&gpu.device, &pl, &shader, None);

    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let prims = vec![sphere_op(Vec3::ZERO, 0.35, BooleanOp::Union { k: 0.0 })];
    let (sg, nid) = make_body(prims, 0, Vec3::ZERO);
    let dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify2d_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });
    let read_staging = |pending: &crate::PendingGpuClassify| -> Vec<u32> {
        assert_eq!(pending.map_ready.peek(), crate::map_state::OK, "readback must map OK");
        let raw: Vec<u32> = {
            let view = pending.staging.slice(..).get_mapped_range();
            bytemuck::cast_slice(&view).to_vec()
        };
        pending.staging.unmap();
        raw
    };

    // Arm A: GPU finalize (2D grid — the path under test), validation-scoped.
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let req = spike_request(&sg, nid, gmin, gmax, res);
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, &classify_pipeline, &gpu.bgl, &dummy_tex,
        req, crate::gpu_jobs::IDENTITY_MAT4, Some(&finalize));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    if let Some(err) = pop_validation_err(&gpu.device) {
        panic!("2D-grid classify finalize FAILED validation at {gx}x{gy} groups: {err}");
    }
    let raw = read_staging(&pending);
    let gpu_si = decode_gpu_finalized_index(&raw, pending.dims_bricks)
        .expect("GPU-finalized index failed structural validation at 2048");

    // Arm B: verdict readback + CPU finalize oracle on the SAME scene/bounds.
    let req = spike_request(&sg, nid, gmin, gmax, res);
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, &classify_pipeline, &gpu.bgl, &dummy_tex,
        req, crate::gpu_jobs::IDENTITY_MAT4, None);
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let raw = read_staging(&pending);
    let verdicts = decode_gpu_classify_verdicts(&raw).expect("garbage verdict buffer");
    let cpu_si = sparse_index_from_verdicts(&verdicts, pending.dims_bricks);

    assert!(gpu_si.allocated > 0, "anti-vacuity: the sphere allocates surface bricks");
    assert_eq!(gpu_si.allocated, cpu_si.allocated, "allocated diverged at 65536 workgroups");
    assert_eq!(gpu_si.index, cpu_si.index,
        "2D-grid GPU finalize must stay BIT-IDENTICAL to the CPU finalize across the \
         65535-workgroup boundary (row flatten / tail guard / block_scan at nb=65536)");
    eprintln!("[Classify2D] 2048³ ({gx}x{gy} groups): GPU finalize bit-identical, \
               bricks={}/{}", gpu_si.allocated, gpu_si.index.len());
}

// ── BEVELED-SUBTRACT STRIPES DIAGNOSTIC (2026-07-17, owner screenshot: stringy sheets in the
//    carved void of a boolean subtract with per-edge bevels) ─────────────────────────────────────
//
// Two competing hypotheses, one gate each (see CACHE_REMESH_512_REVIEW.md session log):
//   H1 (eval): the tri-planar per-edge bevel (`sdf_box_per_edge`, hard max() with mixed radii —
//       the tracked notch bug) produces the sheets → the FULL from-scratch classify+bake diverges
//       from the CPU oracle. Gated by `sparse_beveled_subtract_full_bake_matches_oracle`.
//   H2 (stale bricks): only the INCREMENTAL machinery (windowed reclassify f833129 / frozen-index
//       partial) leaves retained-but-dirty brick content → the full bake is clean and the
//       windowed round diverges. Gated by `sparse_windowed_beveled_subtract_carve`.
//
// REUSE (searched first): `check_scene` and `run_windowed_escalation_gate` already assert
// everything needed (oracle sign flips, crossing-brick holes, out-of-window byte-identity,
// bit-exact in-window parity). Only the beveled-box builder is new — `box_op` hardcodes
// `BoxBevel::default()` and no beveled builder exists in this file or chunked_dc's test helpers.

/// Box prim with MIXED per-edge Round bevels (different radii on different edges — the exact
/// configuration the Per-Edge Bevel Corner Rule flags for the hard-max notch) + one Chamfer edge.
fn beveled_box_op(pos: Vec3, he: Vec3, op: BooleanOp) -> SdfPrimitive {
    use core_math::sdf_shapes::BevelMode;
    let mut bevel = BoxBevel::default();
    // Vertical edges (8-11): mixed Round radii.
    bevel.radii[8] = 0.15; bevel.modes[8] = BevelMode::Round;
    bevel.radii[9] = 0.35; bevel.modes[9] = BevelMode::Round;
    bevel.radii[10] = 0.25; bevel.modes[10] = BevelMode::Round;
    bevel.radii[11] = 0.45; bevel.modes[11] = BevelMode::Round;
    // Bottom edges (0-3): uniform Round + one Chamfer (mode mix).
    bevel.radii[0] = 0.3; bevel.modes[0] = BevelMode::Round;
    bevel.radii[1] = 0.3; bevel.modes[1] = BevelMode::Chamfer;
    bevel.radii[2] = 0.3; bevel.modes[2] = BevelMode::Round;
    bevel.radii[3] = 0.3; bevel.modes[3] = BevelMode::Round;
    let mut p = SdfPrimitive::new(
        "bev".into(), SdfShape::Box { half_extents: he, bevel }, pos);
    p.operation = op;
    p
}

/// H1 GATE — from-scratch FULL classify + full sparse bake of the owner's scene shape (box body,
/// beveled cube subtracted). If the eval itself made the sheets, this fails (oracle sign flips /
/// sentinel sign violations / mode-0-vs-mode-2 bit mismatch). Passing = the analytic field and
/// the full pipeline are clean → the sheets are incremental-machinery state.
#[test]
fn sparse_beveled_subtract_full_bake_matches_oracle() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[BevSub] no GPU adapter — skipping"); return; }
    };
    let res = 96u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let scene = vec![
        box_op(Vec3::ZERO, Vec3::splat(1.0), BooleanOp::Union { k: 0.0 }),
        beveled_box_op(Vec3::new(0.5, 0.5, 0.5), Vec3::splat(0.55), BooleanOp::Subtract { k: 0.0 }),
    ];
    check_scene(&gpu, "beveled-subtract", &scene, 0, Vec3::ZERO, res, gmin, gmax, band, false, false);
    // Hard subtract with k>0 smoothing too (the properties-panel default path).
    let scene_k = vec![
        box_op(Vec3::ZERO, Vec3::splat(1.0), BooleanOp::Union { k: 0.0 }),
        beveled_box_op(Vec3::new(0.5, 0.5, 0.5), Vec3::splat(0.55), BooleanOp::Subtract { k: 0.08 }),
    ];
    check_scene(&gpu, "beveled-subtract-k", &scene_k, 0, Vec3::ZERO, res, gmin, gmax, band, false, false);
}

/// H2 GATE — the owner's gesture: DRAG a beveled subtract cube across a body at an
/// escalation-band window, through the windowed reclassify worker path (f833129). The carve
/// vacates old void surface (reclaim/retag) and opens new carved surface deep in former INTERIOR
/// territory (append). Any retained-but-stale brick (the "sheets") fails the oracle sign check,
/// the crossing-brick hole check, or the in-window bit-exactness vs the full-classify reference.
#[test]
fn sparse_windowed_beveled_subtract_carve() {
    let res = 256u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    // Body: big box (deep INTERIOR sentinels exist past the ~0.58-world classifier margin at
    // this res — the trench floor below must reach them so the carve really APPENDS).
    let body = box_op(Vec3::ZERO, Vec3::splat(1.3), BooleanOp::Union { k: 0.0 });
    // Beveled subtract cube carving a DEEP trench through the top (floor at y=-0.2, past the
    // body center), dragged 1.2 in X (> margin, so the new carved surface really appends in
    // former INTERIOR territory and the vacated side really reclaims).
    let he = Vec3::new(0.6, 1.0, 0.6);
    let (p0, p1) = (Vec3::new(-0.6, 0.8, 0.0), Vec3::new(0.6, 0.8, 0.0));
    let round1 = vec![body.clone(), beveled_box_op(p0, he, BooleanOp::Subtract { k: 0.0 })];
    let round2 = vec![body, beveled_box_op(p1, he, BooleanOp::Subtract { k: 0.0 })];
    run_windowed_escalation_gate(
        "beveled-subtract-carve", res, gmin, gmax, round1, round2,
        (p0 - he).min(p1 - he), (p0 + he).max(p1 + he),
        false,
    );
}

/// Scratch probe (temporary): sentinel census + windowed-walk alloc stats for the carve scene.
#[test]
#[ignore = "diagnostic probe"]
fn probe_beveled_carve_classify() {
    let res = 256u32;
    let (gmin, gmax) = (Vec3::splat(-2.0), Vec3::splat(2.0));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let body = box_op(Vec3::ZERO, Vec3::splat(1.3), BooleanOp::Union { k: 0.0 });
    let he = Vec3::new(0.6, 1.0, 0.6);
    let (p0, p1) = (Vec3::new(-0.6, 0.8, 0.0), Vec3::new(0.6, 0.8, 0.0));
    let round1 = vec![body.clone(), beveled_box_op(p0, he, BooleanOp::Subtract { k: 0.0 })];
    let round2 = vec![body, beveled_box_op(p1, he, BooleanOp::Subtract { k: 0.0 })];
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let n_ext = sp1.index.iter().filter(|&&t| t == BRICK_TAG_EXTERIOR).count();
    let n_int = sp1.index.iter().filter(|&&t| t == BRICK_TAG_INTERIOR).count();
    eprintln!("[Probe] round1: bricks={} allocated={} ext={} int={}", sp1.index.len(), sp1.allocated, n_ext, n_int);
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();
    let margin = Vec3::splat(2.0 * voxel);
    let (win_min, win_max) = world_window_to_voxel((p0 - he).min(p1 - he) - margin, (p0 + he).max(p1 + he) + margin, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    let wb: u64 = (0..3).map(|a| ((dil_max[a] - dil_min[a] + 7) / 8) as u64).product();
    let total: u64 = (0..3).map(|_| ((res + 7) / 8) as u64).product();
    eprintln!("[Probe] window=[{dil_min:?}..{dil_max:?}) wb={wb}/{total} (1/8 band: {} .. {})", total/8, total/2);
    let clf2 = BrickClassifier::new(&round2, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let mut updated = sp1.clone();
    let alloc = sparse_window_allocate_incompatible(&mut updated, 8, dil_min, dil_max, &clf2).unwrap();
    eprintln!("[Probe] alloc: appended={} reused={} retagged={} reclaimed={}", alloc.appended, alloc.reused, alloc.retagged, alloc.reclaimed);
}

/// MESH-LEVEL H1/H2 GATE — the sheets the owner sees are GEOMETRY, so gate the production tiled
/// DC mesher's OUTPUT VERTICES against the true field: every vertex of the beveled-subtract mesh
/// must lie on the iso-surface (|field| ≲ a voxel). Stringy sheets floating in the carved void
/// are vertices at |field| >> voxel — this fails loudly if the mesher (halo reads, QEF at the
/// hard-max bevel ridges, tile stitching) manufactures geometry the field does not have.
/// Runs the FULL pipeline (classify → atlas bake → tiled mesh) then an INCREMENTAL windowed
/// round (bevel-slider shape change) and applies the same vertex gate to both.
#[test]
fn sparse_beveled_subtract_mesh_verts_on_isosurface() {
    let gpu = match bake_gpu() {
        Some(g) => g, None => { eprintln!("[BevMesh] no GPU adapter — skipping"); return; }
    };
    use super::tests_dc::tiled_readback;
    let res = 128u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let voxel = ((gmax - gmin) / (res - 1) as f32).min_element();

    let body = box_op(Vec3::ZERO, Vec3::splat(1.0), BooleanOp::Union { k: 0.0 });
    let sub_pos = Vec3::new(0.5, 0.5, 0.5);
    let sub_he = Vec3::splat(0.55);
    let round1 = vec![body.clone(), beveled_box_op(sub_pos, sub_he, BooleanOp::Subtract { k: 0.0 })];
    // Round 2 = the bevel-SLIDER gesture: same prim, all vertical radii cranked (mixed → larger).
    let mut r2sub = beveled_box_op(sub_pos, sub_he, BooleanOp::Subtract { k: 0.0 });
    if let SdfShape::Box { bevel, .. } = &mut r2sub.shape {
        bevel.radii[8] = 0.5; bevel.radii[9] = 0.5; bevel.radii[10] = 0.1; bevel.radii[11] = 0.3;
    }
    let round2 = vec![body, r2sub];

    // Vertex-on-isosurface gate against the dense CPU oracle (trilinear sample).
    let vert_gate = |label: &str, verts: &[Vec3], oracle: &[f32]| {
        assert!(!verts.is_empty(), "{label}: empty mesh");
        let n = res as usize;
        let scale = (res - 1) as f32 / (gmax.x - gmin.x);
        let (mut worst, mut bad) = (0.0f32, 0usize);
        for v in verts {
            let g = (*v - gmin) * scale;
            let (xi, yi, zi) = (g.x.floor().clamp(0.0, (n - 2) as f32) as usize,
                                g.y.floor().clamp(0.0, (n - 2) as f32) as usize,
                                g.z.floor().clamp(0.0, (n - 2) as f32) as usize);
            let f = |x: usize, y: usize, z: usize| oracle[x + y * n + z * n * n];
            let (tx, ty, tz) = (g.x - xi as f32, g.y - yi as f32, g.z - zi as f32);
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let d = lerp(
                lerp(lerp(f(xi,yi,zi), f(xi+1,yi,zi), tx), lerp(f(xi,yi+1,zi), f(xi+1,yi+1,zi), tx), ty),
                lerp(lerp(f(xi,yi,zi+1), f(xi+1,yi,zi+1), tx), lerp(f(xi,yi+1,zi+1), f(xi+1,yi+1,zi+1), tx), ty),
                tz);
            let ad = d.abs();
            worst = worst.max(ad);
            if ad > 2.0 * voxel { bad += 1; }
        }
        eprintln!("[BevMesh] {label}: verts={} worst|field|={worst:.4} ({}x voxel) off-surface={bad}",
            verts.len(), worst / voxel);
        assert_eq!(bad, 0,
            "{label}: {bad} mesh vertices sit further than 2 voxels from the iso-surface \
             (worst {worst:.4}) — stringy-sheet geometry the field does not have");
    };

    // ROUND 1 — full classify → atlas bake → full tiled mesh.
    let sp1 = classify_prims_to_sparse_index(&round1, 0, Vec3::ZERO, gmin, gmax, res, 8, band);
    let nid = NodeId(802);
    let mut atlas = graphics::SdfBrickAtlasGpu::new(
        &gpu.device, sp1.index.len() as u32, sp1.allocated as u32);
    atlas.upload_index(&gpu.queue, &sp1.index, sp1.allocated as u32, sp1.dims_bricks, res, nid);
    let (sg1, body1) = make_body(round1.clone(), 0, Vec3::ZERO);
    dispatch_bake(&gpu, &sg1, body1, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: sp1.dims_bricks }, None);
    let mesher = crate::sdf_gpu_mesh::SdfMeshGpu::new(&gpu.device);
    let mut pool = crate::sdf_gpu_mesh::SdfMeshPool::new(&gpu.device, false);
    let mut slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&gpu.device, res);
    assert!(mesher.mesh_into_slot_sparse_tiled(
        &gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool, None));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let _ = slot.poll_tile_overflow();
    let (_, _, verts1, _) = tiled_readback(&gpu.device, &gpu.queue, &pool, nid);
    let oracle1 = crate::bake_utils::bake_body_grid(&round1, res, gmin, gmax, 0, 0, Vec3::ZERO);
    let all1: Vec<Vec3> = verts1.iter().flatten().copied().collect();
    vert_gate("full-bake", &all1, &oracle1);

    // ROUND 2 — the slider edit through the PARTIAL machinery: windowed re-bake of the prim's
    // AABB into the SAME atlas (index frozen — handled bricks refresh in place), then the
    // INCREMENTAL dirty-tile mesh. The gate proves no stale-brick sheet survives in the mesh.
    let margin = Vec3::splat(2.0 * voxel);
    let (win_min, win_max) = world_window_to_voxel(
        sub_pos - sub_he - margin, sub_pos + sub_he + margin, gmin, gmax, res);
    let (dil_min, dil_max) = dilate_voxel_window(win_min, win_max, 8, [res; 3]);
    assert!(sparse_window_bricks_all_handled(&sp1.index, sp1.dims_bricks, 8, dil_min, dil_max),
        "premise: slider window must stay within handled bricks (production would escalate)");
    let (sg2, body2) = make_body(round2.clone(), 0, Vec3::ZERO);
    dispatch_bake(&gpu, &sg2, body2, res, gmin, gmax,
        BakeOut::SparseAtlas { atlas: &atlas, dims: sp1.dims_bricks }, Some((dil_min, dil_max)));
    assert!(mesher.mesh_into_slot_sparse_tiled(
        &gpu.device, &gpu.queue, &mut slot, &atlas, gmin, gmax, nid, &mut pool,
        Some(&crate::sdf_gpu_mesh::SdfTileDirty::Voxels(dil_min, dil_max))));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert_eq!(slot.poll_tile_overflow().map(|o| o.len()), Some(0), "unexpected tile overflow");
    let (_, _, verts2, _) = tiled_readback(&gpu.device, &gpu.queue, &pool, nid);
    let oracle2 = crate::bake_utils::bake_body_grid(&round2, res, gmin, gmax, 0, 0, Vec3::ZERO);
    let all2: Vec<Vec3> = verts2.iter().flatten().copied().collect();
    vert_gate("slider-incremental", &all2, &oracle2);
}

// ============================================================================
// GATE 2 — unified DirtyMeshRegion contract (CPU-only; no GPU needed).
//
// The tiled INC gates above prove: given an `SdfTileDirty::Voxels(window)`, the mesher
// dispatches exactly the tiles overlapping that window. So "same dispatched-tile set through
// the UNIFIED region" reduces to: the window the unified region hands the mesher is the SAME
// window the old `sparse_mesh_dirty_window` accumulator produced (union across partial
// landings, cleared by full acceptance, taken exactly once), AND the dense consumers
// (chunked-DC peek / upload decision) can no longer eat it out from under the tiled mesher.
// ============================================================================
#[test]
fn gate2_unified_region_partial_union_take_and_full_clear() {
    use crate::body_volume_cache::BodyVolumeCache;
    let mut cache = BodyVolumeCache::new(256, 2.0);
    let (_sg, nid) = make_body(vec![sphere_op(Vec3::ZERO, 0.8, BooleanOp::Union { k: 0.0 })], 0, Vec3::ZERO);

    // Two accepted PARTIAL landings with a skipped mesh in between → the take must yield the
    // UNION (a deferred consumer never loses a delta) — identical to the old accumulator.
    cache.note_bake_accepted_window(nid, [8, 16, 24], [40, 48, 56]);
    cache.note_bake_accepted_window(nid, [32, 8, 40], [64, 32, 72]);
    // Dense-consumer PEEK (chunked-DC read path) must NOT consume the region.
    assert!(cache.dirty_mesh_region.get(&nid).is_some(), "peek must not consume");
    let win = cache.take_dirty_mesh_region(nid).expect("accumulated window present");
    assert_eq!(win, ([8, 8, 24], [64, 48, 72]), "take = union of both partial landings");
    // Taken exactly once — the next consume is conservative-full, never stale.
    assert!(cache.take_dirty_mesh_region(nid).is_none(), "take consumes exactly once");

    // A partial landing followed by an accepted FULL bake → region cleared (absent = full
    // re-mesh incl. tile-table rebuild — occupancy may have changed anywhere).
    cache.note_bake_accepted_window(nid, [8, 8, 8], [16, 16, 16]);
    cache.note_full_bake_accepted(nid);
    assert!(cache.take_dirty_mesh_region(nid).is_none(), "full acceptance clears the region");

    // Dense accept flavor router: a full-covering window clears; a partial window unions.
    cache.note_bake_accepted_window(nid, [4, 4, 4], [12, 12, 12]);
    cache.note_bake_accepted(nid, ([0, 0, 0], [128, 128, 128]), [128, 128, 128]);
    assert!(cache.dirty_mesh_region.get(&nid).is_none(), "full-covering accept clears");
    cache.note_bake_accepted(nid, ([16, 0, 0], [32, 128, 128]), [128, 128, 128]);
    assert_eq!(cache.dirty_mesh_region.get(&nid).copied(), Some(([16, 0, 0], [32, 128, 128])),
        "partial accept unions into the region");
}
