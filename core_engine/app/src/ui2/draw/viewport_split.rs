// Viewport scene chrome + dual-pane split (corner drag, seam, join).
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::super::*;

impl FourthLayerApp {
    /// Viewport chrome on the 3D scene (not under the library):
    ///   · `.tool-options` bottom-center floating bar — active tool + XYZ readout
    /// (Workspace title / "SDF · Scene" label removed — top menu bar owns that role.)
    /// Pure paint/layout; does not steal camera input (tool-options is a small Area).
    pub(crate) fn ui2_viewport_scene_chrome(&mut self, ctx: &egui::Context, scene: egui::Rect) {
        if scene.width() < 80.0 || scene.height() < 80.0 {
            return;
        }
        // Shift+A / RMB context / Ctrl+M assign own the viewport chrome stack — don't cover
        // them with the Box Tool bar (order alone is not enough if the bar is painted later
        // in the same frame before those popups exist).
        if self.ui.show_add_popup || self.ui.ctx_menu_open || self.ui.show_mat_assign {
            return;
        }

        // —— .tool-options — bottom-center floating bar ——
        // Mockup: left:50%; bottom:16px; transform:translateX(-50%); card + border.
        // Layer: Foreground (below Order::Tooltip popups like Shift+A).
        let tool_name = if self.ui2.active_tool.is_empty() {
            "Select".to_string()
        } else {
            format!("{} Tool", self.ui2.active_tool)
        };
        // Read active node position for XYZ if any.
        let (px, py, pz) = self
            .sel
            .usel
            .active_node()
            .and_then(|id| self.scene.scene_graph.get(id))
            .map(|n| {
                let p = n.local_position;
                (p.x, p.y, p.z)
            })
            .unwrap_or((0.0, 0.0, 0.0));

        // Approximate bar size; paint via Area so it can receive hover later.
        let bar_w = 340.0_f32.min(scene.width() - 24.0).max(200.0);
        let bar_h = 36.0;
        let bar_pos = egui::pos2(
            scene.center().x - bar_w * 0.5,
            scene.bottom() - 16.0 - bar_h,
        );
        // Keep clear of the library hsplit (scene already excludes library).
        if bar_pos.y < scene.top() + 40.0 {
            return;
        }
        egui::Area::new(egui::Id::new("v2_tool_options"))
            .order(egui::Order::Foreground)
            .fixed_pos(bar_pos)
            .movable(false)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(ui_theme::SURFACE_CARD)
                    .rounding(ui_theme::RADIUS_CARD)
                    .stroke(egui::Stroke::new(1.0, ui_theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(bar_w - 28.0);
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            // .to-title
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  {tool_name}",
                                    ph::ARROWS_OUT_CARDINAL
                                ))
                                .size(12.0)
                                .strong()
                                .color(ui_theme::ACCENT_LIGHT),
                            );
                            // .to-grp XYZ
                            for (k, v) in [("X", px), ("Y", py), ("Z", pz)] {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    ui.label(
                                        egui::RichText::new(k)
                                            .size(11.0)
                                            .color(ui_theme::TEXT_LABEL),
                                    );
                                    // .to-in look
                                    egui::Frame::none()
                                        .fill(ui_theme::SURFACE_FIELD)
                                        .rounding(5.0)
                                        .stroke(egui::Stroke::new(1.0, ui_theme::SEAM))
                                        .inner_margin(egui::Margin::symmetric(6, 3))
                                        .show(ui, |ui| {
                                            ui.set_min_width(46.0);
                                            ui.label(
                                                egui::RichText::new(format!("{v:.2}"))
                                                    .size(11.0)
                                                    .family(egui::FontFamily::Monospace)
                                                    .color(ui_theme::TEXT),
                                            );
                                        });
                                });
                            }
                        });
                    });
            });
    }

    /// Gate 8: viewport split chrome — corner drag to split, seam resize, join.
    ///
    /// MUST use Foreground Areas for corner/seam hits: the central panel registers a
    /// full-rect `Sense::click_and_drag` first, which steals every later `ui.interact`
    /// on the same layer. Also: `drag_delta()` on `drag_stopped` is last-frame only
    /// (~0) — we track start pos ourselves.
    /// Begin a dual-pane split: clone View A into View B camera and mark dirty.
    pub(crate) fn ui2_begin_split(&mut self, vertical_seam: bool) {
        self.ui2.vp_split = Some(VpSplit {
            vertical_seam,
            ratio: 0.5,
        });
        // Keep current camera as the active pane (A). B starts as a copy so both
        // match until the user orbits one of them.
        self.ui2.camera_b = Some(self.viewport.camera);
        self.ui2.split_active_pane = 0;
        self.ui2.split_drag = None;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Join split views. Keeps the active pane's camera as the single view.
    pub(crate) fn ui2_join_split(&mut self) {
        // If B was active, `viewport.camera` already holds B — leave it.
        // If A was active, camera is A. Either way the live camera stays.
        self.ui2.vp_split = None;
        self.ui2.camera_b = None;
        self.ui2.split_active_pane = 0;
        self.ui2.split_drag = None;
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Viewport joined");
    }

    /// Copy the current display texture into the View-A split cache (egui texture).
    pub(crate) fn capture_split_pane_a(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let Some(src) = gpu.texture.as_ref() else { return };
        let (w, h) = gpu.texture_size;
        if w < 2 || h < 2 {
            return;
        }
        let need_new = gpu.split_pane_texture.is_none() || gpu.split_pane_size != (w, h);
        if need_new {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("split_pane_a"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            {
                let mut r = gpu.render_state.renderer.write();
                if let Some(old) = gpu.split_pane_texture_id.take() {
                    r.free_texture(&old);
                }
                gpu.split_pane_texture_id =
                    Some(r.register_native_texture(&gpu.device, &view, wgpu::FilterMode::Linear));
            }
            gpu.split_pane_texture = Some(tex);
            gpu.split_pane_size = (w, h);
        }
        if let (Some(src), Some(dst)) = (gpu.texture.as_ref(), gpu.split_pane_texture.as_ref()) {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("split_pane_a_copy"),
                });
            enc.copy_texture_to_texture(
                src.as_image_copy(),
                dst.as_image_copy(),
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit(Some(enc.finish()));
        }
    }

    pub(crate) fn ui2_viewport_split_chrome(&mut self, ctx: &egui::Context, full: egui::Rect) {
        // `full` is already the SCENE rect (above the library). Inset corners so they
        // sit inside the 3D view only — never on the library (orange dots were painting
        // over the library when corners used the hole including the dock-bottom band).
        let inset = 18.0_f32;
        let usable = full.shrink(inset);

        // ── Split active: seam + join + labels ────────────────────────────────
        if let Some(split) = self.ui2.vp_split {
            let (a, sec) = split.pane_rects(full);
            let active = self.ui2.split_active_pane;

            // Labels + active border only (each pane has its own GPU image now).
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("v2_vp_split_paint"));
            let painter = ctx.layer_painter(layer);
            let stroke_a = if active == 0 {
                egui::Stroke::new(1.5, ui_theme::ACCENT)
            } else {
                egui::Stroke::new(1.0, ui_theme::BORDER_SUBTLE)
            };
            let stroke_b = if active == 1 {
                egui::Stroke::new(1.5, ui_theme::ACCENT)
            } else {
                egui::Stroke::new(1.0, ui_theme::BORDER_SUBTLE)
            };
            painter.rect_stroke(a.shrink(0.5), 0.0, stroke_a, egui::StrokeKind::Middle);
            painter.rect_stroke(sec.shrink(0.5), 0.0, stroke_b, egui::StrokeKind::Middle);
            painter.text(
                a.left_top() + egui::vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                if active == 0 { "View A ●" } else { "View A" },
                egui::FontId::proportional(11.0),
                if active == 0 { ui_theme::ACCENT } else { ui_theme::TEXT_FAINT },
            );
            painter.text(
                sec.left_top() + egui::vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                if active == 1 { "View B ●" } else { "View B" },
                egui::FontId::proportional(11.0),
                if active == 1 { ui_theme::ACCENT } else { ui_theme::TEXT_DIM },
            );

            // Join (×) — Foreground Area so viewport drag cannot steal it.
            let join_pos = egui::pos2(sec.right() - 28.0, sec.top() + 6.0);
            egui::Area::new(egui::Id::new("v2_vp_join"))
                .order(egui::Order::Foreground)
                .fixed_pos(join_pos)
                .movable(false)
                .show(ctx, |ui| {
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                    ui.painter().rect_filled(
                        r, 4.0,
                        if resp.hovered() { ui_theme::SURFACE_CONTROL } else { ui_theme::SURFACE_CARD },
                    );
                    ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, ui_theme::BORDER), egui::StrokeKind::Middle);
                    ui.painter().text(
                        r.center(), egui::Align2::CENTER_CENTER, ph::X,
                        egui::FontId::proportional(12.0), ui_theme::TEXT,
                    );
                    if resp.hovered() {
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.ui2_join_split();
                    }
                });

            // Seam resize — Foreground Area strip.
            let seam_rect = if split.vertical_seam {
                egui::Rect::from_min_max(
                    egui::pos2(a.right() - 4.0, full.top()),
                    egui::pos2(a.right() + 4.0, full.bottom()),
                )
            } else {
                egui::Rect::from_min_max(
                    egui::pos2(full.left(), a.bottom() - 4.0),
                    egui::pos2(full.right(), a.bottom() + 4.0),
                )
            };
            egui::Area::new(egui::Id::new("v2_vp_seam"))
                .order(egui::Order::Foreground)
                .fixed_pos(seam_rect.min)
                .movable(false)
                .show(ctx, |ui| {
                    let (r, resp) = ui.allocate_exact_size(seam_rect.size(), egui::Sense::drag());
                    let hot = resp.hovered() || resp.dragged();
                    let pad = if split.vertical_seam {
                        egui::vec2(1.5, 0.0)
                    } else {
                        egui::vec2(0.0, 1.5)
                    };
                    ui.painter().rect_filled(
                        r.shrink2(pad),
                        0.0,
                        if hot { ui_theme::ACCENT } else { ui_theme::SEAM },
                    );
                    if hot {
                        ui.output_mut(|o| {
                            o.cursor_icon = if split.vertical_seam {
                                egui::CursorIcon::ResizeHorizontal
                            } else {
                                egui::CursorIcon::ResizeVertical
                            };
                        });
                    }
                    if resp.dragged() {
                        if let Some(s) = self.ui2.vp_split.as_mut() {
                            if s.vertical_seam {
                                s.ratio = ((a.right() + resp.drag_delta().x - full.left()) / full.width().max(1.0))
                                    .clamp(0.15, 0.85);
                            } else {
                                s.ratio = ((a.bottom() + resp.drag_delta().y - full.top()) / full.height().max(1.0))
                                    .clamp(0.15, 0.85);
                            }
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
            return;
        }

        // ── Single view: corner split handles ───────────────────────────────
        // Owner: dots only when the mouse is over them — never always-on.
        // Invisible hit target at rest; orange + ring only on hover/drag.
        let corners = [
            usable.left_top(),
            usable.right_top(),
            usable.left_bottom(),
            usable.right_bottom(),
        ];
        const HIT: f32 = 22.0;
        for (i, c) in corners.iter().enumerate() {
            let hit_min = egui::pos2(c.x - HIT * 0.5, c.y - HIT * 0.5);
            egui::Area::new(egui::Id::new(("v2_vp_corner", i)))
                .order(egui::Order::Foreground)
                .fixed_pos(hit_min)
                .movable(false)
                .show(ctx, |ui| {
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(HIT, HIT), egui::Sense::click_and_drag());
                    let hot = resp.hovered() || resp.dragged()
                        || self.ui2.split_drag.map(|(ci, _)| ci == i).unwrap_or(false);
                    // Fade in with hover (mockup "nothing before you point at it").
                    let t = ui.ctx().animate_bool_responsive(
                        egui::Id::new(("v2_vp_corner_vis", i)),
                        hot,
                    );
                    if t > 0.01 {
                        let rad = 3.5 + 1.5 * t;
                        let a = (220.0 * t) as u8;
                        ui.painter().circle_filled(
                            r.center(),
                            rad,
                            egui::Color32::from_rgba_unmultiplied(0xff, 0x8a, 0x3d, a),
                        );
                        if t > 0.5 {
                            ui.painter().circle_stroke(
                                r.center(),
                                rad + 3.0,
                                egui::Stroke::new(1.0, ui_theme::accent_tint((120.0 * t) as u8)),
                            );
                        }
                    }
                    if hot {
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Crosshair);
                    }
                    if resp.drag_started() {
                        if let Some(p) = resp.interact_pointer_pos() {
                            self.ui2.split_drag = Some((i, p));
                        }
                    }
                    if resp.dragged() {
                        if let Some(p) = ui.input(|inp| inp.pointer.hover_pos()).or(resp.interact_pointer_pos()) {
                            if let Some((ci, start)) = self.ui2.split_drag {
                                if ci == i {
                                    // Live guide line while dragging.
                                    let d = p - start;
                                    let layer = egui::LayerId::new(
                                        egui::Order::Foreground, egui::Id::new("v2_vp_split_guide"));
                                    let painter = ui.ctx().layer_painter(layer);
                                    if d.length() > 12.0 {
                                        if d.x.abs() >= d.y.abs() {
                                            let x = full.center().x;
                                            painter.line_segment(
                                                [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
                                                egui::Stroke::new(1.5, ui_theme::ACCENT),
                                            );
                                        } else {
                                            let y = full.center().y;
                                            painter.line_segment(
                                                [egui::pos2(full.left(), y), egui::pos2(full.right(), y)],
                                                egui::Stroke::new(1.5, ui_theme::ACCENT),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if resp.drag_stopped() {
                        if let Some((ci, start)) = self.ui2.split_drag.take() {
                            if ci == i {
                                let end = resp.interact_pointer_pos()
                                    .or_else(|| ui.input(|inp| inp.pointer.latest_pos()))
                                    .unwrap_or(start);
                                let d = end - start;
                                // Need a real drag — not a click. Dominant axis chooses split.
                                if d.length() >= 28.0 {
                                    let vertical_seam = d.x.abs() >= d.y.abs();
                                    self.ui2_begin_split(vertical_seam);
                                    self.toast(if vertical_seam {
                                        "Split L|R — drag seam · orbit each pane · × to join"
                                    } else {
                                        "Split T/B — drag seam · orbit each pane · × to join"
                                    });
                                }
                            }
                        }
                    }
                    // Double-click corner = instant 50/50 horizontal split (discoverability).
                    if resp.double_clicked() {
                        self.ui2_begin_split(true);
                        self.toast("Split L|R — drag seam · orbit each pane · × to join");
                    }
                });
        }
    }
}
