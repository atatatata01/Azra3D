// Level 6 — Predictive Warming
//
// During idle frames, pre-bake bodies that don't yet have a cached grid
// so switching to them is instant. Heuristics prioritize recently-selected
// and outliner-hovered bodies.

use core_math::scene_graph::{NodeId, SceneGraph};
use std::collections::VecDeque;

/// Tracks warming state across frames.
pub(crate) struct WarmingState {
    /// Recently activated bodies (most-recent first). Used to prioritize
    /// warming for bodies the user is likely to return to.
    pub mru_bodies: VecDeque<NodeId>,
    /// Maximum MRU entries to track.
    max_mru: usize,
    /// Body currently being hovered in the outliner (strongest signal).
    pub hovered_body: Option<NodeId>,
    /// Tracks the previous frame's active body to detect activation changes.
    prev_active_body: Option<NodeId>,
}

impl WarmingState {
    pub fn new() -> Self {
        Self {
            mru_bodies: VecDeque::new(),
            max_mru: 16,
            hovered_body: None,
            prev_active_body: None,
        }
    }

    /// Record that a body was activated (selected / switched to).
    pub fn record_activation(&mut self, body_nid: NodeId) {
        // Remove if already in MRU, then push to front
        self.mru_bodies.retain(|&nid| nid != body_nid);
        self.mru_bodies.push_front(body_nid);
        if self.mru_bodies.len() > self.max_mru {
            self.mru_bodies.pop_back();
        }
    }

    /// Call once per frame with the current active body. Automatically
    /// records activation when the active body changes.
    pub fn track_active_body(&mut self, active_body_nid: NodeId) {
        if self.prev_active_body != Some(active_body_nid) {
            self.record_activation(active_body_nid);
            self.prev_active_body = Some(active_body_nid);
        }
    }

    /// Pick the best body to warm next, given the current cache state.
    ///
    /// Returns `None` if all bodies are already cached or no candidates exist.
    /// `active_body_nid`: the currently active body (skip — it's already live).
    /// `has_cache`: closure that returns true if a body already has a cached grid.
    pub fn pick_warming_candidate(
        &self,
        active_body_nid: NodeId,
        sg: &SceneGraph,
        has_cache: impl Fn(NodeId) -> bool,
    ) -> Option<NodeId> {
        // A warming candidate must be a body the bake loop can actually bake: a live,
        // TOP-LEVEL SdfBody. A NESTED sub-body never owns a grid (trigger_bake skips it —
        // it is subsumed into its ancestor's recursive bake), so `has_cache` is false for
        // it FOREVER: picking one turned poll_warming into an every-idle-frame
        // mark_body_dirty + clear_debounce + trigger loop the moment the first sub-body
        // existed (the outliner Group gesture) — the "[RetightenArm] … while IDLE"
        // settle-loop feeder. Enforced HERE, on every source (hover of a sub-body row,
        // an MRU entry that got NESTED after it was recorded, and the scene scan), so no
        // caller can reintroduce it. Same nested-scope rule as mark_stale_bodies_dirty /
        // update_lod_resolutions / the bake loop itself.
        let warmable = |nid: NodeId| {
            nid != active_body_nid
                && sg.get(nid).map_or(false, |n| matches!(
                    n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                && sg.parent_body(nid).is_none()
                && !has_cache(nid)
        };

        // 1. Outliner hover (strongest signal)
        if let Some(hovered) = self.hovered_body {
            if warmable(hovered) {
                return Some(hovered);
            }
        }

        // 2. MRU bodies (recently selected)
        for &nid in &self.mru_bodies {
            if warmable(nid) {
                return Some(nid);
            }
        }

        // 3. Any uncached body visible in the scene
        for (nid, node) in sg.all_nodes() {
            if !node.visible { continue; }
            if warmable(nid) {
                return Some(nid);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::primitives::{BlendProfile, BooleanOp, PbrMaterial};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;

    fn body_payload(name: &str) -> NodePayload {
        NodePayload::SdfBody {
            name: name.into(), primitives: Vec::new(), mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),}
    }

    /// OWNER REPRO 2026-07-14 (root #2 of "group into sub-body → [RetightenArm] spam while
    /// IDLE"): a NESTED sub-body never owns a grid (the bake loop skips it), so `has_cache`
    /// is false for it forever — picking it as a warming candidate turned poll_warming into
    /// an every-idle-frame mark_body_dirty + clear_debounce + trigger_body_vol_bake loop the
    /// moment the first sub-body was created. A candidate must be a live TOP-LEVEL SdfBody,
    /// on EVERY source: hover, MRU, and the scene scan.
    #[test]
    fn warming_never_picks_nested_sub_bodies() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let active = sg.add_child(root, body_payload("Active"));
        let top = sg.add_child(root, body_payload("Body.002"));
        let sub = sg.add_child(top, body_payload("Group.001")); // nested — never bakeable

        let w = WarmingState::new();
        // Scene scan: the nested body is uncached forever, but must never be picked;
        // the uncached TOP-LEVEL body is the valid candidate.
        let pick = w.pick_warming_candidate(active, &sg, |_| false);
        assert_eq!(pick, Some(top),
            "scan must pick the uncached TOP-LEVEL body, never the nested sub-body");
        // With the top-level body cached, the ONLY uncached body left is the nested one —
        // the picker must return None instead of spinning on it (the idle-loop feeder).
        let pick = w.pick_warming_candidate(active, &sg, |nid| nid == top);
        assert_eq!(pick, None,
            "FIXED: a nested sub-body must never be a warming candidate (it can never \
             acquire a grid — picking it loops mark_body_dirty+trigger every idle frame)");

        // Hover source: hovering the sub-body's outliner row must not warm it.
        let mut w2 = WarmingState::new();
        w2.hovered_body = Some(sub);
        assert_eq!(w2.pick_warming_candidate(active, &sg, |nid| nid == top), None,
            "hovering a nested sub-body row must not make it a warming candidate");

        // MRU source: a body recorded in the MRU and NESTED afterwards (exactly the Group
        // gesture on a former top-level body) must be skipped too.
        let mut w3 = WarmingState::new();
        w3.record_activation(sub);
        assert_eq!(w3.pick_warming_candidate(active, &sg, |nid| nid == top), None,
            "an MRU entry that got nested after being recorded must be skipped");
    }
}
