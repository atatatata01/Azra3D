use wgpu;
use glam::Vec3;
use core_math::sdf_volume::{SdfVolume, DirtyRegion};
use core_math::scene_graph::NodeId;
use half::f16;

pub struct SdfGpuVolume {
    /// Flat storage buffer — bake output (GPU bake writes here) + sculpt uploads.
    pub gpu_buffer: wgpu::Buffer,
    /// 3D texture for raymarch read path (R16Float, hardware trilinear).
    pub read_texture: wgpu::Texture,
    pub read_view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub size_bytes: u64,
    pub resolution: [u32; 3],
}

impl SdfGpuVolume {
    pub fn new(device: &wgpu::Device, resolution: [u32; 3]) -> Self {
        // Cast to u64 BEFORE multiply to prevent u32 overflow at high resolutions (512³+)
        let mut res = resolution;
        let mut size_bytes = res[0] as u64 * res[1] as u64 * res[2] as u64 * 4;
        // Safety clamp: SdfGpuVolume uses a flat storage buffer, which is limited by
        // max_buffer_size (~256MB). At 512³+ the body_vol_cache uses TextureOnly mode
        // (direct-to-texture, no flat buffer). This clamp only fires for the legacy
        // non-body-vol-cache path (sculpt mode).
        let max_buffer = device.limits().max_buffer_size;
        if size_bytes > max_buffer {
            eprintln!("[GpuVolume] Requested {}MB exceeds GPU max buffer {}MB, clamping to 256³",
                size_bytes / 1_048_576, max_buffer / 1_048_576);
            res = [256, 256, 256];
            size_bytes = 256u64 * 256 * 256 * 4;
        }
        // Flat storage buffer (bake output + sculpt mode upload)
        let gpu_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("SDF GPU Volume"),
            size: size_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 3D texture for raymarch read path (R16Float — 50% memory vs f32 buffer)
        let read_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("voxel_3d_texture"),
            size: wgpu::Extent3d {
                width: res[0],
                height: res[1],
                depth_or_array_layers: res[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let read_view = read_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("voxel_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self { gpu_buffer, read_texture, read_view, sampler, size_bytes, resolution: res }
    }

    /// Upload dirty region of volume data to both the flat buffer and 3D texture.
    ///
    /// Flat buffer: single batched write_buffer covering the dirty Z-slab.
    /// 3D texture: write_texture with f32→f16 conversion for the dirty Z-slab.
    pub fn upload(&self, queue: &wgpu::Queue, volume: &SdfVolume, dirty: &DirtyRegion) {
        // IDENTITY INVARIANT (fail closed): the CPU volume and this GPU volume must be the
        // same resolution. A mismatch means the caller is uploading into a stale/placeholder
        // instance (e.g. the 1³ startup dummy) — the write would overrun the buffer and the
        // texture (wgpu validation error → device loss; the 2026-07-23 sculpt crash class).
        // The caller must (re)create the SdfGpuVolume at the volume's resolution first.
        // NOT an assert: the 1³ dummy legitimately exists before the legacy sculpt room
        // allocates the full-size volume (cpu_volume is constructed dirty), so the correct
        // behavior is to refuse the write, not to crash.
        if volume.resolution != self.resolution {
            eprintln!(
                "[GpuVolume] upload SKIPPED — volume res {:?} != GPU volume res {:?} \
                 (stale/dummy GPU volume; caller must reallocate before uploading)",
                volume.resolution, self.resolution
            );
            return;
        }
        let rx = volume.resolution[0] as u64;
        let ry = volume.resolution[1] as u64;
        let rz = volume.resolution[2] as u64;
        let total_voxels = rx * ry * rz;

        // Clamp dirty region to volume bounds
        let min_z = (dirty.min[2] as u64).min(rz.saturating_sub(1));
        let max_z = (dirty.max[2] as u64).min(rz.saturating_sub(1));

        // Single contiguous upload covering all Z-planes in the dirty region.
        let start_voxel = min_z * rx * ry;
        let end_voxel = ((max_z + 1) * rx * ry).min(total_voxels);
        if start_voxel >= end_voxel { return; }

        let start_usize = start_voxel as usize;
        let end_usize = end_voxel as usize;
        if end_usize > volume.data.len() { return; }

        let slice = &volume.data[start_usize..end_usize];

        // Flat buffer upload (for bake pipeline compatibility).
        // Guard: gpu_volume is often a 1³ (4 B) dummy at init until sculpt/project_load
        // resizes it. Writing a full 128³ slab (8 MB) into that dummy is the
        // "Copy of 0..8388608 … Destination buffer of size 4" device-loss class
        // (visdump 2026-07-20). Skip rather than kill the device.
        let byte_offset = start_voxel * 4;
        let byte_len = (slice.len() as u64) * 4;
        if byte_offset + byte_len > self.size_bytes || byte_offset + byte_len > self.gpu_buffer.size() {
            eprintln!(
                "[GpuVolume] SKIP upload: write {}..{}B exceeds buffer {}B (res={:?}, volume res={:?}) — \
                 resize SdfGpuVolume before uploading",
                byte_offset,
                byte_offset + byte_len,
                self.gpu_buffer.size(),
                self.resolution,
                volume.resolution
            );
            return;
        }
        queue.write_buffer(
            &self.gpu_buffer,
            byte_offset,
            bytemuck::cast_slice(slice),
        );

        // 3D texture upload (f32 → f16 conversion)
        let f16_data: Vec<u16> = slice.iter()
            .map(|&v| f16::from_f32(v).to_bits())
            .collect();
        let num_z = (max_z - min_z + 1) as u32;
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.read_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: min_z as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&f16_data),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(rx as u32 * 2), // 2 bytes per f16 voxel
                rows_per_image: Some(ry as u32),
            },
            wgpu::Extent3d {
                width: rx as u32,
                height: ry as u32,
                depth_or_array_layers: num_z,
            },
        );
    }

    /// Upload the entire volume to both the flat buffer and 3D texture (full rebake).
    pub fn upload_full(&self, queue: &wgpu::Queue, data: &[f32]) {
        let expected = (self.size_bytes / 4) as usize;
        let len = data.len().min(expected);
        if len == 0 { return; }
        let byte_len = (len as u64) * 4;
        if byte_len > self.gpu_buffer.size() {
            eprintln!(
                "[GpuVolume] SKIP upload_full: {}B data vs {}B buffer (res={:?})",
                byte_len,
                self.gpu_buffer.size(),
                self.resolution
            );
            return;
        }

        // Flat buffer
        queue.write_buffer(&self.gpu_buffer, 0, bytemuck::cast_slice(&data[..len]));

        // 3D texture (f32 → f16)
        let f16_data: Vec<u16> = data[..len].iter()
            .map(|&v| f16::from_f32(v).to_bits())
            .collect();
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.read_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&f16_data),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.resolution[0] * 2),
                rows_per_image: Some(self.resolution[1]),
            },
            wgpu::Extent3d {
                width: self.resolution[0],
                height: self.resolution[1],
                depth_or_array_layers: self.resolution[2],
            },
        );
    }

    /// Byte size of the 3D read texture (R16Float).
    pub fn texture_bytes(&self) -> u64 {
        self.resolution[0] as u64 * self.resolution[1] as u64 * self.resolution[2] as u64 * 2
    }
}

// ============================================================
// ResidentVolume — persistent GPU 3D texture for the active body
// ============================================================

/// A persistent GPU-resident 3D texture for the active editing body.
///
/// Instead of allocating a new texture on every bake cycle, the active body
/// owns this texture across frames. Rebakes write into the same texture.
/// Only reallocated when:
/// - The active body changes (old deallocated, new allocated)
/// - The body's AABB grows beyond the texture's coverage
/// - Resolution changes
/// (2026-07-13 legacy-volume cleanup: the per-body R32Float 3D texture + view + sampler are
/// GONE — they were read only by the removed SDF volume raymarch (VP binding 2) and the removed
/// PT rm=3 volume mode (bindings 40/41). `buffer` is the sole GPU field store; the DC meshes it
/// field-direct. VRAM win: one res³ R32Float texture per pool slot — 512MB at 512³.)
pub struct ResidentVolume {
    /// Flat storage buffer mirror (f32, for compute shader compatibility).
    /// `Arc` so the GPU-authority bake (path A) can cheaply clone the handle and bind it as the bake
    /// output without holding a `body_vol_cache` borrow across the dispatch. `&Arc<Buffer>` deref-coerces
    /// to `&Buffer` at the existing call sites (write_buffer, copy_buffer_to_buffer, .size(), bindings).
    pub buffer: std::sync::Arc<wgpu::Buffer>,
    pub body_nid: NodeId,
    pub resolution: [u32; 3],
    /// Allocation bounds (with margin) — used for reallocation threshold only.
    pub bounds_min: Vec3,
    /// Allocation bounds (with margin) — used for reallocation threshold only.
    pub bounds_max: Vec3,
    /// Exact bake grid bounds (no margin) — used by the shader to map
    /// the 3D texture to world space. Must match the AABB used during bake.
    pub bake_bounds_min: Vec3,
    pub bake_bounds_max: Vec3,
    pub buffer_bytes: u64,
}

impl ResidentVolume {
    pub fn new(
        device: &wgpu::Device,
        body_nid: NodeId,
        resolution: [u32; 3],
        bounds_min: Vec3,
        bounds_max: Vec3,
    ) -> Self {
        let buffer_bytes = resolution[0] as u64
            * resolution[1] as u64
            * resolution[2] as u64
            * 4;

        let buffer = std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident_vol_buffer"),
            size: buffer_bytes,
            // COPY_SRC: GPU-authority path A bakes the dirty slab directly into this buffer, then copies
            // it to the readback staging buffer (and the lazy CPU materialize reads it). COPY_DST kept for
            // the legacy output_buf→rv.buffer gpu2gpu copy (realloc frame / non-resident fallback).
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));

        let _ = device; // texture/sampler creation removed with the legacy volume display
        Self {
            buffer,
            body_nid, resolution, bounds_min, bounds_max,
            bake_bounds_min: bounds_min,
            bake_bounds_max: bounds_max,
            buffer_bytes,
        }
    }

    /// Upload a full f32 grid into the flat storage buffer (the sole GPU field store —
    /// the per-body 3D texture was removed with the legacy volume display).
    pub fn upload_full(&self, queue: &wgpu::Queue, data: &[f32]) {
        let expected = (self.buffer_bytes / 4) as usize;
        let len = data.len().min(expected);
        if len == 0 { return; }
        let byte_len = (len as u64) * 4;
        if byte_len > self.buffer.size() {
            eprintln!(
                "[ResidentVolume] SKIP upload_full: {}B data vs {}B buffer (res={:?} body={:?})",
                byte_len,
                self.buffer.size(),
                self.resolution,
                self.body_nid
            );
            return;
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&data[..len]));
    }

    /// Upload a sub-region of the f32 grid into the flat storage buffer.
    /// `bake_min`/`bake_max` are voxel-space coordinates (exclusive max).
    /// `data` is the full grid (resolution³ f32s).
    pub fn upload_subregion(
        &self,
        queue: &wgpu::Queue,
        data: &[f32],
        bake_min: [u32; 3],
        bake_max: [u32; 3],
    ) {
        let [rx, ry, _rz] = self.resolution;
        let sx = bake_max[0].saturating_sub(bake_min[0]);
        let sy = bake_max[1].saturating_sub(bake_min[1]);
        let sz = bake_max[2].saturating_sub(bake_min[2]);
        if sx == 0 || sy == 0 || sz == 0 { return; }

        // Flat buffer: single contiguous write covering the minimal span.
        // CPU data[] and GPU buffer share the same linear layout (z*ry*rx + y*rx + x),
        // so we write from first dirty voxel to last dirty voxel. Unchanged voxels
        // in the gaps (outside X/Y subregion but within the span) are already in sync
        // between CPU and GPU — safe to overwrite with identical values.
        let first_voxel = (bake_min[2] as usize) * (ry as usize) * (rx as usize)
            + (bake_min[1] as usize) * (rx as usize)
            + bake_min[0] as usize;
        let end_voxel = ((bake_max[2] - 1) as usize) * (ry as usize) * (rx as usize)
            + ((bake_max[1] - 1) as usize) * (rx as usize)
            + bake_max[0] as usize;
        if end_voxel > first_voxel && end_voxel <= data.len() {
            let byte_off = (first_voxel as u64) * 4;
            let byte_len = ((end_voxel - first_voxel) as u64) * 4;
            if byte_off + byte_len > self.buffer.size() {
                eprintln!(
                    "[ResidentVolume] SKIP upload_subregion: write {}..{}B exceeds buffer {}B (res={:?})",
                    byte_off,
                    byte_off + byte_len,
                    self.buffer.size(),
                    self.resolution
                );
                return;
            }
            queue.write_buffer(
                &self.buffer,
                byte_off,
                bytemuck::cast_slice(&data[first_voxel..end_voxel]),
            );
        }
    }

    /// Check whether the resident volume needs reallocation (AABB grew or resolution changed).
    pub fn needs_realloc(&self, new_min: Vec3, new_max: Vec3, new_res: [u32; 3]) -> bool {
        if self.resolution != new_res { return true; }
        const EPS: f32 = 0.01;
        new_min.x < self.bounds_min.x - EPS
            || new_min.y < self.bounds_min.y - EPS
            || new_min.z < self.bounds_min.z - EPS
            || new_max.x > self.bounds_max.x + EPS
            || new_max.y > self.bounds_max.y + EPS
            || new_max.z > self.bounds_max.z + EPS
    }
}

// ============================================================
// VolumePool — LRU-managed pool of ResidentVolume slots
// ============================================================

/// A single slot in the VolumePool, wrapping a ResidentVolume with LRU metadata.
///
/// The `volume` field is `Option` because `acquire()` returns slot indices
/// before the caller creates the GPU resource. Callers assign the volume
/// after acquisition.
pub struct VolumePoolSlot {
    pub volume: Option<ResidentVolume>,
    /// NodeId of the body occupying this slot.
    pub body_nid: NodeId,
    /// Frame counter at last use — drives LRU eviction.
    pub last_used_frame: u64,
    /// Current bake resolution (uniform axis).
    pub resolution: u32,
    /// Whether this slot's volume data needs a re-bake.
    pub dirty: bool,
}

/// GPU volume pool with LRU eviction for multi-body editing.
///
/// Holds up to `max_slots` resident volumes so switching between bodies
/// doesn't require dealloc + alloc + upload on every switch. When the pool
/// is full, the least-recently-used slot is evicted to make room.
///
/// This struct is pure bookkeeping — it does NOT allocate GPU resources.
/// Callers create `ResidentVolume` instances and place them into slots.
pub struct VolumePool {
    slots: Vec<Option<VolumePoolSlot>>,
    max_slots: usize,
    current_frame: u64,
    /// Recycled ResidentVolume allocations to avoid GPU alloc on pool miss.
    /// Max 4 entries. Cleared on invalidate_all().
    freelist: Vec<ResidentVolume>,
}

impl VolumePool {
    /// Create a new pool with the given maximum slot count.
    ///
    /// Recommended values based on VRAM: 4 GB → 4 slots, 8 GB → 8 slots,
    /// 24 GB → 16 slots. Use [`Self::max_slots_for_vram`] to compute from
    /// available VRAM bytes.
    pub fn new(max_slots: usize) -> Self {
        let max_slots = max_slots.max(1); // At least 1 slot
        Self {
            slots: (0..max_slots).map(|_| None).collect(),
            max_slots,
            current_frame: 0,
            freelist: Vec::new(),
        }
    }

    /// Find or allocate a slot for `body_nid`.
    ///
    /// Returns `(slot_index, was_cache_hit)`:
    /// - If the body is already in the pool: touches LRU and returns `(index, true)`.
    /// - If an empty slot is available: returns `(index, false)`. The caller must
    ///   create a `ResidentVolume` and assign it via [`Self::get_mut`].
    /// - If full: evicts the LRU slot and returns `(evicted_index, false)`.
    pub fn acquire(&mut self, body_nid: NodeId) -> (usize, bool) {
        // Cache hit — body already resident
        if let Some(idx) = self.find_body(body_nid) {
            self.touch(idx);
            return (idx, true);
        }

        // Find first empty slot
        if let Some(idx) = self.slots.iter().position(|s| s.is_none()) {
            return (idx, false);
        }

        // Pool full — evict LRU
        let (idx, _evicted) = self.evict_lru();
        (idx, false)
    }

    /// Mark a slot as used this frame (update LRU counter).
    pub fn touch(&mut self, slot_index: usize) {
        if let Some(Some(slot)) = self.slots.get_mut(slot_index) {
            slot.last_used_frame = self.current_frame;
        }
    }

    /// Get an immutable reference to a slot.
    pub fn get(&self, slot_index: usize) -> Option<&VolumePoolSlot> {
        self.slots.get(slot_index).and_then(|s| s.as_ref())
    }

    /// Get a mutable reference to a slot.
    pub fn get_mut(&mut self, slot_index: usize) -> Option<&mut VolumePoolSlot> {
        self.slots.get_mut(slot_index).and_then(|s| s.as_mut())
    }

    /// Find the slot index for a body, if it's currently in the pool.
    pub fn find_body(&self, body_nid: NodeId) -> Option<usize> {
        self.slots.iter().position(|s| {
            s.as_ref().map_or(false, |slot| slot.body_nid == body_nid)
        })
    }

    /// Evict the least-recently-used slot. Returns `(slot_index, evicted_body_nid)`.
    ///
    /// If no occupied slots exist (shouldn't happen when called from `acquire`
    /// after the pool is full), returns `(0, None)`.
    fn evict_lru(&mut self) -> (usize, Option<NodeId>) {
        let mut best_idx = 0;
        let mut best_frame = u64::MAX;

        for (i, slot_opt) in self.slots.iter().enumerate() {
            if let Some(slot) = slot_opt {
                if slot.last_used_frame < best_frame {
                    best_frame = slot.last_used_frame;
                    best_idx = i;
                }
            }
        }

        let evicted_nid = self.slots[best_idx]
            .as_ref()
            .map(|s| s.body_nid);
        // Move ResidentVolume to freelist instead of dropping (if freelist not full)
        if let Some(slot) = self.slots[best_idx].take() {
            if let Some(vol) = slot.volume {
                if self.freelist.len() < 4 {
                    self.freelist.push(vol);
                }
                // else: freelist full, vol is dropped normally
            }
        }

        (best_idx, evicted_nid)
    }

    /// Release a specific body from the pool, freeing its slot.
    ///
    /// Returns `true` if the body was found and released.
    pub fn release(&mut self, body_nid: NodeId) -> bool {
        if let Some(idx) = self.find_body(body_nid) {
            // Move ResidentVolume to freelist instead of dropping
            if let Some(slot) = self.slots[idx].take() {
                if let Some(vol) = slot.volume {
                    if self.freelist.len() < 4 {
                        self.freelist.push(vol);
                    }
                }
            }
            true
        } else {
            false
        }
    }

    /// Advance the frame counter. Call once per frame before any `acquire`/`touch`.
    pub fn advance_frame(&mut self) {
        self.current_frame += 1;
    }

    /// Number of currently occupied slots.
    pub fn occupied_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Current frame counter (for diagnostics / debugging).
    pub fn current_frame(&self) -> u64 {
        self.current_frame
    }

    /// Maximum number of slots this pool was created with.
    pub fn max_slots(&self) -> usize {
        self.max_slots
    }

    /// Take a pre-allocated ResidentVolume from the freelist matching the given resolution.
    /// Returns None if no matching entry exists.
    pub fn take_from_freelist(&mut self, resolution: u32) -> Option<ResidentVolume> {
        let pos = self.freelist.iter().position(|v| {
            v.resolution[0] == resolution && v.resolution[1] == resolution && v.resolution[2] == resolution
        });
        pos.map(|i| self.freelist.swap_remove(i))
    }

    /// Clear all recycled ResidentVolume entries from the freelist.
    pub fn clear_freelist(&mut self) {
        self.freelist.clear();
    }

    /// Number of recycled ResidentVolume entries in the freelist (for diagnostics).
    pub fn freelist_len(&self) -> usize {
        self.freelist.len()
    }

    /// Direct mutable access to slots (for callers that need to populate
    /// newly-acquired empty slots with a `VolumePoolSlot`).
    pub fn slots_mut(&mut self) -> &mut Vec<Option<VolumePoolSlot>> {
        &mut self.slots
    }

    /// Calculate the recommended max_slots from available VRAM in bytes.
    ///
    /// Heuristic: ~1 slot per GB of VRAM, capped at 16. Each slot at 128^3
    /// uses ~12 MB (4 MB R16Float texture + 8 MB f32 buffer), so even 16
    /// slots is only ~192 MB — well within budget. The per-GB mapping keeps
    /// the pool modest on low-VRAM GPUs and generous on high-end ones.
    ///
    /// | VRAM   | Slots |
    /// |--------|-------|
    /// | 4 GB   | 4     |
    /// | 8 GB   | 8     |
    /// | 24 GB  | 16    |
    pub fn max_slots_for_vram(vram_bytes: u64) -> usize {
        const ONE_GB: u64 = 1024 * 1024 * 1024;
        let slots = (vram_bytes / ONE_GB) as usize;
        slots.clamp(1, 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(v: u64) -> NodeId { NodeId(v) }

    #[test]
    fn empty_pool_acquire_returns_first_slot() {
        let mut pool = VolumePool::new(4);
        let (idx, hit) = pool.acquire(nid(10));
        assert_eq!(idx, 0);
        assert!(!hit);
    }

    #[test]
    fn cache_hit_after_insert() {
        let mut pool = VolumePool::new(4);
        let (idx, _) = pool.acquire(nid(10));
        // Simulate caller placing a slot
        pool.slots[idx] = Some(VolumePoolSlot {
            volume: None,
            body_nid: nid(10),
            last_used_frame: pool.current_frame,
            resolution: 128,
            dirty: false,
        });
        pool.advance_frame();
        let (idx2, hit) = pool.acquire(nid(10));
        assert_eq!(idx2, idx);
        assert!(hit);
    }

    #[test]
    fn evicts_lru_when_full() {
        let mut pool = VolumePool::new(2);

        // Fill both slots
        for i in 0..2u64 {
            let (idx, _) = pool.acquire(nid(i));
            pool.slots[idx] = Some(VolumePoolSlot {
                volume: None,
                body_nid: nid(i),
                last_used_frame: pool.current_frame,
                resolution: 128,
                dirty: false,
            });
            pool.advance_frame();
        }
        assert_eq!(pool.occupied_count(), 2);

        // Acquire a third body — should evict body 0 (oldest)
        let (idx, hit) = pool.acquire(nid(99));
        assert!(!hit);
        // Slot 0 was LRU (frame 0 vs frame 1)
        assert_eq!(idx, 0);
        assert!(pool.find_body(nid(0)).is_none());
        assert!(pool.find_body(nid(1)).is_some());
    }

    #[test]
    fn release_frees_slot() {
        let mut pool = VolumePool::new(4);
        let (idx, _) = pool.acquire(nid(42));
        pool.slots[idx] = Some(VolumePoolSlot {
            volume: None,
            body_nid: nid(42),
            last_used_frame: 0,
            resolution: 128,
            dirty: false,
        });
        assert_eq!(pool.occupied_count(), 1);
        assert!(pool.release(nid(42)));
        assert_eq!(pool.occupied_count(), 0);
        assert!(!pool.release(nid(42))); // already gone
    }

    #[test]
    fn find_body_returns_none_when_absent() {
        let pool = VolumePool::new(4);
        assert!(pool.find_body(nid(999)).is_none());
    }

    #[test]
    fn max_slots_for_vram_ranges() {
        // 4 GB → 4 slots
        assert_eq!(VolumePool::max_slots_for_vram(4 * 1024 * 1024 * 1024), 4);
        // 8 GB → 8 slots
        assert_eq!(VolumePool::max_slots_for_vram(8 * 1024 * 1024 * 1024), 8);
        // Very small VRAM — clamp to 1
        assert_eq!(VolumePool::max_slots_for_vram(1024), 1);
        // 24 GB → capped at 16
        assert_eq!(VolumePool::max_slots_for_vram(24 * 1024 * 1024 * 1024), 16);
        // 48 GB — still capped at 16
        assert_eq!(VolumePool::max_slots_for_vram(48 * 1024 * 1024 * 1024), 16);
    }

    #[test]
    fn touch_updates_lru() {
        let mut pool = VolumePool::new(2);

        // Fill both slots: body 0 at frame 0, body 1 at frame 1
        for i in 0..2u64 {
            let (idx, _) = pool.acquire(nid(i));
            pool.slots[idx] = Some(VolumePoolSlot {
                volume: None,
                body_nid: nid(i),
                last_used_frame: pool.current_frame,
                resolution: 128,
                dirty: false,
            });
            pool.advance_frame();
        }

        // Touch body 0 to make it more recent than body 1
        let idx0 = pool.find_body(nid(0)).unwrap();
        pool.touch(idx0);

        // Now body 1 is LRU — acquiring body 99 should evict body 1
        let (evicted_idx, _) = pool.acquire(nid(99));
        let idx1_was = 1; // body 1 was in slot 1
        assert_eq!(evicted_idx, idx1_was);
        assert!(pool.find_body(nid(1)).is_none());
        assert!(pool.find_body(nid(0)).is_some());
    }

    #[test]
    fn min_one_slot() {
        let pool = VolumePool::new(0);
        assert_eq!(pool.max_slots, 1);
        assert_eq!(pool.slots.len(), 1);
    }
}
