//! Outliner drag-and-drop — drop indicators, ghost labels, release handling.

use eframe::egui;
use crate::*;
use crate::outliner_panel::OutlinerDeferred;

/// Hover-to-expand dwell while dragging: how long the pointer must rest on a
/// COLLAPSED body row before it auto-expands so the drop can land inside.
/// Shared by both shells (v1 `outliner_draw_body.rs`, ui2 `ui2/outliner/dnd.rs`).
pub(crate) const HOVER_EXPAND_SECS: f64 = 0.6;

impl FourthLayerApp {
    /// Paint drop indicators, ghost labels, and handle drag release for both
    /// prim drags and body drags. Called once per frame after the tree draw.
    pub(crate) fn outliner_dnd_release(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        d: &mut OutlinerDeferred,
    ) {
        // Mutual exclusion — priority: drop_insert (child_order slot) > drop_reparent (drop INTO
        // a body) > drop_target (flat reorder). The zones are disjoint by construction (the pointer
        // is in exactly one row zone), so these clears are defence-in-depth only.
        if self.outliner.drop_insert.is_some() {
            self.outliner.drop_target = None;
            self.outliner.drop_reparent = None;
            self.outliner.drop_on_prim = false;
        } else if self.outliner.drop_reparent.is_some() {
            // A body-row highlight is drawn in outliner_draw_body.rs when drop_reparent is set; an
            // insertion line must NEVER co-exist with it, so clear the reorder line here.
            self.outliner.drop_target = None;
            self.outliner.drop_target_y = None;
        }

        // Hover-to-expand timer upkeep: reset when no collapsed header saw the pointer this
        // frame (the flag is set by the header hit test) or when no drag is live at all.
        let no_drag = self.outliner.dragging.is_none() && self.outliner.dragging_body.is_none();
        if no_drag || !std::mem::take(&mut self.outliner.hover_expand_hit) {
            self.outliner.hover_expand = None;
        }

        // ROOT / empty-area drop detection. This runs AFTER every row + body header has been drawn,
        // so all per-row drop states are final. If a prim drag is live but resolved to NO in-tree
        // target (not over a prim row, not over a body header/drop-into), and the pointer is inside
        // the outliner content, treat it as a ROOT-level insertion → on release we spawn a new
        // top-level SdfBody around the dragged prim(s). A single insertion line is the ONLY feedback.
        if self.outliner.dragging.is_some()
            && self.outliner.drop_target.is_none()
            && self.outliner.drop_reparent.is_none()
            && self.outliner.drop_insert.is_none()
            && self.outliner.drop_root_y.is_none()
        {
            let content_rect = ui.min_rect();
            if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                if content_rect.contains(pointer) {
                    // Snap the line to just below the last drawn content so it reads as "at root, end".
                    let y = pointer.y.min(content_rect.bottom() - 2.0).max(content_rect.top() + 2.0);
                    self.outliner.drop_root_y = Some(y);
                    let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                    self.outliner.drop_visual = Some(app_types::DropVisual {
                        kind: app_types::DropVisualKind::Line,
                        x_left: content_rect.left() + 6.0,
                        y,
                        height: 0.0,
                        label: crate::outliner_drop_slots::slot_label(
                            crate::outliner_drop_slots::SlotAction::NewBody, copy),
                        show_hint: true,
                    });
                }
            }
        }

        // ROOT / empty-area drop detection for a BODY drag: a nested body/group dragged to empty
        // outliner space (no body-row target) un-nests to the graph root on release. Only offered
        // when at least one dragged node has a non-Root parent (Group kitbash nest OR SdfBody
        // boolean nest — parent_body alone misses Group-parented bodies).
        if let Some(dragging_nid) = self.outliner.dragging_body {
            if self.outliner.body_drop_target_nid.is_none()
                && self.dragged_body_set(dragging_nid).iter()
                    .any(|&d| self.is_outliner_nested_node(d))
            {
                let content_rect = ui.min_rect();
                if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                    if content_rect.contains(pointer) {
                        let y = pointer.y.min(content_rect.bottom() - 2.0).max(content_rect.top() + 2.0);
                        self.outliner.body_drop_root_y = Some(y);
                        let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                        self.outliner.drop_visual = Some(app_types::DropVisual {
                            kind: app_types::DropVisualKind::Line,
                            x_left: content_rect.left() + 6.0,
                            y,
                            height: 0.0,
                            label: crate::outliner_drop_slots::slot_label(
                                crate::outliner_drop_slots::SlotAction::ToRoot, copy),
                            show_hint: true,
                        });
                    }
                }
            }
        }

        // UNIFIED indicator painter — renders exactly what the resolved slot said (the
        // DropVisual built at hit-test time). Blue = prim drag, amber = body drag (existing
        // accents). Line = BETWEEN slot: horizontal line starting at the RECEIVING container's
        // indent with a filled circle at its left end (Blender style — the indent is the depth
        // disambiguator). Highlight = drop-INTO: rounded rect over the whole target row.
        if self.outliner.dragging.is_some() || self.outliner.dragging_body.is_some() {
            if let Some(v) = self.outliner.drop_visual.clone() {
                let panel_rect = ui.min_rect();
                let body_drag = self.outliner.dragging_body.is_some();
                let accent = if body_drag {
                    egui::Color32::from_rgb(212, 148, 58)
                } else {
                    egui::Color32::from_rgb(77, 159, 255)
                };
                match v.kind {
                    app_types::DropVisualKind::Line => {
                        let x0 = v.x_left.max(panel_rect.left() + 4.0);
                        let radius = 3.0;
                        let a = egui::pos2(x0 + radius, v.y);
                        let b = egui::pos2(panel_rect.right() - 4.0, v.y);
                        ui.painter().line_segment([a, b], egui::Stroke::new(2.0, accent));
                        ui.painter().circle_filled(egui::pos2(x0, v.y), radius, accent);
                        if v.show_hint {
                            // Faint inline hint (root / new-body zone): the user reads the
                            // outcome on the line itself, not only on the ghost.
                            ui.painter().text(
                                egui::pos2(b.x - 4.0, v.y - 3.0),
                                egui::Align2::RIGHT_BOTTOM,
                                &v.label,
                                egui::FontId::proportional(10.0),
                                accent.gamma_multiply(0.65),
                            );
                        }
                    }
                    app_types::DropVisualKind::Highlight => {
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(v.x_left.max(panel_rect.left() + 4.0), v.y),
                            egui::pos2(panel_rect.right() - 4.0, v.y + v.height),
                        );
                        let fill = if body_drag {
                            egui::Color32::from_rgba_unmultiplied(212, 148, 58, 34)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(77, 159, 255, 28)
                        };
                        ui.painter().rect_filled(rect, 3.0, fill);
                        ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Middle);
                    }
                }
            }
        }

        // Ghost label near cursor during drag
        if let Some(drag_idx) = self.outliner.dragging {
            if drag_idx < self.scene.primitives.len() {
                if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                    let count = self.selected_prim_indices_unified().len();
                    let mut ghost_text = if count > 1 {
                        format!("{} items", count)
                    } else {
                        let name = &self.scene.primitives[drag_idx].name;
                        let icon = self.scene.primitives[drag_idx].shape.icon();
                        format!("{} {}", icon, name)
                    };
                    // Ctrl held = the drop will place a COPY (read again at release time).
                    if ctx.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                        ghost_text.push_str(" +copy");
                    }
                    // LIVE action verb: exactly what a release will do at this pixel,
                    // straight from the resolved slot ("Move after Box.003", "New body", …).
                    if let Some(v) = &self.outliner.drop_visual {
                        ghost_text.push_str(&format!("  \u{2192} {}", v.label));
                    }
                    let ghost_pos = egui::pos2(pointer.x + 12.0, pointer.y - 8.0);
                    let painter = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Tooltip,
                        egui::Id::new("dnd_ghost"),
                    ));
                    painter.text(
                        ghost_pos, egui::Align2::LEFT_BOTTOM,
                        ghost_text,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgba_unmultiplied(255, 200, 100, 200),
                    );
                }
            }
        }

        // Ghost label near cursor during body drag
        if let Some(dragging_nid) = self.outliner.dragging_body {
            if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                // Multi-body drag ghost: "N bodies" when the payload is a selected set.
                let set_len = self.dragged_body_set(dragging_nid).len();
                let mut ghost_text = if set_len > 1 {
                    format!("\u{2B21} {} bodies", set_len)
                } else {
                    let body_name = self.scene.scene_graph.get(dragging_nid)
                        .map(|n| n.payload.display_name().to_string())
                        .unwrap_or_else(|| "Body".into());
                    format!("\u{2B21} {}", body_name)
                };
                // Ctrl held = the drop will place COPIES (read again at release time).
                if ctx.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                    ghost_text.push_str(" +copy");
                }
                // LIVE action verb from the resolved slot ("Move into Body.002", …).
                if let Some(v) = &self.outliner.drop_visual {
                    ghost_text.push_str(&format!("  \u{2192} {}", v.label));
                }
                let ghost_pos = egui::pos2(pointer.x + 12.0, pointer.y - 8.0);
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("dnd_ghost"),
                ));
                painter.text(
                    ghost_pos, egui::Align2::LEFT_BOTTOM,
                    ghost_text,
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_rgba_unmultiplied(122, 150, 171, 200),
                );
            }
        }

        // Handle body drag release → reorder bodies
        if self.outliner.dragging_body.is_some() {
            let cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || !ctx.input(|i| i.pointer.has_pointer());
            if cancel {
                self.clear_body_drag_state();
            } else if ctx.input(|i| i.pointer.any_released()) {
                // Ctrl at DROP time = duplicate-instead-of-move (standard DCC: the user can
                // decide mid-drag). No conflict with Ctrl-click toggle-select: this branch only
                // runs when a drag is live (movement past the 6px threshold), and a plain
                // Ctrl-click never sets `dragging_body`.
                let ctrl_copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if let Some(grabbed) = self.outliner.dragging_body {
                    // Multi-body payload: every selected top-most body when the grabbed header is
                    // selected (visual order), else just the grabbed body.
                    let dragged = self.dragged_body_set(grabbed);
                    if let Some(target_nid) = self.outliner.body_drop_target_nid {
                        // Cycle guard re-checked at RELEASE for the WHOLE set (the hover-level
                        // Rejected already suppressed the indicator; state may have changed).
                        let cycle = dragged.contains(&target_nid) || dragged.iter()
                            .any(|&b| self.scene.scene_graph.is_descendant_of(target_nid, b));
                        if !cycle && self.outliner.body_drop_nest {
                            // CENTER drop → NEST ALL dragged top-most bodies into the target
                            // (each via reparent_body_node — body_op preserved, full Structural
                            // Mutation Checklist for both parents), ONE transaction. Ctrl = nest
                            // fresh COPIES (duplicate_node_no_undo), originals untouched.
                            // Targets: SdfBody (boolean nest), Group, SdfObject, poly / imported mesh.
                            let target_label = match self.scene.scene_graph.get(target_nid)
                                .map(|n| &n.payload)
                            {
                                Some(core_math::scene_graph::NodePayload::SdfBody { .. }) => Some("body"),
                                Some(core_math::scene_graph::NodePayload::Group { .. }) => Some("group"),
                                Some(core_math::scene_graph::NodePayload::SdfObject { .. }) => Some("object"),
                                Some(core_math::scene_graph::NodePayload::PolygonalBody { .. }) => {
                                    Some("mesh")
                                }
                                Some(core_math::scene_graph::NodePayload::ImportedMesh { .. }) => {
                                    Some("mesh")
                                }
                                _ => None,
                            };
                            // Filter no-op re-nests (already a direct child) — copies always apply.
                            let to_nest: Vec<_> = if ctrl_copy { dragged.clone() } else {
                                dragged.iter().copied()
                                    .filter(|&b| self.scene.scene_graph.get(b)
                                        .and_then(|n| n.parent) != Some(target_nid))
                                    .collect()
                            };
                            if let Some(label) = target_label {
                                if !to_nest.is_empty() {
                                use crate::scene_actions::ReparentBodyResult;
                                // Pre-check (non-copy) so a fully-illegal drop does not push empty undo.
                                if !ctrl_copy {
                                    let mut any_ok = false;
                                    let mut any_forbidden = false;
                                    for &b in &to_nest {
                                        match self.reparent_body_check(b, target_nid) {
                                            ReparentBodyResult::Applied => any_ok = true,
                                            ReparentBodyResult::ForbiddenGroupUnderBody => {
                                                any_forbidden = true
                                            }
                                            ReparentBodyResult::Skipped => {}
                                        }
                                    }
                                    if !any_ok {
                                        if any_forbidden {
                                            self.toast(Self::REPARENT_GROUP_UNDER_BODY_MSG);
                                        }
                                        // else all Skipped — silent
                                    } else {
                                        let desc = format!("Nest into {label}");
                                        let n = to_nest.len();
                                        self.transaction(&desc, |s| {
                                            let mut new_ids: Vec<SceneItemId> =
                                                Vec::with_capacity(n);
                                            let mut applied = 0usize;
                                            let mut forbidden = 0usize;
                                            for &b in &to_nest {
                                                match s.reparent_body_node(b, target_nid) {
                                                    ReparentBodyResult::Applied => {
                                                        applied += 1;
                                                        new_ids.push(SceneItemId::Node(b));
                                                    }
                                                    ReparentBodyResult::ForbiddenGroupUnderBody => {
                                                        forbidden += 1
                                                    }
                                                    ReparentBodyResult::Skipped => {}
                                                }
                                            }
                                            if applied > 0 {
                                                s.ol_set_expanded(target_nid, true);
                                                s.clear_selection_unified();
                                                s.select_after_create(&new_ids);
                                                let unit =
                                                    if applied == 1 { "item" } else { "items" };
                                                s.toast(&format!(
                                                    "Nested {applied} {unit} into {label}"
                                                ));
                                            }
                                            if forbidden > 0 && applied > 0 {
                                                s.toast(&format!(
                                                    "{forbidden} skipped — Groups can't nest under SDF bodies"
                                                ));
                                            }
                                        });
                                    }
                                } else {
                                    let desc = format!("Duplicate to {label}");
                                    let n = to_nest.len();
                                    self.transaction(&desc, |s| {
                                        let mut new_ids: Vec<SceneItemId> =
                                            Vec::with_capacity(n);
                                        let mut applied = 0usize;
                                        let mut forbidden = 0usize;
                                        for &b in &to_nest {
                                            let Some(subject) = s.duplicate_node_no_undo(b) else {
                                                continue;
                                            };
                                            match s.reparent_body_node(subject, target_nid) {
                                                ReparentBodyResult::Applied => {
                                                    applied += 1;
                                                    new_ids.push(SceneItemId::Node(subject));
                                                }
                                                ReparentBodyResult::ForbiddenGroupUnderBody => {
                                                    forbidden += 1
                                                }
                                                ReparentBodyResult::Skipped => {}
                                            }
                                        }
                                        if applied > 0 {
                                            s.clear_selection_unified();
                                            s.select_after_create(&new_ids);
                                            let unit =
                                                if applied == 1 { "item" } else { "items" };
                                            s.toast(&format!(
                                                "Duplicated {applied} {unit} into {label}"
                                            ));
                                        } else if forbidden > 0 {
                                            s.toast(Self::REPARENT_GROUP_UNDER_BODY_MSG);
                                        }
                                    });
                                }
                                }
                            }
                        } else if !cycle {
                            // Edge zones (before/after the target row). Same parent = plain reorder.
                            // DIFFERENT parent = move the dragged bodies NEXT TO the target: reparent
                            // to the target's parent first (un-nest a sub-body between top-level rows,
                            // or re-nest between a group's children), then place the whole block
                            // contiguously at the slot (anchor semantics — mirrors multi-prim drops).
                            // Ctrl = place fresh COPIES, originals untouched.
                            let target_parent = self.scene.scene_graph.get(target_nid).and_then(|n| n.parent);
                            if let Some(tp) = target_parent {
                                let parent_cycle = dragged.contains(&tp) || dragged.iter()
                                    .any(|&b| self.scene.scene_graph.is_descendant_of(tp, b));
                                if !parent_cycle {
                                    let drop_before = self.outliner.body_drop_before;
                                    // Destination parent an SdfBody? Sub-body order lives in the unified
                                    // `child_order` (interleaved with prims) — route through the ONE
                                    // reorder op. Root/SdfObject parents use plain node-edge reorder.
                                    let parent_is_body = matches!(
                                        self.scene.scene_graph.get(tp).map(|n| &n.payload),
                                        Some(core_math::scene_graph::NodePayload::SdfBody { .. })
                                    );
                                    let cross_parent = dragged.iter().any(|&b|
                                        self.scene.scene_graph.get(b).and_then(|n| n.parent) != Some(tp));
                                    let desc = if ctrl_copy { "Duplicate body" }
                                               else if cross_parent { "Move body" } else { "Reorder body" };
                                    use crate::outliner_drop_slots::{slot_anchor, slot_insert_position, slot_is_noop};
                                    // Same-parent single-body drop at its current slot → no empty undo.
                                    let body_slot_noop = !ctrl_copy && !cross_parent && dragged.len() == 1 && parent_is_body && {
                                        use core_math::scene_graph::BodyChildRef;
                                        let order = self.scene.scene_graph.body_child_order(tp);
                                        let raw = order.iter()
                                            .position(|&r| r == BodyChildRef::Body(target_nid))
                                            .map(|p| if drop_before { p } else { p + 1 })
                                            .unwrap_or(order.len());
                                        slot_is_noop(&order, BodyChildRef::Body(dragged[0]), raw)
                                    };
                                    let node_slot_noop = !ctrl_copy && !cross_parent && dragged.len() == 1 && !parent_is_body && {
                                        let children = self.scene.scene_graph.children(tp).to_vec();
                                        let raw = children.iter().position(|&c| c == target_nid)
                                            .map(|p| if drop_before { p } else { p + 1 })
                                            .unwrap_or(children.len());
                                        children.get(raw) == Some(&dragged[0])
                                            || (raw > 0 && children.get(raw - 1) == Some(&dragged[0]))
                                    };
                                    if !(body_slot_noop || node_slot_noop) {
                                    self.transaction(desc, |s| {
                                        let subjects: Vec<core_math::scene_graph::NodeId> = if ctrl_copy {
                                            dragged.iter().filter_map(|&b| s.duplicate_node_no_undo(b)).collect()
                                        } else { dragged.clone() };
                                        if subjects.is_empty() { return; }
                                        if parent_is_body {
                                            use core_math::scene_graph::BodyChildRef;
                                            // Anchor BEFORE any move: first ref at/after the slot not
                                            // itself being moved — keeps the block contiguous while
                                            // earlier moves shift the order under us.
                                            let order = s.scene.scene_graph.body_child_order(tp);
                                            let moved: Vec<BodyChildRef> =
                                                subjects.iter().map(|&b| BodyChildRef::Body(b)).collect();
                                            let raw = order.iter()
                                                .position(|&r| r == BodyChildRef::Body(target_nid))
                                                .map(|p| if drop_before { p } else { p + 1 })
                                                .unwrap_or(order.len());
                                            let anchor = slot_anchor(&order, raw, &moved);
                                            for &b in &subjects {
                                                if s.scene.scene_graph.get(b).and_then(|n| n.parent) != Some(tp) {
                                                    let _ = s.reparent_body_node(b, tp);
                                                }
                                                let order = s.scene.scene_graph.body_child_order(tp);
                                                let dref = BodyChildRef::Body(b);
                                                let pos = slot_insert_position(&order, dref, anchor);
                                                s.reorder_child_in_body(tp, dref, pos);
                                            }
                                        } else {
                                            // Node-edge reorder (top-level bodies): same anchored block
                                            // placement, generic slot math over NodeId.
                                            let children = s.scene.scene_graph.children(tp).to_vec();
                                            let raw = children.iter().position(|&c| c == target_nid)
                                                .map(|p| if drop_before { p } else { p + 1 })
                                                .unwrap_or(children.len());
                                            let anchor = slot_anchor(&children, raw, &subjects);
                                            for &b in &subjects {
                                                if s.scene.scene_graph.get(b).and_then(|n| n.parent) != Some(tp) {
                                                    let _ = s.reparent_body_node(b, tp);
                                                }
                                                let children = s.scene.scene_graph.children(tp).to_vec();
                                                let pos = slot_insert_position(&children, b, anchor);
                                                s.scene.scene_graph.reorder_child(tp, b, pos);
                                            }
                                            s.sync_flat_cache();
                                            // Top-level reorder (parent = Root/SdfObject): independent
                                            // bodies, no shared boolean chain, but mark for parity.
                                            s.mark_node_body_vol_dirty(tp);
                                            s.mark_dirty(DirtyFlags::SCENE);
                                            s.mark_outliner_dirty();
                                        }
                                        let ids: Vec<SceneItemId> =
                                            subjects.iter().map(|&b| SceneItemId::Node(b)).collect();
                                        s.clear_selection_unified();
                                        s.select_after_create(&ids);
                                        if ctrl_copy { s.toast(&format!("Duplicated {}", ids.len())); }
                                    });
                                    } // !body/node_slot_noop
                                }
                            }
                        }
                    } else if self.outliner.body_drop_root_y.is_some() {
                        // Empty-area drop → un-nest every non-Root-parented dragged node to root
                        // (kitbash Group children + boolean sub-bodies). Already-at-root members
                        // are skipped. Ctrl = un-nest fresh COPIES to root, originals untouched.
                        let nested: Vec<_> = dragged.iter().copied()
                            .filter(|&b| self.is_outliner_nested_node(b))
                            .collect();
                        if !nested.is_empty() {
                            let root = self.scene.scene_graph.root();
                            let desc = if ctrl_copy { "Duplicate to top level" } else { "Un-nest to top level" };
                            let n = nested.len();
                            self.transaction(desc, |s| {
                                let mut new_ids: Vec<SceneItemId> = Vec::with_capacity(n);
                                for &b in &nested {
                                    let subject = if ctrl_copy {
                                        match s.duplicate_node_no_undo(b) { Some(c) => c, None => continue }
                                    } else { b };
                                    let _ = s.reparent_body_node(subject, root);
                                    new_ids.push(SceneItemId::Node(subject));
                                }
                                s.clear_selection_unified();
                                s.select_after_create(&new_ids);
                                let unit = if n == 1 { "item" } else { "items" };
                                s.toast(&if ctrl_copy {
                                    format!("Duplicated {n} {unit} to top level")
                                } else if n == 1 {
                                    "Un-nested to top level".to_string()
                                } else {
                                    format!("Un-nested {n} {unit} to top level")
                                });
                            });
                        }
                    }
                }
                self.clear_body_drag_state();
            }
        }

        // Handle prim drag release → reorder/reparent (or cancel with Escape / pointer-leave)
        if self.outliner.dragging.is_some() {
            let cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || !ctx.input(|i| i.pointer.has_pointer());
            if cancel {
                self.outliner.dragging = None;
                self.outliner.drop_target = None;
                self.outliner.drop_target_y = None;
                self.outliner.drop_reparent = None;
                self.outliner.drop_on_prim = false;
                self.outliner.drop_root_y = None;
                self.outliner.drop_insert = None;
                self.outliner.drag_start_pos = None;
                self.outliner.drag_source = None;
            } else if ctx.input(|i| i.pointer.any_released()) {
                // Ctrl at DROP time = the drop places fresh COPIES (duplicate_prims_no_undo)
                // instead of moving the originals — originals untouched + deselected, copies
                // selected, ONE transaction. Read at release (user can decide mid-drag). No
                // conflict with Ctrl-click toggle-select: `dragging` only exists after the
                // 4px movement threshold, so a plain Ctrl-click never reaches this branch.
                let ctrl_copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if let Some(from) = self.outliner.dragging {
                    // CHILD_ORDER slot drop (before/after a sub-body row, end-of-body strip):
                    // move the selected prims into `tbody`'s child_order at `idx`. Same body →
                    // reorder in the boolean chain; different body (e.g. dragged OUT of a sub-body
                    // onto a slot in the PARENT body's stack) → reparent + place at the slot.
                    // Takes priority over drop_target/drop_reparent (see mutual exclusion above).
                    if let Some((tbody, idx)) = self.outliner.drop_insert {
                        use core_math::scene_graph::BodyChildRef;
                        let mut sel: Vec<usize> = self.selected_prim_indices_unified();
                        sel.sort();
                        if sel.is_empty() { sel.push(from); }
                        // Locked prims never move (same policy as the drop-on-header path).
                        let prim_uids: Vec<u32> = sel.iter()
                            .filter_map(|&fi| self.scene.primitives.get(fi))
                            .filter(|p| !p.locked)
                            .map(|p| p.prim_uid)
                            .collect();
                        if prim_uids.len() < sel.len() {
                            self.toast("Locked primitive(s) not moved");
                        }
                        // No-op guard: a single prim dropped right where it already sits.
                        // (A Ctrl-copy at the same slot is a real duplicate, never a no-op.)
                        let noop = !ctrl_copy && prim_uids.len() == 1 && {
                            let same_body = self.scene.prim_body_map.get(from).map(|(nid, _)| *nid) == Some(tbody);
                            let order = self.scene.scene_graph.body_child_order(tbody);
                            same_body && crate::outliner_drop_slots::slot_is_noop(&order, BodyChildRef::Prim(prim_uids[0]), idx)
                        };
                        if !prim_uids.is_empty() && !noop {
                            let desc = if ctrl_copy { "Duplicate in stack" } else { "Move in stack" };
                            self.transaction(desc, |s| {
                                let act_uids = if ctrl_copy { s.duplicate_prims_no_undo(&prim_uids) }
                                               else { prim_uids.clone() };
                                s.move_prims_to_body_slot(&act_uids, tbody, idx);
                                let ids: Vec<SceneItemId> = act_uids.iter().map(|uid| SceneItemId::Prim(*uid)).collect();
                                s.clear_selection_unified();
                                s.select_after_create(&ids);
                            });
                        }
                    } else if let Some(to) = self.outliner.drop_target {
                        // drop_target is set ONLY for the center "group with prim" zone
                        // (drop_on_prim). All between-row slots — same-body reorder, cross-body
                        // move, end-of-body — resolve to drop_insert (child_order slot) above:
                        // ONE canonical apply path (move_prims_to_body_slot), never flat indices.
                        if self.outliner.drop_on_prim && from != to {
                            // Drop ON a prim — group all selected + drop target. The DROP TARGET prim is
                            // the boolean reference: the new group inherits its op and the target resets
                            // to Union inside the group (no double-negative — see group_selected_into_body).
                            // B3: NO transaction here — group_selected_into_body opens its OWN transaction
                            // (prim_actions.rs), so snapshotting here made TWO undo entries for one drag-to-group
                            // (the first Ctrl+Z reverted the group, a second was needed to clear the stale entry).
                            let target_uid = self.scene.primitives.get(to).map(|p| p.prim_uid);
                            // INTENTIONAL: add drop target to selection before group_selected_into_body()
                            if let Some(uid) = target_uid {
                                self.sel.usel.selected.insert(SceneItemId::Prim(uid));
                            }
                            self.group_selected_into_body(target_uid);
                        }
                    }
                    // Handle drop on body header (drop_reparent)
                    if let Some(SceneNodeId::Body(target_nid)) = self.outliner.drop_reparent {
                        let mut selected_sorted: Vec<usize> = self.selected_prim_indices_unified();
                        selected_sorted.sort();
                        // Ctrl-copy: same-body prims are legit too (copy INTO the same body).
                        let to_move: Vec<usize> = selected_sorted.iter()
                            .filter(|&&fi| ctrl_copy
                                || self.scene.prim_body_map.get(fi).map(|(nid, _)| *nid) != Some(target_nid))
                            .copied().collect();
                        if !to_move.is_empty() {
                            let prim_uids: Vec<u32> = to_move.iter()
                                .filter_map(|&fi| self.scene.primitives.get(fi).map(|p| p.prim_uid))
                                .collect();
                            let desc = if ctrl_copy { "Duplicate to body" } else { "Move to body" };
                            self.transaction(desc, |s| {
                                let act_uids = if ctrl_copy { s.duplicate_prims_no_undo(&prim_uids) }
                                               else { prim_uids.clone() };
                                // Stable UIDs → fresh flat index each iteration (the flat cache is rebuilt by
                                // each reparent; stale indices moved the wrong prim / dropped some — B1).
                                for &uid in act_uids.iter().rev() {
                                    if let Some(fi) = s.prim_index_by_uid(uid) {
                                        s.reparent_prim_to_body(fi, target_nid, None);
                                    }
                                }
                                let ids: Vec<SceneItemId> = act_uids.iter().map(|uid| SceneItemId::Prim(*uid)).collect();
                                s.clear_selection_unified();
                                s.select_after_create(&ids);
                                s.toast(&if ctrl_copy { format!("Duplicated {} to body", ids.len()) }
                                        else { format!("Moved {} to body", ids.len()) });
                            });
                        }
                    } else if let Some(target_node) = self.outliner.drop_reparent {
                        // Legacy layer/object reparent — capture full selection
                        let mut indices: Vec<usize> = self.selected_prim_indices_unified();
                        indices.sort();
                        if indices.is_empty() { indices.push(from); }
                        d.reparent = Some((indices, target_node));
                    } else if self.outliner.drop_root_y.is_some() {
                        // ROOT / empty-area drop → extract the dragged prim(s) into a NEW top-level
                        // SdfBody (a prim cannot live outside a body). Drive off STABLE UIDs; the whole
                        // move is ONE transaction (one undo entry). Skip if the drag is a lone prim that
                        // is ALREADY the sole occupant of a top-level body (nothing to extract into).
                        let mut selected_sorted: Vec<usize> = self.selected_prim_indices_unified();
                        selected_sorted.sort();
                        if selected_sorted.is_empty() { selected_sorted.push(from); }
                        let prim_uids: Vec<u32> = selected_sorted.iter()
                            .filter_map(|&fi| self.scene.primitives.get(fi).map(|p| p.prim_uid))
                            .collect();
                        // Guard against a no-op: dragging the ONLY prim of an already top-level body to
                        // root would just recreate an identical body. Detect that and do nothing.
                        let is_noop = !ctrl_copy && prim_uids.len() == 1
                            && self.scene.prim_body_map.get(from).map(|(nid, _)| {
                                self.scene.scene_graph.parent_body(*nid).is_none()
                                    && self.body_prim_count(*nid) == 1
                            }).unwrap_or(false);
                        if !prim_uids.is_empty() && !is_noop {
                            let desc = if ctrl_copy { "Duplicate to new body" } else { "Move to new body" };
                            self.transaction(desc, |s| {
                                let act_uids = if ctrl_copy { s.duplicate_prims_no_undo(&prim_uids) }
                                               else { prim_uids.clone() };
                                s.move_prims_to_new_root_body(&act_uids);
                            });
                        }
                    }

                    // No drop target, no reparent target, no root drop → cancel (do nothing).
                }
                self.outliner.dragging = None;
                self.outliner.drop_target = None;
                self.outliner.drop_target_y = None;
                self.outliner.drop_reparent = None;
                self.outliner.drop_on_prim = false;
                self.outliner.drop_root_y = None;
                self.outliner.drop_insert = None;
                self.outliner.drag_start_pos = None;
                self.outliner.drag_source = None;
            }
        }
    }
}
