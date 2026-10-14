//! Outliner panel + right panel layout — moved from ui_panels.rs.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use core_math::primitives::{BooleanOp, ModifierType};
use crate::*;

impl FourthLayerApp {
    /// Compute drop zone for a prim row during drag.
    /// Sets drop_target / drop_target_y / drop_on_prim + the slot's DropVisual (indicator
    /// geometry + action label). Zone semantics live in outliner_drop_slots.rs (pure).
    pub(crate) fn update_prim_drop_zone(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        row_rect: egui::Rect,
        flat_idx: usize,
    ) {
        use crate::outliner_drop_slots as slots;
        if self.outliner.dragging.is_none() || self.outliner.dragging == Some(flat_idx) { return; }
        if let Some(pointer) = ctx.input(|inp| inp.pointer.hover_pos()) {
            // GAP-FREE hit band: the row owns its rect PLUS half the item spacing above and
            // below (adjacent bands tile exactly — see slots::row_band). X is gated by the
            // panel clip rect, not the row rect, so pixels right of a short label still hit.
            let clip = ui.clip_rect();
            let band = slots::row_band(row_rect.top(), row_rect.bottom(), ui.spacing().item_spacing.y);
            if pointer.x >= clip.left() && pointer.x <= clip.right()
                && slots::band_contains(band, pointer.y)
            {
                let copy = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                let name = self.scene.primitives.get(flat_idx)
                    .map(|p| p.name.clone()).unwrap_or_default();
                let zone = slots::classify_zone(row_rect.top(), row_rect.bottom(), pointer.y, 1.0 / 3.0);
                // Between-slots (Top/Bottom) resolve to the CANONICAL child_order slot —
                // `drop_insert = (body, index)` — the SAME representation the visual label is
                // derived from and the SAME one `move_prims_to_body_slot` applies on release.
                // Never `flat_idx ± 1`: a flat index cannot express "end of this body" and
                // silently crosses body boundaries (the dead-release / append-at-end bug class).
                if zone != slots::HeaderZone::Center {
                    let (body_nid, row_uid) = match (
                        self.scene.prim_body_map.get(flat_idx).map(|&(nid, _)| nid),
                        self.scene.primitives.get(flat_idx).map(|p| p.prim_uid),
                    ) {
                        (Some(n), Some(u)) => (n, u),
                        _ => return,
                    };
                    let order = self.scene.scene_graph.body_child_order(body_nid);
                    if let slots::PrimOverPrimRow::Insert { body, index } =
                        slots::resolve_prim_over_prim_row(&order, body_nid, row_uid, zone)
                    {
                        self.outliner.drop_insert = Some((body, index));
                        let (y, action) = if zone == slots::HeaderZone::Top {
                            (row_rect.top(), slots::SlotAction::Before(&name))
                        } else {
                            (row_rect.bottom(), slots::SlotAction::After(&name))
                        };
                        self.outliner.drop_target_y = Some(y);
                        self.outliner.drop_visual = Some(app_types::DropVisual {
                            kind: app_types::DropVisualKind::Line,
                            x_left: row_rect.left(),
                            y,
                            height: 0.0,
                            label: slots::slot_label(action, copy),
                            show_hint: false,
                        });
                    }
                    return;
                }
                // Center zone: group with the row's prim.
                self.outliner.drop_target = Some(flat_idx);
                self.outliner.drop_target_y = Some(row_rect.center().y);
                self.outliner.drop_on_prim = true;
                self.outliner.drop_visual = Some(app_types::DropVisual {
                    kind: app_types::DropVisualKind::Highlight,
                    x_left: row_rect.left(),
                    y: row_rect.top(),
                    height: row_rect.height(),
                    label: slots::slot_label(slots::SlotAction::Group(&name), copy),
                    show_hint: false,
                });
            }
        }
    }

    pub(crate) fn draw_outliner_prim_row(
        &mut self, ui: &mut egui::Ui, ctx: &egui::Context, i: usize, ctrl: bool, shift: bool,
        vis_ch: &mut bool, rename_commit: &mut Option<(usize, String)>, bool_change: &mut Option<(u32, BooleanOp)>,
    ) -> egui::Rect {
        let prims = &self.scene.primitives;
        let prim_visible = prims[i].visible;
        let prim_locked = prims[i].locked;
        let prim_name = prims[i].name.clone();
        let prim_icon = prims[i].shape.icon().to_string();
        let prim_operation = prims[i].operation.clone();
        let prim_instance_master_id = prims[i].instance_master_id;
        let prim_has_active_modifier = prims[i].modifier.is_active();
        let prim_mod_type = prims[i].modifier.mod_type;
        // BASE = first slot of the body's resolved child_order (chain order), NOT flat index 0 —
        // reorder permutes only child_order, so `i == 0` diverged after any reorder (green-dot bug).
        let is_base = self.scene.is_base_prim_flat(i);
        let _op_icon = if is_base { "▶".to_string() } else { match &prim_operation {
            BooleanOp::Union{k}=>if *k>0.0{"∪~"}else{"∪"},
            BooleanOp::Subtract{k}=>if *k>0.0{"−~"}else{"−"},
            BooleanOp::Intersect=>"∩",
            BooleanOp::SmoothIntersect{k}=>if *k>0.0{"∩~"}else{"∩"},
            BooleanOp::ChamferUnion{..}=>"◇∪",
            BooleanOp::ChamferSubtract{..}=>"◇−",
            BooleanOp::Seam{raised,..}=>if *raised{"≈+"}else{"≈−"},
            BooleanOp::Extrude{strength,..}=>if *strength<0.0{"▾"}else{"▴"},
        }.to_string()};
        let sel = self.sel.usel.selected.contains(&SceneItemId::Prim(prims[i].prim_uid));
        let act = self.active_prim_index_unified() == Some(i);
        let prim_uid = prims[i].prim_uid;
        let is_renaming = self.outliner.renaming == Some(SceneNodeId::Prim(prim_uid));

        let _row_id = egui::Id::new(("outliner_row", i));
        let op_color = if is_base { egui::Color32::from_rgb(100,200,100) } else {
            match &prim_operation {
                BooleanOp::Union{k} => if *k>0.0 {egui::Color32::from_rgb(100,180,255)} else {egui::Color32::from_rgb(80,150,220)},
                BooleanOp::Subtract{k} => if *k>0.0 {egui::Color32::from_rgb(255,130,100)} else {egui::Color32::from_rgb(220,90,70)},
                BooleanOp::Intersect => egui::Color32::from_rgb(220,200,80),
                BooleanOp::SmoothIntersect{..} => egui::Color32::from_rgb(220,200,80),
                BooleanOp::ChamferUnion{..} => egui::Color32::from_rgb(100,200,180),
                BooleanOp::ChamferSubtract{..} => egui::Color32::from_rgb(255,160,100),
                BooleanOp::Seam{..} => egui::Color32::from_rgb(190,120,255),
                BooleanOp::Extrude{..} => egui::Color32::from_rgb(130,220,150),
            }};
        // Modo-style: active = orange 35%, selected = orange 20%, hovered = blue 10%
        let row_bg = if act { egui::Color32::from_rgba_unmultiplied(255,158,61,90) }
                     else if sel { egui::Color32::from_rgba_unmultiplied(255,158,61,50) }
                     else { egui::Color32::TRANSPARENT };
        // Full-width drag sink under the row (see outliner_row_shell) so egui does not
        // latch the SidePanel resize edge / height splitter as the drag target.
        let (row_rect, ()) = crate::outliner_actions::outliner_row_shell(
            ui,
            egui::Id::new(("outliner_prim_dnd", prim_uid)),
            |ui| {
                // Visibility toggle (eye icon) — FIXED left column (aligned across all rows/depths)
                if crate::outliner_actions::eye_column_button(ui, prim_visible).on_hover_text("Toggle visibility (Alt = solo)").clicked() {
                    let alt = ctx.input(|inp| inp.modifiers.alt);
                    if alt {
                        // Solo within same body only
                        let my_body = self.scene.prim_body_map.get(i).map(|(nid, _)| *nid);
                        let same_body_indices: Vec<usize> = my_body
                            .map(|b| self.body_prim_indices(b))
                            .unwrap_or_default();
                        let already_solo = same_body_indices.iter()
                            .all(|&fi| if fi == i { self.scene.primitives[fi].visible } else { !self.scene.primitives[fi].visible });
                        if already_solo {
                            // Restore only if something is still hidden (single-prim body is a no-op).
                            let needs = same_body_indices.iter()
                                .any(|&fi| fi < self.scene.primitives.len() && !self.scene.primitives[fi].visible);
                            if needs {
                                self.transaction("Restore prim visibility", |s| {
                                    for &fi in &same_body_indices {
                                        s.write_prim(fi, |p| p.visible = true);
                                    }
                                });
                            }
                        } else {
                            let target = i;
                            self.transaction("Solo prim", |s| {
                                for &fi in &same_body_indices {
                                    let vis = fi == target;
                                    s.write_prim(fi, |p| p.visible = vis);
                                }
                            });
                        }
                        *vis_ch = true;
                        self.mark_dirty(DirtyFlags::RENDER);
                        self.outliner.outliner_dirty = true;
                    } else {
                        self.push_cmd(AppCommand::SetVisible(prim_uid, !prim_visible));
                        *vis_ch = true;
                        self.outliner.outliner_dirty = true;
                    }
                }
                // Lock indicator
                if prim_locked {
                    ui.colored_label(egui::Color32::from_rgb(180, 130, 60), "\u{2612}");
                }
                // Instance indicator
                let is_instance = prim_instance_master_id.is_some();
                let is_master = self.is_master(i);
                if is_instance {
                    ui.colored_label(egui::Color32::from_rgb(120, 200, 255), "◇");
                } else if is_master {
                    ui.colored_label(egui::Color32::from_rgb(255, 200, 80), "★");
                }
                // Boolean op indicator (colored dot)
                let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.5, op_color);
                // Modifier indicator
                if prim_has_active_modifier {
                    let (mod_icon, mod_col) = match prim_mod_type {
                        ModifierType::Twist  => ("T", egui::Color32::from_rgb(180, 120, 255)),
                        ModifierType::Taper  => ("P", egui::Color32::from_rgb(255, 180, 80)),
                        ModifierType::Smooth => ("S", egui::Color32::from_rgb(80, 220, 220)),
                        ModifierType::Displace => ("D", egui::Color32::from_rgb(220, 80, 180)),
                        _ => ("", egui::Color32::TRANSPARENT),
                    };
                    ui.colored_label(mod_col, mod_icon);
                }

                if is_renaming {
                    match crate::outliner_actions::draw_rename_widget(
                        ui, ctx,
                        &mut self.outliner.rename_buf,
                        &mut self.outliner.rename_grace_frames,
                    ) {
                        crate::outliner_actions::RenameResult::Commit(new_name) => {
                            *rename_commit = Some((i, new_name));
                            self.outliner.renaming = None;
                        }
                        crate::outliner_actions::RenameResult::Cancel => {
                            self.outliner.renaming = None;
                        }
                        crate::outliner_actions::RenameResult::Editing => {}
                    }
                } else {
                    let label_text = format!("{} {}", prim_icon, prim_name);
                    let resp = ui.selectable_label(sel, &label_text);

                    if resp.clicked() {
                        if crate::outliner_actions::check_double_click(ui, egui::Id::new(("prim_dblclick", prim_uid))) {
                            self.trigger_rename(SceneNodeId::Prim(prim_uid), &prim_name);
                        } else {
                        let item = SceneItemId::Prim(prim_uid);
                        if shift {
                            let vo = self.outliner.visual_order.clone();
                            self.range_select_unified(item, &vo);
                        } else if ctrl {
                            self.toggle_select_unified(item);
                        } else {
                            self.select_single_unified(item);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }

                    resp.azra_context_menu(|ui| {
                        ui.label(format!("⚙ {} — Boolean", prim_name));
                        ui.separator();
                        if is_base {
                            ui.weak("(Base = no boolean op)");
                        } else {
                            if let Some(new_op) = crate::ui_panels::bool_op_menu_items(ui, &prim_operation, true) {
                                *bool_change = Some((prim_uid, new_op));
                            }
                        }
                        ui.separator();
                        // Lock/Unlock toggle
                        let lock_label = if prim_locked { "🔓 Unlock" } else { "🔒 Lock" };
                        if ui.button(lock_label).clicked() { self.push_cmd(AppCommand::SetLocked(prim_uid, !prim_locked)); ui.close_menu(); }
                        // Instance operations
                        if is_instance {
                            ui.separator();
                            if ui
                                .button("◇ Make Unique / Break Link")
                                .on_hover_text("Instance → independent (stop following master).")
                                .clicked()
                            {
                                self.make_instance_unique(i);
                                ui.close_menu();
                            }
                            // Go to master
                            let master_uid = match prim_instance_master_id { Some(id) => id, None => 0 };
                            if ui.button("★ Select Master").clicked() {
                                self.select_single_unified(SceneItemId::Prim(master_uid));
                                self.mark_dirty(DirtyFlags::RENDER);
                                ui.close_menu();
                            }
                        }
                        if !is_instance {
                            if ui.button("◇ Create Instance").clicked() {
                                self.select_single_unified(SceneItemId::Prim(prim_uid));
                                crate::ops::instance::create::run(self); ui.close_menu();
                            }
                            if ui
                                .button("◇ Instance Into All Bodies")
                                .on_hover_text(
                                    "Linked copy of this prim in every other top-level body \
                                     (same shape/boolean; edit master to update all).",
                                )
                                .clicked()
                            {
                                self.select_single_unified(SceneItemId::Prim(prim_uid));
                                self.instance_into_all_bodies();
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if ui.button("✏ Rename").clicked() { self.trigger_rename(SceneNodeId::Prim(prim_uid), &prim_name); ui.close_menu(); }
                        if ui.button("⎘ Duplicate").clicked() {
                            // Multi-selection aware: if the right-clicked prim is PART of the
                            // current selection, duplicate the WHOLE selection (same multi path
                            // as Shift+D). Only when it is NOT selected do we single-select it
                            // first (today's select-then-duplicate-single behavior).
                            if !self.sel.usel.selected.contains(&SceneItemId::Prim(prim_uid)) {
                                self.select_single_unified(SceneItemId::Prim(prim_uid));
                            }
                            self.duplicate_selected(); ui.close_menu();
                        }
                        if ui.button("🗑 Delete").clicked() {
                            self.select_single_unified(SceneItemId::Prim(prim_uid));
                            self.delete_selected(); ui.close_menu();
                        }
                    });

                    // Drag highlight while this prim is the live drag source
                    if self.outliner.dragging == Some(i) {
                        ui.painter().rect_filled(resp.rect, 2.0, egui::Color32::from_rgba_unmultiplied(255, 150, 30, 80));
                    }
                }
            },
        );
        // Drag reorder — full-row press (not just the label text rect).
        let ptr_pos = ctx.input(|inp| inp.pointer.hover_pos().unwrap_or_default());
        if row_rect.contains(ptr_pos)
            && ctx.input(|inp| inp.pointer.button_pressed(egui::PointerButton::Primary))
            && self.outliner.renaming.is_none()
        {
            self.outliner.drag_start_pos = Some(ptr_pos);
            self.outliner.drag_source = Some(i);
        }
        if let Some(start_pos) = self.outliner.drag_start_pos {
            if let Some(current_pos) = ctx.input(|inp| inp.pointer.hover_pos()) {
                let delta = (current_pos - start_pos).length();
                if delta > 4.0 && self.outliner.dragging.is_none() {
                    if let Some(src) = self.outliner.drag_source {
                        let shift = ctx.input(|inp| inp.modifiers.shift);
                        let src_uid = self.scene.primitives[src].prim_uid;
                        if self.sel.usel.selected.contains(&SceneItemId::Prim(self.scene.primitives[src].prim_uid)) {
                            self.outliner.dragging = Some(src);
                        } else if shift {
                            self.toggle_select_unified(SceneItemId::Prim(src_uid));
                            self.outliner.dragging = Some(src);
                        } else {
                            self.select_single_unified(SceneItemId::Prim(src_uid));
                            self.outliner.dragging = Some(src);
                        }
                    }
                }
            }
            if ctx.input(|inp| inp.pointer.button_released(egui::PointerButton::Primary)) {
                self.outliner.drag_start_pos = None;
                self.outliner.drag_source = None;
            }
        }
        // Paint row background AFTER (no interaction stealing)
        if row_bg != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(row_rect.expand2(egui::vec2(0.0, 1.0)), 3.0, row_bg);
        }
        // Hover feedback (blue tint, Principle 4)
        if !sel && !act {
            if ui.rect_contains_pointer(row_rect) {
                ui.painter().rect_filled(row_rect, 3.0, egui::Color32::from_rgba_unmultiplied(77, 159, 255, 25));
            }
        }
        // Scroll-to for "Find in Outliner" (F key)
        self.check_scroll_to(ui, row_rect, SceneNodeId::Prim(prim_uid));
        ui.add_space(1.0);
        row_rect
    }

    // ================================================================

}
