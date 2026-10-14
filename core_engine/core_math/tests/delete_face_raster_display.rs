#[test]
fn delete_one_face_raster_not_empty() {
    use core_math::half_edge::*;
    use glam::Vec3;
    let p = vec![
        Vec3::new(-1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,1.0),
        Vec3::new(-1.0,-1.0,1.0), Vec3::new(-1.0,1.0,-1.0), Vec3::new(1.0,1.0,-1.0),
        Vec3::new(1.0,1.0,1.0), Vec3::new(-1.0,1.0,1.0),
    ];
    let f: Vec<Vec<usize>> = vec![
        vec![0,1,2,3], vec![7,6,5,4], vec![4,5,1,0],
        vec![6,7,3,2], vec![7,4,0,3], vec![5,6,2,1],
    ];
    let mut m = HalfEdgeMesh::from_polygons(&p, &f);
    m.delete_face(FaceId(0));
    let (v, i) = core_math::poly_mesh::raster_data_ngon(&m);
    assert!(m.face_count() >= 5, "live faces {}", m.face_count());
    assert!(!i.is_empty(), "raster empty after 1 face delete verts={}", v.len());
    assert_eq!(i.len() % 3, 0);
    eprintln!("OK faces={} verts={} idx={}", m.face_count(), v.len(), i.len());
}
