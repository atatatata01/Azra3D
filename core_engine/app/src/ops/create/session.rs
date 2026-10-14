//! Interactive footprint create session — façade over `create_tool` engine.
//!
//! Session state (CreateState, phases) lives in App/tool; this module is the
//! **only** external entry for UI/input so a future "Create Box (interactive)" node
//! can call the same API.

use eframe::egui;
use glam::Vec3;
use crate::{CreateState, FourthLayerApp};

/// Begin footprint drag (returns new CreateState if armed).
pub fn start(app: &mut FourthLayerApp, mouse: egui::Pos2, rect: egui::Rect) -> Option<CreateState> {
    app.start_create(mouse, rect)
}

pub fn update_footprint(
    app: &mut FourthLayerApp,
    mouse: egui::Pos2,
    rect: egui::Rect,
    shift: bool,
    alt: bool,
    snap_world: Option<Vec3>,
) {
    app.update_create_footprint(mouse, rect, shift, alt, snap_world);
}

pub fn finalize_footprint(app: &mut FourthLayerApp, center_mode: bool) {
    app.finalize_footprint(center_mode);
}

pub fn update_extrude(
    app: &mut FourthLayerApp,
    mouse: egui::Pos2,
    rect: egui::Rect,
    shift: bool,
    alt: bool,
) {
    app.update_create_extrude(mouse, rect, shift, alt);
}

pub fn commit(app: &mut FourthLayerApp, infinite: bool) {
    app.commit_create(infinite);
}

pub fn cancel(app: &mut FourthLayerApp) {
    app.cancel_create();
}

pub fn cycle_op(app: &mut FourthLayerApp) {
    app.cycle_create_op();
}

pub fn deactivate(app: &mut FourthLayerApp) {
    app.deactivate_tool();
}
