// core_engine/app/src/app_poll_gpu.rs
//
// Slice (app_poll de-god, PURE RELOCATION) — byte-for-byte method bodies moved out of app_poll.rs;
// no logic change. poll_background_tasks (orchestrator) stays in app_poll.rs and calls these.

use crate::*;
use glam::Vec3;
use eframe::wgpu;

impl AzraApp {
    /// Clipmap-track cids: explicit `block_cid` plus legacy name match (once per call).
    /// Callers must not re-scan `all_nodes` per cid.
    pub(crate) fn cluster_blocked_cids(
        &self,
    ) -> std::collections::HashSet<core_math::scene_graph::MeshCacheId> {
        let mut blocked = self.cache.cluster_bake.blocked_cids.clone();
        for (_, node) in self.scene.scene_graph.all_nodes() {
            if let core_math::scene_graph::NodePayload::PolygonalBody {
                name,
                mesh_cache_id: Some(cid),
                ..
            } = &node.payload
            {
                if crate::cluster_bake::poly_name_blocks_cluster(name) {
                    blocked.insert(*cid);
                }
            }
        }
        blocked
    }

    /// Phase D1: create/grow the GPU instance-cull system (POLL phase — Frame Ordering
    /// Contract; buffer mutation never happens during dispatch). Sized from the structural
    /// draw stream; over-provisioned by next_power_of_two inside ensure_capacity, so steady
    /// scenes hit the early-return. Disabled entirely by `AZRA_NO_GPU_CULL=1` (CPU cull path).
    pub(crate) fn poll_instance_cull(&mut self) {
        if std::env::var("AZRA_NO_GPU_CULL").is_ok_and(|v| v == "1") {
            return;
        }
        // structural_draw_calls is the real instance count (a multi-material poly body spans
        // several draws); draw_stream is the NodeId floor for the first frame before gather.
        let n_instances = self.structural_draw_calls.len().max(self.draw_stream.len());
        if n_instances == 0 {
            return;
        }
        // Batches ≤ instances; sizing by instance count is the safe upper bound without
        // re-deriving the batch partition here (it's computed at dispatch from draw_calls).
        let n_batches = n_instances;
        if let Some(g) = self.gpu.as_mut() {
            match g.raster_cull.as_mut() {
                None => {
                    let sys = render::instance_cull::InstanceCullSystem::new(
                        &g.device,
                        &graphics::bgl::instance_cull_bgl_entries(),
                        graphics::INSTANCE_CULL_WGSL,
                        n_instances.next_power_of_two().max(64),
                        n_batches.next_power_of_two().max(64),
                    );
                    eprintln!("[InstCull] created (cap={})", sys.instance_capacity);
                    g.raster_cull = Some(sys);
                }
                Some(sys) => sys.ensure_capacity(&g.device, n_instances, n_batches),
            }
        }
    }

    /// Phase D2: create/resize the HZB pyramid (POLL phase — Frame Ordering Contract),
    /// keyed on the depth attachment size. `AZRA_NO_HIZ=1` disables entirely.
    pub(crate) fn poll_hzb(&mut self) {
        if std::env::var_os("AZRA_NO_HIZ").is_some()
            || std::env::var("AZRA_NO_GPU_CULL").is_ok_and(|v| v == "1")
        {
            return;
        }
        if let Some(g) = self.gpu.as_mut() {
            let (w, h) = g.raster.depth_size;
            if w == 0 || h == 0 {
                return;
            }
            match g.hzb.as_mut() {
                None => {
                    let sys = render::hzb::HzbSystem::new(
                        &g.device,
                        &graphics::bgl::hzb_seed_bgl_entries(),
                        &graphics::bgl::hzb_down_bgl_entries(),
                        graphics::HZB_BUILD_WGSL,
                        w,
                        h,
                    );
                    eprintln!("[HZB] created ({}² × {} mips for {w}x{h})", sys.size, sys.mip_count);
                    g.hzb = Some(sys);
                }
                Some(sys) => {
                    sys.ensure_size(&g.device, w, h);
                }
            }
        }
    }

    /// Phase D3: cluster-DAG background bake — trigger for dense poly meshes +
    /// drain finished DAGs (≤1/frame). Poll phase; never the frame thread's work
    /// (builds run on their own threads). `AZRA_NO_CLUSTER=1` disables.
    pub(crate) fn poll_cluster_bake(&mut self) {
        if std::env::var("AZRA_NO_CLUSTER").is_ok_and(|v| v == "1") {
            return;
        }
        // WP7 two-track: blocked cids (explicit block_cid + legacy name fallback).
        // Built once per poll — not once per cid (Frame Budget).
        let cluster_blocked = self.cluster_blocked_cids();

        // Trigger: dense display meshes (MESH **or** CLUSTERED). After D4 remap,
        // structural kinds stay CLUSTERED — skipping all "special" kinds froze
        // re-bake forever (WP2). SDF/voxel kinds are still excluded by allowlist.
        //
        // Also seed from scene poly bodies that already have mesh_cache_id — dense
        // stress lands the cache on drain before the next structural rebuild; baking
        // only from structural_draw_calls delayed 1x9 until after raster rebuild.
        let mut candidates: Vec<(core_math::scene_graph::MeshCacheId, u64)> = Vec::new();
        let thr = crate::cluster_bake::CLUSTER_TRI_THRESHOLD;
        let mut consider_cid = |cid: core_math::scene_graph::MeshCacheId| {
            if cluster_blocked.contains(&cid) {
                return;
            }
            if let (Some(stamp), Some((_, idx))) = (
                self.cache.mesh_cache.pt_stamp(cid),
                self.cache.mesh_cache.chunk_mesh_data(cid),
            ) {
                if idx.len() / 3 >= thr {
                    candidates.push((cid, stamp));
                }
            }
        };
        for (i, (cid, _, _, _)) in self.structural_draw_calls.iter().enumerate() {
            let k = self.structural_draw_kind[i];
            if k != crate::raster_common::DRAW_KIND_MESH
                && k != crate::raster_common::DRAW_KIND_CLUSTERED
            {
                continue;
            }
            consider_cid(*cid);
        }
        for (_, node) in self.scene.scene_graph.all_nodes() {
            if let core_math::scene_graph::NodePayload::PolygonalBody {
                mesh_cache_id: Some(cid),
                ..
            } = &node.payload
            {
                consider_cid(*cid);
            }
        }
        // Dedupe shared meshes (B.4 instancing: many bodies, one cid).
        candidates.sort_unstable_by_key(|&(c, _)| c.0);
        candidates.dedup_by_key(|&mut (c, _)| c.0);

        // Drop any DAG already baked for a blocked cid (e.g. terrain baked before WP7).
        if !cluster_blocked.is_empty() {
            let n_before = self.cache.cluster_bake.dags.len();
            self.cache
                .cluster_bake
                .dags
                .retain(|cid, _| !cluster_blocked.contains(cid));
            if self.cache.cluster_bake.dags.len() != n_before {
                self.structural_draw_stale = true;
            }
        }

        // Clusters on but nothing qualifies: one delayed warn so demo landscape
        // (WP7 terrain off path + sub-threshold props) is not mistaken for a D4 test.
        // Delay ~3s so AZRA_DENSE_STRESS display-sync can land before we cry idle.
        {
            use std::cell::Cell;
            use std::time::Instant;
            thread_local! {
                static IDLE_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
                static IDLE_WARNED: Cell<bool> = const { Cell::new(false) };
            }
            // Pending display sync ⇒ not truly idle (1x9 mesh not in cache yet).
            let pending_sync = !self.poly_display_dirty.is_empty();
            let idle = self.render.dbg_clusters
                && candidates.is_empty()
                && self.cache.cluster_bake.dags.is_empty()
                && !pending_sync;
            if !idle {
                IDLE_SINCE.with(|c| c.set(None));
            } else {
                IDLE_SINCE.with(|since| {
                    let now = Instant::now();
                    if since.get().is_none() {
                        since.set(Some(now));
                    }
                    let aged = since
                        .get()
                        .map(|t| now.duration_since(t).as_secs_f32() >= 3.0)
                        .unwrap_or(false);
                    if aged && !IDLE_WARNED.get() {
                        IDLE_WARNED.set(true);
                        let mut max_tris = 0usize;
                        let mut n_mesh_draws = 0usize;
                        let mut n_no_cache = 0usize;
                        for (i, (cid, _, _, _)) in self.structural_draw_calls.iter().enumerate() {
                            let k = self.structural_draw_kind[i];
                            if k != crate::raster_common::DRAW_KIND_MESH
                                && k != crate::raster_common::DRAW_KIND_CLUSTERED
                            {
                                continue;
                            }
                            n_mesh_draws += 1;
                            match self.cache.mesh_cache.chunk_mesh_data(*cid) {
                                Some((_, idx)) => max_tris = max_tris.max(idx.len() / 3),
                                None => n_no_cache += 1,
                            }
                        }
                        eprintln!(
                            "[ClusterBake] IDLE: clusters ON but 0 candidates / 0 DAGs \
                             (threshold={} tris, max_draw_tris={}, blocked_cids={}, \
                             mesh_draws={}, no_mesh_cache={}). \
                             After WP7 the demo landscape does NOT exercise D4 \
                             (Terrain blocked; props << threshold). \
                             Real D4 scene: AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1 \
                             (~3.1M tris / ~90k clusters). Wait for [ClusterGpu] resident \
                             (bake of 3.1M can take tens of seconds).",
                            crate::cluster_bake::CLUSTER_TRI_THRESHOLD,
                            max_tris,
                            cluster_blocked.len(),
                            n_mesh_draws,
                            n_no_cache,
                        );
                    }
                });
            }
        }

        // One-shot diagnostic: why is nothing clustering? (env AZRA_CLUSTER_LOG=1)
        if std::env::var("AZRA_CLUSTER_LOG").is_ok_and(|v| v == "1") {
            use std::cell::Cell;
            thread_local! { static LAST: Cell<Option<std::time::Instant>> = Cell::new(None); }
            let due = LAST.with(|c| {
                let now = std::time::Instant::now();
                let due = c.get().map_or(true, |t| now.duration_since(t).as_secs_f32() >= 5.0);
                if due { c.set(Some(now)); }
                due
            });
            let n_poly = self
                .scene
                .scene_graph
                .all_nodes()
                .filter(|(_, n)| matches!(n.payload, core_math::scene_graph::NodePayload::PolygonalBody { .. }))
                .count();
            if !candidates.is_empty() || n_poly > 0 {
                let mut detail = String::new();
                for (_, node) in self.scene.scene_graph.all_nodes() {
                    if let core_math::scene_graph::NodePayload::PolygonalBody {
                        mesh_cache_id, ..
                    } = &node.payload
                    {
                        use std::fmt::Write as _;
                        let cid = mesh_cache_id;
                        let stamp = cid.and_then(|c| self.cache.mesh_cache.pt_stamp(c));
                        let data = cid.and_then(|c| self.cache.mesh_cache.chunk_mesh_data(c).map(|(_, i)| i.len() / 3));
                        let _ = write!(detail, " [cid={cid:?} stamp={stamp:?} tris={data:?}]");
                    }
                }
                if due {
                    eprintln!(
                        "[ClusterBake] diag: poly_bodies={n_poly} candidates={} (threshold {}) {detail}",
                        candidates.len(),
                        crate::cluster_bake::CLUSTER_TRI_THRESHOLD
                    );
                }
            }
        }
        // Disjoint field borrows: bake state &mut, mesh cache & (same struct).
        let cache = &mut self.cache;
        let mesh_cache = &cache.mesh_cache;
        let cluster_bake = &mut cache.cluster_bake;
        for &(cid, stamp) in &candidates {
            cluster_bake.consider(cid, stamp, || {
                let (verts, idx) = mesh_cache.chunk_mesh_data(cid)?;
                let positions: Vec<[f32; 3]> = verts.iter().map(|v| [v[0], v[1], v[2]]).collect();
                let normals: Vec<[f32; 3]> = verts.iter().map(|v| [v[3], v[4], v[5]]).collect();
                Some((positions, normals, idx.clone()))
            });
        }
        cluster_bake.drain(|cid| mesh_cache.pt_stamp(cid));

        // Prune DAGs that are not mesh-fresh (edit / delete). Mesh stamp is SoT (WP3).
        let dags_changed =
            cluster_bake.retain_live(|cid| mesh_cache.pt_stamp(cid));

        // D4 residency: mirror finished DAGs into GPU buffers — poll phase only,
        // ≤1 upload/frame (a DAG upload can be ~100MB — accepted load hitch, D7
        // owns chunking). Freshness = **mesh** stamp (WP3), not "whatever dags says".
        let mut pool_gen_changed = dags_changed;
        let clusters_draw = self.render.dbg_clusters;
        if let Some(g) = self.gpu.as_mut() {
            let gen_before = g.cluster_pool.generation;
            // Keep pool entry only when mesh stamp == entry stamp AND a matching DAG exists.
            // Also drop two-track blocked cids (Terrain) if they were uploaded earlier.
            g.cluster_pool.retain_fresh(|cid| {
                if cluster_blocked.contains(&cid) {
                    return None;
                }
                let mesh_s = mesh_cache.pt_stamp(cid)?;
                let dag_s = cluster_bake.dags.get(&cid).map(|(s, _)| *s)?;
                if mesh_s == dag_s {
                    Some(mesh_s)
                } else {
                    None
                }
            });
            // Upload only when the draw path can use clusters (WP5 policy A: CPU bake
            // may warm; GPU residency follows the feature flag).
            // WP14: debug palette streams only when cluster debug view is on.
            let want_debug = self.render.dbg_cluster_view;
            if clusters_draw {
                let pending = cluster_bake.dags.iter().find(|(cid, (stamp, _))| {
                    mesh_cache.pt_stamp(**cid) == Some(*stamp)
                        && g.cluster_pool.entries.get(cid).map(|e| e.stamp) != Some(*stamp)
                });
                if let Some((&cid, &(stamp, ref dag))) = pending {
                    let t0 = std::time::Instant::now();
                    g.cluster_pool
                        .upload(&g.device, cid, dag, stamp, want_debug);
                    eprintln!(
                        "[ClusterGpu] resident {cid:?}: {} clusters, {} verts, {} idx in {:.0}ms (debug={want_debug})",
                        g.cluster_pool.entries[&cid].n_clusters,
                        dag.positions.len(),
                        dag.indices.len(),
                        t0.elapsed().as_secs_f32() * 1000.0
                    );
                } else if want_debug {
                    // Debug view on: fill/rebuild every stream that is missing or on
                    // a stale layout version (index-aligned verts fix).
                    let need_dbg: Vec<_> = g
                        .cluster_pool
                        .entries
                        .iter()
                        .filter(|(_, e)| {
                            !e.has_debug_stream
                                || e.debug_layout_version
                                    != render::cluster_gpu::DEBUG_STREAM_LAYOUT_VERSION
                        })
                        .map(|(c, _)| *c)
                        .collect();
                    for cid in need_dbg {
                        if let Some((_, dag)) = cluster_bake.dags.get(&cid) {
                            if g.cluster_pool.ensure_debug_stream(&g.device, cid, dag) {
                                eprintln!("[ClusterGpu] debug stream filled for {cid:?}");
                            }
                        }
                    }
                }
            } else if !g.cluster_pool.entries.is_empty() {
                // Feature off: drop GPU residency so cut cannot run on stale pool.
                g.cluster_pool.entries.clear();
                g.cluster_pool.generation = g.cluster_pool.generation.wrapping_add(1);
            }
            // Cut system: created (poll phase) when the first DAG is resident.
            if !g.cluster_pool.entries.is_empty() && g.cluster_cut.is_none() {
                g.cluster_cut = Some(render::cluster_cut::ClusterCutSystem::new(
                    &g.device,
                    &graphics::bgl::cluster_cut_bgl_entries(),
                    graphics::CLUSTER_CUT_WGSL,
                ));
                eprintln!("[ClusterCut] system created");
            }
            pool_gen_changed = pool_gen_changed || g.cluster_pool.generation != gen_before;
        }
        // Residency changed → the draw-kind remap (D4) must re-run → structural rebuild.
        if pool_gen_changed {
            self.structural_draw_stale = true;
        }
    }

    /// Poll per-body volume cache bake results (non-blocking).
    pub(crate) fn poll_body_vol_cache(&mut self) {
        let _spike_t0 = std::time::Instant::now();
        if !self.cache.use_body_vol_cache { return; }

        let t_poll = std::time::Instant::now();
        let baked_bodies = self.cache.body_vol_cache.poll();
        let poll_ms = t_poll.elapsed().as_secs_f64() * 1000.0;

        let baked_count = baked_bodies.len();
        if !baked_bodies.is_empty() {
            self.mark_dirty(DirtyFlags::RENDER);

            let t_per_body = std::time::Instant::now();
            for body_nid in baked_bodies {
                // cut#1 (CPU-bake path): sync the resident buffer to the just-baked grid BEFORE the DC
                // reads it. `poll()` (bake_rx, the rayon CPU bake path used when no worker GPUs exist)
                // delivers the new grid into body_grids and clears resident_buffer_valid, but — unlike
                // poll_primary_bake — never re-uploads it. Without this, the full re-tighten/escape bakes
                // that land here mesh via the 512MB slot.field fallback (the ~400ms spike) instead of
                // field-direct from rv.buffer. Active body, Object Mode only (volume-only doesn't DC-mesh).
                // NESTED SCOPE: bakes are always TOP-LEVEL bodies; when the user is inside a
                // sub-body scope the raw active nid never matches — resolve to the ancestor
                // (active_top_body) or the rv sync silently skips and the viewport shows a
                // frozen stale volume ("sub-body boolean tree not working").
                if body_nid == self.active_top_body() {
                    if let Some(gpu) = &self.gpu {
                        self.cache.body_vol_cache.update_resident_volume(
                            body_nid, &gpu.device, &gpu.queue);
                    }
                }
                let t_dc = std::time::Instant::now();
                self.dispatch_grid_dc_async(body_nid);
                let dc_ms = t_dc.elapsed().as_secs_f64() * 1000.0;

                let t_cascade = std::time::Instant::now();
                self.cache.body_vol_cache.advance_cascade(body_nid, &self.scene.scene_graph);
                let cascade_ms = t_cascade.elapsed().as_secs_f64() * 1000.0;

                let t_brick = std::time::Instant::now();
                let (d, q) = match self.gpu.as_ref() {
                    Some(g) => (Some(&*g.device), Some(&*g.queue)),
                    None => (None, None),
                };
                self.cache.body_vol_cache.classify_bricks_for_body(body_nid, d, q);
                let brick_ms = t_brick.elapsed().as_secs_f64() * 1000.0;

                if dc_ms + cascade_ms + brick_ms > 2.0 {
                    eprintln!("[PollPerBody] body={:?} dc_dispatch={:.1}ms cascade={:.1}ms brick={:.1}ms",
                        body_nid, dc_ms, cascade_ms, brick_ms);
                }
            }
            let per_body_ms = t_per_body.elapsed().as_secs_f64() * 1000.0;
            if per_body_ms > 2.0 {
                eprintln!("[PollBodies] {} bodies processed in {:.1}ms", baked_count, per_body_ms);
            }
        }

        let t_retry = std::time::Instant::now();
        if self.cache.body_vol_cache.has_pending_dirty() {
            self.trigger_body_vol_bake();
        }
        if self.cache.body_vol_cache.check_draft_promotion() {
            self.trigger_body_vol_bake();
        }
        // Settle-time full re-tighten: after a drag/footprint-create/slider edit stops,
        // re-bake interactively partial-baked bodies at tight bounds + full resolution.
        // Fixes surface vanishing on create (escaped grid) and quality degrading while
        // moving — previously only a resolution change (invalidate_all) recovered it.
        if self.cache.body_vol_cache.check_idle_retighten(&self.scene.scene_graph) {
            self.trigger_body_vol_bake();
        }
        let retry_ms = t_retry.elapsed().as_secs_f64() * 1000.0;

        let total_ms = _spike_t0.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 10.0 {
            eprintln!("[SPIKE] poll_body_vol_cache: {:.1}ms (poll={:.1}ms baked={} retry={:.1}ms)",
                total_ms, poll_ms, baked_count, retry_ms);
        }
    }

    /// Dispatch a grid-based Dual Contouring job to a Rayon background thread.
    /// Gathers all needed data (grid, brick data, existing ChunkedMesh) from self,
    /// then spawns the CPU-heavy DC extraction off the main thread.
    /// Previous mesh stays visible in mesh_cache until the new result arrives.
    /// One job per body — skips if a DC job is already in-flight for this body_nid.
    /// Phase G: GPU-mesh a body's baked grid (QEF Dual Contouring) straight into mesh_cache, replacing
    /// the background CPU DC. Synchronous (one submit + readback, ~few ms vs ~125ms CPU). Reuses the
    /// existing raster draw via `update_editable_mesh` (display-only triangles — not persisted on save).
    pub(crate) fn gpu_mesh_to_cache(
        &mut self, cid: core_math::scene_graph::MeshCacheId, grid: &[f32], nverts: u32,
        grid_min: glam::Vec3, grid_max: glam::Vec3,
    ) {
        // Lazily compile the mesher pipelines once (fallback — normally background-warmed).
        if !self.ensure_sdf_mesher() { return; }
        // Coalesce: one in-flight async readback per body. If a job is already pending for this cid,
        // skip — it lands first, then the next bake re-meshes the current grid. Bounds memory + GPU work.
        if self.cache.pending_sdf_mesh.iter().any(|(c, _)| *c == cid) { return; }
        // ASYNC dispatch: kick the GPU mesh + non-blocking readback, store the pending job. NO
        // device.poll(Wait) here — that blocking wait (on a drag-saturated GPU queue) was the per-frame
        // stall. poll_sdf_mesh_jobs reads the result on a later frame and lands it in mesh_cache.
        let pending = match (self.gpu.as_ref(), self.cache.sdf_mesher.as_ref()) {
            (Some(g), Some(m)) => m.dispatch_async(&g.device, &g.queue, grid, nverts, grid_min, grid_max),
            _ => return,
        };
        self.cache.pending_sdf_mesh.push((cid, pending));
    }

    /// Collect completed async GPU-mesh readbacks → mesh_cache. Called once per frame. The map
    /// callbacks are driven by the frame loop's `device.poll(Poll)`, so a job becomes `ready()` 1-2
    /// frames after dispatch — no blocking wait on the main thread.
    pub(crate) fn poll_sdf_mesh_jobs(&mut self) {
        // Frame Budget Rule: cap mesh landings per frame (each is an upload + mesh_cache write).
        // Matches the other poll loops (bake readback ≤1, DC ≤2). Remaining ready jobs land next
        // frame — they're already meshed on the GPU, so the surface just updates one frame later.
        const MAX_SDF_MESH_PER_FRAME: usize = 2;
        let mut processed = 0;
        let mut i = 0;
        while i < self.cache.pending_sdf_mesh.len() {
            if processed >= MAX_SDF_MESH_PER_FRAME { break; }
            if self.cache.pending_sdf_mesh[i].1.ready() {
                let (cid, pending) = self.cache.pending_sdf_mesh.swap_remove(i);
                let (verts, indices) = pending.take();
                if indices.len() >= 3 {
                    // Find body for this cache id (mesh-mod fold / base invalidation).
                    let body_nid = self.scene.scene_graph.all_ids().find(|&nid| {
                        matches!(
                            self.scene.scene_graph.get(nid).map(|n| &n.payload),
                            Some(core_math::scene_graph::NodePayload::SdfBody {
                                mesh_cache_id: Some(c),
                                ..
                            }) if *c == cid
                        )
                    });
                    if let Some(bn) = body_nid {
                        // Fresh field mesh → drop raw base so mesh-mod / Detail LO sync re-reads.
                        self.invalidate_mesh_mod_display(bn);
                        let needs_fold = self.body_needs_display_fold(bn);
                        if needs_fold {
                            // Raw base = pre-Detail-LO / pre-user-stack DC tris.
                            let base_key = self.mesh_mod_base_key(bn);
                            self.cache
                                .mesh_mod_base
                                .insert(bn, (verts.clone(), indices.clone(), base_key));
                            let (fv, fi) = self.fold_body_display_after_dc(bn, verts, indices);
                            let tri_count = (fi.len() / 3) as u32;
                            self.cache.mesh_cache.update_editable_mesh(cid, (fv, fi), tri_count);
                            // Stamp so ensure_body_mesh_mod_displays won't re-materialize same content.
                            let stamp = match self.scene.scene_graph.get(bn).map(|n| &n.payload) {
                                Some(core_math::scene_graph::NodePayload::SdfBody {
                                    mesh_modifiers,
                                    ..
                                }) => self.mesh_mod_display_stamp_for(bn, mesh_modifiers),
                                _ => 0,
                            };
                            self.cache.mesh_mod_display_stamp.insert(bn, stamp);
                            self.wire_local_cache.remove(&cid.0);
                            self.overlay_lines_fp = u64::MAX;
                        } else {
                            let tri_count = (indices.len() / 3) as u32;
                            self.cache.mesh_cache.update_editable_mesh(cid, (verts, indices), tri_count);
                            self.wire_local_cache.remove(&cid.0);
                            self.overlay_lines_fp = u64::MAX;
                        }
                    } else {
                        let tri_count = (indices.len() / 3) as u32;
                        self.cache.mesh_cache.update_editable_mesh(cid, (verts, indices), tri_count);
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                processed += 1;
                // don't advance i — swap_remove moved the last element into slot i
            } else {
                i += 1;
            }
        }
    }

    /// Lazy FALLBACK for the shared SDF DC mesher (`cache.sdf_mesher`): compile it on the frame
    /// thread ONLY if the background warm (`init_gpu_ctx` → `PipelineReady::SdfMesher`) hasn't
    /// landed yet. Returns false when there is no GPU device. ONE implementation for both callers
    /// (gpu_mesh_to_cache + sdf_populate_resident — searched for existing: the two former inline
    /// blocks were the only ones; this replaces them). The eprintln is one-shot per session and
    /// names the stall this used to be (the 562ms first-select [SLOWPOLL] primary column).
    pub(crate) fn ensure_sdf_mesher(&mut self) -> bool {
        if self.cache.sdf_mesher.is_some() { return true; }
        let Some(g) = self.gpu.as_ref() else { return false; };
        let t = std::time::Instant::now();
        let m = crate::sdf_gpu_mesh::SdfMeshGpu::new_with_cache(
            &g.device, (*g.pipelines.pipeline_cache).as_ref());
        eprintln!("[SPIKE] SDF DC mesher compiled LAZILY on the frame thread (background warm not ready yet): {:.1}ms",
            t.elapsed().as_secs_f64() * 1000.0);
        self.cache.sdf_mesher = Some(m);
        true
    }

    /// G2 RESIDENT path: mesh `grid` into the body's persistent GpuCtx slot — NO readback, NO mesh_cache,
    /// NO grid re-upload. raster_render draws `slot.verts`/`idx` via draw_indexed_indirect. Buffer
    /// allocation happens here (poll phase) only — Frame Ordering compliant.
    pub(crate) fn sdf_populate_resident(
        &mut self, body_nid: core_math::scene_graph::NodeId, nverts: u32,
        grid_min: glam::Vec3, grid_max: glam::Vec3,
    ) {
        // [SPIKE] split (permanent, printed only >50ms): attributes a slow populate to its phase —
        // mesher compile (the fixed 562ms first-select stall), slot/pool allocation, the sparse
        // classify pre-pass, or the mesh dispatch itself. Mirrors the [SLOWPOLL] convention.
        let _pop_t0 = std::time::Instant::now();
        // Lazily compile the shared mesher pipelines once (fallback — normally background-warmed).
        let _t_mesher = std::time::Instant::now();
        if !self.ensure_sdf_mesher() { return; }
        let _mesher_ms = _t_mesher.elapsed().as_secs_f64() * 1000.0;
        // Ensure the per-body slot can hold this resolution (create/GROW only here, in poll).
        // GROW-ONLY: a bake at res ≤ capacity reuses the existing buffers with the new LOGICAL res
        // (slot.res) — the resolution cascade (64→512) and Grid-picker oscillation (512↔256↔128)
        // previously freed + reallocated the multi-GB slot on EVERY switch (a large slice of the
        // measured 837/353ms 512³ frames). Buffer tails beyond the logical extent are never read
        // (counters zeroed + indirect rewritten + cell_vert fully rewritten per dispatch).
        let _t_slot = std::time::Instant::now();
        let need_new = match self.gpu.as_ref().and_then(|g| g.sdf_body_mesh.get(&body_nid)) {
            Some(slot) => slot.capacity_res < nverts,
            None => true,
        };
        if let Some(g) = self.gpu.as_mut() {
            if need_new {
                let slot = crate::sdf_gpu_mesh::SdfBodyMeshSlot::new(&g.device, nverts);
                g.sdf_body_mesh.insert(body_nid, slot);
            } else if let Some(slot) = g.sdf_body_mesh.get_mut(&body_nid) {
                slot.res = nverts; // reuse capacity at the new logical res
            }
        }
        let _slot_ms = _t_slot.elapsed().as_secs_f64() * 1000.0;
        // S2.4 brick-resident gate: the body's CURRENT bake is a SparseBricks bake whose content
        // sits bake-fresh in the persistent SdfBrickAtlasGpu (entry marker == atlas body + res +
        // generation). The sparse branch below meshes straight from the atlas; the G3
        // field-direct gate is EXCLUDED (a stale rv.buffer from a previous dense era could still
        // pass its content-identity checks) and the grid-classify pre-pass MUST NOT run (there is
        // no dense grid, and re-uploading a grid-classified index would desync the handles from
        // the bake-written pool content).
        let brick_resident_ok = self.cache.body_vol_cache.brick_resident_gen(body_nid)
            .map_or(false, |bgen| self.gpu.as_ref()
                .and_then(|g| g.sdf_brick_atlases.get(&body_nid))
                .map_or(false, |a| a.body_nid == Some(body_nid)
                    && a.res == nverts
                    && a.generation == bgen));
        // G3: when the resident-field sub-mode is on AND the resident density buffer provably holds THIS
        // body's current density (same body_nid + resolution + bake_bounds, and it was NOT reallocated this
        // frame — a fresh/recycled buffer isn't GPU2GPU-filled yet), mesh DIRECTLY from rv.buffer, skipping
        // the slot.field host upload. Otherwise fall back to the proven CPU-grid upload (always correct).
        // The content-identity guard is stronger than a bare generation counter for the recycling VolumePool
        // (the same GPU buffer is reassigned across bodies). Bit-parity proven by sdf_dc_resident_field_parity.
        // Break the guard into its parts so the [GpuMeshPath] trace can show WHICH clause fails (the
        // bake_bounds clause is the suspect: a moving prim shifts the body bounds, so rv.bake_bounds — set
        // from the prior frame's grid — may not equal this bake's grid_min/max every drag frame).
        let rv_ok = self.cache.body_vol_cache.resident_volume().map_or(false, |rv| rv.body_nid == body_nid);
        let res_ok = self.cache.body_vol_cache.resident_volume().map_or(false, |rv| rv.resolution == [nverts, nverts, nverts]);
        let bounds_ok = self.cache.body_vol_cache.resident_volume().map_or(false, |rv| {
            (rv.bake_bounds_min - grid_min).length_squared() < 1e-6
                && (rv.bake_bounds_max - grid_max).length_squared() < 1e-6
        });
        // "Buffer content current" gate. The earlier `!realloc` guard was a bind-group-invalidation proxy
        // that stayed stuck true across the draft→full 512³ settle (realloc only clears in viewport_render,
        // and Object Mode doesn't render the VP path that consumes it) → field-direct was permanently off at
        // 512³, forcing the redundant 512MB slot.field upload every bake. `resident_buffer_valid` is the
        // precise signal: true iff rv.buffer holds THIS body's current field (set after any buffer write —
        // CPU upload or path-A in-place bake; cleared on realloc or grid change). The post-accept rv sync
        // (poll_primary_bake) makes it true before this read; the field-direct mesh builds its own bind
        // group, so a realloc'd-but-uploaded buffer is safe. (Object-Mode-only path — dispatch_grid_dc_async
        // early-returns under force_volume_only.)
        let buf_valid = self.cache.body_vol_cache.resident_buffer_current();
        // PROVENANCE gate (the flat-slab killer). `bounds_ok` above compares `rv.bake_bounds` — the
        // *label*, overwritten to the new bounds every frame `resident_upload_needed` is set — so it
        // reads true the instant a settle re-tighten / escape moves the mapping, EVEN THOUGH the
        // buffer CONTENT is still authored at the OLD bounds (the re-upload lands a few frames later).
        // Field-direct then maps stale/empty content under the new mapping → 0% inside → flat slab
        // (AZRA_PROBE ground truth: after every [Retighten], GPU-buf 0.0% vs CPU-grid 6.6%, all bound
        // LABELS agreeing). `last_upload_bounds` moves ONLY when content is truly written, so it is the
        // real provenance: field-direct is safe iff the content was written at the bounds we mesh at.
        // When it is NOT, we fall through to `fallback_grid` (the CPU oracle grid — the probe confirms
        // it is correct at that exact frame), so no stale buffer is ever meshed; field-direct re-engages
        // for free the frame the re-upload lands (last_upload_bounds catches up to grid bounds).
        let content_bounds_ok = {
            let (bmin, bmax) = self.cache.body_vol_cache.resident_buffer_bounds();
            (bmin - grid_min).length_squared() < 1e-6 && (bmax - grid_max).length_squared() < 1e-6
        };
        // NOTE: `content_bounds_ok` is computed for the trace but is NOT a hard gate. It was added
        // (6db7950) as a flat-slab guard, but the actual flat-slab roots turned out to be elsewhere and
        // are fixed at the source: the atlas grow-cooldown thin-slab bake (3a97dd8) and the realloc
        // provenance leak (f430bec — which sets resident_buffer_valid=false on realloc, so `buf_valid`
        // ALREADY rejects a swapped/empty buffer here). In both observed flat-slab cases content_bounds_ok
        // was TRUE, so it never actually prevented one — it only forced the CPU-grid fallback (an ~8MB
        // grid re-upload every frame) on transient bounds shifts during a drag, which killed the "live"
        // feel of moving a prim. A same-instance in-place re-bake is at most 1 frame stale (self-corrects);
        // the dangerous realloc case is still caught by `buf_valid`. So keep field-direct live on a bounds
        // shift and do NOT gate on content_bounds_ok.
        let use_resident_field = self.cache.gpu_sdf_mesh_resident_field
            && !brick_resident_ok // S2.4: sparse-atlas content wins over any (stale) rv.buffer
            && buf_valid && rv_ok && res_ok && bounds_ok;
        // [GpuMeshPath] discrete trace (changed-only — no per-frame spam): proves the RESIDENT path is live
        // and shows whether the G3 field-direct read engaged or fell back to the CPU-grid upload, with the
        // failing clause. If this line never appears, the resident interception isn't running at all.
        {
            use std::cell::RefCell;
            thread_local! { static LAST: RefCell<std::collections::HashMap<String,(bool,bool,bool,bool,bool)>> = RefCell::new(std::collections::HashMap::new()); }
            let key = format!("{body_nid:?}");
            let cur = (use_resident_field, buf_valid, rv_ok, res_ok, bounds_ok && content_bounds_ok);
            let changed = LAST.with(|m| { let mut m = m.borrow_mut(); if m.get(&key) != Some(&cur) { m.insert(key, cur); true } else { false } });
            if changed {
                let path = if brick_resident_ok { "sparse-atlas (SdfBrickAtlasGpu pool)" }
                    else if use_resident_field { "field-direct (rv.buffer)" }
                    else { "CPU-grid fallback (slot.field upload)" };
                eprintln!("[GpuMeshPath] body={body_nid:?} RESIDENT field_direct={use_resident_field} → {path} (buf_valid={buf_valid} rv_ok={rv_ok} res_ok={res_ok} bounds_ok={bounds_ok} content_bounds_ok={content_bounds_ok})");
            }
        }
        // [visrec] flight recorder: mesh dispatch gate (Part B #5) + transient invariant checks
        // (Part D, the auto-trigger). Everything below is gated on AZRA_VISLOG — one cached bool
        // check when disabled, zero per-frame cost. READ-ONLY: no bake/mesh logic is altered.
        if crate::vis_recorder::vis_enabled() {
            let slot_meta = self.gpu.as_ref()
                .and_then(|g| g.sdf_body_mesh.get(&body_nid))
                .map(|s| (s.res, s.capacity_res));
            crate::vis_recorder::visrec(self.diag_frame, "mesh.dispatch", || format!(
                "body={:?} nverts={} grid={:.3?}..{:.3?} field_direct={} \
                 (buf_valid={} rv_ok={} res_ok={} bounds_ok={} content_bounds_ok={}) slot(res,cap)={:?}",
                body_nid, nverts, grid_min, grid_max, use_resident_field,
                buf_valid, rv_ok, res_ok, bounds_ok, content_bounds_ok, slot_meta));
            // Cheap invariant checks — a violation means the mesh is about to be built from a
            // wrong/stale source (the transient vanish / trailing-cube / inverted-surface class).
            let mut violations: Vec<String> = Vec::new();
            if use_resident_field {
                if let Some(rv) = self.cache.body_vol_cache.resident_volume() {
                    if rv.resolution != [nverts, nverts, nverts] {
                        violations.push(format!(
                            "field_direct chosen but rv.resolution={:?} != nverts={}",
                            rv.resolution, nverts));
                    }
                    if (rv.bake_bounds_min - grid_min).abs().max_element() > 1e-4
                        || (rv.bake_bounds_max - grid_max).abs().max_element() > 1e-4
                    {
                        violations.push(format!(
                            "field_direct chosen but rv.bake_bounds={:.4?}..{:.4?} != grid={:.4?}..{:.4?}",
                            rv.bake_bounds_min, rv.bake_bounds_max, grid_min, grid_max));
                    }
                } else {
                    violations.push("field_direct chosen but resident_volume() is None".to_string());
                }
            }
            match slot_meta {
                Some((sres, scap)) => {
                    if sres != nverts {
                        violations.push(format!(
                            "slot.res={} != nverts={} after the ensure block (capacity={})",
                            sres, nverts, scap));
                    }
                }
                None => violations.push("mesh slot missing after the ensure block".to_string()),
            }
            // S2.4: a brick-resident body's grid is empty BY DESIGN (SparseBricks bake) — not a
            // violation; the mesh source is the atlas pool.
            if nverts <= crate::gpu_jobs::max_gridbacked_resolution() && !brick_resident_ok {
                if let Some((g, ..)) = self.cache.body_vol_cache.body_grid(body_nid) {
                    if g.is_empty() {
                        violations.push(format!(
                            "body_grids entry present but len()==0 for a GridBacked body (nverts={})",
                            nverts));
                    }
                }
            }
            if !violations.is_empty() {
                let joined = violations.join(" | ");
                eprintln!("[VISBUG] frame={} body={:?} {}", self.diag_frame, body_nid, joined);
                crate::vis_recorder::visrec(self.diag_frame, "VISBUG", || joined.clone());
                // Auto-dump, rate-limited to 1 per 300 frames (diagnostic throttle only).
                if crate::vis_recorder::should_autodump(self.diag_frame) {
                    let snapshot = self.build_visdump_snapshot();
                    if let Some(path) = crate::vis_recorder::dump_to_file(&snapshot) {
                        eprintln!("[VISBUG] auto-dumped {}", path.display());
                    }
                }
            }
        }
        // The fallback (field guard failed — e.g. the draft→full resolution mismatch) clones the grid
        // HERE only, so the common field-direct path pays no clone. Brick-resident bodies have no
        // dense grid to fall back on (empty by design) — the sparse branch below is their path.
        let want_sparse = self.cache.gpu_sdf_mesh_sparse;
        let fallback_grid: Option<Vec<f32>> = if use_resident_field || brick_resident_ok {
            None
        } else {
            self.cache.body_vol_cache.body_grid(body_nid).map(|(g, ..)| g.to_vec())
        };
        // Phase 3A slice 1: sparse PRE-PASS (POLL context — Frame Ordering Contract: buffer
        // create/grow lives HERE, never in a panel/input path). Classify the dense grid → surface
        // bricks (band math identical to the old in-mesher classify), then create-or-grow the
        // PERSISTENT `SdfBrickAtlasGpu` in GpuCtx (grow-only watermark + generation bump on
        // recreate) and upload this bake's index/pool. The dispatch below binds the SAME persistent
        // buffers — the per-bake transient index/pool alloc (the "FIRST CUT" in
        // mesh_into_slot_sparse) is gone. `sparse_bricks = Some(allocated)` doubles as the dispatch
        // gate and preserves the exact previous condition: want_sparse && slot.res > 512 &&
        // fallback_grid.is_some().
        let _t_prepass = std::time::Instant::now();
        let mut sparse_bricks: Option<usize> = None;
        if brick_resident_ok {
            // S2.4: the SparseBricks BAKE already filled the persistent atlas POOL, and the
            // classifier INDEX was uploaded at dispatch — the atlas content is bake-fresh. Route
            // straight to mesh_into_slot_sparse below. The grid-classify pre-pass in the branch
            // below MUST NOT run for this body (see the brick_resident_ok doc above).
            sparse_bricks = self.gpu.as_ref()
                .and_then(|g| g.sdf_brick_atlases.get(&body_nid))
                .map(|a| a.allocated_bricks as usize);
        } else if want_sparse {
            let slot_res = self.gpu.as_ref()
                .and_then(|g| g.sdf_body_mesh.get(&body_nid))
                .map(|s| s.res)
                .filter(|&r| r > 512);
            if let (Some(gv), Some(res)) = (fallback_grid.as_ref(), slot_res) {
                if let Some(g) = self.gpu.as_mut() {
                    let cell_world = ((grid_max - grid_min) / (res - 1).max(1) as f32).min_element().max(1e-6);
                    let sb = core_math::chunked_dc::classify_grid_to_sparse(gv, res, 8, 2.0 * cell_world);
                    let index_needed = sb.index.len() as u32;
                    let pool_bricks = (sb.pool.len() / sb.brick_voxels() as usize) as u32;
                    let device = &g.device;
                    // P2-B5: per-body atlas + LRU touch/budget (same policy as the bake dispatch).
                    let atlas = g.sdf_brick_atlases.entry(body_nid).or_insert_with(|| {
                        graphics::SdfBrickAtlasGpu::new(device, index_needed, pool_bricks)
                    });
                    atlas.ensure_capacity(device, &g.queue, index_needed, pool_bricks);
                    atlas.upload(&g.queue, &sb.index, &sb.pool, sb.dims_bricks, res, body_nid);
                    sparse_bricks = Some(sb.allocated_bricks());
                    g.sdf_atlas_lru.insert(body_nid, self.diag_frame);
                    for (nid, freed) in crate::gpu_ctx::evict_sdf_atlases_over_budget(
                        &mut g.sdf_brick_atlases, &mut g.sdf_atlas_lru,
                        body_nid, g.sdf_atlas_budget_bytes)
                    {
                        eprintln!("[SparseEvict] body={:?} atlas evicted ({}MB freed) — LRU over the {}MB budget",
                            nid, freed >> 20, g.sdf_atlas_budget_bytes >> 20);
                    }
                }
            }
        }
        let _prepass_ms = _t_prepass.elapsed().as_secs_f64() * 1000.0;
        // GATE 2 / C2 — consume the unified DirtyMeshRegion BEFORE borrowing gpu/mesher (it needs
        // &mut body_vol_cache). Some(window) = every atlas change since the last tiled mesh is
        // inside it (unioned across partial landings); None = full re-mesh. Taken exactly once —
        // a mesh that then doesn't run leaves the NEXT mesh conservative (full), never stale.
        // GATED on the tiled sparse dispatch actually happening: the region is now the SAME
        // channel the dense consumers (chunked DC + partial texture upload) read, so a dense
        // body's populate must NOT eat it here.
        let sparse_dirty = if sparse_bricks.is_some() {
            self.cache.body_vol_cache.take_dirty_mesh_region(body_nid)
                .map(|(mn, mx)| crate::sdf_gpu_mesh::SdfTileDirty::Voxels(mn, mx))
        } else {
            None
        };
        // Dispatch the QEF-DC passes into the slot's resident buffers (index count → indirect args).
        let _t_dispatch = std::time::Instant::now();
        // Phase 4b loud degradation: set inside the gpu borrow, raised after it (toast needs &mut self).
        let mut deferred_trunc_toast: Option<String> = None;
        if let (Some(g), Some(m)) = (self.gpu.as_mut(), self.cache.sdf_mesher.as_ref()) {
            // Stage B-1: lazily create the SHARED sparse mesh pool (POLL phase — Frame Ordering
            // Contract) before the sparse dispatch below targets a region of it.
            if sparse_bricks.is_some() && g.sdf_mesh_pool.is_none() {
                let ready = g.device.features().contains(wgpu::Features::MULTI_DRAW_INDIRECT)
                    && g.device.features().contains(wgpu::Features::INDIRECT_FIRST_INSTANCE);
                g.sdf_mesh_pool = Some(crate::sdf_gpu_mesh::SdfMeshPool::new(&g.device, ready));
                eprintln!("[SdfPool] created (multi_draw_ready={ready})");
            }
            // &mut: the dense CPU-grid fallback lazily allocates slot.field (P0-R3a; poll phase,
            // Frame Ordering holds). The other arms only read the slot.
            if let Some(slot) = g.sdf_body_mesh.get_mut(&body_nid) {
                // B2/Mesher2 stripped — always stock QEF (feature flag stays off).
                crate::sdf_gpu_mesh::set_dc_mesher2_feature(false);
                // Phase 3B sparse: ONLY on a full bake (no field-direct). `classify_grid_to_sparse` must
                // NEVER run on the live-drag field-direct path or it freezes editing — drags stay dense
                // field-direct (live). The classify is rayon-parallelised: SPIKE E (core_math example
                // `spike_e_classify`) measured 63ms @512³ (38× down from the 2.4s single-threaded baseline),
                // so it runs ONCE on the settle full bake — a brief one-time cost, off the live-drag path.
                // USABLE at 512³ now. (Interim still bakes a dense res³ field for the classify to read;
                // 3A will emit sparse bricks straight from the bake to drop that ~0.5GB transient.)
                // SPARSE RE-PARKED to >512 (was >256). The sparse decode IS bit-exact (the NORMAL-parity test
                // sdf_gpu_mesh.rs::sdf_dc_sparse_brick_decode_parity: degenerate sparse=0/dense=0, angle
                // mean=0.00°/max=0.03°) — that is NOT the issue. The issue is the un-park's MEMORY PREMISE was
                // WRONG: the sparse path still allocates the full DENSE SdfBodyMeshSlot (~3.2GB at 512³: field
                // 512MB + cell_vert 533MB + verts 1GB + idx 1GB). Only the TRANSIENT brick buffers shrink, and
                // those add per-bake churn (classify O(res³) + a ~0.54GB pool alloc/frame) that AMPLIFIES VRAM
                // pressure on top of the dense slot → device-loss risk when several 512³ bodies coexist. So
                // sparse-at-512 costs MORE and saves nothing until the SLOT itself is sparse. The real win is
                // Phase 3A (sparse BAKE) + 3B (surface-proportional slotted POOL) per GPU_MESH_EXTRACTION_PLAN /
                // the resolution-architecture plan. Until then 512³ stays on the dense path (the prior working
                // state). >512 keeps the sparse branch live for the future tiled path. (crash mitigation)
                // Slice 3A-1 landed: the per-bake index/pool BUFFER alloc is gone (persistent
                // SdfBrickAtlasGpu, uploaded by the pre-pass above); the classify O(res³) and the
                // dense slot itself remain — the gate stays >512 until 3A's sparse BAKE lands.
                if let Some(bricks) = sparse_bricks {
                    // P2-B5: mesh access counts as use — stamp the LRU before borrowing the atlas.
                    g.sdf_atlas_lru.insert(body_nid, self.diag_frame);
                    let atlas = g.sdf_brick_atlases.get(&body_nid)
                        .expect("sparse atlas populated (pre-pass upload or S2.4 brick-resident bake)");
                    // Slice b: quiet body (no gesture escalation churn) → this dispatch may trim
                    // transient-peak capacity back to the settled surface need (one-shot flag).
                    slot.may_trim = self.cache.body_vol_cache.sparse_quiet(body_nid);
                    // C1: the TILED dispatch is the production sparse path (per-tile regions +
                    // per-tile args, one multi-draw per body). It returns false only when the
                    // pool cannot host the request — the proven monolithic entry then lands on
                    // the per-body slot fallback (same geometry, per-body draw).
                    let tiled_ok = match g.sdf_mesh_pool.as_mut() {
                        Some(p) => m.mesh_into_slot_sparse_tiled(
                            &g.device, &g.queue, slot, atlas, grid_min, grid_max, body_nid, p,
                            sparse_dirty.as_ref()),
                        None => false,
                    };
                    if !tiled_ok {
                        m.mesh_into_slot_sparse(&g.device, &g.queue, slot, atlas, grid_min, grid_max,
                            body_nid, g.sdf_mesh_pool.as_mut());
                        // Phase 4b: the tiled path refused because even the multi-group packing
                        // exceeds the pool budget AND the real surface exceeds the dense ceiling —
                        // the monolithic mesh IS truncated. Never silent (the self-heal toast
                        // precedent); one toast per arming (the latch clears on tiled re-entry).
                        if std::mem::take(&mut slot.envelope_over_budget) {
                            deferred_trunc_toast = Some(format!(
                                "Surface too complex for the mesh pool budget at {}³ — display is \
                                 TRUNCATED. Lower the body resolution.", nverts));
                        }
                    }
                    if crate::perf_spam_on() {
                        let total = ((nverts + 7) / 8).pow(3) as usize;
                        eprintln!("[SparseMesh] body={body_nid:?} res={nverts}³ bricks={bricks}/{total} ({}% of dense; pool≈{}MB vs dense {}MB; atlas gen={} cap={} bricks)",
                            if total > 0 { bricks * 100 / total } else { 0 },
                            (bricks * 512 * 4) >> 20, ((nverts as usize).pow(3) * 4) >> 20,
                            atlas.generation, atlas.pool_capacity_bricks);
                    }
                } else if use_resident_field {
                    let rv = self.cache.body_vol_cache.resident_volume().unwrap();
                    // ONE-SHOT CONFIRMATION PROBE (set env AZRA_PROBE=1). Reads back rv.buffer's CONTENT and
                    // compares it to the correct CPU oracle grid (body_grid) — to settle whether the flat
                    // slab is a CORRUPT buffer (bake wrote it at different bounds → provenance mismatch) or a
                    // GOOD buffer meshed wrong. Throttled ~1/sec; zero cost unless the env var is set.
                    if std::env::var_os("AZRA_PROBE").is_some() {
                        // Fire on every NEW bake config (nverts or bounds changed) so the slab state — which
                        // appears right after a duplicate / resolution change — is always captured, not just
                        // the steady single-box case. Bounds quantized to 0.01 to ignore float jitter.
                        use std::cell::RefCell;
                        thread_local! { static LASTCFG: RefCell<Option<(u32,[i32;6])>> = RefCell::new(None); }
                        let q = |v: f32| (v * 100.0).round() as i32;
                        let cfg = (nverts, [q(grid_min.x),q(grid_min.y),q(grid_min.z),q(grid_max.x),q(grid_max.y),q(grid_max.z)]);
                        let fire = LASTCFG.with(|l| { let mut l = l.borrow_mut(); if l.as_ref() != Some(&cfg) { *l = Some(cfg); true } else { false } });
                        if fire {
                            let count = (nverts as usize).pow(3);
                            let want = ((count * 4) as u64).min(rv.buffer.size());
                            let staging = g.device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("resident-probe"), size: want,
                                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                                mapped_at_creation: false,
                            });
                            let mut enc = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("resident-probe") });
                            enc.copy_buffer_to_buffer(&rv.buffer, 0, &staging, 0, want);
                            g.queue.submit(Some(enc.finish()));
                            let (tx, rx) = std::sync::mpsc::channel();
                            staging.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
                            g.device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
                            if let Ok(Ok(())) = rx.recv() {
                                let mapped = staging.slice(..).get_mapped_range();
                                let gpu: &[f32] = bytemuck::cast_slice(&mapped);
                                let m2 = gpu.len().min(count);
                                let gpu_in = gpu[..m2].iter().filter(|&&d| d < 0.0).count();
                                let (cpu_in, cpu_len, cmin, cmax) = self.cache.body_vol_cache.body_grid(body_nid)
                                    .map(|(cg, _, cmin, cmax, _)| (cg.iter().filter(|&&d| d < 0.0).count(), cg.len(), cmin, cmax))
                                    .unwrap_or((0, 0, glam::Vec3::ZERO, glam::Vec3::ZERO));
                                let gpu_pct = 100.0 * gpu_in as f32 / m2.max(1) as f32;
                                let cpu_pct = 100.0 * cpu_in as f32 / cpu_len.max(1) as f32;
                                let verdict = if (gpu_pct - cpu_pct).abs() > 1.0 {
                                    "MISMATCH → rv.buffer CONTENT is wrong (bake/provenance bug)"
                                } else { "match → buffer content OK (bug is mesher/draw)" };
                                eprintln!("[RESIDENT-PROBE] body={body_nid:?} nverts={nverts} | rv.bake_bounds={:.2?}..{:.2?} dc_bounds={grid_min:.2?}..{grid_max:.2?} cpu_bounds={cmin:.2?}..{cmax:.2?} | GPU-buf inside {gpu_pct:.1}% vs CPU-grid inside {cpu_pct:.1}% → {verdict}",
                                    rv.bake_bounds_min, rv.bake_bounds_max);
                            }
                        }
                    }
                    if !m.mesh_into_slot_from_resident(
                        &g.device, &g.queue, slot, &rv.buffer, grid_min, grid_max,
                    ) {
                        // Dense@>512 or bind-limit reject — do not leave a poisoned encode path.
                        deferred_trunc_toast = Some(format!(
                            "Cannot dense-mesh body at {}³ (GPU bind limit) — use sparse / lower Grid.",
                            nverts
                        ));
                    }
                } else if let Some(ref gv) = fallback_grid {
                    // I7: never dense-DC above GridBacked max (would allocate ~4GB cell_vert @1024).
                    if nverts > crate::gpu_jobs::max_gridbacked_resolution() {
                        eprintln!(
                            "[DcBind] REJECT populate fallthrough dense res={}³ body={:?} — need sparse atlas",
                            nverts, body_nid
                        );
                        deferred_trunc_toast = Some(format!(
                            "Mesh at {}³ needs sparse bricks — rebake or lower Grid (GPU bind limit).",
                            nverts
                        ));
                    } else if !m.mesh_into_slot(&g.device, &g.queue, slot, gv, grid_min, grid_max) {
                        deferred_trunc_toast = Some(format!(
                            "Dense mesh failed at {}³ (GPU bind limit).",
                            nverts
                        ));
                    }
                }
                // P1-#6b (fixed): every arm above just meshed at `grid_min` — if that is the
                // entry's CURRENT anchor, the fresh mesh needs no rigid re-anchor; a mesh from
                // an older bake (a rigid shift raced in after it completed) keeps the offset.
                self.cache.body_vol_cache.clear_mesh_rigid_offset_if_current(body_nid, grid_min);
                crate::sdf_gpu_mesh::set_dc_mesher2_feature(false);
            }
        }
        if let Some(t) = deferred_trunc_toast {
            self.toast(&t);
        }
        let _dispatch_ms = _t_dispatch.elapsed().as_secs_f64() * 1000.0;
        let _pop_ms = _pop_t0.elapsed().as_secs_f64() * 1000.0;
        if _pop_ms > 50.0 {
            eprintln!("[SPIKE] sdf_populate_resident body={:?} res={}³: total={:.0}ms | mesher_compile={:.0} slot_alloc={:.0} sparse_prepass={:.0} mesh_dispatch={:.0} | rest={:.0}",
                body_nid, nverts, _pop_ms, _mesher_ms, _slot_ms, _prepass_ms, _dispatch_ms,
                _pop_ms - _mesher_ms - _slot_ms - _prepass_ms - _dispatch_ms);
        }
        // Gate 0 metrics (instrumentation only): accumulate this body's DC-populate ms into perf
        // (was resident.rs local eprintln-only) and latch the body's live resident-mesh pool VRAM +
        // pool fragmentation. Reads self.gpu (shared) then writes self.perf (disjoint field) — both
        // self.gpu borrows above are released, so no overlap.
        self.perf.vol_dc_ms += _pop_ms;
        self.perf.sdf_pool_body_bytes = self.gpu.as_ref()
            .and_then(|g| g.sdf_mesh_pool.as_ref()).map_or(0, |p| p.body_vram_bytes(body_nid));
        self.perf.sdf_pool_hole_ratio = self.gpu.as_ref()
            .and_then(|g| g.sdf_mesh_pool.as_ref()).map_or(0.0, |p| p.hole_ratio());
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// R3b — poll the per-slot DC counter readbacks (issued by every sparse mesh dispatch) and
    /// handle OUTPUT OVERFLOW: the sparse verts/idx buffers are surface-proportional starting
    /// sizes, not correctness bounds — when the atomicMin-clamped shader truncated the mesh, grow
    /// the buffers (×1.5 / exact need, generation bump) and RE-DISPATCH the mesh from the
    /// still-resident brick atlas so the frame never keeps a truncated surface. POLL PHASE ONLY
    /// (Frame Ordering Contract — buffer recreation is banned in panels/input/draw). The draw +
    /// picking passes bind `slot.verts`/`idx` fresh every pass, so the grow cannot dangle a bind
    /// group; `slot.generation` guards any future consumer that caches one.
    pub(crate) fn poll_sdf_mesh_output_overflow(&mut self) {
        let Some(g) = self.gpu.as_mut() else { return; };
        let Some(m) = self.cache.sdf_mesher.as_ref() else { return; };
        let mut remeshed = false;
        let mut trunc_toasts: Vec<String> = Vec::new(); // raised after the gpu borrow ends
        for (nid, slot) in g.sdf_body_mesh.iter_mut() {
            // C1 TILED arm: per-tile counters readback → raise ONLY the overflowing tiles'
            // cap floors (×1.25 true need) and re-mesh from the still-resident atlas. The
            // untouched tiles re-dispatch into their unchanged regions (C1 full re-mesh; C2
            // narrows this to the dirty set).
            if slot.tiled {
                let Some(over) = slot.poll_tile_overflow() else { continue; };
                if over.is_empty() { continue; }
                if !slot.raise_tile_floors(&over) {
                    eprintln!("[SdfMeshGrow] body={nid:?} {} tile(s) overflowed but floors at the \
                               per-tile bound — mesh stays truncated", over.len());
                    continue;
                }
                eprintln!("[SdfMeshGrow] body={nid:?} {} tile(s) overflowed → floors raised — re-meshing",
                    over.len());
                let atlas_ok = g.sdf_brick_atlases.get(nid)
                    .map_or(false, |a| a.body_nid == Some(*nid) && a.res == slot.res);
                if let (true, Some((gmin, gmax))) = (atlas_ok, slot.last_sparse_bounds) {
                    let atlas = g.sdf_brick_atlases.get(nid).expect("checked above");
                    if let Some(p) = g.sdf_mesh_pool.as_mut() {
                        // C2: overflow only re-meshes/grows the OVERFLOWED tiles (explicit key
                        // set) — clean tiles keep their regions; any staleness falls back to full.
                        let over_keys = crate::sdf_gpu_mesh::SdfTileDirty::Tiles(
                            over.iter().map(|&(k, _)| k).collect());
                        if m.mesh_into_slot_sparse_tiled(&g.device, &g.queue, slot, atlas, gmin, gmax, *nid, p,
                            Some(&over_keys)) {
                            remeshed = true;
                        }
                    }
                }
                continue;
            }
            let Some([vc, ic]) = slot.poll_counter_overflow() else { continue; };
            // Stage B-1: a POOLED slot grows its pool REGION (relocate/rebuild, generation bump);
            // an unpooled slot grows its own buffers (the original R3b path). Same ×1.5 policy.
            let grown = if slot.pooled {
                let max_bind = g.device.limits().max_storage_buffer_binding_size as u64;
                let (dv, di) = crate::sdf_gpu_mesh::dc_output_caps_dense(max_bind, slot.capacity_res);
                let ok = g.sdf_mesh_pool.as_mut()
                    .map_or(false, |p| p.grow_slot(&g.device, &g.queue, *nid, vc as u64, dv, di));
                if ok {
                    if let Some(ps) = g.sdf_mesh_pool.as_ref().and_then(|p| p.slot(*nid)) {
                        slot.vert_cap = ps.vert_cap;
                        slot.idx_cap = ps.idx_cap;
                    }
                    slot.generation += 1; // region relocated — counters over timing tricks
                }
                ok
            } else {
                slot.grow_output_caps(&g.device, vc as u64)
            };
            if !grown {
                // Already at the dense ceiling — same truncation behavior as the dense era
                // (arrayLength/atomicMin keep it safe; never OOB). Nothing more to grow HERE —
                // but a MEASURED need past the ceiling is real truncation, so it degrades LOUDLY
                // (Phase 4b: toast + log, the self-heal precedent — never silent).
                eprintln!("[SdfMeshGrow] body={nid:?} overflow (verts={vc} idx={ic}) but caps at dense ceiling — mesh stays truncated");
                if vc as u64 > slot.vert_cap {
                    trunc_toasts.push(format!(
                        "Mesh truncated at the GPU vertex ceiling ({}M verts) — lower the body resolution.",
                        slot.vert_cap / 1_000_000));
                }
                continue;
            }
            eprintln!("[SdfMeshGrow] body={nid:?} DC output overflow (verts={vc} idx={ic}) → grew caps to v={} i={} (gen={} pooled={}) — re-meshing",
                slot.vert_cap, slot.idx_cap, slot.generation, slot.pooled);
            // Re-dispatch only if the persistent atlas still holds THIS body's content at the
            // slot's res (same identity check as the brick-resident gate). Otherwise the next
            // bake re-meshes with the grown caps — nothing is lost.
            let atlas_ok = g.sdf_brick_atlases.get(nid)
                .map_or(false, |a| a.body_nid == Some(*nid) && a.res == slot.res);
            if let (true, Some((gmin, gmax))) = (atlas_ok, slot.last_sparse_bounds) {
                let atlas = g.sdf_brick_atlases.get(nid).expect("checked above");
                m.mesh_into_slot_sparse(&g.device, &g.queue, slot, atlas, gmin, gmax,
                    *nid, g.sdf_mesh_pool.as_mut());
                remeshed = true;
            }
        }
        for t in trunc_toasts {
            self.toast(&t);
        }
        if remeshed { self.mark_dirty(DirtyFlags::RENDER); }
    }

    /// O2b — automatic per-body octree-LOD policy (POLL PHASE, Frame Ordering Contract). For
    /// every brick-resident body slot, project one voxel to screen pixels and pick an octree
    /// level via `choose_lod_level` (deadband hysteresis) + a 1s/body rate limit; on a level
    /// change, re-mesh from the EXISTING atlas: level > 0 through the windowed
    /// `mesh_into_slot_sparse_lod` (cheap — surface-window dispatch, no re-bake/re-classify),
    /// level 0 through the NORMAL populate path (`sdf_populate_resident` → tiled/monolithic).
    /// The ACTIVE body is always level 0 (it owns the tiled incremental path). `slot.lod_level`
    /// (the actual mesh content) is the current-level truth — a normal bake resetting it to 0
    /// re-enters the policy naturally. Toggle: `cache.sdf_lod_auto` (Developer Tweaks, default
    /// OFF); toggling OFF restores every LOD'd slot to level 0 through the same restore path.
    pub(crate) fn poll_sdf_lod_policy(&mut self) {
        // Frame Budget: bounded work — at most this many level switches (re-mesh dispatches) per
        // frame; the rest re-evaluate next frame.
        const MAX_SDF_LOD_SWITCHES_PER_FRAME: usize = 2;
        if self.gpu.is_none() { return; }
        let enabled = self.cache.sdf_lod_auto && self.cache.dc_route_is_resident();
        // Camera signal (same source as update_lod_resolutions / cam_ray: target + orbit*dist,
        // tan(fov/2), viewport height).
        let cam = &self.viewport.camera;
        let cam_pos = cam.target + cam.orbit * glam::Vec3::new(0.0, 0.0, cam.distance);
        let tan_half = (self.render.fov_deg.to_radians() * 0.5).tan().max(0.01);
        let viewport_h = self.viewport.viewport_rect.height().max(1.0);
        let ortho = self.render.ortho;
        let ortho_hw = cam.distance.max(0.01);
        let active = self.scene.active_body_nid;

        // Read pass: decide per body (no &mut self borrows held across the dispatch pass).
        let mut decisions: Vec<(core_math::scene_graph::NodeId, u32, glam::Vec3, glam::Vec3, u32)> = Vec::new();
        {
            let g = self.gpu.as_ref().expect("checked above");
            self.cache.sdf_lod_last_switch.retain(|nid, _| g.sdf_body_mesh.contains_key(nid));
            for (nid, slot) in g.sdf_body_mesh.iter() {
                let cur = slot.lod_level;
                if !enabled || *nid == active {
                    // Policy off / body being edited: the only job is restoring a LOD'd slot.
                    if cur != 0 {
                        if let Some((gmin, gmax)) = slot.last_sparse_bounds {
                            decisions.push((*nid, 0, gmin, gmax, slot.res));
                        }
                    }
                    continue;
                }
                // Only bodies whose atlas still holds THIS body's content at the slot's res can
                // re-mesh from it (the poll_sdf_mesh_output_overflow identity check).
                let atlas_ok = g.sdf_brick_atlases.get(nid)
                    .map_or(false, |a| a.body_nid == Some(*nid) && a.res == slot.res);
                let Some((gmin, gmax)) = slot.last_sparse_bounds else { continue; };
                if !atlas_ok { continue; }
                // Projected size of one voxel in pixels. Bounds are body-LOCAL — apply the body
                // transform (Prim Position Space rule): center via local_to_world, voxel size
                // scaled by the max world-scale component (conservative: keeps detail).
                let bt = self.scene.scene_graph.body_world_transform(*nid).unwrap_or_default();
                let center = bt.local_to_world((gmin + gmax) * 0.5);
                let voxel_world = ((gmax - gmin).min_element() / (slot.res - 1).max(1) as f32)
                    * bt.scale.abs().max_element().max(1e-6);
                // px/world-unit at the body: perspective (h/2)/(d·tan½) — approximation documented
                // in lod_policy.rs; ortho substitutes the ortho half-height for d·tan½.
                let denom = if ortho { ortho_hw } else { (cam_pos - center).length().max(1e-4) * tan_half };
                let projected_px = voxel_world * (viewport_h * 0.5) / denom;
                let max_level = crate::sdf_gpu_mesh::lod_max_level_for_res(slot.res);
                let want = crate::sdf_gpu_mesh::choose_lod_level(projected_px, cur, max_level);
                if want == cur { continue; }
                // Rate limit: at most one level change per body per second (no popping).
                if self.cache.sdf_lod_last_switch.get(nid)
                    .map_or(false, |t| t.elapsed() < crate::sdf_gpu_mesh::LOD_MIN_SWITCH_INTERVAL) {
                    continue;
                }
                decisions.push((*nid, want, gmin, gmax, slot.res));
            }
        }
        if decisions.is_empty() { return; }

        // Write pass: dispatch the re-meshes (bounded).
        let mut switched = false;
        for (nid, level, gmin, gmax, res) in decisions.into_iter().take(MAX_SDF_LOD_SWITCHES_PER_FRAME) {
            self.cache.sdf_lod_last_switch.insert(nid, std::time::Instant::now());
            if level == 0 {
                // Level-0 restore rides the NORMAL populate path (tiled/monolithic, pooled) —
                // never the LOD entry (which is unpooled-monolithic by design).
                self.sdf_populate_resident(nid, res, gmin, gmax);
                switched = true;
                continue;
            }
            if !self.ensure_sdf_mesher() { return; }
            let m = self.cache.sdf_mesher.as_ref().expect("ensured above");
            let Some(g) = self.gpu.as_mut() else { return; };
            let (Some(slot), Some(atlas)) = (g.sdf_body_mesh.get_mut(&nid), g.sdf_brick_atlases.get(&nid)) else { continue; };
            crate::sdf_gpu_mesh::set_dc_mesher2_feature(false);
            m.mesh_into_slot_sparse_lod(&g.device, &g.queue, slot, atlas, gmin, gmax, level, None);
            if crate::perf_spam_on() {
                eprintln!("[SdfLod] body={nid:?} → level {level} (windowed re-mesh from resident atlas)");
            }
            switched = true;
        }
        if switched { self.mark_dirty(DirtyFlags::RENDER); }
    }

    pub(crate) fn dispatch_grid_dc_async(&mut self, body_nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        let _dc_total_t0 = std::time::Instant::now();

        // Always-cache: all bodies (including active) get DC mesh extraction.
        // During drag, the analytical march provides live feedback (use_lite=false).
        // After drag ends, the DC mesh catches up and raster resumes.
        // One job per body: skip if already in-flight
        if self.cache.pending_dc_jobs.iter().any(|j| j.body_nid == body_nid) {
            return;
        }

        let _t_hash = std::time::Instant::now();
        // A body with 0 OWN visible prims can still be fully defined by a nested sub-body: its grid is
        // baked from the RECURSIVE prim set (serialize_body_for_bake_hot_recursive walks child bodies).
        // `primitives` here is only the body's OWN prims, so bailing on `visible.is_empty()` alone means
        // the moment you move every prim into a sub-body the Object-Mode DC mesh is never built and the
        // surface VANISHES — reappearing only when you add an own prim (the observed cure). Bail only when
        // there is truly nothing to mesh: no own prims AND no child sub-bodies.
        let has_sub_bodies = !self.scene.scene_graph.child_bodies(body_nid).is_empty();
        let mesh_cache_id = match self.scene.scene_graph.get(body_nid) {
            Some(node) => match &node.payload {
                NodePayload::SdfBody { mesh_cache_id: Some(cid), primitives, .. } => {
                    let any_visible = primitives.iter().any(|p| p.visible);
                    if !any_visible && !has_sub_bodies { return; }
                    *cid
                }
                _ => return,
            },
            None => return,
        };
        // Gate 1: unified RECURSIVE BodyContentId — the same stamp the bake entry stores, so a
        // nested-child edit changes the mesh_cache stamp exactly when it changes the bake identity.
        // (Degenerate all-empty-sub-body case falls back to 0, matching the pre-Gate-1 flow.)
        let source_hash = crate::bake_utils::body_content_id(
            &self.scene.scene_graph, body_nid, self.cache.body_vol_cache.lab_operand_gen,
        ).map_or(0, |id| id.0);
        let _hash_ms = _t_hash.elapsed().as_secs_f64() * 1000.0;

        let _t_grid_lookup = std::time::Instant::now();
        let (grid, nverts, grid_min, grid_max, _bake_hash) = match self.cache.body_vol_cache.body_grid(body_nid) {
            Some(data) => data,
            None => return,
        };
        let _grid_lookup_ms = _t_grid_lookup.elapsed().as_secs_f64() * 1000.0;

        // --- S2.4 SPARSE ARM (BEFORE the cubic guard — the body-vanish hazard) ---
        // A brick-resident (SparseBricks) entry has an EMPTY grid by design; the cubic guard
        // below would '[DC] SKIP … non-cubic' it and the body would never mesh. Mesh from the
        // body's persistent SdfBrickAtlasGpu instead — but ONLY while that atlas still holds
        // THIS bake's content (res + generation match the entry's brick_resident marker).
        // P2-B5: atlases are per-body now, so another body's bake no longer swaps the content —
        // a mismatch here means THIS body's atlas grew (generation) or was LRU-evicted; the
        // mesh slot keeps drawing its last mesh and the next bake re-meshes.
        if let Some(bgen) = self.cache.body_vol_cache.brick_resident_gen(body_nid) {
            let atlas_ok = self.gpu.as_ref()
                .and_then(|g| g.sdf_brick_atlases.get(&body_nid))
                .map_or(false, |a| a.body_nid == Some(body_nid)
                    && a.res == nverts
                    && a.generation == bgen);
            // GATE 3: THE route decision (cache_state::dc_mesh_route). A brick-resident body
            // has an empty dense grid by design, so only SparseTiledResident can mesh it —
            // any other route keeps the last mesh until the next bake re-routes.
            if self.cache.dc_mesh_route(atlas_ok) == crate::cache_state::DcMeshRoute::SparseTiledResident {
                // Reuses the slot ensure/grow + sparse dispatch in sdf_populate_resident (its
                // brick-resident branch) — no duplicated slot management.
                self.sdf_populate_resident(body_nid, nverts, grid_min, grid_max);
            } else {
                eprintln!("[SparseBake] DC skip body={:?}: {} — last mesh (if any) kept until the next bake",
                    body_nid,
                    if !self.cache.dc_route_is_resident() { "resident GPU mesh (G2) is OFF" }
                    else { "atlas content no longer matches (another body baked / atlas grew)" });
            }
            return;
        }

        // CUBIC INVARIANT GUARD (defense in depth). The chunked DC extractor indexes the grid as a
        // cube of `nverts` per axis. If a non-cubic grid (len != nverts³) ever reaches here — e.g. an
        // asymmetric bake from an elongated AABB — the walk indexes out of bounds and PANICS (the
        // duplicate-SDF crash). GridBacked bakes are forced cubic upstream (bake_dispatch.rs), so this
        // should never fire; if it does, skip extraction instead of crashing and let the next full bake
        // retighten to a clean cubic grid.
        // Checked on the BORROWED grid (no clone needed yet — see the clone deferral below).
        let nv = nverts as usize;
        if grid.len() != nv * nv * nv {
            eprintln!(
                "[DC] SKIP body={:?}: non-cubic grid len={} (expected {}³={}) — asymmetric grid reached \
                 the cubic DC path; awaiting full rebake",
                body_nid, grid.len(), nverts, nv * nv * nv
            );
            return;
        }

        let grid_res = nverts - 1; // cells = vertices - 1
        let lod = if nverts <= 65 {
            render::mesh_cache::MeshLod::Preview
        } else if nverts <= 129 {
            render::mesh_cache::MeshLod::Medium
        } else {
            render::mesh_cache::MeshLod::High
        };

        // Phase G (experimental, off by default): mesh on the GPU (QEF Dual Contouring) straight into
        // mesh_cache instead of the background CPU DC below. ~2-5ms vs ~125ms → closes the generation
        // race that makes the surface stall/vanish during continuous edits. Reuses the existing raster
        // draw (no new device-loss path). Falls through to CPU DC when the flag is off.
        // G2/G3 RESIDENT path FIRST + BEFORE the 8MB grid clone: it reads rv.buffer (field-direct) or
        // re-fetches the grid itself (rare fallback), so the DEFAULT path pays no per-bake grid clone
        // (~2ms — the bulk of the old dc_dispatch). grid's borrow ends here (unused on this path), so the
        // &mut self call is allowed. Mesh into the persistent slot, drawn via indirect in raster_render.
        // GATE 3: dense-body routing — ONE decision site (cache_state::dc_mesh_route), the
        // former scattered `gpu_sdf_mesh_resident` / `gpu_sdf_mesh` bool tests collapsed.
        match self.cache.dc_mesh_route(false) {
            crate::cache_state::DcMeshRoute::SparseTiledResident => unreachable!(
                "SparseTiledResident requires brick_resident_atlas_ok — handled in the sparse arm above"
            ),
            crate::cache_state::DcMeshRoute::DenseResident => {
                self.sdf_populate_resident(body_nid, nverts, grid_min, grid_max);
                return;
            }
            crate::cache_state::DcMeshRoute::GpuReadbackMeshCache => {
                // Readback path needs an OWNED grid copy — clone only now (not on the resident path).
                let grid_owned = grid.to_vec();
                self.gpu_mesh_to_cache(mesh_cache_id, &grid_owned, nverts, grid_min, grid_max);
                return;
            }
            crate::cache_state::DcMeshRoute::CpuChunkedOracle => {} // falls through to the rayon CPU DC job below
        }
        let _t_grid_copy = std::time::Instant::now();
        let grid_owned = grid.to_vec();
        let _grid_copy_ms = _t_grid_copy.elapsed().as_secs_f64() * 1000.0;

        let generation = self.cache.body_vol_cache.current_generation(body_nid);

        // GATE 3 — DEAD ARM COMPILED OUT (Feature Dependency Audit done): the brick-pool
        // brick-aware CPU DC (`brick_data` + `BrickGridAccessor` extract/update arms) was
        // reachable only when route == CpuChunkedOracle (Developer Tweaks, GPU mesh off) AND
        // `use_brick_pool` (properties toggle, default false, brick pool PARKED per CLAUDE.md)
        // AND `body_brick_volumes` populated (brick_classify.rs — itself gated on use_brick_pool).
        // No production caller. The dense-grid CPU DC below is correct for brick-pool bodies too:
        // the accepted bake ALWAYS lands the dense grid in body_grids (bake_readback runs the grid
        // readback regardless of use_brick_pool), so removing the accessor arm changes no mesh.
        // core_math's BrickGridAccessor / extract_all_from / update_dirty_chunks_from remain
        // (core_math-owned API + tests) — only this app-side wiring is removed.

        // Read dirty range (clone, not remove) — update_resident_volume() needs it
        // later for the partial-vs-full GPU texture upload decision. It will be
        // consumed there after the upload completes.
        let dirty_range = self.cache.body_vol_cache.dirty_mesh_region.get(&body_nid).copied();
        let has_existing_chunked = self.cache.body_vol_cache.body_chunked_meshes.contains_key(&body_nid);
        let is_partial_bake = dirty_range.as_ref().map_or(false, |(bmin, bmax)| {
            bmin[0] > 0 || bmin[1] > 0 || bmin[2] > 0
                || bmax[0] < grid_res || bmax[1] < grid_res || bmax[2] < grid_res
        });

        // Take the existing ChunkedMesh if partial bake (will be returned via DcJobResult)
        let existing_chunked = if is_partial_bake && has_existing_chunked {
            self.cache.body_vol_cache.body_chunked_meshes.remove(&body_nid)
        } else {
            None
        };
        let dirty_range_owned = if existing_chunked.is_some() { dirty_range } else { None };

        let _dc_setup_ms = _dc_total_t0.elapsed().as_secs_f64() * 1000.0;
        if _dc_setup_ms > 0.5 {
            eprintln!(
                "[DcDispatch] body={:?} setup={:.1}ms (hash={:.2}ms grid_lookup={:.2}ms grid_copy={:.2}ms [{}KB])",
                body_nid, _dc_setup_ms, _hash_ms, _grid_lookup_ms, _grid_copy_ms,
                grid_owned.len() * 4 / 1024,
            );
        }

        let smooth_normals = self.cache.smooth_dc_normals;
        let (tx, rx) = std::sync::mpsc::channel();

        rayon::spawn(move || {
            let _t0 = std::time::Instant::now();

            let (mesh, chunked) = if let Some(mut existing) = existing_chunked {
                // Partial bake: update only dirty chunks
                let (dirty_min, dirty_max) = dirty_range_owned.unwrap();
                existing.bounds_min = grid_min;
                existing.bounds_max = grid_max;
                existing.smooth_normals = smooth_normals;
                existing.update_dirty_chunks(&grid_owned, dirty_min, dirty_max);
                let m = existing.to_mesh();
                (m, existing)
            } else {
                // Full extraction
                let mut chunked = core_math::chunked_dc::ChunkedMesh::new(
                    grid_res, 32, grid_min, grid_max, true,
                );
                chunked.smooth_normals = smooth_normals;
                chunked.extract_all(&grid_owned);
                let m = chunked.to_mesh();
                (m, chunked)
            };

            let _ms = _t0.elapsed().as_secs_f64() * 1000.0;
            if _ms > 10.0 { eprintln!("[DC-bg] {:.1}ms for {:?}", _ms, body_nid); }
            let _ = tx.send(DcJobResult { body_nid, mesh, chunked, mesh_cache_id, source_hash, lod, generation });
        });

        self.cache.pending_dc_jobs.push(PendingDcJob { body_nid, rx });
    }

    /// Poll completed background DC extraction jobs. Called once per frame.
    /// Applies results to mesh_cache and body_chunked_meshes, discarding stale results.
    pub(crate) fn poll_dc_jobs(&mut self) {
        let dc_ticket = crate::frame_scheduler::WorkTicket {
            class: crate::frame_scheduler::WorkClass::Medium,
            cost_hint_ms: 0.5,
            name: "dc_results",
        };
        if !self.scheduler.can_run(&dc_ticket) {
            self.scheduler.record_skipped(&dc_ticket);
            return;
        }
        let _sched_t0 = std::time::Instant::now();
        const MAX_DC_RESULTS_PER_FRAME: usize = 2;
        let mut dc_count = 0;
        let mut i = 0;
        while i < self.cache.pending_dc_jobs.len() {
            match self.cache.pending_dc_jobs[i].rx.try_recv() {
                Ok(result) => {
                    self.cache.pending_dc_jobs.swap_remove(i);
                    // Stale check: discard if generation is older than current
                    let current_gen = self.cache.body_vol_cache.current_generation(result.body_nid);
                    if result.generation < current_gen {
                        continue; // don't advance i — swap_remove moved last element here
                    }
                    self.cache.body_vol_cache.body_chunked_meshes.insert(result.body_nid, result.chunked);
                    self.cache.mesh_cache.insert_mesh_direct(
                        result.mesh_cache_id,
                        result.mesh,
                        result.source_hash,
                        result.lod,
                    );
                    // Field mesh changed → invalidate mesh-mod base so next fold uses fresh DC.
                    self.invalidate_mesh_mod_display(result.body_nid);
                    let bn = result.body_nid;
                    let cid = result.mesh_cache_id;
                    let needs_fold = self.body_needs_display_fold(bn);
                    if needs_fold {
                        // Always prefer full sync: stores raw base + Detail LO + mesh-mod fold
                        // + display stamp (required for CachedCpu presentation of Detail LO).
                        // Fall back to direct fold from just-inserted DC if sync cannot get raw.
                        if !self.sync_body_mesh_mod_display(bn) {
                            let raw = self
                                .cache
                                .mesh_cache
                                .get(cid)
                                .map(|c| c.cpu_mesh.to_raster_data());
                            if let Some((verts, indices)) = raw {
                                let base_key = self.mesh_mod_base_key(bn);
                                self.cache.mesh_mod_base.insert(
                                    bn,
                                    (verts.clone(), indices.clone(), base_key),
                                );
                                let (fv, fi) =
                                    self.fold_body_display_after_dc(bn, verts, indices);
                                let tri_count = (fi.len() / 3) as u32;
                                self.cache
                                    .mesh_cache
                                    .update_editable_mesh(cid, (fv, fi), tri_count);
                                let stamp = match self.scene.scene_graph.get(bn).map(|n| &n.payload)
                                {
                                    Some(core_math::scene_graph::NodePayload::SdfBody {
                                        mesh_modifiers,
                                        ..
                                    }) => self.mesh_mod_display_stamp_for(bn, mesh_modifiers),
                                    _ => 0,
                                };
                                self.cache.mesh_mod_display_stamp.insert(bn, stamp);
                                self.wire_local_cache.remove(&cid.0);
                                self.overlay_lines_fp = u64::MAX;
                            }
                        }
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                    // don't advance i — swap_remove moved last element here
                    dc_count += 1;
                    if dc_count >= MAX_DC_RESULTS_PER_FRAME { break; }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1; // still in progress
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Thread panicked or dropped sender — remove job
                    eprintln!("[DC-bg] job disconnected for {:?}", self.cache.pending_dc_jobs[i].body_nid);
                    self.cache.pending_dc_jobs.swap_remove(i);
                }
            }
        }
        self.scheduler.record(&dc_ticket, _sched_t0.elapsed().as_secs_f32() * 1000.0);
    }

    /// Update the persistent GPU resident volume for the active body.
    /// Called after all bake result polling so results are captured first.
    pub(crate) fn update_resident_volume(&mut self) {
        let vol_ticket = crate::frame_scheduler::WorkTicket {
            class: crate::frame_scheduler::WorkClass::Low,
            cost_hint_ms: 1.0,
            name: "vol_upload",
        };
        if !self.cache.use_body_vol_cache {
            return;
        }
        if !self.scheduler.can_run(&vol_ticket) {
            self.scheduler.record_skipped(&vol_ticket);
            return;
        }
        let t0 = std::time::Instant::now();
        let was_body = self.cache.body_vol_cache.resident_body_nid;
        // Always-cache: update resident volume for the active body (GPU upload).
        // NESTED SCOPE: resolve to the top-level ancestor — a nested sub-body never owns a
        // grid, so keying the resident volume on the raw nested nid uploads nothing and the
        // viewport freezes on the pre-nest volume (see active_top_body doc).
        let render_body = self.active_top_body();
        if let Some(gpu) = &self.gpu {
            let device = &gpu.device;
            let queue = &gpu.queue;
            self.cache.body_vol_cache.update_resident_volume(render_body, device, queue);
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // Gate 0 metrics: body switch time + pool event
        let switched = was_body != Some(render_body);
        if switched {
            self.perf.vol_switch_ms = ms;
            let was_hit = self.cache.body_vol_cache.resident_slot.is_some()
                && self.cache.body_vol_cache.volume_pool.find_body(render_body).is_some();
            self.perf.vol_pool_event = if was_hit { 1 } else { 2 };
            // Invalidate mesh-ready cache — the new active body renders analytically,
            // so the raster-readiness check must be recomputed excluding it.
            self.cache.cached_all_meshes_ready = None;
            self.cache.prim_buf_stale = true;
            // Trigger bake for the old body (marked dirty above)
            self.trigger_body_vol_bake();
        }
        // VRAM estimate for active body
        if let Some(slot_idx) = self.cache.body_vol_cache.resident_slot {
            if let Some(slot) = self.cache.body_vol_cache.volume_pool.get(slot_idx) {
                if let Some(ref rv) = slot.volume {
                    self.perf.vol_vram_bytes = rv.buffer_bytes;
                }
            }
        }
        if ms > 10.0 { eprintln!("[SPIKE] update_resident_volume: {:.1}ms", ms); }
        self.scheduler.record(&vol_ticket, t0.elapsed().as_secs_f32() * 1000.0);
    }
}
