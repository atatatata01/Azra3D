//! Phase D2 — Hi-Z (HZB) max-depth pyramid (PHASE_D2_HZB_PLAN.md).
//!
//! R32Float pyramid, power-of-two padded (pad seeded 1.0 = far, so padding never
//! occludes and every level is an exact 2×2 max reduce). Owner: `GpuCtx.hzb`.
//! (Re)creation ONLY in `poll_background_tasks` (Frame Ordering Contract); the
//! build passes are encoded in `dispatch_raster_pass` after the G-buffer pass.
//! `generation` bumps on recreate; `valid` is false until the first build after
//! (re)creation — consumers must fail closed to frustum-only culling while false.

/// Mirrors `HzbParams` in `hzb_build.wgsl` (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HzbParamsGpu {
    pub src_size: [u32; 2],
    pub dst_size: [u32; 2],
}

pub struct HzbSystem {
    pub texture: wgpu::Texture,
    /// One view per mip level (storage write target / sampled source).
    pub mip_views: Vec<wgpu::TextureView>,
    /// Whole-chain sampled view for the cull shader (all mips).
    pub full_view: wgpu::TextureView,
    pub seed_pipeline: wgpu::ComputePipeline,
    pub down_pipeline: wgpu::ComputePipeline,
    pub seed_bgl: wgpu::BindGroupLayout,
    pub down_bgl: wgpu::BindGroupLayout,
    /// Per-level params uniforms (256-aligned slots, one per mip — a single buffer
    /// written between dispatches of one encoder would be clobbered).
    pub params_buffer: wgpu::Buffer,
    /// Pow2 side of mip 0.
    pub size: u32,
    pub mip_count: u32,
    /// Viewport (valid depth) extent the pyramid was last sized for.
    pub viewport: (u32, u32),
    /// Bumped on texture recreate — cached bind groups compare and rebuild.
    pub generation: u64,
    /// False until the first build after (re)create — cull must skip HZB while false.
    pub valid: bool,
    /// (own generation, source depth generation) → [seed bg, down bg per mip 1..].
    cached_bgs: Option<(u64, u64, Vec<wgpu::BindGroup>)>,
}

const PARAM_SLOT: u64 = 256;

impl HzbSystem {
    /// Pow2 side covering `w × h`.
    pub fn side_for(w: u32, h: u32) -> u32 {
        w.max(h).max(1).next_power_of_two()
    }

    /// Build pipelines + pyramid for a viewport. Poll phase only. `seed_entries` /
    /// `down_entries` / `wgsl` come from the app (`graphics::bgl` — single-source rule).
    pub fn new(
        device: &wgpu::Device,
        seed_entries: &[wgpu::BindGroupLayoutEntry],
        down_entries: &[wgpu::BindGroupLayoutEntry],
        wgsl: &str,
        viewport_w: u32,
        viewport_h: u32,
    ) -> Self {
        let seed_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hzb_seed_bgl"),
            entries: seed_entries,
        });
        let down_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hzb_down_bgl"),
            entries: down_entries,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hzb_build"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let mk_pipe = |bgl: &wgpu::BindGroupLayout, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[bgl],
                    push_constant_ranges: &[],
                })),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let seed_pipeline = mk_pipe(&seed_bgl, "cs_hzb_seed");
        let down_pipeline = mk_pipe(&down_bgl, "cs_hzb_downsample");
        let (texture, mip_views, full_view, size, mip_count) =
            Self::make_pyramid(device, viewport_w, viewport_h);
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hzb_params"),
            size: mip_count as u64 * PARAM_SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            texture,
            mip_views,
            full_view,
            seed_pipeline,
            down_pipeline,
            seed_bgl,
            down_bgl,
            params_buffer,
            size,
            mip_count,
            viewport: (viewport_w, viewport_h),
            generation: 0,
            valid: false,
            cached_bgs: None,
        }
    }

    fn make_pyramid(
        device: &wgpu::Device,
        viewport_w: u32,
        viewport_h: u32,
    ) -> (wgpu::Texture, Vec<wgpu::TextureView>, wgpu::TextureView, u32, u32) {
        let size = Self::side_for(viewport_w, viewport_h);
        let mip_count = 32 - size.leading_zeros(); // log2(size) + 1
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hzb_pyramid"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC, // AZRA_HZB_DUMP diagnostic readback
            view_formats: &[],
        });
        let mip_views = (0..mip_count)
            .map(|m| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some(&format!("hzb_mip{m}")),
                    base_mip_level: m,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let full_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, mip_views, full_view, size, mip_count)
    }

    /// Recreate the pyramid when the viewport changed. Poll phase only.
    /// Returns true if recreated (generation bumped, valid cleared).
    pub fn ensure_size(&mut self, device: &wgpu::Device, viewport_w: u32, viewport_h: u32) -> bool {
        if self.viewport == (viewport_w, viewport_h) {
            return false;
        }
        self.viewport = (viewport_w, viewport_h);
        let new_side = Self::side_for(viewport_w, viewport_h);
        self.valid = false; // depth content no longer matches the viewport
        if new_side == self.size {
            return false; // same pyramid, just a different valid region (params handle it)
        }
        let (texture, mip_views, full_view, size, mip_count) =
            Self::make_pyramid(device, viewport_w, viewport_h);
        self.texture = texture;
        self.mip_views = mip_views;
        self.full_view = full_view;
        self.size = size;
        if mip_count != self.mip_count {
            self.mip_count = mip_count;
            self.params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hzb_params"),
                size: mip_count as u64 * PARAM_SLOT,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.generation += 1;
        true
    }

    /// Write per-level params (slot 0 = seed, slot m = downsample into mip m).
    pub fn write_level_params(&self, queue: &wgpu::Queue) {
        let (vw, vh) = self.viewport;
        for m in 0..self.mip_count {
            let dst = self.size >> m;
            let p = HzbParamsGpu {
                // Seed's src = viewport extent; downsample's src = previous mip extent.
                src_size: if m == 0 { [vw, vh] } else { [self.size >> (m - 1), self.size >> (m - 1)] },
                dst_size: [dst.max(1), dst.max(1)],
            };
            queue.write_buffer(&self.params_buffer, m as u64 * PARAM_SLOT, bytemuck::bytes_of(&p));
        }
    }

    /// Byte offset of level `m`'s params slot (for BufferBinding at bind-group build).
    pub fn params_offset(m: u32) -> u64 {
        m as u64 * PARAM_SLOT
    }

    /// Ensure the per-level bind groups match the current pyramid + depth attachment
    /// (generation counters, same-frame detection). Index 0 = seed (depth → mip 0),
    /// index m ≥ 1 = downsample (mip m-1 → mip m).
    pub fn ensure_bind_groups(
        &mut self,
        device: &wgpu::Device,
        depth_view: &wgpu::TextureView,
        depth_generation: u64,
    ) {
        let key_ok = matches!(
            &self.cached_bgs,
            Some((g, dg, _)) if *g == self.generation && *dg == depth_generation
        );
        if key_ok {
            return;
        }
        let params = |m: u32| {
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &self.params_buffer,
                    offset: Self::params_offset(m),
                    size: wgpu::BufferSize::new(std::mem::size_of::<HzbParamsGpu>() as u64),
                }),
            }
        };
        let mut bgs = Vec::with_capacity(self.mip_count as usize);
        bgs.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hzb_seed_bg"),
            layout: &self.seed_bgl,
            entries: &[
                params(0),
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(depth_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.mip_views[0]) },
            ],
        }));
        for m in 1..self.mip_count {
            bgs.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("hzb_down_bg_m{m}")),
                layout: &self.down_bgl,
                entries: &[
                    params(m),
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.mip_views[m as usize]) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.mip_views[(m - 1) as usize]) },
                ],
            }));
        }
        self.cached_bgs = Some((self.generation, depth_generation, bgs));
    }

    /// Encode the full pyramid build (seed + downsample chain) into `enc`.
    /// Caller must have called `write_level_params` + `ensure_bind_groups` this frame.
    /// Sets `valid` — the cull may sample the pyramid from the NEXT dispatch on.
    pub fn encode_build(&mut self, enc: &mut wgpu::CommandEncoder) {
        let Some((_, _, bgs)) = &self.cached_bgs else { return };
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("hzb_build"),
            timestamp_writes: None,
        });
        cp.set_pipeline(&self.seed_pipeline);
        cp.set_bind_group(0, &bgs[0], &[]);
        let d0 = self.size.div_ceil(8);
        cp.dispatch_workgroups(d0, d0, 1);
        cp.set_pipeline(&self.down_pipeline);
        for m in 1..self.mip_count {
            cp.set_bind_group(0, &bgs[m as usize], &[]);
            let d = (self.size >> m).max(1).div_ceil(8);
            cp.dispatch_workgroups(d, d, 1);
        }
        drop(cp);
        self.valid = true;
    }
}
