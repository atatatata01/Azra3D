//! Tool Parameters bake section.

use super::{BakeSettings, BakeSource, MeshMapKind};
use core_math::scene_graph::NodeId;

impl crate::AzraApp {
    fn mesh_maps_stale(&self) -> bool {
        let Some(body) = self.poly_curv_body() else {
            return false;
        };
        let key = self.poly_display_cache.get(&body).map(|e| e.key);
        self.ui.baked_images.for_body(body).any(|e| e.is_stale(key))
    }

    /// Texture slot holding `kind` for `body`, if the registry has an entry.
    ///
    /// Returns the slot even when pixels are missing (D4: never silent Live fallback).
    pub(crate) fn baked_mesh_map(
        &self,
        body: core_math::scene_graph::NodeId,
        kind: MeshMapKind,
    ) -> Option<u32> {
        self.ui
            .baked_images
            .by_body_kind(body, kind.bit())
            .map(|e| e.slot)
    }

    /// Generators / masks pick maps by name.
    pub(crate) fn baked_mesh_map_named(&self, name: &str) -> Option<u32> {
        self.ui.baked_images.by_name(name).map(|e| e.slot)
    }

    pub(crate) fn draw_tool_params_mesh_maps(&mut self, ui: &mut egui::Ui) {
        use crate::{ui_fields as f, ui_theme as t};
        let Some(body) = self.poly_curv_body() else {
            return;
        };
        ui.label(
            egui::RichText::new("Mesh Maps")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        let mut s = self.bake_settings_for(body);
        let mut bake = false;
        let mut cage_hot = false;
        f::section(ui, "live/MeshMaps/params", "Bake", None, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Size").size(10.0).color(t::TEXT_DIM));
                for r in [256u32, 512, 1024, 2048, 4096] {
                    if ui
                        .selectable_label(s.res_global == r, format!("{r}"))
                        .clicked()
                    {
                        s.res_global = r;
                    }
                }
            });
            for k in MeshMapKind::ALL {
                let mut on = s.enabled_maps & k.bit() != 0;
                if f::bool_row(ui, k.label(), &mut on)
                    .on_hover_text(k.hint())
                    .changed()
                {
                    if on {
                        s.enabled_maps |= k.bit();
                    } else {
                        s.enabled_maps &= !k.bit();
                    }
                }
            }
            let ray_maps = MeshMapKind::ALL
                .iter()
                .any(|k| k.needs_rays() && s.enabled_maps & k.bit() != 0);
            if ray_maps {
                let mut rays = s.ao_rays as i32;
                f::int(ui, "Rays", &mut rays, 8..=512)
                    .on_hover_text("Hemisphere samples per texel for AO, Thickness, and Bent Normal.");
                s.ao_rays = rays.clamp(8, 512) as u32;
            }
            f::num(ui, "Curv Radius", &mut s.curv_radius, 0.005, 0.0..=1.0, None)
                .on_hover_text("Widens the curvature band, in world units. 0 = sharpest.");

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Source")
                    .size(10.0)
                    .color(t::TEXT_DIM),
            );
            let is_high = matches!(s.source, BakeSource::HighPoly { .. });
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(!is_high, "Self")
                    .on_hover_text(
                        "Bake this body's modifier-stack display mesh \
                         (Subdivide, Bevel, Solidify, … already in).",
                    )
                    .clicked()
                {
                    s.source = BakeSource::SelfMesh;
                }
                if ui
                    .selectable_label(is_high, "High-poly")
                    .on_hover_text("Project from other scene bodies (high→low).")
                    .clicked()
                {
                    if let BakeSource::HighPoly { .. } = s.source {
                    } else {
                        s.source = BakeSource::HighPoly {
                            nids: Vec::new(),
                            match_by_name: false,
                        };
                    }
                }
            });
            if let BakeSource::HighPoly {
                nids,
                match_by_name,
            } = &mut s.source
            {
                let mut match_name = *match_by_name;
                if f::bool_row(ui, "Match by name", &mut match_name)
                    .on_hover_text("Only project onto bodies that share this stem (_low / _high).")
                    .changed()
                {
                    *match_by_name = match_name;
                }
                let others: Vec<(NodeId, String)> = self
                    .scene
                    .scene_graph
                    .poly_bodies()
                    .filter(|(nid, _)| *nid != body)
                    .map(|(nid, n)| (nid, n.payload.display_name().to_string()))
                    .collect();
                for (nid, name) in others {
                    let mut on = nids.contains(&nid);
                    if f::bool_row(ui, &name, &mut on).changed() {
                        if on {
                            if !nids.contains(&nid) {
                                nids.push(nid);
                            }
                        } else {
                            nids.retain(|x| *x != nid);
                        }
                    }
                }
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Cage")
                    .size(10.0)
                    .color(t::TEXT_DIM),
            );
            let mut pin = self.ui.bake_cage_preview_pinned;
            if f::bool_row(ui, "Show cage", &mut pin)
                .on_hover_text(
                    "Keep the projection cage in the viewport. Also appears while you drag Max Front / Rear.",
                )
                .changed()
            {
                self.ui.bake_cage_preview_pinned = pin;
                if !pin {
                    self.clear_bake_cage_preview();
                }
            }
            let front_r = f::num(ui, "Max Front", &mut s.max_front, 0.001, 0.0..=10.0, None)
                .on_hover_text(
                    "Ray distance along the low-poly normal. Ignored on texels that hit a cage. \
                     Drag to see the envelope in the viewport.",
                );
            cage_hot |= front_r.dragged() || front_r.hovered() || front_r.has_focus() || front_r.changed();
            let rear_r = f::num(ui, "Max Rear", &mut s.max_rear, 0.001, 0.0..=10.0, None)
                .on_hover_text(
                    "Ray distance against the low-poly normal. Drag to see the rear envelope.",
                );
            cage_hot |= rear_r.dragged() || rear_r.hovered() || rear_r.has_focus() || rear_r.changed();
            let cage_others: Vec<(NodeId, String)> = self
                .scene
                .scene_graph
                .poly_bodies()
                .filter(|(nid, _)| *nid != body)
                .map(|(nid, n)| (nid, n.payload.display_name().to_string()))
                .collect();
            if !cage_others.is_empty() {
                ui.label(
                    egui::RichText::new("Cage mesh")
                        .size(10.0)
                        .color(t::TEXT_DIM),
                );
                for (nid, name) in cage_others {
                    let mut on = s.cage_nids.contains(&nid);
                    let cage_r = f::bool_row(ui, &name, &mut on).on_hover_text(
                        "Inflated cage. Rays along the normal stop here instead of Max Front. \
                         Match-by-name also picks *_cage.",
                    );
                    cage_hot |= cage_r.hovered() || cage_r.changed();
                    if cage_r.changed() {
                        if on {
                            if !s.cage_nids.contains(&nid) {
                                s.cage_nids.push(nid);
                            }
                        } else {
                            s.cage_nids.retain(|x| *x != nid);
                        }
                    }
                }
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Quality")
                    .size(10.0)
                    .color(t::TEXT_DIM),
            );
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("SS").size(10.0).color(t::TEXT_DIM));
                for ss in [1u32, 2, 4] {
                    if ui
                        .selectable_label(s.supersample == ss, format!("{ss}×"))
                        .clicked()
                    {
                        s.supersample = ss;
                    }
                }
            });
            let mut pad = s.padding_px as i32;
            f::int(ui, "Padding px", &mut pad, 0..=64)
                .on_hover_text("Island padding in output texels. Stops mip-map seam bleed.");
            s.padding_px = pad.clamp(0, 64) as u32;
            let mut udim = s.udim;
            if f::bool_row(ui, "UDIM", &mut udim)
                .on_hover_text(
                    "Bake each occupied UV tile (1001, 1002, …) as its own image. Off = 0–1 wrap.",
                )
                .changed()
            {
                s.udim = udim;
            }
            let mut gpu_rays = s.gpu_rays;
            if f::bool_row(ui, "GPU rays", &mut gpu_rays)
                .on_hover_text(
                    "Trace AO, Thickness, and Bent Normal on the GPU. Falls back to CPU if no device.",
                )
                .changed()
            {
                s.gpu_rays = gpu_rays;
            }
            let mut bit16 = s.bit16;
            if f::bool_row(ui, "16-bit", &mut bit16)
                .on_hover_text(
                    "Dual-plane 16-bit for Position / Object-Normal / Tangent-Normal / Bent-Normal. Kills RGBA8 banding.",
                )
                .changed()
            {
                s.bit16 = bit16;
            }

            if self.ui.last_mesh_map_ms > 0.0 {
                ui.label(
                    egui::RichText::new(format!("last ray map: {:.0} ms", self.ui.last_mesh_map_ms))
                        .size(10.0)
                        .color(t::TEXT_DIM),
                );
            }
            let names: Vec<String> = self
                .ui
                .baked_images
                .for_body(body)
                .map(|e| e.name.clone())
                .collect();
            if names.is_empty() {
                ui.label(
                    egui::RichText::new("not baked yet")
                        .size(10.0)
                        .color(t::TEXT_DIM),
                );
            } else {
                ui.label(
                    egui::RichText::new(format!("baked: {}", names.join(", ")))
                        .size(10.0)
                        .color(t::TEXT_DIM),
                );
            }
            if self.mesh_maps_stale() {
                ui.label(
                    egui::RichText::new("STALE - the mesh changed since these were baked")
                        .size(10.0)
                        .color(t::WARNING),
                );
            }
            if ui.button("Bake Mesh Maps").clicked() {
                bake = true;
            }
        });
        if cage_hot || self.ui.bake_cage_preview_pinned {
            self.touch_bake_cage_preview(body, &s);
        }
        self.store_bake_settings(body, s);
        if bake {
            self.poly_request_mesh_map_bake();
        }
    }
}
