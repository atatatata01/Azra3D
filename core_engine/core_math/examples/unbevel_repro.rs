// quick repro in a test file
use core_math::half_edge::{HalfEdgeId, HalfEdgeMesh, FaceId, NO_TWIN, NO_FACE};
use glam::Vec3;
use std::collections::HashSet;

fn main() {
    let mut cube = HalfEdgeMesh::new_box(Vec3::ONE);
    // pick first interior edge
    let e0 = (0..cube.half_edges.len() as u32).find(|&i| {
        let h = cube.he(HalfEdgeId(i));
        h.twin != NO_TWIN && h.face != NO_FACE
    }).unwrap();
    let p = core_math::bevel_op::BevelParams { width: 0.28, segments: 3, shape: 0.5, clamp: true };
    let m1 = core_math::bevel_op::bevel_edges_offset(&cube, &HashSet::from([e0]), &p);
    println!("after1: v={} f={} man={:?}", m1.vertices.len(), m1.face_count(), m1.is_manifold());
    // pick 5 edges that are "vertical-ish" or any 5 interior
    let mut edges = HashSet::new();
    for i in 0..m1.half_edges.len() as u32 {
        if edges.len() >= 5 { break; }
        let h = m1.he(HalfEdgeId(i));
        if h.twin == NO_TWIN || h.face == NO_FACE { continue; }
        if i > h.twin.0 { continue; }
        edges.insert(i);
    }
    let m2 = core_math::bevel_op::bevel_edges_offset(&m1, &edges, &core_math::bevel_op::BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true });
    println!("after2: v={} f={} man={:?}", m2.vertices.len(), m2.face_count(), m2.is_manifold());
    // face range from last bevel
    let range = core_math::bevel_op::last_bevel_new_face_range();
    println!("new face range: {:?}", range);
    let mut faces = HashSet::new();
    if let Some((s,e)) = range {
        for f in s..e { faces.insert(f); }
    }
    println!("seed faces {}", faces.len());
    match core_math::unbevel::unbevel_from_faces(&m2, &faces) {
        None => println!("unbevel None"),
        Some(r) => {
            println!("unbevel: v={} f={} sharp={} man={:?}", r.mesh.vertices.len(), r.mesh.face_count(), r.sharp_edges.len(), r.mesh.is_manifold());
            println!("boundary loops {}", core_math::bevel_op::boundary_loop_count(&r.mesh));
        }
    }
}
