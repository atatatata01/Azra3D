//! Op: create SDF Spine / Pipe (3D polyline tube) at world position.
//!
//! Seed-sphere authoring: `add_primitive_with_op` (creation.rs) enters the exclusive
//! `SpinePointEdit` tool via `spine_enter_edit` — Ctrl+click adds points, S scales
//! per-point radius, B opens the corner pie, Space/Enter finishes to Object mode.

use glam::Vec3;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, at: Vec3) {
    crate::ops::create::add_at::run(app, core_math::primitives::spine_pipe_preset(), at);
}
