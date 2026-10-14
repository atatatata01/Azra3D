// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
//
// Viewport shading / render-mode bar — drawn as an IN-VIEWPORT overlay in the
// top-right corner (owner: not the context bar). FOUR mode buttons
// (Wire / MatCap / PBR / Path Trace) + an HDR mute toggle + a trailing "more
// options" ⌄ arrow.
//
// Owner 2026-07-26: Solid was dropped (it shaded the same Pbr-class geometry as
// PBR with flatter lighting — no distinct value) and the second "Material" Pbr
// button was merged into PBR (its env/IBL content already lives in the PBR
// popover). MatCap is now its OWN button (unlit baked-sphere sculpt shading).
// ShadingMode::SolidHdr and ::Lit still exist in the enum (CUDA + other code
// reference them) but are NO LONGER reachable from this bar.
//
//   LEFT-CLICK a mode button  = set the shading mode.
//   RIGHT-CLICK a mode button = open that mode's settings POPOVER (in-viewport,
//                          under the button — a self-owned egui Area, NOT the
//                          dock-float path).
//   LEFT-CLICK the HDR button = mute/unmute the loaded environment (map stays
//                          resident — not Clear). No HDR loaded → open settings.
//   RIGHT-CLICK the HDR button = open EnvSettings popover (load/clear, intensity,
//                          background blur vs reflection blur, …).
//   LEFT-CLICK the ⌄     = open the GLOBAL render-settings popover (camera
//                          Perspective/Ortho + view presets + global body).
//
// Each popover carries a small ⊟ "detach" button that pops the SAME settings out
// into a real dockable `PanelKind` panel via `ui2_reveal_panel` — so
// detach-to-dock is preserved; the in-viewport popover is just the default.
//
// The overlay only interacts within the button rects + popover rect; every other
// pixel of the viewport falls through to camera/tool input (critical bug class —
// the bar must never eat an orbit drag over empty viewport).
//
// Design: docs/UI/VIEWPORT_SHADING_BAR_PLAN.md. Mode→ShadingMode mapping is the
// authoritative viewport_render.rs:1019 vm table.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::*;

/// One shading-bar button. Each button now maps 1:1 to a distinct ShadingMode
/// (Wire / MatCap / PBR / Path Trace) — no Pbr-pair disambiguation any more.
struct ShadeBtn {
    icon: &'static str,
    tip: &'static str,
    mode: ShadingMode,
    panel: PanelKind,
}

impl FourthLayerApp {
    /// Reveal (or focus) a settings `PanelKind` as a real dockable panel. If it
    /// already lives in a group, un-hide that zone + make it the active tab;
    /// otherwise spawn it floating under the bar so it doesn't shove the dock.
    /// The user can then drag it into a dock — all dock/float/tab/persist
    /// plumbing is generic over `PanelKind` (dock.rs), so no new dock code is
    /// needed. `anchor` = a screen point used only for the initial float pos.
    ///
    /// This is the DETACH target for the in-viewport popover's ⊟ button; it is
    /// deliberately left intact so the docked-panel path still works.
    pub(crate) fn ui2_reveal_panel(
        &mut self,
        ctx: &egui::Context,
        kind: PanelKind,
        anchor: egui::Pos2,
    ) {
        // Already present anywhere? Un-hide its zone + activate the tab.
        let existing = self.ui2.dock.iter().position(|g| g.tabs.contains(&kind));
        if let Some(gi) = existing {
            let side = self.ui2.dock[gi].side;
            match side {
                DockSide::Left => self.ui2.left_hidden = false,
                DockSide::Right => self.ui2.right_hidden = false,
                DockSide::Top | DockSide::Bottom => {}
                DockSide::Float => {}
            }
            let g = &mut self.ui2.dock[gi];
            g.collapsed = false;
            g.head_coll = false;
            if let Some(ti) = g.tabs.iter().position(|t| *t == kind) {
                g.active = ti;
            }
            return;
        }
        // Not present → spawn a floating panel under the bar.
        let mut g = self.ui2.mk(vec![kind], DockSide::Float);
        g.size = if kind == PanelKind::Library {
            egui::vec2(720.0, 420.0)
        } else {
            egui::vec2(300.0, 420.0)
        };
        let wanted = if kind == PanelKind::Library {
            egui::pos2(anchor.x - 18.0, anchor.y - g.size.y - 18.0)
        } else {
            egui::pos2(anchor.x - 150.0, anchor.y + 6.0)
        };
        let screen = ctx.screen_rect();
        g.pos = egui::pos2(
            wanted.x.clamp(screen.left(), (screen.right() - g.size.x).max(screen.left())),
            wanted.y.clamp(screen.top(), (screen.bottom() - g.size.y).max(screen.top())),
        );
        self.ui2.dock.push(g);
    }

    /// In-viewport shading overlay: the mode-button well anchored to the
    /// top-right of the scene `rect`, plus its settings popover. Called from
    /// `ui2_draw_scene_overlays`. `rect` is the active-pane content rect (in
    /// split mode it is the active pane, so the bar sits in that pane's corner
    /// and never overlaps the right dock, whose columns are outside `rect`).
    pub(crate) fn ui2_shading_overlay(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: egui::Rect,
    ) {
        // Four mode buttons, left→right. Each maps to one distinct ShadingMode
        // (owner 2026-07-26): Wire / MatCap / PBR / Path Trace. HDR is a separate
        // mute toggle (not a shading mode) drawn after these.
        let buttons: [ShadeBtn; 4] = [
            ShadeBtn { icon: ph::BOUNDING_BOX, tip: "Wire — DC mesh edges (RMB: settings)",
                mode: ShadingMode::Wireframe, panel: PanelKind::WireSettings },
            ShadeBtn { icon: ph::CIRCLE_HALF, tip: "MatCap — unlit baked-sphere shading (RMB: settings)",
                mode: ShadingMode::MatCap, panel: PanelKind::MatCapSettings },
            ShadeBtn { icon: ph::SUN, tip: "PBR — lit + full post-FX (RMB: settings)",
                mode: ShadingMode::Pbr, panel: PanelKind::PbrSettings },
            ShadeBtn { icon: ph::SPARKLE, tip: "Path Trace — progressive PT (RMB: settings)",
                mode: ShadingMode::PathTrace, panel: PanelKind::PtSettings },
        ];

        let cur = self.render.shading_mode;
        let cell = egui::vec2(28.0, 24.0);
        let pad = 2.0;
        let gap = 1.0;
        let n = buttons.len() as f32;
        // Well = 4 modes + HDR toggle + trailing ⌄.
        let extra_cells = 2.0; // HDR + caret
        let arrow_w = cell.x;
        let well_w = cell.x * (n + extra_cells) + pad * 2.0 + gap * (n + extra_cells - 1.0);
        let well_h = cell.y + pad * 2.0;

        // Anchor top-right of the viewport rect, inset ~10px, clear of the dock.
        let inset = 10.0;
        let well = egui::Rect::from_min_size(
            egui::pos2(rect.right() - well_w - inset, rect.top() + inset),
            egui::vec2(well_w, well_h),
        );
        // Bail if the pane is too small to host the bar without overlapping content.
        if well.left() < rect.left() + 4.0 {
            return;
        }

        // Right-click is read once from the pointer state — `resp.secondary_clicked()`
        // on a bare interact region is unreliable (misses on already-hovered cells),
        // so we combine `contains_pointer()` with the raw pointer event.
        let secondary_click = ui.input(|i| i.pointer.secondary_clicked());
        let primary_click = ui.input(|i| i.pointer.primary_clicked());
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());

        ui.painter().rect_filled(well, 6.0, ui_theme::SURFACE_WELL);
        ui.painter().rect_stroke(well, 6.0, egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle);

        let mut x = well.left() + pad;
        let cy = well.center().y;
        // Popover requests collected this frame (applied after the loop so we
        // don't borrow-conflict with the button drawing).
        let mut open_popover: Option<PanelKind> = None;
        let mut set_mode: Option<ShadingMode> = None;

        for b in &buttons {
            let r = egui::Rect::from_center_size(egui::pos2(x + cell.x * 0.5, cy), cell);
            let id = ui.make_persistent_id(("v2_shade", b.tip));
            let resp = ui.interact(r, id, egui::Sense::click());
            let over = resp.contains_pointer();

            // Active-highlight: primary mode matches (+ its siblings for PBR/PT).
            // PBR is now the sole lit button, so it also lights for the (no-longer-
            // bar-reachable) SolidHdr/Lit modes should any other code select them.
            let on = match b.mode {
                ShadingMode::Wireframe => matches!(cur, ShadingMode::Wireframe),
                ShadingMode::MatCap => matches!(cur, ShadingMode::MatCap),
                ShadingMode::Pbr => {
                    matches!(cur, ShadingMode::Pbr | ShadingMode::SolidHdr | ShadingMode::Lit)
                }
                ShadingMode::PathTrace => {
                    matches!(cur, ShadingMode::PathTrace | ShadingMode::Raytrace)
                }
                _ => cur == b.mode,
            };
            // Highlight the button whose popover is currently open too.
            let popover_open_here = self.ui2.shade_popover == Some(b.panel);

            if on || popover_open_here {
                ui.painter().rect_filled(r, 5.0, ui_theme::SURFACE_CONTROL);
            }
            let col = if on {
                ui_theme::ACCENT_LIGHT
            } else if over || popover_open_here {
                ui_theme::TEXT_SECONDARY
            } else {
                ui_theme::TEXT_DIM
            };
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                b.icon,
                egui::FontId::proportional(16.0),
                col,
            );
            if over {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            }
            // LEFT-CLICK → set mode (applied after the loop).
            if resp.clicked() {
                set_mode = Some(b.mode);
            }
            // RIGHT-CLICK → toggle that mode's in-viewport settings popover.
            if secondary_click && over {
                open_popover = Some(b.panel);
            }
            resp.on_hover_text(b.tip);
            x += cell.x + gap;
        }

        // ── HDR mute toggle (not a shading mode) ─────────────────────────────
        // LMB: mute/unmute (map stays loaded). No map yet → open settings to Load.
        // RMB: EnvSettings popover (load/clear, intensity, dual blur, …).
        let hdr_r = egui::Rect::from_center_size(egui::pos2(x + cell.x * 0.5, cy), cell);
        let hdr_id = ui.make_persistent_id("v2_shade_hdr");
        let hdr_resp = ui.interact(hdr_r, hdr_id, egui::Sense::click());
        let hdr_over = hdr_resp.contains_pointer();
        let hdr_loaded = self.env_hdr_width > 0;
        let hdr_on = hdr_loaded && self.render.env_enabled;
        let hdr_pop_open = self.ui2.shade_popover == Some(PanelKind::EnvSettings);
        if hdr_on || hdr_pop_open {
            ui.painter().rect_filled(hdr_r, 5.0, ui_theme::SURFACE_CONTROL);
        }
        let hdr_col = if hdr_on {
            ui_theme::ACCENT_LIGHT
        } else if hdr_over || hdr_pop_open {
            ui_theme::TEXT_SECONDARY
        } else {
            ui_theme::TEXT_DIM
        };
        ui.painter().text(
            hdr_r.center(),
            egui::Align2::CENTER_CENTER,
            ph::GLOBE,
            egui::FontId::proportional(16.0),
            hdr_col,
        );
        if hdr_over {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        let mut toggle_hdr = false;
        if hdr_resp.clicked() {
            if hdr_loaded {
                toggle_hdr = true;
            } else {
                // Nothing to mute — surface the load controls.
                open_popover = Some(PanelKind::EnvSettings);
            }
        }
        if secondary_click && hdr_over {
            open_popover = Some(PanelKind::EnvSettings);
        }
        hdr_resp.on_hover_text(if hdr_loaded {
            if self.render.env_enabled {
                "HDR on — left-click to mute (RMB: settings)"
            } else {
                "HDR muted — left-click to enable (RMB: settings)"
            }
        } else {
            "HDR / Environment — load an HDRI (RMB: settings)"
        });
        x += cell.x + gap;

        // Trailing ⌄ → GLOBAL render-settings popover (camera modes + global body).
        let ar = egui::Rect::from_center_size(egui::pos2(x + arrow_w * 0.5, cy), cell);
        let aid = ui.make_persistent_id("v2_shade_more");
        let aresp = ui.interact(ar, aid, egui::Sense::click());
        let a_over = aresp.contains_pointer();
        let a_open = self.ui2.shade_popover == Some(PanelKind::RenderSettingsGlobal);
        if a_over || a_open {
            ui.painter().rect_filled(ar, 5.0, ui_theme::SURFACE_CONTROL);
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        ui.painter().text(
            ar.center(),
            egui::Align2::CENTER_CENTER,
            ph::CARET_DOWN,
            egui::FontId::proportional(14.0),
            if a_over || a_open { ui_theme::TEXT } else { ui_theme::TEXT_DIM },
        );
        // Caret opens on either button — it is the global "more" affordance.
        if aresp.clicked() || (secondary_click && a_over) {
            open_popover = Some(PanelKind::RenderSettingsGlobal);
        }
        aresp.on_hover_text("Viewport overlays · camera · wire · selection");

        // ── Apply HDR mute ───────────────────────────────────────────────────
        if toggle_hdr {
            self.render.env_enabled = !self.render.env_enabled;
            self.mark_dirty(DirtyFlags::RENDER);
        }

        // ── Apply mode change ────────────────────────────────────────────────
        if let Some(mode) = set_mode {
            self.render.shading_mode = mode;
            match mode {
                ShadingMode::MatCap | ShadingMode::Lit => self.render.dither_enabled = false,
                ShadingMode::Pbr | ShadingMode::Raytrace => {} // keep preference; default off
                _ => {}
            }
            if mode == ShadingMode::PathTrace {
                self.render.pt_accumulated_frames = 0;
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }

        // ── Apply popover open/toggle ────────────────────────────────────────
        if let Some(k) = open_popover {
            // Toggle: right-clicking the same button again closes it.
            self.ui2.shade_popover = if self.ui2.shade_popover == Some(k) { None } else { Some(k) };
        }

        // ── Draw the open popover (if any) ───────────────────────────────────
        if let Some(kind) = self.ui2.shade_popover {
            // Anchor under the button that owns this panel (or the caret / HDR).
            let anchor_x = if kind == PanelKind::RenderSettingsGlobal {
                ar.center().x
            } else if kind == PanelKind::EnvSettings {
                hdr_r.center().x
            } else {
                buttons
                    .iter()
                    .position(|b| b.panel == kind)
                    .map(|i| well.left() + pad + cell.x * (i as f32 + 0.5) + gap * i as f32)
                    .unwrap_or(well.center().x)
            };
            let anchor_y = well.bottom() + 4.0;
            let pop_w = 288.0_f32.min(rect.width() - 20.0).max(180.0);
            // Keep the popover fully inside the viewport rect horizontally.
            let pop_x = (anchor_x - pop_w * 0.5)
                .clamp(rect.left() + 6.0, (rect.right() - pop_w - 6.0).max(rect.left() + 6.0));
            let max_h = (rect.height() - (anchor_y - rect.top()) - 12.0).clamp(120.0, 560.0);

            let close_clicked = self.ui2_draw_shade_popover(
                ctx, kind, egui::pos2(pop_x, anchor_y), pop_w, max_h,
            );
            if close_clicked {
                self.ui2.shade_popover = None;
            }

            // ── Close-on-outside-click / Esc ─────────────────────────────────
            // The popover Area reports its own rect via memory; compute a rough
            // interactive region = the popover Area rect ∪ the bar well. A
            // primary/secondary click outside both closes it. Esc always closes.
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                self.ui2.shade_popover = None;
            } else if (primary_click || secondary_click) && open_popover.is_none() {
                // A click landed this frame and it was NOT on a bar button (those
                // set open_popover). If it is also outside the popover Area, close.
                let pop_area_id = egui::Id::new(("v2_shade_pop", kind_ord(kind)));
                let inside_popover = ctx
                    .memory(|m| m.area_rect(pop_area_id))
                    .map(|ar2| pointer_pos.map_or(false, |p| ar2.contains(p)))
                    .unwrap_or(false);
                let inside_bar = pointer_pos.map_or(false, |p| well.contains(p));
                if !inside_popover && !inside_bar {
                    self.ui2.shade_popover = None;
                }
            }
        }
    }

    /// Draw the in-viewport settings popover for `kind` at screen `pos`. Returns
    /// `true` when the user asked to CLOSE it (the ✕ button). The ⊟ detach button
    /// pops the same settings into a dockable panel and also closes the popover.
    ///
    /// Content is the SAME `render_settings.rs` body used by the docked panels —
    /// reused, not re-authored (No-Duplication rule).
    fn ui2_draw_shade_popover(
        &mut self,
        ctx: &egui::Context,
        kind: PanelKind,
        pos: egui::Pos2,
        width: f32,
        max_h: f32,
    ) -> bool {
        let mut want_close = false;
        let area_id = egui::Id::new(("v2_shade_pop", kind_ord(kind)));
        egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(ctx.screen_rect())
            .show(ctx, |ui| {
                let frame = egui::Frame::none()
                    .fill(ui_theme::SURFACE_PANEL)
                    .stroke(egui::Stroke::new(1.0, ui_theme::SEAM))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 8));
                frame.show(ui, |ui| {
                    ui.set_width(width - 20.0);
                    // ── Header row: title + detach + close ───────────────────
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(kind.label())
                                .size(11.5)
                                .strong()
                                .color(ui_theme::TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Close ✕
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new(ph::X).size(12.0).color(ui_theme::TEXT_DIM),
                                ).fill(egui::Color32::TRANSPARENT).frame(false))
                                .on_hover_text("Close")
                                .clicked()
                            {
                                want_close = true;
                            }
                            // Detach ⊟ → real dockable panel.
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new(ph::ARROW_SQUARE_OUT)
                                        .size(12.0)
                                        .color(ui_theme::TEXT_DIM),
                                ).fill(egui::Color32::TRANSPARENT).frame(false))
                                .on_hover_text("Detach into a dockable panel")
                                .clicked()
                            {
                                let anchor = egui::pos2(pos.x + width * 0.5, pos.y);
                                self.ui2_reveal_panel(ui.ctx(), kind, anchor);
                                want_close = true;
                            }
                        });
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .max_height(max_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            match kind {
                                PanelKind::WireSettings => self.render_wire_settings_body(ui),
                                PanelKind::SolidSettings => self.render_solid_settings_body(ui),
                                // Owner 2026-08-06: "WHERE IS MY MATCAP BG" — MatCap button
                                // opened this popover but the match never drew the body, so
                                // BG Color / Gradient / Rotation / Edge Lines were missing.
                                PanelKind::MatCapSettings => self.render_matcap_settings_body(ui),
                                PanelKind::MaterialSettings => {
                                    // Material == exposure + env/IBL (same as ui2_draw_material_settings).
                                    if self.env_hdr_width > 0 {
                                        if ui.add(egui::Slider::new(&mut self.render.exposure, 0.01..=5.0)
                                            .text("Exposure")
                                            .clamping(egui::SliderClamping::Never)).changed()
                                        {
                                            self.mark_dirty(DirtyFlags::RENDER);
                                        }
                                        ui.separator();
                                    }
                                    self.render_env_ibl_body(ui);
                                }
                                PanelKind::EnvSettings => self.render_env_ibl_body(ui),
                                PanelKind::PbrSettings => {
                                    egui::CollapsingHeader::new("Environment / IBL")
                                        .default_open(true)
                                        .show(ui, |ui| self.render_env_ibl_body(ui));
                                    ui.separator();
                                    self.render_pbr_postfx_body(ui);
                                }
                                PanelKind::PtSettings => self.render_pt_settings_body(ui),
                                PanelKind::RenderSettingsGlobal => {
                                    // Camera + viewport overlays (no quality ladder / no view presets).
                                    self.render_global_settings_body(ui);
                                }
                                _ => {}
                            }
                        });
                });
            });
        want_close
    }

}

/// Stable small ordinal per settings PanelKind, for the popover Area id. Keeps
/// the popover id distinct per panel so re-opening a different panel does not
/// inherit stale Area state.
fn kind_ord(k: PanelKind) -> u8 {
    match k {
        PanelKind::WireSettings => 0,
        PanelKind::SolidSettings => 1,
        PanelKind::MaterialSettings => 2,
        PanelKind::PbrSettings => 3,
        PanelKind::PtSettings => 4,
        PanelKind::RenderSettingsGlobal => 5,
        PanelKind::MatCapSettings => 6,
        PanelKind::EnvSettings => 7,
        _ => 8,
    }
}
