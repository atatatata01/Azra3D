//! Resident volume lifecycle — GPU upload and pool management.

use core_math::scene_graph::NodeId;
use super::{BodyVolumeCache, ResidentVolume};

impl BodyVolumeCache {
    // ============================================================
    // Resident volume lifecycle
    // ============================================================

    /// Mark dirty ONLY the top-level bodies whose cached grid is missing, at draft
    /// resolution, or regime-unservable (`needs_rebake_on_leave`). This is the work-mode /
    /// scope-switch entry point: a mode switch changes NO bake input (Object Mode is the
    /// only render regime since the legacy VP path was removed), so the old blanket
    /// `mark_all_bodies_dirty` there was pure churn — every Vertex↔Object toggle re-ran
    /// the LOD/draft machinery over every body and re-baked bodies with perfectly valid
    /// full-resolution grids (the owner's 1024/512/256 selection-change bake storm).
    pub(crate) fn mark_stale_bodies_dirty(&mut self, sg: &core_math::scene_graph::SceneGraph) {
        for nid in crate::bake_utils::collect_all_body_nids(sg) {
            // Sub-bodies are subsumed into their top-level ancestor's bake (they never have
            // their own grid) — marking them would sit in dirty_bodies forever.
            if sg.parent_body(nid).is_some() { continue; }
            if self.needs_rebake_on_leave(nid) {
                self.dirty_bodies.insert(nid);
            }
        }
    }

    /// Returns true if the body's cached grid is missing or at draft resolution.
    /// Bodies with valid full-resolution grids do NOT need rebake on scope-leave.
    fn needs_rebake_on_leave(&self, nid: NodeId) -> bool {
        let entry = match self.body_grids.get(&nid) {
            Some(e) => e,
            None => return true, // no cache at all
        };
        let target = self.effective_resolution(nid);
        // S2.4 contract C5: a brick-resident (SparseBricks) entry at target res would otherwise
        // pass the `<` check and skip the rebake — but it is only servable while the sparse
        // regime is still active (toggle ON + Object Mode). If it isn't (toggle off / edit mode),
        // the empty-grid entry has nothing to display → rebake on scope-leave.
        if entry.brick_resident.is_some() && !self.sparse_bake_eligible(target) {
            return true;
        }
        entry.bake_resolution < target // draft needs promotion to full res
    }

    /// Update the resident volume for the active body via the volume pool.
    ///
    /// Call once per frame after `poll()`. Handles:
    /// 1. Active body change: acquire from pool (instant if cached, alloc if miss)
    /// 2. AABB growth: reallocate if body grew beyond current coverage
    /// 3. Upload: write latest bake grid into persistent texture (only when changed)
    /// (2026-07-13 legacy-volume cleanup: the `_opt` skip_buffer/skip_texture variant, the
    /// texture-heal routes, and the chunked texture drain are DELETED — `rv.buffer` is the
    /// sole upload target now that the 3D texture is gone from `ResidentVolume`.)
    pub(crate) fn update_resident_volume(
        &mut self,
        active_body_nid: NodeId,
        device: &eframe::wgpu::Device,
        queue: &eframe::wgpu::Queue,
    ) {
        // Detect active body change
        let body_changed = self.resident_body_nid != Some(active_body_nid);
        if body_changed {
            // Freeze the old body's cascade — don't invest GPU time in a body
            // the user just stopped editing. Prevents 256³ bake on body switch.
            // Then mark it dirty so it bakes at target resolution and gets a DC
            // mesh for raster cache rendering. Clear debounce so trigger_bake
            // dispatches immediately (scope-leave is not a rapid edit).
            if let Some(old_nid) = self.resident_body_nid {
                // Always freeze cascade — stop investing GPU time in the old body.
                self.freeze_cascade_on_leave(old_nid);
                // Only mark dirty if the cached grid is missing or at draft resolution.
                // Bodies with valid full-resolution grids don't need rebake on scope-leave.
                if self.needs_rebake_on_leave(old_nid) {
                    self.mark_body_dirty(old_nid);
                    self.last_dirty_time = None; // debounce bypass — scope-leave is not rapid edit
                }
            }
            // Acquire from pool — cache hit means instant switch (L5)
            let (slot_idx, was_hit) = self.volume_pool.acquire(active_body_nid);
            self.resident_body_nid = Some(active_body_nid);
            self.resident_slot = Some(slot_idx);
            // Reset upload dedup key — new body needs fresh upload check
            self.last_upload_key = (0, 0);
            // Buffer-valid on body switch. A pool HIT keeps THIS body's last-resident field in its slot.
            // If that field is still CURRENT (the slot's resident volume matches the body's cached grid
            // res + bounds), the buffer is VALID right now — field-direct meshes it THIS frame with NO
            // CPU-grid fallback re-mesh (which clones the whole grid + re-meshes = the freeze on EVERY body
            // selection). Only a MISS (fresh/recycled slot) or a res/bounds mismatch needs the re-upload.
            // Inactive bodies don't drift (you can only edit the ACTIVE body); an undo/load that DID change
            // one marks it dirty → a pending bake revalidates the slot regardless. Worst case on the rare
            // stale path: one frame of the prior mesh, self-healed by the next bake — never a freeze.
            let slot_current = self.volume_pool.get(slot_idx)
                .and_then(|s| s.volume.as_ref())
                .zip(self.body_grids.get(&active_body_nid))
                .map_or(false, |(rv, e)| !e.grid.is_empty()
                    && rv.body_nid == active_body_nid
                    && rv.resolution == [e.bake_resolution, e.bake_resolution, e.bake_resolution]
                    && (rv.bake_bounds_min - e.grid_min).length_squared() < 1e-6
                    && (rv.bake_bounds_max - e.grid_max).length_squared() < 1e-6);
            if was_hit {
                // Current slot → valid now (no fallback, no re-upload). Stale slot → re-upload revalidates.
                self.resident_buffer_valid = slot_current;
                self.resident_upload_needed = !slot_current;
                // Don't reallocate — texture is valid
            } else {
                // Pool miss: need to allocate a new volume for this slot
                self.resident_buffer_valid = false;
                self.resident_upload_needed = true;
                self.resident_volume_reallocated = true;
            }
            // Defer GPU allocation/upload to next frame — avoids 5-17ms stall
            // during body switch. This frame renders analytically; volume appears
            // on frame N+1 when body_changed is false and upload proceeds.
            return;
        }

        let slot_idx = match self.resident_slot {
            Some(idx) => idx,
            None => return,
        };

        // (The brick-resident dense-re-bake and TEXTURE-HEAL routes lived here — both were
        // reachable only from a texture-consuming caller (skip_texture=false, i.e. the removed
        // SDF-Edit / PT volume raymarch), so they were deleted with the texture.)

        // Early out: nothing to upload and volume already exists in slot
        let has_volume = self.volume_pool.get(slot_idx)
            .map_or(false, |s| s.volume.is_some());
        if !self.resident_upload_needed && has_volume {
            return;
        }

        // If the active body has a cached grid with actual data, ensure the pool
        // slot has a resident volume and upload. Skip for TextureOnly bodies
        // (empty grid) — they render through the atlas, not ResidentVolume.
        // Creating a ResidentVolume at 512³+ would exceed max_storage_buffer_binding_size.
        if let Some(entry) = self.body_grids.get(&active_body_nid).filter(|e| !e.grid.is_empty()) {
            let grid_min = entry.grid_min;
            let grid_max = entry.grid_max;
            let res = entry.bake_resolution;
            let res3 = [res, res, res];

            // Check if existing volume in slot needs realloc
            let needs_alloc = self.volume_pool.get(slot_idx)
                .and_then(|s| s.volume.as_ref())
                .map_or(true, |rv| rv.needs_realloc(grid_min, grid_max, res3));

            if needs_alloc {
                let extent = grid_max - grid_min;
                let margin = extent * 0.15;
                // Try recycled GPU allocation from freelist before creating new
                let new_vol = if let Some(mut recycled) = self.volume_pool.take_from_freelist(res) {
                    // Reuse GPU texture/buffer — just update metadata
                    recycled.body_nid = active_body_nid;
                    recycled.bounds_min = grid_min - margin;
                    recycled.bounds_max = grid_max + margin;
                    recycled.bake_bounds_min = grid_min;
                    recycled.bake_bounds_max = grid_max;
                    recycled
                } else {
                    let mut rv = ResidentVolume::new(
                        device,
                        active_body_nid,
                        res3,
                        grid_min - margin,
                        grid_max + margin,
                    );
                    rv.bake_bounds_min = grid_min;
                    rv.bake_bounds_max = grid_max;
                    rv
                };
                // Place into pool slot
                let slot = graphics::gpu_volume::VolumePoolSlot {
                    volume: Some(new_vol),
                    body_nid: active_body_nid,
                    last_used_frame: self.volume_pool.current_frame(),
                    resolution: res,
                    dirty: false,
                };
                // Overwrite slot (acquire already cleared it or it's a hit we're upgrading)
                if let Some(s) = self.volume_pool.get_mut(slot_idx) {
                    s.volume = slot.volume;
                    s.resolution = slot.resolution;
                } else {
                    // Slot was empty after acquire (pool miss) — need to insert
                    // VolumePool doesn't have direct insert, so use the internal slots
                    // via get_mut. The acquire() returns a valid index — if get_mut
                    // returns None it means the slot was not populated yet.
                    // We'll set it through the pool's raw slot access.
                    self.volume_pool.slots_mut()[slot_idx] = Some(slot);
                }
                // New texture — must upload, reset dedup key + content-bounds
                self.last_upload_key = (0, 0);
                self.last_upload_bounds = (glam::Vec3::splat(1e30), glam::Vec3::splat(-1e30));
                self.resident_upload_needed = true;
                self.resident_volume_reallocated = true;
                self.resident_buffer_valid = false; // fresh/recycled buffer until uploaded
            }

            if self.resident_upload_needed {
                // Detect whether the world->voxel mapping is about to change for the
                // ENTIRE grid: either a realloc (fresh/recycled texture whose content is
                // stale/garbage) or a genuine bake-bounds shift. This MUST be captured
                // BEFORE we overwrite bake_bounds below — otherwise the comparison is
                // always false (the dead `bounds_changed` test in the mode decision).
                // When true, a chunked (multi-frame) upload would display the NEW mapping
                // over OLD/garbage texture content for several frames → visible tearing.
                // Force a single-frame atomic Full upload so texture + mapping change
                // together in one frame. Edit A keeps escapes rare, so the one-frame
                // Full-upload cost is paid only occasionally, never every drag frame.
                // Compare against `last_upload_bounds` — the bounds the buffer CONTENT was
                // actually last written at — NOT `rv.bake_bounds`. `rv.bake_bounds` is the
                // *label*, overwritten at line ~245 every frame this flag is set (even on a
                // dedup-skipped frame), so a comparison against it can read "unchanged" while
                // the content is still authored at the old bounds. `last_upload_bounds` moves
                // ONLY when content is actually written → it can never lie about staleness.
                let force_atomic_upload = self.resident_volume_reallocated
                    || (self.last_upload_bounds.0 - grid_min).length_squared() > 1e-6
                    || (self.last_upload_bounds.1 - grid_max).length_squared() > 1e-6;
                // [visrec] capture rv.bake_bounds BEFORE the overwrite below — the flight
                // recorder logs the pre-overwrite label so a mapping flip is visible in the ring.
                let prev_bake_bounds = if crate::vis_recorder::vis_enabled() {
                    self.volume_pool.get(slot_idx)
                        .and_then(|s| s.volume.as_ref())
                        .map(|rv| (rv.bake_bounds_min, rv.bake_bounds_max))
                } else {
                    None
                };
                // Always update bake bounds — even without realloc, the grid
                // bounds may have changed (e.g., prim scale) within the margin.
                // The shader uses bake_bounds for world-space mapping.
                if let Some(slot) = self.volume_pool.get_mut(slot_idx) {
                    if let Some(ref mut rv) = slot.volume {
                        rv.bake_bounds_min = grid_min;
                        rv.bake_bounds_max = grid_max;
                    }
                }
                // A realloc between the direct bake (frame N — bake_into_resident wrote the OLD rv.buffer
                // instance and latched gpu_texture_upload_done) and this consume (frame N+1) means the flag
                // now certifies a buffer that NO LONGER EXISTS: needs_alloc above just swapped in a fresh /
                // recycled EMPTY (or stale-recycled) buffer. `gpu_texture_upload_done` is provenance for a
                // specific buffer INSTANCE but isn't bound to it, so without this it blindly flips the empty
                // new buffer "valid" (last_upload_bounds=grid, resident_buffer_valid=true) → field-direct
                // meshes empty → the flat slab / stretched-cube (AZRA_PROBE: content_bounds_ok=true yet
                // GPU-buf 0.0%). The DISPATCH side already refuses bake_into_resident when reallocated
                // (app_poll.rs:1363) — mirror that contract here: VOID the stale claim and fall through to
                // the CPU else-branch, where force_atomic_upload (TRUE on realloc, line ~240) does the
                // single-frame Full re-upload into the NEW buffer, stamping last_upload_bounds only AFTER the
                // write actually lands. Steady state has no realloc → the fast direct-bake skip is untouched.
                if self.resident_volume_reallocated {
                    self.gpu_texture_upload_done = false;
                    self.gpu_texture_upload_body = None;
                }
                // GPU-to-GPU path: check if the 3D texture was already updated
                // via copy_buffer_to_texture at dispatch time. If so, skip the
                // entire CPU upload path (saves 5-45ms per cycle).
                if self.gpu_texture_upload_done && self.gpu_texture_upload_body == Some(active_body_nid) {
                    // Bake shader wrote directly into THIS (active) body's rv.buffer. Skip CPU write_texture.
                    // The body match is load-bearing: a different body latching the flag would otherwise flip
                    // THIS body's stale buffer "valid" → field-direct ghost (white box).
                    // [visrec] upload decision (Part B #4): GPU2GPU direct bake — CPU upload skipped.
                    crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "upload.mode", || format!(
                        "body={:?} mode=gpu2gpu-skip force_atomic={} grid={:.3?}..{:.3?} \
                         rv.bake_bounds_before={:.3?}",
                        active_body_nid, force_atomic_upload,
                        grid_min, grid_max, prev_bake_bounds));
                    self.gpu_texture_upload_done = false;
                    self.gpu_texture_upload_body = None;
                    self.last_upload_key = (entry.source_hash, res);
                    // The bake wrote rv.buffer at THIS grid's bounds → record them so a later
                    // re-tighten to different bounds is detected as a content change.
                    self.last_upload_bounds = (grid_min, grid_max);
                    self.dirty_mesh_region.remove(&active_body_nid);
                    // rv.buffer WAS the bake output (bake_into_resident) and now holds this body's current
                    // field (its source_hash == the last_upload_key just set) → the resident buffer is VALID.
                    // Restore the flag the full-readback accept_gpu_result cleared (bake_readback.rs:129): the
                    // settle re-tighten bake takes the staging path (resident_full_direct is idle-gated off),
                    // so without this the following sdf_populate_resident saw buf_valid=false and fell back to a
                    // res³ grid.to_vec() + slot.field write_buffer — the ~400ms idle-settle freeze on every
                    // post-edit select. Field-direct from rv.buffer (already written) skips both copies.
                    self.resident_buffer_valid = true;
                    if crate::perf_spam_on() {
                        eprintln!("[VolumeUpload] body={:?} SKIPPED — direct-to-texture bake", active_body_nid);
                    }
                } else {
                // Dedup: skip upload if the grid data hasn't changed since
                // the last upload. Prevents 42ms/frame stalls when bake results
                // or cascades re-set resident_upload_needed without new data.
                let upload_key = (entry.source_hash, res);
                // `force_atomic_upload` is TRUE when the world→voxel mapping changed (realloc OR a
                // bake-bounds shift, captured above BEFORE bake_bounds was overwritten). The dedup key is
                // ONLY (source_hash, res) — it does NOT encode bounds — so a settle re-tighten / escape that
                // MOVES the bounds but keeps the SAME geometry (same source_hash) would dedup-skip the
                // re-upload, leaving rv.buffer's CONTENT authored at the OLD bounds while rv.bake_bounds is
                // updated to the NEW bounds. The field-direct DC then maps old-bounds content with the new
                // mapping → 0% inside → the flat slab (CONFIRMED by AZRA_PROBE: bounds all agree yet
                // GPU-buffer 0% vs CPU-grid 5.6%). So the dedup MUST NOT fire when the bounds changed —
                // gate it on `!force_atomic_upload`, and force the (Full) re-upload in that case.
                if upload_key == self.last_upload_key && !force_atomic_upload {
                    // True dedup hit: same grid AND same bounds → rv.buffer already current. (A settle
                    // re-tighten that does NOT move bounds still lands here → buffer stays valid, no re-upload.)
                    self.resident_buffer_valid = true;
                    // [visrec] upload decision (Part B #4): dedup hit — no upload.
                    crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "upload.mode", || format!(
                        "body={:?} mode=dedup-hit force_atomic={} grid={:.3?}..{:.3?} \
                         rv.bake_bounds_before={:.3?}",
                        active_body_nid, force_atomic_upload,
                        grid_min, grid_max, prev_bake_bounds));
                }
                if upload_key != self.last_upload_key || force_atomic_upload {
                    // Determine upload mode before borrowing pool (avoids borrow conflict
                    // when starting chunked upload which modifies self).
                    #[derive(Clone, Copy)]
                    enum UploadMode { Partial([u32; 3], [u32; 3]), Full }
                    let mode = self.volume_pool.get(slot_idx)
                        .and_then(|s| s.volume.as_ref())
                        .map(|rv| {
                            // force_atomic_upload was captured BEFORE bake_bounds was
                            // overwritten above. It is true when the whole-grid world
                            // mapping changed (realloc or bounds shift). In that case the
                            // texture content and its mapping MUST land in the SAME frame,
                            // so a single-frame Full upload is mandatory — a Chunked
                            // (multi-frame) upload would show the new mapping over stale /
                            // garbage texels for several frames = tearing.
                            if force_atomic_upload {
                                UploadMode::Full
                            } else {
                                let dirty_range = self.dirty_mesh_region.get(&active_body_nid);
                                let use_partial = dirty_range.map_or(false, |(bmin, bmax)| {
                                    bmin[0] > 0 || bmin[1] > 0 || bmin[2] > 0
                                        || bmax[0] < rv.resolution[0]
                                        || bmax[1] < rv.resolution[1]
                                        || bmax[2] < rv.resolution[2]
                                });
                                if use_partial {
                                    let (bmin, bmax) = *dirty_range.unwrap();
                                    UploadMode::Partial(bmin, bmax)
                                } else {
                                    // (Chunked multi-frame mode removed: it spread the 3D-TEXTURE
                                    // write across frames; the buffer write was always one-shot.)
                                    UploadMode::Full
                                }
                            }
                        });
                    if let Some(mode) = mode {
                        // [visrec] upload decision (Part B #4): the chosen upload mode + full gate state.
                        crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "upload.mode", || {
                            let mode_name = match mode {
                                UploadMode::Partial(bmin, bmax) => format!("Partial({:?}..{:?})", bmin, bmax),
                                UploadMode::Full => "Full".to_string(),
                            };
                            format!(
                                "body={:?} mode={} force_atomic={} grid={:.3?}..{:.3?} \
                                 rv.bake_bounds_before={:.3?}",
                                active_body_nid, mode_name,
                                force_atomic_upload, grid_min, grid_max, prev_bake_bounds)
                        });
                        match mode {
                            UploadMode::Partial(bmin, bmax) => {
                                if cfg!(debug_assertions) {
                                    eprintln!("[VolumeUpload] body={:?} partial=true", active_body_nid);
                                }
                                if let Some(rv) = self.volume_pool.get(slot_idx).and_then(|s| s.volume.as_ref()) {
                                    rv.upload_subregion(queue, &entry.grid, bmin, bmax);
                                }
                            }
                            UploadMode::Full => {
                                if cfg!(debug_assertions) {
                                    eprintln!("[VolumeUpload] body={:?} full (res={}³)", active_body_nid, res);
                                }
                                if let Some(rv) = self.volume_pool.get(slot_idx).and_then(|s| s.volume.as_ref()) {
                                    rv.upload_full(queue, &entry.grid);
                                }
                            }
                        }
                        self.last_upload_key = upload_key;
                        self.dirty_mesh_region.remove(&active_body_nid);
                        // A buffer write just landed (both modes write rv.buffer):
                        // rv.buffer now holds the current field → the field-direct DC mesh may read it.
                        // Record the bounds the content was written at so the next bounds shift is
                        // detected as a content change (not dedup-skipped).
                        self.resident_buffer_valid = true;
                        self.last_upload_bounds = (grid_min, grid_max);
                    }
                }
                } // end else (GPU2GPU texture not done — CPU upload path)
            }
        }
        // Clear flag unconditionally — even if body has no grid yet.
        // Without this, the flag stays true forever when the active body
        // hasn't been baked, causing every frame to fall past the early-out.
        self.resident_upload_needed = false;
    }
}
