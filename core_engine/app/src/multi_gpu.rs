// core_engine/app/src/multi_gpu.rs
//! Multi-GPU worker settings UI + enumeration. Extracted from main.rs
//! (de-god: multi_gpu). Pure relocation, no logic change.
use eframe::egui;
use crate::{FourthLayerApp, WorkerGpu, gpu_worker, gpu_jobs};

impl FourthLayerApp {
    /// Task 14.2: Returns an iterator over enabled worker GPUs.
    /// This will be the single entry point for the job dispatcher in Task 14.3.
    #[allow(dead_code)]
    pub(crate) fn active_worker_gpus(&self) -> impl Iterator<Item = &WorkerGpu> {
        self.worker_gpus.iter().filter(|w| w.enabled)
    }

    /// Task 14.2: UI for multi-GPU settings.
    /// NOTE: This should be called from `properties_render.rs` inside the main
    /// render properties collapsing header.
    pub(crate) fn draw_multi_gpu_settings(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let mut toast_msg: Option<String> = None;
        let mut needs_rebuild = false;
        let mut re_enumerate = false;
        egui::CollapsingHeader::new(egui::RichText::new("\u{1F680} Multi-GPU").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
            .default_open(true)
            .show(ui, |ui| {
                if self.worker_gpus.is_empty() {
                    ui.weak("No worker GPUs detected.");
                } else {
                    ui.label("Enabled GPUs for background compute:");
                    ui.add_space(4.0);
                    // Get per-worker stats snapshot (if dispatcher exists)
                    let stats: Vec<_> = self.job_dispatcher.as_ref()
                        .map(|d| d.worker_stats().iter().map(|s| {
                            (s.worker_idx, s.jobs_completed, s.jobs_failed, s.last_duration_ms, s.healthy)
                        }).collect())
                        .unwrap_or_default();

                    for (i, worker) in self.worker_gpus.iter_mut().enumerate() {
                        let (_, completed, failed, last_ms, healthy) = stats.get(i).copied()
                            .unwrap_or((i, 0, 0, 0.0, true));
                        ui.horizontal(|ui| {
                            if !healthy {
                                // Unhealthy: red text, disabled checkbox
                                let mut dummy = worker.enabled;
                                ui.add_enabled(false, egui::Checkbox::new(&mut dummy, ""));
                                let name = if worker.adapter_name.len() > 30 {
                                    &worker.adapter_name[..30]
                                } else {
                                    &worker.adapter_name
                                };
                                ui.colored_label(egui::Color32::from_rgb(220, 80, 80),
                                    format!("{} (device lost)", name));
                            } else {
                                if ui.checkbox(&mut worker.enabled, "").changed() {
                                    toast_msg = Some(format!("Worker '{}' {}",
                                        worker.adapter_name,
                                        if worker.enabled { "enabled" } else { "disabled" }));
                                    needs_rebuild = true;
                                }
                                let name = if worker.adapter_name.len() > 30 {
                                    &worker.adapter_name[..30]
                                } else {
                                    &worker.adapter_name
                                };
                                let label_text = if completed > 0 || failed > 0 {
                                    format!("{} — {} ok / {} err, last {:.1}ms",
                                        name, completed, failed, last_ms)
                                } else {
                                    format!("{} ({})", name, worker.device_type)
                                };
                                let resp = ui.label(label_text);
                                // Full name tooltip for truncated names
                                if worker.adapter_name.len() > 30 {
                                    resp.on_hover_text(&worker.adapter_name);
                                }
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                if ui.button("Re-enumerate GPUs").clicked() {
                    re_enumerate = true;
                }
            });
        if let Some(msg) = toast_msg { self.toast(&msg); }
        if needs_rebuild {
            if let Some(ref mut d) = self.job_dispatcher {
                d.rebuild_enabled_workers(&self.worker_gpus);
            }
        }
        if re_enumerate {
            // 1. Drop existing dispatcher (triggers worker thread shutdown)
            self.job_dispatcher = None;
            // 2. Clear ALL in-flight trackers (deadlock prevention)
            self.cache.body_vol_cache.clear_all_in_flight();
            // 3. Re-enumerate
            self.worker_gpus = gpu_worker::enumerate_and_open_workers(&self.gpu_info.backend);
            // 4. Restore persisted enabled state (name matching for persistence only)
            for w in &mut self.worker_gpus {
                if let Some((_, enabled)) = self.render.worker_gpu_enabled.iter()
                    .find(|(name, _)| *name == w.adapter_name)
                {
                    w.enabled = *enabled;
                }
            }
            // 5. Rebuild dispatcher if workers exist
            if !self.worker_gpus.is_empty() {
                self.job_dispatcher = Some(gpu_jobs::GpuJobDispatcher::new(&self.worker_gpus));
            }
            self.toast(&format!("Re-enumerated: {} worker GPU(s)", self.worker_gpus.len()));
        }
    }
}
