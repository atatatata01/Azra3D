// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
use eframe::egui;
use crate::*;
use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_status_bar(&mut self, ctx: &egui::Context) {
        // Mockup `.status`: left = live tool/app shortcut hints · right = scene stats.
        // Hints always track tool / gizmo / modeling mode via `status_line_hint()`.
        let frame = egui::Frame::none()
            .fill(ui_theme::SURFACE_APPBAR)
            .inner_margin(egui::Margin::symmetric(12, 0));
        // O(1) — was `all_nodes().count()` every frame (iterator walk over the graph).
        let n = self.scene.scene_graph.node_count();
        let prims = self.scene.primitives.len();
        // Always-on sample: `perf.record_frame` runs every update(). Prefer median of
        // the history so the displayed integer is stable (raw last-frame thrash looked
        // like UI flicker even with fixed width).
        let fps = {
            let avg = self.perf.frame_percentile(0.5); // median ms over non-zero history
            let ms = if avg > 0.01 {
                avg
            } else {
                self.perf.frame_time_ms
            };
            if ms > 0.01 {
                1000.0 / ms
            } else {
                0.0
            }
        };
        let backend = {
            let b = self.gpu_info.backend.as_str();
            // wgpu Debug format is typically "Vulkan" / "Dx12" / "Metal" / "Gl"
            if b.is_empty() || b == "Empty" {
                "GPU"
            } else if b.eq_ignore_ascii_case("dx12") || b.contains("Dx12") {
                "DX12"
            } else if b.eq_ignore_ascii_case("vulkan") || b.contains("Vulkan") {
                "Vulkan"
            } else if b.eq_ignore_ascii_case("metal") || b.contains("Metal") {
                "Metal"
            } else if b.eq_ignore_ascii_case("gl") || b.contains("Gl") {
                "GL"
            } else {
                b
            }
        };
        let sel_name = self
            .sel
            .usel
            .active_node()
            .and_then(|id| {
                self.scene
                    .scene_graph
                    .get(id)
                    .map(|n| n.payload.display_name().to_string())
            })
            .or_else(|| {
                self.active_prim_index_unified()
                    .and_then(|i| self.scene.primitives.get(i).map(|p| p.name.clone()))
            })
            .unwrap_or_else(|| "Scene".into());

        // Parse "KEY: action | KEY: action" into chips (truncate to fit bar).
        let hint = self.status_line_hint();
        let chips: Vec<(String, String)> = hint
            .split(" | ")
            .filter_map(|part| {
                let part = part.trim();
                if part.is_empty() {
                    return None;
                }
                if let Some((k, a)) = part.split_once(':') {
                    Some((k.trim().to_string(), a.trim().to_string()))
                } else {
                    // State badge without a key (e.g. "Snap: ON" already split, or free text)
                    Some((String::new(), part.to_string()))
                }
            })
            .collect();

        let gizmo_lbl = if self.tool.gizmo_armed || self.tool.current.is_gizmo() {
            match self.tool.gizmo_mode {
                GizmoMode::Translate => Some("Move"),
                GizmoMode::Rotate => Some("Rotate"),
                GizmoMode::Scale => Some("Scale"),
                GizmoMode::Elongate => Some("Elongate"),
            }
        } else {
            None
        };
        let tool_lbl = if !self.ui2.active_tool.is_empty() {
            Some(self.ui2.active_tool.as_str())
        } else {
            None
        };

        egui::TopBottomPanel::bottom("v2_status")
            .exact_height(ui_theme::H_STATUS)
            .frame(frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                ui.painter().hline(
                    bar.x_range(),
                    bar.top() + 0.5,
                    egui::Stroke::new(1.0, ui_theme::SEAM),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;

                    // Left: context shortcut chips (always inform about tool / state).
                    let max_left = (bar.width() * 0.62).max(200.0);
                    let mut used = 0.0_f32;
                    for (kbd, action) in &chips {
                        let chip_w = if kbd.is_empty() {
                            action.len() as f32 * 6.2 + 8.0
                        } else {
                            kbd.len() as f32 * 6.5 + 14.0 + action.len() as f32 * 6.0 + 12.0
                        };
                        if used + chip_w > max_left {
                            ui.label(
                                egui::RichText::new("…")
                                    .size(11.0)
                                    .color(ui_theme::TEXT_FAINT),
                            );
                            break;
                        }
                        used += chip_w + 10.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            if !kbd.is_empty() {
                                status_kbd_chip(ui, kbd);
                            }
                            ui.label(
                                egui::RichText::new(action)
                                    .size(11.0)
                                    .color(ui_theme::TEXT_DIM),
                            );
                        });
                    }

                    // Right: selection · tool state · counts · engine
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        // Fixed-width slots: changing digit counts (99→100, 60→505) must not
                        // reflow the bar (RTL layout shifts every chip when the FPS string grows).
                        status_fixed_stat(
                            ui,
                            &format!("{backend} · {:3} fps", (fps.round() as i32).clamp(0, 999)),
                            118.0,
                            ui_theme::SDF,
                            true,
                        );
                        status_fixed_stat(
                            ui,
                            &format!("Objects {n:>4}"),
                            88.0,
                            ui_theme::TEXT_DIM,
                            false,
                        );
                        status_fixed_stat(
                            ui,
                            &format!("Prims {prims:>4}"),
                            78.0,
                            ui_theme::TEXT_DIM,
                            false,
                        );
                        if let Some(g) = gizmo_lbl {
                            ui.label(
                                egui::RichText::new(format!("Gizmo · {g}"))
                                    .size(11.0)
                                    .color(ui_theme::ACCENT_LIGHT),
                            );
                        } else if let Some(t) = tool_lbl {
                            ui.label(
                                egui::RichText::new(format!("Tool · {t}"))
                                    .size(11.0)
                                    .color(ui_theme::ACCENT_LIGHT),
                            );
                        }
                        ui.label(
                            egui::RichText::new(format!("Scene · {sel_name}"))
                                .size(11.0)
                                .color(ui_theme::TEXT_DIM),
                        );
                    });
                });
            });
    }
}

/// Fixed-width status metric (monospace) so live counters don't reflow the bar.
fn status_fixed_stat(ui: &mut egui::Ui, text: &str, width: f32, color: egui::Color32, _strong: bool) {
    let font = egui::FontId::monospace(11.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::hover());
    // Right-align inside the slot (RTL parent already places the slot on the right).
    ui.painter().text(
        egui::pos2(rect.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        text,
        font,
        color,
    );
}

/// Mockup `.status .kbd` chip.
fn status_kbd_chip(ui: &mut egui::Ui, kbd: &str) {
    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            kbd.to_string(),
            egui::FontId::monospace(10.0),
            ui_theme::TEXT_FAINT,
        )
    });
    let kw = (galley.size().x + 10.0).max(18.0);
    let (kr, _) = ui.allocate_exact_size(egui::vec2(kw, 16.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(kr, 4.0, ui_theme::SURFACE_WELL);
    ui.painter()
        .rect_stroke(kr, 4.0, egui::Stroke::new(1.0, ui_theme::SEAM), egui::StrokeKind::Middle);
    ui.painter().text(
        kr.center(),
        egui::Align2::CENTER_CENTER,
        kbd,
        egui::FontId::monospace(10.0),
        ui_theme::TEXT_FAINT,
    );
}
