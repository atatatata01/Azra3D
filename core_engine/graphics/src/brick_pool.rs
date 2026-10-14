//! Sparse Brick Pool — fixed-size brick allocator for SDF volume caching.
//!
//! Each brick is 8³ = 512 f32 values (2,048 bytes). The pool is a single large
//! SSBO that holds all bricks for all bodies. An indirection table per body maps
//! brick grid coordinates → pool slot indices.
//!
//! Phase 1: CPU-only data structures. GPU buffer created but not written.
//! Phase 2+: GPU brick copy/fill shaders write directly to the pool SSBO.

use std::collections::HashMap;
use core_math::scene_graph::NodeId;

// ============================================================
// Constants
// ============================================================

/// Voxels per brick axis (8³ = 512 voxels per brick).
pub const BRICK_DIM: u32 = 8;
/// Total voxels per brick.
pub const BRICK_VOXELS: u32 = BRICK_DIM * BRICK_DIM * BRICK_DIM; // 512
/// Bytes per brick (512 f32 × 4 bytes).
pub const BRICK_BYTES: u32 = BRICK_VOXELS * 4; // 2,048
/// Sentinel value for empty indirection entries.
pub const BRICK_EMPTY: u32 = 0xFFFF_FFFF;

// ============================================================
// BrickPool
// ============================================================

/// Fixed-size brick allocator backed by a single GPU SSBO.
///
/// All bricks are the same size (8³ f32), so allocation is O(1) via free list.
/// No fragmentation possible. Bodies own groups of bricks tracked by
/// `body_bricks` — deallocating a body returns all its bricks at once.
pub struct BrickPool {
    /// GPU SSBO holding all brick data. `None` until `init_gpu()`.
    buffer: Option<wgpu::Buffer>,
    /// Total brick capacity (pool_budget / BRICK_BYTES).
    capacity: u32,
    /// Free list: available pool slot indices (LIFO stack).
    free_list: Vec<u32>,
    /// Per-body: set of pool slot indices owned by that body.
    body_bricks: HashMap<NodeId, Vec<u32>>,
    /// LRU frame counter per body.
    body_last_used: HashMap<NodeId, u64>,
    /// Number of bricks currently allocated.
    used_bricks: u32,
    /// Total budget in bytes.
    budget_bytes: usize,
}

impl BrickPool {
    /// Create a new brick pool with the given memory budget.
    ///
    /// Recommended budgets: 128 MB (65K bricks), 256 MB (131K), 512 MB (262K).
    /// GPU buffer is NOT allocated here — call `init_gpu()` with a device.
    pub fn new(budget_bytes: usize) -> Self {
        let capacity = (budget_bytes / BRICK_BYTES as usize) as u32;
        // Pre-populate free list (all slots free initially)
        let free_list: Vec<u32> = (0..capacity).rev().collect(); // reversed so pop() gives 0,1,2,...
        Self {
            buffer: None,
            capacity,
            free_list,
            body_bricks: HashMap::new(),
            body_last_used: HashMap::new(),
            used_bricks: 0,
            budget_bytes,
        }
    }

    /// Allocate `n` brick slots from the pool.
    ///
    /// Returns `None` if not enough free bricks. Caller must evict first.
    /// Returned slots are contiguous in the free list but NOT necessarily
    /// contiguous in the pool buffer — that's fine for indirection-based access.
    pub fn allocate(&mut self, body_nid: NodeId, n: u32) -> Option<Vec<u32>> {
        if n == 0 { return Some(Vec::new()); }
        if self.free_list.len() < n as usize { return None; }
        let mut slots = Vec::with_capacity(n as usize);
        for _ in 0..n {
            slots.push(self.free_list.pop().unwrap());
        }
        self.used_bricks += n;
        self.body_bricks.entry(body_nid).or_default().extend_from_slice(&slots);
        Some(slots)
    }

    /// Deallocate ALL bricks owned by a body, returning them to the free list.
    pub fn deallocate_body(&mut self, body_nid: NodeId) {
        if let Some(slots) = self.body_bricks.remove(&body_nid) {
            let count = slots.len() as u32;
            for slot in slots {
                self.free_list.push(slot);
            }
            self.used_bricks = self.used_bricks.saturating_sub(count);
        }
        self.body_last_used.remove(&body_nid);
    }

    /// Evict the least-recently-used body (excluding `protect`).
    ///
    /// Returns the evicted body's NodeId, or `None` if no evictable body exists.
    /// The evicted body's bricks are returned to the free list.
    pub fn evict_lru(&mut self, protect: NodeId) -> Option<NodeId> {
        let evict_nid = self.body_last_used.iter()
            .filter(|(&nid, _)| nid != protect)
            .min_by_key(|(_, &frame)| frame)
            .map(|(&nid, _)| nid)?;
        self.deallocate_body(evict_nid);
        Some(evict_nid)
    }

    /// Update LRU timestamp for a body.
    pub fn touch(&mut self, body_nid: NodeId, frame: u64) {
        self.body_last_used.insert(body_nid, frame);
    }

    /// Pool utilization as a fraction [0.0, 1.0].
    pub fn utilization(&self) -> f32 {
        if self.capacity == 0 { return 0.0; }
        self.used_bricks as f32 / self.capacity as f32
    }

    /// Number of bricks currently allocated.
    pub fn used_bricks(&self) -> u32 { self.used_bricks }

    /// Total brick capacity.
    pub fn capacity(&self) -> u32 { self.capacity }

    /// Number of free bricks available.
    pub fn free_bricks(&self) -> u32 { self.free_list.len() as u32 }

    /// Number of bricks owned by a specific body.
    pub fn body_brick_count(&self, body_nid: NodeId) -> u32 {
        self.body_bricks.get(&body_nid).map_or(0, |v| v.len() as u32)
    }

    /// All bodies currently in the pool.
    pub fn resident_bodies(&self) -> Vec<NodeId> {
        self.body_bricks.keys().copied().collect()
    }

    /// Budget in bytes.
    pub fn budget_bytes(&self) -> usize { self.budget_bytes }

    /// Reference to the GPU buffer, if initialized.
    pub fn buffer(&self) -> Option<&wgpu::Buffer> { self.buffer.as_ref() }

    // ============================================================
    // GPU
    // ============================================================

    /// Create the GPU SSBO. Call once after device is available.
    pub fn init_gpu(&mut self, device: &wgpu::Device) {
        if self.buffer.is_some() { return; } // already initialized
        let size = self.capacity as u64 * BRICK_BYTES as u64;
        if size == 0 { return; }
        self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("brick_pool_ssbo"),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
    }

    /// Allocate bricks for a body, evicting LRU bodies if needed.
    ///
    /// Returns `(allocated_slots, evicted_body_nids)`, or `None` if even after
    /// eviction there aren't enough free slots (budget too small for this body).
    /// The caller MUST clean up stale `BodyBrickVolume` entries for evicted bodies.
    pub fn allocate_with_eviction(
        &mut self,
        body_nid: NodeId,
        n: u32,
        protect: NodeId,
    ) -> Option<(Vec<u32>, Vec<NodeId>)> {
        // Try direct allocation first
        if let Some(slots) = self.allocate(body_nid, n) {
            return Some((slots, Vec::new()));
        }
        // Evict until we have enough space
        let mut evicted = Vec::new();
        while self.free_bricks() < n {
            if let Some(evicted_nid) = self.evict_lru(protect) {
                evicted.push(evicted_nid);
            } else {
                return None; // no more bodies to evict
            }
        }
        self.allocate(body_nid, n).map(|slots| (slots, evicted))
    }
}

// ============================================================
// BodyBrickVolume — per-body brick indirection
// ============================================================

/// Per-body sparse volume stored as bricks in the shared BrickPool.
///
/// The indirection table maps brick-grid coordinates to pool slot indices.
/// Empty bricks (far from surface) have `BRICK_EMPTY` in the indirection.
pub struct BodyBrickVolume {
    /// Body this volume belongs to.
    pub body_nid: NodeId,
    /// Brick grid dimensions (e.g., [16, 16, 16] for 128³ effective resolution).
    pub dims_bricks: [u32; 3],
    /// World-space AABB.
    pub aabb_min: glam::Vec3,
    pub aabb_max: glam::Vec3,
    /// Flattened indirection table: `dims_bricks.x * y * z` entries.
    /// Each entry = pool slot index or `BRICK_EMPTY`.
    pub indirection: Vec<u32>,
    /// GPU buffer for indirection upload. `None` until first GPU use.
    pub indirection_buffer: Option<wgpu::Buffer>,
    /// Source hash for change detection (matches BodyCacheEntry.source_hash).
    pub source_hash: u64,
    /// Effective resolution = dims_bricks × BRICK_DIM.
    pub effective_resolution: [u32; 3],
    /// CPU-side brick voxel data — dense packed, indexed as
    /// `cpu_slot_map[pool_slot] * BRICK_VOXELS + local_idx`.
    /// Memory is proportional to num_active_bricks, NOT max_pool_slot.
    pub cpu_data: Vec<f32>,
    /// Maps pool slot → dense local brick index into `cpu_data`.
    pub cpu_slot_map: HashMap<u32, usize>,
}

impl BodyBrickVolume {
    /// Create a new empty brick volume for a body.
    pub fn new(body_nid: NodeId, dims_bricks: [u32; 3], aabb_min: glam::Vec3, aabb_max: glam::Vec3) -> Self {
        let total = (dims_bricks[0] * dims_bricks[1] * dims_bricks[2]) as usize;
        Self {
            body_nid,
            dims_bricks,
            aabb_min,
            aabb_max,
            indirection: vec![BRICK_EMPTY; total],
            indirection_buffer: None,
            source_hash: 0,
            effective_resolution: [
                dims_bricks[0] * BRICK_DIM,
                dims_bricks[1] * BRICK_DIM,
                dims_bricks[2] * BRICK_DIM,
            ],
            cpu_data: Vec::new(),
            cpu_slot_map: HashMap::new(),
        }
    }

    /// Total number of bricks in the grid (including empty).
    pub fn total_bricks(&self) -> u32 {
        self.dims_bricks[0] * self.dims_bricks[1] * self.dims_bricks[2]
    }

    /// Number of active (non-empty) bricks.
    pub fn active_bricks(&self) -> u32 {
        self.indirection.iter().filter(|&&v| v != BRICK_EMPTY).count() as u32
    }

    /// Flat index into the indirection table for a brick coordinate.
    pub fn brick_flat_index(&self, bx: u32, by: u32, bz: u32) -> usize {
        (bx + by * self.dims_bricks[0] + bz * self.dims_bricks[0] * self.dims_bricks[1]) as usize
    }

    /// Set the pool slot for a brick coordinate.
    pub fn set_brick(&mut self, bx: u32, by: u32, bz: u32, pool_slot: u32) {
        let idx = self.brick_flat_index(bx, by, bz);
        self.indirection[idx] = pool_slot;
    }

    /// Clear a brick (mark as empty).
    pub fn clear_brick(&mut self, bx: u32, by: u32, bz: u32) {
        let idx = self.brick_flat_index(bx, by, bz);
        self.indirection[idx] = BRICK_EMPTY;
    }

    /// Upload indirection table to GPU. Creates buffer on first call.
    pub fn upload_indirection(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let data: &[u8] = bytemuck::cast_slice(&self.indirection);
        let buf = self.indirection_buffer.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("brick_indirection"),
                size: data.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        // Recreate if size changed
        if buf.size() < data.len() as u64 {
            *buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("brick_indirection"),
                size: data.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(buf, 0, data);
    }

    /// Compute which brick coordinates a world-space AABB overlaps.
    /// Returns `(min_brick, max_brick)` inclusive ranges.
    pub fn aabb_to_brick_range(&self, aabb_min: glam::Vec3, aabb_max: glam::Vec3) -> ([u32; 3], [u32; 3]) {
        let extent = self.aabb_max - self.aabb_min;
        let inv_extent = glam::Vec3::new(
            if extent.x.abs() > 1e-8 { 1.0 / extent.x } else { 0.0 },
            if extent.y.abs() > 1e-8 { 1.0 / extent.y } else { 0.0 },
            if extent.z.abs() > 1e-8 { 1.0 / extent.z } else { 0.0 },
        );
        let t_min = (aabb_min - self.aabb_min) * inv_extent;
        let t_max = (aabb_max - self.aabb_min) * inv_extent;
        let dims = glam::Vec3::new(
            self.dims_bricks[0] as f32,
            self.dims_bricks[1] as f32,
            self.dims_bricks[2] as f32,
        );
        let bmin = [
            (t_min.x * dims.x).floor().max(0.0) as u32,
            (t_min.y * dims.y).floor().max(0.0) as u32,
            (t_min.z * dims.z).floor().max(0.0) as u32,
        ];
        let bmax = [
            ((t_max.x * dims.x).ceil() as u32).min(self.dims_bricks[0].saturating_sub(1)),
            ((t_max.y * dims.y).ceil() as u32).min(self.dims_bricks[1].saturating_sub(1)),
            ((t_max.z * dims.z).ceil() as u32).min(self.dims_bricks[2].saturating_sub(1)),
        ];
        (bmin, bmax)
    }
}

// ============================================================
// Brick fill helpers (Phase 4 — direct brick bake)
// ============================================================

impl BodyBrickVolume {
    /// Build the sub-brick list for the GPU fill shader dispatch.
    ///
    /// Each 8³ brick is subdivided into 8 sub-bricks (2×2×2 of 4³ each).
    /// Returns a flat `Vec<u32>` with 4 values per sub-brick entry:
    /// `[voxel_origin_x, voxel_origin_y, voxel_origin_z, pool_slot]`.
    ///
    /// Only active (non-empty) bricks are included.
    /// Total entries = active_bricks × 8.
    pub fn build_sub_brick_list(&self) -> Vec<u32> {
        let mut list = Vec::new();
        for bz in 0..self.dims_bricks[2] {
            for by in 0..self.dims_bricks[1] {
                for bx in 0..self.dims_bricks[0] {
                    let idx = self.brick_flat_index(bx, by, bz);
                    let pool_slot = self.indirection[idx];
                    if pool_slot == BRICK_EMPTY { continue; }
                    // 8 sub-bricks (2×2×2) of 4³ voxels each
                    let brick_voxel_base = [bx * BRICK_DIM, by * BRICK_DIM, bz * BRICK_DIM];
                    for sz in 0..2u32 {
                        for sy in 0..2u32 {
                            for sx in 0..2u32 {
                                list.push(brick_voxel_base[0] + sx * 4);
                                list.push(brick_voxel_base[1] + sy * 4);
                                list.push(brick_voxel_base[2] + sz * 4);
                                list.push(pool_slot);
                            }
                        }
                    }
                }
            }
        }
        list
    }

    /// Number of sub-brick entries (for dispatch workgroup count).
    pub fn sub_brick_count(&self) -> u32 {
        self.active_bricks() * 8
    }
}

// ============================================================
// GPU uniform for volume preview shader
// ============================================================

/// Uniform buffer data for the volume preview shader to sample from the brick pool.
/// Must match the WGSL struct `BrickVolumeInfo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BrickVolumeInfo {
    /// Brick grid dimensions (number of bricks per axis).
    pub dims: [u32; 3],
    /// BRICK_DIM (voxels per brick axis, always 8).
    pub brick_dim: u32,
    /// Volume AABB min (world space).
    pub aabb_min: [f32; 3],
    /// 1 = brick sampling enabled, 0 = disabled.
    pub enabled: u32,
    /// Volume AABB max (world space).
    pub aabb_max: [f32; 3],
    /// Total voxels per brick (always 512).
    pub brick_voxels: u32,
}

impl Default for BrickVolumeInfo {
    fn default() -> Self {
        Self {
            dims: [0; 3],
            brick_dim: BRICK_DIM,
            aabb_min: [0.0; 3],
            enabled: 0,
            aabb_max: [0.0; 3],
            brick_voxels: BRICK_VOXELS,
        }
    }
}

impl BrickVolumeInfo {
    /// Build from a BodyBrickVolume (for the active body).
    pub fn from_body_brick_volume(bbv: &BodyBrickVolume) -> Self {
        Self {
            dims: bbv.dims_bricks,
            brick_dim: BRICK_DIM,
            aabb_min: [bbv.aabb_min.x, bbv.aabb_min.y, bbv.aabb_min.z],
            enabled: 1,
            aabb_max: [bbv.aabb_max.x, bbv.aabb_max.y, bbv.aabb_max.z],
            brick_voxels: BRICK_VOXELS,
        }
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_nid(id: u32) -> NodeId { NodeId(id as u64) }

    #[test]
    fn basic_alloc_dealloc() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 100); // 100 bricks
        assert_eq!(pool.capacity(), 100);
        assert_eq!(pool.free_bricks(), 100);
        assert_eq!(pool.used_bricks(), 0);

        let slots = pool.allocate(test_nid(1), 10).unwrap();
        assert_eq!(slots.len(), 10);
        assert_eq!(pool.used_bricks(), 10);
        assert_eq!(pool.free_bricks(), 90);
        assert_eq!(pool.body_brick_count(test_nid(1)), 10);

        pool.deallocate_body(test_nid(1));
        assert_eq!(pool.used_bricks(), 0);
        assert_eq!(pool.free_bricks(), 100);
        assert_eq!(pool.body_brick_count(test_nid(1)), 0);
    }

    #[test]
    fn alloc_fails_when_full() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 10);
        let _s = pool.allocate(test_nid(1), 10).unwrap();
        assert!(pool.allocate(test_nid(2), 1).is_none());
    }

    #[test]
    fn deallocate_body() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 100);
        pool.allocate(test_nid(1), 30).unwrap();
        pool.allocate(test_nid(2), 20).unwrap();
        assert_eq!(pool.used_bricks(), 50);

        pool.deallocate_body(test_nid(1));
        assert_eq!(pool.used_bricks(), 20);
        assert_eq!(pool.body_brick_count(test_nid(1)), 0);
        assert_eq!(pool.body_brick_count(test_nid(2)), 20);
        assert_eq!(pool.free_bricks(), 80);
    }

    #[test]
    fn lru_eviction() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 30);
        pool.allocate(test_nid(1), 10).unwrap();
        pool.touch(test_nid(1), 1);
        pool.allocate(test_nid(2), 10).unwrap();
        pool.touch(test_nid(2), 2);
        pool.allocate(test_nid(3), 10).unwrap();
        pool.touch(test_nid(3), 3);
        assert_eq!(pool.free_bricks(), 0);

        // Evict LRU (body 1, frame 1) while protecting body 3
        let evicted = pool.evict_lru(test_nid(3));
        assert_eq!(evicted, Some(test_nid(1)));
        assert_eq!(pool.free_bricks(), 10);
        assert_eq!(pool.body_brick_count(test_nid(1)), 0);
    }

    #[test]
    fn lru_never_evicts_protected() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 20);
        pool.allocate(test_nid(1), 10).unwrap();
        pool.touch(test_nid(1), 1); // oldest
        pool.allocate(test_nid(2), 10).unwrap();
        pool.touch(test_nid(2), 5); // newer

        // Protect body 1 (the oldest) — should evict body 2 instead
        let evicted = pool.evict_lru(test_nid(1));
        assert_eq!(evicted, Some(test_nid(2)));
    }

    #[test]
    fn allocate_with_eviction() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 30);
        pool.allocate(test_nid(1), 10).unwrap();
        pool.touch(test_nid(1), 1);
        pool.allocate(test_nid(2), 10).unwrap();
        pool.touch(test_nid(2), 2);
        pool.allocate(test_nid(3), 10).unwrap();
        pool.touch(test_nid(3), 3);

        // Body 4 needs 15 bricks — must evict body 1 (10) + body 2 (10)
        let (slots, evicted) = pool.allocate_with_eviction(test_nid(4), 15, test_nid(3)).unwrap();
        assert_eq!(slots.len(), 15);
        assert_eq!(evicted.len(), 2);
        assert!(evicted.contains(&test_nid(1)));
        assert!(evicted.contains(&test_nid(2)));
        assert_eq!(pool.body_brick_count(test_nid(1)), 0);
        assert_eq!(pool.body_brick_count(test_nid(2)), 0);
        assert_eq!(pool.body_brick_count(test_nid(3)), 10); // protected
        assert_eq!(pool.body_brick_count(test_nid(4)), 15);
    }

    #[test]
    fn no_double_allocate() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 100);
        let s1 = pool.allocate(test_nid(1), 50).unwrap();
        let s2 = pool.allocate(test_nid(2), 50).unwrap();
        // All slots should be unique
        let mut all: Vec<u32> = s1.iter().chain(s2.iter()).copied().collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 100);
    }

    #[test]
    fn utilization() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 100);
        assert!((pool.utilization() - 0.0).abs() < 1e-6);
        pool.allocate(test_nid(1), 50).unwrap();
        assert!((pool.utilization() - 0.5).abs() < 1e-6);
        pool.allocate(test_nid(2), 50).unwrap();
        assert!((pool.utilization() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_alloc() {
        let mut pool = BrickPool::new(BRICK_BYTES as usize * 10);
        let slots = pool.allocate(test_nid(1), 0).unwrap();
        assert!(slots.is_empty());
        assert_eq!(pool.used_bricks(), 0);
    }

    #[test]
    fn body_brick_volume_basics() {
        let vol = BodyBrickVolume::new(
            test_nid(1),
            [16, 16, 16],
            glam::Vec3::new(-1.0, -1.0, -1.0),
            glam::Vec3::new(1.0, 1.0, 1.0),
        );
        assert_eq!(vol.total_bricks(), 4096);
        assert_eq!(vol.active_bricks(), 0);
        assert_eq!(vol.effective_resolution, [128, 128, 128]);
    }

    #[test]
    fn body_brick_volume_set_clear() {
        let mut vol = BodyBrickVolume::new(
            test_nid(1),
            [4, 4, 4],
            glam::Vec3::ZERO,
            glam::Vec3::ONE,
        );
        vol.set_brick(1, 2, 3, 42);
        assert_eq!(vol.active_bricks(), 1);
        let idx = vol.brick_flat_index(1, 2, 3);
        assert_eq!(vol.indirection[idx], 42);
        vol.clear_brick(1, 2, 3);
        assert_eq!(vol.active_bricks(), 0);
        assert_eq!(vol.indirection[idx], BRICK_EMPTY);
    }

    #[test]
    fn aabb_to_brick_range() {
        let vol = BodyBrickVolume::new(
            test_nid(1),
            [8, 8, 8],
            glam::Vec3::new(0.0, 0.0, 0.0),
            glam::Vec3::new(2.0, 2.0, 2.0),
        );
        // Query the center 50% of the volume
        let (bmin, bmax) = vol.aabb_to_brick_range(
            glam::Vec3::new(0.5, 0.5, 0.5),
            glam::Vec3::new(1.5, 1.5, 1.5),
        );
        assert_eq!(bmin, [2, 2, 2]);
        assert_eq!(bmax, [6, 6, 6]);
    }
}
