// Headless GPU readback proof for `env_prefilter.wgsl` (split-sum IBL first term).
//
// Dispatches the REAL production shader through the REAL production BGL
// (`bgl::env_prefilter_bgl_entries` — the test bind group must mirror PRODUCTION wiring,
// the lab-cache binding-collision lesson) against a small equirect env buffer laid out
// exactly like the app's `env_buffer` ([7 mips][SH9 tail]), then reads back:
//
//   1. CONSTANT env, any roughness → the prefiltered mip is that constant (energy:
//      GGX importance weights are normalized by ΣNoL — a uniform radiance field must
//      pass through unchanged). Also proves the dst_offset addressing hits ITS mip
//      and nothing else (level 0 and the SH tail are asserted untouched).
//   2. Top-bright env at roughness 1.0 → up-facing texels stay brighter than
//      down-facing ones (the lobe blurs but never flips the field).
//
// Skips gracefully when no GPU adapter is present (headless CI).

use crate::ENV_PREFILTER_WGSL;

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
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("env_prefilter_test_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

/// Mip dims with the WGSL/env_ibl halving convention.
fn mip_dims(w: u32, h: u32, level: u32) -> (u32, u32) {
    let (mut mw, mut mh) = (w, h);
    for _ in 0..level {
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
    }
    (mw, mh)
}

fn chain_len_floats(w: u32, h: u32) -> usize {
    (0..7).map(|l| { let (a, b) = mip_dims(w, h, l); (a * b * 3) as usize }).sum()
}

/// Run one prefilter dispatch on `env_floats` (the full [chain][sh] image) and return the
/// buffer read back. Mirrors app/src/env_ibl.rs `dispatch_env_prefilter_step` exactly.
fn run_prefilter(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    env_floats: &[f32],
    src_w: u32,
    src_h: u32,
    level: u32,
    roughness: f32,
    samples: u32,
) -> Vec<f32> {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("env_prefilter_bgl_test"),
        entries: &crate::bgl::env_prefilter_bgl_entries(),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("env_prefilter_test"),
        source: wgpu::ShaderSource::Wgsl(ENV_PREFILTER_WGSL.into()),
    });
    let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("env_prefilter_test"),
        layout: Some(&pl),
        module: &shader,
        entry_point: Some("prefilter_mip"),
        compilation_options: Default::default(),
        cache: None,
    });

    let env_bytes: Vec<u8> = env_floats.iter().flat_map(|v| v.to_le_bytes()).collect();
    let env_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("env_test"),
        size: env_bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    queue.write_buffer(&env_buf, 0, &env_bytes);

    let (dw, dh) = mip_dims(src_w, src_h, level);
    let mut offset = 0u32;
    for l in 0..level {
        let (mw, mh) = mip_dims(src_w, src_h, l);
        offset += mw * mh * 3;
    }
    let params: [u32; 8] = [src_w, src_h, offset, dw, dh, samples, roughness.to_bits(), 0];
    let params_bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("env_prefilter_params_test"),
        size: 32,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, &params_bytes);

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: env_buf.as_entire_binding() },
        ],
    });

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("env_test_staging"),
        size: env_bytes.len() as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
        cp.set_pipeline(&pipe);
        cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups(dw.div_ceil(8), dh.div_ceil(8), 1);
    }
    enc.copy_buffer_to_buffer(&env_buf, 0, &staging, 0, env_bytes.len() as u64);
    queue.submit(Some(enc.finish()));

    // map_async result captured via channel (GPU Safety rule — never discard with |_| {}).
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map_async callback dropped").expect("staging map failed");
    let out: Vec<f32> = staging
        .slice(..)
        .get_mapped_range()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    staging.unmap();
    out
}

#[test]
fn env_prefilter_gpu_constant_and_directional_probe() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[SKIP] env_prefilter GPU probe: no adapter");
        return;
    };
    let (w, h) = (32u32, 16u32);
    let chain = chain_len_floats(w, h);
    let total = chain + 27;

    // ---- Probe 1: constant env → prefiltered mip stays constant; other regions untouched.
    let mut env = vec![0.0f32; total];
    for v in env[..(w * h * 3) as usize].iter_mut() { *v = 0.5; }
    // Poison the SH tail with a sentinel to prove the dispatch never writes it.
    for v in env[chain..].iter_mut() { *v = 7.25; }
    let level = 2u32;
    let out = run_prefilter(&device, &queue, &env, w, h, level, 2.0 / 6.0, 64);
    let (dw, dh) = mip_dims(w, h, level);
    let mut off = 0usize;
    for l in 0..level { let (a, b) = mip_dims(w, h, l); off += (a * b * 3) as usize; }
    for i in 0..(dw * dh * 3) as usize {
        let v = out[off + i];
        assert!((v - 0.5).abs() < 1e-3, "constant env drifted in prefiltered mip: {v} at {i}");
    }
    // Level 0 untouched (mip 0 == source, by construction)
    for i in 0..(w * h * 3) as usize {
        assert!((out[i] - 0.5).abs() < 1e-6, "level 0 was written by the prefilter");
    }
    // SH tail untouched
    for v in &out[chain..] {
        assert!((v - 7.25).abs() < 1e-6, "SH tail was written by the prefilter");
    }

    // ---- Probe 2: top-bright env at roughness 1.0 keeps its verticality.
    let mut env2 = vec![0.0f32; total];
    for y in 0..h as usize {
        let val = if y < (h as usize) / 2 { 1.0 } else { 0.05 };
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 3;
            env2[i] = val; env2[i + 1] = val; env2[i + 2] = val;
        }
    }
    let level2 = 3u32; // 4×2 mip — deep enough to blur, still has distinct up/down rows
    let out2 = run_prefilter(&device, &queue, &env2, w, h, level2, 1.0, 128);
    let (dw2, dh2) = mip_dims(w, h, level2);
    let mut off2 = 0usize;
    for l in 0..level2 { let (a, b) = mip_dims(w, h, l); off2 += (a * b * 3) as usize; }
    let top = out2[off2]; // first row = up-facing directions
    let bottom = out2[off2 + ((dh2 - 1) * dw2 * 3) as usize];
    assert!(
        top > bottom + 0.05,
        "roughness-1 prefilter lost directionality: top={top} bottom={bottom}"
    );
    eprintln!("[GPU] env_prefilter probe OK (constant pass-through + directional r=1: top={top:.3} bottom={bottom:.3})");
}
