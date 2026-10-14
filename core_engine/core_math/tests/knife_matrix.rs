// Knife behavior matrix + the plane-disambiguation regression (a shared-edge anchor must route each cut to
// the face whose PLANE the cut point lies on — not merge two faces via a footprint false-positive).
use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, VertexId, NO_EDGE};

fn dup_faces(m: &HalfEdgeMesh) -> usize {
    let mut c = 0;
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE { continue; }
        let vs = m.face_vertices(fid);
        let mut s = std::collections::HashSet::new();
        if vs.iter().any(|v| !s.insert(v.0)) { c += 1; }
    }
    c
}
fn cut(pts: &[Vec3], through: bool) -> (HalfEdgeMesh, usize, i32) {
    let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
    let f0 = m.face_count();
    let project = |p: Vec3| -> Option<Vec2> { Some(Vec2::new(p.x, p.y - 0.5 * p.z)) };
    let vd = Vec3::new(0.0, -1.0, -1.0).normalize();
    let poly: Vec<Vec2> = pts.iter().map(|&p| project(p).unwrap()).collect();
    let n = core_math::knife::knife_cut_screen(&mut m, &project, &poly, Some(pts), false, through, vd, None, None);
    let divided = m.face_count() as i32 - f0 as i32;
    (m, n, divided)
}
fn vid(m: &HalfEdgeMesh, p: Vec3) -> Option<u32> {
    (0..m.vertices.len() as u32).find(|&v| m.vert(VertexId(v)).half_edge != NO_EDGE
        && (m.vert(VertexId(v)).position - p).length() < 1e-3)
}

#[test]
fn through_cut_edge_to_edge_divides() {
    let (m, _, d) = cut(&[Vec3::new(-1.0, 1.0, 0.2), Vec3::new(1.0, 1.0, -0.2)], false);
    assert!(m.validate().is_ok());
    assert_eq!(d, 1, "edge-to-edge cut divides the top face into two");
    assert_eq!(dup_faces(&m), 0, "clean division, no slit");
}

#[test]
fn wrap_edge_face_edge_divides() {
    let (m, _, d) = cut(&[Vec3::new(0.2, 1.0, 1.0), Vec3::new(0.0, 0.3, 1.0), Vec3::new(-0.2, -1.0, 1.0)], false);
    assert!(m.validate().is_ok());
    assert_eq!(d, 1, "front face divided across an interior point");
}

#[test]
fn wrap_two_faces_divides_both() {
    let (m, _, d) = cut(&[Vec3::new(-1.0, 1.0, 0.3), Vec3::new(0.3, 0.3, 1.0), Vec3::new(-1.0, -0.3, 1.0)], false);
    assert!(m.validate().is_ok());
    assert_eq!(d, 2, "cut wrapping top+front divides both faces");
    assert_eq!(dup_faces(&m), 0);
}

#[test]
fn interior_endpoints_slit_on_correct_faces_no_merge() {
    // THE reported failing cut: A top-interior, B top-front edge, C/D/E front-interior. Must produce TWO
    // slits routed to the RIGHT faces (top gets A, front gets C/D/E) — NOT one merged cross-plane tangle.
    let pts = [Vec3::new(0.0, 1.0, -0.2), Vec3::new(0.2, 1.0, 1.0),
               Vec3::new(0.0, 0.4, 1.0), Vec3::new(0.4, 0.1, 1.0), Vec3::new(-0.2, -0.4, 1.0)];
    let (m, _, d) = cut(&pts, false);
    assert!(m.validate().is_ok(), "valid mesh");
    assert_eq!(d, 0, "dangling-both-ends cut divides nothing (slits)");
    assert_eq!(dup_faces(&m), 2, "exactly two slit faces (top + front) — not one merged tangle");
    let (va, vc) = (vid(&m, pts[0]).unwrap(), vid(&m, pts[2]).unwrap());
    // A (top) and C (front) must NEVER share a face — the plane-disambiguation regression.
    for f in 0..m.faces.len() as u32 {
        let fid = FaceId(f);
        if m.face(fid).half_edge == NO_EDGE { continue; }
        let vs: Vec<u32> = m.face_vertices(fid).iter().map(|v| v.0).collect();
        assert!(!(vs.contains(&va) && vs.contains(&vc)),
            "top-interior A and front-interior C wired into the SAME face {f}: {vs:?} (cross-plane merge)");
    }
}

#[test]
fn all_interior_no_crossing_splits_ngon() {
    // Was `all_interior_no_crossing_is_noop` — that pinned the BUG ("basic 2-click cut in the middle
    // of a face does not register"). Blender parity: each free endpoint of the interior chain connects
    // to ITS OWN nearest boundary vertex → a boundary-to-boundary chord that SPLITS the top face into
    // two (not a dangling slit). Both clicked points become real vertices; no self-intersecting face.
    let pts = [Vec3::new(-0.3, 1.0, 0.0), Vec3::new(0.3, 1.0, 0.0)];
    let (m, n, d) = cut(&pts, false);
    assert!(m.validate().is_ok());
    assert!(n >= 1, "interior-only cut must create geometry, got {n}");
    assert_eq!(d, 1, "n-gon split: the top face divides into two");
    assert_eq!(dup_faces(&m), 0, "clean chord split — no slit/tangle face");
    for p in pts {
        assert!(vid(&m, p).is_some(), "clicked point {p:?} became a real vertex");
    }
}
