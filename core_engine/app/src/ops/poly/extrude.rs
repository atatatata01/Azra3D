//! Op: extrude selected poly faces (default offset for menus/tools).

use crate::FourthLayerApp;

/// Default offset used by menus / tool one-shot (matches prior menu hardcode).
pub const DEFAULT_OFFSET: f32 = 0.3;

pub fn run(app: &mut FourthLayerApp) {
    run_with_offset(app, DEFAULT_OFFSET);
}

pub fn run_with_offset(app: &mut FourthLayerApp, offset: f32) {
    app.poly_extrude_faces(offset);
}
