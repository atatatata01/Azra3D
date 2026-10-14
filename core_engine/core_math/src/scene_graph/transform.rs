//! Body and world transforms — body-local ↔ world conversion.
//!
//! Pure relocation from former `scene_graph.rs`.

use glam::{Mat4, Quat, Vec3};

use crate::primitives::SdfPrimitive;

use super::{NodeId, NodePayload, SceneGraph};

impl SceneGraph {
    pub fn invalidate_world_xform_cache(&self) {
        if let Ok(mut c) = self.world_xform_cache.lock() {
            c.clear();
        }
    }
    // ----------------------------------------------------------
    // Transform accumulation
    // ----------------------------------------------------------

    /// Compute the world transform for a node by accumulating parent transforms.
    ///
    /// Leaf→root fold: `result = local * result` equals root_local * … * leaf_local.
    ///
    /// AZRA_MASTER_ROADMAP Phase A5: memoized in `world_xform_cache`. Ancestors along the
    /// path are stored so sibling gathers reuse parents. Cleared on `get_mut` / hierarchy edits.
    pub fn world_transform(&self, id: NodeId) -> Mat4 {
        if let Ok(cache) = self.world_xform_cache.lock() {
            if let Some(m) = cache.get(&id).copied() {
                return m;
            }
        }
        // Leaf→root ids, then walk root→leaf while filling the cache.
        let mut chain: Vec<NodeId> = Vec::with_capacity(8);
        let mut current = id;
        loop {
            chain.push(current);
            match self.nodes.get(&current).and_then(|node| node.parent) {
                Some(pid) => current = pid,
                None => break,
            }
        }
        let mut acc = Mat4::IDENTITY;
        let Ok(mut cache) = self.world_xform_cache.lock() else {
            // Poisoned lock — fall back to uncached leaf→root fold.
            let mut result = Mat4::IDENTITY;
            let mut cur = id;
            loop {
                if let Some(node) = self.nodes.get(&cur) {
                    result = node.local_transform() * result;
                    if let Some(pid) = node.parent {
                        cur = pid;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            return result;
        };
        for &nid in chain.iter().rev() {
            if let Some(m) = cache.get(&nid).copied() {
                acc = m;
                continue;
            }
            let local = self
                .nodes
                .get(&nid)
                .map(|node| node.local_transform())
                .unwrap_or(Mat4::IDENTITY);
            acc = acc * local;
            cache.insert(nid, acc);
        }
        acc
    }

    /// Convert legacy world-space Custom pivots to node-local so they follow G/R/S.
    /// Idempotent: already-local pivots are left alone.
    pub fn migrate_custom_pivots_to_local(&mut self) {
        let ids: Vec<NodeId> = self.all_ids().collect();
        let mut updates: Vec<(NodeId, Vec3)> = Vec::new();
        for nid in ids {
            let Some(node) = self.get(nid) else { continue };
            if node.pivot.mode != super::PivotMode::Custom || node.pivot.local {
                continue;
            }
            let world = node.pivot.position;
            let local = self.world_transform(nid).inverse().transform_point3(world);
            updates.push((nid, local));
        }
        for (nid, local) in updates {
            if let Some(node) = self.get_mut(nid) {
                node.pivot.position = local;
                node.pivot.local = true;
            }
        }
    }

}

// ============================================================
// BodyTransform — decomposed TRS for body-local ↔ world conversion
// ============================================================

/// Body world transform — decomposed TRS for body-local ↔ world conversion.
#[derive(Clone, Copy, Debug)]
pub struct BodyTransform {
    pub origin: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for BodyTransform {
    fn default() -> Self {
        Self { origin: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE }
    }
}

impl BodyTransform {
    /// Convert body-local position to world-space.
    pub fn local_to_world(&self, local_pos: Vec3) -> Vec3 {
        self.origin + self.rotation * (local_pos * self.scale)
    }

    /// Convert world-space position to body-local.
    pub fn world_to_local(&self, world_pos: Vec3) -> Vec3 {
        self.rotation.inverse() * (world_pos - self.origin) / self.scale
    }

    /// Transform a body-local AABB to a world-space AABB.
    /// Transforms all 8 corners and computes the axis-aligned bounding box.
    /// Handles rotation and non-uniform scale correctly.
    pub fn local_aabb_to_world(&self, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
        let corners = [
            Vec3::new(local_min.x, local_min.y, local_min.z),
            Vec3::new(local_max.x, local_min.y, local_min.z),
            Vec3::new(local_min.x, local_max.y, local_min.z),
            Vec3::new(local_max.x, local_max.y, local_min.z),
            Vec3::new(local_min.x, local_min.y, local_max.z),
            Vec3::new(local_max.x, local_min.y, local_max.z),
            Vec3::new(local_min.x, local_max.y, local_max.z),
            Vec3::new(local_max.x, local_max.y, local_max.z),
        ];
        let first = self.local_to_world(corners[0]);
        let mut world_min = first;
        let mut world_max = first;
        for &c in &corners[1..] {
            let w = self.local_to_world(c);
            world_min = world_min.min(w);
            world_max = world_max.max(w);
        }
        (world_min, world_max)
    }
}

impl SceneGraph {
    /// Get the world transform of an SdfBody node.
    pub fn body_world_transform(&self, body_nid: NodeId) -> Option<BodyTransform> {
        self.get(body_nid).and_then(|n| match &n.payload {
            NodePayload::SdfBody { origin, body_rotation, body_scale, .. } => Some(BodyTransform {
                origin: *origin,
                rotation: *body_rotation,
                scale: *body_scale,
            }),
            _ => None,
        })
    }

    /// The FULL world transform of a body's local frame, COMPOSING the placement of any parent SdfObject
    /// (Object → Body → Primitive). Equals `world_transform(parent) * BodyTransform(body)` as a Mat4.
    /// For a top-level body (parent == Root, whose world transform is identity) this EQUALS the body's own
    /// transform — so behavior is unchanged until a body actually sits under an SdfObject. The render
    /// instance matrix, the picking matrix, and field-eval world→local must ALL route through this so a
    /// body-under-object renders, picks, and evaluates at exactly one world position (render==pick).
    pub fn effective_body_transform(&self, body_nid: NodeId) -> Option<Mat4> {
        let bt = self.body_world_transform(body_nid)?;
        let body_mat = Mat4::from_scale_rotation_translation(bt.scale, bt.rotation, bt.origin);
        let parent_world = match self.get(body_nid).and_then(|n| n.parent) {
            Some(p) => self.world_transform(p),
            None => Mat4::IDENTITY,
        };
        Some(parent_world * body_mat)
    }

    /// World→body-local using the EFFECTIVE (object-aware) transform — the object-composed analogue of
    /// `BodyTransform::world_to_local`. Returns the point unchanged if the body is missing.
    pub fn effective_world_to_local(&self, body_nid: NodeId, world_pos: Vec3) -> Vec3 {
        match self.effective_body_transform(body_nid) {
            Some(m) => m.inverse().transform_point3(world_pos),
            None => world_pos,
        }
    }
}

/// Batch-convert body-local prims to world-space positions (for serialization/picking).
/// Returns a new Vec with positions transformed; other fields unchanged.
pub fn prims_local_to_world(prims: &[SdfPrimitive], transform: &BodyTransform) -> Vec<SdfPrimitive> {
    prims.iter().map(|p| {
        let mut wp = p.clone();
        wp.position = transform.local_to_world(p.position);
        wp
    }).collect()
}
