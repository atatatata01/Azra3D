//! Profiler sub-panel — extracted from properties_viewport.rs.

use eframe::egui;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_props_profiler(&mut self, ui: &mut egui::Ui) {
            egui::CollapsingHeader::new(egui::RichText::new("📊 Profiler").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
                .default_open(false).show(ui,|ui|{
                ui.checkbox(&mut self.ui.show_profiler_overlay, "Viewport Overlay");
                // Frame time graph (sparkline)
                let n = self.ui.prof_frame_times.len();
                if n > 1 {
                    let max_t = self.ui.prof_frame_times.iter().copied().fold(1.0f32, f32::max);
                    let avg_t = self.ui.prof_frame_times.iter().sum::<f32>() / n as f32;
                    let fps = if avg_t > 0.001 { 1000.0 / avg_t } else { 0.0 };
                    ui.label(format!("{:.1} FPS | {:.2}ms avg", fps, avg_t));
                    // Mini sparkline
                    let (resp, painter) = ui.allocate_painter(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
                    let r = resp.rect;
                    painter.rect_filled(r, 2.0, egui::Color32::from_gray(20));
                    let step = r.width() / (n.max(2) - 1) as f32;
                    let points: Vec<egui::Pos2> = self.ui.prof_frame_times.iter().enumerate()
                        .map(|(i,&t)| egui::pos2(r.left() + i as f32 * step, r.bottom() - (t / max_t) * r.height()))
                        .collect();
                    if points.len() >= 2 {
                        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 180, 255))));
                    }
                    // 16.67ms target line
                    let y16 = r.bottom() - (16.67 / max_t) * r.height();
                    if y16 > r.top() && y16 < r.bottom() {
                        painter.line_segment([egui::pos2(r.left(), y16), egui::pos2(r.right(), y16)],
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 200, 80, 100)));
                    }
                }
                // Per-pass breakdown
                ui.add_space(4.0);
                ui.label("Pass Breakdown:");
                let total: f32 = self.ui.prof_pass_times.iter().sum();
                for i in 0..7 {
                    let t = self.ui.prof_pass_times[i];
                    if t > 0.001 || i < 2 { // always show raymarch + deferred
                        let pct = if total > 0.001 { t / total } else { 0.0 };
                        ui.horizontal(|ui|{
                            let color = match i { 0=>egui::Color32::from_rgb(255,100,100), 1=>egui::Color32::from_rgb(100,200,255),
                                2=>egui::Color32::from_rgb(200,150,255), 3=>egui::Color32::from_rgb(150,200,150),
                                4=>egui::Color32::from_rgb(255,200,100), 5=>egui::Color32::from_rgb(200,200,200),
                                _=>egui::Color32::from_rgb(100,255,200) };
                            let (bar_r, _) = ui.allocate_exact_size(egui::vec2(80.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(bar_r, 2.0, egui::Color32::from_gray(30));
                            let fill = egui::Rect::from_min_size(bar_r.min, egui::vec2(bar_r.width() * pct, bar_r.height()));
                            ui.painter().rect_filled(fill, 2.0, color);
                            ui.label(format!("{}: {:.2}ms", self.ui.prof_pass_names[i], t));
                        });
                    }
                }
                // GPU timestamp pass breakdown (real GPU-side timing)
                if self.ui.prof_gpu_timestamps_available {
                    ui.add_space(4.0);
                    ui.label("GPU Pass Timing (timestamp queries):");
                    let gpu_names = ["Raymarch", "Deferred", "Bevel", "SSR", "Fog",
                                     "Bloom Ext", "Bloom H", "Bloom V", "Tonemap", "Upsample",
                                     "FXAA", "Shadow", "Raster GBuf", "SSGI", "SSAO", "RT Shadow"];
                    // 16 slots, not 15: the table stopped at 15 and so silently
                    // excluded PASS_RT_SHADOW (slot 15) from both the list and the
                    // GPU total, while viewport_overlays.rs did show it. Slot 13 is
                    // now SSGI (it was the dead PASS_BAKE).
                    // Slots 16/17 are CSM cascades 1/2 — folded into the "Shadow" row
                    // rather than listed, so the row means "all 3 cascades" and the
                    // total is not silently missing two full-scene depth passes.
                    let casc12 = self.ui.prof_gpu_pass_times[16] + self.ui.prof_gpu_pass_times[17];
                    let pass_ms = |i: usize| -> f32 {
                        self.ui.prof_gpu_pass_times[i]
                            + if i == crate::gpu_timestamps::PASS_SHADOW as usize { casc12 } else { 0.0 }
                    };
                    let gpu_total: f32 = (0..16).map(pass_ms).sum();
                    for i in 0..16 {
                        let t = pass_ms(i);
                        if t > 0.001 {
                            let pct = if gpu_total > 0.001 { t / gpu_total } else { 0.0 };
                            ui.horizontal(|ui|{
                                let color = egui::Color32::from_rgb(100, 220, 160);
                                let (bar_r, _) = ui.allocate_exact_size(egui::vec2(80.0, 12.0), egui::Sense::hover());
                                ui.painter().rect_filled(bar_r, 2.0, egui::Color32::from_gray(30));
                                let fill = egui::Rect::from_min_size(bar_r.min, egui::vec2(bar_r.width() * pct, bar_r.height()));
                                ui.painter().rect_filled(fill, 2.0, color);
                                ui.label(format!("{}: {:.3}ms", gpu_names[i], t));
                            });
                        }
                    }
                    ui.label(format!("GPU total: {:.3}ms", gpu_total));
                }
                // GPU memory breakdown
                ui.add_space(4.0);
                ui.label(format!("GPU Memory: {:.1} MB total", self.ui.prof_gpu_mem_total_mb));
                ui.label(format!("  Textures: {:.1} MB | Buffers: {:.2} MB | Shadows: {:.1} MB",
                    self.ui.prof_gpu_mem_textures_mb, self.ui.prof_gpu_mem_buffers_mb, self.ui.prof_gpu_mem_shadow_mb));
                ui.label(format!("Render: {}x{}", self.ui.prof_render_res.0, self.ui.prof_render_res.1));
            });
            ui.add_space(2.0); ui.separator(); ui.add_space(2.0);

    }
}
