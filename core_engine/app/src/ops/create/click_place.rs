//! Op: click-create default-sized shape at ray hit (no footprint drag).

use eframe::egui;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, mouse: egui::Pos2, rect: egui::Rect) {
    app.click_create_at_point(mouse, rect);
}
