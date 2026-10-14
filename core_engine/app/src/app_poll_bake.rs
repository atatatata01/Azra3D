// core_engine/app/src/app_poll_bake.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;
use eframe::wgpu;

impl AzraApp {
    /// Dispatch pending primary GPU bake requests (Level 2).
    ///
    /// Takes bake requests from body_vol_cache.primary_bake_requests (populated by
    /// trigger_bake when no worker GPUs are available), encodes them as compute
    /// dispatches on the primary device, and sets up staging buffer readback.
    /// Results arrive via poll_primary_bake() on subsequent frames.
    pub(crate) fn dispatch_pending_primary_bakes(&mut self) {
        let _spike_t0 = std::time::Instant::now();
        let mut requests = std::mem::take(&mut self.cache.body_vol_cache.primary_bake_requests);
        if requests.is_empty() { return; }

        // ---- Bake coalescing + priority + frame budget ----
        // 1. Coalesce: keep only the NEWEST request per body (last wins).
        //    Stale requests are wasted GPU work.
        {
            let mut seen = std::collections::HashSet::new();
            // Iterate in reverse so the LAST request per body survives
            requests = requests.into_iter().rev()
                .filter(|r| seen.insert(r.body_nid))
                .collect::<Vec<_>>();
            requests.reverse(); // restore original order for scene-graph ordering
        }

        // 2. Prioritize: active body first, then rest.
        let active_nid = self.scene.active_body_nid;
        requests.sort_by_key(|r| if r.body_nid == active_nid { 0u32 } else { 1 });

        // 3. Frame budget: dispatch at most N bakes. Return the rest to the queue.
        const MAX_BAKES_PER_FRAME: usize = 4; // 1 urgent + 3 normal
        if requests.len() > MAX_BAKES_PER_FRAME {
            let deferred = requests.split_off(MAX_BAKES_PER_FRAME);
            self.cache.body_vol_cache.primary_bake_requests = deferred;
            eprintln!("[BakeBudget] dispatching {}, deferred {} to next frame",
                requests.len(), self.cache.body_vol_cache.primary_bake_requests.len());
        }
        // Collect direct-to-texture write info for deferred processing
        // (can't set flags inside the gpu borrow scope)
        let mut deferred_direct_writes: Vec<(core_math::scene_graph::NodeId, [u32; 3], glam::Vec3, glam::Vec3, core_math::scene_graph::BodyTransform, u64, bool)> = Vec::new();
        // Toasts queued inside the gpu borrow (self.toast needs &mut self) — shown after the loop.
        let mut deferred_toasts: Vec<String> = Vec::new();
        // Gate 0 — full/partial bake tally (locals; the &mut self.gpu borrow below forbids writing
        // self.perf inside the loop, so accumulate here and flush at the function tail).
        let mut full_bakes = 0u32;
        let mut partial_bakes = 0u32;

        // P3 — multi-mod prim stacks need CPU BodyField (full world_distance stack).
        // GPU volume_bake only has one non-mirror modifier slot.
        let (cpu_mod_reqs, mut requests): (Vec<_>, Vec<_>) = requests
            .into_iter()
            .partition(|r| r.force_cpu_mod_stack);
        if !cpu_mod_reqs.is_empty() {
            eprintln!(
                "[P3] CPU full-stack bake for {} body(ies) (multi-mod honesty)",
                cpu_mod_reqs.len()
            );
            self.cpu_fallback_bakes(cpu_mod_reqs);
        }
        if requests.is_empty() {
            return;
        }

        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => {
                // No GPU — fall back to CPU for all requests
                self.cpu_fallback_bakes(requests);
                return;
            }
        };

        // Ensure pipeline is compiled (lazy init on first use)
        crate::gpu_init::ensure_primary_bake_pipeline(gpu);
        let pipeline = match &gpu.pipelines.primary_bake_pipeline {
            Some(p) => p,
            None => {
                // Pipeline compilation failed — CPU fallback
                let _ = gpu;
                self.cpu_fallback_bakes(requests);
                return;
            }
        };
        let bgl = gpu.bind_groups.primary_bake_bgl.as_ref().unwrap();

        for mut req in requests {
            let device = &gpu.device;
            let queue = &gpu.queue;

            // Store template hash for instancing (rebuild_params uses this to find matching slots)
            if req.template_hash != 0 {
                if let Some(ref mut atlas) = gpu.volume_atlas {
                    atlas.body_hashes.insert(req.body_nid, req.template_hash);
                    // (Instancing skip removed 2026-07-13: it was ONLY valid when the VP shader
                    // was the renderer — in Object Mode every duplicate needs its own DC raster
                    // mesh, so the bake must always run; the gate was constant false.)
                }
            }

            let prim_byte_len = (req.prim_floats.len() * 4) as u64;
            let grid_voxels = req.resolution[0] as u64 * req.resolution[1] as u64 * req.resolution[2] as u64;
            let grid_byte_len = grid_voxels * 4; // f32 per voxel
            let max_res = req.resolution[0].max(req.resolution[1]).max(req.resolution[2]);

            // Select bake output mode: GridBacked (≤512) or TextureOnly (>512 on primary GPU).
            // S2.4/S2.5: a request carrying a SparseBricks routing (Object-Mode bake at ≥512,
            // Object Mode + res >= 512 — decided at REQUEST time in trigger_bake) dispatches as
            // mode 2 instead: binding 2 is rebound to the SdfBrickAtlasGpu POOL, binding 5 to
            // its INDEX. `Full` uploads a fresh index; `Partial` (S2.5) reuses the FROZEN
            // resident index over a dilated dirty window. TextureOnly is excluded at request time.
            let sparse_kind = req.sparse.take();
            let sparse = sparse_kind.is_some();
            let bake_mode = if sparse {
                crate::gpu_jobs::BakeOutputMode::SparseBricks
            } else {
                crate::gpu_jobs::select_bake_output_mode(max_res, true)
            };
            let texture_only = bake_mode == crate::gpu_jobs::BakeOutputMode::TextureOnly;

            // S2.5: validate a frozen-index PARTIAL against the LIVE atlas BEFORE creating any
            // GPU resource for this request. The atlas holds ONE body's content — another body's
            // sparse FULL bake earlier in this same drain (or a grow) may have replaced it since
            // the request was built. Identity + generation counters, never timing assumptions:
            // body, resolution, brick dims and generation must ALL still match the entry's
            // brick_resident marker, or the windowed write would land in someone else's pool.
            // On mismatch → ESCALATE: unlatch in-flight, mark the body fully dirty (clears the
            // regions → the next trigger_bake takes the FULL classify path) and clear the
            // debounce (escalation is a one-shot event, not a rapid-edit stream).
            if let Some(crate::body_volume_cache::SparseBakeRequest::Partial {
                dims_bricks, atlas_generation, ..
            }) = &sparse_kind
            {
                let atlas_ok = gpu.sdf_brick_atlases.get(&req.body_nid).map_or(false, |a| {
                    a.body_nid == Some(req.body_nid)
                        && a.generation == *atlas_generation
                        && a.res == req.resolution[0]
                        && a.dims_bricks == *dims_bricks
                });
                if !atlas_ok {
                    self.cache.body_vol_cache.note_sparse_escalation(
                        req.body_nid, "dispatch: atlas identity/generation mismatch");
                    self.cache.body_vol_cache.clear_in_flight(req.body_nid);
                    self.cache.body_vol_cache.mark_body_dirty(req.body_nid);
                    self.cache.body_vol_cache.clear_bake_debounce();
                    continue;
                }
            }

            // Create/grow prim buffer
            let prim_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("primary_bake_prims"),
                size: prim_byte_len.max(4),
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&prim_buf, 0, bytemuck::cast_slice(&req.prim_floats));

            // Ensure this body has an atlas slot at the correct resolution.
            // allocate() handles: first allocation, resolution changes (free+realloc),
            // and Z-slab clear to +999.0 to prevent ghost geometry.
            // Must be called unconditionally — not just when slot_index is None —
            // because resolution can change (e.g., cascade 64→128→256).
            //
            // Skip atlas for cascade level 0 (64³ draft) — uses resident volume only.
            // First atlas write happens at target resolution (128³+). This avoids a
            // wasted free+realloc when the cascade immediately escalates 64→128.
            // 3D grid atlas allocation: assign a cell for this body.
            // Skip draft bakes (64³) — only allocate at target resolution (128³+).
            // EXCEPTION: realloc an EXISTING slot whose resolution no longer matches req. A deliberate
            // down-switch (Grid 128→64) otherwise leaves a stale 128 slot here, and `effective_res`
            // (below) reads the slot's resolution to drive the bake's grid_res / index math — so a stale
            // slot makes the shader index with the OLD (larger) res into a buffer sized at the NEW
            // (smaller) res. Naga robustness then drops every out-of-range write (all but the bottom
            // octant) → a corrupt/empty grid → the body goes INVISIBLE when static, or only its bottom
            // slab meshes and re-bakes at each position while moving (the "staggered ghost slabs").
            // First-time 64³ cascade drafts (no prior slot) stay skipped — the no-wasted-alloc opt holds.
            // Only a slot LARGER than the request corrupts (effective_res > buffer size → index overflow).
            // A slot SMALLER than req is the legit atlas-clamp case (e.g. 1024³ capped to atlas dims) and
            // must NOT trigger a realloc every bake.
            let stale_atlas_slot = gpu.volume_atlas.as_ref()
                .and_then(|a| a.slot(req.body_nid))
                .map_or(false, |s| (0..3).any(|i| s.resolution[i] > req.resolution[i]));
            // S2.4: sparse bakes never touch the VolumeAtlas — mode 2 skips the textureStore
            // entirely, so allocating (or reallocating) a 3D cell here would waste atlas space AND
            // make `atlas_pad`/`effective_res`/`direct_tex_view` below disagree with the
            // classifier's unpadded [0,res)³ domain. Sparse forces pad=0 / offset=0 / dummy tex.
            if !sparse && (max_res >= 128 || stale_atlas_slot) {
                if let Some(ref mut atlas) = gpu.volume_atlas {
                    match atlas.allocate_or_evict(req.body_nid, req.resolution) {
                        Some((_idx, evicted_nids)) => {
                            for nid in &evicted_nids {
                                self.cache.body_vol_cache.cache_body_to_ram(*nid);
                                self.cache.body_vol_cache.mark_body_dirty(*nid);
                            }
                        }
                        None => {
                            eprintln!("[Atlas] FULL — cannot allocate cell for {:?} res={:?} (grid {}/{})",
                                req.body_nid, req.resolution, atlas.grid_used(), atlas.grid_capacity());
                            // Append-only free leaves holes; if any exist, a compact will reclaim space.
                            // (If not, it's a genuine overflow — don't request, to avoid a compact-loop.)
                            if atlas.has_reclaimable_holes() {
                                atlas.request_compact = true;
                            }
                        }
                    }
                    // No clear needed — bakes fully overwrite padded region (Constraint C3).
                }
            }

            // Output buffer size. In TextureOnly mode the flat output_grid (binding 2) is a pure
            // placeholder: its sole write (volume_bake.wgsl:1013) is `output_mode == 0u`-guarded and
            // TextureOnly sets output_mode=1, so the shader never touches it — and here it is never seeded
            // (guarded below), never copied to staging (staging_buf is None), and never read back. F2:
            // allocate a MINIMAL buffer instead of grid_byte_len. The old code sized it
            // grid_byte_len.min(max_buf) — up to a ~4GB dummy at 1024³ — on the false premise that wgpu's
            // validator rejects an under-sized buffer. Binding 2 is a runtime-sized array<f32> with
            // min_binding_size:None and naga bounds-checks runtime-array access, so a 256-byte placeholder
            // validates and is safe (the guarded write never fires). Saves ~1–4GB VRAM per >512³ body.
            let output_buf_size = if texture_only {
                256
            } else {
                grid_byte_len.max(4)
            };
            // GPU-AUTHORITY (path A): when the resident volume already holds THIS body's field at this res
            // (and wasn't just realloc'd), bake the dirty slab DIRECTLY into rv.buffer in place — no CPU
            // re-seed, no throwaway output_buf, no output_buf→rv copy. The partial bake writes ONLY
            // [bake_min,bake_max) (proven: partial_bake_writes_exact_footprint); the rest is the live
            // accumulated field. A bounds change forces a FULL bake (bake_dispatch.rs:585) that overwrites
            // rv.buffer entirely, so cross-bounds accumulation is impossible. wgpu::Buffer is Arc-backed —
            // the clone is cheap and releases the body_vol_cache borrow.
            let rv_bake_buf: Option<std::sync::Arc<eframe::wgpu::Buffer>> = if texture_only
                || sparse // S2.4: mode 2 writes the brick POOL, never rv.buffer (bake_into_resident=false)
                || self.cache.body_vol_cache.resident_volume_reallocated
                || self.cache.body_vol_cache.resident_body_nid != Some(req.body_nid)
            {
                None
            } else {
                self.cache.body_vol_cache.resident_volume().and_then(|rv| {
                    (rv.resolution == req.resolution && rv.buffer.size() >= grid_byte_len)
                        .then(|| rv.buffer.clone())
                })
            };
            let bake_into_resident = rv_bake_buf.is_some();

            // Throwaway output buffer — only when NOT baking in place. Seeded from the CPU base_grid so a
            // partial bake preserves the non-dirty region. Path A needs neither (rv.buffer already holds it).
            // S2.4: sparse bakes need none either — binding 2 is the persistent atlas POOL below.
            let output_buf = if bake_into_resident || sparse {
                None
            } else {
                // Dense-cap invariant (the dense-vs-sparse split): a dense GridBacked output
                // above max_gridbacked_resolution (e.g. 1024³ = 4GB) exceeds the storage-binding
                // limit — such a request must have routed SparseBricks or TextureOnly upstream
                // (max_mesh_resolution clamps + the sparse fork). Fail loudly here, never as a
                // device loss inside the driver.
                assert!(
                    texture_only
                        || grid_byte_len <= device.limits().max_storage_buffer_binding_size as u64,
                    "dense GridBacked bake at {:?} ({}MB) exceeds the storage-binding limit — \
                     a >{}³ request must route SparseBricks (sparse regime) or TextureOnly",
                    req.resolution, grid_byte_len >> 20,
                    crate::gpu_jobs::max_gridbacked_resolution(),
                );
                let buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("primary_bake_output"),
                    size: output_buf_size,
                    usage: eframe::wgpu::BufferUsages::STORAGE
                        | eframe::wgpu::BufferUsages::COPY_DST
                        | eframe::wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                if !texture_only {
                    // Seed the non-dirty region from the body's cached grid — materialized lazily HERE (only
                    // the rare non-resident / realloc path reaches this throwaway-output_buf branch; path A
                    // bakes in place and never does), so the 64MB clone is OFF the per-partial-edit hot path.
                    // `req.base_grid` is the worker-path seed (None on the primary path); else pull body_grids.
                    let seed: Option<Vec<f32>> = if let Some(ref base) = req.base_grid {
                        Some(base.clone())
                    } else {
                        self.cache.body_vol_cache.body_grid(req.body_nid)
                            .filter(|(g, ..)| (g.len() as u64) * 4 == grid_byte_len)
                            .map(|(g, ..)| g.to_vec())
                    };
                    if let Some(base) = seed {
                        let len = (base.len() as u64 * 4).min(grid_byte_len);
                        queue.write_buffer(&buf, 0, &bytemuck::cast_slice(&base)[..len as usize]);
                    }
                }
                Some(buf)
            };
            // S2.4/S2.5 sparse atlas prep (POLL context — Frame Ordering Contract).
            // FULL: get-or-create the persistent SdfBrickAtlasGpu, grow it to this bake's
            // index/pool needs (generation bump on grow — counters over timing tricks), and
            // upload ONLY the classifier INDEX. The POOL content comes from the bake itself
            // (binding 2 below). The atlas generation is the brick_resident marker recorded at
            // completion (together with the CPU-retained frozen index); the DC sparse arm trusts
            // the atlas only while body + res + generation still match.
            // PARTIAL (S2.5): validated against the live atlas above — NO ensure_capacity
            // (nothing grows, so the generation must NOT bump: no buffer was recreated, only pool
            // CONTENT changes) and NO upload_index (the index is FROZEN — that is the whole
            // point). The windowed mode-2 dispatch below updates the pool in place.
            // SPARSE POOL CEILING (the 1024 device-loss guard, 2026-07-13 — sibling of the dense
            // binding-size assert above): the atlas POOL is bound as ONE storage buffer (binding
            // 2 of the bake, binding 7 of the DC mesher), so `allocated × 2048B` must fit
            // `max_storage_buffer_binding_size`. Nothing upstream bounded it — a fast create-drag
            // at 1024³ ballooned the pool 355MB → 1.43GB in seconds (AZRA_AUTOREPRO capture) and
            // the next ×1.25 grow crossed the 2.1GB limit → wgpu validation error per dispatch →
            // "Command encoder is invalid" cascade → device loss (the owner's crash). The
            // oversized-window escalation in trigger_bake keeps the common path far below this;
            // if a scene's TRUE surface at this resolution still exceeds the ceiling, SELF-HEAL:
            // drop the body's resolution one notch (1024 → 512 ≈ ¼ the bricks) and re-bake,
            // with a loud line + toast — never hand the driver an unbindable buffer.
            // ONE ceiling source (2048-SHIP item 3): the same function
            // `SdfBrickAtlasGpu::ensure_capacity` clamps its grow pad against — the self-heal
            // below and the pad can never disagree about where the limit is.
            let pool_ceiling_bricks = graphics::sdf_brick_atlas::pool_ceiling_bricks(
                gpu.device.limits().max_storage_buffer_binding_size as u64);
            let sparse_over_ceiling = match &sparse_kind {
                Some(crate::body_volume_cache::SparseBakeRequest::Full(si)) =>
                    (si.allocated as u32) > pool_ceiling_bricks,
                Some(crate::body_volume_cache::SparseBakeRequest::Partial {
                    index_update: Some(updated), ..
                }) => (updated.allocated as u32) > pool_ceiling_bricks,
                _ => false,
            };
            if sparse_over_ceiling {
                let res = req.resolution[0];
                let body_nid = req.body_nid;
                let lower = (res / 2).max(64); // halving quarters the brick count — converges
                let alloc_n = match &sparse_kind {
                    Some(crate::body_volume_cache::SparseBakeRequest::Full(si)) => si.allocated,
                    Some(crate::body_volume_cache::SparseBakeRequest::Partial {
                        index_update: Some(updated), .. }) => updated.allocated,
                    _ => 0,
                };
                eprintln!(
                    "[SparseBake] body={body_nid:?} pool would exceed the storage-binding ceiling \
                     ({alloc_n} bricks > {pool_ceiling_bricks} cap) at {res}³ — self-healing: \
                     resolution dropped to {lower}³"
                );
                deferred_toasts.push(format!(
                    "Surface too dense for {}³ on this GPU — resolution dropped to {}³", res, lower));
                self.cache.body_vol_cache.clear_in_flight(body_nid);
                self.cache.body_vol_cache.set_body_resolution(body_nid, lower);
                // PT mesh snapshot keys on (content_hash, res) — force re-snap after self-heal.
                // Direct cache clear (avoid &mut self helper while req borrow may still be active).
                self.cache.pt_sdf_snap_hash.remove(&body_nid);
                self.cache.pt_sdf_snap_seen.remove(&body_nid);
                self.cache.pt_sdf_snap_pending.retain(|(n, ..)| *n != body_nid);
                self.cache.body_vol_cache.clear_bake_debounce();
                continue;
            }
            let sparse_pending: Option<crate::gpu_ctx::SparsePending> = match sparse_kind {
                Some(crate::body_volume_cache::SparseBakeRequest::Full(si)) => {
                    let index_needed = si.index.len() as u32;
                    let pool_bricks = si.allocated as u32;
                    // P2-B5 multi-tenancy: PER-BODY atlas — creating/growing THIS body's atlas
                    // no longer steals another body's residency.
                    // [SPIKE] split (permanent, >20ms): the FIRST 1024 sparse dispatch pays a
                    // one-time atlas pool allocation here (owner log 5: the 58.9ms first
                    // dispatch_pending_primary_bakes vs ~10-20ms after) — name it when it spikes.
                    let _t_atlas = std::time::Instant::now();
                    let atlas = gpu.sdf_brick_atlases.entry(req.body_nid).or_insert_with(|| {
                        graphics::SdfBrickAtlasGpu::new(device, index_needed, pool_bricks)
                    });
                    let _create_ms = _t_atlas.elapsed().as_secs_f64() * 1000.0;
                    let _gen_before = atlas.generation;
                    let _t_ensure = std::time::Instant::now();
                    atlas.ensure_capacity(device, queue, index_needed, pool_bricks);
                    let _ensure_ms = _t_ensure.elapsed().as_secs_f64() * 1000.0;
                    let _grew = atlas.generation != _gen_before;
                    let _split = atlas.upload_index(queue, &si.index, pool_bricks, si.dims_bricks,
                        req.resolution[0], req.body_nid);
                    let _atlas_ms = _t_atlas.elapsed().as_secs_f64() * 1000.0;
                    if _atlas_ms > 20.0 {
                        // Slice D split: create (first-time buffer alloc) / ensure (grow realloc +
                        // preserve-copy; `grew` marks a generation bump) / the upload_index phases
                        // (index write_buffer vs the two CPU index scans).
                        eprintln!("[SPIKE] sparse atlas create/grow/upload body={:?}: {:.1}ms (index={} pool_bricks={} ≈{}MB) | create={:.1} ensure={:.1} (grew={}) write={:.1} bbox={:.1} tiles={:.1}",
                            req.body_nid, _atlas_ms, index_needed, pool_bricks,
                            (pool_bricks as u64 * graphics::sdf_brick_atlas::BRICK_FLOATS as u64 * 4) >> 20,
                            _create_ms, _ensure_ms, _grew,
                            _split.write_ms, _split.bbox_ms, _split.tiles_ms);
                    }
                    // The owner's visibility line — one per sparse FULL bake (debug-toggle path
                    // only, so unconditional like [Retighten]; grep target: [SparseBake]).
                    eprintln!("[SparseBake] dispatch body={:?} res={}³ bricks={}/{} pool={:.1}MB (atlas gen={} caps: index={} pool={})",
                        req.body_nid, req.resolution[0], si.allocated, si.index.len(),
                        (si.allocated as f64 * 512.0 * 4.0) / (1024.0 * 1024.0),
                        atlas.generation, atlas.index_capacity, atlas.pool_capacity_bricks);
                    Some(crate::gpu_ctx::SparsePending::Full {
                        atlas_gen: atlas.generation,
                        // S2.5: retain the EXACT uploaded index CPU-side — record_sparse_bake
                        // stores it on the entry next to the brick_resident marker.
                        frozen: std::sync::Arc::new(si),
                    })
                }
                Some(crate::body_volume_cache::SparseBakeRequest::Partial {
                    atlas_generation, index_update, ..
                }) => {
                    // [PartialBake] (crate::perf_spam_on()) below is the per-window log; keep this path
                    // spam-free — it runs every drag frame.
                    // S2.5c: incremental allocation appended handles at trigger time — grow the
                    // pool (CONTENT-PRESERVING: ensure_capacity GPU-copies the old bricks into
                    // the replacement buffer) and re-upload the updated index. The post-grow
                    // atlas generation travels to completion, which installs the new frozen
                    // index + stamps the marker (never at trigger — a dropped bake must not
                    // leave the CPU index claiming unbaked handles).
                    let update = match index_update {
                        Some(updated) => {
                            let atlas = gpu.sdf_brick_atlases.get_mut(&req.body_nid)
                                .expect("partial sparse validated against a live atlas above");
                            atlas.ensure_capacity(
                                device, queue,
                                updated.index.len() as u32, updated.allocated as u32);
                            atlas.upload_index(
                                queue, &updated.index, updated.allocated as u32,
                                updated.dims_bricks, req.resolution[0], req.body_nid);
                            Some((updated, atlas.generation))
                        }
                        None => None,
                    };
                    Some(crate::gpu_ctx::SparsePending::Partial { atlas_gen: atlas_generation, update })
                }
                None => None,
            };
            // P2-B5: LRU touch + budget enforcement — ONCE per sparse dispatch, after every
            // atlas mutation above and BEFORE any long-lived borrow of this body's atlas below
            // (bake_out / brick_index_res). `keep = req.body_nid`: the body being dispatched is
            // never evicted, so the borrows below can't dangle. Evicting ANOTHER body's atlas
            // is GPU-safe mid-frame (wgpu keeps submitted work's buffers alive internally);
            // that body keeps its drawn mesh and self-heals with a full re-bake on next edit.
            if sparse {
                gpu.sdf_atlas_lru.insert(req.body_nid, self.diag_frame);
                for (nid, freed) in crate::gpu_ctx::evict_sdf_atlases_over_budget(
                    &mut gpu.sdf_brick_atlases, &mut gpu.sdf_atlas_lru,
                    req.body_nid, gpu.sdf_atlas_budget_bytes)
                {
                    eprintln!("[SparseEvict] body={:?} atlas evicted ({}MB freed) — LRU over the {}MB budget",
                        nid, freed >> 20, gpu.sdf_atlas_budget_bytes >> 20);
                }
            }
            // Brick-grid dims for BakeParams routing (mode 2 only; zeros in dense modes).
            let sparse_dims: [u32; 3] = match &sparse_pending {
                Some(crate::gpu_ctx::SparsePending::Full { frozen, .. }) => frozen.dims_bricks,
                Some(crate::gpu_ctx::SparsePending::Partial { .. }) => gpu.sdf_brick_atlases
                    .get(&req.body_nid)
                    .expect("partial sparse validated against a live atlas above")
                    .dims_bricks,
                None => [0; 3],
            };
            // The buffer the bake writes (binding 2) and the readback source: the sparse brick
            // POOL (mode 2), rv.buffer in place (A), or the throwaway output_buf (legacy).
            let bake_out: &eframe::wgpu::Buffer = if sparse {
                &gpu.sdf_brick_atlases.get(&req.body_nid)
                    .expect("sparse atlas prepared above for a sparse_index request")
                    .pool_buf
            } else {
                rv_bake_buf.as_deref().or(output_buf.as_ref())
                    .expect("bake_out: rv_bake_buf (path A) or output_buf (legacy) must be Some")
            };

            // Staging buffer for readback: created LATER (after the resident_full_direct decision) —
            // path-A field-direct fulls never read it, so allocating 512MB per bake for them was the
            // dominant per-bake VRAM churn at 512³. See the creation site below `resident_full_direct`.

            // Write BakeParams uniform
            let prim_count = req.prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32;

            // Atlas padding: when the body has an atlas slot, the bake shader extends
            // its evaluation region by PAD voxels on each side. This fills padding texels
            // with valid SDF data beyond the nominal bounds, preventing trilinear
            // sampling artifacts at body slab boundaries.
            // S2.4: a sparse bake evaluates the classifier's UNPADDED [0,res)³ domain — the brick
            // index was built for exactly grid_res=req.resolution, pad=0, offset=0. A STALE
            // VolumeAtlas slot from a previous dense era (skipped above, never reallocated for
            // sparse) must not leak its pad/clamp into the params or the brick_linear math desyncs
            // from the index.
            let atlas_pad = if !sparse && gpu.volume_atlas.as_ref()
                .and_then(|a| a.slot(req.body_nid)).is_some()
            {
                graphics::volume_atlas::ATLAS_PAD
            } else {
                0 // no atlas or no slot — no padding needed
            };

            // Use atlas slot's actual resolution (may be clamped to atlas dims).
            // Without this, a 1024³ request would produce padded_res=1028³ which
            // exceeds the atlas texture size → out-of-bounds textureStore → GPU crash.
            let effective_res = if sparse {
                req.resolution // classifier domain — never an atlas slot's clamped res
            } else {
                gpu.volume_atlas.as_ref()
                    .and_then(|a| a.slot(req.body_nid))
                    .map(|s| s.resolution)
                    .unwrap_or(req.resolution)
            };
            // AZRA_PROBE confirmation for the thin-slab fix: the atlas slot must never be SMALLER
            // than the requested bake resolution — a smaller slot makes the shader write only a
            // sub-block of the larger output buffer (the 1/8-filled thin slab). This flags any
            // remaining grow that the atlas failed to honor.
            if std::env::var_os("AZRA_PROBE").is_some()
                && (0..3).any(|i| effective_res[i] < req.resolution[i])
            {
                eprintln!("[BAKE-RES] body={:?} UNDER-SIZED SLOT effective_res={:?} < req.resolution={:?} → thin-slab write",
                    req.body_nid, effective_res, req.resolution);
            }
            let padded_res = [
                effective_res[0] + 2 * atlas_pad,
                effective_res[1] + 2 * atlas_pad,
                effective_res[2] + 2 * atlas_pad,
            ];

            // Convert bake region from body-resolution space to padded-grid space.
            // Body voxel v maps to padded voxel (v + pad). To also fill padding
            // texels adjacent to dirty body voxels, expand by pad on each side:
            //   padded_min = (body_min + pad) - pad = body_min
            //   padded_max = (body_max + pad) + pad = body_max + 2*pad
            // Clamped to [0, padded_res) per axis.
            let bake_min = [
                req.bake_min[0].min(padded_res[0]),
                req.bake_min[1].min(padded_res[1]),
                req.bake_min[2].min(padded_res[2]),
            ];
            let bake_max = [
                (req.bake_max[0] + 2 * atlas_pad).min(padded_res[0]),
                (req.bake_max[1] + 2 * atlas_pad).min(padded_res[1]),
                (req.bake_max[2] + 2 * atlas_pad).min(padded_res[2]),
            ];

            let params = crate::gpu_jobs::BakeParams {
                grid_res: padded_res,
                _pad0: 0,
                bounds_min: req.bounds_min.into(),
                _pad1: 0.0,
                bounds_max: req.bounds_max.into(),
                prim_count,
                floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
                // SparseBricks: the classifier's brick-grid dims (brick_linear routing). Dense
                // modes ignore it.
                dims_bricks: sparse_dims,
                bake_min,
                _pad3: 0,
                bake_max,
                _pad4: 0,
                mirror_flags: req.mirror_flags,
                mirror_center: req.mirror_center.into(),
                bevel_radius: req.bevel_radius,
                mirror_sign: req.mirror_sign,
                // Atlas 3D offset: if the body has a cell in the atlas, write to that sub-region.
                // If not (or no atlas), write to origin (per-body ResidentVolume, backward compat).
                atlas_offset: gpu.volume_atlas.as_ref()
                    .and_then(|a| a.body_atlas_offset(req.body_nid))
                    .unwrap_or([0, 0, 0]),
                atlas_pad,
                output_mode: bake_mode as u32,
                _pad5: 0,
                // LAB mesh operand: bake points are body-LOCAL — the shader lifts them
                // to world with this before sampling the world-space operand grid.
                body_local_to_world: self.scene.scene_graph
                    .world_transform(req.body_nid).to_cols_array_2d(),
            };
            let params_buf = gpu.buffers.primary_bake_params_buffer.as_ref().unwrap();
            queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&params));

            // Resolve the 3D texture for direct-to-texture bake output.
            // Priority: 1) Atlas texture (multi-body), 2) ResidentVolume (single-body), 3) Dummy
            // S2.4: sparse bakes bind the DUMMY — mode 2's textureStore is gated off in the shader
            // (`params.output_mode != 2u`, volume_bake.wgsl), and binding a real target would set
            // will_direct_write below, which would LIE (deferred_direct_writes marks slot.valid +
            // gpu_texture_upload_done for content that was never written).
            let atlas_tex_view = gpu.volume_atlas.as_ref()
                .filter(|_| !sparse)
                .filter(|a| a.slot(req.body_nid).is_some())
                .map(|a| a.texture.create_view(&eframe::wgpu::TextureViewDescriptor::default()));
            // Per-body ResidentVolume direct write: rv.texture is GONE (legacy VP path removed
            // 2026-07-13) — the bake binds the DUMMY at binding 3 (the sparse precedent; the
            // shader's out-of-bounds textureStore is a no-op). `rv_direct` keeps the SAME gate the
            // texture leg had, because `will_direct_write` also carries the rv.buffer provenance:
            // the deferred block below latches `gpu_texture_upload_done` for bake_into_resident
            // bakes and stamps rv.bake_bounds — that bookkeeping must keep firing.
            let rv_direct = atlas_tex_view.is_none() && !sparse
                && self.cache.body_vol_cache.resident_body_nid == Some(req.body_nid)
                && self.cache.body_vol_cache.resident_volume()
                    .map_or(false, |rv| rv.resolution == req.resolution);
            let dummy_tex_view = gpu.bake_tex_dummy.create_view(&eframe::wgpu::TextureViewDescriptor::default());
            let tex_view = atlas_tex_view.as_ref().unwrap_or(&dummy_tex_view);
            let will_direct_write = atlas_tex_view.is_some() || rv_direct;

            // Create clip curve buffer
            let clip_bytes: &[u8] = bytemuck::cast_slice(&req.clip_curve_floats);
            let clip_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("primary_bake_clips"),
                size: (clip_bytes.len() as u64).max(4),
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&clip_buf, 0, clip_bytes);

            // LAB poly→SDF: per-prim MeshVolume grids side buffer (binding 5). Packed by
            // serialize_mesh_grids_for_bake; each MeshVolume prim's hot[86] indexes into it.
            // Always ≥1 float, so no dummy needed on the primary path.
            let mesh_bytes: &[u8] = bytemuck::cast_slice(&req.mesh_grid_floats);
            let mesh_grid_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("primary_bake_mesh_grids"),
                size: (mesh_bytes.len() as u64).max(4),
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&mesh_grid_buf, 0, mesh_bytes);

            // T1 image displace height atlas (binding 8).
            let height_bytes: &[u8] = bytemuck::cast_slice(&req.height_map_floats);
            let height_map_buf = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                label: Some("primary_bake_height_maps"),
                size: (height_bytes.len() as u64).max(4),
                usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&height_map_buf, 0, height_bytes);

            // binding(6): SparseBricks brick→pool-handle map. Mode 2 binds the persistent
            // atlas index (already validated + uploaded above for this dispatch); dense modes
            // never read binding 6 — the mesh_grid buffer doubles as a harmless filler (both
            // are read-only storage; re-binding one buffer at two slots is legal). The merge's
            // interim wiring bound ONLY mesh_grid at 5 and had mode 2 bitcast handles out of
            // it — every sparse bake read LAB mesh floats as pool handles (the stepped-ledge
            // surface corruption). brick_index owning binding 6 ends that class: mesh_grid
            // stays live in mode 2 (a sparse body can carry MeshVolume prims).
            let brick_index_res: &eframe::wgpu::Buffer = if sparse {
                &gpu.sdf_brick_atlases.get(&req.body_nid)
                    .expect("sparse atlas prepared above for a sparse_index request")
                    .index_buf
            } else {
                &mesh_grid_buf
            };

            // Create bind group
            let bg = device.create_bind_group(&eframe::wgpu::BindGroupDescriptor {
                label: Some("primary_bake_bg"),
                layout: bgl,
                entries: &[
                    eframe::wgpu::BindGroupEntry {
                        binding: 0,
                        resource: prim_buf.as_entire_binding(),
                    },
                    eframe::wgpu::BindGroupEntry {
                        binding: 1,
                        resource: params_buf.as_entire_binding(),
                    },
                    eframe::wgpu::BindGroupEntry {
                        binding: 2,
                        resource: bake_out.as_entire_binding(),
                    },
                    eframe::wgpu::BindGroupEntry {
                        binding: 3,
                        resource: eframe::wgpu::BindingResource::TextureView(tex_view),
                    },
                    eframe::wgpu::BindGroupEntry {
                        binding: 4,
                        resource: clip_buf.as_entire_binding(),
                    },
                    // binding(5): LAB poly→SDF per-prim MeshVolume grids (packed side buffer).
                    eframe::wgpu::BindGroupEntry {
                        binding: 5,
                        resource: mesh_grid_buf.as_entire_binding(),
                    },
                    // binding(6): sparse brick index (mode 2) / filler (modes 0-1, unread).
                    eframe::wgpu::BindGroupEntry {
                        binding: 6,
                        resource: brick_index_res.as_entire_binding(),
                    },
                    // binding(7): curv_positions — read ONLY by the `curvature_eval` entry,
                    // which the bake never dispatches; mesh_grid fills it so the bind group
                    // matches the shared layout (the bindings 5/6 filler precedent).
                    eframe::wgpu::BindGroupEntry {
                        binding: 7,
                        resource: mesh_grid_buf.as_entire_binding(),
                    },
                    // binding(8): T1 image displace height atlas.
                    eframe::wgpu::BindGroupEntry {
                        binding: 8,
                        resource: height_map_buf.as_entire_binding(),
                    },
                ],
            });

            // Workgroup size is (4,4,4). Dispatch covers the padded bake region.
            let bake_extent = [
                bake_max[0].saturating_sub(bake_min[0]),
                bake_max[1].saturating_sub(bake_min[1]),
                bake_max[2].saturating_sub(bake_min[2]),
            ];
            let region_voxels = bake_extent[0] as u64 * bake_extent[1] as u64 * bake_extent[2] as u64;
            let full_voxels = padded_res[0] as u64 * padded_res[1] as u64 * padded_res[2] as u64;
            let is_partial = region_voxels < full_voxels;
            if is_partial { partial_bakes += 1; } else { full_bakes += 1; } // Gate 0 rebake tally
            // PATH-A: a RESIDENT FULL bake already wrote rv.buffer in place — skip the CPU to_vec
            // readback and mesh straight from rv.buffer, routing completion through
            // note_resident_bake_complete (sets resident_buffer_valid=TRUE).
            //
            // This is the DEFAULT for every resident full bake (nudges, escapes, drag fulls) — zero
            // GPU→CPU copy per edit. The old `is_busy()` gate was interaction-state sniffing (a
            // latency trick): it forced every idle-classified edit (keyboard nudge, settle) through
            // the 64MB copy. Replaced with explicit per-bake intent: `take_cpu_heal` is true ONLY for
            // the bake the settle re-tighten queued as the heal (check_idle_retighten marks it), so
            // exactly ONE readback fires per settle to refresh body_grids (the CPU cache that
            // RAM-eviction / CPU-DC fallback / resample read). A no-copy completion marks the body
            // cpu_grid_stale + re-arms pending_retighten (note_cpu_grid_stale), so the heal is
            // guaranteed even for a full bake that cleared its own pending_retighten at dispatch.
            let heal_readback = self.cache.body_vol_cache.take_cpu_heal(req.body_nid);
            // SETTLE-LOOP FIX (2026-07-16): a no-copy completion is only LEGAL for a body that
            // already has a body_grids entry — "no-copy" means "skip refreshing the CPU cache",
            // and with no entry there is nothing to skip. A FIRST bake that completed no-copy
            // never registered in body_grids, so trigger_bake dispatched the 64³ coarse cascade
            // forever: coarse → PATH-A no-copy → note_cpu_grid_stale (re-arms settle) →
            // advance_cascade re-dirties → coarse again — the owner's endless
            // "[RetightenArm] by=no_copy_completion while IDLE" spam. Requiring an existing
            // entry makes the first-bake cascade contract (accept_gpu_result inserts the entry)
            // hold BY CONSTRUCTION. Steady-state drags are untouched (their entry exists).
            let resident_full_direct = bake_into_resident
                && self.cache.gpu_full_bake_path_a
                && !is_partial
                && !heal_readback
                && self.cache.body_vol_cache.has_cached_grid(req.body_nid);
            // STAGING: allocate the readback buffer ONLY for bakes that will actually READ it (the
            // once-per-settle heal + partials that may fall back + legacy/non-resident paths). A
            // path-A field-direct full never reads it — creating a fresh 512MB MAP_READ buffer +
            // copying the whole grid into it per bake was the dominant per-bake VRAM churn at 512³
            // (a large slice of the measured 837ms first-512 frame). Completion for the no-staging
            // case is fenced by queue.on_submitted_work_done at the push site — a true GPU fence,
            // preserving the map_async backpressure semantics without the buffer.
            // S2.4: sparse bakes never read back (the TextureOnly staging precedent) — the result
            // lives in the atlas POOL, meshed GPU-side. `take_cpu_heal` above still CONSUMED any
            // pending heal mark for this body: a heal readback has no dense output to read on the
            // sparse path, and check_idle_retighten's brick-resident guard prevents re-queuing it.
            let staging_buf = if texture_only || resident_full_direct || sparse {
                None
            } else {
                Some(device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("primary_bake_staging"),
                    size: grid_byte_len.max(4),
                    usage: eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }))
            };
            // [visrec] flight recorder: bake request dispatch decision (Part B #1).
            crate::vis_recorder::visrec(self.diag_frame, "bake.dispatch", || format!(
                "body={:?} res={:?} bounds={:.3?}..{:.3?} is_partial={} bake_into_resident={} \
                 resident_full_direct={} heal_readback={} texture_only={} sparse={} staging={}",
                req.body_nid, effective_res, req.bounds_min, req.bounds_max, is_partial,
                bake_into_resident, resident_full_direct, heal_readback, texture_only, sparse,
                if staging_buf.is_some() { "some" } else { "none" }));
            // [HealTrace] settle-scoped stderr mirror of the dispatch decision (drags never trace).
            // Parked behind crate::perf_spam_on() after the phantom-edit loop was resolved (2026-07-09).
            if crate::perf_spam_on() && self.cache.body_vol_cache.heal_tracing() {
                eprintln!(
                    "[HealTrace] dispatch body={:?} res={:?} partial={} heal_readback={} field_direct={} staging={} gen={}",
                    req.body_nid, effective_res, is_partial, heal_readback, resident_full_direct,
                    staging_buf.is_some(), req.generation);
            }
            if is_partial && crate::perf_spam_on() {
                eprintln!(
                    "[PartialBake] body={:?} region=[{},{},{}]->[{},{},{}] ({} voxels / {} padded = {:.0}%)",
                    req.body_nid,
                    bake_min[0], bake_min[1], bake_min[2],
                    bake_max[0], bake_max[1], bake_max[2],
                    region_voxels, full_voxels,
                    region_voxels as f64 / full_voxels as f64 * 100.0,
                );
            }
            let wg_x = (bake_extent[0] + 3) / 4;
            let wg_y = (bake_extent[1] + 3) / 4;
            let wg_z = (bake_extent[2] + 3) / 4;

            // TDR guard: split large dispatches into Z-slices.
            // 256³ = 262K workgroups can exceed the ~2s Windows TDR timeout.
            // Each slice dispatches ≤ MAX_WG_Z × wg_x × wg_y workgroups, then
            // submits + polls to reset the TDR timer between slices.
            const MAX_WG_Z_PER_SLICE: u32 = 16; // 16 workgroups × 4 = 64 voxels per Z-slice

            if wg_z <= MAX_WG_Z_PER_SLICE {
                // Small bake (≤128³): single dispatch, no slicing needed
                let mut encoder = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
                    label: Some("primary_bake_encoder"),
                });
                {
                    let mut cpass = encoder.begin_compute_pass(&eframe::wgpu::ComputePassDescriptor {
                        label: Some("primary_bake_pass"),
                        timestamp_writes: None,
                    });
                    cpass.set_pipeline(pipeline);
                    cpass.set_bind_group(0, &bg, &[]);
                    cpass.dispatch_workgroups(wg_x, wg_y, wg_z);
                }
                if let Some(ref sb) = staging_buf {
                    encoder.copy_buffer_to_buffer(bake_out, 0, sb, 0, grid_byte_len);
                }
                { let _st = std::time::Instant::now(); queue.submit(std::iter::once(encoder.finish())); let _sms = _st.elapsed().as_secs_f64()*1000.0; if _sms > 10.0 { eprintln!("[SPIKE] queue.submit(primary_bake): {:.1}ms", _sms); } }
            } else {
                // Large bake (256³+): split Z into slices, submit+poll between each
                // to prevent TDR. The shader indexes output by absolute voxel coords
                // (gid + bake_min), so updating bake_min/bake_max Z between slices
                // writes to correct positions in the shared output buffer.
                let voxels_per_slice = MAX_WG_Z_PER_SLICE * 4;
                let total_z_voxels = bake_extent[2];
                let num_slices = (total_z_voxels + voxels_per_slice - 1) / voxels_per_slice;
                let orig_bake_min_z = bake_min[2];
                let orig_bake_max_z = bake_max[2];

                for slice_idx in 0..num_slices {
                    let slice_min_z = orig_bake_min_z + slice_idx * voxels_per_slice;
                    let slice_max_z = (slice_min_z + voxels_per_slice).min(orig_bake_max_z);
                    let slice_wg_z = (slice_max_z - slice_min_z + 3) / 4;

                    // Update params uniform with this slice's Z range
                    let mut slice_params = params;
                    slice_params.bake_min[2] = slice_min_z;
                    slice_params.bake_max[2] = slice_max_z;
                    queue.write_buffer(params_buf, 0, bytemuck::bytes_of(&slice_params));

                    let mut encoder = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
                        label: Some("primary_bake_encoder"),
                    });
                    {
                        let mut cpass = encoder.begin_compute_pass(&eframe::wgpu::ComputePassDescriptor {
                            label: Some("primary_bake_pass"),
                            timestamp_writes: None,
                        });
                        cpass.set_pipeline(pipeline);
                        cpass.set_bind_group(0, &bg, &[]);
                        cpass.dispatch_workgroups(wg_x, wg_y, slice_wg_z);
                    }

                    // Last slice: copy output → staging (GridBacked only)
                    if slice_idx == num_slices - 1 {
                        if let Some(ref sb) = staging_buf {
                            encoder.copy_buffer_to_buffer(bake_out, 0, sb, 0, grid_byte_len);
                        }
                    }

                    queue.submit(std::iter::once(encoder.finish()));

                    // No poll(Wait) — queue.submit() is non-blocking and the GPU
                    // processes submissions in order. TDR timer resets between
                    // command buffer boundaries without blocking the render thread.
                }
                if crate::perf_spam_on() {
                    eprintln!(
                        "[PrimaryBake] Split {:?} dispatch into {} Z-slices ({}wg each) to avoid TDR",
                        req.resolution, num_slices, MAX_WG_Z_PER_SLICE
                    );
                }
            }

            // GPU-to-GPU: copy bake output directly to BOTH the flat buffer AND the
            // 3D texture on the resident volume. Eliminates the entire CPU round-trip.
            // Skip in TextureOnly mode — the flat buffer is a dummy.
            // S2.4: sparse has no flat output to copy (output_buf is None — pool is the output).
            let gpu2gpu_info = if texture_only || sparse { None }
            else if self.cache.body_vol_cache.resident_body_nid == Some(req.body_nid) {
                self.cache.body_vol_cache.resident_volume().and_then(|rv| {
                    if rv.resolution == req.resolution
                        && rv.buffer.size() >= grid_byte_len
                    {
                        Some(())
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            if bake_into_resident {
                // Path A: the bake wrote the dirty slab straight into rv.buffer (and the resident/atlas
                // texture via binding 3). The flat copy is unneeded — the resident field IS the bake output.
                // The CPU texture re-upload is skipped via `gpu_texture_upload_done`, set in the deferred
                // direct-write completion block below — no per-branch flag is needed here.
            } else if let Some(()) = gpu2gpu_info {
                // Re-borrow after the immutable reference is dropped
                let rv = self.cache.body_vol_cache.resident_volume().unwrap();
                let mut conv_enc = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
                    label: Some("gpu2gpu_buf_copy"),
                });
                // Copy to flat storage buffer (GPU-to-GPU, no CPU involved)
                conv_enc.copy_buffer_to_buffer(output_buf.as_ref().unwrap(), 0, &rv.buffer, 0, grid_byte_len);

                // NOTE: copy_buffer_to_texture requires bytes_per_row aligned to 256.
                // The bake output buffer is tightly packed (res * 4 bytes per row).
                // For resolutions not divisible by 64 (e.g. 96), alignment fails → garbage.
                // The 3D texture upload goes through the CPU write_texture path which
                // handles alignment internally. GPU-direct texture copy needs a padded
                // intermediate buffer (future optimization).

                queue.submit(std::iter::once(conv_enc.finish()));

                if crate::perf_spam_on() {
                    eprintln!("[GPU2GPU] body={:?} partial={} region_voxels={} (flat buffer)",
                        req.body_nid, is_partial, region_voxels);
                }
            }

            // Collect direct-write info before the gpu borrow ends.
            // Actual flag-setting happens after the gpu block closes (see below).
            if will_direct_write {
                // Capture the body transform at bake time so the atlas slot
                // can map its bake bounds to the correct world position.
                let bake_bt = self.scene.scene_graph.body_world_transform(req.body_nid)
                    .unwrap_or_default();
                // Capture atlas generation for stale bake rejection.
                let atlas_gen = gpu.volume_atlas.as_ref()
                    .and_then(|a| a.slot_generation(req.body_nid))
                    .unwrap_or(0);
                deferred_direct_writes.push((req.body_nid, effective_res, req.bounds_min, req.bounds_max, bake_bt, atlas_gen, bake_into_resident));
            }

            // Register the completion fence — poll_primary_bake checks map_ready.
            // In TextureOnly mode: no staging buffer, no readback, no PendingBake.
            // The bake result lives solely in the 3D texture.
            if !texture_only {
                let map_ready = std::sync::Arc::new(crate::map_state::MapState::new());
                let map_ready_clone = map_ready.clone();
                match &staging_buf {
                    Some(staging) => {
                        // Legacy/heal readback: the staging map completion IS the fence.
                        staging.slice(..).map_async(eframe::wgpu::MapMode::Read, move |result| {
                            // Capture the Result — an Err (device loss / OOM / aborted map) is recorded as
                            // ERR, not discarded. poll_primary_bake drains ERR entries and recovers the body.
                            map_ready_clone.store_result(&result);
                        });
                    }
                    None => {
                        // Path-A field-direct: no staging buffer exists. Fence on the submitted bake
                        // work directly — fires when the GPU finishes everything submitted so far
                        // (the bake dispatch above). Same backpressure semantics as the map, zero
                        // buffer. Device loss surfaces through the normal device-lost handler, not
                        // this callback, so Ok is the only signal it carries.
                        queue.on_submitted_work_done(move || {
                            map_ready_clone.store_result::<()>(&Ok(()));
                        });
                    }
                }

                gpu.bake_pending.push(PendingBake {
                    body_nid: req.body_nid,
                    generation: req.generation,
                    source_hash: req.source_hash,
                    staging_buffer: staging_buf,
                    map_ready,
                    grid_size: grid_voxels as usize,
                    resolution: effective_res,
                    is_partial,
                    field_direct: resident_full_direct, // path-A: skip the to_vec + route via note_resident_bake_complete
                    sparse: sparse_pending,             // S2.4/S2.5: routes completion to record_sparse_[partial_]bake
                    bounds_min: req.bounds_min,         // dispatch-time provenance (race-immune vs aabb_escape)
                    bounds_max: req.bounds_max,
                    bake_window: (req.bake_min, req.bake_max), // C2: dirty voxel window provenance
                });
            } else {
                // TextureOnly: still need to update the body_grids hash so the cache
                // knows this body was baked (prevents infinite re-dispatch).
                self.cache.body_vol_cache.record_texture_only_bake(
                    req.body_nid, req.source_hash, effective_res, req.generation);
            }

        }
        // Deferred toasts (queued inside the gpu borrow — e.g. the sparse pool-ceiling self-heal).
        for t in deferred_toasts {
            self.toast(&t);
        }
        // Process deferred direct-to-texture writes (outside gpu borrow scope)
        for (body_nid, res, bounds_min, bounds_max, bake_bt, atlas_gen, bake_into_resident) in deferred_direct_writes {
            // Reject stale bake: if the atlas slot was reallocated (new generation)
            // after this bake was dispatched, the bake data targets the old Z-slab.
            if let Some(ref gpu) = self.gpu {
                if let Some(ref atlas) = gpu.volume_atlas {
                    if let Some(current_gen) = atlas.slot_generation(body_nid) {
                        if atlas_gen < current_gen {
                            if cfg!(debug_assertions) {
                                eprintln!("[Atlas] STALE direct write rejected: body={:?} bake_gen={} slot_gen={}",
                                    body_nid, atlas_gen, current_gen);
                            }
                            continue;
                        }
                    }
                }
            }
            // Update atlas slot with the bake results.
            // The bake-time body transform is stored so rebuild_params can compute the
            // correct world AABB even if the body origin shifts before the next bake.
            if let Some(ref mut gpu) = self.gpu {
                if let Some(ref mut atlas) = gpu.volume_atlas {
                    // Read template hash before mutable borrow of slot
                    let th = atlas.body_hashes.get(&body_nid).copied().unwrap_or(0);
                    if let Some(slot) = atlas.slot_mut(body_nid) {
                        slot.bake_bounds_min = bounds_min;
                        slot.bake_bounds_max = bounds_max;
                        slot.bake_transform = bake_bt;
                        slot.valid = true;
                        slot.template_hash = th;
                    }
                    atlas.params_dirty = true;
                }
            }
            // Update the ResidentVolume ONLY when THIS body is the active/resident one AND the bake actually
            // wrote rv.buffer (bake_into_resident). A non-active body baking into its own ATLAS slot must NOT
            // latch gpu_texture_upload_done nor overwrite the RESIDENT slot's bounds (the resident slot belongs
            // to the active body) — otherwise the active body's STALE rv.buffer is later flagged "valid" and
            // meshed field-direct under THIS body's (wrong) bounds = the white-box ghost. The flag carries the
            // body so the consume side (update_resident_volume) re-checks it against the active body.
            if bake_into_resident && self.cache.body_vol_cache.resident_body_nid == Some(body_nid) {
                self.cache.body_vol_cache.gpu_texture_upload_done = true;
                self.cache.body_vol_cache.gpu_texture_upload_body = Some(body_nid);
                if let Some(slot_idx) = self.cache.body_vol_cache.resident_slot {
                    if let Some(slot) = self.cache.body_vol_cache.volume_pool.get_mut(slot_idx) {
                        if let Some(ref mut rv) = slot.volume {
                            rv.bake_bounds_min = bounds_min;
                            rv.bake_bounds_max = bounds_max;
                        }
                    }
                }
                self.cache.body_vol_cache.resident_upload_needed = false;
            }
            self.mark_dirty(DirtyFlags::RENDER);
            if crate::perf_spam_on() {
                eprintln!("[GPU-DIRECT-TEX] body={:?} res={:?} — bake wrote to atlas+texture",
                    body_nid, res);
            }
        }
        // Gate 0 — flush the full/partial tally into the (previously dead) rebake counters. Reached
        // here only after the &mut self.gpu loop released (self.mark_dirty above needs &mut self).
        self.perf.vol_full_rebakes += full_bakes;
        self.perf.vol_partial_rebakes += partial_bakes;
        let _ms = _spike_t0.elapsed().as_secs_f64() * 1000.0;
        if _ms > 10.0 { eprintln!("[SPIKE] dispatch_pending_primary_bakes: {:.1}ms", _ms); }
    }

    /// CPU rayon fallback for bake requests when primary GPU is unavailable.
    /// Re-collects primitives from the scene graph and dispatches on rayon.
    pub(crate) fn cpu_fallback_bakes(&mut self, requests: Vec<crate::body_volume_cache::PrimaryBakeRequest>) {
        for req in requests {
            let body_nid = req.body_nid;
            // SCOPE-AWARE field program in the TOP body's local frame — the same frame the
            // request's bounds (computed from `collect_body_prims_recursive`'s top-local
            // prims) and the GPU bake grid use. Nested sub-body boolean trees evaluate
            // exactly like the GPU scope stack machine (defect (1)/(2), COMPLETE_BUG_TRACKER
            // "boolean tree inside a sub-body not working"). This also removes the old
            // `offset_prims_to_local(collected, body_origin)` double-offset: under
            // BODY_LOCAL_PRIMS the collected prims were ALREADY top-local, so subtracting
            // the origin again baked the field displaced from its grid.
            let field = core_math::field_eval::BodyField::from_graph(
                &self.scene.scene_graph, body_nid,
            );

            let tx = self.cache.body_vol_cache.bake_tx.clone();
            let source_hash = req.source_hash;
            let generation = req.generation;
            // CPU fallback uses max-axis resolution (cubic grid).
            // Per-axis asymmetric grids are GPU-only — CPU path is a rare fallback.
            let res_max = req.resolution[0].max(req.resolution[1]).max(req.resolution[2]);
            let grid_min = req.bounds_min;
            let grid_max = req.bounds_max;
            let base_grid = req.base_grid;
            let bake_min = req.bake_min;
            let bake_max = req.bake_max;
            // Body-level mirror (TRUE UNION) + bevel + nested scopes all live INSIDE the
            // field program (built from the graph above) — same spec as the GPU bake.
            rayon::spawn(move || {
                // GATE 2: the window this job ACTUALLY re-evaluates (region bake honors
                // [bake_min, bake_max); the no-seed branch bakes the full grid).
                let bake_window = if base_grid.is_some() {
                    (bake_min, bake_max)
                } else {
                    ([0, 0, 0], [res_max; 3])
                };
                let grid = if let Some(existing) = base_grid {
                    crate::bake_utils::bake_body_grid_region_field(
                        &field, res_max, grid_min, grid_max,
                        existing, bake_min, bake_max,
                    )
                } else {
                    crate::bake_utils::bake_body_grid_field(
                        &field, res_max, grid_min, grid_max,
                    )
                };
                let _ = tx.send(crate::body_volume_cache::BodyBakeResult {
                    body_id: body_nid,
                    grid,
                    source_hash,
                    generation,
                    grid_min,
                    grid_max,
                    bake_resolution: res_max,
                    bake_window,
                });
            });
        }
    }

    /// STAGE-2 GPU CLASSIFY — dispatch queued sparse FULL classifies as `classify_bricks`
    /// compute passes (poll_background_tasks context, Frame Ordering Contract: all buffer
    /// creation happens here, never in trigger/panels). Each queued record becomes ONE dispatch
    /// over the whole brick grid + a verdict copy into a MAP_READ staging (~1MB at 512³);
    /// `poll_gpu_classify_readbacks` finalizes on a later frame. No device / pipeline compile
    /// failure → the record falls back to the CPU classify worker (already counted in
    /// `classify_pending`), so the flight ALWAYS lands through `classify_tx` either way.
    ///
    /// FIELD-CONSISTENCY: the GPU verdicts are the PRODUCTION truth here — the index must
    /// describe the field the mode-2 bake WRITES and the mesher READS (both GPU, same
    /// `eval_scene_at` chain). The historical CPU-vs-GPU box eval divergence (CPU tri-planar
    /// vs GPU exact-Euclidean `sdf_box`, up to ~2.6e-2 near corners) was FIXED 2026-07-13 by
    /// aligning `SdfShape::Box::distance` to the exact Euclidean form — gated by
    /// `gpu_classify_box_euclidean_alignment_gate` (zero mismatches). The incremental-alloc
    /// path still absorbs any residual CPU-conservative verdict as over-allocation, never a hole.
    pub(crate) fn dispatch_pending_gpu_classifies(&mut self) {
        let queued = std::mem::take(&mut self.cache.body_vol_cache.pending_gpu_classify);
        if queued.is_empty() { return; }
        // Resolve device + pipeline; on any miss, every record falls back to the CPU worker.
        let pipeline_ready = match &mut self.gpu {
            Some(gpu) => {
                crate::gpu_init::ensure_classify_bricks_pipeline(gpu);
                gpu.pipelines.classify_bricks_pipeline.is_some()
            }
            None => false,
        };
        if !pipeline_ready {
            for q in queued {
                self.cache.body_vol_cache.spawn_classify_worker(q.request, q.local_prims, q.field);
            }
            return;
        }
        let gpu = self.gpu.as_mut().expect("pipeline_ready implies a device");
        for q in queued {
            // The LAB mesh-operand lift matrix — SAME source as the bake dispatch (the classify
            // must sample MeshVolume operands exactly as the mode-2 bake will).
            let body_l2w = self.scene.scene_graph
                .world_transform(q.request.body_nid).to_cols_array_2d();
            let pending = encode_gpu_classify_dispatch(
                &gpu.device, &gpu.queue,
                gpu.pipelines.classify_bricks_pipeline.as_ref().unwrap(),
                gpu.bind_groups.primary_bake_bgl.as_ref().unwrap(),
                &gpu.bake_tex_dummy, q.request, body_l2w,
                // STAGE-3: dilate+alloc on-device (compiled as a set with the classify —
                // present whenever classify_bricks_pipeline is; None only if that set is
                // somehow absent, which degrades to the CPU worker-finalize path).
                gpu.pipelines.classify_finalize_pipelines.as_ref(),
            );
            gpu.classify_gpu_pending.push(pending);
        }
    }

    /// STAGE-2/3 GPU CLASSIFY — drain settled classify readbacks (capped at one per frame,
    /// Frame Budget rule).
    ///
    /// GPU-FINALIZED flights (`gpu_finalized`, the production path): the staging holds the
    /// FINAL index (dilate+alloc ran on-device) — the frame thread validates the structural
    /// invariant (dense-packed handles in scan order — a linear u32 scan, same cost as the
    /// verdict decode it replaces) and lands the request directly through `classify_tx` (the
    /// ONE landing point; `poll_async_classifies` validates generation + regime). The ~20ms
    /// (512³) / ~88ms (1024³) worker-thread `sparse_index_from_verdicts` finalize disappears
    /// on this path.
    ///
    /// VERDICT flights (no finalize pipelines — degraded set): decode/validate verdicts on the
    /// frame thread, then the SHARED CPU finalize (`sparse_index_from_verdicts` — the parity
    /// oracle) on a worker thread, same landing. Map errors / corrupt buffers abort the flight
    /// and re-dirty the body (`abort_async_classify`).
    pub(crate) fn poll_gpu_classify_readbacks(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else { return; };
        if gpu.classify_gpu_pending.is_empty() { return; }
        gpu.device.poll(wgpu::PollType::Poll).expect("GPU device polling failed");
        const MAX_CLASSIFY_FINALIZE_PER_FRAME: usize = 1;
        let mut finalized = 0usize;
        let mut i = 0usize;
        while i < gpu.classify_gpu_pending.len() {
            let state = gpu.classify_gpu_pending[i].map_ready.peek();
            if state == crate::map_state::PENDING
                || (state == crate::map_state::OK && finalized >= MAX_CLASSIFY_FINALIZE_PER_FRAME)
            {
                i += 1;
                continue;
            }
            let p = gpu.classify_gpu_pending.swap_remove(i);
            if state != crate::map_state::OK {
                eprintln!("[GpuClassify] body={:?} verdict readback map FAILED — aborting flight, re-dirtying",
                    p.request.body_nid);
                self.cache.body_vol_cache.abort_async_classify(p.request.body_nid);
                continue;
            }
            let verdict_ms = p.submitted.elapsed().as_secs_f64() * 1000.0;
            if p.gpu_finalized {
                // STAGE-3: staging = final index + trailing allocated count. 2048-SHIP item 2
                // (2026-07-17): the structural validate (`decode_gpu_finalized_index`) is a
                // linear walk over EVERY index entry — 5.8ms at 2.1M bricks (1024³) but ~45ms
                // at 16.7M (2048³), plus a 64MB mapped-range clone: [SLOWPOLL] class on the
                // frame thread. Both move to a worker (the verdict-arm precedent below): the
                // mapped staging buffer travels to the thread (wgpu::Buffer is Send+Sync; the
                // map completed — state OK — before we got here), which clones + unmaps +
                // validates + lands through classify_tx (the ONE landing point;
                // `poll_async_classifies` still validates generation + regime). The validation
                // itself is UNCHANGED — it catches real corruption (the lab-cache
                // binding-collision lesson) — only its thread moved. A corrupt index no longer
                // aborts synchronously: the worker logs loudly and never lands, and the 10s
                // bake timeout re-dirties the body (the P1-#7 dead-worker soundness contract,
                // same as a dead verdict worker).
                let tx = self.cache.body_vol_cache.classify_sender();
                let mut request = p.request;
                let dims = p.dims_bricks;
                let total_bricks = p.total_bricks;
                let staging = p.staging;
                std::thread::spawn(move || {
                    let t_val = std::time::Instant::now();
                    let raw: Vec<u32> = {
                        let view = staging.slice(..).get_mapped_range();
                        bytemuck::cast_slice(&view).to_vec()
                    };
                    staging.unmap();
                    debug_assert_eq!(raw.len(), total_bricks + 1);
                    let Some(si) = decode_gpu_finalized_index(&raw, dims) else {
                        eprintln!("[GpuClassify] body={:?} corrupt GPU-finalized index — flight \
                                   dropped (bake timeout re-dirties)", request.body_nid);
                        return;
                    };
                    let val_ms = t_val.elapsed().as_secs_f64() * 1000.0;
                    eprintln!("[GpuClassify] body={:?} verdicts+dilate+alloc(gpu)+readback={verdict_ms:.1}ms \
                               clone+validate(worker)={val_ms:.1}ms bricks={}/{}",
                        request.body_nid, si.allocated, si.index.len());
                    request.sparse = Some(crate::body_volume_cache::SparseBakeRequest::Full(si));
                    let _ = tx.send(request); // receiver dropped only on cache teardown
                });
                finalized += 1;
                continue;
            }
            let raw: Vec<u32> = {
                let view = p.staging.slice(..).get_mapped_range();
                bytemuck::cast_slice(&view).to_vec()
            };
            p.staging.unmap();
            debug_assert_eq!(raw.len(), p.total_bricks);
            let Some(verdicts) = decode_gpu_classify_verdicts(&raw) else {
                // An out-of-range verdict means the buffer holds garbage (device fault /
                // unwritten region) — none of it is trustworthy. Abort the whole flight.
                eprintln!("[GpuClassify] body={:?} corrupt verdict buffer — aborting flight, re-dirtying",
                    p.request.body_nid);
                self.cache.body_vol_cache.abort_async_classify(p.request.body_nid);
                continue;
            };
            // Finalize on a worker: ~14ms at 512³ is over the 12ms frame budget. Same
            // send-through-classify_tx contract as the CPU classify worker; a dead worker
            // never lands and the 10s bake timeout re-dirties (P1-#7 soundness).
            let tx = self.cache.body_vol_cache.classify_sender();
            let mut request = p.request;
            let dims = p.dims_bricks;
            std::thread::spawn(move || {
                let t_fin = std::time::Instant::now();
                let si = core_math::chunked_dc::sparse_index_from_verdicts(&verdicts, dims);
                let fin_ms = t_fin.elapsed().as_secs_f64() * 1000.0;
                // The GPU-path visibility line — replaces the CPU worker's [SparseFull] print.
                eprintln!("[GpuClassify] body={:?} verdicts={verdict_ms:.1}ms finalize={fin_ms:.1}ms bricks={}/{}",
                    request.body_nid, si.allocated, si.index.len());
                request.sparse = Some(crate::body_volume_cache::SparseBakeRequest::Full(si));
                let _ = tx.send(request); // receiver dropped only on cache teardown
            });
            finalized += 1;
        }
    }

}

/// STAGE-2 GPU CLASSIFY — the ONE BakeParams filler for a `classify_bricks` dispatch (spike
/// convention, volume_bake.wgsl): the relabeled spare pads carry `brick_dim` (_pad0),
/// `classify_band` (_pad1) and the CPU-computed non-Lipschitz `classify_margin`
/// (_pad3, bit-passed via `to_bits` so thresholds are BIT-IDENTICAL to `BrickClassifier`'s).
/// Domain is the classifier's UNPADDED [0,res)³ — no atlas pad/offset ever applies.
pub(crate) fn classify_bake_params(
    req: &crate::body_volume_cache::PrimaryBakeRequest,
    dims_bricks: [u32; 3],
    body_local_to_world: [[f32; 4]; 4],
) -> crate::gpu_jobs::BakeParams {
    let res = req.resolution[0]; // cubic — sparse_eligible enforces it at trigger
    // Band + margin from the single-source formulas the CPU classifier uses (REUSE rule):
    // `sparse_classify_band` (shared with the worker / mini-classify / frozen-partial walk)
    // and `core_math::chunked_dc::classifier_margin` (the exact BrickClassifier expression).
    let band = crate::body_volume_cache::bake_dispatch::sparse_classify_band(
        req.bounds_min, req.bounds_max, res);
    let margin = core_math::chunked_dc::classifier_margin(
        req.bounds_min, req.bounds_max, res, crate::body_volume_cache::bake_dispatch::SPARSE_BRICK_DIM);
    crate::gpu_jobs::BakeParams {
        grid_res: [res; 3],
        _pad0: crate::body_volume_cache::bake_dispatch::SPARSE_BRICK_DIM, // WGSL: brick_dim
        bounds_min: req.bounds_min.into(),
        _pad1: band,                                                      // WGSL: classify_band
        bounds_max: req.bounds_max.into(),
        prim_count: req.prim_floats.len() as u32 / graphics::HOT_FLOATS_PER_PRIM as u32,
        floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
        dims_bricks,
        bake_min: [0; 3],
        _pad3: margin.to_bits(),                                          // WGSL: classify_margin
        bake_max: [res; 3],
        // O1: coarse-cull gate (WGSL: coarse_cull). 1 only when the request's prim snapshot is
        // provably 1-Lipschitz (coarse_cull_safe) — the classify may then skip whole 4³-brick
        // super-cells from one center eval. 0 = the pre-O1 9-eval-per-brick behavior, verbatim.
        _pad4: if req.coarse_cull_ok { 1 } else { 0 },
        mirror_flags: req.mirror_flags,
        mirror_center: req.mirror_center.into(),
        bevel_radius: req.bevel_radius,
        mirror_sign: req.mirror_sign,
        atlas_offset: [0; 3],
        atlas_pad: 0,
        output_mode: 2, // informational — classify_bricks never branches on it
        _pad5: 0,
        body_local_to_world,
    }
}

/// STAGE-2 GPU CLASSIFY — encode ONE `classify_bricks` dispatch + verdict readback for a queued
/// sparse FULL request. Shared verbatim by production (`dispatch_pending_gpu_classifies`) and
/// the hardware e2e test (`tests_gpu_classify::gpu_classify_production_flow_*`) so the test
/// exercises the REAL dispatch path. Bindings mirror a production mode-2 bake's dummy wiring
/// wherever the classify reads nothing: 1³ dummy texture at binding 3, the request's own
/// mesh_grid buffer doubling as the binding-6 filler (classify never reads brick_index — the
/// verdicts go OUT through binding 2, which is why the read-only binding 6 cannot carry them).
/// Split `groups` linear workgroups into a (x, y) dispatch grid where x ≤ `max_per_dim`.
/// y == 1 (the flat index degenerates to wg.x — bit-identical to the old 1D dispatch) until
/// `groups` exceeds the per-dimension limit; then x = max_per_dim rows of y. Searched for
/// existing: none (the bake's own TDR slicing splits by Z-slab volume, a different concern).
pub(crate) fn classify_finalize_grid(groups: u32, max_per_dim: u32) -> (u32, u32) {
    let groups = groups.max(1);
    if groups <= max_per_dim { (groups, 1) } else { (max_per_dim, (groups + max_per_dim - 1) / max_per_dim) }
}

pub(crate) fn encode_gpu_classify_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    bake_tex_dummy: &wgpu::Texture,
    request: crate::body_volume_cache::PrimaryBakeRequest,
    body_local_to_world: [[f32; 4]; 4],
    // STAGE-3 GPU FINALIZE: `Some` appends the dilate/scan/emit passes so the readback is the
    // FINAL index (+ allocated count) instead of raw verdicts. `None` = verdict readback,
    // finalized on the CPU (worker fallback + parity oracle).
    finalize: Option<&crate::gpu_ctx::ClassifyFinalizePipelines>,
) -> crate::PendingGpuClassify {
    let res = request.resolution[0];
    let bpa = core_math::chunked_dc::bricks_per_axis(
        res, crate::body_volume_cache::bake_dispatch::SPARSE_BRICK_DIM);
    let dims = [bpa, bpa, bpa];
    let total = (dims[0] * dims[1] * dims[2]) as usize;

    // Per-dispatch buffers. A classify never runs concurrently with a bake INSIDE one bind
    // group — but the shared primary_bake_params uniform is deliberately NOT reused: the bake
    // drain later this same poll rewrites it, and a dedicated 240-byte uniform per (rare,
    // structural-edit-only) classify is cheaper than reasoning about write/submit interleaving.
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_verdicts"), size: (total * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let prim_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_prims"), size: (request.prim_floats.len() as u64 * 4).max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    queue.write_buffer(&prim_buf, 0, bytemuck::cast_slice(&request.prim_floats));
    let clip_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_clips"), size: (request.clip_curve_floats.len() as u64 * 4).max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    if !request.clip_curve_floats.is_empty() {
        queue.write_buffer(&clip_buf, 0, bytemuck::cast_slice(&request.clip_curve_floats));
    }
    // The scope loop samples mesh_grid regardless of output mode — bind the request's REAL
    // MeshVolume operand data (min 1 float by construction), exactly like the bake dispatch.
    let mesh_grid_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_mesh_grids"),
        size: (request.mesh_grid_floats.len() as u64 * 4).max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    queue.write_buffer(&mesh_grid_buf, 0, bytemuck::cast_slice(&request.mesh_grid_floats));
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_params"),
        size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let params = classify_bake_params(&request, dims, body_local_to_world);
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));
    let tex_view = bake_tex_dummy.create_view(&wgpu::TextureViewDescriptor::default());

    // T1 height atlas — classify never samples displace height maps; dummy fills binding 8
    // so the shared bake BGL (0..8) validates. Real maps ride the primary bake path.
    let height_dummy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_height_dummy"),
        size: 4,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu_classify_bg"), layout: bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_grid_buf.as_entire_binding() },
            // binding(6): read-only brick_index — unread by classify_bricks; mesh_grid doubles
            // as the filler (the dense-bake precedent for re-binding one buffer at two slots).
            wgpu::BindGroupEntry { binding: 6, resource: mesh_grid_buf.as_entire_binding() },
            // binding(7): curv_positions — read ONLY by `curvature_eval`; mesh_grid fills it.
            wgpu::BindGroupEntry { binding: 7, resource: mesh_grid_buf.as_entire_binding() },
            // binding(8): T1 height_maps — unread by classify; dummy fills layout.
            wgpu::BindGroupEntry { binding: 8, resource: height_dummy.as_entire_binding() },
        ],
    });

    // Staging: verdicts (total u32) for the CPU-finalize path; final index + trailing
    // `allocated` (total+1 u32) for the GPU-finalize path — same readback size class either way.
    let staging_u32s = if finalize.is_some() { total + 1 } else { total };
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu_classify_staging"), size: (staging_u32s * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu_classify_encoder"),
    });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu_classify_pass"), timestamp_writes: None,
        });
        cp.set_pipeline(pipeline);
        cp.set_bind_group(0, &bg, &[]);
        // One thread per brick, workgroup (4,4,4). 64³ bricks at 512³ = 4096 workgroups × 9
        // evals — a fraction of a bake slice's work, no TDR slicing needed.
        cp.dispatch_workgroups((dims[0] + 3) / 4, (dims[1] + 3) / 4, (dims[2] + 3) / 4);
    }
    if let Some(fin) = finalize {
        // STAGE-3: verdicts→index on-device (shader layout comment: mask | prefix | block
        // sums | index | allocated). Work buffer is transient like out_buf — created, used in
        // this ONE submission, dropped with the flight (same classify staging lifetime).
        let nb = (total + 255) / 256;
        let work_u32s = 3 * total + nb + 1;
        let work_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_classify_finalize_work"), size: (work_u32s * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // Finalize bind group: verdicts (out_buf) at binding 6 (brick_index, read-only u32),
        // work buffer at binding 2 (output_grid, read_write) — same BGL, dummies unchanged.
        let fin_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_classify_finalize_bg"), layout: bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: work_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
                wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: mesh_grid_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: out_buf.as_entire_binding() },
                // binding(7): curv_positions — unread by the finalize chain; mesh_grid fills it.
                wgpu::BindGroupEntry { binding: 7, resource: mesh_grid_buf.as_entire_binding() },
                // binding(8): T1 height_maps — unread by finalize; dummy fills layout.
                wgpu::BindGroupEntry { binding: 8, resource: height_dummy.as_entire_binding() },
            ],
        });
        // 2D DISPATCH GRID (2026-07-17): at 2048³ the brick index is 256³ = 16.7M entries →
        // 65536 linear workgroups — ONE over max_compute_workgroups_per_dimension (65535),
        // which discarded the whole classify submit as a validation error (the spike's first
        // hard break). Split into (x ≤ limit, y rows); the shaders flatten wg.x + wg.y * nwg.x
        // (bit-identical to 1D for y == 1 — every ≤1536³ dispatch, proven by the classify
        // parity tests). Tail workgroups in the last row early-out on `bi >= total`.
        let linear_groups = ((total + 255) / 256) as u32;
        let (gx, gy) = classify_finalize_grid(
            linear_groups, device.limits().max_compute_workgroups_per_dimension);
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gpu_classify_finalize_pass"), timestamp_writes: None,
        });
        cp.set_bind_group(0, &fin_bg, &[]);
        cp.set_pipeline(&fin.dilate);
        cp.dispatch_workgroups(gx, gy, 1);
        cp.set_pipeline(&fin.scan);
        cp.dispatch_workgroups(gx, gy, 1);
        cp.set_pipeline(&fin.block_scan);
        cp.dispatch_workgroups(1, 1, 1);
        cp.set_pipeline(&fin.emit);
        cp.dispatch_workgroups(gx, gy, 1);
        drop(cp);
        // Read back the index region + the trailing allocated slot in one contiguous copy.
        let index_off = ((2 * total + nb) * 4) as u64;
        enc.copy_buffer_to_buffer(&work_buf, index_off, &staging, 0, ((total + 1) * 4) as u64);
    } else {
        enc.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, (total * 4) as u64);
    }
    queue.submit(std::iter::once(enc.finish()));

    let map_ready = std::sync::Arc::new(crate::map_state::MapState::new());
    let map_ready_cb = map_ready.clone();
    // GPU Safety rule: capture the map_async Result — MapState encodes Err as ERR.
    staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        map_ready_cb.store_result(&result);
    });

    crate::PendingGpuClassify {
        request, staging, map_ready,
        total_bricks: total,
        dims_bricks: dims,
        submitted: std::time::Instant::now(),
        gpu_finalized: finalize.is_some(),
    }
}

/// STAGE-3 GPU FINALIZE — decode + VALIDATE a GPU-finalized index readback: `raw` =
/// `total` per-brick tags + 1 trailing `allocated` count (the shader's emit layout). The
/// validation enforces the emit invariant STRUCTURALLY: handles must be EXACTLY 0,1,2,… in
/// brick-linear scan order (dense packing — the same order `emit_sparse_index` produces), and
/// the trailing count must equal the number of handles. Any violation means the buffer holds
/// garbage (device fault / unwritten region) — `None`, caller aborts the flight.
pub(crate) fn decode_gpu_finalized_index(
    raw: &[u32],
    dims: [u32; 3],
) -> Option<core_math::chunked_dc::SparseIndexOnly> {
    use core_math::chunked_dc::{BRICK_TAG_EXTERIOR, BRICK_TAG_INTERIOR};
    let total = (dims[0] * dims[1] * dims[2]) as usize;
    if raw.len() != total + 1 { return None; }
    let allocated = raw[total] as usize;
    if allocated > total { return None; }
    let mut running = 0u32;
    for &tag in &raw[..total] {
        if tag == BRICK_TAG_EXTERIOR || tag == BRICK_TAG_INTERIOR { continue; }
        if tag != running { return None; } // non-dense / out-of-order handle → garbage
        running += 1;
    }
    if running as usize != allocated { return None; }
    Some(core_math::chunked_dc::SparseIndexOnly {
        index: raw[..total].to_vec(),
        dims_bricks: dims,
        allocated,
        // FULL classifies only: a fresh index recycles nothing (freed-list recycling belongs
        // to the PARTIAL walk, which stays CPU) — same contract as sparse_index_from_flags.
        freed: Vec::new(),
    })
}

/// STAGE-2 GPU CLASSIFY — decode + VALIDATE raw shader verdicts (0=EXTERIOR, 1=INTERIOR,
/// 2=SURFACE). `None` = an out-of-range verdict (the buffer holds garbage — device fault or
/// unwritten region); the caller aborts the flight rather than trusting any of it.
pub(crate) fn decode_gpu_classify_verdicts(
    raw: &[u32],
) -> Option<Vec<core_math::chunked_dc::BrickClass>> {
    use core_math::chunked_dc::BrickClass;
    let mut verdicts = Vec::with_capacity(raw.len());
    for &v in raw {
        verdicts.push(match v {
            0 => BrickClass::Exterior,
            1 => BrickClass::Interior,
            2 => BrickClass::Surface,
            _ => return None,
        });
    }
    Some(verdicts)
}

/// STAGE-2 GPU CLASSIFY — decode + shared dilation/allocation finalize in one call (the worker
/// thread splits them in production so only the decode runs on the frame thread). Shared with
/// the hardware e2e test.
pub(crate) fn finalize_gpu_classify_verdicts(
    raw: &[u32],
    dims: [u32; 3],
) -> Option<core_math::chunked_dc::SparseIndexOnly> {
    decode_gpu_classify_verdicts(raw)
        .map(|v| core_math::chunked_dc::sparse_index_from_verdicts(&v, dims))
}
