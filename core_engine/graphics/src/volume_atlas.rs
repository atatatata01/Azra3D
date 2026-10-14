//! Volume Atlas — single 3D texture holding ALL bodies simultaneously.
//!
//! Each body occupies a fixed-size cell in a 3D grid within the atlas.
//! The VP shader raymarches each body from its atlas sub-region using
//! per-body AABB + UVW offsets. Zero body-switch cost. All bodies visible
//! at all times.
//!
//! Layout: dims[0] × dims[1] × dims[2] (R32Float)
//! Bodies are packed into uniform cells of cell_size³ texels.
//! A ShelfAllocator packs variable-size cells with minimal fragmentation.

use wgpu;
use glam::{Vec3, Vec4, Mat4};
use bytemuck::Zeroable;
use core_math::scene_graph::{NodeId, BodyTransform};
use core_math::primitives::BooleanOp;

// ============================================================
// Frustum Culling
// ============================================================

/// 6 frustum planes extracted from a view-projection matrix.
/// Each plane is (normal.xyz, distance) with the convention that
/// dot(plane.xyz, point) + plane.w >= 0 means the point is INSIDE.
pub struct FrustumPlanes {
    pub planes: [Vec4; 6],
}

impl FrustumPlanes {
    /// Extract 6 frustum planes from a view-projection matrix (Gribb/Hartmann method).
    /// Planes point inward — a point inside the frustum satisfies all 6 half-spaces.
    pub fn from_view_proj(vp: &Mat4) -> Self {
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);
        let mut planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r3 + r2, // near
            r3 - r2, // far
        ];
        // Normalize each plane so dot product gives signed distance
        for p in &mut planes {
            let len = Vec3::new(p.x, p.y, p.z).length();
            if len > 1e-8 { *p /= len; }
        }
        Self { planes }
    }

    /// Test if an AABB is entirely outside the frustum (conservative).
    /// Returns true if the AABB is definitely invisible.
    pub fn aabb_outside(&self, aabb_min: Vec3, aabb_max: Vec3) -> bool {
        for plane in &self.planes {
            let n = Vec3::new(plane.x, plane.y, plane.z);
            // P-vertex: corner of AABB most in the direction of the plane normal
            let p = Vec3::new(
                if n.x >= 0.0 { aabb_max.x } else { aabb_min.x },
                if n.y >= 0.0 { aabb_max.y } else { aabb_min.y },
                if n.z >= 0.0 { aabb_max.z } else { aabb_min.z },
            );
            if n.dot(p) + plane.w < 0.0 {
                return true; // entirely outside this plane
            }
        }
        false
    }
}

// ============================================================
// Constants
// ============================================================

/// Maximum atlas dimension on any axis (wgpu max_texture_dimension_3d = 2048).
pub const MAX_TEXTURE_DIM: u32 = 2048;

/// Maximum atlas XY the user can request. 1024³ atlas = 4GB R32Float.
pub const ATLAS_XY_MAX: u32 = 2048; // wgpu max_texture_dimension_3d = 2048

/// Padding voxels on each side of a body cell in the atlas.
/// Padding texels contain valid SDF data (evaluated by the bake shader at
/// world positions extending beyond the nominal bounds). This prevents
/// trilinear sampling artifacts at body cell boundaries — the sampler
/// reads valid distance values instead of the +999.0 sentinel.
///
/// Each body's cell includes PAD voxels on each side per axis:
///   [origin .. origin+PAD)            = padding (valid SDF, before bounds)
///   [origin+PAD .. origin+PAD+res)    = body data
///   [origin+PAD+res .. origin+2*PAD+res) = padding (valid SDF, after bounds)
pub const ATLAS_PAD: u32 = 2;

/// Maximum bodies in the atlas. Buffer size = 256 * 64 = 16KB.
pub const MAX_ATLAS_BODIES: usize = 256;

/// Cooldown (ms) between resolution changes to prevent thrashing when the
/// user rapidly toggles the resolution button (64/128/256).
const RESOLUTION_CHANGE_COOLDOWN_MS: u128 = 500;

/// Default cell size: 128 body voxels + 2*PAD on each side = 132.
pub const DEFAULT_CELL_SIZE: u32 = 128 + 2 * ATLAS_PAD;

// ============================================================
// Per-body GPU parameters (uploaded as storage buffer)
// ============================================================

/// GPU-side per-body data for the VP shader's multi-body loop.
/// Must match the WGSL BodyAtlasParams struct layout exactly (16-byte aligned).
///
/// UVW contract: `atlas_uvw = body_uvw * uvw_scale + uvw_offset` for all 3 axes.
/// Offset includes ATLAS_PAD so body_uvw=0 lands on the first body data texel.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuBodyAtlasParams {
    /// World-space AABB min (body-local bake bounds transformed to world).
    pub aabb_min: [f32; 3],
    /// Boolean operation for combining this body with previous bodies in the scene.
    /// Encoded as f32: 0.0=Union, 1.0=Subtract, 2.0=Intersect.
    /// First body's op is ignored (it IS the base SDF).
    pub body_op: f32,
    /// World-space AABB max.
    pub aabb_max: [f32; 3],
    /// Smooth blend radius for body_op. 0.0 = hard boolean, >0 = smooth transition.
    pub blend_k: f32,
    /// Atlas UVW offset: atlas_uvw = body_uvw * uvw_scale + uvw_offset.
    /// Currently (0, 0, z_start/total_depth). XY reserved for future packing.
    pub uvw_offset: [f32; 3],
    /// Scene-graph body order index (0-based). Same encoding as selected_body_index
    /// uniform and gbuf_motion.r. One contract everywhere.
    pub body_index: u32,
    /// Atlas UVW scale: per-axis (res_x/atlas_xy, res_y/atlas_xy, res_z/total_depth).
    pub uvw_scale: [f32; 3],
    /// Body bake resolution (max of the 3 per-axis resolutions).
    /// Used by the VP shader for texel_size / step computation.
    pub resolution: u32,
}  // 64 bytes = 4 × vec4

// ============================================================
// Atlas slot — tracks one body's position in the atlas
// ============================================================

/// A body's allocation within the atlas 3D texture.
#[derive(Clone, Debug)]
pub struct AtlasSlot {
    pub body_nid: NodeId,
    /// Start X-texel origin in the atlas texture.
    pub x_start: u32,
    /// Start Y-texel origin in the atlas texture.
    pub y_start: u32,
    /// Start Z-texel origin in the atlas texture.
    pub z_start: u32,
    /// Padded size in texels [px, py, pz] including 2*ATLAS_PAD per axis.
    pub padded_size: [u32; 3],
    /// Per-axis body bake resolution [x, y, z].
    /// The longest axis gets the full target resolution; short axes are
    /// proportionally smaller (minimum 8 voxels per axis).
    pub resolution: [u32; 3],
    /// Source hash for dedup (skip re-upload if unchanged). Includes prim_uid.
    pub source_hash: u64,
    /// Template hash for instancing (excludes prim_uid — clones match).
    pub template_hash: u64,
    /// Body-local bake bounds (for world-space AABB computation).
    pub bake_bounds_min: Vec3,
    pub bake_bounds_max: Vec3,
    /// Body transform captured AT BAKE TIME. Used by rebuild_params to compute
    /// the world-space AABB. Using the bake-time transform (not the current one)
    /// ensures old baked data stays at the correct world position even when the
    /// body origin shifts (e.g., recalculate_body_origin after adding a prim).
    /// Updated when a new bake completes for this slot.
    pub bake_transform: BodyTransform,
    /// True when the slot has valid baked data in the atlas texture.
    pub valid: bool,
    /// Generation counter — incremented on each allocate(). Used to reject stale
    /// bake results that were dispatched before a realloc (e.g., slow bake from
    /// resolution N arriving after a faster bake from resolution N+1).
    pub generation: u64,
    /// Frame number when this body was last visible (passed frustum cull and
    /// included in body_params). Used for LRU eviction.
    pub last_used_frame: u64,
}

// ============================================================
// Grid Allocator — fixed-cell 3D grid, zero fragmentation
// ============================================================

/// Variable-size 3D shelf allocator. Bodies get exactly the texels they need.
///
/// Packing: shelves along Z, rows along Y within each shelf, bodies along X in rows.
/// Core invariant: allocator NEVER moves live allocations during free().
struct ShelfAllocator {
    atlas_dims: [u32; 3],
    shelves: Vec<Shelf>,
    z_used: u32,
    allocations: std::collections::HashMap<NodeId, Allocation>,
    /// Count of frees that left an unreclaimed hole since the last full reset. >0 means a compact
    /// would recover space — used to decide whether an allocation failure is fixable (holes) vs a
    /// genuine capacity overflow (so we don't compact-loop on a body that simply doesn't fit).
    frees_since_reset: u32,
}

struct Shelf {
    z_start: u32,
    z_height: u32,
    rows: Vec<Row>,
    y_used: u32,
}

struct Row {
    y_start: u32,
    y_height: u32,
    x_used: u32,
}

#[derive(Clone, Copy)]
struct Allocation {
    x_start: u32,
    y_start: u32,
    z_start: u32,
}

impl ShelfAllocator {
    fn new(atlas_dims: [u32; 3]) -> Self {
        Self {
            atlas_dims,
            shelves: Vec::new(),
            z_used: 0,
            allocations: std::collections::HashMap::new(),
            frees_since_reset: 0,
        }
    }

    fn allocate(&mut self, body_nid: NodeId, padded_size: [u32; 3]) -> Option<(u32, u32, u32)> {
        let [px, py, pz] = padded_size;
        if px > self.atlas_dims[0] || py > self.atlas_dims[1] || pz > self.atlas_dims[2] {
            return None;
        }

        // Try existing shelves — best-fit by Z to minimize waste
        let mut best: Option<(usize, u32)> = None;
        for (si, shelf) in self.shelves.iter().enumerate() {
            if shelf.z_height >= pz {
                let waste = shelf.z_height - pz;
                if best.map_or(true, |(_, bw)| waste < bw) {
                    best = Some((si, waste));
                }
            }
        }

        if let Some((si, _)) = best {
            if let Some((x, y)) = self.try_fit_in_shelf(si, px, py) {
                let z = self.shelves[si].z_start;
                let alloc = Allocation { x_start: x, y_start: y, z_start: z };
                debug_assert!(
                    x + px <= self.atlas_dims[0] && y + py <= self.atlas_dims[1] && z + pz <= self.atlas_dims[2],
                    "Atlas allocation out of bounds: ({},{},{}) + ({},{},{}) > ({},{},{})",
                    x, y, z, px, py, pz, self.atlas_dims[0], self.atlas_dims[1], self.atlas_dims[2]
                );
                self.allocations.insert(body_nid, alloc);
                return Some((x, y, z));
            }
        }

        // New shelf
        if self.z_used + pz > self.atlas_dims[2] {
            return None;
        }
        let si = self.shelves.len();
        self.shelves.push(Shelf {
            z_start: self.z_used,
            z_height: pz,
            rows: Vec::new(),
            y_used: 0,
        });
        self.z_used += pz;

        if let Some((x, y)) = self.try_fit_in_shelf(si, px, py) {
            let z = self.shelves[si].z_start;
            let alloc = Allocation { x_start: x, y_start: y, z_start: z };
            self.allocations.insert(body_nid, alloc);
            Some((x, y, z))
        } else {
            None
        }
    }

    fn try_fit_in_shelf(&mut self, si: usize, px: u32, py: u32) -> Option<(u32, u32)> {
        let atlas_x = self.atlas_dims[0];
        let atlas_y = self.atlas_dims[1];
        let shelf = &mut self.shelves[si];

        // Try existing rows
        for row in &mut shelf.rows {
            if row.y_height >= py && row.x_used + px <= atlas_x {
                let x = row.x_used;
                let y = row.y_start;
                row.x_used += px;
                return Some((x, y));
            }
        }

        // New row
        if shelf.y_used + py <= atlas_y {
            let y = shelf.y_used;
            shelf.rows.push(Row {
                y_start: y,
                y_height: py,
                x_used: px,
            });
            shelf.y_used += py;
            return Some((0, y));
        }

        None
    }

    fn free(&mut self, body_nid: NodeId) {
        if self.allocations.remove(&body_nid).is_some() {
            self.frees_since_reset = self.frees_since_reset.saturating_add(1);
        }
        // Space becomes a hole — not recovered during normal free.
        // Only trimming: if ALL allocations are now empty, full reset.
        if self.allocations.is_empty() {
            self.shelves.clear();
            self.z_used = 0;
            self.frees_since_reset = 0; // empty → no holes
        }
    }

    fn body_count(&self) -> usize { self.allocations.len() }

    fn clear(&mut self) {
        self.shelves.clear();
        self.z_used = 0;
        self.allocations.clear();
        self.frees_since_reset = 0;
    }

    // Keep backward-compat cell_size accessor — returns 0 (variable cells, no fixed size)
    fn cell_size_compat(&self) -> u32 { 0 }
}

// ============================================================
// Volume Atlas
// ============================================================

/// Single 3D texture atlas holding all bodies in a grid of fixed-size cells.
pub struct VolumeAtlas {
    /// The atlas 3D texture (R32Float, dims[0] × dims[1] × dims[2]).
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    /// Atlas texture dimensions [width, height, depth].
    pub dims: [u32; 3],
    /// Per-body slots (indexed by position, not NodeId).
    pub slots: Vec<AtlasSlot>,
    /// Variable-size shelf allocator for cell assignment.
    grid: ShelfAllocator,
    /// Monotonic generation counter — assigned to each new atlas slot allocation.
    next_generation: u64,
    /// Per-body GPU params for the VP shader.
    pub body_params: Vec<GpuBodyAtlasParams>,
    /// GPU buffer for body_params (uploaded each frame when dirty).
    pub body_params_buffer: wgpu::Buffer,
    /// True when body_params changed and needs GPU re-upload.
    pub params_dirty: bool,
    /// Set by the bake dispatch when an allocation FAILS while reclaimable holes exist — a signal to
    /// poll to compact (reclaim holes + rebake all). Distinguished from a genuine capacity overflow
    /// (no holes) so we never compact-loop. Cleared by `compact()`.
    pub request_compact: bool,
    /// Monotonic frame counter for LRU tracking. Incremented once per frame.
    pub frame_counter: u64,
    /// Incremented whenever `self.texture`/`self.view` are recreated (rebuild).
    /// Callers compare this against their cached value to detect stale bind groups.
    pub texture_generation: u64,
    /// Maps body NodeId → source_hash for instancing.
    /// When rebuild_params encounters a body without its own slot,
    /// it uses this hash to find another slot with the same content.
    pub body_hashes: std::collections::HashMap<NodeId, u64>,
    /// Last time a resolution change triggered a free+realloc.
    /// Used for hysteresis to prevent thrashing during rapid resolution toggles.
    last_resolution_change: Option<std::time::Instant>,
}

impl VolumeAtlas {
    /// Create a new atlas with the given dimensions and cell size.
    ///
    /// `dims`: [width, height, depth] of the atlas texture. Each dimension
    /// is clamped to [cell_size, MAX_TEXTURE_DIM]. For RTX 4090 class (24GB),
    /// use [1024, 1024, 1024] (4GB). For smaller GPUs, [512, 512, 512] (512MB).
    ///
    /// `cell_size`: size of each cell in texels per axis (default 132 = 128 + 2*PAD).
    /// All cells are cubic cell_size³. Bodies with smaller resolution than
    /// cell_size use a sub-region — UVW scaling handles the mapping.
    pub fn new(device: &wgpu::Device, dims: [u32; 3], cell_size: u32) -> Self {
        let dims = [
            dims[0].clamp(cell_size, MAX_TEXTURE_DIM),
            dims[1].clamp(cell_size, MAX_TEXTURE_DIM),
            dims[2].clamp(cell_size, MAX_TEXTURE_DIM),
        ];

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("volume_atlas_3d"),
            size: wgpu::Extent3d {
                width: dims[0],
                height: dims[1],
                depth_or_array_layers: dims[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                 | wgpu::TextureUsages::COPY_DST
                 | wgpu::TextureUsages::COPY_SRC
                 | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("volume_atlas_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let body_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas_body_params"),
            size: (MAX_ATLAS_BODIES * std::mem::size_of::<GpuBodyAtlasParams>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let grid = ShelfAllocator::new(dims);

        eprintln!("[VolumeAtlas] created {}x{}x{} variable-cell ({:.0} MB VRAM)",
            dims[0], dims[1], dims[2],
            (dims[0] as f64 * dims[1] as f64 * dims[2] as f64 * 4.0) / (1024.0 * 1024.0));

        Self {
            texture,
            view,
            sampler,
            dims,
            slots: Vec::new(),
            grid,
            next_generation: 1,
            body_params: Vec::new(),
            body_params_buffer,
            params_dirty: true,
            request_compact: false,
            frame_counter: 0,
            texture_generation: 0,
            body_hashes: std::collections::HashMap::new(),
            last_resolution_change: None,
        }
    }

    /// Auto-size atlas based on GPU capabilities.
    /// Uses adapter info to estimate VRAM and pick the largest atlas that
    /// uses at most 25% of total VRAM (R32Float = 4 bytes/voxel).
    ///
    /// | GPU VRAM  | Atlas    | Cells at 132³ | R32Float |
    /// |-----------|----------|---------------|----------|
    /// | 4-6 GB    | 512³     | 27            | 512 MB   |
    /// | 8-12 GB   | 768³     | 125           | 1.7 GB   |
    /// | 16+ GB    | 1024³    | 343           | 4 GB     |
    /// | 24+ GB    | 1024³    | 343           | 4 GB     |
    /// Auto-size atlas based on GPU VRAM.
    /// Uses 25% of VRAM (max 4GB) for the atlas.
    ///
    /// | GPU VRAM  | Atlas    | Cells at 132³ | R32Float |
    /// |-----------|----------|---------------|----------|
    /// | 4 GB      | 396³     | 27            | 237 MB   |
    /// | 8 GB      | 528³     | 64            | 562 MB   |
    /// | 16 GB     | 660³     | 125           | 1.1 GB   |
    /// | 24 GB     | 792³     | 216           | 1.9 GB   |
    pub fn new_auto(device: &wgpu::Device, vram_bytes: u64, _cell_size: u32) -> Self {
        let vram = if vram_bytes > 0 { vram_bytes } else { 4 * 1024 * 1024 * 1024_u64 };

        // Budget: 25% of VRAM, max 2GB (avoids OOM on large GPUs)
        let budget = (vram / 4).min(2 * 1024 * 1024 * 1024);
        // Cube root to get side length (R32Float = 4 bytes/voxel)
        let dim = ((budget / 4) as f64).cbrt() as u32;
        let dim = dim.clamp(256, MAX_TEXTURE_DIM);

        eprintln!("[VolumeAtlas] auto-size: VRAM={}MB budget={}MB → {}³",
            vram / (1024 * 1024),
            budget / (1024 * 1024),
            dim);

        Self::new(device, [dim, dim, dim], _cell_size)
    }

    // ============================================================
    // Allocation / Deallocation
    // ============================================================

    /// Allocate a cell for a body. Returns slot index, or None if atlas is full.
    /// `resolution` is per-axis [x, y, z]. Each axis + 2*ATLAS_PAD must fit
    /// within the cell size.
    ///
    /// Does NOT clear the sub-region — bakes fully overwrite (Constraint C3).
    pub fn allocate(&mut self, body_nid: NodeId, resolution: [u32; 3]) -> Option<usize> {
        // Check if body already has a slot
        if let Some(idx) = self.slot_index(body_nid) {
            let slot_res = self.slots[idx].resolution;
            if slot_res == resolution {
                return Some(idx); // reuse existing slot
            }
            // Resolution changed. The anti-thrash cooldown (rapid resolution-button toggling) may
            // defer a SHRINK or an equal-size churn — but it must NEVER defer a GROW. Returning the
            // smaller stale slot for a larger request is a CORRECTNESS bug, not a perf trade: the bake
            // then writes only a sub-block of the larger output buffer (a 64³ write into a 128³ grid =
            // 1/8 filled = the "thin slab"/flat-slab, and the readback stores that under-filled grid as
            // the full resolution — AZRA_PROBE fingerprint 0.9% ≈ 7.3%/8 = 64³/128³). The slot must never
            // be SMALLER than the bake it backs — the exact invariant the app-side `stale_atlas_slot`
            // guard already enforces for the opposite (slot-larger) direction. So: cooldown only when NOT
            // growing.
            let grow = resolution[0] > slot_res[0]
                || resolution[1] > slot_res[1]
                || resolution[2] > slot_res[2];
            if !grow {
                if let Some(last) = self.last_resolution_change {
                    if last.elapsed().as_millis() < RESOLUTION_CHANGE_COOLDOWN_MS {
                        return Some(idx); // keep current (≥ requested) resolution during cooldown
                    }
                }
            }
            // Grow, or cooldown passed — free old slot and reallocate at the new size.
            self.last_resolution_change = Some(std::time::Instant::now());
            self.free(body_nid);
        }

        if self.slots.len() >= MAX_ATLAS_BODIES {
            return None; // too many bodies
        }

        // Per-axis padded dimensions. Clamp to atlas dims so bodies always fit.
        let max_res = [
            self.dims[0].saturating_sub(2 * ATLAS_PAD),
            self.dims[1].saturating_sub(2 * ATLAS_PAD),
            self.dims[2].saturating_sub(2 * ATLAS_PAD),
        ];
        let resolution = [
            resolution[0].min(max_res[0]),
            resolution[1].min(max_res[1]),
            resolution[2].min(max_res[2]),
        ];
        let padded_size = [
            resolution[0] + 2 * ATLAS_PAD,
            resolution[1] + 2 * ATLAS_PAD,
            resolution[2] + 2 * ATLAS_PAD,
        ];

        // Allocate a variable-size region from the shelf allocator
        let (x_start, y_start, z_start) = self.grid.allocate(body_nid, padded_size)?;

        let gen = self.next_generation;
        self.next_generation += 1;
        let slot = AtlasSlot {
            body_nid,
            x_start,
            y_start,
            z_start,
            padded_size,
            resolution,
            source_hash: 0,
            template_hash: 0,
            bake_bounds_min: Vec3::ZERO,
            bake_bounds_max: Vec3::ONE,
            bake_transform: BodyTransform::default(),
            valid: false,
            generation: gen,
            last_used_frame: self.frame_counter,
        };
        self.slots.push(slot);
        self.params_dirty = true;
        Some(self.slots.len() - 1)
    }

    /// Compact the atlas: reset the (append-only, hole-leaking) shelf allocator and clear ALL slots,
    /// returning the body NodeIds that had valid data so the caller can mark them dirty for re-bake.
    /// The 3D texture is NOT recreated — its stale data is harmless because a freshly-allocated slot is
    /// `valid:false` and `rebuild_params` skips it until its re-bake completes (so nothing reads a stale
    /// cell). Use this on a resolution change to repack every body into a hole-free atlas, fixing the
    /// cross-body corruption where leaked holes starve the atlas and orphan other bodies' slots.
    pub fn compact(&mut self) -> Vec<NodeId> {
        let bodies: Vec<NodeId> = self.slots.iter().filter(|s| s.valid).map(|s| s.body_nid).collect();
        self.grid.clear();
        self.slots.clear();
        self.body_params.clear();
        self.params_dirty = true;
        self.request_compact = false;
        bodies
    }

    /// True when freed-but-unreclaimed holes exist (the append-only allocator never reclaims on free).
    /// On an allocation failure, the caller compacts ONLY if this is true — otherwise the failure is a
    /// genuine capacity overflow (a body that doesn't fit even in a fresh atlas) and compacting would
    /// just loop.
    pub fn has_reclaimable_holes(&self) -> bool {
        self.grid.frees_since_reset > 0
    }

    /// Free a body's slot. Returns the cell to the grid allocator.
    pub fn free(&mut self, body_nid: NodeId) {
        if let Some(idx) = self.slot_index(body_nid) {
            self.slots.remove(idx);
            self.grid.free(body_nid);
            self.params_dirty = true;
        }
    }

    /// Recreate the atlas texture at new dimensions. All existing baked data
    /// is lost — caller must mark all bodies dirty for rebake.
    /// Returns the list of body NodeIds that had valid slots (need rebake).
    /// Bumps texture_generation so callers can detect stale bind groups.
    pub fn rebuild(&mut self, device: &wgpu::Device, new_dims: [u32; 3], cell_size: u32) -> Vec<NodeId> {
        let new_dims = [
            new_dims[0].clamp(cell_size, MAX_TEXTURE_DIM),
            new_dims[1].clamp(cell_size, MAX_TEXTURE_DIM),
            new_dims[2].clamp(cell_size, MAX_TEXTURE_DIM),
        ];

        // Collect bodies that had valid data (caller must rebake these)
        let evicted: Vec<NodeId> = self.slots.iter()
            .filter(|s| s.valid)
            .map(|s| s.body_nid)
            .collect();

        self.texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("volume_atlas_3d"),
            size: wgpu::Extent3d {
                width: new_dims[0],
                height: new_dims[1],
                depth_or_array_layers: new_dims[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                 | wgpu::TextureUsages::COPY_DST
                 | wgpu::TextureUsages::COPY_SRC
                 | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        self.view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.dims = new_dims;
        self.texture_generation += 1; // bind groups referencing old texture are now stale

        // Rebuild shelf allocator for new dimensions
        self.grid = ShelfAllocator::new(new_dims);

        // Clear all slots — data is gone
        self.slots.clear();
        self.body_params.clear();
        self.params_dirty = true;

        eprintln!("[VolumeAtlas] rebuilt to {}x{}x{} variable-cell ({} bodies evicted, tex_gen={})",
            new_dims[0], new_dims[1], new_dims[2],
            evicted.len(), self.texture_generation);
        evicted
    }

    /// Try to allocate a slot. If atlas is full, evict LRU as last resort.
    /// Returns (slot_index, Vec<evicted_body_nids>), or None if truly at capacity.
    pub fn allocate_or_evict(
        &mut self,
        body_nid: NodeId,
        resolution: [u32; 3],
    ) -> Option<(usize, Vec<NodeId>)> {
        // Try normal allocate first
        if let Some(idx) = self.allocate(body_nid, resolution) {
            return Some((idx, Vec::new()));
        }

        // Atlas full — evict LRU as last resort
        if let Some(evicted) = self.evict_lru() {
            if let Some(idx) = self.allocate(body_nid, resolution) {
                return Some((idx, vec![evicted]));
            }
        }
        None
    }

    // ============================================================
    // Slot Queries
    // ============================================================

    /// Find the slot index for a body.
    pub fn slot_index(&self, body_nid: NodeId) -> Option<usize> {
        self.slots.iter().position(|s| s.body_nid == body_nid)
    }

    /// Get a slot by body NodeId.
    pub fn slot(&self, body_nid: NodeId) -> Option<&AtlasSlot> {
        self.slots.iter().find(|s| s.body_nid == body_nid)
    }

    /// Get a mutable slot by body NodeId.
    pub fn slot_mut(&mut self, body_nid: NodeId) -> Option<&mut AtlasSlot> {
        self.slots.iter_mut().find(|s| s.body_nid == body_nid)
    }

    /// Get the atlas generation for a body's slot (for stale bake detection).
    pub fn slot_generation(&self, body_nid: NodeId) -> Option<u64> {
        self.slot(body_nid).map(|s| s.generation)
    }

    /// Number of bodies currently in the atlas.
    pub fn body_count(&self) -> usize {
        self.slots.len()
    }

    /// Get the 3D atlas offset for a body (for BakeParams).
    /// Returns [x_start, y_start, z_start] in texels.
    pub fn body_atlas_offset(&self, body_nid: NodeId) -> Option<[u32; 3]> {
        self.slot(body_nid).map(|s| [s.x_start, s.y_start, s.z_start])
    }

    /// Get the atlas Z-start for a body (backward compat for bake shader offset).
    /// For full 3D offset, use `body_atlas_offset()`.
    pub fn body_z_start(&self, body_nid: NodeId) -> Option<u32> {
        self.slot(body_nid).map(|s| s.z_start)
    }

    /// Grid allocator capacity (total cells).
    pub fn grid_capacity(&self) -> usize {
        MAX_ATLAS_BODIES
    }

    /// Number of bodies currently allocated.
    pub fn grid_used(&self) -> usize {
        self.grid.body_count()
    }

    /// Cell size — returns 0 for variable-cell allocator (backward compat).
    pub fn cell_size(&self) -> u32 {
        self.grid.cell_size_compat()
    }

    /// Find a valid slot with a matching template_hash (for instancing).
    /// Returns the slot if found — the caller can reuse its atlas cell.
    pub fn find_slot_by_hash(&self, template_hash: u64) -> Option<&AtlasSlot> {
        self.slots.iter().find(|s| s.valid && s.template_hash == template_hash)
    }

    // ============================================================
    // GPU Params (per-body data for VP shader)
    // ============================================================

    /// Rebuild the GPU body params array from current slots + scene graph transforms.
    /// Call when slots change or body transforms change.
    ///
    /// `frustum`: when provided, Union bodies whose world AABB is entirely outside
    /// the frustum are culled (not uploaded). Subtract/Intersect bodies are NEVER
    /// culled — they can affect visible union bodies even when offscreen.
    ///
    /// `body_order` maps body NodeId → scene-graph order index (0-based).
    /// This is the same encoding used by `selected_body_index` and `gbuf_motion.r`.
    pub fn rebuild_params(
        &mut self,
        scene_graph: &core_math::scene_graph::SceneGraph,
    ) {
        self.rebuild_params_culled(scene_graph, None);
    }

    /// Rebuild with optional frustum culling.
    /// Iterates ALL scene-graph bodies and emits params for each.
    /// Instancing: if body B has the same source_hash as body A's slot,
    /// body B gets params pointing to body A's atlas cell (shared UVW).
    pub fn rebuild_params_culled(
        &mut self,
        scene_graph: &core_math::scene_graph::SceneGraph,
        frustum: Option<&FrustumPlanes>,
    ) {
        self.body_params.clear();
        let dims_f = [self.dims[0] as f32, self.dims[1] as f32, self.dims[2] as f32];

        // Build body order map: iterate SdfBody nodes in scene-graph order.
        let mut body_order = std::collections::HashMap::new();
        for (idx, (nid, _)) in scene_graph.sdf_bodies().enumerate() {
            body_order.insert(nid, idx as u32);
        }

        // Iterate ALL scene-graph bodies (not just slots).
        // For each body, find its atlas data: own slot OR instanced from another body's slot.
        let all_bodies: Vec<_> = scene_graph.sdf_bodies().map(|(nid, _)| nid).collect();

        for &body_nid in &all_bodies {
            // Find atlas data for this body: own slot first, then hash-match (instancing).
            // Instancing: if this body has the same source_hash as another body's slot,
            // reuse that slot's UVW mapping. Zero extra atlas cells needed.
            let slot = self.slot(body_nid)
                .filter(|s| s.valid)
                .or_else(|| {
                    let hash = self.body_hashes.get(&body_nid).copied().unwrap_or(0);
                    if hash != 0 { self.find_slot_by_hash(hash) } else { None }
                });
            let slot = match slot {
                Some(s) => s,
                None => continue, // no data at all — body not yet baked
            };

            // Transform bake bounds to world space using THIS body's transform
            // (not the slot-owning body's transform — they may be at different positions)
            let (world_min, world_max) = scene_graph.body_world_transform(body_nid)
                .map(|bt| bt.local_aabb_to_world(slot.bake_bounds_min, slot.bake_bounds_max))
                .unwrap_or((slot.bake_bounds_min, slot.bake_bounds_max));

            // Frustum culling
            if let Some(frustum) = frustum {
                let is_union = scene_graph.body_op(body_nid)
                    .map(|op| op.is_additive())
                    .unwrap_or(true);
                if is_union && frustum.aabb_outside(world_min, world_max) {
                    continue;
                }
            }

            // UVW from the SLOT's atlas cell (shared for instances)
            let pad_f = ATLAS_PAD as f32;
            let res_x = slot.resolution[0] as f32;
            let res_y = slot.resolution[1] as f32;
            let res_z = slot.resolution[2] as f32;
            let x_scale = res_x / dims_f[0];
            let x_offset = (slot.x_start as f32 + pad_f) / dims_f[0];
            let y_scale = res_y / dims_f[1];
            let y_offset = (slot.y_start as f32 + pad_f) / dims_f[1];
            let z_scale = res_z / dims_f[2];
            let z_offset = (slot.z_start as f32 + pad_f) / dims_f[2];

            let max_res = slot.resolution[0].max(slot.resolution[1]).max(slot.resolution[2]);

            // body_op from THIS body (not the slot-owning body)
            let (op_code, blend_k) = scene_graph.body_op(body_nid)
                .map(|op| match op {
                    BooleanOp::Union { k } => (0.0_f32, *k),
                    BooleanOp::Subtract { k } => (1.0_f32, *k),
                    BooleanOp::Intersect => (2.0_f32, 0.0_f32),
                    BooleanOp::SmoothIntersect { k } => (2.0_f32, *k),
                    BooleanOp::ChamferUnion { k } => (0.0_f32, *k),
                    BooleanOp::ChamferSubtract { k } => (1.0_f32, *k),
                    // Legacy sculpt-era atlas composite has only op codes 0/1/2 — no seam
                    // or extrude dispatch. Degrade to hard union so the body still composites;
                    // the production seam/extrude path is the volume bake (volume_bake.wgsl).
                    BooleanOp::Seam { .. } => (0.0_f32, 0.0_f32),
                    BooleanOp::Extrude { .. } => (0.0_f32, 0.0_f32),
                })
                .unwrap_or((0.0, 0.0));

            self.body_params.push(GpuBodyAtlasParams {
                aabb_min: [world_min.x, world_min.y, world_min.z],
                body_op: op_code,
                aabb_max: [world_max.x, world_max.y, world_max.z],
                blend_k,
                uvw_offset: [x_offset, y_offset, z_offset],
                body_index: body_order.get(&body_nid).copied().unwrap_or(0),
                uvw_scale: [x_scale, y_scale, z_scale],
                resolution: max_res,
            });
        }
        // LRU: mark all valid slots as used this frame.
        // More precise tracking (only frustum-visible) is a future optimization.
        for slot in self.slots.iter_mut() {
            if slot.valid { slot.last_used_frame = self.frame_counter; }
        }
        self.params_dirty = true;
    }

    /// Upload body params to GPU buffer if dirty.
    /// Writes the full MAX_ATLAS_BODIES array with zero-padding.
    /// VP shader reads atlas_body_count from GpuUniforms to know valid count.
    pub fn upload_params(&mut self, queue: &wgpu::Queue) {
        if !self.params_dirty { return; }
        // Heap allocation — 256 * 64 = 16KB, avoids stack overflow on constrained platforms
        let mut buf = vec![GpuBodyAtlasParams::zeroed(); MAX_ATLAS_BODIES];
        for (i, p) in self.body_params.iter().enumerate() {
            if i >= MAX_ATLAS_BODIES { break; }
            buf[i] = *p;
        }
        queue.write_buffer(
            &self.body_params_buffer,
            0,
            bytemuck::cast_slice(&buf),
        );
        self.params_dirty = false;
    }

    // ============================================================
    // Grid Data Upload
    // ============================================================

    /// Upload a body's baked grid data to the atlas texture at the correct 3D offset.
    /// `data` is the full grid (res_x * res_y * res_z f32s) from body_grids.
    pub fn upload_body_grid(
        &mut self,
        queue: &wgpu::Queue,
        body_nid: NodeId,
        data: &[f32],
        bake_bounds_min: Vec3,
        bake_bounds_max: Vec3,
    ) -> bool {
        let slot_idx = match self.slot_index(body_nid) {
            Some(idx) => idx,
            None => return false,
        };
        let slot = &self.slots[slot_idx];
        let [res_x, res_y, res_z] = slot.resolution;
        let expected = (res_x as usize) * (res_y as usize) * (res_z as usize);
        if data.len() < expected { return false; }

        // Write grid data inside the padded cell.
        // Body data sits at atlas origin (x_start + PAD, y_start + PAD, z_start + PAD).
        // Padding texels around the body data are filled by the bake shader's
        // direct-to-texture writes. This upload path is for CPU-baked grids
        // which don't include padding — padding texels keep their initial
        // value until the next GPU bake fills them.
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: slot.x_start + ATLAS_PAD,
                    y: slot.y_start + ATLAS_PAD,
                    z: slot.z_start + ATLAS_PAD,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data[..expected]),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(res_x * 4), // R32Float = 4 bytes per voxel
                rows_per_image: Some(res_y),
            },
            wgpu::Extent3d {
                width: res_x,
                height: res_y,
                depth_or_array_layers: res_z,
            },
        );

        // Update slot bounds
        let slot = &mut self.slots[slot_idx];
        slot.bake_bounds_min = bake_bounds_min;
        slot.bake_bounds_max = bake_bounds_max;
        slot.valid = true;
        self.params_dirty = true;
        true
    }

    // ============================================================
    // LRU Eviction
    // ============================================================

    /// Advance the frame counter. Call once per frame.
    pub fn advance_frame(&mut self) {
        self.frame_counter += 1;
    }

    /// Find the least-recently-used slot. Returns the slot index of the body
    /// with the smallest `last_used_frame` that is valid.
    /// Returns None if no evictable slots exist.
    fn lru_slot_index(&self) -> Option<usize> {
        self.slots.iter().enumerate()
            .filter(|(_, s)| s.valid)
            .min_by_key(|(_, s)| s.last_used_frame)
            .map(|(i, _)| i)
    }

    /// Evict the least-recently-used body from the atlas.
    /// Returns the evicted body's NodeId so the caller can cache its grid data.
    /// The evicted slot is freed (cell reclaimed).
    pub fn evict_lru(&mut self) -> Option<NodeId> {
        let idx = self.lru_slot_index()?;
        let nid = self.slots[idx].body_nid;
        self.free(nid);
        eprintln!("[Atlas LRU] evicted body {:?} (frame {})", nid, self.frame_counter);
        Some(nid)
    }

    // ============================================================
    // Clear / Reset
    // ============================================================

    /// Clear all slots (e.g., on project load/new).
    pub fn clear(&mut self) {
        self.slots.clear();
        self.grid.clear();
        self.body_params.clear();
        self.params_dirty = true;
    }
}
