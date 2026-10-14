//! Outliner SDF body row — collapsible header with prim tree inside.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::*;
use crate::outliner_panel::OutlinerDeferred;
use app_types::BodyPrimBuckets;

/// Maximum outliner nesting depth — matches GPU scope stack limit of 8.
const MAX_OUTLINER_DEPTH: usize = 8;

/// Approximate row height for prim rows — used for clip-rect culling.
const PRIM_ROW_HEIGHT: f32 = 22.0;

impl FourthLayerApp {
    /// Draw one SDF body node in the outliner panel.
    pub(crate) fn draw_outliner_sdf_body(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        child_nid: NodeId,
        name: &str,
        visible: bool,
        root_children: &[NodeId],
        filtering: bool,
        filter: &str,
        ctrl: bool,
        shift: bool,
        d: &mut OutlinerDeferred,
        vis_ch: &mut bool,
        depth: usize,
    ) {
        if depth >= MAX_OUTLINER_DEPTH { return; }
        let is_primary = child_nid == self.scene.sdf_body_nid;
        let prim_count = self.scene.primitives.len();

        // Helper: check if prim name matches filter
        let prim_matches = |idx: usize, prims: &[core_math::primitives::SdfPrimitive]| -> bool {
            !filtering || prims[idx].name.to_lowercase().contains(filter)
        };

        // Filter: skip if no prims match and body name doesn't match
        let body_name_match = !filtering || name.to_lowercase().contains(filter);
        let any_prim_match = if is_primary {
            !filtering || (0..prim_count).any(|i| {
                self.scene.prim_body_map.get(i).map(|(nid, _)| *nid) == Some(child_nid)
                    && prim_matches(i, &self.scene.primitives)
            })
        } else {
            !filtering || self.scene.prim_body_map.iter().enumerate()
                .any(|(i, (nid, _))| *nid == child_nid && prim_matches(i, &self.scene.primitives))
        };
        if !body_name_match && !any_prim_match { return; }

        // Count prims in this body
        let body_prim_count = self.body_prim_count(child_nid);

        let sdf_sel = self.sel.usel.selected.contains(&SceneItemId::Node(child_nid));
        let is_active_body = self.scene.active_body_nid == child_nid;
        // UNIFORM expansion semantics (user-owned): in `expanded_bodies` = expanded,
        // absent = collapsed (default collapsed keeps large scenes cheap). The active
        // body gets ONE auto-expand insert on activation (outliner_draw.rs) — activating
        // a body never collapses any other body.
        let sdf_collapsed = !self.outliner.expanded_bodies.contains(&child_nid);

        // --- Header row ---
        // The header bg is painted AFTER layout (over the selectable_label's own selected
        // fill), so selection state MUST be reflected here or a selected body row looks
        // identical to an unselected one (the "clicking a sub-body does nothing" bug).
        let sdf_bg = if sdf_sel {
            egui::Color32::from_rgba_unmultiplied(110, 85, 50, 235)
        } else if is_active_body {
            egui::Color32::from_rgba_unmultiplied(80, 65, 45, 220)
        } else {
            egui::Color32::from_rgba_unmultiplied(60, 50, 40, 200)
        };
        // Full-width drag sink under the header so body DnD does not latch panel resize.
        let (header_rect, header_hovered) = crate::outliner_actions::outliner_row_shell(
            ui,
            egui::Id::new(("outliner_body_dnd", child_nid.0)),
            |ui| {
            // Visibility toggle with Alt-solo — FIXED left column (aligned across all rows/depths)
            if crate::outliner_actions::eye_column_button(ui, visible)
                .on_hover_text("Toggle visibility (Alt = solo)").clicked()
            {
                let alt = ctx.input(|i| i.modifiers.alt);
                if alt {
                    d.solo_node = Some(child_nid);
                } else {
                    d.toggle_vis = Some(child_nid);
                }
            }
            // Collapse arrow
            let arrow = if sdf_collapsed && !filtering { "\u{25B6}" } else { "\u{25BC}" };
            if ui.small_button(arrow).clicked() {
                let shift_held = ctx.input(|i| i.modifiers.shift);
                if shift_held {
                    // Shift+click: collapse/expand ALL bodies visually (uniform semantics).
                    let want_collapsed = !sdf_collapsed; // toggle from current visual state
                    for &nid in root_children {
                        if !self.scene.scene_graph.get(nid).map(|n| matches!(n.payload, NodePayload::SdfBody { .. })).unwrap_or(false) {
                            continue;
                        }
                        if want_collapsed {
                            self.outliner.expanded_bodies.remove(&nid);
                        } else {
                            self.outliner.expanded_bodies.insert(nid);
                        }
                    }
                } else {
                    if self.outliner.expanded_bodies.contains(&child_nid) {
                        self.outliner.expanded_bodies.remove(&child_nid);
                    } else {
                        self.outliner.expanded_bodies.insert(child_nid);
                    }
                }
            }
            // SDF body icon — warm amber hexagon
            {
                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let p = ui.painter();
                let c = icon_rect.center();
                let r = 6.5;
                let fill = if is_active_body {
                    egui::Color32::from_rgba_unmultiplied(200, 155, 60, 230)
                } else {
                    egui::Color32::from_rgba_unmultiplied(160, 120, 50, 200)
                };
                let stroke = egui::Stroke::new(0.7, egui::Color32::from_rgb(200, 160, 70));
                let mut hex = Vec::with_capacity(6);
                for k in 0..6 {
                    let angle = std::f32::consts::FRAC_PI_3 * k as f32 - std::f32::consts::FRAC_PI_6;
                    hex.push(egui::pos2(c.x + r * angle.cos(), c.y + r * angle.sin()));
                }
                p.add(egui::Shape::convex_polygon(hex, fill, stroke));
            }
            // Rename or label
            let is_body_renaming = self.outliner.renaming == Some(SceneNodeId::Body(child_nid));
            if is_body_renaming {
                match crate::outliner_actions::draw_rename_widget(
                    ui, ctx,
                    &mut self.outliner.rename_buf,
                    &mut self.outliner.rename_grace_frames,
                ) {
                    crate::outliner_actions::RenameResult::Commit(new_name) => {
                        // Same name → no empty undo (double-click rename + Enter without edit).
                        let unchanged = self.scene.scene_graph.get(child_nid).map(|n| {
                            matches!(&n.payload, NodePayload::SdfBody { name, .. } if name == &new_name)
                        }).unwrap_or(true);
                        if !unchanged {
                            self.transaction("Rename body", |s| {
                                if let Some(node) = s.scene.scene_graph.get_mut(child_nid) {
                                    if let NodePayload::SdfBody { name, .. } = &mut node.payload {
                                        *name = new_name;
                                    }
                                }
                                s.outliner.outliner_dirty = true;
                            });
                        }
                        self.outliner.renaming = None;
                    }
                    crate::outliner_actions::RenameResult::Cancel => {
                        self.outliner.renaming = None;
                    }
                    crate::outliner_actions::RenameResult::Editing => {}
                }
                false
            } else {
                let label = format!("{} ({})", name, body_prim_count);
                let resp = ui.selectable_label(sdf_sel, egui::RichText::new(label).strong());
                if resp.clicked() {
                    if crate::outliner_actions::check_double_click(ui, egui::Id::new(("body_dblclick", child_nid.0))) {
                        self.trigger_rename(SceneNodeId::Body(child_nid), name);
                    } else if shift {
                        let vo = self.outliner.visual_order.clone();
                        self.range_select_unified(SceneItemId::Node(child_nid), &vo);
                    } else {
                        self.enter_body_scope(child_nid);
                    }
                }
                // Context menu
                resp.azra_context_menu(|ui| {
                    ui.label(format!("\u{2B21} {}", name));
                    ui.separator();
                    if ui.button("\u{270F} Rename").clicked() {
                        self.trigger_rename(SceneNodeId::Body(child_nid), name);
                        ui.close_menu();
                    }
                    if ui.button("Enter Scope").clicked() {
                        self.enter_body_scope(child_nid);
                        ui.close_menu();
                    }
                    if ui
                        .button("\u{1F4BE} Save to Library\u{2026}")
                        .on_hover_text(
                            "Save this body to the kitbash library.\n\
                             Choose name + folder, then drag from Body Library into the viewport.",
                        )
                        .clicked()
                    {
                        d.save_to_library = Some(child_nid);
                        ui.close_menu();
                    }
                    if child_nid != self.scene.sdf_body_nid {
                        if ui.button("\u{1F4E4} Dissolve Body").clicked() {
                            self.dissolve_sdf_body(child_nid);
                            ui.close_menu();
                        }
                    }
                    if ui.button("Convert to Polygon Mesh").clicked() {
                        d.convert_action = Some((child_nid, "poly".into()));
                        ui.close_menu();
                    }
                    if ui.button("Duplicate").clicked() {
                        d.duplicate_node = Some(child_nid);
                        ui.close_menu();
                    }
                    // Create Group (kitbash folder) with this body / current selection inside.
                    // Works for 1 body (RMB on a single body) or multi-select (Ctrl+G).
                    let nestable_sel = self
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
                        .count();
                    let group_label = if nestable_sel >= 2 {
                        "Group Selected  Ctrl+G"
                    } else {
                        "Create Group  Ctrl+G"
                    };
                    if ui
                        .button(group_label)
                        .on_hover_text(
                            "Put this body (and any other selected bodies) in a new Group folder.\n\
                             Double-click the group name to rename. Drag more bodies onto the group to nest them.",
                        )
                        .clicked()
                    {
                        // Force-include the right-clicked body (never toggle it off).
                        if !self.sel.usel.selected.contains(&SceneItemId::Node(child_nid)) {
                            if nestable_sel == 0 {
                                self.select_single_unified(SceneItemId::Node(child_nid));
                            } else {
                                self.sel.usel.selected.insert(SceneItemId::Node(child_nid));
                            }
                        }
                        self.group_selected_bodies_or_prims();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("\u{1F5D1} Delete").clicked() {
                        d.delete_node = Some(child_nid);
                        ui.close_menu();
                    }
                });
                resp.hovered()
            }
            },
        );

        // Paint header background AFTER layout
        ui.painter().rect_filled(header_rect.expand2(egui::vec2(0.0, 1.0)), 3.0, sdf_bg);

        // L6 Predictive warming: report hovered body to warming system
        if header_hovered || ui.rect_contains_pointer(header_rect) {
            self.cache.warming.hovered_body = Some(child_nid);
        }

        // Scroll-to for "Find in Outliner" (F key)
        self.check_scroll_to(ui, header_rect, SceneNodeId::Body(child_nid));
        self.outliner.visual_order_next.push(SceneItemId::Node(child_nid));

        // Library material drop target (material_import.rs): dragging a PBR folder material
        // from the content library onto this body row applies it (highlight + release).
        self.material_row_drop_check(ctx, header_rect, child_nid);

        // Track body header bottom for drop-position insertion
        self.outliner.body_header_bottoms.push((child_nid, header_rect.bottom()));

        // Drag-drop: prim drag over an SdfBody header.
        // SUB-body (parent is an SdfBody): three zones — top third = insert BEFORE the group
        // in the parent's child_order, bottom third = insert AFTER the group (after its whole
        // subtree, since the group is ONE ref in the parent's order), center = reparent INTO
        // the group. These are the between-slots flat prim indices cannot express (bugs 1–3:
        // slots adjacent to a group row simply did not exist — the header swallowed the gap).
        // TOP-level body: whole rect = reparent into (unchanged; the root/new-body path owns
        // the space between top-level rows).
        if self.outliner.dragging.is_some() {
            if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                use crate::outliner_drop_slots as slots;
                use core_math::scene_graph::BodyChildRef;
                // Gap-free band (row rect + half spacing each side); x gated by the panel
                // clip rect so the whole row width is hittable, not just the label extent.
                let clip = ui.clip_rect();
                let band = slots::row_band(header_rect.top(), header_rect.bottom(), ui.spacing().item_spacing.y);
                if pointer.x >= clip.left() && pointer.x <= clip.right()
                    && slots::band_contains(band, pointer.y)
                {
                    let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                    let parent_pos = self.scene.scene_graph.get(child_nid)
                        .and_then(|n| n.parent)
                        .filter(|&p| matches!(
                            self.scene.scene_graph.get(p).map(|n| &n.payload),
                            Some(NodePayload::SdfBody { .. })
                        ))
                        .and_then(|parent| {
                            self.scene.scene_graph.body_child_order(parent).iter()
                                .position(|&r| r == BodyChildRef::Body(child_nid))
                                .map(|pos| (parent, pos))
                        });
                    let zone = slots::classify_zone(header_rect.top(), header_rect.bottom(), pointer.y, 1.0 / 3.0);
                    match slots::resolve_prim_over_body_header(child_nid, parent_pos, zone) {
                        slots::PrimOverBodyHeader::InsertIntoOrder { body, index } => {
                            self.outliner.drop_insert = Some((body, index));
                            let (y, action) = if zone == slots::HeaderZone::Top {
                                (header_rect.top(), slots::SlotAction::Before(name))
                            } else {
                                (header_rect.bottom(), slots::SlotAction::After(name))
                            };
                            self.outliner.drop_target_y = Some(y);
                            // Between-slot in the PARENT's order → line at the header's own
                            // indent (= sibling depth of the receiving container).
                            self.outliner.drop_visual = Some(app_types::DropVisual {
                                kind: app_types::DropVisualKind::Line,
                                x_left: header_rect.left(),
                                y,
                                height: 0.0,
                                label: slots::slot_label(action, copy),
                                show_hint: false,
                            });
                        }
                        slots::PrimOverBodyHeader::ReparentInto { body } => {
                            // Dropping on SdfBody = reparent to this body → full-row HIGHLIGHT
                            // (drop-INTO is a highlight, never a line).
                            self.outliner.drop_reparent = Some(SceneNodeId::Body(body));
                            self.outliner.drop_visual = Some(app_types::DropVisual {
                                kind: app_types::DropVisualKind::Highlight,
                                x_left: header_rect.left(),
                                y: header_rect.top(),
                                height: header_rect.height(),
                                label: slots::slot_label(slots::SlotAction::Into(name), copy),
                                show_hint: false,
                            });
                        }
                    }
                }
            }
        }

        // HOVER-TO-EXPAND: hovering a COLLAPSED body header for ~0.6s while dragging
        // (prim or body) expands it so the drop can land inside precisely. Blender leaves
        // it open afterwards — so do we (no restore tracking).
        if (self.outliner.dragging.is_some() || self.outliner.dragging_body.is_some())
            && sdf_collapsed && !filtering
        {
            use crate::outliner_drop_slots as slots;
            if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                let clip = ui.clip_rect();
                let band = slots::row_band(header_rect.top(), header_rect.bottom(), ui.spacing().item_spacing.y);
                if pointer.x >= clip.left() && pointer.x <= clip.right()
                    && slots::band_contains(band, pointer.y)
                {
                    use crate::outliner_dnd::HOVER_EXPAND_SECS;
                    let now = ctx.input(|i| i.time);
                    match self.outliner.hover_expand {
                        Some((nid, since)) if nid == child_nid => {
                            if now - since >= HOVER_EXPAND_SECS {
                                self.outliner.expanded_bodies.insert(child_nid);
                                self.outliner.hover_expand = None;
                            } else {
                                ctx.request_repaint_after(std::time::Duration::from_millis(60));
                            }
                        }
                        _ => {
                            self.outliner.hover_expand = Some((child_nid, now));
                            ctx.request_repaint_after(std::time::Duration::from_millis(650));
                        }
                    }
                    self.outliner.hover_expand_hit = true;
                }
            }
        }

        // Body header drag initiation + drop zones (shared with Group / SdfObject rows).
        self.begin_body_drag_from_header(ctx, header_rect, child_nid);
        self.update_body_drop_over_header(ctx, ui, header_rect, child_nid, name);

        // --- Content section (when not collapsed) ---
        if !sdf_collapsed || filtering {
            // Active body scope indicator
            if is_active_body && child_nid != self.scene.sdf_body_nid {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 160, 70),
                        egui::RichText::new("\u{25B8} editing scope").size(9.0),
                    );
                });
            }

            // Build/fetch cached prim buckets for this body
            if !self.outliner.cached_body_buckets.contains_key(&child_nid) {
                let buckets = Self::build_body_prim_buckets(
                    child_nid, is_primary, &self.scene.prim_body_map, &self.scene.primitives,
                );
                self.outliner.cached_body_buckets.insert(child_nid, buckets);
            }

            ui.indent(egui::Id::new(("sdf_body_indent", child_nid.0)), |ui| {
                if body_prim_count == 0 {
                    ui.weak("(empty)");
                }

                let clip = ui.clip_rect();

                // UNIFIED ORDER: draw prims AND child bodies in the body's single `child_order`
                // sequence, so a sub-body and a loose prim INTERLEAVE in stack order (the feature).
                // Was: all child bodies first, then all loose prims. `resolve_child_order` is the
                // SAME resolver the bake serializer uses (positional per uid → duplicate-uid safe),
                // returning body-LOCAL prim indices; map each to its flat index via the cached bucket.
                let flat = self.outliner.cached_body_buckets[&child_nid].flat.clone();
                let resolved = self.scene.scene_graph.resolve_child_order(child_nid);
                for entry in resolved {
                    match entry {
                        Err(cb_nid) => {
                            // Expand an SdfObject wrapper into its bodies (display nesting); a direct
                            // SdfBody/Group is drawn as one recursion.
                            let expanded: Vec<core_math::scene_graph::NodeId> = match self.scene.scene_graph.get(cb_nid).map(|n| &n.payload) {
                                Some(core_math::scene_graph::NodePayload::SdfObject { .. }) =>
                                    self.scene.scene_graph.child_bodies(cb_nid),
                                _ => vec![cb_nid],
                            };
                            for nid in expanded {
                                let cb_data = self.scene.scene_graph.get(nid).map(|n| {
                                    (n.payload.display_name().to_string(), n.visible)
                                });
                                if let Some((cb_name, cb_visible)) = cb_data {
                                    self.draw_outliner_sdf_body(
                                        ui, ctx, nid, &cb_name, cb_visible,
                                        root_children, filtering, filter,
                                        ctrl, shift, d, vis_ch, depth + 1,
                                    );
                                }
                            }
                        }
                        Ok(local) => {
                            // body-local index → flat index via the cached bucket (built in body-local order).
                            let i = match flat.get(local).copied() { Some(i) => i, None => continue };
                            if !prim_matches(i, &self.scene.primitives) { continue; }
                            self.draw_prim_row_clipped(ui, ctx, i, ctrl, shift, vis_ch, d, &clip);
                        }
                    }
                }

                // END-OF-BODY drop strip: during a prim drag, a half-row catch zone below the
                // last child = "insert at the END of this body's child_order". This is the slot
                // that did not exist when the body's last child is a sub-body (dropping there
                // fell through to the root/new-body path). Only allocated while a drag is live.
                //
                // BLENDER X-RESOLUTION: at the end of a NESTED list this one strip is ambiguous
                // across depths — end of THIS body, after this body in its parent (when this
                // body's subtree is the LAST entry of the parent's order), … up to the root/
                // new-body zone. The cursor X picks the depth: right of the child indent →
                // into the deeper container, left → after it at the shallower depth. The
                // insertion line is drawn at the CHOSEN container's indent (the disambiguator).
                if self.outliner.dragging.is_some() {
                    let (strip_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), PRIM_ROW_HEIGHT * 0.5),
                        egui::Sense::hover(),
                    );
                    if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
                        use crate::outliner_drop_slots as slots;
                        use core_math::scene_graph::BodyChildRef;
                        let clip = ui.clip_rect();
                        let band = slots::row_band(strip_rect.top(), strip_rect.bottom(), ui.spacing().item_spacing.y);
                        if pointer.x >= clip.left() && pointer.x <= clip.right()
                            && slots::band_contains(band, pointer.y)
                        {
                            let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                            let indent_step = ui.spacing().indent;
                            // Candidate slots, DEEPEST-FIRST (descending indent):
                            //   (indent_x, Some((body, child_order index, label)))  — in-tree slot
                            //   (indent_x, None)                                    — root/new-body
                            let mut cands: Vec<(f32, Option<(NodeId, usize, String)>)> = Vec::new();
                            let mut x = strip_rect.left();
                            let own_len = self.scene.scene_graph.body_child_order(child_nid).len();
                            cands.push((x, Some((
                                child_nid, own_len,
                                slots::slot_label(slots::SlotAction::EndOf(name), copy),
                            ))));
                            let mut cur = child_nid;
                            loop {
                                let Some(p) = self.scene.scene_graph.get(cur).and_then(|n| n.parent) else { break };
                                let parent_is_body = matches!(
                                    self.scene.scene_graph.get(p).map(|n| &n.payload),
                                    Some(NodePayload::SdfBody { .. })
                                );
                                if parent_is_body {
                                    let order = self.scene.scene_graph.body_child_order(p);
                                    // The parent's after-slot only shares this y when the
                                    // current subtree is the LAST entry of its order.
                                    if order.last() != Some(&BodyChildRef::Body(cur)) { break; }
                                    x -= indent_step;
                                    let cur_name = self.scene.scene_graph.get(cur)
                                        .map(|n| n.payload.display_name().to_string())
                                        .unwrap_or_default();
                                    cands.push((x, Some((
                                        p, order.len(),
                                        slots::slot_label(slots::SlotAction::After(&cur_name), copy),
                                    ))));
                                    cur = p;
                                } else {
                                    // Chain exits to the root: the shallowest choice is the
                                    // new-body zone (a prim cannot live outside a body).
                                    x -= indent_step;
                                    cands.push((x, None));
                                    break;
                                }
                            }
                            let y = strip_rect.top();
                            match slots::pick_by_indent(&cands, pointer.x) {
                                Some(Some((body, index, label))) => {
                                    self.outliner.drop_insert = Some((*body, *index));
                                    self.outliner.drop_target_y = Some(y);
                                    self.outliner.drop_visual = Some(app_types::DropVisual {
                                        kind: app_types::DropVisualKind::Line,
                                        x_left: cands.iter().find(|(_, c)| c.as_ref()
                                                .map(|(b, i, _)| (*b, *i)) == Some((*body, *index)))
                                            .map(|(cx, _)| *cx).unwrap_or(strip_rect.left()),
                                        y,
                                        height: 0.0,
                                        label: label.clone(),
                                        show_hint: false,
                                    });
                                }
                                Some(None) => {
                                    // Root / new-body zone chosen by cursor X.
                                    self.outliner.drop_root_y = Some(y);
                                    self.outliner.drop_visual = Some(app_types::DropVisual {
                                        kind: app_types::DropVisualKind::Line,
                                        x_left: cands.last().map(|(cx, _)| *cx).unwrap_or(clip.left() + 6.0),
                                        y,
                                        height: 0.0,
                                        label: slots::slot_label(slots::SlotAction::NewBody, copy),
                                        show_hint: true,
                                    });
                                }
                                None => {}
                            }
                        }
                    }
                }
            });
        }
    }

    /// Build prim-index buckets for one SDF body in a single pass.
    /// Called once per dirty cycle, result cached in `outliner.cached_body_buckets`.
    fn build_body_prim_buckets(
        body_nid: NodeId,
        _is_primary: bool,
        prim_body_map: &[(NodeId, usize)],
        primitives: &[core_math::primitives::SdfPrimitive],
    ) -> BodyPrimBuckets {
        let mut b = BodyPrimBuckets::default();
        let prim_count = primitives.len();
        for i in 0..prim_count {
            if prim_body_map.get(i).map(|(nid, _)| *nid) != Some(body_nid) { continue; }
            b.flat.push(i);
        }
        b
    }

    /// Draw a prim row with clip-rect culling. Off-screen rows get `add_space` instead of full widget draw.
    fn draw_prim_row_clipped(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        i: usize,
        ctrl: bool,
        shift: bool,
        vis_ch: &mut bool,
        d: &mut OutlinerDeferred,
        clip: &egui::Rect,
    ) {
        let uid = self.scene.primitives[i].prim_uid;
        let cursor_y = ui.cursor().min.y;
        // If the row is fully above or below the visible clip rect, skip drawing
        if cursor_y + PRIM_ROW_HEIGHT < clip.min.y || cursor_y > clip.max.y {
            // Reserve space so layout stays consistent
            ui.add_space(PRIM_ROW_HEIGHT);
            self.outliner.visual_order_next.push(SceneItemId::Prim(uid));
            // Still handle scroll-to target for off-screen rows
            if self.outliner.scroll_to_node == Some(SceneNodeId::Prim(uid)) {
                let fake_rect = egui::Rect::from_min_size(
                    egui::pos2(ui.cursor().min.x, cursor_y),
                    egui::vec2(ui.available_width(), PRIM_ROW_HEIGHT),
                );
                ui.scroll_to_rect(fake_rect, Some(egui::Align::Center));
                self.outliner.scroll_to_node = None;
            }
            return;
        }
        let row_resp = self.draw_outliner_prim_row(ui, ctx, i, ctrl, shift, vis_ch, &mut d.rename_commit, &mut d.bool_change);
        self.update_prim_drop_zone(ctx, ui, row_resp, i);
        self.outliner.visual_order_next.push(SceneItemId::Prim(uid));
    }

    // enter_body_scope lives in outliner_actions.rs (File Organization rule) —
    // already-there guard + SdfBody payload guard merged there.
}
