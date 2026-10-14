use super::{NativeRtError, REQUIRED_FEATURES};
use eframe::wgpu;

pub struct NativeRtPipelines {
    pub(super) pack_layout: wgpu::BindGroupLayout,
    pub(super) pack: wgpu::ComputePipeline,
    pub(super) acceleration_layout: wgpu::BindGroupLayout,
    pub(super) full: wgpu::ComputePipeline,
    pub(super) interactive: wgpu::ComputePipeline,
}

impl NativeRtPipelines {
    pub fn compile(
        device: &wgpu::Device,
        canonical_material_layout: &wgpu::BindGroupLayout,
        full_wgsl: &str,
        interactive_wgsl: &str,
        cache: Option<&wgpu::PipelineCache>,
    ) -> Result<Self, NativeRtError> {
        if !device.features().contains(REQUIRED_FEATURES) {
            return Err(NativeRtError("native RT features were not enabled on the device"));
        }
        let pack_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Native RT index pack layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(16) }, count: None,
                },
            ],
        });
        let pack_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Native RT index pack"),
            source: wgpu::ShaderSource::Wgsl(include_str!("pack_indices.wgsl").into()),
        });
        let pack_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Native RT index pack pipeline layout"),
            bind_group_layouts: &[&pack_layout], push_constant_ranges: &[],
        });
        let pack = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Native RT index pack pipeline"), layout: Some(&pack_pipeline_layout),
            module: &pack_module, entry_point: Some("pack_indices"),
            compilation_options: Default::default(), cache,
        });
        let acceleration_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Native RT acceleration layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::AccelerationStructure { vertex_return: false }, count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Native RT canonical PT layout"),
            bind_group_layouts: &[canonical_material_layout, &acceleration_layout], push_constant_ranges: &[],
        });
        let compile_variant = |source: &str, interactive: bool| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(if interactive { "Native RT interactive PT shader" } else { "Native RT full PT shader" }),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(if interactive { "Native RT interactive PT" } else { "Native RT full PT" }),
                layout: Some(&layout), module: &module, entry_point: Some("pass_pathtrace_tri"),
                compilation_options: Default::default(), cache,
            })
        };
        Ok(Self { pack_layout, pack, acceleration_layout, full: compile_variant(full_wgsl, false), interactive: compile_variant(interactive_wgsl, true) })
    }
}
