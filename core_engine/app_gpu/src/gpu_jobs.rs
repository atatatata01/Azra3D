//! GPU job dispatch infrastructure — worker threads, job queue, result polling.
//!
//! Architecture:
//! - One persistent `std::thread` per worker GPU, blocking on an inbound channel.
//! - All workers share a single outbound result channel.
//! - `GpuJobDispatcher` owns all channels. `AzraApp` owns the dispatcher.
//! - `GpuJob` is a pure description (kind + generation + target). No device/queue.
//! - `WorkerState` is private to the worker thread (pipeline, buffers, device).

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use crossbeam_channel::{Sender, Receiver};
use eframe::wgpu;
use core_math::scene_graph::NodeId;

// ── Bake Output Mode ────────────────────────────────────────────────

/// How a volume bake delivers its result to the rendering pipeline.
/// Chosen at dispatch time based on resolution, device role, and GPU capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BakeOutputMode {
    /// Full pipeline: shader writes to flat buffer AND texture.
    /// CPU readback via staging buffer populates body_grids cache.
    /// Constraint: resolution³ × 4 must fit in max_storage_buffer_binding_size — max 512³ now
    /// (see max_gridbacked_resolution below); larger resolutions fall to TextureOnly.
    GridBacked = 0,

    /// Texture-only: shader writes ONLY to the 3D texture (binding 3).
    /// No flat buffer, no staging buffer, no CPU readback, no grid cache.
    /// Constraint: primary render device only (workers cannot write to main GPU texture).
    TextureOnly = 1,

    /// Sparse bricks (Phase 3A): shader writes 8³ signed-distance bricks into a pool
    /// arena by handle. Binding 5 (`brick_index`) maps body-space brick coords →
    /// pool handle (sentinels EXTERIOR 0xFFFFFFFF / INTERIOR 0xFFFFFFFE are skipped);
    /// binding 2 (`output_grid`) is REBOUND to the pool buffer at dispatch. No flat
    /// grid write, no texture store. UNREACHABLE dead mode until the sparse dispatch
    /// slice lands — `select_bake_output_mode` never returns it.
    /// (Not serialized — only cast `as u32` into the BakeParams GPU uniform, so
    /// appending at the END is safe.)
    SparseBricks = 2,
}

/// Max resolution where a dense grid³ × 4 field fits in max_storage_buffer_binding_size, so the body
/// bakes GridBacked (dense flat buffer → DC triangle mesh in Object Mode) rather than TextureOnly.
/// 512³ × 4 = 512MB, well under the adapter-max binding size the app requests (2.1GB on the RTX 4090).
/// The DC vertex/index OUTPUT at 512³ is surface-capped in SdfBodyMeshSlot::new (Phase 3B step 2), so it
/// no longer hits the ~7GB dense-output wall. This is the DENSE ceiling and stays at 512 FOREVER:
/// a dense 1024³ field is 4GB — over the binding limit; resolutions above this are exactly what the
/// sparse SDF brick atlas (Phase 3A, `max_sparse_resolution`) exists for. Dense 512³ uses ~0.5GB
/// field + ~0.5GB cell_vert + capped output per body; fine on a 24GB GPU, heavy for many bodies.
pub const fn max_gridbacked_resolution() -> u32 { 512 }

/// Max resolution for the SPARSE (SparseBricks mode-2) Object-Mode pipeline: GPU classify →
/// sparse brick-pool bake → windowed sparse DC mesh. Every buffer on this path is
/// surface-proportional (brick index = (res/8)³ × 4B = 8MB at 1024; pool / cell_vert / verts /
/// idx scale with ALLOCATED bricks ≈ 4× a 512 bake of the same scene), so no dense res³
/// allocation exists anywhere — 1024 is safe where dense 1024 is impossible. The bake dispatch
/// still sweeps res³ threads but is Z-slab sliced (one submit per 64-voxel slab — the TDR
/// guard in dispatch_pending_primary_bakes). Applies ONLY when the sparse regime is active
/// (`BodyVolumeCache::max_mesh_resolution`); never a dense or TextureOnly admission.
///
/// 1024 → 2048 (2026-07-17, 2048-SHIP item 4 — LAST, after items 1-3): the classify-finalize
/// 2D dispatch grid (the 65535-workgroup break at 256³ bricks), the worker-side index
/// validate (~45ms at 16.7M entries), and the binding-limit growth clamps
/// (`pool_ceiling_bricks` shared by the dispatch self-heal and the atlas grow pad) all landed
/// first. 2048 is an OPT-IN picker choice per body, never a default; a surface too dense for
/// the pool ceiling self-heals by dropping one resolution notch. Full-rebuild GPU cost at a
/// realistic 2048 body ≈ 350ms (13ms classify + 219ms z-slab bake + 118ms monolithic mesh —
/// spike table, CACHE_REMESH_512_REVIEW.md).
pub const fn max_sparse_resolution() -> u32 { 2048 }

/// Max resolution for texture-only bake (the legacy dense-texture Volume Preview / SDF Edit
/// Mode raymarch). Capped at 512: a 1024³ R32F volume texture is 4GB of VRAM for a legacy
/// path — resolutions above 512 belong to the sparse brick pipeline (Object Mode), never
/// TextureOnly. (History: this was 2048 while TextureOnly was the only >512 route.)
pub const fn max_textureonly_resolution() -> u32 { 512 }

/// Select bake output mode based on resolution and device role.
pub fn select_bake_output_mode(resolution: u32, is_primary_gpu: bool) -> BakeOutputMode {
    if resolution > max_gridbacked_resolution() && is_primary_gpu {
        BakeOutputMode::TextureOnly
    } else {
        BakeOutputMode::GridBacked
    }
}
use glam::Vec3;

use crate::WorkerGpu;

// ── Job Description ──────────────────────────────────────────────────

/// Which device should execute this job.
pub enum DeviceTarget {
    /// Dispatcher picks the next available worker via round-robin.
    /// Falls back to primary GPU if no workers enabled.
    AnyWorker,
    /// Use the viewport GPU directly (blocking on main thread).
    /// Only for jobs that need immediate results.
    #[allow(dead_code)]
    Primary,
}

/// Describes what work to do. Does NOT carry device/queue.
pub struct GpuJob {
    pub kind: GpuJobKind,
    pub generation: u64,
    pub source_hash: u64,
    pub target: DeviceTarget,
}

pub enum GpuJobKind {
    BodyVolumeBake {
        body_nid: NodeId,
        /// Pre-serialized GPU prim buffer (with SCOPE_OPEN/CLOSE markers for nested bodies).
        prim_floats: Vec<f32>,
        /// Separate clip curve polygon data buffer (binding 4).
        clip_curve_floats: Vec<f32>,
        resolution: [u32; 3],
        bounds_min: Vec3,
        bounds_max: Vec3,
        mirror_flags: u8,
        mirror_sign: u8,
        mirror_center: Vec3,
        bevel_radius: f32,
        /// Sub-region bake bounds (voxel indices, inclusive start).
        /// When `base_grid` is Some, only voxels in [bake_min .. bake_max) are re-evaluated.
        /// Defaults to [0,0,0] for full bake.
        bake_min: [u32; 3],
        /// Sub-region bake bounds (voxel indices, exclusive end).
        /// Defaults to `resolution` for full bake.
        bake_max: [u32; 3],
        /// Existing grid data for partial bake. When Some, uploaded to GPU output buffer
        /// before dispatch so non-dirty voxels retain their previous values.
        /// None = full bake (output buffer initialized to zero).
        base_grid: Option<Vec<f32>>,
    },
    // Future: MeshCacheBuild, SdfMaskCompute, etc.
}

// ── Generation Counter Key ───────────────────────────────────────────

/// Concrete key for generation tracking. One variant per job type + target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuJobTargetKey {
    BodyVolume(NodeId),
    // Future: MeshCache(NodeId), SdfMask(NodeId), etc.
}

/// Extract the target key from a job kind.
fn job_target_key(kind: &GpuJobKind) -> GpuJobTargetKey {
    match kind {
        GpuJobKind::BodyVolumeBake { body_nid, .. } => GpuJobTargetKey::BodyVolume(*body_nid),
    }
}

// ── Result ───────────────────────────────────────────────────────────

pub struct GpuJobResult {
    pub kind: GpuJobResultKind,
    pub generation: u64,
    pub source_hash: u64,
    pub target_key: GpuJobTargetKey,
    pub source_adapter: String,
    pub duration_ms: f32,
    /// Stable index of the worker that produced this result.
    pub worker_idx: usize,
}

// ── Per-Worker Stats ────────────────────────────────────────────────

pub struct WorkerStats {
    pub worker_idx: usize,
    pub adapter_name: String,
    pub jobs_dispatched: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    pub last_duration_ms: f32,
    pub last_job_kind: &'static str,
    pub healthy: bool,
}

/// Centralized device-loss detection. Called in ONE place (poll_results).
fn is_device_loss(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    ["lost", "hang", "reset", "removed", "map failed"]
        .iter().any(|kw| lower.contains(kw))
}

#[allow(dead_code)]
pub enum GpuJobResultKind {
    BodyVolume {
        body_nid: NodeId,
        grid: Vec<f32>,
        resolution: [u32; 3],
        /// GATE 2 — the voxel window `[min, max)` this bake actually re-evaluated (full bakes
        /// carry the full `[0, res)` box). Acceptance folds it into the app's single
        /// `DirtyMeshRegion` (accept-time producer contract).
        bake_window: ([u32; 3], [u32; 3]),
    },
    /// Error result — triggers CPU fallback for this body.
    Error {
        body_nid: NodeId,
        message: String,
    },
}

// ── BakeParams uniform struct ────────────────────────────────────────

/// Must match WGSL BakeParams struct layout EXACTLY (16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BakeParams {
    pub grid_res: [u32; 3],
    pub _pad0: u32,
    pub bounds_min: [f32; 3],
    pub _pad1: f32,
    pub bounds_max: [f32; 3],
    pub prim_count: u32,
    pub floats_per_prim: u32,
    /// Brick-grid dims for SparseBricks (output_mode=2): bricks per axis over the BODY voxel
    /// grid. Repurposed spare padding (was `_pad2`) — field offsets unchanged. `[0; 3]` in
    /// modes 0/1.
    pub dims_bricks: [u32; 3],
    pub bake_min: [u32; 3],   // inclusive start of region to bake
    pub _pad3: u32,
    pub bake_max: [u32; 3],   // exclusive end of region to bake
    pub _pad4: u32,
    pub mirror_flags: u32,    // bit 0=X, bit 1=Y, bit 2=Z
    pub mirror_center: [f32; 3],
    pub bevel_radius: f32,
    pub mirror_sign: u32,     // bit 0=flip X, bit 1=flip Y, bit 2=flip Z
    pub atlas_offset: [u32; 3],  // [x, y, z] texel offset in atlas 3D texture (0 = no atlas)
    pub atlas_pad: u32,       // padding voxels on each side in atlas (ATLAS_PAD)
    pub output_mode: u32,     // 0=GridBacked (write flat buffer + texture), 1=TextureOnly (texture only)
    pub _pad5: u32,           // pad to 16-byte alignment
    /// LAB mesh operand: the baked body's local→world transform (column-major).
    /// Bake sample points are in the TOP body's LOCAL frame (BODY_LOCAL_PRIMS);
    /// the operand grid header is WORLD space — the shader maps local→world with
    /// this before sampling mesh_grid. Identity everywhere the operand is
    /// disabled (workers, brick fill, tests).
    pub body_local_to_world: [[f32; 4]; 4],
}
// Byte size is the WGSL contract (volume_bake.wgsl BakeParams). 208 = 144 (fields through
// _pad5) + 64 (body_local_to_world mat4x4, the LAB mesh operand). Repurposing spare padding
// (e.g. `dims_bricks`) must NEVER change it; adding the operand matrix bumped it 144 → 208.
const _: () = assert!(std::mem::size_of::<BakeParams>() == 208);
const _: () = assert!(std::mem::size_of::<BakeParams>() % 16 == 0);

/// Column-major 4×4 identity for BakeParams.body_local_to_world at sites without
/// a scene transform (workers, brick fill, tests).
pub const IDENTITY_MAT4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

// ── Worker State (private to worker thread) ──────────────────────────

struct WorkerState {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    adapter_name: String,
    bake_pipeline: Option<wgpu::ComputePipeline>,
    bake_bind_group_layout: Option<wgpu::BindGroupLayout>,
    /// 1³ R32Float storage-texture dummy bound as output_tex (@binding 3). The bake shader's
    /// `textureStore(output_tex,…)` is UNCONDITIONAL — it fires even in GridBacked mode, which workers
    /// always use — so the binding MUST exist or `create_compute_pipeline` fails layout validation and
    /// workers can never bake. Out-of-bounds stores to this 1³ texture are discarded no-ops (WebGPU),
    /// so the real output still lands in the flat output_grid. Mirrors the primary GPU's bake_tex_dummy.
    bake_tex_dummy: Option<wgpu::Texture>,
    /// 8-float zeroed storage dummy bound as mesh_grid (@binding 5). The LAB mesh-SDF
    /// operand is a primary-GPU-only spike; workers always bind this disabled dummy
    /// (header res = 0.0 → sample_mesh_grid returns 1e6, union no-op). Same rationale
    /// as bake_tex_dummy: the binding must exist or pipeline layout validation fails.
    lab_mesh_grid_dummy: Option<wgpu::Buffer>,
    bake_pipeline_failed: bool,
    // Reusable GPU buffers (grow-only — never shrink)
    prim_buffer: Option<wgpu::Buffer>,
    prim_buffer_size: u64,
    params_buffer: Option<wgpu::Buffer>,
    output_buffer: Option<wgpu::Buffer>,
    output_buffer_size: u64,
}

impl WorkerState {
    fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>, adapter_name: String) -> Self {
        Self {
            device,
            queue,
            adapter_name,
            bake_pipeline: None,
            bake_bind_group_layout: None,
            bake_tex_dummy: None,
            lab_mesh_grid_dummy: None,
            bake_pipeline_failed: false,
            prim_buffer: None,
            prim_buffer_size: 0,
            params_buffer: None,
            output_buffer: None,
            output_buffer_size: 0,
        }
    }

    /// Lazy-init compute pipeline on first BodyVolumeBake call.
    /// Returns false if pipeline creation failed (caller should return error result).
    fn ensure_bake_pipeline(&mut self) -> bool {
        if self.bake_pipeline.is_some() { return true; }
        if self.bake_pipeline_failed { return false; }

        let shader_source = graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL);

        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("volume_bake.wgsl"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        // BGL entries — SINGLE SOURCE: graphics::bgl::bake_bgl_entries().
        // binding(3) output_tex is written UNCONDITIONALLY by the shader; workers bind a
        // 1³ dummy texture (OOB stores are discarded, real output goes to output_grid).
        // binding(5) mesh_grid gets the zeroed lab_mesh_grid_dummy (operand disabled).
        let bind_group_layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bake_bind_group_layout"),
            entries: &graphics::bgl::bake_bgl_entries(),
        });

        let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bake_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Use create_compute_pipeline (not async) — blocks until done.
        // If the shader fails to compile, wgpu will panic or log an error.
        // We catch this by checking if the pipeline is valid.
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("volume_bake_pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("bake_volume"),
            compilation_options: Default::default(),
            cache: None,
        });

        // 1³ R32Float dummy for output_tex (@binding 3). Created once alongside the pipeline (not per
        // dispatch — Frame Ordering / grow-only spirit). Workers always bake GridBacked, so the real
        // output is output_grid; the shader's unconditional textureStore lands here and is discarded.
        self.bake_tex_dummy = Some(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bake_output_tex_dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        }));
        // Zeroed 8-float header dummy for mesh_grid (@binding 5): res = 0.0 → disabled.
        self.lab_mesh_grid_dummy = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lab_mesh_grid_dummy"),
            size: 8 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }));
        self.bake_bind_group_layout = Some(bind_group_layout);
        self.bake_pipeline = Some(pipeline);
        true
    }

    /// Ensure prim_data buffer is large enough. Grow-only.
    fn ensure_prim_buffer(&mut self, required_bytes: u64) {
        if self.prim_buffer_size >= required_bytes { return; }
        self.prim_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bake_prim_data"),
            size: required_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.prim_buffer_size = required_bytes;
    }

    /// Ensure output buffer is large enough. Grow-only.
    fn ensure_output_buffer(&mut self, required_bytes: u64) {
        if self.output_buffer_size >= required_bytes { return; }
        self.output_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bake_output_grid"),
            size: required_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }));
        self.output_buffer_size = required_bytes;
    }

    /// Ensure params uniform buffer exists.
    fn ensure_params_buffer(&mut self) {
        if self.params_buffer.is_some() { return; }
        self.params_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bake_params"),
            size: std::mem::size_of::<BakeParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }

    fn execute(&mut self, job: GpuJob, worker_idx: usize) -> GpuJobResult {
        let start = std::time::Instant::now();
        let target_key = job_target_key(&job.kind);
        let kind = match job.kind {
            GpuJobKind::BodyVolumeBake { body_nid, prim_floats, clip_curve_floats, resolution, bounds_min, bounds_max, mirror_flags, mirror_sign, mirror_center, bevel_radius, bake_min, bake_max, base_grid } => {
                self.execute_body_volume_bake(body_nid, prim_floats, clip_curve_floats, resolution, bounds_min, bounds_max, mirror_flags, mirror_sign, mirror_center, bevel_radius, bake_min, bake_max, base_grid)
            }
        };
        GpuJobResult {
            kind,
            generation: job.generation,
            source_hash: job.source_hash,
            target_key,
            source_adapter: self.adapter_name.clone(),
            duration_ms: start.elapsed().as_secs_f32() * 1000.0,
            worker_idx,
        }
    }

    fn execute_body_volume_bake(
        &mut self,
        body_nid: NodeId,
        prim_floats: Vec<f32>,
        clip_curve_floats: Vec<f32>,
        resolution: [u32; 3],
        bounds_min: Vec3,
        bounds_max: Vec3,
        mirror_flags: u8,
        mirror_sign: u8,
        mirror_center: Vec3,
        bevel_radius: f32,
        bake_min: [u32; 3],
        bake_max: [u32; 3],
        base_grid: Option<Vec<f32>>,
    ) -> GpuJobResultKind {
        // 1. Lazy pipeline init
        if !self.ensure_bake_pipeline() {
            return GpuJobResultKind::Error {
                body_nid,
                message: format!("Bake pipeline failed on {}", self.adapter_name),
            };
        }

        // 2. Pre-serialized prim buffer (with SCOPE markers)
        let visible_count = prim_floats.len() / graphics::HOT_FLOATS_PER_PRIM;
        let prim_bytes: &[u8] = bytemuck::cast_slice(&prim_floats);

        let grid_count = (resolution[0] as u64) * (resolution[1] as u64) * (resolution[2] as u64);
        let output_bytes = grid_count * 4; // f32

        let is_partial = base_grid.is_some();

        // 3. Ensure buffers
        self.ensure_prim_buffer(prim_bytes.len() as u64);
        self.ensure_output_buffer(output_bytes);
        self.ensure_params_buffer();

        // 4. Write data to buffers
        let params = BakeParams {
            grid_res: resolution,
            _pad0: 0,
            bounds_min: bounds_min.to_array(),
            _pad1: 0.0,
            bounds_max: bounds_max.to_array(),
            prim_count: visible_count as u32,
            floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
            dims_bricks: [0; 3], // SparseBricks-only; workers always bake GridBacked

            bake_min,
            _pad3: 0,
            bake_max,
            _pad4: 0,
            mirror_flags: mirror_flags as u32,
            mirror_center: mirror_center.to_array(),
            bevel_radius,
            mirror_sign: mirror_sign as u32,
            atlas_offset: [0, 0, 0], // worker GPUs don't have the atlas — write to origin
            atlas_pad: 0,             // worker GPUs bake to per-body texture — no atlas padding
            output_mode: BakeOutputMode::GridBacked as u32, // workers always use GridBacked
            _pad5: 0,
            body_local_to_world: IDENTITY_MAT4, // workers bind the disabled operand dummy
        };

        let prim_buf = match self.prim_buffer.as_ref() {
            Some(b) => b,
            None => {
                eprintln!("[WorkerGPU] prim_buffer is None after ensure_prim_buffer on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: prim_buffer is None on {}", self.adapter_name),
                };
            }
        };
        let params_buf = match self.params_buffer.as_ref() {
            Some(b) => b,
            None => {
                eprintln!("[WorkerGPU] params_buffer is None after ensure_params_buffer on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: params_buffer is None on {}", self.adapter_name),
                };
            }
        };
        let output_buf = match self.output_buffer.as_ref() {
            Some(b) => b,
            None => {
                eprintln!("[WorkerGPU] output_buffer is None after ensure_output_buffer on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: output_buffer is None on {}", self.adapter_name),
                };
            }
        };

        self.queue.write_buffer(prim_buf, 0, prim_bytes);
        self.queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&params));

        // 4b. For partial bake: upload existing grid to output buffer so non-dirty
        // voxels retain their previous values. The shader only writes voxels in
        // [bake_min .. bake_max), so everything outside is preserved.
        if let Some(ref bg) = base_grid {
            let bg_bytes: &[u8] = bytemuck::cast_slice(bg);
            self.queue.write_buffer(output_buf, 0, bg_bytes);
        }

        // 4c. Clip curve buffer (binding 4)
        let clip_bytes: &[u8] = bytemuck::cast_slice(&clip_curve_floats);
        let clip_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bake_clip_curves"),
            size: (clip_bytes.len() as u64).max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&clip_buf, 0, clip_bytes);

        // Dummy output_tex view (@binding 3), created per dispatch from the persistent 1³ dummy.
        let bake_dummy_view = match self.bake_tex_dummy.as_ref() {
            Some(t) => t.create_view(&wgpu::TextureViewDescriptor::default()),
            None => {
                eprintln!("[WorkerGPU] bake_tex_dummy is None on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: bake_tex_dummy is None on {}", self.adapter_name),
                };
            }
        };

        // Disabled mesh_grid dummy (@binding 5) — created in ensure_bake_pipeline.
        let mesh_grid_dummy = match self.lab_mesh_grid_dummy.as_ref() {
            Some(b) => b,
            None => {
                eprintln!("[WorkerGPU] lab_mesh_grid_dummy is None on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: lab_mesh_grid_dummy is None on {}", self.adapter_name),
                };
            }
        };

        // 5. Create bind group (recreated each dispatch — cheap)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bake_bind_group"),
            layout: match self.bake_bind_group_layout.as_ref() {
                Some(l) => l,
                None => {
                    eprintln!("[WorkerGPU] bake_bind_group_layout is None on {}", self.adapter_name);
                    return GpuJobResultKind::Error {
                        body_nid,
                        message: format!("GPU resource init failed: bake_bind_group_layout is None on {}", self.adapter_name),
                    };
                }
            },
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: prim_buf,
                        offset: 0,
                        size: Some(std::num::NonZeroU64::new(prim_bytes.len() as u64).unwrap_or(std::num::NonZeroU64::new(4).unwrap())),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params_buf,
                        offset: 0,
                        size: None,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: output_buf,
                        offset: 0,
                        size: Some(std::num::NonZeroU64::new(output_bytes).unwrap_or(std::num::NonZeroU64::new(4).unwrap())),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&bake_dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: clip_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: mesh_grid_dummy.as_entire_binding(),
                },
                // binding(6): SparseBricks brick index — workers force full DENSE bakes
                // (output_mode 0/1), which never read binding 6; the zeroed dummy fills it.
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: mesh_grid_dummy.as_entire_binding(),
                },
                // binding(7): curv_positions — read only by `curvature_eval` (a primary-GPU
                // entry point workers never dispatch); the zeroed dummy fills it so the bind
                // group matches the layout, same rationale as bindings 5/6.
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: mesh_grid_dummy.as_entire_binding(),
                },
                // binding(8): T1 height_maps — workers do not pack image height atlases
                // (primary is the production path; same MeshVolume dummy precedent). hot[92]
                // stays HEIGHT_MAP_NONE on worker-serialized buffers so FBM displace is used.
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: mesh_grid_dummy.as_entire_binding(),
                },
            ],
        });

        // 6. Dispatch compute
        let bake_pipeline = match self.bake_pipeline.as_ref() {
            Some(p) => p,
            None => {
                eprintln!("[WorkerGPU] bake_pipeline is None on {}", self.adapter_name);
                return GpuJobResultKind::Error {
                    body_nid,
                    message: format!("GPU resource init failed: bake_pipeline is None on {}", self.adapter_name),
                };
            }
        };
        // Workgroup size 4x4x4 — dispatch covers only [bake_min..bake_max) region.
        let region_x = bake_max[0].saturating_sub(bake_min[0]);
        let region_y = bake_max[1].saturating_sub(bake_min[1]);
        let region_z = bake_max[2].saturating_sub(bake_min[2]);
        let wg_x = (region_x + 3) / 4;
        let wg_y = (region_y + 3) / 4;
        let wg_z = (region_z + 3) / 4;
        if is_partial {
            eprintln!(
                "[WorkerGPU] Partial bake: region [{},{},{}]->[{},{},{}] dispatch {}x{}x{} on {}",
                bake_min[0], bake_min[1], bake_min[2],
                bake_max[0], bake_max[1], bake_max[2],
                wg_x, wg_y, wg_z, self.adapter_name
            );
        }

        // TDR guard: split large dispatches into Z-slices (same logic as primary bake).
        const MAX_WG_Z_PER_SLICE: u32 = 16;

        if wg_z <= MAX_WG_Z_PER_SLICE {
            // Small dispatch: single pass
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bake_encoder"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("bake_pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(bake_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(wg_x, wg_y, wg_z);
            }
            self.queue.submit(std::iter::once(encoder.finish()));
        } else {
            // Large dispatch (256³+): split Z into slices with poll between each
            let voxels_per_slice = MAX_WG_Z_PER_SLICE * 4;
            let total_z_voxels = region_z;
            let num_slices = (total_z_voxels + voxels_per_slice - 1) / voxels_per_slice;
            let orig_bake_min_z = bake_min[2];
            let orig_bake_max_z = bake_max[2];

            for slice_idx in 0..num_slices {
                let slice_min_z = orig_bake_min_z + slice_idx * voxels_per_slice;
                let slice_max_z = (slice_min_z + voxels_per_slice).min(orig_bake_max_z);
                let slice_wg_z = (slice_max_z - slice_min_z + 3) / 4;

                // Update params uniform with this slice's Z range
                let mut slice_params = params;
                slice_params.bake_min[2] = slice_min_z;
                slice_params.bake_max[2] = slice_max_z;
                self.queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&slice_params));

                let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("bake_encoder"),
                });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("bake_pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(bake_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.dispatch_workgroups(wg_x, wg_y, slice_wg_z);
                }
                self.queue.submit(std::iter::once(encoder.finish()));

                // Poll between slices to flush GPU work and reset TDR timer
                if slice_idx < num_slices - 1 {
                    if let Err(error) = self.device.poll(wgpu::PollType::Wait) {
                        return GpuJobResultKind::Error {
                            body_nid,
                            message: format!("GPU bake wait failed on {}: {error}", self.adapter_name),
                        };
                    }
                }
            }
            eprintln!(
                "[WorkerGPU] Split dispatch into {} Z-slices on {}",
                num_slices, self.adapter_name
            );
        }

        // 7. Readback (blocking on worker thread — that's fine)
        //    Wrap map_async with error detection for device loss handling.
        let read_start = std::time::Instant::now();
        let buffer_slice = output_buf.slice(..output_bytes);
        let map_ok = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let map_flag = map_ok.clone();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            map_flag.store(result.is_ok(), std::sync::atomic::Ordering::SeqCst);
        });
        if let Err(error) = self.device.poll(wgpu::PollType::Wait) {
            return GpuJobResultKind::Error {
                body_nid,
                message: format!("GPU readback wait failed on {}: {error}", self.adapter_name),
            };
        }

        let elapsed = read_start.elapsed();
        if elapsed.as_secs() >= 5 {
            eprintln!(
                "[WorkerGPU] WARNING: readback stalled for {:.1}s on {}",
                elapsed.as_secs_f32(),
                self.adapter_name
            );
        }

        if !map_ok.load(std::sync::atomic::Ordering::SeqCst) {
            return GpuJobResultKind::Error {
                body_nid,
                message: format!("map failed (device lost?) on {}", self.adapter_name),
            };
        }

        let mapped = buffer_slice.get_mapped_range();
        let grid: Vec<f32> = bytemuck::cast_slice(&mapped).to_vec();
        drop(mapped);
        output_buf.unmap();

        // 8. Sanity check
        let expected_len = grid_count as usize;
        if grid.len() != expected_len {
            return GpuJobResultKind::Error {
                body_nid,
                message: format!(
                    "Grid size mismatch: got {} expected {} on {}",
                    grid.len(), expected_len, self.adapter_name
                ),
            };
        }
        if grid.iter().any(|v| v.is_nan() || v.is_infinite()) {
            return GpuJobResultKind::Error {
                body_nid,
                message: format!("NaN/inf in baked grid on {}", self.adapter_name),
            };
        }

        GpuJobResultKind::BodyVolume {
            body_nid,
            grid,
            resolution,
            // GATE 2: a partial bake re-evaluated only [bake_min, bake_max); a full bake
            // (no seed) covered the whole grid regardless of the window args.
            bake_window: if is_partial { (bake_min, bake_max) } else { ([0, 0, 0], resolution) },
        }
    }
}

// ── Dispatcher ───────────────────────────────────────────────────────

pub struct GpuJobDispatcher {
    /// One sender per worker — sends jobs to that worker's thread.
    job_txs: Vec<Sender<GpuJob>>,
    /// Shared receiver — all worker threads send results here.
    result_rx: Receiver<GpuJobResult>,
    /// Generation counter per target key. Incremented at dispatch, checked at poll.
    generation_counters: HashMap<GpuJobTargetKey, u64>,
    /// Cached indices of enabled workers. Rebuilt on toggle change.
    enabled_worker_indices: Vec<usize>,
    /// Round-robin counter for worker selection.
    next_worker_idx: usize,
    /// Thread handles for clean shutdown via Drop.
    worker_handles: Vec<JoinHandle<()>>,
    /// Per-worker stats, indexed by worker_idx (stable).
    worker_stats: Vec<WorkerStats>,
}

impl GpuJobDispatcher {
    /// Create a new dispatcher, spawning one thread per worker GPU.
    pub fn new(workers: &[WorkerGpu]) -> Self {
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<GpuJobResult>();
        let mut job_txs = Vec::with_capacity(workers.len());
        let mut worker_handles = Vec::with_capacity(workers.len());
        let mut worker_stats = Vec::with_capacity(workers.len());

        for (i, worker) in workers.iter().enumerate() {
            let (job_tx, job_rx) = crossbeam_channel::unbounded::<GpuJob>();
            let device = Arc::clone(&worker.device);
            let queue = Arc::clone(&worker.queue);
            let adapter_name = worker.adapter_name.clone();
            let rtx = result_tx.clone();
            let widx = i; // stable worker_idx passed into thread closure

            let handle = std::thread::Builder::new()
                .name(format!("worker-gpu-{}", i))
                .spawn(move || {
                    let mut state = WorkerState::new(device, queue, adapter_name);
                    while let Ok(job) = job_rx.recv() {
                        let result = state.execute(job, widx);
                        let _ = rtx.send(result);
                    }
                })
                .expect("failed to spawn worker GPU thread");

            job_txs.push(job_tx);
            worker_handles.push(handle);
            worker_stats.push(WorkerStats {
                worker_idx: i,
                adapter_name: worker.adapter_name.clone(),
                jobs_dispatched: 0,
                jobs_completed: 0,
                jobs_failed: 0,
                last_duration_ms: 0.0,
                last_job_kind: "",
                healthy: true,
            });
        }

        let enabled_worker_indices: Vec<usize> = workers.iter()
            .enumerate()
            .filter(|(_, w)| w.enabled)
            .map(|(i, _)| i)
            .collect();

        Self {
            job_txs,
            result_rx,
            generation_counters: HashMap::new(),
            enabled_worker_indices,
            next_worker_idx: 0,
            worker_handles,
            worker_stats,
        }
    }

    /// Call after any worker enabled/disabled toggle.
    pub fn rebuild_enabled_workers(&mut self, workers: &[WorkerGpu]) {
        self.enabled_worker_indices = workers.iter()
            .enumerate()
            .filter(|(_, w)| w.enabled)
            .map(|(i, _)| i)
            .collect();
    }

    /// Round-robin over enabled workers. Returns index into job_txs.
    fn pick_worker(&mut self) -> Option<usize> {
        if self.enabled_worker_indices.is_empty() { return None; }
        let pos = self.next_worker_idx % self.enabled_worker_indices.len();
        self.next_worker_idx = self.next_worker_idx.wrapping_add(1);
        Some(self.enabled_worker_indices[pos])
    }

    /// Returns true if there are enabled workers that can accept jobs.
    pub fn has_enabled_workers(&self) -> bool {
        !self.enabled_worker_indices.is_empty()
    }

    /// Dispatch a job. Returns true if sent to a worker, false if caller should
    /// handle it on the primary GPU (no workers available or Primary target).
    pub fn dispatch(&mut self, job: GpuJob) -> bool {
        match job.target {
            DeviceTarget::AnyWorker => {
                if let Some(idx) = self.pick_worker() {
                    let key = job_target_key(&job.kind);
                    *self.generation_counters.entry(key).or_insert(0) = job.generation;
                    if idx < self.worker_stats.len() {
                        self.worker_stats[idx].jobs_dispatched += 1;
                    }
                    let _ = self.job_txs[idx].send(job);
                    true
                } else {
                    false // no workers — caller falls back to primary
                }
            }
            DeviceTarget::Primary => false, // caller handles primary path
        }
    }

    /// Poll for completed results. Non-blocking. Discards stale generations.
    /// Updates per-worker stats and detects device loss.
    pub fn poll_results(&mut self) -> Vec<GpuJobResult> {
        let mut results = Vec::new();
        let mut any_device_loss = false;
        while let Ok(result) = self.result_rx.try_recv() {
            // Update per-worker stats (by index, NOT string)
            if result.worker_idx < self.worker_stats.len() {
                let s = &mut self.worker_stats[result.worker_idx];
                s.last_duration_ms = result.duration_ms;
                match &result.kind {
                    GpuJobResultKind::Error { message, .. } => {
                        s.jobs_failed += 1;
                        s.last_job_kind = "Error";
                        if is_device_loss(message) {
                            s.healthy = false;
                            any_device_loss = true;
                            eprintln!("[WorkerGPU] Device error on worker {}: {}", result.worker_idx, message);
                        }
                    }
                    GpuJobResultKind::BodyVolume { .. } => {
                        s.jobs_completed += 1;
                        s.last_job_kind = "BodyVolume";
                    }
                }
            }
            // Generation check — discard stale results
            let current_gen = self.generation_counters
                .get(&result.target_key)
                .copied()
                .unwrap_or(0);
            if result.generation >= current_gen {
                results.push(result);
            }
        }
        // Remove unhealthy workers from dispatch pool
        if any_device_loss {
            self.enabled_worker_indices.retain(|&idx| {
                idx < self.worker_stats.len() && self.worker_stats[idx].healthy
            });
        }
        results
    }

    /// Public accessor for per-worker stats.
    pub fn worker_stats(&self) -> &[WorkerStats] {
        &self.worker_stats
    }
}

impl Drop for GpuJobDispatcher {
    fn drop(&mut self) {
        // Drop all senders — causes worker threads' recv() to return Err, exiting the loop.
        self.job_txs.clear();
        // Join all worker threads for clean shutdown.
        for handle in self.worker_handles.drain(..) {
            let _ = handle.join();
        }
    }
}
