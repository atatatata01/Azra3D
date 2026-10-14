//! Bake readback — poll, accept_gpu_result, clear_in_flight.

use std::sync::Arc;
use std::time::Instant;
use glam::Vec3;
use core_math::scene_graph::NodeId;
use super::{BodyVolumeCache, BodyCacheEntry, brick_allocator};

impl BodyVolumeCache {
    /// Poll for completed background bake results.
    /// Returns the list of body NodeIds that received new bake results.
    /// Empty vec = nothing changed; non-empty = DC extraction needed
    /// (and caller can trigger grid-based DC for these bodies).
    pub(crate) fn poll(&mut self) -> Vec<NodeId> {
        // Age debug regions — clear after ~60 frames so stale boxes don't persist.
        self.debug_region_age = self.debug_region_age.saturating_add(1);
        if self.debug_region_age > 60 {
            self.debug_last_regions.clear();
        }
        let mut updated = Vec::new();
        const MAX_POLL_RESULTS: usize = 2;
        let mut poll_count = 0;
        while let Ok(result) = self.bake_rx.try_recv() {
            self.in_flight.remove(&(result.body_id, result.source_hash));

            // GENERATION CHECK — discard stale results
            let current_gen = self.generations.get(&result.body_id).copied().unwrap_or(0);
            if result.generation < current_gen {
                continue;
            }

            // Flag resident upload if this result is for the active body
            if self.resident_body_nid == Some(result.body_id) {
                self.resident_upload_needed = true;
                // New grid not yet in rv.buffer → field-direct must wait for the upload.
                self.resident_buffer_valid = false;
            }
            self.body_grids.insert(result.body_id, BodyCacheEntry {
                grid: Arc::new(result.grid),
                source_hash: result.source_hash,
                grid_min: result.grid_min,
                grid_max: result.grid_max,
                bake_resolution: result.bake_resolution,
                shared_source: None,
                brick_resident: None, // dense CPU/worker bake result
                sparse_frozen: None,
            mesh_rigid_offset: Vec3::ZERO,
            });
            // GATE 2 — accept-time production of the single DirtyMeshRegion (CPU/worker path):
            // partial window → union; full-covering window → clear (absent = full, the
            // conservative default of every consumer).
            let r = result.bake_resolution;
            self.note_bake_accepted(result.body_id, result.bake_window, [r, r, r]);
            let stale_body = result.body_id;
            if self.cpu_grid_stale.remove(&stale_body) { // fresh CPU grid — staleness healed
                // [visrec] staleness transition (Part B #8).
                crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                    "body={:?} CLEARED (worker/CPU bake result landed in poll)", stale_body));
            }
            self.note_cpu_heal_complete(stale_body);
            self.finish_bake_timing(result.body_id);
            updated.push(result.body_id);
            poll_count += 1;
            if poll_count >= MAX_POLL_RESULTS { break; }
        }

        if !updated.is_empty() {
            self.last_bake_result_time = Some(Instant::now());
        }

        // Timeout check: only timeout bodies if NO bake results have arrived
        // recently. This prevents false timeouts when a large batch (e.g. 24
        // bodies) is still completing — individual bodies may take longer than
        // the timeout, but the batch as a whole is making progress.
        const BAKE_TIMEOUT_SECS: u64 = 10;
        let now = Instant::now();
        let batch_making_progress = self.last_bake_result_time
            .map(|t| now.duration_since(t).as_secs() < BAKE_TIMEOUT_SECS)
            .unwrap_or(false);

        if !batch_making_progress {
            let mut timed_out = Vec::new();
            for (&body_nid, &start_time) in &self.bake_start_times {
                if now.duration_since(start_time).as_secs() >= BAKE_TIMEOUT_SECS {
                    timed_out.push(body_nid);
                }
            }
            for body_nid in timed_out {
                self.bake_start_times.remove(&body_nid);
                self.in_flight.retain(|(bid, _)| *bid != body_nid);
                self.dirty_bodies.insert(body_nid);
            }
        }

        updated
    }

    /// Accept a GPU-baked body grid result. Returns true if the result was applied
    /// (i.e., not stale). Used by poll_gpu_jobs() to integrate worker GPU results.
    /// Step 2 (lazy readback): a resident field-direct bake completed and we are SKIPPING the CPU
    /// `to_vec` (the surface comes from `rv.buffer`; `body_grids` keeps its last full grid). Do only the
    /// non-grid bookkeeping `accept_gpu_result` would — generation gate, clear in-flight, flag the resident
    /// upload, progress timestamp — and DON'T touch `body_grids`. Returns false if the result is stale.
    pub(crate) fn note_resident_bake_complete(&mut self, body_id: NodeId, generation: u64) -> bool {
        let current_gen = self.generations.get(&body_id).copied().unwrap_or(0);
        if generation < current_gen {
            return false;
        }
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.finish_bake_timing(body_id);
        if self.resident_body_nid == Some(body_id) {
            self.resident_upload_needed = true; // GPU-direct bake → update_resident_volume skips the texture re-upload
            // Path-A in-place bake already wrote the new field straight into rv.buffer (this is the
            // lazy-readback path), so the buffer IS current even though the texture re-upload is pending.
            self.resident_buffer_valid = true;
        }
        self.last_bake_result_time = Some(Instant::now());
        true
    }

    pub(crate) fn accept_gpu_result(
        &mut self,
        body_id: NodeId,
        grid: Vec<f32>,
        generation: u64,
        source_hash: u64,
        // Bake bounds captured AT DISPATCH. `Some` from the primary-GPU staging path — carries the bake's
        // own provenance so a concurrent `aabb_escape` that mutated `body_bounds` mid-flight can't re-scale
        // this landing grid (the white-box ghost). `None` from the worker path → live `body_bounds` (unchanged).
        dispatch_bounds: Option<(Vec3, Vec3)>,
    ) -> bool {
        // Generation check — discard stale results
        let current_gen = self.generations.get(&body_id).copied().unwrap_or(0);
        if generation < current_gen {
            return false;
        }
        // Clear in-flight (all hashes for this body)
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.finish_bake_timing(body_id);
        // Flag resident upload if this result is for the active body
        if self.resident_body_nid == Some(body_id) {
            self.resident_upload_needed = true;
            // Full-readback path: the new grid is NOT yet in rv.buffer. The post-accept rv sync
            // (poll_primary_bake) uploads it and re-sets resident_buffer_valid before the DC reads it.
            self.resident_buffer_valid = false;
        }
        // Bake bounds: prefer the DISPATCH-TIME provenance (immune to an aabb_escape that mutated
        // body_bounds while THIS bake — computed for the old bounds — was in flight; stamping the old grid
        // with the new expanded bounds is the white-box ghost). Fall back to live body_bounds only for
        // callers that don't carry dispatch bounds (worker path).
        let (grid_min, grid_max) = dispatch_bounds.unwrap_or_else(|| {
            self.body_bounds.get(&body_id)
                .copied()
                .unwrap_or((Vec3::splat(-self.bounds), Vec3::splat(self.bounds)))
        });
        // Infer bake resolution from grid size (res^3 entries)
        let bake_resolution = (grid.len() as f32).cbrt().round() as u32;
        let grid_len = grid.len();
        self.body_grids.insert(body_id, BodyCacheEntry {
            grid: Arc::new(grid),
            source_hash,
            grid_min,
            grid_max,
            bake_resolution,
            shared_source: None,
            brick_resident: None, // dense readback result
            sparse_frozen: None,
            mesh_rigid_offset: Vec3::ZERO,
        });
        // [visrec] accept_gpu_result store (Part B #3).
        crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "bake.store", || format!(
            "body={:?} res={} bounds={:.3?}..{:.3?} hash={:#018x} len={} dispatch_bounds={}",
            body_id, bake_resolution, grid_min, grid_max, source_hash, grid_len,
            if dispatch_bounds.is_some() { "some" } else { "none(live body_bounds)" }));
        if self.cpu_grid_stale.remove(&body_id) { // readback landed — CPU grid fresh, staleness healed
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                "body={:?} CLEARED (accept_gpu_result readback)", body_id));
        }
        // Start heal cooldown so no-copy while IDLE does not re-queue settle immediately.
        self.note_cpu_heal_complete(body_id);
        if crate::perf_spam_on() && self.heal_tracing() {
            eprintln!("[HealTrace] readback ACCEPTED body={:?} res={} hash={:016x} — cpu grid fresh, stale cleared",
                body_id, bake_resolution, source_hash);
        }
        // Keep the resident volume's bake_bounds in LOCKSTEP with the grid we just accepted. The rv.buffer
        // will be (re)uploaded from THIS grid (resident_upload_needed was set above), and both
        // sdf_populate_resident and the field-direct DC map rv.buffer with dc_params(grid_min, grid_max).
        // Previously ONLY the field_direct branch (app_poll.rs:1997-2008) synced rv.bake_bounds; a
        // full-readback bake (the path a structural edit / SCENE-dirty rebake takes) left rv.bake_bounds
        // holding the OLD bounds while the new buffer was mapped with the NEW bounds → per-axis SQUISH +
        // sign-crossing REPEAT once the AABB went off-center/non-cubic (the "flatten-to-wall + tile" bug
        // exposed when origin re-centering was removed). Syncing here makes buffer-content bounds and
        // mesh-mapping bounds identical BY CONSTRUCTION for every accepted bake (full-readback + worker).
        if self.resident_body_nid == Some(body_id) {
            if let Some(slot_idx) = self.resident_slot {
                if let Some(slot) = self.volume_pool.get_mut(slot_idx) {
                    if let Some(ref mut rv) = slot.volume {
                        if rv.body_nid == body_id {
                            rv.bake_bounds_min = grid_min;
                            rv.bake_bounds_max = grid_max;
                        }
                    }
                }
            }
        }
        // Update progress timestamp so the timeout guard in poll() knows
        // the batch is still making progress (GPU results arrive here,
        // not through bake_rx).
        self.last_bake_result_time = Some(Instant::now());
        true
    }

    /// After a bake result is accepted, classify which 8³ bricks contain surface.
    /// Stores results in the BrickAllocator for future brick-fill dispatches.
    /// Only runs for full bakes (not partial), to avoid stale data during interaction.
    pub(crate) fn classify_bricks_for_accepted_result(&mut self, body_id: NodeId) {
        // PERF (2026-06-25): brick classification feeds ONLY the brick pool, which is OFF by default
        // (`use_brick_pool = false`, PARKED). This was the LARGEST untimed cost on the SDF-edit hot path —
        // ~2.7ms/bake at 128³, the gap between the [BakeReadback] printed steps (~2.6ms) and its total
        // (~5.3ms) on every frame the `[BrickClassify]` line appears. Its sibling `classify_bricks_for_body`
        // is already gated this way (brick_classify.rs:40); this one wasn't, so it ran every bake into a
        // pool with no consumer (resident DC returns before the brick path; brick-fill is disabled).
        // Toggling the pool ON re-classifies all cached bodies (properties_render.rs:77), so enabling it
        // still works. Net: the default (pool-off) hot path pays nothing.
        if !self.use_brick_pool { return; }
        let entry = match self.body_grids.get(&body_id) {
            Some(e) if !e.grid.is_empty() => e,
            _ => return, // no grid data (TextureOnly or missing)
        };
        let res = entry.bake_resolution;
        if res < 16 { return; } // too coarse for brick classification
        // UPPER guard: the PARKED brick pool's 65536-brick budget can't hold a >256³ body (64³=262144
        // bricks at 512³ → 0 allocated), and this classify is O(res³) CPU on poll_primary_bake — ~2.4s at
        // 512³ (the [SLOWPOLL] primary=2467 freeze when the pool is toggled on). Skip where it can't work
        // so enabling the parked pool at high res can't stall the app. (Pool stays PARKED — this only
        // prevents the footgun.)
        if res > 256 { return; }
        // Skip during partial bakes (dirty regions) — wait for full bake
        if self.dirty_regions.contains_key(&body_id) { return; }

        let (grid_min, grid_max) = self.body_bounds.get(&body_id)
            .copied()
            .unwrap_or((Vec3::splat(-self.bounds), Vec3::splat(self.bounds)));

        let dims_bricks = [
            (res + brick_allocator::BRICK_DIM - 1) / brick_allocator::BRICK_DIM,
            (res + brick_allocator::BRICK_DIM - 1) / brick_allocator::BRICK_DIM,
            (res + brick_allocator::BRICK_DIM - 1) / brick_allocator::BRICK_DIM,
        ];

        let extent = grid_max - grid_min;
        let max_extent = extent.x.max(extent.y).max(extent.z).max(0.001);
        let coarse_dims = [res, res, res];
        let active_bricks = brick_allocator::classify_bricks(
            &entry.grid, coarse_dims, dims_bricks, max_extent,
        );

        let brick_count = active_bricks.len();
        let total_bricks = dims_bricks[0] as u64 * dims_bricks[1] as u64 * dims_bricks[2] as u64;
        let pct = if total_bricks > 0 { (brick_count as f64 / total_bricks as f64 * 100.0) as u32 } else { 0 };

        let frame = brick_allocator::BrickBodyFrame {
            aabb_min: grid_min,
            aabb_max: grid_max,
            resolution: [res, res, res],
        };

        let source_hash = entry.source_hash;
        let (success, evicted) = self.brick_allocator.allocate_bricks(
            body_id, active_bricks, frame, source_hash,
        );

        if !evicted.is_empty() {
            eprintln!("[BrickClassify] Evicted {} bodies to make room for {:?}", evicted.len(), body_id);
        }

        eprintln!("[BrickClassify] body={:?} res={}³ bricks={}/{} ({}%) budget={}/{}{}",
            body_id, res, brick_count, total_bricks, pct,
            self.brick_allocator.total_bricks(), self.brick_allocator.max_bricks(),
            if success { "" } else { " FAILED" });
    }

    /// Record a TextureOnly bake completion. No grid data cached — only the hash
    /// and resolution are stored so the bake dispatch hash check recognizes the body
    /// as already baked (prevents infinite re-dispatch).
    pub(crate) fn record_texture_only_bake(
        &mut self,
        body_id: NodeId,
        source_hash: u64,
        resolution: [u32; 3],
        generation: u64,
    ) {
        let current_gen = self.generations.get(&body_id).copied().unwrap_or(0);
        if generation < current_gen { return; }
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.finish_bake_timing(body_id);
        let bake_resolution = resolution[0].max(resolution[1]).max(resolution[2]);
        let (grid_min, grid_max) = self.body_bounds.get(&body_id)
            .copied()
            .unwrap_or((Vec3::splat(-self.bounds), Vec3::splat(self.bounds)));
        // Store empty grid with valid hash — bake dispatch checks source_hash match
        self.body_grids.insert(body_id, BodyCacheEntry {
            grid: Arc::new(Vec::new()), // no grid data in TextureOnly mode
            source_hash,
            grid_min,
            grid_max,
            bake_resolution,
            shared_source: None,
            brick_resident: None, // TextureOnly: the 3D texture holds the result, not the brick atlas
            sparse_frozen: None,
            mesh_rigid_offset: Vec3::ZERO,
        });
        if self.cpu_grid_stale.remove(&body_id) { // entry replaced (TextureOnly: no CPU grid exists at all)
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                "body={:?} CLEARED (record_texture_only_bake — empty grid entry)", body_id));
        }
        self.last_bake_result_time = Some(Instant::now());
    }

    /// S2.4: record a completed SparseBricks (mode-2) bake. Sibling of `record_texture_only_bake`
    /// — an empty-grid entry with a valid hash so the dispatch hash check recognizes the body as
    /// baked — PLUS the `brick_resident` marker (`SdfBrickAtlasGpu.generation` at dispatch time)
    /// that every sparse consumer keys on. NEVER route sparse completions through
    /// `accept_gpu_result`: its `cbrt(grid.len())` resolution inference is a carry-res violation
    /// on an empty grid (it would stamp bake_resolution = 0).
    ///
    /// `bounds` are the DISPATCH-TIME bake bounds (`PendingBake.bounds_min/max`) — the same
    /// provenance rule as `accept_gpu_result`'s `dispatch_bounds`: a concurrent `aabb_escape`
    /// mutating `body_bounds` mid-flight must not re-scale this bake's brick mapping. (This
    /// deliberately improves on `record_texture_only_bake`, which still reads live `body_bounds`.)
    ///
    /// Returns true if the result was applied (not stale by generation).
    ///
    /// S2.5: `frozen` is the CPU-retained copy of the EXACT classifier index this bake uploaded
    /// to the atlas at dispatch (`upload_index`) — stored on the entry next to the
    /// `brick_resident` marker so the frozen-index partial path (`trigger_bake`) can check the
    /// dilated dirty window against it without touching the GPU or re-classifying.
    pub(crate) fn record_sparse_bake(
        &mut self,
        body_id: NodeId,
        source_hash: u64,
        resolution: [u32; 3],
        generation: u64,
        atlas_generation: u64,
        bounds: (Vec3, Vec3),
        frozen: Arc<core_math::chunked_dc::SparseIndexOnly>,
    ) -> bool {
        let current_gen = self.generations.get(&body_id).copied().unwrap_or(0);
        if generation < current_gen { return false; }
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.finish_bake_timing(body_id);
        // EXPLICIT resolution — sparse dispatches are cubic by construction (request-side guard).
        let bake_resolution = resolution[0].max(resolution[1]).max(resolution[2]);
        self.body_grids.insert(body_id, BodyCacheEntry {
            grid: Arc::new(Vec::new()), // no dense grid — the atlas POOL holds the field
            source_hash,
            grid_min: bounds.0,
            grid_max: bounds.1,
            bake_resolution,
            shared_source: None,
            brick_resident: Some(atlas_generation),
            sparse_frozen: Some(frozen),
            mesh_rigid_offset: Vec3::ZERO,
        });
        // Contract C3: entry replaced by a no-CPU-grid representation — nothing left to heal.
        if self.cpu_grid_stale.remove(&body_id) {
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                "body={:?} CLEARED (record_sparse_bake — brick-resident empty grid entry)", body_id));
        }
        self.cpu_heal_queue.remove(&body_id);
        self.pending_retighten.remove(&body_id);
        self.last_bake_result_time = Some(Instant::now());
        true
    }

    /// S2.5: record a completed FROZEN-INDEX PARTIAL sparse bake (mode 2 over a dilated dirty
    /// window, index untouched). Only pool CONTENT changed — no buffer was created or resized —
    /// so the entry is updated IN PLACE: `source_hash` refreshes (the dispatch dedup check in
    /// `trigger_bake` must recognize the moved prims as baked), while `brick_resident` (the atlas
    /// generation marker), `sparse_frozen` (the index really is frozen), bounds and resolution
    /// all stay EXACTLY as the last full sparse bake recorded them. The atlas generation must NOT
    /// bump for a partial — generation counters track buffer (re)creation, not content writes.
    ///
    /// Returns false (result dropped) when stale by generation OR when the entry is no longer the
    /// brick-resident content this partial patched (`brick_resident != Some(atlas_generation)` —
    /// a dense/regime-switch bake replaced it mid-flight; per-body in-flight coalescing makes
    /// this unreachable in practice, kept as the structural guard).
    pub(crate) fn record_sparse_partial_bake(
        &mut self,
        body_id: NodeId,
        source_hash: u64,
        generation: u64,
        atlas_generation: u64,
        update: Option<(std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>, u64)>,
    ) -> bool {
        let current_gen = self.generations.get(&body_id).copied().unwrap_or(0);
        if generation < current_gen { return false; }
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.finish_bake_timing(body_id);
        let Some(entry) = self.body_grids.get_mut(&body_id) else { return false; };
        // Validate against the TRIGGER-TIME marker (the regime this bake was checked under).
        if entry.brick_resident != Some(atlas_generation) { return false; }
        debug_assert!(entry.sparse_frozen.is_some(),
            "brick-resident entry without a frozen index — record_sparse_bake must set both");
        entry.source_hash = source_hash;
        // S2.5c: the dispatch re-uploaded an incrementally-updated index (and a pool growth may
        // have bumped the atlas generation). Install the new frozen index and move the marker to
        // the post-grow generation — deferred to completion so a superseded/dropped bake can
        // never leave the CPU index claiming handles whose pool bricks were never baked.
        if let Some((frozen, new_gen)) = update {
            entry.sparse_frozen = Some(frozen);
            entry.brick_resident = Some(new_gen);
        }
        self.last_bake_result_time = Some(Instant::now());
        true
    }

    /// GATE 2 (single producer) — union an accepted PARTIAL bake's dirty voxel window `[min, max)`
    /// into the per-body `DirtyMeshRegion` every mesher consumes. Union (not replace): if a
    /// consumer is skipped/deferred between two landings, the next consume must still cover BOTH
    /// deltas. Called ONLY at bake ACCEPTANCE (dense readback, no-copy, CPU/worker poll, sparse
    /// partial landing) — never at dispatch: a superseded/dropped bake must not leave a claim.
    pub(crate) fn note_bake_accepted_window(
        &mut self, body_id: NodeId, wmin: [u32; 3], wmax: [u32; 3],
    ) {
        self.dirty_mesh_region
            .entry(body_id)
            .and_modify(|(mn, mx)| {
                for a in 0..3 {
                    mn[a] = mn[a].min(wmin[a]);
                    mx[a] = mx[a].max(wmax[a]);
                }
            })
            .or_insert((wmin, wmax));
    }

    /// GATE 2 (single producer, full flavor) — an accepted FULL bake clears the region: occupancy
    /// may have changed anywhere, and entry-ABSENT is every consumer's conservative default
    /// (full re-mesh / full upload — incl. the tiled mesher's tile-TABLE rebuild). Never
    /// stale-incremental.
    pub(crate) fn note_full_bake_accepted(&mut self, body_id: NodeId) {
        self.dirty_mesh_region.remove(&body_id);
    }

    /// GATE 2 — convenience producer for dense accept paths: routes a full-covering accepted
    /// window to `note_full_bake_accepted` (clear) and a partial one to
    /// `note_bake_accepted_window` (union). `res3` = the grid resolution the window is against.
    pub(crate) fn note_bake_accepted(
        &mut self, body_id: NodeId, window: ([u32; 3], [u32; 3]), res3: [u32; 3],
    ) {
        let (wmin, wmax) = window;
        let full = (0..3).all(|a| wmin[a] == 0 && wmax[a] >= res3[a]);
        if full {
            self.note_full_bake_accepted(body_id);
        } else {
            self.note_bake_accepted_window(body_id, wmin, wmax);
        }
    }

    /// GATE 2 — consume the region (a mesher takes it exactly once per dispatch it actually
    /// runs; `None` = no proven delta = full re-mesh, the conservative default). Callers that
    /// may NOT dispatch must not take (peek via the field instead) — a taken-but-unmeshed
    /// region only ever degrades to a conservative full, never to stale.
    pub(crate) fn take_dirty_mesh_region(&mut self, body_id: NodeId) -> Option<super::DirtyMeshRegion> {
        self.dirty_mesh_region.remove(&body_id)
    }

    /// Mark a body's in-flight status as cleared (used when GPU bake errored
    /// and we want to allow a CPU retry).
    pub(crate) fn clear_in_flight(&mut self, body_id: NodeId) {
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.bake_start_times.remove(&body_id);
        self.dirty_bodies.insert(body_id);
    }

    /// Clear ALL in-flight trackers. Used during re-enumeration to prevent
    /// deadlock — dead worker results will never arrive, so the cache must
    /// release all locks for CPU fallback to proceed.
    pub(crate) fn clear_all_in_flight(&mut self) {
        for (body_id, _) in self.in_flight.drain() {
            self.dirty_bodies.insert(body_id);
        }
        self.bake_start_times.clear();
    }
}
