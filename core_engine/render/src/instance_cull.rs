//! Phase D1 — GPU-driven instance frustum culling (PHASE_D1_INSTANCE_CULL_PLAN.md).
//!
//! Owns every GPU resource of the cull pass except the instance source
//! (`RasterPipeline.instance_buffer`, which it reads via STORAGE). Created and
//! grown ONLY from `poll_background_tasks` (Frame Ordering Contract); a frame
//! where capacity or the pipeline is not ready runs the CPU cull path instead
//! (fail-closed). Buffer recreation bumps `generation`; the dispatch site caches
//! its bind group keyed on (`generation`, `instance_buffer_generation`) and
//! rebuilds on mismatch — generation counters, never timing tricks.

use wgpu::util::DeviceExt;

/// Per-instance world AABB + owning batch, uploaded on structural rebuild only.
/// Mirrors `CullMeta` in `instance_cull.wgsl` (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CullMetaGpu {
    pub aabb_min: [f32; 3],
    pub batch: u32,
    pub aabb_max: [f32; 3],
    pub _pad: u32,
}

/// Per-batch draw geometry + output base slot, uploaded on structural rebuild only.
/// Mirrors `BatchMeta` in `instance_cull.wgsl` (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BatchMetaGpu {
    pub index_count: u32,
    pub first_index: u32,
    pub base_vertex: u32,
    /// First slot of this batch's region in `visible_instances` (== the batch's
    /// first-instance offset in the packed CPU instance array).
    pub out_base: u32,
}

/// Pass mode: frustum test only (shadow cascades — camera HZB is wrong there).
pub const CULL_MODE_FRUSTUM_ONLY: u32 = 0;
/// Frustum + prev-frame HZB; HZB-fails are flagged for the late pass, not dropped.
pub const CULL_MODE_EARLY: u32 = 1;

/// Cull parameters, one slot per pass. Mirrors `CullParams` (208 bytes ≤ 256 slot).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CullParamsGpu {
    /// 6 frustum planes, Gribb–Hartmann convention of `raster_common::frustum_planes`.
    pub planes: [[f32; 4]; 6],
    /// The view-proj the planes came from (column-major) — HZB corner projection.
    pub view_proj: [[f32; 4]; 4],
    pub n_instances: u32,
    pub n_batches: u32,
    /// Non-zero = mark every instance visible (`AZRA_CULL_PASSTHROUGH` sabotage).
    pub passthrough: u32,
    /// Region bases for this pass (entries / slots, not bytes).
    pub args_base: u32,
    pub visible_base: u32,
    pub mode: u32,
    pub viewport_w: u32,
    pub viewport_h: u32,
    pub hzb_size: u32,
    pub mip_count: u32,
    pub _pad: [u32; 2],
}

/// Camera-early + 3 shadow cascades + camera-late (C4 two-pass HZB). Each pass owns a
/// param slot and a region of the args/visible buffers — one uniform buffer written
/// between dispatches of a single encoder would be clobbered (last write wins).
pub const CULL_PASSES: usize = 5;
/// Region index of the camera-late pass (C4).
pub const PASS_CAMERA_LATE: usize = 4;
/// Uniform-offset alignment for the per-pass param slots (worst-case device limit).
const PARAM_SLOT: u64 = 256;

/// GPU resources for the D1 instance-cull pass. One documented owner: `GpuCtx.raster_cull`
/// (see GPU_INVALIDATION_MATRIX.md). All (re)creation happens in `poll_background_tasks`.
pub struct InstanceCullSystem {
    pub pipeline_reset: wgpu::ComputePipeline,
    pub pipeline_cull: wgpu::ComputePipeline,
    /// C4: late retest of early HZB-rejects against the fresh pyramid.
    pub pipeline_late: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    /// CULL_PASSES × 256-B param slots (camera = slot 0, cascade c = slot 1+c).
    pub params_buffer: wgpu::Buffer,
    /// Per-instance world AABB + batch id (structural upload).
    pub cull_meta_buffer: wgpu::Buffer,
    /// Per-batch geometry + out_base (structural upload).
    pub batch_meta_buffer: wgpu::Buffer,
    /// GPU-written `DrawIndexedIndirectArgs`, one per batch. INDIRECT|STORAGE.
    pub draw_args_buffer: wgpu::Buffer,
    /// Compacted survivors, region per batch. VERTEX|STORAGE.
    pub visible_buffer: wgpu::Buffer,
    /// D2: per-instance "frustum-passed, HZB-failed" marks (early → late pass).
    pub occluded_flags_buffer: wgpu::Buffer,
    /// D2/C5: cull counter atomics (8 × u32).
    pub counters_buffer: wgpu::Buffer,
    /// 1×1 far-depth stand-in bound at the HZB slot when no pyramid exists (a BGL
    /// entry must always be bound; 1.0 never occludes, so it is also correct).
    dummy_hzb_view: wgpu::TextureView,
    pub instance_capacity: usize,
    pub batch_capacity: usize,
    /// Bumped whenever any owned buffer is recreated — cached bind groups compare and rebuild.
    pub generation: u64,
    /// (own generation, source instance_buffer generation) the cached bind groups were built
    /// at, plus one bind group per pass (differ only in their param-slot offset).
    cached_bg: Option<(u64, u64, [wgpu::BindGroup; CULL_PASSES])>,
    /// C5: counter readback staging (32 B, MAP_READ). One in flight max (Frame Budget).
    counter_staging: wgpu::Buffer,
    counter_rx: Option<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    /// Last successfully read counters [tested, frustum, hzb_early, late_recovered, ..].
    pub last_counters: [u32; 8],
}

const ARGS_STRIDE: u64 = 20; // DrawIndexedIndirectArgs: 5 × u32
const INSTANCE_STRIDE: u64 = 144; // render::mesh_gpu::InstanceData

impl InstanceCullSystem {
    /// Build pipelines + initial buffers. Called from the background poll only.
    /// `bgl_entries` / `wgsl` come from the app (`graphics::bgl::instance_cull_bgl_entries()` +
    /// `graphics::INSTANCE_CULL_WGSL` — BGL single-source rule; `render` has no `graphics` dep).
    pub fn new(
        device: &wgpu::Device,
        bgl_entries: &[wgpu::BindGroupLayoutEntry],
        wgsl: &str,
        instance_cap: usize,
        batch_cap: usize,
    ) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("instance_cull_bgl"),
            entries: bgl_entries,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("instance_cull"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("instance_cull_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let mk = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("instance_cull_params"),
            contents: &vec![0u8; CULL_PASSES * PARAM_SLOT as usize],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let (cull_meta_buffer, batch_meta_buffer, draw_args_buffer, visible_buffer) =
            Self::make_buffers(device, instance_cap, batch_cap);
        let occluded_flags_buffer = Self::make_flags(device, instance_cap);
        let counters_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance_cull_counters"),
            size: 8 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 1×1 R32Float seeded 1.0 (far) — never occludes.
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("instance_cull_dummy_hzb"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dummy_hzb_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            pipeline_reset: mk("cs_reset_args"),
            pipeline_cull: mk("cs_frustum_cull"),
            pipeline_late: mk("cs_hzb_late"),
            bgl,
            params_buffer,
            cull_meta_buffer,
            batch_meta_buffer,
            draw_args_buffer,
            visible_buffer,
            occluded_flags_buffer,
            counters_buffer,
            dummy_hzb_view,
            instance_capacity: instance_cap,
            batch_capacity: batch_cap,
            generation: 0,
            cached_bg: None,
            counter_staging: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instance_cull_counter_staging"),
                size: 8 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            counter_rx: None,
            last_counters: [0; 8],
        }
    }

    /// C5: encode the counters → staging copy (call once per frame, pre-submit),
    /// but only when no readback is already in flight (≤1/frame, ≤1 outstanding).
    pub fn encode_counter_copy(&self, enc: &mut wgpu::CommandEncoder) {
        if self.counter_rx.is_none() {
            enc.copy_buffer_to_buffer(&self.counters_buffer, 0, &self.counter_staging, 0, 32);
        }
    }

    /// C5: post-submit — start the async map if idle, else drain a completed one.
    /// Non-blocking; `last_counters` updates 1–2 frames behind (fine for a scoreboard).
    pub fn poll_counters(&mut self) {
        if let Some(rx) = &self.counter_rx {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    {
                        let data = self.counter_staging.slice(..).get_mapped_range();
                        let vals: &[u32] = bytemuck::cast_slice(&data);
                        self.last_counters.copy_from_slice(&vals[..8]);
                    }
                    self.counter_staging.unmap();
                    self.counter_rx = None;
                }
                Ok(Err(_)) => {
                    self.counter_rx = None; // map failed — retry next frame
                }
                Err(_) => {} // still in flight
            }
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            self.counter_staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.counter_rx = Some(rx);
        }
    }

    fn make_buffers(
        device: &wgpu::Device,
        instance_cap: usize,
        batch_cap: usize,
    ) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
        let mk = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
        };
        (
            mk(
                "instance_cull_meta",
                (instance_cap * std::mem::size_of::<CullMetaGpu>()) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            mk(
                "instance_cull_batches",
                (batch_cap * std::mem::size_of::<BatchMetaGpu>()) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            mk(
                "instance_cull_args",
                CULL_PASSES as u64 * batch_cap as u64 * ARGS_STRIDE,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
            ),
            mk(
                "instance_cull_visible",
                CULL_PASSES as u64 * instance_cap as u64 * INSTANCE_STRIDE,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
            ),
        )
    }

    fn make_flags(device: &wgpu::Device, instance_cap: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance_cull_flags"),
            size: (instance_cap * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    }

    /// Grow capacities (poll phase only). Recreates the owned buffers and bumps
    /// `generation`; the cached bind group invalidates itself on next use.
    pub fn ensure_capacity(&mut self, device: &wgpu::Device, instances: usize, batches: usize) {
        if instances <= self.instance_capacity && batches <= self.batch_capacity {
            return;
        }
        self.instance_capacity = self.instance_capacity.max(instances.next_power_of_two());
        self.batch_capacity = self.batch_capacity.max(batches.next_power_of_two());
        let (cm, bm, da, vis) = Self::make_buffers(device, self.instance_capacity, self.batch_capacity);
        self.cull_meta_buffer = cm;
        self.batch_meta_buffer = bm;
        self.draw_args_buffer = da;
        self.visible_buffer = vis;
        self.occluded_flags_buffer = Self::make_flags(device, self.instance_capacity);
        self.generation += 1;
        self.cached_bg = None;
    }

    /// Ensure the cached bind group matches the current buffers + the shared instance
    /// buffer; rebuilt when either generation moves (GPU Resource Lifetime rule —
    /// detection same frame). Two-phase with `cached_bind_group()` so the dispatch site
    /// can drop the &mut before binding pipelines.
    /// `hzb`: the pyramid's full-chain view + its generation, or None → a 1×1 far
    /// dummy is bound (mode must then be FRUSTUM_ONLY — the dummy never occludes
    /// either way). The generation participates in the cache key (fold the None
    /// case to u64::MAX so swapping dummy↔real always rebuilds).
    pub fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        instance_buffer: &wgpu::Buffer,
        instance_buffer_generation: u64,
        hzb: Option<(&wgpu::TextureView, u64)>,
    ) {
        let hzb_key = hzb.map(|(_, g)| g).unwrap_or(u64::MAX);
        // Fold the hzb generation into the instance-buffer slot of the key (two u64s
        // xor-mixed — distinct generations can't cancel because hzb gens are small
        // and instance gens are small; collisions would need equal xor, acceptable
        // for a cache key that only risks an extra rebuild, never a stale bind).
        let combined = instance_buffer_generation ^ hzb_key.rotate_left(32);
        let key_ok = matches!(
            &self.cached_bg,
            Some((g, ig, _)) if *g == self.generation && *ig == combined
        );
        let instance_buffer_generation = combined;
        if !key_ok {
            let bgs = std::array::from_fn(|p| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("instance_cull_bg_p{p}")),
                    layout: &self.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.params_buffer,
                                offset: p as u64 * PARAM_SLOT,
                                size: wgpu::BufferSize::new(std::mem::size_of::<CullParamsGpu>() as u64),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: instance_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.cull_meta_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.batch_meta_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: self.draw_args_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 5, resource: self.visible_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(
                                hzb.map(|(v, _)| v).unwrap_or(&self.dummy_hzb_view),
                            ),
                        },
                        wgpu::BindGroupEntry { binding: 7, resource: self.occluded_flags_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 8, resource: self.counters_buffer.as_entire_binding() },
                    ],
                })
            });
            self.cached_bg = Some((self.generation, instance_buffer_generation, bgs));
        }
    }

    /// Write pass `p`'s param slot. `args_base`/`visible_base` are filled here from the
    /// current capacities — callers only supply planes/counts/passthrough.
    pub fn write_pass_params(&self, queue: &wgpu::Queue, p: usize, mut params: CullParamsGpu) {
        params.args_base = (p * self.batch_capacity) as u32;
        params.visible_base = (p * self.instance_capacity) as u32;
        queue.write_buffer(&self.params_buffer, p as u64 * PARAM_SLOT, bytemuck::bytes_of(&params));
    }

    /// Byte offset of batch `bi`'s indirect args for pass `p`.
    pub fn args_offset(&self, p: usize, bi: usize) -> u64 {
        (p * self.batch_capacity + bi) as u64 * ARGS_STRIDE
    }

    /// Byte offset of instance slot `first` in pass `p`'s visible region.
    pub fn visible_offset(&self, p: usize, first: u32) -> u64 {
        (p * self.instance_capacity) as u64 * INSTANCE_STRIDE + first as u64 * INSTANCE_STRIDE
    }

    /// Pass `p`'s bind group (after `ensure_bind_group`). Panics if never ensured —
    /// dispatch without ensure is a programming error, not a runtime state.
    pub fn pass_bind_group(&self, p: usize) -> &wgpu::BindGroup {
        &self.cached_bg.as_ref().expect("ensure_bind_group before dispatch").2[p]
    }
}
