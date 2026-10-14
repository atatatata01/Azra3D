// Headless GPU readback proof for `pass_ssr` (SSR quality pass, 2026-07-18) —
// the shadow/SSAO standard: dispatch the REAL production shader
// (`process_post_process_wgsl`, the exact module the app compiles) through a
// bind group that MIRRORS the production `ssr_bgl` wiring (app_new.rs:
// uni 1, hdr 5, gbuf_normal 12, gbuf_material 13, gbuf_albedo 14,
// ssr_output 15, env_data 21, ssao 38, brdf_lut 39 — the lab-cache binding-collision
// lesson: test bind groups must mirror PRODUCTION wiring).
//
// Scene: a red cube sitting on a mirror floor (metal 1, rough 0.05), G-buffer
// synthesized on the CPU with the SAME camera math the shader uses (vectors
// read back from `to_gpu_uniforms()` — convention-proof). Proves:
//   1. Hit confidence > 0 where the cube's reflection should land on the floor
//      (CPU mirror-trace oracle), and the reflected color is red-dominant.
//   2. Zero confidence where nothing reflects (sky-mirroring floor + background).
//   3. Determinism: two dispatches produce byte-identical output (per-pixel
//      hash jitter only — no temporal jitter).
//   4. The delta output is non-negative with env off (energy sanity:
//      delta = envelope × conf × (traced − 0)).
// Plus an (informational) 1920×1080 wall-clock timing block.
//
// Skips gracefully when no GPU adapter is present (headless CI).

use half::f16;

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
            label: Some("ssr_test_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()?;
    // A validation error silently drops the submission → zero-initialized output
    // that LOOKS like "no hits". Fail loudly instead.
    device.on_uncaptured_error(Box::new(|e| panic!("[SSR-GPU] wgpu error: {e}")));
    Some((device, queue))
}

// ── CPU scene: mirror floor + red cube (analytic, matches the shader's camera) ──

const FLOOR_Y: f32 = -1.0;
const CUBE_MIN: [f32; 3] = [-0.5, -1.0, -0.5];
const CUBE_MAX: [f32; 3] = [0.5, 0.0, 0.5];
const FLOOR_ALBEDO: [f32; 3] = [0.9, 0.9, 0.9];
const CUBE_HDR: [f32; 3] = [0.8, 0.1, 0.1]; // red — the color SSR must pick up
const FLOOR_HDR: [f32; 3] = [0.25, 0.25, 0.28];

struct Hit {
    t: f32,
    n: [f32; 3],
    /// 0 = floor, 1 = cube
    obj: u32,
}

fn ray_box(o: [f32; 3], d: [f32; 3]) -> Option<f32> {
    let mut t0 = 1e-4f32;
    let mut t1 = f32::INFINITY;
    for a in 0..3 {
        if d[a].abs() < 1e-8 {
            if o[a] < CUBE_MIN[a] || o[a] > CUBE_MAX[a] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / d[a];
        let (mut ta, mut tb) = ((CUBE_MIN[a] - o[a]) * inv, (CUBE_MAX[a] - o[a]) * inv);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

fn box_normal(p: [f32; 3]) -> [f32; 3] {
    // Face with the smallest distance to the hit point
    let dists = [
        (p[0] - CUBE_MIN[0]).abs(),
        (CUBE_MAX[0] - p[0]).abs(),
        (p[1] - CUBE_MIN[1]).abs(),
        (CUBE_MAX[1] - p[1]).abs(),
        (p[2] - CUBE_MIN[2]).abs(),
        (CUBE_MAX[2] - p[2]).abs(),
    ];
    let normals = [
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
    ];
    let mut best = 0;
    for i in 1..6 {
        if dists[i] < dists[best] {
            best = i;
        }
    }
    normals[best]
}

fn trace(o: [f32; 3], d: [f32; 3]) -> Option<Hit> {
    let mut hit: Option<Hit> = None;
    if let Some(t) = ray_box(o, d) {
        let p = [o[0] + d[0] * t, o[1] + d[1] * t, o[2] + d[2] * t];
        hit = Some(Hit { t, n: box_normal(p), obj: 1 });
    }
    if d[1] < -1e-6 {
        let t = (FLOOR_Y - o[1]) / d[1];
        if t > 1e-4 && hit.as_ref().map_or(true, |h| t < h.t) {
            hit = Some(Hit { t, n: [0.0, 1.0, 0.0], obj: 0 });
        }
    }
    hit
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-8);
    [v[0] / l, v[1] / l, v[2] / l]
}

struct Scene {
    w: u32,
    h: u32,
    gu: crate::GpuUniforms,
    /// f16 texel data, 4ch per pixel
    gbuf_normal: Vec<f16>,
    gbuf_material: Vec<f16>,
    hdr: Vec<f16>,
    /// rgba16float linear albedo (production gbuf_albedo format)
    albedo: Vec<f16>,
    /// per-pixel: 0 = background, 1 = floor, 2 = cube
    kind: Vec<u8>,
    /// per-pixel oracle: floor pixel whose mirror ray hits the CUBE (expected SSR hit)
    expect_cube_refl: Vec<bool>,
}

/// Build the G-buffer with the SAME per-pixel camera-ray math as `pass_ssr`
/// (D4: `uv = (gid+0.5)/dim*2-1`, ray from `config.cam_*`), depth = RAY DISTANCE.
fn build_scene(w: u32, h: u32, ssr_steps: u32) -> Scene {
    let mut cfg = crate::RenderConfig::default();
    cfg.width = w as usize;
    cfg.height = h as usize;
    cfg.cam_orbit = glam::Quat::IDENTITY;
    cfg.cam_dist = 5.0;
    cfg.cam_target = glam::Vec3::new(0.0, 0.2, 0.0);
    cfg.env_width = 0; // no env: IBL term = 0 → SSR delta must be ≥ 0
    cfg.env_height = 0;
    cfg.ssr_max_steps = ssr_steps;
    cfg.ssr_intensity = 1.0;
    let gu = cfg.to_gpu_uniforms();

    let cam = [gu.cam_pos[0], gu.cam_pos[1], gu.cam_pos[2]];
    let fwd = [gu.cam_forward[0], gu.cam_forward[1], gu.cam_forward[2]];
    let right = [gu.cam_right[0], gu.cam_right[1], gu.cam_right[2]];
    let up = [gu.cam_up[0], gu.cam_up[1], gu.cam_up[2]];
    let fov_scale = if gu.cam_forward[3] <= 0.0 { 1.0 } else { gu.cam_forward[3] };
    let aspect = w as f32 / h as f32;

    let n_px = (w * h) as usize;
    let mut gn = vec![f16::from_f32(0.0); n_px * 4];
    let mut gm = vec![f16::from_f32(0.0); n_px * 4];
    let mut hdr = vec![f16::from_f32(0.0); n_px * 4];
    let mut alb = vec![f16::from_f32(0.0); n_px * 4];
    let mut kind = vec![0u8; n_px];
    let mut expect = vec![false; n_px];

    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            // Pixel-center NDC — must match pass_ssr D4 reconstruction.
            let uv_x = ((x as f32 + 0.5) / w as f32) * 2.0 - 1.0;
            let uv_y = -(((y as f32 + 0.5) / h as f32) * 2.0 - 1.0);
            let d = norm3([
                fwd[0] + right[0] * (uv_x * aspect * fov_scale) + up[0] * (uv_y * fov_scale),
                fwd[1] + right[1] * (uv_x * aspect * fov_scale) + up[1] * (uv_y * fov_scale),
                fwd[2] + right[2] * (uv_x * aspect * fov_scale) + up[2] * (uv_y * fov_scale),
            ]);
            let Some(hit) = trace(cam, d) else { continue };
            let p = [cam[0] + d[0] * hit.t, cam[1] + d[1] * hit.t, cam[2] + d[2] * hit.t];
            let (rough, metal, albedo, hdr_c) = if hit.obj == 0 {
                (0.05f32, 1.0f32, FLOOR_ALBEDO, FLOOR_HDR)
            } else {
                (0.4, 0.0, CUBE_HDR, CUBE_HDR)
            };
            gn[i * 4] = f16::from_f32(hit.n[0]);
            gn[i * 4 + 1] = f16::from_f32(hit.n[1]);
            gn[i * 4 + 2] = f16::from_f32(hit.n[2]);
            gn[i * 4 + 3] = f16::from_f32(hit.t); // RAY DISTANCE (production semantics)
            gm[i * 4] = f16::from_f32(rough);
            gm[i * 4 + 1] = f16::from_f32(metal);
            gm[i * 4 + 2] = f16::from_f32(1.0); // shadow factor
            gm[i * 4 + 3] = f16::from_f32(1.0); // opacity
            hdr[i * 4] = f16::from_f32(hdr_c[0]);
            hdr[i * 4 + 1] = f16::from_f32(hdr_c[1]);
            hdr[i * 4 + 2] = f16::from_f32(hdr_c[2]);
            hdr[i * 4 + 3] = f16::from_f32(1.0);
            alb[i * 4] = f16::from_f32(albedo[0]);
            alb[i * 4 + 1] = f16::from_f32(albedo[1]);
            alb[i * 4 + 2] = f16::from_f32(albedo[2]);
            alb[i * 4 + 3] = f16::from_f32(1.0);
            kind[i] = if hit.obj == 0 { 1 } else { 2 };

            // Oracle: on the floor, mirror the view ray about +Y and see whether it
            // hits the cube — that is where SSR must find a reflection.
            if hit.obj == 0 {
                let r = [d[0], -d[1], d[2]];
                let o2 = [p[0], p[1] + 1e-3, p[2]];
                if let Some(t2) = ray_box(o2, r) {
                    // Require the reflected hit point to be comfortably on-screen
                    // (project with the same camera) so we only demand hits SSR can see.
                    let hp = [o2[0] + r[0] * t2, o2[1] + r[1] * t2, o2[2] + r[2] * t2];
                    let rel = [hp[0] - cam[0], hp[1] - cam[1], hp[2] - cam[2]];
                    let z = rel[0] * fwd[0] + rel[1] * fwd[1] + rel[2] * fwd[2];
                    if z > 0.05 {
                        let sx = (rel[0] * right[0] + rel[1] * right[1] + rel[2] * right[2])
                            / (z * aspect * fov_scale);
                        let sy = (rel[0] * up[0] + rel[1] * up[1] + rel[2] * up[2]) / (z * fov_scale);
                        let su = (sx + 1.0) * 0.5;
                        let sv = (-sy + 1.0) * 0.5;
                        // Inside the screen-border fade band (0.08 uv) confidence is
                        // deliberately reduced — only demand hits well inside.
                        if su > 0.12 && su < 0.88 && sv > 0.12 && sv < 0.88 {
                            expect[i] = true;
                        }
                    }
                }
            }
        }
    }

    Scene { w, h, gu, gbuf_normal: gn, gbuf_material: gm, hdr, albedo: alb, kind, expect_cube_refl: expect }
}

// ── GPU dispatch through the production module + mirrored production BGL ──

struct SsrRun {
    /// rgba16float readback, decoded to f32, 4ch per pixel
    out: Vec<f32>,
    /// wall-clock per dispatch (submit → poll-complete), millis
    ms_per_dispatch: f64,
}

fn run_ssr(device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene, dispatches: u32) -> SsrRun {
    let (w, h) = (scene.w, scene.h);
    let tex = |label: &str, format: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let bind = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let gn_tex = tex("t_gn", wgpu::TextureFormat::Rgba16Float, bind);
    let gm_tex = tex("t_gm", wgpu::TextureFormat::Rgba16Float, bind);
    let hdr_tex = tex("t_hdr", wgpu::TextureFormat::Rgba16Float, bind);
    let ga_tex = tex("t_ga", wgpu::TextureFormat::Rgba16Float, bind); // production albedo format
    let out_tex = tex(
        "t_ssr_out",
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );

    let write_f16 = |t: &wgpu::Texture, data: &[f16]| {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
        queue.write_texture(
            t.as_image_copy(),
            &bytes,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    };
    write_f16(&gn_tex, &scene.gbuf_normal);
    write_f16(&gm_tex, &scene.gbuf_material);
    write_f16(&hdr_tex, &scene.hdr);
    write_f16(&ga_tex, &scene.albedo);

    let ub = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t_uniforms"),
        size: std::mem::size_of::<crate::GpuUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&ub, 0, scene.gu.as_bytes());
    // env off (env_width == 0) + LUT in Karis mode (env_lut_res == 0): the shader
    // never indexes these, but the production BGL binds them — mirror it.
    let env_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t_env"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t_lut"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });

    // BGL mirrors app_new.rs `ssr_bgl` entry-for-entry.
    let tex_entry = |b: u32| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let buf_ro = |b: u32| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ssr_bgl_test"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            tex_entry(5),
            tex_entry(12),
            tex_entry(13),
            tex_entry(14),
            wgpu::BindGroupLayoutEntry {
                binding: 15,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba16Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            buf_ro(21),
            // C1: SSAO factor (white-fill 1×1 → ssao_factor returns 1.0)
            tex_entry(38),
            buf_ro(39),
            // Auto reflection probe (Phase 3) — pass_ssr references these statically
            // (the tonemap_bgl 62/63/64 lesson: Naga's global-usage analysis ignores
            // the runtime `enabled` gate). Dummies below keep them inert.
            wgpu::BindGroupLayoutEntry {
                binding: 65,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 66,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 67,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    // Probe dummies: 1×1×6 black cube (enabled = 0 in params → probe_sample early-outs,
    // so the SSR numbers this test measures are untouched).
    let probe_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ssr_test_probe_cube"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 6 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let probe_view = probe_tex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    });
    let probe_samp = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let probe_params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ssr_test_probe_params"),
        size: 32,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    // C1: 1×1 white SSAO fill (r=1 ao, g=0 depth → ssao_factor early-outs to 1.0).
    let ssao_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ssr_test_ssao"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    {
        let white = half::f16::from_f32(1.0).to_bits();
        let zero = half::f16::from_f32(0.0).to_bits();
        // r=1, g=0, b=0, a=0 as f16 LE
        let mut px = [0u8; 8];
        px[0..2].copy_from_slice(&white.to_le_bytes());
        px[2..4].copy_from_slice(&zero.to_le_bytes());
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &ssao_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &px,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
    }
    let ssao_view = ssao_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("post_process_test"),
        source: wgpu::ShaderSource::Wgsl(crate::process_post_process_wgsl("rgba16float").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("ssr_test"),
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        })),
        module: &module,
        entry_point: Some("pass_ssr"),
        compilation_options: Default::default(),
        cache: None,
    });
    let views: Vec<wgpu::TextureView> = [&hdr_tex, &gn_tex, &gm_tex, &ga_tex, &out_tex]
        .iter()
        .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
        .collect();
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ssr_bg_test"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 1, resource: ub.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[0]) },
            wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(&views[1]) },
            wgpu::BindGroupEntry { binding: 13, resource: wgpu::BindingResource::TextureView(&views[2]) },
            wgpu::BindGroupEntry { binding: 14, resource: wgpu::BindingResource::TextureView(&views[3]) },
            wgpu::BindGroupEntry { binding: 15, resource: wgpu::BindingResource::TextureView(&views[4]) },
            wgpu::BindGroupEntry { binding: 21, resource: env_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 38, resource: wgpu::BindingResource::TextureView(&ssao_view) },
            wgpu::BindGroupEntry { binding: 39, resource: lut_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 65, resource: wgpu::BindingResource::TextureView(&probe_view) },
            wgpu::BindGroupEntry { binding: 66, resource: wgpu::BindingResource::Sampler(&probe_samp) },
            wgpu::BindGroupEntry { binding: 67, resource: probe_params.as_entire_binding() },
        ],
    });

    // Warm-up dispatch (pipeline/driver compile lands outside the timing loop)
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ssr_warm"), timestamp_writes: None });
        cp.set_pipeline(&pipeline);
        cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups((w + 7) / 8, (h + 7) / 8, 1);
    }
    queue.submit([enc.finish()]);
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");

    let t0 = std::time::Instant::now();
    for _ in 0..dispatches {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ssr_t"), timestamp_writes: None });
            cp.set_pipeline(&pipeline);
            cp.set_bind_group(0, &bg, &[]);
            cp.dispatch_workgroups((w + 7) / 8, (h + 7) / 8, 1);
        }
        queue.submit([enc.finish()]);
    }
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    let ms_per_dispatch = t0.elapsed().as_secs_f64() * 1000.0 / dispatches as f64;

    // Readback
    let bpr = w * 8; // rgba16float — multiple of 256 for the sizes used here
    assert!(bpr % 256 == 0, "test sizes must keep bytes_per_row 256-aligned");
    let rb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t_rb"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        out_tex.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &rb,
            layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    rb.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map_async channel").expect("SSR readback map failed");
    let data = rb.slice(..).get_mapped_range();
    let out: Vec<f32> = data
        .chunks_exact(2)
        .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
        .collect();
    drop(data);
    rb.unmap();
    SsrRun { out, ms_per_dispatch }
}

#[test]
fn ssr_gpu_cube_on_mirror_floor_probe() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[SSR-GPU] no adapter — skipping");
        return;
    };

    let (w, h) = (256u32, 256u32);
    let scene = build_scene(w, h, 64);
    // Scene sanity: the synthesized G-buffer must actually show the cube + floor
    let n_cube = scene.kind.iter().filter(|&&k| k == 2).count();
    let n_floor = scene.kind.iter().filter(|&&k| k == 1).count();
    assert!(n_cube > 500, "scene setup broken: only {} cube pixels", n_cube);
    assert!(n_floor > 2000, "scene setup broken: only {} floor pixels", n_floor);

    let run_a = run_ssr(&device, &queue, &scene, 1);
    let run_b = run_ssr(&device, &queue, &scene, 1);

    // 3. Determinism — byte-identical across dispatches (fixed per-pixel hash only)
    assert_eq!(run_a.out, run_b.out, "SSR output must be deterministic");

    let px = |i: usize| (&run_a.out[i * 4..i * 4 + 4], scene.kind[i]);
    let mut expected_hits = 0usize;
    let mut expected_found = 0usize;
    let mut sum_r = 0f64;
    let mut sum_b = 0f64;
    let mut bg_nonzero = 0usize;
    let mut sky_floor = 0usize;
    let mut sky_floor_conf = 0usize;
    let mut neg_components = 0usize;
    for i in 0..(w * h) as usize {
        let (v, kind) = px(i);
        let conf = v[3];
        if v[0] < -1e-3 || v[1] < -1e-3 || v[2] < -1e-3 {
            neg_components += 1; // env off → delta = envelope·conf·traced ≥ 0
        }
        if kind == 0 {
            // background: G-buffer depth 0 → the pass must write exactly 0
            if conf != 0.0 || v[0] != 0.0 {
                bg_nonzero += 1;
            }
        }
        if scene.expect_cube_refl[i] {
            expected_hits += 1;
            if conf > 0.0 {
                expected_found += 1;
                sum_r += v[0] as f64;
                sum_b += v[2] as f64;
            }
        } else if kind == 1 {
            // floor pixel whose mirror ray hits NOTHING (sky): SSR must report
            // no confidence (rays leave the screen / never cross the depth buffer).
            sky_floor += 1;
            if conf > 0.0 {
                sky_floor_conf += 1;
            }
        }
    }
    assert!(expected_hits > 300, "oracle produced too few expected-reflection pixels: {}", expected_hits);
    let hit_rate = expected_found as f64 / expected_hits as f64;
    eprintln!(
        "[SSR-GPU] {}x{}: cube_px={} floor_px={} expected_refl={} found={} ({:.1}%) sky_floor={} false_conf={} bg_nonzero={} neg={}",
        w, h, n_cube, n_floor, expected_hits, expected_found, hit_rate * 100.0, sky_floor, sky_floor_conf, bg_nonzero, neg_components
    );
    // 1. Reflection lands where the oracle says (DDA + refinement + thickness are
    //    conservative near the cube's silhouette — demand a strong majority).
    assert!(hit_rate > 0.7, "SSR found only {:.1}% of expected cube reflections", hit_rate * 100.0);
    // Reflected color is red-dominant (the cube is red)
    assert!(sum_r > sum_b * 1.5, "reflected color not red-dominant: r={:.3} b={:.3}", sum_r, sum_b);
    // 2. No reflection where nothing reflects: sky-mirroring floor stays clean
    //    (small tolerance: thickness can clip the cube silhouette's 1px halo).
    assert!(
        (sky_floor_conf as f64) < (sky_floor as f64) * 0.02 + 8.0,
        "false confidence on {} of {} sky-floor pixels",
        sky_floor_conf,
        sky_floor
    );
    // Background must be exactly zero
    assert_eq!(bg_nonzero, 0, "background pixels must write vec4(0)");
    // 4. Energy sanity with env off
    assert_eq!(neg_components, 0, "delta must be non-negative with env off");
}

#[test]
fn ssr_gpu_timing_1080p() {
    // Informational perf probe (wall-clock upper bound: submit→poll includes CPU
    // overhead; the in-app PASS_SSR GPU timestamp is the authoritative number).
    let Some((device, queue)) = try_device() else {
        eprintln!("[SSR-GPU] no adapter — skipping");
        return;
    };
    let scene = build_scene(1920, 1080, 48);
    let run = run_ssr(&device, &queue, &scene, 20);
    eprintln!(
        "[SSR-GPU] 1080p, 48 steps: {:.3} ms/dispatch (wall-clock upper bound, 20 dispatches)",
        run.ms_per_dispatch
    );
    // Generous ceiling: catches an accidental O(N²) regression, not a perf gate —
    // the 1.5ms budget is judged on the 4090 via the in-app profiler.
    assert!(run.ms_per_dispatch < 25.0, "SSR pass took {:.2} ms/dispatch — runaway cost", run.ms_per_dispatch);
}
