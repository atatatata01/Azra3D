//! Op: create primitive poly mesh bodies.
//!
//! Placement is **owned by** [`FourthLayerApp::create_poly_mesh`] (workplane → selection →
//! world origin). Do **not** call `ui2_place_active_node` after create: the spawn point
//! from the tools palette is often `Vec3::ZERO` and would stomp a workplane-correct pose.

use glam::Vec3;
use crate::FourthLayerApp;

pub fn box_mesh(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("box");
}

pub fn plane(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("plane");
}

pub fn sphere(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("sphere");
}

pub fn cylinder(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("cylinder");
}

pub fn cone(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("cone");
}

pub fn torus(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("torus");
}

pub fn circle(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("circle");
}

pub fn ico_sphere(app: &mut FourthLayerApp, _at: Vec3) {
    app.create_poly_mesh("icosphere");
}
