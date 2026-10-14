// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_app_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::none()
            .fill(ui_theme::SURFACE_APPBAR)
            .inner_margin(egui::Margin::symmetric(12, 0));
        egui::TopBottomPanel::top("v2_appbar")
            .exact_height(ui_theme::H_APPBAR)
            .frame(frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                // Bottom seam only (mockup border-bottom:1px solid var(--seam)).
                ui.painter().hline(
                    bar.x_range(),
                    bar.bottom() - 0.5,
                    egui::Stroke::new(1.0, ui_theme::SEAM),
                );
                // Claim the full bar so subsequent paint/interact rects are valid.
                let _ = ui.allocate_rect(bar, egui::Sense::hover());

                // —— LEFT: brand + categorized menus ——
                ui.allocate_ui_at_rect(bar, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // .brand .mark — 20×20, r5, accent fill, "A"
                        let (mark, _) =
                            ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                        ui.painter().rect_filled(mark, 5.0, ui_theme::ACCENT);
                        ui.painter().text(
                            mark.center(),
                            egui::Align2::CENTER_CENTER,
                            "A",
                            egui::FontId::proportional(12.0),
                            egui::Color32::WHITE,
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("AZRA")
                                .strong()
                                .size(ui_theme::FONT_UI)
                                .color(ui_theme::TEXT),
                        );
                        // Gap brand → menus (clearer than menu-to-menu spacing).
                        ui.add_space(14.0);
                        // Menu strip: Modo-like breathing room between File / Edit / …
                        // (was item_spacing 2 + pad 6 — labels read as one run-on word).
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
                        ui.style_mut().visuals.button_frame = false;
                        egui::menu::bar(ui, |ui| {
                            // Re-apply spacing inside menu::bar — it resets Style in some egui versions.
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
                            self.ui2_menu_file(ui, ctx);
                            self.ui2_menu_edit(ui);
                            self.ui2_menu_add(ui);
                            self.ui2_menu_select(ui);
                            self.ui2_menu_object(ui);
                            self.ui2_menu_mesh(ui);
                            self.ui2_menu_materials(ui);
                            self.ui2_menu_render(ui);
                            self.ui2_menu_view(ui);
                            self.ui2_menu_window(ui);
                            self.ui2_menu_help(ui);
                        });
                    });
                });

                // ── RIGHT: search · tweaks · Share (mockup .appbar-right) ────
                let mut right_w = 0.0;
                ui.allocate_ui_at_rect(bar, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Share button — mockup `.share`
                        let share = ui.add(
                            egui::Label::new(
                                egui::RichText::new("Share")
                                    .size(ui_theme::FONT_TAB)
                                    .color(ui_theme::TEXT),
                            )
                            .sense(egui::Sense::click()),
                        );
                        {
                            let br = share.rect.expand2(egui::vec2(12.0, 5.0));
                            ui.painter().rect_filled(
                                br,
                                ui_theme::RADIUS_BUTTON,
                                ui_theme::SURFACE_CONTROL,
                            );
                            ui.painter().rect_stroke(
                                br,
                                ui_theme::RADIUS_BUTTON,
                                egui::Stroke::new(
                                    1.0,
                                    if share.hovered() {
                                        ui_theme::BORDER_HOVER
                                    } else {
                                        ui_theme::BORDER
                                    },
                                ), egui::StrokeKind::Middle,
                            );
                            ui.painter().text(
                                share.rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Share",
                                egui::FontId::proportional(ui_theme::FONT_TAB),
                                ui_theme::TEXT,
                            );
                            if share.hovered() {
                                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                            }
                            if share.clicked() {
                                self.toast("Share — coming soon");
                            }
                        }
                        ui.add_space(4.0);
                        if self.ui2_ibtn(ui, ph::SLIDERS_HORIZONTAL, "Tweaks") {
                            self.ui.show_tweak_panel = !self.ui.show_tweak_panel;
                        }
                        ui.add_space(4.0);
                        if self.ui2_ibtn(ui, ph::MAGNIFYING_GLASS, "Search") {
                            self.ui.command_palette.open = !self.ui.command_palette.open;
                            if self.ui.command_palette.open {
                                self.ui.command_palette.search.clear();
                                self.ui.command_palette.selected_idx = 0;
                            }
                        }
                        right_w = ui.min_rect().width();
                    });
                });

                // ── CENTER: workspace tabs (mockup .tabs margin:0 auto) ──────
                let tabs = WorkspaceTab::ALL;
                let tab_pad_x = 12.0;
                let tab_inner_h = 22.0;
                let strip_pad = 3.0;
                let gap = 2.0;
                let tab_ws: Vec<f32> = tabs
                    .iter()
                    .map(|t| {
                        let galley = ui.fonts(|f| {
                            f.layout_no_wrap(
                                t.label().to_string(),
                                egui::FontId::proportional(ui_theme::FONT_TAB),
                                ui_theme::TEXT,
                            )
                        });
                        galley.size().x + tab_pad_x * 2.0
                    })
                    .collect();
                let strip_w = tab_ws.iter().sum::<f32>()
                    + gap * (tabs.len().saturating_sub(1) as f32)
                    + strip_pad * 2.0;
                let strip_h = tab_inner_h + strip_pad * 2.0;
                let mut strip = egui::Rect::from_center_size(
                    egui::pos2(bar.center().x, bar.center().y),
                    egui::vec2(strip_w, strip_h),
                );
                // Keep clear of the right cluster on narrow windows.
                let right_edge = bar.right() - right_w - 8.0;
                if strip.right() > right_edge {
                    strip = strip.translate(egui::vec2(right_edge - strip.right(), 0.0));
                }
                ui.painter().rect_filled(strip, ui_theme::RADIUS_CARD, ui_theme::SURFACE_FRAME);
                ui.painter().rect_stroke(
                    strip,
                    ui_theme::RADIUS_CARD,
                    egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle,
                );
                let mut x = strip.left() + strip_pad;
                let y = strip.center().y;
                for (i, tab) in tabs.iter().enumerate() {
                    let w = tab_ws[i];
                    let trect = egui::Rect::from_center_size(
                        egui::pos2(x + w * 0.5, y),
                        egui::vec2(w, tab_inner_h),
                    );
                    let id = ui.make_persistent_id(("v2_wstab", tab.label()));
                    let resp = ui.interact(trect, id, egui::Sense::click());
                    let on = self.ui2.workspace == *tab;
                    if on {
                        ui.painter().rect_filled(trect, 6.0, ui_theme::ACCENT);
                    } else if resp.hovered() {
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                    }
                    let col = if on {
                        egui::Color32::WHITE
                    } else if resp.hovered() {
                        ui_theme::TEXT_SECONDARY
                    } else {
                        ui_theme::TEXT_DIM
                    };
                    ui.painter().text(
                        trect.center(),
                        egui::Align2::CENTER_CENTER,
                        tab.label(),
                        egui::FontId::proportional(ui_theme::FONT_TAB),
                        col,
                    );
                    // Activity dot on inactive SDF (mockup .tab .dot).
                    if tab.show_dot() && !on {
                        let d = egui::pos2(trect.right() - 4.0, trect.top() + 3.0);
                        ui.painter().circle_filled(d, 2.5, ui_theme::ACCENT);
                    }
                    if resp.clicked() {
                        self.ui2.workspace = *tab;
                        // Light wiring where a real mode exists; others stay chrome for now.
                        match tab {
                            WorkspaceTab::Sculpt => {
                                self.tool.work_mode = WorkMode::Sculpt;
                            }
                            WorkspaceTab::Model => {
                                if self.tool.work_mode == WorkMode::Sculpt {
                                    self.tool.work_mode = WorkMode::Object;
                                }
                            }
                            WorkspaceTab::Sdf => {
                                if let Some(g) = self.ui2.dock.iter_mut().find(|g| {
                                    g.tabs.iter().any(|t| matches!(t, PanelKind::ToolsSdf))
                                }) {
                                    if let Some(ai) =
                                        g.tabs.iter().position(|t| matches!(t, PanelKind::ToolsSdf))
                                    {
                                        g.active = ai;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    x += w + gap;
                }
            });
    }

    // Top menus live in ui2/menus.rs — do not re-add menu trees here.

    /// Mockup `.ibtn` — 28×28 icon button in the app bar right cluster.
    pub(crate) fn ui2_ibtn(&mut self, ui: &mut egui::Ui, icon: &str, tip: &str) -> bool {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui_theme::IBTN_SIZE, ui_theme::IBTN_SIZE),
            egui::Sense::click(),
        );
        if resp.hovered() {
            ui.painter().rect_filled(rect, ui_theme::RADIUS_BUTTON, ui_theme::SURFACE_CONTROL);
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        let col = if resp.hovered() { ui_theme::TEXT } else { ui_theme::TEXT_LABEL };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(16.0),
            col,
        );
        resp.clone().on_hover_text(tip);
        resp.clicked()
    }
}
