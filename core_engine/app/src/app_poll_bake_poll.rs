// core_engine/app/src/app_poll_bake_poll.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;
use glam::Vec3;
use eframe::wgpu;

impl AzraApp {
    /// Poll primary GPU bake staging buffers (non-blocking).
    /// Checks map_ready flag for each in-flight bake. When ready, reads the
    /// grid from the staging buffer and feeds it to `accept_gpu_result()`.
    pub(crate) fn poll_primary_bake(&mut self) {
        // COST HINT — learned, not guessed (owner perf report 2026-07-29).
        //
        // This was a hardcoded `1.0`, and `can_run` for a Medium ticket admits work whenever
        // `remaining_ms() > cost_hint_ms`. A 128³ readback actually costs ~668 ms, so the hint was
        // wrong by two orders of magnitude and the gate could NEVER fire: the scheduler waved
        // through a job that ate 40 frames' worth of budget, producing `[STALL] frame=686ms` after
        // every project load.
        //
        // The fix is not a bigger constant — cost scales with grid resolution, and a constant
        // sized for 128³ would needlessly starve cheap 32³ readbacks. Use the WORST cost actually
        // observed for a readback this session, so the scheduler learns the real number from the
        // machine it is running on instead of trusting an author's estimate.
        //
        // Seeded at 1.0 so the very first readback still runs (nothing is known yet, and blocking
        // it would deadlock the first bake).
        // STARVATION GUARD — this gate must be able to DEFER, never to BLOCK FOREVER.
        //
        // Owner 2026-07-29: "the SDF boolean works but sometimes the surface stops updating, and the
        // viewport is NOT frozen." That symptom is this bug, and I wrote it in `df44281`.
        //
        // The learned cost hint solved a real problem (a 668 ms readback blowing the frame), but a
        // Medium ticket is admitted only while `remaining_ms() > cost_hint_ms`, and the frame budget
        // is 12 ms. So as soon as ANY readback cost more than 12 ms, the learned hint permanently
        // exceeded the entire budget and `can_run` returned false on every subsequent frame — the
        // pending queue never drained, the surface stopped updating, and the UI stayed perfectly
        // responsive because nothing was actually blocking. A deadlock that looks like a render bug.
        //
        // Fix: after `MAX_DEFERRED_FRAMES` consecutive skips, run anyway. Expensive work then costs
        // one janky frame instead of never completing. Deferring is a scheduling choice; starving is
        // a correctness bug, and correctness wins.
        //
        // The counter resets on every actual run, so a steady stream of cheap readbacks never trips
        // it and the original frame-budget protection still applies in the normal case.
        const MAX_DEFERRED_FRAMES: u32 = 8;
        let bake_ticket = crate::frame_scheduler::WorkTicket {
            class: crate::frame_scheduler::WorkClass::Medium,
            cost_hint_ms: self.cache.bake_readback_worst_ms.max(1.0),
            name: "bake_readback",
        };
        if !self.scheduler.can_run(&bake_ticket)
            && self.cache.bake_readback_deferred_frames < MAX_DEFERRED_FRAMES
        {
            self.cache.bake_readback_deferred_frames += 1;
            self.scheduler.record_skipped(&bake_ticket);
            return;
        }
        self.cache.bake_readback_deferred_frames = 0;
        let _sched_t0 = std::time::Instant::now();
        let _spike_t0 = std::time::Instant::now();
        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => {
                self.scheduler.record(&bake_ticket, _sched_t0.elapsed().as_secs_f32() * 1000.0);
                return;
            }
        };
        if gpu.bake_pending.is_empty() {
            self.scheduler.record(&bake_ticket, _sched_t0.elapsed().as_secs_f32() * 1000.0);
            return;
        }

        // Drain settled entries (map_async delivered a result — OK or ERR). PENDING entries stay in
        // flight. An ERR (device loss / OOM / aborted map) is now observable here; without draining it
        // the entry would leak its staging buffer (~67 MB at 256³) and stall the body forever.
        let mut ready_indices = Vec::new();
        for (i, pending) in gpu.bake_pending.iter().enumerate() {
            if pending.map_ready.peek() != crate::map_state::PENDING {
                ready_indices.push(i);
            }
        }
        if ready_indices.is_empty() {
            // #14: still record the budget tick before returning, or the scheduler's frame-budget
            // telemetry desyncs (every other return path in this fn records).
            self.scheduler.record(&bake_ticket, _sched_t0.elapsed().as_secs_f32() * 1000.0);
            return;
        }
        let _readback_t0 = std::time::Instant::now();

        // Cap readbacks per frame to avoid multi-body stalls (remaining stay pending).
        const MAX_BAKE_READBACKS_PER_FRAME: usize = 1;
        ready_indices.truncate(MAX_BAKE_READBACKS_PER_FRAME);

        // Remove from back to front to keep indices valid
        let mut completed = Vec::new();
        for &i in ready_indices.iter().rev() {
            completed.push(gpu.bake_pending.remove(i));
        }
        completed.reverse(); // restore original order

        // [B] Accumulate per-pending [BakeReadback] time so the [SPIKE] line below can report the
        // UNACCOUNTED gap (poll total − processing). The live log shows poll_primary_bake ~5.5ms vs a
        // [BakeReadback] sub-total ~3.1ms — a ~2.4ms hole that no timer currently attributes. If the gap
        // is large + outside the per-pending block, it's frame-loop/scheduler/dealloc overhead, not readback.
        let mut _proc_sum_ms = 0.0f64;

        for pending in completed {
            let _total_t0 = std::time::Instant::now();
            let grid_voxels = pending.grid_size;

            // --- map_async FAILURE recovery (#5) ---
            // The map errored (device loss / OOM / aborted) — there is NO valid mapped range, so DON'T
            // call get_mapped_range()/unmap() (the buffer was never mapped). Free the staging buffer by
            // letting `pending` drop, and clear the in-flight latch so the body re-bakes instead of
            // leaking the buffer + stalling forever. Recovery, not a swallowed error.
            if pending.map_ready.peek() == crate::map_state::ERR {
                self.cache.body_vol_cache.clear_in_flight(pending.body_nid);
                eprintln!(
                    "[PrimaryGPU] map_async FAILED for {:?} (device loss/OOM/aborted) — staging buffer freed, body re-queued for bake",
                    pending.body_nid
                );
                continue; // `pending` (incl. staging_buffer) drops here → buffer freed
            }

            // --- S2.4/S2.5: SparseBricks (mode-2) completion ---
            // The bake wrote the SdfBrickAtlasGpu POOL in place (staging=None, fenced via
            // on_submitted_work_done). Route through record_sparse_bake (Full — records the new
            // frozen index next to the brick_resident marker) / record_sparse_partial_bake
            // (S2.5 Partial — hash refresh only; entry, bounds, marker and frozen index all stay)
            // — NEVER accept_gpu_result (its cbrt(len) inference is a carry-res violation on an
            // empty grid) and never the interaction-defer / lazy / field-direct logic below (all
            // dense-grid semantics). On acceptance, mirror the accept path's post-steps (this fn,
            // "--- 7. dispatch DC ---" dispatch_grid_dc_async — the sparse DC arm re-meshes from
            // the updated pool — + "--- 8. advance cascade ---" advance_cascade + the
            // mark_dirty(RENDER) at the top of the accepted branch). classify_bricks_for_accepted_
            // result (step 6b) is skipped: it is brick-POOL-only (PARKED) and early-returns on an
            // empty grid anyway; the rv sync (step 6a) has nothing to upload (no dense grid).
            if let Some(sp) = pending.sparse {
                let is_partial_sparse = matches!(sp, crate::gpu_ctx::SparsePending::Partial { .. });
                let (accepted, path, atlas_gen) = match sp {
                    crate::gpu_ctx::SparsePending::Full { atlas_gen, frozen } => (
                        self.cache.body_vol_cache.record_sparse_bake(
                            pending.body_nid,
                            pending.source_hash,
                            pending.resolution,
                            pending.generation,
                            atlas_gen,
                            (pending.bounds_min, pending.bounds_max), // dispatch-time provenance
                            frozen,
                        ),
                        "SPARSE_bricks",
                        atlas_gen,
                    ),
                    crate::gpu_ctx::SparsePending::Partial { atlas_gen, update } => (
                        self.cache.body_vol_cache.record_sparse_partial_bake(
                            pending.body_nid,
                            pending.source_hash,
                            pending.generation,
                            atlas_gen,
                            update,
                        ),
                        "SPARSE_bricks_partial",
                        atlas_gen,
                    ),
                };
                crate::vis_recorder::visrec(self.diag_frame, "bake.accept", || format!(
                    "body={:?} path={} res={:?} bounds={:.3?}..{:.3?} gen={} atlas_gen={} accepted={}",
                    pending.body_nid, path, pending.resolution, pending.bounds_min, pending.bounds_max,
                    pending.generation, atlas_gen, accepted));
                if accepted {
                    // GATE 2 — accept-time production of the single DirtyMeshRegion. A PARTIAL
                    // landing unions this bake's voxel window (union, not replace: a
                    // skipped/deferred consumer must not lose earlier deltas); a FULL landing
                    // clears it (occupancy may have changed anywhere — every consumer's
                    // conservative default, absent entry, is a full re-mesh).
                    if is_partial_sparse {
                        self.cache.body_vol_cache.note_bake_accepted_window(
                            pending.body_nid, pending.bake_window.0, pending.bake_window.1);
                    } else {
                        self.cache.body_vol_cache.note_full_bake_accepted(pending.body_nid);
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                    // Sparse land updates resident mesh; mesh-mod CachedCpu must not keep a
                    // frozen pre-edit fold. Invalidate so presentation falls back to Resident
                    // until re-fold lands (see sdf_mesh_presentation stamp gate).
                    let bn = pending.body_nid;
                    self.invalidate_mesh_mod_display(bn);
                    self.dispatch_grid_dc_async(bn);
                    if self.body_needs_display_fold(bn) {
                        let _ = self.sync_body_mesh_mod_display(bn);
                    }
                    self.cache.body_vol_cache.advance_cascade(bn, &self.scene.scene_graph);
                }
                continue;
            }

            // --- 0. Defer CPU readback during interaction ---
            // GPU-to-GPU path already updated the viewport's flat storage buffer
            // at dispatch time. CPU readback (to_vec + nan_scan) is unnecessary
            // during drag — DC extraction is skipped for the active body (line 386),
            // and the 3D texture upload can wait until drag ends.
            // On drag end, rebake_no_undo() triggers a fresh bake read back normally.
            // GPU SDF meshing (Phase G): the readback feeds gpu_mesh_to_cache, so deferring
            // it = the mesh only updates on release (the "still just appears on release" report).
            // GPU meshing is fast enough to run the whole bake→readback→mesh chain every drag frame.
            // GATE 3: defer only on the CPU chunked-DC oracle route (GPU routes re-mesh per frame).
            if self.tool.current.is_busy()
                && self.cache.dc_mesh_route(false) == crate::cache_state::DcMeshRoute::CpuChunkedOracle
            {
                if let Some(sb) = pending.staging_buffer.as_ref() {
                    drop(sb.slice(..).get_mapped_range());
                    sb.unmap();
                }
                self.cache.body_vol_cache.clear_in_flight(pending.body_nid);
                if crate::perf_spam_on() {
                    eprintln!("[BakeReadback] DEFERRED during drag — body={:?}", pending.body_nid);
                }
                continue;
            }

            // --- STEP 2: lazy readback (resident field-direct) ---
            // If the resident field-direct path will mesh this body — surface comes from rv.buffer, which
            // this bake's GPU2GPU copy already updated — AND body_grids already holds a non-empty grid at
            // THIS resolution, SKIP the CPU to_vec (64MB at 256³, the per-drag-frame cost). body_grids keeps
            // its last full grid (valid for save/duplicate; one full-bake stale in data only, refreshed on
            // the next FULL bake — settle re-tighten / res change fail the same-res check and fall through
            // to the full readback below). Full quality preserved (the mesh is from the live rv.buffer).
            let res_cubic = pending.resolution[0];
            // GATE 3: `gpu_sdf_mesh_lazy_readback` is a READBACK policy, not a route (documented
            // in cache_state.rs) — it applies only when the DenseResident route + G3 field-direct
            // sub-policy will mesh from rv.buffer.
            let lazy_bounds: Option<(glam::Vec3, glam::Vec3)> = if self.cache.gpu_sdf_mesh_lazy_readback
                && pending.is_partial
                && self.cache.dc_mesh_route(false) == crate::cache_state::DcMeshRoute::DenseResident
                && self.cache.gpu_sdf_mesh_resident_field
                && !self.cache.body_vol_cache.resident_volume_reallocated
                && pending.resolution[0] == pending.resolution[1]
                && pending.resolution[1] == pending.resolution[2]
            {
                // Capture the last full grid's bounds (Copy) only if it's non-empty at THIS res, then drop
                // that borrow before querying resident_volume() (disjoint immutable borrows otherwise).
                let meta = self.cache.body_vol_cache.body_grid(pending.body_nid)
                    .filter(|(grid, bres, _, _, _)| !grid.is_empty() && *bres == res_cubic)
                    .map(|(_, _, gmin, gmax, _)| (gmin, gmax));
                meta.filter(|(gmin, gmax)| {
                    self.cache.body_vol_cache.resident_volume().map_or(false, |rv| {
                        rv.body_nid == pending.body_nid
                            && rv.resolution == [res_cubic, res_cubic, res_cubic]
                            && (rv.bake_bounds_min - *gmin).length_squared() < 1e-6
                            && (rv.bake_bounds_max - *gmax).length_squared() < 1e-6
                    })
                })
            } else {
                None
            };
            // PATH-A field-direct full bake: body_grids is intentionally stale here (no readback). Source the
            // mesh bounds from body_bounds — the live padded grid this bake was DISPATCHED over, set
            // SYNCHRONOUSLY at trigger time — NOT from rv.bake_bounds, which is refreshed only in the DEFERRED
            // direct-write completion (app_poll.rs:~1680) that runs AFTER this read. For a bounds-CHANGING
            // full bake (bevel / escape) rv.bake_bounds would still hold the OLD bounds → meshing rv.buffer at
            // the wrong offset = the smear/ghost cache bug. Require the rv to be this body at this resolution.
            let field_direct_bounds: Option<(glam::Vec3, glam::Vec3)> = if pending.field_direct
                && pending.resolution[0] == pending.resolution[1]
                && pending.resolution[1] == pending.resolution[2]
                && self.cache.body_vol_cache.resident_volume().map_or(false, |rv| {
                    rv.body_nid == pending.body_nid && rv.resolution == [res_cubic, res_cubic, res_cubic]
                })
            {
                // Dispatch-time provenance, not the live body_bounds — a concurrent aabb_escape can't
                // re-scale THIS bake's surface (same white-box-ghost race as the full-readback path).
                Some((pending.bounds_min, pending.bounds_max))
            } else {
                None
            };
            if let Some((gmin, gmax)) = field_direct_bounds.or(lazy_bounds) {
                // [visrec] bake completion accepted via the no-copy path (Part B #2).
                crate::vis_recorder::visrec(self.diag_frame, "bake.accept", || format!(
                    "body={:?} path={} res={}³ bounds={:.3?}..{:.3?} gen={}",
                    pending.body_nid,
                    if pending.field_direct { "field_direct" } else { "lazy" },
                    res_cubic, gmin, gmax, pending.generation));
                crate::bakeprof_record(true, 0.0); // FAST path: no CPU copy
                // [HealTrace] settle-scoped: a no-copy completion inside the heal window is the
                // re-arm feeder — print its full identity so its dispatcher is unambiguous.
                // Parked behind crate::perf_spam_on() after the phantom-edit loop was resolved (2026-07-09).
                if crate::perf_spam_on() && self.cache.body_vol_cache.heal_tracing() {
                    eprintln!(
                        "[HealTrace] no-copy completion body={:?} res={}³ partial={} field_direct={} gen={} — re-arms settle",
                        pending.body_nid, res_cubic, pending.is_partial, pending.field_direct, pending.generation);
                }
                // The readback was skipped → body_grids is now stale relative to rv.buffer. Record it
                // AND re-arm pending_retighten (a full bake removed the body from that set at dispatch),
                // so the settle re-tighten fires the ONE heal readback that refreshes the CPU cache.
                self.cache.body_vol_cache.note_cpu_grid_stale(pending.body_nid);
                // Release the staging map WITHOUT the 64MB copy (mirror the defer path's unmap), do the
                // non-grid bookkeeping, and mesh straight from rv.buffer.
                if let Some(sb) = pending.staging_buffer.as_ref() {
                    drop(sb.slice(..).get_mapped_range());
                    sb.unmap();
                }
                // PATH-A: a field-direct FULL bake changed the bounds — sync rv.bake_bounds to them NOW so
                // sdf_populate_resident's own bounds_ok gate (rv.bake_bounds vs these) passes and the
                // field-direct DC fires at the correct offset (the deferred direct-write update lands too
                // late). No-op for the lazy partial path (bounds unchanged → already matches).
                if pending.field_direct {
                    if let Some(slot_idx) = self.cache.body_vol_cache.resident_slot {
                        if let Some(slot) = self.cache.body_vol_cache.volume_pool.get_mut(slot_idx) {
                            if let Some(ref mut rv) = slot.volume {
                                if rv.body_nid == pending.body_nid {
                                    rv.bake_bounds_min = gmin;
                                    rv.bake_bounds_max = gmax;
                                }
                            }
                        }
                    }
                }
                if self.cache.body_vol_cache.note_resident_bake_complete(pending.body_nid, pending.generation) {
                    // GATE 2 — accept-time production (no-copy path: path-A field-direct full /
                    // lazy-readback partial). Full-covering window clears; partial unions.
                    self.cache.body_vol_cache.note_bake_accepted(
                        pending.body_nid, pending.bake_window, pending.resolution);
                    self.sdf_populate_resident(pending.body_nid, res_cubic, gmin, gmax);
                    // New resident field/res → mesh-mod fold (Decimate etc.) must re-read base.
                    // Without this, CachedCpu keeps the previous-res folded mesh forever.
                    let bn = pending.body_nid;
                    self.invalidate_mesh_mod_display(bn);
                    if self.body_needs_display_fold(bn) {
                        let _ = self.sync_body_mesh_mod_display(bn);
                    }
                    // Parity with the full-readback path: advance the progressive-resolution cascade
                    // (no-op when none is in flight, i.e. steady-state drag) so cascade behavior is unchanged.
                    self.cache.body_vol_cache.advance_cascade(pending.body_nid, &self.scene.scene_graph);
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if crate::perf_spam_on() {
                    eprintln!("[LazyReadback] body={:?} res={}³ — skipped CPU to_vec (field-direct from rv.buffer)",
                        pending.body_nid, res_cubic);
                }
                continue;
            }

            // staging_buffer=None means a path-A field-direct full whose completion should have been
            // taken by the field_direct_bounds branch above. Reaching HERE with None = the rv match
            // failed between dispatch and poll (body switch / realloc in the 1-frame window): the bake
            // result lives only in a buffer we can no longer trust as this body's resident. There is
            // no staging to fall back on — RE-BAKE instead of silently dropping the result (a silent
            // drop would leave a stale mesh until the next unrelated edit).
            let staging_buf = match pending.staging_buffer.as_ref() {
                Some(sb) => sb,
                None => {
                    // [visrec] SUSPECTED VANISH SOURCE (Part B #10): a path-A field-direct full
                    // completed but the rv match failed between dispatch and poll (body switch /
                    // realloc window) — result discarded, body re-baked.
                    crate::vis_recorder::visrec(self.diag_frame, "recover", || format!(
                        "body={:?} staging=None + rv match FAILED at poll — result dropped, re-bake queued \
                         (gen={} res={:?} bounds={:.3?}..{:.3?})",
                        pending.body_nid, pending.generation, pending.resolution,
                        pending.bounds_min, pending.bounds_max));
                    self.cache.body_vol_cache.clear_in_flight(pending.body_nid);
                    self.cache.body_vol_cache.mark_body_dirty(pending.body_nid);
                    continue;
                }
            };

            // F4: reject a stale result BEFORE the expensive mapped copy. A re-dirty during the bake
            // advances the body's generation; this result gets discarded at accept anyway — but only
            // after get_mapped_range + a ~512MB to_vec at 512³. Check up-front instead. The buffer's
            // map_async has already completed (that's why we're here), so unmap it to release cleanly.
            // Do NOT clear_in_flight — a newer-generation bake already owns the in_flight slot (this
            // mirrors the original post-copy check, which also skipped clear_in_flight).
            let current_gen = self.cache.body_vol_cache.current_generation(pending.body_nid);
            if pending.generation < current_gen {
                staging_buf.unmap();
                continue;
            }

            // --- 1. get_mapped_range ---
            let _t1 = std::time::Instant::now();
            let slice = staging_buf.slice(..);
            let mapped = slice.get_mapped_range();
            let _map_range_ms = _t1.elapsed().as_secs_f64() * 1000.0;

            // --- 2. to_vec (memcpy from mapped GPU → CPU) ---
            let _t2 = std::time::Instant::now();
            let grid: Vec<f32> = bytemuck::cast_slice(&mapped).to_vec();
            let _to_vec_ms = _t2.elapsed().as_secs_f64() * 1000.0;
            crate::bakeprof_record(false, _to_vec_ms); // SLOW path: 64MB GPU→CPU copy

            // --- 3. drop mapped + unmap ---
            let _t3 = std::time::Instant::now();
            drop(mapped);
            staging_buf.unmap();
            let _unmap_ms = _t3.elapsed().as_secs_f64() * 1000.0;

            // --- 4. Validate size ---
            let expected = pending.grid_size;
            if grid.len() != expected {
                eprintln!(
                    "[PrimaryGPU] Grid size mismatch for {:?}: got {} expected {}",
                    pending.body_nid, grid.len(), expected
                );
                self.cache.body_vol_cache.clear_in_flight(pending.body_nid);
                continue;
            }

            // --- 5. NaN/inf spot-check (debug only, sampled not exhaustive) ---
            // [A1] Also skip during interaction: the scan is a dev-build defensive check (already
            // cfg!-gated → free in release); skipping it mid-drag removes ~0.47ms/bake from the dev
            // profiling path. A correct GPU bake doesn't emit NaN; the next idle/release bake re-scans.
            let _t5 = std::time::Instant::now();
            if cfg!(debug_assertions) && !self.tool.current.is_busy() {
                // Sample every 64th element instead of scanning all — saves ~95% of scan time
                let has_bad = grid.iter().step_by(64).any(|v| v.is_nan() || v.is_infinite());
                if has_bad {
                    eprintln!("[PrimaryGPU] NaN/inf in baked grid for {:?}", pending.body_nid);
                    self.cache.body_vol_cache.clear_in_flight(pending.body_nid);
                    continue;
                }
            }
            let _nan_scan_ms = _t5.elapsed().as_secs_f64() * 1000.0;

            // (Generation staleness was checked up-front, before the mapped copy — see F4 above. Within
            // this synchronous loop iteration no re-dirty can occur, and accept_gpu_result re-validates.)

            // --- 6. accept_gpu_result ---
            let _t6 = std::time::Instant::now();
            let body_nid = pending.body_nid;
            let accepted = self.cache.body_vol_cache.accept_gpu_result(body_nid, grid, pending.generation, pending.source_hash, Some((pending.bounds_min, pending.bounds_max)));
            let _accept_ms = _t6.elapsed().as_secs_f64() * 1000.0;
            // [visrec] bake completion via the FULL readback path (Part B #2).
            crate::vis_recorder::visrec(self.diag_frame, "bake.accept", || format!(
                "body={:?} path=FULL_readback res={:?} bounds={:.3?}..{:.3?} gen={} accepted={}",
                body_nid, pending.resolution, pending.bounds_min, pending.bounds_max,
                pending.generation, accepted));

            if accepted {
                self.mark_dirty(DirtyFlags::RENDER);
                // New volume (possibly new res) → drop Decimate/mesh-mod base so fold re-reads DC.
                self.invalidate_mesh_mod_display(body_nid);
                // Re-fold if possible so CachedCpu is not stuck on pre-invalidate content
                // until drag ends (ensure_body_mesh_mod_displays is skipped while gizmo busy).
                if self.body_needs_display_fold(body_nid) {
                    let _ = self.sync_body_mesh_mod_display(body_nid);
                }

                // GATE 2 — accept-time production of the single DirtyMeshRegion, BEFORE any
                // consumer in this accept block runs (6a's update_resident_volume consumes it
                // for the partial-vs-full texture upload; the dc dispatch peeks it).
                self.cache.body_vol_cache.note_bake_accepted(
                    body_nid, pending.bake_window, pending.resolution);

                // --- 6a. sync the resident volume to the just-accepted grid BEFORE the DC dispatch ---
                // accept_gpu_result updated this body's body_grids entry (new resolution/bounds) and set
                // resident_upload_needed; but the frame-start update_resident_volume() (Low priority, often
                // skipped at 512³, and run BEFORE accept) left rv at the prior draft resolution. Without
                // this, sdf_populate_resident below sees a stale rv (res_ok=false) and falls back to the
                // 512MB slot.field upload instead of the field-direct read from rv.buffer. Sync directly
                // (bypassing the scheduler gate — a landed bake is not deferrable work) so rv is current the
                // moment the DC reads it. Active body only (rv is the active body's).
                // NESTED SCOPE: bakes are top-level; resolve the active nid to its ancestor
                // or this sync silently skips inside a sub-body scope (frozen stale volume).
                if body_nid == self.active_top_body() {
                    if let Some(gpu) = &self.gpu {
                        self.cache.body_vol_cache.update_resident_volume(
                            body_nid, &gpu.device, &gpu.queue);
                    }
                }

                // --- 6b. classify bricks from accepted grid (now gated on use_brick_pool — ~0 when off) ---
                let _t6b = std::time::Instant::now();
                self.cache.body_vol_cache.classify_bricks_for_accepted_result(body_nid);
                let _classify_ms = _t6b.elapsed().as_secs_f64() * 1000.0;

                // --- 6c. brick_fill dispatch (DISABLED) ---
                // Brick fill currently overwrites the dense bake output in the atlas,
                // causing voxelization artifacts. The brick_fill should only run when
                // it REPLACES the dense bake (sparse-only mode), not as a post-process
                // on top of it. Re-enable when cascade integration routes through
                // coarse→classify→brick_fill instead of dense→classify→brick_fill.
                // self.dispatch_brick_fill(body_nid);

                // --- 7. dispatch DC ---
                let _t7 = std::time::Instant::now();
                self.dispatch_grid_dc_async(body_nid);
                let _dc_dispatch_ms = _t7.elapsed().as_secs_f64() * 1000.0;

                // --- 8. advance cascade ---
                let _t8 = std::time::Instant::now();
                self.cache.body_vol_cache.advance_cascade(body_nid, &self.scene.scene_graph);
                let _cascade_ms = _t8.elapsed().as_secs_f64() * 1000.0;

                // --- DIAGNOSTIC: per-operation breakdown ---
                let _total_ms = _total_t0.elapsed().as_secs_f64() * 1000.0;
                _proc_sum_ms += _total_ms;
                // Feed the scheduler's cost hint (see the ticket above). WORST, not average: the
                // gate exists to prevent a frame blowout, and an average hides exactly the
                // outlier that causes one.
                if (_total_ms as f32) > self.cache.bake_readback_worst_ms {
                    self.cache.bake_readback_worst_ms = _total_ms as f32;
                }
                // [B] Gated behind PERF_SPAM: this 9-line eprintln fires every bake AFTER _total_ms is
                // measured, so its stderr-pipe write cost lands in the [SPIKE] `unaccounted` gap (the
                // observer effect). Default-off keeps the hot path quiet; flip crate::perf_spam_on() to re-profile.
                // Gate: PERF_SPAM (all bakes) OR any bake over 100ms (the multi-hundred-ms stalls
                // the owner sees after a load). The second arm exists because PERF_SPAM floods the
                // frame thread badly enough to distort the very numbers it prints — the 668ms
                // post-load stall was reported with only its TOTAL captured, so the 9-line
                // breakdown that would name the hot step was never seen. A >100ms bake is rare and
                // pathological by definition, so logging it costs nothing in the normal case.
                if (crate::perf_spam_on() && _total_ms > 0.5) || _total_ms > 100.0 {
                    eprintln!(
                        "[BakeReadback] body={:?} grid={}  total={:.1}ms\n\
                         [BakeReadback]   get_mapped_range:  {:.2}ms\n\
                         [BakeReadback]   to_vec ({}KB):     {:.2}ms\n\
                         [BakeReadback]   unmap:             {:.2}ms\n\
                         [BakeReadback]   nan_scan ({}):     {:.2}ms\n\
                         [BakeReadback]   accept_result:     {:.2}ms\n\
                         [BakeReadback]   classify_bricks:   {:.2}ms\n\
                         [BakeReadback]   dc_dispatch:       {:.2}ms\n\
                         [BakeReadback]   cascade:           {:.2}ms",
                        body_nid, grid_voxels, _total_ms,
                        _map_range_ms,
                        grid_voxels * 4 / 1024, _to_vec_ms,
                        _unmap_ms,
                        grid_voxels, _nan_scan_ms,
                        _accept_ms,
                        _classify_ms,
                        _dc_dispatch_ms,
                        _cascade_ms,
                    );
                }
            } else {
                // Rejected (stale generation etc.) — still log if slow
                let _total_ms = _total_t0.elapsed().as_secs_f64() * 1000.0;
                _proc_sum_ms += _total_ms;
                if crate::perf_spam_on() && _total_ms > 0.5 {
                    eprintln!(
                        "[BakeReadback] REJECTED body={:?} grid={}  total={:.1}ms  \
                         map_range={:.2}ms  to_vec={:.2}ms  unmap={:.2}ms  nan_scan={:.2}ms  accept={:.2}ms",
                        body_nid, grid_voxels, _total_ms,
                        _map_range_ms, _to_vec_ms, _unmap_ms, _nan_scan_ms, _accept_ms,
                    );
                }
            }
        }
        self.perf.bake_readback_ms = _readback_t0.elapsed().as_secs_f64() * 1000.0;
        self.perf.bake_readback_count = ready_indices.len() as u32;
        let _ms = _spike_t0.elapsed().as_secs_f64() * 1000.0;
        if crate::perf_spam_on() && _ms > 0.5 {
            // [B] `unaccounted` = poll total − summed per-pending [BakeReadback] time. If it's the bulk of
            // the cost, the bottleneck is OUTSIDE the readback (frame-loop / staging-buffer drop / scheduler),
            // not the to_vec — which decides whether step 2 (lazy readback) is the right next lever.
            eprintln!("[SPIKE] poll_primary_bake total: {:.1}ms (processed {} bakes; processing={:.1}ms unaccounted={:.1}ms)",
                _ms, ready_indices.len(), _proc_sum_ms, _ms - _proc_sum_ms);
        }
        self.scheduler.record(&bake_ticket, _sched_t0.elapsed().as_secs_f32() * 1000.0);
    }

    /// Dispatch brick_fill compute shader for classified bricks of a body.
    /// Writes SDF values directly into the atlas 3D texture for each active 8³ brick.
    /// Called after brick classification completes (Phase 4c, PLAN_BRICK_ATLAS.md).
    #[allow(dead_code)] // Will be called from cascade integration (Phase 4d)
    pub(crate) fn dispatch_brick_fill(&mut self, body_nid: core_math::scene_graph::NodeId) {
        use crate::body_volume_cache::brick_allocator;
        // Check prerequisites
        let gpu = match &self.gpu { Some(g) => g, None => return };
        let pipeline = match &gpu.pipelines.brick_fill_pipeline { Some(p) => p, None => return };
        let bgl = match &gpu.bind_groups.brick_fill_bgl { Some(b) => b, None => return };
        let atlas = match &gpu.volume_atlas { Some(a) => a, None => return };

        // Body must have classified bricks and an atlas slot
        let brick_coords = match self.cache.body_vol_cache.brick_allocator.body_brick_coords(body_nid) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return,
        };
        let slot = match atlas.slot(body_nid) {
            Some(s) if s.valid => s,
            _ => return,
        };

        let atlas_pad = graphics::volume_atlas::ATLAS_PAD;

        // Build sub-brick list (padded-grid-relative coordinates)
        let sub_brick_list = brick_allocator::build_sub_brick_list(
            &brick_coords, atlas_pad,
        );
        if sub_brick_list.is_empty() { return; }
        let sub_brick_count = sub_brick_list.len() as u32 / 4;

        // Serialize prim data (same as bake dispatch)
        let sg = &self.scene.scene_graph;
        let mut prim_floats = graphics::serialize_body_for_bake_hot(
            sg, body_nid, glam::Vec3::ZERO,
        );
        let clip_curve_floats = graphics::serialize_clip_curves_for_bake(sg, body_nid, &mut prim_floats);

        // Get body properties for BakeParams
        let (mirror_flags, mirror_sign, mirror_center, bevel_radius) = sg.get(body_nid)
            .and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::SdfBody { mirror_flags, mirror_sign, mirror_center, bevel_radius, .. } =>
                    Some((*mirror_flags as u32, *mirror_sign as u32, *mirror_center, *bevel_radius)),
                _ => None,
            })
            .unwrap_or((0, 0, glam::Vec3::ZERO, 0.0));

        let (bounds_min, bounds_max) = self.cache.body_vol_cache.body_bounds.get(&body_nid)
            .copied()
            .unwrap_or((glam::Vec3::splat(-2.0), glam::Vec3::splat(2.0)));

        // Effective resolution from brick grid
        let entry = match self.cache.body_vol_cache.body_grids.get(&body_nid) {
            Some(e) => e,
            None => return,
        };
        let res = entry.bake_resolution;
        let res3 = [res, res, res];

        let gpu = self.gpu.as_ref().unwrap();
        let device = &gpu.device;
        let queue = &gpu.queue;

        // Create GPU buffers
        let prim_bytes: &[u8] = bytemuck::cast_slice(&prim_floats);
        let prim_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
            label: Some("brick_fill_prims"),
            size: (prim_bytes.len() as u64).max(4),
            usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&prim_buf, 0, prim_bytes);

        let list_bytes: &[u8] = bytemuck::cast_slice(&sub_brick_list);
        let list_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
            label: Some("brick_fill_list"),
            size: (list_bytes.len() as u64).max(4),
            usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&list_buf, 0, list_bytes);

        let clip_bytes: &[u8] = bytemuck::cast_slice(&clip_curve_floats);
        let clip_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
            label: Some("brick_fill_clips"),
            size: (clip_bytes.len() as u64).max(4),
            usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&clip_buf, 0, clip_bytes);

        // BakeParams uniform — use padded resolution so brick_fill uses same coord space as volume_bake
        let padded_res = [
            res3[0] + 2 * atlas_pad,
            res3[1] + 2 * atlas_pad,
            res3[2] + 2 * atlas_pad,
        ];
        let prim_count = prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;
        let params = crate::gpu_jobs::BakeParams {
            grid_res: padded_res,
            _pad0: 0,
            bounds_min: bounds_min.to_array(),
            _pad1: 0.0,
            bounds_max: bounds_max.to_array(),
            prim_count,
            floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
            dims_bricks: [0; 3], // SparseBricks-only; brick_fill ignores it
            bake_min: [0, 0, 0],
            _pad3: 0,
            bake_max: padded_res,
            _pad4: 0,
            mirror_flags,
            mirror_center: mirror_center.to_array(),
            bevel_radius,
            mirror_sign,
            atlas_offset: [slot.x_start, slot.y_start, slot.z_start],
            atlas_pad,
            output_mode: 0, // GridBacked (brick_fill always writes to texture, but params needs valid mode)
            _pad5: 0,
            body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4, // brick fill: operand not consumed
        };
        let params_buf = gpu.buffers.primary_bake_params_buffer.as_ref().unwrap();
        queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&params));

        // Atlas texture view for writing
        let atlas_tex_view = atlas.texture.create_view(&eframe::wgpu::TextureViewDescriptor::default());

        // Create bind group
        // T1 height atlas — brick_fill currently reuses clip dummy (hot[92]=NONE until fill packs maps).
        let height_dummy = device.create_buffer(&eframe::wgpu::BufferDescriptor {
            label: Some("brick_fill_height_dummy"),
            size: 4,
            usage: eframe::wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let bg = device.create_bind_group(&eframe::wgpu::BindGroupDescriptor {
            label: Some("brick_fill_bg"),
            layout: bgl,
            entries: &[
                eframe::wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
                eframe::wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                eframe::wgpu::BindGroupEntry { binding: 2, resource: eframe::wgpu::BindingResource::TextureView(&atlas_tex_view) },
                eframe::wgpu::BindGroupEntry { binding: 3, resource: list_buf.as_entire_binding() },
                eframe::wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
                eframe::wgpu::BindGroupEntry { binding: 5, resource: height_dummy.as_entire_binding() },
            ],
        });

        // Dispatch: one workgroup per sub-brick (4×4×4 threads each).
        // GPU limit: max 65535 workgroups per axis. Split across X and Y if needed.
        let max_wg = 65535u32;
        let wg_x = sub_brick_count.min(max_wg);
        let wg_y = (sub_brick_count + max_wg - 1) / max_wg;
        // The shader indexes by global_invocation_id.x = workgroup_id.x + workgroup_id.y * wg_x_count
        // But brick_fill.wgsl uses wg_id.x as sub_idx directly. We need single-axis dispatch
        // or modify the shader. For now, cap at 65535 sub-bricks per dispatch.
        let capped_count = sub_brick_count.min(max_wg);
        let mut encoder = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
            label: Some("brick_fill_encoder"),
        });
        {
            let mut cpass = encoder.begin_compute_pass(&eframe::wgpu::ComputePassDescriptor {
                label: Some("brick_fill_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(pipeline);
            cpass.set_bind_group(0, &bg, &[]);
            cpass.dispatch_workgroups(capped_count, 1, 1);
        }
        queue.submit(std::iter::once(encoder.finish()));
        if sub_brick_count > max_wg {
            eprintln!("[BrickFill] WARNING: {} sub-bricks exceeds GPU limit {}, capped to {}",
                sub_brick_count, max_wg, capped_count);
        }

        eprintln!("[BrickFill] body={:?} dispatched {} sub-bricks ({} bricks) to atlas",
            body_nid, sub_brick_count, brick_coords.len());
    }
}
