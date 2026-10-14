// Regression guard for the poly Face-duplicate weld (app-layer `poly_duplicate_selection`).
// The bug: duplicating all faces of a mesh allocated a fresh vertex per face-corner with no shared
// map, so adjacent duplicated faces never shared a VertexId and `create_face` could not pair their
// interior twins → the "duplicate is broken, all surfaces are not connected" report.
// This test reproduces the FIX's mechanism at the kernel level: a shared source-vertex→new-vertex map
// makes the duplicated faces weld into ONE watertight island (0 boundary edges), independent of the
// source mesh.
use glam::Vec3;
use core_math::half_edge::{HalfEdgeMesh, HVertex, FaceId, NO_EDGE, NO_TWIN};
use std::collections::HashMap;

#[test]
fn face_duplicate_with_shared_vertmap_is_watertight() {
    let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
    let src_faces: Vec<u32> = (0..m.faces.len() as u32)
        .filter(|&f| m.face(FaceId(f)).half_edge != NO_EDGE)
        .collect();
    assert_eq!(src_faces.len(), 6, "cube starts with 6 faces");

    // --- the fix: ONE shared new-vertex per source vertex, welded across all duplicated faces ---
    let mut vert_map: HashMap<u32, _> = HashMap::new();
    let mut new_faces = Vec::new();
    for &fid in &src_faces {
        let verts = m.face_vertices(FaceId(fid));
        let mut ring = Vec::with_capacity(verts.len());
        for &vid in &verts {
            let nv = *vert_map.entry(vid.0).or_insert_with(|| {
                let pos = m.vert(vid).position;
                let normal = m.vert(vid).normal;
                m.alloc_vertex(HVertex { position: pos, normal, half_edge: NO_EDGE })
            });
            ring.push(nv);
        }
        new_faces.push(m.create_face(&ring).expect("create_face").0);
    }

    assert_eq!(vert_map.len(), 8, "8 cube corners → 8 shared new verts (not 24)");

    // Every edge of the duplicated island must have a twin — 0 boundary edges = watertight.
    let mut boundary = 0;
    for i in 0..m.half_edges.len() {
        let he = &m.half_edges[i];
        if new_faces.contains(&he.face.0) && he.twin == NO_TWIN {
            boundary += 1;
        }
    }
    assert_eq!(boundary, 0, "welded duplicate island must be closed (found {boundary} boundary edges)");
}
