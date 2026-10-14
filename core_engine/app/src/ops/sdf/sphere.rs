//! Op: create SDF Sphere at world position.
//! Callable from: tools palette, Geometry menu, Shift+A, future geometry node.

use glam::Vec3;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, at: Vec3) {
    crate::ops::create::add_at::run(app, crate::ops::geo::sphere(), at);
}
