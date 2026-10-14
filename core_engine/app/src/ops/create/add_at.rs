//! Op: add SDF prim at world position with current create boolean op.
//!
//! Pure-ish AppOp: shape + position → scene. Used by instant menu tools and future nodes.

use glam::Vec3;
use core_math::primitives::SdfShape;
use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp, shape: SdfShape, at: Vec3) -> usize {
    app.add_primitive_immediate(shape, at)
}

/// Add using geo preset index (0..=7).
pub fn run_preset(app: &mut FourthLayerApp, shape_idx: usize, at: Vec3) -> usize {
    let shape = crate::ops::geo::shape_presets::by_index(shape_idx);
    run(app, shape, at)
}
