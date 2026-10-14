//! Outliner light rows — visibility, lock, rename, context menu.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use crate::*;
use crate::outliner_panel::OutlinerDeferred;

impl FourthLayerApp {
    /// Draw all light rows in the outliner tree.
    pub(crate) fn draw_outliner_lights(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        filtering: bool,
        filter: &str,
        ctrl: bool,
        shift: bool,
        d: &mut OutlinerDeferred,
    ) {
        // Collect light data snapshots to avoid borrow issues with &mut self
        let light_snaps: Vec<(u32, String, bool, bool, LightType)> = self.scene.scene_graph.lights()
            .map(|(_, l)| (l.id, l.name.clone(), l.visible, l.locked, l.light_type))
            .collect();
        for (light_id, name, vis, locked_l, lt) in &light_snaps {
            let light_id = *light_id;
            let vis = *vis;
            let locked_l = *locked_l;
            if filtering && !name.to_lowercase().contains(filter) { continue; }
            let sel = self.sel.usel.selected.contains(&SceneItemId::Light(light_id));
            let row_bg = if sel { egui::Color32::from_rgba_unmultiplied(200, 180, 60, 50) } else { egui::Color32::TRANSPARENT };
            let light_frame_resp = egui::Frame::none().fill(row_bg).rounding(3.0)
                .inner_margin(egui::Margin { left: 4, right: 4, top: 2, bottom: 2 })
                .show(ui, |ui| { ui.horizontal(|ui| {
                    // FIXED left eye column (aligned across all rows/depths)
                    if crate::outliner_actions::eye_column_button(ui, vis).on_hover_text("Toggle visibility (Alt = solo)").clicked() {
                        let alt = ctx.input(|i| i.modifiers.alt);
                        if alt {
                            let already_solo = light_snaps.iter().all(|(id, _, v, _, _)| {
                                (*id == light_id && *v) || (*id != light_id && !*v)
                            });
                            if already_solo {
                                // Restore only if something is still hidden (single light is a no-op).
                                let needs = light_snaps.iter().any(|(_, _, v, _, _)| !*v);
                                if needs {
                                    let all_ids: Vec<u32> = light_snaps.iter().map(|(id, _, _, _, _)| *id).collect();
                                    self.transaction("Restore light visibility", |s| {
                                        for id in all_ids { s.write_light(id, |l| l.visible = true); }
                                    });
                                }
                            } else {
                                let all_ids: Vec<u32> = light_snaps.iter().map(|(id, _, _, _, _)| *id).collect();
                                self.transaction("Solo light", |s| {
                                    for id in all_ids {
                                        let solo = id == light_id;
                                        s.write_light(id, |l| l.visible = solo);
                                    }
                                });
                            }
                        } else {
                            let v = !vis;
                            self.transaction("Toggle light visibility", |s| {
                                s.write_light(light_id, |l| l.visible = v);
                            });
                        }
                        self.mark_dirty(DirtyFlags::LIGHT);
                        self.outliner.outliner_dirty = true;
                    }
                    if ui.small_button(if locked_l { "\u{1F512}" } else { "  " }).clicked() {
                        let v = !locked_l;
                        self.transaction("Toggle light lock", |s| {
                            s.write_light(light_id, |l| l.locked = v);
                            s.outliner.outliner_dirty = true;
                        });
                    }
                    let icon = match lt {
                        LightType::Directional => "\u{2600}",
                        LightType::Point => "\u{1F4A1}",
                        LightType::Spot => "\u{1F526}",
                    };
                    ui.colored_label(egui::Color32::from_rgb(255, 200, 60), icon);
                    let is_light_renaming = self.outliner.renaming == Some(SceneNodeId::Light(light_id));
                    if is_light_renaming {
                        match crate::outliner_actions::draw_rename_widget(
                            ui, ctx,
                            &mut self.outliner.rename_buf,
                            &mut self.outliner.rename_grace_frames,
                        ) {
                            crate::outliner_actions::RenameResult::Commit(new_name) => {
                                // Same name → no empty undo.
                                let unchanged = self.scene.light_by_id(light_id)
                                    .map(|l| l.name == new_name).unwrap_or(true);
                                if !unchanged {
                                    self.transaction("Rename light", |s| {
                                        s.write_light(light_id, |l| l.name = new_name);
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
                    } else {
                        let resp = ui.selectable_label(sel, name);
                        if resp.clicked() {
                            if crate::outliner_actions::check_double_click(ui, egui::Id::new(("light_dblclick", light_id))) {
                                self.trigger_rename(SceneNodeId::Light(light_id), name);
                            } else {
                                let item = SceneItemId::Light(light_id);
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
                        resp.azra_context_menu(|ui| {
                            if ui.button("\u{270F} Rename").clicked() {
                                self.trigger_rename(SceneNodeId::Light(light_id), name);
                                ui.close_menu();
                            }
                            if ui.button("Duplicate Light").clicked() { d.light_duplicate = Some(light_id); ui.close_menu(); }
                            if ui.button("Delete Light").clicked() { d.light_delete = Some(light_id); ui.close_menu(); }
                        });
                    }
                }); });
            // Scroll-to for "Find in Outliner" (F key)
            self.check_scroll_to(ui, light_frame_resp.response.rect, SceneNodeId::Light(light_id));
            self.outliner.visual_order_next.push(SceneItemId::Light(light_id));
        }
    }
}
