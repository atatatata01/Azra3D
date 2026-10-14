// =====================================================================
// WARNING: ALL transform math lives in compute_transform() in transform.rs.
// Do NOT add grab/rotate/scale calculations here.
// To support a new target: add a TransformTarget variant and write the
// apply logic below. The math is SHARED.
// If you duplicate the math, future you will hate past you.
// =====================================================================

/// When true, prim.position is body-local; body-level transforms only update
/// body.origin / body_rotation / body_scale — no prim mutations, no rebake.
/// Must stay in sync with the same flag in body_volume_cache.rs, serialize.rs,
/// and project_load.rs.
use crate::BODY_LOCAL_PRIMS;

use crate::FourthLayerApp;
use crate::transform::TransformResult;

impl FourthLayerApp {
    // =====================================================================
    // apply_transform_result — single dispatch point for writing results
    // =====================================================================

    /// Apply a computed TransformResult to an SDF primitive by index.
    /// The result position is in world-space; when BODY_LOCAL_PRIMS is true
    /// we convert it to body-local before writing.
    pub(crate) fn apply_to_sdf_prim(&mut self, idx: usize, result: &TransformResult) {
        // Write elongation FIRST so bounding extents are correct for dirty regions. CRITICAL: mirror it
        // to the GRAPH here too. `write_prim_transform` below only mirrors pos/rot/scale, and the GPU
        // bake serializes from the GRAPH — so without this the elongation change reached only the flat
        // cache and NEVER the baked surface. That was the real "E does nothing" bug: the prim's
        // elongation field changed but the rendered volume didn't, so an Elongate edit looked like the
        // shape just translated (the position DID mirror) and never actually grew.
        if idx >= self.scene.primitives.len() {
            return;
        }
        let elong_changed = result.elongation != self.scene.primitives[idx].elongation;
        let pos_before = self.scene.primitives[idx].position;
        if elong_changed {
            self.scene.primitives[idx].elongation = result.elongation;
            if let Some(&(nid, body_idx)) = self.scene.prim_body_map.get(idx) {
                if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                    if body_idx < prims.len() { prims[body_idx].elongation = result.elongation; }
                }
            }
        }

        if BODY_LOCAL_PRIMS {
            let body_nid = self.scene.prim_body_map.get(idx).map(|&(n, _)| n);
            let local_pos = if let Some(nid) = body_nid {
                let transform = self.scene.scene_graph.body_world_transform(nid)
                    .unwrap_or_default();
                transform.world_to_local(result.position)
            } else {
                result.position
            };
            // write_prim_transform marks dirty AABB (now includes updated elongation bounds)
            self.write_prim_transform(idx, local_pos, result.rotation, result.scale);
            if let Some(_nid) = body_nid {
                self.sync_flat_cache();
            }
        } else {
            self.write_prim_transform(idx, result.position, result.rotation, result.scale);
        }

        // Linked instances: content + one-sided recenter delta (not free grab translation).
        // Pure Grab/Rotate/Scale leaves elongation unchanged → no instance push.
        if elong_changed && self.is_master(idx) {
            let uid = self.scene.primitives[idx].prim_uid;
            let pos_after = self.scene.primitives[idx].position;
            self.sync_instances_of_ex(uid, pos_after - pos_before);
        }
    }

    /// Apply a computed TransformResult to a scene graph node.
    pub(crate) fn apply_to_scene_node(&mut self, nid: core_math::scene_graph::NodeId, result: &TransformResult) {
        // Check if this is an SdfBody — needs special handling
        let is_sdf_body = self.scene.scene_graph.get(nid)
            .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
            .unwrap_or(false);

        if is_sdf_body {
            let current_origin = self.scene.scene_graph.get(nid).and_then(|n| {
                if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &n.payload {
                    Some(*origin)
                } else { None }
            }).unwrap_or(glam::Vec3::ZERO);

            let pos_delta = result.position - current_origin;

            // Update origin + body_rotation + body_scale
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let core_math::scene_graph::NodePayload::SdfBody { origin, body_rotation, body_scale, .. } = &mut node.payload {
                    *origin = result.position;
                    // Convert euler degrees (Vec3) → Quat (YXZ convention, matching SceneNode::local_transform)
                    *body_rotation = glam::Quat::from_euler(
                        glam::EulerRot::YXZ,
                        result.rotation.y.to_radians(),
                        result.rotation.x.to_radians(),
                        result.rotation.z.to_radians(),
                    );
                    *body_scale = result.scale;
                }
            }

            if BODY_LOCAL_PRIMS {
                // Body-local prim mode: prim positions are relative to body.origin.
                // Moving/rotating/scaling the body only changes the instance matrix —
                // no prim mutations and no rebake needed.
                // Child body origins must still be offset for TRANSLATE.

                // Collect child body ids (needed for translate child-offset and mark_transform_safe)
                let mut child_body_ids = vec![];
                {
                    let mut queue = self.scene.scene_graph.child_bodies(nid);
                    let mut i = 0;
                    while i < queue.len() {
                        child_body_ids.push(queue[i]);
                        let gc = self.scene.scene_graph.child_bodies(queue[i]);
                        queue.extend(gc);
                        i += 1;
                    }
                }

                // TRANSLATE: shift child body origins so they follow the parent
                if pos_delta.length() > 1e-6 {
                    for &child_nid in &child_body_ids {
                        if let Some(node) = self.scene.scene_graph.get_mut(child_nid) {
                            if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &mut node.payload {
                                *origin += pos_delta;
                            }
                        }
                    }
                }

                // Mark all bodies transform-safe (mesh is valid, only instance matrix changed)
                let mut all_body_ids = child_body_ids;
                all_body_ids.insert(0, nid);
                if self.cache.use_body_vol_cache && !self.cache.suppress_body_vol_dirty {
                    for &body_nid in &all_body_ids {
                        self.cache.body_vol_cache.mark_transform_safe(body_nid);
                    }
                }
                // Force GPU re-serialization with new body transform (applies local_to_world)
                self.cache.prim_buf_stale = true;
            } else {
                // Legacy world-space prim mode: translate all prims by the same delta.
                if pos_delta.length() > 1e-6 {
                    let mut all_body_ids = vec![nid];
                    {
                        let mut queue = self.scene.scene_graph.child_bodies(nid);
                        let mut i = 0;
                        while i < queue.len() {
                            all_body_ids.push(queue[i]);
                            let gc = self.scene.scene_graph.child_bodies(queue[i]);
                            queue.extend(gc);
                            i += 1;
                        }
                    }

                    for &body_nid in &all_body_ids {
                        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_nid) {
                            for prim in prims.iter_mut() {
                                prim.position += pos_delta;
                            }
                        }
                        if body_nid != nid {
                            if let Some(node) = self.scene.scene_graph.get_mut(body_nid) {
                                if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &mut node.payload {
                                    *origin += pos_delta;
                                }
                            }
                        }
                    }
                    self.sync_flat_cache();
                    // TransformSafe: DC mesh is baked in body-local space, so
                    // rigid body moves only need an updated instance matrix
                    // (read from body.origin each frame). No rebake required.
                    // mark_transform_safe is advisory — prevents the body from
                    // appearing as NeverBaked while the mesh is actually valid.
                    if self.cache.use_body_vol_cache && !self.cache.suppress_body_vol_dirty {
                        for &body_nid in &all_body_ids {
                            self.cache.body_vol_cache.mark_transform_safe(body_nid);
                        }
                    }
                }
            }
        } else {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.local_position = result.position;
                node.local_rotation = result.rotation;
                node.local_scale = result.scale;
            }
        }
        // Re-upload prim buffer (re-runs serialization which applies body_local_to_world).
        // In BODY_LOCAL_PRIMS mode this is already set above; here we set it for the
        // non-SdfBody branch and ensure the flag is always true on exit.
        self.cache.prim_buf_stale = true;
        // Transform write → structural draw list + instance buffer must refresh this frame
        // (RENDER alone leaves the GPU drawing the old matrix until the next rebuild).
        self.mark_transform_dirty();
    }

    // ADD NEW TARGETS HERE — not new math
}
