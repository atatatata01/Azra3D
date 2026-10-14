//! SDF transform properties — extracted from properties_sdf.rs.

use eframe::egui;
use core_math::scene_graph::BodyTransform;
use crate::*;
use crate::properties_sdf::PropsContext;

/// When true, prim.position in the flat cache is body-local (relative to body.origin).
/// The Location sliders display world-space values and convert back to local on edit.
use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    pub(crate) fn draw_props_sdf_transform(&mut self, ui: &mut egui::Ui, pctx: &mut PropsContext, idx: usize) {
        // ---- Transform ----
        egui::CollapsingHeader::new(egui::RichText::new("📐 Transform").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
            .default_open(false).show(ui, |ui| {
            ui.label("Location:");
            if BODY_LOCAL_PRIMS {
                // Resolve the body transform for this prim so sliders show world-space.
                let bt: BodyTransform = self.scene.prim_body_map.get(idx)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default();
                // Compute display position in world-space from the body-local flat cache.
                let mut display_pos = bt.local_to_world(self.scene.primitives[idx].position);
                let mut pos_changed = false;
                let mut pos_drag_started = false;
                let r=ui.add(egui::Slider::new(&mut display_pos.x,-1000.0..=1000.0).text("X"));
                if r.drag_started(){pos_drag_started=true;} if r.changed(){pos_changed=true;}
                let r=ui.add(egui::Slider::new(&mut display_pos.y,-1000.0..=1000.0).text("Y"));
                if r.drag_started(){pos_drag_started=true;} if r.changed(){pos_changed=true;}
                let r=ui.add(egui::Slider::new(&mut display_pos.z,-1000.0..=1000.0).text("Z"));
                if r.drag_started(){pos_drag_started=true;} if r.changed(){pos_changed=true;}
                if pos_changed {
                    // Convert the new world-space value back to body-local and write it.
                    self.scene.primitives[idx].position = bt.world_to_local(display_pos);
                    pctx.changed = true;
                    pctx.desc = "Move".into();
                }
                if pos_drag_started { pctx.drag_started = true; }
            } else {
                let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].position.x,-1000.0..=1000.0).text("X"));
                if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Move".into();}
                let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].position.y,-1000.0..=1000.0).text("Y"));
                if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Move".into();}
                let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].position.z,-1000.0..=1000.0).text("Z"));
                if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Move".into();}
            }
            ui.label("Rotation °:");
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].rotation_deg.x,-180.0..=180.0).text("X"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Rotate".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].rotation_deg.y,-180.0..=180.0).text("Y"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Rotate".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].rotation_deg.z,-180.0..=180.0).text("Z"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Rotate".into();}
            ui.label("Scale:");
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].scale.x,0.01..=5.0).text("X"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Scale".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].scale.y,0.01..=5.0).text("Y"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Scale".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].scale.z,0.01..=5.0).text("Z"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Scale".into();}
            ui.label("Elongation:");
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].elongation.x,0.0..=5.0).text("X"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Elongate".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].elongation.y,0.0..=5.0).text("Y"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Elongate".into();}
            let r=ui.add(egui::Slider::new(&mut self.scene.primitives[idx].elongation.z,0.0..=5.0).text("Z"));
            if r.drag_started(){pctx.drag_started=true;} if r.changed(){pctx.changed=true;pctx.desc="Elongate".into();}
        });
        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
    }
}
