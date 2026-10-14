//! Outliner non-SDF node rows — Poly, NURBS, Imported, Group, Empty.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::*;
use crate::outliner_panel::OutlinerDeferred;

/// Approx row height for leaf poly/NURBS/import rows (clip-rect culling).
const NODE_ROW_HEIGHT: f32 = 18.0;

impl AzraApp {
    /// Draw a single non-SDF scene graph node row (Poly, NURBS, Imported, Group, Empty).
    /// Recurses into children so nested hierarchies are visible.
    /// Returns `true` if the node was drawn (not filtered out).
    pub(crate) fn draw_outliner_node_row(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        child_nid: NodeId,
        type_label: &str,
        type_color: egui::Color32,
        name: &str,
        visible: bool,
        locked: bool,
        root_children: &[NodeId],
        filtering: bool,
        filter: &str,
        ctrl_sg: bool,
        shift: bool,
        d: &mut OutlinerDeferred,
        vis_ch: &mut bool,
        depth: usize,
    ) -> bool {
        if depth >= 8 { return false; } // MAX_OUTLINER_DEPTH safety
        // B8: cull only when NEITHER this node's name NOR any descendant matches the search — otherwise a
        // matching nested node (e.g. a prim inside an SDF body inside a non-matching Group) was hidden by
        // its non-matching ancestor's name-only early return.
        if filtering && !name.to_lowercase().contains(filter)
            && !self.node_subtree_matches(child_nid, filter, depth) { return false; }

        // C-scale: leaf rows off-screen → reserve space only (same idea as prim clip cull).
        // 10k poly stress was paying full widget build for every row every frame (~43 ms lay=).
        let has_children = !self.scene.scene_graph.children(child_nid).is_empty();
        if !has_children {
            let clip = ui.clip_rect();
            let cursor_y = ui.cursor().min.y;
            if cursor_y + NODE_ROW_HEIGHT < clip.min.y || cursor_y > clip.max.y {
                ui.add_space(NODE_ROW_HEIGHT);
                self.outliner
                    .visual_order_next
                    .push(SceneItemId::Node(child_nid));
                if self.outliner.scroll_to_node == Some(SceneNodeId::Node(child_nid)) {
                    let fake_rect = egui::Rect::from_min_size(
                        egui::pos2(ui.cursor().min.x, cursor_y),
                        egui::vec2(ui.available_width(), NODE_ROW_HEIGHT),
                    );
                    ui.scroll_to_rect(fake_rect, Some(egui::Align::Center));
                    self.outliner.scroll_to_node = None;
                }
                return true;
            }
        }

        let sel = self.sel.usel.selected.contains(&SceneItemId::Node(child_nid));
        let row_bg = if sel {
            egui::Color32::from_rgba_unmultiplied(100, 180, 255, 50)
        } else {
            egui::Color32::TRANSPARENT
        };
        let node_row_resp = ui.horizontal(|ui| {
                // FIXED left eye column (aligned across all rows/depths)
                if crate::outliner_actions::eye_column_button(ui, visible).on_hover_text("Toggle visibility (Alt = solo)").clicked() {
                    let alt = ctx.input(|i| i.modifiers.alt);
                    if alt {
                        d.solo_node = Some(child_nid);
                    } else {
                        d.toggle_vis = Some(child_nid);
                    }
                }
                let lock_icon = if locked { "\u{1F512}" } else { "  " };
                if ui.small_button(lock_icon).on_hover_text("Toggle lock").clicked() {
                    d.toggle_lock = Some(child_nid);
                }
                // Group folder icon — warm stone
                if type_label == "GRP" {
                    crate::outliner_actions::draw_folder_icon(ui);
                }
                ui.colored_label(type_color, egui::RichText::new(type_label).size(9.0).strong());
                let is_node_renaming = self.outliner.renaming == Some(SceneNodeId::Node(child_nid));
                if is_node_renaming {
                    match crate::outliner_actions::draw_rename_widget(
                        ui, ctx,
                        &mut self.outliner.rename_buf,
                        &mut self.outliner.rename_grace_frames,
                    ) {
                        crate::outliner_actions::RenameResult::Commit(new_name) => {
                            // Same name as before → no undo entry (double-click rename + Enter).
                            let unchanged = self.scene.scene_graph.get(child_nid).map(|n| {
                                match &n.payload {
                                    NodePayload::PolygonalBody { name, .. }
                                    | NodePayload::NurbsBody { name, .. }
                                    | NodePayload::ImportedMesh { name, .. }
                                    | NodePayload::Group { name }
                                    | NodePayload::Empty { name }
                                    | NodePayload::Camera { name, .. } => name == &new_name,
                                    _ => true,
                                }
                            }).unwrap_or(true);
                            if !unchanged {
                                self.transaction("Rename node", |s| {
                                    if let Some(node) = s.scene.scene_graph.get_mut(child_nid) {
                                        match &mut node.payload {
                                            NodePayload::PolygonalBody { name, .. } => *name = new_name.clone(),
                                            NodePayload::NurbsBody { name, .. } => *name = new_name.clone(),
                                            NodePayload::ImportedMesh { name, .. } => *name = new_name.clone(),
                                            NodePayload::Group { name } => *name = new_name.clone(),
                                            NodePayload::Empty { name } => *name = new_name.clone(),
                                            NodePayload::Camera { name, .. } => *name = new_name.clone(),
                                            _ => {}
                                        }
                                    }
                                });
                            }
                            self.outliner.renaming = None;
                        }
                        crate::outliner_actions::RenameResult::Cancel => {
                            self.outliner.renaming = None;
                        }
                        crate::outliner_actions::RenameResult::Editing => {}
                    }
                } else {
                let name_display = if visible { name.to_string() } else { format!("({})", name) };
                let resp = ui.selectable_label(sel, name_display);
                if resp.clicked() {
                    if crate::outliner_actions::check_double_click(ui, egui::Id::new(("node_dblclick", child_nid.0))) {
                        self.trigger_rename(SceneNodeId::Node(child_nid), name);
                    } else {
                        let item = SceneItemId::Node(child_nid);
                        if shift {
                            let vo = self.outliner.visual_order.clone();
                            self.range_select_unified(item, &vo);
                        } else if ctrl_sg {
                            self.toggle_select_unified(item);
                        } else {
                            self.select_single_unified(item);
                        }
                    }
                }
                resp.azra_context_menu(|ui| {
                    if ui.button("\u{270F} Rename").clicked() {
                        self.trigger_rename(SceneNodeId::Node(child_nid), name);
                        ui.close_menu();
                    }
                    if type_label == "GRP" {
                        if ui
                            .button("\u{1F4BE} Save Group to Library\u{2026}")
                            .on_hover_text(
                                "Save this group + all child bodies as one kitbash part.\n\
                                 Renders a thumbnail on save.",
                            )
                            .clicked()
                        {
                            d.save_to_library = Some(child_nid);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if type_label != "POLY" {
                        if ui.button("Convert to Polygon Mesh").clicked() {
                            d.convert_action = Some((child_nid, "poly".into()));
                            ui.close_menu();
                        }
                    }
                    if type_label == "POLY" || type_label == "VOX" {
                        if ui
                            .button("Surface Sculpt")
                            .on_hover_text(
                                "3D-Coat Surface: sculpt this mesh (poly) or switch the voxel to Surface mode",
                            )
                            .clicked()
                        {
                            d.surface_sculpt = Some(child_nid);
                            ui.close_menu();
                        }
                    }
                    if type_label == "POLY" {
                        if ui
                            .button("\u{1F4BE} Save to Part\u{2026}")
                            .on_hover_text(
                                "Save this poly mesh to the kitbash library as a reusable part.\n\
                                 Thumbnail: gray matcap, square crop zoomed on the mesh, transparent background.",
                            )
                            .clicked()
                        {
                            d.save_to_library = Some(child_nid);
                            ui.close_menu();
                        }
                        if ui.button("Subdivide (CC)").clicked() {
                            d.subdivide_node = Some(child_nid);
                            ui.close_menu();
                        }
                        if ui.button("\u{25C8} Convert to SDF Primitive")
                            .on_hover_text("Convert this closed poly into a MeshVolume SDF primitive in the active body (poly consumed)")
                            .clicked() {
                            d.convert_to_sdf = Some(child_nid);
                            ui.close_menu();
                        }
                        if ui.button("\u{1F517} Link as SDF Modifier")
                            .on_hover_text("LIVE: the poly becomes an SDF primitive that follows this mesh.\nMove the poly → imprint follows. Edit the poly → imprint re-converts.\nThe poly is hidden (not deleted) — unhide it to edit.")
                            .clicked() {
                            d.link_to_sdf = Some(child_nid);
                            ui.close_menu();
                        }
                    }
                    if ui.button("Duplicate").clicked() {
                        d.duplicate_node = Some(child_nid);
                        ui.close_menu();
                    }
                    if type_label == "POLY" || type_label == "NURBS" {
                        if ui.button("Export OBJ...").clicked() {
                            d.export_node = Some(child_nid);
                            ui.close_menu();
                        }
                    }
                    if type_label == "POLY" {
                        if ui
                            .button("Bake Mesh Maps…")
                            .on_hover_text("Bake Curvature / AO / Normal / Position into the UV unwrap")
                            .clicked()
                        {
                            self.select_single_unified(crate::SceneItemId::Node(child_nid));
                            self.poly_request_mesh_map_bake_for(child_nid, None);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button("Delete").clicked() {
                        d.delete_node = Some(child_nid);
                        ui.close_menu();
                    }
                });
                }
        });
        // Paint non-SDF node row background AFTER layout
        let header_rect = node_row_resp.response.rect;
        ui.painter().rect_filled(header_rect.expand2(egui::vec2(0.0, 1.0)), 3.0, row_bg);
        // Scroll-to for "Find in Outliner" (F key)
        self.check_scroll_to(ui, header_rect, SceneNodeId::Node(child_nid));
        self.outliner.visual_order_next.push(SceneItemId::Node(child_nid));

        // Library material drop target (material_import.rs) — no-ops unless this row is an
        // SDF/poly body and a PBR folder material drag is live over it.
        self.material_row_drop_check(ctx, header_rect, child_nid);

        // Kitbash folder rows (Group / SdfObject): same body-drag source + drop zones as SdfBody.
        // NestInto release already accepts these payloads; hit-test was missing for OBJ.
        if type_label == "GRP" || type_label == "OBJ" {
            self.begin_body_drag_from_header(ctx, header_rect, child_nid);
            self.update_body_drop_over_header(ctx, ui, header_rect, child_nid, name);
        }

        // Draw children recursively
        let children: Vec<NodeId> = self.scene.scene_graph.children(child_nid).to_vec();
        if !children.is_empty() {
            ui.indent(egui::Id::new(("node_children", child_nid.0)), |ui| {
                for &sub_nid in &children {
                    let sub_data = self.scene.scene_graph.get(sub_nid).map(|n| {
                        (
                            Self::node_type_label(&n.payload),
                            Self::node_type_color(&n.payload),
                            n.payload.display_name().to_string(),
                            n.visible,
                            n.locked,
                            matches!(n.payload, NodePayload::SdfBody { .. }),
                        )
                    });
                    let (sub_label, sub_color, sub_name, sub_vis, sub_lock, is_sdf) =
                        match sub_data { Some(d) => d, None => continue };

                    if is_sdf {
                        self.draw_outliner_sdf_body(
                            ui, ctx, sub_nid, &sub_name, sub_vis,
                            root_children, filtering, filter,
                            ctrl_sg, shift, d, vis_ch, depth + 1,
                        );
                    } else {
                        self.draw_outliner_node_row(
                            ui, ctx, sub_nid, sub_label, sub_color,
                            &sub_name, sub_vis, sub_lock, root_children,
                            filtering, filter, ctrl_sg, shift, d, vis_ch, depth + 1,
                        );
                    }
                }
            });
        }

        true
    }

    /// B8: does `nid`'s subtree contain a search match? True if its own display_name matches, OR — if it is
    /// an SDF body — any of its prims' names match (same predicate the body row uses: `prims[i].name`), OR
    /// any descendant's subtree matches. Lets a matching nested node force its non-matching ancestor rows to
    /// render as context instead of being culled. Read-only; same 8-deep guard as the draw recursion.
    fn node_subtree_matches(&self, nid: NodeId, filter: &str, depth: usize) -> bool {
        if depth >= 8 { return false; }
        let is_sdf = match self.scene.scene_graph.get(nid) {
            Some(n) => {
                if n.payload.display_name().to_lowercase().contains(filter) { return true; }
                matches!(n.payload, NodePayload::SdfBody { .. })
            }
            None => return false,
        };
        if is_sdf && self.scene.prim_body_map.iter().enumerate().any(|(i, (bnid, _))| {
            *bnid == nid
                && self.scene.primitives.get(i).map_or(false, |p| p.name.to_lowercase().contains(filter))
        }) {
            return true;
        }
        let children: Vec<NodeId> = self.scene.scene_graph.children(nid).to_vec();
        children.into_iter().any(|sub| self.node_subtree_matches(sub, filter, depth + 1))
    }
}
