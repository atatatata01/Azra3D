//! BrickAllocator — CPU-side bookkeeping for sparse brick classification.
//!
//! Bricks are a WRITE-SIDE optimization: they decide which 8³ regions contain surface
//! and need full-resolution SDF evaluation. The brick fill shader writes results
//! directly into the atlas 3D texture. The VP shader reads ONLY from the atlas.
//!
//! No GPU buffer. No indirection texture. No SSBO. Pure CPU bookkeeping.
//!
//! Invariants (from PLAN_BRICK_ATLAS.md):
//! 1. Slab clear: full clear to +999.0 before every brick fill
//! 2. Brick halo: each brick writes 10³ (8³ + 1-voxel border)
//! 3. Bounds stability: brick coords valid only within stable frame
//! 4. Generation fencing: stamp on dispatch, check before mark-valid
//! 5. Classifier bias: false positives preferred over false negatives
//! 6. CPU readback latency: acknowledged and measured

use std::collections::{HashMap, HashSet};
use glam::Vec3;
use core_math::scene_graph::NodeId;

/// Brick dimension: 8 voxels per axis per brick.
pub const BRICK_DIM: u32 = 8;

/// Cached body frame — used for bounds stability check (Invariant 3).
/// If any field changes, a full reclassify is required.
#[derive(Clone, Debug)]
pub struct BrickBodyFrame {
    pub aabb_min: Vec3,
    pub aabb_max: Vec3,
    pub resolution: [u32; 3],
}

impl BrickBodyFrame {
    pub fn matches(&self, aabb_min: Vec3, aabb_max: Vec3, resolution: [u32; 3]) -> bool {
        (self.aabb_min - aabb_min).length_squared() < 1e-6
        && (self.aabb_max - aabb_max).length_squared() < 1e-6
        && self.resolution == resolution
    }
}

/// CPU-only sparse brick allocator. Tracks which 8³ regions contain surface per body.
pub(crate) struct BrickAllocator {
    /// Per-body: set of active brick coordinates (sparse).
    body_bricks: HashMap<NodeId, HashSet<[u32; 3]>>,
    /// Per-body: cached frame for bounds stability (Invariant 3).
    body_frame: HashMap<NodeId, BrickBodyFrame>,
    /// Per-body: source hash for change detection.
    body_hash: HashMap<NodeId, u64>,
    /// LRU tracking: frame counter when body was last classified.
    body_last_used: HashMap<NodeId, u64>,
    /// Total allocated brick count across all bodies.
    total_bricks: u32,
    /// Budget cap (max bricks across all bodies).
    max_bricks: u32,
    /// Monotonic frame counter for LRU.
    frame_counter: u64,
}

impl BrickAllocator {
    pub fn new(max_bricks: u32) -> Self {
        Self {
            body_bricks: HashMap::new(),
            body_frame: HashMap::new(),
            body_hash: HashMap::new(),
            body_last_used: HashMap::new(),
            total_bricks: 0,
            max_bricks,
            frame_counter: 0,
        }
    }

    /// Allocate bricks for a body. Evicts LRU bodies if budget exceeded.
    /// Returns (success, list of evicted body NodeIds).
    /// Caller must clear evicted bodies' atlas slabs (Invariant 1).
    pub fn allocate_bricks(
        &mut self,
        body: NodeId,
        coords: HashSet<[u32; 3]>,
        frame: BrickBodyFrame,
        source_hash: u64,
    ) -> (bool, Vec<NodeId>) {
        let mut evicted = Vec::new();
        let new_count = coords.len() as u32;

        // Deallocate old bricks for this body first
        self.deallocate_body(body);

        // Evict LRU bodies until budget is met
        while self.total_bricks + new_count > self.max_bricks {
            if let Some(victim) = self.pick_lru_victim(body) {
                evicted.push(victim);
                self.deallocate_body(victim);
            } else {
                // No victim available (only the requesting body exists or all protected)
                eprintln!("[BrickAllocator] Cannot allocate {} bricks — budget full ({}/{})",
                    new_count, self.total_bricks, self.max_bricks);
                return (false, evicted);
            }
        }

        self.total_bricks += new_count;
        self.body_bricks.insert(body, coords);
        self.body_frame.insert(body, frame);
        self.body_hash.insert(body, source_hash);
        self.body_last_used.insert(body, self.frame_counter);
        (true, evicted)
    }

    /// Deallocate all bricks for a body. Called on delete, undo, new-scene, eviction.
    pub fn deallocate_body(&mut self, body: NodeId) {
        if let Some(coords) = self.body_bricks.remove(&body) {
            self.total_bricks = self.total_bricks.saturating_sub(coords.len() as u32);
        }
        self.body_frame.remove(&body);
        self.body_hash.remove(&body);
        self.body_last_used.remove(&body);
    }

    /// Deallocate all bodies. Called on new scene / invalidate_all.
    pub fn deallocate_all(&mut self) {
        self.body_bricks.clear();
        self.body_frame.clear();
        self.body_hash.clear();
        self.body_last_used.clear();
        self.total_bricks = 0;
    }

    /// Check if a body's brick classification is still valid (Invariant 3).
    pub fn is_frame_stable(&self, body: NodeId, aabb_min: Vec3, aabb_max: Vec3, resolution: [u32; 3]) -> bool {
        self.body_frame.get(&body).map_or(false, |f| f.matches(aabb_min, aabb_max, resolution))
    }

    /// Check if the body's source hash matches (change detection).
    pub fn hash_matches(&self, body: NodeId, source_hash: u64) -> bool {
        self.body_hash.get(&body).copied() == Some(source_hash)
    }

    /// Get the classified brick coordinates for a body.
    pub fn body_brick_coords(&self, body: NodeId) -> Option<&HashSet<[u32; 3]>> {
        self.body_bricks.get(&body)
    }

    /// Touch LRU timestamp for a body.
    pub fn touch(&mut self, body: NodeId) {
        if self.body_bricks.contains_key(&body) {
            self.body_last_used.insert(body, self.frame_counter);
        }
    }

    /// Advance the frame counter (call once per frame).
    pub fn tick(&mut self) {
        self.frame_counter += 1;
    }

    /// Pick the least-recently-used body for eviction, excluding `exclude`.
    fn pick_lru_victim(&self, exclude: NodeId) -> Option<NodeId> {
        self.body_last_used.iter()
            .filter(|(&nid, _)| nid != exclude && self.body_bricks.contains_key(&nid))
            .min_by_key(|(_, &frame)| frame)
            .map(|(&nid, _)| nid)
    }

    pub fn total_bricks(&self) -> u32 { self.total_bricks }
    pub fn max_bricks(&self) -> u32 { self.max_bricks }
    pub fn body_count(&self) -> usize { self.body_bricks.len() }
}

/// Build the sub-brick dispatch list for the GPU brick_fill shader.
///
/// Each 8³ brick is subdivided into 8 sub-bricks (2×2×2 of 4³ each).
/// Returns a flat `Vec<u32>` with 4 values per sub-brick entry:
/// `[grid_voxel_x, grid_voxel_y, grid_voxel_z, 0]`
///
/// Coordinates are in the body's **padded grid space** (0..grid_res), same as volume_bake.wgsl.
/// Brick coordinate [bx, by, bz] maps to grid voxel: pad + brick_coord * 8.
/// The shader converts to world-space and adds atlas_z_offset for textureStore.
pub fn build_sub_brick_list(
    brick_coords: &std::collections::HashSet<[u32; 3]>,
    atlas_pad: u32,
) -> Vec<u32> {
    let mut list = Vec::new();
    let mut sorted: Vec<[u32; 3]> = brick_coords.iter().copied().collect();
    sorted.sort_unstable(); // deterministic order
    for coord in sorted {
        // Grid voxel origin for this brick: pad + brick_coord * 8
        // This is in padded-grid space — same coordinates volume_bake.wgsl uses
        let base = [
            atlas_pad + coord[0] * BRICK_DIM,
            atlas_pad + coord[1] * BRICK_DIM,
            atlas_pad + coord[2] * BRICK_DIM,
        ];
        // 8 sub-bricks (2×2×2) of 4³ voxels each
        for sz in 0..2u32 {
            for sy in 0..2u32 {
                for sx in 0..2u32 {
                    list.push(base[0] + sx * 4);
                    list.push(base[1] + sy * 4);
                    list.push(base[2] + sz * 4);
                    list.push(0); // reserved
                }
            }
        }
    }
    list
}

/// Classify which 8³ bricks in a body's grid contain surface.
/// Input: coarse SDF grid, grid dimensions, brick grid dimensions, world-space extent.
/// Output: set of brick coordinates that need full-resolution evaluation.
///
/// Invariant 5: biased toward false positives. Missing a surface brick = hole in shape.
pub fn classify_bricks(coarse_grid: &[f32], coarse_dims: [u32; 3], dims_bricks: [u32; 3], grid_extent: f32) -> HashSet<[u32; 3]> {
    let mut active = HashSet::new();
    if coarse_grid.is_empty() { return active; }

    // Each brick covers a 2×2×2 region in the coarse grid.
    // Classify if ANY of the 9 test points (8 corners + center) is near the surface.
    // SDF values are in world-space units. A brick's world-space size =
    // grid_extent / dims_bricks. Threshold = 2.5 × brick diagonal (Invariant 5).
    let max_dim = dims_bricks[0].max(dims_bricks[1]).max(dims_bricks[2]).max(1) as f32;
    let brick_world = grid_extent / max_dim;
    let threshold = brick_world * 2.5 * (3.0_f32).sqrt(); // 2.5 × brick diagonal

    let sample = |x: u32, y: u32, z: u32| -> f32 {
        let x = x.min(coarse_dims[0].saturating_sub(1));
        let y = y.min(coarse_dims[1].saturating_sub(1));
        let z = z.min(coarse_dims[2].saturating_sub(1));
        let idx = (z * coarse_dims[1] * coarse_dims[0] + y * coarse_dims[0] + x) as usize;
        coarse_grid.get(idx).copied().unwrap_or(999.0)
    };

    for bz in 0..dims_bricks[2] {
        for by in 0..dims_bricks[1] {
            for bx in 0..dims_bricks[0] {
                // Coarse grid coords for this brick's corners
                let cx = bx * 2;
                let cy = by * 2;
                let cz = bz * 2;
                // Test 9 points: 8 corners + center
                let mut near_surface = false;
                for &(dx, dy, dz) in &[
                    (0,0,0), (2,0,0), (0,2,0), (2,2,0),
                    (0,0,2), (2,0,2), (0,2,2), (2,2,2),
                    (1,1,1), // center
                ] {
                    let d = sample(cx + dx, cy + dy, cz + dz).abs();
                    if d < threshold {
                        near_surface = true;
                        break;
                    }
                }
                if near_surface {
                    active.insert([bx, by, bz]);
                }
            }
        }
    }

    // Flood-fill one layer: also classify all 6 face-adjacent neighbors of active bricks.
    // This ensures halos have valid neighbors (Invariant 5).
    let initial: Vec<[u32; 3]> = active.iter().copied().collect();
    for coord in initial {
        for &(dx, dy, dz) in &[
            (1i32,0,0), (-1,0,0), (0,1,0), (0,-1,0), (0,0,1), (0,0,-1)
        ] {
            let nx = coord[0] as i32 + dx;
            let ny = coord[1] as i32 + dy;
            let nz = coord[2] as i32 + dz;
            if nx >= 0 && ny >= 0 && nz >= 0
                && (nx as u32) < dims_bricks[0]
                && (ny as u32) < dims_bricks[1]
                && (nz as u32) < dims_bricks[2]
            {
                active.insert([nx as u32, ny as u32, nz as u32]);
            }
        }
    }

    active
}
