// core_engine/render/src/tests_triplanar.rs
// ============================================================
// Headless GPU probe: box (triplanar) material sampling
// ============================================================
//
// Mirrors the shadows.rs readback harness: the REAL WGSL is extracted VERBATIM from
// gbuffer.wgsl (the `// ---- Material Map Sampling ----` section — markers are load-bearing),
// compiled into a tiny compute probe against a real device, and the sampled values are read
// back and asserted. No copy that can drift; skips cleanly when no GPU adapter exists (CI).
//
// What is proven headlessly:
//   1. BOX PROJECTION AXIS INVARIANCE — for a +X-facing normal the sample depends only on the
//      local (y,z) coordinates: moving along x does not change the texel. This is the property
//      that makes the projection "stick" to body-local space (the fs wires in.local_position
//      here; that wiring is owner-verified, the sampling math is machine-verified).
//   2. CHECKER LOOKUP — known texel values land at the expected probe points.
//   3. WHITEOUT NORMAL BLEND, FLAT MAP = IDENTITY — a flat (0,0,1) tangent map returns the
//      geometric normal (within 8-bit + sRGB pre-encode quantization).
//   4. WHITEOUT SWIZZLE ORIENTATION — a +u-tilted tangent map bends the normal toward the
//      plane's ACTUAL u axis for each face (+X face: u=Y; +Z face: u=X) — pinning the
//      hand-derived swizzles (.zxy/.xzy/.xyz) that differ from the textbook p.zy convention.

#![cfg(test)]

use wgpu::util::DeviceExt;

const TEX: u32 = 8; // tiny per-layer resolution — texel math below assumes 8

#[test]
fn high_precision_height_preserves_bilinear_interpolation() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[height_probe] no GPU adapter — skipping");
        return;
    };
    for raw in [false, true] {
    for (left, right) in [(0x2000u16, 0x2100u16), (0x20f0, 0x2110), (0x8000, 0x8001), (0xfffe, 0xffff)] {
        if !raw && right - left == 1 { continue; }
        let heights = [left, right];
        let high = layer(|column, _| {
            let value = heights[usize::from(column > 0)];
            let byte = if raw { (value >> 8) as u8 } else { encode_srgb(f32::from(value >> 8) / 255.0) };
            [byte, byte, byte, 255]
        });
        let low = layer(|column, _| {
            let value = heights[usize::from(column > 0)];
            let byte = if raw { (value & 255) as u8 } else { encode_srgb(f32::from(value & 255) / 255.0) };
            [byte, byte, byte, 255]
        });
        let layers = [layer(|_, _| [255; 4]), high, low];
        let probes: Vec<_> = (0..=128)
            .map(|step| {
                let fraction = step as f32 / 128.0;
                (glam::Vec3::new((0.5 + fraction) / TEX as f32, 0.5 / TEX as f32, 0.0),
                 1, glam::Vec3::Z, if raw { 51 } else { 50 })
            })
            .collect();
        let samples = run_probes(&device, &queue, &layers, &probes, wgpu::FilterMode::Linear);
        let first = samples.first().unwrap()[0];
        let last = samples.last().unwrap()[0];
        assert!(last > first);
        if raw {
            assert!((first - f32::from(left) / 65535.0).abs() < 2e-7);
            assert!((last - f32::from(right) / 65535.0).abs() < 2e-7);
        }
        for (step, sample) in samples.iter().enumerate() {
            let expected = first + (last - first) * step as f32 / 128.0;
            assert!((sample[0] - expected).abs() < 2e-6,
                "height filtering stepped at sample {step}: {} != {expected}", sample[0]);
        }
        assert!(samples.windows(2).all(|pair| pair[1][0] > pair[0][0]),
            "filtered heights contain plateaus that become bump bands");
    }
    }
    let layers = [layer(|_, _| [255; 4])];
    let probes: Vec<_> = [1e-9, 1e-6, 1e-3, 1.0, 1000.0].into_iter()
        .flat_map(|scale| [-2.0, 0.0, 2.0].into_iter()
            .map(move |slope| (glam::Vec3::new(scale, slope, 0.25), 0, glam::Vec3::Z, 52)))
        .collect();
    let samples = run_probes(&device, &queue, &layers, &probes, wgpu::FilterMode::Linear);
    for (probe, sample) in probes.iter().zip(samples) {
        let expected = glam::Vec3::new(-probe.0.y, -0.25, 1.0).normalize();
        let actual = glam::Vec3::new(sample[0], sample[1], sample[2]);
        assert!((actual - expected).length() < 1e-5,
            "bump changed with projected pixel size: {actual:?} != {expected:?}");
    }
}

#[test]
fn height_reconstruction_matches_path_tracing() {
    let raster = include_str!("gbuffer.wgsl");
    let pathtrace = include_str!("../../graphics/src/pathtrace_tri_entry.wgsl");
    let raster_body = raster.split("fn reconstruct_u16_channel").nth(1).unwrap()
        .split_once('{').unwrap().1.split_once('}').unwrap().0;
    let pathtrace_body = pathtrace.split("fn pt_reconstruct_u16").nth(1).unwrap()
        .split_once('{').unwrap().1.split_once('}').unwrap().0;
    assert_eq!(raster_body.split_whitespace().collect::<Vec<_>>(),
        pathtrace_body.split_whitespace().collect::<Vec<_>>());
}

#[test]
fn emission_map_decodes_nonbinary_dual_plane_color() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[emission_probe] no GPU adapter — skipping");
        return;
    };
    let words = [0x1f91u16, 0x817e, 0xc123];
    let srgb_to_linear = |value: f32| {
        if value <= 0.04045 { value / 12.92 }
        else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    for raw in [false, true] {
        let planes: Vec<_> = [8, 0].into_iter().map(|shift| {
            let mut color = [0; 4];
            color[3] = 255;
            for channel in 0..3 {
                let byte = ((words[channel] >> shift) & 255) as u8;
                color[channel] = if raw { byte } else { encode_srgb(f32::from(byte) / 255.0) };
            }
            layer(|_, _| color)
        }).collect();
        let layers = [layer(|_, _| [255; 4]), planes[0].clone(), planes[1].clone()];
        let probes = [(glam::Vec3::new(0.5 / TEX as f32, 0.5 / TEX as f32, 0.0),
            1, glam::Vec3::Z, if raw { 54 } else { 53 })];
        let samples = run_probes(&device, &queue, &layers, &probes, wgpu::FilterMode::Linear);
        for channel in 0..3 {
            let word = words[channel];
            let encoded = if raw { f32::from(word) / 65535.0 }
                else {
                    let high = srgb_to_linear(f32::from(encode_srgb(f32::from(word >> 8) / 255.0)) / 255.0);
                    let low = srgb_to_linear(f32::from(encode_srgb(f32::from(word & 255) / 255.0)) / 255.0);
                    (high * 65280.0 + low * 255.0) / 65535.0
                };
            let expected = srgb_to_linear(encoded);
            assert!((samples[0][channel] - expected).abs() < if raw { 2e-6 } else { 0.003 },
                "emission channel={channel} raw={raw}: {:?}, expected {expected}", samples[0]);
        }
    }
}

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
            label: Some("triplanar_probe_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()
}

/// linear → sRGB byte encode (the app's data-map pre-encode, replicated so the probe feeds the
/// sRGB texture exactly like material_textures::decode_file_to_data_map does).
fn encode_srgb(v: f32) -> u8 {
    let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

/// One RGBA8 layer of TEX×TEX from a per-texel closure.
fn layer(f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity((TEX * TEX * 4) as usize);
    for y in 0..TEX {
        for x in 0..TEX {
            v.extend_from_slice(&f(x, y));
        }
    }
    v
}

/// Run the extracted sampling WGSL over probe points. Each probe: (pos, slot, normal, mode);
/// mode 0 = sample_mat_triplanar (color), 1 = sample_mat_triplanar_normal. Returns vec4 results.
fn run_probes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layers: &[Vec<u8>],
    probes: &[(glam::Vec3, u32, glam::Vec3, u32)],
    filter: wgpu::FilterMode,
) -> Vec<[f32; 4]> {
    // --- The real shader section, verbatim ---
    let src = include_str!("gbuffer.wgsl");
    let start = src.find("// ---- Material Map Sampling ----").expect("start marker");
    let end = src.find("// ---- End Material Map Sampling ----").expect("end marker");
    let section = &src[start..end];
    let emission = src.split_once("        let eproj =").unwrap().1
        .split_once("        // If scalar emission").unwrap().0;
    let emission = format!("let eproj ={emission}")
        .replace("bitcast<u32>(mat.emission_map.w)", "packed")
        .replace("mat.emission_map.z", "1.0")
        .replace("emap_slot", "slot")
        .replace("p_local", "a.xyz")
        .replace("n_local", "b.xyz")
        .replace("in.uv", "a.xy");

    let probe_src = format!(
        r#"
@group(0) @binding(0) var mat_tex_array: texture_2d_array<f32>;
@group(0) @binding(1) var mat_tex_sampler: sampler;
@group(0) @binding(4) var mat_linear_array: texture_2d_array<f32>;
@group(0) @binding(2) var<storage, read> probes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> out_buf: array<vec4<f32>>;
{section}
{precision}
@compute @workgroup_size(1)
fn cs_probe(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    let a = probes[2u * i];        // xyz = local pos, w = slot
    let b = probes[2u * i + 1u];   // xyz = local normal, w = mode
    let slot = u32(a.w);
    let mode = u32(b.w);
    if (mode == 0u) {{
        out_buf[i] = vec4(sample_mat_triplanar(slot, a.xyz, b.xyz, 1.0), 0.0);
    }} else if (mode == 1u) {{
        // packed = bit 11 (DirectX green): probe fixtures were authored against the
        // shader's historical green read, so the DX word keeps them byte-identical.
        out_buf[i] = vec4(sample_mat_triplanar_normal(slot, a.xyz, b.xyz, 1.0, 1.0, 2048u), 0.0);
    }} else if (mode == 52u) {{
        out_buf[i] = vec4(material_bump_normal(b.xyz, vec3(a.x, 0.0, 0.0),
            vec3(0.0, a.x, 0.0), a.x * a.y, a.x * a.z, 1.0), 0.0);
    }} else if (mode == 51u) {{
        out_buf[i] = vec4(sample_mat_proj_hp(slot, a.xyz, b.xyz, a.xy, 1.0,
                                            (1u << 18u) | (1u << 10u) | 1u, false), 0.0);
    }} else if (mode == 50u) {{
        out_buf[i] = vec4(sample_mat_proj_hp(slot, a.xyz, b.xyz, a.xy, 1.0,
                                            (1u << 10u) | 1u, false), 0.0);
    }} else if (mode == 53u || mode == 54u) {{
        let packed = (1u << 10u) | 1u | select(0u, 1u << 18u, mode == 54u);
        var em_sample: vec3<f32>;
        {emission}
        out_buf[i] = vec4(em_sample, 0.0);
    }} else if (mode >= 100u) {{
        // mode 100+ = triplanar with an explicit blend of (mode - 100) * 0.25 - exercises the
        // blend parameter end to end through the shipped sampler.
        out_buf[i] = vec4(sample_mat_triplanar_b(slot, a.xyz, b.xyz, 1.0,
                                                 f32(mode - 100u) * 0.25, 0u), 0.0);
    }} else {{
        // mode 2+ = the production dispatcher, with (mode - 2) as the MapProjection code.
        out_buf[i] = vec4(sample_mat_proj(slot, a.xyz, b.xyz, a.xy, 1.0, mode - 2u,
                                          TRIPLANAR_BLEND_DEFAULT, 0u), 0.0);
    }}
}}
"#,
        precision = include_str!("../../graphics/src/material_precision.wgsl"),
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("triplanar_probe"),
        source: wgpu::ShaderSource::Wgsl(probe_src.into()),
    });

    // sRGB texture array — same format as MaterialTextureArray.
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe_mat_array"),
        size: wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: layers.len() as u32 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    for (i, px) in layers.iter().enumerate() {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture, mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: i as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            px,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(TEX * 4), rows_per_image: Some(TEX) },
            wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: 1 },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let linear_view = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("probe_sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });

    let mut probe_data: Vec<[f32; 4]> = Vec::new();
    for (p, slot, n, mode) in probes {
        probe_data.push([p.x, p.y, p.z, *slot as f32]);
        probe_data.push([n.x, n.y, n.z, *mode as f32]);
    }
    let probe_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("probe_pts"),
        contents: bytemuck::cast_slice(&probe_data),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let out_size = (probes.len() * 16) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe_out"),
        size: out_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe_staging"),
        size: out_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("probe_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false, min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false, min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("probe_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&linear_view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: probe_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: out_buf.as_entire_binding() },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("probe_pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("probe_pipeline"),
        layout: Some(&layout),
        module: &module,
        entry_point: Some("cs_probe"),
        compilation_options: Default::default(),
        cache: None,
    });

    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(probes.len() as u32, 1, 1);
    }
    enc.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, out_size);
    queue.submit(Some(enc.finish()));

    // Readback — Result captured via channel (GPU Safety rule: never discard with |_| {}).
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
    rx.recv().expect("map_async channel").expect("map_async failed");
    let data: Vec<[f32; 4]> = bytemuck::cast_slice::<u8, f32>(&slice.get_mapped_range())
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    staging.unmap();
    data
}

#[test]
fn box_projection_probe() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[triplanar_probe] no GPU adapter — skipping");
        return;
    };

    // Layer 0: white sentinel (mirrors the runtime slot convention).
    let l0 = layer(|_, _| [255, 255, 255, 255]);
    // Layer 1 (COLOR): u < 0.5 → black, u >= 0.5 → white. sRGB decode of 0/255 is exact.
    let l1 = layer(|x, _| if x < TEX / 2 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
    // Layer 2 (DATA): flat tangent normal (0,0,1) → raw bytes (128,128,255), PRE-ENCODED to sRGB
    // exactly like decode_file_to_data_map does.
    let flat = [encode_srgb(128.0 / 255.0), encode_srgb(128.0 / 255.0), encode_srgb(1.0), 255];
    let l2 = layer(|_, _| flat);
    // Layer 3 (DATA): tangent normal tilted toward +u: t = (0.6, 0, 0.8) → raw = t*0.5+0.5.
    let tilt = [
        encode_srgb(0.6f32 * 0.5 + 0.5),
        encode_srgb(0.5),
        encode_srgb(0.8f32 * 0.5 + 0.5),
        255,
    ];
    let l3 = layer(|_, _| tilt);
    let layers = vec![l0, l1, l2, l3];

    use glam::Vec3;
    let probes = vec![
        // --- 1+2: color box projection on a +X face (uv = local (y,z); u = y) ---
        (Vec3::new(0.3, 0.25, 0.25), 1, Vec3::X, 0u32),  // u=0.25 → black
        (Vec3::new(0.9, 0.25, 0.25), 1, Vec3::X, 0u32),  // x moved → SAME texel (axis invariance)
        (Vec3::new(0.3, 0.75, 0.25), 1, Vec3::X, 0u32),  // u=0.75 → white
        // --- 3: flat normal map = identity (axis + diagonal geometric normals) ---
        (Vec3::new(0.2, 0.2, 0.2), 2, Vec3::Z, 1u32),
        (Vec3::new(0.2, 0.2, 0.2), 2, Vec3::new(0.577, 0.577, 0.577), 1u32),
        // --- 4: tilt orientation per face (pins the derived swizzles) ---
        (Vec3::new(0.2, 0.2, 0.2), 3, Vec3::Z, 1u32),    // +Z face: u = X → tilt toward +X
        (Vec3::new(0.2, 0.2, 0.2), 3, Vec3::X, 1u32),    // +X face: u = Y → tilt toward +Y
        (Vec3::new(0.2, 0.2, 0.2), 3, Vec3::Y, 1u32),    // +Y face: u = X → tilt toward +X
    ];
    let out = run_probes(&device, &queue, &layers, &probes, wgpu::FilterMode::Nearest);

    // 1+2: checker lookup + axis invariance.
    assert!(out[0][0] < 0.05, "u=0.25 on +X face must sample BLACK, got {:?}", out[0]);
    assert!(
        (out[0][0] - out[1][0]).abs() < 1e-5 && (out[0][1] - out[1][1]).abs() < 1e-5,
        "moving along the projection axis (x, +X face) must not change the sample: {:?} vs {:?}",
        out[0], out[1]
    );
    assert!(out[2][0] > 0.95, "u=0.75 on +X face must sample WHITE, got {:?}", out[2]);

    // 3: flat tangent map returns the geometric normal (quantization tolerance: 8-bit through
    // sRGB encode/decode ⇒ per-component error ≲ 0.01 before normalize).
    for (i, n_geo) in [(3usize, Vec3::Z), (4, Vec3::new(0.577, 0.577, 0.577).normalize())] {
        let n_out = Vec3::new(out[i][0], out[i][1], out[i][2]);
        assert!(
            n_out.dot(n_geo) > 0.999,
            "flat normal map must be identity: probe {i} got {n_out:?}, expected ~{n_geo:?}"
        );
    }

    // 4: whiteout swizzle orientation — tilt lands on each face's true u axis.
    let z_face = Vec3::new(out[5][0], out[5][1], out[5][2]);
    assert!(z_face.x > 0.3 && z_face.z > 0.5, "+Z face tilt must bend toward +X: {z_face:?}");
    assert!(z_face.y.abs() < 0.05, "+Z face tilt must not bend toward Y: {z_face:?}");
    let x_face = Vec3::new(out[6][0], out[6][1], out[6][2]);
    assert!(x_face.y > 0.3 && x_face.x > 0.5, "+X face tilt must bend toward +Y (u=Y): {x_face:?}");
    let y_face = Vec3::new(out[7][0], out[7][1], out[7][2]);
    assert!(y_face.x > 0.3 && y_face.y > 0.5, "+Y face tilt must bend toward +X (u=X): {y_face:?}");

    eprintln!("[triplanar_probe] box projection + whiteout normal blend verified on GPU");
}

/// Static guard: the whole gbuffer.wgsl (with the new maps + varyings) parses and validates —
/// create_shader_module only parses at RUNTIME, so a WGSL typo would otherwise be a launch panic.
#[test]
fn gbuffer_wgsl_parses_and_validates() {
    let src = concat!(include_str!("../../graphics/src/material_precision.wgsl"), "\n", include_str!("gbuffer.wgsl"));
    let module = naga::front::wgsl::parse_str(src)
        .unwrap_or_else(|e| panic!("gbuffer.wgsl parse failed: {e:?}"));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("gbuffer.wgsl validation failed: {e:?}"));
}


/// The three projections added in M2 (Sphere / Cylinder / Planar) must each map position to a
/// DIFFERENT texel than Box does, and must be internally consistent. These run the SHIPPED
/// `sample_mat_proj` — the same dispatcher the G-buffer calls — so a projection that is wired but
/// never reached would fail here.
///
/// Codes come from `MapProjection::as_u32()`: 0 Box, 1 UV, 2 Sphere, 3 Cylinder, 4 Planar.
#[test]
fn new_projections_sample_distinctly() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[triplanar_probe] no GPU adapter — skipping");
        return;
    };
    let l0 = layer(|_, _| [255, 255, 255, 255]);
    // A horizontal ramp: the sampled RED channel reports the u coordinate the projection chose,
    // so each probe's output IS the projection's u — which is what we want to compare.
    let l1 = layer(|x, _| {
        let u = x as f32 / (TEX - 1) as f32;
        [encode_srgb(u), 0, 0, 255]
    });

    // One point on the +X side of a unit ball, normal pointing +X.
    let p = glam::Vec3::new(0.7, 0.2, 0.35);
    let n = p.normalize();
    const SPHERE: u32 = 2 + 2;
    const CYL: u32 = 3 + 2;
    const PLANAR: u32 = 4 + 2;
    let out = run_probes(&device, &queue, &[l0, l1], &[
        (p, 1, n, SPHERE),
        (p, 1, n, CYL),
        (p, 1, n, PLANAR),
    ], wgpu::FilterMode::Nearest);

    let (sph, cyl, pla) = (out[0][0], out[1][0], out[2][0]);
    for (name, v) in [("sphere", sph), ("cylinder", cyl), ("planar", pla)] {
        assert!(v.is_finite() && (0.0..=1.0).contains(&v),
            "{name}: sampled u = {v} is not a valid texel read — the projection produced a \
             NaN/out-of-range coordinate");
    }
    // Sphere and cylinder share the same longitude formula, so at a point with the same x/z they
    // MUST agree on u. That is a real invariant, not a coincidence — it pins both to atan2(z,x).
    assert!((sph - cyl).abs() < 0.02,
        "sphere u ({sph}) and cylinder u ({cyl}) both derive from atan2(z,x) and must agree at \
         the same x/z; they diverged, so one of the two longitude formulas is wrong");
    // Planar reads p.xy directly, which is a different quantity entirely.
    assert!((pla - sph).abs() > 0.05,
        "planar u ({pla}) matched the angular projections ({sph}) — planar must read p.xy \
         directly, so a match means it is silently falling through to another branch");
    eprintln!("[proj_probe] sphere u={sph:.4} cylinder u={cyl:.4} planar u={pla:.4}");
}

/// Box must still be the DEFAULT: code 0 through the dispatcher has to equal a direct
/// `sample_mat_triplanar` call. If the dispatcher's fallthrough ever changed, every existing
/// material would silently reproject — a change no visual test would attribute to this code.
#[test]
fn dispatcher_code_zero_is_still_triplanar() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[triplanar_probe] no GPU adapter — skipping");
        return;
    };
    let l0 = layer(|_, _| [255, 255, 255, 255]);
    let l1 = layer(|x, _| if x < TEX / 2 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
    let p = glam::Vec3::new(0.3, -0.15, 0.42);
    let n = glam::Vec3::new(0.2, 0.9, 0.35).normalize();
    let out = run_probes(&device, &queue, &[l0, l1], &[
        (p, 1, n, 0),        // direct sample_mat_triplanar
        (p, 1, n, 0 + 2),    // dispatcher, MapProjection::Box
    ], wgpu::FilterMode::Nearest);
    let (direct, via) = (out[0][0], out[1][0]);
    assert!((direct - via).abs() < 1e-5,
        "dispatcher code 0 ({via}) diverged from sample_mat_triplanar ({direct}) — Box is the \
         default every existing material uses and must route to the identical code path");
}


/// The triplanar blend control must actually change the rendered result, and in the right
/// direction: a HIGHER exponent means the dominant axis wins more, so the sample moves TOWARD
/// whatever that axis alone would give. A control that is plumbed but inert is the failure this
/// pins (it is invisible in any parse/validate test and looks like "the slider does nothing").
#[test]
fn triplanar_blend_sharpens_toward_the_dominant_axis() {
    let Some((device, queue)) = try_device() else {
        eprintln!("[triplanar_probe] no GPU adapter — skipping");
        return;
    };
    let l0 = layer(|_, _| [255, 255, 255, 255]);
    // Horizontal split: the three planes read different texels, so their blend weights are
    // visible in the output value.
    // A ramp in u, so each plane's sampled value directly reports the u IT chose. The three
    // planes read (p.yz, p.xz, p.xy), i.e. u = p.y, p.x, p.x -- so a point with p.y far from p.x
    // makes the X plane disagree with the other two, and the blend weights become visible.
    let l1 = layer(|x, _| {
        let u = x as f32 / (TEX - 1) as f32;
        [encode_srgb(u), 0, 0, 255]
    });

    // p.y (0.8) and p.x (0.1) are far apart, so the YZ plane reads a very different texel from
    // the other two. The normal leans toward +Y, making the YZ plane subdominant -- raising the
    // exponent therefore pulls the result away from p.y and toward p.x.
    let p = glam::Vec3::new(0.1, 0.8, 0.35);
    let n = glam::Vec3::new(0.45, 0.78, 0.44).normalize();
    let soft = 100 + 1;    // blend 0.25 — nearly a flat average
    let mid = 100 + 16;    // blend 4.00 — the default
    let sharp = 100 + 31;  // blend 7.75 — the storable maximum
    let out = run_probes(&device, &queue, &[l0, l1], &[
        (p, 1, n, soft),
        (p, 1, n, mid),
        (p, 1, n, sharp),
    ], wgpu::FilterMode::Nearest);
    let (s0, s1, s2) = (out[0][0], out[1][0], out[2][0]);
    assert!((s0 - s2).abs() > 0.01,
        "blend made no difference: soft={s0} sharp={s2}. The parameter is plumbed but inert —          check that sample_mat_triplanar_b actually uses it rather than the constant.");
    // Monotonic: increasing sharpness must move the result consistently in one direction, not
    // oscillate. (Which direction depends on the texel the dominant axis reads, so the test
    // asserts ordering rather than a fixed sign.)
    let ascending = s0 < s1 && s1 < s2;
    let descending = s0 > s1 && s1 > s2;
    assert!(ascending || descending,
        "blend response is not monotonic across 0.25 -> 4.0 -> 7.75: {s0}, {s1}, {s2} — the          weights are not simply sharpening toward the dominant axis");
    eprintln!("[blend_probe] soft={s0:.4} default={s1:.4} sharp={s2:.4}");
}
