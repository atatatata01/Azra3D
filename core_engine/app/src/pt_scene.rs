// ============================================================
// Triangle-PT scene sync (Phase 2 path tracer)
// ============================================================
//
// Keeps the PT acceleration structures in lockstep with the scene:
//   gather instances → request stale BLAS builds (background) → poll →
//   assemble global arrays → upload SSBOs.
//
// Runs ONLY from poll_background_tasks (Frame Ordering Contract: all buffer
// creation happens here, never in panels or render) and ONLY while PathTrace
// shading is active (zero cost otherwise). Every buffer (re)creation bumps
// `pt_scene_generation`; viewport_render invalidates `pt_bg` on mismatch.
//
// The instance walk mirrors raster_gbuffer's Phase-1 enumeration (SdfBody /
// ImportedMesh / VoxelBody / PolygonalBody / NurbsBody, same transforms) minus
// the render-only concerns (selection bits, grab flags, resident-pool draws).
// TODO(unify): extract one shared gather with raster_gbuffer once this settles
// — divergence here means PT and raster disagree about what exists.

use core_math::scene_graph::NodePayload;
use eframe::wgpu;
use render::pt_accel::PtInstanceSource;
use crate::BODY_LOCAL_PRIMS;

/// Counts uniform for the traversal shader (binding 47). 32 B.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PtMetaGpu {
    pub tlas_count: u32,
    pub instance_count: u32,
    pub blas_count: u32,
    pub tri_count: u32,
    pub vert_count: u32,
    pub material_count: u32,
    pub _pad: [u32; 2],
}

/// Grow-only SSBO/uniform upload; every realloc bumps `generation` (bind-group
/// invalidation — generation counters over timing tricks).
///
/// Minimum size is **at least one `T`**, never a bare 16 B stub. An empty array
/// still allocates `size_of::<T>()` so wgpu validation passes when the shader
/// declares `array<T>` / a uniform of type `T` (was: 16 B buffer vs 32 B BvhNode
/// → device lost when materials/scene refit hit empty TLAS).
///
/// `extra_usage` is added to the base usages (only the vertex buffer passes
/// `BLAS_INPUT`, and only when the device has native RT). A live buffer missing
/// it is reallocated like an undersized one.
fn upload_pod<T: bytemuck::Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<wgpu::Buffer>,
    generation: &mut u64,
    label: &str,
    data: &[T],
    extra_usage: wgpu::BufferUsages,
) {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let elem = (std::mem::size_of::<T>().max(16)) as u64;
    let size = (bytes.len() as u64).max(elem);
    // Grow-only: compare against the LIVE buffer size (not a stale capacity field).
    // A 4 B placeholder + 8 MB write is the PathTrace switch device-loss class
    // (visdump: "Copy of 0..8388608 … Destination buffer of size 4").
    let needs_new = slot.as_ref().map_or(true, |b| b.size() < size || !b.usage().contains(extra_usage));
    if needs_new {
        let alloc = size.next_power_of_two().max(size).max(4);
        *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: alloc,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST | extra_usage,
            mapped_at_creation: false,
        }));
        *generation += 1;
    }
    let buf = match slot.as_ref() {
        Some(b) => b,
        None => return,
    };
    if !bytes.is_empty() {
        if (bytes.len() as u64) > buf.size() {
            eprintln!(
                "[PtUpload] SKIP {label}: write {}B into buffer {}B (grow failed?)",
                bytes.len(),
                buf.size()
            );
            return;
        }
        queue.write_buffer(buf, 0, bytes);
    } else {
        // Keep a zeroed first element so OOB-safe count gates never read garbage.
        let n = (elem as usize).min(buf.size() as usize);
        if n == 0 {
            return;
        }
        let zero = vec![0u8; n];
        queue.write_buffer(buf, 0, &zero);
    }
}

impl super::AzraApp {
    /// Reset every PT cache/signature tied to MeshCacheId ownership. Call at
    /// each scene/project replacement before new meshes are restored, because
    /// MeshCache IDs and content stamps restart and may alias the prior scene.
    pub(crate) fn reset_pt_accel_state(&mut self) {
        self.cache.pt_accel = render::pt_accel::PtAccelCache::new();
        self.cache.pt_last_scene_hash = 0;
        self.cache.pt_instance_count = 0;
        self.cache.pt_last_instance_ids.clear();
        self.cache.pt_last_meta = PtMetaGpu {
            tlas_count: 0,
            instance_count: 0,
            blas_count: 0,
            tri_count: 0,
            vert_count: 0,
            material_count: 0,
            _pad: [0; 2],
        };
        self.cache.pt_sdf_snap_hash.clear();
        self.cache.pt_sdf_snap_seen.clear();
        self.cache.pt_sdf_snap_pending.clear();
        self.render.pt_accumulated_frames = 0;
        self.render.pt_converged = false;
        self.render.pt_was_live_hybrid = false;
        self.render.pt_pending_soft_hybrid_seed = false;
        // MeshCache IDs restart with the new scene; no native BLAS may survive it.
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(native) = gpu.native_rt.as_mut() {
                native.clear();
            }
            gpu.native_rt_status = app_gpu::native_rt::NativeRtStatus::Empty;
        }
    }

    /// Hand the just-uploaded PT geometry to the hardware ray-query cache. Instances
    /// are the canonical `PtInstanceGpu` order (custom index = PT instance index);
    /// the transform epoch is a pose signature over model/offsets only, so material
    /// edits never invalidate an acceleration structure. A scene the native path
    /// rejects clears it — software traversal stays the PT backend (fails closed).
    fn sync_native_rt_scene(&mut self, instances: &[render::pt_accel::PtInstanceGpu], meta: PtMetaGpu) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let geometry = gpu.native_rt_geometry_epoch;
        let (Some(native), Some(vertices), Some(triangles)) =
            (gpu.native_rt.as_mut(), gpu.buffers.pt_vert_buffer.as_ref(), gpu.buffers.pt_tri_buffer.as_ref())
        else { return };
        let native_instances: Vec<app_gpu::native_rt::NativeInstance> =
            instances.iter().map(app_gpu::native_rt::NativeInstance::from).collect();
        let transforms = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for instance in &native_instances {
                for column in instance.model { for v in column { v.to_bits().hash(&mut h); } }
                instance.tri_off.hash(&mut h);
                instance.vert_off.hash(&mut h);
            }
            h.finish()
        };
        let result = if native_instances.is_empty() || meta.tri_count == 0 {
            native.clear();
            Ok(())
        } else {
            native.set_scene(
                vertices, triangles, meta.vert_count, meta.tri_count, &native_instances,
                app_gpu::native_rt::NativeRtEpochs { geometry, transforms },
            )
        };
        if let Err(error) = result {
            eprintln!("[RTX] scene rejected, software traversal stays active: {error}");
            native.clear();
        }
    }

    /// True when the active body is an SDF body currently being shaped / dirty
    /// (gizmo episode, tool busy, volume not quiet, dirty bake, or snap settle window).
    /// Used by **A** (Hybrid live feedback) and **B** (draft mesh settle).
    pub(crate) fn pt_sdf_edit_episode_active(&self) -> bool {
        use core_math::scene_graph::NodePayload;
        let nid = self.scene.active_body_nid;
        let is_sdf = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        );
        if !is_sdf {
            return false;
        }
        if self.interaction_episode.active {
            return true;
        }
        if self.tool.current.is_busy() || self.sculpt.stroke_active {
            return true;
        }
        if self.cache.body_vol_cache.dirty_bodies.contains(&nid) {
            return true;
        }
        if !self.cache.body_vol_cache.sparse_quiet(nid) {
            return true;
        }
        // Still inside settle window after a hash/res change (shape or Grid resolution).
        if let Some((seen_h, seen_r, t0)) = self.cache.pt_sdf_snap_seen.get(&nid) {
            let cur = self
                .cache
                .body_vol_cache
                .pt_sparse_snapshot_src(nid)
                .or_else(|| self.cache.body_vol_cache.pt_field_snapshot_src(nid))
                .map(|(h, r, ..)| (h, r));
            if cur == Some((*seen_h, *seen_r)) {
                let settle = self.pt_snap_settle_for(nid);
                if t0.elapsed() < settle {
                    return true;
                }
            } else if cur.is_some() {
                // Hash or bake resolution moved — still editing / re-tessellating
                return true;
            }
        }
        false
    }

    /// **A** gate: PathTrace should yield to Hybrid this frame for live SDF feedback.
    ///
    /// M1.c also yields during any undo **episode** (material panel scrub, gizmo drag that
    /// opened `begin_episode`): raster/Hybrid covers the drag; PT restarts once on
    /// `commit_episode` / release so accumulation is not wiped every slider tick.
    pub(crate) fn pt_live_hybrid_feedback_active(&self) -> bool {
        if self.render.shading_mode != crate::ShadingMode::PathTrace {
            return false;
        }
        // M1.c — panel/material scrub (and other begin_episode drags): live raster cover.
        if self.has_active_episode() {
            return true;
        }
        self.render.pt_live_hybrid_edit && self.pt_sdf_edit_episode_active()
    }

    /// Settle duration for PT SDF snapshots. Active body + draft mesh (B) → shorter window.
    pub(crate) fn pt_snap_settle_for(&self, nid: core_math::scene_graph::NodeId) -> std::time::Duration {
        if self.render.pt_live_draft_mesh && nid == self.scene.active_body_nid {
            let ms = self.render.pt_live_draft_settle_ms.max(16) as u64;
            std::time::Duration::from_millis(ms)
        } else {
            crate::cache_state::PT_SNAP_SETTLE
        }
    }

    /// Enumerate every triangle body the PT should trace this frame.
    /// Transform construction matches raster_gbuffer exactly (SdfBody =
    /// effective_body_transform + rigid-offset; everything else = world_transform).
    /// Also returns one RasterMaterial per instance (material_base = slot),
    /// mirroring the raster material-selection rules (SdfBody = last additive
    /// prim's material; every other body = its body material).
    /// PT sparse-SDF snapshot pump — see the call site comment in `update_pt_accel`.
    /// POLL PHASE ONLY (creates buffers + dispatches; Frame Ordering Contract).
    fn pump_pt_sdf_snapshots(&mut self) {
        use core_math::scene_graph::NodePayload;
        // 1) Land completed readbacks (drop stale ones whose hash/res moved while in flight —
        //    the next pump re-kicks at the new pair).
        let mut i = 0;
        while i < self.cache.pt_sdf_snap_pending.len() {
            if !self.cache.pt_sdf_snap_pending[i].4.ready() { i += 1; continue; }
            let (nid, cid, hash, snap_res, pending) = self.cache.pt_sdf_snap_pending.swap_remove(i);
            let t_take = std::time::Instant::now();
            let (verts, indices) = pending.take();
            let take_ms = t_take.elapsed().as_secs_f64() * 1000.0;
            let cur = self.cache.body_vol_cache.pt_sparse_snapshot_src(nid)
                .or_else(|| self.cache.body_vol_cache.pt_field_snapshot_src(nid))
                .map(|(h, r, ..)| (h, r));
            if cur == Some((hash, snap_res)) && !indices.is_empty() {
                let tris = indices.len() as u32 / 3;
                let nverts = verts.len();
                let t_upd = std::time::Instant::now();
                // Hard cap: multi-million-tri PT BLAS at 2048³ device-loses. Drop oversize.
                const PT_SNAP_MAX_TRIS: u32 = 1_200_000;
                if tris > PT_SNAP_MAX_TRIS {
                    eprintln!(
                        "[PtSnap] body={nid:?} REJECTED tris={tris} > {PT_SNAP_MAX_TRIS} \
                         (raise Grid down or enable more LOD) — PT skips this body"
                    );
                    self.toast(&format!(
                        "PT: body mesh too dense ({tris} tris) — skipped (try Grid ≤1024)"
                    ));
                    // Mark seen so we don't re-kick every poll.
                    self.cache.pt_sdf_snap_hash.insert(nid, (hash, snap_res));
                    continue;
                }
                // PT BLAS wants RAW mesh only. Quality fold on large snaps freezes the UI
                // and is display-only — never fold into mesh_cache from the PT pump.
                self.cache
                    .mesh_cache
                    .update_editable_mesh(cid, (verts, indices), tris);
                eprintln!(
                    "[PtSnap] body={nid:?} LANDED verts={nverts} tris={tris} res={snap_res}³ \
                     take={take_ms:.1}ms update={:.1}ms (hash={hash:016x}) | {}",
                    t_upd.elapsed().as_secs_f64() * 1000.0,
                    crate::profiling::perf_stamp()
                );
                self.cache.pt_sdf_snap_hash.insert(nid, (hash, snap_res));
            } else {
                eprintln!(
                    "[PtSnap] body={nid:?} DROPPED (stale={} empty={}) — re-kicks at current hash/res",
                    cur != Some((hash, snap_res)),
                    indices.is_empty()
                );
            }
        }
        // 2) Kick at most ONE new snapshot per poll: the temp mesh dispatch is ~10ms GPU at 1024
        //    and the readback copies are surface-proportional (100s of MB worst case) — bound it.
        if !self.cache.pt_sdf_snap_pending.is_empty() { return; }
        // Precompute before gpu mut borrow (A/B helpers touch scene + cache, not gpu).
        let active_nid = self.scene.active_body_nid;
        let draft_on = self.render.pt_live_draft_mesh;
        let edit_episode = self.pt_sdf_edit_episode_active();
        let draft_settle = self.pt_snap_settle_for(active_nid);
        let full_settle = crate::cache_state::PT_SNAP_SETTLE;
        let dc_resident = self.cache.dc_route_is_resident();
        let mut nids: Vec<core_math::scene_graph::NodeId> = self
            .scene
            .scene_graph
            .all_nodes()
            .filter(|(_, n)| n.visible && matches!(n.payload, NodePayload::SdfBody { .. }))
            .filter(|(nid, _)| self.scene.scene_graph.parent_body(*nid).is_none())
            .map(|(nid, _)| nid)
            .collect();
        if draft_on {
            nids.sort_by_key(|nid| if *nid == active_nid { 0u8 } else { 1u8 });
        }

        let Some(g) = self.gpu.as_mut() else { return; };
        let Some(m) = self.cache.sdf_mesher.as_ref() else { return; };
        // Collect the candidate first (immutable pass), then dispatch (mutable).
        // `sparse`: brick-resident (≥512) — temp-slot mesh dispatch from the body's atlas.
        // `!sparse`: FIELD-DIRECT (≤256) with the resident GPU display ON — dispatch_grid_dc_async
        // meshes into the display slot and returns, so mesh_cache (PT's BLAS source) never
        // updates and prim edits FROZE in PT (owner repro 2026-07-16 at 128³). Fix: copy the
        // DISPLAY slot's own mesh back — it already holds the current geometry, body-local, with
        // coherent counters (unpooled per-body slot) — no extra mesh dispatch at all.
        let mut candidate: Option<(core_math::scene_graph::NodeId, u64, u32, glam::Vec3, glam::Vec3, bool)> = None;
        for nid in nids {
            let sparse_src = self.cache.body_vol_cache.pt_sparse_snapshot_src(nid);
            let sparse = sparse_src.is_some();
            let src = sparse_src.or_else(|| {
                // Field-direct arm only when the resident GPU display owns meshing (otherwise
                // the normal CPU-DC pipeline updates mesh_cache itself — don't fight it).
                if !dc_resident { return None; } // GATE 3
                self.cache.body_vol_cache.pt_field_snapshot_src(nid)
            });
            let Some((hash, res, gmin, gmax)) = src else { continue };
            // (hash, res) — res-only Grid bumps must re-snapshot (content hash is blind to res).
            if self.cache.pt_sdf_snap_hash.get(&nid) == Some(&(hash, res)) { continue; }
            // QUIET-GATE: skip mid-escalation spam. **B draft + active body:** allow while
            // editing so draft mesh can land during the gesture (still rate-limited to 1/poll).
            let draft_active = draft_on && nid == active_nid && edit_episode;
            if !draft_active && !self.cache.body_vol_cache.sparse_quiet(nid) { continue; }
            // SETTLE-GATE: (hash, res) must hold still. Active body + draft uses shorter settle (B).
            let settle = if draft_on && nid == active_nid { draft_settle } else { full_settle };
            match self.cache.pt_sdf_snap_seen.get(&nid) {
                Some((h, r, t)) if *h == hash && *r == res => {
                    if t.elapsed() < settle { continue; }
                }
                _ => {
                    self.cache.pt_sdf_snap_seen.insert(
                        nid,
                        (hash, res, std::time::Instant::now()),
                    );
                    continue;
                }
            }
            if sparse {
                // Atlas identity — the same gate every sparse mesh dispatch uses.
                let atlas_ok = g.sdf_brick_atlases.get(&nid)
                    .map_or(false, |a| a.body_nid == Some(nid) && a.res == res);
                if !atlas_ok { continue; }
            } else {
                // The display slot must exist, match the entry's res, and be a plain per-body
                // monolithic mesh (pooled/tiled slots keep counters in pool regions; an LOD'd
                // slot holds a coarsened mesh — neither is a valid PT source).
                let slot_ok = g.sdf_body_mesh.get(&nid)
                    .map_or(false, |s| crate::mesh_presentation::display_slot_snapshot_ok(s, res));
                if !slot_ok { continue; }
            }
            candidate = Some((nid, hash, res, gmin, gmax, sparse));
            break;
        }
        let Some((nid, hash, res, gmin, gmax, sparse)) = candidate else { return; };
        // Ensure the body has a mesh_cache id (sparse bodies may never have been CPU-meshed).
        let cid = {
            let existing = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::SdfBody { mesh_cache_id, .. } => *mesh_cache_id,
                _ => None,
            });
            match existing {
                Some(c) => c,
                None => {
                    let c = self.cache.mesh_cache.alloc_id();
                    if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody { mesh_cache_id, .. } = &mut n.payload {
                            *mesh_cache_id = Some(c);
                        }
                    }
                    c
                }
            }
        };
        // TEMPORARY slot: monolithic mesh from the body's atlas — the display slot (tiled,
        // per-tile counters/regions) is never touched, and the temp slot's counters are coherent
        // for the plain 3-buffer readback. Uses the WINDOWED monolithic entry (surface-brick
        // bbox), NOT the full-grid LOD dispatch — the first cut dispatched 1023³ ≈ 1.07B threads
        // per pass (owner log: 26ms GPU each, classify ballooned to 265ms from queue contention).
        // pool: None → unpooled slot buffers. Dropped at scope end (wgpu refcount frees).
        // GATE 4: the dispatch machinery is the SHARED materialize job (export uses the same fn),
        // so PT snapshots and export read back the same GPU DC output the display shows.
        use crate::mesh_presentation::{dispatch_sdf_display_readback, SdfDisplayReadbackSrc};
        // PT BLAS must not ingest full L0 surface at 1024³/2048³ — multi-body uploads
        // device-lose (VRAM). Coarsen: 1024→L1 (~¼ surface), 2048→L2 (~1/16).
        let lod_level = if res >= 2048 {
            2
        } else if res >= 1024 {
            1
        } else {
            0
        };
        let src = if sparse {
            let atlas = g.sdf_brick_atlases.get(&nid).expect("atlas_ok checked");
            SdfDisplayReadbackSrc::SparseAtlas {
                atlas,
                res,
                gmin,
                gmax,
                nid,
                lod_level,
            }
        } else {
            // Field-direct: the display slot IS the current mesh — copy it, no dispatch.
            let _ = (gmin, gmax); // sparse-arm-only inputs
            SdfDisplayReadbackSrc::DisplaySlot(g.sdf_body_mesh.get(&nid).expect("slot_ok checked"))
        };
        let pending = dispatch_sdf_display_readback(&g.device, &g.queue, m, src);
        self.cache.pt_sdf_snap_pending.push((nid, cid, hash, res, pending));
        eprintln!(
            "[PtSnap] body={nid:?} res={res}³ lod={lod_level} {} snapshot dispatched (hash={hash:016x}) | {}",
            if sparse { "sparse" } else { "field-direct slot-copy" },
            crate::profiling::perf_stamp()
        );
    }

    /// Invalidate PT mesh snapshot for a body so the next pump re-reads the GPU mesh.
    /// Call when bake resolution changes without a content-hash change (Grid picker).
    pub(crate) fn invalidate_pt_sdf_snapshot(&mut self, nid: core_math::scene_graph::NodeId) {
        self.cache.pt_sdf_snap_hash.remove(&nid);
        self.cache.pt_sdf_snap_seen.remove(&nid);
        self.cache.pt_sdf_snap_pending.retain(|(n, ..)| *n != nid);
    }

    pub(crate) fn gather_pt_instances(&self) -> (Vec<PtInstanceSource>, Vec<render::mesh_gpu::RasterMaterial>) {
        use render::mesh_gpu::RasterMaterial;
        let mut out = Vec::new();
        let mut mats: Vec<RasterMaterial> = Vec::new();
        let mut push = |out: &mut Vec<PtInstanceSource>, mats: &mut Vec<RasterMaterial>,
                        cache_id: core_math::scene_graph::MeshCacheId, model: glam::Mat4,
                        pbr: &core_math::primitives::PbrMaterial| {
            let slot = mats.len() as u32;
            mats.push(RasterMaterial::from_pbr(pbr));
            out.push(PtInstanceSource {
                cache_id,
                model,
                material_base: slot,
                material_id_remap: None,
            });
        };
        // Isolation: raster draws non-isolated bodies as ghosts; PT simply skips
        // them (a ghost has no meaning in a converged path-traced frame).
        let iso = self.scene.isolated_body_nid;

        // AZRA_MASTER_ROADMAP B.2: walk the same structural draw stream as raster (B.1).
        // Camera-only frames reuse candidates; fall back to all_nodes if stream empty/stale.
        let use_stream = !self.draw_stream_stale && !self.draw_stream.is_empty();
        let mut visit = |nid: core_math::scene_graph::NodeId, node: &core_math::scene_graph::SceneNode| {
            if !node.visible { return; }
            if let Some(iso_nid) = iso {
                if nid != iso_nid && !self.scene.scene_graph.is_descendant_of(nid, iso_nid) { return; }
            }
            match &node.payload {
                NodePayload::SdfBody { mesh_cache_id, origin, body_rotation, body_scale, primitives, material: body_mat, .. } => {
                    if !self.scene.scene_graph.body_has_renderable_content(nid) { return; }
                    if self.scene.scene_graph.parent_body(nid).is_some() { return; }
                    let Some(cid) = *mesh_cache_id else { return; };
                    // GATE 4 facade: PT needs the CPU-materialized copy specifically (BLAS source) —
                    // a resident body qualifies once its snapshot has landed.
                    if !self.sdf_cpu_mesh_present(cid) { return; }
                    let mut model = if BODY_LOCAL_PRIMS {
                        self.scene.scene_graph.effective_body_transform(nid)
                            .unwrap_or_else(|| glam::Mat4::from_scale_rotation_translation(*body_scale, *body_rotation, *origin))
                    } else {
                        glam::Mat4::from_translation(*origin)
                    };
                    // Rigid-translate re-anchor (same rule as the raster draw): a rigid
                    // prim drag moves grid bounds without re-meshing, so the cached mesh
                    // pre-translates by the accumulated rigid offset.
                    let off = self.cache.body_vol_cache.mesh_rigid_offset(nid);
                    if off.length_squared() > 0.0 {
                        model *= glam::Mat4::from_translation(off);
                    }
                    // Same as raster_gbuffer: last additive prim, resolved through the
                    // CANONICAL graph (material_id → Material node, else embedded).
                    let src_prim = primitives.iter().rev()
                        .find(|pr| pr.operation.is_additive())
                        .or(primitives.last());
                    let pbr = src_prim
                        .map(|pr| self.scene.scene_graph.resolve_prim_material(pr).clone())
                        .unwrap_or_else(|| body_mat.clone());
                    push(&mut out, &mut mats, cid, model, &pbr);
                }
                NodePayload::ImportedMesh { mesh_cache_id, materials: imesh_mats, .. } => {
                    if self.cache.mesh_cache.get(*mesh_cache_id).is_none() { return; }
                    let model = self.scene.scene_graph.world_transform(nid);
                    let pbr = imesh_mats.first().cloned().unwrap_or_default();
                    push(&mut out, &mut mats, *mesh_cache_id, model, &pbr);
                }
                NodePayload::VoxelBody { mesh_cache_id, material, .. } => {
                    let is_active = self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid && !v.chunks.is_empty());
                    let model = self.scene.scene_graph.world_transform(nid);
                    if is_active {
                        if let Some(v) = self.sculpt.voxel.as_ref() {
                            for (i, &cid) in v.chunks.iter().enumerate() {
                                if v.chunk_tris.get(i).copied().unwrap_or(0) == 0 { continue; }
                                if self.cache.mesh_cache.get(cid).is_none() { continue; }
                                push(&mut out, &mut mats, cid, model, material);
                            }
                        }
                    } else if let Some(cid) = mesh_cache_id {
                        if self.cache.mesh_cache.get(*cid).is_some() {
                            push(&mut out, &mut mats, *cid, model, material);
                        }
                    }
                }
                NodePayload::PolygonalBody { mesh_cache_id: Some(cid), material, mesh, .. } => {
                    if self.cache.mesh_cache.get(*cid).is_none() { return; }
                    let model = self.scene.scene_graph.world_transform(nid);
                    let stamp = self.cache.mesh_cache.get(*cid).unwrap().content_stamp;
                    if let Some((atlas, pbr)) = self.gpu.as_ref().and_then(|gpu| gpu.mat_bake_pipe_cache.poly_curvature.override_for(nid, *cid, stamp)) {
                        push(&mut out, &mut mats, atlas, model, pbr);
                        return;
                    }
                    // M6 multi-material: one instance + per-tri scene MaterialIds in BLAS,
                    // remapped to SSBO slots at assemble. Mono path unchanged.
                    if let Some(ranges) = self
                        .cache
                        .mesh_cache
                        .get(*cid)
                        .and_then(|c| c.material_ranges.as_ref())
                        .filter(|r| r.len() > 1)
                    {
                        use render::mesh_gpu::RasterMaterial;
                        let body_pbr = self
                            .scene
                            .scene_graph
                            .resolve_poly_body_material(mesh, material)
                            .clone();
                        let base_slot = mats.len() as u32;
                        mats.push(RasterMaterial::from_pbr(&body_pbr));
                        let mut remap = std::collections::HashMap::new();
                        for &(mat_id, _, count) in ranges {
                            if count == 0 || mat_id == 0 || remap.contains_key(&mat_id) {
                                continue;
                            }
                            let pbr = self
                                .scene
                                .scene_graph
                                .material_data_if_visible(core_math::primitives::MaterialId(mat_id))
                                .cloned()
                                .unwrap_or_else(|| body_pbr.clone());
                            let slot = mats.len() as u32;
                            mats.push(RasterMaterial::from_pbr(&pbr));
                            remap.insert(mat_id, slot);
                        }
                        out.push(PtInstanceSource {
                            cache_id: *cid,
                            model,
                            material_base: base_slot,
                            material_id_remap: Some(remap),
                        });
                    } else {
                        let pbr = self.scene.scene_graph.resolve_poly_body_material(mesh, material);
                        push(&mut out, &mut mats, *cid, model, pbr);
                    }
                }
                NodePayload::NurbsBody { mesh_cache_id: Some(cid), material, .. }
                | NodePayload::NurbsSolid { mesh_cache_id: Some(cid), material, .. } => {
                    if self.cache.mesh_cache.get(*cid).is_none() { return; }
                    let model = self.scene.scene_graph.world_transform(nid);
                    push(&mut out, &mut mats, *cid, model, material);
                }
                _ => {}
            }
        };

        if use_stream {
            for &nid in &self.draw_stream {
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    visit(nid, node);
                }
            }
        } else {
            for (nid, node) in self.scene.scene_graph.all_nodes() {
                visit(nid, node);
            }
        }
        (out, mats)
    }

    /// Whether the RT-shadow feature wants the PT acceleration structure resident
    /// OUTSIDE PathTrace mode (RT_SHADOWS_DESIGN Stage 0 — the residency decouple).
    /// The whole triangle-PT machinery (BLAS builds, TLAS assemble/refit, the 7 SSBO
    /// uploads, pt_scene_generation invalidation) is reused UNCHANGED; this is the one
    /// gate that keeps it alive when the deferred/Object viewport needs occlusion rays.
    /// Gated on the runtime `RenderSettings.rt_shadows` toggle (Stage 1 wires the UI /
    /// uniform). Default OFF → zero accel/VRAM tax for users who never enable RT shadows.
    pub(crate) fn rt_shadows_active(&self) -> bool {
        self.render.rt_shadows
            // Progressive-refine AUTO SUN (Phase 2c): keep the accel resident whenever
            // the refine system may trace the sun on idle — residency must predate the
            // first idle frame, or the first refined shadows silently keep CSM (the
            // rt_shadows_on gate requires all four PT buffers resident).
            || (self.render.pbr_progressive_enabled
                && self.render.pbr_refine_rt_sun
                && matches!(
                    self.render.shading_mode,
                    crate::ShadingMode::Pbr | crate::ShadingMode::Lit
                ))
    }

    /// Poll-phase PT accel sync. Call from poll_background_tasks.
    pub(crate) fn update_pt_accel(&mut self) {
        // Stage 0 residency decouple: the accel stays live either in PathTrace mode
        // (the original consumer) OR when RT shadows are requested (the second consumer,
        // deferred/Object viewport). Everything below is REUSED verbatim — only this gate
        // changed (RT_SHADOWS_DESIGN §2 Stage 0). MAX_PT_BLAS_REQUESTS_PER_FRAME still caps
        // build spam, so the only added standing cost is resident geometry SSBOs while the
        // toggle is on — bounded and gated, per the §3 residency-tax note.
        if self.render.shading_mode != crate::ShadingMode::PathTrace && !self.rt_shadows_active() {
            return;
        }
        if self.gpu.is_none() {
            return;
        }

        // ── PT-warm UX ──────────────────────────────────────────────────────────────────────────
        // The tri-PT pipeline compiles on the background thread (~2.4s cold); entering PT before
        // the hot-swap lands silently shows raster — the owner read it as a broken toggle.
        // One-shot notice while cold; the latch re-arms when the pipeline lands (so a shader-edit
        // recompile, which nulls the pipeline in dirty.rs, notifies again).
        // PathTrace-mode ONLY: the pt_tri pipeline is the PT DISPLAY pass. RT shadows (Stage 0)
        // keeps the accel resident without that pipeline — never toast "path tracer compiling"
        // when the user only enabled RT shadows in the deferred viewport.
        if self.render.shading_mode == crate::ShadingMode::PathTrace {
            let pt_pipe_ready = self.gpu.as_ref().unwrap().pipelines.pt_tri_pipeline.is_some();
            if !pt_pipe_ready {
                if !self.render.pt_warm_notified {
                    self.render.pt_warm_notified = true;
                    eprintln!("[PtWarm] path tracer compiling — raster shown until ready");
                    self.toast("Path tracer compiling — raster shown until ready");
                }
            } else if self.render.pt_warm_notified {
                self.render.pt_warm_notified = false;
                eprintln!("[PtWarm] path tracer pipeline ready");
            }
        }

        // ── PT sparse-SDF snapshots (consolidation fix) ─────────────────────────────────────────
        // Brick-resident (≥512) bodies have NO CPU mesh (display = GPU slots/pool), so the gather
        // below found 0 instances and PT silently fell back to raster. Snapshot such bodies:
        // a TEMPORARY monolithic level-0 mesh from the body's own atlas (never touches the display
        // slot) + async readback → mesh_cache.update_editable_mesh. One snapshot per content
        // change (keyed on the cache entry's source_hash), one kick per poll (Frame Budget Rule).
        self.pump_pt_sdf_snapshots();

        // B.2: refresh structural draw stream before gather so PT and raster share candidates.
        self.ensure_structural_draw_stream();
        let (instances, materials) = self.gather_pt_instances();

        // Retain by mesh-cache ownership, not current visibility. Hidden or
        // isolation-filtered bodies keep their expensive BLAS so showing them
        // again does not rebuild; deleted meshes are released and late
        // background results stay invalidated (Codex pt-renderer-review).
        let mesh_cache = &self.cache.mesh_cache;
        self.cache
            .pt_accel
            .retain_live(|id| mesh_cache.get(id).is_some());

        // Request BLAS builds for any instance whose mesh content changed.
        // Frame Budget Rule: cap new build jobs per poll — a sculpt stroke can
        // dirty dozens of chunk meshes in one frame; the rest queue next polls
        // (each request also clones the mesh data, so this bounds CPU copies too).
        const MAX_PT_BLAS_REQUESTS_PER_FRAME: u32 = 2;
        /// Per-body BLAS ceiling — beyond this, clone+build+upload tends to TDR / device-loss
        /// with several 1024³/2048³ bodies (owner repro: 4×2048³ ~20 prims → PT crash).
        const PT_MAX_TRIS_PER_BLAS: usize = 1_200_000;
        let mut requested = 0u32;
        for src in &instances {
            if requested >= MAX_PT_BLAS_REQUESTS_PER_FRAME { break; }
            let Some(stamp) = self.cache.mesh_cache.pt_stamp(src.cache_id) else { continue };
            if self.cache.pt_accel.is_current(src.cache_id, stamp) { continue; }
            let t_clone = std::time::Instant::now();
            if let Some((verts, indices)) = self.cache.mesh_cache.pt_source_data(src.cache_id) {
                let clone_ms = t_clone.elapsed().as_secs_f64() * 1000.0;
                let ntris = indices.len() / 3;
                if ntris > PT_MAX_TRIS_PER_BLAS {
                    eprintln!(
                        "[PtBlas] SKIP cid={:?} tris={ntris} > {PT_MAX_TRIS_PER_BLAS} \
                         (Grid too high for PT — lower to ≤1024 or fewer high-res bodies)",
                        src.cache_id
                    );
                    // Mark this stamp as current so we neither re-probe nor
                    // re-log every poll; a content change (new stamp) re-probes
                    // and logs the skip once again.
                    self.cache.pt_accel.mark_skipped(src.cache_id, stamp);
                    continue;
                }
                // M6: stamp per-tri scene MaterialIds from material_ranges when multi-mat.
                let tri_mats = self
                    .cache
                    .mesh_cache
                    .get(src.cache_id)
                    .and_then(|c| c.material_ranges.as_ref())
                    .filter(|r| r.len() > 1)
                    .map(|ranges| {
                        render::pt_accel::expand_material_ranges_to_tri_ids(
                            ranges,
                            indices.len(),
                        )
                    });
                if self.cache.pt_accel.request_ex(
                    src.cache_id,
                    stamp,
                    verts,
                    indices,
                    tri_mats,
                ) {
                    requested += 1;
                    if clone_ms > 5.0 {
                        eprintln!("[PtBlas] cid={:?} built: tris={ntris} source-clone={clone_ms:.1}ms (frame thread)", src.cache_id);
                    }
                }
            }
        }
        let blas_changed = self.cache.pt_accel.poll();

        // Scene hash: instance list + transforms + the BLAS stamps actually in use.
        let scene_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            instances.len().hash(&mut h);
            for src in &instances {
                src.cache_id.0.hash(&mut h);
                for v in src.model.to_cols_array() { v.to_bits().hash(&mut h); }
                src.material_base.hash(&mut h);
            }
            // Material edits must restart accumulation too.
            bytemuck::cast_slice::<_, u8>(&materials).hash(&mut h);
            h.finish()
        };
        if !blas_changed && scene_hash == self.cache.pt_last_scene_hash {
            return; // nothing changed — keep the uploaded scene
        }
        self.cache.pt_last_scene_hash = scene_hash;

        // ── TLAS REFIT fast path (P3.2) ─────────────────────────────────────────────────────────
        // Geometry stays byte-identical when the ordered unique *traceable* BLAS
        // sequence is stable (layout_ids). Instance count/order may vary; shared
        // mesh geometry is deduped in assemble. Re-upload ONLY tlas + instances
        // (+ materials + meta); leave multi-MB blas/tris/verts SSBOs untouched.
        let instance_ids = self.cache.pt_accel.layout_ids(&instances);
        let refit_ok = !blas_changed && instance_ids == self.cache.pt_last_instance_ids;
        if refit_ok {
            let scene = self.cache.pt_accel.refit(&instances);
            // Geometry counts stay from the last full assemble; always refresh
            // tlas/instance/material counts so material assigns can't leave meta
            // pointing past the materials buffer (GPU validation / device lost).
            let meta = PtMetaGpu {
                tlas_count: scene.tlas.len() as u32,
                instance_count: scene.instances.len() as u32,
                blas_count: self.cache.pt_last_meta.blas_count,
                tri_count: self.cache.pt_last_meta.tri_count,
                vert_count: self.cache.pt_last_meta.vert_count,
                material_count: materials.len().max(1) as u32,
                _pad: [0; 2],
            };
            self.cache.pt_last_meta = meta;
            self.cache.pt_instance_count = meta.instance_count;
            let gpu = self.gpu.as_mut().unwrap();
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            let b = &mut gpu.buffers;
            // Always bind at least one default material (shader always indexes [0] on miss).
            let mats_upload = if materials.is_empty() {
                vec![render::mesh_gpu::RasterMaterial::from_pbr(
                    &core_math::primitives::PbrMaterial::default(),
                )]
            } else {
                materials
            };
            upload_pod(&device, &queue, &mut b.pt_tlas_buffer, &mut b.pt_scene_generation, "pt_tlas", &scene.tlas, wgpu::BufferUsages::empty());
            upload_pod(&device, &queue, &mut b.pt_instance_buffer, &mut b.pt_scene_generation, "pt_instances", &scene.instances, wgpu::BufferUsages::empty());
            upload_pod(&device, &queue, &mut b.pt_material_buffer, &mut b.pt_scene_generation, "pt_materials", &mats_upload, wgpu::BufferUsages::empty());
            upload_pod(&device, &queue, &mut b.pt_meta_buffer, &mut b.pt_scene_generation, "pt_meta", &[meta], wgpu::BufferUsages::empty());
            self.sync_native_rt_scene(&scene.instances, meta);
            // M1.c: mid-episode keep accum (Hybrid covers drag); settle restarts on commit.
            if !self.has_active_episode() {
                self.pt_reset_accum_on_scene_change();
            }
            self.mark_dirty(crate::DirtyFlags::RENDER);
            return;
        }

        let t_asm = std::time::Instant::now();
        let scene = self.cache.pt_accel.assemble(&instances);
        let asm_ms = t_asm.elapsed().as_secs_f64() * 1000.0;
        self.cache.pt_instance_count = scene.instances.len() as u32;
        self.cache.pt_last_instance_ids = instance_ids;

        // Total tri ceiling — 4× large LODs must still fit in storage binding limits.
        const PT_MAX_TRIS_TOTAL: usize = 3_500_000;
        if scene.tris.len() > PT_MAX_TRIS_TOTAL {
            eprintln!(
                "[PtAsm] ABORT upload tris={} > {PT_MAX_TRIS_TOTAL} — keeping previous PT scene \
                 (lower Grid from 2048 or hide some bodies)",
                scene.tris.len()
            );
            self.toast(
                "Path Trace: scene mesh too dense — lower Grid (≤1024) or fewer 2048 bodies",
            );
            // Don't upload / don't bump generation with incomplete oversize buffers.
            return;
        }

        // ---- Upload (grow-only buffers; every realloc bumps the generation) ----
        let t_up = std::time::Instant::now();
        let gpu = self.gpu.as_mut().unwrap();
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();

        let vertex_usage = if gpu.native_rt.is_some() { wgpu::BufferUsages::BLAS_INPUT } else { wgpu::BufferUsages::empty() };
        let b = &mut gpu.buffers;
        upload_pod(&device, &queue, &mut b.pt_tlas_buffer, &mut b.pt_scene_generation, "pt_tlas", &scene.tlas, wgpu::BufferUsages::empty());
        upload_pod(&device, &queue, &mut b.pt_instance_buffer, &mut b.pt_scene_generation, "pt_instances", &scene.instances, wgpu::BufferUsages::empty());
        upload_pod(&device, &queue, &mut b.pt_blas_buffer, &mut b.pt_scene_generation, "pt_blas", &scene.blas_nodes, wgpu::BufferUsages::empty());
        upload_pod(&device, &queue, &mut b.pt_tri_buffer, &mut b.pt_scene_generation, "pt_tris", &scene.tris, wgpu::BufferUsages::empty());
        upload_pod(&device, &queue, &mut b.pt_vert_buffer, &mut b.pt_scene_generation, "pt_verts", &scene.verts, vertex_usage);
        let mats_upload = if materials.is_empty() {
            vec![render::mesh_gpu::RasterMaterial::from_pbr(
                &core_math::primitives::PbrMaterial::default(),
            )]
        } else {
            materials
        };
        upload_pod(&device, &queue, &mut b.pt_material_buffer, &mut b.pt_scene_generation, "pt_materials", &mats_upload, wgpu::BufferUsages::empty());
        let meta = PtMetaGpu {
            tlas_count: scene.tlas.len() as u32,
            instance_count: scene.instances.len() as u32,
            blas_count: scene.blas_nodes.len() as u32,
            tri_count: scene.tris.len() as u32,
            vert_count: scene.verts.len() as u32,
            material_count: mats_upload.len() as u32,
            _pad: [0; 2],
        };
        upload_pod(&device, &queue, &mut b.pt_meta_buffer, &mut b.pt_scene_generation, "pt_meta", &[meta], wgpu::BufferUsages::empty());
        self.cache.pt_last_meta = meta;
        // Full assemble = new geometry contents (possibly written into reused buffers,
        // which `pt_scene_generation` does not see) → the native BLAS cache must rebuild.
        gpu.native_rt_geometry_epoch += 1;
        self.sync_native_rt_scene(&scene.instances, meta);
        let up_ms = t_up.elapsed().as_secs_f64() * 1000.0;
        if asm_ms + up_ms > 5.0 {
            eprintln!("[PtAsm] full assemble={asm_ms:.1}ms upload={up_ms:.1}ms (tris={} verts={} blas_nodes={} instances={}) | {}",
                meta.tri_count, meta.vert_count, meta.blas_count, meta.instance_count,
                crate::profiling::perf_stamp());
        }

        // Scene content changed → restart progressive accumulation (hard or soft draft).
        // M1.c: mid-episode (material scrub / drag) keep accum; Hybrid covers the drag.
        if !self.has_active_episode() {
            self.pt_reset_accum_on_scene_change();
        }
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// D — Draft mesh soft reset (toggleable). Soft keeps a short progressive weight so
    /// variance-aware blend collapses ghosts without a full black flash. Hard = frames 0.
    pub(crate) fn pt_reset_accum_on_scene_change(&mut self) {
        self.render.pt_converged = false;
        if self.render.pt_draft_soft_reset {
            self.render.pt_accumulated_frames =
                self.render.pt_draft_soft_frames.clamp(1, 8);
        } else {
            self.render.pt_accumulated_frames = 0;
        }
    }
}
