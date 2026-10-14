//! `PanelKind::Image` — list / preview / re-bake / delete / export of the baked registry.

use super::registry::BakedImage;
use crate::ui_theme as t;
use core_math::scene_graph::NodeId;
use eframe::egui;
use egui_phosphor::regular as ph;

impl crate::AzraApp {
    pub(crate) fn ui2_draw_image_panel(&mut self, ui: &mut egui::Ui) {
        let mut pick: Option<String> = None;
        let mut rename: Option<(String, String)> = None;
        let mut rebake: Option<(NodeId, u32)> = None;
        let mut delete: Option<String> = None;
        let mut export: Option<String> = None;

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("BAKED IMAGES")
                    .size(10.0)
                    .color(t::TEXT_SECTION)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("{}", self.ui.baked_images.images.len()));
            });
        });
        ui.add_space(4.0);

        let selected = self.ui.image_panel_selected.clone();
        let h = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("v2_image_panel_scroll")
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if self.ui.baked_images.images.is_empty() {
                    ui.label(
                        egui::RichText::new("No baked images yet. Select a poly body and Bake Mesh Maps.")
                            .size(12.0)
                            .color(t::TEXT_FAINT),
                    );
                    return;
                }
                let rows: Vec<BakedImage> = self.ui.baked_images.images.clone();
                for img in &rows {
                    let is_sel = selected.as_deref() == Some(img.name.as_str());
                    let body_name = self
                        .scene
                        .scene_graph
                        .get(img.body)
                        .map(|n| n.payload.display_name().to_string())
                        .unwrap_or_else(|| format!("#{}", img.body.0));
                    let kind = img.kind().map(|k| k.label()).unwrap_or("?");
                    let key = self.poly_display_cache.get(&img.body).map(|e| e.key);
                    let stale = img.is_stale(key);
                    let label = format!(
                        "{}  {}  ·  {}  ·  {}²  ·  {}",
                        if stale { ph::WARNING } else { ph::IMAGE },
                        img.name,
                        kind,
                        img.res,
                        body_name
                    );
                    let resp = ui.selectable_label(is_sel, label);
                    if stale {
                        resp.clone().on_hover_text(if img.pixels_missing {
                            "Pixels missing — re-bake. Will not silently fall back to Live."
                        } else {
                            "STALE — the mesh changed since this was baked."
                        });
                    }
                    if resp.clicked() {
                        pick = Some(img.name.clone());
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                if let Some(name) = selected.as_deref() {
                    if let Some(img) = self.ui.baked_images.by_name(name).cloned() {
                        self.draw_image_detail(
                            ui,
                            &img,
                            &mut rename,
                            &mut rebake,
                            &mut delete,
                            &mut export,
                        );
                    }
                }
            });

        if let Some(name) = pick {
            self.ui.image_panel_rename = name.clone();
            self.ui.image_panel_selected = Some(name);
        }
        if let Some((old, new)) = rename {
            match self.ui.baked_images.rename(&old, &new) {
                Ok(()) => {
                    self.ui.image_panel_selected = Some(new);
                    self.toast("Renamed baked image");
                }
                Err(e) => self.toast(&format!("Rename: {e}")),
            }
        }
        if let Some((body, bits)) = rebake {
            self.poly_request_mesh_map_bake_for(body, Some(bits));
        }
        if let Some(name) = delete {
            if self.ui.baked_images.remove_name(&name).is_some() {
                if self.ui.image_panel_selected.as_deref() == Some(name.as_str()) {
                    self.ui.image_panel_selected = None;
                }
                self.toast(&format!("Deleted {name}"));
            }
        }
        if let Some(name) = export {
            self.queue_baked_image_export(&name);
        }
    }

    fn draw_image_detail(
        &mut self,
        ui: &mut egui::Ui,
        img: &BakedImage,
        rename: &mut Option<(String, String)>,
        rebake: &mut Option<(NodeId, u32)>,
        delete: &mut Option<String>,
        export: &mut Option<String>,
    ) {
        ui.label(
            egui::RichText::new("PREVIEW")
                .size(10.0)
                .color(t::TEXT_SECTION)
                .strong(),
        );
        ui.add_space(4.0);
        if img.pixels_missing {
            ui.colored_label(t::WARNING, "pixels missing — re-bake to restore");
        } else if let Some(tid) = self.map_slot_thumb(ui.ctx(), img.slot, false) {
            let side = 192.0_f32.min(ui.available_width());
            ui.image(egui::load::SizedTexture::new(tid, egui::vec2(side, side)));
        } else {
            ui.weak("no thumbnail (GPU not ready)");
        }
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Name").size(10.0).color(t::TEXT_DIM));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.ui.image_panel_rename)
                    .desired_width(ui.available_width()),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                *rename = Some((img.name.clone(), self.ui.image_panel_rename.clone()));
            }
        });
        if ui.small_button("Rename").clicked() {
            *rename = Some((img.name.clone(), self.ui.image_panel_rename.clone()));
        }

        let kind = img.kind().map(|k| k.label()).unwrap_or("?");
        let body_name = self
            .scene
            .scene_graph
            .get(img.body)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_else(|| format!("#{}", img.body.0));
        ui.label(
            egui::RichText::new(format!(
                "{kind}  ·  {}²  ·  {body_name}  ·  {}",
                img.res,
                age_label(img.baked_at_secs)
            ))
            .size(11.0)
            .color(t::TEXT_DIM),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Re-bake", ph::ARROWS_CLOCKWISE)).clicked() {
                *rebake = Some((img.body, img.kind_bit));
            }
            if ui.button(format!("{}  Export PNG", ph::DOWNLOAD_SIMPLE)).clicked() {
                *export = Some(img.name.clone());
            }
            if ui.button(format!("{}  Delete", ph::TRASH)).clicked() {
                *delete = Some(img.name.clone());
            }
        });
    }

    fn queue_baked_image_export(&mut self, name: &str) {
        let Some(img) = self.ui.baked_images.by_name(name).cloned() else {
            self.toast("Export: image not in registry");
            return;
        };
        if img.pixels_missing {
            self.toast("Export: pixels missing — re-bake first");
            return;
        }
        let Some(slot) = self.baked_mesh_map_named(name) else {
            self.toast("Export: image not in registry");
            return;
        };
        let Some(px) = self
            .gpu
            .as_ref()
            .and_then(|g| g.material_textures.slot_rgba(slot))
            .map(|s| s.to_vec())
        else {
            self.toast("Export: no pixels in that slot");
            return;
        };
        let Some(png) = crate::material_textures::encode_map_png(&px) else {
            self.toast("Export: could not encode PNG");
            return;
        };
        let suggested = format!("{}.png", img.name);
        self.cache.pending_baked_export = Some((suggested, png));
        self.toast("Export PNG: choose a file…");
    }

    pub(crate) fn poll_baked_image_export(&mut self) {
        let Some((suggested, png)) = self.cache.pending_baked_export.take() else {
            return;
        };
        std::thread::Builder::new()
            .name("azra-bake-export".into())
            .spawn(move || {
                let path = rfd::FileDialog::new()
                    .set_title("Export baked image")
                    .add_filter("PNG", &["png"])
                    .set_file_name(&suggested)
                    .save_file();
                if let Some(path) = path {
                    let _ = std::fs::write(path, png);
                }
            })
            .ok();
    }
}

fn age_label(baked_at: u64) -> String {
    if baked_at == 0 {
        return "unknown age".into();
    }
    let now = super::registry::now_secs();
    let dt = now.saturating_sub(baked_at);
    if dt < 60 {
        format!("{dt}s ago")
    } else if dt < 3600 {
        format!("{}m ago", dt / 60)
    } else if dt < 86400 {
        format!("{}h ago", dt / 3600)
    } else {
        format!("{}d ago", dt / 86400)
    }
}
