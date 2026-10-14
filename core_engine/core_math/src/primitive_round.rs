//! Rounded corners for parametric primitives (`RoundSpec`).
//!
//! Plan: `docs/planning/poly/PARAMETRIC_PRIMITIVES.md` §3.2. Knowledge base:
//! `docs/planning/poly/BEVEL.md` — READ IT before changing anything here.
//!
//! # There is no corner-rounding math in this file, by design
//!
//! Rounding runs the EXISTING bevel (`bevel_op::bevel_edges_offset`) over the primitive's hard
//! edges. A second corner-rounding implementation would drift from the general bevel's fixes and
//! re-walk BEVEL.md §6's graveyard of ~26 dead ends. This module only decides WHICH edges to
//! bevel and enforces the install guard.
//!
//! # Corner quality — MEASURED, not assumed
//!
//! BEVEL.md §9 #4 warns that the rounded N-way corner (Blender's `M_ADJ` grid) is unimplemented
//! and that a corner with 3+ beveled edges flat-clamps at `segments > 1`. Every corner of a fully
//! rounded box is such a corner, so that would have made `RoundSpec` cosmetic at best.
//!
//! It does not apply here: `rounded_cube_matches_the_blender_oracle_topology` shows a ±1 cube with
//! all 12 edges beveled at offset 0.3 / segments 2 produces **v=56 f=54 — exactly Blender's
//! output** (`docs/planning/poly/rounded_cube_ref.py`). The corner path we take is good enough for
//! primitive rounding at Blender parity.
//!
//! The general gap is still real for arbitrary user selections, so [`RoundOutcome::corners_flat`]
//! reports the fallback whenever the bevel does signal it — callers surface that to the user
//! rather than presenting a chamfer as a round. Do not attempt to close the underlying gap here;
//! it belongs in `bevel_op`, per §9.

use crate::bevel_op::{self, BevelParams};
use crate::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_FACE, NO_TWIN};
use glam::Vec3;
use std::collections::HashSet;

/// Which edges of a primitive get rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundEdges {
    /// Every non-boundary edge. Correct for a box, where all 12 edges are hard.
    AllHard,
    /// Only the rims where a flat cap meets the swept wall — a cylinder's two circular edges.
    /// The vertical barrel seams are deliberately excluded: rounding them would melt the
    /// silhouette into a blob instead of easing the rim, which is what the user asked for.
    CapRims,
}

/// What actually happened, so callers can tell the user the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoundOutcome {
    /// The bevel ran and its result passed the install guard.
    pub applied: bool,
    /// The requested width was clamped against the geometry (BEVEL.md STEP 1 clamp-overlap).
    pub width_clamped: bool,
    /// Corners fell back to flat because the rounded N-way miter is unimplemented
    /// (BEVEL.md §9 #4). The mesh is valid, but the corners are chamfers, not rounds.
    pub corners_flat: bool,
    /// The bevel produced a torn/holed mesh and was REJECTED — the input is returned unchanged.
    /// Fail closed: never install a broken primitive.
    pub rejected: bool,
}

/// Round `which` edges of `mesh`. Returns the input untouched on any failure.
pub fn round_edges(
    mesh: HalfEdgeMesh,
    which: RoundEdges,
    radius: f32,
    segments: u32,
) -> (HalfEdgeMesh, RoundOutcome) {
    let mut out = RoundOutcome::default();
    if radius <= 1e-6 {
        return (mesh, out);
    }

    let selected = match which {
        RoundEdges::AllHard => all_hard_edges(&mesh),
        RoundEdges::CapRims => cap_rim_edges(&mesh),
    };
    if selected.is_empty() {
        return (mesh, out);
    }

    let params = BevelParams {
        width: radius,
        segments: segments.max(1),
        // 0.5 == circular == Blender's default profile. BEVEL.md §6 #9: do NOT default to 1.0.
        shape: 0.5,
        clamp: true,
    };

    let result = bevel_op::bevel_edges_offset(&mesh, &selected, &params);
    out.width_clamped = bevel_op::last_bevel_width_was_clamped();
    out.corners_flat = bevel_op::last_bevel_forced_flat();

    // BEVEL.md: ALWAYS gate the install. A bevel that tears must never reach the scene.
    if bevel_op::bevel_result_installable(&mesh, &result) {
        out.applied = true;
        (result, out)
    } else {
        out.rejected = true;
        (mesh, out)
    }
}

/// Convenience wrapper used by `PrimitiveSpec::build()` for boxes.
pub fn round_all_edges(mesh: HalfEdgeMesh, radius: f32, segments: u32) -> HalfEdgeMesh {
    round_edges(mesh, RoundEdges::AllHard, radius, segments).0
}

/// Convenience wrapper used by `PrimitiveSpec::build()` for cylinders.
pub fn round_cap_rims(mesh: HalfEdgeMesh, radius: f32, segments: u32) -> HalfEdgeMesh {
    round_edges(mesh, RoundEdges::CapRims, radius, segments).0
}

/// Every live interior half-edge that is actually a HARD edge — i.e. the two faces meeting there
/// are not coplanar.
///
/// The flatness filter is what makes rounding work on a SEGMENTED primitive. A 5×5×5 box has 12
/// real edges and hundreds of interior grid edges; the grid ones are perfectly coplanar, and
/// beveling them turned every subdivision line into a rounded ridge (owner 2026-08-04: "the bevel
/// should only affect original edges, not all new segments"). Only the silhouette-defining edges
/// belong in the selection, and coplanarity — not "was this here at creation" — is what
/// distinguishes them, which also makes it correct for any subdivision level.
///
/// Boundary edges are excluded because Blender's bevel is a no-op there and ours skips them in
/// STEP 0 (BEVEL.md §6 #25); including them would only add noise.
fn all_hard_edges(mesh: &HalfEdgeMesh) -> HashSet<u32> {
    // ~1° of slack: a box's grid edges are exactly coplanar, but a cylinder wall's are not, and a
    // transformed mesh can carry a little float drift.
    const COPLANAR_TOL_DEG: f32 = 1.0;
    let mut set = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let he = mesh.he(h);
        if he.face == NO_FACE || he.twin == NO_TWIN {
            continue;
        }
        if mesh.edge_is_flat(h, COPLANAR_TOL_DEG) {
            continue; // interior subdivision line, not a real edge
        }
        set.insert(i);
    }
    set
}

/// Half-edges where a flat cap meets the wall.
///
/// A cap is identified geometrically — a face whose normal is parallel to the revolve axis — not
/// by face index. Index-based identification would silently pick the wrong faces the moment the
/// revolve kernel changes its emission order, which is exactly the kind of tribal knowledge the
/// architecture rules forbid.
fn cap_rim_edges(mesh: &HalfEdgeMesh) -> HashSet<u32> {
    let is_cap = |f: FaceId| -> bool {
        if f == NO_FACE || !mesh.is_face_live(f) {
            return false;
        }
        mesh.face(f).normal.normalize_or_zero().dot(Vec3::Y).abs() > 0.99
    };

    let mut set = HashSet::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let he = mesh.he(h);
        if he.face == NO_FACE || he.twin == NO_TWIN {
            continue;
        }
        let (a, b) = mesh.edge_faces(h);
        // Exactly one side is a cap → this is a rim, not a cap-interior or barrel-seam edge.
        if is_cap(a) != is_cap(b) {
            set.insert(i);
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive_spec::{BoxSpec, CylinderSpec, PrimitiveSpec, RoundSpec};

    #[test]
    fn all_hard_edges_finds_every_cube_edge() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        // 12 undirected edges == 24 half-edges.
        assert_eq!(all_hard_edges(&cube).len(), 24);
    }

    #[test]
    fn a_segmented_box_still_has_only_twelve_hard_edges() {
        // The bug from the screenshot: every subdivision line got rounded into a ridge. A
        // segmented box must select exactly the same 12 silhouette edges as an unsegmented one,
        // no matter how dense the grid.
        for seg in [2u32, 3, 5] {
            let m = PrimitiveSpec::Box(BoxSpec {
                seg_x: seg,
                seg_y: seg,
                seg_z: seg,
                ..Default::default()
            })
            .build();
            let hard = all_hard_edges(&m);
            // 12 edges, each split into `seg` pieces, each piece = 2 half-edges.
            assert_eq!(
                hard.len(),
                (12 * seg * 2) as usize,
                "box seg={seg}: selected {} half-edges — interior grid lines leaked in",
                hard.len()
            );
        }
    }

    #[test]
    fn rounding_a_segmented_box_leaves_the_faces_flat() {
        // End-to-end version of the same guard: the rounded result must still have large flat
        // regions. If the grid lines were beveled, nearly every face would be a small chamfer
        // strip instead.
        let plain = PrimitiveSpec::Box(BoxSpec {
            seg_x: 4,
            seg_y: 4,
            seg_z: 4,
            ..Default::default()
        })
        .build();
        let rounded = PrimitiveSpec::Box(BoxSpec {
            seg_x: 4,
            seg_y: 4,
            seg_z: 4,
            round: RoundSpec { radius: 0.15, segments: 2 },
            ..Default::default()
        })
        .build();
        assert!(rounded.validate().is_ok(), "{:?}", rounded.validate());
        assert_eq!(bevel_op::boundary_loop_count(&rounded), 0);
        // Beveling only 12 edges adds a bounded amount of geometry; beveling all ~300 grid edges
        // would multiply it.
        assert!(
            rounded.face_count() < plain.face_count() * 3,
            "rounded segmented box exploded: {} → {} faces (grid edges were beveled)",
            plain.face_count(),
            rounded.face_count()
        );
    }

    #[test]
    fn cap_rims_excludes_barrel_seams() {
        let cyl = HalfEdgeMesh::new_cylinder(1.0, 2.0, 8);
        // Two rims of 8 edges each == 16 undirected == 32 half-edges. The 8 vertical seams and
        // the cap interiors must NOT be selected.
        assert_eq!(cap_rim_edges(&cyl).len(), 32);
    }

    #[test]
    fn zero_radius_is_a_no_op() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let before = cube.content_signature();
        let (out, outcome) = round_edges(cube, RoundEdges::AllHard, 0.0, 3);
        assert_eq!(out.content_signature(), before);
        assert!(!outcome.applied);
    }

    #[test]
    fn rounding_a_cube_is_valid_and_adds_geometry() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let (out, outcome) = round_edges(cube.clone(), RoundEdges::AllHard, 0.2, 1);
        assert!(outcome.applied, "bevel rejected: {outcome:?}");
        assert!(out.validate().is_ok(), "{:?}", out.validate());
        assert!(out.face_count() > cube.face_count());
        assert_eq!(
            bevel_op::boundary_loop_count(&out),
            0,
            "rounded cube must stay closed"
        );
    }

    #[test]
    fn rounding_a_cylinder_rim_is_valid() {
        let cyl = HalfEdgeMesh::new_cylinder(1.0, 2.0, 16);
        let (out, outcome) = round_edges(cyl.clone(), RoundEdges::CapRims, 0.15, 2);
        assert!(outcome.applied, "bevel rejected: {outcome:?}");
        assert!(out.validate().is_ok(), "{:?}", out.validate());
        assert_eq!(bevel_op::boundary_loop_count(&out), 0);
        assert!(out.face_count() > cyl.face_count());
    }

    #[test]
    fn rounded_cube_matches_the_blender_oracle_topology() {
        // Oracle: docs/planning/poly/rounded_cube_ref.py — ±1 cube, ALL 12 edges, offset 0.3,
        // segments 2, profile 0.5, miter SHARP → nv=56 nf=54.
        //
        // BEVEL.md §9 #4 says the rounded N-way corner (M_ADJ grid) is unimplemented, which would
        // flat-clamp every corner of a fully rounded box. This test measures what we ACTUALLY
        // produce instead of assuming either way, so the gap is recorded as a number rather than
        // as folklore.
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let (out, outcome) = round_edges(cube, RoundEdges::AllHard, 0.3, 2);
        assert!(outcome.applied, "bevel rejected: {outcome:?}");
        assert!(out.validate().is_ok(), "{:?}", out.validate());
        assert_eq!(bevel_op::boundary_loop_count(&out), 0, "must stay closed");

        const BLENDER_NV: usize = 56;
        const BLENDER_NF: usize = 54;
        assert_eq!(
            (out.vertex_count(), out.face_count()),
            (BLENDER_NV, BLENDER_NF),
            "rounded cube diverged from the Blender oracle \
             (ours v={} f={}, Blender v={BLENDER_NV} f={BLENDER_NF}, forced_flat={}). \
             If this is the documented §9 #4 M_ADJ gap, record the real numbers in \
             PARAMETRIC_PRIMITIVES.md §3.2 rather than loosening this assert.",
            out.vertex_count(),
            out.face_count(),
            outcome.corners_flat
        );
    }

    #[test]
    fn a_rejected_bevel_returns_the_input_untouched() {
        // An absurd width cannot produce a sane mesh; the guard must reject and hand back the
        // original rather than install a torn primitive.
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let sig = cube.content_signature();
        let (out, outcome) = round_edges(cube, RoundEdges::AllHard, 1000.0, 2);
        assert!(out.validate().is_ok());
        if outcome.rejected {
            assert_eq!(out.content_signature(), sig, "rejected must be a no-op");
        } else {
            // If the clamp saved it, the result must still be a closed, valid solid.
            assert_eq!(bevel_op::boundary_loop_count(&out), 0);
        }
    }

    #[test]
    fn spec_level_rounding_matches_direct_rounding() {
        // The spec path must not develop its own rounding behavior.
        let via_spec = PrimitiveSpec::Box(BoxSpec {
            round: RoundSpec { radius: 0.2, segments: 1 },
            ..Default::default()
        })
        .build();
        let direct = round_all_edges(HalfEdgeMesh::new_box(Vec3::ONE), 0.2, 1);
        assert_eq!(via_spec.vertex_count(), direct.vertex_count());
        assert_eq!(via_spec.face_count(), direct.face_count());
    }

    #[test]
    fn cylinder_spec_rounds_only_the_rims() {
        let m = PrimitiveSpec::Cylinder(CylinderSpec {
            sides: 16,
            round: RoundSpec { radius: 0.1, segments: 2 },
            ..Default::default()
        })
        .build();
        assert!(m.validate().is_ok());
        assert_eq!(bevel_op::boundary_loop_count(&m), 0);
    }
}
