//! Op: solidify / shell selected poly faces.

use crate::FourthLayerApp;

pub const DEFAULT_THICKNESS: f32 = 0.1;

pub fn run(app: &mut FourthLayerApp) {
    run_with_thickness(app, DEFAULT_THICKNESS);
}

pub fn run_with_thickness(app: &mut FourthLayerApp, thickness: f32) {
    app.poly_solidify(thickness);
}
