//! Poll / readback / curvature results for node-graph bake.
//!
//! Pure relocation from bake_dispatch — no logic change.
//! GPU-adjacent: whole functions only — bind-group / buffer order preserved.

use eframe::wgpu;
use super::bake_dispatch::{
    spawn_graph_bake_quality_dirty_ch, InFlightCurvBake, PendingCurvBake,
};
use super::bake_maps::{
    bake_work_resolution, channels_from_graph, gate_mask_by_panel_ownership,
    graph_uses_image_texture, vertex_curv_keys_for_graph,
};
use super::packing::{final_install_res, snap_work_res, upsample_maps_selective};
use super::texture_cache::{pending_loads_for_material_selective, pending_loads_for_prim};
use super::{
    BakeChannelMask, BakedNodeMaps, GraphBakeOwner, GraphBakeQuality, GraphBakeResult,
    BAKE_WORK_RES,
};
use crate::material_textures::DEFAULT_MAP_RES;
use crate::shader_nodes::NodeGraph;
use glam::Vec3;

/// DIAGNOSTIC (`AZRA_CURV_WHY=1`): name the gate that stopped the vertex-curvature rung.
///
/// The rung has several legitimate early returns and they are indistinguishable from outside, so
/// when curvature "shows nothing" this reports WHICH one fired instead of leaving it to inference.
fn curv_why(reason: &str) {
    if std::env::var("AZRA_CURV_WHY").as_deref() == Ok("1") {
        eprintln!("[CurvWhy] vertex rung SKIPPED: {reason}");
    }
}

impl crate::AzraApp {
    /// Turn a settled GPU bake into the same `BakedNodeMaps` the CPU path produces.
    ///
    /// Split out of `try_gpu_bake_graph` so the submit half can return without blocking: this is the
    /// part that used to run after `device.poll(PollType::Wait)` on the frame thread.
    fn finish_gpu_bake_graph(
        out: crate::gpu_material_bake::BakedGpuMaps,
        graph: &NodeGraph,
        quality: GraphBakeQuality,
        res: u32,
        ms: f64,
        ch: BakeChannelMask,
    ) -> Option<BakedNodeMaps> {
        // Log only slow or forced bakes — avoid console spam during every scrub frame.
        if ms > 8.0
            || matches!(quality, GraphBakeQuality::Final)
            || std::env::var("AZRA_GPU_BAKE").as_deref() == Ok("1")
        {
            eprintln!("[GpuBake] {res}² {quality:?} — {ms:.1} ms (GPU dual-plane, async)");
        }

        let maps = BakedNodeMaps {
            // GPU bake leaves `normal` empty (see the doc comment) — nothing synthesized.
            normal_is_dx: false,
            albedo: out.albedo,
            roughness: out.roughness,
            metallic: out.metallic,
            emission: out.emission,
            // NOT zero-filled — see the doc comment above.
            normal: Vec::new(),
            bump: Vec::new(),
            displacement: Vec::new(),
            // GPU bake has no sss plane (`gpu_bake_channel_plan` rejects sss-wired graphs).
            sss_color: Vec::new(),
            // T1: dual-plane lo planes (same as CPU bake).
            albedo_lo: out.albedo_lo,
            roughness_lo: out.roughness_lo,
            metallic_lo: out.metallic_lo,
            emission_lo: out.emission_lo,
            normal_lo: Vec::new(),
            bump_lo: Vec::new(),
            displacement_lo: Vec::new(),
            sss_color_lo: Vec::new(),
        };
        // Match the CPU path's final step: bake at work res, upsample to install ceiling.
        let install = final_install_res(if graph.bake_res == 0 {
            if graph_uses_image_texture(graph) {
                DEFAULT_MAP_RES
            } else {
                BAKE_WORK_RES
            }
        } else {
            graph.bake_res
        });
        // `ch` is the dispatch mask (graph-wired ∩ F2 filter) — empty planes stay empty.
        Some(upsample_maps_selective(maps, res, install, ch))
    }
    /// Poll-phase: run the `curvature_eval` kernel (once per distinct `CurvKey`) for every parked
    /// bake, then spawn its worker.
    ///
    /// **Frame Ordering Contract:** this is called from `poll_background_tasks` — the documented
    /// legal window for GPU resource creation and dispatch — and is the ONLY place the curvature
    /// kernel runs. It must be called BEFORE `poll_graph_bakes` so a bake parked this frame
    /// starts its worker in the same poll it was dispatched in.
    ///
    /// **Never fails a bake.** No GPU, no pipeline, an unserializable body, or a failed
    /// readback all fall through to that key's grid being ABSENT, which makes `sdf_curvature_
    /// masks` take the CPU path for that node only — the rest of the bake's keys are unaffected.
    ///
    /// **Never blocks (Frame Budget Rule).** Every key's dispatch is SUBMITTED here
    /// (`encode_curvature_dispatch` returns immediately) and moves to `graph_curv_inflight`; a
    /// LATER poll finds each `MapState` settled under a non-blocking `PollType::Poll` and
    /// finishes it. At most `MAX_CURV_READBACKS_PER_FRAME` readbacks are finished per frame
    /// across ALL in-flight jobs — the repo's bake-readback convention
    /// (`MAX_CLASSIFY_FINALIZE_PER_FRAME` = 1) — but submission itself is not bounded per-key:
    /// `queue.submit` for several small compute dispatches is cheap; only the STALL a
    /// `PollType::Wait` would cause is the thing the Frame Budget Rule forbids.
    pub(crate) fn poll_graph_curvature(&mut self) {
        self.drain_curvature_readbacks();
        if self.cache.graph_curv_pending.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.cache.graph_curv_pending);
        for job in jobs {
            let mut flights = Vec::with_capacity(job.keys.len());
            for &key in &job.keys {
                match self.encode_curvature_grid_for_key(&job, key) {
                    Some(readback) => flights.push((key, Some(readback))),
                    None => flights.push((key, None)), // GPU miss for this key — CPU fallback
                }
            }
            // Every key already resolved to a GPU miss (no readback at all) — nothing to wait
            // on, spawn the worker now with whatever grids exist (none).
            if flights.iter().all(|(_, r)| r.is_none()) {
                let mut job = job;
                job.sdf.curv_caches.clear();
                spawn_graph_bake_quality_dirty_ch(
                    self.cache.graph_bake_tx.clone(),
                    job.owner, job.seq, job.graph, job.quality, job.images, job.sdf,
                    job.dirty, job.prior, job.channel_filter,
                );
                continue;
            }
            self.cache.graph_curv_inflight.push(InFlightCurvBake { job, flights, done: Vec::new() });
        }
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Finish settled curvature readbacks — at most `MAX_CURV_READBACKS_PER_FRAME` per frame,
    /// across all in-flight jobs and all their keys (Frame Budget Rule).
    ///
    /// Mirrors `poll_gpu_classify_readbacks` (`app_poll_bake.rs:1159`): one non-blocking
    /// `PollType::Poll` to let wgpu run the map callbacks, then a walk that leaves `PENDING`
    /// flights alone, drops `ERR` flights onto the CPU fallback (for that key only), and consumes
    /// a bounded number of `OK`s. A job's worker spawns once EVERY key in it has settled (or
    /// failed) — not before, since the worker must receive its full `curv_caches` set in one
    /// piece to stay byte-identical to the non-curvature spawn path.
    ///
    /// **Staleness:** a job whose `seq` is no longer the pending seq for its owner has been
    /// superseded by a newer edit. Its buffers are unmapped and the whole job DISCARDED without
    /// spawning a worker — `poll_graph_bakes` would drop that worker's result anyway
    /// (`graph_bake_pending.get(&key) != Some(&result.seq)`), so spawning it would be pure waste.
    /// Same generation test the classify path applies to its own flights.
    fn drain_curvature_readbacks(&mut self) {
        if self.cache.graph_curv_inflight.is_empty() {
            return;
        }
        // Non-blocking: services map callbacks without ever waiting on the GPU.
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.device.poll(eframe::wgpu::PollType::Poll).expect("GPU device polling failed");
        }
        const MAX_CURV_READBACKS_PER_FRAME: usize = 1;
        let mut finished = 0usize;
        let mut i = 0usize;
        while i < self.cache.graph_curv_inflight.len() {
            // SUPERSEDED: a newer bake for this owner was requested while we were in flight —
            // discard the whole job (all its flights) without spawning a worker.
            let job_owner_key = self.cache.graph_curv_inflight[i].job.owner.key();
            let job_seq = self.cache.graph_curv_inflight[i].job.seq;
            if self.cache.graph_bake_pending.get(&job_owner_key) != Some(&job_seq) {
                self.cache.graph_curv_inflight.swap_remove(i);
                continue;
            }
            // Settle as many of THIS job's flights as the per-frame budget allows.
            {
                let entry = &mut self.cache.graph_curv_inflight[i];
                let mut fi = 0usize;
                while fi < entry.flights.len() && finished < MAX_CURV_READBACKS_PER_FRAME {
                    let settled = match &entry.flights[fi].1 {
                        Some(r) => r.peek() != crate::map_state::PENDING,
                        None => true, // already-resolved GPU-miss slot
                    };
                    if !settled {
                        fi += 1;
                        continue;
                    }
                    let (key, readback) = entry.flights.remove(fi);
                    let Some(readback) = readback else { continue }; // already-resolved miss
                    finished += 1;
                    let elapsed_ms = readback.submitted.elapsed().as_secs_f64() * 1000.0;
                    // `take()` unmaps on OK and returns None on ERR — either way the buffer is
                    // released with the flight, so a failed map can never leak it.
                    match readback.take() {
                        Some(data) => {
                            entry.done.push(crate::gpu_curvature::CurvGrid {
                                res: entry.job.res, data, region: key.region, key,
                            });
                        }
                        None => {
                            eprintln!(
                                "[GpuCurvature] readback map FAILED for one key — that node \
                                 falls back to the CPU path (others unaffected)"
                            );
                        }
                    }
                    eprintln!(
                        "[GpuCurvature] {}² grid — dispatch→readback {:.1} ms (async, off the \
                         frame thread)",
                        entry.job.res, elapsed_ms
                    );
                }
            }
            if !self.cache.graph_curv_inflight[i].flights.is_empty() {
                i += 1; // still waiting on other keys for this job
                continue;
            }
            // All keys settled — spawn the worker with whichever grids resolved.
            let f = self.cache.graph_curv_inflight.swap_remove(i);
            let InFlightCurvBake { mut job, done, .. } = f;
            job.sdf.curv_caches = done;
            spawn_graph_bake_quality_dirty_ch(
                self.cache.graph_bake_tx.clone(),
                job.owner, job.seq, job.graph, job.quality, job.images, job.sdf,
                job.dirty, job.prior, job.channel_filter,
            );
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// P2 (GPU_CURVATURE_KERNEL_PLAN.md) — dispatch/refresh the ACTIVE SDF body's per-vertex
    /// curvature buffer(s), one per distinct `VertexCache`-rung `CurvKey` its material graph uses.
    ///
    /// **Frame Ordering Contract:** called from `poll_background_tasks` only. Every buffer create
    /// and dispatch here happens in the legal window; nothing in this function is reachable from
    /// a panel draw.
    ///
    /// **"Once per mesh rewrite, not per frame" (plan §6 risk 2):** the dispatch is skipped
    /// unless the body's mesh `content_gen` (or buffer generation / vert_cap / CurvKey) has
    /// changed since the buffer currently in `sdf_body_curvature` was built. Buffer generation
    /// alone is insufficient — add-prim remeshes rewrite verts into the same capacity without
    /// bumping it. A quiet body costs one HashMap lookup + a handful of integer comparisons per
    /// key per frame, zero GPU work.
    ///
    /// **Never blocks.** `encode_curvature_vertex_dispatch` submits and returns; there is nothing
    /// to read back (the destination buffer is consumed entirely on the GPU by the next draw), so
    /// there is no async drain step to pair with this — unlike the texture-rung grid path, this
    /// is a single-phase submit-and-publish.
    ///
    /// Returns `true` when a dispatch ran or shape params changed — caller should request repaint
    /// so the next frame samples the new buffer without waiting for user input.
    pub(crate) fn poll_vertex_curvature(&mut self) -> bool {
        // All SDF bodies that currently have a material + vertex-rung graph, not only the active
        // body (multi-body scenes used to leave κ dead on non-active bodies).
        let bodies: Vec<core_math::scene_graph::NodeId> = self
            .scene
            .scene_graph
            .sdf_bodies()
            .map(|(id, _)| id)
            .collect();
        let mut any = false;
        for body in bodies {
            any |= self.poll_vertex_curvature_for_body(body);
        }
        any
    }

    fn poll_vertex_curvature_for_body(&mut self, body: core_math::scene_graph::NodeId) -> bool {
        let Some(_prims) = self.scene.scene_graph.body_primitives(body) else {
            return false;
        };

        // Which material this body uses for the Vertex rung.
        let mid = self.props_material_resolve_for_body(body);
        let Some(mid) = mid else {
            curv_why("body has no material id");
            return false;
        };
        let Some(graph) = self.ui.material_node_graphs.get(&mid.0) else {
            curv_why("no node graph registered for this material");
            return false;
        };
        let keys = vertex_curv_keys_for_graph(graph);
        if keys.is_empty() {
            // No VertexCache-rung generator on this body — drop any stale entry rather than
            // leaving a buffer for a rung the user switched away from (it would never be sampled
            // again, but holding it is a silent VRAM leak across many body switches).
            if self.gpu.as_ref().map_or(false, |g| g.sdf_body_curvature.contains_key(&body)) {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.sdf_body_curvature.remove(&body);
                }
            }
            curv_why("no VertexCache-rung curvature keys in this material's graph");
            return false;
        }

        // Resolve the CURRENT mesh's (generation, content_gen, vert_cap, verts buffer, byte
        // offset) — pooled and unpooled bodies keep their vertex data in different owners
        // (plan §4.3). `content_gen` is the critical staleness key: add-prim re-meshes rewrite
        // verts without bumping buffer `generation`.
        let Some((mesh_gen, content_gen, vert_cap, vert_off_bytes)) =
            self.sdf_body_mesh_vertex_span(body)
        else {
            return false; // no resident mesh yet (not baked, or CPU-mesh fallback path)
        };
        // DO NOT DISPATCH WHILE THE MESH IS CHURNING (fix 2026-07-28, from the owner's log).
        //
        // The log showed this loop repeating EVERY FRAME on a body whose settled surface needs
        // ~64 verts:
        //     [SdfPool] trim   9993640 -> 64        (304MB freed)
        //     [SdfPool] rebuild          479.7MB verts + 359.8MB idx
        //     [GpuCurvature] vertex rung: 9993640 verts -- dispatch submitted
        //     [SPIKE] poll_background_tasks: 28-33ms
        //
        // Two separate faults compound there:
        //   1. `vert_cap` is the pool's ALLOCATED CAPACITY, not the vertex count. On a body mid-
        //      gesture that peak can be ~10M while the real surface needs 64 — a ~156,000x
        //      overshoot. The dispatch sizes its thread count AND its 4-wide output buffer from
        //      it, so each frame allocated ~160MB and traced ~10M rays for ~64 useful values.
        //   2. Re-meshing bumps `content_gen` every frame (correctly — the content really did
        //      change), so the staleness gate below always fires. Curvature then re-dispatches
        //      against the transient peak, keeping that capacity referenced.
        //
        // Gate on the SAME quiet signal the pool's own trim uses (`sparse_quiet` —
        // `app_poll_gpu.rs` arms `may_trim` with it). While a body is escalating, its vertex
        // buffer is being rebuilt underneath us, so any curvature computed now is stale before it
        // is read. Waiting costs nothing visually: the previous frame's buffer stays bound (the
        // flag is only cleared when the mesh is genuinely gone), and the dispatch runs once the
        // body settles — which is exactly when the numbers are worth computing.
        if !self.cache.body_vol_cache.sparse_quiet(body) {
            curv_why("sparse bake not quiet yet (mesh still settling)");
            return false;
        }
        if vert_cap == 0 {
            curv_why("vert_cap == 0 (no resident vertices)");
            return false;
        }

        // Only the FIRST eligible key drives the buffer today — the carry (plan §4.2) is ONE
        // scalar per vertex, so a body with two DIFFERENT VertexCache generators (two different
        // radii/regions) cannot carry both simultaneously without a second buffer + a second bind.
        // That is the honest multi-key gap: flagged, not silently overwritten-and-hidden. A second
        // key is simply not dispatched (the layer reading it finds no matching buffer and shows
        // nothing for the vertex rung, same fail-soft contract as a knob mismatch).
        let (mut key, mut out_channel) = keys[0];
        // LIVE panel knobs (Radius / Cx·Cc / Remap / Soft-Sharp / Inv / mask role) come from the
        // DOC, not only the last graph lower — so dragging R on a mask redispatches the same frame
        // without waiting on graph JSON equality (which sometimes no-op'd and left radius dead).
        let (shape_params, invert, blend, as_mask) = self.vertex_curv_shape_from_material(mid);
        if let Some((eps, ch, region)) = self.vertex_curv_live_radius_channel(mid) {
            key.eps = eps;
            key.region = region;
            out_channel = ch;
        }

        let stale = match self.gpu.as_ref().and_then(|g| g.sdf_body_curvature.get(&body)) {
            Some(slot) => {
                slot.mesh_generation != mesh_gen
                    || slot.mesh_content_gen != content_gen
                    || slot.cap != vert_cap
                    || !slot.key.matches(&key)
                    || slot.out_channel != out_channel
            }
            None => true,
        };
        // Keep base Constant on mat.albedo while a Vertex mask is active (Photoshop "below").
        if as_mask {
            let base = self.vertex_mask_base_albedo(mid);
            if let Some(base) = base {
                if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                    if (m.albedo - base).length_squared() > 1e-8 {
                        m.albedo = base;
                        self.cache.prim_buf_stale = true;
                        self.mark_dirty(crate::DirtyFlags::RENDER);
                    }
                }
            }
        }

        if !stale {
            // Refresh display shaping only (opacity/remap/invert/sharpness/blend/mask role).
            let mut dirty = false;
            if let Some(gpu) = self.gpu.as_mut() {
                if let Some(slot) = gpu.sdf_body_curvature.get_mut(&body) {
                    dirty = slot.shape_params != shape_params
                        || slot.invert != invert
                        || slot.blend != blend
                        || slot.as_mask != as_mask;
                    slot.shape_params = shape_params;
                    slot.invert = invert;
                    slot.blend = blend;
                    slot.as_mask = as_mask;
                    if dirty {
                        self.mark_dirty(crate::DirtyFlags::RENDER);
                    }
                }
            }
            return dirty;
        }

        let (bmin, bmax) = {
            let sdf = self.snapshot_sdf_field_for_material(mid);
            sdf.body_aabb(key.prim_uid)
        };
        let c = crate::gpu_curvature::CurvConstants::new(bmin, bmax, key.eps, key.band);

        if !crate::gpu_init::ensure_curvature_eval_pipeline(match self.gpu.as_mut() {
            Some(g) => g,
            None => return false,
        }) {
            return false;
        }

        // Build the mesh-curvature pipelines here — while a &mut borrow is still available and
        // BEFORE the long-lived immutable borrows (`gpu`, `src_verts`) are taken below.
        if std::env::var("AZRA_MESH_CURV").as_deref() == Ok("1") {
            if let Some(g) = self.gpu.as_mut() {
                if g.mesh_curv_pipelines.is_none() {
                    g.mesh_curv_pipelines =
                        Some(crate::gpu_curvature::MeshCurvPipelines::create(&g.device));
                }
            }
        }
        let mut prim_floats = graphics::serialize_body_for_bake_hot(&self.scene.scene_graph, body, Vec3::ZERO);
        let clip_floats = graphics::serialize_clip_curves_for_bake(&self.scene.scene_graph, body, &mut prim_floats);

        // HARD CAP (memory / device binding) — short borrow only.
        const ADJ_BYTES_PER_VERT: u64 = 32;
        const MAX_CURV_VERTS_BUDGET: u64 = 8_000_000;
        {
            let gpu = match self.gpu.as_ref() {
                Some(g) => g,
                None => return false,
            };
            if gpu.pipelines.curvature_eval_pipeline.is_none()
                || gpu.bind_groups.primary_bake_bgl.is_none()
            {
                return false;
            }
            let dev_limit =
                gpu.device.limits().max_storage_buffer_binding_size as u64 / ADJ_BYTES_PER_VERT;
            let max_curv_verts = MAX_CURV_VERTS_BUDGET.min(dev_limit);
            if vert_cap > max_curv_verts {
                eprintln!(
                    "[GpuCurvature] vertex rung SKIPPED: body={body:?} vert_cap={vert_cap} exceeds \
                     {max_curv_verts} (budget {MAX_CURV_VERTS_BUDGET}, device limit {dev_limit})"
                );
                return false;
            }
        }

        // O1 args + pooled sum pipelines (mut) — before long-lived immutable buffer borrows.
        if let Some(g) = self.gpu.as_mut() {
            let _ = crate::gpu_init::ensure_curv_dispatch_args_pipeline(g);
            let _ = crate::gpu_init::ensure_curv_sum_tile_counters_pipeline(g);
        }

        let use_mesh = std::env::var("AZRA_MESH_CURV").as_deref() == Ok("1");
        let dest = if use_mesh {
            let span = self.sdf_body_idx_span(body);
            let steps = ((key.eps / c.feature.max(1e-6)) * 8.0).round();
            let smooth_iters = steps.clamp(0.0, 16.0) as u32;
            let gp = match self.gpu.as_ref() {
                Some(g) => g,
                None => return false,
            };
            let src_verts = match self.sdf_body_verts_buffer(body) {
                Some(v) => v,
                None => return false,
            };
            match (span, self.sdf_body_idx_buffer(body), gp.mesh_curv_pipelines.as_ref()) {
                (Some((idx_cap, idx_off)), Some(idx), Some(mp)) => {
                    let tri_cap = (idx_cap / 3).max(1);
                    let out = crate::gpu_curvature::encode_mesh_curvature_dispatch(
                        &gp.device,
                        &gp.queue,
                        mp,
                        &mp.bgl,
                        src_verts,
                        vert_off_bytes,
                        idx,
                        idx_off,
                        vert_cap,
                        tri_cap,
                        smooth_iters,
                    );
                    eprintln!(
                        "[MeshCurvature] body={body:?} {vert_cap} verts {tri_cap} tris \
                         smooth={smooth_iters} — dispatch submitted"
                    );
                    out
                }
                _ => None,
            }
        } else {
            // Field path + O1: unpooled counters OR pooled tile-counter sum.
            let counters = self.sdf_body_counters_buffer(body);
            let pooled_segs = self.sdf_body_pooled_tile_counter_segs(body);
            let gp = match self.gpu.as_ref() {
                Some(g) => g,
                None => return false,
            };
            let pipeline = match gp.pipelines.curvature_eval_pipeline.as_ref() {
                Some(p) => p,
                None => return false,
            };
            let bgl = match gp.bind_groups.primary_bake_bgl.as_ref() {
                Some(b) => b,
                None => return false,
            };
            let src_verts = match self.sdf_body_verts_buffer(body) {
                Some(v) => v,
                None => return false,
            };
            let args_pipe = gp.pipelines.curv_dispatch_args_pipeline.as_ref();
            let sum_pipe = gp.pipelines.curv_sum_tile_counters_pipeline.as_ref();
            let mode = if counters.is_some() && args_pipe.is_some() {
                "indirect-live"
            } else if !pooled_segs.is_empty() && sum_pipe.is_some() && args_pipe.is_some() {
                "indirect-pooled"
            } else {
                "cap-fallback"
            };
            eprintln!(
                "[GpuCurvature] vertex rung: body={body:?} cap={vert_cap} mode={mode} — dispatch"
            );
            if mode == "indirect-pooled" {
                crate::gpu_curvature::encode_curvature_vertex_dispatch_pooled(
                    &gp.device,
                    &gp.queue,
                    pipeline,
                    bgl,
                    &gp.bake_tex_dummy,
                    &prim_floats,
                    &clip_floats,
                    src_verts,
                    vert_off_bytes,
                    vert_cap,
                    &c,
                    key.region,
                    key.grow,
                    key.contrast,
                    key.strength,
                    args_pipe,
                    sum_pipe,
                    &pooled_segs,
                )
            } else {
                crate::gpu_curvature::encode_curvature_vertex_dispatch(
                    &gp.device,
                    &gp.queue,
                    pipeline,
                    bgl,
                    &gp.bake_tex_dummy,
                    &prim_floats,
                    &clip_floats,
                    src_verts,
                    vert_off_bytes,
                    vert_cap,
                    &c,
                    key.region,
                    key.grow,
                    key.contrast,
                    key.strength,
                    counters,
                    args_pipe,
                )
            }
        };
        let Some(dest) = dest else {
            return false;
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.sdf_body_curvature.insert(
                body,
                crate::gpu_curvature::CurvVertexSlot {
                    buf: dest,
                    cap: vert_cap,
                    mesh_generation: mesh_gen,
                    mesh_content_gen: content_gen,
                    key,
                    out_channel,
                    shape_params,
                    invert,
                    blend,
                    as_mask,
                },
            );
        }
        self.mark_dirty(crate::DirtyFlags::RENDER);
        true
    }

    /// First visible Vertex-rung generator on the material's panel doc → display
    /// shape params `[remap_min, remap_max, opacity, soft_hard]` + invert + blend (0..4)
    /// + `as_mask` (Photoshop layer mask vs content).
    ///
    /// `opacity` = intensity only. `soft_hard` = power falloff (1 linear, <1 soft, >1 hard);
    /// G-buffer uses `pow(mag, soft_hard)` so soft never crushes peak white.
    ///
    /// Stack walk is **top → bottom** (panel visual order). Prefer a **mask** on a layer
    /// before that layer's content so Noise+Curv-mask wins over a lower Curv content layer.
    fn vertex_curv_shape_from_material(
        &self,
        mid: core_math::primitives::MaterialId,
    ) -> ([f32; 4], bool, u32, bool) {
        use crate::material_panel::lower::{
            channel_is_vertex_rung_only, is_vertex_curv_generator,
        };
        use crate::material_panel::model::{BlendMode, GeneratorRung, LayerContent};
        // Legacy G-buffer blend pack (bits 6..8, 0..4 only). P2 live composite is gone;
        // bake owns full MixBlend. Pack nearest legacy code for any residual flag readers.
        let blend_u = |b: BlendMode| -> u32 {
            match b {
                BlendMode::Normal => 0,
                BlendMode::Multiply | BlendMode::ColorBurn | BlendMode::LinearBurn | BlendMode::Darken => 1,
                BlendMode::Screen | BlendMode::ColorDodge | BlendMode::Lighten => 2,
                BlendMode::Overlay
                | BlendMode::SoftLight
                | BlendMode::HardLight
                | BlendMode::VividLight
                | BlendMode::LinearLight
                | BlendMode::PinLight
                | BlendMode::HardMix => 3,
                BlendMode::Add
                | BlendMode::Difference
                | BlendMode::Exclusion
                | BlendMode::Subtract
                | BlendMode::Divide => 4,
            }
        };
        let Some(doc) = self.ui.material_panel_docs.get(&mid.0) else {
            return ([0.0, 1.0, 1.0, 1.0], false, 0, false);
        };
        for ch in &doc.channels {
            // Pure-vertex showcase: force Normal so Mult-on-white doesn't look "flat mid-gray"
            // when the user never changed blend (legacy docs defaulted Mult).
            let pure_v = channel_is_vertex_rung_only(ch);
            // Top layer first (panel draws top of stack at the top of the list).
            for layer in ch.layers.iter().rev().filter(|l| l.visible) {
                // Mask before content — Noise with Curvature mask is the common recipe.
                if let Some(mask) = layer.mask.primary_content() {
                    if is_vertex_curv_generator(mask) {
                        if let LayerContent::Generator {
                            remap,
                            invert,
                            sharpness,
                            ..
                        } = mask
                        {
                            return (
                                [
                                    remap[0],
                                    remap[1],
                                    layer.opacity.clamp(0.0, 1.0),
                                    sharpness.max(1e-4),
                                ],
                                *invert,
                                0, // blend unused for mask path
                                true,
                            );
                        }
                    }
                }
                if is_vertex_curv_generator(&layer.content) {
                    if let LayerContent::Generator {
                        remap,
                        invert,
                        sharpness,
                        ..
                    } = &layer.content
                    {
                        let blend = if pure_v {
                            0 // Normal — replace with B&W κ on white paint
                        } else {
                            blend_u(layer.blend)
                        };
                        return (
                            [
                                remap[0],
                                remap[1],
                                layer.opacity.clamp(0.0, 1.0),
                                sharpness.max(1e-4),
                            ],
                            *invert,
                            blend,
                            false,
                        );
                    }
                }
                let _ = GeneratorRung::Auto;
            }
        }
        ([0.0, 1.0, 1.0, 1.0], false, 0, false)
    }

    /// Live Radius + output channel + kernel region from the panel DOC (mask preferred).
    /// Used so dragging R / Cx / Cc on a mask redispatches without waiting on graph lower.
    /// Returns `(eps, out_channel, region)` with out_channel 1=signed / 2=convex / 3=concave.
    fn vertex_curv_live_radius_channel(
        &self,
        mid: core_math::primitives::MaterialId,
    ) -> Option<(f32, u32, u32)> {
        use crate::material_panel::lower::is_vertex_curv_generator;
        use crate::material_panel::model::{GeneratorOutput, LayerContent};
        let doc = self.ui.material_panel_docs.get(&mid.0)?;
        for ch in &doc.channels {
            for layer in ch.layers.iter().rev().filter(|l| l.visible) {
                let gen = layer.mask.primary_content()
                    .filter(|m| is_vertex_curv_generator(m))
                    .or_else(|| {
                        if is_vertex_curv_generator(&layer.content) {
                            Some(&layer.content)
                        } else {
                            None
                        }
                    })?;
                if let LayerContent::Generator {
                    radius, output, ..
                } = gen
                {
                    let (ch_out, region) = match output {
                        GeneratorOutput::ConvexOnly => (2u32, 1u32),
                        GeneratorOutput::ConcaveOnly => (3u32, 2u32),
                        GeneratorOutput::Signed => (1u32, 0u32),
                    };
                    return Some((radius.max(1e-4), ch_out, region));
                }
            }
        }
        None
    }

    /// Bottom-most visible Constant colour under a Vertex-masked layer — "layers below" for
    /// Photoshop mask (mat.albedo). None if no pure constant base.
    pub(crate) fn vertex_mask_base_albedo(
        &self,
        mid: core_math::primitives::MaterialId,
    ) -> Option<glam::Vec3> {
        use crate::material_panel::lower::is_vertex_curv_generator;
        use crate::material_panel::model::{ChannelValue, LayerContent};
        let doc = self.ui.material_panel_docs.get(&mid.0)?;
        for ch in &doc.channels {
            if ch.kind != crate::material_panel::model::ChannelKind::BaseColor {
                continue;
            }
            // Find topmost layer with vertex mask; then bottom-most constant below it.
            let mut mask_li: Option<usize> = None;
            for (i, layer) in ch.layers.iter().enumerate() {
                if !layer.visible {
                    continue;
                }
                if layer.mask.primary_content()
                    .is_some_and(|m| is_vertex_curv_generator(m))
                {
                    mask_li = Some(i);
                }
            }
            let mask_li = mask_li?;
            for layer in ch.layers[..mask_li].iter().rev() {
                if !layer.visible {
                    continue;
                }
                if let LayerContent::Constant { value } = &layer.content {
                    return Some(match value {
                        ChannelValue::Color(c) => glam::Vec3::new(c[0], c[1], c[2]),
                        ChannelValue::Scalar(s) => glam::Vec3::splat(*s),
                    });
                }
            }
        }
        None
    }

    /// Resolve `(mesh_generation, content_gen, vert_cap, vert_off_bytes)` for body `nid`'s CURRENT
    /// drawable mesh — the SAME generation/offset the draw path reads, via the SAME facade
    /// (`sdf_mesh_presentation`) the draw/pick sites use, so this can never disagree with what is
    /// actually on screen. `content_gen` is `SdfBodyMeshSlot::content_gen` (bumps every mesh
    /// rewrite). `None` when the body has no `Resident` mesh (not yet baked, or drawn from a
    /// folded/PT `CachedCpu` mesh — the vertex-curvature kernel needs a `STORAGE`-usage vertex
    /// buffer, which only the resident slot/pool paths have).
    pub(crate) fn sdf_body_mesh_vertex_span(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> Option<(u64, u64, u64, u64)> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        // content_gen always lives on the per-body mesh slot (even when verts are pooled).
        let content_gen = gpu.sdf_body_mesh.get(&nid)?.content_gen;
        if pres.is_pooled() {
            let pool = gpu.sdf_mesh_pool.as_ref()?;
            let slot = pool.slot(nid)?;
            let group = pool.group_of(nid)?;
            Some((group.generation, content_gen, slot.vert_cap, slot.vert_off))
        } else {
            let slot = gpu.sdf_body_mesh.get(&nid)?;
            Some((slot.generation, content_gen, slot.vert_cap, 0))
        }
    }

    /// The `wgpu::Buffer` backing body `nid`'s CURRENT resident vertex data (pool group's `verts`
    /// for a pooled body, the slot's own `verts` otherwise) — the exact buffer
    /// `draw_sdf_pool_slot`/`draw_sdf_resident_slot` bind at vertex slot 0 (`raster_common.rs`).
    /// `None` under the same conditions as `sdf_body_mesh_vertex_span`.
    /// The `wgpu::Buffer` holding body `nid`'s CURRENT resident INDEX data — the same
    /// pooled/unpooled split `sdf_body_verts_buffer` uses, so the two always describe the same
    /// mesh. Mesh curvature (MESH_CURVATURE_PLAN M1) needs both: positions give geometry, indices
    /// give the adjacency that makes it a SURFACE rather than a point cloud.
    /// `(idx_cap, idx_off_bytes)` for body `nid`'s CURRENT resident mesh — the index-buffer
    /// counterpart of `sdf_body_mesh_vertex_span`, resolved through the same pooled/unpooled
    /// split so the two can never describe different meshes.
    ///
    /// `idx_cap` is CAPACITY, not the true index count (that lives only in the GPU-written
    /// indirect args — `resident.rs` states "NO mesh readback" as a contract). Callers must treat
    /// it as a dispatch CEILING and bounds-check, which `mesh_curvature_adj_fill` does.
    fn sdf_body_idx_span(&self, nid: core_math::scene_graph::NodeId) -> Option<(u64, u64)> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        if pres.is_pooled() {
            let pool = gpu.sdf_mesh_pool.as_ref()?;
            let slot = pool.slot(nid)?;
            Some((slot.idx_cap, slot.idx_off))
        } else {
            let slot = gpu.sdf_body_mesh.get(&nid)?;
            Some((slot.idx_cap, 0))
        }
    }

    fn sdf_body_idx_buffer(&self, nid: core_math::scene_graph::NodeId) -> Option<&wgpu::Buffer> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        if pres.is_pooled() {
            let group = gpu.sdf_mesh_pool.as_ref()?.group_of(nid)?;
            Some(&group.idx)
        } else {
            Some(&gpu.sdf_body_mesh.get(&nid)?.idx)
        }
    }

    fn sdf_body_verts_buffer(&self, nid: core_math::scene_graph::NodeId) -> Option<&wgpu::Buffer> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        if pres.is_pooled() {
            let group = gpu.sdf_mesh_pool.as_ref()?.group_of(nid)?;
            Some(&group.verts)
        } else {
            Some(&gpu.sdf_body_mesh.get(&nid)?.verts)
        }
    }

    /// DC `counters` buffer `[vert_count, index_count]` for **unpooled** resident meshes.
    /// Used by O1 indirect vertex-curvature dispatch. Pooled/tiled bodies use per-tile
    /// counters — see [`Self::sdf_body_pooled_tile_counter_segs`].
    fn sdf_body_counters_buffer(&self, nid: core_math::scene_graph::NodeId) -> Option<&wgpu::Buffer> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() || pres.is_pooled() {
            return None;
        }
        Some(&self.gpu.as_ref()?.sdf_body_mesh.get(&nid)?.counters)
    }

    /// O1 pooled: per-segment tile-counter spans for live-count sum.
    /// Empty when body is not pooled (use unpooled counters) or pool missing.
    fn sdf_body_pooled_tile_counter_segs(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> Vec<crate::gpu_curvature::PooledTileCounterSeg<'_>> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() || !pres.is_pooled() {
            return Vec::new();
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return Vec::new();
        };
        let Some(pool) = gpu.sdf_mesh_pool.as_ref() else {
            return Vec::new();
        };
        let n_seg = pool.segment_count(nid).max(1);
        let mut out = Vec::with_capacity(n_seg as usize);
        for seg in 0..n_seg {
            let Some(slot) = pool.slot_seg(nid, seg) else {
                continue;
            };
            if slot.group >= pool.group_count() {
                continue;
            }
            let grp = pool.group(slot.group);
            let n_tiles = slot.draw_entries().max(1);
            out.push(crate::gpu_curvature::PooledTileCounterSeg {
                tile_counters: &grp.tile_counters,
                args_base: slot.args_base,
                n_tiles,
            });
        }
        out
    }

    /// GPU_CURVATURE_KERNEL_PLAN.md P2 — current vertex-curvature draw state when the
    /// buffer matches this mesh's generation/cap:
    /// `(out_channel, invert, shape_params, blend, as_mask)`.
    /// `out_channel` 1=signed / 2=convex / 3=concave; `shape_params` =
    /// `[remap_min, remap_max, opacity, sharpness]`; `blend` 0=Normal .. 4=Add;
    /// `as_mask` = Photoshop layer mask (not content).
    ///
    /// Uses the SAME readiness test as `curvature_bind_group_for_body` so the instance
    /// flag is never set when the draw would bind the dummy (flag+dummy → OOB device loss).
    pub(crate) fn body_has_current_vertex_curvature(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> Option<(u32, bool, [f32; 4], u32, bool)> {
        let pres = self.sdf_mesh_presentation(nid, None);
        if !pres.is_resident() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        let slot = crate::raster_common::vertex_curv_slot_current(gpu, nid, pres.is_pooled())?;
        Some((
            slot.out_channel,
            slot.invert,
            slot.shape_params,
            slot.blend,
            slot.as_mask,
        ))
    }

    /// Encode + SUBMIT ONE `curvature_eval` for a SINGLE `CurvKey` over the job's `res²` UV grid,
    /// returning the armed (not yet settled) readback. `None` on any GPU miss, in which case the
    /// caller leaves that key's grid absent — `sdf_curvature_masks` falls back to the CPU path
    /// for exactly the nodes using that key, not the whole bake.
    ///
    /// This function does NOT wait for the result — `drain_curvature_readbacks` finishes it on a
    /// later frame. Waiting here is what froze the app.
    ///
    /// The prim stream is built with `serialize_body_for_bake_hot(sg, body, ZERO)` — the SAME
    /// serializer the production bake and the GPU classify use — so the shader's `eval_scene_at`
    /// sees the identical body-local field the CPU `SdfFieldSources` evaluates. That shared
    /// serializer IS the coordinate-frame guarantee (see `gpu_curvature.rs`'s module header).
    fn encode_curvature_grid_for_key(
        &mut self,
        job: &PendingCurvBake,
        key: crate::gpu_curvature::CurvKey,
    ) -> Option<crate::gpu_curvature::PendingCurvReadback> {
        use crate::gpu_curvature as gc;
        // The body whose prims the snapshot captured. `snapshot_sdf_field_for_material` reads
        // `active_body_nid`; serializing that same body reproduces its prim list, in order.
        let body = self.scene.active_body_nid;
        if self.scene.scene_graph.body_primitives(body).is_none() {
            return None;
        }
        // The AABB is the CPU's own `body_aabb` (already padded) — passed in, never re-derived
        // on GPU, so both consumers use provably identical bounds (plan §2.2).
        let (bmin, bmax) = job.sdf.body_aabb(key.prim_uid);
        let c = gc::CurvConstants::new(bmin, bmax, key.eps, key.band);

        if !crate::gpu_init::ensure_curvature_eval_pipeline(self.gpu.as_mut()?) {
            return None;
        }
        let mut prim_floats = graphics::serialize_body_for_bake_hot(
            &self.scene.scene_graph, body, Vec3::ZERO);
        let clip_floats = graphics::serialize_clip_curves_for_bake(
            &self.scene.scene_graph, body, &mut prim_floats);
        let prim_count = (prim_floats.len() / graphics::HOT_FLOATS_PER_PRIM) as u32;
        if prim_count == 0 {
            return None;
        }
        let n = job.res * job.res;
        let params = gc::curvature_bake_params(
            n, key.region, gc::SEED_PLANAR, 3, &c, key.grow, key.contrast, key.strength, prim_count, 0,
        );

        // Read → decide → drop borrows → dispatch. `wgpu::ComputePipeline` is not `Clone`, so
        // the pipeline/BGL/texture are borrowed together from one shared reference.
        let gpu = self.gpu.as_ref()?;
        let pipeline = gpu.pipelines.curvature_eval_pipeline.as_ref()?;
        let bgl = gpu.bind_groups.primary_bake_bgl.as_ref()?;
        // The bake's 1³ R32Float storage-texture dummy: `output_tex` (@binding 3) must be bound
        // or the pipeline layout is invalid, even though `curvature_eval` never stores to it.
        let tex_dummy = &gpu.bake_tex_dummy;
        // Submits and returns — no PollType::Wait, no recv. The readback settles across frames.
        let readback = gc::encode_curvature_dispatch(
            &gpu.device, &gpu.queue, pipeline, bgl, tex_dummy,
            gc::CurvDispatch {
                prim_floats: &prim_floats,
                clip_floats: &clip_floats,
                params,
                n,
                positions: &[],
            },
        )?;
        eprintln!("[GpuCurvature] {}² grid, {} prims — dispatch submitted", job.res, prim_count);
        Some(readback)
    }

    /// Poll-phase: convert SETTLED GPU material bakes into ordinary `GraphBakeResult`s.
    ///
    /// This is the drain half of the fix for the frame-thread `PollType::Wait` (pillar review
    /// 2026-07-30). Non-blocking by construction: `peek()` reads an atomic, and a flight that is
    /// still `PENDING` is simply left in the list for a later frame. Nothing here waits on the GPU —
    /// `wgpu`'s own `PollType::Poll` (already driven elsewhere each frame) advances the mapping.
    fn drain_inflight_gpu_bakes(&mut self) {
        const MAX_GPU_BAKE_FINALIZATIONS_PER_FRAME: usize = 1;
        use crate::gpu_ctx::map_state::PENDING;
        if self.cache.graph_bake_gpu_inflight.is_empty() {
            return;
        }
        // Take the settled ones out; leave PENDING flights in place.
        let mut still_pending = Vec::new();
        let mut finalized = 0;
        let flights = std::mem::take(&mut self.cache.graph_bake_gpu_inflight);
        for f in flights {
            if finalized >= MAX_GPU_BAKE_FINALIZATIONS_PER_FRAME || f.pending.peek() == PENDING {
                still_pending.push(f);
                continue;
            }
            finalized += 1;
            let tx = self.cache.graph_bake_tx.clone();
            rayon::spawn(move || {
                let ms = f.submitted.elapsed().as_secs_f64() * 1000.0;
                let res = f.work_res;
                let channels = f.channels;
                let maps = match f.pending.take() {
                    Some(out) => {
                        Self::finish_gpu_bake_graph(out, &f.graph, f.quality, res, ms, channels)
                    }
                    None => None,
                };
                let _ = tx.send(GraphBakeResult {
                    owner: f.owner,
                    seq: f.seq,
                    maps,
                    channels,
                    quality: f.quality,
                    work_res: f.work_res,
                });
            });
        }
        self.cache.graph_bake_gpu_inflight = still_pending;
    }

    /// Poll-phase: apply finished background bakes → material_map_loads (then upload this frame).
    pub(crate) fn poll_graph_bakes(&mut self) {
        self.poll_node_gpu_previews();
        self.drain_inflight_gpu_bakes();
        // Catch any live request that was marked dirty with no in-flight bake.
        self.flush_live_graph_bake();
        const MAX_GRAPH_BAKE_RESULTS_PER_FRAME: usize = 2;
        for _ in 0..MAX_GRAPH_BAKE_RESULTS_PER_FRAME {
            let result = match self.cache.graph_bake_rx.try_recv() {
                Ok(r) => r,
                Err(_) => break,
            };
            let key = result.owner.key();
            if self.cache.graph_bake_pending.get(&key) != Some(&result.seq) {
                continue; // superseded by a newer bake request
            }
            self.cache.graph_bake_pending.remove(&key);
            self.cache.graph_live_in_flight = false;
            let Some(maps) = result.maps else {
                self.toast("Procedural bake failed (no MaterialOutput or cycle)");
                // Still try to catch up if knobs changed during the failed bake.
                self.flush_live_graph_bake();
                continue;
            };
            match result.owner {
                GraphBakeOwner::Material(mid) => {
                    let mid = core_math::primitives::MaterialId(mid);
                    // Install only channels that were **actually baked** (live = primary only).
                    // Intersect with current wiring so unplugged sockets don't re-upload.
                    let wired = self
                        .ui
                        .material_node_graphs
                        .get(&mid.0)
                        .map(channels_from_graph)
                        .unwrap_or(result.channels);
                    let ch = BakeChannelMask {
                        albedo: wired.albedo && result.channels.albedo,
                        roughness: wired.roughness && result.channels.roughness,
                        metallic: wired.metallic && result.channels.metallic,
                        emission: wired.emission && result.channels.emission,
                        normal: wired.normal && result.channels.normal,
                        bump: wired.bump && result.channels.bump,
                        displacement: wired.displacement && result.channels.displacement,
                        sss_color: wired.sss_color && result.channels.sss_color,
                    };
                    // P2 OWNERSHIP GATE (spec C1 — the forbidden-state fix):
                    // a channel the PANEL owns (doc state = Layered, i.e. a
                    // hand-loaded image) is never clobbered by a graph-bake
                    // install. GraphDriven channels install as before; the
                    // skipped channels surface as a "conflict" badge in the
                    // panel (mp_bake_conflicts). Absent doc = no gate (legacy
                    // behavior, byte-identical).
                    // P3: panel-GENERATED subgraph installs are the panel's own
                    // composite — they pass the gate (no conflict).
                    let panel_gen = self
                        .ui
                        .material_node_graphs
                        .get(&mid.0)
                        .map(crate::material_panel::lower::panel_generated_mask)
                        .unwrap_or(0);
                    let (ch, conflicts) = gate_mask_by_panel_ownership(
                        ch,
                        self.ui.material_panel_docs.get(&mid.0),
                        panel_gen,
                    );
                    if conflicts != 0 {
                        self.ui.mp_bake_conflicts.insert(mid.0, conflicts);
                    } else {
                        self.ui.mp_bake_conflicts.remove(&mid.0);
                    }
                    // INFINITE-BAKE-LOOP FIX (2026-07-28). `ch` still contains channels whose
                    // stack is pure Vertex-rung (`channel_skip_planar_map_install`), and installing
                    // a texture map for those started a self-sustaining cycle:
                    //   bake lands → installs a map on a Vertex-rung channel
                    //     → next panel draw, `mp_ensure_vertex_curvature_path` sees a "stale planar
                    //       map", re-lowers and CLEARS it (that is its whole job)
                    //         → material is dirty again → another Final bake → repeat.
                    // Measured with the bake probe: 418 consecutive res=1024 bakes, ~12.7 s each
                    // (~55 min of work) all carrying the IDENTICAL curvature key — nothing had
                    // changed between them. The comment below correctly says a Final result does
                    // not re-enqueue ITSELF; the loop was never that, it was these two systems
                    // undoing each other's writes every frame.
                    // Fixed at the SOURCE: never install a map the Vertex path is contractually
                    // required to clear. `mp_clear_vertex_rung_texture_maps` then finds nothing to
                    // do, `mp_ensure_vertex_curvature_path` reports no work, and the cycle cannot
                    // start. Channels with real texture content (Noise/Image/Texture-rung) are
                    // untouched — including a Noise layer masked by vertex curvature, which still
                    // needs its map.
                    let mut ch = ch;
                    if let Some(doc) = self.ui.material_panel_docs.get(&mid.0) {
                        use crate::material_panel::edit::MASK_KINDS;
                        let slots: [&mut bool; 8] = [
                            &mut ch.albedo,
                            &mut ch.roughness,
                            &mut ch.metallic,
                            &mut ch.emission,
                            &mut ch.normal,
                            &mut ch.bump,
                            &mut ch.displacement,
                            &mut ch.sss_color,
                        ];
                        for (on, kind) in slots.into_iter().zip(MASK_KINDS) {
                            if *on
                                && doc.channels.iter().any(|c| {
                                    c.kind == kind
                                        && crate::material_panel::lower::channel_skip_planar_map_install(c)
                                })
                            {
                                *on = false;
                            }
                        }
                    }
                    // O4: cache install-res maps as composite base for partial dirty rects.
                    // Only Final (or Live already at target) is a useful base — soft Live@256
                    // would poison a later 2K Final composite.
                    if result.quality == GraphBakeQuality::Final
                        || result.work_res
                            >= final_install_res(
                                self.ui
                                    .material_panel_docs
                                    .get(&mid.0)
                                    .map(|d| d.bake_res)
                                    .or_else(|| {
                                        self.ui.material_node_graphs.get(&mid.0).map(|g| g.bake_res)
                                    })
                                    .unwrap_or(BAKE_WORK_RES),
                            )
                    {
                        self.cache
                            .graph_bake_map_cache
                            .insert(result.owner.key(), maps.clone());
                    }
                    self.cache
                        .material_map_loads
                        .extend(pending_loads_for_material_selective(mid, maps, ch));
                    self.cache.prim_buf_stale = true;
                    self.outliner.outliner_dirty = true;
                    // Live→Final only when Live was interactive-capped BELOW the panel
                    // Bake N² target (e.g. Live@256, target 1024). If already at target
                    // (Bake 256² → Live@256), skip — a second 1024² bake was the lag +
                    // stack-pressure source (owner log: 63ms then 1085ms then overflow).
                    // Panel `doc.bake_res` is authoritative when present (store graph can lag).
                    if result.quality == GraphBakeQuality::Live
                        && !self.cache.graph_live_dirty
                        && result.work_res > 0
                    {
                        let target = self
                            .ui
                            .material_panel_docs
                            .get(&mid.0)
                            .map(|d| {
                                snap_work_res(crate::material_panel::model::snap_panel_bake_res(
                                    d.bake_res,
                                ))
                            })
                            .or_else(|| {
                                self.ui
                                    .material_node_graphs
                                    .get(&mid.0)
                                    .map(bake_work_resolution)
                            })
                            .unwrap_or(BAKE_WORK_RES);
                        if result.work_res < target {
                            self.enqueue_material_node_graph_bake_quality(
                                mid,
                                GraphBakeQuality::Final,
                            );
                        }
                    }
                }
                GraphBakeOwner::Prim(uid) => {
                    let body_nid = core_math::scene_graph::NodeId(0);
                    // O4: cache Final (or Live-at-target) as composite base.
                    if result.quality == GraphBakeQuality::Final
                        || self
                            .ui
                            .node_graphs
                            .get(&uid)
                            .is_some_and(|g| result.work_res >= bake_work_resolution(g))
                    {
                        self.cache
                            .graph_bake_map_cache
                            .insert(result.owner.key(), maps.clone());
                    }
                    self.cache
                        .material_map_loads
                        .extend(pending_loads_for_prim(body_nid, uid, maps));
                    if result.quality == GraphBakeQuality::Live
                        && !self.cache.graph_live_dirty
                        && result.work_res > 0
                    {
                        if let Some(g) = self.ui.node_graphs.get(&uid) {
                            let target = bake_work_resolution(g);
                            if result.work_res < target {
                                self.enqueue_node_graph_bake_quality(uid, GraphBakeQuality::Final);
                            }
                        }
                    }
                }
            }
            self.mark_dirty(crate::DirtyFlags::RENDER);
            // Knobs moved while this bake ran → kick the next live preview immediately.
            self.flush_live_graph_bake();
        }
    }
}

