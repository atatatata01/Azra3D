//! Shared-cache helpers for body duplication (Shift+D).

use core_math::scene_graph::{SceneGraph, NodeId};
use super::{BodyVolumeCache, ResidentVolume};

impl BodyVolumeCache {
    // ============================================================
    // Shared-cache helpers for body duplication (Shift+D)
    // ============================================================

    /// Clone cached volume data from source body to target body.
    ///
    /// Called from `duplicate_selected()` after `duplicate_subtree()`. The
    /// First-class body duplication: clones grid data, allocates a ResidentVolume,
    /// and uploads to GPU — all in one call. The new body is render-ready immediately.
    ///
    /// Returns `true` if the clone succeeded and the GPU volume is ready.
    /// Returns `false` if:
    ///   - Source has no cached grid (caller should `mark_body_dirty(new_nid)`)
    ///   - GPU device unavailable (grid cloned but no GPU upload)
    ///   - Pool full (grid cloned, upload deferred to `update_resident_volume()`)
    pub(crate) fn duplicate_body(
        &mut self,
        source_nid: NodeId,
        new_nid: NodeId,
        device: &eframe::wgpu::Device,
        queue: &eframe::wgpu::Queue,
    ) -> bool {
        // Step 1: Clone grid data (same as clone_cache)
        let entry_cloned = if let Some(entry) = self.body_grids.get(&source_nid) {
            let mut cloned = entry.clone();
            cloned.shared_source = Some(source_nid);
            // S2.4 contract C4: the brick_resident marker must NOT travel with a clone — the
            // SdfBrickAtlasGpu holds ONE body (the source), so the duplicate's marker could never
            // match the atlas identity anyway. (This fn is currently dead code — the live
            // duplicate path in gizmo.rs re-bakes the new body from scratch — kept consistent.)
            cloned.brick_resident = None;
            // S2.5: the frozen index shares the marker's lifecycle — never travels with a clone.
            cloned.sparse_frozen = None;
            self.body_grids.insert(new_nid, cloned);
            true
        } else {
            false
        };
        if !entry_cloned {
            return false;
        }

        // Copy stored body bounds (needed for partial-bake region math)
        if let Some(&bounds) = self.body_bounds.get(&source_nid) {
            self.body_bounds.insert(new_nid, bounds);
        }

        // Remove from dirty set — the clone provides valid data immediately.
        self.dirty_bodies.remove(&new_nid);

        // Step 2: Allocate a ResidentVolume in the pool for the new body
        let entry = self.body_grids.get(&new_nid).unwrap();
        let grid_min = entry.grid_min;
        let grid_max = entry.grid_max;
        let res = entry.bake_resolution;
        let res3 = [res, res, res];
        let grid_data = entry.grid.clone();

        let (slot_idx, _was_hit) = self.volume_pool.acquire(new_nid);

        let extent = grid_max - grid_min;
        let margin = extent * 0.15;
        let mut new_vol = if let Some(mut recycled) = self.volume_pool.take_from_freelist(res) {
            recycled.body_nid = new_nid;
            recycled.bounds_min = grid_min - margin;
            recycled.bounds_max = grid_max + margin;
            recycled
        } else {
            ResidentVolume::new(
                device,
                new_nid,
                res3,
                grid_min - margin,
                grid_max + margin,
            )
        };
        // Set exact bake bounds — the shader uses these for world-space mapping
        new_vol.bake_bounds_min = grid_min;
        new_vol.bake_bounds_max = grid_max;

        // Step 3: Upload grid data to GPU immediately
        new_vol.upload_full(queue, &grid_data);

        // Place into pool slot
        let slot = graphics::gpu_volume::VolumePoolSlot {
            volume: Some(new_vol),
            body_nid: new_nid,
            last_used_frame: self.volume_pool.current_frame(),
            resolution: res,
            dirty: false,
        };
        if let Some(s) = self.volume_pool.get_mut(slot_idx) {
            s.volume = slot.volume;
            s.resolution = slot.resolution;
        } else {
            self.volume_pool.slots_mut()[slot_idx] = Some(slot);
        }

        // Point resident tracking at the new body immediately — the volume is
        // already uploaded and ready. Without this, update_resident_volume sees
        // body_changed → defers upload by 1 frame → blank viewport after duplicate.
        self.resident_body_nid = Some(new_nid);
        self.resident_slot = Some(slot_idx);
        self.resident_upload_needed = false; // already uploaded
        self.resident_buffer_valid = true; // upload_full above wrote rv.buffer with new_nid's field
        self.last_upload_key = (entry.source_hash, res);
        self.resident_volume_reallocated = true; // bind group needs rebuild

        // Track dirty range for DC extraction (full grid)
        // GATE 2: fresh full grid installed for the duplicate — absent region = full
        // re-mesh / full upload, every consumer's conservative default.
        self.dirty_mesh_region.remove(&new_nid);

        true
    }

    /// If this body's cache was cloned from another body (after duplication),
    /// mark it dirty so the next bake produces an independent grid.
    ///
    /// This is called automatically by `mark_body_dirty()`. An explicit call is
    /// only needed if code that doesn't go through `mark_body_dirty()` needs to
    /// detach the shared cache (e.g., body-level property changes routed directly
    /// through the transform-safe path).
    pub(crate) fn invalidate_shared_cache(&mut self, body_nid: NodeId) {
        let is_shared = self.body_grids.get(&body_nid)
            .map_or(false, |e| e.shared_source.is_some());
        if is_shared {
            // Delegate to mark_body_dirty — clears shared_source flag and queues rebake.
            self.mark_body_dirty(body_nid);
        }
    }

    /// Scan all bodies for stale shared-source references and clear them.
    ///
    /// A shared source becomes stale when the original body is deleted. Without
    /// this cleanup the flag is harmless (it only gates whether a rebake is
    /// skipped), but clearing it keeps the state tidy and avoids confusing
    /// diagnostic output.
    ///
    /// Call once after any body deletion (e.g., in the same frame as
    /// `remove_body()`).
    pub(crate) fn validate_shared_sources(&mut self, sg: &SceneGraph) {
        let stale: Vec<NodeId> = self.body_grids.iter()
            .filter_map(|(nid, entry)| {
                entry.shared_source.and_then(|src| {
                    if sg.get(src).is_none() {
                        Some(*nid)
                    } else {
                        None
                    }
                })
            })
            .collect();
        for nid in stale {
            if let Some(entry) = self.body_grids.get_mut(&nid) {
                entry.shared_source = None;
            }
        }
    }
}
