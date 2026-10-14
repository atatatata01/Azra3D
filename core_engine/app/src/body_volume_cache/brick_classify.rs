//! Phase 2/4: CPU brick classification from dense/coarse grids.

use std::collections::HashMap;
use glam::Vec3;
use core_math::scene_graph::NodeId;
use super::BodyVolumeCache;

impl BodyVolumeCache {
    // ============================================================
    // Phase 2: CPU brick classification from dense grid
    // ============================================================

    /// Classify and copy bricks from a dense grid into the shared brick pool.
    ///
    /// For each 8³ brick in the grid, tests whether any voxel is near the
    /// surface (|sdf| < threshold). Active bricks are allocated in the pool
    /// and their voxel data is copied. The indirection table maps brick
    /// coordinates to pool slot indices.
    ///
    /// Call after a bake result is accepted (grid inserted into body_grids).
    /// Classify bricks for ALL bodies that already have cached grids.
    /// Called when brick pool is first enabled to populate from existing data.
    pub(crate) fn classify_all_cached_bodies(
        &mut self,
        device: Option<&eframe::wgpu::Device>,
        queue: Option<&eframe::wgpu::Queue>,
    ) {
        let nids: Vec<NodeId> = self.body_grids.keys().copied().collect();
        for nid in nids {
            self.classify_bricks_for_body(nid, device, queue);
        }
    }

    pub(crate) fn classify_bricks_for_body(
        &mut self,
        body_nid: NodeId,
        device: Option<&eframe::wgpu::Device>,
        queue: Option<&eframe::wgpu::Queue>,
    ) {
        if !self.use_brick_pool { return; }
        // Lazy GPU buffer init
        if let Some(dev) = device {
            self.brick_pool.init_gpu(dev);
        }
        let entry = match self.body_grids.get(&body_nid) {
            Some(e) => e,
            None => return,
        };

        let res = entry.bake_resolution;
        let grid = &entry.grid;
        let grid_min = entry.grid_min;
        let grid_max = entry.grid_max;
        let entry_source_hash = entry.source_hash;

        // TextureOnly bodies (>256³) store grid: Vec::new() — is_brick_active indexes grid[idx] and would
        // panic on the empty Vec (grid[0] on []). The brick pool is dense-grid-only (≤256³); guard like the
        // sibling classifiers already do. (Review #2: a guaranteed OOB panic — bake a 512³ body, click Brick
        // Pool.) Dev-gated + parked, but a hard crash is a hard crash.
        if grid.is_empty() || res > 256 {
            return;
        }

        // Brick grid dimensions
        let dims = [
            (res + graphics::brick_pool::BRICK_DIM - 1) / graphics::brick_pool::BRICK_DIM,
            (res + graphics::brick_pool::BRICK_DIM - 1) / graphics::brick_pool::BRICK_DIM,
            (res + graphics::brick_pool::BRICK_DIM - 1) / graphics::brick_pool::BRICK_DIM,
        ];

        // Surface detection threshold: brick diagonal × 1.5
        let extent = grid_max - grid_min;
        let voxel_size = extent / res as f32;
        let brick_diag = (voxel_size * graphics::brick_pool::BRICK_DIM as f32).length();
        let threshold = brick_diag * 1.5;

        // Classify bricks: scan 8 corners + center of each brick
        let mut active_bricks: Vec<[u32; 3]> = Vec::new();
        for bz in 0..dims[2] {
            for by in 0..dims[1] {
                for bx in 0..dims[0] {
                    if Self::is_brick_active(grid, res, bx, by, bz, threshold) {
                        active_bricks.push([bx, by, bz]);
                    }
                }
            }
        }

        // Deallocate old bricks for this body (if any)
        self.brick_pool.deallocate_body(body_nid);

        // Allocate new bricks
        let n = active_bricks.len() as u32;
        let (slots, evicted) = match self.brick_pool.allocate_with_eviction(
            body_nid, n, body_nid, // protect self during alloc
        ) {
            Some(result) => result,
            None => {
                eprintln!("[BrickPool] Failed to allocate {} bricks for body {:?}", n, body_nid);
                return;
            }
        };
        // Clean up stale BodyBrickVolume entries for evicted bodies
        for evicted_nid in &evicted {
            self.body_brick_volumes.remove(evicted_nid);
        }

        // Build indirection table + copy voxel data
        let mut bbv = graphics::brick_pool::BodyBrickVolume::new(
            body_nid, dims, grid_min, grid_max,
        );

        // Flat buffer for all brick voxels to upload at once
        let bd = graphics::brick_pool::BRICK_DIM;
        let bv = graphics::brick_pool::BRICK_VOXELS as usize;
        let mut brick_data: Vec<f32> = Vec::with_capacity(slots.len() * bv);

        for (i, &[bx, by, bz]) in active_bricks.iter().enumerate() {
            let pool_slot = slots[i];
            bbv.set_brick(bx, by, bz, pool_slot);

            // Copy 8³ voxels from dense grid → flat brick data
            for lz in 0..bd {
                for ly in 0..bd {
                    for lx in 0..bd {
                        let gx = bx * bd + lx;
                        let gy = by * bd + ly;
                        let gz = bz * bd + lz;
                        let val = if gx < res && gy < res && gz < res {
                            grid[(gx + gy * res + gz * res * res) as usize]
                        } else {
                            100.0 // outside grid → far from surface
                        };
                        brick_data.push(val);
                    }
                }
            }
        }

        // Upload brick data to GPU pool SSBO
        if let Some(q) = queue {
            if let Some(buf) = self.brick_pool.buffer() {
                for (i, &pool_slot) in slots.iter().enumerate() {
                    let byte_offset = pool_slot as u64 * graphics::brick_pool::BRICK_BYTES as u64;
                    let float_start = i * bv;
                    let float_end = float_start + bv;
                    q.write_buffer(
                        buf,
                        byte_offset,
                        bytemuck::cast_slice(&brick_data[float_start..float_end]),
                    );
                }
            }
        }

        // Store CPU-side brick data for BrickGridAccessor (Phase 3 DC parity).
        // Dense-packed: brick_data is already sequential by brick index (0..num_active).
        // Build slot_map: pool_slot → dense local index. Memory = O(active_bricks), not O(max_slot).
        let mut cpu_slot_map = std::collections::HashMap::with_capacity(slots.len());
        for (i, &pool_slot) in slots.iter().enumerate() {
            cpu_slot_map.insert(pool_slot, i);
        }
        bbv.cpu_data = brick_data;
        bbv.cpu_slot_map = cpu_slot_map;
        bbv.source_hash = entry_source_hash;

        // Upload indirection table to GPU
        if let (Some(dev), Some(q)) = (device, queue) {
            bbv.upload_indirection(dev, q);
        }
        self.body_brick_volumes.insert(body_nid, bbv);
        self.brick_pool.touch(body_nid, self.volume_pool.current_frame());
    }

    /// Check if a brick contains surface (any voxel |sdf| < threshold).
    /// Tests 9 samples: 8 corners + center.
    fn is_brick_active(grid: &[f32], res: u32, bx: u32, by: u32, bz: u32, threshold: f32) -> bool {
        let bd = graphics::brick_pool::BRICK_DIM;
        // 8 corners
        for c in 0..8u32 {
            let lx = if c & 1 != 0 { bd - 1 } else { 0 };
            let ly = if c & 2 != 0 { bd - 1 } else { 0 };
            let lz = if c & 4 != 0 { bd - 1 } else { 0 };
            let gx = bx * bd + lx;
            let gy = by * bd + ly;
            let gz = bz * bd + lz;
            if gx < res && gy < res && gz < res {
                let val = grid[(gx + gy * res + gz * res * res) as usize];
                if val.abs() < threshold { return true; }
            }
        }
        // Center sample (catches thin features corners miss)
        let cx = bx * bd + bd / 2;
        let cy = by * bd + bd / 2;
        let cz = bz * bd + bd / 2;
        if cx < res && cy < res && cz < res {
            let val = grid[(cx + cy * res + cz * res * res) as usize];
            if val.abs() < threshold { return true; }
        }
        false
    }

    // ============================================================
    // Phase 4 — Coarse-grid brick classification (direct brick bake)
    // ============================================================

    /// Classify bricks from a coarse bake grid (Phase 4 brick bake path).
    ///
    /// The coarse grid has `dims_bricks × 2` resolution — 2 samples per brick per axis,
    /// giving 8 coarse voxels per brick. If any coarse voxel has |sdf| < threshold,
    /// the brick is marked active.
    ///
    /// Allocates pool slots and builds the BodyBrickVolume indirection table.
    /// Does NOT fill brick voxel data — that's done by the brick_fill GPU shader.
    pub(crate) fn classify_bricks_coarse(
        &mut self,
        body_nid: NodeId,
        device: Option<&eframe::wgpu::Device>,
        queue: Option<&eframe::wgpu::Queue>,
    ) {
        if !self.use_brick_pool || !self.use_brick_bake { return; }
        // Lazy GPU buffer init
        if let Some(dev) = device {
            self.brick_pool.init_gpu(dev);
        }
        let entry = match self.body_grids.get(&body_nid) {
            Some(e) => e,
            None => return,
        };

        let coarse_res = entry.bake_resolution;
        let grid = &entry.grid;
        let grid_min = entry.grid_min;
        let grid_max = entry.grid_max;
        let entry_source_hash = entry.source_hash;

        // Target brick dimensions from the full (effective) resolution
        let bd = graphics::brick_pool::BRICK_DIM;
        let dims = [
            self.resolution / bd,
            self.resolution / bd,
            self.resolution / bd,
        ];

        // Surface detection threshold: brick diagonal × 1.5
        let extent = grid_max - grid_min;
        let voxel_size = extent / self.resolution as f32;
        let brick_diag = (voxel_size * bd as f32).length();
        let threshold = brick_diag * 1.5;

        // Coarse samples per brick per axis (should be 2 for dims_bricks*2 grid)
        let samples_per_brick = if dims[0] > 0 { coarse_res / dims[0] } else { 1 };

        let mut active_bricks: Vec<[u32; 3]> = Vec::new();
        for bz in 0..dims[2] {
            for by in 0..dims[1] {
                for bx in 0..dims[0] {
                    let mut active = false;
                    // Check all coarse voxels within this brick
                    'outer: for sz in 0..samples_per_brick {
                        for sy in 0..samples_per_brick {
                            for sx in 0..samples_per_brick {
                                let gx = bx * samples_per_brick + sx;
                                let gy = by * samples_per_brick + sy;
                                let gz = bz * samples_per_brick + sz;
                                if gx < coarse_res && gy < coarse_res && gz < coarse_res {
                                    let idx = (gx + gy * coarse_res + gz * coarse_res * coarse_res) as usize;
                                    if idx < grid.len() && grid[idx].abs() < threshold {
                                        active = true;
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                    if active {
                        active_bricks.push([bx, by, bz]);
                    }
                }
            }
        }

        // Deallocate old bricks for this body (if any)
        self.brick_pool.deallocate_body(body_nid);

        // Allocate new bricks
        let n = active_bricks.len() as u32;
        let (slots, evicted) = match self.brick_pool.allocate_with_eviction(
            body_nid, n, body_nid,
        ) {
            Some(result) => result,
            None => {
                eprintln!("[BrickPool] Failed to allocate {} bricks for body {:?}", n, body_nid);
                return;
            }
        };
        // Clean up stale BodyBrickVolume entries for evicted bodies
        for evicted_nid in &evicted {
            self.body_brick_volumes.remove(evicted_nid);
        }

        // Build indirection table (no voxel data yet — GPU fill shader writes that)
        let mut bbv = graphics::brick_pool::BodyBrickVolume::new(
            body_nid, dims, grid_min, grid_max,
        );
        for (i, &[bx, by, bz]) in active_bricks.iter().enumerate() {
            bbv.set_brick(bx, by, bz, slots[i]);
        }

        // Upload indirection to GPU
        if let (Some(dev), Some(q)) = (device, queue) {
            bbv.upload_indirection(dev, q);
        }

        bbv.source_hash = entry_source_hash;

        eprintln!("[BrickPool] Coarse classify: {} active / {} total bricks for body {:?}",
            n, dims[0] * dims[1] * dims[2], body_nid);

        self.body_brick_volumes.insert(body_nid, bbv);
        self.brick_pool.touch(body_nid, self.volume_pool.current_frame());
    }
}
