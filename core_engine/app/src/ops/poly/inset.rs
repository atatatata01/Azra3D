//! Op: inset selected poly faces.

use crate::FourthLayerApp;

pub const DEFAULT_AMOUNT: f32 = 0.1;

pub fn run(app: &mut FourthLayerApp) {
    run_with_amount(app, DEFAULT_AMOUNT);
}

pub fn run_with_amount(app: &mut FourthLayerApp, amount: f32) {
    app.poly_inset_faces(amount);
}
