use crate::rt_shadow_gpu_test::{try_device, upload};
use std::collections::HashSet;

const STUB: &str = r#"
struct Config {
    pt_accumulated: u32,
    pt_focus_falloff: f32,
    samples_per_batch: u32,
    frame_seed: u32,
}
struct PtStats {
    noisy: atomic<u32>,
    total: atomic<u32>,
}
@group(0) @binding(0) var<uniform> config: Config;
@group(0) @binding(2) var pt_accum_in: texture_2d<f32>;
@group(0) @binding(3) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<storage, read> samples: array<vec4<f32>>;
@group(0) @binding(51) var<storage, read_write> pt_stats: PtStats;
"#;

const ENTRIES: &str = r#"
@compute @workgroup_size(8, 8)
fn accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(pt_accum_out);
    if (gid.x >= size.x || gid.y >= size.y) { return; }
    var sum = vec3(0.0);
    var batch = vec4(0.0);
    for (var sample_index = 0u; sample_index < config.samples_per_batch; sample_index += 1u) {
        let radiance = samples[sample_index].rgb;
        sum += radiance;
        pt_add_luminance_sample(&batch, radiance);
    }
    pt_store_progressive(vec2<i32>(gid.xy), sum / f32(config.samples_per_batch), batch);
}

@compute @workgroup_size(8, 8)
fn sample_stream(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(pt_accum_out);
    if (gid.x >= size.x || gid.y >= size.y) { return; }
    let base = config.frame_seed * 7919u + gid.x * 1973u + gid.y * 9277u;
    let jitter = pt_r2(base);
    let seed = pt_pcg_hash(gid.x + gid.y * size.x + config.frame_seed * size.x * size.y);
    textureStore(pt_accum_out, vec2<i32>(gid.xy), vec4(jitter, f32(seed >> 8u) / 16777216.0, 1.0));
}
"#;

fn production_function(name: &str) -> &'static str {
    let source = include_str!("pathtrace_tri_entry.wgsl");
    let signature = format!("fn {name}(");
    let start = source.find(&signature).expect("production PT function missing");
    let body = &source[start..];
    let end = body.find("\n}").expect("production PT function end missing");
    &body[..end + 2]
}

fn fixture_source() -> String {
    format!(
        "{STUB}\n{}\n{}\n{}\n{ENTRIES}",
        include_str!("pt_progressive.wgsl"),
        production_function("pt_pcg_hash"),
        production_function("pt_r2"),
    )
}

struct Readback {
    accumulation: Vec<[f32; 4]>,
    moments: Vec<[f32; 4]>,
    stats: [u32; 2],
}

struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    accumulate: wgpu::ComputePipeline,
    sampler: wgpu::ComputePipeline,
    bindings: wgpu::BindGroup,
    config: wgpu::Buffer,
    samples: wgpu::Buffer,
    stats: wgpu::Buffer,
    moments: wgpu::Buffer,
    history: wgpu::Texture,
    output: wgpu::Texture,
    width: u32,
    height: u32,
}

impl Fixture {
    fn new(width: u32, height: u32) -> Option<Self> {
        let Some((device, queue)) = try_device() else {
            eprintln!("[PT-PROGRESSIVE-GPU] no adapter — skipping");
            return None;
        };
        assert_eq!(width * 16 % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PT progressive production helpers"),
            source: wgpu::ShaderSource::Wgsl(fixture_source().into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PT progressive test bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 51,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 60,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 61,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PT progressive test layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let accumulate = pipeline("accumulate");
        let sampler = pipeline("sample_stream");
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let history = texture("PT test history", wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        let output = texture("PT test accumulation", wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC);
        let display = texture("PT test display", wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::STORAGE_BINDING);
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label), size, usage, mapped_at_creation: false,
            })
        };
        let config = buffer("PT test config", 16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let samples = upload(&device, &queue, &[[0.0f32; 4]; 16], "PT test samples");
        let read_write = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        let stats = buffer("PT test stats", 8, read_write);
        let moments = buffer("PT test moments", u64::from(width * height) * 16, read_write);
        let history_view = history.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let display_view = display.create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PT progressive test resources"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: config.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&history_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&display_view) },
                wgpu::BindGroupEntry { binding: 4, resource: samples.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 51, resource: stats.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 60, resource: wgpu::BindingResource::TextureView(&output_view) },
                wgpu::BindGroupEntry { binding: 61, resource: moments.as_entire_binding() },
            ],
        });
        Some(Self { device, queue, accumulate, sampler, bindings, config, samples, stats, moments, history, output, width, height })
    }

    fn seed_history(&self, color: [f32; 4], moments: [f32; 4]) {
        let pixels = vec![color; (self.width * self.height) as usize];
        self.queue.write_texture(
            self.history.as_image_copy(),
            bytemuck::cast_slice(&pixels),
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(self.width * 16), rows_per_image: Some(self.height) },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        let moments = vec![moments; pixels.len()];
        self.queue.write_buffer(&self.moments, 0, bytemuck::cast_slice(&moments));
    }

    fn accumulate(&self, frame: u32, threshold: f32, samples: &[[f32; 4]]) {
        assert!(!samples.is_empty() && samples.len() <= 16);
        self.queue.write_buffer(&self.samples, 0, bytemuck::cast_slice(samples));
        self.queue.write_buffer(&self.config, 0, bytemuck::cast_slice(&[frame, threshold.to_bits(), samples.len() as u32, 0]));
        self.queue.write_buffer(&self.stats, 0, bytemuck::cast_slice(&[0u32; 2]));
        self.dispatch(&self.accumulate, true);
    }

    fn sample_stream(&self, frame: u32) {
        self.queue.write_buffer(&self.config, 0, bytemuck::cast_slice(&[0u32, 0, 0, frame]));
        self.dispatch(&self.sampler, false);
    }

    fn dispatch(&self, pipeline: &wgpu::ComputePipeline, copy_history: bool) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PT progressive regression"), timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
        if copy_history {
            encoder.copy_texture_to_texture(self.output.as_image_copy(), self.history.as_image_copy(),
                wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 });
        }
        self.queue.submit([encoder.finish()]);
    }

    fn readback(&self) -> Readback {
        let pixel_bytes = u64::from(self.width * self.height) * 16;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PT progressive readback"),
            size: pixel_bytes * 2 + 8,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(self.output.as_image_copy(), wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(self.width * 16), rows_per_image: Some(self.height) },
        }, wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 });
        encoder.copy_buffer_to_buffer(&self.moments, 0, &staging, pixel_bytes, pixel_bytes);
        encoder.copy_buffer_to_buffer(&self.stats, 0, &staging, pixel_bytes * 2, 8);
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        receiver.recv().expect("PT progressive readback channel").expect("PT progressive map failed");
        let mapped = staging.slice(..).get_mapped_range();
        let split = pixel_bytes as usize;
        let result = Readback {
            accumulation: bytemuck::cast_slice(&mapped[..split]).to_vec(),
            moments: bytemuck::cast_slice(&mapped[split..split * 2]).to_vec(),
            stats: bytemuck::pod_read_unaligned(&mapped[split * 2..]),
        };
        drop(mapped);
        staging.unmap();
        result
    }
}

#[test]
fn pt_progressive_fixture_uses_valid_production_wgsl() {
    let source = fixture_source();
    let module = naga::front::wgsl::parse_str(&source).expect("PT progressive fixture WGSL parse");
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module).expect("PT progressive fixture WGSL validation");
    assert!(source.contains(include_str!("pt_progressive.wgsl")));
}

#[test]
fn pt_progressive_weighted_varying_spp_matches_sample_oracle() {
    let Some(fixture) = Fixture::new(32, 8) else { return; };
    let batches = [vec![0.25], vec![1.0, 2.0, 3.0], vec![0.5, 0.75], vec![4.0; 7]];
    let mut all_samples = Vec::<f64>::new();
    for (frame, batch) in batches.iter().enumerate() {
        let samples: Vec<_> = batch.iter().map(|&value| [value, value * 0.5, value * 2.0, 0.0]).collect();
        fixture.accumulate(frame as u32 + 1, 0.01, &samples);
        all_samples.extend(batch.iter().map(|&value| f64::from(value)));
        let expected = all_samples.iter().sum::<f64>() / all_samples.len() as f64;
        let luminance_scale = 0.2126 + 0.7152 * 0.5 + 0.0722 * 2.0;
        let luminance_mean = expected * luminance_scale;
        let expected_m2 = all_samples.iter().map(|value| (value * luminance_scale - luminance_mean).powi(2)).sum::<f64>();
        let result = fixture.readback();
        assert_eq!(result.stats[1], fixture.width * fixture.height);
        for (pixel, moments) in result.accumulation.iter().zip(&result.moments) {
            for (channel, scale) in [1.0, 0.5, 2.0].into_iter().enumerate() {
                assert!((f64::from(pixel[channel]) - expected * scale).abs() < 2e-6);
            }
            assert_eq!(pixel[3], all_samples.len() as f32);
            assert_eq!(moments[2], pixel[3]);
            assert!((f64::from(moments[0]) - luminance_mean).abs() < 2e-6);
            assert!((f64::from(moments[1]) - expected_m2).abs() < 2e-5);
        }
    }
}

#[test]
fn pt_progressive_counts_beyond_2048_and_retains_fp32_small_updates() {
    let Some(fixture) = Fixture::new(32, 8) else { return; };
    for count in [2048u32, 65536] {
        fixture.seed_history([0.5, 0.5, 0.5, count as f32], [0.5, 0.0, count as f32, 0.0]);
        fixture.accumulate(count + 1, 0.01, &[[1.0, 1.0, 1.0, 0.0]]);
        let result = fixture.readback();
        let expected = 0.5f64 + 0.5 / f64::from(count + 1);
        for (pixel, moments) in result.accumulation.iter().zip(&result.moments) {
            assert_eq!(pixel[3], (count + 1) as f32);
            assert_eq!(moments[2], (count + 1) as f32);
            assert!(pixel[0] > 0.5 + 1e-6, "FP32 accumulation froze: {pixel:?}");
            assert!((f64::from(pixel[0]) - expected).abs() < 1e-7);
            assert_eq!(half::f16::from_f32(pixel[0]), half::f16::from_f32(0.5));
        }
    }
}

#[test]
fn pt_progressive_stationary_noisy_mean_uses_sample_variance() {
    let Some(fixture) = Fixture::new(32, 8) else { return; };
    for frame in 1..=32 {
        fixture.accumulate(frame, 0.01, &[[0.0; 4], [2.0, 2.0, 2.0, 0.0]]);
    }
    let result = fixture.readback();
    assert_eq!(result.stats, [fixture.width * fixture.height; 2]);
    for (pixel, moments) in result.accumulation.iter().zip(&result.moments) {
        assert!((pixel[0] - 1.0).abs() < 1e-6);
        assert!((moments[0] - 1.0).abs() < 1e-6);
        assert!((moments[1] - 64.0).abs() < 1e-4);
        assert_eq!(moments[2], 64.0);
    }
}

#[test]
fn pt_progressive_reset_ignores_old_and_nan_history_and_resets_moments() {
    let Some(fixture) = Fixture::new(32, 8) else { return; };
    for reset_frame in [0, 1] {
        for old in [65536.0, f32::NAN] {
            fixture.seed_history([old; 4], [old; 4]);
            fixture.accumulate(reset_frame, 0.01, &[[0.25, 0.25, 0.25, 0.0], [0.75, 0.75, 0.75, 0.0]]);
            let result = fixture.readback();
            assert_eq!(result.stats, [fixture.width * fixture.height; 2]);
            for (pixel, moments) in result.accumulation.iter().zip(&result.moments) {
                assert!(pixel.iter().chain(moments).all(|value| value.is_finite()));
                assert!((pixel[0] - 0.5).abs() < 1e-6);
                assert_eq!(pixel[3], 2.0);
                assert!((moments[0] - 0.5).abs() < 1e-6);
                assert!((moments[1] - 0.125).abs() < 1e-6);
                assert_eq!(moments[2], 2.0);
                assert_eq!(moments[3], 0.0);
            }
        }
    }
}

#[test]
fn pt_progressive_first_frames_are_noisy_and_dark_zero_converges() {
    let Some(fixture) = Fixture::new(32, 8) else { return; };
    for frame in 1..=5 {
        fixture.accumulate(frame, 0.01, &[[0.0; 4]]);
        let result = fixture.readback();
        let noisy = if frame < 4 { fixture.width * fixture.height } else { 0 };
        assert_eq!(result.stats, [noisy, fixture.width * fixture.height]);
        for (pixel, moments) in result.accumulation.iter().zip(&result.moments) {
            assert_eq!(*pixel, [0.0, 0.0, 0.0, frame as f32]);
            assert_eq!(*moments, [0.0, 0.0, frame as f32, 0.0]);
        }
    }
}

#[test]
fn pt_sampler_high_frames_keep_4k_pixels_unquantized_and_unlocked() {
    let Some(fixture) = Fixture::new(64, 64) else { return; };
    let mut previous = None::<Vec<[f32; 4]>>;
    for frame in [0, 1, 2048, 2049, 65536, 65537, 16777216] {
        fixture.sample_stream(frame);
        let pixels = fixture.readback().accumulation;
        for axis in 0..2 {
            let mut unique = HashSet::new();
            let mut histogram = [0usize; 16];
            for pixel in &pixels {
                let value = pixel[axis];
                assert!(value.is_finite() && (0.0..1.0).contains(&value), "frame={frame} value={value}");
                unique.insert(value.to_bits());
                histogram[(value * 16.0) as usize] += 1;
            }
            assert!(unique.len() > 3900, "frame={frame} axis={axis} quantized to {} values", unique.len());
            assert!(histogram.iter().all(|&count| (160..=360).contains(&count)), "frame={frame} axis={axis} histogram={histogram:?}");
            let mut horizontal = 0.0;
            let mut vertical = 0.0;
            for row in 0..63usize {
                for column in 0..63usize {
                    let index = row * 64 + column;
                    horizontal += (pixels[index][axis] - pixels[index + 1][axis]).abs();
                    vertical += (pixels[index][axis] - pixels[index + 64][axis]).abs();
                }
            }
            horizontal /= 63.0 * 63.0;
            vertical /= 63.0 * 63.0;
            assert!((0.25..0.4).contains(&horizontal), "frame={frame} horizontal lock {horizontal}");
            assert!((0.25..0.4).contains(&vertical), "frame={frame} row lock {vertical}");
        }
        if let Some(previous) = previous {
            let changed = pixels.iter().zip(previous).filter(|(current, old)| current[..2] != old[..2]).count();
            assert!(changed > 4000, "frame={frame} repeats a previous frame's pixel stream");
        }
        fixture.sample_stream(frame);
        assert_eq!(pixels, fixture.readback().accumulation, "frame={frame} stream is not deterministic");
        previous = Some(pixels);
    }
}
