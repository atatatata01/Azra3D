//! Scene graph integrity validation and post-load normalization.
//!
//! Pure relocation from former `scene_graph.rs`.

use super::{BodyChildRef, NodeId, NodePayload, SceneGraph};

impl SceneGraph {
    pub fn recalculate_next_id(&mut self) {
        let max_id = self.nodes.keys().map(|k| k.0).max().unwrap_or(0);
        if self.next_id <= max_id {
            self.next_id = max_id + 1;
        }
    }

    /// Migrate legacy ChamferUnion/ChamferSubtract body_ops to Union/Subtract + Chamfer profile.
    /// Called after deserialization to normalize old project files.
    pub fn normalize_body_ops(&mut self) {
        for (_, node) in self.nodes.iter_mut() {
            if let NodePayload::SdfBody { body_op, blend_profile, .. } = &mut node.payload {
                let (norm, prof) = body_op.normalize();
                if let Some(p) = prof {
                    *body_op = norm;
                    *blend_profile = p;
                }
            }
        }
    }

    /// Validate graph integrity. Returns a list of problems found (empty = healthy).
    /// Checks: root exists, parent/child consistency, no orphans, no cycles, next_id correctness.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Root must exist
        if !self.nodes.contains_key(&self.root) {
            errors.push(format!("Root {:?} not in nodes map", self.root));
            return errors; // can't check further
        }

        // Root must have no parent
        if let Some(root_node) = self.nodes.get(&self.root) {
            if root_node.parent.is_some() {
                errors.push("Root node has a parent".into());
            }
        }

        let max_id = self.nodes.keys().map(|k| k.0).max().unwrap_or(0);
        if self.next_id <= max_id {
            errors.push(format!("next_id ({}) <= max NodeId ({}) — will collide", self.next_id, max_id));
        }

        for (&nid, node) in &self.nodes {
            // Every non-root node must have a parent that exists
            if nid != self.root {
                match node.parent {
                    None => errors.push(format!("{:?} is not root but has no parent", nid)),
                    Some(pid) => {
                        if !self.nodes.contains_key(&pid) {
                            errors.push(format!("{:?} parent {:?} not in graph", nid, pid));
                        } else if let Some(parent) = self.nodes.get(&pid) {
                            if !parent.children.contains(&nid) {
                                errors.push(format!("{:?} claims parent {:?} but parent's children don't include it", nid, pid));
                            }
                        }
                    }
                }
            }

            // Every child reference must exist and point back
            for &child_id in &node.children {
                if !self.nodes.contains_key(&child_id) {
                    errors.push(format!("{:?} lists child {:?} that doesn't exist", nid, child_id));
                } else if let Some(child) = self.nodes.get(&child_id) {
                    if child.parent != Some(nid) {
                        errors.push(format!("{:?} lists child {:?} but child's parent is {:?}", nid, child_id, child.parent));
                    }
                }
            }

            // Walk to root — detect cycles (max steps = node count)
            if nid != self.root {
                let mut current = nid;
                let mut steps = 0;
                let max_steps = self.nodes.len();
                let mut reached_root = false;
                while let Some(n) = self.nodes.get(&current) {
                    if let Some(pid) = n.parent {
                        if pid == self.root { reached_root = true; break; }
                        current = pid;
                        steps += 1;
                        if steps > max_steps {
                            errors.push(format!("{:?} has a cycle in parent chain", nid));
                            break;
                        }
                    } else {
                        break;
                    }
                }
                if !reached_root && steps <= max_steps {
                    errors.push(format!("{:?} cannot reach root via parent chain", nid));
                }
            }
        }

        errors
    }
    pub fn validate_child_order(&self, body_id: NodeId) {
        let (order, prim_uids, child_nids) = match self.get(body_id).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { child_order, primitives, .. }) => (
                child_order.clone(),
                primitives.iter().map(|p| p.prim_uid).collect::<Vec<_>>(),
                self.direct_order_child_nodes(body_id),
            ),
            _ => return,
        };
        let expected = prim_uids.len() + child_nids.len();
        debug_assert_eq!(
            order.len(), expected,
            "child_order length {} != {} prims + {} child bodies for body {:?}",
            order.len(), prim_uids.len(), child_nids.len(), body_id,
        );
        // MULTISET match: child_order's Prim(uid) multiplicities must equal the prims' uid
        // multiset (prim_uid is NOT unique — SdfPrimitive::new defaults to 0), and each Body ref
        // must be a unique, live direct child. NOT a "no duplicates" check (duplicate-uid prims
        // legitimately produce repeated Prim(uid) slots).
        use std::collections::HashMap;
        let mut prim_want: HashMap<u32, i32> = HashMap::new();
        for &u in &prim_uids { *prim_want.entry(u).or_insert(0) += 1; }
        let mut prim_got: HashMap<u32, i32> = HashMap::new();
        let mut body_seen = std::collections::HashSet::new();
        for &r in &order {
            match r {
                BodyChildRef::Prim(uid) => { *prim_got.entry(uid).or_insert(0) += 1; }
                BodyChildRef::Body(nid) => {
                    debug_assert!(child_nids.contains(&nid),
                        "child_order has dangling Body ref {:?} in body {:?}", nid, body_id);
                    debug_assert!(body_seen.insert(nid),
                        "child_order has duplicate Body ref {:?} in body {:?}", nid, body_id);
                }
            }
        }
        debug_assert_eq!(prim_got, prim_want,
            "child_order prim multiset {:?} != prims uid multiset {:?} for body {:?}",
            prim_got, prim_want, body_id);
    }


}
