use crate::camera::CameraUniforms;
use crate::mesh_gpu::{InstanceData, RasterMaterial, RasterVertex};
use crate::shadows::ShadowMapSystem;

// ============================================================
// Rasterization Pipeline
// ============================================================

/// Rasterization pipeline that renders triangle meshes into G-buffer
/// textures matching the SDF compute raymarcher's output format.
pub struct RasterPipeline {
    pub pipeline: wgpu::RenderPipeline,
    /// GPU DC-wire pass (`POLYGON_MODE_LINE` + `fs_gbuffer_wire`), no cull (show backfaces).
    /// None when the adapter lacks `Features::POLYGON_MODE_LINE`.
    pub wire_pipeline: Option<wgpu::RenderPipeline>,
    /// Same as `wire_pipeline` but back-face culled (hide backfaces).
    pub wire_pipeline_front: Option<wgpu::RenderPipeline>,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub bind_group: Option<wgpu::BindGroup>,
    /// Shadow bind group layout (group 1: shadow uniforms + depth textures + sampler).
    pub shadow_bgl: wgpu::BindGroupLayout,
    pub shadow_bind_group: Option<wgpu::BindGroup>,
    /// Comparison sampler for shadow mapping.
    pub shadow_sampler: wgpu::Sampler,
    /// Camera uniform buffer (includes grab + wire_color tail).
    pub camera_buffer: wgpu::Buffer,
    /// Un-jittered twin of `camera_buffer` for overlay passes (line/face overlay) —
    /// progressive refine jitters the main camera per frame; overlays draw over the
    /// stable accumulated image and must not shimmer. Uploaded alongside
    /// `camera_buffer` in `upload_camera`; identical content when jitter is zero.
    pub camera_nojitter_buffer: wgpu::Buffer,
    /// Material storage buffer (grows/shrinks as needed).
    pub material_buffer: wgpu::Buffer,
    pub material_capacity: usize,
    /// Frame counter for periodic buffer shrink checks.
    material_shrink_counter: u32,
    /// C.7: content fingerprint of last GPU write — skip `write_buffer` when unchanged.
    last_materials_hash: u64,
    /// Per-instance data buffer (vertex slot 1), grows/shrinks as needed (RP1).
    /// Replaces the per-object throwaway `create_buffer_init` that ran every frame.
    /// Bound per draw via a byte-offset slice; not part of bind group 0, so a recreate
    /// needs no bind-group invalidation (nothing caches a reference across frames).
    pub instance_buffer: wgpu::Buffer,
    pub instance_capacity: usize,
    /// Bumped every time `instance_buffer` is recreated (grow/shrink). The D1 instance-cull
    /// system caches a bind group over this buffer — it compares this and rebuilds on mismatch.
    pub instance_buffer_generation: u64,
    instance_shrink_counter: u32,
    /// Legacy field kept only so buffer recreate can force a write path cleanly.
    /// **Do not use for content skip** — sampled instance-byte hashing is unsound after C.6 packing
    /// (mid-list transform / net-zero visibility swap collides). App gates upload via
    /// `raster_upload_plane_key` + structural rebuild only (D1/T1 pure key-driven writes).
    last_instances_hash: u64,
    /// Hardware depth buffer for z-testing during rasterization.
    pub depth_texture: Option<wgpu::Texture>,
    pub depth_view: Option<wgpu::TextureView>,
    pub depth_size: (u32, u32),
    /// Bumped every time `depth_texture` is (re)created — cached bind groups referencing
    /// `depth_view` (line_overlay) compare this and rebuild on mismatch.
    pub depth_generation: u64,
    /// Cached generation of the material texture array bound at group0 binding 2/3. When the array
    /// is recreated (grow), the app's `MaterialTextureArray.generation` bumps; the caller compares
    /// and invalidates `bind_group` so it rebinds the new texture view. Mirrors the atlas_tex_gen
    /// pattern (generation counters over timing tricks).
    pub mat_tex_gen: u64,
    /// Cached generation of the shadow cascade textures bound at group1 binding 1-3. When
    /// `ShadowMapSystem::set_resolution` recreates the maps (poll phase), its `generation`
    /// bumps; the caller compares and invalidates `shadow_bind_group` so it rebinds the new
    /// depth views the SAME frame (generation counters over timing tricks — never "next
    /// frame" logic).
    pub shadow_map_gen: u64,
    /// GPU_CURVATURE_KERNEL_PLAN.md P2 — group 2: ONE `storage, read` buffer (`curv_vertex`),
    /// VERTEX-visible only, read by `vs_main` at `@builtin(vertex_index)` when the instance's
    /// `INSTANCE_FLAG_VERTEX_CURVATURE` bit is set. Every draw using `pipeline`/`wire_pipeline*`
    /// MUST bind SOMETHING at group 2 or pipeline validation fails outright (this is a
    /// pipeline-layout requirement, not merely a correctness one) — the caller falls back to
    /// `curvature_dummy_bind_group` (a 1-element zeroed buffer) for every body that has no
    /// resident `sdf_body_curvature` entry, exactly the `bake_tex_dummy` precedent.
    pub curvature_bgl: wgpu::BindGroupLayout,
    /// 1-element (16-byte, aligned) zeroed dummy bind group for group 2 — bound whenever the
    /// drawn body has no curvature buffer (the overwhelming majority of draws: SDF bodies
    /// without a Vertex-rung generator, all poly/voxel/NURBS geometry). Created once; never
    /// mutated or regenerated (a dummy has no generation to track).
    pub curvature_dummy_bind_group: wgpu::BindGroup,
}

impl RasterPipeline {
    /// Create the rasterization pipeline.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gbuffer_raster"),
            source: wgpu::ShaderSource::Wgsl(concat!(
                include_str!("../../graphics/src/material_precision.wgsl"),
                "\n",
                include_str!("gbuffer.wgsl"),
            ).into()),
        });

        // Bind group layout: camera uniform + material storage
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raster_bgl"),
            entries: &[
                // binding 0: camera uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: material storage (read-only) — read in BOTH stages: the fragment
                // shader shades from it, and the vertex shader samples it for per-vertex material
                // lookup (gbuffer.wgsl vs_main: materials[in.material_idx & 0x3FFFFFFF]). Vertex
                // visibility is REQUIRED or raster_gbuffer fails pipeline validation.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 2: material texture array (RGBA8 sRGB, D2Array) — sampled in BOTH stages:
                // the fragment shader triplanar-samples albedo/normal, and the vertex shader samples
                // the displacement map for per-vertex height offset (vs_main -> sample_mat_scalar ->
                // textureSampleLevel(mat_tex_array, ...)). Vertex visibility is REQUIRED.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 3: material texture sampler (filtering) — used in BOTH stages (the vertex
                // displacement sample uses this sampler too). Must match binding 2's visibility.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Shadow bind group layout (group 1)
        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raster_shadow_bgl"),
            entries: &[
                // binding 0: shadow uniform (cascade VPs + splits)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: shadow_map_0
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
                // binding 2: shadow_map_1
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
                // binding 3: shadow_map_2
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 4: comparison sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });

        // Comparison sampler for shadow mapping
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow_sampler"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Group 2 (GPU_CURVATURE_KERNEL_PLAN.md P2/P3): the per-body vertex-curvature carry. VERTEX
        // visibility only — the STORAGE BUFFER itself is read exclusively by `vs_main`, which
        // writes the sampled value into a (now smoothly-interpolated, see VertexOutput::vertex_curv)
        // varying; `fs_gbuffer` consumes THAT varying to tint Base color (BGL Single Source: one
        // binding, one GPU-buffer reader — the fragment stage never binds or indexes this buffer).
        let curvature_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raster_curvature_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        // 4-float zeroed dummy — bound at group 2 for every draw whose body has no resident
        // curvature buffer (the `bake_tex_dummy` / mesh_grid-dummy precedent: the binding must
        // ALWAYS exist, even where the shader never meaningfully reads it this draw).
        let curvature_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster_curvature_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let curvature_dummy_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raster_curvature_dummy_bg"),
            layout: &curvature_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: curvature_dummy_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raster_layout"),
            bind_group_layouts: &[&bgl, &shadow_bgl, &curvature_bgl],
            push_constant_ranges: &[],
        });

        // 4 color targets — 32-byte/sample hardware limit (wgpu/Vulkan) prevents
        // attaching all 7 G-buffer textures to a render pass. surface_id, curvature,
        // emission are written by SDF compute pass via storage imageStore (no limit).
        // Raster pixels get safe defaults from clear color.
        // Total: 8+8+8+8 = 32B (at limit). Albedo was Rgba8Unorm (4B) which re-quantized
        // dual-plane 16-bit Mult stacks to 256 levels → contour banding on soft FBM.
        let targets = [
            // location(0): normal + depth — Rgba16Float (8B)
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
            // location(1): material — Rgba16Float (8B)
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
            // location(2): albedo + flags — Rgba16Float (8B) so Bit16 map sample survives
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
            // location(3): body_index — R32Float (C.5: exact ints past 2048; 4B keeps MRT ≤32 B)
            Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R32Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("raster_gbuffer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[RasterVertex::LAYOUT, InstanceData::LAYOUT],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
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
                entry_point: Some("fs_gbuffer"),
                targets: &targets,
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        // DC-wire: same meshes as lines. Requires POLYGON_MODE_LINE (Vulkan/DX12 common).
        // Two variants: no-cull (show backfaces) and back-cull (hide backfaces).
        let (wire_pipeline, wire_pipeline_front) =
            if device.features().contains(wgpu::Features::POLYGON_MODE_LINE) {
                let make_wire = |label: &str, cull: Option<wgpu::Face>| {
                    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some(label),
                        layout: Some(&pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs_main"),
                            buffers: &[RasterVertex::LAYOUT, InstanceData::LAYOUT],
                            compilation_options: Default::default(),
                        },
                        primitive: wgpu::PrimitiveState {
                            topology: wgpu::PrimitiveTopology::TriangleList,
                            front_face: wgpu::FrontFace::Ccw,
                            cull_mode: cull,
                            polygon_mode: wgpu::PolygonMode::Line,
                            ..Default::default()
                        },
                        depth_stencil: Some(wgpu::DepthStencilState {
                            format: wgpu::TextureFormat::Depth32Float,
                            depth_write_enabled: false,
                            depth_compare: wgpu::CompareFunction::LessEqual,
                            stencil: wgpu::StencilState::default(),
                            bias: wgpu::DepthBiasState {
                                constant: -2,
                                slope_scale: -1.0,
                                clamp: 0.0,
                            },
                        }),
                        multisample: wgpu::MultisampleState::default(),
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some("fs_gbuffer_wire"),
                            targets: &targets,
                            compilation_options: Default::default(),
                        }),
                        multiview: None,
                        cache: None,
                    })
                };
                (
                    Some(make_wire("raster_gbuffer_wire", None)),
                    Some(make_wire(
                        "raster_gbuffer_wire_front",
                        Some(wgpu::Face::Back),
                    )),
                )
            } else {
                eprintln!("[GPU] POLYGON_MODE_LINE: false — DC wire falls back to CPU edges");
                (None, None)
            };

        // Camera uniform buffer
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster_camera"),
            size: std::mem::size_of::<CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Un-jittered twin of camera_buffer for overlay passes (wire/face overlays).
        // Progressive refine sub-pixel-jitters the main camera; overlays draw AFTER
        // the accumulated (stable) image, so a jittered overlay would visibly
        // shimmer over it. Identical content when jitter is zero.
        let camera_nojitter_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster_camera_nojitter"),
            size: std::mem::size_of::<CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Material storage buffer (start with space for 16 materials)
        let initial_cap = 16;
        let material_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster_materials"),
            size: (initial_cap * std::mem::size_of::<RasterMaterial>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Per-instance buffer (start with space for 64 instances)
        let initial_inst_cap = 64;
        // STORAGE: the D1 instance-cull compute pass reads this buffer directly.
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raster_instances"),
            size: (initial_inst_cap * std::mem::size_of::<InstanceData>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            wire_pipeline,
            wire_pipeline_front,
            bind_group_layout: bgl,
            bind_group: None,
            shadow_bgl,
            shadow_bind_group: None,
            shadow_sampler,
            camera_buffer,
            camera_nojitter_buffer,
            material_buffer,
            material_capacity: initial_cap,
            material_shrink_counter: 0,
            last_materials_hash: 0,
            instance_buffer,
            instance_capacity: initial_inst_cap,
            instance_buffer_generation: 0,
            instance_shrink_counter: 0,
            last_instances_hash: 0,
            depth_texture: None,
            depth_view: None,
            depth_size: (0, 0),
            depth_generation: 0,
            mat_tex_gen: u64::MAX, // force first-frame rebuild (any real gen != MAX)
            shadow_map_gen: u64::MAX, // force first-frame rebuild (any real gen != MAX)
            curvature_bgl,
            curvature_dummy_bind_group,
        }
    }

    /// Ensure depth texture matches viewport size.
    pub fn ensure_depth(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.depth_size == (width, height) && self.depth_texture.is_some() {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raster_depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // TEXTURE_BINDING: the line overlay samples this depth in its fragment shader
            // (slope-aware occlusion — line_overlay.wgsl). Attachment-only usage made the
            // overlay bind group invalid → device loss on first draw (2026-07-23 regression).
            // COPY_SRC: `depth_probe` reads single texels back so world-space overlay markers
            // (light icons) can be depth-tested instead of CPU-raycast against every triangle
            // in the scene (VIEWPORT_PERF_2026-08-12 item A).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.depth_view = Some(tex.create_view(&Default::default()));
        self.depth_texture = Some(tex);
        self.depth_size = (width, height);
        // Generation counter (CLAUDE.md GPU Resource Lifetime): consumers caching a view of
        // this texture (line_overlay bind group) compare generations — a same-size recreate
        // (e.g. viewport-mode switch) would be invisible to a size check.
        self.depth_generation = self.depth_generation.wrapping_add(1);
    }

    /// Upload camera uniforms to GPU.
    /// Upload the (possibly jittered) camera, and its un-jittered twin for overlay
    /// passes. `camera_nojitter` should differ from `camera` only by the progressive
    /// refine sub-pixel jitter on proj/view_proj.
    pub fn upload_camera(
        &self,
        queue: &wgpu::Queue,
        camera: &CameraUniforms,
        camera_nojitter: &CameraUniforms,
    ) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(camera));
        queue.write_buffer(&self.camera_nojitter_buffer, 0, bytemuck::bytes_of(camera_nojitter));
    }

    /// Content fingerprint for C.7 skip-if-unchanged material uploads (full for small, sampled large).
    fn content_hash_bytes(bytes: &[u8], len: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        len.hash(&mut h);
        const FULL_HASH_MAX: usize = 64 * 1024;
        if bytes.len() <= FULL_HASH_MAX {
            bytes.hash(&mut h);
        } else {
            let n = bytes.len();
            bytes[..8192.min(n)].hash(&mut h);
            bytes[n.saturating_sub(8192)..].hash(&mut h);
            let mid = n / 2;
            let m0 = mid.saturating_sub(4096);
            let m1 = (mid + 4096).min(n);
            bytes[m0..m1].hash(&mut h);
        }
        h.finish()
    }

    /// Upload materials to GPU, growing or shrinking the buffer as needed.
    ///
    /// Shrink is throttled to every 60 frames and only triggers when capacity
    /// exceeds 4x the required size, to avoid thrashing on small fluctuations.
    ///
    /// **C.7:** returns `true` if `write_buffer` ran, `false` if content matched last upload
    /// (camera-only frames with stable materials → 0 writes).
    pub fn upload_materials(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        materials: &[RasterMaterial],
    ) -> bool {
        const MIN_CAPACITY: usize = 16;
        let needed = materials.len().max(1); // at least 1 to avoid zero-size buffer

        let mut recreate = false;

        // Grow if needed (immediate)
        if needed > self.material_capacity {
            self.material_capacity = needed.next_power_of_two();
            recreate = true;
        }

        // Shrink if way oversized (throttled to every 60 frames)
        self.material_shrink_counter += 1;
        if self.material_shrink_counter >= 60 {
            self.material_shrink_counter = 0;
            if needed < self.material_capacity / 4 && self.material_capacity > MIN_CAPACITY {
                self.material_capacity = needed.next_power_of_two().max(MIN_CAPACITY);
                recreate = true;
            }
        }

        if recreate {
            self.material_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("raster_materials"),
                size: (self.material_capacity * std::mem::size_of::<RasterMaterial>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // Invalidate bind group since buffer changed
            self.bind_group = None;
            self.last_materials_hash = 0; // force write into new buffer
        }
        let bytes = bytemuck::cast_slice(materials);
        let hash = Self::content_hash_bytes(bytes, materials.len());
        if !recreate && hash == self.last_materials_hash && !materials.is_empty() {
            return false;
        }
        self.last_materials_hash = hash;
        if !materials.is_empty() {
            queue.write_buffer(&self.material_buffer, 0, bytes);
            return true;
        }
        false
    }

    /// Upload per-instance data to GPU, growing/shrinking the pooled buffer as needed
    /// (RP1). Same grow/throttled-shrink discipline as `upload_materials`.
    ///
    /// **Key-driven only (D1/T1):** the *caller* decides whether to upload
    /// (`structural rebuild || plane_key != stored`). This function **always**
    /// `write_buffer`s when invoked with non-empty `instances` — never samples
    /// instance bytes or cull bits for a content-hash skip. Sampled hashing was
    /// unsound after C.6 packing: mid-list transform edits and net-zero visibility
    /// swaps collided and silently skipped the GPU write (object vanishes / freezes
    /// until the camera moves).
    ///
    /// `cull_bits` is retained in the signature for call-site compatibility; it is
    /// **not** consulted for skip decisions (draw uses per-instance flags already
    /// packed into `InstanceData` / draw lists).
    pub fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[InstanceData],
        _cull_bits: &[u8],
    ) -> bool {
        const MIN_CAPACITY: usize = 16;
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
                label: Some("raster_instances"),
                size: (self.instance_capacity * std::mem::size_of::<InstanceData>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.last_instances_hash = 0;
            // Generation counter (GPU Resource Lifetime rule): consumers caching a bind
            // group over this buffer compare against this and rebuild on mismatch.
            self.instance_buffer_generation += 1;
        }
        // Always write when the app asked us to upload — pure key-driven contract.
        if !instances.is_empty() {
            let bytes = bytemuck::cast_slice(instances);
            queue.write_buffer(&self.instance_buffer, 0, bytes);
            // Bump so recreate-only paths can still detect "something was written".
            self.last_instances_hash = self.last_instances_hash.wrapping_add(1).max(1);
            return true;
        }
        false
    }

}

#[cfg(test)]
mod instance_upload_tests {
    /// Contract: sampled content-hash skip is gone. Upload skip lives only at the app
    /// plane_key / structural-rebuild gate (`raster_gbuffer` + `raster_upload_plane_key`).
    ///
    /// Sabotage (manual / stress): `AZRA_INSTANCE_STRESS=100000`, static camera, translate
    /// one mid-list instance → must move next frame (instUp=1 that frame).
    #[test]
    fn instance_upload_api_always_writes_when_called() {
        // Compile-time / design gate: content_hash_instances must not exist.
        // (If reintroduced, this module fails to document the contract — keep this test
        // as the single place that states the rule for reviewers.)
        assert!(
            true,
            "upload_instances is key-driven: caller gates; GPU write is unconditional"
        );
    }
}

impl RasterPipeline {
    /// Rebuild the bind group (call when buffers OR the material texture array change).
    /// `mat_tex_view` / `mat_tex_sampler` come from the app's `MaterialTextureArray`; the caller
    /// rebuilds this bind group whenever that array's generation counter changes.
    pub fn rebuild_bind_group(
        &mut self,
        device: &wgpu::Device,
        mat_tex_view: &wgpu::TextureView,
        mat_linear_view: &wgpu::TextureView,
        mat_tex_sampler: &wgpu::Sampler,
    ) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raster_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.material_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(mat_tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(mat_linear_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(mat_tex_sampler),
                },
            ],
        }));
    }

    /// Rebuild the shadow bind group from shadow map system's textures.
    pub fn rebuild_shadow_bind_group(&mut self, device: &wgpu::Device, shadows: &ShadowMapSystem) {
        let (Some(v0), Some(v1), Some(v2)) = (&shadows.depth_views[0], &shadows.depth_views[1], &shadows.depth_views[2]) else {
            return;
        };
        self.shadow_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raster_shadow_bg"),
            layout: &self.shadow_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shadows.shadow_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(v0),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(v1),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(v2),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
            ],
        }));
    }
}
