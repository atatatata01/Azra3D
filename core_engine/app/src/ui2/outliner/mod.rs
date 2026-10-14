//! Outliner — catalog + activation (no dock/shell chrome here).
//!
//! Split for the 500-line rule: types / build / dnd / input / actions / menu;
//! this file owns `ui2_outliner` orchestration + row paint only.

mod types;
mod build;
mod dnd;
mod input;
mod actions;
mod menu;

// Re-export free items so `outliner::OlRow` / `outliner::ol_context_menu` etc. stay reachable.
// Impl methods on FourthLayerApp (build/dnd/input/actions) need no re-export.
pub(crate) use types::*;
#[allow(unused_imports)] // available for external call sites; not all used inside this file
pub(crate) use build::{
    ol_material_maps, ol_node_menu, ol_node_tex, ol_prim_tex, ol_rename_target,
};
pub(crate) use menu::*;

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_outliner(&mut self, ui: &mut egui::Ui) {
        // Latch pointer-over for next-frame keyboard (Space must not double-fire with cancel/deselect).
        self.ui2.ol_pointer_over = ui.ui_contains_pointer();
        let mut do_add: Option<u8> = None;
        // Toolbar: [+ Add ▾]  [⊞/⊟ expand] ·····  [filter]
        let mut expand_all = false;
        let mut collapse_all = false;
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let add_btn = egui::Button::new(
                        egui::RichText::new(format!("{}  Add  {}", ph::PLUS, ph::CARET_DOWN))
                            .size(12.0).color(ui_theme::TEXT))
                        .fill(ui_theme::SURFACE_CONTROL)
                        .stroke(egui::Stroke::new(1.0, ui_theme::BORDER))
                        .rounding(egui::Rounding::same(ui_theme::RADIUS_BUTTON as u8));
                    egui::menu::menu_custom_button(ui, add_btn, |ui| {
                        ui.set_min_width(180.0);
                        ui.label(egui::RichText::new("CREATE").size(9.5).color(ui_theme::TEXT_SECTION));
                        if ui.button(format!("{}   SDF Body", ph::CIRCLES_THREE)).clicked() { do_add = Some(1); ui.close_menu(); }
                        if ui.button(format!("{}   Material", ph::PAINT_BRUSH)).clicked() { do_add = Some(6); ui.close_menu(); }
                        ui.separator();
                        ui.label(egui::RichText::new("LIGHTS").size(9.5).color(ui_theme::TEXT_SECTION));
                        if ui.button(format!("{}   Directional Light", ph::SUN)).clicked() { do_add = Some(2); ui.close_menu(); }
                        if ui.button(format!("{}   Point Light", ph::LIGHTBULB)).clicked() { do_add = Some(3); ui.close_menu(); }
                        if ui.button(format!("{}   Spot Light", ph::FLASHLIGHT)).clicked() { do_add = Some(5); ui.close_menu(); }
                        ui.separator();
                        if ui.button(format!("{}   Camera", ph::VIDEO_CAMERA)).clicked() { do_add = Some(4); ui.close_menu(); }
                    });
                    // Expand / collapse all tree branches
                    let exp_btn = ui.add(egui::Button::new(
                        egui::RichText::new(ph::TREE_STRUCTURE).size(14.0).color(ui_theme::TEXT_DIM))
                        .fill(egui::Color32::TRANSPARENT)
                        .frame(false))
                        .on_hover_text("Expand all");
                    if exp_btn.clicked() { expand_all = true; }
                    let col_btn = ui.add(egui::Button::new(
                        egui::RichText::new(ph::LIST).size(14.0).color(ui_theme::TEXT_DIM))
                        .fill(egui::Color32::TRANSPARENT)
                        .frame(false))
                        .on_hover_text("Collapse all");
                    if col_btn.clicked() { collapse_all = true; }
                    // Kind filter funnel (mockup)
                    let filter_armed = !self.ui2.ol_kinds.is_empty();
                    let funnel_col = if filter_armed { ui_theme::ACCENT_LIGHT } else { ui_theme::TEXT_DIM };
                    let funnel = ui.add(egui::Button::new(
                        egui::RichText::new(ph::FUNNEL).size(14.0).color(funnel_col))
                        .fill(if filter_armed { ui_theme::SURFACE_CONTROL } else { egui::Color32::TRANSPARENT })
                        .frame(false))
                        .on_hover_text("Filter by type");
                    let fpid = ui.make_persistent_id("ol_kind_funnel");
                    if funnel.clicked() { ui.memory_mut(|m| m.toggle_popup(fpid)); }
                    egui::popup_below_widget(ui, fpid, &funnel, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                        ui.set_min_width(180.0);
                        ui.label(egui::RichText::new("SHOW TYPES").size(9.5).color(ui_theme::TEXT_SECTION));
                        ui.small("(none checked = show all)");
                        for k in OlKind::ALL {
                            let mut on = self.ui2.ol_kinds.contains(&k);
                            if ui.checkbox(&mut on, format!("{}  {}", k.icon(), k.label())).changed() {
                                if on { self.ui2.ol_kinds.insert(k); }
                                else { self.ui2.ol_kinds.remove(&k); }
                            }
                        }
                        ui.separator();
                        if ui.button("Show all types").clicked() {
                            self.ui2.ol_kinds.clear();
                            ui.close_menu();
                        }
                    });
                    // Search field pushed right (funnel inside opens the same kind filter as toolbar)
                    let sb_w = 158.0f32.min(ui.available_width());
                    let slack = ui.available_width() - sb_w;
                    if slack > 0.0 { ui.add_space(slack); }
                    let kinds_armed = !self.ui2.ol_kinds.is_empty();
                    ol_searchbox(ui, &mut self.ui2.ol_filter, sb_w, kinds_armed, &mut self.ui2.ol_kinds);
                });
            });

        let mut toggle_col: Option<OlCol> = None;
        // Grid is derived from the width we were GIVEN: `30 | max(168,1fr) | 32*N | 44`.
        // Info columns drop right→left so Name never goes under 168 (the artifact's fitCols).
        let avail_w = ui.available_width();
        let n_cols = ol_fit_cols(avail_w, self.ui2.ol_cols.len());
        let cols: Vec<OlCol> = self.ui2.ol_cols.iter().copied().take(n_cols).collect();
        let name_w = (avail_w - OL_EYE_W - OL_INFO_COL_W * n_cols as f32 - OL_LOCK_W).max(OL_NAME_MIN);

        // Column header: [eye] Name ... [info icons] [empty]
        let (hrect, _) = ui.allocate_exact_size(egui::vec2(avail_w, 22.0), egui::Sense::hover());
        let name_l = hrect.left() + OL_EYE_W;
        {
            let p = ui.painter();
            p.rect_filled(hrect, 0.0, ui_theme::SURFACE_HEADER);
            p.hline(hrect.x_range(), hrect.top() + 0.5, egui::Stroke::new(1.0, ui_theme::SEAM));
            p.hline(hrect.x_range(), hrect.bottom() - 0.5, egui::Stroke::new(1.0, ui_theme::SEAM));
            let cy = hrect.center().y;
            p.text(egui::pos2(hrect.left() + OL_EYE_W * 0.5, cy), egui::Align2::CENTER_CENTER,
                ph::EYE, egui::FontId::proportional(13.0), ui_theme::TEXT_SECTION);
            p.vline(name_l, hrect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));
            p.text(egui::pos2(name_l + 9.0, cy), egui::Align2::LEFT_CENTER,
                "Name", egui::FontId::proportional(ui_theme::FONT_CAPS), ui_theme::TEXT_SECTION);
            p.vline(name_l + name_w, hrect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));
            for (i, c) in cols.iter().enumerate() {
                let x = name_l + name_w + i as f32 * OL_INFO_COL_W;
                p.text(egui::pos2(x + OL_INFO_COL_W * 0.5, cy), egui::Align2::CENTER_CENTER,
                    c.icon(), egui::FontId::proportional(13.0), ui_theme::TEXT_DIM);
                p.vline(x + OL_INFO_COL_W, hrect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));
                // Hover tip for column meaning
                let cell = egui::Rect::from_min_size(
                    egui::pos2(x, hrect.top()),
                    egui::vec2(OL_INFO_COL_W, hrect.height()),
                );
                let hr = ui.interact(cell, ui.id().with(("ol_hdr_col", *c as u8)), egui::Sense::hover());
                hr.on_hover_text(c.tip());
            }
            // trailing header cell is deliberately EMPTY — no label above the lock.
        }
        {
            let name_hdr = egui::Rect::from_min_max(
                egui::pos2(name_l, hrect.top()), egui::pos2(name_l + name_w, hrect.bottom()));
            let r = ui.interact(name_hdr, ui.id().with("ol_hdr_name"), egui::Sense::click());
            let pid = ui.make_persistent_id("ol_colmenu");
            if r.secondary_clicked() { ui.memory_mut(|m| m.toggle_popup(pid)); }
            if r.hovered() { r.clone().on_hover_text("Right-click to add / remove columns"); }
            egui::popup_below_widget(ui, pid, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(170.0);
                ui.label(egui::RichText::new("COLUMNS").size(9.5).color(ui_theme::TEXT_SECTION));
                for c in OlCol::ALL {
                    let mut on = self.ui2.ol_cols.contains(&c);
                    if ui
                        .checkbox(&mut on, format!("{}  {}", c.icon(), c.label()))
                        .on_hover_text(c.tip())
                        .changed()
                    {
                        toggle_col = Some(c);
                    }
                }
            });
        }

        let manifold_col = self.ui2.ol_cols.contains(&OlCol::Manifold);
        if manifold_col {
            self.ol_refresh_manifold_cache();
        }
        // Rebuild flattened rows only when structure/filter/expand changes — not every frame.
        // Selection highlight + manifold dots are patched below (must not force a 10k walk).
        let need_ol_rebuild = self.outliner.outliner_dirty
            || self.ui2.ol_rows_cache.is_empty()
            || self.ui2.ol_rows_filter != self.ui2.ol_filter
            || self.ui2.ol_rows_kinds != self.ui2.ol_kinds
            || self.ui2.ol_rows_collapsed != self.ui2.ol_collapsed;
        if need_ol_rebuild {
            let built = self.ol_build();
            self.ui2.ol_rows_cache = built;
            self.ui2.ol_rows_filter = self.ui2.ol_filter.clone();
            self.ui2.ol_rows_kinds = self.ui2.ol_kinds.clone();
            self.ui2.ol_rows_collapsed = self.ui2.ol_collapsed.clone();
            // Structural rebuild consumed. Selection-only paths that only need highlight
            // must not set outliner_dirty (or they re-walk 10k); highlight is patched below.
            self.outliner.outliner_dirty = false;
        }
        {
            let active = self.scene.active_body_nid;
            let sel = &self.sel.usel.selected;
            // Manifold cache is refreshed every frame when the column is on, but rows only
            // rebuild on hierarchy dirty. Patch the manifold cell so a knife/boolean that
            // breaks the mesh flips green→red without needing a structural outliner rebuild
            // (2026-08-05: column stayed green forever after first paint).
            let man_cache = if manifold_col {
                Some(&self.ui2.ol_manifold_cache)
            } else {
                None
            };
            for r in &mut self.ui2.ol_rows_cache {
                r.selected = match r.item {
                    OlItem::Node(n) => sel.contains(&SceneItemId::Node(NodeId(n))),
                    OlItem::Prim(u) => sel.contains(&SceneItemId::Prim(u)),
                };
                r.active_body = matches!(r.item, OlItem::Node(n) if NodeId(n) == active);
                if let Some(cache) = man_cache {
                    r.manifold = match r.item {
                        OlItem::Node(n) => cache.get(&n).map(|&(_, ok)| ok),
                        OlItem::Prim(_) => None,
                    };
                } else {
                    r.manifold = None;
                }
            }
        }
        // Take cache for the paint pass (avoids clone of 10k rows); restored at end.
        let rows = std::mem::take(&mut self.ui2.ol_rows_cache);
        let mut sel_click: Option<(SceneItemId, SelMode)> = None;
        let mut toggle_vis: Option<(OlItem, bool)> = None;
        let mut toggle_lock: Option<(OlItem, bool)> = None;
        let mut toggle_exp: Option<u64> = None;
        // Rename: `renaming_now` is the row being edited (read once — the draw must not see it
        // change mid-list); `do_rename` starts one; `rename_done` carries the widget's verdict
        // out of the draw. Nothing mutates the scene inside the loop — see `sel_click`.
        let renaming_now = self.outliner.renaming;
        let mut do_rename: Option<(SceneNodeId, String)> = None;
        let mut rename_done: Option<Option<String>> = None;
        // Context menu: the picked action, and the row to adopt into the selection on open.
        let mut ctx_act: Option<OlAct> = None;
        let mut ctx_sel: Option<SceneItemId> = None;
        // `draw_rename_widget` needs the Context while `ui` is borrowed mutably; Context is an
        // Arc handle, so this clone is a refcount bump, not a copy of anything.
        let ectx = ui.ctx().clone();
        let keys = OlKeys {
            dup: self.keybindings.label(ACT_DUPLICATE),
            del: self.keybindings.label(ACT_DELETE),
            hide: self.keybindings.label(ACT_HIDE),
            rename: self.keybindings.label(ACT_RENAME),
            focus: self.keybindings.label(ACT_FOCUS),
        };

        // Scrollbar palette (13px lane, WELL track). Scoped to this Ui: the tree is all custom
        // painting, so re-pointing the widget ladder here cannot restyle any real widget.
        {
            let w = &mut ui.style_mut().visuals.widgets;
            for st in [&mut w.inactive, &mut w.hovered, &mut w.active] { st.corner_radius = egui::Rounding::same(7); }
            w.inactive.bg_fill = ui_theme::SURFACE_CONTROL;
            w.hovered.bg_fill = ui_theme::BORDER_HOVER;
            w.active.bg_fill = ui_theme::ACCENT_DIM;
        }
        // DnD: clear drop targets each frame so hit-tests rebuild cleanly.
        let drag_live = self.outliner.dragging.is_some() || self.outliner.dragging_body.is_some();
        if drag_live {
            self.outliner.drop_insert = None;
            self.outliner.drop_reparent = None;
            self.outliner.drop_target = None;
            self.outliner.drop_on_prim = false;
            self.outliner.drop_root_y = None;
            self.outliner.body_drop_target_nid = None;
            self.outliner.body_drop_before = false;
            self.outliner.body_drop_nest = false;
            self.outliner.body_drop_root_y = None;
            self.outliner.drop_visual = None;
            self.outliner.hover_expand_hit = false;
        }

        // Quick-bar strip (Duplicate / Res− / Res+) is reserved BEFORE the scroll area —
        // the tree gets everything above it and can never steal the strip.
        let tree_h = (ui.available_height() - OL_QUICKBAR_H).max(0.0);
        let tree_w = ui.available_width();
        let ptr_pos = ui.ctx().pointer_hover_pos();
        let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
        let primary_pressed = ui.ctx().input(|i| i.pointer.primary_pressed());
        let ol_scroll = egui::ScrollArea::vertical()
            .id_salt("v2_outliner_scroll")
            .auto_shrink([false, false])
            .max_height(tree_h)
            .show(ui, |ui| {
            ui.set_min_width(tree_w);
            ui.set_max_width(tree_w);
            ui.spacing_mut().item_spacing.y = 0.0;
            let row_w = ui.available_width();
            // Auto-scroll near edges while dragging
            let ctx = ui.ctx().clone();
            self.outliner_drag_autoscroll(&ctx, ui);

            // C-scale: virtualize the row paint. At 10k bodies, painting every row was ~43 ms
            // of `lay=` (scoreboard) — more than raster dispatch. Reserve space for off-screen
            // rows with add_space; only allocate + paint the viewport window (+1-row margin).
            let n_rows = rows.len();
            let origin_y = ui.cursor().min.y;
            let clip = ui.clip_rect();
            let mut first = if n_rows > 0 && clip.min.y > origin_y {
                (((clip.min.y - origin_y) / OL_ROW_H).floor() as i32 - 1).max(0) as usize
            } else {
                0
            };
            first = first.min(n_rows);
            let mut last = {
                let vis = ((clip.height() / OL_ROW_H).ceil() as usize).saturating_add(3);
                (first + vis).min(n_rows)
            };
            // Keep the rename target painted even if it scrolled out of the virtual window.
            if let Some(t) = renaming_now {
                if let Some(ri) = rows.iter().position(|r| r.rename == Some(t)) {
                    first = first.min(ri);
                    last = last.max(ri + 1).min(n_rows);
                }
            }
            if first > 0 {
                ui.add_space(first as f32 * OL_ROW_H);
            }

            for ri in first..last {
                let row = &rows[ri];
                let (rect, row_resp) = ui.allocate_exact_size(egui::vec2(row_w, OL_ROW_H), egui::Sense::click());
                let hov = ui.rect_contains_pointer(rect);
                let cy = rect.center().y;
                // MatMap rows share OlItem::Node(parent) with the material — salt the key so
                // interact ids / rename focus never collide with the parent row.
                let key: u64 = match (&row.item, &row.menu) {
                    (OlItem::Prim(u), _) => 0x8000_0000_0000_0000 | *u as u64,
                    (OlItem::Node(n), OlMenu::MatMap { kind, .. }) => {
                        0xC000_0000_0000_0000 | *n ^ ((*kind as u8 as u64) << 48)
                    }
                    (OlItem::Node(n), _) => *n,
                };
                let name_l = rect.left() + OL_EYE_W;
                let acts_l = rect.right() - OL_LOCK_W;

                // background: zebra → hover → selected. Zebra is a READING AID only — must stay far
                // below hover/selected or it reads as a stuck highlight / harsh "piano keys".
                // Owner: white@3 too loud, white@2 still too contrasted → solid +1 above panel
                // (0x1c→0x1d) so the step is barely perceptible and never washes the row.
                if ri % 2 == 1 {
                    ui.painter().rect_filled(rect, 0.0, ui_theme::OL_ZEBRA);
                }
                if row.selected {
                    ui.painter().rect_filled(rect, 0.0, ui_theme::SELECTED_ROW);
                } else if hov {
                    ui.painter().rect_filled(rect, 0.0, ui_theme::SURFACE_HEADER);
                }
                // The 2px bar is PAINTED over the row, not laid out: the artifact reserves it as a
                // transparent border (`.trow{border-left:2px solid transparent}`) so the state never
                // shifts content — painting keeps that guarantee AND keeps the header separators
                // aligned with the rows'.
                //
                // That gutter is ONE channel, so the two states share it and ACTIVE BODY wins when a
                // row is both: "selected" is ALSO carried by the SELECTED_ROW fill, whereas "this is
                // the body you are editing" has no other tell. Amber, never the violet accent —
                // violet means selection/focus everywhere in this UI.
                let bar = if row.active_body { Some(ui_theme::ACCENT_AMBER) }
                          else if row.selected { Some(ui_theme::ACCENT) }
                          else { None };
                if let Some(c) = bar {
                    ui.painter().vline(rect.left() + 1.0, rect.y_range(), egui::Stroke::new(2.0, c));
                }

                // eye — always TEXT_DIM (no hover brighten); hidden goes red at full opacity
                let (eye_ch, eye_col) = if row.visible { (ph::EYE, ui_theme::TEXT_DIM) }
                                        else { (ph::EYE_SLASH, ui_theme::DANGER) };
                ui.painter().text(egui::pos2(rect.left() + OL_EYE_W * 0.5, cy), egui::Align2::CENTER_CENTER,
                    eye_ch, egui::FontId::proportional(14.0), eye_col);
                ui.painter().vline(name_l, rect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));
                // End of the Name column. The header draws this divider too — rows MUST draw every
                // divider the header does, at the same x, or the grid reads as broken.
                ui.painter().vline(name_l + name_w, rect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));

                // tree connectors: guide x = 13 + (level-1)*17, row midpoint = ROW_H/2
                let stroke = egui::Stroke::new(1.0, OL_TREE_LINE);
                for level in 1..=row.depth {
                    let x = name_l + OL_GUIDE_X0 + (level - 1) as f32 * OL_INDENT;
                    if level == row.depth {
                        ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, cy)], stroke);
                        ui.painter().line_segment([egui::pos2(x, cy), egui::pos2(x + OL_ELBOW_W, cy)], stroke);
                        if !row.is_last {
                            ui.painter().line_segment([egui::pos2(x, cy), egui::pos2(x, rect.bottom())], stroke);
                        }
                    } else if row.prefix.get(level - 1).copied().unwrap_or(false) {
                        ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], stroke);
                    }
                }

                // name cell flow: [chev 12] 7 [badge|icon 16] 7 [name…]
                let content_x = name_l + OL_INDENT_BASE + row.depth as f32 * OL_INDENT;
                let name_clip = egui::Rect::from_min_max(
                    egui::pos2(name_l + 1.0, rect.top()), egui::pos2(name_l + name_w, rect.bottom()));
                let np = ui.painter().with_clip_rect(name_clip);
                if row.has_children {
                    let cv = if row.expanded { ph::CARET_DOWN } else { ph::CARET_RIGHT };
                    np.text(egui::pos2(content_x + OL_CHEV_W * 0.5, cy), egui::Align2::CENTER_CENTER,
                        cv, egui::FontId::proportional(11.0), ui_theme::TEXT_FAINT);
                }
                let slot_l = content_x + OL_CHEV_W + OL_GAP;
                let slot_c = egui::pos2(slot_l + OL_ICON_W * 0.5, cy);
                let dim = if row.visible { 1.0 } else { 0.42 };
                match row.badge {
                    // Badge REPLACES the icon. No resting background; SURFACE_CONTROL on hover.
                    Some((bc, glyph)) => {
                        let br = egui::Rect::from_center_size(slot_c, egui::Vec2::splat(OL_ICON_W));
                        if ui.rect_contains_pointer(br) {
                            np.rect_filled(br, 4.0, ui_theme::SURFACE_CONTROL);
                        }
                        np.text(slot_c, egui::Align2::CENTER_CENTER, glyph,
                            egui::FontId::proportional(13.0), bc.gamma_multiply(dim));
                    }
                    None => {
                        np.text(slot_c, egui::Align2::CENTER_CENTER, row.icon,
                            egui::FontId::proportional(15.0), row.color.gamma_multiply(dim));
                    }
                }
                // Renaming REPLACES the name text only — chevron, connectors and icon stay put,
                // so the row does not jump when the field opens (the artifact's `tx.replaceWith`).
                let is_renaming = row.rename.is_some() && row.rename == renaming_now;
                let text_l = slot_l + OL_ICON_W + OL_GAP;
                if is_renaming {
                    // Full name-column width so the field is always usable (narrow panels used to
                    // clip the TextEdit to a few pixels → rename looked dead).
                    let te_right = (name_l + name_w - 4.0).max(text_l + 48.0);
                    let te_rect = egui::Rect::from_min_max(
                        egui::pos2(text_l - 3.0, rect.top() + 2.0),
                        egui::pos2(te_right, rect.bottom() - 2.0));
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(te_rect), |ui| {
                        // `.trow .nm input.rename` — WELL fill, 1px violet border, r4, 12.5px.
                        // A focused TextEdit takes its fill from `extreme_bg_color` and its border
                        // from `selection.stroke`, so those are the two knobs. Scoped to this Ui
                        // (one TextEdit), so it cannot restyle anything else.
                        ui.set_min_width(te_rect.width().max(48.0));
                        ui.spacing_mut().interact_size.y = OL_ROW_H - 6.0;
                        let v = &mut ui.style_mut().visuals;
                        v.extreme_bg_color = ui_theme::SURFACE_WELL;
                        v.selection.stroke = egui::Stroke::new(1.0, ui_theme::ACCENT);
                        v.override_text_color = Some(ui_theme::TEXT);
                        for st in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                            st.corner_radius = egui::Rounding::same(4);
                        }
                        ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.5));
                        // Stable id so focus survives the virtualized list rebuild.
                        let te_id = ui.id().with(("ol2_rename", key));
                        ui.push_id(te_id, |ui| {
                            match crate::outliner_actions::draw_rename_widget(
                                ui, &ectx,
                                &mut self.outliner.rename_buf,
                                &mut self.outliner.rename_grace_frames,
                            ) {
                                crate::outliner_actions::RenameResult::Commit(n) => rename_done = Some(Some(n)),
                                crate::outliner_actions::RenameResult::Cancel => rename_done = Some(None),
                                crate::outliner_actions::RenameResult::Editing => {}
                            }
                        });
                    });
                } else {
                    let (nm_size, nm_bold, nm_col) = ol_name_style(row);
                    let fam = if nm_bold { egui::FontFamily::Name("azra_semibold".into()) } else { egui::FontFamily::Proportional };
                    np.text(egui::pos2(text_l, cy), egui::Align2::LEFT_CENTER,
                        &row.name, egui::FontId::new(nm_size, fam), nm_col.gamma_multiply(dim));
                }

                // info columns
                for (i, c) in cols.iter().enumerate() {
                    let x = name_l + name_w + i as f32 * OL_INFO_COL_W;
                    let ccx = x + OL_INFO_COL_W * 0.5;
                    let cell_r = egui::Rect::from_min_size(
                        egui::pos2(x, rect.top()),
                        egui::vec2(OL_INFO_COL_W, rect.height()),
                    );
                    let cell = ol_cell(*c, row);
                    match cell {
                        OlCell::Empty => {}
                        OlCell::Num(n) => {
                            ui.painter().text(egui::pos2(ccx, cy), egui::Align2::CENTER_CENTER,
                                n.to_string(), egui::FontId::monospace(10.0), ui_theme::TEXT_SECONDARY);
                        }
                        OlCell::Dot(col) => {
                            ui.painter().circle_filled(egui::pos2(ccx, cy), 3.5, col);
                        }
                        OlCell::Warn(col) => {
                            // Explicit warning triangle — not a green "status ok" lookalike.
                            ui.painter().text(
                                egui::pos2(ccx, cy),
                                egui::Align2::CENTER_CENTER,
                                c.icon(),
                                egui::FontId::proportional(12.0),
                                col,
                            );
                        }
                    }
                    // Hover tip for manifold status (green = ok, warn = non-manifold).
                    if matches!(c, OlCol::Manifold) && !matches!(cell, OlCell::Empty) {
                        let tip = match row.manifold {
                            Some(true) => "Manifold — mesh is closed / edge-manifold",
                            Some(false) => "Non-manifold — broken twins, multi-face edges, or bowtie verts",
                            None => "",
                        };
                        if !tip.is_empty() {
                            let key = match row.item {
                                OlItem::Node(n) => n,
                                OlItem::Prim(u) => u as u64,
                            };
                            let resp = ui.interact(
                                cell_r,
                                ui.id().with(("ol_man", key, i)),
                                egui::Sense::hover(),
                            );
                            resp.on_hover_text(tip);
                        }
                    }
                    ui.painter().vline(x + OL_INFO_COL_W, rect.y_range(), egui::Stroke::new(1.0, ui_theme::SEAM));
                }

                // acts column (44) — a 22x20 chip, revealed on hover OR when locked
                let lock_r = egui::Rect::from_min_max(
                    egui::pos2(acts_l, rect.top()), egui::pos2(rect.right(), rect.bottom()));
                let chip = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 6.0 - OL_LOCK_BTN.x * 0.5, cy), OL_LOCK_BTN);
                if row.locked || hov {
                    let chip_hov = ui.rect_contains_pointer(chip);
                    let (glyph, fg) = if row.locked {
                        let bg = if chip_hov { ui_theme::ACCENT_DIM } else { ui_theme::SELECTED_ROW };
                        ui.painter().rect_filled(chip, ui_theme::RADIUS_CONTROL, bg);
                        (ph::LOCK_SIMPLE, if chip_hov { egui::Color32::WHITE } else { ui_theme::ACCENT_LIGHT })
                    } else {
                        if chip_hov { ui.painter().rect_filled(chip, ui_theme::RADIUS_CONTROL, ui_theme::SURFACE_CONTROL); }
                        (ph::LOCK_SIMPLE_OPEN, if chip_hov { ui_theme::TEXT_SECONDARY } else { ui_theme::TEXT_FAINT })
                    };
                    ui.painter().text(chip.center(), egui::Align2::CENTER_CENTER, glyph,
                        egui::FontId::proportional(13.0), fg);
                }

                // Scroll-to-selected (F-key / frame_selected) — honor pending target.
                if let Some(t) = row.rename {
                    self.check_scroll_to(ui, rect, t);
                }
                if let OlItem::Node(n) = row.item {
                    let nid = NodeId(n);
                    if self.outliner.scroll_to_node == Some(SceneNodeId::Node(nid))
                        || self.outliner.scroll_to_node == Some(SceneNodeId::Body(nid))
                    {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                        self.outliner.scroll_to_node = None;
                    }
                }
                if let OlItem::Prim(u) = row.item {
                    if self.outliner.scroll_to_node == Some(SceneNodeId::Prim(u)) {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                        self.outliner.scroll_to_node = None;
                    }
                }

                // interactions (eye / lock / chevron take priority over row select).
                // The row being renamed owns its clicks — the TextEdit needs them for caret
                // placement and drag-select, and a stray row-click would fight its focus.
                if is_renaming { continue; }
                let is_mat_map = matches!(row.menu, OlMenu::MatMap { .. });
                let is_material = matches!(row.menu, OlMenu::Material { .. });
                let eye_r = egui::Rect::from_min_size(rect.min, egui::vec2(OL_EYE_W, OL_ROW_H));
                let eye_resp = ui.interact(eye_r, egui::Id::new(("ol_eye", key)), egui::Sense::click());
                let lock_resp = ui.interact(lock_r, egui::Id::new(("ol_lock", key)), egui::Sense::click());
                let chev_clicked = row.has_children && {
                    let chev_r = egui::Rect::from_min_size(egui::pos2(content_x, rect.top()), egui::vec2(OL_CHEV_W, OL_ROW_H));
                    ui.interact(chev_r, egui::Id::new(("ol_chev", key)), egui::Sense::click()).clicked()
                };
                // The eye owns its own right-click menu, so it must claim the secondary click
                // before the row's menu sees it (the eye rect is INSIDE the row rect). egui gives
                // the click to the LAST widget registered at that point, and both eye_resp and
                // lock_resp are interacted after row_resp — so exactly one menu can open.
                // Not on the Scene root: it has no sibling group to isolate against.
                // Map-channel rows have no visibility of their own.
                // Materials DO own visibility (SceneNode.visible → resolve falls back when hidden).
                if !row.is_root && !is_mat_map {
                    eye_resp.azra_context_menu(|ui| ol_eye_menu(ui, row, &keys, &mut ctx_act, &mut toggle_vis));
                }
                if !matches!(row.menu, OlMenu::Root) {
                    row_resp.azra_context_menu(|ui| {
                        ol_context_menu(ui, row, &keys, &mut ctx_act, &mut toggle_vis, &mut toggle_lock)
                    });
                    // The trailing acts column hit-tests as the lock button across its full 44px,
                    // so without this a right-click there would land on lock_resp and open NOTHING
                    // — a dead strip down the edge of every row.
                    if !is_mat_map {
                        lock_resp.azra_context_menu(|ui| {
                            ol_context_menu(ui, row, &keys, &mut ctx_act, &mut toggle_vis, &mut toggle_lock)
                        });
                    }
                    // Right-click opens the menu only — it is NOT a viewport empty-click and
                    // must never replace / drop the current selection (owner 2026-08-06).
                    // Selection-driven verbs (Dup / Del / Group) use whatever is already
                    // selected. Row-specific verbs (bool / shape / …) carry the row id in
                    // OlAct. Adopt a row ONLY when selection is empty so those verbs have a
                    // target; if anything is selected, leave it alone.
                    // Map-channel rows adopt the PARENT material when selection is empty.
                    if row_resp.secondary_clicked() || (!is_mat_map && lock_resp.secondary_clicked()) {
                        if let OlMenu::MatMap { parent, .. } = row.menu {
                            ctx_sel = Some(SceneItemId::Node(parent));
                        } else {
                            ctx_sel = Some(row.item.sel_id());
                        }
                    }
                }
                // Alt+click the eye = isolate. Checked BEFORE the plain toggle so alt+click does
                // not ALSO flip this row's own visibility. The root has no sibling group, so it
                // keeps a plain toggle.
                if is_mat_map {
                    // Map channel: click focuses editor; no eye/lock/drag.
                    if row_resp.clicked() {
                        if let OlMenu::MatMap { parent, kind } = row.menu {
                            let now = ui.input(|i| i.time);
                            ctx_act = Some(OlAct::FocusMapChannel(parent, kind, now));
                        }
                    }
                } else if eye_resp.clicked() && !row.is_root && !is_material && ui.input(|i| i.modifiers.alt) {
                    // Alt+eye isolate is body/prim only (material has no sibling group).
                    ctx_act = Some(OlAct::Solo(row.item));
                }
                else if eye_resp.clicked() && !row.is_root {
                    // Bodies, prims, AND materials (owner: eye on Material1 was not clickable).
                    toggle_vis = Some((row.item, row.visible));
                }
                else if lock_resp.clicked() && !is_material { toggle_lock = Some((row.item, row.locked)); }
                else if lock_resp.clicked() && is_material {
                    // Material nodes carry SceneNode.locked — same toggle path as bodies.
                    toggle_lock = Some((row.item, row.locked));
                }
                else if chev_clicked { if let OlItem::Node(n) = row.item { toggle_exp = Some(n); } }
                // Primary only — secondary is the context menu and must not also run
                // select_single (that was "RMB drops multi-select" / viewport-like deselect).
                else if row_resp.clicked() && !row_resp.secondary_clicked() {
                    // Double-click = rename. NEVER `resp.double_clicked()` — egui misfires it on
                    // an already-selected row (CLAUDE "Outliner Double-Click"); the time-based
                    // `check_double_click` is the ONLY sanctioned gate. It must run on EVERY
                    // click to stamp the timestamp, so it is evaluated before any selection work.
                    let dbl = row.rename.is_some()
                        && crate::outliner_actions::check_double_click(
                            ui, egui::Id::new(("ol2_name_dbl", key)));
                    match (dbl, row.rename) {
                        (true, Some(t)) => do_rename = Some((t, row.name.clone())),
                        _ => {
                            let (shift, ctrl) = ui.input(|i|
                                (i.modifiers.shift, i.modifiers.command || i.modifiers.ctrl));
                            let mode = if shift { SelMode::Range }
                                       else if ctrl { SelMode::Toggle }
                                       else { SelMode::Single };
                            sel_click = Some((row.item.sel_id(), mode));
                            self.ui2.ol_kb_row = Some(ri);
                        }
                    }
                }

                // Library folder-material drop onto SDF / poly body rows (v1 parity).
                if let OlItem::Node(n) = row.item {
                    if matches!(
                        row.menu,
                        OlMenu::SdfBody { .. }
                            | OlMenu::Poly
                            | OlMenu::Convertible
                            | OlMenu::Generic
                    ) {
                        let nid = NodeId(n);
                        let droppable = self.scene.scene_graph.get(nid).map(|node| {
                            matches!(
                                &node.payload,
                                NodePayload::SdfBody { .. }
                                    | NodePayload::PolygonalBody { .. }
                            )
                        }).unwrap_or(false);
                        if droppable {
                            self.material_row_drop_check(&ctx, rect, nid);
                        }
                    }
                }

                // —— Drag start (prim / body): press + move past 4px ——
                // Hierarchy drag: SdfBody/Group/SdfObject + PolygonalBody/ImportedMesh
                // (is_body_drag_payload). Never lights, materials, cameras.
                if !row.is_root && !is_mat_map && !is_material && primary_pressed && hov && !row.locked {
                    if let Some(p) = ptr_pos {
                        self.outliner.drag_start_pos = Some(p);
                        match row.item {
                            OlItem::Prim(uid) => {
                                if let Some(fi) = self.prim_index_by_uid(uid) {
                                    self.outliner.drag_source = Some(fi);
                                }
                                self.outliner.body_drag_source = None;
                            }
                            OlItem::Node(n) => {
                                let nid = NodeId(n);
                                let can_body = self
                                    .scene
                                    .scene_graph
                                    .get(nid)
                                    .map(|node| Self::is_body_drag_payload(&node.payload))
                                    .unwrap_or(false);
                                if can_body {
                                    self.outliner.body_drag_source = Some(nid);
                                    self.outliner.body_drag_start_pos = Some(p);
                                    self.outliner.drag_source = None;
                                } else {
                                    self.outliner.body_drag_source = None;
                                    self.outliner.drag_source = None;
                                }
                            }
                        }
                    }
                }
                // Threshold → live drag (selection fixed AFTER the list paint)
                if primary_down {
                    if let (Some(start), Some(cur)) = (self.outliner.drag_start_pos, ptr_pos) {
                        if start.distance(cur) > 4.0 {
                            if let Some(fi) = self.outliner.drag_source {
                                if self.outliner.dragging.is_none() {
                                    self.outliner.dragging = Some(fi);
                                }
                            } else if let Some(nid) = self.outliner.body_drag_source {
                                if self.outliner.dragging_body.is_none() {
                                    self.outliner.dragging_body = Some(nid);
                                }
                            }
                        }
                    }
                }

                // —— Drop hit-test while dragging ——
                if self.outliner.dragging.is_some() || self.outliner.dragging_body.is_some() {
                    if let Some(ptr) = ptr_pos {
                        if rect.contains(ptr) && !row.is_root && !is_mat_map {
                            self.ui2_ol_dnd_hit(&ctx, row, rect, ptr);
                        }
                    }
                }
            } // for ri in first..last

            if last < n_rows {
                ui.add_space((n_rows - last) as f32 * OL_ROW_H);
            }
            // Virtualized DnD: hit-test rows outside the paint window so empty-area "new body"
            // / un-nest does not fire when the pointer is over a real off-screen row's spacer.
            if (self.outliner.dragging.is_some() || self.outliner.dragging_body.is_some())
                && self.outliner.drop_visual.is_none()
            {
                if let Some(ptr) = ptr_pos {
                    if ptr.y >= origin_y && n_rows > 0 {
                        let ri = ((ptr.y - origin_y) / OL_ROW_H).floor() as isize;
                        if ri >= 0 {
                            let ri = ri as usize;
                            if ri < n_rows && (ri < first || ri >= last) {
                                let y = origin_y + ri as f32 * OL_ROW_H;
                                let fake = egui::Rect::from_min_size(
                                    egui::pos2(ui.min_rect().left(), y),
                                    egui::vec2(row_w, OL_ROW_H),
                                );
                                if fake.contains(ptr) && !rows[ri].is_root {
                                    self.ui2_ol_dnd_hit(&ctx, &rows[ri], fake, ptr);
                                }
                            }
                        }
                    }
                }
            }
            // Scroll-to for rows outside the virtual paint window (F / frame_selected).
            if let Some(target) = self.outliner.scroll_to_node {
                let hit = rows.iter().enumerate().find(|(_, r)| match (target, r.item) {
                    (SceneNodeId::Node(nid), OlItem::Node(n))
                    | (SceneNodeId::Body(nid), OlItem::Node(n)) => nid.0 == n,
                    (SceneNodeId::Prim(u), OlItem::Prim(p)) => u == p,
                    _ => false,
                });
                if let Some((ri, _)) = hit {
                    if ri < first || ri >= last {
                        let y = origin_y + ri as f32 * OL_ROW_H;
                        let fake = egui::Rect::from_min_size(
                            egui::pos2(ui.min_rect().left(), y),
                            egui::vec2(row_w, OL_ROW_H),
                        );
                        ui.scroll_to_rect(fake, Some(egui::Align::Center));
                        self.outliner.scroll_to_node = None;
                    }
                }
            }
        });
        // Persist scroll for idle paint (viewport-focused frames) so the tree doesn't jump.
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new("v2_ol_idle_scroll_y"),
                ol_scroll.state.offset.y,
            );
        });

        // ── Quick-bar: Duplicate / Res− / Res+ on the strip reserved under the tree ──
        let mut qb_res: Option<(NodeId, u32)> = None;
        {
            // Sized from the width we were GIVEN, never a requested width.
            let bar_w = ui.available_width();
            let (bar, _) = ui.allocate_exact_size(egui::vec2(bar_w, OL_QUICKBAR_H), egui::Sense::hover());
            ui.painter().rect_filled(bar, 0.0, ui_theme::SURFACE_HEADER);
            ui.painter().hline(bar.x_range(), bar.top() + 0.5, egui::Stroke::new(1.0, ui_theme::SEAM));

            // Duplicate acts on the selection — routed through the SAME OlAct::Duplicate
            // funnel as the row context menu (→ duplicate_selected, the Shift+D path).
            let can_dup = self.sel.usel.has_prims() || self.sel.usel.has_nodes();

            // Res± steps the ACTIVE top-level body through the picker ladder, clamped by
            // the same sparse-pipeline ceiling the pickers use (body_grid_res_state).
            let res_body = self.resolution_target_body();
            let res_state = res_body.map(|b| self.body_grid_res_state(b));
            let ladder = crate::properties_render::RES_LADDER;
            // Nearest ladder step in each direction (also sane if an adaptive/off-ladder
            // resolution is current).
            let raw_up = res_state.and_then(|(c, _)| ladder.iter().copied().find(|&r| r > c));
            let step_up = raw_up.filter(|&r| res_state.map_or(false, |(_, m)| r <= m));
            let step_dn = res_state.and_then(|(c, _)| ladder.iter().copied().rev().find(|&r| r < c));

            let dup_tip = if can_dup {
                format!("Duplicate selection  ({})", keys.dup)
            } else {
                "Duplicate — select a body or primitive first".to_string()
            };
            let cur_lbl = res_state.map(|(c, _)| format!("{c}³")).unwrap_or_default();
            let dn_tip = match (res_body, step_dn) {
                (None, _) => "Bake resolution — no active SDF body".to_string(),
                (_, Some(r)) => format!("Bake resolution {cur_lbl} → {r}³"),
                (_, None) => format!("Bake resolution {cur_lbl} — already at the minimum"),
            };
            let up_tip = match (res_body, step_up, raw_up) {
                (None, _, _) => "Bake resolution — no active SDF body".to_string(),
                (_, Some(r), _) => format!("Bake resolution {cur_lbl} → {r}³"),
                (_, None, Some(_)) =>
                    "1024³/2048³ need the sparse brick pipeline (Developer Tweaks → Sparse bake)".to_string(),
                (_, None, None) => format!("Bake resolution {cur_lbl} — already at the maximum"),
            };

            let cy = bar.center().y;
            let mut x = bar.left() + 8.0;
            // Same 22×20 chip look as the row lock button.
            let mut chip = |ui: &mut egui::Ui, x: &mut f32, icon: &str, enabled: bool, tip: &str, id: u8| -> bool {
                let r = egui::Rect::from_center_size(
                    egui::pos2(*x + OL_LOCK_BTN.x * 0.5, cy), OL_LOCK_BTN);
                *x += OL_LOCK_BTN.x + 6.0;
                let resp = ui.interact(r, ui.id().with(("ol_quickbar", id)), egui::Sense::click());
                let hov = enabled && ui.rect_contains_pointer(r);
                if hov {
                    ui.painter().rect_filled(r, ui_theme::RADIUS_CONTROL, ui_theme::SURFACE_CONTROL);
                }
                let col = if !enabled { ui_theme::TEXT_FAINT }
                          else if hov { ui_theme::TEXT }
                          else { ui_theme::TEXT_DIM };
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icon,
                    egui::FontId::proportional(14.0), col);
                let clicked = enabled && resp.clicked();
                resp.on_hover_text(tip);
                clicked
            };
            if chip(ui, &mut x, ph::COPY, can_dup, &dup_tip, 0) {
                ctx_act = Some(OlAct::Duplicate);
            }
            if chip(ui, &mut x, ph::MINUS_SQUARE, step_dn.is_some(), &dn_tip, 1) {
                if let (Some(b), Some(r)) = (res_body, step_dn) { qb_res = Some((b, r)); }
            }
            if chip(ui, &mut x, ph::PLUS_SQUARE, step_up.is_some(), &up_tip, 2) {
                if let (Some(b), Some(r)) = (res_body, step_up) { qb_res = Some((b, r)); }
            }
        }
        if let Some((body, res)) = qb_res {
            // The SAME shared apply path as the Grid picker / ui2 props SDF page.
            self.apply_body_grid_resolution(body, res);
            self.toast(&format!("Bake resolution: {res}³"));
        }

        // Ensure drag payload is selected before release apply
        if let Some(fi) = self.outliner.dragging {
            if let Some(uid) = self.scene.primitives.get(fi).map(|p| p.prim_uid) {
                let id = SceneItemId::Prim(uid);
                if !self.sel.usel.selected.contains(&id) {
                    self.select_single_unified(id);
                }
            }
        }
        if let Some(nid) = self.outliner.dragging_body {
            let id = SceneItemId::Node(nid);
            if !self.sel.usel.selected.contains(&id) {
                self.select_single_unified(id);
            }
        }

        // DnD release + indicators (shared v1 backend)
        {
            let mut d = crate::outliner_panel::OutlinerDeferred::default();
            let ctx = ui.ctx().clone();
            self.outliner_dnd_release(&ctx, ui, &mut d);
            self.process_outliner_deferred(d);
        }
        // Clear press tracking when button up
        if !primary_down {
            if self.outliner.dragging.is_none() {
                self.outliner.drag_start_pos = None;
                self.outliner.drag_source = None;
            }
            if self.outliner.dragging_body.is_none() {
                self.outliner.body_drag_start_pos = None;
                self.outliner.body_drag_source = None;
            }
        }

        // Keyboard navigation (only when pointer is over the outliner)
        self.ui2_ol_keyboard(ui, &rows, &mut sel_click, &mut do_rename, &mut ctx_act);

        if let Some(c) = toggle_col {
            if let Some(p) = self.ui2.ol_cols.iter().position(|&x| x == c) { self.ui2.ol_cols.remove(p); }
            else {
                self.ui2.ol_cols.push(c);
                self.ui2.ol_cols.sort_by_key(|x| OlCol::ALL.iter().position(|a| a == x).unwrap_or(0));
            }
        }

        // Expand / collapse all — only via ol_set_expanded (dual-writes expanded_bodies).
        if expand_all {
            let expand_ids: Vec<NodeId> = self
                .scene
                .scene_graph
                .all_nodes()
                .filter(|(_, node)| {
                    !node.children.is_empty()
                        || matches!(&node.payload, NodePayload::SdfBody { .. })
                })
                .map(|(nid, _)| nid)
                .collect();
            for nid in expand_ids {
                self.ol_set_expanded(nid, true);
            }
        }
        if collapse_all {
            let collapse_ids: Vec<NodeId> = self
                .scene
                .scene_graph
                .all_nodes()
                .filter(|(_, node)| {
                    let has = !node.children.is_empty()
                        || matches!(&node.payload, NodePayload::SdfBody { .. });
                    has && !matches!(node.payload, NodePayload::Root)
                })
                .map(|(nid, _)| nid)
                .collect();
            for nid in collapse_ids {
                self.ol_set_expanded(nid, false);
            }
        }

        // Rename (default F2): only when pointer is over outliner — never steals viewport.
        // Single = inline field; multi = batch dialog. Bound via ACT_RENAME.
        if self.ui2.ol_pointer_over
            && self.outliner.renaming.is_none()
            && self.ui2.ol_multi_rename.is_none()
            && ui.input(|i| self.keybindings.pressed(ACT_RENAME, i))
        {
            let targets = self.ol_renamable_selection();
            if targets.len() > 1 {
                self.ui2.ol_multi_rename = Some(targets[0].1.clone());
            } else if let Some((t, name)) = targets.into_iter().next() {
                self.trigger_rename(t, &name);
            }
        }

        // Multi-rename popup
        if self.ui2.ol_multi_rename.is_some() {
            let mut close = false;
            let mut apply = false;
            egui::Window::new("Rename Selection")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label("Base name — first item keeps it; others get .001, .002, …");
                    if let Some(ref mut base) = self.ui2.ol_multi_rename {
                        let r = ui.text_edit_singleline(base);
                        r.request_focus();
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            apply = true;
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            apply = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            if apply {
                if let Some(base) = self.ui2.ol_multi_rename.take() {
                    let base = base.trim().to_string();
                    if !base.is_empty() {
                        let targets = self.ol_renamable_selection();
                        let named: Vec<(SceneNodeId, String)> = targets
                            .into_iter()
                            .enumerate()
                            .map(|(i, (t, _))| {
                                let n = if i == 0 {
                                    base.clone()
                                } else {
                                    format!("{base}.{i:03}")
                                };
                                (t, n)
                            })
                            .collect();
                        self.rename_scene_items_batch(&named);
                        self.toast(&format!("Renamed {} items", named.len()));
                    }
                }
            } else if close {
                self.ui2.ol_multi_rename = None;
            }
        }

        // + Add menu (verified self-transacting scene actions)
        match do_add {
            Some(1) => self.add_empty_sdf_body(),
            Some(2) => self.add_light(core_math::scene_graph::LightType::Directional),
            Some(3) => self.add_light(core_math::scene_graph::LightType::Point),
            Some(4) => self.add_camera(),
            Some(5) => self.add_light(core_math::scene_graph::LightType::Spot),
            Some(6) => self.create_material_interactive(),
            _ => {}
        }

        // ── apply collected actions (verified backend paths) ──
        // Rename first: a finished edit must clear `renaming` before a fresh double-click
        // could set it again (they cannot both fire in one frame — a renaming row eats its
        // own clicks — but the order makes that independent of that guard).
        if let Some(result) = rename_done {
            if let (Some(t), Some(new_name)) = (renaming_now, result) {
                self.rename_scene_item(t, new_name);
            }
            self.outliner.renaming = None;
        }
        if let Some((t, cur)) = do_rename { self.trigger_rename(t, &cur); }
        if let Some(nid) = toggle_exp {
            let open = self.ol_is_expanded(NodeId(nid));
            self.ol_set_expanded(NodeId(nid), !open);
        }
        if let Some((item, cur)) = toggle_vis {
            match item {
                OlItem::Node(n) => {
                    let nid = NodeId(n);
                    self.transaction("Toggle visibility", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            node.visible = !node.visible;
                        }
                        // MUST invalidate structural draw list — RENDER alone keeps
                        // hidden meshes on screen (see mark_visibility_dirty).
                        s.mark_visibility_dirty();
                        s.mark_node_body_vol_dirty(nid);
                        s.cache.prim_buf_stale = true;
                        s.outliner.outliner_dirty = true;
                    });
                }
                OlItem::Prim(u) => { self.push_cmd(AppCommand::SetVisible(u, !cur)); }
            }
        }
        if let Some((item, cur)) = toggle_lock {
            match item {
                OlItem::Node(n) => {
                    let nid = NodeId(n);
                    self.transaction("Toggle lock", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) { node.locked = !node.locked; }
                        s.outliner.outliner_dirty = true;
                    });
                }
                OlItem::Prim(u) => { self.push_cmd(AppCommand::SetLocked(u, !cur)); }
            }
        }
        if let Some((id, mode)) = sel_click {
            match mode {
                // v1 parity: single-click an SDF body enters edit scope (select + active_body).
                // Ctrl/Shift keep pure selection so multi-select still works.
                SelMode::Single => {
                    if let SceneItemId::Node(nid) = id {
                        if matches!(
                            self.scene.scene_graph.get(nid).map(|n| &n.payload),
                            Some(NodePayload::SdfBody { .. })
                        ) {
                            self.enter_body_scope(nid);
                        } else {
                            self.select_single_unified(id);
                        }
                    } else {
                        self.select_single_unified(id);
                    }
                }
                SelMode::Toggle => self.toggle_select_unified(id),
                // `range_select_unified` takes the visual order as a PARAMETER, so ui2 feeds it
                // ui2's OWN flattened rows — already collapse- and filter-aware, and in the exact
                // order they were painted. NOT `self.outliner.visual_order`: only the old
                // outliner's draw fills that, so under ui2 it is empty and every `position()`
                // would miss (the fallback would silently degrade every range to a single add).
                SelMode::Range => {
                    let order: Vec<SceneItemId> = rows.iter().map(|r| r.item.sel_id()).collect();
                    self.range_select_unified(id, &order);
                }
            }
        }

        // Right-click adoption — NEVER drop an existing selection (owner 2026-08-06:
        // "right clicking in the outliner should not drop the selection; outliner clicks
        // are not viewport empty-clicks"). Only adopt when nothing is selected so
        // selection-driven verbs have a target; if anything is already selected, leave it.
        if let Some(id) = ctx_sel {
            if self.sel.usel.selected.is_empty() {
                self.select_single_unified(id);
            }
            // else: row already in selection, or multi/single selection stays intact
        }
        self.ui2_ol_apply(ctx_act);

        // Restore row cache taken for the paint pass (no per-frame clone of 10k rows).
        self.ui2.ol_rows_cache = rows;
    }
}

/// Filter field: a 158px `--field` box with magnifier + kind-filter funnel (same SHOW TYPES
/// popover as the toolbar funnel — no longer a dead control).
pub(crate) fn ol_searchbox(
    ui: &mut egui::Ui,
    filter: &mut String,
    w: f32,
    kinds_armed: bool,
    kinds: &mut std::collections::HashSet<OlKind>,
) {
    ui.scope(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
        ui.painter().rect(rect, ui_theme::RADIUS_CONTROL, ui_theme::SURFACE_FIELD,
            egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle);
        let inner = rect.shrink2(egui::vec2(8.0, 0.0));
        let cy = inner.center().y;
        ui.painter().text(egui::pos2(inner.left() + 6.0, cy), egui::Align2::CENTER_CENTER,
            ph::MAGNIFYING_GLASS, egui::FontId::proportional(12.0), ui_theme::TEXT_FAINT);
        let funnel = egui::Rect::from_center_size(egui::pos2(inner.right() - 8.0, cy), egui::Vec2::splat(16.0));
        let fr = ui.interact(funnel, ui.id().with("ol_search_funnel"), egui::Sense::click());
        let fcol = if kinds_armed {
            ui_theme::ACCENT_LIGHT
        } else if fr.hovered() {
            ui_theme::TEXT_SECONDARY
        } else {
            ui_theme::TEXT_FAINT
        };
        ui.painter().text(funnel.center(), egui::Align2::CENTER_CENTER, ph::FUNNEL,
            egui::FontId::proportional(16.0), fcol);
        fr.clone().on_hover_text("Filter by type");
        // Own popup (not shared with toolbar id) so it anchors under the search funnel.
        let fpid = ui.make_persistent_id("ol_search_kind_funnel");
        if fr.clicked() {
            ui.memory_mut(|m| m.toggle_popup(fpid));
        }
        egui::popup_below_widget(ui, fpid, &fr, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(180.0);
            ui.label(egui::RichText::new("SHOW TYPES").size(9.5).color(ui_theme::TEXT_SECTION));
            ui.small("(none checked = show all)");
            for k in OlKind::ALL {
                let mut on = kinds.contains(&k);
                if ui.checkbox(&mut on, format!("{}  {}", k.icon(), k.label())).changed() {
                    if on {
                        kinds.insert(k);
                    } else {
                        kinds.remove(&k);
                    }
                }
            }
            ui.separator();
            if ui.button("Show all types").clicked() {
                kinds.clear();
                ui.close_menu();
            }
        });
        let field = egui::Rect::from_min_max(
            egui::pos2(inner.left() + 12.0 + 6.0, rect.top()),
            egui::pos2(funnel.left() - 6.0, rect.bottom()));
        if field.width() > 8.0 {
            ui.put(field, egui::TextEdit::singleline(filter)
                .hint_text(egui::RichText::new("Filter").size(11.0).color(ui_theme::TEXT_FAINT))
                .font(egui::FontId::proportional(11.0))
                .text_color(ui_theme::TEXT_SECONDARY)
                .frame(false));
        }
    });
}
