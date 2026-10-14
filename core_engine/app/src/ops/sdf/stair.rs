//! Op: create SDF Stair at world position.

use glam::Vec3;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, at: Vec3) {
    crate::ops::create::add_at::run(app, crate::ops::geo::stair(), at);
}
