//! Viewport/render/compositor properties dispatcher — extracted from properties.rs.

use eframe::egui;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_props_viewport(&mut self, ui: &mut egui::Ui) {
        // ---- Viewport / Render / Light (always visible, outside scroll) ----
        ui.add_space(4.0); ui.separator();
        egui::ScrollArea::vertical().id_salt("viewport_scroll").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_gray(140),"Viewport");
            ui.separator();
            self.draw_props_render(ui);
            self.draw_props_profiler(ui);
            ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
            self.draw_props_compositor(ui);
            // Undo/Redo buttons
            let undo_avail=!self.history.undo_stack.is_empty();
            let redo_avail=!self.history.redo_stack.is_empty();
            let undo_len=self.history.undo_stack.len();
            let redo_len=self.history.redo_stack.len();
            ui.horizontal(|ui|{
                if ui.add_enabled(undo_avail,egui::Button::new("\u{21a9} Undo")).clicked(){self.undo();}
                if ui.add_enabled(redo_avail,egui::Button::new("\u{21aa} Redo")).clicked(){self.redo();}
                ui.weak(format!("{}/{}",undo_len,redo_len));
            });
        });
    }
}
