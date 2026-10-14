//! Gate for `HalfEdgeMesh::content_signature` — the key the app's poly display cache uses to skip
//! redundant modifier re-evaluation (bug #6). It MUST be stable when nothing changed and MUST move
//! on both in-place vertex moves (Arc::make_mut) and topological edits, or the cache would serve a
//! stale display.
use core_math::half_edge::{HalfEdgeMesh, VertexId};
use glam::Vec3;

#[test]
fn signature_stable_across_clone() {
    let m = HalfEdgeMesh::new_plane(2.0, 2.0, 8, 8);
    assert_eq!(m.content_signature(), m.clone().content_signature(),
               "an identical mesh must produce the same signature");
}

#[test]
fn signature_moves_on_in_place_vertex_edit() {
    // Simulates the Arc::make_mut drag/circle path: same mesh object, one vertex nudged.
    let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 8, 8);
    let before = m.content_signature();
    m.vert_mut(VertexId(10)).position += Vec3::new(0.001, 0.0, 0.0);
    assert_ne!(before, m.content_signature(),
               "moving a vertex (no count change) MUST change the signature");
}

#[test]
fn signature_moves_on_topology_edit() {
    let base = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
    let mut sub = base.clone();
    sub.subdivide_catmull_clark();
    assert_ne!(base.content_signature(), sub.content_signature(),
               "a topology change (subdivide) MUST change the signature");
}

#[test]
fn signature_moves_on_normals_only_change() {
    // The Solidify modifier reads STORED normals, so a "Recalculate Normals" that rewrites normals
    // WITHOUT moving any vertex/topology/material must invalidate the display cache — else the
    // corrected shell never re-syncs (the latent gap this locks). Isolate: change ONLY a normal.
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let before = m.content_signature();
    let mut m2 = m.clone();
    m2.vert_mut(VertexId(0)).normal = Vec3::new(9.0, 9.0, 9.0); // garbage normal, position untouched
    assert_ne!(before, m2.content_signature(),
               "a stored-normal change (no position/topology change) MUST change the signature");

    // And a face-normal-only change must move it too (Solidify sums face normals).
    let mut m3 = m.clone();
    m3.face_mut(core_math::half_edge::FaceId(0)).normal = Vec3::new(-1.0, -1.0, -1.0);
    assert_ne!(before, m3.content_signature(),
               "a stored FACE-normal change MUST change the signature (Solidify reads it)");
}

#[test]
fn topology_signature_stable_on_position_only() {
    // P3: grab moves verts — connectivity must stay the same fingerprint.
    let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
    let topo = m.topology_signature();
    m.vert_mut(VertexId(3)).position += Vec3::new(0.05, 0.0, 0.0);
    assert_eq!(topo, m.topology_signature(),
               "position-only edit must NOT change topology_signature");
    assert_ne!(topo, m.content_signature(),
               "content_signature must still move on position edit");
}

#[test]
fn topology_signature_moves_on_subdivide() {
    let base = HalfEdgeMesh::new_box(Vec3::ONE);
    let mut sub = base.clone();
    sub.subdivide_catmull_clark();
    assert_ne!(base.topology_signature(), sub.topology_signature());
}
