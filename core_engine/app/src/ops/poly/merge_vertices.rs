//! Op: merge selected vertices (center).

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp) {
    app.poly_merge_vertices();
}

pub fn by_distance(app: &mut FourthLayerApp) {
    app.poly_merge_by_distance();
}
