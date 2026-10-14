use bytemuck::{Pod, Zeroable};

// ============================================================
// Depth-Tested Overlay Line Pipeline
// ============================================================
//
// Draws world-space line segments (edit wireframe / object wireframe overlay) into the
// composited colour texture, depth-tested against the SAME depth buffer the G-buffer
// raster pass wrote (`RasterPipeline.depth_texture`). Raster LINES over raster SOLID
// sharing ONE depth buffer inside one view — the Display Invariant explicitly permits
// this; nothing here fuses a raymarch with a raster over the same pixels.
//
// Replaces the CPU back-face-cull + per-segment ray-triangle hidden-line heuristics in
// the egui overlay, which could not hide a far island sitting behind a near one (edges
// bled through the surface) and cost O(edges x triangles) per frame.

/// One line segment. Uploaded as INSTANCE data — the vertex shader expands each into a
/// 2-triangle screen-space quad (see `line_overlay.wgsl` for why quads, not a line-list).
///
/// Positions are WORLD space: the vertex shader applies the raster camera's view_proj, so
/// a camera orbit needs no CPU rebuild at all (the old egui path re-projected every edge
/// every frame).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LineInstance {
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    /// Packed RGBA8 (read as Unorm8x4). Build with `LineInstance::pack_color`.
    pub color: u32,
    /// Line width in PIXELS (the user-facing 0.5–5.0 thickness sliders).
    pub width: f32,
}

impl LineInstance {
    /// Pack linear 0-1 RGBA into the Unorm8x4 the shader reads.
    pub fn pack_color(r: f32, g: f32, b: f32, a: f32) -> u32 {
        let q = |v: f32| -> u32 { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32 };
        q(r) | (q(g) << 8) | (q(b) << 16) | (q(a) << 24)
    }

    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<LineInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 24, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 28, shader_location: 3 },
        ],
    };
}

/// `LineParams` uniform — xy = viewport pixel size, z = wire AA enable (1/0), w = padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct LineParams {
    viewport: [f32; 4],
}

/// Colour target format — the composited LDR texture the line pass draws over.
pub const LINE_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub struct LineOverlayPipeline {
    /// Depth-aware: fragment shader compares against the depth TEXTURE with a
    /// slope-aware epsilon (hidden-line removal; see line_overlay.wgsl header).
    pub pipeline: wgpu::RenderPipeline,
    /// X-Ray: no depth compare — every edge draws through the surface. Same shader,
    /// same buffer, same draw code; only the fragment entry differs.
    pub pipeline_xray: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    /// Built lazily in `viewport_render` from the RasterPipeline's camera buffer (created
    /// once, never recreated), our params buffer (fixed) and the raster DEPTH view — the
    /// depth texture IS recreated on resize, so callers compare `bind_group_depth_size`
    /// against the current depth size and rebuild on mismatch.
    pub bind_group: Option<wgpu::BindGroup>,
    /// `RasterPipeline.depth_generation` captured when `bind_group` was built —
    /// u64::MAX = never built. A same-size depth recreate (viewport-mode switch) changes
    /// generation but not size, so this MUST be a generation, not a size.
    pub bind_group_depth_gen: u64,
    params_buffer: wgpu::Buffer,
    /// Pooled instance buffer — grows/shrinks like `RasterPipeline.instance_buffer`
    /// rather than allocating per frame.
    pub instance_buffer: wgpu::Buffer,
    pub instance_capacity: usize,
    instance_shrink_counter: u32,
}

impl LineOverlayPipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("line_overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("line_overlay.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("line_overlay_bgl"),
            entries: &[
                // binding 0: camera uniforms (SHARED with the raster G-buffer pass)
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
                // binding 1: line params (viewport size)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 2: raster depth buffer as a TEXTURE — occlusion is resolved in
                // the fragment shader with a slope-aware tolerance (see line_overlay.wgsl
                // header for why hardware depth test + bias/pull variants all failed).
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
            label: Some("line_overlay_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        // Overlay lines carry an opacity (poly_wire_opacity / wire_overlay_opacity), so the
        // target blends alpha-over the already-tonemapped colour.
        let targets = [Some(wgpu::ColorTargetState {
            format: LINE_TARGET_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        // NO depth attachment: occlusion is decided in the fragment shader against the
        // depth TEXTURE (binding 2) with a slope-aware epsilon. The overlay never writes
        // depth either, so it cannot occlude the geometry it annotates.
        let make = |label: &str, fs_entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[LineInstance::LAYOUT],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // The expanded quad's winding depends on the segment's screen direction.
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

        let pipeline = make("line_overlay_depth", "fs_main");
        let pipeline_xray = make("line_overlay_xray", "fs_xray");

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line_overlay_params"),
            size: std::mem::size_of::<LineParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let initial_cap = 1024;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line_overlay_instances"),
            size: (initial_cap * std::mem::size_of::<LineInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            pipeline_xray,
            bind_group_layout: bgl,
            bind_group: None,
            bind_group_depth_gen: u64::MAX,
            params_buffer,
            instance_buffer,
            instance_capacity: initial_cap,
            instance_shrink_counter: 0,
        }
    }

    /// Upload the viewport size the vertex shader expands pixel widths against, plus the
    /// wire anti-aliasing switch (`render.wire_aa_enabled` — analytic coverage ramp in
    /// line_overlay.wgsl; off = legacy hard-edged quads for low-end GPUs).
    /// `dpi_scale` converts authored POINT widths into target texels (1.0 when the
    /// target is sized in points). The AA feather is deliberately NOT scaled — it is
    /// a one-target-texel coverage ramp, which is correct at any resolution.
    pub fn upload_params(&self, queue: &wgpu::Queue, width: u32, height: u32, wire_aa: bool, dpi_scale: f32) {
        let aa = if wire_aa { 1.0 } else { 0.0 };
        let p = LineParams { viewport: [width as f32, height as f32, aa, dpi_scale.max(0.0001)] };
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&p));
    }

    /// Upload segments into the pooled buffer, growing/shrinking with the same
    /// grow-immediately / throttled-shrink discipline as `RasterPipeline::upload_instances`.
    /// The instance buffer is a vertex-slot resource bound by slice at draw time, never
    /// captured by a cached bind group, so a recreate invalidates nothing.
    pub fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[LineInstance],
    ) {
        const MIN_CAPACITY: usize = 1024;
        let needed = instances.len().max(1);

        let mut recreate = false;
        if needed > self.instance_capacity {
            self.instance_capacity = needed.next_power_of_two();
            recreate = true;
        }
        self.instance_shrink_counter += 1;
        if self.instance_shrink_counter >= 60 {
            self.instance_shrink_counter = 0;
            if needed < self.instance_capacity / 4 && self.instance_capacity > MIN_CAPACITY {
                self.instance_capacity = needed.next_power_of_two().max(MIN_CAPACITY);
                recreate = true;
            }
        }
        if recreate {
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("line_overlay_instances"),
                size: (self.instance_capacity * std::mem::size_of::<LineInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    /// Build the bind group. `camera_buffer` is `RasterPipeline.camera_buffer` — the SAME
    /// buffer the G-buffer pass uses, so line and mesh depth share one view_proj.
    /// `depth_view` is the raster depth buffer bound as a texture; `depth_gen` is
    /// `RasterPipeline.depth_generation` at build time — callers compare it against the
    /// current generation and rebuild on mismatch (CLAUDE.md GPU Resource Lifetime).
    pub fn rebuild_bind_group(
        &mut self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
        depth_view: &wgpu::TextureView,
        depth_gen: u64,
    ) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("line_overlay_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: camera_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(depth_view) },
            ],
        }));
        self.bind_group_depth_gen = depth_gen;
    }

    /// Draw `count` segments starting at `first` — 6 vertices per instance (the expanded quad).
    /// `xray` selects the depth-compare-Always pipeline.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, first: u32, count: u32, xray: bool) {
        if count == 0 {
            return;
        }
        let Some(ref bg) = self.bind_group else { return };
        pass.set_pipeline(if xray { &self.pipeline_xray } else { &self.pipeline });
        pass.set_bind_group(0, bg, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, first..(first + count));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_instance_size_matches_layout() {
        // 3*4 + 3*4 + 4 + 4 = 32. The LAYOUT offsets below hardcode this packing.
        assert_eq!(std::mem::size_of::<LineInstance>(), 32);
        assert_eq!(LineInstance::LAYOUT.array_stride, 32);
    }

    /// `create_shader_module` parses WGSL at RUNTIME, so a typo in line_overlay.wgsl would
    /// only surface as a GPU-time panic. Validate it here instead (mirrors the graphics
    /// crate's Naga parse tests).
    #[test]
    fn line_overlay_wgsl_parses_and_validates() {
        let src = include_str!("line_overlay.wgsl");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("line_overlay.wgsl parse failed: {e:?}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("line_overlay.wgsl validation failed: {e:?}"));
    }

    // ---- Headless GPU test: the actual claim of this feature ----
    //
    // "A line BEHIND a nearer surface is hidden by the hardware depth test; X-Ray opts out."
    // That is the whole point of the pass (the CPU back-face-cull heuristic it replaced could
    // not do this), so it is worth proving on a real device rather than by inspection.
    //
    // Method: the depth buffer is CLEARED to a known depth standing in for "a nearer surface
    // at that depth" — an occluder drawn by the raster pass and a cleared depth value are
    // indistinguishable to this pipeline, which only ever reads depth. An ORTHOGRAPHIC camera
    // makes the expected NDC depth exactly computable.

    /// Headless device. Returns None when no adapter exists (CI without a GPU) so the test
    /// skips rather than fails — same policy as graphics/src/voxel_brush_gpu_parity.rs.
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
                label: Some("line_overlay_test_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        ))
        .ok()
    }

    const TEX: u32 = 64;

    /// Render one horizontal line at world-space `z` into a fresh target whose depth is
    /// pre-cleared to `depth_clear`. Returns pixel `px`'s raw (R, G) f16 bits (the target
    /// is cleared TRANSPARENT and the pass alpha-blends, so R directly equals red×coverage).
    /// f16 bits are compared directly (0x0000 = 0.0, 0x3C00 = 1.0) — no float decode needed.
    fn render_line_px(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        z: f32,
        color: u32,
        depth_clear: f32,
        xray: bool,
        wire_aa: bool,
        px: (u32, u32),
    ) -> (u16, u16) {
        let mut lo = LineOverlayPipeline::new(device);

        // Orthographic so depth is linear and hand-checkable. Camera sits at +Z looking -Z.
        let cam = crate::camera::CameraUniforms::from_orbit(
            glam::Quat::IDENTITY,
            5.0,
            glam::Vec3::ZERO,
            1.0,
            45.0,
            0.1,
            100.0,
            true,
        );
        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line_test_cam"),
            size: std::mem::size_of::<crate::camera::CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&cam_buf, 0, bytemuck::bytes_of(&cam));
        lo.upload_params(queue, TEX, TEX, wire_aa, 1.0);

        // Horizontal segment through the origin, thick enough to cover the centre pixel.
        lo.upload_instances(
            device,
            queue,
            &[LineInstance { p0: [-2.0, 0.0, z], p1: [2.0, 0.0, z], color, width: 5.0 }],
        );

        let color_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("line_test_color"),
            size: wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LINE_TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("line_test_depth"),
            size: wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // TEXTURE_BINDING: the overlay samples depth in the fragment shader now.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let cv = color_tex.create_view(&Default::default());
        let dv = depth_tex.create_view(&Default::default());
        lo.rebuild_bind_group(device, &cam_buf, &dv, 0);

        // 64px * 8B = 512, already a multiple of the 256B copy alignment.
        let bpr = TEX * 8;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line_test_staging"),
            size: (bpr * TEX) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc = device.create_command_encoder(&Default::default());
        {
            // Pre-pass: clear the depth texture to the stand-in "nearer surface" depth.
            // (The overlay pass itself has NO depth attachment — it samples the texture.)
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("line_test_depth_clear"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth_clear),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("line_test_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            lo.draw(&mut pass, 0, 1, xray);
        }
        enc.copy_texture_to_buffer(
            color_tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(TEX),
                },
            },
            wgpu::Extent3d { width: TEX, height: TEX, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::Wait).expect("GPU polling failed");
        rx.recv().expect("map_async channel").expect("map_async failed");

        let data = slice.get_mapped_range();
        let texels: &[u16] = bytemuck::cast_slice(&data);
        // Row stride is TEX pixels * 4 channels; px = (column, row).
        let i = (px.1 as usize) * (TEX as usize * 4) + (px.0 as usize) * 4;
        (texels[i], texels[i + 1])
    }

    const F16_ONE: u16 = 0x3C00;
    const F16_ZERO: u16 = 0x0000;
    const CENTRE: (u32, u32) = (TEX / 2, TEX / 2);

    /// The regression this whole feature exists to fix: an edge BEHIND a nearer surface must
    /// not draw through it. Depth clear 0.07 stands in for a surface between the two lines
    /// (ortho depth: world z=0 -> ~0.049, world z=-5 -> ~0.099).
    #[test]
    fn line_behind_surface_is_occluded_and_in_front_is_drawn() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping line_overlay depth test");
            return;
        };
        let red = LineInstance::pack_color(1.0, 0.0, 0.0, 1.0);
        let green = LineInstance::pack_color(0.0, 1.0, 0.0, 1.0);

        // In FRONT of the cleared depth (0.049 <= 0.07) -> passes LessEqual -> red drawn.
        let (r, g) = render_line_px(&device, &queue, 0.0, red, 0.07, false, true, CENTRE);
        assert_eq!((r, g), (F16_ONE, F16_ZERO), "line in front of the surface must be drawn");

        // BEHIND the cleared depth (0.099 > 0.07) -> fails LessEqual -> nothing drawn.
        // This is exactly what the old back-face-cull heuristic could not do.
        let (r, g) = render_line_px(&device, &queue, -5.0, green, 0.07, false, true, CENTRE);
        assert_eq!((r, g), (F16_ZERO, F16_ZERO), "line behind the surface must be occluded");
    }

    /// X-Ray is the opt-in escape hatch: the same behind-the-surface line draws through.
    #[test]
    fn xray_draws_line_through_surface() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping line_overlay xray test");
            return;
        };
        let green = LineInstance::pack_color(0.0, 1.0, 0.0, 1.0);
        let (r, g) = render_line_px(&device, &queue, -5.0, green, 0.07, true, true, CENTRE);
        assert_eq!((r, g), (F16_ZERO, F16_ONE), "X-Ray must draw the occluded line through");
    }

    /// Regression: coplanar crease edges (internal edge shared by two faces on one plane)
    /// must draw when the surface depth EQUALS the wire depth (the z-fight case the old
    /// hardware LessEqual + bias lost). The slope-aware fragment test passes on equality
    /// (flat surface → gradient 0 → eps = base). Also assert the counterpart that the old
    /// world-space pull got WRONG: a surface a solid margin closer (0.048 vs ~0.049, ~1e-3
    /// = far beyond the flat-surface eps) must OCCLUDE — over-tolerance here was the
    /// 2026-07-23 silhouette-leak bug (wire spurs poking past object contours).
    #[test]
    fn coplanar_equal_depth_draws_and_nearer_solid_occludes() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping line_overlay coplanar test");
            return;
        };
        let red = LineInstance::pack_color(1.0, 0.0, 0.0, 1.0);
        // Exact depth of world z=0 under the test camera, computed from the same matrix the
        // shader uses — clearing to it reproduces a perfectly coplanar solid surface.
        let cam = crate::camera::CameraUniforms::from_orbit(
            glam::Quat::IDENTITY, 5.0, glam::Vec3::ZERO, 1.0, 45.0, 0.1, 100.0, true,
        );
        let vp = glam::Mat4::from_cols_array_2d(&cam.view_proj);
        let clip = vp * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        let plane_depth = clip.z / clip.w;
        let (r, g) = render_line_px(&device, &queue, 0.0, red, plane_depth, false, true, CENTRE);
        assert_eq!(
            (r, g),
            (F16_ONE, F16_ZERO),
            "coplanar wire must draw at exactly-equal surface depth (depth {plane_depth})"
        );
        // A genuinely nearer flat surface must occlude (no over-tolerance / leak).
        let (r, g) =
            render_line_px(&device, &queue, 0.0, red, plane_depth - 1e-3, false, true, CENTRE);
        assert_eq!(
            (r, g),
            (F16_ZERO, F16_ZERO),
            "a solidly nearer surface must occlude the wire (no silhouette leak)"
        );
    }

    /// The wire AA feature itself. The 5px line's centerline lands on framebuffer y=32.0,
    /// so row 29 (pixel centre y=29.5) sits exactly on the true edge (d = 2.5 = half-width)
    /// → analytic coverage 0.5. Depth is cleared far (1.0) so occlusion never interferes.
    #[test]
    fn wire_aa_edge_row_is_partial_and_off_is_binary() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping line_overlay AA test");
            return;
        };
        let red = LineInstance::pack_color(1.0, 0.0, 0.0, 1.0);

        // AA on: the edge row is a partial-coverage ramp, ~0.5 (positive f16 bit patterns
        // order numerically: 0x3400 = 0.25, 0x3A00 = 0.75).
        let (r, _) = render_line_px(&device, &queue, 0.0, red, 1.0, false, true, (32, 29));
        assert!(
            (0x3400..=0x3A00).contains(&r),
            "edge row must be ~half coverage with AA on, got {r:#06x}"
        );
        // The feather ring must not bleed: two rows further out stays empty.
        let (r, _) = render_line_px(&device, &queue, 0.0, red, 1.0, false, true, (32, 27));
        assert_eq!(r, F16_ZERO, "outside the AA ramp must stay empty");
        // The line interior stays fully solid with AA on.
        let (r, _) = render_line_px(&device, &queue, 0.0, red, 1.0, false, true, CENTRE);
        assert_eq!(r, F16_ONE, "line interior must stay solid with AA on");

        // AA off: the exact legacy binary quad (spans y 29.5..34.5) — row 30 solid,
        // row 28 empty, nothing widened. (Row 29's centre lies exactly on the quad edge —
        // rasterizer boundary rules own that pixel, so it is deliberately not asserted.)
        let (r, _) = render_line_px(&device, &queue, 0.0, red, 1.0, false, false, (32, 30));
        assert_eq!(r, F16_ONE, "AA off must keep the legacy solid quad");
        let (r, _) = render_line_px(&device, &queue, 0.0, red, 1.0, false, false, (32, 28));
        assert_eq!(r, F16_ZERO, "AA off must not expand the quad");
    }

    #[test]
    fn pack_color_roundtrip() {
        // Opaque red → r=255 in byte 0, a=255 in byte 3.
        assert_eq!(LineInstance::pack_color(1.0, 0.0, 0.0, 1.0), 0xFF0000FF);
        // Opaque blue → b=255 in byte 2.
        assert_eq!(LineInstance::pack_color(0.0, 0.0, 1.0, 1.0), 0xFFFF0000);
        // Clamps out-of-range instead of wrapping.
        assert_eq!(LineInstance::pack_color(2.0, -1.0, 0.0, 1.0), 0xFF0000FF);
    }
}
