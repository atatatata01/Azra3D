//! Outliner helpers — find/scroll, expand parents, solo toggle.
//! Shared actions called from keyboard, context menu, outliner UI.

use crate::*;

/// Result of the shared rename text-edit widget.
pub(crate) enum RenameResult {
    /// Text edit is active, keep showing it.
    Editing,
    /// User confirmed — return the new name.
    Commit(String),
    /// User cancelled — discard.
    Cancel,
}

/// Shared rename text-edit widget used by all outliner row types.
/// Handles grace period, focus management, Enter/Escape/click-away.
pub(crate) fn draw_rename_widget(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    rename_buf: &mut String,
    grace_frames: &mut u8,
) -> RenameResult {
    let te = ui.text_edit_singleline(rename_buf);

    // Grace period: skip lost_focus check for first frames after rename starts
    if *grace_frames > 0 {
        *grace_frames -= 1;
        te.request_focus();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return RenameResult::Cancel;
        }
        return RenameResult::Editing;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return RenameResult::Cancel;
    }

    if te.lost_focus() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        if rename_buf.is_empty() {
            return RenameResult::Cancel;
        }
        return RenameResult::Commit(rename_buf.clone());
    }

    // Keep focus while editing
    if !te.has_focus() {
        te.request_focus();
    }

    RenameResult::Editing
}

/// Check if a click is a double-click using time-based detection.
/// Returns true if this click is the second within 0.4s on the same id.
/// Call inside a `resp.clicked()` block — replaces `resp.double_clicked()`.
/// Uses `ui.ctx().data` for longer-lived storage (survives frame GC).
pub(crate) fn check_double_click(ui: &mut egui::Ui, id: egui::Id) -> bool {
    let now = ui.input(|i| i.time);
    let last_click = ui.ctx().data(|d| d.get_temp::<f64>(id).unwrap_or(0.0));
    if now - last_click < 0.4 && last_click > 0.0 {
        ui.ctx().data_mut(|d| d.insert_temp::<f64>(id, 0.0));
        true
    } else {
        ui.ctx().data_mut(|d| d.insert_temp::<f64>(id, now));
        false
    }
}

/// Width of the fixed visibility (eye) column at the START of every outliner row.
pub(crate) const EYE_COL_W: f32 = 22.0;

/// Allocate a full-width outliner row with a **drag sink registered first** (under children).
///
/// Why: outliner DnD is a custom raw-pointer tracker; row widgets are click/hover only.
/// egui 0.29 then latches some other `Sense::drag` widget as `potential_drag_id` — the
/// SidePanel `__resize` edge or the outliner height splitter — so a row drag resizes the
/// right panel. Registering a full-row `Sense::drag` **before** the clickable children makes
/// the row the drag target (clicks still hit children on top — same as button-on-ScrollArea).
///
/// Returns the full row rect (use for DnD press tests / drop zones / backgrounds).
pub(crate) fn outliner_row_shell<R>(
    ui: &mut egui::Ui,
    id: egui::Id,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> (egui::Rect, R) {
    let row_h = ui.spacing().interact_size.y.max(18.0);
    let w = ui.available_width().max(1.0);
    let out = ui.allocate_ui(egui::vec2(w, row_h), |ui| {
        // Full-row drag sink FIRST — under children so hit_test keeps clicks on labels/buttons.
        let sink_rect = ui.max_rect();
        let _ = ui.interact(sink_rect, id, egui::Sense::drag());
        ui.horizontal(|ui| add_contents(ui)).inner
    });
    (out.response.rect, out.inner)
}

/// Fixed-column visibility toggle: the eye button sits in ONE column at the left
/// edge of the outliner for EVERY row type, regardless of tree depth — indentation
/// applies only to the content AFTER the eye column. Call this FIRST inside the
/// row's `horizontal`; it also reserves leading space so shallow rows' content
/// starts after the column. Returns the button response (chain `.clicked()` /
/// `.on_hover_text()` at the call site).
pub(crate) fn eye_column_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    let col_left = ui.clip_rect().left() + 2.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(col_left, ui.cursor().min.y),
        egui::vec2(EYE_COL_W - 4.0, 16.0),
    );
    // Reserve horizontal space so row content never overlaps the eye column
    // (only needed at shallow depths; deeper indents already clear it).
    let pad = (col_left + EYE_COL_W) - ui.cursor().min.x;
    if pad > 0.0 {
        ui.add_space(pad);
    }
    let icon = if visible { "\u{1F441}" } else { "   " };
    ui.put(rect, egui::Button::new(icon).small())
}

/// Draw the warm-stone folder icon used by Object headers and Group node rows.
pub(crate) fn draw_folder_icon(ui: &mut egui::Ui) {
    let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let p = ui.painter();
    let o = icon_rect.min;
    let fill = egui::Color32::from_rgba_unmultiplied(140, 122, 90, 217);
    let highlight = egui::Color32::from_rgba_unmultiplied(158, 139, 104, 128);
    let stroke = egui::Stroke::new(0.5, egui::Color32::from_rgb(166, 149, 110));
    p.add(egui::Shape::Path(egui::epaint::PathShape::convex_polygon(
        vec![
            egui::pos2(o.x, o.y + 3.0),
            egui::pos2(o.x + 4.5, o.y + 2.0),
            egui::pos2(o.x + 5.5, o.y + 4.0),
            egui::pos2(o.x + 13.0, o.y + 4.0),
            egui::pos2(o.x + 14.0, o.y + 5.0),
            egui::pos2(o.x + 14.0, o.y + 13.0),
            egui::pos2(o.x, o.y + 13.0),
        ],
        fill, stroke,
    )));
    p.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(o.x + 1.0, o.y + 5.0),
            egui::pos2(o.x + 13.0, o.y + 13.0),
        ),
        1.0,
        highlight,
    );
}

impl FourthLayerApp {
    /// Mark the outliner visual_order as needing a rebuild on the next draw frame.
    /// Call whenever scene structure, selection, or visibility changes.
    #[inline]
    pub(crate) fn mark_outliner_dirty(&mut self) {
        self.outliner.outliner_dirty = true;
    }

    /// Edge auto-scroll during outliner drag-and-drop. While a prim or body drag is live
    /// and the pointer sits within `EDGE_PX` of the scroll viewport's top/bottom edge, feed
    /// a continuous per-frame scroll delta (speed ramps with edge proximity) so long trees
    /// can be traversed mid-drag. Call INSIDE the outliner ScrollArea closure — `ui.clip_rect()`
    /// there is the scroll viewport, and `scroll_with_delta` targets the enclosing area.
    /// Slot resolution is unaffected: rows are re-laid-out at their new offsets every frame
    /// and each row's hit test reads its fresh rect.
    pub(crate) fn outliner_drag_autoscroll(&self, ctx: &egui::Context, ui: &mut egui::Ui) {
        const EDGE_PX: f32 = 24.0;   // activation band at each edge
        const MIN_SPEED: f32 = 2.0;  // px/frame at the band's inner boundary
        const MAX_SPEED: f32 = 12.0; // px/frame at the very edge
        if self.outliner.dragging.is_none() && self.outliner.dragging_body.is_none() { return; }
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let clip = ui.clip_rect();
        if p.x < clip.left() || p.x > clip.right() { return; }
        let ramp = |t: f32| MIN_SPEED + t.clamp(0.0, 1.0) * (MAX_SPEED - MIN_SPEED);
        // Positive y-delta scrolls the view UP (toward earlier rows), matching wheel semantics.
        let dy = if p.y <= clip.top() + EDGE_PX {
            ramp((clip.top() + EDGE_PX - p.y) / EDGE_PX)
        } else if p.y >= clip.bottom() - EDGE_PX {
            -ramp((p.y - (clip.bottom() - EDGE_PX)) / EDGE_PX)
        } else {
            return;
        };
        ui.scroll_with_delta(egui::vec2(0.0, dy));
        // Continuous motion: keep frames coming while the pointer holds the edge band.
        ctx.request_repaint();
    }

    /// Scroll the outliner to reveal `rect` if it matches the pending scroll target.
    pub(crate) fn check_scroll_to(&mut self, ui: &mut egui::Ui, rect: egui::Rect, target: SceneNodeId) {
        if self.outliner.scroll_to_node == Some(target) {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
            self.outliner.scroll_to_node = None;
        }
    }

    /// Set up outliner rename state for a given scene node.
    pub(crate) fn trigger_rename(&mut self, target: SceneNodeId, name: &str) {
        self.outliner.renaming = Some(target);
        self.outliner.rename_buf = name.to_string();
        self.outliner.rename_grace_frames = 3;
    }

    /// Frame the selection: orbit/zoom camera to fit, then scroll the outliner.
    pub(crate) fn frame_selected(&mut self) {
        let center = self.selection_median();
        // Compute bounding radius from selected items
        let mut max_r: f32 = 0.5; // minimum radius so camera doesn't zoom to zero
        for uid in self.sel.usel.selected_prims().collect::<Vec<_>>() {
            if let Some(i) = self.prim_index_by_uid(uid) {
                if i >= self.scene.primitives.len() { continue; }
                let p = &self.scene.primitives[i];
                let d = (p.position - center).length();
                let s = p.scale.x.max(p.scale.y).max(p.scale.z);
                max_r = max_r.max(d + s);
            }
        }
        // Lights excluded here and resolved below: a Light node's `local_position` is an unused
        // default, so framing an outliner-selected light measured the distance from the light's
        // real position (the median) back to the world origin and zoomed way out.
        for nid in self.selected_transform_nodes() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                let d = (node.local_position - center).length();
                let s = node.local_scale.x.max(node.local_scale.y).max(node.local_scale.z);
                max_r = max_r.max(d + s);
            }
        }
        for lid in self.selected_light_ids() {
            if let Some(data) = self.scene.light_by_id(lid) {
                let d = (data.position - center).length();
                max_r = max_r.max(d + 1.0);
            }
        }
        // Fit camera distance so bounding sphere fills ~60% of viewport
        let half_fov = (self.render.fov_deg * 0.5).to_radians();
        let fit_dist = (max_r / half_fov.tan()).max(0.5);
        self.viewport.camera.target = center;
        self.viewport.camera.distance = fit_dist;
        self.mark_dirty(DirtyFlags::RENDER);
        // Also scroll outliner
        self.outliner_find_selected();
    }

    /// Ensure a body is visually expanded in the outliner. Dual-writes via `ol_ensure_expanded`.
    fn outliner_expand_body(&mut self, nid: core_math::scene_graph::NodeId) {
        self.ol_ensure_expanded(nid);
    }

    /// Focus the viewport on the selected item and scroll the outliner to reveal it.
    /// Expands any collapsed parent bodies/groups so the item is visible.
    pub(crate) fn outliner_find_selected(&mut self) {
        self.focus_selected();
        // Scroll outliner to show the selected item
        if let Some(li) = self.sel.usel.active_light() {
            self.outliner.scroll_to_node = Some(SceneNodeId::Light(li));
        } else if let Some(nid) = self.sel.usel.active_node() {
            // Expand parent bodies/groups (single helper — no dual-write at call sites)
            let mut current = nid;
            while let Some(parent_nid) = self.scene.scene_graph.get(current).and_then(|n| n.parent) {
                self.outliner_expand_body(parent_nid);
                current = parent_nid;
            }
            self.ol_ensure_expanded(nid);
            self.outliner.scroll_to_node = Some(SceneNodeId::Node(nid));
        } else if let Some(idx) = self.active_prim_index_unified() {
            if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                self.outliner_expand_body(body_nid);
                let mut current = body_nid;
                while let Some(parent_nid) = self.scene.scene_graph.get(current).and_then(|n| n.parent) {
                    self.outliner_expand_body(parent_nid);
                    current = parent_nid;
                }
                if let Some(uid) = self.scene.primitives.get(idx).map(|p| p.prim_uid) {
                    self.outliner.scroll_to_node = Some(SceneNodeId::Prim(uid));
                }
            }
        }
    }
}

// ---- Rename commit -----------------------------------------------------------

/// Set a scene node's display name, whatever its payload — the WRITE-side counterpart of
/// `NodePayload::display_name()`. Every nameable variant is covered here, once, so a new
/// payload type cannot silently swallow a rename (the per-site `match … _ => {}` arms each
/// dropped a different set: outliner_draw_poly.rs:73 drops SdfBody/VoxelBody/SdfObject).
/// `Root` stores no name and is deliberately not renameable.
fn set_payload_name(payload: &mut core_math::scene_graph::NodePayload, new_name: String) {
    use core_math::scene_graph::NodePayload as P;
    match payload {
        P::Root => {}
        P::Light(data) => data.name = new_name,
        P::Group { name }
        | P::SdfBody { name, .. }
        | P::ImportedMesh { name, .. }
        | P::VoxelBody { name, .. }
        | P::PolygonalBody { name, .. }
        | P::NurbsBody { name, .. }
        | P::NurbsSolid { name, .. }
        | P::Camera { name, .. }
        | P::Material { name, .. }
        | P::Empty { name }
        | P::SdfObject { name, .. } => *name = new_name,
    }
}

impl FourthLayerApp {
    /// Commit a rename for ANY outliner item — the ONE name-write path, keyed by the same
    /// `SceneNodeId` the rename STATE (`outliner.renaming`) uses. Self-transacting.
    /// Empty names and targets that vanished mid-rename are a no-op (`draw_rename_widget`
    /// already turns an emptied field into `Cancel`; this is the belt for a stale commit).
    ///
    /// Flat prim indices are resolved HERE, at execution time, via `prim_index_by_uid` —
    /// never stored (CLAUDE "Stable IDs, Not Indices").
    pub(crate) fn rename_scene_item(&mut self, target: SceneNodeId, new_name: String) {
        if new_name.is_empty() {
            return;
        }
        match target {
            SceneNodeId::Prim(uid) => {
                let Some(idx) = self.prim_index_by_uid(uid) else {
                    return;
                };
                self.transaction("Rename primitive", |s| {
                    s.write_prim(idx, |p| p.name = new_name);
                    s.outliner.outliner_dirty = true;
                });
            }
            SceneNodeId::Light(id) => {
                self.transaction("Rename light", |s| {
                    s.write_light(id, |l| l.name = new_name);
                    s.outliner.outliner_dirty = true;
                });
            }
            // A name is not GPU data and not structure: no dirty flag, no rebake.
            SceneNodeId::Body(nid) | SceneNodeId::Node(nid) => {
                self.transaction("Rename node", |s| {
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        set_payload_name(&mut node.payload, new_name);
                    }
                    s.outliner.outliner_dirty = true;
                });
            }
        }
    }

    /// Rename many outliner targets in ONE transaction (multi-select F2).
    /// First item gets `base`; others get `base.001`, `base.002`, …
    pub(crate) fn rename_scene_items_batch(&mut self, targets: &[(SceneNodeId, String)]) {
        if targets.is_empty() {
            return;
        }
        if targets.len() == 1 {
            self.rename_scene_item(targets[0].0, targets[0].1.clone());
            return;
        }
        let batch = targets.to_vec();
        self.transaction("Rename selection", |s| {
            for (target, new_name) in batch {
                if new_name.is_empty() {
                    continue;
                }
                match target {
                    SceneNodeId::Prim(uid) => {
                        if let Some(idx) = s.prim_index_by_uid(uid) {
                            s.write_prim(idx, |p| p.name = new_name);
                        }
                    }
                    SceneNodeId::Light(id) => {
                        s.write_light(id, |l| l.name = new_name);
                    }
                    SceneNodeId::Body(nid) | SceneNodeId::Node(nid) => {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            set_payload_name(&mut node.payload, new_name);
                        }
                    }
                }
            }
            s.outliner.outliner_dirty = true;
        });
    }
}

// ---- Solo / isolate (alt+click the eye) --------------------------------------
impl FourthLayerApp {
    /// Drive visibility across a node's SIBLING group: `solo` → only `nid` visible;
    /// `!solo` → every sibling visible. The shared half of solo/restore — the branch
    /// choice lives in the callers, so each of them does exactly one thing.
    fn apply_sibling_visibility(&mut self, nid: core_math::scene_graph::NodeId, solo: bool, desc: &str) {
        let parent = self.scene.scene_graph.get(nid)
            .and_then(|n| n.parent)
            .unwrap_or(self.scene.scene_graph.root());
        let siblings: Vec<core_math::scene_graph::NodeId> =
            self.scene.scene_graph.children(parent).to_vec();
        self.transaction(desc, |s| {
            for &other in &siblings {
                let vis = !solo || other == nid;
                if let Some(n) = s.scene.scene_graph.get_mut(other) { n.visible = vis; }
                s.mark_node_body_vol_dirty(other);
            }
            // Structural draw list filters visibility only on full rebuild — RENDER alone
            // leaves hidden siblings still drawn (owner 2026-08-05 eye/alt-click).
            s.mark_visibility_dirty();
            s.cache.prim_buf_stale = true;
            s.outliner.outliner_dirty = true;
        });
    }

    /// True when `nid` is the ONLY visible node in its sibling group — i.e. an earlier
    /// solo is still in force, so the next alt+click restores instead of re-soloing.
    fn is_sibling_solo(&self, nid: core_math::scene_graph::NodeId) -> bool {
        let parent = self.scene.scene_graph.get(nid)
            .and_then(|n| n.parent)
            .unwrap_or(self.scene.scene_graph.root());
        self.scene.scene_graph.children(parent).iter().all(|&other| {
            self.scene.scene_graph.get(other)
                .map(|n| n.visible == (other == nid))
                .unwrap_or(true)
        })
    }

    /// Sibling-scoped solo TOGGLE for a scene node (alt+click its eye): hide every sibling,
    /// or restore them all if `nid` is already soloed.
    pub(crate) fn solo_node(&mut self, nid: core_math::scene_graph::NodeId) {
        // The scene ROOT has no sibling group. Its parent lookup falls back to the root itself,
        // so its "siblings" resolve to its own CHILDREN and a solo there would hide every
        // top-level node while isolating nothing. Refuse it here — at the layer that owns the
        // sibling-group invariant — rather than expecting every caller to remember.
        if nid == self.scene.scene_graph.root() { return; }
        if self.is_sibling_solo(nid) {
            self.apply_sibling_visibility(nid, false, "Restore visibility");
        } else {
            self.apply_sibling_visibility(nid, true, "Solo");
        }
    }

    /// Show every node in `nid`'s sibling group (the eye menu's "Show all").
    pub(crate) fn show_all_siblings(&mut self, nid: core_math::scene_graph::NodeId) {
        self.apply_sibling_visibility(nid, false, "Show all");
    }

    /// Drive visibility across every prim of the body that OWNS prim `uid`: `solo` → only
    /// that prim visible; `!solo` → all visible. Prim visibility is a flat-cache + graph
    /// write, so it goes through `write_prim` (never a bare `primitives[i].visible =`).
    fn apply_body_prim_visibility(&mut self, uid: u32, solo: bool, desc: &str) {
        let Some(idx) = self.prim_index_by_uid(uid) else { return };
        let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) else { return };
        // Re-resolved inside the transaction from uids — see the loop below.
        let uids: Vec<u32> = self.body_prim_indices(body_nid).into_iter()
            .filter_map(|fi| self.scene.primitives.get(fi).map(|p| p.prim_uid))
            .collect();
        self.transaction(desc, |s| {
            for u in uids {
                let vis = !solo || u == uid;
                if let Some(fi) = s.prim_index_by_uid(u) {
                    s.write_prim(fi, |p| p.visible = vis);
                }
            }
            // Prim visibility also feeds SDF bake/raster — rebuild structural list.
            s.mark_visibility_dirty();
            s.outliner.outliner_dirty = true;
        });
    }

    /// True when prim `uid` is the ONLY visible prim in its body.
    fn is_body_prim_solo(&self, uid: u32) -> bool {
        let Some(idx) = self.prim_index_by_uid(uid) else { return false };
        let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) else { return false };
        self.body_prim_indices(body_nid).iter().all(|&fi| {
            self.scene.primitives.get(fi)
                .map(|p| p.visible == (fi == idx))
                .unwrap_or(true)
        })
    }

    /// Body-scoped solo TOGGLE for an SDF prim (alt+click its eye).
    pub(crate) fn solo_prim(&mut self, uid: u32) {
        if self.is_body_prim_solo(uid) {
            self.apply_body_prim_visibility(uid, false, "Restore prim visibility");
        } else {
            self.apply_body_prim_visibility(uid, true, "Solo prim");
        }
    }

    /// Show every prim in the body that owns `uid` (the eye menu's "Show all").
    pub(crate) fn show_all_body_prims(&mut self, uid: u32) {
        self.apply_body_prim_visibility(uid, false, "Show all prims");
    }
}

// ---- Body edit scope ---------------------------------------------------------
impl FourthLayerApp {
    /// Enter `nid`'s edit scope: it becomes the body that Shift+A / Ctrl+G / scope-leave
    /// rebake target. GATED on the payload actually being an SdfBody — `active_body_nid` is
    /// read as a body everywhere, so parking a light/camera NodeId in it would corrupt scope.
    ///
    /// The `transaction()` wrapper is NOT optional: `active_body_nid` is part of the undo
    /// entry (snapshot_undo captures it, undo restores it), so a bare assignment desyncs undo.
    pub(crate) fn enter_body_scope(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        ) { return; }
        // Already there (scope + sole selection) → no empty undo entry.
        let item = SceneItemId::Node(nid);
        let already = self.scene.active_body_nid == nid
            && self.sel.usel.active == Some(item)
            && self.sel.usel.selected.len() == 1
            && self.sel.usel.selected.contains(&item);
        if already { return; }
        self.transaction("Enter body scope", |s| {
            s.clear_selection_unified();
            s.select_single_unified(item);
            s.scene.active_body_nid = nid;
        });
    }
}

// ---- Multi-body drag payload (batch-2 item 1) --------------------------------
impl FourthLayerApp {
    /// True if a node participates in outliner hierarchy drag (nest / reorder / un-nest).
    /// Includes poly + imported mesh so mesh objects can parent by drag-drop like SDF bodies.
    pub(crate) fn is_body_drag_payload(payload: &core_math::scene_graph::NodePayload) -> bool {
        use core_math::scene_graph::NodePayload;
        matches!(
            payload,
            NodePayload::SdfBody { .. }
                | NodePayload::Group { .. }
                | NodePayload::SdfObject { .. }
                | NodePayload::PolygonalBody { .. }
                | NodePayload::ImportedMesh { .. }
        )
    }

    /// True if the node is nested under a non-Root parent (kitbash Group, SdfObject, or
    /// SdfBody sub-nest). Used for empty-area "un-nest to root" — `parent_body` alone
    /// misses bodies whose only parent is a Group.
    pub(crate) fn is_outliner_nested_node(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> bool {
        let root = self.scene.scene_graph.root();
        match self.scene.scene_graph.get(nid).and_then(|n| n.parent) {
            Some(p) if p != root => true,
            _ => false,
        }
    }

    /// The FULL set a body/kitbash drag carries. If the grabbed header is part of the
    /// current multi-selection, the drag moves every selected top-most SdfBody / Group /
    /// SdfObject (ancestor filter via `top_most_nodes`), ordered by outliner visual order.
    /// An unselected header drag never hijacks the selection (just the grabbed node).
    pub(crate) fn dragged_body_set(
        &self,
        grabbed: core_math::scene_graph::NodeId,
    ) -> Vec<core_math::scene_graph::NodeId> {
        if !self.sel.usel.selected.contains(&SceneItemId::Node(grabbed)) {
            return vec![grabbed];
        }
        let sel_nodes: Vec<core_math::scene_graph::NodeId> = self
            .sel
            .usel
            .selected_nodes()
            .filter(|&n| {
                self.scene
                    .scene_graph
                    .get(n)
                    .map(|x| Self::is_body_drag_payload(&x.payload))
                    .unwrap_or(false)
            })
            .collect();
        let mut tops = crate::scene_actions::top_most_nodes(&self.scene.scene_graph, &sel_nodes);
        if tops.is_empty() {
            return vec![grabbed];
        }
        let vo = &self.outliner.visual_order;
        tops.sort_by_key(|&n| {
            vo.iter()
                .position(|&id| id == SceneItemId::Node(n))
                .unwrap_or(usize::MAX)
        });
        tops
    }

    /// Shared body-drag hit-test for one nestable header (SdfBody / Group / SdfObject).
    /// Writes `body_drop_*` + `drop_visual` when the pointer is in this row's band.
    /// Last matching row in draw order wins; adjacent bands are half-open / disjoint.
    pub(crate) fn update_body_drop_over_header(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        header_rect: egui::Rect,
        target_nid: core_math::scene_graph::NodeId,
        target_name: &str,
    ) {
        let Some(dragging_nid) = self.outliner.dragging_body else {
            return;
        };
        let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) else {
            return;
        };
        use crate::outliner_drop_slots as slots;
        let clip = ui.clip_rect();
        let band = slots::row_band(
            header_rect.top(),
            header_rect.bottom(),
            ui.spacing().item_spacing.y,
        );
        if pointer.x < clip.left()
            || pointer.x > clip.right()
            || !slots::band_contains(band, pointer.y)
        {
            return;
        }
        // Three zones: top 30% = place BEFORE, bottom 30% = place AFTER,
        // CENTER 40% = NEST into target. Cycle guard: no slot if target is inside the drag set.
        let zone = slots::classify_zone(
            header_rect.top(),
            header_rect.bottom(),
            pointer.y,
            0.30,
        );
        let dragged_set = self.dragged_body_set(dragging_nid);
        let target_inside_dragged = dragged_set
            .iter()
            .any(|&d| self.scene.scene_graph.is_descendant_of(target_nid, d));
        let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        match slots::resolve_body_over_body_header(
            &dragged_set,
            target_nid,
            target_inside_dragged,
            zone,
        ) {
            slots::BodyOverBodyHeader::PlaceNextTo { before } => {
                self.outliner.body_drop_target_nid = Some(target_nid);
                let (y, action) = if before {
                    (header_rect.top(), slots::SlotAction::Before(target_name))
                } else {
                    (header_rect.bottom(), slots::SlotAction::After(target_name))
                };
                self.outliner.body_drop_target_y = Some(y);
                self.outliner.body_drop_before = before;
                self.outliner.body_drop_nest = false;
                self.outliner.drop_visual = Some(app_types::DropVisual {
                    kind: app_types::DropVisualKind::Line,
                    x_left: header_rect.left(),
                    y,
                    height: 0.0,
                    label: slots::slot_label(action, copy),
                    show_hint: false,
                });
            }
            slots::BodyOverBodyHeader::NestInto => {
                self.outliner.body_drop_target_nid = Some(target_nid);
                self.outliner.body_drop_target_y = Some(header_rect.center().y);
                self.outliner.body_drop_before = false;
                self.outliner.body_drop_nest = true;
                self.outliner.drop_visual = Some(app_types::DropVisual {
                    kind: app_types::DropVisualKind::Highlight,
                    x_left: header_rect.left(),
                    y: header_rect.top(),
                    height: header_rect.height(),
                    label: slots::slot_label(slots::SlotAction::Into(target_name), copy),
                    show_hint: false,
                });
            }
            slots::BodyOverBodyHeader::Rejected => {}
        }
    }

    /// Begin a body/kitbash drag from a row header (shared by SdfBody + Group + SdfObject).
    pub(crate) fn begin_body_drag_from_header(
        &mut self,
        ctx: &egui::Context,
        header_rect: egui::Rect,
        child_nid: core_math::scene_graph::NodeId,
    ) {
        let ptr_pos = ctx
            .input(|inp| inp.pointer.hover_pos())
            .unwrap_or_default();
        if header_rect.contains(ptr_pos)
            && ctx.input(|inp| inp.pointer.button_pressed(egui::PointerButton::Primary))
            && self.outliner.renaming.is_none()
            && self.outliner.dragging.is_none()
        {
            self.outliner.body_drag_start_pos = Some(ptr_pos);
            self.outliner.body_drag_source = Some(child_nid);
        }
        if let Some(start_pos) = self.outliner.body_drag_start_pos {
            if self.outliner.body_drag_source == Some(child_nid) {
                if let Some(current_pos) = ctx.input(|inp| inp.pointer.hover_pos()) {
                    let delta = (current_pos - start_pos).length();
                    if delta > 6.0 && self.outliner.dragging_body.is_none() {
                        self.outliner.dragging_body = Some(child_nid);
                    }
                }
                if ctx.input(|inp| inp.pointer.button_released(egui::PointerButton::Primary)) {
                    if self.outliner.body_drag_source == Some(child_nid) {
                        self.outliner.body_drag_start_pos = None;
                        self.outliner.body_drag_source = None;
                    }
                }
            }
        }
    }

    /// Clear all body-drag drop state (cancel / release).
    pub(crate) fn clear_body_drag_state(&mut self) {
        self.outliner.dragging_body = None;
        self.outliner.body_drop_target_nid = None;
        self.outliner.body_drop_target_y = None;
        self.outliner.body_drop_before = false;
        self.outliner.body_drop_nest = false;
        self.outliner.body_drag_start_pos = None;
        self.outliner.body_drag_source = None;
        self.outliner.body_drop_root_y = None;
    }
}
