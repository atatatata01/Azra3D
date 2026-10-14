// Headless GPU readback proof for the per-light VOLUMETRIC in-scatter (god rays),
// the numeric proof flagged as the KNOWN GAP in commit 4473a8e (volumetric lights).
//
// The full `pass_fog` entry depends on the entire sdf_shared stack (camera_ray,
// the prim buffer, the fog config uniform, scene_sdf over the voxel cache, temporal
// reprojection textures) — a full-pass probe is impractical, so this follows the
// round-3 shadow-harness precedent: the production scattering functions
// (`mie_phase`, `fog_shadow`, `vl_*`, `vol_light_inscatter`) are extracted VERBATIM
// from `fog_entry.wgsl` at test time (sliced between two stable comment markers) and
// wrapped in a minimal harness that provides only their four external dependencies:
//   * `config.num_primitives` + `config.fog_phase_g` (a 2-field uniform),
//   * `light_data` (the REAL multi-light buffer — count header + `LightData::
//     serialize_to_gpu()`, so this also exercises the 20-float CPU→GPU contract),
//   * `scene_sdf` (a box occluder placed between one probe and the light).
// Because the functions are sliced from the shipped source, any future edit to the
// production math is picked up here automatically.
//
// Scene: one SPOT light at (0,6,0) aimed straight down, 40°/50° cone, zone 10.
// Probes (all view ray +Z):
//   BEAM  (0, 2,0): on-axis, in-cone, in-zone, CLEAR line to the light  → in-scatter > 0
//   OCC   (2.5,2,0): in-cone, in-zone, but a box occluder sits on its shadow
//                    ray to the light                                    → DARK (≈0)
//   OUT   (0,-5,0): 11 units from the light, past the zone-10 reach      → exactly 0
// Asserts: (a) BEAM > 0 on every channel, (b) OCC ≈ 0 and ≪ BEAM (the inverted-
// shadow guard: an inverted shadow term would make OCC the BRIGHT sample), (c) OUT
// == 0, (d) determinism — two dispatches byte-identical.
//
// Skips gracefully when no GPU adapter is present (headless CI).

use core_math::scene_graph::{LightData, LightVolumetric};

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
            label: Some("vol_light_test_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()?;
    // A validation error silently drops the submission → zero-initialized output that
    // LOOKS like "no scattering". Fail loudly instead.
    device.on_uncaptured_error(Box::new(|e| panic!("[VOL-GPU] wgpu error: {e}")));
    Some((device, queue))
}

/// Slice the production scattering functions out of the shipped fog shader, verbatim.
/// Range: `// --- Mie phase ...` through (exclusive) `// --- Temporal reprojection ...`.
/// That block is exactly `mie_phase` + `fog_shadow` + the `VL_STRIDE`/`vl_*` accessors
/// + `vol_light_inscatter` — the entire per-light volumetric math, unmodified.
fn extract_vol_functions() -> String {
    let src = crate::FOG_ENTRY_WGSL;
    let start_marker = "// --- Mie phase function";
    let end_marker = "// --- Temporal reprojection";
    let start = src.find(start_marker).expect("mie_phase marker moved in fog_entry.wgsl");
    let end = src.find(end_marker).expect("temporal-reprojection marker moved in fog_entry.wgsl");
    assert!(start < end, "fog_entry.wgsl markers out of order");
    let block = &src[start..end];
    // Sanity: the slice must actually contain the function under test.
    assert!(block.contains("fn vol_light_inscatter"), "verbatim slice missed vol_light_inscatter");
    assert!(block.contains("fn fog_shadow"), "verbatim slice missed fog_shadow");
    assert!(block.contains("fn mie_phase"), "verbatim slice missed mie_phase");
    block.to_string()
}

// Occluder box — MUST match the WGSL scene_sdf below. Centered on OCC's shadow ray
// to the light, clear of BEAM's straight-up shadow column (BEAM is at x=0; the box
// spans x∈[0.45,2.05]).
const OCC_C: [f32; 3] = [1.25, 4.0, 0.0];
const OCC_H: [f32; 3] = [0.8, 0.8, 0.8];

fn harness_wgsl() -> String {
    let vol = extract_vol_functions();
    format!(
        r#"
// Minimal config: fields read by fog_shadow / vol_light_inscatter / fog_shadow_depth.
// render_quality = 3 forces the SDF shadow path (depth probes need a real G-buffer).
// fog_has_vol_lights = bitfield (bit 0 = light 0 has VOL).
struct Config {{
    num_primitives: u32,
    fog_phase_g: f32,
    render_quality: u32,
    fog_has_vol_lights: u32,
    // post_flags: vol_point_atten branches on bit 1024 (physical falloff). The
    // harness pins 0 = legacy falloff, keeping the expected values unchanged.
    post_flags: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
    cam_pos: vec4<f32>,
    cam_forward: vec4<f32>,
    cam_right: vec4<f32>,
    cam_up: vec4<f32>,
}};
@group(0) @binding(0) var<uniform> config: Config;
@group(0) @binding(1) var<storage, read> light_data: array<f32>;
@group(0) @binding(2) var<storage, read> probes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> out_buf: array<vec4<f32>>;
// Dummy G-buffer so fog_shadow_depth parses (SDF path is used at quality ≥ 3).
@group(0) @binding(4) var gbuf_normal_in: texture_2d<f32>;

// Box occluder — the ONLY geometry scene_sdf sees. Signed-distance to an axis box.
fn scene_sdf(p: vec3<f32>) -> f32 {{
    let c = vec3<f32>({cx}, {cy}, {cz});
    let h = vec3<f32>({hx}, {hy}, {hz});
    let q = abs(p - c) - h;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}}

{vol}

@compute @workgroup_size(64, 1, 1)
fn probe_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= arrayLength(&probes)) {{ return; }}
    let p = probes[i].xyz;
    // All probes use a horizontal +Z view ray (keeps the Mie phase identical across
    // probes so shadow/reach are the only discriminators).
    let ray_dir = vec3<f32>(0.0, 0.0, 1.0);
    let r = vol_light_inscatter(p, ray_dir, 32u);
    out_buf[i] = vec4<f32>(r, 0.0);
}}
"#,
        vol = vol,
        cx = OCC_C[0], cy = OCC_C[1], cz = OCC_C[2],
        hx = OCC_H[0], hy = OCC_H[1], hz = OCC_H[2],
    )
}

/// Build the REAL light buffer: count header (`f32::from_bits(n)`) + 25 floats per
/// light from `LightData::serialize_to_gpu()` — bit-identical to `serialize_lights_gpu`.
fn build_light_buffer() -> Vec<f32> {
    let mut spot = LightData::new_spot(1, "vol_spot".into(), glam::Vec3::new(0.0, 6.0, 0.0));
    spot.azimuth = 0.0;
    spot.elevation = -90.0; // direction() → (0,-1,0): straight-down aim
    spot.intensity = 5.0;
    spot.color = [1.0, 1.0, 1.0];
    spot.spot_inner_deg = 40.0;
    spot.spot_outer_deg = 50.0;
    spot.range = 15.0;
    spot.volumetric = LightVolumetric { enabled: true, density: 1.0, softness: 0.3, zone: 10.0 };

    // Verify direction() really is straight down (the harness geometry depends on it).
    let d = spot.direction();
    assert!(d[1] < -0.999, "spot aim must be straight down, got {:?}", d);

    let g = spot.serialize_to_gpu();
    assert_eq!(g.len(), 25);
    let mut buf = Vec::with_capacity(1 + 25);
    buf.push(f32::from_bits(1u32)); // one light
    buf.extend_from_slice(&g);
    buf
}

fn run_probes(device: &wgpu::Device, queue: &wgpu::Queue, probes: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let light = build_light_buffer();

    let light_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vol_light_data"),
        size: (light.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&light_buf, 0, bytemuck::cast_slice(&light));

    // Config: num_primitives=1, fog_phase_g=0.6, render_quality=3 (SDF shadow path).
    // Layout: u32,f32,u32,u32 + 4×vec4 camera (cam at origin looking +Z).
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct HarnessConfig {
        num_primitives: u32,
        fog_phase_g: f32,
        render_quality: u32,
        fog_has_vol_lights: u32,
        post_flags: u32,
        _p0: u32,
        _p1: u32,
        _p2: u32,
        cam_pos: [f32; 4],
        cam_forward: [f32; 4],
        cam_right: [f32; 4],
        cam_up: [f32; 4],
    }
    let config = HarnessConfig {
        num_primitives: 1,
        fog_phase_g: 0.6,
        render_quality: 3,
        fog_has_vol_lights: 1, // bit 0 → light index 0 is volumetric
        post_flags: 0, // legacy falloff — expected values below assume it
        _p0: 0,
        _p1: 0,
        _p2: 0,
        cam_pos: [0.0, 0.0, 0.0, 0.0],
        cam_forward: [0.0, 0.0, 1.0, 1.0],
        cam_right: [1.0, 0.0, 0.0, 0.0],
        cam_up: [0.0, 1.0, 0.0, 0.0],
    };
    let cfg_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vol_config"),
        size: std::mem::size_of::<HarnessConfig>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&cfg_buf, 0, bytemuck::bytes_of(&config));

    // 1×1 dummy G-buffer (depth in .w = 0 → sky) for fog_shadow_depth parse path.
    let gbuf_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vol_dummy_gbuf"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let gbuf_view = gbuf_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let probe_flat: Vec<f32> = probes.iter().flat_map(|p| *p).collect();
    let probe_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vol_probes"),
        size: (probe_flat.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&probe_buf, 0, bytemuck::cast_slice(&probe_flat));

    let out_bytes = (probes.len() * 4 * 4) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vol_out"),
        size: out_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let uni = |b: u32, kind: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: kind, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("vol_bgl"),
        entries: &[
            uni(0, wgpu::BufferBindingType::Uniform),
            uni(1, wgpu::BufferBindingType::Storage { read_only: true }),
            uni(2, wgpu::BufferBindingType::Storage { read_only: true }),
            uni(3, wgpu::BufferBindingType::Storage { read_only: false }),
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vol_probe_module"),
        source: wgpu::ShaderSource::Wgsl(harness_wgsl().into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("vol_probe"),
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        })),
        module: &module,
        entry_point: Some("probe_main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("vol_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: cfg_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: light_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: probe_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: out_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&gbuf_view) },
        ],
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vol"), timestamp_writes: None });
        cp.set_pipeline(&pipeline);
        cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups(((probes.len() as u32) + 63) / 64, 1, 1);
    }
    queue.submit([enc.finish()]);

    let rb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vol_rb"),
        size: out_bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&out_buf, 0, &rb, 0, out_bytes);
    queue.submit([enc.finish()]);

    let (tx, rx) = std::sync::mpsc::channel();
    rb.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map_async channel").expect("vol readback map failed");
    let data = rb.slice(..).get_mapped_range();
    let floats: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
    drop(data);
    rb.unmap();

    floats.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
}

#[test]
fn vol_light_spot_beam_through_occluder_probe() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[VOL-GPU] no adapter — skipping");
        return;
    };

    // idx 0 = BEAM (lit), 1 = OCC (shadowed), 2 = OUT (past zone)
    let probes = [
        [0.0f32, 2.0, 0.0, 0.0],  // BEAM
        [2.5, 2.0, 0.0, 0.0],     // OCC
        [0.0, -5.0, 0.0, 0.0],    // OUT (11 units from light @ y=6, zone=10)
    ];

    let run_a = run_probes(&device, &queue, &probes);
    let run_b = run_probes(&device, &queue, &probes);

    // (d) Determinism — vol_light_inscatter has no RNG; bytes must be identical.
    for (a, b) in run_a.iter().zip(run_b.iter()) {
        assert_eq!(a, b, "volumetric in-scatter must be deterministic across dispatches");
    }

    let beam = run_a[0];
    let occ = run_a[1];
    let out = run_a[2];
    let lum = |v: &[f32; 4]| v[0] + v[1] + v[2];
    eprintln!(
        "[VOL-GPU] BEAM={:?} (lum {:.5})  OCC={:?} (lum {:.5})  OUT={:?} (lum {:.5})",
        &beam[..3], lum(&beam), &occ[..3], lum(&occ), &out[..3], lum(&out)
    );

    // (a) Unshadowed beam sample accumulates real in-scatter on every channel.
    assert!(
        beam[0] > 1e-3 && beam[1] > 1e-3 && beam[2] > 1e-3,
        "(a) beam sample must scatter light (>0 per channel), got {:?}", &beam[..3]
    );

    // (b) Sample behind the occluder is DARK. This is the inverted-shadow guard:
    //     OCC is in-cone and in-zone (reach≈1) exactly like BEAM — ONLY the shadow
    //     term differs. If the shadow were inverted, OCC would be the BRIGHT sample.
    assert!(
        lum(&occ) < 0.02,
        "(b) occluded sample must be near-dark, got lum {:.5} {:?}", lum(&occ), &occ[..3]
    );
    assert!(
        lum(&occ) < lum(&beam) * 0.1,
        "(b) occluded sample must be far darker than the lit beam: occ {:.5} vs beam {:.5}",
        lum(&occ), lum(&beam)
    );

    // (c) Sample past the zone radius contributes nothing (radial reach → 0).
    assert!(
        lum(&out) < 1e-5,
        "(c) sample past the zone reach must be ~0, got lum {:.6} {:?}", lum(&out), &out[..3]
    );
}
