//! Op: instant-create default-sized shape at world origin (Ctrl+click shape button).

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, shape_idx: usize) {
    app.instant_create_at_origin(shape_idx);
}
