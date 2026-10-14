//! Soft falloff / move-grab deformation path + resample/rebuild.
use super::*;

impl AzraApp {
    /// MOVE Phase 2 — commit the live mesh-vertex grab into density (density-only warp, no mesh→SDF).
    /// Called on stroke release when `mesh_grab.is_active()`. Returns empty undo delta (undo is pushed
    /// on async land). Always clears grab state. Routes through `voxel_spawn_move_bake` (shared core).
    pub(super) fn voxel_commit_move_grab(&mut self) -> Vec<(u32, f32)> {
        // --- Take the grab state (always consumed on release) + symmetry for the mirror expansion ---
        let Some((grab, symmetry)) = self.sculpt.voxel.as_mut().map(|v| {
            let g = std::mem::take(&mut v.mesh_grab);
            (g, v.symmetry)
        }) else { return Vec::new(); };
        // No movement → nothing to commit (a click with no drag, or a degenerate view plane).
        if grab.last_drag.length() < 1e-5 { return Vec::new(); }
        // Expand to the symmetry-octant instances (each carries this grab's radii), append them to the
        // pending stack, and flush — the SHARED ASYNC core (`voxel_spawn_move_bake` via
        // `voxel_flush_pending_move`), the SAME path the deferred surface-mode flush uses
        // (No-Duplication: one machinery). The instances keep displaying via the shader while the
        // worker bakes; the undo entry is pushed on LAND, so this returns an empty delta and the
        // caller's push block is a structural no-op (kept for the deferred/immediate symmetry).
        let instances: Vec<crate::PendingGrab> = grab.mirror_instances(symmetry).into_iter()
            .map(|(anchor, drag)| crate::PendingGrab { anchor, drag, inner_r: grab.inner_r, outer_r: grab.outer_r })
            .collect();
        // Track release groups for Ctrl+Z (surface stack + consistency with deferred mode).
        // Without this, undo only popped 1 of N symmetry instances and Ctrl+Z felt broken.
        let group_n = instances.len();
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.pending_move_grabs.extend(instances);
            if group_n > 0 {
                v.pending_move_group_sizes.push(group_n);
                v.pending_move_redo.clear();
            }
        }
        self.voxel_flush_pending_move();
        Vec::new()
    }

    /// ASYNC Move voxelize — density-only semi-Lagrangian warp on a WORKER (no mesh→SDF).
    /// Used by immediate commit and deferred surface-mode flush — ONE machinery.
    /// Live preview stays GPU mesh grab; commit matches soft-round + mid-plane blend in density.
    /// Quality without mesh→SDF: more inverse iters on long pulls + local redistance after warp
    /// (same soft-round live feel). Typical cost ~10ms, not 200–600ms SDF.
    /// Instances stay in `pending_move_grabs` while baking (shader keeps preview); land applies
    /// density writes atomically. Returns false when nothing to bake or a bake is already in flight.
    pub(super) fn voxel_spawn_move_bake(&mut self) -> bool {
        // Refuse while a bake is in flight, OR while a resolution rebuild is (the grid is about to
        // be swapped — a bake against the old grid would just be dropped by the land res-guard; the
        // armed timer retries after the swap and bakes against the NEW grid instead).
        if self.sculpt.voxel.as_ref().map_or(true, |v| v.pending_move_bake.is_some() || v.pending_rebuild.is_some()) { return false; }
        let instances: Vec<crate::PendingGrab> =
            self.sculpt.voxel.as_ref().map(|v| v.pending_move_grabs.clone()).unwrap_or_default();
        if instances.is_empty() { return false; }
        // DYNAMIC GRID GROW (3D-Coat "no cube limit") — runs SYNCHRONOUSLY first when the pull would
        // clamp at the sealed border. Returns a pre-grow full-grid snapshot for UndoKind::Grid.
        let (grew, pre_grid) = self.voxel_grow_for_move(&instances);
        let Some((res, bounds, voxel_size)) = self.sculpt.voxel.as_ref()
            .map(|v| (v.grid.res, v.grid.bounds, v.grid.voxel_size())) else { return false; };

        // LOCAL world-pos → grid cell, sealed-border clamped [1, res-2] (POST-GROW res/bounds).
        let to_cell = |p: Vec3, ceil: bool| {
            let gc = ((p + Vec3::splat(bounds)) / (2.0 * bounds)) * (res - 1) as f32;
            let lo = 1.0;
            let hi = (res as f32 - 2.0).max(1.0);
            let comp = |c: f32| (if ceil { c.ceil() } else { c.floor() }).clamp(lo, hi) as u32;
            [comp(gc.x), comp(gc.y), comp(gc.z)]
        };
        let w = 2.0 * voxel_size; // iso band: surface (sdf=0) → density 0.5 over ~1-2 voxels

        // Per-instance swept AABBs (anchor ± R ∪ dest ± R). Extra halo so soft tip + lattice
        // rounding never drop the edge of the stroke on write (owner: rebake removed last stroke).
        let mut inst_aabbs: Vec<[u32; 6]> = Vec::with_capacity(instances.len());
        for g in &instances {
            let pad = Vec3::splat(g.outer_r + 3.0 * voxel_size);
            let c0 = to_cell(g.anchor - pad + g.drag.min(Vec3::ZERO), false);
            let c1 = to_cell(g.anchor + pad + g.drag.max(Vec3::ZERO), true);
            if c0[0] <= c1[0] && c0[1] <= c1[1] && c0[2] <= c1[2] {
                inst_aabbs.push([c0[0], c0[1], c0[2], c1[0], c1[1], c1[2]]);
            }
        }
        if inst_aabbs.is_empty() {
            // Degenerate (all AABBs empty) — abandon the stack, matching the old sync early-return
            // (which had already consumed it). Keeping it would re-display + re-flush forever.
            self.voxel_drain_baked_grabs(instances.len());
            return false;
        }

        // DENSITY-ONLY voxelize. Snapshot density for the worker (not full VoxelGrid colors/mask).
        // High-res cost is dominated by warp cell count (sphere-sweep), not this clone.
        let Some((density, iso)) = self.sculpt.voxel.as_ref().map(|v| (v.grid.density.clone(), v.grid.iso)) else {
            self.voxel_drain_baked_grabs(instances.len());
            return false;
        };
        let baked_n = instances.len();
        let (tx, rx) = std::sync::mpsc::channel();
        let aabbs_for_result = inst_aabbs;
        let inst_pack: Vec<(glam::Vec3, glam::Vec3, f32)> = instances.iter()
            .map(|g| (g.anchor, g.drag, g.outer_r)).collect();
        let spawn_ok = std::thread::Builder::new()
            .name("voxel-move-bake".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let mut grid = core_math::voxel_sculpt::VoxelGrid::from_density_field(
                    density, res, bounds, iso,
                );
                let max_drag = inst_pack.iter().map(|(_, d, _)| d.length()).fold(0.0f32, f32::max);
                let max_r = inst_pack.iter().map(|(_, _, r)| *r).fold(0.0f32, f32::max);
                let n_inst = inst_pack.len();
                let (writes, dirty) = grid.grab_density_displace_writes(&inst_pack);
                let ms = t0.elapsed().as_secs_f32() * 1000.0;
                let span = if dirty[0] <= dirty[3] {
                    (dirty[3] - dirty[0] + 1) as u64
                        * (dirty[4] - dirty[1] + 1) as u64
                        * (dirty[5] - dirty[2] + 1) as u64
                } else { 0 };
                eprintln!(
                    "[MOVEBAKE] worker: density_warp={:.0}ms writes={} inst={} drag/r={:.2} dirty_cells={} dirty=[{},{},{}]-[{},{},{}]",
                    ms, writes.len(), n_inst, max_drag / max_r.max(1e-6), span,
                    dirty[0], dirty[1], dirty[2], dirty[3], dirty[4], dirty[5],
                );
                let _ = tx.send(crate::MoveBakeResult {
                    union: dirty,
                    sdf: Vec::new(),
                    w,
                    density_writes: writes,
                    res,
                    bounds,
                    inst_aabbs: aabbs_for_result,
                });
            }).is_ok();
        if !spawn_ok {
            self.voxel_drain_baked_grabs(baked_n);
            return false;
        }
        if let Some(v) = self.sculpt.voxel.as_mut() {
            let push_density = pre_grid.is_none();
            let has_grid_snap = pre_grid.is_some();
            v.pending_move_bake = Some(crate::PendingMoveBake {
                rx, baked_n, push_undo: push_density, grew, pre_grid,
            });
            eprintln!("[MOVEBAKE] spawn: density-only instances={} grew={} grid_undo={} density_undo={}",
                baked_n, grew, has_grid_snap, push_density);
        }
        true
    }

    /// Drain the leading `n` baked (or abandoned) `pending_move_grabs` entries and manage the
    /// auto-voxelize timer: cleared when the stack empties, re-armed when Move strokes appended new
    /// entries while the bake was in flight (they bake in the NEXT flush).
    pub(super) fn voxel_drain_baked_grabs(&mut self, n: usize) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            let n = n.min(v.pending_move_grabs.len());
            v.pending_move_grabs.drain(..n);
            // Drop matching release-group sizes from the front (bake covers the oldest entries).
            let mut left = n;
            while left > 0 && !v.pending_move_group_sizes.is_empty() {
                let g = v.pending_move_group_sizes[0];
                if g <= left {
                    v.pending_move_group_sizes.remove(0);
                    left -= g;
                } else {
                    v.pending_move_group_sizes[0] = g - left;
                    left = 0;
                }
            }
            // Surface-mode redo is only for unbaked undos; a successful bake commits density.
            if v.pending_move_grabs.is_empty() {
                v.pending_move_group_sizes.clear();
                v.pending_move_redo.clear();
            }
            v.move_voxelize_due = if v.pending_move_grabs.is_empty() { None }
                else { Some(std::time::Instant::now() + std::time::Duration::from_secs(2)) };
        }
    }

    /// Poll wrapper — called every frame (from `voxel_flush_dirty_chunks`): lands a finished Move
    /// bake without blocking; a no-op while the worker is still running or no bake is in flight.
    pub(crate) fn voxel_poll_move_bake(&mut self) { self.voxel_land_move_bake(false); }

    /// CANCEL any in-flight Move bake + discard the whole pending stack (timer cleared). For
    /// wholesale-discard situations — project load, Clear voxels — where the pre-state is being
    /// thrown away and landing the bake would resurrect stale geometry / push undo entries onto
    /// freshly-reset stacks (review P0/P2). Dropping the receiver is enough: the worker's send
    /// fails silently and the thread exits.
    pub(crate) fn voxel_cancel_move_bake(&mut self) {
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.pending_move_bake = None;
            v.pending_move_grabs.clear();
            v.pending_move_group_sizes.clear();
            v.pending_move_redo.clear();
            v.move_voxelize_due = None;
        }
    }

    /// Land the ASYNC Move bake. `block=false` = the per-frame poll (returns immediately if the
    /// worker isn't done). `block=true` = the sync points (save / leave-edit / remesh / grab-cap
    /// overflow) that REQUIRE the grid final before proceeding — the one-shot blocking case.
    pub(crate) fn voxel_land_move_bake(&mut self, block: bool) {
        let Some(bake) = self.sculpt.voxel.as_mut().and_then(|v| v.pending_move_bake.take()) else { return; };
        let result = if block {
            bake.rx.recv().ok() // None ⇒ worker died (panicked) — handled below
        } else {
            match bake.rx.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Worker still running — put the bake back; try again next frame.
                    if let Some(v) = self.sculpt.voxel.as_mut() { v.pending_move_bake = Some(bake); }
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let Some(r) = result else {
            // Worker died — abandon this stack (never leave it re-displaying forever).
            self.voxel_drain_baked_grabs(bake.baked_n);
            // The grid GREW at spawn (grow is synchronous) — the chunk grid is the WRONG size even
            // though the bake never landed; the rebuild must still run (review: grew-abandon).
            if bake.grew { self.voxel_spawn_mesh_rebuild(); }
            self.toast("Move voxelize failed — move discarded");
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        };
        // Guard: the live grid must still be the one the bake was computed against (should be
        // impossible to violate — strokes/undo/remesh are paused while in flight — but a stale
        // write into a changed grid would corrupt it, so verify structurally).
        let live = self.sculpt.voxel.as_ref().map(|v| (v.grid.res, v.grid.bounds));
        if live != Some((r.res, r.bounds)) {
            self.voxel_drain_baked_grabs(bake.baked_n);
            // grew-abandon (review): the spawn-time grow resized the grid; if it has since changed
            // AGAIN the changer owns the chunks, but a rebuild is safe-redundant — never wrong-sized.
            if bake.grew { self.voxel_spawn_mesh_rebuild(); }
            self.toast("Move voxelize dropped (grid changed)");
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        let pre_grid = bake.pre_grid;
        let mut density_delta: Option<Vec<(u32, f32)>> = None;
        // Fast density path (preferred).
        if !r.density_writes.is_empty() {
            let delta: Vec<(u32, f32)> = r.density_writes.iter().map(|&(i, pre, _)| (i, pre)).collect();
            if let Some(v) = self.sculpt.voxel.as_mut() {
                for &(idx, _pre, post) in &r.density_writes {
                    v.grid.density.set_idx(idx as usize, post);
                }
            } else {
                return;
            }
            eprintln!("[MOVEBAKE] land: density_writes={} push_undo={} grew={} undo_order_len={}",
                r.density_writes.len(), bake.push_undo, bake.grew, self.sculpt.voxel_undo_order.len());
            density_delta = Some(delta);
            // ONE remesh path: grow → full rebuild only; else ONE union chunk mark (not inst+union).
            if !bake.grew {
                self.voxel_mark_chunks_in_region(r.union);
            }
        } else if !r.sdf.is_empty() {
            // Mesh→SDF landing (Move bake + surface revoxelize). SIGN RECONCILIATION first
            // (fuzz-caught, seed 5296): far-cell signs from the nearest-triangle pseudo-normal
            // flip over folded/pinched stroke geometry, and write_region_from_sdf turns one
            // flipped far cell into solid garbage (whole slabs at the region edge). Flood-fill
            // the far-cell signs from the trusted pre-bake density around the region before
            // writing; the ±w band keeps the geometric sign.
            let mut sdf = r.sdf;
            let delta = match self.sculpt.voxel.as_mut() {
                Some(v) => {
                    let flipped = v.grid.reconcile_region_sdf_signs(r.union, &mut sdf, r.w);
                    if flipped > 0 {
                        eprintln!("[MOVEBAKE] sign reconciliation flipped {flipped} far cells (folded stroke geometry)");
                    }
                    v.grid.write_region_from_sdf(r.union, &sdf, r.w)
                }
                None => return,
            };
            eprintln!("[MOVEBAKE] land: sdf_delta={} push_undo={} grew={}",
                delta.len(), bake.push_undo, bake.grew);
            density_delta = Some(delta);
            if !bake.grew {
                self.voxel_mark_chunks_in_region(r.union);
            }
        }
        if let Some(pre) = pre_grid {
            self.voxel_push_grid_undo(pre);
        } else if bake.push_undo {
            if let Some(delta) = density_delta {
                if !delta.is_empty() {
                    let evicted = self.sculpt.voxel_undo_stack.push(delta);
                    self.sculpt.voxel_undo_order.push_back(UndoKind::Cpu);
                    prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Cpu, evicted);
                    self.sculpt.voxel_redo_stack.clear();
                    self.sculpt.voxel_color_redo_stack.clear();
                    self.sculpt.voxel_grid_redo_stack.clear();
                    self.sculpt.voxel_redo_order.clear();
                }
            }
        }
        self.voxel_drain_baked_grabs(bake.baked_n);
        // Grid GREW at spawn → ONE full off-screen remesh (do NOT also chunk-remesh above).
        if bake.grew { self.voxel_spawn_mesh_rebuild(); }
        // Strokes released during this bake left entries in pending_move_grabs — bake them next
        // without waiting for the idle timer (keeps multi-stroke Move responsive).
        if self.sculpt.voxel.as_ref().map_or(false, |v| !v.pending_move_grabs.is_empty() && v.pending_move_bake.is_none()) {
            let _ = self.voxel_flush_pending_move();
        }
        // Do NOT full-drain dirty chunks here — that re-froze the UI for ~1s after bake land.
        // Mark dirty + normal per-frame budget drain in voxel_flush_dirty_chunks.
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// DEFERRED VOXELIZE flush (3D-Coat surface mode): bake the accumulated `pending_move_grabs` stack into the
    /// density grid in ONE pass, commit ONE sculpt-undo entry, full-drain the swept chunks, and clear the stack
    /// + the auto-voxelize timer. Returns `true` if anything was voxelized (`false` if the stack was empty).
    /// REUSES `voxel_revoxelize_instances` (the same core the immediate commit uses) — no second code path.
    /// Triggered by: the Voxelize button, the Enter key, the ~2s idle timer, a near-full accumulation buffer,
    /// and the gate that flushes before a non-Move brush stroke / save (so density brushes never fight a stale
    /// grid). After baking, the body's surface mode is OFF (empty stack) → the shader stops displacing.
    pub(crate) fn voxel_flush_pending_move(&mut self) -> bool {
        // ASYNC (the frozen-frame fix): spawn the worker bake over the current stack. The instances
        // STAY in `pending_move_grabs` (the shader keeps displaying the displaced surface); the undo
        // push + per-instance dirty marks + drain + full-drain all happen on LAND
        // (`voxel_land_move_bake`, polled every frame). Returns false when the stack is empty or a
        // bake is already in flight (the in-flight case keeps the timer armed → retried after land).
        let spawned = self.voxel_spawn_move_bake();
        if spawned {
            // Timer off while baking — land re-arms it if Move strokes appended new entries meanwhile.
            if let Some(v) = self.sculpt.voxel.as_mut() { v.move_voxelize_due = None; }
        }
        spawned
    }

    /// Receive worker-meshed chunks for the OFF-SCREEN resolution rebuild and upload each into its
    /// own buffer while the live surface keeps rendering. The expensive meshing runs on the worker
    /// thread (see `voxel_resample_to`); this only uploads, capped per frame so the GPU isn't
    /// flooded. When all chunks have landed, swap the new grid/chunks in atomically and free the
    /// old buffers — the resolution change appears as one instant quality upgrade.
    pub(super) fn voxel_flush_pending_rebuild(&mut self) {
        // Drain worker-streamed chunks under a FRAME BUDGET. At 1024³ ~10–15k surface chunks:
        // a flat 6ms budget left remesh-tail multi-second; scale up when the frame is free.
        // HARD_CAP is a runaway-loop backstop.
        const HARD_CAP: usize = 16_384;
        if self.sculpt.voxel.as_ref().and_then(|v| v.pending_rebuild.as_ref()).is_none() { return; }
        let prev_wall = self.prev_frame_wall_ms;
        let target_res = self.sculpt.voxel.as_ref().map_or(64, |v| v.remesh_target_res.max(v.grid.res));
        // 1024³: apply more CPU mesh uploads per frame when UI is free (worker is the bottleneck otherwise).
        let frame_budget_ms: f32 = if prev_wall > 35.0 {
            4.0
        } else if prev_wall > 22.0 {
            7.0
        } else if target_res >= 896 {
            24.0 // 1024³ stream land — free frames push hard (was 18)
        } else if target_res >= 512 {
            14.0
        } else {
            8.0
        };
        let t_start = std::time::Instant::now();
        let mut applied = 0usize;
        let mut nonempty = 0u32;
        while applied < HARD_CAP {
            let item = match self.sculpt.voxel.as_ref().and_then(|v| v.pending_rebuild.as_ref()) {
                Some(p) => p.rx.try_recv(),
                None => return,
            };
            match item {
                Ok(crate::VoxelRebuildMsg::Total(t)) => {
                    if let Some(p) = self.sculpt.voxel.as_mut().and_then(|v| v.pending_rebuild.as_mut()) { p.total = Some(t); }
                }
                Ok(crate::VoxelRebuildMsg::Grid(g)) => {
                    if let Some(p) = self.sculpt.voxel.as_mut().and_then(|v| v.pending_rebuild.as_mut()) { p.grid = Some(g); }
                }
                Ok(crate::VoxelRebuildMsg::EmptyChunk(ci)) => {
                    // Surface mask hit, zero tris — still progress, no mesh_cache write.
                    if let Some(p) = self.sculpt.voxel.as_mut().and_then(|v| v.pending_rebuild.as_mut()) {
                        if (ci as usize) < p.chunk_tris.len() { p.chunk_tris[ci as usize] = 0; }
                        p.done += 1;
                    }
                    applied += 1;
                }
                Ok(crate::VoxelRebuildMsg::Chunk(ci, verts, idx)) => {
                    let tris = (idx.len() / 3) as u32;
                    let cid = self.sculpt.voxel.as_ref().and_then(|v| v.pending_rebuild.as_ref())
                        .and_then(|p| p.chunks.get(ci as usize).copied());
                    if let Some(cid) = cid {
                        self.cache.mesh_cache.update_editable_mesh(cid, (verts, idx), tris);
                    }
                    if let Some(p) = self.sculpt.voxel.as_mut().and_then(|v| v.pending_rebuild.as_mut()) {
                        if cid.is_some() && (ci as usize) < p.chunk_tris.len() { p.chunk_tris[ci as usize] = tris; }
                        p.done += 1;
                    }
                    if tris > 0 { nonempty += 1; }
                    applied += 1;
                }
                Err(_) => break, // Empty (nothing ready yet) or Disconnected (worker finished/superseded)
            }
            if t_start.elapsed().as_secs_f32() * 1000.0 >= frame_budget_ms { break; }
        }
        let _ = nonempty; // counted for future diagnostics
        // Swap once the resampled grid arrived (sent last) AND every surface chunk is in — atomic
        // upgrade, then free the old chunk buffers. The old surface rendered throughout.
        let done = self.sculpt.voxel.as_ref().and_then(|v| v.pending_rebuild.as_ref())
            .map_or(false, |p| p.grid.is_some() && p.total.map_or(false, |t| p.done >= t));
        if done {
            let mut old_ids = Vec::new();
            let mut surface_n = 0u32;
            let mut pool_hint = 0u32;
            if let Some(v) = self.sculpt.voxel.as_mut() {
                if let Some(mut p) = v.pending_rebuild.take() {
                    old_ids = std::mem::replace(&mut v.chunks, p.chunks);
                    if let Some(g) = p.grid.take() { v.grid = g; }
                    v.chunk_dim = p.chunk_dim;
                    v.chunk_tris = p.chunk_tris;
                    v.tris = v.chunk_tris.iter().sum();
                    // Real shell size for GPU pool (not chunk_dim³).
                    surface_n = v.chunk_tris.iter().filter(|&&t| t > 0).count() as u32;
                    if surface_n == 0 {
                        surface_n = p.total.unwrap_or(0) as u32;
                    }
                    v.surface_chunk_count = surface_n;
                    pool_hint = surface_n.saturating_add(surface_n / 4).max(256).min(16_384);
                    v.dirty_chunks.clear(); // any sculpt marks queued meanwhile referenced the old grid
                    // Surface-mode bookkeeping is keyed on CHUNK IDENTITY (index + MeshCacheId) —
                    // void after the wholesale swap. Stale entries would resurrect: a snapshot
                    // restore into a new-identity chunk, a moved-AABB hit on a re-numbered chunk,
                    // or a `synced` mark letting a verts-only upload land over a slot that now
                    // holds DIFFERENT topology (old indices over new vert order = display spikes).
                    v.surface_moved_aabbs.clear();
                    v.surface_pool_synced.clear();
                    v.stroke_aabb = None;   // coords referenced the old grid/res
                    v.atlas_stroke = false; // the old atlas is gone (grid swapped) — no release sync owed
                    v.stroke_uses_atlas = None;
                    // Seed resident-cap for progressive GPU catch-up after big remesh (1024³).
                    if v.grid.res >= 512 {
                        v.resident_cap = v.resident_cap.max(8).min(32);
                    }
                }
            }
            // Discard barrier (NOT a commit): live-stroke snapshots reference the OLD chunk
            // identities — an undo entry built from them would restore into swapped topology.
            self.sculpt.surface_stroke_snap.clear();
            self.sculpt.surface_stroke_ledger.clear();
            for id in old_ids { self.cache.mesh_cache.remove(id); }
            // The grid was replaced wholesale → the persistent mesh-density buffer (Phase 3c) AND the
            // sparse brick atlas (Phase 4b-2) are stale. Drop both so the next dispatch recreates +
            // full-uploads from the new grid (a `None` mesh inflight here is fine; any in-flight buffer
            // references survive in the command buffer). The new grid's `gc_pending` would also force a
            // full atlas upload, but dropping it here is the explicit, swap-safe reset.
            if let Some(g) = self.gpu.as_mut() {
                g.voxel_mesh_density = None; g.voxel_mesh_density_res = 0;
                g.voxel_mesh_atlas = None;
                // Phase 6 + POOL: chunk indices changed (new chunk_dim) → the resident chunks are now stale
                // geometry for the new grid. Clear them so the new surface re-meshes from scratch (the not-
                // yet-resident fallback draws the worker-rebuilt CPU mesh meanwhile). Reset the pool slot
                // allocator too — the dropped chunks' slots would otherwise leak the pool to exhaustion.
                let queue = g.queue.clone();
                g.voxel_chunk_gpu.clear();
                if let Some(m) = g.voxel_mesh.as_mut() { m.reset_pool_allocator(&queue); }
                // Size pool to surface shell immediately (not full chunk_dim³).
                g.voxel_pool_grow_to = pool_hint;
            }
            eprintln!(
                "[REMESH] swap landed surface_chunks={} pool_hint={} (CPU mesh ready; GPU pool grows async)",
                surface_n, pool_hint
            );
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Resample the voxel grid to a new resolution (the remesh / resolution control). The new
    /// surface is meshed on a WORKER THREAD (off the UI thread, so a high-res rebuild never stalls
    /// the app) and streamed back chunk-by-chunk; the main thread uploads each into its own buffer
    /// while the CURRENT surface keeps rendering, then swaps the whole set in atomically once all
    /// chunks land (`voxel_flush_pending_rebuild`). A "Remeshing NN%" overlay shows progress.
    /// Resets undo (the grid identity changes on swap).
    /// Reset the ENTIRE voxel undo system — both CPU-delta stacks, both Phase-A.2 order-marker deques,
    /// the GPU brick-snapshot ring + in-progress builder, and any queued atlas restore. Called wherever
    /// the grid IDENTITY changes (create / resample swap / project new/load / import): the old GPU pool
    /// handles those snapshots reference no longer exist, so applying a stale entry would corrupt the new
    /// grid. ONE place to keep the two undo systems in lockstep (no "remember to clear all of them" trap).
    pub(crate) fn voxel_undo_reset(&mut self) {
        // Any live-stroke snapshot references the dying grid identity — discard, never commit.
        self.sculpt.surface_stroke_snap.clear();
        self.sculpt.surface_stroke_ledger.clear();
        self.sculpt.voxel_undo_stack.clear();
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_color_undo_stack.clear(); // colour-paint (Fill All) shares the grid identity → reset too
        self.sculpt.voxel_color_redo_stack.clear();
        self.sculpt.voxel_grid_undo_stack.clear();
        self.sculpt.voxel_grid_redo_stack.clear();
        self.sculpt.voxel_surface_undo_stack.clear();
        self.sculpt.voxel_surface_redo_stack.clear();
        self.sculpt.voxel_undo_order.clear();
        self.sculpt.voxel_redo_order.clear();
        self.sculpt.pending_atlas_restore.clear();
        if let Some(g) = self.gpu.as_mut() {
            g.voxel_atlas_undo = Default::default();
            g.voxel_atlas_undo_building = None;
        }
    }

    /// Push a full pre-change grid snapshot onto the sculpt undo stream (`UndoKind::Grid`).
    /// Used for Move grow and Ctrl+R remesh so resolution changes are Ctrl+Z-able.
    pub(crate) fn voxel_push_grid_undo(&mut self, rec: core_math::voxel_sculpt::VoxelGridRecord) {
        let evicted = self.sculpt.voxel_grid_undo_stack.push(rec);
        self.sculpt.voxel_undo_order.push_back(UndoKind::Grid);
        prune_front_undo_markers(&mut self.sculpt.voxel_undo_order, UndoKind::Grid, evicted);
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_color_redo_stack.clear();
        self.sculpt.voxel_grid_redo_stack.clear();
        self.sculpt.voxel_redo_order.clear();
    }

    /// Restore a full grid from a snapshot: swap density/res/bounds, rebuild chunks, clear surface Moves.
    pub(crate) fn voxel_restore_grid_record(&mut self, rec: &core_math::voxel_sculpt::VoxelGridRecord) {
        let new_grid = core_math::voxel_sculpt::VoxelGrid::from_record(rec);
        if let Some(v) = self.sculpt.voxel.as_mut() {
            v.grid = new_grid;
            v.pending_move_grabs.clear();
            v.pending_move_group_sizes.clear();
            v.pending_move_redo.clear();
            v.move_voxelize_due = None;
            v.mesh_grab.cancel();
            v.pending_move_bake = None;
            v.cpu_grid_stale = false;
            v.atlas_stroke = false;
            // Surface-mode working state is meaningless against the restored grid.
            v.surface_dirty_region = None;
            v.surface_moved_aabbs.clear();
            v.surface_pool_synced.clear();
        }
        self.sculpt.surface_stroke_snap.clear(); // discard barrier — snapshots reference the old grid
        self.sculpt.surface_stroke_ledger.clear();
        self.voxel_surface_clear_undo();
        // Full surface rebuild at restored resolution (same path as post-grow / remesh).
        self.voxel_spawn_mesh_rebuild();
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
    }

    pub(crate) fn voxel_resample_to(&mut self, new_res: u32) -> u32 {
        // Drain any in-flight async GPU brush batch into the grid BEFORE we clone it — its edits are
        // still on the GPU and would otherwise be lost when the worker resamples the stale clone. The
        // collect's undo records are dropped below with the stack (grid identity changes on swap), but
        // the density edits must be captured into the grid first. Also clears the inflight so a later
        // collect can't index the about-to-be-replaced grid at the wrong resolution (C7).
        let _t_mat = std::time::Instant::now();
        self.voxel_brush_drain();
        // SURFACE MODE: un-voxelized mesh edits would be silently lost by resampling the stale
        // grid — revoxelize them first; the block-land drain loop below lands the bake.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.surface_dirty_region.is_some()) {
            self.voxel_surface_revoxelize();
        }
        // Drain the ENTIRE pending-Move stack before the resample snapshot (flush+block-land in a
        // loop — grabs may have been appended while a bake was in flight). Grabs must NEVER survive
        // a resolution change: their bake would target the swapped grid, and on a dense→sparse
        // remesh a later Move stroke would take the grid-WRITING elastic path mid-bake (review P0).
        let mut _drain_guard = 0u32;
        while self.sculpt.voxel.as_ref().map_or(false, |v|
            v.pending_move_bake.is_some() || !v.pending_move_grabs.is_empty()) {
            self.voxel_flush_pending_move();
            self.voxel_land_move_bake(true);
            _drain_guard += 1;
            if _drain_guard > 8 { break; } // backstop; every land drains ≥1 baked/abandoned grab
        }
        // Phase A.2/B: the atlas path leaves the CPU grid stale (no per-stroke readback). Materialise it
        // from the GPU atlas ONCE here (no-op if not stale) so the worker resamples the CURRENT surface.
        self.materialize_cpu_from_atlas();
        // Clone only the CURRENT grid (cheap vs resampling) and hand it to the worker; the
        // expensive resample + surface scan + meshing ALL run off the UI thread.
        let old_grid = match self.sculpt.voxel.as_ref() { Some(v) => v.grid.clone(), None => return 0 };
        // Snapshot pre-remesh grid for Ctrl+Z (UndoKind::Grid restores res + density).
        let pre_remesh = old_grid.to_record();
        eprintln!("[REMESH→{}³] materialize+clone={:.0}ms", new_res, _t_mat.elapsed().as_secs_f32() * 1000.0);
        let res = new_res.max(2); // VoxelGrid::new_empty clamps res to >= 2
        let chunk_dim = voxel_chunk_dim(res);
        let n = (chunk_dim * chunk_dim * chunk_dim) as usize;
        // Allocate fresh chunk ids for the new resolution (the old ids keep rendering until swap).
        let mut chunks = Vec::with_capacity(n);
        for _ in 0..n { chunks.push(self.cache.mesh_cache.alloc_id()); }
        // Worker: resample (the ~1s cost), find surface chunks, mesh them — streaming Total → each
        // Chunk/EmptyChunk → the resampled Grid (moved last). Stops if the receiver is dropped.
        // Large stack for 1024³ sparse resample + Surface-Nets (default 2MB can OOM stack on deep calls).
        let (tx, rx) = std::sync::mpsc::channel::<crate::VoxelRebuildMsg>();
        // 32MB stack for 1024³ sparse resample + Surface-Nets (default 2MB can blow stack).
        std::thread::Builder::new()
            .name("voxel-remesh".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let _t = std::time::Instant::now();
                let new_grid = old_grid.resample(new_res);
                let _t_resample = _t.elapsed();
                let mask = new_grid.surface_chunk_mask(VOXEL_CHUNK);
                let _t_mask = _t.elapsed() - _t_resample;
                let mut surface: Vec<u32> = mask.iter().enumerate().filter_map(|(ci, &h)| if h { Some(ci as u32) } else { None }).collect();
                // Spatial order → better density brick cache locality across rayon workers @1024³.
                surface.sort_unstable();
                eprintln!("[REMESH→{}³] resample={:.0}ms surface_chunk_mask={:.0}ms surface_chunks={}",
                    new_res, _t_resample.as_secs_f32() * 1000.0, _t_mask.as_secs_f32() * 1000.0, surface.len());
                if tx.send(crate::VoxelRebuildMsg::Total(surface.len())).is_err() { return; }
                let _t_mesh = std::time::Instant::now();
                // PARALLEL chunk meshing. Empty mask hits → EmptyChunk (no mesh_cache write on main).
                {
                    use rayon::prelude::*;
                    surface.par_iter().for_each_with(tx.clone(), |tx, &ci| {
                        let (a0, a1) = voxel_chunk_anchor(ci, chunk_dim, res);
                        let (verts, idx) = new_grid.mesh_region(a0, a1);
                        if verts.is_empty() || idx.is_empty() {
                            let _ = tx.send(crate::VoxelRebuildMsg::EmptyChunk(ci));
                        } else {
                            let _ = tx.send(crate::VoxelRebuildMsg::Chunk(ci, verts, idx));
                        }
                    });
                }
                eprintln!("[REMESH→{}³] mesh={:.0}ms (worker total={:.0}ms)",
                    new_res, _t_mesh.elapsed().as_secs_f32() * 1000.0, _t.elapsed().as_secs_f32() * 1000.0);
                let _ = tx.send(crate::VoxelRebuildMsg::Grid(new_grid));
            })
            .expect("spawn voxel-remesh worker");
        if let Some(v) = self.sculpt.voxel.as_mut() {
            // If a rebuild was already pending (rapid Ctrl+R), free its orphaned chunk ids first.
            let prev = v.pending_rebuild.take();
            v.remesh_target_res = res;
            v.pending_rebuild = Some(crate::PendingVoxelRebuild {
                chunk_dim, chunks, chunk_tris: vec![0; n], rx, grid: None, total: None, done: 0,
            });
            if let Some(p) = prev { for id in p.chunks { self.cache.mesh_cache.remove(id); } }
        }
        // Grid identity will change on swap → clear density/atlas/color deltas (indices won't map).
        // Keep a Grid snapshot of the pre-remesh surface so Ctrl+Z restores previous resolution.
        self.sculpt.voxel_undo_stack.clear();
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_color_undo_stack.clear();
        self.sculpt.voxel_color_redo_stack.clear();
        self.sculpt.voxel_grid_redo_stack.clear();
        self.sculpt.voxel_redo_order.clear();
        self.sculpt.voxel_undo_order.retain(|&k| k == UndoKind::Grid);
        if let Some(g) = self.gpu.as_mut() {
            g.voxel_atlas_undo = Default::default();
            g.voxel_atlas_undo_building = None;
        }
        self.sculpt.pending_atlas_restore.clear();
        self.voxel_push_grid_undo(pre_remesh);
        self.sculpt.current_stroke.clear();
        self.sculpt.voxel_stroke.clear();
        self.sculpt.stroke_active = false;
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!("Remeshing to {}\u{B3}\u{2026} (Ctrl+Z restores previous res)", new_res));
        0
    }

    /// DYNAMIC VOXEL GRID GROW for the Move tool (3D-Coat "no cube limit"): if re-voxelizing `instances`
    /// would push the displaced surface to/past the sealed border `[1, res-2]` (where the old `to_cell`
    /// clamp filled the box into a perfect cube), regrid the CPU grid to a LARGER world extent so the
    /// surface fits with margin instead. Returns `true` if it grew (the caller's `res`/`bounds` are now
    /// stale — re-read them).
    ///
    /// FRAME ORDERING: this does NO `device.create_*` — it only resamples the CPU grid in place
    /// (`resample_to`) and resets undo. The caller (`voxel_revoxelize_instances`) writes the displaced
    /// density, then spawns the OFF-SCREEN mesh rebuild (`voxel_spawn_mesh_rebuild`); the new grid SWAPS in +
    /// the GPU density buffer / atlas / chunk pool are recreated under the Frame Ordering Contract by the
    /// poll-phase `voxel_flush_pending_rebuild` → `voxel_reset_gpu_after_grid_swap` → `voxel_ensure_pool`
    /// chain (the SAME machinery a Ctrl+R resolution change uses). Safe to call from the input handler
    /// (immediate Move commit) OR the poll (deferred surface-mode flush): the swap can only land in a later poll.
    ///
    /// Returns `(grew, pre_grid_snapshot)`. When grew, `pre_grid` is the full pre-grow/pre-move
    /// `VoxelGridRecord` for `UndoKind::Grid` on land (restores res + shape in one Ctrl+Z).
    pub(super) fn voxel_grow_for_move(
        &mut self,
        instances: &[crate::PendingGrab],
    ) -> (bool, Option<core_math::voxel_sculpt::VoxelGridRecord>) {
        // Bounds-only expand when a Move pull would leave the *usable* volume (sealed border).
        // Owner image: pull past the box → flat cut + debris (density has nowhere to write).
        // NEVER bump resolution (that was the 128→224→255 multi-second freeze). Same res, larger
        // world cube only — voxels get slightly coarser, free form keeps working.
        let Some((res, bounds)) = self.sculpt.voxel.as_ref().map(|v| (v.grid.res, v.grid.bounds)) else {
            return (false, None);
        };
        if bounds <= 0.0 || instances.is_empty() { return (false, None); }
        let voxel_size = (bounds * 2.0) / (res - 1).max(1) as f32;
        // Usable half-extent: sealed border cells [1, res-2] ≈ bounds − 2·vs.
        let usable = (bounds - 2.0 * voxel_size).max(voxel_size);
        let mut needed = 0.0f32;
        for g in instances {
            // Full soft tip must land inside usable space (not just the core).
            let pad = g.outer_r + 2.0 * voxel_size;
            let dest = g.anchor + g.drag;
            for p in [g.anchor, dest] {
                needed = needed
                    .max(p.x.abs() + pad)
                    .max(p.y.abs() + pad)
                    .max(p.z.abs() + pad);
            }
        }
        if needed <= usable {
            return (false, None);
        }
        // Generous headroom so grow doesn't fire every stroke (owner: GROW thrash + 15–60ms release).
        let new_bounds = (needed * 1.20 + 20.0 * voxel_size).max(bounds * 1.15);
        let new_res = res; // KEEP resolution
        if new_bounds <= bounds + 1e-4 {
            return (false, None);
        }

        let pre = self.sculpt.voxel.as_ref().map(|v| v.grid.to_record());
        self.materialize_cpu_from_atlas();
        let grown = match self.sculpt.voxel.as_ref() {
            Some(v) => v.grid.resample_to(new_res, new_bounds),
            None => return (false, None),
        };
        let grown_res = grown.res;
        let grown_bounds = grown.bounds;
        if let Some(v) = self.sculpt.voxel.as_mut() { v.grid = grown; }
        // Density-delta indices map to the old grid — clear those lanes. Grid undo restores res+shape.
        self.sculpt.voxel_undo_stack.clear();
        self.sculpt.voxel_redo_stack.clear();
        self.sculpt.voxel_color_undo_stack.clear();
        self.sculpt.voxel_color_redo_stack.clear();
        self.sculpt.voxel_redo_order.clear();
        self.sculpt.voxel_undo_order.retain(|&k| k == UndoKind::Grid);
        if let Some(g) = self.gpu.as_mut() {
            g.voxel_atlas_undo = Default::default();
            g.voxel_atlas_undo_building = None;
        }
        self.sculpt.pending_atlas_restore.clear();
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!(
            "Volume expanded (bounds {:.2}→{:.2}, res {}³) — Ctrl+Z restores",
            bounds, grown_bounds, grown_res
        ));
        eprintln!(
            "[MOVEBAKE] GROW bounds-only: {:.3}→{:.3} res={}³ (no res bump)",
            bounds, grown_bounds, grown_res
        );
        (true, pre)
    }

    /// Set up an OFF-SCREEN MESH rebuild of the CURRENT (already-final) voxel grid at its present resolution
    /// — the MESH-ONLY counterpart of `voxel_resample_to`. Used after the dynamic Move GROW, where the grid
    /// has ALREADY been regridded synchronously on the CPU; this only re-meshes it (the chunk grid is the
    /// wrong size for the new res) + recreates the GPU resources via the shared swap path. A WORKER THREAD
    /// scans the surface chunks + meshes them in parallel and streams the result via `VoxelRebuildMsg`;
    /// `voxel_flush_pending_rebuild` (poll) swaps the chunk set in atomically + drops the stale GPU buffers
    /// (`voxel_reset_gpu_after_grid_swap`), and the next poll mesh dispatch recreates density/atlas/pool for
    /// the new res — all under the Frame Ordering Contract. Clones the grid for the worker (the live one
    /// keeps rendering until the swap). No-op if there is no voxel object.
    pub(super) fn voxel_spawn_mesh_rebuild(&mut self) {
        let grid = match self.sculpt.voxel.as_ref() { Some(v) => v.grid.clone(), None => return };
        let res = grid.res;
        let chunk_dim = voxel_chunk_dim(res);
        let n = (chunk_dim * chunk_dim * chunk_dim) as usize;
        let mut chunks = Vec::with_capacity(n);
        for _ in 0..n { chunks.push(self.cache.mesh_cache.alloc_id()); }
        let (tx, rx) = std::sync::mpsc::channel::<crate::VoxelRebuildMsg>();
        std::thread::Builder::new()
            .name("voxel-mesh-rebuild".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                // MESH-ONLY (no resample): surface scan + parallel mesh — same streaming as resample worker.
                let _t = std::time::Instant::now();
                let mask = grid.surface_chunk_mask(VOXEL_CHUNK);
                let mut surface: Vec<u32> = mask.iter().enumerate().filter_map(|(ci, &h)| if h { Some(ci as u32) } else { None }).collect();
                surface.sort_unstable();
                eprintln!("[MESH-REBUILD→{}³] surface_chunks={} mask={:.0}ms",
                    res, surface.len(), _t.elapsed().as_secs_f32() * 1000.0);
                if tx.send(crate::VoxelRebuildMsg::Total(surface.len())).is_err() { return; }
                {
                    use rayon::prelude::*;
                    surface.par_iter().for_each_with(tx.clone(), |tx, &ci| {
                        let (a0, a1) = voxel_chunk_anchor(ci, chunk_dim, res);
                        let (verts, idx) = grid.mesh_region(a0, a1);
                        if verts.is_empty() || idx.is_empty() {
                            let _ = tx.send(crate::VoxelRebuildMsg::EmptyChunk(ci));
                        } else {
                            let _ = tx.send(crate::VoxelRebuildMsg::Chunk(ci, verts, idx));
                        }
                    });
                }
                eprintln!("[MESH-REBUILD→{}³] mesh+stream total={:.0}ms",
                    res, _t.elapsed().as_secs_f32() * 1000.0);
                let _ = tx.send(crate::VoxelRebuildMsg::Grid(grid));
            })
            .expect("spawn voxel-mesh-rebuild worker");
        if let Some(v) = self.sculpt.voxel.as_mut() {
            // Supersede any in-flight rebuild (free its orphaned chunk ids first).
            let prev = v.pending_rebuild.take();
            v.remesh_target_res = res;
            v.pending_rebuild = Some(crate::PendingVoxelRebuild {
                chunk_dim, chunks, chunk_tris: vec![0; n], rx, grid: None, total: None, done: 0,
            });
            if let Some(p) = prev { for id in p.chunks { self.cache.mesh_cache.remove(id); } }
        }
    }
}
