use half::f16;
use wgpu::util::DeviceExt;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 16;
const PIXELS: usize = (WIDTH * HEIGHT) as usize;

#[derive(Clone)]
struct Scene {
    color: Vec<[f32; 4]>,
    guide: Vec<[f32; 4]>,
    albedo: Vec<[f32; 4]>,
    moments: Vec<[f32; 4]>,
}

impl Scene {
    fn uniform(value: f32, variance: f32, count: f32) -> Self {
        Self {
            color: vec![[value, value, value, count]; PIXELS],
            guide: vec![[0.0, 0.0, 1.0, 5.0]; PIXELS],
            albedo: vec![[0.2, 0.2, 0.2, 1.0]; PIXELS],
            moments: vec![[value, variance * count * (count - 1.0), count, 0.0]; PIXELS],
        }
    }
}

#[derive(Clone, Copy)]
enum Uniforms {
    OrderedCopies,
    ImmutableBindings,
    CollapsedWrites,
}

struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl Fixture {
    fn new() -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("PT denoise regression requires a real GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("pt_denoise_regression"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        ))
        .expect("PT denoise regression device");
        device.on_uncaptured_error(Box::new(|error| panic!("PT denoise GPU error: {error}")));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pt_denoise_regression"),
            entries: &graphics::bgl::pt_denoise_bgl_entries(),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("production_pt_denoise"),
            source: wgpu::ShaderSource::Wgsl(graphics::PT_DENOISE_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("pass_pt_denoise"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            device,
            queue,
            layout,
            pipeline,
        }
    }

    fn texture(&self, label: &str, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn run(
        &self,
        scene: &Scene,
        steps: &[u32],
        uniforms: Uniforms,
        inverted: bool,
        strength: f32,
    ) -> Vec<[f32; 4]> {
        assert!(!steps.is_empty() && steps.len() <= 4);
        let input = self.texture("raw32_accumulation", wgpu::TextureFormat::Rgba32Float);
        let guide = self.texture("guide", wgpu::TextureFormat::Rgba32Float);
        let albedo = self.texture("albedo", wgpu::TextureFormat::Rgba32Float);
        let display = self.texture("display16", wgpu::TextureFormat::Rgba16Float);
        let scratch = self.texture("denoise_scratch16", wgpu::TextureFormat::Rgba16Float);
        for (texture, data) in [
            (&input, &scene.color),
            (&guide, &scene.guide),
            (&albedo, &scene.albedo),
        ] {
            self.queue.write_texture(
                texture.as_image_copy(),
                bytemuck::cast_slice(data),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(WIDTH * 16),
                    rows_per_image: Some(HEIGHT),
                },
                texture.size(),
            );
        }
        let moments = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("welford_moments"),
                contents: bytemuck::cast_slice(&scene.moments),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params: Vec<[u32; 8]> = steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                [
                    strength.to_bits(),
                    1,
                    WIDTH,
                    HEIGHT,
                    1,
                    *step,
                    index as u32,
                    0,
                ]
            })
            .collect();
        let immutable = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("immutable_pass_params"),
                contents: bytemuck::cast_slice(&params),
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        let shared = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shared_params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let guide_view = guide.create_view(&Default::default());
        let albedo_view = albedo.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let invert_even = inverted && steps.len() % 2 == 0;
        for (index, param) in params.iter().enumerate() {
            let separate = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("reference_pass_params"),
                    contents: bytemuck::cast_slice(param),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let parameter_binding = match uniforms {
                Uniforms::OrderedCopies => {
                    encoder.copy_buffer_to_buffer(&immutable, index as u64 * 32, &shared, 0, 32);
                    &shared
                }
                Uniforms::ImmutableBindings => &separate,
                Uniforms::CollapsedWrites => {
                    self.queue
                        .write_buffer(&shared, 0, bytemuck::cast_slice(param));
                    &shared
                }
            };
            let to_display = (index % 2 == 0) != invert_even;
            let output = if to_display { &display } else { &scratch };
            let source = if index == 0 {
                &input
            } else if to_display {
                &scratch
            } else {
                &display
            };
            let source_view = source.create_view(&Default::default());
            let output_view = output.create_view(&Default::default());
            let binding = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&guide_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&output_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: parameter_binding.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&albedo_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: moments.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.dispatch_workgroups(WIDTH.div_ceil(8), HEIGHT.div_ceil(8), 1);
        }
        if !invert_even && steps.len() % 2 == 0 {
            encoder.copy_texture_to_texture(
                scratch.as_image_copy(),
                display.as_image_copy(),
                display.size(),
            );
        }
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("denoise_and_raw32_readback"),
            size: (PIXELS * 24) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for (texture, offset, bytes_per_row) in [
            (&display, 0, WIDTH * 8),
            (&input, (PIXELS * 8) as u64, WIDTH * 16),
        ] {
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::ImageCopyBuffer {
                    buffer: &readback,
                    layout: wgpu::ImageDataLayout {
                        offset,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(HEIGHT),
                    },
                },
                texture.size(),
            );
        }
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap();
            });
        self.device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        receiver.recv().unwrap().expect("denoise GPU map");
        let mapped = readback.slice(..).get_mapped_range();
        assert_eq!(
            &mapped[PIXELS * 8..],
            bytemuck::cast_slice(&scene.color),
            "denoise modified raw32 history"
        );
        mapped[..PIXELS * 8]
            .chunks_exact(8)
            .map(|pixel| {
                std::array::from_fn(|channel| {
                    f16::from_bits(u16::from_le_bytes([
                        pixel[channel * 2],
                        pixel[channel * 2 + 1],
                    ]))
                    .to_f32()
                })
            })
            .collect()
    }
}

#[test]
fn gpu_multipass_parameters_are_ordered_and_topologies_agree() {
    let fixture = Fixture::new();
    let mut scene = Scene::uniform(0.1, 0.02, 4096.0);
    scene.color[(HEIGHT / 2 * WIDTH + WIDTH / 2) as usize] = [0.3, 0.3, 0.3, 4096.0];
    for count in 1..=4 {
        let steps = &[1, 2, 4, 8][..count];
        let ordered = fixture.run(&scene, steps, Uniforms::OrderedCopies, false, 1.0);
        let reference = fixture.run(&scene, steps, Uniforms::ImmutableBindings, false, 1.0);
        let inverted = fixture.run(&scene, steps, Uniforms::OrderedCopies, true, 1.0);
        assert_eq!(
            ordered, reference,
            "pass count {count}: ordered copies != immutable reference"
        );
        assert_eq!(
            ordered, inverted,
            "pass count {count}: parity topology mismatch"
        );
        assert!(ordered.iter().all(|pixel| pixel[3] == 4096.0));
        if count > 1 {
            let broken = fixture.run(&scene, steps, Uniforms::CollapsedWrites, false, 1.0);
            let difference = ordered
                .iter()
                .zip(&broken)
                .map(|(correct, wrong)| (correct[0] - wrong[0]).abs())
                .fold(0.0, f32::max);
            assert!(
                difference > 0.001,
                "fixture failed to detect collapsed uniforms for {count} passes: {difference}"
            );
        }
    }
}

#[test]
fn gpu_plane_gradient_filters_slopes_but_rejects_geometry_edges() {
    let fixture = Fixture::new();
    let mut flat = Scene::uniform(0.1, 0.02, 8.0);
    for (index, pixel) in flat.color.iter_mut().enumerate() {
        pixel[0] += if index % 2 == 0 { 0.03 } else { -0.03 };
        pixel[1] = pixel[0];
        pixel[2] = pixel[0];
    }
    let mut slope = flat.clone();
    for (index, guide) in slope.guide.iter_mut().enumerate() {
        guide[3] = 1.0 + (index as u32 % WIDTH) as f32 * 0.25;
    }
    let flat_output = fixture.run(&flat, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    let slope_output = fixture.run(&slope, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    for (level, tilted) in flat_output.iter().zip(&slope_output) {
        assert!(
            (level[0] - tilted[0]).abs() < 0.0002,
            "slope incorrectly rejected: {} vs {}",
            level[0],
            tilted[0]
        );
    }
    let mut edge = Scene::uniform(0.01, 1.0, 8.0);
    for index in 0..PIXELS {
        if index as u32 % WIDTH >= WIDTH / 2 {
            edge.color[index] = [0.5, 0.5, 0.5, 8.0];
            edge.guide[index][3] = 5.1;
        }
    }
    let output = fixture.run(&edge, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    let left = (HEIGHT / 2 * WIDTH + WIDTH / 2 - 1) as usize;
    assert!(
        output[left][0] < 0.0102,
        "cross-depth leak: {}",
        output[left][0]
    );
    assert!(
        output[left + 1][0] > 0.499,
        "cross-depth leak: {}",
        output[left + 1][0]
    );
    edge.guide[left] = [0.0, 0.0, 0.0, 0.0];
    edge.color[left] = [0.8, 0.3, 0.1, 8.0];
    let sky = fixture.run(&edge, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    let sky_passthrough = fixture.run(&edge, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 0.0);
    assert_eq!(sky[left], sky_passthrough[left]);
    edge.guide.iter_mut().for_each(|guide| guide[3] = 5.0);
    for index in 0..PIXELS {
        if index as u32 % WIDTH >= WIDTH / 2 {
            edge.guide[index] = [1.0, 0.0, 0.0, 5.0];
        }
    }
    let normal = fixture.run(&edge, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    assert!(normal[left + 1][0] > 0.499, "normal edge leaked");
}

#[test]
fn gpu_dark_noise_uses_variance_not_history_age() {
    let fixture = Fixture::new();
    let mut noisy = Scene::uniform(0.001, 0.000004, 4096.0);
    for (index, pixel) in noisy.color.iter_mut().enumerate() {
        let value = if index % 2 == 0 { 0.0002 } else { 0.0018 };
        *pixel = [value, value, value, 4096.0];
    }
    let filtered = fixture.run(&noisy, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 1.0);
    let noisy_passthrough = fixture.run(&noisy, &[1, 2, 4, 8], Uniforms::OrderedCopies, false, 0.0);
    assert_ne!(
        filtered, noisy_passthrough,
        "filter must detectably change noisy dark pixels"
    );
    let initial_error: f32 = noisy_passthrough
        .iter()
        .map(|pixel| (pixel[0] - 0.001).powi(2))
        .sum();
    let filtered_error: f32 = filtered
        .iter()
        .map(|pixel| (pixel[0] - 0.001).powi(2))
        .sum();
    assert!(
        filtered_error < initial_error * 0.15,
        "dark noise not filtered: {filtered_error} / {initial_error}"
    );
    let mut converged = noisy.clone();
    converged
        .moments
        .iter_mut()
        .for_each(|moment| moment[1] = 0.0);
    let unchanged = fixture.run(
        &converged,
        &[1, 2, 4, 8],
        Uniforms::OrderedCopies,
        false,
        1.0,
    );
    let converged_passthrough = fixture.run(
        &converged,
        &[1, 2, 4, 8],
        Uniforms::OrderedCopies,
        false,
        0.0,
    );
    assert_eq!(
        unchanged, converged_passthrough,
        "converged detail was blurred"
    );
    let mut early = noisy.clone();
    early.moments.iter_mut().for_each(|moment| {
        moment[1] = 0.0;
        moment[2] = 1.0;
    });
    let early_output = fixture.run(&early, &[1], Uniforms::OrderedCopies, false, 1.0);
    let early_passthrough = fixture.run(&early, &[1], Uniforms::OrderedCopies, false, 0.0);
    assert!(
        (early_output[(WIDTH * HEIGHT / 2) as usize][0]
            - early_passthrough[(WIDTH * HEIGHT / 2) as usize][0])
            .abs()
            > 0.0001,
        "one sample incorrectly declared converged"
    );
}
