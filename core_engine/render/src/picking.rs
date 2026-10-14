// ============================================================
// GPU Color-ID Picking Pipeline
// ============================================================
//
// Renders all rasterized meshes with flat per-instance node IDs
// into an Rgba8Uint texture. On click, a single pixel is copied
// to a staging buffer and read back to identify the clicked node.

use wgpu;
use crate::mesh_gpu::{RasterVertex, InstanceData};
use crate::camera::CameraUniforms;

/// Alignment for buffer-to-texture copies (wgpu requires 256).
const COPY_BYTES_PER_ROW_ALIGN: u32 = 256;

pub struct PickingPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
    pub camera_buffer: wgpu::Buffer,
    pub bind_group: Option<wgpu::BindGroup>,
    // Render targets
    pub color_texture: Option<wgpu::Texture>,
    pub color_view: Option<wgpu::TextureView>,
    pub depth_texture: Option<wgpu::Texture>,
    pub depth_view: Option<wgpu::TextureView>,
    pub size: (u32, u32),
    // Readback
    pub staging_buffer: wgpu::Buffer,
    /// Poly sub-element ID pass (Blender select-buffer model): world-space {pos, id} vertices,
    /// no instance buffer. Shares the camera BGL + Rgba8Uint/depth targets with the node-pick pass.
    /// Three pipelines over ONE shader + ONE vertex buffer (range-drawn): faces (triangles),
    /// edges (lines, small depth bias toward the viewer), verts (points, bigger bias) — the
    /// bias ladder is Blender's select-engine priority: an element must WIN the depth race
    /// against its own coplanar face, or reading its own id back is impossible.
    pub poly_pipeline: wgpu::RenderPipeline,
    pub poly_edge_pipeline: wgpu::RenderPipeline,
    pub poly_vert_pipeline: wgpu::RenderPipeline,
    /// BGL + bind group for the element pass (camera + sampled face-pass depth). The bind group
    /// is rebuilt whenever the depth texture is recreated (`ensure_textures`) — generation-free
    /// because the only owner of both is this struct and recreation invalidates in the same call.
    pub poly_elem_bgl: wgpu::BindGroupLayout,
    pub poly_elem_bind_group: Option<wgpu::BindGroup>,
    /// Capacity-backed VB for poly face-ID pass (POLY_VIEWPORT_PERF P5 — no create_buffer_init
    /// every settle). Grows to next power-of-two; rewritten in place via `write_buffer`.
    pub poly_vertex_buffer: wgpu::Buffer,
    pub poly_vertex_capacity: usize,
    /// Full-image staging for the poly select buffer (whole viewport, 256-aligned rows).
    /// Recreated on size change; only ONE readback may be in flight (owner-enforced in the app).
    pub full_staging: Option<wgpu::Buffer>,
    pub full_staging_size: (u32, u32, u32), // (w, h, padded_bytes_per_row)
}

/// Vertex of the poly sub-element ID pass: world-space position + element id (face_index + 1).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PolyPickVertex {
    pub pos: [f32; 3],
    pub id: u32,
}

impl PolyPickVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<PolyPickVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
            wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Uint32 },
        ],
    };
}

impl PickingPipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("picking"),
            source: wgpu::ShaderSource::Wgsl(include_str!("picking.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("picking_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("picking_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("picking"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_picking"),
                buffers: &[RasterVertex::LAYOUT, InstanceData::LAYOUT],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // NO back-face cull — same rule as the poly sub-element pick below: pick
                // visibility is decided by DEPTH alone. The main raster is two-sided
                // (pipeline.rs cull_mode: None + the fragment normal flip), so a single-faced
                // body wound away from the camera (a poly plane) still RENDERS — culling here
                // made exactly those bodies invisible to click-select.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_picking"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Uint,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("picking_camera"),
            size: std::mem::size_of::<CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Staging buffer: 1 row padded to 256 bytes (wgpu alignment)
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("picking_staging"),
            size: COPY_BYTES_PER_ROW_ALIGN as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Poly sub-element ID pass — same camera BGL + targets, {pos,id} vertices, no instancing.
        let poly_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("poly_pick"),
            source: wgpu::ShaderSource::Wgsl(include_str!("poly_pick.wgsl").into()),
        });
        let poly_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("poly_pick_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        // FACE pass: normal depth test + write, into the depth attachment.
        let poly_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("poly_pick"),
            layout: Some(&poly_layout),
            vertex: wgpu::VertexState {
                module: &poly_shader,
                entry_point: Some("vs_poly_pick"),
                buffers: &[PolyPickVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // NO back-face cull: visibility must be decided by DEPTH alone (Blender's select
                // buffer draws both sides too); a front-facing test is what the old CPU heuristics
                // got wrong at silhouettes.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &poly_shader,
                entry_point: Some("fs_poly_pick"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Uint,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        // ELEMENT pass (edges + verts): NO depth attachment. The fragment shader SAMPLES the
        // face-pass depth as a 3×3 window max and discards only when something is genuinely in
        // front. A hardware depth bias cannot do this job: slope-scaled bias derives from the
        // primitive being drawn, and a POINT has zero slope, so verts got only a ULP constant
        // against the face's real per-pixel gradient — about half of a sphere's verts lost the
        // race and disappeared (owner screenshot 2026-08-07). Sampling the neighborhood max
        // expresses the slope exactly, with no tunable constant and no backend dependence.
        let poly_elem_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("poly_pick_elem_bgl"),
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
        let poly_elem_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("poly_pick_elem_layout"),
            bind_group_layouts: &[&poly_elem_bgl],
            push_constant_ranges: &[],
        });
        let make_elem_pipeline = |label: &str, topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&poly_elem_layout),
                vertex: wgpu::VertexState {
                    module: &poly_shader,
                    entry_point: Some("vs_poly_pick"),
                    buffers: &[PolyPickVertex::LAYOUT],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &poly_shader,
                    entry_point: Some("fs_poly_pick_elem"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Uint,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview: None,
                cache: None,
            })
        };
        let poly_edge_pipeline =
            make_elem_pipeline("poly_pick_edges", wgpu::PrimitiveTopology::LineList);
        let poly_vert_pipeline =
            make_elem_pipeline("poly_pick_verts", wgpu::PrimitiveTopology::PointList);

        let poly_vertex_capacity = 256usize;
        let poly_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("poly_pick_verts"),
            size: (poly_vertex_capacity * std::mem::size_of::<PolyPickVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bgl,
            camera_buffer,
            bind_group: None,
            color_texture: None,
            color_view: None,
            depth_texture: None,
            depth_view: None,
            size: (0, 0),
            staging_buffer,
            poly_pipeline,
            poly_edge_pipeline,
            poly_vert_pipeline,
            poly_elem_bgl,
            poly_elem_bind_group: None,
            poly_vertex_buffer,
            poly_vertex_capacity,
            full_staging: None,
            full_staging_size: (0, 0, 0),
        }
    }

    /// Upload poly face-ID vertices into the capacity-backed VB (P5). Returns vertex count.
    pub fn upload_poly_vertices(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[PolyPickVertex],
    ) -> u32 {
        let n = vertices.len();
        if n == 0 {
            return 0;
        }
        if n > self.poly_vertex_capacity {
            self.poly_vertex_capacity = n.next_power_of_two();
            self.poly_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("poly_pick_verts"),
                size: (self.poly_vertex_capacity * std::mem::size_of::<PolyPickVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(
            &self.poly_vertex_buffer,
            0,
            bytemuck::cast_slice(vertices),
        );
        n as u32
    }

    /// Encode the poly select-buffer render: PASS 1 = faces (+ id-0 occluder bodies), depth
    /// tested and depth writing; PASS 2 = edges then verts with NO depth attachment (the
    /// element shader SAMPLES the pass-1 depth — a texture cannot be attachment and sampled
    /// binding in the same pass, hence two).
    ///
    /// Ranges into the single uploaded VB: `[0, nf)` faces, `[nf, ne)` edge lines,
    /// `[ne, vcount)` vert points. Verts draw AFTER edges so a shared corner pixel carries the
    /// VERT id — a click on a corner means the vertex.
    ///
    /// ONE encode path shared by the app (`poly_select_gpu.rs`) and the headless regression
    /// tests, so a test proves the SHIPPING pass wiring rather than a copy that can drift.
    pub fn encode_poly_select(
        &self,
        enc: &mut wgpu::CommandEncoder,
        nf: u32,
        ne: u32,
        vcount: u32,
    ) {
        let (Some(cv), Some(dv)) = (self.color_view.as_ref(), self.depth_view.as_ref()) else {
            return;
        };
        {
            let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("poly_pick_faces"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if nf > 0 {
                if let Some(ref bg) = self.bind_group {
                    rpass.set_bind_group(0, bg, &[]);
                }
                rpass.set_vertex_buffer(0, self.poly_vertex_buffer.slice(..));
                rpass.set_pipeline(&self.poly_pipeline);
                rpass.draw(0..nf, 0..1);
            }
        }
        // Color LOADS (does not clear) so the pass-1 face ids survive underneath; elements
        // overwrite only where they pass the element visibility test.
        if vcount > nf {
            let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("poly_pick_elems"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(ref bg) = self.poly_elem_bind_group {
                rpass.set_bind_group(0, bg, &[]);
                rpass.set_vertex_buffer(0, self.poly_vertex_buffer.slice(..));
                if ne > nf {
                    rpass.set_pipeline(&self.poly_edge_pipeline);
                    rpass.draw(nf..ne, 0..1);
                }
                if vcount > ne {
                    rpass.set_pipeline(&self.poly_vert_pipeline);
                    rpass.draw(ne..vcount, 0..1);
                }
            }
        }
    }

    /// Padded bytes-per-row for a full-image copy of the current picking target.
    pub fn padded_bytes_per_row(width: u32) -> u32 {
        let bpr = width * 4;
        bpr.div_ceil(COPY_BYTES_PER_ROW_ALIGN) * COPY_BYTES_PER_ROW_ALIGN
    }

    /// Ensure the full-image staging buffer matches the current target size.
    /// MUST NOT be called while a full readback is in flight (single-flight is app-enforced).
    pub fn ensure_full_staging(&mut self, device: &wgpu::Device) {
        let (w, h) = self.size;
        let pbr = Self::padded_bytes_per_row(w);
        if self.full_staging.is_some() && self.full_staging_size == (w, h, pbr) { return; }
        self.full_staging = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("poly_pick_full_staging"),
            size: (pbr as u64) * (h as u64),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.full_staging_size = (w, h, pbr);
    }

    /// Copy the WHOLE picking texture to the full staging buffer (inside an encoder, pre-submit).
    pub fn copy_full(&self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(tex), Some(staging)) = (self.color_texture.as_ref(), self.full_staging.as_ref()) else { return };
        let (w, h, pbr) = self.full_staging_size;
        if w == 0 || h == 0 { return; }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(pbr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Start the async map of the full staging buffer (after submit).
    pub fn start_full_readback(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let map_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let Some(staging) = self.full_staging.as_ref() else { return map_ready };
        let flag = map_ready.clone();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            if result.is_ok() {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        map_ready
    }

    /// Decode the mapped full-staging rows into a w*h Vec<u32> of element ids and unmap.
    /// Caller must have confirmed the map completed.
    fn decode_full_staging(&self) -> Option<Vec<u32>> {
        let staging = self.full_staging.as_ref()?;
        let (w, h, pbr) = self.full_staging_size;
        let slice = staging.slice(..);
        let data = slice.get_mapped_range();
        let mut ids = vec![0u32; (w as usize) * (h as usize)];
        for y in 0..h as usize {
            let row = &data[y * pbr as usize..y * pbr as usize + (w as usize) * 4];
            for x in 0..w as usize {
                let b = &row[x * 4..x * 4 + 4];
                ids[y * w as usize + x] =
                    b[0] as u32 | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24);
            }
        }
        drop(data);
        staging.unmap();
        Some(ids)
    }

    /// Non-blocking: when the full readback is mapped, decode the padded rows into a w*h Vec<u32>
    /// of element ids (0 = background) and unmap. Returns None while pending.
    pub fn try_readback_full(
        &self,
        device: &wgpu::Device,
        map_ready: &std::sync::atomic::AtomicBool,
    ) -> Option<Vec<u32>> {
        device.poll(wgpu::PollType::Poll).expect("GPU polling failed");
        if !map_ready.load(std::sync::atomic::Ordering::SeqCst) { return None; }
        self.decode_full_staging()
    }

    /// BLOCKING full readback — waits for the queue + map to complete, then decodes.
    /// Used for the click-time synchronous select-buffer refresh (Blender renders its select
    /// buffer synchronously inside the pick operator; a stale buffer at click time was the
    /// "same click, different result" bug). Bounded wait: bails after the queue is repeatedly
    /// drained without the map callback firing (device loss) — caller falls back to async.
    pub fn blocking_readback_full(
        &self,
        device: &wgpu::Device,
        map_ready: &std::sync::atomic::AtomicBool,
    ) -> Option<Vec<u32>> {
        for _ in 0..64 {
            device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
            if map_ready.load(std::sync::atomic::Ordering::SeqCst) {
                return self.decode_full_staging();
            }
        }
        None
    }

    /// Ensure picking textures match the viewport size.
    pub fn ensure_textures(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.size == (width, height) && self.color_texture.is_some() {
            return;
        }
        self.size = (width, height);

        let color_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("picking_color"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.color_view = Some(color_tex.create_view(&Default::default()));
        self.color_texture = Some(color_tex);

        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("picking_depth"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // TEXTURE_BINDING: the poly element pass SAMPLES this depth (3×3 window max) instead
            // of racing it as an attachment — see the element pipeline comment.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.depth_view = Some(depth_tex.create_view(&Default::default()));
        self.depth_texture = Some(depth_tex);
        // The element bind group holds a view of the texture we just replaced — rebuild it here,
        // in the same call, so no consumer can reference the dead view (GPU Resource Ownership).
        self.rebuild_poly_elem_bind_group(device);
    }

    /// (Re)build the element-pass bind group: camera uniform + the face-pass depth view.
    /// Called on texture (re)creation and after `rebuild_bind_group`.
    pub fn rebuild_poly_elem_bind_group(&mut self, device: &wgpu::Device) {
        let Some(dv) = self.depth_view.as_ref() else {
            self.poly_elem_bind_group = None;
            return;
        };
        self.poly_elem_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("poly_pick_elem_bg"),
            layout: &self.poly_elem_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(dv),
                },
            ],
        }));
    }

    pub fn upload_camera(&self, queue: &wgpu::Queue, camera: &CameraUniforms) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(camera));
    }

    pub fn rebuild_bind_group(&mut self, device: &wgpu::Device) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("picking_bg"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.camera_buffer.as_entire_binding(),
            }],
        }));
    }

    /// Copy a single pixel from the picking texture to the staging buffer.
    /// Must be called inside a command encoder before submission.
    pub fn copy_pixel(&self, encoder: &mut wgpu::CommandEncoder, x: u32, y: u32) {
        let Some(ref tex) = self.color_texture else { return };
        let (w, h) = self.size;
        if x >= w || y >= h { return; }

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.staging_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(COPY_BYTES_PER_ROW_ALIGN),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// Start an async readback of the node ID from the staging buffer.
    /// Call after submitting the copy command. Check result with `try_readback_node_id`.
    /// Readback state: [`MAP_PENDING`] → [`MAP_OK`] or [`MAP_FAILED`].
    ///
    /// Three states, not a bool: the old bool was only ever set on success, so a FAILED map left
    /// it false forever. The caller then treated the pick as permanently in-flight, never cleared
    /// `pending_pick`, and — with the re-entry guard in `pick_node_at` — every later click would be
    /// skipped. Selection is P0; a dropped map must not be able to kill it for the session.
    pub const MAP_PENDING: u8 = 0;
    pub const MAP_OK: u8 = 1;
    pub const MAP_FAILED: u8 = 2;

    pub fn start_readback(&self) -> std::sync::Arc<std::sync::atomic::AtomicU8> {
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Self::MAP_PENDING));
        let flag = state.clone();
        self.staging_buffer.slice(..4).map_async(wgpu::MapMode::Read, move |result| {
            // Record BOTH outcomes — never discard the Err (a discarded map error is what
            // wedges the readback and, before the guard existed, preceded the still-mapped
            // submit panic).
            let v = if result.is_ok() { Self::MAP_OK } else { Self::MAP_FAILED };
            flag.store(v, std::sync::atomic::Ordering::SeqCst);
        });
        state
    }

    /// Try to read the pick result without blocking. Returns `Some(node_id)` if
    /// the GPU readback is complete, `None` if still pending.
    /// After returning `Some`, the staging buffer is unmapped and ready for reuse.
    pub fn try_readback_node_id(
        &self,
        device: &wgpu::Device,
        map_state: &std::sync::atomic::AtomicU8,
    ) -> Option<u64> {
        device.poll(wgpu::PollType::Poll).expect("GPU polling failed");
        match map_state.load(std::sync::atomic::Ordering::SeqCst) {
            Self::MAP_PENDING => return None,
            // Map failed: the buffer was never mapped, so there is nothing to unmap and nothing
            // to decode. Report a resolved "hit nothing" so the caller CLEARS its pending pick —
            // otherwise the slot stays occupied and every later pick is skipped by the guard.
            Self::MAP_FAILED => return Some(0),
            _ => {}
        }
        let slice = self.staging_buffer.slice(..4);
        let data = slice.get_mapped_range();
        let id = if data.len() < 4 {
            0
        } else {
            data[0] as u64
                | ((data[1] as u64) << 8)
                | ((data[2] as u64) << 16)
                | ((data[3] as u64) << 24)
        };
        drop(data);
        self.staging_buffer.unmap();
        Some(id)
    }

    /// BLOCKING pixel readback — waits until the map completes, decodes, **unmaps**.
    ///
    /// Used by `pick_node_at` so `picking_staging` is NEVER left mapped across later
    /// `queue.submit`s in the same frame (mesh upload, voxel regen, poly pick, …).
    /// Owner crash 2026-08-10 09:43 still hit "picking_staging is still mapped" with the
    /// abandon-only fix — any path that left the map open across a submit was fatal.
    /// Bounded: returns 0 (miss) if the callback never fires (device loss).
    pub fn blocking_readback_node_id(
        &self,
        device: &wgpu::Device,
        map_state: &std::sync::atomic::AtomicU8,
    ) -> u64 {
        for _ in 0..64 {
            device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
            if let Some(id) = self.try_readback_node_id(device, map_state) {
                return id;
            }
        }
        0
    }

    /// Abandon an in-flight pixel readback and leave `picking_staging` **unmapped**.
    ///
    /// Crash 2026-08-10 (`%TEMP%/fourthlayer_panic.txt`):
    /// `Queue::submit` panics with `Buffer with 'picking_staging' label is still mapped`
    /// when a previous pick was cancelled by clearing `pending_pick` without unmapping
    /// (tool-drop empty click, etc.). The next pick's `copy_pixel` + submit then hard-crashes
    /// the process (exit `0xcfffffff`). Always call this instead of dropping the pending slot.
    ///
    /// Waits for the map callback (OK or FAILED). On OK, unmaps without reading. If the
    /// callback never fires (device loss), recreates the staging buffer so future submits
    /// cannot see a zombie mapped buffer.
    pub fn abandon_pixel_readback(
        &mut self,
        device: &wgpu::Device,
        map_state: &std::sync::atomic::AtomicU8,
    ) {
        use std::sync::atomic::Ordering;
        for _ in 0..64 {
            device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
            match map_state.load(Ordering::SeqCst) {
                Self::MAP_PENDING => continue,
                Self::MAP_FAILED => return, // never mapped
                Self::MAP_OK => {
                    // Map completed — must unmap before any later submit may touch this buffer.
                    // Guard against double-abandon: if already unmapped, skip.
                    // (wgpu panics if get_mapped_range is called when not mapped.)
                    {
                        let _data = self.staging_buffer.slice(..4).get_mapped_range();
                    }
                    self.staging_buffer.unmap();
                    return;
                }
                _ => return,
            }
        }
        // Callback never fired (device loss / driver hang). Recreate staging so the old
        // buffer object (possibly still considered mapped) cannot poison the next submit.
        self.staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("picking_staging"),
            size: COPY_BYTES_PER_ROW_ALIGN as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_buffer_alignment() {
        assert!(COPY_BYTES_PER_ROW_ALIGN >= 4);
        assert_eq!(COPY_BYTES_PER_ROW_ALIGN % 256, 0);
    }

    /// The poly pick shader compiles only at pipeline creation — i.e. at app STARTUP, where a
    /// mistake is a panic, not a test failure. Parse + validate it here instead.
    #[test]
    fn poly_pick_wgsl_parses_and_validates() {
        let src = include_str!("poly_pick.wgsl");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("poly_pick.wgsl parse failed: {e:?}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("poly_pick.wgsl validation failed: {e:?}"));
    }

    /// Both fragment entry points the two passes bind must exist under their exact names.
    #[test]
    fn poly_pick_wgsl_has_both_fragment_entry_points() {
        let src = include_str!("poly_pick.wgsl");
        let module = naga::front::wgsl::parse_str(src).expect("parse");
        let names: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        for want in ["vs_poly_pick", "fs_poly_pick", "fs_poly_pick_elem"] {
            assert!(names.contains(&want), "missing entry point {want}: have {names:?}");
        }
    }
}
