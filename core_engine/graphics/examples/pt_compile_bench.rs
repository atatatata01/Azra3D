//! Driver-compile benchmark for the triangle path tracer (dev tool, not shipped).
//!
//! `cargo run -p graphics --release --example pt_compile_bench -- <variant> [dump.wgsl]`
//! variant: `full` | `interactive` | `file=<path.wgsl>`
//!
//! Times Naga (shader module) and the driver (compute pipeline) separately on the real
//! Vulkan adapter, with NO pipeline cache, so a cold compile is measured. Run it under a
//! memory/time watchdog: the 2026-10-12 probe saw the PT driver compile grow past 90 GB.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let variant = args.get(1).map(String::as_str).unwrap_or("full");
    let source = match variant {
        "full" => graphics::build_pathtrace_tri_wgsl_with_mode(Some(false)),
        "interactive" => graphics::build_pathtrace_tri_wgsl_with_mode(Some(true)),
        "deferred" => graphics::build_deferred_wgsl(None),
        other => match other.strip_prefix("file=") {
            Some(path) => std::fs::read_to_string(path).expect("read WGSL file"),
            None => panic!("unknown variant {other}"),
        },
    };
    // Unique unused function → unique SPIR-V → the driver's disk cache can never hit.
    let salt = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u32;
    let source = format!("{source}
fn pt_bench_salt_{salt}() -> u32 {{ return {salt}u; }}
");
    if let Some(dump) = args.get(2) {
        std::fs::write(dump, &source).expect("dump WGSL");
    }
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..Default::default() });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("Vulkan adapter");
    let mut limits = wgpu::Limits::default();
    let al = adapter.limits();
    limits.max_storage_buffers_per_shader_stage = al.max_storage_buffers_per_shader_stage;
    limits.max_storage_textures_per_shader_stage = al.max_storage_textures_per_shader_stage;
    limits.max_sampled_textures_per_shader_stage = al.max_sampled_textures_per_shader_stage;
    limits.max_bindings_per_bind_group = al.max_bindings_per_bind_group;
    limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
    let (device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("pt_compile_bench"),
        required_features: adapter.features() & (wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::SHADER_INT64
            | if std::env::var_os("BENCH_RT").is_some() {
                // Same mask the app requests when the adapter supports native RT.
                wgpu::Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE | wgpu::Features::EXPERIMENTAL_RAY_QUERY
            } else { wgpu::Features::empty() }
            | if std::env::var_os("BENCH_CACHE").is_some() { wgpu::Features::PIPELINE_CACHE } else { wgpu::Features::empty() }),
        required_limits: limits,
        memory_hints: Default::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("device");
    device.on_uncaptured_error(Box::new(|error| panic!("wgpu error: {error}")));
    eprintln!("[bench] {variant}: {} lines, {:.1} KB WGSL", source.lines().count(), source.len() as f64 / 1024.0);
    let t = std::time::Instant::now();
    // BENCH_NO_LOOP_BOUND=1: keep bounds checks, drop naga's injected loop-bounding counters.
    let desc = wgpu::ShaderModuleDescriptor { label: Some("pt_bench"), source: wgpu::ShaderSource::Wgsl(source.into()) };
    let module = if std::env::var_os("BENCH_NO_LOOP_BOUND").is_some() {
        eprintln!("[bench] force_loop_bounding=false (bounds_checks=true)");
        unsafe {
            device.create_shader_module_trusted(desc, wgpu::ShaderRuntimeChecks { bounds_checks: std::env::var_os("BENCH_UNCHECKED").is_none(), force_loop_bounding: false })
        }
    } else {
        device.create_shader_module(desc)
    };
    let module_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[bench] naga module: {module_ms:.1} ms — driver compile starting");
    // BENCH_LAYOUT=1: the production explicit layout (`pt_bgl_entries`) instead of `auto`.
    // BENCH_CACHE=1: an (empty) VkPipelineCache like the app's warmup passes.
    let layout = std::env::var_os("BENCH_LAYOUT").map(|_| {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pt_bench_bgl"),
            entries: &if variant == "deferred" { graphics::bgl::deferred_bgl_entries() } else { graphics::bgl::pt_bgl_entries() },
        });
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pt_bench_layout"), bind_group_layouts: &[&bgl], push_constant_ranges: &[],
        })
    });
    let cache = std::env::var_os("BENCH_CACHE").map(|_| unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor { label: Some("pt_bench_cache"), data: None, fallback: true })
    });
    eprintln!("[bench] explicit layout={} pipeline cache={}", layout.is_some(), cache.is_some());
    let t = std::time::Instant::now();
    let _pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pt_bench"),
        layout: layout.as_ref(),
        module: &module,
        entry_point: Some(if variant == "deferred" { "deferred_shade" } else { "pass_pathtrace_tri" }),
        compilation_options: Default::default(),
        cache: cache.as_ref(),
    });
    println!("[bench] {variant}: naga {module_ms:.1} ms, pipeline (SPIR-V + driver) {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
}
