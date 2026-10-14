//! TransformTarget resolver — builds transform targets from unified selection
//! WITHOUT mutating `usel.selected`. Replaces the SdfBody selection-mutation
//! blocks in start_modal_transform() and start_gizmo_drag().

use std::collections::HashSet;
use crate::{
    AzraApp, TransformSnapshot, NodeGizmoTarget, LightTransformSnapshot,
};

/// Resolved transform targets from the current unified selection.
/// Built by `resolve_transform_targets()` — pure read, no side effects.
#[allow(dead_code)] // node_targets/light_targets used by future refactoring steps
pub(crate) struct TransformTargetSet {
    pub(crate) prim_targets: Vec<TransformSnapshot>,
    pub(crate) node_targets: Vec<NodeGizmoTarget>,
    pub(crate) light_targets: Vec<LightTransformSnapshot>,
}

impl AzraApp {
    /// Build transform targets from the current selection without modifying `usel`.
    ///
    /// - Lights → LightTransformSnapshot
    /// - Non-SdfBody nodes → NodeGizmoTarget
    /// - SdfBody nodes → expand recursively to prim TransformSnapshots
    /// - Standalone prims → included only if NOT dominated by a selected SdfBody
    pub(crate) fn resolve_transform_targets(&self) -> TransformTargetSet {
        let mut prim_targets: Vec<TransformSnapshot> = Vec::new();
        let mut node_targets: Vec<NodeGizmoTarget> = Vec::new();
        let mut light_targets: Vec<LightTransformSnapshot> = Vec::new();

        // Track which prim_uids are already claimed by an SdfBody expansion
        // to prevent double-transform when both a body and its prims are selected.
        let mut body_claimed_uids: HashSet<u32> = HashSet::new();

        // --- Lights ---
        // BOTH selection forms (`selected_light_ids`): a viewport pick arrives as
        // `SceneItemId::Light`, an outliner row as `SceneItemId::Node`. The node loop below
        // excludes lights, so each light produces exactly ONE target either way.
        for lid in self.selected_light_ids() {
            if let Some(data) = self.scene.light_by_id(lid) {
                if data.locked { continue; }
                light_targets.push(LightTransformSnapshot {
                    light_id: lid,
                    original_pos: data.position,
                    original_azimuth: data.azimuth,
                    original_elevation: data.elevation,
                    original_range: data.range,
                });
            }
        }

        // --- Nodes (lights excluded — already resolved above as light targets) ---
        for nid in self.selected_transform_nodes() {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => continue,
            };
            if node.locked { continue; }

            let is_sdf_body = matches!(
                node.payload,
                core_math::scene_graph::NodePayload::SdfBody { .. }
            );

            if is_sdf_body {
                // Recursively collect all body nids (this body + nested child bodies)
                let mut all_body_ids: HashSet<core_math::scene_graph::NodeId> = HashSet::new();
                all_body_ids.insert(nid);
                {
                    let mut queue = self.scene.scene_graph.child_bodies(nid);
                    let mut i = 0;
                    while i < queue.len() {
                        all_body_ids.insert(queue[i]);
                        let gc = self.scene.scene_graph.child_bodies(queue[i]);
                        queue.extend(gc);
                        i += 1;
                    }
                }
                // Expand to prim targets via prim_body_map
                for (flat_idx, &(body_nid, _)) in self.scene.prim_body_map.iter().enumerate() {
                    if all_body_ids.contains(&body_nid) && flat_idx < self.scene.primitives.len() {
                        if self.scene.primitives[flat_idx].locked { continue; }
                        let uid = self.scene.primitives[flat_idx].prim_uid;
                        prim_targets.push(TransformSnapshot {
                            prim_uid: uid,
                            original_pos: self.scene.primitives[flat_idx].position,
                            original_rot: self.scene.primitives[flat_idx].rotation_deg,
                            original_scale: self.scene.primitives[flat_idx].scale,
                            original_elongation: self.scene.primitives[flat_idx].elongation,
                            original_orient: self.prim_world_orient(flat_idx),
                        });
                        body_claimed_uids.insert(uid);
                    }
                }
            } else {
                // Non-SdfBody node (poly, NURBS, imported, group, empty)
                let r = node.local_rotation * (std::f32::consts::PI / 180.0);
                let o = glam::Quat::from_euler(glam::EulerRot::YXZ, r.y, r.x, r.z);
                node_targets.push(NodeGizmoTarget {
                    node_id: nid,
                    original_pos: node.local_position,
                    original_rot: node.local_rotation,
                    original_scale: node.local_scale,
                    original_orient: o,
                });
            }
        }

        // --- Standalone prims (not dominated by a selected body) ---
        for uid in self.sel.usel.selected_prims() {
            if body_claimed_uids.contains(&uid) { continue; }
            if let Some(flat_idx) = self.prim_index_by_uid(uid) {
                if flat_idx >= self.scene.primitives.len() { continue; }
                if self.scene.primitives[flat_idx].locked { continue; }
                prim_targets.push(TransformSnapshot {
                    prim_uid: uid,
                    original_pos: self.scene.primitives[flat_idx].position,
                    original_rot: self.scene.primitives[flat_idx].rotation_deg,
                    original_scale: self.scene.primitives[flat_idx].scale,
                    original_elongation: self.scene.primitives[flat_idx].elongation,
                    original_orient: self.prim_world_orient(flat_idx),
                });
            }
        }

        TransformTargetSet { prim_targets, node_targets, light_targets }
    }
}
