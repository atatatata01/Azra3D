//! Unified selection operations — canonical source of truth for selection state.

use crate::*;

impl FourthLayerApp {
    // ==================================================================
    // Selection Operations
    // ==================================================================

    /// Selection set changed → outliner + GPU body outline (`sel_bit` on instances).
    ///
    /// `sel_bit` is baked into the **structural** instance list. `mark_dirty(RENDER)` alone
    /// does not force a structural rebuild, so multi-select could add nodes to the set while
    /// the green rim stayed on the previous single object (owner: highlight does not follow
    /// adding objects to selection). Always stale the structural list here.
    #[inline]
    pub(crate) fn mark_selection_dirty(&mut self) {
        self.outliner.outliner_dirty = true;
        self.structural_draw_stale = true;
        self.raster_upload_plane_key = 0; // force instance re-upload (sel_bit in material_idx)
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Select one item, clearing everything else. Updates last_clicked anchor.
    pub(crate) fn select_single_unified(&mut self, id: SceneItemId) {
        self.sel.usel.selected.clear();
        self.sel.usel.selected.insert(id);
        self.sel.usel.active = Some(id);
        self.sel.usel.last_clicked = Some(id);
        self.sync_orbit_center_to_selection();
        self.mark_selection_dirty();
    }

    /// Toggle item in selection (Ctrl+click). Updates last_clicked anchor.
    pub(crate) fn toggle_select_unified(&mut self, id: SceneItemId) {
        if self.sel.usel.selected.contains(&id) {
            self.sel.usel.selected.swap_remove(&id);
            if self.sel.usel.active == Some(id) {
                self.sel.usel.active = self.sel.usel.selected.last().copied();
            }
        } else {
            self.sel.usel.selected.insert(id);
            self.sel.usel.active = Some(id);
        }
        self.sel.usel.last_clicked = Some(id);
        self.sync_orbit_center_to_selection();
        self.mark_selection_dirty();
    }

    /// Range select in outliner visual order (Shift+click).
    /// Selects ALL items between last_clicked and target, regardless of type.
    /// Does NOT update last_clicked — anchor stays where it was.
    pub(crate) fn range_select_unified(&mut self, target: SceneItemId, visual_order: &[SceneItemId]) {
        if let Some(last) = self.sel.usel.last_clicked {
            let last_pos = visual_order.iter().position(|&id| id == last);
            let target_pos = visual_order.iter().position(|&id| id == target);
            if let (Some(a), Some(b)) = (last_pos, target_pos) {
                let lo = a.min(b);
                let hi = a.max(b);
                for &id in &visual_order[lo..=hi] {
                    self.sel.usel.selected.insert(id);
                }
                self.sel.usel.active = Some(target);
                // last_clicked intentionally NOT updated — anchor persists
                self.sync_orbit_center_to_selection();
                self.mark_selection_dirty();
            } else {
                // FALLBACK — never a silent no-op: anchor or target missing from
                // visual_order (first frame, filtered/collapsed row, stale anchor).
                // Additively select the clicked item so the click always has an effect.
                self.sel.usel.selected.insert(target);
                self.sel.usel.active = Some(target);
                self.sync_orbit_center_to_selection();
                self.mark_selection_dirty();
            }
        } else {
            self.select_single_unified(target);
            return;
        }
    }

    /// Clear all selection.
    pub(crate) fn clear_selection_unified(&mut self) {
        self.sel.usel.selected.clear();
        self.sel.usel.active = None;
        self.sel.usel.last_clicked = None;
        self.mark_selection_dirty();
    }

    /// Bulk additive select — the canonical writer for multi-item selection
    /// (menu Select All, outliner select-children / select-hierarchy).
    /// Inserts every id, sets active to the first id if none is active,
    /// and marks outliner + render dirty. Callers wanting "replace" semantics
    /// call `clear_selection_unified()` first.
    pub(crate) fn extend_selection_unified(&mut self, ids: &[SceneItemId]) {
        for &id in ids {
            self.sel.usel.selected.insert(id);
        }
        if self.sel.usel.active.is_none() {
            self.sel.usel.active = ids.first().copied();
        }
        self.sync_orbit_center_to_selection();
        self.mark_selection_dirty();
    }

    /// Deselect items that no longer exist in the scene.
    pub(crate) fn sanitize_selection_unified(&mut self) {
        // Build a HashSet of valid light IDs once — O(L) — then do O(1) lookups per selected item.
        let valid_light_ids: std::collections::HashSet<u32> =
            self.scene.scene_graph.lights().map(|(_, d)| d.id).collect();

        self.sel.usel.selected.retain(|id| match id {
            SceneItemId::Prim(uid) => self.scene.prim_uid_to_index.contains_key(uid),
            SceneItemId::Node(nid) => self.scene.scene_graph.get(*nid).is_some(),
            SceneItemId::Light(lid) => valid_light_ids.contains(lid),
        });
        if let Some(active) = self.sel.usel.active {
            if !self.sel.usel.selected.contains(&active) {
                self.sel.usel.active = self.sel.usel.selected.last().copied();
            }
        }
        if let Some(last) = self.sel.usel.last_clicked {
            let exists = match last {
                SceneItemId::Prim(uid) => self.scene.prim_uid_to_index.contains_key(&uid),
                SceneItemId::Node(nid) => self.scene.scene_graph.get(nid).is_some(),
                SceneItemId::Light(lid) => valid_light_ids.contains(&lid),
            };
            if !exists { self.sel.usel.last_clicked = None; }
        }
    }

    /// Select newly created items, replacing any existing selection of the same type.
    /// Used after create/duplicate/mirror to select the results.
    pub(crate) fn select_after_create(&mut self, new_ids: &[SceneItemId]) {
        if new_ids.is_empty() { return; }
        // Determine which type(s) we're replacing
        let replacing_prims = new_ids.iter().any(|id| matches!(id, SceneItemId::Prim(_)));
        let replacing_nodes = new_ids.iter().any(|id| matches!(id, SceneItemId::Node(_)));
        let replacing_lights = new_ids.iter().any(|id| matches!(id, SceneItemId::Light(_)));
        // Remove old items of the same type(s)
        self.sel.usel.selected.retain(|id| {
            !(replacing_prims && matches!(id, SceneItemId::Prim(_)))
            && !(replacing_nodes && matches!(id, SceneItemId::Node(_)))
            && !(replacing_lights && matches!(id, SceneItemId::Light(_)))
        });
        // Add new items
        for &id in new_ids {
            self.sel.usel.selected.insert(id);
        }
        // Active = first new item (matches existing convention)
        self.sel.usel.active = new_ids.first().copied();
        // Update last_clicked for shift+range consistency
        self.sel.usel.last_clicked = new_ids.first().copied();
        self.sync_orbit_center_to_selection();
        self.mark_selection_dirty();
    }

    /// Remove items from selection by predicate, then fix active fallback.
    /// Used after delete, dissolve, scope exit, or any operation that
    /// removes items from the scene.
    pub(crate) fn remove_from_selection<F>(&mut self, should_remove: F)
    where F: Fn(&SceneItemId) -> bool
    {
        self.sel.usel.selected.retain(|id| !should_remove(id));
        if let Some(active) = self.sel.usel.active {
            if !self.sel.usel.selected.contains(&active) {
                self.sel.usel.active = self.sel.usel.selected.last().copied();
            }
        }
        if let Some(lc) = self.sel.usel.last_clicked {
            if should_remove(&lc) {
                self.sel.usel.last_clicked = None;
            }
        }
        self.mark_selection_dirty();
    }

    /// Remove specific IDs from selection.
    pub(crate) fn deselect_items(&mut self, ids: &[SceneItemId]) {
        self.remove_from_selection(|id| ids.contains(id));
    }

    // ==================================================================
    // Resolver Helpers
    // ==================================================================

    // NOTE: prim_index_by_uid() already exists in prim_actions.rs — not duplicated.

    /// All selected prim flat indices (resolved from uids). Order matches insertion.
    pub(crate) fn selected_prim_indices_unified(&self) -> Vec<usize> {
        self.sel.usel.selected_prims()
            .filter_map(|uid| self.prim_index_by_uid(uid))
            .collect()
    }

    /// Active prim as flat index (resolved from uid).
    pub(crate) fn active_prim_index_unified(&self) -> Option<usize> {
        self.sel.usel.active_prim_uid().and_then(|uid| self.prim_index_by_uid(uid))
    }

    /// The prim the user is effectively working on: the active prim, or — when
    /// `active` has fallen back to a body/node (e.g. after a range/body select) —
    /// the last-clicked prim. Mirrors the properties-panel resolution
    /// (`properties.rs:20`), so a visibly-selected prim deterministically counts
    /// as "the selected object" for tool activation (fixes the intermittent B
    /// double-press: plain `active_prim_index_unified()` was None whenever
    /// `active` wasn't the Prim variant even though a prim was clearly selected).
    ///
    /// Also accepts: any single selected Prim id, or exactly one prim under the
    /// current selection (including a body with one child). Owner 2026-08-08: B
    /// after prim pick still sometimes needed a second press when `active` was the
    /// body node and `last_clicked` had not been stamped as Prim.
    pub(crate) fn resolved_prim_index_unified(&self) -> Option<usize> {
        if let Some(i) = self.active_prim_index_unified() {
            return Some(i);
        }
        if let Some(SceneItemId::Prim(uid)) = self.sel.usel.last_clicked {
            if let Some(i) = self.prim_index_by_uid(uid) {
                return Some(i);
            }
        }
        // Any single directly-selected prim (active may be a body while prims stay selected).
        let direct = self.selected_prim_indices_unified();
        if direct.len() == 1 {
            return Some(direct[0]);
        }
        // Single prim under selected body(s) — common after object-mode body pick then edit.
        let under = self.selected_prim_indices_including_bodies();
        if under.len() == 1 {
            return Some(under[0]);
        }
        None
    }

    /// Prim indices for the GPU **prim outline** pass only.
    ///
    /// Direct prim selection only. Object-mode **body** selection is drawn by the body mesh
    /// selection rim (`raster_gbuffer` sel_bit) — expanding bodies into every child prim here
    /// double-outlined the whole shape (body contour + every primitive contour on top).
    /// Prim indices for the GPU selection outline pass.
    ///
    /// Direct prims always. **Nested SdfBody (sub-body) picks** also expand to every prim
    /// owned by that body (and nested descendants) so the sub-body volume is outlined —
    /// nested bodies have no standalone mesh (they bake into the parent).
    ///
    /// **Group / SdfObject** under a body: expand to prims of every descendant SdfBody so
    /// selecting a kitbash folder highlights only that folder's volumes, not the parent.
    pub(crate) fn selected_prim_indices_for_gpu(&self) -> Vec<usize> {
        use core_math::scene_graph::NodePayload;
        let mut set: std::collections::HashSet<usize> =
            self.selected_prim_indices_unified().into_iter().collect();
        let sel_nodes: Vec<_> = self.sel.usel.selected_nodes().collect();
        for &nid in &sel_nodes {
            let payload = self.scene.scene_graph.get(nid).map(|n| &n.payload);
            let expand = match payload {
                Some(NodePayload::SdfBody { .. }) => {
                    self.scene.scene_graph.is_sub_body(nid)
                }
                Some(NodePayload::Group { .. }) | Some(NodePayload::SdfObject { .. }) => {
                    // Group under a body, or group of nested bodies: show their prim volumes.
                    true
                }
                _ => false,
            };
            if !expand {
                continue;
            }
            // Prims whose owning body is `nid` (if body) or a descendant SdfBody under it.
            for (flat_idx, (body_nid, _)) in self.scene.prim_body_map.iter().enumerate() {
                if *body_nid == nid
                    || self.scene.scene_graph.is_descendant_of(*body_nid, nid)
                {
                    set.insert(flat_idx);
                }
            }
        }
        set.into_iter().collect()
    }

    /// Direct prims **plus** every prim under selected SDF body nodes.
    /// For tools/edits that treat a body pick as “all its prims” — never for the outline pass
    /// of top-level bodies (those use mesh rim). Nested sub-body outline uses
    /// [`Self::selected_prim_indices_for_gpu`] instead.
    pub(crate) fn selected_prim_indices_including_bodies(&self) -> Vec<usize> {
        let mut set: std::collections::HashSet<usize> =
            self.selected_prim_indices_unified().into_iter().collect();
        let sel_nodes: Vec<_> = self.sel.usel.selected_nodes().collect();
        if !sel_nodes.is_empty() {
            for (flat_idx, (body_nid, _)) in self.scene.prim_body_map.iter().enumerate() {
                if sel_nodes.contains(body_nid) {
                    set.insert(flat_idx);
                    continue;
                }
                // Also include prims under nested descendants of a selected body/group.
                for &sel in &sel_nodes {
                    if self.scene.scene_graph.is_descendant_of(*body_nid, sel) {
                        set.insert(flat_idx);
                        break;
                    }
                }
            }
        }
        set.into_iter().collect()
    }

}
