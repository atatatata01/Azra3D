//! Op: enter interactive polyline draw mode on the workplane.

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp) {
    app.start_polyline_draw();
}
