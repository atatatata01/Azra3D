use bytemuck::{Pod, Zeroable};

// ============================================================
// Depth-Tested Face Selection Overlay
// ============================================================
//
// Transparent triangle fills for selected poly faces, depth-tested against the
// G-buffer solid depth (Blender Overlay parity — see face_overlay.wgsl).
// Architecture mirrors `line_overlay.rs`: no depth attachment, occlusion via
// depth TEXTURE sample in the fragment shader.

/// One world-space vertex of a selected-face triangle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FaceVertex {
    pub position: [f32; 3],
    /// Packed RGBA8 (Unorm8x4). Build with `FaceVertex::pack_color`.
    pub color: u32,
}

impl FaceVertex {
    pub fn pack_color(r: f32, g: f32, b: f32, a: f32) -> u32 {
        let q = |v: f32| -> u32 { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32 };
        q(r) | (q(g) << 8) | (q(b) << 16) | (q(a) << 24)
    }

    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<FaceVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 12,
                shader_location: 1,
            },
        ],
    };
}

/// Colour target format — same LDR target as the line overlay.
pub const FACE_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub struct FaceOverlayPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub pipeline_xray: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub bind_group: Option<wgpu::BindGroup>,
    /// `RasterPipeline.depth_generation` when bind_group was built.
    pub bind_group_depth_gen: u64,
    pub vertex_buffer: wgpu::Buffer,
    pub vertex_capacity: usize,
    vertex_shrink_counter: u32,
}

impl FaceOverlayPipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("face_overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("face_overlay.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("face_overlay_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("face_overlay_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let targets = [Some(wgpu::ColorTargetState {
            format: FACE_TARGET_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let make = |label: &str, fs_entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[FaceVertex::LAYOUT],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // Selection fill must show both sides when X-Ray is off and the user
                    // has "show backface selection" — cull none; CPU already filters.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs_entry),
                    targets: &targets,
                    compilation_options: Default::default(),
                }),
                multiview: None,
                cache: None,
            })
        };

        let pipeline = make("face_overlay_depth", "fs_main");
        let pipeline_xray = make("face_overlay_xray", "fs_xray");

        let initial_cap = 256;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("face_overlay_verts"),
            size: (initial_cap * std::mem::size_of::<FaceVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            pipeline_xray,
            bind_group_layout: bgl,
            bind_group: None,
            bind_group_depth_gen: u64::MAX,
            vertex_buffer,
            vertex_capacity: initial_cap,
            vertex_shrink_counter: 0,
        }
    }

    pub fn upload_vertices(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[FaceVertex],
    ) {
        const MIN_CAPACITY: usize = 256;
        let needed = vertices.len().max(1);

        let mut recreate = false;
        if needed > self.vertex_capacity {
            self.vertex_capacity = needed.next_power_of_two();
            recreate = true;
        }
        self.vertex_shrink_counter += 1;
        if self.vertex_shrink_counter >= 60 {
            self.vertex_shrink_counter = 0;
            if needed < self.vertex_capacity / 4 && self.vertex_capacity > MIN_CAPACITY {
                self.vertex_capacity = needed.next_power_of_two().max(MIN_CAPACITY);
                recreate = true;
            }
        }
        if recreate {
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("face_overlay_verts"),
                size: (self.vertex_capacity * std::mem::size_of::<FaceVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(vertices));
        }
    }

    pub fn rebuild_bind_group(
        &mut self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
        depth_view: &wgpu::TextureView,
        depth_gen: u64,
    ) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("face_overlay_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(depth_view),
                },
            ],
        }));
        self.bind_group_depth_gen = depth_gen;
    }

    /// Draw `count` vertices (must be multiple of 3). `xray` selects the always-visible path.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, count: u32, xray: bool) {
        if count == 0 {
            return;
        }
        let Some(ref bg) = self.bind_group else {
            return;
        };
        pass.set_pipeline(if xray {
            &self.pipeline_xray
        } else {
            &self.pipeline
        });
        pass.set_bind_group(0, bg, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_vertex_size_matches_layout() {
        assert_eq!(std::mem::size_of::<FaceVertex>(), 16);
        assert_eq!(FaceVertex::LAYOUT.array_stride, 16);
    }

    #[test]
    fn face_overlay_wgsl_parses_and_validates() {
        let src = include_str!("face_overlay.wgsl");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("face_overlay.wgsl parse failed: {e:?}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("face_overlay.wgsl validation failed: {e:?}"));
    }

    #[test]
    fn pack_color_roundtrip() {
        assert_eq!(FaceVertex::pack_color(1.0, 0.0, 0.0, 1.0), 0xFF0000FF);
        assert_eq!(FaceVertex::pack_color(0.0, 0.0, 1.0, 0.5), 0x80FF0000);
    }
}
