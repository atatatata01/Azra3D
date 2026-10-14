// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
use crate::ui_menus::DefaultMenu;
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_ctxbar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::none()
            .fill(ui_theme::SURFACE_BAR)
            .inner_margin(egui::Margin::symmetric(10, 0));
        egui::TopBottomPanel::top("v2_ctxbar")
            .exact_height(ui_theme::H_CONTEXT)
            .frame(frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                ui.painter().hline(
                    bar.x_range(),
                    bar.bottom() - 0.5,
                    egui::Stroke::new(1.0, ui_theme::SEAM),
                );
                let _ = ui.allocate_rect(bar, egui::Sense::hover());

                // ── RIGHT: shading + sep + View (mockup #shading margin-left:auto) ──
                ui.allocate_ui_at_rect(bar, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // View pill — hosts split (Gate 8) without inventing app-bar chrome.
                        let view_id = ui.make_persistent_id("v2_view_pill");
                        let view_r = self.ui2_ctx_pill(ui, "View", None, ph::CARET_DOWN);
                        if view_r.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(view_id));
                        }
                        egui::popup_below_widget(
                            ui,
                            view_id,
                            &view_r,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                ui.set_min_width(160.0);
                                let split_on = self.ui2.vp_split.is_some();
                                let lbl = if split_on {
                                    "Join views"
                                } else {
                                    "Split left | right"
                                };
                                if ui.button(lbl).clicked() {
                                    if self.ui2.vp_split.is_some() {
                                        self.ui2_join_split();
                                    } else {
                                        self.ui2_begin_split(true);
                                        self.toast("Split L|R — drag seam · orbit each pane · × to join");
                                    }
                                    ui.close_menu();
                                }
                                if ui.button("Split top / bottom").clicked() {
                                    self.ui2_begin_split(false);
                                    self.toast("Split T/B — drag seam · orbit each pane · × to join");
                                    ui.close_menu();
                                }
                            },
                        );
                        // Shading strip moved to an in-viewport top-right overlay
                        // (ui2_shading_overlay, drawn in ui2_draw_scene_overlays).
                    });
                });

                // ── LEFT: mode-keyed tool dock (Sculpt now; poly/NURBS later) ──
                // Blender-header style: persistent tool settings, same state as brush panel.
                ui.allocate_ui_at_rect(bar, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        self.ui2_ctxbar_mode_tool_dock(ui);
                    });
                });

                // ── CENTER: .ctx-mid absolute centre ─────────────────────────
                // Pills: Global · Bounding Center · | · Snap · Groups · Prop (falloff)
                ui.allocate_ui_at_rect(bar, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // Keep pill labels live with tool / node state.
                        self.ui2.ctx_space = self.ui2_space_pill_label();
                        self.ui2.ctx_pivot = self.ui2_pivot_pill_label();
                        let space = self.ui2.ctx_space;
                        let pivot = self.ui2.ctx_pivot;
                        let snap_lbl = self.ui2_snap_pill_label();
                        let show_groups = self.ui2_show_polygroup_pill();
                        let groups_lbl = if show_groups {
                            self.ui2_polygroup_pill_label()
                        } else {
                            String::new()
                        };
                        let show_prop = self.ui2_show_prop_pill();
                        let prop_lbl = if show_prop {
                            self.ui2_prop_pill_label()
                        } else {
                            String::new()
                        };
                        // Approximate group width so the cluster sits at bar centre.
                        // Sculpt tool settings live on the in-viewport header now.
                        let left_dock = 0.0;
                        let mid_w_est = {
                            let mut w = 340.0;
                            if show_groups {
                                w += 120.0;
                            }
                            if show_prop {
                                w += 130.0;
                            }
                            w
                        };
                        ui.add_space(
                            ((bar.width() - mid_w_est) * 0.5 + left_dock * 0.15).max(left_dock),
                        );
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // ── SPACE (Global / Local) ──
                        {
                            let id = ui.make_persistent_id(("v2_ctxpill", "space"));
                            let r = self.ui2_ctx_pill(ui, space, Some(ph::GLOBE_SIMPLE), ph::CARET_DOWN);
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(id));
                            }
                            egui::popup_below_widget(
                                ui,
                                id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    self.ui2_space_menu(ui);
                                },
                            );
                        }
                        // ── PIVOT (modes + place + edit) ──
                        {
                            let id = ui.make_persistent_id(("v2_ctxpill", "pivot"));
                            let accent = self.tool.current.is_pivot_edit();
                            let r = self.ui2_ctx_pill_accent(
                                ui,
                                pivot,
                                Some(ph::DOTS_THREE_OUTLINE),
                                ph::CARET_DOWN,
                                accent,
                            );
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(id));
                            }
                            egui::popup_below_widget(
                                ui,
                                id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    self.ui2_pivot_menu(ui);
                                },
                            );
                        }
                        ui.add_space(6.0);
                        let (sep, _) =
                            ui.allocate_exact_size(egui::vec2(1.0, 18.0), egui::Sense::hover());
                        ui.painter().rect_filled(sep, 0.0, ui_theme::SEAM);
                        ui.add_space(6.0);
                        {
                            let id = ui.make_persistent_id(("v2_ctxpill", "snap"));
                            let r = self.ui2_ctx_pill(
                                ui,
                                &snap_lbl,
                                Some(ph::MAGNET),
                                ph::CARET_DOWN,
                            );
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(id));
                            }
                            egui::popup_below_widget(
                                ui,
                                id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    self.ui2_snap_menu(ui);
                                },
                            );
                        }
                        // Polygroups: overlay colours + group list / assign / select.
                        // Sits next to Snap (the empty slot in the ctx-mid cluster).
                        if show_groups {
                            ui.add_space(6.0);
                            let id = ui.make_persistent_id(("v2_ctxpill", "polygroups"));
                            let accent = self.ui.poly_show_polygroups;
                            let r = self.ui2_ctx_pill_accent(
                                ui,
                                &groups_lbl,
                                Some(ph::CIRCLES_THREE),
                                ph::CARET_DOWN,
                                accent,
                            );
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(id));
                            }
                            egui::popup_below_widget(
                                ui,
                                id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    self.ui2_polygroup_menu(ui);
                                },
                            );
                        }
                        // Proportional edit (Blender O) — falloff radius on G/R/S.
                        // Owner: "falloff of effect on G … toggle off up here like Blender".
                        if show_prop {
                            ui.add_space(6.0);
                            let id = ui.make_persistent_id(("v2_ctxpill", "prop"));
                            let accent = self.poly_proportional;
                            let r = self.ui2_ctx_pill_accent(
                                ui,
                                &prop_lbl,
                                Some(ph::CIRCLE_HALF),
                                ph::CARET_DOWN,
                                accent,
                            );
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(id));
                            }
                            // Click-through empty: left-click pill opens menu; double-click
                            // or checkbox in menu toggles. Secondary click = quick toggle.
                            if r.secondary_clicked() {
                                self.poly_proportional = !self.poly_proportional;
                                self.toast(if self.poly_proportional {
                                    "Proportional Edit ON (O)"
                                } else {
                                    "Proportional Edit OFF"
                                });
                            }
                            egui::popup_below_widget(
                                ui,
                                id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    self.ui2_prop_menu(ui);
                                },
                            );
                        }
                    });
                });
            });
    }

    /// Mode-keyed persistent tool settings. Sculpt moved to the in-viewport
    /// tool header (`ui2_sculpt_header_overlay`) — Blender VIEW3D header, not ctx-bar.
    pub(crate) fn ui2_ctxbar_mode_tool_dock(&mut self, _ui: &mut egui::Ui) {
        // Sculpt: see ui2/shell/sculpt_header.rs (top of the 3D view).
        // Future: WorkMode::Face/Edge/Vertex → poly dock; NURBS when active.
    }

    /// Header dock while the PolySculpt modifier Edit session is live.
    pub(crate) fn ui2_ctxbar_poly_sculpt_dock(&mut self, ui: &mut egui::Ui) {
        use core_math::voxel_sculpt::VoxelBrush;
        use egui_phosphor::regular as ph;
        if self.sculpt.poly_radius <= 1e-4 {
            self.sculpt.poly_radius = 0.15;
        }
        if self.sculpt.poly_strength <= 1e-4 {
            self.sculpt.poly_strength = 1.0;
        }
        let cur = self.sculpt.poly_brush.unwrap_or(VoxelBrush::Draw);
        let brush_lbl = crate::VoxelObject::factory_label(cur);
        ui.label(
            egui::RichText::new(format!("{}  {brush_lbl}", ph::PAINT_BRUSH))
                .size(ui_theme::FONT_TAB)
                .color(ui_theme::SCULPT_ORANGE)
                .strong(),
        );
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt("v2_ctx_poly_sculpt_brush")
            .selected_text(egui::RichText::new(brush_lbl).size(ui_theme::FONT_TAB))
            .width(110.0)
            .show_ui(ui, |ui| {
                for b in [
                    VoxelBrush::Draw,
                    VoxelBrush::Erase,
                    VoxelBrush::Smooth,
                    VoxelBrush::Flatten,
                    VoxelBrush::Inflate,
                    VoxelBrush::Move,
                    VoxelBrush::Pinch,
                    VoxelBrush::Clay,
                    VoxelBrush::ClayBuildup,
                    VoxelBrush::Scrape,
                    VoxelBrush::Crease,
                    VoxelBrush::SnakeHook,
                ] {
                    if ui
                        .selectable_label(cur == b, crate::VoxelObject::factory_label(b))
                        .clicked()
                    {
                        self.sculpt.poly_brush = Some(b);
                    }
                }
            });
        ui.label(egui::RichText::new("R").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.add(
            egui::DragValue::new(&mut self.sculpt.poly_radius)
                .speed(0.005)
                .range(0.01..=2.0)
                .max_decimals(3),
        )
        .on_hover_text("PolySculpt radius (Z/X)");
        ui.label(egui::RichText::new("S").size(10.0).color(ui_theme::TEXT_FAINT));
        ui.add(
            egui::DragValue::new(&mut self.sculpt.poly_strength)
                .speed(0.01)
                .range(0.05..=2.0)
                .max_decimals(2),
        )
        .on_hover_text("PolySculpt strength");
        if ui.button("Done").clicked() {
            self.poly_sculpt_leave_edit();
        }
    }

    /// Transform space pill menu (Global / Local) — feeds `tool.transform_space`.
    pub(crate) fn ui2_space_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(160.0);
        ui.label(
            egui::RichText::new("SPACE")
                .size(8.5)
                .color(ui_theme::TEXT_FAINT),
        );
        ui.add_space(4.0);
        let cur = self.tool.transform_space;
        if ui
            .selectable_label(cur == TransformSpace::Global, "Global")
            .on_hover_text("World axes for G/R/S and the gizmo")
            .clicked()
        {
            self.tool.transform_space = TransformSpace::Global;
            self.ui2.ctx_space = "Global";
            self.toast("Space: Global");
            ui.close_menu();
        }
        if ui
            .selectable_label(cur == TransformSpace::Local, "Local")
            .on_hover_text("Object / selection local axes for G/R/S")
            .clicked()
        {
            self.tool.transform_space = TransformSpace::Local;
            self.ui2.ctx_space = "Local";
            self.toast("Space: Local");
            ui.close_menu();
        }
    }

    /// Pivot pill menu — all pivot place / edit actions (owner: ctx-bar next to Global).
    pub(crate) fn ui2_pivot_menu(&mut self, ui: &mut egui::Ui) {
        use core_math::scene_graph::PivotMode;
        ui.set_min_width(220.0);
        ui.label(
            egui::RichText::new("PIVOT")
                .size(8.5)
                .color(ui_theme::TEXT_FAINT),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(format!("Current: {}", self.ui2_pivot_pill_label()))
                .size(11.0)
                .color(ui_theme::TEXT_DIM),
        );
        ui.separator();

        // ── Mode ──
        ui.label(
            egui::RichText::new("MODE")
                .size(8.5)
                .color(ui_theme::TEXT_FAINT),
        );
        let nid = self
            .resolve_pivot_target_nid()
            .or_else(|| self.resolve_poly_body_nid());
        let cur_mode = nid
            .and_then(|n| self.scene.scene_graph.get(n))
            .map(|n| n.pivot.mode);
        for (mode, label, tip) in [
            (
                PivotMode::BoundingCenter,
                "Bounding Center",
                "Pivot follows the mesh / body AABB center",
            ),
            (
                PivotMode::WorldOrigin,
                "World Origin",
                "Pivot fixed at world (0, 0, 0)",
            ),
            (
                PivotMode::Custom,
                "Custom",
                "User-placed pivot (follows the object)",
            ),
        ] {
            if ui
                .selectable_label(cur_mode == Some(mode), label)
                .on_hover_text(tip)
                .clicked()
            {
                if let Some(nid) = nid {
                    self.transaction("Set Pivot Mode", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            node.pivot.mode = mode;
                            if mode == PivotMode::WorldOrigin {
                                node.pivot.position = glam::Vec3::ZERO;
                            } else if mode == PivotMode::BoundingCenter {
                                // Refresh position from bounds when switching mode.
                            }
                        }
                        s.mark_dirty(DirtyFlags::RENDER);
                    });
                    if mode == PivotMode::BoundingCenter {
                        self.pivot_center_to_mass();
                    }
                    self.ui.expose_selection_pivot = true;
                    self.toast(&format!("Pivot mode: {label}"));
                } else {
                    self.toast("Select an object first");
                }
                ui.close_menu();
            }
        }
        ui.separator();

        // ── Place ──
        ui.label(
            egui::RichText::new("PLACE")
                .size(8.5)
                .color(ui_theme::TEXT_FAINT),
        );
        if ui
            .button("Pivot to Center / Mass")
            .on_hover_text("Bounding-box center (≈ mass for uniform solids)")
            .clicked()
        {
            self.pivot_center_to_mass();
            // Poly path: also try poly mesh center when no body bounds.
            if self.resolve_poly_body_nid().is_some() {
                self.poly_pivot_to_center_mesh();
            }
            ui.close_menu();
        }
        if ui
            .button("Pivot to World Origin")
            .on_hover_text("Set pivot at world (0, 0, 0)")
            .clicked()
        {
            self.pivot_center_to_origin();
            ui.close_menu();
        }
        if ui
            .button("Pivot to Workplane")
            .on_hover_text("Set pivot at the active workplane origin")
            .clicked()
        {
            self.pivot_center_to_workplane();
            ui.close_menu();
        }
        let has_poly_sel = !self.poly_sel.selected_vertices.is_empty()
            || !self.poly_sel.selected_edges.is_empty()
            || !self.poly_sel.selected_faces.is_empty();
        if ui
            .add_enabled(
                has_poly_sel || self.poly_selection_center().is_some(),
                egui::Button::new("Pivot to Selected"),
            )
            .on_hover_text(
                "Move pivot to the center of the face / edge / vertex selection.\n\
                 Geometry stays put.",
            )
            .clicked()
        {
            self.poly_pivot_to_selected();
            ui.close_menu();
        }
        ui.separator();

        // ── Edit ──
        ui.label(
            egui::RichText::new("EDIT")
                .size(8.5)
                .color(ui_theme::TEXT_FAINT),
        );
        let editing = self.tool.current.is_pivot_edit();
        if ui
            .selectable_label(editing, "Edit Pivot (G / R / S)…")
            .on_hover_text(
                "Enter pivot edit mode:\n\
                 · Drag the yellow marker\n\
                 · Or press G and move (grab)\n\
                 · Ctrl = snap · Enter = confirm · Esc = cancel\n\
                 Geometry stays put for poly meshes.",
            )
            .clicked()
        {
            if editing {
                self.confirm_pivot_mode();
            } else {
                self.enter_pivot_mode();
            }
            ui.close_menu();
        }
        if editing {
            if ui.button("Confirm pivot  Enter").clicked() {
                self.confirm_pivot_mode();
                ui.close_menu();
            }
            if ui.button("Cancel pivot  Esc").clicked() {
                self.cancel_pivot_mode();
                ui.close_menu();
            }
        }
        if ui
            .button("Pivot placement pie…")
            .on_hover_text("Open the D-key pivot pie (Mass / Origin / Workplane / Surface / Drag)")
            .clicked()
        {
            let n = crate::pie_menu::d_pie_def(self.get_active_pivot_persistent())
                .inner
                .items
                .len();
            let pos = ui.ctx().pointer_hover_pos().unwrap_or(egui::pos2(400.0, 300.0));
            self.ui.d_pie.open(pos, n);
            self.ui.d_pie_click_mode = true;
            self.ui.expose_selection_pivot = true;
            ui.close_menu();
        }
        ui.separator();
        let persistent = self.get_active_pivot_persistent();
        if ui
            .button(if persistent {
                "Persistent pivot  ✓"
            } else {
                "Temporary pivot"
            })
            .on_hover_text(
                "Persistent: custom pivot survives transforms.\n\
                 Temporary: resets to Bounding Center after one G/R/S.\n\
                 Click to toggle.",
            )
            .clicked()
        {
            self.toggle_pivot_persistent();
            ui.close_menu();
        }
    }

    /// Live pill text: "Snap" / "Snap · Vertex" / "Snap · 3" / "Snap off".
    pub(crate) fn ui2_snap_pill_label(&self) -> String {
        if !self.viewport.smart_snap_on {
            return "Snap off".into();
        }
        let mut on: Vec<&'static str> = Vec::new();
        if self.viewport.smart_snap_grid {
            on.push("Grid");
        }
        if self.viewport.snap_mode == SnapMode::Increment {
            on.push("Increment");
        }
        if self.viewport.smart_snap_vertex {
            on.push("Vertex");
        }
        if self.viewport.smart_snap_edge || self.viewport.smart_snap_edge_center {
            on.push("Edge");
        }
        if self.viewport.smart_snap_face_center {
            on.push("Face");
        }
        if self.viewport.smart_snap_center {
            on.push("Center");
        }
        if self.ui2.snap_angle_lock && self.viewport.snap_rotation_deg > 0.5 {
            on.push("Angle");
        }
        match on.len() {
            0 => "Snap".into(),
            1 => format!("Snap · {}", on[0]),
            n => format!("Snap · {n}"),
        }
    }

    /// Full Snap popover — mockup ctx snap menu + former viewport smart-snap bar
    /// (V/F/E/G/C, threshold, visible-only) + poly modeling targets.
    pub(crate) fn ui2_snap_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(240.0);
        ui.spacing_mut().item_spacing.y = 2.0;

        let mut snap_on = self.viewport.smart_snap_on;
        if ui
            .checkbox(
                &mut snap_on,
                egui::RichText::new(format!("{}  Snap enabled", ph::MAGNET))
                    .size(12.0)
                    .color(ui_theme::TEXT),
            )
.default_menu("viewport.smart_snap_on", serde_json::json!(self.viewport.smart_snap_on))
            .on_hover_text("Permanent snap. Hold Ctrl for momentary snap when off.")
            .changed()
        {
            self.viewport.smart_snap_on = snap_on;
            self.viewport.snap_enabled = snap_on;
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        let ctx_lbl = match self.modeling_context {
            ModelingContext::Polygonal | ModelingContext::ImportedMesh => "Mesh",
            ModelingContext::Sdf => "SDF",
            ModelingContext::Nurbs => "NURBS",
            ModelingContext::Light => "Light",
            ModelingContext::Camera => "Camera",
            ModelingContext::None => "—",
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("TARGET")
                    .size(9.0)
                    .color(ui_theme::TEXT_FAINT)
                    .extra_letter_spacing(0.6),
            );
            ui.label(
                egui::RichText::new(ctx_lbl)
                    .size(9.0)
                    .color(ui_theme::SDF)
                    .strong(),
            );
        });
        ui.add_space(2.0);

        // Shared targets (viewport smart-snap bar + mockup common set)
        {
            let mut grid = self.viewport.smart_snap_grid;
            if ui
                .checkbox(&mut grid, "Grid")
.default_menu("viewport.smart_snap_grid", serde_json::json!(self.viewport.smart_snap_grid))
                .on_hover_text("Grid intersections")
                .changed()
            {
                self.viewport.smart_snap_grid = grid;
            }
            let mut incr = self.viewport.snap_mode == SnapMode::Increment;
            if ui
                .checkbox(&mut incr, "Increment")
                .on_hover_text(
                    "Quantize free-drag transforms to Grid size (even without a geometry hit).\n\
                     With a geometry hit, also quantizes the snapped position onto the grid.",
                )
                .changed()
            {
                self.viewport.snap_mode = if incr {
                    SnapMode::Increment
                } else {
                    SnapMode::Grid
                };
            }
            let mut v = self.viewport.smart_snap_vertex;
            if ui
                .checkbox(&mut v, "Vertex")
.default_menu("viewport.smart_snap_vertex", serde_json::json!(self.viewport.smart_snap_vertex))
                .on_hover_text("Vertices / corners / origins")
                .changed()
            {
                self.viewport.smart_snap_vertex = v;
            }
            let mut em = self.viewport.smart_snap_edge_center;
            if ui
                .checkbox(&mut em, "Edge midpoint")
.default_menu("viewport.smart_snap_edge_center", serde_json::json!(self.viewport.smart_snap_edge_center))
                .on_hover_text("Edge midpoints")
                .changed()
            {
                self.viewport.smart_snap_edge_center = em;
            }
            let mut e = self.viewport.smart_snap_edge;
            if ui
                .checkbox(&mut e, "Edge")
.default_menu("viewport.smart_snap_edge", serde_json::json!(self.viewport.smart_snap_edge))
                .on_hover_text("Snap to closest point along edges (continuous)")
                .changed()
            {
                self.viewport.smart_snap_edge = e;
            }
            let mut fc = self.viewport.smart_snap_face_center;
            if ui
                .checkbox(&mut fc, "Face center")
.default_menu("viewport.smart_snap_face_center", serde_json::json!(self.viewport.smart_snap_face_center))
                .on_hover_text("Face centers")
                .changed()
            {
                self.viewport.smart_snap_face_center = fc;
            }
            let mut c = self.viewport.smart_snap_center;
            if ui
                .checkbox(&mut c, "Center")
.default_menu("viewport.smart_snap_center", serde_json::json!(self.viewport.smart_snap_center))
                .on_hover_text("Center of mass / prim center")
                .changed()
            {
                self.viewport.smart_snap_center = c;
            }
        }

        // Poly modeling extras (mockup TARGETS.Polygonal)
        if matches!(
            self.modeling_context,
            ModelingContext::Polygonal | ModelingContext::ImportedMesh
        ) || self.ui2.active_pillar == "poly"
        {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("MESH")
                    .size(9.0)
                    .color(ui_theme::TEXT_FAINT)
                    .extra_letter_spacing(0.6),
            );
            let mut ep = self.ui2.snap_edge_perp;
            if ui
                .checkbox(&mut ep, "Edge perpendicular")
                .on_hover_text(
                    "Foot of the perpendicular onto edges (same continuous edge query; \
                     useful when Edge midpoint alone is too coarse)",
                )
                .changed()
            {
                self.ui2.snap_edge_perp = ep;
            }
            let mut vol = self.ui2.snap_volume;
            if ui
                .checkbox(&mut vol, "Volume")
                .on_hover_text("Snap to body AABB centers and mesh surface under the cursor")
                .changed()
            {
                self.ui2.snap_volume = vol;
            }
        }

        if self.modeling_context == ModelingContext::Sdf || self.ui2.active_pillar == "sdf" {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("SDF")
                    .size(9.0)
                    .color(ui_theme::TEXT_FAINT)
                    .extra_letter_spacing(0.6),
            );
            ui.label(
                egui::RichText::new(
                    "Surface / Prim center / Prim origin map to Face, Center, Vertex above",
                )
                .size(10.0)
                .color(ui_theme::TEXT_FAINT),
            );
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // Options from the old floating viewport snap bar
        ui.label(
            egui::RichText::new("OPTIONS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        let mut vis = self.viewport.smart_snap_visible_only;
        if ui
            .checkbox(&mut vis, "Visible only")
.default_menu("viewport.smart_snap_visible_only", serde_json::json!(self.viewport.smart_snap_visible_only))
            .on_hover_text("Off = snap through occluded geometry")
            .changed()
        {
            self.viewport.smart_snap_visible_only = vis;
        }
        let mut ang = self.ui2.snap_angle_lock;
        if ui
            .checkbox(&mut ang, "Angle lock")
            .on_hover_text(
                "Lock free Move / Grab / Extrude direction to Rotation step angles on the workplane.\n\
                 Requires Snap enabled (or hold Ctrl) and Affects → Move.\n\
                 Shift still locks to world X/Y/Z only. Geometry targets (Vertex/Edge/…) win when near.",
            )
            .changed()
        {
            self.ui2.snap_angle_lock = ang;
        }
        let mut align = self.ui2.snap_align_rotation;
        if ui
            .checkbox(&mut align, "Align Rotation to Target")
            .on_hover_text(
                "Blender parity: while snapping (magnet or Ctrl), rotate the object so local +Y\n\
                 aligns to the hit surface normal (face center, mesh surface under cursor, grid floor).\n\
                 Works on Grab / Move for bodies and SDF prims. Enable Face center or leave this on\n\
                 for automatic surface raycast.",
            )
            .changed()
        {
            self.ui2.snap_align_rotation = align;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Threshold")
                    .size(11.0)
                    .color(ui_theme::TEXT_DIM),
            );
            ui.add(
                egui::DragValue::new(&mut self.viewport.smart_snap_threshold)
                    .range(5.0..=60.0)
                    .speed(0.5)
                    .suffix(" px")
                    .min_decimals(0)
                    .max_decimals(0),
            );
        });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Grid size")
                    .size(11.0)
                    .color(ui_theme::TEXT_DIM),
            );
            let g0 = self.viewport.snap_grid;
            ui.add(
                egui::DragValue::new(&mut self.viewport.snap_grid)
                    .range(0.01..=10.0)
                    .speed(0.01)
                    .suffix(" m")
                    .min_decimals(2)
                    .max_decimals(3),
            )
            .on_hover_text(
                "Grid size for lattice snap points AND transform quantize (Increment / free grid).",
            );
            // Keep workplane cell size in sync so visual floor grid matches snap lattice.
            if (self.viewport.snap_grid - g0).abs() > 1e-8 {
                self.viewport.grid_cell_size = self.viewport.snap_grid;
            }
        });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Rotation step")
                    .size(11.0)
                    .color(ui_theme::TEXT_DIM),
            );
            ui.add(
                egui::DragValue::new(&mut self.viewport.snap_rotation_deg)
                    .range(0.0..=180.0)
                    .speed(1.0)
                    .suffix(" °")
                    .min_decimals(0)
                    .max_decimals(1),
            )
            .on_hover_text(
                "Angle increment (degrees).\n\
                 • Angle lock + Move: free Grab / Extrude direction locks to this step on the workplane.\n\
                 • Rotate: angle snaps to this step (magnet or Shift).\n\
                 • Knife / polyline: rubber-band locks to this step (Shift or Snap on).\n\
                 0 = off for angle lock.",
            );
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("AFFECTS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.checkbox(&mut self.ui2.snap_affect_move, "Move")
                .on_hover_text(
                    "Geometry / grid / Angle lock apply to Grab / translate / edge Extrude",
                );
            ui.checkbox(&mut self.ui2.snap_affect_rotate, "Rotate")
                .on_hover_text("Angle step from magnet (Shift always snaps rotation when step > 0)");
            ui.checkbox(&mut self.ui2.snap_affect_scale, "Scale")
                .on_hover_text("Quantize scale factor while magnet is on");
        });

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Hold Ctrl for momentary snap")
                .size(10.0)
                .color(ui_theme::TEXT_FAINT)
                .italics(),
        );
    }

    /// Mockup `.pill` — optional icon + label + caret. Returns the response for popup anchoring.
    pub(crate) fn ui2_ctx_pill(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        icon: Option<&str>,
        caret: &str,
    ) -> egui::Response {
        self.ui2_ctx_pill_accent(ui, label, icon, caret, false)
    }

    /// Like [`Self::ui2_ctx_pill`] with optional accent colour when the feature is active.
    pub(crate) fn ui2_ctx_pill_accent(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        icon: Option<&str>,
        caret: &str,
        accent: bool,
    ) -> egui::Response {
        let text = if let Some(ic) = icon {
            format!("{ic}  {label} {caret}")
        } else {
            format!("{label} {caret}")
        };
        let base_col = if accent {
            ui_theme::SDF
        } else {
            ui_theme::TEXT_SECONDARY
        };
        let r = ui.add(
            egui::Label::new(
                egui::RichText::new(&text)
                    .size(ui_theme::FONT_TAB)
                    .color(base_col),
            )
            .sense(egui::Sense::click()),
        );
        let box_r = r.rect.expand2(egui::vec2(9.0, 4.0));
        if r.hovered() {
            ui.painter()
                .rect_filled(box_r, ui_theme::RADIUS_BUTTON, ui_theme::SURFACE_CONTROL);
            ui.painter().text(
                r.rect.center(),
                egui::Align2::CENTER_CENTER,
                &text,
                egui::FontId::proportional(ui_theme::FONT_TAB),
                if accent { ui_theme::SDF } else { ui_theme::TEXT },
            );
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        r
    }

    /// Show the Groups pill when mesh/poly context is live.
    pub(crate) fn ui2_show_polygroup_pill(&self) -> bool {
        matches!(
            self.modeling_context,
            ModelingContext::Polygonal | ModelingContext::ImportedMesh
        ) || self.ui2.active_pillar == "poly"
            || self.poly_active_body().is_some()
    }

    /// Proportional Edit pill — poly V/E/F/Group only (G/R/S falloff).
    pub(crate) fn ui2_show_prop_pill(&self) -> bool {
        self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
    }

    /// "Prop off" / "Prop · Smooth" / "Prop · Linear" …
    pub(crate) fn ui2_prop_pill_label(&self) -> String {
        if !self.poly_proportional {
            return "Prop off".into();
        }
        format!("Prop · {}", self.poly_prop_falloff.label())
    }

    /// Proportional Edit popover (Blender header O menu): on/off, falloff, radius.
    pub(crate) fn ui2_prop_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(240.0);
        ui.spacing_mut().item_spacing.y = 2.0;

        let mut on = self.poly_proportional;
        if ui
            .checkbox(
                &mut on,
                egui::RichText::new(format!("{}  Proportional editing", ph::CIRCLE_HALF))
                    .size(12.0)
                    .color(ui_theme::TEXT),
            )
            .on_hover_text("Affect nearby verts when transforming (G/R/S).\nShortcut: O · scroll adjusts radius while active.")
            .changed()
        {
            self.poly_proportional = on;
            self.toast(if on {
                "Proportional Edit ON"
            } else {
                "Proportional Edit OFF"
            });
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        ui.label(
            egui::RichText::new("FALLOFF")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_enabled_ui(self.poly_proportional, |ui| {
            for kind in [
                PropFalloff::Smooth,
                PropFalloff::Sphere,
                PropFalloff::Root,
                PropFalloff::InverseSquare,
                PropFalloff::Sharp,
                PropFalloff::Linear,
                PropFalloff::Constant,
                PropFalloff::Random,
            ] {
                let selected = self.poly_prop_falloff == kind;
                if ui
                    .selectable_label(
                        selected,
                        egui::RichText::new(kind.label()).size(12.0),
                    )
                    .clicked()
                {
                    self.poly_prop_falloff = kind;
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        ui.label(
            egui::RichText::new("RADIUS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_enabled_ui(self.poly_proportional, |ui| {
            let mut r = self.poly_prop_radius;
            let resp = ui.add(
                egui::Slider::new(&mut r, 0.05..=50.0)
                    .logarithmic(true)
                    .text("Radius")
                    .clamp_to_range(true),
            );
            if resp.changed() {
                self.poly_prop_radius = r;
                // Keep live G/R/S session in sync (same as scroll-wheel path).
                if let Some(ts) = self.tool.current.as_poly_transform_mut() {
                    if let FalloffKind::Proportional { radius, .. } = &mut ts.falloff {
                        *radius = self.poly_prop_radius;
                    }
                }
            }
            ui.label(
                egui::RichText::new("Scroll while transforming to resize")
                    .size(10.0)
                    .color(ui_theme::TEXT_FAINT)
                    .italics(),
            );
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        ui.label(
            egui::RichText::new("ORIGIN")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        ui.add_enabled_ui(self.poly_proportional, |ui| {
            let placed = self.poly_prop_origin.is_some();
            ui.label(
                egui::RichText::new(if placed {
                    "Placed — drag the amber diamond to move it"
                } else {
                    "Auto — follows the Action Centre"
                })
                .size(11.0)
                .color(if placed { ui_theme::TEXT } else { ui_theme::TEXT_FAINT }),
            );
            if ui
                .add_enabled(placed, egui::Button::new(egui::RichText::new("Reset to Auto").size(11.0)))
                .on_hover_text("Return the falloff origin to the Action Centre.")
                .clicked()
            {
                self.poly_prop_origin_reset();
            }
        });

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("O = toggle · Shift+Alt+O = cycle falloff · RMB pill = quick off")
                .size(10.0)
                .color(ui_theme::TEXT_FAINT)
                .italics(),
        );
    }

    /// Live pill text: "Groups off" / "Groups" / "Groups · 3".
    pub(crate) fn ui2_polygroup_pill_label(&self) -> String {
        if !self.ui.poly_show_polygroups {
            return "Groups off".into();
        }
        let n = self
            .poly_active_body()
            .and_then(|nid| self.poly_cd(nid))
            .map(|cd| cd.group_meta.len())
            .unwrap_or(0);
        match n {
            0 => "Groups".into(),
            n => format!("Groups · {n}"),
        }
    }

    /// Polygroups popover — overlay toggle + assign / select / list with colour swatches.
    pub(crate) fn ui2_polygroup_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(260.0);
        ui.spacing_mut().item_spacing.y = 2.0;

        // ── Overlay master switch ──────────────────────────────────────────
        let mut show = self.ui.poly_show_polygroups;
        if ui
            .checkbox(
                &mut show,
                egui::RichText::new(format!("{}  Show group colours", ph::CIRCLES_THREE))
                    .size(12.0)
                    .color(ui_theme::TEXT),
            )
            .on_hover_text("Tint faces by polygroup colour in the viewport (edit mode)")
            .changed()
        {
            self.ui.poly_show_polygroups = show;
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(if show {
                "Polygroup visualisation ON"
            } else {
                "Polygroup visualisation OFF"
            });
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Actions ────────────────────────────────────────────────────────
        ui.label(
            egui::RichText::new("ACTIONS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );
        if ui
            .button(format!("{}  Assign selection to new group", ph::PLUS))
            .on_hover_text("Create a manual polygroup from the current face selection")
            .clicked()
        {
            self.poly_assign_selection_to_new_group();
            // Turn overlay on so the new group is visible immediately.
            if !self.ui.poly_show_polygroups {
                self.ui.poly_show_polygroups = true;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        if ui
            .button("Select group under face")
            .on_hover_text("Select all faces in the polygroup of the selected/hovered face")
            .clicked()
        {
            self.poly_select_group_under_cursor();
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Group list with colour swatches ────────────────────────────────
        ui.label(
            egui::RichText::new("GROUPS")
                .size(9.0)
                .color(ui_theme::TEXT_FAINT)
                .extra_letter_spacing(0.6),
        );

        let list = self.poly_group_list();
        if list.is_empty() {
            ui.label(
                egui::RichText::new("No groups yet — bevel / extrude auto-tag, or Assign above")
                    .size(10.5)
                    .color(ui_theme::TEXT_FAINT)
                    .italics(),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for m in list {
                        let [r, g, b] = m.color;
                        let swatch = egui::Color32::from_rgb(r, g, b);
                        ui.horizontal(|ui| {
                            // Colour chip
                            let (chip, _) = ui.allocate_exact_size(
                                egui::vec2(12.0, 12.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(chip, 2.0, swatch);
                            ui.painter().rect_stroke(
                                chip,
                                2.0,
                                egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle,
                            );

                            let kind = m.kind.label();
                            let label = format!("{}  ·  {}", m.name, kind);
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(label)
                                            .size(11.5)
                                            .color(ui_theme::TEXT),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Click to select all faces in this group")
                                .clicked()
                            {
                                if !self.ui.poly_show_polygroups {
                                    self.ui.poly_show_polygroups = true;
                                }
                                self.poly_select_group(m.id);
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .small_button("🗑")
                                    .on_hover_text("Delete polygroup")
                                    .clicked()
                                {
                                    self.poly_delete_group(m.id);
                                }
                            });
                        });

                        // Inline rename
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            let mut name = m.name.clone();
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut name)
                                    .desired_width(180.0)
                                    .font(egui::TextStyle::Small)
                                    .hint_text("Rename…"),
                            );
                            if resp.lost_focus()
                                && name.trim() != m.name
                                && !name.trim().is_empty()
                            {
                                self.poly_rename_group(m.id, name.trim().to_string());
                            }
                        });
                        ui.add_space(2.0);
                    }
                });
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Bevel / Extrude / Inset auto-create groups")
                .size(10.0)
                .color(ui_theme::TEXT_FAINT)
                .italics(),
        );
    }
}
