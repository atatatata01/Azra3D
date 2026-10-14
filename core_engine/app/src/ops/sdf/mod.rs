//! SDF create / draw ops — one file per shape/tool.

pub mod sphere;
pub mod box_shape;
pub mod cylinder;
pub mod torus;
pub mod cone;
pub mod capsule;
pub mod polyline;
pub mod draw_polyline;
pub mod stair;
pub mod spiral_stair;
pub mod spine;
pub mod plane_poly;
pub mod terrain_island;

/// Shared spawn OFFSET for menu/tool/node "Add" actions (Shift+A, Geometry menu,
/// tools palette). Owner rule: new objects spawn at the WORKPLANE CENTER when a
/// workplane is locked, else at the WORLD ORIGIN — never under the cursor.
///
/// This returns an OFFSET, not an absolute world position: `add_primitive_with_op`
/// (creation.rs) already anchors `pos` to the workplane origin (locked) or the body
/// origin (unlocked) before the body-local conversion. So "at the center" = zero
/// offset in BOTH modes; returning `wp_resolved.origin` here double-added the
/// workplane origin (prim landed at 2×origin — the "polyline not at center" bug).
/// (Same convention as voxel create in `input_sculpt.rs`.)
#[inline]
pub fn add_spawn_point(_app: &crate::FourthLayerApp) -> glam::Vec3 {
    glam::Vec3::ZERO
}
