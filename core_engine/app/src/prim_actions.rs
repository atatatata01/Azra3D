//! SDF primitive graph operations — add, remove, reorder, reparent, sync.
//! Also covers legacy object grouping and SDF body group/dissolve.
//! Shared actions called from outliner drag, keyboard, pie menu, agent bar.

use crate::*;

impl FourthLayerApp {
    // ---- UID safety ------------------------------------------------------------

    /// Allocate a unique prim UID. Centralizes the increment to prevent collisions.
    #[allow(dead_code)] // FUTURE: prim creation via graph API
    pub(crate) fn alloc_prim_uid(&mut self) -> u32 {
        let uid = self.scene.next_prim_id;
        self.scene.next_prim_id += 1;
        uid
    }

    /// Recalculate `next_prim_id` to be greater than the max `prim_uid` in the scene.
    /// Call after deserialization to prevent stale UID collisions.
    pub(crate) fn recalculate_next_prim_id(&mut self) {
        let max_uid = self.scene.primitives.iter().map(|p| p.prim_uid).max().unwrap_or(0);
        // Also scan the scene graph directly (in case flat cache is stale)
        let max_graph_uid = self.scene.scene_graph.all_primitives_ordered().iter()
            .map(|(_, _, p)| p.prim_uid)
            .max()
            .unwrap_or(0);
        let overall_max = max_uid.max(max_graph_uid);
        if self.scene.next_prim_id <= overall_max {
            self.scene.next_prim_id = overall_max + 1;
        }
    }

    // recalculate_next_light_id moved to scene_actions.rs (graph-native)

    /// Recalculate `next_node_name_id` from existing scene graph node names.
    /// Scans all nodes for numeric suffixes (e.g., "Body.007" -> 7) and sets
    /// the counter to max(current_value, max_suffix + 1).
    /// Call after deserialization to prevent name collisions.
    pub(crate) fn recalculate_next_node_name_id(&mut self) {
        let mut max_suffix: u32 = 0;
        for (_nid, node) in self.scene.scene_graph.all_nodes() {
            let name = node.payload.display_name();
            // Names follow the pattern "Prefix.NNN" — extract the numeric suffix after the last '.'
            if let Some(dot_pos) = name.rfind('.') {
                if let Ok(n) = name[dot_pos + 1..].parse::<u32>() {
                    max_suffix = max_suffix.max(n);
                }
            }
        }
        let needed = max_suffix + 1;
        if self.scene.next_node_name_id < needed {
            self.scene.next_node_name_id = needed;
        }
    }

    // ---- Prim graph CRUD -------------------------------------------------------

    pub(crate) fn add_prim_to_graph(&mut self, prim: SdfPrimitive) -> usize {
        let mut target_nid = self.scene.active_body_nid;
        if self.scene.scene_graph.body_primitives_mut(target_nid).is_none() {
            // First SDF prim (or stale active): create Body.00N and point both body nids.
            // This is the intentional empty-scene path — not a recovery failure.
            target_nid = self.ensure_sdf_body();
        }
        // target_nid is guaranteed valid — ensure_sdf_body creates one if needed
        let new_uid = prim.prim_uid;
        self.scene.scene_graph.body_primitives_mut(target_nid).unwrap().push(prim);
        // child_order: place the new prim at the END of the unified stack (after existing
        // prims AND sub-bodies) — "added last", NOT forced to the top. ensure first canonicalises
        // any out-of-sync state, then we move the just-added ref to the tail.
        self.scene.scene_graph.ensure_body_child_order(target_nid);
        let tail = self.scene.scene_graph.body_child_order(target_nid).len();
        self.scene.scene_graph.insert_child_order(
            target_nid,
            core_math::scene_graph::BodyChildRef::Prim(new_uid),
            tail,
        );
        self.recalculate_body_origin(target_nid);
        self.sync_flat_cache();
        self.mark_node_body_vol_dirty(target_nid);
        // A structural add is a one-shot, not a rapid-edit stream: bypass the 40ms bake
        // debounce so the catch-up full bake (which makes the NEW prim appear) dispatches
        // immediately instead of being stalled — the "new prim invisible until you move it"
        // bug. Same one-shot pattern as scope-leave / check_idle_retighten.
        self.cache.body_vol_cache.clear_bake_debounce();
        self.mark_outliner_dirty();
        self.scene.primitives.len().saturating_sub(1)
    }

    /// Duplicate a prim for Shift+Drag transform: no undo, no offset, no rebake.
    /// Returns (new_flat_idx, new_uid). The duplicate is added to the same body as the source.
    pub(crate) fn duplicate_prim_for_transform(&mut self, idx: usize) -> Option<(usize, u32)> {
        if idx >= self.scene.primitives.len() { return None; }
        // Ensure duplicate goes into the SOURCE prim's body (BUG-45 fix)
        if idx < self.scene.prim_body_map.len() {
            let source_body = self.scene.prim_body_map[idx].0;
            if source_body != self.scene.active_body_nid {
                self.scene.active_body_nid = source_body;
            }
        }
        let uid = self.scene.next_prim_id;
        self.scene.next_prim_id += 1;
        let mut d = self.scene.primitives[idx].clone();
        d.prim_uid = uid;
        d.instance_master_id = None;
        d.name = self.next_name(d.shape.label());
        // NO position offset — duplicate sits exactly on top of original
        let new_idx = self.add_prim_to_graph(d);
        Some((new_idx, uid))
    }

    /// Remove an SDF primitive by flat index from the scene graph's SdfBody, rebuild flat cache.
    pub(crate) fn remove_prim_from_graph(&mut self, flat_idx: usize) {
        if flat_idx >= self.scene.prim_body_map.len() { return; }
        let (nid, body_idx) = self.scene.prim_body_map[flat_idx];
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
            if body_idx < prims.len() {
                prims.remove(body_idx);
            }
        }
        // child_order: drop the now-dangling prim ref (reconcile keeps only live refs).
        self.scene.scene_graph.ensure_body_child_order(nid);
        self.recalculate_body_origin(nid);
        self.sync_flat_cache();
        self.sanitize_selection();
        // Mark body volume dirty so the voxel cache rebakes without the deleted prim.
        // Without this, the old grid (with deleted prim's SDF) persists as a ghost.
        self.mark_node_body_vol_dirty(nid);
        self.mark_outliner_dirty();
    }

    /// Remove multiple SDF primitives by flat indices. Indices must be valid at call time.
    pub(crate) fn remove_prims_from_graph(&mut self, flat_indices: &[usize]) {
        let mut removals: Vec<(NodeId, usize)> = flat_indices.iter()
            .filter_map(|&fi| {
                if fi < self.scene.prim_body_map.len() {
                    Some(self.scene.prim_body_map[fi])
                } else { None }
            })
            .collect();
        // Collect affected bodies for origin recalculation
        let affected_bodies: Vec<NodeId> = removals.iter().map(|(nid, _)| *nid).collect::<std::collections::HashSet<_>>().into_iter().collect();
        // Sort by body_idx descending so we remove from the end first
        removals.sort_by(|a, b| b.1.cmp(&a.1));
        removals.dedup();
        for (nid, body_idx) in removals {
            if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                if body_idx < prims.len() {
                    prims.remove(body_idx);
                }
            }
        }
        for nid in &affected_bodies {
            self.recalculate_body_origin(*nid);
            // child_order: drop dangling prim refs for each affected body.
            self.scene.scene_graph.ensure_body_child_order(*nid);
        }
        self.sync_flat_cache();
        self.sanitize_selection();
        // Mark affected bodies volume-dirty so the voxel cache rebakes without deleted prims.
        for nid in &affected_bodies {
            self.mark_node_body_vol_dirty(*nid);
        }
        self.mark_outliner_dirty();
    }

    /// THE single reorder entry point (§8 of SUBBODY_REORDER_PLAN). Move `dragged` to
    /// `insert_index` within `body`'s unified `child_order`. Used by EVERY reorder path
    /// (prim DnD, sub-body DnD, keyboard hierarchy reorder) so they share one code path that
    /// CANNOT forget the step-4 `mark_node_body_vol_dirty` (the thin-slab bake bug class).
    /// Caller is responsible for the surrounding `transaction()`/episode (undo).
    pub(crate) fn reorder_child_in_body(
        &mut self,
        body: NodeId,
        dragged: core_math::scene_graph::BodyChildRef,
        insert_index: usize,
    ) {
        self.scene.scene_graph.reorder_child_order(body, dragged, insert_index);
        self.sync_flat_cache();
        // Reorder changes the boolean eval chain → structural. SCENE alone does NOT bake
        // (process_dirty skips trigger_body_vol_bake when no body is in dirty_bodies), so in
        // volume-only mode the surface stays stale ("thin slab"). Mark the body volume-dirty
        // (Structural Mutation Checklist step 4) so the bake fires.
        self.mark_node_body_vol_dirty(body);
        self.mark_dirty(DirtyFlags::SCENE);
        self.mark_outliner_dirty();
    }

    // (reorder_prim_in_graph — the flat-index reorder shim — was DELETED with the legacy
    // between-row drop_target path: all reorder/move drops now resolve to a child_order slot
    // and apply through move_prims_to_body_slot / reorder_child_in_body.)

    /// Convert a prim's body-local position/rotation from `src_body` frame into `dst_body`
    /// frame so its **world** pose is preserved. No-op when `BODY_LOCAL_PRIMS` is off or
    /// when src == dst. Without this, reparent / dissolve leave positions in the old body's
    /// local space → wrong world placement → empty/wrong bake bounds → prim vanishes in
    /// viewport and PathTrace (stale body UID / mesh mapping symptom).
    pub(crate) fn convert_prim_body_local(
        &self,
        prim: &mut SdfPrimitive,
        src_body: NodeId,
        dst_body: NodeId,
    ) {
        if !Self::BODY_LOCAL_PRIMS || src_body == dst_body {
            return;
        }
        let src_bt = self
            .scene
            .scene_graph
            .body_world_transform(src_body)
            .unwrap_or_default();
        // New empty body may not yet have a transform entry — default identity.
        let dst_bt = self
            .scene
            .scene_graph
            .body_world_transform(dst_body)
            .unwrap_or_default();
        let world_pos = src_bt.local_to_world(prim.position);
        prim.position = dst_bt.world_to_local(world_pos);
        // Euler deg → quat (same convention as rest of app: XYZ deg).
        let prim_q = glam::Quat::from_euler(
            glam::EulerRot::XYZ,
            prim.rotation_deg.x.to_radians(),
            prim.rotation_deg.y.to_radians(),
            prim.rotation_deg.z.to_radians(),
        );
        let world_q = src_bt.rotation * prim_q;
        let local_q = dst_bt.rotation.inverse() * world_q;
        let (rx, ry, rz) = local_q.to_euler(glam::EulerRot::XYZ);
        prim.rotation_deg = Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
        // Body scale is usually 1; if non-uniform, keep prim.scale as authored local size
        // (shape extents) — do not bake body scale into prim.scale.
    }

    /// Move a prim from its current body to a different body at a specific position.
    ///
    /// **BODY_LOCAL_PRIMS:** prim.position / rotation are body-local. Must convert
    /// source-local → world → target-local before insert, or the prim jumps / vanishes
    /// (wrong world placement → empty bake bounds → PT BLAS never lands / frustum cull).
    pub(crate) fn reparent_prim_to_body(&mut self, flat_idx: usize, target_body_nid: NodeId, insert_pos: Option<usize>) {
        let (source_nid, source_local) = match self.scene.prim_body_map.get(flat_idx) {
            Some(&entry) => entry,
            None => return,
        };
        if source_nid == target_body_nid { return; }
        let mut prim = {
            let prims = match self.scene.scene_graph.body_primitives_mut(source_nid) {
                Some(p) => p,
                None => return,
            };
            if source_local >= prims.len() { return; }
            prims.remove(source_local)
        };
        // Preserve world pose across the body change (body UID map / PT mesh stay coherent).
        self.convert_prim_body_local(&mut prim, source_nid, target_body_nid);
        let moved_uid = prim.prim_uid;
        if let Some(target_prims) = self.scene.scene_graph.body_primitives_mut(target_body_nid) {
            let pos = insert_pos.unwrap_or(target_prims.len()).min(target_prims.len());
            target_prims.insert(pos, prim);
        }
        // child_order: drop the moved prim's ref from the SOURCE body (now dangling), and add it
        // to the TARGET body's order at the END of its prim run (sensible reparent placement).
        self.scene.scene_graph.ensure_body_child_order(source_nid);
        self.scene.scene_graph.ensure_body_child_order(target_body_nid);
        // ensure_body_child_order placed the new uid among the target's prims already (prims-first
        // synthesis); leave it there. No extra move needed.
        let _ = moved_uid;
        self.recalculate_body_origin(source_nid);
        self.recalculate_body_origin(target_body_nid);
        self.sync_flat_cache();
        self.mark_node_body_vol_dirty(source_nid);
        self.mark_node_body_vol_dirty(target_body_nid);
        // Force PathTrace to re-snapshot both bodies (hash + seen + in-flight pending).
        self.invalidate_pt_sdf_snapshot(source_nid);
        self.invalidate_pt_sdf_snapshot(target_body_nid);
        self.mark_dirty(DirtyFlags::SCENE);
        self.mark_outliner_dirty();
    }

    /// Move prims (by stable uid) to a specific SLOT in `target_body`'s unified `child_order`
    /// (the boolean chain order). Works same-body (pure reorder) AND cross-body (reparent +
    /// place at the slot) — the outliner drop-slot path for slots adjacent to sub-body rows.
    ///
    /// Composes the two canonical ops: `reparent_prim_to_body` (full Structural Mutation
    /// Checklist for BOTH bodies) + `reorder_child_in_body` (the ONE child_order reorder op).
    /// The insert slot is anchored to the ref currently AT `insert_index` (skipping refs being
    /// moved), so multi-prim blocks land contiguously before the anchor in selection order.
    /// Caller is responsible for the surrounding `transaction()` (one undo entry per drop).
    pub(crate) fn move_prims_to_body_slot(
        &mut self,
        prim_uids: &[u32],
        target_body: NodeId,
        insert_index: usize,
    ) {
        use crate::outliner_drop_slots::{slot_anchor, slot_insert_position};
        use core_math::scene_graph::BodyChildRef;
        self.scene.scene_graph.ensure_body_child_order(target_body);
        let moved: Vec<BodyChildRef> = prim_uids.iter().map(|&u| BodyChildRef::Prim(u)).collect();
        // Anchor: first ref at/after the slot that is NOT itself being moved (None = end).
        let anchor = slot_anchor(
            &self.scene.scene_graph.body_child_order(target_body),
            insert_index,
            &moved,
        );
        for &uid in prim_uids {
            // Fresh flat index each iteration — reparent rebuilds the flat cache
            // (CLAUDE "Stable IDs, Not Indices").
            let fi = match self.prim_index_by_uid(uid) { Some(fi) => fi, None => continue };
            let src = match self.scene.prim_body_map.get(fi) { Some(&(nid, _)) => nid, None => continue };
            if src != target_body {
                self.reparent_prim_to_body(fi, target_body, None);
            }
            let dragged = BodyChildRef::Prim(uid);
            let order = self.scene.scene_graph.body_child_order(target_body);
            let pos = slot_insert_position(&order, dragged, anchor);
            self.reorder_child_in_body(target_body, dragged, pos);
        }
    }

    // ---- Flat cache sync -------------------------------------------------------

    /// Rebuild `self.scene.primitives` and `self.scene.prim_body_map` from the scene graph.
    /// Call after any mutation that changes primitives through the graph.
    pub(crate) fn sync_flat_cache(&mut self) {
        profile_scope!("sync_flat_cache");
        let _t0 = std::time::Instant::now();
        let prev = std::mem::take(&mut self.scene.primitives);
        self.scene.prim_body_map.clear();
        for (nid, idx, prim) in self.scene.scene_graph.all_primitives_ordered() {
            self.scene.prim_body_map.push((nid, idx));
            self.scene.primitives.push(prim.clone());
        }
        // Graph-direct prim writes (body-wide material assign, import, …) reach the flat cache
        // only here — report material changes so the raster re-gathers resolved materials.
        // Positional pairing: a membership change pairs unrelated prims and bumps spuriously,
        // which only costs one structural re-gather (membership changes rebuild it anyway).
        if prev.len() != self.scene.primitives.len() {
            self.scene.scene_graph.bump_material_generation();
        } else {
            for (old, new) in prev.iter().zip(self.scene.primitives.iter()) {
                if self.scene.scene_graph.note_prim_material_write(old, new) {
                    break;
                }
            }
        }
        // Rebuild O(1) uid→index lookup cache
        self.scene.prim_uid_to_index.clear();
        for (i, prim) in self.scene.primitives.iter().enumerate() {
            self.scene.prim_uid_to_index.insert(prim.prim_uid, i);
        }
        if self.ui.show_perf_hud {
            self.perf.sync_flat_cache_ms += _t0.elapsed().as_secs_f64() * 1000.0;
            self.perf.sync_flat_cache_count += 1;
        }
    }

    /// Write position/rotation/scale to both flat cache and scene graph.
    /// Use during interactive drags for real-time viewport feedback.
    pub(crate) fn write_prim_transform(&mut self, idx: usize, pos: glam::Vec3, rot: glam::Vec3, scale: glam::Vec3) {
        self.write_prim_transform_ex(idx, pos, rot, scale, false);
    }

    /// Like `write_prim_transform`, but `force_field` disables the rigid-translate fast
    /// path (used when elongation / shape domain warps changed — field must re-bake).
    pub(crate) fn write_prim_transform_ex(
        &mut self,
        idx: usize,
        pos: glam::Vec3,
        rot: glam::Vec3,
        scale: glam::Vec3,
        force_field: bool,
    ) {
        if idx < self.scene.primitives.len() {
            // Terrain tiles (Phase 1): clamp prim centre into tile AABB on every write —
            // covers create, gizmo drag, and numeric property edits (L2 lattice invariant).
            let pos = if let Some(&(nid, _)) = self.scene.prim_body_map.get(idx) {
                self.clamp_prim_pos_for_terrain_body(nid, pos)
            } else {
                pos
            };
            // Skip a NO-OP transform write. A gizmo held still (or a leaked drag episode) calls this
            // every frame with the SAME pos/rot/scale; without this guard it re-marks the identical
            // dirty region each frame → a continuous re-bake loop (constant [PartialBake] region
            // scrolling, continuous GPU + readback churn even when nothing moved). Exact equality is
            // correct: the gizmo derives a deterministic pos from the mouse, so a still mouse → same value.
            // Exception: force_field (elongation-only edit) still needs a dirty mark even if
            // pos/rot/scale are unchanged.
            {
                let p = &self.scene.primitives[idx];
                if !force_field && p.position == pos && p.rotation_deg == rot && p.scale == scale {
                    return;
                }
            }
            // [HealTrace] writer provenance: a transform write landing inside the heal window
            // is the phantom edit feeding the settle loop — print the actual value delta so the
            // drifting field (and from its pattern, the caller) is identifiable in the owner log.
            // Parked behind crate::perf_spam_on() after the phantom-edit loop was resolved (2026-07-09).
            // 2026-07-16: also fires during the escalation window (`escalation_hot`) — the idle
            // escalate-loop's driver is a per-repaint hash-changing edit; this names a drifting
            // transform writer (value delta pattern identifies the caller) in the owner log.
            // (Throttled 1-in-30 in the hot window, same as the mirror print — the tripwire
            // already named the writer class; unthrottled it spams per drag tick.)
            let escalation_hot = self.scene.prim_body_map.get(idx)
                .map_or(false, |&(n, _)| self.cache.body_vol_cache.escalation_hot(n))
                && {
                    use std::cell::Cell;
                    thread_local! { static HOTT: Cell<u32> = Cell::new(0); }
                    HOTT.with(|n| { let v = n.get().wrapping_add(1); n.set(v); v % 30 == 1 })
                };
            if (crate::perf_spam_on() && self.cache.body_vol_cache.heal_tracing()) || escalation_hot {
                let p = &self.scene.primitives[idx];
                eprintln!(
                    "[HealTrace] write_prim_transform idx={} uid={} dpos={:.6?} drot={:.4?} dscale={:.6?} new_pos={:.6?}",
                    idx, p.prim_uid, pos - p.position, rot - p.rotation_deg, scale - p.scale, pos);
            }
            // Capture old position + tight AABB BEFORE mutation so we can dirty the
            // vacated region (prevents ghost voxels at the old location).
            let old_pos = self.scene.primitives[idx].position;
            let old_he = self.scene.primitives[idx].bounding_aabb_half_extents();
            // P1-#6b: rigidity verdict for the rigid-translate fast path — a pure translation
            // (rotation + scale bit-identical) of every prim in a body shifts the whole
            // body-local field; the cache then moves the grid WITH the prim instead of baking.
            // NEVER pure-translate when force_field (elongation domain warp).
            let pure_translate = !force_field
                && self.scene.primitives[idx].rotation_deg == rot
                && self.scene.primitives[idx].scale == scale;

            self.scene.primitives[idx].position = pos;
            self.scene.primitives[idx].rotation_deg = rot;
            self.scene.primitives[idx].scale = scale;

            let new_he = self.scene.primitives[idx].bounding_aabb_half_extents();

            if let Some(&(nid, body_idx)) = self.scene.prim_body_map.get(idx) {
                if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                    if body_idx < prims.len() {
                        prims[body_idx].position = pos;
                        prims[body_idx].rotation_deg = rot;
                        prims[body_idx].scale = scale;
                    }
                }
                // Partial bake: dirty old region + new region instead of full body.
                // Bake dispatch is handled by has_pending_dirty() in poll_body_vol_cache.
                // Only set prim_buf_stale for GPU picking/analytical render — do NOT set
                // SCENE (would trigger scene_modified, light re-serialize, and the bake
                // gate in process_dirty blocks during drag anyway).
                if self.cache.use_body_vol_cache && !self.cache.suppress_body_vol_dirty {
                    // Get body-level blend_k to expand dirty region into neighboring prims'
                    // smooth blend zones. Without this, moving prim A near prim B causes B
                    // to flicker because B's voxels in the blend zone aren't rebaked.
                    let body_blend_k = self.scene.scene_graph.get(nid)
                        .and_then(|n| match &n.payload {
                            core_math::scene_graph::NodePayload::SdfBody { body_op, .. } =>
                                Some(body_op.blend_k()),
                            _ => None,
                        })
                        .unwrap_or(0.0);
                    // Also collect blend_k from individual prim operations
                    let prim_blend_k = self.scene.primitives[idx].operation.blend_k();
                    let blend_expand = body_blend_k.max(prim_blend_k);
                    let blend_pad = Vec3::splat(blend_expand);
                    // A prim edit inside a NESTED sub-body must re-bake the TOP-LEVEL ancestor (the
                    // body that actually owns the grid + rendered mesh). Marking the grid-less sub-body
                    // directly just floods `no_grid_in_mark_prim_dirty_aabb` every frame and NEVER bakes
                    // — nested bodies are subsumed into the ancestor's subtree bake (bake_dispatch skips
                    // them). Route through the climbing mark (mark_node_body_vol_dirty ascends to the
                    // top ancestor) for a full ancestor re-bake so the edit actually shows. Top-level
                    // bodies keep the tight partial-region path (no regression to the common case).
                    if self.scene.scene_graph.parent_body(nid).is_some() {
                        self.mark_node_body_vol_dirty(nid);
                    } else {
                        // P1-#6b: the transform-writer mark — same two AABB regions as before,
                        // plus the rigidity note (pure translate) or poison (rotate/scale).
                        let prim_uid = self.scene.primitives[idx].prim_uid;
                        self.cache.body_vol_cache.mark_prim_transform_dirty(
                            nid, prim_uid,
                            old_pos, old_he + blend_pad,
                            pos, new_he + blend_pad,
                            if pure_translate { Some(pos - old_pos) } else { None },
                        );
                    }
                    self.cache.prim_buf_stale = true;
                    // Track which body changed for partial re-serialization.
                    // If a different body was already dirty, fall back to full.
                    self.cache.prim_buf_dirty_body = match self.cache.prim_buf_dirty_body {
                        None => Some(nid),
                        Some(prev) if prev == nid => Some(nid),
                        Some(_) => None, // multiple bodies dirty → full serialize
                    };
                }
            }
        }
    }

    /// Write only position to both flat cache and scene graph.
    #[allow(dead_code)] // FUTURE: position-only writes
    pub(crate) fn write_prim_position(&mut self, idx: usize, pos: glam::Vec3) {
        if idx < self.scene.primitives.len() {
            let rot = self.scene.primitives[idx].rotation_deg;
            let scale = self.scene.primitives[idx].scale;
            self.write_prim_transform(idx, pos, rot, scale);
        }
    }

    /// Apply a mutation to a primitive in both the flat cache and the scene graph.
    /// Use instead of writing `self.scene.primitives[idx].field = value` directly.
    ///
    /// ```ignore
    /// self.write_prim(idx, |p| p.visible = false);
    /// self.write_prim(idx, |p| p.operation = BooleanOp::Subtract { k: 0.0 });
    /// ```
    pub(crate) fn write_prim<F: FnOnce(&mut SdfPrimitive)>(&mut self, idx: usize, f: F) {
        if idx >= self.scene.primitives.len() { return; }
        // If this prim is a master, push content to linked instances after the write.
        // Capture position so one-sided face extrude (shape + recenter) also recenters
        // instances — otherwise they only grow from their center (both sides).
        // Pure content edits leave position unchanged → delta is zero.
        let master_uid = self.scene.primitives[idx].prim_uid;
        let push_instances = self.is_master(idx);
        let pos_before = self.scene.primitives[idx].position;
        f(&mut self.scene.primitives[idx]);
        self.mirror_prim_to_graph(idx);
        if push_instances {
            let pos_delta = self.scene.primitives[idx].position - pos_before;
            self.sync_instances_of_ex(master_uid, pos_delta);
        }
    }

    /// Set the boolean op on prims (by uid) in ONE undo transaction. THE single
    /// mutation path for prim boolean-op changes — `AppCommand::SetBooleanOp`
    /// (Boolean card / outliner / context menu / macros) and the X boolean pie
    /// both route here. Uids that no longer resolve are skipped (macro replay
    /// on a mutated scene).
    /// Returns `true` if at least one prim op actually changed (and an undo entry was pushed).
    pub(crate) fn set_prims_boolean_op(&mut self, changes: &[(u32, BooleanOp)]) -> bool {
        // Drop no-ops (same op already on the prim) so re-picking the pie item does not
        // push an empty undo entry.
        let resolved: Vec<(usize, BooleanOp)> = changes.iter()
            .filter_map(|(uid, op)| {
                let i = self.prim_index_by_uid(*uid)?;
                if i < self.scene.primitives.len() && self.scene.primitives[i].operation == *op {
                    return None;
                }
                Some((i, op.clone()))
            })
            .collect();
        if resolved.is_empty() { return false; }
        self.transaction("Change boolean", |s| {
            for (idx, op) in resolved {
                s.write_prim(idx, |p| p.operation = op);
            }
            s.rebake_no_undo();
        });
        true
    }

    /// True when corner/profile bevel fields are already at the default (zero radii, 45° chamfer).
    /// Used so "Reset bevel" does not push an empty undo.
    pub(crate) fn prim_corner_bevel_needs_reset(p: &core_math::primitives::SdfPrimitive) -> bool {
        const EPS: f32 = 1e-6;
        if let core_math::primitives::SdfShape::Box { bevel, .. } = &p.shape {
            if bevel.radii.iter().any(|r| r.abs() > EPS) { return true; }
        }
        if p.bevel_profile_top.abs() > EPS
            || p.bevel_profile_bot.abs() > EPS
            || p.bevel_profile_vert.abs() > EPS
        {
            return true;
        }
        if (p.chamfer_angle_top - 45.0).abs() > EPS
            || (p.chamfer_angle_bot - 45.0).abs() > EPS
            || (p.chamfer_angle_vert - 45.0).abs() > EPS
        {
            return true;
        }
        if matches!(p.shape, core_math::primitives::SdfShape::Cylinder { .. })
            && (p.cyl_bevel.top.abs() > EPS || p.cyl_bevel.bot.abs() > EPS)
        {
            return true;
        }
        false
    }

    /// Zero corner/profile bevels on a prim (box radii, profiles, chamfer angles, cylinder ends).
    pub(crate) fn reset_prim_corner_bevel(p: &mut core_math::primitives::SdfPrimitive) {
        if let core_math::primitives::SdfShape::Box { bevel, .. } = &mut p.shape {
            for r in &mut bevel.radii { *r = 0.0; }
            for m in &mut bevel.modes {
                *m = core_math::primitives::BevelMode::None;
            }
        }
        p.bevel_profile_top = 0.0;
        p.bevel_profile_bot = 0.0;
        p.bevel_profile_vert = 0.0;
        p.chamfer_angle_top = 45.0;
        p.chamfer_angle_bot = 45.0;
        p.chamfer_angle_vert = 45.0;
        if matches!(p.shape, core_math::primitives::SdfShape::Cylinder { .. }) {
            p.cyl_bevel.top = 0.0;
            p.cyl_bevel.bot = 0.0;
            p.cyl_bevel.top_mode = core_math::primitives::BevelMode::None;
            p.cyl_bevel.bot_mode = core_math::primitives::BevelMode::None;
        }
    }

    /// Reset corner bevels on one prim with ONE undo entry. No-op (no history) if already default.
    /// Returns true when a reset was committed.
    pub(crate) fn reset_prim_corner_bevel_undoable(&mut self, idx: usize) -> bool {
        if idx >= self.scene.primitives.len() { return false; }
        if !Self::prim_corner_bevel_needs_reset(&self.scene.primitives[idx]) {
            return false;
        }
        self.transaction("Reset bevel", |s| {
            if idx < s.scene.primitives.len() {
                Self::reset_prim_corner_bevel(&mut s.scene.primitives[idx]);
                s.mirror_prim_to_graph(idx);
                s.rebake_no_undo();
            }
        });
        true
    }

    /// Zero the boolean blend-k / seam width on one prim. No-op when already ~0.
    pub(crate) fn reset_prim_blend_k_undoable(&mut self, idx: usize) -> bool {
        if idx >= self.scene.primitives.len() { return false; }
        if self.scene.primitives[idx].operation.blend_k().abs() < 1e-6 {
            return false;
        }
        self.transaction("Reset bevel", |s| {
            s.write_prim(idx, |p| p.operation.set_blend_k(0.0));
            s.bool_edge_dirty = true;
            s.rebake_no_undo();
        });
        true
    }

    /// Set a prim's base shape (by uid), preserving every other field — position, scale,
    /// name, material, modifiers AND `prim_uid` (selection/undo/instancing all key off the
    /// uid; a shape swap must never mint a new one).
    ///
    /// This is the PLAIN swap. The reset variant (`*p = SdfPrimitive::new(..)`, restoring
    /// uid/name/scale) is a different operation and still lives at menus.rs:577.
    pub(crate) fn set_prim_shape(&mut self, uid: u32, shape: core_math::primitives::SdfShape) {
        let Some(idx) = self.prim_index_by_uid(uid) else { return };
        self.transaction("Change shape", |s| {
            s.write_prim(idx, |p| p.shape = shape);
            s.rebake_no_undo();
        });
    }

    /// Push a fresh modifier onto a prim's stack (by uid), with the per-type defaults from
    /// the ONE shared source (`ui_panels::default_modifier`).
    ///
    /// Callable from anywhere — unlike the properties panel's inline add, which writes the
    /// flat cache directly and depends on the panel epilogue (`properties.rs:84`) to run
    /// `mirror_prim_to_graph` afterwards. Outside that panel the epilogue never runs, so an
    /// inline copy here would leave the modifier in the GPU cache and never in the graph —
    /// a silent source-of-truth violation. `write_prim` mirrors, closing that gap.
    pub(crate) fn add_prim_modifier(&mut self, uid: u32, mt: core_math::primitives::ModifierType) {
        let Some(idx) = self.prim_index_by_uid(uid) else { return };
        self.transaction("Add modifier", |s| {
            s.write_prim(idx, |p| {
                p.modifiers.push(crate::ui_panels::default_modifier(mt));
                p.sync_modifiers_to_gpu_fields();
            });
            s.rebake_no_undo();
        });
    }

    /// Copy a single primitive from the flat cache to the scene graph.
    /// Use after egui widgets have already mutated `self.scene.primitives[idx]` via `&mut`.
    pub(crate) fn mirror_prim_to_graph(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() { return; }
        if let Some(&(nid, body_idx)) = self.scene.prim_body_map.get(idx) {
            // Capture the GRAPH copy's boolean-op CATEGORY + visibility BEFORE the overwrite
            // below. A change to either is NOT a localized geometric edit — it restructures the
            // body's boolean chain and changes which prims define the bake silhouette
            // (`compute_body_bake_bounds` counts only the base + ADDITIVE prims; a
            // Union→Subtract flip removes this prim from the bounds set and carves the existing
            // field everywhere it overlaps; an Intersect restricts the whole body to the prim's
            // volume). A partial region bake keyed on this prim's own AABB cannot represent that
            // — the field is left stale (the whole surface vanishes until a resolution change
            // forces a full re-tighten). So detect the structural change here and route to a
            // FULL body bake. We compare op CATEGORY (not the full op), so a pure blend-`k` drag
            // — same category, different k — still takes the cheap partial path.
            // Clone the GRAPH copy before the overwrite: feeds (a) the structural check,
            // (b) the bake-identity no-op guard below, (c) the [HealTrace] field diff.
            let prev_copy = self.scene.scene_graph.body_primitives(nid)
                .and_then(|prims| prims.get(body_idx))
                .cloned();
            let prev_structural = prev_copy.as_ref()
                .map(|p| (p.operation.category(), p.visible));

            if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                if body_idx < prims.len() {
                    prims[body_idx] = self.scene.primitives[idx].clone();
                }
            }
            // Material reassign / embedded-material edit → raster re-gathers its cached
            // resolved materials. Must run BEFORE the bake-identity early return below:
            // `hash_prim_slice` ignores materials, so a pure material write IS bake-identical.
            let new_prim = &self.scene.primitives[idx];
            match &prev_copy {
                Some(prev) => { self.scene.scene_graph.note_prim_material_write(prev, new_prim); }
                None => self.scene.scene_graph.bump_material_generation(),
            }
            // Boolean-chain change (op category or visibility) → full bake; otherwise
            // a tight partial region bake around the edited prim.
            let structural_change = match prev_structural {
                Some((prev_cat, prev_visible)) =>
                    prev_cat != new_prim.operation.category() || prev_visible != new_prim.visible,
                // No prior graph copy (shouldn't happen for an existing prim) → be safe, full bake.
                None => true,
            };
            let uid = new_prim.prim_uid; // capture before the &mut self dirty calls (ends the borrow)
            // NO-OP GUARD — the `write_prim_transform` pattern (prim_actions.rs ~306), missing
            // here since day one: if the flat copy is BAKE-IDENTICAL to the graph copy (same
            // `hash_prim_slice` the dispatch dedup trusts — identical semantics by construction),
            // the graph sync above still happened (names/UI fields stay mirrored) but NO dirty
            // mark fires. Without this, any egui panel that reports a spurious change every
            // repaint (clamp/normalize/roundtrip) sustains a repaint→mark→bake→repaint loop
            // at idle — each identical mirror queues a redundant bake.
            let bake_identical = !structural_change && prev_copy.as_ref().map_or(false, |prev| {
                crate::bake_utils::hash_prim_slice(std::slice::from_ref(prev))
                    == crate::bake_utils::hash_prim_slice(std::slice::from_ref(new_prim))
            });
            // [HealTrace] writer provenance — PERF: full Debug of BoxBevel on every slider tick
            // was burning layout/frame time (live log 2026-07-21). Only when crate::perf_spam_on() is on
            // (and still throttled 1-in-30 during escalation_hot; full rate under heal_tracing).
            if crate::perf_spam_on() && !bake_identical {
                let hot_sample = {
                    use std::cell::Cell;
                    thread_local! { static HOTN: Cell<u32> = Cell::new(0); }
                    self.cache.body_vol_cache.escalation_hot(nid)
                        && HOTN.with(|n| {
                            let v = n.get().wrapping_add(1);
                            n.set(v);
                            v % 30 == 1
                        })
                };
                if self.cache.body_vol_cache.heal_tracing() || hot_sample {
                    if let Some(prev) = &prev_copy {
                        let mut d: Vec<String> = Vec::new();
                        if prev.position != new_prim.position {
                            d.push(format!("pos {:?}→{:?}", prev.position, new_prim.position));
                        }
                        if prev.rotation_deg != new_prim.rotation_deg {
                            d.push(format!(
                                "rot {:?}→{:?}",
                                prev.rotation_deg, new_prim.rotation_deg
                            ));
                        }
                        if prev.scale != new_prim.scale {
                            d.push(format!("scale {:?}→{:?}", prev.scale, new_prim.scale));
                        }
                        if prev.elongation != new_prim.elongation {
                            d.push(format!(
                                "elong {:?}→{:?}",
                                prev.elongation, new_prim.elongation
                            ));
                        }
                        let (ps, ns) = (format!("{:?}", prev.shape), format!("{:?}", new_prim.shape));
                        if ps != ns {
                            d.push(format!("shape {ps} → {ns}"));
                        }
                        let (po, no) =
                            (format!("{:?}", prev.operation), format!("{:?}", new_prim.operation));
                        if po != no {
                            d.push(format!("op {po} → {no}"));
                        }
                        if d.is_empty() {
                            d.push("other field (modifier/material/…)".into());
                        }
                        eprintln!(
                            "[HealTrace] mirror_prim_to_graph uid={uid} CHANGED: {}",
                            d.join("; ")
                        );
                    }
                }
            }
            if bake_identical { return; } // graph synced; unchanged data must never queue a bake
            // A REAL prim change must also re-serialize the GPU prim buffer — and its sidecar
            // outline buffers (mesh-grid + CurveExtrude clip-curves), which viewport_render packs
            // only inside the serialize block. Without this, per-frame `write_prim` edits (curve
            // point / bevel-radius drags) update the bake but leave the prim_outline cutter ghost
            // frozen at the pre-drag profile. Same tracking as `write_prim_transform` (~:401).
            self.cache.prim_buf_stale = true;
            self.cache.prim_buf_dirty_body = match self.cache.prim_buf_dirty_body {
                None => Some(nid),
                Some(prev) if prev == nid => Some(nid),
                Some(_) => None, // multiple bodies dirty → full serialize
            };
            // Mark the affected body dirty for the volume bake. Structural boolean-chain
            // edits need a full re-tighten; plain field edits use a targeted partial bake.
            if self.cache.use_body_vol_cache && !self.cache.suppress_body_vol_dirty {
                // A prim inside a NESTED sub-body is subsumed into its TOP-LEVEL ancestor's grid +
                // rendered mesh (trigger_bake skips nested bodies). Marking the grid-less sub-body
                // directly hits the `no_grid_in_mark_prim_dirty` fallback every frame and NEVER
                // re-bakes — so a properties edit (blend_k, radius, material, op flip) on a nested
                // prim is invisible: the "blend doesn't work inside a sub-body" bug. Route to the
                // ancestor via the climbing mark, exactly as write_prim_transform does for a
                // nested-body move. Top-level bodies keep the tight partial-region path.
                if self.scene.scene_graph.parent_body(nid).is_some() {
                    self.mark_node_body_vol_dirty(nid); // climbs to the top-level ancestor (full bake)
                    if structural_change {
                        // One-shot structural event — dispatch promptly, don't wait out the 40ms
                        // debounce (matches the top-level structural branch below).
                        self.cache.body_vol_cache.clear_bake_debounce();
                    }
                } else if structural_change {
                    self.cache.body_vol_cache.mark_body_dirty(nid);
                    // One-shot structural event — bypass the 40ms bake debounce so the
                    // full re-bake dispatches immediately (same pattern as add_prim_to_graph).
                    self.cache.body_vol_cache.clear_bake_debounce();
                } else {
                    self.cache.body_vol_cache.mark_prim_dirty(nid, uid, &self.scene.scene_graph);
                }
            }
        }
    }

    // ---- SDF body group / dissolve ---------------------------------------------

    /// Group selected SDF primitives into a new child SdfBody under active_body_nid.
    /// Group the selected prims into a new child sub-body. `drag_target_uid` is `Some` only when this
    /// comes from dragging one prim ONTO another in the outliner: the new sub-body then INHERITS the
    /// drag target prim's boolean op, and the target prim is reset to Union inside the group (so the
    /// group carries the boolean relationship to the parent and the target is not applied twice — the
    /// double-negative). Menu / keyboard "Group selection" pass `None` → the group is a plain Union.
    pub(crate) fn group_selected_into_body(&mut self, drag_target_uid: Option<u32>) {
        let selected: Vec<usize> = self.selected_prim_indices_unified();
        if selected.is_empty() {
            self.toast("Select at least 1 primitive to group");
            return;
        }

        // GATE: all participants must share ONE body — whichever it is — and THAT body
        // becomes the group parent. active_body_nid is only the fallback for ambiguity
        // (no resolvable body at all). The old gate rejected whenever the shared body
        // != active_body_nid, which broke Ctrl+G / drop-prim-onto-prim after a body-header
        // click silently moved active_body mid-multi-select.
        let active_nid = match crate::scene_actions::shared_prim_body(
            &self.scene.prim_body_map, &selected,
        ) {
            Ok(Some(nid)) => nid,
            Ok(None) => self.scene.active_body_nid, // ambiguity fallback
            Err(()) => {
                self.toast("All selected prims must be in the same body");
                return;
            }
        };

        let selected_len = selected.len();
        self.transaction("Group into body", |s| {
            // Collect body_idx values for selected prims, sorted descending for safe removal
            let mut body_indices: Vec<(usize, usize)> = selected.iter()
                .filter_map(|&fi| {
                    if fi < s.scene.prim_body_map.len() {
                        let (_, body_idx) = s.scene.prim_body_map[fi];
                        Some((fi, body_idx))
                    } else { None }
                })
                .collect();
            body_indices.sort_by(|a, b| b.1.cmp(&a.1)); // descending by body_idx

            // child_order: remember WHERE the group should land in the parent's stack — the
            // earliest position any extracted prim occupied (so the new sub-body replaces the
            // selection in place, NOT forced to the top).
            let extracted_uids: Vec<u32> = body_indices.iter()
                .filter_map(|&(fi, _)| s.scene.primitives.get(fi).map(|p| p.prim_uid))
                .collect();
            let parent_order_before = s.scene.scene_graph.body_child_order(active_nid);
            let insert_slot = extracted_uids.iter()
                .filter_map(|uid| parent_order_before.iter()
                    .position(|&r| r == core_math::scene_graph::BodyChildRef::Prim(*uid)))
                .min()
                .unwrap_or(parent_order_before.len());

            // Extract prims from the active body (remove highest index first)
            let mut extracted_prims: Vec<SdfPrimitive> = Vec::new();
            if let Some(prims) = s.scene.scene_graph.body_primitives_mut(active_nid) {
                for &(_, body_idx) in &body_indices {
                    if body_idx < prims.len() {
                        extracted_prims.push(prims.remove(body_idx));
                    }
                }
            }
            extracted_prims.reverse(); // restore original order

            // Generate name
            let group_name = format!("Group.{:03}", { let id = s.scene.next_node_name_id; s.scene.next_node_name_id += 1; id });

            use core_math::primitives::BooleanOp;
            // BOOLEAN INHERITANCE (drag a prim ONTO another → group): the new sub-body takes the DRAG
            // TARGET prim's op, and the target prim is reset to Union INSIDE the group — so the group
            // carries the boolean relationship to the parent and the target is not applied twice (the
            // double-negative). `drag_target_uid == None` (menu/keyboard group) → plain Union.
            let mut group_op = BooleanOp::Union { k: 0.0 };
            if let Some(tuid) = drag_target_uid {
                if let Some(tp) = extracted_prims.iter_mut().find(|p| p.prim_uid == tuid) {
                    group_op = tp.operation.clone();             // group inherits the target's op
                    tp.operation = BooleanOp::Union { k: 0.0 };  // target becomes the group's base
                }
            }

            // Seed new sub-body origin from the parent so extracted prims (still in parent-local
            // coords) stay correct under BODY_LOCAL_PRIMS. recalculate_body_origin then tightens
            // the pivot without jumping world positions.
            let parent_origin = s
                .scene
                .scene_graph
                .body_world_transform(active_nid)
                .map(|bt| bt.origin)
                .unwrap_or(Vec3::ZERO);
            let parent_rot = s
                .scene
                .scene_graph
                .body_world_transform(active_nid)
                .map(|bt| bt.rotation)
                .unwrap_or(glam::Quat::IDENTITY);
            let parent_scale = s
                .scene
                .scene_graph
                .body_world_transform(active_nid)
                .map(|bt| bt.scale)
                .unwrap_or(Vec3::ONE);

            // Create child SdfBody (origin will be recalculated below)
            let new_body = s.scene.scene_graph.add_child(
                active_nid,
                core_math::scene_graph::NodePayload::SdfBody {
                    name: group_name.clone(),
                    primitives: extracted_prims,
                    mesh_cache_id: None,
                    body_op: group_op,
                    origin: parent_origin,
                    body_rotation: parent_rot,
                    body_scale: parent_scale,
                    mirror_flags: 0,
                    mirror_sign: 0,
                    mirror_center: Vec3::ZERO,
                    bevel_radius: 0.0,
                    blend_profile: core_math::primitives::BlendProfile::default(),
                    material: core_math::primitives::PbrMaterial::default(),
                    child_order: Vec::new(),
                mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
            );

            // child_order: synthesise the new body's own order (its extracted prims), then in the
            // PARENT drop the moved prims' refs and insert the new Body ref where the group was.
            s.scene.scene_graph.ensure_body_child_order(new_body);
            s.scene.scene_graph.ensure_body_child_order(active_nid); // drops the now-dangling prim refs
            s.scene.scene_graph.insert_child_order(
                active_nid,
                core_math::scene_graph::BodyChildRef::Body(new_body),
                insert_slot.min(s.scene.scene_graph.body_child_order(active_nid).len()),
            );

            s.recalculate_body_origin(new_body);
            s.recalculate_body_origin(active_nid);
            s.sync_flat_cache();
            s.mark_node_body_vol_dirty(new_body);
            s.mark_node_body_vol_dirty(active_nid);
            s.invalidate_pt_sdf_snapshot(new_body);
            s.invalidate_pt_sdf_snapshot(active_nid);
            s.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            let prim_word = if selected_len == 1 { "prim" } else { "prims" };
            s.toast(&format!(
                "Grouped {selected_len} {prim_word} into {group_name}"
            ));
        });
    }

    /// Extract the given prims (by stable UID) into a BRAND-NEW top-level SdfBody at the scene root.
    /// Used by the outliner "drag a prim to the root / empty area" gesture: a primitive cannot exist
    /// outside an SdfBody, so we spawn a container for it. The actual per-prim move reuses
    /// `reparent_prim_to_body`, which performs the full Structural Mutation Checklist (recalc origin +
    /// sync_flat_cache + mark_node_body_vol_dirty) for BOTH the source body AND the new body — so this
    /// helper never re-implements that checklist.
    ///
    /// Caller is responsible for the surrounding `transaction()` (one undo entry per drop).
    /// Returns the new body's NodeId, or None if no prims resolved.
    pub(crate) fn move_prims_to_new_root_body(&mut self, prim_uids: &[u32]) -> Option<NodeId> {
        // Resolve which uids are still live (fresh flat index) BEFORE we mutate anything.
        if prim_uids.iter().all(|&uid| self.prim_index_by_uid(uid).is_none()) {
            return None;
        }

        use core_math::primitives::BooleanOp;
        let name = format!("Body.{:03}", { let id = self.scene.next_node_name_id; self.scene.next_node_name_id += 1; id });
        let root = self.scene.scene_graph.root();
        // New empty top-level SdfBody (same payload shape as create_new_sdf_body).
        let new_body = self.scene.scene_graph.add_child(
            root,
            core_math::scene_graph::NodePayload::SdfBody {
                name: name.clone(),
                primitives: Vec::new(),
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: Vec3::ZERO,
                body_rotation: glam::Quat::IDENTITY,
                body_scale: Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: core_math::primitives::BlendProfile::default(),
                material: core_math::primitives::PbrMaterial::default(),
                child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
        );
        // Move each prim in reverse UID order, resolving a FRESH flat index each iteration:
        // reparent_prim_to_body rebuilds the flat cache, so pre-captured indices go stale
        // (CLAUDE "Stable IDs, Not Indices").
        for &uid in prim_uids.iter().rev() {
            if let Some(fi) = self.prim_index_by_uid(uid) {
                self.reparent_prim_to_body(fi, new_body, None);
            }
        }
        // The new body is now the editing scope for follow-up work.
        self.scene.active_body_nid = new_body;
        let ids: Vec<SceneItemId> = prim_uids.iter().map(|uid| SceneItemId::Prim(*uid)).collect();
        self.clear_selection_unified();
        self.select_after_create(&ids);
        self.mark_dirty(DirtyFlags::SCENE);
        self.mark_outliner_dirty();
        self.toast(&format!("Moved {} prim(s) into new {}", prim_uids.len(), name));
        Some(new_body)
    }

    /// Dissolve the given child SdfBody: move its prims to the parent body, remove the node.
    /// If the dissolved body is the active editing scope, exit to parent.
    pub(crate) fn dissolve_sdf_body(&mut self, body_nid: NodeId) {
        if body_nid == self.scene.sdf_body_nid {
            self.toast("Cannot dissolve the root body");
            return;
        }
        let parent_nid = self.scene.scene_graph.parent_body(body_nid)
            .unwrap_or(self.scene.sdf_body_nid);

        self.transaction("Dissolve body", |s| {
            // Move child body's prims to parent, converting body-local transforms so world
            // pose is preserved (same bug class as reparent_prim_to_body without convert).
            let mut child_prims: Vec<SdfPrimitive> = s.scene.scene_graph
                .body_primitives(body_nid)
                .map(|p| p.to_vec())
                .unwrap_or_default();
            for prim in &mut child_prims {
                s.convert_prim_body_local(prim, body_nid, parent_nid);
            }
            if let Some(parent_prims) = s.scene.scene_graph.body_primitives_mut(parent_nid) {
                parent_prims.extend(child_prims);
            }

            // Reparent grandchild bodies to the parent (preserve nesting)
            let grandchildren = s.scene.scene_graph.child_bodies(body_nid);
            for gc_nid in grandchildren {
                s.scene.scene_graph.reparent(gc_nid, parent_nid);
            }

            // Remove the empty body node and its volume cache entry
            s.cache.body_vol_cache.remove_body(body_nid);
            s.scene.scene_graph.remove(body_nid);
            // Drop any pending/landed PT snapshot keyed by the dissolved body UID.
            s.invalidate_pt_sdf_snapshot(body_nid);

            // child_order: the parent's `Body(body_nid)` ref is now dangling; the promoted prims
            // and reparented grandchildren are new. Reconcile drops the dangling ref and appends
            // the promoted children (prims-first, then bodies — at the end of the parent stack).
            s.scene.scene_graph.ensure_body_child_order(parent_nid);

            // Exit scope if we were inside the dissolved body
            if s.scene.active_body_nid == body_nid {
                s.scene.active_body_nid = parent_nid;
            }

            s.recalculate_body_origin(parent_nid);
            s.sync_flat_cache();
            s.mark_node_body_vol_dirty(parent_nid);
            s.invalidate_pt_sdf_snapshot(parent_nid);
            s.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            s.toast("Body dissolved \u{2014} prims moved to parent");
        });
    }

    // ---- Body origin recalculation ---------------------------------------------

    /// When true, prim.position is body-local (relative to body.origin).
    /// Used by recalculate_body_origin, add_primitive_immediate, and SetPosition commands.
    pub(crate) const BODY_LOCAL_PRIMS: bool = crate::BODY_LOCAL_PRIMS;

    /// Recalculate the origin of a single SdfBody based on the current `body_pivot_mode`.
    /// Skips `Custom` mode (user-placed origin) and bodies with no prims.
    ///
    /// With BODY_LOCAL_PRIMS=true, prim.position values are body-local (relative to
    /// body.origin). The centroid/bbox of local positions is a local offset from the
    /// current origin. We advance the world origin by that local offset and shift all
    /// prims by the negative offset to keep them at the same world positions.
    ///
    /// Because SdfBody currently only has translation (no rotation/scale), the
    /// local→world transform is: world_pos = origin + local_pos.
    pub(crate) fn recalculate_body_origin(&mut self, body_nid: NodeId) {
        if self.scene.body_pivot_mode == BodyPivotMode::Custom { return; }

        if Self::BODY_LOCAL_PRIMS {
            use core_math::scene_graph::BodyTransform;
            // Read the old body world transform (origin, rotation, scale).
            let old_bt: BodyTransform = match self.scene.scene_graph.body_world_transform(body_nid) {
                Some(bt) => bt,
                None => return,
            };
            // Compute local_centroid: the pivot point in body-local space.
            let local_centroid = {
                let prims = match self.scene.scene_graph.body_primitives(body_nid) {
                    Some(p) if !p.is_empty() => p,
                    _ => return,
                };
                match self.scene.body_pivot_mode {
                    BodyPivotMode::Centroid => {
                        let sum: Vec3 = prims.iter().map(|p| p.position).sum();
                        sum / prims.len() as f32
                    }
                    BodyPivotMode::WorldOrigin => {
                        // Target world origin = Vec3::ZERO.
                        // The local position that maps to world ZERO under old_bt.
                        old_bt.world_to_local(Vec3::ZERO)
                    }
                    BodyPivotMode::BoundingBox => {
                        let mut min = Vec3::splat(f32::MAX);
                        let mut max = Vec3::splat(f32::MIN);
                        for p in prims {
                            let s = p.scale.x.max(p.scale.y).max(p.scale.z);
                            min = min.min(p.position - Vec3::splat(s));
                            max = max.max(p.position + Vec3::splat(s));
                        }
                        (min + max) * 0.5
                    }
                    BodyPivotMode::Custom => unreachable!(),
                }
            };
            // New world origin: promote local_centroid into world space.
            let new_origin = old_bt.local_to_world(local_centroid);
            // Shift all prims so their world positions remain unchanged.
            if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_nid) {
                for p in prims.iter_mut() {
                    p.position -= local_centroid;
                }
            }
            // Sync body-level material from first prim and update the origin.
            let first_mat = self.scene.scene_graph.body_primitives(body_nid)
                .and_then(|p| p.first())
                .map(|p| p.material.clone());
            if let Some(node) = self.scene.scene_graph.get_mut(body_nid) {
                if let core_math::scene_graph::NodePayload::SdfBody { origin, material, mirror_center, .. } = &mut node.payload {
                    *origin = new_origin;
                    // The prims just moved by -local_centroid (above) to hold world position; the body-local
                    // Body-Mirror plane is anchored at `mirror_center` in that SAME frame, so it MUST move with
                    // them — otherwise the symmetry plane drifts by -local_centroid every time the centroid
                    // shifts (e.g. duplicating a prim, the user-reported creep). Same delta the prims + the
                    // volume cache (translate_body_cache below) already receive. No save/undo change.
                    *mirror_center -= local_centroid;
                    if let Some(mat) = first_mat {
                        *material = mat;
                    }
                }
            }
            eprintln!("[ACT] recalc_origin body={body_nid:?} new_origin={new_origin:?} centroid={local_centroid:?}");
            // Keep the volume cache in the SAME shifted body-local frame as the prims. Re-centring just
            // moved every prim by -local_centroid; the cached grid bounds (grid_min/max, body_bounds) and
            // the cached DC mesh must move with them. Without this, the next bake / the VP shader map the
            // grid against STALE old-frame bounds → the coarse "fin/comb" corruption on prim add/duplicate
            // that previously only a full invalidate_all (the resolution slider) cleared. translate_body_cache
            // shifts grid_min/max + body_bounds + cached mesh vertices by the same delta the prims moved.
            if local_centroid != Vec3::ZERO {
                self.cache.body_vol_cache.translate_body_cache(body_nid, -local_centroid);
                // translate_body_cache shifts only the next-bake working copy
                // (body_chunked_meshes). The raster pass draws from mesh_cache, and the
                // body's instance transform already uses the NEW origin THIS frame — so the
                // displayed mesh must shift by the same delta now, or existing geometry
                // jumps by +local_centroid until an async bake re-lands it (the "everything
                // shifts a bit until you move it" bug). Relocate the display mesh in place
                // (pure offset, no DC extraction — safe in volume-only mode).
                let mesh_cid = self.scene.scene_graph.get(body_nid).and_then(|n| {
                    if let core_math::scene_graph::NodePayload::SdfBody { mesh_cache_id, .. } = &n.payload {
                        *mesh_cache_id
                    } else { None }
                });
                if let Some(cid) = mesh_cid {
                    self.cache.mesh_cache.translate_mesh_in_place(cid, -local_centroid);
                }
            }
        } else {
            // Legacy path: prim.position is world-space, origin is computed directly.
            let new_origin = {
                let prims = match self.scene.scene_graph.body_primitives(body_nid) {
                    Some(p) if !p.is_empty() => p,
                    _ => return,
                };
                match self.scene.body_pivot_mode {
                    BodyPivotMode::Centroid => {
                        let sum: Vec3 = prims.iter().map(|p| p.position).sum();
                        sum / prims.len() as f32
                    }
                    BodyPivotMode::WorldOrigin => Vec3::ZERO,
                    BodyPivotMode::BoundingBox => {
                        let mut min = Vec3::splat(f32::MAX);
                        let mut max = Vec3::splat(f32::MIN);
                        for p in prims {
                            let s = p.scale.x.max(p.scale.y).max(p.scale.z);
                            min = min.min(p.position - Vec3::splat(s));
                            max = max.max(p.position + Vec3::splat(s));
                        }
                        (min + max) * 0.5
                    }
                    BodyPivotMode::Custom => unreachable!(),
                }
            };
            let first_mat = self.scene.scene_graph.body_primitives(body_nid)
                .and_then(|p| p.first())
                .map(|p| p.material.clone());
            if let Some(node) = self.scene.scene_graph.get_mut(body_nid) {
                if let core_math::scene_graph::NodePayload::SdfBody { origin, material, .. } = &mut node.payload {
                    *origin = new_origin;
                    if let Some(mat) = first_mat {
                        *material = mat;
                    }
                }
            }
        }
    }

    /// Recalculate origins for ALL SdfBody nodes in the scene graph.
    #[allow(dead_code)] // FUTURE: batch body origin recalc
    pub(crate) fn recalculate_all_body_origins(&mut self) {
        let body_nids: Vec<NodeId> = self.scene.scene_graph.sdf_bodies()
            .map(|(nid, _)| nid)
            .collect();
        for nid in body_nids {
            self.recalculate_body_origin(nid);
        }
    }

    // ---- Prim query helpers ----------------------------------------------------

    /// Find a primitive's flat index by its unique ID (O(1) via cached HashMap).
    pub(crate) fn prim_index_by_uid(&self, uid: u32) -> Option<usize> {
        self.scene.prim_uid_to_index.get(&uid).copied()
    }

    /// Get flat indices of all prims belonging to the given SdfBody node.
    pub(crate) fn body_prim_indices(&self, body_nid: NodeId) -> Vec<usize> {
        self.scene.prim_body_map
            .iter()
            .enumerate()
            .filter(|(_, (nid, _))| *nid == body_nid)
            .map(|(i, _)| i)
            .collect()
    }

    /// Count prims belonging to the given SdfBody node (avoids Vec allocation).
    pub(crate) fn body_prim_count(&self, body_nid: NodeId) -> usize {
        self.scene.prim_body_map.iter().filter(|(nid, _)| *nid == body_nid).count()
    }

    // ---- Clipboard (Ctrl+C / Ctrl+X / Ctrl+V) ---------------------------------

    /// Copy selected prims to the internal clipboard.
    pub(crate) fn copy_selected_prims(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() {
            self.toast("Nothing selected to copy");
            return;
        }
        self.prim_clipboard.clear();
        for &idx in &indices {
            if idx < self.scene.primitives.len() {
                self.prim_clipboard.push(self.scene.primitives[idx].clone());
            }
        }
        self.toast(&format!("Copied {} prim(s)", self.prim_clipboard.len()));
    }

    /// Cut selected prims: copy to clipboard then delete.
    pub(crate) fn cut_selected_prims(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() {
            self.toast("Nothing selected to cut");
            return;
        }
        // Copy first (before snapshot so clipboard is populated regardless)
        self.prim_clipboard.clear();
        for &idx in &indices {
            if idx < self.scene.primitives.len() {
                self.prim_clipboard.push(self.scene.primitives[idx].clone());
            }
        }
        let count = self.prim_clipboard.len();
        // Then delete
        self.transaction("Cut prims", |s| {
            s.remove_prims_from_graph(&indices);
            s.mark_dirty(DirtyFlags::SCENE);
            s.toast(&format!("Cut {} prim(s)", count));
        });
    }

    /// Paste prims from the internal clipboard into the active body.
    pub(crate) fn paste_clipboard_prims(&mut self) {
        if self.prim_clipboard.is_empty() {
            self.toast("Clipboard is empty");
            return;
        }
        let clipboard = self.prim_clipboard.clone();
        let clip_len = clipboard.len();
        self.transaction("Paste prims", |s| {
            let mut last_uid = 0u32;
            for prim in clipboard {
                let mut p = prim;
                p.prim_uid = s.scene.next_prim_id;
                s.scene.next_prim_id += 1;
                // M71: Clear stale references from source context
                p.object_id = None;
                p.instance_master_id = None;
                // Offset slightly so pasted prims don't overlap originals exactly
                p.position += Vec3::new(0.5, 0.0, 0.5);
                last_uid = p.prim_uid;
                s.add_prim_to_graph(p);
            }
            // Select the last pasted prim
            if last_uid > 0 {
                s.select_single_unified(SceneItemId::Prim(last_uid));
            }
            s.mark_dirty(DirtyFlags::SCENE);
            s.toast(&format!("Pasted {} prim(s)", clip_len));
        });
    }
}
