// Pie menus + pivot center dropdown (v2 host surface).
use eframe::egui;
use crate::*;

impl FourthLayerApp {
    /// Paint all pie menus (Tab mode pie, Z shading, View, F, …). Same defs +
    /// `draw_pie_menu` as v1 overlays — only the host surface differs.
    /// `draw_pie_menu` no-ops when a pie is closed and fully faded out.
    pub(crate) fn ui2_draw_pies(&mut self, ctx: &egui::Context) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new("v2_pies")));

        // Z shading
        {
            let z_def = crate::pie_menu::z_pie_def();
            let active = match self.render.shading_mode {
                ShadingMode::Pbr => Some(0), ShadingMode::Raytrace => Some(1),
                ShadingMode::PathTrace => Some(2), ShadingMode::MatCap => Some(3),
                ShadingMode::Lit => Some(4), ShadingMode::SolidHdr => Some(5),
                ShadingMode::Wireframe => Some(6),
            };
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.z_pie, &z_def, active);
        }
        // View
        {
            let view_def = crate::pie_menu::view_pie_def();
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.view_pie, &view_def, self.ui.last_view_idx);
        }
        // Tab — selection / work mode (mockup hold-Tab pie)
        {
            let tab_def = crate::pie_menu::tab_pie_def_ex(
                self.ui.pie_mode,
                self.poly_pie_shows_group(),
            );
            let active_inner = match self.ui.pie_mode {
                crate::pie_menu::PieMode::Sdf => {
                    if self.ui.sdf_cycle_mode { Some(3) }
                    else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0),
                            WorkMode::Vertex => Some(1),
                            _ => None,
                        }
                    }
                }
                crate::pie_menu::PieMode::Poly => {
                    if self.tool.work_mode == WorkMode::Sculpt {
                        Some(5)
                    } else if self.modeling_context == ModelingContext::Polygonal {
                        match self.poly_work_mode {
                            PolyWorkMode::Object => Some(0),
                            PolyWorkMode::Face => Some(2),
                            PolyWorkMode::Group => Some(3),
                            PolyWorkMode::Edge => Some(4),
                            PolyWorkMode::Vertex => Some(6),
                        }
                    } else {
                        match self.tool.work_mode {
                            WorkMode::Object => Some(0), WorkMode::Face => Some(2),
                            WorkMode::Edge => Some(4), WorkMode::Sculpt => Some(5),
                            WorkMode::Vertex => Some(6),
                        }
                    }
                }
                crate::pie_menu::PieMode::Nurbs => {
                    match self.nurbs_work_mode {
                        NurbsWorkMode::Object => Some(0),
                        NurbsWorkMode::Face => Some(2),
                        NurbsWorkMode::Edge => Some(4),
                        NurbsWorkMode::Vertex => Some(6),
                    }
                }
            };
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.tab_pie, &tab_def, active_inner);
        }
        // Reset / F / D / bool pies
        {
            let reset_def = crate::pie_menu::reset_pie_def();
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.reset_pie, &reset_def, None);
        }
        // T (Action-Center) pie — Poly modeling. Highlights the current center mode.
        if self.ui.t_pie.open {
            let t_def = crate::pie_menu::t_pie_def(self.tool.action_center.label());
            let active = match self.tool.action_center {
                crate::ActionCenter::Automatic => Some(0),
                crate::ActionCenter::Selection => Some(1),
                crate::ActionCenter::Workplane => Some(2),
                crate::ActionCenter::Element => Some(3),
                crate::ActionCenter::Screen => Some(4),
                crate::ActionCenter::Origin => Some(5),
                crate::ActionCenter::Pivot => Some(6),
                crate::ActionCenter::None => Some(7),
                // Sel Auto Axis kept in enum for hotkeys; no dedicated pie slice.
                crate::ActionCenter::SelectionCenterAutoAxis => None,
            };
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.t_pie, &t_def, active);
        }
        {
            let f_def = crate::pie_menu::f_pie_def(
                self.scene.isolated_body_nid.is_some(), self.isolate_context_ghost);
            let active = if self.scene.isolated_body_nid.is_some() { Some(1) } else { None };
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.f_pie, &f_def, active);
        }
        // Shift+O overlay pie — center label shows master ON/OFF
        {
            let o_def = crate::pie_menu::overlay_pie_def(self.viewport.show_overlays);
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.overlay_pie, &o_def, None);
        }
        {
            let persistent = self.get_active_pivot_persistent();
            let d_def = crate::pie_menu::d_pie_def(persistent);
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.d_pie, &d_def, None);
        }
        {
            let bool_def = crate::pie_menu::boolean_pie_def(self.ui.bool_pie_keep_cutter);
            // 8-sector pie: Keep Cutter is slot 6 (was 3 on the old 4-item pie).
            let active = if self.ui.bool_pie_keep_cutter { Some(6) } else { None };
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.bool_pie, &bool_def, active);
        }
        {
            let sdf_bool_def = crate::pie_menu::sdf_boolean_pie_def();
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.sdf_bool_pie, &sdf_bool_def, None);
        }
        // Alt+A poly Align / Flatten pie (view directions + workplane center)
        {
            let align_def = crate::pie_menu::align_pie_def();
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.align_pie, &align_def, None);
        }
        // Alt+M poly Merge pie (Average / Last / By Distance)
        {
            let merge_def = crate::pie_menu::merge_pie_def();
            crate::pie_menu::draw_pie_menu(&painter, &self.ui.merge_pie, &merge_def, None);
        }
        // Ctrl+R SDF resolution pie (shared draw path — current slice highlighted)
        self.draw_resolution_pie_overlay(&painter);
        // Yellow-dot center menu (opened from Tab pie OBJECT accessory)
        self.draw_pivot_center_menu(ctx);
    }

    /// Dropdown after clicking the yellow pivot/center dot next to OBJECT on the Tab pie.
    pub(crate) fn draw_pivot_center_menu(&mut self, ctx: &egui::Context) {
        if !self.ui.pivot_center_menu_open {
            return;
        }
        let mut open = true;
        let pos = self.ui.pivot_center_menu_pos;
        let mut pick: Option<u8> = None;
        // Full-screen click-catcher UNDER the popup (Middle < Foreground): a primary
        // click outside the menu lands here — closing the menu AND consuming the
        // click (egui owns it, so it never falls through to the viewport). Clicks on
        // the popup itself hit the Foreground layer first and are unaffected.
        let screen = ctx.screen_rect();
        let backdrop = egui::Area::new(egui::Id::new("v2_pivot_center_backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .movable(false)
            .show(ctx, |ui| ui.allocate_rect(screen, egui::Sense::click()));
        if backdrop.inner.clicked() {
            open = false;
        }
        egui::Area::new(egui::Id::new("v2_pivot_center_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .movable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(crate::ui_theme::SURFACE_CARD)
                    .stroke(egui::Stroke::new(1.0, crate::ui_theme::BORDER))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(200.0);
                        ui.label(
                            egui::RichText::new("OBJECT CENTER")
                                .size(10.0)
                                .color(crate::ui_theme::TEXT_SECTION)
                                .strong(),
                        );
                        ui.separator();
                        if ui
                            .button("Reset to mass / bounds")
                            .on_hover_text("Pivot at bounding-box center (≈ mass for uniform solids)")
                            .clicked()
                        {
                            pick = Some(0);
                        }
                        if ui
                            .button("Center to origin")
                            .on_hover_text("Pivot at world origin (0, 0, 0)")
                            .clicked()
                        {
                            pick = Some(1);
                        }
                        if ui
                            .button("Center to workplane")
                            .on_hover_text("Pivot at current workplane origin")
                            .clicked()
                        {
                            pick = Some(2);
                        }
                        ui.separator();
                        if ui
                            .button("Edit pivot (drag)…")
                            .on_hover_text("Enter pivot edit mode — drag to place (D key)")
                            .clicked()
                        {
                            pick = Some(3);
                        }
                        if ui
                            .button("Pivot placement pie…")
                            .on_hover_text("Open full D-key pivot pie (Origin / Face / Corner / …)")
                            .clicked()
                        {
                            pick = Some(4);
                        }
                    });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
            // Esc on open menu also clears pivot expose
            self.ui.expose_selection_pivot = false;
        }
        if let Some(p) = pick {
            open = false;
            self.ui.expose_selection_pivot = true;
            match p {
                0 => self.pivot_center_to_mass(),
                1 => self.pivot_center_to_origin(),
                2 => self.pivot_center_to_workplane(),
                3 => self.enter_pivot_mode(),
                4 => {
                    let n = crate::pie_menu::d_pie_def(self.get_active_pivot_persistent())
                        .inner
                        .items
                        .len();
                    self.ui.d_pie.open(pos, n);
                }
                _ => {}
            }
        }
        self.ui.pivot_center_menu_open = open;
    }
}
