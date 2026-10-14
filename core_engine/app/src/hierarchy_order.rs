//! Hierarchy ordering — move selected items up/down in their parent's child list.
//!
//! Works for both prims (within body) and scene graph nodes (within parent).
//! Designed as standalone functions callable from keyboard, outliner buttons,
//! macros, or any future tool binding.

use core_math::scene_graph::{NodeId, NodePayload};
use crate::*;

/// Direction + magnitude for hierarchy reorder operations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReorderDirection {
    /// Move one position toward the beginning (lower index).
    Up,
    /// Move one position toward the end (higher index).
    Down,
    /// Move to the very first position (index 0).
    First,
    /// Move to the very last position.
    Last,
}

impl FourthLayerApp {
    // ================================================================
    // Public API — call from keyboard, outliner buttons, macros, etc.
    // ================================================================

    /// Move the current selection up or down in the hierarchy.
    /// Handles both prims (within body) and nodes (within parent).
    /// Single undo entry per call.
    pub(crate) fn reorder_selection(&mut self, direction: ReorderDirection) {
        // Try prims first (prim selection takes priority when both exist)
        let prim_indices = self.selected_prim_indices_unified();
        if !prim_indices.is_empty() {
            self.reorder_selected_prims(direction, &prim_indices);
            return;
        }

        // Try nodes
        let sel_nodes: Vec<NodeId> = self.sel.usel.selected_nodes().collect();
        if !sel_nodes.is_empty() {
            self.reorder_selected_nodes(direction, &sel_nodes);
            return;
        }

        self.toast("Nothing selected to reorder");
    }

    // ================================================================
    // Prim reordering within body
    // ================================================================

    fn reorder_selected_prims(&mut self, direction: ReorderDirection, flat_indices: &[usize]) {
        if flat_indices.is_empty() { return; }

        // All selected prims must be in the same body
        let body_nid = match self.scene.prim_body_map.get(flat_indices[0]) {
            Some(&(nid, _)) => nid,
            None => return,
        };
        let same_body = flat_indices.iter().all(|&fi| {
            self.scene.prim_body_map.get(fi).map(|&(nid, _)| nid) == Some(body_nid)
        });
        if !same_body {
            self.toast("Can't reorder prims across bodies");
            return;
        }

        // UNIFIED ORDER: the visible/eval sequence is the body's `child_order` (prims interleaved
        // with sub-bodies), NOT the `primitives` Vec. Reorder the selected prims' refs WITHIN
        // child_order so the move is meaningful (and lands relative to sub-bodies too).
        use core_math::scene_graph::BodyChildRef;
        // Map the selected flat prims → their PrimUid → their position in child_order.
        let sel_uids: Vec<u32> = flat_indices.iter()
            .filter_map(|&fi| self.scene.primitives.get(fi).map(|p| p.prim_uid))
            .collect();
        let order = self.scene.scene_graph.body_child_order(body_nid);
        let mut order_positions: Vec<usize> = sel_uids.iter()
            .filter_map(|uid| order.iter().position(|&r| r == BodyChildRef::Prim(*uid)))
            .collect();
        order_positions.sort_unstable();
        order_positions.dedup();
        if order.len() < 2 || order_positions.is_empty() { return; }

        let desc = match direction {
            ReorderDirection::Up => "Move prim(s) up",
            ReorderDirection::Down => "Move prim(s) down",
            ReorderDirection::First => "Move prim(s) to top",
            ReorderDirection::Last => "Move prim(s) to bottom",
        };

        let dir = direction;
        let positions = order_positions.clone();
        // Already at edge (e.g. Move Up on top items) → no empty undo.
        if !reorder_would_change(&positions, dir, order.len()) {
            return;
        }
        self.transaction(desc, |s| {
            // Canonicalise first so positions are valid against the stored order.
            s.scene.scene_graph.ensure_body_child_order(body_nid);
            if let Some(order_mut) = s.scene.scene_graph.body_child_order_mut(body_nid) {
                let len = order_mut.len();
                if len < 2 { return; }
                reorder_indices_in_vec(order_mut, &positions, dir, len);
            }
            s.sync_flat_cache();
            // REORDER1 PARITY: reorder changes the boolean eval chain → structural. SCENE alone does NOT
            // bake (process_dirty skips trigger_body_vol_bake when no body is in dirty_bodies), so the
            // surface stayed stale → the body collapsed to a thin slab. Mark the body volume-dirty.
            s.mark_node_body_vol_dirty(body_nid);
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
        });
    }

    // ================================================================
    // Node reordering within parent
    // ================================================================

    fn reorder_selected_nodes(&mut self, direction: ReorderDirection, sel_nodes: &[NodeId]) {
        if sel_nodes.is_empty() { return; }

        // All selected nodes must share the same parent
        let parent_nid = match self.scene.scene_graph.get(sel_nodes[0]).and_then(|n| n.parent) {
            Some(p) => p,
            None => return,
        };
        let same_parent = sel_nodes.iter().all(|&nid| {
            self.scene.scene_graph.get(nid).and_then(|n| n.parent) == Some(parent_nid)
        });
        if !same_parent {
            self.toast("Can't reorder nodes with different parents");
            return;
        }

        let desc = match direction {
            ReorderDirection::Up => "Move node(s) up",
            ReorderDirection::Down => "Move node(s) down",
            ReorderDirection::First => "Move node(s) to top",
            ReorderDirection::Last => "Move node(s) to bottom",
        };
        let dir = direction;
        let parent = parent_nid;

        // If the parent is an SdfBody, sub-body order lives in its `child_order` (interleaved with
        // prims) — reorder THAT. Otherwise (Root / SdfObject), order is the node-edge `children` Vec.
        let parent_is_body = matches!(
            self.scene.scene_graph.get(parent_nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        );
        if parent_is_body {
            use core_math::scene_graph::BodyChildRef;
            let order = self.scene.scene_graph.body_child_order(parent_nid);
            let mut positions: Vec<usize> = sel_nodes.iter()
                .filter_map(|&nid| order.iter().position(|&r| r == BodyChildRef::Body(nid)))
                .collect();
            positions.sort_unstable();
            positions.dedup();
            if order.len() < 2 || positions.is_empty() { return; }
            if !reorder_would_change(&positions, dir, order.len()) {
                return;
            }
            self.transaction(desc, |s| {
                s.scene.scene_graph.ensure_body_child_order(parent);
                if let Some(order_mut) = s.scene.scene_graph.body_child_order_mut(parent) {
                    let len = order_mut.len();
                    if len < 2 { return; }
                    reorder_indices_in_vec(order_mut, &positions, dir, len);
                }
                s.sync_flat_cache();
                s.mark_node_body_vol_dirty(parent);
                s.mark_dirty(DirtyFlags::SCENE);
                s.mark_outliner_dirty();
            });
            return;
        }

        // Get child indices within parent (Root / SdfObject parent — node-edge order).
        let children: Vec<NodeId> = self.scene.scene_graph.children(parent_nid).to_vec();
        let mut child_positions: Vec<usize> = sel_nodes.iter()
            .filter_map(|&nid| children.iter().position(|&cid| cid == nid))
            .collect();
        child_positions.sort_unstable();
        child_positions.dedup();

        if children.len() < 2 { return; }

        let positions = child_positions.clone();
        if !reorder_would_change(&positions, dir, children.len()) {
            return;
        }
        self.transaction(desc, |s| {
            // Get mutable children list via the parent node
            if let Some(parent_node) = s.scene.scene_graph.get_mut(parent) {
                let len = parent_node.children.len();
                if len < 2 { return; }
                reorder_indices_in_vec(&mut parent_node.children, &positions, dir, len);
            }
            s.sync_flat_cache();
            // REORDER1 PARITY: reordering child nodes (e.g. a sub-body) changes the
            // PARENT body's child boolean eval chain → structural. SCENE alone does NOT
            // bake (process_dirty skips trigger_body_vol_bake when no body is in
            // dirty_bodies) → the parent showed a stale "thin slab" volume. Mark the
            // SHARED PARENT body volume-dirty (checklist step 4): if `parent` is an
            // SdfBody it's marked directly; if it's Root/SdfObject it resolves to the
            // owning body (no-op for independent top-level bodies). Matches the prim
            // reorder above (reorder_selected_prims:100) and the DnD paths.
            s.mark_node_body_vol_dirty(parent);
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
        });
    }
}

// ================================================================
// Generic reorder logic — works on any Vec<T>
// ================================================================

/// Move items at `selected_indices` up/down/first/last within a `Vec<T>`.
/// `selected_indices` must be sorted ascending and within `0..len`.
/// True when `reorder_indices_in_vec` would actually move anything.
/// Used so "Move up" at the top (etc.) does not open an empty undo entry.
fn reorder_would_change(selected_indices: &[usize], direction: ReorderDirection, len: usize) -> bool {
    if selected_indices.is_empty() || len < 2 { return false; }
    match direction {
        ReorderDirection::Up => selected_indices.iter()
            .any(|&idx| idx > 0 && !selected_indices.contains(&(idx - 1))),
        ReorderDirection::Down => selected_indices.iter()
            .any(|&idx| idx + 1 < len && !selected_indices.contains(&(idx + 1))),
        ReorderDirection::First => selected_indices.iter().enumerate()
            .any(|(i, &idx)| idx != i),
        ReorderDirection::Last => {
            let n = selected_indices.len();
            selected_indices.iter().enumerate()
                .any(|(i, &idx)| idx != len - n + i)
        }
    }
}

fn reorder_indices_in_vec<T>(vec: &mut Vec<T>, selected_indices: &[usize], direction: ReorderDirection, len: usize) {
    if selected_indices.is_empty() || len < 2 { return; }

    match direction {
        ReorderDirection::Up => {
            // Move each selected item one position up (toward index 0).
            // Process in order so items don't leapfrog each other.
            for &idx in selected_indices {
                if idx > 0 && !selected_indices.contains(&(idx - 1)) {
                    vec.swap(idx, idx - 1);
                }
            }
        }
        ReorderDirection::Down => {
            // Move each selected item one position down (toward end).
            // Process in reverse so items don't leapfrog each other.
            for &idx in selected_indices.iter().rev() {
                if idx + 1 < len && !selected_indices.contains(&(idx + 1)) {
                    vec.swap(idx, idx + 1);
                }
            }
        }
        ReorderDirection::First => {
            // Move all selected items to the front, preserving their relative order.
            let mut extracted: Vec<T> = Vec::new();
            let mut remaining: Vec<T> = Vec::new();
            for (i, item) in vec.drain(..).enumerate() {
                if selected_indices.contains(&i) {
                    extracted.push(item);
                } else {
                    remaining.push(item);
                }
            }
            vec.extend(extracted);
            vec.extend(remaining);
        }
        ReorderDirection::Last => {
            // Move all selected items to the end, preserving their relative order.
            let mut extracted: Vec<T> = Vec::new();
            let mut remaining: Vec<T> = Vec::new();
            for (i, item) in vec.drain(..).enumerate() {
                if selected_indices.contains(&i) {
                    extracted.push(item);
                } else {
                    remaining.push(item);
                }
            }
            vec.extend(remaining);
            vec.extend(extracted);
        }
    }
}
