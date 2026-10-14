//! Lazy ensure_* pipeline compiles (fallback when background warmup has not landed).

use eframe::wgpu;
use crate::GpuCtx;

use super::pipelines_bg::compile_classify_finalize_pipelines;

/// Lazily compile full + Lite fog pipelines on first use (avoids 40s+ startup cost).
/// Both entry points share one WGSL module (`pass_fog` + `pass_fog_lite`).
pub(crate) fn ensure_fog_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.fog_pipeline.is_some() && gpu.pipelines.fog_lite_pipeline.is_some() {
        return;
    }
    let device = &gpu.device;
    let bgl = &gpu.bind_groups.fog_bgl;
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let t = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fog_wgsl = graphics::build_fog_wgsl(None);
        let fog_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{
            label: Some("Fog (lazy)"),
            source: wgpu::ShaderSource::Wgsl(fog_wgsl.into()),
        });
        let fog_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{
            label: Some("fog"), bind_group_layouts: &[bgl], push_constant_ranges: &[]
        });
        let full = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
            label: Some("fog"), layout: Some(&fog_pl), module: &fog_shader,
            entry_point: Some("pass_fog"), compilation_options: Default::default(),
            cache: cache_ref,
        });
        let lite = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
            label: Some("fog_lite"), layout: Some(&fog_pl), module: &fog_shader,
            entry_point: Some("pass_fog_lite"), compilation_options: Default::default(),
            cache: cache_ref,
        });
        (full, lite)
    }));
    match result {
        Ok((full, lite)) => {
            gpu.pipelines.fog_pipeline = Some(full);
            gpu.pipelines.fog_lite_pipeline = Some(lite);
            eprintln!("[TIMING] Fog pipelines (full+lite) compiled (lazy): {:?}", t.elapsed());
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().map(|s| s.as_str())
                .or_else(|| e.downcast_ref::<&str>().copied())
                .unwrap_or("unknown error");
            eprintln!("[SHADER] Fog pipeline compile failed: {}", msg);
            gpu.pipelines.shader_compile_error = Some(format!("Fog pipeline: {}", msg));
        }
    }
}


/// Lazily compile the FXAA pipeline on first use.
#[allow(dead_code)]
pub(crate) fn ensure_fxaa_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.fxaa_pipeline.is_some() { return; }
    let device = &gpu.device;
    let t = std::time::Instant::now();
    let fxaa_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{
        label: Some("FXAA (lazy)"),
        source: wgpu::ShaderSource::Wgsl(graphics::FXAA_WGSL.into()),
    });
    let fxaa_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{
        label: Some("fxaa"), bind_group_layouts: &[gpu.bind_groups.fxaa_bgl.as_ref().expect("fxaa_bgl must be set before ensure_fxaa_pipeline")], push_constant_ranges: &[]
    });
    gpu.pipelines.fxaa_pipeline = Some(device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
        label: Some("fxaa"), layout: Some(&fxaa_pl), module: &fxaa_shader,
        entry_point: Some("pass_fxaa"), compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    }));
    eprintln!("[TIMING] FXAA pipeline compiled (lazy): {:?}", t.elapsed());
}


/// PT warmup stage 1 readiness: the full progressive kernel (the PT display kernel). The
/// interactive kernel is a LATER stage (`ensure_pt_interactive_pipeline`) — PT displays without
/// it, interaction then runs this kernel at the interaction budget (PT-COLD-WARMUP-BUNDLED).
#[must_use]
pub(crate) fn ensure_pt_tri_pipeline(gpu: &GpuCtx) -> bool {
    gpu.pipelines.pt_tri_pipeline.is_some()
}

/// PT warmup stage 2 readiness: the interactive (ReSTIR reuse) kernel.
#[must_use]
pub(crate) fn ensure_pt_interactive_pipeline(gpu: &GpuCtx) -> bool {
    gpu.pipelines.pt_tri_interactive_pipeline.is_some()
}


/// (Re)build the PT bind group. Called lazily from the PathTrace dispatch arm —
/// requires the HDR/accum textures AND all 6 triangle-scene buffers (created by
/// update_pt_accel in the poll phase). Returns false if any resource is missing.
/// Ensure Phase A adaptive-stop stats buffers exist (atomics + staging readback).
pub(crate) fn ensure_pt_stats_buffers(gpu: &mut GpuCtx) {
    if gpu.buffers.pt_stats_buffer.is_some() { return; }
    let device = &gpu.device;
    gpu.buffers.pt_stats_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pt_stats"),
        size: 16, // two u32 atomics + pad
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    }));
    gpu.buffers.pt_stats_staging = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pt_stats_staging"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    gpu.buffers.pt_stats_map_pending.store(false, std::sync::atomic::Ordering::Relaxed);
    gpu.buffers.pt_stats_map_ready = None;
    gpu.buffers.pt_noisy_fraction_bits.store(f32::to_bits(1.0), std::sync::atomic::Ordering::Relaxed);
}


/// A1′: non-blocking drain of a completed `pt_stats_staging` map_async.
/// Call after `device.poll(Poll)` (poll phase or post-submit Poll). Never uses Wait.
/// Updates `pt_noisy_fraction_bits` for next frame's quality / stop decisions.
pub(crate) fn try_complete_pt_stats_readback(gpu: &mut GpuCtx) {
    use crate::gpu_ctx::map_state;
    use std::sync::atomic::Ordering;

    if !gpu.buffers.pt_stats_map_pending.load(Ordering::Relaxed) {
        return;
    }
    let Some(ready) = gpu.buffers.pt_stats_map_ready.as_ref() else {
        gpu.buffers.pt_stats_map_pending.store(false, Ordering::Relaxed);
        return;
    };
    match ready.peek() {
        map_state::PENDING => return,
        map_state::ERR => {
            // Map failed (device loss / aborted) — drop pending; keep last fraction.
            gpu.buffers.pt_stats_map_ready = None;
            gpu.buffers.pt_stats_map_pending.store(false, Ordering::Relaxed);
            return;
        }
        map_state::OK => {}
        _ => return,
    }
    let Some(ref staging) = gpu.buffers.pt_stats_staging else {
        gpu.buffers.pt_stats_map_ready = None;
        gpu.buffers.pt_stats_map_pending.store(false, Ordering::Relaxed);
        return;
    };
    {
        let slice = staging.slice(..);
        let data = slice.get_mapped_range();
        let noisy = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let total = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let frac = if total > 0 {
            noisy as f32 / total as f32
        } else {
            1.0
        };
        gpu.buffers
            .pt_noisy_fraction_bits
            .store(frac.to_bits(), Ordering::Relaxed);
    }
    staging.unmap();
    gpu.buffers.pt_stats_map_ready = None;
    gpu.buffers.pt_stats_map_pending.store(false, Ordering::Relaxed);
}


/// A1′: start MAP_READ on staging after a GPU copy into it. Non-blocking (no Wait).
/// Caller must only invoke when `!pt_stats_map_pending` and a copy was submitted this frame.
pub(crate) fn start_pt_stats_readback(gpu: &mut GpuCtx) {
    use crate::gpu_ctx::map_state;
    use std::sync::atomic::Ordering;

    if gpu.buffers.pt_stats_map_pending.load(Ordering::Relaxed) {
        return;
    }
    let Some(ref staging) = gpu.buffers.pt_stats_staging else {
        return;
    };
    let ready = std::sync::Arc::new(map_state::MapState::new());
    let ready_cb = ready.clone();
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| {
            ready_cb.store_result(&r);
        });
    gpu.buffers.pt_stats_map_ready = Some(ready);
    gpu.buffers
        .pt_stats_map_pending
        .store(true, Ordering::Relaxed);
}


/// Section K: ensure a bindable env CDF buffer exists (dummy empty header if no HDRI).
pub(crate) fn ensure_pt_env_cdf_buffer(gpu: &mut GpuCtx) {
    if gpu.buffers.pt_env_cdf_buffer.is_some() {
        return;
    }
    crate::env_ibl::upload_pt_env_cdf(gpu, &crate::env_ibl::empty_env_cdf());
}


#[must_use]
pub(crate) fn ensure_pt_upscale_pipeline(gpu: &GpuCtx) -> bool {
    gpu.pipelines.pt_upscale_pipeline.is_some()
        && gpu.bind_groups.pt_upscale_bgl.is_some()
        && gpu.bind_groups.pt_upscale_params_buffer.as_ref().is_some_and(|buffer| {
            buffer.size() >= 16
                && buffer.usage().contains(wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST)
        })
}

/// Hardware ray-query variants of the canonical tri PT. Each source is the SAME static
/// specialization the software pipelines compile, with only `pt_trace_scene`/`pt_occluded`
/// swapped for ray queries — material/lighting parity comes from sharing the source.
/// A rejected adaptation leaves software traversal as the PT backend (logged, never fatal).
fn compile_native_rt_pipelines(
    device: &wgpu::Device,
    tri_bgl: &wgpu::BindGroupLayout,
    cache: Option<&wgpu::PipelineCache>,
) -> Option<std::sync::Arc<app_gpu::native_rt::NativeRtPipelines>> {
    let started = std::time::Instant::now();
    let adapt = |interactive: bool| {
        app_gpu::native_rt::build_native_rt_wgsl(&graphics::build_pathtrace_tri_wgsl_with_mode(Some(interactive)))
    };
    let compiled = adapt(false).and_then(|full| {
        let interactive = adapt(true)?;
        app_gpu::native_rt::NativeRtPipelines::compile(device, tri_bgl, &full, &interactive, cache)
            .map_err(|error| error.to_string())
    });
    match compiled {
        Ok(pipelines) => {
            eprintln!("[TIMING] Native PT full+interactive bg: {:.1}ms", started.elapsed().as_secs_f64() * 1000.0);
            if let Some(cache) = cache {
                super::cache::save_pipeline_cache(cache);
            }
            Some(std::sync::Arc::new(pipelines))
        }
        Err(error) => {
            eprintln!("[RTX] Native PT pipelines unavailable, software traversal stays active: {error}");
            None
        }
    }
}

/// PT warmup worker body (PT-COLD-WARMUP-BUNDLED). Compiles the PT stages in delivery
/// order on the ONE warmup thread and hands each to `deliver` the moment it exists, so the
/// viewport switches to PT after the full stage (~9 s cold) instead of waiting for the whole
/// bundle (~293 s cold, measured 2026-10-13):
///   1. `Full`        — full progressive kernel + denoise + upscale: PT is displayable.
///   2. `Interactive` — the ReSTIR interactive kernel (~122 s cold). Until it is installed,
///      interaction runs the full kernel at the interaction budget (half-res, 1 spp).
///   3. `Native`      — the hardware ray-query pair (~162 s cold, `None` when the device lacks
///      the features or the adaptation is rejected). Software traversal until then.
/// The main thread adopts each stage in `poll_background_tasks` (Frame Ordering Contract).
pub(crate) fn compile_pt_warmup_stages(
    device: &wgpu::Device,
    cache: Option<&wgpu::PipelineCache>,
    mut deliver: impl FnMut(crate::app_poll::PtWarmupStage),
) {
    use crate::app_poll::PtWarmupStage;
    let tri_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pt_bgl_bg"),
        entries: &graphics::bgl::pt_bgl_entries(),
    });
    let tri_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pt_tri_bg"), bind_group_layouts: &[&tri_bgl], push_constant_ranges: &[],
    });
    deliver(PtWarmupStage::Full(crate::app_poll::PtFullStage::compile(device, &tri_layout, cache)));
    deliver(PtWarmupStage::Interactive(compile_pt_interactive_pipeline(device, &tri_layout, cache)));
    let native = if device.features().contains(app_gpu::native_rt::REQUIRED_FEATURES) {
        compile_native_rt_pipelines(device, &tri_bgl, cache)
    } else {
        None
    };
    deliver(PtWarmupStage::Native(native));
}

/// Stage 2 of the PT warmup: the interactive (ReSTIR DI/GI reuse) kernel. Saved to the
/// pipeline cache immediately so a quit before the native stage still keeps this compile.
fn compile_pt_interactive_pipeline(
    device: &wgpu::Device,
    tri_layout: &wgpu::PipelineLayout,
    cache: Option<&wgpu::PipelineCache>,
) -> wgpu::ComputePipeline {
    let interactive_started = std::time::Instant::now();
    let interactive_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Pathtrace-tri interactive (bg)"),
        source: wgpu::ShaderSource::Wgsl(graphics::build_pathtrace_tri_wgsl_with_mode(Some(true)).into()),
    });
    let interactive_module_ms = interactive_started.elapsed().as_secs_f64() * 1000.0;
    let interactive_pipeline_started = std::time::Instant::now();
    let interactive = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pathtrace_tri_interactive"), layout: Some(tri_layout), module: &interactive_shader,
        entry_point: Some("pass_pathtrace_tri"), compilation_options: Default::default(), cache,
    });
    eprintln!("[TIMING] PT interactive bg: module={interactive_module_ms:.1}ms pipeline={:.1}ms", interactive_pipeline_started.elapsed().as_secs_f64() * 1000.0);
    if let Some(cache) = cache {
        super::cache::save_pipeline_cache(cache);
    }
    interactive
}

impl crate::app_poll::PtFullStage {
    /// Stage 1 of the PT warmup: everything the PT display needs (full kernel, denoise, upscale).
    fn compile(device: &wgpu::Device, tri_layout: &wgpu::PipelineLayout, cache: Option<&wgpu::PipelineCache>) -> Self {
        let full_started = std::time::Instant::now();
        let tri_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Pathtrace-tri full (bg)"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_pathtrace_tri_wgsl_with_mode(Some(false)).into()),
        });
        let full_module_ms = full_started.elapsed().as_secs_f64() * 1000.0;
        let full_pipeline_started = std::time::Instant::now();
        let tri = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pathtrace_tri_full"), layout: Some(tri_layout), module: &tri_shader,
            entry_point: Some("pass_pathtrace_tri"), compilation_options: Default::default(), cache,
        });
        eprintln!("[TIMING] PT full bg: module={full_module_ms:.1}ms pipeline={:.1}ms", full_pipeline_started.elapsed().as_secs_f64() * 1000.0);
        if let Some(cache) = cache {
            super::cache::save_pipeline_cache(cache);
        }
        drop(tri_shader);
        let denoise_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pt_denoise_bgl"),
            entries: &graphics::bgl::pt_denoise_bgl_entries(),
        });
        let denoise_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pt_denoise_bg"), bind_group_layouts: &[&denoise_bgl], push_constant_ranges: &[],
        });
        let denoise_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pt_denoise (bg)"),
            source: wgpu::ShaderSource::Wgsl(graphics::PT_DENOISE_WGSL.into()),
        });
        let denoise = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pt_denoise"), layout: Some(&denoise_layout), module: &denoise_shader,
            entry_point: Some("pass_pt_denoise"), compilation_options: Default::default(), cache,
        });
        let denoise_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pt_denoise_params"), size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let upscale_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pt_upscale_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pt_upscale"),
            bind_group_layouts: &[&upscale_bgl],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PtUpscale (bg)"),
            source: wgpu::ShaderSource::Wgsl(graphics::PT_UPSCALE_WGSL.into()),
        });
        let upscale = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pt_upscale"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("pass_pt_upscale"),
            compilation_options: Default::default(),
            cache,
        });
        let upscale_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pt_upscale_params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { tri, denoise, denoise_bgl, denoise_params, upscale, upscale_bgl, upscale_params }
    }
}

#[cfg(test)]
mod pt_specialization_tests {
    use eframe::wgpu;

    #[test]
    fn specialized_pt_modes_validate_and_full_omits_reuse_roots() {
        for interactive in [false, true] {
            let source = graphics::build_pathtrace_tri_wgsl_with_mode(Some(interactive));
            let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|error| {
                panic!("PT mode {interactive} parse: {}", error.emit_to_string(&source))
            });
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                .validate(&module).expect("specialized PT validation");
            assert!(!module.overrides.iter().any(|(_, value)| value.name.as_deref() == Some("PT_INTERACTIVE")));
            let (_, mode) = module.constants.iter().find(|(_, value)| value.name.as_deref() == Some("PT_INTERACTIVE"))
                .expect("static PT mode constant");
            assert!(matches!(module.global_expressions[mode.init], naga::Expression::Literal(naga::Literal::Bool(value)) if value == interactive));
            for name in ["pt_restir_di", "pt_restir_gi"] {
                assert_eq!(module.functions.iter().any(|(_, function)| function.name.as_deref() == Some(name)), interactive,
                    "{name}: full mode must omit reuse functions before driver compilation");
            }
            for name in ["pt_hit_material", "pt_sample_bsdf"] {
                assert!(module.functions.iter().any(|(_, function)| function.name.as_deref() == Some(name)), "lost production material/transport function {name}");
            }
            if !interactive {
                assert!(module.functions.iter().any(|(_, function)| function.name.as_deref() == Some("pt_env_nee")), "full mode lost environment NEE");
            }
        }
        assert_eq!(graphics::build_pathtrace_tri_wgsl_with_mode(None), graphics::build_pathtrace_tri_wgsl());
    }

    #[test]
    #[ignore = "manual cache-disabled production BSDF subset compile/readback probe; coordinate GPU access"]
    fn cold_pt_mode_bsdf_subset_compile_report() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        for interactive in [false, true] {
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })).expect("PT subset probe fresh adapter");
            let (device, queue) = pollster::block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("pt_mode_subset_probe"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                },
            )).expect("PT subset probe device");
            device.on_uncaptured_error(Box::new(|error| panic!("PT subset probe: {error}")));
            let source = format!("{}\n@group(1) @binding(0) var<storage, read_write> probe_result: vec4<f32>;\n@compute @workgroup_size(1) fn pt_mode_subset_probe() {{ var seed = 17u; var pdf = 0.0; var f_cos = vec3<f32>(0.0); let roughness = select(0.75, 0.25, PT_INTERACTIVE); let direction = pt_sample_bsdf(vec3(0.0, 0.0, 1.0), vec3(0.0, 0.0, 1.0), vec3(0.5), roughness, 0.0, &seed, &pdf, &f_cos); probe_result = vec4(direction, pdf); }}",
                graphics::build_pathtrace_tri_wgsl_with_mode(Some(interactive)));
            let started = std::time::Instant::now();
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("production_pt_bsdf_subset"), source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let module_ms = started.elapsed().as_secs_f64() * 1000.0;
            let pipeline_started = std::time::Instant::now();
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("pt_mode_subset_probe"), layout: None, module: &shader,
                entry_point: Some("pt_mode_subset_probe"), compilation_options: Default::default(), cache: None,
            });
            let pipeline_ms = pipeline_started.elapsed().as_secs_f64() * 1000.0;
            eprintln!("[PT-COMPILE-PROBE] mode={interactive} boundary=production-BSDF-only app-cache=none module_ms={module_ms:.1} pipeline_ms={pipeline_ms:.1}; excludes main PT dispatch and reuse traversal");
            let output = device.create_buffer(&wgpu::BufferDescriptor {
                label: None, size: 16, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: None, size: 16, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
            });
            let empty = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None, layout: &pipeline.get_bind_group_layout(0), entries: &[],
            });
            let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None, layout: &pipeline.get_bind_group_layout(1),
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: output.as_entire_binding() }],
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &empty, &[]);
                pass.set_bind_group(1, &binding, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 16);
            queue.submit([encoder.finish()]);
            let (sender, receiver) = std::sync::mpsc::channel();
            readback.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = sender.send(result); });
            device.poll(wgpu::PollType::Wait).expect("PT subset GPU poll");
            receiver.recv().unwrap().expect("PT subset readback map");
            let mapped = readback.slice(..).get_mapped_range();
            let values: &[f32] = bytemuck::cast_slice(&mapped);
            assert!(values.iter().all(|value| value.is_finite()));
            let length_squared: f32 = values[..3].iter().map(|value| value * value).sum();
            assert!((length_squared - 1.0).abs() < 1e-4);
            assert!(values[3] > 0.0);
        }
    }
}


/// Lazily compile the RT projected-shadows visibility pass (RT_SHADOWS_DESIGN Stage 1) on the
/// first RT-shadow frame — never compiled for users who leave the toggle off. Creates the BGL
/// from `rt_shadow_bgl_entries()` (single source) + stores it for `build_rt_shadow_bg`. The
/// pass fires occlusion rays against the SAME triangle-PT BVH the display PT uses (shared
/// PT_TRAVERSAL region). No-op once compiled.
pub(crate) fn ensure_rt_shadow_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.rt_shadow_pipeline.is_some() && gpu.bind_groups.rt_shadow_bgl.is_some() { return; }
    let device = &gpu.device;
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let t = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rt_shadow_bgl"), entries: &graphics::bgl::rt_shadow_bgl_entries(),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rt_shadow"), bind_group_layouts: &[&bgl], push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RtShadow (lazy)"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_rt_shadow_wgsl().into()),
        });
        let p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rt_shadow"), layout: Some(&pl), module: &shader,
            entry_point: Some("rt_shadow"), compilation_options: Default::default(), cache: cache_ref,
        });
        (bgl, p)
    }));
    match result {
        Ok((bgl, p)) => {
            gpu.bind_groups.rt_shadow_bgl = Some(bgl);
            gpu.pipelines.rt_shadow_pipeline = Some(p);
            eprintln!("[TIMING] RT-shadow pipeline compiled (lazy): {:?}", t.elapsed());
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
            eprintln!("[SHADER] RT-shadow lazy compile failed: {}", msg);
            gpu.pipelines.shader_compile_error = Some(format!("RT-shadow pipeline: {}", msg));
        }
    }
}


/// Lazily compile the RT projected-shadow denoise passes (RT_SHADOWS_DESIGN Stage 2): the spatial
/// bilateral (`rt_shadow_denoise`) + the temporal EMA reprojection (`rt_shadow_temporal`). Compiled
/// together on the first SOFT RT-shadow frame (softness>0 && samples>1) — never while the hard
/// anchor path runs. Both BGLs come from `bgl.rs` (single source). No-op once compiled.
pub(crate) fn ensure_rt_denoise_pipelines(gpu: &mut GpuCtx) {
    if gpu.pipelines.rt_denoise_pipeline.is_some() && gpu.pipelines.rt_temporal_pipeline.is_some() { return; }
    let device = &gpu.device;
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let t = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rt_denoise_bgl"), entries: &graphics::bgl::rt_denoise_bgl_entries() });
        let d_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rt_denoise"), bind_group_layouts: &[&d_bgl], push_constant_ranges: &[] });
        let d_sh = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RtShadowDenoise (lazy)"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_rt_shadow_denoise_wgsl().into()) });
        let d_p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rt_denoise"), layout: Some(&d_pl), module: &d_sh,
            entry_point: Some("rt_shadow_denoise"), compilation_options: Default::default(), cache: cache_ref });
        let t_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rt_temporal_bgl"), entries: &graphics::bgl::rt_temporal_bgl_entries() });
        let t_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rt_temporal"), bind_group_layouts: &[&t_bgl], push_constant_ranges: &[] });
        let t_sh = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RtShadowTemporal (lazy)"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_rt_shadow_temporal_wgsl().into()) });
        let t_p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rt_temporal"), layout: Some(&t_pl), module: &t_sh,
            entry_point: Some("rt_shadow_temporal"), compilation_options: Default::default(), cache: cache_ref });
        (d_bgl, d_p, t_bgl, t_p)
    }));
    match result {
        Ok((d_bgl, d_p, t_bgl, t_p)) => {
            gpu.bind_groups.rt_denoise_bgl = Some(d_bgl);
            gpu.pipelines.rt_denoise_pipeline = Some(d_p);
            gpu.bind_groups.rt_temporal_bgl = Some(t_bgl);
            gpu.pipelines.rt_temporal_pipeline = Some(t_p);
            eprintln!("[TIMING] RT-shadow denoise pipelines compiled (lazy): {:?}", t.elapsed());
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
            eprintln!("[SHADER] RT-shadow denoise lazy compile failed: {}", msg);
            gpu.pipelines.shader_compile_error = Some(format!("RT-shadow denoise pipeline: {}", msg));
        }
    }
}


/// Tier 1: when true, the heavy ANALYTICAL-only WGSL pipelines (raymarch, raymarch-march, deferred-FULL,
/// pathtrace, fog) are NOT compiled eagerly at startup — they are compiled lazily via the `ensure_*` fns
/// the first time an analytical / Pbr-Lit / path-trace frame actually needs them. The production volume/
/// raster workflow (lite + volume-preview + deferred-lite + bake) never needs them, so default startup +
/// every per-primitive recompile drop from minutes to seconds. Set `false` to restore the old eager
/// compile (compiles everything up front) if a driver ever misbehaves on the lazy path.
pub(crate) const LAZY_ANALYTICAL_COMPILE: bool = true;


// (ensure_raymarch_pipeline / ensure_raymarch_march_pipeline REMOVED 2026-07-14 —
//  the WGSL analytical march display pipelines are stripped; only pipeline_lite remains.)

/// Lazily compile the selected-prim OUTLINE mask pass on first prim selection. Small stripped
/// stack (header + eval_prim only — no scene/material/lighting), so the one-time compile is
/// fast. AUTO pipeline layout: the entry references only a handful of the shared group-0
/// bindings (output_texture(0)=overlay, config(1), prim_data(3) + eval_prim's transitive
/// deps) — wgpu derives exactly that set, and the dispatch site builds its bind group from
/// `pipeline.get_bind_group_layout(0)` so the two can never disagree.
pub(crate) fn ensure_prim_outline_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.prim_outline_pipeline.is_some() { return; }
    let device = &gpu.device;
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let t = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PrimOutline (lazy)"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_prim_outline_wgsl().into()),
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("prim_outline"), layout: None, module: &shader,
            entry_point: Some("prim_outline"), compilation_options: Default::default(), cache: cache_ref,
        })
    }));
    match result {
        Ok(p) => { gpu.pipelines.prim_outline_pipeline = Some(p); eprintln!("[TIMING] Prim-outline pipeline compiled (lazy): {:?}", t.elapsed()); }
        Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
            eprintln!("[SHADER] Prim-outline lazy compile failed: {}", msg); gpu.pipelines.shader_compile_error = Some(format!("Prim-outline pipeline: {}", msg)); }
    }
}


/// Lazily compile the env GGX prefilter pipeline on the first HDRI install (env_ibl.rs).
/// Tiny self-contained compute (~ms compile) — the `ensure_*` fallback pattern; the pass is
/// budgeted (one mip per poll frame), never frame-critical.
pub(crate) fn ensure_env_prefilter_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.env_prefilter_pipeline.is_some() && gpu.bind_groups.env_prefilter_bgl.is_some() { return; }
    let device = &gpu.device;
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let t = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("env_prefilter_bgl"),
            entries: &graphics::bgl::env_prefilter_bgl_entries(),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("env_prefilter"), bind_group_layouts: &[&bgl], push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("EnvPrefilter (lazy)"),
            source: wgpu::ShaderSource::Wgsl(graphics::ENV_PREFILTER_WGSL.into()),
        });
        let p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("env_prefilter"), layout: Some(&pl), module: &shader,
            entry_point: Some("prefilter_mip"), compilation_options: Default::default(), cache: cache_ref,
        });
        (bgl, p)
    }));
    match result {
        Ok((bgl, p)) => {
            gpu.bind_groups.env_prefilter_bgl = Some(bgl);
            gpu.pipelines.env_prefilter_pipeline = Some(p);
            eprintln!("[TIMING] Env-prefilter pipeline compiled (lazy): {:?}", t.elapsed());
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
            eprintln!("[SHADER] Env-prefilter lazy compile failed: {}", msg);
            gpu.pipelines.shader_compile_error = Some(format!("Env-prefilter pipeline: {}", msg));
        }
    }
}


#[must_use]
pub(crate) fn ensure_pt_denoise_pipeline(gpu: &GpuCtx) -> bool {
    gpu.pipelines.pt_denoise_pipeline.is_some()
        && gpu.bind_groups.pt_denoise_bgl.is_some()
        && gpu.buffers.pt_denoise_params_buffer.as_ref().is_some_and(|buffer| {
            buffer.size() >= 32
                && buffer.usage().contains(wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST)
        })
}


/// Lazily compile the SSAO pipelines on the first enabled PBR/Lit frame if the
/// background compile hasn't landed yet (pt_denoise precedent). Tiny standalone
/// module (render/src/ssao.wgsl) — milliseconds, no draft-compile guard needed.
pub(crate) fn ensure_ssao_pipelines(gpu: &mut GpuCtx) {
    if gpu.pipelines.ssao.is_some() { return; }
    let t = std::time::Instant::now();
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    gpu.pipelines.ssao = Some(render::ssao::SsaoPipelines::new(&gpu.device, cache_ref));
    eprintln!("[TIMING] SSAO pipelines compiled (lazy): {:?}", t.elapsed());
}

/// Lazily compile the SSGI pipelines (SSGI_PLAN.md Gate 1). Not on the background
/// warmup path: SSGI ships default-OFF, so the compile happens on first enable
/// rather than costing every user startup time for a pass most never turn on.
/// The frames before it lands are covered by `SsgiResources::ensure_zero()` —
/// the composite must never sample an undispatched texture.
pub(crate) fn ensure_ssgi_pipelines(gpu: &mut GpuCtx) {
    if gpu.pipelines.ssgi.is_some() { return; }
    let t = std::time::Instant::now();
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    gpu.pipelines.ssgi = Some(graphics::ssgi::SsgiPipelines::new(&gpu.device, cache_ref));
    eprintln!("[TIMING] SSGI pipelines compiled (lazy): {:?}", t.elapsed());
}


/// Lazily compile the PT temporal reprojection pipeline (P3.5) on first camera
/// move in PT mode. Reprojects the accumulated PT image under camera motion
/// (per-pixel history in pt_accum alpha) instead of a global reset-to-black.
/// Tiny standalone shader — same lazy pattern as ensure_pt_denoise_pipeline.
pub(crate) fn ensure_pt_reproject_pipeline(gpu: &mut GpuCtx) {
    let need = std::mem::size_of::<graphics::PtReprojParams>() as u64;
    if gpu.pipelines.pt_reproject_pipeline.is_some() {
        // Section C grew PtReprojParams — recreate undersized uniforms + force BG rebuild.
        if gpu.buffers.pt_reproj_params_buffer.as_ref().map_or(true, |b| b.size() < need) {
            gpu.buffers.pt_reproj_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("PtReprojParams"),
                size: need,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            gpu.bind_groups.pt_reproject_bg = None;
            gpu.bind_groups.pt_reproject_bg_half = None;
        }
        return;
    }
    let device = &gpu.device;
    let t = std::time::Instant::now();

    // BGL — SINGLE SOURCE: graphics::bgl::pt_reproject_bgl_entries()
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pt_reproject_bgl"),
        entries: &graphics::bgl::pt_reproject_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pt_reproject (lazy)"),
        source: wgpu::ShaderSource::Wgsl(graphics::PT_REPROJECT_WGSL.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pt_reproject"), bind_group_layouts: &[&bgl], push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pt_reproject"), layout: Some(&pl), module: &shader,
        entry_point: Some("pt_reproject"), compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("PtReprojParams"),
        size: need,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    gpu.bind_groups.pt_reproject_bgl = Some(bgl);
    gpu.pipelines.pt_reproject_pipeline = Some(pipeline);
    gpu.buffers.pt_reproj_params_buffer = Some(params_buffer);
    gpu.bind_groups.pt_reproject_bg = None;
    gpu.bind_groups.pt_reproject_bg_half = None;
    eprintln!("[TIMING] PT reproject pipeline compiled (lazy): {:?}", t.elapsed());
}


/// Section I: lazily compile ReSTIR reservoir reprojection (camera-motion temporal).
/// Uses an independent `PtReprojParams` uniform. Writes rgba32f reservoirs.
pub(crate) fn ensure_pt_restir_reproject_pipeline(gpu: &mut GpuCtx) {
    let need = std::mem::size_of::<graphics::PtReprojParams>() as u64;
    if gpu.buffers.pt_restir_reproj_params_buffer.as_ref().map_or(true, |buffer| {
        buffer.size() < need
            || !buffer.usage().contains(wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST)
    }) {
        gpu.buffers.pt_restir_reproj_params_buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PtRestirReprojParams"),
            size: need,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        gpu.bind_groups.pt_restir_reproject_bg = None;
        gpu.bind_groups.pt_restir_reproject_bg_half = None;
    }
    if gpu.pipelines.pt_restir_reproject_pipeline.is_some()
        && gpu.bind_groups.pt_restir_reproject_bgl.is_some()
    {
        return;
    }
    let device = &gpu.device;
    let t = std::time::Instant::now();
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pt_restir_reproject_bgl"),
        entries: &graphics::bgl::pt_restir_reproject_bgl_entries(),
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pt_restir_reproject (lazy)"),
        source: wgpu::ShaderSource::Wgsl(graphics::PT_RESTIR_REPROJECT_WGSL.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pt_restir_reproject"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pt_restir_reproject"),
        layout: Some(&pl),
        module: &shader,
        entry_point: Some("pt_restir_reproject"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    gpu.bind_groups.pt_restir_reproject_bgl = Some(bgl);
    gpu.pipelines.pt_restir_reproject_pipeline = Some(pipeline);
    gpu.bind_groups.pt_restir_reproject_bg = None;
    gpu.bind_groups.pt_restir_reproject_bg_half = None;
    eprintln!("[TIMING] PT restir-reproject pipeline compiled (lazy): {:?}", t.elapsed());
}


/// Lazily compile the primary GPU bake pipeline on first use (Level 2).
///
/// Creates the BindGroupLayout, ComputePipeline, and fixed-size BakeParams uniform
/// buffer for dispatching volume bakes on the primary GPU within the render frame's
/// command encoder.  Prim and output buffers are left as None — they are grown on
/// demand at dispatch time (later wave).
pub(crate) fn ensure_primary_bake_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.primary_bake_pipeline.is_some() { return; }
    let device = &gpu.device;
    let t = std::time::Instant::now();

    // ── BindGroupLayout — SINGLE SOURCE: graphics::bgl::bake_bgl_entries() ──
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("primary_bake_bgl"),
        entries: &graphics::bgl::bake_bgl_entries(),
    });

    // ── Shader module ──
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("volume_bake.wgsl (primary)"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()
        ),
    });

    // ── Pipeline layout + compute pipeline ──
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("primary_bake_pipeline_layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("primary_bake_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("bake_volume"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });

    // ── BakeParams uniform buffer (fixed size) ──
    let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("primary_bake_params"),
        size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    gpu.bind_groups.primary_bake_bgl = Some(bgl);
    gpu.pipelines.primary_bake_pipeline = Some(pipeline);
    gpu.buffers.primary_bake_params_buffer = Some(params_buffer);
    ensure_lab_mesh_grid_dummy(gpu);
    eprintln!("[TIMING] Primary bake pipeline compiled (lazy): {:?}", t.elapsed());
}


/// STAGE-2 GPU CLASSIFY — lazy fallback for the `classify_bricks` pipeline (normally
/// hot-swapped by the background compile alongside the bake pipeline; `ensure_*` functions are
/// fallbacks only, per the Frame Budget rule). Same volume_bake.wgsl module + bake BGL —
/// `ensure_primary_bake_pipeline` is the prerequisite (BGL + params buffer + mesh_grid dummy).
pub(crate) fn ensure_classify_bricks_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.classify_bricks_pipeline.is_some() { return; }
    ensure_primary_bake_pipeline(gpu);
    let device = &gpu.device;
    let t = std::time::Instant::now();
    let bgl = match &gpu.bind_groups.primary_bake_bgl {
        Some(b) => b,
        None => return, // bake pipeline compile failed — caller falls back to the CPU worker
    };
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("volume_bake.wgsl (classify, lazy)"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("classify_bricks_pipeline_layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("classify_bricks_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("classify_bricks"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    gpu.pipelines.classify_bricks_pipeline = Some(pipeline);
    // STAGE-3 GPU FINALIZE: compiled as a set with the classify (same module, one-shot lazy
    // fallback) so a GPU classify always has its on-device finalize available.
    gpu.pipelines.classify_finalize_pipelines = Some(compile_classify_finalize_pipelines(
        device, &pipeline_layout, &shader, (*gpu.pipelines.pipeline_cache).as_ref()));
    eprintln!("[TIMING] classify_bricks(+finalize) pipelines compiled (lazy): {:?}", t.elapsed());
}


/// GPU CURVATURE (`docs/planning/GPU_CURVATURE_KERNEL_PLAN.md` P1) — lazy compile of the
/// `curvature_eval` entry of volume_bake.wgsl. Same module + same bake BGL as the bake and the
/// classify: `ensure_primary_bake_pipeline` is the prerequisite (it owns the BGL). Lazy rather
/// than background-compiled because a curvature bake only happens when the user wires an
/// SdfCurvature / Curvature / Cavity node — most sessions never compile it.
///
/// Returns `false` when the pipeline is unavailable (bake BGL missing), which is the caller's
/// signal to run the CPU `sdf_curvature_masks` path unchanged — a curvature bake NEVER fails
/// for want of a GPU.
pub(crate) fn ensure_curvature_eval_pipeline(gpu: &mut GpuCtx) -> bool {
    if gpu.pipelines.curvature_eval_pipeline.is_some() { return true; }
    ensure_primary_bake_pipeline(gpu);
    let device = &gpu.device;
    let t = std::time::Instant::now();
    let bgl = match &gpu.bind_groups.primary_bake_bgl {
        Some(b) => b,
        None => return false, // bake pipeline compile failed — caller falls back to the CPU path
    };
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("volume_bake.wgsl (curvature, lazy)"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into()
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("curvature_eval_pipeline_layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("curvature_eval_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("curvature_eval"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    gpu.pipelines.curvature_eval_pipeline = Some(pipeline);
    eprintln!("[TIMING] curvature_eval pipeline compiled (lazy): {:?}", t.elapsed());
    true
}


/// O1 — compile `counters[0] → (wx,wy,1)` indirect-dispatch args pipeline (lazy, once).
pub(crate) fn ensure_curv_dispatch_args_pipeline(gpu: &mut GpuCtx) -> bool {
    if gpu.pipelines.curv_dispatch_args_pipeline.is_some() {
        return true;
    }
    let device = &gpu.device;
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("curv_dispatch_args_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
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
    // Live vert count (counters[0]) → workgroup grid matching `curvature_dispatch_dims`.
    const WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> counters: array<u32>;
@group(0) @binding(1) var<storage, read_write> args: array<u32>;

@compute @workgroup_size(1)
fn main() {
    let n = counters[0];
    if (n == 0u) {
        args[0] = 0u;
        args[1] = 0u;
        args[2] = 0u;
        return;
    }
    let total_wg = (n + 63u) / 64u;
    let wx = min(total_wg, 65535u);
    let wy = min((total_wg + wx - 1u) / wx, 65535u);
    args[0] = wx;
    args[1] = wy;
    args[2] = 1u;
}
"#;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("curv_dispatch_args"),
        source: wgpu::ShaderSource::Wgsl(WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("curv_dispatch_args_pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("curv_dispatch_args_pipeline"),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    gpu.pipelines.curv_dispatch_args_pipeline = Some((bgl, pipeline));
    true
}


/// O1 pooled — compile sum of per-tile `tile_counters` → body live count (lazy, once).
pub(crate) fn ensure_curv_sum_tile_counters_pipeline(gpu: &mut GpuCtx) -> bool {
    if gpu.pipelines.curv_sum_tile_counters_pipeline.is_some() {
        return true;
    }
    let device = &gpu.device;
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("curv_sum_tile_counters_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
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
    // Sum tile_counters[(args_base+e)*2] and +1 over e in 0..n_tiles into out[0]/out[1].
    // `accumulate` (p.z): 0 = overwrite, 1 = add (multi-segment bodies, sequential passes).
    const WGSL: &str = r#"
struct Params { args_base: u32, n_tiles: u32, accumulate: u32, _pad: u32, }
@group(0) @binding(0) var<storage, read> tile_counters: array<u32>;
@group(0) @binding(1) var<storage, read_write> out_counters: array<u32>;
@group(0) @binding(2) var<uniform> p: Params;

@compute @workgroup_size(1)
fn main() {
    var v = 0u;
    var i = 0u;
    let n = p.n_tiles;
    let base = p.args_base;
    for (var e = 0u; e < n; e = e + 1u) {
        let idx = (base + e) * 2u;
        v = v + tile_counters[idx];
        i = i + tile_counters[idx + 1u];
    }
    if (p.accumulate == 0u) {
        out_counters[0] = v;
        out_counters[1] = i;
    } else {
        // Single-thread sequential passes share the encoder — safe RMW between dispatches.
        out_counters[0] = out_counters[0] + v;
        out_counters[1] = out_counters[1] + i;
    }
}
"#;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("curv_sum_tile_counters"),
        source: wgpu::ShaderSource::Wgsl(WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("curv_sum_tile_counters_pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("curv_sum_tile_counters_pipeline"),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });
    gpu.pipelines.curv_sum_tile_counters_pipeline = Some((bgl, pipeline));
    true
}


/// Disabled-state dummy for the bake shader's mesh_grid operand (@binding 5).
/// 8 zeroed header floats (res = 0.0 → sample_mesh_grid returns 1e6, union no-op).
/// Bound whenever no LAB mesh operand is loaded. Created once; the real operand
/// buffer (lab_mesh_grid_buffer) replaces it in the bind group, never in place.
pub(crate) fn ensure_lab_mesh_grid_dummy(gpu: &mut GpuCtx) {
    if gpu.buffers.lab_mesh_grid_dummy.is_some() { return; }
    gpu.buffers.lab_mesh_grid_dummy = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lab_mesh_grid_dummy"),
        size: 8 * 4,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    }));
}


/// Lazily compile the brick fill pipeline on first use (Phase 4).
///
/// Bindings:
///   0: prim_hot (storage, read-only) — same as volume_bake
///   1: params (uniform, BakeParams) — same as volume_bake
///   2: brick_pool (storage, read-write) — shared pool SSBO
///   3: brick_list (storage, read-only) — sub-brick dispatch list
pub(crate) fn ensure_brick_fill_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.brick_fill_pipeline.is_some() { return; }
    let device = &gpu.device;
    let t = std::time::Instant::now();

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brick_fill_bgl"),
        entries: &[
            // binding(0): prim_hot
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // binding(1): params (BakeParams uniform)
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
            // binding(2): atlas texture (brick fill writes directly to atlas)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::R32Float,
                    view_dimension: wgpu::TextureViewDimension::D3,
                },
                count: None,
            },
            // binding(3): brick_list (read-only storage)
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // binding(4): clip_curves (read-only storage)
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
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brick_fill.wgsl"),
        source: wgpu::ShaderSource::Wgsl(
            graphics::build_brick_fill_wgsl().into()
        ),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brick_fill_pipeline_layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("brick_fill_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("brick_fill"),
        compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    });

    gpu.bind_groups.brick_fill_bgl = Some(bgl);
    gpu.pipelines.brick_fill_pipeline = Some(pipeline);
    eprintln!("[TIMING] Brick fill pipeline compiled (lazy): {:?}", t.elapsed());
}


/// Lazily compile the temporal reprojection pipeline on first use (Level 7).
pub(crate) fn ensure_temporal_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.temporal_reproject_pipeline.is_some() { return; }
    let device = &gpu.device;
    let t = std::time::Instant::now();

    // BindGroupLayout for temporal_reproject.wgsl
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("temporal_reproject_bgl"),
        entries: &[
            // @binding(0) prev_normal_dist: texture_2d<f32>
            wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            // @binding(1) params: uniform TemporalParams
            wgpu::BindGroupLayoutEntry {
                binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            // @binding(2) validity_mask: texture_storage_2d<r32uint, write>
            wgpu::BindGroupLayoutEntry {
                binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::R32Uint, view_dimension: wgpu::TextureViewDimension::D2 },
                count: None,
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("temporal_reproject (lazy)"),
        source: wgpu::ShaderSource::Wgsl(graphics::TEMPORAL_REPROJECT_WGSL.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("temporal_reproject"), bind_group_layouts: &[&bgl], push_constant_ranges: &[],
    });
    gpu.pipelines.temporal_reproject_pipeline = Some(device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("temporal_reproject"), layout: Some(&pl), module: &shader,
        entry_point: Some("temporal_reproject"), compilation_options: Default::default(),
        cache: (*gpu.pipelines.pipeline_cache).as_ref(),
    }));

    // Allocate the TemporalParams uniform buffer
    gpu.buffers.temporal_params_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("TemporalParams"),
        size: std::mem::size_of::<graphics::TemporalParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));

    gpu.bind_groups.temporal_reproject_bgl = Some(bgl);
    eprintln!("[TIMING] Temporal reproject pipeline compiled (lazy): {:?}", t.elapsed());
}


/// Lazily compile the blend/composite pipeline on first use.
pub(crate) fn ensure_blend_pipeline(gpu: &mut GpuCtx) {
    if gpu.pipelines.blend_pipeline.is_some() { return; }
    let device = &gpu.device;
    let t = std::time::Instant::now();
    // Create BGL if needed
    let bgl = gpu.bind_groups.blend_bgl.get_or_insert_with(|| {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend_bgl"),
            entries: &graphics::bgl::blend_bgl_entries(),
        })
    });
    let cache_ref = (*gpu.pipelines.pipeline_cache).as_ref();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blend (lazy)"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../../graphics/src/composite_blend.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend"), bind_group_layouts: &[bgl], push_constant_ranges: &[],
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blend"), layout: Some(&pl), module: &shader,
            entry_point: Some("blend_main"), compilation_options: Default::default(),
            cache: cache_ref,
        })
    }));
    match result {
        Ok(p) => {
            gpu.pipelines.blend_pipeline = Some(p);
            eprintln!("[TIMING] Blend pipeline compiled (lazy): {:?}", t.elapsed());
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().map(|s| s.as_str())
                .or_else(|| e.downcast_ref::<&str>().copied())
                .unwrap_or("unknown error");
            eprintln!("[SHADER] Blend pipeline compile failed: {}", msg);
        }
    }
}
