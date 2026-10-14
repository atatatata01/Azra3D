//! GPU-classify SPIKE (stage 1) — HARDWARE parity harness for the `classify_bricks` compute
//! entry in `volume_bake.wgsl`: the GPU replication of the CPU per-brick Pass-1 predicate
//! ([`BrickClassifier::classify_one_brick`], core_math/chunked_dc.rs) that a later stage will
//! wire in to replace the CPU full classify (`classify_prims_to_sparse_index`). TESTS ONLY —
//! nothing dispatches this entry in production yet.
//!
//! What is compared, PER BRICK, on a real adapter:
//!   CPU `BrickClassifier::classify_one_brick(bx,by,bz)` brute-forced over EVERY brick
//!   (the superblock/containment-proof passes are flag-identical speedups — skipping them is
//!   verdict-equivalent by construction, see `superblock_verdicts` soundness notes)
//!   vs
//!   REAL `volume_bake.wgsl::classify_bricks` — one thread per brick, 9 samples of the SAME
//!   eval chain the mode-2 bake writes (`eval_scene_at`), same clamped vertex span, same
//!   endpoint-inclusive vertex→world mapping, same band+margin thresholds (margin computed on
//!   CPU and passed via the relabeled `_pad3` so the thresholds are bit-identical).
//!
//! Gate: ZERO mismatches on every scene. Verdict encoding (documented in the shader):
//! 0 = EXTERIOR, 1 = INTERIOR, 2 = SURFACE, written as `bitcast<f32>(verdict)` into the
//! output_grid arena (binding 2) — binding 6 (`brick_index`) is read-only in the bake BGL, so
//! it cannot carry the output; a classify dispatch binds a dims³ buffer at binding 2 instead.
//!
//! Searched for existing implementations before writing (REUSE rule): the device/BGL/pipeline/
//! dispatch/readback harness is `tests_sparse_bake.rs` (same BGL single source
//! `graphics::bgl::bake_bgl_entries`, same shader build `stub_out_blend_noise_wgsl`, same
//! `headless_device` from `tests_dc`, same dummy-binding wiring as a production mode-2 bake);
//! the CPU side consumes `BrickClassifier` directly — never re-implemented. Prim/body builders
//! are the same minimal re-statements the sparse harness uses (the canonical ones are
//! `#[cfg(test)]`-private inside core_math).
#![cfg(test)]

use eframe::wgpu;
use glam::{Quat, Vec3};
use std::time::Instant;

use super::tests_dc::headless_device;
use core_math::chunked_dc::{BrickClass, BrickClassifier, NONLIPSCHITZ_MARGIN_FACTOR};
use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial, SdfPrimitive, SdfShape};
use core_math::scene_graph::{NodeId, NodePayload, SceneGraph};
use core_math::sdf_shapes::BoxBevel;

// ── prim/body builders (same shapes as the sparse harness) ──────────────────────────────────────

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

/// Body wrapper for GPU serialization — same construction as `tests_sparse_bake::make_body`.
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

// ── GPU harness (tests_sparse_bake pattern, classify entry point) ───────────────────────────────

struct ClassifyGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    /// STAGE-3 GPU FINALIZE: the four `classify_finalize_*` pipelines, built by the SAME
    /// production compile helper (`gpu_init::compile_classify_finalize_pipelines`) from the
    /// same module/layout — every harness dispatch gates the on-device dilate+alloc against
    /// the CPU finalize (`sparse_index_from_verdicts`).
    finalize: crate::gpu_ctx::ClassifyFinalizePipelines,
}

/// Same BGL single source + shader build as the production bake pipeline; only the entry point
/// differs (`classify_bricks` instead of `bake_volume`).
fn classify_gpu() -> Option<ClassifyGpu> {
    let (_gpu_lock, device, queue) = headless_device()?;
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_classify_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu_classify volume_bake"),
        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gpu_classify"), layout: Some(&pl), module: &shader, entry_point: Some("classify_bricks"),
        compilation_options: Default::default(), cache: None,
    });
    let finalize = crate::gpu_init::compile_classify_finalize_pipelines(&device, &pl, &shader, None);
    Some(ClassifyGpu { device, queue, bgl, pipeline, finalize })
}

/// Serialize `body`, dispatch ONE `classify_bricks` pass over the whole brick grid, then the
/// STAGE-3 finalize chain (dilate → scan → block_scan → emit) over the SAME verdicts. Reads
/// back BOTH the per-brick verdict buffer (binding 2 of the classify) AND the GPU-finalized
/// index (`total+1` u32: tags + trailing allocated) — the strongest parity setup: the CPU
/// finalize oracle runs on the exact verdicts the GPU finalize consumed.
/// Returns `(verdicts, index_raw, gpu_pass_ms)`.
/// Bindings mirror a production mode-2 bake's dummy wiring everywhere the classify reads
/// nothing: 1³ texture (mode 2 precedent), zeroed 32B mesh_grid (operand disabled), 4-byte
/// brick_index dummy for the classify (verdicts OUT via binding 2); the finalize bind group
/// rebinds the verdict buffer at binding 6 and a work buffer at binding 2 (production wiring).
fn dispatch_classify(
    gpu: &ClassifyGpu, sg: &SceneGraph, body: NodeId,
    res: u32, gmin: Vec3, gmax: Vec3, dims: [u32; 3], brick_dim: u32, band: f32, margin: f32,
    coarse: bool,
) -> (Vec<u32>, Vec<u32>, f64) {
    let mut prim_floats = graphics::serialize_body_for_bake_hot(sg, body, Vec3::ZERO);
    let clip_floats = graphics::serialize_clip_curves_for_bake(sg, body, &mut prim_floats);
    let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;

    let total = (dims[0] * dims[1] * dims[2]) as usize;
    let out_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_verdicts"), size: (total * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Prefill with an impossible verdict — every brick must be overwritten by the pass.
    gpu.queue.write_buffer(&out_buf, 0, bytemuck::cast_slice(&vec![0xDEADBEEFu32; total]));
    let prim = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_prim"), size: (prim_floats.len() as u64 * 4).max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&prim, 0, bytemuck::cast_slice(&prim_floats));
    let clip = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_clip"), size: (clip_floats.len() as u64 * 4).max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    if !clip_floats.is_empty() { gpu.queue.write_buffer(&clip, 0, bytemuck::cast_slice(&clip_floats)); }
    let brick_idx_dummy = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_brick_idx_dummy"), size: 4,
        usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false,
    });
    let mesh_dummy = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_mesh_grid_dummy"), size: 32,
        usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false,
    });
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("classify_tex_dummy"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float, usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
    });
    let tex_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_params"), size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let p = crate::gpu_jobs::BakeParams {
        grid_res: [res; 3],
        _pad0: brick_dim,        // WGSL: brick_dim (relabeled spare pad)
        bounds_min: gmin.to_array(),
        _pad1: band,             // WGSL: classify_band (relabeled spare pad)
        bounds_max: gmax.to_array(),
        prim_count, floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
        dims_bricks: dims,
        bake_min: [0; 3],
        _pad3: margin.to_bits(), // WGSL: classify_margin: f32 (relabeled spare pad, bit-passed)
        bake_max: [res; 3], _pad4: if coarse { 1 } else { 0 },
        mirror_flags: 0, mirror_center: [0.0; 3], bevel_radius: 0.0, mirror_sign: 0,
        atlas_offset: [0; 3], atlas_pad: 0,
        output_mode: 2, // informational — classify_bricks never branches on it
        _pad5: 0,
        body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
    };
    gpu.queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&p));
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &gpu.bgl, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: prim.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 2, resource: out_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
        wgpu::BindGroupEntry { binding: 4, resource: clip.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 6, resource: brick_idx_dummy.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
        wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
    ] });
    // Make sure uploads are not billed to the timed pass.
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");

    // STAGE-3 work buffer + finalize bind group (production layout: mask | prefix | block
    // sums | index | allocated — see the shader's finalize header comment).
    let nb = (total + 255) / 256;
    let work_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_finalize_work"), size: ((3 * total + nb + 1) * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let fin_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &gpu.bgl, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: prim.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 2, resource: work_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
        wgpu::BindGroupEntry { binding: 4, resource: clip.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 6, resource: out_buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 7, resource: mesh_dummy.as_entire_binding() }, // curv_positions: unread here (filler)
        wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() }, // T1 height_maps: unread here (filler)
    ] });

    let t0 = Instant::now();
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
        cp.set_pipeline(&gpu.pipeline); cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups((dims[0] + 3) / 4, (dims[1] + 3) / 4, (dims[2] + 3) / 4);
    }
    {
        let linear = ((total + 255) / 256) as u32;
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
        cp.set_bind_group(0, &fin_bg, &[]);
        cp.set_pipeline(&gpu.finalize.dilate); cp.dispatch_workgroups(linear, 1, 1);
        cp.set_pipeline(&gpu.finalize.scan); cp.dispatch_workgroups(linear, 1, 1);
        cp.set_pipeline(&gpu.finalize.block_scan); cp.dispatch_workgroups(1, 1, 1);
        cp.set_pipeline(&gpu.finalize.emit); cp.dispatch_workgroups(linear, 1, 1);
    }
    gpu.queue.submit(std::iter::once(enc.finish()));
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    let gpu_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("classify_stg"), size: ((2 * total + 1) * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, (total * 4) as u64);
    enc.copy_buffer_to_buffer(&work_buf, ((2 * total + nb) * 4) as u64,
        &staging, (total * 4) as u64, ((total + 1) * 4) as u64);
    gpu.queue.submit(std::iter::once(enc.finish()));
    // GPU Safety rule: capture the map_async Result (never discard with |_| {}).
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().expect("map_async callback dropped").expect("verdict readback map failed");
    let all: Vec<u32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    let v = all[..total].to_vec();
    let index_raw = all[total..].to_vec();
    (v, index_raw, gpu_ms)
}

/// Shader verdict encoding → [`BrickClass`] (0 = EXTERIOR, 1 = INTERIOR, 2 = SURFACE).
fn decode_verdict(v: u32) -> BrickClass {
    match v {
        0 => BrickClass::Exterior,
        1 => BrickClass::Interior,
        2 => BrickClass::Surface,
        other => panic!("shader wrote an impossible verdict {other:#x} — brick never classified"),
    }
}

/// One scene: brute-force CPU `classify_one_brick` over every brick vs the GPU pass.
/// Returns per-mismatch `(cpu_class, gpu_class)` pairs — the zero-gate scenes assert the list
/// is EMPTY; the box-divergence pinning test asserts count + direction instead (never a silent
/// mismatch budget on the main gate).
fn check_scene(
    gpu: &ClassifyGpu, name: &str,
    prims: &[SdfPrimitive], mflags: u8, mc: Vec3,
    res: u32, gmin: Vec3, gmax: Vec3, band: f32,
) -> Vec<(BrickClass, BrickClass)> {
    check_scene_coarse(gpu, name, prims, mflags, mc, res, gmin, gmax, band, false)
}

/// Same as `check_scene` with the O1 workgroup coarse cull switchable — `coarse = true` drives
/// `params.coarse_cull = 1`, the production configuration for every `coarse_cull_safe` scene.
/// O1's contract is BIT-IDENTICAL verdicts to the fine path, so the same zero-mismatch gate
/// applies verbatim.
#[allow(clippy::too_many_arguments)]
fn check_scene_coarse(
    gpu: &ClassifyGpu, name: &str,
    prims: &[SdfPrimitive], mflags: u8, mc: Vec3,
    res: u32, gmin: Vec3, gmax: Vec3, band: f32, coarse: bool,
) -> Vec<(BrickClass, BrickClass)> {
    let bd = 8u32;
    let clf = BrickClassifier::new(prims, mflags, mc, gmin, gmax, res, bd, band);
    let dims = clf.dims_bricks();
    let total = (dims[0] * dims[1] * dims[2]) as usize;
    // The EXACT margin BrickClassifier computes (same expression order) — passed to the GPU so
    // the thresholds are bit-identical rather than recomputed in shader arithmetic.
    let denom = if res > 1 { (res - 1) as f32 } else { 1.0 };
    let margin = NONLIPSCHITZ_MARGIN_FACTOR * (((gmax - gmin) / denom) * bd as f32).length();

    // CPU brute force (timed for the speedup print) — every brick through the exact 9-eval
    // predicate, no proofs/superblocks (they are flag-identical speedups, vacuous for parity).
    let t0 = Instant::now();
    let cpu: Vec<BrickClass> = (0..total).map(|bi| {
        let bi = bi as u32;
        clf.classify_one_brick(bi % dims[0], (bi / dims[0]) % dims[1], bi / (dims[0] * dims[1]))
    }).collect();
    let cpu_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let (sg, body) = make_body(prims.to_vec(), mflags, mc);
    let (verdicts, index_raw, gpu_ms) =
        dispatch_classify(gpu, &sg, body, res, gmin, gmax, dims, bd, band, margin, coarse);
    assert_eq!(verdicts.len(), total, "{name}: verdict buffer length != brick count");

    // STAGE-3 FINALIZE PARITY GATE (every scene): the GPU dilate+alloc index must be
    // BIT-IDENTICAL to the CPU finalize (`sparse_index_from_verdicts`) run on the EXACT
    // verdicts the GPU finalize consumed — verdict divergence cannot mask a finalize bug.
    let cpu_si = crate::app_poll_bake::finalize_gpu_classify_verdicts(&verdicts, dims)
        .expect("harness verdicts decode (validated below anyway)");
    let gpu_si = crate::app_poll_bake::decode_gpu_finalized_index(&index_raw, dims)
        .expect("GPU-finalized index failed structural validation (dense-packed scan order)");
    assert_eq!(gpu_si.allocated, cpu_si.allocated,
        "{name}: GPU finalize allocated {} vs CPU {}", gpu_si.allocated, cpu_si.allocated);
    assert_eq!(gpu_si.index, cpu_si.index,
        "{name}: GPU dilate+alloc index must be BIT-IDENTICAL to sparse_index_from_verdicts");
    assert!(gpu_si.freed.is_empty() && cpu_si.freed.is_empty(), "fresh FULL index has no freed list");

    let (mut n_sur, mut n_int, mut n_ext) = (0usize, 0usize, 0usize);
    let mut mismatches: Vec<(BrickClass, BrickClass)> = Vec::new();
    for bi in 0..total {
        let g = decode_verdict(verdicts[bi]);
        match g {
            BrickClass::Surface => n_sur += 1,
            BrickClass::Interior => n_int += 1,
            BrickClass::Exterior => n_ext += 1,
        }
        if g != cpu[bi] {
            mismatches.push((cpu[bi], g));
            // DIAGNOSTIC: recompute the CPU 9-sample mn/mx for this brick and report how far
            // the deciding sample sits from the band+margin threshold — distinguishes an eval
            // drift at the conservative boundary (|mn-thresh| ~ 1e-6) from a systematic bug.
            let b = bi as u32;
            let (bx, by, bz) = (b % dims[0], (b / dims[0]) % dims[1], b / (dims[0] * dims[1]));
            let visible: Vec<&SdfPrimitive> = prims.iter().filter(|p| p.visible).collect();
            let denomf = if res > 1 { (res - 1) as f32 } else { 1.0 };
            let extent = gmax - gmin;
            let last = res - 1;
            let v0 = Vec3::new((bx * bd).min(last) as f32, (by * bd).min(last) as f32, (bz * bd).min(last) as f32);
            let v1 = Vec3::new((bx * bd + bd - 1).min(last) as f32, (by * bd + bd - 1).min(last) as f32, (bz * bd + bd - 1).min(last) as f32);
            let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
            for c in 0..9u32 {
                let gpt = if c < 8 {
                    Vec3::new(
                        if c & 1 != 0 { v1.x } else { v0.x },
                        if c & 2 != 0 { v1.y } else { v0.y },
                        if c & 4 != 0 { v1.z } else { v0.z },
                    )
                } else { (v0 + v1) * 0.5 };
                let d = core_math::field_eval::eval_scene_body_mirrored(
                    gmin + gpt / denomf * extent, &visible, mflags, 0, mc);
                mn = mn.min(d); mx = mx.max(d);
            }
            let msg = format!(
                "brick ({bx},{by},{bz}): cpu={:?} gpu={:?} | cpu mn={mn:.7} mx={mx:.7} \
                 thresh={:.7} (mn-thresh={:+.3e}, mx+thresh={:+.3e})",
                cpu[bi], g, band + margin, mn - (band + margin), mx + (band + margin)
            );
            eprintln!("[GpuClassify] MISMATCH {name}: {msg}");
        }
    }
    eprintln!(
        "[GpuClassify] {name}: verdict pass {gpu_ms:.2} ms for {total} bricks (CPU brute-force \
         {cpu_ms:.1} ms) | gpu: surface={n_sur} interior={n_int} exterior={n_ext} | mismatches={}",
        mismatches.len(),
    );
    // Non-vacuous coverage: every scene must exercise Surface + Exterior at minimum.
    assert!(n_sur > 0, "{name}: GPU classified zero Surface bricks — vacuous scene");
    assert!(n_ext > 0, "{name}: GPU classified zero Exterior bricks — vacuous scene");
    mismatches
}

/// ZERO-MISMATCH gate at production scale, 256³ (32³ = 32768 bricks — toy resolutions are
/// vacuous: the 2×diag margin swamps the geometry, see chunked_dc.rs test notes). Scenes use
/// shapes whose CPU and GPU point evals are the SAME function (sphere + elongation domain warp
/// + smooth/subtract blends + body mirror), so any verdict flip is a classifier-replication bug:
/// (1) multi-sphere with a Subtract, (2) a MIRRORED body, (3) an ELONGATED prim (elongation was
/// recently added to the classify-relevant hash — the domain warp must agree on both sides),
/// (4) a deep-interior sphere so the INTERIOR verdict is actually exercised.
///
/// Boxes are gated separately in `gpu_classify_box_euclidean_alignment_gate` (the CPU box eval
/// was tri-planar and diverged from the GPU exact-Euclidean `sdf_box`; aligned 2026-07-13).
#[test]
fn gpu_classify_verdicts_match_cpu_classifier() {
    let gpu = match classify_gpu() {
        Some(g) => g, None => { eprintln!("[GpuClassify] no GPU adapter — skipping"); return; }
    };
    let res = 256u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();

    // (1) Multi-prim with a Subtract (smooth carve) + a blended union arm.
    let multi = vec![
        sphere_op(Vec3::ZERO, 0.7, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
        sphere_op(Vec3::new(-0.6, 0.6, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
    ];
    let mm = check_scene(&gpu, "multi-subtract", &multi, 0, Vec3::ZERO, res, gmin, gmax, band);
    assert!(mm.is_empty(), "multi-subtract: {} per-brick verdict mismatches", mm.len());

    // (2) Mirrored body: off-center sphere on -X, mirrored across x=0 (bounds are the
    //     mirror-expanded domain, Body Mirror AABB rule).
    let mirrored = vec![sphere_op(Vec3::new(-0.8, 0.0, 0.0), 0.35, BooleanOp::Union { k: 0.0 })];
    let mm = check_scene(&gpu, "mirrored", &mirrored, 0b001, Vec3::ZERO, res, gmin, gmax, band);
    assert!(mm.is_empty(), "mirrored: {} per-brick verdict mismatches", mm.len());

    // (3) Elongated sphere (capsule-like domain warp) with a subtract bite.
    let mut elong = sphere_op(Vec3::ZERO, 0.4, BooleanOp::Union { k: 0.0 });
    elong.elongation = Vec3::new(0.7, 0.0, 0.0);
    let elongated = vec![
        elong,
        sphere_op(Vec3::new(0.0, 0.35, 0.0), 0.3, BooleanOp::Subtract { k: 0.05 }),
    ];
    let mm = check_scene(&gpu, "elongated", &elongated, 0, Vec3::ZERO, res, gmin, gmax, band);
    assert!(mm.is_empty(), "elongated: {} per-brick verdict mismatches", mm.len());

    // (4) Deep-interior sphere — deepest 9-sample ≈ −1.37, far past the 2×diag margin
    //     (−0.326), so the INTERIOR verdict path is exercised.
    let deep = vec![sphere_op(Vec3::ZERO, 1.45, BooleanOp::Union { k: 0.0 })];
    let mm = check_scene(&gpu, "deep-interior-sphere", &deep, 0, Vec3::ZERO, res, gmin, gmax, band);
    assert!(mm.is_empty(), "deep-interior-sphere: {} per-brick verdict mismatches", mm.len());
}

/// BOX ALIGNMENT GATE (was `gpu_classify_box_triplanar_divergence_pinned`): the CPU box SDF
/// (`SdfShape::Box::distance`) was TRI-PLANAR (max of three 2D plane distances — underestimated
/// the true distance in multi-positive corner regions by up to ~22%) while the GPU `sdf_box`
/// (modes==0) is exact Euclidean, flipping a handful of corner-shell bricks
/// CPU-Surface→GPU-Exterior (measured GPU−CPU up to +2.6e-2 on this scene). Aligned 2026-07-13:
/// the plain-box CPU eval now uses the same exact Euclidean form as the GPU
/// (`length(max(q,0)) + min(max-component,0)`), so box scenes must bit-agree like the curved
/// zero-gate scenes — ZERO per-brick verdict mismatches.
#[test]
fn gpu_classify_box_euclidean_alignment_gate() {
    let gpu = match classify_gpu() {
        Some(g) => g, None => { eprintln!("[GpuClassify] no GPU adapter — skipping"); return; }
    };
    let res = 256u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();
    let multi_box = vec![
        box_op(Vec3::ZERO, Vec3::splat(0.7), BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
        sphere_op(Vec3::new(-0.6, 0.6, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
    ];
    let mm = check_scene(&gpu, "box-alignment(gate)", &multi_box, 0, Vec3::ZERO, res, gmin, gmax, band);
    assert!(mm.is_empty(),
        "box-alignment: {} per-brick verdict mismatches — the CPU box eval no longer matches \
         the GPU exact-Euclidean `sdf_box` (check SdfShape::Box::distance fast path)", mm.len());
}

/// STAGE-2 e2e — the REAL production flow, hardware-gated: `trigger_bake` (sparse FULL arm,
/// `gpu_classify_ready`) queues a GPU classify record instead of the CPU worker → the SHARED
/// production encode (`encode_gpu_classify_dispatch`, the exact fn `dispatch_pending_gpu_
/// classifies` calls) dispatches the real `classify_bricks` pass at 512³ → the SHARED finalize
/// (`finalize_gpu_classify_verdicts` → `sparse_index_from_verdicts`) builds the index → the
/// request lands through the ONE landing point (`submit_classified_request` → `classify_tx` →
/// `poll_async_classifies`) into `primary_bake_requests`.
///
/// Gate: the GPU-classified index is EXACTLY EQUAL (bit-identical `index` vec + `allocated`)
/// to a CPU `classify_prims_to_sparse_index` of the request's own snapshot/bounds — on a
/// CURVED scene (spheres + subtract + smooth blend), where the spike proved ZERO verdict
/// divergence (boxes diverge by the pinned tri-planar-vs-Euclidean CPU/GPU eval gap and are
/// deliberately excluded; dilation+alloc determinism makes curved-scene equality bit-exact).
#[test]
fn gpu_classify_production_flow_index_matches_cpu() {
    use crate::body_volume_cache::bake_dispatch::{
        rigid_translate_tests::sparse_cache, sparse_classify_band, SPARSE_BRICK_DIM,
    };
    use crate::app_poll_bake::{decode_gpu_finalized_index, encode_gpu_classify_dispatch};

    let gpu = match classify_gpu() {
        Some(g) => g, None => { eprintln!("[GpuClassify] no GPU adapter — skipping"); return; }
    };

    // Curved sparse-eligible scene (the spike's zero-gate shape family).
    let prims = vec![
        sphere_op(Vec3::ZERO, 0.7, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
        sphere_op(Vec3::new(-0.6, 0.6, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
    ];
    let (sg, nid) = make_body(prims.clone(), 0, Vec3::ZERO);
    let mut cache = sparse_cache(nid, (Vec3::splat(-1.5), Vec3::splat(1.5)));

    // Production regime: GPU present → the sparse FULL arm queues, never spawns the worker.
    cache.gpu_classify_ready = true;
    cache.mark_body_dirty(nid);
    cache.clear_bake_debounce();
    cache.trigger_bake(&sg, None);
    assert!(cache.primary_bake_requests.is_empty(), "FULL classify must not push synchronously");
    assert_eq!(cache.classify_pending, 1, "one classify flight counted");
    assert_eq!(cache.pending_gpu_classify.len(), 1, "GPU regime queues instead of spawning");
    assert!(cache.in_flight.iter().any(|(b, _)| *b == nid), "gate held for the flight");

    let q = cache.pending_gpu_classify.pop().unwrap();
    let res = q.request.resolution[0];
    assert!(res >= 512, "sparse FULL arm at production scale (got {res})");
    let (bmin, bmax) = (q.request.bounds_min, q.request.bounds_max);
    let generation = q.request.generation;
    let local_prims = q.local_prims.clone();

    // The REAL production dispatch (identity l2w: no MeshVolume operand in this scene).
    let pending = encode_gpu_classify_dispatch(
        &gpu.device, &gpu.queue, &gpu.pipeline, &gpu.bgl, &{
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("classify_tex_dummy"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING, view_formats: &[],
            })
        },
        q.request, crate::gpu_jobs::IDENTITY_MAT4,
        // STAGE-3: production regime — dilate+alloc on-device, ONE index readback.
        Some(&gpu.finalize),
    );
    gpu.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    assert_eq!(pending.map_ready.peek(), crate::map_state::OK, "index readback must map OK");
    assert!(pending.gpu_finalized, "finalize pipelines present → GPU-finalized flight");
    let raw: Vec<u32> = {
        let view = pending.staging.slice(..).get_mapped_range();
        bytemuck::cast_slice(&view).to_vec()
    };
    pending.staging.unmap();
    assert_eq!(raw.len(), pending.total_bricks + 1, "index tags + trailing allocated count");

    // Production decode/validate (timed — the frame-thread cost that replaced the worker
    // finalize on this path).
    let t_fin = Instant::now();
    let si = decode_gpu_finalized_index(&raw, pending.dims_bricks)
        .expect("GPU-finalized index failed structural validation on a healthy adapter");
    let fin_ms = t_fin.elapsed().as_secs_f64() * 1000.0;

    // CPU oracle on the SAME snapshot/bounds/band — exact index equality on a curved scene.
    let band = sparse_classify_band(bmin, bmax, res);
    let t_cpu = Instant::now();
    let cpu = core_math::chunked_dc::classify_prims_to_sparse_index(
        &local_prims, 0, Vec3::ZERO, bmin, bmax, res, SPARSE_BRICK_DIM, band,
    );
    let cpu_ms = t_cpu.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[GpuClassify] e2e 512³: gpu-finalized validate={fin_ms:.1}ms (cpu full \
               classify={cpu_ms:.1}ms) bricks={}/{}", si.allocated, si.index.len());
    assert!(si.allocated > 0, "anti-vacuity: curved scene allocates surface bricks");
    assert_eq!(si.dims_bricks, cpu.dims_bricks);
    assert_eq!(si.allocated, cpu.allocated, "allocation count diverged");
    assert_eq!(si.index, cpu.index, "GPU-classified + GPU-FINALIZED index must be BIT-IDENTICAL \
        to the CPU classify on a curved scene (zero verdict divergence + deterministic \
        on-device dilation/alloc in the CPU's scan order)");

    // Land through the ONE landing point — same channel + validation as the CPU worker.
    let mut request = pending.request;
    request.sparse = Some(crate::body_volume_cache::SparseBakeRequest::Full(si));
    cache.submit_classified_request(request);
    let landed = cache.poll_async_classifies();
    assert_eq!(landed, 1, "completed GPU classify lands into primary_bake_requests");
    assert_eq!(cache.primary_bake_requests.len(), 1);
    assert_eq!(cache.primary_bake_requests[0].generation, generation);
    assert_eq!(cache.classify_pending, 0, "flight accounting balanced");
}

/// O1 COARSE-CULL PARITY GATE (2026-07-17 — beveled-subtract stripes diagnostic). O1 (74778de)
/// shipped with NO test: every gate in this file ran `params.coarse_cull = 0`, while production
/// sets 1 for every `coarse_cull_safe` scene (which INCLUDES per-edge-beveled boxes — the
/// tri-planar bevel is 1-Lipschitz, so it does not disqualify). O1's own contract is
/// "BIT-IDENTICAL to the fine result by construction" — assert exactly that, on the canonical
/// zero-gate scenes AND the owner's beveled-subtract shape, cull ON vs the CPU brute force.
#[test]
fn gpu_classify_coarse_cull_matches_cpu_classifier() {
    let gpu = match classify_gpu() {
        Some(g) => g, None => { eprintln!("[GpuClassify] no GPU adapter — skipping"); return; }
    };
    let res = 256u32;
    let (gmin, gmax) = (Vec3::splat(-1.5), Vec3::splat(1.5));
    let band = 1.5 * ((gmax - gmin) / (res - 1) as f32).max_element();

    // (1) The multi-subtract canonical scene (biggest sentinel population → most culls).
    let multi = vec![
        sphere_op(Vec3::ZERO, 0.7, BooleanOp::Union { k: 0.0 }),
        sphere_op(Vec3::new(0.7, 0.0, 0.0), 0.5, BooleanOp::Subtract { k: 0.1 }),
        sphere_op(Vec3::new(-0.6, 0.6, 0.0), 0.35, BooleanOp::Union { k: 0.25 }),
    ];
    let mm = check_scene_coarse(&gpu, "coarse-multi-subtract", &multi, 0, Vec3::ZERO, res, gmin, gmax, band, true);
    assert!(mm.is_empty(), "coarse-multi-subtract: {} verdict mismatches with the O1 cull ON", mm.len());

    // (2) Deep interior — the INTERIOR side of the coarse cull.
    let deep = vec![sphere_op(Vec3::ZERO, 1.45, BooleanOp::Union { k: 0.0 })];
    let mm = check_scene_coarse(&gpu, "coarse-deep-interior", &deep, 0, Vec3::ZERO, res, gmin, gmax, band, true);
    assert!(mm.is_empty(), "coarse-deep-interior: {} verdict mismatches with the O1 cull ON", mm.len());

    // (3) The owner's scene shape: box body + per-edge-beveled cube SUBTRACT (mixed Round radii
    //     + one Chamfer — the tri-planar eval whose magnitude UNDERESTIMATES in corner regions),
    //     plus a second union arm so per-prim-cull windows exist near the beveled prim.
    use core_math::sdf_shapes::{BoxBevel, BevelMode};
    let mut bevel = BoxBevel::default();
    bevel.radii[8] = 0.15; bevel.modes[8] = BevelMode::Round;
    bevel.radii[9] = 0.35; bevel.modes[9] = BevelMode::Round;
    bevel.radii[10] = 0.25; bevel.modes[10] = BevelMode::Round;
    bevel.radii[11] = 0.45; bevel.modes[11] = BevelMode::Round;
    bevel.radii[0] = 0.3; bevel.modes[0] = BevelMode::Round;
    bevel.radii[1] = 0.3; bevel.modes[1] = BevelMode::Chamfer;
    bevel.radii[2] = 0.3; bevel.modes[2] = BevelMode::Round;
    bevel.radii[3] = 0.3; bevel.modes[3] = BevelMode::Round;
    let mut bev_sub = SdfPrimitive::new(
        "bev".into(), SdfShape::Box { half_extents: Vec3::splat(0.55), bevel }, Vec3::new(0.5, 0.5, 0.5));
    bev_sub.operation = BooleanOp::Subtract { k: 0.0 };
    let beveled = vec![
        box_op(Vec3::ZERO, Vec3::splat(0.8), BooleanOp::Union { k: 0.0 }),
        bev_sub,
        sphere_op(Vec3::new(-1.0, -1.0, -1.0), 0.25, BooleanOp::Union { k: 0.0 }),
    ];
    let mm = check_scene_coarse(&gpu, "coarse-beveled-subtract", &beveled, 0, Vec3::ZERO, res, gmin, gmax, band, true);
    assert!(mm.is_empty(), "coarse-beveled-subtract: {} verdict mismatches with the O1 cull ON", mm.len());
}
