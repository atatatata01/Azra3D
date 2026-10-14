//! Right panel draw function (outliner + property dispatcher) — thin hub.
//! The outliner content itself is ONE function (`draw_outliner_contents`) callable
//! from BOTH hosts: docked in the right panel (with a drag splitter) or detached
//! into a floating `egui::Window` (closing the window re-docks it).

use crate::ui_menus::AzraMenus;
use eframe::egui;
use core_math::scene_graph::NodePayload;
use crate::*;
use crate::outliner_panel::OutlinerDeferred;

impl FourthLayerApp {
    pub(crate) fn draw_right_panel(&mut self, ctx: &egui::Context) {
        if !self.layout.show_properties_panel {
            return;
        }

        // --- Detached host: floating, resizable, closable window (close = re-dock) ---
        if self.layout.outliner_detached {
            let mut open = true;
            egui::Window::new("Outliner")
                .id(egui::Id::new("outliner_floating_window"))
                .open(&mut open)
                .resizable(true)
                .default_size(egui::vec2(300.0, 420.0))
                .min_width(180.0)
                .min_height(140.0)
                .show(ctx, |ui| {
                    // Window is user-resizable → the tree fills the window height.
                    let h = ui.available_height().max(100.0);
                    self.draw_outliner_contents(ctx, ui, h);
                });
            if !open {
                self.layout.outliner_detached = false;
                self.layout.save();
            }
        }

        let panel = match self.layout.properties_panel_side {
            PanelSide::Left => egui::SidePanel::left("properties_panel_left"),
            PanelSide::Right => egui::SidePanel::right("properties_panel_right"),
        };
        panel.resizable(true).default_width(self.layout.properties_panel_width).show(ctx, |ui| {
            if !self.layout.outliner_detached {
                // Total panel height BEFORE the outliner — the splitter converts its drag
                // delta into a fraction of this.
                let panel_h = ui.available_height().max(1.0);
                // B9: honour the "Outliner %" layout setting (was hard-coded 0.30 → the slider did nothing).
                // Removed the 250px cap so the slider's full 0.2..=0.8 range actually changes the height;
                // keep a 100px floor so it never collapses below usability.
                let frac = self.layout.outliner_height_fraction.clamp(0.2, 0.8);
                let outliner_h = (panel_h * frac).max(100.0);
                self.draw_outliner_contents(ctx, ui, outliner_h);

                // --- Drag splitter: resize the outliner section within the right panel.
                // Writes the SAME `outliner_height_fraction` the layout slider uses;
                // persisted to ~/.azra/layout.json on drag release.
                //
                // Rows register a full-width Sense::drag sink (outliner_row_shell) so a press
                // on a row never latches this splitter. Belt-and-suspenders: if a custom DnD
                // press-token is live, ignore splitter deltas (press started on a row).
                let outliner_drag_owns_press = self.outliner.drag_start_pos.is_some()
                    || self.outliner.dragging.is_some()
                    || self.outliner.body_drag_start_pos.is_some()
                    || self.outliner.dragging_body.is_some();
                let (splitter_rect, splitter_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 7.0),
                    egui::Sense::drag(),
                );
                let active = !outliner_drag_owns_press
                    && (splitter_resp.hovered() || splitter_resp.dragged());
                if active {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                let line_col = if active {
                    egui::Color32::from_gray(160)
                } else {
                    egui::Color32::from_gray(80)
                };
                ui.painter().hline(
                    splitter_rect.x_range(),
                    splitter_rect.center().y,
                    egui::Stroke::new(if active { 2.0 } else { 1.0 }, line_col),
                );
                if !outliner_drag_owns_press && splitter_resp.dragged() {
                    let delta = splitter_resp.drag_delta().y / panel_h;
                    self.layout.outliner_height_fraction =
                        (self.layout.outliner_height_fraction + delta).clamp(0.2, 0.8);
                }
                if !outliner_drag_owns_press && splitter_resp.drag_stopped() {
                    self.layout.save();
                }
            }

            self.draw_properties_section(ctx, ui);
        });
    }

    /// The whole outliner UI (header, search bar, scene tree, deferred processing) —
    /// the ONE code path shared by the docked panel and the detached floating window.
    /// `outliner_h` = max height of the tree's scroll area.
    fn draw_outliner_contents(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, outliner_h: f32) {
            ui.horizontal(|ui| {
                ui.heading("Outliner");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Detach / re-dock toggle
                    let detached = self.layout.outliner_detached;
                    let detach_icon = if detached { "\u{2199}" } else { "\u{2197}" };
                    let detach_tip = if detached { "Dock outliner back into the panel" } else { "Detach outliner into a floating window" };
                    if ui.small_button(detach_icon).on_hover_text(detach_tip).clicked() {
                        self.layout.outliner_detached = !detached;
                        self.layout.save();
                    }
                    let add_btn = ui.small_button("+");
                    let popup_id = ui.make_persistent_id("outliner_add_popup");
                    if add_btn.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(popup_id));
                    }
                    egui::popup_below_widget(ui, popup_id, &add_btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                        ui.set_min_width(160.0);
                        if ui.button("SDF Body").clicked() { self.add_empty_sdf_body(); }
                        if ui.button("🧊 Voxel Sculpt").clicked() { self.new_voxel_sphere(); }
                        ui.azra_menu_button("Light", |ui| {
                            if ui.button("Directional").clicked() { self.add_light(LightType::Directional); ui.close_menu(); }
                            if ui.button("Point").clicked() { self.add_light(LightType::Point); ui.close_menu(); }
                            if ui.button("Spot").clicked() { self.add_light(LightType::Spot); ui.close_menu(); }
                        });
                        if ui.button("Polygon Mesh").clicked() { self.add_empty_poly_body(); }
                        if ui.button("NURBS Surface").clicked() { self.add_empty_nurbs_body(); }
                        if ui.button("Group").clicked() { self.add_empty_group(); }
                        if ui.button("Empty (Locator)").clicked() { self.add_empty_locator(); }
                        if ui.button("Camera").clicked() { self.add_camera(); }
                        if ui.button("\u{1F3A8} Material").clicked() { self.create_material_interactive(); }
                    });
                });
            });
            ui.separator();

            // --- Search / filter bar ---
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.outliner.search_query)
                    .hint_text("Filter...")
                    .desired_width(ui.available_width() - 22.0));
                if r.changed() {
                    self.outliner.search_active = !self.outliner.search_query.is_empty();
                    self.outliner.outliner_dirty = true;
                }
                if self.outliner.search_active {
                    if ui.small_button("✕").clicked() {
                        self.outliner.search_query.clear();
                        self.outliner.search_active = false;
                        self.outliner.outliner_dirty = true;
                    }
                }
            });
            ui.add_space(2.0);

            let filter = self.outliner.search_query.to_lowercase();
            let filtering = self.outliner.search_active && !filter.is_empty();

            // ---- Unified scene tree ----
            let mut d = OutlinerDeferred::default();
            d.ctrl = ctx.input(|i| i.modifiers.command);

            let mut vis_ch=false; let ctrl=d.ctrl; let shift=ctx.input(|i|i.modifiers.shift);

            // Reset drop target each frame
            self.outliner.drop_target = None;
            self.outliner.drop_target_y = None;
            self.outliner.drop_reparent = None;
            self.outliner.drop_on_prim = false;
            self.outliner.drop_root_y = None;
            self.outliner.drop_insert = None;
            self.outliner.drop_visual = None;
            self.outliner.body_drop_root_y = None;
            self.outliner.body_drop_target_nid = None;
            self.outliner.body_drop_target_y = None;
            self.outliner.body_drop_before = false;
            self.outliner.body_drop_nest = false;
            self.outliner.body_header_bottoms.clear();
            // Double-buffered: rows push into `visual_order_next` during the draw; the
            // completed list is promoted to `visual_order` AFTER the draw. Click handlers
            // fire mid-draw and must see the last COMPLETE order, or shift-range select
            // silently no-ops for any row below the click (the target/anchor wasn't
            // registered yet in the partial list).
            self.outliner.visual_order_next.clear();

            // Cache root children — only rebuild when tree structure changed.
            // During interaction, throttle data rebuilds to panel_refresh_interval_ms (default 5Hz).
            let interacting = self.render.interaction_active_frames >= 2;
            let throttle_active = interacting && self.tweak.throttle_panels_during_interaction;
            let refresh_interval = std::time::Duration::from_millis(self.tweak.panel_refresh_interval_ms as u64);
            let needs_rebuild = self.outliner.outliner_dirty || self.outliner.cached_root_children.is_empty();
            // STRUCTURAL changes (reparent/nest/add/delete) set `outliner_dirty` and MUST rebuild the
            // root-children list immediately — the throttle is a perf gate for redundant re-pulls during
            // interaction, NOT a correctness mechanism. If a nest defers the rebuild, the moved body shows
            // BOTH at root (stale cache) AND nested (live resolve_child_order) — the duplicate-in-outliner
            // bug. `children(root).to_vec()` is cheap; only the heavier buckets are lazily rebuilt elsewhere.
            let throttle_allows = self.outliner.outliner_dirty
                || !throttle_active
                || self.outliner.outliner_last_refresh.elapsed() >= refresh_interval;
            if needs_rebuild && throttle_allows {
                self.outliner.cached_root_children = self.scene.scene_graph.children(self.scene.scene_graph.root()).to_vec();
                self.outliner.cached_body_buckets.clear();
                self.outliner.outliner_last_refresh = std::time::Instant::now();
                // Consume the dirty flag AT THE REBUILD, before the tree draw. Handlers that
                // run DURING the draw (DnD release transaction, row context menus) re-set it,
                // and that new dirty must SURVIVE to next frame — the old end-of-frame clear
                // swallowed it, leaving stale caches after a successful drop (the "drag applies
                // but outliner doesn't update" bug).
                self.outliner.outliner_dirty = false;
            }
            let root_children = self.outliner.cached_root_children.clone();

            // Auto-expand the newly ACTIVATED body — exactly once, on the change edge.
            // Expansion state is USER-owned and uniform (in `expanded_bodies` = expanded):
            // activation may only INSERT; it never removes other bodies' entries (the old
            // per-frame inverted-semantics remove collapsed every other body on selection),
            // and the user can still manually collapse the active body afterwards.
            if self.outliner.last_active_body != Some(self.scene.active_body_nid) {
                self.outliner.expanded_bodies.insert(self.scene.active_body_nid);
                self.outliner.last_active_body = Some(self.scene.active_body_nid);
            }

            egui::ScrollArea::vertical().id_salt("outliner_scroll").max_height(outliner_h).drag_to_scroll(false).show(ui, |ui| {
                // DnD HIT-TEST GUARD — root fix for the "row drag moves the panel/window" class.
                //
                // The tree rows are deliberately non-interactive (`Sense::hover`; the row
                // DnD is a custom raw-pointer tracker), so a row press produces no drag
                // hit of its own. egui 0.29 then resolves the press to SOME OTHER drag
                // widget (hit_test.rs): the nearest one within interact_radius=5px via the
                // (None,None) fallback, or any drag-sense widget whose rect CONTAINS the
                // press. Three egui-INTERNAL thieves match — the docked SidePanel's
                // `__resize` edge (panel width), the detached Window's whole-body Area
                // "move" widget (area.rs registers a window-sized Sense::drag rect, so in
                // detached mode EVERY row press latched it → the window followed the row
                // drag), and the Window's `edge_drag` resize rects. Their drag deltas are
                // consumed INSIDE egui, so no app-side flag can suppress them (that was
                // 214c958's mistake: it deferred only OUR splitter's delta-read).
                //
                // Fix the class, not the instances: register ONE full-viewport
                // `Sense::drag` widget UNDER all rows (registered first ⇒ bottom of the
                // layer). Any press on the tree now resolves drag → THIS widget (a no-op;
                // the raw-pointer DnD keeps working on top), and clicks still resolve to
                // the topmost click widget (label/eye/rename — the "button in a movable
                // window" branch of hit_test_on_close). Widgets registered LATER — the
                // scrollbar handle, the height splitter, panel/window edges — still win
                // direct hits on their own rects. Same pattern egui itself uses for
                // drag-to-scroll ("we must do this BEFORE adding content, or we will
                // steal input from the widgets we contain", scroll_area.rs).
                //
                // Rect = the scroll viewport (clip rect minus clip_rect_margin so the
                // guard never overlaps the search bar above or the splitter below).
                // Row shells (outliner_row_shell) give each row its own full-width drag sink
                // so a press on a label/empty padding never falls through to the height
                // splitter or SidePanel `__resize`. This viewport guard covers empty space
                // between rows / below the last row with the same pattern.
                let guard_rect = ui
                    .clip_rect()
                    .shrink2(egui::vec2(0.0, ui.visuals().clip_rect_margin));
                let _ = ui.interact(
                    guard_rect,
                    ui.id().with("outliner_tree_dnd_guard"),
                    egui::Sense::drag(),
                );

                // Edge auto-scroll while a drag is live — lets long trees be traversed mid-drag.
                self.outliner_drag_autoscroll(ctx, ui);
                let ctrl_sg = ctx.input(|i| i.modifiers.command || i.modifiers.shift);

                // ---- Lights ----
                self.draw_outliner_lights(ui, ctx, filtering, &filter, ctrl, shift, &mut d);

                // ---- Materials (dedicated section — the generic walk skips Material nodes) ----
                self.draw_outliner_materials(ui, ctx, filtering, &filter, ctrl_sg, shift, &mut d);

                // ---- Scene graph nodes ----
                for &child_nid in &root_children {
                    let node_data = self.scene.scene_graph.get(child_nid).map(|n| {
                        (Self::node_type_label(&n.payload), Self::node_type_color(&n.payload),
                         n.payload.display_name().to_string(), n.visible, n.locked,
                         matches!(n.payload, NodePayload::SdfBody { .. }),
                         // Skip Root, and skip Light nodes: lights are drawn by the dedicated
                         // Lights section above (draw_outliner_lights). Drawing them here too
                         // produced a SECOND row for the same light node (both "Sun.001") —
                         // the phantom-duplicate-light outliner bug: deleting via the light row
                         // removed "both" (one node), while the generic LGT row had no working
                         // delete path for a Light payload.
                         // Material nodes are likewise drawn ONLY by the dedicated Materials
                         // section (draw_outliner_materials) — the generic "MAT" row had a
                         // silently no-op rename (its payload match lacked Material) and a
                         // delete with no in-use guard.
                         matches!(n.payload, NodePayload::Root | NodePayload::Light(_) | NodePayload::Material { .. }))
                    });
                    let (type_label, type_color, name, visible, locked, is_sdf_body, skip_row) =
                        match node_data { Some(d) => d, None => continue };
                    if skip_row { continue; }

                    if is_sdf_body {
                        self.draw_outliner_sdf_body(
                            ui, ctx, child_nid, &name, visible,
                            &root_children, filtering, &filter,
                            ctrl, shift, &mut d, &mut vis_ch, 0,
                        );
                        continue;
                    }

                    // ---- Non-SDF node ----
                    self.draw_outliner_node_row(
                        ui, ctx, child_nid, type_label, type_color,
                        &name, visible, locked, &root_children,
                        filtering, &filter, ctrl_sg, shift, &mut d, &mut vis_ch, 0,
                    );
                }

                // Drop indicators must be painted in scroll-content space (same as stored Y positions)
                self.outliner_dnd_release(ctx, ui, &mut d);
            });

            // Promote the completed draw order — from here (and all of NEXT frame's input,
            // which runs before the next draw) `visual_order` is a full, consistent list.
            self.outliner.visual_order = std::mem::take(&mut self.outliner.visual_order_next);

            self.process_outliner_deferred(d);

            // (outliner_dirty is consumed at the REBUILD above, never here — a dirty set
            // during this frame's draw/release/deferred processing must reach next frame.)
            // Dirty ⇒ next frame must redraw (rebuild caches + promote the double-buffered
            // visual_order). Without this, a drop applied on the release frame waits for the
            // next input event before the panel refreshes.
            if self.outliner.outliner_dirty {
                ctx.request_repaint();
            }
    }
}
