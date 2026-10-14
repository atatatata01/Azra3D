// GPU Surface-Nets mesher + shared mesh pool (relocated from voxel_gpu_mesh god-file).
// Generation-counter comments travel with their owning code.

use core_math::voxel_sculpt::VoxelGrid;
use eframe::wgpu;

/// Uniform params for `voxel_mesh.wgsl` — must match the WGSL `SnParams` layout exactly
/// (5 × vec4 = 80 bytes; vec4 alignment).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SnParams {
    a0: [u32; 4],    // xyz = anchor cell min (quads), w = res
    a1: [u32; 4],    // xyz = anchor cell max (inclusive)
    vmin: [u32; 4],  // xyz = vertex region min (= max(a0-1, 0))
    vspan: [u32; 4], // xyz = vertex region size (= a1 - vmin + 1)
    misc: [f32; 4],  // x = iso, y = bounds, z = voxel_size, w = unused
}

/// Where `dispatch_chunks` reads density from. DENSE = a flat res³ buffer (res < 256, the proven
/// path). Atlas = the sparse brick INDEX + POOL buffers (res ≥ 256 → 1024³ without a res³ buffer).
pub(crate) enum MeshDensitySource<'a> {
    Dense(&'a wgpu::Buffer),
    Atlas { index: &'a wgpu::Buffer, pool: &'a wgpu::Buffer, brick_res: u32 },
}

/// GPU-RESIDENT per-chunk mesh state (Phase 6 + POOL): the Surface-Nets compute writes verts/idx straight
/// into a SUB-RANGE of the shared `VoxelMeshGpu` POOL (no CPU readback); the rasteriser draws ALL chunks of
/// a body in ONE `multi_draw_indexed_indirect` over the pool — replacing the ~12k per-chunk draw calls +
/// ~320 buffer-creates/frame that were the measured 1024³ wall. Owned by `GpuCtx.voxel_chunk_gpu` (one slot
/// per chunk linear index, parallel to `VoxelObject.chunks`). A chunk no longer owns ANY of its own buffers
/// — it holds a `pool_slot` index, and ALL its storage (verts/idx/indirect AND the counters/params it used
/// to own) is a SUB-RANGE of a shared `VoxelMeshGpu` pool selected by `pool_slot`. The compute write binds
/// the pool's vertex/index/counters/params sub-ranges for this slot (bindings 3/4/5/1); because each is a
/// sub-range, the shader's slot-local (`vi*8`, `ii`, `params.*`, `atomicAdd(&counters[0/1])`) writes +
/// `arrayLength` overflow guards work UNCHANGED (the bound sub-range size IS the per-slot cap). `generation`
/// bumps when the chunk's slot caps change (a re-`ensure_chunk_gpu` for a cap grow).
pub(crate) struct VoxelChunkGpu {
    /// Assigned slot in the shared `VoxelMeshGpu` pool. The chunk's vertex region is the pool's
    /// `[pool_slot*vert_stride .. +vert_stride)` VERTS (8 f32/vert) sub-range; its index region the pool's
    /// `[pool_slot*idx_stride .. +idx_stride)` u32 sub-range; its indirect args live at byte
    /// `pool_slot*20` in `pool_indirect` (5×u32 per DrawIndexedIndirect entry); its counters + params each
    /// live in the `[pool_slot*256 .. +256)` sub-range of `pool_counters` / `pool_params`.
    pub(crate) pool_slot: u32,
    /// SLOT-CONTENT GENERATION — bumped (from the mesher's ONE monotonic counter, so a
    /// (slot, generation) pair can never repeat across release/re-acquire) every time the slot's
    /// content provenance changes: `ensure_chunk_gpu` (fresh slot / cap-grow indirect re-seed) and
    /// EVERY `dispatch_chunks_resident` that GPU-meshes into the slot. The surface-mode sync key
    /// (`surface_pool_synced`, keyed (pool_slot, generation, pool_gen)) compares against this, so a
    /// verts-only upload over content some OTHER producer wrote (the GPU mesher's foreign vertex
    /// order — the chunk-plate / needle display class) is impossible BY CONSTRUCTION, not by
    /// scattered flag-clearing call sites. GPU Resource Lifetime rule: generation counters over
    /// timing/flag tricks.
    pub(crate) generation: u32,
    cap_verts: u32,
    cap_idx: u32,
    /// CACHED bind group for this chunk's two compute passes. Built once, reused every frame; this kills
    /// the per-chunk `create_bind_group` that was the remaining resident-dispatch CPU cost after the
    /// scratch-buffer win (4d200b8). `None` after (re)create — forces a rebuild. `bg_res_gen` covers the
    /// shared-resource (cell_vert / density / atlas / POOL) invalidation.
    bind_group: Option<wgpu::BindGroup>,
    /// Combined generation of the SHARED resources the cached `bind_group` REFERENCES but does NOT own:
    /// `VoxelMeshGpu.mesh_res_gen` (the `res_cell_vert` scratch grow) + `pool_gen` (the verts/idx/counters/
    /// params pool realloc — the slot's sub-range bindings would otherwise dangle) + the caller's `src_gen`
    /// (atlas index/pool or dense density recreate). Stored as the `wrapping_add` of the three. The BG is
    /// rebuilt when this no longer equals the current combined gen. A cap-grow re-`ensure_chunk_gpu` sets
    /// `bind_group = None`, an independent invalidation.
    bg_res_gen: u32,
}


/// On-demand GPU Surface-Nets mesher (one BGL + the two compute pipelines).
pub(crate) struct VoxelMeshGpu {
    bgl: wgpu::BindGroupLayout,
    pipe_vertex: wgpu::ComputePipeline,
    pipe_quads: wgpu::ComputePipeline,
    /// Persistent SCRATCH cell_vert buffer shared by EVERY chunk in `dispatch_chunks_resident` (the
    /// per-cell vertex index the vertex pass writes + the quads pass reads, never CPU-read). Grown to
    /// the batch's largest chunk so ONE allocation serves all chunks — removes the per-chunk ~20KB
    /// `create_buffer` that dominated the resident dispatch CPU cost. STORAGE; `res_cell_cap` = bytes.
    res_cell_vert: Option<wgpu::Buffer>,
    res_cell_cap: u64,
    /// Generation of the SHARED scratch the per-chunk cached bind groups REFERENCE but don't own:
    /// incremented whenever `res_cell_vert` is reallocated (grow). Folded with the caller's `src_gen`
    /// into each chunk's `bg_res_gen` so a grown scratch buffer invalidates EVERY cached chunk BG (the
    /// old buffer would be dangling). See `dispatch_chunks_resident`.
    mesh_res_gen: u32,
    /// Persistent 4-byte dummy STORAGE buffer for the inactive density source (binding 0 for atlas mode,
    /// bindings 6/7 for dense mode — the shader never indexes it). Created ONCE and reused so the cached
    /// per-chunk bind groups bind a STABLE buffer (a per-dispatch dummy would either dangle in a cached BG
    /// or force a rebuild every frame). Its lifetime is the mesher's, so a cached BG reference stays valid.
    res_dummy: Option<wgpu::Buffer>,
    // --- SHARED VERTEX/INDEX/INDIRECT POOL (the 1024³ draw-bound fix) ---------------------------
    // ONE big vertex buffer + ONE big index buffer + ONE indirect-args buffer, partitioned into fixed-
    // size SLOTS (one per resident chunk). The compute pass binds a chunk's slot as a STORAGE SUB-RANGE
    // (BufferBinding { offset, size }) so the shader writes slot-local exactly as before; the rasteriser
    // binds the WHOLE pool as vertex/index buffer and draws every slot in ONE `multi_draw_indexed_indirect`
    // (empty slots have index_count=0 → self-skip). This replaces ~12k per-chunk draw calls + ~320 per-chunk
    // buffer creates/frame (the measured wall: gpu-mesh ~5ms vs frame ~103ms at 1024³).
    pool_verts: Option<wgpu::Buffer>,    // STORAGE|VERTEX|COPY_DST|COPY_SRC — array<f32>, 8 f32/vert
    pool_idx: Option<wgpu::Buffer>,      // STORAGE|INDEX|COPY_DST|COPY_SRC (Uint32)
    pool_indirect: Option<wgpu::Buffer>, // INDIRECT|COPY_DST|COPY_SRC — pool_cap_slots × DrawIndexedIndirect (5 u32)
    pool_counters: Option<wgpu::Buffer>, // STORAGE|COPY_DST|COPY_SRC — POOL_SLOT_META_BYTES per slot, first 8B = [vert_count, index_count]
    pool_params: Option<wgpu::Buffer>,   // UNIFORM|COPY_DST — POOL_SLOT_META_BYTES per slot, holds one 80B SnParams
    /// Bumped on every pool (re)alloc — folded into each chunk's `bg_res_gen` so a realloc invalidates
    /// EVERY cached chunk bind group (the slot's sub-range binding into the OLD buffer is now dangling).
    pool_gen: u32,
    /// Monotonic source for `VoxelChunkGpu.generation` (slot-content generations). ONE counter for
    /// the whole pool so a (slot, generation) pair never repeats — a freed slot re-acquired by a new
    /// chunk gets a generation no prior sync key can match (no ABA).
    content_counter: u32,
    pool_cap_slots: u32, // number of slots the pool currently holds
    pool_next: u32,      // bump pointer for never-yet-used slots
    pool_free: Vec<u32>, // freed slot indices (reuse before bumping pool_next)
    /// f32-VERTS per slot (8 f32/vert), padded so a slot's byte offset (slot*vert_stride*32) is %256==0.
    vert_stride: u32,
    /// u32-INDICES per slot, padded so a slot's byte offset (slot*idx_stride*4) is %256==0.
    idx_stride: u32,
    // --- DIAGNOSTIC: GPU time of the two mesh compute passes (vertex + quads) for the whole batch.
    // Self-contained (the dispatch submits on its own encoder, separate from the main render's timestamp
    // system) so we can tell whether 1024³ is MESH-bound (this is large) or DRAW-bound (this is small but
    // the whole frame is slow). timestamp 0 = begin of vertex pass, 1 = end of quads pass; duration spans
    // both passes + the barrier between. Read back with one-frame latency (the standard map_async pattern).
    ts_query: Option<wgpu::QuerySet>,
    ts_resolve: Option<wgpu::Buffer>,  // QUERY_RESOLVE | COPY_SRC — 2×u64
    ts_staging: Option<wgpu::Buffer>,  // COPY_DST | MAP_READ
    ts_period: f32,                    // ns per timestamp tick (queue.get_timestamp_period())
    ts_pending: bool,                  // a resolve is in flight (staging mapped) — don't resolve again
    ts_map_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Last measured GPU time (ms) of the mesh dispatch's two compute passes. 0 until the first readback.
    pub(crate) last_mesh_gpu_ms: f32,
}

impl VoxelMeshGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        // 6 storage/uniform bindings, all COMPUTE — must match voxel_mesh.wgsl @group(0).
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("voxel_mesh_bgl"),
            entries: &[
                storage(0, true), // density (read)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }, // params (uniform)
                storage(2, false), // cell_vert (rw)
                storage(3, false), // verts (rw)
                storage(4, false), // indices (rw)
                storage(5, false), // counters (rw, atomic)
                storage(6, true),  // index (sparse atlas top-level tags, read) — Phase 4b
                storage(7, true),  // pool  (sparse atlas brick arena, read)    — Phase 4b
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voxel_mesh.wgsl"),
            source: wgpu::ShaderSource::Wgsl(graphics::VOXEL_MESH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("voxel_mesh_pipeline_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let make = |entry: &str| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("voxel_mesh_pipeline"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        // Pool slot strides (a CAPACITY BOUND per chunk at VOXEL_CHUNK=16, padded for the 256B storage
        // sub-range offset alignment). NOT the 17³/16³·18 checkerboard worst case — see the
        // POOL_VERTS_PER_SLOT/POOL_IDX_PER_SLOT const docs (worst-case strides would make the 1024³
        // index pool a single ~3.5GB allocation). A chunk pair exceeding a slot is REFUSED by
        // `upload_cpu_chunk`/`cpu_pair_fits` (CPU path) or clamped by the shader's arrayLength guard
        // (GPU path). 2048*32 = 65536 B and 16384*4 = 65536 B, both %256==0. Independent of resolution.
        let vert_stride = (POOL_VERTS_PER_SLOT + 7) & !7;        // round up to multiple of 8
        let idx_stride = (POOL_IDX_PER_SLOT + 63) & !63;         // round up to multiple of 64
        debug_assert_eq!((vert_stride as u64 * 32) % 256, 0, "vert slot byte offset must be 256B-aligned");
        debug_assert_eq!((idx_stride as u64 * 4) % 256, 0, "idx slot byte offset must be 256B-aligned");
        Self {
            bgl, pipe_vertex: make("sn_vertex"), pipe_quads: make("sn_quads"),
            res_cell_vert: None, res_cell_cap: 0, mesh_res_gen: 0, res_dummy: None,
            pool_verts: None, pool_idx: None, pool_indirect: None,
            pool_counters: None, pool_params: None, pool_gen: 0, content_counter: 0,
            pool_cap_slots: 0, pool_next: 0, pool_free: Vec::new(), vert_stride, idx_stride,
            ts_query: None, ts_resolve: None, ts_staging: None, ts_period: 0.0,
            ts_pending: false, ts_map_ready: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_mesh_gpu_ms: 0.0,
        }
    }

    /// Dispatch a GPU mesh of MANY anchor ranges in ONE submission and return an `VoxelMeshInflight`
    /// WITHOUT blocking — the staging buffers map asynchronously. `density_buf` is a PERSISTENT `res³`
    /// buffer the caller keeps current (filled once, patched by dirty Z-band each frame — Phase 3c),
    /// shared by every chunk dispatch; each range gets its own small output buffers (a 16³ chunk is
    /// tiny). The caller polls each frame and calls `collect()` only when `map_state()` is Ready. This is the
    /// live sculpt path: the CPU submits and moves on (vs the CPU mesher's ~1.5 ms/chunk on the UI thread).
    pub(crate) fn dispatch_chunks(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &VoxelGrid,
        src: MeshDensitySource,
        ranges: &[([u32; 3], [u32; 3])],
    ) -> VoxelMeshInflight {
        // The shader reads either a DENSE res³ buffer (brick_res==0) or the SPARSE atlas (index+pool).
        // Bind all three; the inactive one is a tiny dummy (the shader never indexes it). Phase 4b.
        let dummy = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vmc_dummy"), size: 4, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let (density_buf, index_buf, pool_buf, brick_res): (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer, u32) = match src {
            MeshDensitySource::Dense(d) => (d, &dummy, &dummy, 0),
            MeshDensitySource::Atlas { index, pool, brick_res } => (&dummy, index, pool, brick_res),
        };
        let max_bind = device.limits().max_storage_buffer_binding_size as u64;
        let mk = |label: &str, size: u64, extra: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label), size: size.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC | extra,
            mapped_at_creation: false,
        });
        let stage = |label: &str, size: u64| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label), size: size.max(4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let wg = |n: u32| (n + 3) / 4; // workgroup_size = (4,4,4)
        let mut jobs: Vec<ChunkStaging> = Vec::with_capacity(ranges.len());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vmc_enc") });

        for &(a0, a1) in ranges {
            let vmin = [a0[0].saturating_sub(1), a0[1].saturating_sub(1), a0[2].saturating_sub(1)];
            let vspan = [a1[0]-vmin[0]+1, a1[1]-vmin[1]+1, a1[2]-vmin[2]+1];
            let vcells = (vspan[0] as u64)*(vspan[1] as u64)*(vspan[2] as u64);
            let aspan = [a1[0]-a0[0]+1, a1[1]-a0[1]+1, a1[2]-a0[2]+1];
            let acells = (aspan[0] as u64)*(aspan[1] as u64)*(aspan[2] as u64);
            let vert_cap = vcells.max(1).min(max_bind / 32);      // 8 f32 = 32 bytes / vertex
            let idx_cap = (acells * SN_MAX_IDX_PER_ANCHOR_CELL as u64).max(6).min(max_bind / 4); // u32 = 4 bytes / index

            let params = SnParams {
                a0: [a0[0],a0[1],a0[2], grid.res], a1: [a1[0],a1[1],a1[2], 0],
                vmin: [vmin[0],vmin[1],vmin[2], 0], vspan: [vspan[0],vspan[1],vspan[2], 0],
                misc: [grid.iso, grid.bounds, grid.voxel_size(), f32::from_bits(brick_res)], // w = brick_res (0=dense)
            };
            let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vmc_params"), size: std::mem::size_of::<SnParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));
            let cell_vert = mk("vmc_cell_vert", vcells * 4, wgpu::BufferUsages::empty());
            let verts = mk("vmc_verts", vert_cap * 32, wgpu::BufferUsages::empty());
            let idx = mk("vmc_idx", idx_cap * 4, wgpu::BufferUsages::empty());
            let counters = mk("vmc_counters", 8, wgpu::BufferUsages::empty());
            queue.write_buffer(&counters, 0, bytemuck::cast_slice(&[0u32, 0u32]));

            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vmc_bg"), layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: cell_vert.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: verts.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: idx.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: counters.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: index_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: pool_buf.as_entire_binding() },
                ],
            });
            {
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vmc_vertex"), timestamp_writes: None });
                p.set_pipeline(&self.pipe_vertex); p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(wg(vspan[0]), wg(vspan[1]), wg(vspan[2]));
            }
            // Separate pass → wgpu inserts the cell_vert write→read barrier between the passes.
            {
                let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vmc_quads"), timestamp_writes: None });
                p.set_pipeline(&self.pipe_quads); p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(wg(aspan[0]), wg(aspan[1]), wg(aspan[2]));
            }
            let s_counters = stage("vmc_s_counters", 8);
            let s_verts = stage("vmc_s_verts", vert_cap * 32);
            let s_idx = stage("vmc_s_idx", idx_cap * 4);
            enc.copy_buffer_to_buffer(&counters, 0, &s_counters, 0, 8);
            enc.copy_buffer_to_buffer(&verts, 0, &s_verts, 0, vert_cap * 32);
            enc.copy_buffer_to_buffer(&idx, 0, &s_idx, 0, idx_cap * 4);
            // params_buf/cell_vert/verts/idx/counters/bg drop here — wgpu keeps them alive for the
            // submitted command buffer until the GPU finishes (only the staging + density are kept).
            jobs.push(ChunkStaging { s_counters, s_verts, s_idx, vert_cap, idx_cap });
        }
        queue.submit(std::iter::once(enc.finish()));

        // Kick off the async maps; completion and failure are both captured by the callbacks.
        let completed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let want = jobs.len() * 3;
        for j in &jobs {
            for buf in [&j.s_counters, &j.s_verts, &j.s_idx] {
                let done = completed.clone();
                let errors = failed.clone();
                buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
                    if let Err(err) = res {
                        errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("[VXGPU] mesh readback map failed: {err:?}");
                    }
                    done.fetch_add(1, std::sync::atomic::Ordering::Release);
                });
            }
        }
        VoxelMeshInflight { jobs, completed, failed, want }
    }

    /// Pool buffer byte size for `slots`, clamped so each pool buffer stays ≤ a SAFE single-allocation
    /// size. Returns `(verts_bytes, idx_bytes, indirect_bytes, slots)` — `slots` may be CLAMPED below the
    /// request. We clamp to `min(max_buffer_size, POOL_MAX_BUFFER_BYTES)`: a device may *report* a huge
    /// `max_buffer_size` (NVIDIA reports ~u64::MAX) yet still FAIL a single multi-GB allocation, so the
    /// 1GB hard cap is what actually keeps the create reliable (the per-chunk-buffer era worked because
    /// every buffer was tiny). With the realistic slot strides (~64KB/slot for both verts and idx) 1GB
    /// holds ~16k slots — comfortably above the ~12k surface chunks at 1024³.
    fn pool_byte_layout(&self, device: &wgpu::Device, slots: u32) -> (u64, u64, u64, u32) {
        let safe_max = device.limits().max_buffer_size.min(POOL_MAX_BUFFER_BYTES);
        let vbytes_per = self.vert_stride as u64 * 32;
        let ibytes_per = self.idx_stride as u64 * 4;
        let ind_per = POOL_INDIRECT_STRIDE_BYTES; // 20
        // Clamp on whichever pool is largest per slot (apply the same slot count to all three).
        let max_slots_idx = (safe_max / ibytes_per).max(1) as u32;
        let max_slots_vert = (safe_max / vbytes_per).max(1) as u32;
        let fit = slots.min(max_slots_idx).min(max_slots_vert).max(1);
        if fit < slots {
            eprintln!("[VXPOOL] requested {} slots but the {:.1}GB per-buffer cap fits only {} \
                (idx {:.1}MB + verts {:.1}MB) — excess chunks fall back to the CPU mesh path this session",
                slots, safe_max as f64 / 1.0e9, fit,
                (fit as u64 * ibytes_per) as f64 / 1.0e6, (fit as u64 * vbytes_per) as f64 / 1.0e6);
        }
        (fit as u64 * vbytes_per, fit as u64 * ibytes_per, fit as u64 * ind_per, fit)
    }

    /// (Re)allocate the three pool buffers to hold `slots` slots (clamped to `max_buffer_size`) if the
    /// current capacity is smaller. On realloc: bump `pool_gen` (invalidates every cached chunk BG — the
    /// slot sub-range binding into the old buffer is now dangling), reset the allocator (`pool_next=0`,
    /// `pool_free.clear()`), and zero `pool_indirect` so EVERY slot starts empty (index_count=0 = a draw
    /// no-op). POLL-PHASE ONLY (Frame Ordering Contract — these are multi-GB buffer creates). Idempotent
    /// when `pool_cap_slots >= slots`.
    pub(crate) fn ensure_pool(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, slots: u32) {
        if self.pool_cap_slots >= slots && self.pool_verts.is_some() { return; }
        let (vbytes, ibytes, indbytes, fit) = self.pool_byte_layout(device, slots.max(1));
        self.pool_verts = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vxpool_verts"),
            size: vbytes.max(32),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false }));
        self.pool_idx = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vxpool_idx"),
            size: ibytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false }));
        let indirect = device.create_buffer(&wgpu::BufferDescriptor { label: Some("vxpool_indirect"),
            size: indbytes.max(POOL_INDIRECT_STRIDE_BYTES),
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false });
        // Zero ALL indirect slots → every slot a no-op draw (index_count=0) until a chunk seeds it. Without
        // this, an unwritten slot in the multi-draw would read garbage index_count → OOB indirect draw.
        let zeros = vec![0u32; (fit as usize) * 5];
        queue.write_buffer(&indirect, 0, bytemuck::cast_slice(&zeros));
        self.pool_indirect = Some(indirect);
        // Pooled per-slot counters + params (the LAST two per-chunk buffers, now slot sub-ranges — kills the
        // ~36ms/frame ensure_chunk_gpu allocation churn at 1024³). Each slot owns a 256B stride: counters uses
        // the first 8B ([vert_count, index_count]), params holds one ≤256B SnParams. Tiny (fit*256 ≈ 4MB at
        // 16384 slots — well under any per-buffer cap). Reallocated together with verts/idx, so the SAME
        // pool_gen bump below invalidates every chunk's cached BG (the slot sub-range into the old buffer
        // would otherwise dangle). Counters needs COPY_SRC for the indirect index_count copy.
        let meta_bytes = (fit as u64 * POOL_SLOT_META_BYTES).max(POOL_SLOT_META_BYTES);
        self.pool_counters = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vxpool_counters"),
            size: meta_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false }));
        self.pool_params = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vxpool_params"),
            size: meta_bytes,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false }));
        self.pool_cap_slots = fit;
        self.pool_next = 0;
        self.pool_free.clear();
        self.pool_gen = self.pool_gen.wrapping_add(1);
        eprintln!("[VXPOOL] alloc {} slots: verts {:.1}MB + idx {:.1}MB (vert_stride={} idx_stride={}) gen={}",
            fit, vbytes as f64 / 1.0e6, ibytes as f64 / 1.0e6, self.vert_stride, self.idx_stride, self.pool_gen);
    }

    /// Pop a free slot, else bump `pool_next`, else `None` (pool full this frame — the caller leaves the
    /// chunk dirty + flags a grow). NEVER silently corrupts: an exhausted pool returns None, it does not
    /// hand out an out-of-range slot.
    pub(crate) fn alloc_slot(&mut self) -> Option<u32> {
        if let Some(s) = self.pool_free.pop() { return Some(s); }
        if self.pool_next < self.pool_cap_slots { let s = self.pool_next; self.pool_next += 1; return Some(s); }
        None
    }

    /// Reset the slot allocator WITHOUT reallocating the buffers: free every slot and zero ALL indirect
    /// entries (every slot → no-op draw). Call whenever the resident chunk set is cleared (chunk_dim swap)
    /// so the orphaned slots of the dropped chunks are reclaimed instead of leaking the pool to exhaustion.
    /// Cheaper than a realloc (keeps the buffers); does NOT bump `pool_gen` (the buffers are unchanged — any
    /// cached BG is invalidated independently by the chunks being dropped).
    pub(crate) fn reset_pool_allocator(&mut self, queue: &wgpu::Queue) {
        self.pool_next = 0;
        self.pool_free.clear();
        if let Some(ind) = self.pool_indirect.as_ref() {
            let zeros = vec![0u32; (self.pool_cap_slots as usize) * 5];
            if !zeros.is_empty() { queue.write_buffer(ind, 0, bytemuck::cast_slice(&zeros)); }
        }
    }

    pub(crate) fn pool_cap_slots(&self) -> u32 { self.pool_cap_slots }
    pub(crate) fn pool_generation(&self) -> u32 { self.pool_gen }
    /// Next slot-content generation (monotonic pool-wide — see `VoxelChunkGpu.generation`).
    fn next_content_gen(&mut self) -> u32 {
        self.content_counter = self.content_counter.wrapping_add(1);
        self.content_counter
    }
    /// The chunk's CURRENT pool sync key: (slot, slot-content generation, pool generation).
    /// A stored surface sync key equal to this proves the slot still holds the SAME buffers
    /// (pool_gen), the SAME slot (no release/re-acquire — generation is pool-monotonic) and that
    /// no other producer (GPU mesher, cap-grow indirect re-seed) rewrote the slot since the last
    /// surface topology upload (generation).
    pub(crate) fn slot_sync_key(&self, c: &VoxelChunkGpu) -> PoolSlotSyncKey {
        (c.pool_slot, c.generation, self.pool_gen)
    }
    pub(crate) fn pool_verts(&self) -> Option<&wgpu::Buffer> { self.pool_verts.as_ref() }
    pub(crate) fn pool_idx(&self) -> Option<&wgpu::Buffer> { self.pool_idx.as_ref() }
    pub(crate) fn pool_indirect(&self) -> Option<&wgpu::Buffer> { self.pool_indirect.as_ref() }

    /// True iff a CPU chunk pair of `nv` verts / `ni` indices fits inside ONE pool slot's fixed
    /// strides (`POOL_VERTS_PER_SLOT`/`POOL_IDX_PER_SLOT` — a deliberate capacity BOUND, ~3–8×
    /// the typical brush-sculpted chunk but BELOW the 17³/16³·18 checkerboard worst case, see the
    /// const docs). The surface-mode upload path pre-checks this so an oversized chunk never
    /// allocates (or keeps) a slot it can't fill — it must draw via its per-chunk CPU mesh
    /// instead. Pure (testable): the same predicate `upload_cpu_chunk` enforces independently.
    pub(crate) fn cpu_pair_fits(&self, nv: usize, ni: usize) -> bool {
        nv as u32 <= self.vert_stride && ni as u32 <= self.idx_stride
    }

    /// Return `slot` to the free list and zero its indirect entry (index_count=0 → the pool
    /// multi-draw self-skips it from this frame on). The FAIL-CLOSED exit when a chunk can no
    /// longer be pool-drawn (its CPU pair exceeds the slot strides): the caller also drops its
    /// `VoxelChunkGpu` so the raster loop's per-chunk CPU-mesh fallback takes over — the pool
    /// must never keep showing the stale pre-refusal geometry (the frozen-chunk border-spike
    /// class). `Queue::write_buffer` only — input-handler safe (Frame Ordering Contract).
    pub(crate) fn release_slot(&mut self, queue: &wgpu::Queue, slot: u32) {
        if slot >= self.pool_cap_slots { return; }
        if let Some(ind) = self.pool_indirect.as_ref() {
            queue.write_buffer(ind, slot as u64 * POOL_INDIRECT_STRIDE_BYTES, bytemuck::cast_slice(&[0u32; 5]));
        }
        if !self.pool_free.contains(&slot) { self.pool_free.push(slot); }
    }

    /// GATE 1 (surface mode): upload a CPU chunk mesh into the chunk's pool slot IN PLACE —
    /// `Queue::write_buffer` only, NO buffer creation (input-handler safe per the Frame Ordering
    /// Contract; the pool itself is allocated in poll via `ensure_pool`). `indices: Some` also
    /// rewrites the slot's index sub-range + its indirect `index_count` (topology sync —
    /// MANDATORY the first time a CPU pair lands over GPU-mesher slot content: the CPU mesher's
    /// vertex ORDER differs from the GPU mesher's, so a verts-only write over GPU-ordered indices
    /// would scramble the mesh). `indices: None` = the in-place dab hot path (topology unchanged
    /// by construction — surface strokes never change topology). Fails closed (`false`) when the
    /// pool is missing, the slot is out of range, or the data exceeds the slot strides.
    pub(crate) fn upload_cpu_chunk(
        &self,
        queue: &wgpu::Queue,
        slot: u32,
        verts: &[[f32; 8]],
        indices: Option<&[u32]>,
    ) -> bool {
        let (Some(pv), Some(pi), Some(pind)) =
            (self.pool_verts.as_ref(), self.pool_idx.as_ref(), self.pool_indirect.as_ref())
        else { return false };
        if slot >= self.pool_cap_slots { return false; }
        if verts.len() as u32 > self.vert_stride { return false; }
        if let Some(idx) = indices {
            if idx.len() as u32 > self.idx_stride { return false; }
        }
        if !verts.is_empty() {
            queue.write_buffer(pv, slot as u64 * self.vert_stride as u64 * 32, bytemuck::cast_slice(verts));
        }
        if let Some(idx) = indices {
            if !idx.is_empty() {
                queue.write_buffer(pi, slot as u64 * self.idx_stride as u64 * 4, bytemuck::cast_slice(idx));
            }
            // index_count = first u32 of the slot's DrawIndexedIndirect entry; the other 4 args
            // (instance_count=1, first_index, base_vertex, first_instance) are the static per-slot
            // values `ensure_chunk_gpu` seeded — an empty index list self-skips (count 0).
            queue.write_buffer(pind, slot as u64 * POOL_INDIRECT_STRIDE_BYTES, bytemuck::cast_slice(&[idx.len() as u32]));
        }
        true
    }

    /// Ensure chunk `ci`'s GPU-RESIDENT state exists + has a pool slot (Phase 6 + POOL). A chunk owns NO
    /// buffers — its vertex/index/indirect AND its counters/params storage are all SUB-RANGES of the shared
    /// pools selected by `pool_slot`. `cap_verts`/`cap_idx` = the chunk's OWN worst case (vspan³
    /// verts, acells·18 idx); they MUST NOT exceed the pool slot strides (the shader's `arrayLength` guard
    /// is bounded by the bound sub-range = the stride) — clamped here. Returns false (and creates nothing)
    /// if the pool is exhausted (`alloc_slot` → None): the caller leaves the chunk dirty + flags a grow.
    /// This creates ZERO buffers (only seeds the slot's static indirect args); every pool — including the
    /// counters/params pools — is allocated in `ensure_pool` (poll phase) per the Frame Ordering Contract.
    pub(crate) fn ensure_chunk_gpu(&mut self, slot: &mut Option<VoxelChunkGpu>, device: &wgpu::Device, queue: &wgpu::Queue, cap_verts: u32, cap_idx: u32) -> bool {
        // The slot sub-range bounds the shader writes; clamp the recorded caps so they can never exceed it.
        let cap_verts = cap_verts.min(self.vert_stride);
        let cap_idx = cap_idx.min(self.idx_stride);
        let need = match slot { Some(c) => c.cap_verts < cap_verts || c.cap_idx < cap_idx, None => true };
        if !need { return true; }
        // Reuse the existing slot on a cap grow; allocate a fresh one for a brand-new chunk.
        let pool_slot = match slot.as_ref() {
            Some(c) => c.pool_slot,
            None => match self.alloc_slot() { Some(s) => s, None => return false },
        };
        // Slot content changes provenance here (fresh slot OR cap-grow indirect re-seed below) →
        // new pool-monotonic content generation; any stored surface sync key is now stale.
        let generation = self.next_content_gen();
        // No per-chunk buffer creation: the chunk's counters + params now live in SUB-RANGES of the shared
        // `pool_counters` / `pool_params` (allocated in `ensure_pool`), selected by `pool_slot`. This kills
        // the last two per-chunk `create_buffer` calls — the ~36ms/frame allocation churn at 1024³ (64 new
        // chunks/frame × 2 buffers). The pool sub-ranges are bound in `dispatch_chunks_resident`.
        // Seed this slot's STATIC indirect args once: [index_count=0, instance_count=1,
        // first_index = slot*idx_stride, base_vertex = slot*vert_stride, first_instance=0]. Only index_count
        // changes per dispatch (copied from counters[1]); the rest are constant for the slot's lifetime.
        if let Some(ind) = self.pool_indirect.as_ref() {
            let args = [0u32, 1, pool_slot * self.idx_stride, pool_slot * self.vert_stride, 0];
            queue.write_buffer(ind, pool_slot as u64 * POOL_INDIRECT_STRIDE_BYTES, bytemuck::cast_slice(&args));
        }
        *slot = Some(VoxelChunkGpu {
            pool_slot, generation, cap_verts, cap_idx,
            bind_group: None, bg_res_gen: 0,
        });
        true
    }

    /// GPU-RESIDENT dispatch (Phase 6): mesh `ranges` straight into each chunk's POOL slot sub-ranges
    /// (`chunks[k]` ↔ `ranges[k]`), then copy each chunk's GPU index_count (`counters[1]`) into its
    /// `indirect` args — NO staging, NO map_async, NO CPU readback. The rasteriser draws each chunk via
    /// `draw_indexed_indirect`. Removes the ~5000 chunks/sec readback ceiling (the fast-stroke tearing).
    ///
    /// `src_gen` is the caller's generation counter for the `src` buffers (atlas index/pool OR the dense
    /// density buffer) — it MUST increment whenever the caller recreates them. The per-chunk bind groups
    /// are CACHED across frames and rebuilt only when a referenced SHARED buffer reallocated; `src_gen`
    /// (src buffers) and `self.mesh_res_gen` (the `res_cell_vert` scratch) together drive that invalidation.
    /// `chunks` is `&mut` because each dispatch may mutate the chunk's cached `bind_group` / `bg_res_gen`.
    pub(crate) fn dispatch_chunks_resident(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue, grid: &VoxelGrid, src: MeshDensitySource,
        ranges: &[([u32; 3], [u32; 3])], chunks: &mut [&mut VoxelChunkGpu], src_gen: u32,
    ) {
        if ranges.is_empty() { return; }
        // SLOT-CONTENT GENERATION BUMP (chunk-plate / needle display class): this dispatch REWRITES
        // every listed chunk's slot verts+indices+indirect in the GPU MESHER'S vertex order. Bump each
        // chunk's content generation FIRST (before any long-lived immutable borrows of self) so any
        // surface-mode sync key recorded against the pre-dispatch content mismatches — the next
        // surface write is then forced to re-upload TOPOLOGY instead of scrambling verts-only over
        // foreign order. Structural (generation counters), not a flag a call site must remember to clear.
        for k in 0..chunks.len() {
            let g = self.next_content_gen();
            chunks[k].generation = g;
        }
        // [VXMESH-GPU] Read back the PREVIOUS dispatch's mesh GPU time (one-frame latency), then record a
        // fresh measurement below. This is the number that tells us MESH-bound vs DRAW-bound at 1024³.
        if self.ts_pending && self.ts_map_ready.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(st) = self.ts_staging.as_ref() {
                let view = st.slice(..).get_mapped_range();
                let t: &[u64] = bytemuck::cast_slice(&view[..]);
                if t.len() >= 2 {
                    self.last_mesh_gpu_ms = (t[1].saturating_sub(t[0]) as f32) * self.ts_period / 1.0e6;
                }
                drop(view);
                st.unmap();
            }
            self.ts_pending = false;
            self.ts_map_ready.store(false, std::sync::atomic::Ordering::Release);
        }
        // Lazily create the 2-slot timestamp query set (begin of vertex pass, end of quads pass).
        if self.ts_query.is_none() && device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            self.ts_query = Some(device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("vmc_mesh_ts"), ty: wgpu::QueryType::Timestamp, count: 2 }));
            self.ts_resolve = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vmc_mesh_ts_resolve"),
                size: 16, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }));
            self.ts_staging = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("vmc_mesh_ts_staging"),
                size: 16, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false }));
            self.ts_period = queue.get_timestamp_period();
        }
        // Time this dispatch only if a prior readback isn't still in flight (staging mapped).
        let do_ts = self.ts_query.is_some() && !self.ts_pending;
        let _t_total = std::time::Instant::now(); // [VXMESH] diagnostic: time the dispatch breakdown
        let mut _bg_rebuilt = 0u32;
        // Persistent dummy for the inactive density source (created once, reused) so a cached BG binds a
        // STABLE buffer. A per-dispatch dummy would dangle in the cache or force a rebuild every frame.
        if self.res_dummy.is_none() {
            self.res_dummy = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vmc_res_dummy"), size: 4, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false }));
        }
        let dummy = self.res_dummy.as_ref().unwrap();
        let (density_buf, index_buf, pool_buf, brick_res): (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer, u32) = match src {
            MeshDensitySource::Dense(d) => (d, dummy, dummy, 0),
            MeshDensitySource::Atlas { index, pool, brick_res } => (dummy, index, pool, brick_res),
        };
        let wg = |n: u32| (n + 3) / 4;

        // --- SHARED SCRATCH (the resident-dispatch CPU win) -------------------------------------
        // Old code did, PER CHUNK: create_buffer(cell_vert, ~vcells*4 ≈ 20KB) + create_buffer(params,
        // 80B) + write_buffer(params). At ~100µs/chunk × 256 chunks that's the ~25ms that let the
        // dirty backlog outrun the drain. The scratch buffer is allocated ONCE per batch and reused.
        //
        // cell_vert: a per-cell scratch the vertex pass WRITES and the quads pass READS — the CPU
        // never reads it, and its contents need not survive past each chunk's own quads pass. So ONE
        // buffer sized to the batch's LARGEST chunk serves every chunk. SAFETY: reusing a single
        // cell_vert across all chunks in this single encoder is correct because, for each chunk, the
        // vertex pass (writes cell_vert) and the quads pass (reads it) are ADJACENT and wgpu inserts
        // an implicit write→read barrier between separate compute passes; compute passes execute in
        // submission order, so chunk k+1's vertex pass cannot begin before chunk k's quads pass has
        // consumed cell_vert. No aliasing hazard, no stale read. (Each chunk's quads pass overwrites
        // only the cells it touches; subsequent chunks overwrite via their own vertex pass first.)
        // Per-chunk vertex region + its BASE OFFSET into a cell_vert that packs the WHOLE batch (disjoint
        // slice [base .. base+vspan³) per chunk). This lets the dispatch run ONE vertex pass + ONE quads
        // pass for all chunks instead of two begin_compute_pass calls PER chunk — the ~0.08ms/chunk CPU
        // command-recording cost (measured) that made a big edit's surface appear "one square at a time".
        // The shader's local_idx adds the per-chunk base (params.a1.w). Disjoint slices + the single
        // write→read barrier between the two batched passes = no aliasing, no stale read.
        struct Layout { vmin: [u32; 3], vspan: [u32; 3], aspan: [u32; 3], base: u32 }
        let mut layout: Vec<Layout> = Vec::with_capacity(ranges.len());
        let mut total_cells: u64 = 0;
        for &(a0, a1) in ranges.iter() {
            let vmin = [a0[0].saturating_sub(1), a0[1].saturating_sub(1), a0[2].saturating_sub(1)];
            let vspan = [a1[0]-vmin[0]+1, a1[1]-vmin[1]+1, a1[2]-vmin[2]+1];
            let aspan = [a1[0]-a0[0]+1, a1[1]-a0[1]+1, a1[2]-a0[2]+1];
            layout.push(Layout { vmin, vspan, aspan, base: total_cells as u32 });
            total_cells += (vspan[0] as u64) * (vspan[1] as u64) * (vspan[2] as u64);
        }
        let cell_bytes = (total_cells * 4).max(4);
        if self.res_cell_vert.is_none() || self.res_cell_cap < cell_bytes {
            // Grow with headroom so a slightly larger later batch doesn't reallocate every frame.
            let cap = cell_bytes.max(self.res_cell_cap * 2).max(cell_bytes);
            self.res_cell_vert = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vmc_res_cellvert_scratch"), size: cap,
                usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false }));
            self.res_cell_cap = cap;
            // The shared scratch buffer (binding 2 in every cached BG) was reallocated → bump the shared
            // gen so EVERY chunk's cached BG is rebuilt this dispatch (the old buffer is now dangling).
            self.mesh_res_gen = self.mesh_res_gen.wrapping_add(1);
        }

        // Combined generation of the SHARED resources a cached BG references but doesn't own: the
        // res_cell_vert scratch (`mesh_res_gen`) + the verts/idx POOL (`pool_gen` — the slot sub-range
        // binding would otherwise dangle after a pool realloc) + the src buffers (`src_gen`). A chunk
        // rebuilds its BG when its stored `bg_res_gen` != this — i.e. when ANY shared buffer reallocated.
        // (Chunk-owned counters/params are invalidated separately: ensure_chunk_gpu sets bind_group=None.)
        let combined_gen = self.mesh_res_gen.wrapping_add(self.pool_gen).wrapping_add(src_gen);
        // Refs the BG build borrows immutably from self — taken before the loop so the per-chunk &mut on
        // `chunks` (disjoint from self) and these &self borrows don't conflict. The POOL must exist (the
        // caller sized it in poll before any chunk got a slot); a missing pool here is a programming error.
        let cell_vert = self.res_cell_vert.as_ref().unwrap();
        let bgl = &self.bgl;
        let pool_verts = self.pool_verts.as_ref().expect("pool_verts must be allocated before resident dispatch");
        let pool_idx = self.pool_idx.as_ref().expect("pool_idx must be allocated before resident dispatch");
        let pool_indirect = self.pool_indirect.as_ref().expect("pool_indirect must be allocated before resident dispatch");
        let pool_counters = self.pool_counters.as_ref().expect("pool_counters allocated before resident dispatch");
        let pool_params = self.pool_params.as_ref().expect("pool_params allocated before resident dispatch");
        let vert_stride = self.vert_stride;
        let idx_stride = self.idx_stride;
        // Timestamp writes: query 0 at the START of the vertex pass, query 1 at the END of the quads pass.
        // Their delta = GPU time for the whole batch's meshing (both passes + the barrier). None if disabled.
        let ts_vert = if do_ts { self.ts_query.as_ref().map(|qs| wgpu::ComputePassTimestampWrites {
            query_set: qs, beginning_of_pass_write_index: Some(0), end_of_pass_write_index: None }) } else { None };
        let ts_quad = if do_ts { self.ts_query.as_ref().map(|qs| wgpu::ComputePassTimestampWrites {
            query_set: qs, beginning_of_pass_write_index: None, end_of_pass_write_index: Some(1) }) } else { None };

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vmc_res_enc") });
        // (1) Per chunk: write params (a1.w = the chunk's cell_vert BASE offset) + zero counters + ensure
        // the cached bind group. binding 2 is the shared cell_vert (the SAME buffer for every chunk — each
        // chunk's slice is selected by the base offset in params.a1.w, not by a distinct binding).
        for (k, &(a0, a1)) in ranges.iter().enumerate() {
            let lo = &layout[k];
            let vc = &mut *chunks[k];
            let params = SnParams {
                a0: [a0[0],a0[1],a0[2], grid.res], a1: [a1[0],a1[1],a1[2], lo.base],
                vmin: [lo.vmin[0],lo.vmin[1],lo.vmin[2], 0], vspan: [lo.vspan[0],lo.vspan[1],lo.vspan[2], 0],
                misc: [grid.iso, grid.bounds, grid.voxel_size(), f32::from_bits(brick_res)],
            };
            queue.write_buffer(pool_params, vc.pool_slot as u64 * POOL_SLOT_META_BYTES, bytemuck::bytes_of(&params));
            queue.write_buffer(pool_counters, vc.pool_slot as u64 * POOL_SLOT_META_BYTES, bytemuck::cast_slice(&[0u32, 0u32]));
            if vc.bind_group.is_none() || vc.bg_res_gen != combined_gen {
                vc.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vmc_res_bg"), layout: bgl, entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: density_buf.as_entire_binding() },
                    // binding 1 (params): the chunk's slot as a 256B SUB-RANGE of the shared UNIFORM pool.
                    // Bound size 256 ≥ the 80B SnParams (matches the old per-chunk 256B params_buf); offset is
                    // slot*256 → always 256-aligned → satisfies min_uniform_buffer_offset_alignment.
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: pool_params,
                        offset: vc.pool_slot as u64 * POOL_SLOT_META_BYTES,
                        size: Some(std::num::NonZeroU64::new(POOL_SLOT_META_BYTES).unwrap()),
                    }) },
                    wgpu::BindGroupEntry { binding: 2, resource: cell_vert.as_entire_binding() },
                    // binding 3 (verts) / 4 (indices): the chunk's SLOT as a SUB-RANGE of the shared pool.
                    // The shader writes slot-local (vi*8, ii) exactly as before, and arrayLength(&verts) /
                    // arrayLength(&indices) return the bound SUB-RANGE size = this slot's cap → the existing
                    // overflow guards hold UNCHANGED. Offsets are 256B-aligned (vert_stride %8, idx_stride %64).
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: pool_verts,
                        offset: vc.pool_slot as u64 * vert_stride as u64 * 32,
                        size: Some(std::num::NonZeroU64::new(vert_stride as u64 * 32).unwrap()),
                    }) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: pool_idx,
                        offset: vc.pool_slot as u64 * idx_stride as u64 * 4,
                        size: Some(std::num::NonZeroU64::new(idx_stride as u64 * 4).unwrap()),
                    }) },
                    // binding 5 (counters): the chunk's slot as a 256B SUB-RANGE of the shared STORAGE pool.
                    // The shader's atomicAdd(&counters[0/1]) writes slot-local exactly as before; offset is
                    // slot*256 → 256-aligned → satisfies min_storage_buffer_offset_alignment.
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: pool_counters,
                        offset: vc.pool_slot as u64 * POOL_SLOT_META_BYTES,
                        size: Some(std::num::NonZeroU64::new(POOL_SLOT_META_BYTES).unwrap()),
                    }) },
                    wgpu::BindGroupEntry { binding: 6, resource: index_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: pool_buf.as_entire_binding() },
                ] }));
                vc.bg_res_gen = combined_gen;
                _bg_rebuilt += 1;
            }
        }
        // (2) ONE vertex pass over ALL chunks (set_pipeline once; per-chunk bind group + dispatch). Each
        // chunk writes its disjoint cell_vert slice and appends verts to its own buffer.
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vmc_res_vertex_batch"), timestamp_writes: ts_vert });
            p.set_pipeline(&self.pipe_vertex);
            for (k, lo) in layout.iter().enumerate() {
                p.set_bind_group(0, chunks[k].bind_group.as_ref().unwrap(), &[]);
                p.dispatch_workgroups(wg(lo.vspan[0]), wg(lo.vspan[1]), wg(lo.vspan[2]));
            }
        }
        // (3) The implicit write→read barrier between the two separate passes guarantees every chunk's
        // cell_vert slice is fully written before any quads read → ONE quads pass over ALL chunks.
        {
            let mut p = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vmc_res_quads_batch"), timestamp_writes: ts_quad });
            p.set_pipeline(&self.pipe_quads);
            for (k, lo) in layout.iter().enumerate() {
                p.set_bind_group(0, chunks[k].bind_group.as_ref().unwrap(), &[]);
                p.dispatch_workgroups(wg(lo.aspan[0]), wg(lo.aspan[1]), wg(lo.aspan[2]));
            }
        }
        // (4) Per chunk: GPU index_count (counters[1] @ byte 4) → the slot's indirect index_count (the
        // first u32 of its DrawIndexedIndirect entry at byte slot*20 in the SHARED pool_indirect). No
        // readback. The other 4 args (instance_count=1, first_index, base_vertex, first_instance) are static
        // per slot, seeded once in ensure_chunk_gpu — only index_count changes each dispatch.
        for vc in chunks.iter() {
            enc.copy_buffer_to_buffer(pool_counters, vc.pool_slot as u64 * POOL_SLOT_META_BYTES + 4,
                pool_indirect, vc.pool_slot as u64 * POOL_INDIRECT_STRIDE_BYTES, 4);
        }
        // (5) [VXMESH-GPU] Resolve the 2 mesh timestamps into staging (mapped after submit). DIAGNOSTIC only.
        if do_ts {
            let qs = self.ts_query.as_ref().unwrap();
            enc.resolve_query_set(qs, 0..2, self.ts_resolve.as_ref().unwrap(), 0);
            enc.copy_buffer_to_buffer(self.ts_resolve.as_ref().unwrap(), 0, self.ts_staging.as_ref().unwrap(), 0, 16);
        }
        // [VXMESH] diagnostic: split the dispatch CPU cost into encode-loop / finish(validate) / submit,
        // with the bind-group rebuild count, so we know whether the lag peak is CPU encoding, command-buffer
        // validation, or driver submit. Prints only on a SLOW dispatch (>20ms) — captures the peak frames
        // (the ~128ms brush-lag hitch) without spamming every frame. Remove once the throughput fix lands.
        let _encode_ms = _t_total.elapsed().as_secs_f32() * 1000.0;
        let _t_fin = std::time::Instant::now();
        let cmd = enc.finish();
        let _finish_ms = _t_fin.elapsed().as_secs_f32() * 1000.0;
        let _t_sub = std::time::Instant::now();
        queue.submit(std::iter::once(cmd));
        let _submit_ms = _t_sub.elapsed().as_secs_f32() * 1000.0;
        // [VXMESH-GPU] Kick the async readback of the timestamps resolved this submission. The map callback
        // flips ts_map_ready; the NEXT dispatch reads it (one-frame latency, like every other readback here).
        if do_ts {
            let ready = self.ts_map_ready.clone();
            ready.store(false, std::sync::atomic::Ordering::Release);
            self.ts_staging.as_ref().unwrap().slice(..).map_async(wgpu::MapMode::Read, move |r| {
                ready.store(r.is_ok(), std::sync::atomic::Ordering::Release);
            });
            self.ts_pending = true;
        }
        if _encode_ms + _finish_ms + _submit_ms > 20.0 {
            eprintln!("[VXMESH] chunks={} bg_rebuilt={} encode={:.1}ms finish={:.1}ms submit={:.1}ms (total {:.1}ms)",
                ranges.len(), _bg_rebuilt, _encode_ms, _finish_ms, _submit_ms, _encode_ms + _finish_ms + _submit_ms);
        }
    }

    /// Blocking mesh of many ranges — dispatch + wait + collect. Used by the parity check. For a Sparse
    /// grid it builds the brick ATLAS (index+pool) from the CPU slices and meshes via that (exercising
    /// the exact GPU atlas read path); for Dense it uploads a one-shot res³ buffer.
    pub(crate) fn mesh_chunks(&self, device: &wgpu::Device, queue: &wgpu::Queue, grid: &VoxelGrid, ranges: &[([u32; 3], [u32; 3])]) -> Vec<(Vec<[f32; 8]>, Vec<u32>)> {
        if ranges.is_empty() { return Vec::new(); }
        let inflight = match (grid.density.index_slice(), grid.density.bricks_slice()) {
            (Some(index), Some(pool)) => {
                // Sparse → brick atlas.
                let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vmc_index_oneshot"), size: ((index.len() * 4) as u64).max(4),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                queue.write_buffer(&index_buf, 0, bytemuck::cast_slice(index));
                let pool_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vmc_pool_oneshot"), size: ((pool.len() * 4) as u64).max(4),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                queue.write_buffer(&pool_buf, 0, bytemuck::cast_slice(pool));
                let src = MeshDensitySource::Atlas { index: &index_buf, pool: &pool_buf, brick_res: grid.density.brick_res() };
                let infl = self.dispatch_chunks(device, queue, grid, src, ranges);
                device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
                return if infl.map_state() == VoxelMeshMapState::Ready {
                    infl.collect()
                } else {
                    eprintln!("[VXGPU] one-shot sparse mesh readback failed");
                    Vec::new()
                };
            }
            _ => {
                // Dense → one-shot res³ buffer.
                let density_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vmc_density_oneshot"), size: ((grid.density.len() * 4) as u64).max(4),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                queue.write_buffer(&density_buf, 0, bytemuck::cast_slice(&grid.density.to_dense()));
                self.dispatch_chunks(device, queue, grid, MeshDensitySource::Dense(&density_buf), ranges)
            }
        };
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed"); // parity/debug path only; GPU done + callbacks fired
        if inflight.map_state() == VoxelMeshMapState::Ready {
            inflight.collect()
        } else {
            eprintln!("[VXGPU] one-shot dense mesh readback failed");
            Vec::new()
        }
    }

    /// Mesh a single anchor range — thin wrapper over `mesh_chunks` (used by the parity check).
    pub(crate) fn mesh_region(&self, device: &wgpu::Device, queue: &wgpu::Queue, grid: &VoxelGrid, a0: [u32; 3], a1: [u32; 3]) -> (Vec<[f32; 8]>, Vec<u32>) {
        self.mesh_chunks(device, queue, grid, &[(a0, a1)]).pop().unwrap_or_default()
    }
}

/// Per-chunk readback staging buffers (the mapped copy targets) + their capacities.
struct ChunkStaging {
    s_counters: wgpu::Buffer, s_verts: wgpu::Buffer, s_idx: wgpu::Buffer,
    vert_cap: u64, idx_cap: u64,
}

/// An in-flight GPU mesh batch: the staging buffers are filling + mapping asynchronously. Poll the
/// device each frame; when `map_state()` is Ready, `collect()` reads the results (one per chunk). The
/// shared density buffer is owned by `GpuCtx` (persistent, Phase 3c) so it stays alive across the
/// in-flight period; the submitted command buffer also holds an internal reference until the GPU finishes.
pub(crate) struct VoxelMeshInflight {
    jobs: Vec<ChunkStaging>,
    completed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    failed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    want: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoxelMeshMapState {
    Pending,
    Ready,
    Failed,
}

#[inline]
pub(crate) fn voxel_mesh_map_state(completed: usize, failed: usize, want: usize) -> VoxelMeshMapState {
    if failed > 0 {
        VoxelMeshMapState::Failed
    } else if completed >= want {
        VoxelMeshMapState::Ready
    } else {
        VoxelMeshMapState::Pending
    }
}

impl VoxelMeshInflight {
    /// Explicit callback state, advanced by `device.poll(Poll)` without a frame-count timeout.
    pub(crate) fn map_state(&self) -> VoxelMeshMapState {
        let completed = self.completed.load(std::sync::atomic::Ordering::Acquire);
        let failed = self.failed.load(std::sync::atomic::Ordering::Acquire);
        voxel_mesh_map_state(completed, failed, self.want)
    }
    /// Read all mapped staging buffers → (vertices, indices) per chunk. Call only when Ready.
    pub(crate) fn collect(self) -> Vec<(Vec<[f32; 8]>, Vec<u32>)> {
        let mut out = Vec::with_capacity(self.jobs.len());
        for j in &self.jobs {
            let cnt: Vec<u32> = bytemuck::cast_slice(&j.s_counters.slice(..).get_mapped_range()).to_vec();
            let vcount = (cnt[0] as u64).min(j.vert_cap) as usize;
            let icount = (cnt[1] as u64).min(j.idx_cap) as usize;
            let vflat: Vec<f32> = bytemuck::cast_slice(&j.s_verts.slice(..).get_mapped_range()).to_vec();
            let iflat: Vec<u32> = bytemuck::cast_slice(&j.s_idx.slice(..).get_mapped_range()).to_vec();
            j.s_counters.unmap(); j.s_verts.unmap(); j.s_idx.unmap();
            let mut verts = Vec::with_capacity(vcount);
            for i in 0..vcount {
                let o = i * 8;
                verts.push([vflat[o],vflat[o+1],vflat[o+2],vflat[o+3],vflat[o+4],vflat[o+5],vflat[o+6],vflat[o+7]]);
            }
            out.push((verts, iflat[..icount].to_vec()));
        }
        out
    }
}

#[cfg(test)]
mod voxel_mesh_map_state_tests {
    // Brush helpers live in the sibling `upload` module; resolve via the facade so this test
    // module (now nested under mesh_build) keeps the same assertions as the pre-split file.
    use super::{voxel_mesh_map_state, VoxelMeshMapState};
    use crate::voxel_gpu_mesh::{
        gpu_mask_is_active, voxel_brush_map_state, VoxelBrushMapState,
    };

    #[test]
    fn pending_ready_and_failed_are_distinct_without_frame_timeouts() {
        assert_eq!(voxel_mesh_map_state(0, 0, 3), VoxelMeshMapState::Pending);
        assert_eq!(voxel_mesh_map_state(2, 0, 3), VoxelMeshMapState::Pending);
        assert_eq!(voxel_mesh_map_state(3, 0, 3), VoxelMeshMapState::Ready);
        assert_eq!(voxel_mesh_map_state(1, 1, 3), VoxelMeshMapState::Failed);
        assert_eq!(voxel_mesh_map_state(3, 1, 3), VoxelMeshMapState::Failed);
    }

    #[test]
    fn brush_readback_failure_is_settled_not_pending() {
        assert_eq!(
            voxel_brush_map_state(crate::map_state::PENDING),
            VoxelBrushMapState::Pending,
        );
        assert_eq!(
            voxel_brush_map_state(crate::map_state::OK),
            VoxelBrushMapState::Ready,
        );
        assert_eq!(
            voxel_brush_map_state(crate::map_state::ERR),
            VoxelBrushMapState::Failed,
        );
    }

    #[test]
    fn zero_or_missing_mask_uses_the_dummy_binding() {
        assert!(!gpu_mask_is_active(&[], 8));
        assert!(!gpu_mask_is_active(&[0.0; 8], 8));
        assert!(!gpu_mask_is_active(&[1.0], 8));
        assert!(gpu_mask_is_active(&[0.0, 0.25, 0.0], 3));
    }
}



/// Max indices `sn_quads` (voxel_mesh.wgsl) can emit per ANCHOR cell: 3 faces (+X/+Y/+Z) × 2 triangles
/// × 3 indices = 18. The per-chunk index buffer is sized `acells * this` so the GPU atomic index counter
/// can't exceed it in the no-drift case. If sn_quads's emission ever changes, update this in lockstep
/// (the shader has a matching comment). Safety against an OOB indirect draw is ALSO guaranteed
/// independently by the `atomicMin(&counters[1], arrayLength(&indices))` clamp at the end of sn_quads —
/// this const only keeps the buffer SIZE matching the emission so a future change drops no triangles.
pub(crate) const SN_MAX_IDX_PER_ANCHOR_CELL: u32 = 18;

/// POOL slot sizing (the shared vertex/index/indirect pool — the 1024³ draw-bound fix). A slot is one
/// chunk's CAPACITY at `VOXEL_CHUNK = 16` cells/axis.
///
/// These are a generous *capacity bound*, NOT the absolute checkerboard worst case (vspan³ = 17³ = 4913
/// verts, 16³·18 = 73728 idx). Why not the worst case: the pool is ONE buffer per resource, and at 1024³
/// (~12k surface chunks) worst-case strides make the index pool a single ~3.5GB buffer — a single GPU
/// allocation that large fails on real drivers even with VRAM free (the pre-pool code worked precisely
/// because it used 12k *small* per-chunk buffers). The absolute worst case is also unreachable by BRUSH
/// sculpting: a smooth/curved surface through a 16³ chunk emits a few hundred verts; these bounds give
/// 3–8× margin. If a (procedural/imported) density field ever exceeds a slot, the shader's `arrayLength`
/// guard (bounded by the bound sub-range = the stride) drops the overflow as a small hole — never an OOB
/// write or device loss. The no-compromise path that restores the absolute worst-case guarantee is to
/// TILE the pool into many sub-GB buffers (one multi-draw per tile) — tracked as a follow-up.
///
/// Both are chosen so the 256B `min_storage_buffer_offset_alignment` holds: 2048*32 = 65536 (%256==0)
/// and 16384*4 = 65536 (%256==0); `VoxelMeshGpu::new` rounds vert→mult-of-8, idx→mult-of-64 (both no-ops
/// here). At 12k slots the pool is ~786MB verts + ~786MB idx — each well under the 1GB per-buffer cap in
/// `pool_byte_layout`, so allocation is reliable.
pub(crate) const POOL_VERTS_PER_SLOT: u32 = 2048;
pub(crate) const POOL_IDX_PER_SLOT: u32 = 16384;

/// Surface-mode pool sync key: `(pool_slot, slot content generation, pool generation)`.
/// Stored per chunk in `surface_pool_synced` at every successful surface upload; compared against
/// the chunk's CURRENT key (`VoxelMeshGpu::slot_sync_key`) before any verts-only upload.
pub(crate) type PoolSlotSyncKey = (u32, u32, u32);

/// THE verts-only gate (pure, unit-tested — the GPU consistency test drives this exact function):
/// a verts-only in-place upload is allowed ONLY when the slot provably still holds THIS chunk's
/// last surface-uploaded topology — same slot, same content generation (no GPU-mesher rewrite, no
/// release/re-acquire, no cap-grow re-seed), same pool buffers (no realloc) and no CPU topology
/// change. Anything else ⇒ the caller MUST upload topology too. This keys the sync to
/// (chunk, slot, generation) structurally — the chunk-plate / needle "verts over foreign vertex
/// order" display class cannot be reintroduced by a forgotten flag-clear call site.
pub(crate) fn pool_verts_only_allowed(
    stored: Option<PoolSlotSyncKey>,
    current: PoolSlotSyncKey,
    cpu_topology_changed: bool,
) -> bool {
    !cpu_topology_changed && stored == Some(current)
}
/// Bytes per DrawIndexedIndirect entry: 5 × u32 = 20.
pub(crate) const POOL_INDIRECT_STRIDE_BYTES: u64 = 20;
/// Per-slot stride for the pooled counters + params buffers. 256 bytes: counters uses the first 8
/// (2×u32), params holds one ≤256B SnParams. 256 satisfies BOTH min_storage_buffer_offset_alignment
/// (counters sub-range) AND min_uniform_buffer_offset_alignment (params sub-range) on all targets.
pub(crate) const POOL_SLOT_META_BYTES: u64 = 256;
/// Hard cap on a single pool buffer (1 GiB). Devices report `max_buffer_size` far above what a single
/// allocation can reliably grant; capping here keeps the create from failing (the failure that returns a
/// poisoned buffer → `InvalidBufferId` in every bind group). 1GiB / ~64KB per slot ≈ 16k slots, above the
/// ~12k surface chunks at 1024³. Beyond this, excess chunks fall back to the CPU mesh path (graceful).
pub(crate) const POOL_MAX_BUFFER_BYTES: u64 = 1 << 30;



/// Order-independent vertex-position parity: the MAX over every `a` vertex of its nearest-neighbour
/// distance to the `b` set. Vertex emission ORDER differs (GPU atomic append) AND GPU/CPU f32 math
/// rounds differently (~1e-5), so exact set-equality is the wrong test — nearest-neighbour distance
/// is robust (no rounding-boundary flips). `cell` buckets `b` (≈ voxel_size) so the search is the 27
/// neighbouring buckets. Returns ~1e-5 when the meshes agree (FP noise), large if topology differs.
pub(crate) fn max_nn_dist(a: &[[f32; 8]], b: &[[f32; 8]], cell: f32) -> f32 {
    use std::collections::HashMap;
    let cell = cell.max(1e-6);
    let key = |p: &[f32; 8]| (
        (p[0] / cell).floor() as i64, (p[1] / cell).floor() as i64, (p[2] / cell).floor() as i64,
    );
    let mut grid: HashMap<(i64, i64, i64), Vec<[f32; 3]>> = HashMap::new();
    for v in b { grid.entry(key(v)).or_default().push([v[0], v[1], v[2]]); }
    let mut max_d = 0.0f32;
    for v in a {
        let (kx, ky, kz) = key(v);
        let mut best = f32::INFINITY;
        for dz in -1..=1 { for dy in -1..=1 { for dx in -1..=1 {
            if let Some(bucket) = grid.get(&(kx + dx, ky + dy, kz + dz)) {
                for q in bucket {
                    let d = ((v[0]-q[0]).powi(2) + (v[1]-q[1]).powi(2) + (v[2]-q[2]).powi(2)).sqrt();
                    if d < best { best = d; }
                }
            }
        }}}
        if best > max_d { max_d = best; }
    }
    max_d
}

// ============================================================================
// GPU POOL <-> CPU CONSISTENCY TEST (real device, real pool, production APIs).
//
// The systemic gate for the surface-sculpt display-desync family (needle spikes
// 4dc3eea, chunk plates 2026-07-23): after every step of the REAL slot
// lifecycle — GPU-mesher populate (the real `dispatch_chunks_resident`, real
// foreign vertex order), surface first-write with topology, verts-only
// follow-ups, GPU-mesher repopulation, release + re-acquire by another chunk —
// the pool's verts/idx/indirect are READ BACK and byte-compared against the
// CPU-side expectation, and the verts-only gate (`pool_verts_only_allowed`,
// the exact function production calls) is asserted to refuse every foreign-
// content state. The readbacks target the SAME buffers + sub-range offsets the
// production multi-draw binds (`pool_verts`/`pool_idx`/`pool_indirect` — the
// lab-cache lesson: a harness must mirror production wiring or it stays green
// while production corrupts).
//
// Skips gracefully when no GPU adapter is present (headless CI) — the pure
// `pool_verts_only_allowed` unit tests below are the always-on gate.
// ============================================================================
#[cfg(test)]
mod pool_consistency_gpu {
    use super::*;

    #[test]
    fn verts_only_gate_pure_properties() {
        let k1: PoolSlotSyncKey = (3, 7, 1);
        let k2: PoolSlotSyncKey = (3, 8, 1); // content generation moved (GPU-mesher rewrite / re-seed)
        let k3: PoolSlotSyncKey = (3, 7, 2); // pool realloc
        let k4: PoolSlotSyncKey = (4, 7, 1); // different slot
        assert!(pool_verts_only_allowed(Some(k1), k1, false), "matching key must allow verts-only");
        assert!(!pool_verts_only_allowed(None, k1, false), "first write must upload topology");
        assert!(!pool_verts_only_allowed(Some(k1), k2, false), "content-generation mismatch must force topology");
        assert!(!pool_verts_only_allowed(Some(k1), k3, false), "pool-generation mismatch must force topology");
        assert!(!pool_verts_only_allowed(Some(k1), k4, false), "slot mismatch must force topology");
        assert!(!pool_verts_only_allowed(Some(k1), k1, true), "CPU topology change must force topology");
    }

    #[test]
    fn pool_partition_is_disjoint_per_chunk() {
        // The G-buffer / shadow / pick draw lists all classify through this ONE predicate:
        // slot present -> pool multi-draw ONLY; absent -> CPU fallback ONLY. Disjoint by definition
        // for every slot state; resident_base=false forces the whole body to the CPU set.
        let slots: Vec<Option<VoxelChunkGpu>> = vec![
            None,
            Some(VoxelChunkGpu { pool_slot: 0, generation: 1, cap_verts: 8, cap_idx: 8, bind_group: None, bg_res_gen: 0 }),
        ];
        for (i, s) in slots.iter().enumerate() {
            let pool = crate::raster_common::voxel_chunk_pool_partition(&slots, true, i);
            assert_eq!(pool, s.is_some());
            assert!(!crate::raster_common::voxel_chunk_pool_partition(&slots, false, i));
        }
        // Out-of-range chunk index is never pool-drawn.
        assert!(!crate::raster_common::voxel_chunk_pool_partition(&slots, true, 99));
    }

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
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("vxpool_consistency_device"),
                required_features: wgpu::Features::empty(),
                // The mesher BGL needs 7 storage bindings per stage — downlevel_defaults caps at 4.
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        )).ok()?;
        Some((device, queue))
    }

    /// Blocking readback of `len` bytes at `off` from `buf` — maps a staging copy.
    fn readback(device: &wgpu::Device, queue: &wgpu::Queue, buf: &wgpu::Buffer, off: u64, len: u64) -> Vec<u8> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vxpool_test_staging"), size: len.max(4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(buf, off, &staging, 0, len);
        queue.submit(std::iter::once(enc.finish()));
        // GPU Safety rule: capture the map result via a channel, never discard it.
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..len).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        rx.recv().expect("map_async callback dropped").expect("readback map failed");
        let data = staging.slice(..len).get_mapped_range().to_vec();
        staging.unmap();
        data
    }

    /// The 5 u32 indirect args of `slot`.
    fn read_indirect(device: &wgpu::Device, queue: &wgpu::Queue, m: &VoxelMeshGpu, slot: u32) -> [u32; 5] {
        let b = readback(device, queue, m.pool_indirect.as_ref().unwrap(),
            slot as u64 * POOL_INDIRECT_STRIDE_BYTES, 20);
        let v: &[u32] = bytemuck::cast_slice(&b);
        [v[0], v[1], v[2], v[3], v[4]]
    }

    fn read_slot_verts(device: &wgpu::Device, queue: &wgpu::Queue, m: &VoxelMeshGpu, slot: u32, n: usize) -> Vec<u8> {
        readback(device, queue, m.pool_verts.as_ref().unwrap(),
            slot as u64 * m.vert_stride as u64 * 32, (n * 32) as u64)
    }

    fn read_slot_idx(device: &wgpu::Device, queue: &wgpu::Queue, m: &VoxelMeshGpu, slot: u32, n: usize) -> Vec<u8> {
        readback(device, queue, m.pool_idx.as_ref().unwrap(),
            slot as u64 * m.idx_stride as u64 * 4, (n * 4) as u64)
    }

    /// Assert the slot's live bytes equal the CPU pair, and the indirect entry draws exactly it.
    fn assert_slot_matches_cpu(
        device: &wgpu::Device, queue: &wgpu::Queue, m: &VoxelMeshGpu,
        slot: u32, verts: &[[f32; 8]], idx: &[u32], what: &str,
    ) {
        assert_eq!(read_slot_verts(device, queue, m, slot, verts.len()),
            bytemuck::cast_slice::<_, u8>(verts).to_vec(), "{what}: slot verts != CPU verts");
        assert_eq!(read_slot_idx(device, queue, m, slot, idx.len()),
            bytemuck::cast_slice::<_, u8>(idx).to_vec(), "{what}: slot idx != CPU idx");
        let ind = read_indirect(device, queue, m, slot);
        assert_eq!(ind, [idx.len() as u32, 1, slot * m.idx_stride, slot * m.vert_stride, 0],
            "{what}: indirect args wrong");
    }

    #[test]
    fn pool_cpu_consistency_over_slot_lifecycle() {
        let Some((device, queue)) = try_device() else {
            eprintln!("[VXPOOL-TEST] no GPU adapter — skipped (pure gate tests still ran)");
            return;
        };
        // Small dense grid with an OFF-CENTER sphere: asymmetric surface -> the GPU mesher's
        // emission order genuinely differs from the CPU mesher's (the parity check compares
        // vertex SETS for exactly this reason) — the foreign-order property under test is real.
        let mut grid = VoxelGrid::new_empty(40, 1.0);
        grid.fill_sphere(glam::Vec3::new(0.06, 0.04, -0.05), 0.5);
        let res = grid.res;
        let chunk_dim = crate::input_sculpt::voxel_chunk_dim(res);

        // Two surface chunks whose CPU pairs are non-empty and fit a pool slot.
        let mut picks: Vec<(u32, ([u32; 3], [u32; 3]), Vec<[f32; 8]>, Vec<u32>)> = Vec::new();
        for ci in 0..(chunk_dim * chunk_dim * chunk_dim) {
            let (a0, a1) = crate::input_sculpt::voxel_chunk_anchor(ci, chunk_dim, res);
            let (v, i) = grid.mesh_region(a0, a1);
            if !v.is_empty() && v.len() as u32 <= POOL_VERTS_PER_SLOT && i.len() as u32 <= POOL_IDX_PER_SLOT {
                picks.push((ci, (a0, a1), v, i));
                if picks.len() == 2 { break; }
            }
        }
        assert!(picks.len() == 2, "test grid must yield 2 pool-fitting surface chunks");
        let (_ci_a, (a0, a1), cpu_verts_a, cpu_idx_a) = picks.remove(0);
        let (_ci_b, (b0, b1), cpu_verts_b, cpu_idx_b) = picks.remove(0);

        let mut mesher = VoxelMeshGpu::new(&device);
        mesher.ensure_pool(&device, &queue, 4);

        // Dense density source — the same upload shape production uses (`voxel_mesh_density`).
        let dense = grid.density.to_dense();
        let dbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vxpool_test_density"), size: (dense.len() as u64 * 4).max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&dbuf, 0, bytemuck::cast_slice(&dense));

        // ---- (a) REAL GPU-mesher populate of chunk A's slot ------------------------------------
        let vspan = [a1[0] - a0[0].saturating_sub(1) + 1, a1[1] - a0[1].saturating_sub(1) + 1, a1[2] - a0[2].saturating_sub(1) + 1];
        let acells = (a1[0] - a0[0] + 1) * (a1[1] - a0[1] + 1) * (a1[2] - a0[2] + 1);
        let mut slot_a: Option<VoxelChunkGpu> = None;
        assert!(mesher.ensure_chunk_gpu(&mut slot_a, &device, &queue,
            vspan[0] * vspan[1] * vspan[2], acells * SN_MAX_IDX_PER_ANCHOR_CELL));
        let gen_after_ensure = slot_a.as_ref().unwrap().generation;
        {
            let mut refs: Vec<&mut VoxelChunkGpu> = vec![slot_a.as_mut().unwrap()];
            mesher.dispatch_chunks_resident(&device, &queue, &grid,
                MeshDensitySource::Dense(&dbuf), &[(a0, a1)], &mut refs, 1);
        }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        assert!(slot_a.as_ref().unwrap().generation != gen_after_ensure,
            "dispatch_chunks_resident must bump the slot content generation (the sync-key invalidation)");
        let slot_id = slot_a.as_ref().unwrap().pool_slot;
        let gpu_ind = read_indirect(&device, &queue, &mesher, slot_id);
        assert!(gpu_ind[0] > 0, "GPU mesher produced an empty chunk — pick a surface chunk");
        // FOREIGN-ORDER property: the GPU pair is a different byte sequence than the CPU pair
        // (same surface, different emission order) — the exact state a verts-only upload would
        // scramble. This is what makes steps (b)/(d) meaningful.
        let n_probe = cpu_verts_a.len().max(1);
        let gpu_verts_prefix = read_slot_verts(&device, &queue, &mesher, slot_id, n_probe);
        assert_ne!(gpu_verts_prefix, bytemuck::cast_slice::<_, u8>(&cpu_verts_a[..n_probe]).to_vec(),
            "GPU and CPU vertex orders coincide — test scene must make them differ");

        // ---- (b) surface FIRST WRITE: the gate must demand topology; readback == CPU pair ------
        let mut stored: Option<PoolSlotSyncKey> = None; // fresh surface session
        let key1 = mesher.slot_sync_key(slot_a.as_ref().unwrap());
        assert!(!pool_verts_only_allowed(stored, key1, false), "first write over GPU content must upload topology");
        assert!(mesher.upload_cpu_chunk(&queue, slot_id, &cpu_verts_a, Some(&cpu_idx_a)));
        stored = Some(key1);
        assert_slot_matches_cpu(&device, &queue, &mesher, slot_id, &cpu_verts_a, &cpu_idx_a, "(b) first write");

        // ---- (c) verts-only follow-ups (the dab hot path) --------------------------------------
        let mut moved = cpu_verts_a.clone();
        for v in moved.iter_mut() { v[1] += 0.011; }
        let key2 = mesher.slot_sync_key(slot_a.as_ref().unwrap());
        assert!(pool_verts_only_allowed(stored, key2, false), "same key must allow the verts-only hot path");
        assert!(mesher.upload_cpu_chunk(&queue, slot_id, &moved, None));
        assert_slot_matches_cpu(&device, &queue, &mesher, slot_id, &moved, &cpu_idx_a, "(c) verts-only");

        // ---- (d) GPU-mesher REPOPULATION mid-session — today's bug class -----------------------
        // (mode-cycle / leftover dirty mark / any future sequence: the mesher rewrites the slot in
        // its own order; the OLD sync key must now refuse verts-only, forcing a topology re-sync.)
        {
            let mut refs: Vec<&mut VoxelChunkGpu> = vec![slot_a.as_mut().unwrap()];
            mesher.dispatch_chunks_resident(&device, &queue, &grid,
                MeshDensitySource::Dense(&dbuf), &[(a0, a1)], &mut refs, 1);
        }
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        let key3 = mesher.slot_sync_key(slot_a.as_ref().unwrap());
        assert_ne!(Some(key3), stored, "GPU repopulation must change the sync key");
        assert!(!pool_verts_only_allowed(stored, key3, false),
            "REGRESSION (chunk-plate class): verts-only over GPU-repopulated slot must be refused");
        assert!(mesher.upload_cpu_chunk(&queue, slot_id, &cpu_verts_a, Some(&cpu_idx_a)));
        stored = Some(key3);
        assert_slot_matches_cpu(&device, &queue, &mesher, slot_id, &cpu_verts_a, &cpu_idx_a, "(d) re-sync");

        // ---- (e) release + re-acquire by ANOTHER chunk (freed-slot reuse, no ABA) --------------
        let released = slot_a.take().unwrap();
        mesher.release_slot(&queue, released.pool_slot);
        assert_eq!(read_indirect(&device, &queue, &mesher, slot_id), [0; 5],
            "released slot's indirect must be zeroed (multi-draw self-skip) before any re-acquire");
        let mut slot_b: Option<VoxelChunkGpu> = None;
        let bspan = [b1[0] - b0[0].saturating_sub(1) + 1, b1[1] - b0[1].saturating_sub(1) + 1, b1[2] - b0[2].saturating_sub(1) + 1];
        let bcells = (b1[0] - b0[0] + 1) * (b1[1] - b0[1] + 1) * (b1[2] - b0[2] + 1);
        assert!(mesher.ensure_chunk_gpu(&mut slot_b, &device, &queue,
            bspan[0] * bspan[1] * bspan[2], bcells * SN_MAX_IDX_PER_ANCHOR_CELL));
        assert_eq!(slot_b.as_ref().unwrap().pool_slot, slot_id, "freed slot must be reused (pool_free)");
        let key_b = mesher.slot_sync_key(slot_b.as_ref().unwrap());
        assert!(!pool_verts_only_allowed(stored, key_b, false),
            "chunk A's stale key must not authorize verts-only into the slot chunk B now owns (no ABA)");
        assert!(mesher.upload_cpu_chunk(&queue, slot_id, &cpu_verts_b, Some(&cpu_idx_b)));
        assert_slot_matches_cpu(&device, &queue, &mesher, slot_id, &cpu_verts_b, &cpu_idx_b, "(e) re-acquire");

        // ---- (f) mode-cycle transition: entry/exit clears the map -> first write re-syncs ------
        let stored_after_mode_cycle: Option<PoolSlotSyncKey> = None; // surface_pool_synced.clear()
        assert!(!pool_verts_only_allowed(stored_after_mode_cycle, key_b, false),
            "after a mode cycle the first write must upload topology again");

        // ---- (g) ORPHAN SWEEP: every live (nonzero) indirect entry belongs to exactly one owned
        // slot — a nonzero orphan would be pool-drawn geometry no chunk owns (the floating-plate
        // display class). Ownership table mirrors `GpuCtx.voxel_chunk_gpu`.
        let owners: Vec<Option<VoxelChunkGpu>> = vec![None /* chunk A released */, slot_b];
        let owned: std::collections::HashSet<u32> =
            owners.iter().flatten().map(|c| c.pool_slot).collect();
        assert_eq!(owned.len(), owners.iter().flatten().count(), "two chunks must never share a slot");
        for s in 0..mesher.pool_cap_slots() {
            let ind = read_indirect(&device, &queue, &mesher, s);
            if ind[0] > 0 {
                assert!(owned.contains(&s),
                    "orphan live slot {s}: nonzero index_count with no owning chunk (would draw a stale plate)");
            }
        }
    }
}
