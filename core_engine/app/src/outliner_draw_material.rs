//! Outliner Materials section — lists every `NodePayload::Material` graph node.
//!
//! Materials are scene-graph nodes (CLAUDE.md "Materials Are Referenced, Never Embedded" /
//! "Scene Graph Is The Only Owner"), so they live in the outliner like lights do: a dedicated
//! section with its own rows, NOT generic "MAT" tree rows (the generic walk skips them — see
//! outliner_draw.rs). Rows select as `SceneItemId::Node(nid)` via the unified-selection writers,
//! rename through `check_double_click` + `draw_rename_widget` (the outliner rename rule), and all
//! mutations are deferred into `OutlinerDeferred` → processed in `transaction()` after the draw.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use crate::*;
use crate::outliner_panel::OutlinerDeferred;

impl AzraApp {
    /// Draw the "Materials" section of the outliner tree.
    pub(crate) fn draw_outliner_materials(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        filtering: bool,
        filter: &str,
        ctrl: bool,
        shift: bool,
        d: &mut OutlinerDeferred,
    ) {
        use core_math::pbr_material::MapKind;
        use core_math::scene_graph::NodeId;
        // Snapshot (nid, mid, name, data) to avoid borrow issues with &mut self during the draw.
        let mut mats: Vec<(NodeId, u32, String, core_math::primitives::PbrMaterial)> = self
            .scene.scene_graph.materials()
            .map(|(nid, mid, name, data)| (nid, mid.0, name.to_string(), data.clone()))
            .collect();
        // HashMap iteration = random order (CLAUDE.md) — sort by stable MaterialId.
        mats.sort_by_key(|(_, mid, _, _)| *mid);

        // Pre-resolve each material's LOADED map channels into child-row labels (Modo shader-tree
        // style: "Metal_Scratched_01 — Roughness"). The map's runtime display name comes from the
        // texture array's slot-name registry (file stem at load time); slots restored from a save
        // have no name → fall back to the material's own name. Snapshotted here so the draw
        // closures below don't fight the `self.gpu` borrow.
        let channel_tag = |k: MapKind| match k {
            MapKind::Albedo => "Diffuse Color", // owner's Modo naming; editor calls it Albedo
            other => other.label(),
        };
        let mat_children: Vec<Vec<(MapKind, String, u32)>> = mats.iter().map(|(_, _, name, data)| {
            MapKind::ALL.iter().filter(|k| data.map(**k).is_set()).map(|&k| {
                let slot = data.map(k).tex_id;
                let map_name = self.gpu.as_ref()
                    .and_then(|g| g.material_textures.slot_name(slot))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| if name.is_empty() { "Image".into() } else { name.clone() });
                (k, format!("{} — {}", map_name, channel_tag(k)), slot)
            }).collect()
        }).collect();

        // Materials are EXPLICITLY-ADDED items (owner refinement 2026-07-17): with zero materials
        // in the scene, no Materials section renders at all — creation lives in the outliner's
        // + menu and the main Add menu ("🎨 Material").
        if mats.is_empty() { return; }

        // While filtering, hide the whole section unless a material name matches.
        if filtering && !mats.iter().any(|(_, _, n, _)| n.to_lowercase().contains(filter)) {
            return;
        }

        let header = format!("\u{1F3A8} Materials ({})", mats.len());
        egui::CollapsingHeader::new(egui::RichText::new(header).size(11.0).strong()
                .color(egui::Color32::from_rgb(200, 140, 220)))
            .id_salt("outliner_materials_section")
            .default_open(!mats.is_empty())
            .show(ui, |ui| {
                for (mi, (nid, mid, name, data)) in mats.iter().enumerate() {
                    let nid = *nid;
                    if filtering && !name.to_lowercase().contains(filter) { continue; }
                    let children = &mat_children[mi];
                    let has_children = !children.is_empty();
                    let expanded = self.outliner.expanded_materials.contains(&nid);
                    let sel = self.sel.usel.selected.contains(&SceneItemId::Node(nid));
                    let row_bg = if sel {
                        egui::Color32::from_rgba_unmultiplied(200, 140, 220, 50)
                    } else { egui::Color32::TRANSPARENT };
                    let row_resp = egui::Frame::none().fill(row_bg).rounding(3.0)
                        .inner_margin(egui::Margin { left: 4, right: 4, top: 1, bottom: 1 })
                        .show(ui, |ui| { ui.horizontal(|ui| {
                            // Keep name column aligned with eye-column rows.
                            ui.add_space(crate::outliner_actions::EYE_COL_W);
                            // Expand caret (Modo-shader-tree children = loaded map channels).
                            // Disabled (still space-reserving, so names stay aligned) when the
                            // material has no loaded maps. Same uniform expansion semantics as
                            // bodies: in `expanded_materials` = expanded.
                            let arrow = if expanded && has_children { "\u{25BC}" } else { "\u{25B6}" };
                            if ui.add_enabled(has_children, egui::Button::new(egui::RichText::new(arrow).small()).frame(false))
                                .on_hover_text("Show this material's maps")
                                .clicked()
                            {
                                if expanded { self.outliner.expanded_materials.remove(&nid); }
                                else { self.outliner.expanded_materials.insert(nid); }
                            }
                            // Sphere preview swatch (same cache the properties panel uses).
                            let tid = self.cache.mat_preview_cache.get_or_create(ctx, data);
                            ui.image(egui::load::SizedTexture::new(tid, egui::vec2(14.0, 14.0)));
                            // Chain badge: material owns a procedural node graph.
                            if self.ui.material_node_graphs.contains_key(mid) {
                                ui.label(
                                    egui::RichText::new("\u{1F517}")
                                        .small()
                                        .color(egui::Color32::from_rgb(120, 200, 255)),
                                )
                                .on_hover_text("Has procedural shader graph — edit in Properties");
                            }

                            let renaming = self.outliner.renaming == Some(SceneNodeId::Node(nid));
                            if renaming {
                                match crate::outliner_actions::draw_rename_widget(
                                    ui, ctx,
                                    &mut self.outliner.rename_buf,
                                    &mut self.outliner.rename_grace_frames,
                                ) {
                                    crate::outliner_actions::RenameResult::Commit(new_name) => {
                                        // Same name → no empty undo.
                                        let unchanged = self.scene.scene_graph.get(nid).map(|n| {
                                            matches!(&n.payload, core_math::scene_graph::NodePayload::Material { name, .. } if name == &new_name)
                                        }).unwrap_or(true);
                                        if !unchanged {
                                            self.transaction("Rename material", |s| {
                                                if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                                                    if let core_math::scene_graph::NodePayload::Material { name, .. } = &mut node.payload {
                                                        *name = new_name;
                                                    }
                                                }
                                                s.outliner.outliner_dirty = true;
                                            });
                                        }
                                        self.outliner.renaming = None;
                                    }
                                    crate::outliner_actions::RenameResult::Cancel => { self.outliner.renaming = None; }
                                    crate::outliner_actions::RenameResult::Editing => {}
                                }
                            } else {
                                let label = if name.is_empty() { format!("Material {}", mid) } else { name.clone() };
                                let resp = ui.selectable_label(sel, label);
                                if resp.clicked() {
                                    // Outliner Double-Click rule: NEVER resp.double_clicked().
                                    if crate::outliner_actions::check_double_click(ui, egui::Id::new(("mat_dblclick", nid.0))) {
                                        self.trigger_rename(SceneNodeId::Node(nid), name);
                                    } else {
                                        let item = SceneItemId::Node(nid);
                                        if shift {
                                            let vo = self.outliner.visual_order.clone();
                                            self.range_select_unified(item, &vo);
                                        } else if ctrl {
                                            self.toggle_select_unified(item);
                                        } else {
                                            self.select_single_unified(item);
                                        }
                                    }
                                }
                                let (prim_refs, face_refs) = self.scene.scene_graph
                                    .material_use_counts(core_math::primitives::MaterialId(*mid));
                                let in_use = prim_refs + face_refs > 0;
                                let resp = if in_use {
                                    resp.on_hover_text(format!("Used by {} prim(s), {} face(s)", prim_refs, face_refs))
                                } else {
                                    resp.on_hover_text("Not assigned to anything")
                                };
                                resp.azra_context_menu(|ui| {
                                    if ui.button("\u{270F} Rename").clicked() {
                                        self.trigger_rename(SceneNodeId::Node(nid), name);
                                        ui.close_menu();
                                    }
                                    if ui.button("Duplicate").clicked() {
                                        d.mat_duplicate = Some(nid);
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    let del_label = if in_use {
                                        format!("Delete (in use: {} prim(s), {} face(s))", prim_refs, face_refs)
                                    } else { "Delete".into() };
                                    if ui.button(del_label)
                                        .on_hover_text("Users revert to the default material (undoable)")
                                        .clicked()
                                    {
                                        d.mat_delete = Some(nid);
                                        ui.close_menu();
                                    }
                                });
                            }
                        }); });
                    self.check_scroll_to(ui, row_resp.response.rect, SceneNodeId::Node(nid));
                    self.outliner.visual_order_next.push(SceneItemId::Node(nid));

                    // ---- Map-channel CHILD rows (Modo shader-tree style) ----
                    // One row per LOADED channel: image icon + "map name — channel". Click =
                    // select the parent material AND focus its editor on that channel (highlight
                    // + scroll — see properties_material_node.rs). Context menu = Clear Map.
                    // NOT selection items (no SceneItemId — range select skips them), NOT
                    // draggable, NOT reorderable — layer drag/reorder is future shader-tree work.
                    if expanded && has_children && !filtering {
                        for (kind, label, slot) in children {
                            let kind = *kind;
                            // Tiny thumbnail of the actual image (same cache as the editor's
                            // 48px thumbs — one build serves both). None ⇒ generic icon.
                            let thumb = self.map_slot_thumb(ctx, *slot, kind.is_data());
                            ui.horizontal(|ui| {
                                // Indent one level past the parent's caret column.
                                ui.add_space(crate::outliner_actions::EYE_COL_W + 22.0);
                                if let Some(tid) = thumb {
                                    ui.image(egui::load::SizedTexture::new(tid, egui::vec2(14.0, 14.0)));
                                } else {
                                    ui.label(egui::RichText::new("\u{1F5BC}").size(10.0));
                                }
                                let resp = ui.selectable_label(false,
                                    egui::RichText::new(label.as_str()).small()
                                        .color(egui::Color32::from_rgb(190, 175, 200)));
                                if resp.clicked() {
                                    self.select_single_unified(SceneItemId::Node(nid));
                                    let now = ui.input(|i| i.time);
                                    self.outliner.focus_map_channel = Some((nid, kind, now));
                                }
                                resp.on_hover_text(format!("{} channel — click to edit", kind.label()))
                                    .azra_context_menu(|ui| {
                                        if ui.button("Clear Map")
                                            .on_hover_text("Unlink this map from the channel (undoable)")
                                            .clicked()
                                        {
                                            d.mat_clear_map = Some((nid, kind));
                                            ui.close_menu();
                                        }
                                    });
                            });
                        }
                    }
                }

                // "+ New Material" affordance at the bottom of the section.
                if ui.add(egui::Button::new(egui::RichText::new("+ New Material").small()).frame(false))
                    .on_hover_text("Create a material (also: outliner + menu, or Ctrl+M on a selection)")
                    .clicked()
                {
                    d.mat_create = true;
                }
            });
    }
}
