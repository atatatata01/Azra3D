use wgpu::hal::{self, Device as _};

fn main() -> Result<(), String> {
    let path = std::env::args_os().nth(1).ok_or("expected validated native RT SPIR-V path")?;
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() % 4 != 0 {
        return Err("SPIR-V byte length is not a multiple of four".into());
    }
    let words: Vec<u32> = bytes.chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap())).collect();
    if words.first() != Some(&0x07230203) {
        return Err("invalid SPIR-V header".into());
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let required = wgpu::Features::RAY_TRACING_ACCELERATION_STRUCTURE | wgpu::Features::RAY_QUERY;
    let adapter = instance.enumerate_adapters(wgpu::Backends::VULKAN).into_iter()
        .find(|adapter| adapter.features().contains(required))
        .ok_or("no Vulkan adapter exposes acceleration structures and ray queries")?;
    println!("Adapter: {:?}; required features: {:?}", adapter.get_info(), required);
    let (device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("native RT full-material pipeline validation"),
        required_features: required | (adapter.features() & wgpu::Features::FLOAT32_FILTERABLE),
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
    }, None)).map_err(|error| error.to_string())?;
    let mut material_entries = graphics::bgl::pt_bgl_entries();
    material_entries.sort_by_key(|entry| entry.binding);
    unsafe {
        device.as_hal::<hal::api::Vulkan, _, _>(|native| {
            let native = native.ok_or("Vulkan device unavailable")?;
            validate_pipeline(native, &material_entries, &words)
        })
    }.ok_or("native HAL access is unavailable")?
}

unsafe fn validate_pipeline(
    device: &hal::vulkan::Device,
    material_entries: &[wgpu::BindGroupLayoutEntry],
    words: &[u32],
) -> Result<(), String> {
    let material_layout = device.create_bind_group_layout(&hal::BindGroupLayoutDescriptor {
        label: Some("canonical PT materials and integration"),
        flags: hal::BindGroupLayoutFlags::empty(),
        entries: material_entries,
    }).map_err(|error| error.to_string())?;
    let acceleration_layout = device.create_bind_group_layout(&hal::BindGroupLayoutDescriptor {
        label: Some("native triangle TLAS"),
        flags: hal::BindGroupLayoutFlags::empty(),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::AccelerationStructure,
            count: None,
        }],
    }).map_err(|error| error.to_string())?;
    let layout = device.create_pipeline_layout(&hal::PipelineLayoutDescriptor {
        label: Some("full material native RT"),
        flags: hal::PipelineLayoutFlags::empty(),
        bind_group_layouts: &[&material_layout, &acceleration_layout],
        push_constant_ranges: &[],
    }).map_err(|error| error.to_string())?;
    let shader = device.create_shader_module(&hal::ShaderModuleDescriptor {
        label: Some("Naga 25 validated WGSL with native traversal"),
        runtime_checks: true,
    }, hal::ShaderInput::SpirV(words)).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let result = device.create_compute_pipeline(&hal::ComputePipelineDescriptor {
        label: Some("complete triangle material shader native RT"),
        layout: &layout,
        stage: hal::ProgrammableStage {
            module: &shader,
            entry_point: "pass_pathtrace_tri",
            constants: &Default::default(),
            zero_initialize_workgroup_memory: true,
            vertex_pulling_transform: false,
        },
        cache: None,
    });
    let result = match result {
        Ok(pipeline) => {
            device.destroy_compute_pipeline(pipeline);
            println!("Full material native ray-query pipeline created in {:?}; no dispatch or routing enabled", started.elapsed());
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    };
    device.destroy_shader_module(shader);
    device.destroy_pipeline_layout(layout);
    device.destroy_bind_group_layout(acceleration_layout);
    device.destroy_bind_group_layout(material_layout);
    result
}
