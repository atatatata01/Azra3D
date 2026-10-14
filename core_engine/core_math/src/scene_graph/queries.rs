//! Scene graph queries — tree ops, iterators, body helpers, child order.
//!
//! Pure relocation from former `scene_graph.rs`.

use glam::Vec3;

use crate::primitives::{BooleanOp, SdfPrimitive};

use super::{
    BodyChildRef, LightData, MeshCacheId, NodeId, NodePayload, SceneGraph, SceneNode,
};

impl SceneGraph {
    // ----------------------------------------------------------
    // Node access
    // ----------------------------------------------------------

    pub fn get(&self, id: NodeId) -> Option<&SceneNode> {
        self.nodes.get(&id)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        // May mutate TRS or hierarchy-adjacent fields — drop world cache (A5).
        self.invalidate_world_xform_cache();
        self.nodes.get_mut(&id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// All node IDs in the graph (unordered).
    pub fn all_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys().copied()
    }

    // ----------------------------------------------------------
    // Tree operations
    // ----------------------------------------------------------

    /// Add a child node under `parent`. Returns the new node's ID.
    pub fn add_child(&mut self, parent: NodeId, payload: NodePayload) -> NodeId {
        self.invalidate_world_xform_cache();
        debug_assert!(
            self.nodes.contains_key(&parent),
            "add_child: parent {:?} does not exist in the scene graph",
            parent
        );
        let id = self.alloc_id();
        let mut node = SceneNode::new(id, payload);
        // Graceful fallback: if parent doesn't exist, attach to root so the
        // node is never orphaned with a dangling parent reference.
        let actual_parent = if self.nodes.contains_key(&parent) {
            parent
        } else {
            self.root
        };
        node.parent = Some(actual_parent);
        if let Some(p) = self.nodes.get_mut(&actual_parent) {
            p.children.push(id);
        }
        self.nodes.insert(id, node);
        id
    }

    /// Add a child node with a specific transform.
    pub fn add_child_with_transform(
        &mut self,
        parent: NodeId,
        payload: NodePayload,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
    ) -> NodeId {
        let id = self.add_child(parent, payload);
        if let Some(node) = self.nodes.get_mut(&id) {
            node.local_position = position;
            node.local_rotation = rotation;
            node.local_scale = scale;
        }
        id
    }

    /// Remove a node and all its descendants. Returns the removed node (if it existed).
    pub fn remove(&mut self, id: NodeId) -> Option<SceneNode> {
        self.invalidate_world_xform_cache();
        if id == self.root {
            return None; // cannot remove root
        }
        // Collect descendants (BFS)
        let mut to_remove = vec![id];
        let mut i = 0;
        while i < to_remove.len() {
            if let Some(node) = self.nodes.get(&to_remove[i]) {
                to_remove.extend_from_slice(&node.children);
            }
            i += 1;
        }
        // Unlink from parent
        if let Some(node) = self.nodes.get(&id) {
            if let Some(pid) = node.parent {
                if let Some(parent) = self.nodes.get_mut(&pid) {
                    parent.children.retain(|c| *c != id);
                }
            }
        }
        // Remove all
        let removed = self.nodes.remove(&id);
        for desc in to_remove.iter().skip(1) {
            self.nodes.remove(desc);
        }
        removed
    }

    /// Move a node to a new parent.
    pub fn reparent(&mut self, node_id: NodeId, new_parent: NodeId) {
        self.invalidate_world_xform_cache();
        if node_id == self.root || node_id == new_parent {
            return;
        }
        // Guard: node_id must exist — don't create dangling child references
        if !self.nodes.contains_key(&node_id) {
            return;
        }
        // Same-parent guard — avoid silent reorder
        if let Some(node) = self.nodes.get(&node_id) {
            if node.parent == Some(new_parent) {
                return;
            }
        }
        // Check for cycles (new_parent must not be a descendant of node_id)
        if self.is_descendant_of(new_parent, node_id) {
            return;
        }
        // Unlink from old parent
        if let Some(node) = self.nodes.get(&node_id) {
            if let Some(old_parent) = node.parent {
                if let Some(p) = self.nodes.get_mut(&old_parent) {
                    p.children.retain(|c| *c != node_id);
                }
            }
        }
        // Link to new parent
        if let Some(p) = self.nodes.get_mut(&new_parent) {
            p.children.push(node_id);
        }
        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.parent = Some(new_parent);
        }
    }

    /// Check if `candidate` is a descendant of `ancestor`.
    pub fn is_descendant_of(&self, candidate: NodeId, ancestor: NodeId) -> bool {
        let mut current = candidate;
        while let Some(node) = self.nodes.get(&current) {
            if let Some(pid) = node.parent {
                if pid == ancestor {
                    return true;
                }
                current = pid;
            } else {
                break;
            }
        }
        false
    }
    // ----------------------------------------------------------
    // Iterators for specific node types
    // ----------------------------------------------------------

    /// Iterate over all nodes.
    pub fn all_nodes(&self) -> impl Iterator<Item = (NodeId, &SceneNode)> {
        self.nodes.iter().map(|(&id, n)| (id, n))
    }

    pub fn referenced_mesh_cache_ids(&self) -> impl Iterator<Item = MeshCacheId> + '_ {
        self.all_nodes().filter_map(|(_, node)| match &node.payload {
            NodePayload::ImportedMesh { mesh_cache_id, .. } => Some(*mesh_cache_id),
            NodePayload::SdfBody { mesh_cache_id, .. }
            | NodePayload::PolygonalBody { mesh_cache_id, .. }
            | NodePayload::VoxelBody { mesh_cache_id, .. }
            | NodePayload::NurbsBody { mesh_cache_id, .. }
            | NodePayload::NurbsSolid { mesh_cache_id, .. }
            | NodePayload::SdfObject { mesh_cache_id, .. } => *mesh_cache_id,
            _ => None,
        })
    }

    /// Iterate over all SdfBody nodes.
    pub fn sdf_bodies(&self) -> impl Iterator<Item = (NodeId, &SceneNode)> {
        self.nodes
            .iter()
            .filter(|(_, n)| matches!(n.payload, NodePayload::SdfBody { .. }))
            .map(|(&id, n)| (id, n))
    }

    /// Iterate over all SdfBody nodes (mutable).
    ///
    /// Hands out `&mut SceneNode` (owns TRS). Must invalidate world-xform cache — same
    /// contract as `get_mut`. Zero callers today, but an uncached path here is a silent
    /// wrong-place render (A5 loaded gun).
    pub fn sdf_bodies_mut(&mut self) -> impl Iterator<Item = (NodeId, &mut SceneNode)> {
        self.invalidate_world_xform_cache();
        self.nodes
            .iter_mut()
            .filter(|(_, n)| matches!(n.payload, NodePayload::SdfBody { .. }))
            .map(|(id, n)| (*id, n))
    }

    /// Iterate over all SdfObject (container) nodes.
    pub fn sdf_objects(&self) -> impl Iterator<Item = (NodeId, &SceneNode)> {
        self.nodes
            .iter()
            .filter(|(_, n)| matches!(n.payload, NodePayload::SdfObject { .. }))
            .map(|(&id, n)| (id, n))
    }

    /// Iterate over all SdfObject (container) nodes (mutable).
    ///
    /// Same A5 contract as `sdf_bodies_mut` / `get_mut` — TRS lives on `SceneNode`.
    pub fn sdf_objects_mut(&mut self) -> impl Iterator<Item = (NodeId, &mut SceneNode)> {
        self.invalidate_world_xform_cache();
        self.nodes
            .iter_mut()
            .filter(|(_, n)| matches!(n.payload, NodePayload::SdfObject { .. }))
            .map(|(id, n)| (*id, n))
    }

    /// Flat collection of ALL SDF primitives across all bodies.
    /// Returns Vec of (body_node_id, prim_index_in_body, &SdfPrimitive).
    ///
    /// NOTE: Uses HashMap iteration order — non-deterministic. For stable ordering
    /// (matching the flat `primitives` cache), use `all_primitives_ordered()`.
    pub fn all_primitives(&self) -> Vec<(NodeId, usize, &SdfPrimitive)> {
        let mut result = Vec::new();
        for (&nid, node) in &self.nodes {
            if let NodePayload::SdfBody { primitives, .. } = &node.payload {
                for (i, p) in primitives.iter().enumerate() {
                    result.push((nid, i, p));
                }
            }
        }
        result
    }

    /// Flat collection of ALL SDF primitives in depth-first tree order.
    /// This is the canonical ordering used by the flat `primitives` cache.
    pub fn all_primitives_ordered(&self) -> Vec<(NodeId, usize, &SdfPrimitive)> {
        let mut result = Vec::new();
        for (_depth, nid, node) in self.walk_depth_first(self.root) {
            if let NodePayload::SdfBody { primitives, .. } = &node.payload {
                for (i, p) in primitives.iter().enumerate() {
                    result.push((nid, i, p));
                }
            }
        }
        result
    }

    /// Total number of SDF primitives across all bodies.
    pub fn primitive_count(&self) -> usize {
        self.nodes
            .values()
            .filter_map(|n| {
                if let NodePayload::SdfBody { primitives, .. } = &n.payload {
                    Some(primitives.len())
                } else {
                    None
                }
            })
            .sum()
    }

    /// Iterate over all Light nodes.
    pub fn lights(&self) -> impl Iterator<Item = (NodeId, &LightData)> {
        self.nodes.iter().filter_map(|(&id, n)| {
            if let NodePayload::Light(data) = &n.payload {
                Some((id, data))
            } else {
                None
            }
        })
    }

    /// Iterate over all Light nodes (mutable).
    pub fn lights_mut(&mut self) -> impl Iterator<Item = (NodeId, &mut LightData)> {
        self.nodes.iter_mut().filter_map(|(id, n)| {
            if let NodePayload::Light(data) = &mut n.payload {
                Some((*id, data))
            } else {
                None
            }
        })
    }

    /// Find a Light node by its stable `id` field. Returns (NodeId, &LightData).
    pub fn light_by_id(&self, id: u32) -> Option<(NodeId, &LightData)> {
        self.nodes.iter().find_map(|(&nid, n)| {
            if let NodePayload::Light(data) = &n.payload {
                if data.id == id { return Some((nid, data)); }
            }
            None
        })
    }

    /// Find a Light node by its stable `id` field (mutable).
    pub fn light_by_id_mut(&mut self, id: u32) -> Option<(NodeId, &mut LightData)> {
        self.nodes.iter_mut().find_map(|(nid, n)| {
            if let NodePayload::Light(data) = &mut n.payload {
                if data.id == id { return Some((*nid, data)); }
            }
            None
        })
    }

    /// Count of Light nodes.
    pub fn light_count(&self) -> usize {
        self.nodes.values().filter(|n| matches!(&n.payload, NodePayload::Light(_))).count()
    }
    pub fn renderable_meshes(&self) -> impl Iterator<Item = (NodeId, MeshCacheId)> + '_ {
        self.nodes.iter().filter_map(|(&id, n)| match &n.payload {
            NodePayload::SdfBody {
                mesh_cache_id: Some(mid),
                ..
            } => Some((id, *mid)),
            NodePayload::ImportedMesh { mesh_cache_id, .. } => Some((id, *mesh_cache_id)),
            NodePayload::VoxelBody { mesh_cache_id: Some(mid), .. } => Some((id, *mid)),
            NodePayload::PolygonalBody {
                mesh_cache_id: Some(mid),
                ..
            } => Some((id, *mid)),
            NodePayload::NurbsBody {
                mesh_cache_id: Some(mid),
                ..
            } => Some((id, *mid)),
            NodePayload::NurbsSolid {
                mesh_cache_id: Some(mid),
                ..
            } => Some((id, *mid)),
            _ => None,
        })
    }

    /// Iterate over all PolygonalBody nodes.
    pub fn poly_bodies(&self) -> impl Iterator<Item = (NodeId, &SceneNode)> {
        self.nodes
            .iter()
            .filter(|(_, n)| matches!(n.payload, NodePayload::PolygonalBody { .. }))
            .map(|(&id, n)| (id, n))
    }

    /// Iterate over all NURBS nodes (`NurbsBody` single-patch + `NurbsSolid` multi-face).
    pub fn nurbs_bodies(&self) -> impl Iterator<Item = (NodeId, &SceneNode)> {
        self.nodes
            .iter()
            .filter(|(_, n)| {
                matches!(
                    n.payload,
                    NodePayload::NurbsBody { .. } | NodePayload::NurbsSolid { .. }
                )
            })
            .map(|(&id, n)| (id, n))
    }

    /// Find the SdfBody node containing a primitive with the given uid.
    pub fn find_body_containing_prim(&self, prim_uid: u32) -> Option<NodeId> {
        for (&nid, node) in &self.nodes {
            if let NodePayload::SdfBody { primitives, .. } = &node.payload {
                if primitives.iter().any(|p| p.prim_uid == prim_uid) {
                    return Some(nid);
                }
            }
        }
        None
    }

    /// Get a reference to the primitives of a specific SdfBody.
    pub fn body_primitives(&self, body_id: NodeId) -> Option<&[SdfPrimitive]> {
        self.nodes.get(&body_id).and_then(|n| {
            if let NodePayload::SdfBody { primitives, .. } = &n.payload {
                Some(primitives.as_slice())
            } else {
                None
            }
        })
    }

    /// Resolve the albedo texture map of a body's FIRST visible primitive — the same material the
    /// SDF volume-preview shader reads (`read_body_material` scans the first non-scope prim). Used to
    /// feed the active body's albedo map into `GpuUniforms` for the VP path. Returns the default
    pub fn body_primitives_mut(&mut self, body_id: NodeId) -> Option<&mut Vec<SdfPrimitive>> {
        self.nodes.get_mut(&body_id).and_then(|n| {
            if let NodePayload::SdfBody { primitives, .. } = &mut n.payload {
                Some(primitives)
            } else {
                None
            }
        })
    }

    /// Get child SdfBody NodeIds of a given node, descending THROUGH any SdfObject container layer.
    /// So `child_bodies(root)` still returns the top-level bodies after they are wrapped in Objects
    /// (Object → Body), and `child_bodies(object)` returns that object's bodies. Stops AT a body (does
    /// not recurse into the body's own nested bodies — direct-bodies semantics, just object-transparent).
    pub fn child_bodies(&self, parent: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for &cid in self.children(parent).iter() {
            match self.get(cid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { .. }) => out.push(cid),
                Some(NodePayload::SdfObject { .. }) => out.extend(self.child_bodies(cid)),
                _ => {}
            }
        }
        out
    }

    /// True if `parent` has at least one nested child SdfBody (descending through SdfObject wrappers),
    /// WITHOUT allocating — the fast form of `!child_bodies(parent).is_empty()`.
    pub fn has_child_bodies(&self, parent: NodeId) -> bool {
        self.children(parent).iter().any(|&cid| match self.get(cid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { .. }) => true,
            Some(NodePayload::SdfObject { .. }) => self.has_child_bodies(cid),
            _ => false,
        })
    }

    /// CANONICAL renderability predicate. True if this node is an SdfBody that contributes geometry to
    /// Object Mode — either it owns primitives OR it has nested child sub-bodies (whose geometry is
    /// subsumed into this body's recursive subtree bake). A top-level body with 0 own prims but child
    /// groups IS renderable: its resident slot / subtree mesh holds the geometry.
    ///
    /// Draw, pick, mesh-eligibility and LOD MUST decide via THIS predicate — NEVER `primitives.is_empty()`
    /// alone, which skips a body whose prims were all grouped into sub-bodies. That scattered assumption
    /// is exactly what caused "group everything into a sub-body → surface vanishes / un-clickable": one
    /// call site (`dispatch_grid_dc_async`) was fixed while the raster draw/pick/mesh/LOD sites rotted.
    /// Route every renderability check through here so the invariant can never diverge again.
    pub fn body_has_renderable_content(&self, nid: NodeId) -> bool {
        match self.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { primitives, .. }) =>
                !primitives.is_empty() || self.has_child_bodies(nid),
            _ => false,
        }
    }

    /// Direct SdfObject children of a node (the Object layer under Root, normally).
    pub fn child_objects(&self, parent: NodeId) -> Vec<NodeId> {
        self.children(parent).iter()
            .filter(|id| matches!(self.get(**id).map(|n| &n.payload), Some(NodePayload::SdfObject { .. })))
            .copied()
            .collect()
    }

    /// The nearest ancestor SdfObject of a node (the Object that contains a given body), if any.
    pub fn parent_object(&self, node_id: NodeId) -> Option<NodeId> {
        let mut current = node_id;
        while let Some(node) = self.get(current) {
            match node.parent {
                Some(pid) => {
                    if matches!(self.get(pid).map(|n| &n.payload), Some(NodePayload::SdfObject { .. })) {
                        return Some(pid);
                    }
                    current = pid;
                }
                None => break,
            }
        }
        None
    }

    /// Alias for `parent_object` read from a body's perspective — the SdfObject that owns `body_nid`.
    pub fn find_object_containing_body(&self, body_nid: NodeId) -> Option<NodeId> {
        self.parent_object(body_nid)
    }

    /// Count total primitives in a body subtree (including child bodies, recursively).
    pub fn body_prim_count(&self, body_id: NodeId) -> usize {
        let node = match self.get(body_id) { Some(n) => n, None => return 0 };
        let own = match &node.payload {
            NodePayload::SdfBody { primitives, .. } => primitives.len(),
            _ => return 0,
        };
        let children: usize = self.child_bodies(body_id).iter()
            .map(|&cid| self.body_prim_count(cid))
            .sum();
        own + children
    }

    /// Find the nearest ancestor SdfBody of a node (not including self).
    pub fn parent_body(&self, node_id: NodeId) -> Option<NodeId> {
        let mut current = node_id;
        while let Some(node) = self.get(current) {
            if let Some(pid) = node.parent {
                if matches!(self.get(pid).map(|n| &n.payload), Some(NodePayload::SdfBody { .. })) {
                    return Some(pid);
                }
                current = pid;
            } else {
                break;
            }
        }
        None
    }

    /// Climb to the TOP-LEVEL SdfBody ancestor of `node_id` (the body with no SdfBody parent).
    /// Returns `node_id` itself if it is already top-level. This is the "active analytical unit"
    /// (CLAUDE.md): the whole top-level subtree bakes as ONE grid at ONE resolution, so any
    /// per-body property that governs the bake (resolution) must be keyed on this ancestor — a
    /// sub-body must never carry its own bake resolution.
    pub fn top_level_body(&self, node_id: NodeId) -> NodeId {
        let mut current = node_id;
        while let Some(parent) = self.parent_body(current) {
            current = parent;
        }
        current
    }

    /// True if this SdfBody is nested inside another SdfBody (a sub-body / group), i.e. NOT top-level.
    pub fn is_sub_body(&self, node_id: NodeId) -> bool {
        self.parent_body(node_id).is_some()
    }

    /// Get the world-space origin (pivot) of an SdfBody node.
    pub fn body_origin(&self, body_id: NodeId) -> Option<Vec3> {
        self.get(body_id).and_then(|n| {
            if let NodePayload::SdfBody { origin, .. } = &n.payload {
                Some(*origin)
            } else {
                None
            }
        })
    }

    /// Get the body_op of an SdfBody node.
    pub fn body_op(&self, body_id: NodeId) -> Option<&BooleanOp> {
        self.get(body_id).and_then(|n| {
            if let NodePayload::SdfBody { body_op, .. } = &n.payload {
                Some(body_op)
            } else {
                None
            }
        })
    }

    /// Set the body_op of an SdfBody node.
    pub fn set_body_op(&mut self, body_id: NodeId, op: BooleanOp) {
        if let Some(n) = self.get_mut(body_id) {
            if let NodePayload::SdfBody { body_op, .. } = &mut n.payload {
                *body_op = op;
            }
        }
    }

    // ----------------------------------------------------------
    // Children ordering
    // ----------------------------------------------------------

    /// Get the ordered children of a node.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes
            .get(&id)
            .map(|n| n.children.as_slice())
            .unwrap_or(&[])
    }

    /// Get the index of a child within its parent's children list.
    pub fn child_index(&self, parent_nid: NodeId, child_nid: NodeId) -> Option<usize> {
        self.children(parent_nid).iter().position(|&nid| nid == child_nid)
    }

    /// Move `child_nid` to `target_index` among `parent_nid`'s children.
    pub fn reorder_child(&mut self, parent_nid: NodeId, child_nid: NodeId, target_index: usize) {
        if let Some(parent) = self.nodes.get_mut(&parent_nid) {
            if let Some(pos) = parent.children.iter().position(|&nid| nid == child_nid) {
                parent.children.remove(pos);
                let insert_at = target_index.min(parent.children.len());
                parent.children.insert(insert_at, child_nid);
            }
        }
    }

    // ----------------------------------------------------------
    // Unified child ordering (prims + sub-bodies in ONE sequence)
    // ----------------------------------------------------------

    /// The set of `Body(nid)` refs a body's `child_order` should contain — its DIRECT
    /// child nodes that participate in display/eval (SdfBody, SdfObject, Group). Referenced
    /// by their DIRECT-child NodeId (NOT descended through objects) so a `child_order` slot
    /// maps 1:1 to one `SceneNode.children` edge at this level. The bake serializer's
    /// `child_bodies()` descent through SdfObject is a render-time concern; the ordering
    /// index stays at the direct-edge granularity the outliner draws and the user reorders.
    /// Shared with `validate::validate_child_order` (same former private helper).
    pub(super) fn direct_order_child_nodes(&self, body_id: NodeId) -> Vec<NodeId> {
        self.children(body_id)
            .iter()
            .copied()
            .filter(|&cid| matches!(
                self.get(cid).map(|n| &n.payload),
                Some(NodePayload::SdfBody { .. })
                    | Some(NodePayload::SdfObject { .. })
                    | Some(NodePayload::Group { .. })
            ))
            .collect()
    }

    /// Read the unified child ordering of a body (clone — callers iterate then drop the borrow).
    /// Returns the canonicalised order: it lazily reconciles against the live prim/child sets so
    /// callers never observe a stale/dangling order even if a mutation forgot to maintain it.
    /// Empty (non-SdfBody or genuinely empty body) → empty Vec.
    pub fn body_child_order(&self, body_id: NodeId) -> Vec<BodyChildRef> {
        let stored = match self.get(body_id).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { child_order, .. }) => child_order,
            _ => return Vec::new(),
        };
        self.reconcile_child_order(body_id, stored)
    }

    /// Produce a canonical `child_order` for `body_id` from a (possibly empty/stale) starting
    /// list: keep every still-valid ref in its stored position, drop dangling refs, then APPEND
    /// any prim/child missing from the list. The append order is prims-first-then-bodies — which
    /// matches the legacy bake order, so a freshly-synthesised order (empty input) reproduces the
    /// pre-v8 surface exactly. This is the single source of truth for "what child_order should be".
    ///
    /// DUPLICATE-UID SAFE: `prim_uid` is NOT guaranteed unique within a body — `SdfPrimitive::new`
    /// defaults it to 0, so a freshly-built body can hold several uid-0 prims. The result is therefore
    /// MULTIPLICITY-correct: it contains exactly as many `Prim(uid)` slots as there are prims with that
    /// uid (never collapsing them), so the bake/draw walk emits EVERY prim. Reorder of duplicate-uid
    /// prims is inherently ambiguous (the refs are identical), which is acceptable — uniqueness matters
    /// only for user-driven reordering, and prims the user reorders carry assigned unique uids.
    fn reconcile_child_order(&self, body_id: NodeId, stored: &[BodyChildRef]) -> Vec<BodyChildRef> {
        let (prim_uids, child_nids): (Vec<u32>, Vec<NodeId>) = match self.get(body_id).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { primitives, .. }) => (
                primitives.iter().map(|p| p.prim_uid).collect(),
                self.direct_order_child_nodes(body_id),
            ),
            _ => return Vec::new(),
        };
        // Remaining multiplicity budget per kind: how many slots we still owe.
        // Prims keyed by uid (count of prims with that uid); bodies are unique by NodeId.
        let mut prim_budget: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for &uid in &prim_uids { *prim_budget.entry(uid).or_insert(0) += 1; }
        let mut body_remaining: std::collections::HashSet<NodeId> = child_nids.iter().copied().collect();

        let mut out: Vec<BodyChildRef> = Vec::with_capacity(prim_uids.len() + child_nids.len());
        // 1. Keep stored refs in their stored order, consuming budget (so duplicate-uid prims keep
        //    their stored count; a stored ref beyond the live count, or a dangling one, is dropped).
        for &slot in stored {
            match slot {
                BodyChildRef::Prim(uid) => {
                    if let Some(n) = prim_budget.get_mut(&uid) {
                        if *n > 0 { *n -= 1; out.push(slot); }
                    }
                }
                BodyChildRef::Body(nid) => {
                    if body_remaining.remove(&nid) { out.push(slot); }
                }
            }
        }
        // 2. Append any prim slots still owed (prims first — legacy order), in primitives-Vec order.
        for &uid in &prim_uids {
            if let Some(n) = prim_budget.get_mut(&uid) {
                if *n > 0 { *n -= 1; out.push(BodyChildRef::Prim(uid)); }
            }
        }
        // 3. Append any child body not yet present (bodies after prims — legacy order).
        for &nid in &child_nids {
            if body_remaining.remove(&nid) { out.push(BodyChildRef::Body(nid)); }
        }
        out
    }

    /// Canonicalise and WRITE back a body's `child_order` so the stored field is a clean
    /// permutation of {prims} ∪ {direct child bodies}. Idempotent. Call after any structural
    /// mutation that adds/removes a prim or child node, and after constructing a body whose
    /// `primitives`/`children` were pre-populated. Reused by `migrate_v7_to_v8`.
    pub fn ensure_body_child_order(&mut self, body_id: NodeId) {
        let stored = match self.get(body_id).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { child_order, .. }) => child_order.clone(),
            _ => return,
        };
        let reconciled = self.reconcile_child_order(body_id, &stored);
        if let Some(n) = self.nodes.get_mut(&body_id) {
            if let NodePayload::SdfBody { child_order, .. } = &mut n.payload {
                *child_order = reconciled;
            }
        }
    }

    /// Mutable access to a body's raw `child_order`. Prefer the typed helpers
    /// (`reorder_child_order`, `ensure_body_child_order`) — this is for callers that need
    /// surgical insert at a chosen index (e.g. "new sub-body after current selection").
    pub fn body_child_order_mut(&mut self, body_id: NodeId) -> Option<&mut Vec<BodyChildRef>> {
        self.nodes.get_mut(&body_id).and_then(|n| {
            if let NodePayload::SdfBody { child_order, .. } = &mut n.payload {
                Some(child_order)
            } else {
                None
            }
        })
    }

    /// Insert `child` into `body_id`'s `child_order` at `insert_index`, FIRST reconciling so the
    /// stored order is canonical (covers a child added to `primitives`/`children` without a
    /// matching ref yet). If `child` is already present it is moved; otherwise it is inserted.
    /// `insert_index` is clamped. This is THE single reorder/placement op (§8 of the plan) — every
    /// reorder and every "place a new child at a sensible slot" routes through here.
    pub fn insert_child_order(&mut self, body_id: NodeId, child: BodyChildRef, insert_index: usize) {
        // Reconcile against live sets first (so a just-added prim/child has a slot to move).
        self.ensure_body_child_order(body_id);
        if let Some(order) = self.body_child_order_mut(body_id) {
            // Remove existing occurrence (move semantics).
            if let Some(pos) = order.iter().position(|&r| r == child) {
                order.remove(pos);
            }
            let at = insert_index.min(order.len());
            order.insert(at, child);
        }
    }

    /// Move `child` to `insert_index` within `body_id`'s `child_order` — the ONE reorder op.
    /// Thin alias over `insert_child_order` (which already moves-if-present). Kept as the named
    /// entry point all reorder callers (DnD, keyboard, prim/body reorder) funnel through.
    pub fn reorder_child_order(&mut self, body_id: NodeId, child: BodyChildRef, insert_index: usize) {
        self.insert_child_order(body_id, child, insert_index);
    }

    /// Remove a ref from a body's `child_order` (call when a prim/child is deleted or moved out).
    pub fn remove_child_order_ref(&mut self, body_id: NodeId, child: BodyChildRef) {
        if let Some(order) = self.body_child_order_mut(body_id) {
            order.retain(|&r| r != child);
        }
    }

    /// SAVE side-channel: collect each SdfBody's stored `child_order`, keyed by raw node id.
    /// `child_order` is `#[serde(skip)]`, so this is how the order leaves the in-memory graph for
    /// the `FL4ORDER` trailing section (see `project.rs save_binary`). Only NON-EMPTY orders are
    /// emitted — an empty order means "use the default (legacy) order", which the loader self-heals,
    /// so writing it would be redundant and would bloat the file. Returns `(node_id, order)` pairs.
    pub fn collect_body_child_orders(&self) -> Vec<(u64, Vec<BodyChildRef>)> {
        let mut out = Vec::new();
        for (&nid, node) in &self.nodes {
            if let NodePayload::SdfBody { child_order, .. } = &node.payload {
                if !child_order.is_empty() {
                    out.push((nid.0, child_order.clone()));
                }
            }
        }
        // Stable order for deterministic files (HashMap iteration is random).
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// LOAD side-channel: apply a loaded `(node_id, order)` map onto the live graph, then canonicalise
    /// EVERY SdfBody. A body present in the map gets its saved order (reconciled against the live
    /// prim/child sets so any dangling ref is dropped); a body NOT in the map (genuine v7 file, or a
    /// body that had the default order) self-heals to the legacy prims-then-bodies order. This is the
    /// single funnel that guarantees every body has a valid `child_order` after load.
    pub fn apply_body_child_orders(&mut self, orders: &[(u64, Vec<BodyChildRef>)]) {
        for (id, order) in orders {
            let nid = NodeId(*id);
            if let Some(slot) = self.body_child_order_mut(nid) {
                *slot = order.clone();
            }
        }
        // Canonicalise/self-heal ALL bodies (covers uncovered bodies + drops any dangling refs).
        let body_nids: Vec<NodeId> = self.nodes.iter()
            .filter_map(|(&nid, n)| matches!(&n.payload, NodePayload::SdfBody { .. }).then_some(nid))
            .collect();
        for nid in body_nids {
            self.ensure_body_child_order(nid);
        }
    }

    /// Resolve a body's `child_order` into a concrete eval/draw sequence over its OWN prim indices
    /// and direct child node ids. Each `Prim(uid)` slot is matched to the NEXT not-yet-consumed prim
    /// with that uid (positional within the uid group) — so duplicate-uid prims (e.g. uid 0 from
    /// `SdfPrimitive::new`) each get emitted exactly once, in primitives-Vec order. Returns a Vec of
    /// `OrderedChild`-style entries: `Ok(local_prim_index)` for a prim, `Err(child_node_id)` for a body.
    /// Shared by the bake serializer (graphics) and the outliner draw so they cannot diverge.
    pub fn resolve_child_order(&self, body_id: NodeId) -> Vec<Result<usize, NodeId>> {
        let prims: &[SdfPrimitive] = match self.body_primitives(body_id) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let order = self.body_child_order(body_id);
        let mut consumed = vec![false; prims.len()];
        let mut out = Vec::with_capacity(order.len());
        for slot in order {
            match slot {
                BodyChildRef::Prim(uid) => {
                    // Next unconsumed prim with this uid (positional → duplicate-uid safe).
                    if let Some(i) = prims.iter().enumerate()
                        .position(|(i, p)| p.prim_uid == uid && !consumed[i])
                    {
                        consumed[i] = true;
                        out.push(Ok(i));
                    }
                }
                BodyChildRef::Body(nid) => out.push(Err(nid)),
            }
        }
        out
    }

    /// The body's BASE slot = the FIRST entry of `resolve_child_order(body_id)` — the operand
    /// that SEEDS the boolean fold. The evaluator seeds the first operand regardless of its
    /// stored op (`field_eval.rs` `build_scope` walks `resolve_child_order`; the fold seeds
    /// `None` with the first child; volume_bake.wgsl's scope machine does the same on GPU).
    /// `Some(Ok(local_prim_index))` → that prim is the base; `Some(Err(child_nid))` → a
    /// SUB-BODY seeds the chain and NO prim of this body is the base (its result is the
    /// first operand). This is the SINGLE source of truth for every "is the base/first prim"
    /// UI check (outliner green dot, X op-cycle gating, boolean panel, raycast smart filter).
    /// NEVER key base-ness on flat/Vec index 0: `reorder_child_order` permutes ONLY
    /// `child_order` — the prims Vec never moves, so index-0 diverges after any reorder.
    /// Structural, visibility-agnostic: a hidden base prim is still the marked base (a hidden
    /// operand is skipped entirely by the eval, so no op of a hidden prim ever applies).
    pub fn body_base_slot(&self, body_id: NodeId) -> Option<Result<usize, NodeId>> {
        self.resolve_child_order(body_id).into_iter().next()
    }

    /// True iff the prim at BODY-LOCAL index `local_idx` is the body's base operand.
    /// Takes a local index (not `prim_uid`) because prim_uid is NOT unique (duplicate-uid
    /// prims exist — see `resolve_child_order`); the resolved local index is exact.
    pub fn is_base_prim(&self, body_id: NodeId, local_idx: usize) -> bool {
        self.body_base_slot(body_id) == Some(Ok(local_idx))
    }

    /// Debug-only: assert `child_order` is a permutation of {prims} ∪ {direct child bodies}.
    /// No missing refs, no dangling refs, no duplicates. Asserts (never repairs) per CLAUDE.md
    /// No-Bandaids — a violation means a structural mutation forgot to maintain the order.
    /// Depth-first traversal from a starting node. Yields (depth, NodeId, &SceneNode).
    /// Uses a lazy iterator — only allocates a small stack (O(depth)), not a full result Vec.
    pub fn walk_depth_first(
        &self,
        start: NodeId,
    ) -> DepthFirstIter<'_> {
        DepthFirstIter {
            graph: self,
            stack: vec![(0, start)],
        }
    }

}

/// Lazy depth-first iterator over a SceneGraph. Only allocates a stack (O(depth)),
/// not a full result Vec (O(nodes)). Supports early exit.
pub struct DepthFirstIter<'a> {
    graph: &'a SceneGraph,
    stack: Vec<(usize, NodeId)>,
}

impl<'a> Iterator for DepthFirstIter<'a> {
    type Item = (usize, NodeId, &'a SceneNode);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((depth, nid)) = self.stack.pop() {
            if let Some(node) = self.graph.nodes.get(&nid) {
                // Push children in reverse so first child is visited first
                for child_id in node.children.iter().rev() {
                    self.stack.push((depth + 1, *child_id));
                }
                return Some((depth, nid, node));
            }
        }
        None
    }
}
