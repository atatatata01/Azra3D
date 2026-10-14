//! Op: create a polygonal plane and place active node at world position.

use glam::Vec3;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, at: Vec3) {
    app.create_poly_mesh("plane");
    app.ui2_place_active_node(at);
}
