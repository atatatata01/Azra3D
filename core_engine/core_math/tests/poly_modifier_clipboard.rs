//! Copy/paste of a poly modifier ACROSS bodies.
//!
//! The load-bearing question is not "does the struct clone" (derive does that) but
//! "does authored LOCAL-space state survive a change of owner body". `rebased_to`
//! is the one place that policy lives; these tests pin it.

use core_math::scene_graph::{PolyModifier, PolyModifierType};
use glam::{Mat4, Vec3};

/// Body A sits at the origin, body B is translated — a Pipe copied A→B must keep the
/// same WORLD shape, i.e. its local path must shift by the inverse of B's offset.
#[test]
fn pipe_path_is_rebased_into_the_target_body_space() {
    let mut m = PolyModifier::new(PolyModifierType::Pipe);
    m.pipe_path = vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 2.0, 0.0)];
    m.pipe_radius = 0.25;

    let src_world = Mat4::IDENTITY;
    let dst_world = Mat4::from_translation(Vec3::new(5.0, 0.0, -3.0));
    let out = m.rebased_to(src_world, dst_world);

    // Same world positions after the target body's transform is applied again.
    for (i, p) in out.pipe_path.iter().enumerate() {
        let world = dst_world.transform_point3(*p);
        let orig_world = src_world.transform_point3(m.pipe_path[i]);
        assert!(
            (world - orig_world).length() < 1e-5,
            "pipe point {i} moved in world space: {world} vs {orig_world}"
        );
    }
    // Scalars are untouched.
    assert_eq!(out.pipe_radius, 0.25);
    assert_eq!(out.pipe_segments, m.pipe_segments);
}

/// Same-body paste (duplicate a modifier in place) must be a bit-identical copy.
#[test]
fn same_body_paste_is_identity() {
    let mut m = PolyModifier::new(PolyModifierType::Pipe);
    m.pipe_path = vec![Vec3::new(0.3, -1.0, 2.0), Vec3::new(4.0, 4.0, 4.0)];
    let w = Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0));
    let out = m.rebased_to(w, w);
    for (a, b) in out.pipe_path.iter().zip(m.pipe_path.iter()) {
        assert!((*a - *b).length() < 1e-5);
    }
}

/// `array_offset` is a DISPLACEMENT: rotation/scale apply, translation must not.
/// `circ_center` is a POINT: translation must apply.
#[test]
fn offsets_translate_as_vectors_centers_as_points() {
    let mut m = PolyModifier::new(PolyModifierType::Array);
    m.array_offset = Vec3::new(2.0, 0.0, 0.0);
    m.circ_center = Vec3::new(1.0, 0.0, 0.0);

    let dst_world = Mat4::from_translation(Vec3::new(10.0, 10.0, 10.0));
    let out = m.rebased_to(Mat4::IDENTITY, dst_world);

    assert!(
        (out.array_offset - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-5,
        "array_offset must be translation-invariant, got {}",
        out.array_offset
    );
    assert!(
        (out.circ_center - Vec3::new(-9.0, -10.0, -10.0)).length() < 1e-5,
        "circ_center must rebase as a point, got {}",
        out.circ_center
    );
}

/// A Boolean's cutter is a SCENE REFERENCE, not local-space state: pasting the modifier
/// onto another object must keep cutting against the SAME cutter body.
#[test]
fn boolean_cutter_reference_is_preserved_verbatim() {
    let mut m = PolyModifier::new(PolyModifierType::Boolean);
    m.bool_cutter_nid = 4242;
    m.bool_op = 2;
    let out = m.rebased_to(
        Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)),
        Mat4::from_scale(Vec3::splat(2.0)),
    );
    assert_eq!(out.bool_cutter_nid, 4242);
    assert_eq!(out.bool_op, 2);
}

/// A degenerate target transform must not produce NaN paths (fails closed to a plain copy).
#[test]
fn degenerate_target_transform_falls_back_to_a_plain_copy() {
    let mut m = PolyModifier::new(PolyModifierType::Pipe);
    m.pipe_path = vec![Vec3::new(1.0, 1.0, 1.0)];
    let out = m.rebased_to(Mat4::IDENTITY, Mat4::from_scale(Vec3::ZERO));
    assert!(out.pipe_path[0].is_finite());
    assert_eq!(out.pipe_path[0], Vec3::new(1.0, 1.0, 1.0));
}
