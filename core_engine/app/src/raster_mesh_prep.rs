// ============================================================
// Raster mesh provisioning (Slice 17)
// ============================================================
//
// Low-risk mesh-prep methods relocated out of raster_render.rs: poll the mesh
// cache and ensure every SDF / poly / NURBS body has an up-to-date mesh cache
// entry, plus the idle LOD-upgrade pass. These are `impl AzraApp` methods, so
// they resolve crate-wide alongside the rest of the raster code. `body_aabb_bounds`
// lives in raster_common.rs (re-exported at the crate root).

use core_math::scene_graph::NodePayload;
use crate::DirtyFlags;
use crate::raster_common::body_aabb_bounds;
use render::mesh_cache::MeshLod;

/// When true, prim positions are stored in body-local space.
use crate::BODY_LOCAL_PRIMS;

/// Offset prim positions to body-local space by subtracting body.origin.
impl super::AzraApp {
    /// Poll the mesh cache for completed background jobs and upload GPU buffers.
    /// Call once per frame before rendering.
    pub(crate) fn poll_mesh_cache(&mut self) {
        // Land any completed async GPU-mesh readbacks first (Phase G live path), then the CPU DC poll.
        self.poll_sdf_mesh_jobs();
        let updated = self.cache.mesh_cache.poll();
        if !updated.is_empty() {
            if let Some(gpu) = &self.gpu {
                self.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue);
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    /// Ensure all SDF bodies in the scene graph have mesh cache entries.
    /// Queues background meshing for any bodies that need it.
    /// Uses Preview LOD (64³) for fast feedback — upgrade_mesh_lod() will
    /// promote to Medium/High once editing stops.
    ///
    /// Body-local bake: prim positions are offset by -body.origin so the DC
    /// mesh vertices come out in body-local space. The raster instance
    /// transform translates them back to world space via body.origin.
    pub(crate) fn ensure_body_meshes(&mut self) {
        let mut needs_mesh: Vec<(core_math::scene_graph::NodeId, Vec<core_math::primitives::SdfPrimitive>, u64, u8, glam::Vec3, f32)> = Vec::new();

        for (_nid, node) in self.scene.scene_graph.sdf_bodies() {
            if let NodePayload::SdfBody { primitives, mesh_cache_id, mirror_flags, mirror_center, bevel_radius, origin, .. } = &node.payload {
                // Skip clean bodies that already have a mesh — no hash needed.
                // Only rehash bodies that are dirty or need initial allocation.
                if mesh_cache_id.is_some() && !self.cache.body_vol_cache.dirty_bodies.contains(&node.id) {
                    continue;
                }
                if primitives.is_empty() { continue; }
                // Filter to visible prims only — matches voxel bake path (lib.rs:648)
                let visible_prims: Vec<_> = primitives.iter().filter(|p| p.visible).cloned().collect();
                if visible_prims.is_empty() { continue; }
                // With BODY_LOCAL_PRIMS, prim positions are already body-local — no offset needed.
                // Legacy path: subtract body.origin to move prims into local space.
                let body_origin = *origin;
                let local_prims = if BODY_LOCAL_PRIMS {
                    visible_prims
                } else {
                    crate::bake_utils::offset_prims_to_local(&visible_prims, body_origin)
                };
                let local_mc = if BODY_LOCAL_PRIMS {
                    *mirror_center // already body-local
                } else {
                    *mirror_center - body_origin
                };
                // Gate 1: unified RECURSIVE BodyContentId (body-local prims + child order +
                // lab_operand_gen — rigid body moves still don't change it, so no unnecessary
                // remeshing; a nested-child edit now DOES change it, which is strictly safer).
                let hash = crate::bake_utils::body_content_id(
                    &self.scene.scene_graph, node.id, self.cache.body_vol_cache.lab_operand_gen,
                ).map_or(0, |id| id.0);
                let (bmin, bmax) = body_aabb_bounds(&local_prims, *mirror_flags, local_mc, *bevel_radius);
                match mesh_cache_id {
                    Some(cid) => {
                        // CR-44 / BUG#1: in volume-cache mode the GRID DC (dispatch_grid_dc_async →
                        // insert_mesh_direct) is the SOLE owner of an SDF body's display mesh. Re-requesting
                        // an analytical 64³ Preview here on EVERY dirty frame re-stamps a low-detail mesh
                        // OVER the high-res grid mesh → the rasterized surface visibly loses detail right
                        // after an SDF edit. So the Preview fires ONLY as a first-paint placeholder, before
                        // any baked grid exists; once the body has a grid, the grid path owns the mesh.
                        let grid_owns = self.cache.use_body_vol_cache
                            && self.cache.body_vol_cache.has_cached_grid(node.id);
                        if !grid_owns && self.cache.mesh_cache.needs_update(*cid, hash) {
                            self.cache.mesh_cache.request_mesh(*cid, local_prims, hash, MeshLod::Preview, bmin, bmax);
                        }
                    }
                    None => {
                        needs_mesh.push((node.id, local_prims, hash, *mirror_flags, local_mc, *bevel_radius));
                    }
                }
            }
        }

        for (nid, prims, hash, mf, mc, bv) in needs_mesh {
            let cid = self.cache.mesh_cache.alloc_id();
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::SdfBody { mesh_cache_id, .. } = &mut node.payload {
                    *mesh_cache_id = Some(cid);
                }
            }
            let (bmin, bmax) = body_aabb_bounds(&prims, mf, mc, bv);
            self.cache.mesh_cache.request_mesh(cid, prims, hash, MeshLod::Preview, bmin, bmax);
        }
    }

    /// Ensure all PolygonalBody nodes have drawable mesh cache entries.
    ///
    /// **POLY_VIEWPORT_PERF P1b.3 / C15 — single producer only.**
    /// Historically this path tessellated the **base** mesh via `raster_data_ngon` with **no**
    /// modifiers and **no** `poly_display_cache` write, while `poly_sync_mesh_cache` produced the
    /// real display mesh (modifiers + ear-clip). Two producers → wrong solid / wrong pick / wasted
    /// work. Recovery always goes through `poly_sync_mesh_cache` (the sole owner of poly display
    /// geometry + GPU upload).
    ///
    /// Also recovers solids that exist in the cache but are **undrawable** (`gpu_mesh` missing
    /// or `index_count == 0` while the half-edge mesh still has faces) — the "full mesh vanishes
    /// after face delete" path.
    pub(crate) fn ensure_poly_meshes(&mut self) {
        // D1 / T1: walking 100k poly_bodies was ~6–10 ms of scoreboard `mesh=` with nothing
        // to recover. Latch `poly_meshes_drawable_ok` after a clean pass; clear on SCENE/MESH.
        //
        // R6a: while a budgeted display drain is in flight, do NOT walk or sync here —
        // recovery enqueues into the same `poly_display_dirty` queue. Inline
        // `poly_sync_mesh_cache` for the whole recover list was a 13s frame on 8x9.
        // If any "mesh vanishes" report returns, these early-outs are suspect #1 — they
        // remove the undrawable-solid recovery sweep on hot frames.
        if !self.poly_display_dirty.is_empty() {
            return;
        }
        if self.poly_meshes_drawable_ok && !self.structural_draw_stale {
            return;
        }
        let mut recover: Vec<core_math::scene_graph::NodeId> = Vec::new();
        // C-scale: MeshCacheId → drawable? 10k B.4 clones share one cid — probe once.
        // `false` also means "already queued one recover for this cid" so siblings don't all sync.
        let mut cid_drawable: std::collections::HashMap<
            core_math::scene_graph::MeshCacheId,
            bool,
        > = std::collections::HashMap::new();

        for (_nid, node) in self.scene.scene_graph.poly_bodies() {
            if let NodePayload::PolygonalBody {
                mesh,
                mesh_cache_id,
                ..
            } = &node.payload
            {
                let live_faces = mesh.face_count();
                let need = match mesh_cache_id {
                    None => live_faces > 0,
                    Some(cid) => {
                        if cid_drawable.contains_key(cid) {
                            // Already know drawable, or one recover is queued for this share.
                            false
                        } else {
                            let entry = self.cache.mesh_cache.get(*cid);
                            let ok = match entry {
                                None => false,
                                Some(c) => {
                                    let no_gpu = c.gpu_mesh.is_none();
                                    let zero_idx = c
                                        .gpu_mesh
                                        .as_ref()
                                        .map(|g| g.index_count == 0)
                                        .unwrap_or(true);
                                    let zero_tri = c.triangle_count == 0;
                                    !(no_gpu || (live_faces > 0 && (zero_idx || zero_tri)))
                                }
                            };
                            cid_drawable.insert(*cid, ok);
                            !ok && live_faces > 0
                        }
                    }
                };
                if need {
                    recover.push(node.id);
                }
            }
        }

        // One producer per missing cid (siblings share after B.4 COW rules).
        if recover.is_empty() {
            self.poly_meshes_drawable_ok = true;
        } else {
            self.poly_meshes_drawable_ok = false;
            // R6a: enqueue into budgeted drain — NEVER poly_sync_mesh_cache inline.
            for nid in recover {
                self.poly_display_dirty.insert(nid);
            }
        }
    }

    /// Ensure all NurbsBody nodes have mesh cache entries.
    /// Tessellates the NURBS surface and inserts into the cache.
    pub(crate) fn ensure_nurbs_meshes(&mut self) {
        let mut needs: Vec<(core_math::scene_graph::NodeId, (Vec<[f32; 8]>, Vec<u32>))> = Vec::new();
        let mut needs_alloc: Vec<(core_math::scene_graph::NodeId, (Vec<[f32; 8]>, Vec<u32>))> = Vec::new();

        // TESSELLATE ONLY ON A CACHE MISS (owner soak P0, 2026-07-29).
        //
        // This called `surface.tessellate()` BEFORE checking the cache, so every NURBS body was
        // re-tessellated on EVERY frame and the result thrown away whenever the cache already had
        // it. Measured ~66 ms average, peaks near 98 ms — an idle Hybrid scene containing any
        // NURBS body ran at 120-160 ms frames doing work it immediately discarded.
        //
        // The cache check is cheap and the tessellation is not, so the order is the whole fix:
        // decide FIRST, compute only when the answer is "missing".
        for (_nid, node) in self.scene.scene_graph.nurbs_bodies() {
            match &node.payload {
                NodePayload::NurbsBody {
                    surface,
                    mesh_cache_id,
                    ..
                } => match mesh_cache_id {
                    Some(cid) => {
                        if self.cache.mesh_cache.get(*cid).is_none() {
                            needs.push((node.id, surface.tessellate()));
                        }
                    }
                    None => {
                        needs_alloc.push((node.id, surface.tessellate()));
                    }
                },
                NodePayload::NurbsSolid {
                    faces,
                    mesh_cache_id,
                    ..
                } => {
                    let merge = || {
                        let mut verts: Vec<[f32; 8]> = Vec::new();
                        let mut indices: Vec<u32> = Vec::new();
                        for face in faces {
                            // Prefer face.tessellate() (trim-aware + denser Fillet/Corner)
                            let (fv, fi) = face.tessellate();
                            let base = verts.len() as u32;
                            verts.extend(fv);
                            indices.extend(fi.into_iter().map(|i| i + base));
                        }
                        (verts, indices)
                    };
                    match mesh_cache_id {
                        Some(cid) => {
                            if self.cache.mesh_cache.get(*cid).is_none() {
                                needs.push((node.id, merge()));
                            }
                        }
                        None => {
                            needs_alloc.push((node.id, merge()));
                        }
                    }
                }
                _ => {}
            }
        }

        for (nid, raster) in needs {
            // B.4: shared MeshCacheId after Shift+D — COW before overwrite.
            self.cow_mesh_cache_id_before_write(nid);
            if let Some(node) = self.scene.scene_graph.get(nid) {
                let cid = match &node.payload {
                    NodePayload::NurbsBody {
                        mesh_cache_id: Some(cid),
                        ..
                    }
                    | NodePayload::NurbsSolid {
                        mesh_cache_id: Some(cid),
                        ..
                    } => Some(*cid),
                    _ => None,
                };
                if let Some(cid) = cid {
                    let tri_count = raster.1.len() as u32 / 3;
                    self.cache.mesh_cache.update_editable_mesh(cid, raster, tri_count);
                }
            }
        }

        for (nid, raster) in needs_alloc {
            let cid = self.cache.mesh_cache.alloc_id();
            let tri_count = raster.1.len() as u32 / 3;
            self.cache.mesh_cache.update_editable_mesh(cid, raster, tri_count);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                match &mut node.payload {
                    NodePayload::NurbsBody { mesh_cache_id, .. }
                    | NodePayload::NurbsSolid { mesh_cache_id, .. } => {
                        *mesh_cache_id = Some(cid);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Upgrade SDF body meshes to higher quality after idle.
    /// Preview→Medium→High. Call once per frame; only triggers when no mesh
    /// jobs are in flight and scene is not dirty.
    pub(crate) fn upgrade_mesh_lod(&mut self) {
        // Only upgrade when no jobs pending and scene is stable
        if self.cache.mesh_cache.pending_count() > 0 {
            return;
        }
        if self.is_dirty {
            return;
        }

        let mut upgrades: Vec<(core_math::scene_graph::MeshCacheId, Vec<core_math::primitives::SdfPrimitive>, u64, MeshLod, u8, glam::Vec3, f32)> = Vec::new();

        for (_nid, node) in self.scene.scene_graph.sdf_bodies() {
            if let NodePayload::SdfBody { primitives, mesh_cache_id: Some(cid), mirror_flags, mirror_center, bevel_radius, origin, .. } = &node.payload {
                if primitives.is_empty() { continue; }
                // Filter to visible prims only — must match ensure_body_meshes hash scope
                let visible_prims: Vec<_> = primitives.iter().filter(|p| p.visible).cloned().collect();
                if visible_prims.is_empty() { continue; }
                if let Some(cached) = self.cache.mesh_cache.get(*cid) {
                    let next_lod = match cached.lod {
                        MeshLod::Preview => Some(MeshLod::Medium),
                        MeshLod::Medium => Some(MeshLod::High),
                        MeshLod::High => None,
                    };
                    if let Some(target_lod) = next_lod {
                        // With BODY_LOCAL_PRIMS, prim positions are already body-local — no offset needed.
                        // Legacy path: subtract body.origin to move prims into local space.
                        let body_origin = *origin;
                        let local_prims = if BODY_LOCAL_PRIMS {
                            visible_prims
                        } else {
                            crate::bake_utils::offset_prims_to_local(&visible_prims, body_origin)
                        };
                        let local_mc = if BODY_LOCAL_PRIMS {
                            *mirror_center // already body-local
                        } else {
                            *mirror_center - body_origin
                        };
                        // Gate 1: unified BodyContentId (consistent with ensure_body_meshes)
                        let hash = crate::bake_utils::body_content_id(
                            &self.scene.scene_graph, node.id, self.cache.body_vol_cache.lab_operand_gen,
                        ).map_or(0, |id| id.0);
                        upgrades.push((*cid, local_prims, hash, target_lod, *mirror_flags, local_mc, *bevel_radius));
                    }
                }
            }
        }

        // Limit to one upgrade per frame to avoid flooding threads
        if let Some((cid, prims, hash, target_lod, mf, mc, bv)) = upgrades.into_iter().next() {
            let (bmin, bmax) = body_aabb_bounds(&prims, mf, mc, bv);
            self.cache.mesh_cache.request_mesh(cid, prims, hash, target_lod, bmin, bmax);
        }
    }
}

#[cfg(test)]
mod nurbs_tessellation_gate {
    /// OWNER SOAK P0 2026-07-29: `ensure_nurbs_meshes` called `surface.tessellate()` BEFORE
    /// checking the mesh cache, so every NURBS body was re-tessellated on EVERY frame and the
    /// result discarded whenever the cache already held it. Measured ~66 ms average, peaks near
    /// 98 ms — an idle Hybrid scene with any NURBS body ran at 120-160 ms frames.
    ///
    /// This reads the SHIPPED source rather than timing a frame: the defect is an ordering one
    /// (compute-then-decide instead of decide-then-compute), and ordering is exactly what a source
    /// assertion can pin cheaply and deterministically.
    ///
    /// Safe because NURBS EDITS do not rely on this path: every mutation site in nurbs_tools.rs
    /// calls `nurbs_sync_mesh_cache`, which re-tessellates and updates the cache directly. This
    /// function's only job is filling a MISSING entry.
    #[test]
    fn ensure_nurbs_meshes_tessellates_only_on_a_cache_miss() {
        let src = include_str!("raster_mesh_prep.rs");
        let start = src.find("pub(crate) fn ensure_nurbs_meshes")
            .expect("ensure_nurbs_meshes moved");
        let end = src[start..].find("\n    }").map(|i| start + i).unwrap_or(src.len());
        let body = &src[start..end];

        // The collection loop must not bind an unconditional tessellation before the match.
        assert!(!body.contains("let raster = surface.tessellate();"),
            "ensure_nurbs_meshes tessellates BEFORE checking the cache again - that is the ~66ms \
             every-frame regression. Tessellate inside the miss branches instead.");
        // ...and must still tessellate somewhere, or it would never fill the cache at all.
        assert!(body.contains("surface.tessellate()"),
            "ensure_nurbs_meshes no longer tessellates at all - a NURBS body would never get a mesh");
    }
}
